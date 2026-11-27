//! `Number.prototype.toString(radix)` body (dispatch code 300).
//!
//! ES5.1 15.7.4.2 with a bounded domain:
//!
//! * `radix` is already `ToInteger`-ed and range-checked by the dispatcher
//!   (`2..=36`); the default `10` arrives as an explicit Number.
//! * radix 10 delegates to the accepted ES5 9.8.1 shortest-digit producer
//!   `__compat_es5_number_string_managed`.
//! * `NaN`, `±0` and `±Infinity` never reach that producer (it asserts on a
//!   non-digit Core display); they are spelled directly as UTF-16 units, which
//!   is exactly what 9.8.1 steps 2–4 return.
//! * a non-decimal radix converts the integer part by exact repeated division.
//!   `q mod radix` is `fmod` (exact) and `(q - d) / radix` divides an exactly
//!   representable multiple, so every quotient stays exact while `|q| <= 2^53`.
//!   Values above that, and non-integers, are rejected with a `TypeError`
//!   instead of returning a silently wrong digit string.
//!
//! Units are published through the accepted UTF-16 bridge
//! (`units_to_managed`); this producer never allocates a String node itself.

use super::super::{lowerer::Lowerer, lowerer::assert_stmt, representation as value};
use super::object::{
    add, and, assign, branch, call, choose, declare, eq, int, lt, not, or, sub, var, Emit,
};
use swc_common::Span;
use vm::{BuiltinFunction as B, Expr as E, FrontendIr, ParseError, Stmt as S, TypeSchema as T};

const NAME: &str = "__compat_es5_number_proto_to_string";
/// Largest magnitude whose integer part converts exactly in `f64` arithmetic.
const EXACT_LIMIT: f64 = 9_007_199_254_740_992.0;

pub(crate) fn append(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: Span,
    stmts: &mut Vec<S>,
    index: u16,
) -> Result<(), ParseError> {
    let publish = lower.utf16_publish_helper().ok_or_else(|| {
        lower.error(
            span,
            "Number.prototype.toString requires the installed UTF-16 bridge",
        )
    })?;
    let number_string = lower.number_string_managed();
    let type_error = lower.intrinsics.expect("intrinsics").type_error;
    let heap = lower.heap_ref(span)?.helpers;
    let c = lower.slot(span)?;
    let input = lower.slot(span)?;
    let radix = lower.slot(span)?;
    let mut e = Emit::new(lower, span, c)?;
    let l = e.line;
    let sp = e.lower.ir_span(span);
    let decode = e.decode;
    let out = e.temp(
        value::logical_result_schema(),
        value::result_ok(value::completion_normal(), sp),
    )?;
    let frame = e.integer()?;
    let active = e.boolean()?;
    let negative = e.boolean()?;
    let zero = e.boolean()?;
    let nan = e.boolean()?;
    let infinite = e.boolean()?;
    let exact = e.boolean()?;
    let number = e.temp(T::Number, E::Float(0.0))?;
    let magnitude = e.temp(T::Number, E::Float(0.0))?;
    let quotient = e.temp(T::Number, E::Float(0.0))?;
    let digit = e.temp(T::Number, E::Float(0.0))?;
    let units = e.temp(
        T::Array(Box::new(T::Int)),
        call(B::ArrayNew.call_index(), vec![]),
    )?;
    let digits = e.temp(
        T::Array(Box::new(T::Int)),
        call(B::ArrayNew.call_index(), vec![]),
    )?;
    let count = e.integer()?;
    let cursor = e.integer()?;
    let unit = e.integer()?;
    let published = e.boxed()?;
    let err = e.boxed()?;

    // Append one already-decoded UTF-16 unit.
    let push_unit = |units: u16, unit: u16| {
        assign(
            units,
            call(
                B::ArrayPush.call_index(),
                vec![E::MoveVar(units), var(unit)],
            ),
            l,
        )
    };
    // Push a literal ASCII spelling.
    let spelling = |text: &[i64]| {
        let mut stmts = Vec::new();
        for code in text {
            stmts.push(assign(unit, int(*code), l));
            stmts.push(push_unit(units, unit));
        }
        stmts
    };

    // 9.8.1 steps 2–4: "NaN", "0", "Infinity", "-Infinity".
    let mut special = Vec::new();
    let mut infinity = spelling(&[73, 110, 102, 105, 110, 105, 116, 121]);
    let mut negative_infinity = spelling(&[45]);
    negative_infinity.extend(infinity.clone());
    infinity = vec![branch(var(negative), negative_infinity, infinity, l)];
    special.push(branch(
        var(nan),
        spelling(&[78, 97, 78]),
        vec![branch(
            var(zero),
            spelling(&[48]),
            infinity,
            l,
        )],
        l,
    ));

    // Integer part by exact repeated division, least significant digit first.
    let mut convert = vec![
        assign(quotient, var(magnitude), l),
        assign(count, int(0), l),
    ];
    convert.push(S::While {
        condition: not(lt(var(quotient), E::Float(1.0))),
        body: vec![
            assign(
                digit,
                E::Mod(Box::new(var(quotient)), Box::new(var(radix))),
                l,
            ),
            assign(
                quotient,
                E::Div(
                    Box::new(sub(var(quotient), var(digit))),
                    Box::new(var(radix)),
                ),
                l,
            ),
            assign(
                digits,
                call(
                    B::ArrayPush.call_index(),
                    vec![E::MoveVar(digits), call(decode, vec![var(digit)])],
                ),
                l,
            ),
            assign(count, add(var(count), int(1)), l),
            assert_stmt(lt(var(count), int(64)), l),
        ],
        line: l,
    });
    convert.push(assign(
        cursor,
        sub(
            call(B::Len.call_index(), vec![E::Borrow(Box::new(var(digits)))]),
            int(1),
        ),
        l,
    ));
    convert.push(branch(
        var(negative),
        spelling(&[45]),
        Vec::new(),
        l,
    ));
    convert.push(S::While {
        condition: not(lt(var(cursor), int(0))),
        body: {
            let mut body = vec![assign(
                digit,
                call(
                    B::Get.call_index(),
                    vec![E::Borrow(Box::new(var(digits))), var(cursor)],
                ),
                l,
            )];
            body.push(assign(
                unit,
                call(
                    decode,
                    vec![choose(
                        lt(var(digit), E::Float(10.0)),
                        add(var(digit), E::Float(48.0)),
                        add(var(digit), E::Float(87.0)),
                    )],
                ),
                l,
            ));
            body.push(push_unit(units, unit));
            body.push(assign(cursor, sub(var(cursor), int(1)), l));
            body
        },
        line: l,
    });

    let reject = vec![
        assign(err, call(type_error, vec![var(c)]), l),
        e.root(var(frame), int(0), var(err))?,
        assign(out, value::result_err(var(err), sp), l),
        assign(active, E::Bool(false), l),
    ];
    let decimal = vec![
        assign(
            published,
            call(
                number_string,
                vec![var(c), var(number), var(frame), int(1)],
            ),
            l,
        ),
        assign(
            out,
            value::result_ok(value::completion_return(var(published)), sp),
            l,
        ),
        // The shared publish step below only serves the two units-building
        // paths; this arm already published through the accepted producer.
        assign(active, E::Bool(false), l),
    ];
    // The radix conversion requires an integer magnitude that still converts
    // exactly. Anything else is refused, never rounded into a wrong string.
    let numeric = vec![
        assign(
            exact,
            and(
                eq(
                    var(magnitude),
                    call(B::MathFloor.call_index(), vec![var(magnitude)]),
                ),
                not(lt(E::Float(EXACT_LIMIT), var(magnitude))),
            ),
            l,
        ),
        branch(var(exact), convert, reject, l),
    ];
    let body = vec![
        assign(frame, call(heap.root_enter, vec![var(c), int(2)]), l),
        assign(units, call(B::ArrayNew.call_index(), vec![]), l),
        assign(digits, call(B::ArrayNew.call_index(), vec![]), l),
        assign(number, var(input), l),
        assign(negative, lt(var(number), E::Float(0.0)), l),
        assign(
            magnitude,
            choose(var(negative), E::Neg(Box::new(var(number))), var(number)),
            l,
        ),
        assign(nan, not(eq(var(number), var(number))), l),
        assign(zero, eq(var(magnitude), E::Float(0.0)), l),
        assign(infinite, eq(var(magnitude), E::Float(f64::INFINITY)), l),
        assign(active, E::Bool(true), l),
        branch(
            or(or(var(nan), var(zero)), var(infinite)),
            special,
            vec![branch(
                eq(var(radix), E::Float(10.0)),
                decimal,
                numeric,
                l,
            )],
            l,
        ),
        branch(
            var(active),
            vec![
                assign(
                    published,
                    call(publish, vec![var(c), var(units), var(frame), int(1)]),
                    l,
                ),
                assign(
                    out,
                    value::result_ok(value::completion_return(var(published)), sp),
                    l,
                ),
            ],
            Vec::new(),
            l,
        ),
        e.effect(heap.root_leave, vec![var(c), var(frame)])?,
    ];
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        NAME,
        index,
        vec![c, input, radix],
        vec![carrier_schema.clone(), T::Number, T::Number],
        value::logical_result_schema(),
        body,
        var(out),
        l,
    );
    Ok(())
}
