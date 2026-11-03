//! Execution defaults and environment layering from the pinned envd contract.
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

/// Oracle execcontext.BuiltinDefaultUser (context.go:17): the user envd serves
/// as when nothing has told it otherwise. It lives here rather than in the
/// daemon so the handover layer can recognise "still on the fallback" — which,
/// on a handover boot, is an identity that was lost rather than a default that
/// was chosen.
pub const BUILTIN_DEFAULT_USER: &str = "root";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    pub home: String,
    pub uid: u32,
    pub gid: u32,
    pub groups: Vec<u32>,
}

#[derive(Clone, Debug)]
pub struct Defaults {
    pub user: String,
    pub workdir: Option<String>,
    pub user_delivered: bool,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            user: "root".into(),
            workdir: None,
            user_delivered: false,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ContextError {
    MissingUsername,
    UserSpecificHome,
    CurrentDirectory(std::io::ErrorKind),
    NonUtf8Path,
}

impl std::fmt::Display for ContextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingUsername => f.write_str("username not provided"),
            Self::UserSpecificHome => f.write_str("cannot expand user-specific home dir"),
            Self::CurrentDirectory(_) => f.write_str("failed to resolve current directory"),
            Self::NonUtf8Path => f.write_str("resolved path is not valid UTF-8"),
        }
    }
}
impl std::error::Error for ContextError {}

impl Defaults {
    pub fn resolve_username<'a>(
        &'a self,
        supplied: Option<&'a str>,
    ) -> Result<&'a str, ContextError> {
        supplied
            .or_else(|| (!self.user.is_empty()).then_some(self.user.as_str()))
            .ok_or(ContextError::MissingUsername)
    }

    pub fn resolve_path(&self, supplied: &str, user: &Account) -> Result<String, ContextError> {
        let path = if supplied.is_empty() {
            self.workdir.as_deref().unwrap_or("")
        } else {
            supplied
        };
        let expanded;
        let path = if let Some(rest) = path.strip_prefix('~') {
            if !rest.is_empty() && !rest.starts_with(['/', '\\']) {
                return Err(ContextError::UserSpecificHome);
            }
            expanded = clean_join(&user.home, rest);
            expanded.as_str()
        } else {
            path
        };
        if Path::new(path).is_absolute() {
            return Ok(path.to_owned());
        }
        let joined = clean_join(&user.home, path);
        if Path::new(&joined).is_absolute() {
            return Ok(joined);
        }
        let cwd = std::env::current_dir()
            .map_err(|error| ContextError::CurrentDirectory(error.kind()))?;
        let cwd = cwd.to_str().ok_or(ContextError::NonUtf8Path)?;
        Ok(clean_join(cwd, &joined))
    }
}

// Go filepath.Join cleans lexically; it does not resolve symlinks or require existence.
fn clean_join(home: &str, path: &str) -> String {
    let joined = match (home.is_empty(), path.is_empty()) {
        (true, _) => path.to_owned(),
        (_, true) => home.to_owned(),
        _ => format!("{home}/{path}"),
    };
    let mut result = PathBuf::new();
    for component in Path::new(&joined).components() {
        match component {
            Component::ParentDir => match result.components().next_back() {
                Some(Component::Normal(_)) => {
                    result.pop();
                }
                Some(Component::RootDir) => {}
                _ => result.push(".."),
            },
            Component::CurDir => {}
            component => result.push(component.as_os_str()),
        }
    }
    if result.as_os_str().is_empty() && !joined.is_empty() {
        ".".to_owned()
    } else {
        result.to_string_lossy().into_owned()
    }
}

/// Parse only the Basic username; password bytes are deliberately ignored.
pub(crate) fn basic_username(headers: &axum::http::HeaderMap) -> Option<String> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let value = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let (scheme, encoded) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let decoded = STANDARD.decode(encoded.replace(['\r', '\n'], "")).ok()?;
    let colon = decoded.iter().position(|byte| *byte == b':')?;
    Some(String::from_utf8_lossy(&decoded[..colon]).into_owned())
}

/// Resolve the Basic username (password is ignored), or the delivered default.
/// OS account and group lookup can block; HTTP adapters call this on a blocking worker.
pub fn request_account(
    defaults: &Defaults,
    ctx: &connectrpc::RequestContext,
) -> Result<Account, connectrpc::ConnectError> {
    let supplied = basic_username(ctx.headers());
    let username = defaults
        .resolve_username(supplied.as_deref())
        .map_err(|_| connectrpc::ConnectError::unauthenticated("no user specified"))?;
    let invalid = || {
        connectrpc::ConnectError::unauthenticated(format!(
            "invalid {}user: '{username}'",
            if supplied.is_some() { "" } else { "default " }
        ))
    };
    crate::platform::lookup_account(username).map_err(|_| invalid())
}

#[derive(Clone, Debug, Default)]
pub struct Environment {
    internal: BTreeMap<String, String>,
    user: BTreeMap<String, String>,
}

impl Environment {
    pub fn store_internal(&mut self, key: &str, value: &str) {
        self.internal.insert(key.to_owned(), value.to_owned());
        self.user.remove(key);
    }

    pub fn replace_user(&mut self, values: BTreeMap<String, String>) {
        self.user = values
            .into_iter()
            .filter(|(key, _)| !self.internal.contains_key(key))
            .collect();
    }

    pub fn all(&self) -> BTreeMap<String, String> {
        self.user
            .iter()
            .chain(self.internal.iter())
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }

    pub fn for_process(
        &self,
        user: &Account,
        daemon_path: Option<&str>,
        request: &BTreeMap<String, String>,
    ) -> BTreeMap<String, String> {
        let mut values = BTreeMap::from([
            ("HOME".into(), user.home.clone()),
            ("USER".into(), user.name.clone()),
            ("LOGNAME".into(), user.name.clone()),
        ]);
        if let Some(path) = daemon_path {
            values.insert("PATH".into(), path.to_owned());
        }
        values.extend(self.all());
        values.extend(
            request
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
        values
    }
}
