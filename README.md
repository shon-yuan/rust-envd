# rust-envd

A Rust implementation of the [e2b](https://github.com/e2b-dev/runtime) envd
sandbox daemon, functionally aligned with the pinned upstream release
`0.10.2` (commit `51083a83cc0903185d992725ea6bb33683814b29`): process
runtime, filesystem REST + Connect RPCs, PTY/watchers, MMDS metadata
lifecycle, init effects (clock, CA bundle, hyperloop, cgroups/freeze),
port forwarding, upgrade handover, and the daemon CLI.

Licensed under [MIT](LICENSE) except `crates/envd/third_party/e2b/`, which
contains upstream protobuf schemas redistributed byte-for-byte under their
[Apache-2.0 license](crates/envd/third_party/e2b/LICENSE) with full
[provenance](crates/envd/third_party/e2b/PROVENANCE); generated code built
from those schemas carries the same attribution. The Go-derived redirect URL
helper in `crates/envd/src/mmds/go_url.rs` retains The Go Authors'
BSD-3-Clause notice.

## Build and test

```sh
cargo build --locked -j4 -p coorworker-envd
cargo test -j4 --locked -p coorworker-envd
cargo clippy -j4 --locked -p coorworker-envd --all-targets -- -D warnings
cargo fmt -p coorworker-envd --check
```

Code generation runs locally in `build.rs` via `protoc` and
`connectrpc-build`; nothing is downloaded. See
[crates/envd/README.md](crates/envd/README.md) for the full capability
narrative, acceptance evidence, and the current limitation list (NFS volume
mounts are not implemented and answer `501`).
