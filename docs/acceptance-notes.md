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
verified complete; and every system-init effect is implemented except a
non-empty `volumeMounts` list, which keeps answering 501 because NFS mounting
remains unfinished pending separate authorization. MMDS init authentication now
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
Guest mode enables the OOM watcher unless `--isnotfc` is supplied; development mode
keeps it disabled. Known history is returned oldest first, up to 32 kills, while
unknown history is omitted. A dedicated dual-VM run validated one real kernel OOM,
rejection of a userspace-forged kmsg record, and repeated nonconsuming snapshots at
`/tmp/envd-oom-vm-az03i6p_/result-1.json`. The corrected harness enforces a host
absolute stream deadline; independent source review closed the original timeout
finding. It does not establish the 32-entry limit or
reopen/recovery behavior against real kernel logs; those currently have injected tests.
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
protocol inputs and system behavior remain unfinished. Requests for
unimplemented system init effects return 501. Existing serving processes and
images have not been switched.

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
those notices. The Go-derived redirect URL helper in `src/mmds/go_url.rs` retains
The Go Authors' BSD-3-Clause license; binary redistribution must reproduce that
notice too. The HTTP/TLS path also links dependencies with ISC and Unicode-3.0
obligations, including ring, WebPKI and ICU. Cargo metadata has been inspected; a
complete distributable dependency notice bundle and guest/image packaging are still
pending.
