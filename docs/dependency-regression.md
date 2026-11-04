# Dependency maintenance runtime regression

## Candidate and isolation — 2026-11-04

This follow-up tests maintenance commit `c3cb81a3428115bca25046f8acbc49cb12495036`, after the local suites recorded in [Dependency security](dependency-security.md). It does not reuse the earlier integration revision's CI as evidence for this commit.

The fresh Linux release build used `--locked --offline -j4 --target x86_64-unknown-linux-musl --release --bin coorworker-envd`, with Clang and the existing freestanding ring flags. The frozen base candidate is a static PIE with no ELF interpreter:

```text
SHA-256 4dfcab2636c42ef308cdb8cafca9237de16bd2a3bfea834672a3b53b764bd789
```

The official Go oracle was rebuilt from runtime commit `51083a83cc0903185d992725ea6bb33683814b29`, envd 0.10.2, using pinned Go 1.26.10 and bounded build parallelism (`GOMAXPROCS=4`, `go build -p=4`). The source archive SHA-256 is `247166f90c4eb2931c9cca26edc51f5fe73de17cb4cadf487f1096410b834071`; oracle binary SHA-256 is `2fe9ca624cedb8260b7f57437e085b0792851408d16fbfa445b0832a63f91a55`. Its version and commit flags passed the builder's identity checks. An initial direct-network toolchain download timed out; the successful retry inherited only configured proxy variables in addition to the builder's explicit environment.

Protocol tests use disposable QEMU/KVM initramfs guests, 384 MiB and two vCPUs each, with restricted user networking, numeric loopback host forwarding and no guest default route. No persistent VM disk or existing service is modified. Upgrade relay dependencies come from Alpine 3.20, matching the supplied rootfs release family; their archive/file hashes and ELF dependency closure are recorded. Downloaded binaries run only inside these guests. The OOM helper runs only inside a memory-bounded guest.

## Completed observations

| Check | Observation |
| --- | --- |
| Linux musl release build | Passed; frozen candidate hash above |
| Expanded Go/Rust protocol differential | Passed; 10 observation groups per side, no fixture failures or semantic mismatches |
| HTTP/1 edge differential | Both sides passed; 71 cases, normalized documents identical |
| Malformed-input differential | Both sides passed; 334 cases, normalized documents identical |
| Re-exec upgrade differential | Both sides passed all five scenarios; normalized documents identical |
| Install upgrade differential | Both sides passed all five scenarios; normalized documents identical |
| Parity harness unit tests | 248 passed, 6 integration tests deselected |
| Embedded CUA private Xvfb smoke | Passed; 63 tools, 800×600 PNG |

The protocol groups are health/auth/envs/init, realtime start, nonzero exit, stdin/List/Connect, protocol version, filesystem move, filesystem ListDir, process signal, upgrade errors and legacy SDK edges. `--expanded` enables these selected expanded cases; this is not every specialized VM gate.

The CUA smoke uses the previously built desktop-enabled debug binary from the same maintenance source (SHA-256 `d133f1e8aa1c48870f13ca9f45ebe54aa44afd1a925571e2b14625735785ecba`), independently of the frozen base musl binary. It creates a private Xvfb display and sanitized environment. It verifies pre-init admission 401, S0 header separation, shared envd credential admission, zero worker children before desktop execution, decoding the existing `cua.default` theme, and rejection of the valid-ID `test.corrupt` theme specifically during compressed payload decoding. No real user desktop is accessed.

## Upgrade and raw-protocol acceptance

Both re-exec and install completed on both sides. All four invocations exited 0 without a timeout and passed all five scenarios, using 20 disposable upgrade guests in total. Each mode's parsed result documents are identical after removing only root side/version/commit metadata. No `retained` field or other semantic observation had to be removed. The frozen candidate hash remained unchanged after the campaign.

Each upgrade run exercises ordinary pipe and PTY handover, process/tag reconnects, watcher drain/re-arm, forwarder preservation, pre-init and failed-blob-write refusal, guest OOM history reconstruction after three kills, eight alternating-mode chained upgrades, one further upgrade after listener restart, and the non-loopback binding filter.

The HTTP/1 edge corpus has 71 cases. The malformed corpus contains 110 HTTP, 18 route, 174 RPC, 8 CORS and 24 fixed-seed random cases. Both gates completed on fresh Go and Rust guests. Their parsed JSON documents are identical after removing only the root side/version/commit metadata; all corpus values remain significant. Each side also remained healthy after its corpus.

## Evidence and limits

Local evidence root: `/tmp/envd-test-c3cb81a-evidence-b5bns7tg`. It contains the frozen candidate, candidate identity, Go build manifest, guest relay provenance, protocol result JSON, harness log and private-Xvfb result. `protocol-comparisons.json` and `upgrade-comparisons.json` record the inspected pairwise comparisons; `upgrade-runs.json` records all four run outcomes and durations. Each upgrade run also has its complete result JSON and a log naming the retained guest artifacts. The protocol VM's serial logs and image manifest are under `/tmp/envd-oracle-vm-d12yjeju`. These temporary paths are local receipts, not published artifacts or durable downloads.

No new native Windows user-session/service acceptance or remote CI run has been completed for this branch. A Windows VM disk and DHCP lease were discoverable, but no dedicated test-session connection was identifiable in the available configuration; neither the disk nor lease proves isolation or current liveness. The earlier MSVC base crosscheck remains a crosscheck. Forced FD exhaustion (EMFILE), whole-repository coverage and complete production-environment parity are not established by these runs. No deployment, merge or push is performed.
