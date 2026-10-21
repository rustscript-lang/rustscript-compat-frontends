//! Shared original-source ES5 string-literal codec (P2-A).
//!
//! One private raw-lexeme decoder feeds both grammar admission and IR
//! lowering, so literal values are never derived from the locked parser's
//! cooked payload. Normative reference is ES5.1 section 7.8.4; the only
//! additional extension is the explicitly retained sloppy Annex B.1.2 octal
//! form, which strict code always rejects. The decoder consumes the raw
//! token bytes including both quote delimiters, sliced by the literal's own
//! source span, and emits exact UTF-16 units with no normalization:
//! Unicode escapes emit their four-hex-digit value directly (lone
//! surrogates preserved), hex escapes consume exactly two hex digits, and
//! `\0` is NUL only when the next character is not a DecimalDigit.
//!
//! The actual Annex B.1.2 productions (including DecimalDigit lookaheads)
//! are implemented, not modern greedy-octal behavior: a lone OctalDigit
//! requires the next character to not be a DecimalDigit,
//! ZeroToThree+OctalDigit requires the same lookahead, FourToSeven+
//! OctalDigit consumes exactly two units, and ZeroToThree+OctalDigit+
//! OctalDigit consumes exactly three. Consequently `\08`, `\09` and `\118`
//! have no permitted derivation and are rejected in both modes, while
//! `\400` stays the two-digit FourToSeven production followed by an
//! ordinary `0`. Decimal `8`/`9` are EscapeCharacters, never
//! NonEscapeCharacters, so `\8`/`\9` are rejected in both modes as well.
//!
//! This is a compiler-time codec, never a runtime interpreter. Oversized
//! payloads stop at one unit past the bound and surface as resource errors,
//! kept separate from lexical errors.

use swc_common::{BytePos, Span as SwcSpan};

/// Bounded literal emission: at most this many UTF-16 units per literal.
/// Single owner; lowering reuses this instead of a second constant.
pub(super) const MAX_LITERAL_UTF16_UNITS: usize = 16384;

/// Explicit strict/sloppy-AnnexB policy: the whole containing code's
/// resolved strictness, never the physical typing policy.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum StringStrictness {
    Strict,
    Sloppy,
}

/// Codec error category: an ES5 grammar violation, a post-ES5 spelling, or
/// the unit-bound resource refusal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum StringCodecErrorKind {
    Lexical,
    PostEs5,
    Oversize,
}

/// Structured lexical error with the byte offset relative to the raw token
/// start, so callers can narrow the diagnostic span to the offending escape.
pub(super) struct StringCodecError {
    pub relative_offset: usize,
    pub kind: StringCodecErrorKind,
    pub message: String,
}

fn error(offset: usize, kind: StringCodecErrorKind, message: String) -> StringCodecError {
    StringCodecError {
        relative_offset: offset,
        kind,
        message,
    }
}

/// Narrow a literal token span to the offending escape for precise
/// diagnostics. Always stays a non-empty sub-range of the token span.
pub(super) fn narrowed_span(span: SwcSpan, relative_offset: usize) -> SwcSpan {
    let width = span.hi.0.saturating_sub(span.lo.0);
    if width == 0 {
        return span;
    }
    let clamped = (relative_offset as u32).min(width.saturating_sub(1));
    let lo = span.lo.0.saturating_add(clamped);
    SwcSpan::new(BytePos(lo), BytePos(lo.saturating_add(1).min(span.hi.0)))
}

fn hex_value(byte: u8) -> Option<u16> {
    match byte {
        b'0'..=b'9' => Some((byte - b'0') as u16),
        b'a'..=b'f' => Some((byte - b'a' + 10) as u16),
        b'A'..=b'F' => Some((byte - b'A' + 10) as u16),
        _ => None,
    }
}

fn is_decimal_digit(byte: u8) -> bool {
    byte.is_ascii_digit()
}

fn is_octal_digit(byte: u8) -> bool {
    matches!(byte, b'0'..=b'7')
}

const LINE_SEPARATOR: char = '\u{2028}';
const PARAGRAPH_SEPARATOR: char = '\u{2029}';

/// Decode raw string-token bytes (including both quote delimiters) under
/// the given strictness policy into exact UTF-16 units.
pub(super) fn decode_string_literal(
    raw: &str,
    strict: StringStrictness,
) -> Result<Vec<u16>, StringCodecError> {
    let bytes = raw.as_bytes();
    if bytes.len() < 2 {
        return Err(error(
            0,
            StringCodecErrorKind::Lexical,
            "string literal token is too short to hold delimiters".to_owned(),
        ));
    }
    let quote = bytes[0];
    if quote != b'"' && quote != b'\'' {
        return Err(error(
            0,
            StringCodecErrorKind::Lexical,
            "string literal token is missing its opening quote".to_owned(),
        ));
    }
    if bytes[bytes.len() - 1] != quote {
        return Err(error(
            bytes.len() - 1,
            StringCodecErrorKind::Lexical,
            "string literal token is missing its closing quote".to_owned(),
        ));
    }
    let strict_mode = strict == StringStrictness::Strict;
    let mut units: Vec<u16> = Vec::new();
    let push = |units: &mut Vec<u16>, unit: u16| units.push(unit);
    let mut index = 1usize;
    let end = bytes.len() - 1;
    while index < end {
        let byte = bytes[index];
        if byte == b'\\' {
            let escape_at = index;
            index += 1;
            if index >= end {
                return Err(error(
                    escape_at,
                    StringCodecErrorKind::Lexical,
                    "string literal ends with a dangling backslash".to_owned(),
                ));
            }
            match bytes[index] {
                b'\'' => {
                    push(&mut units, 0x27);
                    index += 1;
                }
                b'"' => {
                    push(&mut units, 0x22);
                    index += 1;
                }
                b'\\' => {
                    push(&mut units, 0x5C);
                    index += 1;
                }
                b'b' => {
                    push(&mut units, 0x08);
                    index += 1;
                }
                b'f' => {
                    push(&mut units, 0x0C);
                    index += 1;
                }
                b'n' => {
                    push(&mut units, 0x0A);
                    index += 1;
                }
                b'r' => {
                    push(&mut units, 0x0D);
                    index += 1;
                }
                b't' => {
                    push(&mut units, 0x09);
                    index += 1;
                }
                b'v' => {
                    push(&mut units, 0x0B);
                    index += 1;
                }
                b'0' => {
                    if strict_mode {
                        if bytes
                            .get(index + 1)
                            .is_some_and(|next| is_decimal_digit(*next))
                        {
                            return Err(error(
                                escape_at,
                                StringCodecErrorKind::Lexical,
                                "legacy octal escapes are not allowed in strict mode code"
                                    .to_owned(),
                            ));
                        }
                        push(&mut units, 0x0000);
                        index += 1;
                    } else {
                        index = decode_sloppy_octal(bytes, raw, index, escape_at, &mut units)?;
                    }
                }
                b'1'..=b'7' => {
                    if strict_mode {
                        return Err(error(
                            escape_at,
                            StringCodecErrorKind::Lexical,
                            "legacy octal escapes are not allowed in strict mode code".to_owned(),
                        ));
                    }
                    index = decode_sloppy_octal(bytes, raw, index, escape_at, &mut units)?;
                }
                b'8' | b'9' => {
                    return Err(error(
                        escape_at,
                        StringCodecErrorKind::Lexical,
                        format!(
                            "string literal decimal escape '\\{}' has no ES5.1 or Annex-B derivation",
                            bytes[index] as char,
                        ),
                    ));
                }
                b'x' => {
                    let (Some(&high), Some(&low)) = (bytes.get(index + 1), bytes.get(index + 2))
                    else {
                        return Err(error(
                            escape_at,
                            StringCodecErrorKind::Lexical,
                            "invalid ES5 hex escape: want exactly two hex digits".to_owned(),
                        ));
                    };
                    match (hex_value(high), hex_value(low)) {
                        (Some(high), Some(low)) => {
                            push(&mut units, 16 * high + low);
                            index += 3;
                        }
                        _ => {
                            return Err(error(
                                escape_at,
                                StringCodecErrorKind::Lexical,
                                "invalid ES5 hex escape: want exactly two hex digits".to_owned(),
                            ));
                        }
                    }
                }
                b'u' => {
                    if bytes.get(index + 1) == Some(&b'{') {
                        return Err(error(
                            escape_at,
                            StringCodecErrorKind::PostEs5,
                            "JavaScript ES5 string literals reject post-ES5 braced Unicode escapes"
                                .to_owned(),
                        ));
                    }
                    let mut digits = [0u16; 4];
                    for (position, digit) in digits.iter_mut().enumerate() {
                        match bytes
                            .get(index + 1 + position)
                            .and_then(|byte| hex_value(*byte))
                        {
                            Some(value) => *digit = value,
                            None => {
                                return Err(error(
                                    escape_at,
                                    StringCodecErrorKind::Lexical,
                                    "invalid ES5 Unicode escape: want exactly four hex digits"
                                        .to_owned(),
                                ));
                            }
                        }
                    }
                    push(
                        &mut units,
                        4096 * digits[0] + 256 * digits[1] + 16 * digits[2] + digits[3],
                    );
                    index += 5;
                }
                b'\n' => {
                    // `\<LF>` line continuation contributes no units.
                    index += 1;
                }
                b'\r' => {
                    // `\<CR><LF>` is one continuation; lone `\<CR>` another.
                    if bytes.get(index + 1) == Some(&b'\n') {
                        index += 2;
                    } else {
                        index += 1;
                    }
                }
                _ => {
                    // `raw[index..]` starts at a character boundary: every
                    // advance below consumes whole ASCII bytes or whole
                    // decoded characters.
                    let rest = &raw[index..];
                    let character = rest.chars().next().ok_or_else(|| {
                        error(
                            escape_at,
                            StringCodecErrorKind::Lexical,
                            "string literal ends mid-escape".to_owned(),
                        )
                    })?;
                    if character == LINE_SEPARATOR || character == PARAGRAPH_SEPARATOR {
                        // Escaped `\<LS>` / `\<PS>` is a line continuation.
                        index += character.len_utf8();
                    } else {
                        // NonEscapeCharacter: every ASCII EscapeCharacter
                        // (`' " \ b f n r t v`, DecimalDigits, `x`, `u`) and
                        // every LineTerminator is handled above, so this
                        // source character contributes itself.
                        let mut encoded = [0u16; 2];
                        for unit in character.encode_utf16(&mut encoded) {
                            push(&mut units, *unit);
                        }
                        index += character.len_utf8();
                    }
                }
            }
        } else if byte == quote {
            return Err(error(
                index,
                StringCodecErrorKind::Lexical,
                "string literal contains an unescaped quote".to_owned(),
            ));
        } else {
            // `index` is at a character boundary by the same invariant.
            let rest = &raw[index..];
            let character = rest.chars().next().ok_or_else(|| {
                error(
                    index,
                    StringCodecErrorKind::Lexical,
                    "string literal ends mid-character".to_owned(),
                )
            })?;
            if character == LINE_SEPARATOR || character == PARAGRAPH_SEPARATOR {
                return Err(error(
                    index,
                    StringCodecErrorKind::Lexical,
                    "JavaScript ES5 string literals reject raw U+2028/U+2029 separators".to_owned(),
                ));
            }
            if character == '\n' || character == '\r' {
                return Err(error(
                    index,
                    StringCodecErrorKind::Lexical,
                    "string literal contains an unescaped line terminator".to_owned(),
                ));
            }
            let mut encoded = [0u16; 2];
            for unit in character.encode_utf16(&mut encoded) {
                push(&mut units, *unit);
            }
            index += character.len_utf8();
        }
        if units.len() > MAX_LITERAL_UTF16_UNITS {
            return Err(error(
                index.min(end),
                StringCodecErrorKind::Oversize,
                "JavaScript ES5 string literal exceeds the 16384 UTF-16 unit bound".to_owned(),
            ));
        }
    }
    Ok(units)
}

/// Decode one sloppy Annex B.1.2 OctalEscapeSequence starting at the first
/// octal digit (`index`), returning the index just past the consumed
/// spelling. Implements the actual productions with their DecimalDigit
/// lookaheads; anything without a derivation is a lexical error.
fn decode_sloppy_octal(
    bytes: &[u8],
    raw: &str,
    index: usize,
    escape_at: usize,
    units: &mut Vec<u16>,
) -> Result<usize, StringCodecError> {
    debug_assert!(is_octal_digit(bytes[index]));
    let first = (bytes[index] - b'0') as u16;
    let spelling =
        |consumed: usize| raw[escape_at..(escape_at + consumed).min(raw.len())].to_owned();
    if first <= 3 {
        let second = bytes.get(index + 1).copied();
        let third = bytes.get(index + 2).copied();
        if second.is_some_and(is_octal_digit) && third.is_some_and(is_octal_digit) {
            let value =
                64 * first + 8 * (second.unwrap() - b'0') as u16 + (third.unwrap() - b'0') as u16;
            units.push(value);
            return Ok(index + 3);
        }
        if second.is_some_and(is_octal_digit) {
            if third.is_some_and(is_decimal_digit) {
                return Err(error(
                    escape_at,
                    StringCodecErrorKind::Lexical,
                    format!(
                        "string literal escape {:?} has no ES5.1 or Annex-B derivation",
                        spelling(4),
                    ),
                ));
            }
            units.push(8 * first + (second.unwrap() - b'0') as u16);
            return Ok(index + 2);
        }
        if second.is_some_and(is_decimal_digit) {
            return Err(error(
                escape_at,
                StringCodecErrorKind::Lexical,
                format!(
                    "string literal escape {:?} has no ES5.1 or Annex-B derivation",
                    spelling(3),
                ),
            ));
        }
        units.push(first);
        Ok(index + 1)
    } else {
        let second = bytes.get(index + 1).copied();
        if second.is_some_and(is_octal_digit) {
            units.push(8 * first + (second.unwrap() - b'0') as u16);
            return Ok(index + 2);
        }
        if second.is_some_and(is_decimal_digit) {
            return Err(error(
                escape_at,
                StringCodecErrorKind::Lexical,
                format!(
                    "string literal escape {:?} has no ES5.1 or Annex-B derivation",
                    spelling(3),
                ),
            ));
        }
        units.push(first);
        Ok(index + 1)
    }
}
