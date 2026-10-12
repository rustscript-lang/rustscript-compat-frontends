//! Frozen dependency proof and the exact TypeScript/Lua example corpus.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use vm::{
    SourcePathError, Value, Vm, VmStatus, compile_source, compile_source_file_with_options,
    compile_source_with_flavor_and_options, decode_program, encode_program,
};

#[allow(clippy::duplicate_mod)]
#[path = "common/core_provenance.rs"]
mod core_provenance;
const FROZEN_STRINGS_RSS: &str = include_str!("fixtures/frozen_stdlib/strings.rss");
const FROZEN_STRINGS_BYTES: usize = 3522;
const EXPECTED_TS_LUA_EXAMPLES: usize = 5;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn locked_core_package() -> serde_json::Value {
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
    core_provenance::assert_locked_core_provenance(&root, &metadata).clone()
}

fn scratch_root() -> PathBuf {
    std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

struct ScratchTree {
    root: PathBuf,
}

impl ScratchTree {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        loop {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let root = scratch_root().join(format!(
                "compat-frontends-corpus-{}-{id}",
                std::process::id()
            ));
            match fs::create_dir(&root) {
                Ok(()) => return Self { root },
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create scratch tree {}: {error}", root.display()),
            }
        }
    }
}

impl Drop for ScratchTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap_or_else(|error| panic!("read {}: {error}", dir.display()))
    {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if matches!(name, ".git" | "target") {
                continue;
            }
            collect_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn rust_sources(dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    collect_files(dir, &mut paths);
    paths.retain(|path| path.extension().and_then(|ext| ext.to_str()) == Some("rs"));
    paths.sort();
    paths
}

fn example_corpus() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    collect_files(&manifest_dir().join("examples"), &mut paths);
    paths.retain(|path| {
        matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("ts" | "mts" | "lua")
        )
    });
    paths.sort();
    paths
}

fn prepare_runnable_corpus(corpus: &[PathBuf]) -> (ScratchTree, PathBuf) {
    let scratch = ScratchTree::new();
    let examples_dir = scratch.root.join("compat").join("examples");
    fs::create_dir_all(&examples_dir).expect("corpus examples directory");
    let stdlib = scratch
        .root
        .join("rustscript")
        .join("stdlib")
        .join("rss")
        .join("strings.rss");
    fs::create_dir_all(stdlib.parent().expect("stdlib parent")).expect("stdlib directory");
    fs::write(&stdlib, FROZEN_STRINGS_RSS).expect("frozen strings.rss");

    for source in corpus {
        let name = source.file_name().expect("example file name");
        fs::copy(source, examples_dir.join(name))
            .unwrap_or_else(|error| panic!("copy {}: {error}", source.display()));
    }
    (scratch, examples_dir)
}

fn compile_example(path: &Path) -> vm::CompiledProgram {
    compile_source_file_with_options(path, pd_vm_compat_frontends::compile_options())
        .unwrap_or_else(|error| panic!("{} failed to compile: {error}", path.display()))
}

fn runner_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_pd-vm-compat-run"))
}

fn run_example_cli(path: &Path) -> (String, String) {
    let output = Command::new(runner_bin())
        .arg(path)
        .output()
        .unwrap_or_else(|error| panic!("run {}: {error}", path.display()));
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "{} failed through pd-vm-compat-run ({}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
        path.display(),
        output.status
    );
    (stdout, stderr)
}

fn expected_corpus_output(name: &str) -> &'static str {
    match name {
        "example.ts" => "42",
        "example.mts" => "TypeScript module entry",
        "example_complex.ts" => "12",
        "example.lua" => "42",
        "example_complex.lua" => "12",
        other => panic!("unexpected corpus example {other}"),
    }
}

fn expected_corpus_value(name: &str) -> Value {
    match name {
        "example.ts" | "example.lua" => Value::Int(42),
        "example_complex.ts" | "example_complex.lua" => Value::Int(12),
        "example.mts" => Value::string("TypeScript module entry"),
        other => panic!("unexpected corpus example {other}"),
    }
}

#[test]
fn core_dependency_provenance_matches_the_selected_build_state() {
    let package = locked_core_package();
    assert_eq!(package["name"], "pd-vm");
}

fn forbidden_frontend_authority_token(source: &str) -> Option<&'static str> {
    [
        "HostApiBuilder",
        "HostFunctionRegistry",
        "HostModuleDescriptor",
        "HostFunctionDescriptor",
        "HostApiCatalog::new",
        "HostApiCatalog::standard",
        ".implicit_extern_names.push",
        "pd_host_function",
    ]
    .into_iter()
    .find(|token| source.contains(token))
}

#[test]
fn frontend_authority_guard_detects_catalog_and_descriptor_controls() {
    for token in [
        "HostApiBuilder",
        "HostFunctionRegistry",
        "HostModuleDescriptor",
        "HostFunctionDescriptor",
        "HostApiCatalog::new",
        "HostApiCatalog::standard",
        ".implicit_extern_names.push",
        "pd_host_function",
    ] {
        assert_eq!(
            forbidden_frontend_authority_token(token),
            Some(token),
            "frontend guard must detect authority creation token {token}"
        );
    }
}

#[test]
fn production_sources_consume_catalog_authority_without_creating_it() {
    let root = manifest_dir();
    let mut sources = rust_sources(&root.join("src/typescript"));
    sources.extend(rust_sources(&root.join("src/frontends")));
    sources.push(root.join("src/source_loader.rs"));
    sources.sort();
    sources.dedup();
    assert!(!sources.is_empty());
    for path in sources {
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        if let Some(token) = forbidden_frontend_authority_token(&source) {
            panic!(
                "{} must consume caller authority without creating it ({token})",
                path.display()
            );
        }
    }

    let runtime_owner = root.join("src/runtime/array.rs");
    assert!(
        runtime_owner.is_file(),
        "native array host authority must live under the explicit runtime owner"
    );
    let owner_source = fs::read_to_string(&runtime_owner)
        .unwrap_or_else(|error| panic!("read {}: {error}", runtime_owner.display()));
    assert!(owner_source.contains("pub fn typescript_array_host_module()"));
    let frontend_module = fs::read_to_string(root.join("src/typescript/mod.rs"))
        .expect("read TypeScript frontend module");
    assert!(!frontend_module.contains("typescript_array_host_module"));
}

#[test]
fn example_corpus_has_the_exact_checked_in_count() {
    let corpus = example_corpus();
    assert_eq!(
        corpus.len(),
        EXPECTED_TS_LUA_EXAMPLES,
        "TS/Lua example count drifted: {corpus:?}"
    );
}

#[test]
fn example_corpus_compiles_emits_vmbc_and_runs_through_the_runner() {
    let corpus = example_corpus();
    assert_eq!(corpus.len(), EXPECTED_TS_LUA_EXAMPLES);
    let (_scratch, runnable) = prepare_runnable_corpus(&corpus);
    for source in corpus {
        let name = source
            .file_name()
            .expect("example file name")
            .to_str()
            .expect("UTF-8 example name");
        let path = runnable.join(name);
        let compiled = compile_example(&path);
        let vmbc = encode_program(&compiled.program)
            .unwrap_or_else(|error| panic!("{} VMBC encode failed: {error}", source.display()));
        assert!(!vmbc.is_empty(), "{} produced empty VMBC", source.display());
        let decoded = decode_program(&vmbc)
            .unwrap_or_else(|error| panic!("{} VMBC decode failed: {error}", source.display()));
        if name != "example_complex.lua" {
            let mut machine = Vm::new(decoded);
            assert_eq!(
                machine.run().expect("decoded VMBC program should run"),
                VmStatus::Halted,
                "{} decoded VMBC status",
                source.display()
            );
            assert_eq!(
                machine.stack(),
                &[expected_corpus_value(name)],
                "{} decoded VMBC result",
                source.display()
            );
        }
        let (stdout, stderr) = run_example_cli(&path);
        assert!(
            stderr.trim().is_empty(),
            "{} produced unexpected stderr: {stderr}",
            source.display()
        );
        assert!(
            stdout.contains(expected_corpus_output(name)),
            "{} stdout should contain {}: {stdout:?}",
            source.display(),
            expected_corpus_output(name)
        );
    }
}

#[test]
fn lua_scalar_and_mutable_local_semantics_remain_available() {
    let compiled = compile_source_with_flavor_and_options(
        "local value = 40\nvalue = value + 2\nvalue\n",
        pd_vm_compat_frontends::lua_flavor(),
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("accepted Lua fixture should compile");
    let mut machine = Vm::new(compiled.program);
    assert_eq!(
        machine.run().expect("Lua fixture should run"),
        VmStatus::Halted
    );
    assert_eq!(machine.stack(), &[Value::Int(42)]);
}

#[test]
fn runner_rejects_javascript_entries_and_dependencies_without_vmbc() {
    let scratch = ScratchTree::new();
    for extension in ["js", "mjs", "cjs"] {
        let entry = scratch.root.join(format!("entry.{extension}"));
        fs::write(&entry, "const value = 1;\nvalue;\n").expect("write entry source");
        let entry_vmbc = scratch.root.join(format!("entry-{extension}.vmbc"));
        let entry_output = Command::new(runner_bin())
            .args(["--emit-vmbc"])
            .arg(&entry_vmbc)
            .arg(&entry)
            .output()
            .expect("run JavaScript-family entry through the compatibility CLI");
        let entry_diagnostic = format!(
            "{}{}",
            String::from_utf8_lossy(&entry_output.stdout),
            String::from_utf8_lossy(&entry_output.stderr)
        );
        assert!(
            !entry_output.status.success(),
            ".{extension} entry unexpectedly succeeded: {entry_diagnostic}"
        );
        assert!(
            entry_diagnostic.contains(&format!(".{extension}")),
            ".{extension} entry diagnostic did not identify its extension: {entry_diagnostic}"
        );
        assert!(
            !entry_vmbc.exists(),
            ".{extension} entry failure created VMBC at {}",
            entry_vmbc.display()
        );

        let dependency_dir = scratch.root.join(format!("dependency-{extension}"));
        fs::create_dir(&dependency_dir).expect("create dependency project");
        let dependency = dependency_dir.join(format!("module.{extension}"));
        let dependency_source = if extension == "cjs" {
            "module.exports = { value: 7 };\n"
        } else {
            "export function value(): number { return 7; }\n"
        };
        fs::write(&dependency, dependency_source).expect("write dependency source");
        let main = dependency_dir.join("main.ts");
        fs::write(
            &main,
            format!("import {{ value }} from \"./module.{extension}\";\nvalue();\n"),
        )
        .expect("write TypeScript importer");
        let dependency_vmbc = scratch.root.join(format!("dependency-{extension}.vmbc"));
        let dependency_output = Command::new(runner_bin())
            .args(["--emit-vmbc"])
            .arg(&dependency_vmbc)
            .arg(&main)
            .output()
            .expect("run JavaScript-family dependency through the compatibility CLI");
        let dependency_diagnostic = format!(
            "{}{}",
            String::from_utf8_lossy(&dependency_output.stdout),
            String::from_utf8_lossy(&dependency_output.stderr)
        );
        assert!(
            !dependency_output.status.success(),
            ".{extension} dependency unexpectedly succeeded: {dependency_diagnostic}"
        );
        assert!(
            dependency_diagnostic.contains(&format!(".{extension}")),
            ".{extension} dependency diagnostic did not identify its extension: {dependency_diagnostic}"
        );
        assert!(
            !dependency_vmbc.exists(),
            ".{extension} dependency failure created VMBC at {}",
            dependency_vmbc.display()
        );
    }
}

#[test]
fn unsupported_language_and_invalid_sources_produce_portable_diagnostics() {
    let scratch = ScratchTree::new();
    let python = scratch.root.join("probe.py");
    fs::write(&python, "print(1)\n").expect("write unsupported source");
    let error = match compile_source_file_with_options(
        &python,
        pd_vm_compat_frontends::compile_options(),
    ) {
        Ok(_) => panic!("Python is not a compatibility frontend"),
        Err(error) => error,
    };
    assert!(matches!(error, SourcePathError::UnsupportedExtension(ref ext) if ext == "py"));
    assert!(error.to_string().contains(".py"));
    let rss_error = match compile_source("fn broken(") {
        Ok(_) => panic!("invalid RSS should fail"),
        Err(error) => error,
    };
    assert!(!rss_error.to_string().is_empty());
}

#[test]
fn frozen_strings_fixture_matches_the_exact_locked_core_source_bytes() {
    let bytes = include_bytes!("fixtures/frozen_stdlib/strings.rss");
    assert_eq!(bytes.len(), FROZEN_STRINGS_BYTES);
    let core_package = locked_core_package();
    let pinned_core = core_provenance::locked_core_root(&core_package);
    let core_bytes =
        fs::read(pinned_core.join("stdlib/rss/strings.rss")).expect("read locked Core strings.rss");
    assert_eq!(
        bytes.as_slice(),
        core_bytes.as_slice(),
        "frozen stdlib bytes differ from exact locked Core source"
    );
    assert!(FROZEN_STRINGS_RSS.contains("pub fn non_empty(value: string) -> bool"));
}
