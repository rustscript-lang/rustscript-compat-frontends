import json
import os
import shlex
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


WORKFLOW = Path(__file__).resolve().parents[2] / ".github" / "workflows" / "publish-crates.yml"


class PublishWorkflowInputTests(unittest.TestCase):
    def test_dispatch_values_reach_release_script_as_quoted_shell_arguments(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")
        prepare_step = workflow.split("      - name: Prepare the compat-only registry candidate\n", 1)[1]
        prepare_step = prepare_step.split("      - name:", 1)[0]
        run_source = prepare_step.split("        run: >-\n", 1)[1]

        temp_parent = os.environ.get("TMPDIR")
        with tempfile.TemporaryDirectory(dir=temp_parent) as isolated_temp:
            marker = Path(isolated_temp) / "shell-input-marker"
            payload = f"$(touch {shlex.quote(str(marker))})"
            printer = "import json,sys; print(json.dumps(sys.argv[1:]))"
            python = shlex.quote(sys.executable)
            old_shell_source = (
                f"{python} -c {shlex.quote(printer)} "
                f'--compat-version "{payload}" --pd-vm-version "0.1.0"'
            )
            old = subprocess.run(
                ["bash", "-eu", "-c", old_shell_source],
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertEqual(old.returncode, 0, old.stderr)
            self.assertTrue(marker.exists(), "old source interpolation must execute the inert marker")
            marker.unlink()

            corrected_shell_source = (
                f"{python} -c {shlex.quote(printer)} "
                '--compat-version "$COMPAT_VERSION" --pd-vm-version "$PD_VM_VERSION"'
            )
            environment = os.environ.copy()
            environment["COMPAT_VERSION"] = payload
            environment["PD_VM_VERSION"] = "0.1.0"
            corrected = subprocess.run(
                ["bash", "-eu", "-c", corrected_shell_source],
                check=False,
                capture_output=True,
                text=True,
                env=environment,
            )
            self.assertEqual(corrected.returncode, 0, corrected.stderr)
            self.assertEqual(
                json.loads(corrected.stdout),
                ["--compat-version", payload, "--pd-vm-version", "0.1.0"],
            )
            self.assertFalse(marker.exists(), "quoted environment values must remain inert")

        self.assertIn("COMPAT_VERSION: ${{ inputs.compat_version }}", prepare_step)
        self.assertIn("PD_VM_VERSION: ${{ inputs.pd_vm_version }}", prepare_step)
        self.assertNotIn("${{ inputs.", run_source)
        self.assertIn('--compat-version "$COMPAT_VERSION"', run_source)
        self.assertIn('--pd-vm-version "$PD_VM_VERSION"', run_source)


if __name__ == "__main__":
    unittest.main()
