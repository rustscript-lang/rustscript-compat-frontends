//! IEEE-754 codecs for the numeric cells of the shared byte backing.
use super::{Element, divide, remainder};
use crate::javascript::prelude::object::*;
use vm::{BuiltinFunction as B, Expr as E, ParseError, Stmt as S, TypeSchema as T};

pub(super) struct Format {
    pub(super) kind: Element,
    fraction_bits: i32,
    bias: i32,
}

impl Format {
    pub(super) const ALL: [Self; 2] = [
        Self {
            kind: Element::Float32,
            fraction_bits: 23,
            bias: 127,
        },
        Self {
            kind: Element::Float64,
            fraction_bits: 52,
            bias: 1023,
        },
    ];

    // Split off the two high bytes so even binary64's low word fits exactly
    // in a Number. No operation constructs a 64-bit bit pattern as a Number.
    fn low_bytes(&self) -> i64 {
        self.kind.width() - 2
    }
    fn low_radix(&self) -> f64 {
        pow2(8 * self.low_bytes() as i32)
    }
    fn significand(&self) -> f64 {
        pow2(self.fraction_bits)
    }
    fn exponent_stride(&self) -> f64 {
        self.significand() / self.low_radix()
    }
    fn exponent_limit(&self) -> f64 {
        (2 * self.bias + 1) as f64
    }
    fn min_normal(&self) -> f64 {
        pow2(1 - self.bias)
    }
    fn min_subnormal(&self) -> f64 {
        pow2(1 - self.bias - self.fraction_bits)
    }
    fn shifts(&self) -> impl Iterator<Item = i32> {
        (0..)
            .map(|n| 1 << n)
            .take_while(|&shift| shift <= self.bias)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
    }

    pub(super) fn read(
        &self,
        e: &mut Emit<'_, '_>,
        address: u16,
        out: u16,
    ) -> Result<Vec<S>, ParseError> {
        let l = e.line;
        let cursor = e.integer()?;
        let high = float(e)?;
        let exponent = float(e)?;
        let fraction = float(e)?;
        let negative = e.boolean()?;
        let stride = f(self.exponent_stride());
        let load = |off| e.read(add(var(address), int(off)));
        let mut body = vec![
            assign(
                high,
                add(
                    load(self.low_bytes()),
                    mul(f(256.0), load(self.low_bytes() + 1)),
                ),
                l,
            ),
            assign(negative, not(lt(var(high), f(32768.0))), l),
            assign(high, remainder(var(high), f(32768.0)), l),
            assign(exponent, trunc(divide(var(high), stride.clone())), l),
            assign(fraction, remainder(var(high), stride), l),
            assign(cursor, int(self.low_bytes()), l),
            S::While {
                condition: lt(int(0), var(cursor)),
                body: vec![
                    assign(cursor, sub(var(cursor), int(1)), l),
                    assign(
                        fraction,
                        add(
                            mul(var(fraction), f(256.0)),
                            e.read(add(var(address), var(cursor))),
                        ),
                        l,
                    ),
                ],
                line: l,
            },
        ];
        let normal = vec![
            assign(
                out,
                add(f(1.0), divide(var(fraction), f(self.significand()))),
                l,
            ),
            assign(exponent, sub(var(exponent), f(self.bias as f64)), l),
            branch(
                lt(var(exponent), f(0.0)),
                self.scale(out, exponent, -1, l),
                self.scale(out, exponent, 1, l),
                l,
            ),
        ];
        body.extend([
            branch(
                eq(var(exponent), f(self.exponent_limit())),
                vec![assign(
                    out,
                    choose(eq(var(fraction), f(0.0)), f(f64::INFINITY), f(f64::NAN)),
                    l,
                )],
                vec![branch(
                    eq(var(exponent), f(0.0)),
                    vec![assign(out, mul(var(fraction), f(self.min_subnormal())), l)],
                    normal,
                    l,
                )],
                l,
            ),
            branch(
                var(negative),
                vec![assign(out, mul(f(-1.0), var(out)), l)],
                vec![],
                l,
            ),
        ]);
        Ok(body)
    }

    /// Scale by an integral exponent using exact powers of two. The normal
    /// result's magnitude bounds every intermediate in the same direction.
    fn scale(&self, out: u16, exponent: u16, sign: i32, l: u32) -> Vec<S> {
        self.shifts()
            .map(|shift| {
                let delta = f((shift * sign) as f64);
                let skip = if sign < 0 {
                    lt(delta.clone(), var(exponent))
                } else {
                    lt(var(exponent), delta.clone())
                };
                branch(
                    not(skip),
                    vec![
                        assign(out, mul(var(out), f(pow2(sign * shift))), l),
                        assign(exponent, sub(var(exponent), delta), l),
                    ],
                    vec![],
                    l,
                )
            })
            .collect()
    }

    /// Extract a normal significand and its biased exponent with exact scaling.
    fn normalize(&self, magnitude: u16, exponent: u16, sign: i32, l: u32) -> Vec<S> {
        self.shifts()
            .map(|shift| {
                let test = if sign < 0 {
                    lt(var(magnitude), f(pow2(1 - shift)))
                } else {
                    not(lt(var(magnitude), f(pow2(shift))))
                };
                branch(
                    test,
                    vec![
                        assign(magnitude, mul(var(magnitude), f(pow2(-sign * shift))), l),
                        assign(exponent, add(var(exponent), f((shift * sign) as f64)), l),
                    ],
                    vec![],
                    l,
                )
            })
            .collect()
    }

    pub(super) fn write(
        &self,
        e: &mut Emit<'_, '_>,
        address: u16,
        n: u16,
    ) -> Result<Vec<S>, ParseError> {
        let l = e.line;
        let magnitude = float(e)?;
        let exponent = float(e)?;
        let fraction = float(e)?;
        let high = float(e)?;
        let low = float(e)?;
        let negative = e.boolean()?;
        let cursor = e.integer()?;
        let significand = f(self.significand());
        let normal = vec![
            assign(exponent, f(self.bias as f64), l),
            branch(
                lt(var(magnitude), f(1.0)),
                self.normalize(magnitude, exponent, -1, l),
                self.normalize(magnitude, exponent, 1, l),
                l,
            ),
            assign(
                fraction,
                mul(sub(var(magnitude), f(1.0)), significand.clone()),
                l,
            ),
        ];
        let mut finite = vec![branch(
            lt(var(magnitude), f(self.min_normal())),
            vec![
                assign(exponent, f(0.0), l),
                assign(fraction, divide(var(magnitude), f(self.min_subnormal())), l),
            ],
            normal,
            l,
        )];
        if matches!(self.kind, Element::Float32) {
            // At most 23 fraction bits before rounding. Keep the remainder
            // exactly, then increment for above-half or half with an odd LSB.
            let truncated = float(e)?;
            let tail = float(e)?;
            finite.extend([
                assign(truncated, trunc(var(fraction)), l),
                assign(tail, sub(var(fraction), var(truncated)), l),
                assign(fraction, var(truncated), l),
                branch(
                    or(
                        lt(f(0.5), var(tail)),
                        and(
                            eq(var(tail), f(0.5)),
                            eq(remainder(var(truncated), f(2.0)), f(1.0)),
                        ),
                    ),
                    vec![assign(fraction, add(var(fraction), f(1.0)), l)],
                    vec![],
                    l,
                ),
                // Carry handles both the subnormal-to-normal and finite-to-infinity seams.
                branch(
                    eq(var(fraction), significand.clone()),
                    vec![
                        assign(fraction, f(0.0), l),
                        assign(exponent, add(var(exponent), f(1.0)), l),
                    ],
                    vec![],
                    l,
                ),
            ]);
        }
        let low_radix = f(self.low_radix());
        Ok(vec![
            assign(
                negative,
                or(
                    lt(var(n), f(0.0)),
                    and(eq(var(n), f(0.0)), lt(divide(f(1.0), var(n)), f(0.0))),
                ),
                l,
            ),
            assign(
                magnitude,
                choose(var(negative), mul(f(-1.0), var(n)), num(var(n))),
                l,
            ),
            assign(exponent, f(self.exponent_limit()), l),
            assign(fraction, f(0.0), l),
            branch(
                not(eq(num(var(n)), num(var(n)))),
                vec![
                    // Canonical quiet NaN on numeric stores; integer aliases can
                    // still write arbitrary payload bits, decoded as NaN above.
                    assign(fraction, f(self.significand() / 2.0), l),
                ],
                vec![branch(
                    lt(
                        var(magnitude),
                        f(if matches!(self.kind, Element::Float32) {
                            pow2(128)
                        } else {
                            f64::INFINITY
                        }),
                    ),
                    finite,
                    vec![],
                    l,
                )],
                l,
            ),
            assign(low, remainder(var(fraction), low_radix.clone()), l),
            assign(
                high,
                add(
                    add(
                        trunc(divide(var(fraction), low_radix)),
                        mul(var(exponent), f(self.exponent_stride())),
                    ),
                    choose(var(negative), f(32768.0), f(0.0)),
                ),
                l,
            ),
            assign(cursor, int(0), l),
            S::While {
                condition: lt(var(cursor), int(self.low_bytes())),
                body: vec![
                    e.write(
                        add(var(address), var(cursor)),
                        remainder(var(low), f(256.0)),
                    )?,
                    assign(low, trunc(divide(var(low), f(256.0))), l),
                    assign(cursor, add(var(cursor), int(1)), l),
                ],
                line: l,
            },
            e.write(
                add(var(address), int(self.low_bytes())),
                remainder(var(high), f(256.0)),
            )?,
            e.write(
                add(var(address), int(self.low_bytes() + 1)),
                trunc(divide(var(high), f(256.0))),
            )?,
        ])
    }
}

fn pow2(exp: i32) -> f64 {
    // Form the literal at lowering time, including exact subnormal powers.
    if exp >= -1022 {
        f64::from_bits(((exp + 1023) as u64) << 52)
    } else {
        f64::from_bits(1_u64 << (exp + 1074))
    }
}

fn f(value: f64) -> E {
    E::Float(value)
}
fn float(e: &mut Emit<'_, '_>) -> Result<u16, ParseError> {
    e.temp(T::Float, f(0.0))
}
fn trunc(expr: E) -> E {
    num(call(B::MathTrunc.call_index(), vec![num(expr)]))
}
