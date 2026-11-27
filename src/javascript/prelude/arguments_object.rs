//! Private two-way mirror between the activation's Core invocation vector and
//! its single guest `arguments` object.
//!
//! Pinned Micro materializes `arguments` as one real Array per activation, so
//! a rewritten `Array.isArray` observes (and can mutate) the same object on
//! every pass. The invocation vector is a Core `Array<JsValue>`, which is not a
//! guest value at all, so the object handed to guest code is one heap Array per
//! activation and this helper keeps the two in step:
//!
//! * `dir == 0` copies the object into the vector and returns the new vector
//!   (Core arrays are immutable values, so the caller must reassign).
//! * `dir == 1` copies the vector into the object's cells in place.
//!
//! Length is part of the mirror: truncation uses the existing `Slice`
//! primitive and growth appends in index order, so holes read as Undefined on
//! both sides. No guest call, allocation beyond the vector itself, or root
//! frame is involved; element payloads are JsValue handles.

use super::super::{lowerer::Lowerer, representation as value};
use super::heap::fresh_index;
use super::object::{add, assign, branch, call, declare, eq, int, lt, not, var};
use vm::{BuiltinFunction as B, Expr as E, FrontendIr, ParseError, Stmt as S, TypeSchema as T};

pub(crate) fn append_sync(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
) -> Result<u16, ParseError> {
    let index = fresh_index(lower, ir, span)?;
    let heap = lower.heap_ref(span)?;
    let h = heap.helpers;
    let c = lower.slot(span)?;
    let vector = lower.slot(span)?;
    let object = lower.slot(span)?;
    let direction = lower.slot(span)?;
    let slot = lower.slot(span)?;
    let generation = lower.slot(span)?;
    let base = lower.slot(span)?;
    let count = lower.slot(span)?;
    let current = lower.slot(span)?;
    let cursor = lower.slot(span)?;
    let payload = lower.slot(span)?;
    let line = lower.line(span);
    let vector_ref = || E::Borrow(Box::new(var(vector)));
    let item = |lower_index: u16| call(B::Get.call_index(), vec![vector_ref(), var(lower_index)]);

    let mut body = vec![
        S::Let {
            index: slot,
            declared_schema: Some(T::Int),
            expr: value::field(object, "ref_slot"),
            line,
        },
        S::Let {
            index: generation,
            declared_schema: Some(T::Int),
            expr: value::field(object, "generation"),
            line,
        },
        S::Let {
            index: base,
            declared_schema: Some(T::Int),
            expr: call(h.checked_base, vec![var(c), var(slot), var(generation)]),
            line,
        },
        S::Let {
            index: count,
            declared_schema: Some(T::Int),
            expr: E::IfElse {
                condition: Box::new(eq(var(direction), int(0))),
                then_expr: Box::new(call(
                    h.decode_u32,
                    vec![call(h.array_length, vec![var(c), var(base)])],
                )),
                else_expr: Box::new(call(
                    B::Len.call_index(),
                    vec![E::Borrow(Box::new(var(vector)))],
                )),
            },
            line,
        },
        S::Let {
            index: current,
            declared_schema: Some(T::Int),
            expr: int(0),
            line,
        },
        S::Let {
            index: cursor,
            declared_schema: Some(T::Int),
            expr: int(0),
            line,
        },
        S::Let {
            index: payload,
            declared_schema: Some(value::schema()),
            expr: value::undefined(),
            line,
        },
        branch(
            eq(var(direction), int(0)),
            vec![
                // Box -> vector. Truncate first: `Set` appends at index ==
                // length, so a shorter source must not leave a stale tail.
                assign(current, call(B::Len.call_index(), vec![vector_ref()]), line),
                branch(
                    lt(var(count), var(current)),
                    vec![assign(
                        vector,
                        call(B::Slice.call_index(), vec![var(vector), int(0), var(count)]),
                        line,
                    )],
                    vec![],
                    line,
                ),
                assign(cursor, int(0), line),
                S::While {
                    condition: lt(var(cursor), var(count)),
                    body: vec![
                        assign(
                            payload,
                            call(h.array_get, vec![var(c), var(base), var(cursor)]),
                            line,
                        ),
                        assign(
                            vector,
                            call(B::Set.call_index(), vec![var(vector), var(cursor), var(payload)]),
                            line,
                        ),
                        assign(cursor, add(var(cursor), int(1)), line),
                    ],
                    line,
                },
            ],
            vec![
                // Vector -> object. The object's length cell is resized first
                // (in place, base unchanged) so every `array_put` below writes
                // inside the physically present extent.
                assign(
                    current,
                    call(
                        h.decode_u32,
                        vec![call(h.array_length, vec![var(c), var(base)])],
                    ),
                    line,
                ),
                branch(
                    not(eq(var(current), var(count))),
                    // The setter resizes the same node in place and returns
                    // Null, so the base stays valid for the writes below.
                    vec![S::Expr {
                        expr: call(h.array_set_length, vec![var(c), var(base), var(count)]),
                        line,
                    }],
                    vec![],
                    line,
                ),
                assign(cursor, int(0), line),
                S::While {
                    condition: lt(var(cursor), var(count)),
                    body: vec![
                        // `array_put` relocates storage when it fills a hole
                        // (`[present, length)`), updating the directory row
                        // and releasing the old extent. The base cached before
                        // the resize is dead after that first relocation, so
                        // re-read the directory row on every iteration.
                        assign(
                            base,
                            call(h.checked_base, vec![var(c), var(slot), var(generation)]),
                            line,
                        ),
                        assign(payload, item(cursor), line),
                        S::Expr {
                            expr: call(
                                h.array_put,
                                vec![
                                    var(c),
                                    var(base),
                                    var(cursor),
                                    var(payload),
                                    var(slot),
                                    var(generation),
                                ],
                            ),
                            line,
                        },
                        assign(cursor, add(var(cursor), int(1)), line),
                    ],
                    line,
                },
            ],
            line,
        ),
    ];
    let vector_schema = T::Array(Box::new(value::schema()));
    declare(
        ir,
        stmts,
        "__compat_es5_arguments_sync",
        index,
        vec![c, vector, object, direction],
        vec![
            carrier_schema.clone(),
            vector_schema.clone(),
            value::schema(),
            T::Int,
        ],
        vector_schema,
        std::mem::take(&mut body),
        var(vector),
        line,
    );
    Ok(index)
}
