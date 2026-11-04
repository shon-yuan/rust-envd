# Provenance: vendored Cua Driver crates

Source: <https://github.com/trycua/cua>, `libs/cua-driver/rust`, commit
`d6855806e6eebfa9813b91986f59931e7f103a51` (2026-11-04).

License: MIT (`LICENSE` in this directory, copied verbatim from the upstream
repository root). Every vendored crate carries the upstream
`license.workspace = true` declaration (MIT).

Vendored crates, byte-for-byte except for `rust/Cargo.toml` (members list
pruned; see below):

- `crates/cua-driver-core`
- `crates/cua-driver-contract`
- `crates/platform-linux`
- `crates/platform-windows`
- `crates/cursor-overlay`
- `crates/cua-cursor-motion`
- `crates/pip-preview`
- `rust/Skills/` — the skill documents `cua-driver-core` embeds with
  `include_str!`; the crate does not build without them.

Deliberately not vendored:

- `crates/cua-perception` and the perception extension: AGPL-3.0-only
  components. rust-envd must not ship or link them.
- `crates/cua-driver` (standalone MCP binary), `cua-driver-sdk`,
  `cua-driver-bindgen`, `cua-driver-testkit`, `cua-driver-e2e`,
  `cua-driver-uia`, `cursor-theme-cli`, `platform-macos`: not used by the
  rust-envd worker.
- `libs/cua-spacesd`: FSL-1.1-MIT (source-available with a competing-use
  restriction), not MIT. It must not be merged into this repository.

`rust/Cargo.toml` is the upstream workspace manifest with `members` reduced to
the vendored set. `rust/Cargo.lock` is upstream's lock, unmodified.

Update procedure: re-copy the same seven crate directories and the two
manifests from a reviewed upstream commit, then re-apply the `members` edit.
