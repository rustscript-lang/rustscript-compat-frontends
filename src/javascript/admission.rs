//! ES5.1 Script grammar admission and strict-mode early errors.
//!
//! The locked SWC parser accepts modern ECMAScript forms and silently admits
//! several ES5 strict-mode early errors, so the parse target alone is not an
//! ES5 whitelist. [`admit_script`] walks the whole parsed [`Script`][ast::Script]
//! recursively and enforces the explicit ES5.1 whitelist plus strictness
//! handling:
//!
//! * post-ES5 declarations and forms are refused with `unsupported syntax`
//!   diagnostics, even when nested inside otherwise ordinary code;
//! * ordinary ES5 forms (functions, `var`, loops, `for-in`, labels,
//!   `try`/`catch`/`finally`, `with` in sloppy code, data/getter/setter
//!   object properties, sparse arrays, ...) are admitted at this grammar
//!   stage even when P0 lowering cannot execute them yet;
//! * strict mode follows the actual source-spelling directive prologue and
//!   is inherited by nested function code; the physical
//!   [`FrontendTypingPolicy::Strict`][vm::FrontendTypingPolicy::Strict] never
//!   activates JavaScript strict mode;
//! * object-literal duplicate-property rules follow ES5.1, comparing
//!   canonical UTF-16 property names (equivalent identifier, string and
//!   numeric spellings collide, including escapes);
//! * legacy octal numeric and string forms are strict-mode early errors and
//!   sloppy-mode legacy admissions (Annex-B-style implementation extension,
//!   labeled as such in tests, never a normative conformance claim);
//! * only ES5 `g`/`i`/`m` regexp flags are admitted; lookahead and
//!   non-capturing groups stay admitted while named groups and lookbehind
//!   are refused; full regexp pattern conformance stays deferred
//!   to the later RegExp phase and is never inferred from the Regex AST.
//!
//! Grammar admission success is not runtime lowering success: admitted forms
//! may still be refused later by the P0 lowerer with its own diagnostics.
//! Matches stay exhaustive so a new locked AST variant breaks compilation
//! instead of slipping through silently.

use std::collections::HashMap;

use swc_common::{Span as SwcSpan, Spanned};
use swc_ecma_ast as ast;
use vm::ParseError;

use super::source_map::{parse_error, syntax_error, unsupported_error};
use super::string_literal::{
    StringCodecError, StringCodecErrorKind, StringStrictness, decode_string_literal, narrowed_span,
};

/// ES5 strict-only future reserved words (ES5.1 section 7.6.1.2). These are
/// ordinary identifiers in sloppy code (`var let` is legal) but cannot name
/// bindings in strict code.
const STRICT_RESERVED_WORDS: &[&str] = &[
    "implements",
    "interface",
    "let",
    "package",
    "private",
    "protected",
    "public",
    "static",
    "yield",
];

/// Object-literal property roles for the ES5.1 duplicate-property rules.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PropertyKind {
    Data,
    Getter,
    Setter,
}

/// Accumulated roles seen for one canonical property name. A getter+setter
/// pair accumulates both flags; any repeat beyond the pair still conflicts.
#[derive(Clone, Copy, Default)]
struct PropertyRoles {
    data: bool,
    getter: bool,
    setter: bool,
}

/// Admit a parsed ES5 Script: recursive whitelist plus strict early errors.
///
/// `source_id` is the caller's source id carried on every diagnostic and
/// `base` is the scratch source-file start used for byte spans. `source`
/// is the raw text the identifier raw-lexeme checks slice token spellings
/// from; no global text scan is ever performed.
pub(super) fn admit_script(
    script: &ast::Script,
    source: &str,
    source_id: u32,
    base: u32,
) -> Result<(), ParseError> {
    let mut checker = Checker {
        source,
        source_id,
        base,
        strict: false,
        function_depth: 0,
        loop_depth: 0,
        switch_depth: 0,
        labels: Vec::new(),
    };
    checker.strict = prologue_strict(&script.body);
    for stmt in &script.body {
        checker.admit_source_element(stmt)?;
    }
    Ok(())
}

/// Reusable strictness helper: does the statement list open with an exact
/// source-spelling `"use strict"` (or `'use strict'`) directive prologue?
///
/// Only exact raw spellings activate strict mode: escaped lookalikes such as
/// `"use\u0020strict"` have different raw text, and parenthesized
/// `("use strict")` is not an expression-statement string literal, so neither
/// counts. Later consumers reuse this for nested function bodies.
pub(super) fn prologue_strict(stmts: &[ast::Stmt]) -> bool {
    for stmt in stmts {
        match stmt {
            ast::Stmt::Expr(expr_stmt) => match &*expr_stmt.expr {
                ast::Expr::Lit(ast::Lit::Str(value)) => {
                    if is_use_strict_raw(value) {
                        return true;
                    }
                    // Any other string literal keeps the prologue open.
                }
                _ => return false,
            },
            _ => return false,
        }
    }
    false
}

/// Exact raw-spelling `"use strict"` check with a sound cooked fallback.
///
/// Parsed sources always carry `raw`; the cooked fallback only matters for
/// synthetic ASTs and errs toward strictness (never toward missing a strict
/// early error).
fn is_use_strict_raw(value: &ast::Str) -> bool {
    if let Some(raw) = &value.raw {
        return raw.as_str() == "\"use strict\"" || raw.as_str() == "'use strict'";
    }
    value.value.as_wtf8().as_str() == Some("use strict")
}

/// ES5 NumberToString (ES5.1 section 9.8.1) for numeric property names.
///
/// Rust float display alone is not the proof: `1e21` must canonicalize to
/// `1e+21` (so a `"1e21"` string key does not collide while `"1e+21"` does),
/// and large finite values use the ES5 digit/threshold rules, not Rust
/// formatting.
fn number_to_string(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value == 0.0 {
        return "0".to_owned();
    }
    if value.is_infinite() {
        return if value > 0.0 {
            "Infinity".to_owned()
        } else {
            "-Infinity".to_owned()
        };
    }
    if value < 0.0 {
        return format!("-{}", number_to_string(-value));
    }
    // Shortest round-trip digits from the platform formatter, decomposed
    // into ES5 (n, k, s): value = s * 10^(n-k) with minimal digit count.
    let printed = format!("{value}");
    let (mantissa, exponent) = match printed.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i32>().unwrap_or(0)),
        None => (printed.as_str(), 0),
    };
    let point = mantissa.find('.').unwrap_or(mantissa.len());
    let digits: Vec<u8> = mantissa.bytes().filter(|byte| *byte != b'.').collect();
    let leading = digits.iter().take_while(|byte| **byte == b'0').count();
    let mut significant = digits[leading..].to_vec();
    while significant.last() == Some(&b'0') {
        significant.pop();
    }
    debug_assert!(!significant.is_empty());
    let digits: String = significant.iter().map(|byte| *byte as char).collect();
    let k = digits.len() as i32;
    let n = point as i32 - leading as i32 + exponent;
    if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        let n = n as usize;
        format!("{}.{}", &digits[..n], &digits[n..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let mantissa = if k > 1 {
            format!("{}.{}", &digits[..1], &digits[1..])
        } else {
            digits[..1].to_owned()
        };
        let shift = n - 1;
        let sign = if shift < 0 { '-' } else { '+' };
        format!("{mantissa}e{sign}{}", shift.abs())
    }
}

struct Checker<'source> {
    /// Raw source text for actual identifier-token slicing. Only genuine
    /// token spans are inspected, never a global scan.
    source: &'source str,
    source_id: u32,
    base: u32,
    strict: bool,
    function_depth: u32,
    loop_depth: u32,
    switch_depth: u32,
    /// Enclosing labels: name plus whether the label labels an iteration
    /// statement (label sets accumulate through label chains, so
    /// `a: b: for(;;)` marks both `a` and `b` as iteration labels).
    labels: Vec<(String, bool)>,
}

impl<'source> Checker<'source> {
    fn syntax(&self, span: SwcSpan, message: impl Into<String>) -> ParseError {
        syntax_error(self.source_id, self.base, span, message)
    }

    fn unsupported(&self, span: SwcSpan, message: impl Into<String>) -> ParseError {
        unsupported_error(self.source_id, self.base, span, message)
    }

    /// Binding-occurrence check for declared/introduced names (variable,
    /// function, parameter and catch names). The raw-lexeme check runs in
    /// both modes, before the strictness gate below: `eval`/`arguments` and
    /// the strict reserved words are restricted only in strict code, but a
    /// braced escape is post-ES5 everywhere; sloppy `var let` / `var yield`
    /// stay legal.
    fn check_binding(&self, name: &str, span: SwcSpan, role: &str) -> Result<(), ParseError> {
        self.check_ident_raw(span, role)?;
        if !self.strict {
            return Ok(());
        }
        if name == "eval" || name == "arguments" {
            return Err(self.syntax(
                span,
                format!("{role} must not bind 'eval' or 'arguments' in strict mode code"),
            ));
        }
        if STRICT_RESERVED_WORDS.contains(&name) {
            return Err(self.syntax(
                span,
                format!("{role} must not use strict-mode reserved word '{name}'"),
            ));
        }
        Ok(())
    }

    /// Reference-occurrence check for assignment/update targets. The
    /// raw-lexeme check runs in both modes; only `eval`/`arguments` writes
    /// are restricted, and only in strict code; member-expression property
    /// roles are checked by the caller, never here.
    fn check_assignment_target(&self, name: &str, span: SwcSpan) -> Result<(), ParseError> {
        self.check_ident_raw(span, "an assignment target")?;
        if self.strict && (name == "eval" || name == "arguments") {
            return Err(self.syntax(
                span,
                "assignment to 'eval' or 'arguments' is not allowed in strict mode code",
            ));
        }
        Ok(())
    }

    /// Raw-lexeme check for identifier positions: post-ES5 braced Unicode
    /// escapes (`\u{...}`) are refused before the cooked SWC symbol loses
    /// the spelling. Only the actual token span is sliced, never a global
    /// text scan, so comments, string data, regexp patterns and
    /// escaped-backslash lookalikes are unaffected. Fixed ES5 `\uXXXX`
    /// escapes contain no brace and stay allowed; synthetic spans with no
    /// source slice pass through instead of misfiring.
    fn check_ident_raw(&self, span: SwcSpan, role: &str) -> Result<(), ParseError> {
        let lo = span.lo.0.saturating_sub(self.base) as usize;
        let hi = span.hi.0.saturating_sub(self.base) as usize;
        let raw = self.source.get(lo..hi).unwrap_or("");
        if raw.contains("\\u{") {
            return Err(self.unsupported(
                span,
                format!("{role} with a braced Unicode escape is post-ES5"),
            ));
        }
        Ok(())
    }

    fn admit_block(&mut self, block: &ast::BlockStmt) -> Result<(), ParseError> {
        for stmt in &block.stmts {
            self.admit_stmt(stmt)?;
        }
        Ok(())
    }

    /// SourceElement positions (script bodies and function bodies):
    /// function declarations are normative ES5.1 here in both modes, so
    /// they take the direct declaration path. Every other statement takes
    /// the ordinary statement path, where a function declaration is a
    /// strict-mode SyntaxError or a sloppy implementation extension.
    fn admit_source_element(&mut self, stmt: &ast::Stmt) -> Result<(), ParseError> {
        if let ast::Stmt::Decl(ast::Decl::Fn(function)) = stmt {
            return self.admit_function_decl(function);
        }
        self.admit_stmt(stmt)
    }

    fn admit_stmt(&mut self, stmt: &ast::Stmt) -> Result<(), ParseError> {
        match stmt {
            ast::Stmt::Block(block) => self.admit_block(block),
            ast::Stmt::Empty(_) | ast::Stmt::Debugger(_) => Ok(()),
            ast::Stmt::With(with) => {
                if self.strict {
                    return Err(self.syntax(
                        with.span,
                        "the 'with' statement is not allowed in strict mode code",
                    ));
                }
                // Sloppy `with` is admitted at the grammar stage; lowering it
                // is later work, refused by the P0 lowerer, not here.
                self.admit_expr(&with.obj)?;
                self.admit_stmt(&with.body)
            }
            ast::Stmt::Return(ret) => {
                if self.function_depth == 0 {
                    return Err(self.syntax(
                        ret.span,
                        "a 'return' statement is only allowed inside function code",
                    ));
                }
                if let Some(arg) = &ret.arg {
                    self.admit_expr(arg)?;
                }
                Ok(())
            }
            ast::Stmt::Labeled(labeled) => self.admit_labeled(labeled.label.sym.as_str(), labeled),
            ast::Stmt::Break(stop) => {
                match &stop.label {
                    Some(label) => {
                        self.check_ident_raw(label.span, "a 'break' label")?;
                        if !self
                            .labels
                            .iter()
                            .any(|(name, _)| name.as_str() == label.sym.as_str())
                        {
                            return Err(self.syntax(
                                stop.span,
                                format!("'break' targets undefined label '{}'", label.sym),
                            ));
                        }
                    }
                    None => {
                        if self.loop_depth == 0 && self.switch_depth == 0 {
                            return Err(self.syntax(
                                stop.span,
                                "an unlabeled 'break' is only allowed inside a loop or 'switch'",
                            ));
                        }
                    }
                }
                Ok(())
            }
            ast::Stmt::Continue(resume) => match &resume.label {
                Some(label) => {
                    self.check_ident_raw(label.span, "a 'continue' label")?;
                    match self
                        .labels
                        .iter()
                        .find(|(name, _)| name.as_str() == label.sym.as_str())
                    {
                        Some((_, true)) => Ok(()),
                        Some((_, false)) => Err(self.syntax(
                            resume.span,
                            format!(
                                "'continue' label '{}' does not label an iteration statement",
                                label.sym
                            ),
                        )),
                        None => Err(self.syntax(
                            resume.span,
                            format!("'continue' targets undefined label '{}'", label.sym),
                        )),
                    }
                }
                None => {
                    if self.loop_depth == 0 {
                        return Err(self.syntax(
                            resume.span,
                            "an unlabeled 'continue' is only allowed inside a loop",
                        ));
                    }
                    Ok(())
                }
            },
            ast::Stmt::If(branch) => {
                self.admit_expr(&branch.test)?;
                self.admit_stmt(&branch.cons)?;
                if let Some(alt) = &branch.alt {
                    self.admit_stmt(alt)?;
                }
                Ok(())
            }
            ast::Stmt::Switch(select) => {
                self.admit_expr(&select.discriminant)?;
                self.switch_depth += 1;
                for case in &select.cases {
                    if let Some(test) = &case.test {
                        self.admit_expr(test)?;
                    }
                    for stmt in &case.cons {
                        self.admit_stmt(stmt)?;
                    }
                }
                self.switch_depth -= 1;
                Ok(())
            }
            ast::Stmt::Throw(throw) => self.admit_expr(&throw.arg),
            ast::Stmt::Try(guarded) => {
                self.admit_block(&guarded.block)?;
                if let Some(handler) = &guarded.handler {
                    match &handler.param {
                        // `catch {}` without a binding is post-ES5.
                        None => {
                            return Err(self.unsupported(
                                handler.span,
                                "a 'catch' clause without a binding is post-ES5",
                            ));
                        }
                        Some(ast::Pat::Ident(binding)) => {
                            self.check_binding(
                                binding.id.sym.as_str(),
                                binding.id.span,
                                "a 'catch' parameter",
                            )?;
                        }
                        Some(other) => {
                            return Err(self.unsupported(
                                other.span(),
                                "destructuring 'catch' parameters are post-ES5",
                            ));
                        }
                    }
                    self.admit_block(&handler.body)?;
                }
                if let Some(finalizer) = &guarded.finalizer {
                    self.admit_block(finalizer)?;
                }
                Ok(())
            }
            ast::Stmt::While(loop_) => {
                self.admit_expr(&loop_.test)?;
                self.loop_depth += 1;
                let result = self.admit_stmt(&loop_.body);
                self.loop_depth -= 1;
                result
            }
            ast::Stmt::DoWhile(loop_) => {
                self.loop_depth += 1;
                let result = self.admit_stmt(&loop_.body);
                self.loop_depth -= 1;
                result?;
                self.admit_expr(&loop_.test)
            }
            ast::Stmt::For(loop_) => {
                if let Some(init) = &loop_.init {
                    match init {
                        ast::VarDeclOrExpr::VarDecl(decl) => self.admit_var_decl(decl)?,
                        ast::VarDeclOrExpr::Expr(expr) => {
                            self.admit_expr(expr)?;
                        }
                    }
                }
                if let Some(test) = &loop_.test {
                    self.admit_expr(test)?;
                }
                if let Some(update) = &loop_.update {
                    self.admit_expr(update)?;
                }
                self.loop_depth += 1;
                let result = self.admit_stmt(&loop_.body);
                self.loop_depth -= 1;
                result
            }
            ast::Stmt::ForIn(loop_) => {
                match &loop_.left {
                    ast::ForHead::VarDecl(decl) => {
                        if decl.kind != ast::VarDeclKind::Var {
                            return Err(self.unsupported(
                                decl.span,
                                "'for-in' with 'let' or 'const' is post-ES5",
                            ));
                        }
                        if decl.decls.len() != 1 {
                            return Err(self.syntax(
                                decl.span,
                                "a 'for-in' declaration binds exactly one name",
                            ));
                        }
                        let declarator = &decl.decls[0];
                        match &declarator.name {
                            ast::Pat::Ident(binding) => {
                                self.check_binding(
                                    binding.id.sym.as_str(),
                                    binding.id.span,
                                    "a 'for-in' variable",
                                )?;
                            }
                            other => {
                                return Err(self.unsupported(
                                    other.span(),
                                    "destructuring 'for-in' targets are post-ES5",
                                ));
                            }
                        }
                        if declarator.init.is_some() {
                            return Err(self.syntax(
                                declarator.span,
                                "a 'for-in' declaration must not have an initializer",
                            ));
                        }
                    }
                    ast::ForHead::UsingDecl(decl) => {
                        return Err(
                            self.unsupported(decl.span, "'using' declarations are post-ES5")
                        );
                    }
                    ast::ForHead::Pat(target) => self.admit_for_in_target(target)?,
                }
                self.admit_expr(&loop_.right)?;
                self.loop_depth += 1;
                let result = self.admit_stmt(&loop_.body);
                self.loop_depth -= 1;
                result
            }
            ast::Stmt::ForOf(loop_) => Err(self.unsupported(
                loop_.span,
                "'for-of' iteration is post-ES5; ES5.1 only has 'for-in'",
            )),
            ast::Stmt::Decl(decl) => {
                match decl {
                    ast::Decl::Fn(function) => self.admit_statement_function(function),
                    ast::Decl::Var(var) => self.admit_var_decl(var),
                    ast::Decl::Class(class) => {
                        Err(self.unsupported(class.span(), "'class' declarations are post-ES5"))
                    }
                    ast::Decl::Using(using) => {
                        Err(self.unsupported(using.span, "'using' declarations are post-ES5"))
                    }
                    ast::Decl::TsInterface(interface) => Err(self.unsupported(
                        interface.span(),
                        "TypeScript interface declarations are not ES5.1",
                    )),
                    ast::Decl::TsTypeAlias(alias) => {
                        Err(self.unsupported(alias.span, "TypeScript type aliases are not ES5.1"))
                    }
                    ast::Decl::TsEnum(enumeration) => Err(self.unsupported(
                        enumeration.span(),
                        "TypeScript enum declarations are not ES5.1",
                    )),
                    ast::Decl::TsModule(module) => Err(self
                        .unsupported(module.span, "TypeScript module declarations are not ES5.1")),
                }
            }
            ast::Stmt::Expr(write) => self.admit_expr(&write.expr),
        }
    }

    /// Labeled statements with label-set semantics: labels accumulate through
    /// a label chain, so every label in `a: b: <loop>` labels the loop.
    fn admit_labeled(&mut self, label: &str, labeled: &ast::LabeledStmt) -> Result<(), ParseError> {
        self.check_ident_raw(labeled.label.span, "a label")?;
        if self.labels.iter().any(|(name, _)| name == label) {
            return Err(self.syntax(labeled.span, format!("duplicate label '{label}'")));
        }
        let mut chain: &ast::Stmt = &labeled.body;
        while let ast::Stmt::Labeled(inner) = chain {
            self.check_ident_raw(inner.label.span, "a label")?;
            if inner.label.sym.as_str() == label
                || self
                    .labels
                    .iter()
                    .any(|(name, _)| name == inner.label.sym.as_str())
            {
                return Err(self.syntax(inner.span, "duplicate label in label chain"));
            }
            chain = &inner.body;
        }
        let is_iteration = matches!(
            chain,
            ast::Stmt::While(_) | ast::Stmt::DoWhile(_) | ast::Stmt::For(_) | ast::Stmt::ForIn(_)
        );
        self.labels.push((label.to_owned(), is_iteration));
        // Inner chain labels are pushed by the recursive walk below.
        let result = self.admit_stmt(&labeled.body);
        self.labels.truncate(
            self.labels
                .iter()
                .rposition(|(name, _)| name == label)
                .unwrap_or(0),
        );
        result
    }

    /// Assignment-target expressions: identifiers are raw-checked then
    /// strict-checked (`eval`/`arguments` writes are rejected in strict
    /// code) while member expressions are walked as references through
    /// parentheses. Private member names are post-ES5 in target position
    /// exactly as in read position. Anything else is a syntax error, not
    /// an unsupported-lowering case.
    ///
    /// Member-expression property roles never reach the binding checks here:
    /// `x.eval = 1` is a property write, not an `eval` binding.
    fn admit_assign_target_expr(&mut self, target: &ast::Expr) -> Result<(), ParseError> {
        let mut current = target;
        loop {
            match current {
                ast::Expr::Ident(name) => {
                    return self.check_assignment_target(name.sym.as_str(), name.span);
                }
                ast::Expr::Member(member) => {
                    self.admit_expr(&member.obj)?;
                    match &member.prop {
                        ast::MemberProp::Computed(computed) => {
                            self.admit_expr(&computed.expr)?;
                        }
                        ast::MemberProp::Ident(prop) => {
                            self.check_ident_raw(prop.span, "a member name")?;
                        }
                        ast::MemberProp::PrivateName(name) => {
                            return Err(
                                self.unsupported(name.span, "private member names are post-ES5")
                            );
                        }
                    }
                    return Ok(());
                }
                ast::Expr::Paren(parenthesized) => {
                    current = &parenthesized.expr;
                }
                other => {
                    return Err(self.syntax(other.span(), "invalid assignment target"));
                }
            }
        }
    }

    /// `for-in` reference targets: identifiers bind (strict-checked) while
    /// parenthesized and member targets are walked as assignment references.
    /// Anything else is a syntax error, not an unsupported-lowering case.
    fn admit_for_in_target(&mut self, target: &ast::Pat) -> Result<(), ParseError> {
        match target {
            ast::Pat::Ident(binding) => self.check_binding(
                binding.id.sym.as_str(),
                binding.id.span,
                "a 'for-in' variable",
            ),
            ast::Pat::Expr(expr) => self.admit_assign_target_expr(expr),
            other => {
                Err(self.unsupported(other.span(), "destructuring 'for-in' targets are post-ES5"))
            }
        }
    }

    fn admit_var_decl(&mut self, decl: &ast::VarDecl) -> Result<(), ParseError> {
        match decl.kind {
            ast::VarDeclKind::Var => {}
            ast::VarDeclKind::Let => {
                return Err(self.unsupported(decl.span, "'let' declarations are post-ES5"));
            }
            ast::VarDeclKind::Const => {
                return Err(self.unsupported(decl.span, "'const' declarations are post-ES5"));
            }
        }
        if decl.declare {
            return Err(
                self.unsupported(decl.span, "'declare' variable declarations are not ES5.1")
            );
        }
        for declarator in &decl.decls {
            match &declarator.name {
                ast::Pat::Ident(binding) => {
                    self.check_binding(binding.id.sym.as_str(), binding.id.span, "a variable")?;
                }
                other => {
                    return Err(self.unsupported(
                        other.span(),
                        "destructuring variable declarations are post-ES5",
                    ));
                }
            }
            if let Some(init) = &declarator.init {
                self.admit_expr(init)?;
            }
        }
        Ok(())
    }

    /// Shared function admission for declarations and expressions.
    ///
    /// The effective strictness is the inherited strictness plus the
    /// function's own directive prologue; the function's own name and
    /// parameters are validated against that effective strictness (so
    /// `function f(eval) { "use strict"; }` is rejected even though the
    /// parameter list precedes the directive textually).
    fn admit_function(
        &mut self,
        name: Option<&ast::Ident>,
        function: &ast::Function,
    ) -> Result<(), ParseError> {
        if function.is_generator {
            return Err(self.unsupported(function.span, "generator functions are post-ES5"));
        }
        if function.is_async {
            return Err(self.unsupported(function.span, "'async' functions are post-ES5"));
        }
        if function.this_param.is_some()
            || function.type_params.is_some()
            || function.return_type.is_some()
            || !function.decorators.is_empty()
        {
            return Err(self.unsupported(function.span, "annotated function headers are not ES5.1"));
        }
        let Some(body) = &function.body else {
            return Err(self.syntax(function.span, "a function requires a body"));
        };
        let effective_strict = self.strict || prologue_strict(&body.stmts);
        let mut params: Vec<(&str, SwcSpan)> = Vec::with_capacity(function.params.len());
        for param in &function.params {
            if !param.decorators.is_empty() {
                return Err(self.unsupported(param.span, "decorated parameters are post-ES5"));
            }
            match &param.pat {
                ast::Pat::Ident(binding) => {
                    params.push((binding.id.sym.as_str(), binding.id.span));
                }
                ast::Pat::Rest(_) => {
                    return Err(self.unsupported(param.span, "rest parameters are post-ES5"));
                }
                ast::Pat::Assign(_) => {
                    return Err(self.unsupported(param.span, "default parameters are post-ES5"));
                }
                ast::Pat::Array(_) | ast::Pat::Object(_) => {
                    return Err(
                        self.unsupported(param.span, "destructuring parameters are post-ES5")
                    );
                }
                ast::Pat::Invalid(invalid) => {
                    return Err(self.syntax(invalid.span, "invalid function parameter"));
                }
                ast::Pat::Expr(_) => {
                    return Err(self.syntax(param.span, "invalid function parameter"));
                }
            }
        }
        let outer_strict = self.strict;
        let outer_loop_depth = self.loop_depth;
        let outer_switch_depth = self.switch_depth;
        let outer_labels = std::mem::take(&mut self.labels);
        self.strict = effective_strict;
        // Every function body opens a fresh control-flow context: `break`,
        // `continue` and labels never cross the boundary in either
        // direction, and getter/setter bodies route through here as well.
        self.loop_depth = 0;
        self.switch_depth = 0;
        self.function_depth += 1;
        let result = self.admit_function_body(name, &params, &body.stmts, effective_strict);
        self.function_depth -= 1;
        self.switch_depth = outer_switch_depth;
        self.loop_depth = outer_loop_depth;
        self.labels = outer_labels;
        self.strict = outer_strict;
        result
    }

    /// Function-body walk with the caller's control context already isolated.
    /// Uses `?` freely: the caller restores the outer loop/switch/label
    /// state on every exit, success or failure.
    fn admit_function_body(
        &mut self,
        name: Option<&ast::Ident>,
        params: &[(&str, SwcSpan)],
        body: &[ast::Stmt],
        effective_strict: bool,
    ) -> Result<(), ParseError> {
        if let Some(name) = name {
            self.check_binding(name.sym.as_str(), name.span, "a function name")?;
        }
        for &(name, span) in params {
            self.check_binding(name, span, "a parameter")?;
        }
        if effective_strict {
            let mut seen: HashMap<&str, SwcSpan> = HashMap::new();
            for &(name, span) in params {
                if let Some(first) = seen.get(name) {
                    return Err(self.syntax(
                        span,
                        format!(
                            "duplicate parameter '{name}' is not allowed in strict mode code (first at bytes {}..{})",
                            first.lo.0.saturating_sub(self.base),
                            first.hi.0.saturating_sub(self.base),
                        ),
                    ));
                }
                seen.insert(name, span);
            }
        }
        for stmt in body {
            self.admit_source_element(stmt)?;
        }
        Ok(())
    }

    fn admit_function_decl(&mut self, decl: &ast::FnDecl) -> Result<(), ParseError> {
        self.admit_function(Some(&decl.ident), &decl.function)
    }

    /// Statement-position function declarations (blocks, `if` branches,
    /// loop bodies, labels, `switch` cases, `try` parts): normative ES5.1
    /// has no FunctionDeclaration here at all. Strict code rejects them
    /// with a SyntaxError; sloppy code keeps them only as an explicitly
    /// labeled implementation extension outside normative ES5.1 (never an
    /// ES5.1 Annex B rule: ES5.1 normatively allows declarations in
    /// SourceElements positions only).
    ///
    /// The position check reads the enclosing strictness: an own strict
    /// directive inside the admitted extension function governs its name,
    /// params and body but never makes the outer block strict.
    fn admit_statement_function(&mut self, decl: &ast::FnDecl) -> Result<(), ParseError> {
        if self.strict {
            return Err(self.syntax(
                decl.function.span,
                "function declarations in statement position are not allowed in strict mode code",
            ));
        }
        self.admit_function(Some(&decl.ident), &decl.function)
    }

    fn admit_expr(&mut self, expr: &ast::Expr) -> Result<(), ParseError> {
        match expr {
            ast::Expr::This(_) => Ok(()),
            ast::Expr::Array(literal) => {
                for element in literal.elems.iter().flatten() {
                    if element.spread.is_some() {
                        return Err(
                            self.unsupported(element.expr.span(), "spread elements are post-ES5")
                        );
                    }
                    self.admit_expr(&element.expr)?;
                }
                Ok(())
            }
            ast::Expr::Object(literal) => self.admit_object(literal),
            ast::Expr::Fn(function) => {
                self.admit_function(function.ident.as_ref(), &function.function)
            }
            ast::Expr::Unary(unary) => {
                if unary.op == ast::UnaryOp::Delete {
                    // Only unqualified identifiers are restricted, and only in
                    // strict code; `delete obj.prop` stays admitted.
                    let mut target = &*unary.arg;
                    loop {
                        match target {
                            ast::Expr::Ident(_) => {
                                if self.strict {
                                    return Err(self.syntax(
                                        unary.span,
                                        "deleting an unqualified identifier is not allowed in strict mode code",
                                    ));
                                }
                                break;
                            }
                            ast::Expr::Paren(parenthesized) => {
                                target = &parenthesized.expr;
                            }
                            _ => break,
                        }
                    }
                }
                self.admit_expr(&unary.arg)
            }
            ast::Expr::Update(update) => self.admit_assign_target_expr(&update.arg),
            ast::Expr::Bin(binary) => {
                match binary.op {
                    ast::BinaryOp::Exp => {
                        return Err(self
                            .unsupported(binary.span, "the exponentiation operator is post-ES5"));
                    }
                    ast::BinaryOp::NullishCoalescing => {
                        return Err(self.unsupported(
                            binary.span,
                            "the nullish-coalescing operator is post-ES5",
                        ));
                    }
                    _ => {}
                }
                self.admit_expr(&binary.left)?;
                self.admit_expr(&binary.right)
            }
            ast::Expr::Assign(assign) => {
                match assign.op {
                    ast::AssignOp::ExpAssign
                    | ast::AssignOp::AndAssign
                    | ast::AssignOp::OrAssign
                    | ast::AssignOp::NullishAssign => {
                        return Err(self.unsupported(
                            assign.span,
                            "logical and exponentiation assignment are post-ES5",
                        ));
                    }
                    _ => {}
                }
                match &assign.left {
                    ast::AssignTarget::Simple(ast::SimpleAssignTarget::Ident(binding)) => {
                        self.check_assignment_target(binding.id.sym.as_str(), binding.id.span)?;
                    }
                    ast::AssignTarget::Simple(ast::SimpleAssignTarget::Member(member)) => {
                        self.admit_expr(&member.obj)?;
                        match &member.prop {
                            ast::MemberProp::Computed(computed) => {
                                self.admit_expr(&computed.expr)?;
                            }
                            ast::MemberProp::Ident(prop) => {
                                self.check_ident_raw(prop.span, "a member name")?;
                            }
                            ast::MemberProp::PrivateName(name) => {
                                return Err(self
                                    .unsupported(name.span, "private member names are post-ES5"));
                            }
                        }
                    }
                    ast::AssignTarget::Simple(ast::SimpleAssignTarget::Paren(parenthesized)) => {
                        self.admit_assign_target_expr(&parenthesized.expr)?;
                    }
                    ast::AssignTarget::Simple(
                        ast::SimpleAssignTarget::SuperProp(_)
                        | ast::SimpleAssignTarget::OptChain(_)
                        | ast::SimpleAssignTarget::TsAs(_)
                        | ast::SimpleAssignTarget::TsSatisfies(_)
                        | ast::SimpleAssignTarget::TsNonNull(_)
                        | ast::SimpleAssignTarget::TsTypeAssertion(_)
                        | ast::SimpleAssignTarget::TsInstantiation(_)
                        | ast::SimpleAssignTarget::Invalid(_),
                    ) => {
                        return Err(self.syntax(assign.span, "invalid assignment target"));
                    }
                    ast::AssignTarget::Pat(_) => {
                        return Err(
                            self.unsupported(assign.span, "destructuring assignment is post-ES5")
                        );
                    }
                }
                self.admit_expr(&assign.right)
            }
            ast::Expr::Member(member) => {
                self.admit_expr(&member.obj)?;
                match &member.prop {
                    // Reserved words are legal property names in ES5
                    // (`x.let`); no binding check applies here, but the raw
                    // lexeme is still checked for post-ES5 escapes.
                    ast::MemberProp::Ident(prop) => {
                        self.check_ident_raw(prop.span, "a member name")?;
                        Ok(())
                    }
                    ast::MemberProp::Computed(computed) => self.admit_expr(&computed.expr),
                    ast::MemberProp::PrivateName(name) => {
                        Err(self.unsupported(name.span, "private member names are post-ES5"))
                    }
                }
            }
            ast::Expr::SuperProp(prop) => {
                Err(self.unsupported(prop.span, "'super' property access is post-ES5"))
            }
            ast::Expr::Cond(branch) => {
                self.admit_expr(&branch.test)?;
                self.admit_expr(&branch.cons)?;
                self.admit_expr(&branch.alt)
            }
            ast::Expr::Call(call) => {
                match &call.callee {
                    ast::Callee::Super(_) => {
                        return Err(self.unsupported(call.span, "'super' calls are post-ES5"));
                    }
                    ast::Callee::Import(_) => {
                        return Err(
                            self.unsupported(call.span, "'import' expressions are post-ES5")
                        );
                    }
                    ast::Callee::Expr(callee) => self.admit_expr(callee)?,
                }
                if call.type_args.is_some() {
                    return Err(
                        self.unsupported(call.span, "generic call type arguments are not ES5.1")
                    );
                }
                for arg in &call.args {
                    if arg.spread.is_some() {
                        return Err(
                            self.unsupported(arg.expr.span(), "spread call arguments are post-ES5")
                        );
                    }
                    self.admit_expr(&arg.expr)?;
                }
                Ok(())
            }
            ast::Expr::New(construct) => {
                self.admit_expr(&construct.callee)?;
                if let Some(args) = &construct.args {
                    for arg in args {
                        if arg.spread.is_some() {
                            return Err(self.unsupported(
                                arg.expr.span(),
                                "spread construction arguments are post-ES5",
                            ));
                        }
                        self.admit_expr(&arg.expr)?;
                    }
                }
                if construct.type_args.is_some() {
                    return Err(self.unsupported(
                        construct.span,
                        "generic construction type arguments are not ES5.1",
                    ));
                }
                Ok(())
            }
            ast::Expr::Seq(sequence) => {
                for expr in &sequence.exprs {
                    self.admit_expr(expr)?;
                }
                Ok(())
            }
            // Identifier reads are always admitted: `eval` and `arguments`
            // are only restricted as binding/assignment targets in strict
            // code, never as references. The raw lexeme is still checked
            // for post-ES5 escapes before the cooked symbol is trusted.
            ast::Expr::Ident(name) => self.check_ident_raw(name.span, "an identifier"),
            ast::Expr::Lit(literal) => self.admit_literal(literal),
            ast::Expr::Paren(parenthesized) => self.admit_expr(&parenthesized.expr),
            ast::Expr::Tpl(_) => {
                Err(self.unsupported(expr.span(), "template literals are post-ES5"))
            }
            ast::Expr::TaggedTpl(_) => {
                Err(self.unsupported(expr.span(), "tagged templates are post-ES5"))
            }
            ast::Expr::Arrow(_) => {
                Err(self.unsupported(expr.span(), "arrow functions are post-ES5"))
            }
            ast::Expr::Class(_) => {
                Err(self.unsupported(expr.span(), "'class' expressions are post-ES5"))
            }
            ast::Expr::Yield(_) => {
                Err(self.unsupported(expr.span(), "'yield' expressions are post-ES5"))
            }
            ast::Expr::MetaProp(_) => {
                Err(self.unsupported(expr.span(), "'new.target' meta-properties are post-ES5"))
            }
            ast::Expr::Await(_) => {
                Err(self.unsupported(expr.span(), "'await' expressions are post-ES5"))
            }
            ast::Expr::JSXMember(_)
            | ast::Expr::JSXNamespacedName(_)
            | ast::Expr::JSXEmpty(_)
            | ast::Expr::JSXElement(_)
            | ast::Expr::JSXFragment(_) => {
                Err(self.unsupported(expr.span(), "JSX expressions are not ES5.1"))
            }
            ast::Expr::TsTypeAssertion(_)
            | ast::Expr::TsConstAssertion(_)
            | ast::Expr::TsNonNull(_)
            | ast::Expr::TsAs(_)
            | ast::Expr::TsInstantiation(_)
            | ast::Expr::TsSatisfies(_) => Err(self.unsupported(
                expr.span(),
                "TypeScript assertion expressions are not ES5.1",
            )),
            ast::Expr::PrivateName(name) => {
                Err(self.unsupported(name.span, "private names are post-ES5"))
            }
            ast::Expr::OptChain(_) => {
                Err(self.unsupported(expr.span(), "optional chaining is post-ES5"))
            }
            ast::Expr::Invalid(invalid) => Err(self.syntax(invalid.span, "invalid expression")),
        }
    }

    fn admit_literal(&mut self, literal: &ast::Lit) -> Result<(), ParseError> {
        match literal {
            ast::Lit::Str(value) => self.admit_string(value),
            ast::Lit::Num(value) => self.admit_number(value),
            ast::Lit::Bool(_) | ast::Lit::Null(_) => Ok(()),
            ast::Lit::Regex(value) => self.admit_regex(value),
            ast::Lit::BigInt(value) => {
                Err(self.unsupported(value.span, "BigInt literals are post-ES5"))
            }
            ast::Lit::JSXText(_) => Err(self.unsupported(literal.span(), "JSX text is not ES5.1")),
        }
    }

    /// String lexical forms through the shared original-source codec. The
    /// whole containing code's resolved strictness governs the
    /// strict/sloppy-AnnexB policy, including strings before a later exact
    /// directive in the same prologue (checker strictness is resolved
    /// upfront) and inherited strict function code. Every string token
    /// position the admission walk visits is validated here: directives,
    /// unlowered statement/function bodies, values and computed member
    /// expressions (property names route through `canonical_key`).
    fn admit_string(&self, value: &ast::Str) -> Result<(), ParseError> {
        let Some(raw) = &value.raw else {
            return Err(self.syntax(
                value.span,
                "string literal is missing its original source spelling",
            ));
        };
        match decode_string_literal(raw.as_str(), self.string_policy()) {
            Ok(_) => Ok(()),
            Err(codec) => Err(self.string_codec_error(value.span, codec)),
        }
    }

    /// Current strictness as the shared codec policy.
    fn string_policy(&self) -> StringStrictness {
        if self.strict {
            StringStrictness::Strict
        } else {
            StringStrictness::Sloppy
        }
    }

    /// Map a codec failure to its truthful grammar-stage layer: ES5
    /// violations are SyntaxErrors, post-ES5 spellings are unsupported
    /// syntax, and the unit bound stays a resource diagnostic. The span is
    /// narrowed to the offending escape with the caller source id kept.
    fn string_codec_error(&self, span: SwcSpan, codec: StringCodecError) -> ParseError {
        let span = narrowed_span(span, codec.relative_offset);
        match codec.kind {
            StringCodecErrorKind::Lexical => self.syntax(span, codec.message),
            StringCodecErrorKind::PostEs5 => self.unsupported(span, codec.message),
            StringCodecErrorKind::Oversize => {
                parse_error(self.source_id, self.base, span, codec.message)
            }
        }
    }

    /// Numeric lexical forms. Explicit `0b`/`0o` prefixes and separators are
    /// post-ES5 in every mode; legacy leading-zero decimals are strict-mode
    /// early errors and sloppy-mode legacy admissions. Hex stays admitted.
    fn admit_number(&self, value: &ast::Number) -> Result<(), ParseError> {
        let Some(raw) = &value.raw else {
            return Ok(());
        };
        let text = raw.as_str();
        if text.contains('_') {
            return Err(self.unsupported(value.span, "numeric separators are post-ES5"));
        }
        let lowered = text.to_ascii_lowercase();
        if lowered.starts_with("0b") || lowered.starts_with("0o") {
            return Err(self.unsupported(
                value.span,
                "binary and explicit-octal numeric prefixes are post-ES5",
            ));
        }
        let bytes = text.as_bytes();
        if bytes.len() > 1 && bytes[0] == b'0' && bytes[1].is_ascii_digit() {
            // Sloppy legacy octal decimals (`010`, and the `08`/`09`
            // implementation extension) are admitted as legacy; strict code
            // rejects them.
            if self.strict {
                return Err(self.syntax(
                    value.span,
                    "legacy octal numeric literals are not allowed in strict mode code",
                ));
            }
        }
        Ok(())
    }

    /// ES5 regexp admission: only `g`/`i`/`m` flags without duplicates;
    /// lookahead (`(?=...)`, `(?!...)`) and non-capturing (`(?:...)`) groups
    /// are normative ES5.1 section 15.10.1 and stay admitted, while named
    /// groups and lookbehind are refused. The scan skips escapes and
    /// character classes, so `/[()]/`, `/\\(/` and `/(?=a)/` stay admitted.
    /// Full pattern conformance is deferred to the later RegExp phase.
    fn admit_regex(&self, value: &ast::Regex) -> Result<(), ParseError> {
        let flags = value.flags.as_str();
        for flag in flags.chars() {
            if !matches!(flag, 'g' | 'i' | 'm') {
                return Err(self.unsupported(
                    value.span,
                    format!("regexp flag '{flag}' is post-ES5 (ES5.1 admits only g, i, m)"),
                ));
            }
        }
        let mut seen = [false; 128];
        for flag in flags.bytes() {
            if seen[flag as usize] {
                return Err(self.syntax(
                    value.span,
                    format!("duplicate regexp flag '{}'", flag as char),
                ));
            }
            seen[flag as usize] = true;
        }
        let pattern = value.exp.as_str().as_bytes();
        let mut index = 0;
        while index < pattern.len() {
            match pattern[index] {
                b'\\' => index += 2,
                b'[' => {
                    index += 1;
                    while index < pattern.len() && pattern[index] != b']' {
                        index += if pattern[index] == b'\\' { 2 } else { 1 };
                    }
                    index += 1;
                }
                b'(' => {
                    if pattern.get(index + 1) == Some(&b'?') {
                        match pattern.get(index + 2) {
                            // ES5.1 section 15.10.1 lookahead and
                            // non-capturing groups stay admitted.
                            Some(b'=') | Some(b'!') | Some(b':') => index += 3,
                            // Named groups and lookbehind are unambiguous
                            // modern syntax.
                            _ => {
                                return Err(self.unsupported(
                                    value.span,
                                    "named regexp groups and lookbehind are post-ES5",
                                ));
                            }
                        }
                    } else {
                        index += 1;
                    }
                }
                _ => index += 1,
            }
        }
        Ok(())
    }

    /// Object literals with ES5.1 duplicate-property rules. Keys compare by
    /// canonical UTF-16 units, so `1`, `"1"`, `1.0` and `"\u0031"` collide
    /// while `"01"` and `"1e21"` do not. `__proto__` keys are ordinary data
    /// keys for grammar purposes. Strict code rejects duplicate data keys,
    /// duplicate getters, duplicate setters and data/accessor conflicts; only
    /// a lone getter+setter pair stays legal (a second getter after a pair is
    /// still a duplicate getter). Sloppy code admits every combination.
    fn admit_object(&mut self, literal: &ast::ObjectLit) -> Result<(), ParseError> {
        let mut seen: HashMap<Vec<u16>, (PropertyRoles, SwcSpan)> = HashMap::new();
        for prop in &literal.props {
            match prop {
                ast::PropOrSpread::Spread(spread) => {
                    return Err(
                        self.unsupported(spread.expr.span(), "spread properties are post-ES5")
                    );
                }
                ast::PropOrSpread::Prop(prop) => match &**prop {
                    ast::Prop::KeyValue(pair) => {
                        let key = self.canonical_key(&pair.key)?;
                        self.check_duplicate(&mut seen, key, PropertyKind::Data, pair.span())?;
                        self.admit_expr(&pair.value)?;
                    }
                    ast::Prop::Getter(getter) => {
                        let key = self.canonical_key(&getter.key)?;
                        self.check_duplicate(&mut seen, key, PropertyKind::Getter, getter.span)?;
                        if !getter.function.params.is_empty() {
                            return Err(self.syntax(getter.span, "a getter takes no parameters"));
                        }
                        self.admit_function(None, &getter.function)?;
                    }
                    ast::Prop::Setter(setter) => {
                        let key = self.canonical_key(&setter.key)?;
                        self.check_duplicate(&mut seen, key, PropertyKind::Setter, setter.span)?;
                        if setter.function.params.len() != 1 {
                            return Err(
                                self.syntax(setter.span, "a setter takes exactly one parameter")
                            );
                        }
                        self.admit_function(None, &setter.function)?;
                    }
                    ast::Prop::Shorthand(_) => {
                        return Err(
                            self.unsupported(prop.span(), "shorthand properties are post-ES5")
                        );
                    }
                    ast::Prop::Assign(_) => {
                        return Err(self.unsupported(
                            prop.span(),
                            "assignment properties are not valid object literals",
                        ));
                    }
                    ast::Prop::Method(_) => {
                        return Err(self.unsupported(prop.span(), "method properties are post-ES5"));
                    }
                },
            }
        }
        Ok(())
    }

    fn canonical_key(&self, key: &ast::PropName) -> Result<Vec<u16>, ParseError> {
        match key {
            ast::PropName::Ident(name) => {
                self.check_ident_raw(name.span, "a property name")?;
                Ok(name.sym.encode_utf16().collect())
            }
            // Property names decode through the same shared codec over the
            // original token bytes, so admission spelling checks and
            // duplicate-key canonicalization observe identical units.
            ast::PropName::Str(value) => {
                let Some(raw) = &value.raw else {
                    return Err(self.syntax(
                        value.span(),
                        "property name is missing its original source spelling",
                    ));
                };
                match decode_string_literal(raw.as_str(), self.string_policy()) {
                    Ok(units) => Ok(units),
                    Err(codec) => Err(self.string_codec_error(value.span(), codec)),
                }
            }
            ast::PropName::Num(value) => Ok(number_to_string(value.value).encode_utf16().collect()),
            ast::PropName::Computed(computed) => {
                Err(self.unsupported(computed.span, "computed property names are post-ES5"))
            }
            ast::PropName::BigInt(value) => {
                Err(self.unsupported(value.span, "BigInt property keys are post-ES5"))
            }
        }
    }

    fn check_duplicate(
        &self,
        seen: &mut HashMap<Vec<u16>, (PropertyRoles, SwcSpan)>,
        key: Vec<u16>,
        kind: PropertyKind,
        span: SwcSpan,
    ) -> Result<(), ParseError> {
        use PropertyKind::{Data, Getter, Setter};
        let (roles, _) = seen
            .entry(key)
            .or_insert_with(|| (PropertyRoles::default(), span));
        let conflict = if !roles.data && !roles.getter && !roles.setter {
            // First occurrence of this canonical name.
            None
        } else {
            match kind {
                Data if roles.data => Some("duplicate data property"),
                Data => Some("data and accessor property conflict"),
                Getter if roles.getter => Some("duplicate getter"),
                Getter if roles.data => Some("data and accessor property conflict"),
                Setter if roles.setter => Some("duplicate setter"),
                Setter if roles.data => Some("data and accessor property conflict"),
                // The lone getter+setter pair accumulates without conflict.
                Getter | Setter => None,
            }
        };
        let record = |roles: &mut PropertyRoles| match kind {
            Data => roles.data = true,
            Getter => roles.getter = true,
            Setter => roles.setter = true,
        };
        match conflict {
            Some(label) if self.strict => {
                Err(self.syntax(span, format!("{label} is not allowed in strict mode code")))
            }
            _ => {
                record(roles);
                Ok(())
            }
        }
    }
}
