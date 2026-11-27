//! Guest UTF16 numeric-prefix scanner. Unicode 3.0 Zs plus ES5 explicit whitespace.
use super::*;
fn f(n: f64) -> E {
    E::Float(n)
}
fn ws(c: E) -> E {
    [
        9, 10, 11, 12, 13, 32, 160, 5760, 8192, 8193, 8194, 8195, 8196, 8197, 8198, 8199, 8200,
        8201, 8202, 8203, 8232, 8233, 8239, 12288, 65279,
    ]
    .into_iter()
    .map(|n| eq(c.clone(), int(n)))
    .reduce(or)
    .expect("whitespace")
}
/// True iff `coeff * 10 + digit` fits in i64, tested before the multiply.
/// Digit 0..=7 still admits the i64::MAX prefix; digit 8..=9 does not.
fn i64_coeff_accepts(coeff: u16, digit: E, identity: u16) -> E {
    not(lt(
        div(sub(int(i64::MAX), digit), int(10)),
        call(identity, vec![var(coeff)]),
    ))
}
fn digit(c: E) -> E {
    choose(
        and(not(lt(c.clone(), int(48))), lt(c.clone(), int(58))),
        sub(c.clone(), int(48)),
        choose(
            and(not(lt(c.clone(), int(65))), lt(c.clone(), int(91))),
            sub(c.clone(), int(55)),
            choose(
                and(not(lt(c.clone(), int(97))), lt(c.clone(), int(123))),
                sub(c, int(87)),
                int(99),
            ),
        ),
    )
}
fn radix(r: E) -> E {
    let integral = builtin(B::MathTrunc, vec![r.clone()]);
    let modulo = sub(
        integral.clone(),
        mul(
            builtin(B::MathFloor, vec![div(integral, f(4294967296.0))]),
            f(4294967296.0),
        ),
    );
    choose(builtin(B::MathIsFinite, vec![r]), modulo, f(0.0))
}
/// Primitive ToString tokens have no observable conversion hooks. Scan their
/// exact ASCII digits in the selected radix, without allocating a guest string.
pub(super) fn parse_token(e: &mut Emit<'_, '_>, primitive: E, r: E) -> Result<E, ParseError> {
    fn suffix(units: &[u8], rad: E, acc: E) -> E {
        let Some((&unit, rest)) = units.split_first() else {
            return acc;
        };
        let digit = f(f64::from(unit - b'a' + 10));
        choose(
            lt(digit.clone(), rad.clone()),
            suffix(rest, rad.clone(), add(mul(acc.clone(), rad), digit)),
            acc,
        )
    }
    fn token(text: &str, rad: E) -> E {
        let (&first, rest) = text.as_bytes().split_first().expect("primitive token");
        let digit = f(f64::from(first - b'a' + 10));
        choose(
            lt(digit.clone(), rad.clone()),
            suffix(rest, rad, digit),
            nan(),
        )
    }
    let rad = e.temp(T::Float, f(0.0))?;
    let body = vec![
        assign(rad, radix(r), e.line),
        branch(
            eq(var(rad), f(0.0)),
            vec![assign(rad, f(10.0), e.line)],
            vec![],
            e.line,
        ),
    ];
    let parsed = choose(
        tag_is(primitive.clone(), value::BOOLEAN),
        choose(
            field(primitive.clone(), "flag"),
            token("true", var(rad)),
            token("false", var(rad)),
        ),
        choose(
            tag_is(primitive.clone(), value::NULL),
            token("null", var(rad)),
            choose(
                tag_is(primitive, value::UNDEFINED),
                token("undefined", var(rad)),
                nan(),
            ),
        ),
    );
    Ok(E::Block {
        stmts: body,
        expr: Box::new(choose(
            and(not(lt(var(rad), f(2.0))), not(lt(f(36.0), var(rad)))),
            parsed,
            nan(),
        )),
    })
}
/// Number ToString prefix scanning, without exposing Number formatting APIs.
pub(super) fn parse_numeric(e: &mut Emit<'_, '_>, x: E, r: E) -> Result<E, ParseError> {
    let l = e.line;
    let rad = e.temp(T::Float, f(0.0))?;
    let acc = e.temp(T::Float, f(0.0))?;
    let active = e.boolean()?;
    let have = e.boolean()?;
    let negsign = lt(x.clone(), f(0.0));
    let mut body = vec![
        assign(rad, radix(r), l),
        branch(
            eq(var(rad), f(0.0)),
            vec![assign(rad, f(10.0), l)],
            vec![],
            l,
        ),
        assign(
            active,
            and(not(lt(var(rad), f(2.0))), not(lt(f(36.0), var(rad)))),
            l,
        ),
        assign(have, E::Bool(false), l),
        assign(acc, f(0.0), l),
    ];
    let finite = super::number_to_string::append_prefix(e, x.clone(), rad, acc, have)?;
    let mut special = Vec::new();
    for (isnan, digits) in [
        (true, vec![23, 10, 23]),
        (false, vec![18, 23, 15, 18, 23, 18, 29, 34]),
    ] {
        let mut steps = Vec::new();
        for digit in digits {
            steps.push(branch(
                var(active),
                vec![branch(
                    lt(f(digit as f64), var(rad)),
                    vec![
                        assign(acc, add(mul(var(acc), var(rad)), f(digit as f64)), l),
                        assign(have, E::Bool(true), l),
                    ],
                    vec![assign(active, E::Bool(false), l)],
                    l,
                )],
                vec![],
                l,
            ));
        }
        special.push(branch(
            if isnan {
                not(eq(x.clone(), x.clone()))
            } else {
                eq(x.clone(), x.clone())
            },
            steps,
            vec![],
            l,
        ));
    }
    body.push(branch(
        var(active),
        vec![branch(
            builtin(B::MathIsFinite, vec![x]),
            finite,
            special,
            l,
        )],
        vec![],
        l,
    ));
    Ok(E::Block {
        stmts: body,
        expr: Box::new(choose(
            var(have),
            choose(negsign, neg(var(acc)), var(acc)),
            nan(),
        )),
    })
}
#[allow(clippy::too_many_arguments)]
pub(super) fn append(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    index: u16,
    round: u16,
    name: &str,
) -> Result<(), ParseError> {
    let c = lower.slot(span)?;
    let input = lower.slot(span)?;
    let mode = lower.slot(span)?;
    let r = lower.slot(span)?;
    let identity = lower.helper_int_identity.expect("Int boundary");
    let mut e = Emit::new(lower, span, c)?;
    let l = e.line;
    let base = e.integer()?;
    let len = e.integer()?;
    let p = e.integer()?;
    let unit = e.integer()?;
    let d = e.integer()?;
    let start = e.integer()?;
    let sig_start = e.integer()?;
    let sig_end = e.integer()?;
    let digit_start = e.integer()?;
    let digits = e.integer()?;
    let exponent_start = e.integer()?;
    let exponent_digits = e.integer()?;
    let signed = e.boolean()?;
    let negative = e.boolean()?;
    let exp_negative = e.boolean()?;
    let valid = e.boolean()?;
    let scanning = e.boolean()?;
    let hexadecimal = e.boolean()?;
    let n = e.temp(T::Float, f(0.0))?;
    let scale = e.temp(T::Float, f(0.0))?;
    let exp = e.temp(T::Float, f(0.0))?;
    // Exact decimal coefficient while it fits in a signed Int. `coeff_ok` is
    // cleared when another digit would wrap; the fallback float scan stays
    // only for that overflow, which the general converter replaces.
    let coeff = e.integer()?;
    let coeff_ok = e.boolean()?;
    let scale_i = e.integer()?;
    let exp_i = e.integer()?;
    let rad_i = e.integer()?;
    let fast_done = e.boolean()?;
    let rad_acc = e.temp(T::Float, f(0.0))?;
    let product = e.integer()?;
    let step = e.integer()?;
    let pow10 = e.temp(T::Float, f(1.0))?;
    let rad = e.temp(T::Float, radix(var(r)))?;
    let out = e.temp(T::Float, nan())?;
    let inf = e.boolean()?;
    let read = |p: E| e.decoded(add(add(call(identity, vec![var(base)]), int(3)), p));
    let mut body = vec![
        assign(coeff, int(0), l),
        assign(coeff_ok, E::Bool(true), l),
        assign(scale_i, int(0), l),
        assign(exp_i, int(0), l),
        assign(rad_i, int(0), l),
        assign(fast_done, E::Bool(false), l),
        assert_stmt(tag_is(var(input), value::STRING), l),
        assign(base, e.base(var(input)), l),
        assert_stmt(
            eq(
                e.decoded(call(
                    e.lower.helper_int_identity.expect("Int boundary"),
                    vec![call(identity, vec![var(base)])],
                )),
                int(layout::KIND_STRING),
            ),
            l,
        ),
        assign(
            len,
            e.decoded(add(call(identity, vec![var(base)]), int(2))),
            l,
        ),
        S::While {
            condition: and(
                lt(call(identity, vec![var(p)]), call(identity, vec![var(len)])),
                ws(read(call(identity, vec![var(p)]))),
            ),
            body: vec![assign(p, add(call(identity, vec![var(p)]), int(1)), l)],
            line: l,
        },
        assign(start, call(identity, vec![var(p)]), l),
        assign(
            unit,
            choose(
                lt(call(identity, vec![var(p)]), call(identity, vec![var(len)])),
                read(call(identity, vec![var(p)])),
                int(0),
            ),
            l,
        ),
        assign(
            signed,
            or(
                eq(call(identity, vec![var(unit)]), int(43)),
                eq(call(identity, vec![var(unit)]), int(45)),
            ),
            l,
        ),
        assign(negative, eq(call(identity, vec![var(unit)]), int(45)), l),
        branch(
            var(signed),
            vec![assign(p, add(call(identity, vec![var(p)]), int(1)), l)],
            vec![],
            l,
        ),
        assign(sig_start, call(identity, vec![var(p)]), l),
    ];
    // Infinity is a decimal token only. Integer parsing instead treats its letters as digits.
    let mut infinity = E::Bool(true);
    for (i, u) in "Infinity".encode_utf16().enumerate() {
        infinity = and(
            infinity,
            and(
                lt(
                    add(call(identity, vec![var(p)]), int(i as i64)),
                    call(identity, vec![var(len)]),
                ),
                eq(
                    read(add(call(identity, vec![var(p)]), int(i as i64))),
                    int(i64::from(u)),
                ),
            ),
        );
    }
    body.push(assign(
        inf,
        and(not(eq(call(identity, vec![var(mode)]), int(1))), infinity),
        l,
    ));
    body.push(branch(
        var(inf),
        vec![
            assign(n, f(f64::INFINITY), l),
            assign(p, add(call(identity, vec![var(p)]), int(8)), l),
            assign(valid, E::Bool(true), l),
        ],
        vec![],
        l,
    ));
    let mut parse = vec![
        assign(valid, E::Bool(true), l),
        assign(hexadecimal, E::Bool(false), l),
    ];
    let prefix = and(
        lt(
            add(call(identity, vec![var(p)]), int(1)),
            call(identity, vec![var(len)]),
        ),
        and(
            eq(read(call(identity, vec![var(p)])), int(48)),
            or(
                eq(read(add(call(identity, vec![var(p)]), int(1))), int(120)),
                eq(read(add(call(identity, vec![var(p)]), int(1))), int(88)),
            ),
        ),
    );
    parse.push(branch(
        eq(call(identity, vec![var(mode)]), int(1)),
        vec![
            assign(
                valid,
                or(
                    eq(var(rad), f(0.0)),
                    and(not(lt(var(rad), f(2.0))), not(lt(f(36.0), var(rad)))),
                ),
                l,
            ),
            branch(
                and(
                    prefix.clone(),
                    or(eq(var(rad), f(0.0)), eq(var(rad), f(16.0))),
                ),
                vec![
                    assign(rad, f(16.0), l),
                    assign(p, add(call(identity, vec![var(p)]), int(2)), l),
                ],
                vec![],
                l,
            ),
            branch(
                eq(var(rad), f(0.0)),
                vec![assign(rad, f(10.0), l)],
                vec![],
                l,
            ),
        ],
        vec![
            assign(
                hexadecimal,
                and(
                    eq(call(identity, vec![var(mode)]), int(0)),
                    and(not(var(signed)), prefix),
                ),
                l,
            ),
            branch(
                var(hexadecimal),
                vec![
                    assign(rad, f(16.0), l),
                    assign(p, add(call(identity, vec![var(p)]), int(2)), l),
                ],
                vec![assign(rad, f(10.0), l)],
                l,
            ),
        ],
        l,
    ));
    let mut intscan = vec![
        assign(digit_start, call(identity, vec![var(p)]), l),
        assign(scanning, var(valid), l),
        assign(rad_i, int(0), l),
        assign(rad_acc, f(0.0), l),
        S::While {
            condition: and(
                lt(var(rad_acc), var(rad)),
                lt(call(identity, vec![var(rad_i)]), int(36)),
            ),
            body: vec![
                assign(rad_i, add(call(identity, vec![var(rad_i)]), int(1)), l),
                assign(rad_acc, add(var(rad_acc), f(1.0)), l),
            ],
            line: l,
        },
    ];
    intscan.push(S::While {
        condition: and(
            var(scanning),
            lt(call(identity, vec![var(p)]), call(identity, vec![var(len)])),
        ),
        body: vec![
            assign(d, digit(read(call(identity, vec![var(p)]))), l),
            branch(
                lt(num(call(identity, vec![var(d)])), var(rad)),
                vec![
                    assign(
                        n,
                        add(mul(var(n), var(rad)), num(call(identity, vec![var(d)]))),
                        l,
                    ),
                    branch(
                        and(var(coeff_ok), lt(int(1), call(identity, vec![var(rad_i)]))),
                        vec![branch(
                            not(lt(
                                div(
                                    sub(
                                        int(i64::MAX),
                                        sub(call(identity, vec![var(rad_i)]), int(1)),
                                    ),
                                    call(identity, vec![var(rad_i)]),
                                ),
                                call(identity, vec![var(coeff)]),
                            )),
                            vec![assign(
                                coeff,
                                add(
                                    mul(
                                        call(identity, vec![var(coeff)]),
                                        call(identity, vec![var(rad_i)]),
                                    ),
                                    call(identity, vec![var(d)]),
                                ),
                                l,
                            )],
                            vec![assign(coeff_ok, E::Bool(false), l)],
                            l,
                        )],
                        vec![assign(coeff_ok, E::Bool(false), l)],
                        l,
                    ),
                    assign(digits, add(call(identity, vec![var(digits)]), int(1)), l),
                    assign(p, add(call(identity, vec![var(p)]), int(1)), l),
                ],
                vec![assign(scanning, E::Bool(false), l)],
                l,
            ),
        ],
        line: l,
    });
    intscan.push(assign(
        valid,
        and(var(valid), lt(int(0), call(identity, vec![var(digits)]))),
        l,
    ));
    // An integer that fits in i64 is an exact binary64 integer or a correctly
    // rounded one: Core Int-to-Float is round-ties-even.
    intscan.push(branch(
        and(var(valid), var(coeff_ok)),
        vec![
            assign(n, num(call(identity, vec![var(coeff)])), l),
            assign(fast_done, E::Bool(true), l),
        ],
        vec![],
        l,
    ));
    // Nineteen digits fit in i64. A longer mandatory-radix integer must not
    // keep the binary64 `n * radix + digit` accumulation: that is not
    // correctly rounded. Optional radices stay on that path.
    let exact_radix = or(
        eq(call(identity, vec![var(rad_i)]), int(2)),
        or(
            eq(call(identity, vec![var(rad_i)]), int(4)),
            or(
                eq(call(identity, vec![var(rad_i)]), int(8)),
                or(
                    eq(call(identity, vec![var(rad_i)]), int(10)),
                    or(
                        eq(call(identity, vec![var(rad_i)]), int(16)),
                        eq(call(identity, vec![var(rad_i)]), int(32)),
                    ),
                ),
            ),
        ),
    );
    intscan.push(branch(
        and(var(valid), not(var(coeff_ok))),
        vec![branch(
            exact_radix,
            vec![
                assign(
                    n,
                    call(
                        round,
                        vec![
                            var(c),
                            int(0),
                            int(0),
                            int(1),
                            call(identity, vec![var(base)]),
                            call(identity, vec![var(digit_start)]),
                            call(identity, vec![var(p)]),
                            call(identity, vec![var(rad_i)]),
                        ],
                    ),
                    l,
                ),
                assign(fast_done, E::Bool(true), l),
            ],
            vec![],
            l,
        )],
        vec![],
        l,
    ));
    let mut dec = vec![assign(scanning, E::Bool(true), l)];
    let decimal_digit = |u: E| and(not(lt(u.clone(), int(48))), lt(u, int(58)));
    dec.push(S::While {
        condition: and(
            lt(call(identity, vec![var(p)]), call(identity, vec![var(len)])),
            decimal_digit(read(call(identity, vec![var(p)]))),
        ),
        body: vec![
            assign(d, sub(read(call(identity, vec![var(p)])), int(48)), l),
            assign(
                n,
                add(mul(var(n), f(10.0)), num(call(identity, vec![var(d)]))),
                l,
            ),
            branch(
                var(coeff_ok),
                vec![branch(
                    i64_coeff_accepts(coeff, call(identity, vec![var(d)]), identity),
                    vec![assign(
                        coeff,
                        add(
                            mul(call(identity, vec![var(coeff)]), int(10)),
                            call(identity, vec![var(d)]),
                        ),
                        l,
                    )],
                    vec![assign(coeff_ok, E::Bool(false), l)],
                    l,
                )],
                vec![],
                l,
            ),
            assign(digits, add(call(identity, vec![var(digits)]), int(1)), l),
            assign(p, add(call(identity, vec![var(p)]), int(1)), l),
        ],
        line: l,
    });
    dec.push(branch(
        and(
            lt(call(identity, vec![var(p)]), call(identity, vec![var(len)])),
            eq(read(call(identity, vec![var(p)])), int(46)),
        ),
        vec![
            assign(p, add(call(identity, vec![var(p)]), int(1)), l),
            S::While {
                condition: and(
                    lt(call(identity, vec![var(p)]), call(identity, vec![var(len)])),
                    decimal_digit(read(call(identity, vec![var(p)]))),
                ),
                body: vec![
                    assign(d, sub(read(call(identity, vec![var(p)])), int(48)), l),
                    assign(
                        n,
                        add(mul(var(n), f(10.0)), num(call(identity, vec![var(d)]))),
                        l,
                    ),
                    assign(scale, sub(var(scale), f(1.0)), l),
                    assign(scale_i, sub(call(identity, vec![var(scale_i)]), int(1)), l),
                    branch(
                        var(coeff_ok),
                        vec![branch(
                            i64_coeff_accepts(coeff, call(identity, vec![var(d)]), identity),
                            vec![assign(
                                coeff,
                                add(
                                    mul(call(identity, vec![var(coeff)]), int(10)),
                                    call(identity, vec![var(d)]),
                                ),
                                l,
                            )],
                            vec![assign(coeff_ok, E::Bool(false), l)],
                            l,
                        )],
                        vec![],
                        l,
                    ),
                    assign(digits, add(call(identity, vec![var(digits)]), int(1)), l),
                    assign(p, add(call(identity, vec![var(p)]), int(1)), l),
                ],
                line: l,
            },
        ],
        vec![],
        l,
    ));
    dec.push(assign(
        valid,
        lt(int(0), call(identity, vec![var(digits)])),
        l,
    ));
    let mut exponent = vec![
        assign(exponent_start, call(identity, vec![var(p)]), l),
        assign(p, add(call(identity, vec![var(p)]), int(1)), l),
        assign(
            unit,
            choose(
                lt(call(identity, vec![var(p)]), call(identity, vec![var(len)])),
                read(call(identity, vec![var(p)])),
                int(0),
            ),
            l,
        ),
        assign(
            exp_negative,
            eq(call(identity, vec![var(unit)]), int(45)),
            l,
        ),
        branch(
            or(
                eq(call(identity, vec![var(unit)]), int(43)),
                eq(call(identity, vec![var(unit)]), int(45)),
            ),
            vec![assign(p, add(call(identity, vec![var(p)]), int(1)), l)],
            vec![],
            l,
        ),
        S::While {
            condition: and(
                lt(call(identity, vec![var(p)]), call(identity, vec![var(len)])),
                decimal_digit(read(call(identity, vec![var(p)]))),
            ),
            body: vec![
                assign(
                    exp,
                    choose(
                        lt(var(exp), f(100000.0)),
                        add(
                            mul(var(exp), f(10.0)),
                            num(sub(read(call(identity, vec![var(p)])), int(48))),
                        ),
                        var(exp),
                    ),
                    l,
                ),
                assign(
                    exp_i,
                    choose(
                        lt(call(identity, vec![var(exp_i)]), int(100_000)),
                        add(
                            mul(call(identity, vec![var(exp_i)]), int(10)),
                            sub(read(call(identity, vec![var(p)])), int(48)),
                        ),
                        call(identity, vec![var(exp_i)]),
                    ),
                    l,
                ),
                assign(
                    exponent_digits,
                    add(call(identity, vec![var(exponent_digits)]), int(1)),
                    l,
                ),
                assign(p, add(call(identity, vec![var(p)]), int(1)), l),
            ],
            line: l,
        },
    ];
    exponent.push(branch(
        eq(call(identity, vec![var(exponent_digits)]), int(0)),
        vec![assign(p, call(identity, vec![var(exponent_start)]), l)],
        vec![
            assign(
                scale,
                add(
                    var(scale),
                    choose(var(exp_negative), neg(var(exp)), var(exp)),
                ),
                l,
            ),
            assign(
                scale_i,
                add(
                    call(identity, vec![var(scale_i)]),
                    choose(
                        var(exp_negative),
                        E::Neg(Box::new(call(identity, vec![var(exp_i)]))),
                        call(identity, vec![var(exp_i)]),
                    ),
                ),
                l,
            ),
        ],
        l,
    ));
    dec.push(assign(sig_end, call(identity, vec![var(p)]), l));
    dec.push(branch(
        and(
            var(valid),
            and(
                lt(call(identity, vec![var(p)]), call(identity, vec![var(len)])),
                or(
                    eq(read(call(identity, vec![var(p)])), int(101)),
                    eq(read(call(identity, vec![var(p)])), int(69)),
                ),
            ),
        ),
        exponent,
        vec![],
        l,
    ));
    // Exact paths: a coefficient that fits in i64 with a scale whose power of ten
    // is an exact binary64 (or an exact integer product). Ties use Core RNE.
    dec.push(branch(
        and(var(valid), var(coeff_ok)),
        vec![branch(
            eq(call(identity, vec![var(scale_i)]), int(0)),
            vec![
                assign(n, num(call(identity, vec![var(coeff)])), l),
                assign(fast_done, E::Bool(true), l),
            ],
            vec![branch(
                lt(int(0), call(identity, vec![var(scale_i)])),
                vec![
                    assign(product, call(identity, vec![var(coeff)]), l),
                    assign(step, int(0), l),
                    S::While {
                        condition: and(
                            lt(
                                call(identity, vec![var(step)]),
                                call(identity, vec![var(scale_i)]),
                            ),
                            not(lt(
                                int(922_337_203_685_477_580),
                                call(identity, vec![var(product)]),
                            )),
                        ),
                        body: vec![
                            assign(product, mul(call(identity, vec![var(product)]), int(10)), l),
                            assign(step, add(call(identity, vec![var(step)]), int(1)), l),
                        ],
                        line: l,
                    },
                    branch(
                        eq(
                            call(identity, vec![var(step)]),
                            call(identity, vec![var(scale_i)]),
                        ),
                        vec![
                            assign(n, num(call(identity, vec![var(product)])), l),
                            assign(fast_done, E::Bool(true), l),
                        ],
                        vec![branch(
                            and(
                                not(lt(int(22), call(identity, vec![var(scale_i)]))),
                                not(lt(
                                    int(9_007_199_254_740_992),
                                    call(identity, vec![var(coeff)]),
                                )),
                            ),
                            vec![
                                assign(pow10, f(1.0), l),
                                assign(step, int(0), l),
                                S::While {
                                    condition: lt(
                                        call(identity, vec![var(step)]),
                                        call(identity, vec![var(scale_i)]),
                                    ),
                                    body: vec![
                                        assign(pow10, mul(var(pow10), f(10.0)), l),
                                        assign(
                                            step,
                                            add(call(identity, vec![var(step)]), int(1)),
                                            l,
                                        ),
                                    ],
                                    line: l,
                                },
                                assign(
                                    n,
                                    mul(num(call(identity, vec![var(coeff)])), var(pow10)),
                                    l,
                                ),
                                assign(fast_done, E::Bool(true), l),
                            ],
                            vec![],
                            l,
                        )],
                        l,
                    ),
                ],
                vec![branch(
                    and(
                        not(lt(call(identity, vec![var(scale_i)]), int(-22))),
                        not(lt(
                            int(9_007_199_254_740_992),
                            call(identity, vec![var(coeff)]),
                        )),
                    ),
                    vec![
                        assign(pow10, f(1.0), l),
                        assign(step, int(0), l),
                        S::While {
                            condition: lt(
                                call(identity, vec![var(step)]),
                                E::Neg(Box::new(call(identity, vec![var(scale_i)]))),
                            ),
                            body: vec![
                                assign(pow10, mul(var(pow10), f(10.0)), l),
                                assign(step, add(call(identity, vec![var(step)]), int(1)), l),
                            ],
                            line: l,
                        },
                        assign(n, div(num(call(identity, vec![var(coeff)])), var(pow10)), l),
                        assign(fast_done, E::Bool(true), l),
                    ],
                    vec![],
                    l,
                )],
                l,
            )],
            l,
        )],
        vec![],
        l,
    ));
    // Split extreme exponents so a representable subnormal is not rounded to zero before scaling.
    // Skipped when an exact path already wrote `n`.
    dec.push(branch(
        not(var(fast_done)),
        vec![branch(
            and(var(valid), var(coeff_ok)),
            vec![assign(
                n,
                call(
                    round,
                    vec![
                        var(c),
                        call(identity, vec![var(coeff)]),
                        call(identity, vec![var(scale_i)]),
                        int(0),
                        int(0),
                        int(0),
                        int(0),
                        int(10),
                    ],
                ),
                l,
            )],
            vec![branch(
                var(valid),
                vec![assign(
                    n,
                    call(
                        round,
                        vec![
                            var(c),
                            int(0),
                            call(identity, vec![var(scale_i)]),
                            int(1),
                            call(identity, vec![var(base)]),
                            call(identity, vec![var(sig_start)]),
                            call(identity, vec![var(sig_end)]),
                            int(10),
                        ],
                    ),
                    l,
                )],
                vec![assign(
                    n,
                    choose(
                        eq(var(n), f(0.0)),
                        var(n),
                        choose(
                            lt(var(scale), f(-308.0)),
                            mul(
                                div(var(n), f(1e308)),
                                builtin(B::MathPowF, vec![f(10.0), add(var(scale), f(308.0))]),
                            ),
                            mul(var(n), builtin(B::MathPowF, vec![f(10.0), var(scale)])),
                        ),
                    ),
                    l,
                )],
                l,
            )],
            l,
        )],
        vec![],
        l,
    ));
    parse.push(branch(
        or(
            eq(call(identity, vec![var(mode)]), int(1)),
            var(hexadecimal),
        ),
        intscan,
        dec,
        l,
    ));
    body.push(branch(not(var(inf)), parse, vec![], l));
    // Number consumes the entire trimmed token; parseInt/parseFloat stop at the first invalid unit.
    body.push(branch(
        eq(call(identity, vec![var(mode)]), int(0)),
        vec![
            S::While {
                condition: and(
                    lt(call(identity, vec![var(p)]), call(identity, vec![var(len)])),
                    ws(read(call(identity, vec![var(p)]))),
                ),
                body: vec![assign(p, add(call(identity, vec![var(p)]), int(1)), l)],
                line: l,
            },
            assign(
                valid,
                and(
                    var(valid),
                    eq(call(identity, vec![var(p)]), call(identity, vec![var(len)])),
                ),
                l,
            ),
            branch(
                eq(
                    call(identity, vec![var(start)]),
                    call(identity, vec![var(len)]),
                ),
                vec![assign(valid, E::Bool(true), l), assign(n, f(0.0), l)],
                vec![],
                l,
            ),
        ],
        vec![],
        l,
    ));
    body.push(assign(
        out,
        choose(
            var(valid),
            choose(var(negative), neg(var(n)), var(n)),
            nan(),
        ),
        l,
    ));
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        name,
        index,
        vec![c, input, mode, r],
        vec![carrier_schema.clone(), value::schema(), T::Int, T::Number],
        T::Float,
        body,
        var(out),
        l,
    );
    Ok(())
}
