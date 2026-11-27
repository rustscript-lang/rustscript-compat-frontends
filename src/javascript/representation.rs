//! Compiler-side construction of the private, typed guest primitive value.
use std::collections::HashMap;
use vm::compiler::ir::{StructDecl, StructDeclOrigin};
use vm::{BuiltinFunction, Expr, TypeSchema};

pub(super) const TYPE_NAME: &str = "__CompatEs5Primitive";
pub(super) const UNDEFINED: i64 = 0;
pub(super) const NULL: i64 = 1;
pub(super) const BOOLEAN: i64 = 2;
pub(super) const NUMBER: i64 = 3;
pub(super) const STRING: i64 = 4;
pub(super) const OBJECT: i64 = 5;

pub(super) const TYPE_REF: &str = "__CompatEs5Reference";
pub(super) const TYPE_COMPLETION: &str = "__CompatEs5Completion";
/// Normal empty completion kind (falling off / empty body).
pub(super) const COMPLETION_NORMAL: i64 = 0;
/// Explicit `return` completion kind (with or without a value).
pub(super) const COMPLETION_RETURN: i64 = 1;
/// Guest `throw` pending kind. Undefined payloads still carry `has_value`.
pub(super) const COMPLETION_THROW: i64 = 2;
/// Source `break` pending kind. The positive target names the statically
/// resolved iteration/label boundary; `target` 0 stays the nonjump sentinel.
pub(super) const COMPLETION_BREAK: i64 = 3;
/// Source `continue` pending kind. Only iteration targets are legal.
pub(super) const COMPLETION_CONTINUE: i64 = 4;

pub(super) fn schema() -> TypeSchema {
    TypeSchema::Named(TYPE_NAME.to_owned(), Vec::new())
}

pub(super) fn reference_schema() -> TypeSchema {
    TypeSchema::Named(TYPE_REF.to_owned(), Vec::new())
}

pub(super) fn completion_schema() -> TypeSchema {
    TypeSchema::Named(TYPE_COMPLETION.to_owned(), Vec::new())
}

/// Source-function and dispatch boundary: `Result<Completion, JsValue>`.
/// Physical `ValueType::Map` stays on the declaration and does not replace this.
pub(super) fn value_result_schema() -> TypeSchema {
    TypeSchema::Result {
        ok: Box::new(schema()),
        err: Box::new(schema()),
    }
}

pub(super) fn logical_result_schema() -> TypeSchema {
    TypeSchema::Result {
        ok: Box::new(completion_schema()),
        err: Box::new(schema()),
    }
}

pub(super) fn result_ok(payload: Expr, span: vm::Span) -> Expr {
    Expr::ResultConstruct {
        ok: true,
        payload: Box::new(payload),
        result_schema: logical_result_schema(),
        span,
        semantic_id: None,
    }
}

pub(super) fn result_err(payload: Expr, span: vm::Span) -> Expr {
    Expr::ResultConstruct {
        ok: false,
        payload: Box::new(payload),
        result_schema: logical_result_schema(),
        span,
        semantic_id: None,
    }
}

pub(super) fn reference_declaration() -> StructDecl {
    StructDecl {
        name: TYPE_REF.to_owned(),
        type_params: Vec::new(),
        body_schema: TypeSchema::Object(HashMap::from([
            ("kind".to_owned(), TypeSchema::Int),
            ("base".to_owned(), schema()),
            ("key_slot".to_owned(), TypeSchema::Int),
            ("key_generation".to_owned(), TypeSchema::Int),
            ("strict".to_owned(), TypeSchema::Bool),
        ])),
        origin: StructDeclOrigin::Guest,
    }
}

/// Private pending-allocation handle `{slot, generation, base}` returned by
/// the node reservation helper. The physical base is compiler-internal and
/// never becomes a user value field.
pub(super) fn allocation_declaration() -> StructDecl {
    StructDecl {
        name: super::heap::TYPE_ALLOCATION.to_owned(),
        type_params: Vec::new(),
        body_schema: TypeSchema::Object(HashMap::from([
            ("slot".to_owned(), TypeSchema::Int),
            ("generation".to_owned(), TypeSchema::Int),
            ("base".to_owned(), TypeSchema::Int),
        ])),
        origin: StructDeclOrigin::Guest,
    }
}

pub(super) fn allocation_schema() -> TypeSchema {
    TypeSchema::Named(super::heap::TYPE_ALLOCATION.to_owned(), Vec::new())
}

/// Build a pending-allocation handle record from dynamic Int fields (used
/// for the reservation helper's return value).
pub(super) fn allocation(slot: Expr, generation: Expr, base: Expr) -> Expr {
    let mut result = call(BuiltinFunction::MapNew, Vec::new());
    for (key, value) in [("slot", slot), ("generation", generation), ("base", base)] {
        result = call(
            BuiltinFunction::Set,
            vec![result, Expr::String(key.to_owned()), value],
        );
    }
    result
}

pub(super) fn completion_declaration() -> StructDecl {
    StructDecl {
        name: TYPE_COMPLETION.to_owned(),
        type_params: Vec::new(),
        body_schema: TypeSchema::Object(HashMap::from([
            ("kind".to_owned(), TypeSchema::Int),
            ("value".to_owned(), schema()),
            ("has_value".to_owned(), TypeSchema::Bool),
            ("target".to_owned(), TypeSchema::Int),
        ])),
        origin: StructDeclOrigin::Guest,
    }
}

pub(super) fn declaration() -> StructDecl {
    StructDecl {
        name: TYPE_NAME.to_owned(),
        type_params: Vec::new(),
        body_schema: TypeSchema::Object(HashMap::from([
            ("tag".to_owned(), TypeSchema::Int),
            ("number".to_owned(), TypeSchema::Float),
            ("flag".to_owned(), TypeSchema::Bool),
            ("ref_slot".to_owned(), TypeSchema::Int),
            ("generation".to_owned(), TypeSchema::Int),
        ])),
        origin: StructDeclOrigin::Guest,
    }
}

fn call(builtin: BuiltinFunction, args: Vec<Expr>) -> Expr {
    Expr::Call(builtin.call_index(), Vec::new(), args, None, None)
}

fn record(tag: i64, number: Expr, flag: Expr) -> Expr {
    let mut result = call(BuiltinFunction::MapNew, Vec::new());
    for (key, value) in [
        ("tag", Expr::Int(tag)),
        ("number", number),
        ("flag", flag),
        ("ref_slot", Expr::Int(0)),
        ("generation", Expr::Int(0)),
    ] {
        result = call(
            BuiltinFunction::Set,
            vec![result, Expr::String(key.to_owned()), value],
        );
    }
    result
}

pub(super) fn undefined() -> Expr {
    record(UNDEFINED, Expr::Float(0.0), Expr::Bool(false))
}

pub(super) fn null() -> Expr {
    record(NULL, Expr::Float(0.0), Expr::Bool(false))
}

pub(super) fn number(value: Expr) -> Expr {
    record(NUMBER, value, Expr::Bool(false))
}

pub(super) fn boolean(value: Expr) -> Expr {
    record(BOOLEAN, Expr::Float(0.0), value)
}

pub(super) fn field(slot: u16, key: &str) -> Expr {
    call(
        BuiltinFunction::Get,
        vec![Expr::Var(slot), Expr::String(key.to_owned())],
    )
}

/// Read a field from an arbitrary record expression (used for nested
/// `Get(Get(...))` chains on Reference/Completion carriers).
pub(super) fn field_of(record: Expr, key: &str) -> Expr {
    call(
        BuiltinFunction::Get,
        vec![record, Expr::String(key.to_owned())],
    )
}

/// Build a JsValue record from dynamic field expressions (used when
/// reconstructing a stored tagged payload inside guest helpers).
pub(super) fn dynamic_value(
    tag: Expr,
    number: Expr,
    flag: Expr,
    ref_slot: Expr,
    generation: Expr,
) -> Expr {
    let mut result = call(BuiltinFunction::MapNew, Vec::new());
    for (key, value) in [
        ("tag", tag),
        ("number", number),
        ("flag", flag),
        ("ref_slot", ref_slot),
        ("generation", generation),
    ] {
        result = call(
            BuiltinFunction::Set,
            vec![result, Expr::String(key.to_owned()), value],
        );
    }
    result
}

/// Operational member Reference `{kind, base, key_slot, key_generation,
/// strict}`. `base` is the evaluated receiver JsValue, `key_slot` plus
/// `key_generation` name the UTF-16 key string node, `strict` the
/// caller-code strictness.
pub(super) fn reference(base: Expr, key_slot: Expr, key_generation: Expr, strict: Expr) -> Expr {
    let mut result = call(BuiltinFunction::MapNew, Vec::new());
    for (key, value) in [
        ("kind", Expr::Int(0)),
        ("base", base),
        ("key_slot", key_slot),
        ("key_generation", key_generation),
        ("strict", strict),
    ] {
        result = call(
            BuiltinFunction::Set,
            vec![result, Expr::String(key.to_owned()), value],
        );
    }
    result
}

/// Uniform Completion `{kind, value, has_value, target}`. Normal empty
/// carries Undefined payload with `has_value=false`; explicit return
/// (including bare `return;`) carries the value with `has_value=true`.
pub(super) fn completion(kind: i64, value: Expr, has_value: Expr) -> Expr {
    completion_from_parts(Expr::Int(kind), value, has_value, Expr::Int(0))
}

pub(super) fn completion_from_parts(
    kind: Expr,
    value: Expr,
    has_value: Expr,
    target: Expr,
) -> Expr {
    let mut result = call(BuiltinFunction::MapNew, Vec::new());
    for (key, expr) in [
        ("kind", kind),
        ("value", value),
        ("has_value", has_value),
        ("target", target),
    ] {
        result = call(
            BuiltinFunction::Set,
            vec![result, Expr::String(key.to_owned()), expr],
        );
    }
    result
}

pub(super) fn completion_normal() -> Expr {
    completion(COMPLETION_NORMAL, undefined(), Expr::Bool(false))
}

pub(super) fn completion_return(value: Expr) -> Expr {
    completion(COMPLETION_RETURN, value, Expr::Bool(true))
}

pub(super) fn completion_throw(value: Expr) -> Expr {
    completion(COMPLETION_THROW, value, Expr::Bool(true))
}

/// Guest string-equality helper target: the owning carrier plus the flat
/// function index of the appended `(Carrier, JsValue, JsValue) -> Bool`
/// content comparator. The comparator runs only for matching STRING tags,
/// so it never reads a non-string base.
pub(super) struct StringEquality {
    pub carrier: Expr,
    pub index: u16,
}

fn as_number(expr: Expr) -> Expr {
    super::lowerer::Lowerer::as_number(expr)
}

fn tag_is(tag: Expr, code: i64) -> Expr {
    Expr::Eq(
        Box::new(as_number(tag)),
        Box::new(as_number(Expr::Int(code))),
    )
}

pub(super) fn strict_equal(left: u16, right: u16, strings: Option<StringEquality>) -> Expr {
    boolean(strict_equal_flag(left, right, strings))
}

/// Unboxed strict-equality flag (no JsValue wrapper). Used by `!==` so
/// inversion branches on an unboxed Bool temporary; native SSA rejects
/// `Not` directly on a Tagged Get.
pub(super) fn strict_equal_flag(left: u16, right: u16, strings: Option<StringEquality>) -> Expr {
    let tag = field(left, "tag");
    let same_tag = Expr::Eq(Box::new(tag.clone()), Box::new(field(right, "tag")));
    let identity = Expr::And(
        Box::new(Expr::Eq(
            Box::new(field(left, "ref_slot")),
            Box::new(field(right, "ref_slot")),
        )),
        Box::new(Expr::Eq(
            Box::new(field(left, "generation")),
            Box::new(field(right, "generation")),
        )),
    );
    let payload = Expr::IfElse {
        condition: Box::new(tag_is(tag.clone(), NUMBER)),
        then_expr: Box::new(Expr::Eq(
            Box::new(field(left, "number")),
            Box::new(field(right, "number")),
        )),
        else_expr: Box::new(Expr::IfElse {
            condition: Box::new(tag_is(tag.clone(), BOOLEAN)),
            then_expr: Box::new(Expr::Eq(
                Box::new(field(left, "flag")),
                Box::new(field(right, "flag")),
            )),
            else_expr: Box::new(Expr::IfElse {
                condition: Box::new(tag_is(tag.clone(), STRING)),
                then_expr: Box::new(match strings {
                    Some(equal) => Expr::Call(
                        equal.index,
                        Vec::new(),
                        vec![equal.carrier, Expr::Var(left), Expr::Var(right)],
                        None,
                        None,
                    ),
                    // Strings always lower through the realm path, which
                    // appends the comparator; fail closed if that ever lapses.
                    None => Expr::Bool(false),
                }),
                else_expr: Box::new(Expr::IfElse {
                    condition: Box::new(tag_is(tag, OBJECT)),
                    then_expr: Box::new(identity),
                    else_expr: Box::new(Expr::Bool(true)),
                }),
            }),
        }),
    };
    Expr::And(Box::new(same_tag), Box::new(payload))
}

#[cfg(test)]
mod caller_domain_tests {
    use super::*;
    use vm::{SourcePlugin, Value};

    #[test]
    fn p7_caller_private_record_runtime_domains() {
        struct Constructors;
        impl SourcePlugin for Constructors {
            fn descriptor(&self) -> vm::SourceFrontendDescriptor {
                SourcePlugin::descriptor(&crate::JAVASCRIPT_ES5_PLUGIN)
            }
            fn parse_source(
                &self,
                source: &str,
                context: &vm::SourceFrontendContext<'_>,
            ) -> Result<vm::FrontendIr, vm::ParseError> {
                let mut ir =
                    SourcePlugin::parse_source(&crate::JAVASCRIPT_ES5_PLUGIN, source, context)?;
                for declaration in [declaration(), completion_declaration()] {
                    ir.struct_schemas
                        .insert(declaration.name.clone(), declaration);
                }
                let mut records = vec![
                    completion_normal(),
                    completion_return(undefined()),
                    completion_throw(null()),
                    completion(COMPLETION_BREAK, undefined(), Expr::Bool(false)),
                    completion(COMPLETION_CONTINUE, undefined(), Expr::Bool(false)),
                ];
                for tag in UNDEFINED..=OBJECT {
                    records.push(record(tag, Expr::Float(-0.0), Expr::Bool(true)));
                }
                records.push(result_ok(
                    completion_return(undefined()),
                    vm::Span::new(0, 0, 0),
                ));
                records.push(result_err(
                    boolean(Expr::Bool(true)),
                    vm::Span::new(0, 0, 0),
                ));
                ir.stmts.extend(
                    records
                        .into_iter()
                        .map(|expr| vm::Stmt::Expr { expr, line: 1 }),
                );
                Ok(ir)
            }
        }
        let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
            "0;",
            crate::javascript_es5_flavor(),
            vm::CompileSourceFileOptions::new().with_source_plugin(&Constructors),
        )
        .unwrap();
        let mut machine = compiled.into_vm().unwrap();
        machine.set_jit_config(vm::JitConfig {
            enabled: false,
            ..vm::JitConfig::default()
        });
        machine.set_fuel(10_000_000);
        assert_eq!(machine.run().unwrap(), vm::VmStatus::Halted);
        assert_eq!(machine.aot_exec_count(), 0);
        assert_eq!(machine.jit_native_exec_count(), 0);
        assert!(!machine.jit_config().enabled);
        let field = |v: &Value, key: &str| {
            let Value::Map(m) = v else {
                panic!("private record: {v:?}")
            };
            m.get(&Value::string(key)).unwrap().clone()
        };
        let values = &machine.stack()[machine.stack().len() - 13..];
        for (kind, record) in values[..5].iter().enumerate() {
            assert_eq!(field(record, "kind"), Value::Int(kind as i64));
            assert_eq!(
                field(record, "has_value"),
                Value::Bool(kind == 1 || kind == 2)
            );
        }
        for (tag, record) in values[5..11].iter().enumerate() {
            assert_eq!(field(record, "tag"), Value::Int(tag as i64));
            assert_eq!(field(record, "flag"), Value::Bool(true));
            assert!(
                matches!(field(record, "number"), Value::Float(n) if n == 0.0 && n.is_sign_negative())
            );
        }
        assert_eq!(field(&values[11], "tag"), Value::Bool(true));
        assert_eq!(field(&values[12], "tag"), Value::Bool(false));
        machine.reset_for_reuse().unwrap();
        assert_eq!(machine.host_context().resource_count(), 0);
        eprintln!(
            "P7_CALLER_DOMAINS constructors_Completion_kind=Int0..4 primitive_tag=Int0..5 has_value=Bool Result_tag=Bool Number=-0.0 trace=0 aot=0"
        );
    }
}
