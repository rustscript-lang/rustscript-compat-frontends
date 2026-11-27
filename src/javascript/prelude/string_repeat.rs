//! `String.prototype.repeat`, dispatch code 309.
//!
//! Order matches Micro `js_string_repeat`: reject a non-string, saturate the
//! count, reject a negative count, then either return the same receiver or
//! publish a concatenation. There is no `JS_STRING_LEN_MAX` in this tree.
//! A product past the existing UTF-16 bridge ceiling is a host assert, not a
//! guest `RangeError` and not a new limit constant.

use super::super::{lowerer::Lowerer, lowerer::assert_stmt, representation as value};
use super::object::{
    add, assign, branch, call, choose, declare, eq, field, int, lt, mul, not, num, tag_is, var,
    Emit,
};
use swc_common::Span;
use vm::{BuiltinFunction as B, Expr as E, FrontendIr, ParseError, Stmt as S, TypeSchema as T};

const NAME: &str = "__compat_es5_string_repeat";

pub(crate) fn append(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: Span,
    stmts: &mut Vec<S>,
    index: u16,
) -> Result<(), ParseError> {
    let to_units = lower.utf16_units_helper().ok_or_else(|| {
        lower.error(span, "String.prototype.repeat requires the installed UTF-16 bridge")
    })?;
    let publish = lower.utf16_publish_helper().ok_or_else(|| {
        lower.error(span, "String.prototype.repeat requires the installed UTF-16 bridge")
    })?;
    let parser = lower.published_number_parser().ok_or_else(|| {
        lower.error(span, "String.prototype.repeat requires the installed number parser")
    })?;
    let intrinsic = lower.intrinsics.expect("intrinsics");
    let type_error = intrinsic.type_error;
    let range_error = intrinsic.range_error.filter(|_| lower.range_error_used.get());
    let heap = lower.heap_ref(span)?.helpers;
    let c = lower.slot(span)?;
    let receiver = lower.slot(span)?;
    let args = lower.slot(span)?;
    let mut e = Emit::new(lower, span, c)?;
    let l = e.line;
    let sp = e.lower.ir_span(span);
    let out = e.temp(
        value::logical_result_schema(),
        value::result_ok(value::completion_normal(), sp),
    )?;
    let frame = e.integer()?;
    let err = e.boxed()?;
    let raw = e.boxed()?;
    let number = e.temp(T::Number, E::Float(0.0))?;
    let sat = e.temp(T::Number, E::Float(0.0))?;
    let units = e.temp(array(), call(B::ArrayNew.call_index(), vec![]))?;
    let source_len = e.integer()?;
    let n = e.integer()?;
    let copy_i = e.integer()?;
    let unit_i = e.integer()?;
    let unit = e.integer()?;
    let out_units = e.temp(array(), call(B::ArrayNew.call_index(), vec![]))?;
    let published = e.boxed()?;

    let throw_type = vec![
        assign(err, call(type_error, vec![var(c)]), l),
        e.root(var(frame), int(0), var(err))?,
        assign(out, value::result_err(var(err), sp), l),
    ];
    let throw_range = match range_error {
        Some(range_error) => vec![
            assign(err, call(range_error, vec![var(c), int(0)]), l),
            e.root(var(frame), int(0), var(err))?,
            assign(out, value::result_err(var(err), sp), l),
        ],
        // `.repeat` as an identifier installs the allocator before this helper
        // is built. A computed key that never names it is outside this P.
        None => vec![assert_stmt(eq(int(0), int(1)), l)],
    };
    let same = vec![assign(
        out,
        value::result_ok(value::completion_return(var(receiver)), sp),
        l,
    )];
    let publish_units = |slot: u16| {
        vec![assign(
            published,
            call(publish, vec![var(c), var(slot), var(frame), int(1)]),
            l,
        )]
    };
    let mut publish_empty = vec![assign(
        out_units,
        call(B::ArrayNew.call_index(), vec![]),
        l,
    )];
    publish_empty.extend(publish_units(out_units));
    publish_empty.push(assign(
        out,
        value::result_ok(value::completion_return(var(published)), sp),
        l,
    ));
    let mut concat = vec![
        assert_stmt(not(lt(E::Float(super::utf16_bridge::CEILING as f64), var(sat))), l),
        assert_stmt(
            not(lt(
                E::Float(super::utf16_bridge::CEILING as f64),
                mul(var(sat), num(var(source_len))),
            )),
            l,
        ),
        assign(n, call(e.decode, vec![var(sat)]), l),
        assert_stmt(lt(int(0), var(source_len)), l),
        assert_stmt(not(lt(int(super::utf16_bridge::CEILING), mul(var(n), var(source_len)))), l),
        assign(out_units, call(B::ArrayNew.call_index(), vec![]), l),
        assign(copy_i, int(0), l),
        S::While {
            condition: lt(var(copy_i), var(n)),
            body: vec![
                assign(unit_i, int(0), l),
                S::While {
                    condition: lt(var(unit_i), var(source_len)),
                    body: vec![
                        assign(
                            unit,
                            call(
                                B::Get.call_index(),
                                vec![E::Borrow(Box::new(var(units))), var(unit_i)],
                            ),
                            l,
                        ),
                        assign(
                            out_units,
                            call(
                                B::ArrayPush.call_index(),
                                vec![E::MoveVar(out_units), var(unit)],
                            ),
                            l,
                        ),
                        assign(unit_i, add(var(unit_i), int(1)), l),
                    ],
                    line: l,
                },
                assign(copy_i, add(var(copy_i), int(1)), l),
            ],
            line: l,
        },
    ];
    concat.extend(publish_units(out_units));
    concat.push(assign(
        out,
        value::result_ok(value::completion_return(var(published)), sp),
        l,
    ));
    let after_negative = vec![
        assign(
            units,
            call(to_units, vec![var(c), var(receiver)]),
            l,
        ),
        assign(
            source_len,
            call(B::Len.call_index(), vec![E::Borrow(Box::new(var(units)))]),
            l,
        ),
        branch(
            eq(var(source_len), int(0)),
            same.clone(),
            vec![branch(
                eq(var(sat), E::Float(1.0)),
                same,
                vec![branch(
                    eq(var(sat), E::Float(0.0)),
                    publish_empty,
                    concat,
                    l,
                )],
                l,
            )],
            l,
        ),
    ];
    let trunc = call(B::MathTrunc.call_index(), vec![var(number)]);
    let in_range = choose(
        and_zero(trunc.clone()),
        E::Float(0.0),
        trunc.clone(),
    );
    let sat_expr = choose(
        not(call(B::MathIsFinite.call_index(), vec![var(number)])),
        choose(
            lt(E::Float(0.0), var(number)),
            E::Float(2_147_483_647.0),
            choose(
                lt(var(number), E::Float(0.0)),
                E::Float(-2_147_483_648.0),
                E::Float(0.0),
            ),
        ),
        choose(
            not(lt(trunc.clone(), E::Float(2_147_483_648.0))),
            E::Float(2_147_483_647.0),
            choose(
                lt(trunc, E::Float(-2_147_483_648.0)),
                E::Float(-2_147_483_648.0),
                in_range,
            ),
        ),
    );
    let count = vec![
        assign(
            raw,
            choose(
                lt(
                    call(B::Len.call_index(), vec![E::Borrow(Box::new(var(args)))]),
                    int(1),
                ),
                value::undefined(),
                call(
                    B::Get.call_index(),
                    vec![E::Borrow(Box::new(var(args))), int(0)],
                ),
            ),
            l,
        ),
        assign(number, to_number(var(raw), c, parser), l),
        assign(sat, sat_expr, l),
        branch(lt(var(sat), E::Float(0.0)), throw_range, after_negative, l),
    ];
    let body = vec![
        assign(frame, call(heap.root_enter, vec![var(c), int(2)]), l),
        e.root(var(frame), int(0), var(receiver))?,
        branch(tag_is(var(receiver), value::STRING), count, throw_type, l),
        e.effect(heap.root_leave, vec![var(c), var(frame)])?,
    ];
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        NAME,
        index,
        vec![c, receiver, args],
        vec![
            carrier_schema.clone(),
            value::schema(),
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

fn and_zero(trunc: E) -> E {
    super::object::and(
        not(lt(trunc.clone(), E::Float(0.0))),
        not(lt(E::Float(0.0), trunc)),
    )
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
