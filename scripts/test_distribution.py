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
        self.package("hyper", self.repo / "vendor/hyper")
        for name in ("cua-driver-core", "platform-linux", "platform-windows"):
            self.package(
                name, self.repo / "vendor/cua-driver/rust/crates" / name,
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

    def test_root_bundle_covers_direct_native_dependencies_and_vendor_patches(self):
        with patch.object(notices.subprocess, "run") as run:
            run.return_value.stdout = json.dumps(self.metadata("coorworker-envd")).encode()
            text, tree, total, third_party = notices.generate(
                self.repo, self.repo, title="Notices", scope=notices.MAIN_SCOPE,
                root_label="rust-envd", license_link="../LICENSE",
            )
        self.assertEqual((total, third_party), (5, 4))
        self.assertNotIn("coorworker-cua", text)
        self.assertNotIn("standalone", text)
        self.assertIn("native", text)
        self.assertIn("vendor/hyper/PATCHES.md", text)
        self.assertIn("vendor/cua-driver/PROVENANCE.md", text)
        self.assertNotIn("cua-driver-core/PATCHES.md", text)
        self.assertNotIn("cua-worker/PATCHES.md", text)
        for name in ("cua-driver-core", "platform-linux", "platform-windows"):
            self.assertEqual(tree[f"licenses/{name}-0.1.0/LICENSE"],
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
        self.assertEqual((total, third_party), (6, 5))
        self.assertIn("licenses/upstream-helper-0.1.0/LICENSE", tree)

    def test_missing_native_license_metadata_fails_closed(self):
        self.packages[-1] = {**self.packages[-1], "license": None}
        with patch.object(notices.subprocess, "run") as run:
            run.return_value.stdout = json.dumps(self.metadata("coorworker-envd")).encode()
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as raised:
                notices.generate(
                    self.repo, self.repo, title="Notices", scope=notices.MAIN_SCOPE,
                    root_label="rust-envd", license_link="../LICENSE",
                )
        self.assertEqual(raised.exception.code, 1)

    def test_missing_native_upstream_license_cannot_use_generic_mit_text(self):
        (self.repo / "vendor/cua-driver/LICENSE").unlink()
        with patch.object(notices.subprocess, "run") as run:
            run.return_value.stdout = json.dumps(self.metadata("coorworker-envd")).encode()
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as raised:
                notices.generate(
                    self.repo, self.repo, title="Notices", scope=notices.MAIN_SCOPE,
                    root_label="rust-envd", license_link="../LICENSE",
                )
        self.assertEqual(raised.exception.code, 1)

    def test_native_license_mismatch_cannot_use_upstream_mit_text(self):
        self.packages[-1] = {**self.packages[-1], "license": "Apache-2.0"}
        with patch.object(notices.subprocess, "run") as run:
            run.return_value.stdout = json.dumps(self.metadata("coorworker-envd")).encode()
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as raised:
                notices.generate(
                    self.repo, self.repo, title="Notices", scope=notices.MAIN_SCOPE,
                    root_label="rust-envd", license_link="../LICENSE",
                )
        self.assertEqual(raised.exception.code, 1)

    def test_generate_and_check_use_single_root_graph_preserving_archive_bytes(self):
        archive = self.repo / "third_party/cua-worker"
        archived = {
            "NOTICES.md": b"historical notices\r\n",
            "licenses/retired/LICENSE": b"MIT\r\nCopyright archived\n\x00",
            "Cargo.toml": b"historical manifest\n",
            "Cargo.lock": b"historical lock\n",
        }
        for relative, content in archived.items():
            path = archive / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)

        def root_metadata(command, **kwargs):
            self.assertEqual(Path(kwargs["cwd"]), self.repo)
            self.assertEqual(command[:2], ["cargo", "metadata"])
            return subprocess.CompletedProcess(
                command, 0, stdout=json.dumps(self.metadata("coorworker-envd")).encode(),
            )

        for arguments in ([], ["--check"]):
            with self.subTest(arguments=arguments), patch.object(
                notices, "__file__", str(self.repo / "scripts/generate_notices.py")
            ), patch.object(notices.subprocess, "run", side_effect=root_metadata), patch.object(
                sys, "argv", ["generate_notices.py", *arguments]
            ), contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(notices.main(), 0)
            self.assertEqual(notices.snapshot(archive), archived)
        self.assertTrue((self.repo / "third_party/NOTICES.md").is_file())

    def test_restricted_licenses_abort_instead_of_using_vendor_mit_text(self):
        for index in range(1, len(self.packages)):
            for expression in ("AGPL-3.0-only", "FSL-1.1-MIT", "MIT OR AGPL-3.0-only"):
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
            handle.write('\n[dependencies]\ncua-driver-core = { path = "vendor/cua-driver/rust/crates/cua-driver-core", optional = true }\n'
                         '\n[target.\'cfg(target_os = "linux")\'.dependencies]\n'
                         'platform-linux = { path = "vendor/cua-driver/rust/crates/platform-linux", optional = true }\n'
                         '\n[target.\'cfg(windows)\'.dependencies]\n'
                         'platform-windows = { path = "vendor/cua-driver/rust/crates/platform-windows", optional = true }\n'
                         '\n[patch.crates-io]\nhyper = { path = "vendor/hyper" }\n')
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
        self.assertEqual((total, third_party), (5, 4))
        self.assertIn("licenses/hyper-0.1.0/LICENSE", tree)
        for name in ("cua-driver-core", "platform-linux", "platform-windows"):
            self.assertEqual(tree[f"licenses/{name}-0.1.0/LICENSE"],
                             (self.repo / "vendor/cua-driver/LICENSE").read_bytes())
        self.assertIn("vendor/cua-driver/PROVENANCE.md", text)


class PackagingTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        # Only the active root graph belongs to the distribution bundle.
        # An archived graph is deliberately absent from this fixture.
        required = (
            "LICENSE", "third_party/NOTICES.md", "third_party/e2b/LICENSE",
            "third_party/e2b/PROVENANCE", "vendor/connectrpc/LICENSE",
            "vendor/connectrpc/PATCHES.md", "vendor/hyper/LICENSE",
            "vendor/hyper/PATCHES.md", "Cargo.toml", "Cargo.lock",
            "vendor/cua-driver/LICENSE", "vendor/cua-driver/PROVENANCE.md",
        )
        for name in required:
            path = self.repo / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("fixture\n")
        path = self.repo / "third_party/licenses/fixture/LICENSE"
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

    def test_default_desktop_provenance_matches_ordinary_build(self):
        manifest = self.invoke()
        self.assertEqual(manifest["artifact_kind"], "desktop")
        self.assertEqual(manifest["features"], ["cua-desktop"])
        self.assertIsNone(manifest["target"])
        self.assertEqual(manifest["build_command"], "cargo build --locked -j4 --release")
        self.assertEqual(manifest["notice_check"], "ok")
        self.assertEqual(manifest["binary"]["size"], 13)

    def test_explicit_base_suppresses_defaults_for_native_and_cross_targets(self):
        for target in (None, "x86_64-unknown-linux-musl", "x86_64-pc-windows-msvc"):
            with self.subTest(target=target):
                args = ["--artifact-kind", "base"]
                if target:
                    args.extend(["--target", target])
                manifest = self.invoke(*args)
                self.assertEqual(manifest["artifact_kind"], "base")
                self.assertEqual(manifest["features"], [])
                self.assertEqual(manifest["target"], target)
                expected = "cargo build --locked -j4 --release --no-default-features"
                if target:
                    expected += " --target " + target
                self.assertEqual(manifest["build_command"], expected)

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
                expected = "cargo build --locked -j4 --release"
                if target:
                    expected += " --target " + target
                self.assertEqual(manifest["build_command"], expected)

    def test_provenance_v2_hashes_only_active_graph_files_and_license_tree(self):
        manifest = self.invoke()
        self.assertEqual(manifest["schema"], 2)
        self.assertNotIn("worker_license_tree", manifest)
        self.assertEqual(manifest["license_tree"]["path"], "third_party/licenses")
        self.assertEqual(manifest["license_tree"]["file_count"], 1)
        paths = {record["path"] for record in manifest["covered_files"]}
        self.assertIn("Cargo.toml", paths)
        self.assertIn("Cargo.lock", paths)
        self.assertIn("vendor/cua-driver/LICENSE", paths)
        self.assertFalse(any("cua-worker" in path for path in paths))

    def test_missing_active_license_tree_fails_closed(self):
        (self.repo / "third_party/licenses/fixture/LICENSE").unlink()
        (self.repo / "third_party/licenses/fixture").rmdir()
        (self.repo / "third_party/licenses").rmdir()
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as raised:
            self.invoke()
        self.assertEqual(raised.exception.code, 1)

    def test_unknown_artifact_kind_is_rejected(self):
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as raised:
            self.invoke("--artifact-kind", "worker")
        self.assertEqual(raised.exception.code, 2)


class DistributionContractTests(unittest.TestCase):
    def test_ci_uses_root_native_tests_and_single_graph(self):
        workflow = (Path(__file__).resolve().parent.parent / ".github/workflows/ci.yml").read_text()
        self.assertNotIn("--manifest-path cua-worker/", workflow)
        self.assertNotIn("--test cua_context", workflow)
        self.assertNotIn("--test cua_admission", workflow)
        self.assertNotIn("--test cua_factory", workflow)
        self.assertNotIn("--test cua_ipv6", workflow)
        self.assertNotIn("--test cua_embedded", workflow)
        self.assertIn("--test cua_native", workflow)
        self.assertIn("--test cua_wayland", workflow)
        self.assertIn("--test cua_completion", workflow)
        self.assertIn("rg '^cua-driver-core '", workflow)
        self.assertIn("rg '^platform-linux '", workflow)
        self.assertNotIn("\n          rg '^coorworker-cua '", workflow)
        self.assertIn("--no-default-features --release --target x86_64-unknown-linux-musl", workflow)
        self.assertIn("--no-default-features --target x86_64-pc-windows-msvc", workflow)
        self.assertIn("python3 -I -B scripts/test_distribution.py", workflow)


if __name__ == "__main__":
    unittest.main()
