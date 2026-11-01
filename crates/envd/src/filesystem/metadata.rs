//! user.e2b.* metadata: pinned shared filesystem/xattr.go semantics.
use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::path::Path;
use xattr::FileExt;

const PREFIX: &str = "user.e2b.";
const MAX_KEY: usize = 255 - PREFIX.len();
const MAX_TOTAL: usize = 4096;

pub fn validate_metadata(values: &HashMap<String, String>) -> io::Result<()> {
    let mut total = 0usize;
    for (key, value) in values {
        if key.is_empty() || key.len() > MAX_KEY {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid metadata key length",
            ));
        }
        if !key
            .bytes()
            .chain(value.bytes())
            .all(|b| (0x20..=0x7e).contains(&b))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "metadata must be printable ASCII",
            ));
        }
        total = total.saturating_add(PREFIX.len() + key.len() + value.len());
    }
    if total > MAX_TOTAL {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "metadata exceeds 4096 bytes",
        ));
    }
    Ok(())
}

/// Follow symlinks as the Go xattr library does. Unsupported filesystems read empty.
pub fn read_metadata(path: &Path) -> io::Result<HashMap<String, String>> {
    let names = match xattr::list_deref(path) {
        Ok(names) => names,
        Err(err) if is_unsupported(&err) => return Ok(HashMap::new()),
        Err(err) => return Err(err),
    };
    let mut result = HashMap::new();
    for name in names {
        let Some(key) = name.to_str().and_then(|name| name.strip_prefix(PREFIX)) else {
            continue;
        };
        if let Some(value) = xattr::get_deref(path, &name)? {
            if let Ok(value) = String::from_utf8(value) {
                result.insert(key.to_owned(), value);
            }
        }
    }
    Ok(result)
}

/// Replace only our namespace using an open FD, preserving unrelated attributes.
/// Unsupported writes remain errors for the upload caller to report/degrade explicitly.
pub fn write_metadata(file: &File, values: &HashMap<String, String>) -> io::Result<()> {
    validate_metadata(values)?;
    for name in file.list_xattr()? {
        let ours = name.as_encoded_bytes().starts_with(PREFIX.as_bytes());
        let keep = name
            .to_str()
            .and_then(|name| name.strip_prefix(PREFIX))
            .is_some_and(|key| values.contains_key(key));
        if ours && !keep {
            match file.remove_xattr(&name) {
                Ok(()) => {}
                Err(err) if err.raw_os_error() == Some(nix::libc::ENODATA) => {}
                Err(err) => return Err(err),
            }
        }
    }
    for (key, value) in values {
        file.set_xattr(format!("{PREFIX}{key}"), value.as_bytes())?;
    }
    Ok(())
}

pub fn is_unsupported(error: &io::Error) -> bool {
    error.raw_os_error() == Some(nix::libc::EOPNOTSUPP)
}
