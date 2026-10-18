//! Test-only reads of real produced IR locals/vectors; never evaluate JS ASTs.
use vm::{BuiltinFunction as B, Expr, FrontendIr, ParseError, SourcePlugin, Stmt, TypeSchema};

#[derive(Default)]
pub struct Observer {
    pub reads: Vec<String>,
    pub string: Option<(String, usize)>,
    pub vector_numbers: Option<Vec<f64>>,
    pub collect: bool,
    pub force_gc: bool,
    pub stats: bool,
}

fn error(message: &str) -> ParseError {
    ParseError::at_span(vm::compiler::source_map::Span::new(0, 0, 0), message)
}
fn call(index: u16, args: Vec<Expr>) -> Expr {
    Expr::Call(index, Vec::new(), args, None, None)
}
fn field(slot: u16, name: &str) -> Expr {
    call(
        B::Get.call_index(),
        vec![Expr::Var(slot), Expr::String(name.into())],
    )
}
fn helper(ir: &FrontendIr, name: &str) -> Result<u16, ParseError> {
    ir.functions
        .iter()
        .find(|f| f.name == name)
        .map(|f| f.index)
        .ok_or_else(|| error(name))
}
fn binding(ir: &FrontendIr, name: &str) -> Result<u16, ParseError> {
    ir.local_bindings
        .iter()
        .rfind(|(n, _)| n == name)
        .map(|(_, s)| *s)
        .ok_or_else(|| error(name))
}
fn fresh(ir: &mut FrontendIr) -> Result<u16, ParseError> {
    let slot = u16::try_from(ir.locals).map_err(|_| error("observer local limit"))?;
    ir.locals += 1;
    Ok(slot)
}
fn carrier(ir: &FrontendIr) -> Result<u16, ParseError> {
    let make = helper(ir, "__compat_ref_make")?;
    ir.stmts
        .iter()
        .find_map(|s| match s {
            Stmt::Let {
                index,
                expr: Expr::Call(target, _, _, _, _),
                ..
            } if *target == make => Some(*index),
            _ => None,
        })
        .ok_or_else(|| error("realm carrier"))
}
fn number(e: Expr) -> Expr {
    Expr::Div(Box::new(e), Box::new(Expr::Float(1.0)))
}
fn assert(e: Expr) -> Stmt {
    Stmt::Expr {
        expr: call(B::Assert.call_index(), vec![e]),
        line: 1,
    }
}
impl SourcePlugin for Observer {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-arity-observe"),
            extensions: &["js"],
            typing_policy: vm::FrontendTypingPolicy::Strict,
            supports_ownership_checking: true,
        }
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<FrontendIr, ParseError> {
        let mut ir = pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        if self.force_gc {
            // Force the existing allocation quantum at entry to the real
            // node_begin helper, matching the maintained safepoint control.
            let begin = helper(&ir, "__compat_es5_node_begin")?;
            let write = helper(&ir, "__compat_ref_write")?;
            let slot = fresh(&mut ir)?;
            let body = ir
                .function_impls
                .get_mut(&begin)
                .ok_or_else(|| error("node begin"))?;
            let c = body.param_slots[0];
            body.body_stmts.insert(
                0,
                Stmt::Let {
                    index: slot,
                    declared_schema: Some(TypeSchema::Number),
                    expr: call(
                        write,
                        vec![Expr::Var(c), Expr::Float(10.0), Expr::Float(32.0)],
                    ),
                    line: 1,
                },
            );
        }
        if let Some(expected) = &self.vector_numbers {
            let index = helper(&ir, "__compat_es5_fn_0")?;
            let args = ir
                .function_impls
                .get(&index)
                .ok_or_else(|| error("source impl"))?
                .param_slots[3];
            let mut checks = vec![assert(Expr::Eq(
                Box::new(call(B::Len.call_index(), vec![Expr::Var(args)])),
                Box::new(Expr::Int(expected.len() as i64)),
            ))];
            for (ordinal, n) in expected.iter().enumerate() {
                let slot = fresh(&mut ir)?;
                checks.push(Stmt::Let {
                    index: slot,
                    declared_schema: Some(TypeSchema::Named(
                        "__CompatEs5Primitive".into(),
                        Vec::new(),
                    )),
                    expr: call(
                        B::Get.call_index(),
                        vec![
                            Expr::Borrow(Box::new(Expr::Var(args))),
                            Expr::Int(ordinal as i64),
                        ],
                    ),
                    line: 1,
                });
                checks.push(assert(Expr::Eq(
                    Box::new(field(slot, "tag")),
                    Box::new(Expr::Int(3)),
                )));
                checks.push(assert(Expr::Eq(
                    Box::new(number(field(slot, "number"))),
                    Box::new(Expr::Float(*n)),
                )));
            }
            let implementation = ir
                .function_impls
                .get_mut(&index)
                .ok_or_else(|| error("source impl"))?;
            let pos = implementation
                .body_stmts
                .len()
                .checked_sub(2)
                .ok_or_else(|| error("source epilogue"))?;
            implementation.body_stmts.splice(pos..pos, checks);
        }
        if self.collect {
            let c = carrier(&ir)?;
            let collect = helper(&ir, "__compat_es5_heap_collect")?;
            let slot = fresh(&mut ir)?;
            ir.stmts.push(Stmt::Let {
                index: slot,
                declared_schema: Some(TypeSchema::Int),
                expr: call(collect, vec![Expr::Var(c)]),
                line: 1,
            });
        }
        for name in &self.reads {
            let slot = binding(&ir, name)?;
            ir.stmts.push(Stmt::Expr {
                expr: Expr::Var(slot),
                line: 1,
            });
        }
        if let Some((name, units)) = &self.string {
            let s = binding(&ir, name)?;
            let c = carrier(&ir)?;
            let checked = helper(&ir, "__compat_es5_checked_base")?;
            let read = helper(&ir, "__compat_ref_read")?;
            let base = fresh(&mut ir)?;
            ir.stmts.push(Stmt::Let {
                index: base,
                declared_schema: Some(TypeSchema::Int),
                expr: call(
                    checked,
                    vec![Expr::Var(c), field(s, "ref_slot"), field(s, "generation")],
                ),
                line: 1,
            });
            for offset in 2..3 + *units {
                let slot = fresh(&mut ir)?;
                ir.stmts.push(Stmt::Let {
                    index: slot,
                    declared_schema: Some(TypeSchema::Number),
                    expr: number(call(
                        read,
                        vec![
                            Expr::Var(c),
                            number(Expr::Add(
                                Box::new(Expr::Var(base)),
                                Box::new(Expr::Int(offset as i64)),
                            )),
                        ],
                    )),
                    line: 1,
                });
                ir.stmts.push(Stmt::Expr {
                    expr: Expr::Var(slot),
                    line: 1,
                });
            }
        }
        if self.stats {
            let c = carrier(&ir)?;
            let observe = helper(&ir, "__compat_es5_heap_observe")?;
            // live nodes, occupied node cells, completed collections.
            for selector in [0, 7, 5] {
                let slot = fresh(&mut ir)?;
                ir.stmts.push(Stmt::Let {
                    index: slot,
                    declared_schema: Some(TypeSchema::Int),
                    expr: call(
                        observe,
                        vec![Expr::Var(c), Expr::Int(selector), Expr::Int(0)],
                    ),
                    line: 1,
                });
                ir.stmts.push(Stmt::Expr {
                    expr: Expr::Var(slot),
                    line: 1,
                });
            }
        }
        Ok(ir)
    }
}
