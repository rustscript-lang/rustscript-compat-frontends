//! `String(value)` call semantics (dispatch code 312).
//!
//! ES5.1 15.5.1 as a bounded domain: no argument is the empty String, an
//! already-String argument is returned unchanged, a Number goes through the
//! accepted ES5 9.8.1 producer, and Boolean/Null/Undefined go through the
//! accepted fixed-scalar producer. An Object argument would need the guest
//! ToPrimitive walk; it is refused with a `TypeError` instead of guessing.
//!
//! `new String(value)` never reaches this helper: the frontend rejects the
//! construction form, exactly like `new Number(value)`.

use super::super::{lowerer::Lowerer, representation as value};
use super::object::{
    assign, branch, call, declare, eq, field, int, num, or, tag_is, var, Emit,
};
use swc_common::Span;
use vm::{BuiltinFunction as B, Expr as E, FrontendIr, ParseError, Stmt as S, TypeSchema as T};

const NAME: &str = "__compat_es5_string_call";

pub(crate) fn append(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: Span,
    stmts: &mut Vec<S>,
    index: u16,
) -> Result<(), ParseError> {
    let publish = lower.utf16_publish_helper().ok_or_else(|| {
        lower.error(span, "String call requires the installed UTF-16 bridge")
    })?;
    let number_string = lower.number_string_managed();
    let fixed_string = lower.fixed_scalar_string();
    let type_error = lower.intrinsics.expect("intrinsics").type_error;
    let heap = lower.heap_ref(span)?.helpers;
    let c = lower.slot(span)?;
    let args = lower.slot(span)?;
    let mut e = Emit::new(lower, span, c)?;
    let l = e.line;
    let sp = e.lower.ir_span(span);
    let out = e.temp(
        value::logical_result_schema(),
        value::result_ok(value::completion_normal(), sp),
    )?;
    let frame = e.integer()?;
    let argc = e.integer()?;
    let raw = e.boxed()?;
    let err = e.boxed()?;
    let units = e.temp(
        T::Array(Box::new(T::Int)),
        call(B::ArrayNew.call_index(), vec![]),
    )?;
    let return_ok = |value: E| {
        assign(
            out,
            value::result_ok(value::completion_return(value), sp),
            l,
        )
    };
    let empty = vec![
        assign(units, call(B::ArrayNew.call_index(), vec![]), l),
        return_ok(call(publish, vec![var(c), var(units), var(frame), int(1)])),
    ];
    let failure = vec![
        assign(err, call(type_error, vec![var(c)]), l),
        e.root(var(frame), int(0), var(err))?,
        assign(out, value::result_err(var(err), sp), l),
    ];
    let fixed = vec![return_ok(call(
        fixed_string,
        vec![var(c), var(raw), var(frame), int(1)],
    ))];
    let number = vec![return_ok(call(
        number_string,
        vec![
            var(c),
            num(field(var(raw), "number")),
            var(frame),
            int(1),
        ],
    ))];
    let text = vec![return_ok(var(raw))];
    let object = vec![branch(
        tag_is(var(raw), value::NUMBER),
        number,
        vec![branch(
            or(
                or(
                    tag_is(var(raw), value::BOOLEAN),
                    tag_is(var(raw), value::NULL),
                ),
                tag_is(var(raw), value::UNDEFINED),
            ),
            fixed,
            failure,
            l,
        )],
        l,
    )];
    let body = vec![
        assign(frame, call(heap.root_enter, vec![var(c), int(2)]), l),
        assign(
            argc,
            call(B::Len.call_index(), vec![E::Borrow(Box::new(var(args)))]),
            l,
        ),
        branch(
            eq(var(argc), int(0)),
            empty,
            vec![
                assign(
                    raw,
                    call(
                        B::Get.call_index(),
                        vec![E::Borrow(Box::new(var(args))), int(0)],
                    ),
                    l,
                ),
                branch(tag_is(var(raw), value::STRING), text, object, l),
            ],
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
        vec![c, args],
        vec![
            carrier_schema.clone(),
            T::Array(Box::new(value::schema())),
        ],
        value::logical_result_schema(),
        body,
        var(out),
        l,
    );
    Ok(())
}
