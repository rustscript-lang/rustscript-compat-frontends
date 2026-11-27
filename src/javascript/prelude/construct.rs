//! The one private `new F(...)` construction helper.
//!
//! `new` on a source function is lowered as: evaluate the callee, evaluate
//! every actual left to right into one Array, then `Get(F, "prototype")`,
//! allocate the instance with that prototype (an ordinary object falls back
//! to `Object.prototype` when the value is not an object), call F with the
//! instance as receiver, and keep the returned object or the instance.
//!
//! This helper performs the allocate/call/select half. The lowering owns the
//! evaluation order, because the prototype Get must happen strictly after the
//! actuals. The callee must be a *source* function node (`KIND_FUNCTION` with
//! code `0..=255`). A statically recognized builtin-constructor alias is
//! routed away from this helper by the lowering; any remaining intrinsic,
//! bound or non-function callee keeps the guest `TypeError: not a function`.

use super::super::{
    heap as layout,
    lowerer::Lowerer,
    representation as value,
};
use super::object::{
    Emit, add, assign, branch, call, choose, declare, eq, field, int, lt, matched, result,
    tag_is, var,
};
use vm::{FrontendIr, ParseError, Stmt as S, TypeSchema as T};

/// Append the private construct helper and return its flat function index.
pub(crate) fn append(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
) -> Result<u16, ParseError> {
    let index = super::heap::fresh_index(lower, ir, span)?;
    let carrier = lower.slot(span)?;
    let func = lower.slot(span)?;
    let args = lower.slot(span)?;
    let proto = lower.slot(span)?;
    let allocate = lower.intrinsics.expect("intrinsics").allocate;
    let type_error = lower.intrinsics.expect("intrinsics").type_error;
    let root = lower.intrinsics.expect("intrinsics").root;
    let dispatch = lower
        .helper_dispatch
        .ok_or_else(|| lower.error(span, "new requires the realm dispatch helper"))?;
    let helper = lower.heap_ref(span)?.helpers;
    let mut e = Emit::new(lower, span, carrier)?;
    let l = e.line;
    let sp = e.lower.ir_span(span);
    let vschema = value::value_result_schema();
    let out = e.temp(
        vschema.clone(),
        result(vschema.clone(), true, value::undefined(), sp),
    )?;
    let frame = e.integer()?;
    let base = e.integer()?;
    let kind = e.integer()?;
    let err = e.boxed()?;
    let instance = e.boxed()?;
    let inst_proto = e.boxed()?;
    let raw = e.temp(
        value::logical_result_schema(),
        value::result_ok(value::completion_normal(), sp),
    )?;
    let completion = e.temp(value::completion_schema(), value::completion_normal())?;
    let error = e.boxed()?;
    let returned = e.boxed()?;
    let answer = e.boxed()?;
    let mut body = vec![
        assign(frame, call(helper.root_enter, vec![var(carrier), int(8)]), l),
        e.root(var(frame), int(0), var(func))?,
        e.root(var(frame), int(2), var(proto))?,
    ];
    let fail = {
        let mut fail = vec![assign(err, call(type_error, vec![var(carrier)]), l)];
        fail.push(e.root(var(frame), int(5), var(err))?);
        fail.push(assign(out, result(vschema.clone(), false, var(err), sp), l));
        fail
    };
    let mut ordinary = vec![
        // OrdinaryCreateFromConstructor: a non-object `prototype` value falls
        // back to the intrinsic Object.prototype, exactly the documented
        // `L.prototype = 7` case. The fallback is read from the permanent
        // root frame, never from a guest-visible row.
        assign(inst_proto, value::null(), l),
        branch(
            tag_is(var(proto), value::OBJECT),
            vec![assign(inst_proto, var(proto), l)],
            vec![assign(inst_proto, call(root, vec![var(carrier), int(0)]), l)],
            l,
        ),
        // Publishing the instance may collect; the callee box is refreshed
        // from its rooted slot before the call, never from a stale base.
        assign(
            instance,
            call(
                allocate,
                vec![
                    var(carrier),
                    int(layout::KIND_OBJECT),
                    var(inst_proto),
                    int(0),
                    int(0),
                    int(0),
                    int(0),
                    var(frame),
                    int(3),
                ],
            ),
            l,
        ),
        assign(
            raw,
            call(
                dispatch,
                vec![var(carrier), var(func), var(instance), var(args)],
            ),
            l,
        ),
    ];
    let ok_body = vec![
        assign(
            returned,
            choose(
                eq(field(var(completion), "kind"), int(value::COMPLETION_RETURN)),
                field(var(completion), "value"),
                value::undefined(),
            ),
            l,
        ),
        assign(
            answer,
            choose(
                tag_is(var(returned), value::OBJECT),
                var(returned),
                var(instance),
            ),
            l,
        ),
        e.root(var(frame), int(4), var(answer))?,
        assign(out, result(vschema.clone(), true, var(answer), sp), l),
    ];
    let err_body = vec![assign(
        out,
        result(vschema.clone(), false, var(error), sp),
        l,
    )];
    ordinary.push(matched(
        &mut e,
        raw,
        value::logical_result_schema(),
        completion,
        error,
        ok_body,
        err_body,
    )?);
    // Validate the callee shape before touching the heap: `checked_base`
    // dereferences the box, so a non-object callee must take the guest
    // TypeError arm instead of a host assertion. Kind and code are decoded
    // only after the object tag holds.
    body.push(branch(
        tag_is(var(func), value::OBJECT),
        vec![
            assign(base, e.base(var(func)), l),
            assign(kind, e.decoded(var(base)), l),
            branch(
                eq(var(kind), int(layout::KIND_FUNCTION)),
                vec![branch(
                    lt(e.decoded(add(var(base), int(5))), int(256)),
                    ordinary,
                    fail.clone(),
                    l,
                )],
                fail.clone(),
                l,
            ),
        ],
        fail,
        l,
    ));
    body.push(e.effect(helper.root_leave, vec![var(carrier), var(frame)])?);
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        "__compat_es5_construct",
        index,
        vec![carrier, func, args, proto],
        vec![
            carrier_schema.clone(),
            value::schema(),
            T::Array(Box::new(value::schema())),
            value::schema(),
        ],
        vschema,
        body,
        var(out),
        l,
    );
    Ok(index)
}

/// The pre-`Get` half of the same slice: `new` must reject a non-source
/// function *before* it reads `F.prototype`.
///
/// The specification order is callee, every actual, then `[[Construct]]`,
/// which rejects a non-constructor before any `Get`. A plain object callee (or
/// a formal that holds one) can carry a `prototype` getter, so reading the
/// prototype first would surface that getter's throw instead of the pinned
/// `TypeError` — and could allocate for a construction that never started.
/// This helper is one guest Result: `Ok(())` for a `KIND_FUNCTION` node with
/// code `0..=255`, and `Err(TypeError)` for everything else. The lowering
/// consumes it right after the actuals, so its error preempts the `Get`.
pub(crate) fn append_check(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
) -> Result<u16, ParseError> {
    let index = super::heap::fresh_index(lower, ir, span)?;
    let carrier = lower.slot(span)?;
    let func = lower.slot(span)?;
    let type_error = lower.intrinsics.expect("intrinsics").type_error;
    let helper = lower.heap_ref(span)?.helpers;
    let mut e = Emit::new(lower, span, carrier)?;
    let l = e.line;
    let sp = e.lower.ir_span(span);
    let vschema = value::value_result_schema();
    let out = e.temp(
        vschema.clone(),
        result(vschema.clone(), true, value::undefined(), sp),
    )?;
    let frame = e.integer()?;
    let base = e.integer()?;
    let err = e.temp(value::schema(), value::undefined())?;
    let fail = vec![
        assign(err, call(type_error, vec![var(carrier)]), l),
        e.root(var(frame), int(0), var(err))?,
        assign(out, result(vschema.clone(), false, var(err), sp), l),
    ];
    let body = vec![
        assign(frame, call(helper.root_enter, vec![var(carrier), int(2)]), l),
        branch(
            tag_is(var(func), value::OBJECT),
            vec![
                assign(base, e.base(var(func)), l),
                branch(
                    eq(e.decoded(var(base)), int(layout::KIND_FUNCTION)),
                    vec![branch(
                        lt(e.decoded(add(var(base), int(5))), int(256)),
                        vec![assign(
                            out,
                            result(vschema.clone(), true, value::undefined(), sp),
                            l,
                        )],
                        fail.clone(),
                        l,
                    )],
                    fail.clone(),
                    l,
                ),
            ],
            fail,
            l,
        ),
        e.effect(helper.root_leave, vec![var(carrier), var(frame)])?,
    ];
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        "__compat_es5_construct_check",
        index,
        vec![carrier, func],
        vec![carrier_schema.clone(), value::schema()],
        vschema,
        body,
        var(out),
        l,
    );
    Ok(index)
}
