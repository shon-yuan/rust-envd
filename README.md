# rust-envd

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
`src/mmds/go_url.rs` retains The Go Authors' BSD-3-Clause notice.
`vendor/connectrpc/` is an Apache-2.0 in-tree fork of `connectrpc` 0.9.1
carrying connect-go error-text parity; its changes are recorded in
`vendor/connectrpc/PATCHES.md`. `vendor/hyper/` is an MIT in-tree fork of
`hyper` 1.12.0 carrying Go `net/http`'s HTTP/1.1 trailer write path and
chunked-body limits; its changes are recorded in `vendor/hyper/PATCHES.md`.
The distributable notice bundle
([`third_party/NOTICES.md`](third_party/NOTICES.md), `third_party/licenses/`)
and the release manifest procedure are specified in
[`third_party/PACKAGING.md`](third_party/PACKAGING.md).

## Platforms

Linux x86_64 is the only supported target today, the same as the Go daemon.
A Windows sandbox (the daemon running inside a Windows guest and serving the
complete API) is evaluated, with an endpoint-by-endpoint compatibility matrix
and a phased plan, in [docs/windows-port.md](docs/windows-port.md); nothing
of that port is implemented yet.

## Differences from the Go daemon

Within the differential corpora listed in
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
cargo build -j4 --locked -p coorworker-envd \
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
