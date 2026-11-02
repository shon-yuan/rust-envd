//! Go `syscall.Errno.Error()` text — the strings the oracle's `%w`-wrapped
//! messages actually carry. Over errno 1..133 Go's amd64 table diverges from
//! nix's `Errno::desc` for 26 values: 23 are translated explicitly here
//! (verified 2026-11-01 against Go 1.26's zerrors_linux_amd64.go `errors`
//! table and a nix 0.29 probe), and the remaining 3 — 41, 58, 133 — are
//! absent from Go's amd64 table entirely, so Go renders them with its
//! fallback "errno N"; the nix probe showed desc gives other text, and no
//! syscall envd surfaces can produce them on amd64 or arm64, so they are not
//! encoded. Over 1..132 the divergence count is
//! therefore 25 minus the unreachable — i.e. exactly the 23 in [`GO_TEXT`] —
//! and every other value falls back to nix's description lowercased, which
//! matches Go verbatim.

/// Go's strings for the errno numbers where nix's desc would differ. Keyed by
/// the raw value; the asm-generic numbering is identical on every arch envd
/// targets (x86_64, arm64).
const GO_TEXT: &[(i32, &str)] = &[
    (5, "input/output error"),
    (9, "bad file descriptor"),
    (11, "resource temporarily unavailable"),
    (12, "cannot allocate memory"),
    (18, "invalid cross-device link"),
    (23, "too many open files in system"),
    (25, "inappropriate ioctl for device"),
    (33, "numerical argument out of domain"),
    (34, "numerical result out of range"),
    (35, "resource deadlock avoided"),
    (37, "no locks available"),
    (40, "too many levels of symbolic links"),
    (50, "no CSI structure available"),
    (73, "RFS specific error"),
    (74, "bad message"),
    (84, "invalid or incomplete multibyte or wide character"),
    (95, "operation not supported"),
    (102, "network dropped connection on reset"),
    (118, "not a XENIX named type file"),
    (119, "no XENIX semaphores available"),
    (121, "remote I/O error"),
    (122, "disk quota exceeded"),
    (132, "operation not possible due to RF-kill"),
];

/// The message text Go's fmt `%w` would render for this io::Error: the table
/// hit when the raw errno is one of the divergent values, otherwise nix's
/// description lowercased (Go-identical for the remaining values). A
/// non-OS error passes through unchanged.
pub(crate) fn go_text(error: &std::io::Error) -> String {
    match error.raw_os_error() {
        Some(raw) => GO_TEXT
            .iter()
            .find(|(number, _)| *number == raw)
            .map_or_else(
                || nix::errno::Errno::from_raw(raw).desc().to_ascii_lowercase(),
                |(_, text)| (*text).to_owned(),
            ),
        None => error.to_string(),
    }
}

/// Go `os.Stat`'s failure text: an *os.PathError with op "stat". Rust's std
/// rejects a path containing NUL before the syscall (InvalidInput, no errno),
/// while Go's syscall returns EINVAL, so that one case is rendered explicitly.
#[allow(dead_code)] // Unused only where the test harness compiles this file directly.
pub(crate) fn stat_text(path: &str, error: &std::io::Error) -> String {
    match error.raw_os_error() {
        Some(_) => format!("stat {path}: {}", go_text(error)),
        None if error.kind() == std::io::ErrorKind::InvalidInput => {
            format!("stat {path}: invalid argument")
        }
        None => format!("stat {path}: {error}"),
    }
}

/// Go `syscall.Signal.String()` descriptions (sigtable), lowercase without the
/// SIG prefix; unknown numbers render as `signal N`. Used wherever Go renders
/// `os.ProcessState.String()` or an `exec.ExitError` ("signal: " + name).
pub(crate) fn signal_description(signal: i32) -> String {
    let name = match signal {
        1 => "hangup",
        2 => "interrupt",
        3 => "quit",
        4 => "illegal instruction",
        5 => "trace/breakpoint trap",
        6 => "aborted",
        7 => "bus error",
        8 => "floating point exception",
        9 => "killed",
        10 => "user defined signal 1",
        11 => "segmentation fault",
        12 => "user defined signal 2",
        13 => "broken pipe",
        14 => "alarm clock",
        15 => "terminated",
        16 => "stack fault",
        17 => "child exited",
        18 => "continued",
        19 => "stopped (signal)",
        20 => "stopped",
        21 => "stopped (tty input)",
        22 => "stopped (tty output)",
        23 => "urgent I/O condition",
        24 => "CPU time limit exceeded",
        25 => "file size limit exceeded",
        26 => "virtual timer expired",
        27 => "profiling timer expired",
        28 => "window changed",
        29 => "I/O possible",
        30 => "power failure",
        31 => "bad system call",
        _ => return format!("signal {signal}"),
    };
    name.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divergent_values_translate_and_the_rest_fall_back() {
        // The ramfs FIFREEZE failure: EOPNOTSUPP.
        assert_eq!(
            go_text(&std::io::Error::from_raw_os_error(95)),
            "operation not supported"
        );
        // Rename across mounts: EXDEV.
        assert_eq!(
            go_text(&std::io::Error::from_raw_os_error(18)),
            "invalid cross-device link"
        );
        // Symlink cycles: ELOOP.
        assert_eq!(
            go_text(&std::io::Error::from_raw_os_error(40)),
            "too many levels of symbolic links"
        );
        // ENOENT (2) agrees between nix and Go, so the fallback must
        // reproduce Go verbatim.
        assert_eq!(
            go_text(&std::io::Error::from_raw_os_error(2)),
            "no such file or directory"
        );
        // A non-OS error keeps its own Display text.
        assert_eq!(go_text(&std::io::Error::other("made up")), "made up");
    }

    /// Go `syscall.Signal.String()` (sigtable, zerrors_linux_amd64). These are
    /// the names `os.ProcessState.String()` and `exec.ExitError.Error()` embed.
    #[test]
    fn signal_descriptions_match_go_sigtable() {
        let table: &[(i32, &str)] = &[
            (1, "hangup"),
            (2, "interrupt"),
            (3, "quit"),
            (4, "illegal instruction"),
            (5, "trace/breakpoint trap"),
            (6, "aborted"),
            (7, "bus error"),
            (8, "floating point exception"),
            (9, "killed"),
            (10, "user defined signal 1"),
            (11, "segmentation fault"),
            (12, "user defined signal 2"),
            (13, "broken pipe"),
            (14, "alarm clock"),
            (15, "terminated"),
            (16, "stack fault"),
            (17, "child exited"),
            (18, "continued"),
            (19, "stopped (signal)"),
            (20, "stopped"),
            (21, "stopped (tty input)"),
            (22, "stopped (tty output)"),
            (23, "urgent I/O condition"),
            (24, "CPU time limit exceeded"),
            (25, "file size limit exceeded"),
            (26, "virtual timer expired"),
            (27, "profiling timer expired"),
            (28, "window changed"),
            (29, "I/O possible"),
            (30, "power failure"),
            (31, "bad system call"),
        ];
        for (signal, expected) in table {
            assert_eq!(signal_description(*signal), *expected, "signal {signal}");
        }
        // Go falls back to "signal N" for numbers outside the table.
        assert_eq!(signal_description(0), "signal 0");
        assert_eq!(signal_description(32), "signal 32");
        assert_eq!(signal_description(64), "signal 64");
    }
}
