use vm::{
    TypeSchema, Value, Vm, VmStatus, compile_source_with_flavor_and_options, compiler::FrontendId,
};

fn options() -> vm::CompileSourceFileOptions {
    pd_vm_compat_frontends::compile_options()
}

fn flavor() -> vm::SourceFlavor {
    vm::SourceFlavor::Plugin(FrontendId::new("typescript"))
}

#[test]
fn optional_generic_alias_and_named_record_keep_their_schema_through_execution() {
    let source = r#"import type { Int } from "rustscript:types";
interface Profile { name: string; age: Int; nickname?: string; }
type Maybe<T> = T | null;
function preserve<T>(value: T): Maybe<T> { return value; }
const profile: Profile = { name: "Ada", age: 37, nickname: "Ada" };
const result = preserve<Profile>(profile);
const name = profile.name;
const nickname = profile.nickname;
name;
nickname;
result;
"#;
    let hints = pd_vm_compat_frontends::collect_typescript_type_hints(source)
        .expect("record and generic alias schemas should pass checked hinting");
    assert_eq!(
        hints
            .iter()
            .find(|hint| hint.name == "name")
            .unwrap()
            .schema,
        Some(TypeSchema::String)
    );
    let nickname_hint = hints.iter().find(|hint| hint.name == "nickname").unwrap();
    assert_eq!(
        nickname_hint.schema,
        Some(TypeSchema::Optional(Box::new(TypeSchema::String)))
    );
    assert_eq!(nickname_hint.inferred_type, "string | null");
    let result_hint = hints.iter().find(|hint| hint.name == "result").unwrap();
    assert!(matches!(
        &result_hint.schema,
        Some(TypeSchema::Optional(inner))
            if matches!(inner.as_ref(), TypeSchema::Named(name, args)
                if name.starts_with("__guest_type_m") && args.is_empty())
    ));
    assert_eq!(result_hint.inferred_type, "Profile | null");

    let compiled = compile_source_with_flavor_and_options(source, flavor(), options())
        .expect("typed record and generic alias should compile");
    let mut machine = Vm::new(compiled.program);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    let stack = machine.stack();
    assert_eq!(stack.len(), 3);
    assert_eq!(stack[0], Value::string("Ada"));
    assert_eq!(stack[1], Value::string("Ada"));
    assert!(matches!(stack[2], Value::Map(_)));
}

#[test]
fn fixed_and_rest_tuple_parameter_schemas_are_public() {
    let source = r#"import type { Int } from "rustscript:types";
function keepPair(pair: [Int, string]): [Int, string] { return pair; }
function keepRest(rest: [Int, ...string[]]): [Int, ...string[]] { return rest; }
"#;
    let hints = pd_vm_compat_frontends::collect_typescript_type_hints(source)
        .expect("fixed and rest tuples should be checked");
    let schema = |name: &str| {
        hints
            .iter()
            .find(|hint| hint.name == name)
            .and_then(|hint| hint.schema.clone())
            .unwrap_or_else(|| panic!("missing schema for {name}"))
    };
    assert_eq!(
        schema("pair"),
        TypeSchema::ArrayTuple(vec![TypeSchema::Int, TypeSchema::String])
    );
    assert_eq!(
        schema("rest"),
        TypeSchema::ArrayTupleRest {
            prefix: vec![TypeSchema::Int],
            rest: Box::new(TypeSchema::Array(Box::new(TypeSchema::String))),
        }
    );
}

#[test]
fn complex_generic_constraints_are_rejected_at_the_type_parameter() {
    let source = "function constrained<T extends string>(value: T): T { return value; }\n";
    let result = compile_source_with_flavor_and_options(source, flavor(), options());
    let error = match result {
        Ok(_) => panic!("complex generic constraints must be rejected"),
        Err(error) => error,
    };
    let rendered = error.to_string();
    assert!(rendered.contains("constraint"), "{rendered}");
    assert!(rendered.contains("[21..37]"), "unexpected span: {rendered}");
}

#[test]
fn numeric_literals_keep_the_signed_i64_boundary_and_neighboring_f64_values() {
    let source = r#"const positiveBoundary = 9223372036854775808;
const negativeBoundary = -9223372036854775808;
const insidePositive = 9223372036854774784;
const insideNegative = -9223372036854774784;
const outsidePositive = 9223372036854777856;
const outsideNegative = -9223372036854777856;
positiveBoundary;
negativeBoundary;
insidePositive;
insideNegative;
outsidePositive;
outsideNegative;
"#;
    let hints = pd_vm_compat_frontends::collect_typescript_type_hints(source)
        .expect("finite numeric literals should remain representable");
    let schema = |name: &str| {
        hints
            .iter()
            .find(|hint| hint.name == name)
            .and_then(|hint| hint.schema.clone())
            .unwrap_or_else(|| panic!("missing schema for {name}"))
    };
    assert_eq!(schema("positiveBoundary"), TypeSchema::Float);
    assert_eq!(schema("negativeBoundary"), TypeSchema::Int);
    assert_eq!(schema("insidePositive"), TypeSchema::Int);
    assert_eq!(schema("insideNegative"), TypeSchema::Int);
    assert_eq!(schema("outsidePositive"), TypeSchema::Float);
    assert_eq!(schema("outsideNegative"), TypeSchema::Float);

    let compiled = compile_source_with_flavor_and_options(source, flavor(), options())
        .expect("boundary numeric values should compile");
    let mut machine = Vm::new(compiled.program);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(
        machine.stack(),
        &[
            Value::Float(9_223_372_036_854_775_808.0),
            Value::Int(i64::MIN),
            Value::Int(9_223_372_036_854_774_784),
            Value::Int(-9_223_372_036_854_774_784),
            Value::Float(9_223_372_036_854_777_856.0),
            Value::Float(-9_223_372_036_854_777_856.0),
        ]
    );

    let too_large_int = compile_source_with_flavor_and_options(
        "import type { Int } from \"rustscript:types\";\nconst value: Int = 9223372036854775808;\n",
        flavor(),
        options(),
    );
    assert!(
        too_large_int.is_err(),
        "positive 2^63 must not narrow to Int"
    );
}
