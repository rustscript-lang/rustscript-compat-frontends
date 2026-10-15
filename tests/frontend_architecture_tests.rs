use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[allow(clippy::duplicate_mod)]
#[path = "common/core_provenance.rs"]
mod core_provenance;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn collect_source_files(root: &Path, output: &mut Vec<PathBuf>) {
    let entries =
        fs::read_dir(root).unwrap_or_else(|error| panic!("read {}: {error}", root.display()));
    for entry in entries {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            if matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some(".git" | "target")
            ) {
                continue;
            }
            collect_source_files(&path, output);
        } else if matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("rs" | "toml" | "json" | "ts" | "mts")
        ) {
            output.push(path);
        }
    }
}

fn assert_no_typescript_core_paths(core_root: &Path) {
    let needles = ["typescript", "TypeScript", ".mts", "swc_ecma"];
    let mut files = Vec::new();
    for area in ["src", "tests", "examples"] {
        let path = core_root.join(area);
        if path.exists() {
            collect_source_files(&path, &mut files);
        }
    }
    files.push(core_root.join("Cargo.toml"));
    files.push(core_root.join("Cargo.lock"));
    for path in files {
        if !path.is_file() {
            continue;
        }
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        for needle in needles {
            assert!(
                !source.contains(needle),
                "Core source contains forbidden TypeScript marker {needle:?}: {}",
                path.display()
            );
        }
    }
}

#[test]
fn compat_production_has_no_javascript_frontend_routes() {
    let root = manifest_dir();
    for path in ["src/javascript.rs", "src/js_namespace.rs"] {
        assert!(
            !root.join(path).exists(),
            "retired frontend file remains: {path}"
        );
    }
    for path in ["examples/example.js", "examples/example_complex.js"] {
        assert!(
            !root.join(path).exists(),
            "retired JavaScript example remains: {path}"
        );
    }
    let retired = [
        "JavaScriptPlugin",
        "JAVASCRIPT_PLUGIN",
        "SourceFlavor::JavaScript",
        "FrontendImportSyntax::JavaScript",
        "parse_js_imports",
        "js_namespace",
    ];
    let mut files = Vec::new();
    collect_source_files(&root.join("src"), &mut files);
    for path in files {
        if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
            continue;
        }
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        for identifier in retired {
            assert!(
                !source.contains(identifier),
                "retired production identifier {identifier:?} remains in {}",
                path.display()
            );
        }
    }
}

#[test]
fn architecture_gate_scans_the_selected_locked_core_source() {
    let root = manifest_dir();
    let output = Command::new("cargo")
        .args(["metadata", "--locked", "--format-version", "1"])
        .current_dir(&root)
        .output()
        .expect("run locked Cargo metadata");
    assert!(
        output.status.success(),
        "locked Cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("Cargo metadata JSON");
    let package = core_provenance::assert_locked_core_provenance(&root, &metadata);
    let core_root = core_provenance::locked_core_root(package);
    assert_no_typescript_core_paths(&core_root);
}
