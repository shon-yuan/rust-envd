# Native CUA and historical extensions

The official envd surface remains frozen: 13 REST operations and 17 Connect
procedures at the pinned upstream release `0.10.2` (`51083a8`). CUA is an
additive native HTTP service in the existing daemon. Its four routes run
inside S0 routing and authentication; there is no extension mounting layer.
The implemented [native CUA contract](native-cua.md) has passed the local
verification matrix described below. Historical results remain separately
scoped to earlier revisions.

## Native interface

| Endpoint | Request | Response |
| --- | --- | --- |
| `GET /cua/tools` | No body | `{"tools":[{"name":"…","description":"…","input_schema":{…}}]}` |
| `POST /cua/call` | `{"name":"…","arguments":{…}}` | `{"success":true,"content":[…],"data":…}` |
| `GET /cua/health` | No body | Passive runtime availability and reason; no desktop probe or action. |
| `GET /cua/metrics` | No body | Bounded Prometheus tool counters, latency and runtime readiness. |

The call body must be an object with exactly `name` and `arguments`, and
`arguments` must be an object. JSON-RPC envelopes, notifications, batches and
unknown fields are rejected. Tool schemas use `input_schema`; image content
uses `mime_type`. Results carry no JSON-RPC `id`/`result` or MCP
`isError`/`structuredContent`. Tool-reported failures use `success:false`;
`data` preserves the driver's raw structured diagnostics. A failure without
structured data receives `{"code":"tool_error"}`. Discovery omits
`output_schema`: the vendor's normalized output schema does not describe every
raw diagnostic payload. Transport failures use the existing newline-terminated
S0 JSON error envelope with `code` and `message`.

| Status | Transport failure |
| --- | --- |
| 400 | Invalid JSON or arguments |
| 401 | Missing, uninitialized, ambiguous or invalid credentials |
| 404 | Unknown route or tool |
| 405 | Wrong method |
| 413 | Request too large |
| 415 | Unsupported media type |
| 429 | Execution already busy |
| 500 | Internal failure |
| 503 | Unavailable runtime |
| 504 | Execution deadline exceeded |

Requests are limited to 1 MiB and serialized responses to 32 MiB. The
execution deadline is 30 seconds. One execution slot stays held until the
underlying action actually finishes, including after an HTTP timeout or
client disconnect. A timeout does not guarantee cancellation of native OS
work. The desktop adapter uses the vendor's `work_scope::InvocationScope` to
track and drain finite blocking, async and raw-thread handoffs, including
nested work and panic cleanup. Markers are acquired before enqueueing, and
bound futures retain their caller-selected runtime. Finite GNOME compositor
requests preserve their absolute deadline and reject expired queued work
before polling or helper invocation; finite accessibility probes also carry
the scope. Persistent session resources and event loops stay under their
existing owners, outside the per-call drain. A wedged OS call can keep the
slot held until completion or process termination.

## Authentication and observation

All CUA paths, including `/cua`, unknown descendants and removed endpoints,
require completed envd initialization and a nonempty current
`X-Access-Token`. Exactly one unambiguous header value is required; duplicate,
comma-joined, empty or invalid values are rejected. Bearer, Basic and file
signatures cannot replace that header. Authentication runs in the existing S0
middleware before decoding a body or invoking a tool. Existing CORS preflight behavior remains unchanged
and cannot execute a tool. The original S0 paths, protobufs, authentication
exceptions and response contracts stay unchanged.

Credential rotation affects subsequent requests; an admitted action may
finish after rotation. Rejected credentials do not count as tool execution.
Both health and metrics require the same token. Health is passive availability
information, not evidence of successful screenshot/input/window actions.
CUA Prometheus metrics are separate from the unchanged S0 `/metrics` JSON
surface; worker supervision and restart metrics have no role in this API.

## Runtime and setup

The sole engine is envd's directly linked, in-process vendored desktop
registry. There is no MCP endpoint, initialization handshake, extension
framework, worker supervisor, helper transport or execution-mode selection.
Native desktop failures can terminate envd because execution shares its
process.

The default build enables optional `cua-desktop` dependencies. Linux and known
Windows desktop user sessions are eligible. Use the normal envd launch and
token provisioning procedure, then list tools through `GET /cua/tools` and
invoke them through `POST /cua/call`. No separate process, listener, transport
token or MCP client initialization is required. Model/provider credentials
belong in AgentHost.

Windows SCM-attached processes, Session 0, service successors in Session 0,
and unknown sessions report unavailable before constructing a desktop
registry. They neither start nor contact a helper. The S0 filesystem and
process services remain available. Use `--no-default-features` for the
desktop-free base/static build; CUA operations then report unavailable after
authentication. Desktop dependencies remain optional.

Remove old `COORWORKER_CUA_*` mode/worker/proxy configuration and
`CUA_ENV_TOKEN` from production launch environments: their presence is
rejected at startup with an actionable migration error. There is no legacy
compatibility mode. `/cua/mcp`, `/cua/info`, extension discovery and independent
`coorworker-cua` startup are removed. Earlier helper setup, restart budgets,
proxy credentials and Bearer-based examples below are historical evidence,
not deployment instructions for the native service.

## Linux window enumeration and vendor patches

Native Wayland enumeration remains opt-in through
`CUA_DRIVER_RS_ENABLE_WAYLAND=1`, `WAYLAND_DISPLAY`, `XDG_RUNTIME_DIR` and the
current `HYPRLAND_INSTANCE_SIGNATURE` for a Hyprland session. A stale
signature is an IPC failure. Without the opt-in, the registry uses its
existing X11 path; rootless XWayland with no X11 clients can correctly return
zero windows. These driver settings are distinct from the removed
`COORWORKER_CUA_*` execution configuration.

The local Hyprland patch accepts legacy numeric workspace IDs and 0.57 stable
workspace addresses, including named and special workspaces. Addresses take
precedence, display names do not establish identity, and empty workspaces are
excluded from visibility matching. A mapped window without a workspace
identity is an error. Full compositor window IDs, PID/geometry validation,
and hidden/DPMS visibility rules remain intact. The fallible dispatcher
preserves authoritative Hyprland/niri enumeration and blocking-task failures;
a successful empty enumeration still returns an empty list. The native HTTP
adapter uses `success:false` for tool failures rather than the older MCP
result envelope. Other internal callers retain the best-effort interface;
the generic Wayland/X11 fallback policy is unchanged.

These patches cover enumeration, not rotated multi-monitor capture/input.
Screen-size and action admission keep their existing checks. Patch provenance
and update obligations are in
[`vendor/cua-driver/PROVENANCE.md`](../vendor/cua-driver/PROVENANCE.md).
Earlier worker/MCP tests below do not establish native API acceptance.

## Licensing

Only the MIT crates are vendored: `cua-driver-core`, `cua-driver-contract`,
`platform-linux`, `platform-windows`, `cursor-overlay`, `cua-cursor-motion`
and `pip-preview`. The AGPL-3.0-only `cua-perception` and FSL-1.1-MIT
`libs/cua-spacesd` remain excluded. Preserve the local patches and upstream
license attribution. Distribution notices and manifest procedures are
documented in [`third_party/PACKAGING.md`](../third_party/PACKAGING.md);
older worker bundle counts are not claims about this migration's resolved
dependency graph.

## Verified local native API results

The latest full tests passed default 907/base 881 tests, zero failures, with
24 doctests ignored in each configuration. Final strict Clippy for both
configurations and all targets, formatting, the Windows base MSVC
library/binary crosscheck, 17 distribution checks, notices verification and
rebuild passed. Focused reruns of the 15 completion, 17 native API and five
Wayland regressions also passed after the deadline fix. Counts exclude nested
filtered subprocess summaries.

The full tests include 17 isolated native API contracts per configuration,
five desktop Wayland fixture regressions, 15 desktop completion regressions,
and the actual registry adapter's in-memory completion test. All 15 completion
regressions also passed separately, covering finite nested blocking/async/raw-
thread work after timeout, disconnect or panic, pre-enqueue binding, dedicated
runtimes and all three exact compositor deadline-helper cases. The added
resumed-I/O case verifies that `BeforeDeadline` checks the deadline and timer
before every inner poll, so I/O becoming ready after expiry cannot dispatch a
request. The final Rust recheck closed that finding and confirmed timer
wake-up behavior, with no remaining blocking defect, alongside code/security
follow-up. A cached-database audit reported zero vulnerabilities and seven
unmaintained-dependency warnings, without a fresh advisory-data claim.

Observed rebuilt Linux smoke found 63 tools, one envd process with no children
and one owned TCP listener; old MCP returned 404, JSON-RPC returned 400 and
obsolete startup configuration was rejected. No desktop actions were performed.
The final rebuilt repetition after the deadline fix passed. Final scoped line
coverage for three root modules is recorded in the canonical verification
record; dependency exports were empty, so no numeric
`work_scope` coverage is claimed.

These are local fixture and compilation results. Native Windows desktop/live
S0 Windows acceptance of the new API remains unrun; no local musl compiler
was available. No fresh Go differential/SDK campaign, remote CI, commit, push,
merge, deployment or real desktop action is claimed. The canonical
[verification record and limits](native-cua.md#verified-local-state) retain
that scope.

## Historical evidence — older revisions only

All results in the following subsections predate the native four-route API.
They retain the original counts, commands, transport fields and limitations
as evidence for the recorded campaigns. References to extension mounting,
MCP/Bearer admission, modes, helpers, workers and their tests describe removed
architecture. They do not validate native REST admission, new runtime wiring,
the current dependency graph or new Linux/Windows platform acceptance. No new
tests, builds, CI or desktop acceptance are claimed here.

### Shared-mode acceptance and dependency audit (before native REST)

On 2026-11-04, an isolated Xvfb desktop and a real desktop-enabled envd
process completed MCP initialization, discovery of 63 tools, and
`get_desktop_state` returning an 800×600 PNG. No worker child existed before
the call. CUA rejected requests before `/init`; S0 still required
`X-Access-Token`. This probe did not access the real user's desktop.

Shared-mode tests cover live token installation/rotation/reset, failed and
stale init, header separation, admission before body/backend/accounting,
configuration-lock release, helper credential isolation, embedded MCP,
limits/timeouts, and Windows service/session policy. Final Linux daemon suites
passed: base 881 tests and desktop 885 tests, each with 0 failures and 24
ignored tests. Shared-module coverage was 278/283 lines (98.23%) and 37/37
functions (100%); this is not whole-project or branch coverage. Strict Clippy,
formatting and the Windows base MSVC crosscheck passed locally. That integration
campaign did not include native Windows desktop/service acceptance, and its
static build attempt lacked `x86_64-linux-musl-gcc`. The later
[Linux maintenance regression](dependency-regression.md) includes a successful
fresh static musl build and base 905 / desktop 909 tests. The
[Windows maintenance regression](windows-dependency-regression.md) includes
native base/desktop/worker gates and SDK 27/27. The separate
[Windows CUA campaign](windows-cua-acceptance.md) adds real embedded graphics,
window operations and SCM/helper lifecycle/recovery at the maintenance source
revision; native credential live reset remains unsupported by the DPAPI
provisioning path. CI configuration alone is not evidence of a successful run.

The follow-up [dependency security maintenance](dependency-security.md) updates
the root to Tokio 1.53.3 and both consumer graphs to zstd 0.14.3 /
zstd-safe 8.2.0. The standalone worker retains its already-fixed Tokio 1.54.1.
Both local `cargo audit --no-fetch` runs now exit 0 with zero vulnerabilities
and zero unsound warnings. Seven unmaintained-dependency warnings remain in
each graph; they have not been hidden or classified as fixed. The earlier CUA
acceptance audit failed with three vulnerabilities and the Tokio warning;
the maintenance record preserves that baseline and the ownership migration.

### Earlier worker and enumeration evidence (before native REST)

- Worker contract tests: 9 passed, plus 5 Linux worker/MCP integration
  regressions (`cargo test --manifest-path cua-worker/Cargo.toml --lib --bins
  --tests`). Before the patch, the integration target compiled and ran with
  2 passing controls and 3 failures; the same target passes all 5 afterward.
  Private Hyprland/Wayland sockets exercise legacy IDs, 0.57 numbered/named/
  special addresses, full-width window IDs, geometry, PID/on-screen filters,
  hidden/unmapped windows, genuine empty results and IPC/JSON/identity errors,
  without a host desktop connection or inherited credentials.
- Read-only Hyprland 0.57 acceptance (2026-11-04): before the patch,
  compositor IPC reported 6 mapped clients while worker MCP `list_windows`
  returned a successful empty list; an invalid session signature also
  returned empty success. After the patch the worker returned 6 windows,
  with all 6 stable compositor addresses, PIDs, positions and sizes matching
  the before/after IPC snapshots. The invalid signature returned
  `isError: true`, code `tool_invocation_failed`. The probe called only
  `list_windows`; it did not capture, send input or launch desktop apps.
  This is enumeration evidence, not rotated-display or action acceptance.
- rust-envd suite: 866 passed, 0 failed (24 ignored by features), including
  `tests/extensions.rs` (no-shadow, merge over the Connect fallback,
  empty-set identity) and the supervisor/proxy tests in `src/ext/cua.rs`
  (26 in that module: health and failure reporting, restart count and
  replacement after exit, backoff bounds and jitter, restart-budget
  exhaustion, `COORWORKER_CUA_MAX_RESTARTS` parsing, Prometheus exposition,
  tool-metric label cap and `other` overflow, `tools/call` extraction from
  single and batch JSON-RPC, forwarded-outcome classification, route absent
  without a token, 401 for missing/wrong tokens, forwarding with worker-token
  injection and status pass-through, proxy tool counts and latency, 502
  unreachable, 503 dead worker, discovery fields, launch resolution,
  adopted-worker probing, recovery and `observed_restarts`).
- Supervision: a child that exits is replaced within the first backoff step
  and reported healthy once the replacement is installed
  (`supervisor_replaces_a_worker_that_exits`, with a fake relauncher so no
  real worker binary is needed).
- Metrics run (Xvfb `:98`, envd dev `127.0.0.1:49987`, real worker on
  `3212`): `/cua/metrics` returned `..._worker_up 1` and
  `..._restarts_total 0`. After `kill -9` it reported `..._worker_up 0` with
  `..._restarts_total 1`, then `..._worker_up 1` again with a new worker
  PID. S2 `/metrics` still returned the oracle's JSON document unchanged.
- Adoption run (Xvfb `:98`, worker started by hand on `3212`, envd dev on
  `49987` with `COORWORKER_CUA_EXTERNAL=1` and a proxy token, no
  `COORWORKER_CUA_BIN`): `/cua/health` reported healthy with `restarts 0`,
  `/cua/info` published `proxy_path`/`proxy_auth`, `/cua/metrics` reported
  `worker_up 1`, and the envd process had no children — it did not spawn a
  worker. `POST /cua/mcp` without the token returned `401`; with it,
  `initialize` and `tools/list` (63 tools) went through. `kill -9` on the
  worker turned `/cua/health` into `503` with the connection error in
  `detail` and `worker_up 0` while S0 `/health` stayed `204`; envd did not
  restart it. Starting the worker again returned the extension to healthy
  with `worker_up 1`, still `restarts_total 0`. Verification processes and
  the temporary directory were removed.
- Failure-budget run (Xvfb `:98`, a stub binary that exits `1` on every
  launch, `COORWORKER_CUA_MAX_RESTARTS=3`): the launcher ran exactly three
  times and stopped. `/cua/health` stayed `503` with
  `budget_exhausted: true` and detail `restart budget exhausted after 3
  attempts: …`; `/cua/metrics` reported
  `..._worker_budget_exhausted 1`; six seconds later the launch count was
  still three and the daemon had no children. S0 `/health` stayed `204`.
- Ready-then-crash budget regression: a successful relaunch must retain the
  failure count until a stable run, rather than resetting it at readiness.
  `ready_workers_that_crash_still_exhaust_the_restart_budget` failed by
  timing out before the fix and passes afterward. A real daemon run with a
  worker that binds its loopback listener for 0.5 s and then exits `1` on
  every launch reached healthy, launched exactly three times with budget
  `3`, then stopped with `restarts: 3` and `budget_exhausted: true`. After
  another three seconds the launch count was unchanged, there were no child
  processes, and S0 `/health` still returned `204`.
- Proxy tool-metric run (Xvfb `:98`, spawned worker on `3216`, envd dev on
  `49989` with `COORWORKER_CUA_PROXY_TOKEN`): MCP `initialize` and
  `tools/call` for `list_windows` (one real window) and `get_screen_size`
  went through `POST /cua/mcp`. `/cua/metrics` then reported
  `..._tool_calls_total{tool="list_windows",status="ok"} 1`,
  `..._tool_calls_total{tool="get_screen_size",status="ok"} 1` and
  `..._tool_latency_seconds_sum 0.118216`. S2 `/metrics` still returned the
  oracle's JSON document (`content-type: application/json`, same keys) and
  S0 `/health` stayed `204`.
- Adoption recovery run (Xvfb `:98`, worker started by hand on `3217`, envd
  dev on `49990` with `COORWORKER_CUA_EXTERNAL=1`, no
  `COORWORKER_CUA_BIN`): healthy with `observed_restarts 0`, `restarts 0`.
  `kill -9` turned `/cua/health` into `503` (`Connection refused (os error
  111)`) with `worker_up 0` and left the counters at `0/0`; restarting the
  worker on the same port returned the extension to healthy with
  `observed_restarts 1` and `restarts_total` still `0`. The envd process
  never had a child. A second run with the worker absent at adoption showed
  it appearing as healthy with `observed_restarts` still `0` — a start, not
  a recovery.
- End-to-end run on Xvfb `:99` (before supervision existed, M5): daemon →
  spawned worker → `/cua/health` `200`; MCP `initialize` (`cua-driver
  0.38.0`), `tools/list` (63 tools), `tools/call get_desktop_state` returned
  a valid 1024×768 PNG; killing the worker turned `/cua/health` into `503`
  while S0 `/health` stayed `204`. With the supervisor, that same kill now
  leads to a relaunch instead of staying unhealthy.
- Proxy run on the same display with `COORWORKER_CUA_PROXY_TOKEN` set:
  `/cua/info` published `proxy_path`/`proxy_auth`; an unauthenticated
  `POST /cua/mcp` returned `401`; with the token, `initialize` (protocol
  `2025-06-18`), `tools/list` (63 tools, 44,350 description bytes) and
  `tools/call get_desktop_state` (valid 1024×768 PNG, `image/png`) all went
  through envd; killing the worker made `POST /cua/mcp` and `/cua/health`
  `503` while S0 `/health` stayed `204` (M5, before the supervisor). A
  233-byte PNG is the blank Xvfb screen, not a truncation: the IHDR says
  1024×768 and the signature is valid.
- Downstream client conformance: the coorworker AgentHost's MCP client
  (official Go SDK v1.8.0, Streamable HTTP, session-less, `Authorization:
  Bearer`) completes `initialize`, lists all 63 tools and calls
  `get_desktop_state` against `POST /mcp`. Tool descriptions total 44,350
  bytes; two exceed 4 KiB, which is why the AgentHost per-description bound
  was raised to 8 KiB with a 256 KiB aggregate bound rather than truncating
  the driver's usage notes.
