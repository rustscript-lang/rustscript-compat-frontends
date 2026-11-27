//! ES5.1 9.8.1 notation for numeric `parseInt` prefixes.
//!
//! Core `ToString` remains the shortest-digit primitive. This scanner reads
//! that ASCII, recovers `(n, k, s)`, and applies steps 6–10 only as far as
//! `parseInt` consumes. Significant digits are packed into one integer so the
//! guest function stays small enough to compile inside the existing deadline.
//! It does not emit a new formatter or choose a tie digit.

use super::*;

/// f64 shortest form has at most 17 significant digits. 18 fit in an `i64`.
const MAX_DIGITS: i64 = 18;

fn f(n: f64) -> E {
    E::Float(n)
}

fn at(text: u16, index: E) -> E {
    call(
        B::Get.call_index(),
        vec![E::Borrow(Box::new(var(text))), index],
    )
}

fn is_char(text: u16, index: E, literal: &str) -> E {
    eq(at(text, index), E::String(literal.into()))
}

/// ASCII class of one Core string unit: 0–9, 10='.', 11='e'/'E', 12='+', 13='-', 14=other.
fn classify(text: u16, index: E) -> E {
    let mut expr = int(14);
    for (literal, code) in [("+", 12), ("-", 13), (".", 10), ("e", 11), ("E", 11)] {
        expr = choose(is_char(text, index.clone(), literal), int(code), expr);
    }
    for digit in (0..10).rev() {
        expr = choose(
            is_char(text, index.clone(), &digit.to_string()),
            int(digit),
            expr,
        );
    }
    expr
}

fn imod(a: E, b: E) -> E {
    E::Mod(Box::new(a), Box::new(b))
}

fn idiv(a: E, b: E) -> E {
    E::Div(Box::new(a), Box::new(b))
}

/// `digit = packed / 10^(width-1-index) % 10`. `width` is the packed length
/// and must not be the stripped `k`.
fn load_digit(
    packed: u16,
    width: u16,
    index: E,
    digit: u16,
    place: u16,
    left: u16,
    line: u32,
) -> Vec<S> {
    vec![
        assign(place, int(1), line),
        assign(left, sub(sub(var(width), int(1)), index), line),
        S::While {
            condition: lt(int(0), var(left)),
            body: vec![
                assign(place, mul(var(place), int(10)), line),
                assign(left, sub(var(left), int(1)), line),
            ],
            line,
        },
        assign(digit, imod(idiv(var(packed), var(place)), int(10)), line),
    ]
}

fn push_digit(packed: u16, count: u16, code: u16, overflow: u16, line: u32) -> S {
    branch(
        lt(var(count), int(MAX_DIGITS)),
        vec![
            assign(packed, add(mul(var(packed), int(10)), var(code)), line),
            assign(count, add(var(count), int(1)), line),
        ],
        vec![assign(overflow, int(1), line)],
        line,
    )
}

/// Scan `ToString(actual)` and write the radix prefix into `acc`/`have`.
pub(super) fn append_prefix(
    e: &mut Emit<'_, '_>,
    x: E,
    rad: u16,
    acc: u16,
    have: u16,
) -> Result<Vec<S>, ParseError> {
    let l = e.line;
    let text = e.temp(T::String, E::String(String::new()))?;
    let len = e.temp(T::Int, int(0))?;
    let i = e.temp(T::Int, int(0))?;
    let code = e.temp(T::Int, int(0))?;
    let point = e.temp(T::Int, int(0))?;
    let mant = e.temp(T::Int, int(0))?;
    let leading = e.temp(T::Int, int(0))?;
    let started = e.temp(T::Int, int(0))?;
    let pending = e.temp(T::Int, int(0))?;
    let seen_point = e.temp(T::Int, int(0))?;
    let in_exp = e.temp(T::Int, int(0))?;
    let exp_neg = e.temp(T::Int, int(0))?;
    let exp = e.temp(T::Int, int(0))?;
    let packed = e.temp(T::Int, int(0))?;
    let count = e.temp(T::Int, int(0))?;
    let overflow = e.temp(T::Int, int(0))?;
    let k = e.temp(T::Int, int(0))?;
    let n = e.temp(T::Int, int(0))?;
    let stripping = e.temp(T::Int, int(0))?;
    let digit = e.temp(T::Int, int(0))?;
    let place = e.temp(T::Int, int(0))?;
    let left = e.temp(T::Int, int(0))?;
    let j = e.temp(T::Int, int(0))?;
    let stopped = e.temp(T::Int, int(0))?;
    let zeros = e.temp(T::Int, int(0))?;
    let z = e.temp(T::Int, int(0))?;

    let mut body = vec![
        assign(text, call(B::ToString.call_index(), vec![x]), l),
        assign(
            len,
            call(B::Len.call_index(), vec![E::Borrow(Box::new(var(text)))]),
            l,
        ),
        assign(i, int(0), l),
    ];
    body.push(branch(
        lt(int(0), var(len)),
        vec![branch(
            is_char(text, int(0), "-"),
            vec![assign(i, int(1), l)],
            vec![],
            l,
        )],
        vec![],
        l,
    ));
    let exp_step = branch(
        lt(var(code), int(10)),
        vec![assign(exp, add(mul(var(exp), int(10)), var(code)), l)],
        vec![branch(
            eq(var(code), int(13)),
            vec![assign(exp_neg, int(1), l)],
            vec![],
            l,
        )],
        l,
    );
    let digit_step = vec![
        assign(mant, add(var(mant), int(1)), l),
        branch(
            eq(var(code), int(0)),
            vec![branch(
                eq(var(started), int(0)),
                vec![assign(leading, add(var(leading), int(1)), l)],
                vec![assign(pending, add(var(pending), int(1)), l)],
                l,
            )],
            vec![
                assign(started, int(1), l),
                S::While {
                    condition: lt(int(0), var(pending)),
                    body: vec![
                        branch(
                            lt(var(count), int(MAX_DIGITS)),
                            vec![
                                assign(packed, mul(var(packed), int(10)), l),
                                assign(count, add(var(count), int(1)), l),
                            ],
                            vec![assign(overflow, int(1), l)],
                            l,
                        ),
                        assign(pending, sub(var(pending), int(1)), l),
                    ],
                    line: l,
                },
                push_digit(packed, count, code, overflow, l),
            ],
            l,
        ),
    ];
    let mant_step = branch(
        eq(var(code), int(11)),
        vec![assign(in_exp, int(1), l)],
        vec![branch(
            eq(var(code), int(10)),
            vec![
                assign(point, var(mant), l),
                assign(seen_point, int(1), l),
                assign(mant, add(var(mant), int(1)), l),
            ],
            vec![branch(lt(var(code), int(10)), digit_step, vec![], l)],
            l,
        )],
        l,
    );
    body.push(S::While {
        condition: lt(var(i), var(len)),
        body: vec![
            assign(code, classify(text, var(i)), l),
            branch(eq(var(in_exp), int(1)), vec![exp_step], vec![mant_step], l),
            assign(i, add(var(i), int(1)), l),
        ],
        line: l,
    });
    body.push(assign(
        point,
        choose(eq(var(seen_point), int(0)), var(mant), var(point)),
        l,
    ));
    body.push(assign(
        exp,
        choose(eq(var(exp_neg), int(1)), sub(int(0), var(exp)), var(exp)),
        l,
    ));
    body.push(assign(k, var(count), l));
    body.push(assign(stripping, int(1), l));
    let mut strip_load = load_digit(packed, count, sub(var(k), int(1)), digit, place, left, l);
    strip_load.push(branch(
        eq(var(digit), int(0)),
        vec![assign(k, sub(var(k), int(1)), l)],
        vec![assign(stripping, int(0), l)],
        l,
    ));
    body.push(S::While {
        condition: eq(var(stripping), int(1)),
        body: vec![branch(
            lt(int(0), var(k)),
            strip_load,
            vec![assign(stripping, int(0), l)],
            l,
        )],
        line: l,
    });
    body.push(assign(n, add(sub(var(point), var(leading)), var(exp)), l));

    let mut sci = load_digit(packed, count, int(0), digit, place, left, l);
    sci.push(branch(
        lt(num(var(digit)), var(rad)),
        vec![
            assign(acc, num(var(digit)), l),
            assign(have, E::Bool(true), l),
            branch(
                and(eq(var(k), int(1)), lt(f(14.0), var(rad))),
                vec![assign(acc, add(mul(var(acc), var(rad)), f(14.0)), l)],
                vec![],
                l,
            ),
        ],
        vec![],
        l,
    ));
    let mut take = load_digit(packed, count, var(j), digit, place, left, l);
    take.push(branch(
        lt(num(var(digit)), var(rad)),
        vec![
            assign(acc, add(mul(var(acc), var(rad)), num(var(digit))), l),
            assign(have, E::Bool(true), l),
            assign(j, add(var(j), int(1)), l),
        ],
        vec![assign(stopped, int(1), l)],
        l,
    ));
    let mut step6 = vec![
        assign(j, int(0), l),
        assign(stopped, int(0), l),
        assign(acc, f(0.0), l),
    ];
    step6.push(S::While {
        condition: eq(var(stopped), int(0)),
        body: vec![branch(
            lt(var(j), var(k)),
            take.clone(),
            vec![assign(stopped, int(1), l)],
            l,
        )],
        line: l,
    });
    step6.push(assign(zeros, sub(var(n), var(k)), l));
    step6.push(assign(z, int(0), l));
    step6.push(assign(
        stopped,
        choose(eq(var(j), var(k)), int(0), int(1)),
        l,
    ));
    step6.push(S::While {
        condition: eq(var(stopped), int(0)),
        body: vec![branch(
            lt(var(z), var(zeros)),
            vec![
                assign(acc, mul(var(acc), var(rad)), l),
                assign(z, add(var(z), int(1)), l),
            ],
            vec![assign(stopped, int(1), l)],
            l,
        )],
        line: l,
    });
    let mut step7 = vec![
        assign(j, int(0), l),
        assign(stopped, int(0), l),
        assign(acc, f(0.0), l),
    ];
    step7.push(S::While {
        condition: eq(var(stopped), int(0)),
        body: vec![branch(
            lt(var(j), var(n)),
            take,
            vec![assign(stopped, int(1), l)],
            l,
        )],
        line: l,
    });
    let placed = branch(
        not(lt(int(0), var(n))),
        vec![assign(acc, f(0.0), l), assign(have, E::Bool(true), l)],
        vec![branch(not(lt(var(n), var(k))), step6, step7, l)],
        l,
    );
    let scientific = or(lt(int(21), var(n)), not(lt(int(-6), var(n))));
    body.push(branch(
        eq(var(overflow), int(1)),
        vec![],
        vec![branch(
            eq(var(k), int(0)),
            vec![assign(acc, f(0.0), l), assign(have, E::Bool(true), l)],
            vec![branch(scientific, sci, vec![placed], l)],
            l,
        )],
        l,
    ));
    Ok(body)
}
