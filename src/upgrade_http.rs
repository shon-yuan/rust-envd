//! The shared `POST /upgrade` front half.
//!
//! Go's `main.go:427-475` handler is platform-neutral except for the fixed
//! staging path and the staged-binary write: the pre-init refusal, the
//! target-header check, the detached choreography and the 500 mapping are the
//! same bytes on every platform. This module owns that half; each daemon
//! supplies only its wired [`UpgradeRun`] and its staging path through
//! [`UpgradeState`].

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    body::{Body, HttpBody},
    extract::State,
    http::{Request, StatusCode},
    response::Response,
};

use crate::http::{plain_http_error, ConfigurationState};
use crate::upgrade::UpgradeRun;

/// The daemon state the shared handler needs. `configuration()` (from
/// [`ConfigurationState`]) supplies the pre-init check; the two methods
/// supply the platform-specific pieces.
pub trait UpgradeState: ConfigurationState + Send + Sync + 'static {
    /// The outgoing choreography wired by the binary's entry point, if any.
    fn upgrade_run(&self) -> Option<UpgradeRun>;

    /// The only filesystem path a delivered binary is staged to and exec'd
    /// from. On Linux this is `/usr/bin/envd.next`; on Windows
    /// `%ProgramData%\e2b\envd.next.exe`.
    fn upgrade_bin_path(&self) -> PathBuf;
}

/// Go main.go:427-475. The choreography's non-exec parts live here so they are
/// testable without the wired services: the pre-init 409, the foreign-target
/// 400, and the fixed-path body delivery. On success the process is replaced
/// and this never responds; the caller treats a dropped connection as
/// success. The run itself is spawned detached (Go's never-cancelled handler
/// goroutine, main.go:382-385), so a disconnect that drops this future still
/// lets the choreography run to its exec or its release+thaw tail.
pub async fn upgrade<S: UpgradeState>(
    State(daemon): State<Arc<S>>,
    request: Request<Body>,
) -> Response {
    // Refuse upgrades until the first authenticated /init. A re-exec'd envd
    // serves before /init with its access token cleared, so without this a
    // guest process could drive an unauthenticated upgrade in that window
    // (main.go:427-433).
    if !daemon.configuration().lock().await.access.initialized() {
        return plain_http_error(StatusCode::CONFLICT, "envd not initialized");
    }
    // The delivered binary is always written to and exec'd from a fixed path;
    // reject a caller asking for anything else (main.go:437-445).
    let target = daemon.upgrade_bin_path();
    if let Some(requested) = request
        .headers()
        .get("x-envd-upgrade-bin")
        .and_then(|value| value.to_str().ok())
    {
        if requested != target.to_string_lossy().as_ref() {
            return plain_http_error(StatusCode::BAD_REQUEST, "unsupported upgrade target");
        }
    }
    let mut new_bin = None;
    if request.body().size_hint().exact() != Some(0) {
        // The orchestrator streams the new binary in the request body; write it
        // to the fixed path before swapping (main.go:447-468).
        new_bin = Some(target.to_string_lossy().into_owned());
        if let Err(error) =
            crate::platform::upgrade_fs::stage_binary(request.into_body(), &target).await
        {
            return plain_http_error(StatusCode::INTERNAL_SERVER_ERROR, &error);
        }
    }
    let Some(run) = daemon.upgrade_run() else {
        // Unreachable on the guest path (main always wires the choreography);
        // a development daemon has nothing to swap into.
        return plain_http_error(StatusCode::INTERNAL_SERVER_ERROR, "upgrade is not wired");
    };
    // The choreography runs DETACHED from the connection, Go's own shape:
    // net/http never cancels a handler goroutine, so doUpgrade always
    // completes and its deferred release+thaw runs on every non-exec exit
    // (main.go:382-385) — the orchestrator treats the dropped connection as
    // success (main.go:426). axum drops this handler future on a disconnect,
    // and dropping an awaited future cancels it: an inline await would leave
    // the workload frozen with the spawn barrier up while a success that
    // never exec'd was recorded. The spawn owns the run from here on; this
    // future only awaits the handle, so a drop lands on the handle and never
    // reaches the choreography.
    let run_task = tokio::spawn(async move { run(new_bin).await });
    match run_task.await {
        // doUpgrade does not return on success — the process has been
        // replaced and this never responds. Reaching here means a wired hook
        // that came back, which Go could only produce by a failed Exec
        // (main.go:295).
        Ok(Ok(())) => plain_http_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "upgrade returned without replacing the process",
        ),
        Ok(Err(error)) => plain_http_error(StatusCode::INTERNAL_SERVER_ERROR, &error),
        // The spawned task owns its rollback, so a join error is a panic
        // outside the choreography's own error paths; answer the same 500 the
        // failed run returns.
        Err(join_error) => {
            plain_http_error(StatusCode::INTERNAL_SERVER_ERROR, &join_error.to_string())
        }
    }
}
