# Windows sandbox port — evaluation and plan

Status: P0 complete 2026-11-03 — the platform seam exists, the Linux backends
are moved under `src/platform/linux/`, the Windows skeleton compiles for
`x86_64-pc-windows-msvc`, and the full Linux parity gate set is IDENTICAL on
the same candidate.
P1 in progress — the filesystem backend, the shared HTTP surface, the
resource endpoints and all seventeen Connect procedures (filesystem and
process) are implemented and contract-tested; only `POST /upgrade` remains.
See "P1 progress".
This document scopes goal D: **a Windows guest sandbox that serves the complete
existing envd API**, while the Linux daemon keeps its byte-level parity with the
pinned Go oracle.

## 1. Objective

One binary crate, two execution backends:

- **Linux** (today): the parity target. Unchanged wire behaviour, unchanged
  gates, unchanged claims.
- **Windows** (new): runs as a native Windows service inside a Windows guest,
  exposes every API the Linux daemon exposes, with Windows-native execution
  semantics.

"API compatibility" is defined as:

- every path and procedure listed in section 4 exists and answers;
- request and response bodies validate against the same protobuf/OpenAPI
  schemas (`third_party/e2b`, `spec/envd.yaml` of the pinned oracle);
- status codes, error codes, headers and streaming framing follow the same
  rules;
- no endpoint is left returning `unimplemented` merely because the platform
  differs. Where Windows cannot produce a Linux value (for example the
  `uid`-derived `owner` field), the field is populated with a documented,
  deterministic Windows value, never omitted.

This is **not** a parity claim. There is no official Windows envd oracle, so
Windows behaviour is validated by contract tests and by a capability matrix,
not by differential evidence.

## 2. What goal D includes

1. **envd on Windows**: execution, filesystem, watchers, freeze, metrics,
   upgrade endpoints backed by Win32 APIs.
2. **Windows guest platform**: a VMM that can boot Windows. Firecracker
   supports Linux guest kernels only, so the microVM backend becomes Cloud
   Hypervisor (UEFI/OVMF, virtio drivers, TAP networking) or Hyper-V.
3. **Guest image pipeline**: 30–64 GB disk images instead of MB-scale
   initramfs, Windows activation/licensing, first-boot provisioning, agent
   installation, boot-time SLOs.
4. **Tool bridge**: the guest-side tool agent currently speaks AF_VSOCK port
   5000 and a guest-local UDS. Windows has no AF_VSOCK and no general
   virtio-vsock driver, so the bridge uses Hyper-V sockets (AF_HYPERV) or a
   private virtual NIC with TCP plus named pipes.
5. **Orchestration**: backend selection, image build, idle reaping, health
   checks, and a Windows security model (SIDs/ACLs, service accounts).

## 3. Current state — measured surface

- 33,697 lines of Rust across 77 source files.
- 36 of 77 files call `nix`/`libc` directly (225 call sites).
- Linux-bound backend code by area: `process/` 2,311 lines, `cgroups/` 2,801,
  `filesystem/` 1,390, `port/` 2,582, `cpus/` 1,190, `mmds/` 1,899, plus
  `oom.rs`, `upgrade.rs`, `clock.rs`, `cacerts/`, `hyperloop.rs` (~4,000).
- Platform-neutral code: the HTTP/1 engine, CORS, upload/download/compose,
  Connect RPC plumbing, auth, JSON/protobuf, logging (~20,000 lines).

Roughly **10,000–12,000 lines** need a platform abstraction and a Windows
implementation; the rest is reused.

## 4. API compatibility matrix

### 4.1 REST endpoints

| Endpoint | Windows implementation | Contract notes |
| --- | --- | --- |
| `GET /health` | Service health check | 204 unchanged. |
| `GET /metrics` | `GetSystemTimes`, `GetActiveProcessorCount` (fallback `GetSystemInfo`), `GlobalMemoryStatusEx`, `GetPerformanceInfo`, `GetDiskFreeSpaceExW` on the system volume | Same twelve-field JSON shape and presence rules. `cpu_used_pct` uses the shared `CpuTimes` formula and two-decimal rounding; `mem_cache` is `SystemCache * PageSize`. `cpu_possible`/`cpu_target`/`cpu_target_attempts`/`cpu_write_pending_ms` are all `0` (no CPU hotplug manager, the Linux `CpuManager::noop` fallback); `oom_kills` is omitted (no source in this slice, which the OpenAPI permits). Win32 failures produce the same empty 500 with `application/json` + `no-store`. |
| `POST /init` | Token and defaults setup, certificate install into the Windows certificate store, time check against host, SMB/NFS mount bookkeeping | Same request/response; `X-Envd-Version`, `X-Envd-Memory`, `X-Envd-Thaw: skip` and `X-Envd-Init-Clock-Correction` as on Linux. A `timestamp` is decoded with the oracle's RFC3339 acceptance set, corrected by the handler delay, gated newest-wins, and stepped with `SetSystemTime` when it leaves the 50 ms/5 s bands (§6.2); the freeze-audit header stays a documented omission (no cgroup to classify). Linux-only effects (cgroup manager, MMDS route, hyperloop) become documented no-ops with a log line; the response contract is unchanged. `thaw` is decoded: unless it is `"skip"`, `/init` resumes the workload through the generation-guarded thaw, registered as a detached task so a dropped client still thaws it. |
| `POST /freeze` | Suspend every thread of every tracked process (`SuspendThread` per enumerated thread, with a re-enumeration confirm) | Same body (all 13 fields), same `maxWaitMs` semantics and the bare-204/structured-200 split. Legacy mode suspends the registry's User/Pty pids; hierarchy mode uses the Job Object pid list minus the daemon, falling back to legacy when no Job is shared. Counter reconciliation matches Linux; `unobservable` is always 0 (no kernel read-back). |
| `POST /unfreeze` | Resume exactly the threads the last sweep suspended | Same body and idempotence: one `ResumeThread` per recorded tid, so a guest's own suspend counts are preserved; processes never touched by the sweep are not resumed. |
| `POST /collapse` | Real working-set trim (`SetProcessWorkingSetSizeEx`) and `GetProcessHeaps`/`HeapCompact`, plus a `VirtualQuery` scan | Same six-field response schema. `regions`/`chunks` are scan values (committed private `PAGE_READWRITE` regions and fully contained 2 MiB windows); `collapsed`/`alreadyHuge` are `0` and `skipped == chunks`, because Windows has no API to migrate base pages into huge pages. This replaces the earlier "counters are reported as zero" statement. |
| `POST /fsfreeze` | Volume flush (`FlushFileBuffers` over the system-volume handle) | Same response schema. Flush-only semantics: no FIFREEZE write barrier and no VSS in this slice. Errors use Win32 text; the envelope shape is unchanged. Requires volume write access (typically elevated). |
| `POST /fsthaw` | Repeat the volume flush; succeeds as a no-op | Same response. Idempotent, like `/fsfreeze`; no VSS snapshot to release. |
| `GET /envs` | Process environment and defaults from the service account | Same shape. |
| `GET /files` | Win32 file read, `Range`, conditional requests, directory handling | Same headers (including `Content-Range`/`Content-Length`/`ETag` behaviour) and error bodies. |
| `POST /files` | Multipart upload, gzip members, overwrite, metadata | Same body/framing; paths accept Windows drive and UNC forms. |
| `POST /files/compose` | Sequential composition with the same validation order and error texts | Same. Windows path strings and Windows system-message error tails are declared deviations; see the P1 compose entry. |
| `POST /upgrade` | Shared front half, then a strict freeze hold, retained-exit/exec-context export, the pinned blob, an `ENVDWIN1` sidecar carrying the duplicated listening socket, and a suspended `CreateProcessW` successor | Endpoint contract unchanged: 409 pre-init, 400 foreign `x-envd-upgrade-bin`, body staged to `%ProgramData%\e2b\envd.next.exe`, 500 on refusal/run error, and no response on success. Windows has no `exec`, so the successor is a new PID announced over a two-phase readiness pipe while the outgoing process drains nothing and exits 0. See §8.3. |
| `OPTIONS *`, CORS preflight | Shared code | Unchanged. |

Implementation status (2026-11-04): all thirteen REST operations are served by
the Windows daemon and covered by contract tests, `POST /upgrade` included
(the choreography is described in §8.3). The binary entry point
(`platform/windows/entry.rs`) serves the daemon as an SCM service, a marked
console guest, or a loopback-only development process.

### 4.2 Connect procedures

| Procedure | Windows implementation | Contract notes |
| --- | --- | --- |
| `process.Process/List` | Implemented: the runtime's live-entry registry (order unspecified, as on Linux) | Same `ProcessInfo` shape; `pid` is the Windows PID, and `config`/`tag` echo the Start request exactly. Retained exits are excluded, as on Linux. |
| `process.Process/Connect` | Implemented: attach to the entry's event mux, Start-frame first, then future data and the terminal `EndEvent`; retained exits answer Start+End for 30 s | Same `ProcessEvent` oneof, keepalive, and terminal `EndEvent`. No prior output is replayed and dropping the subscription never kills the child. |
| `process.Process/Start` | Implemented: `CreateProcessW` suspended, assigned to the sandbox Job shared with `/freeze` first and then to a per-process kill-on-close Job, then resumed; ConPTY when `pty` is present | Same request/validation order and error codes (cwd check, deadline, spawn wrapping). A requested account other than the service account is a documented `failed_precondition` because no credential provider is wired. |
| `process.Process/Update` | Implemented: `ResizePseudoConsole` | Same request/response; absent `pty` is a no-op, and the PTY-only error path is preserved. Degenerate sizes are truncated to `i16` and clamped below 1 (declared deviation). |
| `process.Process/StreamInput`, `SendInput` | Implemented: stdin pipe writes, or ConPTY input writes | Same framing, ordering, and Go error wrappers. |
| `process.Process/CloseStdin` | Implemented: close the stdin write handle | Same semantics, including idempotence and the PTY rejection text. |
| `process.Process/SendSignal` | Implemented: `SIGKILL` → `TerminateProcess` + `TerminateJobObject`; `SIGTERM` → `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT)`, ConPTY ETX, or `PostMessageW(WM_CLOSE)`, then `TerminateProcess` after a 5 s grace | Same request/response and synthesized `EndEvent`; delivery mechanism differs. |
| `filesystem.Filesystem/Stat`, `ListDir` | Implemented: `GetFileAttributesEx` via `std::fs` metadata, reparse points for symlinks, sorted depth-first walk | Same `EntryInfo` fields and messages; value synthesis and the declared differences are in 5.2. |
| `MakeDir`, `Move`, `Remove` | Implemented: `CreateDirectory`, `MoveFileExW` + `MOVEFILE_REPLACE_EXISTING`, recursive delete | Same validation order, codes and message shapes. `Move` cannot replace a directory; `Remove` clears read-only attributes once. |
| `WatchDir`, `CreateWatcher`, `GetWatcherEvents`, `RemoveWatcher` | Implemented: overlapped `ReadDirectoryChangesW`, one dedicated thread and directory handle per watcher | Same event schema, `w<pid hex><counter hex>` ids, sticky terminal error and Start/keepalive framing. RDCW batching and the missing CHMOD event are documented deviations. |

Implementation status (2026-11-04): all seventeen Connect procedures are
served by the Windows daemon. The nine Filesystem procedures run over
`WindowsFilesystemRuntime`/`WindowsWatcherProvider`; the eight Process
procedures run the shared `src/rpc/process.rs` adapter over
`WindowsProcessRuntime`, mounted by both `app()` and `app_with_execution()`.
An unknown procedure still answers the shared router's 404
`unimplemented`/`method not found: <path>`.

**Process Connect deviations (P1).** Windows behaviour is contract-tested,
not differentially verified (there is no Windows oracle):

- there is no `/bin/sh -c` wrapper: the request `cmd` is the executable and
  `args` become its argv, so shell syntax in `cmd` does not run;
- a missing executable is `invalid_argument` (HTTP 400) instead of a child
  that exits 127 with `error starting process ...: exit status 127`;
- there is no `oom_score_adj`/`nice`/`ionice` wrapper command, because those
  Linux concepts do not exist;
- `SIGTERM` is best-effort: `CTRL_BREAK_EVENT` for console children,
  ETX (0x03) to a ConPTY child, or `WM_CLOSE` for GUI/no-console children,
  followed by `TerminateProcess` after a 5 s grace period; Linux SIGTERM is
  always deliverable;
- PTY bytes are ConPTY-encoded (VT sequences with ConPTY's own echo/line
  handling), not Linux termios bytes; `Update` truncates `u32` dimensions to
  `i16` and clamps values below 1 to 1, where Linux accepts 0x0;
- signal deaths are synthesized as `exitCode -1`, `exited false`,
  `status`/`error` `signal: killed` or `signal: terminated`, so the wire shape
  matches Linux even though Windows reports numeric exit codes; a
  `CTRL_BREAK` default-handler exit (`STATUS_CONTROL_C_EXIT`) maps to
  `signal: terminated`, and any other Windows exit code is reported as
  `exit status N`;
- a process for another real account requires credentials that are not
  plumbed (the Basic password is discarded), so it is refused with
  `failed_precondition` instead of `setuid`; unknown accounts are still the
  Linux 401;
- every child is assigned to the sandbox Job first (the nesting root) and
  then to a per-process Job with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`;
  either assignment failing fails the start, so no process can escape
  hierarchy freeze accounting. The per-process Job is terminated as soon as
  the direct child is reaped, which releases descendant-held stdio pipes and
  lets `End` publish without waiting for a detached grandchild (Linux leaves
  that descendant running);
- the child environment is `Environment::for_process` plus the fixed
  Windows defaults (`SystemRoot`, `SystemDrive`, `ComSpec`, `TEMP`, `TMP`,
  `PATHEXT`), never the daemon's environment block, matching Linux's
  `env_clear` so service credentials do not leak into sandbox processes.
  NUL bytes in a key or value are `invalid_argument` with Linux's
  `nul byte found in provided data`; an empty name or a name starting with
  `=` is refused because the block would encode it as a hidden drive
  variable;
- there is no live-upgrade adoption: `export_processes`/`readopt_processes`
  are absent, so a Windows handover cannot carry a running process (P3).

## 5. Architecture

### 5.1 Seams

Introduce compile-time platform modules behind small traits, selected by
`#[cfg]`:

```text
src/platform/
├── mod.rs          # pub use linux::* / windows::* by cfg
├── linux/          # current modules, moved verbatim
└── windows/
    ├── process.rs  # CreateProcess, Job Object, ConPTY, signals
    ├── cgroup.rs   # Job Object limits, freeze, OOM notifications
    ├── fs.rs       # Win32 metadata, ACL synthesis, watchers
    ├── net.rs      # GetExtendedTcpTable
    ├── port/       # in-process loopback port forwarder
    ├── clock.rs
    └── metadata.rs # token delivery without MMDS
```

Traits: `ProcessBackend`, `ResourceBackend` (limits/freeze/OOM/metrics),
`FsBackend`, `WatcherBackend`, `PortScanBackend`, `ClockBackend`,
`MetadataBackend`.

Rules:

- Linux implementations are moved, not rewritten. The abstraction is a
  zero-cost wrapper (`#[inline]`, no dynamic dispatch on Linux), so timing
  behaviour of the parity paths does not change.
- Shared code (HTTP, Connect, auth, JSON) never calls a platform API directly.
- The acceptance gate for every refactor step is: all Linux parity gates
  re-run on the same candidate and remain IDENTICAL / `passed=true`.

### 5.2 Filesystem value synthesis

| Field | Windows source |
| --- | --- |
| `mode` | Synthesized from the read-only attribute and the entry kind: read-only → `0444`/`0555`, writable → `0644`/`0755`. A resolvable reparse-point entry reports `0777`; a dangling link reports `0` (mirroring Linux `lstat`). |
| `permissions` | Rendered from the same synthesized mode. |
| `owner`, `group` | Account name of the SID that owns the object (`DOMAIN\user`); group from the primary group SID. Never empty. |
| `metadata` | NTFS extended attributes under the same `user.e2b.` convention when readable; otherwise an empty map, never an error. |
| `symlink_target` | Reparse-point target; `is_symlink` and link classification come from `FILE_ATTRIBUTE_REPARSE_POINT`, so directory junctions and mount points are treated as links too. |
| `size` | The entry's logical size. For a link it is the UTF-16 byte length of the stored target path, the analogue of Linux `lstat`'s target-length `st_size`; if the target cannot be read it falls back to the reported file size rather than silently reporting 0. |

Clients that compare `owner` to a Linux username will see a SID-derived name;
that is a documented value difference, not a missing API. P1 currently
returns the empty map for `metadata`; NTFS extended-attribute reading is a
later slice.

**Filesystem Connect deviations (P1).** Windows behaviour is contract-tested,
not differentially verified (there is no Windows oracle):

- Returned `EntryInfo.path`, `symlink_target` and error messages use native
  `\` separators; watcher event `name` values use `/` relative to the watched
  path and `.` for its root.
- `mode`, `permissions`, `owner` and `group` are synthesized (read-only
  attribute plus entry kind; `DOMAIN\name`, SID string, or `None`). Windows
  exposes no POSIX special bits, so `permissions` renders only the
  `r`/`w`/`x` triples plus `d`/`L`. A dangling link reports
  `FILE_TYPE_UNSPECIFIED` and mode `0` (its `permissions` string stays
  `Lrwxrwxrwx`), matching Linux `lstat`.
- `EntryInfo.metadata` is always empty; NTFS extended attributes are not
  mapped.
- `Move` replaces an existing file (`MOVEFILE_REPLACE_EXISTING`) but refuses
  an existing directory, reported as `internal`; cross-volume moves fail.
  Moving onto a destination that has `FILE_ATTRIBUTE_READONLY` also fails with
  `internal` / `permission denied`, where Linux `rename(2)` succeeds because a
  file's own mode does not protect its directory entry; `Move` does not clear
  the destination's read-only attribute.
- `Remove` clears `FILE_ATTRIBUTE_READONLY` on regular files before deleting
  them and retries once on access-denied. Deletion is an explicit depth-first
  walk using `symlink_metadata` for every child. A reparse-point directory
  (junction, mount point or directory symlink) is unlinked with
  `RemoveDirectory` and never recursed into, so its target and the target's
  contents are left intact; the walk does not use `fs::remove_dir_all`.
- `ListDir` orders children by the `OsString` comparison order, i.e. WTF-8
  byte order, not UTF-16 code-unit order (for example U+E000 sorts before
  U+10000).
- Watchers emit no `EVENT_TYPE_CHMOD`: `FILE_ACTION_MODIFIED` cannot
  distinguish an attribute change from a write and is reported as Write.
  Recursive watching uses one subtree handle, so a directory moved in
  produces no synthetic child Create events and rename pairing is the OS's
  OLD then NEW records. Moving the watched root does not end the watch.
  `CreateWatcher`/`WatchDir` do not return before the reader thread has issued
  its first overlapped `ReadDirectoryChangesW`, so no event after the call is
  lost to a startup race (a thread that cannot start within 5 s fails with
  `internal`). The read buffer is 8-byte aligned. A zero-byte completion (the
  notify buffer overflowed), `ERROR_NOTIFY_ENUM_DIR`, and a malformed notify
  record (including an odd `FileNameLength`) are terminal and report
  `watcher event queue overflow` or `watcher event buffer is malformed`. A
  record whose entry cannot be stat-ed still produces the event, without the
  optional `entry` payload; a closed event channel reports
  `watcher event channel closed`.
- There is no SID/domain cache; `owner`/`group` render per entry with
  `LookupAccountSidW`.
- UNCs and drives of type `DRIVE_REMOTE` are treated as network mounts for
  `allow_network_mounts`.
- The Windows daemon uses `Logger::none()`, so the filesystem interceptor
  produces no log lines (logs are not part of the HTTP contract).

## 6. Guest platform

- **VMM**: Cloud Hypervisor with UEFI (`CLOUDHV.fd`), virtio-blk/net/console,
  and `--cpus nested=on` only if a client needs Hyper-V inside the guest.
  Firecracker is not an option for Windows guests.
- **Networking**: Cloud Hypervisor has no user-mode network or `hostfwd`;
  guests use TAP devices bridged/NATed by the host. The control-plane
  forwarder is the daemon's own TCP proxy, enumerated from
  `GetExtendedTcpTable`.
- **Image**: Windows Server/11 image with virtio drivers, the daemon installed
  as a service, and first-boot provisioning (token, environment, certificates)
  replacing `/init`'s Linux effects. Licensing and activation are operational
  prerequisites, not code.
- **Boot contract**: Linux validates PID 1 + `coorworker.envd=1`; the Windows
  service validates its service identity and an equivalent boot marker.

### 6.1 Loopback port forwarding

`platform/windows/port/` serves the oracle's `internal/port` package. The scan
loop, its subscriber rendezvous, the LISTEN filter and the `ForwardedPort`
record are the shared `platform/port` modules the Linux daemon also uses; only
the forward action differs: where the oracle spawns
`socat TCP4-LISTEN:<port>,bind=169.254.0.21,reuseaddr,fork
TCP<family>:localhost:<port>`, the Windows daemon runs an in-process Tokio
proxy that binds the same forward address and dials the same upstream (`::1`
for a family-6 listener, `127.0.0.1` otherwise).

- The refresh state machine is unchanged: every scan marks all tracked ports
  DELETE, re-marks the survivors FORWARD, starts a proxy for a new port and
  stops whatever stayed DELETE; the map key stays `<listener_pid>-<port>`.
- A start refused because the workload gate is held (the Windows analogue of
  the frozen socat cgroup) is removed again so the next scan retries; a start
  that fails to bind stays tracked and reads as forwarded-but-dark, exactly
  like a socat that failed to start.
- The proxy is stopped synchronously: the accept loop is joined so the
  listener is closed before the next refresh can rebind the port (the oracle
  kills socat asynchronously and relies on `socat`'s `reuseaddr`).
- `entry.rs` starts the scanner and the forwarder before the daemon accepts
  (`main.go:341-360`) and stops both on shutdown. A development daemon takes
  the oracle's own unavailable path: no subscription, no tasks, so a host
  daemon never binds `169.254.0.21`.
- A live upgrade carries no forwards: the proxy state lives in this process,
  so the successor's first scan re-establishes every proxy (the Linux blob
  carries socat pids because those children outlive the execve).
- Guest-image prerequisite: `169.254.0.21` must be a local address on the
  guest NIC, as in the E2B guest image.
- Tests (native, `cargo test --lib` on Windows): the WinSock family mapping,
  the gateway-listen/listener-dial shape, the unavailable-forwarder gate, the
  freeze → refuse → remove → thaw → retry sequence, and an end-to-end relay
  through the proxy on two loopback aliases.

### 6.2 Clock

`/init`'s clock slice is shared code (`src/clock.rs`: RFC3339 decode, the
handler-delay correction, the newest-wins gate, and the 50 ms past / 5 s
future bands) plus one platform edge: the step sink. On Windows it is
`platform/windows/clock.rs`, which steps with `SetSystemTime` — the analogue
of the oracle's `clock_settime(CLOCK_REALTIME)` (`clock_linux.go:11-15`).

- The correction contract is the Linux one: the reading is taken at the end of
  the body read (`init.go:250`), `timestamp` is replaced by
  `timestamp + correction`, the truncated whole milliseconds ride
  `X-Envd-Init-Clock-Correction` on success and error responses alike, a
  corrected value older than the last accepted `UnixNano` skips SetData
  entirely (envs, token, defaults, step) while still answering 204, and an
  equal value is accepted (`utils/atomic.go:16-26`).
- Band check and step target use the corrected timestamp (`init.go:433-443`,
  `:1089-1091`). A failed step is logged and never fails `/init`.
- `SetSystemTime` takes UTC calendar fields, so the nanosecond target reduces
  to 100-ns ticks since 1601-01-01 and then through Hinnant's
  `civil_from_days`, floor-normalized like `unix.NsecToTimespec` (pre-epoch
  targets included).
- The step needs `SeSystemtimePrivilege` held *and enabled* in the process
  token; the sink enables it on demand and checks the residual
  `ERROR_NOT_ALL_ASSIGNED` (`AdjustTokenPrivileges` reports success anyway).
  The service runs as LocalSystem, whose token carries the privilege
  disabled by default; an unelevated console user gets a logged failure.
- `entry.rs` wires the sink only for guest modes, so a development daemon
  never touches the host clock — the same guest-only rule as the Linux
  `/etc/hosts` write. A timestamped `/init` on a development daemon still
  answers the correction header and advances the gate with the step skipped.
- Declared deviations: `SYSTEMTIME` truncates to milliseconds (the oracle's
  target is nanosecond-precise, though its guest clocks are not); the step is
  a direct syscall, not "error text must match"; and an unelevated caller's
  failure text is Win32, not errno, as elsewhere in this port.
- Tests (native, `cargo test --lib` on Windows): `daemon_windows/contract_tests/clock.rs`
  drives the production `app()` over a real loopback listener with recorded
  sinks — the absent/`null` timestamp, the in-band no-step case, the forward
  step target, a failing sink still applying SetData, the stale-equal-newest
  gate, millisecond truncation (negative corrections included), the malformed
  and JSON-escaped RFC3339 rejects, the pre-epoch/wrapped-timestamp refusal,
  and the sink-less development daemon. The pure conversion is unit-tested in
  `platform/windows/clock.rs` (epoch, a known instant, pre-epoch flooring,
  leap days).

## 7. Tool bridge

The sandbox stack's guest tool agent currently runs over AF_VSOCK port 5000
and a guest-local UDS. On Windows it must run over AF_HYPERV (requires the
Hyper-V socket provider and a VMM that exposes it) or a private virtio-net
segment with TCP loopback plus named pipes for local subprocesses. This is a
separate crate from envd and is scoped as its own work item.

## 8. Entry point, service lifecycle and live upgrade

### 8.1 Entry point and launch modes

`src/platform/windows/entry.rs` is the Windows entry point. It lives in the
library (not in `src/main.rs`) because the handover sidecar seams are
crate-internal; `main.rs` only calls `entry::run()`.

1. `E2B_REEXEC_HELPER` guard (reexec.go:49-68), then the shared `cli` parser,
   then `--version` / `--commit` in Go's order.
2. `--development` skips the SCM entirely and runs the development boot.
3. Every other start tries `SCM` dispatch. `StartServiceCtrlDispatcherW`
   answering `ERROR_FAILED_SERVICE_CONTROLLER_CONNECT` (1063) means the
   process was not started by the SCM: it falls back to console mode.
4. The boot guard (`boot::validate`) is the declared equivalent of Linux's
   pid-1 + `/proc/cmdline` check:
   - **service** — a guest by construction, no marker required;
   - **console** — requires `COORWORKER_ENVD=1`, and refuses the builtin
     SYSTEM account;
   - **successor** (`--resume-handover`) — a guest by construction, so it
     skips the console marker;
   - **development** — must bind loopback and must not be SYSTEM.
5. Service and console guests keep the oracle's `0.0.0.0:49983` default bind;
   only development keeps the loopback-ephemeral default.
6. The daemon builds the shared `app_with_execution` router over one
   `WindowsProcessRuntime`; a runtime-construction failure logs and leaves the
   filesystem/HTTP surfaces serving with the process procedures on the
   router's 404 fallback.
7. Serves through the shared `http1::serve_gated` with an accept gate that the
   upgrade choreography can close without dropping in-flight connections.

`run()` points missing standard handles at the NUL device before any
diagnostic. A service has no console, and the successor it spawns inherits
none (the handle-list attribute passes only the readiness pipe), while Rust's
`println!`/`eprintln!` panic on a failed write. Console-less processes report
through the event log (`RegisterEventSourceW`/`ReportEventW`, source
`coorworker-envd`); console-attached processes keep stderr.

### 8.2 Service lifecycle

`src/platform/windows/service.rs` owns the SCM contract:

- `StartServiceCtrlDispatcherW` registers the `coorworker-envd` service table;
- `ServiceMain` registers `RegisterServiceCtrlHandlerExW`, reports
  `START_PENDING` (checkpoint 1, 30 s wait hint) then `RUNNING` (accepting
  STOP and SHUTDOWN), builds the multi-thread runtime and blocks on the serve
  future;
- STOP and SHUTDOWN report `STOP_PENDING` and flip a `watch` channel the
  serve future waits on; the handler never blocks the SCM. INTERROGATE
  re-reports the current state without stopping;
- when the serve future returns (including a failed boot), `ServiceMain`
  reports `STOPPED`.

A successor spawned by an outgoing envd is not SCM-registered: the outgoing
service reports `STOPPED` and exits after the successor is serving (declared
deviation; see §8.3).

### 8.3 Live upgrade

The pinned `HandoverState` protobuf stays byte-identical to Linux (the Linux
schema gate applies unchanged). Windows-only data travels in an `ENVDWIN1`
sidecar next to the blob. The outgoing choreography (`do_upgrade` in
`entry.rs`):

1. refuse a `newBin` other than `%ProgramData%\e2b\envd.next.exe`;
2. take `WindowsResourceBackend::freeze_workload_hold(2 s)` — the same strict
   sweep as Linux, with the freeze permit held for the whole handover, so a
   workload that did not stop refuses the swap with the shared
   `WorkloadNotFrozen` text;
3. export the retention cache (`RetainedExit` with each remaining TTL) and the
   live exec context (`handover_defaults`), and write the blob to
   `%ProgramData%\e2b\envd-handover.pb`;
4. create a readiness pipe and a `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` holding
   only its write end; spawn the successor with `CreateProcessW` **suspended**,
   duplicating the listening socket with `WSADuplicateSocketW` for the
   successor PID; write the sidecar (magic, version, raw `WSAPROTOCOL_INFOW`,
   readiness handle, empty process table); `ResumeThread`;
5. wait for 'S' — the successor reconstructed the listener with `WSASocketW`;
6. close the accept gate (the listener and established connections stay
   alive) and wait for 'R' — the successor applied the blob, restored the
   retention cache, wired its own upgrade hook and is about to accept;
7. report `SERVICE_STOPPED` in service mode and exit 0 without an HTTP
   response. The orchestrator treats the dropped connection as success.

Every failure before 'R' terminates the unconfirmed successor, deletes the
sidecar, re-opens the accept gate, drops the freeze hold and thaws; the old
envd keeps serving and the `/upgrade` request answers 500.

Declared deviations (there is no Windows oracle):

- a live process table cannot cross processes: each per-process Job is
  kill-on-close, so children end with the outgoing daemon and the successor's
  sidecar process table is empty (reserved by the versioned format);
- filesystem watchers and the mount ledger are not carried;
- the successor PID differs from the parent's, and the successor is not
  SCM-registered;
- the successor's startup is observably asynchronous (two readiness bytes)
  instead of Linux's atomic `execve`;
- a running image cannot be deleted, so staging renames it to
  `<name>.old-<pid>`; the successor deletes stale images on startup, which for
  the outgoing process's own renamed image only succeeds on a later boot;
- freeze-before-upgrade suspends threads, not cgroups.

## 9. Security model

- The daemon runs as a dedicated low-privilege service account; destructive
  operations are bounded by the Job Object and ACLs.
- Tokens are stored with DPAPI, never in the image, logs, or command lines.
  Provisioning is a user-scope DPAPI blob at
  `%ProgramData%\coorworker\envd\metadata.bin` containing the camelCase JSON
  document `{accessToken, instanceId, envId, address}`; only the service
  account and administrators may write it. `/init` compares
  `SHA-512(accessToken)` (lowercase hex) with the request token exactly as the
  Linux MMDS path does. A missing file means "nothing provisioned" and keeps
  the first `/init` permissive; a corrupt or unreadable file degrades to the
  same state (the token is never logged).
- `user` impersonation requires explicit credentials; the daemon never
  elevates implicitly.
- No model/provider credentials are ever placed in the guest, matching the
  Linux policy.

## 10. Validation strategy

1. **Schema conformance**: every response validated against the pinned
   protobuf/OpenAPI schemas on both platforms.
2. **Contract tests per endpoint**: one Windows test per REST path and per
   Connect procedure, asserting status, headers, body schema and the
   documented Windows semantics (including error paths).
3. **Linux regressions**: the full parity gate set re-runs after every shared
   refactor; any identity loss blocks the step.
4. **SDK matrix**: exercise the official e2b SDKs against a Windows sandbox;
   path syntax and `owner`/`mode` assumptions are documented per SDK.
5. **No differential claim**: Windows results are contract evidence only.

## 11. P0 preparation checklist

Status: all P0 items are done, including the Linux gate rerun.

- [x] Create `src/platform/mod.rs`; move the Linux backends under
  `src/platform/linux/` with `git mv` only — no code edits in the moved files
  (one comment's path was updated). `process/`, `cgroups/`, `filesystem/`,
  `port/`, `cpus/`, `mmds/` moved; `lib.rs` re-exports them under
  `#[cfg(target_os = "linux")]`, so every existing `crate::<backend>::…` path
  is unchanged. The daemon binary entry point moved to `src/main_linux.rs`
  behind a target dispatcher.
- [x] Define the seven traits with the daemon's existing error types; no
  `dyn` dispatch on Linux, no behavioural change. `ProcessBackend`,
  `ResourceBackend`, `FsBackend`, `WatcherBackend`, `PortScanBackend`,
  `ClockBackend`, `MetadataBackend` live in `src/platform/mod.rs`; Linux keeps
  calling its backends directly until the P1 adapters exist.
- [x] Add `[target.'cfg(windows)'.dependencies] windows-sys` (Win32 bindings,
  pinned); Linux dependency set unchanged. Pinned to `=0.61.2`, a version
  already present in `Cargo.lock`; `nix`, `xattr`, `inotify`,
  `tokio-rustls` and `rustls-native-certs` moved to
  `[target.'cfg(target_os = "linux")'.dependencies]`.
- [x] Add a Windows skeleton implementing every trait with `unsupported`
  errors so the crate compiles for `x86_64-pc-windows-msvc`. Verified:
  `cargo check --target x86_64-pc-windows-msvc --lib --bins` and
  `cargo build --target x86_64-pc-windows-msvc --lib` both succeed on a Linux
  host. Linking the binary needs an MSVC linker, so that step belongs to a
  Windows toolchain host.
- [x] CI: the Linux parity gates are unchanged; a new Windows job runs
  `cargo check --target x86_64-pc-windows-msvc` plus the shared-crate build.
  `.github/workflows/ci.yml` adds a Linux job (build, test, clippy, fmt, notice
  bundle) and the Windows check job. The external oracle gates stay in the
  workspace parity suite, not in this repository.
- [x] Re-run the full Linux parity gate set on the refactor candidate and
  record the evidence; any loss of identity blocks P1. Candidate sha
  `c036e19540ccb90a757e373de56ee4fc22bf7a91552cbfbc1173aee335f2c323`
  (`-version 0.10.2`, `-commit 51083a83…`): schema (73 cases), malformed
  (110+18+174+8+24 groups), HTTP/1 edges (71/71) and both upgrade modes are
  IDENTICAL against the oracle; conformance (`--expanded --repeat 2`),
  freeze, guestprobe, MMDS, CPUs, OOM global/cgroup/group and both NFS
  scenarios are `passed=true`. Gate documents and status logs are in
  `/tmp/envd-gates-c036e195/`; the workspace campaign plan records the
  artifact directories and the two invocation fixes (NFS server ownership,
  `socat` guest-bin name and SONAME guest libs).

## 12. Milestones

| Phase | Deliverable | Acceptance |
| --- | --- | --- |
| P0 | Platform traits; Linux modules moved verbatim; Windows build skeleton | All Linux gates IDENTICAL on the same candidate; `cargo build --target x86_64-pc-windows-msvc` succeeds for the shared crate. |
| P1 | Windows execution backend: files CRUD, process Start/List/Connect/Update/input/signal, ConPTY, watchers | Contract tests green for those endpoints; errors match the documented codes. |
| P2 | Resource backend: Job limits, freeze/unfreeze, collapse/fsfreeze responses, metrics, OOM notifications | `/metrics`, freeze and collapse contract tests green. |
| P3 | Entry point/service lifecycle, upgrade endpoint, token/init effects, port forwarding | Live-upgrade contract test green; the daemon serves as an SCM service, a console guest, and a successor; the loopback forwarder mirrors the oracle's scan/refresh state machine with an in-process proxy. |
| P4 | Windows guest image + Cloud Hypervisor backend + tool bridge | End-to-end: boot Windows guest, run a tool through the sandbox stack. |
| P5 | SDK matrix, security review, performance/boot baselines | Release checklist green. |

### P1 progress

- **Filesystem backend (2026-11-03)**: `WindowsFsBackend` implements `stat`,
  `read_dir`, `create_dir`, `remove`, and `rename` on `std::fs`. `stat`
  synthesizes `mode`/`permissions` from the read-only attribute and the entry
  kind, resolves `owner`/`group` from the object's security descriptor
  (`GetNamedSecurityInfoW` + `LookupAccountSidW`, with SID-string and `None`
  fallbacks so the value is never empty), reports the resolved reparse-point
  target with the `\\?\` prefix stripped, and returns an empty `metadata`
  map. Declared difference: Windows has no POSIX special bits, so
  `permissions` renders only the `r`/`w`/`x` triples. Unit tests live in
  `src/platform/windows/fs.rs`; the `windows-test` CI job runs them on
  `windows-latest` with `cargo test --lib` (run `41585352844`: 6 passed,
  including all five filesystem tests).
- **Linux evidence for this slice**: candidate sha
  `ca97781c8b08c74b8cf30ca05a09b3f94c23ca7eabf551bfc7a6286c91f7bb4d` — the
  schema, malformed, HTTP/1 edge and both upgrade documents are IDENTICAL to
  the oracle, and conformance, freeze, guestprobe, MMDS, CPUs, OOM and NFS
  all pass (`/tmp/envd-gates-ca97781/`). The Windows side still makes no
  differential claim.
- **HTTP surface and contract tests (2026-11-03)**: the platform-neutral
  HTTP stack moved out of the Linux daemon into `src/http/` (connection-level
  `Expect`/`OPTIONS *` checks, rs/cors, the chi/connect-go frontdoor,
  authorization, `/health`, `/envs`) and `src/files_http.rs` (`GET /files`).
  Both daemons mount that same code, so the wire behaviour is one
  implementation rather than two. `src/daemon_windows.rs` serves `/health`,
  `/envs`, `/init` and `GET /files` through it, with these documented
  deviations:
  - no MMDS token hash: `/init` authorizes against the installed token only;
  - `caBundle`, `hyperloopIP`, `cpuCount`, `lifecycleID` and `volumeMounts`
    are validated the way Go validates them and then have no effect;
    `timestamp` and `thaw` do have an effect (§6.2, §8);
  - `X-Envd-Memory` reports `{"request":0,"low":0,"floor":0,"partial":true}`;
  - the builtin default user is the service account (`USERNAME`), because
    Windows has no `root`; `defaultUser: "root"` therefore fails account
    resolution with the same 401 an unknown Linux user produces.
  `POST /files`, `/files/compose` and the other REST operations answer 501
  with a message naming this document; the 17 Connect procedures answer
  connect-go's 404 `unimplemented` JSON error with the same message.
  Contract tests live in `src/daemon_windows.rs` (11 tests) and drive a real
  loopback listener: 204 `/health` with the empty content-type header, the
  401 JSON envelope, `/init` token install and `X-Envd-*` headers (error
  responses included), file read and missing-path errors, chi 404/405, CORS
  preflight, raw-socket `OPTIONS *`/failing-`Expect` checks, and the 501/404
  gap responses. The `windows-test` job ran them natively on
  `windows-latest` in run `41590760185`: 29 tests passed (the 11 contract
  tests plus the existing backend tests).
- **Linux evidence for the HTTP slice**: candidate sha
  `5eb944656b644c4f42f392c0dbfe9e517e355430a70979b27e945f9493ac188d`. The
  schema, malformed, HTTP/1 edge and both upgrade documents are IDENTICAL to
  the oracle; conformance, freeze, guestprobe, MMDS, CPUs, OOM
  global/cgroup/group and both NFS scenarios are `passed=true`. Status log and
  document copies: `/tmp/envd-gates-5eb94465/`. Linux suite: 814 tests, 0
  failed; clippy `-D warnings` and fmt clean; the notice bundle still matches
  the lockfile.
- **`POST /files` (2026-11-03)**: the upload flow moved from the Linux daemon
  to `src/files_upload.rs` and is generic over `ConfigurationState`, so both
  daemons mount the same decoder, multipart scanner, alias check and response
  shape. Its platform-owned parts sit behind `src/platform/file_writes.rs`
  (shared `user.e2b.*` validation policy) with one adapter per platform:
  Linux delegates to the existing `filesystem::{paths, metadata}`
  (chown/fchown, `0666` creation mode, `ENOSPC`/`EDQUOT` mapping, xattr
  metadata), so the parity path is unchanged. Windows creates parents one
  component at a time, creates/truncates with the service account's ACL
  instead of chowning, identifies files for the multipart same-path refusal
  with `GetFileInformationByHandle` (volume serial + file index), maps the
  Windows disk-full/quota errors to the same 507 responses, and answers
  `Unsupported` for `user.e2b.*` writes — the shared flow logs and continues,
  exactly as it does on a Linux filesystem without xattr support.
  `src/daemon_windows.rs` now routes `POST /files` to that shared handler.
  Contract tests (four new, in `src/daemon_windows.rs`) drive a real loopback
  listener: raw upload into a missing directory then read it back through
  `GET /files`, a two-part multipart upload, the directory-target /
  invalid-metadata / unsupported-content-type 400 shapes, and the 401 without
  a token. The `windows-test` job ran them natively on `windows-latest` in run
  `41657417627`: 33 tests passed (the 29 existing plus the four upload
  tests).
- **Linux evidence for the `POST /files` slice**: candidate sha
  `bccb04e9e6b38d2dbb3d3e7eddbb7b05fd05738e4cfcb673d37e070ae8eec3e7`. The
  schema, malformed, HTTP/1 edge and both upgrade documents are IDENTICAL to
  the oracle; conformance, freeze, guestprobe, MMDS, CPUs, OOM
  global/cgroup/group and both NFS scenarios are `passed=true`. Status log and
  document copies: `/tmp/envd-gates-bccb04e9/`. Linux suite: 814 tests, 0
  failed; clippy `-D warnings` (Linux all-targets and Windows `--lib`) and fmt
  clean.
- **`POST /files/compose` (2026-11-04)**: the compose handler and its
  incremental Go JSON scanner moved out of the Linux-only daemon into
  `src/files_compose.rs` and `src/compose_decode.rs`; both daemons mount the
  same handler, so the decoder, validation order, error strings, HTML escaping
  and response framing are the moved Linux code. The OS-owned operations sit
  behind `src/platform/compose_files.rs`: Linux keeps the moved-verbatim
  `/dev/urandom`, `0666` temp mode, `0755` created directories with chown,
  `copy_file_range` with the 32 KiB fallback, descriptor close and lowercased
  errno text. `src/platform/windows/compose_files.rs` implements the same
  facade with real Win32: `BCryptGenRandom` (adding
  `Win32_Security_Cryptography`), exclusive temp creation sharing
  read/write/delete, a drive/UNC-aware lexical parent, `fs::create_dir`
  without chown, a no-op `adopt_owner`, `GetFileInformationByHandle` identity
  (volume serial + file index, optionally `FILE_FLAG_OPEN_REPARSE_POINT`), a
  1 MiB buffered copy with the oracle's `read {source}` / `write {temp}`
  operation strings, `FlushFileBuffers` + `CloseHandle`, Windows
  system-message `errno_text`, and delegation to the shared disk-full
  classifier. `src/daemon_windows.rs` routes `/files/compose` to the shared
  handler; the 501 gap test moved to a still-unimplemented endpoint
  (`POST /freeze`). Four unit tests cover the Windows parent, identity,
  buffered copy and 507 classifier; seven contract tests plus the updated gap
  test cover compose order and source deletion, overwrite and
  destination-symlink replacement, validation order and file preservation,
  frozen malformed bodies and trailing bytes, token enforcement, unknown
  users, and temp cleanup after a copy failure. **Windows behaviour is
  contract-tested, not differentially verified** (no Windows oracle exists).
  Declared deviations (Windows only):
  - no POSIX mode/uid/gid: the temp inherits the destination directory's ACL,
    the service account is its creator, and `adopt_owner` is a no-op; a later
    `EntryInfo` reports the synthesized mode and SID-derived owner/group;
  - OS failures carry Windows system-message tails instead of lowercased
    errno text; status codes and the JSON envelope are unchanged;
  - no `copy_file_range`, so the operation text
    `write <temp>: copy_file_range` never appears (buffered read/write is
    used);
  - Win32 may collapse `..` lexically where Linux requires each component to
    exist;
  - a read-only source cannot be unlinked and remains after a successful
    compose (source cleanup is best effort);
  - renaming over a destination another process holds open without
    `FILE_SHARE_DELETE` returns a sharing-violation 500 where Linux succeeds;
  - `ERROR_DISK_FULL` and `ERROR_HANDLE_DISK_FULL` map to 507, while
    `ERROR_DISK_QUOTA_EXCEEDED` stays 500 because Linux compose special-cases
    only `ENOSPC`;
  - long relative paths can still fail near `MAX_PATH`.
  The Windows tests run only in the `windows-test` job on `windows-latest`
  (`cargo test --lib`); no Windows run id is recorded for this slice yet.
- **Filesystem Connect slice (2026-11-04)**: the nine Filesystem procedures
  are implemented on Windows. `src/platform/windows/fs_runtime.rs` provides
  the `FilesystemService` runtime with the Linux resolution and error
  semantics (lstat-shaped Stat, MakeDir's `already_exists`/`invalid_argument`
  order, Move's destination-parents side effect, the 255-link ListDir walk
  with alias paths, idempotent recursive Remove) over Win32 calls, plus a
  published Win32-to-errno text table. `src/platform/windows/watchers.rs`
  provides the watcher registry and provider over overlapped
  `ReadDirectoryChangesW` (64 KiB buffer, dedicated thread, stop event,
  `CancelIoEx` + `GetOverlappedResult` drain + join on drop, event mapping,
  network-mount classification). `src/daemon_windows.rs` mounts the shared
  `crate::filesystem_rpc::router` through `transport::rpc_service`, so
  production and contract tests serve the same Connect surface; unknown
  process procedures now answer the router's 404
  `unimplemented`/`method not found: <path>`. The declared deviations are in
  §5.2. Contract tests drive a real loopback/oneshot surface: Stat value
  synthesis and messages, MakeDir, Move, ListDir, Remove (including a
  junction), token/Basic auth, watcher create/drain/remove, WatchDir Start/
  Create/Write/Rename/keepalive framing and handle release; unit tests cover
  the RDCW buffer mapping, `/` names, overflow mapping, network
  classification, error text, and the runtime walk/move/junction cases.
  **Windows behaviour is contract-tested, not differentially verified.**
- **Resource slice: metrics, freeze/unfreeze, collapse, fsfreeze (2026-11-04)**:
  the remaining five REST resource operations are served by
  `src/daemon_windows.rs` through real Win32 backends.
  `src/platform/windows/metrics.rs` builds the shared `Metrics` shape from
  `GetSystemTimes`, `GetActiveProcessorCount`, `GlobalMemoryStatusEx`,
  `GetPerformanceInfo` and `GetDiskFreeSpaceExW` using the shared `CpuTimes`
  and `disk_usage` helpers; `src/platform/windows/collapse.rs` implements the
  shared `Collapser` with `SetProcessWorkingSetSizeEx`,
  `GetProcessHeaps`/`HeapCompact` and a `VirtualQuery` scan;
  `src/platform/windows/fsfreeze.rs` implements the shared `FsFreezer` by
  opening the system volume (`GetWindowsDirectoryW` -> `GetVolumePathNameW` ->
  `\\.\C:`) and calling `FlushFileBuffers`. `src/platform/windows/resource.rs`
  (freeze/unfreeze over per-thread `SuspendThread`) was already in place; this
  slice adds the `/init` generation-guarded thaw and the metrics/collapse/
  fsfreeze wiring. Contract tests cover the twelve-field metrics shape and its
  headers, the GET-only method matrix with auth precedence, the six-field
  collapse shape, the bodiless fsfreeze/fsthaw 204s and their 500 envelope,
  token enforcement, and `/init` thawing unless `thaw: "skip"`. Unit tests
  cover the shared CPU formula, region-window counting and cancellation, the
  volume-device mapping, the guarded thaw and the freeze-lock serialization.
  **Windows behaviour is contract-tested, not differentially verified**; the
  new tests were type-checked against the Windows target on this Linux host
  and cannot run without a Windows runner. Declared deviations:
  - fsfreeze is a volume flush, not a FIFREEZE barrier; there is no VSS
    snapshot (the COM interfaces are not in `windows-sys`);
  - `/fsthaw` repeats the flush and is a no-op, because nothing is held;
  - collapse trims the working set and compacts heaps but cannot migrate base
    pages into 2 MiB pages: `collapsed == alreadyHuge == 0` and
    `skipped == chunks` (regions/chunks are real scan values);
  - the four CPU hotplug fields are 0 (no CPU manager);
  - `/metrics` omits `oom_kills` (no known source in this slice);
  - Win32 error text replaces Go errno text in the 500 envelopes; status,
    headers and framing are unchanged;
  - volume flush needs write access to the system volume, so an elevated
    service account is required (the low-privilege service account in §9 may
    receive a documented 500);
  - a daemon crash while processes are suspended leaves them frozen until
    reboot: the suspended-tid set is not persisted (declared limitation, not
    a recovery guarantee). A live daemon is guarded by the same ten-minute
    thaw watchdog as Linux (`freeze.go:206-216`): a sweep that suspended
    something arms a generation-guarded backstop that resumes the recorded
    threads if no `/unfreeze`, thawing `/init`, or later watch event releases
    them; only a clean thaw disarms it, and a partially failed thaw keeps the
    unresolved thread records and the spawn gate for the retry;
  - `preFrozen` counts pre-suspended processes (distinct pids), not threads,
    matching Linux's `guest_frozen` cgroup count;
  - a spawn-drain refusal reports `failed: 2` (the two workload classes) with
    zero walk counters and the requested mode, matching the Linux static
    sweep rather than the Windows target list;
  - `POST /init` answers `X-Envd-Thaw: skip` when the thaw is withheld. The
    `X-Envd-Freeze-Audit` header is not emitted: the audit classifies frozen
    cgroups, and there is no cgroup on Windows, so its absence is declared
    rather than emulated. The clock slice is implemented (§6.2), so
    `X-Envd-Init-Clock-Correction` is present.
- **Process Connect slice (2026-11-04)**: the eight Process procedures are
  served by the shared `src/rpc/process.rs` adapter over
  `WindowsProcessRuntime`. The runtime's registry, connect/retention and
  start ordering are the Linux algorithms; `CreateProcessW` is created
  suspended, assigned to the sandbox Job first and then to a per-process
  kill-on-close Job, and resumed. `src/daemon_windows.rs` now owns
  `process_logger` (`Logger::none()`), implements `ProcessRpcState`, and mounts
  `crate::rpc::process::router` merged with the filesystem router under the
  same `transport::rpc_service` fallback in `app_with_execution`;
  `app()` builds and retains one `WindowsProcessRuntime` from the service
  account (`current_user_name` + `USERPROFILE`), the `Configuration`
  defaults/environment and `PATH`, the daemon's sandbox `JobHandle`, and the
  workload registry shared with `WindowsResourceBackend::with_registry` (the
  sandbox `JobHandle` is attached with `with_job`), so a
  `/freeze` sweep sees started processes. If the runtime cannot be built (Job
  creation failed, the service account does not resolve, or the configuration
  is locked), `app()` logs the reason and keeps the shared router's 404
  `unimplemented` fallback instead of panicking.
  ```text
  src/daemon_windows.rs            process_logger, ProcessRpcState,
                                   app_with_execution, runtime construction
  src/platform/windows/account.rs  pub(crate) is_current_user (SID compare)
  src/daemon_windows/contract_tests/process.rs   19 contract tests
  src/platform/windows/process/{pipe,spawn}.rs   unit tests
  ```
  Declared deviations (full list in §4.2): no `/bin/sh` wrapper; a missing
  executable is `invalid_argument` rather than exit 127; no
  `oom_score_adj`/`nice`/`ionice` wrapper; SIGTERM is best-effort
  Ctrl-Break/ETX/WM_CLOSE plus a grace `TerminateProcess`; ConPTY-encoded PTY
  bytes and `i16`-clamped resize; synthesized signal `EndEvent`s; a
  different-user start needs credentials that are not plumbed, so it is
  `failed_precondition`; `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` reaps
  descendants; and there is no live-upgrade adoption. **Windows behaviour is
  contract-tested, not differentially verified** (there is no Windows
  oracle). Contract tests drive a real loopback listener over the production
  `app()`: Start/List/config echo, Connect without replay plus retained
  exits, selector/authentication refusals, stdin/PTY channel rules,
  StreamInput ordering and wrappers, CloseStdin idempotence, Update
  no-op/PTY/after-exit paths, SIGKILL/SIGTERM end synthesis, invalid signals,
  keepalive, PTY-only events and ConPTY close, and Job
  kill-on-close descendant reaping. Unit tests cover the closed-pipe
  `FailedPrecondition` classification and `CommandLineToArgvW` round-trip
  quoting plus NUL rejection. The tests were type-checked against
  `x86_64-pc-windows-msvc` on this Linux host and cannot run without a
  Windows runner. Linux suite: 817 passed / 0 failed.
- **Entry point and live upgrade (2026-11-04)**: `src/platform/windows/entry.rs`
  now serves the daemon (SCM service → console fallback, marked console guest,
  loopback-only development), and every REST operation including
  `POST /upgrade` is wired. `src/platform/windows/service.rs` implements the
  SCM lifecycle, `log.rs` the event-log sink, `boot.rs` the launch-mode guard,
  and `handover.rs` the sidecar, suspended successor spawn and inherited
  listener adoption described in §8. The pinned blob stays byte-identical to
  Linux; only the `ENVDWIN1` sidecar is platform-specific. Declared
  deviations are listed in §8.3. Evidence: the Windows target type-checks
  (`cargo check`/`clippy -D warnings` for `x86_64-pc-windows-msvc --lib
  --bins`) and its tests compile up to the documented missing `link.exe`;
  the Linux suite is 818 passed / 0 failed. Windows side: contract evidence
  only, no differential claim.
- **Native Windows validation (2026-11-04)**: the whole library suite ran
  natively inside a Windows 11 guest (`cargo test -j4 --lib --
  --test-threads=1` in an interactive session, `PROTOC` set), which is the
  first time these tests executed on Windows rather than only compiling.
  275 passed / 0 failed, twice in a row. The run found four real defects that
  neither the Linux host nor type-checking could see:
  - `ReadDirectoryChangesW` on an overlapped directory handle returns nonzero
    when the request is merely *queued*; `lpBytesReturned` is undefined until
    the completion is delivered. The watcher read the count immediately and
    always saw `ERROR_IO_INCOMPLETE` (996). It now waits on the stop event and
    the overlapped event and then calls `GetOverlappedResult`.
  - `seckey`'s non-Unix fallback toggles the token page between
    `PAGE_NOACCESS` and `PAGE_READONLY` around every borrow, and a
    `VirtualProtect` to `PAGE_NOACCESS` drops the page's `VirtualLock`
    (`VirtualUnlock` then reports `ERROR_NOT_LOCKED`, and `region`'s
    `LockGuard::drop` panics). Windows now owns a fixed `Box<[u8]>` token
    buffer whose protection never changes, locked once through the refcounted
    `PageLocks` registry.
  - A pseudoconsole child without an explicit standard-handle triple inherits
    the daemon's redirected handles: the PTY stream carried only conhost's
    86-byte init sequence and every child byte landed in the daemon's stdout
    (verified: `echo X` reached the caller's log, `echo X>CONOUT$` reached the
    stream, and `mode con` reported the caller's console). PTY children are now
    created with `STARTF_USESTDHANDLES` and `INVALID_HANDLE_VALUE` for all
    three handles, so conhost's console handles are used; `mode con` then
    reports the requested size.
  - `mode con` is localized, so the resize contract test parsed nothing on a
    zh-CN guest. It now reads the report structurally (the first two
    `<label>: <number>` pairs are the line and column counts) and asserts
    `[40, 100]` after the `Update`.
  Two smaller fixes came out of the same run: the "vanished pid" tests in
  `suspend.rs`/`resource.rs` now use a never-existing pid (`u32::MAX - 3`)
  instead of a reused one, and a Ctrl-Break delivered while the child is still
  in loader initialization aborts it with `STATUS_DLL_INIT_FAILED`
  (`0xC0000142`) rather than `STATUS_CONTROL_C_EXIT`. The end synthesis maps
  that code to `signal: terminated` only when a Term signal was accepted for
  that child (`Entry::term_accepted`); an unsolicited loader failure keeps its
  raw exit status. Declared deviations: the Windows token buffer keeps a fixed
  protection instead of `seckey`'s protection toggling, and a Term that lands
  during loader initialization is reported as `signal: terminated` even though
  the child exits with `STATUS_DLL_INIT_FAILED`. Windows behaviour is
  contract-tested, not differentially verified (there is no Windows oracle).
- **Loopback port forwarding, the `/init` clock, and two native defeats
  (2026-11-04)**: the last two platform slices. `src/platform/port/` now
  carries the platform-neutral listener scan both daemons share (`ScanSource`,
  `Addr`, `ConnectionStat`, `Scanner`, `ScannerSubscriber`/`ScannerFilter`,
  `ForwardedPort`, `PortState`, `DEFAULT_GATEWAY_IP`); only the `/proc` source
  and the socat spawn stay Linux-specific, and only the in-process proxy stays
  Windows-specific (§6.1). `/init` now decodes `timestamp` with the oracle's
  raw-bytes rules, applies the handler-delay correction and the newest-wins
  gate, and steps the guest clock through `SetSystemTime` (§6.2). This
  supersedes the earlier "`X-Envd-Init-Clock-Correction` is not emitted"
  statement; only `X-Envd-Freeze-Audit` stays absent.
  The native runs found two defects that neither the Linux host nor the
  Windows type-check could see:
  - the `SYSTEMTIME` conversion divided the sub-second tick remainder by ten
    instead of 10 000, so every step carried a bogus millisecond field
    (`123` became `57920` after the `u16` cast). It now divides by
    `TICKS_PER_MILLISECOND`; the known-instant and pre-epoch tests pin it.
  - the `/init` decoder walked a `serde_json::Value`, which decodes JSON
    escapes before the timestamp parse. Go hands `time.Time` and `SecureToken`
    the raw bytes, so an escaped timestamp or token the oracle answers 400 for
    was accepted here (204). The decoder now captures those two members from
    the raw body, accepts a literal `null` body as the zero struct the way Go
    does, and type-checks the Go-typed fields it ignores (`cpuCount`,
    `volumeMounts`, `caBundle`, `hyperloopIP`, `lifecycleID`) so a body Go
    rejects is a 400 on Windows too.
  A native flake was also fixed: the StreamInput ordering test started
  `more`, closed its stdin and immediately asserted the input rules; `more`
  exits on the closed stdin and can be reaped first, so one run answered
  `not_found` instead. The child is now `ping -n 3`, which outlives the round
  trips and still ends cleanly.
  Evidence: the Windows 11 guest ran `cargo test -j4 --lib --
  --test-threads=1` twice back to back, 305 passed / 0 failed both times
  (exit 0). Linux host: `cargo fmt --check` clean; `cargo check --locked
  --all-targets` clean; the Windows target `check`/`clippy -D warnings` on
  `--lib --bins` clean; Linux `clippy --all-targets -D warnings` clean;
  `cargo test -j4 --offline` 819 passed / 0 failed; the notices check matches
  the locked graph. Windows behaviour remains contract evidence — there is no
  Windows oracle, so no differential claim is made.
- **Still open**: the Windows guest image + Cloud Hypervisor backend (P4) and
  the SDK matrix (P5). Every REST operation and all seventeen Connect
  procedures are implemented and served. The metrics, freeze/unfreeze,
  collapse and fsfreeze resource backends are implemented and served. The
  port-scan backend (GetExtendedTcpTable) and the loopback port forwarder are
  implemented and wired into the daemon. The metadata/token backend is
  implemented (DPAPI). The
  shallow `WatcherBackend` trait adapter has no Windows implementation: the
  served watcher surface is `WatcherRegistry` over
  `ReadDirectoryChangesW`, and the former `WindowsWatcherBackend`
  `Unsupported` stub was deleted rather than left advertised. The
  `ProcessBackend` P0 trait is implemented as thin delegates over
  `WindowsProcessRuntime`.

Rough effort: 30–45 person-weeks (6–9 months for one engineer; 3–5 months with
2–3 engineers). P0 alone is 2–3 weeks and calibrates the rest.

## 13. Open decisions

1. Freeze fidelity: full thread suspension vs. cooperative stop; both keep the
   API, neither is identical to cgroup v2.
2. `user` impersonation: require credentials, or restrict to service-account
   mapping.
3. NTFS extended attributes for `EntryInfo.metadata`: implement or return an
   empty map.
4. Windows licensing/activation ownership and image distribution.
5. AF_HYPERV availability across candidate VMMs; fallback to TCP on a private
   segment.
6. Boot-time SLO (tens of seconds vs. sub-second Linux) and image size
   (30–64 GB vs. MB initramfs).

## 14. Non-goals

- Byte-level parity with Go envd on Windows: no oracle exists.
- Firecracker support for Windows guests.
- Changing Linux behaviour, packaging, or parity claims.
