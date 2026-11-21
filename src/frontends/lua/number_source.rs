//! Narrow source seam from a parsed Lua chunk onto the binary64 number kernel.
//!
//! Only a grammar kind that is already binary64, or a local this walk just
//! bound from a classified value, enters the kernel. Hex integers and hex
//! floats share one conversion. `LL`, `ULL`, imaginary suffixes, and names
//! this walk did not prove stay outside it. Chains are hoisted with the
//! kernel's temps so a flat source spine does not become an unbounded Core
//! expression. Slot reuse stays with Core.

use super::ast::{BinOp, Chunk, Expr, ExprList, LocalName, NumberKind, Span, Stmt, UnOp};
use super::number::{ClassifiedNumber, NumberLowering};
use super::resolve::Resolution;
use vm::{FrontendIr, LocalIrBuilder, ParseError, Stmt as CoreStmt};

pub(crate) struct LoweredNumberChunk {
    pub(crate) ir: FrontendIr,
    pub(crate) logical_slots: usize,
    pub(crate) binding_names: Vec<String>,
    /// `resolve_local_expr` calls made while naming this chunk's temps.
    pub(crate) name_probes: u64,
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
    let name_probes = {
        let mut lowering = NumberLowering::new(&mut builder);
        let mut env = Vec::new();
        for stmt in &chunk.body.stmts {
            lower_stmt(source, stmt, &mut lowering, &mut stmts, &mut env)?;
        }
        lowering.name_probes()
    };
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
        name_probes,
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
        NumberKind::HexFloat | NumberKind::HexInteger => parse_hex_literal(raw)?,
        NumberKind::Integer | NumberKind::Float => parse_decimal(raw)?,
        NumberKind::Binary => parse_binary_literal(raw)?,
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

fn parse_binary_literal(raw: &str) -> Result<f64, NumberSourceError> {
    let digits = raw
        .get(2..)
        .filter(|digits| !digits.is_empty())
        .ok_or_else(|| unsupported(UnsupportedReason::LiteralForm))?;
    let marker = raw.as_bytes().get(1).copied().unwrap_or(0) | 0x20;
    if raw.len() < 3 || raw.as_bytes()[0] != b'0' || marker != b'b' {
        return Err(unsupported(UnsupportedReason::LiteralForm));
    }
    let mut magnitude: u64 = 0;
    for byte in digits.bytes() {
        let digit = match byte {
            b'0' | b'1' => byte - b'0',
            _ => return Err(unsupported(UnsupportedReason::LiteralForm)),
        };
        let scaled = magnitude
            .checked_mul(2)
            .ok_or_else(|| unsupported(UnsupportedReason::LiteralForm))?;
        magnitude = scaled
            .checked_add(u64::from(digit))
            .ok_or_else(|| unsupported(UnsupportedReason::LiteralForm))?;
    }
    Ok(magnitude as f64)
}

/// One round-to-nearest-even from the full hex significand onto binary64.
///
/// The scan keeps 64 bits of magnitude and one sticky flag. That is enough for
/// a 53-bit significand plus a guard bit; every later digit folds into sticky.
/// Extra space stays constant. The exponent is applied as a bit place, not as
/// a second floating multiply.
fn parse_hex_literal(raw: &str) -> Result<f64, NumberSourceError> {
    let bytes = raw.as_bytes();
    if bytes.len() < 3 || bytes[0] != b'0' || (bytes[1] | 0x20) != b'x' {
        return Err(unsupported(UnsupportedReason::LiteralForm));
    }
    let body = &bytes[2..];
    let exp_at = body.iter().position(|byte| (byte | 0x20) == b'p');
    let digits = match exp_at {
        Some(at) => &body[..at],
        None => body,
    };
    let bin_exp = match exp_at {
        Some(at) => parse_bin_exp(&body[at + 1..])?,
        None => 0,
    };
    hex_digits_to_f64(digits, bin_exp)
}

fn parse_bin_exp(text: &[u8]) -> Result<i64, NumberSourceError> {
    if text.is_empty() {
        return Err(unsupported(UnsupportedReason::LiteralForm));
    }
    let (negative, digits) = match text[0] {
        b'+' => (false, &text[1..]),
        b'-' => (true, &text[1..]),
        _ => (false, text),
    };
    if digits.is_empty() {
        return Err(unsupported(UnsupportedReason::LiteralForm));
    }
    let mut magnitude: i64 = 0;
    for byte in digits {
        if !byte.is_ascii_digit() {
            return Err(unsupported(UnsupportedReason::LiteralForm));
        }
        let digit = i64::from(byte - b'0');
        magnitude = match magnitude
            .checked_mul(10)
            .and_then(|value| value.checked_add(digit))
        {
            Some(value) => value,
            None => return Ok(if negative { i64::MIN } else { i64::MAX }),
        };
    }
    Ok(if negative { -magnitude } else { magnitude })
}

fn hex_digits_to_f64(digits: &[u8], bin_exp: i64) -> Result<f64, NumberSourceError> {
    let mut saw_digit = false;
    let mut seen_dot = false;
    let mut integer_len = 0usize;
    let mut first_nz: Option<(usize, u8)> = None;
    for (index, byte) in digits.iter().copied().enumerate() {
        if byte == b'.' {
            if seen_dot {
                return Err(unsupported(UnsupportedReason::LiteralForm));
            }
            seen_dot = true;
            continue;
        }
        let digit = hex_value(byte).ok_or_else(|| unsupported(UnsupportedReason::LiteralForm))?;
        saw_digit = true;
        if !seen_dot {
            integer_len += 1;
        }
        if digit != 0 && first_nz.is_none() {
            first_nz = Some((index, digit));
        }
    }
    if !saw_digit {
        return Err(unsupported(UnsupportedReason::LiteralForm));
    }
    let Some((index, digit)) = first_nz else {
        return Ok(0.0);
    };
    let place = match hex_place(index, integer_len) {
        Place::Over => return Ok(f64::INFINITY),
        Place::Under => return Ok(0.0),
        Place::Exact(place) => place,
    };
    let bitlen = 32 - u32::from(digit).leading_zeros();
    let msb_exp = match msb_exponent(bin_exp, place, bitlen) {
        Place::Over => return Ok(f64::INFINITY),
        Place::Under => return Ok(0.0),
        Place::Exact(exp) => exp,
    };
    if msb_exp > 1023 {
        return Ok(f64::INFINITY);
    }
    if msb_exp < -1075 {
        return Ok(0.0);
    }
    let (mag, sticky) = hex_window(digits, index, digit, bitlen);
    Ok(round_hex_window(mag, msb_exp, sticky))
}

enum Place {
    Exact(i64),
    Over,
    Under,
}

fn hex_place(index: usize, integer_len: usize) -> Place {
    if index < integer_len {
        match i64::try_from(integer_len - 1 - index) {
            Ok(place) => Place::Exact(place),
            Err(_) => Place::Over,
        }
    } else {
        let fraction_index = index - integer_len - 1;
        match i64::try_from(fraction_index) {
            Ok(place) => Place::Exact(-1 - place),
            Err(_) => Place::Under,
        }
    }
}

fn msb_exponent(bin_exp: i64, place: i64, bitlen: u32) -> Place {
    let shift = match place.checked_mul(4) {
        Some(shift) => shift,
        None => return if place < 0 { Place::Under } else { Place::Over },
    };
    let placed = match bin_exp.checked_add(shift) {
        Some(placed) => placed,
        None => {
            return if place < 0 && bin_exp < 0 {
                Place::Under
            } else {
                Place::Over
            };
        }
    };
    match placed.checked_add(i64::from(bitlen - 1)) {
        Some(exp) => Place::Exact(exp),
        None => {
            if placed < 0 {
                Place::Under
            } else {
                Place::Over
            }
        }
    }
}

fn hex_window(digits: &[u8], start: usize, first: u8, bitlen: u32) -> (u64, bool) {
    let mut mag = u64::from(first) << (64 - bitlen);
    let mut filled = bitlen;
    let mut sticky = false;
    for byte in &digits[start + 1..] {
        if *byte == b'.' {
            continue;
        }
        let Some(digit) = hex_value(*byte) else {
            break;
        };
        if filled >= 64 {
            if digit != 0 {
                sticky = true;
            }
            continue;
        }
        if filled + 4 <= 64 {
            mag |= u64::from(digit) << (64 - filled - 4);
            filled += 4;
            continue;
        }
        let room = 64 - filled;
        mag |= u64::from(digit) >> (4 - room);
        let dropped = digit & ((1u8 << (4 - room)) - 1);
        if dropped != 0 {
            sticky = true;
        }
        filled = 64;
    }
    (mag, sticky)
}

fn round_hex_window(mag: u64, msb_exp: i64, sticky: bool) -> f64 {
    if msb_exp >= -1022 {
        let mut significand = round_right(mag, 11, sticky);
        let mut exp = msb_exp;
        if significand == 1u64 << 53 {
            significand = 1u64 << 52;
            exp += 1;
        }
        if exp > 1023 {
            return f64::INFINITY;
        }
        let exp_field = u64::try_from(exp + 1023).expect("binary64 exponent");
        let fraction = significand & ((1u64 << 52) - 1);
        return f64::from_bits((exp_field << 52) | fraction);
    }
    let shift = u32::try_from(-msb_exp - 1011).expect("subnormal shift");
    let significand = round_right(mag, shift, sticky);
    if significand >= 1u64 << 52 {
        f64::from_bits(1u64 << 52)
    } else {
        f64::from_bits(significand)
    }
}

fn round_right(mag: u64, shift: u32, sticky: bool) -> u64 {
    if shift == 0 {
        return mag;
    }
    if shift > 64 {
        return 0;
    }
    if shift == 64 {
        let guard = mag >> 63;
        let rest = mag & ((1u64 << 63) - 1);
        let round_up = guard == 1 && (rest != 0 || sticky);
        return u64::from(round_up);
    }
    let kept = mag >> shift;
    let guard = (mag >> (shift - 1)) & 1;
    let rest_mask = (1u64 << (shift - 1)) - 1;
    let rest = mag & rest_mask;
    let round_up = guard == 1 && (rest != 0 || sticky || (kept & 1) == 1);
    if round_up { kept + 1 } else { kept }
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
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
