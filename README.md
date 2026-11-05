# rust-envd

English | [简体中文](README.zh-CN.md)

A Rust implementation of the [e2b](https://github.com/e2b-dev/runtime) envd
sandbox daemon, functionally aligned with the pinned upstream release
`0.10.2` (commit `51083a83cc0903185d992725ea6bb33683814b29`): process
runtime, filesystem REST + Connect RPCs, PTY/watchers, MMDS metadata
lifecycle, init effects (clock, CA bundle, hyperloop, cgroups/freeze),
port forwarding, upgrade handover, and the daemon CLI.

Licensed under [MIT](LICENSE) except `third_party/e2b/`, which contains
upstream protobuf schemas redistributed byte-for-byte under their
[Apache-2.0 license](third_party/e2b/LICENSE) with full
[provenance](third_party/e2b/PROVENANCE); generated code built from those
schemas carries the same attribution. The Go-derived redirect URL helper in
`src/platform/linux/mmds/go_url.rs` retains The Go Authors' BSD-3-Clause notice.
`vendor/connectrpc/` is an Apache-2.0 in-tree fork of `connectrpc` 0.9.1
carrying connect-go error-text parity; its changes are recorded in
`vendor/connectrpc/PATCHES.md`. `vendor/hyper/` is an MIT in-tree fork of
`hyper` 1.12.0 carrying Go `net/http`'s HTTP/1.1 trailer write path and
chunked-body limits; its changes are recorded in `vendor/hyper/PATCHES.md`.
`vendor/cua-driver/` contains MIT-licensed Cua Driver crates for envd's
in-process desktop registry (provenance in
`vendor/cua-driver/PROVENANCE.md`); the AGPL perception crates and the FSL
`cua-spacesd` are excluded.
The distributable notice bundle
([`third_party/NOTICES.md`](third_party/NOTICES.md), `third_party/licenses/`)
and release manifest procedure are specified in
[`third_party/PACKAGING.md`](third_party/PACKAGING.md).
Dependency security updates, remaining maintenance warnings and the Linux
handover ownership migration are recorded in
[`docs/dependency-security.md`](docs/dependency-security.md).

## Platforms

Linux x86_64 is the differential-parity target: the tables below and the
differential gates in [`docs/acceptance-notes.md`](docs/acceptance-notes.md)
are Linux evidence.

Windows x86_64 is a supported guest target (tested on Windows 11) with a
native Windows backend behind the same API. Both daemons serve the complete
surface — all 13 REST operations and all 17 Connect procedures — with the
same schemas, status codes, error envelopes and Connect framing:

- **Process**: `CreateProcessW` under job objects, ConPTY pseudo-terminals,
  `CTRL_BREAK`/ETX/`WM_CLOSE` termination for signals, retained-exit registry.
- **Filesystem**: the REST and RPC surface, with watchers over
  `ReadDirectoryChangesW`.
- **Resources**: metrics from Win32 counters, freeze/unfreeze through thread
  suspension, and the collapse/fsfreeze backends.
- **Ports**: scanning through `GetExtendedTcpTable` and the in-process
  loopback forwarder on `169.254.0.21`.
- **`/init`**: token and defaults, CA install into the Windows certificate
  store, clock correction and step through `SetSystemTime`, thaw; metadata
  persistence through DPAPI.
- **Lifecycle**: incoming handover adoption and outgoing live upgrade, and
  the SCM service lifecycle (a service is a guest by construction; a console
  start needs `COORWORKER_ENVD=1` and binds `0.0.0.0:49983` like the oracle).

Build it with a Windows MSVC toolchain and `PROTOC` set to a `protoc`
executable:

```sh
cargo build --locked --bin coorworker-envd
```

Historical Windows evidence: the [dependency maintenance regression](docs/windows-dependency-regression.md)
runs natively on Windows 11 at commit `c3cb81a`: 321 library tests plus 5 base
or 9 desktop integration tests, and 16 standalone-worker tests passed. The
official E2B Python SDK 2.62.1 drives a live guest through its own codec,
generated clients and `CommandHandle` (27/27; the agent repo's
`scripts/tests/envd-parity/sdk_conformance.py`). There is no Windows oracle,
so Windows behaviour is contract evidence only — no differential claim is
made. Declared deviations: freeze is thread suspension rather than cgroup v2;
processes run as the daemon's own account (no arbitrary-user impersonation);
and the SDK's `commands.run`, which hardcodes `/bin/bash -l -c`, is refused
with `invalid_argument` because a Windows guest has no POSIX shell.

The full API matrix, semantics, native evidence log and open decisions are in
[`docs/windows-port.md`](docs/windows-port.md). Booting Windows under Cloud
Hypervisor/Hyper-V (the guest image pipeline) is separate open work; the
daemon itself runs in an existing Windows VM.

## Native CUA

The official envd surface above remains frozen. The implemented
[native CUA contract](docs/native-cua.md) adds four REST routes on the existing
envd listener, inside the S0 routing and authentication stack:

| Endpoint | Purpose |
| --- | --- |
| `GET /cua/tools` | List tools with `name`, `description` and `input_schema`. |
| `POST /cua/call` | Execute `{"name":"…","arguments":{…}}`; return `success`, `content` and optional `data`. |
| `GET /cua/health` | Report passive runtime availability and reason without touching the desktop. |
| `GET /cua/metrics` | Expose bounded Prometheus tool counters, latency and runtime readiness. |

Every CUA route, including unknown `/cua` paths, requires completed envd
initialization and a nonempty current `X-Access-Token`. Bearer, Basic and file
signatures cannot substitute for that header. Authentication precedes body
decoding; existing CORS preflight behavior and S0 authentication exceptions
remain unchanged. Token rotation affects subsequent requests; an admitted
action can finish after rotation. Calls use the native JSON body above, with
exactly `name` and object-valued `arguments`; there is no JSON-RPC envelope or
initialization handshake. Images use `mime_type`, tool failures use
`success:false`, and transport errors use S0's `code`/`message` JSON envelope.
Native `data` preserves raw driver diagnostics, with `{"code":"tool_error"}`
for a failure without structured data. Discovery omits the vendor's normalized
output schema because it does not describe all raw diagnostic payloads.

The only execution engine is the in-process vendored desktop registry, linked
directly by envd. There is no extension mounting framework, MCP endpoint,
worker, helper transport or execution-mode selector. `/cua/mcp`, `/cua/info`
and independent `coorworker-cua` startup are removed. Remove old
`COORWORKER_CUA_*` configuration and `CUA_ENV_TOKEN` from the launch
environment: production startup rejects them with a migration error.

The ordinary `cargo build --locked -j4` enables `cua-desktop` by default.
Linux desktop builds need the X11/XInput/XTest/XFixes/XRandR/XComposite/XRender/
XExt/XDamage and xkbcommon development headers. Windows desktop builds require
matching MSVC v143 x64/x86 Spectre-mitigated libraries. Desktop dependencies
remain optional: `--no-default-features` builds the desktop-free base/static
guest; authenticated CUA operations then report unavailable.

Linux and known Windows desktop user sessions are eligible for the registry.
Windows SCM-attached processes, Session 0, service handover successors in
Session 0, and unknown sessions report CUA unavailable before constructing a
registry. They do not contact or launch a helper; S0 filesystem and process
services continue serving. Model/provider credentials belong in AgentHost.

Local verification of the integrated native API passed: default 907 tests and
base 881 tests, zero failures, with 24 doctests ignored in each
configuration. Final strict Clippy for default and base across all targets,
formatting, the Windows base MSVC library/binary crosscheck, 17 distribution
checks, notices verification and the rebuild passed. The desktop matrix includes 15 completion
regressions covering finite work after timeout, disconnect and panic, including
resumed I/O after expiry. The final Rust recheck closed the deadline finding
alongside code/security follow-up. Observed Linux smoke found 63 tools, one
envd process and one listener, rejected old MCP/JSON-RPC/configuration, and
performed no desktop actions; the final rebuilt repetition passed. Windows
native desktop/live S0 execution, a fresh musl build, fresh Go/SDK comparison,
remote CI and real desktop actions are outside this evidence; no commit, push,
merge or deployment is claimed. See [the local verification record](docs/native-cua.md#verified-local-state).

The earlier [Windows CUA acceptance](docs/windows-cua-acceptance.md) records
embedded capture/input/window actions and SCM/helper recovery at an older
revision. Its DPAPI rotation and live-reset limitation remain historical
evidence. Current protocol details, limits, migration guidance and explicitly
scoped older evidence are in [`docs/extensions.md`](docs/extensions.md).

## Differences from the Go daemon

On Linux, within the differential corpora listed in
[`docs/acceptance-notes.md`](docs/acceptance-notes.md), the daemon is
semantically identical to the pinned Go binary: status lines, header
values, framing, trailers and bodies match, and every VM gate passes on the
same candidate. The differences that remain are these.

### Deliberate behaviour differences

| Difference | Go envd | This daemon |
| --- | --- | --- |
| Boot mode | Runs the guest path whenever started; `--isnotfc` only skips MMDS and the log exporter | Requires `--guest`, root, PID 1 and `coorworker.envd=1` on the kernel command line. A bare Go-style invocation (`coorworker-envd --port 49983`) refuses to start, so switching an image also changes its launch line and kernel arguments. `--development` (ordinary user, loopback address) is the local mode. |
| Port-forwarder availability | `forwardingAvailable()` returns nil unconditionally (`internal/port/forward.go:77`) | Only a `--guest` daemon scans ports and spawns `socat`; development mode never touches host loopback services. Guest behaviour is identical. |
| `cpu-online` helper identity | The reexec helper writes the online file with no boot check | Additionally requires root, the kernel opt-in and `/proc/1/exe` to be the helper itself (`src/guest.rs`), a hardening deviation. |

The three extra flags (`--development`, `--guest`, `--listen`) are not
advertised: `-h` lists exactly the oracle's eight flags and reproduces its
usage text apart from the program name in the first line. Conversely every
oracle flag, both dash forms, `=value`, base-0 int64 parsing and Go `flag`
diagnostics/exit codes are reproduced.

### Windows behaviour

Windows has no Go oracle, so the Windows backend is verified by contract
tests and by the official SDK rather than by differential comparison. Its
declared deviations are:

- **Freeze** suspends threads instead of freezing a cgroup v2 subtree.
- **Processes** always run as the daemon's own account; there is no
  arbitrary-user impersonation.
- **`X-Envd-Memory`** reports `partial: true` with zeroed figures, because
  there is no cgroup from which to read them.
- **`permissions`** carries only the `r`/`w`/`x` triples; Windows has no
  POSIX special bits.
- **The SDK's `commands.run`** is refused: it hardcodes `/bin/bash -l -c`,
  and a Windows guest has no POSIX shell.

### Comparison granularity

- Header names are compared case-insensitively and order-insensitively:
  Go writes canonical MIME casing in sorted order, this daemon writes
  lowercase in insertion order. Values themselves are compared.
- `Date` and volatile runtime values (clock, PIDs, CPU/memory samples,
  latencies) are normalised; bodies are compared byte-for-byte.
- Inputs outside the captured corpora are not claimed, including arbitrary
  malformed protocol traffic and unenumerated system behaviour.

### Outside the parity claim

- TLS and a completed h2c switch. Go envd itself serves plaintext HTTP/1.1
  (`main.go:328-335`), and the corpus compares both sides' rejection paths.
- Chains longer than eight consecutive upgrade swaps (they repeat the same
  export/relocate path).
- The production Firecracker transport path and the host facade, and the
  Go repository's separate host-side proxies (`client-proxy`, the
  orchestrator `tcpfirewall`), which are different programs.
- A distributable release artifact: the provenance manifest must be
  regenerated from a clean checkout of the published commit.

## Build and test

```sh
cargo build --locked -j4
cargo test -j4 --locked
cargo clippy -j4 --locked --all-targets -- -D warnings
cargo fmt --check
```

Code generation runs locally in `build.rs` via `protoc` and
`connectrpc-build`; nothing is downloaded. The engineering acceptance log —
capability narrative, differential evidence, and the current limitation
list — lives in [docs/acceptance-notes.md](docs/acceptance-notes.md). NFS
`volumeMounts` mounts through the real `mount.nfs` path and passes the
isolated QEMU differential gate against the pinned Go oracle; the sandbox
serving images have not switched.

## Usage

### Development instance

```sh
cargo run -j4 --locked -p coorworker-envd -- --development --listen 127.0.0.1:0
```

Runs as the current ordinary user on a loopback address (root is rejected) —
enough to exercise the HTTP/Connect surface locally.

### Guest instance inside a microVM

In production shape the daemon is the guest's PID 1. Startup enforces four
things: the `--guest` flag, effective root, PID 1, and `coorworker.envd=1`
on the kernel command line. Build the static guest binary first:

```sh
CC_x86_64_unknown_linux_musl=/usr/bin/clang \
CFLAGS_x86_64_unknown_linux_musl='-ffreestanding -nostdlibinc -DRING_CORE_NOSTDLIBINC=1' \
cargo build -j4 --locked -p coorworker-envd --no-default-features \
  --target x86_64-unknown-linux-musl --release --bin coorworker-envd
```

A minimal initramfs guest has this shape (busybox plus the musl loader as
the only userspace, a root account, one NIC driver):

```
/envd                              # the static binary built above
/bin/busybox + /bin/{sh,mount,...} # busybox applet symlinks
/lib/ld-musl-x86_64.so.1
/lib/e1000.ko                      # unpacked from the host's e1000.ko.zst
/etc/passwd, /etc/group            # root account entries
/init:
  #!/bin/sh
  export PATH=/bin
  mount -t proc proc /proc
  mount -t sysfs sysfs /sys
  mount -t devtmpfs devtmpfs /dev
  mkdir -p /dev/pts && mount -t devpts devpts /dev/pts
  insmod /lib/e1000.ko || poweroff -f
  ip link set lo up
  ip link set eth0 up
  ip addr add 10.0.2.15/24 dev eth0
  exec /envd --guest --isnotfc --listen 0.0.0.0:49983
```

Boot it with any x86_64 bzImage (an ELF `vmlinux` without a PVH note, such
as the Firecracker quickstart kernel, is not loadable by `qemu -kernel`):

```sh
qemu-system-x86_64 -no-user-config -nodefaults -display none -serial stdio \
  -no-reboot -accel kvm -m 384 -smp 2 \
  -kernel /boot/vmlinuz-linux -initrd initramfs.cpio.gz \
  -append "console=ttyS0 rdinit=/init panic=-1 coorworker.envd=1" \
  -netdev user,id=n,restrict=on,hostfwd=tcp:127.0.0.1:49983-:49983 \
  -device e1000,netdev=n
```

- `--isnotfc` skips the MMDS poll and the HTTP log exporter, so the
  instance starts fresh and accepts its first `/init` access token.
- The network shape above gives no default route: the guest cannot reach
  metadata, cloud or public services; only the one host forward exists.
- Readiness is the forwarded `GET /health` answering `204`.
- Prerequisites: `qemu-system-x86_64`, `zstd` (to unpack the module) and
  unprivileged access to `/dev/kvm` (`-accel tcg` works without KVM,
  slower).

The dual-VM differential harness that boots a Go oracle and this binary
side by side lives in the internal workspace's parity suite, not in this
repository; production Firecracker launch (persistent ext4 rootfs, MMDS
wiring) is a separate deployment track.
