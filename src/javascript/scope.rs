//! Compiler-owned Script/Function scope graph for the closed declaration subset.
//!
//! One BindingId per declaration-owned name in a scope. Blocks do not create
//! var scopes. Nested functions are separate scopes and are never walked as
//! if their vars belonged to the parent. This is environment erasure for the
//! nonescaping subset, not a runtime EnvironmentRecord.

use std::collections::HashMap;

use swc_ecma_ast as ast;
use vm::ParseError;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) struct ScopeId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[allow(dead_code)]
pub(super) struct BindingId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ScopeKind {
    Script,
    Function,
}

#[derive(Clone)]
pub(super) struct FnSite {
    pub span: swc_common::Span,
    pub function: Box<ast::Function>,
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
}

pub(super) struct ScopeGraph {
    pub scopes: Vec<Scope>,
    pub bindings: Vec<Binding>,
    pub script: ScopeId,
    /// Function AST span `(lo, hi)` to the function's own scope.
    pub function_scopes: HashMap<(u32, u32), ScopeId>,
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
        });
        let scope_slot = &mut self.graph.scopes[scope.0 as usize];
        scope_slot.order.push(id);
        scope_slot.by_name.insert(name.to_owned(), id);
        id
    }

    fn error(&self, span: swc_common::Span, message: &str) -> ParseError {
        let lo = span.lo.0.saturating_sub(self.base) as usize;
        let hi = span.hi.0.saturating_sub(self.base) as usize;
        ParseError::at_span(
            vm::compiler::source_map::Span::new(self.source_id, lo, hi),
            message,
        )
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
                for declarator in &declaration.decls {
                    let ast::Pat::Ident(binding) = &declarator.name else {
                        return Err(
                            self.error(declarator.span, "P0 requires an identifier binding")
                        );
                    };
                    self.declare(scope, binding.id.sym.as_str(), binding.id.span);
                    if let Some(init) = declarator.init.as_deref() {
                        self.walk_expr(init, scope)?;
                    }
                }
                Ok(())
            }
            ast::Stmt::Decl(ast::Decl::Fn(decl)) => {
                self.declare(scope, decl.ident.sym.as_str(), decl.ident.span);
                self.record_function(scope, decl.ident.sym.as_str(), &decl.function)
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
            ast::Stmt::Empty(_) => Ok(()),
            // Unsupported statements are refused at lowering. Do not invent
            // bindings from forms this subset does not execute.
            _ => Ok(()),
        }
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
                if let ast::AssignTarget::Simple(ast::SimpleAssignTarget::Member(member)) =
                    &assign.left
                {
                    self.walk_expr(&member.obj, scope)?;
                }
                Ok(())
            }
            ast::Expr::Unary(unary) => self.walk_expr(&unary.arg, scope),
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
                    if let ast::PropOrSpread::Prop(boxed) = prop
                        && let ast::Prop::KeyValue(pair) = boxed.as_ref()
                    {
                        self.walk_expr(&pair.value, scope)?;
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
        if let Some(body) = function.body.as_ref() {
            self.walk_stmts(&body.stmts, child)?;
        }
        Ok(())
    }
}
