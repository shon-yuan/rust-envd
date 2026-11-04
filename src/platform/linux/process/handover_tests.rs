use super::{dup_keep, validate_carried_fds};
use crate::proto::upgrade::HandoverProc;
use nix::fcntl::{fcntl, FcntlArg::F_GETFD, FdFlag};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;

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
