//! The envd live-upgrade handover, ported from the pinned oracle
//! `internal/services/process/upgrade.go` (outgoing and incoming), the schema
//! discipline of `spec/upgrade/handover.proto`, the exec-context carry
//! (`execcontext`), and the api service's handover outcome header
//! (`internal/api/store.go`/`init.go`) at 51083a83cc0903185d992725ea6bb33683814b29.
//!
//! The blob is the cross-version contract: an additive protobuf written to a
//! tmpfs file by the outgoing envd and decoded by the incoming one after the
//! execve (upgrade.go:22-28). The wire types are generated from the unchanged
//! pinned `third_party/e2b/spec/upgrade/handover.proto`, so encoding is
//! protobuf-identical to Go's `proto.Marshal` by construction.
//!
//! Runtime differences are disclosed at their use sites. The process carry
//! now follows the oracle's choreography (`process/mod.rs`, `process/pipe.rs`,
//! `process/supervise::run_adopted`): the outgoing envd dup3's each live
//! child's daemon-side descriptors onto the fixed slot window with CLOEXEC
//! cleared (upgrade.go:156-250), the blob carries them, and the incoming envd
//! relocates them and rebuilds readers, stdin and terminals around the
//! inherited descriptors (readopt.go `Readopt`). The children themselves keep
//! running as this process's children across the execve — the pid never
//! changes — so the reaper collects their status with a pid-specific wait.
//! Everything else about the swap (freeze hold, watchers, mounts, forwards,
//! helpers, the retention cache, guest-frozen cgroups, defaults, schema
//! gating, argv/env of the exec, the failure thaw) is carried 1:1.

use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;
use std::sync::Arc;

use buffa::Message as _;
use serde::Serialize;

use crate::exec_context::{self, Defaults};
use crate::proto::upgrade as wire;

/// Oracle upgrade.go:28 (`var HandoverPath`): the tmpfs blob the outgoing envd
/// writes and the incoming one reads. A parameter of the write/read entry
/// points (Go makes it a var so tests can point it at a temp file).
pub const HANDOVER_PATH: &str = "/run/e2b/envd-handover.pb";

/// Oracle upgrade.go:105 (`DefaultUpgradeBinPath`): the only filesystem path a
/// live upgrade will write a delivered binary to and re-exec into.
pub const DEFAULT_UPGRADE_BIN_PATH: &str = "/usr/bin/envd.next";

/// The schema versions (upgrade.go:53-60). A writer stamps the LOWEST version
/// that can express the blob it produced (upgrade.go:51-52).
pub const HANDOVER_SCHEMA_BASE: u32 = 1;
pub const HANDOVER_SCHEMA_GUEST_FROZEN: u32 = 2;
pub const HANDOVER_SCHEMA_DEFAULTS: u32 = 3;

/// Oracle upgrade.go:49: the maximum schema this envd reads; a blob above it
/// is refused rather than mis-read (upgrade.go:30-48).
pub const HANDOVER_SCHEMA: u32 = HANDOVER_SCHEMA_DEFAULTS;

/// The append-only argv tail the outgoing envd re-execs with
/// (upgrade.go:286-288: original flags kept, `--resume-handover` appended).
pub const RESUME_HANDOVER_FLAG: &str = "--resume-handover";

/// Oracle upgrade.go:572-574 (`ErrWorkloadNotFrozen`).
#[derive(Debug, thiserror::Error)]
#[error("workload was not frozen before the handover")]
pub struct WorkloadNotFrozen;

/// The outgoing upgrade choreography (Go's `doUpgrade` closure, main.go:369-420)
/// as the daemon's seam: main.rs closes over its services exactly as Go's
/// closure closes over main's locals. `None` means self re-exec (Go's empty
/// newBin). `Ok` means the process image was replaced — a `doUpgrade` that
/// returns normally has already exec'd, so the HTTP response is never written.
pub type UpgradeRun = Arc<
    // Parenthesised deliberately: a `dyn Fn(..) -> T + Send` object binds the
    // trailing bounds to T when T ends in a trait object, so without them the
    // Send/Sync would land on the boxed future instead of the callable.
    dyn (Fn(
            Option<String>,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>)
        + Send
        + Sync,
>;

/// The read/marshal failures the handover reports. Messages match the Go
/// `fmt.Errorf` texts the old envd would log at the same points.
#[derive(Debug, thiserror::Error)]
pub enum UpgradeError {
    #[error("read handover: {0}")]
    Read(#[source] std::io::Error),
    #[error("unmarshal handover: {0}")]
    Unmarshal(#[source] buffa::DecodeError),
    #[error("handover schema {actual} exceeds max supported {max}")]
    Schema { actual: u32, max: u32 },
    #[error("write handover: {0}")]
    Write(#[source] std::io::Error),
}

/// Oracle upgrade.go:71-94 (`schemaFor`): the LOWEST lossless schema for this
/// state, so an older reader either understands every field or refuses the
/// blob instead of silently dropping one.
pub fn schema_for(state: &wire::HandoverState) -> u32 {
    // A carried default that differs from the compiled-in fallback is
    // behaviour an older reader would drop, so stamp so such a reader refuses
    // (upgrade.go:75-88). The fallback itself is free.
    if let Some(defaults) = state.defaults.as_option() {
        if (!defaults.user.is_empty() && defaults.user != exec_context::BUILTIN_DEFAULT_USER)
            || defaults.has_workdir
        {
            return HANDOVER_SCHEMA_DEFAULTS;
        }
    }
    if !state.guest_frozen_cgroups.is_empty() {
        return HANDOVER_SCHEMA_GUEST_FROZEN;
    }
    HANDOVER_SCHEMA_BASE
}

/// Oracle upgrade.go:618-643 (`handoverDefaults`): the wire form of the live
/// exec context. Env vars are deliberately omitted — /init replaces them
/// wholesale. Only a DELIVERED user is carried; the workdir rides
/// independently; an empty record collapses to absent.
pub fn handover_defaults(defaults: &Defaults) -> Option<wire::HandoverDefaults> {
    let mut carried = wire::HandoverDefaults::default();
    if defaults.user_delivered {
        carried.user = defaults.user.clone();
    }
    if let Some(workdir) = &defaults.workdir {
        carried.workdir = workdir.clone();
        carried.has_workdir = true;
    }
    if carried.user.is_empty() && !carried.has_workdir {
        return None;
    }
    Some(carried)
}

/// Oracle upgrade.go:655-672 (`ApplyHandoverDefaults`): restore the carried
/// exec context onto the incoming envd's defaults, per field and only
/// widening, reporting which fields were applied.
pub fn apply_handover_defaults(
    state: &wire::HandoverState,
    defaults: &mut Defaults,
) -> (bool, bool) {
    let Some(carried) = state.defaults.as_option() else {
        return (false, false);
    };
    let mut user = false;
    let mut workdir = false;
    if !carried.user.is_empty() {
        defaults.user = carried.user.clone();
        defaults.user_delivered = true;
        user = true;
    }
    if carried.has_workdir {
        defaults.workdir = Some(carried.workdir.clone());
        workdir = true;
    }
    (user, workdir)
}

/// Oracle upgrade.go:154 (`st.Schema = schemaFor(st)`) plus 270-276 (marshal +
/// 0o600 write). Returns the schema stamped into the blob.
pub fn write_handover(path: &Path, state: &wire::HandoverState) -> Result<u32, UpgradeError> {
    let mut stamped = wire::HandoverState {
        schema: schema_for(state),
        ..state.clone()
    };
    stamped.schema = schema_for(&stamped);
    let blob = stamped.encode_to_vec();
    // The Go failure text is os.WriteFile's *fs.PathError Display —
    // `open <path>: <errno>` on the open and `write <path>: <errno>` on the
    // write — which the handler's "write handover: %w" wraps into the 500
    // body (upgrade.go:270-276).
    let path_error = |op: &str, error: std::io::Error| {
        UpgradeError::Write(crate::process::path_text_error(
            op,
            &path.to_string_lossy(),
            error,
        ))
    };
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        // os.WriteFile's perm applies on create only; an existing blob keeps
        // its mode (upgrade.go:274).
        .mode(0o600)
        .open(path)
        .map_err(|error| path_error("open", error))?;
    file.write_all(&blob)
        .map_err(|error| path_error("write", error))?;
    Ok(stamped.schema)
}

/// Oracle upgrade.go:341-361: the incoming side. A missing file is a no-op
/// resume; a malformed blob or a schema above [`HANDOVER_SCHEMA`] is refused
/// — post-execve there is no old binary to fall back to, so refusing is the
/// contract (upgrade.go:354-361).
pub fn read_handover(path: &Path) -> Result<Option<wire::HandoverState>, UpgradeError> {
    // The Go refusal text mirrors the write side: os.ReadFile's
    // *fs.PathError under "read handover: %w" (upgrade.go:341-361). A missing
    // blob stays the no-op resume.
    let path_error = |error: std::io::Error| {
        UpgradeError::Read(crate::process::path_text_error(
            "open",
            &path.to_string_lossy(),
            error,
        ))
    };
    let blob = match std::fs::read(path) {
        Ok(blob) => blob,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(path_error(error)),
    };
    let state = wire::HandoverState::decode_from_slice(&blob).map_err(UpgradeError::Unmarshal)?;
    if state.schema > HANDOVER_SCHEMA {
        return Err(UpgradeError::Schema {
            actual: state.schema,
            max: HANDOVER_SCHEMA,
        });
    }
    Ok(Some(state))
}

/// Oracle upgrade.go:349-352: decode without the schema gate, for the writer's
/// own round-trip tests. Production resume goes through [`read_handover`].
pub fn decode_handover(blob: &[u8]) -> Result<wire::HandoverState, UpgradeError> {
    wire::HandoverState::decode_from_slice(blob).map_err(UpgradeError::Unmarshal)
}

/// One restored terminal event. Defined in `crate::process` (which owns the
/// retention cache) and re-exported here so the handover surface is complete;
/// the wire conversions live with it.
pub use crate::process::RetainedExit;

/// Oracle store.go:119-133 (`handoverResult`) — the /init `X-Envd-Handover`
/// JSON header payload. Field names are Go's json tags (snake_case on the
/// *_failed counters, lower-case elsewhere).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct HandoverOutcome {
    pub failed: bool,
    pub procs: i64,
    pub procs_failed: i64,
    pub retained: i64,
    pub retained_failed: i64,
    pub watchers: i64,
    pub watchers_failed: i64,
}

impl HandoverOutcome {
    /// Oracle upgrade.go:522-534: the successful resume's summary from the
    /// decoded state and the re-arm counts.
    pub fn resumed(state: &wire::HandoverState, watchers: i64, watchers_failed: i64) -> Self {
        Self::resumed_with_failures(
            state.processes.len() as i64,
            0,
            state,
            watchers,
            watchers_failed,
        )
    }

    /// The same summary when the re-adoption loop reported failures
    /// (readopt.go's `procsFailed`): every item is total + failed-subset
    /// (ok = total - failed).
    pub fn resumed_with_failures(
        procs: i64,
        procs_failed: i64,
        state: &wire::HandoverState,
        watchers: i64,
        watchers_failed: i64,
    ) -> Self {
        Self {
            failed: false,
            procs,
            procs_failed,
            retained: state.terminated.len() as i64,
            retained_failed: 0,
            watchers,
            watchers_failed,
        }
    }
}

// Oracle upgrade.go:300-323 (`HandoverResult`) maps onto main.rs directly:
// the counters are [`HandoverOutcome`] (which rides the /init header), the
// mounts restore through [`crate::daemon::Daemon::import_mounts`], the
// forwards through [`crate::port::Forwarder::import_forwards`], the helpers
// through [`crate::cpus::ChildRegistry::adopt`], and the summary log line is
// main.rs's handover_resumed event — Go returns them as a struct only because
// its process service owns none of those consumers.
#[cfg(test)]
mod tests {
    use super::*;

    /// The Go builder never writes a stale stamp: the two-pass stamp keeps
    /// schema_for's input the stamped state (upgrade.go:154).
    #[test]
    fn write_handover_stamps_schema_for_on_the_stamped_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob.pb");
        let state = wire::HandoverState {
            guest_frozen_cgroups: vec!["x".into()],
            ..Default::default()
        };
        let stamp = write_handover(&path, &state).unwrap();
        assert_eq!(stamp, HANDOVER_SCHEMA_GUEST_FROZEN);
        let read = read_handover(&path).unwrap().unwrap();
        assert_eq!(read.schema, HANDOVER_SCHEMA_GUEST_FROZEN);
        // The written file carries owner-only permissions (upgrade.go:274).
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    /// Oracle os.WriteFile renders its failure as the *fs.PathError Display —
    /// `open <path>: <errno text>` — and the handler's `write handover: %w`
    /// wraps it: this is the failure guest's 500 body on the wire
    /// (upgrade.go:270-276 via main.go's writeHandover call).
    #[test]
    fn write_handover_refusal_carries_the_go_path_error_text() {
        let dir = tempfile::tempdir().unwrap();
        // A regular file where the blob's parent directory would live makes
        // the open fail with ENOTDIR deterministically.
        let blocker = dir.path().join("e2b");
        std::fs::write(&blocker, b"x").unwrap();
        let path = blocker.join("envd-handover.pb");
        let error = write_handover(&path, &wire::HandoverState::default()).unwrap_err();
        assert!(matches!(error, UpgradeError::Write(_)), "{error:?}");
        assert_eq!(
            error.to_string(),
            format!("write handover: open {}: not a directory", path.display())
        );
    }
}
