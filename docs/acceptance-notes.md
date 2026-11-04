# Acceptance notes

This is the working acceptance log for aligning this Rust daemon with the
pinned upstream envd 0.10.2; the entries below record what was built, how each
slice was verified, and what remains.

The target is the complete official envd 0.10.2 behavior at commit
`51083a83cc0903185d992725ea6bb33683814b29`, as inventoried in
the upstream surface matrix and
the internal implementation plan.

`build.rs` generates Buffa messages and Connect service traits from unchanged
pinned sources in `third_party/e2b`. It requires local `protoc`, performs no
network acquisition, and leaves the earlier Go bindings' provenance intact.
The current tests cover schema representations and real loopback Connect
streaming, including input before EOF, post-header deadlines and subscription
drop. The crate now also exercises real ordinary-user children, the five basic
filesystem RPCs and metadata, protected token storage, Basic username resolution,
init defaults/environment replacement, and idle stream keepalives. These tests
and the initial differential cases do not establish complete daemon parity.

A loopback-only development executable can be launched as an ordinary user:

```sh
cargo run -j4 --locked -p coorworker-envd -- --development --listen 127.0.0.1:0
```

It executes commands as the resolved local account and is for disposable local
acceptance instances. Development mode rejects root startup. The separate `--guest`
entrypoint requires root PID 1 and the exact kernel argument `coorworker.envd=1`;
it can be exercised with the isolated dual-VM runner harness.
This opt-in is a test startup guard, not full guest initialization. The CLI now
reproduces all eight oracle flags with Go `flag` semantics (both dash forms,
`=value`, base-0 int64 parsing, help output, `bad flag syntax` diagnostics,
`--`/bare-`-` terminators, exit codes); the metrics lifecycle inventory is
verified complete; and every system-init effect is implemented, including a
non-empty `volumeMounts` list, which mounts through the real `mount.nfs` path
and has its own isolated QEMU differential gate. MMDS init authentication now
has an injected provider and separate cancellation-safe init serialization. Guarded
Firecracker guest startup wires a fresh v2 token/metadata client and route repair;
development and `--isnotfc` retain the noop provider. The envd-side transparent
interception surface — the oracle's only in-guest proxy machinery — is the MMDS
iptables route pin (`internal/host/mmds_route_linux.go`) with its self-heal call
site (`internal/api/init.go` checkMMDSHash); the Rust side now mirrors it,
including the Go `os/exec` error text (`exec: "…"`, `fork/exec …`, `exit status
N`, `signal: …`) and the rate-limited zerolog warn on the envd logger. The
host-side proxies (client-proxy, orchestrator tcpfirewall) are separate binaries
outside this crate's parity target. The prior `fd9bb60f…f6ed28`
candidate accepted an unauthorized first token despite a ready metadata service at
`/tmp/envd-mmds-vm-f4os3x0e/result-1.json`; the Go oracle rejected it. New candidate
`ed4d919d6a4c3d62014d9f173cf16d26227743fa68f3746ae8622e52336a80b2`
passes those MMDS auth cases at `/tmp/envd-mmds-vm-i0ooal9h/result-1.json`, along with
14 regular groups, real CPU and OOM gates. Independent review then found HTTP gzip
multimember and redirect URL/userinfo differences. Those fixes now pass 21 MMDS tests;
the subsequent `643e727f…4e94d1` candidate passes the regular, CPU and OOM gates.
Follow-up escaped-hostname/scoped-IPv6 and Referer findings now have RED→GREEN
regressions and independent source closure, including unknown-zone fallback and
Unicode host serialization. New immutable candidate `01c641a0…94a53b6` passes the
14 regular groups, real CPU and OOM gates, and — after the harness gained an
atomic per-mode counter baseline and mutation-verified in-window evidence — the
expanded MMDS corpus at `/tmp/envd-mmds-vm-njx826c0/result-1.json`. IPv6 socket
acceptance is explicitly skipped where local IPv6 loopback is unavailable; pure
scope and Host tests do not establish a successful socket connection.
The MMDS metadata polling lifecycle (50 ms ticker, fresh PUT/GET per attempt,
internal env plus best-effort `/run/e2b` ID files, boot and post-init refresh
with Go-mirrored terminal diagnostics) then passed independent source review
with its cancel-path log-fidelity finding fixed RED-first. Immutable candidate
`27399e2b…d9d7d05` passes the 14 regular groups, real CPU, real OOM and expanded
MMDS gates (`/tmp/envd-oracle-vm-cbp5k20c`, `/tmp/envd-cpus-vm-bawtye35`,
`/tmp/envd-oom-vm-9xfve18b`, `/tmp/envd-mmds-vm-bzr_pw1a`), pinning 85 source
inputs; the mmds channel receiver is retained for the upcoming log exporter.
The hyperloop slice (optional `/init` `hyperloopIP` rewriting `/etc/hosts` in
txeh v1.8.1 shape plus the internal `E2B_EVENTS_ADDRESS` env, guest-gated)
then passed independent review with all findings fixed RED-first, and immutable
candidate `be78e0a1…504366` passes the same four gates
(`/tmp/envd-oracle-vm-g5gw7mzd`, `/tmp/envd-cpus-vm-nhcxx6m4`,
`/tmp/envd-oom-vm-s0uadwq_`, `/tmp/envd-mmds-vm-aamzxksd`) pinning 87 inputs.
The clock slice (correction header, latest-wins gate, strict ±50ms/5s bands
against the corrected timestamp, guest-only `clock_settime` sink) then passed
independent review with its escaped-RFC3339 finding fixed RED-first, and
immutable candidate `1139e2ed…d7c286` passes the same four gates
(`/tmp/envd-oracle-vm-0f5q7loz`, `/tmp/envd-cpus-vm-sc4o1azx`,
`/tmp/envd-oom-vm-wjdjhsrq`, `/tmp/envd-mmds-vm-18ku9r3x`) pinning 89 inputs.
The CA bundle slice (byte-exact PEM codec, intent-first state.json, guest-only
installer, 503-on-in-progress, bare-text init errors) then passed review with
its padding-mode and body-envelope findings fixed RED-first, and immutable
candidate `6f21492b…7a9836` passes the same four gates
(`/tmp/envd-oracle-vm-mh0dnqp6`, `/tmp/envd-cpus-vm-2mbtdy3n`,
`/tmp/envd-oom-vm-xfjbq42e`, `/tmp/envd-mmds-vm-_dcq7m7f`) pinning 93 inputs.
The cgroups/freeze slice (real cgroup v2 hierarchy sweep with the official
allowlist, spawn-barrier admission and drain, freeze/unfreeze state machines with
the thaw watchdog, memory-protection reporting, the `/init` freeze-audit header
with the 501 set narrowed to VOLUMEMOUNTS-only, and the Go cancellation
asymmetry: a registered thaw always lands while `/freeze` stays caller-bound
until the lock is held) then passed three independent review rounds with every
finding fixed RED-first, and immutable candidate
`4125b114067f98b0260fda257c42ce6fd9a9726d49ca5e5d5b172ccae67fb528` passes the
same four gates (`/tmp/envd-oracle-vm-jii1ys_d`, `/tmp/envd-cpus-vm-nmoaz2bb`,
`/tmp/envd-oom-vm-9285y_9z`, `/tmp/envd-mmds-vm-oz4pwdu3`) pinning 101 inputs.
The daemon never writes the host cgroupfs; it reads `/proc/self/cgroup` and
`/sys/fs/cgroup` for the memory-protection report and degrades to partial when
they are unavailable. Spawn process placement and frozen-resume integration are
deliberately staged for the live-upgrade slice. The full suite is 428 top-level
tests with strict all-target Clippy clean.
The MMDS log exporter slice (the official queue semantics — 8 MiB drop-oldest,
192 KiB single-line cap, snapshot-based flush that purges later arrivals,
start-once sender wired to the retained metadata-options receiver, zerolog-shaped
JSON with RFC3339Nano timestamps, rate-limited diagnostics, process start/end
lifecycle lines with byte counters, filesystem interceptor lines with operation
IDs, and the init-time flush budget) then passed independent review with its
filesystem-logger wiring finding fixed RED-first, and immutable candidate
`402ec6c2c7dbb02e70c7b5fed1d5d6f336124ad232a8ae99d5c7b89af1e2329b` passes the
same four gates (`/tmp/envd-oracle-vm-kbi3_8dv`, `/tmp/envd-cpus-vm-we8i3ehj`,
`/tmp/envd-oom-vm-vrkh9780`, `/tmp/envd-mmds-vm-19spaqr1`) pinning 110 inputs.
Process RPC interceptor lines and payload fields, the WatchDir disconnect
error-level end line, and the freeze-flush operation ID then closed as a
follow-up slice after review — including two adjudications: a disconnect end
line carries gRPC code 2 (the interceptor logs the bare context error; connect
only maps codes when building responses), and the logged method names match the
oracle's actual `process.Process`/`filesystem.Filesystem` procedures exactly.
Immutable candidate
`60e629e6ee3e5f8fcd7386eeb1175f04fb30679144cf1359d8c162c606978e89` passes the
same four gates (`/tmp/envd-oracle-vm-ls7cxkhm`, `/tmp/envd-cpus-vm-dxrsxmh2`,
`/tmp/envd-oom-vm-0map86qg`, `/tmp/envd-mmds-vm-8k43lpx3`) pinning 113 inputs;
the full suite is 487 top-level tests with strict all-target Clippy clean.
The ports/loopback forwarder slice (the official forwarder contract: gateway
169.254.0.21, insert-before-start tracking where failed spawns stay tracked
dark until their listener returns, frozen refusals retried after thaw, socat
process-group kill on stop, export/import with live-pid adoption, a
second-interval scanner with rendezvous subscriber semantics, and /proc/net
parsing that renders v4-mapped IPv6 listeners as dotted IPv4 exactly like Go)
then passed two review rounds — including a HIGH fix where the v4-mapped
rendering was wrong and its test had pinned the wrong expectation, corrected
RED-first with a real `::ffff:127.0.0.1` listener — and immutable candidate
`c12658df2443aa091edbafcc5c225b49240bcd484d5707a035f4beccffda79cc` passes the
same four gates (`/tmp/envd-oracle-vm-ty92xgeh`, `/tmp/envd-cpus-vm-c7bwtq29`,
`/tmp/envd-oom-vm-h_0pk7mg`, `/tmp/envd-mmds-vm-reo6o61w`) pinning 118 inputs.
Handover wire encoding and the resume flag then landed in the subsequent
live-upgrade slice; the suite stood at 520 top-level tests at that checkpoint.
The revised checkpoint passes 286 top-level Rust tests plus an
isolated TLS child test, with 88.53% implementation line coverage; the HTTP/TLS and
retry fixtures are scoped tests. The route module now has 72.01% line coverage:
the runner's lookup/exec machinery is exercised through stand-in programs
(Go `os/exec` error texts, signal/errno tables, cancellation kill) while the real
iptables argv execution itself stays reserved for isolated guests.
GET `/metrics` now collects real Linux CPU intervals, memory and
statfs data with pinned formulas, no-store JSON and empty collection-error 500s.
Its CPU manager now persists targets, reconciles Linux online masks through tracked
helper children, and reports status on `/metrics` and `X-Envd-Cpus`. Guest mode without
`--isnotfc` activates it; development and non-Firecracker modes use the noop manager.
CPU acceptance passed for immutable candidate
`fd9bb60fe97ed6c0a870386ef49248b9d155e741bf77ec85305e583561f6ed28`
at `/tmp/envd-cpus-vm-p2ulakog/result-1.json`: grow/shrink, CPU0 retention, persisted
targets, exact advisory rejections, and authentication preservation. The earlier
activated candidate's init-auth-body failure remains recorded in the generation gate.
Startup/cancellation ordering has deterministic regression tests.
The live-upgrade/handover slice (the handover blob with schema and stamp
discipline; the outgoing freeze-hold → locked exports → blob write → execve
choreography with lock-drop-then-thaw on every non-exec exit; the full incoming
`--resume-handover` flow — early defaults restore, helper adoption before the
CPU manager, retained-exit restore, blob removal only on success, watcher
re-arm under the freeze hold, the guest-frozen record, the mount ledger, the
`X-Envd-Handover` outcome header, and a 60-second fallback thaw gated on
Initialized at fire time; plus the forwarder export hold, pre-task import, and
the context-cancellation socat kill) then passed three independent review
rounds. The reviewer confirmed the whole surface map and all twelve disclosures
against the oracle source and found two HIGH defects — the upgrade choreography
was bound to the HTTP connection so a disconnect could strand the guest frozen,
and a lock-order inversion between the daemon state guard and the freeze permit
could deadlock a failing /init — both fixed RED-first, plus a coverage gap
closed with five direct tests and a vacuous fallback-thaw test rewritten with
mutation RED. Immutable candidate
`d978b697d3e1b96b850f3ac8f428f8c3cfd64864878385abd1e40a5865825d59` passes the
same four gates (`/tmp/envd-oracle-vm-r2u3nv8l`, `/tmp/envd-cpus-vm-8h7gtu9t`,
`/tmp/envd-oom-vm-04acpful`, `/tmp/envd-mmds-vm-44dkqen0`) pinning 120 inputs;
the full suite is 551 top-level tests with strict all-target Clippy clean.
The record-closure slice for network interception then verified — and
independent review confirmed by exhaustive enumeration of the oracle tree —
that the oracle envd 0.10.2 contains no transparent proxy: the only in-guest
interception surface is the MMDS iptables route pin and its self-heal (already
mirrored, above), so the recorded transparent-proxy item closed as outside the
envd parity target (the host-side proxies are separate binaries). Two observable
deltas closed RED-first — the pin-failure warn became the rate-limited zerolog
event on the envd logger, and the iptables runner now reproduces Go's os/exec
error texts including LookPath semantics and the errno/signal name tables —
with four mutation-checked discriminating tests. Immutable candidate
`c8d4599ef551006e0d78a752acc1e587507b9c5ac28965761c94407cee107e6d` passes the
same four gates (`/tmp/envd-oracle-vm-y686tu6x`, `/tmp/envd-cpus-vm-bzz6zwt7`,
`/tmp/envd-oom-vm-0_qvt_4k`, `/tmp/envd-mmds-vm-k0k7r36q`) pinning 121 inputs;
the full suite is 562 top-level tests in 45 suites (an interim 563/46 headline
double-counted an isolated subprocess's own summary line) with strict
all-target Clippy clean.
The differential-corpus slice (the complete operation inventory — thirteen
scenario cases plus the bootstrap make the fourteen standard observation groups
the gate has always reported, plus an opt-in `--expanded` registry adding eight
groups for 22 total, and a freeze-lifecycle gate whose real guest cgroup2
freeze runs are bounded by 2 s freeze / 5 s abandoned-init / 300 s suite
timeouts) then passed two review rounds. The expanded corpus exposed four HIGH
parity gaps, all fixed RED-first: bare `/freeze` 204 carries no headers (only
the structured report sets `no-store` + JSON); errno texts now come from a
Go-identical table (26 divergences over 1..133, 23 encodable); the getProcess
selector texts are exact (`process with pid %d not found` / tag form,
CloseStdin included after a fix round); and the guest freezer is one shared
instance between the daemon and the process runtime. The fix round closed the
fsfreeze success-predicate asymmetry (EBUSY only for freeze, EINVAL only for
thaw), the disconnect-guaranteed `/fsthaw` thaw, Go-`%q`-exact quoting via a
shared `goquote` module, and context cancellation in `/collapse`. Counting
note: the `tests/mmds.rs` self-exec child's own summary line is not a suite —
corrected totals are 582 passed / 0 failed / 24 ignored in 45 suites. Immutable
candidate
`289dded566e50f9ca2e8429b39ac3cf941716e0133a33518276cf46ae11c389c` passes six
same-hash VM gates — the standard four (`/tmp/envd-oracle-vm-r7f8j041`,
`/tmp/envd-cpus-vm-jg4x5ogj`, `/tmp/envd-oom-vm-uv_u697f`,
`/tmp/envd-mmds-vm-zbwrdvta`) plus the expanded differential
(`/tmp/envd-oracle-vm-gcpccwdv`, two repeats) and the freeze lifecycle
(`/tmp/envd-freeze-vm-17sl8nql`, three repeats) — pinning 125 crate inputs with
the harness snapshotted separately; Clippy and fmt are clean and no dependency
was added.
The real-guest-probe slice (differential probes against the real guest kernel
for the three behaviors previously covered only by injected tests — clock
correction through a real `clock_settime` step with the newest-wins gate and
band bounds, CA trust through a real in-guest `openssl verify` of the settled
`base + canonical` bundle with idempotent reinstall and CA2-retires-CA1, and
the six cgroups items through the real hierarchy with allowlisted socats,
exact freeze-audit header, thaw-before-204, and memory-protection equal to
real /proc reads) then passed independent review with all three probe-exposed
gaps fixed RED-first: escaped-RFC3339 correction values are now rejected
exactly like the oracle (raw-byte timestamp parsing), spawn places the child
before the credential drop (the candidate's disclosed pre-exec mechanism — the
oracle itself places via CLONE_INTO_CGROUP without writing cgroup.procs), and
PTY start errors carry the oracle's exact shape. The report's LOW follow-up
(the spawn error-code table lacks the oracle's ENOSPC→ResourceExhausted
mapping, pre-existing) is recorded for a future slice. Immutable candidate
`db7e06fdd1c6eb7aa17f5221956d7a019e28a06041a8f98fdcf793591e8e7c01` passes
seven same-hash VM gate families — the standard four
(`/tmp/envd-oracle-vm-qg5w81w_`, `/tmp/envd-cpus-vm-qz4faeds`,
`/tmp/envd-oom-vm-zsim3_mn`, `/tmp/envd-mmds-vm-fh98lgf5`), the expanded
differential (`/tmp/envd-oracle-vm-7xvaffsf`, two repeats), the freeze
lifecycle (`/tmp/envd-freeze-vm-dv8f79zk`, three repeats), and the new guest
probes (`/tmp/envd-guestprobe-vm-qy3afhj3`, two repeats, six groups each) —
pinning 125 crate inputs; the full suite is 584 top-level tests in 45 suites
with strict all-target Clippy clean.
The filesystem unary payload and spawn-error closure slice then aligned every
filesystem RPC's captured payload with the oracle (interceptor.go:52-127,
filesystem.pb.go tags, zerolog v1.35.1): a new `daemon/filesystem_payload.rs`
encodes Go-tag field names, omitempty semantics, enum numbers, nested
timestamps, sorted map keys and optional pointers for all eight unary methods
plus the WatchDir stream start line and clean-end `response: null`, closing six
RED tests. Independent review caught a new HIGH defect no earlier round
examined: unary success lines must log at `info` (zerolog maps `Err(nil)` to
`Info()`), now fixed via `unary_event()` with the failure path at error level
and streams unchanged. The spawn error-code table was audited across all nine
rows: ENOSPC now maps to `ResourceExhausted`, the deadline rows became a real
pre-fork refusal (code 4, deadline armed per start.go:40-44 with the
exec.CommandContext refusal) plus a `SpawnBarrier` mirroring spawn.go:183-206
admission order, and caller cancellation keeps future-drop semantics with the
same wire-observable end state. Spawn failures now carry the oracle's full
PathError text (`fork/exec /bin/sh: …`, `open /dev/ptmx: …`) through a
`PathTextError` whose source chain preserves errno classification, with
exact-equality assertions; non-ENOENT cwd stat failures fall through to the
wrapped spawn text, and grantpt-family setup failures keep bare errno text
because the oracle's own ioctl surfaces are bare too. Three review rounds ended
APPROVE. Immutable candidate
`b3d7efecbe2cab1491bd5bccf6fc21acf70b9a7ab68bbe07ee8c2327d53b73b9` passes
seven same-hash VM gate families (standard 14-group
`/tmp/envd-oracle-vm-4qrjarph`, CPU `/tmp/envd-cpus-vm-rhkjxmmp`, OOM
`/tmp/envd-oom-vm-xszeikig`, MMDS `/tmp/envd-mmds-vm-1u9a3b3q`, expanded
`/tmp/envd-oracle-vm-r3qhq4qc` twice, freeze `/tmp/envd-freeze-vm-8nlyeoee`
three times, guest probes `/tmp/envd-guestprobe-vm-wmu3n7sb` twice) pinning
126 crate inputs; the full suite is 605 top-level tests in 45 suites with
strict all-target Clippy clean.
The declaration-audit slice then closed the README's remaining unfinished
declarations with executed evidence: the CLI now reproduces the oracle's full
`flag` behavior — all eight flags, both dash forms, `=value`, base-0 int64
parsing with Go's underscore rules, help output, `bad flag syntax`
diagnostics, `--`/bare-`-` terminators and exit codes — locked by unit and
integration tests (an independent review round probed 101 argument vectors
against the oracle binary, overturned the first pass, and the fix round
reached approval); the metrics lifecycle inventory (metrics RPC shapes,
sampling against pinned gopsutil v4.26.6, OOM watcher start/stop/reopen with
the 32-entry bound and non-consuming readers, CPU manager) is verified
complete, with the cpu-online helper's boot-identity validation recorded as an
intentional hardening deviation; and every `/init` effect except a non-empty
`volumeMounts` list is implemented — `null`/`[]` are absent no-ops exactly
like the oracle, while a non-empty list keeps answering 501 as NFS mounting
stays blocked pending separate authorization. Immutable candidate
`045c5291613e2e74edd09e88357e67bddf846ec461de67315607e96e42f10734` passes
seven same-hash VM gate families (standard 14-group
`/tmp/envd-oracle-vm-fv8g2p9y`, CPU `/tmp/envd-cpus-vm-j7_atrve`, OOM
`/tmp/envd-oom-vm-ekh9u9cd`, MMDS `/tmp/envd-mmds-vm-u3dmk2uy`, expanded
`/tmp/envd-oracle-vm-8e49mdi4` twice, freeze `/tmp/envd-freeze-vm-xd7q1c_k`
three times, guest probes `/tmp/envd-guestprobe-vm-5ody7tny` twice) pinning
126 crate inputs; the full suite is 626 top-level tests in 45 suites with
strict all-target Clippy clean.
A coverage-parity slice then met the requirement that Rust coverage reach or
exceed the original Go envd. Under a like-for-like convention on both sides
(implementation lines, generated code excluded, `main.rs` included), Rust
measures 89.29–89.30% lines (±1 load-sensitive line in cpus/manager.rs),
88.03% regions and 85.81% functions against a fair 72.91% for the Go oracle
(bounds 70.24–73.90%: the root package cannot run offline because
kin-openapi v0.152.0 is absent from the module cache; raw 50.98%; two
bindfs-failing filesystem tests disclosed). Twenty-one new test functions
closed the Go-ahead modules RED-first and surfaced two real production
defects, now fixed: terminal process-state rendering is byte-exact against
Go's `ProcessState.String()` including the " (core dumped)" suffix
(exec_posix.go:149-150), and the signal-name table lives once in
`src/errno_text.rs` with `mmds/route.rs` delegating to it. The Go-ahead
cgroups aggregate was then closed by the follow-up slice below; the
cabundle comparison remains structurally asymmetric (`der.rs` ports parts
of Go's stdlib crypto/x509, which Go's cabundle denominator does not
count). Immutable candidate
`5fbd8bae80b6bc008459418c698c5a423c549f233b49efcd35c3202adc4a7174` passes
the same seven same-hash VM gate families (standard 14-group
`/tmp/envd-oracle-vm-zn9h4uxa`, CPU `/tmp/envd-cpus-vm-jmzfzbcg`, OOM
`/tmp/envd-oom-vm-2gawene4`, MMDS `/tmp/envd-mmds-vm-3t6j4l7g`, expanded
`/tmp/envd-oracle-vm-dwzk41zr` twice, freeze `/tmp/envd-freeze-vm-tf44t8xg`
three times, guest probes `/tmp/envd-guestprobe-vm-jfwf7jp2` twice); the
full suite is 650 top-level tests in 45 suites. An external rewrite of the
host `/etc` group database on 2026-11-01 dropped unnamed gid 950, so this
checkpoint's totals were "measured under a disclosed test-harness-only
getgroups LD_PRELOAD shim necessitated by the external /etc rewrite; changes
no assertions; suite passes identically without it once the group db is
restored or the session runs 950-free" (the oracle fails identically under
the same condition, so this is not a parity defect). The two known flakes
were then root-caused and fixed deterministically: the whole cacerts family
staged files through tokio's blocking pool without `flush()`, letting a
same-pool `rename` publish an empty or partial file (fixed with a single
`flush().await` at the staging seam; the amplified loop went 18/20 red
pre-fix to 0/25 green post-fix), and the process_runtime slow-observer test
measured machine latency (fork/exec under load) rather than the
deadlock-freedom property it guards (now a 30 s deadlock tripwire whose
`listed` step also asserts pid visibility; the former 200 ms windows failed
on machine speed alone at measured 331 ms spawn+reap under a fork storm).
A follow-up slice then closed every remaining flake follow-up RED-first.
`daemon.rs::write_upgrade_binary` now `flush()`es before the caller can
swap (same seam as `cacerts::prepare_file`; tokio reports `write_all`
complete once the write is queued on the blocking pool — a 200-round
`#[cfg(test)]` test was deterministically red at 64512/65536 bytes before
the fix and 0/30 after); the metadata_poll busy-wait became a bounded
2 s wall-clock wait on the file (under the paused test clock, 100
`yield_now`s had no real-time bound at all; 88/720 red to 0/720); the two
load-sensitive process_runtime tests kept every assertion and gained a
2 s trigger deadline, 30 s deadlock tripwires, and consume-before-
disconnect (their stress-era reds are disclosed honest negatives within
the current load policy, not green-from-fix claims); and the route_runner
ETXTBSY was root-caused to kernel fork fd-inheritance (a fork copies the
fd table and O_CLOEXEC closes only at the child's own execve, so a
sibling thread's in-flight write fd makes execve fail — fixed with a
process-wide SPAWN_GATE serializing every fork/exec in that test binary;
19/150 red to 0/200; production `route.rs` holds no program fd and is
not implicated, and the analogous window beside the upgrade seam is
annotated as parity-identical with Go's `io.Copy`). Coverage closed the
cgroups gap past the Go oracle: global 90.69% lines (17203/18969),
cgroups 96.70% against Go's 88.97%, cacerts 93.36% (like-for-like
`mod.rs` 92.46%; the residual against cabundle's 97.83% is the DER
parser, which is the stdlib x509 port outside Go's denominator), 27 new
tests, and every disclosed uncovered arm verified genuinely uncovered
(`days_in_month`'s `_ => 0` is proven unreachable from source). The
suite is 677 top-level tests in 45 suites. An independent adversarial
review returned APPROVE with four LOW findings, all adjudicated: the
+130-new-test-lines-vs-net-+129 denominator nuance and the cgroup2.rs:96
closure-arm phrasing are folded into these notes as stated, the
production-side fork/fd window note was added, and two legacy
`yield_now` loops in `accepted_init_refresh_survives_http_cancel…` are
ledgered unconverted (no observed flake, loud asserts, and the current
Docker load policy cannot reproduce the historical reds, so a conversion
would be unverifiable). Remaining follow-ups: the cacerts structural
residual and those two ledgered loops.

An NFS/volumeMounts slice then completed the last system-init effect. The
whole /init body now decodes the way Go `encoding/json` decodes a
`*[]VolumeMount` (case-folded names, null/absent/empty three-state, null
elements as zero values, any other JSON type failing the whole body as a bare
400), a non-empty list mounts for real, and the 501 stub is gone. `setup_nfs`
mirrors the oracle's ordering (raw-depth sort, unconditional stale detach,
symlink-resolved re-sort, sequential mounts, statx `STATX_MNT_ID`
verification, hidden-by-overmount recheck, ledger last) and `mount_nfs`
mirrors the retry matrix (20s per-volume budget, 10s mkdir and per-attempt
deadlines, at most two timed-out attempts with a forced unmount between,
500ms immediate retries to the 10s window, `after %d attempts` wrapping,
process-group SIGKILL with a 2s WaitDelay, 512-byte cap before trim); the
lifecycle ledger and failure memoization ride the upgrade handover. Failures
answer 400 with `X-Envd-Failed-Volume` and a plain-text body while the
deferred thaw still runs, and success-only headers stay off the error path.
The unit tests caught two real parity bugs before any review did: the
`filepath.Clean` port left a trailing separator after a `..` pop, and a
deadline-SIGKILLed mount was misclassified as an immediate failure, which
would have driven the 500ms retry path instead of the forced-unmount retry.
An adversarial review re-derived the oracle contract from the pinned source,
ran three mutations (argv corruption, memoized-failure short-circuit removal,
lifecycleID nil/empty flip) — each turned the predicted tests red and was
restored byte-identical — and approved with corrections: the slice adds a
direct `libc = "0.2.190"` dependency (nix exposes neither `umount2` nor the
statx mount-ID probe; already in the lock as a transitive dependency, so the
lock changes by one dependency-list line and no new packages), and the net
test accounting is +34 over the previous checkpoint. The review's LOW-1
(thaw-skip unpinned on the failing mount path) closed with a test: a frozen
daemon asked to `thaw:"skip"` that fails a mount still advertises
`x-envd-thaw: skip` on the 400 and stays frozen. The known corner: when the
2s WaitDelay fires after the deadline kill, partial child output is dropped
where Go's CombinedOutput would keep it; loss is limited to that hang corner
and mount output is small and capped. Immutable candidate
`f6a3865c61a8b04225d5b6bbb75fe2962f4741a4b7ed723455f2fdf43c57c1a7` passed the
eleven-gate differential battery (14-group regular, real CPU, real OOM,
MMDS, expanded 22-group twice, freeze lifecycle thrice, guest probes twice)
with 712 tests green over 47 suites. Serving processes and guest
images have not been switched.

The NFS P1 differential gate (2026-11-02) then bound the same candidate
`f6a3865c…c1a7` and the pinned Go oracle `2fe9ca62…91a55` (test NFS server
`e26c44f4…7f4a`) to the volumeMounts gate in identical disposable QEMU
guests: the six default cases (`nfs-mount-success`, `nfs-lifecycle-ledger`,
`nfs-overmount-hidden`, `nfs-empty-and-decode`, `nfs-bad-address`,
`nfs-stale-timestamp`) ran twice (`/tmp/envd-nfs-vm-013vxoip`,
`/tmp/envd-nfs-vm-s9jffuj9`) and the helper-missing variant twice
(`/tmp/envd-nfs-vm-e7xpzq16`, `/tmp/envd-nfs-vm-fy8227tf`), both sides with
`mismatches=[]` and no failures. The dead-target case is answered by an RPC
PROG_UNAVAIL refuser — slirp guestfwd accepts the guest SYN, so a
connection-level refusal never reaches the guest and only the RPC reply can.
Findings kept as gate constraints: the musl guest needs `/etc/mtab` →
`/proc/mounts` or `mount.nfs` hangs inside `sigismember`; the oracle's
daemon-reported mount `duration` is a guest clock-jump artifact, not a real
mount time; and the gate must run with `--repeat 1` because guest mounts
outlive the per-run host NFS server. Harness coverage under the gate:
`fixtures_nfs.py` 69%, `nfs_vm.py` 44% (integration paths excluded; the
gate's authority is the real-guest differential evidence). This validates
NFSv3 mount semantics in the parity guest only — the sandbox Firecracker
image still has no NFS client or helper, the production transport decision
(P2/P3) remains unauthorized, and serving processes and guest images have
not been switched.

Guest mode enables the OOM watcher unless `--isnotfc` is supplied; development mode
keeps it disabled. Known history is returned oldest first, up to 32 kills, while
unknown history is omitted. A dedicated dual-VM run validated one real kernel OOM,
rejection of a userspace-forged kmsg record, and repeated nonconsuming snapshots at
`/tmp/envd-oom-vm-az03i6p_/result-1.json`. The corrected harness enforces a host
absolute stream deadline; independent source review closed the original timeout
finding. The 2026-11-02 campaign then extended the gate to 33 real kills per
side (32-entry cap, oldest-first eviction, non-consuming reads, unrelated-kill
isolation) and linked it to the live-upgrade gate, which proves the restarted
watcher rebuilds the same snapshot from `/dev/kmsg`; see the closure section.
Malformed proc data has explicit defensive differences, recorded
in the internal generation-gate record. GET/POST `/files` now stream downloads/uploads with gzip,
Range/conditional responses, multipart and metadata replacement. POST
`/files/compose` assembles sources through a temporary destination before replacement.
The exact `connect-python` User-Agent enables typed legacy filesystem response
conversion for JSON and protobuf, including WatchDir event pruning and notification
headers on streaming handler errors. Global Basic user
validation precedes HTTP/RPC handlers, and password bytes do not change the selected
account. Real PTY and inotify watchers also have targeted tests. Fourteen observation
groups passed for immutable candidate `fd9bb60fe97ed6c0a870386ef49248b9d155e741bf77ec85305e583561f6ed28`
at `/tmp/envd-oracle-vm-11z7bdc0/result-1.json`, including CPU init decoding, Compose,
legacy, StreamInput, gzip response framing and real guest metrics. CPU interval
assertions use causal bounds that reject constant 0/100 results. Both sides explicitly
disabled Firecracker features for that run. Separate real CPU and OOM acceptance used
the same binary, with OOM evidence at `/tmp/envd-oom-vm-qo8kvboi/result-1.json`.
At that checkpoint, 255 Rust tests passed and implementation line coverage was 88.71%
(main included, generated code excluded). Subsequent MMDS work requires new acceptance.
This remains a subset of the full differential and lifecycle gates; arbitrary malformed
protocol inputs and system behavior remain unfinished. At that checkpoint,
requests for unimplemented system init effects returned 501. Existing serving
processes and
images have not been switched. The 2026-11-02 parity campaign closed the
malformed-input, real-kernel OOM/history-rebuild, live-upgrade, and forwarder
gaps and re-ran every gate on the final candidate; the 2026-11-03 campaign
closed the HTTP/1 protocol-edge, nested cgroup/`memory.oom.group` and
non-loopback binding gaps. See the “Parity campaign closure” sections at the
end, which state exactly what is and is not claimed.

Run from the workspace:

```sh
cargo test -j4 --locked -p coorworker-envd
cargo clippy -j4 --locked -p coorworker-envd --all-targets -- -D warnings
cargo fmt -p coorworker-envd --check
```

The `client` feature forwards `connectrpc/client` for the generated clients.
Buffa envd messages remain separate from the existing Prost ToolHost messages.
Any distributed binary/image using these schemas must include
`third_party/e2b/LICENSE` and `PROVENANCE`; generated output does not replace
those notices. The Go-derived redirect URL helper in `src/platform/linux/mmds/go_url.rs` retains
The Go Authors' BSD-3-Clause license; binary redistribution must reproduce that
notice too. The HTTP/TLS path also links dependencies with ISC and Unicode-3.0
obligations, including ring, WebPKI and ICU. The distributable notice bundle is
now generated from the locked graph and checked in: `third_party/NOTICES.md`
plus 388 verbatim license files covering 216 crates
(`python3 scripts/generate_notices.py --check`), including the
Apache-2.0 in-tree fork `vendor/connectrpc` selected by `[patch.crates-io]`
(its `LICENSE` and `PATCHES.md` provenance are covered by the bundle and by
`third_party/PACKAGING.md`). `scripts/packaging_manifest.py` emits the
per-artifact provenance manifest (source commit, dirty state, toolchain,
canonical `--locked` build command, binary and covered-file SHA-256, license
tree digest) and refuses a dirty tree unless explicitly allowed; the guest
artifact is the static `x86_64-unknown-linux-musl` release build. The 2026-11-02
manifest run against that build recorded schema 1, `notice_check: ok`, and a
2098-byte document (the trial run honestly records `dirty: true` because the
parity sync is not committed yet; a distributable release must be built from a
clean tree).

## Parity campaign closure (2026-11-02)

Final candidate: `b21f4835fde1640283a847c5de2ef8ea4dd56bf021cd051572dc65e2d9df2fd6`
(`/tmp/rust-envd-candidate-d61303f9/coorworker-envd`). Its `src/` and `tests/`
are byte-identical to the copies in this repository (`cmp`-verified), the
in-tree `vendor/connectrpc` fork is mirrored with its `PATCHES.md`, and this
repository builds the same static musl artifact. Both repositories' suites are
green with 0 failures (`--tests --list`: 778 tests, plus 24 ignored
doc-tests; the dedicated handover tests re-run their binary in a child, whose
output is not counted again), with clippy `-D warnings` and
`cargo fmt --check` clean; the musl artifact built here also matches the
oracle on the schema gate (`/tmp/schema-rustenvd-v7.json`, IDENTICAL to the
oracle document) and carries the same candidate hash binding in its gate
manifests.

Gaps closed in this campaign, each bound to the candidate:

- **Malformed input, routes and CORS.** A deterministic corpus (13 HTTP
  operations × malformed shapes, 17 Connect procedures × framing faults,
  preflight/HEAD/random) is driven against both sides and the normalized
  documents diffed (`/tmp/envd-malformed-vm-frrkuexw`,
  `/tmp/envd-malformed-vm-a87tvg50`; IDENTICAL in
  `/tmp/upgrade-v7-diff.txt`). Connect/JSON decode, envelope
  framing, compression refusal and unary cardinality texts are byte-identical
  after the in-tree connectrpc fork; regression tests live in
  `tests/malformed_rpc.rs`.
- **HTTP/1 rejection bytes.** `src/daemon/http1/` replicates Go
  `net/http`'s request-line/URL/MIME/Host/Content-Length/Transfer-Encoding
  validation order and serves its exact 400/431/501/505 bodies and headers
  between the socket and hyper (39 unit tests). Re-running the gate exposed
  and fixed three real differences: EOF residual lines, the post-response
  four-byte `Peek` idle gate, and CONNECT withholding the remaining bytes.
  The historical 21-probe divergence is now zero.
- **Protojson schema-error texts.** 73 cases (17 RPCs × top-level shape,
  trailing values, unknown fields including invalid UTF-8/lone surrogates/bad
  escapes/10000- vs 9999-deep nesting, extension keys, duplicate fields/map
  keys, oneof double-set, integer/enum forms, base64 padding, case aliases)
  produce byte-identical status, content type and body
  (`/tmp/envd-schema-vm-gs8s3ggz`, `/tmp/envd-schema-vm-ff3qaobk`).
  `vendor/connectrpc/src/protojson.rs` reproduces Go's `DiscardUnknown` walk
  and well-known-type decoding; the schema is registered from the compile-time
  descriptor set in `src/transport/protojson_schema.rs`, and
  `tests/protojson_schema.rs` replays the oracle fixture on the host router.
- **Live-upgrade handover, chained.** Five guests per side: both `reexec` and
  `install` modes with plain/PTY/tagged processes, a retained exit, watcher,
  socat listeners and a failure guest; OOM history rebuilt after the swap;
  eight consecutive handovers per side over three concurrent forwarder ports
  plus a ninth after a forwarder restart; and a binding-filter guest. The
  re-adopted process stdio defect is fixed: the entry keeps the
  relocated endpoints and the pump registers the same descriptor
  (`pipe::SharedFd`, one fd per stream like Go's shared `*os.File`), pinned by
  `tests/process_runtime.rs::handover_chained_export_carries_readopted_stdio`;
  a failed registration fails that adoption, and the inherited child is killed
  and reaped rather than left running with no entry or reaper
  (`tests/process_runtime.rs::failed_adoption_reaps_the_inherited_child`). The
  two sides' documents are identical (`/tmp/upgrade-*-v8.json`; the v7
  documents remain as earlier evidence; artifacts
  `/tmp/envd-upgrade-vm-{eccdmcqm,9x_i9_kp,tvizmb5a,9knbtz6c}`).
- **Multi-port and socat crash/restart.** Three concurrent loopback
  forwarders are probed before and after every handover with all three
  observed; SIGKILLing the daemon-spawned socat leaves its port dark (the
  forwarder is keyed by listener `pid-port` and does not respawn), and
  restarting the listener yields a new key and a working relay while the
  other ports stay up; a further handover keeps the restarted forward alive.
- **cgroup-scoped OOM and memory pressure.** `--scenario cgroup` runs a real
  `memory.max=64M` cgroup (helpers create it; the daemon never writes
  cgroupfs) for three memcg kills, then a bounded `memory.high=16M` stall
  under `memory.max=256M`. Both sides record four raw kernel records (one
  userspace spoof plus three `Memory cgroup out of memory: Killed process …`),
  retain three, ignore the spoof, and report
  `{throttled: true, oom: 0, oom_kill: 0}` without a fabricated kill
  (`/tmp/envd-oom-vm-9v_mjzui`).
- **Real-kernel OOM boundary and rebuild.** 33 real OOM kills per side leave
  exactly the latest 32 oldest-first, non-consuming, unaffected by an
  unrelated SIGKILL (`/tmp/envd-oom-vm-a0zanolr`: 34 raw records, 32
  retained); the upgrade gate's third guest proves the restarted watcher
  rebuilds the same snapshot from `/dev/kmsg`.
- **Loopback forwarder.** The guest ships real socat; the upgrade gate now
  *fails* unless the re-adopted forwarding mapping relays a probe end to end.
- **Distribution.** `third_party/NOTICES.md` covers 216 third-party crates
  and 388 verbatim license files, including the Apache-2.0 connectrpc fork;
  `scripts/packaging_manifest.py` binds an artifact to commit, dirty state,
  toolchain, build command and SHA-256 digests; `PACKAGING.md` states the
  musl release build and completeness checklist.

Every previously existing gate was re-run on this candidate and passed
(`result-1.json`: `passed=true`, `failure=null`, candidate manifest binding
the same sha): expanded conformance `/tmp/envd-oracle-vm-rjs28xl_`
(`mismatches=[]`), CPU `/tmp/envd-cpus-vm-wmlovb0q`, MMDS
`/tmp/envd-mmds-vm-4cgc_1n0`, freeze `/tmp/envd-freeze-vm-fzff69p9`, guest
probes `/tmp/envd-guestprobe-vm-zmobz4z4`, NFS default
`/tmp/envd-nfs-vm-ni_khzkp` and helper-missing
`/tmp/envd-nfs-vm-_hyjuh7p`, OOM global and cgroup above, malformed and
schema above, and upgrade
`/tmp/envd-upgrade-vm-{2z8f9rgx,umz6k9le,9dz5n5kv,omppai2y}`. The MMDS gate's
first run failed on the oracle side only (`MMDS protocol request violated
fixture contract`, artifact `/tmp/envd-mmds-vm-qsz0fzap`) before any candidate
observation; an immediate rerun on the same candidate passed with both sides
(`/tmp/envd-mmds-vm-4cgc_1n0`), and the assertion now reports the full status
so a recurrence is diagnosable.

Explicitly **not** claimed:

- inputs outside the captured corpora, including arbitrary malformed protocol
  traffic and unenumerated system behavior;
- the remaining HTTP/1 parser edges: the `PRI * HTTP/2.0` preface answers one
  generic 400 where Go handles it through its mux first; a request with
  `Connection: close`, HTTP/1.0 without keep-alive, or an h2c upgrade makes
  Go stop reading after the response while the guard still parses the tail;
  non-100-continue `Expect` values and URI forms Go accepts but hyper rejects
  are outside the corpus;
- chains longer than eight consecutive swaps (they repeat the same
  export/relocate path);
- cgroup OOM inside a nested/limited hierarchy other than the helper's own
  cgroup, and stalls long enough to trigger `memory.oom.group`;
- non-loopback forwarding bindings (the scanner follows Go and recognises only
  127.0.0.1 LISTEN);
- the production Firecracker transport path (P2/P3) and the host facade (S4);
- a clean-tree distribution: this working tree is uncommitted, so the trial
  manifest records `dirty: true`. Commits and pushes await explicit
  authorization.

## Parity campaign closure (2026-11-03)

Final candidate: `73e66bb4a94fd8b4de725db886aab9b375e7660bfaf763d8ec6562d401e93db0`
(`/tmp/rust-envd-candidate-rVZ0mk/coorworker-envd`, `-version 0.10.2`,
`-commit 51083a83…`). Its `src/` and `tests/` are byte-identical to the copies
in this repository (`cmp`/`diff -r` verified, including the
`vendor/connectrpc` fork and its `PATCHES.md`). This repository's suite is
green (799 passed, 0 failed), clippy `-D warnings` and `cargo fmt --check`
clean, and the static musl artifact built here
(`target/x86_64-unknown-linux-musl/release/coorworker-envd`, sha256
`562dbf67abe498912f157deafd068b7992992c8f37758fcbe65620f87e44e5c4`) matches
the oracle document on the protojson schema gate
(`/tmp/schema-rustenvd-v8.json`, IDENTICAL; artifact
`/tmp/envd-schema-vm-11j0nr0j`).

Gaps closed in this campaign, each bound to the same candidate:

- **HTTP/1 protocol edges.** A 34-case raw-socket corpus (`PRI * HTTP/2.0`
  prefaces, `Expect`, 100-continue, `Connection: close`, HTTP/1.0, h2c upgrade,
  pipelining, absolute/asterisk forms, control bytes, invalid UTF-8, HTTP/1.2,
  missing `Host`, 505, unauthenticated `OPTIONS`, and a complete head over Go's
  1,052,672-byte budget) is byte-identical after three fixes: the head guard
  now applies `MAX_HEAD_BYTES=(1<<20)+4096` and returns Go's exact 431; HEAD
  responses drop the injected `content-length: 0` (Go never sends it); and
  method resolution precedes routing like chi v5.5.0 (bare 405 without
  `Allow` for an unknown method, one `Allow` per allowed method otherwise).
  Documents: `/tmp/envd-http1-edges-vm-{hmerf6pf,q5ghomat}`, IDENTICAL for all
  34 cases (`/tmp/envd-gates-73e66bb4/http1-edges-*.json`).
- **Nested cgroup OOM with `memory.oom.group`.** `--scenario group` builds a
  memory parent with a leaf (`envd-group/leaf`), sets `memory.oom.group=1`,
  and provokes two real group kills. The fixture follows cgroup-v2.rst: an
  `oom_score_adj=-1000` task is never group-killed, so the bystander stays
  killable at 0 and redirects its stdio to `/dev/null` (a surviving sibling
  holding the daemon's pipes would keep the Start stream open past the kill).
  Both sides mirror the kernel's three records per event
  (`[victim, bystander, victim]`) in `/metrics`, reject a userspace spoof, and
  retain two events as six records non-consumingly
  (`/tmp/envd-oom-vm-6wndvmr0`, `history {'spawned': 2, 'retained': 6}`).
- **Non-loopback forwarding bindings.** A fifth upgrade guest starts listeners
  on `127.0.0.1`, `127.0.0.2`, `0.0.0.0`, `10.0.2.15` and `::1`, proves each
  alive, then reads the daemon's per-port socat count and an end-to-end relay.
  Only `127.0.0.1` and `::1` are forwarded; `127.0.0.2` is excluded despite
  being loopback by subnet. The `0.0.0.0` relay verdict is recorded as
  `relay_confounded`, not asserted: `169.254.0.21` is a guest-local address,
  so the wildcard listener answers that probe itself even with zero
  daemon-spawned socats. A post-handover read proves the carried set keeps
  only the control.
- **Chained handover depth.** The chained guest now performs eight consecutive
  swaps, the requested mode alternating with the other, plus a ninth after the
  forwarder-restart sequence. Every handover re-proves the dropped connection,
  handover header, three forwarder probes, post-handover watcher delivery and
  unchanged live set, so the export/relocate and forwarder-refresh paths are
  exercised past the former four-swap boundary. All four upgrade documents
  (oracle/candidate × reexec/install) record the eight `chained.upgrades` and
  the post-restart handover, and are identical per mode after removing
  `side`/identity/`retained` (`/tmp/upgrade-*-v8.json`).

Every previously existing gate was re-run on this candidate and passed
(`result-1.json`: `passed=true`, `failure=null`, candidate manifest binding
the same sha): conformance `--expanded --repeat 2`
`/tmp/envd-oracle-vm-2e1ow6ll` (2/2, `mismatches=[]`), schema
`/tmp/envd-schema-vm-{wrt7uc7i,t_0hzqb5}`, malformed
`/tmp/envd-malformed-vm-{bg9egozu,vy1lh4fz}`, HTTP/1 edges above, OOM global
`/tmp/envd-oom-vm-ai4btxrw`, cgroup `/tmp/envd-oom-vm-5bu4erk5` and group
above, CPU `/tmp/envd-cpus-vm-83lmathy`, MMDS `/tmp/envd-mmds-vm-0cow6133`,
freeze `/tmp/envd-freeze-vm-mgyt1diu`, guest probes
`/tmp/envd-guestprobe-vm-thcwahpa`, NFS default `/tmp/envd-nfs-vm-fr2o9cke`
and helper-missing `/tmp/envd-nfs-vm-p4c_naue`, and upgrade
`/tmp/envd-upgrade-vm-{eccdmcqm,9x_i9_kp,tvizmb5a,9knbtz6c}`. The NFS fixture
now normalizes the wall-clock-derived `after N attempts` count to
`after <ATTEMPTS:single|repeated>`: an oracle-only repeat reported 19 attempts
for a case another oracle run reported 20, so the number itself is not a
parity signal while retry-vs-no-retry stays compared.

Explicitly **not** claimed:

- inputs outside the captured corpora, including arbitrary malformed protocol
  traffic and unenumerated system behavior;
- TLS and a completed h2c switch;
- chains longer than eight consecutive swaps (they repeat the same
  export/relocate path);
- the production Firecracker transport path (P2/P3) and the host facade (S4);
- a clean-tree distribution until the mirror commit is made; the provenance
  manifest must be regenerated from the clean tree before release.

The 2026-11-02 non-claims for HTTP/1 parser edges, nested cgroup
`memory.oom.group`, and non-loopback bindings are superseded by this section.
The 2026-11-03 non-claim for HTTP/1 response trailers is superseded by the
section below (gRPC terminal trailers over HTTP/1.1 are now compared).

## Parity campaign closure (2026-11-03, HTTP/1 drain and gRPC error frames)

Final candidate: `1215b0b35ede5992d7481f2c64a12a9ea0f990966f9fd85a13f32cd062d5a739`
(`/tmp/rust-envd-candidate-1215b0b3/coorworker-envd`, `-version 0.10.2`,
`-commit 51083a83…`). Its `src/` and `tests/` are byte-identical to the copies
in this repository, and both vendored forks (`vendor/connectrpc`,
`vendor/hyper`) match theirs (`diff -r` verified).

Gaps closed on this candidate, each bound to oracle documents:

- **Go's post-handler drain.** When a handler leaves the request body unread,
  Go consumes at most 256 KiB + 1 bytes; a corrupt chunked body or an
  oversized one sets `closeAfterReply`, an unconsumed `Expect: 100-continue`
  body closes without a drain, and CONNECT never reuses the connection
  (`server.go:1369-1375,1393-1435`). `daemon/http1/mod.rs` wraps the body in a
  `Drainable` that recovers it from the handler future and drains it before the
  response head is written; corrupt/oversized bodies and HTTP/1.1 non-2xx
  CONNECT responses carry `Connection: close`, while HTTP/1.0 deletes that
  header instead (`server.go:1529-1540`).
- **Chunked limits.** The vendored hyper decoder now follows
  `internal/chunked.go`: a chunk-size line of 4094 bytes is accepted and 4095
  is rejected, and the running non-data overhead is capped at 16 KiB after
  crediting 16 + 2n per chunk. Without this the drain above disagreed with Go
  on bodies the oracle refuses.
- **gRPC and gRPC-Web error frames.** The vendored connectrpc fork matches
  connect-go 1.18.1: `grpc-accept-encoding` on every response including
  errors, `grpc-status`/`grpc-message` only as trailers, padded
  `grpc-status-details-bin` only when details exist, and the gRPC-Web
  trailers-only error as HTTP headers with an empty body
  (`protocol_grpc.go:171,533-543,558-572,859-866`). The handler-visible
  `Transfer-Encoding` stays deleted (`transfer.go:636`) while the canonical
  chunked form is re-emitted to hyper so it still frames the body.
- **Smaller net/http details.** `OPTIONS *?x` no longer reaches the general
  options handler (`server.go:3327`), the listener uses Go's `SO_REUSEADDR`
  and somaxconn backlog, and the daemon installs no signal handler, so
  SIGINT/SIGTERM terminate immediately as Go envd does.

Evidence on that candidate (VM artifacts under `/tmp/envd-gates-1215b0b3/`):
the HTTP/1 protocol-edge corpus grew from 34 to 71 cases (Expect combinations,
HTTP/1.0 keep-alive/pipelining, TE and chunked boundaries, header-block
shapes, gRPC and gRPC-Web errors, CONNECT over HTTP/1.0 and 1.1) and is
IDENTICAL (`/tmp/envd-http1-edges-vm-{ikizpbrz,jrls9izp}`, 71/71). Schema
(`/tmp/envd-schema-vm-{zr35ghpe,gzgtrh4m}`), malformed
(`/tmp/envd-malformed-vm-{5anlhm5y,glrx3hll}`) and both live-upgrade modes
(install `/tmp/envd-upgrade-vm-{qymfncd4,ae41z5e6}`, reexec
`/tmp/envd-upgrade-vm-{d5fgoqp8,54krm1ql}`) are IDENTICAL. Conformance
`--expanded --repeat 2` (`/tmp/envd-oracle-vm-yj6jz611`), OOM global
`/tmp/envd-oom-vm-c7zhihnh`, cgroup `/tmp/envd-oom-vm-cd9tsm82`, group
`/tmp/envd-oom-vm-nqe6ybma`, CPU `/tmp/envd-cpus-vm-qlxq42t5`, MMDS
`/tmp/envd-mmds-vm-flu3owp_`, freeze `/tmp/envd-freeze-vm-js1otrer`, guest
probes `/tmp/envd-guestprobe-vm-4p8ggexb`, NFS default
`/tmp/envd-nfs-vm-wz8xds61` and helper-missing `/tmp/envd-nfs-vm-kqpueczr`
all report `passed=true`. The first reexec upgrade attempt failed on each side
in the same post-OOM phase (listener-forwarder timing); fresh guests passed on
both sides and the documents are identical, so the failure is recorded as a
timing flake, not a parity difference.

The malformed gate's `/metrics` normalizer now collapses a whole decimal
literal (`0`, `0.0`, `0.01`, `1e-05`) to `N`; an idle guest can report an
integral percentage on one run and a fractional one on the next, and the old
digit-only rule left a spurious `N` vs `N.N` difference.

Tests on this tree: `cargo test -j4 --offline` green (814 passed, 0 failed;
24 generated-code doctests ignored), clippy `-D warnings` and `cargo fmt
--check` clean, harness `254 passed / 295 subtests`. A full workspace run is not applicable to this standalone
crate; in the upstream agent workspace the same sources pass 1184 tests with
two known failures that depend on a stale local guest image (2026-10-14)
lacking the current `counter`/`extension` tool declarations, unrelated to
envd, hyper or connectrpc.

## Windows port P0 scaffolding (2026-11-03)

P0 of the Windows sandbox port is a compile-time seam, not a feature claim.
The Linux backends (`process`, `cgroups`, `filesystem`, `port`, `cpus`,
`mmds`) moved verbatim to `src/platform/linux/` and remain the only
implementations the daemon calls; `src/lib.rs` re-exports them under
`#[cfg(target_os = "linux")]`, and the binary entry point moved to
`src/main_linux.rs` behind a target dispatcher. Seven backend traits
(`ProcessBackend`, `ResourceBackend`, `FsBackend`, `WatcherBackend`,
`PortScanBackend`, `ClockBackend`, `MetadataBackend`) live in
`src/platform/mod.rs`; the `src/platform/windows/` skeleton implements each
with `io::ErrorKind::Unsupported`, except the read-only clock.

Verification on this candidate: Linux `cargo test -j4 --offline` green,
clippy `-D warnings` and `cargo fmt --check` clean, notice bundle unchanged
and `--check` green; `cargo check --target x86_64-pc-windows-msvc --lib
--bins` and `cargo build --target x86_64-pc-windows-msvc --lib` succeed.
Linking the Windows binary is deferred to a Windows toolchain host. The
Linux parity gate rerun on the same candidate (sha `c036e195…`) is
IDENTICAL for the schema, malformed, HTTP/1 edge and both upgrade documents,
and the conformance, freeze, guestprobe, MMDS, CPUs, OOM and NFS gates all
pass; the artifact directories and status logs are recorded in the
workspace campaign plan (`/tmp/envd-gates-c036e195/`). That evidence covers
the listed corpus and scenarios only; the Windows side makes no
differential claim.

## Windows port P1 — filesystem backend (2026-11-03)

`WindowsFsBackend` now implements `stat`, `read_dir`, `create_dir`, `remove`
and `rename`; the value synthesis follows `docs/windows-port.md` §5.2
(synthesized `mode`/`permissions`, SID-derived `owner`/`group` with
non-empty fallbacks, resolved reparse-point target, empty `metadata` map).
The backend is not yet reachable from the daemon, which stays Linux-gated.

Verification: Windows-target `cargo check --lib --bins` and clippy
`-D warnings` clean on a Linux cross toolchain; the module's unit tests run
on the new `windows-test` CI job and pass on `windows-latest` (run
`41585352844`: 6 passed, including all five filesystem tests), so the
backend has real Windows runtime evidence, not just cross-compilation. Linux is unchanged behaviourally: the full suite is 814
passed / 0 failed, clippy and fmt clean, and the full parity gate set on the
same candidate (sha `ca97781c…`) is IDENTICAL for the schema, malformed,
HTTP/1 edge and both upgrade documents, with conformance, freeze, guestprobe,
MMDS, CPUs, OOM and NFS all passing (`/tmp/envd-gates-ca97781/`). The
workflow addition exposed a
pre-existing timing assumption in `tests/route_runner.rs`: it waited five
seconds for a probe child while another test could hold the shared spawn gate,
and it located the child by scanning every `/proc` entry for an argv[0] match.
The test now takes the gate before starting its deadline, has the probe report
its PID through a file, judges liveness by `/proc/<pid>/cmdline` (empty for a
zombie), execs `/usr/bin/sleep` by absolute path because sibling tests mutate
`PATH`, and surfaces an early runner error with its output. It is test-only and
does not change the daemon; the guest binary is byte-identical to the gated
candidate. Two further CI-only findings were fixed the same way: the
hyperloop tests now poll with 1 ms sleeps instead of a fixed number of
`yield_now` calls (a `spawn_blocking` rewrite could outlast the budget on a
loaded runner), and the Linux job runs `cargo fetch --locked` before the
notice check because `cargo metadata --offline` needs the non-Linux sources
that the bundle covers.

## Windows port P1 — shared HTTP surface and contract tests (2026-11-03)

The platform-neutral HTTP stack left the Linux daemon: `src/http/` owns the
`net/http` connection-level checks (`Expect`, `OPTIONS *`), rs/cors, the
chi/connect-go frontdoor, authorization, `/health` and `/envs`;
`src/files_http.rs` owns `GET /files`. `section 5.1`'s rule that shared code
never calls a platform API now holds: account lookup goes through
`platform::lookup_account` (password database on Linux, `LookupAccountNameW`
on Windows), and the one Go errno-text renderer is cfg-split (Windows keeps
the `stat <path>:` prefix and std's own message). `src/daemon_windows.rs`
serves `/health`, `/envs`, `/init` and `GET /files` over that shared code;
the rest of the REST surface answers 501 and the Connect procedures answer
connect-go's 404 `unimplemented`, each naming the documented gap. The
Windows deviations are listed in `docs/windows-port.md`.

Verification, Linux: 814 tests passed / 0 failed across all targets, clippy
`-D warnings` and fmt clean, and the full parity gate set was rerun on the
new candidate before this note was written: sha
`5eb94465…`, five IDENTICAL documents (schema, malformed, HTTP/1 edges,
upgrade reexec and install) and every single-side gate `passed=true`
(`/tmp/envd-gates-5eb94465/`). Verification, Windows: the `windows-check`
cross-compile job, and clippy on the Windows target (`--lib`, `-D warnings`)
plus `cargo test --no-run --lib` (which type-checks the Windows test bodies;
only the MSVC linker is absent locally) are clean. The contract tests
themselves ran on `windows-latest` in the `windows-test` job
(`cargo test --lib`) in run `41590760185`: 29 passed / 0 failed, including
all 11 contract tests, against the real Win32 account database and
filesystem. They assert the wire
behaviour an SDK sees: 204 `/health` with an empty content-type, the 401 JSON
envelope, `/init` token install plus `X-Envd-Version`/`X-Envd-Memory`, a real
file read and the missing-path error shape, chi 404/405 with `Allow`, CORS
preflight, and raw-socket `OPTIONS *`/failing-`Expect` responses.

## Windows port P1 — `POST /files` on the shared upload path (2026-11-03)

The upload flow moved out of the Linux daemon into `src/files_upload.rs` and is
generic over `ConfigurationState`, so both daemons run the same decoder,
multipart scanner, same-path alias check and response shape. The platform-owned
parts are behind `src/platform/file_writes.rs`: the `user.e2b.*` validation
policy (moved verbatim, now shared) and one adapter per platform. Linux delegates
to the existing `filesystem::{paths, metadata}` — chown/fchown, `0666` creation
mode, `ENOSPC`/`EDQUOT` mapping, xattr persistence — so its behaviour is
unchanged. Windows creates parents one component at a time, creates/truncates
with the service account's ACL instead of chowning, identifies files for the
multipart same-path refusal with `GetFileInformationByHandle` (volume serial +
file index), maps the Windows disk-full/quota errors to the same 507 responses,
and answers `Unsupported` for `user.e2b.*` writes; the shared flow logs that and
continues, the same degradation a Linux filesystem without xattr support gets.
`src/daemon_windows.rs` now routes `POST /files` to the shared handler.

Verification, Linux: 814 tests passed / 0 failed, clippy `-D warnings` (Linux
all targets, Windows `--lib`) and fmt clean. The full parity gate set was rerun
on the new candidate sha
`bccb04e9e6b38d2dbb3d3e7eddbb7b05fd05738e4cfcb673d37e070ae8eec3e7`: five
IDENTICAL documents (schema, malformed, HTTP/1 edges, upgrade reexec and
install) and every single-side gate `passed=true`
(`/tmp/envd-gates-bccb04e9/`). Verification, Windows: the Windows-target
`check --lib --bins`, clippy `-D warnings` and `cargo test --no-run --lib`
(which type-checks the test bodies; only the MSVC linker is absent locally) are
clean. The new contract tests — raw upload into a missing directory read back
through `GET /files`, a two-part multipart upload, the directory-target /
invalid-metadata / unsupported-content-type 400 shapes, and the 401 without a
token — ran in the `windows-test` job on `windows-latest` in run
`41657417627`: 33 tests passed / 0 failed (the 29 existing plus the four upload
tests), and all three jobs are green.

That run followed `41656501320`, whose Windows jobs were green but whose Linux
job failed on a wall-clock guard: `tests/upload.rs`'s
`review_unfinished_part_headers_reject_before_sender_closes` pushes the full
10 MiB header budget through multer, which rescans the growing unterminated
line on every read, so the debug-build test alone costs several seconds
(3.7–5.0 s measured locally) and crossed its 10 s deadline on the runner while
still rejecting correctly. The deadline now exists only to fail a hang
(60 s), with the reason in the test; the guest binary is byte-identical
(`bccb04e9…`), so the gated candidate stands.

## Windows port P1 — Filesystem Connect slice (2026-11-04)

The nine Filesystem Connect procedures are served on Windows through the same
shared adapter as Linux. `src/platform/windows/fs_runtime.rs` implements the
`FilesystemService` runtime with the Linux resolution order and message shapes
over `std::fs`/Win32 calls: lstat-shaped `Stat` with synthesized
mode/permissions/owner/group and an empty metadata map; `MakeDir`'s
`already_exists`/`invalid_argument` ordering; `Move`'s destination-parents
side effect before the source check plus `MoveFileExW` with
`MOVEFILE_REPLACE_EXISTING`; the 255-link, sorted depth-first `ListDir` walk
with alias paths; and idempotent, symlink-safe recursive `Remove` that clears
the read-only attribute once on access-denied. A published Win32-to-errno text
table supplies the Go-shaped tails of the wrapped messages.
`src/platform/windows/watchers.rs` implements the watcher service over
overlapped `ReadDirectoryChangesW`: one directory handle and dedicated thread
per watcher, a 64 KiB buffer, a manual-reset stop event, `CancelIoEx` plus a
`GetOverlappedResult` drain and a join on drop, the documented action mapping,
`/`-separated relative names, `ERROR_NOTIFY_ENUM_DIR` as the terminal
`watcher event queue overflow`, and UNCs/`DRIVE_REMOTE` as network mounts.
`src/daemon_windows.rs` owns the watcher registry and `Logger::none()`, mounts
the shared router through `transport::rpc_service` in both `app()` and
`app_with_filesystem()`, and answers unknown process procedures with the
Connect router's 404 `method not found: <path>`.

Declared deviations: native `\` separators in returned paths and messages
while event names use `/`; synthesized mode/owner/group; `metadata` always
empty; no CHMOD events; RDCW batching and no synthetic child Create events
for a directory moved in; moving the watched root does not end the watch;
`Move` refuses an existing directory; `Remove` clears read-only attributes;
no SID cache; no interceptor log lines on Windows. The complete list is in
`docs/windows-port.md` §5.2. **Windows behaviour is contract-tested, not
differentially verified** — no Windows oracle exists.

Verification, Linux: the filesystem runtime/RPC, watcher RPC and production
log suites named in the step's verification list, clippy `-D warnings` on all
targets and fmt are clean on this candidate. Verification, Windows: the
Windows-target `check --lib --bins` and clippy `-D warnings` are clean. The
new contract tests (Stat value synthesis and message shapes, MakeDir, Move,
ListDir, Remove including a junction, token/Basic auth,
CreateWatcher/GetWatcherEvents/RemoveWatcher drain and authorization,
missing/file watcher refusals, WatchDir Start/Create/Write/Rename frames,
keepalive, and handle release) and the new unit tests (RDCW buffer mapping,
`/` names, overflow, network classification, error table, runtime
move/junction/walk/stat) are written for the `windows-test` job on
`windows-latest`; they were type-checked against the Windows target locally
but cannot run without a Windows host.

## Windows port P1 — `POST /files/compose` (2026-11-04)

The compose handler and the incremental Go encoding/json scanner moved out of
the Linux-only daemon into `src/files_compose.rs` and `src/compose_decode.rs`;
both daemons mount one handler, so the decoder, validation order, error
strings, HTML escaping and response framing are the moved Linux code. The
file operations moved behind `src/platform/compose_files.rs`. Linux keeps the
verbatim `/dev/urandom`, `0666` temp mode, chown of the temp and created
directories, `copy_file_range` with the 32 KiB buffered fallback, descriptor
close and lowercased errno text. Windows implements the same facade with real
Win32 calls: `BCryptGenRandom`, exclusive temp creation sharing
read/write/delete, a drive/UNC-aware lexical parent, `fs::create_dir` without
chown, a no-op `adopt_owner`, `GetFileInformationByHandle` identity (volume
serial + file index, optionally `FILE_FLAG_OPEN_REPARSE_POINT`), a 1 MiB
buffered copy with the oracle's operation strings, `FlushFileBuffers` +
`CloseHandle`, Windows system-message `errno_text`, and the shared disk-full
classifier. `src/daemon_windows.rs` routes `/files/compose` to the shared
handler; the 501 gap test now uses `POST /freeze`.

Declared deviations: no POSIX mode/uid/gid (the temp inherits the destination
directory's ACL, the service account is its creator, and ownership adoption is
a no-op); Windows system-message tails in OS error details instead of
lowercased errno text; no `copy_file_range` operation string; lexical `..`
collapse in Win32 path resolution; a read-only source cannot be unlinked by
the best-effort cleanup; renaming over a destination held open without delete
sharing returns a 500; `ERROR_DISK_QUOTA_EXCEEDED` stays 500 while disk-full
maps to 507; and long relative paths can still fail near `MAX_PATH`. The
Windows unit and contract tests (four unit, seven contract plus the updated
gap test) run only in the `windows-test` job on `windows-latest`; they were
type-checked against the Windows target locally but cannot run without a
Windows host.

Verification, Linux: `cargo fmt --check` clean; `cargo check -j4 --offline
--all-targets` clean; `cargo clippy -j4 --offline --all-targets -- -D
warnings` clean; `cargo test -j4 --offline` 814 passed / 0 failed. Verification,
Windows (cross-compiled on this Linux host): `cargo check -j4 --offline --lib
--bins --target x86_64-pc-windows-msvc` clean and `cargo clippy -j4 --offline
--lib --target x86_64-pc-windows-msvc -- -D warnings` clean;
`cargo test --no-run --lib --target x86_64-pc-windows-msvc` compiled every
Windows test body and stopped only at the absent MSVC `link.exe`. The Windows
tests themselves have not run here.

## Windows port P1 — resource slice: metrics, freeze/unfreeze, collapse, fsfreeze (2026-11-04)

The remaining five REST resource operations are now served by the Windows
daemon. `src/platform/windows/metrics.rs` implements `WindowsCollector` on the
shared `Metrics`/`CpuTimes`/`disk_usage` code: `GetSystemTimes` for the CPU
delta, `GetActiveProcessorCount` (with a `GetSystemInfo` fallback),
`GlobalMemoryStatusEx` for memory, `GetPerformanceInfo().SystemCache *
PageSize` for cache, and `GetDiskFreeSpaceExW` on the system volume. The four
CPU hotplug fields are 0 (no CPU manager) and `oom_kills` is omitted.
`src/platform/windows/collapse.rs` implements the shared `Collapser` with
`SetProcessWorkingSetSizeEx`, `GetProcessHeaps`/`HeapCompact` and a
`VirtualQuery` scan of committed private `PAGE_READWRITE` regions; it cannot
migrate base pages, so `collapsed == alreadyHuge == 0` and
`skipped == chunks`. `src/platform/windows/fsfreeze.rs` implements the shared
`FsFreezer` by resolving the system volume and calling `FlushFileBuffers` on
`\\.\C:`. `src/daemon_windows.rs` wires `GET /metrics`, `POST /collapse`,
`POST /fsfreeze` and `POST /fsthaw` with the exact Linux wire contract (headers,
newline framing, 204s, 500 envelopes, one shared fsfreeze semaphore with a
request-bound freeze acquire and a detached thaw), and `/init` now decodes
`thaw` and runs the generation-guarded thaw unless it is `"skip"`.

Review hardening (same slice): the freeze backend now has the Linux ten-minute
thaw watchdog — a sweep that suspended anything arms a generation-guarded
backstop task, only a clean thaw disarms it, and a partially failed thaw keeps
the unresolved thread records and the spawn gate so a retry resumes exactly
what is left. `/init` registers its thaw as a detached task at the defer point
(a dropped client still thaws it) and answers `X-Envd-Thaw: skip` when the
thaw is withheld. `generation()` is taken under the freeze lock, so a freeze
accepted just before `/init` is waited out instead of skipped. `preFrozen`
counts pre-suspended processes rather than threads, a spawn-drain refusal
reports the Linux `failed: 2` with zero walk counters, and a recycled tid is
rejected by `GetProcessIdOfThread` before any resume. The spawn gate stores
its drain permit instead of `mem::forget`ting it, fsfreeze handlers hold the
request body until the response, and a failed volume resolution still names
`%SystemDrive%` in the error text.

Declared deviations: fsfreeze is a flush-only operation (no FIFREEZE write
barrier and no VSS snapshot); fsthaw repeats the flush and is a no-op; collapse
cannot produce or attribute huge pages; CPU hotplug metrics stay 0;
`oom_kills` is absent; Win32 error text replaces Go errno text in 500
envelopes; `X-Envd-Freeze-Audit` is not emitted (no cgroup observation); the
clock-correction header was absent at this slice and is emitted now (see
*loopback forwarding, the `/init` clock* below); volume flush requires
system-volume write access (typically an elevated service account); and a
crash while workloads are suspended leaves them frozen until reboot because
the suspended-tid set is not persisted.

New contract tests (in `src/daemon_windows/contract_tests.rs`) cover the
twelve metrics fields/headers/newline and the absent `oom_kills`, the zero
hotplug fields, the GET-only method matrix with auth precedence, the empty-500
shape of a failed metrics sample, the six-field collapse shape and its token
requirement, bodiless no-store fsfreeze/fsthaw 204s and their 500 envelopes,
token enforcement, and `/init` thawing by default but not when `thaw: "skip"`
(including its `X-Envd-Thaw: skip` header). New unit tests cover the shared
CPU formula, region-window counting and cancellation, the volume-device
mapping, the guarded thaw and freeze-lock serialization, the thaw watchdog,
pre-frozen pid counting, and the tid-ownership check.

Verification, Linux: see the status lines below. Verification, Windows
(cross-compiled on this Linux host): `cargo check -j4 --offline --lib --bins
--target x86_64-pc-windows-msvc` clean and `cargo clippy -j4 --offline --lib
--target x86_64-pc-windows-msvc -- -D warnings` clean; `cargo test --no-run
--lib --target x86_64-pc-windows-msvc` compiled every Windows test body and
stopped only at the absent MSVC `link.exe`. The Windows tests themselves have
not run here.

Verification, Linux (this candidate): `cargo fmt --check` clean; `cargo check
-j4 --offline --all-targets` clean; `cargo check -j4 --offline --lib --bins
--target x86_64-pc-windows-msvc` clean; `cargo clippy -j4 --offline
--all-targets -- -D warnings` clean; `cargo clippy -j4 --offline --lib
--target x86_64-pc-windows-msvc -- -D warnings` clean; `cargo test -j4
--offline` 817 passed / 0 failed (the 814 pre-slice Linux count plus the three
`freeze_wire` tests already present in the uncommitted resource work).

## Windows port P1 — process Connect slice (2026-11-04)

The eight Process Connect procedures (`List`, `Start`, `Connect`,
`StreamInput`, `SendInput`, `CloseStdin`, `Update`, `SendSignal`) are served
on Windows by the same shared adapter as Linux, `src/rpc/process.rs`, over
`WindowsProcessRuntime`. The runtime keeps the Linux registry, connect and
30-second retention algorithms one-for-one: `CreateProcessW` creates the
child suspended, assigns it to the sandbox Job used by hierarchy freeze first
and then to a per-process `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` Job, registers
it before the
spawn-gate permit is released, queues the `StartEvent` before the readers
start, and the supervisor synthesizes the terminal `EndEvent`, moves the entry
to retained, and expires it. `src/daemon_windows.rs` owns `process_logger`
(`Logger::none()`), implements `ProcessRpcState`, and adds
`app_with_execution(state, runtime)` which merges
`crate::rpc::process::router` with the filesystem router under the one
`transport::rpc_service` fallback. `app()` builds and retains a single
`WindowsProcessRuntime` from the service account (`current_user_name` +
`USERPROFILE`), the `Configuration` defaults/environment and `PATH`, the
sandbox `JobHandle`, and the workload registry shared with
`WindowsResourceBackend::with_registry` (the sandbox `JobHandle` is attached
with `with_job`), and wraps `process_logger` in the shared
`logs::ProcessLogger` (`with_process_logs`), so the handler.go:599-605/:697-706
lifecycle lines appear when a logger is configured; the resource backend and
the runtime therefore share one registry, and a `/freeze` sweep sees started
processes.
When the runtime cannot be constructed (no Job, unresolvable service account,
locked configuration), `app()` logs the reason and keeps the Connect router's
404 `unimplemented` fallback instead of panicking. The existing gap test now
targets an unknown procedure (`/process.Process/NotAProcedure`) because all
declared procedures are served. `src/platform/windows/account.rs` already
exposed `pub(crate) is_current_user` (the SID comparison the start check
uses), so this slice consumes it without changes.

Declared deviations: no `/bin/sh` wrapper (the request `cmd` is the
executable, so shell syntax does not run); a missing executable is
`invalid_argument` (400) rather than a child exiting 127; no
`oom_score_adj`/`nice`/`ionice` wrapper; SIGTERM is best-effort
`CTRL_BREAK_EVENT` (console), ETX 0x03 (ConPTY) or `WM_CLOSE`
(GUI/no-console) followed by `TerminateProcess` after a 5 s grace; PTY bytes
are ConPTY-encoded and `Update` truncates to `i16` and clamps below 1;
signal deaths are synthesized as `exitCode -1`/`exited false` with
`signal: killed`/`signal: terminated`, and a `STATUS_CONTROL_C_EXIT` exit
maps to `signal: terminated`; a different-user start needs credentials that
are not plumbed and is refused `failed_precondition` (unknown accounts remain
the Linux 401); the sandbox Job is assigned first (the nesting root) and the
per-process kill-on-close Job second, and a failure of either fails the start
so hierarchy freeze cannot miss a live process; the per-process Job is
terminated as soon as the direct child is reaped, which releases
descendant-held stdio and lets `End` publish promptly (Linux leaves such a
descendant running); the child environment is the shared
`Environment::for_process` plus fixed Windows defaults, never the daemon's
block, matching Linux `env_clear` so service credentials do not leak; NUL,
empty and `=`-leading names are refused as `invalid_argument`; and there is no
live-upgrade adoption (`export_processes`/`readopt_processes` are absent). The
complete list is in `docs/windows-port.md` §4.2. **Windows behaviour is
contract-tested, not differentially verified** — no Windows oracle exists.

New contract tests (in `src/daemon_windows/contract_tests/process.rs`) drive a
real loopback listener over the production `app()`: Start→Data→End for a real
`cmd.exe` child with stdout/stderr/exit 7 and no live List entry; List config,
tag and Windows-pid echo; Connect attach without output replay and retained
exits by pid and tag; selector refusals for all five procedures plus the
unset-selector text; missing-cwd and unknown-Basic-user errors with List
staying empty; stdin/PTY channel rules and the absent-input text; StreamInput
ordering and Go wrappers; CloseStdin idempotence and the PTY rejection;
Update no-op/non-PTY/after-exit paths and a live ConPTY resize; SIGKILL and
SIGTERM end synthesis; invalid signal names; keepalive at the request-header
interval; PTY-only data events with a retained exit after ConPTY close; the
different-user `failed_precondition`; Job kill-on-close descendant reaping
with a prompt-`End` assertion; a >2 MiB PTY burst before close reaching `End`;
SIGTERM served while a blocked PTY writer holds the input path; the
`connect-timeout-ms` execution deadline (`signal: killed`, List empty); and a
ConPTY resize verified through `mode con`'s reported columns. Runtime-level
tests in `process/mod.rs` exercise the retained window and tag-reuse pruning
through a shortened test retention, avoiding a 30 s real-time wait. New unit
tests cover the closed-pipe `FailedPrecondition` classification (`pipe.rs`),
`CommandLineToArgvW` round-trip quoting, NUL rejection and environment-name
validation (`spawn.rs`, adding the `Win32_UI_Shell` feature; no new crate and
no lockfile change). These tests only run on a Windows host; the Linux suite
does not compile them.

Verification, Linux (this candidate): `cargo fmt --check` clean; `cargo check
-j4 --offline --all-targets` clean; `cargo check -j4 --offline --lib --bins
--target x86_64-pc-windows-msvc` clean; `cargo clippy -j4 --offline
--all-targets -- -D warnings` clean; `cargo clippy -j4 --offline --lib
--target x86_64-pc-windows-msvc -- -D warnings` clean; `cargo test -j4
--offline` 817 passed / 0 failed. Verification, Windows (cross-compiled on
this Linux host): `cargo test --no-run --lib --target
x86_64-pc-windows-msvc` compiled every Windows test body and stopped only at
the absent MSVC `link.exe`. The Windows tests themselves have not run here.

## Windows port P3 — entry point, SCM lifecycle and `POST /upgrade` (2026-11-04)

The Windows daemon now has a real entry point and serves every REST operation.
`src/platform/windows/entry.rs` (library-side, because the handover seams are
crate-internal; `src/main.rs` calls `entry::run()` on Windows) parses the
shared CLI, guards `E2B_REEXEC_HELPER`, runs the boot guard
(`src/platform/windows/boot.rs`: SCM service is a guest by construction,
console requires `COORWORKER_ENVD=1`, `--resume-handover` is a guest by
construction, development must be loopback and non-SYSTEM), and serves through
the shared `http1::serve_gated` with an accept gate. A process the SCM did not
start falls back from `StartServiceCtrlDispatcherW` error 1063 to console
mode. Service and console guests keep the oracle's `0.0.0.0:49983` default;
development keeps the loopback-ephemeral default. Missing standard handles are
pointed at NUL before any diagnostic (Rust's `println!` panics on a failed
write, and the successor inherits no std handles), and console-less processes
report through the event log (`src/platform/windows/log.rs`).

`src/platform/windows/service.rs` implements the SCM contract:
`StartServiceCtrlDispatcherW`, `RegisterServiceCtrlHandlerExW`, and the
START_PENDING → RUNNING → STOP_PENDING → STOPPED transitions. STOP and
SHUTDOWN flip the serve shutdown watch; INTERROGATE re-reports the current
state; the serve future runs on a multi-thread runtime built inside
`ServiceMain`, so a console fallback never shares or leaks one.

`POST /upgrade` is served by the shared front half; the Windows choreography
freezes the workload with the strict `freeze_workload_hold(2 s)` (the permit
is held across the handover), exports the retention cache and exec context
into the pinned Linux-identical blob under `%ProgramData%\e2b\`, writes an
`ENVDWIN1` sidecar (duplicated socket, readiness pipe, empty process table),
spawns the successor suspended with `CreateProcessW` +
`PROC_THREAD_ATTRIBUTE_HANDLE_LIST` + `WSADuplicateSocketW`, waits for 'S',
closes the accept gate, waits for 'R', reports `SERVICE_STOPPED` in service
mode, and exits 0 without an HTTP response. Failures before 'R' terminate the
unconfirmed successor, delete the sidecar, re-open the gate, drop the freeze
hold and thaw, leaving the old envd serving and the request answering 500.
`src/platform/windows/handover.rs` owns the sidecar format, adoption
(`WSAStartup`/`WSASocketW`) and the successor spawn; the pinned blob is
unchanged, so the Linux schema gate still applies.

Declared deviations are listed in `docs/windows-port.md` §8.3: no live
process/watcher/mount carry (per-process kill-on-close Jobs end children at
the outgoing process's exit), a successor PID separate from the parent's, a
successor that is not SCM-registered, a two-phase readiness handshake instead
of `execve`, best-effort removal of the renamed predecessor image, and
thread-suspension freezing. **Windows behaviour is contract evidence only; no
differential claim.**

New unit tests cover the boot guard's four modes, the SCM transitions and
control handling, the event-log severity mapping, and the sidecar
round-trip/refusal and command-line quoting. Windows tests cannot run on this
Linux host.

Verification, Linux (this candidate): `cargo fmt --check` clean; `cargo check
-j4 --offline --all-targets` clean; `cargo check -j4 --offline --target
x86_64-pc-windows-msvc --lib --bins` clean; `cargo clippy -j4 --offline
--all-targets -- -D warnings` clean; `cargo clippy -j4 --offline --target
x86_64-pc-windows-msvc --lib --bins -- -D warnings` clean; `cargo test -j4
--offline` 818 passed / 0 failed. Verification, Windows (cross-compiled):
`cargo test --no-run --lib --target x86_64-pc-windows-msvc` compiled every
Windows test body and stopped only at the absent MSVC `link.exe`. The Windows
tests themselves have not run here.

## Windows port P1 — native guest validation and four runtime fixes (2026-11-04)

The Windows library suite ran natively for the first time, inside a Windows 11
guest (interactive session, `cargo test -j4 --lib -- --test-threads=1`,
`PROTOC` set): 275 passed / 0 failed, twice in a row. Until now these tests
only compiled for `x86_64-pc-windows-msvc` on the Linux host; the run found
four defects that cross-compilation cannot see.

`ReadDirectoryChangesW` on an `FILE_FLAG_OVERLAPPED` directory handle returns
nonzero when the request is queued, not completed, and `lpBytesReturned` is
undefined until the completion arrives. The watcher read the count
immediately, so every refresh ended in `ERROR_IO_INCOMPLETE` (996). It now
waits on the stop event and the overlapped event and then reads the count with
`GetOverlappedResult`; the seven watcher contract tests pass.

`seckey`'s non-Unix fallback toggles the token page between `PAGE_NOACCESS`
and `PAGE_READONLY` around every borrow, and `VirtualProtect` to
`PAGE_NOACCESS` drops the page's `VirtualLock`, so the later `VirtualUnlock`
reports `ERROR_NOT_LOCKED` and `region`'s `LockGuard::drop` panics. Windows
now keeps a fixed `Box<[u8]>` token buffer whose protection never changes,
locked once through the refcounted `PageLocks` registry.

A pseudoconsole child created without an explicit standard-handle triple
inherits the daemon's redirected handles. The PTY stream then carried only
conhost's 86-byte init sequence and every child byte landed in the daemon's
stdout — verified directly: `echo X` reached the caller's log,
`echo X>CONOUT$` reached the stream, and `mode con` reported the caller's
console. PTY children are now created with `STARTF_USESTDHANDLES` and
`INVALID_HANDLE_VALUE` for all three handles, so conhost's console handles are
used; `mode con` then reports the requested size and the 2 MiB PTY-output
contract test passes.

`mode con` is localized (the guest prints 行/列), so the resize contract test
parsed nothing on a zh-CN guest. It now reads the report structurally — the
first two `<label>: <number>` pairs are the line and column counts — and
asserts `[40, 100]` after the `Update`. Two smaller fixes came out of the same
run: the "vanished pid" tests use a never-existing pid (`u32::MAX - 3`)
instead of a recycled one, and a Ctrl-Break delivered while the child is still
in loader initialization aborts it with `STATUS_DLL_INIT_FAILED`
(`0xC0000142`) rather than `STATUS_CONTROL_C_EXIT`. The end synthesis maps
that code to `signal: terminated` only when a Term signal was accepted for
that child; an unsolicited loader failure keeps its raw exit status. The
SIGTERM contract test passed in both full runs after this change.

Declared deviations (also in `docs/windows-port.md`): the Windows token buffer
keeps one protection instead of `seckey`'s protection toggling, and a Term
that lands during loader initialization is reported as `signal: terminated`
although the child exits with `STATUS_DLL_INIT_FAILED`. **Windows behaviour is
contract evidence only; no differential claim.**

## Windows port P1 — loopback forwarding, the `/init` clock, and two native defeats (2026-11-04)

The last two platform slices are in. The loopback port forwarder is an
in-process Tokio proxy behind the oracle's scan/refresh state machine
(`docs/windows-port.md` §6.1), and `/init` now decodes `timestamp` with the
oracle's raw-bytes rules, corrects it by the handler delay, gates it
newest-wins, and steps the guest clock with `SetSystemTime` (§6.2). Both ride
shared code: `src/platform/port/` is the scan both daemons use, and
`src/clock.rs` is the whole correction contract, with only the step sink
platform-specific.

The native Windows 11 guest runs (`cargo test -j4 --lib --
--test-threads=1`) found two defects that neither the Linux host nor a
cross-compiled type-check could see:

The `SYSTEMTIME` conversion divided the sub-second tick remainder by 10
instead of 10 000, so every clock step carried a bogus millisecond field —
`123` ms arrived as `57920` after the `u16` cast. The conversion now divides
by `TICKS_PER_MILLISECOND`; the known-instant and pre-epoch unit tests pin
the field.

The `/init` decoder walked a `serde_json::Value`, which decodes JSON escapes
before the timestamp parse. Go hands `time.Time` and `SecureToken` the raw
bytes of their value, so a body the oracle answers 400 for — an escaped
timestamp, an escaped token — was accepted here with 204. The decoder now
captures those two members from the raw body, accepts a literal `null` body
as the zero struct the way Go does, and type-checks the Go-typed fields it
ignores (`cpuCount`, `volumeMounts`, `caBundle`, `hyperloopIP`,
`lifecycleID`), so a body Go rejects is a 400 on Windows too. The new
contract tests pin both rules.

One native flake was fixed as well:
`process_stream_input_orders_selection_and_reports_go_wrappers` started
`more`, closed its stdin and immediately asserted the input rules; `more`
exits on the closed stdin and can be reaped first, so a run answered
`not_found` instead of the input errors. The child is now `ping -n 3`, which
outlives the round trips and still ends with the expected exit code.

Verification, Windows (native guest, twice back to back): 305 passed /
0 failed in both runs, exit 0. Verification, Linux (this candidate):
`cargo fmt --check` clean; `cargo check --locked --all-targets` clean;
Windows target `cargo check`/`clippy -D warnings` on `--lib --bins` clean;
`cargo clippy --all-targets -D warnings` clean; `cargo test -j4 --offline`
819 passed / 0 failed; `python3 scripts/generate_notices.py --check` matches
the locked graph. **Windows behaviour is contract evidence only; no
differential claim.**

## Windows port P0/P1 — one neutral `/init` decoder and official-SDK conformance (2026-11-04)

`/init` is now decoded by one module for both targets
(`src/init_payload.rs`): the request visitor, the raw-bytes capture of
`timestamp`/`accessToken`, Go's field folding, the Go-typed checks for members
the daemon ignores, and the streamed body reader. Linux's daemon and the
Windows daemon call the same `decode` and `collect_body`, so a decoder
divergence cannot exist on one target only. This also removes the Windows
body reader's own 32 MB `to_bytes` cap (the oracle is unbounded) and deletes
the NFS module's local `VolumeMount` decoder, whose folding missed U+212A.

Evidence, Windows guest (`cargo test -j4 --lib -- --test-threads=1`): 314
passed / 0 failed, exit 0. Evidence, Linux host: `cargo test -j4 --offline`
830 passed; `cargo fmt --check`, `cargo check --locked --all-targets` and
`cargo clippy --all-targets -D warnings` clean.

The official E2B Python SDK (2.62.1) then drove the live daemon through its
own codec, generated Connect clients and `CommandHandle`
(`scripts/tests/envd-parity/sdk_conformance.py` in the agent repo): REST
health/init/envs/metrics/upload/download/Range/compose, filesystem
write/read/info/exists/list/make_dir/rename/remove/watchers, and process
start/list/kill/connect/stdin. Result: **27/27 on the Windows guest** and
**27/27 on the Linux daemon** (the Linux run passes `--user xuyuan`: with no
`defaultUser` the oracle's fallback is the compiled-in `root`, and a non-root
daemon then correctly fails to spawn and chown as root). The run corrected three
harness assumptions
against the pinned Go source (flat `/envs` body, `source_paths` in the
compose body, compose consuming its sources), not the daemon.

Declared deviation: the SDK's `commands.run` hardcodes `/bin/bash -l -c`, so
a Windows guest refuses it with `invalid_argument` naming the missing
executable; the process surface is otherwise fully exercised. Windows
behaviour remains contract evidence only; no differential claim.
