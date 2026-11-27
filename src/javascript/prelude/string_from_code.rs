//! `String.fromCharCode` (310) and `String.fromCodePoint` (311).
//!
//! Order matches Micro `js_string_fromCharCode` / `js_string_fromCodePoint`:
//! each argument is `JS_ToInt32` (not ToUint16). `fromCharCode` then keeps the
//! low 16 bits. `fromCodePoint` accepts `0..=0x10FFFF`, including a lone
//! surrogate, and encodes `0x10000..=0x10FFFF` as a real surrogate pair.
//! The carrier itself is rejected by the existing Number-carrier call path.

use super::super::{lowerer::Lowerer, lowerer::assert_stmt, representation as value};
use super::object::{
    add, and, assign, branch, call, choose, declare, eq, field, int, lt, not, or, sub, tag_is, var,
    Emit,
};
use swc_common::Span;
use vm::{BuiltinFunction as B, Expr as E, FrontendIr, ParseError, Stmt as S, TypeSchema as T};

const NAME: &str = "__compat_es5_string_from_code";
const MODULUS: f64 = 4_294_967_296.0;
const SIGN_BIT: f64 = 2_147_483_648.0;

pub(crate) fn append(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: Span,
    stmts: &mut Vec<S>,
    index: u16,
) -> Result<(), ParseError> {
    let publish = lower.utf16_publish_helper().ok_or_else(|| {
        lower.error(span, "String.fromCharCode requires the installed UTF-16 bridge")
    })?;
    let parser = lower.published_number_parser().ok_or_else(|| {
        lower.error(span, "String.fromCharCode requires the installed number parser")
    })?;
    let range_error = lower.intrinsics.expect("intrinsics").range_error.filter(|_| lower.range_error_used.get());
    let heap = lower.heap_ref(span)?.helpers;
    let c = lower.slot(span)?;
    let code = lower.slot(span)?;
    let args = lower.slot(span)?;
    let mut e = Emit::new(lower, span, c)?;
    let l = e.line;
    let sp = e.lower.ir_span(span);
    let decode = e.decode;
    let out = e.temp(
        value::logical_result_schema(),
        value::result_ok(value::completion_normal(), sp),
    )?;
    let frame = e.integer()?;
    let units = e.temp(array(), call(B::ArrayNew.call_index(), vec![]))?;
    let published = e.boxed()?;
    let err = e.boxed()?;
    let i = e.integer()?;
    let argc = e.integer()?;
    let raw = e.boxed()?;
    let number = e.temp(T::Number, E::Float(0.0))?;
    let uint32 = e.temp(T::Number, E::Float(0.0))?;
    let signed = e.temp(T::Number, E::Float(0.0))?;
    let trunc = e.temp(T::Number, E::Float(0.0))?;
    let rem = e.temp(T::Number, E::Float(0.0))?;
    let masked = e.temp(T::Number, E::Float(0.0))?;
    let unit = e.integer()?;
    let pair_u = e.temp(T::Number, E::Float(0.0))?;
    let high = e.temp(T::Number, E::Float(0.0))?;
    let low = e.temp(T::Number, E::Float(0.0))?;
    let active = e.boolean()?;
    let throw_range = match range_error {
        Some(range_error) => vec![
            assign(err, call(range_error, vec![var(c), int(0)]), l),
            e.root(var(frame), int(0), var(err))?,
            assign(out, value::result_err(var(err), sp), l),
            assign(active, E::Bool(false), l),
        ],
        // A resolved `String.fromCodePoint` installs the allocator before this
        // helper is built, including a function body and a string-literal key.
        // A fromCharCode-only source never enters this arm.
        None => vec![assert_stmt(eq(int(0), int(1)), l)],
    };
    let push = |slot: u16| {
        vec![
            assign(unit, call(decode, vec![var(slot)]), l),
            assign(
                units,
                call(B::ArrayPush.call_index(), vec![E::MoveVar(units), var(unit)]),
                l,
            ),
        ]
    };
    let mut step = vec![
        assign(
            raw,
            call(
                B::Get.call_index(),
                vec![E::Borrow(Box::new(var(args))), var(i)],
            ),
            l,
        ),
        assign(number, to_number(var(raw), c, parser), l),
        assign(uint32, E::Float(0.0), l),
        branch(
            and(
                call(B::MathIsFinite.call_index(), vec![var(number)]),
                not(eq(var(number), E::Float(0.0))),
            ),
            vec![
                assign(trunc, call(B::MathTrunc.call_index(), vec![var(number)]), l),
                assign(
                    rem,
                    E::Mod(Box::new(var(trunc)), Box::new(E::Float(MODULUS))),
                    l,
                ),
                assign(
                    rem,
                    choose(
                        lt(var(rem), E::Float(0.0)),
                        add(var(rem), E::Float(MODULUS)),
                        var(rem),
                    ),
                    l,
                ),
                assign(
                    uint32,
                    choose(eq(var(rem), E::Float(0.0)), E::Float(0.0), var(rem)),
                    l,
                ),
            ],
            vec![],
            l,
        ),
        assign(
            signed,
            choose(
                lt(var(uint32), E::Float(SIGN_BIT)),
                var(uint32),
                sub(var(uint32), E::Float(MODULUS)),
            ),
            l,
        ),
    ];
    let mut surrogate = vec![
        assign(pair_u, sub(var(signed), E::Float(65_536.0)), l),
        assign(
            high,
            add(
                E::Float(55_296.0),
                call(
                    B::MathFloor.call_index(),
                    vec![E::Div(Box::new(var(pair_u)), Box::new(E::Float(1_024.0)))],
                ),
            ),
            l,
        ),
        assign(
            low,
            add(
                E::Float(56_320.0),
                E::Mod(Box::new(var(pair_u)), Box::new(E::Float(1_024.0))),
            ),
            l,
        ),
    ];
    surrogate.extend(push(high));
    surrogate.extend(push(low));
    let mut one_unit = push(signed);
    let code_point = vec![branch(
        or(
            lt(var(signed), E::Float(0.0)),
            lt(E::Float(1_114_111.0), var(signed)),
        ),
        throw_range,
        vec![
            branch(
                lt(E::Float(65_535.0), var(signed)),
                surrogate,
                one_unit.split_off(0),
                l,
            ),
            assign(i, add(var(i), int(1)), l),
        ],
        l,
    )];
    let mut mask_arm = vec![assign(
        masked,
        E::Mod(Box::new(var(uint32)), Box::new(E::Float(65_536.0))),
        l,
    )];
    mask_arm.extend(push(masked));
    mask_arm.push(assign(i, add(var(i), int(1)), l));
    step.push(branch(
        eq(var(code), int(super::intrinsics::STRING_FROM_CODE_POINT_CODE)),
        code_point,
        mask_arm,
        l,
    ));
    let mut publish_ok = vec![assign(
        published,
        call(publish, vec![var(c), var(units), var(frame), int(1)]),
        l,
    )];
    publish_ok.push(assign(
        out,
        value::result_ok(value::completion_return(var(published)), sp),
        l,
    ));
    let body = vec![
        assign(frame, call(heap.root_enter, vec![var(c), int(2)]), l),
        assign(units, call(B::ArrayNew.call_index(), vec![]), l),
        assign(i, int(0), l),
        assign(active, E::Bool(true), l),
        assign(
            argc,
            call(B::Len.call_index(), vec![E::Borrow(Box::new(var(args)))]),
            l,
        ),
        S::While {
            condition: and(var(active), lt(var(i), var(argc))),
            body: step,
            line: l,
        },
        branch(var(active), publish_ok, vec![], l),
        e.effect(heap.root_leave, vec![var(c), var(frame)])?,
    ];
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        NAME,
        index,
        vec![c, code, args],
        vec![
            carrier_schema.clone(),
            T::Int,
            T::Array(Box::new(value::schema())),
        ],
        value::logical_result_schema(),
        body,
        var(out),
        l,
    );
    Ok(())
}

fn array() -> T {
    T::Array(Box::new(T::Int))
}

fn to_number(raw: E, carrier: u16, parser: u16) -> E {
    choose(
        tag_is(raw.clone(), value::NUMBER),
        field(raw.clone(), "number"),
        choose(
            tag_is(raw.clone(), value::UNDEFINED),
            E::Float(f64::NAN),
            choose(
                tag_is(raw.clone(), value::NULL),
                E::Float(0.0),
                choose(
                    tag_is(raw.clone(), value::BOOLEAN),
                    choose(field(raw.clone(), "flag"), E::Float(1.0), E::Float(0.0)),
                    choose(
                        tag_is(raw.clone(), value::STRING),
                        call(parser, vec![var(carrier), raw, int(0), E::Float(0.0)]),
                        E::Float(f64::NAN),
                    ),
                ),
            ),
        ),
    )
}
