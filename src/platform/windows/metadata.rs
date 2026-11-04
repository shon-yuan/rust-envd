//! Access-token delivery from the control plane, replacing the Linux MMDS
//! route.
//!
//! The provisioning file is `%ProgramData%\coorworker\envd\metadata.bin`: a
//! DPAPI (user-scope) protected JSON document
//! `{accessToken, instanceId, envId, address}`. The plaintext is zeroized
//! after parsing and the DPAPI allocation is wiped and released with
//! `LocalFree`. A missing file means "nothing provisioned" (`Ok(None)`); any
//! other read, decrypt or parse failure is an error the caller degrades to the
//! permissive first `/init`, exactly like Linux without MMDS. Tokens are never
//! logged or rendered into an error string.
use std::{ffi::c_void, fs, io, path::PathBuf};

use serde::Deserialize;
use windows_sys::Win32::Foundation::{LocalFree, HLOCAL};
use windows_sys::Win32::Security::Cryptography::{
    CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};
use zeroize::{Zeroize, Zeroizing};

use crate::platform::{MetadataBackend, MetadataIds};

#[derive(Debug)]
pub struct WindowsMetadataBackend {
    path: PathBuf,
}

impl Default for WindowsMetadataBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowsMetadataBackend {
    /// Production: the machine's `%ProgramData%` provisioning file.
    pub fn new() -> Self {
        Self {
            path: default_metadata_path(),
        }
    }

    /// Test/provisioning seam: read a specific file without changing the
    /// production location.
    pub fn with_path(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    fn load(&self) -> io::Result<Option<MetadataFile>> {
        let encrypted = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let plaintext = decrypt(&encrypted)?;
        let record = serde_json::from_slice(plaintext.as_slice())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        Ok(Some(record))
    }
}

impl MetadataBackend for WindowsMetadataBackend {
    fn init_token(&self) -> io::Result<Option<Vec<u8>>> {
        let Some(mut record) = self.load()? else {
            return Ok(None);
        };
        let token = record.access_token.as_bytes().to_vec();
        // Wipe the parsed String's allocation; the returned copy is wrapped in
        // `Zeroizing` by `init_token_sha512`.
        record.access_token.zeroize();
        // An absent/empty token means the control plane has not provisioned
        // one; enforce nothing rather than hashing the empty string.
        if token.is_empty() {
            return Ok(None);
        }
        Ok(Some(token))
    }

    fn metadata_ids(&self) -> io::Result<Option<MetadataIds>> {
        let Some(record) = self.load()? else {
            return Ok(None);
        };
        let MetadataFile {
            mut access_token,
            instance_id,
            env_id,
            address,
        } = record;
        access_token.zeroize();
        Ok(Some(MetadataIds {
            instance_id,
            env_id,
            address,
        }))
    }
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetadataFile {
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    instance_id: String,
    #[serde(default)]
    env_id: String,
    #[serde(default)]
    address: String,
}

impl std::fmt::Debug for MetadataFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetadataFile")
            .field("access_token", &"<redacted>")
            .field("instance_id", &self.instance_id)
            .field("env_id", &self.env_id)
            .field("address", &self.address)
            .finish()
    }
}

fn default_metadata_path() -> PathBuf {
    let mut path = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    path.push("coorworker");
    path.push("envd");
    path.push("metadata.bin");
    path
}

/// DPAPI user-scope decrypt. The returned plaintext is zeroized on drop.
fn decrypt(blob: &[u8]) -> io::Result<Zeroizing<Vec<u8>>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: blob.len() as u32,
        pbData: blob.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: input points at `blob` for the call's duration; the optional
    // entropy/description/reserved/prompt arguments are null, which DPAPI
    // accepts; output is a writable BLOB the API fills and allocates with
    // LocalAlloc.
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let plaintext = if output.pbData.is_null() || output.cbData == 0 {
        Zeroizing::new(Vec::new())
    } else {
        // SAFETY: DPAPI allocated cbData bytes at pbData.
        let copied =
            unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
        // SAFETY: same allocation; wipe the DPAPI-owned copy before freeing.
        unsafe {
            std::ptr::write_bytes(output.pbData, 0, output.cbData as usize);
        }
        Zeroizing::new(copied)
    };
    // SAFETY: pbData came from LocalAlloc inside DPAPI.
    unsafe {
        let _ = LocalFree(output.pbData.cast::<c_void>() as HLOCAL);
    }
    Ok(plaintext)
}

/// Deterministic test double; never compiled into the production service.
#[cfg(test)]
#[derive(Clone, Debug)]
pub struct StaticMetadata {
    token: Option<Vec<u8>>,
    ids: MetadataIds,
}

#[cfg(test)]
impl StaticMetadata {
    pub fn with_values(
        token: Option<&str>,
        instance_id: &str,
        env_id: &str,
        address: &str,
    ) -> Self {
        Self {
            token: token.map(|token| token.as_bytes().to_vec()),
            ids: MetadataIds {
                instance_id: instance_id.to_owned(),
                env_id: env_id.to_owned(),
                address: address.to_owned(),
            },
        }
    }
}

#[cfg(test)]
impl MetadataBackend for StaticMetadata {
    fn init_token(&self) -> io::Result<Option<Vec<u8>>> {
        Ok(self.token.clone())
    }

    fn metadata_ids(&self) -> io::Result<Option<MetadataIds>> {
        Ok(Some(self.ids.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::MetadataBackend;
    use sha2::{Digest, Sha512};

    #[test]
    fn token_hash_is_sha512_hex() {
        let metadata = StaticMetadata::with_values(Some("probe"), "", "", "");
        let expected = format!("{:x}", Sha512::digest(b"probe"));
        assert_eq!(expected.len(), 128);
        assert_eq!(expected, expected.to_ascii_lowercase());
        let hash = metadata.init_token_sha512().expect("hash");
        assert_eq!(hash.as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn missing_file_is_none_and_malformed_degrades() {
        let directory = tempfile::tempdir().expect("tempdir");
        let missing = WindowsMetadataBackend::with_path(directory.path().join("absent.bin"));
        assert_eq!(missing.init_token().expect("missing"), None);
        assert_eq!(missing.metadata_ids().expect("missing ids"), None);

        let corrupt = directory.path().join("corrupt.bin");
        fs::write(&corrupt, b"not a DPAPI blob").expect("write corrupt");
        let backend = WindowsMetadataBackend::with_path(&corrupt);
        assert!(backend.init_token().is_err());

        if let Ok(encrypted) = protect(b"{not json") {
            let invalid = directory.path().join("invalid-json.bin");
            fs::write(&invalid, encrypted).expect("write invalid json");
            assert!(WindowsMetadataBackend::with_path(&invalid)
                .init_token()
                .is_err());
        }
    }

    #[test]
    fn dpapi_round_trip() {
        let plaintext = br#"{"accessToken":"token-123","instanceId":"sandbox-7","envId":"template-9","address":"10.0.0.5:5000"}"#;
        let Ok(encrypted) = protect(plaintext) else {
            eprintln!("skipping: DPAPI unavailable in this session");
            return;
        };
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("metadata.bin");
        fs::write(&path, encrypted).expect("write metadata");
        let backend = WindowsMetadataBackend::with_path(&path);
        assert_eq!(
            backend.init_token().expect("token"),
            Some(b"token-123".to_vec())
        );
        let ids = backend.metadata_ids().expect("ids").expect("provisioned");
        assert_eq!(ids.instance_id, "sandbox-7");
        assert_eq!(ids.env_id, "template-9");
        assert_eq!(ids.address, "10.0.0.5:5000");
        let expected = format!("{:x}", Sha512::digest(b"token-123"));
        assert_eq!(backend.init_token_sha512().expect("hash"), Some(expected));
    }

    /// Test-only DPAPI protect, mirroring what the control plane writes.
    fn protect(plaintext: &[u8]) -> io::Result<Vec<u8>> {
        use windows_sys::Win32::Security::Cryptography::CryptProtectData;
        let input = CRYPT_INTEGER_BLOB {
            cbData: plaintext.len() as u32,
            pbData: plaintext.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB::default();
        // SAFETY: input points at `plaintext` for the call; output is filled
        // and allocated by crypt32.
        let ok = unsafe {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let encrypted = if output.pbData.is_null() || output.cbData == 0 {
            Vec::new()
        } else {
            // SAFETY: DPAPI allocated cbData bytes at pbData.
            unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec()
        };
        // SAFETY: pbData came from LocalAlloc inside DPAPI.
        unsafe {
            let _ = LocalFree(output.pbData.cast::<c_void>() as HLOCAL);
        }
        Ok(encrypted)
    }
}
