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

pub(super) fn schema() -> TypeSchema {
    TypeSchema::Named(TYPE_NAME.to_owned(), Vec::new())
}

pub(super) fn reference_schema() -> TypeSchema {
    TypeSchema::Named(TYPE_REF.to_owned(), Vec::new())
}

pub(super) fn completion_schema() -> TypeSchema {
    TypeSchema::Named(TYPE_COMPLETION.to_owned(), Vec::new())
}

pub(super) fn reference_declaration() -> StructDecl {
    StructDecl {
        name: TYPE_REF.to_owned(),
        type_params: Vec::new(),
        body_schema: TypeSchema::Object(HashMap::from([
            ("kind".to_owned(), TypeSchema::Int),
            ("base".to_owned(), schema()),
            ("key_slot".to_owned(), TypeSchema::Int),
            ("strict".to_owned(), TypeSchema::Bool),
        ])),
        origin: StructDeclOrigin::Guest,
    }
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

pub(super) fn array(base: Expr, generation: Expr) -> Expr {
    let mut result = call(BuiltinFunction::MapNew, Vec::new());
    for (key, value) in [
        ("tag", Expr::Int(OBJECT)),
        ("number", Expr::Float(0.0)),
        ("flag", Expr::Bool(false)),
        ("ref_slot", base),
        ("generation", generation),
    ] {
        result = call(
            BuiltinFunction::Set,
            vec![result, Expr::String(key.to_owned()), value],
        );
    }
    result
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

/// A boxed plain-object or function value: tag OBJECT with the logical
/// realm base in `ref_slot`. The heap header kind (3 plain object, 4
/// function) distinguishes them; identity compares base+generation.
pub(super) fn object_value(base: Expr, generation: Expr) -> Expr {
    let mut result = call(BuiltinFunction::MapNew, Vec::new());
    for (key, value) in [
        ("tag", Expr::Int(OBJECT)),
        ("number", Expr::Float(0.0)),
        ("flag", Expr::Bool(false)),
        ("ref_slot", base),
        ("generation", generation),
    ] {
        result = call(
            BuiltinFunction::Set,
            vec![result, Expr::String(key.to_owned()), value],
        );
    }
    result
}

pub(super) fn function_value(base: Expr, generation: Expr) -> Expr {
    object_value(base, generation)
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

/// Operational member Reference `{kind, base, key_slot, strict}`. `base`
/// is the evaluated receiver JsValue, `key_slot` the Int base of the
/// UTF-16 key string node, `strict` the caller-code strictness.
pub(super) fn reference(base: Expr, key_slot: Expr, strict: Expr) -> Expr {
    let mut result = call(BuiltinFunction::MapNew, Vec::new());
    for (key, value) in [
        ("kind", Expr::Int(0)),
        ("base", base),
        ("key_slot", key_slot),
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
    let mut result = call(BuiltinFunction::MapNew, Vec::new());
    for (key, expr) in [
        ("kind", Expr::Int(kind)),
        ("value", value),
        ("has_value", has_value),
        ("target", Expr::Int(0)),
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

/// A boxed JS string: tag 4 with the logical realm base in `ref_slot`.
/// Payload lives in the shared realm heap, never in this record.
pub(super) fn string(base: Expr, generation: Expr) -> Expr {
    let mut result = call(BuiltinFunction::MapNew, Vec::new());
    for (key, value) in [
        ("tag", Expr::Int(STRING)),
        ("number", Expr::Float(0.0)),
        ("flag", Expr::Bool(false)),
        ("ref_slot", base),
        ("generation", generation),
    ] {
        result = call(
            BuiltinFunction::Set,
            vec![result, Expr::String(key.to_owned()), value],
        );
    }
    result
}

/// Guest string-equality helper target: the owning carrier plus the flat
/// function index of the appended `(Carrier, JsValue, JsValue) -> Bool`
/// content comparator. The comparator runs only for matching STRING tags,
/// so it never reads a non-string base.
pub(super) struct StringEquality {
    pub carrier: Expr,
    pub index: u16,
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
        condition: Box::new(Expr::Eq(Box::new(tag.clone()), Box::new(Expr::Int(NUMBER)))),
        then_expr: Box::new(Expr::Eq(
            Box::new(field(left, "number")),
            Box::new(field(right, "number")),
        )),
        else_expr: Box::new(Expr::IfElse {
            condition: Box::new(Expr::Eq(
                Box::new(tag.clone()),
                Box::new(Expr::Int(BOOLEAN)),
            )),
            then_expr: Box::new(Expr::Eq(
                Box::new(field(left, "flag")),
                Box::new(field(right, "flag")),
            )),
            else_expr: Box::new(Expr::IfElse {
                condition: Box::new(Expr::Eq(Box::new(tag.clone()), Box::new(Expr::Int(STRING)))),
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
                    condition: Box::new(Expr::Eq(Box::new(tag), Box::new(Expr::Int(OBJECT)))),
                    then_expr: Box::new(identity),
                    else_expr: Box::new(Expr::Bool(true)),
                }),
            }),
        }),
    };
    Expr::And(Box::new(same_tag), Box::new(payload))
}
