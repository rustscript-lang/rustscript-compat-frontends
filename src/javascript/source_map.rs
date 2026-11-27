//! Byte-span mapping for the ES5 Script parser/admission stage.
//!
//! SWC spans are absolute [`swc_common::BytePos`] values inside the scratch
//! [`swc_common::SourceMap`] file created for one parse. Every diagnostic
//! produced on this path carries the caller's `source_id` and file-relative
//! byte offsets, including after UTF-8 prefixes and inside nested functions.
//! The historic hardcoded `source_id` 0 is never used here.

use swc_common::Span as SwcSpan;
use vm::{ParseError, compiler::source_map::Span as SourceSpan};

/// Build a [`ParseError`] for `span` with the caller's `source_id`.
///
/// `base` is the start position of the scratch source file, so `lo`/`hi`
/// are file-relative byte offsets suitable for downstream source maps.
pub(super) fn parse_error(source_id: u32, base: u32, span: SwcSpan, message: String) -> ParseError {
    let lo = span.lo.0.saturating_sub(base) as usize;
    let hi = span.hi.0.saturating_sub(base) as usize;
    ParseError::at_span(SourceSpan::new(source_id, lo, hi), message)
}

/// Grammar/early-error rejection: the source violates ES5.1 Script grammar
/// or a strict-mode early-error rule.
pub(super) fn syntax_error(
    source_id: u32,
    base: u32,
    span: SwcSpan,
    message: impl Into<String>,
) -> ParseError {
    parse_error(
        source_id,
        base,
        span,
        format!("JavaScript ES5 SyntaxError: {}", message.into()),
    )
}

/// Grammar-stage refusal for post-ES5 forms: parsed by the modern SWC lexer
/// but outside the ES5.1 whitelist. This is admission refusal, distinct from
/// later P0 lowering refusal for admitted-but-unlowered ES5 forms.
pub(super) fn unsupported_error(
    source_id: u32,
    base: u32,
    span: SwcSpan,
    message: impl Into<String>,
) -> ParseError {
    parse_error(
        source_id,
        base,
        span,
        format!("JavaScript ES5 unsupported syntax: {}", message.into()),
    )
}
