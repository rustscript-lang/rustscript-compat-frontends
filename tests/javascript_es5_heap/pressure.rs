//! Real managed array payloads force physical pressure before slot choice.
use super::*;
fn large_array(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    frame: u16,
    payload: i64,
) -> Result<(u16, u16, u16), vm::ParseError> {
    let allocation = call_allocation(ir, ctx, 5 + payload, 1)?;
    let mut fields = Vec::new();
    for name in ["slot", "generation", "base"] {
        let out = heap_cycle_take(ir, "pressure")?;
        ir.stmts.push(vm::Stmt::Let {
            index: out,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_field_get(ctx.get, allocation, name),
            line: 1,
        });
        fields.push(out);
    }
    for (offset, value) in [(2, payload), (3, 0), (4, 0)] {
        heap_cycle_write(
            ir,
            ctx,
            ctx.carrier,
            vm::Expr::Add(
                Box::new(vm::Expr::Var(fields[2])),
                Box::new(vm::Expr::Int(offset)),
            ),
            vm::Expr::Float(value as f64),
            "pressure",
        )?;
    }
    let counter = heap_cycle_take(ir, "pressure")?;
    let ack = heap_cycle_take(ir, "pressure")?;
    ir.stmts.push(vm::Stmt::Let {
        index: counter,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: vm::Expr::Int(0),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::Let {
        index: ack,
        declared_schema: Some(vm::TypeSchema::Number),
        expr: vm::Expr::Float(0.0),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::While {
        condition: vm::Expr::Lt(
            Box::new(vm::Expr::Var(counter)),
            Box::new(vm::Expr::Int(payload)),
        ),
        body: vec![
            vm::Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: ack,
                expr: heap_guest_call(
                    ctx.write,
                    vec![
                        vm::Expr::Var(ctx.carrier),
                        heap_as_number(vm::Expr::Add(
                            Box::new(vm::Expr::Add(
                                Box::new(vm::Expr::Var(fields[2])),
                                Box::new(vm::Expr::Int(5)),
                            )),
                            Box::new(vm::Expr::Var(counter)),
                        )),
                        vm::Expr::Float(7.0),
                    ],
                ),
                line: 1,
            },
            vm::Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: counter,
                expr: vm::Expr::Add(Box::new(vm::Expr::Var(counter)), Box::new(vm::Expr::Int(1))),
                line: 1,
            },
        ],
        line: 1,
    });
    let publish = heap_helper_index(ir, "__compat_es5_node_publish")?;
    let boxed = heap_cycle_take(ir, "pressure")?;
    ir.stmts.push(vm::Stmt::Let {
        index: boxed,
        declared_schema: Some(vm::TypeSchema::Named(
            "__CompatEs5Primitive".to_owned(),
            Vec::new(),
        )),
        expr: heap_guest_call(
            publish,
            vec![
                vm::Expr::Var(ctx.carrier),
                vm::Expr::Var(fields[0]),
                vm::Expr::Var(fields[1]),
                vm::Expr::Var(fields[2]),
                vm::Expr::Var(frame),
                vm::Expr::Int(0),
            ],
        ),
        line: 1,
    });
    Ok((fields[0], fields[1], fields[2]))
}
struct PressurePlugin;
impl vm::SourcePlugin for PressurePlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-pressure"),
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
        let frame = call_int(
            &mut ir,
            ctx.root_enter,
            vec![vm::Expr::Var(ctx.carrier), vm::Expr::Int(1)],
        )?;
        let (old_slot, old_generation, _) = large_array(&mut ir, &ctx, frame, 64000)?;
        let leave = heap_helper_index(&ir, "__compat_es5_root_leave")?;
        call_int(
            &mut ir,
            leave,
            vec![vm::Expr::Var(ctx.carrier), vm::Expr::Var(frame)],
        )?;
        let frame = call_int(
            &mut ir,
            ctx.root_enter,
            vec![vm::Expr::Var(ctx.carrier), vm::Expr::Int(1)],
        )?;
        let (slot, generation, base) = large_array(&mut ir, &ctx, frame, 995)?;
        for out in [old_slot, old_generation, slot, generation] {
            ir.stmts.push(vm::Stmt::Expr {
                expr: vm::Expr::Var(out),
                line: 1,
            });
        }
        let old_base = call_int(
            &mut ir,
            ctx.checked_base,
            vec![
                vm::Expr::Var(ctx.carrier),
                vm::Expr::Int(0),
                vm::Expr::Int(1),
            ],
        )?;
        for base in [old_base, base] {
            let value = heap_cycle_read_number(
                &mut ir,
                &ctx,
                ctx.carrier,
                vm::Expr::Add(Box::new(vm::Expr::Var(base)), Box::new(vm::Expr::Int(5))),
                "pressure-payload",
            )?;
            ir.stmts.push(vm::Stmt::Expr {
                expr: vm::Expr::Var(value),
                line: 1,
            });
        }
        for which in [0, 2, 3, 4, 5, 7, 8, 11] {
            heap_cycle_observe(&mut ir, &ctx, ctx.carrier, which, 0, "pressure")?;
        }
        Ok(ir)
    }
}
#[test]
fn p2_pressure_collection_precedes_slot_selection_and_preserves_payloads() {
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        "var a=[7];",
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-pressure")),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(&PressurePlugin)
            .with_host_api_catalog(heap_catalog()),
    )
    .expect("real managed pressure fixture");
    let mut machine = compilation.into_vm().expect("certified VM");
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
        machine.run().expect("pressure must collect and succeed"),
        VmStatus::Halted
    );
    let stack = machine.stack();
    let v = heap_stale_ints(&stack[stack.len() - 14..]);
    eprintln!("P2_PRESSURE_LIVE evidence={v:?}");
    assert_eq!(&v[..6], &[1, 1, 1, 2, 7, 7]);
    assert_eq!(
        (v[6], v[7], v[9], v[11], v[12], v[13]),
        (2, 2, 2, 1012, 0, 0)
    );
    assert!(v[10] >= 1);
    // Prior to measurement: source547 + frame7 + managed big64008=64562;
    // an extra frame can cost7 more. New1003 cannot grow under65536 and
    // must reuse the reclaimed extent instead. Both real arrays stay valid.
    assert!(v[8] <= 64569);
    assert!(v[8] + 1003 > 65536);
    machine.reset_for_reuse().expect("reset pressure VM");
    assert_eq!(machine.host_context().resource_count(), 0);
}
