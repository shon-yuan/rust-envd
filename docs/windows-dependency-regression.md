# Windows dependency maintenance regression

## Candidate and environment — 2026-11-04

This native Windows follow-up tests maintenance commit
`c3cb81a3428115bca25046f8acbc49cb12495036`. It complements the
[Linux runtime regression](dependency-regression.md) and the earlier local
checks in [Dependency security](dependency-security.md). Earlier integration
CI is not evidence for this maintenance revision.

A `git archive` of that commit was transferred to a new directory on the
existing Windows 11 validation VM. The archive SHA-256, verified again on
Windows before extraction, is
`67a2692834ff0e131257313113c042d8fd4b36e13db657c7686acc3d033a1ffd`.
The existing source checkout was preserved. Source, Cargo target files,
logs and executable copies use the new campaign directory. The VM is an
existing machine, not a disposable or exclusively owned fixture.

| Property | Observed value |
| --- | --- |
| Windows version | 10.0.26300.0, x86_64 |
| Execution context | OpenSSH command session, Session 0 |
| Rust | rustc 1.99.1 (`58cf8f9c3`, 2026-10-13) |
| Cargo | 1.99.1 (`5f94df478`, 2026-08-27) |
| Protobuf compiler | libprotoc 36.1 |
| Visual Studio | 2022 Build Tools 17.14.37827.9 |
| MSVC toolset | 14.44.35207 |
| Build/test limits | `--locked -j4`, sequential Cargo stages; `--test-threads=1`, `TOKIO_WORKER_THREADS=2` |

The desktop build initially exited 101 because `msvc_spectre_libs` could
not find the matching MSVC Spectre libraries. Installing
`Microsoft.VisualStudio.Component.VC.14.44.17.14.x86.x64.Spectre` into the
existing Build Tools instance exited 0, with no restart requested or
performed and no installer UI. The pipeline then resumed at desktop build,
retaining the two completed base stages. No Rust source, dependency feature,
lockfile or mitigation setting was changed to bypass this prerequisite.
See [Packaging](../third_party/PACKAGING.md#canonical-build).

## Native build and test results

Every stage below exited 0. Test totals count each parent test harness once;
isolated subprocess output is not added again.

| Stage | Result |
| --- | --- |
| Base envd build | Passed, debug executable |
| Base library and selected CUA contracts | 321 library + 5 integration tests passed; 0 failed or ignored |
| Desktop envd build (`cua-desktop`) | Passed, debug executable |
| Desktop library and selected CUA contracts | 321 library + 9 integration tests passed; 0 failed or ignored |
| Standalone worker build | Passed, debug executable |
| Standalone worker library, binary and integration tests | 16 passed; 0 failed or ignored |

The base invocation was:

```powershell
cargo test --locked -j4 --lib --test cua_context --test cua_admission --test cua_factory --test cua_ipv6 -- --test-threads=1
cargo build --locked -j4 --bin coorworker-envd
```

The desktop and standalone-worker invocations were:

```powershell
cargo build --locked -j4 --features cua-desktop --bin coorworker-envd
cargo test --locked -j4 --features cua-desktop --lib --test cua_context --test cua_admission --test cua_factory --test cua_ipv6 --test cua_embedded -- --test-threads=1
cargo build --locked -j4 --manifest-path cua-worker/Cargo.toml --bin coorworker-cua
cargo test --locked -j4 --manifest-path cua-worker/Cargo.toml --lib --bins --tests -- --test-threads=1
```

The native suite exercises the Windows backend, including its process and
ConPTY contracts. CUA integration tests cover admission, factory modes,
launch-context policy, IPv6, shared credentials and embedded MCP contracts.
The embedded real-provider check registers and lists tools; it does not
capture a desktop or execute input tools.

## Live daemon checks

The official E2B Python SDK 2.62.1 passed **27/27** against the newly built
base daemon through an SSH loopback tunnel. The trusted harness is the
agent repository's `scripts/tests/envd-parity/sdk_conformance.py`. It covers
health, init, token admission, metrics, file upload/download/range/compose,
filesystem operations and watches, process listing/signalling, retained
exits and stdin. Its Windows `commands.run` case verifies the declared
`invalid_argument` refusal of `/bin/bash -l -c`; it does not establish POSIX
shell support.

A separate live desktop executable ran with
`COORWORKER_CUA_MODE=embedded` in actual Session 0. It verified:

- Before `/init`, `POST /cua/mcp` returns 401.
- After installing a fresh test credential, `/cua/health` returns 503 with
  `auth_ready=true`, `ready=false`, `runtime_available=false` and
  `desktop_access="not-probed"`.
- `/cua/info` reports `mode="embedded"` and `auth_source="envd"`.
- An authenticated Bearer `tools/list` request returns 503: production
  launch-context detection refuses to initialize the embedded backend.
- S0 `/envs` rejects Bearer alone with 401 and accepts `X-Access-Token`
  with 200.

Both final live checks exited 0, including daemon shutdown. The test drivers
supplied fresh random credentials over HTTP; host model/provider keys were
not supplied to the Windows test daemon. The final process inventory found
no campaign processes and no active Cargo, rustc, MSVC compiler or linker.

## Artifact identity and receipts

These are native debug test artifacts, not release distribution manifests.

| Copied executable | Bytes | SHA-256 |
| --- | ---: | --- |
| `base-envd.exe` | 13,988,352 | `d23d557c59bfe45b18293dc1dc17386c85d2ef7552ce4731bec21f43c94d0e9d` |
| `desktop-envd.exe` | 53,318,144 | `5efdc3a70dbad9aeb2ab5ff0cbaba3b41e4197443c222d61e2b17e25b3d36367` |
| `coorworker-cua.exe` | 42,764,288 | `0cd3b608727e0c354aa49a0658ccb98dd87ba04932efb42e13b4f70e7e4cebf1` |

Local evidence root: `/tmp/envd-windows-c3cb81a-12np0y6q`. The guest campaign
root is `C:\envd\regression-c3cb81a-12np0y6q`. Receipts include
`native-identity.json`, `visual-studio-identity.json`, `native-results.json`,
`binary-manifest.json`, `spectre-install.json`, each stage's stdout/stderr, `sdk-result-retry.json`,
`sdk-retry.log`, `guard-result-retry.json`, `guard-retry.log`, both final
shutdown logs and `process-cleanup.json`. Temporary local paths are not
published artifacts or durable downloads.

The original failures remain recorded. The first native wrapper could not
read `Start-Process`'s exit code despite passing test output; a corrected
`Diagnostics.Process` wrapper reran the base stages and recorded exit 0.
The first SDK run passed 27 tests but its stdin shutdown handshake timed
out, making that wrapper fail; its own identified daemon was stopped, and
a bounded stop-file launcher then passed a complete SDK rerun and cleanup.
The missing-Spectre build failure is retained separately from the successful
resumed pipeline. These fixture/environment failures are not omitted or
counted as successful runs.

## Limits and remaining work

This run establishes native build, selected contract and SDK regression
acceptance for the recorded revision. It does not establish interactive
Session 1 capture, clicking, typing or application launch; actual SCM
registration/start/stop; or service-to-user-session helper operation. No
real desktop input or capture was performed. Session 0 refusal is not a
substitute for those acceptance scenarios.

There is no Windows Go oracle, so no Windows differential claim is made.
No fresh Windows Clippy, coverage, release-profile build, guest image
pipeline or remote CI run was performed here. The Rust 1.99.1 desktop/worker
build reports an `AtomicU64::fetch_update` deprecation and unused
`build_registry` / `current_token_write_denial_reason` functions in vendored
CUA crates; this is not a warning-free Windows build. The seven dependency
maintenance warnings disclosed in the security record remain outside the
advisory fixes. This campaign performs no deployment, merge or push.
