//! Directory high-water boundary with a consistent private retired fixture.
//! Retired metadata is explicitly injected; this does not claim4096 live
//! nodes and all8192 roots fit simultaneously under the physical quota.
use super::*;
struct DirectoryPlugin {
    refuse: bool,
}
impl vm::SourcePlugin for DirectoryPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-directory-limit"),
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
        let frame = ir
            .stmts
            .iter()
            .find_map(|stmt| match stmt {
                vm::Stmt::Let {
                    index,
                    expr: vm::Expr::Call(target, _, args, _, _),
                    ..
                } if *target == ctx.root_enter && matches!(args.get(1), Some(vm::Expr::Int(1))) => {
                    Some(*index)
                }
                _ => None,
            })
            .ok_or_else(|| heap_rejection("one source root".to_owned()))?;
        for offset in 4..7 {
            heap_cycle_write(
                &mut ir,
                &ctx,
                ctx.carrier,
                vm::Expr::Add(
                    Box::new(vm::Expr::Var(frame)),
                    Box::new(vm::Expr::Int(offset)),
                ),
                vm::Expr::Float(0.0),
                "directory-unroot",
            )?;
        }
        heap_cycle_collect(&mut ir, &ctx, ctx.carrier, "directory-unroot")?;
        let reserve = heap_helper_index(&ir, "__compat_es5_raw_reserve")?;
        let page = heap_cycle_take(&mut ir, "directory")?;
        let row = heap_cycle_take(&mut ir, "directory")?;
        let base = heap_cycle_take(&mut ir, "directory")?;
        let ack = heap_cycle_take(&mut ir, "directory")?;
        for (slot, schema, expr) in [
            (page, vm::TypeSchema::Int, vm::Expr::Int(0)),
            (row, vm::TypeSchema::Int, vm::Expr::Int(0)),
            (base, vm::TypeSchema::Int, vm::Expr::Int(0)),
            (ack, vm::TypeSchema::Number, vm::Expr::Float(0.0)),
        ] {
            ir.stmts.push(vm::Stmt::Let {
                index: slot,
                declared_schema: Some(schema),
                expr,
                line: 1,
            });
        }
        let assign = |index, expr| vm::Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index,
            expr,
            line: 1,
        };
        let write = |addr, payload| {
            assign(
                ack,
                heap_guest_call(
                    ctx.write,
                    vec![
                        vm::Expr::Var(ctx.carrier),
                        heap_as_number(addr),
                        heap_as_number(payload),
                    ],
                ),
            )
        };
        let page_addr = vm::Expr::Add(Box::new(vm::Expr::Int(16)), Box::new(vm::Expr::Var(page)));
        let row_addr = |offset: i64| {
            vm::Expr::Add(
                Box::new(vm::Expr::Add(
                    Box::new(vm::Expr::Var(base)),
                    Box::new(vm::Expr::Mul(
                        Box::new(vm::Expr::Var(row)),
                        Box::new(vm::Expr::Int(7)),
                    )),
                )),
                Box::new(vm::Expr::Int(3 + offset)),
            )
        };
        let mut inner = Vec::new();
        for offset in 0..7 {
            inner.push(write(
                row_addr(offset),
                match offset {
                    0 => vm::Expr::Int(4),
                    1 => vm::Expr::Float(f64::from(u32::MAX)),
                    _ => vm::Expr::Int(0),
                },
            ));
        }
        inner.push(assign(
            row,
            vm::Expr::Add(Box::new(vm::Expr::Var(row)), Box::new(vm::Expr::Int(1))),
        ));
        ir.stmts.push(vm::Stmt::While {
            condition: vm::Expr::Lt(Box::new(vm::Expr::Var(page)), Box::new(vm::Expr::Int(64))),
            body: vec![
                vm::Stmt::IfElse {
                    condition: vm::Expr::Eq(
                        Box::new(vm::Expr::Var(page)),
                        Box::new(vm::Expr::Int(0)),
                    ),
                    then_branch: vec![assign(
                        base,
                        heap_guest_call(
                            ctx.decode,
                            vec![heap_guest_call(
                                ctx.read,
                                vec![vm::Expr::Var(ctx.carrier), vm::Expr::Float(16.0)],
                            )],
                        ),
                    )],
                    else_branch: vec![assign(
                        base,
                        vm::Expr::Sub(
                            Box::new(heap_guest_call(
                                reserve,
                                vec![
                                    vm::Expr::Var(ctx.carrier),
                                    vm::Expr::Int(451),
                                    vm::Expr::Int(2),
                                ],
                            )),
                            Box::new(vm::Expr::Int(3)),
                        ),
                    )],
                    line: 1,
                },
                write(page_addr, vm::Expr::Var(base)),
                assign(row, vm::Expr::Int(0)),
                vm::Stmt::While {
                    condition: vm::Expr::Lt(
                        Box::new(vm::Expr::Var(row)),
                        Box::new(vm::Expr::Int(64)),
                    ),
                    body: inner,
                    line: 1,
                },
                assign(
                    page,
                    vm::Expr::Add(Box::new(vm::Expr::Var(page)), Box::new(vm::Expr::Int(1))),
                ),
            ],
            line: 1,
        });
        for (offset, value) in [(4, 4095), (5, 0), (12, 4095), (14, 64)] {
            heap_cycle_write(
                &mut ir,
                &ctx,
                ctx.carrier,
                vm::Expr::Int(offset),
                vm::Expr::Float(value as f64),
                "directory-highwater",
            )?;
        }
        // Last row is genuinely unused; allocation below proves slot4095 is
        // valid before the independent4096 refusal. All other rows retired.
        for offset in [0, 1] {
            heap_cycle_write(
                &mut ir,
                &ctx,
                ctx.carrier,
                vm::Expr::Add(
                    Box::new(vm::Expr::Var(base)),
                    Box::new(vm::Expr::Int(3 + 63 * 7 + offset)),
                ),
                vm::Expr::Float(0.0),
                "directory-last",
            )?;
        }
        let (last, generation, _) = managed_node(&mut ir, &ctx, frame, 0, 1, &[9.0])?;
        for (value, want) in [(last, 4095), (generation, 1)] {
            ir.stmts.push(vm::Stmt::Expr {
                expr: heap_guest_call(
                    vm::BuiltinFunction::Assert.call_index(),
                    vec![vm::Expr::Eq(
                        Box::new(vm::Expr::Var(value)),
                        Box::new(vm::Expr::Int(want)),
                    )],
                ),
                line: 1,
            });
        }
        let snapshot = heap_helper_index(&ir, "__compat_ref_snapshot_values")?;
        ir.stmts.push(vm::Stmt::Expr {
            expr: heap_guest_call(snapshot, vec![vm::Expr::Var(ctx.carrier)]),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::String("directory-limit-ready".to_owned()),
            line: 1,
        });
        if self.refuse {
            call_allocation(&mut ir, &ctx, 6, 1)?;
        }
        Ok(ir)
    }
}
#[test]
fn p2_directory_last_slot_and_exhaustion_are_transactional() {
    let mut capacity = None;
    for refuse in [false, true] {
        let plugin = Box::leak(Box::new(DirectoryPlugin { refuse }));
        let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
            "var a=[7];",
            vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-directory-limit")),
            vm::CompileSourceFileOptions::new()
                .with_source_plugin(plugin)
                .with_host_api_catalog(heap_catalog()),
        )
        .expect("directory fixture compile");
        let mut machine = compilation.into_vm().expect("certified directory VM");
        pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
            &mut machine,
            pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
        )
        .expect("B1");
        transaction_registry()
            .bind_vm_cached(&mut machine)
            .expect("bind");
        machine.set_fuel(100_000_000);
        let outcome = machine.run();
        if refuse {
            assert!(
                matches!(&outcome,Err(vm::VmError::HostError(m)) if m.contains("assertion failed"))
            );
        } else {
            assert_eq!(
                *outcome.as_ref().expect("directory last-slot success"),
                VmStatus::Halted
            );
        }
        let phase = machine
            .stack()
            .iter()
            .position(|v| *v == Value::string("directory-limit-ready"))
            .expect("last slot succeeded before refusal");
        let Value::Array(before) = &machine.stack()[phase - 1] else {
            panic!("actual snapshot")
        };
        let before = before.to_vec();
        let after = backing_snapshot(&mut machine);
        assert_eq!(before, after, "directory refusal cannot change any cell");
        let cells = heap_stale_ints(&after);
        assert_eq!(
            (cells[4], cells[6], cells[7], cells[8], cells[12], cells[14]),
            (4096, 1, 9, 0, 4095, 64)
        );
        for slot in 0..4095 {
            let addr = cells[16 + slot / 64] as usize + 3 + (slot % 64) * 7;
            assert_eq!(&cells[addr..addr + 2], &[4, i64::from(u32::MAX)]);
        }
        let stats = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
            .unwrap();
        if let Some(expected) = capacity {
            assert_eq!(stats.reserved_bytes, expected);
        } else {
            capacity = Some(stats.reserved_bytes);
        }
        eprintln!(
            "P2_DIRECTORY_LIMIT refuse={refuse} outcome={outcome:?} header={:?} bytes={}",
            &cells[..16],
            stats.reserved_bytes
        );
        machine.reset_for_reuse().expect("reset directory fixture");
        assert_eq!(machine.host_context().resource_count(), 0);
    }
}
