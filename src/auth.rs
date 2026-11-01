//! Guest access-token and signed-file policy. Host-control routing is a separate seam.
use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine};
use seckey::SecBytes;
use sha2::{Digest, Sha256, Sha512};
use std::sync::Mutex;
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, Zeroizing};

// seckey isolates allocations with guard pages, but its allocator ignores mlock
// failures. Keep an explicit, fallible OS lock for the allocation's entire life.
struct ProtectedToken {
    _lock: region::LockGuard,
    bytes: Mutex<SecBytes>,
}

impl ProtectedToken {
    fn new(token: &str) -> Result<Self, AuthError> {
        // Allocation happens before copying the secret. seckey's constructor can
        // panic on allocation failure; no secret is present in that allocation yet.
        let mut bytes = std::panic::catch_unwind(|| SecBytes::new(token.len()))
            .map_err(|_| AuthError::TokenStorage)?;
        let lock = {
            let readable = bytes.read();
            region::lock(readable.as_ptr(), readable.len()).map_err(|_| AuthError::TokenStorage)?
        };
        bytes.write().copy_from_slice(token.as_bytes());
        Ok(Self {
            _lock: lock,
            bytes: Mutex::new(bytes),
        })
    }

    fn with_bytes<T>(&self, read: impl FnOnce(&[u8]) -> T) -> Result<T, AuthError> {
        let bytes = self.bytes.lock().map_err(|_| AuthError::TokenStorage)?;
        let readable = bytes.read();
        Ok(read(&readable))
    }
}

impl Drop for ProtectedToken {
    fn drop(&mut self) {
        // Wipe before releasing the explicit lock, then let seckey destroy its
        // isolated allocation. A poisoned mutex still owns valid SecBytes.
        let bytes = self
            .bytes
            .get_mut()
            .unwrap_or_else(|error| error.into_inner());
        bytes.write().zeroize();
    }
}

#[derive(Clone, Copy, Debug)]
pub enum ReadWrite {
    Read,
    Write,
}
impl ReadWrite {
    fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
        }
    }
}

#[derive(Clone, Copy)]
pub struct FileAccess<'a> {
    pub path: &'a str,
    pub operation: ReadWrite,
    pub username: Option<&'a str>,
    pub header_token: Option<&'a str>,
    pub signature: Option<&'a str>,
    pub expiration: Option<i64>,
    pub now: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AuthError {
    Unauthorized,
    NotInitialized,
    MissingToken,
    MissingSignature,
    InvalidSignature,
    Expired,
    InvalidUsername,
    EmptyToken,
    TokenStorage,
    ResetNotAuthorized,
    TokenMismatch,
}
impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unauthorized => "unauthorized access, please provide a valid access token or method signing if supported",
            Self::NotInitialized => "envd not initialized",
            Self::MissingToken => "access token is not set",
            Self::MissingSignature => "missing signature query parameter",
            Self::InvalidSignature => "invalid signature",
            Self::Expired => "signature is already expired",
            Self::InvalidUsername => "invalid signature: a user name cannot contain a colon",
            Self::EmptyToken => "empty token not allowed",
            Self::TokenStorage => "secure token storage unavailable",
            Self::ResetNotAuthorized => "access token reset not authorized",
            Self::TokenMismatch => "access token validation failed",
        })
    }
}
impl std::error::Error for AuthError {}

pub struct Access {
    token: Option<ProtectedToken>,
    handover: bool,
    initialized: bool,
}

impl Access {
    pub fn fresh() -> Self {
        Self {
            token: None,
            handover: false,
            initialized: false,
        }
    }
    pub fn handover() -> Self {
        Self {
            handover: true,
            ..Self::fresh()
        }
    }
    /// Rejects empty input and preserves the old token if protected storage fails.
    /// `None` explicitly clears the token; callers must authorize init first.
    pub fn set_token(&mut self, token: Option<String>) -> Result<(), AuthError> {
        let replacement = token
            .map(Zeroizing::new)
            .map(|token| {
                if token.is_empty() {
                    return Err(AuthError::EmptyToken);
                }
                ProtectedToken::new(&token)
            })
            .transpose()?;
        self.token = replacement;
        Ok(())
    }

    /// Only an installed matching token can bypass an MMDS lookup.
    pub fn matches_current_token(&self, request_token: Option<&str>) -> bool {
        request_token.is_some_and(|token| self.token_matches(token))
    }

    /// Pure init policy. MMDS lookup failures and non-Firecracker mode are
    /// represented by `None`; an empty hash is also absent in the Go policy.
    pub fn validate_init(
        &self,
        request_token: Option<&str>,
        mmds_hash: Option<&str>,
    ) -> Result<(), AuthError> {
        if request_token == Some("") {
            return Err(AuthError::EmptyToken);
        }
        if request_token.is_some_and(|token| self.token_matches(token)) {
            return Ok(());
        }
        let mmds_hash = mmds_hash.filter(|hash| !hash.is_empty());
        if let Some(expected) = mmds_hash {
            let digest = Sha512::digest(request_token.unwrap_or("").as_bytes());
            let actual = format!("{digest:x}");
            if bool::from(actual.as_bytes().ct_eq(expected.as_bytes())) {
                return Ok(());
            }
        }
        if self.token.is_none() && mmds_hash.is_none() {
            return Ok(());
        }
        Err(if request_token.is_none() {
            AuthError::ResetNotAuthorized
        } else {
            AuthError::TokenMismatch
        })
    }
    pub fn mark_initialized(&mut self) {
        self.initialized = true;
    }

    /// Go store.go:112-113 Initialized: flips true on the first authenticated
    /// /init. Gates /upgrade (main.go:427-433).
    pub fn initialized(&self) -> bool {
        self.initialized
    }

    fn token_matches(&self, candidate: &str) -> bool {
        self.token.as_ref().is_some_and(|token| {
            token
                .with_bytes(|bytes| bool::from(bytes.ct_eq(candidate.as_bytes())))
                .unwrap_or(false)
        })
    }

    pub fn authorize(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
    ) -> Result<(), AuthError> {
        let minimal = matches!((method, path), ("GET", "/health") | ("POST", "/init"));
        let excluded = minimal || matches!((method, path), ("GET", "/files") | ("POST", "/files"));
        if self.token.is_some() {
            if !excluded && !self.token_matches(token.unwrap_or("")) {
                return Err(AuthError::Unauthorized);
            }
        } else if self.handover && !self.initialized && !minimal {
            return Err(AuthError::NotInitialized);
        }
        Ok(())
    }

    pub fn signature(
        &self,
        path: &str,
        operation: ReadWrite,
        username: &str,
        expiration: Option<i64>,
    ) -> Result<String, AuthError> {
        let token = self.token.as_ref().ok_or(AuthError::MissingToken)?;
        let mut hash = Sha256::new();
        for field in [path, operation.as_str(), username] {
            hash.update(field.as_bytes());
            hash.update(b":");
        }
        token.with_bytes(|bytes| hash.update(bytes))?;
        if let Some(expiration) = expiration {
            hash.update(b":");
            hash.update(expiration.to_string().as_bytes());
        }
        Ok(format!("v1_{}", STANDARD_NO_PAD.encode(hash.finalize())))
    }

    pub fn authorize_file(&self, request: &FileAccess<'_>) -> Result<(), AuthError> {
        if self.token.is_none() {
            return Ok(());
        }
        if let Some(token) = request.header_token.filter(|token| !token.is_empty()) {
            return if self.token_matches(token) {
                Ok(())
            } else {
                Err(AuthError::Unauthorized)
            };
        }
        let supplied = request.signature.ok_or(AuthError::MissingSignature)?;
        let username = request.username.unwrap_or("");
        if username.contains(':') {
            return Err(AuthError::InvalidUsername);
        }
        let expected = self.signature(
            request.path,
            request.operation,
            username,
            request.expiration,
        )?;
        if !bool::from(expected.as_bytes().ct_eq(supplied.as_bytes())) {
            return Err(AuthError::InvalidSignature);
        }
        if request
            .expiration
            .is_some_and(|expiration| expiration < request.now)
        {
            return Err(AuthError::Expired);
        }
        Ok(())
    }
}
