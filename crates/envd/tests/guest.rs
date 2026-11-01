use coorworker_envd::guest::{validate_cpu_helper_boot, validate_guest_boot};

#[test]
fn cpu_helper_requires_root_boot_opt_in_and_the_same_pid_one_executable() {
    let executable = std::path::Path::new("/envd");
    assert!(validate_cpu_helper_boot(0, "coorworker.envd=1", executable, executable).is_ok());
    for (uid, cmdline, init) in [
        (1000, "coorworker.envd=1", "/envd"),
        (0, "", "/envd"),
        (0, "coorworker.envd=10", "/envd"),
        (0, "coorworker.envd=1", "/sbin/init"),
    ] {
        assert!(
            validate_cpu_helper_boot(uid, cmdline, std::path::Path::new(init), executable).is_err()
        );
    }
}

#[test]
fn guest_requires_root_pid_one_and_exact_kernel_opt_in() {
    assert!(validate_guest_boot(0, 1, "console=ttyS0 coorworker.envd=1 panic=-1").is_ok());
    for (uid, pid, cmdline) in [
        (1000, 1, "coorworker.envd=1"),
        (0, 42, "coorworker.envd=1"),
        (0, 1, ""),
        (0, 1, "othercoorworker.envd=1"),
        (0, 1, "coorworker.envd=10"),
    ] {
        assert!(validate_guest_boot(uid, pid, cmdline).is_err());
    }
}
