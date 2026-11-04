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
```

The extension is enabled only when the worker binary is configured. With no
`COORWORKER_CUA_BIN`, the build is the parity daemon: no routes, no child,
and the router is identical to S0.

### Extension routes

`/cua/health` returns `200` with `{"status":"healthy"}` while the worker
process is alive, and `503` with the reason in `detail` after it exits or
fails to start. `/cua/info` publishes `mcp_url`, `transport` and `protocol`
and never the token: a discovery document that leaked it would hand every
guest process control of the desktop. Both routes are extension-owned —
they are mounted after `http::layered` and do not use the envd `/init`
access token.

A worker that dies does not fail the daemon: S0 keeps serving and the
extension stays visible as unhealthy.

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
| `--listen` | Worker flag for the same address (the extension passes it). |
| `--token-file` / `CUA_ENV_TOKEN` | Bearer token for `/mcp`. Accepted as `Authorization: Bearer …` or `X-Cua-Env-Authorization`. |

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

### Licensing

Only the MIT crates are vendored (`vendor/cua-driver/`, provenance in
`vendor/cua-driver/PROVENANCE.md`): `cua-driver-core`,
`cua-driver-contract`, `platform-linux`, `platform-windows`,
`cursor-overlay`, `cua-cursor-motion`, `pip-preview`. `cua-perception` is
AGPL-3.0-only and is excluded. `libs/cua-spacesd` is FSL-1.1-MIT and must
not be merged into this MIT repository.

### Evidence

- Worker contract tests: 9 passed (`cua-worker/`, `cargo test --lib --bins`).
- rust-envd suite: 843 passed, including `tests/extensions.rs` (no-shadow,
  merge over the Connect fallback, empty-set identity) and the supervisor
  tests in `src/ext/cua.rs`.
- End-to-end run on Xvfb `:99`: daemon → spawned worker → `/cua/health`
  `200`; MCP `initialize` (`cua-driver 0.38.0`), `tools/list` (63 tools),
  `tools/call get_desktop_state` returned a valid 1024×768 PNG; killing the
  worker turned `/cua/health` into `503` while S0 `/health` stayed `204`.
