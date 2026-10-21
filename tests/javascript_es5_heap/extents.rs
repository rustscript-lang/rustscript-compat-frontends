//! Exact raw extent split/unsplittable-tail fixtures.
use super::*;

struct FragmentPlugin {
    slack: usize,
}
impl vm::SourcePlugin for FragmentPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-heap-fragment"),
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
        managed_node(&mut ir, &ctx, frame, 0, 1, &[11.0, 12.0, 13.0])?;
        let leave = heap_helper_index(&ir, "__compat_es5_root_leave")?;
        call_int(
            &mut ir,
            leave,
            vec![vm::Expr::Var(ctx.carrier), vm::Expr::Var(frame)],
        )?;
        heap_cycle_collect(&mut ir, &ctx, ctx.carrier, "fragment")?;
        let frame = call_int(
            &mut ir,
            ctx.root_enter,
            vec![vm::Expr::Var(ctx.carrier), vm::Expr::Int(1)],
        )?;
        managed_node(&mut ir, &ctx, frame, 0, 1, &vec![7.0; 3 - self.slack])?;
        let snapshot = heap_helper_index(&ir, "__compat_ref_snapshot_values")?;
        ir.stmts.push(vm::Stmt::Expr {
            expr: heap_guest_call(snapshot, vec![vm::Expr::Var(ctx.carrier)]),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::String("fragment-before-collect".to_owned()),
            line: 1,
        });
        heap_cycle_collect(&mut ir, &ctx, ctx.carrier, "fragment-after")?;
        Ok(ir)
    }
}

fn assert_raw_tiling(cells: &[Value]) {
    let data = heap_stale_ints(cells);
    let mut cursor = 80usize;
    let mut node_cells = 0i64;
    while cursor < data.len() {
        let size = data[cursor];
        assert!(
            size >= 4 && cursor + size as usize <= data.len(),
            "invalid raw extent at {cursor}: size={size} end={}",
            data.len()
        );
        if data[cursor + 1] == 1 {
            node_cells += size;
        }
        cursor += size as usize;
    }
    assert_eq!(cursor, data.len(), "all cells must belong to one raw block");
    assert_eq!(
        node_cells, data[7],
        "actual occupied node extents must equal their charge"
    );
}

#[test]
fn p2_raw_remainders_one_two_three_preserve_tiling_and_exact_charge() {
    for slack in 1..=3 {
        let plugin = Box::leak(Box::new(FragmentPlugin { slack }));
        let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
            "var a=[7];",
            vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-heap-fragment")),
            vm::CompileSourceFileOptions::new()
                .with_source_plugin(plugin)
                .with_host_api_catalog(heap_catalog()),
        )
        .expect("fragment compilation");
        let mut machine = compilation.into_vm().expect("certified fragment VM");
        pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
            &mut machine,
            pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
        )
        .expect("B1");
        transaction_registry()
            .bind_vm_cached(&mut machine)
            .expect("exact registry");
        machine.set_fuel(10_000_000);
        let outcome = machine.run();
        let marker = machine
            .stack()
            .iter()
            .position(|v| *v == Value::string("fragment-before-collect"))
            .expect("real short-tail allocation reached");
        let Value::Array(snapshot) = &machine.stack()[marker - 1] else {
            panic!("real snapshot")
        };
        eprintln!(
            "P2_FRAGMENT slack={slack} outcome={outcome:?} header={:?} raw_tail={:?}",
            &snapshot[..16],
            &snapshot[547..]
        );
        assert_raw_tiling(snapshot);
        assert_eq!(
            outcome.expect("collection over all raw blocks"),
            VmStatus::Halted
        );
        let after = backing_snapshot(&mut machine);
        assert_raw_tiling(&after);
        machine.reset_for_reuse().expect("reset fragment VM");
        assert_eq!(machine.host_context().resource_count(), 0);
    }
}

struct IslandsPlugin;
impl vm::SourcePlugin for IslandsPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-free-islands"),
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
            vec![vm::Expr::Var(ctx.carrier), vm::Expr::Int(3)],
        )?;
        managed_node(&mut ir, &ctx, frame, 0, 1, &[1.0; 4])?;
        let (kept, generation, _) = managed_node(&mut ir, &ctx, frame, 1, 1, &[7.0])?;
        managed_node(&mut ir, &ctx, frame, 2, 1, &[])?;
        let leave = heap_helper_index(&ir, "__compat_es5_root_leave")?;
        call_int(
            &mut ir,
            leave,
            vec![vm::Expr::Var(ctx.carrier), vm::Expr::Var(frame)],
        )?;
        let frame = call_int(
            &mut ir,
            ctx.root_enter,
            vec![vm::Expr::Var(ctx.carrier), vm::Expr::Int(2)],
        )?;
        let root = heap_helper_index(&ir, "__compat_es5_root_write_node")?;
        call_int(
            &mut ir,
            root,
            vec![
                vm::Expr::Var(ctx.carrier),
                vm::Expr::Var(frame),
                vm::Expr::Int(0),
                vm::Expr::Int(5),
                vm::Expr::Var(kept),
                vm::Expr::Var(generation),
            ],
        )?;
        heap_cycle_collect(&mut ir, &ctx, ctx.carrier, "free-islands")?;
        let snapshot = heap_helper_index(&ir, "__compat_ref_snapshot_values")?;
        ir.stmts.push(vm::Stmt::Expr {
            expr: heap_guest_call(snapshot, vec![vm::Expr::Var(ctx.carrier)]),
            line: 1,
        });
        managed_node(&mut ir, &ctx, frame, 1, 1, &[9.0; 3])?;
        heap_cycle_observe(&mut ir, &ctx, ctx.carrier, 10, 0, "free-islands")?;
        Ok(ir)
    }
}
#[test]
fn p2_collector_links_all_free_islands_across_live_blocks() {
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        "var a=[7];",
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-free-islands")),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(&IslandsPlugin)
            .with_host_api_catalog(heap_catalog()),
    )
    .expect("real island fixture");
    let mut machine = compilation.into_vm().expect("certified island VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("B1");
    transaction_registry()
        .bind_vm_cached(&mut machine)
        .expect("bind");
    machine.set_fuel(10_000_000);
    assert_eq!(
        machine.run().expect("real island execution"),
        VmStatus::Halted
    );
    let stack = machine.stack();
    let Value::Array(before) = &stack[stack.len() - 2] else {
        panic!("actual island snapshot")
    };
    let cells = heap_stale_ints(before);
    let mut all = Vec::new();
    let mut cursor = 80usize;
    while cursor < cells.len() {
        if cells[cursor + 1] == 0 {
            all.push(cursor);
        }
        cursor += cells[cursor] as usize;
    }
    let mut linked = Vec::new();
    let mut cursor = cells[2] as usize;
    while cursor != 0 {
        assert!(!linked.contains(&cursor), "free chain cycle");
        linked.push(cursor);
        cursor = cells[cursor + 2] as usize;
    }
    all.sort();
    linked.sort();
    eprintln!(
        "P2_FREE_ISLANDS all={all:?} linked={linked:?} before_length={} after_length={:?}",
        cells.len(),
        stack.last()
    );
    assert!(all.len() >= 2);
    assert_eq!(
        all, linked,
        "coalescing must retain all separated free extents"
    );
    assert_eq!(
        heap_stale_ints(&stack[stack.len() - 1..]),
        [cells.len() as i64],
        "first fitting earlier island must be reusable without growth"
    );
    machine.reset_for_reuse().expect("reset islands");
    assert_eq!(machine.host_context().resource_count(), 0);
}
