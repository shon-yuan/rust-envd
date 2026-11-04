# Provenance: vendored Cua Driver crates

Source: <https://github.com/trycua/cua>, `libs/cua-driver/rust`, commit
`d6855806e6eebfa9813b91986f59931e7f103a51` (2026-11-04).

License: MIT (`LICENSE` in this directory, copied verbatim from the upstream
repository root). Every vendored crate carries the upstream
`license.workspace = true` declaration (MIT).

Vendored crates, copied from the pinned commit with the local changes listed
below:

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

## Local changes

- `rust/Cargo.toml`: workspace `members` reduced to the vendored set.
  `rust/Cargo.lock` remains upstream's lock, unmodified.
- `rust/crates/platform-linux/src/wayland/hyprland.rs`: deserialize Hyprland
  0.57 workspace addresses, retaining legacy numeric IDs. Addresses take
  precedence; display names never identify workspaces. Explicit empty
  workspaces do not contribute to visibility; missing required identity is
  an error. Existing window identity, PID, geometry, hidden and DPMS rules
  remain in place.
- `rust/crates/platform-linux/src/wayland/mod.rs`: add a fallible window
  dispatcher for authoritative Hyprland/niri IPC errors. Keep the existing
  best-effort dispatcher for other callers and the existing fallback order
  for other compositors and X11.
- `rust/crates/platform-linux/src/tools/list_windows.rs`: use the fallible
  dispatcher and return MCP tool errors for enumeration or blocking-task
  failure. A successful empty enumeration still returns an empty list.

The workspace compatibility change follows the JSON emitted by Hyprland
`v0.57.0` (`0aa6745f705f2c4b0a5004eb019ef76e5bbdb426`,
`src/ipc/s1/Commands.cpp`); no Hyprland implementation code is copied.
Cua main at `fc5363878a94d84f1e5ea8fcad045c0b9ada4b59` still had the
pinned Hyprland parser, so these are local patches, not an upstream upgrade.
Their worker-level regressions live in `cua-worker/tests/wayland_windows.rs`
and run against isolated IPC fixtures through the real worker MCP endpoint.

Update procedure: re-copy the same seven crate directories, `rust/Skills/`
and the two manifests from a reviewed upstream commit. Re-apply the members
edit and the three Linux patches above unless the upstream update already
implements them; review that comparison explicitly. Run the worker tests
including `--tests`, worker lint/build and both notice-bundle checks. Keep
this provenance and the extension documentation aligned with the resulting
patch set; do not regenerate the upstream lock merely to run the trimmed
vendor workspace's tests.
