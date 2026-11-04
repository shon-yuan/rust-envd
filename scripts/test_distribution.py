#!/usr/bin/env python3
"""Distribution regressions; stdlib only, with every subprocess mocked.

Run: python3 -I -B -m unittest discover -s scripts -p test_distribution.py -v
"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


def load_script(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


notices = load_script("generate_notices")
packaging = load_script("packaging_manifest")


class NoticeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        (self.repo / "LICENSE").write_text("MIT\nCopyright 2026 Project contributors\n")
        self.packages = []
        self.package("coorworker-envd", self.repo)
        self.package("coorworker-cua", self.repo / "cua-worker", license_file=False)
        self.package("hyper", self.repo / "vendor/hyper")
        self.package(
            "cua-driver-core", self.repo / "vendor/cua-driver/rust/crates/cua-driver-core",
            license_file=False,
        )
        (self.repo / "vendor/cua-driver/LICENSE").write_text("MIT\nCopyright 2026 Cua contributors\n")

    def package(self, name, directory, license_file=True):
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "Cargo.toml").write_text(
            f'[package]\nname = "{name}"\nversion = "0.1.0"\nlicense = "MIT"\n'
        )
        if license_file:
            (directory / "LICENSE").write_text(f"MIT\nCopyright 2026 {name} contributors\n")
        self.packages.append({
            "id": name, "name": name, "version": "0.1.0", "license": "MIT",
            "manifest_path": str(directory / "Cargo.toml"), "source": None,
        })

    def metadata(self, root):
        return {"packages": self.packages, "resolve": {"root": root}}

    def test_metadata_requests_all_features_for_distribution_superset(self):
        with patch.object(notices.subprocess, "run") as run:
            run.return_value.stdout = json.dumps(self.metadata("coorworker-envd")).encode()
            _packages, _root, method = notices.cargo_metadata_packages(self.repo)
        self.assertEqual(run.call_args.args[0], [
            "cargo", "metadata", "--locked", "--offline", "--all-features",
            "--format-version", "1",
        ])
        self.assertIn("--all-features", method)

    def test_root_bundle_distinguishes_project_library_patches_and_cua_vendor(self):
        with patch.object(notices.subprocess, "run") as run:
            run.return_value.stdout = json.dumps(self.metadata("coorworker-envd")).encode()
            text, tree, total, third_party = notices.generate(
                self.repo, self.repo, title="Notices", scope=notices.MAIN_SCOPE,
                root_label="rust-envd", license_link="../LICENSE",
            )
        self.assertEqual((total, third_party), (4, 2))
        self.assertIn("`coorworker-cua`", text.split("## Third-party crates")[0])
        self.assertNotIn("licenses/coorworker-cua-0.1.0/LICENSE-MIT", tree)
        self.assertIn("vendor/hyper/PATCHES.md", text)
        self.assertIn("vendor/cua-driver/PROVENANCE.md", text)
        self.assertNotIn("cua-driver-core/PATCHES.md", text)
        self.assertNotIn("cua-worker/PATCHES.md", text)
        self.assertEqual(tree["licenses/cua-driver-core-0.1.0/LICENSE"],
                         (self.repo / "vendor/cua-driver/LICENSE").read_bytes())
        self.assertIn("cua-desktop", text)
        self.assertIn("AGPL/FSL", text)

    def test_other_in_repo_paths_keep_their_own_license_notices(self):
        self.package("upstream-helper", self.repo / "third_party/upstream-helper")
        with patch.object(notices.subprocess, "run") as run:
            run.return_value.stdout = json.dumps(self.metadata("coorworker-envd")).encode()
            _text, tree, total, third_party = notices.generate(
                self.repo, self.repo, title="Notices", scope=notices.MAIN_SCOPE,
                root_label="rust-envd", license_link="../LICENSE",
            )
        self.assertEqual((total, third_party), (5, 3))
        self.assertIn("licenses/upstream-helper-0.1.0/LICENSE", tree)

    def test_worker_bundle_keeps_own_graph_and_allows_embedded_library(self):
        self.packages = [pkg for pkg in self.packages if pkg["name"] != "coorworker-envd"]
        with patch.object(notices.subprocess, "run") as run:
            run.return_value.stdout = json.dumps(self.metadata("coorworker-cua")).encode()
            text, tree, _total, _third_party = notices.generate(
                self.repo / "cua-worker", self.repo, title="Worker notices",
                scope=notices.WORKER_SCOPE, root_label="coorworker-cua",
                license_link="../../LICENSE", vendored_kind="cua",
            )
        self.assertEqual(run.call_args.kwargs["cwd"], str(self.repo / "cua-worker"))
        self.assertIn("licenses/cua-driver-core-0.1.0/LICENSE", tree)
        self.assertNotIn("supervises but does not link", text)
        self.assertIn("cua-desktop", text)
        self.assertNotIn("copied byte-for-byte from a pinned upstream commit", text)

    def test_restricted_licenses_abort_instead_of_using_vendor_mit_text(self):
        for index in (1, -1):
            for expression in ("AGPL-3.0-only", "FSL-1.1-MIT"):
                with self.subTest(package=self.packages[index]["name"], license=expression):
                    original = self.packages[index]["license"]
                    self.packages[index]["license"] = expression
                    try:
                        with patch.object(notices.subprocess, "run") as run:
                            run.return_value.stdout = json.dumps(self.metadata("coorworker-envd")).encode()
                            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as raised:
                                notices.generate(
                                    self.repo, self.repo, title="Notices", scope=notices.MAIN_SCOPE,
                                    root_label="rust-envd", license_link="../LICENSE",
                                )
                        self.assertEqual(raised.exception.code, 1)
                    finally:
                        self.packages[index]["license"] = original

    def test_unresolved_locked_path_package_fails_closed(self):
        (self.repo / "Cargo.lock").write_text(
            '[[package]]\nname = "missing-vendor"\nversion = "0.1.0"\n'
        )
        with patch.object(notices.subprocess, "run", side_effect=FileNotFoundError):
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as raised:
                notices.generate(
                    self.repo, self.repo, title="Notices", scope=notices.MAIN_SCOPE,
                    root_label="rust-envd", license_link="../LICENSE",
                )
        self.assertEqual(raised.exception.code, 1)

    def test_lockfile_fallback_retains_path_packages_and_inherited_licenses(self):
        # Optional and patched dependencies are both source-less in Cargo.lock.
        manifest = self.repo / "Cargo.toml"
        with manifest.open("a") as handle:
            handle.write('\n[dependencies]\ncoorworker-cua = { path = "cua-worker", optional = true }\n'
                         '\n[patch.crates-io]\nhyper = { path = "vendor/hyper" }\n')
        with (self.repo / "cua-worker/Cargo.toml").open("a") as handle:
            handle.write('\n[dependencies]\ncua-driver-core = { path = "../vendor/cua-driver/rust/crates/cua-driver-core" }\n')
        vendor = self.repo / "vendor/cua-driver/rust"
        (vendor / "Cargo.toml").write_text('[workspace.package]\nlicense = "MIT"\n')
        (vendor / "crates/cua-driver-core/Cargo.toml").write_text(
            '[package]\nname = "cua-driver-core"\nversion = "0.1.0"\nlicense.workspace = true\n'
        )
        (self.repo / "Cargo.lock").write_text("\n".join(
            f'[[package]]\nname = "{pkg["name"]}"\nversion = "0.1.0"\n'
            for pkg in self.packages
        ))
        with patch.object(notices.subprocess, "run", side_effect=FileNotFoundError):
            text, tree, total, third_party = notices.generate(
                self.repo, self.repo, title="Notices", scope=notices.MAIN_SCOPE,
                root_label="rust-envd", license_link="../LICENSE",
            )
        self.assertEqual((total, third_party), (4, 2))
        self.assertIn("licenses/hyper-0.1.0/LICENSE", tree)
        self.assertIn("licenses/cua-driver-core-0.1.0/LICENSE", tree)
        self.assertIn("vendor/cua-driver/PROVENANCE.md", text)


class PackagingTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        for name in packaging.REQUIRED_FILES:
            path = self.repo / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("fixture\n")
        for name in (packaging.LICENSES_DIR, packaging.WORKER_LICENSES_DIR):
            path = self.repo / name / "fixture/LICENSE"
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("MIT\n")
        self.binary = self.repo / "coorworker-envd"
        self.binary.write_bytes(b"test artifact")

    def invoke(self, *arguments):
        def subprocess_result(command, **kwargs):
            command = tuple(command)
            outputs = {
                ("git", "status", "--porcelain"): "",
                ("git", "rev-parse", "HEAD"): "a" * 40,
                ("rustc", "-vV"): "rustc test\nhost: x86_64-unknown-linux-gnu\n",
                ("cargo", "--version"): "cargo test\n",
            }
            if command in outputs:
                return subprocess.CompletedProcess(command, 0, stdout=outputs[command])
            if command == (sys.executable, "-I", "-B", str(self.repo / "scripts/generate_notices.py"), "--check"):
                return subprocess.CompletedProcess(command, 0, stdout="ok", stderr="")
            self.fail(f"unexpected subprocess: {command!r}")

        stdout = io.StringIO()
        with patch.object(packaging, "REPO", self.repo), patch.object(
            packaging.subprocess, "run", side_effect=subprocess_result
        ), patch.object(sys, "argv", ["packaging_manifest.py", "--binary", str(self.binary), *arguments]), contextlib.redirect_stdout(stdout):
            self.assertEqual(packaging.main(), 0)
        return json.loads(stdout.getvalue())

    def test_default_base_provenance_preserves_desktop_free_command(self):
        manifest = self.invoke()
        self.assertEqual(manifest["artifact_kind"], "base")
        self.assertEqual(manifest["features"], [])
        self.assertIsNone(manifest["target"])
        self.assertEqual(manifest["build_command"], "cargo build --locked -j4 --release")
        self.assertEqual(manifest["notice_check"], "ok")
        self.assertEqual(manifest["binary"]["size"], 13)

    def test_explicit_base_records_static_target_without_desktop(self):
        manifest = self.invoke("--artifact-kind", "base", "--target", "x86_64-unknown-linux-musl")
        self.assertEqual(manifest["artifact_kind"], "base")
        self.assertEqual(manifest["features"], [])
        self.assertEqual(manifest["target"], "x86_64-unknown-linux-musl")
        self.assertEqual(manifest["build_command"],
                         "cargo build --locked -j4 --release --target x86_64-unknown-linux-musl")

    def test_desktop_records_feature_and_platform_build_command(self):
        for target in (None, "x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"):
            with self.subTest(target=target):
                args = ["--artifact-kind", "desktop"]
                if target:
                    args.extend(["--target", target])
                manifest = self.invoke(*args)
                self.assertEqual(manifest["artifact_kind"], "desktop")
                self.assertEqual(manifest["features"], ["cua-desktop"])
                self.assertEqual(manifest["target"], target)
                expected = "cargo build --locked -j4 --release --features cua-desktop"
                if target:
                    expected += " --target " + target
                self.assertEqual(manifest["build_command"], expected)

    def test_unknown_artifact_kind_is_rejected(self):
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as raised:
            self.invoke("--artifact-kind", "worker")
        self.assertEqual(raised.exception.code, 2)


if __name__ == "__main__":
    unittest.main()
