//! Finite typed guest backtracking VM. All matching executes in guest IR.
#![allow(dead_code)]
use super::super::lowerer::{Lowerer, assert_stmt};
use super::super::regexp_lowering::{Pattern, SCRATCH_LIMIT, TRANSITION_LIMIT};
use super::super::{heap as layout, representation as value};
use super::object as obj;
use vm::compiler::ir::{FunctionDecl, FunctionImpl, StructDecl, StructDeclOrigin};
use vm::{
    BuiltinFunction as B, Expr as E, FrontendIr, ParseError, Stmt as S, TypeSchema as T, ValueType,
};
fn v(s: u16) -> E {
    E::Var(s)
}
fn n(x: i64) -> E {
    E::Int(x)
}
fn call(i: u16, args: Vec<E>) -> E {
    E::Call(i, vec![], args, None, None)
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
fn eq(a: E, b: E) -> E {
    E::Eq(Box::new(a), Box::new(b))
}
fn lt(a: E, b: E) -> E {
    E::Lt(Box::new(a), Box::new(b))
}
fn and(a: E, b: E) -> E {
    E::And(Box::new(a), Box::new(b))
}
fn or(a: E, b: E) -> E {
    E::Or(Box::new(a), Box::new(b))
}
fn not(a: E) -> E {
    E::Not(Box::new(a))
}
fn borrow(s: u16) -> E {
    E::Borrow(Box::new(v(s)))
}
fn read(s: u16, i: E) -> E {
    builtin(B::Get, vec![borrow(s), i])
}
fn len(s: u16) -> E {
    builtin(B::Len, vec![borrow(s)])
}
fn put(s: u16, i: E, x: E) -> S {
    assign(s, builtin(B::Set, vec![v(s), i, x]))
}
fn push(s: u16, x: E) -> S {
    assign(s, builtin(B::ArrayPush, vec![v(s), x]))
}
fn assign(index: u16, expr: E) -> S {
    S::Assign {
        kind: vm::AssignmentKind::Set,
        index,
        expr,
        line: 1,
    }
}
fn branch(condition: E, then_branch: Vec<S>, else_branch: Vec<S>) -> S {
    S::IfElse {
        condition,
        then_branch,
        else_branch,
        line: 1,
    }
}
fn wh(condition: E, body: Vec<S>) -> S {
    S::While {
        condition,
        body,
        line: 1,
    }
}
fn inc(s: u16) -> S {
    assign(s, add(v(s), n(1)))
}
fn tick(body: Vec<S>) -> S {
    branch(
        lt(v(SPENT), v(BUDGET)),
        {
            let mut b = vec![inc(SPENT)];
            b.extend(body);
            b
        },
        vec![assign(STATUS, n(2))],
    )
}
fn running() -> E {
    lt(v(STATUS), n(0))
}
fn write(i: E, x: E) -> S {
    branch(
        eq(i.clone(), len(ST)),
        vec![push(ST, x.clone())],
        vec![put(ST, i, x)],
    )
}
const SUBJECT: u16 = 0;
const START: u16 = 1;
const BUDGET: u16 = 2;
const LIMIT: u16 = 3;
const PC: u16 = 4;
const POS: u16 = 5;
const SEARCH: u16 = 6;
const LENGTH: u16 = 7;
const STATUS: u16 = 8;
const SPENT: u16 = 9;
const SP: u16 = 10;
const ST: u16 = 11;
const CODE: u16 = 12;
const RANGES: u16 = 13;
const X: u16 = 14;
const I: u16 = 15;
const FRAME: u16 = 16;
const KIND: u16 = 17;
const CONT: u16 = 18;
const NEXT: u16 = 19;
const OP: u16 = 20;
const A: u16 = 21;
const C: u16 = 22;
const D: u16 = 23;
const F: u16 = 24;
const G: u16 = 25;
const COUNT: u16 = 26;
const FAIL: u16 = 27;
const PUSH: u16 = 28;
const PUSH_KIND: u16 = 29;
const PUSH_PC: u16 = 30;
const J: u16 = 31;
const Y: u16 = 32;
const OK: u16 = 33;
const CAPS: u16 = 34;
const RESERVED: u16 = 35;
const H: u16 = 36;
#[allow(clippy::too_many_arguments)] // Mirrors the typed guest declaration fields.
fn declaration(
    ir: &mut FrontendIr,
    index: u16,
    name: &str,
    params: Vec<u16>,
    schemas: Vec<T>,
    result: T,
    body: Vec<S>,
    expr: E,
) {
    ir.functions.push(FunctionDecl {
        name: name.into(),
        arity: params.len() as u8,
        index,
        args: params.iter().map(|i| format!("p{i}")).collect(),
        arg_schemas: schemas.into_iter().map(Some).collect(),
        return_schema: Some(result),
        type_params: vec![],
        exported: false,
        return_type: if name.ends_with("fold") {
            ValueType::Int
        } else {
            ValueType::Map
        },
        symbol: None,
    });
    ir.function_impls.insert(
        index,
        FunctionImpl {
            param_slots: params,
            capture_copies: vec![],
            body_stmts: body,
            body_expr: expr,
            body_expr_line: 1,
        },
    );
}
fn folding(pairs: &[(i64, i64)]) -> E {
    let unit = H + 1;
    if pairs.is_empty() {
        return v(unit);
    }
    let mid = pairs.len() / 2;
    let (a, b) = pairs[mid];
    E::IfElse {
        condition: Box::new(eq(v(unit), n(a))),
        then_expr: Box::new(n(b)),
        else_expr: Box::new(E::IfElse {
            condition: Box::new(lt(v(unit), n(a))),
            then_expr: Box::new(folding(&pairs[..mid])),
            else_expr: Box::new(folding(&pairs[mid + 1..])),
        }),
    }
}
fn line_terminator(x: E) -> E {
    or(
        or(eq(x.clone(), n(10)), eq(x.clone(), n(13))),
        or(eq(x.clone(), n(0x2028)), eq(x, n(0x2029))),
    )
}
fn word(x: E) -> E {
    or(
        or(
            and(not(lt(x.clone(), n(48))), lt(x.clone(), n(58))),
            and(not(lt(x.clone(), n(65))), lt(x.clone(), n(91))),
        ),
        or(
            and(not(lt(x.clone(), n(97))), lt(x.clone(), n(123))),
            eq(x, n(95)),
        ),
    )
}
pub(crate) fn append(ir: &mut FrontendIr, p: &Pattern) -> Result<u16, ParseError> {
    let base =
        u16::try_from(ir.locals).map_err(|_| ParseError::new("RegExp local slot capacity"))?;
    let locals = ir
        .locals
        .checked_add(usize::from(H) + 2)
        .filter(|&n| n <= usize::from(u16::MAX) + 1)
        .ok_or_else(|| ParseError::new("RegExp local slot capacity"))?;
    let next = match ir.functions.iter().map(|f| f.index).max() {
        None => 0,
        Some(n) => n
            .checked_add(1)
            .ok_or_else(|| ParseError::new("RegExp function capacity"))?,
    };
    let fold = next;
    let matcher = next
        .checked_add(1)
        .ok_or_else(|| ParseError::new("RegExp function capacity"))?;
    let pairs = include_str!("../../../tests/fixtures/regexp/unicode-3.0-es5.txt")
        .lines()
        .filter_map(|l| {
            let mut s = l.split_whitespace();
            if s.next() != Some("U") {
                return None;
            }
            Some((
                i64::from_str_radix(s.next().unwrap(), 16).unwrap(),
                i64::from_str_radix(s.next().unwrap(), 16).unwrap(),
            ))
        })
        .collect::<Vec<_>>();
    declaration(
        ir,
        fold,
        &format!("__es5_regexp_{fold}_fold"),
        vec![H + 1],
        vec![T::Int],
        T::Int,
        vec![],
        folding(&pairs),
    );
    let caplen = (2 * (p.captures + 1)) as i64;
    let state = caplen + 2 * p.repeats as i64;
    let width = state + 3;
    let array = T::Array(Box::new(T::Int));
    let mut body = Vec::new();
    for slot in 4..=H {
        let (schema, expr) = if [ST, CODE, RANGES, CAPS].contains(&slot) {
            (array.clone(), builtin(B::ArrayNew, vec![]))
        } else if [FAIL, PUSH, OK].contains(&slot) {
            (T::Bool, E::Bool(false))
        } else {
            (T::Int, n(0))
        };
        body.push(S::Let {
            index: slot,
            declared_schema: Some(schema),
            expr,
            line: 1,
        });
    }
    for row in &p.code {
        for &x in row {
            body.push(push(CODE, n(x)))
        }
    }
    for &x in &p.ranges {
        body.push(push(RANGES, n(x)))
    }
    body.extend([
        assign(LENGTH, len(SUBJECT)),
        assign(STATUS, n(-1)),
        assign(SEARCH, v(START)),
        assign(POS, v(START)),
        assign(PC, n(p.entry)),
        assign(SP, n(state)),
    ]);
    body.push(branch(
        or(
            or(lt(v(BUDGET), n(0)), lt(n(TRANSITION_LIMIT), v(BUDGET))),
            or(lt(v(LIMIT), n(0)), lt(n(SCRATCH_LIMIT), v(LIMIT))),
        ),
        vec![assign(STATUS, n(4))],
        vec![],
    ));
    body.push(branch(
        lt(v(START), n(0)),
        vec![assign(STATUS, n(4))],
        vec![],
    ));
    body.push(branch(
        and(running(), lt(v(LIMIT), n(state + caplen))),
        vec![assign(STATUS, n(3))],
        vec![],
    ));
    // Invalid-domain status 4 stays 4. A still-running call records that the
    // initial state plus full capture output fits the selected scratch limit.
    body.push(branch(running(), vec![assign(RESERVED, n(1))], vec![]));
    body.push(wh(
        and(running(), lt(v(I), n(state))),
        vec![tick(vec![push(ST, n(-1)), inc(I)])],
    ));
    body.push(assign(I, n(0)));
    body.push(wh(
        and(running(), lt(v(I), v(LENGTH))),
        vec![tick(vec![
            assign(X, read(SUBJECT, v(I))),
            branch(
                or(lt(v(X), n(0)), lt(n(65535), v(X))),
                vec![assign(STATUS, n(4))],
                vec![],
            ),
            inc(I),
        ])],
    ));
    body.push(branch(
        and(running(), lt(v(LENGTH), v(START))),
        vec![assign(STATUS, n(0))],
        vec![],
    ));
    body.push(branch(running(), vec![put(ST, n(0), v(SEARCH))], vec![]));
    let restore = |first: i64| {
        vec![
            assign(I, n(first)),
            wh(
                and(running(), lt(v(I), n(state))),
                vec![tick(vec![
                    put(ST, v(I), read(ST, add(add(v(SP), n(3)), v(I)))),
                    inc(I),
                ])],
            ),
        ]
    };
    let mut pop = vec![
        assign(SP, sub(v(SP), n(width))),
        assign(KIND, read(ST, v(SP))),
        assign(CONT, read(ST, add(v(SP), n(1)))),
        assign(POS, read(ST, add(v(SP), n(2)))),
    ];
    pop.extend(restore(0));
    pop.extend([assign(PC, v(CONT)), assign(FAIL, eq(v(KIND), n(1)))]);
    let new_search = vec![
        inc(SEARCH),
        branch(
            lt(v(LENGTH), v(SEARCH)),
            vec![assign(STATUS, n(0))],
            vec![
                assign(I, n(0)),
                wh(
                    and(running(), lt(v(I), n(state))),
                    vec![tick(vec![put(ST, v(I), n(-1)), inc(I)])],
                ),
                put(ST, n(0), v(SEARCH)),
                assign(POS, v(SEARCH)),
                assign(PC, n(p.entry)),
                assign(FAIL, E::Bool(false)),
            ],
        ),
    ];
    let fail = vec![branch(eq(v(SP), n(state)), new_search, pop)];
    let mut ops: Vec<(i64, Vec<S>)> = vec![
        (0, vec![put(ST, n(1), v(POS)), assign(STATUS, n(1))]),
        (
            1,
            vec![
                assign(OK, E::Bool(false)),
                branch(
                    lt(v(POS), v(LENGTH)),
                    vec![
                        assign(X, read(SUBJECT, v(POS))),
                        assign(I, v(A)),
                        assign(J, add(v(A), mul(v(C), n(2)))),
                        wh(
                            and(and(running(), lt(v(I), v(J))), not(v(OK))),
                            vec![tick(vec![
                                assign(
                                    OK,
                                    and(
                                        not(lt(v(X), read(RANGES, v(I)))),
                                        not(lt(read(RANGES, add(v(I), n(1))), v(X))),
                                    ),
                                ),
                                assign(I, add(v(I), n(2))),
                            ])],
                        ),
                    ],
                    vec![],
                ),
                branch(
                    v(OK),
                    vec![inc(POS), assign(PC, v(NEXT))],
                    vec![assign(FAIL, E::Bool(true))],
                ),
            ],
        ),
        (2, vec![put(ST, v(A), v(POS)), assign(PC, v(NEXT))]),
        (
            3,
            vec![
                assign(PUSH, E::Bool(true)),
                assign(PUSH_KIND, n(0)),
                assign(PUSH_PC, v(A)),
                assign(PC, v(NEXT)),
            ],
        ),
        (4, vec![assign(PC, v(NEXT))]),
    ];
    let beginning = if p.flags.multiline {
        or(
            eq(v(POS), n(0)),
            and(
                lt(n(0), v(POS)),
                line_terminator(read(SUBJECT, sub(v(POS), n(1)))),
            ),
        )
    } else {
        eq(v(POS), n(0))
    };
    ops.push((
        5,
        vec![branch(
            beginning,
            vec![assign(PC, v(NEXT))],
            vec![assign(FAIL, E::Bool(true))],
        )],
    ));
    let ending = if p.flags.multiline {
        or(
            eq(v(POS), v(LENGTH)),
            and(
                lt(v(POS), v(LENGTH)),
                line_terminator(read(SUBJECT, v(POS))),
            ),
        )
    } else {
        eq(v(POS), v(LENGTH))
    };
    ops.push((
        6,
        vec![branch(
            ending,
            vec![assign(PC, v(NEXT))],
            vec![assign(FAIL, E::Bool(true))],
        )],
    ));
    let boundary = not(eq(
        and(lt(n(0), v(POS)), word(read(SUBJECT, sub(v(POS), n(1))))),
        and(lt(v(POS), v(LENGTH)), word(read(SUBJECT, v(POS)))),
    ));
    ops.push((
        7,
        vec![branch(
            not(eq(boundary, eq(v(A), n(1)))),
            vec![assign(PC, v(NEXT))],
            vec![assign(FAIL, E::Bool(true))],
        )],
    ));
    let canon = |x: E| {
        if p.flags.ignore_case {
            call(fold, vec![x])
        } else {
            x
        }
    };
    ops.push((
        8,
        vec![
            assign(I, read(ST, v(A))),
            assign(J, read(ST, add(v(A), n(1)))),
            assign(OK, E::Bool(true)),
            branch(
                and(not(lt(v(I), n(0))), not(lt(v(J), n(0)))),
                vec![wh(
                    and(and(running(), lt(v(I), v(J))), v(OK)),
                    vec![tick(vec![branch(
                        lt(v(POS), v(LENGTH)),
                        vec![
                            assign(
                                OK,
                                eq(canon(read(SUBJECT, v(I))), canon(read(SUBJECT, v(POS)))),
                            ),
                            inc(POS),
                            inc(I),
                        ],
                        vec![assign(OK, E::Bool(false))],
                    )])],
                )],
                vec![],
            ),
            branch(
                v(OK),
                vec![assign(PC, v(NEXT))],
                vec![assign(FAIL, E::Bool(true))],
            ),
        ],
    ));
    ops.push((
        9,
        vec![
            assign(PUSH, E::Bool(true)),
            assign(PUSH_KIND, v(A)),
            assign(PUSH_PC, v(C)),
            assign(PC, v(NEXT)),
        ],
    ));
    let mut look = vec![
        assign(KIND, n(0)),
        wh(
            and(and(running(), lt(n(state), v(SP))), eq(v(KIND), n(0))),
            vec![tick(vec![
                assign(SP, sub(v(SP), n(width))),
                assign(KIND, read(ST, v(SP))),
            ])],
        ),
    ];
    let mut negative = restore(0);
    negative.push(assign(FAIL, E::Bool(true)));
    let mut positive = restore(caplen);
    positive.push(assign(PC, v(CONT)));
    // Exhaustion while searching for the assertion marker leaves SP at the
    // end of scratch. Do not dereference that unfinished pop operation.
    look.push(branch(
        running(),
        vec![
            assign(CONT, read(ST, add(v(SP), n(1)))),
            assign(POS, read(ST, add(v(SP), n(2)))),
            branch(eq(v(KIND), n(2)), negative, positive),
        ],
        vec![],
    ));
    ops.push((10, look));
    ops.push((11, vec![put(ST, v(A), n(0)), assign(PC, v(NEXT))]));
    ops.push((
        12,
        vec![
            assign(COUNT, read(ST, v(A))),
            branch(
                and(not(lt(v(D), n(0))), not(lt(v(COUNT), v(D)))),
                vec![assign(PC, v(G))],
                vec![branch(
                    lt(v(COUNT), v(C)),
                    vec![assign(PC, v(NEXT))],
                    vec![
                        assign(PUSH, E::Bool(true)),
                        assign(PUSH_KIND, n(0)),
                        branch(
                            eq(v(F), n(1)),
                            vec![assign(PUSH_PC, v(G)), assign(PC, v(NEXT))],
                            vec![assign(PUSH_PC, v(NEXT)), assign(PC, v(G))],
                        ),
                    ],
                )],
            ),
        ],
    ));
    ops.push((
        13,
        vec![
            assign(COUNT, read(ST, v(A))),
            branch(
                and(
                    eq(v(POS), read(ST, add(v(A), n(1)))),
                    not(lt(v(COUNT), v(D))),
                ),
                vec![assign(FAIL, E::Bool(true))],
                vec![put(ST, v(A), add(v(COUNT), n(1))), assign(PC, v(C))],
            ),
        ],
    ));
    ops.push((
        14,
        vec![
            assign(I, v(A)),
            wh(
                and(running(), lt(v(I), v(C))),
                vec![tick(vec![put(ST, v(I), n(-1)), inc(I)])],
            ),
            put(ST, add(v(D), n(1)), v(POS)),
            assign(PC, v(NEXT)),
        ],
    ));
    fn dispatch(ops: &[(i64, Vec<S>)]) -> Vec<S> {
        if ops.is_empty() {
            return vec![assign(STATUS, n(4))];
        }
        let m = ops.len() / 2;
        vec![branch(
            eq(v(OP), n(ops[m].0)),
            ops[m].1.clone(),
            vec![branch(
                lt(v(OP), n(ops[m].0)),
                dispatch(&ops[..m]),
                dispatch(&ops[m + 1..]),
            )],
        )]
    }
    let mut execute = vec![assign(H, mul(v(PC), n(10))), assign(OP, read(CODE, v(H)))];
    for (slot, offset) in [(A, 1), (C, 2), (D, 3), (F, 4), (G, 5), (NEXT, 9)] {
        execute.push(assign(slot, read(CODE, add(v(H), n(offset)))))
    }
    execute.extend(dispatch(&ops));
    let push_frame = vec![
        branch(
            lt(v(LIMIT), add(add(v(SP), n(width)), n(caplen))),
            vec![assign(STATUS, n(3))],
            vec![
                write(v(SP), v(PUSH_KIND)),
                write(add(v(SP), n(1)), v(PUSH_PC)),
                write(add(v(SP), n(2)), v(POS)),
                assign(I, n(0)),
                wh(
                    and(running(), lt(v(I), n(state))),
                    vec![tick(vec![
                        write(add(add(v(SP), n(3)), v(I)), read(ST, v(I))),
                        inc(I),
                    ])],
                ),
                assign(SP, add(v(SP), n(width))),
            ],
        ),
        assign(PUSH, E::Bool(false)),
    ];
    body.push(wh(
        running(),
        vec![tick(vec![
            branch(v(FAIL), fail, execute),
            branch(and(running(), v(PUSH)), push_frame, vec![]),
        ])],
    ));
    // Capture output counts in mutable scratch high water. Emit the full caplen
    // pairs only when that reservation was admitted, input stayed valid, and
    // actual ST high water plus those cells still fits the original limit.
    // Invalid input and a refused reservation keep their status and an empty
    // captures array; they are not rewritten as match or no-match.
    body.push(assign(I, n(0)));
    body.push(wh(
        and(
            and(
                and(eq(v(RESERVED), n(1)), not(eq(v(STATUS), n(4)))),
                not(lt(v(LIMIT), add(len(ST), n(caplen)))),
            ),
            lt(v(I), n(caplen)),
        ),
        vec![
            branch(
                eq(v(STATUS), n(1)),
                vec![push(CAPS, read(ST, v(I)))],
                vec![push(CAPS, n(-1))],
            ),
            inc(I),
        ],
    ));
    let name = "__Es5RegExpKernelResult";
    let fields = [
        ("status", T::Int),
        ("start", T::Int),
        ("end", T::Int),
        ("search_start", T::Int),
        ("transitions", T::Int),
        ("captures", array.clone()),
    ];
    ir.struct_schemas.insert(
        name.into(),
        StructDecl {
            name: name.into(),
            type_params: vec![],
            body_schema: T::Object(fields.into_iter().map(|(k, v)| (k.into(), v)).collect()),
            origin: StructDeclOrigin::Guest,
        },
    );
    let matched = |a: E| E::IfElse {
        condition: Box::new(eq(v(STATUS), n(1))),
        then_expr: Box::new(a),
        else_expr: Box::new(n(-1)),
    };
    let result = [
        ("status", v(STATUS)),
        ("start", matched(v(SEARCH))),
        ("end", matched(v(POS))),
        ("search_start", v(START)),
        ("transitions", v(SPENT)),
        ("captures", v(CAPS)),
    ]
    .into_iter()
    .fold(builtin(B::MapNew, vec![]), |r, (k, x)| {
        builtin(B::Set, vec![r, E::String(k.into()), x])
    });
    declaration(
        ir,
        matcher,
        &format!("__es5_regexp_{matcher}_kernel"),
        vec![SUBJECT, START, BUDGET, LIMIT],
        vec![array, T::Int, T::Int, T::Int],
        T::Named(name.into(), vec![]),
        body,
        result,
    );
    // Relocate only our finite generated trees, never caller IR. Their maximum
    // nesting is fixed by this emitter and the logarithmic fixed fold table.
    fn shift_expr(e: &mut E, base: u16) {
        match e {
            E::Var(i) => *i += base,
            E::Call(_, _, args, _, _) => {
                for a in args {
                    shift_expr(a, base);
                }
            }
            E::Borrow(e) | E::Not(e) => shift_expr(e, base),
            E::Add(a, b)
            | E::Sub(a, b)
            | E::Mul(a, b)
            | E::Eq(a, b)
            | E::Lt(a, b)
            | E::And(a, b)
            | E::Or(a, b) => {
                shift_expr(a, base);
                shift_expr(b, base);
            }
            E::IfElse {
                condition,
                then_expr,
                else_expr,
            } => {
                shift_expr(condition, base);
                shift_expr(then_expr, base);
                shift_expr(else_expr, base);
            }
            E::Int(_) | E::Bool(_) | E::String(_) => {}
            _ => unreachable!("expression not emitted by RegExp kernel"),
        }
    }
    fn shift_stmts(stmts: &mut [S], base: u16) {
        for s in stmts {
            match s {
                S::Let { index, expr, .. } | S::Assign { index, expr, .. } => {
                    *index += base;
                    shift_expr(expr, base);
                }
                S::IfElse {
                    condition,
                    then_branch,
                    else_branch,
                    ..
                } => {
                    shift_expr(condition, base);
                    shift_stmts(then_branch, base);
                    shift_stmts(else_branch, base);
                }
                S::While {
                    condition, body, ..
                } => {
                    shift_expr(condition, base);
                    shift_stmts(body, base);
                }
                _ => unreachable!("statement not emitted by RegExp kernel"),
            }
        }
    }
    for index in [fold, matcher] {
        let implementation = ir.function_impls.get_mut(&index).unwrap();
        for slot in &mut implementation.param_slots {
            *slot += base;
        }
        shift_stmts(&mut implementation.body_stmts, base);
        shift_expr(&mut implementation.body_expr, base);
    }
    ir.locals = locals;
    Ok(matcher)
}

/// The public `/a/.test` slice: one completed-IR matcher, one private value
/// helper for the admitted literal, and one dispatch adapter.
///
/// The literal value is a Kind-RegExp object carrying exactly one own
/// non-enumerable `test` data row, so the unchanged ordinary Get finds the
/// real function node and the unchanged guest call machinery dispatches it.
/// No intrinsic object is installed: every admitted literal allocates its own
/// method and key in the caller's root frame, like the admitted `new Date(0)`.
/// The returned index is the kernel `append_matcher` actually appended, and
/// the caller records it so the gated `String.prototype.search` row calls
/// that exact function instead of recomputing it from `test_index`.
pub(crate) fn append_test_support(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    value_index: u16,
    test_index: u16,
) -> Result<u16, ParseError> {
    let to_units = lower.utf16_units_helper().ok_or_else(|| {
        lower.error(
            span,
            "the admitted regexp test requires the UTF-16 bridge",
        )
    })?;
    // The kernel reserves its own locals from the completed-IR high-water
    // mark, so both slot bounds must agree before and after it is appended.
    lower.reserve_private_locals(ir, span)?;
    let matcher = match super::super::regexp_lowering::append_matcher(ir, &[97u16], "") {
        Ok(matcher) => matcher,
        Err(error) => {
            return Err(lower.error(span, &format!("RegExp test kernel: {error}")));
        }
    };
    lower.reserve_private_locals(ir, span)?;
    // The realm's one `/a/` kernel is the shared specialisation both the
    // literal's `test` adapter and the gated `String.prototype.search` row
    // call. The private value helper takes the next index and the adapter
    // after it, so the kernel is exactly two below the test adapter; this
    // lowering-time check proves the contiguous window the helper reservation
    // relies on, while the actual returned index is handed to the caller.
    debug_assert_eq!(matcher.checked_add(2), Some(test_index));
    append_test_adapter(
        lower,
        ir,
        carrier_schema,
        span,
        stmts,
        test_index,
        matcher,
        to_units,
    )?;
    append_test_value(lower, ir, carrier_schema, span, stmts, value_index)?;
    Ok(matcher)
}

/// Admitted `/a/.test` behind dispatch code 274.
///
/// The function is found by the unchanged ordinary Get on the object the one
/// admitted `/a/` literal returned, so no member name is special-cased in the
/// lowerer. The adapter is handed the actual callee the dispatch already
/// captured, because evaluating the argument may add or overwrite the
/// receiver's own rows; it never re-reads a row to rediscover the method.
/// Only the admitted shape succeeds: exactly one actual argument whose value
/// is a String, and a callee that is the function node carrying code 274 on
/// the function prototype chain. The String's UTF-16 units come from the
/// existing managed bridge and the real guest kernel decides the match.
/// The code and the method value alone are copyable, so that captured
/// function must additionally carry a private environment pair naming the
/// owner: the checked `KIND_ENVIRONMENT` record holds exactly one Object
/// entry whose slot and generation must equal the actual receiver's. A copied
/// method therefore keeps the managed TypeError on a plain receiver.
/// Status 1 is true, status 0 is false, and the resource or domain statuses
/// 2..4 raise the managed TypeError: they are never reported as a non-match.
/// Everything else -- a borrowed function, an ordinary object receiver, any
/// other arity, or a foreign callee -- keeps that same TypeError.
fn append_test_adapter(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    index: u16,
    matcher: u16,
    to_units: u16,
) -> Result<(), ParseError> {
    let c = lower.slot(span)?;
    let code = lower.slot(span)?;
    let receiver = lower.slot(span)?;
    let args = lower.slot(span)?;
    // The boxed function the dispatch resolved by ordinary Get. This private
    // adapter signature extends by one argument; the public dispatch ABI
    // stays the same four parameters.
    let callee = lower.slot(span)?;
    let heap = lower.heap_ref(span)?.helpers;
    let intrinsic = lower.intrinsics.expect("intrinsics");
    let mut e = obj::Emit::new(lower, span, c)?;
    let l = e.line;
    let sp = e.lower.ir_span(span);
    let out = e.temp(
        value::logical_result_schema(),
        value::result_ok(value::completion_normal(), sp),
    )?;
    let frame = e.integer()?;
    let count = e.integer()?;
    let function_base = e.integer()?;
    let env_slot = e.integer()?;
    let env_generation = e.integer()?;
    let env_base = e.integer()?;
    let env_count = e.integer()?;
    let env_entry = e.integer()?;
    let env_tag = e.integer()?;
    let error = e.boxed()?;
    let string = e.boxed()?;
    let units = e.temp(
        T::Array(Box::new(T::Int)),
        obj::call(B::ArrayNew.call_index(), vec![]),
    )?;
    let kernel = e.lower.slot(span)?;
    let mut body = vec![assert_stmt(
        obj::eq(obj::var(code), obj::int(super::intrinsics::REGEXP_TEST_CODE)),
        l,
    )];
    body.push(obj::assign(
        frame,
        obj::call(heap.root_enter, vec![obj::var(c), obj::int(6)]),
        l,
    ));
    body.push(e.root(obj::var(frame), obj::int(0), obj::var(receiver))?);
    // The captured callee is rooted for the whole adapter: the caller may
    // already have overwritten or extended the receiver's own rows while
    // evaluating the argument, so this function is no longer discoverable
    // from the receiver and must not be re-read from those mutable rows.
    body.push(e.root(obj::var(frame), obj::int(3), obj::var(callee))?);
    let fail = vec![
        obj::assign(
            error,
            obj::call(intrinsic.type_error, vec![obj::var(c)]),
            l,
        ),
        e.root(obj::var(frame), obj::int(1), obj::var(error))?,
        obj::assign(out, value::result_err(obj::var(error), sp), l),
    ];
    // The captured callee is authenticated instead of the receiver's current
    // rows. It is the function node this literal published -- kind and
    // dispatch code -- and its own checked environment pair must name a
    // `KIND_ENVIRONMENT` record whose single Object entry names the actual
    // receiver's slot and generation. An argument that adds or overwrites
    // receiver rows therefore cannot break the brand, and a copied method on
    // an ordinary receiver still fails the owner identity check.
    let mut stored = vec![obj::assign(
        function_base,
        e.base(obj::var(callee)),
        l,
    )];
    let matched = argument_arm(
        &mut e, frame, args, count, string, out, units, kernel, matcher, to_units, sp, &fail,
    )?;
    // The function node's own private environment pair names the single
    // KIND_ENVIRONMENT record the literal's value helper published, and that
    // record's one boxed entry names the owner object's slot and generation.
    // A copied method keeps this same function node, so a receiver that only
    // copies the public key row and method still fails this identity check.
    let mut record = vec![
        obj::assign(
            env_slot,
            e.decoded(obj::add(obj::var(function_base), obj::int(6))),
            l,
        ),
        obj::assign(
            env_generation,
            e.generation(obj::add(obj::var(function_base), obj::int(7))),
            l,
        ),
    ];
    let mut owner = vec![
        obj::assign(
            env_count,
            e.decoded(obj::add(obj::var(env_base), obj::int(2))),
            l,
        ),
        obj::assign(
            env_entry,
            obj::add(obj::var(env_base), obj::int(layout::ENV_HEADER_CELLS)),
            l,
        ),
        obj::assign(env_tag, e.decoded(obj::var(env_entry)), l),
    ];
    owner.push(obj::branch(
        obj::and(
            obj::eq(obj::var(env_count), obj::int(1)),
            obj::eq(obj::var(env_tag), obj::int(value::OBJECT)),
        ),
        vec![obj::branch(
            obj::and(
                obj::eq(
                    obj::num(obj::field(obj::var(receiver), "ref_slot")),
                    obj::num(e.read(obj::add(obj::var(env_entry), obj::int(3)))),
                ),
                obj::eq(
                    obj::num(obj::field(obj::var(receiver), "generation")),
                    obj::num(e.read(obj::add(obj::var(env_entry), obj::int(4)))),
                ),
            ),
            matched,
            fail.clone(),
            l,
        )],
        fail.clone(),
        l,
    ));
    // The environment pair is only dereferenced once it is a live nonzero edge,
    // and the record kind and reserved header cells are checked before its
    // count and entry are read. Any mismatch keeps the managed TypeError, so a
    // foreign function never reaches an internal assertion.
    record.push(obj::branch(
        obj::and(
            obj::not(obj::eq(obj::var(env_generation), obj::int(0))),
            obj::and(
                obj::not(obj::lt(obj::var(env_slot), obj::int(0))),
                obj::lt(obj::var(env_slot), obj::int(layout::MAX_SLOTS)),
            ),
        ),
        vec![
            obj::assign(
                env_base,
                obj::call(
                    heap.checked_base,
                    vec![obj::var(c), obj::var(env_slot), obj::var(env_generation)],
                ),
                l,
            ),
            obj::branch(
                obj::and(
                    obj::eq(
                        e.decoded(obj::var(env_base)),
                        obj::int(layout::KIND_ENVIRONMENT),
                    ),
                    obj::and(
                        obj::eq(
                            e.decoded(obj::add(obj::var(env_base), obj::int(3))),
                            obj::int(0),
                        ),
                        obj::eq(
                            e.generation(obj::add(obj::var(env_base), obj::int(4))),
                            obj::int(0),
                        ),
                    ),
                ),
                owner,
                fail.clone(),
                l,
            ),
        ],
        fail.clone(),
        l,
    ));
    stored.push(obj::branch(
        obj::and(
            obj::eq(
                e.decoded(obj::var(function_base)),
                obj::int(layout::KIND_FUNCTION),
            ),
            obj::eq(
                e.decoded(obj::add(obj::var(function_base), obj::int(5))),
                obj::int(super::intrinsics::REGEXP_TEST_CODE),
            ),
        ),
        record,
        fail.clone(),
        l,
    ));
    // The callee value is tag-checked before its base is read, so anything
    // that is not an object keeps the managed TypeError instead of failing an
    // internal assertion. Its kind and code are then read inside the function
    // layout before the private environment pair is dereferenced.
    body.push(obj::branch(
        obj::tag_is(obj::var(callee), value::OBJECT),
        stored,
        fail,
        l,
    ));
    body.push(e.effect(heap.root_leave, vec![obj::var(c), obj::var(frame)])?);
    let body = e.finish(body);
    obj::declare(
        ir,
        stmts,
        "__compat_es5_regexp_test",
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
        obj::var(out),
        l,
    );
    Ok(())
}

/// One actual String argument, then the real kernel. The argument container is
/// read through the existing Array Get, the String is rooted before the bridge
/// allocates, and only the kernel's own status decides true/false.
#[allow(clippy::too_many_arguments)] // Mirrors the typed adapter's live locals.
fn argument_arm(
    e: &mut obj::Emit<'_, '_>,
    frame: u16,
    args: u16,
    count: u16,
    string: u16,
    out: u16,
    units: u16,
    kernel: u16,
    matcher: u16,
    to_units: u16,
    sp: vm::Span,
    fail: &[S],
) -> Result<Vec<S>, ParseError> {
    let l = e.line;
    let c = e.carrier;
    let mut single = Vec::new();
    single.push(obj::assign(
        string,
        obj::call(
            B::Get.call_index(),
            vec![E::Borrow(Box::new(obj::var(args))), obj::int(0)],
        ),
        l,
    ));
    let mut evaluate = vec![e.root(obj::var(frame), obj::int(2), obj::var(string))?];
    evaluate.push(obj::assign(
        units,
        obj::call(to_units, vec![obj::var(c), obj::var(string)]),
        l,
    ));
    evaluate.push(S::Let {
        index: kernel,
        declared_schema: None,
        expr: obj::call(
            matcher,
            vec![
                obj::var(units),
                obj::int(0),
                obj::int(TRANSITION_LIMIT),
                obj::int(SCRATCH_LIMIT),
            ],
        ),
        line: l,
    });
    // Status 1 is the match and status 0 the non-match. Both are the kernel's
    // own typed result; the quota and bad-input statuses 2..4 keep the managed
    // TypeError and are never collapsed into `false`.
    evaluate.push(obj::branch(
        obj::eq(
            obj::num(obj::field(obj::var(kernel), "status")),
            obj::num(obj::int(1)),
        ),
        vec![obj::assign(
            out,
            value::result_ok(value::completion_return(value::boolean(E::Bool(true))), sp),
            l,
        )],
        vec![obj::branch(
            obj::eq(
                obj::num(obj::field(obj::var(kernel), "status")),
                obj::num(obj::int(0)),
            ),
            vec![obj::assign(
                out,
                value::result_ok(value::completion_return(value::boolean(E::Bool(false))), sp),
                l,
            )],
            fail.to_vec(),
            l,
        )],
        l,
    ));
    single.push(obj::branch(
        obj::tag_is(obj::var(string), value::STRING),
        evaluate,
        fail.to_vec(),
        l,
    ));
    Ok(vec![
        obj::assign(
            count,
            obj::call(
                B::Len.call_index(),
                vec![E::Borrow(Box::new(obj::var(args)))],
            ),
            l,
        ),
        obj::branch(
            obj::eq(obj::var(count), obj::int(1)),
            single,
            fail.to_vec(),
            l,
        ),
    ])
}

/// One admitted `/a/` literal value.
///
/// The result is a real object node whose private kind is
/// `KIND_REGEXP` and whose single own data row is the non-enumerable
/// four-unit `test` key and the real function node carrying dispatch code
/// 274. That private kind is the `String.prototype.search` brand: guest code
/// can rewrite own rows but never cell 0 of a node header. The method also
/// owns one private checked `KIND_ENVIRONMENT` record whose single boxed
/// entry names this same object's slot and generation, which is the identity
/// the `.test` adapter authenticates. The object, its method, its key and
/// that record are ordinary guest nodes published into this helper's own
/// root frame: nothing is rooted through a Core array, no permanent intrinsic
/// object is installed, and no `source`/`flags`/`exec`/`lastIndex` surface is
/// admitted.
fn append_test_value(
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
    let mut e = obj::Emit::new(lower, span, c)?;
    let l = e.line;
    // Entry 0 keeps the four-unit `test` key alive across the method
    // reservation, entry 1 keeps the method alive across the object
    // reservation, and entry 2 keeps the object alive while its row is
    // written, while entry 3 holds the published private environment record.
    let object_proto = obj::call(intrinsic.root, vec![obj::var(c), obj::int(0)]);
    let function_proto = obj::call(intrinsic.root, vec![obj::var(c), obj::int(1)]);
    let method = e.boxed()?;
    let object = e.boxed()?;
    let frame = e.integer()?;
    let mut body = vec![obj::assign(
        frame,
        obj::call(heap.root_enter, vec![obj::var(c), obj::int(7)]),
        l,
    )];
    let test = super::intrinsics::string(&mut e, "test", frame, 0, &mut body)?;
    body.push(obj::assign(
        method,
        obj::call(
            intrinsic.allocate,
            vec![
                obj::var(c),
                obj::int(layout::KIND_FUNCTION),
                function_proto,
                obj::int(1),
                obj::int(super::intrinsics::REGEXP_TEST_CODE),
                obj::int(1),
                obj::int(0),
                obj::var(frame),
                obj::int(1),
            ],
        ),
        l,
    ));
    body.push(obj::assign(
        object,
        obj::call(
            intrinsic.allocate,
            vec![
                obj::var(c),
                obj::int(layout::KIND_REGEXP),
                object_proto,
                obj::int(1),
                obj::int(0),
                obj::int(0),
                obj::int(0),
                obj::var(frame),
                obj::int(2),
            ],
        ),
        l,
    ));
    super::intrinsics::trusted_data(
        &mut e,
        obj::var(object),
        0,
        obj::var(test),
        obj::var(method),
        (true, false, true),
        &mut body,
    )?;
    // Private receiver brand, built only after the owner object carries its
    // complete valid own row: one checked KIND_ENVIRONMENT node stores this
    // same object's slot and generation in its single boxed entry. The owner
    // and its method stay rooted across the environment reservation, so every
    // collection point sees either the empty function environment pair or the
    // complete ENV -> owner cycle, never a partially initialized object row.
    let allocation = e.temp(
        value::allocation_schema(),
        value::allocation(obj::int(0), obj::int(0), obj::int(0)),
    )?;
    let env_slot = e.integer()?;
    let env_generation = e.integer()?;
    let env_base = e.integer()?;
    body.push(obj::assign(
        allocation,
        obj::call(
            heap.node_begin,
            vec![
                obj::var(c),
                obj::int(layout::ENV_HEADER_CELLS + layout::ENV_CELLS_PER_ENTRY),
                obj::int(layout::KIND_ENVIRONMENT),
            ],
        ),
        l,
    ));
    for (target, name) in [
        (env_slot, "slot"),
        (env_generation, "generation"),
        (env_base, "base"),
    ] {
        body.push(obj::assign(
            target,
            obj::field(obj::var(allocation), name),
            l,
        ));
    }
    let env_entry = e.integer()?;
    body.push(obj::assign(
        env_entry,
        obj::add(
            obj::var(env_base),
            obj::int(layout::ENV_HEADER_CELLS),
        ),
        l,
    ));
    // Header count one, reserved scalar and generation cells zero: the same
    // shape the existing full-value environment consumers authenticate.
    for (offset, payload) in [
        (2, obj::int(1)),
        (3, obj::int(0)),
        (4, obj::int(0)),
    ] {
        body.push(e.write(
            obj::add(obj::var(env_base), obj::int(offset)),
            payload,
        )?);
    }
    // One Object-tagged entry naming the owner's current slot and generation.
    for (offset, payload) in [
        (0, obj::int(value::OBJECT)),
        (1, E::Float(0.0)),
        (2, obj::int(0)),
        (3, obj::field(obj::var(object), "ref_slot")),
        (4, obj::field(obj::var(object), "generation")),
    ] {
        body.push(e.write(
            obj::add(obj::var(env_entry), obj::int(offset)),
            payload,
        )?);
    }
    body.push(e.effect(
        heap.node_publish_edge,
        vec![
            obj::var(c),
            obj::var(env_slot),
            obj::var(env_generation),
            obj::var(env_base),
            obj::var(frame),
            obj::int(3),
            obj::int(layout::ROOT_INTERNAL),
        ],
    )?);
    // Patch the method's own checked environment pair last, after the record
    // is published and bounded by the owner cycle. A failed brand check leaves
    // the pair empty, so the adapter reports the managed TypeError instead of
    // trusting an uninitialized carrier.
    let method_base = e.integer()?;
    body.push(obj::assign(
        method_base,
        e.base(obj::var(method)),
        l,
    ));
    body.push(obj::branch(
        obj::and(
            obj::and(
                obj::eq(
                    e.decoded(obj::var(method_base)),
                    obj::int(layout::KIND_FUNCTION),
                ),
                obj::eq(
                    e.decoded(obj::add(obj::var(method_base), obj::int(5))),
                    obj::int(super::intrinsics::REGEXP_TEST_CODE),
                ),
            ),
            obj::and(
                obj::eq(
                    e.decoded(obj::add(obj::var(method_base), obj::int(6))),
                    obj::int(0),
                ),
                obj::eq(
                    e.generation(obj::add(obj::var(method_base), obj::int(7))),
                    obj::int(0),
                ),
            ),
        ),
        vec![
            e.write(
                obj::add(obj::var(method_base), obj::int(6)),
                obj::var(env_slot),
            )?,
            e.write(
                obj::add(obj::var(method_base), obj::int(7)),
                obj::var(env_generation),
            )?,
        ],
        vec![],
        l,
    ));
    body.push(e.effect(heap.root_leave, vec![obj::var(c), obj::var(frame)])?);
    let body = e.finish(body);
    obj::declare(
        ir,
        stmts,
        "__compat_es5_regexp_test_value",
        index,
        vec![c],
        vec![carrier_schema.clone()],
        value::schema(),
        body,
        obj::var(object),
        l,
    );
    Ok(())
}

