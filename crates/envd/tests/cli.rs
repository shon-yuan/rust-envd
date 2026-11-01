use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

#[test]
fn cpu_helper_dispatch_cannot_write_files_from_the_development_host() {
    let directory = tempfile::tempdir().unwrap();
    let cpu = directory.path().join("cpu1");
    std::fs::create_dir(&cpu).unwrap();
    let online = cpu.join("online");
    std::fs::write(&online, "unchanged").unwrap();
    for helper_env in [None, Some("cpu-online")] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_coorworker-envd"));
        command.args(["cpu-online", online.to_str().unwrap(), "1"]);
        command.env_remove("E2B_REEXEC_HELPER");
        if let Some(value) = helper_env {
            command.env("E2B_REEXEC_HELPER", value);
        }
        let result = command.output().unwrap();
        assert!(!result.status.success());
        assert_eq!(std::fs::read_to_string(&online).unwrap(), "unchanged");
    }
}

#[test]
fn development_accepts_explicit_non_firecracker_mode() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_coorworker-envd"))
        .args(["--development", "--isnotfc", "--listen", "127.0.0.1:0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = send.send(line);
    });
    let line = receive.recv_timeout(std::time::Duration::from_secs(5));
    let _ = child.kill();
    let output = child.wait_with_output().unwrap();
    reader.join().unwrap();
    assert!(
        line.unwrap_or_default()
            .starts_with("envd listening 127.0.0.1:"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn development_entrypoint_reports_version_and_refuses_unscoped_or_guest_host_start() {
    let binary = env!("CARGO_BIN_EXE_coorworker-envd");
    let version = Command::new(binary).arg("--version").output().unwrap();
    assert!(version.status.success());
    assert_eq!(version.stdout, b"0.10.2\n");
    for args in [
        vec![],
        vec!["--guest", "--listen", "0.0.0.0:49983"],
        vec!["--development", "--listen", "0.0.0.0:49983"],
    ] {
        let result = Command::new(binary).args(args).output().unwrap();
        assert!(!result.status.success());
    }
}

/// The pinned upstream envd source commit this port implements (main.go:57,
/// injected by the oracle build's `-X main.commitSHA`). `-commit` is part of
/// the official CLI surface (main.go:89-94) and must answer it exactly.
const PINNED_COMMIT: &str = "51083a83cc0903185d992725ea6bb33683814b29";

/// Go main.go:89-94: `-commit`/`--commit` print the source commit and exit 0.
#[test]
fn commit_flag_reports_the_pinned_source_commit_and_exits_zero() {
    let binary = env!("CARGO_BIN_EXE_coorworker-envd");
    for flag in ["-commit", "--commit"] {
        let output = Command::new(binary).arg(flag).output().unwrap();
        assert!(
            output.status.success(),
            "{flag}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            output.stdout,
            format!("{PINNED_COMMIT}\n").as_bytes(),
            "{flag}"
        );
        assert!(output.stderr.is_empty(), "{flag}");
    }
}

/// Go main() checks versionFlag before commitFlag (main.go:162-172), so both
/// orders answer the version even when the other flag is present.
#[test]
fn version_wins_over_commit_like_the_oracle() {
    let binary = env!("CARGO_BIN_EXE_coorworker-envd");
    for args in [vec!["--version", "--commit"], vec!["--commit", "--version"]] {
        let output = Command::new(binary).args(&args).output().unwrap();
        assert!(output.status.success(), "{args:?}");
        assert_eq!(output.stdout, b"0.10.2\n", "{args:?}");
    }
}

/// Go flag's ErrHelp path: usage goes to stderr and the process exits 0. The
/// flag list is the oracle's own, byte-for-byte in flag-name order.
#[test]
fn help_prints_the_go_usage_on_stderr_and_exits_zero() {
    let binary = env!("CARGO_BIN_EXE_coorworker-envd");
    for flag in ["-h", "--help", "-help"] {
        let output = Command::new(binary).arg(flag).output().unwrap();
        assert_eq!(output.status.code(), Some(0), "{flag}");
        assert!(output.stdout.is_empty(), "{flag}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.starts_with("Usage of "), "{flag}: {stderr}");
        for line in [
            "  -cgroup-root string\n    \tcgroup root directory (default \"/sys/fs/cgroup\")\n",
            "  -commit\n    \tprint envd source commit\n",
            "  -isnotfc\n    \trun outside of Firecracker (skips MMDS poll and HTTP log exporter)\n",
            "  -no-cgroups\n    \tdisable cgroup management; use a no-op cgroup manager instead\n",
            "  -port int\n    \ta port on which the daemon should run (default 49983)\n",
            "  -resume-handover\n    \tinternal: re-adopt processes from the live-upgrade handover blob (set by the outgoing envd)\n",
            "  -verbose\n    \twrite envd logs to stdout\n",
            "  -version\n    \tprint envd version\n",
        ] {
            assert!(stderr.contains(line), "{flag}: missing {line:?} in {stderr:?}");
        }
    }
}

/// `-h` exits during flag parsing, so a later bad flag is never seen; the
/// reverse order reports the bad flag and exits 2 (probed on the oracle).
#[test]
fn help_short_circuits_flags_that_follow_it() {
    let binary = env!("CARGO_BIN_EXE_coorworker-envd");
    let output = Command::new(binary)
        .args(["-h", "--bogus"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let output = Command::new(binary)
        .args(["--bogus", "-h"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

/// Go flag's parse-error path: one diagnostic line to stderr, the usage, exit 2.
#[test]
fn unknown_flags_report_go_style_errors_and_exit_two() {
    let binary = env!("CARGO_BIN_EXE_coorworker-envd");
    let output = Command::new(binary).arg("--bogus").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("flag provided but not defined: -bogus\nUsage of "),
        "{stderr:?}"
    );
}

/// Go flag's value errors (probed on the oracle): missing value and the
/// base-0 ParseInt diagnostics differ, and both exit 2.
#[test]
fn flag_value_errors_match_the_oracle_text() {
    let binary = env!("CARGO_BIN_EXE_coorworker-envd");
    for (args, expected) in [
        (
            vec!["--cgroup-root"],
            "flag needs an argument: -cgroup-root\nUsage of ",
        ),
        (
            vec!["--port=abc"],
            "invalid value \"abc\" for flag -port: parse error\nUsage of ",
        ),
        (
            vec!["--port=99999999999999999999"],
            "invalid value \"99999999999999999999\" for flag -port: value out of range\nUsage of ",
        ),
        (
            vec!["--verbose=abc"],
            "invalid boolean value \"abc\" for -verbose: parse error\nUsage of ",
        ),
    ] {
        let output = Command::new(binary).args(&args).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.starts_with(expected),
            "{args:?}: expected {expected:?}, got {stderr:?}"
        );
    }
}

/// Go flag accepts one or two dashes and `-name=value`; int64 is parsed with
/// base 0, so port literals may be hexadecimal (probed on the oracle).
#[test]
fn single_dash_aliases_and_equals_forms_are_accepted() {
    let binary = env!("CARGO_BIN_EXE_coorworker-envd");
    let output = Command::new(binary)
        .args(["-version", "--port=0x10", "-isnotfc=false", "-verbose=true"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"0.10.2\n");
}

/// Go flag stops at the first non-flag argument, so a flag after it is never
/// parsed (probed on the oracle: `foo --version` starts the daemon instead).
/// The Rust port must not treat it as an unknown flag.
#[test]
fn parsing_stops_at_the_first_non_flag_argument() {
    let binary = env!("CARGO_BIN_EXE_coorworker-envd");
    let output = Command::new(binary)
        .args(["positional", "--bogus"])
        .output()
        .unwrap();
    // Reaching serve (and failing the development guard) proves the trailing
    // flag was not parsed as an unknown argument.
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("flag provided but not defined"),
        "{stderr:?}"
    );
    assert!(
        stderr.contains("development mode requires"),
        "the parser must stop at the positional instead of refusing it: {stderr:?}"
    );
}

/// reexec.Main (reexec.go:49-68): a process marked as a helper that names no
/// registered helper must refuse to fall through and run the daemon again.
#[test]
fn a_marked_reexec_process_without_a_helper_refuses_to_fall_through() {
    let binary = env!("CARGO_BIN_EXE_coorworker-envd");
    let output = Command::new(binary)
        .env("E2B_REEXEC_HELPER", "cpu-online")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("started as helper") && stderr.contains("no such helper is defined"),
        "{stderr:?}"
    );
}

/// Go flag's parseOne special-cases the name h/help in the unknown-flag branch,
/// BEFORE any value handling (flag/flag.go), so every value form takes the
/// ErrHelp path (probed on the identity oracle: usage on stderr, exit 0).
#[test]
fn help_with_a_value_takes_the_errhelp_path() {
    let binary = env!("CARGO_BIN_EXE_coorworker-envd");
    for flag in [
        "-h=1",
        "--help=1",
        "-help=x",
        "-h=false",
        "-help=",
        "--help=false",
    ] {
        let output = Command::new(binary).arg(flag).output().unwrap();
        assert_eq!(output.status.code(), Some(0), "{flag}");
        assert!(output.stdout.is_empty(), "{flag}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.starts_with("Usage of "), "{flag}: {stderr:?}");
        assert!(
            stderr.contains("  -version\n    \tprint envd version\n"),
            "{flag}: {stderr:?}"
        );
    }
}

/// Go rejects malformed flag tokens before any flag lookup with
/// `bad flag syntax: <arg>`, quoting the original argument including all
/// dashes (probed on the identity oracle; exit 2, usage follows).
#[test]
fn malformed_flag_tokens_report_bad_flag_syntax() {
    let binary = env!("CARGO_BIN_EXE_coorworker-envd");
    for arg in ["---x", "-=x", "--=x", "---", "--="] {
        let output = Command::new(binary).arg(arg).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{arg}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.starts_with(&format!("bad flag syntax: {arg}\nUsage of ")),
            "{arg}: {stderr:?}"
        );
    }
}

/// `--` terminates flag parsing and everything after it is ignored (probed on
/// the identity oracle: `-version -- -port=abc` answers the version, exit 0;
/// a value flag BEFORE `--` still consumes it, `-port -- -version` is
/// `invalid value "--"`).
#[test]
fn double_dash_terminates_flag_parsing() {
    let binary = env!("CARGO_BIN_EXE_coorworker-envd");
    let output = Command::new(binary)
        .args(["-version", "--", "-port=abc"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, b"0.10.2\n");
    let output = Command::new(binary)
        .args(["--port", "--", "-version"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("invalid value \"--\" for flag -port: parse error"),
        "{stderr:?}"
    );
    // On this port the terminator hands the rest to the development guard, so
    // reaching serve instead of printing the version proves the trailing
    // `-version` was never parsed.
    let output = Command::new(binary)
        .args(["--", "-version"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("development mode requires"), "{stderr:?}");
}

/// A bare `-` is a positional argument, not a flag: parsing stops there
/// (probed on the identity oracle: `-version -` answers the version, exit 0).
#[test]
fn a_bare_dash_stops_parsing_as_a_positional() {
    let binary = env!("CARGO_BIN_EXE_coorworker-envd");
    let output = Command::new(binary)
        .args(["-version", "-"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, b"0.10.2\n");
    let output = Command::new(binary).args(["-", "-bogus"]).output().unwrap();
    // Reaching serve (the development guard) proves the trailing `-bogus` was
    // not parsed as an unknown flag.
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("flag provided but not defined") && !stderr.contains("bad flag syntax"),
        "{stderr:?}"
    );
}

/// Go main.go:124-129: --resume-handover is an internal flag the outgoing
/// envd appends when it re-execs itself (main.go:287-288); the parser must
/// accept it alongside the development flags instead of refusing it as an
/// unknown argument.
#[test]
fn the_resume_handover_flag_is_accepted_by_the_entrypoint() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_coorworker-envd"))
        .args([
            "--development",
            "--isnotfc",
            "--resume-handover",
            "--listen",
            "127.0.0.1:0",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = send.send(line);
    });
    let line = receive.recv_timeout(std::time::Duration::from_secs(5));
    let _ = child.kill();
    let output = child.wait_with_output().unwrap();
    reader.join().unwrap();
    // The resumed daemon must reach serving (no handover blob exists on this
    // host, so ResumeFromHandover is a no-op: upgrade.go:341-344).
    assert!(
        line.unwrap_or_default()
            .starts_with("envd listening 127.0.0.1:"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
