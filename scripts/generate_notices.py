#!/usr/bin/env python3
"""Generate the distributable dependency notice bundle for rust-envd.

Enumerates every locked package in the dependency graph (via
`cargo metadata --locked --offline`, falling back to Cargo.lock plus the
local cargo registry when metadata is unavailable), copies each crate's
license files verbatim into third_party/licenses/<crate>-<version>/ and
writes third_party/NOTICES.md.

python3 stdlib only. Fully offline.

Usage (from anywhere; the repository root is the parent of scripts/):
    python3 scripts/generate_notices.py            # regenerate in place
    python3 scripts/generate_notices.py --check    # verify committed output
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tarfile
import tomllib
from pathlib import Path

REGISTRY_SRC = Path.home() / ".cargo" / "registry" / "src"
REGISTRY_CACHE = Path.home() / ".cargo" / "registry" / "cache"

# Top-level files that carry license/attribution text. Deliberately broad:
# anything matched is copied verbatim, so a false positive costs a byte copy,
# a false negative would be a missing notice.
LICENSE_FILE_RE = re.compile(
    r"^(LICEN[CS]E|UNLICEN[CS]E|COPY(ING|RIGHT)|NOTICE|AUTHORS)([.\-_ ].*)?$",
    re.IGNORECASE,
)
# A copyright-bearing line: "Copyright (c) ...", "Copyright 2009 ...", "(c) ...", "(c)...".
COPYRIGHT_RE = re.compile(
    r"(?i)copyright\s*(?:\(c\)|\xa9|\d{4})|\xa9|\(c\)\s*\d{4}"
)
# Apache-2.0 appendix placeholder; never a real holder statement.
PLACEHOLDER_RE = re.compile(
    r"(?i)copyright\s*(?:\(c\)\s*)?(?:\[yyyy\]|<year>|<copyright holders>)"
)

# Crates that declare a license expression but ship no license file in the
# published package get canonical text synthesized either from the dominant
# byte-identical copy in the local registry (Apache-2.0) or from the checked-in
# SPDX license texts under scripts/license-overrides/ (MIT, MPL-2.0, which
# embed no package-specific copyright line). Keyed by SPDX identifier; value
# is (pattern matching canonical source files, output name).
SYNTHETIC_LICENSE_SOURCES = {
    "Apache-2.0": (
        re.compile(r"(?i)license[.\-_ ]?apache"),
        "LICENSE-APACHE",
    ),
    "MIT": (None, "LICENSE-MIT"),
    "MPL-2.0": (None, "LICENSE-MPL"),
    "BSL-1.0": (None, "LICENSE-BSL"),
}

OVERRIDES_DIR = Path(__file__).resolve().parent / "license-overrides"

NOTICES_NAME = "NOTICES.md"
LICENSES_DIR = "licenses"


def fail(msg: str) -> None:
    print(f"error: {msg}", file=sys.stderr)


class Collector:
    """Accumulates per-crate failures so one run reports all of them."""

    def __init__(self) -> None:
        self.errors: list[str] = []

    def add(self, crate: str, msg: str) -> None:
        self.errors.append(f"{crate}: {msg}")

    @property
    def ok(self) -> bool:
        return not self.errors


# ---------------------------------------------------------------------------
# Package enumeration
# ---------------------------------------------------------------------------


def cargo_metadata_packages(repo: Path) -> tuple[list[dict], dict, str] | None:
    """Enumerate locked packages via cargo metadata. Returns None on failure."""
    try:
        proc = subprocess.run(
            ["cargo", "metadata", "--locked", "--offline", "--format-version", "1"],
            cwd=str(repo),
            capture_output=True,
            check=True,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    meta = json.loads(proc.stdout)
    root_id = (meta.get("resolve") or {}).get("root")
    pkgs: list[dict] = []
    root: dict = {}
    for p in meta["packages"]:
        entry = {
            "name": p["name"],
            "version": p["version"],
            "license": p.get("license") or None,
            "license_file": p.get("license_file") or None,
            "repository": p.get("repository") or p.get("homepage") or None,
            "manifest_dir": Path(p["manifest_path"]).parent,
        }
        if p["id"] == root_id:
            root = entry
        else:
            pkgs.append(entry)
    if not root:
        fail("cargo metadata returned no workspace root package")
        sys.exit(1)
    return pkgs, root, "cargo metadata --locked --offline"


def _toml_bytes_for(pkg: dict) -> bytes | None:
    src_dir = pkg.get("src_dir")
    if src_dir is not None:
        cand = src_dir / "Cargo.toml"
        if cand.is_file():
            return cand.read_bytes()
    tarball = pkg.get("tarball")
    if tarball is not None:
        top = f"{pkg['name']}-{pkg['version']}"
        with tarfile.open(tarball, "r:gz") as tf:
            for member in tf.getmembers():
                if member.name == f"{top}/Cargo.toml" and member.isfile():
                    fh = tf.extractfile(member)
                    if fh is not None:
                        return fh.read()
    return None


def lockfile_packages(repo: Path) -> tuple[list[dict], dict, str]:
    """Fallback: enumerate Cargo.lock via tomllib + the local cargo registry."""
    lock = repo / "Cargo.lock"
    try:
        with open(lock, "rb") as fh:
            data = tomllib.load(fh)
    except (OSError, tomllib.TOMLDecodeError) as exc:
        fail(f"cannot parse {lock}: {exc}")
        sys.exit(1)

    root_toml = repo / "Cargo.toml"
    try:
        with open(root_toml, "rb") as fh:
            root_manifest = tomllib.load(fh)
    except (OSError, tomllib.TOMLDecodeError) as exc:
        fail(f"cannot parse {root_toml}: {exc}")
        sys.exit(1)
    root = {
        "name": root_manifest.get("package", {}).get("name", "(unknown)"),
        "version": str(root_manifest.get("package", {}).get("version", "?")),
        "license": root_manifest.get("package", {}).get("license"),
        "license_file": root_manifest.get("package", {}).get("license_file"),
        "repository": root_manifest.get("package", {}).get("repository"),
        "src_dir": repo,
        "tarball": None,
    }

    pkgs: list[dict] = []
    for p in data.get("package", []):
        if not p.get("source"):
            continue  # workspace member(s), already covered by root
        name, version = p["name"], p["version"]
        pkgs.append(
            {
                "name": name,
                "version": version,
                "license": None,
                "license_file": None,
                "repository": None,
                "manifest_dir": None,
                "src_dir": registry_src_dir(name, version),
                "tarball": registry_cache_crate(name, version),
            }
        )
    return pkgs, root, "Cargo.lock + local cargo registry (cargo metadata unavailable)"


def fill_license_fields(pkgs: list[dict]) -> None:
    """In lockfile mode, read license/license_file/repository from each
    crate's Cargo.toml (extracted source dir, else cache tarball)."""
    for pkg in pkgs:
        if pkg["license"] or pkg["license_file"]:
            continue
        raw = _toml_bytes_for(pkg)
        if raw is None:
            continue
        try:
            manifest = tomllib.loads(raw.decode("utf-8"))
        except (tomllib.TOMLDecodeError, UnicodeDecodeError):
            continue
        section = manifest.get("package", {})
        pkg["license"] = section.get("license")
        pkg["license_file"] = section.get("license_file")
        pkg["repository"] = section.get("repository") or section.get("homepage")


def registry_src_dir(name: str, version: str) -> Path | None:
    if not REGISTRY_SRC.is_dir():
        return None
    for reg in sorted(REGISTRY_SRC.iterdir()):
        cand = reg / f"{name}-{version}"
        if cand.is_dir():
            return cand
    return None


def registry_cache_crate(name: str, version: str) -> Path | None:
    if not REGISTRY_CACHE.is_dir():
        return None
    for reg in sorted(REGISTRY_CACHE.iterdir()):
        cand = reg / f"{name}-{version}.crate"
        if cand.is_file():
            return cand
    return None


def resolve_src_dir(pkg: dict) -> Path | None:
    src_dir = pkg.get("src_dir")
    if src_dir is not None and src_dir.is_dir():
        return src_dir
    manifest_dir = pkg.get("manifest_dir")
    if manifest_dir is not None and manifest_dir.is_dir():
        return manifest_dir
    return registry_src_dir(pkg["name"], pkg["version"])


# ---------------------------------------------------------------------------
# License file collection
# ---------------------------------------------------------------------------


def collect_basenames(pkg: dict, src_dir: Path | None) -> list[str]:
    names: set[str] = set()
    if src_dir is not None:
        for entry in sorted(src_dir.iterdir()):
            if entry.is_file() and LICENSE_FILE_RE.match(entry.name):
                names.add(entry.name)
    declared = pkg.get("license_file")
    if declared and src_dir is not None:
        base = Path(declared).name
        if (src_dir / base).is_file():
            names.add(base)
    return sorted(names, key=str.lower)


def read_crate_file(pkg: dict, src_dir: Path | None, basename: str) -> bytes | None:
    if src_dir is not None:
        cand = src_dir / basename
        if cand.is_file():
            return cand.read_bytes()
    tarball = pkg.get("tarball")
    if tarball is not None:
        top = f"{pkg['name']}-{pkg['version']}"
        with tarfile.open(tarball, "r:gz") as tf:
            for member in tf.getmembers():
                if member.name == f"{top}/{basename}" and member.isfile():
                    fh = tf.extractfile(member)
                    if fh is not None:
                        return fh.read()
    return None


def decode(raw: bytes) -> str:
    try:
        return raw.decode("utf-8")
    except UnicodeDecodeError:
        return raw.decode("latin-1")


# ---------------------------------------------------------------------------
# Copyright extraction
# ---------------------------------------------------------------------------


def copyright_block(text: str) -> tuple[list[str], int]:
    """First contiguous run of copyright-bearing lines, capped at 3, plus the
    count of further lines in the same run."""
    hits: list[str] = []
    extra = 0
    started = False
    for line in text.splitlines()[:80]:
        if PLACEHOLDER_RE.search(line):
            continue
        if COPYRIGHT_RE.search(line):
            started = True
            if len(hits) < 3:
                hits.append(line.strip())
            else:
                extra += 1
        elif started:
            break
    return hits, extra


# ---------------------------------------------------------------------------
# Bundle generation
# ---------------------------------------------------------------------------


def build_canonical_index(pkgs: list[dict]) -> dict[str, tuple[bytes, str]]:
    """For each synthetic-source identifier, find the dominant byte-identical
    license text across the registry set. Returns {identifier: (bytes, origin)}."""
    groups: dict[str, dict[bytes, list[str]]] = {}
    for pkg in sorted(pkgs, key=lambda p: (p["name"].lower(), p["version"])):
        src_dir = resolve_src_dir(pkg)
        if src_dir is None:
            continue
        try:
            tops = sorted(src_dir.iterdir())
        except OSError:
            continue
        for entry in tops:
            if not entry.is_file():
                continue
            for ident, (pat, _out) in SYNTHETIC_LICENSE_SOURCES.items():
                if pat is not None and pat.match(entry.name):
                    origin = f"{pkg['name']}-{pkg['version']}/{entry.name}"
                    groups.setdefault(ident, {}).setdefault(
                        entry.read_bytes(), []
                    ).append(origin)
    index: dict[str, tuple[bytes, str]] = {}
    for ident, by_bytes in groups.items():
        ranked = sorted(
            by_bytes.items(),
            key=lambda kv: (-len(kv[1]), kv[1][0].lower()),
        )
        best_bytes, origins = ranked[0]
        index[ident] = (best_bytes, origins[0])
    return index


def synthesized_license(
    repo: Path, pkg: dict, canonical: dict[str, tuple[bytes, str]]
) -> tuple[str, bytes, str] | None:
    """Text for a crate whose published package ships no license file.

    The vendored Cua Driver crates reuse the upstream repository's MIT text,
    which covers them and names the actual holder. Other crates fall back to
    the dominant registry copy (Apache-2.0) or the checked-in SPDX text
    (MIT, MPL-2.0), which carry no holder of their own.
    """
    manifest_dir = pkg.get("manifest_dir")
    if manifest_dir is not None:
        try:
            manifest_dir.resolve().relative_to((repo / "vendor" / "cua-driver").resolve())
        except ValueError:
            pass
        else:
            upstream = repo / "vendor" / "cua-driver" / "LICENSE"
            if upstream.is_file():
                return (
                    "LICENSE",
                    upstream.read_bytes(),
                    "upstream repository license, reproduced byte-for-byte;"
                    " the published crate ships no per-crate file (see"
                    " vendor/cua-driver/PROVENANCE.md)",
                )
    if not pkg["license"]:
        return None
    for ident, (_pat, out_name) in SYNTHETIC_LICENSE_SOURCES.items():
        if ident not in pkg["license"]:
            continue
        if ident in canonical:
            payload, origin = canonical[ident]
            return (
                out_name,
                payload,
                f"canonical {ident} text; the crate package ships no license"
                f" file — reproduced byte-for-byte from {origin} in the local"
                " registry",
            )
        override = OVERRIDES_DIR / f"{ident}.txt"
        if override.is_file():
            return (
                out_name,
                override.read_bytes(),
                f"canonical {ident} text from the SPDX license list"
                f" (scripts/license-overrides/{override.name}); the crate"
                " package ships no license file and embeds no copyright notice",
            )
    return None


def generate(
    package_dir: Path,
    repo: Path,
    *,
    title: str,
    scope: str,
    root_label: str,
    license_link: str,
    vendored_kind: str = "patch",
) -> tuple[str, dict[str, bytes], int, int]:
    """Return (NOTICES.md text, {relative path under the bundle root: bytes},
    total package count, third-party crate count).

    ``package_dir`` is the crate whose dependency graph is enumerated (the
    repository root, or a nested package such as ``cua-worker`` with its own
    lockfile). ``repo`` stays the repository root for shared license and
    provenance files.
    """
    enumerated = cargo_metadata_packages(package_dir)
    if enumerated is None:
        enumerated = lockfile_packages(package_dir)
    pkgs, root, method = enumerated
    if method.startswith("Cargo.lock"):
        fill_license_fields(pkgs)

    collector = Collector()
    canonical = build_canonical_index(pkgs)
    collected = []
    for pkg in sorted(pkgs, key=lambda p: (p["name"].lower(), p["version"])):
        crate = f"{pkg['name']}-{pkg['version']}"
        if not pkg["license"] and not pkg["license_file"]:
            collector.add(
                crate,
                "neither a license expression nor a license-file is declared",
            )
            continue
        src_dir = resolve_src_dir(pkg)
        basenames = collect_basenames(pkg, src_dir)
        synthetic: dict[str, str] = {}
        synthetic_payloads: dict[str, bytes] = {}
        if not basenames:
            synthesized = synthesized_license(repo, pkg, canonical)
            if synthesized is not None:
                out_name, payload, note = synthesized
                synthetic_payloads[out_name] = payload
                synthetic[out_name] = note
        if not basenames and not synthetic_payloads:
            where = str(src_dir) if src_dir else "registry source/cache"
            collector.add(
                crate,
                f"no license files found in {where} (declared: "
                f"license={pkg['license']!r}, license_file={pkg['license_file']!r})",
            )
            continue
        files: dict[str, bytes] = {}
        missing = False
        for basename in basenames:
            raw = read_crate_file(pkg, src_dir, basename)
            if raw is None:
                collector.add(crate, f"cannot read license file {basename!r}")
                missing = True
                continue
            files[basename] = raw
        if missing:
            continue
        files.update(synthetic_payloads)
        collected.append((pkg, files, synthetic))

    if not collector.ok:
        for err in collector.errors:
            fail(err)
        fail(f"{len(collector.errors)} crate(s) failed; bundle not written")
        sys.exit(1)

    text = render_notices(
        repo,
        root,
        method,
        collected,
        len(pkgs) + 1,
        title=title,
        scope=scope,
        root_label=root_label,
        license_link=license_link,
        vendored_kind=vendored_kind,
    )
    tree: dict[str, bytes] = {NOTICES_NAME: text.encode("utf-8")}
    for pkg, files, _synthetic in collected:
        prefix = f"{LICENSES_DIR}/{pkg['name']}-{pkg['version']}"
        for basename, raw in files.items():
            tree[f"{prefix}/{basename}"] = raw
    return text, tree, len(pkgs) + 1, len(collected)


def vendored_forks(repo: Path, collected: list[tuple[dict, dict, dict]]) -> list[tuple[str, dict]]:
    """Packages resolved from an in-tree path (not crates.io)."""
    root = repo.resolve()
    forks: list[tuple[str, dict]] = []
    for pkg, _files, _synthetic in collected:
        directory = pkg.get("manifest_dir")
        if directory is None:
            continue
        try:
            relative = directory.resolve().relative_to(root)
        except ValueError:
            continue
        forks.append((relative.as_posix(), pkg))
    return forks


def render_notices(
    repo: Path,
    root: dict,
    method: str,
    collected: list[tuple[dict, dict[str, bytes], dict[str, str]]],
    total: int,
    *,
    title: str,
    scope: str,
    root_label: str,
    license_link: str,
    vendored_kind: str,
) -> str:
    own_copyright = ""
    lic = repo / "LICENSE"
    if lic.is_file():
        own_copyright = (copyright_block(decode(lic.read_bytes()))[0] or [""])[0]

    lines: list[str] = []
    add = lines.append
    add(f"# {title}")
    add("")
    add(
        "This file and the `licenses/` directory beside it constitute the"
        " distributable dependency notice bundle for this project."
    )
    add("")
    add("## Scope")
    add("")
    add(scope)
    add("")
    add(
        f"Enumerated from the pinned dependency graph via {method}:"
        f" {total} packages total including this crate; the"
        f" {len(collected)} third-party crates indexed below carry the"
        " remaining linked-license obligations. Regenerate with"
        " `python3 scripts/generate_notices.py`; verify a checkout with"
        " `python3 scripts/generate_notices.py --check`."
    )
    add("")
    add("## This project (MIT)")
    add("")
    add(
        f"{root_label} (crate `{root['name']}` {root['version']}) is licensed"
        " under the MIT License; see"
        f" [`LICENSE`]({license_link}) at the repository root."
    )
    add("")
    if own_copyright:
        add(f"{own_copyright}")
        add("")
    if vendored_kind != "cua":
        add("## Vendored upstream schemas (Apache-2.0)")
        add("")
        add(
            "`third_party/e2b/` contains e2b `envd` protobuf schemas"
            " redistributed byte-for-byte from https://github.com/e2b-dev/runtime"
            " (release 0.10.2, commit"
            " 51083a83cc0903185d992725ea6bb33683814b29) under the Apache License,"
            " 2.0. Any covered distribution must include"
            " `third_party/e2b/LICENSE` and `third_party/e2b/PROVENANCE`;"
            " generated code built from those schemas carries the same"
            " attribution and does not replace those files."
        )
        add("")
        add("## Go-derived source (BSD-3-Clause)")
        add("")
        add(
            "`src/platform/linux/mmds/go_url.rs` is a Go-derived redirect URL helper carrying"
            ' The Go Authors\' BSD-3-Clause notice ("Copyright 2009 The Go'
            ' Authors.") in its file header. Binary redistribution of a covered'
            " distribution must reproduce that notice; source distributions"
            " include the file itself."
        )
        add("")
    forks = vendored_forks(repo, collected)
    if vendored_kind == "cua":
        add("## Vendored Cua Driver crates")
        add("")
        add(
            "`vendor/cua-driver/` carries the MIT-licensed Cua Driver crates"
            " used by the optional `coorworker-cua` worker, copied"
            " byte-for-byte from a pinned upstream commit. Provenance, the"
            " vendored list, and the deliberately excluded AGPL/FSL"
            " components are recorded in `vendor/cua-driver/PROVENANCE.md`."
            " Covered distributions must include that file and the license"
            " texts enumerated below."
        )
        add("")
        for relative, pkg in forks:
            crate = f"{pkg['name']}-{pkg['version']}"
            add(
                f"`{relative}/` is a vendored upstream crate `{pkg['name']}`"
                f" {pkg['version']} ({pkg['license'] or 'see files'}). Its"
                f" license text is under `{LICENSES_DIR}/{crate}/` and is"
                " also indexed below."
            )
            add("")
    else:
        add("## Vendored Rust crate forks")
        add("")
        if not forks:
            add(
                "No in-tree crate forks are resolved by the current lockfile; this"
                " section becomes active when a `[patch.crates-io]` path"
                " dependency is present."
            )
            add("")
        for relative, pkg in forks:
            crate = f"{pkg['name']}-{pkg['version']}"
            add(
                f"`{relative}/` is an in-tree fork of crates.io `{pkg['name']}`"
                f" {pkg['version']} ({pkg['license'] or 'see files'}), selected with"
                " `[patch.crates-io]`. Covered distributions must include this"
                f" file, the license text(s) under `{LICENSES_DIR}/{crate}/`,"
                f" and `{relative}/PATCHES.md`, which records the upstream"
                " revision and every local change. The fork's license text is"
                " enumerated below with the other third-party crates."
            )
            add("")
    add("## Third-party crates")
    add("")
    add(
        "Full license texts are copied verbatim under"
        " `licenses/<crate>-<version>/`. Copyright lines quoted per crate"
        " below are extracted from those files for convenience; where none"
        " is embedded in the license file(s), consult the full text."
    )
    add("")
    add("### Index")
    add("")
    name_w = max(len(p["name"]) for p, _, _ in collected)
    ver_w = max(len(p["version"]) for p, _, _ in collected)
    lic_w = max(
        [len(p["license"] or f"license-file: {p['license_file']}") for p, _, _ in collected]
        + [len("License")]
    )
    add(
        f"| Crate{' ' * (name_w - 5)} | Version{' ' * (ver_w - 7)} |"
        f" License{' ' * (lic_w - 7)} | Files copied |"
    )
    add(f"|{'-' * (name_w + 2)}|{'-' * (ver_w + 2)}|{'-' * (lic_w + 2)}|---|")
    for pkg, files, _synthetic in collected:
        lic_expr = pkg["license"] or f"license-file: {pkg['license_file']}"
        files_str = ", ".join(sorted(files, key=str.lower))
        add(
            f"| {pkg['name'].ljust(name_w)} | {pkg['version'].ljust(ver_w)} |"
            f" {lic_expr.ljust(lic_w)} | {files_str} |"
        )
    add("")
    add("### Per-crate notices")
    add("")
    for pkg, files, synthetic in collected:
        crate = f"{pkg['name']}-{pkg['version']}"
        lic_expr = pkg["license"] or f"license-file: {pkg['license_file']}"
        add(f"#### {pkg['name']} {pkg['version']} — {lic_expr}")
        add("")
        file_list = ", ".join(
            f"licenses/{crate}/{b}" for b in sorted(files, key=str.lower)
        )
        add(f"- Files: {file_list}")
        for basename in sorted(synthetic, key=str.lower):
            add(f"- License text: {synthetic[basename]}")
        shown = 0
        for basename in sorted(files, key=str.lower):
            if shown >= 2:
                break
            hits, extra = copyright_block(decode(files[basename]))
            if not hits:
                continue
            for hit in hits:
                add(f"- Copyright ({basename}): {hit}")
            if extra:
                add(
                    f"- ... (+{extra} more copyright line(s) in {basename};"
                    " see full text)"
                )
            shown += 1
        if shown == 0:
            add(
                "- Copyright: none embedded in the license file(s); see full"
                " text"
            )
        if pkg.get("repository"):
            add(f"- Repository: {pkg['repository']}")
        add("")
    add("---")
    add("")
    add(
        "Bundle generated by `scripts/generate_notices.py` from the locked"
        " dependency graph. Do not edit by hand; regenerate instead."
    )
    add("")
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Output / verification
# ---------------------------------------------------------------------------


def snapshot(root: Path) -> dict[str, bytes]:
    snap: dict[str, bytes] = {}
    if not root.is_dir():
        return snap
    for path in sorted(root.rglob("*")):
        if path.is_file():
            snap[str(path.relative_to(root))] = path.read_bytes()
    return snap


def write_bundle(third_party: Path, tree: dict[str, bytes]) -> None:
    licenses_root = third_party / LICENSES_DIR
    licenses_root.mkdir(parents=True, exist_ok=True)
    keep_files = {third_party / rel for rel in tree if rel != NOTICES_NAME}
    for rel, payload in sorted(tree.items()):
        dest = third_party / rel
        dest.parent.mkdir(parents=True, exist_ok=True)
        if not dest.exists() or dest.read_bytes() != payload:
            dest.write_bytes(payload)
    # Prune files/dirs no longer part of the bundle.
    for path in sorted(licenses_root.rglob("*"), reverse=True):
        if path.is_file() and path not in keep_files:
            path.unlink()
    for path in sorted(licenses_root.rglob("*"), reverse=True):
        if path.is_dir() and not any(path.iterdir()):
            path.rmdir()
    notices = third_party / NOTICES_NAME
    notices.write_bytes(tree[NOTICES_NAME])


def committed_bundle(base: Path) -> dict[str, bytes]:
    """Snapshot of the committed bundle only: NOTICES.md + licenses/ tree."""
    committed: dict[str, bytes] = {}
    notices = base / NOTICES_NAME
    if notices.is_file():
        committed[NOTICES_NAME] = notices.read_bytes()
    for rel, payload in snapshot(base / LICENSES_DIR).items():
        committed[f"{LICENSES_DIR}/{rel}"] = payload
    return committed


def check_bundle(base: Path, tree: dict[str, bytes], label: str) -> int:
    committed = committed_bundle(base)
    if committed == tree:
        print(
            f"OK: {label}/{NOTICES_NAME} and {label}/{LICENSES_DIR}/"
            f" match the locked dependency graph ({len(tree) - 1} license files)."
        )
        return 0
    only_committed = sorted(set(committed) - set(tree))
    only_new = sorted(set(tree) - set(committed))
    changed = sorted(
        k for k in set(committed) & set(tree) if committed[k] != tree[k]
    )
    print(f"MISMATCH between committed {label}/ and regenerated bundle:", file=sys.stderr)
    for description, paths in (
        ("committed but not in lockfile bundle", only_committed),
        ("in lockfile bundle but not committed", only_new),
        ("content differs", changed),
    ):
        for rel in paths[:20]:
            print(f"  {description}: {rel}", file=sys.stderr)
        if len(paths) > 20:
            print(f"  {description}: ... and {len(paths) - 20} more", file=sys.stderr)
    print("Run `python3 scripts/generate_notices.py` to refresh the bundle.", file=sys.stderr)
    return 1


MAIN_SCOPE = (
    "These notices cover every distribution built from this repository:"
    " the linked `envd` daemon binaries produced from this crate, and any"
    " guest or sandbox image that embeds such a binary. Distributing a"
    " covered distribution requires reproducing this file, the"
    " `licenses/` tree, and the files called out in"
    " [`PACKAGING.md`](PACKAGING.md)."
)
WORKER_SCOPE = (
    "These notices cover the `coorworker-cua` worker binary built from"
    " `cua-worker/`, a separate package with its own lockfile that rust-envd"
    " supervises but does not link. Distributing the worker requires"
    " reproducing this file and the `licenses/` tree beside it; the daemon's"
    " bundle in `third_party/` is separate. Packaging steps are specified in"
    " [`../PACKAGING.md`](../PACKAGING.md)."
)


def bundles(repo: Path) -> list[tuple[str, Path, tuple[str, dict[str, bytes], int, int]]]:
    """The notice bundles this repository ships: the daemon's and the
    worker's. Each is generated from its own locked dependency graph."""
    return [
        (
            "third_party",
            repo / "third_party",
            generate(
                repo,
                repo,
                title="Third-Party Notices — rust-envd",
                scope=MAIN_SCOPE,
                root_label="rust-envd",
                license_link="../LICENSE",
            ),
        ),
        (
            "third_party/cua-worker",
            repo / "third_party" / "cua-worker",
            generate(
                repo / "cua-worker",
                repo,
                title="Third-Party Notices — coorworker-cua",
                scope=WORKER_SCOPE,
                root_label="coorworker-cua",
                license_link="../../LICENSE",
                vendored_kind="cua",
            ),
        ),
    ]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--check",
        action="store_true",
        help="regenerate and exit nonzero if a committed bundle differs",
    )
    args = parser.parse_args()
    repo = Path(__file__).resolve().parent.parent

    if args.check:
        failed = 0
        for label, base, (_text, tree, _total, _ncrates) in bundles(repo):
            failed |= check_bundle(base, tree, label)
        return 0 if failed == 0 else 1

    for label, base, (text, tree, total, ncrates) in bundles(repo):
        write_bundle(base, tree)
        print(
            f"Wrote {label}/{NOTICES_NAME} ({len(text.encode('utf-8'))} bytes)"
            f" and {len(tree) - 1} license files for {ncrates} crates"
            f" ({total} packages in the locked graph, root included)."
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
