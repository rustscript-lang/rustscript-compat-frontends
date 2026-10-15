use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

const CORE_GIT: &str = "https://github.com/rustscript-lang/rustscript";
const CORE_REV: &str = "47cac633cde1f9cb67607141e0a6cfe9103ca4a4";
const CRATES_IO_SOURCE: &str = "registry+https://github.com/rust-lang/crates.io-index";
const REGISTRY_VERSION_ENV: &str = "PD_VM_EXPECTED_REGISTRY_VERSION";

pub fn assert_locked_core_provenance<'a>(root: &Path, metadata: &'a Value) -> &'a Value {
    let packages = metadata["packages"]
        .as_array()
        .expect("Cargo metadata packages");
    let lock = fs::read_to_string(root.join("Cargo.lock")).expect("Cargo.lock");
    let manifest = fs::read_to_string(root.join("Cargo.toml")).expect("Cargo.toml");

    if let Ok(expected_version) = env::var(REGISTRY_VERSION_ENV) {
        assert!(
            !expected_version.is_empty(),
            "{REGISTRY_VERSION_ENV} must be non-empty"
        );
        let expected_manifest_line = format!("pd-vm = {{ version = \"{expected_version}\" }}");
        assert!(
            manifest
                .lines()
                .any(|line| line.trim() == expected_manifest_line),
            "prepared manifest must select pd-vm {expected_version} from the registry"
        );
        assert_lock_packages_use_registry(&lock, &expected_version);
        let mut matching = packages.iter().filter(|package| {
            package["name"] == "pd-vm" && package["version"].as_str() == Some(&expected_version)
        });
        let package = matching
            .next()
            .expect("locked metadata resolves the requested registry pd-vm package");
        assert_eq!(
            package["source"].as_str(),
            Some(CRATES_IO_SOURCE),
            "selected pd-vm version must come from crates.io"
        );
        assert!(
            matching.next().is_none(),
            "locked metadata has duplicate pd-vm package identities for the selected version"
        );
        package
    } else {
        let expected_source = format!("git+{CORE_GIT}?rev={CORE_REV}#{CORE_REV}");
        let expected_manifest_line =
            format!("pd-vm = {{ git = \"{CORE_GIT}\", rev = \"{CORE_REV}\" }}");
        assert!(
            manifest
                .lines()
                .any(|line| line.trim() == expected_manifest_line),
            "development manifest must keep the exact accepted Core Git revision"
        );
        assert_lock_packages_use_git(&lock, &expected_source);
        packages
            .iter()
            .find(|package| {
                package["name"] == "pd-vm"
                    && package["source"].as_str() == Some(expected_source.as_str())
            })
            .expect("Cargo metadata resolves the accepted exact Core Git revision")
    }
}

pub fn locked_core_root(package: &Value) -> PathBuf {
    PathBuf::from(
        package["manifest_path"]
            .as_str()
            .expect("locked pd-vm manifest path"),
    )
    .parent()
    .expect("pd-vm package root")
    .to_path_buf()
}

fn package_blocks<'a>(lock: &'a str, name: &str) -> Vec<&'a str> {
    lock.split("[[package]]")
        .filter(|block| {
            block
                .lines()
                .any(|line| line.trim() == format!("name = \"{name}\""))
        })
        .collect()
}

fn assert_lock_packages_use_registry(lock: &str, expected_pd_vm_version: &str) {
    for name in ["pd-vm", "pd-host-function", "pd-host-schema"] {
        let blocks = package_blocks(lock, name);
        assert!(!blocks.is_empty(), "Cargo.lock has no {name} package");
        for block in blocks {
            let source = block
                .lines()
                .find_map(|line| line.trim().strip_prefix("source = \"")?.strip_suffix('\"'))
                .unwrap_or_else(|| panic!("Cargo.lock {name} has no package source"));
            assert_eq!(
                source, CRATES_IO_SOURCE,
                "prepared Cargo.lock {name} must use crates.io source"
            );
            if name == "pd-vm" {
                assert!(
                    block.lines().any(
                        |line| line.trim() == format!("version = \"{expected_pd_vm_version}\"")
                    ),
                    "Cargo.lock pd-vm version does not match the selected registry release"
                );
            }
        }
    }
}

fn assert_lock_packages_use_git(lock: &str, expected_source: &str) {
    for name in ["pd-vm", "pd-host-function", "pd-host-schema"] {
        let blocks = package_blocks(lock, name);
        assert!(!blocks.is_empty(), "Cargo.lock has no {name} package");
        for block in blocks {
            assert!(
                block
                    .lines()
                    .any(|line| line.trim() == format!("source = \"{expected_source}\"")),
                "Cargo.lock {name} source is not pinned to accepted Core"
            );
        }
    }
}
