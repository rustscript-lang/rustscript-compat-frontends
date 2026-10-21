use std::collections::VecDeque;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use vm::{
    CallOutcome, CallReturn, HostApiBuilder, HostFunctionRegistry, HostFunctionSchema,
    HostParamSchema, HostTypeSchema, TypeSchema, Value, Vm, VmError, VmStatus,
    compile_source_file_with_options,
};

const ORIGINAL_FACTORIAL: &[u8] = include_bytes!("fixtures/typescript/factorial.ts");
const NODE_FACTORIAL_VECTORS: &str =
    include_str!("fixtures/typescript/factorial-node-vectors.json");

struct ScratchTree(PathBuf);

impl ScratchTree {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        loop {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!("compat-cli-services-{}-{id}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create scratch tree {}: {error}", path.display()),
            }
        }
    }
}

impl Drop for ScratchTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn runner_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_pd-vm-compat-run"))
}

fn factorial_project(root: &Path) -> PathBuf {
    let maths = root.join("maths");
    fs::create_dir_all(&maths).expect("create factorial module directory");
    let factorial = maths.join("factorial.ts");
    fs::write(&factorial, ORIGINAL_FACTORIAL).expect("write unchanged upstream factorial source");
    assert_eq!(
        fs::read(&factorial)
            .expect("read factorial module")
            .as_slice(),
        ORIGINAL_FACTORIAL,
        "imported calls must use exact upstream bytes"
    );
    root.join("main.ts")
}

fn run_cli(args: &[String]) -> Output {
    run_cli_with_stdin(args, &[])
}

fn run_cli_with_stdin(args: &[String], input: &[u8]) -> Output {
    let mut child = Command::new(runner_bin())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start pd-vm-compat-run");
    let mut stdin = child.stdin.take().expect("pipe CLI stdin");
    let input = input.to_vec();
    let input_writer = thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let mut stdout = child.stdout.take().expect("capture CLI stdout");
    let mut stderr = child.stderr.take().expect("capture CLI stderr");
    let stdout_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).expect("read CLI stdout");
        bytes
    });
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).expect("read CLI stderr");
        bytes
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll CLI process") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("pd-vm-compat-run exceeded the 30-second test bound");
        }
        thread::sleep(Duration::from_millis(10));
    };
    input_writer.join().expect("join CLI stdin writer");
    Output {
        status,
        stdout: stdout_reader.join().expect("join stdout reader"),
        stderr: stderr_reader.join().expect("join stderr reader"),
    }
}

fn probe_catalog() -> Arc<vm::HostApiCatalog> {
    static CATALOG: OnceLock<Arc<vm::HostApiCatalog>> = OnceLock::new();
    Arc::clone(CATALOG.get_or_init(|| {
        let error_catalog =
            pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_catalog();
        let mut builder = HostApiBuilder::new();
        for function in error_catalog.functions() {
            builder.function(function.clone());
        }
        builder.function(HostFunctionSchema::with_return(
            "probe::after",
            Vec::<HostParamSchema>::new(),
            HostTypeSchema::Null,
        ));
        Arc::new(builder.build().expect("proof host catalog is valid"))
    }))
}

fn probe_compile_options() -> vm::CompileSourceFileOptions {
    pd_vm_compat_frontends::compile_options_with_catalog(probe_catalog())
}

struct VmInvocation {
    initialized: AtomicBool,
    init_runs: AtomicUsize,
    after_calls: AtomicUsize,
}

struct AfterMarker {
    invocation: Arc<VmInvocation>,
}

impl vm::HostFunction for AfterMarker {
    fn call(&mut self, _machine: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
        if !args.is_empty() {
            return Err(VmError::HostError(
                "probe::after takes no arguments".to_owned(),
            ));
        }
        if !self.invocation.initialized.load(Ordering::SeqCst) {
            return Err(VmError::HostError(
                "VM init did not precede probe::after".to_owned(),
            ));
        }
        self.invocation.after_calls.fetch_add(1, Ordering::SeqCst);
        Ok(CallOutcome::Return(CallReturn::one(Value::Null)))
    }
}

struct ProbeServices {
    services: vm::cli::CliHostServices<'static>,
    invocations: Arc<Mutex<Vec<Arc<VmInvocation>>>>,
}

fn probe_services() -> ProbeServices {
    let catalog = probe_catalog();
    let after_schema = vm::catalog_import_schemas(&catalog, "probe::after")
        .into_iter()
        .next()
        .expect("probe host schema is in the exact catalog");
    let invocations = Arc::new(Mutex::new(Vec::new()));
    let pending = Arc::new(Mutex::new(VecDeque::new()));
    let registry_invocations = Arc::clone(&invocations);
    let registry_pending = Arc::clone(&pending);
    let init_pending = Arc::clone(&pending);
    let registry_catalog = Arc::clone(&catalog);
    let services = vm::cli::CliHostServices::new(catalog, move || {
        let invocation = Arc::new(VmInvocation {
            initialized: AtomicBool::new(false),
            init_runs: AtomicUsize::new(0),
            after_calls: AtomicUsize::new(0),
        });
        registry_pending
            .lock()
            .expect("registry invocation queue")
            .push_back(Arc::clone(&invocation));
        registry_invocations
            .lock()
            .expect("per-VM invocation records")
            .push(Arc::clone(&invocation));
        let host_invocation = Arc::clone(&invocation);
        let mut registry = HostFunctionRegistry::new();
        pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module()
            .install_from_catalog(&mut registry, &registry_catalog)?;
        registry
            .register_catalog(after_schema.clone(), move || {
                Box::new(AfterMarker {
                    invocation: Arc::clone(&host_invocation),
                })
            })
            .map_err(|error| VmError::HostError(error.to_string()))?;
        Ok(registry)
    })
    .with_vm_init(move |_machine| {
        let invocation = init_pending
            .lock()
            .expect("initializer invocation queue")
            .pop_front()
            .expect("registry factory created a per-VM invocation record");
        invocation.initialized.store(true, Ordering::SeqCst);
        invocation.init_runs.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    ProbeServices {
        services,
        invocations,
    }
}

fn assert_probe_invocation(probe: &ProbeServices, index: usize, expected_after_calls: usize) {
    let invocations = probe
        .invocations
        .lock()
        .expect("read per-VM invocation records");
    assert_eq!(invocations.len(), index + 1, "one fresh registry per VM");
    let invocation = invocations
        .get(index)
        .expect("each VM has its own invocation record");
    if index > 0 {
        assert!(
            !Arc::ptr_eq(&invocations[index - 1], invocation),
            "independent VMs must not share mutable invocation state"
        );
    }
    assert!(invocation.initialized.load(Ordering::SeqCst));
    assert_eq!(invocation.init_runs.load(Ordering::SeqCst), 1);
    assert_eq!(
        invocation.after_calls.load(Ordering::SeqCst),
        expected_after_calls
    );
}

fn probe_runtime() -> vm::cli::CliRuntime {
    vm::cli::CliRuntime {
        binary_name: "compat-host-services-test",
        default_source: "unused.ts",
        compile_options: probe_compile_options,
    }
}

fn empty_host_catalog() -> Arc<vm::HostApiCatalog> {
    Arc::new(
        HostApiBuilder::new()
            .build()
            .expect("empty catalog is valid"),
    )
}

fn wrong_error_catalog() -> Arc<vm::HostApiCatalog> {
    let mut builder = HostApiBuilder::new();
    builder.function(HostFunctionSchema::with_return(
        "typescript::throw_error",
        vec![HostParamSchema::value("message", HostTypeSchema::String)],
        HostTypeSchema::Int,
    ));
    Arc::new(
        builder
            .build()
            .expect("wrong-return catalog is structurally valid"),
    )
}

fn empty_catalog_options() -> vm::CompileSourceFileOptions {
    pd_vm_compat_frontends::compile_options_with_catalog(empty_host_catalog())
}

fn wrong_error_options() -> vm::CompileSourceFileOptions {
    pd_vm_compat_frontends::compile_options_with_catalog(wrong_error_catalog())
}

#[test]
fn production_cli_factory_retains_source_certification_before_guest() {
    let scratch = ScratchTree::new();
    let main = factorial_project(&scratch.0);
    fs::write(&main, "import { factorial as original } from \"./maths/factorial.ts\";\nconst result: number = original(6);\nresult;\n").expect("write real original-file importer");
    let observed = Arc::new(AtomicBool::new(false));
    let during_init = Arc::clone(&observed);
    let services = pd_vm_compat_frontends::runtime::cli::typescript_cli_host_services()
        .with_vm_init(move |machine| {
            during_init.store(
                machine
                    .capture_certification_stats()
                    .is_some_and(|stats| stats.valid),
                Ordering::SeqCst,
            );
            Ok(())
        });
    let runtime = vm::cli::CliRuntime {
        binary_name: "pd-vm-compat-run",
        default_source: "examples/example.ts",
        compile_options: pd_vm_compat_frontends::compile_options,
    };
    vm::cli::run_with_args(
        &runtime,
        &["--fuel".into(), "100000".into(), main.display().to_string()],
        &services,
    )
    .expect("actual production source CLI must execute the original import");
    assert!(
        observed.load(Ordering::SeqCst),
        "production source facts must reach vm_init before guest execution"
    );
}

#[test]
fn original_factorial_entry_runs_through_the_published_services_cli() {
    let scratch = ScratchTree::new();
    let entry = scratch.0.join("factorial.ts");
    fs::write(&entry, ORIGINAL_FACTORIAL).expect("write unchanged upstream factorial source");
    assert_eq!(
        fs::read(&entry).expect("read factorial entry").as_slice(),
        ORIGINAL_FACTORIAL,
        "the CLI must consume the exact upstream fixture bytes"
    );

    let output = run_cli(&[
        "--fuel".to_owned(),
        "50000".to_owned(),
        entry.display().to_string(),
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "original factorial entry failed through pd-vm-compat-run ({}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );
    assert!(
        stdout.contains("stack: []"),
        "definition-only original entry should finish with an empty stack: {stdout}"
    );

    let artifact = scratch.0.join("factorial-entry.vmbc");
    let emitted = run_cli(&[
        "--emit-vmbc".to_owned(),
        artifact.display().to_string(),
        entry.display().to_string(),
    ]);
    assert!(
        emitted.status.success(),
        "original factorial VMBC emission failed: {}",
        String::from_utf8_lossy(&emitted.stderr)
    );
    assert!(
        artifact.is_file(),
        "CLI must write the requested VMBC artifact"
    );
    let loaded = run_cli(&[
        "--fuel".to_owned(),
        "50000".to_owned(),
        artifact.display().to_string(),
    ]);
    assert!(
        loaded.status.success(),
        "original factorial VMBC load failed: {}",
        String::from_utf8_lossy(&loaded.stderr)
    );
    assert!(
        String::from_utf8_lossy(&loaded.stdout).contains("stack: []"),
        "definition-only VMBC entry should finish with an empty stack"
    );
}

#[test]
fn imported_original_factorial_cli_matches_all_node_return_and_error_vectors() {
    let vectors: serde_json::Value =
        serde_json::from_str(NODE_FACTORIAL_VECTORS).expect("Node vectors are JSON");
    assert_eq!(vectors["node"], "v26.7.0");
    assert_eq!(
        vectors["upstream_commit"],
        "19b4ced86c99815f142d4a46a028f55487b8038a"
    );
    let cases = vectors["cases"]
        .as_array()
        .expect("Node cases are an array");
    assert_eq!(cases.len(), 9);
    assert_eq!(
        cases
            .iter()
            .filter(|case| case["outcome"] == "return")
            .count(),
        4
    );
    assert_eq!(
        cases
            .iter()
            .filter(|case| case["outcome"] == "throw")
            .count(),
        5
    );

    let scratch = ScratchTree::new();
    let main = factorial_project(&scratch.0);
    for case in cases {
        let expression = case["expression"]
            .as_str()
            .expect("Node input expression is text");
        let source = format!(
            "import {{ factorial as factorialFromFile }} from \"./maths/factorial.ts\";\n\
             const result: number = factorialFromFile({expression});\n\
             result;\n"
        );
        fs::write(&main, source).expect("write factorial importer");
        let output = run_cli(&[
            "--fuel".to_owned(),
            "100000".to_owned(),
            main.display().to_string(),
        ]);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        match case["outcome"].as_str() {
            Some("return") => {
                assert!(
                    output.status.success(),
                    "CLI factorial return for {expression} failed:\n{stdout}\n{stderr}"
                );
                let expected = case["value"].as_i64().expect("return vector is integral");
                assert!(
                    stdout.contains(&format!("Int({expected})")),
                    "CLI result for {expression} should be {expected}: {stdout}"
                );
            }
            Some("throw") => {
                assert!(!output.status.success(), "throw case {expression} returned");
                assert_eq!(case["error_name"], "Error");
                let expected = case["message"].as_str().expect("Node Error message");
                assert!(
                    stderr.contains(expected),
                    "CLI must preserve the Node Error message for {expression}: {stderr}"
                );
                assert!(
                    !stdout.contains("stack:"),
                    "throw must not produce a final return stack for {expression}: {stdout}"
                );
            }
            other => panic!("unexpected Node result {other:?} for {expression}"),
        }
    }
}

#[test]
fn imported_original_factorial_runs_actual_native_ssa_without_branch_stack_leak() {
    let scratch = ScratchTree::new();
    let main = factorial_project(&scratch.0);
    fs::write(
        &main,
        "import { factorial as factorialFromFile } from \"./maths/factorial.ts\";\nconst result: number = factorialFromFile(6);\nresult;\n",
    )
    .expect("write unchanged-file native importer");
    let output = run_cli(&[
        "--aot".to_owned(),
        "--aot-dump".to_owned(),
        "--fuel".to_owned(),
        "100000".to_owned(),
        main.display().to_string(),
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "original native importer failed: {stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("stack: [Int(720)]"),
        "actual native result: {stdout}"
    );
    assert!(
        stdout.contains("lowering=ssa"),
        "native lowering missing: {stdout}"
    );
    let count = stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("aot executions: "))
        .and_then(|raw| raw.parse::<u64>().ok())
        .expect("native execution counter is observable");
    assert!(
        count > 1,
        "definition-only/interpreter execution is insufficient: {stdout}"
    );
}

#[test]
fn original_factorial_native_matches_node_vectors_and_fresh_artifact_routes() {
    let vectors: serde_json::Value =
        serde_json::from_str(NODE_FACTORIAL_VECTORS).expect("Node vectors");
    let cases = vectors["cases"].as_array().expect("nine Node cases");
    assert_eq!(cases.len(), 9);
    let mut successful_returns = 0;
    let mut exact_throws = 0;
    for (index, case) in cases.iter().enumerate() {
        let scratch = ScratchTree::new();
        let main = factorial_project(&scratch.0);
        let expression = case["expression"].as_str().expect("Node expression");
        fs::write(&main, format!("import {{ factorial as factorialFromFile }} from \"./maths/factorial.ts\";\nconst result: number = factorialFromFile({expression});\nresult;\n")).expect("write original importer");
        let artifact = scratch.0.join(format!("original-{index}.pat"));
        let written = run_cli(&[
            "--aot-save".to_owned(),
            artifact.display().to_string(),
            "--aot-dump".to_owned(),
            "--fuel".to_owned(),
            "100000".to_owned(),
            main.display().to_string(),
        ]);
        let stdout = String::from_utf8_lossy(&written.stdout);
        let stderr = String::from_utf8_lossy(&written.stderr);
        // CLI diagnostics are emitted after successful execution only. On a
        // throw, inspect and run the saved SSA artifact with a retained VM too.
        assert!(
            artifact.is_file(),
            "AOT-save must compile before executing {expression}: {stderr}"
        );
        let mut retained =
            Vm::new_from_aot_artifact_file_with_jit_config(&artifact, vm::JitConfig::default())
                .expect("rematerialize original native program");
        let catalog =
            pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_catalog();
        let mut registry = HostFunctionRegistry::new();
        pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module()
            .install_from_catalog(&mut registry, &catalog)
            .expect("install exact typed throw schema");
        registry
            .bind_vm_cached(&mut retained)
            .expect("bind saved original imports");
        retained.set_fuel(100000);
        assert!(retained.dump_aot_info().contains("lowering=ssa"));
        let retained_outcome = retained.run();
        assert!(
            retained.aot_exec_count() > 1,
            "original function was not executed natively: {}",
            retained.dump_aot_info()
        );
        eprintln!(
            "original factorial retained expression={expression} native_count={}",
            retained.aot_exec_count()
        );
        if case["outcome"] == "throw" {
            exact_throws += 1;
            let message = case["message"].as_str().expect("exact Node Error message");
            match retained_outcome {
                Err(VmError::HostError(actual)) => assert_eq!(actual, message),
                other => panic!("native throw changed outcome for {expression}: {other:?}"),
            }
            assert!(
                !written.status.success(),
                "native throw returned for {expression}"
            );
            assert!(stderr.contains(message), "{stderr}");
            assert!(
                !stdout.contains("stack:"),
                "throw published a return stack: {stdout}"
            );
            fs::remove_file(&main).expect("remove throwing importer");
            fs::remove_file(scratch.0.join("maths/factorial.ts"))
                .expect("remove throwing original source copy");
            let fresh = run_cli(&[
                "--aot-load".to_owned(),
                artifact.display().to_string(),
                "--fuel".to_owned(),
                "100000".to_owned(),
            ]);
            assert!(!fresh.status.success(), "standalone native throw returned");
            assert!(String::from_utf8_lossy(&fresh.stderr).contains(message));
            continue;
        }
        assert!(matches!(retained_outcome, Ok(VmStatus::Halted)));
        successful_returns += 1;
        let expected = case["value"].as_i64().expect("Node integral return");
        assert_native_original_return(&written, expected, expression);
        assert!(
            artifact.is_file(),
            "native artifact writer must produce bytes"
        );
        let companion = run_cli(&[
            "--aot-load".to_owned(),
            artifact.display().to_string(),
            "--aot-dump".to_owned(),
            "--fuel".to_owned(),
            "100000".to_owned(),
            main.display().to_string(),
        ]);
        assert_native_original_return(&companion, expected, expression);
        fs::remove_file(&main).expect("remove importer before standalone load");
        fs::remove_file(scratch.0.join("maths/factorial.ts"))
            .expect("remove original source copy before standalone load");
        let standalone = run_cli(&[
            "--aot-load".to_owned(),
            artifact.display().to_string(),
            "--aot-dump".to_owned(),
            "--fuel".to_owned(),
            "100000".to_owned(),
        ]);
        assert_native_original_return(&standalone, expected, expression);
        let plain = run_cli(&[
            "--aot-load".to_owned(),
            artifact.display().to_string(),
            "--fuel".to_owned(),
            "100000".to_owned(),
        ]);
        assert!(
            plain.status.success(),
            "standalone no-dump failed: {}",
            String::from_utf8_lossy(&plain.stderr)
        );
        assert!(
            String::from_utf8_lossy(&plain.stdout).contains(&format!("stack: [Int({expected})]"))
        );
    }
    assert_eq!((successful_returns, exact_throws), (4, 5));
}

fn assert_native_original_return(output: &Output, expected: i64, expression: &str) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "original native {expression} failed: {stdout}\n{stderr}"
    );
    assert!(
        stdout.contains(&format!("stack: [Int({expected})]")),
        "{stdout}"
    );
    assert!(
        stdout.contains("lowering=ssa"),
        "native SSA evidence missing: {stdout}"
    );
    let count = stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("aot executions: "))
        .and_then(|raw| raw.parse::<u64>().ok())
        .expect("actual original native counter");
    assert!(
        count > 1,
        "native importer must invoke the algorithm: {stdout}"
    );
    eprintln!(
        "original factorial native expression={expression} result=Int({expected}) native_count={count}"
    );
}

#[test]
fn imported_module_alias_type_hints_match_prototype_and_runtime_call() {
    let scratch = ScratchTree::new();
    let main = factorial_project(&scratch.0);
    let source = "import { factorial as factorialFromFile } from \"./maths/factorial.ts\";\n\
                 const alias = factorialFromFile;\n\
                 const result = alias(6);\n\
                 result;\n";
    fs::write(&main, source).expect("write imported-alias TypeScript source");
    let expected = TypeSchema::Callable {
        params: vec![TypeSchema::Number],
        result: Box::new(TypeSchema::Number),
    };

    let hints = pd_vm_compat_frontends::collect_typescript_type_hints_at_path_with_options(
        &main,
        source,
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("path-based hints should resolve the imported module");
    let alias = hints
        .iter()
        .find(|hint| hint.name == "alias")
        .expect("imported function alias has a local hint");
    assert_eq!(alias.schema, Some(expected.clone()));
    assert_eq!(alias.inferred_type, "(number) => number");
    assert_eq!(
        hints
            .iter()
            .find(|hint| hint.name == "result")
            .expect("call result has a local hint")
            .schema,
        Some(TypeSchema::Number)
    );

    let compiled =
        compile_source_file_with_options(&main, pd_vm_compat_frontends::compile_options())
            .expect("the same importer should compile through the public source path");
    assert!(
        compiled
            .program
            .exported_callables
            .iter()
            .any(|callable| callable.name == "factorial"),
        "the exact upstream module still exports factorial"
    );
    assert!(
        compiled
            .program
            .callable_prototypes
            .iter()
            .any(|prototype| prototype.schema.as_ref() == Some(&expected)),
        "the exported callable prototype must carry the same number -> number schema"
    );
    let catalog = pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_catalog();
    let mut registry = HostFunctionRegistry::new();
    pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("bind exact scalar Error adapter for runtime proof");
    let mut machine = Vm::new(compiled.program);
    registry
        .bind_vm_cached(&mut machine)
        .expect("imported algorithm host imports are bound");
    assert_eq!(
        machine.run().expect("imported alias call executes"),
        VmStatus::Halted
    );
    assert_eq!(machine.stack(), &[Value::Int(720)]);
}

#[test]
fn published_services_rebuild_registry_and_initialize_before_each_guest_effect() {
    let scratch = ScratchTree::new();
    let main = factorial_project(&scratch.0);
    let source = "import { factorial as factorialFromFile } from \"./maths/factorial.ts\";\n\
                 import { after } from \"probe\";\n\
                 const result: number = factorialFromFile(6);\n\
                 after();\n\
                 result;\n";
    fs::write(&main, source).expect("write CLI importer with explicit after marker");
    let probe = probe_services();
    let runtime = probe_runtime();

    vm::cli::run_with_args(
        &runtime,
        &[
            "--fuel".to_owned(),
            "100000".to_owned(),
            main.display().to_string(),
        ],
        &probe.services,
    )
    .expect("first VM binds hosts and runs the importer");
    assert_probe_invocation(&probe, 0, 1);

    vm::cli::run_with_args(
        &runtime,
        &[
            "--fuel".to_owned(),
            "100000".to_owned(),
            "--debug".to_owned(),
            "--no-stop-on-entry".to_owned(),
            main.display().to_string(),
        ],
        &probe.services,
    )
    .expect("debug startup must bind and initialize a fresh VM");
    assert_probe_invocation(&probe, 1, 1);

    let recording = scratch.0.join("probe.pdr");
    vm::cli::run_with_args(
        &runtime,
        &[
            "--fuel".to_owned(),
            "100000".to_owned(),
            "--record".to_owned(),
            recording.display().to_string(),
            main.display().to_string(),
        ],
        &probe.services,
    )
    .expect("record startup must bind and initialize a fresh VM");
    let recording = vm::VmRecording::load_from_file(&recording).expect("load CLI recording");
    assert!(
        !recording.frames.is_empty(),
        "recording contains real VM frames"
    );
    assert_probe_invocation(&probe, 2, 1);

    let vectors: serde_json::Value =
        serde_json::from_str(NODE_FACTORIAL_VECTORS).expect("Node vectors are JSON");
    let negative = vectors["cases"]
        .as_array()
        .expect("Node cases are an array")
        .iter()
        .find(|case| case["expression"] == "-1")
        .expect("Node vector contains the negative factorial input");
    let expression = negative["expression"].as_str().unwrap();
    let throw_source = format!(
        "import {{ factorial as factorialFromFile }} from \"./maths/factorial.ts\";\n\
         import {{ after }} from \"probe\";\n\
         const result: number = factorialFromFile({expression});\n\
         after();\n\
         result;\n"
    );
    fs::write(&main, throw_source).expect("write throw followed by marker effect");
    let error = vm::cli::run_with_args(
        &runtime,
        &[
            "--fuel".to_owned(),
            "100000".to_owned(),
            main.display().to_string(),
        ],
        &probe.services,
    )
    .expect_err("throw must stop before the imported marker call");
    let expected = negative["message"].as_str().unwrap();
    assert!(error.to_string().contains(expected), "{error}");
    assert_probe_invocation(&probe, 3, 0);

    fs::write(&main, source).expect("restore successful imported source");
    let wire = scratch.0.join("probe.vmbc");
    vm::cli::run_with_args(
        &runtime,
        &[
            "--emit-vmbc".to_owned(),
            wire.display().to_string(),
            main.display().to_string(),
        ],
        &probe.services,
    )
    .expect("VMBC emission binds a fresh registry without executing guest effects");
    assert_probe_invocation(&probe, 4, 0);
    vm::cli::run_with_args(&runtime, &[wire.display().to_string()], &probe.services)
        .expect("VMBC load binds the same exact catalog and runs the import");
    assert_probe_invocation(&probe, 5, 1);

    let aot_source = scratch.0.join("probe-aot.ts");
    fs::write(
        &aot_source,
        "import { after } from \"probe\";\nafter();\n42;\n",
    )
    .expect("write simple AOT host-call source");
    let aot = scratch.0.join("probe.pat");
    vm::cli::run_with_args(
        &runtime,
        &[
            "--fuel".to_owned(),
            "100000".to_owned(),
            "--aot-save".to_owned(),
            aot.display().to_string(),
            aot_source.display().to_string(),
        ],
        &probe.services,
    )
    .expect("source AOT invokes the per-VM adapter");
    assert!(aot.is_file(), "source-owned native artifact was saved");
    assert_probe_invocation(&probe, 6, 1);
    vm::cli::run_with_args(
        &runtime,
        &[
            "--fuel".to_owned(),
            "100000".to_owned(),
            "--aot-load".to_owned(),
            aot.display().to_string(),
        ],
        &probe.services,
    )
    .expect("loaded AOT adapter call runs with a newly initialized VM");
    assert_probe_invocation(&probe, 7, 1);

    let aot_throw_source = scratch.0.join("probe-aot-throw.ts");
    fs::write(
        &aot_throw_source,
        "import { after } from \"probe\";\nthrow new Error(\"probe AOT error\");\nafter();\n",
    )
    .expect("write AOT error and following marker");
    let aot_throw = scratch.0.join("probe-throw.pat");
    let aot_error = vm::cli::run_with_args(
        &runtime,
        &[
            "--fuel".to_owned(),
            "100000".to_owned(),
            "--aot-save".to_owned(),
            aot_throw.display().to_string(),
            aot_throw_source.display().to_string(),
        ],
        &probe.services,
    )
    .expect_err("source AOT must preserve the scalar Error adapter failure");
    assert!(
        aot_error.to_string().contains("probe AOT error"),
        "{aot_error}"
    );
    assert_probe_invocation(&probe, 8, 0);
    let loaded_aot_error = vm::cli::run_with_args(
        &runtime,
        &[
            "--fuel".to_owned(),
            "100000".to_owned(),
            "--aot-load".to_owned(),
            aot_throw.display().to_string(),
        ],
        &probe.services,
    )
    .expect_err("loaded AOT must preserve the scalar Error adapter failure");
    assert!(
        loaded_aot_error.to_string().contains("probe AOT error"),
        "{loaded_aot_error}"
    );
    assert_probe_invocation(&probe, 9, 0);
}

#[test]
fn imported_factorial_returns_and_throws_through_vmbc_emit_load() {
    let scratch = ScratchTree::new();
    let main = factorial_project(&scratch.0);
    let vectors: serde_json::Value =
        serde_json::from_str(NODE_FACTORIAL_VECTORS).expect("Node vectors are JSON");
    let cases = vectors["cases"]
        .as_array()
        .expect("Node cases are an array");
    let error_case = cases
        .iter()
        .find(|case| case["expression"] == "-1")
        .expect("Node vector includes a throwing negative input");
    for expression in ["6", "-1"] {
        let source = format!(
            "import {{ factorial as factorialFromFile }} from \"./maths/factorial.ts\";\n\
             const result: number = factorialFromFile({expression});\n\
             result;\n"
        );
        fs::write(&main, source).expect("write imported factorial source");
        let artifact = scratch.0.join(format!("factorial-{expression}.vmbc"));
        let emitted = run_cli(&[
            "--emit-vmbc".to_owned(),
            artifact.display().to_string(),
            main.display().to_string(),
        ]);
        assert!(
            emitted.status.success(),
            "VMBC emission for {expression} failed: {}",
            String::from_utf8_lossy(&emitted.stderr)
        );
        assert!(artifact.is_file(), "VMBC output exists for {expression}");
        let loaded = run_cli(&[
            "--fuel".to_owned(),
            "100000".to_owned(),
            artifact.display().to_string(),
        ]);
        let stdout = String::from_utf8_lossy(&loaded.stdout);
        let stderr = String::from_utf8_lossy(&loaded.stderr);
        if expression == "6" {
            assert!(
                loaded.status.success(),
                "VMBC return failed: {stdout}\n{stderr}"
            );
            assert!(stdout.contains("stack: [Int(720)]"), "{stdout}");
        } else {
            assert!(!loaded.status.success(), "VMBC throw case returned");
            let expected = error_case["message"].as_str().expect("Node Error message");
            assert!(stderr.contains(expected), "{stderr}");
            assert!(
                !stdout.contains("stack:"),
                "throw produced final stack: {stdout}"
            );
        }
    }
}

#[test]
fn published_cli_executes_native_source_and_preserves_error_adapter_message() {
    let scratch = ScratchTree::new();
    let main = factorial_project(&scratch.0);
    let success_source = "const result: number = 1 + 2;\n\
                          result;\n";
    fs::write(&main, success_source).expect("write AOT success source");
    let success_artifact = scratch.0.join("scalar-success.pat");
    let saved = run_cli(&[
        "--aot-save".to_owned(),
        success_artifact.display().to_string(),
        "--aot-dump".to_owned(),
        "--fuel".to_owned(),
        "100000".to_owned(),
        main.display().to_string(),
    ]);
    let saved_stdout = String::from_utf8_lossy(&saved.stdout);
    let saved_stderr = String::from_utf8_lossy(&saved.stderr);
    assert!(
        saved.status.success(),
        "source AOT failed: {saved_stdout}\n{saved_stderr}"
    );
    assert!(saved_stdout.contains("stack: [Int(3)]"), "{saved_stdout}");
    assert!(
        saved_stdout.contains("whole-program aot: enabled"),
        "{saved_stdout}"
    );
    assert!(
        saved_stdout.contains("native codegen backend:"),
        "{saved_stdout}"
    );
    assert!(
        saved_stdout.contains("lowering=ssa"),
        "AOT did not report SSA lowering: {saved_stdout}"
    );
    let saved_executions = saved_stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("aot executions: "))
        .expect("AOT dump reports execution count")
        .parse::<u64>()
        .expect("AOT execution count is numeric");
    assert!(
        saved_executions > 0,
        "native AOT did not execute: {saved_stdout}"
    );
    assert!(success_artifact.is_file(), "source AOT artifact was saved");

    let expected = "compat AOT Error marker";
    let throw_source = "throw new Error(\"compat AOT Error marker\");\n";
    fs::write(&main, throw_source).expect("write AOT throw source");
    let throw_artifact = scratch.0.join("scalar-throw.pat");
    let thrown = run_cli(&[
        "--aot-save".to_owned(),
        throw_artifact.display().to_string(),
        "--fuel".to_owned(),
        "100000".to_owned(),
        main.display().to_string(),
    ]);
    assert_eq!(
        thrown.status.code(),
        Some(1),
        "AOT error must be a handled CLI failure, not a process fault"
    );
    let thrown_stderr = String::from_utf8_lossy(&thrown.stderr);
    assert!(thrown_stderr.contains(expected), "{thrown_stderr}");
    assert!(
        throw_artifact.is_file(),
        "throwing source AOT artifact was saved before execution"
    );
}

#[test]
fn supported_rss_repl_uses_published_services_and_continues_after_error() {
    let output = run_cli_with_stdin(
        &["repl".to_owned()],
        b"1 + 2\ntypescript::throw_error(\"repl marker\")\n40 + 2\n.quit\n",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "Core RSS REPL failed ({}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );
    assert!(stdout.contains("pd-vm REPL (RustScript)"), "{stdout}");
    assert!(
        stdout.contains("=> 3"),
        "first RSS snippet did not execute: {stdout}"
    );
    assert!(
        stdout.contains("repl marker"),
        "Error adapter was unavailable: {stdout}"
    );
    assert!(
        stdout.contains("=> 42"),
        "REPL did not continue after the Error: {stdout}"
    );
}

#[test]
fn published_services_refuse_conflicting_explicit_compiler_catalogs() {
    let scratch = ScratchTree::new();
    let entry = scratch.0.join("factorial.ts");
    fs::write(&entry, ORIGINAL_FACTORIAL).expect("write original entry");
    let runtime = vm::cli::CliRuntime {
        binary_name: "compat-catalog-authority-test",
        default_source: "unused.ts",
        compile_options: empty_catalog_options,
    };
    let services = pd_vm_compat_frontends::runtime::cli::typescript_cli_host_services();
    let error = vm::cli::run_with_args(&runtime, &[entry.display().to_string()], &services)
        .expect_err("explicit empty catalog conflicts with the published services catalog");
    assert!(
        error
            .to_string()
            .contains("CLI compiler catalog conflicts with host services"),
        "{error}"
    );
}

#[test]
fn empty_and_wrong_error_catalogs_do_not_satisfy_the_scalar_error_contract() {
    let scratch = ScratchTree::new();
    let entry = scratch.0.join("factorial.ts");
    fs::write(&entry, ORIGINAL_FACTORIAL).expect("write original entry");

    let empty = empty_host_catalog();
    let empty_services = vm::cli::CliHostServices::new(Arc::clone(&empty), || {
        Ok::<_, VmError>(HostFunctionRegistry::new())
    });
    let empty_runtime = vm::cli::CliRuntime {
        binary_name: "compat-empty-catalog-test",
        default_source: "unused.ts",
        compile_options: empty_catalog_options,
    };
    let empty_error = vm::cli::run_with_args(
        &empty_runtime,
        &[entry.display().to_string()],
        &empty_services,
    )
    .expect_err("empty catalog must not compile the Error adapter import");
    assert!(
        empty_error.to_string().contains(
            "requires the exact typescript::throw_error(message: string): null host catalog entry"
        ),
        "{empty_error}"
    );

    let wrong = wrong_error_catalog();
    let wrong_services = vm::cli::CliHostServices::new(Arc::clone(&wrong), || {
        Ok::<_, VmError>(HostFunctionRegistry::new())
    });
    let wrong_runtime = vm::cli::CliRuntime {
        binary_name: "compat-wrong-catalog-test",
        default_source: "unused.ts",
        compile_options: wrong_error_options,
    };
    let wrong_error = vm::cli::run_with_args(
        &wrong_runtime,
        &[entry.display().to_string()],
        &wrong_services,
    )
    .expect_err("same-name wrong-return catalog must not satisfy the scalar Error contract");
    assert!(
        wrong_error.to_string().contains(
            "requires the exact typescript::throw_error(message: string): null host catalog entry"
        ),
        "{wrong_error}"
    );
}

#[test]
fn exact_catalog_without_error_adapter_does_not_grant_host_authority() {
    let scratch = ScratchTree::new();
    let entry = scratch.0.join("factorial.ts");
    fs::write(&entry, ORIGINAL_FACTORIAL).expect("write original entry");
    let catalog = pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_catalog();
    let services =
        vm::cli::CliHostServices::new(catalog, || Ok::<_, VmError>(HostFunctionRegistry::new()));
    let runtime = vm::cli::CliRuntime {
        binary_name: "compat-unbound-authority-test",
        default_source: "unused.ts",
        compile_options: pd_vm_compat_frontends::compile_options,
    };
    let error = vm::cli::run_with_args(&runtime, &[entry.display().to_string()], &services)
        .expect_err("catalog metadata alone cannot install the Error host authority");
    assert!(
        error
            .to_string()
            .contains("unbound host import 'typescript::throw_error'"),
        "{error}"
    );
}

#[test]
fn production_services_do_not_authorize_shadowed_error_bindings() {
    let scratch = ScratchTree::new();
    let source = scratch.0.join("shadowed-error.ts");
    fs::write(
        &source,
        "const Error: string = \"shadow\";\nthrow new Error(\"message\");\n",
    )
    .expect("write shadowed TypeScript Error source");
    let output = run_cli(&[
        "--fuel".to_owned(),
        "100000".to_owned(),
        source.display().to_string(),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("TypeScript throw cannot use a shadowed Error binding"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
