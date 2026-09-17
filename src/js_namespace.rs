use std::collections::{HashMap, HashSet};

use vm::{FrontendIr, ImportClause};

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

/// Lower `alias.member(` file-module calls to unique placeholders, then map those
/// placeholders onto qualified `alias::member` names in IR.
pub(crate) fn lower_file_module_namespace_calls(
    source: &str,
    aliases: &HashSet<String>,
) -> (String, HashMap<String, String>) {
    if aliases.is_empty() {
        return (source.to_string(), HashMap::new());
    }
    let tokens = tokenize_js(source);
    let rewrites = collect_file_module_call_rewrites(source, &tokens, aliases);
    if rewrites.is_empty() {
        return (source.to_string(), HashMap::new());
    }
    let mut out = String::with_capacity(source.len());
    let mut last = 0usize;
    let mut renames = HashMap::new();
    for (index, rewrite) in rewrites.into_iter().enumerate() {
        out.push_str(&source[last..rewrite.start]);
        let placeholder = format!("__pdns{index}");
        let original_len = rewrite.end - rewrite.start;
        if placeholder.len() <= original_len {
            for _ in 0..(original_len - placeholder.len()) {
                out.push(' ');
            }
            out.push_str(&placeholder);
        } else {
            out.push_str(&placeholder);
        }
        renames.insert(
            placeholder,
            format!("{}::{}", rewrite.alias, rewrite.member),
        );
        last = rewrite.end;
    }
    out.push_str(&source[last..]);
    (out, renames)
}

pub(crate) fn apply_file_module_call_renames(
    ir: &mut FrontendIr,
    renames: &HashMap<String, String>,
) {
    if renames.is_empty() {
        return;
    }
    for func in &mut ir.functions {
        if let Some(qualified) = renames.get(&func.name) {
            func.name = qualified.clone();
        }
    }
    for name in &mut ir.implicit_extern_names {
        if let Some(qualified) = renames.get(name) {
            *name = qualified.clone();
        }
    }
}

struct CallRewrite {
    start: usize,
    end: usize,
    alias: String,
    member: String,
}

fn collect_file_module_call_rewrites(
    source: &str,
    tokens: &[Token],
    aliases: &HashSet<String>,
) -> Vec<CallRewrite> {
    let mut rewrites = Vec::new();
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
        if let Some(rewrite) =
            match_file_module_member_call(source, tokens, index, aliases, &scopes)
        {
            rewrites.push(rewrite);
            index += 3;
            continue;
        }
        if tokens[index].kind == TokenKind::Arrow {
            bind_arrow_params(source, tokens, index, &mut scopes);
        }
        index += 1;
    }
    rewrites
}

fn match_file_module_member_call(
    source: &str,
    tokens: &[Token],
    index: usize,
    aliases: &HashSet<String>,
    scopes: &[HashSet<String>],
) -> Option<CallRewrite> {
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
    Some(CallRewrite {
        start: alias_tok.start,
        end: member_tok.end,
        alias: alias.to_string(),
        member,
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
