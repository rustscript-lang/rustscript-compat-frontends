//! Narrow source seam from a parsed Lua chunk onto the binary64 number kernel.
//!
//! Only a grammar kind that is already binary64, or a local whose initializer
//! this walk proved, may receive [`FloatTypeEvidence`]. `LL`, `ULL`, imaginary
//! suffixes, and names this walk did not prove stay outside that token.
//! Chains are hoisted with the kernel's temps so a flat source spine does not
//! become an unbounded Core expression. Slot reuse stays with Core.

use super::ast::{BinOp, Chunk, Expr, ExprList, LocalName, NumberKind, Span, Stmt, UnOp};
use super::number::{ClassifiedNumber, FloatTypeEvidence, NumberLowering};
use super::resolve::Resolution;
use vm::{Expr as CoreExpr, FrontendIr, LocalIrBuilder, LocalSlot, ParseError, Stmt as CoreStmt};

pub(crate) struct LoweredNumberChunk {
    pub(crate) ir: FrontendIr,
    pub(crate) logical_slots: usize,
    pub(crate) binding_names: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NumberSourceError {
    Syntax { message: String },
    CompileUnsupported { reason: String },
    NotFloat { kind: &'static str },
}

struct ProvenLocal {
    name: Vec<u8>,
    slot: LocalSlot,
}

pub(crate) fn lower_proven_chunk(
    source: &str,
    chunk: &Chunk,
    resolution: &Resolution,
) -> Result<LoweredNumberChunk, NumberSourceError> {
    if let Some(error) = resolution.errors.first() {
        return Err(NumberSourceError::Syntax {
            message: format!("resolve: {error:?}"),
        });
    }
    let mut builder = LocalIrBuilder::new();
    let mut stmts = Vec::new();
    {
        let mut lowering = NumberLowering::new(&mut builder);
        let mut env = Vec::new();
        for stmt in &chunk.body.stmts {
            lower_stmt(source, stmt, &mut lowering, &mut stmts, &mut env)?;
        }
    }
    let ir = builder.finish(stmts);
    let binding_names = ir
        .local_bindings
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    let logical_slots = ir.locals;
    Ok(LoweredNumberChunk {
        ir,
        logical_slots,
        binding_names,
    })
}

fn lower_stmt(
    source: &str,
    stmt: &Stmt,
    lowering: &mut NumberLowering<'_>,
    stmts: &mut Vec<CoreStmt>,
    env: &mut Vec<ProvenLocal>,
) -> Result<(), NumberSourceError> {
    match stmt {
        Stmt::Empty { .. } => Ok(()),
        Stmt::Local {
            span,
            names,
            values,
        } => lower_local(source, *span, names, values, lowering, stmts, env),
        Stmt::Return { span, values } => {
            let value = single_proven(values, "return is not a single proven number")?;
            let lowered = lower_expr(source, value, lowering, stmts, env)?;
            stmts.push(CoreStmt::Expr {
                expr: lowered.into_expr(),
                line: line_of(source, *span),
            });
            Ok(())
        }
        _ => Err(unsupported("statement is outside the numeric subset")),
    }
}

fn lower_local(
    source: &str,
    span: Span,
    names: &[LocalName],
    values: &ExprList,
    lowering: &mut NumberLowering<'_>,
    stmts: &mut Vec<CoreStmt>,
    env: &mut Vec<ProvenLocal>,
) -> Result<(), NumberSourceError> {
    if names.len() != 1 || values.exprs.len() != 1 {
        return Err(unsupported("local is not a single proven number"));
    }
    let lowered = lower_expr(source, &values.exprs[0], lowering, stmts, env)?;
    let name = &names[0].name;
    let text = std::str::from_utf8(&name.bytes)
        .map_err(|_| unsupported("local name is not a proven number"))?;
    let slot = lowering.alloc_local_named(text).map_err(kernel_error)?;
    stmts.push(CoreStmt::Let {
        index: slot,
        declared_schema: None,
        expr: lowered.into_expr(),
        line: line_of(source, span),
    });
    env.push(ProvenLocal {
        name: name.bytes.clone(),
        slot,
    });
    Ok(())
}

fn lower_expr(
    source: &str,
    expr: &Expr,
    lowering: &mut NumberLowering<'_>,
    stmts: &mut Vec<CoreStmt>,
    env: &[ProvenLocal],
) -> Result<ClassifiedNumber, NumberSourceError> {
    match expr {
        Expr::Number { raw, kind, .. } => lower_number(raw, *kind),
        Expr::Name { name, .. } => lookup_proven(name.bytes.as_slice(), env),
        Expr::Paren { inner, .. } => lower_expr(source, inner, lowering, stmts, env),
        Expr::Unary { op, expr, .. } => lower_unary(*op, expr, source, lowering, stmts, env),
        Expr::Binary {
            op, left, right, ..
        } => {
            let lhs = lower_expr(source, left, lowering, stmts, env)?;
            let rhs = lower_expr(source, right, lowering, stmts, env)?;
            apply(*op, lhs, rhs, lowering, stmts)
        }
        Expr::BinaryChain { base, steps, .. } => {
            let mut acc = lower_expr(source, base, lowering, stmts, env)?;
            for (index, step) in steps.iter().enumerate() {
                let rhs = lower_expr(source, &step.expr, lowering, stmts, env)?;
                acc = apply(step.op, acc, rhs, lowering, stmts)?;
                if index + 1 != steps.len() {
                    acc = lowering.materialize(acc, stmts).map_err(kernel_error)?;
                }
            }
            Ok(acc)
        }
        _ => Err(unsupported("expression is outside the numeric subset")),
    }
}

fn lower_unary(
    op: UnOp,
    expr: &Expr,
    source: &str,
    lowering: &mut NumberLowering<'_>,
    stmts: &mut Vec<CoreStmt>,
    env: &[ProvenLocal],
) -> Result<ClassifiedNumber, NumberSourceError> {
    if op != UnOp::Neg {
        return Err(unsupported("operator is outside the numeric subset"));
    }
    let inner = lower_expr(source, expr, lowering, stmts, env)?;
    let inner = lowering.materialize(inner, stmts).map_err(kernel_error)?;
    attest(inner.neg())
}

fn apply(
    op: BinOp,
    lhs: ClassifiedNumber,
    rhs: ClassifiedNumber,
    lowering: &mut NumberLowering<'_>,
    stmts: &mut Vec<CoreStmt>,
) -> Result<ClassifiedNumber, NumberSourceError> {
    let lhs = lowering.materialize(lhs, stmts).map_err(kernel_error)?;
    let rhs = lowering.materialize(rhs, stmts).map_err(kernel_error)?;
    let expr = match op {
        BinOp::Add => lhs.add(rhs),
        BinOp::Sub => lhs.sub(rhs),
        BinOp::Mul => lhs.mul(rhs),
        BinOp::Div => lhs.div(rhs),
        BinOp::Pow => lhs.pow(rhs),
        BinOp::Mod => lowering.floor_mod(lhs, rhs).map_err(kernel_error)?,
        _ => return Err(unsupported("operator is outside the numeric subset")),
    };
    attest(expr)
}

fn lower_number(raw: &str, kind: NumberKind) -> Result<ClassifiedNumber, NumberSourceError> {
    let value = match kind {
        NumberKind::Signed64 => return Err(not_float("Signed64")),
        NumberKind::Unsigned64 => return Err(not_float("Unsigned64")),
        NumberKind::Imaginary => return Err(not_float("Imaginary")),
        NumberKind::HexFloat => return Err(unsupported("literal form is not lowered")),
        NumberKind::Integer | NumberKind::Float => parse_decimal(raw)?,
        NumberKind::HexInteger => parse_prefixed_integer(raw, 16)?,
        NumberKind::Binary => parse_prefixed_integer(raw, 2)?,
    };
    if value.is_nan() {
        return Err(unsupported("literal form is not lowered"));
    }
    Ok(ClassifiedNumber::literal(value))
}

fn parse_decimal(raw: &str) -> Result<f64, NumberSourceError> {
    let normalized = if raw.starts_with('.') {
        format!("0{raw}")
    } else if let Some(prefix) = raw.strip_suffix('.') {
        format!("{prefix}.0")
    } else {
        raw.to_string()
    };
    normalized
        .parse::<f64>()
        .map_err(|_| unsupported("literal form is not lowered"))
}

fn parse_prefixed_integer(raw: &str, radix: u32) -> Result<f64, NumberSourceError> {
    let digits = raw
        .get(2..)
        .filter(|digits| !digits.is_empty())
        .ok_or_else(|| unsupported("literal form is not lowered"))?;
    let marker = raw.as_bytes().get(1).copied().unwrap_or(0) | 0x20;
    let expected = if radix == 16 { b'x' } else { b'b' };
    if raw.len() < 3 || raw.as_bytes()[0] != b'0' || marker != expected {
        return Err(unsupported("literal form is not lowered"));
    }
    let mut magnitude: u64 = 0;
    for byte in digits.bytes() {
        let digit = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => return Err(unsupported("literal form is not lowered")),
        };
        if u32::from(digit) >= radix {
            return Err(unsupported("literal form is not lowered"));
        }
        let scaled = magnitude
            .checked_mul(u64::from(radix))
            .ok_or_else(|| unsupported("literal form is not lowered"))?;
        magnitude = scaled
            .checked_add(u64::from(digit))
            .ok_or_else(|| unsupported("literal form is not lowered"))?;
    }
    Ok(magnitude as f64)
}

fn lookup_proven(name: &[u8], env: &[ProvenLocal]) -> Result<ClassifiedNumber, NumberSourceError> {
    let slot = env
        .iter()
        .rev()
        .find(|local| local.name == name)
        .map(|local| local.slot)
        .ok_or_else(|| unsupported("name is not a proven local"))?;
    attest(CoreExpr::Var(slot))
}

fn single_proven<'a>(
    values: &'a ExprList,
    reason: &'static str,
) -> Result<&'a Expr, NumberSourceError> {
    if values.exprs.len() == 1 {
        Ok(&values.exprs[0])
    } else {
        Err(unsupported(reason))
    }
}

fn attest(expr: CoreExpr) -> Result<ClassifiedNumber, NumberSourceError> {
    ClassifiedNumber::from_attested_float(expr, FloatTypeEvidence::attested()).map_err(kernel_error)
}

fn kernel_error(error: ParseError) -> NumberSourceError {
    NumberSourceError::CompileUnsupported {
        reason: format!("number kernel: {}", error.message),
    }
}

fn unsupported(reason: &'static str) -> NumberSourceError {
    NumberSourceError::CompileUnsupported {
        reason: reason.to_string(),
    }
}

fn not_float(kind: &'static str) -> NumberSourceError {
    NumberSourceError::NotFloat { kind }
}

fn line_of(source: &str, span: Span) -> u32 {
    let end = usize::try_from(span.start)
        .unwrap_or(source.len())
        .min(source.len());
    let line = source[..end].bytes().filter(|byte| *byte == b'\n').count() + 1;
    u32::try_from(line).unwrap_or(u32::MAX)
}
