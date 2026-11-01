//! Oracle forward.go: the loopback forwarder itself.
//!
//! Every scan refresh (forward.go:126-204) marks all tracked ports DELETE,
//! re-marks the ones still seen LISTENing FORWARD, spawns a socat for new
//! ports and kills the process group of the DELETE ones. A socat that could
//! not start is NOT tracked (the next scan sees an unforwarded port and tries
//! again, forward.go:184-189) — but a spawn REFUSED because the socat cgroup
//! is frozen retries on the next scan the same way (forward.go:236-248):
//! both paths remove the entry, the difference is only the log level.
//!
//! Deviation from the oracle, disclosed: Go's `forwardingAvailable()` always
//! returns nil because Go envd only ever builds for the guest image; the Rust
//! daemon also runs as a development daemon on a host, where spawning socats
//! bound to the guest gateway IP (169.254.0.21) would try to expose host
//! loopback services. There the oracle's own "unavailable" path is used —
//! `forwarding_available(false)` errors, the forwarder logs the same
//! "port forwarding disabled" warning (forward.go:104), subscribes to nothing
//! and starts no tasks (main.go:356-360).

use std::{collections::HashMap, io, sync::Arc};

use nix::{
    sys::signal::{kill, Signal},
    unistd::Pid,
};
use tokio::sync::oneshot;

use super::{
    procnet::ConnectionStat,
    scan::Scanner,
    scan_subscriber::{ScannerFilter, ScannerSubscriber},
};
use crate::{
    cgroups::{freeze::WorkloadFreezer, place_into_cgroup, ProcessType},
    logs::Logger,
};

/// Oracle forward.go:31 — the guest gateway IP the socats bind.
pub const DEFAULT_GATEWAY_IP: [u8; 4] = [169, 254, 0, 21];

/// Oracle forward.go:24-29.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PortState {
    Forward,
    Delete,
}

/// One active port-forward carried across a live-upgrade
/// (handover.proto ForwardedPort fields 1-5). The wire encoding is the
/// generated `proto::upgrade::ForwardedPort` — byte-identical to Go's
/// proto.Marshal by construction; `to_wire`/`from_wire` are the conversions
/// the upgrade blob travels through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForwardedPort {
    /// The forwarder map key: "<listener_pid>-<port>" (forward.go:160).
    pub key: String,
    pub port: u32,
    /// pid of the guest process listening on the port.
    pub listener_pid: i32,
    /// IP version (4 or 6).
    pub family: u32,
    /// pid of the running socat child to re-adopt.
    pub socat_pid: i32,
}

impl ForwardedPort {
    pub fn to_wire(&self) -> crate::proto::upgrade::ForwardedPort {
        crate::proto::upgrade::ForwardedPort {
            key: self.key.clone(),
            port: self.port,
            listener_pid: self.listener_pid,
            family: self.family,
            socat_pid: self.socat_pid,
            ..Default::default()
        }
    }

    pub fn from_wire(port: &crate::proto::upgrade::ForwardedPort) -> Self {
        Self {
            key: port.key.clone(),
            port: port.port,
            listener_pid: port.listener_pid,
            family: port.family,
            socat_pid: port.socat_pid,
        }
    }
}

/// A socat this envd spawned itself: Go keeps the `*exec.Cmd`
/// (forward.go:34); only its pid is read afterwards (socatPID).
struct SocatHandle {
    pid: i32,
}

/// Oracle forward.go:33-45.
struct PortToForward {
    /// Present only for a socat this envd spawned; a re-adopted socat has no
    /// child handle across the handover (forward.go:35-38).
    socat: Option<SocatHandle>,
    /// The pid of a socat re-adopted across a live-upgrade.
    socat_pid: i32,
    /// Process ID of the process that's listening on port.
    pid: i32,
    /// family version of the ip.
    family: u32,
    state: PortState,
    port: u32,
}

impl PortToForward {
    /// Oracle socatPID (forward.go:50-56): the running socat's pid regardless
    /// of how it was started; 0 when there is none.
    fn socat_pid(&self) -> i32 {
        match &self.socat {
            Some(handle) => handle.pid,
            None => self.socat_pid,
        }
    }
}

/// syscall.AF_INET -> 4, syscall.AF_INET6 -> 6, anything else 0
/// (forward.go:389-398).
pub fn family_to_ip_version(family: u32) -> u32 {
    match family {
        super::procnet::AF_INET => 4,
        super::procnet::AF_INET6 => 6,
        _ => 0,
    }
}

/// The exact socat argv (forward.go:211-218): `socat -d -d -d
/// TCP4-LISTEN:<port>,bind=<gateway>,reuseaddr,fork TCP<family>:localhost:<port>`
/// — reuseaddr fixes "Address already in use" on quick socat restarts.
pub(crate) fn socat_argv(port: u32, family: u32, source_ip: [u8; 4]) -> Vec<String> {
    let [a, b, c, d] = source_ip;
    vec![
        "-d".into(),
        "-d".into(),
        "-d".into(),
        format!("TCP4-LISTEN:{port},bind={a}.{b}.{c}.{d},reuseaddr,fork"),
        format!("TCP{family}:localhost:{port}"),
    ]
}

/// The socat spawn edge. Go builds an `exec.Cmd` inline (forward.go:214-218);
/// the seam exists so the tracking/retry/stop semantics are testable without
/// depending on a socat binary on the development host (the cpus module's
/// CpuSysfs injection is the established precedent). The production
/// implementation is [`RealSocat`].
pub trait SocatSpawn: Send + Sync {
    /// Start the socat described by `argv` (argv[0] is the program); a cgroup
    /// fd, when given, must place the child into that cgroup before exec.
    fn spawn(&self, argv: &[String], cgroup_fd: Option<i32>) -> io::Result<tokio::process::Child>;
}

/// The production spawn: `socat` in its own process group
/// (forward.go:222-224 Setpgid), placed into the socat cgroup at spawn time
/// when the manager hands out a cgroup fd (forward.go:220-225
/// applyCgroupFD). Go uses clone3(CLONE_INTO_CGROUP) through SysProcAttr;
/// the Rust child opens `cgroup.procs` relative to that directory fd in
/// pre_exec and writes "0" (the calling task), joining the cgroup before
/// execve — the same placement, reached with async-signal-safe syscalls only.
///
/// The frozen-cgroup risk model, kept honest: the placement equivalence
/// holds (the child is born into the cgroup, before execve), but the spawn
/// is NOT free of the oracle's wedge risk. std/tokio `spawn` synchronizes
/// with the child's execve on the CLOEXEC error pipe, so a child frozen in
/// the BeginSpawn probe window (spawn.go:229-235 — "leaves a window between
/// the read and the clone that it cannot close") parks the caller inside
/// `spawn()`. The oracle accepts the same window with the same comment
/// (forward.go:235-237); the blast radius differs — Go's
/// clone3-with-vfork-style fork wedges an unpreemptible runtime syscall and
/// the next stop-the-world stops ALL of envd, while the Rust caller parked in
/// spawn holds the forwarder's ports lock and one runtime worker. Strictly
/// smaller, same accepted risk.
pub struct RealSocat;

impl SocatSpawn for RealSocat {
    fn spawn(&self, argv: &[String], cgroup_fd: Option<i32>) -> io::Result<tokio::process::Child> {
        let program = argv.first().cloned().unwrap_or_default();
        let mut command = tokio::process::Command::new(program);
        command.args(&argv[1..]);
        // SAFETY: the closure runs between fork and exec in the child; it
        // calls setpgid (a raw syscall over scalars) and, with a cgroup fd,
        // openat/write/close on the inherited descriptor — all
        // async-signal-safe, no allocator, locks or user code.
        unsafe {
            command.pre_exec(move || {
                nix::unistd::setpgid(Pid::from_raw(0), Pid::from_raw(0))
                    .map_err(io::Error::from)?;
                if let Some(fd) = cgroup_fd {
                    place_into_cgroup(fd)?;
                }
                Ok(())
            });
        }
        command.spawn()
    }
}

/// Oracle forward.go:75-77: this build can forward ports. Go returns nil
/// unconditionally because it only builds for the guest image; the Rust
/// daemon also runs as a development daemon on a host, where socat children
/// bound to the guest gateway IP would try to expose host loopback services —
/// there the oracle's own "unavailable" path disables the forwarder exactly
/// as designed (forward.go:101-105).
#[derive(Debug, thiserror::Error)]
#[error("port forwarding forwards guest loopback ports onto the guest gateway IP; it is only available to a guest daemon")]
pub struct ForwardUnavailable;

pub fn forwarding_available(guest: bool) -> Result<(), ForwardUnavailable> {
    if guest {
        Ok(())
    } else {
        Err(ForwardUnavailable)
    }
}

/// Oracle forward.go:58-73. The ports map is a `tokio::sync::Mutex`: the scan
/// loop holds it across the whole refresh, including the awaited spawn
/// (forward.go:143-146), and the export/import paths take it too
/// (forward.go:66-68 serializes the map access).
pub struct Forwarder {
    logger: Logger,
    /// Admission for every socat spawn (forward.go:59-64): a frozen workload
    /// blocks new socats until thawed (insert→start→retry contract).
    freezer: Arc<WorkloadFreezer>,
    /// Both places the socat (through its manager) and serializes the map
    /// (forward.go:59-64, 66-68). Arc'd so ExportForwardsHold can hand out an
    /// owned guard that survives to the execve.
    ports: Arc<tokio::sync::Mutex<HashMap<String, PortToForward>>>,
    scanner_subscriber: Option<Arc<ScannerSubscriber>>,
    source_ip: [u8; 4],
    spawner: Arc<dyn SocatSpawn>,
}

impl Forwarder {
    /// Oracle NewForwarder (forward.go:79-85) with this environment's
    /// availability.
    pub fn new(
        logger: &Logger,
        scanner: &Arc<Scanner>,
        freezer: Arc<WorkloadFreezer>,
        guest: bool,
    ) -> Self {
        Self::build(
            logger,
            scanner,
            freezer,
            forwarding_available(guest).map_err(|error| error.to_string()),
        )
    }

    /// Oracle newForwarder (forward.go:89-117); `unavailable` is why this
    /// build cannot forward ports, Ok where it can. An unavailable forwarder
    /// subscribes to nothing — a subscription nobody drains would park the
    /// scan loop on its first broadcast (forward.go:101-105).
    pub fn build(
        logger: &Logger,
        scanner: &Arc<Scanner>,
        freezer: Arc<WorkloadFreezer>,
        unavailable: Result<(), String>,
    ) -> Self {
        let mut scanner_subscriber = None;
        match unavailable {
            Err(unavailable) => {
                logger
                    .warn()
                    .err(&unavailable)
                    .msg("port forwarding disabled");
            }
            Ok(()) => {
                // We only want to forward ports that are actively listening on
                // localhost (forward.go:106-113).
                scanner_subscriber = Some(scanner.add_subscriber(
                    "port-forwarder",
                    Some(ScannerFilter {
                        ips: vec!["127.0.0.1".into(), "localhost".into(), "::1".into()],
                        state: "LISTEN".into(),
                    }),
                ));
            }
        }
        Self {
            logger: logger.clone(),
            freezer,
            ports: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            scanner_subscriber,
            source_ip: DEFAULT_GATEWAY_IP,
            spawner: Arc::new(RealSocat),
        }
    }

    /// Test seam: an injected spawn edge (see [`SocatSpawn`]).
    #[doc(hidden)]
    pub fn with_spawner(mut self, spawner: Arc<dyn SocatSpawn>) -> Self {
        self.spawner = spawner;
        self
    }

    /// Oracle Forwards (forward.go:122-124): whether StartForwarding and the
    /// scan it consumes need to run.
    pub fn forwards(&self) -> bool {
        self.scanner_subscriber.is_some()
    }

    /// Test support: the tracked map keys under the lock
    /// (TestStartForwarding_RetriesAPortSkippedWhileFrozen's `tracked()`).
    #[doc(hidden)]
    pub async fn tracked_keys(&self) -> Vec<String> {
        self.ports.lock().await.keys().cloned().collect()
    }

    /// Test support: the forwarder's own subscription, so tests can drive
    /// scans the way the oracle tests drive `f.scannerSubscriber.Messages`.
    #[doc(hidden)]
    pub fn subscriber(&self) -> Option<Arc<ScannerSubscriber>> {
        self.scanner_subscriber.clone()
    }

    /// Test support: the recorded socat pids this forwarder spawned and
    /// still tracks, so tests can clean process groups up.
    #[doc(hidden)]
    pub async fn tracked_socat_pids(&self) -> Vec<i32> {
        self.ports
            .lock()
            .await
            .values()
            .filter_map(|port| port.socat.as_ref().map(|handle| handle.pid))
            .collect()
    }

    /// Oracle StartForwarding (forward.go:126-204). Returns on the stop
    /// signal (Go's ctx) or when the scan channel closed (forward.go:133-141).
    pub async fn start_forwarding(&self, mut stop: oneshot::Receiver<()>) {
        let Some(subscriber) = self.scanner_subscriber.clone() else {
            self.logger
                .error()
                .msg("Cannot start forwarding because scanner subscriber is nil");
            return;
        };
        loop {
            let procs = tokio::select! {
                // Go's ctx cancellation (the run loop's CommandContext,
                // forward.go:214) kills every SELF-spawned socat pid-only on
                // the way out; a re-adopted socat has no *exec.Cmd and is not
                // ctx-bound (forward.go:296-298), and a closed scan channel is
                // not a kill trigger (forward.go:135-137) — so this arm is the
                // only shutdown that kills.
                _ = &mut stop => {
                    self.kill_self_spawned_on_shutdown().await;
                    return;
                }
                procs = subscriber.recv() => match procs {
                    None => return,
                    Some(procs) => procs,
                },
            };
            // Serialize the whole refresh against a concurrent export
            // (forward.go:143-146).
            let mut ports = self.ports.lock().await;
            self.refresh(&mut ports, &procs).await;
        }
    }

    /// The refresh body (forward.go:148-199), under the ports lock.
    async fn refresh(&self, ports: &mut HashMap<String, PortToForward>, procs: &[ConnectionStat]) {
        // Everything tracked goes to DELETE first (forward.go:151-155).
        for port in ports.values_mut() {
            port.state = PortState::Delete;
        }
        for seen in procs {
            let key = format!("{}-{}", seen.pid, seen.laddr.port);
            match ports.get_mut(&key) {
                // Still forwarded; keep its running socat (forward.go:163-167).
                Some(tracked) => tracked.state = PortState::Forward,
                None => {
                    self.logger
                        .debug()
                        .str("ip", &seen.laddr.ip)
                        .int("port", seen.laddr.port as i64)
                        .int("family", family_to_ip_version(seen.family) as i64)
                        .str("state", &seen.status)
                        .msg("Detected new opened port on localhost that is not forwarded");
                    // Go inserts the entry BEFORE starting the socat
                    // (forward.go:177-189): a failed start (retry=false)
                    // STAYS tracked — the port reads as forwarded and dark
                    // until its listener reopens — while a REFUSED start
                    // (retry=true, frozen cgroup) is removed so the next scan
                    // sees an unforwarded port and tries again.
                    ports.insert(
                        key.clone(),
                        PortToForward {
                            socat: None,
                            socat_pid: 0,
                            pid: seen.pid,
                            port: seen.laddr.port,
                            state: PortState::Forward,
                            family: family_to_ip_version(seen.family),
                        },
                    );
                    let mut retry = false;
                    if let Some(tracked) = ports.get_mut(&key) {
                        retry = self.start_port_forwarding(tracked).await;
                    }
                    if retry {
                        ports.remove(&key);
                    }
                }
            }
        }
        // Stop everything still marked DELETE (forward.go:193-199).
        for port in ports.values_mut() {
            if port.state == PortState::Delete {
                self.stop_port_forwarding(port);
            }
        }
    }

    /// Oracle startPortForwarding (forward.go:210-275). Returns retry: a
    /// start REFUSED (frozen socat cgroup) is attempted again on the next
    /// scan; a start that FAILED is not (forward.go:206-209).
    async fn start_port_forwarding(&self, port: &mut PortToForward) -> bool {
        let argv = socat_argv(port.port, port.family, self.source_ip);
        let command_display = argv_display("socat", &argv);
        let cgroup_fd = self
            .freezer
            .manager()
            .get_file_descriptor(ProcessType::Socat);
        self.logger
            .debug()
            .str("socatCmd", &command_display)
            .int("pid", port.pid as i64)
            .int("family", port.family as i64)
            .str("sourceIP", dotted(self.source_ip))
            .int("port", port.port as i64)
            .msg("About to start port forwarding");

        // A socat cloned into a frozen cgroup never reaches execve — the
        // oracle skips the spawn and lets the next scan retry for exactly
        // that (forward.go:235-237). The BeginSpawn probe narrows but never
        // closes the window (spawn.go:229-235); a socat wedged in it takes no
        // admission, so nothing catches it, here or in Go — see the
        // RealSocat risk model above for the accepted blast radius.
        let spawn_admission = match self.freezer.begin_spawn(ProcessType::Socat, None).await {
            Ok(admission) => admission,
            Err(error) => {
                self.logger
                    .debug()
                    .int("port", port.port as i64)
                    .err(&error)
                    .msg("Deferring port forwarding: the socat cgroup is frozen");
                return true;
            }
        };
        let spawned = self.spawner.spawn(&argv, cgroup_fd);
        drop(spawn_admission);

        let child = match spawned {
            Ok(child) => child,
            Err(error) => {
                self.logger
                    .error()
                    .str("socatCmd", &command_display)
                    .err(&error)
                    .msg("Failed to start port forwarding - failed to start socat");
                return false;
            }
        };
        let pid = child.id().map(|raw| raw as i32).unwrap_or_default();
        // Go starts a Wait goroutine per socat and logs a nonzero exit at
        // Debug (forward.go:262-270); a failed reap logs the same way.
        let wait_logger = self.logger.clone();
        let wait_command = command_display.clone();
        tokio::spawn(async move {
            let mut child = child;
            match child.wait().await {
                Ok(status) if !status.success() => {
                    wait_logger
                        .debug()
                        .str("socatCmd", &wait_command)
                        .err(&status)
                        .msg("Port forwarding socat process exited");
                }
                Ok(_) => {}
                Err(error) => {
                    wait_logger
                        .debug()
                        .str("socatCmd", &wait_command)
                        .err(&error)
                        .msg("Port forwarding socat process exited");
                }
            }
        });
        port.socat = Some(SocatHandle { pid });
        false
    }

    /// Oracle stopPortForwarding (forward.go:277-305): capture the identity
    /// fields, clear the socat handle (Go's deferred clear), then kill the
    /// socat's process group (Setpgid made it a group leader).
    fn stop_port_forwarding(&self, port: &mut PortToForward) {
        let pid = port.socat_pid();
        if pid <= 0 {
            return;
        }
        let logger = self
            .logger
            .with_str("socatPid", pid.to_string())
            .with_str("pid", port.pid.to_string())
            .with_str("family", port.family.to_string())
            .with_str("sourceIP", dotted(self.source_ip))
            .with_str("port", port.port.to_string());
        port.socat = None;
        port.socat_pid = 0;
        logger.debug().msg("Stopping port forwarding");
        if let Err(error) = kill(Pid::from_raw(-pid), Signal::SIGKILL) {
            logger
                .error()
                .err(&error)
                .msg("Failed to kill process group");
            return;
        }
        logger.debug().msg("Stopped port forwarding");
    }

    /// Oracle ExportForwards (forward.go:312-317, 334-351): only ports that
    /// actually have a socat are carried.
    pub async fn export_forwards(&self) -> Vec<ForwardedPort> {
        let ports = self.ports.lock().await;
        export_locked(&ports)
    }

    /// Oracle ExportForwardsHold (forward.go:319-332): the snapshot with the
    /// ports lock KEPT, so the scan loop cannot spawn a new socat in the
    /// handover window — one started after the snapshot would be orphaned by
    /// the swap (never carried, never re-adopted) or duplicate a port the new
    /// envd re-adopts, contending on the same bind address. A successful
    /// execve replaces this process and the held lock vanishes with it;
    /// dropping the guard on a failure path restores scanning.
    pub async fn export_forwards_hold(&self) -> (Vec<ForwardedPort>, ForwardsHold) {
        let guard = self.ports.clone().lock_owned().await;
        (export_locked(&guard), ForwardsHold { _guard: guard })
    }

    /// The ctx-death kill of the run loop (Go's CommandContext,
    /// forward.go:214): SIGKILL each SELF-spawned socat's pid — pid-only,
    /// exactly Process.Kill; the re-adopted entries (`socat_pid`, no handle)
    /// are not ctx-bound and stay. The per-socat wait task reaps what dies.
    async fn kill_self_spawned_on_shutdown(&self) {
        let ports = self.ports.lock().await;
        for port in ports.values() {
            let Some(handle) = &port.socat else {
                continue;
            };
            // SAFETY: kill(2) over scalars; a pid that already exited just
            // errors and is ignored, the way Go's kill goroutine races.
            let _ = kill(Pid::from_raw(handle.pid), Signal::SIGKILL);
        }
    }

    /// Oracle ImportForwards (forward.go:360-387): seed the map so the next
    /// scan recognizes each already-forwarded port instead of spawning a
    /// duplicate socat, and reap each re-adopted socat (there is no surviving
    /// Wait goroutine). A socat that did not survive the handover is skipped
    /// so the next scan respawns a fresh one.
    pub async fn import_forwards(&self, forwards: Vec<ForwardedPort>) -> usize {
        let mut ports = self.ports.lock().await;
        let mut readopted = 0;
        for forwarded in forwards {
            let pid = forwarded.socat_pid;
            if pid <= 0 {
                continue;
            }
            // Only re-adopt a socat that is still alive (kill -0), so a dead
            // pid is never recorded as "forwarded" (forward.go:369-374).
            // SAFETY: kill(2) over scalars; signal 0 is the liveness probe.
            if unsafe { nix::libc::kill(pid, 0) } != 0 {
                continue;
            }
            ports.insert(
                forwarded.key.clone(),
                PortToForward {
                    socat: None,
                    socat_pid: pid,
                    pid: forwarded.listener_pid,
                    port: forwarded.port,
                    family: forwarded.family,
                    state: PortState::Forward,
                },
            );
            start_reaper(pid);
            readopted += 1;
        }
        readopted
    }
}

/// The shared export body (forward.go:334-351): only ports that actually have
/// a socat running are carried — a tracked-but-dark entry has nothing to
/// re-adopt and the new envd's scan respawns it anyway.
fn export_locked(ports: &HashMap<String, PortToForward>) -> Vec<ForwardedPort> {
    let mut forwards = Vec::new();
    for (key, port) in ports {
        let socat_pid = port.socat_pid();
        if socat_pid <= 0 {
            continue;
        }
        forwards.push(ForwardedPort {
            key: key.clone(),
            port: port.port,
            listener_pid: port.pid,
            family: port.family,
            socat_pid,
        });
    }
    forwards
}

/// The ports lock held from an ExportForwardsHold snapshot through the execve
/// (forward.go:319-332): the scan loop cannot start a socat in the handover
/// window — one started after the snapshot would be orphaned by the swap (never
/// carried, never re-adopted) or duplicate a port the new envd re-adopts,
/// contending on the same bind address. An owned guard, so it can outlive the
/// snapshot call; a successful execve replaces this process and the held lock
/// vanishes with it, and dropping it on a failure path restores scanning.
pub struct ForwardsHold {
    _guard: tokio::sync::OwnedMutexGuard<HashMap<String, PortToForward>>,
}

fn dotted(ip: [u8; 4]) -> String {
    let [a, b, c, d] = ip;
    format!("{a}.{b}.{c}.{d}")
}

/// Log display for a spawn. Deviation from the oracle, disclosed: Go logs
/// `cmd.String()`, which renders the PATH-resolved absolute command path
/// (exec.CommandContext LookPath-resolves "socat" into cmd.Path) plus args;
/// this joins the literal `"socat"` program name instead — display only, the
/// spawned argv is the same.
fn argv_display(program: &str, argv: &[String]) -> String {
    let mut display = String::from(program);
    for arg in argv {
        display.push(' ');
        display.push_str(arg);
    }
    display
}

/// Go `go reaper.Reap(pid)` (forward.go:382): wait4 the re-adopted socat so
/// it does not linger as a zombie once it exits (reaper.go Reap).
fn start_reaper(pid: i32) {
    let _ = std::thread::Builder::new()
        .name(format!("socat-reaper-{pid}"))
        .spawn(move || reap(pid));
}

fn reap(pid: i32) {
    loop {
        match nix::sys::wait::waitpid(Pid::from_raw(pid), None) {
            Err(nix::errno::Errno::EINTR) => continue,
            _ => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::procnet::{Addr, AF_INET, AF_INET6};
    use std::os::unix::process::CommandExt;
    use std::{sync::Mutex, time::Duration};

    /// A scripted scan source the forwarder tests never call (scans are
    /// driven through the subscriber directly).
    struct NullSource;

    impl crate::port::procnet::ScanSource for NullSource {
        fn connections(&self) -> io::Result<Vec<ConnectionStat>> {
            Ok(Vec::new())
        }
    }

    fn test_logger() -> Logger {
        Logger::none()
    }

    fn test_scanner(logger: &Logger) -> Arc<Scanner> {
        Arc::new(Scanner::with_source(
            logger,
            Duration::from_secs(3600),
            Arc::new(NullSource),
        ))
    }

    fn noop_freezer() -> Arc<WorkloadFreezer> {
        Arc::new(WorkloadFreezer::new(Arc::new(
            crate::cgroups::noop::NoopManager,
        )))
    }

    fn listen_row(pid: i32, port: u32) -> ConnectionStat {
        ConnectionStat {
            fd: 0,
            family: AF_INET,
            sock_type: 1,
            laddr: Addr {
                ip: "127.0.0.1".into(),
                port,
            },
            raddr: Addr {
                ip: "0.0.0.0".into(),
                port: 0,
            },
            status: "LISTEN".into(),
            pid,
        }
    }

    fn batch(rows: Vec<ConnectionStat>) -> super::super::scan_subscriber::ScanBatch {
        Arc::new(rows)
    }

    /// Records every spawn request; either fails (no socat in this build) or
    /// starts a real `sleep 30` in its own process group, so stop-kill and
    /// re-adoption semantics run against a real process.
    struct RecordingSocat {
        spawns: Mutex<Vec<RecordedSpawn>>,
        fail: bool,
    }

    #[derive(Clone, Debug)]
    struct RecordedSpawn {
        argv: Vec<String>,
        cgroup_fd: Option<i32>,
    }

    impl RecordingSocat {
        fn failing() -> Arc<Self> {
            Arc::new(Self {
                spawns: Mutex::new(Vec::new()),
                fail: true,
            })
        }

        fn sleeping() -> Arc<Self> {
            Arc::new(Self {
                spawns: Mutex::new(Vec::new()),
                fail: false,
            })
        }

        fn recorded(&self) -> Vec<RecordedSpawn> {
            self.spawns.lock().unwrap().clone()
        }
    }

    impl SocatSpawn for RecordingSocat {
        fn spawn(
            &self,
            argv: &[String],
            cgroup_fd: Option<i32>,
        ) -> io::Result<tokio::process::Child> {
            self.spawns.lock().unwrap().push(RecordedSpawn {
                argv: argv.to_vec(),
                cgroup_fd,
            });
            if self.fail {
                return Err(io::Error::other("no socat in this build"));
            }
            let mut command = tokio::process::Command::new("/bin/sleep");
            command.arg("30");
            // SAFETY: setpgid between fork and exec, scalars only.
            unsafe {
                command.pre_exec(|| {
                    nix::unistd::setpgid(Pid::from_raw(0), Pid::from_raw(0))
                        .map_err(io::Error::from)
                });
            }
            command.spawn()
        }
    }

    /// The group of `pid` is gone (kill(-pid, 0) fails) within the timeout.
    async fn group_gone(pid: i32) -> bool {
        for _ in 0..200 {
            // SAFETY: kill(2) over scalars; signal 0 is the liveness probe.
            if unsafe { nix::libc::kill(-pid, 0) } != 0 {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    /// Oracle forward.go:389-398.
    #[test]
    fn family_to_ip_version_maps_only_inet_families() {
        assert_eq!(family_to_ip_version(AF_INET), 4);
        assert_eq!(family_to_ip_version(AF_INET6), 6);
        assert_eq!(family_to_ip_version(1), 0, "unknown family stays 0");
    }

    /// Oracle forward.go:211-218: the exact socat argv, reuseaddr included.
    #[test]
    fn socat_argv_matches_the_oracle_command_shape() {
        assert_eq!(
            socat_argv(4000, 4, DEFAULT_GATEWAY_IP),
            vec![
                "-d".to_owned(),
                "-d".to_owned(),
                "-d".to_owned(),
                "TCP4-LISTEN:4000,bind=169.254.0.21,reuseaddr,fork".to_owned(),
                "TCP4:localhost:4000".to_owned(),
            ]
        );
        // The family rides the TARGET address only; the listener is always
        // TCP4 (forward.go:216-217).
        assert_eq!(
            socat_argv(4000, 6, DEFAULT_GATEWAY_IP)[4],
            "TCP6:localhost:4000"
        );
    }

    /// Oracle scanner_test.go:101-133: an unavailable forwarder holds no
    /// subscription nobody drains and tells envd not to run the scan.
    #[test]
    fn an_unavailable_forwarder_subscribes_to_nothing() {
        let logger = test_logger();
        let scanner = test_scanner(&logger);
        let forwarder = Forwarder::build(
            &logger,
            &scanner,
            noop_freezer(),
            Err("no forwarding in this build".into()),
        );
        assert!(!forwarder.forwards());
        assert_eq!(scanner.subscriber_count(), 0);

        let available = Forwarder::build(&logger, &scanner, noop_freezer(), Ok(()));
        assert!(available.forwards());
        assert_eq!(scanner.subscriber_count(), 1);
        let subscriber = available.subscriber().unwrap();
        assert_eq!(subscriber.id(), "port-forwarder");
    }

    /// The disclosed dev-mode gate: forwarding_available is the oracle's
    /// forwardingAvailable() with the environment as its only input.
    #[test]
    fn forwarding_available_follows_the_guest_environment() {
        assert!(forwarding_available(true).is_ok());
        assert!(forwarding_available(false).is_err());
    }

    /// Oracle forward_handover_test.go:27-41: export only ports that actually
    /// have a socat — a half-set-up entry (its socat was stopped) is not
    /// carried as if it were forwarded.
    #[tokio::test]
    async fn export_carries_only_entries_with_a_live_socat_pid() {
        let logger = test_logger();
        let scanner = test_scanner(&logger);
        let forwarder = Arc::new(
            Forwarder::build(&logger, &scanner, noop_freezer(), Ok(()))
                .with_spawner(RecordingSocat::sleeping()),
        );
        let subscriber = forwarder.subscriber().unwrap();
        let (stop, stop_rx) = oneshot::channel();
        let forwarder_for_task = Arc::clone(&forwarder);
        let task = tokio::spawn(async move { forwarder_for_task.start_forwarding(stop_rx).await });

        // A real child stands in for the socat (oracle: exec sleep 30 with
        // Setpgid, forward_handover_test.go:65-67); import re-adopts it
        // exactly as the upgrade handover does.
        let mut command = std::process::Command::new("/bin/sleep");
        command.arg("30");
        // SAFETY: setpgid between fork and exec, scalars only.
        unsafe {
            command.pre_exec(|| {
                nix::unistd::setpgid(Pid::from_raw(0), Pid::from_raw(0)).map_err(io::Error::from)
            });
        }
        let mut child = command.spawn().unwrap();
        let socat_pid = child.id() as i32;

        let readopted = forwarder
            .import_forwards(vec![ForwardedPort {
                key: "100-8080".into(),
                port: 8080,
                listener_pid: 100,
                family: 4,
                socat_pid,
            }])
            .await;
        assert_eq!(readopted, 1);

        let exported = forwarder.export_forwards().await;
        assert_eq!(
            exported,
            vec![ForwardedPort {
                key: "100-8080".into(),
                port: 8080,
                listener_pid: 100,
                family: 4,
                socat_pid,
            }]
        );

        // Drive a refresh where the port is gone: the entry stays in the map
        // (Go never removes keys) but its socat is stopped and cleared, so a
        // later export must skip it (forward.go:336-340).
        subscriber.signal(batch(vec![])).await;
        subscriber.signal(batch(vec![])).await;
        assert!(forwarder.export_forwards().await.is_empty());
        assert!(forwarder.tracked_keys().await.contains(&"100-8080".into()));

        // Cleanup: the stop already killed the group; make sure the child is
        // reaped and the group is gone.
        let _ = child.wait();
        assert!(group_gone(socat_pid).await, "the socat group must be dead");
        let _ = stop.send(());
        let _ = task.await;
    }

    /// Oracle forward_handover_test.go:46-56: a socat that did not survive
    /// the handover (kill -0 fails) is left out so the next scan respawns.
    #[tokio::test]
    async fn import_skips_a_dead_socat() {
        let logger = test_logger();
        let scanner = test_scanner(&logger);
        let forwarder = Forwarder::build(&logger, &scanner, noop_freezer(), Ok(()));
        let readopted = forwarder
            .import_forwards(vec![ForwardedPort {
                key: "1-1".into(),
                port: 1,
                listener_pid: 1,
                family: 4,
                // A pid that is overwhelmingly unlikely to exist.
                socat_pid: 2_147_483_646,
            }])
            .await;
        assert_eq!(readopted, 0);
        assert!(forwarder.tracked_keys().await.is_empty());
    }

    /// Oracle forward_handover_test.go:62-83: a live socat is re-adopted into
    /// the ports map as pid-only (no child handle) and reaped when it exits.
    #[tokio::test]
    async fn import_readopts_a_live_socat_as_pid_only() {
        let logger = test_logger();
        let scanner = test_scanner(&logger);
        let forwarder = Arc::new(Forwarder::build(&logger, &scanner, noop_freezer(), Ok(())));

        let mut command = std::process::Command::new("/bin/sleep");
        command.arg("30");
        // SAFETY: setpgid between fork and exec, scalars only.
        unsafe {
            command.pre_exec(|| {
                nix::unistd::setpgid(Pid::from_raw(0), Pid::from_raw(0)).map_err(io::Error::from)
            });
        }
        let mut child = command.spawn().unwrap();
        let pid = child.id() as i32;

        let readopted = forwarder
            .import_forwards(vec![ForwardedPort {
                key: "100-8080".into(),
                port: 8080,
                listener_pid: 100,
                family: 4,
                socat_pid: pid,
            }])
            .await;
        assert_eq!(readopted, 1);
        assert!(forwarder
            .tracked_keys()
            .await
            .contains(&"100-8080".to_owned()));

        // The reaper started for the re-adopted pid must reap the child once
        // it dies: kill the group, then the pid must become waitable-and-reaped
        // (no zombie lingering).
        kill(Pid::from_raw(-pid), Signal::SIGKILL).unwrap();
        for _ in 0..200 {
            // A reaped child reports ECHILD to a second waitpid; a zombie
            // would keep reporting "exited, unreaped" to kill -0's group probe
            // staying alive.
            // SAFETY: kill(2) over scalars.
            if unsafe { nix::libc::kill(pid, 0) } != 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // Give the reaper thread its turn, then assert no zombie remains:
        // waitpid from here must fail (already reaped), not report the child.
        tokio::time::sleep(Duration::from_millis(50)).await;
        match nix::sys::wait::waitpid(
            Pid::from_raw(pid),
            Some(nix::sys::wait::WaitPidFlag::WNOHANG),
        ) {
            Err(nix::errno::Errno::ECHILD) => {}
            other => panic!("expected the reaper to have reaped the child, got {other:?}"),
        }
        let _ = child.wait();
    }

    /// Oracle forward_frozen_test.go:46-95: a forward refused while the socat
    /// cgroup is frozen must not be recorded; once thawed the next scan must
    /// attempt the forward again.
    #[tokio::test]
    async fn a_port_refused_while_frozen_is_not_tracked_and_is_retried_after_thaw() {
        let logger = test_logger();
        let scanner = test_scanner(&logger);
        let root = tempfile::tempdir().unwrap().keep();
        let fake = Arc::new(crate::cgroups::fake::FakeCgroupfs::new(
            &root,
            &[("socats", ProcessType::Socat)],
        ));
        let freezer = Arc::new(WorkloadFreezer::new(fake.clone()));
        let spawner = RecordingSocat::sleeping();
        let forwarder = Arc::new(
            Forwarder::build(&logger, &scanner, freezer, Ok(())).with_spawner(spawner.clone()),
        );
        let subscriber = forwarder.subscriber().unwrap();
        let (stop, stop_rx) = oneshot::channel();
        let forwarder_for_task = Arc::clone(&forwarder);
        let task = tokio::spawn(async move { forwarder_for_task.start_forwarding(stop_rx).await });

        let socats_path = root.join("socats");
        std::fs::write(socats_path.join("cgroup.freeze"), "1\n").unwrap();
        std::fs::write(socats_path.join("cgroup.events"), "populated 1\nfrozen 1\n").unwrap();

        // Messages is unbuffered and the loop only receives again once it has
        // finished the previous scan, so the second signal returning means the
        // first one was processed (oracle forward_frozen_test.go:84-89).
        subscriber
            .signal(batch(vec![listen_row(4242, 65_000)]))
            .await;
        subscriber
            .signal(batch(vec![listen_row(4242, 65_000)]))
            .await;
        assert!(
            forwarder.tracked_keys().await.is_empty(),
            "a forward refused while the cgroup is frozen must not be recorded"
        );
        assert!(
            spawner.recorded().is_empty(),
            "a refused start must be attempted before any spawn"
        );

        // Thaw: clear both the requested freeze and the settled state.
        std::fs::write(socats_path.join("cgroup.freeze"), "0\n").unwrap();
        std::fs::write(socats_path.join("cgroup.events"), "populated 1\nfrozen 0\n").unwrap();

        subscriber
            .signal(batch(vec![listen_row(4242, 65_000)]))
            .await;
        subscriber
            .signal(batch(vec![listen_row(4242, 65_000)]))
            .await;
        assert!(
            forwarder
                .tracked_keys()
                .await
                .contains(&"4242-65000".to_owned()),
            "once the cgroup is thawed the next scan must attempt the forward again"
        );
        assert_eq!(spawner.recorded().len(), 1);

        // Cleanup: kill whatever the recording spawner started.
        for pid in forwarder.tracked_socat_pids().await {
            let _ = kill(Pid::from_raw(-pid), Signal::SIGKILL);
        }
        let _ = stop.send(());
        let _ = task.await;
    }

    /// Oracle forward.go:177-189 and 252-259: a start that FAILED is not
    /// retried — the entry stays in the map (the insert precedes the start),
    /// the port reads as forwarded and stays dark until its listener reopens,
    /// and the next scan spawns nothing new.
    #[tokio::test]
    async fn a_failed_spawn_stays_tracked_dark_and_is_not_retried() {
        let logger = test_logger();
        let scanner = test_scanner(&logger);
        let spawner = RecordingSocat::failing();
        let forwarder = Arc::new(
            Forwarder::build(&logger, &scanner, noop_freezer(), Ok(()))
                .with_spawner(spawner.clone()),
        );
        let subscriber = forwarder.subscriber().unwrap();
        let (stop, stop_rx) = oneshot::channel();
        let forwarder_for_task = Arc::clone(&forwarder);
        let task = tokio::spawn(async move { forwarder_for_task.start_forwarding(stop_rx).await });

        subscriber
            .signal(batch(vec![listen_row(4242, 65_001)]))
            .await;
        subscriber
            .signal(batch(vec![listen_row(4242, 65_001)]))
            .await;
        assert!(
            forwarder
                .tracked_keys()
                .await
                .contains(&"4242-65001".to_owned()),
            "the failed entry stays in the map (Go inserts before starting)"
        );
        assert!(
            forwarder.export_forwards().await.is_empty(),
            "a dark entry has no socat and is not exported"
        );
        assert_eq!(
            spawner.recorded().len(),
            1,
            "a failed start must not retry on the next scan"
        );

        let _ = stop.send(());
        let _ = task.await;
    }

    /// The full forward lifecycle against a real child standing in for socat:
    /// tracked with the exact argv, then killed BY PROCESS GROUP when the
    /// port disappears from the scan (forward.go:193-199, 277-305).
    #[tokio::test]
    async fn a_detected_port_spawns_socat_and_a_closed_port_kills_its_group() {
        let logger = test_logger();
        let scanner = test_scanner(&logger);
        let spawner = RecordingSocat::sleeping();
        let forwarder = Arc::new(
            Forwarder::build(&logger, &scanner, noop_freezer(), Ok(()))
                .with_spawner(spawner.clone()),
        );
        let subscriber = forwarder.subscriber().unwrap();
        let (stop, stop_rx) = oneshot::channel();
        let forwarder_for_task = Arc::clone(&forwarder);
        let task = tokio::spawn(async move { forwarder_for_task.start_forwarding(stop_rx).await });

        subscriber
            .signal(batch(vec![listen_row(4242, 65_002)]))
            .await;
        subscriber
            .signal(batch(vec![listen_row(4242, 65_002)]))
            .await;
        let key = "4242-65002".to_owned();
        assert!(forwarder.tracked_keys().await.contains(&key));
        let recorded = spawner.recorded();
        assert_eq!(recorded.len(), 1);
        assert_eq!(
            recorded[0].argv,
            vec![
                "-d".to_owned(),
                "-d".to_owned(),
                "-d".to_owned(),
                "TCP4-LISTEN:65002,bind=169.254.0.21,reuseaddr,fork".to_owned(),
                "TCP4:localhost:65002".to_owned(),
            ]
        );
        // The no-op cgroup manager hands out no cgroup fd, so nothing may be
        // placed into a cgroup (forward_cgroupfd_other.go).
        assert_eq!(recorded[0].cgroup_fd, None);

        let socat_pid = forwarder.tracked_socat_pids().await[0];
        assert!(socat_pid > 0);

        // The port disappears: the DELETE sweep kills the group.
        subscriber.signal(batch(vec![])).await;
        subscriber.signal(batch(vec![])).await;
        assert!(
            group_gone(socat_pid).await,
            "the stopped socat's process group must be killed"
        );

        let _ = stop.send(());
        let _ = task.await;
    }

    /// Oracle forward_test.go:15-42: StartForwarding stops when the scan
    /// channel closed (the defensive two-value receive).
    #[tokio::test]
    async fn start_forwarding_stops_when_the_channel_closes() {
        let logger = test_logger();
        let scanner = test_scanner(&logger);
        let forwarder = Arc::new(Forwarder::build(&logger, &scanner, noop_freezer(), Ok(())));
        let subscriber = forwarder.subscriber().unwrap();
        let (_stop, stop_rx) = oneshot::channel();
        let forwarder_for_task = Arc::clone(&forwarder);
        let task = tokio::spawn(async move { forwarder_for_task.start_forwarding(stop_rx).await });

        subscriber.close();

        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("StartForwarding did not stop after Messages was closed")
            .unwrap();
    }

    /// The Go ctx arm: the stop signal returns the loop too.
    #[tokio::test]
    async fn start_forwarding_stops_on_the_stop_signal() {
        let logger = test_logger();
        let scanner = test_scanner(&logger);
        let forwarder = Arc::new(Forwarder::build(&logger, &scanner, noop_freezer(), Ok(())));
        let (stop, stop_rx) = oneshot::channel();
        let forwarder_for_task = Arc::clone(&forwarder);
        let task = tokio::spawn(async move { forwarder_for_task.start_forwarding(stop_rx).await });

        let _ = stop.send(());

        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("StartForwarding did not stop after the stop signal")
            .unwrap();
    }

    /// End to end through the REAL /proc scan source: a real loopback
    /// listener on a high port is detected, and the spawn request carries its
    /// real port and listener pid. The spawn itself is recorded, not run —
    /// a real socat would bind the guest gateway IP, which is guest-only.
    #[tokio::test]
    async fn a_real_listener_is_detected_through_the_real_scan_source() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let logger = test_logger();
        let scanner = Arc::new(Scanner::new(&logger, Duration::from_millis(5)));
        let spawner = RecordingSocat::failing();
        let forwarder = Arc::new(
            Forwarder::build(&logger, &scanner, noop_freezer(), Ok(()))
                .with_spawner(spawner.clone()),
        );
        let scan_task = tokio::spawn({
            let scanner = scanner.clone();
            async move { scanner.scan_and_broadcast().await }
        });
        let (stop_fwd, fwd_rx) = oneshot::channel();
        let forwarder_for_task = Arc::clone(&forwarder);
        let fwd_task =
            tokio::spawn(async move { forwarder_for_task.start_forwarding(fwd_rx).await });

        let mut detected = false;
        for _ in 0..500 {
            if spawner.recorded().iter().any(|spawn| {
                spawn
                    .argv
                    .iter()
                    .any(|arg| arg.contains(&format!("LISTEN:{port},")))
            }) {
                detected = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(detected, "the real listener must reach the forwarder");
        assert!(
            forwarder.export_forwards().await.is_empty(),
            "spawned socats failed to start, so nothing may be exported"
        );

        scanner.destroy();
        let _ = tokio::time::timeout(Duration::from_secs(2), scan_task).await;
        let _ = stop_fwd.send(());
        let _ = tokio::time::timeout(Duration::from_secs(2), fwd_task).await;
    }
}
