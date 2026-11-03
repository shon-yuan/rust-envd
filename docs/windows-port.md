# Windows sandbox port — evaluation and plan

Status: evaluation, 2026-11-03. Nothing in this document is implemented yet.
It scopes goal D: **a Windows guest sandbox that serves the complete existing
envd API**, while the Linux daemon keeps its byte-level parity with the pinned
Go oracle.

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
| `GET /metrics` | `GlobalMemoryStatusEx`, `GetSystemInfo`, `GetDiskFreeSpaceEx`, Job Object accounting, per-process CPU deltas | Same JSON fields (`ts`, `cpu_count`, `cpu_possible`, `cpu_target*`, `cpu_used_pct`, `mem_*`, `disk_*`, `oom_kills`). `cpu_target*` track our own Job/cpu-set requests; `oom_kills` is fed by Job Object memory-limit notifications. Values are Windows measurements; schema and presence rules unchanged, including omitting `oom_kills` where the Linux daemon omits it. |
| `POST /init` | Token and defaults setup, certificate install into the Windows certificate store, time check against host, SMB/NFS mount bookkeeping | Same request/response and `X-Envd-*` headers. Linux-only effects (cgroup manager, MMDS route, hyperloop) become documented no-ops with a log line; the response contract is unchanged. |
| `POST /freeze` | Suspend every thread of every tracked process (`NtSuspendProcess` with a documented fallback to a cooperative stop), re-verify with a query | Same body (`requested`, `frozen`, `audit`), same `maxWaitMs` semantics. A process that cannot be suspended is reported in `audit`, exactly as an uninterruptible Linux process is. |
| `POST /unfreeze` | Resume suspended threads | Same body and idempotence. |
| `POST /collapse` | No huge-page collapse on Windows | Same response schema; counters are reported as zero with the same field names. |
| `POST /fsfreeze` | Volume flush (`FlushFileBuffers` over the volume handle), VSS snapshot freeze when available | Same response schema. Semantics documented as "flush + optional VSS"; Linux ioctl parity is not claimed. |
| `POST /fsthaw` | VSS thaw when a snapshot was taken; no-op otherwise | Same response. |
| `GET /envs` | Process environment and defaults from the service account | Same shape. |
| `GET /files` | Win32 file read, `Range`, conditional requests, directory handling | Same headers (including `Content-Range`/`Content-Length`/`ETag` behaviour) and error bodies. |
| `POST /files` | Multipart upload, gzip members, overwrite, metadata | Same body/framing; paths accept Windows drive and UNC forms. |
| `POST /files/compose` | Sequential composition with the same validation order and error texts | Same. |
| `POST /upgrade` | Spawn the new binary, transfer the listening socket with `WSADuplicateSocket`, pass the handover blob, retire the old process | Endpoint contract unchanged. Windows has no `exec`, so the PID changes; the handover blob carries process metadata instead of open file descriptors. |
| `OPTIONS *`, CORS preflight | Shared code | Unchanged. |

### 4.2 Connect procedures

| Procedure | Windows implementation | Contract notes |
| --- | --- | --- |
| `process.Process/List` | Enumerate Job Object processes | Same `ProcessInfo` shape; `config` echoed from what was started. |
| `process.Process/Connect` | Event stream over Job notifications and pipe readers | Same `ProcessEvent` oneof, keepalive, and terminal `EndEvent`. |
| `process.Process/Start` | `CreateProcess` (+ `LogonUser`/`CreateProcessAsUser` when `user` is set) with Job Object assignment; ConPTY when `pty` is present | Same request/validation order and error codes. `user` requires credentials or a service account mapping; otherwise a documented `failed_precondition` error, matching how the Linux daemon rejects unknown users. |
| `process.Process/Update` | `ResizePseudoConsole` | Same request/response; PTY-only error path preserved. |
| `process.Process/StreamInput`, `SendInput` | Pipe writes | Same framing and ordering. |
| `process.Process/CloseStdin` | Close the stdin handle | Same semantics, including the PTY rejection. |
| `process.Process/SendSignal` | `SIGKILL` → `TerminateJobObject`/`TerminateProcess`; `SIGTERM` → `GenerateConsoleCtrlEvent` (or `WM_CLOSE` for GUI processes) then terminate after a grace period | Same request/response. Delivery mechanism differs; the terminal `EndEvent` still arrives. |
| `filesystem.Filesystem/Stat`, `ListDir` | `GetFileAttributesEx`, `FindFirstFileEx`; reparse points for symlinks | Same `EntryInfo` fields. See 5.2 for value synthesis. |
| `MakeDir`, `Move`, `Remove` | Win32 equivalents, same validation order and error mapping | Same. |
| `WatchDir`, `CreateWatcher`, `GetWatcherEvents`, `RemoveWatcher` | `ReadDirectoryChangesW` per watcher, same event batching contract | Same event schema; batching boundaries are documented as platform-defined where Linux inotify coalesces. |

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
    ├── net.rs      # GetExtendedTcpTable + in-process forwarder
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
| `mode` | Synthesized from attributes and ACL: read-only → `0444`/`0555`, writable → `0644`/`0755`; never zero. |
| `permissions` | Rendered from the same synthesized mode. |
| `owner`, `group` | Account name of the SID that owns the object (`DOMAIN\user`); group from the primary group SID. Never empty. |
| `metadata` | NTFS extended attributes under the same `user.e2b.` convention when readable; otherwise an empty map, never an error. |
| `symlink_target` | Reparse-point target; `is_symlink` from the reparse tag. |

Clients that compare `owner` to a Linux username will see a SID-derived name;
that is a documented value difference, not a missing API.

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

## 7. Tool bridge

The sandbox stack's guest tool agent currently runs over AF_VSOCK port 5000
and a guest-local UDS. On Windows it must run over AF_HYPERV (requires the
Hyper-V socket provider and a VMM that exposes it) or a private virtio-net
segment with TCP loopback plus named pipes for local subprocesses. This is a
separate crate from envd and is scoped as its own work item.

## 8. Live upgrade

Linux re-execs in place and passes file descriptors. Windows:

1. the new process starts with `--resume-handover` and a handover blob;
2. the old process duplicates the listening socket with `WSADuplicateSocket`;
3. the new process imports process metadata (PIDs, tags, exit statuses,
   watcher registrations) and reattaches to the same Job Objects;
4. the old process exits after the new one answers readiness.

PID identity changes, and freeze-before-upgrade is best-effort (thread
suspension). The endpoint's request/response contract is unchanged.

## 9. Security model

- The daemon runs as a dedicated low-privilege service account; destructive
  operations are bounded by the Job Object and ACLs.
- Tokens are stored with DPAPI, never in the image, logs, or command lines.
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

- [ ] Create `src/platform/mod.rs`; move the Linux backends under
  `src/platform/linux/` with `git mv` only — no code edits in the move commit.
- [ ] Define the seven traits with the daemon's existing error types; no
  `dyn` dispatch on Linux, no behavioural change.
- [ ] Add `[target.'cfg(windows)'.dependencies] windows-sys` (Win32 bindings,
  pinned); Linux dependency set unchanged.
- [ ] Add a Windows skeleton implementing every trait with `unsupported`
  errors so the crate compiles for `x86_64-pc-windows-msvc`.
- [ ] CI: the Linux parity gates are unchanged; a new Windows job runs
  `cargo check --target x86_64-pc-windows-msvc` plus the platform-neutral
  tests.
- [ ] Re-run the full Linux parity gate set on the refactor candidate and
  record the evidence; any loss of identity blocks P1.

## 12. Milestones

| Phase | Deliverable | Acceptance |
| --- | --- | --- |
| P0 | Platform traits; Linux modules moved verbatim; Windows build skeleton | All Linux gates IDENTICAL on the same candidate; `cargo build --target x86_64-pc-windows-msvc` succeeds for the shared crate. |
| P1 | Windows execution backend: files CRUD, process Start/List/Connect/Update/input/signal, ConPTY, watchers | Contract tests green for those endpoints; errors match the documented codes. |
| P2 | Resource backend: Job limits, freeze/unfreeze, collapse/fsfreeze responses, metrics, OOM notifications | `/metrics`, freeze and collapse contract tests green. |
| P3 | Upgrade endpoint, token/init effects, port forwarding | Live-upgrade contract test green. |
| P4 | Windows guest image + Cloud Hypervisor backend + tool bridge | End-to-end: boot Windows guest, run a tool through the sandbox stack. |
| P5 | SDK matrix, security review, performance/boot baselines | Release checklist green. |

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
