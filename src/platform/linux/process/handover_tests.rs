use super::{dup_keep, pipe, validate_carried_fds, Entry, ProcessRuntime, HANDOVER_FD_BASE};
use crate::exec_context::{Account, Defaults, Environment};
use crate::proto::upgrade::HandoverProc;
use nix::fcntl::{fcntl, FcntlArg::F_GETFD, FdFlag};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::AtomicI64;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

fn identity(fd: RawFd) -> (libc::dev_t, libc::ino_t) {
    let stat = nix::sys::stat::fstat(fd).expect("test descriptor must remain open");
    (stat.st_dev, stat.st_ino)
}

#[test]
fn dup_keep_rejects_occupied_target_without_changing_either_endpoint() {
    let (source, _source_peer) = UnixStream::pair().unwrap();
    let (target, _target_peer) = UnixStream::pair().unwrap();
    let source_before = identity(source.as_raw_fd());
    let target_before = identity(target.as_raw_fd());
    let target_flags = fcntl(target.as_raw_fd(), F_GETFD).unwrap();
    assert_ne!(source_before, target_before);

    let result = dup_keep(source.as_raw_fd(), target.as_raw_fd());

    assert!(result.is_err(), "an occupied target must be refused");
    assert_eq!(identity(target.as_raw_fd()), target_before);
    assert_eq!(identity(source.as_raw_fd()), source_before);
    assert_eq!(fcntl(target.as_raw_fd(), F_GETFD).unwrap(), target_flags);
}

#[test]
fn dup_keep_rejects_source_as_target_without_changing_it() {
    let (source, _peer) = UnixStream::pair().unwrap();
    let before = identity(source.as_raw_fd());
    let flags = fcntl(source.as_raw_fd(), F_GETFD).unwrap();

    assert!(dup_keep(source.as_raw_fd(), source.as_raw_fd()).is_err());

    assert_eq!(identity(source.as_raw_fd()), before);
    assert_eq!(fcntl(source.as_raw_fd(), F_GETFD).unwrap(), flags);
}

#[test]
fn dup_keep_absent_source_leaves_occupied_target_unchanged() {
    let (target, _peer) = UnixStream::pair().unwrap();
    let before = identity(target.as_raw_fd());
    let flags = fcntl(target.as_raw_fd(), F_GETFD).unwrap();

    for absent in [-1, i32::MIN] {
        assert_eq!(dup_keep(absent, target.as_raw_fd()), Ok(-1));
        assert_eq!(identity(target.as_raw_fd()), before);
        assert_eq!(fcntl(target.as_raw_fd(), F_GETFD).unwrap(), flags);
    }
}

#[test]
fn dup_keep_allocates_exact_vacant_target_with_cloexec_cleared() {
    const CHILD: &str = "ENVD_DUP_KEEP_VACANT_TARGET_CHILD";
    if std::env::var_os(CHILD).is_none() {
        // Derive the harness name so this also works when process/mod.rs is
        // compiled directly by tests/process_runtime.rs.
        let qualified = concat!(
            module_path!(),
            "::dup_keep_allocates_exact_vacant_target_with_cloexec_cleared"
        );
        let (_, test_name) = qualified.split_once("::").unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .output()
            .expect("start isolated fd-table test");
        assert!(
            output.status.success(),
            "isolated fd-table test failed: {}\n{}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    // Only this test runs in the child; no asynchronous runtime or sibling
    // test can claim the reserved slot between close and dup_keep.
    let source = std::fs::File::open("/dev/null").unwrap();
    let reserved = std::fs::File::open("/dev/zero").unwrap();
    let target = reserved.as_raw_fd();
    assert_ne!(
        fcntl(source.as_raw_fd(), F_GETFD).unwrap() & FdFlag::FD_CLOEXEC.bits(),
        0
    );
    drop(reserved);

    let allocated = dup_keep(source.as_raw_fd(), target).expect("vacant target must be allocated");

    assert_eq!(allocated, target);
    // SAFETY: dup_keep just allocated this previously vacant descriptor;
    // this child test is its only owner.
    let allocated = unsafe { OwnedFd::from_raw_fd(allocated) };
    assert_eq!(
        fcntl(allocated.as_raw_fd(), F_GETFD).unwrap() & FdFlag::FD_CLOEXEC.bits(),
        0
    );
    assert_eq!(
        identity(allocated.as_raw_fd()),
        identity(source.as_raw_fd())
    );
}

fn carried(pid: u32, endpoints: [RawFd; 4]) -> HandoverProc {
    HandoverProc {
        pid,
        cg_type: "user".into(),
        stdout_fd: endpoints[0],
        stderr_fd: endpoints[1],
        stdin_fd: endpoints[2],
        tty_fd: endpoints[3],
        ..Default::default()
    }
}

fn assert_test_fds_open(fds: &[OwnedFd]) {
    for fd in fds {
        assert!(
            fcntl(fd.as_raw_fd(), F_GETFD).is_ok(),
            "validation consumed test descriptor {}",
            fd.as_raw_fd()
        );
    }
}

#[test]
fn validate_carried_fds_rejects_every_within_process_endpoint_alias() {
    let fds: [OwnedFd; 4] =
        std::array::from_fn(|_| std::fs::File::open("/dev/null").unwrap().into());
    let unique = fds.each_ref().map(AsRawFd::as_raw_fd);
    // All six pairs, including endpoints unused for a particular cg_type,
    // must be checked before any carried descriptor can be consumed.
    for first in 0..4 {
        for second in (first + 1)..4 {
            let mut endpoints = unique;
            endpoints[second] = endpoints[first];

            let result: Result<(), String> = validate_carried_fds(&[carried(101, endpoints)]);

            assert!(
                result.is_err(),
                "accepted aliased endpoint positions {first} and {second}"
            );
            assert_test_fds_open(&fds);
        }
    }
}

#[test]
fn validate_carried_fds_rejects_every_across_process_endpoint_alias() {
    let fds: [OwnedFd; 8] =
        std::array::from_fn(|_| std::fs::File::open("/dev/null").unwrap().into());
    let first = std::array::from_fn(|index| fds[index].as_raw_fd());
    let unique_second: [RawFd; 4] = std::array::from_fn(|index| fds[index + 4].as_raw_fd());
    for first_endpoint in 0..4 {
        for second_endpoint in 0..4 {
            let mut second = unique_second;
            second[second_endpoint] = first[first_endpoint];
            let processes = [carried(101, first), carried(202, second)];

            let result: Result<(), String> = validate_carried_fds(&processes);

            assert!(
                result.is_err(),
                "accepted cross-process alias at positions {first_endpoint} and {second_endpoint}"
            );
            assert_test_fds_open(&fds);
        }
    }
}

#[test]
fn validate_carried_fds_accepts_empty_and_repeated_negative_absent_endpoints() {
    assert_eq!(validate_carried_fds(&[]), Ok(()));
    let processes = [
        carried(101, [-1, -1, i32::MIN, -2]),
        carried(202, [-1, i32::MIN, -2, -1]),
    ];

    assert_eq!(validate_carried_fds(&processes), Ok(()));
}

#[test]
fn validate_carried_fds_accepts_unique_live_descriptors_and_absent_endpoints() {
    let fds: [OwnedFd; 8] =
        std::array::from_fn(|_| std::fs::File::open("/dev/null").unwrap().into());
    let processes = [
        carried(101, std::array::from_fn(|index| fds[index].as_raw_fd())),
        carried(202, std::array::from_fn(|index| fds[index + 4].as_raw_fd())),
        carried(303, [-1, i32::MIN, -1, -2]),
    ];

    assert_eq!(validate_carried_fds(&processes), Ok(()));

    assert_test_fds_open(&fds);
}

#[test]
fn validate_carried_fds_rejects_closed_batch_tail_without_consuming_valid_prefix() {
    let fds: [OwnedFd; 4] =
        std::array::from_fn(|_| std::fs::File::open("/dev/null").unwrap().into());
    let processes = [
        carried(101, fds.each_ref().map(AsRawFd::as_raw_fd)),
        carried(202, [-1, -1, -1, i32::MAX]),
    ];

    let result = validate_carried_fds(&processes);

    assert!(
        result.is_err(),
        "accepted a closed descriptor at the batch tail"
    );
    assert_test_fds_open(&fds);
}

// Return true only inside this test's own process. Fixed slots and Tokio's
// process-global signal descriptors must never share a table with other tests.
fn in_isolated_process(name: &str) -> bool {
    const CHILD: &str = "ENVD_HANDOVER_ISOLATED_TEST";
    if std::env::var(CHILD).as_deref() == Ok(name) {
        return true;
    }
    let qualified = format!("{}::{name}", module_path!());
    let (_, test_name) = qualified.split_once("::").unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env(CHILD, name)
        .output()
        .expect("start isolated handover test");
    assert!(
        output.status.success(),
        "isolated handover test failed: {}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    false
}

fn private_fd_runtime() -> (tokio::runtime::Runtime, [OwnedFd; 4]) {
    for fd in HANDOVER_FD_BASE..HANDOVER_FD_BASE + 4 {
        // This single-test child has not initialized Tokio or created any
        // owners in the handover window. Closing inherited copies is local.
        let _ = nix::unistd::close(fd);
    }
    let reserved = std::array::from_fn(|index| {
        let target = HANDOVER_FD_BASE + index as RawFd;
        let fresh = fcntl(
            libc::STDERR_FILENO,
            nix::fcntl::FcntlArg::F_DUPFD_CLOEXEC(target),
        )
        .expect("reserve private handover slot");
        assert_eq!(fresh, target);
        // SAFETY: fcntl allocated this descriptor in this thread's private table.
        unsafe { OwnedFd::from_raw_fd(fresh) }
    });
    // Reserve the window until the fixture's sources and reactor are ready,
    // so none of their owners can accidentally occupy the slots under test.
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .unwrap();
    (executor, reserved)
}

fn export_entry() -> (Arc<Entry>, [OwnedFd; 3]) {
    let flags = nix::fcntl::OFlag::O_NONBLOCK | nix::fcntl::OFlag::O_CLOEXEC;
    let (stdout, stdout_peer) = nix::unistd::pipe2(flags).unwrap();
    let (stderr, stderr_peer) = nix::unistd::pipe2(flags).unwrap();
    let (stdin_peer, stdin) = nix::unistd::pipe2(flags).unwrap();
    // SAFETY: File exclusively owns this fresh pipe, and the entry's stdin
    // mutex serializes removal with all registered I/O.
    let stdin = unsafe { tokio::io::unix::AsyncFd::register(std::fs::File::from(stdin)) }.unwrap();
    let (control, _controls) = mpsc::channel(1);
    let entry = Arc::new(Entry {
        info: crate::proto::process::ProcessInfo {
            pid: 101,
            ..Default::default()
        },
        stdin: Mutex::new(Some(pipe::StdinPipe::Adopted(stdin))),
        pty: Mutex::new(None),
        is_pty: false,
        subscribers: Mutex::new(Default::default()),
        control,
        stdout_bytes: Arc::new(AtomicI64::new(0)),
        stderr_bytes: Arc::new(AtomicI64::new(0)),
        pty_bytes: Arc::new(AtomicI64::new(0)),
        operation_id: None,
        stdout_dup: Some(Arc::new(stdout)),
        stderr_dup: Some(Arc::new(stderr)),
        deadline_at: Mutex::new(None),
    });
    (entry, [stdout_peer, stderr_peer, stdin_peer])
}

fn fixture_runtime() -> ProcessRuntime {
    // No spawn or account lookup is needed for registry-only fixtures.
    ProcessRuntime::new(
        Account {
            name: "handover-fixture".into(),
            home: "/tmp".into(),
            uid: nix::unistd::geteuid().as_raw(),
            gid: nix::unistd::getegid().as_raw(),
            groups: Vec::new(),
        },
        Defaults::default(),
        Environment::default(),
        None,
    )
    .unwrap()
}

fn export_runtime(entry: Arc<Entry>) -> ProcessRuntime {
    let runtime = fixture_runtime();
    runtime
        .registry
        .try_lock()
        .unwrap()
        .live
        .insert(entry.info.pid, entry);
    runtime
}

#[test]
fn export_cancellation_while_waiting_for_deadline_closes_allocated_slots() {
    if !in_isolated_process("export_cancellation_while_waiting_for_deadline_closes_allocated_slots")
    {
        return;
    }
    let (executor, reserved) = private_fd_runtime();
    executor.block_on(async {
        let (entry, _peers) = export_entry();
        let runtime = export_runtime(entry.clone());
        let stdin_fd = entry.stdin.lock().await.as_ref().unwrap().raw_fd();
        let sources = [
            entry.stdout_dup.as_ref().unwrap().as_ref().as_raw_fd(),
            entry.stderr_dup.as_ref().unwrap().as_ref().as_raw_fd(),
            stdin_fd,
        ];
        let identities = sources.map(identity);
        let slots = std::array::from_fn::<_, 3, _>(|index| HANDOVER_FD_BASE + index as RawFd);
        let _deadline = entry.deadline_at.lock().await;
        drop(reserved);
        let mut export = Box::pin(runtime.export_processes());

        assert!(futures_util::poll!(export.as_mut()).is_pending());
        for (slot, expected) in slots.iter().zip(identities) {
            assert_eq!(
                identity(*slot),
                expected,
                "export must reach the deadline wait"
            );
            assert_eq!(
                fcntl(*slot, F_GETFD).unwrap() & FdFlag::FD_CLOEXEC.bits(),
                0
            );
        }
        // Drop the owned future, not just a Pin<&mut _>, to cancel it here.
        drop(export);

        let remaining = slots.map(|slot| fcntl(slot, F_GETFD));
        // Clean up observed leaks even on RED, after capturing the evidence.
        for (slot, state) in slots.into_iter().zip(remaining) {
            if state.is_ok() {
                nix::unistd::close(slot).unwrap();
            }
        }
        assert_eq!(
            remaining,
            [Err(nix::errno::Errno::EBADF); 3],
            "cancelled export leaked already allocated handover slots"
        );
        assert_eq!(sources.map(identity), identities);
        assert!(runtime.registry.try_lock().is_ok());
    });
}

#[test]
fn export_keeps_stdin_owner_while_waiting_for_pty_snapshot() {
    if !in_isolated_process("export_keeps_stdin_owner_while_waiting_for_pty_snapshot") {
        return;
    }
    let (executor, reserved) = private_fd_runtime();
    executor.block_on(async {
        let (entry, _peers) = export_entry();
        let runtime = export_runtime(entry.clone());
        let source = entry.stdin.lock().await.as_ref().unwrap().raw_fd();
        let before = identity(source);
        let pty = entry.pty.lock().await;
        drop(reserved);
        let mut export = Box::pin(runtime.export_processes());

        // The private registry is unlocked and stdin is available, so this
        // first poll reads stdin and stops exactly at the held pty mutex.
        assert!(futures_util::poll!(export.as_mut()).is_pending());
        let removed = entry.stdin.try_lock().ok().and_then(|mut stdin| stdin.take());
        let was_removable = removed.is_some();
        drop(removed);
        let source_is_open = fcntl(source, F_GETFD).is_ok();
        // Never resume a vulnerable export after invalidating its source.
        drop(export);
        drop(pty);

        assert!(
            !was_removable,
            "export released stdin ownership before duplication; source still open: {source_is_open}"
        );
        assert!(source_is_open);
        assert_eq!(identity(source), before);
        assert!(entry.stdin.try_lock().unwrap().is_some());
    });
}

#[test]
fn readopt_cancellation_closes_current_and_later_transferred_endpoints() {
    if !in_isolated_process("readopt_cancellation_closes_current_and_later_transferred_endpoints") {
        return;
    }
    use std::io::Read;
    use std::os::fd::IntoRawFd;

    let (executor, _reserved) = private_fd_runtime();
    executor.block_on(async {
        let runtime = fixture_runtime();
        let (endpoints, mut peers): (Vec<_>, Vec<_>) = (0..8)
            .map(|_| {
                let (endpoint, peer) = UnixStream::pair().unwrap();
                endpoint.set_nonblocking(true).unwrap();
                peer.set_nonblocking(true).unwrap();
                (endpoint, peer)
            })
            .unzip();
        let registry = runtime.registry.lock().await;
        let (_thaw, thawed) = tokio::sync::watch::channel(());
        let raw: Vec<_> = endpoints.into_iter().map(IntoRawFd::into_raw_fd).collect();
        // Reserved identities <= 1 also ensure an unexpected refusal cannot
        // signal a real process through the unadopted-child cleanup path.
        let processes = [
            carried(0, std::array::from_fn(|index| raw[index])),
            carried(1, std::array::from_fn(|index| raw[index + 4])),
        ];
        // SAFETY: every endpoint is open and unique; into_raw_fd transferred
        // its sole owner, and no caller closes/reuses a transferred number.
        let mut readopt = Box::pin(unsafe { runtime.readopt_processes(&processes, thawed) });

        // Valid socket registrations complete synchronously. Only insertion
        // can block here, and the second process remains pending in the batch.
        assert!(futures_util::poll!(readopt.as_mut()).is_pending());
        drop(readopt);

        for (index, peer) in peers.iter_mut().enumerate() {
            let mut byte = [0];
            assert_eq!(
                peer.read(&mut byte)
                    .expect("cancelled handover must reach EOF"),
                0,
                "transferred endpoint {index} survived cancellation"
            );
        }
        assert!(registry.live.is_empty());
    });
}

#[test]
fn readopt_registration_failure_releases_endpoints_and_adopts_valid_sibling() {
    if !in_isolated_process(
        "readopt_registration_failure_releases_endpoints_and_adopts_valid_sibling",
    ) {
        return;
    }
    use std::io::Read;
    use std::os::fd::IntoRawFd;

    let (executor, _reserved) = private_fd_runtime();
    executor.block_on(async {
        let runtime = fixture_runtime();
        let (stdout, mut stdout_peer) = UnixStream::pair().unwrap();
        let (stdin, mut stdin_peer) = UnixStream::pair().unwrap();
        let (tty, mut tty_peer) = UnixStream::pair().unwrap();
        for socket in [&stdout, &stdout_peer, &stdin, &stdin_peer, &tty, &tty_peer] {
            socket.set_nonblocking(true).unwrap();
        }
        let stderr = std::fs::File::open("/dev/null").unwrap();
        let (_thaw, thawed) = tokio::sync::watch::channel(());
        let processes = [
            // stdout can enter epoll; /dev/null deterministically fails the
            // subsequent stderr registration with EPERM. PID 0 is never killed.
            carried(
                0,
                [
                    stdout.into_raw_fd(),
                    stderr.into_raw_fd(),
                    stdin.into_raw_fd(),
                    tty.into_raw_fd(),
                ],
            ),
            carried(1, [-1; 4]),
        ];

        // SAFETY: the four open descriptors have distinct numbers and were
        // surrendered above. The sibling has no transferred descriptors.
        let result = unsafe { runtime.readopt_processes(&processes, thawed).await };

        assert_eq!(result, (1, 1));
        for peer in [&mut stdout_peer, &mut stdin_peer, &mut tty_peer] {
            let mut byte = [0];
            assert_eq!(
                peer.read(&mut byte)
                    .expect("failed adoption must release its endpoints"),
                0
            );
        }
        let registry = runtime.registry.lock().await;
        assert!(!registry.live.contains_key(&0));
        assert!(registry.live.contains_key(&1));
    });
}

#[test]
fn terminal_handover_descriptor_keeps_master_alive_after_close() {
    if !in_isolated_process("terminal_handover_descriptor_keeps_master_alive_after_close") {
        return;
    }
    let (executor, _reserved) = private_fd_runtime();
    executor.block_on(async {
        // A private /dev/ptmx pair needs neither a child process nor a desktop.
        let pair = nix::pty::openpty(None, None).unwrap();
        let _slave = pair.slave;
        let terminal = super::pty::Terminal::from_master(pair.master).unwrap();
        let snapshot: Arc<tokio::io::unix::AsyncFd<std::fs::File>> = terminal
            .handover_descriptor()
            .expect("open terminal must provide an owned handover snapshot");
        let fd = snapshot.get_ref().as_raw_fd();
        let before = identity(fd);

        terminal.close();

        assert!(terminal.handover_descriptor().is_none());
        assert_eq!(identity(snapshot.get_ref().as_raw_fd()), before);
        assert!(
            fcntl(fd, F_GETFD).is_ok(),
            "snapshot must keep the master open"
        );
        drop(snapshot);
        assert_eq!(
            fcntl(fd, F_GETFD),
            Err(nix::errno::Errno::EBADF),
            "closed terminal's last snapshot must release the master"
        );
    });
}
