//! TypedArray adapters. Array's own join/toString dispatch is unchanged.
use super::*;
use crate::javascript::prelude::math::math_ext::to_uint32;

struct Method {
    code: u16,
    receiver: u16,
    args: u16,
    out: u16,
    frame: u16,
    active: u16,
    converted: u16,
    primitive: u16,
    err: u16,
    n: u16,
    objects: ObjectHelpers,
}

impl Method {
    fn new(
        e: &mut Emit<'_, '_>,
        code: u16,
        receiver: u16,
        args: u16,
        out: u16,
    ) -> Result<Self, ParseError> {
        let sp = e.lower.ir_span(e.span);
        Ok(Self {
            code,
            receiver,
            args,
            out,
            frame: e.integer()?,
            active: e.boolean()?,
            converted: e.temp(
                value::value_result_schema(),
                result(value::value_result_schema(), true, value::undefined(), sp),
            )?,
            primitive: e.boxed()?,
            err: e.boxed()?,
            n: e.temp(T::Float, E::Float(0.0))?,
            objects: e.lower.objects.expect("TypedArray object runtime"),
        })
    }

    fn done(&self, e: &Emit<'_, '_>, answer: E) -> S {
        assign(
            self.out,
            value::result_ok(value::completion_return(answer), e.lower.ir_span(e.span)),
            e.line,
        )
    }

    fn fail(&self, e: &Emit<'_, '_>) -> Vec<S> {
        vec![
            assign(
                self.out,
                value::result_err(var(self.err), e.lower.ir_span(e.span)),
                e.line,
            ),
            assign(self.active, E::Bool(false), e.line),
        ]
    }

    fn error(
        &self,
        e: &mut Emit<'_, '_>,
        message: &str,
        range: bool,
    ) -> Result<Vec<S>, ParseError> {
        let mut body = error(e, message, range, self.err)?;
        body.extend(self.fail(e));
        Ok(body)
    }

    fn arg(&self, i: i64) -> E {
        let args = || E::Borrow(Box::new(var(self.args)));
        choose(
            lt(int(i), call(B::Len.call_index(), vec![args()])),
            call(B::Get.call_index(), vec![args(), int(i)]),
            value::undefined(),
        )
    }

    fn matched_value(
        &self,
        e: &mut Emit<'_, '_>,
        expr: E,
        ok: Vec<S>,
    ) -> Result<Vec<S>, ParseError> {
        Ok(vec![
            assign(self.converted, expr, e.line),
            matched(
                e,
                self.converted,
                value::value_result_schema(),
                self.primitive,
                self.err,
                ok,
                self.fail(e),
            )?,
        ])
    }

    fn to_number(
        &self,
        e: &mut Emit<'_, '_>,
        raw: E,
        mut ok: Vec<S>,
    ) -> Result<Vec<S>, ParseError> {
        ok.insert(
            0,
            assign(
                self.n,
                number(
                    e,
                    e.lower
                        .published_number_parser()
                        .expect("TypedArray number parser"),
                    self.primitive,
                ),
                e.line,
            ),
        );
        self.matched_value(
            e,
            call(
                self.objects.numeric_default_value,
                vec![var(e.carrier), raw, E::Bool(false)],
            ),
            ok,
        )
    }

    fn get(
        &self,
        e: &mut Emit<'_, '_>,
        obj: E,
        key: u16,
        ok: Vec<S>,
    ) -> Result<Vec<S>, ParseError> {
        let reference = value::reference(
            obj,
            field(var(key), "ref_slot"),
            field(var(key), "generation"),
            E::Bool(false),
        );
        self.matched_value(
            e,
            call(self.objects.get, vec![var(e.carrier), reference]),
            ok,
        )
    }

    fn length(&self, e: &mut Emit<'_, '_>, obj: E, len: u16) -> Result<Vec<S>, ParseError> {
        let mut body = Vec::new();
        let key = intrinsics::string(e, "length", self.frame, 0, &mut body)?;
        let (u32_value, mut convert) = to_uint32(e, e.line, self.n)?;
        convert.push(assign(
            len,
            call(e.decode_u32, vec![var(u32_value)]),
            e.line,
        ));
        let raw = e.boxed()?;
        let mut got = vec![
            assign(raw, var(self.primitive), e.line),
            e.root(var(self.frame), int(1), var(raw))?,
        ];
        got.extend(self.to_number(e, var(raw), convert)?);
        body.extend(self.get(e, obj, key, got)?);
        Ok(body)
    }

    fn index_key(&self, e: &Emit<'_, '_>, cursor: u16) -> E {
        call(
            self.objects.index_string,
            vec![var(e.carrier), var(cursor), var(self.frame), int(0)],
        )
    }
}

fn trunc_or_zero(n: u16) -> E {
    choose(
        eq(var(n), var(n)),
        num(call(B::MathTrunc.call_index(), vec![var(n)])),
        E::Float(0.0),
    )
}

fn relative(e: &Emit<'_, '_>, n: u16, len: u16) -> E {
    // Clamp to the small checked window before exact integer decoding. Values
    // beyond signed 32-bit saturate to the same endpoint as JS_ToInt32Clamp.
    let length = num(var(len));
    let adjusted = choose(
        lt(var(n), E::Float(0.0)),
        add(var(n), length.clone()),
        var(n),
    );
    call(
        e.decode,
        vec![num(choose(
            lt(adjusted.clone(), E::Float(0.0)),
            E::Float(0.0),
            choose(lt(length.clone(), adjusted.clone()), length, adjusted),
        ))],
    )
}

fn subarray(e: &mut Emit<'_, '_>, method: &Method, base: u16) -> Result<Vec<S>, ParseError> {
    let l = e.line;
    let start = e.integer()?;
    let end = e.integer()?;
    let raw = e.boxed()?;
    let mut body = Vec::new();
    let window = checked_window(e, base, &mut body)?;
    body.push(assign(end, var(window.length), l));
    let bound = |e: &mut Emit<'_, '_>, dst| {
        method.to_number(
            e,
            var(raw),
            vec![
                assign(method.n, trunc_or_zero(method.n), l),
                assign(dst, relative(e, method.n, window.length), l),
            ],
        )
    };
    body.push(assign(raw, method.arg(0), l));
    body.extend(bound(e, start)?);
    body.push(assign(raw, method.arg(1), l));
    body.push(branch(
        and(var(method.active), not(tag_is(var(raw), value::UNDEFINED))),
        bound(e, end)?,
        vec![],
        l,
    ));

    let heap = e.lower.heap_ref(e.span)?.helpers;
    let c = e.carrier;
    let buffer = e.boxed()?;
    let proto_slot = e.integer()?;
    let proto_generation = e.integer()?;
    let allocation = e.temp(
        value::allocation_schema(),
        value::allocation(int(0), int(0), int(0)),
    )?;
    let view = e.boxed()?;
    // mqjs stores an element offset (mquickjs.c:15379, 15436–15437).
    // Our checked window stores bytes. The receiver roots both edges while
    // node_begin may collect; keep identities, never a raw address, across it.
    let mut publish = vec![
        assign(base, e.base(var(method.receiver)), l),
        assign(buffer, backing(e, var(base)), l),
        assign(proto_slot, cell(e, base, 3), l),
        assign(proto_generation, e.generation(add(var(base), int(4))), l),
        assign(
            allocation,
            call(
                heap.node_begin,
                vec![var(c), int(layout::VIEW_CELLS), int(layout::KIND_VIEW)],
            ),
            l,
        ),
        assign(base, field(var(allocation), "base"), l),
    ];
    for (field_offset, payload) in [
        (2, int(0)),
        (3, var(proto_slot)),
        (4, var(proto_generation)),
        (layout::VIEW_BUFFER, field(var(buffer), "ref_slot")),
        (layout::VIEW_BUFFER + 1, field(var(buffer), "generation")),
        (
            layout::VIEW_OFFSET,
            add(var(window.offset), mul(var(start), var(window.width))),
        ),
        (
            layout::VIEW_LENGTH,
            choose(lt(var(start), var(end)), sub(var(end), var(start)), int(0)),
        ),
        (layout::VIEW_ELEMENT, var(window.element)),
        (layout::VIEW_WIDTH, var(window.width)),
    ] {
        publish.push(e.write(add(var(base), int(field_offset)), payload)?);
    }
    publish.extend([
        assign(
            view,
            call(
                heap.node_publish,
                vec![
                    var(c),
                    field(var(allocation), "slot"),
                    field(var(allocation), "generation"),
                    var(base),
                    var(method.frame),
                    int(7),
                ],
            ),
            l,
        ),
        method.done(e, var(view)),
    ]);
    body.push(branch(var(method.active), publish, vec![], l));
    Ok(body)
}

fn copy_bytes(
    e: &mut Emit<'_, '_>,
    dst: &Window,
    src: &Window,
    offset: u16,
) -> Result<Vec<S>, ParseError> {
    let l = e.line;
    let from = e.integer()?;
    let to = e.integer()?;
    let count = e.integer()?;
    let cursor = e.integer()?;
    let reverse = e.boolean()?;
    let byte = e.integer()?;
    let data = |w: &Window| {
        add(
            add(var(w.bytes), int(layout::BYTES_HEADER_CELLS)),
            var(w.offset),
        )
    };
    // No allocations or guest calls after obtaining the checked addresses.
    // Backward traversal only matters for a rightward overlap in one backing.
    Ok(vec![
        assign(from, data(src), l),
        assign(to, add(data(dst), mul(var(offset), var(dst.width))), l),
        assign(count, mul(var(src.length), var(src.width)), l),
        assign(
            reverse,
            and(
                lt(var(from), var(to)),
                lt(var(to), add(var(from), var(count))),
            ),
            l,
        ),
        assign(cursor, int(0), l),
        S::While {
            condition: lt(var(cursor), var(count)),
            body: vec![
                assign(
                    byte,
                    choose(
                        var(reverse),
                        sub(sub(var(count), int(1)), var(cursor)),
                        var(cursor),
                    ),
                    l,
                ),
                e.write(
                    add(var(to), var(byte)),
                    e.decoded(add(var(from), var(byte))),
                )?,
                assign(cursor, add(var(cursor), int(1)), l),
            ],
            line: l,
        },
    ])
}

fn set(e: &mut Emit<'_, '_>, method: &Method, base: u16) -> Result<Vec<S>, ParseError> {
    let l = e.line;
    let offset = e.integer()?;
    let src = e.boxed()?;
    let src_base = e.integer()?;
    let len = e.integer()?;
    let i = e.integer()?;
    let dst_i = e.integer()?;
    // The shared writer mutates a Number scratch (not a Float slot), notably
    // when Uint8Clamped rounding reuses its Number-typed floor.
    let element_number = e.temp(T::Number, E::Float(0.0))?;
    let key = e.boxed()?;
    let raw = e.boxed()?;
    let mut body = method.to_number(
        e,
        method.arg(1),
        vec![assign(method.n, trunc_or_zero(method.n), l)],
    )?;
    let invalid = method.error(e, "invalid array length", true)?;
    let bad_source = method.error(e, "not an object", false)?;
    let mut object = vec![
        assign(base, e.base(var(method.receiver)), l),
        assign(src_base, e.base(var(src)), l),
    ];
    let dst = checked_window(e, base, &mut object)?;
    object.push(assign(
        offset,
        call(
            e.decode,
            vec![num(choose(
                lt(num(var(dst.length)), var(method.n)),
                num(add(var(dst.length), int(1))),
                var(method.n),
            ))],
        ),
        l,
    ));
    let fits_source = |len| fits(var(offset), var(len), var(dst.length));

    let mut write = vec![
        assign(base, e.base(var(method.receiver)), l),
        assign(dst_i, add(var(offset), var(i)), l),
    ];
    write_element(e, base, dst_i, element_number, &mut write)?;
    let mut typed = Vec::new();
    let src_window = checked_window(e, src_base, &mut typed)?;
    let mut cross = Vec::new();
    let element = read_element(e, src_base, i, &mut cross)?;
    cross.push(assign(element_number, element, l));
    cross.extend(write.clone());
    let forward = |step| S::While {
        condition: and(var(method.active), lt(var(i), var(len))),
        body: step,
        line: l,
    };
    cross.push(assign(i, add(var(i), int(1)), l));
    let same = copy_bytes(e, &dst, &src_window, offset)?;
    typed.push(assign(len, var(src_window.length), l));
    typed.push(branch(
        fits_source(len),
        vec![branch(
            eq(var(dst.element), var(src_window.element)),
            same,
            vec![assign(i, int(0), l), forward(cross)],
            l,
        )],
        invalid.clone(),
        l,
    ));

    let mut generic = method.length(e, var(src), len)?;
    let mut step = vec![assign(key, method.index_key(e, i), l)];
    let mut got = vec![
        assign(raw, var(method.primitive), l),
        e.root(var(method.frame), int(1), var(raw))?,
    ];
    let mut numeric_write = vec![assign(element_number, var(method.n), l)];
    numeric_write.extend(write);
    got.extend(method.to_number(e, var(raw), numeric_write)?);
    step.extend(method.get(e, var(src), key, got)?);
    step.push(assign(i, add(var(i), int(1)), l));
    generic.push(branch(
        var(method.active),
        vec![branch(
            fits_source(len),
            vec![assign(i, int(0), l), forward(step)],
            invalid.clone(),
            l,
        )],
        vec![],
        l,
    ));
    object.push(branch(
        eq(e.decoded(var(src_base)), int(layout::KIND_VIEW)),
        typed,
        generic,
        l,
    ));
    body.push(branch(
        var(method.active),
        vec![branch(
            lt(var(method.n), E::Float(0.0)),
            invalid,
            vec![
                assign(src, method.arg(0), l),
                branch(tag_is(var(src), value::OBJECT), object, bad_source, l),
            ],
            l,
        )],
        vec![],
        l,
    ));
    body.push(branch(
        var(method.active),
        vec![method.done(e, value::undefined())],
        vec![],
        l,
    ));
    Ok(body)
}

fn join(e: &mut Emit<'_, '_>, method: &Method, base: u16) -> Result<Vec<S>, ParseError> {
    let l = e.line;
    let c = e.carrier;
    let heap = e.lower.heap_ref(e.span)?.helpers;
    let to_units = e
        .lower
        .utf16_units_helper()
        .expect("TypedArray UTF-16 reader");
    let publish = e
        .lower
        .utf16_publish_helper()
        .expect("TypedArray UTF-16 publisher");
    let len = e.integer()?;
    let i = e.integer()?;
    let cursor = e.integer()?;
    let count = e.integer()?;
    let raw = e.boxed()?;
    let key = e.boxed()?;
    let answer = e.boxed()?;
    let kind = e.integer()?;
    let int_array = T::Array(Box::new(T::Int));
    let empty = || call(B::ArrayNew.call_index(), vec![]);
    let units = e.temp(int_array.clone(), empty())?;
    let sep = e.temp(int_array.clone(), empty())?;
    let part = e.temp(int_array, empty())?;
    let unit_len = |slot| call(B::Len.call_index(), vec![E::Borrow(Box::new(var(slot)))]);
    let push = |slot, unit| {
        assign(
            slot,
            call(B::ArrayPush.call_index(), vec![E::MoveVar(slot), unit]),
            l,
        )
    };
    let append = |from| {
        vec![
            assign(count, unit_len(from), l),
            assert_stmt(not(lt(int(65_536), add(unit_len(units), var(count)))), l),
            assign(cursor, int(0), l),
            S::While {
                condition: lt(var(cursor), var(count)),
                body: vec![
                    push(
                        units,
                        call(
                            B::Get.call_index(),
                            vec![E::Borrow(Box::new(var(from))), var(cursor)],
                        ),
                    ),
                    assign(cursor, add(var(cursor), int(1)), l),
                ],
                line: l,
            },
        ]
    };
    let string = |e: &mut Emit<'_, '_>, dst, mut tail: Vec<S>| {
        tail.insert(
            0,
            assign(dst, call(to_units, vec![var(c), var(method.primitive)]), l),
        );
        let mut body = vec![e.root(var(method.frame), int(1), var(raw))?];
        body.extend(method.matched_value(
            e,
            call(
                method.objects.key_string,
                vec![var(c), var(raw), var(method.frame)],
            ),
            tail,
        )?);
        Ok::<_, ParseError>(body)
    };
    let mut typed_len = Vec::new();
    let window = checked_window(e, base, &mut typed_len)?;
    typed_len.push(assign(len, var(window.length), l));
    let array_len = vec![assign(
        len,
        call(
            e.decode_u32,
            vec![call(heap.array_length, vec![var(c), var(base)])],
        ),
        l,
    )];
    let other_len = method.length(e, var(method.receiver), len)?;
    let mut body = vec![
        assign(kind, e.decoded(var(base)), l),
        branch(
            eq(var(kind), int(layout::KIND_VIEW)),
            typed_len,
            vec![branch(
                eq(var(kind), int(layout::KIND_ARRAY)),
                array_len,
                other_len,
                l,
            )],
            l,
        ),
    ];
    let separator = vec![
        assign(
            raw,
            choose(
                eq(var(method.code), int(JOIN)),
                method.arg(0),
                value::undefined(),
            ),
            l,
        ),
        branch(
            tag_is(var(raw), value::UNDEFINED),
            vec![push(sep, int(i64::from(b',')))],
            string(e, sep, vec![])?,
            l,
        ),
    ];
    body.push(branch(var(method.active), separator, vec![], l));
    let mut typed_read = vec![assign(base, e.base(var(method.receiver)), l)];
    let n = read_element(e, base, i, &mut typed_read)?;
    typed_read.push(assign(raw, value::number(n), l));
    let array_read = vec![
        assign(base, e.base(var(method.receiver)), l),
        assign(
            raw,
            call(heap.array_get, vec![var(c), var(base), var(i)]),
            l,
        ),
    ];
    let mut ordinary_read = vec![assign(key, method.index_key(e, i), l)];
    ordinary_read.extend(method.get(
        e,
        var(method.receiver),
        key,
        vec![assign(raw, var(method.primitive), l)],
    )?);
    let step = vec![
        branch(lt(int(0), var(i)), append(sep), vec![], l),
        branch(
            eq(var(kind), int(layout::KIND_VIEW)),
            typed_read,
            vec![branch(
                eq(var(kind), int(layout::KIND_ARRAY)),
                array_read,
                ordinary_read,
                l,
            )],
            l,
        ),
        branch(
            and(
                var(method.active),
                not(or(
                    tag_is(var(raw), value::UNDEFINED),
                    tag_is(var(raw), value::NULL),
                )),
            ),
            string(e, part, append(part))?,
            vec![],
            l,
        ),
        assign(i, add(var(i), int(1)), l),
    ];
    body.extend([
        assign(i, int(0), l),
        S::While {
            condition: and(var(method.active), lt(var(i), var(len))),
            body: step,
            line: l,
        },
        branch(
            var(method.active),
            vec![
                assign(
                    answer,
                    call(
                        publish,
                        vec![var(c), E::MoveVar(units), var(method.frame), int(7)],
                    ),
                    l,
                ),
                method.done(e, var(answer)),
            ],
            vec![],
            l,
        ),
    ]);
    Ok(body)
}

pub(super) fn emit(
    e: &mut Emit<'_, '_>,
    code: u16,
    receiver: u16,
    args: u16,
    out: u16,
) -> Result<Vec<S>, ParseError> {
    // Keep the added locals and native unit buffers out of the constructor/getter path.
    let mut scratch = Emit::new(e.lower, e.span, e.carrier)?;
    let e = &mut scratch;
    let method = Method::new(e, code, receiver, args, out)?;
    let l = e.line;
    let c = e.carrier;
    let base = e.integer()?;
    let heap = e.lower.heap_ref(e.span)?.helpers;
    let sub = subarray(e, &method, base)?;
    let set = set(e, &method, base)?;
    let join = join(e, &method, base)?;
    let bad_view = method.error(e, "not a TypedArray", false)?;
    let non_object = method.error(e, "not an object", false)?;
    let view_method = or(eq(var(code), int(SUBARRAY)), eq(var(code), int(SET)));
    let body = vec![
        assign(method.frame, call(heap.root_enter, vec![var(c), int(8)]), l),
        e.root(var(method.frame), int(6), var(receiver))?,
        assign(method.active, E::Bool(true), l),
        branch(
            tag_is(var(receiver), value::OBJECT),
            vec![
                assign(base, e.base(var(receiver)), l),
                branch(
                    view_method.clone(),
                    vec![branch(
                        eq(e.decoded(var(base)), int(layout::KIND_VIEW)),
                        vec![branch(eq(var(code), int(SUBARRAY)), sub, set, l)],
                        bad_view.clone(),
                        l,
                    )],
                    join,
                    l,
                ),
            ],
            vec![branch(view_method, bad_view, non_object, l)],
            l,
        ),
        e.effect(heap.root_leave, vec![var(c), var(method.frame)])?,
    ];
    Ok(scratch.finish(body))
}
