use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use vm::{
    CallOutcome, CallReturn, HostApiCatalog, HostFunctionRegistry, HostFunctionSchema,
    HostParamSchema, HostTypeSchema, TypeSchema, Value, Vm, VmError, VmResult, VmStatus,
    compile_source_file_with_options,
};

const ORIGINAL_FACTORIAL: &[u8] = include_bytes!("fixtures/typescript/factorial.ts");
const NODE_FACTORIAL_VECTORS: &str =
    include_str!("fixtures/typescript/factorial-node-vectors.json");

struct ProjectDir(PathBuf);

impl ProjectDir {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        loop {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!(
                "typescript-factorial-upstream-{}-{id}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create test project {}: {error}", path.display()),
            }
        }
    }
}

impl Drop for ProjectDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct CountAfter(Arc<AtomicUsize>);

impl vm::HostFunction for CountAfter {
    fn call(&mut self, _machine: &mut Vm, args: &[Value]) -> VmResult<CallOutcome> {
        assert!(args.is_empty());
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(CallOutcome::Return(CallReturn::one(Value::Null)))
    }
}

fn project_with_factorial(main_source: Option<&str>) -> (ProjectDir, PathBuf) {
    let project = ProjectDir::new();
    let maths = project.0.join("maths");
    fs::create_dir_all(&maths).expect("create source directory");
    let factorial = maths.join("factorial.ts");
    fs::write(&factorial, ORIGINAL_FACTORIAL).expect("write exact upstream source");
    assert_eq!(
        fs::read(&factorial).expect("read copied source"),
        ORIGINAL_FACTORIAL,
        "acceptance source must remain byte-identical to pinned upstream"
    );
    if let Some(main_source) = main_source {
        fs::write(project.0.join("main.ts"), main_source).expect("write importer source");
    }
    (project, factorial)
}

fn catalog_with_error_and_after() -> (
    Arc<HostApiCatalog>,
    vm::HostModuleDescriptor,
    vm::HostImportSchema,
) {
    let error_module =
        pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module();
    let error_catalog = error_module
        .catalog()
        .expect("the scalar Error module should have a valid catalog");
    let mut builder = vm::HostApiBuilder::new();
    for function in error_catalog.functions() {
        builder.function(function.clone());
    }
    let after_schema = HostFunctionSchema::with_return(
        "proof::after",
        Vec::<HostParamSchema>::new(),
        HostTypeSchema::Null,
    );
    builder.function(after_schema.clone());
    let catalog = Arc::new(builder.build().expect("combined proof catalog is valid"));
    let after_import = vm::catalog_import_schemas(&catalog, "proof::after")
        .into_iter()
        .next()
        .expect("after binding is present in the exact catalog");
    (catalog, error_module, after_import)
}

#[test]
fn original_upstream_factorial_file_is_accepted_as_an_entry() {
    let (project, factorial) = project_with_factorial(None);
    let error_module =
        pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module();
    let catalog = pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_catalog();
    let mut registry = HostFunctionRegistry::new();
    error_module
        .install_from_catalog(&mut registry, &catalog)
        .expect("the Error adapter should bind from its exact catalog");

    let source = std::str::from_utf8(ORIGINAL_FACTORIAL).expect("pinned TypeScript is UTF-8");
    let hints = pd_vm_compat_frontends::collect_typescript_type_hints_with_options(
        source,
        pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("the original file should retain typed-hint analysis");
    assert!(
        !hints.is_empty(),
        "the original source should produce at least one typed local hint: {hints:?}"
    );

    let compiled = compile_source_file_with_options(
        &factorial,
        pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("the exact upstream file should compile as an entry");
    assert!(
        compiled
            .program
            .callable_prototypes
            .iter()
            .any(|prototype| {
                prototype.schema.as_ref()
                    == Some(&TypeSchema::Callable {
                        params: vec![TypeSchema::Number],
                        result: Box::new(TypeSchema::Number),
                    })
            }),
        "original export keeps its annotated number-to-number callable schema"
    );
    assert!(
        compiled
            .program
            .exported_callables
            .iter()
            .any(|callable| callable.name == "factorial"),
        "the original exported factorial callable should remain visible"
    );

    let mut machine = Vm::new(compiled.program);
    registry
        .bind_vm_cached(&mut machine)
        .expect("the exact Error adapter should bind to the VM");
    assert_eq!(
        machine.run().expect("entry should execute"),
        VmStatus::Halted
    );
    drop(project);
}

#[test]
fn custom_catalog_must_explicitly_authorize_the_error_adapter() {
    let (_project, factorial) = project_with_factorial(None);
    let catalog = Arc::new(
        vm::HostApiBuilder::new()
            .build()
            .expect("empty caller-owned host catalog is valid"),
    );
    let result = compile_source_file_with_options(
        &factorial,
        pd_vm_compat_frontends::compile_options_with_catalog(catalog),
    );
    let error = match result {
        Ok(_) => panic!("custom catalog without Error must not silently authorize throw"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains(
            "requires the exact typescript::throw_error(message: string): null host catalog entry"
        ),
        "unexpected catalog admission error: {error}"
    );
}

#[test]
fn scalar_error_shadow_and_invalid_arguments_keep_specific_rejections() {
    let cases = [
        (
            "shadowed Error binding",
            "const Error: string = \"shadow\";\nthrow new Error(\"message\");\n",
            "TypeScript throw cannot use a shadowed Error binding",
        ),
        (
            "missing message",
            "throw new Error();\n",
            "TypeScript throw new Error requires one string-typed message",
        ),
        (
            "extra message",
            "throw new Error(\"first\", \"second\");\n",
            "TypeScript throw new Error requires one string-typed message",
        ),
        (
            "spread message",
            "const messages: string[] = [\"message\"];\nthrow new Error(...messages);\n",
            "TypeScript throw new Error requires one string-typed message",
        ),
        (
            "non-string message",
            "const message: number = 42;\nthrow new Error(message);\n",
            "TypeScript throw new Error requires one string-typed message",
        ),
    ];

    for (label, source, expected_diagnostic) in cases {
        let result = vm::compile_source_with_flavor_and_options(
            source,
            vm::compiler::SourceFlavor::Plugin(vm::compiler::FrontendId::new("typescript")),
            pd_vm_compat_frontends::compile_options(),
        );
        let error = match result {
            Ok(_) => panic!("{label} should be rejected during TypeScript lowering"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains(expected_diagnostic),
            "{label} should produce its specific Error diagnostic, got: {error}"
        );
    }
}

#[test]
fn imported_original_factorial_matches_all_recorded_node_cases() {
    let vectors: serde_json::Value =
        serde_json::from_str(NODE_FACTORIAL_VECTORS).expect("recorded Node vectors are JSON");
    assert_eq!(vectors["node"], "v26.7.0");
    assert_eq!(
        vectors["upstream_commit"],
        "19b4ced86c99815f142d4a46a028f55487b8038a"
    );
    let cases = vectors["cases"]
        .as_array()
        .expect("Node packet contains factorial cases");
    assert_eq!(cases.len(), 9);

    let (catalog, error_module, after_schema) = catalog_with_error_and_after();
    for case in cases {
        let expression = case["expression"]
            .as_str()
            .expect("each Node input has a normalized TypeScript expression");
        let main_source = format!(
            "import {{ factorial as factorialFromFile }} from \"./maths/factorial.ts\";\n\
             import {{ after }} from \"proof\";\n\
             const result: number = factorialFromFile({expression});\n\
             after();\n\
             result;\n"
        );
        let (project, _) = project_with_factorial(Some(&main_source));
        let main = project.0.join("main.ts");
        let compiled = compile_source_file_with_options(
            &main,
            pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog)),
        )
        .unwrap_or_else(|error| {
            panic!("original factorial import for {expression} should compile: {error}")
        });

        let calls = Arc::new(AtomicUsize::new(0));
        let mut registry = HostFunctionRegistry::new();
        error_module
            .install_from_catalog(&mut registry, &catalog)
            .expect("Error adapter installs from the exact combined catalog");
        let factory_calls = Arc::clone(&calls);
        registry
            .register_catalog(after_schema.clone(), move || {
                Box::new(CountAfter(Arc::clone(&factory_calls)))
            })
            .expect("post-call counter binding matches the catalog");
        let mut machine = Vm::new(compiled.program);
        registry
            .bind_vm_cached(&mut machine)
            .expect("imported Error and proof adapters bind");
        let outcome = machine.run();
        match case["outcome"].as_str() {
            Some("return") => {
                assert_eq!(
                    outcome.unwrap_or_else(|error| {
                        panic!("Node return for {expression} should halt: {error}")
                    }),
                    VmStatus::Halted
                );
                let expected = case["value"]
                    .as_i64()
                    .expect("Node return value is integral");
                assert_eq!(machine.stack().last(), Some(&Value::Int(expected)));
                assert_eq!(
                    calls.load(Ordering::SeqCst),
                    1,
                    "after-call should run once"
                );
            }
            Some("throw") => {
                assert_eq!(case["error_name"], "Error");
                let expected = case["message"]
                    .as_str()
                    .expect("Node Error case records its message");
                assert!(
                    matches!(outcome, Err(VmError::HostError(ref message)) if message == expected),
                    "Error message for {expression}: {outcome:?}"
                );
                assert_eq!(
                    calls.load(Ordering::SeqCst),
                    0,
                    "throw must stop later effects"
                );
            }
            other => panic!("unexpected Node outcome {other:?} for {expression}"),
        }
        drop(project);
    }
}
