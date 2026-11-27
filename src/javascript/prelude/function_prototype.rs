//! The one gated `Function.prototype.prototype` get/set accessor.
//!
//! Upstream installs no per-function `prototype` row on *source* functions.
//! Every source function inherits one accessor from `Function.prototype`; the
//! getter lazily materializes `{constructor: F, [[Prototype]]:
//! Object.prototype}` and installs it as F's own writable/enumerable/
//! configurable data row. Deleting that row therefore makes the next read
//! materialize a genuinely new object, and a materialized row is an ordinary
//! own property in every later `hasOwnProperty`/`Object.keys`/`in`
//! observation.
//!
//! The getter and setter are two private two-row function nodes behind codes
//! 357/358. Dispatch routes both into this one adapter. The getter answers by
//! receiver code, matching the pinned engine:
//!
//! * code `0..=255` — a source function: lazily materialize
//!   `{constructor: F, [[Prototype]]: Object.prototype}` as F's own row;
//! * code 256 — `Function.prototype` itself: the guest
//!   `TypeError: not a function`;
//! * every other function node — intrinsic methods (`Object.keys`,
//!   `Math.max`), bound products (`fn.bind`) and the builtin constructor
//!   carriers: `undefined`, no throw. The five in-scope builtin carriers
//!   (`Object`, `Array`, `Number`, `String`, `Boolean`) own a real bootstrap
//!   `prototype` data row, so the ordinary Get finds that row first; after a
//!   `delete` the inherited accessor must not resurrect the root, hence this
//!   arm never reads a builtin prototype root.
//!
//! The setter keeps the source-function store, the 256 refusal, and the
//! pinned four-way split for the remaining function nodes:
//!
//! * a bound product (code 270) stores an ordinary own row;
//! * a builtin constructor carrier (`Object`, `Array`, `Number`, `String`,
//!   `Boolean`) stores its writable bootstrap `prototype` row (normally the
//!   ordinary own row answers before this accessor, but the setter must also
//!   accept the deleted-then-written shape);
//! * `Function.prototype` itself (code 256) and every ordinary intrinsic
//!   method node reject the write with the same guest TypeError.

use super::super::{
    heap as layout,
    lowerer::{Lowerer, assert_stmt},
    representation as value,
};
use super::object::{
    Emit, add, assign, branch, call, choose, data_descriptor, declare, eq, int, lt, matched,
    not, or, result, result_schema, tag_is, var,
};
use vm::{BuiltinFunction as B, Expr as E, FrontendIr, ParseError, Stmt as S, TypeSchema as T};

/// Append the shared get/set adapter and return its flat function index.
pub(crate) fn append(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
) -> Result<u16, ParseError> {
    let index = super::heap::fresh_index(lower, ir, span)?;
    // The five in-scope builtin carriers install their own `prototype` data
    // row at bootstrap, so no builtin code needs a root-table answer here.
    let carrier = lower.slot(span)?;
    let code = lower.slot(span)?;
    let receiver = lower.slot(span)?;
    let args = lower.slot(span)?;
    let h = lower.objects.expect("object runtime");
    let allocate = lower.intrinsics.expect("intrinsics").allocate;
    let root = lower.intrinsics.expect("intrinsics").root;
    let type_error = lower.intrinsics.expect("intrinsics").type_error;
    let helper = lower.heap_ref(span)?.helpers;
    let mut e = Emit::new(lower, span, carrier)?;
    let l = e.line;
    let sp = e.lower.ir_span(span);
    let out = e.temp(
        value::logical_result_schema(),
        value::result_ok(value::completion_normal(), sp),
    )?;
    let bools = result_schema(T::Bool);
    let frame = e.integer()?;
    let base = e.integer()?;
    let kind = e.integer()?;
    let constructor_code = e.integer()?;
    let len = e.integer()?;
    let err = e.boxed()?;
    let err2 = e.boxed()?;
    let proto = e.boxed()?;
    let instance = e.boxed()?;
    let rhs = e.boxed()?;
    let ok = e.boolean()?;
    let ok2 = e.boolean()?;
    let defined = e.temp(
        bools.clone(),
        result(bools.clone(), true, E::Bool(true), sp),
    )?;
    let defined2 = e.temp(
        bools.clone(),
        result(bools.clone(), true, E::Bool(true), sp),
    )?;
    let mut body = vec![
        assert_stmt(
            or(
                eq(var(code), int(super::intrinsics::FUNCTION_PROTOTYPE_GET_CODE)),
                eq(var(code), int(super::intrinsics::FUNCTION_PROTOTYPE_SET_CODE)),
            ),
            l,
        ),
        assign(frame, call(helper.root_enter, vec![var(carrier), int(8)]), l),
        e.root(var(frame), int(0), var(receiver))?,
    ];
    // Every rejection is the same guest TypeError the realm's private
    // allocator already produces: the adapter never invents a message row.
    let fail = {
        let mut fail = vec![assign(err, call(type_error, vec![var(carrier)]), l)];
        fail.push(e.root(var(frame), int(7), var(err))?);
        fail.push(assign(out, value::result_err(var(err), sp), l));
        fail
    };
    let refused1 = {
        let mut refused = vec![assign(err, call(type_error, vec![var(carrier)]), l)];
        refused.push(e.root(var(frame), int(7), var(err))?);
        refused.push(assign(out, value::result_err(var(err), sp), l));
        refused
    };
    let refused2 = {
        let mut refused = vec![assign(err2, call(type_error, vec![var(carrier)]), l)];
        refused.push(e.root(var(frame), int(7), var(err2))?);
        refused.push(assign(out, value::result_err(var(err2), sp), l));
        refused
    };

    // GET. The key String, the Object.prototype value and the fresh instance
    // are published in entries 1..=4 before any nested allocation can collect.
    let mut getter = Vec::new();
    let prototype_key = super::intrinsics::string(
        &mut e,
        super::intrinsics::FUNCTION_PROTOTYPE_NAME,
        frame,
        1,
        &mut getter,
    )?;
    getter.push(assign(proto, call(root, vec![var(carrier), int(0)]), l));
    getter.push(e.root(var(frame), int(2), var(proto))?);
    getter.push(assign(
        instance,
        call(
            allocate,
            vec![
                var(carrier),
                int(layout::KIND_OBJECT),
                var(proto),
                int(0),
                int(0),
                int(0),
                int(0),
                var(frame),
                int(3),
            ],
        ),
        l,
    ));
    let constructor_key = super::intrinsics::string(&mut e, "constructor", frame, 4, &mut getter)?;
    getter.push(assign(
        defined,
        call(
            h.define,
            vec![
                var(carrier),
                var(instance),
                var(constructor_key),
                data_descriptor(
                    var(receiver),
                    E::Bool(true),
                    E::Bool(true),
                    E::Bool(true),
                ),
                E::Bool(false),
            ],
        ),
        l,
    ));
    let mut constructor_ok = vec![branch(not(var(ok)), refused1.clone(), Vec::new(), l)];
    constructor_ok.push(assign(
        defined2,
        call(
            h.define,
            vec![
                var(carrier),
                var(receiver),
                var(prototype_key),
                data_descriptor(var(instance), E::Bool(true), E::Bool(true), E::Bool(true)),
                E::Bool(false),
            ],
        ),
        l,
    ));
    let mut install_ok = vec![branch(not(var(ok2)), refused2.clone(), Vec::new(), l)];
    install_ok.push(assign(
        out,
        value::result_ok(value::completion_return(var(instance)), sp),
        l,
    ));
    constructor_ok.push(matched(
        &mut e,
        defined2,
        bools.clone(),
        ok2,
        err2,
        install_ok,
        vec![assign(out, value::result_err(var(err2), sp), l)],
    )?);
    getter.push(matched(
        &mut e,
        defined,
        bools.clone(),
        ok,
        err,
        constructor_ok,
        vec![assign(out, value::result_err(var(err), sp), l)],
    )?);

    // SET. The supplied actual is stored verbatim, including a non-object
    // value such as 7; a missing actual is Undefined, exactly like the
    // ordinary single-parameter accessor call.
    let mut setter = Vec::new();
    let prototype_key = super::intrinsics::string(
        &mut e,
        super::intrinsics::FUNCTION_PROTOTYPE_NAME,
        frame,
        1,
        &mut setter,
    )?;
    setter.push(assign(
        len,
        call(B::Len.call_index(), vec![E::Borrow(Box::new(var(args)))]),
        l,
    ));
    setter.push(assign(
        rhs,
        choose(
            lt(int(0), var(len)),
            call(B::Get.call_index(), vec![E::Borrow(Box::new(var(args))), int(0)]),
            value::undefined(),
        ),
        l,
    ));
    setter.push(e.root(var(frame), int(2), var(rhs))?);
    setter.push(assign(
        defined,
        call(
            h.define,
            vec![
                var(carrier),
                var(receiver),
                var(prototype_key),
                data_descriptor(var(rhs), E::Bool(true), E::Bool(true), E::Bool(true)),
                E::Bool(false),
            ],
        ),
        l,
    ));
    let mut store_ok = vec![branch(not(var(ok)), refused1.clone(), Vec::new(), l)];
    store_ok.push(assign(
        out,
        value::result_ok(value::completion_normal(), sp),
        l,
    ));
    setter.push(matched(
        &mut e,
        defined,
        bools.clone(),
        ok,
        err,
        store_ok,
        vec![assign(out, value::result_err(var(err), sp), l)],
    )?);

    // GET body for a non-constructor function node. Every builtin carrier
    // has its own bootstrap `prototype` row (found by the ordinary Get before
    // this accessor), so a remaining builtin code only appears after that row
    // was deleted. The pinned answer there is `undefined`, never the realm
    // root again. Intrinsic methods and bound products are the same class.
    let intrinsic_reply = vec![assign(
        out,
        value::result_ok(value::completion_return(value::undefined()), sp),
        l,
    )];

    // Validate the receiver shape before touching the heap: only an ordinary
    // object can carry the base cell `checked_base` dereferences, so a
    // non-object receiver must fall into the guest TypeError arm without a
    // host-side assertion. Kind is decoded only after the tag holds. The
    // code cell then selects one of the four documented classes.
    body.push(branch(
        tag_is(var(receiver), value::OBJECT),
        vec![
            assign(base, e.base(var(receiver)), l),
            assign(kind, e.decoded(var(base)), l),
            branch(
                eq(var(kind), int(layout::KIND_FUNCTION)),
                vec![
                    assign(
                        constructor_code,
                        e.decoded(add(var(base), int(5))),
                        l,
                    ),
                    branch(
                        eq(
                            var(constructor_code),
                            int(super::intrinsics::FUNCTION_PROTOTYPE_CODE),
                        ),
                        fail.clone(),
                        vec![branch(
                            lt(var(constructor_code), int(256)),
                            vec![branch(
                                eq(var(code), int(super::intrinsics::FUNCTION_PROTOTYPE_GET_CODE)),
                                getter,
                                setter.clone(),
                                l,
                            )],
                            vec![branch(
                                eq(var(code), int(super::intrinsics::FUNCTION_PROTOTYPE_GET_CODE)),
                                intrinsic_reply,
                                // SET on a non-source, non-256 function node:
                                // a bound product stores an ordinary own row,
                                // and a builtin constructor carrier keeps a
                                // writable `prototype` row (normally reached
                                // by the ordinary own row before this
                                // accessor). Every ordinary intrinsic method
                                // node rejects the write.
                                vec![branch(
                                    or(
                                        eq(
                                            var(constructor_code),
                                            int(super::intrinsics::FUNCTION_BIND_CODE),
                                        ),
                                        or(
                                            or(
                                                eq(
                                                    var(constructor_code),
                                                    int(super::intrinsics::OBJECT_CODE),
                                                ),
                                                eq(
                                                    var(constructor_code),
                                                    int(super::intrinsics::ARRAY_NAMESPACE_CODE),
                                                ),
                                            ),
                                            or(
                                                eq(
                                                    var(constructor_code),
                                                    int(super::intrinsics::NUMBER_NAMESPACE_CODE),
                                                ),
                                                or(
                                                    eq(
                                                        var(constructor_code),
                                                        int(super::intrinsics::STRING_NAMESPACE_CODE),
                                                    ),
                                                    eq(
                                                        var(constructor_code),
                                                        int(super::intrinsics::BOOLEAN_CODE),
                                                    ),
                                                ),
                                            ),
                                        ),
                                    ),
                                    setter,
                                    refused2,
                                    l,
                                )],
                                l,
                            )],
                            l,
                        )],
                        l,
                    ),
                ],
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
        "__compat_es5_function_prototype_api",
        index,
        vec![carrier, code, receiver, args],
        vec![
            carrier_schema.clone(),
            T::Int,
            value::schema(),
            T::Array(Box::new(value::schema())),
        ],
        value::logical_result_schema(),
        body,
        var(out),
        l,
    );
    Ok(index)
}
