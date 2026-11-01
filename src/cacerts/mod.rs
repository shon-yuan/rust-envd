//! Pinned `internal/cabundle/bundle.go` + `internal/host/cacerts.go`: parse
//! orchestrator-supplied certificates, record intent in
//! `/run/e2b/ca/state.json` (0600) before publishing trust, append to
//! `/etc/ssl/certs/ca-certificates.crt`, write `e2b-<fingerprint>.crt` (0644)
//! sources, reconcile them once a second, and retire `e2b-ca.crt` only once
//! nothing requests it. One weighted slot serializes install and reconcile;
//! a failed acquire under a dead context answers `ErrCAInstallInProgress`
//! (the /init handler maps that to 503, everything else to 400).
//!
//! PEM and DER acceptance are pinned to the pinned Go toolchain by the probe
//! in /tmp/go-cacerts-probe; deviations (only reachable with adversarial
//! DER): the x509 gate skips Go's on-curve point verification, deep value
//! parsing of extensions other than basicConstraints/subjectAltName, and
//! RSA/EC key integer depth. Everything the battery pins is byte-exact.
//!
//! Disclosed divergences (coordinator-reviewed, note-only): a non-UTF-8 PEM
//! type line escapes as U+FFFD where Go %q prints \xNN byte escapes, and the
//! staging file in prepareFile is created with the umask'd default and
//! chmod'd to the final mode before any content is written, where Go
//! CreateTemp starts at 0600 — no content is ever readable in either window.

mod der;

pub use der::validate_certificate;

/// The probed crypto gate, re-exported under the Go package name.
pub mod x509 {
    pub use super::der::validate_certificate;
}

use base64::Engine as _;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const CA_BUNDLE_PATH: &str = "/etc/ssl/certs/ca-certificates.crt";
const CA_CERT_DIR: &str = "/usr/local/share/ca-certificates";
const STATE_PATH: &str = "/run/e2b/ca/state.json";
const LEGACY_CA_CERT_FILE: &str = "e2b-ca.crt";

/// A parsed certificate: SHA-256 hex of the DER bytes as delivered, plus the
/// normalized PEM (canonical re-encode, headers dropped).
#[derive(Clone, Debug)]
pub struct Certificate {
    pub fingerprint: String,
    pub pem: Vec<u8>,
}

pub type Bundle = Vec<Certificate>;

/// The persisted intent record (`cacerts.go:32-35`).
#[derive(Clone, Debug, Default)]
struct CaState {
    desired_pem: String,
    /// Go []string: None is a nil slice (JSON null), Some an actual slice.
    owned: Option<Vec<String>>,
}

impl CaState {
    // Go json.Marshal emits struct field order: desiredPEM, then owned; a nil
    // slice marshals as null (probe-confirmed), an empty slice as [].
    fn marshal(&self) -> String {
        let owned = match &self.owned {
            None => "null".to_string(),
            Some(owned) => {
                let items: Vec<String> = owned.iter().map(|fp| format!("\"{fp}\"")).collect();
                format!("[{}]", items.join(","))
            }
        };
        format!(
            "{{\"desiredPEM\":{},\"owned\":{}}}",
            serde_json::to_string(&self.desired_pem).unwrap_or_default(),
            owned
        )
    }

    fn unmarshal(data: &[u8]) -> Result<Self, String> {
        #[derive(serde::Deserialize)]
        struct Wire {
            #[serde(rename = "desiredPEM", default)]
            desired_pem: String,
            #[serde(default)]
            owned: Option<Vec<String>>,
        }
        let wire: Wire =
            serde_json::from_slice(data).map_err(|error| format!("decode CA state: {error}"))?;
        Ok(CaState {
            desired_pem: wire.desired_pem,
            owned: wire.owned,
        })
    }
}

/// `cacerts.go:22` ErrCAInstallInProgress plus the error chains the /init
/// handler writes back verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaError {
    InProgress,
    Failed(String),
}

impl CaError {
    /// Go init.go:738-745 writeInitError: the in-progress mapping answers 503
    /// ("not a failure, a concurrent install still holds the CA lock"),
    /// everything else the default 400.
    pub fn init_status(&self) -> axum::http::StatusCode {
        match self {
            CaError::InProgress => axum::http::StatusCode::SERVICE_UNAVAILABLE,
            CaError::Failed(_) => axum::http::StatusCode::BAD_REQUEST,
        }
    }
}

impl std::fmt::Display for CaError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaError::InProgress => {
                write!(
                    formatter,
                    "CA install already in progress: context canceled"
                )
            }
            CaError::Failed(message) => write!(formatter, "{message}"),
        }
    }
}

fn failed(message: impl Into<String>) -> CaError {
    CaError::Failed(message.into())
}

fn malformed_pem() -> CaError {
    failed("malformed certificate PEM")
}

/// Go syscall errno text: the strerror string lowercased, without Rust's
/// " (os error N)" suffix (probe: "open /x: no such file or directory").
fn go_errno_message(error: &std::io::Error) -> String {
    let text = error.to_string();
    let text = text
        .split(" (os error ")
        .next()
        .unwrap_or(&text)
        .to_string();
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().collect::<String>() + chars.as_str(),
        None => text,
    }
}

/// Go *fs.PathError from os.ReadFile/WriteFile/CreateTemp/Chmod/Remove:
/// "{operation} {path}: {message}".
fn go_path_error(op: &str, path: &Path, error: &std::io::Error) -> String {
    format!("{op} {}: {}", path.display(), go_errno_message(error))
}

/// Go Rename's PathError carries both paths.
fn go_rename_error(from: &Path, to: &Path, error: &std::io::Error) -> String {
    format!(
        "rename {} {}: {}",
        from.display(),
        to.display(),
        go_errno_message(error)
    )
}

/// Go %q for a PEM type line (ASCII-typical; other bytes escape as \xNN).
fn go_quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for byte in text.chars() {
        match byte {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// --- PEM codec, pinned to the pinned encoding/pem (pem.go:80-198) ---

#[derive(Clone, Debug, Default)]
struct PemBlock {
    typ: String,
    headers: BTreeMap<String, String>,
    bytes: Vec<u8>,
}

const PEM_END: &[u8] = b"\n-----END ";
const PEM_END_OF_LINE: &[u8] = b"-----";
const BEGIN_LINE: &[u8] = b"-----BEGIN ";

/// One getLine: the line (trailing CR swallowed, then trailing spaces/tabs
/// trimmed), then the offset just past the newline; with no newline the whole
/// tail is the line and the offset is the length.
fn get_line(data: &[u8]) -> (&[u8], usize) {
    let newline = data.iter().position(|b| *b == b'\n');
    let (end, next) = match newline {
        None => (data.len(), data.len()),
        Some(index) => {
            let mut end = index;
            if end > 0 && data[end - 1] == b'\r' {
                end -= 1;
            }
            (end, index + 1)
        }
    };
    let mut end = end;
    while end > 0 && (data[end - 1] == b' ' || data[end - 1] == b'\t') {
        end -= 1;
    }
    (&data[..end], next)
}

fn trim_space(data: &[u8]) -> &[u8] {
    let is_space = |b: u8| matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r');
    let mut start = 0;
    let mut end = data.len();
    while start < end && is_space(data[start]) {
        start += 1;
    }
    while end > start && is_space(data[end - 1]) {
        end -= 1;
    }
    &data[start..end]
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|i| &haystack[*i..*i + needle.len()] == needle)
}

fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len())
        .rev()
        .find(|i| &haystack[*i..*i + needle.len()] == needle)
}

/// Go encoding/pem Decode (pem.go:89-198), including the skip-forward loop
/// after a bad block and the negative-index header accounting. The indexes
/// are signed exactly like Go's, which lets a header line swallow the END
/// marker and restart the search one trailer later.
fn pem_decode(data: &[u8]) -> Option<(PemBlock, &[u8])> {
    let mut rest: &[u8] = data;
    let mut end_trailer_index: isize = 0;
    loop {
        if end_trailer_index < 0 || end_trailer_index > rest.len() as isize {
            return None;
        }
        rest = &rest[end_trailer_index as usize..];

        let end_index = find(rest, PEM_END)? as isize;
        end_trailer_index = end_index + PEM_END.len() as isize;
        let begin_index = rfind(&rest[..end_index as usize], BEGIN_LINE)? as isize;
        if begin_index > 0 && rest[(begin_index - 1) as usize] != b'\n' {
            continue;
        }
        // pemStart is "\n-----BEGIN " (12 bytes): advancing begin_index +
        // len(pemStart) - 1 lands right after the 11-byte "-----BEGIN ".
        let shift = begin_index + BEGIN_LINE.len() as isize;
        rest = &rest[shift as usize..];
        let mut end_index = end_index - shift;
        let mut end_trailer_index = end_trailer_index - shift;

        let (type_line, consumed) = get_line(rest);
        rest = &rest[consumed..];
        end_index -= consumed as isize;
        end_trailer_index -= consumed as isize;
        if !type_line.ends_with(PEM_END_OF_LINE) {
            continue;
        }
        let type_line = &type_line[..type_line.len() - PEM_END_OF_LINE.len()];
        let mut block = PemBlock {
            typ: String::from_utf8_lossy(type_line).into_owned(),
            ..PemBlock::default()
        };

        while !rest.is_empty() {
            let (line, next) = get_line(rest);
            let Some(colon) = line.iter().position(|b| *b == b':') else {
                break;
            };
            let key = trim_space(&line[..colon]);
            let value = trim_space(&line[colon + 1..]);
            block.headers.insert(
                String::from_utf8_lossy(key).into_owned(),
                String::from_utf8_lossy(value).into_owned(),
            );
            rest = &rest[next..];
            end_index -= next as isize;
            end_trailer_index -= next as isize;
        }

        // If there were headers, there must be a newline between the headers
        // and the END line (pem.go:158-160).
        if !block.headers.is_empty() && end_index < 0 {
            continue;
        }

        // After the "-----" of the ending line, there should be the same type
        // and then a final five dashes (pem.go:162-175).
        if end_trailer_index < 0 || end_trailer_index as usize > rest.len() {
            continue;
        }
        let end_trailer_full = &rest[end_trailer_index as usize..];
        let end_trailer_len = type_line.len() + PEM_END_OF_LINE.len();
        if end_trailer_full.len() < end_trailer_len {
            continue;
        }
        let rest_of_end_line = &end_trailer_full[end_trailer_len..];
        let end_trailer = &end_trailer_full[..end_trailer_len];
        if !end_trailer.starts_with(type_line) || !end_trailer.ends_with(PEM_END_OF_LINE) {
            continue;
        }
        // The line must end with only whitespace (pem.go:177-180).
        if !get_line(rest_of_end_line).0.is_empty() {
            continue;
        }

        block.bytes = Vec::new();
        if end_index > 0 {
            // removeSpacesAndTabs strips only ' ' and '\t' (pem.go:336); the
            // Go base64 decoder itself skips \r and \n and ignores non-zero
            // trailing bits, while StdEncoding still requires canonical
            // padding -- mirror that exact combination in the engine.
            let base64_data: Vec<u8> = rest[..end_index as usize]
                .iter()
                .copied()
                .filter(|b| !matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
                .collect();
            match go_base64().decode(&base64_data) {
                Ok(bytes) => block.bytes = bytes,
                Err(_) => continue,
            }
        }

        // The -1 is because we might have only matched pemEnd without the
        // leading newline if the PEM block was empty (pem.go:193-196).
        let tail_start = end_trailer_index - 1;
        let tail = &rest[tail_start as usize..];
        let after = get_line(tail).1;
        return Some((block, &rest[(tail_start + after as isize) as usize..]));
    }
}

/// Go base64.StdEncoding.Decode semantics: standard alphabet, canonical
/// padding required (an unpadded final quantum is CorruptInputError,
/// base64.go:316-321, probe-confirmed in /tmp/go-fxround-probe), non-zero
/// trailing bits ignored.
fn go_base64() -> base64::engine::GeneralPurpose {
    use base64::alphabet::STANDARD;
    use base64::engine::{DecodePaddingMode, GeneralPurposeConfig};
    static ENGINE: std::sync::OnceLock<base64::engine::GeneralPurpose> = std::sync::OnceLock::new();
    ENGINE
        .get_or_init(|| {
            base64::engine::GeneralPurpose::new(
                &STANDARD,
                GeneralPurposeConfig::new()
                    .with_decode_padding_mode(DecodePaddingMode::RequireCanonical)
                    .with_decode_allow_trailing_bits(true),
            )
        })
        .clone()
}

/// Canonical pem.Encode (pem.go:255-330): begin line, headers with Proc-Type
/// first (RFC 1421 4.6.1.1) and the remaining keys sorted, a blank separator
/// line, 64-column base64, end line.
fn pem_encode(block: &PemBlock) -> Vec<u8> {
    let mut out = Vec::with_capacity(block.bytes.len() * 4 / 3 + 96);
    out.extend_from_slice(BEGIN_LINE);
    out.extend_from_slice(block.typ.as_bytes());
    out.extend_from_slice(b"-----\n");
    if !block.headers.is_empty() {
        if let Some(proc_type) = block.headers.get("Proc-Type") {
            out.extend_from_slice(b"Proc-Type: ");
            out.extend_from_slice(proc_type.as_bytes());
            out.push(b'\n');
        }
        let mut keys: Vec<&String> = block.headers.keys().collect();
        keys.sort();
        for key in keys {
            if key == "Proc-Type" {
                continue;
            }
            out.extend_from_slice(key.as_bytes());
            out.extend_from_slice(b": ");
            out.extend_from_slice(block.headers[key].as_bytes());
            out.push(b'\n');
        }
        out.push(b'\n');
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(&block.bytes);
    for chunk in encoded.as_bytes().chunks(64) {
        out.extend_from_slice(chunk);
        out.push(b'\n');
    }
    out.extend_from_slice(&PEM_END[1..]);
    out.extend_from_slice(block.typ.as_bytes());
    out.extend_from_slice(b"-----\n");
    out
}

// --- cabundle.Parse / Equal / Reconcile, pinned (bundle.go) ---

fn count_begin(region: &[u8]) -> usize {
    let mut count = 0;
    let mut offset = 0;
    while let Some(found) = find(&region[offset..], BEGIN_LINE) {
        count += 1;
        offset += found + BEGIN_LINE.len();
    }
    count
}

/// `bundle.go:23` Parse: rejects partial or malformed delivered bundles,
/// removes duplicates, and normalizes the stored PEM.
pub fn parse(bundle_pem: &str) -> Result<Bundle, CaError> {
    let mut certs = Bundle::new();
    let mut seen = HashSet::new();
    let mut rest: &[u8] = trim_space(bundle_pem.as_bytes());
    while !rest.is_empty() {
        let Some((block, tail)) = pem_decode(rest) else {
            return Err(malformed_pem());
        };
        let consumed = rest.len() - tail.len();
        // Decode skips malformed prefixes; reject them rather than silently
        // installing only part of the delivered set (bundle.go:28-32).
        if !rest.starts_with(BEGIN_LINE) || count_begin(&rest[..consumed]) != 1 {
            return Err(malformed_pem());
        }
        if block.typ != "CERTIFICATE" {
            return Err(failed(format!(
                "PEM block is {}, not a certificate",
                go_quote(&block.typ)
            )));
        }
        validate_certificate(&block.bytes)
            .map_err(|detail| failed(format!("parse certificate: {detail}")))?;
        let fingerprint = fingerprint_of(&block.bytes);
        if !seen.contains(&fingerprint) {
            certs.push(Certificate {
                fingerprint: fingerprint.clone(),
                pem: pem_encode(&PemBlock {
                    typ: "CERTIFICATE".into(),
                    bytes: block.bytes,
                    ..PemBlock::default()
                }),
            });
            seen.insert(fingerprint);
        }
        rest = trim_space(tail);
    }
    if certs.is_empty() {
        return Err(failed("no certificates"));
    }
    Ok(certs)
}

fn fingerprint_of(der: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(der);
    hex_lower(&hasher.finalize())
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// `bundle.go:54` Equal: identities regardless of order or PEM formatting.
pub fn bundle_equal(a: &Bundle, b: &Bundle) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let set: HashSet<&str> = a.iter().map(|cert| cert.fingerprint.as_str()).collect();
    b.iter().all(|cert| set.contains(cert.fingerprint.as_str()))
}

/// `bundle.go:73` Reconcile: remove owned identities from the aggregate, then
/// append the desired set; unrelated certificates keep their trust (re-encoded
/// canonically, headers preserved); a junk tail is dropped when Decode stalls.
pub fn reconcile(content: &[u8], owned: &[String], desired: &Bundle) -> Vec<u8> {
    let mut managed: HashSet<&str> = HashSet::with_capacity(owned.len() + desired.len());
    for id in owned {
        managed.insert(id.as_str());
    }
    for cert in desired {
        managed.insert(cert.fingerprint.as_str());
    }
    let mut out = Vec::with_capacity(content.len() + desired.len() * 1024);
    let mut rest: &[u8] = content;
    while !rest.is_empty() {
        let Some((block, tail)) = pem_decode(rest) else {
            break;
        };
        rest = tail;
        if block.typ == "CERTIFICATE" && managed.contains(fingerprint_of(&block.bytes).as_str()) {
            continue;
        }
        out.extend_from_slice(&pem_encode(&block));
    }
    for cert in desired {
        out.extend_from_slice(&cert.pem);
    }
    out
}

/// `cacerts.go:23` managedCertFilename ^e2b-([0-9a-f]{64})\.crt$.
fn managed_fingerprint(name: &str) -> Option<&str> {
    let fingerprint = name.strip_prefix("e2b-")?.strip_suffix(".crt")?;
    let is_lower_hex = |byte: u8| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte);
    (fingerprint.len() == 64 && fingerprint.bytes().all(is_lower_hex)).then_some(fingerprint)
}

/// `cacerts.go:360` makeCertName.
pub fn make_cert_name(fingerprint: &str) -> String {
    format!("e2b-{fingerprint}.crt")
}

// --- the installer (cacerts.go:39-53) ---

/// Installer state guarded by the weighted slot. The type is public only so
/// tests can hold the lock; every field stays private.
pub struct CaFields {
    state: Option<CaState>,
    requested: Bundle,
    loaded: bool,
    installed: bool,
    pending: bool,
    /// Stand-in for Go's `c.state != state` pointer-identity gates.
    generation: u64,
}

/// `cacerts.go:39` CACertInstaller. The weighted semaphore and every state
/// field it guards share one async mutex: Go's persist holds the slot across
/// its mutation windows and releases it for the source writes, and so does
/// this.
pub struct CaCertInstaller {
    mu: tokio::sync::Mutex<CaFields>,
    state_path: PathBuf,
    bundle_path: PathBuf,
    cert_dir: PathBuf,
    tick: Duration,
    wake_tx: tokio::sync::mpsc::Sender<()>,
    wake_rx: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<()>>,
}

/// Pinned default paths; tests inject tempdir roots.
pub struct CaCertInstallerBuilder {
    state_path: PathBuf,
    bundle_path: PathBuf,
    cert_dir: PathBuf,
    tick: Duration,
}

impl Default for CaCertInstallerBuilder {
    fn default() -> Self {
        Self {
            state_path: PathBuf::from(STATE_PATH),
            bundle_path: PathBuf::from(CA_BUNDLE_PATH),
            cert_dir: PathBuf::from(CA_CERT_DIR),
            tick: Duration::from_secs(1),
        }
    }
}

impl CaCertInstallerBuilder {
    pub fn state_path(mut self, path: PathBuf) -> Self {
        self.state_path = path;
        self
    }
    pub fn bundle_path(mut self, path: PathBuf) -> Self {
        self.bundle_path = path;
        self
    }
    pub fn cert_dir(mut self, path: PathBuf) -> Self {
        self.cert_dir = path;
        self
    }
    pub fn tick(mut self, tick: Duration) -> Self {
        self.tick = tick;
        self
    }
    pub fn build(self) -> CaCertInstaller {
        CaCertInstaller::from_builder(self)
    }
}

impl CaCertInstaller {
    pub fn builder() -> CaCertInstallerBuilder {
        CaCertInstallerBuilder::default()
    }

    fn from_builder(builder: CaCertInstallerBuilder) -> Self {
        let (wake_tx, wake_rx) = tokio::sync::mpsc::channel(1);
        Self {
            mu: tokio::sync::Mutex::new(CaFields {
                state: None,
                requested: Bundle::new(),
                loaded: false,
                installed: false,
                pending: false,
                generation: 0,
            }),
            state_path: builder.state_path,
            bundle_path: builder.bundle_path,
            cert_dir: builder.cert_dir,
            tick: builder.tick,
            wake_tx,
            wake_rx: tokio::sync::Mutex::new(wake_rx),
        }
    }

    /// `cacerts.go:55` Install: the /init foreground half.
    pub async fn install(&self, bundle_pem: &str) -> Result<(), CaError> {
        self.install_cancellable(bundle_pem, std::future::pending())
            .await
    }

    /// `cacerts.go:59` install with a cancellation side channel standing in
    /// for Go's request context. The acquire itself (updateBundle:77-80) is
    /// x/sync weighted.Acquire, which fails a done context before even trying
    /// the fast path (semaphore.go:66-73, "prefer to fail even if
    /// uncontended") — so a fired cancellation is ErrCAInstallInProgress
    /// whether or not the slot is free.
    pub async fn install_cancellable(
        &self,
        bundle_pem: &str,
        cancel: impl std::future::Future<Output = ()> + Send,
    ) -> Result<(), CaError> {
        if bundle_pem.is_empty() {
            return Ok(());
        }
        let certs =
            parse(bundle_pem).map_err(|error| failed(format!("parse CA bundle: {error}")))?;
        let mut cancel = std::pin::pin!(cancel);
        // Acquire's done-context pre-check, ahead of the try-lock fast path.
        if futures_util::FutureExt::now_or_never(std::future::poll_fn(|cx| {
            cancel.as_mut().poll(cx)
        }))
        .is_some()
        {
            return Err(CaError::InProgress);
        }
        let result = {
            let mut fields = match self.mu.try_lock() {
                Ok(fields) => fields,
                Err(_) => tokio::select! {
                    fields = self.mu.lock() => fields,
                    _ = &mut cancel => return Err(CaError::InProgress),
                },
            };
            self.update_bundle(&mut fields, &certs).await
        };
        // cacerts.go:67-73: wake the reconcile loop regardless of the outcome
        // of updateBundle (a parse failure returns before this, as in Go).
        let _ = self.wake_tx.try_send(());
        result
    }

    /// Test seam: hold the weighted slot like a wedged reconcile would.
    #[doc(hidden)]
    pub async fn hold_lock_for_tests(&self) -> tokio::sync::MutexGuard<'_, CaFields> {
        self.mu.lock().await
    }

    /// `cacerts.go:77` updateBundle: record every pending retirement before
    /// publishing new trust.
    async fn update_bundle(&self, fields: &mut CaFields, certs: &Bundle) -> Result<(), CaError> {
        self.load_state(fields).await?;
        if fields.installed && bundle_equal(&fields.requested, certs) {
            return Ok(());
        }
        let recovering = !fields.installed && fields.state.is_some();
        // cacerts.go:97-100: clone(nil) stays nil, but every cert appends, so
        // a real install always lands a non-nil set.
        let mut owned = fields
            .state
            .as_ref()
            .and_then(|state| state.owned.clone())
            .unwrap_or_default();
        let mut desired = String::new();
        for cert in certs {
            if !owned.contains(&cert.fingerprint) {
                owned.push(cert.fingerprint.clone());
            }
            desired.push_str(std::str::from_utf8(&cert.pem).unwrap_or_default());
        }
        let state = CaState {
            desired_pem: desired,
            owned: Some(owned),
        };
        self.save_state(&state).await?;
        fields.state = Some(state.clone());
        fields.requested = certs.clone();
        fields.installed = false;
        fields.pending = true;
        fields.generation += 1;
        let mut data = tokio::fs::read(&self.bundle_path).await.map_err(|error| {
            failed(format!(
                "read CA bundle: {}",
                go_path_error("open", &self.bundle_path, &error)
            ))
        })?;
        if recovering {
            // Repair an interrupted older append before init can report
            // success (cacerts.go:110-112).
            data = reconcile(&data, state.owned.as_deref().unwrap_or(&[]), certs);
        } else {
            // Ordinary installs copy existing bytes without decoding the
            // bundle (cacerts.go:113-117).
            data.push(b'\n');
            data.extend_from_slice(state.desired_pem.as_bytes());
        }
        write_file_atomic(&self.bundle_path, &data)
            .await
            .map_err(|error| failed(format!("publish CA bundle: {error}")))?;
        fields.installed = true;
        Ok(())
    }

    /// `cacerts.go:237` loadState: missing state never authorizes removal of
    /// existing trust; an existing file always marks pending intent.
    async fn load_state(&self, fields: &mut CaFields) -> Result<(), CaError> {
        if fields.loaded {
            return Ok(());
        }
        match tokio::fs::read(&self.state_path).await {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fields.loaded = true;
            }
            Err(error) => {
                return Err(failed(format!(
                    "read CA state: {}",
                    go_path_error("open", &self.state_path, &error)
                )))
            }
            Ok(data) => {
                let state = CaState::unmarshal(&data).map_err(failed)?;
                let certs = if state.desired_pem.is_empty() {
                    Bundle::new()
                } else {
                    parse(&state.desired_pem)
                        .map_err(|error| failed(format!("parse CA state: {error}")))?
                };
                fields.state = Some(state);
                fields.requested = certs;
                fields.loaded = true;
                fields.pending = true;
            }
        }
        Ok(())
    }

    /// `cacerts.go:267` saveState: intent lands atomically at 0600 in a 0700
    /// directory before the aggregate is touched.
    async fn save_state(&self, state: &CaState) -> Result<(), CaError> {
        let data = state.marshal();
        if let Some(parent) = self.state_path.parent() {
            mkdir_all_mode(parent, 0o700)
                .map_err(|error| failed(format!("create CA state directory: {error}")))?;
        }
        let tmp = prepare_file(&self.state_path, data.as_bytes(), 0o600)
            .await
            .map_err(|error| failed(format!("prepare CA state: {error}")))?;
        if let Err(error) = tokio::fs::rename(&tmp, &self.state_path).await {
            let error = go_rename_error(&tmp, &self.state_path, &error);
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(failed(format!("publish CA state: {error}")));
        }
        Ok(())
    }

    /// `cacerts.go:153` persist: one reconcile pass. Source writes run outside
    /// the slot; every publish is gated on the generation captured under it
    /// (Go's `c.state != state` pointer compare).
    pub async fn persist(&self) -> Result<(), CaError> {
        let (state, certs, generation) = {
            let mut fields = self.mu.lock().await;
            self.load_state(&mut fields).await?;
            if !fields.pending {
                return Ok(());
            }
            (
                fields.state.clone().expect("pending implies state"),
                fields.requested.clone(),
                fields.generation,
            )
        };

        // Aggregate cleanup.
        let content = tokio::fs::read(&self.bundle_path).await.map_err(|error| {
            failed(format!(
                "read CA bundle for cleanup: {}",
                go_path_error("open", &self.bundle_path, &error)
            ))
        })?;
        let content = reconcile(&content, state.owned.as_deref().unwrap_or(&[]), &certs);
        // Go returns prepareFile's PathError unwrapped (cacerts.go:181-184).
        let tmp = prepare_file(&self.bundle_path, &content, 0o644)
            .await
            .map_err(failed)?;
        {
            let mut fields = self.mu.lock().await;
            if fields.generation != generation {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Ok(());
            }
            if let Err(error) = tokio::fs::rename(&tmp, &self.bundle_path).await {
                let error = go_rename_error(&tmp, &self.bundle_path, &error);
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(failed(format!("publish CA bundle: {error}")));
            }
            fields.installed = true;
        }

        // Rootfs failures must not prevent retirement in the active bundle
        // (cacerts.go:201-207).
        write_cert_files(&self.cert_dir, &certs).await?;
        self.remove_retired_cert_files(&certs).await?;

        // Forget retired identities only after source files and the aggregate
        // are cleaned (cacerts.go:226). nextOwned starts nil
        // (cacerts.go:189-193), so an empty requested set persists as null.
        let next_state = CaState {
            desired_pem: state.desired_pem.clone(),
            owned: if certs.is_empty() {
                None
            } else {
                Some(certs.iter().map(|cert| cert.fingerprint.clone()).collect())
            },
        };
        let data = next_state.marshal();
        let state_tmp = prepare_file(&self.state_path, data.as_bytes(), 0o600)
            .await
            .map_err(failed)?;
        let mut fields = self.mu.lock().await;
        if fields.generation != generation {
            let _ = tokio::fs::remove_file(&state_tmp).await;
            return Ok(());
        }
        if let Err(error) = tokio::fs::rename(&state_tmp, &self.state_path).await {
            let error = go_rename_error(&state_tmp, &self.state_path, &error);
            let _ = tokio::fs::remove_file(&state_tmp).await;
            return Err(failed(format!("publish CA state: {error}")));
        }
        fields.state = Some(next_state);
        fields.installed = true;
        fields.pending = false;
        Ok(())
    }

    /// `cacerts.go:301` removeRetiredCertFiles: prune stale managed names,
    /// leave foreign files alone, route the legacy alias through its own
    /// latest-request check.
    async fn remove_retired_cert_files(&self, certs: &Bundle) -> Result<(), CaError> {
        let wanted: HashSet<&str> = certs.iter().map(|cert| cert.fingerprint.as_str()).collect();
        let mut entries = tokio::fs::read_dir(&self.cert_dir).await.map_err(|error| {
            // Go os.ReadDir: the open failure is a plain PathError.
            failed(format!(
                "read CA cert dir: {}",
                go_path_error("open", &self.cert_dir, &error)
            ))
        })?;
        loop {
            let entry = entries.next_entry().await.map_err(|error| {
                // Go File.ReadDir: readdirent errors carry the directory.
                failed(format!(
                    "read CA cert dir: {}",
                    go_path_error("readdirent", &self.cert_dir, &error)
                ))
            })?;
            let Some(entry) = entry else { break };
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == LEGACY_CA_CERT_FILE {
                self.remove_legacy_cert(&entry.path()).await?;
                continue;
            }
            let Some(fingerprint) = managed_fingerprint(&name) else {
                continue;
            };
            if wanted.contains(fingerprint) {
                continue;
            }
            if let Err(error) = tokio::fs::remove_file(entry.path()).await {
                if error.kind() != std::io::ErrorKind::NotFound {
                    return Err(failed(format!(
                        "remove stale CA cert: {}",
                        go_path_error("remove", &entry.path(), &error)
                    )));
                }
            }
        }
        Ok(())
    }

    /// `cacerts.go:335` removeLegacyCert: keep the alias while the latest
    /// request still needs it (existing OpenSSL hash links resolve through
    /// the legacy source filename); check and unlink under the slot so init
    /// cannot adopt the alias in between.
    async fn remove_legacy_cert(&self, path: &Path) -> Result<(), CaError> {
        let data = tokio::fs::read(path).await.map_err(|error| {
            failed(format!(
                "read legacy CA cert: {}",
                go_path_error("open", path, &error)
            ))
        })?;
        let legacy = parse(&String::from_utf8_lossy(&data));
        let fields = self.mu.lock().await;
        if let Ok(legacy) = legacy {
            if legacy.len() == 1
                && fields
                    .requested
                    .iter()
                    .any(|cert| cert.fingerprint == legacy[0].fingerprint)
            {
                return Ok(());
            }
        }
        if let Err(error) = tokio::fs::remove_file(path).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(failed(format!(
                    "remove legacy CA cert: {}",
                    go_path_error("remove", path, &error)
                )));
            }
        }
        Ok(())
    }

    /// `cacerts.go:131` run: resume persisted intent immediately and retry
    /// interrupted work once a tick; the first consecutive failure logs, the
    /// rest stay silent until a success resets the flag.
    pub async fn run(&self, mut stop: tokio::sync::watch::Receiver<bool>) {
        let mut wake = self.wake_rx.lock().await;
        let mut failed = false;
        loop {
            match self.persist().await {
                Ok(()) => failed = false,
                Err(error) => {
                    if !failed && !*stop.borrow() {
                        failed = true;
                        eprintln!("Failed to reconcile CA certificates: {error}");
                    }
                }
            }
            tokio::select! {
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        return;
                    }
                }
                _ = wake.recv() => {}
                _ = tokio::time::sleep(self.tick) => {}
            }
        }
    }
}

/// `cacerts.go:287` writeCertFiles.
async fn write_cert_files(cert_dir: &Path, certs: &Bundle) -> Result<(), CaError> {
    mkdir_all_mode(cert_dir, 0o755)
        .map_err(|error| failed(format!("create CA cert dir: {error}")))?;
    for cert in certs {
        let path = cert_dir.join(make_cert_name(&cert.fingerprint));
        write_file_atomic(&path, &cert.pem)
            .await
            .map_err(|error| failed(format!("persist CA cert: {error}")))?;
    }
    Ok(())
}

/// Go os.MkdirAll(path, mode): create every missing component with the mode;
/// an existing non-directory fails as Go's mkdir/ENOTDIR PathError.
fn mkdir_all_mode(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    if path.as_os_str().is_empty() || path == Path::new("/") {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            mkdir_all_mode(parent, mode)?;
        }
    }
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(format!("mkdir {}: not a directory", path.display())),
        Err(_) => {
            use std::os::unix::fs::DirBuilderExt;
            match std::fs::DirBuilder::new().mode(mode).create(path) {
                Ok(()) => std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
                    .map_err(|error| go_path_error("chmod", path, &error)),
                Err(create_error) => match std::fs::metadata(path) {
                    Ok(meta) if meta.is_dir() => Ok(()),
                    _ => Err(go_path_error("mkdir", path, &create_error)),
                },
            }
        }
    }
}

/// `cacerts.go:365` prepareFile: stage a complete replacement beside the
/// destination (created, chmod'd to the final mode, then written — no fsync,
/// matching the pinned durability model of rename-only). Errors are Go
/// PathError texts because Go returns them unwrapped to its callers.
async fn prepare_file(path: &Path, data: &[u8], mode: u32) -> Result<PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = directory.join(format!(
        ".{}.tmp{}-{}",
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        std::process::id(),
        unique
    ));
    let mut file = tokio::fs::File::create(&tmp)
        .await
        .map_err(|error| go_path_error("open", &tmp, &error))?;
    let result = async {
        use tokio::io::AsyncWriteExt;
        file.set_permissions(std::fs::Permissions::from_mode(mode))
            .await
            .map_err(|error| go_path_error("chmod", &tmp, &error))?;
        file.write_all(data)
            .await
            .map_err(|error| go_path_error("write", &tmp, &error))
    }
    .await;
    if let Err(error) = result {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(error);
    }
    Ok(tmp)
}

/// `cacerts.go:387` writeFileAtomic.
async fn write_file_atomic(path: &Path, data: &[u8]) -> Result<(), String> {
    let tmp = prepare_file(path, data, 0o644).await?;
    if let Err(error) = tokio::fs::rename(&tmp, path).await {
        let error = go_rename_error(&tmp, path, &error);
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(error);
    }
    Ok(())
}
