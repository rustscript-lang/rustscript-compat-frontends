//! Bounded guest dyadics for private Date mathematical intermediates.
//! Rust constructs IR; all operand-dependent arithmetic runs in the guest.
#![allow(dead_code)]
use vm::compiler::ir::{FunctionDecl, FunctionImpl, StructDecl, StructDeclOrigin};
use vm::{
    BuiltinFunction as B, Expr as E, FrontendIr, ParseError, Stmt as S, TypeSchema as T, ValueType,
};
const RADIX: i64 = 1 << 30;
const CAPACITY: i64 = 80;
#[derive(Clone)]
pub(in crate::javascript) struct Entries {
    pub schema: T,
    pub decode: u16,
    pub add: u16,
    pub negate: u16,
    pub multiply: u16,
    pub quotient: u16,
    pub modulo: u16,
    pub to_number: u16,
    normalize: u16,
    shift: u16,
}
fn i(x: i64) -> E {
    E::Int(x)
}
fn f(x: f64) -> E {
    E::Float(x)
}
fn v(x: u16) -> E {
    E::Var(x)
}
fn call(id: u16, args: Vec<E>) -> E {
    E::Call(id, vec![], args, None, None)
}
fn builtin(b: B, args: Vec<E>) -> E {
    call(b.call_index(), args)
}
fn add(a: E, b: E) -> E {
    E::Add(Box::new(a), Box::new(b))
}
fn sub(a: E, b: E) -> E {
    E::Sub(Box::new(a), Box::new(b))
}
fn mul(a: E, b: E) -> E {
    E::Mul(Box::new(a), Box::new(b))
}
fn div(a: E, b: E) -> E {
    E::Div(Box::new(a), Box::new(b))
}
fn rem(a: E, b: E) -> E {
    E::Mod(Box::new(a), Box::new(b))
}
fn eq(a: E, b: E) -> E {
    E::Eq(Box::new(a), Box::new(b))
}
fn lt(a: E, b: E) -> E {
    E::Lt(Box::new(a), Box::new(b))
}
fn and(a: E, b: E) -> E {
    E::And(Box::new(a), Box::new(b))
}
fn not(a: E) -> E {
    E::Not(Box::new(a))
}
fn choose(c: E, a: E, b: E) -> E {
    E::IfElse {
        condition: Box::new(c),
        then_expr: Box::new(a),
        else_expr: Box::new(b),
    }
}
fn array() -> T {
    T::Array(Box::new(T::Int))
}
fn empty() -> E {
    builtin(B::ArrayNew, vec![])
}
fn borrow(s: u16) -> E {
    E::Borrow(Box::new(v(s)))
}
fn get(s: u16, key: E) -> E {
    builtin(B::Get, vec![borrow(s), key])
}
fn field(s: u16, key: &str) -> E {
    get(s, E::String(key.into()))
}
fn len(s: u16) -> E {
    builtin(B::Len, vec![borrow(s)])
}
fn set(s: u16, x: E) -> S {
    S::Assign {
        kind: vm::AssignmentKind::Set,
        index: s,
        expr: x,
        line: 1,
    }
}
fn push(s: u16, x: E) -> S {
    // Refuse before allocating an 81st active limb; never extend or truncate.
    branch(
        lt(len(s), i(CAPACITY)),
        vec![set(s, builtin(B::ArrayPush, vec![v(s), x]))],
        vec![assertion(E::Bool(false))],
    )
}
fn put(s: u16, k: E, x: E) -> S {
    set(s, builtin(B::Set, vec![v(s), k, x]))
}
fn inc(s: u16) -> S {
    set(s, add(v(s), i(1)))
}
fn wh(c: E, body: Vec<S>) -> S {
    S::While {
        condition: c,
        body,
        line: 1,
    }
}
fn branch(c: E, a: Vec<S>, b: Vec<S>) -> S {
    S::IfElse {
        condition: c,
        then_branch: a,
        else_branch: b,
        line: 1,
    }
}
fn assertion(c: E) -> S {
    S::Expr {
        expr: builtin(B::Assert, vec![c]),
        line: 1,
    }
}
fn record(valid: E, negative: E, exponent: E, limbs: E) -> E {
    [
        ("valid", valid),
        ("negative", negative),
        ("exponent", exponent),
        ("limbs", limbs),
    ]
    .into_iter()
    .fold(builtin(B::MapNew, vec![]), |r, (k, x)| {
        builtin(B::Set, vec![r, E::String(k.into()), x])
    })
}
struct Build<'a> {
    ir: &'a mut FrontendIr,
    id: u16,
    params: Vec<u16>,
    schemas: Vec<T>,
    body: Vec<S>,
}
impl<'a> Build<'a> {
    fn new(ir: &'a mut FrontendIr, id: u16, schemas: Vec<T>) -> Self {
        let params = schemas
            .iter()
            .map(|_| {
                let s = ir.locals as u16;
                ir.locals += 1;
                s
            })
            .collect();
        Self {
            ir,
            id,
            params,
            schemas,
            body: vec![],
        }
    }
    fn p(&self, k: usize) -> E {
        v(self.params[k])
    }
    fn local(&mut self, t: T, x: E) -> u16 {
        let s = self.ir.locals as u16;
        self.ir.locals += 1;
        self.body.push(S::Let {
            index: s,
            declared_schema: Some(t),
            expr: x,
            line: 1,
        });
        s
    }
    fn finish(self, name: &str, t: T, x: E) {
        let return_type = match &t {
            T::Int => ValueType::Int,
            T::Number => ValueType::Float,
            T::Array(_) => ValueType::Array,
            _ => ValueType::Map,
        };
        self.ir.functions.push(FunctionDecl {
            name: format!("__es5_date_exact_{}_{}", self.id, name),
            arity: self.params.len() as u8,
            index: self.id,
            args: self.params.iter().map(|s| format!("p{s}")).collect(),
            arg_schemas: self.schemas.into_iter().map(Some).collect(),
            return_schema: Some(t),
            type_params: vec![],
            exported: false,
            return_type,
            symbol: None,
        });
        self.ir.function_impls.insert(
            self.id,
            FunctionImpl {
                param_slots: self.params,
                capture_copies: vec![],
                body_stmts: self.body,
                body_expr: x,
                body_expr_line: 1,
            },
        );
    }
}
impl Entries {
    pub(in crate::javascript) fn decode_number(&self, x: E) -> E {
        call(self.decode, vec![x])
    }
    pub(in crate::javascript) fn sum(&self, a: E, b: E) -> E {
        call(self.add, vec![a, b])
    }
    pub(in crate::javascript) fn neg(&self, a: E) -> E {
        call(self.negate, vec![a])
    }
    pub(in crate::javascript) fn times(&self, a: E, b: i64) -> E {
        call(self.multiply, vec![a, i(b)])
    }
    pub(in crate::javascript) fn floor(&self, a: E, b: i64) -> E {
        call(self.quotient, vec![a, i(b)])
    }
    pub(in crate::javascript) fn residue(&self, a: E, b: i64) -> E {
        call(self.modulo, vec![a, i(b)])
    }
    pub(in crate::javascript) fn number(&self, a: E) -> E {
        call(self.to_number, vec![a])
    }
}
pub(in crate::javascript) fn append(ir: &mut FrontendIr) -> Result<Entries, ParseError> {
    let next = ir
        .functions
        .iter()
        .map(|d| d.index)
        .chain(ir.function_impls.keys().copied())
        .max()
        .map_or(Some(0), |x| x.checked_add(1))
        .ok_or_else(|| ParseError::new("Date exact function capacity"))?;
    next.checked_add(8)
        .ok_or_else(|| ParseError::new("Date exact function capacity"))?;
    ir.locals
        .checked_add(192)
        .filter(|&n| n <= usize::from(u16::MAX) + 1)
        .ok_or_else(|| ParseError::new("Date exact local capacity"))?;
    let name = format!("__Es5DateExact_{next}");
    if ir.struct_schemas.contains_key(&name) {
        return Err(ParseError::new("Date exact schema collision"));
    }
    let schema = T::Named(name.clone(), vec![]);
    ir.struct_schemas.insert(
        name.clone(),
        StructDecl {
            name,
            type_params: vec![],
            body_schema: T::Object(
                [
                    ("valid".into(), T::Bool),
                    ("negative".into(), T::Bool),
                    ("exponent".into(), T::Int),
                    ("limbs".into(), array()),
                ]
                .into_iter()
                .collect(),
            ),
            origin: StructDeclOrigin::Guest,
        },
    );
    let e = Entries {
        schema: schema.clone(),
        normalize: next,
        shift: next + 1,
        decode: next + 2,
        add: next + 3,
        negate: next + 4,
        multiply: next + 5,
        quotient: next + 6,
        modulo: next + 7,
        to_number: next + 8,
    };
    // Normalize without mutating the caller's magnitude: remove whole low zero
    // limbs and trailing bits; trim high zeros. Nonzero coefficient is odd.
    {
        let mut b = Build::new(ir, e.normalize, vec![T::Bool, T::Bool, T::Int, array()]);
        let a = b.params[3];
        let high = b.local(T::Int, len(a));
        let low = b.local(T::Int, i(0));
        let bits = b.local(T::Int, i(0));
        let power = b.local(T::Int, i(1));
        let x = b.local(T::Int, i(0));
        let out = b.local(array(), empty());
        let k = b.local(T::Int, i(0));
        b.body.push(assertion(not(lt(i(CAPACITY), v(high)))));
        b.body.push(wh(
            lt(v(k), v(high)),
            vec![
                assertion(and(not(lt(get(a, v(k)), i(0))), lt(get(a, v(k)), i(RADIX)))),
                inc(k),
            ],
        ));
        b.body.push(assertion(not(lt(b.p(2), i(-1074)))));
        b.body.push(wh(
            and(lt(i(0), v(high)), eq(get(a, sub(v(high), i(1))), i(0))),
            vec![set(high, sub(v(high), i(1)))],
        ));
        b.body.push(branch(
            and(b.p(0), lt(i(0), v(high))),
            vec![
                wh(eq(get(a, v(low)), i(0)), vec![inc(low)]),
                set(x, get(a, v(low))),
                wh(
                    eq(rem(v(x), i(2)), i(0)),
                    vec![
                        set(x, div(v(x), i(2))),
                        inc(bits),
                        set(power, mul(v(power), i(2))),
                    ],
                ),
                set(k, v(low)),
                wh(
                    lt(v(k), v(high)),
                    vec![
                        set(x, div(get(a, v(k)), v(power))),
                        branch(
                            lt(add(v(k), i(1)), v(high)),
                            vec![set(
                                x,
                                add(
                                    v(x),
                                    mul(
                                        rem(get(a, add(v(k), i(1))), v(power)),
                                        div(i(RADIX), v(power)),
                                    ),
                                ),
                            )],
                            vec![],
                        ),
                        push(out, v(x)),
                        inc(k),
                    ],
                ),
            ],
            vec![],
        ));
        // The final quotient limb may be zero; copy a bounded prefix.
        let copy = b.local(array(), empty());
        let count = b.local(T::Int, len(out));
        b.body.push(branch(
            and(lt(i(0), v(count)), eq(get(out, sub(v(count), i(1))), i(0))),
            vec![set(count, sub(v(count), i(1)))],
            vec![],
        ));
        b.body.push(set(k, i(0)));
        b.body.push(wh(
            lt(v(k), v(count)),
            vec![push(copy, get(out, v(k))), inc(k)],
        ));
        let nonzero = lt(i(0), v(count));
        let result = record(
            b.p(0),
            and(nonzero.clone(), b.p(1)),
            choose(nonzero, add(b.p(2), add(mul(v(low), i(30)), v(bits))), i(0)),
            v(copy),
        );
        b.finish("normalize", schema.clone(), result);
    }
    // Fresh magnitude left shift. A limb product and carry are <2^59.
    {
        let mut b = Build::new(ir, e.shift, vec![array(), T::Int]);
        let a = b.params[0];
        let words = b.local(T::Int, div(b.p(1), i(30)));
        let bits = b.local(T::Int, rem(b.p(1), i(30)));
        let power = b.local(T::Int, i(1));
        let k = b.local(T::Int, i(0));
        let carry = b.local(T::Int, i(0));
        let x = b.local(T::Int, i(0));
        let out = b.local(array(), empty());
        b.body.push(assertion(and(
            not(lt(b.p(1), i(0))),
            not(lt(i(2400), b.p(1))),
        )));
        b.body.push(wh(
            lt(v(k), v(bits)),
            vec![set(power, mul(v(power), i(2))), inc(k)],
        ));
        b.body.push(set(k, i(0)));
        b.body.push(branch(
            lt(i(0), len(a)),
            vec![
                wh(lt(v(k), v(words)), vec![push(out, i(0)), inc(k)]),
                set(k, i(0)),
                wh(
                    lt(v(k), len(a)),
                    vec![
                        set(x, add(mul(get(a, v(k)), v(power)), v(carry))),
                        push(out, rem(v(x), i(RADIX))),
                        set(carry, div(v(x), i(RADIX))),
                        inc(k),
                    ],
                ),
                branch(lt(i(0), v(carry)), vec![push(out, v(carry))], vec![]),
            ],
            vec![],
        ));
        b.body.push(assertion(not(lt(i(CAPACITY), len(out)))));
        b.finish("shift", array(), v(out));
    }
    // Decompose finite binary64 by exact power-of-two scaling and comparison.
    {
        let mut b = Build::new(ir, e.decode, vec![T::Number]);
        let x = b.local(T::Number, mul(b.p(0), f(1.0)));
        let valid = b.local(T::Bool, builtin(B::MathIsFinite, vec![v(x)]));
        let negative = b.local(T::Bool, lt(v(x), f(0.0)));
        let exponent = b.local(T::Int, i(0));
        let a = b.local(array(), empty());
        let k = b.local(T::Int, i(0));
        let bit = b.local(T::Int, i(0));
        let carry = b.local(T::Int, i(0));
        let z = b.local(T::Int, i(0));
        let count = b.local(T::Int, i(0));
        b.body
            .push(branch(v(negative), vec![set(x, sub(f(0.0), v(x)))], vec![]));
        b.body.push(branch(
            and(v(valid), not(eq(v(x), f(0.0)))),
            vec![
                wh(
                    not(lt(v(x), f(2.0))),
                    vec![set(x, div(v(x), f(2.0))), inc(exponent), inc(count)],
                ),
                wh(
                    lt(v(x), f(1.0)),
                    vec![
                        set(x, mul(v(x), f(2.0))),
                        set(exponent, sub(v(exponent), i(1))),
                        inc(count),
                    ],
                ),
                assertion(not(lt(i(1074), v(count)))),
                set(count, i(0)),
                wh(
                    and(lt(v(count), i(53)), not(eq(v(x), f(0.0)))),
                    vec![
                        set(bit, choose(lt(v(x), f(1.0)), i(0), i(1))),
                        set(carry, v(bit)),
                        set(k, i(0)),
                        wh(
                            lt(v(k), len(a)),
                            vec![
                                set(z, add(mul(get(a, v(k)), i(2)), v(carry))),
                                put(a, v(k), rem(v(z), i(RADIX))),
                                set(carry, div(v(z), i(RADIX))),
                                inc(k),
                            ],
                        ),
                        branch(lt(i(0), v(carry)), vec![push(a, v(carry))], vec![]),
                        set(
                            x,
                            mul(sub(v(x), choose(eq(v(bit), i(1)), f(1.0), f(0.0))), f(2.0)),
                        ),
                        inc(count),
                    ],
                ),
                assertion(eq(v(x), f(0.0))),
                set(exponent, sub(v(exponent), sub(v(count), i(1)))),
            ],
            vec![],
        ));
        let result = call(e.normalize, vec![v(valid), v(negative), v(exponent), v(a)]);
        b.finish("decode", schema.clone(), result);
    }
    // Signed addition with exact exponent alignment and unsigned subtraction.
    {
        let mut b = Build::new(ir, e.add, vec![schema.clone(), schema.clone()]);
        let p = b.params[0];
        let q = b.params[1];
        let valid = b.local(T::Bool, and(field(p, "valid"), field(q, "valid")));
        let exponent = b.local(
            T::Int,
            choose(
                lt(field(p, "exponent"), field(q, "exponent")),
                field(p, "exponent"),
                field(q, "exponent"),
            ),
        );
        let a = b.local(
            array(),
            call(
                e.shift,
                vec![field(p, "limbs"), sub(field(p, "exponent"), v(exponent))],
            ),
        );
        let c = b.local(
            array(),
            call(
                e.shift,
                vec![field(q, "limbs"), sub(field(q, "exponent"), v(exponent))],
            ),
        );
        let negative = b.local(T::Bool, field(p, "negative"));
        let same = b.local(T::Bool, eq(field(p, "negative"), field(q, "negative")));
        let cmp = b.local(
            T::Int,
            choose(
                lt(len(a), len(c)),
                i(-1),
                choose(lt(len(c), len(a)), i(1), i(0)),
            ),
        );
        let k = b.local(T::Int, len(a));
        let x = b.local(T::Int, i(0));
        let y = b.local(T::Int, i(0));
        let carry = b.local(T::Int, i(0));
        let out = b.local(array(), empty());
        let width = b.local(T::Int, choose(lt(len(a), len(c)), len(c), len(a)));
        b.body.push(wh(
            and(eq(v(cmp), i(0)), lt(i(0), v(k))),
            vec![
                set(k, sub(v(k), i(1))),
                set(
                    cmp,
                    choose(
                        lt(get(a, v(k)), get(c, v(k))),
                        i(-1),
                        choose(lt(get(c, v(k)), get(a, v(k))), i(1), i(0)),
                    ),
                ),
            ],
        ));
        b.body.push(branch(
            and(not(v(same)), lt(v(cmp), i(0))),
            vec![set(negative, field(q, "negative"))],
            vec![],
        ));
        b.body.push(set(k, i(0)));
        b.body.push(wh(
            lt(v(k), v(width)),
            vec![
                set(x, choose(lt(v(k), len(a)), get(a, v(k)), i(0))),
                set(y, choose(lt(v(k), len(c)), get(c, v(k)), i(0))),
                branch(
                    v(same),
                    vec![
                        set(x, add(add(v(x), v(y)), v(carry))),
                        set(carry, div(v(x), i(RADIX))),
                        set(x, rem(v(x), i(RADIX))),
                    ],
                    vec![
                        set(
                            x,
                            sub(
                                choose(lt(v(cmp), i(0)), sub(v(y), v(x)), sub(v(x), v(y))),
                                v(carry),
                            ),
                        ),
                        set(carry, choose(lt(v(x), i(0)), i(1), i(0))),
                        set(x, add(v(x), mul(v(carry), i(RADIX)))),
                    ],
                ),
                push(out, v(x)),
                inc(k),
            ],
        ));
        b.body.push(branch(
            and(v(same), lt(i(0), v(carry))),
            vec![push(out, v(carry))],
            vec![],
        ));
        b.body.push(assertion(not(lt(i(CAPACITY), len(out)))));
        b.finish(
            "add",
            schema.clone(),
            call(
                e.normalize,
                vec![v(valid), v(negative), v(exponent), v(out)],
            ),
        );
    }
    {
        let b = Build::new(ir, e.negate, vec![schema.clone()]);
        let p = b.params[0];
        let result = call(
            e.normalize,
            vec![
                field(p, "valid"),
                not(field(p, "negative")),
                field(p, "exponent"),
                field(p, "limbs"),
            ],
        );
        b.finish("negate", schema.clone(), result);
    }
    // Small nonnegative multiplier <=86400000: x < RADIX*86400000 <2^57.
    {
        let mut b = Build::new(ir, e.multiply, vec![schema.clone(), T::Int]);
        let p = b.params[0];
        let a = b.local(array(), field(p, "limbs"));
        let out = b.local(array(), empty());
        let k = b.local(T::Int, i(0));
        let carry = b.local(T::Int, i(0));
        let x = b.local(T::Int, i(0));
        b.body.push(assertion(and(
            not(lt(b.p(1), i(0))),
            not(lt(i(86400000), b.p(1))),
        )));
        b.body.push(wh(
            lt(v(k), len(a)),
            vec![
                set(x, add(mul(get(a, v(k)), b.p(1)), v(carry))),
                push(out, rem(v(x), i(RADIX))),
                set(carry, div(v(x), i(RADIX))),
                inc(k),
            ],
        ));
        b.body.push(branch(
            lt(i(0), v(carry)),
            vec![push(out, v(carry))],
            vec![],
        ));
        b.body.push(assertion(not(lt(i(CAPACITY), len(out)))));
        b.finish(
            "multiply",
            schema.clone(),
            call(
                e.normalize,
                vec![
                    field(p, "valid"),
                    field(p, "negative"),
                    field(p, "exponent"),
                    v(out),
                ],
            ),
        );
    }
    for (id, want_remainder) in [(e.quotient, false), (e.modulo, true)] {
        let mut b = Build::new(ir, id, vec![schema.clone(), T::Int]);
        let p = b.params[0];
        let a = b.local(
            array(),
            call(e.shift, vec![field(p, "limbs"), field(p, "exponent")]),
        );
        let out = b.local(array(), empty());
        let k = b.local(T::Int, i(0));
        let carry = b.local(T::Int, i(0));
        let x = b.local(T::Int, i(0));
        b.body
            .push(assertion(and(lt(i(0), b.p(1)), not(lt(i(400), b.p(1))))));
        b.body
            .push(wh(lt(v(k), len(a)), vec![push(out, i(0)), inc(k)]));
        b.body.push(wh(
            lt(i(0), v(k)),
            vec![
                set(k, sub(v(k), i(1))),
                set(x, add(mul(v(carry), i(RADIX)), get(a, v(k)))),
                put(out, v(k), div(v(x), b.p(1))),
                set(carry, rem(v(x), b.p(1))),
            ],
        ));
        if want_remainder {
            let result = choose(
                and(field(p, "negative"), lt(i(0), v(carry))),
                sub(b.p(1), v(carry)),
                v(carry),
            );
            b.finish("modulo", T::Int, result);
        } else {
            // floor(-a/d) = -(trunc(a/d)+[remainder!=0]).
            b.body.push(branch(
                and(field(p, "negative"), lt(i(0), v(carry))),
                vec![
                    set(carry, i(1)),
                    set(k, i(0)),
                    wh(
                        and(lt(v(k), len(out)), lt(i(0), v(carry))),
                        vec![
                            set(x, add(get(out, v(k)), v(carry))),
                            put(out, v(k), rem(v(x), i(RADIX))),
                            set(carry, div(v(x), i(RADIX))),
                            inc(k),
                        ],
                    ),
                    branch(lt(i(0), v(carry)), vec![push(out, v(carry))], vec![]),
                ],
                vec![],
            ));
            b.finish(
                "floor",
                schema.clone(),
                call(
                    e.normalize,
                    vec![field(p, "valid"), field(p, "negative"), i(0), v(out)],
                ),
            );
        }
    }
    // Explicit §8.5 boundary. Scan coefficient bits, retain 53, then guard,
    // sticky, and parity. All reconstruction scalings are exact until the
    // final overflow; never construct an underflowed power-of-two factor.
    {
        let mut b = Build::new(ir, e.to_number, vec![schema]);
        let p = b.params[0];
        let a = b.local(array(), field(p, "limbs"));
        let k = b.local(T::Int, len(a));
        let power = b.local(T::Int, i(1 << 29));
        let x = b.local(T::Int, i(0));
        let bit = b.local(T::Int, i(0));
        let count = b.local(T::Int, i(0));
        let exponent = b.local(T::Int, field(p, "exponent"));
        let started = b.local(T::Bool, E::Bool(false));
        let guard = b.local(T::Bool, E::Bool(false));
        let sticky = b.local(T::Bool, E::Bool(false));
        let odd = b.local(T::Bool, E::Bool(false));
        let number = b.local(T::Number, f(0.0));
        b.body.push(assertion(not(lt(v(exponent), i(-1074)))));
        b.body.push(wh(
            lt(i(0), v(k)),
            vec![
                set(k, sub(v(k), i(1))),
                set(x, get(a, v(k))),
                set(power, i(1 << 29)),
                wh(
                    lt(i(0), v(power)),
                    vec![
                        set(bit, div(v(x), v(power))),
                        set(x, rem(v(x), v(power))),
                        branch(eq(v(bit), i(1)), vec![set(started, E::Bool(true))], vec![]),
                        branch(
                            v(started),
                            vec![
                                branch(
                                    lt(v(count), i(53)),
                                    vec![
                                        set(
                                            number,
                                            add(
                                                mul(v(number), f(2.0)),
                                                choose(eq(v(bit), i(1)), f(1.0), f(0.0)),
                                            ),
                                        ),
                                        set(odd, eq(v(bit), i(1))),
                                    ],
                                    vec![
                                        inc(exponent),
                                        branch(
                                            eq(v(count), i(53)),
                                            vec![set(guard, eq(v(bit), i(1)))],
                                            vec![branch(
                                                eq(v(bit), i(1)),
                                                vec![set(sticky, E::Bool(true))],
                                                vec![],
                                            )],
                                        ),
                                    ],
                                ),
                                inc(count),
                            ],
                            vec![],
                        ),
                        set(power, div(v(power), i(2))),
                    ],
                ),
            ],
        ));
        b.body.push(branch(
            and(v(guard), E::Or(Box::new(v(sticky)), Box::new(v(odd)))),
            vec![set(number, add(v(number), f(1.0)))],
            vec![],
        ));
        b.body.push(wh(
            lt(i(0), v(exponent)),
            vec![
                set(number, mul(v(number), f(2.0))),
                set(exponent, sub(v(exponent), i(1))),
            ],
        ));
        b.body.push(wh(
            lt(v(exponent), i(0)),
            vec![set(number, div(v(number), f(2.0))), inc(exponent)],
        ));
        let result = choose(
            field(p, "valid"),
            choose(field(p, "negative"), sub(f(0.0), v(number)), v(number)),
            f(f64::NAN),
        );
        b.finish("to_number", T::Number, result);
    }
    Ok(e)
}
