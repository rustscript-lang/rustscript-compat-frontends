//! Recursive-descent parser for the locked LuaJIT 2.1 Lua 5.2-compat grammar.
//!
//! This is the private syntax front. It does not lower or execute. Soft
//! keywords, safe navigation, compound assignment and short functions follow
//! the locked parser, including the statement/expression split for `~=`.

use super::ast::*;
use super::lexer::{self, Tok, Token};

#[derive(Clone, Copy)]
struct Flags {
    nores: bool,
    nocolon: bool,
    nonav: bool,
}

impl Flags {
    fn expr() -> Self {
        Self {
            nores: false,
            nocolon: false,
            nonav: false,
        }
    }

    fn stmt() -> Self {
        Self {
            nores: true,
            nocolon: false,
            nonav: false,
        }
    }

    fn nonav(self) -> Self {
        Self {
            nonav: true,
            ..self
        }
    }
}

struct Parser<'a> {
    source: &'a [u8],
    tokens: Vec<Token>,
    index: usize,
    last_statement: bool,
    level: u32,
}

const LJ_MAX_XLEVEL: u32 = 200;

pub fn parse_chunk(source: &[u8]) -> Result<Chunk, lexer::SyntaxError> {
    let lexed = lexer::lex(source)?;
    let mut parser = Parser {
        source,
        tokens: lexed.tokens,
        index: 0,
        last_statement: false,
        level: 0,
    };
    let start = parser.here();
    let body = parser.block()?;
    parser.expect_kind(Tok::Eof, "end of file")?;
    Ok(Chunk {
        span: start.cover(parser.previous_span()),
        has_bom: lexed.has_bom,
        body,
    })
}

impl<'a> Parser<'a> {
    fn here(&self) -> Span {
        self.tokens[self.index].span
    }

    fn line(&self) -> u32 {
        self.tokens[self.index].line
    }

    fn kind(&self) -> Tok {
        self.tokens[self.index].kind
    }

    fn previous_span(&self) -> Span {
        if self.index == 0 {
            self.here()
        } else {
            self.tokens[self.index - 1].span
        }
    }

    fn bump(&mut self) -> Token {
        let token = self.tokens[self.index].clone();
        if self.index + 1 < self.tokens.len() {
            self.index += 1;
        }
        token
    }

    fn eat(&mut self, kind: Tok) -> bool {
        if self.kind() == kind {
            self.bump();
            true
        } else {
            false
        }
    }

    fn synlevel_begin(&mut self) -> Result<(), lexer::SyntaxError> {
        self.level += 1;
        if self.level >= LJ_MAX_XLEVEL {
            return Err(self.error("chunk has too many syntax levels"));
        }
        Ok(())
    }

    fn synlevel_end(&mut self) {
        self.level = self.level.saturating_sub(1);
    }

    fn error(&self, message: impl Into<String>) -> lexer::SyntaxError {
        lexer::SyntaxError::at(self.here(), self.line(), message)
    }

    fn expect_kind(&mut self, kind: Tok, what: &str) -> Result<Token, lexer::SyntaxError> {
        if self.kind() == kind {
            Ok(self.bump())
        } else {
            Err(self.error(format!("expected {what}")))
        }
    }

    fn expect_char(&mut self, ch: u8) -> Result<Token, lexer::SyntaxError> {
        self.expect_kind(Tok::Char(ch), &format!("'{}'", ch as char))
    }

    fn is_end(&self) -> bool {
        matches!(
            self.kind(),
            Tok::Else | Tok::Elseif | Tok::End | Tok::Until | Tok::Eof
        )
    }

    fn is_name_kind(&self) -> bool {
        matches!(self.kind(), Tok::Name | Tok::Const | Tok::Continue)
    }

    fn name(&mut self) -> Result<Name, lexer::SyntaxError> {
        if !self.is_name_kind() {
            return Err(self.error("expected name"));
        }
        let token = self.bump();
        Ok(Name {
            span: token.span,
            bytes: token.text,
        })
    }

    fn block(&mut self) -> Result<Block, lexer::SyntaxError> {
        self.synlevel_begin()?;
        let block = self.block_body();
        if block.is_ok() {
            self.synlevel_end();
        }
        block
    }

    fn block_body(&mut self) -> Result<Block, lexer::SyntaxError> {
        let start = self.here();
        let saved = self.last_statement;
        let mut stmts = Vec::new();
        let mut is_last = false;
        // LuaJIT parses a trailing label by recursing into parse_label, and
        // each recursion takes a syntax level. The first label does not.
        // Semicolons stay inside that chain; another statement ends it.
        let mut label_chain = 0u32;
        while !is_last && !self.is_end() {
            if self.kind() == Tok::Label {
                if label_chain > 0 {
                    self.synlevel_begin()?;
                }
                label_chain += 1;
            } else if label_chain > 1 {
                for _ in 1..label_chain {
                    self.synlevel_end();
                }
                label_chain = 0;
            } else {
                label_chain = 0;
            }
            stmts.push(self.statement()?);
            is_last = self.last_statement;
            while self.eat(Tok::Char(b';')) {}
        }
        if label_chain > 1 {
            for _ in 1..label_chain {
                self.synlevel_end();
            }
        }
        self.last_statement = saved;
        let end = if stmts.is_empty() {
            start
        } else {
            stmts.last().map(|stmt| stmt_span(stmt)).unwrap_or(start)
        };
        Ok(Block {
            span: start.cover(end),
            stmts,
        })
    }

    fn statement(&mut self) -> Result<Stmt, lexer::SyntaxError> {
        self.last_statement = false;
        let start = self.here();
        match self.kind() {
            Tok::If => self.if_stmt(start),
            Tok::While => self.while_stmt(start),
            Tok::Do => {
                self.bump();
                let body = self.block()?;
                self.expect_kind(Tok::End, "end")?;
                Ok(Stmt::Do {
                    span: start.cover(self.previous_span()),
                    body,
                })
            }
            Tok::For => self.for_stmt(start),
            Tok::Repeat => self.repeat_stmt(start),
            Tok::Function => self.function_stmt(start),
            Tok::Local => self.local_stmt(start, false),
            Tok::Const => {
                let next = self.peek_kind();
                if matches!(next, Some(Tok::Function)) || next.is_some_and(is_name_token) {
                    self.local_stmt(start, true)
                } else {
                    self.call_or_assign(start)
                }
            }
            Tok::Return => {
                self.bump();
                let values = if self.is_end() || self.kind() == Tok::Char(b';') {
                    ExprList::empty(self.here())
                } else {
                    self.expr_list()?
                };
                self.last_statement = true;
                Ok(Stmt::Return {
                    span: start.cover(if values.exprs.is_empty() {
                        self.previous_span()
                    } else {
                        values.span
                    }),
                    values,
                })
            }
            Tok::Break => {
                self.bump();
                Ok(Stmt::Break {
                    span: start.cover(self.previous_span()),
                })
            }
            Tok::Continue => {
                let next = self.peek_kind();
                if next.is_some_and(|kind| is_block_end(kind) || kind == Tok::Char(b';')) {
                    self.bump();
                    self.last_statement = true;
                    Ok(Stmt::Continue {
                        span: start.cover(self.previous_span()),
                    })
                } else {
                    self.call_or_assign(start)
                }
            }
            Tok::Char(b';') => {
                self.bump();
                Ok(Stmt::Empty {
                    span: start.cover(self.previous_span()),
                })
            }
            Tok::Label => self.label_stmt(start),
            Tok::Goto => {
                self.bump();
                let name = self.name()?;
                Ok(Stmt::Goto {
                    span: start.cover(name.span),
                    name,
                })
            }
            _ => self.call_or_assign(start),
        }
    }

    fn if_stmt(&mut self, start: Span) -> Result<Stmt, lexer::SyntaxError> {
        let mut arms = Vec::new();
        self.bump();
        loop {
            let cond = self.expr(Flags::expr())?;
            self.expect_kind(Tok::Then, "then")?;
            let body = self.block()?;
            arms.push(IfArm { cond, body });
            if self.kind() == Tok::Elseif {
                self.bump();
                continue;
            }
            break;
        }
        let else_body = if self.eat(Tok::Else) {
            Some(self.block()?)
        } else {
            None
        };
        self.expect_kind(Tok::End, "end")?;
        Ok(Stmt::If {
            span: start.cover(self.previous_span()),
            arms,
            else_body,
        })
    }

    fn while_stmt(&mut self, start: Span) -> Result<Stmt, lexer::SyntaxError> {
        self.bump();
        let cond = self.expr(Flags::expr())?;
        self.expect_kind(Tok::Do, "do")?;
        let body = self.block()?;
        self.expect_kind(Tok::End, "end")?;
        Ok(Stmt::While {
            span: start.cover(self.previous_span()),
            cond,
            body,
        })
    }

    fn repeat_stmt(&mut self, start: Span) -> Result<Stmt, lexer::SyntaxError> {
        self.bump();
        let body = self.block()?;
        self.expect_kind(Tok::Until, "until")?;
        let until = self.expr(Flags::expr())?;
        Ok(Stmt::Repeat {
            span: start.cover(until.span()),
            body,
            until,
        })
    }

    fn for_stmt(&mut self, start: Span) -> Result<Stmt, lexer::SyntaxError> {
        self.bump();
        let first = self.name()?;
        if self.eat(Tok::Char(b'=')) {
            let start_expr = self.expr(Flags::expr())?;
            self.expect_char(b',')?;
            let limit = self.expr(Flags::expr())?;
            let step = if self.eat(Tok::Char(b',')) {
                Some(self.expr(Flags::expr())?)
            } else {
                None
            };
            self.expect_kind(Tok::Do, "do")?;
            let body = self.block()?;
            self.expect_kind(Tok::End, "end")?;
            return Ok(Stmt::NumericFor {
                span: start.cover(self.previous_span()),
                name: first,
                start: start_expr,
                limit,
                step,
                body,
            });
        }
        let mut names = vec![first];
        while self.eat(Tok::Char(b',')) {
            names.push(self.name()?);
        }
        self.expect_kind(Tok::In, "in")?;
        let iters = self.expr_list()?;
        self.expect_kind(Tok::Do, "do")?;
        let body = self.block()?;
        self.expect_kind(Tok::End, "end")?;
        Ok(Stmt::GenericFor {
            span: start.cover(self.previous_span()),
            names,
            iters,
            body,
        })
    }

    fn function_stmt(&mut self, start: Span) -> Result<Stmt, lexer::SyntaxError> {
        self.bump();
        let mut segments = vec![self.name()?];
        while self.eat(Tok::Char(b'.')) {
            segments.push(self.name()?);
        }
        let method = if self.eat(Tok::Char(b':')) {
            Some(self.name()?)
        } else {
            None
        };
        let name_span = method
            .as_ref()
            .map(|name| name.span)
            .unwrap_or_else(|| segments.last().unwrap().span);
        let body = self.func_body(method.is_some())?;
        Ok(Stmt::Function {
            span: start.cover(body.span),
            name: FuncName {
                span: segments[0].span.cover(name_span),
                segments,
                method,
            },
            body,
        })
    }

    fn local_stmt(&mut self, start: Span, const_binding: bool) -> Result<Stmt, lexer::SyntaxError> {
        self.bump();
        if self.eat(Tok::Function) {
            let name = self.name()?;
            let func = self.func_body(false)?;
            return Ok(Stmt::LocalFunction {
                span: start.cover(func.span),
                name,
                const_binding,
                func,
            });
        }
        let mut names = vec![LocalName {
            name: self.name()?,
            const_binding,
        }];
        while self.eat(Tok::Char(b',')) {
            names.push(LocalName {
                name: self.name()?,
                const_binding,
            });
        }
        let values = if self.eat(Tok::Char(b'=')) {
            self.expr_list()?
        } else {
            ExprList::empty(self.previous_span())
        };
        let end = if values.exprs.is_empty() {
            names.last().unwrap().name.span
        } else {
            values.span
        };
        Ok(Stmt::Local {
            span: start.cover(end),
            names,
            values,
        })
    }

    fn label_stmt(&mut self, start: Span) -> Result<Stmt, lexer::SyntaxError> {
        self.bump();
        let name = self.name()?;
        self.expect_kind(Tok::Label, "'::'")?;
        Ok(Stmt::Label {
            span: start.cover(self.previous_span()),
            name,
        })
    }

    fn call_or_assign(&mut self, start: Span) -> Result<Stmt, lexer::SyntaxError> {
        let primary = self.primary(Flags::stmt())?;
        if is_call(&primary.expr) {
            return Ok(Stmt::Call {
                span: primary.expr.span(),
                call: primary.expr,
            });
        }
        if primary.nav_used && self.kind() == Tok::Char(b',') {
            return Err(self.error("safe navigation cannot be used in parallel assignment"));
        }
        let target = assign_target(primary.expr).map_err(|span| {
            lexer::SyntaxError::at(span, self.line(), "invalid assignment target")
        })?;
        if let Some(op) = self.compound_op() {
            let value = self.expr(Flags::expr())?;
            return Ok(Stmt::CompoundAssign {
                span: start.cover(value.span()),
                target,
                op,
                value,
            });
        }
        let mut targets = vec![target];
        let mut nvars = 1u32;
        while self.eat(Tok::Char(b',')) {
            if self.level + nvars >= LJ_MAX_XLEVEL {
                return Err(self.error("too many variable names"));
            }
            nvars += 1;
            let next = self.primary(Flags::stmt().nonav())?;
            if is_call(&next.expr) {
                return Err(self.error("invalid assignment target"));
            }
            targets.push(assign_target(next.expr).map_err(|span| {
                lexer::SyntaxError::at(span, self.line(), "invalid assignment target")
            })?);
        }
        self.expect_char(b'=')?;
        let values = self.expr_list()?;
        Ok(Stmt::Assign {
            span: start.cover(values.span),
            targets,
            values,
        })
    }

    fn compound_op(&mut self) -> Option<BinOp> {
        let op = match self.kind() {
            Tok::Char(b'+') => BinOp::Add,
            Tok::Char(b'-') => BinOp::Sub,
            Tok::Char(b'*') => BinOp::Mul,
            Tok::Char(b'/') => BinOp::Div,
            Tok::Char(b'%') => BinOp::Mod,
            Tok::Char(b'&') => BinOp::Band,
            Tok::Char(b'|') => BinOp::Bor,
            Tok::Char(b'~') => BinOp::BXor,
            Tok::Shl => BinOp::Shl,
            Tok::Shr => BinOp::Shr,
            Tok::Sar => BinOp::Sar,
            Tok::Concat => BinOp::Concat,
            Tok::Ne => BinOp::BXor,
            _ => return None,
        };
        if self.kind() == Tok::Ne {
            self.bump();
            return Some(op);
        }
        let end = self.here().end as usize;
        if self.source.get(end) != Some(&b'=') {
            return None;
        }
        self.bump();
        if self.kind() == Tok::Char(b'=') {
            self.bump();
        }
        Some(op)
    }

    fn func_body(&mut self, need_self: bool) -> Result<FuncBody, lexer::SyntaxError> {
        let start = self.here();
        self.expect_char(b'(')?;
        let mut params = Vec::new();
        let mut self_param = false;
        if need_self {
            self_param = true;
        }
        let mut vararg = false;
        if self.kind() != Tok::Char(b')') {
            loop {
                if self.eat(Tok::Dots) {
                    vararg = true;
                    break;
                }
                params.push(self.name()?);
                if !self.eat(Tok::Char(b',')) {
                    break;
                }
                if self.kind() == Tok::Char(b')') {
                    return Err(self.error("expected name"));
                }
            }
        }
        self.expect_char(b')')?;
        let body = self.block()?;
        self.expect_kind(Tok::End, "end")?;
        Ok(FuncBody {
            span: start.cover(self.previous_span()),
            params,
            vararg,
            self_param,
            body,
        })
    }

    fn short_func(&mut self, name: Option<Name>, flags: Flags) -> Result<Expr, lexer::SyntaxError> {
        let start = name
            .as_ref()
            .map(|item| item.span)
            .unwrap_or_else(|| self.here());
        let mut params = Vec::new();
        let mut vararg = false;
        if let Some(name) = name {
            params.push(name);
        } else if !self.eat(Tok::OrOr) {
            self.expect_char(b'|')?;
            if self.kind() != Tok::Char(b'|') {
                loop {
                    if self.eat(Tok::Dots) {
                        vararg = true;
                        break;
                    }
                    params.push(self.name()?);
                    if !self.eat(Tok::Char(b',')) {
                        break;
                    }
                }
            }
            self.expect_char(b'|')?;
        }
        self.expect_kind(Tok::Arrow, "'->'")?;
        let body = if self.eat(Tok::Do) {
            let block = self.block()?;
            self.expect_kind(Tok::End, "end")?;
            ShortBody::Block(block)
        } else {
            ShortBody::Expr(Box::new(self.expr(flags)?))
        };
        let end = match &body {
            ShortBody::Expr(expr) => expr.span(),
            ShortBody::Block(block) => block.span.cover(self.previous_span()),
        };
        Ok(Expr::ShortFunction {
            span: start.cover(end),
            params,
            vararg,
            body: Box::new(body),
        })
    }

    fn expr_list(&mut self) -> Result<ExprList, lexer::SyntaxError> {
        let first = self.expr(Flags::expr())?;
        let start = first.span();
        let mut exprs = vec![first];
        while self.eat(Tok::Char(b',')) {
            exprs.push(self.expr(Flags::expr())?);
        }
        let end = exprs.last().unwrap().span();
        Ok(ExprList {
            span: start.cover(end),
            exprs,
        })
    }

    fn expr(&mut self, flags: Flags) -> Result<Expr, lexer::SyntaxError> {
        let mut left = self.binop(flags, 0)?;
        if self.eat(Tok::Char(b'?')) {
            self.synlevel_begin()?;
            let then_expr = self.expr(Flags {
                nores: false,
                nocolon: true,
                nonav: false,
            })?;
            self.expect_char(b':')?;
            let else_expr = self.expr(Flags::expr())?;
            self.synlevel_end();
            let span = left.span().cover(else_expr.span());
            left = Expr::Ternary {
                span,
                cond: Box::new(left),
                then_expr: Box::new(then_expr),
                else_expr: Box::new(else_expr),
            };
        }
        Ok(left)
    }

    fn binop(&mut self, flags: Flags, limit: u8) -> Result<Expr, lexer::SyntaxError> {
        self.synlevel_begin()?;
        let expr = self.binop_body(flags, limit);
        if expr.is_ok() {
            self.synlevel_end();
        }
        expr
    }

    fn binop_body(&mut self, flags: Flags, limit: u8) -> Result<Expr, lexer::SyntaxError> {
        let mut base = self.unop(flags)?;
        while let Some((op, left_pri, right_pri)) = binop_of(self.kind()) {
            if left_pri <= limit {
                break;
            }
            if right_pri < left_pri {
                // Right-associative `^` and `..` spend a syntax level. Nest
                // them; do not append them to a left spine.
                self.bump();
                let right = self.binop(flags, right_pri)?;
                let span = base.span().cover(right.span());
                base = Expr::Binary {
                    span,
                    op,
                    left: Box::new(base),
                    right: Box::new(right),
                };
                continue;
            }
            let chain_pri = left_pri;
            let mut steps = Vec::new();
            while let Some((op, left_pri, right_pri)) = binop_of(self.kind()) {
                if left_pri != chain_pri || left_pri <= limit {
                    break;
                }
                self.bump();
                let right = self.binop(flags, right_pri)?;
                steps.push(BinStep { op, expr: right });
            }
            base = finish_binary_chain(base, steps);
        }
        Ok(base)
    }

    fn unop(&mut self, flags: Flags) -> Result<Expr, lexer::SyntaxError> {
        let op = match self.kind() {
            Tok::Not => UnOp::Not,
            Tok::Char(b'!') => UnOp::Bang,
            Tok::Char(b'-') => UnOp::Neg,
            Tok::Char(b'#') => UnOp::Len,
            Tok::Char(b'~') => UnOp::BNot,
            _ => return self.simple(flags),
        };
        let start = self.here();
        self.bump();
        let expr = self.binop(flags, 12)?;
        Ok(Expr::Unary {
            span: start.cover(expr.span()),
            op,
            expr: Box::new(expr),
        })
    }

    fn simple(&mut self, flags: Flags) -> Result<Expr, lexer::SyntaxError> {
        let start = self.here();
        match self.kind() {
            Tok::Number => {
                let token = self.bump();
                Ok(Expr::Number {
                    span: token.span,
                    raw: String::from_utf8_lossy(&token.text).into_owned(),
                    kind: token.number_kind.unwrap_or(NumberKind::Float),
                })
            }
            Tok::String => {
                let token = self.bump();
                Ok(Expr::String {
                    span: token.span,
                    bytes: token.text,
                })
            }
            Tok::Nil => {
                self.bump();
                Ok(Expr::Nil {
                    span: start.cover(self.previous_span()),
                })
            }
            Tok::True => {
                self.bump();
                Ok(Expr::Bool {
                    span: start.cover(self.previous_span()),
                    value: true,
                })
            }
            Tok::False => {
                self.bump();
                Ok(Expr::Bool {
                    span: start.cover(self.previous_span()),
                    value: false,
                })
            }
            Tok::Dots => {
                self.bump();
                Ok(Expr::Vararg {
                    span: start.cover(self.previous_span()),
                })
            }
            Tok::Char(b'{') => self.table(),
            Tok::Function => {
                self.bump();
                let body = self.func_body(false)?;
                Ok(Expr::Function {
                    span: start.cover(body.span),
                    body,
                })
            }
            Tok::Char(b'|') | Tok::OrOr => self.short_func(None, flags),
            _ => Ok(self.primary(flags)?.expr),
        }
    }

    fn primary(&mut self, flags: Flags) -> Result<Primary, lexer::SyntaxError> {
        let start = self.here();
        let expr = if self.eat(Tok::Char(b'(')) {
            let inner = self.expr(Flags::expr())?;
            self.expect_char(b')')?;
            Expr::Paren {
                span: start.cover(self.previous_span()),
                inner: Box::new(inner),
            }
        } else if self.is_name_kind() {
            let name = self.name()?;
            if !flags.nores && self.kind() == Tok::Arrow {
                return Ok(Primary {
                    expr: self.short_func(Some(name), flags)?,
                    nav_used: false,
                });
            }
            Expr::Name {
                span: name.span,
                name,
            }
        } else {
            return Err(self.error("expected expression"));
        };
        let base_span = expr.span();
        let mut steps = Vec::new();
        let mut nav_used = false;
        loop {
            let mut nav = false;
            if !flags.nonav && self.eat(Tok::Nav) {
                nav = true;
                nav_used = true;
            }
            if self.kind() == Tok::Char(b'[') {
                let key = self.bracket()?;
                steps.push(Suffix::Index {
                    span: base_span.cover(self.previous_span()),
                    key,
                    nav,
                });
            } else if self.kind() == Tok::Char(b':') {
                if flags.nocolon {
                    if nav {
                        return Err(self.error("expected expression"));
                    }
                    break;
                }
                self.bump();
                let method = self.name()?;
                let mut method_nav = false;
                if !flags.nonav && self.eat(Tok::Nav) {
                    method_nav = true;
                    nav_used = true;
                }
                let args = self.args()?;
                steps.push(Suffix::Method {
                    span: base_span.cover(args_span(&args)),
                    method,
                    args,
                    receiver_nav: nav,
                    method_nav,
                });
            } else if is_args_start(self.kind()) {
                let args = self.args()?;
                steps.push(Suffix::Call {
                    span: base_span.cover(args_span(&args)),
                    args,
                    nav,
                });
            } else if nav || self.kind() == Tok::Char(b'.') {
                if !nav {
                    self.bump();
                }
                let field = self.name()?;
                steps.push(Suffix::Field {
                    span: base_span.cover(field.span),
                    name: field,
                    nav,
                });
            } else {
                break;
            }
        }
        Ok(Primary {
            expr: finish_suffix_chain(expr, steps),
            nav_used,
        })
    }

    fn bracket(&mut self) -> Result<Expr, lexer::SyntaxError> {
        self.expect_char(b'[')?;
        let expr = self.expr(Flags::expr())?;
        self.expect_char(b']')?;
        Ok(expr)
    }

    fn args(&mut self) -> Result<Args, lexer::SyntaxError> {
        match self.kind() {
            Tok::Char(b'(') => {
                self.bump();
                let list = if self.kind() == Tok::Char(b')') {
                    ExprList::empty(self.here())
                } else {
                    self.expr_list()?
                };
                self.expect_char(b')')?;
                Ok(Args::List(list))
            }
            Tok::String => {
                let token = self.bump();
                Ok(Args::String {
                    span: token.span,
                    bytes: token.text,
                })
            }
            Tok::Char(b'{') => {
                let table = self.table()?;
                let Expr::Table { span, fields } = table else {
                    unreachable!("table constructor");
                };
                Ok(Args::Table { span, fields })
            }
            _ => Err(self.error("expected function arguments")),
        }
    }

    fn table(&mut self) -> Result<Expr, lexer::SyntaxError> {
        let start = self.here();
        self.expect_char(b'{')?;
        let mut fields = Vec::new();
        while self.kind() != Tok::Char(b'}') && self.kind() != Tok::Eof {
            if self.kind() == Tok::Char(b'[') {
                let key = self.bracket()?;
                self.expect_char(b'=')?;
                let value = self.expr(Flags::expr())?;
                let span = key.span().cover(value.span());
                fields.push(Field::Index { span, key, value });
            } else if self.is_name_kind() && self.peek_kind() == Some(Tok::Char(b'=')) {
                let name = self.name()?;
                self.bump();
                let value = self.expr(Flags::expr())?;
                let span = name.span.cover(value.span());
                fields.push(Field::Name { span, name, value });
            } else {
                let value = self.expr(Flags::expr())?;
                let span = value.span();
                fields.push(Field::Item { span, value });
            }
            if !self.eat(Tok::Char(b',')) && !self.eat(Tok::Char(b';')) {
                break;
            }
        }
        self.expect_char(b'}')?;
        Ok(Expr::Table {
            span: start.cover(self.previous_span()),
            fields,
        })
    }

    fn peek_kind(&self) -> Option<Tok> {
        self.tokens.get(self.index + 1).map(|token| token.kind)
    }
}

struct Primary {
    expr: Expr,
    nav_used: bool,
}

fn is_name_token(kind: Tok) -> bool {
    matches!(kind, Tok::Name | Tok::Const | Tok::Continue)
}

fn is_block_end(kind: Tok) -> bool {
    matches!(
        kind,
        Tok::Else | Tok::Elseif | Tok::End | Tok::Until | Tok::Eof
    )
}

fn is_args_start(kind: Tok) -> bool {
    matches!(kind, Tok::Char(b'(') | Tok::String | Tok::Char(b'{'))
}

fn is_call(expr: &Expr) -> bool {
    match expr {
        Expr::SuffixChain { steps, .. } => {
            matches!(
                steps.last(),
                Some(Suffix::Call { .. } | Suffix::Method { .. })
            )
        }
        _ => false,
    }
}

fn assign_target(expr: Expr) -> Result<AssignTarget, Span> {
    match expr {
        Expr::Name { name, .. } => Ok(AssignTarget::Name(name)),
        Expr::SuffixChain {
            span,
            base,
            mut steps,
        } => {
            let last = steps.pop();
            let table = finish_suffix_chain(*base, steps);
            match last {
                Some(Suffix::Index { key, nav, .. }) => Ok(AssignTarget::Index {
                    span,
                    table: Box::new(table),
                    key: Box::new(key),
                    nav,
                }),
                Some(Suffix::Field { name, nav, .. }) => Ok(AssignTarget::Field {
                    span,
                    table: Box::new(table),
                    name,
                    nav,
                }),
                _ => Err(span),
            }
        }
        other => Err(other.span()),
    }
}

fn finish_binary_chain(base: Expr, steps: Vec<BinStep>) -> Expr {
    let Some(last) = steps.last() else {
        return base;
    };
    let span = base.span().cover(last.expr.span());
    Expr::BinaryChain {
        span,
        base: Box::new(base),
        steps,
    }
}

fn finish_suffix_chain(base: Expr, steps: Vec<Suffix>) -> Expr {
    let Some(last) = steps.last() else {
        return base;
    };
    let span = last.span();
    Expr::SuffixChain {
        span,
        base: Box::new(base),
        steps,
    }
}

fn args_span(args: &Args) -> Span {
    match args {
        Args::List(list) => list.span,
        Args::String { span, .. } | Args::Table { span, .. } => *span,
    }
}

fn binop_of(kind: Tok) -> Option<(BinOp, u8, u8)> {
    let op = match kind {
        Tok::Char(b'+') => BinOp::Add,
        Tok::Char(b'-') => BinOp::Sub,
        Tok::Char(b'*') => BinOp::Mul,
        Tok::Char(b'/') => BinOp::Div,
        Tok::Char(b'%') => BinOp::Mod,
        Tok::Char(b'^') => BinOp::Pow,
        Tok::Char(b'&') => BinOp::Band,
        Tok::Char(b'|') => BinOp::Bor,
        Tok::Char(b'~') => BinOp::BXor,
        Tok::Shl => BinOp::Shl,
        Tok::Shr => BinOp::Shr,
        Tok::Sar => BinOp::Sar,
        Tok::Concat => BinOp::Concat,
        Tok::Eq => BinOp::Eq,
        Tok::Ne | Tok::NeBang => BinOp::Ne,
        Tok::Char(b'<') => BinOp::Lt,
        Tok::Char(b'>') => BinOp::Gt,
        Tok::Le => BinOp::Le,
        Tok::Ge => BinOp::Ge,
        Tok::And | Tok::AndAnd => BinOp::And,
        Tok::Or | Tok::OrOr => BinOp::Or,
        Tok::Coal => BinOp::Coal,
        _ => return None,
    };
    let (left, right) = match op {
        BinOp::Add | BinOp::Sub => (10, 10),
        BinOp::Mul | BinOp::Div | BinOp::Mod => (11, 11),
        BinOp::Pow => (14, 13),
        BinOp::Band => (6, 6),
        BinOp::BXor => (5, 5),
        BinOp::Bor => (4, 4),
        BinOp::Shl | BinOp::Shr | BinOp::Sar => (7, 7),
        BinOp::Concat => (9, 8),
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => (3, 3),
        BinOp::And => (2, 2),
        BinOp::Or | BinOp::Coal => (1, 1),
    };
    Some((op, left, right))
}

fn stmt_span(stmt: &Stmt) -> Span {
    match stmt {
        Stmt::Empty { span }
        | Stmt::Do { span, .. }
        | Stmt::While { span, .. }
        | Stmt::Repeat { span, .. }
        | Stmt::If { span, .. }
        | Stmt::NumericFor { span, .. }
        | Stmt::GenericFor { span, .. }
        | Stmt::Local { span, .. }
        | Stmt::LocalFunction { span, .. }
        | Stmt::Function { span, .. }
        | Stmt::Assign { span, .. }
        | Stmt::CompoundAssign { span, .. }
        | Stmt::Call { span, .. }
        | Stmt::Label { span, .. }
        | Stmt::Goto { span, .. }
        | Stmt::Break { span }
        | Stmt::Continue { span }
        | Stmt::Return { span, .. } => *span,
    }
}
