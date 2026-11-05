# Native embedded CUA

## Contract and migration

CUA is an additive native HTTP service in the existing envd daemon. The original S0 paths, protobufs, authentication exceptions and response contracts remain unchanged. The sole CUA execution engine is the in-process desktop registry. There is no MCP endpoint, JSON-RPC envelope, initialization handshake, extension mounting layer, worker supervisor, helper transport, or execution-mode selection.

| Endpoint | Request | Response |
| --- | --- | --- |
| `GET /cua/tools` | No body | `{"tools":[{"name":"…","description":"…","input_schema":{…}}]}` |
| `POST /cua/call` | `{"name":"…","arguments":{…}}` | `{"success":true,"content":[…],"data":…}` |
| `GET /cua/health` | No body | Passive runtime availability and reason; does not probe or act on the desktop |
| `GET /cua/metrics` | No body | Bounded Prometheus tool counters, latency and runtime readiness |

All CUA routes, including unknown paths under `/cua`, require completed envd initialization with a nonempty current `X-Access-Token`. Bearer, Basic and file signatures cannot replace it. CORS preflight retains the existing shared behavior and cannot execute a tool. Authentication runs in the existing S0 middleware before body decoding. Credential rotation affects subsequent requests; an admitted action is not retroactively cancelled.

The native call body is an object with exactly `name` and `arguments`; arguments must be an object. JSON-RPC requests, notifications, batches and unknown fields are rejected. Tool schemas use `input_schema`; image content uses `mime_type`. Results contain no JSON-RPC `id`/`result` or MCP `isError`/`structuredContent`. Tool-reported failures use `success:false`; `data` preserves the driver's structured diagnostics, with `{"code":"tool_error"}` when a failure provides none. Discovery omits the vendor's normalized output schema because it does not describe every raw diagnostic payload. Transport failures use the existing newline-terminated S0 JSON error envelope (`code`, `message`).

Transport errors: 400 invalid JSON/arguments; 401 missing, uninitialized, ambiguous or invalid credentials; 404 unknown route/tool; 405 wrong method; 413 request too large; 415 unsupported media type; 429 execution already busy; 500 internal failure; 503 unavailable runtime; 504 execution deadline exceeded. CUA requests are limited to 1 MiB and serialized responses to 32 MiB. The execution deadline is 30 seconds. One execution slot remains held until the underlying action actually finishes, even after an HTTP timeout or disconnect; timeout does not guarantee cancellation of native OS work.

Linux and known Windows desktop user sessions are eligible. SCM-attached processes, Session 0, service successors in Session 0 and unknown Windows sessions expose unavailable CUA without constructing a desktop registry. They do not start or contact a helper. Filesystem/process S0 services remain available. `--no-default-features` keeps the desktop-free base/static build, with CUA operations returning unavailable after authentication.

Old `COORWORKER_CUA_*` mode/worker configuration and `CUA_ENV_TOKEN` are rejected at production startup with an actionable migration error. Remove these variables and use the envd credential. `/cua/mcp`, extension discovery and independent `coorworker-cua` startup are removed. Historical acceptance documents describe older revisions and do not validate this native API.

## Implementation

The native service and HTTP handlers are implemented in `src/cua.rs`,
`src/cua/` and `src/http/cua.rs`, with admission in the shared S0 middleware
and startup wiring in both daemon backends. Desktop platform crates are
optional dependencies linked directly from envd; the extension framework,
MCP transport and standalone worker runtime have been removed. Production
startup rejects the retired configuration rather than silently changing its
meaning.

The desktop adapter wraps registry calls in the vendored
`work_scope::InvocationScope`. It tracks finite blocking, async and raw-thread
handoffs, including nested work, before enqueueing and drains them before
releasing admission, including on timeout, disconnect or panic. A future bound
before enqueueing retains its caller-selected runtime and releases its marker
if dropped unpolled. Finite GNOME compositor requests preserve their absolute
deadline and reject queued work once it has expired, including before every
poll of a request already waiting on its own I/O and before helper invocation;
finite accessibility probes also propagate the scope. Persistent
session resources and event loops keep their existing owners and stay outside
the per-call drain. A wedged OS call can therefore keep admission held after
the HTTP deadline until completion or process termination; completion tracking
does not guarantee native cancellation. The patch groups and update obligations
are recorded in [vendor provenance](../vendor/cua-driver/PROVENANCE.md).

## Verified local state

The integrated working tree on `feat/native-embedded-cua` passed the latest
full test runs below. Counts exclude nested filtered subprocess summaries.

| Gate | Recorded result |
| --- | --- |
| Default desktop tests | 907 passed, 0 failed; 24 doctests ignored |
| Desktop-free base tests | 881 passed, 0 failed; 24 doctests ignored |
| Strict Clippy, default and base | Final all-target checks passed with warnings denied |
| Formatting | Final check passed |
| Windows base MSVC crosscheck | Final library/binary check passed; compilation evidence |
| Distribution checks | Final 17 checks passed |
| Notices verification and rebuild | Passed |
| Focused completion/native/Wayland reruns | 15 / 17 / 5 passed after the deadline fix |

The latest full tests include 17 isolated native API contracts in each build,
five Wayland fixture regressions in the desktop build, and 15 isolated
completion regressions in the desktop build. The completion target also passed
separately: deadlines, disconnect and panic; nested blocking/async/raw-thread
work; markers acquired before enqueueing; caller-selected runtimes; and all
three exact compositor deadline-helper cases. The added resumed-I/O case
proved that an inner request must not dispatch after its deadline when its
previously pending I/O becomes ready. `BeforeDeadline` checks the absolute
deadline and timer before every inner poll, including resumed polls. The
actual registry adapter's in-memory completion test passed in the full default
matrix. These fixtures do not operate the real desktop.

### Scoped root-module line coverage

| Root module | Covered / instrumented lines | Line coverage |
| --- | --- | --- |
| `src/cua.rs` | 187 / 197 | 94.92% |
| `src/cua/desktop.rs` | 195 / 200 | 97.50% |
| `src/http/cua.rs` | 104 / 115 | 90.43% |

These final post-deadline-fix and post-atomic-correction measurements cover
only the listed files, not the whole project or branches. The coverage run
executed all 289 library tests,
15 completion regressions, 17 native API contracts and five Wayland fixtures
without filtering; all passed. Dependency coverage exports were empty, so no
numeric `work_scope` coverage is claimed.

### Observed Linux smoke

A rebuilt Linux daemon smoke observed 63 tools, one envd process with no child
processes and one owned TCP listener. The removed MCP endpoint returned 404,
JSON-RPC input returned 400, and obsolete configuration was rejected at
startup. No desktop actions were performed. The final rebuilt repetition after
the latest deadline fix passed with the same results.

The final Rust source recheck closed the deadline finding, confirmed timer
wake-up behavior and reported no remaining blocking defect, alongside the
code/security follow-up. All final remaining checks and the rebuilt smoke
passed. The cached-advisory-database audit reported zero vulnerabilities and seven
unmaintained-dependency warnings; those warnings remain declared, and the
cache does not establish advisory freshness.

## CI toolchain correction

The first remote Linux base run passed its tests but failed strict Clippy on
Rust 1.99.1 because `AtomicU64::fetch_update` became deprecated; the initial
local matrix used Rust 1.98.1. The latency accumulator now uses a
`compare_exchange_weak` retry loop, preserving the existing saturating sum and
relaxed ordering on both compiler versions. No lint allowance or toolchain pin
was added. After the correction, default/base all-target strict Clippy passed
on Rust 1.98.1, base all-target strict Clippy passed on Rust 1.99.0, and the
289 library, 15 completion, 17 native and five Wayland tests passed again.
The scoped coverage table above was refreshed from that run. A targeted code
review found no blocking defect in the correction.

The next remote desktop run passed build, tests, strict Clippy and formatting,
then failed the dependency-graph gate because the runner did not provide `rg`.
The shared Linux prerequisite step now explicitly installs `ripgrep`. Both
profiles' graph checks must be rerun with that tool present: the base guard's
`if rg ...` could otherwise treat a missing command as an absent dependency.

## Acceptance limits

This is local implementation and verification evidence. Native Windows
desktop execution and live S0 Windows acceptance have not been run for the new
API; the Windows base crosscheck does not establish them. No local musl
compiler was available, so this migration has no fresh local static musl build.
No fresh Go differential or official SDK conformance campaign was run.
Historical acceptance documents retain their older revision scopes.

Delivery and subsequent remote CI results are tracked in
[PR #5](https://github.com/shon-yuan/rust-envd/pull/5). This local verification
record does not establish remote CI success, deployment or real desktop action.
Further interactive acceptance must use an isolated fixture; real desktop
access is outside this migration's authorization.
