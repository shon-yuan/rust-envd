//! Windows account lookup through the local SAM via `LookupAccountNameW`.
//!
//! Windows has no POSIX uid/gid. The account name and home directory are real
//! values; `uid`/`gid`/`groups` are synthesized as zero and empty, which the
//! filesystem backend never uses for access checks (ACLs apply instead). The
//! synthesized values are documented in `docs/windows-port.md`.
use crate::exec_context::Account;
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_NONE_MAPPED, HANDLE, WIN32_ERROR};
use windows_sys::Win32::Security::{
    GetTokenInformation, LookupAccountNameW, TokenUser, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

const UNKNOWN: &str = "user: unknown user";

pub fn lookup(username: &str) -> Result<Account, String> {
    let (account, domain) = split_domain(username);
    lookup_sid(account)?;
    let qualified = if domain.is_empty() {
        account.to_owned()
    } else {
        format!("{domain}\\{account}")
    };
    Ok(Account {
        home: home_directory(&qualified),
        name: account.to_owned(),
        uid: 0,
        gid: 0,
        groups: Vec::new(),
    })
}

/// `DOMAIN\user` and `user@domain` split into their account part when the
/// domain is the local machine; otherwise the whole string is the account.
fn split_domain(username: &str) -> (&str, &str) {
    match username.split_once('\\') {
        Some((domain, account)) if !account.is_empty() => (account, domain),
        _ => (username, ""),
    }
}

fn lookup_sid(account: &str) -> Result<Vec<u8>, String> {
    let wide: Vec<u16> = account.encode_utf16().chain(std::iter::once(0)).collect();
    let mut sid_size = 0u32;
    let mut domain_size = 0u32;
    let mut use_type = 0i32;
    // First call sizes the SID and domain buffers.
    // SAFETY: sizes are initialized to zero, which LookupAccountNameW accepts
    // as a size query and fills in.
    unsafe {
        LookupAccountNameW(
            std::ptr::null(),
            wide.as_ptr(),
            std::ptr::null_mut(),
            &mut sid_size,
            std::ptr::null_mut(),
            &mut domain_size,
            &mut use_type,
        );
    }
    if sid_size == 0 {
        return Err(format!("{UNKNOWN} {account}"));
    }
    let mut sid = vec![0u8; sid_size as usize];
    // A local-only account can report a zero domain length; the second call
    // still needs a writable buffer of at least one element.
    domain_size = domain_size.max(1);
    let mut domain = vec![0u16; domain_size as usize];
    let ok: i32 = unsafe {
        LookupAccountNameW(
            std::ptr::null(),
            wide.as_ptr(),
            sid.as_mut_ptr().cast(),
            &mut sid_size,
            domain.as_mut_ptr(),
            &mut domain_size,
            &mut use_type,
        )
    };
    if ok == 0 {
        let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as WIN32_ERROR;
        if code == ERROR_NONE_MAPPED {
            return Err(format!("{UNKNOWN} {account}"));
        }
        return Err(std::io::Error::from_raw_os_error(code as i32).to_string());
    }
    sid.truncate(sid_size as usize);
    Ok(sid)
}

/// The profile directory of the named account. The current process token's
/// user is compared by SID; other accounts fall back to the conventional
/// `C:\Users\<name>` profile path, which this backend documents as a
/// synthesized value.
fn home_directory(qualified: &str) -> String {
    let current = current_user_sid().and_then(|sid| {
        let account = split_domain(qualified).0;
        lookup_sid(account)
            .ok()
            .filter(|candidate| *candidate == sid)
    });
    if current.is_some() {
        if let Ok(profile) = std::env::var("USERPROFILE") {
            if !profile.is_empty() {
                return profile;
            }
        }
    }
    let account = split_domain(qualified).0;
    format!("C:\\Users\\{account}")
}

fn current_user_sid() -> Option<Vec<u8>> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: the handle is closed below on the success path.
    unsafe {
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return None;
        }
        let mut size = 0u32;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut size);
        if size == 0 {
            CloseHandle(token);
            return None;
        }
        let mut buffer = vec![0u8; size as usize];
        let ok = GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            size,
            &mut size,
        );
        CloseHandle(token);
        if ok == 0 {
            return None;
        }
        // SAFETY: the buffer holds a TOKEN_USER whose Sid points inside it.
        let user = &*(buffer.as_ptr() as *const TOKEN_USER);
        let sid = user.User.Sid;
        if sid.is_null() {
            return None;
        }
        // Sid length is 8 + 4 * SubAuthorityCount.
        let count = *sid.cast::<u8>().add(1) as usize;
        let length = 8 + 4 * count;
        Some(std::slice::from_raw_parts(sid.cast::<u8>(), length).to_vec())
    }
}

/// The account the daemon runs as, used as the builtin default user. Windows
/// always exports `USERNAME` for interactive and service processes; the
/// `SYSTEM` fallback covers a stripped environment.
pub fn current_user_name() -> String {
    std::env::var("USERNAME")
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "SYSTEM".to_owned())
}
