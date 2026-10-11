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
