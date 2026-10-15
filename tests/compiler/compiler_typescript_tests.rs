use std::fmt::Display;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use vm::{
    Value, Vm, VmStatus, compile_source_file_with_options, compile_source_with_flavor_and_options,
    compiler::{FrontendId, SourceFlavor},
};

fn compile(source: &str) -> Result<vm::CompiledProgram, vm::SourcePathError> {
    compile_source_with_flavor_and_options(
        source,
        SourceFlavor::Plugin(FrontendId::new("typescript")),
        pd_vm_compat_frontends::compile_options(),
    )
}

struct ProjectDir(PathBuf);

impl ProjectDir {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        loop {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!("typescript-modules-{}-{id}", std::process::id()));
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

struct ApplyHost(Arc<Mutex<Option<Value>>>);

impl vm::HostFunction for ApplyHost {
    fn call(&mut self, _machine: &mut Vm, args: &[Value]) -> vm::VmResult<vm::CallOutcome> {
        assert_eq!(args.len(), 1);
        *self.0.lock().expect("callback store lock") = Some(args[0].clone());
        Ok(vm::CallOutcome::Return(vm::CallReturn::one(Value::Int(0))))
    }
}

struct CountedHost(Arc<AtomicUsize>);

impl vm::HostFunction for CountedHost {
    fn call(&mut self, _machine: &mut Vm, args: &[Value]) -> vm::VmResult<vm::CallOutcome> {
        assert_eq!(args.len(), 1);
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(vm::CallOutcome::Return(vm::CallReturn::one(Value::Bool(
            true,
        ))))
    }
}

fn expect_error<T, E: Display>(result: Result<T, E>) -> E {
    match result {
        Ok(_) => panic!("source should be rejected"),
        Err(error) => error,
    }
}

#[test]
fn block_shadowing_keeps_outer_binding_visible_after_if() {
    let compiled =
        compile("let value: number = 1;\nif (true) { const value: number = 2; }\nvalue;\n")
            .expect("block-local shadowing should compile");
    let mut machine = Vm::new(compiled.program);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(machine.stack(), &[Value::Int(1)]);
}

#[test]
fn typed_for_loop_and_mutable_bindings_execute() {
    let compiled = compile(
        "let total: number = 0;\nfor (let i: number = 0; i < 3; i = i + 1) { total = total + i; }\ntotal;\n",
    )
    .expect("typed for loop should compile");
    let mut machine = Vm::new(compiled.program);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(machine.stack(), &[Value::Int(3)]);
}

#[test]
fn const_binding_reassignment_is_rejected() {
    let error = expect_error(compile("const total: number = 1;\ntotal = 2;\n"));
    assert!(
        error.to_string().contains("const binding"),
        "unexpected const assignment diagnostic: {error}"
    );
}

#[test]
fn annotated_function_declaration_calls_typed_local_arguments() {
    let compiled = compile(
        r#"function add(lhs: number, rhs: number): number { return lhs + rhs; }
const result: number = add(4, 5);
result;
"#,
    )
    .expect("annotated TypeScript function should compile");
    let mut machine = Vm::new(compiled.program);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(machine.stack(), &[Value::Int(9)]);
}

#[test]
fn function_parameter_without_context_is_rejected_as_implicit_any() {
    let error = expect_error(compile(
        r#"function echo(value) { return value; }
echo("x");
"#,
    ));
    assert!(error.to_string().contains("noImplicitAny"));
}

#[test]
fn function_return_schema_is_checked_against_its_body() {
    let error = expect_error(compile(
        r#"function bad(): string { return 123; }
bad();
"#,
    ));
    assert!(
        error.to_string().contains("declared to return"),
        "unexpected return diagnostic: {error}"
    );
}

#[test]
fn function_call_argument_schema_is_checked() {
    let error = expect_error(compile(
        r#"function echo(value: string): string { return value; }
echo(123);
"#,
    ));
    assert!(error.to_string().to_ascii_lowercase().contains("type"));
}

#[test]
fn imported_typescript_function_links_and_executes_with_its_declared_schema() {
    let project = ProjectDir::new();
    let library = project.0.join("lib.ts");
    let main = project.0.join("main.ts");
    fs::write(
        &library,
        r#"export function add(lhs: number, rhs: number): number { return lhs + rhs; }
"#,
    )
    .unwrap();
    fs::write(
        &main,
        r#"import { add } from "./lib.ts";
const result: number = add(4, 5);
result;
"#,
    )
    .unwrap();
    let compiled =
        compile_source_file_with_options(&main, pd_vm_compat_frontends::compile_options())
            .expect("TypeScript import should link");
    let mut machine = Vm::new(compiled.program);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(machine.stack(), &[Value::Int(9)]);
}

#[test]
fn namespace_typescript_function_import_links_and_executes() {
    let project = ProjectDir::new();
    let library = project.0.join("library.ts");
    let main = project.0.join("main.ts");
    fs::write(
        &library,
        r#"export function add(lhs: number, rhs: number): number { return lhs + rhs; }
"#,
    )
    .unwrap();
    fs::write(
        &main,
        r#"import * as arithmetic from "./library.ts";
arithmetic.add(3, 4);
"#,
    )
    .unwrap();
    let compiled =
        compile_source_file_with_options(&main, pd_vm_compat_frontends::compile_options())
            .expect("namespace TypeScript import should link");
    let mut machine = Vm::new(compiled.program);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(machine.stack(), &[Value::Int(7)]);
}

#[test]
fn imported_resource_intrinsic_resolves_only_declared_catalog_keys() {
    let key = vm::ResourceTypeKey::new("io.file").expect("valid resource key");
    let mut builder = vm::HostApiBuilder::new();
    builder.resource(vm::ResourceTypeSchema::new(key.clone(), "file handle"));
    builder.function(vm::HostFunctionSchema::with_return(
        "files::close",
        vec![vm::HostParamSchema::with_passing(
            "handle",
            vm::HostTypeSchema::Resource(key),
            vm::HostParamPassing::TakeOwned,
        )],
        vm::HostTypeSchema::Bool,
    ));
    let catalog = Arc::new(builder.build().expect("valid resource catalog"));
    let compiled = compile_source_with_flavor_and_options(
        "import type { Resource } from \"rustscript:types\";\nimport { close } from \"files\";\nfunction close_file(handle: Resource<\"io.file\">): boolean { return close(handle); }\n",
        SourceFlavor::Plugin(FrontendId::new("typescript")),
        pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("catalog-declared resource type should lower to its nominal key");
    assert!(
        compiled
            .program
            .imports
            .iter()
            .any(|import| import.name == "files::close")
    );
    let unknown_key = expect_error(compile_source_with_flavor_and_options(
        "import type { Resource } from \"rustscript:types\";\nimport { close } from \"files\";\nfunction bad(handle: Resource<\"io.socket\">): boolean { return close(handle); }\n",
        SourceFlavor::Plugin(FrontendId::new("typescript")),
        pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog)),
    ));
    assert!(
        unknown_key.to_string().contains("not declared"),
        "unexpected rejection for undeclared resource key: {unknown_key}"
    );
}

#[test]
fn resource_borrow_and_borrow_mut_keep_the_parameter_live() {
    let key = vm::ResourceTypeKey::new("io.file").expect("valid resource key");
    let mut builder = vm::HostApiBuilder::new();
    builder.resource(vm::ResourceTypeSchema::new(key.clone(), "file handle"));
    for (name, passing) in [
        ("files::borrow", vm::HostParamPassing::Borrow),
        ("files::borrow_mut", vm::HostParamPassing::BorrowMut),
    ] {
        builder.function(vm::HostFunctionSchema::with_return(
            name,
            vec![vm::HostParamSchema::with_passing(
                "handle",
                vm::HostTypeSchema::Resource(key.clone()),
                passing,
            )],
            vm::HostTypeSchema::Bool,
        ));
    }
    let catalog = Arc::new(builder.build().expect("valid borrow catalog"));
    let compiled = compile_source_with_flavor_and_options(
        "import type { Resource } from \"rustscript:types\";\nimport { borrow, borrow_mut } from \"files\";\nfunction valid(handle: Resource<\"io.file\">): boolean { borrow(handle); borrow_mut(handle); return borrow(handle); }\n",
        SourceFlavor::Plugin(FrontendId::new("typescript")),
        pd_vm_compat_frontends::compile_options_with_catalog(catalog),
    )
    .expect("shared and mutable borrows should end at each host call boundary");
    assert_eq!(compiled.program.imports.len(), 2);
}

#[test]
fn resource_take_owned_rejects_use_after_transfer() {
    let key = vm::ResourceTypeKey::new("io.file").expect("valid resource key");
    let mut builder = vm::HostApiBuilder::new();
    builder.resource(vm::ResourceTypeSchema::new(key.clone(), "file handle"));
    builder.function(vm::HostFunctionSchema::with_return(
        "files::take",
        vec![vm::HostParamSchema::with_passing(
            "handle",
            vm::HostTypeSchema::Resource(key),
            vm::HostParamPassing::TakeOwned,
        )],
        vm::HostTypeSchema::Bool,
    ));
    let catalog = Arc::new(builder.build().expect("valid ownership catalog"));
    let error = expect_error(compile_source_with_flavor_and_options(
        "import type { Resource } from \"rustscript:types\";\nimport { take } from \"files\";\nfunction invalid(handle: Resource<\"io.file\">): boolean { take(handle); return take(handle); }\n",
        SourceFlavor::Plugin(FrontendId::new("typescript")),
        pd_vm_compat_frontends::compile_options_with_catalog(catalog),
    ));
    assert!(error.to_string().contains("moved") || error.to_string().contains("consumed"));
}

#[test]
fn numeric_callback_return_cannot_narrow_to_host_int_result() {
    let callback = vm::HostTypeSchema::Callable {
        params: vec![vm::HostTypeSchema::Number],
        result: Box::new(vm::HostTypeSchema::Int),
    };
    let mut builder = vm::HostApiBuilder::new();
    builder.function(vm::HostFunctionSchema::with_return(
        "callbacks::apply",
        vec![vm::HostParamSchema::value("callback", callback)],
        vm::HostTypeSchema::Int,
    ));
    let catalog = Arc::new(builder.build().expect("valid callback catalog"));
    let error = expect_error(compile_source_with_flavor_and_options(
        "import { apply } from \"callbacks\";\nfunction bad(value: number): number { return value; }\napply(bad);\n",
        SourceFlavor::Plugin(FrontendId::new("typescript")),
        pd_vm_compat_frontends::compile_options_with_catalog(catalog),
    ));
    assert!(
        error.to_string().contains("numeric ABI")
            || error
                .to_string()
                .contains("declared as schema type 'int' but was assigned number"),
        "{error}"
    );
}

#[test]
fn catalog_callback_is_contextually_typed_and_runs_through_the_vm() {
    let callback = vm::HostTypeSchema::Callable {
        params: vec![vm::HostTypeSchema::Int],
        result: Box::new(vm::HostTypeSchema::Int),
    };
    let mut builder = vm::HostApiBuilder::new();
    builder.function(vm::HostFunctionSchema::with_return(
        "callbacks::apply",
        vec![vm::HostParamSchema::value("callback", callback)],
        vm::HostTypeSchema::Int,
    ));
    let catalog = Arc::new(builder.build().expect("valid callback catalog"));
    let compiled = compile_source_with_flavor_and_options(
        "import type { Int } from \"rustscript:types\";\nimport { apply } from \"callbacks\";\nconst offset: Int = 1;\nconst result: number = apply((value) => value + offset);\nresult;\n",
        SourceFlavor::Plugin(FrontendId::new("typescript")),
        pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("catalog callback parameter should contextually type the arrow");
    let schema = vm::catalog_import_schemas(&catalog, "callbacks::apply")
        .into_iter()
        .next()
        .expect("callback host schema");
    let callback_value = Arc::new(Mutex::new(None));
    let factory_callback = Arc::clone(&callback_value);
    let mut registry = vm::HostFunctionRegistry::new();
    registry
        .register_catalog(schema, move || {
            Box::new(ApplyHost(Arc::clone(&factory_callback)))
        })
        .unwrap();
    let mut machine = Vm::new(compiled.program);
    registry.bind_vm_cached(&mut machine).unwrap();
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(machine.stack(), &[Value::Int(0)]);
    let callback = callback_value
        .lock()
        .expect("callback store lock")
        .take()
        .expect("host should receive callback value");
    assert_eq!(
        machine
            .invoke_callable(callback, &[Value::Int(41)])
            .unwrap(),
        Value::Int(42)
    );
}

#[test]
fn imported_host_function_uses_the_supplied_catalog_and_executes() {
    let project = ProjectDir::new();
    let main = project.0.join("main.ts");
    fs::write(
        &main,
        r#"import { exists as path_exists } from "testing";
path_exists("probe");
"#,
    )
    .unwrap();
    let mut builder = vm::HostApiBuilder::new();
    builder.function(vm::HostFunctionSchema::with_return(
        "testing::exists",
        vec![vm::HostParamSchema::value(
            "path",
            vm::HostTypeSchema::String,
        )],
        vm::HostTypeSchema::Bool,
    ));
    let catalog = Arc::new(builder.build().expect("valid host catalog"));
    let options = pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog));
    let compiled = compile_source_file_with_options(&main, options)
        .expect("catalog-declared host function should compile");
    assert!(
        compiled
            .program
            .imports
            .iter()
            .any(|import| import.name == "testing::exists")
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let mut registry = vm::HostFunctionRegistry::new();
    let schema = vm::catalog_import_schemas(&catalog, "testing::exists")
        .into_iter()
        .next()
        .expect("catalog schema is compiled into the host import");
    let factory_calls = Arc::clone(&calls);
    registry
        .register_catalog(schema, move || {
            Box::new(CountedHost(Arc::clone(&factory_calls)))
        })
        .expect("host binding matches the supplied catalog");
    let mut machine = Vm::new(compiled.program);
    registry
        .bind_vm_cached(&mut machine)
        .expect("catalog host binding attaches to VM");
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(machine.stack(), &[Value::Bool(true)]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn host_type_only_import_preserves_catalog_named_identity() {
    let project = ProjectDir::new();
    let main = project.0.join("main.ts");
    fs::write(
        &main,
        "import type { Payload as HostPayload } from \"numeric\";\nimport { accept as take_payload } from \"numeric\";\nfunction forward(payload: HostPayload): boolean { return take_payload(payload); }\n",
    )
    .unwrap();
    let payload = vm::HostStructSchema::new(
        "Payload",
        vec![vm::HostStructField::new("value", vm::HostTypeSchema::Int)],
    );
    let mut builder = vm::HostApiBuilder::new();
    builder.named_struct(payload.clone());
    builder.function(vm::HostFunctionSchema::with_return(
        "numeric::accept",
        vec![vm::HostParamSchema::value("payload", payload.as_type())],
        vm::HostTypeSchema::Bool,
    ));
    let catalog = Arc::new(builder.build().expect("valid named host catalog"));
    let compiled = compile_source_file_with_options(
        &main,
        pd_vm_compat_frontends::compile_options_with_catalog(catalog),
    )
    .expect("host type-only import should use the supplied catalog");
    assert_eq!(
        compiled
            .program
            .imports
            .iter()
            .filter(|import| import.name == "numeric::accept")
            .count(),
        1
    );
}

#[test]
fn host_type_namespace_import_has_no_runtime_binding() {
    let project = ProjectDir::new();
    let main = project.0.join("main.ts");
    fs::write(
        &main,
        "import type * as host from \"numeric\";\nfunction forward(payload: host.Payload): boolean { return true; }\n",
    )
    .unwrap();
    let payload = vm::HostStructSchema::new(
        "Payload",
        vec![vm::HostStructField::new("value", vm::HostTypeSchema::Int)],
    );
    let mut builder = vm::HostApiBuilder::new();
    builder.named_struct(payload.clone());
    builder.function(vm::HostFunctionSchema::with_return(
        "numeric::accept",
        vec![vm::HostParamSchema::value("payload", payload.as_type())],
        vm::HostTypeSchema::Bool,
    ));
    let catalog = Arc::new(builder.build().expect("valid named host catalog"));
    let compiled = compile_source_file_with_options(
        &main,
        pd_vm_compat_frontends::compile_options_with_catalog(catalog),
    )
    .expect("host namespace type import should use catalog declarations");
    assert!(compiled.program.imports.is_empty());
}

#[test]
fn intrinsic_numeric_types_are_imported_by_symbol_identity() {
    let project = ProjectDir::new();
    let main = project.0.join("main.ts");
    fs::write(
        &main,
        "import type { Int as Whole, Float as Decimal, Bytes as Binary } from \"rustscript:types\";\ntype Int = string;\nfunction require_int(value: Whole): Whole { return value; }\nfunction require_float(value: Decimal): Decimal { return value; }\nfunction require_bytes(value: Binary): Binary { return value; }\nfunction keep_local_name(value: Int): Int { return value; }\n",
    )
    .unwrap();
    let compiled =
        compile_source_file_with_options(&main, pd_vm_compat_frontends::compile_options())
            .expect("intrinsic type imports should resolve by the imported symbol");
    assert!(compiled.program.imports.is_empty());
}

#[test]
fn number_schema_cannot_narrow_to_host_int_abi() {
    let project = ProjectDir::new();
    let main = project.0.join("main.ts");
    fs::write(
        &main,
        "import { accept as take_int } from \"numeric\";\nfunction forward(value: number): boolean { return take_int(value); }\n",
    )
    .unwrap();
    let mut builder = vm::HostApiBuilder::new();
    builder.function(vm::HostFunctionSchema::with_return(
        "numeric::accept",
        vec![vm::HostParamSchema::value("value", vm::HostTypeSchema::Int)],
        vm::HostTypeSchema::Bool,
    ));
    let catalog = Arc::new(builder.build().expect("valid numeric host catalog"));
    let error = expect_error(compile_source_file_with_options(
        &main,
        pd_vm_compat_frontends::compile_options_with_catalog(catalog),
    ));
    assert!(
        error.to_string().contains("numeric ABI"),
        "expected concrete ABI diagnostic, got: {error}"
    );
}

#[test]
fn nested_number_schema_cannot_narrow_to_host_named_int_field() {
    let project = ProjectDir::new();
    let main = project.0.join("main.ts");
    fs::write(
        &main,
        "import { accept as take_payload } from \"numeric\";\nfunction forward(payload: { value: number }): boolean { return take_payload(payload); }\n",
    )
    .unwrap();
    let payload = vm::HostStructSchema::new(
        "Payload",
        vec![vm::HostStructField::new("value", vm::HostTypeSchema::Int)],
    );
    let mut builder = vm::HostApiBuilder::new();
    builder.named_struct(payload.clone());
    builder.function(vm::HostFunctionSchema::with_return(
        "numeric::accept",
        vec![vm::HostParamSchema::value("payload", payload.as_type())],
        vm::HostTypeSchema::Bool,
    ));
    let catalog = Arc::new(builder.build().expect("valid aggregate host catalog"));
    let error = expect_error(compile_source_file_with_options(
        &main,
        pd_vm_compat_frontends::compile_options_with_catalog(catalog),
    ));
    assert!(
        error.to_string().contains("numeric ABI"),
        "expected nested ABI diagnostic, got: {error}"
    );
}

#[test]
fn namespace_host_import_resolves_from_catalog_and_runs_registered_binding() {
    let project = ProjectDir::new();
    let main = project.0.join("main.ts");
    fs::write(
        &main,
        r#"import * as tools from "testing";
tools.exists("probe");
"#,
    )
    .unwrap();
    let mut builder = vm::HostApiBuilder::new();
    builder.function(vm::HostFunctionSchema::with_return(
        "testing::exists",
        vec![vm::HostParamSchema::value(
            "path",
            vm::HostTypeSchema::String,
        )],
        vm::HostTypeSchema::Bool,
    ));
    let catalog = Arc::new(builder.build().expect("valid host catalog"));
    let compiled = compile_source_file_with_options(
        &main,
        pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("host namespace is catalog-validated");
    let calls = Arc::new(AtomicUsize::new(0));
    let mut registry = vm::HostFunctionRegistry::new();
    let schema = vm::catalog_import_schemas(&catalog, "testing::exists")
        .into_iter()
        .next()
        .expect("catalog schema exists");
    let factory_calls = Arc::clone(&calls);
    registry
        .register_catalog(schema, move || {
            Box::new(CountedHost(Arc::clone(&factory_calls)))
        })
        .unwrap();
    let mut machine = Vm::new(compiled.program);
    registry.bind_vm_cached(&mut machine).unwrap();
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(machine.stack(), &[Value::Bool(true)]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn type_only_typescript_import_exports_nominal_schema_without_runtime_imports() {
    let project = ProjectDir::new();
    let types = project.0.join("types.ts");
    let main = project.0.join("main.ts");
    fs::write(
        &types,
        r#"export interface Profile {
  name: string;
  age?: number;
}
"#,
    )
    .unwrap();
    fs::write(
        &main,
        r#"import type { Profile } from "./types.ts";
function maybe(profile: Profile | null): Profile | null { return null; }
"#,
    )
    .unwrap();
    let compiled =
        compile_source_file_with_options(&main, pd_vm_compat_frontends::compile_options())
            .expect("imported nominal interface type should be checked");
    assert!(compiled.program.imports.is_empty());
    let mut machine = Vm::new(compiled.program);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
}

#[test]
fn local_parameter_shadows_imported_namespace_for_calls() {
    let project = ProjectDir::new();
    let module = project.0.join("math.ts");
    let main = project.0.join("main.ts");
    fs::write(
        &module,
        "export function twice(value: number): number { return value * 2; }\n",
    )
    .unwrap();
    fs::write(
        &main,
        "import * as math from \"./math.ts\";\nfunction shadowed(math: number): number { return math.twice(2); }\nshadowed(3);\n",
    )
    .unwrap();
    let error = expect_error(compile_source_file_with_options(
        &main,
        pd_vm_compat_frontends::compile_options(),
    ));
    assert!(error.to_string().contains("namespace"));
}

#[test]
fn same_named_types_from_distinct_modules_keep_distinct_identity() {
    let project = ProjectDir::new();
    let left = project.0.join("left.ts");
    let right = project.0.join("right.ts");
    let main = project.0.join("main.ts");
    fs::write(&left, "export interface Shared { left: string; }\n").unwrap();
    fs::write(
        &right,
        "export interface Shared { right: number; }\nexport function take(value: Shared): number { return 1; }\n",
    )
    .unwrap();
    fs::write(
        &main,
        r#"import type { Shared as LeftShared } from "./left.ts";
import { take } from "./right.ts";
function preserve(value: LeftShared): LeftShared { return value; }
function cross(value: LeftShared): number { return take(value); }
"#,
    )
    .unwrap();
    let result = compile_source_file_with_options(&main, pd_vm_compat_frontends::compile_options());
    let error = result
        .err()
        .expect("same-named guest types from different modules must remain distinct");
    assert!(
        error.to_string().contains("field 'right' is required"),
        "unexpected module type diagnostic: {error}"
    );
}

#[test]
fn generic_typescript_identity_preserves_instantiated_argument_type() {
    let compiled = compile(
        "function identity<T>(value: T): T { return value; }\nconst result: number = identity(7);\nresult;\n",
    )
    .expect("generic type parameter should remain in checked IR");
    let mut machine = Vm::new(compiled.program);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(machine.stack(), &[Value::Int(7)]);
}

#[test]
fn generic_type_aliases_link_through_type_only_imports() {
    let project = ProjectDir::new();
    let types = project.0.join("types.ts");
    let main = project.0.join("main.ts");
    fs::write(
        &types,
        "export type Result<T> = T | null;\nexport type Display = string;\nexport type Fields = { name: string };\n",
    )
    .unwrap();
    fs::write(
        &main,
        r#"import type { Result as Outcome, Display as Label, Fields } from "./types.ts";
function use(outcome: Outcome<string>, label: Label, fields: Fields): Label { return label; }
"#,
    )
    .unwrap();
    let compiled =
        compile_source_file_with_options(&main, pd_vm_compat_frontends::compile_options())
            .expect("generic and object aliases must survive type-only module linkage");
    let mut machine = Vm::new(compiled.program);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert!(machine.stack().is_empty());
}

#[test]
fn annotated_string_declaration_is_checked_and_executes() {
    let compiled = compile(
        r#"const name: string = "Ada";
name;
"#,
    )
    .expect("valid annotated TypeScript should compile");
    let mut machine = Vm::new(compiled.program);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(machine.stack(), &[Value::string("Ada")]);
}

#[test]
fn incompatible_annotation_is_rejected_before_execution() {
    let error = expect_error(compile(
        r#"const name: string = 7;
name;
"#,
    ));
    assert!(error.to_string().to_ascii_lowercase().contains("type"));
}

#[test]
fn any_annotation_is_rejected_with_its_source_span() {
    let error = expect_error(compile("const item: any = \"value\";\n"));
    let rendered = error.to_string();
    assert!(rendered.contains("unsupported TypeScript type annotation"));
    assert!(
        rendered.contains("[12..15]"),
        "unexpected source span: {rendered}"
    );
}

#[test]
fn nested_typescript_modules_keep_relative_type_ownership_and_execute() {
    let project = ProjectDir::new();
    let nested = project.0.join("nested");
    fs::create_dir_all(&nested).expect("nested module directory");
    fs::write(
        nested.join("types.ts"),
        "export interface Profile { name: string; }\n",
    )
    .unwrap();
    fs::write(
        nested.join("label.ts"),
        "import type { Profile } from \"./types.ts\";\nexport function label(profile: Profile): string { return profile.name; }\n",
    )
    .unwrap();
    let main = project.0.join("main.ts");
    fs::write(
        &main,
        "import type { Profile } from \"./nested/types.ts\";\nimport { label } from \"./nested/label.ts\";\nconst profile: Profile = { name: \"Ada\" };\nlabel(profile);\n",
    )
    .unwrap();

    let compiled =
        compile_source_file_with_options(&main, pd_vm_compat_frontends::compile_options())
            .expect("nested module dependencies should resolve against their own directory");
    let mut machine = Vm::new(compiled.program);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(machine.stack(), &[Value::string("Ada")]);
}

#[test]
fn typescript_import_rejects_javascript_dependencies() {
    let project = ProjectDir::new();
    for extension in ["js", "mjs", "cjs"] {
        let dependency = project.0.join(format!("dependency.{extension}"));
        let main = project.0.join("main.ts");
        fs::write(
            &dependency,
            "export function value(): number { return 7; }\n",
        )
        .unwrap();
        fs::write(
            &main,
            format!("import {{ value }} from \"./dependency.{extension}\";\nvalue();\n"),
        )
        .unwrap();

        let error = expect_error(compile_source_file_with_options(
            &main,
            pd_vm_compat_frontends::compile_options(),
        ));
        assert!(
            error.to_string().contains(&format!(".{extension}")),
            "JavaScript dependency diagnostic should identify .{extension}: {error}"
        );
    }
}

#[test]
fn typescript_to_rss_import_preserves_the_exported_callable_contract() {
    let project = ProjectDir::new();
    fs::write(
        project.0.join("strings.rss"),
        "pub fn non_empty(value: string) -> bool { value.length != 0; }\n",
    )
    .unwrap();
    let main = project.0.join("main.ts");
    fs::write(
        &main,
        "import { non_empty } from \"./strings.rss\";\nconst result: boolean = non_empty(\"Ada\");\nresult;\n",
    )
    .unwrap();

    let compiled =
        compile_source_file_with_options(&main, pd_vm_compat_frontends::compile_options())
            .expect("TypeScript should call typed RSS exports");
    let mut machine = Vm::new(compiled.program);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(machine.stack(), &[Value::Bool(true)]);
}

#[test]
fn cyclic_typescript_module_graph_is_rejected() {
    let project = ProjectDir::new();
    let left = project.0.join("left.ts");
    let right = project.0.join("right.ts");
    let main = project.0.join("main.ts");
    fs::write(
        &left,
        "import { right } from \"./right.ts\";\nexport function left(value: string): string { return right(value); }\n",
    )
    .unwrap();
    fs::write(
        &right,
        "import { left } from \"./left.ts\";\nexport function right(value: string): string { return left(value); }\n",
    )
    .unwrap();
    fs::write(
        &main,
        "import { left } from \"./left.ts\";\nleft(\"Ada\");\n",
    )
    .unwrap();

    let error = expect_error(compile_source_file_with_options(
        &main,
        pd_vm_compat_frontends::compile_options(),
    ));
    assert!(
        error.to_string().to_ascii_lowercase().contains("cycle")
            || error.to_string().to_ascii_lowercase().contains("circular"),
        "cycle diagnostic should identify the module cycle: {error}"
    );
}
