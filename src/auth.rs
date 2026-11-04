//! Guest access-token and signed-file policy. Host-control routing is a separate seam.
use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine};
#[cfg(not(windows))]
use seckey::SecBytes;
use sha2::{Digest, Sha256, Sha512};
use std::collections::BTreeMap;
use std::sync::Mutex;
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, Zeroizing};

/// Pages held locked by live tokens, refcounted per page.
///
/// `VirtualLock` does not nest: unlocking a page releases it for every owner.
/// seckey allocates small buffers from the C allocator, so two tokens can share
/// a page — `Access::set_token` builds the replacement before dropping the old
/// token — and a per-token guard would unlock the survivor's pages, whose own
/// `Drop` then fails with `ERROR_NOT_LOCKED`. The counters keep a page locked
/// until its last owner goes away.
static LOCKED_PAGES: Mutex<BTreeMap<usize, LockedPage>> = Mutex::new(BTreeMap::new());

/// One locked page and its owner count. The guard is what keeps the page
/// resident; dropping the entry (the last owner) releases it, so no path ever
/// unlocks a page another token still relies on.
struct LockedPage {
    owners: usize,
    _guard: region::LockGuard,
}

/// The page locks a token owns; released in `Drop`.
struct PageLocks {
    pages: Vec<usize>,
}

impl PageLocks {
    fn acquire(address: *const u8, len: usize) -> Result<Self, AuthError> {
        let page = region::page::size();
        let start = (address as usize) / page * page;
        let end = (address as usize)
            .checked_add(len)
            .ok_or(AuthError::TokenStorage)?
            .checked_add(page - 1)
            .ok_or(AuthError::TokenStorage)?
            / page
            * page;
        let pages: Vec<usize> = (start..end).step_by(page).collect();
        let mut locked = LOCKED_PAGES.lock().map_err(|_| AuthError::TokenStorage)?;

        // Lock every page this token needs that no other token holds yet. A
        // failed lock (the working-set quota) is rollback-safe: the guards of
        // the pages already taken are dropped here, before any owner is
        // counted.
        let mut acquired = Vec::new();
        for &page_start in &pages {
            if locked.contains_key(&page_start) {
                continue;
            }
            // SAFETY: `page_start` names a committed page of the live
            // allocation backing the token; the rounding arguments are
            // page-aligned, so exactly this page is locked.
            match region::lock(page_start as *const u8, page) {
                Ok(guard) => {
                    locked.insert(
                        page_start,
                        LockedPage {
                            owners: 0,
                            _guard: guard,
                        },
                    );
                    acquired.push(page_start);
                }
                Err(_) => {
                    for held in acquired.drain(..) {
                        locked.remove(&held);
                    }
                    return Err(AuthError::TokenStorage);
                }
            }
        }
        for &page_start in &pages {
            locked
                .get_mut(&page_start)
                .ok_or(AuthError::TokenStorage)?
                .owners += 1;
        }
        Ok(Self { pages })
    }
}

impl Drop for PageLocks {
    fn drop(&mut self) {
        let Ok(mut locked) = LOCKED_PAGES.lock() else {
            return;
        };
        for page_start in self.pages.drain(..) {
            let released = match locked.get_mut(&page_start) {
                Some(entry) => {
                    entry.owners -= 1;
                    entry.owners == 0
                }
                None => false,
            };
            if released {
                // Dropping the entry drops its `LockGuard`, which unlocks the
                // page exactly once, after the last owner is gone.
                locked.remove(&page_start);
            }
        }
    }
}

// The token buffer, plus the explicit, fallible page lock that keeps it
// resident for the token's entire life.
//
// On Unix seckey builds an isolated mapping with guard pages. Its Windows
// fallback is a plain heap allocation that toggles the page between
// `PAGE_NOACCESS` and `PAGE_READONLY` around every borrow, and a
// `VirtualProtect` to `PAGE_NOACCESS` drops the page's `VirtualLock` (the guest
// reports ERROR_NOT_LOCKED from the later `VirtualUnlock`). Windows therefore
// owns a fixed heap buffer and leaves its protection alone, so the lock
// acquired here stays valid until the buffer is dropped.
#[cfg(not(windows))]
struct ProtectedToken {
    _pages: PageLocks,
    bytes: Mutex<SecBytes>,
}

#[cfg(windows)]
struct ProtectedToken {
    _pages: PageLocks,
    bytes: Mutex<Box<[u8]>>,
}

#[cfg(not(windows))]
impl ProtectedToken {
    fn new(token: &str) -> Result<Self, AuthError> {
        // Allocation happens before copying the secret. seckey's constructor can
        // panic on allocation failure; no secret is present in that allocation yet.
        let mut bytes = std::panic::catch_unwind(|| SecBytes::new(token.len()))
            .map_err(|_| AuthError::TokenStorage)?;
        let pages = {
            let readable = bytes.read();
            PageLocks::acquire(readable.as_ptr(), readable.len())?
        };
        bytes.write().copy_from_slice(token.as_bytes());
        Ok(Self {
            _pages: pages,
            bytes: Mutex::new(bytes),
        })
    }

    fn with_bytes<T>(&self, read: impl FnOnce(&[u8]) -> T) -> Result<T, AuthError> {
        let bytes = self.bytes.lock().map_err(|_| AuthError::TokenStorage)?;
        let readable = bytes.read();
        Ok(read(&readable))
    }
}

#[cfg(windows)]
impl ProtectedToken {
    fn new(token: &str) -> Result<Self, AuthError> {
        // Allocate before copying the secret, and only lock after the buffer has
        // its final address: `into_boxed_slice` may move an over-reserved `Vec`.
        let mut buffer: Vec<u8> = Vec::new();
        buffer
            .try_reserve_exact(token.len())
            .map_err(|_| AuthError::TokenStorage)?;
        buffer.resize(token.len(), 0);
        let mut bytes: Box<[u8]> = buffer.into_boxed_slice();
        let pages = PageLocks::acquire(bytes.as_ptr(), bytes.len())?;
        bytes[..].copy_from_slice(token.as_bytes());
        Ok(Self {
            _pages: pages,
            bytes: Mutex::new(bytes),
        })
    }

    fn with_bytes<T>(&self, read: impl FnOnce(&[u8]) -> T) -> Result<T, AuthError> {
        let bytes = self.bytes.lock().map_err(|_| AuthError::TokenStorage)?;
        Ok(read(&bytes[..]))
    }
}

#[cfg(not(windows))]
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

#[cfg(windows)]
impl Drop for ProtectedToken {
    fn drop(&mut self) {
        // The `Drop` body runs before the fields do, so the token is wiped while
        // its pages are still locked; `_pages` then releases the lock.
        let bytes = self
            .bytes
            .get_mut()
            .unwrap_or_else(|error| error.into_inner());
        bytes[..].zeroize();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacing_a_token_keeps_only_the_new_one_valid() {
        let mut access = Access::fresh();
        access.set_token(Some("first".to_owned())).expect("first");
        access.set_token(Some("second".to_owned())).expect("second");
        assert!(access.matches_current_token(Some("second")));
        assert!(!access.matches_current_token(Some("first")));
    }

    /// Two live tokens can share a page: seckey's small allocations come from
    /// the C allocator, and `set_token` builds the replacement before dropping
    /// the old token. Unlocking must follow the last owner, not each token: a
    /// per-token guard would unlock a page the survivor still needs, and the
    /// survivor's own `Drop` then panics inside `region`'s guard.
    ///
    /// The registry is process-wide and other tests create tokens
    /// concurrently, so the assertion is the owner invariant rather than an
    /// empty registry: a page stays locked while an owner lives, and a
    /// released page leaves no ownerless entry behind.
    #[test]
    fn two_live_tokens_release_their_shared_pages_once() {
        let first = ProtectedToken::new("first").expect("first");
        let second = ProtectedToken::new("second").expect("second");
        {
            let registry = LOCKED_PAGES.lock().expect("registry");
            for page in &first._pages.pages {
                assert!(
                    registry.get(page).is_some_and(|entry| entry.owners > 0),
                    "page {page} must stay locked while its token lives"
                );
            }
        }
        drop(first);
        drop(second);
        assert!(LOCKED_PAGES
            .lock()
            .expect("registry")
            .values()
            .all(|entry| entry.owners > 0));
    }

    #[test]
    fn a_failed_token_store_keeps_the_old_token() {
        let mut access = Access::fresh();
        access.set_token(Some("token".to_owned())).expect("token");
        assert!(matches!(
            access.set_token(Some(String::new())),
            Err(AuthError::EmptyToken)
        ));
        assert!(access.matches_current_token(Some("token")));
    }
}
