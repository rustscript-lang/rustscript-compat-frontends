//! Managed implementation shared by `Array(...)` and the admitted `new Array`.

use super::super::{heap as layout, lowerer::Lowerer, representation as value};
use super::intrinsics::{self, Intrinsics};
use super::object::*;
use vm::{BuiltinFunction as B, Expr as E, FrontendIr, ParseError, Stmt as S, TypeSchema as T};

const MAX_LENGTH: f64 = 1_073_741_823.0;

pub(super) fn append(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
) -> Result<u16, ParseError> {
    let index = super::heap::fresh_index(lower, ir, span)?;
    let h = lower.heap_ref(span)?.helpers;
    let intrinsics = lower.intrinsics.expect("intrinsics");
    let c = lower.slot(span)?;
    let args = lower.slot(span)?;
    let mut e = Emit::new(lower, span, c)?;
    let l = e.line;
    let sp = e.lower.ir_span(span);
    let schema = value::logical_result_schema();
    let out = e.temp(schema.clone(), value::result_ok(value::completion_normal(), sp))?;
    let frame = e.integer()?;
    let count = e.integer()?;
    let length = e.integer()?;
    let item = e.boxed()?;
    let numeric = e.temp(T::Float, E::Float(0.0))?;
    let invalid = e.boolean()?;
    let proto = e.boxed()?;
    let allocation = e.temp(value::allocation_schema(), value::allocation(int(0), int(0), int(0)))?;
    let base = e.integer()?;
    let offset = e.integer()?;
    let i = e.integer()?;
    let array = e.boxed()?;
    let args_ref = || E::Borrow(Box::new(var(args)));
    let get = |i| call(B::Get.call_index(), vec![args_ref(), i]);
    let mut body = vec![
        assign(frame, call(h.root_enter, vec![var(c), int(3)]), l),
        assign(count, call(B::Len.call_index(), vec![args_ref()]), l),
        assign(length, var(count), l),
    ];
    // For this bound, ToInt32 is valid exactly when the unsigned residue is
    // <= MAX_LENGTH: its high half would become negative after signing.
    let (u32_value, mut length_only) = super::math::math_ext::to_uint32(&mut e, l, numeric)?;
    length_only.insert(0, assign(numeric, num(field(var(item), "number")), l));
    length_only.extend([
        assign(count, int(0), l),
        assign(invalid, lt(E::Float(MAX_LENGTH), var(u32_value)), l),
        branch(
            not(var(invalid)),
            vec![assign(length, call(e.decode_u32, vec![num(var(u32_value))]), l)],
            vec![],
            l,
        ),
    ]);
    body.push(branch(
        eq(var(count), int(1)),
        vec![assign(item, get(int(0)), l), branch(tag_is(var(item), value::NUMBER), length_only, vec![], l)],
        vec![],
        l,
    ));

    // Each actual has a caller-frame root. Nothing allocates between
    // node_begin and node_publish, so fill the pending node in a single pass.
    let stride = layout::ARRAY_TAGGED_CELLS_PER_ENTRY;
    let mut make = vec![
        assign(proto, call(intrinsics.array_proto, vec![var(c)]), l),
        assign(allocation, call(h.node_begin, vec![
            var(c), add(int(layout::ARRAY_HEADER_CELLS), mul(var(count), int(stride))), int(layout::KIND_TAGGED_ARRAY),
        ]), l),
        assign(base, field(var(allocation), "base"), l),
    ];
    for (cell, payload) in [
        (2, var(length)),
        (layout::ARRAY_PROTO_OFFSET, field(var(proto), "ref_slot")),
        (layout::ARRAY_PROTO_OFFSET + 1, field(var(proto), "generation")),
        (layout::ARRAY_PRESENT_OFFSET, add(var(count), int(1))),
    ] {
        make.push(e.write(add(var(base), int(cell)), payload)?);
    }
    let mut fill = vec![
        assign(item, get(var(i)), l),
        assign(offset, add(var(base), add(int(layout::ARRAY_HEADER_CELLS), mul(var(i), int(stride)))), l),
    ];
    for (cell, payload) in [
        (0, field(var(item), "tag")),
        (1, field(var(item), "number")),
        (2, choose(field(var(item), "flag"), int(1), int(0))),
        (3, field(var(item), "ref_slot")),
        (4, field(var(item), "generation")),
    ] {
        fill.push(e.write(add(var(offset), int(cell)), payload)?);
    }
    fill.push(assign(i, add(var(i), int(1)), l));
    make.extend([
        S::While { condition: lt(var(i), var(count)), body: fill, line: l },
        assign(array, call(h.node_publish, vec![
            var(c), field(var(allocation), "slot"), field(var(allocation), "generation"), var(base), var(frame), int(2),
        ]), l),
        assign(out, value::result_ok(value::completion_return(var(array)), sp), l),
    ]);
    let range = invalid_length(&mut e, intrinsics, frame, out)?;
    body.push(branch(var(invalid), range, make, l));
    body.push(e.effect(h.root_leave, vec![var(c), var(frame)])?);
    declare(
        ir, stmts, "__compat_es5_array_constructor", index, vec![c, args],
        vec![carrier_schema.clone(), T::Array(Box::new(value::schema()))],
        schema, e.finish(body), var(out), l,
    );
    Ok(index)
}

/// Box one Core `Array<JsValue>` (the activation invocation vector) into a
/// real guest Tagged Array value. The only consumer is the bare-`arguments`
/// actual of `Array.isArray`: the realm method inspects a JsValue, while the
/// invocation vector is a Core container with no guest identity of its own.
/// Elements are copied as-is; the result is a real Array (`Array.isArray`
/// recognizes `KIND_TAGGED_ARRAY`), and the copy is what a rewritten method
/// observes.
pub(crate) fn append_from_values(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
) -> Result<u16, ParseError> {
    let index = super::heap::fresh_index(lower, ir, span)?;
    let h = lower.heap_ref(span)?.helpers;
    let intrinsics = lower.intrinsics.expect("intrinsics");
    let c = lower.slot(span)?;
    let args = lower.slot(span)?;
    let mut e = Emit::new(lower, span, c)?;
    let l = e.line;
    let frame = e.integer()?;
    let count = e.integer()?;
    let item = e.boxed()?;
    let proto = e.boxed()?;
    let allocation = e.temp(value::allocation_schema(), value::allocation(int(0), int(0), int(0)))?;
    let base = e.integer()?;
    let offset = e.integer()?;
    let i = e.integer()?;
    let array = e.boxed()?;
    let args_ref = || E::Borrow(Box::new(var(args)));
    let get = |i| call(B::Get.call_index(), vec![args_ref(), i]);
    let stride = layout::ARRAY_TAGGED_CELLS_PER_ENTRY;
    let mut body = vec![
        assign(frame, call(h.root_enter, vec![var(c), int(3)]), l),
        assign(count, call(B::Len.call_index(), vec![args_ref()]), l),
        // `array_proto` is a permanent root and reads without allocating.
        assign(proto, call(intrinsics.array_proto, vec![var(c)]), l),
        assign(
            allocation,
            call(
                h.node_begin,
                vec![
                    var(c),
                    add(
                        int(layout::ARRAY_HEADER_CELLS),
                        mul(var(count), int(stride)),
                    ),
                    int(layout::KIND_TAGGED_ARRAY),
                ],
            ),
            l,
        ),
        assign(base, field(var(allocation), "base"), l),
    ];
    for (cell, payload) in [
        (2, var(count)),
        (layout::ARRAY_PROTO_OFFSET, field(var(proto), "ref_slot")),
        (
            layout::ARRAY_PROTO_OFFSET + 1,
            field(var(proto), "generation"),
        ),
        (layout::ARRAY_PRESENT_OFFSET, add(var(count), int(1))),
    ] {
        body.push(e.write(add(var(base), int(cell)), payload)?);
    }
    let mut fill = vec![
        assign(item, get(var(i)), l),
        assign(
            offset,
            add(
                var(base),
                add(int(layout::ARRAY_HEADER_CELLS), mul(var(i), int(stride))),
            ),
            l,
        ),
    ];
    for (cell, payload) in [
        (0, field(var(item), "tag")),
        (1, field(var(item), "number")),
        (2, choose(field(var(item), "flag"), int(1), int(0))),
        (3, field(var(item), "ref_slot")),
        (4, field(var(item), "generation")),
    ] {
        fill.push(e.write(add(var(offset), int(cell)), payload)?);
    }
    fill.push(assign(i, add(var(i), int(1)), l));
    body.extend([
        S::While {
            condition: lt(var(i), var(count)),
            body: fill,
            line: l,
        },
        assign(
            array,
            call(
                h.node_publish,
                vec![
                    var(c),
                    field(var(allocation), "slot"),
                    field(var(allocation), "generation"),
                    var(base),
                    var(frame),
                    int(2),
                ],
            ),
            l,
        ),
        e.effect(h.root_leave, vec![var(c), var(frame)])?,
    ]);
    declare(
        ir,
        stmts,
        "__compat_es5_array_from_values",
        index,
        vec![c, args],
        vec![carrier_schema.clone(), T::Array(Box::new(value::schema()))],
        value::schema(),
        e.finish(body),
        var(array),
        l,
    );
    Ok(index)
}

fn invalid_length(
    e: &mut Emit<'_, '_>,
    intrinsics: Intrinsics,
    frame: u16,
    out: u16,
) -> Result<Vec<S>, ParseError> {
    let l = e.line;
    let sp = e.lower.ir_span(e.span);
    let mut body = Vec::new();
    let key = intrinsics::string(e, "message", frame, 0, &mut body)?;
    let message = intrinsics::string(e, "invalid array length", frame, 1, &mut body)?;
    let err = e.boxed()?;
    let allocate = intrinsics.range_error.expect("Array selects RangeError allocator");
    body.push(assign(err, call(allocate, vec![var(e.carrier), int(1)]), l));
    // No allocation before the error leaves this frame in the Result.
    intrinsics::trusted_data(e, var(err), 0, var(key), var(message), (true, false, true), &mut body)?;
    body.push(assign(out, value::result_err(var(err), sp), l));
    Ok(body)
}
