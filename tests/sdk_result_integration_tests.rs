use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use vm::{
    GuestResult, IntoVmValue, SourcePosition, TypeSchema, Value, Vm, VmStatus,
    analyze_source_from_string_with_options, compile_source_with_flavor_and_options,
};

struct FinishHost {
    calls: Arc<AtomicUsize>,
}

impl vm::HostFunction for FinishHost {
    fn call(&mut self, _machine: &mut Vm, args: &[Value]) -> vm::VmResult<vm::CallOutcome> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let value = match args {
            [Value::Int(0)] => GuestResult::<i64, String>::Ok(7).into_vm_value(),
            [Value::Int(1)] => {
                GuestResult::<i64, String>::Err("guest failure".to_owned()).into_vm_value()
            }
            [Value::Int(_)] => {
                return Err(vm::VmError::HostError("transport failure".to_owned()));
            }
            _ => {
                return Err(vm::VmError::HostError(
                    "unexpected finish argument".to_owned(),
                ));
            }
        };
        Ok(vm::CallOutcome::Return(vm::CallReturn::one(value)))
    }
}

fn finish_catalog() -> Arc<vm::HostApiCatalog> {
    let mut builder = vm::HostApiBuilder::new();
    builder.function(vm::HostFunctionSchema::with_return(
        "sdk::finish",
        vec![vm::HostParamSchema::value("mode", vm::HostTypeSchema::Int)],
        vm::HostTypeSchema::Result {
            ok: Box::new(vm::HostTypeSchema::Int),
            err: Box::new(vm::HostTypeSchema::String),
        },
    ));
    Arc::new(builder.build().expect("valid sdk result catalog"))
}

fn result_schema() -> TypeSchema {
    TypeSchema::Result {
        ok: Box::new(TypeSchema::Int),
        err: Box::new(TypeSchema::String),
    }
}

#[test]
fn explicit_catalog_guest_result_reaches_hints_analysis_and_runtime_without_source_syntax() {
    let source = "import { finish } from \"sdk\";\nconst okValue = finish(0);\nconst errValue = finish(1);\nokValue;\nerrValue;\n";
    let catalog = finish_catalog();
    let options = pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog));
    let hints = pd_vm_compat_frontends::collect_typescript_type_hints_with_options(
        source,
        pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("hinting uses the explicit result catalog");
    let hint = |name: &str| {
        hints
            .iter()
            .find(|hint| hint.name == name)
            .unwrap_or_else(|| panic!("missing hint for {name}"))
    };
    for name in ["okValue", "errValue"] {
        assert_eq!(hint(name).schema, Some(result_schema()));
        assert_eq!(hint(name).inferred_type, "Result<Int, string>");
        assert!(!matches!(hint(name).schema, Some(TypeSchema::Map(_))));
        assert!(!matches!(hint(name).schema, Some(TypeSchema::Named(_, _))));
    }

    let model = analyze_source_from_string_with_options(
        "main.ts",
        source,
        pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("analysis uses the same explicit result catalog");
    let ok_offset = source.find("okValue =").expect("ok binding");
    assert_eq!(
        model.inferred_schema_at(SourcePosition::new(0, ok_offset)),
        Some(result_schema())
    );

    let compiled = compile_source_with_flavor_and_options(
        source,
        pd_vm_compat_frontends::typescript_flavor(),
        options,
    )
    .expect("explicit result catalog should compile without source result syntax");
    let finish_schema = vm::catalog_import_schemas(&catalog, "sdk::finish")
        .into_iter()
        .next()
        .expect("finish schema comes from the catalog");
    assert!(matches!(
        finish_schema.return_type,
        vm::HostTypeSchema::Result { .. }
    ));
    assert!(
        compiled
            .program
            .host_import_schemas()
            .iter()
            .flatten()
            .any(|schema| schema == &finish_schema),
        "compiled metadata omitted the catalog result schema"
    );

    let calls = Arc::new(AtomicUsize::new(0));
    let factory_calls = Arc::clone(&calls);
    let mut registry = vm::HostFunctionRegistry::new();
    registry
        .register_catalog(finish_schema, move || {
            Box::new(FinishHost {
                calls: Arc::clone(&factory_calls),
            })
        })
        .expect("finish binding matches the catalog");
    let mut machine = Vm::new(compiled.program);
    registry.bind_vm_cached(&mut machine).unwrap();
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        machine.stack(),
        &[
            GuestResult::<i64, String>::Ok(7).into_vm_value(),
            GuestResult::<i64, String>::Err("guest failure".to_owned()).into_vm_value(),
        ]
    );
}

#[test]
fn outer_vm_error_stays_outside_guest_result_err() {
    let source = "import { finish } from \"sdk\";\nfinish(2);\n";
    let catalog = finish_catalog();
    let compiled = compile_source_with_flavor_and_options(
        source,
        pd_vm_compat_frontends::typescript_flavor(),
        pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("transport probe uses the same catalog");
    let finish_schema = vm::catalog_import_schemas(&catalog, "sdk::finish")
        .into_iter()
        .next()
        .expect("finish schema");
    let calls = Arc::new(AtomicUsize::new(0));
    let factory_calls = Arc::clone(&calls);
    let mut registry = vm::HostFunctionRegistry::new();
    registry
        .register_catalog(finish_schema, move || {
            Box::new(FinishHost {
                calls: Arc::clone(&factory_calls),
            })
        })
        .expect("finish binding matches the catalog");
    let mut machine = Vm::new(compiled.program);
    registry.bind_vm_cached(&mut machine).unwrap();
    match machine.run() {
        Err(error) => {
            let rendered = error.to_string();
            assert!(
                rendered.contains("transport failure"),
                "outer transport error lost its payload: {rendered}"
            );
        }
        Ok(_) => panic!("transport failure must stay an outer VmResult error"),
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_ne!(
        machine.stack().last(),
        Some(&GuestResult::<i64, String>::Err("guest failure".to_owned()).into_vm_value())
    );
}
