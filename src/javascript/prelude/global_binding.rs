//! Nonallocating full-value ENV consumers and authenticated private Int transport.
use super::super::lowerer::{Lowerer, assert_stmt};
use super::super::{heap as h, representation as v};
use super::heap::fresh_index;
use super::object::*;
use vm::{Expr as E, FrontendIr, ParseError, Stmt as S, TypeSchema as T};

const RADIX: i64 = 4096;
const MAX: i64 = 17_592_186_044_415;
#[derive(Clone, Copy)]
pub(crate) struct Helpers {
    pub pack: u16,
    pub slot: u16,
    pub generation: u16,
    pub checked: u16,
    pub get: u16,
    pub set: u16,
}
fn mul(a: E, b: E) -> E {
    E::Mul(Box::new(a), Box::new(b))
}
fn div(a: E, b: E) -> E {
    E::Div(Box::new(a), Box::new(b))
}
fn range(x: E, max: i64, l: u32) -> S {
    assert_stmt(and(not(lt(x.clone(), int(0))), not(lt(int(max), x))), l)
}
fn pair(
    e: &mut Emit<'_, '_>,
    slot: u16,
    generation: u16,
    body: &mut Vec<S>,
) -> Result<u16, ParseError> {
    let l = e.line;
    body.extend([
        range(var(slot), RADIX - 1, l),
        range(var(generation), 4_294_967_295, l),
        assert_stmt(lt(int(0), var(generation)), l),
    ]);
    let base = e.integer()?;
    body.push(assign(
        base,
        call(
            e.checked_base,
            vec![var(e.carrier), var(slot), var(generation)],
        ),
        l,
    ));
    body.extend([
        assert_stmt(eq(e.decoded(var(base)), int(h::KIND_ENVIRONMENT)), l),
        assert_stmt(
            eq(
                e.decoded(add(var(base), int(2))),
                int(e.lower.globals.count()),
            ),
            l,
        ),
        assert_stmt(eq(e.decoded(add(var(base), int(3))), int(0)), l),
        assert_stmt(eq(e.generation(add(var(base), int(4))), int(0)), l),
        assert_stmt(
            not(lt(
                e.decoded(sub(var(base), int(h::RAW_HEADER_CELLS))),
                int(h::RAW_HEADER_CELLS
                    + h::ENV_HEADER_CELLS
                    + h::ENV_CELLS_PER_ENTRY * e.lower.globals.count()),
            )),
            l,
        ),
    ]);
    Ok(base)
}
pub(crate) fn append(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
) -> Result<Helpers, ParseError> {
    let l = lower.line(span);
    let mut split = Vec::new();
    for want_slot in [true, false] {
        let id = fresh_index(lower, ir, span)?;
        let x = lower.slot(span)?;
        let g = lower.slot(span)?;
        let s = lower.slot(span)?;
        let body = vec![
            range(var(x), MAX, l),
            S::Let {
                index: g,
                declared_schema: Some(T::Int),
                expr: div(var(x), int(RADIX)),
                line: l,
            },
            S::Let {
                index: s,
                declared_schema: Some(T::Int),
                expr: sub(var(x), mul(var(g), int(RADIX))),
                line: l,
            },
            range(var(s), RADIX - 1, l),
            range(var(g), 4_294_967_295, l),
            assert_stmt(or(not(eq(var(g), int(0))), eq(var(s), int(0))), l),
        ];
        declare(
            ir,
            stmts,
            if want_slot {
                "__compat_es5_global_env_slot"
            } else {
                "__compat_es5_global_env_generation"
            },
            id,
            vec![x],
            vec![T::Int],
            T::Int,
            body,
            var(if want_slot { s } else { g }),
            l,
        );
        split.push(id);
    }
    let slot = split[0];
    let generation = split[1];
    let pack = fresh_index(lower, ir, span)?;
    let c = lower.slot(span)?;
    let s = lower.slot(span)?;
    let g = lower.slot(span)?;
    let mut e = Emit::new(lower, span, c)?;
    let mut body = Vec::new();
    pair(&mut e, s, g, &mut body)?;
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        "__compat_es5_global_env_pack",
        pack,
        vec![c, s, g],
        vec![carrier_schema.clone(), T::Int, T::Int],
        T::Int,
        body,
        add(mul(var(g), int(RADIX)), var(s)),
        l,
    );
    let checked = fresh_index(lower, ir, span)?;
    let c = lower.slot(span)?;
    let p = lower.slot(span)?;
    let mut e = Emit::new(lower, span, c)?;
    let s = e.integer()?;
    let g = e.integer()?;
    let mut body = vec![
        assign(s, call(slot, vec![var(p)]), l),
        assign(g, call(generation, vec![var(p)]), l),
    ];
    let base = pair(&mut e, s, g, &mut body)?;
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        "__compat_es5_global_env_checked",
        checked,
        vec![c, p],
        vec![carrier_schema.clone(), T::Int],
        T::Int,
        body,
        var(base),
        l,
    );
    let mut consumers = Vec::new();
    for store in [false, true] {
        let id = fresh_index(lower, ir, span)?;
        let c = lower.slot(span)?;
        let p = lower.slot(span)?;
        let entry = lower.slot(span)?;
        let payload = if store { Some(lower.slot(span)?) } else { None };
        let mut e = Emit::new(lower, span, c)?;
        let base = e.integer()?;
        let at = e.integer()?;
        let tag = e.integer()?;
        let number = e.temp(T::Float, E::Float(0.0))?;
        let flag = e.integer()?;
        let rs = e.integer()?;
        let rg = e.integer()?;
        let managed = e.integer()?;
        let mut body = vec![
            assign(base, call(checked, vec![var(c), var(p)]), l),
            range(var(entry), e.lower.globals.count() - 1, l),
            assign(
                at,
                add(
                    add(var(base), int(h::ENV_HEADER_CELLS)),
                    mul(var(entry), int(h::ENV_CELLS_PER_ENTRY)),
                ),
                l,
            ),
        ];
        if let Some(payload) = payload {
            body.extend([
                assign(tag, v::field(payload, "tag"), l),
                range(var(tag), v::OBJECT, l),
                assign(number, v::field(payload, "number"), l),
                assign(flag, choose(v::field(payload, "flag"), int(1), int(0)), l),
                assign(rs, v::field(payload, "ref_slot"), l),
                assign(rg, v::field(payload, "generation"), l),
            ]);
        } else {
            let loaded = e.lower.slot(span)?;
            let result = e.lower.slot(span)?;
            // The B1 read schema is Number; Type(Float) checks physical storage
            // without narrowing its logical schema. Existing pure math::copysign
            // returns Float by contract. Identical already-loaded Float operands
            // preserve every bit, including signed zero and NaN payload/sign.
            // Never admit its Int conversion path. Shared callers must grant this
            // existing builtin; no public Math member or host store is added.
            let number_read = E::Match {
                value_slot: loaded,
                result_slot: result,
                value: Box::new(call(e.read, vec![var(c), num(add(var(at), int(1)))])),
                arms: vec![(
                    vm::compiler::ir::MatchPattern::Type(vm::compiler::ir::MatchTypePattern::Float),
                    E::Call(
                        vm::BuiltinFunction::MathCopySign.call_index(),
                        Vec::new(),
                        vec![var(loaded), var(loaded)],
                        None,
                        None,
                    ),
                )],
                default: Box::new(E::Block {
                    stmts: vec![assert_stmt(E::Bool(false), l)],
                    expr: Box::new(E::Float(0.0)),
                }),
            };
            body.extend([
                assign(tag, e.decoded(var(at)), l),
                range(var(tag), v::OBJECT, l),
                assign(number, number_read, l),
                assign(flag, e.decoded(add(var(at), int(2))), l),
                range(var(flag), 1, l),
                assign(rs, e.decoded(add(var(at), int(3))), l),
                assign(rg, e.generation(add(var(at), int(4))), l),
            ]);
        }
        let managed_body = vec![
            range(var(rs), RADIX - 1, l),
            assign(
                managed,
                call(e.checked_base, vec![var(c), var(rs), var(rg)]),
                l,
            ),
            assign(managed, e.decoded(var(managed)), l),
            assert_stmt(
                choose(
                    eq(var(tag), int(v::STRING)),
                    eq(var(managed), int(h::KIND_STRING)),
                    super::object::object_like(var(managed)),
                ),
                l,
            ),
        ];
        body.push(branch(
            or(eq(var(tag), int(v::STRING)), eq(var(tag), int(v::OBJECT))),
            managed_body,
            vec![assert_stmt(
                and(eq(var(rs), int(0)), eq(var(rg), int(0))),
                l,
            )],
            l,
        ));
        let result = if store {
            for (offset, x) in [var(tag), var(number), var(flag), var(rs), var(rg)]
                .into_iter()
                .enumerate()
            {
                if offset == 1 {
                    let sink = e.temp(T::Number, E::Float(0.0))?;
                    body.push(assign(
                        sink,
                        call(e.write, vec![var(c), num(add(var(at), int(1))), x]),
                        l,
                    ));
                } else {
                    body.push(e.write(add(var(at), int(offset as i64)), x)?);
                }
            }
            int(0)
        } else {
            v::dynamic_value(
                var(tag),
                var(number),
                eq(var(flag), int(1)),
                var(rs),
                var(rg),
            )
        };
        let body = e.finish(body);
        let mut params = vec![c, p, entry];
        let mut schemas = vec![carrier_schema.clone(), T::Int, T::Int];
        if let Some(payload) = payload {
            params.push(payload);
            schemas.push(v::schema());
        }
        declare(
            ir,
            stmts,
            if store {
                "__compat_es5_global_env_set"
            } else {
                "__compat_es5_global_env_get"
            },
            id,
            params,
            schemas,
            if store { T::Int } else { v::schema() },
            body,
            result,
            l,
        );
        consumers.push(id);
    }
    Ok(Helpers {
        pack,
        slot,
        generation,
        checked,
        get: consumers[0],
        set: consumers[1],
    })
}
