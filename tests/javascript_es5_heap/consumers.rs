//! Real producer/consumer generation checks. No replacement JS evaluator.
use super::*;

struct ConsumerPlugin {
    binding: &'static str,
    stale: bool,
}
impl vm::SourcePlugin for ConsumerPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-stale-consumer"),
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
        let binding = heap_binding_index(&ir, self.binding)?;
        // Hoisted bindings are predeclared by Let, then stored by Assign.
        // Anchor on that store so the suffix keeps the real publication.
        let at = ir
            .stmts
            .iter()
            .position(|s| matches!(s, vm::Stmt::Assign { index, .. } if *index == binding))
            .ok_or_else(|| heap_rejection("consumer binding".to_owned()))?;
        let ack = matches!(ir.stmts.get(at+1),Some(vm::Stmt::Let{expr:vm::Expr::Call(target,_,_,_,_),..}) if *target==ctx.root_write);
        let suffix = ir.stmts.split_off(at + 1 + usize::from(ack));
        let old = heap_cycle_take(&mut ir, "consumer-old")?;
        ir.stmts.push(vm::Stmt::Let {
            index: old,
            declared_schema: Some(vm::TypeSchema::Named(
                "__CompatEs5Primitive".to_owned(),
                Vec::new(),
            )),
            expr: vm::Expr::Var(binding),
            line: 1,
        });
        let (_, oldslot, oldgen) = heap_cycle_identity(&mut ir, &ctx, binding, "consumer-old")?;
        heap_replay_binding(
            &mut ir,
            binding,
            ctx.node_begin,
            ctx.root_enter,
            ctx.root_write,
        )?;
        heap_cycle_collect(&mut ir, &ctx, ctx.carrier, "consumer-reuse")?;
        if self.binding == "o" {
            // Two real freed nodes: key0 and object1. Reverse the valid free
            // row chain so replay assigns the same kind to each old slot;
            // default LIFO sweep order would swap their kinds instead.
            let raw = heap_cycle_read_number(
                &mut ir,
                &ctx,
                ctx.carrier,
                vm::Expr::Int(16),
                "consumer-page",
            )?;
            let page = heap_cycle_take(&mut ir, "consumer-page")?;
            ir.stmts.push(vm::Stmt::Let {
                index: page,
                declared_schema: Some(vm::TypeSchema::Int),
                expr: heap_guest_call(ctx.decode, vec![vm::Expr::Var(raw)]),
                line: 1,
            });
            for (address, value) in [
                (vm::Expr::Int(5), 1),
                (
                    vm::Expr::Add(
                        Box::new(vm::Expr::Var(page)),
                        Box::new(vm::Expr::Int(3 + 6)),
                    ),
                    2,
                ),
                (
                    vm::Expr::Add(
                        Box::new(vm::Expr::Var(page)),
                        Box::new(vm::Expr::Int(3 + 7 + 6)),
                    ),
                    0,
                ),
            ] {
                heap_cycle_write(
                    &mut ir,
                    &ctx,
                    ctx.carrier,
                    address,
                    vm::Expr::Float(value as f64),
                    "consumer-free-order",
                )?;
            }
        }
        heap_replay_binding(
            &mut ir,
            binding,
            ctx.node_begin,
            ctx.root_enter,
            ctx.root_write,
        )?;
        let (newbox, newslot, newgen) =
            heap_cycle_identity(&mut ir, &ctx, binding, "consumer-new")?;
        // Both objects are deliberately unregistered diagnostic copies for
        // identity only; production consumers below receive the chosen box.

        for (a, b, equal) in [(oldslot, newslot, true), (oldgen, newgen, false)] {
            let comparison = vm::Expr::Eq(Box::new(vm::Expr::Var(a)), Box::new(vm::Expr::Var(b)));
            ir.stmts.push(vm::Stmt::Expr {
                expr: heap_guest_call(
                    vm::BuiltinFunction::Assert.call_index(),
                    vec![if equal {
                        comparison
                    } else {
                        vm::Expr::Not(Box::new(comparison))
                    }],
                ),
                line: 1,
            });
        }
        let generation_diff = vm::Expr::Eq(
            Box::new(vm::Expr::Var(newgen)),
            Box::new(vm::Expr::Add(
                Box::new(vm::Expr::Var(oldgen)),
                Box::new(vm::Expr::Int(1)),
            )),
        );
        ir.stmts.push(vm::Stmt::Expr {
            expr: heap_guest_call(
                vm::BuiltinFunction::Assert.call_index(),
                vec![generation_diff],
            ),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: binding,
            expr: vm::Expr::Var(if self.stale { old } else { newbox }),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::String("consumer-ready".to_owned()),
            line: 1,
        });
        ir.stmts.extend(suffix);
        Ok(ir)
    }
}
fn consumer_run(
    source: &str,
    binding: &'static str,
    stale: bool,
    expected_tag: i64,
    number: Option<f64>,
) {
    let plugin = Box::leak(Box::new(ConsumerPlugin { binding, stale }));
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-stale-consumer")),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(plugin)
            .with_host_api_catalog(heap_catalog()),
    )
    .expect("actual consumer compiles");
    let mut machine = compilation.into_vm().expect("certified consumer VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("B1");
    transaction_registry()
        .bind_vm_cached(&mut machine)
        .expect("bind");
    machine.set_fuel(10_000_000);
    let outcome = machine.run();
    let phase = machine
        .stack()
        .iter()
        .position(|v| *v == Value::string("consumer-ready"))
        .expect("same-slot/same-kind generation assertions must pass before the actual consumer");
    let evidence = heap_stale_ints(&machine.stack()[phase - 4..phase]);
    assert_eq!(evidence[0], evidence[2]);
    assert_eq!(evidence[3], evidence[1] + 1);
    if stale {
        assert!(
            matches!(&outcome,Err(vm::VmError::HostError(m)) if m.contains("assertion failed")),
            "stale actual {binding} consumer: {outcome:?}"
        );
    } else {
        assert_eq!(
            *outcome.as_ref().expect("fresh consumer halt"),
            VmStatus::Halted
        );
        let value = machine.stack().last().expect("actual result");
        assert_eq!(heap_box_tag(value), expected_tag);
        if let Some(number) = number {
            assert_eq!(heap_box_number(value), number);
        }
        if expected_tag == 2 {
            let Value::Map(fields) = value else {
                unreachable!()
            };
            assert_eq!(fields.get(&Value::string("flag")), Some(&Value::Bool(true)));
        }
    }
    eprintln!(
        "P2_ACTUAL_CONSUMER binding={binding} stale={stale} evidence={evidence:?} outcome={outcome:?}"
    );
    machine.reset_for_reuse().expect("reset stale/fresh VM");
    assert_eq!(machine.host_context().resource_count(), 0);
}
#[test]
fn p2_stale_actual_array_read_and_write_consumers() {
    for (source, want) in [
        ("var a=[7];var result=a[0];result;", 7.0),
        ("var a=[7];a[0]=9;var result=a[0];result;", 9.0),
    ] {
        for stale in [true, false] {
            consumer_run(source, "a", stale, 3, Some(want));
        }
    }
}
#[test]
fn p2_stale_actual_string_equality_consumer() {
    for stale in [true, false] {
        consumer_run(
            "var s=\"\\uD800\\0\";var result=s===\"\\uD800\\0\";result;",
            "s",
            stale,
            2,
            None,
        );
    }
}
#[test]
fn p2_stale_actual_object_get_and_function_dispatch_consumers() {
    for stale in [true, false] {
        consumer_run(
            "var o={k:7};var result=o.k;result;",
            "o",
            stale,
            3,
            Some(7.0),
        );
        consumer_run(
            "var f=function(){\"use strict\";return 7;};var result=f();result;",
            "f",
            stale,
            3,
            Some(7.0),
        );
    }
}
