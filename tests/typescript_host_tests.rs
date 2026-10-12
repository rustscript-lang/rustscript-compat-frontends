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
