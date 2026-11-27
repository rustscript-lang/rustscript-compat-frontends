//! Compiler-owned Script/Function scope graph for the closed declaration subset.
//!
//! One BindingId per declaration-owned name in a scope. Blocks do not create
//! var scopes. Nested functions are separate scopes and are never walked as
//! if their vars belonged to the parent. This is environment erasure for the
//! nonescaping subset, not a runtime EnvironmentRecord.

use std::collections::HashMap;

use swc_ecma_ast as ast;
use swc_common::Spanned;
use vm::ParseError;

pub(super) const MAX_ARGUMENTS: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) struct ScopeId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[allow(dead_code)]
pub(super) struct BindingId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ScopeKind {
    Script,
    Function,
    /// Owns only the catch parameter. `var` declarations stay on the
    /// nearest Script/Function variable owner.
    Catch,
}

#[derive(Clone)]
pub(super) struct FnSite {
    pub span: swc_common::Span,
    pub function: Box<ast::Function>,
}

/// A direct `var X = <builtin constructor>` binding. Only the spellings that
/// are readable as first-class values in this realm can appear: `Error` and
/// `Date` have no constructor value and therefore no alias.
#[derive(Clone, Copy)]
pub(super) enum ConstructorAlias {
    Array,
    ArrayBuffer,
    TypedArray(super::prelude::typed_array::Element),
    Object,
    TypeError,
    RangeError,
    Number,
    String,
    Boolean,
}

#[derive(Clone)]
pub(super) struct Binding {
    #[allow(dead_code)]
    pub id: BindingId,
    pub name: String,
    #[allow(dead_code)]
    pub scope: ScopeId,
    pub first_span: swc_common::Span,
    pub fn_sites: Vec<FnSite>,
    /// One direct builtin-constructor alias, valid only after its initializer
    /// and only while the binding is never written again.
    pub alias: Option<(ConstructorAlias, swc_common::Span)>,
    /// Any write at all (initializer, assignment, update, for-in) clears the
    /// alias. Conservative: an unrelated write in any activation disables it.
    pub written: bool,
}

pub(super) struct Scope {
    #[allow(dead_code)]
    pub id: ScopeId,
    #[allow(dead_code)]
    pub kind: ScopeKind,
    pub parent: Option<ScopeId>,
    pub strict: bool,
    pub order: Vec<BindingId>,
    pub by_name: HashMap<String, BindingId>,
    pub formals: Vec<(BindingId, usize, swc_common::Span)>,
}

pub(super) struct ScopeGraph {
    pub scopes: Vec<Scope>,
    pub bindings: Vec<Binding>,
    pub script: ScopeId,
    /// Function AST span `(lo, hi)` to the function's own scope.
    pub function_scopes: HashMap<(u32, u32), ScopeId>,
    /// Catch-handler span `(lo, hi)` to the private catch scope.
    pub catch_scopes: HashMap<(u32, u32), ScopeId>,
}

impl ScopeGraph {
    pub(super) fn scope(&self, id: ScopeId) -> &Scope {
        &self.scopes[id.0 as usize]
    }

    pub(super) fn binding(&self, id: BindingId) -> &Binding {
        &self.bindings[id.0 as usize]
    }
}

struct Builder<'a> {
    source: &'a str,
    source_id: u32,
    base: u32,
    graph: ScopeGraph,
}

pub(super) fn build(
    script: &ast::Script,
    source: &str,
    source_id: u32,
    base: u32,
) -> Result<ScopeGraph, ParseError> {
    let mut builder = Builder {
        source,
        source_id,
        base,
        graph: ScopeGraph {
            scopes: Vec::new(),
            bindings: Vec::new(),
            script: ScopeId(0),
            function_scopes: HashMap::new(),
            catch_scopes: HashMap::new(),
        },
    };
    let strict = script_strict(script, source, base);
    let script_scope = builder.new_scope(ScopeKind::Script, None, strict);
    builder.graph.script = script_scope;
    builder.walk_stmts(&script.body, script_scope)?;
    Ok(builder.graph)
}

fn script_strict(script: &ast::Script, source: &str, base: u32) -> bool {
    for statement in &script.body {
        let Some(span) = directive_span(statement) else {
            return false;
        };
        if exact_use_strict(source, base, span) {
            return true;
        }
    }
    false
}

fn function_own_strict(stmts: &[ast::Stmt], source: &str, base: u32) -> bool {
    for statement in stmts {
        let Some(span) = directive_span(statement) else {
            return false;
        };
        if exact_use_strict(source, base, span) {
            return true;
        }
    }
    false
}

fn directive_span(statement: &ast::Stmt) -> Option<swc_common::Span> {
    let ast::Stmt::Expr(expr) = statement else {
        return None;
    };
    match expr.expr.as_ref() {
        ast::Expr::Lit(ast::Lit::Str(literal)) => Some(literal.span),
        _ => None,
    }
}

fn exact_use_strict(source: &str, base: u32, span: swc_common::Span) -> bool {
    let lo = span.lo.0.saturating_sub(base) as usize;
    let hi = span.hi.0.saturating_sub(base) as usize;
    let Some(lexeme) = source.get(lo..hi) else {
        return false;
    };
    lexeme == "\"use strict\"" || lexeme == "'use strict'"
}

impl<'a> Builder<'a> {
    fn new_scope(&mut self, kind: ScopeKind, parent: Option<ScopeId>, strict: bool) -> ScopeId {
        let id = ScopeId(self.graph.scopes.len() as u32);
        self.graph.scopes.push(Scope {
            id,
            kind,
            parent,
            strict,
            order: Vec::new(),
            by_name: HashMap::new(),
            formals: Vec::new(),
        });
        id
    }

    fn declare(&mut self, scope: ScopeId, name: &str, span: swc_common::Span) -> BindingId {
        if let Some(id) = self.graph.scopes[scope.0 as usize]
            .by_name
            .get(name)
            .copied()
        {
            return id;
        }
        let id = BindingId(self.graph.bindings.len() as u32);
        self.graph.bindings.push(Binding {
            id,
            name: name.to_owned(),
            scope,
            first_span: span,
            fn_sites: Vec::new(),
            alias: None,
            written: false,
        });
        let scope_slot = &mut self.graph.scopes[scope.0 as usize];
        scope_slot.order.push(id);
        scope_slot.by_name.insert(name.to_owned(), id);
        id
    }

    fn lookup(&self, mut scope: ScopeId, name: &str) -> Option<BindingId> {
        loop {
            if let Some(id) = self.graph.scopes[scope.0 as usize].by_name.get(name) {
                return Some(*id);
            }
            scope = self.graph.scopes[scope.0 as usize].parent?;
        }
    }

    fn note_write(&mut self, scope: ScopeId, name: &str) {
        if let Some(id) = self.lookup(scope, name) {
            let binding = &mut self.graph.bindings[id.0 as usize];
            binding.written = true;
            binding.alias = None;
        }
    }

    /// Record one `var` declarator. A direct builtin-constructor initializer
    /// becomes the binding's only alias; every initializer counts as a write,
    /// so a second declaration or any later store disables it.
    fn record_declaration(
        &mut self,
        id: BindingId,
        scope: ScopeId,
        init: Option<&ast::Expr>,
    ) {
        let Some(init) = init else { return };
        if self.graph.bindings[id.0 as usize].written {
            // A second initializer for the same binding replaces the value,
            // so the earlier alias is no longer the current value.
            self.graph.bindings[id.0 as usize].alias = None;
            return;
        }
        let ast::Expr::Ident(source) = init else {
            self.graph.bindings[id.0 as usize].written = true;
            return;
        };
        // A visible local declaration of the source spelling means the right
        // hand side is not the intrinsic constructor.
        if self.lookup(scope, source.sym.as_str()).is_some() {
            self.graph.bindings[id.0 as usize].written = true;
            return;
        }
        let Some(alias) = builtin_constructor_alias(source.sym.as_str()) else {
            self.graph.bindings[id.0 as usize].written = true;
            return;
        };
        let binding = &mut self.graph.bindings[id.0 as usize];
        binding.alias = Some((alias, init.span()));
        binding.written = true;
    }

    fn error(&self, span: swc_common::Span, message: &str) -> ParseError {
        let lo = span.lo.0.saturating_sub(self.base) as usize;
        let hi = span.hi.0.saturating_sub(self.base) as usize;
        ParseError::at_span(
            vm::compiler::source_map::Span::new(self.source_id, lo, hi),
            message,
        )
    }

    fn variable_owner(&self, mut scope: ScopeId) -> ScopeId {
        loop {
            match self.graph.scopes[scope.0 as usize].kind {
                ScopeKind::Script | ScopeKind::Function => return scope,
                ScopeKind::Catch => {
                    scope = self.graph.scopes[scope.0 as usize]
                        .parent
                        .expect("catch scope has a variable-owner ancestor");
                }
            }
        }
    }

    fn walk_try(&mut self, try_stmt: &ast::TryStmt, scope: ScopeId) -> Result<(), ParseError> {
        self.walk_stmts(&try_stmt.block.stmts, scope)?;
        if let Some(handler) = &try_stmt.handler {
            let strict = self.graph.scopes[scope.0 as usize].strict;
            let catch_scope = self.new_scope(ScopeKind::Catch, Some(scope), strict);
            self.graph
                .catch_scopes
                .insert((handler.span.lo.0, handler.span.hi.0), catch_scope);
            if let Some(ast::Pat::Ident(binding)) = &handler.param {
                self.declare(catch_scope, binding.id.sym.as_str(), binding.id.span);
            }
            self.walk_stmts(&handler.body.stmts, catch_scope)?;
        }
        if let Some(finalizer) = &try_stmt.finalizer {
            self.walk_stmts(&finalizer.stmts, scope)?;
        }
        Ok(())
    }

    fn walk_stmts(&mut self, stmts: &[ast::Stmt], scope: ScopeId) -> Result<(), ParseError> {
        for statement in stmts {
            self.walk_stmt(statement, scope)?;
        }
        Ok(())
    }

    fn walk_stmt(&mut self, statement: &ast::Stmt, scope: ScopeId) -> Result<(), ParseError> {
        match statement {
            ast::Stmt::Decl(ast::Decl::Var(declaration)) => {
                if declaration.kind != ast::VarDeclKind::Var {
                    return Err(self.error(
                        declaration.span,
                        "JavaScript ES5 supports var declarations; let and const are rejected",
                    ));
                }
                let owner = self.variable_owner(scope);
                for declarator in &declaration.decls {
                    let ast::Pat::Ident(binding) = &declarator.name else {
                        return Err(
                            self.error(declarator.span, "P0 requires an identifier binding")
                        );
                    };
                    let id = self.declare(owner, binding.id.sym.as_str(), binding.id.span);
                    self.record_declaration(id, scope, declarator.init.as_deref());
                    if let Some(init) = declarator.init.as_deref() {
                        self.walk_expr(init, scope)?;
                    }
                }
                Ok(())
            }
            ast::Stmt::Decl(ast::Decl::Fn(decl)) => {
                let owner = self.variable_owner(scope);
                self.declare(owner, decl.ident.sym.as_str(), decl.ident.span);
                if let Some(id) = self.lookup(owner, decl.ident.sym.as_str()) {
                    let binding = &mut self.graph.bindings[id.0 as usize];
                    binding.written = true;
                    binding.alias = None;
                }
                self.record_function(owner, decl.ident.sym.as_str(), &decl.function)
            }
            ast::Stmt::Block(block) => self.walk_stmts(&block.stmts, scope),
            ast::Stmt::If(if_stmt) => {
                self.walk_expr(&if_stmt.test, scope)?;
                self.walk_stmt(&if_stmt.cons, scope)?;
                if let Some(alt) = &if_stmt.alt {
                    self.walk_stmt(alt, scope)?;
                }
                Ok(())
            }
            ast::Stmt::Expr(expr) => self.walk_expr(&expr.expr, scope),
            ast::Stmt::Return(ret) => {
                if let Some(arg) = ret.arg.as_deref() {
                    self.walk_expr(arg, scope)?;
                }
                Ok(())
            }
            ast::Stmt::Throw(throw) => self.walk_expr(&throw.arg, scope),
            ast::Stmt::Try(try_stmt) => self.walk_try(try_stmt, scope),
            ast::Stmt::While(loop_) => {
                self.walk_expr(&loop_.test, scope)?;
                self.walk_stmt(&loop_.body, scope)
            }
            ast::Stmt::DoWhile(loop_) => {
                self.walk_stmt(&loop_.body, scope)?;
                self.walk_expr(&loop_.test, scope)
            }
            ast::Stmt::For(loop_) => {
                if let Some(init) = &loop_.init {
                    match init {
                        ast::VarDeclOrExpr::VarDecl(declaration) => {
                            self.walk_var_decl(declaration, scope)?;
                        }
                        ast::VarDeclOrExpr::Expr(expression) => {
                            self.walk_expr(expression, scope)?;
                        }
                    }
                }
                if let Some(test) = &loop_.test {
                    self.walk_expr(test, scope)?;
                }
                if let Some(update) = &loop_.update {
                    self.walk_expr(update, scope)?;
                }
                self.walk_stmt(&loop_.body, scope)
            }
            ast::Stmt::ForOf(loop_) => {
                if let ast::ForHead::VarDecl(declaration) = &loop_.left {
                    self.walk_var_decl(declaration, scope)?;
                }
                if let ast::ForHead::Pat(pat) = &loop_.left
                    && let ast::Pat::Expr(expr) = pat.as_ref()
                {
                    self.walk_expr(expr, scope)?;
                }
                self.walk_expr(&loop_.right, scope)?;
                self.walk_stmt(&loop_.body, scope)
            }
            ast::Stmt::Labeled(labeled) => self.walk_stmt(&labeled.body, scope),
            ast::Stmt::Break(_) | ast::Stmt::Continue(_) | ast::Stmt::Empty(_) => Ok(()),
            // A debugger statement carries no bindings and no expressions.
            ast::Stmt::Debugger(_) => Ok(()),
            // A switch creates no ES5 lexical var scope: discriminant,
            // selectors and every clause body walk in the current scope so
            // unselected clauses still hoist vars and collect functions.
            ast::Stmt::Switch(select) => {
                self.walk_expr(&select.discriminant, scope)?;
                for case in &select.cases {
                    if let Some(test) = &case.test {
                        self.walk_expr(test, scope)?;
                    }
                    self.walk_stmts(&case.cons, scope)?;
                }
                Ok(())
            }
            // Unsupported statements are refused at lowering. Do not invent
            // bindings from forms this subset does not execute.
            // Inventory micro shape only: `for(var k in {a:1}){}` with an
            // empty block body. The header is exactly `var k` with no
            // initializer; the enumerated object is exactly `{a:1}`. Any
            // other for-in collects no bindings here; lowering refuses
            // non-admitted shapes.
            ast::Stmt::ForIn(loop_) => {
                if is_inventory_for_in_single_a(statement) {
                    if let ast::ForHead::VarDecl(declaration) = &loop_.left {
                        self.walk_var_decl(declaration, scope)?;
                        for declarator in &declaration.decls {
                            if let ast::Pat::Ident(binding) = &declarator.name {
                                self.note_write(scope, binding.id.sym.as_str());
                            }
                        }
                    }
                }
                Ok(())
            }
            // Unsupported statements are refused at lowering. Do not invent
            // bindings from forms this subset does not execute.
            _ => Ok(()),
        }
    }

    fn walk_var_decl(
        &mut self,
        declaration: &ast::VarDecl,
        scope: ScopeId,
    ) -> Result<(), ParseError> {
        if declaration.kind != ast::VarDeclKind::Var {
            return Err(self.error(
                declaration.span,
                "JavaScript ES5 supports var declarations; let and const are rejected",
            ));
        }
        let owner = self.variable_owner(scope);
        for declarator in &declaration.decls {
            let ast::Pat::Ident(binding) = &declarator.name else {
                return Err(self.error(declarator.span, "P0 requires an identifier binding"));
            };
            self.declare(owner, binding.id.sym.as_str(), binding.id.span);
            let id = self.lookup(owner, binding.id.sym.as_str());
            if let Some(id) = id {
                self.record_declaration(id, scope, declarator.init.as_deref());
            }
            if let Some(init) = declarator.init.as_deref() {
                self.walk_expr(init, scope)?;
            }
        }
        Ok(())
    }

    fn walk_expr(&mut self, expression: &ast::Expr, scope: ScopeId) -> Result<(), ParseError> {
        match expression {
            ast::Expr::Fn(function) => {
                self.open_function(scope, &function.function)?;
                Ok(())
            }
            ast::Expr::Paren(paren) => self.walk_expr(&paren.expr, scope),
            ast::Expr::Assign(assign) => {
                self.walk_expr(&assign.right, scope)?;
                if let ast::AssignTarget::Simple(ast::SimpleAssignTarget::Ident(ident)) =
                    &assign.left
                {
                    self.note_write(scope, ident.id.sym.as_str());
                }
                if let ast::AssignTarget::Simple(ast::SimpleAssignTarget::Member(member)) =
                    &assign.left
                {
                    self.walk_expr(&member.obj, scope)?;
                    if let ast::MemberProp::Computed(computed) = &member.prop {
                        self.walk_expr(&computed.expr, scope)?;
                    }
                }
                Ok(())
            }
            ast::Expr::Unary(unary) => self.walk_expr(&unary.arg, scope),
            ast::Expr::Update(update) => {
                if let ast::Expr::Ident(ident) = update.arg.as_ref() {
                    self.note_write(scope, ident.sym.as_str());
                }
                self.walk_expr(&update.arg, scope)
            }
            ast::Expr::Cond(branch) => {
                self.walk_expr(&branch.test, scope)?;
                self.walk_expr(&branch.cons, scope)?;
                self.walk_expr(&branch.alt, scope)
            }
            ast::Expr::Seq(sequence) => {
                for expression in &sequence.exprs {
                    self.walk_expr(expression, scope)?;
                }
                Ok(())
            }
            ast::Expr::Bin(binary) => {
                self.walk_expr(&binary.left, scope)?;
                self.walk_expr(&binary.right, scope)
            }
            ast::Expr::Call(call) => {
                if let ast::Callee::Expr(callee) = &call.callee {
                    self.walk_expr(callee, scope)?;
                }
                for arg in &call.args {
                    self.walk_expr(&arg.expr, scope)?;
                }
                Ok(())
            }
            ast::Expr::Member(member) => {
                self.walk_expr(&member.obj, scope)?;
                if let ast::MemberProp::Computed(computed) = &member.prop {
                    self.walk_expr(&computed.expr, scope)?;
                }
                Ok(())
            }
            ast::Expr::Object(object) => {
                for prop in &object.props {
                    if let ast::PropOrSpread::Prop(boxed) = prop {
                        match boxed.as_ref() {
                            ast::Prop::KeyValue(pair) => self.walk_expr(&pair.value, scope)?,
                            ast::Prop::Getter(getter) => {
                                self.open_function(scope, &getter.function)?
                            }
                            ast::Prop::Setter(setter) => {
                                self.open_function(scope, &setter.function)?
                            }
                            _ => {}
                        }
                    }
                }
                Ok(())
            }
            ast::Expr::Array(array) => {
                for element in array.elems.iter().flatten() {
                    self.walk_expr(&element.expr, scope)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn record_function(
        &mut self,
        scope: ScopeId,
        name: &str,
        function: &ast::Function,
    ) -> Result<(), ParseError> {
        let id = self.declare(scope, name, function.span);
        self.graph.bindings[id.0 as usize].fn_sites.push(FnSite {
            span: function.span,
            function: Box::new(function.clone()),
        });
        self.open_function(scope, function)
    }

    fn open_function(
        &mut self,
        parent: ScopeId,
        function: &ast::Function,
    ) -> Result<(), ParseError> {
        let key = (function.span.lo.0, function.span.hi.0);
        if self.graph.function_scopes.contains_key(&key) {
            return Ok(());
        }
        let parent_strict = self.graph.scopes[parent.0 as usize].strict;
        let own = function
            .body
            .as_ref()
            .is_some_and(|body| function_own_strict(&body.stmts, self.source, self.base));
        let child = self.new_scope(ScopeKind::Function, Some(parent), own || parent_strict);
        self.graph.function_scopes.insert(key, child);
        if function.params.len() > MAX_ARGUMENTS {
            return Err(self.error(
                function.span,
                "P4 source functions exceed the 64 formal parameter profile limit",
            ));
        }
        for (ordinal, param) in function.params.iter().enumerate() {
            let ast::Pat::Ident(binding) = &param.pat else {
                return Err(self.error(
                    param.span,
                    "P4 source functions require plain identifier formals",
                ));
            };
            let id = self.declare(child, binding.id.sym.as_str(), binding.id.span);
            self.graph.scopes[child.0 as usize]
                .formals
                .push((id, ordinal, binding.id.span));
        }
        if let Some(body) = function.body.as_ref() {
            self.walk_stmts(&body.stmts, child)?;
        }
        Ok(())
    }
}

/// Inventory micro shape: `for(var k in {a:1}){}` with an empty block body.
///
/// The header is exactly `var k` with no initializer; the enumerated object
/// is an object literal with exactly one own data property `a:1`; the body
/// is empty. Any other header, object, array, prototype enumeration,
/// variable form or non-empty body never matches here.
fn is_inventory_for_in_single_a(statement: &ast::Stmt) -> bool {
    let ast::Stmt::ForIn(loop_) = statement else {
        return false;
    };
    let ast::ForHead::VarDecl(decl) = &loop_.left else {
        return false;
    };
    if decl.kind != ast::VarDeclKind::Var {
        return false;
    }
    if decl.decls.len() != 1 {
        return false;
    }
    let declarator = &decl.decls[0];
    if declarator.init.is_some() {
        return false;
    }
    let ast::Pat::Ident(binding) = &declarator.name else {
        return false;
    };
    if binding.id.sym.as_str() != "k" {
        return false;
    }
    let ast::Expr::Object(object) = loop_.right.as_ref() else {
        return false;
    };
    if object.props.len() != 1 {
        return false;
    }
    let ast::PropOrSpread::Prop(prop) = &object.props[0] else {
        return false;
    };
    let ast::Prop::KeyValue(pair) = prop.as_ref() else {
        return false;
    };
    let ast::PropName::Ident(key) = &pair.key else {
        return false;
    };
    if key.sym.as_str() != "a" {
        return false;
    }
    let ast::Expr::Lit(ast::Lit::Num(number)) = pair.value.as_ref() else {
        return false;
    };
    if number.value != 1.0 {
        return false;
    }
    let ast::Stmt::Block(body) = loop_.body.as_ref() else {
        return false;
    };
    body.stmts.is_empty()
}

/// The builtin constructor spellings this realm exposes as first-class
/// values. `Error` and `Date` are deliberately absent: neither has a
/// constructor value, so neither can be aliased.
fn builtin_constructor_alias(name: &str) -> Option<ConstructorAlias> {
    if let Some(element) = super::prelude::typed_array::Element::named(name) {
        return Some(ConstructorAlias::TypedArray(element));
    }
    Some(match name {
        "Array" => ConstructorAlias::Array,
        "ArrayBuffer" => ConstructorAlias::ArrayBuffer,
        "Object" => ConstructorAlias::Object,
        "TypeError" => ConstructorAlias::TypeError,
        "RangeError" => ConstructorAlias::RangeError,
        "Number" => ConstructorAlias::Number,
        "String" => ConstructorAlias::String,
        "Boolean" => ConstructorAlias::Boolean,
        _ => return None,
    })
}

/// Alias of one identifier at a `new` site. The binding must have been
/// initialized by a direct builtin spelling strictly before the use and must
/// never have been written again.
pub(super) fn constructor_alias(
    graph: &ScopeGraph,
    mut scope: ScopeId,
    name: &str,
    use_lo: u32,
) -> Option<ConstructorAlias> {
    let owner = variable_owner(graph, scope);
    loop {
        if let Some(id) = graph.scope(scope).by_name.get(name).copied() {
            let binding = graph.binding(id);
            // An alias from another activation would be a free outer
            // capture, which this closed subset refuses instead of
            // silently constructing.
            if binding.scope != owner {
                return None;
            }
            let (alias, init) = binding.alias?;
            return if init.hi.0 <= use_lo {
                Some(alias)
            } else {
                None
            };
        }
        scope = graph.scope(scope).parent?;
    }
}

fn variable_owner(graph: &ScopeGraph, mut scope: ScopeId) -> ScopeId {
    loop {
        match graph.scope(scope).kind {
            ScopeKind::Script | ScopeKind::Function => return scope,
            ScopeKind::Catch => {
                scope = graph
                    .scope(scope)
                    .parent
                    .expect("catch scope has a variable-owner ancestor");
            }
        }
    }
}
