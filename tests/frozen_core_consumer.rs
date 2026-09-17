//! Frozen-core pin proof and exact JS/Lua example corpus.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use vm::{
    CompileSourceFileOptions, SourceFlavor, SourcePathError, Value, Vm, VmStatus, compile_source,
    compile_source_file_with_options, compile_source_with_flavor_and_options, encode_program,
};

const FROZEN_RUSTSCRIPT_REV: &str = "b1d6cffede77f49410bf63525f30b9a46b02dc01";
const RUSTSCRIPT_GIT: &str = "https://github.com/rustscript-lang/rustscript";
const EXPECTED_JS_LUA_EXAMPLES: usize = 4;
const STUB_STRINGS_RSS: &str = r#"
pub fn non_empty(value: string) -> bool {
    value.length != 0
}
"#;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn scratch_root() -> PathBuf {
    let root = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    root.join("compat-frontends-corpus")
}

fn rustscript_lock_source() -> String {
    format!("git+{RUSTSCRIPT_GIT}?rev={FROZEN_RUSTSCRIPT_REV}#{FROZEN_RUSTSCRIPT_REV}")
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
            continue;
        }
        out.push(path);
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
            Some("js" | "mjs" | "lua")
        )
    });
    paths.sort();
    paths
}

fn lock_package_blocks(lockfile: &str) -> Vec<&str> {
    lockfile.split("\n[[package]]").collect()
}

fn lock_package_name(block: &str) -> Option<&str> {
    block.lines().find_map(|line| {
        line.strip_prefix("name = \"")
            .and_then(|rest| rest.strip_suffix('"'))
    })
}

fn lock_package_source(block: &str) -> Option<&str> {
    block
        .lines()
        .find_map(|line| line.strip_prefix("source = "))
}

fn prepare_runnable_corpus() -> PathBuf {
    let root = scratch_root();
    let _ = fs::remove_dir_all(&root);
    let examples_dir = root.join("compat").join("examples");
    fs::create_dir_all(&examples_dir).expect("corpus examples directory");
    let stdlib = root
        .join("rustscript")
        .join("stdlib")
        .join("rss")
        .join("strings.rss");
    fs::create_dir_all(stdlib.parent().expect("stdlib parent")).expect("stdlib directory");
    fs::write(&stdlib, STUB_STRINGS_RSS).expect("stub strings.rss");

    for source in example_corpus() {
        let name = source
            .file_name()
            .expect("example file name")
            .to_str()
            .expect("utf-8 example name");
        fs::copy(&source, examples_dir.join(name))
            .unwrap_or_else(|error| panic!("copy {}: {error}", source.display()));
    }
    examples_dir
}

fn compile_example(path: &Path) -> vm::CompiledProgram {
    compile_source_file_with_options(path, pd_vm_compat_frontends::compile_options())
        .unwrap_or_else(|error| panic!("{} failed to compile: {error}", path.display()))
}

fn expect_err<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> E {
    match result {
        Ok(_) => panic!("{context}"),
        Err(error) => error,
    }
}

fn runner_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_pd-vm-compat-run"))
}

fn run_example_cli(path: &Path) {
    let output = Command::new(runner_bin())
        .arg(path)
        .output()
        .unwrap_or_else(|error| panic!("run {}: {error}", path.display()));
    assert!(
        output.status.success(),
        "{} failed through pd-vm-compat-run (status {}):\nstdout:\n{}\nstderr:\n{}",
        path.display(),
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn rustscript_crates_are_pinned_to_the_frozen_full_sha() {
    let cargo_toml = fs::read_to_string(manifest_dir().join("Cargo.toml")).expect("Cargo.toml");
    let cargo_lock = fs::read_to_string(manifest_dir().join("Cargo.lock")).expect("Cargo.lock");

    assert!(
        cargo_toml.contains(&format!("rev = \"{FROZEN_RUSTSCRIPT_REV}\"")),
        "Cargo.toml must pin the frozen full SHA"
    );
    assert!(
        cargo_toml.contains(&format!("git = \"{RUSTSCRIPT_GIT}\"")),
        "Cargo.toml must use the canonical HTTPS Git remote"
    );
    assert!(
        !cargo_toml.contains("path = \"../") && !cargo_toml.contains("path = '/"),
        "production RustScript crates must not use path pins"
    );
    assert!(
        !cargo_toml.contains("/home/"),
        "production RustScript crates must not use machine-specific paths"
    );

    let expected_source = rustscript_lock_source();
    let mut proven = Vec::new();
    for block in lock_package_blocks(&cargo_lock) {
        let Some(name) = lock_package_name(block) else {
            continue;
        };
        let Some(source) = lock_package_source(block) else {
            continue;
        };
        if !source.contains("github.com/rustscript-lang/rustscript") {
            continue;
        }
        assert_eq!(
            source.trim(),
            format!("\"{expected_source}\""),
            "Cargo.lock {name} must use the canonical HTTPS source at the pinned full rev"
        );
        proven.push(name.to_string());
    }
    assert!(
        proven.iter().any(|name| name == "pd-vm"),
        "Cargo.lock must prove pd-vm source: {proven:?}"
    );
    assert!(
        proven.iter().any(|name| name == "pd-host-function"),
        "Cargo.lock must prove pd-host-function source: {proven:?}"
    );
    assert!(
        proven.iter().any(|name| name == "pd-host-schema"),
        "Cargo.lock must prove pd-host-schema source: {proven:?}"
    );
}

#[test]
fn production_sources_are_consumer_only() {
    let sources = rust_sources(&manifest_dir().join("src"));
    assert!(!sources.is_empty(), "expected Rust sources under src/");
    for path in sources {
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        for token in [
            "HostApiBuilder",
            "HostApiCatalog",
            "HostFunctionRegistry",
            "HostModuleDescriptor",
            "HostFunctionDescriptor",
            "pd_host_function",
        ] {
            assert!(
                !source.contains(token),
                "{} is a consumer frontend and must not define hosts ({token})",
                path.display()
            );
        }
    }
}

#[test]
fn example_corpus_has_the_exact_checked_in_count() {
    let corpus = example_corpus();
    assert_eq!(
        corpus.len(),
        EXPECTED_JS_LUA_EXAMPLES,
        "JS/Lua example corpus count drifted: {corpus:?}"
    );
}

#[test]
fn example_corpus_compiles_emits_vmbc_and_runs_through_the_runner() {
    let corpus = example_corpus();
    assert_eq!(corpus.len(), EXPECTED_JS_LUA_EXAMPLES);
    let runnable = prepare_runnable_corpus();

    for source in corpus {
        let name = source
            .file_name()
            .expect("example file name")
            .to_str()
            .expect("utf-8 example name");
        let runnable_path = runnable.join(name);
        let compiled = compile_example(&runnable_path);
        let vmbc = encode_program(&compiled.program)
            .unwrap_or_else(|error| panic!("{} VMBC encode failed: {error}", source.display()));
        assert!(!vmbc.is_empty(), "{} produced empty VMBC", source.display());

        if name.contains("complex") {
            let imports: Vec<&str> = compiled
                .program
                .imports
                .iter()
                .map(|import| import.name.as_str())
                .collect();
            assert!(
                imports
                    .iter()
                    .any(|import| import.contains("runtime::sleep")),
                "{} missing host import runtime::sleep: {imports:?}",
                source.display()
            );
            assert!(
                imports.contains(&"print")
                    || !compiled.program.callable_prototypes.is_empty()
                    || !compiled.functions.is_empty(),
                "{} should keep callable print provenance: {imports:?}",
                source.display()
            );
        }

        run_example_cli(&runnable_path);
    }
}

#[test]
fn javascript_map_array_and_callable_semantics_survive_the_frozen_core() {
    let compiled = compile_source_with_flavor_and_options(
        r#"
            function add(lhs, rhs) {
                return lhs + rhs;
            }
            const obj = { score: 7 };
            const arr = [1, 2, 3];
            add(obj.score, arr[1]);
        "#,
        SourceFlavor::JavaScript,
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("map/array/callable fixture should compile");
    assert!(
        !compiled.program.callable_prototypes.is_empty()
            || !compiled.program.script_functions.is_empty()
            || !compiled.functions.is_empty(),
        "callable provenance should be recorded for a JS function"
    );
    let vmbc = encode_program(&compiled.program).expect("fixture VMBC");
    assert!(!vmbc.is_empty());

    let mut vm = Vm::new(compiled.program);
    let status = vm.run().expect("fixture should run");
    assert_eq!(status, VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::Int(9)]);
}

#[test]
fn lua_map_array_and_callable_semantics_survive_the_frozen_core() {
    let compiled = compile_source_with_flavor_and_options(
        r#"
            local function add(lhs, rhs)
                return lhs + rhs
            end
            local obj = { score = 7 }
            local arr = {1, 2, 3}
            add(obj.score, arr[2])
        "#,
        SourceFlavor::Lua,
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("lua map/array/callable fixture should compile");
    assert!(
        !compiled.program.callable_prototypes.is_empty()
            || !compiled.program.script_functions.is_empty()
            || !compiled.functions.is_empty(),
        "callable provenance should be recorded for a Lua function"
    );
    let mut vm = Vm::new(compiled.program);
    let status = vm.run().expect("fixture should run");
    assert_eq!(status, VmStatus::Halted);
    // Compatibility Lua arrays are 0-indexed, matching JS: arr[2] is 3.
    assert_eq!(vm.stack(), &[Value::Int(10)]);
}

#[test]
fn unsupported_language_and_invalid_sources_produce_portable_diagnostics() {
    let tmp = scratch_root().join("diagnostics");
    fs::create_dir_all(&tmp).expect("diagnostics directory");
    let python = tmp.join("probe.py");
    fs::write(&python, "print(1)\n").expect("write unsupported source");
    let error = expect_err(
        compile_source_file_with_options(
            python.as_path(),
            pd_vm_compat_frontends::compile_options(),
        ),
        "Python is not a compatibility frontend",
    );
    let message = error.to_string();
    assert!(
        matches!(error, SourcePathError::UnsupportedExtension(_))
            || message.to_ascii_lowercase().contains("unsupported"),
        "unsupported-language diagnostic should mention the extension: {message}"
    );
    assert!(
        !message.contains("/home/wow"),
        "diagnostics must not mention a machine-specific path: {message}"
    );

    let js_error = expect_err(
        compile_source_with_flavor_and_options(
            "function (",
            SourceFlavor::JavaScript,
            pd_vm_compat_frontends::compile_options(),
        ),
        "invalid JS should fail",
    );
    let js_message = js_error.to_string();
    assert!(!js_message.is_empty(), "JS diagnostics must not be empty");
    assert!(
        !js_message.contains("/home/wow"),
        "JS diagnostics must not mention a machine-specific path: {js_message}"
    );

    let lua_error = expect_err(
        compile_source_with_flavor_and_options(
            "function (",
            SourceFlavor::Lua,
            pd_vm_compat_frontends::compile_options(),
        ),
        "invalid Lua should fail",
    );
    let lua_message = lua_error.to_string();
    assert!(!lua_message.is_empty(), "Lua diagnostics must not be empty");
    assert!(
        !lua_message.contains("/home/wow"),
        "Lua diagnostics must not mention a machine-specific path: {lua_message}"
    );

    let rss_error = expect_err(compile_source("fn broken("), "invalid RSS should fail");
    let rss_message = rss_error.to_string();
    assert!(!rss_message.is_empty());
    let _ = CompileSourceFileOptions::new();
}
