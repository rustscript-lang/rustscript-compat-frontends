#!/usr/bin/env python3
"""Prepare only this compat crate for a verified registry release."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tomllib
import urllib.error
import urllib.request
from pathlib import Path
from typing import NoReturn

CORE_GIT = "https://github.com/rustscript-lang/rustscript"
CORE_REV = "d8c21caf8f91a8e5ceaffa10ea0b1473cdd4f43b"
CRATES_API = "https://crates.io/api/v1/crates"
CRATES_IO_SOURCE = "registry+https://github.com/rust-lang/crates.io-index"
SEMVER = re.compile(
    r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
    r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
    r"(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$"
)


def fail(message: str) -> NoReturn:
    print(f"release prerequisite failed: {message}", file=sys.stderr)
    raise SystemExit(2)


def read_registry_version(version: str) -> dict[str, object]:
    url = f"{CRATES_API}/pd-vm/{version}"
    request = urllib.request.Request(url, headers={"User-Agent": "compat-frontends-release-preflight"})
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            payload = json.load(response)
    except urllib.error.HTTPError as error:
        if error.code == 404:
            fail(f"pd-vm {version} is not published; publish the compatible Core release first")
        fail(f"crates.io lookup for pd-vm {version} failed with HTTP {error.code}")
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot verify pd-vm {version} on crates.io: {error}")
    record = payload.get("version", {})
    if record.get("num") != version:
        fail(f"crates.io returned a different pd-vm version for requested {version}")
    if record.get("yanked"):
        fail(f"pd-vm {version} is yanked")
    return record


def validate_registry_resolution(metadata: dict[str, object], expected_pd_vm_version: str) -> None:
    packages = metadata.get("packages")
    if not isinstance(packages, list):
        fail("locked Cargo metadata has no package list")
    core_packages = {
        name: [item for item in packages if isinstance(item, dict) and item.get("name") == name]
        for name in ("pd-vm", "pd-host-function", "pd-host-schema")
    }
    pd_vm = core_packages["pd-vm"]
    if len(pd_vm) != 1 or pd_vm[0].get("version") != expected_pd_vm_version:
        fail("locked metadata does not resolve exactly the requested pd-vm version")
    if pd_vm[0].get("source") != CRATES_IO_SOURCE:
        fail("locked metadata did not resolve pd-vm from crates.io")
    for name, entries in core_packages.items():
        if not entries or not all(item.get("source") == CRATES_IO_SOURCE for item in entries):
            fail(f"locked metadata must resolve {name} from crates.io")


def prepare_registry_dependency(
    root: Path,
    pd_vm_version: str,
    *,
    run_command=subprocess.run,
) -> None:
    """Rewrite and lock the selected registry dependency as one rollback-safe step."""
    manifest_path = root / "Cargo.toml"
    lock_path = root / "Cargo.lock"
    manifest_bytes = manifest_path.read_bytes()
    lock_bytes = lock_path.read_bytes()
    manifest_text = manifest_bytes.decode("utf-8")
    pattern = re.compile(r"(?m)^(pd-vm\s*=\s*)\{[^\n]*\}\s*$")
    if len(pattern.findall(manifest_text)) != 1:
        fail("expected one inline pd-vm dependency in this compat manifest")
    prepared_manifest = pattern.sub(
        lambda match: f'{match.group(1)}{{ version = "{pd_vm_version}" }}',
        manifest_text,
        count=1,
    )
    try:
        manifest_path.write_text(prepared_manifest, encoding="utf-8")
        update = run_command(
            ["cargo", "update", "--package", "pd-vm", "--precise", pd_vm_version],
            cwd=root,
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )
        print(update.stdout, end="")
        if update.returncode:
            fail("Cargo could not resolve the requested registry dependency; the checked-out release workspace was restored")
        metadata = run_command(
            ["cargo", "metadata", "--locked", "--format-version", "1"],
            cwd=root,
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        if metadata.returncode:
            fail(f"locked Cargo metadata failed: {metadata.stderr.strip()}")
        try:
            resolved = json.loads(metadata.stdout)
        except json.JSONDecodeError as error:
            fail(f"locked Cargo metadata is invalid JSON: {error}")
        validate_registry_resolution(resolved, pd_vm_version)
    except BaseException:
        manifest_path.write_bytes(manifest_bytes)
        lock_path.write_bytes(lock_bytes)
        raise


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--compat-version", required=True, help="version already set in this compat manifest")
    parser.add_argument(
        "--pd-vm-version",
        required=True,
        help="explicit published pd-vm registry version; no fallback is selected",
    )
    args = parser.parse_args()
    for label, value in (("compat", args.compat_version), ("pd-vm", args.pd_vm_version)):
        if not SEMVER.fullmatch(value):
            fail(f"{label} version must be canonical semantic version text")

    root = Path(__file__).resolve().parents[1]
    manifest_path = root / "Cargo.toml"
    lock_path = root / "Cargo.lock"
    manifest_bytes = manifest_path.read_bytes()
    lock_bytes = lock_path.read_bytes()
    manifest_text = manifest_bytes.decode("utf-8")
    lock_text = lock_bytes.decode("utf-8")
    try:
        manifest = tomllib.loads(manifest_text)
        lock = tomllib.loads(lock_text)
    except tomllib.TOMLDecodeError as error:
        fail(f"manifest or lockfile is invalid TOML: {error}")

    if manifest["package"]["version"] != args.compat_version:
        fail(
            f"compat manifest version is {manifest['package']['version']}, "
            f"not requested {args.compat_version}; release a matching source revision"
        )
    dependency = manifest.get("dependencies", {}).get("pd-vm")
    if not isinstance(dependency, dict) or dependency.get("git") != CORE_GIT or dependency.get("rev") != CORE_REV:
        fail("development manifest no longer pins the accepted exact Core Git revision")
    core_source = f"git+{CORE_GIT}?rev={CORE_REV}#{CORE_REV}"
    lock_packages = [item for item in lock.get("package", []) if item.get("name") == "pd-vm"]
    if not any(item.get("source") == core_source for item in lock_packages):
        fail("Cargo.lock does not resolve pd-vm from the accepted exact Core Git revision")

    read_registry_version(args.pd_vm_version)
    prepare_registry_dependency(root, args.pd_vm_version)
    print(
        f"Prepared compat-only registry candidate: pd-vm {args.pd_vm_version}; "
        "the publication workflow must still pass locked build/test and cargo publish --dry-run."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
