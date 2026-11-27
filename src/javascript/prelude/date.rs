//! Deterministic ECMA-262 5.1 §15.9.1 private Number arithmetic.
//! Rust emits typed guest IR only. No host date, clock, timezone, or coercion.
#![allow(dead_code)] // Private prerequisite, deliberately not registered as public Date.
use vm::compiler::ir::{FunctionDecl, FunctionImpl, StructDecl, StructDeclOrigin};
use vm::{
    BuiltinFunction as B, Expr as E, FrontendIr, ParseError, Stmt as S, TypeSchema as T, ValueType,
};

#[derive(Clone)]
pub(in crate::javascript) struct Entries {
    pub time_clip: u16,
    pub make_time: u16,
    pub make_day: u16,
    pub make_date: u16,
    pub utc: u16,
    pub parts: u16,
    pub compose: u16,
    pub exact: super::date_exact::Entries,
}
fn n(x: f64) -> E {
    E::Float(x)
}
fn v(x: u16) -> E {
    E::Var(x)
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
fn div(a: E, b: E) -> E {
    E::Div(Box::new(a), Box::new(b))
}
fn lt(a: E, b: E) -> E {
    E::Lt(Box::new(a), Box::new(b))
}
fn eq(a: E, b: E) -> E {
    E::Eq(Box::new(a), Box::new(b))
}
fn not(a: E) -> E {
    E::Not(Box::new(a))
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
fn floor(x: E) -> E {
    builtin(B::MathFloor, vec![x])
}
fn trunc(x: E) -> E {
    builtin(B::MathTrunc, vec![mul(x, n(1.0))])
}
fn finite(x: E) -> E {
    builtin(B::MathIsFinite, vec![x])
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
fn modulo(x: E, d: f64) -> E {
    // Remainder first avoids cancellation in x - floor(x/d)*d for huge x.
    let r = E::Mod(Box::new(x), Box::new(n(d)));
    add(
        choose(lt(r.clone(), n(0.0)), add(r.clone(), n(d)), r),
        n(0.0),
    )
}
fn day_from_year(y: E) -> E {
    add(
        sub(
            add(
                mul(n(365.0), sub(y.clone(), n(1970.0))),
                floor(div(sub(y.clone(), n(1969.0)), n(4.0))),
            ),
            floor(div(sub(y.clone(), n(1901.0)), n(100.0))),
        ),
        floor(div(sub(y, n(1601.0)), n(400.0))),
    )
}
fn leap(y: E) -> E {
    choose(
        and(
            eq(modulo(y.clone(), 4.0), n(0.0)),
            or(
                not(eq(modulo(y.clone(), 100.0), n(0.0))),
                eq(modulo(y, 400.0), n(0.0)),
            ),
        ),
        n(1.0),
        n(0.0),
    )
}
fn month_start(month: E, leap: E) -> E {
    // §15.9.1.4 cumulative first-day offsets, zero-based month.
    let starts = [
        0.0, 31.0, 59.0, 90.0, 120.0, 151.0, 181.0, 212.0, 243.0, 273.0, 304.0, 334.0,
    ];
    starts
        .iter()
        .enumerate()
        .rev()
        .fold(n(0.0), |tail, (m, &d)| {
            choose(
                eq(month.clone(), n(m as f64)),
                if m < 2 { n(d) } else { add(n(d), leap.clone()) },
                tail,
            )
        })
}
struct Build<'a> {
    ir: &'a mut FrontendIr,
    index: u16,
    name: &'static str,
    params: Vec<u16>,
    arg_schemas: Vec<T>,
    body: Vec<S>,
}
impl<'a> Build<'a> {
    fn new(ir: &'a mut FrontendIr, index: u16, name: &'static str, arity: usize) -> Self {
        let params = (0..arity)
            .map(|_| {
                let s = ir.locals as u16;
                ir.locals += 1;
                s
            })
            .collect();
        Self {
            ir,
            index,
            name,
            params,
            arg_schemas: vec![T::Number; arity],
            body: vec![],
        }
    }
    fn p(&self, i: usize) -> E {
        v(self.params[i])
    }
    fn local(&mut self, schema: T, expr: E) -> u16 {
        let index = self.ir.locals as u16;
        self.ir.locals += 1;
        self.body.push(S::Let {
            index,
            declared_schema: Some(schema),
            expr,
            line: 1,
        });
        index
    }
    fn finish(self, schema: T, expr: E) {
        let numeric = schema == T::Number;
        self.ir.functions.push(FunctionDecl {
            name: format!("__es5_date_{}_{}", self.index, self.name),
            arity: self.params.len() as u8,
            index: self.index,
            args: (0..self.params.len()).map(|i| format!("p{i}")).collect(),
            arg_schemas: self.arg_schemas.into_iter().map(Some).collect(),
            return_schema: Some(schema),
            type_params: vec![],
            exported: false,
            return_type: if numeric {
                ValueType::Float
            } else {
                ValueType::Map
            },
            symbol: None,
        });
        self.ir.function_impls.insert(
            self.index,
            FunctionImpl {
                param_slots: self.params,
                capture_copies: vec![],
                body_stmts: self.body,
                body_expr: expr,
                body_expr_line: 1,
            },
        );
    }
}
/// Append-only: existing caller slots, functions, captures and schemas are untouched.
/// Results normalize TimeClip zero to +0 (the second choice in §15.9.1.14).
pub(in crate::javascript) fn append(ir: &mut FrontendIr) -> Result<Entries, ParseError> {
    // Reserve a conservative bound before any u16 conversion. Emit into a
    // transactional clone so any collision/capacity refusal preserves input IR.
    ir.locals
        .checked_add(256)
        .filter(|&n| n <= usize::from(u16::MAX) + 1)
        .ok_or_else(|| ParseError::new("Date local capacity"))?;
    let mut candidate = ir.clone();
    let entries = append_inner(&mut candidate)?;
    *ir = candidate;
    Ok(entries)
}
fn append_inner(ir: &mut FrontendIr) -> Result<Entries, ParseError> {
    let exact = super::date_exact::append(ir)?;
    let next = ir
        .functions
        .iter()
        .map(|d| d.index)
        .chain(ir.function_impls.keys().copied())
        .max()
        .map_or(Some(0), |n| n.checked_add(1))
        .ok_or_else(|| ParseError::new("Date function capacity"))?;
    next.checked_add(6)
        .ok_or_else(|| ParseError::new("Date function capacity"))?;
    // The outer reservation also covers the seven Date entry declarations.
    ir.locals
        .checked_add(64)
        .filter(|&n| n <= usize::from(u16::MAX) + 1)
        .ok_or_else(|| ParseError::new("Date local capacity"))?;
    if ir
        .struct_schemas
        .contains_key(&format!("__Es5UtcParts_{}", next + 5))
    {
        return Err(ParseError::new("Date result schema collision"));
    }
    let entries = Entries {
        time_clip: next,
        make_time: next + 1,
        make_day: next + 2,
        make_date: next + 3,
        utc: next + 4,
        parts: next + 5,
        compose: next + 6,
        exact,
    };
    let b = Build::new(ir, entries.time_clip, "time_clip", 1);
    let x = b.p(0);
    let expr = choose(
        and(
            finite(x.clone()),
            not(lt(n(8.64e15), builtin(B::MathAbs, vec![x.clone()]))),
        ),
        add(trunc(x), n(0.0)),
        n(f64::NAN),
    );
    b.finish(T::Number, expr);
    {
        let b = Build::new(ir, entries.make_time, "make_time", 4);
        let valid = (0..4).map(|i| finite(b.p(i))).reduce(and).unwrap();
        let time = add(
            add(
                add(
                    mul(trunc(b.p(0)), n(3_600_000.0)),
                    mul(trunc(b.p(1)), n(60_000.0)),
                ),
                mul(trunc(b.p(2)), n(1000.0)),
            ),
            trunc(b.p(3)),
        );
        b.finish(T::Number, choose(valid, time, n(f64::NAN)));
    }
    {
        let e = &entries.exact;
        let mut b = Build::new(ir, entries.make_day, "make_day", 3);
        let y = b.local(e.schema.clone(), e.decode_number(trunc(b.p(0))));
        let m = b.local(e.schema.clone(), e.decode_number(trunc(b.p(1))));
        let dt = b.local(e.schema.clone(), e.decode_number(trunc(b.p(2))));
        let ym = b.local(e.schema.clone(), e.sum(v(y), e.floor(v(m), 12)));
        let mn = b.local(T::Int, e.residue(v(m), 12));
        let constant = |x: f64| e.decode_number(n(x));
        let minus = |x: E, c: f64| e.sum(x, e.neg(constant(c)));
        let first = b.local(
            e.schema.clone(),
            e.sum(
                e.sum(
                    e.times(minus(v(ym), 1970.0), 365),
                    e.floor(minus(v(ym), 1969.0), 4),
                ),
                e.sum(
                    e.neg(e.floor(minus(v(ym), 1901.0), 100)),
                    e.floor(minus(v(ym), 1601.0), 400),
                ),
            ),
        );
        let is_leap = and(
            eq(e.residue(v(ym), 4), E::Int(0)),
            or(
                not(eq(e.residue(v(ym), 100), E::Int(0))),
                eq(e.residue(v(ym), 400), E::Int(0)),
            ),
        );
        let offset = b.local(
            T::Number,
            month_start(mul(v(mn), n(1.0)), choose(is_leap, n(1.0), n(0.0))),
        );
        // Parent-selected mathematical Gregorian first-of-month witness.
        // No binary64 precision-gap search or Date-object year rejection.
        let result = e.sum(
            e.sum(v(first), e.decode_number(v(offset))),
            minus(v(dt), 1.0),
        );
        b.finish(e.schema.clone(), result);
    }
    {
        let e = &entries.exact;
        let mut b = Build::new(ir, entries.compose, "compose", 2);
        b.arg_schemas[0] = e.schema.clone();
        let result = e.sum(e.times(b.p(0), 86_400_000), e.decode_number(b.p(1)));
        b.finish(e.schema.clone(), result);
    }
    {
        let e = &entries.exact;
        let b = Build::new(ir, entries.make_date, "make_date", 2);
        let result = call(entries.compose, vec![e.decode_number(b.p(0)), b.p(1)]);
        b.finish(e.schema.clone(), result);
    }
    {
        let b = Build::new(ir, entries.utc, "utc", 7);
        let day = call(entries.make_day, (0..3).map(|i| b.p(i)).collect());
        let time = call(entries.make_time, (3..7).map(|i| b.p(i)).collect());
        b.finish(
            T::Number,
            call(
                entries.time_clip,
                vec![entries.exact.number(call(entries.compose, vec![day, time]))],
            ),
        );
    }
    let name = format!("__Es5UtcParts_{}", entries.parts);
    let fields = [
        "year",
        "month",
        "date",
        "weekday",
        "hour",
        "minute",
        "second",
        "millisecond",
    ];
    let schema = T::Named(name.clone(), vec![]);
    ir.struct_schemas.insert(
        name.clone(),
        StructDecl {
            name,
            type_params: vec![],
            body_schema: T::Object(
                std::iter::once(("valid".into(), T::Bool))
                    .chain(fields.iter().map(|k| ((*k).into(), T::Number)))
                    .collect(),
            ),
            origin: StructDeclOrigin::Guest,
        },
    );
    let mut b = Build::new(ir, entries.parts, "parts", 1);
    // This callable decomposes a Date time value: TimeClip first, then UTC.
    let t = b.local(T::Number, call(entries.time_clip, vec![b.p(0)]));
    let valid = b.local(T::Bool, finite(v(t)));
    let day = b.local(T::Number, floor(div(v(t), n(86_400_000.0))));
    // A year contains at least one day. These brackets therefore contain
    // every clipped year, by the ±100,000,000 day contract, without any
    // arbitrary product year limit. Binary search takes at most 28 steps.
    let lo = b.local(T::Number, n(1970.0 - 100_000_000.0));
    let hi = b.local(T::Number, n(1970.0 + 100_000_001.0));
    let mid = b.local(T::Number, n(0.0));
    b.body.push(S::While {
        condition: and(v(valid), lt(n(1.0), sub(v(hi), v(lo)))),
        body: vec![
            assign(mid, floor(div(add(v(lo), v(hi)), n(2.0)))),
            branch(
                not(lt(v(day), day_from_year(v(mid)))),
                vec![assign(lo, v(mid))],
                vec![assign(hi, v(mid))],
            ),
        ],
        line: 1,
    });
    let within = b.local(T::Number, sub(v(day), day_from_year(v(lo))));
    let is_leap = b.local(T::Number, leap(v(lo)));
    let month = b.local(T::Number, n(0.0));
    for m in 1..12 {
        b.body.push(branch(
            not(lt(v(within), month_start(n(m as f64), v(is_leap)))),
            vec![assign(month, n(m as f64))],
            vec![],
        ));
    }
    let values = [
        v(lo),
        v(month),
        add(sub(v(within), month_start(v(month), v(is_leap))), n(1.0)),
        modulo(add(v(day), n(4.0)), 7.0),
        modulo(floor(div(v(t), n(3_600_000.0))), 24.0),
        modulo(floor(div(v(t), n(60_000.0))), 60.0),
        modulo(floor(div(v(t), n(1000.0))), 60.0),
        modulo(v(t), 1000.0),
    ];
    let expr = std::iter::once(("valid", v(valid)))
        .chain(
            fields
                .into_iter()
                .zip(values)
                .map(|(k, x)| (k, choose(v(valid), x, n(f64::NAN)))),
        )
        .fold(builtin(B::MapNew, vec![]), |r, (k, x)| {
            builtin(B::Set, vec![r, E::String(k.into()), x])
        });
    b.finish(schema, expr);
    Ok(entries)
}
