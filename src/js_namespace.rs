use std::borrow::Cow;
use std::collections::HashSet;

use vm::{FrontendIr, ImportClause, ParseError, Span};

use crate::source_loader::{is_file_module_spec, parse_js_imports};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TokenKind {
    Ident,
    String,
    Number,
    Dot,
    Star,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Semi,
    Arrow,
    Other,
}

#[derive(Clone, Debug)]
struct Token {
    kind: TokenKind,
    start: usize,
    end: usize,
}

pub(crate) fn file_module_namespace_aliases(source: &str) -> HashSet<String> {
    parse_js_imports(source)
        .into_iter()
        .filter_map(|import| match import.clause {
            ImportClause::Namespace(alias) if is_file_module_spec(&import.spec) => Some(alias),
            _ => None,
        })
        .collect()
}

/// A Call whose callee is a MemberExpression rooted at an imported file-module
/// alias, with the original source span of `alias.member`.
#[derive(Clone, Debug)]
struct FileModuleMemberCall {
    alias: String,
    member: String,
    start: usize,
    end: usize,
    placeholder: String,
}

#[derive(Debug)]
pub(crate) struct FileModuleCallAnalysis {
    calls: Vec<FileModuleMemberCall>,
}

impl FileModuleCallAnalysis {
    pub(crate) fn parse_source<'a>(&self, source: &'a str) -> Result<Cow<'a, str>, ParseError> {
        if self.calls.is_empty() {
            return Ok(Cow::Borrowed(source));
        }
        // Frozen `try_parse_js_dotted_call` rewinds unknown (file-module) dotted
        // calls, leaving `alias.member()` unparsed. Fold only those callees to
        // same-length idents so the dialect can parse the original argument list
        // at the original offsets; lookalike literals/comments are untouched.
        let mut out = String::with_capacity(source.len());
        let mut last = 0usize;
        for call in &self.calls {
            if call.placeholder.len() != call.end.saturating_sub(call.start) {
                return Err(fold_error(
                    source,
                    call.start,
                    call.end,
                    format!(
                        "file-module member call '{}'.'{}' cannot be folded without changing source length",
                        call.alias, call.member
                    ),
                ));
            }
            out.push_str(&source[last..call.start]);
            out.push_str(&call.placeholder);
            last = call.end;
        }
        out.push_str(&source[last..]);
        if out.len() != source.len() {
            return Err(ParseError::new(
                "file-module member call fold changed source length",
            ));
        }
        if newline_offsets(&out) != newline_offsets(source) {
            return Err(ParseError::new(
                "file-module member call fold moved line boundaries",
            ));
        }
        Ok(Cow::Owned(out))
    }

    pub(crate) fn lower_ir(&self, ir: &mut FrontendIr) {
        if self.calls.is_empty() {
            return;
        }
        let extern_names = ir
            .implicit_extern_names
            .iter()
            .cloned()
            .collect::<HashSet<_>>();
        for func in &mut ir.functions {
            if !extern_names.contains(&func.name) {
                continue;
            }
            if let Some(call) = self.call_for_placeholder(&func.name) {
                func.name = qualified_name(call);
            }
        }
        for name in &mut ir.implicit_extern_names {
            if let Some(call) = self.call_for_placeholder(name) {
                *name = qualified_name(call);
            }
        }
        if let Some(index) = ir.parsed_semantic_index.as_mut() {
            for site in &mut index.call_sites {
                if let Some(call) = self.call_for_span(site.callee_span.lo, site.callee_span.hi) {
                    site.name = qualified_name(call);
                    site.is_namespace_call = true;
                    site.callee_span.lo = call.start;
                    site.callee_span.hi = call.end;
                }
            }
            for func_ref in &mut index.func_refs {
                if let Some(call) =
                    self.call_for_span(func_ref.ident_span.lo, func_ref.ident_span.hi)
                {
                    func_ref.name = qualified_name(call);
                }
            }
        }
        for token in &mut ir.lexer_tokens {
            if token.kind != "Ident" {
                continue;
            }
            if let Some(call) = self.call_for_span(token.span.lo, token.span.hi) {
                token.ident = qualified_name(call);
            }
        }
    }

    fn call_for_placeholder(&self, name: &str) -> Option<&FileModuleMemberCall> {
        self.calls.iter().find(|call| call.placeholder == name)
    }

    fn call_for_span(&self, lo: usize, hi: usize) -> Option<&FileModuleMemberCall> {
        self.calls
            .iter()
            .find(|call| call.start == lo && call.end == hi)
    }
}

pub(crate) fn analyze_file_module_member_calls(
    source: &str,
) -> Result<FileModuleCallAnalysis, ParseError> {
    let aliases = file_module_namespace_aliases(source);
    if aliases.is_empty() {
        return Ok(FileModuleCallAnalysis { calls: Vec::new() });
    }
    let tokens = tokenize_js(source);
    let mut calls = collect_file_module_member_calls(source, &tokens, &aliases);
    assign_placeholders(source, &tokens, &mut calls)?;
    Ok(FileModuleCallAnalysis { calls })
}

fn qualified_name(call: &FileModuleMemberCall) -> String {
    format!("{}::{}", call.alias, call.member)
}

fn newline_offsets(source: &str) -> Vec<usize> {
    source
        .bytes()
        .enumerate()
        .filter(|(_, byte)| *byte == b'\n' || *byte == b'\r')
        .map(|(index, _)| index)
        .collect()
}

fn line_number(source: &str, offset: usize) -> usize {
    source.as_bytes()[..offset.min(source.len())]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
        + 1
}

fn fold_error(source: &str, start: usize, end: usize, message: String) -> ParseError {
    ParseError {
        line: line_number(source, start),
        message,
        span: Some(Span::new(0, start, end)),
        code: None,
    }
}

/// Lexer keywords in the frozen parser, plus JS dialect aliases. Placeholders
/// that match these are tokenized as keywords, not identifiers.
const FROZEN_LEXER_KEYWORDS: &[&str] = &[
    "pub", "use", "import", "from", "as", "fn", "function", "struct", "let", "const", "var", "for",
    "if", "else", "match", "while", "break", "continue", "true", "false", "null", "return",
    "typeof", "require",
];

const PLACEHOLDER_FIRST: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_";
const PLACEHOLDER_REST: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_";

fn assign_placeholders(
    source: &str,
    tokens: &[Token],
    calls: &mut [FileModuleMemberCall],
) -> Result<(), ParseError> {
    let mut used = tokens
        .iter()
        .filter(|tok| tok.kind == TokenKind::Ident)
        .map(|tok| token_text(source, tok).to_string())
        .collect::<HashSet<_>>();
    used.extend(FROZEN_LEXER_KEYWORDS.iter().map(|name| (*name).to_string()));
    for call in calls.iter_mut() {
        let span = &source.as_bytes()[call.start..call.end];
        if span.iter().any(|byte| *byte == b'\n' || *byte == b'\r') {
            return Err(fold_error(
                source,
                call.start,
                call.end,
                format!(
                    "file-module member call '{}'.'{}' spans a line break; the compatibility frontend cannot fold it without shifting parser line mapping",
                    call.alias, call.member
                ),
            ));
        }
        let len = call.end - call.start;
        let Some(placeholder) = unique_placeholder(len, &mut used) else {
            return Err(fold_error(
                source,
                call.start,
                call.end,
                format!(
                    "file-module member call '{}'.'{}' cannot be folded to a collision-free identifier of {len} bytes",
                    call.alias, call.member
                ),
            ));
        };
        call.placeholder = placeholder;
    }
    Ok(())
}

fn unique_placeholder(len: usize, used: &mut HashSet<String>) -> Option<String> {
    let mut index = 0u128;
    while let Some(ident) = nth_ident(len, index) {
        if used.insert(ident.clone()) {
            return Some(ident);
        }
        index += 1;
        if index > 1_000_000 {
            break;
        }
    }
    None
}

fn nth_ident(len: usize, mut index: u128) -> Option<String> {
    if len == 0 {
        return None;
    }
    let first_len = PLACEHOLDER_FIRST.len() as u128;
    let rest_len = PLACEHOLDER_REST.len() as u128;
    let mut bytes = vec![0u8; len];
    for slot in (1..len).rev() {
        bytes[slot] = PLACEHOLDER_REST[(index % rest_len) as usize];
        index /= rest_len;
    }
    bytes[0] = PLACEHOLDER_FIRST[(index % first_len) as usize];
    index /= first_len;
    if index != 0 {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn collect_file_module_member_calls(
    source: &str,
    tokens: &[Token],
    aliases: &HashSet<String>,
) -> Vec<FileModuleMemberCall> {
    let mut calls = Vec::new();
    let mut scopes: Vec<HashSet<String>> = vec![HashSet::new()];
    let mut index = 0usize;
    while index < tokens.len() {
        if matches_keyword(source, &tokens[index], "function") {
            bind_function_scope(source, tokens, &mut index, &mut scopes);
            continue;
        }
        if matches_keyword(source, &tokens[index], "let")
            || matches_keyword(source, &tokens[index], "const")
            || matches_keyword(source, &tokens[index], "var")
        {
            bind_declaration_names(source, tokens, &mut index, &mut scopes);
            continue;
        }
        if matches_keyword(source, &tokens[index], "for") {
            index += 1;
            continue;
        }
        if tokens[index].kind == TokenKind::LBrace {
            scopes.push(HashSet::new());
            index += 1;
            continue;
        }
        if tokens[index].kind == TokenKind::RBrace {
            if scopes.len() > 1 {
                scopes.pop();
            }
            index += 1;
            continue;
        }
        if let Some(call) = match_file_module_member_call(source, tokens, index, aliases, &scopes) {
            calls.push(call);
            index += 3;
            continue;
        }
        if tokens[index].kind == TokenKind::Arrow {
            bind_arrow_params(source, tokens, index, &mut scopes);
        }
        index += 1;
    }
    calls
}

fn match_file_module_member_call(
    source: &str,
    tokens: &[Token],
    index: usize,
    aliases: &HashSet<String>,
    scopes: &[HashSet<String>],
) -> Option<FileModuleMemberCall> {
    let alias_tok = tokens.get(index)?;
    let dot = tokens.get(index + 1)?;
    let member_tok = tokens.get(index + 2)?;
    let lparen = tokens.get(index + 3)?;
    if alias_tok.kind != TokenKind::Ident
        || dot.kind != TokenKind::Dot
        || member_tok.kind != TokenKind::Ident
        || lparen.kind != TokenKind::LParen
    {
        return None;
    }
    if index > 0 && tokens[index - 1].kind == TokenKind::Dot {
        return None;
    }
    let alias = token_text(source, alias_tok);
    if !aliases.contains(alias) || is_shadowed(alias, scopes) {
        return None;
    }
    let member = token_text(source, member_tok).to_string();
    if !is_ident(member.as_str()) {
        return None;
    }
    Some(FileModuleMemberCall {
        alias: alias.to_string(),
        member,
        start: alias_tok.start,
        end: member_tok.end,
        placeholder: String::new(),
    })
}

fn bind_function_scope(
    source: &str,
    tokens: &[Token],
    index: &mut usize,
    scopes: &mut Vec<HashSet<String>>,
) {
    *index += 1;
    if tokens
        .get(*index)
        .is_some_and(|tok| tok.kind == TokenKind::Ident)
    {
        bind_name(source, &tokens[*index], scopes);
        *index += 1;
    }
    if tokens
        .get(*index)
        .is_some_and(|tok| tok.kind == TokenKind::LParen)
    {
        let mut params = HashSet::new();
        *index += 1;
        while *index < tokens.len() && tokens[*index].kind != TokenKind::RParen {
            if tokens[*index].kind == TokenKind::Ident {
                params.insert(token_text(source, &tokens[*index]).to_string());
            }
            *index += 1;
        }
        if *index < tokens.len() {
            *index += 1;
        }
        scopes.push(params);
        if tokens
            .get(*index)
            .is_some_and(|tok| tok.kind != TokenKind::LBrace)
        {
            scopes.pop();
        }
    }
}

fn bind_declaration_names(
    source: &str,
    tokens: &[Token],
    index: &mut usize,
    scopes: &mut [HashSet<String>],
) {
    *index += 1;
    while *index < tokens.len() {
        let tok = &tokens[*index];
        if tok.kind == TokenKind::Ident {
            bind_name(source, tok, scopes);
            *index += 1;
            continue;
        }
        if tok.kind == TokenKind::Comma {
            *index += 1;
            continue;
        }
        break;
    }
}

fn bind_arrow_params(
    source: &str,
    tokens: &[Token],
    arrow_index: usize,
    scopes: &mut [HashSet<String>],
) {
    let Some(current) = scopes.last_mut() else {
        return;
    };
    if arrow_index == 0 {
        return;
    }
    let prev = &tokens[arrow_index - 1];
    if prev.kind == TokenKind::Ident {
        current.insert(token_text(source, prev).to_string());
        return;
    }
    if prev.kind != TokenKind::RParen {
        return;
    }
    let mut depth = 1i32;
    let mut cursor = arrow_index - 1;
    while cursor > 0 && depth > 0 {
        cursor -= 1;
        match tokens[cursor].kind {
            TokenKind::RParen => depth += 1,
            TokenKind::LParen => depth -= 1,
            TokenKind::Ident if depth == 1 => {
                current.insert(token_text(source, &tokens[cursor]).to_string());
            }
            _ => {}
        }
    }
}

fn bind_name(source: &str, tok: &Token, scopes: &mut [HashSet<String>]) {
    if let Some(current) = scopes.last_mut() {
        current.insert(token_text(source, tok).to_string());
    }
}

fn is_shadowed(name: &str, scopes: &[HashSet<String>]) -> bool {
    scopes.iter().rev().any(|scope| scope.contains(name))
}

fn matches_keyword(source: &str, tok: &Token, keyword: &str) -> bool {
    tok.kind == TokenKind::Ident && token_text(source, tok) == keyword
}

fn token_text<'a>(source: &'a str, tok: &Token) -> &'a str {
    &source[tok.start..tok.end]
}

fn is_ident(input: &str) -> bool {
    let mut chars = input.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn tokenize_js(source: &str) -> Vec<Token> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0usize;
    let mut last_significant: Option<TokenKind> = None;
    while index < bytes.len() {
        let ch = bytes[index];
        if ch == b'/' && index + 1 < bytes.len() {
            if bytes[index + 1] == b'/' {
                index = skip_line_comment(bytes, index);
                continue;
            }
            if bytes[index + 1] == b'*' {
                index = skip_block_comment(bytes, index);
                continue;
            }
            if can_start_regex(last_significant)
                && let Some(end) = skip_regex_literal(bytes, index)
            {
                last_significant = Some(TokenKind::Other);
                index = end;
                continue;
            }
        }
        if ch == b'"' || ch == b'\'' {
            let end = skip_quoted(bytes, index, ch);
            tokens.push(Token {
                kind: TokenKind::String,
                start: index,
                end,
            });
            last_significant = Some(TokenKind::String);
            index = end;
            continue;
        }
        if ch == b'`' {
            index = skip_template(bytes, index);
            last_significant = Some(TokenKind::String);
            continue;
        }
        if ch.is_ascii_whitespace() {
            index += 1;
            continue;
        }
        if ident_start(ch) {
            let end = ident_end(bytes, index);
            tokens.push(Token {
                kind: TokenKind::Ident,
                start: index,
                end,
            });
            last_significant = Some(TokenKind::Ident);
            index = end;
            continue;
        }
        if ch.is_ascii_digit() {
            let end = number_end(bytes, index);
            tokens.push(Token {
                kind: TokenKind::Number,
                start: index,
                end,
            });
            last_significant = Some(TokenKind::Number);
            index = end;
            continue;
        }
        if ch == b'=' && index + 1 < bytes.len() && bytes[index + 1] == b'>' {
            tokens.push(Token {
                kind: TokenKind::Arrow,
                start: index,
                end: index + 2,
            });
            last_significant = Some(TokenKind::Arrow);
            index += 2;
            continue;
        }
        let kind = match ch {
            b'.' => TokenKind::Dot,
            b'*' => TokenKind::Star,
            b'(' => TokenKind::LParen,
            b')' => TokenKind::RParen,
            b'{' => TokenKind::LBrace,
            b'}' => TokenKind::RBrace,
            b'[' => TokenKind::LBracket,
            b']' => TokenKind::RBracket,
            b',' => TokenKind::Comma,
            b';' => TokenKind::Semi,
            _ => TokenKind::Other,
        };
        let width = if ch < 0x80 {
            1
        } else {
            source[index..]
                .chars()
                .next()
                .map(|c| c.len_utf8())
                .unwrap_or(1)
        };
        tokens.push(Token {
            kind,
            start: index,
            end: index + width,
        });
        last_significant = Some(kind);
        index += width;
    }
    tokens
}

fn ident_start(ch: u8) -> bool {
    ch.is_ascii_alphabetic() || ch == b'_' || ch == b'$'
}

fn ident_continue(ch: u8) -> bool {
    ident_start(ch) || ch.is_ascii_digit()
}

fn ident_end(bytes: &[u8], start: usize) -> usize {
    let mut end = start + 1;
    while end < bytes.len() && ident_continue(bytes[end]) {
        end += 1;
    }
    end
}

fn number_end(bytes: &[u8], start: usize) -> usize {
    let mut end = start + 1;
    while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
        end += 1;
    }
    end
}

fn skip_line_comment(bytes: &[u8], start: usize) -> usize {
    let mut index = start + 2;
    while index < bytes.len() && bytes[index] != b'\n' {
        index += 1;
    }
    index
}

fn skip_block_comment(bytes: &[u8], start: usize) -> usize {
    let mut index = start + 2;
    while index + 1 < bytes.len() {
        if bytes[index] == b'*' && bytes[index + 1] == b'/' {
            return index + 2;
        }
        index += 1;
    }
    bytes.len()
}

fn skip_quoted(bytes: &[u8], start: usize, quote: u8) -> usize {
    let mut index = start + 1;
    let mut escaped = false;
    while index < bytes.len() {
        let ch = bytes[index];
        if escaped {
            escaped = false;
        } else if ch == b'\\' {
            escaped = true;
        } else if ch == quote {
            return index + 1;
        }
        index += 1;
    }
    bytes.len()
}

fn skip_template(bytes: &[u8], start: usize) -> usize {
    let mut index = start + 1;
    let mut escaped = false;
    while index < bytes.len() {
        let ch = bytes[index];
        if escaped {
            escaped = false;
            index += 1;
            continue;
        }
        if ch == b'\\' {
            escaped = true;
            index += 1;
            continue;
        }
        if ch == b'`' {
            return index + 1;
        }
        if ch == b'$' && index + 1 < bytes.len() && bytes[index + 1] == b'{' {
            index += 2;
            let mut depth = 1i32;
            while index < bytes.len() && depth > 0 {
                match bytes[index] {
                    b'{' => depth += 1,
                    b'}' => depth -= 1,
                    b'"' | b'\'' => index = skip_quoted(bytes, index, bytes[index]) - 1,
                    b'`' => index = skip_template(bytes, index) - 1,
                    _ => {}
                }
                index += 1;
            }
            continue;
        }
        index += 1;
    }
    bytes.len()
}

fn skip_regex_literal(bytes: &[u8], start: usize) -> Option<usize> {
    let mut index = start + 1;
    let mut escaped = false;
    let mut in_class = false;
    while index < bytes.len() {
        let ch = bytes[index];
        if escaped {
            escaped = false;
            index += 1;
            continue;
        }
        if ch == b'\\' {
            escaped = true;
            index += 1;
            continue;
        }
        if ch == b'\n' {
            return None;
        }
        if ch == b'[' {
            in_class = true;
        } else if ch == b']' {
            in_class = false;
        } else if ch == b'/' && !in_class {
            index += 1;
            while index < bytes.len() && bytes[index].is_ascii_alphabetic() {
                index += 1;
            }
            return Some(index);
        }
        index += 1;
    }
    None
}

fn can_start_regex(previous: Option<TokenKind>) -> bool {
    !matches!(
        previous,
        Some(
            TokenKind::Ident
                | TokenKind::Number
                | TokenKind::String
                | TokenKind::RParen
                | TokenKind::RBracket
                | TokenKind::RBrace
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analysis_source() -> &'static str {
        concat!(
            "import * as string from \"./strings.rss\";\n",
            "const single = 'string.non_empty(';\n",
            "const double = \"string.non_empty(\";\n",
            "const tmpl = \"`string.non_empty(`\";\n",
            "const re = \"/string.non_empty(/\";\n",
            "// string.non_empty(\"no\")\n",
            "/* string.non_empty(\"no\") */\n",
            "const utf = \"是\";\n",
            "string.non_empty(\"yes\");\n",
        )
    }

    fn assert_fold_preserves_layout(source: &str, analysis: &FileModuleCallAnalysis) -> String {
        let folded = analysis
            .parse_source(source)
            .expect("safe fold must succeed")
            .into_owned();
        assert_eq!(
            folded.len(),
            source.len(),
            "fold must not change byte length"
        );
        assert_eq!(
            newline_offsets(&folded),
            newline_offsets(source),
            "fold must keep every \\n and \\r at the original offset"
        );
        for call in &analysis.calls {
            let original = &source[call.start..call.end];
            let folded_callee = &folded[call.start..call.end];
            assert_eq!(folded_callee, call.placeholder);
            assert_eq!(folded_callee.len(), original.len());
            assert!(
                !folded_callee.contains('.'),
                "folded callee must be a single identifier, got {folded_callee:?}"
            );
            assert!(
                !folded_callee.as_bytes().contains(&b'\n')
                    && !folded_callee.as_bytes().contains(&b'\r'),
                "folded callee must not swallow line breaks, got {folded_callee:?}"
            );
            assert!(
                is_ident(folded_callee),
                "folded callee must be a valid identifier, got {folded_callee:?}"
            );
        }
        folded
    }

    fn assert_no_placeholder_leak(ir: &FrontendIr, analysis: &FileModuleCallAnalysis) {
        for call in &analysis.calls {
            let placeholder = call.placeholder.as_str();
            assert!(
                ir.functions.iter().all(|func| func.name != placeholder),
                "function table leaked placeholder {placeholder}, got {:?}",
                ir.functions
                    .iter()
                    .map(|func| &func.name)
                    .collect::<Vec<_>>()
            );
            assert!(
                ir.implicit_extern_names
                    .iter()
                    .all(|name| name != placeholder),
                "implicit externs leaked placeholder {placeholder}, got {:?}",
                ir.implicit_extern_names
            );
            let index = ir
                .parsed_semantic_index
                .as_ref()
                .expect("parser-produced semantic index");
            assert!(
                index.call_sites.iter().all(|site| site.name != placeholder),
                "call sites leaked placeholder {placeholder}"
            );
            assert!(
                index.func_decls.iter().all(|decl| decl.name != placeholder),
                "func decls leaked placeholder {placeholder}"
            );
            assert!(
                index
                    .func_refs
                    .iter()
                    .all(|func_ref| func_ref.name != placeholder),
                "func refs leaked placeholder {placeholder}"
            );
            assert!(
                ir.lexer_tokens
                    .iter()
                    .all(|token| token.ident != placeholder),
                "lexer tokens leaked placeholder {placeholder}"
            );
        }
    }

    #[test]
    fn lookalike_literals_are_not_file_module_calls() {
        let source = analysis_source();
        let analysis = analyze_file_module_member_calls(source).expect("analyze");
        assert_eq!(analysis.calls.len(), 1);
        assert_eq!(analysis.calls[0].alias, "string");
        assert_eq!(analysis.calls[0].member, "non_empty");
        assert_eq!(
            &source[analysis.calls[0].start..analysis.calls[0].end],
            "string.non_empty"
        );
        let parse_source = assert_fold_preserves_layout(source, &analysis);
        assert!(parse_source.contains("const single = 'string.non_empty(';"));
        assert!(parse_source.contains(r#"const double = "string.non_empty(";"#));
        assert!(parse_source.contains(r#"const tmpl = "`string.non_empty(`";"#));
        assert!(parse_source.contains(r#"const re = "/string.non_empty(/";"#));
        assert!(parse_source.contains("是"));
        assert!(!parse_source.contains("__pdns"));
        assert_ne!(
            &parse_source[analysis.calls[0].start..analysis.calls[0].end],
            "string.non_empty"
        );
    }

    #[test]
    fn same_name_locals_object_members_nested_computed_optional_are_not_namespace_calls() {
        let source = concat!(
            "import * as string from \"./strings.rss\";\n",
            "string.non_empty(\"yes\");\n",
            "{\n",
            "  const string = {};\n",
            "  string.non_empty(\"no\");\n",
            "}\n",
            "const box = { non_empty: 1 };\n",
            "box.non_empty;\n",
            "obj.string.non_empty(\"no\");\n",
            "string[\"non_empty\"](\"no\");\n",
            "string?.non_empty(\"no\");\n",
        );
        let analysis = analyze_file_module_member_calls(source).expect("analyze");
        assert_eq!(analysis.calls.len(), 1);
        assert_eq!(analysis.calls[0].alias, "string");
        assert_eq!(analysis.calls[0].member, "non_empty");
        assert_eq!(
            &source[analysis.calls[0].start..analysis.calls[0].end],
            "string.non_empty"
        );
        assert!(source[..analysis.calls[0].start].contains("import * as string"));
        assert!(source[analysis.calls[0].end..].starts_with("(\"yes\")"));
    }

    #[test]
    fn lowered_ir_keeps_qualified_names_and_original_callee_spans() {
        let source = analysis_source();
        let analysis = analyze_file_module_member_calls(source).expect("analyze");
        let ir = crate::javascript::lower_to_ir(source).expect("original lookalikes must parse");
        assert!(
            ir.functions
                .iter()
                .any(|func| func.name == "string::non_empty"),
            "function table must carry the qualified file-module call, got {:?}",
            ir.functions
                .iter()
                .map(|func| &func.name)
                .collect::<Vec<_>>()
        );
        assert!(
            ir.implicit_extern_names
                .iter()
                .any(|name| name == "string::non_empty"),
            "implicit externs must carry the qualified file-module call, got {:?}",
            ir.implicit_extern_names
        );
        assert_no_placeholder_leak(&ir, &analysis);
        let index = ir
            .parsed_semantic_index
            .as_ref()
            .expect("parser-produced semantic index");
        let site = index
            .call_sites
            .iter()
            .find(|site| site.is_namespace_call && site.name == "string::non_empty")
            .expect("qualified namespace call site");
        assert_eq!(
            source.get(site.callee_span.lo..site.callee_span.hi),
            Some("string.non_empty")
        );
    }

    #[test]
    fn user_local_m0_is_not_rewritten_as_namespace_target() {
        let source = concat!(
            "import * as a from \"./m.rss\";\n",
            "const m0 = 2;\n",
            "a.b();\n",
            "m0;\n",
        );
        let analysis = analyze_file_module_member_calls(source).expect("analyze");
        assert_eq!(analysis.calls.len(), 1);
        assert_ne!(analysis.calls[0].placeholder, "m0");
        assert_fold_preserves_layout(source, &analysis);
        let ir = crate::javascript::lower_to_ir(source).expect("local m0 must parse");
        assert_no_placeholder_leak(&ir, &analysis);
        let index = ir
            .parsed_semantic_index
            .as_ref()
            .expect("parser-produced semantic index");
        assert!(
            index.local_decls.iter().any(|decl| decl.name == "m0"),
            "user local m0 must remain a local, got {:?}",
            index
                .local_decls
                .iter()
                .map(|decl| &decl.name)
                .collect::<Vec<_>>()
        );
        assert!(
            ir.functions.iter().all(|func| func.name != "m0"),
            "local m0 must not become a function, got {:?}",
            ir.functions
                .iter()
                .map(|func| &func.name)
                .collect::<Vec<_>>()
        );
        assert_eq!(ir.implicit_extern_names, vec!["a::b".to_string()]);
        let site = index
            .call_sites
            .iter()
            .find(|site| site.is_namespace_call)
            .expect("namespace call");
        assert_eq!(site.name, "a::b");
        assert_eq!(
            source.get(site.callee_span.lo..site.callee_span.hi),
            Some("a.b")
        );
    }

    #[test]
    fn user_function_m0_is_not_renamed_with_file_module_call() {
        let source = concat!(
            "import * as a from \"./m.rss\";\n",
            "function m0() { return 1; }\n",
            "a.b();\n",
            "m0();\n",
        );
        let analysis = analyze_file_module_member_calls(source).expect("analyze");
        assert_eq!(analysis.calls.len(), 1);
        assert_ne!(analysis.calls[0].placeholder, "m0");
        assert_fold_preserves_layout(source, &analysis);
        let ir = crate::javascript::lower_to_ir(source).expect("function m0 must parse");
        assert_no_placeholder_leak(&ir, &analysis);
        assert!(
            ir.functions.iter().any(|func| func.name == "m0"),
            "user function m0 must keep its name, got {:?}",
            ir.functions
                .iter()
                .map(|func| &func.name)
                .collect::<Vec<_>>()
        );
        assert!(
            ir.functions.iter().any(|func| func.name == "a::b"),
            "file-module call must lower to a::b, got {:?}",
            ir.functions
                .iter()
                .map(|func| &func.name)
                .collect::<Vec<_>>()
        );
        let index = ir
            .parsed_semantic_index
            .as_ref()
            .expect("parser-produced semantic index");
        assert!(
            index.func_decls.iter().any(|decl| decl.name == "m0"),
            "func_decls must keep user function m0, got {:?}",
            index
                .func_decls
                .iter()
                .map(|decl| &decl.name)
                .collect::<Vec<_>>()
        );
        assert!(
            index.func_decls.iter().all(|decl| decl.name != "a::b"),
            "IR lowering must not rewrite every declaration sharing placeholder text"
        );
        let names: Vec<&str> = index
            .call_sites
            .iter()
            .map(|site| site.name.as_str())
            .collect();
        assert!(
            names.contains(&"a::b"),
            "namespace call site missing, got {names:?}"
        );
        assert!(
            names.contains(&"m0"),
            "user m0() call site missing, got {names:?}"
        );
    }

    #[test]
    fn many_short_calls_keep_unique_same_length_placeholders() {
        let mut source = String::from("import * as a from \"./m.rss\";\n");
        for _ in 0..110 {
            source.push_str("a.b();\n");
        }
        let analysis = analyze_file_module_member_calls(&source).expect("analyze");
        assert_eq!(analysis.calls.len(), 110);
        let mut placeholders = HashSet::new();
        for call in &analysis.calls {
            assert_eq!(call.end - call.start, 3);
            assert_eq!(call.placeholder.len(), 3);
            assert_eq!(&source[call.start..call.end], "a.b");
            assert!(
                placeholders.insert(call.placeholder.clone()),
                "placeholder {} reused",
                call.placeholder
            );
        }
        let folded = assert_fold_preserves_layout(&source, &analysis);
        let ir = crate::javascript::lower_to_ir(&source).expect("110 short calls must parse");
        assert_no_placeholder_leak(&ir, &analysis);
        assert!(
            ir.functions.iter().all(|func| func.name == "a::b"),
            "every implicit extern must lower to a::b, got {:?}",
            ir.functions
                .iter()
                .map(|func| &func.name)
                .collect::<Vec<_>>()
        );
        assert!(
            ir.implicit_extern_names.iter().all(|name| name == "a::b"),
            "implicit externs must all be a::b, got {:?}",
            ir.implicit_extern_names
        );
        let index = ir
            .parsed_semantic_index
            .as_ref()
            .expect("parser-produced semantic index");
        let sites: Vec<_> = index
            .call_sites
            .iter()
            .filter(|site| site.is_namespace_call)
            .collect();
        assert_eq!(sites.len(), 110);
        for site in sites {
            assert_eq!(site.name, "a::b");
            assert_eq!(
                source.get(site.callee_span.lo..site.callee_span.hi),
                Some("a.b")
            );
            assert_eq!(&folded[site.callee_span.lo..site.callee_span.hi].len(), &3);
        }
    }

    #[test]
    fn distinct_same_length_qualified_calls_keep_separate_targets() {
        let source = concat!(
            "import * as ab from \"./left.rss\";\n",
            "import * as cd from \"./right.rss\";\n",
            "ab.xy();\n",
            "cd.uv();\n",
        );
        let analysis = analyze_file_module_member_calls(source).expect("analyze");
        assert_eq!(analysis.calls.len(), 2);
        assert_eq!(analysis.calls[0].end - analysis.calls[0].start, 5);
        assert_eq!(analysis.calls[1].end - analysis.calls[1].start, 5);
        assert_ne!(analysis.calls[0].placeholder, analysis.calls[1].placeholder);
        assert_fold_preserves_layout(source, &analysis);
        let ir = crate::javascript::lower_to_ir(source).expect("distinct calls must parse");
        assert_no_placeholder_leak(&ir, &analysis);
        let mut names = ir
            .functions
            .iter()
            .map(|func| func.name.as_str())
            .collect::<Vec<_>>();
        names.sort_unstable();
        names.dedup();
        assert!(
            names.contains(&"ab::xy") && names.contains(&"cd::uv"),
            "distinct qualified targets must survive, got {names:?}"
        );
        let index = ir
            .parsed_semantic_index
            .as_ref()
            .expect("parser-produced semantic index");
        let sites: Vec<_> = index
            .call_sites
            .iter()
            .filter(|site| site.is_namespace_call)
            .map(|site| {
                (
                    site.name.as_str(),
                    source.get(site.callee_span.lo..site.callee_span.hi),
                )
            })
            .collect();
        assert!(sites.contains(&("ab::xy", Some("ab.xy"))));
        assert!(sites.contains(&("cd::uv", Some("cd.uv"))));
    }

    #[test]
    fn alias_member_lengths_near_ident_boundaries() {
        let source = concat!(
            "import * as a from \"./a.rss\";\n",
            "import * as aa from \"./aa.rss\";\n",
            "a.b();\n",
            "aa.b();\n",
            "a.bb();\n",
        );
        let analysis = analyze_file_module_member_calls(source).expect("analyze");
        assert_eq!(analysis.calls.len(), 3);
        let lens: Vec<usize> = analysis
            .calls
            .iter()
            .map(|call| call.placeholder.len())
            .collect();
        assert_eq!(lens, vec![3, 4, 4]);
        assert_eq!(
            &source[analysis.calls[0].start..analysis.calls[0].end],
            "a.b"
        );
        assert_eq!(
            &source[analysis.calls[1].start..analysis.calls[1].end],
            "aa.b"
        );
        assert_eq!(
            &source[analysis.calls[2].start..analysis.calls[2].end],
            "a.bb"
        );
        assert_fold_preserves_layout(source, &analysis);
        let ir = crate::javascript::lower_to_ir(source).expect("boundary lengths must parse");
        assert_no_placeholder_leak(&ir, &analysis);
        let mut names = ir.implicit_extern_names.to_vec();
        names.sort();
        assert_eq!(
            names,
            vec!["a::b".to_string(), "a::bb".to_string(), "aa::b".to_string()]
        );
    }

    #[test]
    fn multiline_alias_member_call_fails_closed() {
        let source = concat!(
            "import * as string from \"./strings.rss\";\n",
            "string\n",
            ".non_empty(\"x\");\n",
        );
        let error =
            analyze_file_module_member_calls(source).expect_err("multiline must fail closed");
        assert!(
            error.message.contains("spans a line break"),
            "diagnostic must name the unsupported construct, got {}",
            error.message
        );
        assert!(
            error.message.contains("string") && error.message.contains("non_empty"),
            "diagnostic must name the original target, got {}",
            error.message
        );
        assert_eq!(error.line, 2);
        let span = error.span.expect("multiline fold error must carry a span");
        assert!(source[span.lo..span.hi].contains('\n'));
        assert!(source[span.lo..span.hi].contains("string"));
        assert!(source[span.lo..span.hi].contains("non_empty"));
    }

    #[test]
    fn multiline_dot_before_member_fails_closed() {
        let source = concat!(
            "import * as string from \"./strings.rss\";\n",
            "string.\n",
            "non_empty(\"x\");\n",
        );
        let error =
            analyze_file_module_member_calls(source).expect_err("multiline must fail closed");
        assert!(error.message.contains("spans a line break"));
        assert_eq!(error.line, 2);
    }

    #[test]
    fn carriage_return_in_callee_fails_closed() {
        let source = "import * as string from \"./strings.rss\";\nstring\r.non_empty(\"x\");\n";
        let error =
            analyze_file_module_member_calls(source).expect_err("CR callee must fail closed");
        assert!(error.message.contains("spans a line break"));
        assert!(source[error.span.expect("span").lo..error.span.expect("span").hi].contains('\r'));
    }

    #[test]
    fn carriage_return_outside_callee_is_preserved() {
        let source = concat!(
            "import * as a from \"./m.rss\";\n",
            "const cr = \"x\ry\";\n",
            "a.b();\n",
        );
        let analysis = analyze_file_module_member_calls(source).expect("analyze");
        let folded = assert_fold_preserves_layout(source, &analysis);
        assert!(folded.contains('\r'));
        assert_eq!(folded.find('\r'), source.find('\r'));
    }
}
