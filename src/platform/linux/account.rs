//! POSIX account lookup through the password database.
//!
//! Every adapter (HTTP auth, `/files`, Connect filesystem) resolves a username
//! through this one seam so the error text is identical across them.
use crate::exec_context::Account;

/// The `user: unknown user <name>` text Go's `os/user` returns, without the
/// `error looking up user '<name>': ` wrapper each caller adds.
const UNKNOWN: &str = "user: unknown user";

pub fn lookup(username: &str) -> Result<Account, String> {
    let user = nix::unistd::User::from_name(username)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("{UNKNOWN} {username}"))?;
    let name =
        std::ffi::CString::new(user.name.as_str()).map_err(|_| format!("{UNKNOWN} {username}"))?;
    let groups = nix::unistd::getgrouplist(&name, user.gid)
        .map_err(|error| error.to_string())?
        .iter()
        .map(|group| group.as_raw())
        .collect();
    Ok(Account {
        name: user.name,
        home: user.dir.to_string_lossy().into_owned(),
        uid: user.uid.as_raw(),
        gid: user.gid.as_raw(),
        groups,
    })
}
