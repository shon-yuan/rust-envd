# Dependency security maintenance

## Scope — 2026-11-04

This follow-up to CUA integration `0155255` updates the smallest coherent dependency set for the recorded Tokio and zstd advisories. It preserves the frozen protocol, optional desktop boundary and existing vendor patches. No real user desktop access, deployment or broad dependency refresh is part of this work. Desktop verification uses a private Xvfb display.

Baseline audits were executed separately against the root and standalone worker lockfiles with `cargo audit --no-fetch --file <lockfile> --json`:

| Lockfile | Vulnerabilities | Unsound warnings | Unmaintained warnings |
| --- | ---: | ---: | ---: |
| Root | 3 | 11 | 7 |
| Standalone worker | 3 | 10 | 7 |

Both baseline audits exited 1. Root resolved Tokio 1.53.2; the worker already resolved 1.54.1. Both resolved zstd 0.13.4, zstd-safe 7.4.0 and zstd-sys 2.1.2+zstd.1.5.7.

## Upgrade plan

1. Raise direct Tokio minimums to 1.53.3 and resolve the root to that patch release, retaining the worker's already-fixed Tokio version.
2. Review descriptor ownership, close/dup behavior, cancellation and handover before replacing deprecated AsyncFd constructors with the new unsafe registration API. Record each safety invariant at its callsite; do not suppress deprecation warnings.
3. Raise the vendored workspace's zstd requirement to 0.14.3, resolving zstd-safe at least 8.2.0 in both lockfiles. Record this local manifest patch in vendor provenance. The zstd 0.14 license is BSD-3-Clause; regenerate both notice bundles.
4. Check the lockfile delta for unrelated changes. Re-run focused process/PTY/OOM/handover and CUA contracts, full relevant suites, strict Clippy and Windows base crosscheck. Verify theme decoding with the upgraded library and existing artifact format.
5. Re-run both audits, record exact remaining warnings and preserve the distinction between package advisories, API reachability and platform acceptance. Rollback is a revert of this maintenance change and both lockfile/notice updates together.

## Primary sources

- [Tokio 1.53.3 release](https://github.com/tokio-rs/tokio/releases/tag/tokio-1.53.3): AsyncFd registration APIs and I/O-safety migration.
- [Tokio advisory](https://rustsec.org/advisories/RUSTSEC-2026-0386.html).
- [zstd 0.14.3 package](https://crates.io/crates/zstd/0.14.3) and [zstd-safe 8.2.0 package](https://crates.io/crates/zstd-safe/8.2.0): fixed versions, dependency requirements, Rust minimums and licenses verified against registry metadata.
- [zstd upstream security fixes](https://github.com/gyscos/zstd-rs/pull/384).
- [zstd 0.14 migration](https://github.com/gyscos/zstd-rs/releases/tag/v0.14.0): prepared dictionary lifetimes, decoder finish behavior and license change. The current theme loader uses a fresh read decoder and read-to-end, without prepared dictionaries or finish calls.

## Dependency resolution and migration evidence

The resolved delta is limited to root Tokio 1.53.2 → 1.53.3 and zstd 0.13.4 → 0.14.3 / zstd-safe 7.4.0 → 8.2.0 in both consumer lockfiles. Worker Tokio remains 1.54.1; zstd-sys remains 2.1.2+zstd.1.5.7. No unrelated resolved versions changed.

After resolution, both `cargo audit --no-fetch` runs exited 0 with zero vulnerabilities and zero unsound warnings. Each still reports seven unmaintained dependencies: atomic-polyfill, basic-toml, ttf-parser, unsafe-libyaml, zbus_names, zvariant and zvariant_derive. This is not a claim that the dependency graph has no maintenance risks.

Strict Clippy after the Tokio patch produced the intended migration RED: eight deprecated AsyncFd constructors (seven production sites and one test), before changing those callsites. Source security review also identified an unchecked invalid-FD fallback during handover relocation and a check-then-overwrite race during handover export. The corrections below are required to assert the new registration safety preconditions.

The relocation fallback that constructed `OwnedFd` after any duplication error is removed: a closed nonnegative descriptor now returns EBADF rather than manufacturing invalid ownership, and a resource error fails that process's adoption with cleanup. Export uses atomic `F_DUPFD` allocation and accepts only the requested free slot; it never overwrites an occupied descriptor. Incoming `readopt_processes` is now an unsafe Rust API with an explicit exclusive-transfer contract for inherited raw descriptors. It validates the whole batch before consumption and moves all accepted endpoints into RAII storage before its first await. This tightens the Rust ownership boundary; no HTTP/protobuf field or authentication policy changes.

The first upgrade regression pass completed successfully: root base 893 passed / 0 failed / 24 generated doc tests ignored; root desktop 897 / 0 / 24; standalone worker 21 / 0. These totals exclude eight nested child-harness result lines from each root suite. Base, desktop and worker strict all-target Clippy passed, as did Windows MSVC base lib/bins crosscheck, both fmt checks, 11 distribution tests and generation/check of both notice bundles. Cargo also reports an existing future-incompatibility warning for nom 1.2.4.

The first runtime pass used a real desktop-enabled envd on a private Xvfb display: it initialized and listed 63 tools, decoded the existing embedded `cua.default` theme with zstd 0.14.3, and returned an 800×600 PNG. It had no worker child before desktop execution; pre-init CUA admission returned 401 and S0 header separation passed. The initial corrupt-theme check only proved rejection of an invalid theme ID; it did not establish decoder rejection and was replaced by a valid reverse-DNS ID plus a payload-decoding error assertion. No real user desktop was accessed.

A follow-up security review found two additional export ownership issues: stdin/PTY raw numbers could outlive their source owners before duplication, and cancellation while waiting after slot allocation could leak inheritable slots. Checkpoint `d3d264e` records two deterministic runtime REDs and the missing PTY ownership-snapshot API's compile RED. Export now keeps the stdin mutex guard and a strong PTY registration reference through duplication, and owns allocated slots in `HandoverFdGuard` from the first allocation. Errors and cancellation release the partial snapshot automatically.

All 14 handover unit tests pass after the fix, including the previously failing exports and PTY snapshot. Additional controls verify rejection of a closed batch tail without consuming its prefix, EOF on all eight current/pending endpoints after readopt cancellation, and cleanup after a later registration fails while a valid sibling still adopts successfully. The process-runtime handover filter also passes all 20 selected parent tests. The fixtures use isolated single-test subprocesses and a current-thread reactor, reserving fixed slots before Tokio initialization; no random race stress or real desktop is involved. This replaces the initial thread-private FD-table fixture after review identified that process-global Tokio signal descriptor numbers could overlap its fixed window. An observed full-suite failure in the existing adopted-worker probe test was also corrected by retaining its listener throughout and toggling a health response from 503 to 204, rather than releasing and reusing an ephemeral port.

## Final validation

| Check | Result |
| --- | --- |
| Root base full suite | 905 passed, 0 failed, 24 generated doc tests ignored |
| Root desktop full suite | 909 passed, 0 failed, 24 generated doc tests ignored |
| Standalone worker suite | 21 passed, 0 failed |
| Strict all-target Clippy | Base, desktop and worker passed |
| Windows base MSVC lib/bins | Crosscheck passed |
| Formatting | Root and worker passed |
| Distribution tests | 11 passed |
| Generated notices | Both bundles generated and checked; 983 root / 864 worker license files |
| Dependency audits | Both exit 0; 0 vulnerabilities, 0 unsound warnings, 7 maintenance warnings each |

The final root totals count parent harness results only. The worker, Windows and distribution results came from the first upgrade pass; their code/graphs were unchanged by the final Linux export and fixture corrections. The full root suites and root lint were rerun after those corrections.

Targeted LLVM instrumentation of the four FD helpers (`validate_carried_fds`, `relocate_carried_fds`, `relocate_carried_fd`, `dup_keep`) in the process-runtime harness covered 77/87 regions (88.51%), including their nested closures; all four helper entry points executed. This is a scoped region metric, not whole-repository line coverage. Branch coverage was not enabled.

After explicitly building the desktop binary, the final private Xvfb acceptance passed with 63 tools and an 800×600 PNG, successful existing embedded-theme decoding, and rejection of `test.corrupt` specifically at payload decoding. Pre-init 401, S0 header separation and absence of a worker child before desktop execution also passed. No real user desktop was accessed.

Independent code, Rust and security reviews closed the export ownership findings; the final test-fixture review found no remaining actionable issue. Checkpoint `419bf55` preserves the invalid-relocation/aliasing RED; `d3d264e` preserves the export lifetime/cancellation and PTY snapshot RED. Maintenance warnings without a patched compatible release are not silently ignored or treated as fixed.

## Evidence limits

At maintenance commit `c3cb81a`, the checks above had not rerun the Go oracle differential, native Windows desktop/service acceptance, or a local musl build. The subsequent [runtime regression record](dependency-regression.md) records fresh results separately. The Windows base MSVC result is a crosscheck, and the earlier CUA integration's six successful remote CI jobs apply to `0155255`, not this maintenance branch. The relocation cleanup on forced FD exhaustion (EMFILE) is source-reviewed but not exercised by an exhaustion fixture. The tests do exercise invalid/aliased input, registration failure, cancellation and normal chained handover. No release artifact, deployment, merge or push is implied by local checks.
