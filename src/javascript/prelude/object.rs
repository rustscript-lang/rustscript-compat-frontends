//! Shared compact ordinary-object metadata and descriptor emitters.
//! All decoders validate exact numeric domains before inspecting the union.

use super::super::{
    heap as layout,
    lowerer::{Lowerer, assert_stmt},
    representation as value,
};
use super::heap::declare_helper;
use super::values::read_string_unit;
use vm::{
    BuiltinFunction as B, Expr as E, FrontendIr, ParseError, Stmt as S, TypeSchema as T, ValueType,
};

pub(crate) fn call(i: u16, args: Vec<E>) -> E {
    E::Call(i, Vec::new(), args, None, None)
}
pub(crate) fn int(n: i64) -> E {
    E::Int(n)
}
pub(crate) fn var(n: u16) -> E {
    E::Var(n)
}
pub(crate) fn add(a: E, b: E) -> E {
    E::Add(Box::new(a), Box::new(b))
}
pub(crate) fn sub(a: E, b: E) -> E {
    E::Sub(Box::new(a), Box::new(b))
}
pub(crate) fn mul(a: E, b: E) -> E {
    E::Mul(Box::new(a), Box::new(b))
}
pub(crate) fn eq(a: E, b: E) -> E {
    E::Eq(Box::new(a), Box::new(b))
}
pub(crate) fn lt(a: E, b: E) -> E {
    E::Lt(Box::new(a), Box::new(b))
}
/// Invert a proven Boolean once. Generic record Get and loop joins retain
/// Tagged in SSA even for Bool locals; conditional branches validate/unbox
/// that runtime Bool, while the Not opcode requires an unboxed SSA Bool.
pub(crate) fn not(a: E) -> E {
    choose(a, E::Bool(false), E::Bool(true))
}
pub(crate) fn and(a: E, b: E) -> E {
    E::And(Box::new(a), Box::new(b))
}
pub(crate) fn or(a: E, b: E) -> E {
    E::Or(Box::new(a), Box::new(b))
}
pub(crate) fn choose(c: E, a: E, b: E) -> E {
    E::IfElse {
        condition: Box::new(c),
        then_expr: Box::new(a),
        else_expr: Box::new(b),
    }
}
pub(crate) fn field(e: E, k: &str) -> E {
    let e = match e {
        E::Var(_) => E::Borrow(Box::new(e)),
        other => other,
    };
    value::field_of(e, k)
}
pub(crate) fn num(e: E) -> E {
    Lowerer::as_number(e)
}
/// Integer tag codes live in the value record, but generic Get is Tagged.
/// Native SSA rejects Ceq of that Get against an Int literal unless both
/// sides are one numeric domain. `num` is the existing pure projection.
pub(crate) fn tag_is(record: E, tag: i64) -> E {
    eq(num(field(record, "tag")), num(int(tag)))
}
pub(crate) fn record(fields: Vec<(&str, E)>) -> E {
    fields
        .into_iter()
        .fold(call(B::MapNew.call_index(), vec![]), |r, (k, v)| {
            call(B::Set.call_index(), vec![r, E::String(k.into()), v])
        })
}
pub(crate) fn assign(index: u16, expr: E, line: u32) -> S {
    S::Assign {
        kind: vm::AssignmentKind::Set,
        index,
        expr,
        line,
    }
}
pub(crate) fn branch(condition: E, then_branch: Vec<S>, else_branch: Vec<S>, line: u32) -> S {
    S::IfElse {
        condition,
        then_branch,
        else_branch,
        line,
    }
}

/// Temporaries dominate sibling arms and loop bodies. They are declared at
/// helper entry, never in a repeatedly executed IR branch.
pub(crate) struct Emit<'a, 'b> {
    pub lower: &'a mut Lowerer<'b>,
    pub span: swc_common::Span,
    pub line: u32,
    pub declarations: Vec<S>,
    pub carrier: u16,
    pub read: u16,
    pub write: u16,
    pub decode: u16,
    pub checked_base: u16,
    pub decode_u32: u16,
    pub(super) number_write_sink: Option<u16>,
    pub(super) bootstrap_row_suffix: Option<u16>,
    pub(super) bootstrap_ascii_init: Option<u16>,
}
impl<'a, 'b> Emit<'a, 'b> {
    pub fn new(
        lower: &'a mut Lowerer<'b>,
        span: swc_common::Span,
        carrier: u16,
    ) -> Result<Self, ParseError> {
        let heap = lower.heap_ref(span)?;
        let (read, write, checked_base, decode_u32) = (
            heap.helper_read,
            heap.helper_write,
            heap.helpers.checked_base,
            heap.helpers.decode_u32,
        );
        let decode = lower
            .helper_decode
            .ok_or_else(|| lower.error(span, "missing exact decoder"))?;
        let line = lower.line(span);
        Ok(Self {
            lower,
            span,
            line,
            declarations: Vec::new(),
            carrier,
            read,
            write,
            decode,
            checked_base,
            decode_u32,
            number_write_sink: None,
            bootstrap_row_suffix: None,
            bootstrap_ascii_init: None,
        })
    }
    pub fn effect(&mut self, index: u16, args: Vec<E>) -> Result<S, ParseError> {
        let sink = self.integer()?;
        Ok(assign(sink, call(index, args), self.line))
    }
    pub fn root(&mut self, frame: E, entry: E, payload: E) -> Result<S, ParseError> {
        let write = self.lower.heap_ref(self.span)?.helpers.root_write;
        self.effect(write, vec![var(self.carrier), frame, entry, payload])
    }
    pub fn temp(&mut self, schema: T, initial: E) -> Result<u16, ParseError> {
        let index = self.lower.slot(self.span)?;
        self.declarations.push(S::Let {
            index,
            declared_schema: Some(schema),
            expr: initial,
            line: self.line,
        });
        Ok(index)
    }
    pub fn integer(&mut self) -> Result<u16, ParseError> {
        self.temp(T::Int, int(0))
    }
    pub fn boolean(&mut self) -> Result<u16, ParseError> {
        self.temp(T::Bool, E::Bool(false))
    }
    pub fn boxed(&mut self) -> Result<u16, ParseError> {
        self.temp(value::schema(), value::undefined())
    }
    pub fn read(&self, address: E) -> E {
        // `__compat_ref_read` takes `number`, which already accepts an integer
        // cell address. Dividing that address by 1.0 asks the interpreter for
        // a float and traps on ordinary-object rows.
        num(call(self.read, vec![var(self.carrier), address]))
    }
    pub fn decoded(&self, address: E) -> E {
        call(self.decode, vec![self.read(address)])
    }
    pub fn generation(&self, address: E) -> E {
        call(self.decode_u32, vec![self.read(address)])
    }
    pub fn base(&self, object: E) -> E {
        call(
            self.checked_base,
            vec![
                var(self.carrier),
                field(object.clone(), "ref_slot"),
                field(object, "generation"),
            ],
        )
    }
    pub fn write(&mut self, address: E, payload: E) -> Result<S, ParseError> {
        let sink = self.discarded_number_write_sink()?;
        Ok(assign(
            sink,
            call(self.write, vec![var(self.carrier), address, num(payload)]),
            self.line,
        ))
    }
    fn discarded_number_write_sink(&mut self) -> Result<u16, ParseError> {
        if let Some(slot) = self.number_write_sink {
            return Ok(slot);
        }
        let slot = self.temp(T::Number, E::Float(0.0))?;
        self.number_write_sink = Some(slot);
        Ok(slot)
    }
    pub fn finish(mut self, body: Vec<S>) -> Vec<S> {
        self.declarations.extend(body);
        self.declarations
    }
}

pub(crate) struct Header {
    pub kind: u16,
    pub count: u16,
    pub extensible: u16,
    pub error: u16,
    pub error_instance: u16,
    pub header: u16,
}
/// Header2 is exact17, count <=256, with no reserved flag combinations.
/// Actual occupied extent, including an unsplittable tail, bounds live rows.
pub(crate) fn decode_header(
    e: &mut Emit<'_, '_>,
    base: u16,
    body: &mut Vec<S>,
) -> Result<Header, ParseError> {
    decode_header_impl(e, base, body, None)
}

/// Collector-side variant: the gray drain already read and exact-decoded this
/// node's kind for the current queue entry. Reusing that validated Int local
/// removes one ref read plus one exact decode per popped Object/Function while
/// the Object/Function kind assertion and every later header/row check stay
/// exactly where they were. Other `decode_header` callers keep the original
/// read-and-decode path through `decode_header` above.
pub(crate) fn decode_header_known_kind(
    e: &mut Emit<'_, '_>,
    base: u16,
    body: &mut Vec<S>,
    known_kind: u16,
) -> Result<Header, ParseError> {
    decode_header_impl(e, base, body, Some(known_kind))
}

fn property_header(e: &mut Emit<'_, '_>, base: u16, body: &mut Vec<S>) -> Result<Header, ParseError> {
    if !(e.lower.array_buffer_used.get() || e.lower.typed_array_used()) {
        return decode_header(e, base, body);
    }
    let kind = e.integer()?;
    let l = e.line;
    body.push(assign(kind, e.decoded(var(base)), l));
    let mut ordinary = Vec::new();
    let header = decode_header_known_kind(e, base, &mut ordinary, kind)?;
    body.push(branch(or(eq(var(kind), int(layout::KIND_BYTES)), eq(var(kind), int(layout::KIND_VIEW))), vec![
        assert_stmt(eq(e.decoded(add(var(base), int(2))), int(0)), l),
        assign(header.count, int(0), l),
        assign(header.extensible, E::Bool(false), l),
        assign(header.error, E::Bool(false), l),
        assign(header.error_instance, E::Bool(false), l),
        assign(header.header, choose(eq(var(kind), int(layout::KIND_VIEW)),
            int(layout::VIEW_CELLS), int(layout::BYTES_HEADER_CELLS)), l),
    ], ordinary, l));
    Ok(header)
}

fn decode_header_impl(
    e: &mut Emit<'_, '_>,
    base: u16,
    body: &mut Vec<S>,
    known_kind: Option<u16>,
) -> Result<Header, ParseError> {
    let packed = e.integer()?;
    let count = e.integer()?;
    let ext = e.boolean()?;
    let error = e.boolean()?;
    let error_instance = e.boolean()?;
    let cells = e.integer()?;
    let header = e.integer()?;
    let kind = match known_kind {
        Some(kind) => kind,
        None => e.integer()?,
    };
    let l = e.line;
    if known_kind.is_none() {
        body.push(assign(kind, e.decoded(var(base)), l));
    }
    body.extend([
        assert_stmt(
            or(
                or(
                    eq(var(kind), int(layout::KIND_OBJECT)),
                    eq(var(kind), int(layout::KIND_FUNCTION)),
                ),
                eq(var(kind), int(layout::KIND_REGEXP)),
            ),
            l,
        ),
        assign(packed, e.decoded(add(var(base), int(2))), l),
        assert_stmt(
            not(lt(int(layout::OBJECT_MAX_PACKED_HEADER), var(packed))),
            l,
        ),
        assign(count, int(0), l),
        S::While {
            condition: not(lt(var(packed), int(layout::OBJECT_COUNT_FACTOR))),
            body: vec![
                assign(
                    packed,
                    sub(var(packed), int(layout::OBJECT_COUNT_FACTOR)),
                    l,
                ),
                assign(count, add(var(count), int(1)), l),
            ],
            line: l,
        },
        assign(error_instance, not(lt(var(packed), int(layout::OBJECT_ERROR_INSTANCE_FLAG))), l),
        assign(
            packed,
            choose(var(error_instance), sub(var(packed), int(layout::OBJECT_ERROR_INSTANCE_FLAG)), var(packed)),
            l,
        ),
        assign(error, not(lt(var(packed), int(layout::OBJECT_LEGACY_ERROR_FLAG))), l),
        assign(
            packed,
            choose(var(error), sub(var(packed), int(layout::OBJECT_LEGACY_ERROR_FLAG)), var(packed)),
            l,
        ),
        assign(ext, eq(var(packed), int(layout::OBJECT_EXTENSIBLE_FLAG)), l),
        assert_stmt(
            and(
                not(and(eq(var(kind), int(layout::KIND_FUNCTION)), var(error))),
                or(not(var(error_instance)), var(error)),
            ),
            l,
        ),
        assign(
            header,
            choose(
                eq(var(kind), int(layout::KIND_FUNCTION)),
                int(layout::FUNCTION_CELLS),
                choose(var(error_instance), int(layout::ERROR_HEADER_CELLS), int(layout::OBJECT_HEADER_CELLS)),
            ),
            l,
        ),
        assign(
            cells,
            e.decoded(sub(var(base), int(layout::RAW_HEADER_CELLS))),
            l,
        ),
        assert_stmt(
            not(lt(
                var(cells),
                add(
                    int(layout::RAW_HEADER_CELLS),
                    add(
                        var(header),
                        mul(int(layout::OBJECT_CELLS_PER_PROPERTY), var(count)),
                    ),
                ),
            )),
            l,
        ),
    ]);
    Ok(Header {
        kind,
        count,
        extensible: ext,
        error,
        error_instance,
        header,
    })
}

pub(crate) const DESCRIPTOR: &str = "__CompatEs5Descriptor";
pub(crate) fn descriptor_schema() -> T {
    T::Named(DESCRIPTOR.into(), Vec::new())
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn descriptor(present: E, value: E, get: E, set: E, w: E, en: E, c: E) -> E {
    record(vec![
        ("present", present),
        ("value", value),
        ("get", get),
        ("set", set),
        ("writable", w),
        ("enumerable", en),
        ("configurable", c),
    ])
}
pub(crate) fn empty_descriptor() -> E {
    descriptor(
        int(0),
        value::undefined(),
        value::undefined(),
        value::undefined(),
        E::Bool(false),
        E::Bool(false),
        E::Bool(false),
    )
}
pub(crate) fn data_descriptor(v: E, w: E, en: E, c: E) -> E {
    descriptor(int(51), v, value::undefined(), value::undefined(), w, en, c)
}
pub(crate) fn accessor_descriptor(g: E, s: E, en: E, c: E) -> E {
    descriptor(int(60), value::undefined(), g, s, E::Bool(false), en, c)
}
pub(crate) fn descriptor_declaration() -> vm::compiler::ir::StructDecl {
    use std::collections::HashMap;
    vm::compiler::ir::StructDecl {
        name: DESCRIPTOR.into(),
        type_params: Vec::new(),
        body_schema: T::Object(HashMap::from([
            ("present".into(), T::Int),
            ("value".into(), value::schema()),
            ("get".into(), value::schema()),
            ("set".into(), value::schema()),
            ("writable".into(), T::Bool),
            ("enumerable".into(), T::Bool),
            ("configurable".into(), T::Bool),
        ])),
        origin: vm::compiler::ir::StructDeclOrigin::Guest,
    }
}

/// Validated scalar outputs of one property row. The key base is snapshotted
/// before the union arms reuse the payload scratch locals below; a tracing
/// consumer additionally snapshots the key slot/generation pair through the
/// emitter's `key_snapshot` slots, so the key edge needs no second read or
/// decode of the same row cells. Union payload outputs are only meaningful
/// under their own arm discriminator (`accessor`, per-field generation):
/// data rows use `slot`/`generation`, accessor rows use `get_slot`/`get_gen`
/// plus the reused `slot`/`generation` for the setter. Arm-only outputs are
/// only ever read inside the arm that defines them, so no stale
/// previous-row value may be read.
#[derive(Clone, Copy)]
pub(crate) struct CheckedRow {
    pub key_base: u16,
    pub tag: u16,
    pub number: u16,
    pub flag: u16,
    pub slot: u16,
    pub generation: u16,
    pub get_slot: u16,
    pub get_gen: u16,
    pub writable: u16,
    pub enumerable: u16,
    pub configurable: u16,
    pub accessor: u16,
}

/// Compile-time optional last-key cache for the collector's row scan. The
/// four slots are ordinary Int/Bool temporaries the collector declares once
/// per collection: `valid` starts false, and `slot`/`generation`/`base` hold
/// the last key pair whose original `checked_base` and String-brand proof
/// succeeded during this same drain. Slot 0 is a legal key slot, so validity
/// is the dedicated Bool and never a zero sentinel. The ordinary descriptor
/// consumer passes `None` and keeps the unconditional validation.
#[derive(Clone, Copy)]
pub(crate) struct CheckedKeyCache {
    pub valid: u16,
    pub slot: u16,
    pub generation: u16,
    pub base: u16,
}

/// Validate one property row and emit only its checked scalars: live String
/// key identity, exact metadata/tag/accessor legality, then the matching
/// data/inactive-cell or getter/setter cell checks with full live-state,
/// generation and brand validation. When the caller supplies dedicated
/// `key_snapshot` (slot, generation) temps, the validated key pair is
/// preserved into them before the union arms reuse `s`/`g` for payload work.
/// When the caller supplies a collector-local `key_cache`, the two original
/// numeric key decoders still run on every row first and only the repeated
/// `checked_base`/String-brand proof may be replaced by a hit; a miss runs
/// that proof unchanged and records the pair only after it succeeds.
/// The caller-supplied `on_accessor`/`on_data` tails are spliced at the end
/// of the matching validation arm, so every consumer read stays inside the
/// arm that defines it: no arm-only scratch is ever named at top level and
/// no per-row initialization needs to span the validation branch. No record
/// is constructed here; the ordinary descriptor consumer and the collector
/// tracing consumer below share exactly these checks in exactly this order.
pub(crate) fn emit_checked_row(
    e: &mut Emit<'_, '_>,
    row: u16,
    body: &mut Vec<S>,
    key_snapshot: Option<(u16, u16)>,
    key_cache: Option<CheckedKeyCache>,
    on_accessor: impl FnOnce(&CheckedRow) -> Vec<S>,
    on_data: impl FnOnce(&CheckedRow) -> Vec<S>,
) -> Result<CheckedRow, ParseError> {
    let l = e.line;
    let meta = e.integer()?;
    let tag = e.integer()?;
    let bits = e.integer()?;
    let w = e.boolean()?;
    let en = e.boolean()?;
    let c = e.boolean()?;
    let access = e.boolean()?;
    let n = e.temp(T::Float, E::Float(0.0))?;
    let flag = e.integer()?;
    let s = e.integer()?;
    let g = e.integer()?;
    let gs = e.integer()?;
    let gg = e.integer()?;
    let checked = e.integer()?;
    let key_base = e.integer()?;
    let data_meta = and(not(var(access)), not(lt(int(value::OBJECT), var(tag))));
    let accessor_bits = or(
        or(eq(var(bits), int(8)), eq(var(bits), int(10))),
        or(eq(var(bits), int(12)), eq(var(bits), int(14))),
    );
    let accessor_meta = and(eq(var(tag), int(0)), accessor_bits);
    // Scalar index map for the consumer tails spliced into the arms below.
    let scalars = CheckedRow {
        key_base,
        tag,
        number: n,
        flag,
        slot: s,
        generation: g,
        get_slot: gs,
        get_gen: gg,
        writable: w,
        enumerable: en,
        configurable: c,
        accessor: access,
    };
    // Every row always runs the two original numeric key decoders first. The
    // optional collector cache only replaces the repeated checked-base/brand
    // proof of a pair that already passed both decoders and that same proof
    // earlier in this drain; the ordinary consumer has no cache at all.
    body.push(assign(s, e.decoded(var(row)), l));
    body.push(assign(g, e.generation(add(var(row), int(1))), l));
    let checked_key = vec![
        assign(
            checked,
            call(e.checked_base, vec![var(e.carrier), var(s), var(g)]),
            l,
        ),
        assert_stmt(eq(e.decoded(var(checked)), int(layout::KIND_STRING)), l),
        // Keep the validated key base before the union arms reuse `checked`
        // for their own payload identities.
        assign(key_base, var(checked), l),
    ];
    match key_cache {
        None => body.extend(checked_key),
        Some(cache) => {
            let mut miss = checked_key;
            // Record the pair only after the original proof succeeded, and
            // mark it valid last so a refused row never seeds the cache.
            miss.push(assign(cache.slot, var(s), l));
            miss.push(assign(cache.generation, var(g), l));
            miss.push(assign(cache.base, var(checked), l));
            miss.push(assign(cache.valid, E::Bool(true), l));
            let hit = vec![
                assign(checked, var(cache.base), l),
                assign(key_base, var(cache.base), l),
            ];
            body.push(branch(
                var(cache.valid),
                vec![branch(
                    and(
                        eq(var(s), var(cache.slot)),
                        eq(var(g), var(cache.generation)),
                    ),
                    hit,
                    miss.clone(),
                    l,
                )],
                miss,
                l,
            ));
        }
    }
    if let Some((key_slot, key_gen)) = key_snapshot {
        // Snapshot the validated key slot and generation into the caller's
        // dedicated temps before the union arms reuse `s` and `g` for payload
        // work; the key pair never aliases overwritten payload scratch.
        body.push(assign(key_slot, var(s), l));
        body.push(assign(key_gen, var(g), l));
    }
    body.extend([
        assign(meta, e.decoded(add(var(row), int(2))), l),
        assert_stmt(not(lt(int(layout::OBJECT_MAX_DATA_META), var(meta))), l),
        assign(tag, int(0), l),
        assign(bits, var(meta), l),
        S::While {
            condition: not(lt(var(bits), int(layout::OBJECT_TAG_FACTOR))),
            body: vec![
                assign(bits, sub(var(bits), int(layout::OBJECT_TAG_FACTOR)), l),
                assign(tag, add(var(tag), int(1)), l),
            ],
            line: l,
        },
        assign(access, not(lt(var(bits), int(8))), l),
        assert_stmt(or(data_meta, accessor_meta), l),
        assign(
            bits,
            choose(var(access), sub(var(bits), int(8)), var(bits)),
            l,
        ),
        assign(c, not(lt(var(bits), int(4))), l),
        assign(bits, choose(var(c), sub(var(bits), int(4)), var(bits)), l),
        assign(en, not(lt(var(bits), int(2))), l),
        assign(bits, choose(var(en), sub(var(bits), int(2)), var(bits)), l),
        assign(w, eq(var(bits), int(1)), l),
    ]);
    let mut accessor = Vec::new();
    for (offset, slot, generation) in [(3, gs, gg), (5, s, g)] {
        accessor.extend([
            assign(slot, e.decoded(add(var(row), int(offset))), l),
            assign(generation, e.generation(add(var(row), int(offset + 1))), l),
            branch(
                eq(var(generation), int(0)),
                vec![assert_stmt(eq(var(slot), int(0)), l)],
                vec![
                    assign(
                        checked,
                        call(
                            e.checked_base,
                            vec![var(e.carrier), var(slot), var(generation)],
                        ),
                        l,
                    ),
                    assert_stmt(eq(e.decoded(var(checked)), int(layout::KIND_FUNCTION)), l),
                ],
                l,
            ),
        ]);
    }
    let mut data = vec![
        assign(n, e.read(add(var(row), int(3))), l),
        assign(flag, e.decoded(add(var(row), int(4))), l),
        assert_stmt(or(eq(var(flag), int(0)), eq(var(flag), int(1))), l),
        assign(s, e.decoded(add(var(row), int(5))), l),
        assign(g, e.generation(add(var(row), int(6))), l),
        assert_stmt(
            or(eq(var(tag), int(value::BOOLEAN)), eq(var(flag), int(0))),
            l,
        ),
        assert_stmt(
            or(eq(var(tag), int(value::NUMBER)), eq(var(n), E::Float(0.0))),
            l,
        ),
    ];
    data.push(branch(
        or(
            eq(var(tag), int(value::STRING)),
            eq(var(tag), int(value::OBJECT)),
        ),
        vec![
            assign(
                checked,
                call(e.checked_base, vec![var(e.carrier), var(s), var(g)]),
                l,
            ),
            assert_stmt(
                choose(
                    eq(var(tag), int(value::STRING)),
                    eq(e.decoded(var(checked)), int(layout::KIND_STRING)),
                    object_like(e.decoded(var(checked))),
                ),
                l,
            ),
        ],
        vec![assert_stmt(and(eq(var(s), int(0)), eq(var(g), int(0))), l)],
        l,
    ));
    // Consumer tails run at the end of their matching validation arm, after
    // every check above and confined to the arm that defines the scratch
    // they read.
    accessor.extend(on_accessor(&scalars));
    data.extend(on_data(&scalars));
    body.push(branch(var(access), accessor, data, l));
    Ok(scalars)
}

/// Decode both union alternatives, including inactive fields and every live
/// key/value/callable identity. No physical pointer crosses a safepoint here.
/// The returned second slot is the row key's already validated base, so a
/// scanning caller can compare its units without a second checked_base of
/// the same node.
///
/// The row checks come from the single shared [`emit_checked_row`] emitter in
/// exactly the original validation and error order; this ordinary consumer
/// splices the record builds into the matching validation arms and assembles
/// the descriptor once afterwards, so every record field, flag and value is
/// identical and only one definition of the descriptor exists.
pub(crate) fn decode_row(
    e: &mut Emit<'_, '_>,
    row: u16,
    body: &mut Vec<S>,
) -> Result<(u16, u16), ParseError> {
    let l = e.line;
    let v = e.boxed()?;
    let get = e.boxed()?;
    let set = e.boxed()?;
    let d = e.temp(descriptor_schema(), empty_descriptor())?;
    body.extend([
        assign(v, value::undefined(), l),
        assign(get, value::undefined(), l),
        assign(set, value::undefined(), l),
    ]);
    // Record builds are spliced into the matching validation arms; the
    // descriptor itself is assembled once afterwards from the same values,
    // so only one definition exists and no arm assigns it twice.
    let scalars = emit_checked_row(
        e,
        row,
        body,
        None,
        None,
        |r| {
            let mut build = Vec::new();
            for (slot, generation, target) in
                [(r.get_slot, r.get_gen, get), (r.slot, r.generation, set)]
            {
                build.push(branch(
                    eq(var(generation), int(0)),
                    Vec::new(),
                    vec![assign(
                        target,
                        value::dynamic_value(
                            int(value::OBJECT),
                            E::Float(0.0),
                            E::Bool(false),
                            var(slot),
                            var(generation),
                        ),
                        l,
                    )],
                    l,
                ));
            }
            build
        },
        |r| {
            vec![assign(
                v,
                value::dynamic_value(
                    var(r.tag),
                    var(r.number),
                    eq(var(r.flag), int(1)),
                    var(r.slot),
                    var(r.generation),
                ),
                l,
            )]
        },
    )?;
    body.push(assign(
        d,
        choose(
            var(scalars.accessor),
            accessor_descriptor(
                var(get),
                var(set),
                var(scalars.enumerable),
                var(scalars.configurable),
            ),
            data_descriptor(
                var(v),
                var(scalars.writable),
                var(scalars.enumerable),
                var(scalars.configurable),
            ),
        ),
        l,
    ));
    Ok((d, scalars.key_base))
}

pub(crate) fn object_like(kind: E) -> E {
    or(
        or(
            or(eq(kind.clone(), int(layout::KIND_OBJECT)), eq(kind.clone(), int(layout::KIND_FUNCTION))),
            or(
                or(eq(kind.clone(), int(layout::KIND_ARRAY)), eq(kind.clone(), int(layout::KIND_TAGGED_ARRAY))),
                or(eq(kind.clone(), int(layout::KIND_BYTES)), eq(kind.clone(), int(layout::KIND_VIEW))),
            ),
        ),
        eq(kind, int(layout::KIND_REGEXP)),
    )
}

pub(crate) fn pack_header(count: E, extensible: E, error: E) -> E {
    pack_object_header(count, extensible, error, E::Bool(false))
}

/// Pack the object header from explicit brand bits. `brand` is `0`, the
/// legacy prototype brand `OBJECT_LEGACY_ERROR_FLAG`, or the instance brand
/// `OBJECT_LEGACY_ERROR_FLAG | OBJECT_ERROR_INSTANCE_FLAG`. Extensibility is
/// passed separately because every trusted allocation is extensible.
pub(crate) fn pack_brand(count: E, extensible: E, brand: E) -> E {
    add(
        mul(int(layout::OBJECT_COUNT_FACTOR), count),
        add(
            choose(extensible, int(layout::OBJECT_EXTENSIBLE_FLAG), int(0)),
            brand,
        ),
    )
}

fn pack_object_header(count: E, extensible: E, error: E, error_instance: E) -> E {
    add(
        mul(int(layout::OBJECT_COUNT_FACTOR), count),
        add(
            choose(extensible, int(layout::OBJECT_EXTENSIBLE_FLAG), int(0)),
            add(
                choose(error, int(layout::OBJECT_LEGACY_ERROR_FLAG), int(0)),
                choose(error_instance, int(layout::OBJECT_ERROR_INSTANCE_FLAG), int(0)),
            ),
        ),
    )
}
pub(crate) fn pack_data(tag: E, w: E, en: E, c: E) -> E {
    add(
        mul(int(layout::OBJECT_TAG_FACTOR), tag),
        add(
            choose(w, int(1), int(0)),
            add(choose(en, int(2), int(0)), choose(c, int(4), int(0))),
        ),
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn declare(
    ir: &mut FrontendIr,
    stmts: &mut Vec<S>,
    name: &str,
    index: u16,
    params: Vec<u16>,
    schemas: Vec<T>,
    result: T,
    body: Vec<S>,
    expr: E,
    line: u32,
) {
    let physical = match &result {
        T::Int => ValueType::Int,
        T::Bool => ValueType::Bool,
        T::Float | T::Number => ValueType::Float,
        T::Null => ValueType::Null,
        _ => ValueType::Map,
    };
    declare_helper(
        ir,
        stmts,
        name,
        index,
        params.len() as u8,
        (0..params.len()).map(|i| format!("arg{i}")).collect(),
        schemas.into_iter().map(Some).collect(),
        Some(result),
        physical,
        params,
        body,
        expr,
        line,
    );
}

/// A scalar representation boundary for proven private Int values only.
/// This is identity, not numeric coercion or validation of raw wire cells.
/// Both the parameter and physical return contract matter to Core codegen.
/// It adds a guest call/frame and fuel instructions, with no managed allocation.
pub(crate) fn append_int_identity(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
) -> Result<u16, ParseError> {
    let index = super::heap::fresh_index(lower, ir, span)?;
    let param = lower.slot(span)?;
    declare(
        ir,
        stmts,
        "__compat_es5_int_identity",
        index,
        vec![param],
        vec![T::Int],
        T::Int,
        vec![],
        var(param),
        lower.line(span),
    );
    Ok(index)
}

pub(crate) fn result_schema(ok: T) -> T {
    T::Result {
        ok: Box::new(ok),
        err: Box::new(value::schema()),
    }
}
pub(crate) fn result(schema: T, ok: bool, payload: E, span: vm::Span) -> E {
    E::ResultConstruct {
        ok,
        payload: Box::new(payload),
        result_schema: schema,
        span,
        semantic_id: None,
    }
}
pub(crate) const OWN: &str = "__CompatEs5OwnDescriptor";
pub(crate) fn own_schema() -> T {
    T::Named(OWN.into(), Vec::new())
}
pub(crate) fn own(found: E, desc: E, index: E) -> E {
    record(vec![
        ("found", found),
        ("descriptor", desc),
        ("index", index),
    ])
}
pub(crate) fn own_declaration() -> vm::compiler::ir::StructDecl {
    vm::compiler::ir::StructDecl {
        name: OWN.into(),
        type_params: Vec::new(),
        body_schema: T::Object(std::collections::HashMap::from([
            ("found".into(), T::Bool),
            ("descriptor".into(), descriptor_schema()),
            ("index".into(), T::Int),
        ])),
        origin: vm::compiler::ir::StructDeclOrigin::Guest,
    }
}
#[derive(Clone, Copy)]
pub(crate) struct ObjectHelpers {
    pub own: u16,
    pub has: u16,
    pub same: u16,
    pub reserve: u16,
    pub get: u16,
    pub put: u16,
    pub delete: u16,
    pub define: u16,
    pub to_desc: u16,
    pub from_desc: u16,
    pub define_properties: u16,
    pub api: u16,
    /// Decimal index-to-String producer used by the dense Array key path.
    pub index_string: u16,
    /// Public `Object.keys` producer behind method code 266.
    pub keys: u16,
    /// Receiver-aware `Object.prototype` method adapter (codes 534/535/267).
    pub proto_call: u16,
    /// Admitted `Function.prototype.call` adapter (code 268). It receives the
    /// real function as receiver and the real argument container.
    pub function_call: u16,
    /// Admitted `Function.prototype.apply` adapter (code 269). It receives
    /// the real function as receiver and the real argument container.
    pub function_apply: u16,
    /// Admitted `Function.prototype.bind` adapter (code 270). It receives
    /// the real function as receiver, the real argument container, and the
    /// real callee box, so a zero-row bound object can re-enter dispatch.
    pub function_bind: u16,
    /// Admitted `JSON.stringify` adapter (code 271). It receives the real
    /// `JSON` object as receiver and the real argument container.
    pub json_stringify: u16,
    /// Admitted `JSON.parse` adapter (code 272). It receives the real `JSON`
    /// object as receiver and the real argument container.
    pub json_parse: u16,
    /// The one admitted `new Date(0)` construction. It receives only the
    /// carrier and returns the real ordinary object whose own rows are the
    /// `getTime` method and the private epoch value Number 0.
    pub date_epoch: u16,
    /// Admitted `Date.prototype.getTime` adapter (code 273). It receives the
    /// epoch object as receiver and the real argument container.
    pub date_gettime: u16,
    /// The one admitted `/a/` literal value. It receives only the carrier and
    /// returns a fresh ordinary object whose single own data row is the
    /// `test` method function the literal publishes.
    pub regexp_value: u16,
    /// Admitted `/a/.test` adapter (code 274). It receives that literal object
    /// as receiver, the real argument container, and the boxed function the
    /// dispatch already resolved. It never re-reads the receiver's own rows,
    /// because argument evaluation may have changed them.
    pub regexp_test: u16,
    /// The actual index `regexp_lowering::append_matcher` returned for the
    /// realm's one `/a/` kernel. `append_test_support` hands the recorded
    /// value back, so `String.prototype.search` calls that exact function
    /// instead of recomputing it from `regexp_test`.
    pub regexp_matcher: u16,
    /// Canonical decimal index of one String key, or -1.
    pub index_key: u16,
    /// Managed String-hint ToString of one property key.
    pub key_string: u16,
    /// Shared `String.prototype.charCodeAt` / `charAt` / `codePointAt` /
    /// `concat` adapter. Dispatch codes 275, 276, 277 and 278 enter this
    /// helper; the index stays start+32.
    pub char_code_at: u16,
    pub array_method: u16,
    /// Numeric-hint DefaultValue. Tries `valueOf`, then `toString`. Same
    /// helper `String.prototype.charCodeAt` already calls; index is start+31.
    pub numeric_default_value: u16,
}
impl ObjectHelpers {
    fn number_parser(self) -> u16 {
        // StringNumericLiteral scanner and its decimal rounder precede DefaultValue.
        self.numeric_default_value - 2
    }

    /// The object helper window: 25 earlier helpers, the two completed-IR
    /// matcher functions the admitted `/a/` literal appends
    /// (`__es5_regexp_*_fold` and `__es5_regexp_*_kernel`), the two private
    /// helpers that build and dispatch it, the canonical StringNumericLiteral
    /// parser pair, the numeric DefaultValue converter, and the real
    /// `String.prototype.charCodeAt` adapter. Keeping all 33 inside one
    /// contiguous reservation means the dispatch index computed from this
    /// count still lands exactly on the next free function index.
    pub const COUNT: u16 = 34;
    /// Initialization-time root-frame reservations, not each helper's runtime
    /// worst case. Sites: getter/setter, DefineOwnProperty, descriptor
    /// conversion in each direction, two-phase definitions, API, the
    /// receiver-aware prototype adapter, the admitted `/a/` value helper
    /// (7 entries: its `test` key, method, owner object and private
    /// environment record), that literal's `test` adapter (6 entries,
    /// including the captured callee it must keep alive), and the String
    /// `charCodeAt`/`charAt`/`concat` adapter (8 entries; the new String is
    /// published in the previously unused entry 7) plus its numeric
    /// DefaultValue converter (4 entries).
    ///
    /// A helper realm initialization actually enters is charged its real
    /// `root_enter` capacity; a helper bootstrap never enters keeps its birth
    /// capacity of 8 as a conservative floor. Both are upper bounds on
    /// initialization, and neither is the helper's runtime worst case.
    ///
    /// The sixth entry is the two-phase `defineProperties` helper
    /// (`define_properties`, `start + 10`). That slot is born as `int(8)` and
    /// bootstrap never calls the helper, so it is charged 8 rather than the
    /// runtime `8 + 4 * props rows` (at most 1032 when a packed object has
    /// 256 rows). The runtime request allocates through `raw_reserve` under
    /// `PHYSICAL_CEILING` / `MAX_ACTIVE_ROOT_ENTRIES`, not through this
    /// table; charging 1032 here reserved 3072 cells that no initialization
    /// allocation consumes. The sum below is order-independent, so every
    /// entry-to-helper mapping has to stay documented here.
    pub fn helper_frame_request_cells() -> u64 {
        [6u64, 6, 8, 8, 8, 8, 8, 8, 7, 6, 8, 4, 3]
            .into_iter()
            .map(|capacity| 4 + 3 * capacity)
            .sum()
    }
    pub fn at(start: u16) -> Self {
        Self {
            own: start,
            has: start + 1,
            same: start + 2,
            reserve: start + 3,
            get: start + 4,
            put: start + 5,
            delete: start + 6,
            define: start + 7,
            to_desc: start + 8,
            from_desc: start + 9,
            define_properties: start + 10,
            api: start + 11,
            index_string: start + 13,
            keys: start + 14,
            proto_call: start + 15,
            function_call: start + 18,
            function_apply: start + 19,
            function_bind: start + 20,
            json_stringify: start + 21,
            json_parse: start + 22,
            date_epoch: start + 23,
            date_gettime: start + 24,
            // The completed-IR matcher pair (`fold` then `kernel`) takes the
            // next two indices when it is appended first, so the two literal
            // helpers keep the slots after it and the whole window stays the
            // contiguous `COUNT` the dispatch index is reserved from.
            regexp_value: start + 27,
            regexp_test: start + 28,
            // Provisional contiguous-window slot; `append` overwrites it with
            // the index `append_matcher` actually returned before the String
            // adapter that reads it is appended.
            regexp_matcher: start + 26,
            char_code_at: start + 32,
            array_method: start + 33,
            numeric_default_value: start + 31,
            index_key: start + 16,
            key_string: start + 17,
        }
    }
}

fn append_own(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    h: ObjectHelpers,
) -> Result<(), ParseError> {
    let carrier = lower.slot(span)?;
    let object = lower.slot(span)?;
    let key = lower.slot(span)?;
    let mut e = Emit::new(lower, span, carrier)?;
    let l = e.line;
    let base = e.integer()?;
    let row = e.integer()?;
    let cursor = e.integer()?;
    let found = e.boolean()?;
    let out = e.temp(descriptor_schema(), empty_descriptor())?;
    let key_base = e.integer()?;
    let key_length = e.temp(T::Number, E::Float(0.0))?;
    let row_length = e.temp(T::Number, E::Float(0.0))?;
    let equal = e.boolean()?;
    let unit = e.integer()?;
    let mut body = vec![
        assign(base, e.base(var(object)), l),
        assert_stmt(tag_is(var(key), value::STRING), l),
        // The reference key is loop invariant: validate it once, then read
        // its units through that checked base. The scan allocates, writes
        // and collects nothing, so its generation cannot move underneath.
        assign(
            key_base,
            call(
                e.checked_base,
                vec![
                    var(e.carrier),
                    field(var(key), "ref_slot"),
                    field(var(key), "generation"),
                ],
            ),
            l,
        ),
        assert_stmt(eq(e.decoded(var(key_base)), int(layout::KIND_STRING)), l),
        assign(key_length, e.read(add(var(key_base), int(2))), l),
    ];
    let header = property_header(&mut e, base, &mut body)?;
    if e.lower.typed_array_used() {
        use super::typed_array as view;
        let index = e.integer()?;
        let mut own_index = Vec::new();
        let element = view::read_element(&mut e, base, index, &mut own_index)?;
        own_index.extend([
            assign(out, data_descriptor(value::number(element), E::Bool(true), E::Bool(true), E::Bool(false)), l),
            assign(found, E::Bool(true), l),
        ]);
        body.push(branch(eq(var(header.kind), int(layout::KIND_VIEW)), vec![
            assign(index, call(h.index_key, vec![var(carrier), var(key)]), l),
            branch(view::in_range(&e, base, index), own_index, Vec::new(), l),
        ], Vec::new(), l));
    }
    let mut scan = vec![assign(
        row,
        add(add(var(base), var(header.header)), mul(int(7), var(cursor))),
        l,
    )];
    // decode_row already validated this row's key node and returned its
    // checked base, so the comparison must not pay a second checked_base for
    // the same identity.
    let (desc, stored_base) = decode_row(&mut e, row, &mut scan)?;
    scan.push(assign(row_length, e.read(add(var(stored_base), int(2))), l));
    scan.push(assign(equal, eq(var(row_length), var(key_length)), l));
    scan.push(assign(unit, int(0), l));
    scan.push(S::While {
        condition: and(var(equal), lt(num(var(unit)), var(row_length))),
        body: vec![
            assign(
                equal,
                and(
                    var(equal),
                    eq(
                        read_string_unit(e.read, e.carrier, stored_base, unit),
                        read_string_unit(e.read, e.carrier, key_base, unit),
                    ),
                ),
                l,
            ),
            assign(unit, add(var(unit), int(1)), l),
        ],
        line: l,
    });
    scan.push(branch(
        var(equal),
        vec![assign(out, var(desc), l), assign(found, E::Bool(true), l)],
        vec![assign(cursor, add(var(cursor), int(1)), l)],
        l,
    ));
    body.push(S::While {
        condition: and(not(var(found)), lt(var(cursor), var(header.count))),
        body: scan,
        line: l,
    });
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        "__compat_es5_get_own_property",
        h.own,
        vec![carrier, object, key],
        vec![carrier_schema.clone(), value::schema(), value::schema()],
        own_schema(),
        body,
        own(var(found), var(out), var(cursor)),
        l,
    );
    Ok(())
}

fn append_has(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    h: ObjectHelpers,
) -> Result<(), ParseError> {
    let carrier = lower.slot(span)?;
    let object = lower.slot(span)?;
    let key = lower.slot(span)?;
    let mut e = Emit::new(lower, span, carrier)?;
    let l = e.line;
    let current = e.boxed()?;
    let found = e.boolean()?;
    let base = e.integer()?;
    let generation = e.integer()?;
    let slot = e.integer()?;
    let d = e.temp(
        own_schema(),
        own(E::Bool(false), empty_descriptor(), int(0)),
    )?;
    let mut body = vec![assign(current, var(object), l)];
    let lookup = vec![
        assign(
            d,
            call(h.own, vec![var(carrier), var(current), var(key)]),
            l,
        ),
        assign(found, field(var(d), "found"), l),
    ];
    let load_base = assign(base, e.base(var(current)), l);
    let mut step = if e.lower.typed_array_used() {
        // Views have no ordinary own rows. Ignore their virtual indices for
        // `in`, but still follow the prototype just like for other objects.
        vec![
            load_base,
            branch(
                eq(e.decoded(var(base)), int(layout::KIND_VIEW)),
                vec![assert_stmt(eq(e.decoded(add(var(base), int(2))), int(0)), l)],
                lookup,
                l,
            ),
        ]
    } else {
        let mut step = lookup;
        step.push(load_base);
        step
    };
    step.extend([
        assign(slot, e.decoded(add(var(base), int(3))), l),
        assign(generation, e.generation(add(var(base), int(4))), l),
        assert_stmt(
            or(not(eq(var(generation), int(0))), eq(var(slot), int(0))),
            l,
        ),
        assign(
            current,
            choose(
                eq(var(generation), int(0)),
                value::null(),
                value::dynamic_value(
                    int(value::OBJECT),
                    E::Float(0.0),
                    E::Bool(false),
                    var(slot),
                    var(generation),
                ),
            ),
            l,
        ),
    ]);
    body.push(S::While {
        condition: and(not(var(found)), tag_is(var(current), value::OBJECT)),
        body: step,
        line: l,
    });
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        "__compat_es5_has_property",
        h.has,
        vec![carrier, object, key],
        vec![carrier_schema.clone(), value::schema(), value::schema()],
        T::Bool,
        body,
        var(found),
        l,
    );
    Ok(())
}

fn append_same(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    h: ObjectHelpers,
) -> Result<(), ParseError> {
    let carrier = lower.slot(span)?;
    let left = lower.slot(span)?;
    let right = lower.slot(span)?;
    let strings = ir
        .functions
        .iter()
        .find(|f| f.name == layout::STRING_EQUALITY_HELPER)
        .expect("string comparator")
        .index;
    let line = lower.line(span);
    let ordinary = value::strict_equal_flag(
        left,
        right,
        Some(value::StringEquality {
            carrier: var(carrier),
            index: strings,
        }),
    );
    let a = num(field(var(left), "number"));
    let b = num(field(var(right), "number"));
    let reciprocal = |v: E| E::Div(Box::new(E::Float(1.0)), Box::new(v));
    let numbers = or(
        and(not(eq(a.clone(), a.clone())), not(eq(b.clone(), b.clone()))),
        and(
            eq(a.clone(), b.clone()),
            or(
                not(eq(a.clone(), E::Float(0.0))),
                eq(reciprocal(a), reciprocal(b)),
            ),
        ),
    );
    let expr = choose(
        and(
            tag_is(var(left), value::NUMBER),
            tag_is(var(right), value::NUMBER),
        ),
        numbers,
        ordinary,
    );
    declare(
        ir,
        stmts,
        "__compat_es5_same_value",
        h.same,
        vec![carrier, left, right],
        vec![carrier_schema.clone(), value::schema(), value::schema()],
        T::Bool,
        Vec::new(),
        expr,
        line,
    );
    Ok(())
}

fn append_reserve(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    h: ObjectHelpers,
) -> Result<(), ParseError> {
    let carrier = lower.slot(span)?;
    let object = lower.slot(span)?;
    let required = lower.slot(span)?;
    let heap = lower.heap_ref(span)?.helpers;
    let mut e = Emit::new(lower, span, carrier)?;
    let l = e.line;
    let base = e.integer()?;
    let size = e.integer()?;
    let needed = e.integer()?;
    let fresh = e.integer()?;
    let fresh_size = e.integer()?;
    let cursor = e.integer()?;
    let used = e.integer()?;
    let slot = e.integer()?;
    let page = e.integer()?;
    let ordinal = e.integer()?;
    let directory = e.integer()?;
    let next = e.integer()?;
    let cells = e.integer()?;
    let mut body = vec![
        assign(base, e.base(var(object)), l),
        assert_stmt(not(lt(int(256), var(required))), l),
    ];
    let header = property_header(&mut e, base, &mut body)?;
    body.extend([
        assert_stmt(not(lt(var(required), var(header.count))), l),
        assign(size, e.decoded(sub(var(base), int(3))), l),
        assign(
            needed,
            add(int(3), add(var(header.header), mul(int(7), var(required)))),
            l,
        ),
    ]);
    let mut grow = vec![
        assign(
            fresh,
            call(
                heap.raw_reserve,
                vec![var(carrier), var(needed), int(layout::RAW_CLASS_NODE)],
            ),
            l,
        ),
        // Reservation can collect. Refresh every physical address afterwards.
        assign(base, e.base(var(object)), l),
        assign(size, e.decoded(sub(var(base), int(3))), l),
        assign(fresh_size, e.decoded(sub(var(fresh), int(3))), l),
        assign(
            used,
            add(var(header.header), mul(int(7), var(header.count))),
            l,
        ),
        assign(cursor, int(0), l),
    ];
    let copy = vec![
        e.write(
            add(var(fresh), var(cursor)),
            choose(
                lt(var(cursor), var(used)),
                e.read(add(var(base), var(cursor))),
                E::Float(0.0),
            ),
        )?,
        assign(cursor, add(var(cursor), int(1)), l),
    ];
    grow.push(S::While {
        condition: lt(var(cursor), sub(var(fresh_size), int(3))),
        body: copy,
        line: l,
    });
    grow.extend([
        assign(slot, field(var(object), "ref_slot"), l),
        assign(page, int(0), l),
        assign(ordinal, var(slot), l),
        S::While {
            condition: not(lt(var(ordinal), int(64))),
            body: vec![
                assign(ordinal, sub(var(ordinal), int(64)), l),
                assign(page, add(var(page), int(1)), l),
            ],
            line: l,
        },
        assign(
            directory,
            add(
                add(e.decoded(add(int(layout::HEADER_CELLS), var(page))), int(3)),
                mul(int(7), var(ordinal)),
            ),
            l,
        ),
        assign(next, e.decoded(int(layout::HDR_RAW_FREE_HEAD)), l),
        assign(cells, e.decoded(int(layout::HDR_NODE_CELLS)), l),
    ]);
    // Directory switch, accounting, and old block retirement are one
    // nonallocating commit interval. Slot/generation/mark state never change.
    grow.push(e.write(
        add(var(directory), int(layout::ROW_PHYSICAL_BASE)),
        var(fresh),
    )?);
    grow.push(e.write(
        add(var(directory), int(layout::ROW_DATA_CELLS)),
        sub(var(fresh_size), int(3)),
    )?);
    grow.push(e.write(
        int(layout::HDR_NODE_CELLS),
        add(var(cells), sub(var(fresh_size), var(size))),
    )?);
    grow.push(e.write(sub(var(base), int(2)), int(layout::RAW_CLASS_FREE))?);
    grow.push(e.write(sub(var(base), int(1)), var(next))?);
    grow.push(e.write(int(layout::HDR_RAW_FREE_HEAD), sub(var(base), int(3)))?);
    body.push(branch(lt(var(size), var(needed)), grow, Vec::new(), l));
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        "__compat_es5_object_reserve_rows",
        h.reserve,
        vec![carrier, object, required],
        vec![carrier_schema.clone(), value::schema(), T::Int],
        T::Null,
        body,
        E::Null,
        l,
    );
    Ok(())
}

/// Decode the six descriptor presence bits without masking unknown bits.
fn presence(
    e: &mut Emit<'_, '_>,
    descriptor: E,
    body: &mut Vec<S>,
) -> Result<[u16; 6], ParseError> {
    let l = e.line;
    let p = e.integer()?;
    let flags = (0..6).map(|_| e.boolean()).collect::<Result<Vec<_>, _>>()?;
    body.push(assign(p, field(descriptor, "present"), l));
    body.push(assert_stmt(
        and(not(lt(var(p), int(0))), lt(var(p), int(64))),
        l,
    ));
    for i in (0..6).rev() {
        let bit = 1 << i;
        body.push(assign(flags[i], not(lt(var(p), int(bit))), l));
        body.push(assign(
            p,
            choose(var(flags[i]), sub(var(p), int(bit)), var(p)),
            l,
        ));
    }
    Ok(flags.try_into().expect("six flags"))
}

fn store_row(
    e: &mut Emit<'_, '_>,
    row: E,
    key: E,
    descriptor: E,
    body: &mut Vec<S>,
) -> Result<(), ParseError> {
    let access = eq(field(descriptor.clone(), "present"), int(60));
    let payload = field(descriptor.clone(), "value");
    let get = field(descriptor.clone(), "get");
    let set = field(descriptor.clone(), "set");
    let meta = choose(
        access.clone(),
        add(
            int(8),
            add(
                choose(field(descriptor.clone(), "enumerable"), int(2), int(0)),
                choose(field(descriptor.clone(), "configurable"), int(4), int(0)),
            ),
        ),
        pack_data(
            field(payload.clone(), "tag"),
            field(descriptor.clone(), "writable"),
            field(descriptor.clone(), "enumerable"),
            field(descriptor, "configurable"),
        ),
    );
    for (offset, payload) in [
        (0, field(key.clone(), "ref_slot")),
        (1, field(key, "generation")),
        (2, meta),
        (
            3,
            choose(
                access.clone(),
                num(field(get.clone(), "ref_slot")),
                field(payload.clone(), "number"),
            ),
        ),
        (
            4,
            choose(
                access.clone(),
                field(get, "generation"),
                choose(field(payload.clone(), "flag"), int(1), int(0)),
            ),
        ),
        (
            5,
            choose(
                access.clone(),
                field(set.clone(), "ref_slot"),
                field(payload.clone(), "ref_slot"),
            ),
        ),
        (
            6,
            choose(
                access,
                field(set, "generation"),
                field(payload, "generation"),
            ),
        ),
    ] {
        body.push(e.write(add(row.clone(), int(offset)), payload)?);
    }
    Ok(())
}

fn append_define(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    h: ObjectHelpers,
) -> Result<(), ParseError> {
    let carrier = lower.slot(span)?;
    let object = lower.slot(span)?;
    let key = lower.slot(span)?;
    let desc = lower.slot(span)?;
    let throw = lower.slot(span)?;
    let heap = lower.heap_ref(span)?.helpers;
    let error = lower.intrinsics.expect("intrinsics").type_error;
    let mut e = Emit::new(lower, span, carrier)?;
    let l = e.line;
    let source_span = e.lower.ir_span(span);
    let schema = result_schema(T::Bool);
    let out = e.temp(
        schema.clone(),
        result(schema.clone(), true, E::Bool(true), source_span),
    )?;
    let frame = e.integer()?;
    let base = e.integer()?;
    let position = e.integer()?;
    let count = e.integer()?;
    let reject = e.boolean()?;
    let previous = e.temp(
        own_schema(),
        own(E::Bool(false), empty_descriptor(), int(0)),
    )?;
    let current = e.temp(descriptor_schema(), empty_descriptor())?;
    let merged = e.temp(descriptor_schema(), empty_descriptor())?;
    let payload = e.boxed()?;
    let get = e.boxed()?;
    let set = e.boxed()?;
    let writable = e.boolean()?;
    let enumerable = e.boolean()?;
    let configurable = e.boolean()?;
    let access = e.boolean()?;
    let mut body = vec![assign(
        frame,
        call(heap.root_enter, vec![var(carrier), int(8)]),
        l,
    )];
    for (i, payload) in [
        var(object),
        var(key),
        field(var(desc), "value"),
        field(var(desc), "get"),
        field(var(desc), "set"),
    ]
    .into_iter()
    .enumerate()
    {
        body.push(e.root(var(frame), int(i as i64), payload)?);
    }
    body.extend([
        assign(
            previous,
            call(h.own, vec![var(carrier), var(object), var(key)]),
            l,
        ),
        assign(current, field(var(previous), "descriptor"), l),
        assign(position, field(var(previous), "index"), l),
        assign(base, e.base(var(object)), l),
    ]);
    let header = property_header(&mut e, base, &mut body)?;
    let flags = presence(&mut e, var(desc), &mut body)?;
    let [hv, hw, hg, hs, he, hc] = flags;
    let incoming_data = or(var(hv), var(hw));
    let incoming_access = or(var(hg), var(hs));
    body.push(assert_stmt(
        not(and(incoming_data.clone(), incoming_access.clone())),
        l,
    ));
    body.extend([
        assign(access, eq(field(var(current), "present"), int(60)), l),
        assign(payload, field(var(current), "value"), l),
        assign(get, field(var(current), "get"), l),
        assign(set, field(var(current), "set"), l),
        assign(writable, field(var(current), "writable"), l),
        assign(enumerable, field(var(current), "enumerable"), l),
        assign(configurable, field(var(current), "configurable"), l),
    ]);
    let switch = and(
        or(incoming_data.clone(), incoming_access.clone()),
        not(eq(var(access), incoming_access.clone())),
    );
    let frozen_reject = or(
        and(var(hc), field(var(desc), "configurable")),
        and(
            var(he),
            not(eq(field(var(desc), "enumerable"), var(enumerable))),
        ),
    );
    let value_changes = and(
        var(hv),
        not(call(
            h.same,
            vec![var(carrier), var(payload), field(var(desc), "value")],
        )),
    );
    let writable_changes = and(var(hw), field(var(desc), "writable"));
    let immutable_data = and(
        not(var(access)),
        and(not(var(writable)), or(writable_changes, value_changes)),
    );
    let immutable_accessor = and(
        var(access),
        or(
            and(
                var(hg),
                not(call(
                    h.same,
                    vec![var(carrier), var(get), field(var(desc), "get")],
                )),
            ),
            and(
                var(hs),
                not(call(
                    h.same,
                    vec![var(carrier), var(set), field(var(desc), "set")],
                )),
            ),
        ),
    );
    let existing = vec![
        assign(
            reject,
            and(
                not(var(configurable)),
                or(
                    frozen_reject,
                    or(switch.clone(), or(immutable_data, immutable_accessor)),
                ),
            ),
            l,
        ),
        branch(
            switch,
            vec![
                assign(access, incoming_access.clone(), l),
                assign(payload, value::undefined(), l),
                assign(get, value::undefined(), l),
                assign(set, value::undefined(), l),
                assign(writable, E::Bool(false), l),
            ],
            Vec::new(),
            l,
        ),
    ];
    let fresh = vec![
        assign(reject, not(var(header.extensible)), l),
        assign(access, incoming_access, l),
    ];
    body.push(branch(field(var(previous), "found"), existing, fresh, l));
    for (has, destination, name) in [
        (hv, payload, "value"),
        (hw, writable, "writable"),
        (hg, get, "get"),
        (hs, set, "set"),
        (he, enumerable, "enumerable"),
        (hc, configurable, "configurable"),
    ] {
        body.push(branch(
            var(has),
            vec![assign(destination, field(var(desc), name), l)],
            Vec::new(),
            l,
        ));
    }
    body.push(assign(
        merged,
        choose(
            var(access),
            accessor_descriptor(var(get), var(set), var(enumerable), var(configurable)),
            data_descriptor(
                var(payload),
                var(writable),
                var(enumerable),
                var(configurable),
            ),
        ),
        l,
    ));
    let row = e.integer()?;
    let sink = e.temp(T::Null, E::Null)?;
    let mut commit = vec![
        assign(
            count,
            add(
                var(header.count),
                choose(field(var(previous), "found"), int(0), int(1)),
            ),
            l,
        ),
        assert_stmt(not(lt(int(256), var(count))), l),
        assign(
            sink,
            call(h.reserve, vec![var(carrier), var(object), var(count)]),
            l,
        ),
        assign(base, e.base(var(object)), l),
        assign(
            row,
            add(
                add(var(base), var(header.header)),
                mul(int(7), var(position)),
            ),
            l,
        ),
    ];
    store_row(&mut e, var(row), var(key), var(merged), &mut commit)?;
    commit.push(e.write(
        add(var(base), int(2)),
        pack_object_header(var(count), var(header.extensible), var(header.error), var(header.error_instance)),
    )?);
    let mut fail = vec![assign(
        out,
        result(schema.clone(), true, E::Bool(false), source_span),
        l,
    )];
    let mut thrown = vec![assign(payload, call(error, vec![var(carrier)]), l)];
    thrown.push(e.root(var(frame), int(7), var(payload))?);
    thrown.push(assign(
        out,
        result(schema.clone(), false, var(payload), source_span),
        l,
    ));
    fail.push(branch(var(throw), thrown, Vec::new(), l));
    if e.lower.typed_array_used() {
        let unsupported = super::typed_array::error(&mut e, "unsupported object class", false, payload)?;
        let mut refused = unsupported;
        refused.push(assign(out, result(schema.clone(), false, var(payload), source_span), l));
        body.push(branch(eq(var(header.kind), int(layout::KIND_VIEW)), refused,
            vec![branch(var(reject), fail, commit, l)], l));
    } else {
        body.push(branch(var(reject), fail, commit, l));
    }
    body.push(e.effect(heap.root_leave, vec![var(carrier), var(frame)])?);
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        "__compat_es5_define_own_property",
        h.define,
        vec![carrier, object, key, desc, throw],
        vec![
            carrier_schema.clone(),
            value::schema(),
            value::schema(),
            descriptor_schema(),
            T::Bool,
        ],
        schema,
        body,
        var(out),
        l,
    );
    Ok(())
}

pub(crate) fn matched(
    e: &mut Emit<'_, '_>,
    raw: u16,
    schema: T,
    ok: u16,
    error: u16,
    ok_body: Vec<S>,
    error_body: Vec<S>,
) -> Result<S, ParseError> {
    let sink = e.temp(T::Null, E::Null)?;
    Ok(assign(
        sink,
        E::ResultMatch {
            value: Box::new(var(raw)),
            value_slot: raw,
            ok_binding: Some(ok),
            err_binding: Some(error),
            ok_expr: Box::new(E::Block {
                stmts: ok_body,
                expr: Box::new(E::Null),
            }),
            err_expr: Box::new(E::Block {
                stmts: error_body,
                expr: Box::new(E::Null),
            }),
            scrutinee_schema: schema,
            span: e.lower.ir_span(e.span),
            semantic_id: None,
        },
        e.line,
    ))
}

/// Invoke a real accessor through the unchanged four-argument dispatcher.
/// Caller supplies its own rooted receiver and independently retains RHS.
fn accessor_call(
    e: &mut Emit<'_, '_>,
    roots: (E, E, E),
    callable: E,
    rhs: Option<E>,
    dispatch: u16,
    out: u16,
    body: &mut Vec<S>,
) -> Result<(), ParseError> {
    let (receiver, key, holder) = roots;
    let heap = e.lower.heap_ref(e.span)?.helpers;
    let l = e.line;
    let span = e.lower.ir_span(e.span);
    let frame = e.integer()?;
    let args = e.temp(
        T::Array(Box::new(value::schema())),
        call(B::ArrayNew.call_index(), Vec::new()),
    )?;
    let raw = e.temp(
        value::logical_result_schema(),
        value::result_ok(value::completion_normal(), span),
    )?;
    let ok = e.temp(value::completion_schema(), value::completion_normal())?;
    let error = e.boxed()?;
    let payload = e.boxed()?;
    body.push(assign(
        frame,
        call(heap.root_enter, vec![var(e.carrier), int(6)]),
        l,
    ));
    for (i, payload) in [
        receiver.clone(),
        key,
        holder,
        callable.clone(),
        rhs.clone().unwrap_or_else(value::undefined),
    ]
    .into_iter()
    .enumerate()
    {
        body.push(e.root(var(frame), int(i as i64), payload)?);
    }
    body.push(assign(args, call(B::ArrayNew.call_index(), Vec::new()), l));
    if let Some(rhs) = rhs.clone() {
        body.push(assign(
            args,
            call(B::ArrayPush.call_index(), vec![var(args), rhs]),
            l,
        ));
    }
    body.push(assign(
        raw,
        call(
            dispatch,
            vec![var(e.carrier), callable, receiver, var(args)],
        ),
        l,
    ));
    let schema = result_schema(value::schema());
    let ok_body = vec![
        assign(
            payload,
            if rhs.is_some() {
                value::undefined()
            } else {
                choose(
                    eq(field(var(ok), "kind"), int(value::COMPLETION_RETURN)),
                    field(var(ok), "value"),
                    value::undefined(),
                )
            },
            l,
        ),
        e.root(var(frame), int(5), var(payload))?,
        assign(out, result(schema.clone(), true, var(payload), span), l),
    ];
    let error_body = vec![
        e.root(var(frame), int(5), var(error))?,
        assign(out, result(schema, false, var(error), span), l),
    ];
    body.push(matched(
        e,
        raw,
        value::logical_result_schema(),
        ok,
        error,
        ok_body,
        error_body,
    )?);
    body.push(e.effect(heap.root_leave, vec![var(e.carrier), var(frame)])?);
    Ok(())
}

fn append_get(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    h: ObjectHelpers,
    dispatch: u16,
) -> Result<(), ParseError> {
    let carrier = lower.slot(span)?;
    let reference = lower.slot(span)?;
    let error = lower.intrinsics.expect("intrinsics").type_error;
    let root = lower.intrinsics.expect("intrinsics").root;
    let heap = lower.heap_ref(span)?.helpers;
    let mut e = Emit::new(lower, span, carrier)?;
    let l = e.line;
    let source_span = e.lower.ir_span(span);
    let schema = result_schema(value::schema());
    let out = e.temp(
        schema.clone(),
        result(schema.clone(), true, value::undefined(), source_span),
    )?;
    let original = e.boxed()?;
    let current = e.boxed()?;
    let key = e.boxed()?;
    let base = e.integer()?;
    let slot = e.integer()?;
    let generation = e.integer()?;
    let found = e.boolean()?;
    let kind = e.integer()?;
    let key_base = e.integer()?;
    let length = e.temp(T::Number, E::Float(0.0))?;
    let index_value = e.integer()?;
    let element = e.boxed()?;
    let method_table = e.boxed()?;
    let array_proto = e.boxed()?;
    let d = e.temp(
        own_schema(),
        own(E::Bool(false), empty_descriptor(), int(0)),
    )?;
    let desc = e.temp(descriptor_schema(), empty_descriptor())?;
    let mut body = vec![
        assign(original, field(var(reference), "base"), l),
        assign(current, var(original), l),
        assign(
            key,
            value::dynamic_value(
                int(value::STRING),
                E::Float(0.0),
                E::Bool(false),
                field(var(reference), "key_slot"),
                field(var(reference), "key_generation"),
            ),
            l,
        ),
        assign(key_base, e.base(var(key)), l),
        assert_stmt(eq(e.decoded(var(key_base)), int(layout::KIND_STRING)), l),
    ];
    let mut ordinary = vec![
        assign(
            d,
            call(h.own, vec![var(carrier), var(current), var(key)]),
            l,
        ),
        assign(found, field(var(d), "found"), l),
        assign(desc, field(var(d), "descriptor"), l),
    ];
    let mut get = Vec::new();
    accessor_call(
        &mut e,
        (var(original), var(key), var(current)),
        field(var(desc), "get"),
        None,
        dispatch,
        out,
        &mut get,
    )?;
    let mut got = vec![branch(
        eq(field(var(desc), "present"), int(51)),
        vec![assign(
            out,
            result(schema.clone(), true, field(var(desc), "value"), source_span),
            l,
        )],
        vec![branch(
            not(tag_is(field(var(desc), "get"), value::UNDEFINED)),
            get,
            Vec::new(),
            l,
        )],
        l,
    )];
    let miss = vec![
        assign(base, e.base(var(current)), l),
        assign(slot, e.decoded(add(var(base), int(3))), l),
        assign(generation, e.generation(add(var(base), int(4))), l),
        assert_stmt(
            or(not(eq(var(generation), int(0))), eq(var(slot), int(0))),
            l,
        ),
        assign(
            current,
            choose(
                eq(var(generation), int(0)),
                value::null(),
                value::dynamic_value(
                    int(value::OBJECT),
                    E::Float(0.0),
                    E::Bool(false),
                    var(slot),
                    var(generation),
                ),
            ),
            l,
        ),
    ];
    // Dense Array own surface: a canonical in-range index reads its real
    // element, `length` reads the live header length. Everything else is not
    // own and continues through the actual prototype pair.
    let mut array_own = vec![
        assign(base, e.base(var(current)), l),
        assign(
            length,
            call(heap.array_length, vec![var(carrier), var(base)]),
            l,
        ),
        assign(
            index_value,
            call(h.index_key, vec![var(carrier), var(key)]),
            l,
        ),
    ];
    let in_range = and(
        not(lt(var(index_value), int(-1))),
        not(eq(var(index_value), int(-1))),
    );
    let in_range = and(in_range, lt(num(var(index_value)), var(length)));
    let element_read = vec![
        assign(
            element,
            call(
                heap.array_get,
                vec![var(carrier), var(base), var(index_value)],
            ),
            l,
        ),
        assign(
            out,
            result(schema.clone(), true, var(element), source_span),
            l,
        ),
        assign(found, E::Bool(true), l),
    ];
    let length_read = vec![
        assign(
            out,
            result(
                schema.clone(),
                true,
                value::number(num(var(length))),
                source_span,
            ),
            l,
        ),
        assign(found, E::Bool(true), l),
    ];
    let mut method_lookup = vec![
        assign(method_table, call(e.lower.intrinsics.expect("intrinsics").root, vec![var(carrier), int(super::intrinsics::array_method_table_root(e.lower))]), l),
        assign(d, call(h.own, vec![var(carrier), var(method_table), var(key)]), l),
        assign(found, field(var(d), "found"), l),
        assign(desc, field(var(d), "descriptor"), l),
    ];
    method_lookup.push(branch(var(found), got.clone(), miss.clone(), l));
    let method_or_miss = vec![
        assign(array_proto, call(e.lower.intrinsics.expect("intrinsics").array_proto, vec![var(carrier)]), l),
        branch(and(eq(field(var(current), "ref_slot"), field(var(array_proto), "ref_slot")), eq(field(var(current), "generation"), field(var(array_proto), "generation"))), method_lookup, miss.clone(), l),
    ];
    array_own.push(branch(
        in_range,
        element_read,
        vec![branch(
            key_is_length(&e, key_base),
            length_read,
            method_or_miss,
            l,
        )],
        l,
    ));
    ordinary.push(branch(var(found), std::mem::take(&mut got), miss, l));
    let mut loop_body = vec![
        assign(base, e.base(var(current)), l),
        assign(kind, e.decoded(var(base)), l),
        branch(
            or(
                eq(var(kind), int(layout::KIND_ARRAY)),
                eq(var(kind), int(layout::KIND_TAGGED_ARRAY)),
            ),
            array_own,
            ordinary,
            l,
        ),
    ];
    if e.lower.array_buffer_used.get() {
        let normal = loop_body.pop().expect("ordinary/array Get branch");
        loop_body.push(branch(
            and(eq(var(kind), int(layout::KIND_BYTES)), not(lt(call(h.index_key, vec![var(carrier), var(key)]), int(0)))),
            vec![assign(out, result(schema.clone(), false, call(error, vec![var(carrier)]), source_span), l), assign(found, E::Bool(true), l)],
            vec![normal], l,
        ));
    }
    if e.lower.typed_array_used() {
        use super::typed_array as view;
        let mut indexed = Vec::new();
        let element = view::read_element(&mut e, base, index_value, &mut indexed)?;
        indexed.push(assign(out, result(schema.clone(), true, value::number(element), source_span), l));
        let err = e.boxed()?;
        let mut unsupported = view::error(&mut e, "unsupported object class", false, err)?;
        unsupported.extend([
            assign(out, result(schema.clone(), false, var(err), source_span), l),
            assign(found, E::Bool(true), l),
        ]);
        let unsupported_method = key_is_ascii(&e, key_base, "fill");
        let normal = loop_body.pop().expect("ordinary Get branch");
        let mut view_body = view_index(&mut e, h.index_key, key, index_value)?;
        view_body.push(branch(not(eq(var(index_value), int(-1))), vec![
            branch(view::in_range(&e, base, index_value), indexed,
                vec![assign(out, result(schema.clone(), true, value::undefined(), source_span), l)], l),
            assign(found, E::Bool(true), l),
        ], vec![branch(unsupported_method, unsupported, Vec::new(), l)], l));
        loop_body.push(branch(eq(var(kind), int(layout::KIND_VIEW)), view_body, Vec::new(), l));
        loop_body.push(branch(not(var(found)), vec![normal], Vec::new(), l));
    }
    let loop_body = vec![S::While {
        condition: and(not(var(found)), tag_is(var(current), value::OBJECT)),
        body: loop_body,
        line: l,
    }];
    let failure = vec![assign(
        out,
        result(
            schema.clone(),
            false,
            call(error, vec![var(carrier)]),
            source_span,
        ),
        l,
    )];
    // Non-object receivers branch on the ORIGINAL tag. Null and Undefined keep
    // the exact existing TypeError, so no later argument is evaluated. A known
    // String reads its virtual own rows first: `length` comes from the live
    // String header and is never masked by a prototype row, and a canonical
    // in-range index keeps the existing TypeError because public character
    // production is outside this bridge. Every other String/Number/Boolean
    // name continues on the same ordinary Reference chain with `current` set
    // to the private primitive prototype; the original primitive stays the
    // actual receiver for the accessor protocol.
    let string_entry = vec![
        assign(base, e.base(var(original)), l),
        assert_stmt(eq(e.decoded(var(base)), int(layout::KIND_STRING)), l),
        assign(length, e.read(add(var(base), int(2))), l),
        assign(
            index_value,
            call(h.index_key, vec![var(carrier), var(key)]),
            l,
        ),
        branch(
            key_is_length(&e, key_base),
            vec![
                assign(
                    out,
                    result(
                        schema.clone(),
                        true,
                        value::number(num(var(length))),
                        source_span,
                    ),
                    l,
                ),
                assign(found, E::Bool(true), l),
            ],
            vec![branch(
                and(
                    not(lt(var(index_value), int(0))),
                    lt(num(var(index_value)), var(length)),
                ),
                failure.clone(),
                vec![assign(
                    current,
                    call(
                        root,
                        vec![
                            var(carrier),
                            int(super::intrinsics::string_prototype_root(e.lower)),
                        ],
                    ),
                    l,
                )],
                l,
            )],
            l,
        ),
    ];
    let number_entry = vec![assign(
        current,
        call(
            root,
            vec![
                var(carrier),
                int(super::intrinsics::number_prototype_root(e.lower)),
            ],
        ),
        l,
    )];
    let boolean_entry = vec![assign(
        current,
        call(
            root,
            vec![
                var(carrier),
                int(super::intrinsics::boolean_prototype_root(e.lower)),
            ],
        ),
        l,
    )];
    let mut number_arm = number_entry;
    number_arm.extend(loop_body.iter().cloned());
    let mut boolean_arm = boolean_entry;
    boolean_arm.extend(loop_body.iter().cloned());
    let number_arm = vec![branch(
        tag_is(var(original), value::NUMBER),
        number_arm,
        boolean_arm,
        l,
    )];
    let mut string_arm = string_entry;
    string_arm.extend(loop_body.iter().cloned());
    let primitive = vec![branch(
        tag_is(var(original), value::STRING),
        string_arm,
        vec![branch(
            or(
                tag_is(var(original), value::NUMBER),
                tag_is(var(original), value::BOOLEAN),
            ),
            number_arm,
            failure,
            l,
        )],
        l,
    )];
    body.push(branch(
        tag_is(var(original), value::OBJECT),
        loop_body,
        primitive,
        l,
    ));
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        layout::REF_GET_HELPER,
        h.get,
        vec![carrier, reference],
        vec![carrier_schema.clone(), value::reference_schema()],
        schema,
        body,
        var(out),
        l,
    );
    Ok(())
}

fn append_delete(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    h: ObjectHelpers,
) -> Result<(), ParseError> {
    let carrier = lower.slot(span)?;
    let reference = lower.slot(span)?;
    let error = lower.intrinsics.expect("intrinsics").type_error;
    let mut e = Emit::new(lower, span, carrier)?;
    let l = e.line;
    let source_span = e.lower.ir_span(span);
    let schema = result_schema(T::Bool);
    let out = e.temp(
        schema.clone(),
        result(schema.clone(), true, E::Bool(true), source_span),
    )?;
    let object = e.boxed()?;
    let key = e.boxed()?;
    let base = e.integer()?;
    let row = e.integer()?;
    let cursor = e.integer()?;
    let end = e.integer()?;
    let d = e.temp(
        own_schema(),
        own(E::Bool(false), empty_descriptor(), int(0)),
    )?;
    let mut body = vec![
        assign(object, field(var(reference), "base"), l),
        assign(
            key,
            value::dynamic_value(
                int(value::STRING),
                E::Float(0.0),
                E::Bool(false),
                field(var(reference), "key_slot"),
                field(var(reference), "key_generation"),
            ),
            l,
        ),
    ];
    let mut ordinary = vec![
        assign(d, call(h.own, vec![var(carrier), var(object), var(key)]), l),
        assign(base, e.base(var(object)), l),
    ];
    let header = property_header(&mut e, base, &mut ordinary)?;
    let mut remove = vec![
        assign(
            row,
            add(
                add(var(base), var(header.header)),
                mul(int(7), field(var(d), "index")),
            ),
            l,
        ),
        assign(cursor, var(row), l),
        assign(
            end,
            add(
                add(var(base), var(header.header)),
                mul(int(7), sub(var(header.count), int(1))),
            ),
            l,
        ),
    ];
    remove.push(S::While {
        condition: lt(var(cursor), var(end)),
        body: vec![
            e.write(var(cursor), e.read(add(var(cursor), int(7))))?,
            assign(cursor, add(var(cursor), int(1)), l),
        ],
        line: l,
    });
    for i in 0..7 {
        remove.push(e.write(add(var(end), int(i)), int(0))?);
    }
    remove.push(e.write(
        add(var(base), int(2)),
        pack_object_header(
            sub(var(header.count), int(1)),
            var(header.extensible),
            var(header.error),
            var(header.error_instance),
        ),
    )?);
    let rejected = vec![
        assign(
            out,
            result(schema.clone(), true, E::Bool(false), source_span),
            l,
        ),
        branch(
            field(var(reference), "strict"),
            vec![assign(
                out,
                result(
                    schema.clone(),
                    false,
                    call(error, vec![var(carrier)]),
                    source_span,
                ),
                l,
            )],
            Vec::new(),
            l,
        ),
    ];
    ordinary.push(branch(
        field(var(d), "found"),
        vec![branch(
            field(field(var(d), "descriptor"), "configurable"),
            remove,
            rejected,
            l,
        )],
        Vec::new(),
        l,
    ));
    body.push(branch(
        tag_is(var(object), value::OBJECT),
        ordinary,
        vec![assign(
            out,
            result(
                schema.clone(),
                false,
                call(error, vec![var(carrier)]),
                source_span,
            ),
            l,
        )],
        l,
    ));
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        "__compat_es5_delete_property",
        h.delete,
        vec![carrier, reference],
        vec![carrier_schema.clone(), value::reference_schema()],
        schema,
        body,
        var(out),
        l,
    );
    Ok(())
}

fn append_put(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    h: ObjectHelpers,
    dispatch: u16,
) -> Result<(), ParseError> {
    let carrier = lower.slot(span)?;
    let reference = lower.slot(span)?;
    let rhs = lower.slot(span)?;
    let error = lower.intrinsics.expect("intrinsics").type_error;
    let mut e = Emit::new(lower, span, carrier)?;
    let l = e.line;
    let source_span = e.lower.ir_span(span);
    let schema = result_schema(T::Null);
    let values = result_schema(value::schema());
    let bools = result_schema(T::Bool);
    let out = e.temp(
        schema.clone(),
        result(schema.clone(), true, E::Null, source_span),
    )?;
    let object = e.boxed()?;
    let key = e.boxed()?;
    let current = e.boxed()?;
    let base = e.integer()?;
    let generation = e.integer()?;
    let slot = e.integer()?;
    let found = e.boolean()?;
    let reject = e.boolean()?;
    let setter = e.boolean()?;
    let is_own = e.boolean()?;
    let d = e.temp(
        own_schema(),
        own(E::Bool(false), empty_descriptor(), int(0)),
    )?;
    let desc = e.temp(descriptor_schema(), empty_descriptor())?;
    let called = e.temp(
        values.clone(),
        result(values.clone(), true, value::undefined(), source_span),
    )?;
    let raw = e.temp(
        bools.clone(),
        result(bools.clone(), true, E::Bool(true), source_span),
    )?;
    let ok = e.boolean()?;
    let err = e.boxed()?;
    let value_ok = e.boxed()?;
    let mut body = vec![
        assign(object, field(var(reference), "base"), l),
        assign(current, var(object), l),
        assign(
            key,
            value::dynamic_value(
                int(value::STRING),
                E::Float(0.0),
                E::Bool(false),
                field(var(reference), "key_slot"),
                field(var(reference), "key_generation"),
            ),
            l,
        ),
    ];
    let mut ordinary = vec![
        assign(
            d,
            call(h.own, vec![var(carrier), var(current), var(key)]),
            l,
        ),
        assign(is_own, field(var(d), "found"), l),
        assign(base, e.base(var(object)), l),
    ];
    let header = property_header(&mut e, base, &mut ordinary)?;
    let walk = vec![
        assign(
            d,
            call(h.own, vec![var(carrier), var(current), var(key)]),
            l,
        ),
        assign(found, field(var(d), "found"), l),
        assign(desc, field(var(d), "descriptor"), l),
        branch(
            not(var(found)),
            vec![
                assign(base, e.base(var(current)), l),
                assign(slot, e.decoded(add(var(base), int(3))), l),
                assign(generation, e.generation(add(var(base), int(4))), l),
                assert_stmt(
                    or(not(eq(var(generation), int(0))), eq(var(slot), int(0))),
                    l,
                ),
                assign(
                    current,
                    choose(
                        eq(var(generation), int(0)),
                        value::null(),
                        value::dynamic_value(
                            int(value::OBJECT),
                            E::Float(0.0),
                            E::Bool(false),
                            var(slot),
                            var(generation),
                        ),
                    ),
                    l,
                ),
            ],
            Vec::new(),
            l,
        ),
    ];
    ordinary.push(S::While {
        condition: and(not(var(found)), tag_is(var(current), value::OBJECT)),
        body: walk,
        line: l,
    });
    ordinary.extend([
        assign(
            setter,
            and(var(found), eq(field(var(desc), "present"), int(60))),
            l,
        ),
        assign(
            reject,
            choose(
                var(setter),
                tag_is(field(var(desc), "set"), value::UNDEFINED),
                or(
                    and(var(found), not(field(var(desc), "writable"))),
                    and(not(var(is_own)), not(var(header.extensible))),
                ),
            ),
            l,
        ),
    ]);
    let mut set = Vec::new();
    accessor_call(
        &mut e,
        (var(object), var(key), var(current)),
        field(var(desc), "set"),
        Some(var(rhs)),
        dispatch,
        called,
        &mut set,
    )?;
    set.push(matched(
        &mut e,
        called,
        values.clone(),
        value_ok,
        err,
        Vec::new(),
        vec![assign(
            out,
            result(schema.clone(), false, var(err), source_span),
            l,
        )],
    )?);
    let mut data = vec![assign(
        raw,
        call(
            h.define,
            vec![
                var(carrier),
                var(object),
                var(key),
                choose(
                    var(is_own),
                    descriptor(
                        int(1),
                        var(rhs),
                        value::undefined(),
                        value::undefined(),
                        E::Bool(false),
                        E::Bool(false),
                        E::Bool(false),
                    ),
                    data_descriptor(var(rhs), E::Bool(true), E::Bool(true), E::Bool(true)),
                ),
                field(var(reference), "strict"),
            ],
        ),
        l,
    )];
    data.push(matched(
        &mut e,
        raw,
        bools,
        ok,
        err,
        Vec::new(),
        vec![assign(
            out,
            result(schema.clone(), false, var(err), source_span),
            l,
        )],
    )?);
    let rejection = vec![branch(
        field(var(reference), "strict"),
        vec![assign(
            out,
            result(
                schema.clone(),
                false,
                call(error, vec![var(carrier)]),
                source_span,
            ),
            l,
        )],
        Vec::new(),
        l,
    )];
    ordinary.push(branch(
        var(reject),
        rejection,
        vec![branch(var(setter), set, data, l)],
        l,
    ));
    let failure = vec![assign(out, result(schema.clone(), false, call(error, vec![var(carrier)]), source_span), l)];
    let ordinary = if e.lower.array_buffer_used.get() {
        vec![branch(
            and(eq(e.decoded(e.base(var(object))), int(layout::KIND_BYTES)), not(lt(call(h.index_key, vec![var(carrier), var(key)]), int(0)))),
            failure.clone(), ordinary, l,
        )]
    } else { ordinary };
    let ordinary = if e.lower.typed_array_used() {
        use super::typed_array as view;
        let index = e.integer()?;
        let n = e.temp(T::Number, E::Float(0.0))?;
        let failure = |e: &mut Emit<'_, '_>, message| -> Result<Vec<S>, ParseError> {
            let mut body = view::error(e, message, false, err)?;
            body.push(assign(out, result(schema.clone(), false, var(err), source_span), l));
            Ok(body)
        };
        let invalid = failure(&mut e, "invalid array subscript")?;
        let unsupported = failure(&mut e, "unsupported object class")?;
        let mut write = Vec::new();
        view::write_element(&mut e, base, index, n, &mut write)?;
        let converted = vec![assign(n, view::number(&e, h.number_parser(), value_ok), l),
            assign(base, e.base(var(object)), l),
            branch(view::in_range(&e, base, index), write, invalid, l)];
        let numeric = h.numeric_default_value;
        let put = vec![
            assign(called, call(numeric, vec![var(carrier), var(rhs), E::Bool(false)]), l),
            matched(&mut e, called, values, value_ok, err, converted,
                vec![assign(out, result(schema.clone(), false, var(err), source_span), l)])?,
        ];
        let mut indexed = view_index(&mut e, h.index_key, key, index)?;
        indexed.push(branch(not(eq(var(index), int(-1))), put, unsupported, l));
        vec![branch(eq(e.decoded(e.base(var(object))), int(layout::KIND_VIEW)), indexed, ordinary, l)]
    } else { ordinary };
    body.push(branch(
        tag_is(var(object), value::OBJECT),
        ordinary,
        vec![assign(out, result(schema.clone(), false, call(error, vec![var(carrier)]), source_span), l)],
        l,
    ));
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        "__compat_es5_ref_put",
        h.put,
        vec![carrier, reference, rhs],
        vec![
            carrier_schema.clone(),
            value::reference_schema(),
            value::schema(),
        ],
        schema,
        body,
        var(out),
        l,
    );
    Ok(())
}

pub(crate) fn truth(e: &Emit<'_, '_>, payload: E) -> E {
    let tag = field(payload.clone(), "tag");
    let n = num(field(payload.clone(), "number"));
    choose(
        eq(num(tag.clone()), num(int(value::BOOLEAN))),
        field(payload.clone(), "flag"),
        choose(
            eq(num(tag.clone()), num(int(value::NUMBER))),
            and(eq(n.clone(), n.clone()), not(eq(n, E::Float(0.0)))),
            choose(
                eq(num(tag.clone()), num(int(value::OBJECT))),
                E::Bool(true),
                choose(
                    eq(num(tag), num(int(value::STRING))),
                    not(eq(e.read(add(e.base(payload), int(2))), E::Float(0.0))),
                    E::Bool(false),
                ),
            ),
        ),
    )
}

/// Place only this validator's declared locals at their original first writes.
/// Sibling/loop definitions never escape: every read must already be defined
/// in its lexical dominator set. No initializer is emitted in this callee.
fn row_validator_first_definitions(
    body: &mut [S],
    schemas: &std::collections::BTreeMap<u16, T>,
    defined: &mut std::collections::BTreeSet<u16>,
    placed: &mut std::collections::BTreeSet<u16>,
) -> Result<(), &'static str> {
    fn reads_defined(expr: &E, defined: &std::collections::BTreeSet<u16>) -> bool {
        let mut valid = true;
        scalar_expr(&mut expr.clone(), &mut |expr| {
            if let E::Var(index) | E::MoveVar(index) = expr {
                valid &= defined.contains(index);
            }
        });
        valid
    }
    for stmt in body {
        match stmt {
            S::Assign {
                kind: vm::AssignmentKind::Set,
                index,
                expr,
                line,
            } => {
                if !reads_defined(expr, defined) {
                    return Err("row validator read lacks a dominating definition");
                }
                if !defined.contains(index) {
                    let schema = schemas.get(index).ok_or("unknown row validator local")?;
                    if !placed.insert(*index) {
                        return Err("row validator local has sibling first definitions");
                    }
                    defined.insert(*index);
                    *stmt = S::Let {
                        index: *index,
                        declared_schema: Some(schema.clone()),
                        expr: expr.clone(),
                        line: *line,
                    };
                }
            }
            S::Expr { expr, .. } => {
                if !reads_defined(expr, defined) {
                    return Err("row validator assertion lacks a dominating definition");
                }
            }
            S::IfElse {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                if !reads_defined(condition, defined) {
                    return Err("row validator branch lacks a dominating definition");
                }
                row_validator_first_definitions(
                    then_branch,
                    schemas,
                    &mut defined.clone(),
                    placed,
                )?;
                row_validator_first_definitions(
                    else_branch,
                    schemas,
                    &mut defined.clone(),
                    placed,
                )?;
            }
            S::While {
                condition, body, ..
            } => {
                if !reads_defined(condition, defined) {
                    return Err("row validator loop lacks a dominating definition");
                }
                row_validator_first_definitions(body, schemas, &mut defined.clone(), placed)?;
            }
            _ => return Err("unexpected row validator statement"),
        }
    }
    Ok(())
}

/// The ordinary descriptor-row validator: the shared checked-row producer with
/// no key snapshot, no collector cache and no consumer tails. It proves the row
/// and returns the validated String key base as Int, so a HasProperty scan that
/// only compares the compiler-known key performs no map allocation for the row
/// it inspects.
fn append_row_validator(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    index: u16,
) -> Result<(), ParseError> {
    let carrier = lower.slot(span)?;
    let row = lower.slot(span)?;
    let mut e = Emit::new(lower, span, carrier)?;
    let mut body = Vec::new();
    let scalars = emit_checked_row(
        &mut e,
        row,
        &mut body,
        None,
        None,
        |_| Vec::new(),
        |_| Vec::new(),
    )?;
    let schemas = e
        .declarations
        .iter()
        .map(|decl| match decl {
            S::Let {
                index,
                declared_schema: Some(schema),
                ..
            } => (*index, schema.clone()),
            _ => unreachable!("Emit::temp always declares a typed local"),
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut defined = std::collections::BTreeSet::from([carrier, row]);
    let mut placed = std::collections::BTreeSet::new();
    row_validator_first_definitions(&mut body, &schemas, &mut defined, &mut placed)
        .map_err(|message| e.lower.error(span, message))?;
    // The shared emitter declares exactly the 15 checked scalar locals, and the
    // returned key base is one of them: it is forwarded right after the checked
    // String identity proof and never aliases a payload scratch slot.
    let complete =
        schemas.len() == 15 && placed.len() == schemas.len() && defined.contains(&scalars.key_base);
    if !complete {
        return Err(e
            .lower
            .error(span, "incomplete row validator definition placement"));
    }
    declare(
        ir,
        stmts,
        "__compat_es5_descriptor_row_validator",
        index,
        vec![carrier, row],
        vec![carrier_schema.clone(), T::Int],
        T::Int,
        body,
        var(scalars.key_base),
        e.line,
    );
    Ok(())
}

/// HasProperty for a compiler-known descriptor field can retain the real
/// matching key instead of allocating a second String node for that name.
/// It performs no guest calls or coercion; the subsequent Get remains a
/// separate operation with the original receiver and a newly refreshed base.
fn literal_key(
    e: &mut Emit<'_, '_>,
    attrs: E,
    name: &str,
    key: u16,
    validator: u16,
    body: &mut Vec<S>,
) -> Result<u16, ParseError> {
    let l = e.line;
    let current = e.boxed()?;
    let base = e.integer()?;
    let row = e.integer()?;
    let cursor = e.integer()?;
    let found = e.boolean()?;
    let key_base = e.integer()?;
    let candidate = e.boxed()?;
    let slot = e.integer()?;
    let generation = e.integer()?;
    body.extend([
        assign(current, attrs, l),
        assign(found, E::Bool(false), l),
        assign(key, value::undefined(), l),
    ]);
    let mut step = vec![assign(base, e.base(var(current)), l)];
    let header = property_header(e, base, &mut step)?;
    let mut scan = vec![assign(
        row,
        add(add(var(base), var(header.header)), mul(int(7), var(cursor))),
        l,
    )];
    // The private validator returns the row's validated String key base
    // directly, so this consumer keeps the candidate formation and its own
    // UTF-16 reads but declares no discarded descriptor prefix and performs no
    // second checked_base of the same key node.
    scan.push(assign(
        key_base,
        call(validator, vec![var(e.carrier), var(row)]),
        l,
    ));
    scan.push(assign(
        candidate,
        value::dynamic_value(
            int(value::STRING),
            E::Float(0.0),
            E::Bool(false),
            e.decoded(var(row)),
            e.generation(add(var(row), int(1))),
        ),
        l,
    ));
    let units: Vec<u16> = name.encode_utf16().collect();
    let mut same = eq(
        e.read(add(var(key_base), int(2))),
        E::Float(units.len() as f64),
    );
    for (i, unit) in units.into_iter().enumerate() {
        same = and(
            same,
            eq(
                e.read(add(var(key_base), int(3 + i as i64))),
                E::Float(f64::from(unit)),
            ),
        );
    }
    scan.push(branch(
        same,
        vec![
            assign(key, var(candidate), l),
            assign(found, E::Bool(true), l),
        ],
        Vec::new(),
        l,
    ));
    scan.push(assign(cursor, add(var(cursor), int(1)), l));
    step.push(assign(cursor, int(0), l));
    step.push(S::While {
        condition: and(not(var(found)), lt(var(cursor), var(header.count))),
        body: scan,
        line: l,
    });
    step.extend([
        assign(slot, e.decoded(add(var(base), int(3))), l),
        assign(generation, e.generation(add(var(base), int(4))), l),
        assert_stmt(
            or(not(eq(var(generation), int(0))), eq(var(slot), int(0))),
            l,
        ),
        assign(
            current,
            choose(
                eq(var(generation), int(0)),
                value::null(),
                value::dynamic_value(
                    int(value::OBJECT),
                    E::Float(0.0),
                    E::Bool(false),
                    var(slot),
                    var(generation),
                ),
            ),
            l,
        ),
    ]);
    body.push(S::While {
        condition: and(not(var(found)), tag_is(var(current), value::OBJECT)),
        body: step,
        line: l,
    });
    Ok(found)
}

fn append_to_descriptor(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    h: ObjectHelpers,
) -> Result<(), ParseError> {
    let carrier = lower.slot(span)?;
    let attrs = lower.slot(span)?;
    let heap = lower.heap_ref(span)?.helpers;
    let error_fn = lower.intrinsics.expect("intrinsics").type_error;
    let mut e = Emit::new(lower, span, carrier)?;
    let l = e.line;
    let source_span = e.lower.ir_span(span);
    let schema = result_schema(descriptor_schema());
    let values = result_schema(value::schema());
    let out = e.temp(
        schema.clone(),
        result(schema.clone(), true, empty_descriptor(), source_span),
    )?;
    let frame = e.integer()?;
    let active = e.boolean()?;
    let p = e.integer()?;
    let key = e.boxed()?;
    let got = e.boxed()?;
    let err = e.boxed()?;
    let payload = e.boxed()?;
    let get = e.boxed()?;
    let set = e.boxed()?;
    let writable = e.boolean()?;
    let enumerable = e.boolean()?;
    let configurable = e.boolean()?;
    let raw = e.temp(
        values.clone(),
        result(values.clone(), true, value::undefined(), source_span),
    )?;
    let mut body = vec![
        assign(frame, call(heap.root_enter, vec![var(carrier), int(8)]), l),
        e.root(var(frame), int(0), var(attrs))?,
        assign(active, tag_is(var(attrs), value::OBJECT), l),
    ];
    for (name, bit, target, root_entry) in [
        ("enumerable", 16, enumerable, None),
        ("configurable", 32, configurable, None),
        ("value", 1, payload, Some(1)),
        ("writable", 2, writable, None),
        ("get", 4, get, Some(2)),
        ("set", 8, set, Some(3)),
    ] {
        let mut step = Vec::new();
        let exists = literal_key(&mut e, var(attrs), name, key, h.api + 1, &mut step)?;
        step.push(e.root(var(frame), int(4), var(key))?);
        let mut present = vec![assign(
            raw,
            call(
                h.get,
                vec![
                    var(carrier),
                    value::reference(
                        var(attrs),
                        field(var(key), "ref_slot"),
                        field(var(key), "generation"),
                        E::Bool(false),
                    ),
                ],
            ),
            l,
        )];
        let mut success = vec![
            assign(
                target,
                if root_entry.is_some() {
                    var(got)
                } else {
                    truth(&e, var(got))
                },
                l,
            ),
            assign(p, add(var(p), int(bit)), l),
        ];
        if let Some(entry) = root_entry {
            success.push(e.root(var(frame), int(entry), var(target))?);
        }
        if name == "get" || name == "set" {
            let base = e.integer()?;
            let valid = e.boolean()?;
            success.push(assign(valid, tag_is(var(target), value::UNDEFINED), l));
            success.push(branch(
                tag_is(var(target), value::OBJECT),
                vec![
                    assign(base, e.base(var(target)), l),
                    assign(
                        valid,
                        eq(e.decoded(var(base)), int(layout::KIND_FUNCTION)),
                        l,
                    ),
                ],
                Vec::new(),
                l,
            ));
            success.push(branch(
                not(var(valid)),
                vec![
                    assign(active, E::Bool(false), l),
                    assign(err, call(error_fn, vec![var(carrier)]), l),
                    e.root(var(frame), int(5), var(err))?,
                    assign(out, result(schema.clone(), false, var(err), source_span), l),
                ],
                Vec::new(),
                l,
            ));
        }
        let failure = vec![
            e.root(var(frame), int(5), var(err))?,
            assign(active, E::Bool(false), l),
            assign(out, result(schema.clone(), false, var(err), source_span), l),
        ];
        present.push(matched(
            &mut e,
            raw,
            values.clone(),
            got,
            err,
            success,
            failure,
        )?);
        step.push(branch(var(exists), present, Vec::new(), l));
        body.push(branch(var(active), step, Vec::new(), l));
    }
    // Presence, not payload, decides whether explicitly Undefined get/set
    // conflicts with an explicitly supplied data field.
    let complete = e.temp(descriptor_schema(), empty_descriptor())?;
    let mut finish = vec![assign(
        complete,
        descriptor(
            var(p),
            var(payload),
            var(get),
            var(set),
            var(writable),
            var(enumerable),
            var(configurable),
        ),
        l,
    )];
    let bits = presence(&mut e, var(complete), &mut finish)?;
    let invalid = and(
        or(var(bits[0]), var(bits[1])),
        or(var(bits[2]), var(bits[3])),
    );
    finish.push(branch(
        invalid,
        vec![
            assign(err, call(error_fn, vec![var(carrier)]), l),
            e.root(var(frame), int(5), var(err))?,
            assign(out, result(schema.clone(), false, var(err), source_span), l),
        ],
        vec![assign(
            out,
            result(schema.clone(), true, var(complete), source_span),
            l,
        )],
        l,
    ));
    body.push(branch(var(active), finish, Vec::new(), l));
    // Invalid receiver is a semantic TypeError, distinct from corrupt rows.
    body.push(branch(
        not(tag_is(var(attrs), value::OBJECT)),
        vec![
            assign(err, call(error_fn, vec![var(carrier)]), l),
            e.root(var(frame), int(5), var(err))?,
            assign(out, result(schema.clone(), false, var(err), source_span), l),
        ],
        Vec::new(),
        l,
    ));
    body.push(e.effect(heap.root_leave, vec![var(carrier), var(frame)])?);
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        "__compat_es5_to_property_descriptor",
        h.to_desc,
        vec![carrier, attrs],
        vec![carrier_schema.clone(), value::schema()],
        schema,
        body,
        var(out),
        l,
    );
    Ok(())
}

fn append_from_descriptor(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    h: ObjectHelpers,
) -> Result<(), ParseError> {
    let carrier = lower.slot(span)?;
    let own = lower.slot(span)?;
    let heap = lower.heap_ref(span)?.helpers;
    let intrinsic = lower.intrinsics.expect("intrinsics");
    let mut e = Emit::new(lower, span, carrier)?;
    let l = e.line;
    let source_span = e.lower.ir_span(span);
    let schema = result_schema(value::schema());
    let out = e.temp(
        schema.clone(),
        result(schema.clone(), true, value::undefined(), source_span),
    )?;
    let frame = e.integer()?;
    let object = e.boxed()?;
    let desc = e.temp(descriptor_schema(), empty_descriptor())?;
    let mut body = vec![assign(desc, field(var(own), "descriptor"), l)];
    let mut make = vec![assign(
        frame,
        call(heap.root_enter, vec![var(carrier), int(8)]),
        l,
    )];
    for (i, name) in ["value", "get", "set"].into_iter().enumerate() {
        make.push(e.root(var(frame), int(2 + i as i64), field(var(desc), name))?);
    }
    make.push(assign(
        object,
        call(
            intrinsic.allocate,
            vec![
                var(carrier),
                int(3),
                call(intrinsic.root, vec![var(carrier), int(0)]),
                int(4),
                int(0),
                int(0), int(0),
                var(frame),
                int(0),
            ],
        ),
        l,
    ));
    let mut data = Vec::new();
    let mut accessor = Vec::new();
    for (names, branch) in [
        (
            ["value", "writable", "enumerable", "configurable"],
            &mut data,
        ),
        (["get", "set", "enumerable", "configurable"], &mut accessor),
    ] {
        for (i, name) in names.into_iter().enumerate() {
            let key = super::intrinsics::string(&mut e, name, frame, 1, branch)?;
            let payload = if matches!(name, "writable" | "enumerable" | "configurable") {
                value::boolean(field(var(desc), name))
            } else {
                field(var(desc), name)
            };
            super::intrinsics::trusted_data(
                &mut e,
                var(object),
                i as i64,
                var(key),
                payload,
                (true, true, true),
                branch,
            )?;
        }
    }
    make.push(branch(
        eq(field(var(desc), "present"), int(51)),
        data,
        accessor,
        l,
    ));
    make.push(assign(
        out,
        result(schema.clone(), true, var(object), source_span),
        l,
    ));
    make.push(e.effect(heap.root_leave, vec![var(carrier), var(frame)])?);
    body.push(branch(field(var(own), "found"), make, Vec::new(), l));
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        "__compat_es5_from_property_descriptor",
        h.from_desc,
        vec![carrier, own],
        vec![carrier_schema.clone(), own_schema()],
        schema,
        body,
        var(out),
        l,
    );
    Ok(())
}

pub(crate) const PAIR: &str = "__CompatEs5DescriptorPair";
pub(crate) fn pair_schema() -> T {
    T::Named(PAIR.into(), Vec::new())
}
pub(crate) fn pair(key: E, desc: E) -> E {
    record(vec![("key", key), ("descriptor", desc)])
}
pub(crate) fn pair_declaration() -> vm::compiler::ir::StructDecl {
    vm::compiler::ir::StructDecl {
        name: PAIR.into(),
        type_params: Vec::new(),
        body_schema: T::Object(std::collections::HashMap::from([
            ("key".into(), value::schema()),
            ("descriptor".into(), descriptor_schema()),
        ])),
        origin: vm::compiler::ir::StructDeclOrigin::Guest,
    }
}

fn append_define_properties(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    h: ObjectHelpers,
) -> Result<(), ParseError> {
    let carrier = lower.slot(span)?;
    let target = lower.slot(span)?;
    let props = lower.slot(span)?;
    let heap = lower.heap_ref(span)?.helpers;
    let error_fn = lower.intrinsics.expect("intrinsics").type_error;
    let mut e = Emit::new(lower, span, carrier)?;
    let l = e.line;
    let source_span = e.lower.ir_span(span);
    let schema = result_schema(value::schema());
    let descs = result_schema(descriptor_schema());
    let bools = result_schema(T::Bool);
    let out = e.temp(
        schema.clone(),
        result(schema.clone(), true, var(target), source_span),
    )?;
    let frame = e.integer()?;
    let active = e.boolean()?;
    let base = e.integer()?;
    let props_base = e.integer()?;
    let initial_count = e.integer()?;
    let capacity = e.temp(T::Int, int(8))?;
    let row = e.integer()?;
    let cursor = e.integer()?;
    let count = e.integer()?;
    let length = e.integer()?;
    let pairs = e.temp(
        T::Array(Box::new(pair_schema())),
        call(B::ArrayNew.call_index(), Vec::new()),
    )?;
    let item = e.temp(pair_schema(), pair(value::undefined(), empty_descriptor()))?;
    let key = e.boxed()?;
    let value_slot = e.boxed()?;
    let error = e.boxed()?;
    let desc = e.temp(descriptor_schema(), empty_descriptor())?;
    let ok = e.boolean()?;
    let get_raw = e.temp(
        schema.clone(),
        result(schema.clone(), true, value::undefined(), source_span),
    )?;
    let desc_raw = e.temp(
        descs.clone(),
        result(descs.clone(), true, empty_descriptor(), source_span),
    )?;
    let define_raw = e.temp(
        bools.clone(),
        result(bools.clone(), true, E::Bool(true), source_span),
    )?;
    // The reservation bound comes only from the props metadata, and only after
    // the existing target/props tag gate succeeds. The target's ordinary
    // kind/header is still never inspected early: an Array target with no
    // enumerable props returns unchanged below. Both actual arguments are
    // already rooted by the public call lowering and the Object adapter frame,
    // so this prepass reads only caller-protected values and carries a bounded
    // Int row count across the allocation, never a physical base.
    let mut prepass = vec![assign(props_base, e.base(var(props)), l)];
    let reserved = property_header(&mut e, props_base, &mut prepass)?;
    prepass.extend([
        assign(initial_count, var(reserved.count), l),
        assign(capacity, add(int(8), mul(int(4), var(initial_count))), l),
    ]);
    let mut body = vec![
        assign(
            active,
            and(
                tag_is(var(target), value::OBJECT),
                tag_is(var(props), value::OBJECT),
            ),
            l,
        ),
        branch(var(active), prepass, Vec::new(), l),
        assign(
            frame,
            call(heap.root_enter, vec![var(carrier), var(capacity)]),
            l,
        ),
        e.root(var(frame), int(0), var(target))?,
        e.root(var(frame), int(1), var(props))?,
    ];
    let fail = |e: &mut Emit<'_, '_>| -> Result<Vec<S>, ParseError> {
        Ok(vec![
            e.root(var(frame), int(7), var(error))?,
            assign(active, E::Bool(false), l),
            assign(
                out,
                result(schema.clone(), false, var(error), source_span),
                l,
            ),
        ])
    };
    let mut snapshot = vec![assign(base, e.base(var(props)), l)];
    let header = property_header(&mut e, base, &mut snapshot)?;
    // Local fail-closed capacity certificate: the fresh post-entry snapshot can
    // only be as large as the bounded prepass value, because `root_enter` may
    // collect/resize but never runs a guest callback or changes a live row
    // count. Every retained key is then written below 8 + 4 * count.
    snapshot.push(assert_stmt(
        not(lt(var(initial_count), var(header.count))),
        l,
    ));
    let mut scan = vec![assign(
        row,
        add(add(var(base), var(header.header)), mul(int(7), var(cursor))),
        l,
    )];
    let (row_desc, _) = decode_row(&mut e, row, &mut scan)?;
    let retain = vec![
        assign(
            key,
            value::dynamic_value(
                int(value::STRING),
                E::Float(0.0),
                E::Bool(false),
                e.decoded(var(row)),
                e.generation(add(var(row), int(1))),
            ),
            l,
        ),
        e.root(var(frame), add(int(8), mul(int(4), var(count))), var(key))?,
        assign(
            pairs,
            call(
                B::ArrayPush.call_index(),
                vec![var(pairs), pair(var(key), empty_descriptor())],
            ),
            l,
        ),
        assign(count, add(var(count), int(1)), l),
    ];
    scan.push(branch(
        field(var(row_desc), "enumerable"),
        retain,
        Vec::new(),
        l,
    ));
    scan.push(assign(cursor, add(var(cursor), int(1)), l));
    snapshot.push(S::While {
        condition: lt(var(cursor), var(header.count)),
        body: scan,
        line: l,
    });
    body.push(branch(
        var(active),
        snapshot,
        vec![
            assign(error, call(error_fn, vec![var(carrier)]), l),
            e.root(var(frame), int(7), var(error))?,
            assign(
                out,
                result(schema.clone(), false, var(error), source_span),
                l,
            ),
        ],
        l,
    ));
    body.extend([assign(cursor, int(0), l), assign(length, var(count), l)]);
    let get_item = call(
        B::Get.call_index(),
        vec![E::Borrow(Box::new(var(pairs))), var(cursor)],
    );
    let mut convert = vec![
        assign(item, get_item.clone(), l),
        assign(key, field(var(item), "key"), l),
        assign(
            get_raw,
            call(
                h.get,
                vec![
                    var(carrier),
                    value::reference(
                        var(props),
                        field(var(key), "ref_slot"),
                        field(var(key), "generation"),
                        E::Bool(false),
                    ),
                ],
            ),
            l,
        ),
    ];
    let failure = fail(&mut e)?;
    let success = vec![e.root(var(frame), int(3), var(value_slot))?];
    convert.push(matched(
        &mut e,
        get_raw,
        schema.clone(),
        value_slot,
        error,
        success,
        failure,
    )?);
    let mut convert_desc = vec![assign(
        desc_raw,
        call(h.to_desc, vec![var(carrier), var(value_slot)]),
        l,
    )];
    let mut success = Vec::new();
    for (offset, name) in [(1, "value"), (2, "get"), (3, "set")] {
        success.push(e.root(
            var(frame),
            add(add(int(8), mul(int(4), var(cursor))), int(offset)),
            field(var(desc), name),
        )?);
    }
    success.push(assign(
        pairs,
        call(
            B::Set.call_index(),
            vec![var(pairs), var(cursor), pair(var(key), var(desc))],
        ),
        l,
    ));
    let failure = fail(&mut e)?;
    convert_desc.push(matched(
        &mut e, desc_raw, descs, desc, error, success, failure,
    )?);
    convert.push(branch(var(active), convert_desc, Vec::new(), l));
    convert.push(assign(cursor, add(var(cursor), int(1)), l));
    body.push(S::While {
        condition: and(var(active), lt(var(cursor), var(length))),
        body: convert,
        line: l,
    });
    body.push(assign(cursor, int(0), l));
    let mut define = vec![
        assign(item, get_item, l),
        assign(key, field(var(item), "key"), l),
        assign(desc, field(var(item), "descriptor"), l),
        assign(
            define_raw,
            call(
                h.define,
                vec![
                    var(carrier),
                    var(target),
                    var(key),
                    var(desc),
                    E::Bool(true),
                ],
            ),
            l,
        ),
    ];
    let failure = fail(&mut e)?;
    define.push(matched(
        &mut e,
        define_raw,
        bools,
        ok,
        error,
        Vec::new(),
        failure,
    )?);
    define.push(assign(cursor, add(var(cursor), int(1)), l));
    body.push(S::While {
        condition: and(var(active), lt(var(cursor), var(length))),
        body: define,
        line: l,
    });
    body.push(e.effect(heap.root_leave, vec![var(carrier), var(frame)])?);
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        "__compat_es5_define_properties",
        h.define_properties,
        vec![carrier, target, props],
        vec![carrier_schema.clone(), value::schema(), value::schema()],
        schema,
        body,
        var(out),
        l,
    );
    Ok(())
}

/// Private decimal-index String producer `(Carrier, index, frame, entry)`.
/// The digits are computed twice (length, then right to left) so no managed
/// allocation is open while the pending String node exists.
#[allow(clippy::too_many_arguments)]
fn append_index_string(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    helper: u16,
) -> Result<(), ParseError> {
    let carrier = lower.slot(span)?;
    let index = lower.slot(span)?;
    let frame = lower.slot(span)?;
    let entry = lower.slot(span)?;
    let mut e = Emit::new(lower, span, carrier)?;
    let l = e.line;
    let heap = e.lower.heap_ref(span)?.helpers;
    let n = e.integer()?;
    let digits = e.integer()?;
    let pos = e.integer()?;
    let digit = e.integer()?;
    let base = e.integer()?;
    let alloc = e.temp(
        value::allocation_schema(),
        value::allocation(int(0), int(0), int(0)),
    )?;
    let output = e.boxed()?;
    let body = vec![
        assert_stmt(
            and(
                not(lt(var(index), int(0))),
                not(lt(int(65_536), var(index))),
            ),
            l,
        ),
        assign(n, var(index), l),
        assign(digits, int(1), l),
        S::While {
            condition: not(lt(var(n), int(10))),
            body: vec![
                assign(n, E::Div(Box::new(var(n)), Box::new(int(10))), l),
                assign(digits, add(var(digits), int(1)), l),
            ],
            line: l,
        },
        assign(
            alloc,
            call(
                heap.node_begin,
                vec![
                    var(carrier),
                    add(int(3), var(digits)),
                    int(layout::KIND_STRING),
                ],
            ),
            l,
        ),
        assign(base, field(var(alloc), "base"), l),
        e.write(add(var(base), int(2)), var(digits))?,
        assign(pos, var(digits), l),
        assign(n, var(index), l),
        S::While {
            condition: not(eq(var(pos), int(0))),
            body: vec![
                assign(digit, E::Mod(Box::new(var(n)), Box::new(int(10))), l),
                e.write(
                    add(add(var(base), int(3)), sub(var(pos), int(1))),
                    add(var(digit), int(48)),
                )?,
                assign(n, E::Div(Box::new(var(n)), Box::new(int(10))), l),
                assign(pos, sub(var(pos), int(1)), l),
            ],
            line: l,
        },
        assign(
            output,
            call(
                heap.node_publish,
                vec![
                    var(carrier),
                    field(var(alloc), "slot"),
                    field(var(alloc), "generation"),
                    var(base),
                    var(frame),
                    var(entry),
                ],
            ),
            l,
        ),
    ];
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        "__compat_es5_object_index_string",
        helper,
        vec![carrier, index, frame, entry],
        vec![carrier_schema.clone(), T::Int, T::Int, T::Int],
        value::schema(),
        body,
        var(output),
        l,
    );
    Ok(())
}

/// Public `Object.keys` producer `(Carrier, target, owner frame, owner entry)`.
/// Ordinary objects and functions enumerate their own active enumerable String
/// keys in stored row order without running any accessor; dense arrays produce
/// fresh decimal index Strings. The result is a real kind-6 String array whose
/// complete extent is published and rooted before the first key String exists.
#[allow(clippy::too_many_arguments)]
fn append_keys(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    helper: u16,
    index_string: u16,
) -> Result<(), ParseError> {
    let carrier = lower.slot(span)?;
    let target = lower.slot(span)?;
    let owner_frame = lower.slot(span)?;
    let owner_entry = lower.slot(span)?;
    let mut e = Emit::new(lower, span, carrier)?;
    let l = e.line;
    let heap = e.lower.heap_ref(span)?.helpers;
    let base = e.integer()?;
    let kind = e.integer()?;
    let count = e.integer()?;
    let cells = e.integer()?;
    let cursor = e.integer()?;
    let kept = e.integer()?;
    let row = e.integer()?;
    let out_base = e.integer()?;
    let key_slot = e.integer()?;
    let key_gen = e.integer()?;
    let alloc = e.temp(
        value::allocation_schema(),
        value::allocation(int(0), int(0), int(0)),
    )?;
    let output = e.boxed()?;
    let key = e.boxed()?;
    // The dense writer returns Null, so its result never lands in an Int sink.
    let put = e.temp(T::Null, E::Null)?;
    let mut body = Vec::new();
    let own_frame = e.integer()?;
    body.push(assign(
        own_frame,
        call(heap.root_enter, vec![var(carrier), int(3)]),
        l,
    ));
    body.push(e.root(var(own_frame), int(0), var(target))?);
    body.push(assign(base, e.base(var(target)), l));
    body.push(assign(kind, e.decoded(var(base)), l));
    body.push(assert_stmt(object_like(var(kind)), l));
    let dense = or(
        eq(var(kind), int(layout::KIND_ARRAY)),
        eq(var(kind), int(layout::KIND_TAGGED_ARRAY)),
    );
    let dense_prepare = vec![
        assign(
            count,
            call(
                e.decode_u32,
                vec![call(heap.array_length, vec![var(carrier), var(base)])],
            ),
            l,
        ),
        assign(
            cells,
            add(int(layout::ARRAY_HEADER_CELLS), mul(int(5), var(count))),
            l,
        ),
    ];
    let indices = if e.lower.typed_array_used() {
        or(dense.clone(), eq(var(kind), int(layout::KIND_VIEW)))
    } else { dense.clone() };
    let dense_prepare = if e.lower.typed_array_used() {
        vec![branch(eq(var(kind), int(layout::KIND_VIEW)), vec![
            assign(count, e.decoded(add(var(base), int(layout::VIEW_LENGTH))), l),
            assign(cells, add(int(layout::ARRAY_HEADER_CELLS), mul(int(5), var(count))), l),
        ], dense_prepare, l)]
    } else { dense_prepare };
    let mut ordinary_prepare = Vec::new();
    let header = property_header(&mut e, base, &mut ordinary_prepare)?;
    ordinary_prepare.push(assign(count, var(header.count), l));
    ordinary_prepare.push(assign(
        cells,
        add(int(layout::ARRAY_HEADER_CELLS), mul(int(5), var(count))),
        l,
    ));
    body.push(branch(indices.clone(), dense_prepare, ordinary_prepare, l));
    // The complete output extent exists (as Undefined entries) and is rooted
    // before any key String allocation, so an interval collection inside the
    // key loop can only trace a fully initialized array.
    body.push(assign(
        alloc,
        call(
            heap.node_begin,
            vec![var(carrier), var(cells), int(layout::KIND_TAGGED_ARRAY)],
        ),
        l,
    ));
    body.push(assign(out_base, field(var(alloc), "base"), l));
    // The dense result is a real Array: it takes the same private realm Array
    // prototype as every source packed/tagged array.
    let proto = e.boxed()?;
    body.push(assign(
        proto,
        call(
            e.lower.intrinsics.expect("intrinsics").array_proto,
            vec![var(carrier)],
        ),
        l,
    ));
    for (offset, payload) in [
        (2i64, var(count)),
        (3, num(value::field(proto, "ref_slot"))),
        (4, num(value::field(proto, "generation"))),
        (
            layout::ARRAY_PRESENT_OFFSET,
            add(var(count), int(1)),
        ),
    ] {
        body.push(e.write(add(var(out_base), int(offset)), payload)?);
    }
    body.push(assign(cursor, int(0), l));
    body.push(S::While {
        condition: lt(var(cursor), mul(int(5), var(count))),
        body: vec![
            e.write(
                add(
                    var(out_base),
                    add(int(layout::ARRAY_HEADER_CELLS), var(cursor)),
                ),
                int(0),
            )?,
            assign(cursor, add(var(cursor), int(1)), l),
        ],
        line: l,
    });
    body.push(assign(
        output,
        call(
            heap.node_publish,
            vec![
                var(carrier),
                field(var(alloc), "slot"),
                field(var(alloc), "generation"),
                var(out_base),
                var(owner_frame),
                var(owner_entry),
            ],
        ),
        l,
    ));
    body.push(assign(cursor, int(0), l));
    body.push(assign(kept, int(0), l));
    let dense_fill = vec![
        assign(
            key,
            call(
                index_string,
                vec![var(carrier), var(cursor), var(own_frame), int(2)],
            ),
            l,
        ),
        assign(out_base, e.base(var(output)), l),
        assign(
            put,
            call(
                heap.array_put,
                vec![
                    var(carrier),
                    var(out_base),
                    var(cursor),
                    var(key),
                    field(var(output), "ref_slot"),
                    field(var(output), "generation"),
                ],
            ),
            l,
        ),
        assign(kept, add(var(kept), int(1)), l),
        assign(cursor, add(var(cursor), int(1)), l),
    ];
    let mut ordinary_fill = vec![assign(
        row,
        add(add(var(base), var(header.header)), mul(int(7), var(cursor))),
        l,
    )];
    let (desc, _) = decode_row(&mut e, row, &mut ordinary_fill)?;
    ordinary_fill.push(assign(out_base, e.base(var(output)), l));
    let take = vec![
        assign(key_slot, e.decoded(var(row)), l),
        assign(key_gen, e.generation(add(var(row), int(1))), l),
        assign(
            key,
            value::dynamic_value(
                int(value::STRING),
                E::Float(0.0),
                E::Bool(false),
                var(key_slot),
                var(key_gen),
            ),
            l,
        ),
        assign(
            put,
            call(
                heap.array_put,
                vec![
                    var(carrier),
                    var(out_base),
                    var(kept),
                    var(key),
                    field(var(output), "ref_slot"),
                    field(var(output), "generation"),
                ],
            ),
            l,
        ),
        assign(kept, add(var(kept), int(1)), l),
    ];
    ordinary_fill.push(branch(field(var(desc), "enumerable"), take, Vec::new(), l));
    ordinary_fill.push(assign(cursor, add(var(cursor), int(1)), l));
    body.push(branch(
        indices,
        vec![S::While {
            condition: lt(var(cursor), var(count)),
            body: dense_fill,
            line: l,
        }],
        vec![S::While {
            condition: lt(var(cursor), var(count)),
            body: ordinary_fill,
            line: l,
        }],
        l,
    ));
    body.push(assign(out_base, e.base(var(output)), l));
    body.push(e.write(add(var(out_base), int(2)), var(kept))?);
    body.push(e.write(
        add(var(out_base), int(layout::ARRAY_PRESENT_OFFSET)),
        add(var(kept), int(1)),
    )?);
    body.push(e.effect(heap.root_leave, vec![var(carrier), var(own_frame)])?);
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        "__compat_es5_object_keys",
        helper,
        vec![carrier, target, owner_frame, owner_entry],
        vec![carrier_schema.clone(), value::schema(), T::Int, T::Int],
        value::schema(),
        body,
        var(output),
        l,
    );
    Ok(())
}

/// Canonical decimal index of a managed String key, or -1 when the key is not
/// a canonical array-index name. Reads only: no allocation, no frame change.
fn append_index_key(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    index: u16,
) -> Result<(), ParseError> {
    let carrier = lower.slot(span)?;
    let key = lower.slot(span)?;
    let mut e = Emit::new(lower, span, carrier)?;
    let l = e.line;
    let base = e.integer()?;
    let length = e.temp(T::Number, E::Float(0.0))?;
    let value = e.integer()?;
    let unit = e.integer()?;
    let valid = e.boolean()?;
    let digit = e.boolean()?;
    let cursor = e.integer()?;
    let mut body = vec![
        assert_stmt(tag_is(var(key), value::STRING), l),
        assign(base, e.base(var(key)), l),
        assert_stmt(eq(e.decoded(var(base)), int(layout::KIND_STRING)), l),
        assign(length, e.read(add(var(base), int(2))), l),
        // Canonical index names are 1..10 decimal digits without a leading
        // zero (except "0" itself); everything else is not an index.
        assign(
            valid,
            and(
                not(lt(var(length), E::Float(1.0))),
                not(lt(E::Float(10.0), var(length))),
            ),
            l,
        ),
        assign(value, int(0), l),
        assign(cursor, int(0), l),
    ];
    body.push(S::While {
        condition: and(var(valid), lt(num(var(cursor)), var(length))),
        body: vec![
            assign(
                unit,
                call(
                    e.decode,
                    vec![read_string_unit(e.read, e.carrier, base, cursor)],
                ),
                l,
            ),
            assign(
                digit,
                and(not(lt(var(unit), int(48))), not(lt(int(57), var(unit)))),
                l,
            ),
            branch(
                not(var(digit)),
                vec![assign(valid, E::Bool(false), l)],
                vec![branch(
                    and(
                        eq(var(cursor), int(0)),
                        and(lt(E::Float(1.0), var(length)), eq(var(unit), int(48))),
                    ),
                    vec![assign(valid, E::Bool(false), l)],
                    vec![assign(
                        value,
                        add(mul(var(value), int(10)), sub(var(unit), int(48))),
                        l,
                    )],
                    l,
                )],
                l,
            ),
            assign(cursor, add(var(cursor), int(1)), l),
        ],
        line: l,
    });
    body.push(assign(value, choose(var(valid), var(value), int(-1)), l));
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        "__compat_es5_index_key",
        index,
        vec![carrier, key],
        vec![carrier_schema.clone(), value::schema()],
        T::Int,
        body,
        var(value),
        l,
    );
    Ok(())
}

/// True when the managed String key is exactly `length`.
fn key_is_length(e: &Emit<'_, '_>, key_base: u16) -> E {
    key_is_ascii(e, key_base, "length")
}

/// Canonical signed decimal names are index attempts too. A negative name or
/// a decimal too large for the ordinary index parser cannot address a byte.
fn view_index(e: &mut Emit<'_, '_>, index_key: u16, key: u16, index: u16) -> Result<Vec<S>, ParseError> {
    let l = e.line;
    let base = e.integer()?;
    let len = e.integer()?;
    let pos = e.integer()?;
    let digit = e.integer()?;
    let valid = e.boolean()?;
    let unit = |pos| call(e.decode, vec![read_string_unit(e.read, e.carrier, base, pos)]);
    let scan = vec![
        assign(pos, choose(eq(e.decoded(add(var(base), int(layout::STRING_HEADER_CELLS))), int(45)), int(1), int(0)), l),
        assign(valid, lt(var(pos), var(len)), l),
        branch(and(var(valid), lt(add(var(pos), int(1)), var(len))),
            vec![assign(valid, not(eq(unit(pos), int(48))), l)], Vec::new(), l),
        S::While {
            condition: and(var(valid), lt(var(pos), var(len))),
            body: vec![assign(digit, unit(pos), l),
                assign(valid, and(not(lt(var(digit), int(48))), not(lt(int(57), var(digit)))), l),
                assign(pos, add(var(pos), int(1)), l)],
            line: l,
        },
        branch(var(valid), vec![assign(index, int(-2), l)], Vec::new(), l),
    ];
    Ok(vec![assign(index, call(index_key, vec![var(e.carrier), var(key)]), l),
        branch(eq(var(index), int(-1)), vec![
            assign(base, e.base(var(key)), l),
            assign(len, e.decoded(add(var(base), int(2))), l),
            branch(lt(int(0), var(len)), scan, Vec::new(), l),
        ], Vec::new(), l)])
}

fn key_is_ascii(e: &Emit<'_, '_>, key_base: u16, name: &str) -> E {
    debug_assert!(!name.is_empty() && name.is_ascii());
    let mut text = None;
    for (offset, unit) in name.encode_utf16().enumerate() {
        let read = e.read(add(
            add(var(key_base), int(3)),
            int(i64::try_from(offset).expect("length unit")),
        ));
        let test = eq(read, E::Float(f64::from(unit)));
        text = Some(match text {
            Some(previous) => and(previous, test),
            None => test,
        });
    }
    and(
        eq(e.read(add(var(key_base), int(2))), E::Float(name.len() as f64)),
        text.expect("key units"),
    )
}

/// Managed ES5 String-hint `ToString` of one property key.
///
/// An Object key takes the ordinary `[[DefaultValue]]` with the existing
/// Reference Get and four-argument dispatcher: `toString` first, then
/// `valueOf`, a non-callable candidate is skipped, only an Object result
/// falls through to the second candidate, a thrown payload is propagated
/// unchanged, and two Object results are the managed TypeError. The caller
/// roots the four frame entries this helper publishes into.
#[allow(clippy::too_many_arguments)]
fn append_key_string(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    index: u16,
    dispatch: u16,
    fixed_string: u16,
    number_string: u16,
    h: ObjectHelpers,
) -> Result<(), ParseError> {
    let carrier = lower.slot(span)?;
    let input = lower.slot(span)?;
    let frame = lower.slot(span)?;
    let mut e = Emit::new(lower, span, carrier)?;
    let l = e.line;
    let sp = e.lower.ir_span(span);
    let error = e.lower.intrinsics.expect("intrinsics").type_error;
    let schema = result_schema(value::schema());
    let out = e.temp(
        schema.clone(),
        result(schema.clone(), true, value::undefined(), sp),
    )?;
    let primitive = e.boxed()?;
    let active = e.boolean()?;
    let failed = e.boolean()?;
    let key = e.boxed()?;
    let got = e.boxed()?;
    let err = e.boxed()?;
    let payload = e.boxed()?;
    let callable = e.boolean()?;
    let raw = e.temp(
        schema.clone(),
        result(schema.clone(), true, value::undefined(), sp),
    )?;
    let called = e.temp(
        value::logical_result_schema(),
        value::result_ok(value::completion_normal(), sp),
    )?;
    let completion = e.temp(value::completion_schema(), value::completion_normal())?;
    let args = e.temp(
        T::Array(Box::new(value::schema())),
        call(B::ArrayNew.call_index(), vec![]),
    )?;
    let mut body = vec![
        assign(primitive, var(input), l),
        assign(active, tag_is(var(input), value::OBJECT), l),
        assign(failed, E::Bool(false), l),
    ];
    for attempt in 0..2 {
        let mut step = Vec::new();
        let key_slot = super::intrinsics::string(
            &mut e,
            if attempt == 0 { "toString" } else { "valueOf" },
            frame,
            super::intrinsics::KEY_ENTRY + 1,
            &mut step,
        )?;
        step.push(assign(key, var(key_slot), l));
        step.push(e.root(var(frame), int(super::intrinsics::KEY_ENTRY + 1), var(key))?);
        step.push(assign(
            raw,
            call(
                h.get,
                vec![
                    var(carrier),
                    value::reference(
                        var(input),
                        field(var(key), "ref_slot"),
                        field(var(key), "generation"),
                        E::Bool(false),
                    ),
                ],
            ),
            l,
        ));
        let mut success = vec![
            e.root(var(frame), int(super::intrinsics::KEY_ENTRY + 2), var(got))?,
            assign(callable, E::Bool(false), l),
            branch(
                tag_is(var(got), value::OBJECT),
                vec![assign(
                    callable,
                    eq(e.decoded(e.base(var(got))), int(layout::KIND_FUNCTION)),
                    l,
                )],
                vec![],
                l,
            ),
        ];
        let mut invoke = vec![assign(
            called,
            call(
                dispatch,
                vec![var(carrier), var(got), var(input), var(args)],
            ),
            l,
        )];
        let good = vec![
            assign(
                payload,
                choose(
                    eq(
                        field(var(completion), "kind"),
                        int(value::COMPLETION_RETURN),
                    ),
                    field(var(completion), "value"),
                    value::undefined(),
                ),
                l,
            ),
            e.root(
                var(frame),
                int(super::intrinsics::KEY_ENTRY + 2),
                var(payload),
            )?,
            branch(
                not(tag_is(var(payload), value::OBJECT)),
                vec![
                    assign(primitive, var(payload), l),
                    assign(active, E::Bool(false), l),
                ],
                vec![],
                l,
            ),
        ];
        let fail = vec![
            e.root(var(frame), int(super::intrinsics::KEY_ENTRY + 3), var(err))?,
            assign(out, result(schema.clone(), false, var(err), sp), l),
            assign(active, E::Bool(false), l),
            assign(failed, E::Bool(true), l),
        ];
        invoke.push(matched(
            &mut e,
            called,
            value::logical_result_schema(),
            completion,
            err,
            good,
            fail.clone(),
        )?);
        success.push(branch(var(callable), invoke, vec![], l));
        step.push(matched(
            &mut e,
            raw,
            schema.clone(),
            got,
            err,
            success,
            fail,
        )?);
        body.push(branch(var(active), step, vec![], l));
    }
    // Both candidates produced Objects: one managed TypeError, no third try.
    body.push(branch(
        var(active),
        vec![
            assign(err, call(error, vec![var(carrier)]), l),
            e.root(var(frame), int(super::intrinsics::KEY_ENTRY + 3), var(err))?,
            assign(out, result(schema.clone(), false, var(err), sp), l),
            assign(active, E::Bool(false), l),
            assign(failed, E::Bool(true), l),
        ],
        vec![],
        l,
    ));
    // ToString of the resulting primitive. Strings stay; Numbers and the
    // fixed scalars reuse the accepted managed String producers.
    let mut string_arm = vec![assign(
        out,
        result(schema.clone(), true, var(primitive), sp),
        l,
    )];
    string_arm.push(assign(active, E::Bool(false), l));
    let mut number_arm = vec![assign(
        out,
        result(
            schema.clone(),
            true,
            call(
                number_string,
                vec![
                    var(carrier),
                    super::math::checked_number(var(primitive)),
                    var(frame),
                    int(super::intrinsics::KEY_ENTRY + 3),
                ],
            ),
            sp,
        ),
        l,
    )];
    number_arm.push(assign(active, E::Bool(false), l));
    let mut fixed_arm = vec![assign(
        out,
        result(
            schema.clone(),
            true,
            call(
                fixed_string,
                vec![
                    var(carrier),
                    var(primitive),
                    var(frame),
                    int(super::intrinsics::KEY_ENTRY + 3),
                ],
            ),
            sp,
        ),
        l,
    )];
    fixed_arm.push(assign(active, E::Bool(false), l));
    body.push(branch(
        not(var(failed)),
        vec![branch(
            tag_is(var(primitive), value::STRING),
            string_arm,
            vec![branch(
                tag_is(var(primitive), value::NUMBER),
                number_arm,
                fixed_arm,
                l,
            )],
            l,
        )],
        vec![],
        l,
    ));
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        "__compat_es5_to_string_key",
        index,
        vec![carrier, input, frame],
        vec![carrier_schema.clone(), value::schema(), T::Int],
        schema,
        body,
        var(out),
        l,
    );
    Ok(())
}

/// Real public `String.prototype.charCodeAt` (275), `charAt` (276),
/// `codePointAt` (277) and `concat` (278).
///
/// These codes share this helper name, index, schema, eight-entry frame and
/// the existing managed String-hint ToString / numeric DefaultValue /
/// StringNumericLiteral path. ToString(this) runs before ToNumber(position);
/// either step may invoke guest getters, valueOf/toString and throw. A
/// nullish receiver or a failed conversion returns the existing throw and
/// does not allocate a result. Code 275 keeps the original Number path: the
/// in-range unit is returned as a Number, and a negative, non-finite or
/// out-of-range position returns the canonical Number NaN box. Code 276
/// returns one managed String. An in-range call copies the UTF-16 unit into
/// a scalar before `node_begin`; a normal out-of-range call allocates an
/// empty String and does not write a unit cell. The fresh String is published
/// into frame entry 7. Entries 0..6 remain receiver, raw/primitive, text,
/// method-name key, candidate, resulting String/throw payload and TypeError.
/// Entry 3 is never overwritten. Missing or non-numeric positions still
/// convert through NaN to index zero. Code 277 returns Undefined when the
/// position is negative, non-finite or out of range, and allocates nothing.
/// An in-range call reads the first unit into the existing unit. A second
/// read happens only for a high surrogate whose next index is still in
/// range, and only that nested read tests a low surrogate and combines
/// `(first - 0xD800) * 0x400 + (second - 0xDC00) + 0x10000`. Otherwise the
/// first unit is the result. The Number box is built once after those reads.
/// Entry 7 remains the 276 publication. Code 278 is `concat` on this same
/// adapter, index, schema and eight-entry frame. It forks after the receiver
/// String is rooted at entry 2 and before the position read. Zero arguments
/// return that rooted String and do not call the bridge, publish again, or
/// allocate a String. One or more arguments use one Int accumulator and one
/// final publication into entry 7. A failed argument ToString sets a local
/// active flag, stops later conversions, and does not publish. Argument
/// expressions were already evaluated by the ordinary call. Code 279 is
/// `substring` on this same adapter. It forks beside concat after entry 2 is
/// rooted and before the legacy position read. Zero arguments return that
/// rooted String. A present start converts through number_value; the end
/// phase, including a second number_value, stays inside that Ok arm. An
/// omitted or explicit Undefined end uses the UTF-16 length and does not
/// convert. Both endpoints are Float, clamped, then decoded. The copy
/// publishes once into entry 7. Code 281 is `indexOf` on this same adapter,
/// frame and schema. It forks after entry 2 is rooted and does not enter the
/// legacy position read. ToString(searchString) always runs, including a
/// missing argument, and its UTF-16 units are copied before position
/// conversion. Only a raw missing or Undefined position skips number_value.
/// An object that converts to Undefined still projects through NaN, zero and
/// Math.trunc. Receiver units are copied after that conversion. The Int
/// result, including -1, is boxed as a Float and is not passed to decode.
/// Code 282 is `lastIndexOf` on this same adapter, frame and schema. It forks
/// beside indexOf after entry 2 is rooted and before the legacy position
/// read, and it does not enter the indexOf arm. ToString(searchString) always
/// runs, including a missing argument, and its UTF-16 units are copied before
/// position conversion. A raw missing or Undefined position writes the finite
/// 65536 sentinel and does not call number_value. An explicit position uses
/// `last_truncated`, which maps NaN to 65536 rather than 0. The original clamp
/// then bounds that Float to 0..=len, including infinities, and only the
/// clamped Float is decoded. An empty needle returns the clamped start. A
/// non-empty needle has no room only when its length exceeds the receiver
/// length. The search starts at min(start, len - needleLen) and steps back by
/// one through zero. The Int result, including -1, is boxed as a Float and is
/// not published to entry 7.
/// Code 283 is `String.prototype.toString` on this same adapter, frame and
/// schema. It forks before `string_phase` and `key_string`. A String receiver
/// returns that receiver and does not convert, allocate a String, read
/// arguments, or write entry 2 or 7. Every other tag, including Number,
/// Boolean, Null, Undefined and Object, reuses the nullish entry-6 TypeError
/// and does not enter the entry-5 failure arm or call the receiver's
/// toString/valueOf. This is not `Object.prototype.toString`.
#[allow(clippy::too_many_arguments)]
fn append_string_char_code_at(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    index: u16,
    parser: u16,
    number_value: u16,
    h: ObjectHelpers,
) -> Result<(), ParseError> {
    let to_units = lower.utf16_units_helper().ok_or_else(|| {
        lower.error(
            span,
            "String.prototype.concat requires the installed UTF-16 bridge",
        )
    })?;
    let publish = lower.utf16_publish_helper().ok_or_else(|| {
        lower.error(
            span,
            "String.prototype.concat requires the installed UTF-16 bridge",
        )
    })?;
    let c = lower.slot(span)?;
    let code = lower.slot(span)?;
    let receiver = lower.slot(span)?;
    let args = lower.slot(span)?;
    let heap = lower.heap_ref(span)?.helpers;
    let error = lower.intrinsics.expect("intrinsics").type_error;
    let decode = lower.helper_decode.expect("decode helper");
    let mut e = Emit::new(lower, span, c)?;
    let l = e.line;
    let sp = e.lower.ir_span(span);
    let schema = result_schema(value::schema());
    let out = e.temp(
        value::logical_result_schema(),
        value::result_ok(value::completion_normal(), sp),
    )?;
    let frame = e.integer()?;
    let text = e.boxed()?;
    let raw = e.boxed()?;
    let primitive = e.boxed()?;
    let err = e.boxed()?;
    let base = e.integer()?;
    let position = e.temp(T::Float, E::Float(f64::NAN))?;
    let length = e.temp(T::Number, E::Float(0.0))?;
    let index_int = e.integer()?;
    let answer = e.boxed()?;
    let unit = e.temp(T::Number, E::Float(0.0))?;
    let char_len = e.integer()?;
    let fresh = e.integer()?;
    let alloc = e.temp(
        value::allocation_schema(),
        value::allocation(int(0), int(0), int(0)),
    )?;
    let key_result = e.temp(
        schema.clone(),
        result(schema.clone(), true, value::undefined(), sp),
    )?;
    let conversion = e.temp(
        schema.clone(),
        result(schema.clone(), true, value::undefined(), sp),
    )?;
    // `String.prototype.search` locals. The argument container is read
    // through the existing Array Get, its single value is rooted in entry 1
    // while the bridge allocates, and the private `/a/` kernel decides the
    // first match offset. The brand is the private node kind of the admitted
    // literal, read without decoding any header or own row, so no guest
    // allocation can move the argument between its identity check and the
    // kernel call.
    let search_raw = e.boxed()?;
    let search_base = e.integer()?;
    let search_found = e.boolean()?;
    let search_units = e.lower.slot(span)?;
    let search_kernel = e.lower.slot(span)?;
    // `String.prototype.match` locals. It shares the `search` argument brand,
    // the same UTF-16 bridge and the same private `/a/` kernel; the extra
    // locals assemble the real tagged Array and its one element String. The
    // match offset pair comes from the same kernel result, so no second
    // matcher and no new helper index exist.
    let match_raw = e.boxed()?;
    let match_base = e.integer()?;
    let match_found = e.boolean()?;
    let match_from = e.temp(T::Number, E::Float(0.0))?;
    let match_to = e.temp(T::Number, E::Float(0.0))?;
    let match_begin = e.integer()?;
    let match_end = e.integer()?;
    let match_cursor = e.integer()?;
    let match_cells = e.integer()?;
    let match_proto = e.boxed()?;
    let match_units = e.lower.slot(span)?;
    let match_kernel = e.lower.slot(span)?;
    let match_chars = e.lower.slot(span)?;
    let match_element = e.boxed()?;
    let match_alloc = e.temp(
        value::allocation_schema(),
        value::allocation(int(0), int(0), int(0)),
    )?;
    let match_put = e.temp(T::Null, E::Null)?;
    let fail = vec![
        e.root(var(frame), int(5), var(err))?,
        assign(out, value::result_err(var(err), sp), l),
    ];
    let nullish = vec![
        assign(err, call(error, vec![var(c)]), l),
        e.root(var(frame), int(6), var(err))?,
        assign(out, value::result_err(var(err), sp), l),
    ];
    // The search domain's own TypeError. `fail` only propagates an `err` the
    // receiver's ToString `ResultMatch` already bound, so every non-RegExp
    // argument and every kernel quota status needs the managed TypeError
    // assigned first; otherwise the adapter would propagate the empty
    // placeholder instead of the TypeError the guest catches.
    let search_fail = vec![
        assign(err, call(error, vec![var(c)]), l),
        e.root(var(frame), int(5), var(err))?,
        assign(out, value::result_err(var(err), sp), l),
    ];
    let position_value = choose(
        tag_is(var(primitive), value::NUMBER),
        num(field(var(primitive), "number")),
        choose(
            tag_is(var(primitive), value::NULL),
            E::Float(0.0),
            choose(
                tag_is(var(primitive), value::BOOLEAN),
                choose(field(var(primitive), "flag"), E::Float(1.0), E::Float(0.0)),
                choose(
                    tag_is(var(primitive), value::STRING),
                    call(parser, vec![var(c), var(primitive), int(0), E::Float(0.0)]),
                    E::Float(f64::NAN),
                ),
            ),
        ),
    );
    let in_range = and(
        not(lt(var(position), E::Float(0.0))),
        lt(num(var(position)), var(length)),
    );
    let inside = vec![
        assign(index_int, call(decode, vec![num(var(position))]), l),
        assign(
            answer,
            value::number(read_string_unit(e.read, c, base, index_int)),
            l,
        ),
        assign(
            out,
            value::result_ok(value::completion_return(var(answer)), sp),
            l,
        ),
    ];
    let outside = vec![
        assign(answer, value::number(E::Float(f64::NAN)), l),
        assign(
            out,
            value::result_ok(value::completion_return(var(answer)), sp),
            l,
        ),
    ];
    // Code 276 only. The in-range arm copies the unit before node_begin,
    // which can collect and move the source. The shared tail then publishes
    // an empty String (extent 3) or a one-unit String (extent 4) into entry 7.
    let mut char_at_result = vec![
        assign(
            alloc,
            call(
                heap.node_begin,
                vec![var(c), add(int(3), var(char_len)), int(layout::KIND_STRING)],
            ),
            l,
        ),
        assign(fresh, field(var(alloc), "base"), l),
    ];
    char_at_result.push(e.write(add(var(fresh), int(2)), var(char_len))?);
    char_at_result.push(branch(
        eq(var(char_len), int(1)),
        vec![e.write(add(var(fresh), int(3)), var(unit))?],
        vec![],
        l,
    ));
    char_at_result.push(assign(
        answer,
        call(
            heap.node_publish,
            vec![
                var(c),
                field(var(alloc), "slot"),
                field(var(alloc), "generation"),
                var(fresh),
                var(frame),
                int(7),
            ],
        ),
        l,
    ));
    char_at_result.push(assign(
        out,
        value::result_ok(value::completion_return(var(answer)), sp),
        l,
    ));
    let mut char_at = vec![branch(
        in_range.clone(),
        vec![
            assign(index_int, call(decode, vec![num(var(position))]), l),
            assign(unit, read_string_unit(e.read, c, base, index_int), l),
            assign(char_len, int(1), l),
        ],
        vec![assign(char_len, int(0), l)],
        l,
    )];
    char_at.extend(char_at_result);
    // Code 277 only. The unit reader takes a slot, so the next index is
    // written into index_int after the in-range test and before that read.
    // position is reused only after index_int is decoded. The low-surrogate
    // test and the scalar combination stay inside the second read. The Number
    // box is built once after every read. Out of range returns Undefined.
    let code_point_at = vec![branch(
        in_range.clone(),
        vec![
            assign(index_int, call(decode, vec![num(var(position))]), l),
            assign(unit, read_string_unit(e.read, c, base, index_int), l),
            branch(
                and(
                    and(
                        not(lt(num(var(unit)), num(int(0xD800)))),
                        lt(num(var(unit)), num(int(0xDC00))),
                    ),
                    lt(num(add(var(index_int), int(1))), var(length)),
                ),
                vec![
                    assign(index_int, add(var(index_int), int(1)), l),
                    assign(position, read_string_unit(e.read, c, base, index_int), l),
                    branch(
                        and(
                            not(lt(num(var(position)), num(int(0xDC00)))),
                            lt(num(var(position)), num(int(0xE000))),
                        ),
                        vec![assign(
                            unit,
                            num(add(
                                add(
                                    mul(sub(num(var(unit)), num(int(0xD800))), num(int(0x400))),
                                    sub(num(var(position)), num(int(0xDC00))),
                                ),
                                num(int(0x10000)),
                            )),
                            l,
                        )],
                        vec![],
                        l,
                    ),
                ],
                vec![],
                l,
            ),
            assign(answer, value::number(num(var(unit))), l),
            assign(
                out,
                value::result_ok(value::completion_return(var(answer)), sp),
                l,
            ),
        ],
        vec![
            assign(answer, value::undefined(), l),
            assign(
                out,
                value::result_ok(value::completion_return(var(answer)), sp),
                l,
            ),
        ],
        l,
    )];
    let conversion_ok = vec![
        e.root(var(frame), int(1), var(primitive))?,
        assign(position, position_value.clone(), l),
        assign(
            position,
            choose(
                not(eq(num(var(position)), num(var(position)))),
                E::Float(0.0),
                num(call(B::MathTrunc.call_index(), vec![num(var(position))])),
            ),
            l,
        ),
        assign(base, e.base(var(text)), l),
        assert_stmt(eq(e.decoded(var(base)), int(layout::KIND_STRING)), l),
        assign(length, e.read(add(var(base), int(2))), l),
        branch(
            eq(var(code), int(super::intrinsics::STRING_CHAR_AT_CODE)),
            char_at,
            vec![branch(
                eq(var(code), int(super::intrinsics::STRING_CODE_POINT_AT_CODE)),
                code_point_at,
                vec![branch(in_range, inside, outside, l)],
                l,
            )],
            l,
        ),
    ];
    let units = e.lower.slot(span)?;
    let arg_units = e.lower.slot(span)?;
    let arg_index = e.lower.slot(span)?;
    let arg_count = e.lower.slot(span)?;
    let active = e.lower.slot(span)?;
    let cursor = e.lower.slot(span)?;
    let unit = e.lower.slot(span)?;
    let arg_len = e.lower.slot(span)?;
    let array = T::Array(Box::new(T::Int));
    let array_len = |slot: u16| call(B::Len.call_index(), vec![E::Borrow(Box::new(var(slot)))]);
    // Reject before this argument's units are pushed. The fatal assert is the
    // existing capacity failure, not a guest RangeError. The bridge check stays.
    let append_units = |from: u16| -> Vec<S> {
        vec![
            assign(arg_len, array_len(from), l),
            assert_stmt(not(lt(int(65_536), add(array_len(units), var(arg_len)))), l),
            assign(cursor, int(0), l),
            S::While {
                condition: lt(var(cursor), var(arg_len)),
                body: vec![
                    assign(
                        unit,
                        call(
                            B::Get.call_index(),
                            vec![E::Borrow(Box::new(var(from))), var(cursor)],
                        ),
                        l,
                    ),
                    assign(
                        units,
                        call(
                            B::ArrayPush.call_index(),
                            vec![E::MoveVar(units), var(unit)],
                        ),
                        l,
                    ),
                    assign(cursor, add(var(cursor), int(1)), l),
                ],
                line: l,
            },
        ]
    };
    let concat_err = vec![
        e.root(var(frame), int(6), var(err))?,
        assign(out, value::result_err(var(err), sp), l),
        assign(active, E::Bool(false), l),
    ];
    let mut concat_err_stop = concat_err.clone();
    concat_err_stop.push(assign(arg_index, var(arg_count), l));
    // One publication, after every argument has been converted. Move the
    // accumulator; do not publish a prefix per argument.
    let concat_publish = vec![
        assert_stmt(not(lt(int(65_536), array_len(units))), l),
        assign(
            answer,
            call(publish, vec![var(c), E::MoveVar(units), var(frame), int(7)]),
            l,
        ),
        assign(
            out,
            value::result_ok(value::completion_return(var(answer)), sp),
            l,
        ),
    ];
    // Argument expressions already ran in the ordinary call. Stopping ToString
    // does not undo those effects. key_string may use entries 3, 4 and 5, so
    // the returned String is re-rooted at entry 1. Entry 3 is not stable.
    let concat_rest = {
        let mut body = vec![
            assign(
                raw,
                call(
                    B::Get.call_index(),
                    vec![E::Borrow(Box::new(var(args))), var(arg_index)],
                ),
                l,
            ),
            e.root(var(frame), int(1), var(raw))?,
            assign(
                conversion,
                call(h.key_string, vec![var(c), var(raw), var(frame)]),
                l,
            ),
        ];
        // root reserves an integer sink via effect/temp/slot. matched reserves
        // its temp only after this arm exists, matching prior argument order.
        let mut ok = vec![
            e.root(var(frame), int(1), var(primitive))?,
            assign(arg_units, call(to_units, vec![var(c), var(primitive)]), l),
        ];
        ok.extend(append_units(arg_units));
        ok.push(assign(arg_index, add(var(arg_index), int(1)), l));
        body.push(matched(
            &mut e,
            conversion,
            schema.clone(),
            primitive,
            err,
            ok,
            concat_err_stop,
        )?);
        S::While {
            condition: lt(var(arg_index), var(arg_count)),
            body,
            line: l,
        }
    };
    let mut concat_first_ok = vec![
        e.root(var(frame), int(1), var(primitive))?,
        S::Let {
            index: arg_units,
            declared_schema: Some(array.clone()),
            expr: call(to_units, vec![var(c), var(primitive)]),
            line: l,
        },
    ];
    concat_first_ok.extend(append_units(arg_units));
    concat_first_ok.push(S::Let {
        index: arg_index,
        declared_schema: Some(T::Int),
        expr: int(1),
        line: l,
    });
    concat_first_ok.push(concat_rest);
    concat_first_ok.push(branch(var(active), concat_publish, vec![], l));
    // The Int accumulator is initialized only on this non-empty concat path.
    let concat_nonempty = vec![
        S::Let {
            index: units,
            declared_schema: Some(array.clone()),
            expr: call(to_units, vec![var(c), var(text)]),
            line: l,
        },
        S::Let {
            index: arg_count,
            declared_schema: Some(T::Int),
            expr: array_len(args),
            line: l,
        },
        S::Let {
            index: active,
            declared_schema: Some(T::Bool),
            expr: E::Bool(true),
            line: l,
        },
        S::Let {
            index: cursor,
            declared_schema: Some(T::Int),
            expr: int(0),
            line: l,
        },
        S::Let {
            index: unit,
            declared_schema: Some(T::Int),
            expr: int(0),
            line: l,
        },
        S::Let {
            index: arg_len,
            declared_schema: Some(T::Int),
            expr: int(0),
            line: l,
        },
        assign(
            raw,
            call(
                B::Get.call_index(),
                vec![E::Borrow(Box::new(var(args))), int(0)],
            ),
            l,
        ),
        e.root(var(frame), int(1), var(raw))?,
        assign(
            conversion,
            call(h.key_string, vec![var(c), var(raw), var(frame)]),
            l,
        ),
        matched(
            &mut e,
            conversion,
            schema.clone(),
            primitive,
            err,
            concat_first_ok,
            concat_err,
        )?,
    ];
    // Zero arguments return the receiver String already rooted at entry 2.
    // Do not call the bridge, publish a second time, or allocate another String.
    let concat_zero = vec![assign(
        out,
        value::result_ok(value::completion_return(var(text)), sp),
        l,
    )];
    let concat_steps = vec![branch(
        lt(
            int(0),
            call(B::Len.call_index(), vec![E::Borrow(Box::new(var(args)))]),
        ),
        concat_nonempty,
        concat_zero,
        l,
    )];
    let mut legacy = vec![
        assign(
            raw,
            choose(
                lt(
                    int(0),
                    call(B::Len.call_index(), vec![E::Borrow(Box::new(var(args)))]),
                ),
                call(
                    B::Get.call_index(),
                    vec![E::Borrow(Box::new(var(args))), int(0)],
                ),
                value::undefined(),
            ),
            l,
        ),
        e.root(var(frame), int(1), var(raw))?,
        assign(
            conversion,
            call(number_value, vec![var(c), var(raw), E::Bool(false)]),
            l,
        ),
    ];
    let conversion_match = matched(
        &mut e,
        conversion,
        schema.clone(),
        primitive,
        err,
        conversion_ok,
        fail.clone(),
    )?;
    legacy.push(conversion_match);
    let text_rooted = e.root(var(frame), int(2), var(text))?;
    let src_units = e.lower.slot(span)?;
    let src_len = e.lower.slot(span)?;
    let start_num = e.lower.slot(span)?;
    let start_int = e.lower.slot(span)?;
    let end_num = e.lower.slot(span)?;
    let end_int = e.lower.slot(span)?;
    let range = e.lower.slot(span)?;
    let sub_from = e.lower.slot(span)?;
    let sub_to = e.lower.slot(span)?;
    let sub_cursor = e.lower.slot(span)?;
    let sub_unit = e.lower.slot(span)?;
    let sub_argc = e.lower.slot(span)?;
    let case_units = e.lower.slot(span)?;
    let case_len = e.lower.slot(span)?;
    let case_cursor = e.lower.slot(span)?;
    let case_unit = e.lower.slot(span)?;
    let case_out = e.lower.slot(span)?;
    // Same numeric projection as the legacy position. Built after each
    // conversion's root, so a guest ToNumber runs once per present endpoint.
    let endpoint_number = || -> E {
        choose(
            tag_is(var(primitive), value::NUMBER),
            num(field(var(primitive), "number")),
            choose(
                tag_is(var(primitive), value::NULL),
                E::Float(0.0),
                choose(
                    tag_is(var(primitive), value::BOOLEAN),
                    choose(field(var(primitive), "flag"), E::Float(1.0), E::Float(0.0)),
                    choose(
                        tag_is(var(primitive), value::STRING),
                        call(parser, vec![var(c), var(primitive), int(0), E::Float(0.0)]),
                        E::Float(f64::NAN),
                    ),
                ),
            ),
        )
    };
    let truncated = |number: E| -> E {
        choose(
            not(eq(num(number.clone()), num(number.clone()))),
            E::Float(0.0),
            num(call(B::MathTrunc.call_index(), vec![num(number)])),
        )
    };
    let clamped = |number: E| -> E {
        // `substring` keeps the original clamped projection. `slice` applies
        // the relative-negative rule to the already truncated Float: a
        // negative value is measured from the end and clamped at zero, a
        // non-negative value is clamped at the source length. The result
        // stays a Float in 0..=len so the exact decode never sees a raw
        // negative.
        let slice = choose(
            lt(number.clone(), E::Float(0.0)),
            choose(
                lt(add(num(var(src_len)), number.clone()), E::Float(0.0)),
                E::Float(0.0),
                add(num(var(src_len)), number.clone()),
            ),
            choose(
                lt(num(var(src_len)), number.clone()),
                num(var(src_len)),
                number.clone(),
            ),
        );
        choose(
            eq(var(code), int(super::intrinsics::STRING_SLICE_CODE)),
            slice,
            choose(
                or(
                    lt(number.clone(), E::Float(0.0)),
                    eq(number.clone(), E::Float(0.0)),
                ),
                E::Float(0.0),
                choose(
                    lt(num(var(src_len)), number.clone()),
                    num(var(src_len)),
                    number,
                ),
            ),
        )
    };
    // Both endpoints are Int before this copy. `substring` orders them; `slice`
    // keeps the already clamped start/end order and relies on the same
    // from+cursor<to loop to publish an empty range when end < start. Empty
    // and reversed ranges still publish once through the installed bridge:
    // extent 3, length 0, no unit. This is not the charAt one-unit path and
    // does not call node_begin. The fatal assert is the existing capacity
    // failure.
    let substring_finish = vec![
        S::Let {
            index: sub_from,
            declared_schema: Some(T::Int),
            expr: choose(
                eq(var(code), int(super::intrinsics::STRING_SLICE_CODE)),
                var(start_int),
                choose(
                    lt(var(start_int), var(end_int)),
                    var(start_int),
                    var(end_int),
                ),
            ),
            line: l,
        },
        S::Let {
            index: sub_to,
            declared_schema: Some(T::Int),
            expr: choose(
                eq(var(code), int(super::intrinsics::STRING_SLICE_CODE)),
                var(end_int),
                choose(
                    lt(var(start_int), var(end_int)),
                    var(end_int),
                    var(start_int),
                ),
            ),
            line: l,
        },
        S::Let {
            index: range,
            declared_schema: Some(array.clone()),
            expr: call(B::ArrayNew.call_index(), Vec::new()),
            line: l,
        },
        S::Let {
            index: sub_cursor,
            declared_schema: Some(T::Int),
            expr: int(0),
            line: l,
        },
        S::Let {
            index: sub_unit,
            declared_schema: Some(T::Int),
            expr: int(0),
            line: l,
        },
        S::While {
            condition: lt(add(var(sub_from), var(sub_cursor)), var(sub_to)),
            body: vec![
                assign(
                    sub_unit,
                    call(
                        B::Get.call_index(),
                        vec![
                            E::Borrow(Box::new(var(src_units))),
                            add(var(sub_from), var(sub_cursor)),
                        ],
                    ),
                    l,
                ),
                assign(
                    range,
                    call(
                        B::ArrayPush.call_index(),
                        vec![E::MoveVar(range), var(sub_unit)],
                    ),
                    l,
                ),
                assign(sub_cursor, add(var(sub_cursor), int(1)), l),
            ],
            line: l,
        },
        assert_stmt(not(lt(int(65_536), array_len(range))), l),
        assign(
            answer,
            call(publish, vec![var(c), E::MoveVar(range), var(frame), int(7)]),
            l,
        ),
        assign(
            out,
            value::result_ok(value::completion_return(var(answer)), sp),
            l,
        ),
    ];
    let mut substring_nonempty = vec![
        assign(
            raw,
            call(
                B::Get.call_index(),
                vec![E::Borrow(Box::new(var(args))), int(0)],
            ),
            l,
        ),
        e.root(var(frame), int(1), var(raw))?,
        assign(
            conversion,
            call(number_value, vec![var(c), var(raw), E::Bool(false)]),
            l,
        ),
    ];
    let mut start_ok = vec![
        e.root(var(frame), int(1), var(primitive))?,
        S::Let {
            index: start_num,
            declared_schema: Some(T::Float),
            expr: endpoint_number(),
            line: l,
        },
        assign(start_num, truncated(var(start_num)), l),
        S::Let {
            index: src_units,
            declared_schema: Some(array.clone()),
            expr: call(to_units, vec![var(c), var(text)]),
            line: l,
        },
        S::Let {
            index: src_len,
            declared_schema: Some(T::Int),
            expr: array_len(src_units),
            line: l,
        },
        assert_stmt(not(lt(int(65_536), var(src_len))), l),
        assign(start_num, clamped(var(start_num)), l),
        S::Let {
            index: start_int,
            declared_schema: Some(T::Int),
            expr: call(decode, vec![num(var(start_num))]),
            line: l,
        },
        S::Let {
            index: sub_argc,
            declared_schema: Some(T::Int),
            expr: array_len(args),
            line: l,
        },
        assign(
            raw,
            choose(
                lt(var(sub_argc), int(2)),
                value::undefined(),
                call(
                    B::Get.call_index(),
                    vec![E::Borrow(Box::new(var(args))), int(1)],
                ),
            ),
            l,
        ),
        e.root(var(frame), int(1), var(raw))?,
    ];
    // Argument expressions already ran in the ordinary call. This Err stops
    // the rest of the phase and does not publish. It does not undo those
    // effects, and it does not reuse concat's active flag.
    let end_err = vec![
        e.root(var(frame), int(6), var(err))?,
        assign(out, value::result_err(var(err), sp), l),
    ];
    let mut end_ok = vec![
        e.root(var(frame), int(1), var(primitive))?,
        S::Let {
            index: end_num,
            declared_schema: Some(T::Float),
            expr: endpoint_number(),
            line: l,
        },
        assign(end_num, truncated(var(end_num)), l),
        assign(end_num, clamped(var(end_num)), l),
        S::Let {
            index: end_int,
            declared_schema: Some(T::Int),
            expr: call(decode, vec![num(var(end_num))]),
            line: l,
        },
    ];
    end_ok.extend(substring_finish.clone());
    // Missing end and an explicit Undefined argument use Float(length) with
    // no number_value. An object whose valueOf returns undefined is not this
    // raw tag, so it still converts and becomes ToInteger 0.
    let mut end_default = vec![
        S::Let {
            index: end_num,
            declared_schema: Some(T::Float),
            expr: num(var(src_len)),
            line: l,
        },
        S::Let {
            index: end_int,
            declared_schema: Some(T::Int),
            expr: var(src_len),
            line: l,
        },
    ];
    end_default.extend(substring_finish);
    let end_match = matched(
        &mut e,
        conversion,
        schema.clone(),
        primitive,
        err,
        end_ok,
        end_err,
    )?;
    start_ok.push(branch(
        or(
            lt(var(sub_argc), int(2)),
            tag_is(var(raw), value::UNDEFINED),
        ),
        end_default,
        vec![
            assign(
                conversion,
                call(number_value, vec![var(c), var(raw), E::Bool(false)]),
                l,
            ),
            end_match,
        ],
        l,
    ));
    let start_err = vec![
        e.root(var(frame), int(6), var(err))?,
        assign(out, value::result_err(var(err), sp), l),
    ];
    substring_nonempty.push(matched(
        &mut e,
        conversion,
        schema.clone(),
        primitive,
        err,
        start_ok,
        start_err,
    )?);
    // Zero arguments return the receiver String already rooted at entry 2.
    // Omitted start is ToInteger(undefined) = 0 and omitted end is the length,
    // so that rooted String is the result. This path does not call the bridge,
    // publish a second time, or allocate a String or an array. It is not
    // concat's zero-argument vector.
    let substring_zero = vec![assign(
        out,
        value::result_ok(value::completion_return(var(text)), sp),
        l,
    )];
    let substring_steps = vec![branch(
        lt(int(0), array_len(args)),
        substring_nonempty,
        substring_zero,
        l,
    )];
    // Code 281 reuses the existing result sink and `fresh` as a root-write
    // sink. No new physical local, no new Bool, and no ArrayNew in the
    // 275..280 prefix. Five typed S::Let writes stay needle array, needle
    // length, receiver array, receiver length, and clamped start. `sub_cursor`
    // is the same Int slot as substring's j. That Let does not dominate this
    // branch, so the scan arm declares it once before the search loop.
    let result_sink = {
        let found = e.declarations.iter().rev().find_map(|stmt| match stmt {
            S::Let {
                index,
                declared_schema: Some(T::Null),
                ..
            } => Some(*index),
            _ => None,
        });
        found.ok_or_else(|| {
            e.lower.error(
                span,
                "String.prototype.indexOf requires an existing result sink",
            )
        })?
    };
    let root_entry = |entry: i64, payload: E| -> S {
        assign(
            fresh,
            call(
                heap.root_write,
                vec![var(c), var(frame), int(entry), payload],
            ),
            l,
        )
    };
    let match_result = |raw: u16, ok_body: Vec<S>, error_body: Vec<S>| -> S {
        assign(
            result_sink,
            E::ResultMatch {
                value: Box::new(var(raw)),
                value_slot: raw,
                ok_binding: Some(primitive),
                err_binding: Some(err),
                ok_expr: Box::new(E::Block {
                    stmts: ok_body,
                    expr: Box::new(E::Null),
                }),
                err_expr: Box::new(E::Block {
                    stmts: error_body,
                    expr: Box::new(E::Null),
                }),
                scrutinee_schema: schema.clone(),
                span: sp,
                semantic_id: None,
            },
            l,
        )
    };
    let indexof_scan = vec![
        assign(sub_cursor, int(0), l),
        S::While {
            condition: and(
                not(lt(var(sub_cursor), int(0))),
                lt(var(sub_cursor), var(sub_to)),
            ),
            body: vec![branch(
                eq(
                    call(
                        B::Get.call_index(),
                        vec![
                            E::Borrow(Box::new(var(src_units))),
                            add(var(sub_from), var(sub_cursor)),
                        ],
                    ),
                    call(
                        B::Get.call_index(),
                        vec![E::Borrow(Box::new(var(range))), var(sub_cursor)],
                    ),
                ),
                vec![assign(sub_cursor, add(var(sub_cursor), int(1)), l)],
                vec![assign(sub_cursor, int(-1), l)],
                l,
            )],
            line: l,
        },
        branch(
            eq(var(sub_cursor), var(sub_to)),
            vec![assign(index_int, var(sub_from), l)],
            vec![assign(sub_from, add(var(sub_from), int(1)), l)],
            l,
        ),
    ];
    let indexof_after_position = vec![
        S::Let {
            index: src_units,
            declared_schema: Some(array.clone()),
            expr: call(to_units, vec![var(c), var(text)]),
            line: l,
        },
        S::Let {
            index: src_len,
            declared_schema: Some(T::Int),
            expr: array_len(src_units),
            line: l,
        },
        assert_stmt(not(lt(int(65_536), var(src_len))), l),
        assign(
            position,
            choose(
                lt(var(position), E::Float(0.0)),
                E::Float(0.0),
                choose(
                    lt(num(var(src_len)), var(position)),
                    num(var(src_len)),
                    var(position),
                ),
            ),
            l,
        ),
        S::Let {
            index: sub_from,
            declared_schema: Some(T::Int),
            expr: call(decode, vec![num(var(position))]),
            line: l,
        },
        assign(index_int, int(-1), l),
        branch(
            eq(var(sub_to), int(0)),
            vec![assign(index_int, var(sub_from), l)],
            vec![branch(
                lt(var(src_len), add(var(sub_from), var(sub_to))),
                vec![],
                vec![
                    S::Let {
                        index: sub_cursor,
                        declared_schema: Some(T::Int),
                        expr: int(0),
                        line: l,
                    },
                    S::While {
                        condition: and(
                            lt(var(index_int), int(0)),
                            not(lt(var(src_len), add(var(sub_from), var(sub_to)))),
                        ),
                        body: indexof_scan,
                        line: l,
                    },
                ],
                l,
            )],
            l,
        ),
        assign(answer, value::number(num(var(index_int))), l),
        assign(
            out,
            value::result_ok(value::completion_return(var(answer)), sp),
            l,
        ),
    ];
    let position_ok = {
        let mut ok = vec![
            root_entry(1, var(primitive)),
            assign(position, truncated(endpoint_number()), l),
        ];
        ok.extend(indexof_after_position.iter().cloned());
        ok
    };
    let position_phase = vec![
        assign(
            raw,
            choose(
                lt(array_len(args), int(2)),
                value::undefined(),
                call(
                    B::Get.call_index(),
                    vec![E::Borrow(Box::new(var(args))), int(1)],
                ),
            ),
            l,
        ),
        root_entry(1, var(raw)),
        branch(
            tag_is(var(raw), value::UNDEFINED),
            {
                let mut skipped = vec![assign(position, E::Float(0.0), l)];
                skipped.extend(indexof_after_position);
                skipped
            },
            vec![
                assign(
                    conversion,
                    call(number_value, vec![var(c), var(raw), E::Bool(false)]),
                    l,
                ),
                match_result(conversion, position_ok, fail.clone()),
            ],
            l,
        ),
    ];
    let search_ok = vec![
        root_entry(1, var(primitive)),
        S::Let {
            index: range,
            declared_schema: Some(array.clone()),
            expr: call(to_units, vec![var(c), var(primitive)]),
            line: l,
        },
        S::Let {
            index: sub_to,
            declared_schema: Some(T::Int),
            expr: array_len(range),
            line: l,
        },
        assert_stmt(not(lt(int(65_536), var(sub_to))), l),
    ]
    .into_iter()
    .chain(position_phase)
    .collect::<Vec<_>>();
    let indexof_steps = vec![
        assign(
            raw,
            choose(
                lt(array_len(args), int(1)),
                value::undefined(),
                call(
                    B::Get.call_index(),
                    vec![E::Borrow(Box::new(var(args))), int(0)],
                ),
            ),
            l,
        ),
        root_entry(1, var(raw)),
        assign(
            conversion,
            call(h.key_string, vec![var(c), var(raw), var(frame)]),
            l,
        ),
        match_result(conversion, search_ok, fail.clone()),
    ];
    // Code 282 reuses the existing result sink, `fresh` root-write sink,
    // `position` Float and `index_int`. No new physical local, no new Bool,
    // and no ArrayNew or conversion on the 275..281 prefix. Six typed S::Let
    // writes stay on this arm: needle array, needle length, receiver array,
    // receiver length, clamped start, and the scan-only cursor. Sibling arms
    // do not dominate those slots.
    let last_truncated = |number: E| -> E {
        choose(
            not(eq(num(number.clone()), num(number.clone()))),
            E::Float(65536.0),
            num(call(B::MathTrunc.call_index(), vec![num(number)])),
        )
    };
    let last_scan = vec![
        assign(sub_cursor, int(0), l),
        S::While {
            condition: and(
                not(lt(var(sub_cursor), int(0))),
                lt(var(sub_cursor), var(sub_to)),
            ),
            body: vec![branch(
                eq(
                    call(
                        B::Get.call_index(),
                        vec![
                            E::Borrow(Box::new(var(src_units))),
                            add(var(sub_from), var(sub_cursor)),
                        ],
                    ),
                    call(
                        B::Get.call_index(),
                        vec![E::Borrow(Box::new(var(range))), var(sub_cursor)],
                    ),
                ),
                vec![assign(sub_cursor, add(var(sub_cursor), int(1)), l)],
                vec![assign(sub_cursor, int(-1), l)],
                l,
            )],
            line: l,
        },
        branch(
            eq(var(sub_cursor), var(sub_to)),
            vec![assign(index_int, var(sub_from), l)],
            vec![assign(sub_from, sub(var(sub_from), int(1)), l)],
            l,
        ),
    ];
    let last_after_position = vec![
        S::Let {
            index: src_units,
            declared_schema: Some(array.clone()),
            expr: call(to_units, vec![var(c), var(text)]),
            line: l,
        },
        S::Let {
            index: src_len,
            declared_schema: Some(T::Int),
            expr: array_len(src_units),
            line: l,
        },
        assert_stmt(not(lt(int(65_536), var(src_len))), l),
        assign(position, clamped(var(position)), l),
        S::Let {
            index: sub_from,
            declared_schema: Some(T::Int),
            expr: call(decode, vec![num(var(position))]),
            line: l,
        },
        assign(index_int, int(-1), l),
        branch(
            eq(var(sub_to), int(0)),
            vec![assign(index_int, var(sub_from), l)],
            vec![branch(
                lt(var(src_len), var(sub_to)),
                vec![],
                vec![
                    assign(
                        sub_from,
                        choose(
                            lt(var(sub_from), sub(var(src_len), var(sub_to))),
                            var(sub_from),
                            sub(var(src_len), var(sub_to)),
                        ),
                        l,
                    ),
                    S::Let {
                        index: sub_cursor,
                        declared_schema: Some(T::Int),
                        expr: int(0),
                        line: l,
                    },
                    S::While {
                        condition: and(lt(var(index_int), int(0)), not(lt(var(sub_from), int(0)))),
                        body: last_scan,
                        line: l,
                    },
                ],
                l,
            )],
            l,
        ),
        assign(answer, value::number(num(var(index_int))), l),
        assign(
            out,
            value::result_ok(value::completion_return(var(answer)), sp),
            l,
        ),
    ];
    let last_position_ok = {
        let mut ok = vec![
            root_entry(1, var(primitive)),
            assign(position, last_truncated(endpoint_number()), l),
        ];
        ok.extend(last_after_position.iter().cloned());
        ok
    };
    let last_position_phase = vec![
        assign(
            raw,
            choose(
                lt(array_len(args), int(2)),
                value::undefined(),
                call(
                    B::Get.call_index(),
                    vec![E::Borrow(Box::new(var(args))), int(1)],
                ),
            ),
            l,
        ),
        root_entry(1, var(raw)),
        branch(
            tag_is(var(raw), value::UNDEFINED),
            {
                let mut skipped = vec![assign(position, E::Float(65536.0), l)];
                skipped.extend(last_after_position);
                skipped
            },
            vec![
                assign(
                    conversion,
                    call(number_value, vec![var(c), var(raw), E::Bool(false)]),
                    l,
                ),
                match_result(conversion, last_position_ok, fail.clone()),
            ],
            l,
        ),
    ];
    let last_search_ok = vec![
        root_entry(1, var(primitive)),
        S::Let {
            index: range,
            declared_schema: Some(array.clone()),
            expr: call(to_units, vec![var(c), var(primitive)]),
            line: l,
        },
        S::Let {
            index: sub_to,
            declared_schema: Some(T::Int),
            expr: array_len(range),
            line: l,
        },
        assert_stmt(not(lt(int(65_536), var(sub_to))), l),
    ]
    .into_iter()
    .chain(last_position_phase)
    .collect::<Vec<_>>();
    let lastindexof_steps = vec![
        assign(
            raw,
            choose(
                lt(array_len(args), int(1)),
                value::undefined(),
                call(
                    B::Get.call_index(),
                    vec![E::Borrow(Box::new(var(args))), int(0)],
                ),
            ),
            l,
        ),
        root_entry(1, var(raw)),
        assign(
            conversion,
            call(h.key_string, vec![var(c), var(raw), var(frame)]),
            l,
        ),
        match_result(conversion, last_search_ok, fail.clone()),
    ];
    // Codes 284 and 285 fork after entry 2 is rooted and before concat. They
    // do not take the code-283 String-brand return. Units are copied to Int
    // scalars before the result allocation. ASCII A-Z / a-z map by 32; every
    // other unit is unchanged. Empty and unchanged inputs still publish one
    // new String into entry 7. Extra arguments are ignored.
    let ascii_shift = |lo: i64, hi: i64, delta: i64| {
        choose(
            and(
                not(lt(var(case_unit), int(lo))),
                lt(var(case_unit), int(hi + 1)),
            ),
            if delta < 0 {
                sub(var(case_unit), int(-delta))
            } else {
                add(var(case_unit), int(delta))
            },
            var(case_unit),
        )
    };
    let case_steps = vec![
        S::Let {
            index: case_units,
            declared_schema: Some(array.clone()),
            expr: call(to_units, vec![var(c), var(text)]),
            line: l,
        },
        S::Let {
            index: case_len,
            declared_schema: Some(T::Int),
            expr: array_len(case_units),
            line: l,
        },
        assert_stmt(not(lt(int(65_536), var(case_len))), l),
        S::Let {
            index: case_cursor,
            declared_schema: Some(T::Int),
            expr: int(0),
            line: l,
        },
        S::Let {
            index: case_unit,
            declared_schema: Some(T::Int),
            expr: int(0),
            line: l,
        },
        S::Let {
            index: case_out,
            declared_schema: Some(array.clone()),
            expr: call(B::ArrayNew.call_index(), Vec::new()),
            line: l,
        },
        S::While {
            condition: lt(var(case_cursor), var(case_len)),
            body: vec![
                assign(
                    case_unit,
                    call(
                        B::Get.call_index(),
                        vec![E::Borrow(Box::new(var(case_units))), var(case_cursor)],
                    ),
                    l,
                ),
                assign(
                    case_out,
                    call(
                        B::ArrayPush.call_index(),
                        vec![
                            E::MoveVar(case_out),
                            choose(
                                eq(
                                    var(code),
                                    int(super::intrinsics::STRING_TO_LOWER_CASE_CODE),
                                ),
                                ascii_shift(65, 90, 32),
                                ascii_shift(97, 122, -32),
                            ),
                        ],
                    ),
                    l,
                ),
                assign(case_cursor, add(var(case_cursor), int(1)), l),
            ],
            line: l,
        },
        assert_stmt(not(lt(int(65_536), array_len(case_out))), l),
        assign(
            answer,
            call(
                publish,
                vec![var(c), E::MoveVar(case_out), var(frame), int(7)],
            ),
            l,
        ),
        assign(
            out,
            value::result_ok(value::completion_return(var(answer)), sp),
            l,
        ),
    ];
    // Codes 279 and 280 fork beside concat. Code 281 forks beside them. Code
    // 282 forks beside 281, after text is rooted at entry 2 and before the
    // legacy argument read. It does not enter the indexOf arm.
    // Codes 289..291 fork after entry 2 is rooted and before case conversion.
    // Magic 3/2/1 is the code itself: trim strips both ends, trimEnd the right
    // end, trimStart the left end. Units are the existing UTF-16 bridge.
    // Space is the MicroQuickJS set, not complete ES5.1 WhiteSpace. The kept
    // span is copied and published through the same helper as substring.
    let micro_is_space = |unit: E| {
        or(
            and(not(lt(unit.clone(), int(0x0009))), lt(unit.clone(), int(0x000E))),
            or(
                eq(unit.clone(), int(0x0020)),
                or(
                    eq(unit.clone(), int(0x00A0)),
                    or(
                        eq(unit.clone(), int(0x1680)),
                        or(
                            and(
                                not(lt(unit.clone(), int(0x2000))),
                                lt(unit.clone(), int(0x200B)),
                            ),
                            or(
                                and(
                                    not(lt(unit.clone(), int(0x2028))),
                                    lt(unit.clone(), int(0x202A)),
                                ),
                                or(
                                    eq(unit.clone(), int(0x202F)),
                                    or(
                                        eq(unit.clone(), int(0x205F)),
                                        or(eq(unit.clone(), int(0x3000)), eq(unit, int(0xFEFF))),
                                    ),
                                ),
                            ),
                        ),
                    ),
                ),
            ),
        )
    };
    let trim_left = or(
        eq(var(code), int(super::intrinsics::STRING_TRIM_CODE)),
        eq(var(code), int(super::intrinsics::STRING_TRIM_START_CODE)),
    );
    let trim_right = or(
        eq(var(code), int(super::intrinsics::STRING_TRIM_CODE)),
        eq(var(code), int(super::intrinsics::STRING_TRIM_END_CODE)),
    );
    let trim_steps = vec![
        S::Let {
            index: case_units,
            declared_schema: Some(array.clone()),
            expr: call(to_units, vec![var(c), var(text)]),
            line: l,
        },
        S::Let {
            index: case_len,
            declared_schema: Some(T::Int),
            expr: array_len(case_units),
            line: l,
        },
        assert_stmt(not(lt(int(65_536), var(case_len))), l),
        S::Let {
            index: case_cursor,
            declared_schema: Some(T::Int),
            expr: int(0),
            line: l,
        },
        S::Let {
            index: sub_to,
            declared_schema: Some(T::Int),
            expr: var(case_len),
            line: l,
        },
        S::Let {
            index: case_unit,
            declared_schema: Some(T::Int),
            expr: int(0),
            line: l,
        },
        S::Let {
            index: sub_unit,
            declared_schema: Some(T::Int),
            expr: int(1),
            line: l,
        },
        branch(
            trim_left,
            vec![S::While {
                condition: and(
                    lt(var(case_cursor), var(case_len)),
                    eq(var(sub_unit), int(1)),
                ),
                body: vec![
                    assign(
                        case_unit,
                        call(
                            B::Get.call_index(),
                            vec![E::Borrow(Box::new(var(case_units))), var(case_cursor)],
                        ),
                        l,
                    ),
                    assign(
                        sub_unit,
                        choose(micro_is_space(var(case_unit)), int(1), int(0)),
                        l,
                    ),
                    assign(
                        case_cursor,
                        choose(
                            eq(var(sub_unit), int(1)),
                            add(var(case_cursor), int(1)),
                            var(case_cursor),
                        ),
                        l,
                    ),
                ],
                line: l,
            }],
            Vec::new(),
            l,
        ),
        assign(sub_unit, int(1), l),
        branch(
            trim_right,
            vec![S::While {
                condition: and(lt(var(case_cursor), var(sub_to)), eq(var(sub_unit), int(1))),
                body: vec![
                    assign(
                        case_unit,
                        call(
                            B::Get.call_index(),
                            vec![
                                E::Borrow(Box::new(var(case_units))),
                                sub(var(sub_to), int(1)),
                            ],
                        ),
                        l,
                    ),
                    assign(
                        sub_unit,
                        choose(micro_is_space(var(case_unit)), int(1), int(0)),
                        l,
                    ),
                    assign(
                        sub_to,
                        choose(
                            eq(var(sub_unit), int(1)),
                            sub(var(sub_to), int(1)),
                            var(sub_to),
                        ),
                        l,
                    ),
                ],
                line: l,
            }],
            Vec::new(),
            l,
        ),
        S::Let {
            index: sub_from,
            declared_schema: Some(T::Int),
            expr: var(case_cursor),
            line: l,
        },
        S::Let {
            index: case_out,
            declared_schema: Some(array.clone()),
            expr: call(B::ArrayNew.call_index(), Vec::new()),
            line: l,
        },
        S::Let {
            index: sub_cursor,
            declared_schema: Some(T::Int),
            expr: int(0),
            line: l,
        },
        S::While {
            condition: lt(add(var(sub_from), var(sub_cursor)), var(sub_to)),
            body: vec![
                assign(
                    sub_unit,
                    call(
                        B::Get.call_index(),
                        vec![
                            E::Borrow(Box::new(var(case_units))),
                            add(var(sub_from), var(sub_cursor)),
                        ],
                    ),
                    l,
                ),
                assign(
                    case_out,
                    call(
                        B::ArrayPush.call_index(),
                        vec![E::MoveVar(case_out), var(sub_unit)],
                    ),
                    l,
                ),
                assign(sub_cursor, add(var(sub_cursor), int(1)), l),
            ],
            line: l,
        },
        assert_stmt(not(lt(int(65_536), array_len(case_out))), l),
        assign(
            answer,
            call(
                publish,
                vec![var(c), E::MoveVar(case_out), var(frame), int(7)],
            ),
            l,
        ),
        assign(
            out,
            value::result_ok(value::completion_return(var(answer)), sp),
            l,
        ),
    ];
    // Code 292: string/string first-match replacement. Keep both converted
    // arguments in the existing root frame while the UTF-16 bridge allocates.
    // The search conversion precedes the replacement conversion, so a throw
    // short-circuits the latter. The arrays preserve lone surrogate units.
    let replace_search = e.lower.slot(span)?;
    let replace_value = e.lower.slot(span)?;
    let replace_needle = e.lower.slot(span)?;
    let replace_units = e.lower.slot(span)?;
    let replace_pos = e.lower.slot(span)?;
    let replace_scan = e.lower.slot(span)?;
    let replace_cursor = e.lower.slot(span)?;
    let replace_equal = e.lower.slot(span)?;
    let replace_copy = e.lower.slot(span)?;
    let replace_token = e.lower.slot(span)?;
    let replace_next = e.lower.slot(span)?;
    let replace_length = e.lower.slot(span)?;
    let replace_needle_len = e.lower.slot(span)?;
    let replace_repl_len = e.lower.slot(span)?;
    let replace_item = e.lower.slot(span)?;
    let replace_end = e.lower.slot(span)?;
    let replace_first = e.lower.slot(span)?;
    let replace_done = e.lower.slot(span)?;
    let append_units = |from: E, to: E, source: u16| -> Vec<S> {
        vec![
            assign(replace_copy, from, l),
            S::While {
                condition: lt(var(replace_copy), to),
                body: vec![
                    assign(
                        replace_item,
                        call(
                            B::Get.call_index(),
                            vec![E::Borrow(Box::new(var(source))), var(replace_copy)],
                        ),
                        l,
                    ),
                    assign(
                        case_out,
                        call(B::ArrayPush.call_index(), vec![E::MoveVar(case_out), var(replace_item)]),
                        l,
                    ),
                    assign(replace_copy, add(var(replace_copy), int(1)), l),
                ],
                line: l,
            },
        ]
    };
    let mut expanded_token = vec![assign(replace_cursor, add(var(replace_cursor), int(2)), l)];
    let token_range = |from: E, to: E| append_units(from, to, src_units);
    expanded_token.push(branch(
        eq(var(replace_next), int(36)),
        vec![assign(case_out, call(B::ArrayPush.call_index(), vec![E::MoveVar(case_out), int(36)]), l)],
        vec![branch(
            eq(var(replace_next), int(38)),
            token_range(var(replace_pos), add(var(replace_pos), var(replace_needle_len))),
            vec![branch(
                eq(var(replace_next), int(96)),
                token_range(int(0), var(replace_pos)),
                token_range(add(var(replace_pos), var(replace_needle_len)), var(replace_length)),
                l,
            )],
            l,
        )],
        l,
    ));
    let literal_token = vec![
        assign(case_out, call(B::ArrayPush.call_index(), vec![E::MoveVar(case_out), var(replace_token)]), l),
        assign(replace_cursor, add(var(replace_cursor), int(1)), l),
    ];
    let mut replacement_result = vec![
        S::Let { index: replace_units, declared_schema: Some(array.clone()), expr: call(to_units, vec![var(c), var(replace_value)]), line: l },
        S::Let { index: replace_repl_len, declared_schema: Some(T::Int), expr: array_len(replace_units), line: l },
        S::Let { index: src_units, declared_schema: Some(array.clone()), expr: call(to_units, vec![var(c), var(text)]), line: l },
        S::Let { index: replace_length, declared_schema: Some(T::Int), expr: array_len(src_units), line: l },
        S::Let { index: replace_needle, declared_schema: Some(array.clone()), expr: call(to_units, vec![var(c), var(replace_search)]), line: l },
        S::Let { index: replace_needle_len, declared_schema: Some(T::Int), expr: array_len(replace_needle), line: l },
        S::Let { index: replace_pos, declared_schema: Some(T::Int), expr: int(-1), line: l },
        S::Let { index: replace_end, declared_schema: Some(T::Int), expr: int(0), line: l },
        S::Let { index: replace_first, declared_schema: Some(T::Int), expr: int(1), line: l },
        S::Let { index: replace_done, declared_schema: Some(T::Int), expr: int(0), line: l },
        S::Let { index: replace_scan, declared_schema: Some(T::Int), expr: int(0), line: l },
        S::Let { index: replace_cursor, declared_schema: Some(T::Int), expr: int(0), line: l },
        S::Let { index: replace_equal, declared_schema: Some(T::Int), expr: int(1), line: l },
        S::Let { index: replace_copy, declared_schema: Some(T::Int), expr: int(0), line: l },
        S::Let { index: replace_token, declared_schema: Some(T::Int), expr: int(0), line: l },
        S::Let { index: replace_next, declared_schema: Some(T::Int), expr: int(0), line: l },
        S::Let { index: replace_item, declared_schema: Some(T::Int), expr: int(0), line: l },
        S::Let { index: case_out, declared_schema: Some(array.clone()), expr: call(B::ArrayNew.call_index(), Vec::new()), line: l },
        S::While {
            condition: eq(var(replace_done), int(0)),
            body: {
                // One match step. `replace()` stops after the first match and
                // therefore runs this body exactly once; `replaceAll()` keeps
                // advancing `replace_end` until the search fails. The
                // replacement tokens are re-expanded on every hit so `$&`,
                // `$`` and `$'` see the current match position.
                let mut match_step = vec![branch(
                    eq(var(replace_needle_len), int(0)),
                    // Empty needle: the first match is at 0; later matches
                    // are one UTF-16 unit past the previous insertion, which
                    // reproduces upstream's `-a-b-` behavior.
                    vec![branch(
                        eq(var(replace_first), int(1)),
                        vec![assign(replace_pos, int(0), l)],
                        vec![assign(replace_pos, choose(
                            not(lt(var(replace_end), var(replace_length))),
                            int(-1),
                            add(var(replace_end), int(1)),
                        ), l)],
                        l,
                    )],
                    vec![
                        assign(replace_pos, int(-1), l),
                        assign(replace_scan, var(replace_end), l),
                        S::While {
                            condition: and(lt(var(replace_pos), int(0)), lt(var(replace_scan), add(sub(var(replace_length), var(replace_needle_len)), int(1)))),
                            body: vec![
                                assign(replace_cursor, int(0), l),
                                assign(replace_equal, int(1), l),
                                S::While {
                                    condition: and(lt(var(replace_cursor), var(replace_needle_len)), eq(var(replace_equal), int(1))),
                                    body: vec![
                                        assign(replace_equal, choose(eq(
                                            call(B::Get.call_index(), vec![E::Borrow(Box::new(var(src_units))), add(var(replace_scan), var(replace_cursor))]),
                                            call(B::Get.call_index(), vec![E::Borrow(Box::new(var(replace_needle))), var(replace_cursor)]),
                                        ), int(1), int(0)), l),
                                        assign(replace_cursor, add(var(replace_cursor), int(1)), l),
                                    ],
                                    line: l,
                                },
                                branch(eq(var(replace_equal), int(1)), vec![assign(replace_pos, var(replace_scan), l)], vec![assign(replace_scan, add(var(replace_scan), int(1)), l)], l),
                            ],
                            line: l,
                        },
                    ],
                    l,
                )];
                let mut matched = token_range(var(replace_end), var(replace_pos));
                matched.push(assign(replace_cursor, int(0), l));
                matched.push(S::While {
                    condition: lt(var(replace_cursor), var(replace_repl_len)),
                    body: vec![
                        assign(replace_token, call(B::Get.call_index(), vec![E::Borrow(Box::new(var(replace_units))), var(replace_cursor)]), l),
                        branch(
                            and(eq(var(replace_token), int(36)), lt(add(var(replace_cursor), int(1)), var(replace_repl_len))),
                            vec![
                                assign(replace_next, call(B::Get.call_index(), vec![E::Borrow(Box::new(var(replace_units))), add(var(replace_cursor), int(1))]), l),
                                branch(
                                    or(or(eq(var(replace_next), int(36)), eq(var(replace_next), int(38))), or(eq(var(replace_next), int(96)), eq(var(replace_next), int(39)))),
                                    expanded_token,
                                    literal_token.clone(),
                                    l,
                                ),
                            ],
                            literal_token,
                            l,
                        ),
                    ],
                    line: l,
                });
                matched.push(assign(replace_end, add(var(replace_pos), var(replace_needle_len)), l));
                matched.push(assign(replace_first, int(0), l));
                matched.push(branch(
                    not(eq(var(code), int(super::intrinsics::STRING_REPLACE_ALL_CODE))),
                    vec![assign(replace_done, int(1), l)],
                    vec![],
                    l,
                ));
                match_step.push(branch(
                    lt(var(replace_pos), int(0)),
                    vec![assign(replace_done, int(1), l)],
                    matched,
                    l,
                ));
                match_step
            },
            line: l,
        },
    ];
    replacement_result.extend(token_range(var(replace_end), var(replace_length)));
    replacement_result.extend([
        assert_stmt(not(lt(int(65_536), array_len(case_out))), l),
        assign(answer, call(publish, vec![var(c), E::MoveVar(case_out), var(frame), int(7)]), l),
        assign(out, value::result_ok(value::completion_return(var(answer)), sp), l),
    ]);
    let replace_replacement_ok = vec![
        S::Let { index: replace_value, declared_schema: Some(value::schema()), expr: var(primitive), line: l },
        root_entry(1, var(replace_value)),
    ].into_iter().chain(replacement_result).collect::<Vec<_>>();
    let replace_search_ok = vec![
        S::Let { index: replace_search, declared_schema: Some(value::schema()), expr: var(primitive), line: l },
        root_entry(6, var(replace_search)),
        assign(raw, choose(lt(array_len(args), int(2)), value::undefined(), call(B::Get.call_index(), vec![E::Borrow(Box::new(var(args))), int(1)])), l),
        root_entry(1, var(raw)),
        // `replaceAll` admits primitive replacements only. An Object here is
        // a function (upstream calls it) or a RegExp/other object (upstream
        // ToStrings it); both are outside this limited domain and are
        // rejected instead of entering the object conversion path.
        branch(
            and(
                eq(var(code), int(super::intrinsics::STRING_REPLACE_ALL_CODE)),
                tag_is(var(raw), value::OBJECT),
            ),
            nullish.clone(),
            vec![
                assign(conversion, call(h.key_string, vec![var(c), var(raw), var(frame)]), l),
                match_result(conversion, replace_replacement_ok, fail.clone()),
            ],
            l,
        ),
    ];
    let replace_steps = vec![
        assign(raw, choose(lt(array_len(args), int(1)), value::undefined(), call(B::Get.call_index(), vec![E::Borrow(Box::new(var(args))), int(0)])), l),
        root_entry(1, var(raw)),
        // A RegExp pattern is the only admitted regexp value in this frontend
        // (`/a/` or `/a/g`), and replace's object ToString path is not part of
        // the replaceAll domain. Reject every Object pattern instead of
        // silently matching the literal `/a/` source text.
        branch(
            and(
                eq(var(code), int(super::intrinsics::STRING_REPLACE_ALL_CODE)),
                tag_is(var(raw), value::OBJECT),
            ),
            nullish.clone(),
            vec![
                assign(conversion, call(h.key_string, vec![var(c), var(raw), var(frame)]), l),
                match_result(conversion, replace_search_ok, fail.clone()),
            ],
            l,
        ),
    ];
    // Split converts limit before separator. The separator remains rooted in
    // entry 6 while the receiver units and result strings are allocated.
    let split_limit = e.temp(T::Float, E::Float(4_294_967_295.0))?;
    let split_separator = e.boxed()?;
    let split_needle = e.lower.slot(span)?;
    let split_needle_len = e.integer()?;
    let split_length = e.integer()?;
    let split_count = e.integer()?;
    let split_start = e.integer()?;
    let split_scan = e.integer()?;
    let split_end = e.integer()?;
    let split_found = e.integer()?;
    let split_compare = e.integer()?;
    let split_equal = e.integer()?;
    let split_active = e.integer()?;
    let split_kept = e.integer()?;
    let split_copy = e.integer()?;
    let split_item = e.integer()?;
    let split_cells = e.integer()?;
    let split_base = e.integer()?;
    let split_proto = e.boxed()?;
    let split_piece = e.boxed()?;
    let split_put = e.temp(T::Null, E::Null)?;
    let split_alloc = e.temp(value::allocation_schema(), value::allocation(int(0), int(0), int(0)))?;
    let split_find = vec![
        assign(split_scan, var(split_start), l),
        assign(split_found, int(0), l),
        S::While {
            condition: and(
                eq(var(split_found), int(0)),
                not(lt(var(split_length), add(var(split_scan), var(split_needle_len)))),
            ),
            body: vec![
                assign(split_compare, int(0), l),
                assign(split_equal, int(1), l),
                S::While {
                    condition: and(lt(var(split_compare), var(split_needle_len)), eq(var(split_equal), int(1))),
                    body: vec![
                        assign(split_equal, choose(eq(
                            call(B::Get.call_index(), vec![E::Borrow(Box::new(var(src_units))), add(var(split_scan), var(split_compare))]),
                            call(B::Get.call_index(), vec![E::Borrow(Box::new(var(split_needle))), var(split_compare)]),
                        ), int(1), int(0)), l),
                        assign(split_compare, add(var(split_compare), int(1)), l),
                    ],
                    line: l,
                },
                branch(eq(var(split_equal), int(1)),
                    vec![assign(split_found, int(1), l)],
                    vec![assign(split_scan, add(var(split_scan), int(1)), l)], l),
            ],
            line: l,
        },
        assign(split_end, choose(eq(var(split_found), int(1)), var(split_scan), var(split_length)), l),
    ];
    let split_separator_ready = vec![
        S::Let { index: split_separator, declared_schema: Some(value::schema()), expr: var(primitive), line: l },
        root_entry(6, var(split_separator)),
        S::Let { index: src_units, declared_schema: Some(array.clone()), expr: call(to_units, vec![var(c), var(text)]), line: l },
        S::Let { index: split_length, declared_schema: Some(T::Int), expr: array_len(src_units), line: l },
        branch(tag_is(var(split_separator), value::UNDEFINED),
            vec![S::Let { index: split_needle, declared_schema: Some(array.clone()), expr: call(B::ArrayNew.call_index(), Vec::new()), line: l }],
            vec![S::Let { index: split_needle, declared_schema: Some(array.clone()), expr: call(to_units, vec![var(c), var(split_separator)]), line: l }], l),
        S::Let { index: split_needle_len, declared_schema: Some(T::Int), expr: array_len(split_needle), line: l },
    ];
    let split_is_undefined = tag_is(var(split_separator), value::UNDEFINED);
    let mut split_count_search = vec![
        assign(split_count, int(0), l),
        assign(split_start, int(0), l),
        assign(split_active, int(1), l),
        S::While {
            condition: and(lt(num(var(split_count)), var(split_limit)), eq(var(split_active), int(1))),
            body: {
                let mut scan = split_find.clone();
                scan.extend([
                    assign(split_count, add(var(split_count), int(1)), l),
                    branch(eq(var(split_found), int(1)),
                        vec![assign(split_start, add(var(split_scan), var(split_needle_len)), l)],
                        vec![assign(split_active, int(0), l)], l),
                ]);
                scan
            },
            line: l,
        },
    ];
    let mut split_count_steps = split_separator_ready;
    split_count_steps.push(branch(
        eq(var(split_limit), E::Float(0.0)),
        vec![assign(split_count, int(0), l)],
        vec![branch(split_is_undefined.clone(),
            vec![assign(split_count, int(1), l)],
            vec![branch(eq(var(split_needle_len), int(0)),
                vec![assign(split_count, choose(lt(num(var(split_length)), var(split_limit)), var(split_length), call(decode, vec![var(split_limit)])), l)],
                std::mem::take(&mut split_count_search), l)], l)], l));
    split_count_steps.extend([
        // The UTF-16 bridge bounds the receiver; this also bounds all result
        // elements. A capacity refusal remains an internal fatal assertion.
        assert_stmt(not(lt(int(65_536), var(split_count))), l),
        assign(split_cells, add(int(layout::ARRAY_HEADER_CELLS), mul(int(5), var(split_count))), l),
        assign(split_alloc, call(heap.node_begin, vec![var(c), var(split_cells), int(layout::KIND_TAGGED_ARRAY)]), l),
        assign(split_base, field(var(split_alloc), "base"), l),
        assign(split_proto, call(e.lower.intrinsics.expect("intrinsics").array_proto, vec![var(c)]), l),
    ]);
    for (offset, payload) in [
        (2i64, var(split_count)),
        (3, num(value::field(split_proto, "ref_slot"))),
        (4, num(value::field(split_proto, "generation"))),
        (layout::ARRAY_PRESENT_OFFSET, add(var(split_count), int(1))),
    ] {
        split_count_steps.push(e.write(add(var(split_base), int(offset)), payload)?);
    }
    split_count_steps.extend([
        assign(split_copy, int(0), l),
        S::While {
            condition: lt(var(split_copy), mul(int(5), var(split_count))),
            body: vec![
                e.write(add(var(split_base), add(int(layout::ARRAY_HEADER_CELLS), var(split_copy))), int(0))?,
                assign(split_copy, add(var(split_copy), int(1)), l),
            ],
            line: l,
        },
        assign(answer, call(heap.node_publish, vec![
            var(c), field(var(split_alloc), "slot"), field(var(split_alloc), "generation"),
            var(split_base), var(frame), int(7),
        ]), l),
        assign(split_kept, int(0), l),
        assign(split_start, int(0), l),
    ]);
    let mut split_fill = vec![branch(
        split_is_undefined,
        vec![assign(split_end, var(split_length), l), assign(split_found, int(0), l)],
        vec![branch(eq(var(split_needle_len), int(0)),
            vec![assign(split_end, add(var(split_start), int(1)), l), assign(split_found, int(1), l)],
            split_find, l)], l)];
    split_fill.extend([
        assign(split_copy, var(split_start), l),
        S::Let { index: case_out, declared_schema: Some(array.clone()), expr: call(B::ArrayNew.call_index(), Vec::new()), line: l },
        S::While {
            condition: lt(var(split_copy), var(split_end)),
            body: vec![
                assign(split_item, call(B::Get.call_index(), vec![E::Borrow(Box::new(var(src_units))), var(split_copy)]), l),
                assign(case_out, call(B::ArrayPush.call_index(), vec![E::MoveVar(case_out), var(split_item)]), l),
                assign(split_copy, add(var(split_copy), int(1)), l),
            ],
            line: l,
        },
        assign(split_piece, call(publish, vec![var(c), E::MoveVar(case_out), var(frame), int(1)]), l),
        assign(split_base, e.base(var(answer)), l),
        assign(split_put, call(heap.array_put, vec![
            var(c), var(split_base), var(split_kept), var(split_piece),
            field(var(answer), "ref_slot"), field(var(answer), "generation"),
        ]), l),
        assign(split_start, choose(eq(var(split_needle_len), int(0)), var(split_end), add(var(split_end), var(split_needle_len))), l),
        assign(split_kept, add(var(split_kept), int(1)), l),
    ]);
    split_count_steps.extend([
        S::While { condition: lt(var(split_kept), var(split_count)), body: split_fill, line: l },
        assign(out, value::result_ok(value::completion_return(var(answer)), sp), l),
    ]);
    let split_separator_steps = vec![
        assign(raw, choose(lt(array_len(args), int(1)), value::undefined(), call(B::Get.call_index(), vec![E::Borrow(Box::new(var(args))), int(0)])), l),
        root_entry(1, var(raw)),
        branch(tag_is(var(raw), value::UNDEFINED),
            vec![assign(primitive, value::undefined(), l)].into_iter().chain(split_count_steps.clone()).collect(),
            vec![
                assign(conversion, call(h.key_string, vec![var(c), var(raw), var(frame)]), l),
                match_result(conversion, split_count_steps, fail.clone()),
            ], l),
    ];
    let split_limit_ok = vec![
        root_entry(1, var(primitive)),
        assign(split_limit, position_value.clone(), l),
        assign(split_limit, choose(
            not(eq(var(split_limit), var(split_limit))), E::Float(0.0),
            choose(eq(var(split_limit), E::Float(f64::INFINITY)), E::Float(0.0),
                choose(eq(var(split_limit), E::Float(f64::NEG_INFINITY)), E::Float(0.0),
                    call(B::MathTrunc.call_index(), vec![var(split_limit)]))),
        ), l),
        assign(split_limit, E::Mod(Box::new(var(split_limit)), Box::new(E::Float(4_294_967_296.0))), l),
        assign(split_limit, choose(lt(var(split_limit), E::Float(0.0)), add(var(split_limit), E::Float(4_294_967_296.0)), var(split_limit)), l),
    ];
    let mut split_limit_converted = split_limit_ok;
    split_limit_converted.extend(split_separator_steps.clone());
    let split_steps = vec![
        assign(raw, choose(lt(array_len(args), int(2)), value::undefined(), call(B::Get.call_index(), vec![E::Borrow(Box::new(var(args))), int(1)])), l),
        root_entry(1, var(raw)),
        branch(tag_is(var(raw), value::UNDEFINED), split_separator_steps,
            vec![
                assign(conversion, call(number_value, vec![var(c), var(raw), E::Bool(false)]), l),
                match_result(conversion, split_limit_converted, fail.clone()),
            ], l),
    ];
    // `String.prototype.search` in the one admitted `/a/` RegExp domain. The
    // only admitted argument is the object the admitted `/a/` literal
    // publishes, and its brand is the private node kind written by that
    // literal's allocator: guest code can add, overwrite or delete own rows
    // but no admitted operation rewrites cell 0 of a node header, so
    // `delete r.test` or `r.test = 5` cannot drop the identity and a copy of
    // the public `test` row on an ordinary object cannot forge it. Any other
    // argument keeps the managed TypeError, exactly like MicroQuickJS's
    // `not a regular expression`. The receiver is the existing ToString
    // `text` rooted in entry 2; the argument is rooted in entry 1 while the
    // UTF-16 bridge allocates. The kind check runs before any row or header
    // decode, so OBJECT-tagged Arrays, typed-array views and ArrayBuffer
    // backings reach that guest TypeError instead of an internal assertion.
    // Only the private guest kernel decides the first match offset, and
    // statuses 2..4 stay TypeErrors.
    let search_steps = {
        let brand = vec![
            assign(search_base, e.base(var(search_raw)), l),
            branch(
                eq(e.decoded(var(search_base)), int(layout::KIND_REGEXP)),
                vec![assign(search_found, E::Bool(true), l)],
                vec![],
                l,
            ),
        ];
        vec![
            assign(
                search_raw,
                choose(
                    lt(array_len(args), int(1)),
                    value::undefined(),
                    call(
                        B::Get.call_index(),
                        vec![E::Borrow(Box::new(var(args))), int(0)],
                    ),
                ),
                l,
            ),
            root_entry(1, var(search_raw)),
            assign(search_found, E::Bool(false), l),
            branch(
                and(
                    eq(array_len(args), int(1)),
                    tag_is(var(search_raw), value::OBJECT),
                ),
                brand,
                vec![],
                l,
            ),
            branch(
                var(search_found),
                vec![
                    S::Let {
                        index: search_units,
                        declared_schema: Some(array.clone()),
                        expr: call(to_units, vec![var(c), var(text)]),
                        line: l,
                    },
                    S::Let {
                        index: search_kernel,
                        declared_schema: None,
                        expr: call(
                            h.regexp_matcher,
                            vec![
                                var(search_units),
                                int(0),
                                int(super::super::regexp_lowering::TRANSITION_LIMIT),
                                int(super::super::regexp_lowering::SCRATCH_LIMIT),
                            ],
                        ),
                        line: l,
                    },
                    branch(
                        or(
                            eq(
                                num(field(var(search_kernel), "status")),
                                num(int(0)),
                            ),
                            eq(
                                num(field(var(search_kernel), "status")),
                                num(int(1)),
                            ),
                        ),
                        vec![
                            assign(
                                answer,
                                value::number(num(field(var(search_kernel), "start"))),
                                l,
                            ),
                            assign(
                                out,
                                value::result_ok(value::completion_return(var(answer)), sp),
                                l,
                            ),
                        ],
                        search_fail.clone(),
                        l,
                    ),
                ],
                search_fail.clone(),
                l,
            ),
        ]
    };
    let search_or_legacy = if e.lower.string_search_used.get() {
        vec![branch(
            eq(var(code), int(super::intrinsics::STRING_SEARCH_CODE)),
            search_steps,
            legacy.clone(),
            l,
        )]
    } else {
        legacy
    };
    // `String.prototype.match` in the same one admitted `/a/` RegExp domain as
    // `search`. The argument brand, the ToString receiver and the private
    // kernel are all the `search` infrastructure; only the result shape
    // differs. Upstream MicroQuickJS ignores extra arguments (`"abc".match(/a/,
    // 7)` is `["a"]`), so this gate asks only whether the first argument is the
    // branded literal the admitted `/a/` allocator publishes. Every other first
    // argument (a String, a Number, `undefined`, `null`, an ordinary object, an
    // Array, a TypedArray view, an ArrayBuffer, and the `/a/g` literal, which
    // this frontend publishes without a RegExp brand) is the managed TypeError,
    // exactly like MicroQuickJS's `not a regular expression`. The brand is read
    // from cell 0 of the node header before any prototype/own-row decode, so
    // OBJECT-tagged Arrays, typed-array views and ArrayBuffer backings reach
    // that guest TypeError instead of an internal assertion.
    //
    // Status 0 is the upstream `null`; status 1 allocates a real tagged Array
    // with one element, the matched substring published through the same UTF-16
    // bridge `split`/`replaceAll` use. Statuses 2..4 stay TypeErrors. The
    // element String is published into root entry 1 and the result Array into
    // entry 7 before `array_put` stores it, so every allocation point leaves the
    // roots complete.
    //
    // Upstream also gives that Array the enumerable own data rows `index` and
    // `input`. This realm's dense tagged Array has no named-row directory at
    // all (the seven-cell rows live on KIND_OBJECT/FUNCTION/REGEXP only, and
    // `reference.rs` pins "the Array itself has no named own rows"), so the
    // admitted result is the real Array half only. That is a documented domain
    // limitation, not a silent approximation.
    let match_steps = {
        let brand = vec![
            assign(match_base, e.base(var(match_raw)), l),
            branch(
                eq(e.decoded(var(match_base)), int(layout::KIND_REGEXP)),
                vec![assign(match_found, E::Bool(true), l)],
                vec![],
                l,
            ),
        ];
        let mut built = vec![
            assign(
                match_from,
                num(field(var(match_kernel), "start")),
                l,
            ),
            assign(match_to, num(field(var(match_kernel), "end")), l),
            assign(match_begin, call(decode, vec![var(match_from)]), l),
            assign(match_end, call(decode, vec![var(match_to)]), l),
            S::Let {
                index: match_chars,
                declared_schema: Some(array.clone()),
                expr: call(B::ArrayNew.call_index(), Vec::new()),
                line: l,
            },
            assign(match_cursor, var(match_begin), l),
            S::While {
                condition: lt(var(match_cursor), var(match_end)),
                body: vec![
                    assign(
                        match_chars,
                        call(
                            B::ArrayPush.call_index(),
                            vec![
                                E::MoveVar(match_chars),
                                call(
                                    B::Get.call_index(),
                                    vec![
                                        E::Borrow(Box::new(var(match_units))),
                                        var(match_cursor),
                                    ],
                                ),
                            ],
                        ),
                        l,
                    ),
                    assign(match_cursor, add(var(match_cursor), int(1)), l),
                ],
                line: l,
            },
            assign(
                match_element,
                call(
                    publish,
                    vec![var(c), E::MoveVar(match_chars), var(frame), int(1)],
                ),
                l,
            ),
            // One dense element: the six-cell Array header plus one five-cell
            // tagged entry. `present` is `length + 1`, the existing encoding.
            assign(
                match_cells,
                add(
                    int(layout::ARRAY_HEADER_CELLS),
                    mul(int(layout::ARRAY_TAGGED_CELLS_PER_ENTRY), int(1)),
                ),
                l,
            ),
            assign(
                match_alloc,
                call(
                    heap.node_begin,
                    vec![
                        var(c),
                        var(match_cells),
                        int(layout::KIND_TAGGED_ARRAY),
                    ],
                ),
                l,
            ),
            assign(match_base, field(var(match_alloc), "base"), l),
            assign(
                match_proto,
                call(
                    e.lower.intrinsics.expect("intrinsics").array_proto,
                    vec![var(c)],
                ),
                l,
            ),
        ];
        for (offset, payload) in [
            (2i64, int(1)),
            (3, num(value::field(match_proto, "ref_slot"))),
            (4, num(value::field(match_proto, "generation"))),
            (layout::ARRAY_PRESENT_OFFSET, int(2)),
        ] {
            built.push(e.write(add(var(match_base), int(offset)), payload)?);
        }
        built.push(assign(match_cursor, int(0), l));
        built.push(S::While {
            condition: lt(
                var(match_cursor),
                mul(int(layout::ARRAY_TAGGED_CELLS_PER_ENTRY), int(1)),
            ),
            body: vec![
                e.write(
                    add(
                        var(match_base),
                        add(int(layout::ARRAY_HEADER_CELLS), var(match_cursor)),
                    ),
                    int(0),
                )?,
                assign(match_cursor, add(var(match_cursor), int(1)), l),
            ],
            line: l,
        });
        built.extend([
            assign(
                answer,
                call(
                    heap.node_publish,
                    vec![
                        var(c),
                        field(var(match_alloc), "slot"),
                        field(var(match_alloc), "generation"),
                        var(match_base),
                        var(frame),
                        int(7),
                    ],
                ),
                l,
            ),
            assign(match_base, e.base(var(answer)), l),
            assign(
                match_put,
                call(
                    heap.array_put,
                    vec![
                        var(c),
                        var(match_base),
                        int(0),
                        var(match_element),
                        field(var(answer), "ref_slot"),
                        field(var(answer), "generation"),
                    ],
                ),
                l,
            ),
            assign(
                out,
                value::result_ok(value::completion_return(var(answer)), sp),
                l,
            ),
        ]);
        vec![
            assign(
                match_raw,
                choose(
                    lt(array_len(args), int(1)),
                    value::undefined(),
                    call(
                        B::Get.call_index(),
                        vec![E::Borrow(Box::new(var(args))), int(0)],
                    ),
                ),
                l,
            ),
            root_entry(1, var(match_raw)),
            assign(match_found, E::Bool(false), l),
            branch(
                and(
                    not(lt(array_len(args), int(1))),
                    tag_is(var(match_raw), value::OBJECT),
                ),
                brand,
                vec![],
                l,
            ),
            branch(
                var(match_found),
                vec![
                    S::Let {
                        index: match_units,
                        declared_schema: Some(array.clone()),
                        expr: call(to_units, vec![var(c), var(text)]),
                        line: l,
                    },
                    S::Let {
                        index: match_kernel,
                        declared_schema: None,
                        expr: call(
                            h.regexp_matcher,
                            vec![
                                var(match_units),
                                int(0),
                                int(super::super::regexp_lowering::TRANSITION_LIMIT),
                                int(super::super::regexp_lowering::SCRATCH_LIMIT),
                            ],
                        ),
                        line: l,
                    },
                    branch(
                        eq(num(field(var(match_kernel), "status")), num(int(1))),
                        built,
                        vec![branch(
                            eq(num(field(var(match_kernel), "status")), num(int(0))),
                            vec![
                                assign(answer, value::null(), l),
                                assign(
                                    out,
                                    value::result_ok(
                                        value::completion_return(var(answer)),
                                        sp,
                                    ),
                                    l,
                                ),
                            ],
                            search_fail.clone(),
                            l,
                        )],
                        l,
                    ),
                ],
                search_fail.clone(),
                l,
            ),
        ]
    };
    let match_or_legacy = if e.lower.string_match_used.get() {
        vec![branch(
            eq(var(code), int(super::intrinsics::STRING_MATCH_CODE)),
            match_steps,
            search_or_legacy,
            l,
        )]
    } else {
        search_or_legacy
    };
    let to_string_ok = vec![
        text_rooted,
        branch(
            eq(var(code), int(super::intrinsics::STRING_SPLIT_CODE)),
            split_steps,
            vec![branch(
            or(
                eq(var(code), int(super::intrinsics::STRING_REPLACE_CODE)),
                eq(var(code), int(super::intrinsics::STRING_REPLACE_ALL_CODE)),
            ),
            replace_steps,
            vec![branch(
            or(
                or(
                    eq(var(code), int(super::intrinsics::STRING_TRIM_CODE)),
                    eq(var(code), int(super::intrinsics::STRING_TRIM_END_CODE)),
                ),
                eq(var(code), int(super::intrinsics::STRING_TRIM_START_CODE)),
            ),
            trim_steps,
            vec![branch(
            or(
                eq(var(code), int(super::intrinsics::STRING_TO_LOWER_CASE_CODE)),
                eq(var(code), int(super::intrinsics::STRING_TO_UPPER_CASE_CODE)),
            ),
            case_steps,
            vec![branch(
                eq(var(code), int(super::intrinsics::STRING_CONCAT_CODE)),
            concat_steps,
            vec![branch(
                or(
                    eq(var(code), int(super::intrinsics::STRING_SUBSTRING_CODE)),
                    eq(var(code), int(super::intrinsics::STRING_SLICE_CODE)),
                ),
                substring_steps,
                vec![branch(
                    eq(var(code), int(super::intrinsics::STRING_LAST_INDEX_OF_CODE)),
                    lastindexof_steps,
                    vec![branch(
                        eq(var(code), int(super::intrinsics::STRING_INDEXOF_CODE)),
                        indexof_steps,
                        match_or_legacy,
                        l,
                    )],
                    l,
                )],
                l,
            )],
            l,
        )],
        l,
        )],
        l,
        )],
        l,
        )], l),
        ];
        let to_string_match = matched(
        &mut e,
        key_result,
        schema.clone(),
        text,
        err,
        to_string_ok,
        fail.clone(),
    )?;
    let string_phase = vec![
        assign(
            key_result,
            call(h.key_string, vec![var(c), var(receiver), var(frame)]),
            l,
        ),
        to_string_match,
    ];
    let admitted_code = or(
        or(
            or(
                or(
                    or(
                        or(
                            eq(var(code), int(super::intrinsics::STRING_CHAR_CODE_AT_CODE)),
                            eq(var(code), int(super::intrinsics::STRING_CHAR_AT_CODE)),
                        ),
                        eq(var(code), int(super::intrinsics::STRING_CODE_POINT_AT_CODE)),
                    ),
                    eq(var(code), int(super::intrinsics::STRING_CONCAT_CODE)),
                ),
                or(
                    eq(var(code), int(super::intrinsics::STRING_SUBSTRING_CODE)),
                    eq(var(code), int(super::intrinsics::STRING_SLICE_CODE)),
                ),
            ),
            eq(var(code), int(super::intrinsics::STRING_INDEXOF_CODE)),
        ),
        or(
            or(
                or(
                    eq(var(code), int(super::intrinsics::STRING_REPLACE_CODE)),
                    eq(var(code), int(super::intrinsics::STRING_REPLACE_ALL_CODE)),
                ),
                eq(var(code), int(super::intrinsics::STRING_SPLIT_CODE)),
            ),
            or(
                or(
                    eq(var(code), int(super::intrinsics::STRING_LAST_INDEX_OF_CODE)),
                    eq(var(code), int(super::intrinsics::STRING_TO_STRING_CODE)),
                ),
                or(
                    or(
                        eq(var(code), int(super::intrinsics::STRING_TO_LOWER_CASE_CODE)),
                        eq(var(code), int(super::intrinsics::STRING_TO_UPPER_CASE_CODE)),
                    ),
                    or(
                        or(
                            eq(var(code), int(super::intrinsics::STRING_TRIM_CODE)),
                            eq(var(code), int(super::intrinsics::STRING_TRIM_END_CODE)),
                        ),
                        eq(var(code), int(super::intrinsics::STRING_TRIM_START_CODE)),
                    ),
                ),
            ),
        ),
    );
    // Code 355 is admitted only when the gated `search` row exists, and code
    // 356 only when the gated `match` row exists, so an unreferenced realm
    // keeps the exact previous assertion.
    let mut admitted_code = if e.lower.string_search_used.get() {
        or(admitted_code, eq(var(code), int(super::intrinsics::STRING_SEARCH_CODE)))
    } else {
        admitted_code
    };
    if e.lower.string_match_used.get() {
        admitted_code = or(
            admitted_code,
            eq(var(code), int(super::intrinsics::STRING_MATCH_CODE)),
        );
    }
    let mut body = vec![
        assert_stmt(admitted_code, l),
        assign(frame, call(heap.root_enter, vec![var(c), int(8)]), l),
        e.root(var(frame), int(0), var(receiver))?,
        branch(
            eq(var(code), int(super::intrinsics::STRING_TO_STRING_CODE)),
            vec![branch(
                tag_is(var(receiver), value::STRING),
                vec![assign(
                    out,
                    value::result_ok(value::completion_return(var(receiver)), sp),
                    l,
                )],
                nullish.clone(),
                l,
            )],
            vec![branch(
                or(
                    tag_is(var(receiver), value::UNDEFINED),
                    tag_is(var(receiver), value::NULL),
                ),
                nullish,
                string_phase,
                l,
            )],
            l,
        ),
    ];
    body.push(e.effect(heap.root_leave, vec![var(c), var(frame)])?);
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        "__compat_es5_string_char_code_at",
        index,
        vec![c, code, receiver, args],
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
    Ok(())
}

/// Build the `Error.prototype.toString` arm of the receiver-aware prototype
/// adapter (dispatch code 286).
///
/// MicroQuickJS brands an Error receiver by its node class, not by any public
/// property: `JS_IsError` accepts only a live `JS_CLASS_ERROR` object. In this
/// realm the analogous brand is the packed object header's
/// `OBJECT_ERROR_INSTANCE_FLAG`. The realm `Error`/`TypeError` prototypes are
/// plain objects (brand 0, upstream `[object Object]`), so they fail this gate
/// and `TypeError.prototype.toString()` stays a TypeError. Primitive receivers,
/// non-Object node kinds (Arrays, TypedArrays, functions) and unbranded
/// objects all take the same TypeError path.
///
/// `name` is an ordinary `Get` (a getter may run and its abrupt completion is
/// propagated). `message` is read from the instance's own `message` data row,
/// the admitted stand-in for MicroQuickJS's internal message slot; a missing
/// row yields the empty String, so the `": "` separator is appended even when
/// the message is empty. The name is stringified with the managed String-hint
/// ToString, and `name` missing/Undefined falls back to the literal `"Error"`.
#[allow(clippy::too_many_arguments)]
fn append_error_to_string(
    e: &mut Emit<'_, '_>,
    frame: u16,
    receiver: u16,
    base: u16,
    kind: u16,
    out: u16,
    err: u16,
    h: ObjectHelpers,
    schema: &T,
    sp: vm::Span,
    fail: &[S],
) -> Result<Vec<S>, ParseError> {
    let l = e.line;
    let c = e.carrier;
    let to_units = e.lower.utf16_units_helper().ok_or_else(|| {
        e.lower.error(
            e.span,
            "Error.prototype.toString requires the installed UTF-16 bridge",
        )
    })?;
    let publish = e.lower.utf16_publish_helper().ok_or_else(|| {
        e.lower.error(
            e.span,
            "Error.prototype.toString requires the installed UTF-16 bridge",
        )
    })?;
    let emit_span = e.span;
    let local = |e: &mut Emit<'_, '_>| -> Result<u16, ParseError> { e.lower.slot(emit_span) };
    let name_raw = local(e)?;
    let name_value = local(e)?;
    let name_text = local(e)?;
    let message_raw = local(e)?;
    let message_value = local(e)?;
    let probe = local(e)?;
    let units = local(e)?;
    let from = local(e)?;
    let from_len = local(e)?;
    let cursor = local(e)?;
    let unit = local(e)?;
    let result_text = local(e)?;
    let active = local(e)?;
    let array = T::Array(Box::new(T::Int));
    let array_len = |slot: u16| call(B::Len.call_index(), vec![E::Borrow(Box::new(var(slot)))]);
    let let_stmt = |index: u16, declared_schema: T, expr: E| S::Let {
        index,
        declared_schema: Some(declared_schema),
        expr,
        line: l,
    };
    let value_let = |index: u16| S::Let {
        index,
        declared_schema: Some(value::schema()),
        expr: value::undefined(),
        line: l,
    };
    let append_units = |source: u16| -> Vec<S> {
        vec![
            assign(from_len, array_len(source), l),
            assert_stmt(not(lt(int(65_536), add(array_len(units), var(from_len)))), l),
            assign(cursor, int(0), l),
            S::While {
                condition: lt(var(cursor), var(from_len)),
                body: vec![
                    assign(
                        unit,
                        call(
                            B::Get.call_index(),
                            vec![E::Borrow(Box::new(var(source))), var(cursor)],
                        ),
                        l,
                    ),
                    assign(
                        units,
                        call(
                            B::ArrayPush.call_index(),
                            vec![E::MoveVar(units), var(unit)],
                        ),
                        l,
                    ),
                    assign(cursor, add(var(cursor), int(1)), l),
                ],
                line: l,
            },
        ]
    };
    // The separator is appended unconditionally: every admitted Error
    // instance either carries its own message row or reads as the empty
    // String, which still yields the trailing ": ".
    let publish_and_return = vec![
        assert_stmt(not(lt(int(65_536), array_len(units))), l),
        assign(
            result_text,
            call(publish, vec![var(c), E::MoveVar(units), var(frame), int(7)]),
            l,
        ),
        assign(
            out,
            value::result_ok(value::completion_return(var(result_text)), sp),
            l,
        ),
    ];

    // Own `message` data row: no getter runs, and a missing row is the empty
    // message. The descriptor value is stringified through key_string, so a
    // non-String assignment still produces a String pair.
    let mut message_steps = vec![let_stmt(
        probe,
        own_schema(),
        own(E::Bool(false), empty_descriptor(), int(0)),
    )];
    let message_key = super::intrinsics::string(e, "message", frame, 3, &mut message_steps)?;
    message_steps.push(assign(
        probe,
        call(h.own, vec![var(c), var(receiver), var(message_key)]),
        l,
    ));
    let mut have_message = vec![
        assign(
            message_raw,
            call(
                h.key_string,
                vec![
                    var(c),
                    field(field(var(probe), "descriptor"), "value"),
                    var(frame),
                ],
            ),
            l,
        ),
    ];
    let mut have_ok = vec![e.root(var(frame), int(1), var(message_value))?];
    have_ok.push(let_stmt(
        from,
        array.clone(),
        call(B::ArrayNew.call_index(), Vec::new()),
    ));
    have_ok.push(assign(
        from,
        call(to_units, vec![var(c), var(message_value)]),
        l,
    ));
    have_ok.extend(append_units(from));
    have_ok.push(branch(
        var(active),
        publish_and_return.clone(),
        Vec::new(),
        l,
    ));
    let have_fail = vec![
        e.root(var(frame), int(6), var(err))?,
        assign(out, value::result_err(var(err), sp), l),
        assign(active, E::Bool(false), l),
    ];
    have_message.push(matched(
        e,
        message_raw,
        schema.clone(),
        message_value,
        err,
        have_ok,
        have_fail,
    )?);
    message_steps.push(branch(
        field(var(probe), "found"),
        have_message,
        publish_and_return,
        l,
    ));

    // `name` uses ordinary Get, including prototype-chain and getter
    // semantics. Undefined selects the "Error" fallback; every other value
    // goes through the managed ToString.
    let mut name_steps = vec![let_stmt(
        name_raw,
        schema.clone(),
        result(schema.clone(), true, value::undefined(), sp),
    )];
    let name_key = super::intrinsics::string(e, "name", frame, 3, &mut name_steps)?;
    name_steps.push(assign(
        name_raw,
        call(
            h.get,
            vec![
                var(c),
                value::reference(
                    var(receiver),
                    field(var(name_key), "ref_slot"),
                    field(var(name_key), "generation"),
                    E::Bool(false),
                ),
            ],
        ),
        l,
    ));
    let mut undefined_name = Vec::new();
    let error_key = super::intrinsics::string(e, "Error", frame, 3, &mut undefined_name)?;
    undefined_name.push(assign(name_text, var(error_key), l));
    undefined_name.push(e.root(var(frame), int(1), var(name_text))?);
    let mut converted_name = vec![
        assign(
            message_raw,
            call(h.key_string, vec![var(c), var(name_value), var(frame)]),
            l,
        ),
    ];
    let converted_ok = vec![e.root(var(frame), int(1), var(name_text))?];
    let converted_fail = vec![
        e.root(var(frame), int(6), var(err))?,
        assign(out, value::result_err(var(err), sp), l),
        assign(active, E::Bool(false), l),
    ];
    converted_name.push(matched(
        e,
        message_raw,
        schema.clone(),
        name_text,
        err,
        converted_ok,
        converted_fail,
    )?);
    let mut name_ok = vec![e.root(var(frame), int(1), var(name_value))?];
    name_ok.push(branch(
        tag_is(var(name_value), value::UNDEFINED),
        undefined_name,
        converted_name,
        l,
    ));
    let get_fail = vec![
        e.root(var(frame), int(6), var(err))?,
        assign(out, value::result_err(var(err), sp), l),
        assign(active, E::Bool(false), l),
    ];
    name_steps.push(matched(
        e,
        name_raw,
        schema.clone(),
        name_value,
        err,
        name_ok,
        get_fail,
    )?);

    let mut branded = vec![
        value_let(name_value),
        value_let(name_text),
        value_let(message_value),
        let_stmt(
            message_raw,
            schema.clone(),
            result(schema.clone(), true, value::undefined(), sp),
        ),
        let_stmt(
            units,
            array.clone(),
            call(B::ArrayNew.call_index(), Vec::new()),
        ),
        let_stmt(
            from,
            array.clone(),
            call(B::ArrayNew.call_index(), Vec::new()),
        ),
        let_stmt(from_len, T::Int, int(0)),
        let_stmt(cursor, T::Int, int(0)),
        let_stmt(unit, T::Int, int(0)),
        value_let(result_text),
        let_stmt(active, T::Bool, E::Bool(true)),
    ];
    branded.extend(name_steps);
    // name_text is rooted at frame entry 1 by every successful name arm.
    // Any abrupt name completion clears `active`, so neither the separator
    // nor the message phase runs after an error Result was produced.
    let mut after_name = vec![assign(
        units,
        call(to_units, vec![var(c), var(name_text)]),
        l,
    )];
    after_name.push(assign(
        units,
        call(
            B::ArrayPush.call_index(),
            vec![E::MoveVar(units), int(58)],
        ),
        l,
    ));
    after_name.push(assign(
        units,
        call(
            B::ArrayPush.call_index(),
            vec![E::MoveVar(units), int(32)],
        ),
        l,
    ));
    after_name.extend(message_steps);
    branded.push(branch(var(active), after_name, Vec::new(), l));

    // Brand gate: live OBJECT tag, KIND_OBJECT node, instance flag in the
    // packed header. The legacy error flag alone is not sufficient.
    let mut decode_ok = Vec::new();
    let header = decode_header(e, base, &mut decode_ok)?;
    decode_ok.push(branch(var(header.error_instance), branded, fail.to_vec(), l));
    let mut brand_object = vec![
        assign(base, e.base(var(receiver)), l),
        assign(kind, e.decoded(var(base)), l),
    ];
    brand_object.push(branch(
        eq(var(kind), int(layout::KIND_OBJECT)),
        decode_ok,
        fail.to_vec(),
        l,
    ));
    Ok(vec![branch(
        tag_is(var(receiver), value::OBJECT),
        brand_object,
        fail.to_vec(),
        l,
    )])
}

/// Receiver-aware `Object.prototype` methods behind dispatch codes 534
/// (`valueOf`), 535 (`toString`) and 267 (`hasOwnProperty`).
///
/// This is the only prototype-method adapter: it receives the real receiver
/// and the real argument container. `hasOwnProperty` converts its key before
/// the receiver check, matches Null/Undefined only after that conversion, and
/// answers own-property questions without invoking any queried getter.
#[allow(clippy::too_many_arguments)]
fn append_proto_call(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    index: u16,
    h: ObjectHelpers,
) -> Result<(), ParseError> {
    let c = lower.slot(span)?;
    let code = lower.slot(span)?;
    let receiver = lower.slot(span)?;
    let args = lower.slot(span)?;
    let heap = lower.heap_ref(span)?.helpers;
    let intrinsic = lower.intrinsics.expect("intrinsics");
    let key_string = h.key_string;
    let index_key = h.index_key;
    let mut e = Emit::new(lower, span, c)?;
    let l = e.line;
    let sp = e.lower.ir_span(span);
    let schema = result_schema(value::schema());
    let out = e.temp(
        value::logical_result_schema(),
        value::result_ok(value::completion_normal(), sp),
    )?;
    let frame = e.integer()?;
    let base = e.integer()?;
    let kind = e.integer()?;
    let err = e.boxed()?;
    let text = e.boxed()?;
    let raw = e.boxed()?;
    let converted = e.boxed()?;
    let answer = e.boolean()?;
    let length = e.temp(T::Number, E::Float(0.0))?;
    let index_value = e.integer()?;
    let key_base = e.integer()?;
    let array_proto = e.boxed()?;
    let method_table = e.boxed()?;
    let key_raw = e.temp(
        schema.clone(),
        result(schema.clone(), true, value::undefined(), sp),
    )?;
    let probe = e.temp(
        own_schema(),
        own(E::Bool(false), empty_descriptor(), int(0)),
    )?;
    let mut body = vec![
        assert_stmt(
            or(
                and(
                    not(lt(var(code), int(super::math::OBJECT_VALUE_OF))),
                    lt(var(code), int(super::math::OBJECT_TO_STRING + 1)),
                ),
                or(
                    eq(var(code), int(super::intrinsics::HAS_OWN_PROPERTY_CODE)),
                    eq(var(code), int(super::intrinsics::ERROR_TO_STRING_CODE)),
                ),
            ),
            l,
        ),
        assign(frame, call(heap.root_enter, vec![var(c), int(8)]), l),
        e.root(var(frame), int(0), var(receiver))?,
    ];
    let fail = vec![
        assign(err, call(intrinsic.type_error, vec![var(c)]), l),
        e.root(var(frame), int(6), var(err))?,
        assign(out, value::result_err(var(err), sp), l),
    ];
    let value_of = vec![assign(
        out,
        value::result_ok(value::completion_return(var(receiver)), sp),
        l,
    )];
    let class = |e: &mut Emit<'_, '_>, label: &str| -> Result<Vec<S>, ParseError> {
        let mut steps = Vec::new();
        let slot = super::intrinsics::string(e, label, frame, 1, &mut steps)?;
        steps.push(assign(text, var(slot), e.line));
        steps.push(assign(
            out,
            value::result_ok(value::completion_return(var(text)), sp),
            e.line,
        ));
        Ok(steps)
    };
    let undefined_text = class(&mut e, "[object Undefined]")?;
    let null_text = class(&mut e, "[object Null]")?;
    let boolean_text = class(&mut e, "[object Boolean]")?;
    let number_text = class(&mut e, "[object Number]")?;
    let string_text = class(&mut e, "[object String]")?;
    let array_text = class(&mut e, "[object Array]")?;
    let function_text = class(&mut e, "[object Function]")?;
    let error_text = class(&mut e, "[object Error]")?;
    let object_text = class(&mut e, "[object Object]")?;
    let math_text = class(&mut e, "[object Math]")?;
    let mut object_kind = Vec::new();
    let header = property_header(&mut e, base, &mut object_kind)?;
    let ordinary = if e.lower.math_used.get() {
        let math = e.boxed()?;
        let root = e.lower.intrinsics.expect("intrinsics").root;
        vec![
            assign(math, call(root, vec![var(c), int(5)]), l),
            branch(
                and(
                    eq(
                        num(field(var(receiver), "ref_slot")),
                        num(field(var(math), "ref_slot")),
                    ),
                    eq(
                        num(field(var(receiver), "generation")),
                        num(field(var(math), "generation")),
                    ),
                ),
                math_text,
                object_text,
                l,
            ),
        ]
    } else {
        object_text
    };
    object_kind.push(branch(var(header.error), error_text, ordinary, l));
    let to_string = vec![
        assign(base, e.base(var(receiver)), l),
        assign(kind, e.decoded(var(base)), l),
        branch(
            or(
                eq(var(kind), int(layout::KIND_ARRAY)),
                eq(var(kind), int(layout::KIND_TAGGED_ARRAY)),
            ),
            array_text,
            vec![branch(
                eq(var(kind), int(layout::KIND_FUNCTION)),
                function_text,
                object_kind,
                l,
            )],
            l,
        ),
    ];
    let class_string = vec![branch(
        tag_is(var(receiver), value::UNDEFINED),
        undefined_text,
        vec![branch(
            tag_is(var(receiver), value::NULL),
            null_text,
            vec![branch(
                tag_is(var(receiver), value::BOOLEAN),
                boolean_text,
                vec![branch(
                    tag_is(var(receiver), value::NUMBER),
                    number_text,
                    vec![branch(
                        tag_is(var(receiver), value::STRING),
                        string_text,
                        vec![branch(
                            tag_is(var(receiver), value::OBJECT),
                            to_string,
                            fail.clone(),
                            l,
                        )],
                        l,
                    )],
                    l,
                )],
                l,
            )],
            l,
        )],
        l,
    )];
    // hasOwnProperty: ToString(key) first, then the receiver check.
    let mut own_property = vec![
        assign(
            raw,
            choose(
                lt(
                    int(0),
                    call(B::Len.call_index(), vec![E::Borrow(Box::new(var(args)))]),
                ),
                call(
                    B::Get.call_index(),
                    vec![E::Borrow(Box::new(var(args))), int(0)],
                ),
                value::undefined(),
            ),
            l,
        ),
        e.root(var(frame), int(1), var(raw))?,
        assign(
            key_raw,
            call(key_string, vec![var(c), var(raw), var(frame)]),
            l,
        ),
    ];
    let nullish = vec![
        assign(err, call(intrinsic.type_error, vec![var(c)]), l),
        e.root(var(frame), int(6), var(err))?,
        assign(out, value::result_err(var(err), sp), l),
    ];
    let finish = |e: &mut Emit<'_, '_>, answer: u16| -> Result<Vec<S>, ParseError> {
        Ok(vec![assign(
            out,
            value::result_ok(value::completion_return(value::boolean(var(answer))), sp),
            e.line,
        )])
    };
    let array_own = vec![
        assign(base, e.base(var(receiver)), l),
        assign(length, call(heap.array_length, vec![var(c), var(base)]), l),
        assign(
            index_value,
            call(index_key, vec![var(c), var(converted)]),
            l,
        ),
        assign(
            answer,
            or(
                and(
                    not(lt(var(index_value), int(0))),
                    lt(num(var(index_value)), var(length)),
                ),
                key_is_length(&e, key_base),
            ),
            l,
        ),
        assign(array_proto, call(intrinsic.array_proto, vec![var(c)]), l),
        branch(and(eq(field(var(receiver), "ref_slot"), field(var(array_proto), "ref_slot")), eq(field(var(receiver), "generation"), field(var(array_proto), "generation"))), vec![
            assign(method_table, call(intrinsic.root, vec![var(c), int(super::intrinsics::array_method_table_root(e.lower))]), l),
            assign(probe, call(h.own, vec![var(c), var(method_table), var(converted)]), l),
            assign(answer, or(var(answer), field(var(probe), "found")), l),
        ], Vec::new(), l),
    ];
    let string_own = vec![
        assign(base, e.base(var(receiver)), l),
        assert_stmt(eq(e.decoded(var(base)), int(layout::KIND_STRING)), l),
        assign(length, e.read(add(var(base), int(2))), l),
        assign(
            index_value,
            call(index_key, vec![var(c), var(converted)]),
            l,
        ),
        assign(
            answer,
            or(
                and(
                    not(lt(var(index_value), int(0))),
                    lt(num(var(index_value)), var(length)),
                ),
                key_is_length(&e, key_base),
            ),
            l,
        ),
    ];
    let mut plain_own = vec![
        assign(
            probe,
            call(h.own, vec![var(c), var(receiver), var(converted)]),
            l,
        ),
        assign(answer, field(var(probe), "found"), l),
    ];
    plain_own.extend(finish(&mut e, answer)?);
    let mut array_answer = array_own;
    array_answer.extend(finish(&mut e, answer)?);
    let mut string_answer = string_own;
    string_answer.extend(finish(&mut e, answer)?);
    let mut number_answer = vec![assign(answer, E::Bool(false), l)];
    number_answer.extend(finish(&mut e, answer)?);
    let receiver_kinds = vec![branch(
        or(
            eq(var(kind), int(layout::KIND_ARRAY)),
            eq(var(kind), int(layout::KIND_TAGGED_ARRAY)),
        ),
        array_answer,
        plain_own,
        l,
    )];
    let mut object_dispatch = vec![
        assign(base, e.base(var(receiver)), l),
        assign(kind, e.decoded(var(base)), l),
    ];
    object_dispatch.extend(receiver_kinds);
    let primitive_dispatch = vec![branch(
        tag_is(var(receiver), value::STRING),
        string_answer,
        vec![branch(
            or(
                tag_is(var(receiver), value::NUMBER),
                tag_is(var(receiver), value::BOOLEAN),
            ),
            number_answer,
            fail.clone(),
            l,
        )],
        l,
    )];
    let mut own_body = vec![
        assign(key_base, e.base(var(converted)), l),
        assert_stmt(eq(e.decoded(var(key_base)), int(layout::KIND_STRING)), l),
        branch(
            or(
                tag_is(var(receiver), value::NULL),
                tag_is(var(receiver), value::UNDEFINED),
            ),
            nullish,
            vec![branch(
                tag_is(var(receiver), value::OBJECT),
                object_dispatch,
                primitive_dispatch,
                l,
            )],
            l,
        ),
    ];
    let mut key_ok = vec![e.root(var(frame), int(2), var(converted))?];
    key_ok.append(&mut own_body);
    let mut key_bad = vec![e.root(var(frame), int(6), var(err))?];
    key_bad.push(assign(out, value::result_err(var(err), sp), l));
    own_property.push(matched(
        &mut e,
        key_raw,
        schema.clone(),
        converted,
        err,
        key_ok,
        key_bad,
    )?);
    let error_to_string = append_error_to_string(
        &mut e,
        frame,
        receiver,
        base,
        kind,
        out,
        err,
        h,
        &schema,
        sp,
        &fail,
    )?;
    body.push(branch(
        eq(var(code), int(super::intrinsics::HAS_OWN_PROPERTY_CODE)),
        own_property,
        vec![branch(
            eq(var(code), int(super::math::OBJECT_VALUE_OF)),
            vec![branch(
                tag_is(var(receiver), value::OBJECT),
                value_of,
                fail,
                l,
            )],
            vec![branch(
                eq(var(code), int(super::intrinsics::ERROR_TO_STRING_CODE)),
                error_to_string,
                class_string,
                l,
            )],
            l,
        )],
        l,
    ));
    body.push(e.effect(heap.root_leave, vec![var(c), var(frame)])?);
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        "__compat_es5_object_proto",
        index,
        vec![c, code, receiver, args],
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
    Ok(())
}

/// Admitted `Function.prototype.call` behind dispatch code 268.
///
/// A closed source-function target takes the whole argument vector: `thisArg`
/// is `args[0]` (undefined when no argument is given) and every later argument
/// is appended in order to the container the target's dispatch receives, which
/// is exactly the activation's `arguments`/parameter vector. The target's code
/// cell selects the shape: source functions carry their zero-based dispatch
/// id, every intrinsic node a fixed code at or above `OBJECT_CODE`.
/// The intrinsic path keeps the pre-existing admission. The four TypedArray
/// method codes forward arguments after thisArg; `Error.prototype.toString`
/// (code 286) has arity 0, so `.call` admits extra arguments and dispatch
/// ignores them, matching upstream `ts.call(e, 1, 2)` returning `Error: boom`.
fn append_function_call(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    index: u16,
    dispatch: u16,
) -> Result<(), ParseError> {
    let c = lower.slot(span)?;
    let code = lower.slot(span)?;
    let receiver = lower.slot(span)?;
    let args = lower.slot(span)?;
    let heap = lower.heap_ref(span)?.helpers;
    let intrinsic = lower.intrinsics.expect("intrinsics");
    let mut e = Emit::new(lower, span, c)?;
    let l = e.line;
    let sp = e.lower.ir_span(span);
    let out = e.temp(
        value::logical_result_schema(),
        value::result_ok(value::completion_normal(), sp),
    )?;
    let frame = e.integer()?;
    let base = e.integer()?;
    let kind = e.integer()?;
    let target = e.integer()?;
    let source = e.boolean()?;
    let cursor = e.integer()?;
    let typed_method = e.boolean()?;
    let count = e.integer()?;
    let this_arg = e.boxed()?;
    let empty = e.temp(
        T::Array(Box::new(value::schema())),
        call(B::ArrayNew.call_index(), Vec::new()),
    )?;
    let raw = e.temp(
        value::logical_result_schema(),
        value::result_ok(value::completion_normal(), sp),
    )?;
    let ok = e.temp(value::completion_schema(), value::completion_normal())?;
    let error = e.boxed()?;
    let payload = e.boxed()?;
    let mut body = vec![
        assert_stmt(
            eq(var(code), int(super::intrinsics::FUNCTION_CALL_CODE)),
            l,
        ),
        assign(frame, call(heap.root_enter, vec![var(c), int(4)]), l),
        e.root(var(frame), int(0), var(receiver))?,
    ];
    let fail = vec![
        assign(error, call(intrinsic.type_error, vec![var(c)]), l),
        e.root(var(frame), int(3), var(error))?,
        assign(out, value::result_err(var(error), sp), l),
    ];
    let mut prefix = vec![assign(empty, call(B::ArrayNew.call_index(), Vec::new()), l)];
    // A closed source function receives its whole argument vector: every
    // argument after thisArg is appended in order. A bound product forwards
    // it in exactly the same way; its own stored arguments are prepended by
    // the bind adapter one dispatch level down. Math.min/max are the only
    // numeric intrinsics that also receive that vector; dispatch still owns
    // numeric conversion. Every other intrinsic keeps the old admission.
    prefix.push(branch(
        or(
            or(
                var(source),
                eq(var(target), int(super::intrinsics::FUNCTION_BIND_CODE)),
            ),
            super::math::extrema_target(var(target)),
        ),
        vec![
            assign(cursor, int(1), l),
            S::While {
                condition: lt(var(cursor), var(count)),
                body: vec![
                    assign(
                        empty,
                        call(
                            B::ArrayPush.call_index(),
                            vec![
                                E::MoveVar(empty),
                                call(
                                    B::Get.call_index(),
                                    vec![E::Borrow(Box::new(var(args))), var(cursor)],
                                ),
                            ],
                        ),
                        l,
                    ),
                    assign(cursor, add(var(cursor), int(1)), l),
                ],
                line: l,
            },
        ],
        Vec::new(),
        l,
    ));
    let mut callable = vec![
        assign(base, e.base(var(receiver)), l),
        assign(kind, e.decoded(var(base)), l),
    ];
    // The private code cell lives in the function header, so the kind check
    // gates the read: an ordinary object has no cell at `base + 5`. Codes
    // `0..=255` are the closed source-function ids; `source` uses the
    // exclusive 256 boundary, which deliberately excludes Function.prototype
    // (code 256, a function node that is not callable) from the forwarding
    // arm. The gate below keeps the old single-argument admission for the
    // intrinsic method nodes.
    let mut function = vec![
        assign(target, e.decoded(add(var(base), int(5))), l),
        assign(
            source,
            lt(
                var(target),
                int(super::intrinsics::SOURCE_FUNCTION_CODE_LIMIT),
            ),
            l,
        ),
    ];
    if e.lower.typed_array_used() {
        use super::typed_array::{SUBARRAY, TO_STRING};
        function.push(assign(
            typed_method,
            and(
                not(lt(var(target), int(SUBARRAY))),
                not(lt(int(TO_STRING), var(target))),
            ),
            l,
        ));
        let values = || E::Borrow(Box::new(var(args)));
        prefix.push(branch(var(typed_method), vec![
            assign(this_arg, choose(lt(int(0), var(count)),
                call(B::Get.call_index(), vec![values(), int(0)]), value::undefined()), l),
            e.root(var(frame), int(1), var(this_arg))?,
            assign(cursor, int(1), l),
            S::While {
                condition: lt(var(cursor), var(count)),
                body: vec![
                    assign(empty, call(B::ArrayPush.call_index(), vec![E::MoveVar(empty),
                        call(B::Get.call_index(), vec![values(), var(cursor)])]), l),
                    assign(cursor, add(var(cursor), int(1)), l),
                ],
                line: l,
            },
        ], Vec::new(), l));
    }
    let mut invoke = prefix;
    invoke.push(assign(
            raw,
            call(
                dispatch,
                vec![var(c), var(receiver), var(this_arg), var(empty)],
            ),
            l,
        ));
    let ok_body = vec![
        assign(
            payload,
            choose(
                eq(
                    field(var(ok), "kind"),
                    int(value::COMPLETION_RETURN),
                ),
                field(var(ok), "value"),
                value::undefined(),
            ),
            l,
        ),
        e.root(var(frame), int(3), var(payload))?,
        assign(
            out,
            value::result_ok(value::completion_return(var(payload)), sp),
            l,
        ),
    ];
    let error_body = vec![
        e.root(var(frame), int(3), var(error))?,
        assign(out, value::result_err(var(error), sp), l),
    ];
    invoke.push(matched(
        &mut e,
        raw,
        value::logical_result_schema(),
        ok,
        error,
        ok_body,
        error_body,
    )?);
    // Function.prototype (code 256) is a function node without a body. It
    // must be refused before the gates below can admit it: `.call(null)` has
    // exactly one argument and would otherwise reach dispatch, whose poison
    // branch silently yields a normal completion instead of the pinned
    // `TypeError: not a function`.
    let not_prototype = not(eq(
        var(target),
        int(super::intrinsics::FUNCTION_PROTOTYPE_CODE),
    ));
    let forwards_arguments = or(
        or(
            var(source),
            eq(var(target), int(super::intrinsics::FUNCTION_BIND_CODE)),
        ),
        super::math::extrema_target(var(target)),
    );
    let can_invoke = if e.lower.typed_array_used() {
        // `Error.prototype.toString` is a zero-arity method: upstream ignores
        // extra arguments, so `.call(e, 1, 2)` must reach the brand check with
        // `e` as thisArg instead of being refused at the argument gate.
        // Math.min/max join the forwarding admission only; Object, other
        // Error methods, TypedArray and ordinary intrinsics keep this gate.
        and(
            not_prototype.clone(),
            or(
                forwards_arguments,
                or(
                    or(
                        var(typed_method),
                        eq(var(target), int(super::intrinsics::ERROR_TO_STRING_CODE)),
                    ),
                    or(
                        eq(var(target), int(super::intrinsics::FUNCTION_TO_STRING_CODE)),
                        eq(var(count), int(1)),
                    ),
                ),
            ),
        )
    } else {
        and(
            not_prototype,
            or(
                forwards_arguments,
                or(
                    eq(var(count), int(1)),
                    or(
                        eq(var(target), int(super::intrinsics::ERROR_TO_STRING_CODE)),
                        eq(var(target), int(super::intrinsics::FUNCTION_TO_STRING_CODE)),
                    ),
                ),
            ),
        )
    };
    function.push(branch(can_invoke, invoke, fail.clone(), l));
    callable.push(branch(
        eq(var(kind), int(layout::KIND_FUNCTION)),
        function,
        fail.clone(),
        l,
    ));
    body.push(assign(
        count,
        call(B::Len.call_index(), vec![E::Borrow(Box::new(var(args)))]),
        l,
    ));
    // Any admitted call with at least one argument takes args[0] as thisArg.
    // The extra-argument path (code 286) relies on this same read: the
    // TypedArray path below re-reads args[0] only when it forwards arguments.
    body.push(assign(
        this_arg,
        choose(
            lt(int(0), var(count)),
            call(
                B::Get.call_index(),
                vec![E::Borrow(Box::new(var(args))), int(0)],
            ),
            value::undefined(),
        ),
        l,
    ));
    body.push(e.root(var(frame), int(1), var(this_arg))?);
    body.push(branch(
        tag_is(var(receiver), value::OBJECT),
        callable,
        fail,
        l,
    ));
    body.push(e.effect(heap.root_leave, vec![var(c), var(frame)])?);
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        "__compat_es5_function_call",
        index,
        vec![c, code, receiver, args],
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
    Ok(())
}

/// Pinned `apply` argument-vector bound. A dense array of 65535 entries is the
/// largest accepted invocation vector; 65536 and above refuse with the pinned
/// call-argument TypeError before a single element is expanded. The value is
/// the pinned engine's u16 argument-count ceiling, not a frontend memory
/// limit, so it is independent of the realm cell budget.
const MAX_APPLY_ARGUMENTS: i64 = 65_535;

/// Admitted `Function.prototype.apply` behind dispatch code 269.
///
/// Two shapes succeed. A closed source-function target takes any dense array
/// object as the second argument: `thisArg` is `args[0]` (any value, including
/// null/undefined), the array's `length` elements are appended in order to the
/// container the target's dispatch receives, and arguments past the array are
/// ignored like the pinned engine does. Missing argument or a non-array second
/// argument keeps the throw, matching upstream `TypeError: not an array`.
/// The intrinsic path keeps the pre-existing shape: exactly two arguments, a
/// null thisArg, a callable function receiver, and an empty array object,
/// invoked with an empty argument container. `Math.min`/`Math.max` do not use
/// that fallback: they share the source/bind expansion above, including the
/// 65535 ceiling. Neither the boxed argument array nor the fresh empty Core
/// array is rooted as a Core array.
fn append_function_apply(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    index: u16,
    dispatch: u16,
) -> Result<(), ParseError> {
    let c = lower.slot(span)?;
    let code = lower.slot(span)?;
    let receiver = lower.slot(span)?;
    let args = lower.slot(span)?;
    let heap = lower.heap_ref(span)?.helpers;
    let intrinsic = lower.intrinsics.expect("intrinsics");
    let mut e = Emit::new(lower, span, c)?;
    let l = e.line;
    let sp = e.lower.ir_span(span);
    let out = e.temp(
        value::logical_result_schema(),
        value::result_ok(value::completion_normal(), sp),
    )?;
    let frame = e.integer()?;
    let base = e.integer()?;
    let kind = e.integer()?;
    let target = e.integer()?;
    let source = e.boolean()?;
    let dense = e.boolean()?;
    let cursor = e.integer()?;
    let count = e.integer()?;
    let this_arg = e.boxed()?;
    let arg_array = e.boxed()?;
    let arg_base = e.integer()?;
    let arg_kind = e.integer()?;
    let arg_length = e.integer()?;
    let empty = e.temp(
        T::Array(Box::new(value::schema())),
        call(B::ArrayNew.call_index(), Vec::new()),
    )?;
    let raw = e.temp(
        value::logical_result_schema(),
        value::result_ok(value::completion_normal(), sp),
    )?;
    let ok = e.temp(value::completion_schema(), value::completion_normal())?;
    let error = e.boxed()?;
    let payload = e.boxed()?;
    // Extrema `.apply` reserves one guest root per String/Object element of
    // the source Array's valid physical prefix. Only the capacity integer and
    // the caller-rooted boxes cross `root_enter`; every base address is
    // re-resolved by the existing admission below after entry.
    let capacity = e.temp(T::Int, int(4))?;
    let present = e.integer()?;
    let reserved = e.integer()?;
    let root_count = e.integer()?;
    let item = e.boxed()?;
    let tag = e.integer()?;
    let mut body = vec![
        assert_stmt(
            eq(var(code), int(super::intrinsics::FUNCTION_APPLY_CODE)),
            l,
        ),
        assign(
            count,
            call(B::Len.call_index(), vec![E::Borrow(Box::new(var(args)))]),
            l,
        ),
        assign(
            this_arg,
            choose(
                lt(int(1), var(count)),
                call(
                    B::Get.call_index(),
                    vec![E::Borrow(Box::new(var(args))), int(0)],
                ),
                value::undefined(),
            ),
            l,
        ),
        assign(
            arg_array,
            choose(
                lt(int(1), var(count)),
                call(
                    B::Get.call_index(),
                    vec![E::Borrow(Box::new(var(args))), int(1)],
                ),
                value::undefined(),
            ),
            l,
        ),
    ];
    let fail = vec![
        assign(error, call(intrinsic.type_error, vec![var(c)]), l),
        e.root(var(frame), int(3), var(error))?,
        assign(out, value::result_err(var(error), sp), l),
    ];
    // Capacity probe for the extrema `.apply` shape. Every read below is a
    // non-allocating cell read of values the caller keeps rooted, and only
    // integers survive across the following `root_enter`; a pressure
    // collection there therefore cannot leave a stale base behind.
    let prescan = vec![
        // Valid physical prefix: the public length bounded by the stored
        // present extent. Cells beyond it were never published and are never
        // scanned.
        assign(
            present,
            choose(
                lt(
                    var(arg_length),
                    sub(
                        e.decoded(add(var(arg_base), int(layout::ARRAY_PRESENT_OFFSET))),
                        int(1),
                    ),
                ),
                var(arg_length),
                sub(
                    e.decoded(add(var(arg_base), int(layout::ARRAY_PRESENT_OFFSET))),
                    int(1),
                ),
            ),
            l,
        ),
        assign(cursor, int(0), l),
        assign(reserved, int(0), l),
        S::While {
            condition: lt(var(cursor), var(present)),
            body: vec![
                assign(
                    tag,
                    e.decoded(add(
                        var(arg_base),
                        add(
                            int(layout::ARRAY_HEADER_CELLS),
                            mul(int(layout::ARRAY_TAGGED_CELLS_PER_ENTRY), var(cursor)),
                        ),
                    )),
                    l,
                ),
                branch(
                    or(
                        eq(var(tag), int(value::STRING)),
                        eq(var(tag), int(value::OBJECT)),
                    ),
                    vec![assign(reserved, add(var(reserved), int(1)), l)],
                    Vec::new(),
                    l,
                ),
                assign(cursor, add(var(cursor), int(1)), l),
            ],
            line: l,
        },
        assign(capacity, add(int(4), var(reserved)), l),
    ];
    let probe = vec![
        assign(base, e.base(var(receiver)), l),
        assign(kind, e.decoded(var(base)), l),
        branch(
            eq(var(kind), int(layout::KIND_FUNCTION)),
            vec![
                assign(target, e.decoded(add(var(base), int(5))), l),
                assign(
                    source,
                    lt(
                        var(target),
                        int(super::intrinsics::SOURCE_FUNCTION_CODE_LIMIT),
                    ),
                    l,
                ),
                branch(
                    tag_is(var(arg_array), value::OBJECT),
                    vec![
                        assign(arg_base, e.base(var(arg_array)), l),
                        assign(arg_kind, e.decoded(var(arg_base)), l),
                        assign(
                            dense,
                            or(
                                eq(var(arg_kind), int(layout::KIND_ARRAY)),
                                eq(var(arg_kind), int(layout::KIND_TAGGED_ARRAY)),
                            ),
                            l,
                        ),
                        assign(arg_length, int(0), l),
                        branch(
                            var(dense),
                            vec![assign(
                                arg_length,
                                call(
                                    e.decode_u32,
                                    vec![call(heap.array_length, vec![var(c), var(arg_base)])],
                                ),
                                l,
                            )],
                            Vec::new(),
                            l,
                        ),
                        branch(
                            and(
                                and(
                                    super::math::extrema_target(var(target)),
                                    eq(var(arg_kind), int(layout::KIND_TAGGED_ARRAY)),
                                ),
                                and(
                                    lt(int(1), var(count)),
                                    not(lt(int(MAX_APPLY_ARGUMENTS), var(arg_length))),
                                ),
                            ),
                            prescan,
                            Vec::new(),
                            l,
                        ),
                    ],
                    Vec::new(),
                    l,
                ),
            ],
            Vec::new(),
            l,
        ),
    ];
    body.push(branch(
        tag_is(var(receiver), value::OBJECT),
        probe,
        Vec::new(),
        l,
    ));
    // One reservation (fixed four slots plus one per reference element) and
    // then the pre-existing prologue roots. `root_enter` asserts the realm and
    // physical budgets before the frame is linked, so a failed reservation is
    // a VM resource failure, never a guest TypeError.
    body.push(assign(
        frame,
        call(heap.root_enter, vec![var(c), var(capacity)]),
        l,
    ));
    body.push(e.root(var(frame), int(0), var(receiver))?);
    body.push(e.root(var(frame), int(1), var(this_arg))?);
    body.push(e.root(var(frame), int(2), var(arg_array))?);
    let mut invoke = vec![
        assign(
            raw,
            call(
                dispatch,
                vec![var(c), var(receiver), var(this_arg), var(empty)],
            ),
            l,
        ),
    ];
    let ok_body = vec![
        assign(
            payload,
            choose(
                eq(
                    field(var(ok), "kind"),
                    int(value::COMPLETION_RETURN),
                ),
                field(var(ok), "value"),
                value::undefined(),
            ),
            l,
        ),
        e.root(var(frame), int(3), var(payload))?,
        assign(
            out,
            value::result_ok(value::completion_return(var(payload)), sp),
            l,
        ),
    ];
    let error_body = vec![
        e.root(var(frame), int(3), var(error))?,
        assign(out, value::result_err(var(error), sp), l),
    ];
    invoke.push(matched(
        &mut e,
        raw,
        value::logical_result_schema(),
        ok,
        error,
        ok_body,
        error_body,
    )?);
    let mut array_args = vec![
        assign(arg_base, e.base(var(arg_array)), l),
        assign(arg_kind, e.decoded(var(arg_base)), l),
        assign(
            dense,
            or(
                eq(var(arg_kind), int(layout::KIND_ARRAY)),
                eq(var(arg_kind), int(layout::KIND_TAGGED_ARRAY)),
            ),
            l,
        ),
        assign(arg_length, int(0), l),
        // Only the two dense array kinds carry a length header. A plain object
        // that reaches this branch is an array-like, which the pinned engine
        // refuses with the guest TypeError; reading the header first would fire
        // the allocator's kind assertion instead of that refusal.
        branch(
            var(dense),
            vec![assign(
                arg_length,
                call(
                    e.decode_u32,
                    vec![call(heap.array_length, vec![var(c), var(arg_base)])],
                ),
                l,
            )],
            Vec::new(),
            l,
        ),
    ];
    // A closed source-function target takes the dense array's elements in
    // order, one per element, and ignores arguments past the array. The bound
    // owns the whole expansion: an overlong vector returns the TypeError
    // without running a single iteration or invoking the target. The limit is
    // inclusive, so 65535 expands and 65536 refuses.
    let mut expand = vec![
        assign(cursor, int(0), l),
        S::While {
            condition: lt(var(cursor), var(arg_length)),
            body: vec![
                assign(
                    empty,
                    call(
                        B::ArrayPush.call_index(),
                        vec![
                            E::MoveVar(empty),
                            call(
                                heap.array_get,
                                vec![var(c), var(arg_base), var(cursor)],
                            ),
                        ],
                    ),
                    l,
                ),
                assign(cursor, add(var(cursor), int(1)), l),
            ],
            line: l,
        },
    ];
    expand.extend(invoke.clone());
    let forward = vec![branch(
        lt(int(MAX_APPLY_ARGUMENTS), var(arg_length)),
        fail.clone(),
        expand,
        l,
    )];
    // Extrema keep the original expansion order but write every String and
    // Object value into the consecutive guest roots reserved above. The
    // post-entry scan consumes exactly the pre-entry physical-prefix count.
    let mut extrema_expand = vec![
        assign(cursor, int(0), l),
        assign(root_count, int(0), l),
        S::While {
            condition: lt(var(cursor), var(arg_length)),
            body: vec![
                assign(
                    item,
                    call(heap.array_get, vec![var(c), var(arg_base), var(cursor)]),
                    l,
                ),
                assign(
                    empty,
                    call(
                        B::ArrayPush.call_index(),
                        vec![E::MoveVar(empty), var(item)],
                    ),
                    l,
                ),
                branch(
                    or(
                        tag_is(var(item), value::STRING),
                        tag_is(var(item), value::OBJECT),
                    ),
                    vec![
                        e.root(var(frame), add(int(4), var(root_count)), var(item))?,
                        assign(root_count, add(var(root_count), int(1)), l),
                    ],
                    Vec::new(),
                    l,
                ),
                assign(cursor, add(var(cursor), int(1)), l),
            ],
            line: l,
        },
        assert_stmt(eq(var(root_count), var(reserved)), l),
    ];
    extrema_expand.extend(invoke.clone());
    let extrema_forward = vec![branch(
        lt(int(MAX_APPLY_ARGUMENTS), var(arg_length)),
        fail.clone(),
        extrema_expand,
        l,
    )];
    // Source and bind keep the fixed four-slot frame; only the extrema arm
    // uses the dynamic 4 + R reservation. Every other intrinsic stays on the
    // empty-array fallback below. Function.prototype (256) is never an
    // extrema code and is never callable.
    array_args.push(branch(
        and(
            or(
                var(source),
                eq(var(target), int(super::intrinsics::FUNCTION_BIND_CODE)),
            ),
            and(lt(int(1), var(count)), var(dense)),
        ),
        forward,
        vec![branch(
            and(
                super::math::extrema_target(var(target)),
                and(lt(int(1), var(count)), var(dense)),
            ),
            extrema_forward,
            // The else arm keeps the original intrinsic shape: exactly two
            // arguments, a null thisArg, and an empty dense array. Every
            // ingredient is required: `arg_length` defaults to zero for a
            // non-array, so an `{length:0}` object or a zero-length typed
            // array would otherwise slip through. Extrema targets never reach
            // this arm.
            vec![branch(
                and(
                    and(
                        not(var(source)),
                        not(eq(
                            var(target),
                            int(super::intrinsics::FUNCTION_BIND_CODE),
                        )),
                    ),
                    and(
                        not(eq(
                            var(target),
                            int(super::intrinsics::FUNCTION_PROTOTYPE_CODE),
                        )),
                        and(
                            var(dense),
                            and(
                                eq(var(count), int(2)),
                                and(
                                    tag_is(var(this_arg), value::NULL),
                                    eq(var(arg_length), int(0)),
                                ),
                            ),
                        ),
                    ),
                ),
                invoke,
                fail.clone(),
                l,
            )],
            l,
        )],
        l,
    ));
    let mut callable = vec![
        assign(base, e.base(var(receiver)), l),
        assign(kind, e.decoded(var(base)), l),
    ];
    // The code cell is read only after the kind gate: ordinary objects do not
    // carry a function header, and the 256 boundary keeps Function.prototype
    // out of the source-id classification used by the forward arm.
    let function = vec![
        assign(target, e.decoded(add(var(base), int(5))), l),
        assign(
            source,
            lt(
                var(target),
                int(super::intrinsics::SOURCE_FUNCTION_CODE_LIMIT),
            ),
            l,
        ),
        branch(
            tag_is(var(arg_array), value::OBJECT),
            array_args,
            fail.clone(),
            l,
        ),
    ];
    callable.push(branch(
        eq(var(kind), int(layout::KIND_FUNCTION)),
        function,
        fail.clone(),
        l,
    ));
    body.push(branch(
        tag_is(var(receiver), value::OBJECT),
        callable,
        fail,
        l,
    ));
    body.push(e.effect(heap.root_leave, vec![var(c), var(frame)])?);
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        "__compat_es5_function_apply",
        index,
        vec![c, code, receiver, args],
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
    Ok(())
}

/// Admitted `Function.prototype.bind` behind private codes 270 and 354.
///
/// The helper also receives the real callee box: the call carries the bound
/// object as callee with an unrelated receiver, so the bound target cannot
/// travel in the receiver slot. Two shapes succeed and anything else keeps
/// the existing throw. The callee's private header code cell selects which
/// shape applies: the method node itself carries
/// `FUNCTION_BIND_METHOD_CODE` (354), a bound product keeps code
/// `FUNCTION_BIND_CODE` (270) because dispatch routes its calls back here.
///
/// Creation admits the method callee and a callable non-method function
/// receiver (a bound product included, so `b.bind(...)` re-binds). `thisArg`
/// is `args[0]` (any value; `undefined` when the argument is missing) and
/// every later argument is a bound argument.
///
/// The bound object is a distinct function node that inherits from the realm
/// `Function.prototype`: the prototype pair (cells 3/4) names that intrinsic,
/// so `Object.getPrototypeOf(b) === Function.prototype` and a guest
/// `Object.setPrototypeOf(b, otherFn)` can only rewrite the inherited row
/// chain, never the call target. Both the target function and the bound
/// payload travel in the function header's private environment pair (cells
/// 6/7, traced by the collector as `KIND_ENVIRONMENT`): entry 0 is one
/// private tagged Array holding `[thisArg, boundArg0, ...]`, entry 1 is the
/// target function itself. Neither pair nor Array is an own row, so
/// `delete bound.length`, a rewritten row, or `Object.setPrototypeOf` can
/// never drop a bound argument or retarget a call; both edges are ordinary
/// traced pairs.
///
/// Own rows keep the pinned shape: none at all for `length` (the inherited
/// `Function.prototype.length` is the Number 0 the product reports, so
/// `hasOwnProperty("length")` is false and `Object.keys` is empty), plus the
/// `bound` name row only under the existing name/toString gate.
///
/// Invocation rebuilds the argument container as the bound arguments
/// followed by the call's own arguments and re-enters dispatch with the
/// stored `thisArg`. The callee's own `this` and every later thisArg
/// (`.call`, `.apply`, another `bind`) are ignored while their argument
/// vectors still append.
fn append_function_bind(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    index: u16,
    dispatch: u16,
) -> Result<(), ParseError> {
    let c = lower.slot(span)?;
    let code = lower.slot(span)?;
    let receiver = lower.slot(span)?;
    let args = lower.slot(span)?;
    let callee = lower.slot(span)?;
    let heap = lower.heap_ref(span)?.helpers;
    let intrinsic = lower.intrinsics.expect("intrinsics");
    // The name/toString gate selects only the optional `name` row. `length`
    // has no own row at all: the product inherits the realm
    // `Function.prototype.length` (Number 0), exactly like the pinned engine's
    // bound function object.
    let function_names = lower.function_name_used.get() || lower.function_to_string_used.get();
    let mut e = Emit::new(lower, span, c)?;
    let l = e.line;
    let sp = e.lower.ir_span(span);
    let out = e.temp(
        value::logical_result_schema(),
        value::result_ok(value::completion_normal(), sp),
    )?;
    let frame = e.integer()?;
    let base = e.integer()?;
    let kind = e.integer()?;
    let cbase = e.integer()?;
    let count = e.integer()?;
    let this_arg = e.boxed()?;
    let proto = e.boxed()?;
    let bound = e.boxed()?;
    let bound_name_key = e.boxed()?;
    let bound_base = e.integer()?;
    let function_rows = i64::from(function_names);
    // Private payload Array `[thisArg, boundArg0, ...]`, the environment
    // record that owns it, and the bound node itself.
    let payload_alloc = e.temp(
        value::allocation_schema(),
        value::allocation(int(0), int(0), int(0)),
    )?;
    let payload_base = e.integer()?;
    let payload_box = e.boxed()?;
    let payload_length = e.integer()?;
    let env_alloc = e.temp(
        value::allocation_schema(),
        value::allocation(int(0), int(0), int(0)),
    )?;
    let env_base = e.integer()?;
    let env_box = e.boxed()?;
    let bound_alloc = e.temp(
        value::allocation_schema(),
        value::allocation(int(0), int(0), int(0)),
    )?;
    let item = e.boxed()?;
    let offset = e.integer()?;
    let cursor = e.integer()?;
    // Invocation: the callee's private environment pair and its payload.
    let env_slot = e.integer()?;
    let env_gen = e.integer()?;
    let pbase = e.integer()?;
    let payload_slot = e.integer()?;
    let payload_gen = e.integer()?;
    let tslot = e.integer()?;
    let tgen = e.integer()?;
    let tbase = e.integer()?;
    let tkind = e.integer()?;
    let target = e.boxed()?;
    let combined = e.temp(
        T::Array(Box::new(value::schema())),
        call(B::ArrayNew.call_index(), Vec::new()),
    )?;
    let raw = e.temp(
        value::logical_result_schema(),
        value::result_ok(value::completion_normal(), sp),
    )?;
    let ok = e.temp(value::completion_schema(), value::completion_normal())?;
    let error = e.boxed()?;
    let payload = e.boxed()?;
    let get_at = |index: E| {
        call(
            B::Get.call_index(),
            vec![E::Borrow(Box::new(var(args))), index],
        )
    };
    let mut body = vec![
        assert_stmt(
            or(
                eq(var(code), int(super::intrinsics::FUNCTION_BIND_CODE)),
                eq(
                    var(code),
                    int(super::intrinsics::FUNCTION_BIND_METHOD_CODE),
                ),
            ),
            l,
        ),
        // 0 receiver, 1 thisArg, 2 bound/name, 3 error/payload, 4 the private
        // payload Array, 5 the environment record that owns it.
        assign(frame, call(heap.root_enter, vec![var(c), int(6)]), l),
        e.root(var(frame), int(0), var(receiver))?,
    ];
    let fail = vec![
        assign(error, call(intrinsic.type_error, vec![var(c)]), l),
        e.root(var(frame), int(3), var(error))?,
        assign(out, value::result_err(var(error), sp), l),
    ];
    // Creation: the bound payload Array, then the checked environment record
    // that owns it, then the bound node. Nothing allocates between a
    // `node_begin` and its matching `node_publish`, so every collection point
    // sees either an empty pair or a complete edge. The target stays in the
    // rooted `receiver` local until the environment record commits it, and the
    // prototype pair names the realm `Function.prototype`, never a guest
    // value.
    let mut create = vec![
        e.root(var(frame), int(1), var(receiver))?,
        assign(proto, call(intrinsic.root, vec![var(c), int(1)]), l),
    ];
    let mut bound_name = None;
    if function_names {
        let name_key = intrinsic
            .function_name_key
            .ok_or_else(|| e.lower.error(span, "function name key helper missing"))?;
        bound_name = Some(super::intrinsics::string(
            &mut e,
            "bound",
            frame,
            2,
            &mut create,
        )?);
        create.push(assign(bound_name_key, call(name_key, vec![var(c)]), l));
    }
    // `count` is the whole argument vector: `[thisArg, boundArg0, ...]`. A
    // missing first argument still occupies entry 0 so the fixed `this`
    // (undefined) has a home; the bound Array always has at least one
    // element.
    create.push(assign(
        payload_length,
        choose(lt(var(count), int(1)), int(1), var(count)),
        l,
    ));
    create.push(assign(
        payload_alloc,
        call(
            heap.node_begin,
            vec![
                var(c),
                add(
                    int(layout::ARRAY_HEADER_CELLS),
                    mul(var(payload_length), int(layout::ARRAY_TAGGED_CELLS_PER_ENTRY)),
                ),
                int(layout::KIND_TAGGED_ARRAY),
            ],
        ),
        l,
    ));
    create.push(assign(payload_base, field(var(payload_alloc), "base"), l));
    for (cell, payload) in [
        (2, var(payload_length)),
        (layout::ARRAY_PROTO_OFFSET, int(0)),
        (layout::ARRAY_PROTO_OFFSET + 1, int(0)),
        (
            layout::ARRAY_PRESENT_OFFSET,
            add(var(payload_length), int(1)),
        ),
    ] {
        create.push(e.write(add(var(payload_base), int(cell)), payload)?);
    }
    let mut fill_payload = vec![
        assign(
            item,
            choose(
                eq(var(cursor), int(0)),
                var(this_arg),
                get_at(var(cursor)),
            ),
            l,
        ),
        assign(
            offset,
            add(
                var(payload_base),
                add(
                    int(layout::ARRAY_HEADER_CELLS),
                    mul(var(cursor), int(layout::ARRAY_TAGGED_CELLS_PER_ENTRY)),
                ),
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
        fill_payload.push(e.write(add(var(offset), int(cell)), payload)?);
    }
    fill_payload.push(assign(cursor, add(var(cursor), int(1)), l));
    create.push(assign(cursor, int(0), l));
    create.push(S::While {
        condition: lt(var(cursor), var(payload_length)),
        body: fill_payload,
        line: l,
    });
    create.push(assign(
        payload_box,
        call(
            heap.node_publish,
            vec![
                var(c),
                field(var(payload_alloc), "slot"),
                field(var(payload_alloc), "generation"),
                var(payload_base),
                var(frame),
                int(4),
            ],
        ),
        l,
    ));
    // Two-entry `KIND_ENVIRONMENT` record: entry 0 names the payload Array,
    // entry 1 names the target function. The function header pair is traced
    // with the environment kind expectation, so this is the only node shape
    // that can legally live in cells 6/7. Both boxed entries carry the same
    // tag/number/flag/slot/generation encoding the collector scans.
    create.push(assign(
        env_alloc,
        call(
            heap.node_begin,
            vec![
                var(c),
                int(layout::ENV_HEADER_CELLS + 2 * layout::ENV_CELLS_PER_ENTRY),
                int(layout::KIND_ENVIRONMENT),
            ],
        ),
        l,
    ));
    create.push(assign(env_base, field(var(env_alloc), "base"), l));
    for (cell, payload) in [
        (2, int(2)),
        (3, int(0)),
        (4, int(0)),
        (layout::ENV_HEADER_CELLS, int(value::OBJECT)),
        (layout::ENV_HEADER_CELLS + 1, E::Float(0.0)),
        (layout::ENV_HEADER_CELLS + 2, int(0)),
        (
            layout::ENV_HEADER_CELLS + 3,
            field(var(payload_box), "ref_slot"),
        ),
        (
            layout::ENV_HEADER_CELLS + 4,
            field(var(payload_box), "generation"),
        ),
        (
            layout::ENV_HEADER_CELLS + layout::ENV_CELLS_PER_ENTRY,
            int(value::OBJECT),
        ),
        (
            layout::ENV_HEADER_CELLS + layout::ENV_CELLS_PER_ENTRY + 1,
            E::Float(0.0),
        ),
        (
            layout::ENV_HEADER_CELLS + layout::ENV_CELLS_PER_ENTRY + 2,
            int(0),
        ),
        (
            layout::ENV_HEADER_CELLS + layout::ENV_CELLS_PER_ENTRY + 3,
            field(var(receiver), "ref_slot"),
        ),
        (
            layout::ENV_HEADER_CELLS + layout::ENV_CELLS_PER_ENTRY + 4,
            field(var(receiver), "generation"),
        ),
    ] {
        create.push(e.write(add(var(env_base), int(cell)), payload)?);
    }
    // The environment record is not an object-like guest value: it publishes
    // through the internal edge variant with the environment root category,
    // which is exactly the expectation the collector applies to it.
    create.push(e.effect(
        heap.node_publish_edge,
        vec![
            var(c),
            field(var(env_alloc), "slot"),
            field(var(env_alloc), "generation"),
            var(env_base),
            var(frame),
            int(5),
            int(layout::ROOT_INTERNAL),
        ],
    )?);
    // The bound node: Function.prototype in the prototype pair, the private
    // environment pair in cells 6/7, no `length` row at all and the gated
    // `name` row as the only own property. Every cell is written before
    // publication.
    create.push(assign(
        bound_alloc,
        call(
            heap.node_begin,
            vec![
                var(c),
                int(
                    layout::FUNCTION_CELLS
                        + function_rows * layout::OBJECT_CELLS_PER_PROPERTY,
                ),
                int(layout::KIND_FUNCTION),
            ],
        ),
        l,
    ));
    create.push(assign(bound_base, field(var(bound_alloc), "base"), l));
    for (cell, payload) in [
        (
            2,
            int(function_rows * layout::OBJECT_COUNT_FACTOR + layout::OBJECT_EXTENSIBLE_FLAG),
        ),
        (3, field(var(proto), "ref_slot")),
        (4, field(var(proto), "generation")),
        (5, int(super::intrinsics::FUNCTION_BIND_CODE)),
        (6, field(var(env_alloc), "slot")),
        (7, field(var(env_alloc), "generation")),
        (8, int(1)),
        (9, int(0)),
    ] {
        create.push(e.write(add(var(bound_base), int(cell)), payload)?);
    }
    if let Some(bound_name) = bound_name {
        for (cell, payload) in [
            (0, field(var(bound_name_key), "ref_slot")),
            (1, field(var(bound_name_key), "generation")),
            (2, int(value::STRING * layout::OBJECT_TAG_FACTOR)),
            (3, int(0)),
            (4, int(0)),
            (5, field(var(bound_name), "ref_slot")),
            (6, field(var(bound_name), "generation")),
        ] {
            create.push(e.write(
                add(var(bound_base), int(layout::FUNCTION_CELLS + cell)),
                payload,
            )?);
        }
    }
    create.push(assign(
        bound,
        call(
            heap.node_publish,
            vec![
                var(c),
                field(var(bound_alloc), "slot"),
                field(var(bound_alloc), "generation"),
                var(bound_base),
                var(frame),
                int(2),
            ],
        ),
        l,
    ));
    create.push(assign(
        out,
        value::result_ok(value::completion_return(var(bound)), sp),
        l,
    ));
    let ok_body = vec![
        assign(
            payload,
            choose(
                eq(
                    field(var(ok), "kind"),
                    int(value::COMPLETION_RETURN),
                ),
                field(var(ok), "value"),
                value::undefined(),
            ),
            l,
        ),
        e.root(var(frame), int(3), var(payload))?,
        assign(
            out,
            value::result_ok(value::completion_return(var(payload)), sp),
            l,
        ),
    ];
    let error_body = vec![
        e.root(var(frame), int(3), var(error))?,
        assign(out, value::result_err(var(error), sp), l),
    ];
    // Invocation: the product's cells 6/7 name the private environment
    // record; entry 0 owns the payload Array and entry 1 owns the original
    // target function. Neither lives in the prototype pair, so the guest
    // cannot retarget a call by rewriting cells 3/4.
    let mut invoke = vec![
        assign(env_slot, e.decoded(add(var(cbase), int(6))), l),
        assign(env_gen, e.generation(add(var(cbase), int(7))), l),
        assign(
            env_box,
            value::dynamic_value(
                int(value::OBJECT),
                E::Float(0.0),
                E::Bool(false),
                var(env_slot),
                var(env_gen),
            ),
            l,
        ),
        // The environment edge itself is not a guest-visible value and never
        // needs a frame root: the callee owns it through cells 6/7, and no
        // allocation runs between these reads and the target/payload roots
        // below. Both edges are traversed through `checked_base`, so a
        // retired or mismatched pair traps here instead of being read as raw
        // cells.
        assign(env_base, e.base(var(env_box)), l),
        assert_stmt(
            eq(
                e.decoded(add(var(env_base), int(layout::ENV_HEADER_CELLS))),
                int(value::OBJECT),
            ),
            l,
        ),
        assign(
            payload_slot,
            e.decoded(add(var(env_base), int(layout::ENV_HEADER_CELLS + 3))),
            l,
        ),
        assign(
            payload_gen,
            e.generation(add(var(env_base), int(layout::ENV_HEADER_CELLS + 4))),
            l,
        ),
        // Entry 1 is the original target. Storing it only here means a guest
        // `Object.setPrototypeOf(b, otherFn)` cannot change what `b()` calls.
        assert_stmt(
            eq(
                e.decoded(add(
                    var(env_base),
                    int(layout::ENV_HEADER_CELLS + layout::ENV_CELLS_PER_ENTRY),
                )),
                int(value::OBJECT),
            ),
            l,
        ),
        assign(
            tslot,
            e.decoded(add(
                var(env_base),
                int(layout::ENV_HEADER_CELLS + layout::ENV_CELLS_PER_ENTRY + 3),
            )),
            l,
        ),
        assign(
            tgen,
            e.generation(add(
                var(env_base),
                int(layout::ENV_HEADER_CELLS + layout::ENV_CELLS_PER_ENTRY + 4),
            )),
            l,
        ),
        assign(
            target,
            value::dynamic_value(
                int(value::OBJECT),
                E::Float(0.0),
                E::Bool(false),
                var(tslot),
                var(tgen),
            ),
            l,
        ),
        e.root(var(frame), int(4), var(target))?,
        assign(tbase, e.base(var(target)), l),
        assign(tkind, e.decoded(var(tbase)), l),
    ];
    let mut forward = vec![
        assign(
            payload_box,
            value::dynamic_value(
                int(value::OBJECT),
                E::Float(0.0),
                E::Bool(false),
                var(payload_slot),
                var(payload_gen),
            ),
            l,
        ),
        e.root(var(frame), int(5), var(payload_box))?,
        assign(pbase, e.base(var(payload_box)), l),
        assign(
            payload_length,
            call(
                e.decode_u32,
                vec![call(heap.array_length, vec![var(c), var(pbase)])],
            ),
            l,
        ),
        // Entry 0 is the fixed `thisArg`; it is restored into the local the
        // preamble already rooted so the collector never sees a gap.
        assign(
            this_arg,
            call(heap.array_get, vec![var(c), var(pbase), int(0)]),
            l,
        ),
        e.root(var(frame), int(1), var(this_arg))?,
        assign(combined, call(B::ArrayNew.call_index(), Vec::new()), l),
        assign(cursor, int(1), l),
        S::While {
            condition: lt(var(cursor), var(payload_length)),
            body: vec![
                assign(
                    combined,
                    call(
                        B::ArrayPush.call_index(),
                        vec![
                            E::MoveVar(combined),
                            call(
                                heap.array_get,
                                vec![var(c), var(pbase), var(cursor)],
                            ),
                        ],
                    ),
                    l,
                ),
                assign(cursor, add(var(cursor), int(1)), l),
            ],
            line: l,
        },
        assign(cursor, int(0), l),
        S::While {
            condition: lt(var(cursor), var(count)),
            body: vec![
                assign(
                    combined,
                    call(
                        B::ArrayPush.call_index(),
                        vec![E::MoveVar(combined), get_at(var(cursor))],
                    ),
                    l,
                ),
                assign(cursor, add(var(cursor), int(1)), l),
            ],
            line: l,
        },
        assign(
            raw,
            call(
                dispatch,
                vec![var(c), var(target), var(this_arg), var(combined)],
            ),
            l,
        ),
    ];
    forward.push(matched(
        &mut e,
        raw,
        value::logical_result_schema(),
        ok,
        error,
        ok_body,
        error_body,
    )?);
    invoke.push(branch(
        eq(var(tkind), int(layout::KIND_FUNCTION)),
        forward,
        fail.clone(),
        l,
    ));
    // The receiver kind is the only creation witness: the private code cell
    // may only be read after the header has been proven to be a function
    // node, because a zero-row ordinary object has no cell 5 at all. Every
    // callable target is legal, including the bind method itself: binding
    // `f.bind` produces a product that re-enters this creation shape with the
    // product's fixed `thisArg` as the new target, exactly like the pinned
    // `f.bind.bind(f, null)()()` chain.
    let mut create_gate = vec![
        assign(base, e.base(var(receiver)), l),
        assign(kind, e.decoded(var(base)), l),
    ];
    create_gate.push(branch(
        eq(var(kind), int(layout::KIND_FUNCTION)),
        create,
        fail.clone(),
        l,
    ));
    body.push(assign(
        count,
        call(B::Len.call_index(), vec![E::Borrow(Box::new(var(args)))]),
        l,
    ));
    body.push(assign(
        this_arg,
        choose(
            lt(int(0), var(count)),
            call(
                B::Get.call_index(),
                vec![E::Borrow(Box::new(var(args))), int(0)],
            ),
            value::undefined(),
        ),
        l,
    ));
    body.push(e.root(var(frame), int(1), var(this_arg))?);
    body.push(assign(cbase, e.base(var(callee)), l));
    // The private code cell of the callee selects the shape. The method
    // node (354) only creates; a bound product (270) only re-enters
    // dispatch. A guest can add rows or rewrite the prototype pair of the
    // method, but neither touches the code cell written by the allocator,
    // so it can never masquerade as a bound product.
    body.push(branch(
        eq(var(code), int(super::intrinsics::FUNCTION_BIND_METHOD_CODE)),
        vec![branch(
            tag_is(var(receiver), value::OBJECT),
            create_gate,
            fail.clone(),
            l,
        )],
        vec![branch(
            tag_is(var(callee), value::OBJECT),
            invoke,
            fail.clone(),
            l,
        )],
        l,
    ));
    body.push(e.effect(heap.root_leave, vec![var(c), var(frame)])?);
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        "__compat_es5_function_bind",
        index,
        vec![c, code, receiver, args, callee],
        vec![
            carrier_schema.clone(),
            T::Int,
            value::schema(),
            T::Array(Box::new(value::schema())),
            value::schema(),
        ],
        value::logical_result_schema(),
        body,
        var(out),
        l,
    );
    Ok(())
}

fn append_api(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    h: ObjectHelpers,
) -> Result<(), ParseError> {
    let carrier = lower.slot(span)?;
    let code = lower.slot(span)?;
    let args = lower.slot(span)?;
    let heap = lower.heap_ref(span)?.helpers;
    let intrinsic = lower.intrinsics.expect("intrinsics");
    let mut e = Emit::new(lower, span, carrier)?;
    let l = e.line;
    let source_span = e.lower.ir_span(span);
    let values = result_schema(value::schema());
    let descs = result_schema(descriptor_schema());
    let bools = result_schema(T::Bool);
    let frame = e.integer()?;
    let target = e.boxed()?;
    let key = e.boxed()?;
    let attrs = e.boxed()?;
    let answer = e.boxed()?;
    let error = e.boxed()?;
    let failed = e.boolean()?;
    let base = e.integer()?;
    let slot = e.integer()?;
    let generation = e.integer()?;
    let kind = e.integer()?;
    let new_slot = e.integer()?;
    let new_gen = e.integer()?;
    let ack = e.integer()?;
    let proto_base = e.integer()?;
    let proto_kind = e.integer()?;
    let d = e.temp(
        own_schema(),
        own(E::Bool(false), empty_descriptor(), int(0)),
    )?;
    let desc = e.temp(descriptor_schema(), empty_descriptor())?;
    let ok = e.boolean()?;
    let desc_raw = e.temp(
        descs.clone(),
        result(descs.clone(), true, empty_descriptor(), source_span),
    )?;
    let value_raw = e.temp(
        values.clone(),
        result(values.clone(), true, value::undefined(), source_span),
    )?;
    let bool_raw = e.temp(
        bools.clone(),
        result(bools.clone(), true, E::Bool(true), source_span),
    )?;
    let out = e.temp(
        value::logical_result_schema(),
        value::result_ok(value::completion_normal(), source_span),
    )?;
    let actual = |ordinal: i64| {
        choose(
            lt(
                int(ordinal),
                call(B::Len.call_index(), vec![E::Borrow(Box::new(var(args)))]),
            ),
            call(
                B::Get.call_index(),
                vec![E::Borrow(Box::new(var(args))), int(ordinal)],
            ),
            value::undefined(),
        )
    };
    let mut body = vec![
        assign(frame, call(heap.root_enter, vec![var(carrier), int(8)]), l),
        assign(target, actual(0), l),
        assign(key, actual(1), l),
        assign(attrs, actual(2), l),
    ];
    for (i, payload) in [var(target), var(key), var(attrs)].into_iter().enumerate() {
        body.push(e.root(var(frame), int(i as i64), payload)?);
    }
    let fail = |e: &mut Emit<'_, '_>| -> Result<Vec<S>, ParseError> {
        Ok(vec![
            e.root(var(frame), int(7), var(error))?,
            assign(failed, E::Bool(true), l),
        ])
    };
    let type_error = |e: &mut Emit<'_, '_>| -> Result<Vec<S>, ParseError> {
        let mut b = vec![assign(
            error,
            call(intrinsic.type_error, vec![var(carrier)]),
            l,
        )];
        b.extend(fail(e)?);
        Ok(b)
    };
    let mut key_conversion = Vec::new();
    let undefined_key =
        super::intrinsics::string(&mut e, "undefined", frame, 1, &mut key_conversion)?;
    key_conversion.push(assign(key, var(undefined_key), l));
    let key_conversion = vec![
        branch(
            tag_is(var(key), value::UNDEFINED),
            key_conversion,
            Vec::new(),
            l,
        ),
        assert_stmt(tag_is(var(key), value::STRING), l),
    ];
    let mut branches = Vec::new();
    // Bare Object call: object identity, or a fresh object for null/undefined.
    let mut make = vec![assign(
        answer,
        call(
            intrinsic.allocate,
            vec![
                var(carrier),
                int(3),
                call(intrinsic.root, vec![var(carrier), int(0)]),
                int(0),
                int(0),
                int(0), int(0),
                var(frame),
                int(6),
            ],
        ),
        l,
    )];
    let invalid = type_error(&mut e)?;
    make = vec![branch(
        or(
            tag_is(var(target), value::NULL),
            tag_is(var(target), value::UNDEFINED),
        ),
        make,
        invalid,
        l,
    )];
    branches.push((
        super::intrinsics::OBJECT_CODE,
        vec![branch(
            tag_is(var(target), value::OBJECT),
            vec![assign(answer, var(target), l)],
            make,
            l,
        )],
    ));
    // Object.create uses the internal two-phase helper, never a mutable public method.
    let mut create = vec![assign(
        answer,
        call(
            intrinsic.allocate,
            vec![
                var(carrier),
                int(3),
                var(target),
                int(0),
                int(0),
                int(0), int(0),
                var(frame),
                int(6),
            ],
        ),
        l,
    )];
    let mut properties = vec![assign(
        value_raw,
        call(
            h.define_properties,
            vec![var(carrier), var(answer), var(key)],
        ),
        l,
    )];
    let failure = fail(&mut e)?;
    properties.push(matched(
        &mut e,
        value_raw,
        values.clone(),
        answer,
        error,
        Vec::new(),
        failure,
    )?);
    create.push(branch(
        not(tag_is(var(key), value::UNDEFINED)),
        properties,
        Vec::new(),
        l,
    ));
    let invalid = type_error(&mut e)?;
    branches.push((
        258,
        vec![branch(
            or(
                tag_is(var(target), value::OBJECT),
                tag_is(var(target), value::NULL),
            ),
            create,
            invalid,
            l,
        )],
    ));
    let mut define = key_conversion.clone();
    define.push(assign(
        desc_raw,
        call(h.to_desc, vec![var(carrier), var(attrs)]),
        l,
    ));
    let mut converted = Vec::new();
    for (i, name) in ["value", "get", "set"].into_iter().enumerate() {
        converted.push(e.root(var(frame), int(3 + i as i64), field(var(desc), name))?);
    }
    converted.push(assign(
        bool_raw,
        call(
            h.define,
            vec![
                var(carrier),
                var(target),
                var(key),
                var(desc),
                E::Bool(true),
            ],
        ),
        l,
    ));
    let failure = fail(&mut e)?;
    converted.push(matched(
        &mut e,
        bool_raw,
        bools,
        ok,
        error,
        vec![assign(answer, var(target), l)],
        failure,
    )?);
    let failure = fail(&mut e)?;
    define.push(matched(
        &mut e, desc_raw, descs, desc, error, converted, failure,
    )?);
    branches.push((259, define));
    let mut definitions = vec![assign(
        value_raw,
        call(
            h.define_properties,
            vec![var(carrier), var(target), var(key)],
        ),
        l,
    )];
    let failure = fail(&mut e)?;
    definitions.push(matched(
        &mut e,
        value_raw,
        values.clone(),
        answer,
        error,
        Vec::new(),
        failure,
    )?);
    branches.push((260, definitions));
    let mut own = key_conversion;
    own.extend([
        assign(d, call(h.own, vec![var(carrier), var(target), var(key)]), l),
        assign(value_raw, call(h.from_desc, vec![var(carrier), var(d)]), l),
    ]);
    let failure = fail(&mut e)?;
    own.push(matched(
        &mut e,
        value_raw,
        values,
        answer,
        error,
        Vec::new(),
        failure,
    )?);
    branches.push((261, own));
    let proto = vec![
        assign(base, e.base(var(target)), l),
        assign(slot, e.decoded(add(var(base), int(3))), l),
        assign(generation, e.generation(add(var(base), int(4))), l),
        assert_stmt(
            or(not(eq(var(generation), int(0))), eq(var(slot), int(0))),
            l,
        ),
        assign(
            answer,
            choose(
                eq(var(generation), int(0)),
                value::null(),
                value::dynamic_value(
                    int(value::OBJECT),
                    E::Float(0.0),
                    E::Bool(false),
                    var(slot),
                    var(generation),
                ),
            ),
            l,
        ),
    ];
    branches.push((262, proto));
    let mut prevent = vec![assign(base, e.base(var(target)), l)];
    let header = property_header(&mut e, base, &mut prevent)?;
    prevent.push(e.write(
        add(var(base), int(2)),
        pack_object_header(var(header.count), E::Bool(false), var(header.error), var(header.error_instance)),
    )?);
    prevent.push(assign(answer, var(target), l));
    branches.push((263, prevent));
    let mut extensible = vec![assign(base, e.base(var(target)), l)];
    let header = property_header(&mut e, base, &mut extensible)?;
    extensible.push(assign(answer, value::boolean(var(header.extensible)), l));
    branches.push((264, extensible));
    // setPrototypeOf: same pair is a no-op before the extensibility check.
    // Null is stored here; it must not enter checked_base. Object protos reuse
    // the existing cycle-checked setter. Arrays never decode offset 2.
    let mut set_proto = vec![
        assign(base, e.base(var(target)), l),
        assign(kind, e.decoded(var(base)), l),
        assert_stmt(object_like(var(kind)), l),
        assign(slot, e.decoded(add(var(base), int(3))), l),
        assign(generation, e.generation(add(var(base), int(4))), l),
        assert_stmt(
            or(not(eq(var(generation), int(0))), eq(var(slot), int(0))),
            l,
        ),
    ];
    set_proto.push(branch(
        tag_is(var(key), value::NULL),
        vec![assign(new_slot, int(0), l), assign(new_gen, int(0), l)],
        vec![branch(
            tag_is(var(key), value::OBJECT),
            vec![
                assign(proto_base, e.base(var(key)), l),
                assign(proto_kind, e.decoded(var(proto_base)), l),
                assert_stmt(object_like(var(proto_kind)), l),
                assign(new_slot, field(var(key), "ref_slot"), l),
                assign(new_gen, field(var(key), "generation"), l),
            ],
            type_error(&mut e)?,
            l,
        )],
        l,
    ));
    let null_store = vec![
        e.write(add(var(base), int(3)), int(0))?,
        e.write(add(var(base), int(4)), int(0))?,
        assign(answer, var(target), l),
    ];
    let mut link = vec![assign(
        ack,
        call(
            heap.set_prototype,
            vec![var(carrier), var(target), var(key)],
        ),
        l,
    )];
    link.push(branch(
        eq(var(ack), int(0)),
        type_error(&mut e)?,
        vec![assign(answer, var(target), l)],
        l,
    ));
    let array_change = vec![branch(tag_is(var(key), value::NULL), null_store, link, l)];
    let mut ordinary = Vec::new();
    let set_header = property_header(&mut e, base, &mut ordinary)?;
    let null_store = vec![
        e.write(add(var(base), int(3)), int(0))?,
        e.write(add(var(base), int(4)), int(0))?,
        assign(answer, var(target), l),
    ];
    let mut link = vec![assign(
        ack,
        call(
            heap.set_prototype,
            vec![var(carrier), var(target), var(key)],
        ),
        l,
    )];
    link.push(branch(
        eq(var(ack), int(0)),
        type_error(&mut e)?,
        vec![assign(answer, var(target), l)],
        l,
    ));
    ordinary.push(branch(
        var(set_header.extensible),
        vec![branch(tag_is(var(key), value::NULL), null_store, link, l)],
        type_error(&mut e)?,
        l,
    ));
    set_proto.push(branch(
        not(var(failed)),
        vec![branch(
            and(
                eq(var(slot), var(new_slot)),
                eq(var(generation), var(new_gen)),
            ),
            vec![assign(answer, var(target), l)],
            vec![branch(
                or(
                    eq(var(kind), int(layout::KIND_ARRAY)),
                    eq(var(kind), int(layout::KIND_TAGGED_ARRAY)),
                ),
                array_change,
                ordinary,
                l,
            )],
            l,
        )],
        Vec::new(),
        l,
    ));
    branches.push((super::intrinsics::SET_PROTOTYPE_OF_CODE, set_proto));
    // Object.keys: own enumerable String keys as one real dense String array.
    // The ordinary/missing check is the shared `tag_is(target, OBJECT)` gate
    // above, so primitives and the zero-argument call throw the managed
    // TypeError before any node is pending.
    branches.push((
        super::intrinsics::KEYS_CODE,
        vec![assign(
            answer,
            call(h.keys, vec![var(carrier), var(target), var(frame), int(6)]),
            l,
        )],
    ));
    let mut tail = vec![assert_stmt(E::Bool(false), l)];
    for (code_id, action) in branches.into_iter().rev() {
        let action = if code_id >= 259 {
            let invalid = type_error(&mut e)?;
            vec![branch(
                tag_is(var(target), value::OBJECT),
                action,
                invalid,
                l,
            )]
        } else {
            action
        };
        tail = vec![branch(eq(var(code), int(code_id)), action, tail, l)];
    }
    body.extend(tail);
    body.push(e.root(var(frame), int(6), var(answer))?);
    body.push(assign(
        out,
        choose(
            var(failed),
            value::result_err(var(error), source_span),
            value::result_ok(value::completion_return(var(answer)), source_span),
        ),
        l,
    ));
    body.push(e.effect(heap.root_leave, vec![var(carrier), var(frame)])?);
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        "__compat_es5_object_intrinsic_call",
        h.api,
        vec![carrier, code, args],
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

/// Admitted `JSON.stringify` behind dispatch code 271.
///
/// `JSON` is an ordinary global object and `stringify` its one own data
/// property, so the unchanged ordinary Get finds this function on the real
/// object. Only the admitted shape succeeds: a callable function value, the
/// ordinary `JSON` object as receiver, exactly one argument, and that argument
/// an ordinary Object node or the admitted `/a/` RegExp node whose whole own
/// row set is either empty (the result is the guest String `{}`) or one data
/// property whose key is the one-unit String `a` and whose value is the
/// Number 1 (the result is the guest String `{"a":1}` with no spaces).
///
/// Everything else — `JSON.parse`, other arities (replacer/space), primitives,
/// arrays, functions, accessor or exotic rows, other keys, nested objects and
/// values — keeps the existing TypeError instead of growing the method. Array
/// elements are never enumerated and no Core array is rooted here.
fn append_json_stringify(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    index: u16,
) -> Result<(), ParseError> {
    let c = lower.slot(span)?;
    let code = lower.slot(span)?;
    let receiver = lower.slot(span)?;
    let args = lower.slot(span)?;
    let heap = lower.heap_ref(span)?.helpers;
    let intrinsic = lower.intrinsics.expect("intrinsics");
    let mut e = Emit::new(lower, span, c)?;
    let l = e.line;
    let sp = e.lower.ir_span(span);
    let out = e.temp(
        value::logical_result_schema(),
        value::result_ok(value::completion_normal(), sp),
    )?;
    let frame = e.integer()?;
    let count = e.integer()?;
    let argument = e.boxed()?;
    let base = e.integer()?;
    let kind = e.integer()?;
    let row = e.integer()?;
    let key_length = e.temp(T::Number, E::Float(0.0))?;
    let unit = e.integer()?;
    let error = e.boxed()?;
    let mut body = vec![
        assert_stmt(
            eq(var(code), int(super::intrinsics::JSON_STRINGIFY_CODE)),
            l,
        ),
        assign(frame, call(heap.root_enter, vec![var(c), int(4)]), l),
        e.root(var(frame), int(0), var(receiver))?,
    ];
    let fail = vec![
        assign(error, call(intrinsic.type_error, vec![var(c)]), l),
        e.root(var(frame), int(3), var(error))?,
        assign(out, value::result_err(var(error), sp), l),
    ];
    // The admitted result is a real guest String, published into this helper's
    // own root frame before the completion escapes.
    let mut succeed = Vec::new();
    let text = super::intrinsics::string(&mut e, "{\"a\":1}", frame, 2, &mut succeed)?;
    succeed.push(assign(
        out,
        value::result_ok(value::completion_return(var(text)), sp),
        l,
    ));
    // Only two own row sets are admitted: the empty set and the single data
    // row `a` with the Number 1. The header is decoded once and the row set is
    // never read before its exact count is known.
    let mut ordinary = Vec::new();
    let header = property_header(&mut e, base, &mut ordinary)?;
    // The object must have exactly one own row before that row may be read: a
    // multi-row object never touches row cells. The single row must be a data
    // descriptor whose key is the one-unit String `a` and whose value is the
    // Number 1. The key unit is read only after the one-unit length check, so
    // a shorter or longer key never reads outside its node.
    let mut single = Vec::new();
    single.push(assign(row, add(var(base), var(header.header)), l));
    let (desc, key_base) = decode_row(&mut e, row, &mut single)?;
    single.push(assign(key_length, e.read(add(var(key_base), int(2))), l));
    single.push(assign(unit, int(0), l));
    let mut keyed = Vec::new();
    keyed.push(branch(
        and(
            eq(
                read_string_unit(e.read, e.carrier, key_base, unit),
                E::Float(97.0),
            ),
            and(
                eq(num(field(var(desc), "present")), num(int(51))),
                and(
                    tag_is(field(var(desc), "value"), value::NUMBER),
                    eq(
                        num(field(field(var(desc), "value"), "number")),
                        E::Float(1.0),
                    ),
                ),
            ),
        ),
        succeed,
        fail.clone(),
        l,
    ));
    single.push(branch(
        eq(var(key_length), E::Float(1.0)),
        keyed,
        fail.clone(),
        l,
    ));
    // The empty shape is the only sibling of the unchanged single-row shape:
    // an empty own row set stringifies to the empty text `{}` without touching
    // any row cell, exactly like the admitted RegExp value once its one
    // non-enumerable `test` row is deleted. Its success arm falls out of the
    // helper; every non-empty row set still runs the same one-row check (and
    // its same failures) as the else arm.
    let mut empty = Vec::new();
    let empty_text = super::intrinsics::string(&mut e, "{}", frame, 2, &mut empty)?;
    empty.push(assign(
        out,
        value::result_ok(value::completion_return(var(empty_text)), sp),
        l,
    ));
    ordinary.push(branch(
        and(eq(var(header.count), int(0)), not(var(header.error))),
        empty,
        vec![branch(
            and(eq(var(header.count), int(1)), not(var(header.error))),
            single,
            fail.clone(),
            l,
        )],
        l,
    ));
    body.push(assign(
        count,
        call(B::Len.call_index(), vec![E::Borrow(Box::new(var(args)))]),
        l,
    ));
    body.push(assign(
        argument,
        choose(
            eq(var(count), int(1)),
            call(
                B::Get.call_index(),
                vec![E::Borrow(Box::new(var(args))), int(0)],
            ),
            value::undefined(),
        ),
        l,
    ));
    body.push(e.root(var(frame), int(1), var(argument))?);
    let admitted = vec![
        assign(base, e.base(var(argument)), l),
        assign(kind, e.decoded(var(base)), l),
        branch(
            // The admitted `/a/` RegExp value carries its own private node
            // kind. Its own row set stays ordinary guest-visible data (the
            // non-enumerable `test` row and any row the source adds), so the
            // unchanged shape check below still decides success; only the
            // kind acceptance widens from the ordinary Object node to it.
            or(
                eq(var(kind), int(layout::KIND_OBJECT)),
                eq(var(kind), int(layout::KIND_REGEXP)),
            ),
            ordinary,
            fail.clone(),
            l,
        ),
    ];
    // The receiver must be the very `JSON` object installed at its permanent
    // root entry, not merely some Object: a borrowed `JSON.stringify` called
    // on any other object keeps the existing throw. Both the ref slot and the
    // generation are compared, exactly like the `Math` identity check, so a
    // stale root never matches either.
    if e.lower.json_used.get() {
        let json = e.boxed()?;
        body.push(assign(
            json,
            call(
                intrinsic.root,
                vec![var(c), int(super::intrinsics::json_root(e.lower))],
            ),
            l,
        ));
        body.push(branch(
            and(
                tag_is(var(receiver), value::OBJECT),
                and(
                    and(
                        eq(
                            num(field(var(receiver), "ref_slot")),
                            num(field(var(json), "ref_slot")),
                        ),
                        eq(
                            num(field(var(receiver), "generation")),
                            num(field(var(json), "generation")),
                        ),
                    ),
                    and(eq(var(count), int(1)), tag_is(var(argument), value::OBJECT)),
                ),
            ),
            admitted,
            fail,
            l,
        ));
    } else {
        // Dispatch code 271 only exists on the installed `JSON` object, so a
        // source that never named `JSON` never reaches this helper and no
        // uninstalled root entry is ever read.
        body.extend(fail);
    }
    body.push(e.effect(heap.root_leave, vec![var(c), var(frame)])?);
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        "__compat_es5_json_stringify",
        index,
        vec![c, code, receiver, args],
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
    Ok(())
}

/// Admitted `JSON.parse` behind dispatch code 272.
///
/// `JSON` is an ordinary global object and `parse` its second own data
/// property, so the unchanged ordinary Get finds this function on the real
/// object. Only the admitted shape succeeds: the ordinary `JSON` object as
/// receiver, exactly one argument, and that argument the one guest String
/// `{"a":7}`. The result is a new ordinary object whose single own data
/// property is `a` with the Number 7, so the ordinary `.a` read yields 7.
///
/// Everything else — other strings, extra arguments and revivers, primitives,
/// arrays, nested objects, borrowed functions and any other receiver — keeps
/// the existing TypeError. No Core array is allocated or rooted here.
fn append_json_parse(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    index: u16,
) -> Result<(), ParseError> {
    let c = lower.slot(span)?;
    let code = lower.slot(span)?;
    let receiver = lower.slot(span)?;
    let args = lower.slot(span)?;
    let heap = lower.heap_ref(span)?.helpers;
    let intrinsic = lower.intrinsics.expect("intrinsics");
    let mut e = Emit::new(lower, span, c)?;
    let l = e.line;
    let sp = e.lower.ir_span(span);
    let out = e.temp(
        value::logical_result_schema(),
        value::result_ok(value::completion_normal(), sp),
    )?;
    let frame = e.integer()?;
    let count = e.integer()?;
    let argument = e.boxed()?;
    let base = e.integer()?;
    let length = e.temp(T::Number, E::Float(0.0))?;
    let error = e.boxed()?;
    let parsed = e.boxed()?;
    let mut body = vec![
        assert_stmt(eq(var(code), int(super::intrinsics::JSON_PARSE_CODE)), l),
        assign(frame, call(heap.root_enter, vec![var(c), int(5)]), l),
        e.root(var(frame), int(0), var(receiver))?,
    ];
    let fail = vec![
        assign(error, call(intrinsic.type_error, vec![var(c)]), l),
        e.root(var(frame), int(2), var(error))?,
        assign(out, value::result_err(var(error), sp), l),
    ];
    // The admitted result is a new ordinary object: its `a` key String
    // publishes into this helper's own root frame (entry 3) and the object
    // itself into entry 4 before the one trusted data row is written. Both
    // stay rooted until the completion escapes.
    let mut succeed = Vec::new();
    let key = super::intrinsics::string(&mut e, "a", frame, 3, &mut succeed)?;
    succeed.push(assign(
        parsed,
        call(
            intrinsic.allocate,
            vec![
                var(c),
                int(3),
                call(intrinsic.root, vec![var(c), int(0)]),
                int(1),
                int(0),
                int(0), int(0),
                var(frame),
                int(4),
            ],
        ),
        l,
    ));
    super::intrinsics::trusted_data(
        &mut e,
        var(parsed),
        0,
        var(key),
        value::number(E::Float(7.0)),
        (true, true, true),
        &mut succeed,
    )?;
    succeed.push(assign(
        out,
        value::result_ok(value::completion_return(var(parsed)), sp),
        l,
    ));
    // The argument must be a String whose seven units are exactly `{"a":7}`.
    // The length is checked before any unit is read, so a shorter or longer
    // String never reads outside its node, and each unit is compared through
    // the already checked base.
    let units: [(i64, f64); 7] = [
        (0, 123.0),
        (1, 34.0),
        (2, 97.0),
        (3, 34.0),
        (4, 58.0),
        (5, 55.0),
        (6, 125.0),
    ];
    let mut literal: Option<E> = None;
    for (offset, unit) in units {
        let actual = e.read(add(
            add(var(base), int(layout::STRING_HEADER_CELLS)),
            int(offset),
        ));
        let comparison = eq(actual, E::Float(unit));
        literal = Some(match literal {
            Some(previous) => and(previous, comparison),
            None => comparison,
        });
    }
    // Every one of the seven units is listed above, so the fold is total.
    let literal = literal.expect("JSON.parse literal units");
    let admitted = vec![
        assign(base, e.base(var(argument)), l),
        assign(length, e.read(add(var(base), int(2))), l),
        branch(
            and(eq(var(length), E::Float(7.0)), literal),
            succeed,
            fail.clone(),
            l,
        ),
    ];
    body.push(assign(
        count,
        call(B::Len.call_index(), vec![E::Borrow(Box::new(var(args)))]),
        l,
    ));
    body.push(assign(
        argument,
        choose(
            eq(var(count), int(1)),
            call(
                B::Get.call_index(),
                vec![E::Borrow(Box::new(var(args))), int(0)],
            ),
            value::undefined(),
        ),
        l,
    ));
    body.push(e.root(var(frame), int(1), var(argument))?);
    let string_argument = vec![branch(
        tag_is(var(argument), value::STRING),
        admitted,
        fail.clone(),
        l,
    )];
    // The receiver must be the very `JSON` object installed at its permanent
    // root entry: a borrowed `JSON.parse` called on any other object keeps the
    // existing throw. Both the ref slot and the generation are compared, so a
    // stale root never matches either.
    if e.lower.json_used.get() {
        let json = e.boxed()?;
        body.push(assign(
            json,
            call(
                intrinsic.root,
                vec![var(c), int(super::intrinsics::json_root(e.lower))],
            ),
            l,
        ));
        body.push(branch(
            and(
                tag_is(var(receiver), value::OBJECT),
                and(
                    and(
                        eq(
                            num(field(var(receiver), "ref_slot")),
                            num(field(var(json), "ref_slot")),
                        ),
                        eq(
                            num(field(var(receiver), "generation")),
                            num(field(var(json), "generation")),
                        ),
                    ),
                    and(eq(var(count), int(1)), tag_is(var(argument), value::STRING)),
                ),
            ),
            string_argument,
            fail,
            l,
        ));
    } else {
        // Dispatch code 272 only exists on the installed `JSON` object, so a
        // source that never named `JSON` never reaches this helper and no
        // uninstalled root entry is ever read.
        body.extend(fail);
    }
    body.push(e.effect(heap.root_leave, vec![var(c), var(frame)])?);
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        "__compat_es5_json_parse",
        index,
        vec![c, code, receiver, args],
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
    Ok(())
}

/// The one admitted `new Date(0)` construction.
///
/// `Date` is not a global binding and this module never installs one: the
/// profile layer admits exactly the call callee `Date` with the single
/// numeric literal argument `0`. This helper builds the real ordinary object
/// that value denotes. Its own rows are the admitted `getTime` function (code
/// 273) and one private non-enumerable data row carrying the epoch
/// millisecond value as the Number 0, so no host clock is ever read. Nothing
/// is rooted through the carrier's Core array: the object, its method and both
/// keys are ordinary guest nodes published into this helper's own root frame.
fn append_date_epoch(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    index: u16,
) -> Result<(), ParseError> {
    let c = lower.slot(span)?;
    let heap = lower.heap_ref(span)?.helpers;
    let intrinsic = lower.intrinsics.expect("intrinsics");
    let mut e = Emit::new(lower, span, c)?;
    let l = e.line;
    let object_proto = call(intrinsic.root, vec![var(c), int(0)]);
    let function_proto = call(intrinsic.root, vec![var(c), int(1)]);
    let epoch = e.boxed()?;
    let method = e.boxed()?;
    let frame = e.integer()?;
    let out = e.boxed()?;
    let mut body = vec![assign(
        frame,
        call(heap.root_enter, vec![var(c), int(6)]),
        l,
    )];
    // Entry 0 keeps the seven-unit `getTime` key alive across the method
    // reservation; entry 1 keeps the method alive across the object
    // reservation; entry 2 keeps the object alive while both rows are written;
    // entry 3 keeps the private one-unit epoch key alive across that write.
    let get_time = super::intrinsics::string(&mut e, "getTime", frame, 0, &mut body)?;
    body.push(assign(
        method,
        call(
            intrinsic.allocate,
            vec![
                var(c),
                int(4),
                function_proto,
                int(1),
                int(super::intrinsics::DATE_GETTIME_CODE),
                int(0), int(0),
                var(frame),
                int(1),
            ],
        ),
        l,
    ));
    body.push(assign(
        epoch,
        call(
            intrinsic.allocate,
            vec![
                var(c),
                int(3),
                object_proto,
                int(2),
                int(0),
                int(0), int(0),
                var(frame),
                int(2),
            ],
        ),
        l,
    ));
    super::intrinsics::trusted_data(
        &mut e,
        var(epoch),
        0,
        var(get_time),
        var(method),
        (true, false, true),
        &mut body,
    )?;
    let time_key = super::intrinsics::string(&mut e, "\u{0}", frame, 3, &mut body)?;
    super::intrinsics::trusted_data(
        &mut e,
        var(epoch),
        1,
        var(time_key),
        value::number(E::Float(0.0)),
        (false, false, false),
        &mut body,
    )?;
    body.push(assign(out, var(epoch), l));
    body.push(e.effect(heap.root_leave, vec![var(c), var(frame)])?);
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        "__compat_es5_date_epoch",
        index,
        vec![c],
        vec![carrier_schema.clone()],
        value::schema(),
        body,
        var(out),
        l,
    );
    Ok(())
}

/// Admitted `Date.prototype.getTime` behind dispatch code 273.
///
/// The function is found by the unchanged ordinary Get on the object the one
/// admitted construction returned, so no member name is special-cased in the
/// lowerer. Only the admitted shape succeeds: no actual arguments and the
/// receiver an ordinary object with exactly two own rows, the first a
/// seven-unit `getTime` data row and the second the private one-unit data row
/// whose Number value is 0. That stored time value is the result, so
/// `new Date(0).getTime()` is the Number 0 without any live clock read.
/// Everything else -- a borrowed function, another receiver, any argument or
/// any other row set -- keeps the existing TypeError.
fn append_date_gettime(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    index: u16,
) -> Result<(), ParseError> {
    let c = lower.slot(span)?;
    let code = lower.slot(span)?;
    let receiver = lower.slot(span)?;
    let args = lower.slot(span)?;
    let heap = lower.heap_ref(span)?.helpers;
    let intrinsic = lower.intrinsics.expect("intrinsics");
    let mut e = Emit::new(lower, span, c)?;
    let l = e.line;
    let sp = e.lower.ir_span(span);
    let out = e.temp(
        value::logical_result_schema(),
        value::result_ok(value::completion_normal(), sp),
    )?;
    let frame = e.integer()?;
    let count = e.integer()?;
    let base = e.integer()?;
    let method = e.integer()?;
    let row = e.integer()?;
    let unit = e.integer()?;
    let key_length = e.temp(T::Number, E::Float(0.0))?;
    let error = e.boxed()?;
    let mut body = Vec::new();
    body.push(assert_stmt(
        eq(var(code), int(super::intrinsics::DATE_GETTIME_CODE)),
        l,
    ));
    body.push(assign(
        frame,
        call(heap.root_enter, vec![var(c), int(6)]),
        l,
    ));
    body.push(e.root(var(frame), int(0), var(receiver))?);
    let fail = vec![
        assign(error, call(intrinsic.type_error, vec![var(c)]), l),
        e.root(var(frame), int(1), var(error))?,
        assign(out, value::result_err(var(error), sp), l),
    ];
    // The receiver's tag is checked before its base is read, and its node kind
    // before any header cell, so a non-object throws the managed TypeError
    // instead of failing an internal assertion.
    let kind = e.integer()?;
    let mut ordinary = Vec::new();
    ordinary.push(assign(base, e.base(var(receiver)), l));
    ordinary.push(assign(kind, e.decoded(var(base)), l));
    let mut rows = Vec::new();
    let header = property_header(&mut e, base, &mut rows)?;
    // Row 0 must be the admitted seven-unit `getTime` data row whose Function
    // node carries dispatch code 273; that is the only function node the
    // construction ever publishes.
    let mut routine = Vec::new();
    routine.push(assign(method, add(var(base), var(header.header)), l));
    let (routine_desc, routine_key) = decode_row(&mut e, method, &mut routine)?;
    routine.push(assign(key_length, e.read(add(var(routine_key), int(2))), l));
    let mut time = Vec::new();
    time.push(assign(
        row,
        add(var(base), add(var(header.header), int(7))),
        l,
    ));
    let (time_desc, time_key) = decode_row(&mut e, row, &mut time)?;
    time.push(assign(key_length, e.read(add(var(time_key), int(2))), l));
    time.push(assign(unit, int(0), l));
    // The stored time value is the result: the Number 0 is read out of the
    // private row instead of being reintroduced as a literal at the call site.
    let succeed = vec![assign(
        out,
        value::result_ok(
            value::completion_return(value::number(num(field(
                field(var(time_desc), "value"),
                "number",
            )))),
            sp,
        ),
        l,
    )];
    time.push(branch(
        and(
            eq(var(key_length), E::Float(1.0)),
            and(
                eq(num(field(var(time_desc), "present")), num(int(51))),
                and(
                    tag_is(field(var(time_desc), "value"), value::NUMBER),
                    and(
                        eq(
                            read_string_unit(e.read, e.carrier, time_key, unit),
                            E::Float(0.0),
                        ),
                        eq(
                            num(field(field(var(time_desc), "value"), "number")),
                            E::Float(0.0),
                        ),
                    ),
                ),
            ),
        ),
        succeed,
        fail.clone(),
        l,
    ));
    routine.push(branch(
        and(
            eq(var(key_length), E::Float(7.0)),
            eq(num(field(var(routine_desc), "present")), num(int(51))),
        ),
        time,
        fail.clone(),
        l,
    ));
    rows.push(branch(
        and(eq(var(header.count), int(2)), not(var(header.error))),
        routine,
        fail.clone(),
        l,
    ));
    ordinary.push(branch(
        eq(var(kind), int(layout::KIND_OBJECT)),
        rows,
        fail.clone(),
        l,
    ));
    body.push(assign(
        count,
        call(B::Len.call_index(), vec![E::Borrow(Box::new(var(args)))]),
        l,
    ));
    body.push(branch(
        and(tag_is(var(receiver), value::OBJECT), eq(var(count), int(0))),
        ordinary,
        fail,
        l,
    ));
    body.push(e.effect(heap.root_leave, vec![var(c), var(frame)])?);
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        "__compat_es5_date_gettime",
        index,
        vec![c, code, receiver, args],
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
    Ok(())
}
/// Receiver-aware Array push/pop/shift/unshift/reverse/indexOf/lastIndexOf/slice/concat.
/// indexOf searches the internal length, including an unmaterialized tail read
/// as `undefined`. That is this model's prefix semantics, not ES5 HasProperty.
/// slice and concat copy that same prefix into a new tagged array. concat
/// flattens one level: an Array argument contributes its internal length, and
/// anything else is one element. Neither reads the `length` property and
/// neither allocates through a new constructor.
pub(crate) fn is_array_callback_code(code: E) -> E {
    and(
        not(lt(code.clone(), int(super::intrinsics::ARRAY_EVERY_CODE))),
        not(lt(int(super::intrinsics::ARRAY_REDUCE_RIGHT_CODE), code)),
    )
}

pub(crate) fn is_array_join_code(code: E) -> E {
    or(
        eq(code.clone(), int(super::intrinsics::ARRAY_JOIN_CODE)),
        eq(code, int(super::intrinsics::ARRAY_TO_STRING_CODE)),
    )
}

/// Build the sort arm inside the existing true-Array adapter. The private
/// snapshot is traced by the managed collector; the merge buffers contain only
/// Int indices and can never hold or expose guest identities.
fn array_sort_body(
    outer: &mut Emit<'_, '_>,
    receiver: u16,
    args: u16,
    out: u16,
    dispatch: u16,
    number_value: u16,
    key_string: u16,
    to_units: u16,
) -> Result<Vec<S>, ParseError> {
    // The other Array methods do not allocate sort's scratch buffers.
    let mut scratch = Emit::new(outer.lower, outer.span, outer.carrier)?;
    let e = &mut scratch;
    let c = e.carrier;
    let l = e.line;
    let sp = e.lower.ir_span(e.span);
    let heap = e.lower.heap_ref(e.span)?.helpers;
    let intrinsic = e.lower.intrinsics.expect("intrinsics");
    let frame = e.integer()?;
    let base = e.integer()?;
    let backing_kind = e.integer()?;
    let packed = e.boolean()?;
    let present = e.integer()?;
    let stride = e.integer()?;
    let cells = e.integer()?;
    let snapshot_base = e.integer()?;
    let allocation = e.temp(value::allocation_schema(), value::allocation(int(0), int(0), int(0)))?;
    let snapshot = e.boxed()?;
    let comparator = e.boxed()?;
    let default = e.boolean()?;
    let active = e.boolean()?;
    let cursor = e.integer()?;
    let count = e.integer()?;
    let width = e.integer()?;
    let left = e.integer()?;
    let mid = e.integer()?;
    let right = e.integer()?;
    let end = e.integer()?;
    let take_right = e.boolean()?;
    let a = e.boxed()?;
    let b = e.boxed()?;
    let text_a = e.boxed()?;
    let converted = e.boxed()?;
    let err = e.boxed()?;
    let completion = e.temp(value::completion_schema(), value::completion_normal())?;
    let logical = value::logical_result_schema();
    let called = e.temp(logical.clone(), value::result_ok(value::completion_normal(), sp))?;
    let boxed_result = result_schema(value::schema());
    let conversion = e.temp(boxed_result.clone(), result(boxed_result.clone(), true, value::undefined(), sp))?;
    let new_array = || call(B::ArrayNew.call_index(), vec![]);
    let ints = T::Array(Box::new(T::Int));
    let order = e.temp(ints.clone(), new_array())?;
    let merged = e.temp(ints.clone(), new_array())?;
    let a_units = e.temp(ints.clone(), new_array())?;
    let b_units = e.temp(ints, new_array())?;
    let callback_args = e.temp(T::Array(Box::new(value::schema())), new_array())?;
    let mutation = e.temp(T::Null, E::Null)?;
    let len = |slot| call(B::Len.call_index(), vec![E::Borrow(Box::new(var(slot)))]);
    let get = |slot, i| call(B::Get.call_index(), vec![E::Borrow(Box::new(var(slot))), i]);
    let push = |slot, item| assign(slot, call(B::ArrayPush.call_index(), vec![E::MoveVar(slot), item]), l);
    let min = |a: E, b: E| choose(lt(a.clone(), b.clone()), a, b);
    let present_at = |e: &Emit<'_, '_>| {
        min(
            call(e.decode_u32, vec![call(heap.array_length, vec![var(c), var(base)])]),
            sub(e.decoded(add(var(base), int(layout::ARRAY_PRESENT_OFFSET))), int(1)),
        )
    };
    let snapshot_get = |e: &Emit<'_, '_>, i| call(heap.array_get, vec![var(c), e.base(var(snapshot)), i]);
    let next = |slot| assign(slot, add(var(slot), int(1)), l);

    // Slots 0 and 1 retain the snapshot and comparator. The caller owns the
    // receiver. ToString uses 3..=5; 7 retains the first converted String
    // across conversion of the second operand, or protects an abrupt payload.
    let failure = vec![
        e.root(var(frame), int(7), var(err))?,
        assign(out, value::result_err(var(err), sp), l),
        assign(active, E::Bool(false), l),
    ];
    let mut invalid = Vec::new();
    let key = super::intrinsics::string(e, "message", frame, 0, &mut invalid)?;
    let message = super::intrinsics::string(e, "not a function", frame, 1, &mut invalid)?;
    invalid.push(assign(err, call(intrinsic.allocate, vec![
        var(c), int(3), call(intrinsic.root, vec![var(c), int(3)]), int(1), int(0), int(0), int(6), var(frame), int(7),
    ]), l));
    super::intrinsics::trusted_data(e, var(err), 0, var(key), var(message), (true, false, true), &mut invalid)?;
    invalid.push(assign(out, value::result_err(var(err), sp), l));

    let numeric = vec![
        assign(converted, choose(
            eq(field(var(completion), "kind"), int(value::COMPLETION_RETURN)),
            field(var(completion), "value"), value::undefined(),
        ), l),
        e.root(var(frame), int(7), var(converted))?,
        assign(conversion, call(number_value, vec![var(c), var(converted), E::Bool(false)]), l),
        // Only positive orders right before left. NaN and both zeros take left.
        matched(e, conversion, boxed_result.clone(), converted, err, vec![
            assign(take_right, lt(E::Float(0.0), super::math::checked_number(var(converted))), l),
        ], failure.clone())?,
    ];
    let custom = vec![
        assign(callback_args, new_array(), l),
        push(callback_args, var(a)),
        push(callback_args, var(b)),
        assign(called, call(dispatch, vec![var(c), var(comparator), value::undefined(), E::MoveVar(callback_args)]), l),
        matched(e, called, logical, completion, err, numeric, failure.clone())?,
    ];

    // This reuses the managed String bridge even for loose UTF-16 (including
    // isolated surrogates). Compare code units, not Unicode scalar values.
    let unit = e.integer()?;
    let string_order = vec![
        assign(a_units, call(to_units, vec![var(c), var(text_a)]), l),
        assign(b_units, call(to_units, vec![var(c), var(converted)]), l),
        assign(unit, int(0), l),
        S::While {
            condition: and(lt(var(unit), len(a_units)), lt(var(unit), len(b_units))),
            body: vec![
                branch(not(eq(get(a_units, var(unit)), get(b_units, var(unit)))),
                    vec![S::Break { line: l }], vec![], l),
                next(unit),
            ],
            line: l,
        },
        assign(take_right, choose(
            eq(var(unit), len(b_units)), lt(var(unit), len(a_units)),
            choose(eq(var(unit), len(a_units)), E::Bool(false), lt(get(b_units, var(unit)), get(a_units, var(unit)))),
        ), l),
    ];
    let compare_b = vec![
        e.root(var(frame), int(7), var(text_a))?,
        assign(conversion, call(key_string, vec![var(c), var(b), var(frame)]), l),
        matched(e, conversion, boxed_result.clone(), converted, err, string_order, failure.clone())?,
    ];
    let default_compare = vec![
        assign(conversion, call(key_string, vec![var(c), var(a), var(frame)]), l),
        matched(e, conversion, boxed_result, text_a, err, compare_b, failure)?,
    ];
    let pick = vec![
        assign(a, snapshot_get(e, get(order, var(left))), l),
        assign(b, snapshot_get(e, get(order, var(right))), l),
        branch(var(default), default_compare, custom, l),
    ];
    let merge = vec![
        assign(take_right, eq(var(left), var(mid)), l),
        branch(and(lt(var(left), var(mid)), lt(var(right), var(end))), pick, vec![], l),
        branch(var(active), vec![branch(var(take_right), vec![
            push(merged, get(order, var(right))), next(right),
        ], vec![
            push(merged, get(order, var(left))), next(left),
        ], l)], vec![], l),
    ];
    let sort = vec![
        assign(width, int(1), l),
        S::While {
            condition: and(var(active), lt(var(width), var(count))),
            body: vec![
                assign(merged, new_array(), l),
                assign(cursor, int(0), l),
                S::While {
                    condition: and(var(active), lt(var(cursor), var(count))),
                    body: vec![
                        assign(left, var(cursor), l),
                        assign(mid, min(add(var(left), var(width)), var(count)), l),
                        assign(right, var(mid), l),
                        assign(end, min(add(var(mid), var(width)), var(count)), l),
                        S::While {
                            condition: and(var(active), or(lt(var(left), var(mid)), lt(var(right), var(end)))),
                            body: merge,
                            line: l,
                        },
                        assign(cursor, var(end), l),
                    ],
                    line: l,
                },
                assign(order, E::MoveVar(merged), l),
                assign(width, mul(var(width), int(2)), l),
            ],
            line: l,
        },
    ];

    // Copy only the initialized prefix, with its original packed/tagged
    // representation. Between node_begin and node_publish nothing collects.
    // The snapshot's null prototype never participates in guest lookup.
    let payload = |at| add(add(var(at), int(layout::ARRAY_HEADER_CELLS)), var(cursor));
    let capture = vec![
        assign(stride, choose(var(packed), int(1), int(layout::ARRAY_TAGGED_CELLS_PER_ENTRY)), l),
        assign(cells, mul(var(present), var(stride)), l),
        assign(allocation, call(heap.node_begin, vec![var(c), add(int(layout::ARRAY_HEADER_CELLS), var(cells)), var(backing_kind)]), l),
        assign(snapshot_base, field(var(allocation), "base"), l),
        assign(base, e.base(var(receiver)), l),
        assert_stmt(not(lt(e.decoded(sub(var(base), int(layout::RAW_HEADER_CELLS))),
            add(int(layout::RAW_HEADER_CELLS + layout::ARRAY_HEADER_CELLS), var(cells)))), l),
        e.write(add(var(snapshot_base), int(2)), var(present))?,
        e.write(add(var(snapshot_base), int(layout::ARRAY_PROTO_OFFSET)), int(0))?,
        e.write(add(var(snapshot_base), int(layout::ARRAY_PROTO_OFFSET + 1)), int(0))?,
        e.write(add(var(snapshot_base), int(layout::ARRAY_PRESENT_OFFSET)), add(var(present), int(1)))?,
        assign(cursor, int(0), l),
        S::While {
            condition: lt(var(cursor), var(cells)),
            body: vec![e.write(payload(snapshot_base), e.read(payload(base)))?, next(cursor)],
            line: l,
        },
        assign(snapshot, call(heap.node_publish, vec![var(c), field(var(allocation), "slot"), field(var(allocation), "generation"), var(snapshot_base), var(frame), int(0)]), l),
        assign(cursor, int(0), l),
        S::While {
            condition: lt(var(cursor), var(present)),
            body: vec![
                branch(or(var(packed), not(tag_is(snapshot_get(e, var(cursor)), value::UNDEFINED))),
                    vec![push(order, var(cursor))], vec![], l),
                next(cursor),
            ],
            line: l,
        },
        assign(count, len(order), l),
    ];
    let commit = vec![
        assign(base, e.base(var(receiver)), l),
        assign(present, min(var(present), present_at(e)), l),
        assign(cursor, int(0), l),
        S::While {
            condition: lt(var(cursor), var(present)),
            body: vec![
                assign(a, choose(lt(var(cursor), var(count)), snapshot_get(e, get(order, var(cursor))), value::undefined()), l),
                assign(base, e.base(var(receiver)), l),
                assign(mutation, call(heap.array_put, vec![var(c), var(base), var(cursor), var(a), field(var(receiver), "ref_slot"), field(var(receiver), "generation")]), l),
                assert_stmt(eq(var(mutation), E::Null), l),
                next(cursor),
            ],
            line: l,
        },
    ];
    let mut work = capture;
    work.extend(sort);
    work.push(branch(var(active), commit, vec![], l));
    let valid = vec![
        e.root(var(frame), int(1), var(comparator))?,
        assign(out, value::result_ok(value::completion_return(var(receiver)), sp), l),
        assign(active, E::Bool(true), l),
        assign(base, e.base(var(receiver)), l),
        assign(backing_kind, e.decoded(var(base)), l),
        assign(packed, eq(var(backing_kind), int(layout::KIND_ARRAY)), l),
        assign(present, present_at(e), l),
        branch(lt(int(1), var(present)), work, vec![], l),
    ];
    let body = vec![
        assign(frame, call(heap.root_enter, vec![var(c), int(8)]), l),
        assign(comparator, choose(lt(int(0), len(args)), get(args, int(0)), value::undefined()), l),
        assign(default, tag_is(var(comparator), value::UNDEFINED), l),
        branch(or(var(default), and(tag_is(var(comparator), value::OBJECT),
            eq(e.decoded(e.base(var(comparator))), int(layout::KIND_FUNCTION)))), valid, invalid, l),
        e.effect(heap.root_leave, vec![var(c), var(frame)])?,
    ];
    Ok(scratch.finish(body))
}

fn append_array_method(
    lower: &mut Lowerer<'_>, ir: &mut FrontendIr, carrier_schema: &T,
    span: swc_common::Span, stmts: &mut Vec<S>, index: u16,
    parser: u16, number_value: u16, dispatch: u16, key_string: u16,
) -> Result<(), ParseError> {
    let to_units = lower.utf16_units_helper().expect("installed UTF-16 reader");
    let publish = lower.utf16_publish_helper().expect("installed UTF-16 publisher");
    let c = lower.slot(span)?;
    let code = lower.slot(span)?;
    let receiver = lower.slot(span)?;
    let args = lower.slot(span)?;
    let heap = lower.heap_ref(span)?.helpers;
    let intrinsic = lower.intrinsics.expect("intrinsics");
    let mut e = Emit::new(lower, span, c)?;
    let l = e.line;
    let sp = e.lower.ir_span(span);
    let out = e.temp(value::logical_result_schema(), value::result_ok(value::completion_normal(), sp))?;
    let frame = e.integer()?;
    let base = e.integer()?;
    let kind = e.integer()?;
    let len = e.integer()?;
    let count = e.integer()?;
    let cursor = e.integer()?;
    let limit = e.integer()?;
    let present = e.integer()?;
    let destination = e.integer()?;
    let arg = e.boxed()?;
    let answer = e.boxed()?;
    let err = e.boxed()?;
    let mutation = e.temp(T::Null, E::Null)?;
    let position = e.temp(T::Float, E::Float(0.0))?;
    let primitive = e.boxed()?;
    let raw = e.boxed()?;
    let conversion = e.temp(result_schema(value::schema()), result(result_schema(value::schema()), true, value::undefined(), sp))?;
    let mut body = vec![
        assign(frame, call(heap.root_enter, vec![var(c), int(3)]), l),
        e.root(var(frame), int(0), var(receiver))?,
    ];
    let fail = vec![
        assign(err, call(intrinsic.type_error, vec![var(c)]), l),
        e.root(var(frame), int(2), var(err))?,
        assign(out, value::result_err(var(err), sp), l),
    ];
    // Check the brand before calling any Array helper, which asserts on a
    // non-Array base and would otherwise leak a host assertion. Slice, concat
    // and the callback methods use the logical UInt32 length. Other methods
    // keep the 65536 exact-int decoder.
    let logical_length = call(e.decode_u32, vec![call(heap.array_length, vec![var(c), var(base)])]);
    let join_length = logical_length.clone();
    let mut accepted = vec![branch(
        or(
            or(
                eq(var(code), int(super::intrinsics::ARRAY_SLICE_CODE)),
                eq(var(code), int(super::intrinsics::ARRAY_CONCAT_CODE)),
            ),
            is_array_callback_code(var(code)),
        ),
        vec![assign(len, logical_length.clone(), l)],
        vec![assign(len, e.decoded(add(var(base), int(2))), l)],
        l,
    )];
    accepted.push(assign(count, call(B::Len.call_index(), vec![E::Borrow(Box::new(var(args)))]), l));
    let mut push = vec![assign(limit, int(65527), l)];
    push.push(branch(eq(var(kind), int(layout::KIND_TAGGED_ARRAY)), vec![assign(limit, int(13105), l)], Vec::new(), l));
    push.push(assign(cursor, int(0), l));
    push.push(S::While {
        condition: lt(var(cursor), var(count)),
        body: vec![
            assign(arg, call(B::Get.call_index(), vec![E::Borrow(Box::new(var(args))), var(cursor)]), l),
            branch(not(tag_is(var(arg), value::NUMBER)), vec![assign(limit, int(13105), l)], Vec::new(), l),
            assign(cursor, add(var(cursor), int(1)), l),
        ], line: l,
    });
    // The packed numeric limit is 65527, but appending at the present tail
    // can enter tagged expansion, whose storage limit is 13105. This preflight
    // does not guarantee that allocation succeeds: tagged growth can fail as
    // HostError, not a guest TypeError.
    push.push(assert_stmt(and(not(lt(var(limit), add(var(len), var(count)))), lt(add(var(len), var(count)), int(1_073_741_824))), l));
    push.push(assign(cursor, int(0), l));
    let mut put_one = vec![
        assign(arg, call(B::Get.call_index(), vec![E::Borrow(Box::new(var(args))), var(cursor)]), l),
        e.root(var(frame), int(1), var(arg))?,
        assign(base, e.base(var(receiver)), l),
        assign(mutation, call(heap.array_set_length, vec![var(c), var(base), add(var(len), int(1))]), l),
        assert_stmt(eq(var(mutation), E::Null), l),
        assert_stmt(eq(e.decoded(add(e.base(var(receiver)), int(2))), add(var(len), int(1))), l),
        assign(base, e.base(var(receiver)), l),
        assign(mutation, call(heap.array_put, vec![var(c), var(base), var(len), var(arg), field(var(receiver), "ref_slot"), field(var(receiver), "generation")]), l),
        assert_stmt(eq(var(mutation), E::Null), l),
        assert_stmt(eq(e.decoded(add(e.base(var(receiver)), int(2))), add(var(len), int(1))), l),
        assign(len, add(var(len), int(1)), l),
        assign(cursor, add(var(cursor), int(1)), l),
    ];
    push.push(S::While { condition: lt(var(cursor), var(count)), body: std::mem::take(&mut put_one), line: l });
    push.push(assign(answer, value::number(num(var(len))), l));
    push.push(assign(out, value::result_ok(value::completion_return(var(answer)), sp), l));
    let mut pop_nonempty = vec![
        assign(answer, call(heap.array_get, vec![var(c), var(base), sub(var(len), int(1))]), l),
        e.root(var(frame), int(2), var(answer))?,
        assign(base, e.base(var(receiver)), l),
        assign(mutation, call(heap.array_set_length, vec![var(c), var(base), sub(var(len), int(1))]), l),
        assert_stmt(eq(var(mutation), E::Null), l),
    ];
    let mut pop = vec![assign(answer, value::undefined(), l)];
    pop.push(branch(lt(int(0), var(len)), std::mem::take(&mut pop_nonempty), Vec::new(), l));
    pop.push(assign(out, value::result_ok(value::completion_return(var(answer)), sp), l));
    // Logical length may exceed the physical prefix. Move only initialized
    // storage, then restore the hole-only suffix without allocating it.
    let mut shift = vec![assign(answer, value::undefined(), l)];
    let mut shift_nonempty = vec![
        assign(present, sub(e.decoded(add(var(base), int(layout::ARRAY_PRESENT_OFFSET))), int(1)), l),
        branch(lt(var(len), var(present)), vec![assign(present, var(len), l)], Vec::new(), l),
    ];
    let mut shift_present = vec![
        assign(answer, call(heap.array_get, vec![var(c), var(base), int(0)]), l),
        e.root(var(frame), int(2), var(answer))?,
        assign(cursor, int(1), l),
    ];
    shift_present.push(S::While { condition: lt(var(cursor), var(present)), body: vec![
        assign(base, e.base(var(receiver)), l),
        assign(arg, call(heap.array_get, vec![var(c), var(base), var(cursor)]), l),
        e.root(var(frame), int(1), var(arg))?,
        assign(base, e.base(var(receiver)), l),
        assign(mutation, call(heap.array_put, vec![var(c), var(base), sub(var(cursor), int(1)), var(arg), field(var(receiver), "ref_slot"), field(var(receiver), "generation")]), l),
        assert_stmt(eq(var(mutation), E::Null), l),
        assign(cursor, add(var(cursor), int(1)), l),
    ], line: l });
    shift_present.extend([
        assign(base, e.base(var(receiver)), l),
        assign(mutation, call(heap.array_set_length, vec![var(c), var(base), sub(var(present), int(1))]), l),
        assert_stmt(eq(var(mutation), E::Null), l),
    ]);
    shift_nonempty.push(branch(lt(int(0), var(present)), shift_present, Vec::new(), l));
    shift_nonempty.push(branch(lt(var(present), var(len)), vec![
        assign(base, e.base(var(receiver)), l),
        assign(mutation, call(heap.array_set_length, vec![var(c), var(base), sub(var(len), int(1))]), l),
        assert_stmt(eq(var(mutation), E::Null), l),
    ], Vec::new(), l));
    shift.push(branch(lt(int(0), var(len)), shift_nonempty, Vec::new(), l));
    shift.push(assign(out, value::result_ok(value::completion_return(var(answer)), sp), l));
    let mut unshift_nonempty = vec![
        assign(present, sub(e.decoded(add(var(base), int(layout::ARRAY_PRESENT_OFFSET))), int(1)), l),
        branch(lt(var(len), var(present)), vec![assign(present, var(len), l)], Vec::new(), l),
        assign(limit, int(65527), l),
        branch(eq(var(kind), int(layout::KIND_TAGGED_ARRAY)), vec![assign(limit, int(13105), l)], Vec::new(), l),
        assign(cursor, int(0), l),
    ];
    unshift_nonempty.push(S::While { condition: lt(var(cursor), var(count)), body: vec![
        assign(arg, call(B::Get.call_index(), vec![E::Borrow(Box::new(var(args))), var(cursor)]), l),
        branch(not(tag_is(var(arg), value::NUMBER)), vec![assign(limit, int(13105), l)], Vec::new(), l),
        assign(cursor, add(var(cursor), int(1)), l),
    ], line: l });
    unshift_nonempty.extend([
        // Preflight both the new logical length and the highest physical write.
        assert_stmt(and(not(lt(add(var(len), var(count)), int(0))), lt(add(var(len), var(count)), int(1_073_741_824))), l),
        assign(destination, add(var(present), var(count)), l),
        assert_stmt(not(lt(var(limit), var(destination))), l),
        assign(base, e.base(var(receiver)), l),
        assign(mutation, call(heap.array_set_length, vec![var(c), var(base), add(var(len), var(count))]), l),
        assert_stmt(eq(var(mutation), E::Null), l),
        assign(cursor, var(present), l),
    ]);
    unshift_nonempty.push(S::While { condition: lt(int(0), var(cursor)), body: vec![
        assign(cursor, sub(var(cursor), int(1)), l),
        assign(base, e.base(var(receiver)), l),
        assign(arg, call(heap.array_get, vec![var(c), var(base), var(cursor)]), l),
        e.root(var(frame), int(1), var(arg))?,
        assign(base, e.base(var(receiver)), l),
        assign(mutation, call(heap.array_put, vec![var(c), var(base), add(var(cursor), var(count)), var(arg), field(var(receiver), "ref_slot"), field(var(receiver), "generation")]), l),
        assert_stmt(eq(var(mutation), E::Null), l),
    ], line: l });
    unshift_nonempty.push(assign(cursor, int(0), l));
    unshift_nonempty.push(S::While { condition: lt(var(cursor), var(count)), body: vec![
        assign(arg, call(B::Get.call_index(), vec![E::Borrow(Box::new(var(args))), var(cursor)]), l),
        e.root(var(frame), int(1), var(arg))?,
        assign(base, e.base(var(receiver)), l),
        assign(mutation, call(heap.array_put, vec![var(c), var(base), var(cursor), var(arg), field(var(receiver), "ref_slot"), field(var(receiver), "generation")]), l),
        assert_stmt(eq(var(mutation), E::Null), l),
        assign(cursor, add(var(cursor), int(1)), l),
    ], line: l });
    let mut unshift = vec![branch(lt(int(0), var(count)), unshift_nonempty, Vec::new(), l)];
    unshift.push(assign(answer, value::number(num(add(var(len), var(count)))), l));
    unshift.push(assign(out, value::result_ok(value::completion_return(var(answer)), sp), l));
    // Reverse the physical present prefix in place and return the same
    // receiver. Cell 5 is present+1, a prefix count, not a per-slot bit, so a
    // hole suffix cannot move past a present value. Length and the present
    // cell are not written, and the length property is not read.
    let mut reverse = vec![
        assign(base, e.base(var(receiver)), l),
        assign(present, sub(e.decoded(add(var(base), int(layout::ARRAY_PRESENT_OFFSET))), int(1)), l),
        branch(lt(var(len), var(present)), vec![assign(present, var(len), l)], Vec::new(), l),
    ];
    let mut reverse_swap = vec![
        assign(cursor, int(0), l),
        assign(destination, sub(var(present), int(1)), l),
    ];
    reverse_swap.push(S::While {
        condition: lt(var(cursor), var(destination)),
        body: vec![
            assign(base, e.base(var(receiver)), l),
            assign(arg, call(heap.array_get, vec![var(c), var(base), var(cursor)]), l),
            e.root(var(frame), int(1), var(arg))?,
            assign(base, e.base(var(receiver)), l),
            assign(answer, call(heap.array_get, vec![var(c), var(base), var(destination)]), l),
            e.root(var(frame), int(2), var(answer))?,
            assign(base, e.base(var(receiver)), l),
            assign(mutation, call(heap.array_put, vec![var(c), var(base), var(cursor), var(answer), field(var(receiver), "ref_slot"), field(var(receiver), "generation")]), l),
            assert_stmt(eq(var(mutation), E::Null), l),
            assign(base, e.base(var(receiver)), l),
            assign(mutation, call(heap.array_put, vec![var(c), var(base), var(destination), var(arg), field(var(receiver), "ref_slot"), field(var(receiver), "generation")]), l),
            assert_stmt(eq(var(mutation), E::Null), l),
            assign(cursor, add(var(cursor), int(1)), l),
            assign(destination, sub(var(destination), int(1)), l),
        ],
        line: l,
    });
    reverse.push(branch(lt(int(1), var(present)), reverse_swap, Vec::new(), l));
    reverse.push(assign(out, value::result_ok(value::completion_return(var(receiver)), sp), l));
    let string_eq = e.lower.string_equality(span).ok_or_else(|| e.lower.error(span, "Array.prototype.indexOf requires string equality"))?.index;
    let equal_element = || value::strict_equal_flag(arg, answer, Some(value::StringEquality { carrier: var(c), index: string_eq }));
    let projected = choose(
        tag_is(var(primitive), value::NUMBER),
        num(field(var(primitive), "number")),
        choose(
            tag_is(var(primitive), value::NULL),
            E::Float(0.0),
            choose(
                tag_is(var(primitive), value::BOOLEAN),
                choose(field(var(primitive), "flag"), E::Float(1.0), E::Float(0.0)),
                choose(
                    tag_is(var(primitive), value::STRING),
                    call(parser, vec![var(c), var(primitive), int(0), E::Float(0.0)]),
                    E::Float(f64::NAN),
                ),
            ),
        ),
    );
    let truncated = choose(
        not(eq(num(projected.clone()), num(projected.clone()))),
        E::Float(0.0),
        num(call(B::MathTrunc.call_index(), vec![num(projected)])),
    );
    let mut search = vec![
        assign(base, e.base(var(receiver)), l),
        assign(len, call(e.decode_u32, vec![call(heap.array_length, vec![var(c), var(base)])]), l),
        assign(destination, int(-1), l),
    ];
    search.push(branch(eq(var(code), int(super::intrinsics::ARRAY_LAST_INDEX_OF_CODE)), vec![
        branch(lt(sub(var(len), int(1)), var(cursor)), vec![assign(cursor, sub(var(len), int(1)), l)], Vec::new(), l),
    ], Vec::new(), l));
    search.push(branch(
        eq(var(code), int(super::intrinsics::ARRAY_LAST_INDEX_OF_CODE)),
        vec![S::While {
            condition: and(lt(var(destination), int(0)), not(lt(var(cursor), int(0)))),
            body: vec![
                assign(answer, call(heap.array_get, vec![var(c), var(base), var(cursor)]), l),
                branch(equal_element(), vec![assign(destination, var(cursor), l)], vec![assign(cursor, sub(var(cursor), int(1)), l)], l),
            ],
            line: l,
        }],
        vec![S::While {
            condition: and(lt(var(destination), int(0)), lt(var(cursor), var(len))),
            body: vec![
                assign(answer, call(heap.array_get, vec![var(c), var(base), var(cursor)]), l),
                branch(equal_element(), vec![assign(destination, var(cursor), l)], vec![assign(cursor, add(var(cursor), int(1)), l)], l),
            ],
            line: l,
        }],
        l,
    ));
    search.push(assign(answer, value::number(num(var(destination))), l));
    search.push(assign(out, value::result_ok(value::completion_return(var(answer)), sp), l));
    let mut no_from = vec![assign(cursor, choose(eq(var(code), int(super::intrinsics::ARRAY_LAST_INDEX_OF_CODE)), sub(var(len), int(1)), int(0)), l)];
    no_from.extend(search.clone());
    let index_clamp = vec![
        branch(lt(var(position), E::Float(0.0)), vec![
            assign(position, add(var(position), num(var(len))), l),
            branch(lt(var(position), E::Float(0.0)), vec![assign(position, E::Float(0.0), l)], Vec::new(), l),
        ], vec![
            branch(lt(num(var(len)), var(position)), vec![assign(position, num(var(len)), l)], Vec::new(), l),
        ], l),
        assign(cursor, call(e.decode, vec![num(var(position))]), l),
    ];
    let last_clamp = vec![
        branch(lt(var(position), E::Float(-1.0)), vec![
            assign(position, add(var(position), num(var(len))), l),
            branch(lt(var(position), E::Float(-1.0)), vec![assign(position, E::Float(-1.0), l)], Vec::new(), l),
        ], vec![
            branch(lt(sub(num(var(len)), E::Float(1.0)), var(position)), vec![assign(position, sub(num(var(len)), E::Float(1.0)), l)], Vec::new(), l),
        ], l),
        branch(lt(var(position), E::Float(0.0)), vec![assign(cursor, int(-1), l)], vec![assign(cursor, call(e.decode, vec![num(var(position))]), l)], l),
    ];
    let mut from_ok = vec![assign(position, truncated, l)];
    from_ok.push(branch(eq(var(code), int(super::intrinsics::ARRAY_LAST_INDEX_OF_CODE)), last_clamp, index_clamp, l));
    from_ok.extend(search);
    let conv_fail = vec![
        e.root(var(frame), int(2), var(err))?,
        assign(out, value::result_err(var(err), sp), l),
    ];
    let mut locate = vec![
        assign(arg, choose(lt(var(count), int(1)), value::undefined(), call(B::Get.call_index(), vec![E::Borrow(Box::new(var(args))), int(0)])), l),
        e.root(var(frame), int(1), var(arg))?,
    ];
    locate.push(branch(
        lt(int(1), var(count)),
        vec![
            assign(raw, call(B::Get.call_index(), vec![E::Borrow(Box::new(var(args))), int(1)]), l),
            e.root(var(frame), int(2), var(raw))?,
            assign(base, e.base(var(receiver)), l),
            assign(len, call(e.decode_u32, vec![call(heap.array_length, vec![var(c), var(base)])]), l),
            assign(conversion, call(number_value, vec![var(c), var(raw), E::Bool(false)]), l),
            matched(&mut e, conversion, result_schema(value::schema()), primitive, err, from_ok, conv_fail)?,
        ],
        no_from,
        l,
    ));
    // Micro js_array_slice: clamp both ends against the pre-conversion length,
    // skip ToInt32 when the end argument is undefined, then re-read the internal
    // length. start is not re-clamped. An unmaterialized tail copies as undefined.
    // The result is a new tagged array from node_begin/node_publish, not a
    // constructor and not a host vector. The existing 3-entry frame holds the
    // receiver, the element being copied, and the published result.
    let slice_alloc = e.temp(value::allocation_schema(), value::allocation(int(0), int(0), int(0)))?;
    let slice_base = e.integer()?;
    let slice_proto = e.boxed()?;
    let slice_fill = e.integer()?;
    // Clamped indices can equal the logical length, including 70000. The slot
    // decoder stops at 65536; decode_u32 is the logical-length domain. This
    // local is only the slice clamp.
    let decode = e.decode_u32;
    let slice_projected = choose(
        tag_is(var(primitive), value::NUMBER),
        num(field(var(primitive), "number")),
        choose(
            tag_is(var(primitive), value::NULL),
            E::Float(0.0),
            choose(
                tag_is(var(primitive), value::BOOLEAN),
                choose(field(var(primitive), "flag"), E::Float(1.0), E::Float(0.0)),
                choose(
                    tag_is(var(primitive), value::STRING),
                    call(parser, vec![var(c), var(primitive), int(0), E::Float(0.0)]),
                    E::Float(f64::NAN),
                ),
            ),
        ),
    );
    let slice_truncated = choose(
        not(eq(num(slice_projected.clone()), num(slice_projected.clone()))),
        E::Float(0.0),
        num(call(B::MathTrunc.call_index(), vec![num(slice_projected)])),
    );
    let clamp_saved = |dest: u16, truncated: E| -> Vec<S> {
        vec![
            assign(position, truncated, l),
            branch(lt(var(position), E::Float(0.0)), vec![
                assign(position, add(var(position), num(var(limit))), l),
                branch(lt(var(position), E::Float(0.0)), vec![assign(position, E::Float(0.0), l)], Vec::new(), l),
            ], vec![
                branch(lt(num(var(limit)), var(position)), vec![assign(position, num(var(limit)), l)], Vec::new(), l),
            ], l),
            assign(dest, call(decode, vec![num(var(position))]), l),
        ]
    };
    let mut slice_copy = vec![
        assign(base, e.base(var(receiver)), l),
        assign(len, call(e.decode_u32, vec![call(heap.array_length, vec![var(c), var(base)])]), l),
        branch(lt(var(len), var(destination)), vec![assign(destination, var(len), l)], Vec::new(), l),
        assign(present, sub(var(destination), var(cursor)), l),
        branch(lt(var(present), int(0)), vec![assign(present, int(0), l)], Vec::new(), l),
        assign(slice_alloc, call(heap.node_begin, vec![var(c), add(int(layout::ARRAY_HEADER_CELLS), mul(int(5), var(present))), int(layout::KIND_TAGGED_ARRAY)]), l),
        assign(slice_base, field(var(slice_alloc), "base"), l),
        assign(slice_proto, call(intrinsic.array_proto, vec![var(c)]), l),
    ];
    for (offset, payload) in [
        (2i64, var(present)),
        (3, num(field(var(slice_proto), "ref_slot"))),
        (4, num(field(var(slice_proto), "generation"))),
        (layout::ARRAY_PRESENT_OFFSET, add(var(present), int(1))),
    ] {
        slice_copy.push(e.write(add(var(slice_base), int(offset)), payload)?);
    }
    slice_copy.extend([
        assign(slice_fill, int(0), l),
        S::While {
            condition: lt(var(slice_fill), mul(int(5), var(present))),
            body: vec![
                e.write(add(var(slice_base), add(int(layout::ARRAY_HEADER_CELLS), var(slice_fill))), int(0))?,
                assign(slice_fill, add(var(slice_fill), int(1)), l),
            ],
            line: l,
        },
        assign(answer, call(heap.node_publish, vec![
            var(c), field(var(slice_alloc), "slot"), field(var(slice_alloc), "generation"),
            var(slice_base), var(frame), int(2),
        ]), l),
        assign(count, int(0), l),
    ]);
    slice_copy.push(S::While {
        condition: lt(var(count), var(present)),
        body: vec![
            assign(base, e.base(var(receiver)), l),
            assign(arg, call(heap.array_get, vec![var(c), var(base), add(var(cursor), var(count))]), l),
            e.root(var(frame), int(1), var(arg))?,
            assign(base, e.base(var(answer)), l),
            assign(mutation, call(heap.array_put, vec![var(c), var(base), var(count), var(arg), field(var(answer), "ref_slot"), field(var(answer), "generation")]), l),
            assert_stmt(eq(var(mutation), E::Null), l),
            assign(count, add(var(count), int(1)), l),
        ],
        line: l,
    });
    slice_copy.push(assign(out, value::result_ok(value::completion_return(var(answer)), sp), l));
    let slice_conv_fail = vec![
        e.root(var(frame), int(2), var(err))?,
        assign(out, value::result_err(var(err), sp), l),
    ];
    let mut slice_end_ok = clamp_saved(destination, slice_truncated.clone());
    slice_end_ok.extend(slice_copy.clone());
    let slice_end_convert = vec![
        assign(conversion, call(number_value, vec![var(c), var(raw), E::Bool(false)]), l),
        matched(&mut e, conversion, result_schema(value::schema()), primitive, err, slice_end_ok, slice_conv_fail.clone())?,
    ];
    let splice_truncated = slice_truncated.clone();
    let mut slice_after_start = clamp_saved(cursor, slice_truncated);
    slice_after_start.push(assign(raw, choose(lt(var(count), int(2)), value::undefined(), call(B::Get.call_index(), vec![E::Borrow(Box::new(var(args))), int(1)])), l));
    slice_after_start.push(e.root(var(frame), int(1), var(raw))?);
    slice_after_start.push(branch(
        tag_is(var(raw), value::UNDEFINED),
        {
            let mut kept = vec![assign(destination, var(limit), l)];
            kept.extend(slice_copy);
            kept
        },
        slice_end_convert,
        l,
    ));
    let slice_body = vec![
        assign(limit, var(len), l),
        assign(raw, choose(lt(var(count), int(1)), value::undefined(), call(B::Get.call_index(), vec![E::Borrow(Box::new(var(args))), int(0)])), l),
        e.root(var(frame), int(1), var(raw))?,
        assign(conversion, call(number_value, vec![var(c), var(raw), E::Bool(false)]), l),
        matched(&mut e, conversion, result_schema(value::schema()), primitive, err, slice_after_start, slice_conv_fail)?,
    ];
    // Micro js_array_concat. One level only: an Array argument contributes its
    // internal length, anything else is one element. The sum finishes before
    // any copy, so no user code runs between the passes. A total above
    // JS_SHORTINT_MAX (2^30-1) takes the existing TypeError path. The upstream
    // spelling "Array loo long" is not attached: this TypeError allocator has
    // no message argument. The result is a new tagged array from the existing
    // node_begin/node_publish path, not a new constructor. The existing 3-slot
    // frame holds the current source, the element, and the published result.
    let add_one = vec![assign(limit, add(var(limit), int(1)), l)];
    let add_len = vec![assign(
        limit,
        add(var(limit), call(e.decode_u32, vec![call(heap.array_length, vec![var(c), var(base)])])),
        l,
    )];
    let is_array = or(eq(var(kind), int(layout::KIND_ARRAY)), eq(var(kind), int(layout::KIND_TAGGED_ARRAY)));
    let mut concat_sum = vec![assign(limit, var(len), l), assign(cursor, int(0), l)];
    concat_sum.push(S::While {
        condition: lt(var(cursor), var(count)),
        body: vec![
            assign(arg, call(B::Get.call_index(), vec![E::Borrow(Box::new(var(args))), var(cursor)]), l),
            branch(
                tag_is(var(arg), value::OBJECT),
                vec![
                    assign(base, e.base(var(arg)), l),
                    assign(kind, e.decoded(var(base)), l),
                    branch(is_array.clone(), add_len.clone(), add_one.clone(), l),
                ],
                add_one.clone(),
                l,
            ),
            assign(cursor, add(var(cursor), int(1)), l),
        ],
        line: l,
    });
    let mut concat_fill = vec![
        assign(slice_alloc, call(heap.node_begin, vec![var(c), add(int(layout::ARRAY_HEADER_CELLS), mul(int(5), var(limit))), int(layout::KIND_TAGGED_ARRAY)]), l),
        assign(slice_base, field(var(slice_alloc), "base"), l),
        assign(slice_proto, call(intrinsic.array_proto, vec![var(c)]), l),
    ];
    for (offset, payload) in [(2i64, var(limit)), (3, num(field(var(slice_proto), "ref_slot"))), (4, num(field(var(slice_proto), "generation"))), (layout::ARRAY_PRESENT_OFFSET, add(var(limit), int(1)))] {
        concat_fill.push(e.write(add(var(slice_base), int(offset)), payload)?);
    }
    concat_fill.extend([
        assign(slice_fill, int(0), l),
        S::While {
            condition: lt(var(slice_fill), mul(int(5), var(limit))),
            body: vec![e.write(add(var(slice_base), add(int(layout::ARRAY_HEADER_CELLS), var(slice_fill))), int(0))?, assign(slice_fill, add(var(slice_fill), int(1)), l)],
            line: l,
        },
        assign(answer, call(heap.node_publish, vec![var(c), field(var(slice_alloc), "slot"), field(var(slice_alloc), "generation"), var(slice_base), var(frame), int(2)]), l),
        assign(destination, int(0), l),
        assign(cursor, int(0), l),
    ]);
    concat_fill.push(S::While {
        condition: lt(var(cursor), var(len)),
        body: vec![
            assign(base, e.base(var(receiver)), l),
            assign(arg, call(heap.array_get, vec![var(c), var(base), var(cursor)]), l),
            e.root(var(frame), int(1), var(arg))?,
            assign(base, e.base(var(answer)), l),
            assign(mutation, call(heap.array_put, vec![var(c), var(base), var(destination), var(arg), field(var(answer), "ref_slot"), field(var(answer), "generation")]), l),
            assert_stmt(eq(var(mutation), E::Null), l),
            assign(destination, add(var(destination), int(1)), l),
            assign(cursor, add(var(cursor), int(1)), l),
        ],
        line: l,
    });
    let put_one = vec![
        e.root(var(frame), int(1), var(arg))?,
        assign(base, e.base(var(answer)), l),
        assign(mutation, call(heap.array_put, vec![var(c), var(base), var(destination), var(arg), field(var(answer), "ref_slot"), field(var(answer), "generation")]), l),
        assert_stmt(eq(var(mutation), E::Null), l),
        assign(destination, add(var(destination), int(1)), l),
    ];
    let mut from_array = vec![
        e.root(var(frame), int(0), var(arg))?,
        assign(base, e.base(var(arg)), l),
        assign(len, call(e.decode_u32, vec![call(heap.array_length, vec![var(c), var(base)])]), l),
        assign(present, int(0), l),
    ];
    from_array.push(S::While {
        condition: lt(var(present), var(len)),
        body: vec![
            assign(base, e.base(var(arg)), l),
            assign(raw, call(heap.array_get, vec![var(c), var(base), var(present)]), l),
            e.root(var(frame), int(1), var(raw))?,
            assign(base, e.base(var(answer)), l),
            assign(mutation, call(heap.array_put, vec![var(c), var(base), var(destination), var(raw), field(var(answer), "ref_slot"), field(var(answer), "generation")]), l),
            assert_stmt(eq(var(mutation), E::Null), l),
            assign(destination, add(var(destination), int(1)), l),
            assign(present, add(var(present), int(1)), l),
        ],
        line: l,
    });
    concat_fill.push(assign(cursor, int(0), l));
    concat_fill.push(S::While {
        condition: lt(var(cursor), var(count)),
        body: vec![
            assign(arg, call(B::Get.call_index(), vec![E::Borrow(Box::new(var(args))), var(cursor)]), l),
            branch(
                tag_is(var(arg), value::OBJECT),
                vec![assign(base, e.base(var(arg)), l), assign(kind, e.decoded(var(base)), l), branch(is_array.clone(), from_array, put_one.clone(), l)],
                put_one,
                l,
            ),
            assign(cursor, add(var(cursor), int(1)), l),
        ],
        line: l,
    });
    concat_fill.push(assign(out, value::result_ok(value::completion_return(var(answer)), sp), l));
    concat_sum.push(branch(lt(int(1_073_741_823), var(limit)), fail.clone(), concat_fill, l));
    let concat_body = concat_sum;
    let get_arg = |i| call(B::Get.call_index(), vec![E::Borrow(Box::new(var(args))), int(i)]);
    let active = e.boolean()?;
    let this_arg = e.boxed()?;
    let callback_args = e.lower.slot(span)?;
    let called = e.lower.slot(span)?;
    let completion = e.lower.slot(span)?;
    let every = eq(var(code), int(super::intrinsics::ARRAY_EVERY_CODE));
    let for_each = eq(var(code), int(super::intrinsics::ARRAY_FOR_EACH_CODE));
    let map = eq(var(code), int(super::intrinsics::ARRAY_MAP_CODE));
    let filter = eq(var(code), int(super::intrinsics::ARRAY_FILTER_CODE));
    let produces_array = or(map.clone(), filter.clone());
    // The published result reuses root 2 across callbacks; root 1 holds the
    // input element, or the mapped value when it is ready to write.
    let mut callback_array = vec![
        assign(limit, choose(map.clone(), var(len), int(0)), l),
        assign(slice_alloc, call(heap.node_begin, vec![var(c), add(int(layout::ARRAY_HEADER_CELLS), mul(int(layout::ARRAY_TAGGED_CELLS_PER_ENTRY), var(limit))), int(layout::KIND_TAGGED_ARRAY)]), l),
        assign(slice_base, field(var(slice_alloc), "base"), l),
        assign(slice_proto, call(intrinsic.array_proto, vec![var(c)]), l),
    ];
    for (offset, payload) in [(2i64, var(limit)), (3, num(field(var(slice_proto), "ref_slot"))), (4, num(field(var(slice_proto), "generation"))), (layout::ARRAY_PRESENT_OFFSET, add(var(limit), int(1)))] {
        callback_array.push(e.write(add(var(slice_base), int(offset)), payload)?);
    }
    // Micro's dense map result has undefined entries at indices that the
    // callback never reaches when the input shrinks.
    callback_array.extend([
        assign(slice_fill, int(0), l),
        S::While {
            condition: lt(var(slice_fill), mul(int(layout::ARRAY_TAGGED_CELLS_PER_ENTRY), var(limit))),
            body: vec![e.write(add(var(slice_base), add(int(layout::ARRAY_HEADER_CELLS), var(slice_fill))), int(0))?, assign(slice_fill, add(var(slice_fill), int(1)), l)],
            line: l,
        },
        assign(answer, call(heap.node_publish, vec![var(c), field(var(slice_alloc), "slot"), field(var(slice_alloc), "generation"), var(slice_base), var(frame), int(2)]), l),
        assign(destination, int(0), l),
    ]);
    let mut iterate = vec![
        assign(this_arg, choose(lt(int(1), var(count)), get_arg(1), value::undefined()), l),
        branch(produces_array.clone(), callback_array, vec![
            branch(for_each.clone(), vec![assign(answer, value::undefined(), l)], vec![assign(answer, value::boolean(every.clone()), l)], l),
        ], l),
        assign(out, value::result_ok(value::completion_return(var(answer)), sp), l),
        assign(cursor, int(0), l),
        assign(active, E::Bool(true), l),
    ];
    let mut step = vec![
        // The loop bound is the entry length. Rebase each time because a
        // callback can move the backing store or shrink the array.
        assign(base, e.base(var(receiver)), l),
        branch(not(lt(var(cursor), logical_length.clone())), vec![S::Break { line: l }], vec![], l),
        assign(raw, call(heap.array_get, vec![var(c), var(base), var(cursor)]), l),
        // Incoming arguments are retained by the caller. The fetched element
        // needs its own root: guest code may remove it from the receiver.
        e.root(var(frame), int(1), var(raw))?,
    ];
    let emit_callback = |step: &mut Vec<S>, callback_args, called, receiver, payloads: Vec<E>| {
        step.push(S::Let { index: callback_args, declared_schema: Some(T::Array(Box::new(value::schema()))), expr: call(B::ArrayNew.call_index(), vec![]), line: l });
        for payload in payloads {
            step.push(assign(callback_args, call(B::ArrayPush.call_index(), vec![var(callback_args), payload]), l));
        }
        step.push(S::Let {
            index: called,
            declared_schema: Some(value::logical_result_schema()),
            expr: call(dispatch, vec![var(c), var(arg), receiver, var(callback_args)]),
            line: l,
        });
    };
    emit_callback(&mut step, callback_args, called, var(this_arg), vec![var(raw), value::number(num(var(cursor))), var(receiver)]);
    let predicate_value = choose(
        eq(field(var(completion), "kind"), int(value::COMPLETION_RETURN)),
        field(var(completion), "value"),
        value::undefined(),
    );
    let stop = vec![
        assign(out, value::result_ok(value::completion_return(value::boolean(not(every.clone()))), sp), l),
        assign(active, E::Bool(false), l),
    ];
    let store = vec![
        assign(base, e.base(var(answer)), l),
        branch(map.clone(), vec![
            assign(raw, var(primitive), l),
            e.root(var(frame), int(1), var(raw))?,
            assign(destination, var(cursor), l),
        ], vec![
            assign(mutation, call(heap.array_set_length, vec![var(c), var(base), add(var(destination), int(1))]), l),
            assert_stmt(eq(var(mutation), E::Null), l),
        ], l),
        assign(mutation, call(heap.array_put, vec![var(c), var(base), var(destination), var(raw), field(var(answer), "ref_slot"), field(var(answer), "generation")]), l),
        assert_stmt(eq(var(mutation), E::Null), l),
        assign(destination, add(var(destination), int(1)), l),
    ];
    let ok = vec![branch(for_each, vec![], vec![
        assign(primitive, predicate_value, l),
        branch(produces_array, vec![
            branch(or(map, truth(&e, var(primitive))), store, vec![], l),
        ], vec![
            branch(not(eq(truth(&e, var(primitive)), every)), stop, vec![], l),
        ], l),
    ], l)];
    let thrown = vec![
        e.root(var(frame), int(2), var(err))?,
        assign(out, value::result_err(var(err), sp), l),
        assign(active, E::Bool(false), l),
    ];
    step.push(matched(&mut e, called, value::logical_result_schema(), completion, err, ok, thrown.clone())?);
    step.push(assign(cursor, add(var(cursor), int(1)), l));
    iterate.push(S::While { condition: and(var(active), lt(var(cursor), var(len))), body: step, line: l });

    let reduce_right = eq(var(code), int(super::intrinsics::ARRAY_REDUCE_RIGHT_CODE));
    let reduce = or(eq(var(code), int(super::intrinsics::ARRAY_REDUCE_CODE)), reduce_right.clone());
    let reduce_index = |offset: E| choose(reduce_right.clone(), sub(sub(var(len), int(1)), offset.clone()), offset);
    let has_initial = lt(int(1), var(count));
    let mut reduce_iterate = vec![
        assign(cursor, int(0), l),
        branch(has_initial.clone(), vec![assign(answer, get_arg(1), l)], vec![
            assign(answer, call(heap.array_get, vec![var(c), var(base), reduce_index(int(0))]), l),
            assign(cursor, int(1), l),
        ], l),
        e.root(var(frame), int(2), var(answer))?,
        assign(active, E::Bool(true), l),
    ];
    let mut reduce_step = vec![
        assign(present, reduce_index(var(cursor)), l),
        assign(base, e.base(var(receiver)), l),
        branch(not(lt(var(present), logical_length)), vec![S::Break { line: l }], vec![], l),
        assign(raw, call(heap.array_get, vec![var(c), var(base), var(present)]), l),
        e.root(var(frame), int(1), var(raw))?,
    ];
    let reduce_args = e.lower.slot(span)?;
    let reduce_called = e.lower.slot(span)?;
    emit_callback(&mut reduce_step, reduce_args, reduce_called, value::undefined(), vec![
        var(answer), var(raw), value::number(num(var(present))), var(receiver),
    ]);
    let next = choose(
        eq(field(var(completion), "kind"), int(value::COMPLETION_RETURN)),
        field(var(completion), "value"),
        value::undefined(),
    );
    let keep = vec![
        assign(answer, next, l),
        e.root(var(frame), int(2), var(answer))?,
    ];
    reduce_step.push(matched(&mut e, reduce_called, value::logical_result_schema(), completion, err, keep, thrown)?);
    reduce_step.push(assign(cursor, add(var(cursor), int(1)), l));
    reduce_iterate.push(S::While { condition: and(var(active), lt(var(cursor), var(len))), body: reduce_step, line: l });
    reduce_iterate.push(branch(var(active), vec![assign(out, value::result_ok(value::completion_return(var(answer)), sp), l)], vec![], l));

    // A TypeError has its own message. Both strings and the instance reuse
    // the scratch roots; the receiver can be released on this exit path.
    let mut callback_type_error = |text| -> Result<Vec<S>, ParseError> {
        let mut body = Vec::new();
        let key = super::intrinsics::string(&mut e, "message", frame, 0, &mut body)?;
        let message = super::intrinsics::string(&mut e, text, frame, 1, &mut body)?;
        body.push(assign(err, call(intrinsic.allocate, vec![
            var(c), int(3), call(intrinsic.root, vec![var(c), int(3)]), int(1), int(0), int(0), int(6), var(frame), int(2),
        ]), l));
        super::intrinsics::trusted_data(&mut e, var(err), 0, var(key), var(message), (true, false, true), &mut body)?;
        body.push(assign(out, value::result_err(var(err), sp), l));
        Ok(body)
    };
    let not_a_function = callback_type_error("not a function")?;
    let empty_array = callback_type_error("empty array")?;
    let length_changed = callback_type_error("array length was modified")?;
    let reduce_body = vec![branch(and(not(has_initial), eq(var(len), int(0))), empty_array, reduce_iterate, l)];
    let callable = choose(
        tag_is(var(arg), value::OBJECT),
        eq(e.decoded(e.base(var(arg))), int(layout::KIND_FUNCTION)),
        E::Bool(false),
    );
    let callbacks = vec![
        assign(arg, choose(lt(int(0), var(count)), get_arg(0), value::undefined()), l),
        branch(callable, vec![branch(reduce, reduce_body, iterate, l)], not_a_function, l),
    ];
    accepted.push(branch(
        eq(var(code), int(super::intrinsics::ARRAY_PUSH_CODE)),
        push,
        vec![branch(
            eq(var(code), int(super::intrinsics::ARRAY_POP_CODE)),
            pop,
            vec![branch(
                eq(var(code), int(super::intrinsics::ARRAY_SHIFT_CODE)),
                shift,
                vec![branch(
                    eq(var(code), int(super::intrinsics::ARRAY_UNSHIFT_CODE)),
                    unshift,
                    vec![branch(
                        eq(var(code), int(super::intrinsics::ARRAY_REVERSE_CODE)),
                        reverse,
                        vec![branch(
                            or(
                                eq(var(code), int(super::intrinsics::ARRAY_INDEX_OF_CODE)),
                                eq(var(code), int(super::intrinsics::ARRAY_LAST_INDEX_OF_CODE)),
                            ),
                            locate,
                            vec![branch(
                                eq(var(code), int(super::intrinsics::ARRAY_SLICE_CODE)),
                                slice_body,
                                vec![branch(
                                    eq(var(code), int(super::intrinsics::ARRAY_CONCAT_CODE)),
                                    concat_body,
                                    vec![branch(is_array_callback_code(var(code)), callbacks, fail.clone(), l)],
                                    l,
                                )],
                                l,
                            )],
                            l,
                        )],
                        l,
                    )],
                    l,
                )],
                l,
            )],
            l,
        )],
        l,
    ));
    // Both codes stay inside the existing true-Array brand check. Capture
    // length before separator coercion; only the contents are read live.
    let join_frame = e.integer()?;
    let units = e.lower.slot(span)?;
    let sep = e.lower.slot(span)?;
    let part = e.lower.slot(span)?;
    let int_array = T::Array(Box::new(T::Int));
    let unit_len = |slot| call(B::Len.call_index(), vec![E::Borrow(Box::new(var(slot)))]);
    let push_unit = |slot, unit| assign(slot, call(B::ArrayPush.call_index(), vec![E::MoveVar(slot), unit]), l);
    let append_units = |from| vec![
        assign(present, unit_len(from), l),
        assert_stmt(not(lt(int(65_536), add(unit_len(units), var(present)))), l),
        assign(destination, int(0), l),
        S::While {
            condition: lt(var(destination), var(present)),
            body: vec![
                assign(limit, call(B::Get.call_index(), vec![E::Borrow(Box::new(var(from))), var(destination)]), l),
                push_unit(units, var(limit)),
                assign(destination, add(var(destination), int(1)), l),
            ],
            line: l,
        },
    ];
    let mut join = vec![
        assign(len, join_length, l),
        assign(join_frame, call(heap.root_enter, vec![var(c), int(8)]), l),
        assign(active, E::Bool(true), l),
        S::Let { index: units, declared_schema: Some(int_array.clone()), expr: call(B::ArrayNew.call_index(), vec![]), line: l },
        S::Let { index: sep, declared_schema: Some(int_array.clone()), expr: call(B::ArrayNew.call_index(), vec![]), line: l },
        assign(raw, value::undefined(), l),
        branch(
            and(eq(var(code), int(super::intrinsics::ARRAY_JOIN_CODE)), lt(int(0), call(B::Len.call_index(), vec![E::Borrow(Box::new(var(args)))]))),
            vec![assign(raw, get_arg(0), l)], vec![], l,
        ),
    ];
    // The Array adapter roots the receiver in the outer frame. Root the coercion
    // input at 1, leaving 2..=5 to ToString and 7 to result publication.
    // Only unmanaged UTF-16 units survive from one conversion to the next.
    let join_error = vec![
        assign(out, value::result_err(var(err), sp), l),
        assign(active, E::Bool(false), l),
    ];
    let convert = |e: &mut Emit<'_, '_>, success| -> Result<Vec<S>, ParseError> {
        Ok(vec![
            e.root(var(join_frame), int(1), var(raw))?,
            assign(conversion, call(key_string, vec![var(c), var(raw), var(join_frame)]), l),
            matched(e, conversion, result_schema(value::schema()), primitive, err, success, join_error.clone())?,
        ])
    };
    join.push(branch(
        tag_is(var(raw), value::UNDEFINED),
        vec![push_unit(sep, int(i64::from(b',')))],
        convert(&mut e, vec![assign(sep, call(to_units, vec![var(c), var(primitive)]), l)])?,
        l,
    ));
    let mut append_part = vec![S::Let {
        index: part, declared_schema: Some(int_array), expr: call(to_units, vec![var(c), var(primitive)]), line: l,
    }];
    append_part.extend(append_units(part));
    join.push(assign(cursor, int(0), l));
    join.push(S::While {
        condition: and(var(active), lt(var(cursor), var(len))),
        body: vec![
            branch(lt(int(0), var(cursor)), append_units(sep), vec![], l),
            // array_get checks current logical length and physical presence.
            assign(base, e.base(var(receiver)), l),
            assign(raw, call(heap.array_get, vec![var(c), var(base), var(cursor)]), l),
            branch(
                not(or(tag_is(var(raw), value::UNDEFINED), tag_is(var(raw), value::NULL))),
                convert(&mut e, append_part)?, vec![], l,
            ),
            assign(cursor, add(var(cursor), int(1)), l),
        ],
        line: l,
    });
    join.push(branch(var(active), vec![
        assign(answer, call(publish, vec![var(c), E::MoveVar(units), var(join_frame), int(7)]), l),
        assign(out, value::result_ok(value::completion_return(var(answer)), sp), l),
    ], vec![], l));
    join.push(e.effect(heap.root_leave, vec![var(c), var(join_frame)])?);
    let accepted = vec![branch(is_array_join_code(var(code)), join, accepted, l)];
    // Micro js_array_splice snapshots the length before argument coercion and
    // checks it again after allocating the dense array of removed values.
    let splice = {
        let start = e.integer()?;
        let deleted = e.integer()?;
        let inserted = e.integer()?;
        let tail = e.integer()?;
        let shift_by = e.integer()?;
        let new_len = e.integer()?;
        let length = |base| call(decode, vec![call(heap.array_length, vec![var(c), base])]);
        let get = |owner, i| call(heap.array_get, vec![var(c), var(owner), i]);
        let put = |e: &mut Emit<'_, '_>, owner, i| -> Result<Vec<S>, ParseError> {
            Ok(vec![
                e.root(var(frame), int(1), var(arg))?,
                assign(base, e.base(var(owner)), l),
                assign(mutation, call(heap.array_put, vec![var(c), var(base), i, var(arg), field(var(owner), "ref_slot"), field(var(owner), "generation")]), l),
                assert_stmt(eq(var(mutation), E::Null), l),
            ])
        };
        let resize = |e: &Emit<'_, '_>, length| vec![
            assign(base, e.base(var(receiver)), l),
            assign(mutation, call(heap.array_set_length, vec![var(c), var(base), length]), l),
            assert_stmt(eq(var(mutation), E::Null), l),
        ];
        let mut apply = vec![assign(cursor, int(0), l)];
        let mut copy = vec![
            assign(base, e.base(var(receiver)), l),
            assign(arg, get(base, add(var(start), var(cursor))), l),
        ];
        copy.extend(put(&mut e, answer, var(cursor))?);
        copy.push(assign(cursor, add(var(cursor), int(1)), l));
        apply.push(S::While { condition: lt(var(cursor), var(deleted)), body: copy, line: l });
        apply.extend([
            assign(base, e.base(var(receiver)), l),
            assign(present, sub(e.decoded(add(var(base), int(layout::ARRAY_PRESENT_OFFSET))), int(1)), l),
            branch(lt(var(len), var(present)), vec![assign(present, var(len), l)], Vec::new(), l),
            assign(tail, add(var(start), var(deleted)), l),
            assign(shift_by, sub(var(inserted), var(deleted)), l),
            assign(new_len, add(var(len), var(shift_by)), l),
        ]);
        let mut move_one = vec![
            assign(base, e.base(var(receiver)), l),
            assign(arg, get(base, var(cursor)), l),
        ];
        move_one.extend(put(&mut e, receiver, add(var(cursor), var(shift_by)))?);
        let mut grow = resize(&e, var(new_len));
        grow.push(assign(cursor, var(present), l));
        let mut backwards = vec![assign(cursor, sub(var(cursor), int(1)), l)];
        backwards.extend(move_one.clone());
        grow.push(S::While { condition: lt(var(tail), var(cursor)), body: backwards, line: l });
        let mut shrink = vec![assign(cursor, var(tail), l)];
        move_one.push(assign(cursor, add(var(cursor), int(1)), l));
        shrink.push(S::While { condition: lt(var(cursor), var(present)), body: move_one, line: l });
        // Clear the vacated physical tail even if the new logical length is
        // still above it. Restoring the logical length does not materialize it.
        shrink.push(assign(destination, choose(
            lt(var(tail), var(present)), add(var(present), var(shift_by)),
            choose(lt(var(start), var(present)), var(start), var(present)),
        ), l));
        shrink.extend(resize(&e, var(destination)));
        shrink.push(branch(lt(var(destination), var(new_len)), resize(&e, var(new_len)), Vec::new(), l));
        apply.push(branch(lt(int(0), var(shift_by)), grow, vec![
            branch(lt(var(shift_by), int(0)), shrink, Vec::new(), l),
        ], l));
        apply.push(assign(cursor, int(0), l));
        let mut insert = vec![assign(arg, call(B::Get.call_index(), vec![E::Borrow(Box::new(var(args))), add(var(cursor), int(2))]), l)];
        let insert_at = add(var(start), var(cursor));
        // Ordinary packed writes remain number-only; widen this receiver only
        // when splice is about to store a tagged value.
        insert.push(assign(base, e.base(var(receiver)), l));
        insert.push(branch(and(
            not(tag_is(var(arg), value::NUMBER)), eq(e.decoded(var(base)), int(layout::KIND_ARRAY)),
        ), vec![
            e.root(var(frame), int(1), var(arg))?,
            assign(base, call(heap.array_expand, vec![var(c), field(var(receiver), "ref_slot"), field(var(receiver), "generation"), insert_at.clone()]), l),
        ], Vec::new(), l));
        insert.extend(put(&mut e, receiver, insert_at)?);
        insert.push(assign(cursor, add(var(cursor), int(1)), l));
        apply.push(S::While { condition: lt(var(cursor), var(inserted)), body: insert, line: l });
        apply.push(assign(out, value::result_ok(value::completion_return(var(answer)), sp), l));

        // Root 0 holds the receiver, root 1 the value in transit, and root 2
        // the published result. Re-resolve bases after allocating or writing.
        let payload = mul(int(layout::ARRAY_TAGGED_CELLS_PER_ENTRY), var(deleted));
        let mut allocate = vec![
            assign(slice_alloc, call(heap.node_begin, vec![var(c), add(int(layout::ARRAY_HEADER_CELLS), payload.clone()), int(layout::KIND_TAGGED_ARRAY)]), l),
            assign(slice_base, field(var(slice_alloc), "base"), l),
            assign(slice_proto, call(intrinsic.array_proto, vec![var(c)]), l),
        ];
        for (offset, value) in [(2i64, var(deleted)), (3, num(field(var(slice_proto), "ref_slot"))), (4, num(field(var(slice_proto), "generation"))), (layout::ARRAY_PRESENT_OFFSET, add(var(deleted), int(1)))] {
            allocate.push(e.write(add(var(slice_base), int(offset)), value)?);
        }
        allocate.extend([
            assign(slice_fill, int(0), l),
            S::While { condition: lt(var(slice_fill), payload), body: vec![
                e.write(add(var(slice_base), add(int(layout::ARRAY_HEADER_CELLS), var(slice_fill))), int(0))?,
                assign(slice_fill, add(var(slice_fill), int(1)), l),
            ], line: l },
            assign(answer, call(heap.node_publish, vec![var(c), field(var(slice_alloc), "slot"), field(var(slice_alloc), "generation"), var(slice_base), var(frame), int(2)]), l),
            branch(eq(length(e.base(var(receiver))), var(len)), apply, length_changed, l),
        ]);

        let after_delete = vec![
            assign(position, splice_truncated.clone(), l),
            assign(deleted, call(decode, vec![choose(lt(var(position), E::Float(0.0)), E::Float(0.0), choose(lt(num(var(deleted)), var(position)), num(var(deleted)), var(position)))]), l),
            assign(inserted, sub(var(count), int(2)), l),
        ];
        let convert = |e: &mut Emit<'_, '_>, i, ok| -> Result<Vec<S>, ParseError> {
            Ok(vec![
                assign(raw, get_arg(i), l),
                e.root(var(frame), int(1), var(raw))?,
                assign(conversion, call(number_value, vec![var(c), var(raw), E::Bool(false)]), l),
                matched(e, conversion, result_schema(value::schema()), primitive, err, ok, vec![
                    assign(active, E::Bool(false), l),
                    assign(out, value::result_err(var(err), sp), l),
                ])?,
            ])
        };
        let mut after_start = clamp_saved(start, splice_truncated);
        after_start.push(assign(deleted, sub(var(len), var(start)), l));
        after_start.push(branch(lt(int(1), var(count)), convert(&mut e, 1, after_delete)?, Vec::new(), l));
        vec![
            assign(len, length(var(base)), l),
            assign(limit, var(len), l),
            assign(count, call(B::Len.call_index(), vec![E::Borrow(Box::new(var(args)))]), l),
            assign(active, E::Bool(true), l),
            assign(start, int(0), l),
            assign(deleted, int(0), l),
            assign(inserted, int(0), l),
            branch(lt(int(0), var(count)), convert(&mut e, 0, after_start)?, Vec::new(), l),
            branch(var(active), allocate, Vec::new(), l),
        ]
    };
    let accepted = vec![branch(eq(var(code), int(super::intrinsics::ARRAY_SPLICE_CODE)), splice, accepted, l)];
    let sort = array_sort_body(&mut e, receiver, args, out, dispatch, number_value, key_string, to_units)?;
    let accepted = vec![branch(eq(var(code), int(super::intrinsics::ARRAY_SORT_CODE)), sort, accepted, l)];
    let array_brand = vec![branch(
        or(eq(var(kind), int(layout::KIND_ARRAY)), eq(var(kind), int(layout::KIND_TAGGED_ARRAY))),
        accepted, fail.clone(), l,
    )];
    body.push(branch(tag_is(var(receiver), value::OBJECT), vec![
        assign(base, e.base(var(receiver)), l),
        assign(kind, e.decoded(var(base)), l),
        array_brand[0].clone(),
    ], fail, l));
    body.push(e.effect(heap.root_leave, vec![var(c), var(frame)])?);
    let body = e.finish(body);
    declare(ir, stmts, "__compat_es5_array_method", index,
        vec![c, code, receiver, args],
        vec![carrier_schema.clone(), T::Int, value::schema(), T::Array(Box::new(value::schema()))],
        value::logical_result_schema(), body, var(out), l);
    Ok(())
}

pub(crate) fn append(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    dispatch: u16,
) -> Result<ObjectHelpers, ParseError> {
    let start = super::heap::fresh_index(lower, ir, span)?;
    let mut h = ObjectHelpers::at(start);
    for declaration in [own_declaration(), pair_declaration()] {
        ir.struct_schemas
            .insert(declaration.name.clone(), declaration);
    }
    append_own(lower, ir, carrier_schema, span, stmts, h)?;
    append_has(lower, ir, carrier_schema, span, stmts, h)?;
    append_same(lower, ir, carrier_schema, span, stmts, h)?;
    append_reserve(lower, ir, carrier_schema, span, stmts, h)?;
    append_get(lower, ir, carrier_schema, span, stmts, h, dispatch)?;
    append_put(lower, ir, carrier_schema, span, stmts, h, dispatch)?;
    append_delete(lower, ir, carrier_schema, span, stmts, h)?;
    append_define(lower, ir, carrier_schema, span, stmts, h)?;
    append_to_descriptor(lower, ir, carrier_schema, span, stmts, h)?;
    append_from_descriptor(lower, ir, carrier_schema, span, stmts, h)?;
    append_define_properties(lower, ir, carrier_schema, span, stmts, h)?;
    append_api(lower, ir, carrier_schema, span, stmts, h)?;
    append_row_validator(lower, ir, carrier_schema, span, stmts, h.api + 1)?;
    append_index_string(lower, ir, carrier_schema, span, stmts, h.index_string)?;
    append_keys(
        lower,
        ir,
        carrier_schema,
        span,
        stmts,
        h.keys,
        h.index_string,
    )?;
    let number_string = lower.number_string_managed();
    let fixed_string = lower.fixed_scalar_string();
    append_index_key(lower, ir, carrier_schema, span, stmts, h.index_key)?;
    append_key_string(
        lower,
        ir,
        carrier_schema,
        span,
        stmts,
        h.key_string,
        dispatch,
        fixed_string,
        number_string,
        h,
    )?;
    append_proto_call(lower, ir, carrier_schema, span, stmts, h.proto_call, h)?;
    append_function_call(
        lower,
        ir,
        carrier_schema,
        span,
        stmts,
        h.function_call,
        dispatch,
    )?;
    append_function_apply(
        lower,
        ir,
        carrier_schema,
        span,
        stmts,
        h.function_apply,
        dispatch,
    )?;
    append_function_bind(
        lower,
        ir,
        carrier_schema,
        span,
        stmts,
        h.function_bind,
        dispatch,
    )?;
    append_json_stringify(lower, ir, carrier_schema, span, stmts, h.json_stringify)?;
    append_json_parse(lower, ir, carrier_schema, span, stmts, h.json_parse)?;
    append_date_epoch(lower, ir, carrier_schema, span, stmts, h.date_epoch)?;
    append_date_gettime(lower, ir, carrier_schema, span, stmts, h.date_gettime)?;
    // The admitted `/a/` literal publishes its own `test` method row, so this
    // appends the real guest matcher kernel plus the two private helpers that
    // build and dispatch it. Nothing is installed on a permanent intrinsic.
    // The recorded kernel index is the one `append_matcher` actually
    // returned; the String adapter appended below reads this field.
    h.regexp_matcher = super::regexp::append_test_support(
        lower,
        ir,
        carrier_schema,
        span,
        stmts,
        h.regexp_value,
        h.regexp_test,
    )?;
    // Real `String.prototype.charCodeAt` needs the canonical managed
    // StringNumericLiteral parser and the numeric DefaultValue converter. Both
    // stay inside the same contiguous helper window as the adapter above, so
    // the dispatch index reservation still lands on the next free index.
    let parser = super::math::append_string_number_parser(
        lower,
        ir,
        carrier_schema,
        span,
        stmts,
        (
            "__compat_es5_char_code_at_numeric_scan",
            "__compat_es5_char_code_at_round_decimal",
        ),
    )?;
    debug_assert_eq!(parser, h.number_parser());
    if lower.published_number_parser().is_none() {
        lower.publish_math_number_parser(parser);
    }
    let numeric = parser
        .checked_add(2)
        .ok_or_else(|| lower.error(span, "String charCodeAt helper index space exhausted"))?;
    debug_assert_eq!(numeric, h.numeric_default_value);
    super::math::append_default_value(
        lower,
        ir,
        carrier_schema,
        span,
        stmts,
        numeric,
        "__compat_es5_char_code_number",
        h,
        dispatch,
    )?;
    let char_code_at = parser
        .checked_add(3)
        .ok_or_else(|| lower.error(span, "String charCodeAt helper index space exhausted"))?;
    debug_assert_eq!(char_code_at, h.char_code_at);
    append_string_char_code_at(
        lower,
        ir,
        carrier_schema,
        span,
        stmts,
        char_code_at,
        parser,
        numeric,
        h,
    )?;
    append_array_method(lower, ir, carrier_schema, span, stmts, h.array_method, parser, h.numeric_default_value, dispatch, h.key_string)?;
    Ok(h)
}

/// The diagnosed M1 family and its direct numeric helper closure. Raw carrier
/// wrappers deliberately remain untouched; their consumers project Number.
const SCALAR_HELPERS: &[&str] = &[
    "__compat_es5_bootstrap_row_suffix",
    "__compat_es5_bootstrap_ascii_init",
    "__compat_es5_decode_int",
    "__compat_es5_decode_u32",
    "__compat_es5_checked_base",
    "__compat_es5_mark",
    "__compat_es5_heap_collect",
    "__compat_es5_raw_reserve",
    "__compat_es5_root_enter",
    "__compat_es5_root_write",
    "__compat_es5_root_leave",
    "__compat_es5_node_begin",
    "__compat_es5_node_publish",
    "__compat_es5_string_equal",
    "__compat_es5_intrinsic_root",
    "__compat_es5_object_allocate",
    "__compat_es5_intrinsic_bootstrap",
    "__compat_es5_new_type_error",
    "__compat_es5_function_length_key",
    "__compat_es5_get_own_property",
    "__compat_es5_has_property",
    "__compat_es5_same_value",
    "__compat_es5_object_reserve_rows",
    "__compat_es5_ref_get",
    "__compat_es5_ref_put",
    "__compat_es5_delete_property",
    "__compat_es5_define_own_property",
    "__compat_es5_to_property_descriptor",
    "__compat_es5_descriptor_row_validator",
    "__compat_es5_from_property_descriptor",
    "__compat_es5_define_properties",
    "__compat_es5_object_intrinsic_call",
    "__compat_es5_dispatch",
    "__compat_es5_numeric_scan",
    "__compat_es5_char_code_at_numeric_scan",
    "__compat_es5_numeric_primitive",
    "__compat_es5_numeric_call",
    "__compat_es5_char_code_number",
    "__compat_es5_object_proto",
    "__compat_es5_function_call",
    "__compat_es5_function_apply",
    "__compat_es5_function_bind",
    "__compat_es5_index_key",
    "__compat_es5_to_string_key",
    "__compat_es5_array_prototype",
    "__compat_es5_undefined",
];

/// This is a private emitted-helper pass, not source-JS type inference. It
/// preserves expression positions/order and never crosses a callable body.
fn scalar_expr(e: &mut E, visit: &mut impl FnMut(&mut E)) {
    match e {
        E::Add(a, b)
        | E::Sub(a, b)
        | E::Mul(a, b)
        | E::Div(a, b)
        | E::Mod(a, b)
        | E::Eq(a, b)
        | E::Lt(a, b)
        | E::Gt(a, b)
        | E::And(a, b)
        | E::Or(a, b) => {
            scalar_expr(a, visit);
            scalar_expr(b, visit);
        }
        E::Neg(a)
        | E::Not(a)
        | E::ToOwned(a)
        | E::Borrow(a)
        | E::BorrowMut(a)
        | E::ResultConstruct { payload: a, .. }
        | E::ResultQuery { value: a, .. }
        | E::ResultTry { value: a, .. } => scalar_expr(a, visit),
        E::Call(_, _, args, _, _) | E::LocalCall(_, _, args, _) | E::ModuleCall(_, _, args, _) => {
            for a in args {
                scalar_expr(a, visit);
            }
        }
        E::IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            scalar_expr(condition, visit);
            scalar_expr(then_expr, visit);
            scalar_expr(else_expr, visit);
        }
        E::Block { stmts, expr } => {
            scalar_stmts(stmts, visit);
            scalar_expr(expr, visit);
        }
        E::ResultMatch {
            value,
            ok_expr,
            err_expr,
            ..
        } => {
            scalar_expr(value, visit);
            scalar_expr(ok_expr, visit);
            scalar_expr(err_expr, visit);
        }
        E::ResultConvert { value, .. } => scalar_expr(value, visit),
        E::ResultMethod { receiver, argument, .. }
        | E::OptionMethod { receiver, argument, .. } => {
            scalar_expr(receiver, visit);
            scalar_expr(argument, visit);
        }
        E::OptionConstruct { payload, .. } => {
            if let Some(payload) = payload {
                scalar_expr(payload, visit);
            }
        }
        E::OptionMatch {
            value,
            some_expr,
            none_expr,
            ..
        } => {
            scalar_expr(value, visit);
            scalar_expr(some_expr, visit);
            scalar_expr(none_expr, visit);
        }
        E::Match {
            value,
            arms,
            default,
            ..
        } => {
            scalar_expr(value, visit);
            for (_, a) in arms {
                scalar_expr(a, visit);
            }
            scalar_expr(default, visit);
        }
        E::OptionalGet { container, key, .. } => {
            scalar_expr(container, visit);
            scalar_expr(key, visit);
        }
        E::OptionUnwrapOr {
            value, fallback, ..
        } => {
            scalar_expr(value, visit);
            scalar_expr(fallback, visit);
        }
        // Callable environments, opaque values and source bodies are outside
        // this finite helper operand policy.
        E::Closure(_)
        | E::ClosureCall(_, _)
        | E::Null
        | E::Int(_)
        | E::Float(_)
        | E::Bool(_)
        | E::String(_)
        | E::Bytes(_)
        | E::FunctionRef(_, _)
        | E::ModuleFunctionRef(_, _)
        | E::UnresolvedFunctionRef { .. }
        | E::Var(_)
        | E::MoveVar(_)
        | E::MoveField { .. }
        | E::MoveIndex { .. } => {}
        | E::Unit => {}
    }
    visit(e);
}
fn scalar_stmts(stmts: &mut [S], visit: &mut impl FnMut(&mut E)) {
    for s in stmts {
        match s {
            S::Let { expr, .. } | S::Assign { expr, .. } | S::Expr { expr, .. } => {
                scalar_expr(expr, visit)
            }
            S::IfElse {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                scalar_expr(condition, visit);
                scalar_stmts(then_branch, visit);
                scalar_stmts(else_branch, visit);
            }
            S::While {
                condition, body, ..
            } => {
                scalar_expr(condition, visit);
                scalar_stmts(body, visit);
            }
            S::For {
                init,
                condition,
                post,
                body,
                ..
            } => {
                scalar_stmts(std::slice::from_mut(init), visit);
                scalar_expr(condition, visit);
                scalar_stmts(std::slice::from_mut(post), visit);
                scalar_stmts(body, visit);
            }
            S::Noop { .. }
            | S::FuncDecl { .. }
            | S::ClosureLet { .. }
            | S::Break { .. }
            | S::Continue { .. }
            | S::Drop { .. } => {}
        }
    }
}

struct ScalarBoundary<'a> {
    ir: &'a FrontendIr,
    locals: std::collections::HashMap<u16, T>,
    /// All writers, not a destination annotation, must retain concrete I64.
    concrete: std::collections::HashSet<u16>,
    floats: std::collections::HashSet<u16>,
    proven_ints: std::collections::HashSet<u16>,
    identity: u16,
}
impl ScalarBoundary<'_> {
    fn schema(&self, e: &E) -> Option<T> {
        match e {
            E::Int(_) => Some(T::Int),
            E::Float(_) => Some(T::Float),
            E::Bool(_) => Some(T::Bool),
            E::Var(i) | E::MoveVar(i) => {
                let schema = self.locals.get(i)?;
                if *schema == T::Int && !self.proven_ints.contains(i) {
                    return None;
                }
                Some(schema.clone())
            }
            E::Borrow(a) | E::BorrowMut(a) | E::ToOwned(a) | E::Neg(a) => self.schema(a),
            E::Call(i, _, args, _, _) if *i == B::Get.call_index() && args.len() == 2 => {
                let E::String(key) = &args[1] else {
                    return None;
                };
                self.field_schema(&args[0], key)
            }
            E::Call(i, _, _, _, _) => self
                .ir
                .functions
                .iter()
                .find(|f| f.index == *i)?
                .return_schema
                .clone(),
            E::LocalCall(i, _, _, _) => match self.locals.get(i)? {
                T::Callable { result, .. } => Some(*result.clone()),
                _ => None,
            },
            E::Add(a, b) | E::Sub(a, b) | E::Mul(a, b) | E::Div(a, b) | E::Mod(a, b) => {
                match (self.schema(a), self.schema(b)) {
                    (Some(T::Int), Some(T::Int)) => Some(T::Int),
                    (Some(T::Int | T::Float | T::Number), Some(T::Int | T::Float | T::Number)) => {
                        Some(T::Number)
                    }
                    _ => None,
                }
            }
            E::Eq(_, _) | E::Lt(_, _) | E::Gt(_, _) | E::Not(_) | E::And(_, _) | E::Or(_, _) => {
                Some(T::Bool)
            }
            E::IfElse {
                then_expr,
                else_expr,
                ..
            } => {
                let a = self.schema(then_expr)?;
                let b = self.schema(else_expr)?;
                (a == b).then_some(a)
            }
            E::Block { expr, .. } => self.schema(expr),
            E::ResultTry {
                operand_schema: T::Result { ok, .. },
                ..
            } => Some(*ok.clone()),
            _ => None,
        }
    }
    fn field_schema(&self, record: &E, key: &str) -> Option<T> {
        // Literal Set chains are actual private producers, not an assumed
        // annotation on their result. Preserve the record and its evaluation.
        match record {
            E::Borrow(a) | E::BorrowMut(a) | E::ToOwned(a) => return self.field_schema(a, key),
            E::Call(i, _, args, _, _) if *i == B::Set.call_index() && args.len() == 3 => {
                if matches!(&args[1], E::String(k) if k == key) {
                    return self.schema(&args[2]);
                }
                return self.field_schema(&args[0], key);
            }
            _ => {}
        }
        let T::Named(name, _) = self.schema(record)? else {
            return None;
        };
        // Only compiler-owned scalar record domains. Bool and opaque fields
        // remain untouched even inside these same nominal records.
        if !matches!(
            name.as_str(),
            value::TYPE_NAME
                | value::TYPE_REF
                | value::TYPE_COMPLETION
                | layout::TYPE_ALLOCATION
                | DESCRIPTOR
                | OWN
        ) {
            return None;
        }
        let T::Object(fields) = &self.ir.struct_schemas.get(&name)?.body_schema else {
            return None;
        };
        fields.get(key).cloned()
    }
    fn i64(&self, e: &E) -> bool {
        match e {
            E::Int(_) => true,
            E::Var(i) => self.concrete.contains(i),
            E::Call(i, _, _, _, _) => self.ir.functions.iter().any(|f| {
                f.index == *i && f.return_type == ValueType::Int && f.return_schema == Some(T::Int)
            }),
            E::Add(a, b) | E::Sub(a, b) | E::Mul(a, b) | E::Div(a, b) => self.i64(a) && self.i64(b),
            E::IfElse {
                then_expr,
                else_expr,
                ..
            } => self.i64(then_expr) && self.i64(else_expr),
            _ => false,
        }
    }
    fn int_operand(&self, e: &mut Box<E>) {
        if !self.i64(e) {
            let operand = std::mem::replace(e.as_mut(), E::Null);
            **e = call(self.identity, vec![operand]);
        }
    }
    fn f64(&self, e: &E) -> bool {
        match e {
            E::Float(_) => true,
            E::Var(i) => self.floats.contains(i),
            E::Call(i, _, _, _, _) => self
                .ir
                .functions
                .iter()
                .any(|f| f.index == *i && f.return_type == ValueType::Float),
            E::Div(_, b) if matches!(b.as_ref(), E::Float(n) if *n == 1.0) => true,
            E::Add(a, b) | E::Sub(a, b) | E::Mul(a, b) | E::Div(a, b) => {
                (self.f64(a) && matches!(self.schema(b), Some(T::Int | T::Float | T::Number)))
                    || (self.f64(b)
                        && matches!(self.schema(a), Some(T::Int | T::Float | T::Number)))
            }
            E::IfElse {
                then_expr,
                else_expr,
                ..
            } => self.f64(then_expr) && self.f64(else_expr),
            _ => false,
        }
    }
    fn number_operand(&self, e: &mut Box<E>) {
        if !self.f64(e) {
            let operand = std::mem::replace(e.as_mut(), E::Null);
            **e = num(operand);
        }
    }
    fn apply(&self, e: &mut E) {
        // An existing projection is already a physical F64 producer. Its
        // inner arithmetic was visited first, so it cannot hide a bad Add.
        if matches!(e, E::Div(_,b) if matches!(b.as_ref(), E::Float(n) if *n == 1.0)) {
            return;
        }
        match e {
            E::Add(a, b)
            | E::Sub(a, b)
            | E::Mul(a, b)
            | E::Div(a, b)
            | E::Eq(a, b)
            | E::Lt(a, b)
            | E::Gt(a, b) => {
                match (self.schema(a), self.schema(b)) {
                    (Some(T::Int), Some(T::Int)) => {
                        self.int_operand(a);
                        self.int_operand(b);
                    }
                    (Some(T::Int | T::Float | T::Number), Some(T::Int | T::Float | T::Number)) => {
                        self.number_operand(a);
                        self.number_operand(b);
                    }
                    _ => {} // Bool, opaque/Result/resource and unproved domains.
                }
            }
            _ => {}
        }
    }
}

fn scalar_locals(
    stmts: &[S],
    locals: &mut std::collections::HashMap<u16, T>,
    writers: &mut std::collections::HashMap<u16, Vec<E>>,
) {
    for s in stmts {
        match s {
            S::Let {
                index,
                declared_schema,
                expr,
                ..
            } => {
                if let Some(t) = declared_schema {
                    locals.insert(*index, t.clone());
                }
                writers.entry(*index).or_default().push(expr.clone());
            }
            S::Assign { index, expr, .. } => {
                writers.entry(*index).or_default().push(expr.clone());
            }
            S::IfElse {
                then_branch,
                else_branch,
                ..
            } => {
                scalar_locals(then_branch, locals, writers);
                scalar_locals(else_branch, locals, writers);
            }
            S::While { body, .. } => scalar_locals(body, locals, writers),
            S::For {
                init, post, body, ..
            } => {
                scalar_locals(std::slice::from_ref(init), locals, writers);
                scalar_locals(std::slice::from_ref(post), locals, writers);
                scalar_locals(body, locals, writers);
            }
            _ => {}
        }
    }
}

pub(crate) fn apply_scalar_boundaries(ir: &mut FrontendIr) {
    let Some(identity) = ir
        .functions
        .iter()
        .find(|f| f.name == "__compat_es5_int_identity")
        .map(|f| f.index)
    else {
        return;
    };
    // Stable declaration/producer view: modifications cannot change domains
    // or cause one sibling's representation facts to leak into another.
    let original = ir.clone();
    for decl in &original.functions {
        if !SCALAR_HELPERS.contains(&decl.name.as_str()) {
            continue;
        }
        let Some(body) = original.function_impls.get(&decl.index) else {
            continue;
        };
        let mut locals = std::collections::HashMap::new();
        for (slot, schema) in body.param_slots.iter().zip(&decl.arg_schemas) {
            if let Some(t) = schema {
                locals.insert(*slot, t.clone());
            }
        }
        let mut writers = std::collections::HashMap::new();
        scalar_locals(&body.body_stmts, &mut locals, &mut writers);
        // Blocks are expression-local control flow, including pending-Result
        // handoffs. Their assignments participate in the same writer proof.
        let mut collect = |e: &mut E| match e {
            E::Block { stmts, .. } => scalar_locals(stmts, &mut locals, &mut writers),
            E::ResultMatch {
                ok_binding,
                err_binding,
                scrutinee_schema: T::Result { ok, err },
                ..
            } => {
                if let Some(i) = ok_binding {
                    locals.entry(*i).or_insert(*ok.clone());
                }
                if let Some(i) = err_binding {
                    locals.entry(*i).or_insert(*err.clone());
                }
            }
            _ => {}
        };
        scalar_stmts(&mut body.body_stmts.clone(), &mut collect);
        scalar_expr(&mut body.body_expr.clone(), &mut collect);
        let concrete = locals
            .iter()
            .filter_map(|(i, t)| (*t == T::Int).then_some(*i))
            .collect();
        let floats = locals
            .iter()
            .filter_map(|(i, t)| {
                (matches!(t, T::Float | T::Number)
                    && (!body.param_slots.contains(i) || *t == T::Float))
                    .then_some(*i)
            })
            .collect();
        let proven_ints = locals
            .iter()
            .filter_map(|(i, t)| (*t == T::Int).then_some(*i))
            .collect();
        let mut policy = ScalarBoundary {
            ir: &original,
            locals,
            concrete,
            floats,
            proven_ints,
            identity,
        };
        // Logical Int is admitted only when all actual definitions remain in
        // the Int domain. A declaration with a Float/unknown writer is not a
        // license to call the identity boundary.
        loop {
            let remove: Vec<_> = policy
                .proven_ints
                .iter()
                .filter(|i| {
                    writers
                        .get(i)
                        .is_some_and(|defs| defs.iter().any(|e| policy.schema(e) != Some(T::Int)))
                })
                .copied()
                .collect();
            if remove.is_empty() {
                break;
            }
            for i in remove {
                policy.proven_ints.remove(&i);
                policy.concrete.remove(&i);
            }
        }
        loop {
            let remove: Vec<_> = policy
                .floats
                .iter()
                .filter(|i| {
                    writers
                        .get(i)
                        .is_some_and(|defs| defs.iter().any(|e| !policy.f64(e)))
                })
                .copied()
                .collect();
            if remove.is_empty() {
                break;
            }
            for i in remove {
                policy.floats.remove(&i);
            }
        }
        // Greatest fixed point over every writer. Int-only cycles stay I64;
        // any generic Get/unknown producer propagates uncertainty to users.
        loop {
            let remove: Vec<_> = policy
                .concrete
                .iter()
                .filter(|i| {
                    writers
                        .get(i)
                        .is_some_and(|defs| defs.iter().any(|e| !policy.i64(e)))
                })
                .copied()
                .collect();
            if remove.is_empty() {
                break;
            }
            for i in remove {
                policy.concrete.remove(&i);
            }
        }
        let body = ir.function_impls.get_mut(&decl.index).unwrap();
        scalar_stmts(&mut body.body_stmts, &mut |e| policy.apply(e));
        scalar_expr(&mut body.body_expr, &mut |e| policy.apply(e));
    }
}

#[cfg(test)]
mod scalar_boundary_tests {
    use super::*;
    use vm::SourcePlugin;

    #[test]
    fn p7_row14_first_definitions_reject_nondominating_reads() {
        use std::collections::{BTreeMap, BTreeSet};
        let schemas = BTreeMap::from([(2, T::Int), (3, T::Int)]);
        let mut body = vec![
            branch(E::Bool(true), vec![assign(2, int(7), 1)], vec![], 1),
            assign(3, var(2), 1),
        ];
        assert_eq!(
            row_validator_first_definitions(
                &mut body,
                &schemas,
                &mut BTreeSet::new(),
                &mut BTreeSet::new()
            ),
            Err("row validator read lacks a dominating definition")
        );
        let mut body = vec![
            assign(2, int(0), 1),
            S::While {
                condition: lt(var(2), int(3)),
                body: vec![assign(2, add(var(2), int(1)), 1)],
                line: 1,
            },
            assign(3, var(2), 1),
        ];
        let original = body.clone();
        let mut placed = BTreeSet::new();
        row_validator_first_definitions(&mut body, &schemas, &mut BTreeSet::new(), &mut placed)
            .unwrap();
        assert_eq!(placed, BTreeSet::from([2, 3]));
        assert!(matches!(
            body[0],
            S::Let {
                declared_schema: Some(T::Int),
                ..
            }
        ));
        assert_eq!(format!("{:?}", body[1]), format!("{:?}", original[1]));
    }

    // This is a physical producer proof for the test oracle only. It does
    // not grant ScalarBoundary an optimization or identity-adapter license.
    fn unwritten_bytes_parameter_len(
        e: &E,
        param_slots: &[u16],
        arg_schemas: &[Option<T>],
        writers: &std::collections::HashMap<u16, Vec<E>>,
    ) -> bool {
        let E::Call(i, type_args, args, meta, semantic) = e else {
            return false;
        };
        if *i != B::Len.call_index()
            || !type_args.is_empty()
            || meta.is_some()
            || semantic.is_some()
        {
            return false;
        }
        let [E::Borrow(receiver)] = args.as_slice() else {
            return false;
        };
        let E::Var(slot) = receiver.as_ref() else {
            return false;
        };
        // Zip alone could hide missing schemas or duplicate parameter slots.
        if param_slots.len() != arg_schemas.len()
            || param_slots
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != param_slots.len()
        {
            return false;
        }
        param_slots
            .iter()
            .zip(arg_schemas)
            .any(|(i, schema)| i == slot && schema.as_ref() == Some(&T::Bytes))
            && writers.get(slot).is_none_or(Vec::is_empty)
    }

    #[test]
    fn p7_scalar_private_int_producers_and_domain_exclusions() {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = vm::SourceFrontendContext {
            source_id: 7,
            original_path: None,
            phase: vm::compiler::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let ir = crate::JAVASCRIPT_ES5_PLUGIN
            .parse_source("var o={x:1};o;", &context)
            .unwrap();
        let identity = ir
            .functions
            .iter()
            .find(|f| f.name == "__compat_es5_int_identity")
            .unwrap()
            .index;
        let mut producers = 0;
        let mut writers_checked = 0;
        let mut physical_bytes_len_writers = 0;
        for decl in &ir.functions {
            if !SCALAR_HELPERS.contains(&decl.name.as_str()) {
                continue;
            }
            let body = &ir.function_impls[&decl.index];
            let mut locals = std::collections::HashMap::new();
            for (i, t) in body.param_slots.iter().zip(&decl.arg_schemas) {
                if let Some(t) = t {
                    locals.insert(*i, t.clone());
                }
            }
            let mut writers = std::collections::HashMap::new();
            scalar_locals(&body.body_stmts, &mut locals, &mut writers);
            let mut collect = |e: &mut E| match e {
                E::Block { stmts, .. } => scalar_locals(stmts, &mut locals, &mut writers),
                E::ResultMatch {
                    ok_binding,
                    err_binding,
                    scrutinee_schema: T::Result { ok, err },
                    ..
                } => {
                    if let Some(i) = ok_binding {
                        locals.entry(*i).or_insert(*ok.clone());
                    }
                    if let Some(i) = err_binding {
                        locals.entry(*i).or_insert(*err.clone());
                    }
                }
                _ => {}
            };
            scalar_stmts(&mut body.body_stmts.clone(), &mut collect);
            scalar_expr(&mut body.body_expr.clone(), &mut collect);
            let proven_ints = locals
                .iter()
                .filter_map(|(i, t)| (*t == T::Int).then_some(*i))
                .collect();
            let policy = ScalarBoundary {
                ir: &ir,
                locals,
                concrete: Default::default(),
                floats: Default::default(),
                proven_ints,
                identity,
            };
            for (i, definitions) in &writers {
                if policy.locals.get(i) == Some(&T::Int) {
                    for definition in definitions {
                        let physical_bytes_len = unwritten_bytes_parameter_len(
                            definition,
                            &body.param_slots,
                            &decl.arg_schemas,
                            &writers,
                        );
                        if physical_bytes_len {
                            assert_eq!(policy.schema(definition), None);
                            physical_bytes_len_writers += 1;
                        }
                        assert!(
                            policy.schema(definition) == Some(T::Int) || physical_bytes_len,
                            "actual Int writer in {} slot {i}: {definition:?}",
                            decl.name
                        );
                        writers_checked += 1;
                    }
                }
            }
            let mut check = |e: &mut E| {
                if let E::Call(i, _, args, _, _) = e
                    && *i == B::Set.call_index()
                    && args.len() == 3
                    && matches!(&args[1],E::String(k) if matches!(k.as_str(), "tag" | "ref_slot" | "generation" | "index" | "present" | "slot" | "key_slot" | "key_generation" | "kind" | "target"))
                {
                    assert_eq!(
                        policy.schema(&args[2]),
                        Some(T::Int),
                        "actual private scalar field producer in {}: {args:?}",
                        decl.name
                    );
                    producers += 1;
                }
            };
            scalar_stmts(&mut body.body_stmts.clone(), &mut check);
            scalar_expr(&mut body.body_expr.clone(), &mut check);
        }
        assert!(producers > 100 && writers_checked > 100);
        assert_eq!(physical_bytes_len_writers, 1);

        // Exercise all proof guards in this maintained identity. Synthetic
        // controls cannot substitute for the real parsed writer counted above.
        let bytes_slot = 2;
        let params = [bytes_slot, 3];
        let schemas = [Some(T::Bytes), Some(T::Int)];
        let no_writers = std::collections::HashMap::new();
        let receiver = E::Borrow(Box::new(var(bytes_slot)));
        let len = call(B::Len.call_index(), vec![receiver.clone()]);
        assert!(unwritten_bytes_parameter_len(
            &len,
            &params,
            &schemas,
            &no_writers
        ));
        let empty_writers = std::collections::HashMap::from([(bytes_slot, vec![])]);
        assert!(unwritten_bytes_parameter_len(
            &len,
            &params,
            &schemas,
            &empty_writers
        ));
        let mut negative_guards = 0;
        let mut reject =
            |e: &E,
             slots: &[u16],
             types: &[Option<T>],
             writers: &std::collections::HashMap<u16, Vec<E>>| {
                assert!(
                    !unwritten_bytes_parameter_len(e, slots, types, writers),
                    "excluded physical producer: {e:?}, parameters={slots:?}, schemas={types:?}"
                );
                negative_guards += 1;
            };
        for schema in [
            T::Array(Box::new(T::Int)),
            T::String,
            T::Map(Box::new(T::Int)),
            T::Unknown,
            T::Bool,
            T::Number,
            T::Int,
            value::schema(),
            T::Optional(Box::new(T::Bytes)),
        ] {
            reject(&len, &params, &[Some(schema), Some(T::Int)], &no_writers);
        }
        // Missing, shifted, extra, and duplicate parameter associations are
        // rejected even when zip would encounter an apparently valid pair.
        for (slots, types) in [
            (vec![], vec![]),
            (vec![3], vec![Some(T::Bytes)]),
            (params.to_vec(), vec![None, Some(T::Int)]),
            (params.to_vec(), vec![Some(T::Int), Some(T::Bytes)]),
            (params.to_vec(), vec![Some(T::Bytes)]),
            (vec![bytes_slot], schemas.to_vec()),
            (vec![bytes_slot, bytes_slot], schemas.to_vec()),
            (
                vec![bytes_slot, bytes_slot],
                vec![Some(T::Bytes), Some(T::Bytes)],
            ),
            (
                vec![bytes_slot, 3, 3],
                vec![Some(T::Bytes), Some(T::Int), Some(T::Int)],
            ),
        ] {
            reject(&len, &slots, &types, &no_writers);
        }
        for e in [
            call(B::Get.call_index(), vec![receiver.clone(), int(0)]),
            call(B::Len.call_index(), vec![]),
            call(B::Len.call_index(), vec![receiver.clone(), int(0)]),
            call(B::Len.call_index(), vec![var(bytes_slot)]),
            call(
                B::Len.call_index(),
                vec![E::BorrowMut(Box::new(var(bytes_slot)))],
            ),
            call(B::Len.call_index(), vec![E::Bytes(vec![1])]),
            call(
                B::Len.call_index(),
                vec![E::Borrow(Box::new(E::Bytes(vec![1])))],
            ),
            call(
                B::Len.call_index(),
                vec![E::Borrow(Box::new(var(u16::MAX)))],
            ),
            E::Call(
                B::Len.call_index(),
                vec![T::Bytes],
                vec![receiver.clone()],
                None,
                None,
            ),
            E::Call(
                B::Len.call_index(),
                vec![],
                vec![receiver.clone()],
                None,
                Some(vm::compiler::ir::SemanticNodeId(1)),
            ),
        ] {
            reject(&e, &params, &schemas, &no_writers);
        }
        let metadata = vm::compiler::ResolvedHostCall {
            name: "len".into(),
            params: vec![vm::compiler::ResolvedHostParam {
                name: "bytes".into(),
                schema: T::Bytes,
            }],
            return_type: T::Int,
            passing: vec![vm::host_api::HostParamPassing::Borrow],
            fingerprint: serde_json::from_str("7").unwrap(),
            host_params: vec![vm::host_api::HostTypeSchema::Bytes],
            host_return_type: vm::host_api::HostTypeSchema::Int,
        };
        reject(
            &E::Call(
                B::Len.call_index(),
                vec![],
                vec![receiver],
                Some(Box::new(metadata)),
                None,
            ),
            &params,
            &schemas,
            &no_writers,
        );
        let mut local_schemas = std::collections::HashMap::new();
        let mut local_writers = std::collections::HashMap::new();
        scalar_locals(
            &[S::Let {
                index: bytes_slot,
                declared_schema: Some(T::Bytes),
                expr: E::Bytes(vec![1]),
                line: 1,
            }],
            &mut local_schemas,
            &mut local_writers,
        );
        assert_eq!(local_schemas.get(&bytes_slot), Some(&T::Bytes));
        reject(&len, &[], &[], &local_writers);
        // Even a Bytes-annotated slot with no remaining definitions is not
        // a parameter, and an actual parameter with any writer is excluded.
        reject(&len, &[], &[], &no_writers);
        reject(&len, &params, &schemas, &local_writers);
        let mut reassigned_writers = std::collections::HashMap::new();
        scalar_locals(
            &[branch(
                E::Bool(true),
                vec![assign(bytes_slot, E::Bytes(vec![2]), 1)],
                vec![],
                1,
            )],
            &mut local_schemas,
            &mut reassigned_writers,
        );
        assert_eq!(reassigned_writers[&bytes_slot].len(), 1);
        reject(&len, &params, &schemas, &reassigned_writers);
        let unrelated_writers = std::collections::HashMap::from([(3, vec![int(1)])]);
        assert!(unwritten_bytes_parameter_len(
            &len,
            &params,
            &schemas,
            &unrelated_writers
        ));
        let policy = ScalarBoundary {
            ir: &ir,
            locals: Default::default(),
            concrete: Default::default(),
            floats: Default::default(),
            proven_ints: Default::default(),
            identity,
        };
        // Source Bool/opaque/unknown values have no numeric-adapter license.
        for mut e in [
            eq(E::Bool(false), E::Bool(true)),
            add(var(u16::MAX), int(1)),
            eq(value::undefined(), value::null()),
        ] {
            reject(&e, &params, &schemas, &no_writers);
            let before = format!("{e:?}");
            policy.apply(&mut e);
            assert_eq!(format!("{e:?}"), before);
        }
        let mut number = mul(int(16), E::Float(-0.0));
        reject(&number, &params, &schemas, &no_writers);
        policy.apply(&mut number);
        reject(&number, &params, &schemas, &no_writers);
        assert!(
            matches!(number,E::Mul(a,b) if matches!(a.as_ref(),E::Div(_,unit) if matches!(unit.as_ref(),E::Float(n) if *n == 1.0)) && matches!(b.as_ref(),E::Float(n) if *n == 0.0 && n.is_sign_negative()))
        );
        let mut declared_only = ScalarBoundary {
            ir: &ir,
            locals: std::collections::HashMap::from([(0, T::Int)]),
            concrete: Default::default(),
            floats: Default::default(),
            proven_ints: Default::default(),
            identity,
        };
        let mut e = add(var(0), int(1));
        reject(&e, &params, &schemas, &no_writers);
        let before = format!("{e:?}");
        declared_only.apply(&mut e);
        assert_eq!(
            format!("{e:?}"),
            before,
            "an annotation without a proven Int producer cannot authorize identity"
        );
        declared_only.proven_ints.insert(0);
        declared_only.apply(&mut e);
        assert!(
            matches!(e,E::Add(a,_) if matches!(a.as_ref(),E::Call(i,_,args,_,_) if *i == identity && matches!(args.as_slice(),[E::Var(0)])))
        );
        eprintln!(
            "P7_SCALAR_BYTES_LEN physical_writers={physical_bytes_len_writers} negative_guards={negative_guards} production_schema=None"
        );
        eprintln!(
            "P7_SCALAR_PRODUCERS int_fields={producers} int_writers={writers_checked} Bool_opaque_unknown_unchanged=true Number_power_two_not_Shl=true"
        );
    }
}
