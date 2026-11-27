//! Private completed-IR Number-to-UTF16 producer. Public wiring is separate.
//! The fixed ASCII digit source is Core Float Display on the qualified build.
#![allow(dead_code)]
use super::super::{lowerer::assert_stmt, representation};
use super::object::{add, and, call, eq, int, lt, sub, var};
use super::utf16_bridge;
use std::collections::HashSet;
use vm::{
    AssignmentKind, BuiltinFunction as B, Expr as E, FrontendIr, ParseError, Stmt as S,
    TypeSchema as T, ValueType,
};

const UNITS: &str = "__compat_es5_number_string_units";
const MANAGED: &str = "__compat_es5_number_string_managed";
pub(in crate::javascript) struct Entries {
    pub units: u16,
    pub managed: u16,
}
fn next_index(ir: &FrontendIr) -> Result<u16, ParseError> {
    let mut indices = HashSet::new();
    for f in &ir.functions {
        if !indices.insert(f.index) || f.index >= 0xFF90 {
            return Err(error("duplicate/reserved function index"));
        }
        if f.name == UNITS || f.name == MANAGED {
            return Err(error("duplicate formatter helper"));
        }
    }
    if ir
        .local_bindings
        .iter()
        .any(|(name, slot)| name == UNITS || name == MANAGED || usize::from(*slot) >= ir.locals)
    {
        return Err(error("local binding collision/limit"));
    }
    for implementation in ir.function_impls.values() {
        if implementation
            .param_slots
            .iter()
            .chain(
                implementation
                    .capture_copies
                    .iter()
                    .flat_map(|(a, b)| [a, b]),
            )
            .any(|slot| usize::from(*slot) >= ir.locals)
        {
            return Err(error("local limit"));
        }
    }
    if let Some(metadata) = &ir.host_api_metadata {
        for index in metadata.function_indices() {
            let f = ir
                .functions
                .iter()
                .find(|f| f.index == index)
                .ok_or_else(|| error("orphan host metadata"))?;
            let candidates = metadata
                .candidates(index)
                .ok_or_else(|| error("missing host metadata"))?;
            if candidates.is_empty()
                || ir.function_impls.contains_key(&index)
                || candidates
                    .iter()
                    .any(|c| c.name != f.name || c.params.len() != usize::from(f.arity))
            {
                return Err(error("malformed host metadata"));
            }
        }
    }
    let next = ir
        .functions
        .iter()
        .map(|f| f.index)
        .chain(ir.function_impls.keys().copied())
        .chain(
            ir.host_api_metadata
                .iter()
                .flat_map(|m| m.function_indices()),
        )
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| error("function limit"))?;
    if next >= 0xFF90 {
        return Err(error("reserved function limit"));
    }
    Ok(next)
}
fn require_schema(ir: &FrontendIr, name: &str, body: T) -> Result<(), ParseError> {
    let s = ir
        .struct_schemas
        .get(name)
        .ok_or_else(|| error("missing bridge schema"))?;
    if s.name != name || !s.type_params.is_empty() || s.body_schema != body {
        return Err(error("incompatible bridge schema"));
    }
    Ok(())
}
fn actual_bridge(
    ir: &FrontendIr,
    index: u16,
    name: &str,
    args: Vec<T>,
    result: T,
) -> Result<(), ParseError> {
    let mut matches = ir.functions.iter().filter(|f| f.name == name);
    let f = matches
        .next()
        .ok_or_else(|| error("missing actual bridge"))?;
    if matches.next().is_some()
        || f.index != index
        || f.exported
        || !f.type_params.is_empty()
        || usize::from(f.arity) != args.len()
        || f.arg_schemas != args.into_iter().map(Some).collect::<Vec<_>>()
        || f.return_schema != Some(result)
        || ir.function_impls.get(&index).is_none_or(|b| {
            b.param_slots.len() != usize::from(f.arity)
                || b.param_slots.iter().collect::<HashSet<_>>().len() != b.param_slots.len()
                || !b.capture_copies.is_empty()
        })
    {
        return Err(error("incompatible actual bridge"));
    }
    Ok(())
}
/// The existing carrier and actual-bridge ABI checks, shared by production
/// append and the completed-IR gate test. Order and error text are unchanged.
fn validate_abi(ir: &FrontendIr, bridge: &utf16_bridge::Entries) -> Result<T, ParseError> {
    let named = |name: &str| T::Named(name.into(), vec![]);
    let carrier = named("__CompatRefCarrier");
    require_schema(
        ir,
        "__CompatRefCarrier",
        T::Object(
            [(
                "dispatcher".into(),
                T::Callable {
                    params: vec![T::Number; 3],
                    result: Box::new(T::Array(Box::new(T::Number))),
                },
            )]
            .into(),
        ),
    )?;
    require_schema(
        ir,
        representation::TYPE_NAME,
        representation::declaration().body_schema,
    )?;
    actual_bridge(
        ir,
        bridge.managed_to_units,
        "__compat_es5_managed_to_units",
        vec![carrier.clone(), representation::schema()],
        array(),
    )?;
    actual_bridge(
        ir,
        bridge.units_to_managed,
        "__compat_es5_units_to_managed",
        vec![carrier.clone(), array(), T::Int, T::Int],
        representation::schema(),
    )?;
    Ok(carrier)
}
/// Caller appends the real bridge once. No new root frame or permanent root.
/// The complete source producer and publication wrapper commit transactionally.
pub(in crate::javascript) fn append(
    ir: &mut FrontendIr,
    bridge: &utf16_bridge::Entries,
) -> Result<Entries, ParseError> {
    next_index(ir)?;
    let carrier = validate_abi(ir, bridge)?;
    let mut staged = ir.clone();
    let units = emit_units(&mut staged)?;
    let managed = units
        .checked_add(1)
        .filter(|i| *i < 0xFF90)
        .ok_or_else(|| error("function limit"))?;
    let start = u16::try_from(staged.locals).map_err(|_| error("local limit"))?;
    let end = start.checked_add(5).ok_or_else(|| error("local limit"))?;
    let c = start;
    let input = start + 1;
    let frame = start + 2;
    let entry = start + 3;
    let output = start + 4;
    let mut declarations = vec![];
    super::heap::declare_helper(
        &mut staged,
        &mut declarations,
        MANAGED,
        managed,
        4,
        vec![
            "carrier".into(),
            "number".into(),
            "frame".into(),
            "entry".into(),
        ],
        vec![Some(carrier), Some(T::Number), Some(T::Int), Some(T::Int)],
        Some(representation::schema()),
        ValueType::Map,
        vec![c, input, frame, entry],
        vec![let_(output, array(), call(units, vec![var(input)]))],
        call(
            bridge.units_to_managed,
            vec![var(c), E::MoveVar(output), var(frame), var(entry)],
        ),
        1,
    );
    staged.locals = usize::from(end);
    staged.stmts.extend(declarations);
    *ir = staged;
    Ok(Entries { units, managed })
}

fn array() -> T {
    T::Array(Box::new(T::Int))
}
fn error(message: &str) -> ParseError {
    ParseError::at_span(vm::Span::new(0, 0, 0), format!("Number string: {message}"))
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
fn bi(b: B, args: Vec<E>) -> E {
    call(b.call_index(), args)
}
fn borrowed(index: u16) -> E {
    E::Borrow(Box::new(var(index)))
}
fn loop_(condition: E, body: Vec<S>) -> S {
    S::While {
        condition,
        body,
        line: 1,
    }
}
fn branch(condition: E, then_branch: Vec<S>, else_branch: Vec<S>) -> S {
    S::IfElse {
        condition,
        then_branch,
        else_branch,
        line: 1,
    }
}
fn not(e: E) -> E {
    E::Not(Box::new(e))
}
fn choose(condition: E, then_expr: E, else_expr: E) -> E {
    E::IfElse {
        condition: Box::new(condition),
        then_expr: Box::new(then_expr),
        else_expr: Box::new(else_expr),
    }
}
fn bounded(e: E, lo: i64, hi: i64) -> E {
    and(not(lt(e.clone(), int(lo))), not(lt(int(hi), e)))
}
fn push(out: u16, unit: E) -> S {
    set(out, bi(B::ArrayPush, vec![E::MoveVar(out), unit]))
}

/// Append to completed IR, with all changes committed together.
pub(in crate::javascript) fn append_units(ir: &mut FrontendIr) -> Result<u16, ParseError> {
    let mut staged = ir.clone();
    let entry = emit_units(&mut staged)?;
    *ir = staged;
    Ok(entry)
}
fn emit_units(ir: &mut FrontendIr) -> Result<u16, ParseError> {
    let index = next_index(ir)?;
    let start = u16::try_from(ir.locals).map_err(|_| error("local limit"))?;
    let end = start.checked_add(18).ok_or_else(|| error("local limit"))?;
    let input = start;
    let number = start + 1;
    let raw = start + 2;
    let bytes = start + 3;
    let len = start + 4;
    let cursor = start + 5;
    let out = start + 6;
    let negative = start + 7;
    let point = start + 8;
    let first = start + 9;
    let last = start + 10;
    let byte = start + 11;
    let k = start + 12;
    let n = start + 13;
    let j = start + 14;
    let exponent = start + 15;
    let digit = start + 16;
    let seen_point = start + 17;
    let get = |offset| bi(B::Get, vec![borrowed(bytes), offset]);
    // Offset j addresses selected significant digits, skipping the single point.
    let selected = |offset: E| {
        let position = add(var(first), offset);
        get(choose(
            and(
                lt(var(first), var(point)),
                not(lt(position.clone(), var(point))),
            ),
            add(position.clone(), int(1)),
            position,
        ))
    };
    let copy_digits = |condition| {
        loop_(
            condition,
            vec![push(out, selected(var(j))), set(j, add(var(j), int(1)))],
        )
    };
    let prefix = vec![
        let_(
            number,
            T::Number,
            E::Div(Box::new(var(input)), Box::new(E::Float(1.0))),
        ),
        let_(out, array(), bi(B::ArrayNew, vec![])),
    ];
    let mut body = vec![
        let_(negative, T::Bool, lt(var(number), E::Float(0.0))),
        set(
            number,
            choose(var(negative), E::Neg(Box::new(var(number))), var(number)),
        ),
        let_(raw, T::String, bi(B::ToString, vec![var(number)])),
        let_(bytes, T::Bytes, bi(B::BytesFromUtf8, vec![borrowed(raw)])),
        let_(len, T::Int, bi(B::Len, vec![borrowed(bytes)])),
        assert_stmt(bounded(var(len), 1, 343), 1),
        let_(cursor, T::Int, int(0)),
        let_(point, T::Int, var(len)),
        let_(first, T::Int, var(len)),
        let_(last, T::Int, int(0)),
        let_(byte, T::Int, int(0)),
        let_(seen_point, T::Bool, E::Bool(false)),
        loop_(
            lt(var(cursor), var(len)),
            vec![
                set(byte, get(var(cursor))),
                branch(
                    eq(var(byte), int(46)),
                    vec![
                        assert_stmt(not(var(seen_point)), 1),
                        set(seen_point, E::Bool(true)),
                        set(point, var(cursor)),
                    ],
                    vec![
                        assert_stmt(bounded(var(byte), 48, 57), 1),
                        branch(
                            not(eq(var(byte), int(48))),
                            vec![
                                branch(
                                    eq(var(first), var(len)),
                                    vec![set(first, var(cursor))],
                                    vec![],
                                ),
                                set(last, var(cursor)),
                            ],
                            vec![],
                        ),
                    ],
                ),
                set(cursor, add(var(cursor), int(1))),
            ],
        ),
        assert_stmt(lt(var(first), var(len)), 1),
        let_(k, T::Int, sub(add(var(last), int(1)), var(first))),
        branch(
            and(lt(var(first), var(point)), lt(var(point), var(last))),
            vec![set(k, sub(var(k), int(1)))],
            vec![],
        ),
        let_(n, T::Int, sub(var(point), var(first))),
        branch(
            lt(var(point), var(first)),
            vec![set(n, add(var(n), int(1)))],
            vec![],
        ),
        assert_stmt(bounded(var(k), 1, 17), 1),
        assert_stmt(bounded(var(n), -323, 309), 1),
        let_(j, T::Int, int(0)),
        let_(exponent, T::Int, int(0)),
        let_(digit, T::Int, int(0)),
        branch(var(negative), vec![push(out, int(45))], vec![]),
    ];
    body.push(branch(
        and(not(lt(var(n), var(k))), not(lt(int(21), var(n)))),
        vec![
            copy_digits(lt(var(j), var(k))),
            loop_(
                lt(var(j), var(n)),
                vec![push(out, int(48)), set(j, add(var(j), int(1)))],
            ),
        ],
        vec![branch(
            bounded(var(n), 1, 21),
            vec![
                copy_digits(lt(var(j), var(n))),
                push(out, int(46)),
                copy_digits(lt(var(j), var(k))),
            ],
            vec![branch(
                bounded(var(n), -5, 0),
                vec![
                    push(out, int(48)),
                    push(out, int(46)),
                    set(cursor, int(0)),
                    loop_(
                        lt(var(cursor), E::Neg(Box::new(var(n)))),
                        vec![push(out, int(48)), set(cursor, add(var(cursor), int(1)))],
                    ),
                    copy_digits(lt(var(j), var(k))),
                ],
                vec![
                    push(out, selected(int(0))),
                    set(j, int(1)),
                    branch(
                        lt(int(1), var(k)),
                        vec![push(out, int(46)), copy_digits(lt(var(j), var(k)))],
                        vec![],
                    ),
                    push(out, int(101)),
                    set(exponent, sub(var(n), int(1))),
                    branch(
                        lt(var(exponent), int(0)),
                        vec![
                            push(out, int(45)),
                            set(exponent, E::Neg(Box::new(var(exponent)))),
                        ],
                        vec![push(out, int(43))],
                    ),
                    // All exponent operations are Int subtraction, never guest division.
                    set(digit, int(0)),
                    loop_(
                        not(lt(var(exponent), int(100))),
                        vec![
                            set(exponent, sub(var(exponent), int(100))),
                            set(digit, add(var(digit), int(1))),
                        ],
                    ),
                    branch(
                        lt(int(0), var(digit)),
                        vec![push(out, add(var(digit), int(48)))],
                        vec![],
                    ),
                    set(cursor, var(digit)),
                    set(digit, int(0)),
                    loop_(
                        not(lt(var(exponent), int(10))),
                        vec![
                            set(exponent, sub(var(exponent), int(10))),
                            set(digit, add(var(digit), int(1))),
                        ],
                    ),
                    branch(
                        E::Or(
                            Box::new(lt(int(0), var(cursor))),
                            Box::new(lt(int(0), var(digit))),
                        ),
                        vec![push(out, add(var(digit), int(48)))],
                        vec![],
                    ),
                    push(out, add(var(exponent), int(48))),
                ],
            )],
        )],
    ));
    let token = |text: &[u8]| text.iter().map(|b| push(out, int(i64::from(*b)))).collect();
    let mut complete = prefix;
    complete.push(branch(
        not(eq(var(number), var(number))),
        token(b"NaN"),
        vec![branch(
            eq(var(number), E::Float(0.0)),
            token(b"0"),
            vec![branch(
                eq(var(number), E::Float(f64::INFINITY)),
                token(b"Infinity"),
                vec![branch(
                    eq(var(number), E::Float(f64::NEG_INFINITY)),
                    token(b"-Infinity"),
                    body,
                )],
            )],
        )],
    ));
    complete.push(assert_stmt(
        bounded(bi(B::Len, vec![borrowed(out)]), 1, 25),
        1,
    ));
    let mut declarations = vec![];
    super::heap::declare_helper(
        ir,
        &mut declarations,
        UNITS,
        index,
        1,
        vec!["number".into()],
        vec![Some(T::Number)],
        Some(array()),
        ValueType::Array,
        vec![input],
        complete,
        var(out),
        1,
    );
    ir.locals = usize::from(end);
    ir.stmts.extend(declarations);
    Ok(index)
}

#[cfg(test)]
mod native_vector_gate_tests {
    use super::*;
    use vm::SourcePlugin;

    /// One real production JavaScript-realm parse: the completed Standalone-phase
    /// IR with its managed UTF-16 bridge already present. Validation only, no VM or guest.
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

    /// The real completed-IR bridge entries; the gate never appends a bridge.
    fn bridge_index(ir: &FrontendIr, name: &str) -> u16 {
        let mut found = ir.functions.iter().filter(|f| f.name == name);
        let entry = found.next().expect("real completed bridge helper");
        assert!(found.next().is_none(), "duplicate bridge helper {name}");
        entry.index
    }

    #[test]
    fn carrier_gate_requires_one_native_number_vector_result() {
        let ir = realm_ir();
        let bridge = utf16_bridge::Entries {
            managed_to_units: bridge_index(&ir, "__compat_es5_managed_to_units"),
            units_to_managed: bridge_index(&ir, "__compat_es5_units_to_managed"),
        };
        // Positive control: the completed production IR already carries the
        // native Array(Number) dispatcher result; this gate only validates it.
        super::validate_abi(&ir, &bridge)
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
            let error = super::validate_abi(&rejected, &bridge)
                .err()
                .expect("a wrong dispatcher result must fail closed");
            assert!(
                format!("{error:?}").contains("incompatible bridge schema"),
                "{label}: {error:?}"
            );
            assert_eq!(
                format!("{rejected:?}"),
                before,
                "{label}: rejection is transactional"
            );
        }
        eprintln!("NUMBER_CARRIER_GATE positive=1 negatives=3 transactional=true");
    }
}
