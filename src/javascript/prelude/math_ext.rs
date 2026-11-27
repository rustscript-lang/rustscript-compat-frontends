//! Guest numeric bodies for the public Micro Math extensions.
//! Integer conversion follows the existing bitwise ToUint32/ToInt32 kernel.
use super::super::super::lowerer::assert_stmt;
use super::super::object::*;
use vm::{BuiltinFunction as B, Expr as E, ParseError, Stmt as S, TypeSchema as T};

const MODULUS: f64 = 4_294_967_296.0;
const HALF: f64 = 65_536.0;
const SIGN_BIT: f64 = 2_147_483_648.0;
const F32_OVERFLOW: f64 = 340_282_366_920_938_463_463_374_607_431_768_211_456.0;
const F32_MIN_NORMAL: f64 = 1.175_494_350_822_287_5e-38;
const F32_MIN_SUB: f64 = 1.401_298_464_324_817e-45;

fn f(n: f64) -> E {
    E::Float(n)
}
fn raw(b: B, args: Vec<E>) -> E {
    call(b.call_index(), args)
}
fn floor_div(a: E, b: E) -> E {
    raw(B::MathFloor, vec![E::Div(Box::new(a), Box::new(b))])
}
fn positive_zero(x: E) -> E {
    choose(eq(x.clone(), f(0.0)), f(0.0), x)
}
fn signed32(x: E) -> E {
    choose(lt(x.clone(), f(SIGN_BIT)), x.clone(), sub(x, f(MODULUS)))
}

/// ES5 ToUint32, same steps as the bitwise kernel: trunc, remainder, repair sign.
pub(in crate::javascript::prelude) fn to_uint32(
    e: &mut Emit<'_, '_>,
    line: u32,
    x: u16,
) -> Result<(u16, Vec<S>), ParseError> {
    let u = e.temp(T::Float, f(0.0))?;
    let t = e.temp(T::Float, f(0.0))?;
    let r = e.temp(T::Float, f(0.0))?;
    Ok((
        u,
        vec![
            assign(u, f(0.0), line),
            branch(
                and(raw(B::MathIsFinite, vec![var(x)]), not(eq(var(x), f(0.0)))),
                vec![
                    assign(t, raw(B::MathTrunc, vec![var(x)]), line),
                    assign(r, E::Mod(Box::new(var(t)), Box::new(f(MODULUS))), line),
                    assign(
                        u,
                        positive_zero(choose(lt(var(r), f(0.0)), add(var(r), f(MODULUS)), var(r))),
                        line,
                    ),
                ],
                vec![],
                line,
            ),
        ],
    ))
}

fn split16(e: &mut Emit<'_, '_>, line: u32, u: u16) -> Result<(u16, u16, Vec<S>), ParseError> {
    let lo = e.temp(T::Float, f(0.0))?;
    let hi = e.temp(T::Float, f(0.0))?;
    Ok((
        lo,
        hi,
        vec![
            assign(lo, E::Mod(Box::new(var(u)), Box::new(f(HALF))), line),
            assign(hi, floor_div(var(u), f(HALF)), line),
        ],
    ))
}

/// Low 32 bits of the product via 16-bit partials. No full 32×32 Int multiply.
pub(super) fn imul(
    e: &mut Emit<'_, '_>,
    line: u32,
    x: u16,
    y: u16,
    out: u16,
) -> Result<Vec<S>, ParseError> {
    let (ua, mut stmts) = to_uint32(e, line, x)?;
    let (ub, more) = to_uint32(e, line, y)?;
    stmts.extend(more);
    let (alo, ahi, split_a) = split16(e, line, ua)?;
    let (blo, bhi, split_b) = split16(e, line, ub)?;
    stmts.extend(split_a);
    stmts.extend(split_b);
    let low = e.temp(T::Float, f(0.0))?;
    let cross = e.temp(T::Float, f(0.0))?;
    let wrapped = e.temp(T::Float, f(0.0))?;
    stmts.extend([
        assign(
            low,
            E::Mod(Box::new(mul(var(alo), var(blo))), Box::new(f(MODULUS))),
            line,
        ),
        assign(
            cross,
            mul(
                E::Mod(
                    Box::new(add(mul(var(alo), var(bhi)), mul(var(ahi), var(blo)))),
                    Box::new(f(HALF)),
                ),
                f(HALF),
            ),
            line,
        ),
        assign(
            wrapped,
            E::Mod(Box::new(add(var(low), var(cross))), Box::new(f(MODULUS))),
            line,
        ),
        assign(out, signed32(var(wrapped)), line),
    ]);
    Ok(stmts)
}

/// Leading zeros of ToUint32. Zero yields 32. Five exact power checks, no loop.
pub(super) fn clz32(
    e: &mut Emit<'_, '_>,
    line: u32,
    x: u16,
    out: u16,
) -> Result<Vec<S>, ParseError> {
    let (u, mut stmts) = to_uint32(e, line, x)?;
    let count = e.temp(T::Float, f(0.0))?;
    stmts.push(assign(count, f(0.0), line));
    stmts.push(branch(
        eq(var(u), f(0.0)),
        vec![assign(out, f(32.0), line)],
        {
            let mut body = Vec::new();
            for (addend, threshold, shift) in [
                (16.0, 65_536.0, 65_536.0),
                (8.0, 16_777_216.0, 256.0),
                (4.0, 268_435_456.0, 16.0),
                (2.0, 1_073_741_824.0, 4.0),
                (1.0, 2_147_483_648.0, 2.0),
            ] {
                body.push(branch(
                    lt(var(u), f(threshold)),
                    vec![
                        assign(count, add(var(count), f(addend)), line),
                        assign(u, mul(var(u), f(shift)), line),
                    ],
                    vec![],
                    line,
                ));
            }
            body.push(assign(out, var(count), line));
            body
        },
        line,
    ));
    Ok(stmts)
}

fn proven_copysign(
    e: &mut Emit<'_, '_>,
    line: u32,
    mag: u16,
    sign_src: u16,
) -> Result<E, ParseError> {
    let mag_slot = e.temp(T::Float, f(0.0))?;
    let sign_slot = e.temp(T::Float, f(0.0))?;
    let inner = e.temp(T::Float, f(0.0))?;
    let outer = e.temp(T::Float, f(0.0))?;
    let fail = E::Block {
        stmts: vec![assert_stmt(E::Bool(false), line)],
        expr: Box::new(f(0.0)),
    };
    Ok(E::Match {
        value_slot: mag_slot,
        result_slot: outer,
        value: Box::new(var(mag)),
        arms: vec![(
            vm::compiler::ir::MatchPattern::Type(vm::compiler::ir::MatchTypePattern::Float),
            E::Match {
                value_slot: sign_slot,
                result_slot: inner,
                value: Box::new(var(sign_src)),
                arms: vec![(
                    vm::compiler::ir::MatchPattern::Type(vm::compiler::ir::MatchTypePattern::Float),
                    E::Call(
                        B::MathCopySign.call_index(),
                        Vec::new(),
                        vec![var(mag_slot), var(sign_slot)],
                        None,
                        None,
                    ),
                )],
                default: Box::new(fail),
            },
        )],
        default: Box::new(E::Block {
            stmts: vec![assert_stmt(E::Bool(false), line)],
            expr: Box::new(f(0.0)),
        }),
    })
}

/// Round-ties-to-even binary32. Exponent correction is one exact power-of-two step.
pub(super) fn fround(
    e: &mut Emit<'_, '_>,
    line: u32,
    x: u16,
    out: u16,
) -> Result<Vec<S>, ParseError> {
    let absx = e.temp(T::Float, f(0.0))?;
    let step = e.temp(T::Float, f(0.0))?;
    let exp = e.temp(T::Float, f(0.0))?;
    let pow = e.temp(T::Float, f(0.0))?;
    let q = e.temp(T::Float, f(0.0))?;
    let n = e.temp(T::Float, f(0.0))?;
    let frac = e.temp(T::Float, f(0.0))?;
    let product = e.temp(T::Float, f(0.0))?;
    let inf = e.temp(T::Float, f(f64::INFINITY))?;
    let signed_inf = proven_copysign(e, line, inf, x)?;
    let signed_product = proven_copysign(e, line, product, x)?;
    let odd = eq(E::Mod(Box::new(var(n)), Box::new(f(2.0))), f(1.0));
    let tie_up = or(lt(f(0.5), var(frac)), and(eq(var(frac), f(0.5)), odd));
    Ok(vec![
        assign(out, var(x), line),
        assign(inf, f(f64::INFINITY), line),
        branch(
            or(
                not(eq(num(var(x)), num(var(x)))),
                or(not(raw(B::MathIsFinite, vec![var(x)])), eq(var(x), f(0.0))),
            ),
            vec![],
            vec![
                assign(absx, raw(B::MathAbs, vec![var(x)]), line),
                branch(
                    not(lt(var(absx), f(F32_OVERFLOW))),
                    vec![assign(out, signed_inf.clone(), line)],
                    vec![
                        branch(
                            lt(var(absx), f(F32_MIN_NORMAL)),
                            vec![assign(step, f(F32_MIN_SUB), line)],
                            vec![
                                assign(
                                    exp,
                                    raw(B::MathFloor, vec![raw(B::MathLog2, vec![var(absx)])]),
                                    line,
                                ),
                                assign(pow, raw(B::MathExp2, vec![var(exp)]), line),
                                branch(
                                    lt(var(absx), var(pow)),
                                    vec![assign(exp, sub(var(exp), f(1.0)), line)],
                                    vec![],
                                    line,
                                ),
                                branch(
                                    not(lt(var(absx), mul(f(2.0), var(pow)))),
                                    vec![assign(exp, add(var(exp), f(1.0)), line)],
                                    vec![],
                                    line,
                                ),
                                assign(step, raw(B::MathExp2, vec![sub(var(exp), f(23.0))]), line),
                            ],
                            line,
                        ),
                        assign(q, E::Div(Box::new(var(absx)), Box::new(var(step))), line),
                        assign(n, raw(B::MathFloor, vec![var(q)]), line),
                        assign(frac, sub(var(q), var(n)), line),
                        branch(
                            tie_up,
                            vec![assign(n, add(var(n), f(1.0)), line)],
                            vec![],
                            line,
                        ),
                        assign(product, mul(var(n), var(step)), line),
                        branch(
                            not(lt(var(product), f(F32_OVERFLOW))),
                            vec![assign(out, signed_inf, line)],
                            vec![assign(out, signed_product, line)],
                            line,
                        ),
                    ],
                    line,
                ),
            ],
            line,
        ),
    ])
}
