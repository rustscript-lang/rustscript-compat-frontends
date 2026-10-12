use vm::{
    TypeSchema, Value, Vm, VmStatus, compiler::{FrontendId, SourceFlavor},
};

fn flavor() -> SourceFlavor {
    SourceFlavor::Plugin(FrontendId::new("typescript"))
}

#[test]
fn exported_annotated_arrow_keeps_recursive_callable_schema_and_executes() {
    let source = r#"export const factorial = (num: number): number =>
  num === 0 ? 1 : num * factorial(num - 1);
const callback: (num: number) => number = factorial;
const answer = callback(6);
answer;
"#;
    let hints = pd_vm_compat_frontends::collect_typescript_type_hints(source)
        .expect("exported typed arrows should be analyzed by the normal pipeline");
    let callback = hints
        .iter()
        .find(|hint| hint.name == "callback")
        .expect("callback hint should be present");
    let callable = TypeSchema::Callable {
        params: vec![TypeSchema::Number],
        result: Box::new(TypeSchema::Number),
    };
    assert_eq!(callback.schema, Some(callable.clone()));
    assert_eq!(
        hints
            .iter()
            .find(|hint| hint.name == "answer")
            .expect("answer hint should be present")
            .schema,
        Some(TypeSchema::Number)
    );

    let compiled = vm::compile_source_with_flavor_and_options(
        source,
        flavor(),
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("exported annotated arrow should compile");
    assert!(compiled.program.callable_prototypes.iter().any(|prototype| {
        prototype.schema.as_ref() == Some(&callable)
    }));
    assert!(compiled.program.exported_callables.iter().any(|callable| {
        callable.name == "factorial"
    }));
    let mut machine = Vm::new(compiled.program);
    assert_eq!(machine.run().expect("recursive arrow should execute"), VmStatus::Halted);
    assert_eq!(machine.stack(), &[Value::Int(720)]);
}

#[test]
fn default_arrow_parameters_use_prior_arguments_and_respect_explicit_values() {
    let source = r#"export const withDefault = (base: number, value: number = base + 1): number => value;
const omitted = withDefault(40);
const explicit = withDefault(40, 7);
omitted * 10 + explicit;
"#;
    let hints = pd_vm_compat_frontends::collect_typescript_type_hints(source)
        .expect("typed default arrows should preserve hint analysis");
    assert_eq!(
        hints.iter().filter(|hint| hint.name == "value").count(),
        1,
        "generated default evaluation must not duplicate source parameter hints"
    );
    let compiled = vm::compile_source_with_flavor_and_options(
        source,
        flavor(),
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("typed arrow defaults should compile");
    let mut machine = Vm::new(compiled.program);
    assert_eq!(machine.run().expect("default calls should execute"), VmStatus::Halted);
    assert_eq!(machine.stack(), &[Value::Int(417)]);
}

#[test]
fn nested_arrow_annotations_keep_capture_types() {
    let source = r#"const addTo = (base: number): number => {
  const add = (step: number): number => base + step;
  return add(2);
};
const result = addTo(40);
result;
"#;
    let compiled = vm::compile_source_with_flavor_and_options(
        source,
        flavor(),
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("annotated nested arrows should preserve typed captures");
    let mut machine = Vm::new(compiled.program);
    assert_eq!(machine.run().expect("nested arrow should execute"), VmStatus::Halted);
    assert_eq!(machine.stack(), &[Value::Int(42)]);
}

#[test]
fn typed_array_length_uses_array_container_length() {
    let source = r#"const lengthOf = (values: number[]): number => values.length;
const result = lengthOf([1, 2, 3]);
result;
"#;
    let compiled = vm::compile_source_with_flavor_and_options(
        source,
        flavor(),
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("typed array length should compile");
    let mut machine = Vm::new(compiled.program);
    assert_eq!(machine.run().expect("array length should execute"), VmStatus::Halted);
    assert_eq!(machine.stack(), &[Value::Int(3)]);
}

#[test]
fn typed_math_and_number_numeric_globals_evaluate() {
    let source = r#"const integer = Number.isInteger(3);
const fraction = Number.isInteger(3.5);
const result = Math.floor(2.8) + Math.sqrt(16) + Math.round(2.4) + (integer ? 1 : 0) + (fraction ? 10 : 0);
result;
"#;
    let compiled = vm::compile_source_with_flavor_and_options(
        source,
        flavor(),
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("typed Math and Number globals should compile");
    let mut machine = Vm::new(compiled.program);
    assert_eq!(machine.run().expect("numeric globals should execute"), VmStatus::Halted);
    assert_eq!(machine.stack(), &[Value::Float(9.0)]);
}

#[test]
fn nan_predicates_accept_runtime_math_results() {
    let source = r#"const invalid = Math.sqrt(-1);
const bothChecks = Number.isNaN(invalid) && isNaN(invalid);
const result = bothChecks ? 1 : 0;
result;
"#;
    let compiled = vm::compile_source_with_flavor_and_options(
        source,
        flavor(),
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("Number.isNaN and global isNaN should compile");
    let mut machine = Vm::new(compiled.program);
    assert_eq!(machine.run().expect("NaN predicates should execute"), VmStatus::Halted);
    assert_eq!(machine.stack(), &[Value::Int(1)]);
}

#[test]
fn exported_arrow_parameters_remain_strictly_typed() {
    let source = "export const identity = (value) => value;\n";
    let error = pd_vm_compat_frontends::collect_typescript_type_hints(source)
        .expect_err("an exported arrow without context or annotation must fail strict typing");
    assert!(error.to_string().contains("noImplicitAny"), "unexpected diagnostic: {error}");
    assert!(error.to_string().contains("value"), "unexpected diagnostic: {error}");
}
