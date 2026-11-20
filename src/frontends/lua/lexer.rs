//! Scanner for the locked LuaJIT 2.1 Lua 5.2-compat text grammar.
//!
//! Character classes match `lj_char.c`: ASCII letters, digits, `_`, and every
//! byte >= 128 are identifier characters. A leading BOM is recorded and
//! skipped. Line directives and shebangs are consumed here so the parser sees
//! only tokens.

use super::ast::{NumberKind, Span};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxError {
    pub span: Span,
    pub line: u32,
    pub message: String,
}

impl SyntaxError {
    pub fn at(span: Span, line: u32, message: impl Into<String>) -> Self {
        Self {
            span,
            line,
            message: message.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tok {
    Eof,
    Name,
    String,
    Number,
    Dots,
    Concat,
    Eq,
    Ne,
    NeBang,
    Le,
    Ge,
    Shl,
    Shr,
    Sar,
    AndAnd,
    OrOr,
    Coal,
    Nav,
    Arrow,
    Label,
    And,
    Break,
    Do,
    Else,
    Elseif,
    End,
    False,
    For,
    Function,
    Goto,
    If,
    In,
    Local,
    Nil,
    Not,
    Or,
    Repeat,
    Return,
    Then,
    True,
    Until,
    While,
    Const,
    Continue,
    Char(u8),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: Tok,
    pub span: Span,
    pub line: u32,
    pub text: Vec<u8>,
    pub number_kind: Option<NumberKind>,
}

pub struct Lexed {
    pub tokens: Vec<Token>,
    pub has_bom: bool,
}

struct Scan<'a> {
    src: &'a [u8],
    pos: usize,
    line: u32,
    has_bom: bool,
}

pub fn lex(source: &[u8]) -> Result<Lexed, SyntaxError> {
    let mut scan = Scan {
        src: source,
        pos: 0,
        line: 1,
        has_bom: false,
    };
    if source.starts_with(&[0xef, 0xbb, 0xbf]) {
        scan.pos = 3;
        scan.has_bom = true;
    }
    let mut tokens = Vec::new();
    if scan.cur() == Some(b'#') {
        if scan.peek() == Some(b'!') {
            scan.skip_shebang()?;
        } else {
            // A leading '#' that is not '#!' is a line directive, as if a
            // newline had just been consumed. Do not consume the '#" here.
            scan.line = 1;
            let hash = scan.pos;
            if !scan.line_directive()? {
                tokens.push(scan.token(Tok::Char(b'#'), hash, hash + 1, Vec::new(), None));
            }
        }
    }
    while scan.cur().is_some() {
        if let Some(token) = scan.next_token()? {
            let done = token.kind == Tok::Eof;
            tokens.push(token);
            if done {
                break;
            }
        }
    }
    if tokens.last().is_none_or(|token| token.kind != Tok::Eof) {
        let end = u32::try_from(source.len()).unwrap_or(u32::MAX);
        tokens.push(Token {
            kind: Tok::Eof,
            span: Span { start: end, end },
            line: scan.line,
            text: Vec::new(),
            number_kind: None,
        });
    }
    Ok(Lexed {
        tokens,
        has_bom: scan.has_bom,
    })
}

impl<'a> Scan<'a> {
    fn cur(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos + 1).copied()
    }

    fn advance(&mut self) -> Option<u8> {
        let cur = self.cur();
        if self.pos < self.src.len() {
            self.pos += 1;
        }
        cur
    }

    fn token(
        &self,
        kind: Tok,
        start: usize,
        end: usize,
        text: Vec<u8>,
        number_kind: Option<NumberKind>,
    ) -> Token {
        Token {
            kind,
            span: Span::new(start, end),
            line: self.line,
            text,
            number_kind,
        }
    }

    fn err(&self, start: usize, message: impl Into<String>) -> SyntaxError {
        SyntaxError::at(Span::new(start, self.pos), self.line, message)
    }

    fn is_eol(c: u8) -> bool {
        c == b'\n' || c == b'\r'
    }

    fn finish_newline(&mut self) {
        let old = self.cur();
        self.advance();
        if let Some(next) = self.cur() {
            if Self::is_eol(next) && Some(next) != old {
                self.advance();
            }
        }
        self.line = self.line.saturating_add(1);
    }

    fn skip_shebang(&mut self) -> Result<(), SyntaxError> {
        while let Some(c) = self.cur() {
            if Self::is_eol(c) {
                self.finish_newline();
                return Ok(());
            }
            self.advance();
        }
        Ok(())
    }

    fn line_directive(&mut self) -> Result<bool, SyntaxError> {
        let start = self.pos;
        if self.cur() != Some(b'#') {
            return Ok(false);
        }
        self.advance();
        while matches!(self.cur(), Some(b' ' | b'\t')) {
            self.advance();
        }
        let Some(c) = self.cur() else {
            return Ok(false);
        };
        if !c.is_ascii_digit() {
            return Ok(false);
        }
        let mut line: u32 = u32::from(c - b'0');
        self.advance();
        while let Some(digit) = self.cur() {
            if !digit.is_ascii_digit() {
                break;
            }
            if line >= (i32::MAX as u32 - 9) / 10 {
                return Err(self.err(start, "malformed line number directive"));
            }
            line = line * 10 + u32::from(digit - b'0');
            self.advance();
        }
        while matches!(self.cur(), Some(b' ' | b'\t')) {
            self.advance();
        }
        if self.cur() == Some(b'"') {
            let before = self.line;
            self.advance();
            self.short_string(b'"', start)?;
            if self.line != before {
                return Err(self.err(start, "malformed line number directive"));
            }
            while matches!(self.cur(), Some(b' ' | b'\t')) {
                self.advance();
            }
        }
        match self.cur() {
            Some(c) if Self::is_eol(c) => self.finish_newline(),
            None => {}
            _ => return Err(self.err(start, "malformed line number directive")),
        }
        self.line = line;
        Ok(true)
    }

    fn next_token(&mut self) -> Result<Option<Token>, SyntaxError> {
        loop {
            let Some(c) = self.cur() else {
                return Ok(Some(self.token(
                    Tok::Eof,
                    self.pos,
                    self.pos,
                    Vec::new(),
                    None,
                )));
            };
            if is_ident(c) {
                if c.is_ascii_digit() {
                    return Ok(Some(self.number(self.pos, false)?));
                }
                return Ok(Some(self.name_or_keyword()));
            }
            let start = self.pos;
            match c {
                b'\n' | b'\r' => {
                    self.finish_newline();
                    while self.cur() == Some(b'#') {
                        let hash = self.pos;
                        if !self.line_directive()? {
                            return Ok(Some(self.token(
                                Tok::Char(b'#'),
                                hash,
                                hash + 1,
                                Vec::new(),
                                None,
                            )));
                        }
                    }
                }
                b' ' | b'\t' | 0x0b | 0x0c => {
                    self.advance();
                }
                b'-' => {
                    self.advance();
                    if self.cur() == Some(b'>') {
                        self.advance();
                        return Ok(Some(self.token(
                            Tok::Arrow,
                            start,
                            self.pos,
                            Vec::new(),
                            None,
                        )));
                    }
                    if self.cur() != Some(b'-') {
                        return Ok(Some(self.token(
                            Tok::Char(b'-'),
                            start,
                            self.pos,
                            Vec::new(),
                            None,
                        )));
                    }
                    self.advance();
                    if self.cur() == Some(b'[') {
                        let sep = self.skip_eq(b'[')?;
                        if sep >= 0 {
                            self.long_string(sep, false)?;
                            continue;
                        }
                    }
                    while let Some(ch) = self.cur() {
                        if Self::is_eol(ch) {
                            break;
                        }
                        self.advance();
                    }
                }
                b'[' => {
                    let sep = self.skip_eq(b'[')?;
                    if sep >= 0 {
                        let bytes = self.long_string(sep, true)?;
                        return Ok(Some(self.token(Tok::String, start, self.pos, bytes, None)));
                    } else if sep == -1 {
                        return Ok(Some(self.token(
                            Tok::Char(b'['),
                            start,
                            self.pos,
                            Vec::new(),
                            None,
                        )));
                    } else {
                        return Err(self.err(start, "invalid long string delimiter"));
                    }
                }
                b'=' => {
                    self.advance();
                    if self.cur() == Some(b'=') {
                        self.advance();
                        return Ok(Some(self.token(Tok::Eq, start, self.pos, Vec::new(), None)));
                    }
                    return Ok(Some(self.token(
                        Tok::Char(b'='),
                        start,
                        self.pos,
                        Vec::new(),
                        None,
                    )));
                }
                b'<' => {
                    self.advance();
                    if self.cur() == Some(b'=') {
                        self.advance();
                        return Ok(Some(self.token(Tok::Le, start, self.pos, Vec::new(), None)));
                    }
                    if self.cur() == Some(b'<') {
                        self.advance();
                        return Ok(Some(self.token(
                            Tok::Shl,
                            start,
                            self.pos,
                            Vec::new(),
                            None,
                        )));
                    }
                    return Ok(Some(self.token(
                        Tok::Char(b'<'),
                        start,
                        self.pos,
                        Vec::new(),
                        None,
                    )));
                }
                b'>' => {
                    self.advance();
                    if self.cur() == Some(b'=') {
                        self.advance();
                        return Ok(Some(self.token(Tok::Ge, start, self.pos, Vec::new(), None)));
                    }
                    if self.cur() == Some(b'>') {
                        self.advance();
                        return Ok(Some(self.token(
                            Tok::Shr,
                            start,
                            self.pos,
                            Vec::new(),
                            None,
                        )));
                    }
                    return Ok(Some(self.token(
                        Tok::Char(b'>'),
                        start,
                        self.pos,
                        Vec::new(),
                        None,
                    )));
                }
                b'~' => {
                    self.advance();
                    if self.cur() == Some(b'=') {
                        self.advance();
                        return Ok(Some(self.token(Tok::Ne, start, self.pos, Vec::new(), None)));
                    }
                    if self.cur() == Some(b'>') {
                        self.advance();
                        if self.cur() != Some(b'>') {
                            return Err(self.err(start, "invalid symbol"));
                        }
                        self.advance();
                        return Ok(Some(self.token(
                            Tok::Sar,
                            start,
                            self.pos,
                            Vec::new(),
                            None,
                        )));
                    }
                    return Ok(Some(self.token(
                        Tok::Char(b'~'),
                        start,
                        self.pos,
                        Vec::new(),
                        None,
                    )));
                }
                b'!' => {
                    self.advance();
                    if self.cur() == Some(b'=') {
                        self.advance();
                        return Ok(Some(self.token(
                            Tok::NeBang,
                            start,
                            self.pos,
                            Vec::new(),
                            None,
                        )));
                    }
                    return Ok(Some(self.token(
                        Tok::Char(b'!'),
                        start,
                        self.pos,
                        Vec::new(),
                        None,
                    )));
                }
                b':' => {
                    self.advance();
                    if self.cur() == Some(b':') {
                        self.advance();
                        return Ok(Some(self.token(
                            Tok::Label,
                            start,
                            self.pos,
                            Vec::new(),
                            None,
                        )));
                    }
                    return Ok(Some(self.token(
                        Tok::Char(b':'),
                        start,
                        self.pos,
                        Vec::new(),
                        None,
                    )));
                }
                b'?' => {
                    self.advance();
                    if self.cur() == Some(b'.') {
                        self.advance();
                        return Ok(Some(self.token(
                            Tok::Nav,
                            start,
                            self.pos,
                            Vec::new(),
                            None,
                        )));
                    }
                    if self.cur() == Some(b'?') {
                        self.advance();
                        return Ok(Some(self.token(
                            Tok::Coal,
                            start,
                            self.pos,
                            Vec::new(),
                            None,
                        )));
                    }
                    return Ok(Some(self.token(
                        Tok::Char(b'?'),
                        start,
                        self.pos,
                        Vec::new(),
                        None,
                    )));
                }
                b'&' => {
                    self.advance();
                    if self.cur() == Some(b'&') {
                        self.advance();
                        return Ok(Some(self.token(
                            Tok::AndAnd,
                            start,
                            self.pos,
                            Vec::new(),
                            None,
                        )));
                    }
                    return Ok(Some(self.token(
                        Tok::Char(b'&'),
                        start,
                        self.pos,
                        Vec::new(),
                        None,
                    )));
                }
                b'|' => {
                    self.advance();
                    if self.cur() == Some(b'|') {
                        self.advance();
                        return Ok(Some(self.token(
                            Tok::OrOr,
                            start,
                            self.pos,
                            Vec::new(),
                            None,
                        )));
                    }
                    return Ok(Some(self.token(
                        Tok::Char(b'|'),
                        start,
                        self.pos,
                        Vec::new(),
                        None,
                    )));
                }
                b'"' | b'\'' => {
                    let delim = c;
                    self.advance();
                    let bytes = self.short_string(delim, start)?;
                    return Ok(Some(self.token(Tok::String, start, self.pos, bytes, None)));
                }
                b'.' => {
                    self.advance();
                    if self.cur() == Some(b'.') {
                        self.advance();
                        if self.cur() == Some(b'.') {
                            self.advance();
                            return Ok(Some(self.token(
                                Tok::Dots,
                                start,
                                self.pos,
                                Vec::new(),
                                None,
                            )));
                        }
                        return Ok(Some(self.token(
                            Tok::Concat,
                            start,
                            self.pos,
                            Vec::new(),
                            None,
                        )));
                    }
                    if self.cur().is_some_and(|ch| ch.is_ascii_digit()) {
                        return Ok(Some(self.number(start, true)?));
                    }
                    return Ok(Some(self.token(
                        Tok::Char(b'.'),
                        start,
                        self.pos,
                        Vec::new(),
                        None,
                    )));
                }
                _ => {
                    self.advance();
                    return Ok(Some(self.token(
                        Tok::Char(c),
                        start,
                        self.pos,
                        Vec::new(),
                        None,
                    )));
                }
            }
        }
    }

    fn name_or_keyword(&mut self) -> Token {
        let start = self.pos;
        let mut text = Vec::new();
        while let Some(c) = self.cur() {
            if !is_ident(c) {
                break;
            }
            text.push(c);
            self.advance();
        }
        let kind = keyword(&text).unwrap_or(Tok::Name);
        self.token(kind, start, self.pos, text, None)
    }

    fn number(&mut self, start: usize, started_with_dot: bool) -> Result<Token, SyntaxError> {
        let mut raw = Vec::new();
        if started_with_dot {
            raw.push(b'.');
        }
        let mut xp = b'e';
        let mut last = if started_with_dot {
            b'.'
        } else {
            self.cur().unwrap_or(b'0')
        };
        if !started_with_dot {
            let c = self.advance().unwrap_or(b'0');
            raw.push(c);
            last = c;
            if c == b'0' {
                while self.cur() == Some(b'_') {
                    self.advance();
                }
                if self.cur().is_some_and(|ch| ch | 0x20 == b'x') {
                    xp = b'p';
                }
            }
        }
        while let Some(c) = self.cur() {
            let exp_sign = (c == b'-' || c == b'+') && (last | 0x20) == xp;
            if is_ident(c) || c == b'.' || exp_sign {
                if c != b'_' {
                    raw.push(c);
                    last = c;
                }
                self.advance();
            } else {
                break;
            }
        }
        let kind = classify_number(&raw).map_err(|_| self.err(start, "malformed number"))?;
        let text = String::from_utf8(raw).unwrap_or_default().into_bytes();
        Ok(self.token(Tok::Number, start, self.pos, text, Some(kind)))
    }

    fn skip_eq(&mut self, open: u8) -> Result<i32, SyntaxError> {
        let start = self.pos;
        if self.cur() != Some(open) {
            return Err(self.err(start, "invalid long bracket"));
        }
        self.advance();
        let mut count = 0i32;
        while self.cur() == Some(b'=') && count < 0x2000_0000 {
            count += 1;
            self.advance();
        }
        if self.cur() == Some(open) {
            Ok(count)
        } else {
            Ok(-count - 1)
        }
    }

    fn long_string(&mut self, sep: i32, keep: bool) -> Result<Vec<u8>, SyntaxError> {
        let start = self.pos;
        self.advance();
        if self.cur().is_some_and(Self::is_eol) {
            self.finish_newline();
        }
        let mut bytes = Vec::new();
        loop {
            let Some(c) = self.cur() else {
                return Err(self.err(
                    start,
                    if keep {
                        "unfinished long string"
                    } else {
                        "unfinished long comment"
                    },
                ));
            };
            if c == b']' {
                let saved = self.pos;
                let close = self.skip_eq(b']')?;
                if close == sep {
                    self.advance();
                    return Ok(bytes);
                }
                if keep {
                    bytes.extend_from_slice(&self.src[saved..self.pos]);
                }
                continue;
            }
            if Self::is_eol(c) {
                if keep {
                    bytes.push(b'\n');
                }
                self.finish_newline();
                continue;
            }
            if keep {
                bytes.push(c);
            }
            self.advance();
        }
    }

    fn short_string(&mut self, delim: u8, start: usize) -> Result<Vec<u8>, SyntaxError> {
        let mut bytes = Vec::new();
        loop {
            let Some(c) = self.cur() else {
                return Err(self.err(start, "unfinished string"));
            };
            if c == delim {
                self.advance();
                return Ok(bytes);
            }
            match c {
                b'\n' | b'\r' => return Err(self.err(start, "unfinished string")),
                b'\\' => {
                    self.advance();
                    let Some(esc) = self.cur() else {
                        return Err(self.err(start, "unfinished string"));
                    };
                    match esc {
                        b'a' => {
                            bytes.push(0x07);
                            self.advance();
                        }
                        b'b' => {
                            bytes.push(0x08);
                            self.advance();
                        }
                        b'f' => {
                            bytes.push(0x0c);
                            self.advance();
                        }
                        b'n' => {
                            bytes.push(b'\n');
                            self.advance();
                        }
                        b'r' => {
                            bytes.push(b'\r');
                            self.advance();
                        }
                        b't' => {
                            bytes.push(b'\t');
                            self.advance();
                        }
                        b'v' => {
                            bytes.push(0x0b);
                            self.advance();
                        }
                        b'x' => bytes.push(self.hex_escape(start)?),
                        b'u' => self.unicode_escape(start, &mut bytes)?,
                        b'z' => {
                            self.advance();
                            while let Some(ch) = self.cur() {
                                if !is_space(ch) {
                                    break;
                                }
                                if Self::is_eol(ch) {
                                    self.finish_newline();
                                } else {
                                    self.advance();
                                }
                            }
                        }
                        b'\n' | b'\r' => {
                            bytes.push(b'\n');
                            self.finish_newline();
                        }
                        b'\\' | b'"' | b'\'' => {
                            bytes.push(esc);
                            self.advance();
                        }
                        digit if digit.is_ascii_digit() => {
                            bytes.push(self.decimal_escape(start, digit)?);
                        }
                        _ => return Err(self.err(start, "invalid escape")),
                    }
                }
                _ => {
                    bytes.push(c);
                    self.advance();
                }
            }
        }
    }

    fn hex_escape(&mut self, start: usize) -> Result<u8, SyntaxError> {
        self.advance();
        let hi = self.hex_digit(start)?;
        self.advance();
        let lo = self.hex_digit(start)?;
        self.advance();
        Ok((hi << 4) | lo)
    }

    fn hex_digit(&self, start: usize) -> Result<u8, SyntaxError> {
        match self.cur() {
            Some(c) if c.is_ascii_digit() => Ok(c - b'0'),
            Some(c) if (b'a'..=b'f').contains(&c) => Ok(c - b'a' + 10),
            Some(c) if (b'A'..=b'F').contains(&c) => Ok(c - b'A' + 10),
            _ => Err(self.err(start, "invalid escape")),
        }
    }

    fn unicode_escape(&mut self, start: usize, bytes: &mut Vec<u8>) -> Result<(), SyntaxError> {
        self.advance();
        if self.cur() != Some(b'{') {
            return Err(self.err(start, "invalid escape"));
        }
        self.advance();
        let mut code: u32 = 0;
        let mut digits = 0u32;
        loop {
            let Some(c) = self.cur() else {
                return Err(self.err(start, "invalid escape"));
            };
            if c == b'}' {
                if digits == 0 {
                    return Err(self.err(start, "invalid escape"));
                }
                break;
            }
            let digit = self.hex_digit(start)?;
            code = (code << 4) | u32::from(digit);
            if code >= 0x110000 {
                return Err(self.err(start, "invalid escape"));
            }
            digits += 1;
            self.advance();
        }
        self.advance();
        if (0xd800..0xe000).contains(&code) {
            return Err(self.err(start, "invalid escape"));
        }
        if code < 0x80 {
            bytes.push(code as u8);
        } else if code < 0x800 {
            bytes.push(0xc0 | (code >> 6) as u8);
            bytes.push(0x80 | (code & 0x3f) as u8);
        } else if code < 0x10000 {
            bytes.push(0xe0 | (code >> 12) as u8);
            bytes.push(0x80 | ((code >> 6) & 0x3f) as u8);
            bytes.push(0x80 | (code & 0x3f) as u8);
        } else {
            bytes.push(0xf0 | (code >> 18) as u8);
            bytes.push(0x80 | ((code >> 12) & 0x3f) as u8);
            bytes.push(0x80 | ((code >> 6) & 0x3f) as u8);
            bytes.push(0x80 | (code & 0x3f) as u8);
        }
        Ok(())
    }

    fn decimal_escape(&mut self, start: usize, first: u8) -> Result<u8, SyntaxError> {
        let mut value = u32::from(first - b'0');
        self.advance();
        if self.cur().is_some_and(|c| c.is_ascii_digit()) {
            value = value * 10 + u32::from(self.cur().unwrap() - b'0');
            self.advance();
            if self.cur().is_some_and(|c| c.is_ascii_digit()) {
                value = value * 10 + u32::from(self.cur().unwrap() - b'0');
                if value > 255 {
                    return Err(self.err(start, "invalid escape"));
                }
                self.advance();
            }
        }
        Ok(value as u8)
    }
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 128
}

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

fn keyword(text: &[u8]) -> Option<Tok> {
    Some(match text {
        b"and" => Tok::And,
        b"break" => Tok::Break,
        b"do" => Tok::Do,
        b"else" => Tok::Else,
        b"elseif" => Tok::Elseif,
        b"end" => Tok::End,
        b"false" => Tok::False,
        b"for" => Tok::For,
        b"function" => Tok::Function,
        b"goto" => Tok::Goto,
        b"if" => Tok::If,
        b"in" => Tok::In,
        b"local" => Tok::Local,
        b"nil" => Tok::Nil,
        b"not" => Tok::Not,
        b"or" => Tok::Or,
        b"repeat" => Tok::Repeat,
        b"return" => Tok::Return,
        b"then" => Tok::Then,
        b"true" => Tok::True,
        b"until" => Tok::Until,
        b"while" => Tok::While,
        b"const" => Tok::Const,
        b"continue" => Tok::Continue,
        _ => return None,
    })
}

fn classify_number(raw: &[u8]) -> Result<NumberKind, ()> {
    if raw.is_empty() {
        return Err(());
    }
    let mut p = 0usize;
    let mut base = 10u32;
    if raw[0] == b'0' && raw.len() > 1 {
        let mark = raw[1] | 0x20;
        if mark == b'x' {
            base = 16;
            p = 2;
        } else if mark == b'b' {
            base = 2;
            p = 2;
        }
    }
    let mut hasdig = false;
    let mut dot: Option<usize> = None;
    if p < raw.len() && raw[p] <= b'0' {
        loop {
            if p >= raw.len() {
                break;
            }
            if raw[p] == b'0' {
                hasdig = true;
                p += 1;
            } else if raw[p] == b'.' {
                if dot.is_some() {
                    return Err(());
                }
                dot = Some(p);
                p += 1;
            } else {
                break;
            }
        }
    }
    let mut digits = 0u32;
    let mut decimal_sig = [0u8; 20];
    let mut decimal_len = 0usize;
    let mut decimal_too_long = false;
    while p < raw.len() {
        if is_base_digit(raw[p], base) {
            if base == 2 && raw[p] > b'1' {
                return Err(());
            }
            if base == 10 && dot.is_none() {
                if decimal_len < decimal_sig.len() {
                    decimal_sig[decimal_len] = raw[p];
                    decimal_len += 1;
                } else {
                    decimal_too_long = true;
                }
            }
            digits += 1;
            p += 1;
        } else if raw[p] == b'.' {
            if dot.is_some() {
                return Err(());
            }
            dot = Some(p);
            p += 1;
        } else {
            break;
        }
    }
    if !hasdig && digits == 0 {
        return Err(());
    }
    let mut float_fmt = false;
    if let Some(dot_at) = dot {
        if base == 2 {
            return Err(());
        }
        float_fmt = true;
        if digits > 0 {
            let mut ex = dot_at as i32 - (p as i32 - 1);
            let mut scan = p;
            while ex < 0 && scan > 0 && raw[scan - 1] == b'0' {
                ex += 1;
                scan -= 1;
            }
            if ex <= -(1 << 20) {
                return Err(());
            }
        }
    }
    let exp_letter = if base == 16 { b'p' } else { b'e' };
    if base >= 10 && p < raw.len() && (raw[p] | 0x20) == exp_letter {
        float_fmt = true;
        p += 1;
        if p < raw.len() && (raw[p] == b'+' || raw[p] == b'-') {
            p += 1;
        }
        if p >= raw.len() || !raw[p].is_ascii_digit() {
            return Err(());
        }
        let mut exp = u32::from(raw[p] - b'0');
        p += 1;
        while p < raw.len() && raw[p].is_ascii_digit() {
            exp = exp
                .saturating_mul(10)
                .saturating_add(u32::from(raw[p] - b'0'));
            if exp >= (1 << 20) {
                return Err(());
            }
            p += 1;
        }
    }
    let mut kind = if base == 2 {
        NumberKind::Binary
    } else if base == 16 && float_fmt {
        NumberKind::HexFloat
    } else if base == 16 {
        NumberKind::HexInteger
    } else if float_fmt {
        NumberKind::Float
    } else {
        NumberKind::Integer
    };
    let mut saw_ll = false;
    if p < raw.len() {
        if (raw[p] | 0x20) == b'i' {
            p += 1;
            kind = NumberKind::Imaginary;
        } else if !float_fmt {
            let mut saw_u = false;
            if (raw[p] | 0x20) == b'u' {
                p += 1;
                saw_u = true;
            }
            if p < raw.len() && (raw[p] | 0x20) == b'l' {
                p += 1;
                if p < raw.len() && (raw[p] | 0x20) == b'l' {
                    p += 1;
                    saw_ll = true;
                } else {
                    return Err(());
                }
            }
            if p < raw.len() && (raw[p] | 0x20) == b'u' && (!saw_u || saw_ll) {
                if !saw_u || saw_ll {
                    p += 1;
                    saw_u = true;
                }
            }
            if saw_u && !saw_ll {
                return Err(());
            }
            if saw_ll && saw_u {
                kind = NumberKind::Unsigned64;
            } else if saw_ll {
                kind = NumberKind::Signed64;
            }
        }
        if p < raw.len() {
            return Err(());
        }
    }
    // strscan_bin rejects more than 64 significant bits for every format.
    // Leading zeros are already skipped, matching lj_strscan.c.
    if base == 2 && digits > 64 {
        return Err(());
    }
    // LL/ULL integers share the unsigned 64-bit scan limit. A leading minus
    // is a separate unary operator, so it does not widen this token.
    if saw_ll
        && !float_fmt
        && base == 10
        && (decimal_too_long || decimal_above_u64(&decimal_sig[..decimal_len]))
    {
        return Err(());
    }
    if saw_ll && !float_fmt && base == 16 && digits > 16 {
        return Err(());
    }
    Ok(kind)
}

fn decimal_above_u64(digits: &[u8]) -> bool {
    digits.len() == 20 && digits > b"18446744073709551615"
}

fn is_base_digit(c: u8, base: u32) -> bool {
    match base {
        2 | 10 => c.is_ascii_digit(),
        16 => c.is_ascii_hexdigit(),
        _ => false,
    }
}
