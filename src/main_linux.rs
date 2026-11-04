//! The envd entrypoint: flag parsing, the development and guest boot paths,
//! and the live-upgrade choreography at both ends — the incoming
//! `--resume-handover` adoption and the outgoing `doUpgrade` closure the
//! /upgrade endpoint runs (Go main.go run()).
use coorworker_envd::cli::{parse_args, usage, Args, CliError, COMMIT, VERSION};
use coorworker_envd::{
    daemon::{app_with_execution, Daemon},
    exec_context::{Account, Defaults, Environment},
    logs::{HttpExporter, LogSink, Logger, ProcessLogger, StdoutSink},
    mmds::HyperTransport,
    process::ProcessRuntime,
};
use std::{net::SocketAddr, sync::Arc};

/// Bind like Go's `net.Listen("tcp", "0.0.0.0:<port>")`: `SO_REUSEADDR`,
/// non-blocking, and a listen backlog of `somaxconn` (Go's
/// `maxListenerBacklog`, clamped at 65535). std's default backlog is 128.
fn bind_go_listener(address: SocketAddr) -> std::io::Result<tokio::net::TcpListener> {
    let socket = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )?;
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&address.into())?;
    let backlog = std::fs::read_to_string("/proc/sys/net/core/somaxconn")
        .ok()
        .and_then(|value| value.trim().parse::<i32>().ok())
        .unwrap_or(4096)
        .clamp(1, 65535);
    socket.listen(backlog)?;
    tokio::net::TcpListener::from_std(socket.into())
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().nth(1).as_deref() == Some("cpu-online") {
        let cmdline = std::fs::read_to_string("/proc/cmdline")?;
        let init_executable = std::fs::read_link("/proc/1/exe")?;
        let helper_executable = std::fs::read_link("/proc/self/exe")?;
        coorworker_envd::guest::validate_cpu_helper_boot(
            nix::unistd::geteuid().as_raw(),
            &cmdline,
            &init_executable,
            &helper_executable,
        )?;
        coorworker_envd::cpus::write_online_file(&std::env::args().skip(2).collect::<Vec<_>>())?;
        return Ok(());
    }
    // reexec.Main (reexec.go:49-68): a process the outgoing daemon marked as a
    // helper must not fall through into the daemon when argv[1] names no
    // registered helper, or a broken Exec would run the program again.
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
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(serve(args))
}

async fn serve(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let address = args.resolve_address().map_err(std::io::Error::other)?;
    let Args {
        development,
        guest,
        is_not_fc,
        no_cgroups,
        cgroup_root,
        verbose,
        resume_handover,
        ..
    } = args;
    if guest && !development {
        let cmdline = std::fs::read_to_string("/proc/cmdline")?;
        coorworker_envd::guest::validate_guest_boot(
            nix::unistd::geteuid().as_raw(),
            std::process::id(),
            &cmdline,
        )?;
    } else if !development
        || guest
        || nix::unistd::geteuid().is_root()
        || !address.ip().is_loopback()
    {
        return Err("development mode requires an ordinary user and loopback address; guest mode requires explicit VM boot opt-in".into());
    }
    let user =
        nix::unistd::User::from_uid(nix::unistd::geteuid())?.ok_or("current user not found")?;
    let mut defaults = Defaults {
        user: user.name.clone(),
        ..Default::default()
    };
    // Live-upgrade incoming side (Go main.go:226-257): if this envd was
    // re-exec'd by an outgoing one, the handover blob carries the exec context
    // the outgoing envd served with. The read and the defaults restore happen
    // before the process runtime and daemon are built, the way Go's shared
    // *execcontext.Defaults is mutated by ResumeFromHandover before the
    // services that hold it serve a request (upgrade.go:396-405); everything
    // that acts on the rest of the blob — watchers, mounts, forwards, the
    // retained exits, the freeze — runs once those services exist, in Go's
    // order. A read/decode/schema refusal is NOT fatal here (main.go:250-256):
    // the old binary is gone post-execve, so the resume is reported as failed
    // and the envd keeps serving to be torn down by the orchestrator.
    let mut incoming_handover: Result<
        Option<coorworker_envd::proto::upgrade::HandoverState>,
        coorworker_envd::upgrade::UpgradeError,
    > = Ok(None);
    if resume_handover {
        incoming_handover = coorworker_envd::upgrade::read_handover(std::path::Path::new(
            coorworker_envd::upgrade::HANDOVER_PATH,
        ));
        if let Ok(Some(state)) = &incoming_handover {
            let (user_restored, workdir_restored) =
                coorworker_envd::upgrade::apply_handover_defaults(state, &mut defaults);
            if user_restored || workdir_restored {
                // Name the fields actually applied (upgrade.go:403-405): a
                // message that always says "default user" would report the
                // compiled-in fallback as a restored identity on a
                // workdir-only blob.
                eprintln!(
                    "envd: restored exec context from the handover blob (user={user_restored} workdir={workdir_restored})"
                );
            }
            // journald-visible proof of the running image after the swap
            // (upgrade.go:407-409).
            eprintln!(
                "envd: resumed as v0.10.2 after handover (from v{}, {} procs)",
                state.from_ver,
                state.processes.len()
            );
        }
    }
    let account = Account {
        name: user.name,
        home: user.dir.to_string_lossy().into(),
        uid: user.uid.as_raw(),
        gid: user.gid.as_raw(),
        groups: nix::unistd::getgroups()?
            .iter()
            .map(|gid| gid.as_raw())
            .collect(),
    };
    let runtime = ProcessRuntime::new(
        account.clone(),
        defaults.clone(),
        Environment::default(),
        std::env::var("PATH").ok(),
    )?;
    let listener = bind_go_listener(address)?;
    println!("envd listening {}", listener.local_addr()?);
    let run_dir = std::path::PathBuf::from("/run/e2b");
    if guest {
        coorworker_envd::mmds::poll::initialize_run_dir(&run_dir, !is_not_fc);
    }
    // Go logs.NewLogger (main.go:203-210): the exporter is appended when the
    // log exporter is enabled and os.Stdout follows with --verbose. Built
    // before the MMDS clients so the pin-failure warn (init.go:92) reaches
    // the same sinks as Go's a.logger.
    let exporter =
        exporter_enabled(guest, is_not_fc).then(|| HttpExporter::new(Arc::new(HyperTransport)));
    let mut sinks: Vec<Arc<dyn LogSink>> = Vec::new();
    if let Some(exporter) = &exporter {
        sinks.push(exporter.clone());
    }
    if verbose {
        // Go logs.NewLogger appends os.Stdout after the exporters.
        sinks.push(Arc::new(StdoutSink));
    }
    let base_logger = Logger::new(sinks);
    let env_logger = base_logger.with_str("logger", "envd");
    let filesystem_logger = base_logger.with_str("logger", "filesystem");
    let process_logger = base_logger.with_str("logger", "process");
    let metadata_client = if guest {
        Some(Arc::new(
            coorworker_envd::mmds::MmdsClient::new(
                Arc::new(coorworker_envd::mmds::HyperTransport),
                Arc::new(coorworker_envd::mmds::IptablesRoute::new(Arc::new(
                    coorworker_envd::mmds::ProcessRunner::default(),
                ))),
            )
            .with_pin_logger(env_logger.clone()),
        ))
    } else {
        None
    };
    let (oom_stop, oom_task, cpu_stop, cpu_task, daemon, children) = if guest && !is_not_fc {
        let children = Arc::new(coorworker_envd::cpus::ChildRegistry::default());
        // Go main.go:259-261: the handover's helper pids join the reaper
        // registry BEFORE the cpu manager is constructed, so a helper that
        // exited during the swap is waited on from adoption. Only a
        // successfully decoded blob carries helpers — a failed resume has
        // none, the way Go's zero HandoverResult has none.
        if let Ok(Some(state)) = &incoming_handover {
            for pid in &state.helper_pids {
                if let Err(error) = children.adopt(*pid) {
                    eprintln!("handover: adopt helper pid {pid}: {error}");
                }
            }
        }
        let sysfs = Arc::new(coorworker_envd::cpus::KernelSysfs::new(
            "/sys/devices/system/cpu",
            children.clone(),
        ));
        let cpus = Arc::new(coorworker_envd::cpus::CpuManager::new(
            sysfs,
            children.clone(),
            "/run/e2b/cpu-target".into(),
        ));
        let watcher = Arc::new(coorworker_envd::oom::OomWatcher::new());
        let route = Arc::new(coorworker_envd::mmds::IptablesRoute::new(Arc::new(
            coorworker_envd::mmds::ProcessRunner::default(),
        )));
        let provider = Arc::new(
            coorworker_envd::mmds::MmdsClient::new(
                Arc::new(coorworker_envd::mmds::HyperTransport),
                route,
            )
            .with_pin_logger(env_logger.clone()),
        );
        let daemon = Daemon::with_guest_services(cpus.clone(), watcher.clone(), provider);
        let (stop, receiver) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move { watcher.run(receiver).await });
        let (cpu_stop, receiver) = tokio::sync::oneshot::channel();
        let cpu_task = tokio::spawn(async move { cpus.run(receiver).await });
        (
            Some(stop),
            Some(task),
            Some(cpu_stop),
            Some(cpu_task),
            daemon,
            // Kept for the outgoing handover's helpers.ExportHold() (main.go:391).
            Some(children),
        )
    } else {
        (None, None, None, None, Daemon::new(), None)
    };
    let daemon = match metadata_client {
        Some(client) => {
            daemon
                .with_metadata_polling(client, run_dir, !is_not_fc)
                .await
        }
        None => daemon,
    };
    // Go main.go:196-210: the MMDS options channel (capacity 1) feeds the log
    // exporter, which only starts its send loop when the first options arrive.
    // Development never polls MMDS in this port, so it has no exporter either
    // (disclosed): the same end state as an exporter that never receives
    // options.
    let metadata_options = daemon.take_metadata_options_receiver();
    let runtime = runtime.with_process_logs(Arc::new(ProcessLogger::new(process_logger.clone())));
    let mut daemon = daemon
        .with_logger(env_logger)
        // Go main.go:215-216.
        .with_filesystem_logger(filesystem_logger)
        // Go main.go:243: the process RPC interceptor lines.
        .with_process_logger(process_logger.clone());
    if let Some(exporter) = &exporter {
        daemon = daemon.with_log_flusher(exporter.clone());
    }
    let log_tasks = match (exporter.as_ref(), metadata_options) {
        (Some(exporter), Some(options)) => Some(exporter.start(options)?),
        _ => None,
    };
    // Go main.go:218-243: one freezer shared by the process service and the
    // HTTP API, backed by the cgroup2 manager the guest booted with, and the
    // thaw watchdog backing up every freeze. Guests only: development never
    // WRITES the host cgroupfs (it only reads /proc/self/cgroup and
    // /sys/fs/cgroup to size X-Envd-Memory), mirroring the guest-only
    // clock/hosts/CA wiring below. --no-cgroups (Go main.go:493-501) keeps
    // the no-op manager without reading /sys/fs/cgroup at all. The same
    // freezer instance is the port forwarder's (main.go:229, 347).
    let freezer: Option<Arc<coorworker_envd::cgroups::freeze::WorkloadFreezer>> = if guest {
        let manager: Arc<dyn coorworker_envd::cgroups::Manager> =
            create_cgroup_manager(no_cgroups, &cgroup_root);
        let freezer = Arc::new(coorworker_envd::cgroups::freeze::WorkloadFreezer::new(
            manager,
        ));
        freezer.set_thaw_watchdog(
            coorworker_envd::cgroups::DEFAULT_THAW_WATCHDOG_WINDOW,
            Some(Arc::new(|result, error| {
                let mut message = format!(
                    "thaw watchdog fired: a freeze was never followed by a thaw: \
                     thawed={} visited={} failed={} truncated={}",
                    result.thawed, result.visited, result.failed, result.truncated
                );
                if let Some(error) = error {
                    message.push_str(&format!(" error={error}"));
                }
                eprintln!("{message}");
            })),
        );
        Some(freezer)
    } else {
        None
    };
    // The process service shares the same instance (Go main.go:243 passes the
    // one workloadFreezer to both processRpc.Handle and the API), so a /freeze
    // holds the spawn barrier for Start. A development daemon gets its default
    // no-op freezer, keeping the same refusal semantics without host writes.
    let (daemon, runtime) = coorworker_envd::daemon::share_freezer(daemon, runtime, &freezer);
    // Go init.go:1031 writes the production hosts file and init.go:437 steps
    // the system clock; only real guests take these builders so development
    // mode can never touch host /etc/hosts or the host clock. hyperloopIP on
    // a development daemon is then ignored entirely (no env), and a
    // timestamped /init still answers its correction header with the step
    // skipped -- the same end state as Go dev failing the write or the step
    // with a log line. The CA installer and its reconcile loop
    // (store.go:183-185 RunCACertCleanup, main.go:297) follow the same
    // guest-only wiring: development never reads or writes host trust stores.
    let mut ca_stop: Option<tokio::sync::watch::Sender<bool>> = None;
    let mut ca_task: Option<tokio::task::JoinHandle<()>> = None;
    let daemon = if guest {
        let installer = Arc::new(coorworker_envd::cacerts::CaCertInstaller::builder().build());
        let (stop, receiver) = tokio::sync::watch::channel(false);
        let worker = Arc::clone(&installer);
        ca_task = Some(tokio::spawn(async move { worker.run(receiver).await }));
        ca_stop = Some(stop);
        daemon
            .with_ca_certinstaller(installer)
            .with_hyperloop_hosts_path(std::path::PathBuf::from("/etc/hosts"))
            .with_set_system_time(Arc::new(coorworker_envd::clock::system_time_sink))
    } else {
        daemon
    };
    // A handover boot serves from the pre-init gate (auth.go:59-77) with the
    // restored exec context already installed — Go's shared *Defaults carries
    // the resume's applications into api.New (main.go:296, upgrade.go:396-405).
    let daemon = if resume_handover {
        daemon
            .with_handover_auth()
            .await
            .with_initial_defaults(defaults.clone())
            .await
    } else {
        daemon
    };
    let daemon = Arc::new(daemon);
    // Go main.go:247-330: everything below runs only on a handover boot.
    if resume_handover {
        resume_incoming_handover(
            &incoming_handover,
            &daemon,
            &runtime,
            &account,
            &defaults,
            &process_logger,
            freezer.clone(),
            std::path::Path::new(coorworker_envd::upgrade::HANDOVER_PATH),
            HANDOVER_FALLBACK_THAW_TIMEOUT,
        )
        .await;
    }
    // Go main.go:341-360: the port scanner and forwarder run whenever the
    // build can forward ports; the development daemon gets the oracle's own
    // "port forwarding disabled" path and starts no tasks. The resume's
    // carried forwards are re-adopted before the tasks start (main.go:347-354).
    let resume_forwards: Vec<coorworker_envd::port::ForwardedPort> = match &incoming_handover {
        Ok(Some(state)) => state
            .forwards
            .iter()
            .map(coorworker_envd::port::ForwardedPort::from_wire)
            .collect(),
        _ => Vec::new(),
    };
    let port_wiring =
        wire_port_forwarding(&base_logger, freezer.clone(), guest, &resume_forwards).await;
    // Live-upgrade outgoing trigger (Go main.go:369-420): guest-only here, a
    // disclosed deviation — Go has no development mode, and a development
    // daemon must never freeze host cgroups, write /run/e2b or execve. Its
    // /upgrade answers 500 "upgrade is not wired". Go wires doUpgrade
    // unconditionally, so every guest boot gets it regardless of --isnotfc:
    // the parity upgrade gate caught the earlier isnotfc exclusion (oracle
    // re-exec'd under the same flag while this daemon answered 500).
    if guest {
        // The guest always has the real freezer (built above); the daemon
        // default keeps the type total for the dev path.
        let upgrade_freezer = freezer.clone().unwrap_or_else(|| daemon.freezer());
        let run: coorworker_envd::upgrade::UpgradeRun = Arc::new({
            let freezer = upgrade_freezer;
            let forwarder = port_wiring.forwarder.clone();
            let daemon = daemon.clone();
            let runtime = runtime.clone();
            let children = children.clone();
            move |new_bin: Option<String>| {
                let freezer = freezer.clone();
                let forwarder = forwarder.clone();
                let daemon = daemon.clone();
                let runtime = runtime.clone();
                let children = children.clone();
                Box::pin(async move {
                    do_upgrade(&new_bin, &freezer, &forwarder, &daemon, &runtime, &children).await
                })
            }
        });
        daemon.set_upgrade_run(run);
    }
    let result = coorworker_envd::http1::serve(
        listener,
        app_with_execution(daemon.clone(), runtime),
        // Go envd installs no signal handler: SIGINT/SIGTERM keep the default
        // disposition and terminate the process immediately. A pending
        // shutdown future preserves that (no graceful drain Go does not do).
        std::future::pending(),
    )
    .await;
    daemon.shutdown_metadata().await?;
    port_wiring.shutdown().await;
    if let Some(tasks) = log_tasks {
        tasks.shutdown().await;
    }
    if let Some(stop) = oom_stop {
        let _ = stop.send(());
    }
    if let Some(stop) = cpu_stop {
        let _ = stop.send(());
    }
    if let Some(stop) = ca_stop {
        let _ = stop.send(true);
    }
    let oom_result = match oom_task {
        Some(task) => task.await,
        None => Ok(()),
    };
    let cpu_result = match cpu_task {
        Some(task) => task.await,
        None => Ok(()),
    };
    let ca_result = match ca_task {
        Some(task) => task.await,
        None => Ok(()),
    };
    oom_result?;
    cpu_result?;
    ca_result?;
    result?;
    Ok(())
}

/// Go main.go:196-210 creates the exporter only when `!isNotFC`. This port
/// additionally requires guest mode because only the guest path polls MMDS at
/// all; without options the exporter would stay inert forever.
fn exporter_enabled(guest: bool, is_not_fc: bool) -> bool {
    guest && !is_not_fc
}

/// The incoming side of the live-upgrade handover, on the already-built
/// services — Go main.go:247-330's `if resumeHandover` block plus the body of
/// processService.ResumeFromHandover (upgrade.go:325-535) that is not bound to
/// the process runtime. The blob was read and its defaults applied before the
/// services existed; everything that ACTS on it happens here, in Go's order:
/// retained exits, blob removal, watcher re-arm while still frozen, the
/// guest-frozen record, the freeze re-arm, then the api-side installs (mount
/// ledger, handover outcome, fallback thaw). On any failure the workload is
/// thawed (upgrade.go:330-338) — a degraded-but-running workload beats a hung
/// one.
///
/// `blob_path` and `fallback_thaw_timeout` are the two knobs the service
/// carries as constants in Go (upgrade.go:28's HANDOVER_PATH and main.go:50's
/// 60s); production passes exactly those, tests point them at tempdirs and
/// short windows.
#[allow(
    clippy::too_many_arguments,
    // The input surface is Go's ResumeFromHandover: the read result, the four
    // services it acts on, the freezer, and the two service constants.
)]
async fn resume_incoming_handover(
    incoming: &Result<
        Option<coorworker_envd::proto::upgrade::HandoverState>,
        coorworker_envd::upgrade::UpgradeError,
    >,
    daemon: &Arc<Daemon>,
    runtime: &ProcessRuntime,
    account: &Account,
    defaults: &Defaults,
    logger: &Logger,
    freezer: Option<Arc<coorworker_envd::cgroups::freeze::WorkloadFreezer>>,
    blob_path: &std::path::Path,
    fallback_thaw_timeout: std::time::Duration,
) {
    // A development daemon carries the no-op freezer: the resume runs
    // uniformly, and the no-op manager means nothing below writes a host
    // cgroupfs (the same rule as every other dev-mode wiring in this file).
    let resume_freezer = freezer.clone().unwrap_or_else(|| daemon.freezer());
    let unfreeze = |freezer: Arc<coorworker_envd::cgroups::freeze::WorkloadFreezer>| async move {
        // processService.UnfreezeWorkload (upgrade.go:607-613): idempotent, and
        // its failure is logged, not fatal.
        if let Err(error) = freezer.unfreeze().await {
            eprintln!("handover: unfreeze failed: {error}");
        }
    };
    let outcome = match incoming {
        Err(error) => {
            eprintln!("resume-from-handover failed: {error}");
            unfreeze(resume_freezer.clone()).await;
            coorworker_envd::upgrade::HandoverOutcome {
                failed: true,
                ..Default::default()
            }
        }
        Ok(None) => {
            // No blob: a no-op resume (upgrade.go:341-344). The deferred
            // unfreeze still runs — with nothing frozen it is the no-op it is
            // designed to be.
            unfreeze(resume_freezer.clone()).await;
            coorworker_envd::upgrade::HandoverOutcome::default()
        }
        Ok(Some(state)) => {
            // Re-adopt the live children FIRST (upgrade.go:429-470 before
            // :488-493): the retention restore below skips a pid that came
            // back as a live process. The thaw watch is captured before the
            // adoption so each carried kill timer starts counting only when
            // the workload actually runs again (readopt.go's Thawed).
            let thawed = resume_freezer.thawed();
            let (adopted, adopted_failed) =
                runtime.readopt_processes(&state.processes, thawed).await;
            // Restore the retention cache (upgrade.go:488-493); restore_retained
            // skips a pid that came back as a live process (upgrade.go:543-545).
            runtime
                .restore_retained(
                    state
                        .terminated
                        .iter()
                        .map(coorworker_envd::upgrade::RetainedExit::from_wire)
                        .collect(),
                )
                .await;
            // The blob's job is done (upgrade.go:495); it stays behind on the
            // failure paths above exactly as Go's early returns leave it.
            let _ = std::fs::remove_file(blob_path);
            // Re-arm filesystem watchers while the workload is STILL frozen, so
            // no event is lost between the thaw and the re-arm
            // (upgrade.go:497-503); failures are counted, not fatal.
            let (rearmed, failed) =
                daemon
                    .watcher_registry()
                    .import_watchers(account, defaults, &state.watchers);
            // Loki-queryable summary (upgrade.go:506-518).
            logger
                .info()
                .str("event_type", "handover_resumed")
                .str("from_ver", &state.from_ver)
                .int("procs", state.processes.len() as i64)
                .int("procs_failed", adopted_failed as i64)
                .int("retained", state.terminated.len() as i64)
                .int("retained_failed", 0)
                .int("watchers", (rearmed + failed) as i64)
                .int("watchers_failed", failed as i64)
                .msg("re-adopted workload after envd self-upgrade");
            let adopted_total = state.processes.len() as i64;
            let adopted_failed_total = adopted_failed as i64;
            let _ = adopted;
            // Restore the guest-frozen record before any thaw, so a later
            // freeze adopts the guest's own cgroups and the /init thaw leaves
            // them frozen (upgrade.go:525-531). Empty restores empty — the safe
            // direction.
            resume_freezer.set_guest_frozen_paths(&state.guest_frozen_cgroups);
            // Tell the freezer it owns a live freeze again (upgrade.go:533-546):
            // neither the freeze-active state, nor the watchdog, nor the spawn
            // barrier crossed the execve. Logged, not fatal: failing the
            // handover here would trip the thaw and release a re-adopted
            // workload before /init restores auth.
            if let Err(error) = resume_freezer.resume_frozen().await {
                eprintln!(
                    "inherited a frozen workload without the spawn barrier: only the pre-spawn \
                     probe guards process starts until /init: {error}"
                );
            }
            coorworker_envd::upgrade::HandoverOutcome::resumed_with_failures(
                adopted_total,
                adopted_failed_total,
                state,
                (rearmed + failed) as i64,
                failed as i64,
            )
        }
    };
    // Restore the NFS mount ledger before the post-upgrade /init runs its
    // setupNFS (main.go:299-302); a failed resume carries none.
    let mounts: Vec<coorworker_envd::proto::upgrade::MountEntry> = match incoming {
        Ok(Some(state)) => state.mounts.clone(),
        _ => Vec::new(),
    };
    daemon.import_mounts(&mounts);
    // Surface the handover outcome on the next /init (main.go:305-311): the
    // envd-side result is otherwise invisible fleet-wide.
    daemon.set_handover_result(outcome);
    // Safety net for the freeze-until-/init handover (main.go:313-330): thaw
    // after the grace window when the post-upgrade /init never lands. Gated on
    // Initialized() so it never races a legitimate later /freeze.
    let fallback_daemon = daemon.clone();
    tokio::spawn(async move {
        let freezer = match freezer {
            Some(freezer) => freezer,
            None => fallback_daemon.freezer(),
        };
        tokio::time::sleep(fallback_thaw_timeout).await;
        if !fallback_daemon.initialized().await {
            eprintln!(
                "envd: post-upgrade /init did not arrive within {fallback_thaw_timeout:?}; \
                 thawing workload as fallback"
            );
            if let Err(error) = freezer.unfreeze().await {
                eprintln!("handover: unfreeze failed: {error}");
            }
        }
    });
}

/// Go's doUpgrade closure body (main.go:369-420 plus Upgrade's assembly,
/// upgrade.go:120-296). Freeze the workload and hold it, export every
/// service's state under locks that cross the execve, write the handover blob
/// and re-exec self with `--resume-handover` appended. On success this never
/// returns (the process image is replaced); every other exit releases the
/// freeze hold and thaws (main.go:377-383), leaving the old envd serving.
///
/// Disclosed: the live-child fd carry (upgrade.go:156-250, dup3 onto fdBase
/// 200) has no equivalent here — the Rust runtime owns its children's pipes —
/// so the carried process table is always empty and only the retention cache,
/// watchers, mounts, forwards, helpers, guest-frozen cgroups and defaults
/// cross the swap.
async fn do_upgrade(
    new_bin: &Option<String>,
    freezer: &Arc<coorworker_envd::cgroups::freeze::WorkloadFreezer>,
    forwarder: &Arc<coorworker_envd::port::Forwarder>,
    daemon: &Arc<Daemon>,
    runtime: &ProcessRuntime,
    children: &Option<Arc<coorworker_envd::cpus::ChildRegistry>>,
) -> Result<(), String> {
    eprintln!(
        "envd: self-upgrade (from v0.10.2, newBin={:?})",
        new_bin.as_deref().unwrap_or("")
    );
    // Upgrade's own gate (upgrade.go:122-124): only self (empty) or the fixed
    // delivered path — the endpoint's 400 is not the only check.
    if let Some(target) = new_bin {
        if target != coorworker_envd::upgrade::DEFAULT_UPGRADE_BIN_PATH {
            return Err(format!("refusing upgrade to unexpected binary {target:?}"));
        }
    }
    // Hold the freeze lock across the WHOLE handover (main.go:373-375), strict:
    // a workload that did not stop refuses the swap
    // (process/upgrade.go:576-605). Go passes context.Background() so the
    // acquire cannot be cancelled out from under the handover (main.go:375).
    let hold = match freezer.freeze_workload_hold(HANDOVER_MAX_WAIT).await {
        Ok(hold) => hold,
        Err(error) => {
            // The hold was released on refusal; the deferred
            // release+thaw (main.go:377-383) runs on this path too, and
            // thawing a not-frozen workload is the no-op it is designed to be.
            if let Err(error) = freezer.unfreeze().await {
                eprintln!("handover: unfreeze failed: {error}");
            }
            return Err(error.to_string());
        }
    };
    // Export and exec. The holds (forwards, watchers, helpers, freeze) are
    // locals of this block: a successful execve replaces the process and they
    // vanish with it; any failure drops them, releases the freeze hold and
    // thaws, in Go's lock-before-thaw order (main.go:377-383).
    let attempted: Result<std::convert::Infallible, String> = async {
        // ExportForwardsHold first: its acquisition awaits, so it cannot sit
        // behind the std-locked watcher hold (which must never cross an
        // await). Go's export order (main.go:389-394) differs observably in
        // nothing: the locks are independent and the freeze already quiesced
        // their writers.
        let (forwards, _forwards_hold) = forwarder.export_forwards_hold().await;
        // The retention cache (upgrade.go:254-271), so a process that exited
        // shortly before the upgrade keeps its exit code retrievable.
        let terminated = runtime.export_retained().await;
        // The live exec context, read before the watcher hold is taken (the
        // std lock must never sit across an await).
        let defaults = daemon.current_defaults().await;
        // The process table (upgrade.go:218-250): dup3 each live child's
        // daemon-side pipe ends onto the fixed slot window with CLOEXEC
        // cleared. Concurrent forks are excluded for the rest of the window
        // by the per-subsystem holds this block already carries — the freeze
        // hold gates process starts, the forwards hold gates socat spawns,
        // the helpers hold gates cpu helpers — so no concurrent fork can
        // inherit the CLOEXEC-cleared descriptors the way Go's ForkLock
        // prevented. The guard closes the relocated slots on any failure
        // path (upgrade.go's deferred cleanup); on the exec path the process
        // image is replaced first and the guard never drops. This is the
        // block's last await: everything below, once the std-locked watcher
        // hold is taken, is synchronous through the execve.
        let (processes, _handover_fds) = runtime
            .export_processes()
            .await
            .map_err(|error| format!("relocate handover fds: {error}"))?;
        // ExportWatchersHold: the snapshot with the registry lock KEPT, so a
        // concurrent GetWatcherEvents cannot drain events the snapshot already
        // carried (watch_handover.go:49-57). No await may sit under this std
        // lock — and none does from here to the execve.
        let (watchers, _watchers_hold) = daemon
            .watcher_registry()
            .export_watchers_hold()
            .map_err(|error| error.to_string())?;
        // ExportMounts (api/mounts_handover.go).
        let mounts = daemon.export_mounts();
        // helpers.ExportHold (main.go:391-393): the helper registry under its
        // export lock, so a cpu helper cannot start into the handover window.
        let helper_pids = children
            .as_ref()
            .map(|children| children.export_hold().pids().to_vec())
            .unwrap_or_default();
        let state = coorworker_envd::proto::upgrade::HandoverState {
            from_ver: "0.10.2".into(),
            terminated: terminated.iter().map(|exit| exit.to_wire()).collect(),
            processes,
            watchers,
            mounts,
            forwards: forwards.iter().map(|port| port.to_wire()).collect(),
            helper_pids,
            // Which cgroups the GUEST had frozen before the sweep
            // (upgrade.go:136-140): read from the freezer here rather than
            // plumbed in by the caller.
            guest_frozen_cgroups: freezer.guest_frozen_paths(),
            // The exec context this envd was serving with
            // (upgrade.go:141-149) — the live record, exactly Go's shared
            // defaults pointer at this moment.
            defaults: coorworker_envd::upgrade::handover_defaults(&defaults).into(),
            ..Default::default()
        };
        coorworker_envd::upgrade::write_handover(
            std::path::Path::new(coorworker_envd::upgrade::HANDOVER_PATH),
            &state,
        )
        .map_err(|error| error.to_string())?;
        // argv: keep original flags, append --resume-handover
        // (upgrade.go:285-288).
        let program = match new_bin {
            Some(path) => std::path::PathBuf::from(path),
            None => {
                std::env::current_exe().map_err(|error| format!("resolve executable: {error}"))?
            }
        };
        let mut command = std::process::Command::new(&program);
        command
            .args(std::env::args().skip(1))
            .arg(coorworker_envd::upgrade::RESUME_HANDOVER_FLAG);
        use std::os::unix::process::CommandExt as _;
        // Exec only returns on failure (a corrupt/missing staged binary);
        // exec never "succeeds" as a call — the image is gone.
        let error = command.exec();
        Err(format!("execve {}: {error}", program.display()))
    }
    .await;
    let failure = match attempted {
        // The block never completes successfully — exec has no success return.
        Ok(never) => match never {},
        Err(failure) => failure,
    };
    drop(hold);
    // processService.UnfreezeWorkload (upgrade.go:607-613) on every non-exec
    // exit, after the lock drop.
    if let Err(error) = freezer.unfreeze().await {
        eprintln!("handover: unfreeze failed: {error}");
    }
    Err(failure)
}

/// Go main.go:44: the port scanner period.
const PORT_SCANNER_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Go cgroups.HandoverMaxWait (freeze.go:137-145): the strict pre-handover
/// freeze budget — the swap is refused over a workload that has not stopped.
const HANDOVER_MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Go main.go:46-50: the fallback thaw bounds how long a live-upgraded envd
/// keeps its re-adopted workload frozen waiting for the post-upgrade /init.
const HANDOVER_FALLBACK_THAW_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// The running port-forwarding tasks and their stop handles.
struct PortTasks {
    stop_forwarding: tokio::sync::oneshot::Sender<()>,
    forwarding: tokio::task::JoinHandle<()>,
    scanning: tokio::task::JoinHandle<()>,
}

/// The port scanner and forwarder wired the way Go main.go:341-360 does. The
/// forwarder Arc is what the outgoing handover's ExportForwardsHold runs
/// against (main.go:390).
struct PortWiring {
    scanner: Arc<coorworker_envd::port::Scanner>,
    forwarder: Arc<coorworker_envd::port::Forwarder>,
    tasks: Option<PortTasks>,
}

impl PortWiring {
    /// Go main.go:345 `defer portScanner.Destroy()` plus the ctx cancellation
    /// that ends StartForwarding: stop the scan, unpark any broadcast, end the
    /// forwarder, and wait for both tasks so nothing leaks.
    async fn shutdown(self) {
        self.scanner.destroy();
        if let Some(tasks) = self.tasks {
            let _ = tasks.stop_forwarding.send(());
            let _ = tasks.forwarding.await;
            let _ = tasks.scanning.await;
        }
    }
}

/// Go main.go:341-360: one scanner, one forwarder on the shared freezer, and
/// both tasks started only when the forwarder forwards. A development daemon
/// reaches the oracle's own "unavailable" path (`forwarding_available(false)`,
/// see coorworker_envd::port): the forwarder warns, subscribes to nothing and
/// Forwards() is false, so neither task runs — a socat bound to the guest
/// gateway IP must never be spawned against a host. The resume's carried
/// forwards are re-adopted BEFORE the tasks start (main.go:347-354), so the
/// first scan recognizes already-forwarded ports instead of spawning
/// duplicate socats and leaking the originals as un-reaped zombies.
async fn wire_port_forwarding(
    base_logger: &coorworker_envd::logs::Logger,
    freezer: Option<Arc<coorworker_envd::cgroups::freeze::WorkloadFreezer>>,
    guest: bool,
    resume_forwards: &[coorworker_envd::port::ForwardedPort],
) -> PortWiring {
    let port_logger = base_logger.with_str("logger", "port-forwarder");
    let scanner = Arc::new(coorworker_envd::port::Scanner::new(
        &port_logger,
        PORT_SCANNER_INTERVAL,
    ));
    // Bind all open ports on 127.0.0.1 and localhost to the eth0 interface
    // (main.go:343). The freezer is the one shared instance (main.go:229);
    // a dev daemon's forwarder is disabled before the freezer is ever read.
    let freezer = freezer.unwrap_or_else(|| {
        Arc::new(coorworker_envd::cgroups::freeze::WorkloadFreezer::new(
            Arc::new(coorworker_envd::cgroups::noop::NoopManager),
        ))
    });
    let forwarder = Arc::new(coorworker_envd::port::Forwarder::new(
        &port_logger,
        &scanner,
        freezer,
        guest,
    ));
    if !resume_forwards.is_empty() {
        // main.go:347-354.
        let readopted = forwarder.import_forwards(resume_forwards.to_vec()).await;
        if readopted > 0 {
            eprintln!("envd: re-adopted {readopted} forwarded port(s) after handover");
        }
    }
    let tasks = if forwarder.forwards() {
        let (stop_forwarding, stop_receiver) = tokio::sync::oneshot::channel();
        let forwarder_for_task = Arc::clone(&forwarder);
        let forwarding = tokio::spawn(async move {
            forwarder_for_task.start_forwarding(stop_receiver).await;
        });
        let scanner_for_task = Arc::clone(&scanner);
        let scanning = tokio::spawn(async move {
            scanner_for_task.scan_and_broadcast().await;
        });
        Some(PortTasks {
            stop_forwarding,
            forwarding,
            scanning,
        })
    } else {
        None
    };
    PortWiring {
        scanner,
        forwarder,
        tasks,
    }
}

/// Go main.go:485-545 createCgroupManager: try the real cgroup2 manager with
/// the three process cgroups and the memory sizing the oracle uses; any
/// failure falls back to the no-op manager with a stderr note. cgroup_root is
/// the --cgroup-root flag value: appended as the manager root only when
/// non-empty (main.go:534-536), so "" keeps the cgroup2.go:59-61 default.
fn create_cgroup_manager(
    no_cgroups: bool,
    cgroup_root: &str,
) -> Arc<dyn coorworker_envd::cgroups::Manager> {
    use coorworker_envd::cgroups::{cgroup2, noop::NoopManager, ProcessType};
    if no_cgroups {
        eprintln!("cgroups disabled via --no-cgroups; using no-op cgroup manager");
        return Arc::new(NoopManager);
    }
    const MEGABYTE: u64 = 1024 * 1024;
    let Some(mem_total) = mem_total_bytes() else {
        eprintln!("failed to calculate host metrics; using no-op cgroup manager");
        return Arc::new(NoopManager);
    };
    // Try to keep 1/8 of the memory free, but no more than 128 MB; memory.high
    // equal to memory.max means OOM-kill immediately when throttling cannot
    // reclaim enough (main.go:508-513).
    let max_memory_reserved = (mem_total / 8).min(128 * MEGABYTE);
    let memory_max = mem_total - max_memory_reserved;
    let memory_high = memory_max;
    let entries = vec![
        cgroup2::Cgroup2Entry {
            process_type: ProcessType::Pty,
            relative_path: "ptys".into(),
            properties: vec![
                ("cpu.weight".into(), "200".into()),
                ("io.weight".into(), "default 50".into()),
                ("memory.high".into(), memory_high.to_string()),
                ("memory.max".into(), memory_max.to_string()),
            ],
        },
        cgroup2::Cgroup2Entry {
            process_type: ProcessType::Socat,
            relative_path: "socats".into(),
            properties: vec![
                ("cpu.weight".into(), "150".into()),
                ("io.weight".into(), "default 50".into()),
                ("memory.min".into(), (5 * MEGABYTE).to_string()),
                ("memory.low".into(), (8 * MEGABYTE).to_string()),
            ],
        },
        cgroup2::Cgroup2Entry {
            process_type: ProcessType::User,
            relative_path: "user".into(),
            properties: vec![
                ("memory.high".into(), memory_high.to_string()),
                ("memory.max".into(), memory_max.to_string()),
                ("cpu.weight".into(), "50".into()),
                ("io.weight".into(), "default 10".into()),
            ],
        },
    ];
    match cgroup2::Cgroup2Manager::new(cgroup2::Cgroup2Options {
        root: cgroup2_root(cgroup_root),
        entries,
    }) {
        Ok(manager) => Arc::new(manager),
        Err(error) => {
            eprintln!("failed to create cgroup2 manager: {error}");
            eprintln!("falling back to no-op cgroup manager");
            Arc::new(NoopManager)
        }
    }
}

/// Go main.go:534-536: the root option is appended only when the flag value
/// is non-empty, so an explicit `--cgroup-root=` keeps the manager's own
/// default (/sys/fs/cgroup, cgroup2.go:59-61).
fn cgroup2_root(cgroup_root: &str) -> Option<std::path::PathBuf> {
    (!cgroup_root.is_empty()).then(|| std::path::PathBuf::from(cgroup_root))
}

/// Go host.GetMetrics' MemTotal input, read from /proc/meminfo.
fn mem_total_bytes() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in meminfo.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            let kilobytes: u64 = rest.trim().trim_end_matches(" kB").trim().parse().ok()?;
            return Some(kilobytes * 1024);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Go main.go:44: the port scanner runs every second.
    #[test]
    fn port_scanner_interval_matches_the_oracle_constant() {
        assert_eq!(PORT_SCANNER_INTERVAL, std::time::Duration::from_secs(1));
    }

    /// Go main.go:534-536: only a non-empty --cgroup-root reaches the manager.
    #[test]
    fn cgroup_root_option_is_appended_only_when_set() {
        assert_eq!(cgroup2_root(""), None);
        assert_eq!(
            cgroup2_root("/tmp/cgroup2"),
            Some(std::path::PathBuf::from("/tmp/cgroup2"))
        );
    }

    /// Go host.GetMetrics reads MemTotal from /proc/meminfo and reports bytes
    /// (host/metrics.go), so the value must match the host's kB figure.
    #[test]
    fn mem_total_bytes_matches_proc_meminfo() {
        let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap();
        let expected: u64 = meminfo
            .lines()
            .find_map(|line| line.strip_prefix("MemTotal:"))
            .expect("MemTotal in /proc/meminfo")
            .trim()
            .trim_end_matches(" kB")
            .trim()
            .parse::<u64>()
            .unwrap()
            * 1024;
        assert_eq!(mem_total_bytes(), Some(expected));
        assert!(expected > 0);
    }

    /// Go main.go:344-360: a guest daemon wires the scanner and forwarder and
    /// starts both tasks (the build can forward, so Forwards() is true); a
    /// development daemon gets the oracle's own "unavailable" forwarder and
    /// starts nothing.
    #[tokio::test]
    async fn port_forwarding_tasks_follow_the_guest_gate() {
        let guest = wire_port_forwarding(&Logger::none(), None, true, &[]).await;
        assert!(guest.forwarder.forwards());
        let tasks = guest.tasks.expect("a guest forwarder must start its tasks");
        guest.scanner.destroy();
        let _ = tasks.stop_forwarding.send(());
        let _ = tasks.forwarding.await;
        let _ = tasks.scanning.await;

        let dev = wire_port_forwarding(&Logger::none(), None, false, &[]).await;
        assert!(!dev.forwarder.forwards());
        assert!(dev.tasks.is_none(), "a dev daemon must start no port tasks");
        dev.scanner.destroy();
    }

    /// Go main.go:347-354: the resume's carried forwards are re-adopted before
    /// the forwarder's first scan, so the tracked map already holds them (a
    /// live socat pid is re-adopted; a dead one is skipped so the scan
    /// respawns it). Driven against the development daemon's disabled
    /// forwarder: re-adoption is map bookkeeping and needs no guest.
    #[tokio::test]
    async fn resume_forwards_are_readopted_before_the_tasks_start() {
        let mut socat = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("test helper process");
        let pid = socat.id() as i32;
        let carried = coorworker_envd::port::ForwardedPort {
            key: format!("{pid}-8080"),
            port: 8080,
            listener_pid: pid,
            family: 4,
            socat_pid: pid,
        };
        let dead = coorworker_envd::port::ForwardedPort {
            key: "999999-9090".into(),
            port: 9090,
            listener_pid: 999_999,
            family: 4,
            socat_pid: 999_999,
        };
        let wiring = wire_port_forwarding(&Logger::none(), None, false, &[carried, dead]).await;
        assert!(wiring.tasks.is_none());
        let mut tracked = wiring.forwarder.tracked_keys().await;
        tracked.sort();
        assert_eq!(tracked, vec![format!("{pid}-8080")]);
        wiring.scanner.destroy();
        socat.kill().expect("kill test helper");
        let _ = socat.wait();
    }

    #[test]
    fn the_log_exporter_is_guest_only_and_skipped_for_isnotfc() {
        // Go main.go:199-210 gates the exporter on !isNotFC; this port also
        // needs the guest MMDS poll that supplies its options.
        assert!(exporter_enabled(true, false));
        assert!(!exporter_enabled(true, true));
        assert!(!exporter_enabled(false, false));
    }

    #[test]
    fn the_root_reaches_the_manager_options_like_gos_option_append() {
        assert_eq!(
            cgroup2_root("/host/dir"),
            Some(std::path::PathBuf::from("/host/dir")),
            "main.go:534-536"
        );
        // An empty value keeps the manager default instead of pointing it at "".
        assert_eq!(cgroup2_root(""), None);
    }

    #[test]
    fn no_cgroups_short_circuits_to_the_noop_manager() {
        // Go main.go:493-501: the opt-out returns before any /sys/fs/cgroup
        // access — no statfs, no mkdir, whatever the root flag says. The
        // no-op manager carries no path manager; the cgroup2 manager does.
        //
        // Proof-power note: with a nonexistent root this cannot by itself
        // distinguish the branches — statfs fails on it either way, so both
        // paths would fall back to the same no-op manager and this assert
        // would hold even without the short-circuit. The real semantics are
        // guaranteed by the guard's position (it returns before the manager
        // is ever built); the test pins the observable end state (no-op
        // manager, no path manager) that the guard produces.
        assert!(create_cgroup_manager(true, "/nonexistent-cgroupfs-root")
            .as_path_manager()
            .is_none());
    }
}

/// B1 (review round 1): direct tests of the incoming handover composition
/// (`resume_incoming_handover`) — the piecewise parts were tested, the
/// composition's landed behaviors were not. Oracle:
/// upgrade.go:330-338 (failure thaws), :341-344 (absent blob is a no-op),
/// :472-534 (the success order) and main.go:313-330 (the 60s fallback).
/// Guest-real operations stay fake: the freezer runs on the injected
/// tempdir cgroupfs tree (the same fake tests/upgrade_handover.rs uses), and
/// the blob lands in a tempfile.
#[cfg(test)]
mod handover_resume_tests {
    use super::*;
    use coorworker_envd::cgroups::{
        freeze::WorkloadFreezer, spawn::SpawnError, CgroupError, Manager, PathManager, ProcessType,
    };
    use coorworker_envd::proto::upgrade as wire;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    const INIT_PROCFILE: &str = "0::/system.slice/envd.service\n";
    /// A fallback window no test waits out: the arming is observable through
    /// a lingering task, never through this firing.
    const NEVER: std::time::Duration = std::time::Duration::from_secs(3_600);

    struct FakeCgroupfs {
        root: PathBuf,
        paths: HashMap<ProcessType, PathBuf>,
    }

    /// The fake kernel: a freeze request settles immediately for an idle
    /// tree — the write flips both the request and the frozen flag the events
    /// file reports, the way real cgroupfs does once the tasks stop.
    fn set_freeze_state(path: &Path, frozen: bool) {
        std::fs::write(
            path.join("cgroup.freeze"),
            if frozen { "1\n" } else { "0\n" },
        )
        .unwrap();
        std::fs::write(
            path.join("cgroup.events"),
            if frozen {
                "populated 1\nfrozen 1\n"
            } else {
                "populated 1\nfrozen 0\n"
            },
        )
        .unwrap();
    }

    impl FakeCgroupfs {
        fn new(root: &tempfile::TempDir) -> Self {
            let paths = [("user", ProcessType::User), ("pty", ProcessType::Pty)]
                .into_iter()
                .map(|(relative, process_type)| {
                    let path = root.path().join(relative);
                    std::fs::create_dir_all(&path).unwrap();
                    set_freeze_state(&path, false);
                    (process_type, path)
                })
                .collect();
            Self {
                root: root.path().to_owned(),
                paths,
            }
        }

        fn freeze_file(&self, process_type: ProcessType) -> PathBuf {
            self.paths[&process_type].join("cgroup.freeze")
        }
    }

    impl Manager for FakeCgroupfs {
        fn get_file_descriptor(&self, _process_type: ProcessType) -> Option<i32> {
            None
        }
        fn freeze(&self, process_type: ProcessType) -> Result<(), CgroupError> {
            set_freeze_state(&self.paths[&process_type], true);
            Ok(())
        }
        fn unfreeze(&self, process_type: ProcessType) -> Result<(), CgroupError> {
            set_freeze_state(&self.paths[&process_type], false);
            Ok(())
        }
        fn frozen(&self, process_type: ProcessType) -> Result<bool, CgroupError> {
            self.frozen_at(&self.paths[&process_type])
        }
        fn close(&self) -> Result<(), CgroupError> {
            Ok(())
        }
        fn as_path_manager(&self) -> Option<&dyn PathManager> {
            Some(self)
        }
    }

    impl PathManager for FakeCgroupfs {
        fn root(&self) -> PathBuf {
            self.root.clone()
        }
        fn path_of(&self, process_type: ProcessType) -> Option<PathBuf> {
            self.paths.get(&process_type).cloned()
        }
        fn children_of(&self, path: &Path) -> Result<Vec<PathBuf>, CgroupError> {
            let mut out = Vec::new();
            for entry in std::fs::read_dir(path)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    out.push(entry.path());
                }
            }
            Ok(out)
        }
        fn freeze_at(&self, path: &Path) -> Result<(), CgroupError> {
            set_freeze_state(path, true);
            Ok(())
        }
        fn unfreeze_at(&self, path: &Path) -> Result<(), CgroupError> {
            set_freeze_state(path, false);
            Ok(())
        }
        fn frozen_at(&self, path: &Path) -> Result<bool, CgroupError> {
            let content = std::fs::read_to_string(path.join("cgroup.events"))?;
            Ok(content.contains("frozen 1"))
        }
        fn freeze_requested_at(&self, path: &Path) -> Result<bool, CgroupError> {
            Ok(std::fs::read_to_string(path.join("cgroup.freeze"))?.trim() == "1")
        }
    }

    /// A hierarchy freezer over the fake tree, the way the re-exec'd envd
    /// builds its own (membership file per process). The caller keeps the
    /// membership tempdir alive for the test.
    fn hierarchy_freezer(
        fake: &FakeCgroupfs,
        membership: &tempfile::TempDir,
    ) -> Arc<WorkloadFreezer> {
        let procfile = membership.path().join("proc_self_cgroup");
        std::fs::write(&procfile, INIT_PROCFILE).unwrap();
        Arc::new(
            WorkloadFreezer::new(Arc::new(FakeCgroupfs {
                root: fake.root.clone(),
                paths: fake.paths.clone(),
            }))
            .with_proc_self_cgroup(procfile),
        )
    }

    fn resume_account() -> Account {
        use nix::unistd::{getegid, geteuid, getgroups};
        Account {
            name: "handover-resume".into(),
            home: "/tmp".into(),
            uid: geteuid().as_raw(),
            gid: getegid().as_raw(),
            groups: getgroups()
                .unwrap()
                .into_iter()
                .map(|group| group.as_raw())
                .collect(),
        }
    }

    fn runtime() -> ProcessRuntime {
        ProcessRuntime::new(
            resume_account(),
            Defaults {
                workdir: Some("/tmp".into()),
                ..Default::default()
            },
            Environment::default(),
            None,
        )
        .unwrap()
    }

    /// The carried state a successful outgoing upgrade writes: one watcher,
    /// one mount ledger entry, one retained exit, one guest-frozen record.
    fn carried_state(watch_dir: &Path) -> wire::HandoverState {
        wire::HandoverState {
            from_ver: "0.10.2".into(),
            terminated: vec![coorworker_envd::upgrade::RetainedExit {
                pid: 4_000_001,
                tag: Some("done".into()),
                end: Some(coorworker_envd::proto::process::process_event::EndEvent {
                    exit_code: 7,
                    exited: true,
                    status: "exited".into(),
                    ..Default::default()
                }),
                remaining_ms: 30_000,
            }
            .to_wire()],
            watchers: vec![wire::HandoverWatcher {
                id: "w-handover".into(),
                path: watch_dir.to_string_lossy().into_owned(),
                recursive: false,
                include_entry_info: false,
                ..Default::default()
            }],
            mounts: vec![wire::MountEntry {
                path: "/data/nfs".into(),
                lifecycle_id: "lc-42".into(),
                ..Default::default()
            }],
            guest_frozen_cgroups: vec!["customer/paused".into()],
            ..Default::default()
        }
    }

    /// The outcome rides every /init response (store.go:119-149 via the
    /// X-Envd-Handover header); a `thaw: skip` probe reads it without
    /// disturbing a frozen workload.
    async fn handover_header(daemon: Arc<Daemon>, runtime: ProcessRuntime) -> serde_json::Value {
        use tower::ServiceExt as _;
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/init")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(br#"{"thaw":"skip"}"#.to_vec()))
            .expect("probe request");
        let response = coorworker_envd::daemon::app_with_execution(daemon, runtime)
            .oneshot(request)
            .await
            .expect("router response");
        let value = response
            .headers()
            .get("x-envd-handover")
            .expect("a handover-booted envd advertises the outcome on every /init")
            .to_str()
            .expect("the outcome header is ascii json")
            .to_owned();
        serde_json::from_str(&value).expect("the outcome header is json")
    }

    /// Like [`handover_header`], but for a daemon that has not resumed yet:
    /// no outcome may be advertised, and the initialization side effect is
    /// the point.
    async fn init_without_outcome(daemon: Arc<Daemon>, runtime: ProcessRuntime) {
        use tower::ServiceExt as _;
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/init")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(br#"{"thaw":"skip"}"#.to_vec()))
            .expect("probe request");
        let response = coorworker_envd::daemon::app_with_execution(daemon, runtime)
            .oneshot(request)
            .await
            .expect("router response");
        assert!(response.headers().get("x-envd-handover").is_none());
        assert_eq!(response.status(), axum::http::StatusCode::NO_CONTENT);
    }

    fn write_frozen_workload(fake: &FakeCgroupfs) {
        // The outgoing envd froze the workload before the execve; the tree it
        // left behind reads frozen on both workload cgroups.
        for process_type in [ProcessType::User, ProcessType::Pty] {
            set_freeze_state(&fake.paths[&process_type], true);
        }
    }

    /// upgrade.go:330-338: a resume that fails at the read thaws the
    /// inherited workload, keeps the blob (its early returns leave it), and
    /// reports the failed outcome so the orchestrator tears the sandbox down
    /// instead of mistaking the version flip for success.
    #[tokio::test]
    async fn a_failed_resume_unfreezes_keeps_the_blob_and_reports_failed() {
        let tree = tempfile::tempdir().unwrap();
        let fake = FakeCgroupfs::new(&tree);
        write_frozen_workload(&fake);
        let blob_dir = tempfile::tempdir().unwrap();
        let blob_path = blob_dir.path().join("envd-handover.pb");
        coorworker_envd::upgrade::write_handover(
            &blob_path,
            &wire::HandoverState {
                from_ver: "0.10.2".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let incoming: Result<Option<wire::HandoverState>, coorworker_envd::upgrade::UpgradeError> =
            Err(coorworker_envd::upgrade::UpgradeError::Read(
                std::io::Error::new(std::io::ErrorKind::NotFound, "blob vanished mid-boot"),
            ));
        let membership = tempfile::tempdir().unwrap();
        let freezer = hierarchy_freezer(&fake, &membership);
        let daemon = Arc::new(Daemon::new().with_freezer(freezer.clone()));
        resume_incoming_handover(
            &incoming,
            &daemon,
            &runtime(),
            &resume_account(),
            &Defaults::default(),
            &Logger::none(),
            Some(freezer.clone()),
            &blob_path,
            NEVER,
        )
        .await;
        // The security property: the refused resume never leaves the
        // workload frozen (upgrade.go:330-338).
        assert_eq!(
            std::fs::read_to_string(fake.freeze_file(ProcessType::User)).unwrap(),
            "0\n",
            "a failed resume must unfreeze the inherited workload"
        );
        assert_eq!(
            std::fs::read_to_string(fake.freeze_file(ProcessType::Pty)).unwrap(),
            "0\n"
        );
        // The blob stays behind on the failure paths (disclosed #12).
        assert!(
            blob_path.exists(),
            "a failed resume must keep the blob, as Go's early returns do"
        );
        // A failed resume carries no mount ledger (main.go:299-302).
        assert!(daemon.export_mounts().is_empty());
        // The outcome the next /init advertises is the FAILED one
        // (store.go:119-133).
        let outcome = handover_header(daemon, runtime()).await;
        assert_eq!(outcome["failed"], serde_json::Value::Bool(true));
        assert_eq!(outcome["procs"], 0);
        assert_eq!(outcome["watchers"], 0);
        assert_eq!(outcome["retained"], 0);
    }

    /// upgrade.go:341-344: no blob is a no-op resume — but the deferred
    /// unfreeze still runs (it is the no-op it is designed to be on an
    /// already-thawed tree) and the outcome is the default, unfailed one.
    #[tokio::test]
    async fn a_resume_without_a_blob_unfreezes_and_reports_a_default_outcome() {
        let tree = tempfile::tempdir().unwrap();
        let fake = FakeCgroupfs::new(&tree);
        write_frozen_workload(&fake);
        let blob_dir = tempfile::tempdir().unwrap();
        let blob_path = blob_dir.path().join("envd-handover.pb");
        let incoming = coorworker_envd::upgrade::read_handover(&blob_path);
        assert!(matches!(incoming, Ok(None)), "no blob: the no-op resume");
        let membership = tempfile::tempdir().unwrap();
        let freezer = hierarchy_freezer(&fake, &membership);
        let daemon = Arc::new(Daemon::new().with_freezer(freezer.clone()));
        resume_incoming_handover(
            &incoming,
            &daemon,
            &runtime(),
            &resume_account(),
            &Defaults::default(),
            &Logger::none(),
            Some(freezer.clone()),
            &blob_path,
            NEVER,
        )
        .await;
        assert_eq!(
            std::fs::read_to_string(fake.freeze_file(ProcessType::User)).unwrap(),
            "0\n",
            "the deferred unfreeze runs on the no-op path too"
        );
        let outcome = handover_header(daemon, runtime()).await;
        assert_eq!(outcome["failed"], serde_json::Value::Bool(false));
        assert_eq!(outcome["procs"], 0);
        assert_eq!(outcome["watchers"], 0);
        assert_eq!(outcome["watchers_failed"], 0);
        assert_eq!(outcome["retained"], 0);
    }

    /// upgrade.go:472-534 in order: the retained exit is restored, the blob
    /// is removed ONLY on this path, the watcher re-arms under its preserved
    /// id, the guest-frozen record lands before any thaw, the inherited
    /// freeze raises the spawn barrier (resume_frozen failure would be
    /// logged, not fatal), the mount ledger is restored, and the outcome is
    /// the resumed one with per-item counts.
    #[tokio::test]
    async fn a_successful_resume_removes_the_blob_and_readopts_every_ledger() {
        let tree = tempfile::tempdir().unwrap();
        let fake = FakeCgroupfs::new(&tree);
        write_frozen_workload(&fake);
        let watch_dir = tempfile::tempdir().unwrap();
        let blob_dir = tempfile::tempdir().unwrap();
        let blob_path = blob_dir.path().join("envd-handover.pb");
        let carried = carried_state(watch_dir.path());
        coorworker_envd::upgrade::write_handover(&blob_path, &carried).unwrap();
        // The incoming side is exactly what the re-exec'd main computes: the
        // whole read result of the blob from disk.
        let incoming = coorworker_envd::upgrade::read_handover(&blob_path);
        assert_eq!(
            incoming
                .as_ref()
                .unwrap()
                .as_ref()
                .expect("the blob was just written")
                .terminated
                .len(),
            1
        );
        let membership = tempfile::tempdir().unwrap();
        let freezer = hierarchy_freezer(&fake, &membership);
        let daemon = Arc::new(Daemon::new().with_freezer(freezer.clone()));
        let test_runtime = runtime();
        resume_incoming_handover(
            &incoming,
            &daemon,
            &test_runtime,
            &resume_account(),
            &Defaults::default(),
            &Logger::none(),
            Some(freezer.clone()),
            &blob_path,
            NEVER,
        )
        .await;
        // Blob removed only on success (upgrade.go:495).
        assert!(
            !blob_path.exists(),
            "the success path removes the blob; its job is done"
        );
        // The retained exit is served again (upgrade.go:488-493).
        let exported = test_runtime.export_retained().await;
        assert_eq!(exported.len(), 1, "{exported:?}");
        assert_eq!(exported[0].pid, 4_000_001);
        // The watcher re-armed under its PRESERVED id (upgrade.go:497-503).
        let snapshots = daemon.watcher_registry().snapshots().unwrap();
        assert_eq!(snapshots.len(), 1, "{snapshots:?}");
        assert_eq!(snapshots[0].id, "w-handover");
        assert_eq!(snapshots[0].watch_path, watch_dir.path());
        // The guest-frozen record is restored (upgrade.go:525-531).
        assert_eq!(
            freezer.guest_frozen_paths(),
            vec!["customer/paused".to_owned()]
        );
        // resume_frozen made the inherited freeze exact: workload spawns are
        // refused (upgrade.go:533-546, freeze.go:1080-1086).
        let refused = freezer
            .begin_spawn(ProcessType::User, None)
            .await
            .expect_err("an inherited freeze must refuse workload spawns");
        assert!(matches!(refused, SpawnError::WorkloadFrozen));
        // The mount ledger is restored before the post-upgrade /init
        // (main.go:299-302).
        let mounts = daemon.export_mounts();
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].path, "/data/nfs");
        assert_eq!(mounts[0].lifecycle_id, "lc-42");
        // The outcome is the resumed one with per-item counts
        // (upgrade.go:522-534).
        let outcome = handover_header(daemon, test_runtime).await;
        assert_eq!(outcome["failed"], serde_json::Value::Bool(false));
        assert_eq!(outcome["procs"], 0);
        assert_eq!(outcome["retained"], 1);
        assert_eq!(outcome["retained_failed"], 0);
        assert_eq!(outcome["watchers"], 1);
        assert_eq!(outcome["watchers_failed"], 0);
    }

    /// main.go:313-330: with /init never arriving, the fallback thaw lands
    /// after the grace window and clears the inherited freeze.
    ///
    /// Discrimination note (review round 2, R1): this runs the SUCCESS path
    /// on purpose. The Err and Ok(None) resume outcomes run the deferred
    /// unfreeze themselves (upgrade.go:330-338/:341-344), which thaws and
    /// fires the signal BEFORE any timer — a fallback firing there would be
    /// masked, and the test would stay green with the fallback spawn deleted.
    /// On the success path the composition never unfreezes: resume_frozen
    /// adopts the freeze, and the watchdog stays disarmed because this fresh
    /// freezer's window is ZERO (the default, freeze.rs:254-261 — arm_watchdog
    /// early-returns), so the fallback is the ONLY thaw mechanism. An /init
    /// would consume the freeze earlier; it never arrives here.
    #[tokio::test(start_paused = true)]
    async fn the_fallback_thaw_fires_when_no_init_arrives() {
        let tree = tempfile::tempdir().unwrap();
        let fake = FakeCgroupfs::new(&tree);
        write_frozen_workload(&fake);
        let blob_dir = tempfile::tempdir().unwrap();
        let blob_path = blob_dir.path().join("envd-handover.pb");
        coorworker_envd::upgrade::write_handover(
            &blob_path,
            &wire::HandoverState {
                from_ver: "0.10.2".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let incoming = coorworker_envd::upgrade::read_handover(&blob_path);
        let membership = tempfile::tempdir().unwrap();
        let freezer = hierarchy_freezer(&fake, &membership);
        let daemon = Arc::new(Daemon::new().with_freezer(freezer.clone()));
        let mut thawed = freezer.thawed();
        resume_incoming_handover(
            &incoming,
            &daemon,
            &runtime(),
            &resume_account(),
            &Defaults::default(),
            &Logger::none(),
            Some(freezer.clone()),
            &blob_path,
            std::time::Duration::from_millis(50),
        )
        .await;
        // The composition left the inherited freeze in place (the success
        // path never unfreezes) and the daemon was never initialized, so at
        // fire time the gate lets the backstop through. Paused time advances
        // to the deadline the moment nothing else is runnable — no wall-clock
        // waiting.
        tokio::time::timeout(std::time::Duration::from_secs(2), thawed.changed())
            .await
            .expect("the fallback thaw must land when /init never arrives")
            .expect("the thaw signal channel is alive");
        assert_eq!(
            std::fs::read_to_string(fake.freeze_file(ProcessType::User)).unwrap(),
            "0\n",
            "the fallback must clear the inherited freeze"
        );
        assert_eq!(
            std::fs::read_to_string(fake.freeze_file(ProcessType::Pty)).unwrap(),
            "0\n"
        );
    }

    /// main.go:318-323: the fallback is gated on Initialized() AT FIRE TIME —
    /// an /init that arrived (here: before the window) leaves the workload
    /// frozen for the legitimate later freeze to own. The SUCCESS path is the
    /// scenario where the inherited freeze survives the resume itself
    /// (resume_frozen instead of the deferred unfreeze), so the only thaw
    /// that could answer here is the fallback.
    #[tokio::test(start_paused = true)]
    async fn the_fallback_thaw_is_gated_on_an_arrived_init() {
        let tree = tempfile::tempdir().unwrap();
        let fake = FakeCgroupfs::new(&tree);
        write_frozen_workload(&fake);
        let blob_dir = tempfile::tempdir().unwrap();
        let blob_path = blob_dir.path().join("envd-handover.pb");
        coorworker_envd::upgrade::write_handover(
            &blob_path,
            &wire::HandoverState {
                from_ver: "0.10.2".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let membership = tempfile::tempdir().unwrap();
        let freezer = hierarchy_freezer(&fake, &membership);
        let daemon = Arc::new(Daemon::new().with_freezer(freezer.clone()));
        let test_runtime = runtime();
        // /init arrives BEFORE the window: initialization is what the gate
        // reads when the timer fires.
        init_without_outcome(daemon.clone(), test_runtime.clone()).await;
        let incoming = coorworker_envd::upgrade::read_handover(&blob_path);
        let mut thawed = freezer.thawed();
        resume_incoming_handover(
            &incoming,
            &daemon,
            &test_runtime,
            &resume_account(),
            &Defaults::default(),
            &Logger::none(),
            Some(freezer.clone()),
            &blob_path,
            std::time::Duration::from_millis(50),
        )
        .await;
        // The timer still fires; the gate reads initialized and stands down.
        let fired =
            tokio::time::timeout(std::time::Duration::from_millis(150), thawed.changed()).await;
        assert!(
            fired.is_err(),
            "the gated fallback must not thaw an initialized sandbox"
        );
        assert_eq!(
            std::fs::read_to_string(fake.freeze_file(ProcessType::User)).unwrap(),
            "1\n",
            "an arrived /init keeps the inherited freeze"
        );
        assert_eq!(
            std::fs::read_to_string(fake.freeze_file(ProcessType::Pty)).unwrap(),
            "1\n"
        );
    }
}
