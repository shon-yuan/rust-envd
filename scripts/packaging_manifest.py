#!/usr/bin/env python3
"""Emit the provenance manifest for a built rust-envd distribution.

Binds one built binary to the source revision, toolchain, canonical build
command, and every covered notice file (see third_party/PACKAGING.md). The
manifest is JSON on stdout, or written to --output.

python3 stdlib only. Fully offline except local git/cargo/rustc.

Usage (from anywhere; the repository root is the parent of scripts/):
    python3 scripts/packaging_manifest.py --binary target/release/coorworker-envd
    python3 scripts/packaging_manifest.py --binary ... --output dist/manifest.json

A dirty working tree is refused unless --allow-dirty is passed (the manifest
still records dirty=true in that case); distributable artifacts must be built
from a clean checkout.
"""

from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# Everything a covered distribution must ship, per PACKAGING.md, plus the
# files that pin the dependency graph the notices describe.
REQUIRED_FILES = (
    "LICENSE",
    "third_party/NOTICES.md",
    "third_party/e2b/LICENSE",
    "third_party/e2b/PROVENANCE",
    "vendor/connectrpc/LICENSE",
    "vendor/connectrpc/PATCHES.md",
    "vendor/hyper/LICENSE",
    "vendor/hyper/PATCHES.md",
    "Cargo.toml",
    "Cargo.lock",
)
LICENSES_DIR = "third_party/licenses"
MANIFEST_SCHEMA = 1


def fail(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)
    sys.exit(1)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def run(*args: str) -> str:
    return subprocess.run(
        args, cwd=str(REPO), check=True, capture_output=True, text=True
    ).stdout.strip()


def file_record(path: Path) -> dict:
    return {
        "path": path.relative_to(REPO).as_posix(),
        "size": path.stat().st_size,
        "sha256": sha256_file(path),
    }


def tree_record(root: Path) -> dict:
    """Deterministic digest over the sorted (relative path, content) pairs."""
    files = sorted(path for path in root.rglob("*") if path.is_file())
    digest = hashlib.sha256()
    total = 0
    for path in files:
        relative = path.relative_to(REPO).as_posix()
        content = path.read_bytes()
        digest.update(relative.encode("utf-8") + b"\0")
        digest.update(hashlib.sha256(content).hexdigest().encode("ascii") + b"\n")
        total += len(content)
    return {
        "path": root.relative_to(REPO).as_posix(),
        "file_count": len(files),
        "size": total,
        "sha256": digest.hexdigest(),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, help="built envd binary")
    parser.add_argument("--output", help="write the manifest here (default: stdout)")
    parser.add_argument(
        "--target", help="rustc target triple the binary was built for"
    )
    parser.add_argument(
        "--allow-dirty",
        action="store_true",
        help="permit a dirty tree (recorded as dirty=true; not distributable)",
    )
    args = parser.parse_args()

    try:
        binary = Path(args.binary).resolve(strict=True)
    except OSError as exc:
        fail(f"cannot read --binary: {exc}")
    if not binary.is_file():
        fail(f"--binary is not a file: {binary}")

    missing = [name for name in REQUIRED_FILES if not (REPO / name).is_file()]
    if not (REPO / LICENSES_DIR).is_dir():
        missing.append(LICENSES_DIR + "/")
    if missing:
        fail("missing covered file(s): " + ", ".join(missing))

    porcelain = run("git", "status", "--porcelain")
    dirty = bool(porcelain)
    if dirty and not args.allow_dirty:
        fail(
            "working tree is dirty; commit first or pass --allow-dirty "
            "(the manifest will record dirty=true)"
        )

    notice_check = subprocess.run(
        [sys.executable, "-I", "-B", str(REPO / "scripts/generate_notices.py"), "--check"],
        cwd=str(REPO),
        capture_output=True,
        text=True,
    )
    if notice_check.returncode != 0:
        fail(
            "notice bundle does not match the locked graph; run "
            "python3 scripts/generate_notices.py\n" + notice_check.stderr.strip()
        )

    rustc = run("rustc", "-vV").splitlines()
    manifest = {
        "schema": MANIFEST_SCHEMA,
        "generated_at": datetime.datetime.now(datetime.timezone.utc)
        .replace(microsecond=0)
        .isoformat()
        .replace("+00:00", "Z"),
        "source": {
            "commit": run("git", "rev-parse", "HEAD"),
            "dirty": dirty,
            "dirty_entry_count": len(porcelain.splitlines()),
        },
        "toolchain": {
            "cargo": run("cargo", "--version"),
            "rustc": rustc[0] if rustc else "",
        },
        "target": args.target,
        "build_command": "cargo build --locked -j4 --release"
        + (f" --target {args.target}" if args.target else ""),
        "binary": file_record(binary),
        "covered_files": [file_record(REPO / name) for name in REQUIRED_FILES],
        "license_tree": tree_record(REPO / LICENSES_DIR),
        "notice_check": "ok",
    }

    text = json.dumps(manifest, indent=2, sort_keys=True) + "\n"
    if args.output:
        output = Path(args.output)
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(text)
        print(f"Wrote {output} ({len(text)} bytes)", file=sys.stderr)
    else:
        print(text, end="")
    return 0


if __name__ == "__main__":
    sys.exit(main())
