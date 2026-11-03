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
