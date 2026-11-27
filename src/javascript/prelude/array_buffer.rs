//! Managed fixed ArrayBuffer backing and the branded byteLength accessor.
use super::super::{
    heap as layout,
    lowerer::{Lowerer, assert_stmt},
    representation as value,
};
use super::intrinsics;
use super::object::*;
use vm::{BuiltinFunction as B, Expr as E, FrontendIr, ParseError, Stmt as S, TypeSchema as T};

pub(crate) const CALL: i64 = 300;
pub(crate) const GET_LENGTH: i64 = 301;
pub(crate) const CONSTRUCT: i64 = 0;
const INDEX_MAX: f64 = 1_073_741_823.0;
const LENGTH_KEY: &str = "byteLength";
const CONSTRUCTOR_ROWS: i64 = 2;
const PROTOTYPE_ROWS: i64 = 2;
const GETTER_ROWS: i64 = 1;
pub(crate) const BOOTSTRAP_NODES: u64 = 4;
pub(crate) const BOOTSTRAP_CELLS: u64 = (4 * layout::RAW_HEADER_CELLS
    + 2 * layout::FUNCTION_CELLS
    + layout::OBJECT_HEADER_CELLS
    + (CONSTRUCTOR_ROWS + PROTOTYPE_ROWS + GETTER_ROWS) * layout::OBJECT_CELLS_PER_PROPERTY
    + layout::STRING_HEADER_CELLS) as u64
    + LENGTH_KEY.len() as u64;

pub(super) fn bootstrap(
    e: &mut Emit<'_, '_>,
    allocate: u16,
    permanent: u16,
    stage: u16,
    (object_proto, function_proto): (u16, u16),
    (length, prototype, constructor): (u16, u16, u16),
    body: &mut Vec<S>,
) -> Result<(), ParseError> {
    let l = e.line;
    let (ctor, proto, getter) = (e.boxed()?, e.boxed()?, e.boxed()?);
    let root = intrinsics::array_buffer_constructor_root(e.lower);
    for (out, kind, parent, rows, code, arity, frame, entry) in [
        (
            ctor,
            layout::KIND_FUNCTION,
            function_proto,
            CONSTRUCTOR_ROWS,
            CALL,
            1,
            permanent,
            root,
        ),
        (
            proto,
            layout::KIND_OBJECT,
            object_proto,
            PROTOTYPE_ROWS,
            0,
            0,
            stage,
            1,
        ),
        (
            getter,
            layout::KIND_FUNCTION,
            function_proto,
            GETTER_ROWS,
            GET_LENGTH,
            0,
            stage,
            2,
        ),
    ] {
        body.push(assign(
            out,
            call(
                allocate,
                vec![
                    var(e.carrier),
                    int(kind),
                    var(parent),
                    int(rows),
                    int(code),
                    int(arity), int(0),
                    var(frame),
                    int(entry),
                ],
            ),
            l,
        ));
    }
    let attrs = (false, false, false);
    intrinsics::trusted_data(
        e,
        var(ctor),
        0,
        var(length),
        value::number(E::Float(1.0)),
        attrs,
        body,
    )?;
    intrinsics::trusted_data(e, var(ctor), 1, var(prototype), var(proto), attrs, body)?;
    intrinsics::trusted_data(
        e,
        var(getter),
        0,
        var(length),
        value::number(E::Float(0.0)),
        attrs,
        body,
    )?;
    intrinsics::trusted_data(
        e,
        var(proto),
        0,
        var(constructor),
        var(ctor),
        (true, false, true),
        body,
    )?;
    let key = intrinsics::string(e, LENGTH_KEY, stage, 0, body)?;
    let base = e.integer()?;
    body.push(assign(base, e.base(var(proto)), l));
    let row = |offset| {
        add(
            var(base),
            int(layout::OBJECT_HEADER_CELLS + layout::OBJECT_CELLS_PER_PROPERTY + offset),
        )
    };
    // Accessor row: configurable, non-enumerable, present getter, absent setter.
    for (offset, payload) in [
        (0, field(var(key), "ref_slot")),
        (1, field(var(key), "generation")),
        (2, int(8 + 4)),
        (3, field(var(getter), "ref_slot")),
        (4, field(var(getter), "generation")),
        (5, int(0)),
        (6, int(0)),
    ] {
        body.push(e.write(row(offset), payload)?);
    }
    body.push(e.write(
        add(var(base), int(2)),
        pack_header(int(PROTOTYPE_ROWS), E::Bool(true), E::Bool(false)),
    )?);
    Ok(())
}

pub(crate) fn append(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    quota_error_host: Option<u16>,
) -> Result<u16, ParseError> {
    let index = super::heap::fresh_index(lower, ir, span)?;
    let heap = lower.heap_ref(span)?.helpers;
    let intrinsic = lower.intrinsics.expect("intrinsics");
    let number_value = lower.objects.expect("object runtime").numeric_default_value;
    let parser = lower
        .published_number_parser()
        .expect("object runtime number parser");
    let root = intrinsics::array_buffer_constructor_root(lower);
    let (c, mode, receiver, args) = (
        lower.slot(span)?,
        lower.slot(span)?,
        lower.slot(span)?,
        lower.slot(span)?,
    );
    let mut e = Emit::new(lower, span, c)?;
    let l = e.line;
    let sp = e.lower.ir_span(span);
    let schema = value::logical_result_schema();
    let out = e.temp(
        schema.clone(),
        value::result_ok(value::completion_normal(), sp),
    )?;
    let frame = e.integer()?;
    let base = e.integer()?;
    let length = e.integer()?;
    let cursor = e.integer()?;
    let raw = e.boxed()?;
    let primitive = e.boxed()?;
    let err = e.boxed()?;
    let proto = e.boxed()?;
    let bytes = e.boxed()?;
    let number = e.temp(T::Number, E::Float(0.0))?;
    let converted = e.temp(
        value::value_result_schema(),
        result(value::value_result_schema(), true, value::undefined(), sp),
    )?;
    let allocation = e.temp(
        value::allocation_schema(),
        value::allocation(int(0), int(0), int(0)),
    )?;
    let ret = |v| assign(out, value::result_ok(value::completion_return(v), sp), l);
    let throw = |v| assign(out, value::result_err(v, sp), l);
    let begin = || assign(frame, call(heap.root_enter, vec![var(c), int(3)]), l);

    let error = |e: &mut Emit<'_, '_>, message, range| -> Result<Vec<S>, ParseError> {
        let mut body = Vec::new();
        let key = intrinsics::string(e, "message", frame, 0, &mut body)?;
        let text = intrinsics::string(e, message, frame, 1, &mut body)?;
        let allocate = if range {
            call(
                intrinsic
                    .range_error
                    .expect("ArrayBuffer selects RangeError"),
                vec![var(c), int(1)],
            )
        } else {
            call(
                intrinsic.allocate,
                vec![
                    var(c),
                    int(layout::KIND_OBJECT),
                    call(intrinsic.root, vec![var(c), int(3)]),
                    int(1),
                    int(0),
                    int(0), int(6),
                    var(frame),
                    int(2),
                ],
            )
        };
        body.push(assign(err, allocate, l));
        intrinsics::trusted_data(
            e,
            var(err),
            0,
            var(key),
            var(text),
            (true, false, true),
            &mut body,
        )?;
        body.push(throw(var(err)));
        Ok(body)
    };
    let mut brand_failure = vec![begin()];
    brand_failure.extend(error(&mut e, "expected an ArrayBuffer", false)?);
    brand_failure.push(e.effect(heap.root_leave, vec![var(c), var(frame)])?);
    let read_length = vec![
        assign(
            length,
            e.decoded(add(var(base), int(layout::BYTES_LENGTH_OFFSET))),
            l,
        ),
        assert_stmt(
            and(
                not(lt(int(layout::BYTES_MAX_LENGTH), var(length))),
                not(lt(
                    e.decoded(sub(var(base), int(layout::RAW_HEADER_CELLS))),
                    add(
                        int(layout::RAW_HEADER_CELLS + layout::BYTES_HEADER_CELLS),
                        var(length),
                    ),
                )),
            ),
            l,
        ),
        ret(value::number(num(var(length)))),
    ];
    let get = vec![branch(
        tag_is(var(receiver), value::OBJECT),
        vec![
            assign(base, e.base(var(receiver)), l),
            branch(
                eq(e.decoded(var(base)), int(layout::KIND_BYTES)),
                read_length,
                brand_failure.clone(),
                l,
            ),
        ],
        brand_failure,
        l,
    )];
    let mut call_only = vec![begin()];
    call_only.extend(error(&mut e, "must be called with new", false)?);
    call_only.push(e.effect(heap.root_leave, vec![var(c), var(frame)])?);

    // Prototype row 1 is immutable and points to the rooted ordinary prototype.
    // The helper never exposes the backing as Array or through the UTF-16 bridge.
    let mut make = vec![
        assign(length, call(e.decode, vec![num(var(number))]), l),
        assign(proto, call(intrinsic.root, vec![var(c), int(root)]), l),
        assign(base, e.base(var(proto)), l),
    ];
    let row = add(
        var(base),
        int(layout::FUNCTION_CELLS + layout::OBJECT_CELLS_PER_PROPERTY),
    );
    make.push(assert_stmt(
        eq(
            e.decoded(add(row.clone(), int(2))),
            int(value::OBJECT * layout::OBJECT_TAG_FACTOR),
        ),
        l,
    ));
    make.push(assign(
        proto,
        value::dynamic_value(
            int(value::OBJECT),
            E::Float(0.0),
            E::Bool(false),
            e.decoded(add(row.clone(), int(5))),
            e.generation(add(row, int(6))),
        ),
        l,
    ));
    make.push(assert_stmt(
        eq(e.decoded(e.base(var(proto))), int(layout::KIND_OBJECT)),
        l,
    ));
    make.extend([
        assign(
            allocation,
            call(
                heap.node_begin,
                vec![
                    var(c),
                    add(int(layout::BYTES_HEADER_CELLS), var(length)),
                    int(layout::KIND_BYTES),
                ],
            ),
            l,
        ),
        assign(base, field(var(allocation), "base"), l),
        e.write(add(var(base), int(2)), int(0))?,
        e.write(add(var(base), int(3)), field(var(proto), "ref_slot"))?,
        e.write(add(var(base), int(4)), field(var(proto), "generation"))?,
        e.write(
            add(var(base), int(layout::BYTES_LENGTH_OFFSET)),
            var(length),
        )?,
        S::While {
            condition: lt(var(cursor), var(length)),
            body: vec![
                e.write(
                    add(add(var(base), int(layout::BYTES_HEADER_CELLS)), var(cursor)),
                    E::Float(0.0),
                )?,
                assign(cursor, add(var(cursor), int(1)), l),
            ],
            line: l,
        },
        assign(
            bytes,
            call(
                heap.node_publish,
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
        ret(var(bytes)),
    ]);
    let invalid = error(&mut e, "invalid array index", true)?;
    // The per-buffer quota is a fatal host error, separate from the guest
    // RangeError and checked before reserving any backing cells. Leave the
    // frame here because a host error bypasses the constructor epilogue.
    //
    // `quota_error_host` is `None` exactly when every reachable construction in
    // the source was proven to name a literal inside the byte budget: the
    // branch is then dead, so it keeps the guest RangeError instead of calling
    // an adapter the host does not provide.
    let quota_failure = match quota_error_host {
        Some(quota_error_host) => {
            let quota_sink = e.temp(T::Null, E::Null)?;
            vec![
                e.effect(heap.root_leave, vec![var(c), var(frame)])?,
                assign(
                    quota_sink,
                    call(
                        quota_error_host,
                        vec![E::String("array buffer length exceeds host quota".to_owned())],
                    ),
                    l,
                ),
            ]
        }
        None => Vec::new(),
    };
    // No adapter means no reachable quota branch, so it answers the same guest
    // RangeError the domain failure uses instead of asserting. The clone keeps
    // the domain branch above intact.
    let overflow = if quota_failure.is_empty() {
        invalid.clone()
    } else {
        quota_failure
    };
    let numeric = choose(
        tag_is(var(primitive), value::NUMBER),
        num(field(var(primitive), "number")),
        choose(
            tag_is(var(primitive), value::BOOLEAN),
            choose(field(var(primitive), "flag"), E::Float(1.0), E::Float(0.0)),
            choose(
                tag_is(var(primitive), value::STRING),
                call(parser, vec![var(c), var(primitive), int(0), E::Float(0.0)]),
                E::Float(0.0),
            ),
        ),
    );
    let convert_ok = vec![
        assign(number, numeric, l),
        assign(
            number,
            choose(
                and(
                    eq(var(number), var(number)),
                    not(eq(var(number), E::Float(0.0))),
                ),
                num(call(B::MathTrunc.call_index(), vec![num(var(number))])),
                E::Float(0.0),
            ),
            l,
        ),
        branch(
            or(
                lt(var(number), E::Float(0.0)),
                lt(E::Float(INDEX_MAX), var(number)),
            ),
            invalid,
            vec![branch(
                lt(E::Float(layout::BYTES_MAX_LENGTH as f64), var(number)),
                overflow,
                make,
                l,
            )],
            l,
        ),
    ];
    let args_ref = || E::Borrow(Box::new(var(args)));
    let construct = vec![
        assert_stmt(eq(var(mode), int(CONSTRUCT)), l),
        begin(),
        assign(
            raw,
            choose(
                lt(int(0), call(B::Len.call_index(), vec![args_ref()])),
                call(B::Get.call_index(), vec![args_ref(), int(0)]),
                value::undefined(),
            ),
            l,
        ),
        e.root(var(frame), int(0), var(raw))?,
        assign(
            converted,
            call(number_value, vec![var(c), var(raw), E::Bool(false)]),
            l,
        ),
        matched(
            &mut e,
            converted,
            value::value_result_schema(),
            primitive,
            err,
            convert_ok,
            vec![throw(var(err))],
        )?,
        e.effect(heap.root_leave, vec![var(c), var(frame)])?,
    ];
    let body = vec![branch(
        eq(var(mode), int(GET_LENGTH)),
        get,
        vec![branch(eq(var(mode), int(CALL)), call_only, construct, l)],
        l,
    )];
    declare(
        ir,
        stmts,
        "__compat_es5_array_buffer",
        index,
        vec![c, mode, receiver, args],
        vec![
            carrier_schema.clone(),
            T::Int,
            value::schema(),
            T::Array(Box::new(value::schema())),
        ],
        schema,
        e.finish(body),
        var(out),
        l,
    );
    Ok(index)
}
