//! ES5 numeric intrinsics: ordinary objects, checked guest calls and Float payloads.
use super::super::{
    heap as layout,
    lowerer::{Lowerer, assert_stmt},
    representation as value,
};
use super::object::*;
use vm::{BuiltinFunction as B, Expr as E, FrontendIr, ParseError, Stmt as S, TypeSchema as T};
#[path = "decimal_round.rs"]
mod decimal_round;
#[path = "math_ext.rs"]
pub(super) mod math_ext;
#[path = "stdlib_number.rs"]
mod number;
#[path = "number_to_string.rs"]
mod number_to_string;

pub(crate) const FIRST_CODE: i64 = 512;
pub(crate) const OBJECT_VALUE_OF: i64 = 534;
pub(crate) const OBJECT_TO_STRING: i64 = 535;
/// Parser, primitive converter, decimal rounder, and numeric call. The
/// receiver-aware `Object.prototype` adapter lives in the ordinary object
/// runtime, not in this numeric block.
pub(crate) const COUNT: u16 = 4;
pub(crate) const METHODS: [(&str, i64); 18] = [
    ("abs", 1),
    ("acos", 1),
    ("asin", 1),
    ("atan", 1),
    ("atan2", 2),
    ("ceil", 1),
    ("cos", 1),
    ("exp", 1),
    ("floor", 1),
    ("log", 1),
    ("max", 2),
    ("min", 2),
    ("pow", 2),
    ("random", 0),
    ("round", 1),
    ("sin", 1),
    ("sqrt", 1),
    ("tan", 1),
];
pub(crate) const GLOBALS: [(&str, i64); 4] = [
    ("parseInt", 2),
    ("parseFloat", 1),
    ("isNaN", 1),
    ("isFinite", 1),
];
/// Public Micro extensions. Codes are explicit so bootstrap, dispatch, arity
/// and collision share one table. They follow URI 536..539 and do not shift
/// the existing Math 512..529 or numeric-global 530..533 codes.
pub(crate) const EXTENSIONS: [(&str, i64, i64); 7] = [
    ("sign", 1, 540),
    ("imul", 2, 541),
    ("clz32", 1, 542),
    ("fround", 1, 543),
    ("trunc", 1, 544),
    ("log2", 1, 545),
    ("log10", 1, 546),
];
pub(crate) fn global_code(index: usize) -> i64 {
    FIRST_CODE + METHODS.len() as i64 + index as i64
}
pub(crate) fn occupied_codes() -> Vec<i64> {
    let mut codes = Vec::with_capacity(METHODS.len() + GLOBALS.len() + EXTENSIONS.len() + 2);
    codes.extend((0..METHODS.len()).map(|index| FIRST_CODE + index as i64));
    codes.extend((0..GLOBALS.len()).map(global_code));
    codes.extend([OBJECT_VALUE_OF, OBJECT_TO_STRING]);
    codes.extend(EXTENSIONS.iter().map(|entry| entry.2));
    codes
}
fn extension_code(name: &str) -> i64 {
    EXTENSIONS
        .iter()
        .find(|entry| entry.0 == name)
        .map(|entry| entry.2)
        .expect("math extension")
}
fn accepted_numeric_code(code: u16) -> E {
    let legacy_end = FIRST_CODE + (METHODS.len() + GLOBALS.len()) as i64;
    let ext_lo = EXTENSIONS[0].2;
    let ext_hi = EXTENSIONS[EXTENSIONS.len() - 1].2 + 1;
    or(
        and(
            not(lt(var(code), int(FIRST_CODE))),
            lt(var(code), int(legacy_end)),
        ),
        and(not(lt(var(code), int(ext_lo))), lt(var(code), int(ext_hi))),
    )
}
fn descriptor_converts_two(code: u16) -> E {
    let mut two = E::Bool(false);
    for (index, (name, arity)) in METHODS.iter().enumerate() {
        if *arity == 2 && *name != "max" && *name != "min" {
            two = or(two, eq(var(code), int(FIRST_CODE + index as i64)));
        }
    }
    for (index, (_, arity)) in GLOBALS.iter().enumerate() {
        if *arity == 2 {
            two = or(two, eq(var(code), int(global_code(index))));
        }
    }
    for (_, arity, ext) in EXTENSIONS {
        if arity == 2 {
            two = or(two, eq(var(code), int(ext)));
        }
    }
    two
}
const CONSTANTS: [(&str, f64); 8] = [
    ("E", std::f64::consts::E),
    ("LN10", std::f64::consts::LN_10),
    ("LN2", std::f64::consts::LN_2),
    ("LOG2E", std::f64::consts::LOG2_E),
    ("LOG10E", std::f64::consts::LOG10_E),
    ("PI", std::f64::consts::PI),
    ("SQRT1_2", std::f64::consts::FRAC_1_SQRT_2),
    ("SQRT2", std::f64::consts::SQRT_2),
];
pub(crate) fn used(lower: &Lowerer<'_>) -> bool {
    // Number.parseInt/parseFloat reuse this kernel. Selecting Number must
    // enable it without publishing the bare global roots.
    lower.math_used.get()
        || lower.numeric_globals.get() != 0
        || lower.number_namespace_used.get()
}
/// Function objects created for Number.parseInt/parseFloat when the matching
/// bare global was not selected. 20 cells / 1 node each, same as a global.
pub(crate) fn number_parse_static_request(globals: u8) -> (u64, u64) {
    let mut cells = 0;
    let mut nodes = 0;
    for i in 0..2 {
        if globals & (1 << i) == 0 {
            cells += 20;
            nodes += 1;
        }
    }
    (cells, nodes)
}
pub(crate) fn global_root(lower: &Lowerer<'_>, i: u8) -> i64 {
    5 + i64::from(lower.math_used.get())
        + i64::from((lower.numeric_globals.get() & ((1 << i) - 1)).count_ones())
}
/// Count every fresh node/key/row, helper frame and extra directory page.
/// Unsplittable reused tails remain checked by the unchanged runtime quota.
pub(crate) fn bootstrap_request(lower: &Lowerer<'_>) -> (u64, u64) {
    debug_assert_eq!(
        occupied_codes().len(),
        occupied_codes()
            .into_iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
    );
    let mut cells = (4 + 3 * 6 + 4 + 3 * 4) as u64;
    let mut nodes = 0;
    if lower.math_used.get() {
        let members = (METHODS.len() + CONSTANTS.len() + EXTENSIONS.len()) as u64;
        cells += 8 + 7 * members;
        nodes += 1;
        for (name, _) in METHODS {
            cells += 20 + 6 + name.len() as u64;
            nodes += 2;
        }
        for (name, _, _) in EXTENSIONS {
            cells += 20 + 6 + name.len() as u64;
            nodes += 2;
        }
        for (name, _) in CONSTANTS {
            cells += 6 + name.len() as u64;
            nodes += 1;
        }
    }
    for (i, _) in GLOBALS.iter().enumerate() {
        if lower.numeric_globals.get() & (1 << i) != 0 {
            cells += 20;
            nodes += 1;
        }
    }
    if lower.number_namespace_used.get() {
        let (extra_cells, extra_nodes) = number_parse_static_request(lower.numeric_globals.get());
        cells += extra_cells;
        nodes += extra_nodes;
    }
    // Math still charges both arms of each attempt (54 cells / 4 nodes) so
    // existing Math near-bound admission does not move. The reachable
    // producer materializes one key per attempt; no-Math object programs
    // charge that measured pair via default_value_key_request.
    let (key_cells, key_nodes) = default_value_key_request();
    cells += key_cells * 2;
    nodes += key_nodes * 2;
    (cells, nodes)
}

/// Cells and nodes for the two DefaultValue keys that can be live together.
/// `string` passes `STRING_HEADER + units` to `node_begin`, which adds
/// `RAW_HEADER`. valueOf is 7 units, toString is 8. One arm runs per attempt.
pub(crate) fn default_value_key_request() -> (u64, u64) {
    let block = |units: u64| {
        layout::RAW_HEADER_CELLS as u64 + layout::STRING_HEADER_CELLS as u64 + units
    };
    (block(7) + block(8), 2)
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn bootstrap(
    e: &mut Emit<'_, '_>,
    permanent: u16,
    stage: u16,
    object_proto: E,
    function_proto: E,
    allocate: u16,
    length_key: E,
    body: &mut Vec<S>,
) -> Result<(), ParseError> {
    let l = e.line;
    let math = e.boxed()?;
    body.push(assign(
        math,
        call(
            allocate,
            vec![
                var(e.carrier),
                int(3),
                object_proto,
                int((METHODS.len() + CONSTANTS.len() + EXTENSIONS.len()) as i64),
                int(0),
                int(0), int(0),
                var(permanent),
                int(5),
            ],
        ),
        l,
    ));
    let function = e.boxed()?;
    for (i, (name, length)) in METHODS.iter().enumerate() {
        let key = super::intrinsics::string(e, name, stage, 0, body)?;
        body.push(assign(
            function,
            call(
                allocate,
                vec![
                    var(e.carrier),
                    int(4),
                    function_proto.clone(),
                    int(1),
                    int(FIRST_CODE + i as i64),
                    int(*length), int(0),
                    var(stage),
                    int(1),
                ],
            ),
            l,
        ));
        super::intrinsics::trusted_data(
            e,
            var(function),
            0,
            length_key.clone(),
            boxed_number(E::Float(*length as f64)),
            (false, false, false),
            body,
        )?;
        super::intrinsics::trusted_data(
            e,
            var(math),
            i as i64,
            var(key),
            var(function),
            (true, false, true),
            body,
        )?;
    }
    for (i, (name, length, code)) in EXTENSIONS.iter().enumerate() {
        let key = super::intrinsics::string(e, name, stage, 0, body)?;
        body.push(assign(
            function,
            call(
                allocate,
                vec![
                    var(e.carrier),
                    int(4),
                    function_proto.clone(),
                    int(1),
                    int(*code),
                    int(*length), int(0),
                    var(stage),
                    int(1),
                ],
            ),
            l,
        ));
        super::intrinsics::trusted_data(
            e,
            var(function),
            0,
            length_key.clone(),
            boxed_number(E::Float(*length as f64)),
            (false, false, false),
            body,
        )?;
        super::intrinsics::trusted_data(
            e,
            var(math),
            (METHODS.len() + i) as i64,
            var(key),
            var(function),
            (true, false, true),
            body,
        )?;
    }
    for (i, (name, n)) in CONSTANTS.iter().enumerate() {
        let key = super::intrinsics::string(e, name, stage, 0, body)?;
        super::intrinsics::trusted_data(
            e,
            var(math),
            (METHODS.len() + EXTENSIONS.len() + i) as i64,
            var(key),
            boxed_number(E::Float(*n)),
            (false, false, false),
            body,
        )?;
    }
    Ok(())
}
pub(crate) fn bootstrap_globals(
    e: &mut Emit<'_, '_>,
    permanent: u16,
    _stage: u16,
    function_proto: E,
    allocate: u16,
    length_key: E,
    body: &mut Vec<S>,
) -> Result<(), ParseError> {
    if e.lower.numeric_globals.get() == 0 {
        return Ok(());
    }
    let l = e.line;
    let function = e.boxed()?;
    for (i, (_, length)) in GLOBALS.iter().enumerate() {
        if e.lower.numeric_globals.get() & (1 << i) == 0 {
            continue;
        }
        body.push(assign(
            function,
            call(
                allocate,
                vec![
                    var(e.carrier),
                    int(4),
                    function_proto.clone(),
                    int(1),
                    int(FIRST_CODE + METHODS.len() as i64 + i as i64),
                    int(*length), int(0),
                    var(permanent),
                    int(global_root(e.lower, i as u8)),
                ],
            ),
            l,
        ));
        super::intrinsics::trusted_data(
            e,
            var(function),
            0,
            length_key.clone(),
            boxed_number(E::Float(*length as f64)),
            (false, false, false),
            body,
        )?;
    }
    Ok(())
}

/// Ordinary [[DefaultValue]], preserving getter/call exceptions and receiver.
/// Missing prototype methods are not synthesized; they remain a realm
/// dependency. The numeric call path and the String charCodeAt adapter reuse
/// this one emitter with distinct private names and their own real object/
/// dispatch registrations.
#[allow(clippy::too_many_arguments)]
pub(crate) fn append_default_value(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    index: u16,
    name: &str,
    objects: ObjectHelpers,
    dispatch: u16,
) -> Result<(), ParseError> {
    let c = lower.slot(span)?;
    let input = lower.slot(span)?;
    let string_first = lower.slot(span)?;
    let h = lower.heap_ref(span)?.helpers;
    let error = lower.intrinsics.expect("intrinsics").type_error;
    let mut e = Emit::new(lower, span, c)?;
    let l = e.line;
    let sp = e.lower.ir_span(span);
    let schema = result_schema(value::schema());
    let out = e.temp(schema.clone(), result(schema.clone(), true, var(input), sp))?;
    let frame = e.integer()?;
    let key = e.boxed()?;
    let got = e.boxed()?;
    let err = e.boxed()?;
    let payload = e.boxed()?;
    let active = e.boolean()?;
    let callable = e.boolean()?;
    let raw = e.temp(
        schema.clone(),
        result(schema.clone(), true, value::undefined(), sp),
    )?;
    let called = e.temp(
        value::logical_result_schema(),
        value::result_ok(value::completion_normal(), sp),
    )?;
    let completion = e.temp(value::completion_schema(), value::completion_normal())?;
    let args = e.temp(
        T::Array(Box::new(value::schema())),
        call(B::ArrayNew.call_index(), vec![]),
    )?;
    let mut body = vec![
        assign(frame, call(h.root_enter, vec![var(c), int(4)]), l),
        e.root(var(frame), int(0), var(input))?,
        assign(active, tag_is(var(input), value::OBJECT), l),
    ];
    for attempt in 0..2 {
        let mut step = Vec::new();
        // Only the runtime-selected key is materialized. Each arm publishes the
        // selected node into frame entry 1 before the ordinary Get.
        let mut string_first_step = Vec::new();
        let selected_string_first = super::intrinsics::string(
            &mut e,
            if attempt == 0 { "toString" } else { "valueOf" },
            frame,
            1,
            &mut string_first_step,
        )?;
        string_first_step.push(assign(key, var(selected_string_first), l));
        let mut other_step = Vec::new();
        let selected_other = super::intrinsics::string(
            &mut e,
            if attempt == 0 { "valueOf" } else { "toString" },
            frame,
            1,
            &mut other_step,
        )?;
        other_step.push(assign(key, var(selected_other), l));
        step.push(branch(var(string_first), string_first_step, other_step, l));
        step.push(e.root(var(frame), int(1), var(key))?);
        step.push(assign(
            raw,
            call(
                objects.get,
                vec![
                    var(c),
                    value::reference(
                        var(input),
                        field(var(key), "ref_slot"),
                        field(var(key), "generation"),
                        E::Bool(false),
                    ),
                ],
            ),
            l,
        ));
        let mut success = vec![
            e.root(var(frame), int(2), var(got))?,
            assign(callable, E::Bool(false), l),
            branch(
                tag_is(var(got), value::OBJECT),
                vec![assign(
                    callable,
                    eq(e.decoded(e.base(var(got))), int(layout::KIND_FUNCTION)),
                    l,
                )],
                vec![],
                l,
            ),
        ];
        let mut invoke = vec![assign(
            called,
            call(dispatch, vec![var(c), var(got), var(input), var(args)]),
            l,
        )];
        let good = vec![
            assign(
                payload,
                choose(
                    eq(
                        field(var(completion), "kind"),
                        int(value::COMPLETION_RETURN),
                    ),
                    field(var(completion), "value"),
                    value::undefined(),
                ),
                l,
            ),
            e.root(var(frame), int(3), var(payload))?,
            branch(
                not(tag_is(var(payload), value::OBJECT)),
                vec![
                    assign(out, result(schema.clone(), true, var(payload), sp), l),
                    assign(active, E::Bool(false), l),
                ],
                vec![],
                l,
            ),
        ];
        let fail = vec![
            e.root(var(frame), int(3), var(err))?,
            assign(out, result(schema.clone(), false, var(err), sp), l),
            assign(active, E::Bool(false), l),
        ];
        invoke.push(matched(
            &mut e,
            called,
            value::logical_result_schema(),
            completion,
            err,
            good,
            fail.clone(),
        )?);
        success.push(branch(var(callable), invoke, vec![], l));
        step.push(matched(
            &mut e,
            raw,
            schema.clone(),
            got,
            err,
            success,
            fail,
        )?);
        body.push(branch(var(active), step, vec![], l));
    }
    body.push(branch(
        var(active),
        vec![
            assign(err, call(error, vec![var(c)]), l),
            e.root(var(frame), int(3), var(err))?,
            assign(out, result(schema.clone(), false, var(err), sp), l),
        ],
        vec![],
        l,
    ));
    body.push(e.effect(h.root_leave, vec![var(c), var(frame)])?);
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        name,
        index,
        vec![c, input, string_first],
        vec![carrier_schema.clone(), value::schema(), T::Bool],
        schema,
        body,
        var(out),
        l,
    );
    Ok(())
}

/// Existing numeric DefaultValue helper registration used by the numeric
/// call path; the emitter itself is shared with the String charCodeAt adapter.
#[allow(clippy::too_many_arguments)]
fn append_primitive(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    index: u16,
) -> Result<(), ParseError> {
    let objects = lower.objects.expect("object helpers");
    let dispatch = lower.helper_dispatch.expect("dispatch");
    append_default_value(
        lower,
        ir,
        carrier_schema,
        span,
        stmts,
        index,
        "__compat_es5_numeric_primitive",
        objects,
        dispatch,
    )
}
fn boxed_number(n: E) -> E {
    value::number(num(n))
}
fn nan() -> E {
    E::Float(f64::NAN)
}
fn div(a: E, b: E) -> E {
    E::Div(Box::new(a), Box::new(b))
}
fn neg(a: E) -> E {
    // A generic loop-join Var is Tagged; Core Neg may then select its Int
    // operation. Multiplication by a Float selects the real Number domain
    // while preserving zero signs, infinities and NaN.
    mul(E::Float(-1.0), a)
}
fn builtin(b: B, a: Vec<E>) -> E {
    let result = call(b.call_index(), a);
    if matches!(b, B::MathIsFinite | B::MathIsInfinite | B::MathIsNaN) {
        result
    } else {
        num(result)
    }
}

/// Shared ES5 pow kernel over two already-bound Float slots. The exponent
/// `±0` yields 1; a NaN exponent yields NaN; a unit base with an infinite
/// exponent yields NaN. Both inputs are scalar Float locals, so the unit and
/// infinity tests are ordinary comparisons and the kernel needs no MathAbs or
/// MathIsInfinite capability. Callers must bind each operand exactly once and
/// pass the slots, so effects and conversions are never duplicated.
pub(crate) fn number_power(base: u16, exponent: u16) -> E {
    choose(
        eq(var(exponent), E::Float(0.0)),
        E::Float(1.0),
        choose(
            not(eq(num(var(exponent)), num(var(exponent)))),
            nan(),
            choose(
                and(
                    or(
                        eq(num(var(base)), E::Float(1.0)),
                        eq(num(var(base)), E::Float(-1.0)),
                    ),
                    or(
                        eq(num(var(exponent)), E::Float(f64::INFINITY)),
                        eq(num(var(exponent)), E::Float(f64::NEG_INFINITY)),
                    ),
                ),
                nan(),
                builtin(B::MathPowF, vec![var(base), var(exponent)]),
            ),
        ),
    )
}
fn to_number(e: &Emit<'_, '_>, parse: u16, payload: E) -> E {
    choose(
        tag_is(payload.clone(), value::NUMBER),
        num(field(payload.clone(), "number")),
        choose(
            tag_is(payload.clone(), value::NULL),
            E::Float(0.0),
            choose(
                tag_is(payload.clone(), value::BOOLEAN),
                choose(field(payload.clone(), "flag"), E::Float(1.0), E::Float(0.0)),
                choose(
                    tag_is(payload.clone(), value::STRING),
                    call(parse, vec![var(e.carrier), payload, int(0), E::Float(0.0)]),
                    nan(),
                ),
            ),
        ),
    )
}
pub(crate) struct NumericFns {
    pub parser: u16,
    pub call: u16,
}

fn direct_primitive(payload: E) -> E {
    or(
        or(
            tag_is(payload.clone(), value::NUMBER),
            tag_is(payload.clone(), value::BOOLEAN),
        ),
        or(
            or(
                tag_is(payload.clone(), value::NULL),
                tag_is(payload.clone(), value::UNDEFINED),
            ),
            tag_is(payload, value::STRING),
        ),
    )
}

/// Runtime numeric boundary for broad source-call results. Never changes their Kind.
pub(crate) fn checked_number(payload: E) -> E {
    E::Block {
        stmts: vec![assert_stmt(tag_is(payload.clone(), value::NUMBER), 1)],
        expr: Box::new(num(field(payload, "number"))),
    }
}

/// Append only the existing StringNumericLiteral parser and decimal rounder.
/// The private pair has no ordinary-object, dispatch, or public Math dependency.
pub(crate) fn append_string_number_parser(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    names: (&str, &str),
) -> Result<u16, ParseError> {
    let parser = super::heap::fresh_index(lower, ir, span)?;
    let round = parser
        .checked_add(1)
        .ok_or_else(|| lower.error(span, "String Number parser index space exhausted"))?;
    decimal_round::append(lower, ir, carrier_schema, span, stmts, round, names.1)?;
    number::append(
        lower,
        ir,
        carrier_schema,
        span,
        stmts,
        parser,
        round,
        names.0,
    )?;
    ir.locals = ir.locals.max(usize::from(lower.next_slot));
    Ok(parser)
}

pub(crate) fn append(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    random: Option<u16>,
) -> Result<NumericFns, ParseError> {
    let start = super::heap::fresh_index(lower, ir, span)?;
    decimal_round::append(
        lower,
        ir,
        carrier_schema,
        span,
        stmts,
        start + 3,
        "__compat_es5_round_decimal",
    )?;
    number::append(
        lower,
        ir,
        carrier_schema,
        span,
        stmts,
        start,
        start + 3,
        "__compat_es5_numeric_scan",
    )?;
    append_primitive(lower, ir, carrier_schema, span, stmts, start + 1)?;
    let index = start + 2;
    let c = lower.slot(span)?;
    let code = lower.slot(span)?;
    let args = lower.slot(span)?;
    let receiver = lower.slot(span)?;
    let h = lower.heap_ref(span)?.helpers;
    let mut e = Emit::new(lower, span, c)?;
    let l = e.line;
    let sp = e.lower.ir_span(span);
    let out = e.temp(
        value::logical_result_schema(),
        value::result_ok(value::completion_normal(), sp),
    )?;
    let frame = e.integer()?;
    let n = e.integer()?;
    let count = e.integer()?;
    let i = e.integer()?;
    let input = e.boxed()?;
    let primitive = e.boxed()?;
    let first = e.boxed()?;
    let err = e.boxed()?;
    let schema = result_schema(value::schema());
    let converted = e.temp(
        schema.clone(),
        result(schema.clone(), true, value::undefined(), sp),
    )?;
    let x = e.temp(T::Float, nan())?;
    let y = e.temp(T::Float, nan())?;
    let active = e.boolean()?;
    let skip_slow = e.boolean()?;
    let first_string = e.boolean()?;
    let answer = e.boxed()?;
    let max = FIRST_CODE + 10;
    let min = FIRST_CODE + 11;
    let pow = FIRST_CODE + 12;
    let rand = FIRST_CODE + 13;
    let pi = FIRST_CODE + 18;
    let pf = pi + 1;
    let isnan = pi + 2;
    let finite = pi + 3;
    let mut body = vec![
        assert_stmt(accepted_numeric_code(code), l),
        assign(active, E::Bool(true), l),
        assign(skip_slow, E::Bool(false), l),
        assign(
            n,
            call(B::Len.call_index(), vec![E::Borrow(Box::new(var(args)))]),
            l,
        ),
        assign(
            first_string,
            or(eq(var(code), int(pi)), eq(var(code), int(pf))),
            l,
        ),
        assign(
            count,
            choose(
                or(eq(var(code), int(max)), eq(var(code), int(min))),
                var(n),
                choose(
                    eq(var(code), int(rand)),
                    int(0),
                    choose(descriptor_converts_two(code), int(2), int(1)),
                ),
            ),
            l,
        ),
        assign(
            x,
            choose(
                eq(var(code), int(max)),
                E::Float(f64::NEG_INFINITY),
                choose(eq(var(code), int(min)), E::Float(f64::INFINITY), nan()),
            ),
            l,
        ),
    ];
    // Borrowed `o.f("1")` passes a foreign object. `Number.parseInt(...)` passes
    // the Number carrier; bare and extracted calls pass undefined. Reject only
    // the foreign object through the existing guest TypeError and skip the slow
    // frame so that error remains the return value.
    let type_error = e.lower.intrinsics.expect("intrinsics").type_error;
    let reject = vec![
        assign(
            out,
            value::result_err(call(type_error, vec![var(c)]), sp),
            l,
        ),
        assign(active, E::Bool(false), l),
        assign(skip_slow, E::Bool(true), l),
    ];
    if e.lower.number_namespace_used.get() {
        let number_obj = e.boxed()?;
        let root_fn = e.lower.intrinsics.expect("intrinsics").root;
        let number_root = super::intrinsics::number_namespace_root(e.lower);
        body.push(branch(
            and(var(first_string), tag_is(var(receiver), value::OBJECT)),
            vec![
                assign(
                    number_obj,
                    call(root_fn, vec![var(c), int(number_root)]),
                    l,
                ),
                branch(
                    not(value::strict_equal_flag(receiver, number_obj, None)),
                    reject,
                    vec![],
                    l,
                ),
            ],
            vec![],
            l,
        ));
    } else {
        body.push(branch(
            and(var(first_string), tag_is(var(receiver), value::OBJECT)),
            reject,
            vec![],
            l,
        ));
    }
    let mut step = vec![
        assign(
            input,
            choose(
                lt(var(i), var(n)),
                call(
                    B::Get.call_index(),
                    vec![E::Borrow(Box::new(var(args))), var(i)],
                ),
                value::undefined(),
            ),
            l,
        ),
        assign(converted, result(schema.clone(), true, var(input), sp), l),
    ];
    step.push(branch(
        direct_primitive(var(input)),
        vec![],
        vec![assign(
            converted,
            call(
                start + 1,
                vec![
                    var(c),
                    var(input),
                    and(var(first_string), eq(var(i), int(0))),
                ],
            ),
            l,
        )],
        l,
    ));
    let v = to_number(&e, start, var(primitive));
    let mut success = vec![e.root(var(frame), int(0), var(primitive))?];
    let pair = vec![branch(
        eq(var(i), int(0)),
        vec![
            assign(first, var(primitive), l),
            e.root(var(frame), int(1), var(first))?,
            assign(x, v.clone(), l),
        ],
        vec![assign(y, v.clone(), l)],
        l,
    )];
    let mut extrema = vec![assign(y, v, l)];
    let same_zero = and(
        eq(num(var(x)), E::Float(0.0)),
        eq(num(var(y)), E::Float(0.0)),
    );
    let replace = choose(
        eq(var(code), int(max)),
        lt(num(var(x)), num(var(y))),
        lt(num(var(y)), num(var(x))),
    );
    // Addition gives +0 except -0 + -0; negated addition implements min zeros.
    let zero = choose(
        eq(var(code), int(max)),
        add(num(var(x)), num(var(y))),
        neg(add(neg(num(var(x))), neg(num(var(y))))),
    );
    extrema.push(assign(
        x,
        choose(
            or(
                not(eq(num(var(x)), num(var(x)))),
                not(eq(num(var(y)), num(var(y)))),
            ),
            nan(),
            choose(same_zero, zero, choose(replace, var(y), var(x))),
        ),
        l,
    ));
    success.push(branch(
        or(eq(var(code), int(max)), eq(var(code), int(min))),
        extrema,
        pair,
        l,
    ));
    let failure = vec![
        e.root(var(frame), int(2), var(err))?,
        assign(out, value::result_err(var(err), sp), l),
        assign(active, E::Bool(false), l),
    ];
    step.push(matched(
        &mut e, converted, schema, primitive, err, success, failure,
    )?);
    step.push(assign(i, add(var(i), int(1)), l));
    let mut unary_fast = [0, 1, 2, 3, 5, 6, 7, 8, 9, 15, 16, 17]
        .into_iter()
        .fold(E::Bool(false), |acc, offset| {
            or(acc, eq(var(code), int(FIRST_CODE + offset)))
        });
    for (_, arity, ext) in EXTENSIONS {
        if arity == 1 {
            unary_fast = or(unary_fast, eq(var(code), int(ext)));
        }
    }
    body.push(branch(
        unary_fast,
        vec![branch(
            eq(var(n), int(0)),
            vec![assign(x, nan(), l), assign(skip_slow, E::Bool(true), l)],
            vec![
                assign(
                    input,
                    call(
                        B::Get.call_index(),
                        vec![E::Borrow(Box::new(var(args))), int(0)],
                    ),
                    l,
                ),
                branch(
                    direct_primitive(var(input)),
                    vec![
                        assign(x, to_number(&e, start, var(input)), l),
                        assign(skip_slow, E::Bool(true), l),
                    ],
                    vec![],
                    l,
                ),
            ],
            l,
        )],
        vec![],
        l,
    ));
    body.push(branch(
        not(var(skip_slow)),
        vec![
            assign(frame, call(h.root_enter, vec![var(c), int(6)]), l),
            S::While {
                condition: and(var(active), lt(var(i), var(count))),
                body: step,
                line: l,
            },
        ],
        vec![],
        l,
    ));
    let mut finish = Vec::new();
    let unary = [
        (0, B::MathAbs),
        (1, B::MathAcos),
        (2, B::MathAsin),
        (3, B::MathAtan),
        (5, B::MathCeil),
        (6, B::MathCos),
        (7, B::MathExp),
        (8, B::MathFloor),
        (9, B::MathLn),
        (15, B::MathSin),
        (16, B::MathSqrt),
        (17, B::MathTan),
    ];
    for (offset, b) in unary {
        finish.push(branch(
            eq(var(code), int(FIRST_CODE + offset)),
            vec![assign(answer, boxed_number(builtin(b, vec![var(x)])), l)],
            vec![],
            l,
        ));
    }
    finish.push(branch(
        eq(var(code), int(FIRST_CODE + 4)),
        vec![assign(
            answer,
            boxed_number(builtin(B::MathAtan2, vec![var(x), var(y)])),
            l,
        )],
        vec![],
        l,
    ));
    finish.push(branch(
        or(eq(var(code), int(max)), eq(var(code), int(min))),
        vec![assign(answer, boxed_number(var(x)), l)],
        vec![],
        l,
    ));
    // ES5 pow: a NaN exponent is NaN even for base1; |base|=1 with infinite exponent is NaN.
    let p = number_power(x, y);
    finish.push(branch(
        eq(var(code), int(pow)),
        vec![assign(answer, boxed_number(p), l)],
        vec![],
        l,
    ));
    if let Some(random) = random {
        finish.push(branch(
            eq(var(code), int(rand)),
            vec![assign(answer, boxed_number(num(call(random, vec![]))), l)],
            vec![],
            l,
        ));
    }
    let floor = builtin(B::MathFloor, vec![var(x)]);
    let rounded = choose(
        or(
            not(builtin(B::MathIsFinite, vec![var(x)])),
            eq(var(x), E::Float(0.0)),
        ),
        var(x),
        choose(
            and(lt(var(x), E::Float(0.0)), not(lt(var(x), E::Float(-0.5)))),
            E::Float(-0.0),
            add(
                floor.clone(),
                choose(
                    lt(sub(var(x), floor), E::Float(0.5)),
                    E::Float(0.0),
                    E::Float(1.0),
                ),
            ),
        ),
    );
    finish.push(branch(
        eq(var(code), int(FIRST_CODE + 14)),
        vec![assign(answer, boxed_number(rounded), l)],
        vec![],
        l,
    ));
    // String parsing consumes UTF16 directly; no host evaluator or callbacks.
    let parsed = call(
        start,
        vec![
            var(c),
            var(first),
            choose(eq(var(code), int(pi)), int(1), int(2)),
            var(y),
        ],
    );
    let integer_parse = number::parse_numeric(&mut e, var(x), var(y))?;
    let token_parse = number::parse_token(&mut e, var(first), var(y))?;
    let primitive_parse = choose(eq(var(code), int(pi)), token_parse, nan());
    let numeric_parse = choose(
        eq(var(code), int(pf)),
        choose(eq(var(x), E::Float(0.0)), E::Float(0.0), var(x)),
        integer_parse,
    );
    finish.push(branch(
        or(eq(var(code), int(pi)), eq(var(code), int(pf))),
        vec![assign(
            answer,
            boxed_number(choose(
                tag_is(var(first), value::STRING),
                parsed,
                choose(
                    tag_is(var(first), value::NUMBER),
                    numeric_parse,
                    primitive_parse,
                ),
            )),
            l,
        )],
        vec![],
        l,
    ));
    finish.push(branch(
        eq(var(code), int(isnan)),
        vec![assign(
            answer,
            value::boolean(not(eq(num(var(x)), num(var(x))))),
            l,
        )],
        vec![],
        l,
    ));
    finish.push(branch(
        eq(var(code), int(finite)),
        vec![assign(
            answer,
            value::boolean(builtin(B::MathIsFinite, vec![var(x)])),
            l,
        )],
        vec![],
        l,
    ));
    let preserve = or(not(eq(num(var(x)), num(var(x)))), eq(var(x), E::Float(0.0)));
    finish.push(branch(
        eq(var(code), int(extension_code("trunc"))),
        vec![assign(
            answer,
            boxed_number(builtin(B::MathTrunc, vec![var(x)])),
            l,
        )],
        vec![],
        l,
    ));
    finish.push(branch(
        eq(var(code), int(extension_code("log2"))),
        vec![assign(
            answer,
            boxed_number(builtin(B::MathLog2, vec![var(x)])),
            l,
        )],
        vec![],
        l,
    ));
    finish.push(branch(
        eq(var(code), int(extension_code("log10"))),
        vec![assign(
            answer,
            boxed_number(builtin(B::MathLog10, vec![var(x)])),
            l,
        )],
        vec![],
        l,
    ));
    let imul_out = e.temp(T::Float, nan())?;
    let clz_out = e.temp(T::Float, nan())?;
    let fround_out = e.temp(T::Float, nan())?;
    let mut imul_steps = math_ext::imul(&mut e, l, x, y, imul_out)?;
    imul_steps.push(assign(answer, boxed_number(var(imul_out)), l));
    finish.push(branch(
        eq(var(code), int(extension_code("imul"))),
        imul_steps,
        vec![],
        l,
    ));
    let mut clz_steps = math_ext::clz32(&mut e, l, x, clz_out)?;
    clz_steps.push(assign(answer, boxed_number(var(clz_out)), l));
    finish.push(branch(
        eq(var(code), int(extension_code("clz32"))),
        clz_steps,
        vec![],
        l,
    ));
    let mut fround_steps = math_ext::fround(&mut e, l, x, fround_out)?;
    fround_steps.push(assign(answer, boxed_number(var(fround_out)), l));
    finish.push(branch(
        eq(var(code), int(extension_code("fround"))),
        fround_steps,
        vec![],
        l,
    ));
    finish.push(branch(
        eq(var(code), int(extension_code("sign"))),
        vec![
            assign(answer, boxed_number(var(x)), l),
            branch(
                not(preserve),
                vec![assign(
                    answer,
                    boxed_number(builtin(B::MathSignum, vec![var(x)])),
                    l,
                )],
                vec![],
                l,
            ),
        ],
        vec![],
        l,
    ));
    finish.push(assign(
        out,
        value::result_ok(value::completion_return(var(answer)), sp),
        l,
    ));
    body.push(branch(var(active), finish, vec![], l));
    let leave = e.effect(h.root_leave, vec![var(c), var(frame)])?;
    body.push(branch(not(var(skip_slow)), vec![leave], vec![], l));
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        "__compat_es5_numeric_call",
        index,
        vec![c, code, args, receiver],
        vec![
            carrier_schema.clone(),
            T::Int,
            T::Array(Box::new(value::schema())),
            value::schema(),
        ],
        value::logical_result_schema(),
        body,
        var(out),
        l,
    );
    Ok(NumericFns {
        parser: start,
        call: index,
    })
}

/// Extend only the just-emitted checked dispatcher; preserve source/Object codes.
pub(crate) fn extend_dispatch(ir: &mut FrontendIr, dispatch: u16, numeric: u16) {
    let f = ir
        .function_impls
        .get_mut(&dispatch)
        .expect("fresh dispatcher");
    let [carrier, _, receiver, args] = f.param_slots.as_slice() else {
        unreachable!("dispatch ABI")
    };
    let E::Var(out) = f.body_expr else {
        unreachable!("dispatch result")
    };
    let Some(S::IfElse {
        then_branch: ordinary,
        ..
    }) = f.body_stmts.last_mut()
    else {
        unreachable!("dispatch tag")
    };
    let Some(S::IfElse {
        then_branch: callable,
        ..
    }) = ordinary.last_mut()
    else {
        unreachable!("dispatch kind")
    };
    let S::Assign { index: code, .. } = callable[0] else {
        unreachable!("dispatch code")
    };
    let original = callable.pop().expect("dispatch tail");
    let line = f.body_expr_line;
    callable.push(branch(
        accepted_numeric_code(code),
        vec![assign(
            out,
            call(numeric, vec![var(*carrier), var(code), var(*args), var(*receiver)]),
            line,
        )],
        vec![original],
        line,
    ));
}
