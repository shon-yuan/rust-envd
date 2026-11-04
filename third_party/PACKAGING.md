# Packaging — what a covered distribution must include

A **covered distribution** of this project is any linked `envd` binary built
from this crate, any guest or sandbox image that embeds such a binary, and the
optional `coorworker-cua` worker built from `cua-worker/`. Every covered
distribution must ship:

| # | Artifact | Why |
|---|----------|-----|
| 1 | The built binary (`target/release/coorworker-envd`) | The distributed program. |
| 2 | `LICENSE` | The project's MIT grant. |
| 3 | `third_party/e2b/LICENSE` and `third_party/e2b/PROVENANCE` | e2b protobuf schemas redistributed under Apache-2.0; generated code built from them carries the same attribution. |
| 4 | `third_party/NOTICES.md` | Aggregate third-party notice: scope statement, project MIT grant, e2b Apache-2.0 pointer, Go BSD-3-Clause notice, per-crate index and copyright lines. |
| 5 | The complete `third_party/licenses/` tree | Verbatim license texts for every crate in the locked dependency graph, one directory per crate-version. |
| 6 | The Go Authors' BSD-3-Clause notice for `src/platform/linux/mmds/go_url.rs` | Required for binary redistribution; the notice is reproduced in `third_party/NOTICES.md` ("Copyright 2009 The Go Authors."), so including item 4 satisfies it. Source distributions must also keep the notice in the file header itself. |
| 7 | `vendor/connectrpc/LICENSE` + `vendor/connectrpc/PATCHES.md`, and `vendor/hyper/LICENSE` + `vendor/hyper/PATCHES.md` | The in-tree `connectrpc` 0.9.1 (Apache-2.0) and `hyper` 1.12.0 (MIT) forks selected by `[patch.crates-io]`; each license text is also enumerated under `licenses/`, and each `PATCHES.md` records the upstream revision and every local change. |
| 8 | A provenance manifest from `scripts/packaging_manifest.py` | Binds the binary to `git rev-parse HEAD`, dirty state, toolchain, canonical build command, and SHA-256 digests of every covered file and the whole license tree. |
| 9 | `third_party/cua-worker/NOTICES.md` and the complete `third_party/cua-worker/licenses/` tree | The worker's own notice bundle, generated from `cua-worker/Cargo.lock` (502 packages, 501 third-party crates). It is separate because the worker is a separate package with its own lockfile. |
| 10 | `cua-worker/Cargo.toml` + `cua-worker/Cargo.lock` | Pins the worker's dependency graph, including path dependencies on the vendored Cua Driver crates. |
| 11 | `vendor/cua-driver/LICENSE` and `vendor/cua-driver/PROVENANCE.md` | MIT license for the vendored Cua Driver crates and the upstream commit plus the deliberately excluded AGPL/FSL components. |

Do not drop, truncate, or reflow items 2–5 or 9: the license texts are copied
verbatim from the crates.io packages and must stay byte-for-byte. The only
non-package texts are the SPDX overrides in `scripts/license-overrides/`,
which are used solely for crates that ship no license file, and the vendored
Cua Driver repository license, which is used for its crates that ship no
per-crate file.

## Canonical build

From a clean checkout at the pinned revision:

```sh
git rev-parse HEAD            # record this with the artifact
cargo build --locked -j4      # or: cargo build --locked --release -j4
```

`--locked` makes cargo use exactly the versions in `Cargo.lock` — the same
versions covered by `third_party/NOTICES.md`. The distributed guest binary is
the static musl build:

```sh
CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
CC_x86_64_unknown_linux_musl=clang AR_x86_64_unknown_linux_musl=llvm-ar \
cargo build --locked --release -j4 --target x86_64-unknown-linux-musl
```

The worker is built from its own package and lockfile:

```sh
(cd cua-worker && cargo build --locked -j4 --release --bin coorworker-cua)
```

## Binding artifacts to source

- Record `git rev-parse HEAD` (and `git status --porcelain` to prove a clean
  tree) next to each artifact.
- Build only with `cargo build --locked`; never an unlocked build.
- Emit the provenance manifest for the exact artifact being distributed:

  ```sh
  python3 scripts/packaging_manifest.py \
    --binary target/x86_64-unknown-linux-musl/release/coorworker-envd \
    --target x86_64-unknown-linux-musl --output dist/provenance.json
  ```

  The script refuses a dirty tree unless `--allow-dirty` is passed (the
  resulting manifest still records `dirty: true`), refuses missing covered
  files, and rejects a notice bundle that does not match `Cargo.lock`.
- When either `Cargo.lock` changes (the daemon's or `cua-worker/Cargo.lock`),
  refresh the notice bundles before distributing:

  ```sh
  python3 scripts/generate_notices.py          # regenerate both NOTICES.md + licenses/ trees
  python3 scripts/generate_notices.py --check  # CI-style verification of both
  ```

  The generator fails closed: any crate without a discoverable license
  aborts the bundle rather than shipping a partial notice. The bundles
  cover every platform in each lockfile, so `cargo fetch --locked` must have
  populated the registry cache for both packages (an offline `cargo metadata`
  needs those sources); CI fetches both lockfiles before running the check.

## Completeness check

- [ ] binary
- [ ] `LICENSE`
- [ ] `third_party/e2b/LICENSE` + `third_party/e2b/PROVENANCE`
- [ ] `third_party/NOTICES.md`
- [ ] `third_party/licenses/` — full tree, unmodified
- [ ] `vendor/connectrpc/LICENSE` + `vendor/connectrpc/PATCHES.md`
- [ ] `vendor/hyper/LICENSE` + `vendor/hyper/PATCHES.md`
- [ ] `third_party/cua-worker/NOTICES.md`
- [ ] `third_party/cua-worker/licenses/` — full tree, unmodified
- [ ] `cua-worker/Cargo.toml` + `cua-worker/Cargo.lock`
- [ ] `vendor/cua-driver/LICENSE` + `vendor/cua-driver/PROVENANCE.md`
- [ ] provenance manifest for this artifact
