//! Narrow source seam from a parsed Lua chunk onto the binary64 number kernel.
//!
//! Only a grammar kind that is already binary64, or a local this walk just
//! bound from a classified value, enters the kernel. `LL`, `ULL`, imaginary
//! suffixes, and names this walk did not prove stay outside it. Chains are
//! hoisted with the kernel's temps so a flat source spine does not become an
//! unbounded Core expression. Slot reuse stays with Core.

use super::ast::{BinOp, Chunk, Expr, ExprList, LocalName, NumberKind, Span, Stmt, UnOp};
use super::number::{ClassifiedNumber, NumberLowering};
use super::resolve::Resolution;
use vm::{FrontendIr, LocalIrBuilder, ParseError, Stmt as CoreStmt};

pub(crate) struct LoweredNumberChunk {
    pub(crate) ir: FrontendIr,
    pub(crate) logical_slots: usize,
    pub(crate) binding_names: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NumberSourceError {
    Syntax { message: String },
    CompileUnsupported { reason: UnsupportedReason },
    NotFloat { kind: NumberKind },
}

/// Why a parsed fragment does not enter the binary64 kernel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UnsupportedReason {
    Statement,
    Operator,
    Expression,
    LiteralForm,
    UnprovenName,
    NotSingle,
    LocalName,
    Kernel { message: String },
}

struct ProvenLocal {
    name: Vec<u8>,
    value: ClassifiedNumber,
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
            let value = single_proven(values)?;
            let lowered = lower_expr(source, value, lowering, stmts, env)?;
            stmts.push(CoreStmt::Expr {
                expr: lowered.into_expr(),
                line: line_of(source, *span),
            });
            Ok(())
        }
        _ => Err(unsupported(UnsupportedReason::Statement)),
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
        return Err(unsupported(UnsupportedReason::NotSingle));
    }
    let lowered = lower_expr(source, &values.exprs[0], lowering, stmts, env)?;
    let name = &names[0].name;
    let text =
        std::str::from_utf8(&name.bytes).map_err(|_| unsupported(UnsupportedReason::LocalName))?;
    let bound = lowering
        .bind_named(text, lowered, line_of(source, span), stmts)
        .map_err(kernel_error)?;
    env.push(ProvenLocal {
        name: name.bytes.clone(),
        value: bound,
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
        _ => Err(unsupported(UnsupportedReason::Expression)),
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
        return Err(unsupported(UnsupportedReason::Operator));
    }
    let inner = lower_expr(source, expr, lowering, stmts, env)?;
    let inner = lowering.materialize(inner, stmts).map_err(kernel_error)?;
    Ok(inner.neg())
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
    match op {
        BinOp::Add => Ok(lhs.add(rhs)),
        BinOp::Sub => Ok(lhs.sub(rhs)),
        BinOp::Mul => Ok(lhs.mul(rhs)),
        BinOp::Div => Ok(lhs.div(rhs)),
        BinOp::Pow => Ok(lhs.pow(rhs)),
        BinOp::Mod => lowering.floor_mod(lhs, rhs).map_err(kernel_error),
        _ => Err(unsupported(UnsupportedReason::Operator)),
    }
}

fn lower_number(raw: &str, kind: NumberKind) -> Result<ClassifiedNumber, NumberSourceError> {
    let value = match kind {
        NumberKind::Signed64 => return Err(not_float(NumberKind::Signed64)),
        NumberKind::Unsigned64 => return Err(not_float(NumberKind::Unsigned64)),
        NumberKind::Imaginary => return Err(not_float(NumberKind::Imaginary)),
        NumberKind::HexFloat => return Err(unsupported(UnsupportedReason::LiteralForm)),
        NumberKind::Integer | NumberKind::Float => parse_decimal(raw)?,
        NumberKind::HexInteger => parse_prefixed_integer(raw, 16)?,
        NumberKind::Binary => parse_prefixed_integer(raw, 2)?,
    };
    if value.is_nan() {
        return Err(unsupported(UnsupportedReason::LiteralForm));
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
        .map_err(|_| unsupported(UnsupportedReason::LiteralForm))
}

fn parse_prefixed_integer(raw: &str, radix: u32) -> Result<f64, NumberSourceError> {
    let digits = raw
        .get(2..)
        .filter(|digits| !digits.is_empty())
        .ok_or_else(|| unsupported(UnsupportedReason::LiteralForm))?;
    let marker = raw.as_bytes().get(1).copied().unwrap_or(0) | 0x20;
    let expected = if radix == 16 { b'x' } else { b'b' };
    if raw.len() < 3 || raw.as_bytes()[0] != b'0' || marker != expected {
        return Err(unsupported(UnsupportedReason::LiteralForm));
    }
    let mut magnitude: u64 = 0;
    for byte in digits.bytes() {
        let digit = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => return Err(unsupported(UnsupportedReason::LiteralForm)),
        };
        if u32::from(digit) >= radix {
            return Err(unsupported(UnsupportedReason::LiteralForm));
        }
        let scaled = magnitude
            .checked_mul(u64::from(radix))
            .ok_or_else(|| unsupported(UnsupportedReason::LiteralForm))?;
        magnitude = scaled
            .checked_add(u64::from(digit))
            .ok_or_else(|| unsupported(UnsupportedReason::LiteralForm))?;
    }
    Ok(magnitude as f64)
}

fn lookup_proven(name: &[u8], env: &[ProvenLocal]) -> Result<ClassifiedNumber, NumberSourceError> {
    env.iter()
        .rev()
        .find(|local| local.name == name)
        .map(|local| local.value.clone())
        .ok_or_else(|| unsupported(UnsupportedReason::UnprovenName))
}

fn single_proven(values: &ExprList) -> Result<&Expr, NumberSourceError> {
    if values.exprs.len() == 1 {
        Ok(&values.exprs[0])
    } else {
        Err(unsupported(UnsupportedReason::NotSingle))
    }
}

fn kernel_error(error: ParseError) -> NumberSourceError {
    NumberSourceError::CompileUnsupported {
        reason: UnsupportedReason::Kernel {
            message: error.message,
        },
    }
}

fn unsupported(reason: UnsupportedReason) -> NumberSourceError {
    NumberSourceError::CompileUnsupported { reason }
}

fn not_float(kind: NumberKind) -> NumberSourceError {
    NumberSourceError::NotFloat { kind }
}

fn line_of(source: &str, span: Span) -> u32 {
    let end = usize::try_from(span.start)
        .unwrap_or(source.len())
        .min(source.len());
    let line = source[..end].bytes().filter(|byte| *byte == b'\n').count() + 1;
    u32::try_from(line).unwrap_or(u32::MAX)
}
