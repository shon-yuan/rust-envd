# Archived worker dependency graph

This directory preserves the retired standalone worker's manifest, lockfile,
third-party notices and license texts as historical attribution evidence.
The manifest is an archival copy: its relative source paths are historical,
and this directory is not a buildable package or a supported runtime.

The active native CUA implementation is part of envd and uses the root
`Cargo.toml`, `Cargo.lock` and `third_party/NOTICES.md`. Current notice
generation and binary provenance do not read or regenerate this archive.
See [the packaging policy](../PACKAGING.md) and
[the native CUA contract](../../docs/native-cua.md).
