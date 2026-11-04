//! Windows boot guard: the platform equivalent of Linux's pid-1 + kernel
//! cmdline check (`guest::validate_guest_boot`).
//!
//! Windows has no pid 1 and no `/proc/cmdline`. The declared equivalent is:
//! - SCM service mode is a guest by construction (only an installed service
//!   is dispatched), so it needs no marker;
//! - console mode must opt in with `COORWORKER_ENVD=1`;
//! - development mode must bind a loopback address;
//! - no mode may run as the builtin SYSTEM account outside the SCM, because
//!   that is the host-privileged identity a sandbox must never expose on a
//!   console.

use std::net::SocketAddr;

/// The environment marker a console guest exports before starting envd.
pub const GUEST_MARKER: &str = "COORWORKER_ENVD";

/// How the daemon was launched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Started by the Service Control Manager.
    Service,
    /// SCM fallback or a marked console guest.
    Console,
    /// `--development`: local, loopback-only, ordinary user.
    Development,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Service => "service",
            Mode::Console => "console",
            Mode::Development => "development",
        }
    }
}

/// Reads the guest marker from the process environment.
pub fn marker_from_env() -> Option<String> {
    std::env::var(GUEST_MARKER).ok()
}

/// Validates the launch mode. `marker` is the value of [`GUEST_MARKER`]
/// (`None` when unset), and `is_system` reports whether the process token is
/// the builtin SYSTEM account.
pub fn validate(
    mode: Mode,
    address: SocketAddr,
    marker: Option<&str>,
    is_system: bool,
) -> Result<(), String> {
    match mode {
        Mode::Service => Ok(()),
        Mode::Console => {
            if marker != Some("1") {
                return Err(format!(
                    "guest mode requires explicit VM boot opt-in ({GUEST_MARKER}=1)"
                ));
            }
            if is_system {
                return Err(
                    "guest mode requires an ordinary user; the SYSTEM account is reserved for the \
                     Service Control Manager"
                        .to_owned(),
                );
            }
            Ok(())
        }
        Mode::Development => {
            if !address.ip().is_loopback() || is_system {
                return Err(
                    "development mode requires an ordinary user and loopback address; guest mode \
                     requires explicit VM boot opt-in"
                        .to_owned(),
                );
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(ip: &str) -> SocketAddr {
        SocketAddr::new(ip.parse().unwrap(), 49983)
    }

    #[test]
    fn service_mode_is_a_guest_by_construction() {
        assert!(validate(Mode::Service, address("0.0.0.0"), None, true).is_ok());
    }

    #[test]
    fn console_mode_requires_the_marker_and_an_ordinary_user() {
        assert!(validate(Mode::Console, address("0.0.0.0"), None, false).is_err());
        assert!(validate(Mode::Console, address("0.0.0.0"), Some("0"), false).is_err());
        assert!(validate(Mode::Console, address("0.0.0.0"), Some("1"), true).is_err());
        assert!(validate(Mode::Console, address("0.0.0.0"), Some("1"), false).is_ok());
    }

    #[test]
    fn development_mode_is_loopback_only_and_never_system() {
        assert!(validate(Mode::Development, address("127.0.0.1"), None, false).is_ok());
        assert!(validate(Mode::Development, address("::1"), None, false).is_ok());
        let error = validate(Mode::Development, address("0.0.0.0"), None, false).unwrap_err();
        assert!(error.contains("loopback"), "{error}");
        assert!(validate(Mode::Development, address("127.0.0.1"), None, true).is_err());
    }
}
