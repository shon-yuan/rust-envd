# Extensions

rust-envd serves the official envd surface **plus** additive surfaces. The
compatibility claim is scoped to the official surface; an extension is a
separate, independently versioned product that happens to run in the same
process.

```text
S0  official envd surface   frozen: 13 REST + 17 Connect, byte-identical
S1  extension surfaces      additive, namespaced (/cua/...), own auth
S2  control surface         /health, /metrics (unchanged)
```

## Invariants

1. **S0 is frozen.** Every request the pinned Go daemon (0.10.2,
   `51083a8`) accepts keeps its status line, headers, framing, trailers and
   body. The differential gates run S0 only.
2. **An extension cannot shadow S0.** Extensions are nested under
   `/<name>` and `Extensions::new` rejects a name that is one of envd's
   first path segments (`health`, `files`, `init`, …). Names are otherwise
   `[a-z][a-z0-9_-]{0,62}`; a dot or slash is rejected, so a name can never
   collide with a Connect `/<package>.<Service>/<Method>` path.
3. **Layering is separate.** `app_with_*` mounts extensions *after*
   `http::layered`, so the envd frontdoor, authorization and CORS layers
   never run for an extension request. An extension owns its status codes,
   credential policy and error envelope. The explicitly selected shared CUA
   modes read the live protected `/init` credential; this does not apply S0
   middleware to extensions or change S0's accepted headers.
4. **No extension means no change.** `Extensions::empty()` is the parity
   build; mounting it is a no-op. The suite that proves S0 is the same
   suite the campaign has been running.
5. **Evidence is separated.** S0 evidence is differential. An extension is
   proven by its own contract tests and, where one exists, by an upstream
   client's conformance run. Neither result stands in for the other.

## Usage

```rust
use coorworker_envd::daemon::app_with_execution_and_extensions;
use coorworker_envd::ext::cua::CuaExtension;
use coorworker_envd::ext::{Extension, Extensions};

let cua = CuaExtension::spawn(worker_binary, worker_listen).await;
let extensions = Extensions::new(vec![Arc::new(cua) as Arc<dyn Extension>])?;
let router = app_with_execution_and_extensions(state, runtime, &extensions);
```

`CuaExtension::from_env()` is the same thing driven by `COORWORKER_CUA_BIN`.

`Extension::router()` returns paths relative to the extension prefix and must
not install a fallback: the envd router owns the process-wide 404 shape.
`Extension::health()` reports readiness without failing S0.

Extension configuration is not carried in `/init`: Go's decoder ignores
unknown members, so interpreting one there would make the same input mean two
different things on the two daemons.

## Status

| Extension | State |
| --- | --- |
| `cua` | Embedded execution or supervised/adopted `coorworker-cua` worker; both reuse the vendored Cua Driver registry. |

## `cua`: computer-use surface

### Shared credential and embedded execution

Set `COORWORKER_CUA_MODE` explicitly to migrate from legacy deployment:

| Mode | Runtime | Requirements |
| --- | --- | --- |
| unset / `legacy` | Existing worker supervision or adoption | Existing `BIN` / `EXTERNAL` and proxy configuration below |
| `embedded` | MCP registry inside envd | Build with `--features cua-desktop`; run in desktop user's session |
| `shared-external` | Adopted user-session helper | Loopback `COORWORKER_CUA_LISTEN`; nonempty matching `CUA_ENV_TOKEN` |

Shared modes expose `POST /cua/mcp` on the existing envd listener. The caller
sends `Authorization: Bearer <current envd access token>`. Before installing a
token through `/init`, and after an authorized token reset, CUA denies all
requests. Successful `/init` rotation immediately invalidates the old token
for subsequent admissions. Failed or timestamp-skipped init retains the
actual current state. Authentication occurs before body parsing, backend
execution and accounting; admitted work may finish after rotation without
holding the configuration lock. S0 continues accepting `X-Access-Token` and
keeps its original fresh-boot policy and exclusions. Neither header substitutes
for the other.

```sh
cargo build --locked -j4 --features cua-desktop
COORWORKER_CUA_MODE=embedded target/debug/coorworker-envd --development
```

Initialize envd using its normal token provisioning procedure before connecting
an MCP client to `/cua/mcp`. No extra worker listener or process is created.
The desktop build links the same platform registry as the standalone worker;
its native crashes can therefore terminate envd. The default build does not
activate those dependencies and remains suitable for static musl guests.
Selecting embedded mode in a base build reports unavailable rather than
falling back to another mode.

Windows SCM mode, service handover successors, Session 0, and failed session
lookup refuse embedded initialization before the registry is constructed.
For these deployments select `shared-external`, run the standalone helper in
the target user's session, and supply its local transport token to envd using
`CUA_ENV_TOKEN`. The helper can read the same value through `--token-file`.
Provisioning the helper and securing that file are deployment responsibilities.
The envd token is never copied to the helper. Missing or empty local transport
credentials leave the shared extension unavailable. The helper's `/health`
probe establishes liveness; it cannot prove a separately configured transport
token matches, so MCP may still return 401 for a mismatch.

Shared discovery reports `mode`, `auth_source: "envd"`, `auth_configured`, and
relative `proxy_path: "/cua/mcp"`. Embedded mode has no helper URL. Health
separates backend availability from credential readiness; neither is a GUI
session or screenshot acceptance probe. Embedded worker gauges/restart
counters stay zero. Backend/readiness gauges and tool counters describe the
shared endpoint; rejected credentials do not increment tool-call counters.
`/cua/info`, `/cua/health`, and `/cua/metrics` remain credential-free metadata
and belong on a trusted listener or behind deployment access controls.
Invalid explicit modes never downgrade to legacy authentication.

### Legacy separate process

The Cua Driver desktop stack (X11/AT-SPI on Linux, UI Automation on Windows)
is excluded from the default daemon dependency graph. A separate
`coorworker-cua` binary remains useful for that lightweight build, native crash
isolation, and Windows services that execute outside the interactive session.
The following configuration describes the unchanged legacy mode.

```text
guest / host
  coorworker-envd
    ext::cua  ── spawns, watches ──►  coorworker-cua
      GET /cua/health                   POST /mcp   (MCP JSON-RPC)
      GET /cua/info                     GET  /mcp   → 405
      GET /cua/metrics
      POST /cua/mcp   (optional proxy)
```

The extension is enabled only when the worker binary is configured. With no
`COORWORKER_CUA_BIN`, the build is the parity daemon: no routes, no child,
and the router is identical to S0.

### Extension routes

`/cua/health` returns `200` with `{"status":"healthy"}` (plus the
`restarts` count) while the worker process is alive, and `503` with the
reason in `detail` after it exits or fails to start. The document also carries
`observed_restarts` and `budget_exhausted`. In spawn mode `observed_restarts`
is always `0`; `budget_exhausted` turns `true` only when the supervisor gave
up. `/cua/info` publishes
`name` (`cua`), `mcp_url`, `transport`, `protocol` and `auth` (`bearer` when
the worker has a `CUA_ENV_TOKEN`, otherwise `none`) and never the token: a
discovery document that leaked it would hand every guest process control of
the desktop. Both routes are extension-owned — they are mounted after
`http::layered` and do not use the envd `/init` access token.

A worker that exits is replaced. The supervisor relaunches it with
exponential backoff (0.5 s doubling to a 30 s cap, each step jittered between
half and 1.5× its nominal delay; a worker that stayed up for 60 s resets the
sequence) and only reports healthy once the new process accepts on its
loopback port. A worker that never comes up does not fail the daemon: S0 keeps
serving and the extension stays visible as unhealthy with the reason in
`detail`.

After `COORWORKER_CUA_MAX_RESTARTS` consecutive failed launches or immediate
exits (default 8; `0` restores unlimited retry) the supervisor stops
relaunching and records `budget_exhausted: true` with detail
`restart budget exhausted after N attempts: …`. `/cua/health` stays `503` and
`coorworker_envd_cua_worker_budget_exhausted` becomes `1`; nothing retries in
the background. Recovery is manual: fix or replace the worker binary and
restart envd. The budget counts a crash loop, not occasional restarts — one
stable run clears it.

### Adopted workers

With `COORWORKER_CUA_EXTERNAL` set, envd does not spawn the worker; it
**adopts** the one listening at `COORWORKER_CUA_LISTEN`. The probe runs at
startup and every 2 s: `GET /health` must answer `204` within 2 s, so a
process that still accepts but no longer serves is not reported healthy.
`/cua/health`, `/cua/info`, `/cua/metrics` and the optional proxy behave
exactly as in spawn mode; a worker that disappears turns the extension
unhealthy with the connection error in `detail`, and one that comes back is
picked up again. `restarts` stays `0` in this mode: envd never replaces a
process it does not own, and `coorworker_envd_cua_worker_restarts_total`
must not be read as a restart count for it. Instead,
`observed_restarts` (and `coorworker_envd_cua_worker_observed_restarts_total`)
counts healthy→unhealthy→healthy recoveries the 2 s probe actually saw: a
worker that is absent at adoption and later appears is a start (0), not a
restart. `COORWORKER_CUA_EXTERNAL` wins when both variables are set.

`/cua/metrics` exposes Prometheus text:

| Series | Meaning |
| --- | --- |
| `coorworker_envd_cua_worker_up` | `1` while the worker answers; `0` otherwise. |
| `coorworker_envd_cua_worker_restarts_total` | Exits the supervisor replaced. Spawn mode only; always `0` for an adopted worker. |
| `coorworker_envd_cua_worker_observed_restarts_total` | Recoveries observed while adopting. Never mixed with the supervisor counter. |
| `coorworker_envd_cua_worker_budget_exhausted` | `1` once the spawn supervisor gave up. |
| `coorworker_envd_cua_tool_calls_total{tool,status}` | `tools/call` requests forwarded through `POST /cua/mcp`, by tool and `ok`/`error`. |
| `coorworker_envd_cua_tool_latency_seconds_sum` | Cumulative time spent forwarding those calls. |

The tool series cover only the optional envd proxy. A caller that reaches the
worker directly, and the ToolHost WS relay, are not counted; do not read them
as global desktop-tool usage. At most 64 distinct tool names are kept and
every later name is attributed to `tool="other"`, so a caller cannot grow the
label set without bound. These counters are extension-owned and deliberately
separate from S2 `/metrics`, whose JSON wire format is the oracle's and stays
byte-identical.

A scrape sample:

```yaml
scrape_configs:
  - job_name: coorworker-envd-cua
    metrics_path: /cua/metrics
    scrape_interval: 15s
    static_configs:
      - targets: ["127.0.0.1:49983"]
```

`/cua/health` and `/cua/metrics` are unauthenticated like the existing
extension precedent and must stay on a loopback or otherwise trusted
listener; `/cua/mcp` is the only route with a credential.

### Remote access without a second port

The worker listens on loopback only, so a caller outside the guest cannot
reach it. Configuring `COORWORKER_CUA_PROXY_TOKEN` mounts `POST /cua/mcp` on
the envd surface instead: envd checks that bearer token (constant-time; an
empty value means "not configured" and the route is absent) and forwards one
JSON-RPC request to the worker. The deployment then needs no extra port
beyond the one envd already exposes.

- The proxy token and the worker's `CUA_ENV_TOKEN` are different credentials.
  The caller only ever learns the proxy token; envd injects the worker token,
  so the caller's `Authorization` header never reaches the worker.
- Only `mcp-protocol-version` and `mcp-session-id` are forwarded besides the
  body. Requests are capped at 1 MiB and worker responses at 32 MiB; the
  proxy waits 30 s. Errors are extension-owned: `401` wrong or missing token,
  `503` worker not running, `502` worker unreachable, `504` timeout. The
  worker's own HTTP status is passed through unchanged (a JSON-RPC `400`
  stays `400`).
- `/cua/info` adds `proxy_path` (`/cua/mcp`) and `proxy_auth` (`bearer`) when
  the proxy is mounted. It is relative because envd does not know the address
  a caller reached it on, and the token is never echoed.
- This route is extension-owned like the rest: it is mounted after
  `http::layered` and does not accept the envd `/init` access token.

### Worker interface

| Method and path | Behaviour |
| --- | --- |
| `GET /health` | `204`, no body. |
| `POST /mcp` | MCP JSON-RPC over HTTP: `initialize`, `tools/list`, `tools/call`, notifications (`202`, empty body). Parse errors are JSON-RPC `-32700` / `-32600` with HTTP `400`. |
| `GET /mcp` | `405`. |

Tools come from `platform_linux::register_tools()` /
`platform_windows::register_tools()` (63 tools on Linux at the vendored
commit). Every tool name must carry a reviewed risk classification in Cua
Driver; an unclassified name is denied, and that policy is not relaxed by
this worker.

### Configuration

| Variable / flag | Meaning |
| --- | --- |
| `COORWORKER_CUA_BIN` | Path to the worker binary. Unset disables the extension. |
| `COORWORKER_CUA_LISTEN` | Worker listen address; default `127.0.0.1:3211`. |
| `COORWORKER_CUA_EXTERNAL` | Non-empty and not `0`/`false`: adopt the worker at `COORWORKER_CUA_LISTEN` instead of spawning one. Wins over `COORWORKER_CUA_BIN`. |
| `COORWORKER_CUA_PROXY_TOKEN` | Enables `POST /cua/mcp` and is the bearer token callers present. Empty means disabled. |
| `COORWORKER_CUA_MAX_RESTARTS` | Consecutive failed launches/exits before the supervisor stops; default `8`, `0` means unlimited. Invalid values leave the extension visible but unavailable with the parse error. |
| `--listen` | Worker flag for the same address (the extension passes it). |
| `--token-file` / `CUA_ENV_TOKEN` | Bearer token for `/mcp`. Accepted as `Authorization: Bearer …` or `X-Cua-Env-Authorization`. When set in envd's environment the worker inherits it and the proxy injects it on forwards. |

The worker refuses to bind a non-loopback address without a token. On
loopback with no token, auth is disabled and the worker says so on startup.

### Build

The standalone worker is its own package with a separate lockfile and target
directory. The default daemon excludes its desktop dependencies; the optional
`cua-desktop` build links its reusable library:

```sh
cargo build --locked --bin coorworker-envd
cd cua-worker && cargo build --locked --bin coorworker-cua
```

Set `DISPLAY` for the worker on Linux; it needs a running X server.

### Windows

A session-0 service cannot drive the interactive desktop, so the service
must not spawn the worker. It can still serve the `cua` surface by adopting
one the user session started:

1. Start `coorworker-cua --listen 127.0.0.1:3211` in the logged-in session
   (for example a logon scheduled task, or the same command in the user's
   startup folder).
2. Start envd as the SCM service with `COORWORKER_CUA_EXTERNAL=1` and, if the
   worker uses one, `CUA_ENV_TOKEN` set to the same token. Loopback is shared
   between sessions, so the service reaches the worker without a second port.

envd then probes the worker every 2 s and reports `/cua/health`,
`/cua/info`, `/cua/metrics` and the optional proxy as usual. A service
started with only `COORWORKER_CUA_BIN` still refuses the extension with a log
line, and the log now names `COORWORKER_CUA_EXTERNAL`. This is adoption, not
a `CreateProcessAsUser` bridge: envd does not start the worker in the user's
session, so the deployment must.

CI builds the worker with the MSVC toolchain and runs its contract tests on
`windows-latest`, so the Windows code path compiles and serves MCP. Those
tests use a static tool provider: real UI Automation actions still require a
logged-in desktop session and are not exercised there.

### Linux window enumeration

Native Wayland enumeration is opt-in: set `CUA_DRIVER_RS_ENABLE_WAYLAND=1`,
`WAYLAND_DISPLAY`, `XDG_RUNTIME_DIR` and the current
`HYPRLAND_INSTANCE_SIGNATURE` for a Hyprland session. A stale signature is
an IPC failure. Without the opt-in the worker uses its existing X11 path;
rootless XWayland with no X11 clients can correctly return zero windows.

The local Hyprland patch accepts both legacy numeric workspace IDs and
0.57 stable workspace addresses, including named and special workspaces.
Addresses take precedence when present, while display names do not establish
identity. Empty workspaces are excluded from visibility matching, and a
mapped window without a workspace identity is an error. Full compositor
window IDs, PID/geometry validation and hidden/DPMS visibility rules are
preserved. See `vendor/cua-driver/PROVENANCE.md` for the pinned upstream and
local patch list.

For explicit `tools/call list_windows`, recognized Hyprland/niri enumeration
failures and blocking-task failures return MCP `result.isError: true` with a
structured error code. They must not be interpreted as an empty desktop or
trigger a fallback into another window-ID namespace. An actual successful
empty enumeration still returns `structuredContent.windows: []`. Other
internal callers retain their best-effort interface, and the generic
Wayland/X11 fallback policy is unchanged. This does not claim that every
backend failure or every tool now uses the stricter policy.

This change covers enumeration only. It does not add rotated multi-monitor
capture/input support; screen-size and action admission retain their existing
checks. Desktop interaction and Windows desktop support require separate
acceptance evidence.

### Licensing

Only the MIT crates are vendored (`vendor/cua-driver/`, provenance in
`vendor/cua-driver/PROVENANCE.md`): `cua-driver-core`,
`cua-driver-contract`, `platform-linux`, `platform-windows`,
`cursor-overlay`, `cua-cursor-motion`, `pip-preview`. `cua-perception` is
AGPL-3.0-only and is excluded. `libs/cua-spacesd` is FSL-1.1-MIT and must
not be merged into this MIT repository. The worker's dependency notices are
a separate bundle under `third_party/cua-worker/` (502 packages, generated
from `cua-worker/Cargo.lock` by `scripts/generate_notices.py`); packages that
ship no license file use the upstream Cua Driver license or the checked-in
SPDX texts in `scripts/license-overrides/`.

### Shared-mode acceptance and dependency audit

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
formatting and the Windows base MSVC crosscheck passed locally. Native Windows desktop/service
acceptance is pending. The current static build attempt failed because
`x86_64-linux-musl-gcc` was unavailable; the desktop-free base dependency graph
was verified separately. CI now defines base/desktop and static build lanes;
that configuration alone is not a successful CI run.

The follow-up [dependency security maintenance](dependency-security.md) updates
the root to Tokio 1.53.3 and both consumer graphs to zstd 0.14.3 /
zstd-safe 8.2.0. The standalone worker retains its already-fixed Tokio 1.54.1.
Both local `cargo audit --no-fetch` runs now exit 0 with zero vulnerabilities
and zero unsound warnings. Seven unmaintained-dependency warnings remain in
each graph; they have not been hidden or classified as fixed. The earlier CUA
acceptance audit failed with three vulnerabilities and the Tokio warning;
the maintenance record preserves that baseline and the ownership migration.

### Earlier worker and enumeration evidence

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
