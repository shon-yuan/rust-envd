//! The Go-compatible command-line surface (oracle `main.go:59-172`).
//!
//! The flag parser is shared by the Linux and Windows entry points so
//! `--version`, `--commit`, `-h`, unknown-flag diagnostics and exit codes are
//! identical on both platforms. `--guest`, `--development` and `--listen` are
//! this port's extensions; the oracle flags keep their exact names, types and
//! defaults.

use std::net::SocketAddr;

/// Go main.go:59-115, the flag set. cgroup_root defaults to /sys/fs/cgroup
/// (main.go:105-106); no_cgroups is the explicit opt-out of cgroup management
/// (main.go:112-114); resume_handover is the internal flag the outgoing envd
/// appends when it re-execs itself during a live self-upgrade
/// (main.go:122-128). development, guest and listen are this port's
/// extensions: development/guest select the safe boot guard, and listen (or
/// the official --port) supplies the bind address.
#[derive(Clone)]
pub struct Args {
    pub address: Option<SocketAddr>,
    pub port: Option<i64>,
    pub development: bool,
    pub guest: bool,
    pub is_not_fc: bool,
    pub no_cgroups: bool,
    pub cgroup_root: String,
    pub verbose: bool,
    pub resume_handover: bool,
    pub version: bool,
    pub commit: bool,
}

impl Args {
    pub fn defaults() -> Self {
        Self {
            address: None,
            port: None,
            development: false,
            guest: false,
            is_not_fc: false,
            no_cgroups: false,
            // Go main.go:105-106.
            cgroup_root: "/sys/fs/cgroup".to_owned(),
            // Go main.go:109-114.
            verbose: false,
            // Go main.go:122-128.
            resume_handover: false,
            version: false,
            commit: false,
        }
    }

    /// Go main.go:334 binds `0.0.0.0:<port>`, whose default is 49983
    /// (main.go:42, :96-101). A guest boot keeps that default; the development
    /// extension keeps its loopback-ephemeral default so it can never expose a
    /// development daemon off-host. --listen wins when both are given last.
    pub fn resolve_address(&self) -> Result<SocketAddr, String> {
        if let Some(address) = self.address {
            return Ok(address);
        }
        if let Some(port) = self.port {
            let port = u16::try_from(port)
                .map_err(|_| format!("listen tcp: address {port}: invalid port"))?;
            return Ok(SocketAddr::from(([0, 0, 0, 0], port)));
        }
        Ok(if self.guest {
            SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT))
        } else {
            SocketAddr::from(([127, 0, 0, 1], 0))
        })
    }
}

/// Go's `flag` errors are one diagnostic line followed by the default usage
/// text; `-h`/`-help` is its ErrHelp path, which prints only the usage.
#[derive(Debug)]
pub enum CliError {
    Help,
    Flag(String),
}

/// Go main.go:42: the official daemon's default port.
pub const DEFAULT_PORT: u16 = 49983;
/// Go main.go:162-166 / pkg/version.go.
pub const VERSION: &str = "0.10.2";
/// The pinned upstream envd source commit this port implements. Like VERSION,
/// this is the emulated baseline identity (the oracle build injects it into
/// main.commitSHA), not this repository's own build commit.
pub const COMMIT: &str = "51083a83cc0903185d992725ea6bb33683814b29";

pub fn usage() -> String {
    let program = std::env::args().next().unwrap_or_else(|| "envd".to_owned());
    let mut text = format!("Usage of {program}:\n");
    // Go's flag package lists the set in name order (main.go:74-131).
    for (name, kind, help) in [
        (
            "cgroup-root",
            " string",
            "cgroup root directory (default \"/sys/fs/cgroup\")",
        ),
        ("commit", "", "print envd source commit"),
        (
            "isnotfc",
            "",
            "run outside of Firecracker (skips MMDS poll and HTTP log exporter)",
        ),
        (
            "no-cgroups",
            "",
            "disable cgroup management; use a no-op cgroup manager instead",
        ),
        (
            "port",
            " int",
            "a port on which the daemon should run (default 49983)",
        ),
        (
            "resume-handover",
            "",
            "internal: re-adopt processes from the live-upgrade handover blob (set by the outgoing envd)",
        ),
        ("verbose", "", "write envd logs to stdout"),
        ("version", "", "print envd version"),
    ] {
        text.push_str(&format!("  -{name}{kind}\n    \t{help}\n"));
    }
    text
}

pub fn parse_args<I, S>(args: I) -> Result<Args, CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut args = args.into_iter().map(Into::into);
    let mut parsed = Args::defaults();
    while let Some(argument) = args.next() {
        // Go flag: "--" ends the flag list; the first non-flag argument (or a
        // bare "-") stops parsing and leaves the rest as positional arguments.
        if argument == "--" {
            break;
        }
        let Some(rest) = argument.strip_prefix('-') else {
            break;
        };
        let minuses = if rest.starts_with('-') { 2 } else { 1 };
        let body = &argument[minuses..];
        if body.is_empty() {
            break;
        }
        // Go parseOne rejects a name that starts with '-' or '=' before any
        // flag lookup, quoting the ORIGINAL argument with all its dashes
        // (probed on the oracle: `---x`, `-=x`, `--=x` all answer
        // `bad flag syntax: <arg>`, exit 2).
        if body.starts_with('-') || body.starts_with('=') {
            return Err(CliError::Flag(format!("bad flag syntax: {argument}")));
        }
        let (name, inline) = match body.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (body, None),
        };
        match name {
            // Go's flag package special-cases the names h/help in the
            // unknown-flag branch BEFORE any value handling, so every value
            // form (`-h=1`, `-help=`, `-h=false`) takes the ErrHelp path too
            // (probed on the oracle: usage on stderr, exit 0).
            "h" | "help" => return Err(CliError::Help),
            "version" => parsed.version = bool_flag(name, inline.as_deref())?,
            "commit" => parsed.commit = bool_flag(name, inline.as_deref())?,
            "isnotfc" => parsed.is_not_fc = bool_flag(name, inline.as_deref())?,
            "no-cgroups" => parsed.no_cgroups = bool_flag(name, inline.as_deref())?,
            "verbose" => parsed.verbose = bool_flag(name, inline.as_deref())?,
            "resume-handover" => parsed.resume_handover = bool_flag(name, inline.as_deref())?,
            "development" => parsed.development = bool_flag(name, inline.as_deref())?,
            "guest" => parsed.guest = bool_flag(name, inline.as_deref())?,
            "cgroup-root" => parsed.cgroup_root = value(name, inline, &mut args)?,
            "listen" => {
                let address = value(name, inline, &mut args)?;
                parsed.address =
                    Some(address.parse().map_err(|error: std::net::AddrParseError| {
                        CliError::Flag(format!(
                            "invalid value {address:?} for flag -{name}: {error}"
                        ))
                    })?);
                parsed.port = None;
            }
            "port" => {
                let raw = value(name, inline, &mut args)?;
                parsed.port = Some(go_int64(&raw).map_err(|reason| {
                    CliError::Flag(format!("invalid value {raw:?} for flag -{name}: {reason}"))
                })?);
                parsed.address = None;
            }
            _ => {
                return Err(CliError::Flag(format!(
                    "flag provided but not defined: -{name}"
                )))
            }
        }
    }
    Ok(parsed)
}

fn value(
    name: &str,
    inline: Option<String>,
    args: &mut impl Iterator<Item = String>,
) -> Result<String, CliError> {
    match inline {
        Some(value) => Ok(value),
        None => args
            .next()
            .ok_or_else(|| CliError::Flag(format!("flag needs an argument: -{name}"))),
    }
}

/// Go bool flags take no following argument; `-name` means true and
/// `-name=<strconv.ParseBool>` sets the value (probed on the oracle).
fn bool_flag(name: &str, inline: Option<&str>) -> Result<bool, CliError> {
    match inline {
        None => Ok(true),
        Some(raw) => match raw {
            "1" | "t" | "T" | "TRUE" | "true" | "True" => Ok(true),
            "0" | "f" | "F" | "FALSE" | "false" | "False" => Ok(false),
            _ => Err(CliError::Flag(format!(
                "invalid boolean value {raw:?} for -{name}: parse error"
            ))),
        },
    }
}

/// Go flag parses Int64Var with strconv.ParseInt(s, 0, 64): optional sign,
/// 0b/0o/0x prefixes, a leading 0 as octal, and Go integer-literal
/// underscores. Its diagnostics are exactly "parse error" and "value out of
/// range" (probed on the oracle).
pub fn go_int64(text: &str) -> Result<i64, &'static str> {
    let (negative, rest) = match text.as_bytes().first() {
        Some(b'+') => (false, &text[1..]),
        Some(b'-') => (true, &text[1..]),
        _ => (false, text),
    };
    if rest.is_empty() {
        return Err("parse error");
    }
    let (base, digits, prefixed) =
        if let Some(d) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
            (16, d, true)
        } else if let Some(d) = rest.strip_prefix("0b").or_else(|| rest.strip_prefix("0B")) {
            (2, d, true)
        } else if let Some(d) = rest.strip_prefix("0o").or_else(|| rest.strip_prefix("0O")) {
            (8, d, true)
        } else if rest.len() > 1 && rest.starts_with('0') {
            // A leading 0 is the octal base prefix, so `0_7` is valid.
            (8, &rest[1..], true)
        } else {
            (10, rest, false)
        };
    let digits = separators(digits, base, prefixed).ok_or("parse error")?;
    if digits.is_empty() {
        return Err("parse error");
    }
    let limit = if negative {
        (i64::MAX as i128) + 1
    } else {
        i64::MAX as i128
    };
    let mut value: i128 = 0;
    for ch in digits.chars() {
        let digit = ch.to_digit(base).ok_or("parse error")? as i128;
        value = value
            .checked_mul(i128::from(base))
            .and_then(|value| value.checked_add(digit))
            .ok_or("value out of range")?;
        if value > limit {
            return Err("value out of range");
        }
    }
    if negative {
        if value == limit {
            Ok(i64::MIN)
        } else {
            Ok(-(value as i64))
        }
    } else {
        Ok(value as i64)
    }
}

fn separators(digits: &str, base: u32, prefixed: bool) -> Option<String> {
    if !digits.contains('_') {
        return Some(digits.to_owned());
    }
    let chars: Vec<char> = digits.chars().collect();
    let is_digit = |ch: char| ch.is_digit(base);
    let mut out = String::with_capacity(digits.len());
    for (index, ch) in chars.iter().enumerate() {
        if *ch == '_' {
            // Go allows an underscore after the base prefix and between
            // digits, never at the end, doubled, or before any digit.
            let previous_ok = (index == 0 && prefixed) || (index > 0 && is_digit(chars[index - 1]));
            let next_ok = chars.get(index + 1).is_some_and(|ch| is_digit(*ch));
            if !(previous_ok && next_ok) {
                return None;
            }
        } else if is_digit(*ch) {
            out.push(*ch);
        } else {
            return None;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbose_flag_defaults_off_and_enables_stdout_when_set() {
        // Go main.go:109-114.
        assert!(!parse_args(Vec::<String>::new()).unwrap().verbose);
        assert!(parse_args(["--verbose"]).unwrap().verbose);
    }

    #[test]
    fn flag_defaults_match_the_oracle() {
        let args = parse_args(Vec::<String>::new()).unwrap();
        assert_eq!(args.cgroup_root, "/sys/fs/cgroup", "main.go:105-106");
        assert!(!args.no_cgroups, "main.go:112-114");
        assert!(!args.guest);
        assert!(!args.development);
    }

    #[test]
    fn cgroup_root_flag_takes_its_value() {
        let args = parse_args(["--development", "--cgroup-root", "/host/dir"]).unwrap();
        assert_eq!(args.cgroup_root, "/host/dir");
        assert!(args.development);
    }

    #[test]
    fn cgroup_root_without_a_value_is_refused() {
        assert!(parse_args(["--cgroup-root"]).is_err());
    }

    #[test]
    fn unknown_arguments_are_still_refused() {
        assert!(parse_args(["--bogus"]).is_err());
    }

    /// Go main.go:334 binds 0.0.0.0 at the default port 49983 (main.go:42);
    /// the development extension keeps its loopback-ephemeral default.
    #[test]
    fn bind_address_defaults_follow_the_boot_mode() {
        let guest = parse_args(["--guest"]).unwrap();
        assert_eq!(
            guest.resolve_address().unwrap().to_string(),
            "0.0.0.0:49983"
        );
        let development = parse_args(["--development"]).unwrap();
        assert_eq!(
            development.resolve_address().unwrap().to_string(),
            "127.0.0.1:0"
        );
    }

    /// Go main.go:96-101: --port is an int64 that becomes the `0.0.0.0:<port>`
    /// bind address; it is parsed with strconv.ParseInt base 0.
    #[test]
    fn port_flag_builds_the_go_bind_address() {
        for (raw, expected) in [
            ("49983", "0.0.0.0:49983"),
            ("0x10", "0.0.0.0:16"),
            ("0o20", "0.0.0.0:16"),
            ("020", "0.0.0.0:16"),
            ("0b10000", "0.0.0.0:16"),
            ("1_000", "0.0.0.0:1000"),
        ] {
            let args = parse_args(["--guest", "--port", raw]).unwrap();
            assert_eq!(
                args.resolve_address().unwrap().to_string(),
                expected,
                "{raw}"
            );
        }
        assert!(parse_args(["--port", "-1"])
            .unwrap()
            .resolve_address()
            .is_err());
    }

    /// Go flag accepts one or two dashes and `-name=value`; a bool's value is
    /// only ever the inline form, and parsing stops at the first non-flag.
    #[test]
    fn go_flag_forms_and_stop_rules_are_mirrored() {
        let args = parse_args(["-verbose", "-isnotfc=false", "--no-cgroups=0"]).unwrap();
        assert!(args.verbose);
        assert!(!args.is_not_fc);
        assert!(!args.no_cgroups);
        let stopped = parse_args(["positional", "--verbose", "--bogus"]).unwrap();
        assert!(!stopped.verbose);
        // `--` terminates parsing and a bare "-" is positional: the rest is
        // never parsed as flags (probed on the oracle).
        assert!(!parse_args(["--", "--bogus"]).unwrap().version);
        assert!(!parse_args(["-", "--bogus"]).unwrap().version);
        // A value flag consumes the next token even when it looks like a flag
        // (probed on the oracle: `-port -- -version` is a value error).
        assert!(matches!(
            parse_args(["--port", "--", "-version"]),
            Err(CliError::Flag(message))
                if message == "invalid value \"--\" for flag -port: parse error"
        ));
    }

    /// Go flag's parseOne rejects malformed tokens before any lookup with
    /// `bad flag syntax: <arg>` (original token, all dashes), and takes the
    /// ErrHelp path for the names h/help REGARDLESS of an attached value
    /// (probed on the oracle: `-h=1`, `-help=` and friends print the usage and
    /// exit 0).
    #[test]
    fn help_takes_a_value_like_the_oracle_and_bad_tokens_are_named() {
        for flag in [
            "-h=1",
            "--help=1",
            "-help=x",
            "-h=false",
            "-help=",
            "--help=false",
        ] {
            assert!(matches!(parse_args([flag]), Err(CliError::Help)), "{flag}");
        }
        for (arg, message) in [
            ("---x", "bad flag syntax: ---x"),
            ("-=x", "bad flag syntax: -=x"),
            ("--=x", "bad flag syntax: --=x"),
            ("---", "bad flag syntax: ---"),
            ("--=", "bad flag syntax: --="),
        ] {
            assert!(
                matches!(parse_args([arg]), Err(CliError::Flag(m)) if m == message),
                "{arg}"
            );
        }
    }

    #[test]
    fn go_int64_matches_strconv_base_zero() {
        assert_eq!(go_int64("0"), Ok(0));
        assert_eq!(go_int64("+42"), Ok(42));
        assert_eq!(go_int64("-42"), Ok(-42));
        assert_eq!(go_int64("9223372036854775807"), Ok(i64::MAX));
        assert_eq!(go_int64("-9223372036854775808"), Ok(i64::MIN));
        assert_eq!(go_int64("9223372036854775808"), Err("value out of range"));
        assert_eq!(go_int64("-9223372036854775809"), Err("value out of range"));
        assert_eq!(go_int64("abc"), Err("parse error"));
        assert_eq!(go_int64("0x"), Err("parse error"));
        assert_eq!(go_int64("08"), Err("parse error"));
        assert_eq!(go_int64("1__0"), Err("parse error"));
        // Underscores are only legal after a base prefix or between digits.
        assert_eq!(go_int64("_1"), Err("parse error"));
        assert_eq!(go_int64("+_1"), Err("parse error"));
        assert_eq!(go_int64("0x_1"), Ok(1));
        assert_eq!(go_int64("-0x_1"), Ok(-1));
        assert_eq!(go_int64("0_7"), Ok(7));
    }

    #[test]
    fn help_is_the_errhelp_path_and_flag_errors_carry_go_text() {
        assert!(matches!(parse_args(["-h"]), Err(CliError::Help)));
        assert!(matches!(parse_args(["--help"]), Err(CliError::Help)));
        assert!(matches!(
            parse_args(["--bogus"]),
            Err(CliError::Flag(message))
                if message == "flag provided but not defined: -bogus"
        ));
        assert!(matches!(
            parse_args(["--cgroup-root"]),
            Err(CliError::Flag(message))
                if message == "flag needs an argument: -cgroup-root"
        ));
    }
}
