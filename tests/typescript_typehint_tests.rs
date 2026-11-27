use vm::{
    SourcePosition, TypeSchema, Value, Vm, VmStatus, analyze_source_from_string_with_options,
    compile_source_with_flavor_and_options,
    compiler::{FrontendId, SourceFlavor},
};

fn flavor() -> SourceFlavor {
    SourceFlavor::Plugin(FrontendId::new("typescript"))
}

fn hint<'a>(hints: &'a [vm::InferredLocalTypeHint], name: &str) -> &'a vm::InferredLocalTypeHint {
    hints
        .iter()
        .find(|hint| hint.name == name)
        .unwrap_or_else(|| panic!("missing TypeScript hint for {name}"))
}

#[test]
fn public_hints_preserve_optional_callable_results_and_execute_the_prototype() {
    let source = r#"function maybe(value: string): string | null { return value; }
const callback = maybe;
const result = callback("Ada");
result;
"#;
    let expected_callable = TypeSchema::Callable {
        params: vec![TypeSchema::String],
        result: Box::new(TypeSchema::Optional(Box::new(TypeSchema::String))),
    };
    let hints = pd_vm_compat_frontends::collect_typescript_type_hints(source)
        .expect("type hints use the checked TypeScript pipeline");
    let callable = hint(&hints, "callback");
    assert_eq!(callable.schema, Some(expected_callable.clone()));
    assert_eq!(callable.inferred_type, "(string) => string | null");
    assert_eq!(
        hint(&hints, "result").schema,
        Some(TypeSchema::Optional(Box::new(TypeSchema::String)))
    );
    let name_offset = source.find("callback =").expect("callback declaration");
    assert_eq!(
        &source[callable.span.expect("declaration span").lo
            ..callable.span.expect("declaration span").hi],
        "callback"
    );
    assert_eq!(name_offset, callable.span.expect("declaration span").lo);

    let compiled = compile_source_with_flavor_and_options(
        source,
        flavor(),
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("optional callable should compile");
    assert!(
        compiled
            .program
            .callable_prototypes
            .iter()
            .any(|prototype| { prototype.schema.as_ref() == Some(&expected_callable) })
    );
    let mut machine = Vm::new(compiled.program);
    assert_eq!(
        machine.run().expect("program should execute"),
        VmStatus::Halted
    );
    assert_eq!(machine.stack(), &[Value::string("Ada")]);
}

#[test]
fn function_type_annotation_contextually_types_the_arrow_and_is_executable() {
    let source = r#"const render: (value: string) => string = (value) => value;
const result = render("Ada");
result;
"#;
    let expected = TypeSchema::Callable {
        params: vec![TypeSchema::String],
        result: Box::new(TypeSchema::String),
    };
    let hints = pd_vm_compat_frontends::collect_typescript_type_hints(source)
        .expect("function types should be preserved by the checked query");
    assert_eq!(hint(&hints, "render").schema, Some(expected.clone()));
    assert_eq!(hint(&hints, "value").schema, Some(TypeSchema::String));

    let compiled = compile_source_with_flavor_and_options(
        source,
        flavor(),
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("contextually typed arrow should compile");
    assert!(
        compiled
            .program
            .callable_prototypes
            .iter()
            .any(|prototype| { prototype.schema.as_ref() == Some(&expected) })
    );
    let mut machine = Vm::new(compiled.program);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(machine.stack(), &[Value::string("Ada")]);
}

#[test]
fn generic_return_specializes_per_call_without_changing_the_declaration() {
    let source = r#"import type { Int } from "rustscript:types";
function identity<T>(value: T): T { return value; }
const numberValue = identity<Int>(7);
const textValue = identity<string>("seven");
numberValue;
textValue;
"#;
    let hints = pd_vm_compat_frontends::collect_typescript_type_hints(source)
        .expect("generic calls should have per-call inferred results");
    assert_eq!(hint(&hints, "numberValue").schema, Some(TypeSchema::Int));
    assert_eq!(hint(&hints, "textValue").schema, Some(TypeSchema::String));
    let model = analyze_source_from_string_with_options(
        "main.ts",
        source,
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("semantic analysis shares the checked generic view");
    let declaration_offset = source.find("identity<T>").expect("generic declaration");
    assert_eq!(
        model.inferred_schema_at(SourcePosition::new(0, declaration_offset)),
        Some(TypeSchema::GenericParam("T".to_owned()))
    );
    let number_offset = source.find("numberValue =").expect("number local");
    let text_offset = source.find("textValue =").expect("text local");
    assert_eq!(
        model.inferred_schema_at(SourcePosition::new(0, number_offset)),
        Some(TypeSchema::Int)
    );
    assert_eq!(
        model.inferred_schema_at(SourcePosition::new(0, text_offset)),
        Some(TypeSchema::String)
    );
    let call_offset = source.rfind("identity<Int>(7)").expect("generic call") + 1;
    let definition = model
        .definition_at(SourcePosition::new(0, call_offset))
        .expect("generic call resolves to its function definition");
    assert_eq!(definition.span.lo, source.find("identity<T>").unwrap());

    let compiled = compile_source_with_flavor_and_options(
        source,
        flavor(),
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("generic function should compile");
    let mut machine = Vm::new(compiled.program);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(machine.stack(), &[Value::Int(7), Value::string("seven")]);
}

#[test]
fn uninstantiated_generic_function_value_is_rejected() {
    let source =
        "function identity<T>(value: T): T { return value; }\nconst callback = identity;\n";
    assert!(
        pd_vm_compat_frontends::collect_typescript_type_hints(source).is_err(),
        "hinting must preserve generic function-value admission"
    );
    assert!(
        compile_source_with_flavor_and_options(
            source,
            flavor(),
            pd_vm_compat_frontends::compile_options(),
        )
        .is_err(),
        "a bare generic function value needs explicit arguments or context"
    );
}
