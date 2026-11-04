//! The Windows daemon entry point: flag parsing, the boot guard, SCM
//! dispatch with its console fallback, the incoming handover adoption, and
//! the outgoing live-upgrade choreography.
//!
//! This lives in the library, not in `src/main.rs`, because the handover
//! sidecar seams (`adopt_listener`, `start_successor`, `write_ready`) are
//! crate-internal: the binary crate can only call public API. `main.rs`'s
//! Windows arm is a one-line call to [`run`].
//!
//! Launch modes (`docs/windows-port.md`):
//! - the SCM starts the installed `coorworker-envd` service → service mode,
//!   a guest by construction;
//! - a process the SCM did not start gets
//!   `ERROR_FAILED_SERVICE_CONTROLLER_CONNECT` from the dispatcher and falls
//!   back to console mode, which requires `COORWORKER_ENVD=1`;
//! - a successor spawned by an outgoing envd (`--resume-handover`) is a guest
//!   by construction too, so it skips the console marker;
//! - `--development` never talks to the SCM and must bind loopback.

use std::error::Error;
use std::ffi::OsString;
use std::future::Future;
use std::os::windows::io::{AsRawSocket as _, RawSocket};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::watch;
use windows_sys::Win32::Foundation::ERROR_FAILED_SERVICE_CONTROLLER_CONNECT;

use crate::cli::{parse_args, usage, Args, CliError, COMMIT, VERSION};
use crate::daemon::{
    app_with_execution_and_extensions, app_with_filesystem_and_extensions, Daemon,
};
use crate::exec_context::Defaults;
use crate::http1::{serve_gated, AcceptGate};
use crate::platform::port::scan::Scanner;
use crate::platform::windows::port::Forwarder;
use crate::platform::windows::process::WindowsProcessRuntime;
use crate::platform::windows::{
    account, boot, handover, log, resource::WindowsResourceBackend, service,
};
use crate::process_contract::RetainedExit;
use crate::upgrade::{self, HandoverOutcome, UpgradeRun};

/// Go cgroups.HandoverMaxWait (freeze.go:137-145): the strict pre-handover
/// freeze budget — the swap is refused over a workload that has not stopped.
const HANDOVER_MAX_WAIT: Duration = Duration::from_secs(2);

/// Go main.go:343 `portScannerInterval`: the forwarder rescans every second.
/// The Linux daemon wires the same value (`PORT_SCANNER_INTERVAL`).
const PORT_SCANNER_INTERVAL: Duration = Duration::from_secs(1);

/// `--resume-handover` is appended only by an outgoing envd, so its presence
/// is the handover marker the boot guard keys on.
pub fn run() -> Result<(), Box<dyn Error>> {
    // A console-less launch (an SCM service, or the successor it spawns) has
    // no valid standard handles, and Rust's `println!` panics on a failed
    // write. Point them at NUL before any diagnostic.
    service::ensure_std_handles();
    // reexec.Main (reexec.go:49-68): a process the outgoing daemon marked as
    // a helper must not fall through into the daemon.
    if let Ok(name) = std::env::var("E2B_REEXEC_HELPER") {
        eprintln!("reexec: started as helper {name:?} but no such helper is defined");
        std::process::exit(1);
    }
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(CliError::Help) => {
            eprint!("{}", usage());
            std::process::exit(0);
        }
        Err(CliError::Flag(message)) => {
            eprintln!("{message}");
            eprint!("{}", usage());
            std::process::exit(2);
        }
    };
    // Go main.go:162-172: version is checked before commit, and both are
    // checked only after the whole flag set parsed (a bad flag wins).
    if args.version {
        println!("{VERSION}");
        return Ok(());
    }
    if args.commit {
        println!("{COMMIT}");
        return Ok(());
    }
    // A development daemon is never a service and never a handover target:
    // its upgrade choreography stays unwired, so it can never freeze host
    // workload or replace itself.
    if args.development {
        return block_on_serve(args, boot::Mode::Development, None);
    }
    let service_args = args.clone();
    let dispatched = service::dispatch(Box::new(move |handle| {
        Box::pin(async move {
            if let Err(error) = serve(service_args, boot::Mode::Service, Some(handle)).await {
                eprintln!("{error}");
            }
        })
    }));
    match dispatched {
        Ok(()) => Ok(()),
        // Not started by the SCM: this is a console guest. A successor spawned
        // by an outgoing envd is a guest by construction; every other console
        // start needs the explicit VM boot opt-in.
        Err(ERROR_FAILED_SERVICE_CONTROLLER_CONNECT) => {
            let mode = if args.resume_handover {
                boot::Mode::Service
            } else {
                boot::Mode::Console
            };
            block_on_serve(args, mode, None)
        }
        Err(code) => {
            Err(format!("StartServiceCtrlDispatcherW failed with Win32 error {code}").into())
        }
    }
}

fn block_on_serve(
    args: Args,
    mode: boot::Mode,
    service: Option<service::ServiceHandle>,
) -> Result<(), Box<dyn Error>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(serve(args, mode, service))?;
    Ok(())
}

/// Serves the complete envd API until the shutdown signal fires.
///
/// On a handover boot the successor reconstructs the inherited listening
/// socket, reports 'S' (listener ready ⇒ the outgoing envd may stop
/// accepting), and reports 'R' only after every resume action and the upgrade
/// wiring are in place, immediately before it starts accepting.
async fn serve(
    args: Args,
    mode: boot::Mode,
    service: Option<service::ServiceHandle>,
) -> Result<(), String> {
    // The SCM service has no console, and the successor it spawns inherits
    // none; both report to the event log. A console-attached guest keeps
    // stderr.
    let event_log = service.is_some() || !log::console_attached();
    let sink = log::Sink::open(event_log, log::SOURCE_NAME);
    let mut args = args;
    // Service and console guests keep the oracle's guest bind default
    // (0.0.0.0:49983); only development uses the loopback-ephemeral default.
    if mode != boot::Mode::Development {
        args.guest = true;
    }
    let address = args.resolve_address().map_err(|error| fail(&sink, error))?;
    let sidecar_path = handover::sidecar_path();
    let sidecar = if args.resume_handover {
        handover::read_sidecar(&sidecar_path)
            .map_err(|error| fail(&sink, format!("read handover sidecar: {error}")))?
    } else {
        None
    };
    if let Err(error) = boot::validate(
        mode,
        address,
        boot::marker_from_env().as_deref(),
        account::is_system_account(),
    ) {
        // Tell the outgoing envd we will never serve, so it does not wait out
        // its readiness timeout before releasing the freeze.
        if let Some(sidecar) = &sidecar {
            let _ = handover::write_ready(sidecar.ready_pipe, b'F');
        }
        return Err(fail(&sink, error));
    }
    sink.info(&format!(
        "envd starting in {} mode on {address}",
        mode.as_str()
    ));

    let account =
        account::lookup(&account::current_user_name()).map_err(|error| fail(&sink, error))?;
    let mut defaults = Defaults {
        user: account.name.clone(),
        ..Default::default()
    };
    // Live-upgrade incoming side (Go main.go:226-257): the blob's exec context
    // is applied before the daemon is built, so the first request already sees
    // it. A read/decode/schema refusal is not fatal — there is no old binary
    // to fall back to, so the resume is reported as failed and the daemon
    // keeps serving to be torn down by the orchestrator.
    let mut incoming: Result<Option<crate::proto::upgrade::HandoverState>, upgrade::UpgradeError> =
        Ok(None);
    if args.resume_handover {
        incoming = upgrade::read_handover(&upgrade::handover_path());
        match &incoming {
            Ok(Some(state)) => {
                let (user_restored, workdir_restored) =
                    upgrade::apply_handover_defaults(state, &mut defaults);
                if user_restored || workdir_restored {
                    sink.info(&format!(
                        "envd: restored exec context from the handover blob \
                         (user={user_restored} workdir={workdir_restored})"
                    ));
                }
                sink.info(&format!(
                    "envd: resumed as v{VERSION} after handover (from v{}, {} procs)",
                    state.from_ver,
                    state.processes.len()
                ));
            }
            Ok(None) => sink.warn("envd: --resume-handover found no blob; resuming empty"),
            Err(error) => sink.error(&format!("resume-from-handover failed: {error}")),
        }
    }

    // The readiness-pipe handle of the sidecar this boot adopted ('S' was
    // already reported through it). `None` means a fresh bind.
    let ready_pipe: Option<u64>;
    let listener = match sidecar {
        Some(sidecar) => match handover::adopt_listener(&sidecar) {
            Ok(listener) => {
                // 'S': the socket is reconstructed in this process. The
                // outgoing envd may now close its accept gate. A dead pipe
                // means the outgoing envd is gone, so there is no handover to
                // complete.
                handover::write_ready(sidecar.ready_pipe, b'S').map_err(|error| {
                    fail(&sink, format!("handover: cannot report readiness: {error}"))
                })?;
                ready_pipe = Some(sidecar.ready_pipe);
                listener
            }
            Err(error) => {
                let _ = handover::write_ready(sidecar.ready_pipe, b'F');
                return Err(fail(
                    &sink,
                    format!("adopt the inherited listener: {error}"),
                ));
            }
        },
        None => {
            ready_pipe = None;
            bind_listener(address).map_err(|error| fail(&sink, error))?
        }
    };
    let local = listener
        .local_addr()
        .map_err(|error| fail(&sink, error.to_string()))?;

    let mut daemon = Daemon::new();
    // Go init.go:437 steps the system clock on /init; only a guest takes this
    // builder, so a development daemon can never touch the host clock. A
    // timestamped /init still answers its correction header with the step
    // skipped — the same end state as Go dev failing the step with a log line.
    if args.guest {
        daemon = daemon.with_set_system_time(Arc::new(crate::clock::system_time_sink));
    }
    if args.resume_handover {
        daemon = daemon
            .with_handover_auth()
            .await
            .with_initial_defaults(defaults.clone())
            .await;
    }
    let daemon = Arc::new(daemon);
    let runtime = match daemon.process_runtime() {
        Ok(runtime) => Some(runtime),
        Err(error) => {
            // The filesystem and HTTP surfaces still serve; the process
            // procedures answer the router's 404 unimplemented fallback.
            sink.error(&format!("process procedures stay unavailable: {error}"));
            None
        }
    };
    // The retention cache is the only live state a Windows handover carries
    // (upgrade.go:488-493): the sidecar's process table is empty by design.
    if let (Ok(Some(state)), Some(runtime)) = (&incoming, &runtime) {
        runtime
            .restore_retained(
                state
                    .terminated
                    .iter()
                    .map(RetainedExit::from_wire)
                    .collect(),
            )
            .await;
    }
    // A successful decode consumed the blob (upgrade.go:495); the sidecar is
    // consumed by the adoption, and the renamed predecessor image is removed
    // now that the old process is gone.
    if let Ok(Some(_)) = &incoming {
        let _ = std::fs::remove_file(upgrade::handover_path());
    }
    if ready_pipe.is_some() {
        handover::delete_sidecar(&sidecar_path);
    }
    handover::cleanup_stale_images(&upgrade::upgrade_bin_path());
    if args.resume_handover {
        // Surface the handover outcome on the next /init (main.go:305-311).
        let outcome = match &incoming {
            Ok(Some(state)) => HandoverOutcome::resumed(state, 0, 0),
            _ => HandoverOutcome {
                failed: true,
                ..Default::default()
            },
        };
        daemon.set_handover_result(outcome);
    }

    let (gate_send, gate) = AcceptGate::channel(true);
    // Guest modes wire the outgoing choreography; development never does, so
    // its /upgrade answers 500 "upgrade is not wired".
    if mode != boot::Mode::Development {
        if let Some(process_runtime) = runtime.clone() {
            let install = wire_upgrade(
                &daemon,
                process_runtime,
                gate_send.clone(),
                listener.as_raw_socket(),
            );
            daemon.set_upgrade_run(install);
        }
    }
    // Check boot identity as well as the actual session: an SCM successor can
    // lack a service handle and must still never initialize the desktop stack.
    let mut session = 0;
    // SAFETY: current PID is valid and session points to writable u32 storage.
    let session_known = unsafe {
        windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId(
            windows_sys::Win32::System::Threading::GetCurrentProcessId(),
            &mut session,
        )
    } != 0;
    // Resume uses Service as a boot-guard shortcut for console successors too.
    // SCM attachment identifies a service here; a detached service successor
    // stays excluded by its inherited Session 0 (or a failed session lookup).
    let context = crate::ext::cua::LaunchContext::for_windows(
        service.is_some(),
        session_known.then_some(session),
    );
    let cua = crate::ext::cua::CuaExtension::from_env_with_context(
        crate::http::ConfigurationState::configuration(daemon.as_ref()).clone(),
        context,
    )
    .await;
    let extensions = match cua {
        Some(extension) => {
            let extension: std::sync::Arc<dyn crate::ext::Extension> =
                std::sync::Arc::new(extension);
            crate::ext::Extensions::new(vec![extension]).unwrap_or_else(|error| {
                eprintln!("cua extension disabled: {error}");
                crate::ext::Extensions::empty()
            })
        }
        None => crate::ext::Extensions::empty(),
    };
    let app = match &runtime {
        Some(runtime) => {
            app_with_execution_and_extensions(daemon.clone(), runtime.clone(), &extensions)
        }
        None => app_with_filesystem_and_extensions(daemon.clone(), &extensions),
    };
    let shutdown: Pin<Box<dyn Future<Output = ()> + Send>> = match &service {
        Some(handle) => Box::pin(wait_for_stop(handle.subscribe())),
        // Console and development daemons install no signal handler (Go's
        // default disposition): serve until the process is terminated.
        None => Box::pin(std::future::pending()),
    };

    println!("envd listening {local}");
    if event_log {
        sink.info(&format!("envd listening {local}"));
    }
    // Go main.go:341-360: the scanner and the forwarder start before the
    // daemon accepts, so a guest serves its forwarded ports from the first
    // request. A development daemon reaches the oracle's "unavailable" path
    // and starts neither task.
    let port_wiring = wire_port_forwarding(&daemon, args.guest);
    if port_wiring.forwards() {
        sink.info("port forwarding enabled on 169.254.0.21");
    } else {
        sink.info("port forwarding disabled: this daemon is not a guest");
    }
    // 'R': every resume action is done and the upgrade hook is wired; start
    // accepting. The outgoing envd exits once it observes this byte.
    if let Some(ready_pipe) = ready_pipe {
        if let Err(error) = handover::write_ready(ready_pipe, b'R') {
            port_wiring.shutdown().await;
            return Err(fail(
                &sink,
                format!("handover: cannot report serving: {error}"),
            ));
        }
    }
    let served = serve_gated(listener, app, shutdown, gate).await;
    port_wiring.shutdown().await;
    served.map_err(|error| fail(&sink, error.to_string()))
}

/// The port scanner and forwarder, wired the way Go main.go:341-360 does:
/// one scanner over the Win32 TCP table, one in-process forwarder on the
/// daemon's workload gate, and both tasks started only when the forwarder
/// forwards. The Windows handover carries no forwards (`docs/windows-port.md`
/// §8.3), so nothing here survives a swap — the successor's own scan
/// re-establishes every proxy.
struct PortWiring {
    scanner: Arc<Scanner>,
    tasks: Option<PortTasks>,
}

struct PortTasks {
    stop_forwarding: tokio::sync::oneshot::Sender<()>,
    forwarding: tokio::task::JoinHandle<()>,
    scanning: tokio::task::JoinHandle<()>,
}

impl PortWiring {
    fn forwards(&self) -> bool {
        self.tasks.is_some()
    }

    /// Go's `defer portScanner.Destroy()` plus the ctx cancellation that ends
    /// StartForwarding: stop the scan, unpark any broadcast, stop every
    /// proxy, and wait for both tasks so nothing leaks.
    async fn shutdown(self) {
        self.scanner.destroy();
        if let Some(tasks) = self.tasks {
            let _ = tasks.stop_forwarding.send(());
            let _ = tasks.forwarding.await;
            let _ = tasks.scanning.await;
        }
    }
}

fn wire_port_forwarding(daemon: &Arc<Daemon>, guest: bool) -> PortWiring {
    // Windows keeps the daemon's own logging off (`Logger::none()`), the same
    // choice the filesystem and process adapters make.
    let logger = crate::logs::Logger::none();
    let scanner = Arc::new(Scanner::new(&logger, PORT_SCANNER_INTERVAL));
    let forwarder = Arc::new(Forwarder::new(
        &logger,
        &scanner,
        daemon.resource().registry().clone(),
        guest,
    ));
    let tasks = if forwarder.forwards() {
        let (stop_forwarding, stop_receiver) = tokio::sync::oneshot::channel();
        let forwarding = tokio::spawn(async move {
            forwarder.start_forwarding(stop_receiver).await;
        });
        let scanning = tokio::spawn({
            let scanner = Arc::clone(&scanner);
            async move { scanner.scan_and_broadcast().await }
        });
        Some(PortTasks {
            stop_forwarding,
            forwarding,
            scanning,
        })
    } else {
        None
    };
    PortWiring { scanner, tasks }
}

/// Builds the daemon's outgoing upgrade hook. It captures the raw listening
/// socket (valid while `serve_gated` holds the listener), the shared resource
/// backend, the process runtime, and the accept gate.
fn wire_upgrade(
    daemon: &Arc<Daemon>,
    runtime: Arc<WindowsProcessRuntime>,
    gate: watch::Sender<bool>,
    socket: RawSocket,
) -> UpgradeRun {
    let resource: Arc<WindowsResourceBackend> = daemon.resource().clone();
    let daemon = daemon.clone();
    Arc::new(move |new_bin: Option<String>| {
        let resource = resource.clone();
        let daemon = daemon.clone();
        let runtime = runtime.clone();
        let gate = gate.clone();
        Box::pin(
            async move { do_upgrade(&new_bin, &resource, &daemon, &runtime, &gate, socket).await },
        )
    })
}

/// Go's doUpgrade closure body (main.go:369-420): freeze the workload and
/// hold it, export the state that crosses the swap, write the pinned blob,
/// spawn the successor on the inherited socket and exit without an HTTP
/// response. Every failure path drops the freeze hold, thaws, re-opens the
/// accept gate and leaves the old envd serving.
async fn do_upgrade(
    new_bin: &Option<String>,
    resource: &Arc<WindowsResourceBackend>,
    daemon: &Arc<Daemon>,
    runtime: &Arc<WindowsProcessRuntime>,
    gate: &watch::Sender<bool>,
    socket: RawSocket,
) -> Result<(), String> {
    let target = upgrade::upgrade_bin_path();
    eprintln!(
        "envd: self-upgrade (from v{VERSION}, newBin={:?})",
        new_bin.as_deref().unwrap_or("")
    );
    // Upgrade's own gate (upgrade.go:122-124): only self (empty) or the fixed
    // delivered path — the endpoint's 400 is not the only check.
    if let Some(requested) = new_bin {
        if *requested != target.to_string_lossy() {
            return Err(format!(
                "refusing upgrade to unexpected binary {requested:?}"
            ));
        }
    }
    // Hold the freeze lock across the WHOLE handover, strict: a workload that
    // did not stop refuses the swap (process/upgrade.go:576-605).
    let hold = match resource.freeze_workload_hold(HANDOVER_MAX_WAIT).await {
        Ok(hold) => hold,
        Err(error) => {
            if let Err(error) = resource.unfreeze().await {
                eprintln!("handover: unfreeze failed: {error}");
            }
            return Err(error);
        }
    };
    let failure = match hand_over(new_bin, daemon, runtime, gate, socket).await {
        // The handover block never completes: success replaces the process.
        Ok(never) => match never {},
        Err(failure) => failure,
    };
    drop(hold);
    if let Err(error) = resource.unfreeze().await {
        eprintln!("handover: unfreeze failed: {error}");
    }
    Err(failure)
}

/// The export → blob → successor half. Success exits the process (or reports
/// the failure that kept it serving).
async fn hand_over(
    new_bin: &Option<String>,
    daemon: &Arc<Daemon>,
    runtime: &Arc<WindowsProcessRuntime>,
    gate: &watch::Sender<bool>,
    socket: RawSocket,
) -> Result<std::convert::Infallible, String> {
    let terminated = runtime.export_retained().await;
    let defaults = daemon.current_defaults().await;
    let state = crate::proto::upgrade::HandoverState {
        from_ver: VERSION.into(),
        terminated: terminated.iter().map(RetainedExit::to_wire).collect(),
        // The live exec context this envd was serving with (upgrade.go:141-149).
        defaults: upgrade::handover_defaults(&defaults).into(),
        // Declared deviations: live processes, watchers and mounts are not
        // carried on Windows. Each child dies with this process (per-process
        // kill-on-close Jobs), so there is nothing to re-adopt, and Windows
        // has no guest-frozen cgroups.
        ..Default::default()
    };
    let blob = upgrade::handover_path();
    if let Some(parent) = blob.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    upgrade::write_handover(&blob, &state).map_err(|error| error.to_string())?;
    let program = match new_bin {
        Some(path) => PathBuf::from(path),
        None => std::env::current_exe().map_err(|error| format!("resolve executable: {error}"))?,
    };
    // `start_successor` appends the resume flag itself (upgrade.go:285-288).
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let sidecar_path = handover::sidecar_path();
    // Writes the sidecar, resumes the successor and waits for 'S' (listener
    // reconstructed). On failure the successor is terminated and the sidecar
    // removed, so this process remains the only listener.
    let mut successor = handover::start_successor(socket, &program, &args, &sidecar_path)?;
    // Stop accepting new connections on this listener. Established
    // connections are untouched; the shared backlog holds new ones until the
    // successor starts accepting.
    let _ = gate.send(false);
    if let Err(error) = successor.wait_serving() {
        // The successor never reached serving: abort it (Drop terminates an
        // unconfirmed child and removes the sidecar) and take the socket back.
        let _ = gate.send(true);
        return Err(error);
    }
    // The successor is serving on the duplicated socket. Tell the SCM this
    // service is stopping before the replacement process takes over (a
    // successor spawned this way is not SCM-registered; a declared
    // deviation).
    service::report_stopped_for_exit();
    std::process::exit(0)
}

fn bind_listener(address: std::net::SocketAddr) -> Result<TcpListener, String> {
    let listener = std::net::TcpListener::bind(address).map_err(|error| error.to_string())?;
    listener
        .set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    TcpListener::from_std(listener).map_err(|error| error.to_string())
}

/// Resolves when the SCM asks the service to stop.
async fn wait_for_stop(mut stopping: watch::Receiver<bool>) {
    if *stopping.borrow() {
        return;
    }
    let _ = stopping.changed().await;
}

/// Logs a fatal startup error on both sinks and returns it unchanged.
fn fail(sink: &log::Sink, message: impl Into<String>) -> String {
    let message = message.into();
    sink.error(&message);
    message
}
