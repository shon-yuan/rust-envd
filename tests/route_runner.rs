//! ProcessRunner parity with the oracle's iptables runner.
//!
//! The oracle runs `exec.CommandContext(ctx, "iptables", ...).CombinedOutput`
//! (internal/host/mmds_route_linux.go:46-55 at
//! 51083a83cc0903185d992725ea6bb33683814b29), so the observable surface of a
//! failed pin is Go's own error text: `exec: "iptables": executable file not
//! found in $PATH` on a lookup miss, `fork/exec <path>: <errno>` when execve
//! fails after lookup, `exit status N` / `signal: <name>` from
//! os.ProcessState. Those strings reach the pin-failure warn line (init.go
//! 88-93), so they are production-observable, not log-internal.
//!
//! Every test here runs harmless stand-in programs (shell scripts, a copy of
//! sleep) — the host iptables is never invoked. All tests that mutate PATH
//! share one function so they cannot race each other.

use coorworker_envd::mmds::{CommandRunner, ProcessRunner};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Serializes every fork/exec in this binary.
///
/// Linux copies the parent's descriptor table into a fork child and closes
/// O_CLOEXEC descriptors only at that child's own execve. A thread that writes
/// a fixture and then execs it therefore races any other thread's in-flight
/// spawn: the other child can still hold the writer's descriptor, and the
/// writer's execve fails with ETXTBSY ("text file busy") instead of reaching
/// the loader. The race reproduces with `std::fs::write` + `std::process`
/// alone across four threads and needs no envd code (kernel/fs behavior; Go's
/// forkAndExecInChild inherits O_CLOEXEC descriptors the same way). Holding
/// this gate across each run makes fork events exclusive: a run's child has
/// always completed its own execve before the gate is released, so no
/// inherited write descriptor can survive into the next exec.
static SPAWN_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn run_gated(
    runner: &ProcessRunner,
    args: Vec<String>,
) -> std::io::Result<coorworker_envd::mmds::CommandOutput> {
    let _gate = SPAWN_GATE.lock().await;
    runner.run(args).await
}

fn tempdir() -> PathBuf {
    tempfile::tempdir().unwrap().keep()
}

fn write_script(dir: &Path, name: &str, body: &str, mode: u32) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, body).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    path
}

/// Restores the previous PATH when the section finishes, and drops the
/// fixture directory even when an assertion fails, so the package tree never
/// keeps test residue.
struct PathFixture {
    previous: Option<std::ffi::OsString>,
    directory: Option<PathBuf>,
}

impl PathFixture {
    fn with_directory() -> Self {
        Self {
            previous: std::env::var_os("PATH"),
            directory: Some(tempdir()),
        }
    }

    fn with_cwd_directory(name: &str) -> Self {
        let directory = std::env::current_dir().unwrap().join(name);
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        Self {
            previous: std::env::var_os("PATH"),
            directory: Some(directory),
        }
    }

    fn path(&self) -> PathBuf {
        self.directory.clone().unwrap()
    }

    fn set_path(&self, value: &str) {
        std::env::set_var("PATH", value);
    }

    fn set_path_to_directory(&self) {
        let value = self.path().display().to_string();
        self.set_path(&value);
    }
}

impl Drop for PathFixture {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(previous) => std::env::set_var("PATH", previous),
            None => std::env::remove_var("PATH"),
        }
        if let Some(directory) = self.directory.take() {
            let _ = fs::remove_dir_all(&directory);
        }
    }
}

#[tokio::test]
async fn path_lookup_semantics_match_go_look_path() {
    // Section 1: a PATH without the program reports Go's exec-not-found text
    // (os/exec LookPath ErrNotFound, reached from exec.Command at
    // mmds_route_linux.go:50).
    {
        let fixture = PathFixture::with_directory();
        fixture.set_path_to_directory();
        let outcome = run_gated(
            &ProcessRunner::with_program("no-such-iptables-xyz"),
            vec!["-w".into(), "5".into()],
        )
        .await;
        assert_eq!(
            outcome.unwrap_err().to_string(),
            "exec: \"no-such-iptables-xyz\": executable file not found in $PATH"
        );
    }

    // Section 2: an empty or unset PATH searches nothing (Go's SplitList
    // collapses "" to no entries).
    for state in ["empty", "unset"] {
        let fixture = PathFixture::with_directory();
        match state {
            "empty" => fixture.set_path(""),
            "unset" => std::env::remove_var("PATH"),
            _ => unreachable!(),
        }
        let outcome = run_gated(
            &ProcessRunner::with_program("no-such-iptables-xyz"),
            vec!["-t".into(), "nat".into()],
        )
        .await;
        assert_eq!(
            outcome.unwrap_err().to_string(),
            "exec: \"no-such-iptables-xyz\": executable file not found in $PATH",
            "PATH state: {state}"
        );
    }

    // Section 3: the walk skips directories and non-executable files and
    // picks the first executable entry (Go findExecutable + continue).
    {
        let fixture = PathFixture::with_directory();
        let plain_directory = tempdir();
        let pick_directory = tempdir();
        // First entry: the name is a directory — refused, keep walking.
        fs::create_dir(fixture.path().join("pick-pgm2")).unwrap();
        // Second entry: the name is a non-executable file — refused, keep
        // walking.
        write_script(&plain_directory, "pick-pgm2", "data\n", 0o644);
        // Third entry: executable — picked.
        write_script(
            &pick_directory,
            "pick-pgm2",
            "#!/bin/sh\necho from-later-entry\n",
            0o755,
        );
        fixture.set_path(&format!(
            "{}:{}:{}",
            fixture.path().display(),
            plain_directory.display(),
            pick_directory.display()
        ));
        let outcome = run_gated(&ProcessRunner::with_program("pick-pgm2"), Vec::new()).await;
        let output = outcome.unwrap();
        assert!(output.success, "{output:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.output),
            "from-later-entry\n"
        );
    }

    // Section 3b: an empty PATH entry means the current directory (Go's unix
    // shell semantics); the name is searched there too. The unique name only
    // exists in the later entry, so the empty entry must be walked without
    // disturbing the result.
    {
        let fixture = PathFixture::with_directory();
        let pick_directory = tempdir();
        write_script(
            &pick_directory,
            "empty-entry-pgm-xyz",
            "#!/bin/sh\necho after-empty-entry\n",
            0o755,
        );
        fixture.set_path(&format!(":{}", pick_directory.display()));
        let outcome = run_gated(
            &ProcessRunner::with_program("empty-entry-pgm-xyz"),
            Vec::new(),
        )
        .await;
        let output = outcome.unwrap();
        assert!(output.success, "{output:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.output),
            "after-empty-entry\n"
        );
    }

    // Section 4: a hit below a relative PATH entry is refused with Go's
    // ErrDot text; nothing runs.
    {
        let fixture = PathFixture::with_cwd_directory("route-runner-reldir-fixture");
        write_script(&fixture.path(), "rel-pgm", "#!/bin/sh\nexit 0\n", 0o755);
        fixture.set_path("route-runner-reldir-fixture");
        let outcome = run_gated(&ProcessRunner::with_program("rel-pgm"), Vec::new()).await;
        assert_eq!(
            outcome.unwrap_err().to_string(),
            "exec: \"rel-pgm\": cannot run executable found relative to current directory"
        );
    }

    // Section 5: dotted PATH entries are cleaned like Go's filepath.Join, so
    // the fork/exec error text shows the cleaned absolute path.
    {
        let fixture = PathFixture::with_directory();
        let program = write_script(&fixture.path(), "dirty-pgm", "garbage\n", 0o755);
        fixture.set_path(&format!("{}/./", fixture.path().display()));
        let outcome = run_gated(&ProcessRunner::with_program("dirty-pgm"), Vec::new()).await;
        assert_eq!(
            outcome.unwrap_err().to_string(),
            format!("fork/exec {}: exec format error", program.display())
        );
    }
}

#[tokio::test]
async fn slash_program_missing_reports_stat_path_error_text() {
    let outcome = run_gated(
        &ProcessRunner::with_program("/route-runner-no-such-dir-xyz/pgm"),
        Vec::new(),
    )
    .await;
    assert_eq!(
        outcome.unwrap_err().to_string(),
        "exec: \"/route-runner-no-such-dir-xyz/pgm\": \
         stat /route-runner-no-such-dir-xyz/pgm: no such file or directory"
    );
}

#[tokio::test]
async fn slash_program_directory_reports_is_a_directory() {
    let dir = tempdir();
    let outcome = run_gated(
        &ProcessRunner::with_program(dir.display().to_string()),
        Vec::new(),
    )
    .await;
    assert_eq!(
        outcome.unwrap_err().to_string(),
        format!("exec: \"{}\": is a directory", dir.display())
    );
}

#[tokio::test]
async fn slash_program_non_executable_reports_permission_denied() {
    let dir = tempdir();
    let file = write_script(&dir, "plain-pgm", "data\n", 0o644);
    let outcome = run_gated(
        &ProcessRunner::with_program(file.display().to_string()),
        Vec::new(),
    )
    .await;
    assert_eq!(
        outcome.unwrap_err().to_string(),
        format!("exec: \"{}\": permission denied", file.display())
    );
}

#[tokio::test]
async fn executable_but_invalid_format_reports_fork_exec_exec_format_error() {
    let dir = tempdir();
    let file = write_script(&dir, "garbage-pgm", "not a valid binary\n", 0o755);
    let outcome = run_gated(
        &ProcessRunner::with_program(file.display().to_string()),
        Vec::new(),
    )
    .await;
    assert_eq!(
        outcome.unwrap_err().to_string(),
        format!("fork/exec {}: exec format error", file.display())
    );
}

#[tokio::test]
async fn signal_death_reports_go_signal_name() {
    for (self_kill, expected) in [
        ("kill -9 $$", "signal: killed"),
        ("kill -15 $$", "signal: terminated"),
    ] {
        let outcome = run_gated(
            &ProcessRunner::with_program("/bin/sh"),
            vec!["-c".into(), self_kill.into()],
        )
        .await;
        let output = outcome.unwrap();
        assert!(!output.success, "{output:?}");
        assert_eq!(output.error, expected, "script: {self_kill}");
    }
}

#[tokio::test]
async fn exit_status_and_combined_output_preserve_write_order() {
    let outcome = run_gated(
        &ProcessRunner::with_program("/bin/sh"),
        vec!["-c".into(), "echo out; echo err >&2; exit 7".into()],
    )
    .await;
    let output = outcome.unwrap();
    assert!(!output.success);
    // Go's CombinedOutput interleaves on one shared pipe, and its ExitError
    // renders as `exit status N` (mmds_route_linux.go:50-54).
    assert_eq!(String::from_utf8_lossy(&output.output), "out\nerr\n");
    assert_eq!(output.error, "exit status 7");
}

#[tokio::test]
async fn success_reports_success() {
    let outcome = run_gated(
        &ProcessRunner::with_program("/bin/sh"),
        vec!["-c".into(), "printf ok".into()],
    )
    .await;
    let output = outcome.unwrap();
    assert!(output.success);
    assert_eq!(String::from_utf8_lossy(&output.output), "ok");
}

#[tokio::test]
async fn dropping_the_run_future_kills_the_child() {
    // Go binds the pin to the /init request context: cancellation kills the
    // iptables child (exec.CommandContext, mmds_route_linux.go:50). The
    // runner's future is dropped when the caller goes away; kill-on-drop must
    // leave no orphan behind.
    let dir = tempdir();
    let probe = dir.join("route-runner-kill-probe");
    fs::copy("/usr/bin/sleep", &probe).unwrap();
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o755)).unwrap();

    fn probe_processes(probe: &Path) -> usize {
        let mut found = 0;
        let entries = match fs::read_dir("/proc") {
            Ok(entries) => entries,
            Err(_) => return 0,
        };
        for entry in entries.flatten() {
            let command = match fs::read(entry.path().join("cmdline")) {
                Ok(command) => command,
                Err(_) => continue,
            };
            // argv[0] is the first NUL-separated token.
            let argv0 = command.split(|byte| *byte == 0).next().unwrap_or(&[]);
            if argv0 == probe.as_os_str().as_encoded_bytes() {
                found += 1;
            }
        }
        found
    }

    let runner = ProcessRunner::with_program(probe.display().to_string());
    // Take the gate here rather than inside the task: another test may hold it
    // for seconds, and the appearance deadline must start only once this spawn
    // can actually run (CI exposed the difference on a loaded runner).
    let gate = SPAWN_GATE.lock().await;
    let task = tokio::spawn(async move {
        let _gate = gate;
        runner.run(vec!["30".into()]).await
    });
    let started = Instant::now();
    while probe_processes(&probe) == 0 {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "probe child never appeared"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    task.abort();
    let deadline = Instant::now();
    while probe_processes(&probe) > 0 {
        assert!(
            deadline.elapsed() < Duration::from_secs(10),
            "dropping the run future left the child alive"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
