//! Canonical owners and bounded initializedness for the ten writable names.
use super::scope::{Binding, BindingId, ScopeGraph, ScopeId};
use swc_common::Span;
use swc_ecma_ast as a;

pub(super) const NAMES: [&str; 10] = [
    "Object",
    "Math",
    "parseInt",
    "parseFloat",
    "isNaN",
    "isFinite",
    "encodeURIComponent",
    "decodeURIComponent",
    "encodeURI",
    "decodeURI",
];
pub(super) fn index(name: &str) -> Option<u8> {
    NAMES.iter().position(|n| *n == name).map(|i| i as u8)
}
#[derive(Default)]
pub(super) struct Plan {
    pub mutated: u16,
    pub initial: u16,
    pub shared: bool,
}
impl Plan {
    pub fn contains(&self, i: u8) -> bool {
        self.mutated & (1 << i) != 0
    }
    pub fn entry(&self, i: u8) -> i64 {
        (self.mutated & ((1 << i) - 1)).count_ones() as i64
    }
    pub fn count(&self) -> i64 {
        self.mutated.count_ones() as i64
    }
}
fn canonical(g: &ScopeGraph, mut scope: ScopeId, name: &str) -> Option<u8> {
    let i = index(name)?;
    loop {
        if g.scope(scope).by_name.contains_key(name) {
            return (scope == g.script).then_some(i);
        }
        let Some(parent) = g.scope(scope).parent else {
            return Some(i);
        };
        scope = parent;
    }
}
struct Scan<'a> {
    g: &'a ScopeGraph,
    plan: Plan,
    deferred_access: u16,
    first: [Option<Span>; NAMES.len()],
    defined: u16,
}
impl Scan<'_> {
    fn access(&mut self, scope: ScopeId, name: &str, write: bool, span: Span, proof: bool) {
        if let Some(i) = canonical(self.g, scope, name) {
            let bit = 1 << i;
            if scope != self.g.script {
                self.deferred_access |= bit;
            }
            if write {
                self.plan.mutated |= bit;
                self.first[i as usize].get_or_insert(span);
                if proof && scope == self.g.script {
                    self.defined |= bit;
                }
            } else if scope != self.g.script || self.defined & bit == 0 {
                self.plan.initial |= bit;
            }
        }
    }
    fn function(&mut self, f: &a::Function) {
        if let Some(&s) = self.g.function_scopes.get(&(f.span.lo.0, f.span.hi.0))
            && let Some(b) = &f.body
        {
            self.list(&b.stmts, s, false);
        }
    }
    fn list(&mut self, ss: &[a::Stmt], s: ScopeId, proof: bool) {
        for st in ss {
            self.stmt(st, s, proof);
        }
    }
    fn var(&mut self, d: &a::VarDecl, s: ScopeId, proof: bool) {
        for v in &d.decls {
            if let Some(e) = &v.init {
                self.expr(e, s, proof);
                if let a::Pat::Ident(n) = &v.name {
                    self.access(s, n.id.sym.as_str(), true, n.span, proof);
                }
            }
        }
    }
    fn stmt(&mut self, st: &a::Stmt, s: ScopeId, proof: bool) {
        match st {
            a::Stmt::Decl(a::Decl::Var(d)) => self.var(d, s, proof),
            a::Stmt::Decl(a::Decl::Fn(d)) => self.function(&d.function),
            a::Stmt::Expr(e) => self.expr(&e.expr, s, proof),
            a::Stmt::Block(b) => self.list(&b.stmts, s, proof),
            a::Stmt::Return(r) => {
                if let Some(e) = &r.arg {
                    self.expr(e, s, false);
                }
            }
            a::Stmt::Throw(t) => self.expr(&t.arg, s, false),
            a::Stmt::If(b) => {
                self.expr(&b.test, s, proof);
                let before = self.defined;
                self.stmt(&b.cons, s, false);
                self.defined = before;
                if let Some(e) = &b.alt {
                    self.stmt(e, s, false);
                }
                self.defined = before;
            }
            a::Stmt::While(l) => {
                self.expr(&l.test, s, false);
                self.stmt(&l.body, s, false);
            }
            a::Stmt::DoWhile(l) => {
                self.stmt(&l.body, s, false);
                self.expr(&l.test, s, false);
            }
            a::Stmt::For(l) => {
                if let Some(i) = &l.init {
                    match i {
                        a::VarDeclOrExpr::VarDecl(d) => self.var(d, s, false),
                        a::VarDeclOrExpr::Expr(e) => self.expr(e, s, false),
                    }
                }
                if let Some(e) = &l.test {
                    self.expr(e, s, false);
                }
                if let Some(e) = &l.update {
                    self.expr(e, s, false);
                }
                self.stmt(&l.body, s, false);
            }
            a::Stmt::ForOf(l) => {
                match &l.left {
                    a::ForHead::VarDecl(d) => {
                        for v in &d.decls {
                            if let a::Pat::Ident(n) = &v.name {
                                self.access(s, n.id.sym.as_str(), true, n.span, false);
                            }
                        }
                    }
                    a::ForHead::Pat(p) => match p.as_ref() {
                        a::Pat::Ident(n) => {
                            self.access(s, n.id.sym.as_str(), true, n.span, false);
                        }
                        a::Pat::Expr(e) => {
                            if let a::Expr::Member(m) = e.as_ref() {
                                self.member(m, s, false);
                            } else {
                                self.expr(e, s, false);
                            }
                        }
                        _ => {}
                    },
                    _ => {}
                }
                self.expr(&l.right, s, false);
                self.stmt(&l.body, s, false);
            }
            a::Stmt::Labeled(l) => self.stmt(&l.body, s, false),
            a::Stmt::Switch(v) => {
                self.expr(&v.discriminant, s, false);
                for c in &v.cases {
                    if let Some(e) = &c.test {
                        self.expr(e, s, false);
                    }
                    self.list(&c.cons, s, false);
                }
            }
            a::Stmt::Try(t) => {
                let before = self.defined;
                self.list(&t.block.stmts, s, false);
                self.defined = before;
                if let Some(h) = &t.handler {
                    let hs = self
                        .g
                        .catch_scopes
                        .get(&(h.span.lo.0, h.span.hi.0))
                        .copied()
                        .unwrap_or(s);
                    self.list(&h.body.stmts, hs, false);
                }
                self.defined = before;
                if let Some(f) = &t.finalizer {
                    self.list(&f.stmts, s, false);
                }
                self.defined = before;
            }
            _ => {}
        }
    }
    fn expr(&mut self, e: &a::Expr, s: ScopeId, proof: bool) {
        match e {
            a::Expr::Ident(n) => self.access(s, n.sym.as_str(), false, n.span, proof),
            a::Expr::Paren(p) => self.expr(&p.expr, s, proof),
            a::Expr::Assign(x) => {
                if let a::AssignTarget::Simple(a::SimpleAssignTarget::Member(m)) = &x.left {
                    self.member(m, s, proof);
                }
                self.expr(&x.right, s, proof);
                if let a::AssignTarget::Simple(a::SimpleAssignTarget::Ident(n)) = &x.left {
                    self.access(s, n.id.sym.as_str(), true, n.span, proof);
                }
            }
            a::Expr::Fn(f) => self.function(&f.function),
            a::Expr::Member(m) => self.member(m, s, proof),
            a::Expr::Call(c) => {
                if let a::Callee::Expr(e) = &c.callee {
                    self.expr(e, s, proof);
                }
                for arg in &c.args {
                    self.expr(&arg.expr, s, proof);
                }
            }
            a::Expr::Unary(u) => self.expr(&u.arg, s, proof),
            a::Expr::Update(u) => self.expr(&u.arg, s, false),
            a::Expr::Bin(b) => {
                self.expr(&b.left, s, proof);
                self.expr(
                    &b.right,
                    s,
                    proof && !matches!(b.op, a::BinaryOp::LogicalAnd | a::BinaryOp::LogicalOr),
                );
            }
            a::Expr::Seq(v) => {
                for e in &v.exprs {
                    self.expr(e, s, proof);
                }
            }
            a::Expr::Cond(c) => {
                self.expr(&c.test, s, proof);
                let before = self.defined;
                self.expr(&c.cons, s, false);
                self.defined = before;
                self.expr(&c.alt, s, false);
                self.defined = before;
            }
            a::Expr::Array(v) => {
                for e in v.elems.iter().flatten() {
                    self.expr(&e.expr, s, proof);
                }
            }
            a::Expr::Object(v) => {
                for p in &v.props {
                    if let a::PropOrSpread::Prop(p) = p {
                        match p.as_ref() {
                            a::Prop::KeyValue(p) => {
                                if let a::PropName::Computed(k) = &p.key {
                                    self.expr(&k.expr, s, proof);
                                }
                                self.expr(&p.value, s, proof);
                            }
                            a::Prop::Getter(p) => self.function(&p.function),
                            a::Prop::Setter(p) => self.function(&p.function),
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }
    fn member(&mut self, m: &a::MemberExpr, s: ScopeId, proof: bool) {
        self.expr(&m.obj, s, proof);
        if let a::MemberProp::Computed(k) = &m.prop {
            self.expr(&k.expr, s, proof);
        }
    }
}
/// Add compiler binding identities only for newly writable fallback names.
/// Scope ownership/shadow rules remain the existing graph's rules.
pub(super) fn prepare(g: &mut ScopeGraph, script: &a::Script) -> Plan {
    let mut scan = Scan {
        g,
        plan: Plan::default(),
        deferred_access: 0,
        first: [None; NAMES.len()],
        defined: 0,
    };
    for id in &g.scope(g.script).order {
        let b = g.binding(*id);
        if !b.fn_sites.is_empty() && index(&b.name).is_some() {
            scan.access(g.script, &b.name, true, b.first_span, true);
        }
    }
    scan.list(&script.body, g.script, true);
    scan.plan.shared = scan.plan.mutated & scan.deferred_access != 0;
    let Scan { plan, first, .. } = scan;
    for (i, span) in first.into_iter().enumerate() {
        if let Some(span) = span {
            let name = NAMES[i];
            if !g.scope(g.script).by_name.contains_key(name) {
                let id = BindingId(g.bindings.len() as u32);
                g.bindings.push(Binding {
                    id,
                    name: name.into(),
                    scope: g.script,
                    first_span: span,
                    fn_sites: Vec::new(),
                    alias: None,
                    written: true,
                });
                g.scopes[g.script.0 as usize]
                    .by_name
                    .insert(name.into(), id);
                g.scopes[g.script.0 as usize].order.push(id);
            }
        }
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_uri123_upper_owner_masks() {
        assert_eq!(
            NAMES,
            [
                "Object",
                "Math",
                "parseInt",
                "parseFloat",
                "isNaN",
                "isFinite",
                "encodeURIComponent",
                "decodeURIComponent",
                "encodeURI",
                "decodeURI"
            ]
        );
        for (source, mutated, initial, shared) in [
            ("encodeURI=1;encodeURI;", 1u16 << 8, 0u16, false),
            ("decodeURI=1;decodeURI;", 1 << 9, 0, false),
            ("encodeURI;encodeURI=1;", 1 << 8, 1 << 8, false),
            ("decodeURI;decodeURI=1;", 1 << 9, 1 << 9, false),
            (
                "function f(){return encodeURI;}encodeURI=1;",
                1 << 8,
                1 << 8,
                true,
            ),
            (
                "function f(){return decodeURI;}decodeURI=1;",
                1 << 9,
                1 << 9,
                true,
            ),
            (
                "function f(){return encodeURI+decodeURI;}encodeURI=1;decodeURI=2;",
                0x300,
                0x300,
                true,
            ),
            (
                "Object;Math;encodeURIComponent;decodeURIComponent;encodeURI;decodeURI;Object=1;Math=2;encodeURIComponent=3;decodeURIComponent=4;encodeURI=5;decodeURI=6;",
                0x3c3,
                0x3c3,
                false,
            ),
            (
                "function f(encodeURI,decodeURI){encodeURI=1;decodeURI=2;return encodeURI;}",
                0,
                0,
                false,
            ),
        ] {
            let (script, base) = crate::javascript::parser::parse_script(source, 0).unwrap();
            let mut graph = crate::javascript::scope::build(&script, source, 0, base).unwrap();
            let plan = prepare(&mut graph, &script);
            assert_eq!(
                (plan.mutated, plan.initial & plan.mutated, plan.shared),
                (mutated, initial, shared),
                "{source}"
            );
            let mut entry = 0;
            for i in 0..NAMES.len() as u8 {
                assert_eq!(plan.contains(i), mutated & (1u16 << i) != 0);
                if plan.contains(i) {
                    assert_eq!(plan.entry(i), entry);
                    entry += 1;
                }
            }
            assert_eq!(plan.count(), entry);
            eprintln!(
                "URI123_OWNER source={source:?} mutated={mutated} initial={initial} shared={shared} count={entry}"
            );
        }
    }
}
