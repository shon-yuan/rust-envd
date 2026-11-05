# Provenance: vendored Cua Driver crates

Source: <https://github.com/trycua/cua>, `libs/cua-driver/rust`, commit
`d6855806e6eebfa9813b91986f59931e7f103a51` (2026-11-04).

License: MIT (`LICENSE` in this directory, copied verbatim from the upstream
repository root). Every vendored crate carries the upstream
`license.workspace = true` declaration (MIT).

Vendored crates, copied from the pinned commit with the local changes listed
below:

- `crates/cua-driver-core`
- `crates/cua-driver-contract`
- `crates/platform-linux`
- `crates/platform-windows`
- `crates/cursor-overlay`
- `crates/cua-cursor-motion`
- `crates/pip-preview`
- `rust/Skills/` — the skill documents `cua-driver-core` embeds with
  `include_str!`; the crate does not build without them.

Deliberately not vendored:

- `crates/cua-perception` and the perception extension: AGPL-3.0-only
  components. rust-envd must not ship or link them.
- `crates/cua-driver` (standalone MCP binary), `cua-driver-sdk`,
  `cua-driver-bindgen`, `cua-driver-testkit`, `cua-driver-e2e`,
  `cua-driver-uia`, `cursor-theme-cli`, `platform-macos`: not used by the
  rust-envd in-process registry.
- `libs/cua-spacesd`: FSL-1.1-MIT (source-available with a competing-use
  restriction), not MIT. It must not be merged into this repository.

## Native envd integration

The implemented [native CUA contract](../../docs/native-cua.md) links the desktop
registry directly into envd through optional desktop dependencies. It exposes
four native REST routes inside S0 authentication with the initialized,
nonempty current `X-Access-Token`. There is no MCP transport, extension
framework, worker/helper or execution-mode selector in envd's native service.
Windows service/Session 0/unknown contexts report unavailable before registry
construction; `--no-default-features` retains the desktop-free base build.
Latest local full tests passed default 907/base 881, zero failures and
24 doctests ignored in each configuration. Final default/base all-target
strict Clippy, formatting, Windows base MSVC library/binary crosscheck,
17 distribution checks, notices verification and rebuild passed. This is local fixture
and compilation evidence, not native Windows desktop/live S0 acceptance. The
[verification record](../../docs/native-cua.md#verified-local-state) retains
all platform, toolchain and delivery limits.

## Local changes

- `rust/Cargo.toml`: workspace `members` reduced to the vendored set.
  `rust/Cargo.lock` remains upstream's lock, unmodified. Raise the local zstd
  requirement from `0.13` to `0.14.3` for the security fixes in zstd and
  zstd-safe 8.2.0 (RUSTSEC-2026-0391, -0398, -0403 and related unsoundness
  advisories). At the maintenance revision, the two consuming lockfiles
  resolved the fixed versions and both generated notice bundles carried
  zstd 0.14's BSD-3-Clause license. Those graph/bundle claims describe the
  earlier daemon/worker layout, not the native migration's resolved graph.
  The theme artifact format and loader APIs are unchanged by this patch.
- `rust/crates/platform-linux/src/wayland/hyprland.rs`: deserialize Hyprland
  0.57 workspace addresses, retaining legacy numeric IDs. Addresses take
  precedence; display names never identify workspaces. Explicit empty
  workspaces do not contribute to visibility; missing required identity is
  an error. Existing window identity, PID, geometry, hidden and DPMS rules
  remain in place.
- `rust/crates/platform-linux/src/wayland/mod.rs`: add a fallible window
  dispatcher for authoritative Hyprland/niri IPC errors. Keep the existing
  best-effort dispatcher for other callers and the existing fallback order
  for other compositors and X11.
- `rust/crates/platform-linux/src/tools/list_windows.rs`: use the fallible
  dispatcher and return `ToolResult::error` for enumeration or blocking-task
  failure (historically exposed as MCP tool errors). A successful empty
  enumeration still returns an empty list.

The workspace compatibility change follows the JSON emitted by Hyprland
`v0.57.0` (`0aa6745f705f2c4b0a5004eb019ef76e5bbdb426`,
`src/ipc/s1/Commands.cpp`); no Hyprland implementation code is copied.
Cua main at `fc5363878a94d84f1e5ea8fcad045c0b9ada4b59` still had the
pinned Hyprland parser, so these are local patches, not an upstream upgrade.
Historical regression location: before the native migration,
`cua-worker/tests/wayland_windows.rs` exercised these patches through the
worker MCP endpoint against isolated IPC fixtures. That path and its passing
results belong to the earlier revision; preserve the coverage intent for the
direct registry/native API without claiming those old runs validate it.

## Native invocation completion patch

`rust/crates/cua-driver-core/src/work_scope.rs` (exported from `src/lib.rs`)
adds opt-in completion tracking. Native envd wraps each registry invocation in
`InvocationScope::run`: it drains finite handed-off work before returning,
including when the invocation panics, then resumes that panic for the host's
existing error mapping. Markers are acquired before enqueueing; a dropped
unstarted closure, a completed worker, or worker unwind releases its marker.
Blocking workers bind the scope for nested work and restore the prior thread
binding on exit. Finite async cleanup inherits the scope as well. `bind_future`
acquires its marker before enqueueing a future on a caller-selected runtime,
releases it even if dropped unpolled, and binds the scope while the future is
polled. Outside an invocation scope the wrappers retain ordinary Tokio/thread
execution semantics.

The blocking-spawn substitutions deliberately cover ordinary awaited actions
as well as direct timeouts: `run_script` can cancel arbitrary nested dispatch,
and browser preparation can cancel discovery/consent futures. The changed
paths, relative to `rust/crates`, are:

- `cua-driver-core/src/{clipboard.rs,window_target.rs,recording_tools.rs,
  perception_client.rs,perception_client/containment/windows.rs,
  browser/prepare.rs}`: finite blocking jobs use `work_scope::spawn_blocking`.
- `platform-linux/src/{input/action.rs,input/mod.rs,atspi/mod.rs,
  atspi/native.rs,browser_platform.rs,browser_consent_ui.rs,health_report.rs,
  wayland/portal_screenshot.rs}` and the blocking jobs in
  `platform-linux/src/tools/*.rs`: use the same tracked spawn. The existing
  `wayland/hyprland_input.rs` blocking-spawn test sites use that wrapper too.
- `platform-windows/src/{launch_uwp.rs,browser_platform.rs,
  browser_consent_ui.rs,health_report.rs}` and the blocking jobs in
  `platform-windows/src/tools/*.rs`: use the same wrapper. This includes both
  UIA/screenshot timeout jobs in `tools/get_window_state.rs`, AppsFolder
  lookup, and the ShellExecute timeout in `tools/launch_app.rs`.
- `cua-driver-core/src/{recording.rs,script_tool.rs}`: bind finite raw-thread
  state capture and the per-call script engine. The script engine exits on
  completion or its existing interrupt/cancel mechanism; it is not a pool or
  session-lifetime event loop.
- `platform-linux/src/{capture.rs,input/foreground.rs,input/mod.rs,
  atspi/native.rs,browser_platform.rs,recording_hooks.rs,
  wayland/portal_screenshot.rs}`: bind finite raw-thread timeouts, abandoned
  input release, process-output reaping, and joined native-runtime bridges so
  nested jobs remain attributable after a local deadline.
- `platform-windows/src/{uia/windows_enum.rs,win32/installed_apps.rs}`: bind
  the uncancellable UIA and WinRT scan threads across their receive deadlines.
- `cua-driver-core/src/{tool.rs,browser/engine.rs,browser/prepare.rs}` and
  `platform-windows/src/tools/{click.rs,double_click.rs,drag.rs,impl_.rs,
  launch_app.rs,right_click.rs}`: track finite async revocation, cancellation
  rollback, and foreground restoration, including nested blocking work.
- `platform-linux/src/wayland/shell_helper/{transport.rs,deadline.rs}`: bind
  each finite request before enqueueing on the existing dedicated runtime.
  Preserve the caller's absolute deadline, reject an expired queued request
  before polling it, check expiry before every subsequent poll (with a timer
  that wakes pending work at expiry), and recheck expiry after connection-lock
  acquisition and before helper invocation. The per-poll check matters because
  Tokio's timeout polls its inner future before its timer: a request already
  waiting on its own I/O could otherwise send after the deadline once that I/O
  completed. Persistent bus/runtime ownership stays outside the per-request
  drain.
- `platform-linux/src/tools/impl_.rs::atspi_up`: return the existing Wayland
  result before starting its unneeded accessibility probe. On X11 the finite
  joined probe also binds its scope for nested work.

Persistent managed resources remain outside an invocation's drain: CDP socket
readers/parked-socket reapers (`browser/cdp_ws.rs`), history writers, cursor
sampling and video/recording loops, AT-SPI event listeners, compositor/input
and overlay event loops, session-owned idle popup/device cleanup, and launched
application/process lifetime reapers. Their lifetime is controlled by their
existing session/resource owners; successful start/launch tools do not wait for
those resources to stop. Finite state-capture/setup and cleanup jobs are tracked
separately. Completion tracking cannot cancel a wedged OS call: the host may
return its deadline response while admission stays held until actual completion
or process termination. It does not turn a persistent resource into action work.

The host adapter and isolated regression coverage are
`src/cua/desktop.rs` and `tests/cua_completion.rs` in the repository root.
The regression uses blocked in-memory worker threads, never a real desktop.
The root integration test includes the exact pure compositor deadline helper
source, so its queued-expiry and live-deadline tests run without D-Bus or a
separate vendor workspace build. These are coverage for the identified handoff
boundaries, not proof that every possible native handoff has been inventoried.
All 15 completion regressions passed in the latest full default tests and
separately, including all three exact compositor deadline-helper cases:
expired before first poll, live completion and resumed I/O after expiry.
`BeforeDeadline` checks the absolute deadline and timer before every inner
poll, preventing dispatch when previously pending I/O becomes ready too late.
The actual registry adapter's in-memory completion test also passed in the
latest full tests, which recorded 907 default and 881 base tests with no
failures. The final Rust source recheck closed the deadline finding and
confirmed timer wake-up behavior, with no remaining blocking defect,
alongside code/security follow-up. Final remaining checks and rebuilt smoke
passed. The smoke found 63 tools, one process
and one listener and rejected old MCP/JSON-RPC/configuration without desktop
actions. These isolated results do not establish native Windows desktop
execution or live S0 Windows acceptance of the new API; the local environment
had no musl compiler. No fresh Go/SDK comparison, remote CI or
commit/push/merge/deploy is claimed. Root-module coverage is scoped in the
native verification record; dependency coverage exports were empty, so no
numeric coverage is claimed for `work_scope`.

Update procedure: re-copy the same seven crate directories, `rust/Skills/`
and the two manifests from a reviewed upstream commit. Re-apply the members
edit, zstd security requirement, the three Linux patches, and the native
invocation completion patch above unless the upstream update already implements
them; review that comparison explicitly.
Validate the direct registry through the isolated native API/platform fixtures,
the default and desktop-free daemon gates, and the notices checks for the
resulting dependency graph as specified in `docs/native-cua.md`. Do not restore
a worker/MCP runtime to satisfy the older regression commands. Keep this
provenance and the native CUA documentation aligned with the resulting patch
set; do not regenerate the upstream lock merely to run the trimmed vendor
workspace's tests. These obligations apply to future vendor updates; the local
migration results above retain their explicit acceptance limits.
