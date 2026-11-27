import contextlib
import importlib.util
import io
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


SCRIPT = Path(__file__).resolve().parents[1] / "prepare-release.py"
CORE_SOURCE = "git+https://github.com/rustscript-lang/rustscript?rev=d8c21caf8f91a8e5ceaffa10ea0b1473cdd4f43b#d8c21caf8f91a8e5ceaffa10ea0b1473cdd4f43b"


class PrepareReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR"))
        self.root = Path(self.temp.name)
        scripts = self.root / "scripts"
        scripts.mkdir()
        self.script_path = scripts / "prepare-release.py"
        shutil.copy2(SCRIPT, self.script_path)
        self.manifest = self.root / "Cargo.toml"
        self.lock = self.root / "Cargo.lock"
        self.manifest.write_text(
            "[package]\nname = \"pd-vm-compat-frontends\"\nversion = \"0.1.0\"\n"
            "[dependencies]\npd-vm = { git = \"https://github.com/rustscript-lang/rustscript\", "
            "rev = \"d8c21caf8f91a8e5ceaffa10ea0b1473cdd4f43b\" }\n",
            encoding="utf-8",
        )
        self.lock.write_text(
            "version = 4\n\n"
            "[[package]]\nname = \"pd-vm\"\nversion = \"0.1.0\"\n"
            f"source = \"{CORE_SOURCE}\"\n\n"
            "[[package]]\nname = \"pd-host-function\"\nversion = \"0.1.0\"\n"
            f"source = \"{CORE_SOURCE}\"\n\n"
            "[[package]]\nname = \"pd-host-schema\"\nversion = \"0.1.0\"\n"
            f"source = \"{CORE_SOURCE}\"\n",
            encoding="utf-8",
        )
        spec = importlib.util.spec_from_file_location("prepare_release_under_test", self.script_path)
        if spec is None or spec.loader is None:
            self.fail("could not load the copied release script")
        self.module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.module)

    def tearDown(self):
        self.temp.cleanup()

    def assert_unchanged(self, manifest_before, lock_before):
        self.assertEqual(self.manifest.read_bytes(), manifest_before)
        self.assertEqual(self.lock.read_bytes(), lock_before)

    def test_pd_vm_registry_version_has_no_default_and_does_not_mutate(self):
        manifest_before = self.manifest.read_bytes()
        lock_before = self.lock.read_bytes()
        output = io.StringIO()
        with contextlib.redirect_stderr(output), mock.patch.object(sys, "argv", [str(self.script_path)]):
            with self.assertRaises(SystemExit) as error:
                self.module.main()
        self.assertEqual(error.exception.code, 2)
        self.assertIn("--pd-vm-version", output.getvalue())
        self.assert_unchanged(manifest_before, lock_before)

    def test_manifest_version_mismatch_fails_before_registry_lookup_or_mutation(self):
        manifest_before = self.manifest.read_bytes()
        lock_before = self.lock.read_bytes()
        output = io.StringIO()
        with contextlib.redirect_stderr(output), mock.patch.object(
            sys,
            "argv",
            [str(self.script_path), "--compat-version", "0.1.1", "--pd-vm-version", "0.2.0"],
        ), mock.patch.object(self.module, "read_registry_version", side_effect=AssertionError("network lookup was premature")):
            with self.assertRaises(SystemExit) as error:
                self.module.main()
        self.assertEqual(error.exception.code, 2)
        self.assertIn("compat manifest version", output.getvalue())
        self.assert_unchanged(manifest_before, lock_before)

    def test_registry_resolution_rejects_development_git_source(self):
        metadata = {
            "packages": [
                {"name": "pd-vm", "version": "0.1.0", "source": CORE_SOURCE},
                {"name": "pd-host-function", "version": "0.1.0", "source": CORE_SOURCE},
                {"name": "pd-host-schema", "version": "0.1.0", "source": CORE_SOURCE},
            ]
        }
        output = io.StringIO()
        with contextlib.redirect_stderr(output):
            with self.assertRaises(SystemExit) as error:
                self.module.validate_registry_resolution(metadata, "0.1.0")
        self.assertEqual(error.exception.code, 2)
        self.assertIn("from crates.io", output.getvalue())

    def test_cargo_update_failure_restores_manifest_and_lock_bytes(self):
        manifest_before = self.manifest.read_bytes()
        lock_before = self.lock.read_bytes()

        def failed_update(command, **_kwargs):
            self.lock.write_bytes(b"partially updated lock")
            return subprocess.CompletedProcess(command, 101, "resolver failed\n", "")

        output = io.StringIO()
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit) as error:
                self.module.prepare_registry_dependency(self.root, "0.2.0", run_command=failed_update)
        self.assertEqual(error.exception.code, 2)
        self.assertIn("resolver failed", output.getvalue())
        self.assert_unchanged(manifest_before, lock_before)

    def test_locked_metadata_failure_restores_manifest_and_lock_bytes(self):
        manifest_before = self.manifest.read_bytes()
        lock_before = self.lock.read_bytes()
        calls = 0

        def failed_metadata(command, **_kwargs):
            nonlocal calls
            calls += 1
            if command[1] == "update":
                self.lock.write_bytes(b"successfully updated lock before metadata failure")
                return subprocess.CompletedProcess(command, 0, "lock updated\n", "")
            return subprocess.CompletedProcess(command, 1, "", "metadata failed\n")

        errors = io.StringIO()
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(errors):
            with self.assertRaises(SystemExit) as error:
                self.module.prepare_registry_dependency(self.root, "0.2.0", run_command=failed_metadata)
        self.assertEqual(error.exception.code, 2)
        self.assertEqual(calls, 2)
        self.assertIn("metadata failed", errors.getvalue())
        self.assert_unchanged(manifest_before, lock_before)


if __name__ == "__main__":
    unittest.main()
