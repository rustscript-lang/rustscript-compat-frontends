use vm::{
    Value, Vm, VmStatus, compile_source_with_flavor_and_options,
    compiler::{FrontendId, SourceFlavor},
};

fn flavor() -> SourceFlavor {
    SourceFlavor::Plugin(FrontendId::new("typescript"))
}

#[test]
fn public_type_hints_match_executable_schema_and_source_bytes() {
    let source = "const amount: number = 42;\namount;\n";
    let hints = pd_vm_compat_frontends::collect_typescript_type_hints(source)
        .expect("typed hint collection should use the normal compiler pipeline");
    let hint = hints
        .iter()
        .find(|hint| hint.name == "amount")
        .expect("amount hint should be present");
    assert_eq!(hint.schema, Some(vm::TypeSchema::Number));
    assert_eq!(hint.inferred_type, "number");
    let start = source.find("amount").expect("amount identifier");
    assert_eq!(
        hint.span,
        Some(vm::compiler::source_map::Span::new(
            0,
            start,
            start + "amount".len(),
        ))
    );
}

#[test]
fn annotated_string_declaration_is_checked_and_executes() {
    let source = "const name: string = \"Ada\";\nname;\n";
    let compiled = compile_source_with_flavor_and_options(
        source,
        flavor(),
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("valid annotated TypeScript should compile");
    let mut machine = Vm::new(compiled.program);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(machine.stack(), &[Value::string("Ada")]);
}

#[test]
fn incompatible_annotation_is_rejected_before_execution() {
    let error = match compile_source_with_flavor_and_options(
        "const name: string = 7;\nname;\n",
        flavor(),
        pd_vm_compat_frontends::compile_options(),
    ) {
        Ok(_) => panic!("string annotation must reject an integer initializer"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("type"));
}
