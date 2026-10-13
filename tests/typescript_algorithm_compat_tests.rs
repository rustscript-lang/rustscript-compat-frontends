use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use vm::{
    TypeSchema, Value, Vm, VmStatus,
    compiler::{FrontendId, SourceFlavor},
};

fn flavor() -> SourceFlavor {
    SourceFlavor::Plugin(FrontendId::new("typescript"))
}

struct CountedResult {
    calls: Arc<AtomicUsize>,
    value: Value,
}

impl vm::HostFunction for CountedResult {
    fn call(&mut self, _machine: &mut Vm, _args: &[Value]) -> vm::VmResult<vm::CallOutcome> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(vm::CallOutcome::Return(vm::CallReturn::one(
            self.value.clone(),
        )))
    }
}

fn counted_host_program(
    source: &str,
    return_type: vm::HostTypeSchema,
    result: Value,
    calls: Arc<AtomicUsize>,
) -> (
    vm::CompiledProgram,
    vm::HostFunctionRegistry,
    Arc<vm::HostApiCatalog>,
) {
    let function_name = "effects::next";
    let mut builder = vm::HostApiBuilder::new();
    builder.function(vm::HostFunctionSchema::with_return(
        function_name,
        Vec::new(),
        return_type,
    ));
    let catalog = Arc::new(builder.build().expect("valid counted host catalog"));
    let compiled = vm::compile_source_with_flavor_and_options(
        source,
        flavor(),
        pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("counted host fixture should compile against its catalog");
    let schema = vm::catalog_import_schemas(&catalog, function_name)
        .into_iter()
        .next()
        .expect("counted host schema should be catalog-owned");
    let mut registry = vm::HostFunctionRegistry::new();
    registry
        .register_catalog(schema, move || {
            Box::new(CountedResult {
                calls: Arc::clone(&calls),
                value: result.clone(),
            })
        })
        .expect("counted host binding should match its catalog");
    (compiled, registry, catalog)
}

struct OrderedOperand {
    events: Arc<Mutex<Vec<&'static str>>>,
    name: &'static str,
    value: Value,
}

impl vm::HostFunction for OrderedOperand {
    fn call(&mut self, _machine: &mut Vm, args: &[Value]) -> vm::VmResult<vm::CallOutcome> {
        assert!(args.is_empty());
        self.events
            .lock()
            .expect("operand event recorder should not be poisoned")
            .push(self.name);
        Ok(vm::CallOutcome::Return(vm::CallReturn::one(
            self.value.clone(),
        )))
    }
}

fn strict_equality_effect_program(
    source: &str,
    events: Arc<Mutex<Vec<&'static str>>>,
) -> (vm::CompiledProgram, vm::HostFunctionRegistry) {
    let mut builder = vm::HostApiBuilder::new();
    for name in ["effects::left", "effects::right"] {
        builder.function(vm::HostFunctionSchema::with_return(
            name,
            Vec::<vm::HostParamSchema>::new(),
            vm::HostTypeSchema::Number,
        ));
    }
    let catalog = Arc::new(builder.build().expect("valid operand recorder catalog"));
    let compiled = vm::compile_source_with_flavor_and_options(
        source,
        flavor(),
        pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("numeric equality side-effect fixture should compile");

    let mut registry = vm::HostFunctionRegistry::new();
    for (function_name, event_name) in [("effects::left", "left"), ("effects::right", "right")] {
        let schema = vm::catalog_import_schemas(&catalog, function_name)
            .into_iter()
            .next()
            .expect("operand recorder schema should be catalog-owned");
        let function_events = Arc::clone(&events);
        registry
            .register_catalog(schema, move || {
                Box::new(OrderedOperand {
                    events: Arc::clone(&function_events),
                    name: event_name,
                    value: Value::Int(7),
                })
            })
            .expect("operand recorder binding should match its catalog");
    }
    (compiled, registry)
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
    assert!(
        compiled
            .program
            .callable_prototypes
            .iter()
            .any(|prototype| { prototype.schema.as_ref() == Some(&callable) })
    );
    assert!(
        compiled
            .program
            .exported_callables
            .iter()
            .any(|callable| { callable.name == "factorial" })
    );
    let mut machine = Vm::new(compiled.program);
    assert_eq!(
        machine.run().expect("recursive arrow should execute"),
        VmStatus::Halted
    );
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
    assert_eq!(
        machine.run().expect("default calls should execute"),
        VmStatus::Halted
    );
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
    assert_eq!(
        machine.run().expect("nested arrow should execute"),
        VmStatus::Halted
    );
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
    assert_eq!(
        machine.run().expect("array length should execute"),
        VmStatus::Halted
    );
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
    assert_eq!(
        machine.run().expect("numeric globals should execute"),
        VmStatus::Halted
    );
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
    assert_eq!(
        machine.run().expect("NaN predicates should execute"),
        VmStatus::Halted
    );
    assert_eq!(machine.stack(), &[Value::Int(1)]);
}

#[test]
fn standard_api_number_is_integer_evaluates_argument_once_without_public_temporaries() {
    let source = r#"import { next } from "effects";
const result = Number.isInteger(next());
result;
"#;
    let calls = Arc::new(AtomicUsize::new(0));
    let (compiled, registry, catalog) = counted_host_program(
        source,
        vm::HostTypeSchema::Number,
        Value::Int(3),
        Arc::clone(&calls),
    );
    let hints = pd_vm_compat_frontends::collect_typescript_type_hints_with_options(
        source,
        pd_vm_compat_frontends::compile_options_with_catalog(catalog),
    )
    .expect("Number.isInteger argument should remain analyzable");
    assert_eq!(
        hints
            .iter()
            .map(|hint| hint.name.as_str())
            .collect::<Vec<_>>(),
        ["result"],
        "compiler-private locals must not appear in public hints"
    );

    let mut machine = Vm::new(compiled.program);
    registry
        .bind_vm_cached(&mut machine)
        .expect("counted numeric host should bind");
    assert_eq!(
        machine.run().expect("Number.isInteger should execute"),
        VmStatus::Halted
    );
    assert_eq!(machine.stack(), &[Value::Bool(true)]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn standard_api_number_is_integer_accepts_negative_zero() {
    let compiled = vm::compile_source_with_flavor_and_options(
        "Number.isInteger(-0);",
        flavor(),
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("Number.isInteger(-0) should compile");
    let mut machine = Vm::new(compiled.program);
    assert_eq!(
        machine.run().expect("negative zero should execute"),
        VmStatus::Halted
    );
    assert_eq!(machine.stack(), &[Value::Bool(true)]);
}

#[test]
fn relational_comparison_does_not_treat_negative_zero_as_less_than_zero() {
    let compiled = vm::compile_source_with_flavor_and_options(
        "-0 < 0;",
        flavor(),
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("negative-zero comparison should compile");
    let mut machine = Vm::new(compiled.program);
    assert_eq!(
        machine
            .run()
            .expect("negative-zero comparison should execute"),
        VmStatus::Halted
    );
    assert_eq!(machine.stack(), &[Value::Bool(false)]);
}

#[test]
fn strict_equality_treats_negative_zero_as_equal_to_zero() {
    let compiled = vm::compile_source_with_flavor_and_options(
        "-0 === 0;",
        flavor(),
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("negative-zero equality should compile");
    let mut machine = Vm::new(compiled.program);
    assert_eq!(
        machine
            .run()
            .expect("negative-zero equality should execute"),
        VmStatus::Halted
    );
    assert_eq!(machine.stack(), &[Value::Bool(true)]);
}

#[test]
fn strict_numeric_equality_uses_javascript_number_precision() {
    let source = "const a: number = 9007199254740992;\nconst b: number = a + 1;\na === b;\n";
    let compiled = vm::compile_source_with_flavor_and_options(
        source,
        flavor(),
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("typed number equality should compile");
    let mut machine = Vm::new(compiled.program);
    assert_eq!(
        machine.run().expect("typed number equality should execute"),
        VmStatus::Halted
    );
    assert_eq!(
        machine.stack(),
        &[Value::Bool(true)],
        "JavaScript rounds both values to the same binary64 Number"
    );
}

#[test]
fn strict_numeric_equality_evaluates_each_operand_once_left_to_right() {
    for (operator, expected) in [("===", true), ("!==", false)] {
        let source =
            format!("import {{ left, right }} from \"effects\";\nleft() {operator} right();\n");
        let events = Arc::new(Mutex::new(Vec::new()));
        let (compiled, registry) = strict_equality_effect_program(&source, Arc::clone(&events));
        let mut machine = Vm::new(compiled.program);
        registry
            .bind_vm_cached(&mut machine)
            .expect("operand recorder imports should bind");
        assert_eq!(
            machine
                .run()
                .expect("strict equality operands should execute"),
            VmStatus::Halted
        );
        assert_eq!(machine.stack(), &[Value::Bool(expected)], "{operator}");

        let recorded = events
            .lock()
            .expect("operand event recorder should not be poisoned")
            .clone();
        assert_eq!(recorded, ["left", "right"], "{operator} evaluation order");
        assert_eq!(
            recorded.iter().filter(|event| **event == "left").count(),
            1,
            "{operator} must evaluate the left operand once"
        );
        assert_eq!(
            recorded.iter().filter(|event| **event == "right").count(),
            1,
            "{operator} must evaluate the right operand once"
        );
    }
}

#[test]
fn standard_api_math_round_matches_javascript_numeric_edges() {
    let cases = [
        ("-1.5", -1.0),
        ("-2.5", -2.0),
        ("-0.5", -0.0),
        ("-0.1", -0.0),
        ("-0", -0.0),
        ("-0.49999999999999994", -0.0),
        ("-0.5000000000000001", -1.0),
        ("0.49999999999999994", 0.0),
        ("0.5", 1.0),
        ("0.5000000000000001", 1.0),
        ("9007199254740992.0", 9_007_199_254_740_992.0),
        ("1e308 * 10", f64::INFINITY),
        ("-1e308 * 10", f64::NEG_INFINITY),
        ("Math.sqrt(-1)", f64::NAN),
    ];

    for (expression, expected) in cases {
        let source = format!("Math.round({expression});");
        let compiled = vm::compile_source_with_flavor_and_options(
            &source,
            flavor(),
            pd_vm_compat_frontends::compile_options(),
        )
        .unwrap_or_else(|error| panic!("Math.round({expression}) should compile: {error}"));
        let mut machine = Vm::new(compiled.program);
        assert_eq!(
            machine.run().expect("Math.round case should execute"),
            VmStatus::Halted,
            "Math.round({expression})"
        );
        let actual = match machine.stack().last() {
            Some(Value::Int(value)) => *value as f64,
            Some(Value::Float(value)) => *value,
            other => panic!("Math.round({expression}) returned {other:?}"),
        };
        if expected.is_nan() {
            assert!(
                actual.is_nan(),
                "Math.round({expression}) should preserve NaN"
            );
        } else {
            assert_eq!(actual, expected, "Math.round({expression})");
            if expected == 0.0 && expected.is_sign_negative() {
                assert!(
                    actual.is_sign_negative(),
                    "Math.round({expression}) should return -0"
                );
            }
        }
    }

    let source = r#"import { next } from "effects";
const rounded = Math.round(next());
rounded;
"#;
    let calls = Arc::new(AtomicUsize::new(0));
    let (compiled, registry, _) = counted_host_program(
        source,
        vm::HostTypeSchema::Number,
        Value::Int(9_007_199_254_740_993),
        Arc::clone(&calls),
    );
    let mut machine = Vm::new(compiled.program);
    registry
        .bind_vm_cached(&mut machine)
        .expect("counted numeric host should bind for Math.round");
    assert_eq!(
        machine.run().expect("Math.round call should execute"),
        VmStatus::Halted
    );
    assert_eq!(machine.stack(), &[Value::Float(9_007_199_254_740_992.0)]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn standard_api_number_is_nan_is_noncoercing_for_known_scalars() {
    let source = r#"import { next } from "effects";
const textResult = Number.isNaN(next());
const booleanResult = Number.isNaN(true);
const nullResult = Number.isNaN(null);
const numberResult = Number.isNaN(42);
(textResult ? 1 : 0)
    + (booleanResult ? 2 : 0)
    + (nullResult ? 4 : 0)
    + (numberResult ? 8 : 0);
"#;
    let calls = Arc::new(AtomicUsize::new(0));
    let (compiled, registry, _) = counted_host_program(
        source,
        vm::HostTypeSchema::String,
        Value::string("NaN"),
        Arc::clone(&calls),
    );
    let mut machine = Vm::new(compiled.program);
    registry
        .bind_vm_cached(&mut machine)
        .expect("counted string host should bind for Number.isNaN");
    assert_eq!(
        machine.run().expect("Number.isNaN should execute"),
        VmStatus::Halted
    );
    assert_eq!(machine.stack(), &[Value::Int(0)]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn standard_api_number_is_nan_rejects_unsupported_shapes_at_the_argument() {
    let source = "const values: number[] = [1];\nNumber.isNaN(values);\n";
    let error = vm::compile_source_with_flavor_and_options(
        source,
        flavor(),
        pd_vm_compat_frontends::compile_options(),
    )
    .err()
    .expect("Number.isNaN must reject non-scalar arrays");
    let rendered = error.to_string();
    let start = source.find("Number.isNaN(").unwrap() + "Number.isNaN(".len();
    let end = start + "values".len();
    assert!(
        rendered.contains("Number.isNaN does not support non-scalar input"),
        "unexpected diagnostic: {rendered}"
    );
    assert!(
        rendered.contains(&format!("[{start}..{end}]")),
        "unsupported input diagnostic should identify the argument: {rendered}"
    );
}

#[test]
fn standard_api_global_is_nan_remains_numeric_only_with_argument_span() {
    let source = "const input: string = \"42\";\nconst answer = isNaN(input);\nanswer;\n";
    let error = vm::compile_source_with_flavor_and_options(
        source,
        flavor(),
        pd_vm_compat_frontends::compile_options(),
    )
    .err()
    .expect("global isNaN coercion is outside the strict typed subset");
    let rendered = error.to_string();
    let start = source.find("isNaN(input)").unwrap() + "isNaN(".len();
    let end = start + "input".len();
    assert!(
        rendered.contains("global isNaN only accepts numeric inputs in this typed subset"),
        "unexpected diagnostic: {rendered}"
    );
    assert!(
        rendered.contains(&format!("[{start}..{end}]")),
        "global isNaN diagnostic should identify the argument: {rendered}"
    );
}

#[test]
fn exported_arrow_parameters_remain_strictly_typed() {
    let source = "export const identity = (value) => value;\n";
    let error = pd_vm_compat_frontends::collect_typescript_type_hints(source)
        .expect_err("an exported arrow without context or annotation must fail strict typing");
    assert!(
        error.to_string().contains("noImplicitAny"),
        "unexpected diagnostic: {error}"
    );
    assert!(
        error.to_string().contains("value"),
        "unexpected diagnostic: {error}"
    );
}
