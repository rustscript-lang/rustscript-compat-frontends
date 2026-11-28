//! Fixed typed-array windows into the managed kind-7 byte backing.
mod float;
mod methods;
use super::super::{
    heap as layout,
    lowerer::{Lowerer, assert_stmt},
    representation as value,
};
use super::{intrinsics, object::*};
use vm::{BuiltinFunction as B, Expr as E, FrontendIr, ParseError, Stmt as S, TypeSchema as T};

pub(crate) const CALL: i64 = 328;
pub(crate) const ABSTRACT: i64 = 329;
pub(crate) const GET_LENGTH: i64 = 330;
pub(crate) const GET_BYTE_LENGTH: i64 = 331;
pub(crate) const GET_BYTE_OFFSET: i64 = 332;
pub(crate) const GET_BUFFER: i64 = 333;
pub(crate) const LAST_CALL: i64 = Element::Float64.call_code();
pub(crate) const SUBARRAY: i64 = 342;
pub(crate) const SET: i64 = 343;
pub(crate) const JOIN: i64 = 344;
pub(crate) const TO_STRING: i64 = 345;

#[derive(Clone, Copy)]
#[repr(u8)]
pub(crate) enum Element {
    Uint8 = 1,
    Int8,
    Uint8Clamped,
    Int16,
    Uint16,
    Int32,
    Uint32,
    Float32,
    Float64,
}

impl Element {
    pub(crate) const ALL: [Self; 9] = [
        Self::Uint8, Self::Int8, Self::Uint8Clamped, Self::Int16,
        Self::Uint16, Self::Int32, Self::Uint32, Self::Float32, Self::Float64,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Uint8 => "Uint8Array",
            Self::Int8 => "Int8Array",
            Self::Uint8Clamped => "Uint8ClampedArray",
            Self::Int16 => "Int16Array",
            Self::Uint16 => "Uint16Array",
            Self::Int32 => "Int32Array",
            Self::Uint32 => "Uint32Array",
            Self::Float32 => "Float32Array",
            Self::Float64 => "Float64Array",
        }
    }

    pub(crate) fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|element| element.name() == name)
    }

    const fn width(self) -> i64 {
        match self {
            Self::Uint8 | Self::Int8 | Self::Uint8Clamped => 1,
            Self::Int16 | Self::Uint16 => 2,
            Self::Int32 | Self::Uint32 | Self::Float32 => 4,
            Self::Float64 => 8,
        }
    }

    pub(crate) const fn bit(self) -> u16 {
        1 << (self as u8 - 1)
    }

    pub(crate) const fn call_code(self) -> i64 {
        match self {
            Self::Uint8 => CALL,
            _ => GET_BUFFER + self as i64 - 1,
        }
    }

    /// Construction calls are compiler-only; positive modes are guest call codes.
    pub(crate) const fn construct_mode(self) -> i64 {
        1 - self as i64
    }
}

fn selected(lower: &Lowerer<'_>) -> impl Iterator<Item = Element> + use<> {
    let mask = lower.typed_array_elements.get();
    Element::ALL.into_iter().filter(move |element| mask & element.bit() != 0)
}
const GETTERS: [(&str, i64); 4] = [
    ("length", GET_LENGTH),
    ("byteLength", GET_BYTE_LENGTH),
    ("byteOffset", GET_BYTE_OFFSET),
    ("buffer", GET_BUFFER),
];
const BPE: &str = "BYTES_PER_ELEMENT";
const CTOR_ROWS: i64 = 3;
const ABSTRACT_ROWS: i64 = 2;
const PROTO_ROWS: i64 = 2;
const METHODS: [(&str, i64, i64); 4] = [
    ("join", JOIN, 1),
    ("toString", TO_STRING, 0),
    ("subarray", SUBARRAY, 2),
    ("set", SET, 1),
];
const SHARED_ROWS: i64 = 5 + METHODS.len() as i64;
pub(crate) fn bootstrap_request(lower: &Lowerer<'_>) -> (u64, u64) {
    let concrete = u64::from(lower.typed_array_elements.get().count_ones());
    let methods = METHODS.len() as i64;
    let nodes = 10 + 2 * concrete + 2 * methods as u64;
    let shared = (5 + methods) * layout::FUNCTION_CELLS + layout::OBJECT_HEADER_CELLS
        + (ABSTRACT_ROWS + SHARED_ROWS + 4 + methods) * layout::OBJECT_CELLS_PER_PROPERTY
        + (4 + methods) * layout::STRING_HEADER_CELLS;
    let pair = layout::FUNCTION_CELLS + layout::OBJECT_HEADER_CELLS
        + (CTOR_ROWS + PROTO_ROWS) * layout::OBJECT_CELLS_PER_PROPERTY;
    let cells = nodes * layout::RAW_HEADER_CELLS as u64 + shared as u64
        + concrete * pair as u64
        + GETTERS[1..].iter().map(|(name, _)| name.len() as u64).sum::<u64>()
        + BPE.len() as u64
        + METHODS.iter().map(|(name, _, _)| name.len() as u64).sum::<u64>();
    (cells, nodes)
}

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
    let (shared, proto, abstract_ctor, ctor, getter) =
        (e.boxed()?, e.boxed()?, e.boxed()?, e.boxed()?, e.boxed()?);
    let carrier = e.carrier;
    // Prototypes are ordinary objects, exactly as upstream MicroQuickJS presents
    // `Uint8Array.prototype` to `Object.prototype.toString` (`[object Object]`,
    // not `[object Error]`). Only Error *instances* carry the legacy|instance
    // brand, and this bootstrap allocates no instances.
    let alloc = |kind, parent, rows, code, arity, frame, entry| {
        call(
            allocate,
            vec![
                var(carrier),
                int(kind),
                var(parent),
                int(rows),
                int(code),
                int(arity),
                int(0),
                var(frame),
                int(entry),
            ],
        )
    };
    body.extend([
        assign(shared, alloc(layout::KIND_OBJECT, object_proto, SHARED_ROWS, 0, 0, stage, 1), l),
        assign(abstract_ctor, alloc(layout::KIND_FUNCTION, function_proto, ABSTRACT_ROWS, ABSTRACT, 0, stage, 3), l),
    ]);
    let fixed = (false, false, false);
    // Pinned mqjs stores no property attribute bits at all: `JSProperty` keeps
    // only `prop_type` (mquickjs.c:266-283), so each `TA_DEF` row is an ordinary
    // own data property that is writable, enumerable and configurable
    // (mqjs_stdlib.c:268-276; deletion always succeeds, mquickjs.c:3414-3415).
    let bpe_attributes = (true, true, true);
    let link = |e: &mut Emit<'_, '_>, function, instance_proto, arity, body: &mut Vec<S>| {
        intrinsics::trusted_data(
            e, var(function), 0, var(length), value::number(E::Float(arity)), fixed, body,
        )?;
        intrinsics::trusted_data(e, var(function), 1, var(prototype), var(instance_proto), fixed, body)?;
        intrinsics::trusted_data(
            e, var(instance_proto), 0, var(constructor), var(function), (true, false, true), body,
        )
    };
    link(e, abstract_ctor, shared, 0.0, body)?;
    let bpe = intrinsics::string(e, BPE, stage, 0, body)?;
    for element in selected(e.lower) {
        let root = intrinsics::typed_array_constructor_root(e.lower, element);
        body.extend([
            assign(proto, alloc(layout::KIND_OBJECT, shared, PROTO_ROWS, 0, 0, stage, 2), l),
            assign(ctor, alloc(layout::KIND_FUNCTION, abstract_ctor, CTOR_ROWS, element.call_code(), 3, permanent, root), l),
        ]);
        link(e, ctor, proto, 3.0, body)?;
        for (target, row) in [(ctor, 2), (proto, 1)] {
            intrinsics::trusted_data(
                e,
                var(target),
                row,
                var(bpe),
                value::number(E::Float(element.width() as f64)),
                bpe_attributes,
                body,
            )?;
        }
    }
    let base = e.integer()?;
    for (i, (name, code)) in GETTERS.into_iter().enumerate() {
        let key = if name == "length" {
            length
        } else {
            intrinsics::string(e, name, stage, 0, body)?
        };
        body.push(assign(
            getter,
            alloc(layout::KIND_FUNCTION, function_proto, 1, code, 0, stage, 4),
            l,
        ));
        intrinsics::trusted_data(
            e,
            var(getter),
            0,
            var(length),
            value::number(E::Float(0.0)),
            fixed,
            body,
        )?;
        body.push(assign(base, e.base(var(shared)), l));
        let row = layout::OBJECT_HEADER_CELLS + (i as i64 + 1) * layout::OBJECT_CELLS_PER_PROPERTY;
        for (offset, payload) in [
            (0, field(var(key), "ref_slot")),
            (1, field(var(key), "generation")),
            (2, int(8 + 4)),
            (3, field(var(getter), "ref_slot")),
            (4, field(var(getter), "generation")),
            (5, int(0)),
            (6, int(0)),
        ] {
            body.push(e.write(add(var(base), int(row + offset)), payload)?);
        }
        // Commit each complete accessor before reusing its temporary roots.
        body.push(e.write(
            add(var(base), int(2)),
            pack_header(int(i as i64 + 2), E::Bool(true), E::Bool(false)),
        )?);
    }
    for (i, (name, code, arity)) in METHODS.into_iter().enumerate() {
        let key = intrinsics::string(e, name, stage, 0, body)?;
        body.push(assign(
            getter,
            alloc(layout::KIND_FUNCTION, function_proto, 1, code, arity, stage, 4),
            l,
        ));
        intrinsics::trusted_data(
            e, var(getter), 0, var(length), value::number(E::Float(arity as f64)), fixed, body,
        )?;
        intrinsics::trusted_data(
            e, var(shared), (1 + GETTERS.len() + i) as i64, var(key), var(getter),
            (true, true, true), body,
        )?;
    }
    Ok(())
}

pub(super) fn backing(e: &Emit<'_, '_>, base: E) -> E {
    value::dynamic_value(
        int(value::OBJECT),
        E::Float(0.0),
        E::Bool(false),
        e.decoded(add(base.clone(), int(layout::VIEW_BUFFER))),
        e.generation(add(base, int(layout::VIEW_BUFFER + 1))),
    )
}

pub(super) fn fits(offset: E, length: E, capacity: E) -> E {
    and(
        and(
            not(lt(offset.clone(), int(0))),
            not(lt(length.clone(), int(0))),
        ),
        and(
            not(lt(capacity.clone(), offset.clone())),
            not(lt(sub(capacity, offset), length)),
        ),
    )
}

pub(super) fn checked_byte_length(
    e: &mut Emit<'_, '_>,
    base: u16,
    body: &mut Vec<S>,
) -> Result<u16, ParseError> {
    let l = e.line;
    let length = e.integer()?;
    body.extend([
        assert_stmt(eq(e.decoded(var(base)), int(layout::KIND_BYTES)), l),
        assign(
            length,
            e.decoded(add(var(base), int(layout::BYTES_LENGTH_OFFSET))),
            l,
        ),
        assert_stmt(fits(int(0), var(length), int(layout::BYTES_MAX_LENGTH)), l),
        assert_stmt(
            not(lt(
                e.decoded(sub(var(base), int(layout::RAW_HEADER_CELLS))),
                add(
                    int(layout::RAW_HEADER_CELLS + layout::BYTES_HEADER_CELLS),
                    var(length),
                ),
            )),
            l,
        ),
    ]);
    Ok(length)
}

fn remainder(a: E, b: E) -> E {
    E::Mod(Box::new(a), Box::new(b))
}

fn divide(a: E, b: E) -> E {
    E::Div(Box::new(a), Box::new(b))
}

fn aligned(offset: E, width: E) -> E {
    eq(remainder(offset, width), int(0))
}

fn cell(e: &Emit<'_, '_>, base: u16, offset: i64) -> E {
    e.decoded(add(var(base), int(offset)))
}

struct Window {
    bytes: u16,
    offset: u16,
    length: u16,
    element: u16,
    width: u16,
}

fn checked_window(e: &mut Emit<'_, '_>, base: u16, body: &mut Vec<S>) -> Result<Window, ParseError> {
    let l = e.line;
    let window = Window {
        bytes: e.integer()?, offset: e.integer()?, length: e.integer()?,
        element: e.integer()?, width: e.integer()?,
    };
    let Window { bytes, offset, length, element, width } = window;
    body.extend([
        assert_stmt(eq(e.decoded(var(base)), int(layout::KIND_VIEW)), l),
        assert_stmt(not(lt(e.decoded(sub(var(base), int(layout::RAW_HEADER_CELLS))),
            int(layout::RAW_HEADER_CELLS + layout::VIEW_CELLS))), l),
        assign(element, cell(e, base, layout::VIEW_ELEMENT), l),
        assign(width, cell(e, base, layout::VIEW_WIDTH), l),
    ]);
    let element_width = selected(e.lower).map(|kind| {
        and(eq(var(element), int(kind as i64)), eq(var(width), int(kind.width())))
    }).reduce(or).expect("typed array elements selected");
    body.extend([
        assert_stmt(element_width, l),
        assign(bytes, e.base(backing(e, var(base))), l),
        assign(offset, cell(e, base, layout::VIEW_OFFSET), l),
        assign(length, cell(e, base, layout::VIEW_LENGTH), l),
    ]);
    let capacity = checked_byte_length(e, bytes, body)?;
    // Bound the element count before multiplying, even for a corrupt payload.
    body.extend([
        assert_stmt(fits(var(offset), var(length), var(capacity)), l),
        assert_stmt(aligned(var(offset), var(width)), l),
        assert_stmt(fits(var(offset), mul(var(length), var(width)), var(capacity)), l),
    ]);
    Ok(window)
}

pub(super) fn check_window(e: &mut Emit<'_, '_>, base: u16, body: &mut Vec<S>) -> Result<(), ParseError> {
    checked_window(e, base, body).map(|_| ())
}

fn checked_element(
    e: &mut Emit<'_, '_>, base: u16, index: u16, body: &mut Vec<S>,
) -> Result<(Window, u16), ParseError> {
    let l = e.line;
    let window = checked_window(e, base, body)?;
    let address = e.integer()?;
    body.extend([
        assert_stmt(and(not(lt(var(index), int(0))), lt(var(index), var(window.length))), l),
        assign(address, add(var(window.bytes), add(int(layout::BYTES_HEADER_CELLS),
            add(var(window.offset), mul(var(index), var(window.width))))), l),
    ]);
    Ok((window, address))
}

/// Decode the shared backing in little-endian byte order.
pub(super) fn read_element(
    e: &mut Emit<'_, '_>, base: u16, index: u16, body: &mut Vec<S>,
) -> Result<E, ParseError> {
    let l = e.line;
    let (window, address) = checked_element(e, base, index, body)?;
    let n = e.temp(T::Number, E::Float(0.0))?;
    let i = e.integer()?;
    let mut decode = vec![
        assign(n, E::Float(0.0), l),
        assign(i, var(window.width), l),
        S::While {
            condition: lt(int(0), var(i)),
            body: vec![
                assign(i, sub(var(i), int(1)), l),
                assign(n, add(mul(var(n), E::Float(256.0)), e.read(add(var(address), var(i)))), l),
            ],
            line: l,
        },
    ];
    for kind in selected(e.lower).filter(|kind| matches!(kind, Element::Int8 | Element::Int16 | Element::Int32)) {
        let modulus = (1_u64 << (8 * kind.width())) as f64;
        decode.push(branch(and(eq(var(window.element), int(kind as i64)),
            not(lt(var(n), E::Float(modulus / 2.0)))),
            vec![assign(n, sub(var(n), E::Float(modulus)), l)], Vec::new(), l));
    }
    for format in float::Format::ALL {
        if e.lower.typed_array_elements.get() & format.kind.bit() != 0 {
            decode = vec![branch(eq(var(window.element), int(format.kind as i64)),
                format.read(e, address, n)?, decode, l)];
        }
    }
    body.extend(decode);
    Ok(num(var(n)))
}

pub(super) fn write_element(
    e: &mut Emit<'_, '_>, base: u16, index: u16, n: u16, body: &mut Vec<S>,
) -> Result<(), ParseError> {
    let l = e.line;
    let (window, address) = checked_element(e, base, index, body)?;
    let mut encode = write_integer(e, &window, address, n)?;
    for format in float::Format::ALL {
        if e.lower.typed_array_elements.get() & format.kind.bit() != 0 {
            encode = vec![branch(eq(var(window.element), int(format.kind as i64)),
                format.write(e, address, n)?, encode, l)];
        }
    }
    body.extend(encode);
    Ok(())
}

fn write_integer(e: &mut Emit<'_, '_>, window: &Window, address: u16, n: u16) -> Result<Vec<S>, ParseError> {
    let l = e.line;
    let mut body = Vec::new();
    let modulus = E::Float(4_294_967_296.0);
    let truncate = |x| num(call(B::MathTrunc.call_index(), vec![num(x)]));
    // Low bytes of ToUint32 give the same modular conversions for all widths.
    let finite = and(lt(E::Float(f64::NEG_INFINITY), var(n)), lt(var(n), E::Float(f64::INFINITY)));
    let wrap = vec![
        branch(finite, vec![assign(n, remainder(truncate(var(n)), modulus.clone()), l)],
            vec![assign(n, E::Float(0.0), l)], l),
        branch(lt(var(n), E::Float(0.0)), vec![assign(n, add(var(n), modulus), l)], Vec::new(), l),
    ];
    if e.lower.typed_array_elements.get() & Element::Uint8Clamped.bit() != 0 {
        let floor = e.temp(T::Number, E::Float(0.0))?;
        let fraction = e.temp(T::Number, E::Float(0.0))?;
        let round = vec![
            assign(floor, truncate(var(n)), l),
            assign(fraction, sub(var(n), var(floor)), l),
            branch(or(lt(E::Float(0.5), var(fraction)), and(eq(var(fraction), E::Float(0.5)),
                eq(remainder(var(floor), E::Float(2.0)), E::Float(1.0)))),
                vec![assign(n, add(var(floor), E::Float(1.0)), l)],
                vec![assign(n, var(floor), l)], l),
        ];
        let clamp = vec![branch(not(lt(E::Float(0.0), var(n))), vec![assign(n, E::Float(0.0), l)],
            vec![branch(not(lt(var(n), E::Float(255.0))), vec![assign(n, E::Float(255.0), l)], round, l)], l)];
        body.push(branch(eq(var(window.element), int(Element::Uint8Clamped as i64)), clamp, wrap, l));
    } else {
        body.extend(wrap);
    }
    // Canonicalize -0 before extracting/storing the low byte.
    body.push(branch(eq(var(n), E::Float(0.0)), vec![assign(n, E::Float(0.0), l)], Vec::new(), l));
    let i = e.integer()?;
    body.extend([
        assign(i, int(0), l),
        S::While {
            condition: lt(var(i), var(window.width)),
            body: vec![
                e.write(add(var(address), var(i)), remainder(var(n), E::Float(256.0)))?,
                assign(n, truncate(divide(var(n), E::Float(256.0))), l),
                assign(i, add(var(i), int(1)), l),
            ],
            line: l,
        },
    ]);
    Ok(body)
}

pub(super) fn in_range(e: &Emit<'_, '_>, base: u16, index: u16) -> E {
    and(
        not(lt(var(index), int(0))),
        lt(
            var(index),
            e.decoded(add(var(base), int(layout::VIEW_LENGTH))),
        ),
    )
}

/// Number projection after the managed Number-hint DefaultValue operation.
pub(super) fn number(e: &Emit<'_, '_>, parser: u16, primitive: u16) -> E {
    // Undefined rounds to zero on the integer and ToIndex paths, but a
    // floating store must distinguish it (NaN) from null (+0).
    let nullish = if e.lower.typed_array_elements.get() & (Element::Float32.bit() | Element::Float64.bit()) != 0 {
        choose(tag_is(var(primitive), value::UNDEFINED), E::Float(f64::NAN), E::Float(0.0))
    } else {
        E::Float(0.0)
    };
    choose(
        tag_is(var(primitive), value::NUMBER),
        num(field(var(primitive), "number")),
        choose(
            tag_is(var(primitive), value::BOOLEAN),
            choose(field(var(primitive), "flag"), E::Float(1.0), E::Float(0.0)),
            choose(
                tag_is(var(primitive), value::STRING),
                call(
                    parser,
                    vec![var(e.carrier), var(primitive), int(0), E::Float(0.0)],
                ),
                nullish,
            ),
        ),
    )
}

pub(super) fn error(
    e: &mut Emit<'_, '_>,
    message: &str,
    range: bool,
    out: u16,
) -> Result<Vec<S>, ParseError> {
    let l = e.line;
    let c = e.carrier;
    let heap = e.lower.heap_ref(e.span)?.helpers;
    let intrinsic = e.lower.intrinsics.expect("intrinsics");
    let frame = e.integer()?;
    let mut body = vec![assign(
        frame,
        call(heap.root_enter, vec![var(c), int(3)]),
        l,
    )];
    let key = intrinsics::string(e, "message", frame, 0, &mut body)?;
    let text = intrinsics::string(e, message, frame, 1, &mut body)?;
    let alloc = if range {
        call(
            intrinsic.range_error.expect("view RangeError"),
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
    body.push(assign(out, alloc, l));
    intrinsics::trusted_data(
        e,
        var(out),
        0,
        var(key),
        var(text),
        (true, false, true),
        &mut body,
    )?;
    body.push(e.effect(heap.root_leave, vec![var(c), var(frame)])?);
    Ok(body)
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
    let intr = lower.intrinsics.expect("intrinsics");
    let numeric = lower.objects.expect("object runtime").numeric_default_value;
    let parser = lower
        .published_number_parser()
        .expect("object realm parser");
    let elements: Vec<_> = selected(lower).collect();
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
    let ret = |v| assign(out, value::result_ok(value::completion_return(v), sp), l);
    let thrown = |v| assign(out, value::result_err(v, sp), l);
    let err = e.boxed()?;
    let failure = |e: &mut Emit<'_, '_>, message, range| -> Result<Vec<S>, ParseError> {
        let mut stmts = error(e, message, range, err)?;
        stmts.push(thrown(var(err)));
        Ok(stmts)
    };
    let unsupported = failure(&mut e, "unsupported object class", false)?;
    let no_new = failure(&mut e, "must be called with new", false)?;
    let invalid_index = failure(&mut e, "invalid array index", true)?;
    let invalid_offset = failure(&mut e, "invalid offset", true)?;
    let invalid_length = failure(&mut e, "invalid length", true)?;
    let base = e.integer()?;
    let get = vec![branch(
        tag_is(var(receiver), value::OBJECT),
        vec![
            assign(base, e.base(var(receiver)), l),
            branch(
                eq(e.decoded(var(base)), int(layout::KIND_VIEW)),
                vec![branch(
                    eq(var(mode), int(GET_BUFFER)),
                    vec![ret(backing(&e, var(base)))],
                    vec![branch(
                        eq(var(mode), int(GET_BYTE_OFFSET)),
                        vec![ret(value::number(
                            e.read(add(var(base), int(layout::VIEW_OFFSET))),
                        ))],
                        vec![branch(
                            eq(var(mode), int(GET_BYTE_LENGTH)),
                            vec![ret(value::number(mul(
                                e.read(add(var(base), int(layout::VIEW_LENGTH))),
                                e.read(add(var(base), int(layout::VIEW_WIDTH))),
                            )))],
                            vec![ret(value::number(e.read(add(var(base), int(layout::VIEW_LENGTH)))))],
                            l,
                        )],
                        l,
                    )],
                    l,
                )],
                unsupported.clone(),
                l,
            ),
        ],
        unsupported.clone(),
        l,
    )];

    let (frame, argc, offset, len, which) = (
        e.integer()?,
        e.integer()?,
        e.integer()?,
        e.integer()?,
        e.integer()?,
    );
    let (element, width, root) = (e.integer()?, e.integer()?, e.integer()?);
    let (buffer, raw, primitive, proto, view) =
        (e.boxed()?, e.boxed()?, e.boxed()?, e.boxed()?, e.boxed()?);
    let failed = e.boolean()?;
    let n = e.temp(T::Number, E::Float(0.0))?;
    let converted = e.temp(
        value::value_result_schema(),
        result(value::value_result_schema(), true, value::undefined(), sp),
    )?;
    let alloc = e.temp(
        value::allocation_schema(),
        value::allocation(int(0), int(0), int(0)),
    )?;
    let at = |i| call(B::Get.call_index(), vec![E::Borrow(Box::new(var(args))), i]);
    // Caller roots every actual. Save only the checked byte length across
    // user conversion; the backing address is not needed after this read.
    let mut build = Vec::new();
    let capacity = checked_byte_length(&mut e, base, &mut build)?;
    let remaining = e.integer()?;
    let derive_length = vec![
        assign(remaining, sub(var(capacity), var(offset)), l),
        branch(aligned(var(remaining), var(width)),
            vec![assign(len, call(e.decode, vec![num(divide(var(remaining), var(width)))]), l)],
            Vec::new(), l),
    ];
    let mut make = vec![branch(lt(var(len), int(0)), derive_length, Vec::new(), l)];
    let mut publish = vec![
        assign(base, e.base(call(intr.root, vec![var(c), var(root)])), l),
        assign(
            proto,
            value::dynamic_value(
                int(value::OBJECT),
                E::Float(0.0),
                E::Bool(false),
                e.decoded(add(
                    var(base),
                    int(layout::FUNCTION_CELLS + layout::OBJECT_CELLS_PER_PROPERTY + 5),
                )),
                e.generation(add(
                    var(base),
                    int(layout::FUNCTION_CELLS + layout::OBJECT_CELLS_PER_PROPERTY + 6),
                )),
            ),
            l,
        ),
        assign(
            alloc,
            call(
                heap.node_begin,
                vec![var(c), int(layout::VIEW_CELLS), int(layout::KIND_VIEW)],
            ),
            l,
        ),
        assign(base, field(var(alloc), "base"), l),
    ];
    for (off, cell) in [
        (2, int(0)),
        (3, field(var(proto), "ref_slot")),
        (4, field(var(proto), "generation")),
        (layout::VIEW_BUFFER, field(var(buffer), "ref_slot")),
        (layout::VIEW_BUFFER + 1, field(var(buffer), "generation")),
        (layout::VIEW_OFFSET, var(offset)),
        (layout::VIEW_LENGTH, var(len)),
        (layout::VIEW_ELEMENT, var(element)),
        (layout::VIEW_WIDTH, var(width)),
    ] {
        publish.push(e.write(add(var(base), int(off)), cell)?);
    }
    publish.push(assign(
        view,
        call(
            heap.node_publish,
            vec![
                var(c),
                field(var(alloc), "slot"),
                field(var(alloc), "generation"),
                var(base),
                var(frame),
                int(0),
            ],
        ),
        l,
    ));
    publish.push(ret(var(view)));
    make.push(branch(
        fits(var(offset), mul(var(len), var(width)), var(capacity)),
        publish,
        invalid_length,
        l,
    ));
    let mut bad_index = invalid_index.clone();
    bad_index.push(assign(failed, E::Bool(true), l));
    let mut bad_offset = invalid_offset;
    bad_offset.push(assign(failed, E::Bool(true), l));
    // Match MicroQuickJS's JS_ToIndex limit before applying the buffer bounds.
    let success = vec![
        assign(n, number(&e, parser, primitive), l),
        assign(
            n,
            choose(
                eq(var(n), var(n)),
                num(call(B::MathTrunc.call_index(), vec![num(var(n))])),
                E::Float(0.0),
            ),
            l,
        ),
        branch(
            or(
                lt(var(n), E::Float(0.0)),
                lt(E::Float(((1_i64 << 30) - 1) as f64), var(n)),
            ),
            bad_index,
            vec![
                branch(
                    lt(E::Float(layout::BYTES_MAX_LENGTH as f64), var(n)),
                    vec![assign(
                        n,
                        E::Float((layout::BYTES_MAX_LENGTH + 1) as f64),
                        l,
                    )],
                    Vec::new(),
                    l,
                ),
                branch(
                    eq(var(which), int(1)),
                    vec![
                        assign(offset, call(e.decode, vec![num(var(n))]), l),
                        // Reject the offset before the next iteration can convert length.
                        branch(or(lt(var(capacity), var(offset)), not(aligned(var(offset), var(width)))),
                            bad_offset, Vec::new(), l),
                    ],
                    vec![assign(len, call(e.decode, vec![num(var(n))]), l)],
                    l,
                ),
            ],
            l,
        ),
    ];
    let convert = vec![
        assign(
            converted,
            call(numeric, vec![var(c), var(raw), E::Bool(false)]),
            l,
        ),
        matched(
            &mut e,
            converted,
            value::value_result_schema(),
            primitive,
            err,
            success,
            vec![thrown(var(err)), assign(failed, E::Bool(true), l)],
        )?,
    ];
    // --- Owned-length `new Uint8Array(length)` ---------------------------
    // The first actual is converted exactly once, after every actual
    // expression already evaluated in JS order. Trailing actuals are never
    // read by this path, so their valueOf/toString hooks never run.
    //
    // Order is load-bearing: the guest index domain is decided before the
    // host byte quota, so 1073741824 is the catchable RangeError while 32769
    // is the fatal host error. Both decisions happen before any root frame
    // or node is reserved.
    let quota_sink = e.temp(T::Null, E::Null)?;
    let quota_failure = match quota_error_host {
        Some(adapter) => vec![assign(
            quota_sink,
            call(
                adapter,
                vec![E::String(
                    "array buffer length exceeds host quota".to_owned(),
                )],
            ),
            l,
        )],
        // No reachable owned-length site can exceed the budget; keep the
        // branch a fatal trap rather than degrading it to a guest RangeError.
        None => vec![assert_stmt(E::Bool(false), l)],
    };
    let mut owned = vec![
        assign(n, number(&e, parser, buffer), l),
        // JS_ToInt32Sat truncates toward zero and saturates; NaN becomes +0.
        // `-0.9` therefore truncates to -0 and passes the non-negative test.
        assign(
            n,
            choose(
                eq(var(n), var(n)),
                num(call(B::MathTrunc.call_index(), vec![num(var(n))])),
                E::Float(0.0),
            ),
            l,
        ),
    ];
    let mut allocate = Vec::new();
    {
        // The ArrayBuffer prototype is permanently reachable from the
        // constructor's immutable `prototype` row; read it as a slot/generation
        // handle so a collection during the backing reservation cannot stale it.
        allocate.push(assign(
            frame,
            call(heap.root_enter, vec![var(c), int(2)]),
            l,
        ));
        allocate.push(assign(
            proto,
            call(
                intr.root,
                vec![
                    var(c),
                    int(intrinsics::array_buffer_constructor_root(e.lower)),
                ],
            ),
            l,
        ));
        allocate.push(assign(base, e.base(var(proto)), l));
        allocate.push(assign(
            proto,
            value::dynamic_value(
                int(value::OBJECT),
                E::Float(0.0),
                E::Bool(false),
                e.decoded(add(
                    var(base),
                    int(layout::FUNCTION_CELLS + layout::OBJECT_CELLS_PER_PROPERTY + 5),
                )),
                e.generation(add(
                    var(base),
                    int(layout::FUNCTION_CELLS + layout::OBJECT_CELLS_PER_PROPERTY + 6),
                )),
            ),
            l,
        ));
        let bytes = super::array_buffer::emit_bytes_allocation(
            &mut e, len, proto, frame, 0, &mut allocate,
        )?;
        // The publication committed the backing into frame[0]: it is rooted
        // before the view reservation can collect.
        allocate.push(assign(buffer, var(bytes), l));
        // The concrete Uint8Array prototype is immutable on its constructor.
        allocate.push(assign(
            base,
            e.base(call(intr.root, vec![var(c), var(root)])),
            l,
        ));
        allocate.push(assign(
            proto,
            value::dynamic_value(
                int(value::OBJECT),
                E::Float(0.0),
                E::Bool(false),
                e.decoded(add(
                    var(base),
                    int(layout::FUNCTION_CELLS + layout::OBJECT_CELLS_PER_PROPERTY + 5),
                )),
                e.generation(add(
                    var(base),
                    int(layout::FUNCTION_CELLS + layout::OBJECT_CELLS_PER_PROPERTY + 6),
                )),
            ),
            l,
        ));
        // Reservation may collect. The backing slot/generation are read back
        // from the rooted publication handle, never from a stale base local.
        allocate.push(assign(
            alloc,
            call(
                heap.node_begin,
                vec![var(c), int(layout::VIEW_CELLS), int(layout::KIND_VIEW)],
            ),
            l,
        ));
        allocate.push(assign(base, field(var(alloc), "base"), l));
        for (off, cell) in [
            (2, int(0)),
            (3, field(var(proto), "ref_slot")),
            (4, field(var(proto), "generation")),
            (layout::VIEW_BUFFER, field(var(buffer), "ref_slot")),
            (layout::VIEW_BUFFER + 1, field(var(buffer), "generation")),
            (layout::VIEW_OFFSET, int(0)),
            (layout::VIEW_LENGTH, var(len)),
            (layout::VIEW_ELEMENT, int(Element::Uint8 as i64)),
            (layout::VIEW_WIDTH, int(1)),
        ] {
            allocate.push(e.write(add(var(base), int(off)), cell)?);
        }
        allocate.push(assign(
            view,
            call(
                heap.node_publish,
                vec![
                    var(c),
                    field(var(alloc), "slot"),
                    field(var(alloc), "generation"),
                    var(base),
                    var(frame),
                    int(1),
                ],
            ),
            l,
        ));
        allocate.push(ret(var(view)));
        allocate.push(e.effect(heap.root_leave, vec![var(c), var(frame)])?);
    }
    owned.push(branch(
        or(
            lt(var(n), E::Float(0.0)),
            lt(E::Float(((1_i64 << 30) - 1) as f64), var(n)),
        ),
        invalid_index.clone(),
        vec![branch(
            lt(E::Float(layout::BYTES_MAX_LENGTH as f64), var(n)),
            quota_failure,
            {
                let mut success = vec![assign(len, call(e.decode, vec![num(var(n))]), l)];
                success.extend(allocate);
                success
            },
            l,
        )],
        l,
    ));
    build.extend([
        assign(frame, call(heap.root_enter, vec![var(c), int(1)]), l),
        assign(len, int(-1), l),
        assign(which, int(1), l),
        S::While {
            condition: and(
                not(var(failed)),
                and(lt(var(which), var(argc)), lt(var(which), int(3))),
            ),
            body: vec![
                assign(raw, at(var(which)), l),
                branch(
                    not(tag_is(var(raw), value::UNDEFINED)),
                    convert,
                    Vec::new(),
                    l,
                ),
                assign(which, add(var(which), int(1)), l),
            ],
            line: l,
        },
        branch(not(var(failed)), make, Vec::new(), l),
    ]);
    build.push(e.effect(heap.root_leave, vec![var(c), var(frame)])?);
    let mut construct = Vec::new();
    // Only emit constructor roots for identifiers selected by the program.
    for element_kind in elements {
        construct.push(branch(eq(var(mode), int(element_kind.construct_mode())), vec![
            assign(element, int(element_kind as i64), l),
            assign(width, int(element_kind.width()), l),
            assign(root, int(intrinsics::typed_array_constructor_root(e.lower, element_kind)), l),
        ], Vec::new(), l));
    }
    construct.push(assert_stmt(lt(int(0), var(width)), l));
    construct.extend([
        assign(
            argc,
            call(B::Len.call_index(), vec![E::Borrow(Box::new(var(args)))]),
            l,
        ),
        assign(buffer, value::undefined(), l),
        branch(
            lt(int(0), var(argc)),
            vec![assign(buffer, at(int(0)), l)],
            Vec::new(),
            l,
        ),
    ]);
    // A non-object first actual is the owned-length form. An object stays on
    // the existing shared-window path and still answers the guest TypeError
    // for every class other than the real kind-7 backing.
    construct.push(branch(
        and(
            eq(var(element), int(Element::Uint8 as i64)),
            not(tag_is(var(buffer), value::OBJECT)),
        ),
        owned,
        vec![branch(
            and(lt(int(0), var(argc)), lt(var(argc), int(4))),
            vec![branch(
                tag_is(var(buffer), value::OBJECT),
                vec![
                    assign(base, e.base(var(buffer)), l),
                    branch(
                        eq(e.decoded(var(base)), int(layout::KIND_BYTES)),
                        build,
                        unsupported.clone(),
                        l,
                    ),
                ],
                unsupported.clone(),
                l,
            )],
            unsupported.clone(),
            l,
        )],
        l,
    ));
    let methods = methods::emit(&mut e, mode, receiver, args, out)?;
    let body = vec![branch(
        not(lt(int(0), var(mode))),
        construct,
        vec![branch(
            or(eq(var(mode), int(CALL)), and(lt(int(GET_BUFFER), var(mode)),
                not(lt(int(LAST_CALL), var(mode))))),
            no_new,
            vec![branch(eq(var(mode), int(ABSTRACT)), unsupported,
                vec![branch(lt(var(mode), int(SUBARRAY)), get, methods, l)], l)],
            l,
        )],
        l,
    )];
    declare(
        ir,
        stmts,
        "__compat_es5_typed_array",
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
