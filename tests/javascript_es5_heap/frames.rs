//! Root-only history uses the actual compiled strict function returning7.
//! No node allocation in its body can mask a lost frame by triggering GC.
use super::*;
struct PrimitiveHistoryPlugin {
    iterations: i64,
}
impl vm::SourcePlugin for PrimitiveHistoryPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-primitive-history"),
            extensions: &["js"],
            typing_policy: vm::FrontendTypingPolicy::Strict,
            supports_ownership_checking: true,
        }
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        let mut ctx = heap_cycle_ctx(&ir)?;
        ctx.carrier = heap_cycle_carrier(&ir)?;
        let binding = heap_binding_index(&ir, "o")?;
        // The call block is flattened into ANF Lets plus the hoisted store.
        // Replay those Lets, not the predeclare Let of undefined.
        let at = ir
            .stmts
            .iter()
            .position(|s| matches!(s, vm::Stmt::Assign { index, .. } if *index == binding))
            .ok_or_else(|| heap_rejection("actual primitive result".to_owned()))?;
        let vm::Stmt::Assign { expr: stored, .. } = &ir.stmts[at] else {
            unreachable!("position matched Assign")
        };
        let stored = stored.clone();
        let (run_start, run_end) = heap_call_replay_bounds(&ir, at)?;
        let mut replay = Vec::new();
        for stmt in &ir.stmts[run_start..at] {
            let vm::Stmt::Let {
                index, expr, line, ..
            } = stmt
            else {
                return Err(heap_rejection("real dispatch ANF".to_owned()));
            };
            replay.push(vm::Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: *index,
                expr: expr.clone(),
                line: *line,
            });
        }
        replay.push(vm::Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: binding,
            expr: stored,
            line: 1,
        });
        for stmt in &ir.stmts[at + 1..run_end] {
            let vm::Stmt::Let {
                index, expr, line, ..
            } = stmt
            else {
                return Err(heap_rejection("actual handoff/cleanup ack".to_owned()));
            };
            replay.push(vm::Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: *index,
                expr: expr.clone(),
                line: *line,
            });
        }
        let counter = heap_cycle_take(&mut ir, "primitive-history")?;
        ir.stmts.push(vm::Stmt::Let {
            index: counter,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: vm::Expr::Int(0),
            line: 1,
        });
        replay.push(vm::Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: counter,
            expr: vm::Expr::Add(Box::new(vm::Expr::Var(counter)), Box::new(vm::Expr::Int(1))),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::While {
            condition: vm::Expr::Lt(
                Box::new(vm::Expr::Var(counter)),
                Box::new(vm::Expr::Int(self.iterations)),
            ),
            body: replay,
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(binding),
            line: 1,
        });
        for which in [0, 2, 3, 4, 5] {
            heap_cycle_observe(&mut ir, &ctx, ctx.carrier, which, 0, "primitive-history")?;
        }
        Ok(ir)
    }
}
#[test]
fn p2_primitive_return_history_reuses_frames_without_any_node_gc() {
    let mut warm = None;
    for iterations in [64, 4096] {
        let plugin = Box::leak(Box::new(PrimitiveHistoryPlugin { iterations }));
        let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
            "var f=function(){\"use strict\";return 7;};var o=f();",
            vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-primitive-history")),
            vm::CompileSourceFileOptions::new()
                .with_source_plugin(plugin)
                .with_host_api_catalog(heap_catalog()),
        )
        .expect("primitive history source");
        let mut machine = compilation.into_vm().expect("certified history VM");
        pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
            &mut machine,
            pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
        )
        .expect("B1");
        transaction_registry()
            .bind_vm_cached(&mut machine)
            .expect("bind");
        machine.set_fuel(100_000_000);
        assert_eq!(
            machine.run().expect("primitive history must halt"),
            VmStatus::Halted
        );
        let stack = machine.stack();
        let tail = &stack[stack.len() - 6..];
        assert_eq!(heap_box_number(&tail[0]), 7.0);
        let counters = heap_stale_ints(&tail[1..]);
        assert_eq!((counters[0], counters[1], counters[4]), (1, 1, 0));
        eprintln!("P2_PRIMITIVE_HISTORY iterations={iterations} counters={counters:?}");
        // After the first warmed source call, every frame is immediately
        // available for the next equal-size activation. Differential bound0.
        if let Some(expected) = &warm {
            assert_eq!(
                &counters, expected,
                "root-only history cannot rely on future node collection"
            );
        } else {
            warm = Some(counters);
        }
        machine.reset_for_reuse().expect("reset history");
        assert_eq!(machine.host_context().resource_count(), 0);
    }
}
