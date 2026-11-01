// SPDX-License-Identifier: Apache-2.0
//! MMDS authentication exchange adapted from E2B runtime 51083a83cc0903185d992725ea6bb33683814b29.
//! Metadata polling and exporter integration belong to a separate lifecycle.
mod http;
pub mod poll;
mod route;
pub use http::{HttpReply, HttpRequest, HttpTransport, HyperTransport};
pub use route::{CommandOutput, CommandRunner, IptablesRoute, ProcessRunner, RoutePin};
use std::{future::Future, io, pin::Pin, sync::Arc, time::Duration};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MmdsOpts {
    pub instance_id: String,
    pub env_id: String,
    pub address: String,
    pub access_token_hash: String,
}
impl MmdsOpts {
    pub fn from_json(body: &[u8]) -> io::Result<Self> {
        use serde::Deserialize;
        let text = go_json_text(body);
        let mut stream = serde_json::Deserializer::from_str(&text);
        let value = Self::deserialize(&mut stream).map_err(io::Error::other)?;
        stream.end().map_err(io::Error::other)?;
        Ok(value)
    }
}
// Go encoding/json replaces malformed UTF-8 and unpaired UTF-16 escapes.
// Preserve valid pairs and every other escape for serde's syntax validation.
fn go_json_text(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let bytes = text.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    let mut quoted = false;
    while index < bytes.len() {
        if bytes[index] == b'"' {
            quoted = !quoted;
        }
        if quoted && bytes[index] == b'\\' && index + 1 < bytes.len() {
            let unit = |at: usize| -> Option<u16> {
                let hex = bytes.get(at..at + 6)?;
                if &hex[..2] != b"\\u" {
                    return None;
                }
                u16::from_str_radix(std::str::from_utf8(&hex[2..]).ok()?, 16).ok()
            };
            if let Some(first) = unit(index) {
                if (0xd800..=0xdbff).contains(&first)
                    && unit(index + 6).is_some_and(|second| (0xdc00..=0xdfff).contains(&second))
                {
                    output.extend_from_slice(&bytes[index..index + 12]);
                    index += 12;
                    continue;
                }
                if (0xd800..=0xdfff).contains(&first) {
                    output.extend_from_slice("�".as_bytes());
                    index += 6;
                    continue;
                }
            }
            output.extend_from_slice(&bytes[index..index + 2]);
            index += 2;
            continue;
        }
        output.push(bytes[index]);
        index += 1;
    }
    // Input started as valid UTF-8; replacements and copied escapes remain valid.
    String::from_utf8_lossy(&output).into_owned()
}

impl<'de> serde::Deserialize<'de> for MmdsOpts {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = MmdsOpts;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("MMDS metadata object or null")
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(MmdsOpts::default())
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<Self::Value, M::Error> {
                let mut opts = MmdsOpts::default();
                while let Some(key) = map.next_key::<String>()? {
                    let key: String = key
                        .chars()
                        .map(|ch| match ch {
                            '\u{212a}' => 'k',
                            '\u{17f}' => 's',
                            other => other.to_ascii_lowercase(),
                        })
                        .collect();
                    let field = match key.as_str() {
                        "instanceid" => Some(&mut opts.instance_id),
                        "envid" => Some(&mut opts.env_id),
                        "address" => Some(&mut opts.address),
                        "accesstokenhash" => Some(&mut opts.access_token_hash),
                        _ => None,
                    };
                    if let Some(field) = field {
                        if let Some(value) = map.next_value::<Option<String>>()? {
                            *field = value;
                        }
                    } else {
                        map.next_value::<serde::de::IgnoredAny>()?;
                    }
                }
                Ok(opts)
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

pub trait TokenHashProvider: Send + Sync {
    fn token_hash(&self) -> BoxFuture<'_, io::Result<String>>;
}
pub struct NoopProvider;
impl TokenHashProvider for NoopProvider {
    fn token_hash(&self) -> BoxFuture<'_, io::Result<String>> {
        Box::pin(async { Ok(String::new()) })
    }
}

pub struct MmdsClient {
    transport: Arc<dyn HttpTransport>,
    route: Arc<dyn RoutePin>,
    endpoint: String,
    pin_limiter: crate::logs::ratelimit::Limiter,
    pin_logger: crate::logs::Logger,
}
impl MmdsClient {
    pub fn new(transport: Arc<dyn HttpTransport>, route: Arc<dyn RoutePin>) -> Self {
        Self::with_endpoint(transport, route, "http://169.254.169.254/".into())
    }
    /// Endpoint injection is for local fixtures. Guest production uses new().
    pub fn with_endpoint(
        transport: Arc<dyn HttpTransport>,
        route: Arc<dyn RoutePin>,
        endpoint: String,
    ) -> Self {
        Self {
            transport,
            route,
            endpoint,
            // init.go:33: pinMMDSWarnLimit = ratelimit.New(10 * time.Second).
            pin_limiter: crate::logs::ratelimit::Limiter::new(Duration::from_secs(10)),
            // Go emits through a.logger (main.go:214: the envd logger);
            // tests and dev wiring default to a sink-less logger.
            pin_logger: crate::logs::Logger::none(),
        }
    }
    /// Go `a.logger` (main.go:214, init.go:92): the pin warn flows through
    /// the envd logger's sinks — the log exporter and, with --verbose,
    /// stdout.
    pub fn with_pin_logger(mut self, logger: crate::logs::Logger) -> Self {
        self.pin_logger = logger;
        self
    }
    /// Clock seam for the 10s warn floor (init.go:33); production uses the
    /// constructor default.
    pub fn with_pin_limiter(mut self, limiter: crate::logs::ratelimit::Limiter) -> Self {
        self.pin_limiter = limiter;
        self
    }
    async fn request(&self, request: HttpRequest) -> io::Result<HttpReply> {
        tokio::time::timeout(Duration::from_secs(10), self.transport.request(request))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "MMDS request timed out"))?
    }
    /// Fresh PUT token + GET metadata; neither status code nor hash is cached.
    pub async fn fetch_opts(&self) -> io::Result<MmdsOpts> {
        let token_url = format!("{}/latest/api/token", self.endpoint.trim_end_matches('/'));
        let token = self
            .request(HttpRequest {
                method: "PUT".into(),
                url: token_url,
                headers: vec![("X-metadata-token-ttl-seconds".into(), b"60".to_vec())],
                body: Vec::new(),
            })
            .await
            .and_then(|reply| {
                if reply.body.is_empty() {
                    Err(io::Error::other("mmds token is an empty string"))
                } else {
                    Ok(reply.body)
                }
            })
            .map_err(|error| {
                io::Error::new(error.kind(), format!("failed to get MMDS token: {error}"))
            })?;
        let response = self
            .request(HttpRequest {
                method: "GET".into(),
                url: self.endpoint.clone(),
                headers: vec![
                    ("X-metadata-token".into(), token),
                    ("Accept".into(), b"application/json".to_vec()),
                ],
                body: Vec::new(),
            })
            .await
            .and_then(|reply| MmdsOpts::from_json(&reply.body))
            .map_err(|error| {
                io::Error::new(error.kind(), format!("failed to get MMDS opts: {error}"))
            })?;
        Ok(response)
    }
    /// init.go:88-93: a failed pin is rate-limited to one zerolog warn per
    /// 10s window on the envd logger, carrying the suppressed count; the
    /// lookup is retried regardless of the warn outcome.
    fn warn_pin(&self, error: &io::Error) {
        let (allowed, suppressed) = self.pin_limiter.allow();
        if allowed {
            self.pin_logger
                .warn()
                .err(error)
                .int("suppressed", suppressed)
                .msg("failed to pin MMDS iptables route");
        }
    }
}
impl TokenHashProvider for MmdsClient {
    fn token_hash(&self) -> BoxFuture<'_, io::Result<String>> {
        Box::pin(async {
            match self.fetch_opts().await {
                Ok(opts) => Ok(opts.access_token_hash),
                Err(_) => {
                    if let Err(error) = self.route.pin().await {
                        self.warn_pin(&error);
                    }
                    Ok(self.fetch_opts().await?.access_token_hash)
                }
            }
        })
    }
}
