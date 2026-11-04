# Packaging — what a covered distribution must include

A **covered distribution** of this project is any linked `envd` binary built
from this crate (both the default **base** and optional **desktop** variant),
any guest or sandbox image that embeds such a binary, and the optional
standalone `coorworker-cua` worker built from `cua-worker/`. The desktop daemon
enables `cua-desktop` and links the worker library in process. The base daemon
remains desktop-free. Every covered distribution must ship:

| # | Artifact | Why |
|---|----------|-----|
| 1 | The built binary (`target/release/coorworker-envd`) | The distributed program. |
| 2 | `LICENSE` | The project's MIT grant. |
| 3 | `third_party/e2b/LICENSE` and `third_party/e2b/PROVENANCE` | e2b protobuf schemas redistributed under Apache-2.0; generated code built from them carries the same attribution. |
| 4 | `third_party/NOTICES.md` | Aggregate third-party notice: scope statement, project MIT grant, e2b Apache-2.0 pointer, Go BSD-3-Clause notice, per-crate index and copyright lines. |
| 5 | The complete `third_party/licenses/` tree | Verbatim license texts for the root dependency superset across all features and platforms, including `cua-desktop`, one directory per third-party crate-version. |
| 6 | The Go Authors' BSD-3-Clause notice for `src/platform/linux/mmds/go_url.rs` | Required for binary redistribution; the notice is reproduced in `third_party/NOTICES.md` ("Copyright 2009 The Go Authors."), so including item 4 satisfies it. Source distributions must also keep the notice in the file header itself. |
| 7 | `vendor/connectrpc/LICENSE` + `vendor/connectrpc/PATCHES.md`, and `vendor/hyper/LICENSE` + `vendor/hyper/PATCHES.md` | The in-tree `connectrpc` 0.9.1 (Apache-2.0) and `hyper` 1.12.0 (MIT) forks selected by `[patch.crates-io]`; each license text is also enumerated under `licenses/`, and each `PATCHES.md` records the upstream revision and every local change. |
| 8 | A provenance manifest from `scripts/packaging_manifest.py` | Binds the binary to `git rev-parse HEAD`, dirty state, toolchain, declared artifact kind, enabled root features, target, canonical build command, and SHA-256 digests of every covered file and the whole license tree. |
| 9 | `third_party/cua-worker/NOTICES.md` and the complete `third_party/cua-worker/licenses/` tree | The standalone worker's own notice bundle, generated with all features from `cua-worker/Cargo.lock`. It stays separate because the worker is a separate package with its own lockfile; the root bundle covers embedded desktop linking. |
| 10 | `cua-worker/Cargo.toml` + `cua-worker/Cargo.lock` | Pins the worker's dependency graph, including path dependencies on the vendored Cua Driver crates. |
| 11 | `vendor/cua-driver/LICENSE` and `vendor/cua-driver/PROVENANCE.md` | MIT license for the vendored Cua Driver crates and the upstream commit plus the deliberately excluded AGPL/FSL components. |

Do not drop, truncate, or reflow items 2–5 or 9: the license texts are copied
verbatim from the crates.io packages and must stay byte-for-byte. The only
non-package texts are the SPDX overrides in `scripts/license-overrides/`,
which are used solely for crates that ship no license file, and the vendored
Cua Driver repository license, which is used for its crates that ship no
per-crate file. The first-party `coorworker-cua` library is covered by the
project's root MIT license, rather than being labeled as a crates.io patch.
The vendored Cua Driver crates use `vendor/cua-driver/PROVENANCE.md` for the
upstream revision and local changes; the connectrpc and hyper forks retain
their own `PATCHES.md` files. AGPL perception components and FSL cua-spacesd
remain excluded and must not be shipped or linked. The generator rejects
packages whose declared license expression includes AGPL or FSL.

## Canonical build

From a clean checkout at the pinned revision:

```sh
git rev-parse HEAD            # record this with the artifact
cargo build --locked -j4 --release   # default base variant, without desktop dependencies
```

`--locked` makes cargo use exactly the versions in `Cargo.lock` — the same
versions covered by `third_party/NOTICES.md`. The distributed guest binary is
the static musl build:

```sh
CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
CC_x86_64_unknown_linux_musl=clang AR_x86_64_unknown_linux_musl=llvm-ar \
cargo build --locked --release -j4 --target x86_64-unknown-linux-musl
```

The optional desktop daemon uses a desktop-capable platform target (for
example Linux GNU or Windows MSVC):

```sh
cargo build --locked -j4 --release --features cua-desktop
# Cross-target example, with the appropriate platform toolchain installed:
cargo build --locked -j4 --release --features cua-desktop --target x86_64-pc-windows-msvc
```

The static guest build above remains the base variant. Native Windows MSVC
desktop builds and the standalone worker additionally require the MSVC v143
x64/x86 Spectre-mitigated libraries for the active toolset, installed into the
selected Visual Studio instance. The component ID is
`Microsoft.VisualStudio.Component.VC.Runtimes.x86.x64.Spectre`, or the matching
version-specific component such as
`Microsoft.VisualStudio.Component.VC.14.44.17.14.x86.x64.Spectre` for MSVC 14.44.
The desktop dependency's `msvc_spectre_libs` build script checks for these libraries.
Component names and unattended installation parameters are documented in the
[Microsoft component catalog](https://learn.microsoft.com/en-us/visualstudio/install/workload-component-id-vs-build-tools?view=vs-2022)
and [installer command reference](https://learn.microsoft.com/en-us/visualstudio/install/use-command-line-parameters-to-install-visual-studio?view=vs-2022).

Distribution notices are deliberately a superset and do not enable desktop
dependencies in a base build. The standalone worker is built from its own package and lockfile:

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
    --artifact-kind base --target x86_64-unknown-linux-musl \
    --output dist/provenance.json
  ```

  For a desktop build:

  ```sh
  python3 scripts/packaging_manifest.py \
    --binary target/x86_64-pc-windows-msvc/release/coorworker-envd.exe \
    --artifact-kind desktop --target x86_64-pc-windows-msvc \
    --output dist/provenance-desktop.json
  ```

  `--artifact-kind` accepts `base` (the default, `features: []`) or `desktop`
  (`features: ["cua-desktop"]`). The manifest records `artifact_kind`,
  `features`, `target`, and the corresponding release `build_command`,
  including `--features cua-desktop` only for desktop. Pass `--target` when
  the build used it; omitted targets are recorded as null for a native build.
  These are the packager's declarations: the script hashes the binary but
  does not infer or verify its compiled feature set. Match them to the actual
  build invocation. It records the Cargo command; the musl linker/compiler
  environment shown above must also be retained in the build logs.

  The script refuses a dirty tree unless `--allow-dirty` is passed (the
  resulting manifest still records `dirty: true`), refuses missing covered
  files, and rejects a notice bundle that does not match `Cargo.lock`.
- When a manifest, feature definition, path dependency, or either lockfile
  changes (the daemon's `Cargo.lock` or `cua-worker/Cargo.lock`), refresh the
  notice bundles before distributing:

  ```sh
  python3 scripts/generate_notices.py          # regenerate both NOTICES.md + licenses/ trees
  python3 scripts/generate_notices.py --check  # CI-style verification of both
  ```

  The generator fails closed: any crate without a discoverable license
  aborts the bundle rather than shipping a partial notice. For both packages
  it runs `cargo metadata --locked --offline --all-features --format-version 1`
  without a target filter, covering all root features and target platforms.
  Populate the registry cache for both packages and all features before an
  offline check, for example:

  ```sh
  cargo fetch --locked
  cargo fetch --locked --manifest-path cua-worker/Cargo.toml
  ```

  If metadata is unavailable, the fallback enumerates the whole lockfile and
  resolves local path crates from their manifests, including inherited
  workspace licenses. An unresolved locked path crate aborts generation.
  Regeneration writes both notice bundles, even when only one graph changed.

The script regressions run without Cargo, external packages, or bundle writes:

```sh
python3 -I -B -m unittest discover -s scripts -p test_distribution.py -v
```

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
