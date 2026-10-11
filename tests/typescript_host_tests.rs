use std::sync::{Arc, Mutex};

use vm::{TypeSchema, Value, Vm, VmStatus, compile_source_with_flavor_and_options};

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
