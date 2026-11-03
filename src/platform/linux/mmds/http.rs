// SPDX-License-Identifier: Apache-2.0
//! Fresh Hyper connections; redirects follow Go 1.26.10 net/http/client.go.
use super::BoxFuture;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{
    header::{HeaderMap, HeaderName, HeaderValue},
    Request,
};
use hyper_util::rt::TokioIo;
use std::{io, sync::Arc};
use tokio::net::TcpStream;
use tokio_rustls::{
    rustls::{pki_types::ServerName, ClientConfig, RootCertStore},
    TlsConnector,
};
#[path = "go_url.rs"]
mod go_url;
use go_url::GoUrl;

#[derive(Clone, Debug)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, Vec<u8>)>,
    pub body: Vec<u8>,
}
#[derive(Debug)]
pub struct HttpReply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}
pub trait HttpTransport: Send + Sync {
    fn request(&self, request: HttpRequest) -> BoxFuture<'_, io::Result<HttpReply>>;
    /// Go callers that never read the response body close it right after the
    /// headers arrive (exporter.go:70-78). Implementations that cannot stream
    /// may fall back to `request`.
    fn request_headers(&self, request: HttpRequest) -> BoxFuture<'_, io::Result<HttpReply>> {
        self.request(request)
    }
}
#[derive(Default)]
pub struct HyperTransport;
impl HttpTransport for HyperTransport {
    fn request(&self, request: HttpRequest) -> BoxFuture<'_, io::Result<HttpReply>> {
        Box::pin(follow_redirects(request, true))
    }
    fn request_headers(&self, request: HttpRequest) -> BoxFuture<'_, io::Result<HttpReply>> {
        Box::pin(follow_redirects(request, false))
    }
}

fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, error.to_string())
}

async fn tls_config() -> io::Result<Arc<ClientConfig>> {
    tokio::task::spawn_blocking(|| {
        let native = rustls_native_certs::load_native_certs();
        let mut roots = RootCertStore::empty();
        roots.add_parsable_certificates(native.certs);
        if roots.is_empty() {
            return Err(io::Error::other(format!(
                "no system TLS roots: {:?}",
                native.errors
            )));
        }
        let config = ClientConfig::builder_with_provider(Arc::new(
            tokio_rustls::rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(io::Error::other)?
        .with_root_certificates(roots)
        .with_no_client_auth();
        Ok(Arc::new(config))
    })
    .await
    .map_err(io::Error::other)?
}

async fn exchange(
    url: &GoUrl,
    method: &str,
    headers: &HeaderMap,
    body: Vec<u8>,
    read_body: bool,
) -> io::Result<HttpReply> {
    let host = url.dial_host()?;
    if host.is_empty() {
        return Err(invalid("missing HTTP host"));
    }
    let port = url.port()?;
    let tcp = if let Some(address) = url.scoped_address()? {
        TcpStream::connect(address).await?
    } else {
        TcpStream::connect((host.as_str(), port)).await?
    };
    match url.scheme.as_str() {
        "http" => send(tcp, url, method, headers, body, read_body).await,
        "https" => {
            let name = ServerName::try_from(host.split('%').next().unwrap_or_default().to_owned())
                .map_err(invalid)?;
            let tls = TlsConnector::from(tls_config().await?)
                .connect(name, tcp)
                .await?;
            send(tls, url, method, headers, body, read_body).await
        }
        _ => Err(invalid("unsupported URL scheme")),
    }
}

async fn send<S>(
    socket: S,
    url: &GoUrl,
    method: &str,
    headers: &HeaderMap,
    body: Vec<u8>,
    read_body: bool,
) -> io::Result<HttpReply>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(socket))
        .await
        .map_err(io::Error::other)?;
    let target = url.target();
    let body_length = body.len();
    let mut request = Request::builder()
        .method(method)
        .uri(target)
        .body(Full::new(Bytes::from(body)))
        .map_err(invalid)?;
    *request.headers_mut() = headers.clone();
    let host = url.wire_authority()?;
    if request
        .headers()
        .get("authorization")
        .is_none_or(|value| value.is_empty())
    {
        if let Some((username, password)) = &url.user {
            use base64::Engine;
            let credentials = [username.as_slice(), b":", password.as_slice()].concat();
            let value = format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(credentials)
            );
            request.headers_mut().insert(
                "authorization",
                HeaderValue::from_str(&value).map_err(invalid)?,
            );
        }
    }
    if !request.headers().contains_key("host") {
        request
            .headers_mut()
            .insert("host", HeaderValue::from_str(&host).map_err(invalid)?);
    }
    request
        .headers_mut()
        .insert("connection", HeaderValue::from_static("close"));
    request
        .headers_mut()
        .entry("user-agent")
        .or_insert(HeaderValue::from_static("Go-http-client/1.1"));
    if body_length > 0 {
        request
            .headers_mut()
            .entry("content-length")
            .or_insert(HeaderValue::from_str(&body_length.to_string()).map_err(invalid)?);
    } else if method == "PUT" || method == "POST" || method == "PATCH" {
        request
            .headers_mut()
            .entry("content-length")
            .or_insert(HeaderValue::from_static("0"));
    }
    let implicit_gzip = !request.headers().contains_key("accept-encoding")
        && !request.headers().contains_key("range")
        && method != "HEAD";
    if implicit_gzip {
        request
            .headers_mut()
            .insert("accept-encoding", HeaderValue::from_static("gzip"));
    }
    let response = async {
        let response = sender
            .send_request(request)
            .await
            .map_err(io::Error::other)?;
        let status = response.status().as_u16();
        let decompress = implicit_gzip
            && response
                .headers()
                .get("content-encoding")
                .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"gzip"));
        let headers = response
            .headers()
            .iter()
            .filter(|(key, _)| {
                !decompress || !matches!(key.as_str(), "content-encoding" | "content-length")
            })
            .map(|(key, value)| {
                (
                    key.to_string(),
                    String::from_utf8_lossy(value.as_bytes()).into_owned(),
                )
            })
            .collect();
        let is_redirect = matches!(status, 301 | 302 | 303 | 307 | 308)
            && response
                .headers()
                .get("location")
                .is_some_and(|value| !value.is_empty());
        // Go slurps at most 2KiB on redirect; a large body must not delay the next hop.
        let length = response
            .headers()
            .get("content-length")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        let mut body = response.into_body();
        let mut bytes = Vec::new();
        let slurp_redirect = is_redirect;
        if slurp_redirect {
            // Go slurps at most 2KiB on redirect; a large body must not delay the next hop.
            if length.is_none_or(|length| length <= 2048) {
                while let Some(frame) = body.frame().await {
                    let frame = match frame {
                        Ok(frame) => frame,
                        Err(_) => break,
                    };
                    if let Ok(data) = frame.into_data() {
                        bytes.extend_from_slice(&data);
                        if bytes.len() >= 2048 {
                            break;
                        }
                    }
                }
            }
        } else if read_body {
            while let Some(frame) = body.frame().await {
                let frame = frame.map_err(io::Error::other)?;
                if let Ok(data) = frame.into_data() {
                    bytes.extend_from_slice(&data);
                }
            }
        }
        // read_body == false closes the body here: Go's exporter never reads it
        // (exporter.go:74), so a stalled body must not delay the status check.
        if decompress && !is_redirect && read_body {
            use tokio::io::AsyncReadExt;
            let mut decoded = Vec::new();
            let mut decoder = async_compression::tokio::bufread::GzipDecoder::new(bytes.as_slice());
            decoder.multiple_members(true);
            decoder.read_to_end(&mut decoded).await?;
            bytes = decoded;
        }
        Ok(HttpReply {
            status,
            headers,
            body: bytes,
        })
    };
    tokio::pin!(response, connection);
    // Both futures own their state locally. Cancellation drops the socket directly,
    // without a detached connection task retaining an unauthenticated exchange.
    tokio::select! {
        biased;
        result = &mut response => result,
        result = &mut connection => {
            result.map_err(io::Error::other)?;
            response.await
        }
    }
}

fn trusted_redirect(initial: &GoUrl, next: &GoUrl) -> bool {
    let source = initial
        .dial_host()
        .unwrap_or_else(|_| initial.hostname().to_owned());
    let target = next
        .dial_host()
        .unwrap_or_else(|_| next.hostname().to_owned());
    target == source
        || (!target.contains([':', '%'])
            && target
                .strip_suffix(&source)
                .is_some_and(|prefix| prefix.ends_with('.')))
}
fn copied_headers(original: &HeaderMap, sensitive: bool, body: bool) -> HeaderMap {
    original
        .iter()
        .filter(|(key, _)| {
            !(sensitive
                && matches!(
                    key.as_str(),
                    "authorization"
                        | "www-authenticate"
                        | "cookie"
                        | "cookie2"
                        | "proxy-authorization"
                        | "proxy-authenticate"
                )
                || body
                    && matches!(
                        key.as_str(),
                        "content-encoding"
                            | "content-language"
                            | "content-location"
                            | "content-type"
                    ))
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}
async fn follow_redirects(request: HttpRequest, read_body: bool) -> io::Result<HttpReply> {
    let initial = GoUrl::parse(&request.url)?;
    let mut url = initial.clone();
    let mut method = request.method;
    let body = request.body;
    let mut original = HeaderMap::new();
    for (name, value) in request.headers {
        original.append(
            HeaderName::from_bytes(name.as_bytes()).map_err(invalid)?,
            HeaderValue::from_bytes(&value).map_err(invalid)?,
        );
    }
    let mut headers = original.clone();
    let mut strip_sensitive = false;
    let mut include_body = true;
    for count in 1..=10 {
        if !matches!(url.scheme.as_str(), "http" | "https") {
            return Err(invalid("unsupported URL scheme"));
        }
        // `bytes.Buffer` gives the Go request a GetBody, so 307/308 replay the
        // original body; 301-303 cleared include_body (client.go:514-541).
        let hop_body = if include_body {
            body.clone()
        } else {
            Vec::new()
        };
        let reply = exchange(&url, &method, &headers, hop_body, read_body).await?;
        if !matches!(reply.status, 301 | 302 | 303 | 307 | 308) {
            return Ok(reply);
        }
        let location = reply
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("location"))
            .map(|(_, value)| value.as_str())
            .unwrap_or_default();
        if location.is_empty() {
            return Ok(reply);
        }
        let next = url.resolve(location)?;
        if count == 10 {
            return Err(io::Error::other("stopped after 10 redirects"));
        }
        if matches!(reply.status, 301..=303) {
            if method != "GET" && method != "HEAD" {
                method = "GET".into();
            }
            include_body = false;
        }
        strip_sensitive |= !trusted_redirect(&initial, &next);
        headers = copied_headers(&original, strip_sensitive, !include_body);
        if !(url.scheme.as_str() == "https" && next.scheme.as_str() == "http")
            && !headers.contains_key("referer")
        {
            headers.insert(
                "referer",
                HeaderValue::from_str(&url.referer()).map_err(invalid)?,
            );
        }
        url = next;
    }
    Err(io::Error::other("stopped after 10 redirects"))
}
