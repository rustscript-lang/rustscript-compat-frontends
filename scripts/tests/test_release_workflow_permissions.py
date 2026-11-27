import re
import unittest
from pathlib import Path


WORKFLOW = Path(__file__).resolve().parents[2] / ".github" / "workflows" / "ci.yml"


def job_block(source, name):
    match = re.search(
        rf"^  {re.escape(name)}:\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:\n|\Z)",
        source,
        re.MULTILINE | re.DOTALL,
    )
    if match is None:
        raise AssertionError(f"missing CI job {name!r}")
    return match.group("body")


class ReleaseWorkflowPermissionTests(unittest.TestCase):
    def test_release_asset_writer_is_tag_gated_and_waits_for_all_builds(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")
        self.assertIn("permissions:\n  contents: read\n", workflow)
        rust = job_block(workflow, "rust")
        binaries = job_block(workflow, "binaries")
        release = job_block(workflow, "release-assets")

        self.assertIn("permissions:\n      contents: read", rust)
        self.assertIn("permissions:\n      contents: read", binaries)
        self.assertIn("os: [ubuntu-latest, macos-latest, windows-latest]", binaries)
        self.assertIn("uses: actions/upload-artifact@v4", binaries)
        self.assertIn("name: pd-vm-compat-run-${{ runner.os }}-${{ runner.arch }}", binaries)
        self.assertNotIn("softprops/action-gh-release", binaries)

        self.assertIn("if: startsWith(github.ref, 'refs/tags/')", release)
        self.assertIn("needs: [rust, binaries]", release)
        self.assertIn("permissions:\n      contents: write", release)
        self.assertIn("uses: actions/download-artifact@v4", release)
        self.assertIn("pattern: pd-vm-compat-run-*", release)
        self.assertIn("merge-multiple: true", release)
        self.assertIn("uses: softprops/action-gh-release@v2", release)
        self.assertIn("files: dist/*", release)


if __name__ == "__main__":
    unittest.main()
