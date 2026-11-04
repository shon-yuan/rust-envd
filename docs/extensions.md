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
   never run for an extension request. An extension owns its own token,
   status codes and error envelope. Reusing the `/init` access token for an
   extension is a defect.
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
| `cua` | shipped. `src/ext/cua.rs` supervises the separate `coorworker-cua` worker (`cua-worker/`); the MCP surface is the vendored Cua Driver registry. |

## `cua`: computer-use surface

### Why a separate process

The Cua Driver desktop stack (X11/AT-SPI on Linux, UI Automation on Windows)
cannot live inside the envd daemon binary. On Linux the X11 crates resolve
`libX11` through `pkg-config` at build time, so the static musl guest build
would break; on Windows the daemon runs as an SCM service in session 0 and
cannot drive the interactive desktop. The worker is therefore a separate
`coorworker-cua` binary that rust-envd starts and supervises.

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
reason in `detail` after it exits or fails to start. `/cua/info` publishes
`name` (`cua`), `mcp_url`, `transport`, `protocol` and `auth` (`bearer` when
the worker has a `CUA_ENV_TOKEN`, otherwise `none`) and never the token: a
discovery document that leaked it would hand every guest process control of
the desktop. Both routes are extension-owned — they are mounted after
`http::layered` and do not use the envd `/init` access token.

A worker that exits is replaced. The supervisor relaunches it with
exponential backoff (0.5 s doubling to 30 s; a worker that stayed up for
60 s resets the sequence) and only reports healthy once the new process
accepts on its loopback port. A worker that never comes up does not fail the
daemon: S0 keeps serving and the extension stays visible as unhealthy with
the reason in `detail`.

`/cua/metrics` exposes Prometheus text with `coorworker_envd_cua_worker_up`
(1/0) and `coorworker_envd_cua_worker_restarts_total`. It is extension-owned
and deliberately separate from S2 `/metrics`, whose JSON wire format is the
oracle's and stays byte-identical.

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
| `COORWORKER_CUA_PROXY_TOKEN` | Enables `POST /cua/mcp` and is the bearer token callers present. Empty means disabled. |
| `--listen` | Worker flag for the same address (the extension passes it). |
| `--token-file` / `CUA_ENV_TOKEN` | Bearer token for `/mcp`. Accepted as `Authorization: Bearer …` or `X-Cua-Env-Authorization`. When set in envd's environment the worker inherits it and the proxy injects it on forwards. |

The worker refuses to bind a non-loopback address without a token. On
loopback with no token, auth is disabled and the worker says so on startup.

### Build

The worker is its own package (it carries its own lockfile and target
directory); the daemon never links it:

```sh
cargo build --locked --bin coorworker-envd
cd cua-worker && cargo build --locked --bin coorworker-cua
```

Set `DISPLAY` for the worker on Linux; it needs a running X server.

### Windows

The extension is disabled in SCM service mode with a log line, because a
session-0 service cannot reach the interactive desktop. Running the worker
next to the logged-in user is the supported shape; a service-to-session
bridge is separate work.

CI builds the worker with the MSVC toolchain and runs its contract tests on
`windows-latest`, so the Windows code path compiles and serves MCP. Those
tests use a static tool provider: real UI Automation actions still require a
logged-in desktop session and are not exercised there.

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

### Evidence

- Worker contract tests: 9 passed (`cua-worker/`, `cargo test --lib --bins`).
- rust-envd suite: 853 passed (24 ignored by features), including
  `tests/extensions.rs` (no-shadow, merge over the Connect fallback,
  empty-set identity) and the supervisor/proxy tests in `src/ext/cua.rs`
  (13 in that module: health and failure reporting, restart count and
  replacement after exit, backoff bounds, Prometheus exposition, route
  absent without a token, 401 for missing/wrong tokens, forwarding with
  worker-token injection and status pass-through, 502 unreachable, 503 dead
  worker, discovery fields).
- Supervision: a child that exits is replaced within the first backoff step
  and reported healthy once the replacement is installed
  (`supervisor_replaces_a_worker_that_exits`, with a fake relauncher so no
  real worker binary is needed).
- Metrics run (Xvfb `:98`, envd dev `127.0.0.1:49987`, real worker on
  `3212`): `/cua/metrics` returned `..._worker_up 1` and
  `..._restarts_total 0`. After `kill -9` it reported `..._worker_up 0` with
  `..._restarts_total 1`, then `..._worker_up 1` again with a new worker
  PID. S2 `/metrics` still returned the oracle's JSON document unchanged.
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
