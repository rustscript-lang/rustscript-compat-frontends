//! Private already-coerced UTF-16 URI kernel; not installed as ES globals.
//! Rust constructs ordinary typed guest IR only. No host codec is used.
#![allow(dead_code)] // Preparatory private kernel; public integration is separate.
use std::collections::HashMap;
use vm::compiler::ir::{StructDecl, StructDeclOrigin};
use vm::{
    AssignmentKind, BuiltinFunction as B, Expr as E, FrontendIr, ParseError, Stmt as S,
    TypeSchema as T, ValueType,
};
const RESULT: &str = "__CompatEs5UriKernelResult";
const CEILING: i64 = 65_536;
pub(in crate::javascript) fn result_schema() -> T {
    T::Named(RESULT.into(), vec![])
}
fn call(index: u16, args: Vec<E>) -> E {
    E::Call(index, vec![], args, None, None)
}
fn bi(b: B, args: Vec<E>) -> E {
    call(b.call_index(), args)
}
fn v(s: u16) -> E {
    E::Var(s)
}
fn n(i: i64) -> E {
    E::Int(i)
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
fn gt(a: E, b: E) -> E {
    E::Gt(Box::new(a), Box::new(b))
}
fn and(a: E, b: E) -> E {
    E::And(Box::new(a), Box::new(b))
}
fn or(a: E, b: E) -> E {
    E::Or(Box::new(a), Box::new(b))
}
fn choose(c: E, a: E, b: E) -> E {
    E::IfElse {
        condition: Box::new(c),
        then_expr: Box::new(a),
        else_expr: Box::new(b),
    }
}
fn set(s: u16, e: E) -> S {
    S::Assign {
        kind: AssignmentKind::Set,
        index: s,
        expr: e,
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
fn loop_(c: E, body: Vec<S>) -> S {
    S::While {
        condition: c,
        body,
        line: 1,
    }
}
fn borrowed(s: u16) -> E {
    E::Borrow(Box::new(v(s)))
}
fn get(s: u16, key: E) -> E {
    bi(B::Get, vec![borrowed(s), key])
}
fn range(c: E, a: i64, b: i64) -> E {
    and(gt(c.clone(), n(a - 1)), lt(c, n(b + 1)))
}
fn member(c: E, chars: &[u8]) -> E {
    fn tree(c: E, sorted: &[u8]) -> E {
        if sorted.is_empty() {
            return E::Bool(false);
        }
        let mid = sorted.len() / 2;
        let pivot = n(i64::from(sorted[mid]));
        choose(
            eq(c.clone(), pivot.clone()),
            E::Bool(true),
            choose(
                lt(c.clone(), pivot),
                tree(c.clone(), &sorted[..mid]),
                tree(c, &sorted[mid + 1..]),
            ),
        )
    }
    let mut sorted = chars.to_vec();
    sorted.sort_unstable();
    tree(c, &sorted)
}
fn reserved(c: E) -> E {
    member(c, b";/?:@&=+$,#")
}
fn unescaped(c: E, mode: E) -> E {
    and(
        range(c.clone(), 33, 126),
        or(
            or(
                or(range(c.clone(), 65, 90), range(c.clone(), 97, 122)),
                or(range(c.clone(), 48, 57), member(c.clone(), b"-_.!~*'()")),
            ),
            and(eq(mode, n(0)), reserved(c)),
        ),
    )
}
fn hex(c: E) -> E {
    choose(lt(c.clone(), n(10)), add(c.clone(), n(48)), add(c, n(55)))
}
fn record(status: E, output: E) -> E {
    let mut r = bi(B::MapNew, vec![]);
    for (name, value) in [("status", status), ("output", output)] {
        r = bi(B::Set, vec![r, E::String(name.into()), value]);
    }
    r
}
fn error(message: &str) -> ParseError {
    ParseError::at_span(vm::Span::new(0, 0, 0), message)
}
struct Emit<'a> {
    ir: &'a mut FrontendIr,
    init: Vec<S>,
}
impl Emit<'_> {
    fn local(&mut self, t: T, e: E) -> Result<u16, ParseError> {
        let s = u16::try_from(self.ir.locals).map_err(|_| error("URI local limit"))?;
        self.ir.locals += 1;
        self.init.push(S::Let {
            index: s,
            declared_schema: Some(t),
            expr: e,
            line: 1,
        });
        Ok(s)
    }
    fn int(&mut self) -> Result<u16, ParseError> {
        self.local(T::Int, n(0))
    }
}
#[derive(Clone, Copy)]
struct Slots {
    input: u16,
    mode: u16,
    limit: u16,
    status: u16,
    i: u16,
    len: u16,
    count: u16,
    out: u16,
    c: u16,
    cp: u16,
    b: u16,
    h: u16,
    l: u16,
    width: u16,
    divisor: u16,
    lead: u16,
    j: u16,
    min: u16,
    start: u16,
}
impl Slots {
    fn ok(self) -> E {
        eq(v(self.status), n(0))
    }
    fn put(self, unit: E, emit: bool) -> S {
        if emit {
            // Preflight has proved the complete carrier and final length.
            // The input is borrowed and no host callback can mutate it between
            // passes. Emission needs no redundant per-unit policy checks.
            return set(self.out, bi(B::ArrayPush, vec![E::MoveVar(self.out), unit]));
        }
        branch(
            self.ok(),
            vec![branch(
                lt(v(self.count), v(self.limit)),
                vec![set(self.count, add(v(self.count), n(1)))],
                vec![set(self.status, n(2))],
            )],
            vec![],
        )
    }
    fn byte(self) -> Vec<S> {
        // Each access is dominated by the bounds guard; input was prevalidated
        // as a bounded code-unit carrier before either codec scan.
        vec![branch(
            lt(add(v(self.i), n(2)), v(self.len)),
            vec![branch(
                eq(get(self.input, v(self.i)), n(37)),
                vec![
                    set(self.h, unhex(get(self.input, add(v(self.i), n(1))))),
                    set(self.l, unhex(get(self.input, add(v(self.i), n(2))))),
                    branch(
                        or(lt(v(self.h), n(0)), lt(v(self.l), n(0))),
                        vec![set(self.status, n(1))],
                        vec![
                            set(self.b, add(mul(v(self.h), n(16)), v(self.l))),
                            set(self.i, add(v(self.i), n(3))),
                        ],
                    ),
                ],
                vec![set(self.status, n(1))],
            )],
            vec![set(self.status, n(1))],
        )]
    }
    fn encode(self, emit: bool) -> Vec<S> {
        let mut utf8 = vec![
            set(self.width, n(1)),
            set(self.divisor, n(1)),
            set(self.lead, n(0)),
        ];
        for (threshold, width, divisor, lead) in [
            (127, 2, 64, 192),
            (2047, 3, 4096, 224),
            (65535, 4, 262144, 240),
        ] {
            utf8.push(branch(
                gt(v(self.cp), n(threshold)),
                vec![
                    set(self.width, n(width)),
                    set(self.divisor, n(divisor)),
                    set(self.lead, n(lead)),
                ],
                vec![],
            ));
        }
        if emit {
            utf8.extend([
                set(self.j, n(0)),
                loop_(
                    lt(v(self.j), v(self.width)),
                    vec![
                        set(self.b, add(div(v(self.cp), v(self.divisor)), v(self.lead))),
                        self.put(n(37), true),
                        self.put(hex(div(v(self.b), n(16))), true),
                        self.put(hex(rem(v(self.b), n(16))), true),
                        set(self.cp, rem(v(self.cp), v(self.divisor))),
                        set(self.divisor, div(v(self.divisor), n(64))),
                        set(self.lead, n(128)),
                        set(self.j, add(v(self.j), n(1))),
                    ],
                ),
            ]);
        } else {
            // Count the entire scalar's UTF-8 percent chunk in one checked
            // addition; no byte-generation work or payload storage in preflight.
            utf8.push(branch(
                gt(add(v(self.count), mul(v(self.width), n(3))), v(self.limit)),
                vec![set(self.status, n(2))],
                vec![set(
                    self.count,
                    add(v(self.count), mul(v(self.width), n(3))),
                )],
            ));
        }
        vec![branch(
            unescaped(v(self.c), v(self.mode)),
            vec![self.put(v(self.c), emit), set(self.i, add(v(self.i), n(1)))],
            vec![
                set(self.cp, v(self.c)),
                set(self.i, add(v(self.i), n(1))),
                branch(
                    range(v(self.c), 0xdc00, 0xdfff),
                    vec![set(self.status, n(1))],
                    vec![],
                ),
                branch(
                    range(v(self.c), 0xd800, 0xdbff),
                    vec![branch(
                        lt(v(self.i), v(self.len)),
                        vec![
                            set(self.b, get(self.input, v(self.i))),
                            branch(
                                range(v(self.b), 0xdc00, 0xdfff),
                                vec![
                                    set(
                                        self.cp,
                                        add(
                                            add(
                                                mul(sub(v(self.c), n(0xd800)), n(1024)),
                                                sub(v(self.b), n(0xdc00)),
                                            ),
                                            n(65536),
                                        ),
                                    ),
                                    set(self.i, add(v(self.i), n(1))),
                                ],
                                vec![set(self.status, n(1))],
                            ),
                        ],
                        vec![set(self.status, n(1))],
                    )],
                    vec![],
                ),
                branch(self.ok(), utf8, vec![]),
            ],
        )]
    }
    fn decode(self, emit: bool) -> Vec<S> {
        let mut multi = vec![
            set(self.width, n(0)),
            set(self.min, n(0)),
            set(self.cp, n(0)),
        ];
        // RFC3629/Table21: C0/C1, continuation leaders and F5..FF never enter
        // a valid sequence. Accumulated scalar checks reject overlongs,
        // surrogate scalars and values beyond the Unicode maximum.
        for (low, high, width, min, mask) in [
            (194, 223, 2, 128, 32),
            (224, 239, 3, 2048, 16),
            (240, 244, 4, 65536, 8),
        ] {
            multi.push(branch(
                range(v(self.b), low, high),
                vec![
                    set(self.width, n(width)),
                    set(self.min, n(min)),
                    set(self.cp, rem(v(self.b), n(mask))),
                ],
                vec![],
            ));
        }
        multi.push(branch(
            eq(v(self.width), n(0)),
            vec![set(self.status, n(1))],
            vec![],
        ));
        multi.push(set(self.j, n(1)));
        let mut next = self.byte();
        next.push(branch(
            self.ok(),
            vec![branch(
                range(v(self.b), 128, 191),
                vec![set(
                    self.cp,
                    add(mul(v(self.cp), n(64)), sub(v(self.b), n(128))),
                )],
                vec![set(self.status, n(1))],
            )],
            vec![],
        ));
        next.push(set(self.j, add(v(self.j), n(1))));
        multi.push(loop_(and(self.ok(), lt(v(self.j), v(self.width))), next));
        multi.push(branch(
            self.ok(),
            vec![branch(
                or(
                    or(lt(v(self.cp), v(self.min)), gt(v(self.cp), n(0x10ffff))),
                    range(v(self.cp), 0xd800, 0xdfff),
                ),
                vec![set(self.status, n(1))],
                vec![branch(
                    lt(v(self.cp), n(65536)),
                    vec![self.put(v(self.cp), emit)],
                    vec![
                        set(self.cp, sub(v(self.cp), n(65536))),
                        self.put(add(div(v(self.cp), n(1024)), n(0xd800)), emit),
                        self.put(add(rem(v(self.cp), n(1024)), n(0xdc00)), emit),
                    ],
                )],
            )],
            vec![],
        ));
        let ascii = vec![branch(
            and(eq(v(self.mode), n(2)), reserved(v(self.b))),
            vec![
                self.put(get(self.input, v(self.start)), emit),
                self.put(get(self.input, add(v(self.start), n(1))), emit),
                self.put(get(self.input, add(v(self.start), n(2))), emit),
            ],
            vec![self.put(v(self.b), emit)],
        )];
        let mut escaped = vec![set(self.start, v(self.i))];
        escaped.extend(self.byte());
        escaped.push(branch(
            self.ok(),
            vec![branch(lt(v(self.b), n(128)), ascii, multi)],
            vec![],
        ));
        vec![branch(
            eq(v(self.c), n(37)),
            escaped,
            vec![self.put(v(self.c), emit), set(self.i, add(v(self.i), n(1)))],
        )]
    }
    fn scan(self, emit: bool) -> Vec<S> {
        vec![
            set(self.i, n(0)),
            set(self.count, n(0)),
            loop_(
                and(self.ok(), lt(v(self.i), v(self.len))),
                vec![
                    set(self.c, get(self.input, v(self.i))),
                    branch(lt(v(self.mode), n(2)), self.encode(emit), self.decode(emit)),
                ],
            ),
        ]
    }
}
fn unhex(c: E) -> E {
    choose(
        range(c.clone(), 48, 57),
        sub(c.clone(), n(48)),
        choose(
            range(c.clone(), 65, 70),
            sub(c.clone(), n(55)),
            choose(range(c.clone(), 97, 102), sub(c, n(87)), n(-1)),
        ),
    )
}
/// mode 0 encodeURI, 1 encodeURIComponent, 2 decodeURI, 3 decodeURIComponent.
/// status 0 success, 1 malformed URI, 2 private representation/size refusal.
pub(in crate::javascript) fn append(ir: &mut FrontendIr) -> Result<u16, ParseError> {
    if ir.struct_schemas.contains_key(RESULT) {
        return Err(error("URI kernel already appended"));
    }
    let index = ir
        .functions
        .iter()
        .map(|f| f.index)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| error("URI function limit"))?;
    let first = u16::try_from(ir.locals).map_err(|_| error("URI local limit"))?;
    first
        .checked_add(3)
        .ok_or_else(|| error("URI local limit"))?;
    ir.locals += 3;
    let mut e = Emit { ir, init: vec![] };
    let input = first;
    let mode = first + 1;
    let limit = first + 2;
    let status = e.int()?;
    let i = e.int()?;
    let len = e.int()?;
    let count = e.int()?;
    let c = e.int()?;
    let out = u16::try_from(e.ir.locals).map_err(|_| error("URI local limit"))?;
    e.ir.locals += 1;
    let cp = e.int()?;
    let b = e.int()?;
    let h = e.int()?;
    let l = e.int()?;
    let width = e.int()?;
    let divisor = e.int()?;
    let lead = e.int()?;
    let j = e.int()?;
    let min = e.int()?;
    let start = e.int()?;
    let s = Slots {
        input,
        mode,
        limit,
        status,
        i,
        len,
        count,
        out,
        c,
        cp,
        b,
        h,
        l,
        width,
        divisor,
        lead,
        j,
        min,
        start,
    };
    let mut body = e.init;
    body.push(set(len, bi(B::Len, vec![borrowed(input)])));
    body.push(branch(
        or(
            or(lt(v(limit), n(0)), gt(v(limit), n(CEILING))),
            or(
                or(lt(v(mode), n(0)), gt(v(mode), n(3))),
                gt(v(len), n(CEILING)),
            ),
        ),
        vec![set(status, n(2))],
        vec![],
    ));
    body.push(loop_(
        and(s.ok(), lt(v(i), v(len))),
        vec![
            set(c, get(input, v(i))),
            branch(
                or(lt(v(c), n(0)), gt(v(c), n(65535))),
                vec![set(status, n(2))],
                vec![],
            ),
            set(i, add(v(i), n(1))),
        ],
    ));
    body.extend(s.scan(false));
    // The preflight has validated every unit and checked every needed output
    // length before the first payload allocation/push. Even refusals return
    // the required empty carrier, whose backing has zero elements/capacity.
    body.push(S::Let {
        index: out,
        declared_schema: Some(T::Array(Box::new(T::Int))),
        expr: bi(B::ArrayNew, vec![]),
        line: 1,
    });
    body.push(branch(s.ok(), s.scan(true), vec![]));
    e.ir.struct_schemas.insert(
        RESULT.into(),
        StructDecl {
            name: RESULT.into(),
            type_params: vec![],
            body_schema: T::Object(HashMap::from([
                ("status".into(), T::Int),
                ("output".into(), T::Array(Box::new(T::Int))),
            ])),
            origin: StructDeclOrigin::Guest,
        },
    );
    let mut decl = vec![];
    super::heap::declare_helper(
        e.ir,
        &mut decl,
        "__compat_es5_uri_kernel",
        index,
        3,
        vec!["input".into(), "mode".into(), "limit".into()],
        vec![Some(T::Array(Box::new(T::Int))), Some(T::Int), Some(T::Int)],
        Some(result_schema()),
        ValueType::Map,
        vec![input, mode, limit],
        body,
        record(v(status), v(out)),
        1,
    );
    e.ir.stmts.extend(decl);
    Ok(index)
}
