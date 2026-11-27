use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use vm::{
    SourcePosition, TypeSchema, Value, Vm, VmStatus, analyze_source_from_string_with_options,
    compile_source_with_flavor_and_options,
};

fn profile_value(name: &str) -> Value {
    Value::map(vec![
        (Value::string("name"), Value::string(name)),
        (Value::string("age"), Value::Int(37)),
    ])
}

struct LoadProfile;

impl vm::HostFunction for LoadProfile {
    fn call(&mut self, _machine: &mut Vm, args: &[Value]) -> vm::VmResult<vm::CallOutcome> {
        assert_eq!(args, &[Value::string("Ada")]);
        Ok(vm::CallOutcome::Return(vm::CallReturn::one(profile_value(
            "Ada",
        ))))
    }
}

struct OnProfile(Arc<Mutex<Option<Value>>>);

impl vm::HostFunction for OnProfile {
    fn call(&mut self, _machine: &mut Vm, args: &[Value]) -> vm::VmResult<vm::CallOutcome> {
        assert_eq!(args.len(), 1);
        *self.0.lock().expect("callback store lock") = Some(args[0].clone());
        Ok(vm::CallOutcome::Return(vm::CallReturn::one(Value::Bool(
            false,
        ))))
    }
}

struct ChooseByType;

impl vm::HostFunction for ChooseByType {
    fn call(&mut self, _machine: &mut Vm, args: &[Value]) -> vm::VmResult<vm::CallOutcome> {
        let label = match args.first() {
            Some(Value::Int(_)) => "int",
            Some(Value::String(_)) => "string",
            other => panic!("unexpected overload argument {other:?}"),
        };
        Ok(vm::CallOutcome::Return(vm::CallReturn::one(Value::string(
            label,
        ))))
    }
}

fn profile_catalog() -> Arc<vm::HostApiCatalog> {
    let profile = vm::HostStructSchema::new(
        "Profile",
        vec![
            vm::HostStructField::new("name", vm::HostTypeSchema::String),
            vm::HostStructField::new("age", vm::HostTypeSchema::Int),
        ],
    );
    let callback = vm::HostTypeSchema::Callable {
        params: vec![profile.as_type()],
        result: Box::new(vm::HostTypeSchema::Bool),
    };
    let mut builder = vm::HostApiBuilder::new();
    builder.named_struct(profile.clone());
    builder.function(vm::HostFunctionSchema::with_return(
        "profile::load",
        vec![vm::HostParamSchema::value(
            "name",
            vm::HostTypeSchema::String,
        )],
        profile.as_type(),
    ));
    builder.function(vm::HostFunctionSchema::with_return(
        "profile::onProfile",
        vec![vm::HostParamSchema::value("callback", callback)],
        vm::HostTypeSchema::Bool,
    ));
    Arc::new(builder.build().expect("valid profile host catalog"))
}

#[test]
fn host_named_result_and_callback_flow_through_hints_and_vm() {
    let source = r#"import { load, onProfile } from "profile";
import type { Profile } from "profile";
const result = load("Ada");
const named: Profile = result;
const name = result.name;
const accepted = onProfile((value) => value.age > 20);
name;
accepted;
"#;
    let catalog = profile_catalog();
    let hints = pd_vm_compat_frontends::collect_typescript_type_hints_with_options(
        source,
        pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("hinting uses the same authoritative host catalog");
    let hint = |name: &str| {
        hints
            .iter()
            .find(|hint| hint.name == name)
            .unwrap_or_else(|| panic!("missing hint for {name}"))
    };
    assert_eq!(
        hint("result").schema,
        Some(TypeSchema::Named("Profile".to_owned(), Vec::new()))
    );
    assert_eq!(hint("result").inferred_type, "Profile");
    assert_eq!(hint("name").schema, Some(TypeSchema::String));
    assert_eq!(
        hint("value").schema,
        Some(TypeSchema::Named("Profile".to_owned(), Vec::new()))
    );

    let compiled = compile_source_with_flavor_and_options(
        source,
        pd_vm_compat_frontends::typescript_flavor(),
        pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("catalog named type and callback should compile");
    assert!(
        compiled
            .program
            .imports
            .iter()
            .any(|item| item.name == "profile::load")
    );
    assert!(
        compiled
            .program
            .imports
            .iter()
            .any(|item| item.name == "profile::onProfile")
    );
    let mut registry = vm::HostFunctionRegistry::new();
    let load_schema = vm::catalog_import_schemas(&catalog, "profile::load")
        .into_iter()
        .next()
        .expect("load binding schema comes from the catalog");
    registry
        .register_catalog(load_schema, || Box::new(LoadProfile))
        .expect("load binding matches the catalog");
    let callback_schema = vm::catalog_import_schemas(&catalog, "profile::onProfile")
        .into_iter()
        .next()
        .expect("callback binding schema comes from the catalog");
    let callback_value = Arc::new(Mutex::new(None));
    let factory_callback = Arc::clone(&callback_value);
    registry
        .register_catalog(callback_schema, move || {
            Box::new(OnProfile(Arc::clone(&factory_callback)))
        })
        .expect("callback binding matches the catalog");
    let mut machine = Vm::new(compiled.program);
    registry.bind_vm_cached(&mut machine).unwrap();
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(machine.stack(), &[Value::string("Ada"), Value::Bool(false)]);
    let callback = callback_value
        .lock()
        .expect("callback store lock")
        .take()
        .expect("host receives the typed callback value");
    assert_eq!(
        machine
            .invoke_callable(callback, &[profile_value("Ada")])
            .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn invalid_host_arguments_fail_before_any_host_binding_can_run() {
    let catalog = profile_catalog();
    let result = compile_source_with_flavor_and_options(
        "import { load } from \"profile\";\nload(7);\n",
        pd_vm_compat_frontends::typescript_flavor(),
        pd_vm_compat_frontends::compile_options_with_catalog(catalog),
    );
    let error = match result {
        Ok(_) => panic!("host argument type must be checked during compile"),
        Err(error) => error,
    };
    let rendered = error.to_string();
    assert!(
        !rendered.is_empty(),
        "compile-time host rejection should have a diagnostic"
    );
}

#[test]
fn declared_host_function_is_catalog_checked_hinted_and_executable() {
    let source = r#"import type { Profile } from "profile";
declare function load(name: string): Profile;
const result = load("Ada");
const name = result.name;
name;
"#;
    let catalog = profile_catalog();
    let hints = pd_vm_compat_frontends::collect_typescript_type_hints_with_options(
        source,
        pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("declare function must be checked against the supplied host catalog");
    let hint = |name: &str| {
        hints
            .iter()
            .find(|hint| hint.name == name)
            .unwrap_or_else(|| panic!("missing hint for {name}"))
    };
    assert_eq!(
        hint("result").schema,
        Some(TypeSchema::Named("Profile".to_owned(), Vec::new()))
    );
    assert_eq!(hint("name").schema, Some(TypeSchema::String));

    let model = analyze_source_from_string_with_options(
        "main.ts",
        source,
        pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("semantic analysis must use the same catalog-backed declaration");
    let result_offset = source.find("result =").expect("result binding");
    assert_eq!(
        model.inferred_schema_at(SourcePosition::new(0, result_offset)),
        Some(TypeSchema::Named("Profile".to_owned(), Vec::new()))
    );

    let compiled = compile_source_with_flavor_and_options(
        source,
        pd_vm_compat_frontends::typescript_flavor(),
        pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("catalog declaration should compile to the host import");
    assert!(compiled.program.script_functions.is_empty());
    let load_schema = vm::catalog_import_schemas(&catalog, "profile::load")
        .into_iter()
        .next()
        .expect("catalog load schema");
    assert!(
        compiled
            .program
            .host_import_schemas()
            .iter()
            .flatten()
            .any(|schema| { schema == &load_schema })
    );
    let mut registry = vm::HostFunctionRegistry::new();
    registry
        .register_catalog(load_schema, || Box::new(LoadProfile))
        .expect("the VM binding uses the authoritative catalog schema");
    let mut machine = Vm::new(compiled.program);
    registry.bind_vm_cached(&mut machine).unwrap();
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(machine.stack(), &[Value::string("Ada")]);
}

#[test]
fn declared_host_overloads_preserve_catalog_candidates_by_type() {
    let mut builder = vm::HostApiBuilder::new();
    for parameter in [vm::HostTypeSchema::Int, vm::HostTypeSchema::String] {
        builder.function(vm::HostFunctionSchema::with_return(
            "tools::choose",
            vec![vm::HostParamSchema::value("value", parameter)],
            vm::HostTypeSchema::String,
        ));
    }
    let catalog = Arc::new(builder.build().expect("valid overloaded host catalog"));
    let source = r#"import type { Int } from "rustscript:types";
declare function choose(value: Int): string;
declare function choose(value: string): string;
const integer = choose(7);
const text = choose("seven");
integer;
text;
"#;
    let compiled = compile_source_with_flavor_and_options(
        source,
        pd_vm_compat_frontends::typescript_flavor(),
        pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("every declaration overload maps to an existing catalog candidate");
    let catalog_schemas = vm::catalog_import_schemas(&catalog, "tools::choose");
    assert_eq!(catalog_schemas.len(), 2);
    for expected in &catalog_schemas {
        assert!(
            compiled
                .program
                .host_import_schemas()
                .iter()
                .flatten()
                .any(|actual| actual == expected),
            "compiled host ABI omitted catalog overload {expected:?}"
        );
    }

    let mut registry = vm::HostFunctionRegistry::new();
    for schema in catalog_schemas {
        registry
            .register_catalog(schema, || Box::new(ChooseByType))
            .expect("overload runtime binding uses catalog identity");
    }
    let mut machine = Vm::new(compiled.program);
    registry.bind_vm_cached(&mut machine).unwrap();
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(
        machine.stack(),
        &[Value::string("int"), Value::string("string")]
    );
}

struct CountConsume(Arc<AtomicUsize>);

impl vm::HostFunction for CountConsume {
    fn call(&mut self, _machine: &mut Vm, args: &[Value]) -> vm::VmResult<vm::CallOutcome> {
        assert_eq!(args.len(), 1);
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(vm::CallOutcome::Return(vm::CallReturn::one(Value::Bool(
            true,
        ))))
    }
}

#[test]
fn same_name_guest_profile_cannot_impersonate_host_catalog_profile() {
    let profile = vm::HostStructSchema::new(
        "Profile",
        vec![vm::HostStructField::new("name", vm::HostTypeSchema::String)],
    );
    let mut builder = vm::HostApiBuilder::new();
    builder.named_struct(profile.clone());
    builder.function(vm::HostFunctionSchema::with_return(
        "profile::consume",
        vec![vm::HostParamSchema::value("value", profile.as_type())],
        vm::HostTypeSchema::Bool,
    ));
    let catalog = Arc::new(builder.build().expect("valid host catalog"));
    let calls = Arc::new(AtomicUsize::new(0));
    let schema = vm::catalog_import_schemas(&catalog, "profile::consume")
        .into_iter()
        .next()
        .expect("consume binding schema comes from the catalog");
    let mut registry = vm::HostFunctionRegistry::new();
    let factory_calls = Arc::clone(&calls);
    registry
        .register_catalog(schema, move || {
            Box::new(CountConsume(Arc::clone(&factory_calls)))
        })
        .expect("counter binding matches the caller's host catalog");
    let source = r#"import { consume } from "profile";
interface Profile { admin: boolean }
const value: Profile = { admin: true };
consume(value);
"#;

    let result = compile_source_with_flavor_and_options(
        source,
        pd_vm_compat_frontends::typescript_flavor(),
        pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog)),
    );
    let diagnostic = match result {
        Err(error) => {
            let diagnostic = error.to_string();
            println!("guest/host same-name diagnostic: {error:?}");
            Some(diagnostic)
        }
        Ok(compiled) => {
            let mut machine = Vm::new(compiled.program);
            registry
                .bind_vm_cached(&mut machine)
                .expect("bind the real host catalog counter");
            let status = machine.run();
            println!(
                "guest/host same-name probe: compile=accepted, run={status:?}, host_calls={}",
                calls.load(Ordering::SeqCst)
            );
            None
        }
    };
    println!(
        "guest/host same-name probe: compile_diagnostic={diagnostic:?}, host_calls={}",
        calls.load(Ordering::SeqCst)
    );
    let diagnostic = diagnostic.expect("same-name guest type is rejected at the host call");
    assert!(
        diagnostic.contains("no host function `profile::consume` matches the arguments"),
        "unexpected rejection reason: {diagnostic}"
    );
    assert!(
        diagnostic.contains("expected Profile, found __guest_type_"),
        "rejection did not preserve distinct guest schema identity: {diagnostic}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "rejected guest/host collision must not invoke consume"
    );
}

#[test]
fn same_name_guest_profile_declaration_only_is_rejected_in_compile_analyze_and_hints() {
    let profile = vm::HostStructSchema::new(
        "Profile",
        vec![vm::HostStructField::new("name", vm::HostTypeSchema::String)],
    );
    let mut builder = vm::HostApiBuilder::new();
    builder.named_struct(profile.clone());
    builder.function(vm::HostFunctionSchema::with_return(
        "consume",
        vec![vm::HostParamSchema::value("value", profile.as_type())],
        vm::HostTypeSchema::Bool,
    ));
    builder.function(vm::HostFunctionSchema::with_return(
        "consume_many",
        vec![vm::HostParamSchema::value(
            "values",
            vm::HostTypeSchema::Array(Box::new(profile.as_type())),
        )],
        vm::HostTypeSchema::Bool,
    ));
    let catalog = Arc::new(builder.build().expect("valid host catalog"));
    let source = r#"interface Profile { admin: boolean }
const value: Profile = { admin: true };
declare function consume(value: Profile): boolean;
value;
"#;
    let options = || pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog));

    let error = match compile_source_with_flavor_and_options(
        source,
        pd_vm_compat_frontends::typescript_flavor(),
        options(),
    ) {
        Ok(_) => panic!("unused wrong-origin host declaration must be rejected"),
        Err(error) => error,
    };
    let diagnostic = error.to_string();
    println!("unused wrong-origin declaration diagnostic: {error:?}");
    assert!(
        diagnostic.contains("does not match its catalog signature"),
        "unexpected declaration rejection: {diagnostic}"
    );
    let declaration = "declare function consume(value: Profile): boolean;";
    let declaration_start = source
        .find(declaration)
        .expect("declaration signature remains in source");
    let declaration_end = declaration_start + declaration.len();
    let expected_span = format!("[{declaration_start}..{declaration_end}]");
    assert!(
        diagnostic.contains(&expected_span),
        "declaration rejection lost its source span {expected_span}: {diagnostic}"
    );
    assert!(
        pd_vm_compat_frontends::collect_typescript_type_hints_with_options(source, options())
            .is_err(),
        "public type hints must reject the same declaration"
    );
    assert!(
        analyze_source_from_string_with_options("main.ts", source, options()).is_err(),
        "analysis must reject the same declaration"
    );

    let nested_alias_source = r#"interface Profile { admin: boolean }
type GuestProfiles = Profile[];
const values: GuestProfiles = [{ admin: true }];
declare function consume_many(values: GuestProfiles): boolean;
values;
"#;
    let nested_alias_error = match compile_source_with_flavor_and_options(
        nested_alias_source,
        pd_vm_compat_frontends::typescript_flavor(),
        options(),
    ) {
        Ok(_) => panic!("unused wrong-origin alias/nested declaration must be rejected"),
        Err(error) => error,
    };
    assert!(
        nested_alias_error
            .to_string()
            .contains("does not match its catalog signature"),
        "unexpected alias/nested declaration rejection: {nested_alias_error}"
    );
    assert!(
        pd_vm_compat_frontends::collect_typescript_type_hints_with_options(
            nested_alias_source,
            options(),
        )
        .is_err()
    );
    assert!(
        analyze_source_from_string_with_options("main.ts", nested_alias_source, options()).is_err()
    );
}

#[test]
fn same_name_guest_profile_without_declaration_remains_host_free() {
    let profile = vm::HostStructSchema::new(
        "Profile",
        vec![vm::HostStructField::new("name", vm::HostTypeSchema::String)],
    );
    let mut builder = vm::HostApiBuilder::new();
    builder.named_struct(profile.clone());
    builder.function(vm::HostFunctionSchema::with_return(
        "consume",
        vec![vm::HostParamSchema::value("value", profile.as_type())],
        vm::HostTypeSchema::Bool,
    ));
    let catalog = Arc::new(builder.build().expect("valid host catalog"));
    let options = || pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog));
    let source = r#"interface Profile { admin: boolean }
const value: Profile = { admin: true };
value;
"#;
    let hints =
        pd_vm_compat_frontends::collect_typescript_type_hints_with_options(source, options())
            .expect("guest type remains available to public type hints");
    assert!(hints.iter().any(|hint| {
        hint.name == "value"
            && hint.schema
                == Some(TypeSchema::Named(
                    "__guest_type_m0_s0".to_owned(),
                    Vec::new(),
                ))
    }));
    analyze_source_from_string_with_options("main.ts", source, options())
        .expect("analysis accepts the independent guest type");
    let compiled = compile_source_with_flavor_and_options(
        source,
        pd_vm_compat_frontends::typescript_flavor(),
        options(),
    )
    .expect("guest-only same-name record remains compilable");
    assert!(compiled.program.imports.is_empty());
    assert!(compiled.program.host_import_schemas().is_empty());

    let calls = Arc::new(AtomicUsize::new(0));
    let factory_calls = Arc::clone(&calls);
    let mut registry = vm::HostFunctionRegistry::new();
    registry.register("consume", 1, move || {
        Box::new(CountConsume(Arc::clone(&factory_calls)))
    });
    let mut machine = Vm::new(compiled.program);
    registry
        .bind_vm_cached(&mut machine)
        .expect("bind the handler against a VM with no consume import");
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn explicit_host_profile_type_alias_calls_the_catalog_counter() {
    let profile = vm::HostStructSchema::new(
        "Profile",
        vec![vm::HostStructField::new("name", vm::HostTypeSchema::String)],
    );
    let mut builder = vm::HostApiBuilder::new();
    builder.named_struct(profile.clone());
    builder.function(vm::HostFunctionSchema::with_return(
        "consume",
        vec![vm::HostParamSchema::value("value", profile.as_type())],
        vm::HostTypeSchema::Bool,
    ));
    let catalog = Arc::new(builder.build().expect("valid host catalog"));
    let calls = Arc::new(AtomicUsize::new(0));
    let options = || pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog));
    let source = r#"import type { Profile as HostProfile } from "profile";
type HostAlias = HostProfile;
const value: HostAlias = { name: "Ada" };
declare function consume(value: HostAlias): boolean;
consume(value);
"#;
    let hints =
        pd_vm_compat_frontends::collect_typescript_type_hints_with_options(source, options())
            .expect("hints preserve an explicit host type alias");
    assert!(
        hints
            .iter()
            .any(|hint| hint.name == "value" && hint.schema.is_some())
    );
    analyze_source_from_string_with_options("main.ts", source, options())
        .expect("analysis preserves an explicit host type alias");
    let compiled = compile_source_with_flavor_and_options(
        source,
        pd_vm_compat_frontends::typescript_flavor(),
        options(),
    )
    .expect("explicit host type alias should match the catalog declaration");
    let schema = vm::catalog_import_schemas(&catalog, "consume")
        .into_iter()
        .next()
        .expect("consume binding schema comes from the catalog");
    let mut registry = vm::HostFunctionRegistry::new();
    let factory_calls = Arc::clone(&calls);
    registry
        .register_catalog(schema, move || {
            Box::new(CountConsume(Arc::clone(&factory_calls)))
        })
        .expect("counter binding matches the caller's host catalog");
    let mut machine = Vm::new(compiled.program);
    registry
        .bind_vm_cached(&mut machine)
        .expect("bind explicit alias program to the real host counter");
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    println!(
        "explicit host Profile alias probe: compile=accepted, host_calls={}",
        calls.load(Ordering::SeqCst)
    );
}

#[test]
fn forged_or_mismatched_host_declarations_fail_in_compile_analysis_and_hints() {
    let no_catalog = "declare function forged(value: string): string;\nforged(\"x\");\n";
    let error = match compile_source_with_flavor_and_options(
        no_catalog,
        pd_vm_compat_frontends::typescript_flavor(),
        pd_vm_compat_frontends::compile_options(),
    ) {
        Ok(_) => panic!("ambient host declaration requires caller catalog authority"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("catalog"), "{error}");
    assert!(pd_vm_compat_frontends::collect_typescript_type_hints(no_catalog).is_err());
    assert!(
        analyze_source_from_string_with_options(
            "main.ts",
            no_catalog,
            pd_vm_compat_frontends::compile_options(),
        )
        .is_err()
    );

    let catalog = profile_catalog();
    let forged = "declare function forged(value: string): string;\nforged(\"x\");\n";
    let error = match compile_source_with_flavor_and_options(
        forged,
        pd_vm_compat_frontends::typescript_flavor(),
        pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog)),
    ) {
        Ok(_) => panic!("unknown catalog names must not become extern functions"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("does not declare"), "{error}");

    let mismatched = r#"import type { Profile } from "profile";
declare function load(name: number): Profile;
const result = load(1);
result;
"#;
    let options = || pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog));
    let error = match compile_source_with_flavor_and_options(
        mismatched,
        pd_vm_compat_frontends::typescript_flavor(),
        options(),
    ) {
        Ok(_) => panic!("declaration parameter types must match the catalog exactly"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("does not match"), "{error}");
    assert!(
        pd_vm_compat_frontends::collect_typescript_type_hints_with_options(mismatched, options(),)
            .is_err()
    );
    assert!(analyze_source_from_string_with_options("main.ts", mismatched, options()).is_err());
}

#[test]
fn declared_resource_function_keeps_catalog_key_and_passing_mode() {
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
    let catalog = Arc::new(builder.build().expect("valid resource host catalog"));
    let source = r#"import type { Resource } from "rustscript:types";
declare function close(handle: Resource<"io.file">): boolean;
function release(handle: Resource<"io.file">): boolean { return close(handle); }
"#;
    let compiled = compile_source_with_flavor_and_options(
        source,
        pd_vm_compat_frontends::typescript_flavor(),
        pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("resource declaration must retain the catalog key and passing mode");
    let schema = vm::catalog_import_schemas(&catalog, "files::close")
        .into_iter()
        .next()
        .expect("close import schema");
    assert!(
        compiled
            .program
            .host_import_schemas()
            .iter()
            .flatten()
            .any(|actual| {
                actual == &schema
                    && actual.params[0].passing == vm::HostParamPassing::TakeOwned
                    && actual.params[0].schema
                        == vm::HostTypeSchema::Resource(
                            vm::ResourceTypeKey::new("io.file").unwrap(),
                        )
            })
    );

    let forged = source.replace("io.file", "io.socket");
    assert!(
        compile_source_with_flavor_and_options(
            &forged,
            pd_vm_compat_frontends::typescript_flavor(),
            pd_vm_compat_frontends::compile_options_with_catalog(catalog),
        )
        .is_err()
    );
}

struct RecordComputedIntHostProbe(Arc<Mutex<Vec<Value>>>);

impl vm::HostFunction for RecordComputedIntHostProbe {
    fn call(&mut self, _machine: &mut Vm, args: &[Value]) -> vm::VmResult<vm::CallOutcome> {
        assert_eq!(args.len(), 1, "raw Int probe expects exactly one argument");
        self.0
            .lock()
            .expect("computed record probe lock")
            .extend(args.iter().cloned());
        Ok(vm::CallOutcome::Return(vm::CallReturn::one(Value::Bool(
            true,
        ))))
    }
}

#[test]
fn computed_record_string_cannot_reach_raw_int_host() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(|| {
            let source = r#"import { takeInt } from "recordProbe"; interface Row { text: string; number: number; } const row: Row = { text: "wrong", number: 21 }; const key: string = "text"; takeInt(row[key]);"#;
            eprintln!("TS_RECORD_HOST_SOURCE {source}");
            let mut builder = vm::HostApiBuilder::new();
            builder.function(vm::HostFunctionSchema::with_return(
                "recordProbe::takeInt",
                vec![vm::HostParamSchema::value("value", vm::HostTypeSchema::Int)],
                vm::HostTypeSchema::Bool,
            ));
            let catalog = Arc::new(builder.build().expect("valid raw int host catalog"));
            let result = compile_source_with_flavor_and_options(
                source,
                pd_vm_compat_frontends::typescript_flavor(),
                pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog)),
            );
            let compiled = match result {
                Ok(compiled) => compiled,
                Err(error) => {
                    let rendered = error.to_string();
                    eprintln!("TS_RECORD_HOST_COMPILE_REFUSAL {error:?}");
                    assert!(!rendered.is_empty(), "compile refusal must carry a diagnostic");
                    assert!(
                        !rendered.contains("computed property access"),
                        "refusal must not restore the whole-Get computed access rejection: {rendered}"
                    );
                    return;
                }
            };
            eprintln!("TS_RECORD_HOST_COMPILE_OK");
            let schemas = vm::catalog_import_schemas(&catalog, "recordProbe::takeInt");
            assert_eq!(schemas.len(), 1, "raw Int host must expose exactly one catalog schema");
            let schema = schemas.into_iter().next().expect("takeInt catalog schema");
            let host_schema_matches = compiled
                .program
                .host_import_schemas()
                .iter()
                .flatten()
                .any(|actual| actual == &schema);
            eprintln!(
                "TS_RECORD_HOST_PROGRAM host_schema_matches_catalog={} import_count={}",
                host_schema_matches,
                compiled.program.imports.len()
            );
            assert!(
                host_schema_matches,
                "compiled program omitted the sole catalog schema {schema:?}"
            );
            let observed = Arc::new(Mutex::new(Vec::new()));
            let factory_observed = Arc::clone(&observed);
            let mut registry = vm::HostFunctionRegistry::new();
            registry
                .register_catalog(schema, move || {
                    Box::new(RecordComputedIntHostProbe(Arc::clone(&factory_observed)))
                })
                .expect("raw Int probe binding matches the catalog");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            registry
                .bind_vm_cached(&mut machine)
                .expect("bind the raw Int probe from the catalog");
            eprintln!("TS_RECORD_HOST_GUEST_BEGIN");
            let outcome = machine.run();
            let counts = (machine.jit_native_exec_count(), machine.aot_exec_count());
            let calls = observed.lock().expect("computed record probe lock").clone();
            eprintln!(
                "TS_RECORD_HOST_OBSERVED outcome={outcome:?} calls={} args={:?} stack={:?} native={} aot={}",
                calls.len(),
                calls,
                machine.stack(),
                counts.0,
                counts.1
            );
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_RECORD_HOST_RESET resource_count=0 guest_invocations=1");
            assert!(
                calls.is_empty(),
                "raw Int callback received unchecked computed record Value; args={calls:?}"
            );
            assert!(
                outcome.is_err(),
                "compiled program must deny the computed record before importing the raw Int host: {outcome:?}"
            );
            assert_eq!(
                counts,
                (0, 0),
                "JIT/AOT native execution must stay disabled for the boundary probe"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

fn ts_annotated_number_host_probe(source: &str, allow_call: bool) {
    let owned = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            eprintln!("TS_ANNOTATED_NUMBER_HOST_SOURCE {owned}");
            let mut builder = vm::HostApiBuilder::new();
            builder.function(vm::HostFunctionSchema::with_return(
                "recordProbe::takeNumber",
                vec![vm::HostParamSchema::value(
                    "value",
                    vm::HostTypeSchema::Number,
                )],
                vm::HostTypeSchema::Bool,
            ));
            let catalog = Arc::new(builder.build().expect("valid raw Number host catalog"));
            let result = compile_source_with_flavor_and_options(
                &owned,
                pd_vm_compat_frontends::typescript_flavor(),
                pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog)),
            );
            let compiled = match result {
                Ok(compiled) => compiled,
                Err(error) => {
                    let rendered = error.to_string();
                    eprintln!("TS_ANNOTATED_NUMBER_HOST_COMPILE_REFUSAL {error:?}");
                    assert!(
                        !allow_call,
                        "annotated number control source must compile: {error:?}"
                    );
                    assert!(
                        !rendered.is_empty(),
                        "compile refusal must carry a diagnostic"
                    );
                    assert!(
                        !rendered.contains("computed property access"),
                        "refusal must not restore the whole-Get computed access rejection: {rendered}"
                    );
                    return;
                }
            };
            eprintln!("TS_ANNOTATED_NUMBER_HOST_COMPILE_OK");
            let schemas = vm::catalog_import_schemas(&catalog, "recordProbe::takeNumber");
            assert_eq!(
                schemas.len(),
                1,
                "raw Number host must expose exactly one catalog schema"
            );
            let schema = schemas.into_iter().next().expect("takeNumber catalog schema");
            let host_schema_matches = compiled
                .program
                .host_import_schemas()
                .iter()
                .flatten()
                .any(|actual| actual == &schema);
            eprintln!(
                "TS_ANNOTATED_NUMBER_HOST_PROGRAM host_schema_matches_catalog={} import_count={}",
                host_schema_matches,
                compiled.program.imports.len()
            );
            assert!(
                host_schema_matches,
                "compiled program omitted the sole catalog schema {schema:?}"
            );
            let observed = Arc::new(Mutex::new(Vec::new()));
            let factory_observed = Arc::clone(&observed);
            let mut registry = vm::HostFunctionRegistry::new();
            registry
                .register_catalog(schema, move || {
                    Box::new(RecordComputedIntHostProbe(Arc::clone(&factory_observed)))
                })
                .expect("raw Number probe binding matches the catalog");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            registry
                .bind_vm_cached(&mut machine)
                .expect("bind the raw Number probe from the catalog");
            eprintln!("TS_ANNOTATED_NUMBER_HOST_GUEST_BEGIN");
            let outcome = machine.run();
            let stack_before_reset = machine.stack().to_vec();
            let counts = (machine.jit_native_exec_count(), machine.aot_exec_count());
            let calls = observed
                .lock()
                .expect("annotated number probe lock")
                .clone();
            eprintln!(
                "TS_ANNOTATED_NUMBER_HOST_OBSERVED outcome={outcome:?} calls={} args={:?} stack={:?} native={} aot={}",
                calls.len(),
                calls,
                machine.stack(),
                counts.0,
                counts.1
            );
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_ANNOTATED_NUMBER_HOST_RESET resource_count=0 guest_invocations=1");
            assert_eq!(
                counts,
                (0, 0),
                "JIT/AOT native execution must stay disabled for the boundary probe"
            );
            if allow_call {
                assert_eq!(
                    outcome.unwrap(),
                    VmStatus::Halted,
                    "annotated Int control must halt normally"
                );
                assert_eq!(
                    calls.len(),
                    1,
                    "annotated Int control must reach the raw Number callback exactly once; args={calls:?}"
                );
                assert_eq!(
                    calls,
                    vec![Value::Int(21)],
                    "annotated Int control must pass Int 21 unchanged; args={calls:?}"
                );
                assert_eq!(
                    stack_before_reset.as_slice(),
                    &[Value::Bool(true)],
                    "annotated Int control must observe Bool true"
                );
            } else {
                assert!(
                    calls.is_empty(),
                    "raw Number callback received unchecked computed record Value; args={calls:?}"
                );
                assert!(
                    outcome.is_err(),
                    "compiled program must deny the computed record before importing the raw Number host: {outcome:?}"
                );
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn annotated_number_raw_host_accepts_known_int_control() {
    ts_annotated_number_host_probe(
        r#"import { takeNumber } from "recordProbe"; const selected: number = 21; takeNumber(selected);"#,
        true,
    );
}

#[test]
fn annotated_number_cannot_launder_computed_record_string_to_raw_host() {
    ts_annotated_number_host_probe(
        r#"import { takeNumber } from "recordProbe"; interface Row { text: string; number: number; } const row: Row = { text: "wrong", number: 21 }; const key: string = "text"; const selected: number = row[key]; takeNumber(selected);"#,
        false,
    );
}

#[test]
fn annotated_number_alias_initializer_cannot_reach_raw_host() {
    ts_annotated_number_host_probe(
        r#"import { takeNumber } from "recordProbe"; interface Row { text: string; number: number; } const row: Row = { text: "wrong", number: 21 }; const key: string = "text"; const raw = row[key]; const selected: number = raw; takeNumber(selected);"#,
        false,
    );
}

#[test]
fn annotated_number_alias_assignment_cannot_reach_raw_host() {
    ts_annotated_number_host_probe(
        r#"import { takeNumber } from "recordProbe"; interface Row { text: string; number: number; } const row: Row = { text: "wrong", number: 21 }; const key: string = "text"; const raw = row[key]; let selected: number = 21; selected = raw; takeNumber(selected);"#,
        false,
    );
}

#[test]
fn function_local_annotated_number_alias_cannot_reach_raw_host() {
    ts_annotated_number_host_probe(
        r#"import { takeNumber } from "recordProbe"; interface Row { text: string; number: number; } function readRow(): boolean { const row: Row = { text: "wrong", number: 21 }; const key: string = "text"; const raw = row[key]; const selected: number = raw; return takeNumber(selected); } readRow();"#,
        false,
    );
}

struct NullableMapHostValueProbe {
    value: Value,
    calls: Arc<Mutex<usize>>,
}

impl vm::HostFunction for NullableMapHostValueProbe {
    fn call(&mut self, _machine: &mut Vm, args: &[Value]) -> vm::VmResult<vm::CallOutcome> {
        assert!(args.is_empty(), "nullable value probe takes no arguments");
        *self.calls.lock().expect("nullable probe counter") += 1;
        Ok(vm::CallOutcome::Return(vm::CallReturn::one(
            self.value.clone(),
        )))
    }
}

fn nullable_map_host_probe(source: &str, kind: &str) {
    let source = source.to_owned();
    let kind = kind.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            eprintln!("NULLABLE_MAP_HOST_SOURCE {source:?}");
            let number_return = kind == "number_control";
            let expect_refusal = kind == "unknown_refusal";
            let result_schema = vm::HostTypeSchema::Optional(Box::new(if number_return {
                vm::HostTypeSchema::Number
            } else {
                vm::HostTypeSchema::Unknown
            }));
            let mut builder = vm::HostApiBuilder::new();
            builder.function(vm::HostFunctionSchema::with_return(
                "nullableProbe::read",
                Vec::new(),
                result_schema,
            ));
            let catalog = Arc::new(builder.build().expect("synthetic nullable catalog"));
            let result = vm::compile_source_with_flavor_and_options(
                &source,
                vm::SourceFlavor::RustScript,
                pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog)),
            );
            let compiled = match result {
                Err(error) => {
                    eprintln!("NULLABLE_MAP_HOST_COMPILE_REFUSAL {error:?}");
                    assert!(expect_refusal, "nullable catalog control refused: {error:?}");
                    let source_error = match &error {
                        vm::SourcePathError::Source(error)
                        | vm::SourcePathError::SourceWithMap { error, .. } => error,
                        other => panic!("wrong nullable refusal layer: {other:?}"),
                    };
                    let vm::SourceError::Compile(vm::CompileError::InvalidFieldAccess { detail, line, span, .. }) = source_error else {
                        panic!("wrong nullable store refusal category: {source_error:?}");
                    };
                    let detail = detail.to_ascii_lowercase();
                    assert!(detail.contains("number") && (detail.contains("unknown") || detail.contains("not provable")), "wrong nullable proof diagnostic: {detail}");
                    let start = source.find("state[\"value\"] =").expect("Map store");
                    let line_number = source[..start].bytes().filter(|byte| *byte == b'\n').count() + 1;
                    assert_eq!(*line, Some(u32::try_from(line_number).expect("source line fits u32")));
                    assert!(span.is_some());
                    eprintln!("NULLABLE_MAP_HOST_REFUSAL_END guest_invocations=0 callbacks=0");
                    return;
                }
                Ok(compiled) => compiled,
            };
            assert!(compiled.program.type_map.as_ref().is_some_and(|types| types.strict_types));
            let schemas = vm::catalog_import_schemas(&catalog, "nullableProbe::read");
            assert_eq!(schemas.len(), 1);
            let schema = schemas.into_iter().next().expect("nullable catalog schema");
            assert!(compiled.program.host_import_schemas().iter().flatten().any(|actual| actual == &schema));
            eprintln!("NULLABLE_MAP_HOST_COMPILE_OK strict_types=true catalog_schema_matches=true kind={kind}");
            assert!(!expect_refusal, "strict scalar Map accepted catalog Optional<Unknown> element store");
            let calls = Arc::new(Mutex::new(0));
            let factory_calls = Arc::clone(&calls);
            let return_value = if number_return { Value::Int(9) } else { Value::string("wrong") };
            let factory_value = return_value.clone();
            let mut registry = vm::HostFunctionRegistry::new();
            registry.register_catalog(schema, move || Box::new(NullableMapHostValueProbe {
                value: factory_value.clone(),
                calls: Arc::clone(&factory_calls),
            })).expect("register exact nullable host schema");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig { enabled: false, ..*machine.jit_config() });
            machine.set_fuel(10_000_000);
            registry.bind_vm_cached(&mut machine).expect("bind nullable catalog");
            eprintln!("NULLABLE_MAP_HOST_GUEST_BEGIN");
            let outcome = machine.run();
            let snapshot = machine.stack().to_vec();
            let call_count = *calls.lock().expect("nullable probe counter");
            let counts = (machine.jit_native_exec_count(), machine.aot_exec_count());
            eprintln!("NULLABLE_MAP_HOST_OBSERVED outcome={outcome:?} calls={call_count} stack={snapshot:?} native={} aot={}", counts.0, counts.1);
            machine.reset_for_reuse().expect("reset nullable host probe");
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("NULLABLE_MAP_HOST_RESET resource_count=0 guest_invocations=1");
            assert_eq!(outcome.expect("nullable control guest"), VmStatus::Halted);
            assert_eq!(snapshot, vec![return_value]);
            assert_eq!(call_count, 1);
            assert_eq!(counts, (0, 0));
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn nullable_unknown_host_returns_non_null_string_control() {
    nullable_map_host_probe(
        "use nullableProbe;\nnullableProbe::read();\n",
        "unknown_control",
    );
}

#[test]
fn nullable_number_host_value_remains_valid_map_store() {
    nullable_map_host_probe(
        "use nullableProbe;\nlet mut state: map<number> = {value: 1};\nstate[\"value\"] = nullableProbe::read();\nstate[\"value\"];\n",
        "number_control",
    );
}

#[test]
fn nullable_unknown_host_value_cannot_enter_scalar_map() {
    nullable_map_host_probe(
        "use nullableProbe;\nlet mut state: map<number> = {value: 1};\nstate[\"value\"] = nullableProbe::read();\nstate[\"value\"];\n",
        "unknown_refusal",
    );
}
