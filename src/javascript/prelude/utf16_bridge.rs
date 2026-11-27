//! Private completed-IR managed UTF-16 adapters. No public globals/coercion.
//!
//! The caller owns all managed roots. Core arrays contain only copied Ints;
//! Core pushes do not enter the B1 allocator. Publication uses the existing
//! pending-node transaction and roots the complete output before returning.
#![allow(dead_code)] // Private prerequisite; no public global registration.
use super::super::{heap as layout, lowerer::assert_stmt, representation as value};
use super::object::{add, and, call, eq, field, int, lt, num, sub, var};
use std::collections::HashSet;
use vm::{
    AssignmentKind, BuiltinFunction as B, Expr as E, FrontendIr, ParseError, Stmt as S,
    TypeSchema as T, ValueType,
};

const TO: &str = "__compat_es5_managed_to_units";
const FROM: &str = "__compat_es5_units_to_managed";
pub(super) const CEILING: i64 = 65_536;
pub(in crate::javascript) struct Entries {
    pub managed_to_units: u16,
    pub units_to_managed: u16,
}
fn error(text: &str) -> ParseError {
    ParseError::at_span(
        vm::Span::new(0, 0, 0),
        format!("managed UTF16 bridge: {text}"),
    )
}
fn array() -> T {
    T::Array(Box::new(T::Int))
}
fn named(name: &str) -> T {
    T::Named(name.into(), vec![])
}
fn object(fields: &[(&str, T)]) -> T {
    T::Object(
        fields
            .iter()
            .map(|(k, v)| ((*k).into(), v.clone()))
            .collect(),
    )
}
fn schema(ir: &FrontendIr, name: &str, expected: T) -> Result<(), ParseError> {
    let declaration = ir
        .struct_schemas
        .get(name)
        .ok_or_else(|| error(&format!("missing schema {name}")))?;
    if declaration.name != name
        || !declaration.type_params.is_empty()
        || declaration.body_schema != expected
    {
        return Err(error(&format!("incompatible schema {name}")));
    }
    Ok(())
}
fn require(ir: &FrontendIr, name: &str, args: Vec<T>, result: T) -> Result<u16, ParseError> {
    let mut found = ir.functions.iter().filter(|f| f.name == name);
    let f = found
        .next()
        .ok_or_else(|| error(&format!("missing actual helper {name}")))?;
    if found.next().is_some()
        || f.exported
        || !f.type_params.is_empty()
        || usize::from(f.arity) != args.len()
        || f.arg_schemas != args.into_iter().map(Some).collect::<Vec<_>>()
        || f.return_schema != Some(result)
        || ir.function_impls.get(&f.index).is_none_or(|body| {
            body.param_slots.len() != usize::from(f.arity) || !body.capture_copies.is_empty()
        })
    {
        return Err(error(&format!("incompatible actual helper {name}")));
    }
    Ok(f.index)
}
struct Abi {
    carrier: T,
    read: u16,
    write: u16,
    decode: u16,
    base: u16,
    begin: u16,
    publish: u16,
}
impl Abi {
    fn discover(ir: &FrontendIr) -> Result<Self, ParseError> {
        let carrier = named("__CompatRefCarrier");
        schema(
            ir,
            "__CompatRefCarrier",
            object(&[(
                "dispatcher",
                T::Callable {
                    params: vec![T::Number; 3],
                    result: Box::new(T::Array(Box::new(T::Number))),
                },
            )]),
        )?;
        schema(ir, value::TYPE_NAME, value::declaration().body_schema)?;
        schema(
            ir,
            layout::TYPE_ALLOCATION,
            value::allocation_declaration().body_schema,
        )?;
        require(
            ir,
            "__compat_ref_make",
            vec![T::Array(Box::new(T::Number))],
            carrier.clone(),
        )?;
        let read = require(
            ir,
            "__compat_ref_read",
            vec![carrier.clone(), T::Number],
            T::Number,
        )?;
        let write = require(
            ir,
            "__compat_ref_write",
            vec![carrier.clone(), T::Number, T::Number],
            T::Number,
        )?;
        let decode = require(ir, "__compat_es5_decode_int", vec![T::Number], T::Int)?;
        let base = require(
            ir,
            layout::CHECKED_BASE_HELPER,
            vec![carrier.clone(), T::Int, T::Int],
            T::Int,
        )?;
        let begin = require(
            ir,
            layout::NODE_BEGIN_HELPER,
            vec![carrier.clone(), T::Int, T::Int],
            value::allocation_schema(),
        )?;
        let publish = require(
            ir,
            layout::NODE_PUBLISH_HELPER,
            vec![carrier.clone(), T::Int, T::Int, T::Int, T::Int, T::Int],
            value::schema(),
        )?;
        Ok(Self {
            carrier,
            read,
            write,
            decode,
            base,
            begin,
            publish,
        })
    }
    fn decoded(&self, c: u16, address: E) -> E {
        call(
            self.decode,
            vec![num(call(self.read, vec![var(c), num(address)]))],
        )
    }
}
fn let_(index: u16, schema: T, expr: E) -> S {
    S::Let {
        index,
        declared_schema: Some(schema),
        expr,
        line: 1,
    }
}
fn set(index: u16, expr: E) -> S {
    S::Assign {
        kind: AssignmentKind::Set,
        index,
        expr,
        line: 1,
    }
}
fn loop_(condition: E, body: Vec<S>) -> S {
    S::While {
        condition,
        body,
        line: 1,
    }
}
fn bounded(expr: E, maximum: i64) -> E {
    and(
        E::Not(Box::new(lt(expr.clone(), int(0)))),
        E::Not(Box::new(lt(int(maximum), expr))),
    )
}
fn borrowed(index: u16) -> E {
    E::Borrow(Box::new(var(index)))
}

/// Append only to completed typed frontend IR, after all Lowerer reservations.
/// Every schema/index/local check finishes before the first mutation.
pub(in crate::javascript) fn append(ir: &mut FrontendIr) -> Result<Entries, ParseError> {
    let abi = Abi::discover(ir)?;
    let mut indices = HashSet::new();
    if ir
        .functions
        .iter()
        .any(|f| !indices.insert(f.index) || f.name == TO || f.name == FROM)
    {
        return Err(error("duplicate helper/index"));
    }
    let maximum = ir
        .functions
        .iter()
        .map(|f| f.index)
        .chain(ir.function_impls.keys().copied())
        .max()
        .unwrap_or(0);
    let first = maximum
        .checked_add(1)
        .ok_or_else(|| error("function limit"))?;
    let second = first
        .checked_add(1)
        .ok_or_else(|| error("function limit"))?;
    let start = u16::try_from(ir.locals).map_err(|_| error("local limit"))?;
    let end = start.checked_add(20).ok_or_else(|| error("local limit"))?;
    let mut declarations = vec![];
    let c = start;
    let string = start + 1;
    let base = start + 2;
    let extent = start + 3;
    let len = start + 4;
    let cursor = start + 5;
    let unit = start + 6;
    let out = start + 7;
    let body = vec![
        assert_stmt(eq(field(var(string), "tag"), int(value::STRING)), 1),
        let_(
            base,
            T::Int,
            call(
                abi.base,
                vec![
                    var(c),
                    field(var(string), "ref_slot"),
                    field(var(string), "generation"),
                ],
            ),
        ),
        assert_stmt(eq(abi.decoded(c, var(base)), int(layout::KIND_STRING)), 1),
        let_(
            extent,
            T::Int,
            abi.decoded(c, sub(var(base), int(layout::RAW_HEADER_CELLS))),
        ),
        assert_stmt(
            E::Not(Box::new(lt(
                var(extent),
                int(layout::RAW_HEADER_CELLS + layout::STRING_HEADER_CELLS),
            ))),
            1,
        ),
        let_(len, T::Int, abi.decoded(c, add(var(base), int(2)))),
        assert_stmt(bounded(var(len), CEILING), 1),
        assert_stmt(
            E::Not(Box::new(lt(
                sub(
                    var(extent),
                    int(layout::RAW_HEADER_CELLS + layout::STRING_HEADER_CELLS),
                ),
                var(len),
            ))),
            1,
        ),
        let_(cursor, T::Int, int(0)),
        let_(unit, T::Int, int(0)),
        let_(out, array(), call(B::ArrayNew.call_index(), vec![])),
        loop_(
            lt(var(cursor), var(len)),
            vec![
                set(
                    unit,
                    abi.decoded(
                        c,
                        add(
                            add(var(base), int(layout::STRING_HEADER_CELLS)),
                            var(cursor),
                        ),
                    ),
                ),
                assert_stmt(bounded(var(unit), 65535), 1),
                set(
                    out,
                    call(B::ArrayPush.call_index(), vec![E::MoveVar(out), var(unit)]),
                ),
                set(cursor, add(var(cursor), int(1))),
            ],
        ),
    ];
    super::heap::declare_helper(
        ir,
        &mut declarations,
        TO,
        first,
        2,
        vec!["carrier".into(), "string".into()],
        vec![Some(abi.carrier.clone()), Some(value::schema())],
        Some(array()),
        ValueType::Array,
        vec![c, string],
        body,
        var(out),
        1,
    );
    let c = start + 8;
    let units = start + 9;
    let frame = start + 10;
    let entry = start + 11;
    let len = start + 12;
    let cursor = start + 13;
    let unit = start + 14;
    let allocation = start + 15;
    let base = start + 16;
    let ack = start + 17;
    let slot = start + 18;
    let generation = start + 19;
    let body = vec![
        let_(
            len,
            T::Int,
            call(B::Len.call_index(), vec![borrowed(units)]),
        ),
        assert_stmt(bounded(var(len), CEILING), 1),
        let_(cursor, T::Int, int(0)),
        let_(unit, T::Int, int(0)),
        // Complete source validation before entering the pending transaction.
        loop_(
            lt(var(cursor), var(len)),
            vec![
                set(
                    unit,
                    call(B::Get.call_index(), vec![borrowed(units), var(cursor)]),
                ),
                assert_stmt(bounded(var(unit), 65535), 1),
                set(cursor, add(var(cursor), int(1))),
            ],
        ),
        // node_begin accounts n+3 data cells, raw overhead, page and occupied
        // extent. Its existing global pressure/collection checks are unchanged.
        let_(
            allocation,
            value::allocation_schema(),
            call(
                abi.begin,
                vec![
                    var(c),
                    add(var(len), int(layout::STRING_HEADER_CELLS)),
                    int(layout::KIND_STRING),
                ],
            ),
        ),
        let_(base, T::Int, field(var(allocation), "base")),
        let_(slot, T::Int, field(var(allocation), "slot")),
        let_(generation, T::Int, field(var(allocation), "generation")),
        let_(
            ack,
            T::Number,
            call(
                abi.write,
                vec![var(c), num(add(var(base), int(2))), num(var(len))],
            ),
        ),
        set(cursor, int(0)),
        // No managed allocation or callback is permitted between begin/publish.
        loop_(
            lt(var(cursor), var(len)),
            vec![
                set(
                    unit,
                    call(B::Get.call_index(), vec![borrowed(units), var(cursor)]),
                ),
                set(
                    ack,
                    call(
                        abi.write,
                        vec![
                            var(c),
                            num(add(
                                add(var(base), int(layout::STRING_HEADER_CELLS)),
                                var(cursor),
                            )),
                            num(var(unit)),
                        ],
                    ),
                ),
                set(cursor, add(var(cursor), int(1))),
            ],
        ),
    ];
    super::heap::declare_helper(
        ir,
        &mut declarations,
        FROM,
        second,
        4,
        vec![
            "carrier".into(),
            "units".into(),
            "frame".into(),
            "entry".into(),
        ],
        vec![Some(abi.carrier), Some(array()), Some(T::Int), Some(T::Int)],
        Some(value::schema()),
        ValueType::Map,
        vec![c, units, frame, entry],
        body,
        call(
            abi.publish,
            vec![
                var(c),
                var(slot),
                var(generation),
                var(base),
                var(frame),
                var(entry),
            ],
        ),
        1,
    );
    ir.locals = usize::from(end);
    ir.stmts.extend(declarations);
    Ok(Entries {
        managed_to_units: first,
        units_to_managed: second,
    })
}

#[cfg(test)]
mod native_vector_gate_tests {
    use super::*;
    use vm::SourcePlugin;

    /// One real production JavaScript-realm parse: the completed Standalone-phase
    /// IR with its managed UTF-16 bridge already present. Validation only, no VM or AOT.
    fn realm_ir() -> FrontendIr {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = vm::SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::compiler::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        crate::JAVASCRIPT_ES5_PLUGIN
            .parse_source("var o={};", &context)
            .expect("real JavaScript realm prelude IR")
    }

    /// The exact documented physical carrier record with the given dispatcher
    /// arity and result schema.
    fn carrier(arity: usize, result: T) -> T {
        T::Object(
            [(
                "dispatcher".into(),
                T::Callable {
                    params: vec![T::Number; arity],
                    result: Box::new(result),
                },
            )]
            .into(),
        )
    }

    #[test]
    fn carrier_gate_requires_one_native_number_vector_result() {
        let ir = realm_ir();
        // Positive control: the completed production IR already carries the
        // native Array(Number) dispatcher result; this gate only validates it.
        super::Abi::discover(&ir)
            .expect("the current native Number vector dispatcher result is accepted");
        // Negatives are genuinely wrong under the current contract: the
        // obsolete nominal envelope, a wrong element type and a wrong
        // dispatcher arity all fail closed and leave the IR untouched.
        for (label, body) in [
            (
                "obsolete nominal envelope",
                carrier(3, T::Named("__CompatRefResult".into(), vec![])),
            ),
            (
                "wrong element type",
                carrier(3, T::Array(Box::new(T::String))),
            ),
            (
                "wrong dispatcher arity",
                carrier(2, T::Array(Box::new(T::Number))),
            ),
        ] {
            let mut rejected = ir.clone();
            rejected
                .struct_schemas
                .get_mut("__CompatRefCarrier")
                .expect("the real carrier schema must exist")
                .body_schema = body;
            let before = format!("{rejected:?}");
            let error = super::Abi::discover(&rejected)
                .err()
                .expect("a wrong dispatcher result must fail closed");
            assert!(
                format!("{error:?}").contains("incompatible schema __CompatRefCarrier"),
                "{label}: {error:?}"
            );
            assert_eq!(
                format!("{rejected:?}"),
                before,
                "{label}: rejection is transactional"
            );
        }
        eprintln!("UTF16_CARRIER_GATE positive=1 negatives=3 transactional=true");
    }
}
