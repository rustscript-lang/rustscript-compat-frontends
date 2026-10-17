//! Maintained P2-B acceptance fixtures. Included by the heap target only.
use super::*;
use std::cell::RefCell;

#[path = "extents.rs"]
mod extents;

#[path = "consumers.rs"]
mod consumers;

#[path = "safepoints.rs"]
mod safepoints;

#[path = "directory.rs"]
mod directory;

#[path = "frames.rs"]
mod frames;

#[path = "pressure.rs"]
mod pressure;

thread_local! { static LAST_HANDLE: RefCell<Option<Value>> = const { RefCell::new(None) }; }
static ORIGINAL_READ: std::sync::OnceLock<vm::StaticHostFunction> = std::sync::OnceLock::new();

fn capture_read(machine: &mut vm::Vm, args: &[Value]) -> vm::VmResult<vm::CallOutcome> {
    LAST_HANDLE.with(|handle| *handle.borrow_mut() = args.first().cloned());
    ORIGINAL_READ.get().expect("actual production read")(machine, args)
}

// Observe the actual B1 resource through its existing typed read-only boundary.
// This calls no guest code, resumes no failed script and implements no JS semantics.
fn backing_snapshot(machine: &mut vm::Vm) -> Vec<Value> {
    let handle = LAST_HANDLE.with(|handle| handle.borrow().clone().expect("real read handle"));
    let descriptor =
        pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module()
            .descriptors()
            .into_iter()
            .find(|d| d.schema.name == "arrays::snapshot")
            .expect("real snapshot descriptor");
    let outcome = match descriptor.adapter {
        vm::HostAdapterDescriptor::Static(f)
        | vm::HostAdapterDescriptor::StaticStack(f)
        | vm::HostAdapterDescriptor::StaticStackRuntimeOwned(f) => f(machine, &[handle]),
        _ => panic!("snapshot requires actual static adapter"),
    }
    .expect("independent typed snapshot of still-open payload");
    match outcome {
        vm::CallOutcome::Return(vm::CallReturn::One(Value::Array(values))) => values.to_vec(),
        other => panic!("real snapshot must return numeric array: {other:?}"),
    }
}

fn transaction_registry() -> vm::HostFunctionRegistry {
    let mut descriptors = Vec::new();
    for module in [
        pd_vm_compat_frontends::runtime::array::typescript_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module(),
        pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module(),
    ] {
        descriptors.extend(module.descriptors());
    }
    for descriptor in &mut descriptors {
        if descriptor.schema.name == "arrays::read" {
            descriptor.adapter = match descriptor.adapter {
                vm::HostAdapterDescriptor::Static(f) => {
                    ORIGINAL_READ.set(f).ok();
                    vm::HostAdapterDescriptor::Static(capture_read)
                }
                vm::HostAdapterDescriptor::StaticStack(f) => {
                    ORIGINAL_READ.set(f).ok();
                    vm::HostAdapterDescriptor::StaticStack(capture_read)
                }
                vm::HostAdapterDescriptor::StaticStackRuntimeOwned(f) => {
                    ORIGINAL_READ.set(f).ok();
                    vm::HostAdapterDescriptor::StaticStackRuntimeOwned(capture_read)
                }
                _ => panic!("read must be a real VM-aware static adapter"),
            };
        }
    }
    let mut registry = vm::HostFunctionRegistry::restricted();
    let effective = vm::HostModuleDescriptor::install_descriptors(&mut registry, &descriptors)
        .expect("unchanged descriptor installation");
    assert_eq!(
        format!("{:?}", effective.fingerprint()),
        format!("{:?}", heap_catalog().fingerprint())
    );
    for descriptor in &descriptors {
        registry.authorize_registered_builtin_import(&descriptor.schema.name);
    }
    registry
        .allow_builtin("math::floor")
        .expect("numeric builtin");
    registry
}

#[derive(Clone, Copy, Debug)]
enum Boundary {
    Fresh,
    Reuse,
    NewPage,
    Physical,
    Pending,
    RootSingle,
    RootCumulative,
    CorruptGeneration,
    CorruptState,
}
struct BoundaryPlugin {
    boundary: Boundary,
    refuse: bool,
}

impl vm::SourcePlugin for BoundaryPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-heap-boundary"),
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
        if matches!(
            self.boundary,
            Boundary::Reuse | Boundary::CorruptGeneration | Boundary::CorruptState
        ) {
            let binding = heap_binding_index(&ir, "a")?;
            heap_replay_binding(
                &mut ir,
                binding,
                ctx.node_begin,
                ctx.root_enter,
                ctx.root_write,
            )?;
            heap_cycle_collect(&mut ir, &ctx, ctx.carrier, "quota-reuse")?;
            if matches!(
                self.boundary,
                Boundary::CorruptGeneration | Boundary::CorruptState
            ) {
                let raw = heap_cycle_read_number(
                    &mut ir,
                    &ctx,
                    ctx.carrier,
                    vm::Expr::Int(16),
                    "corrupt-free",
                )?;
                let page = call_int(&mut ir, ctx.decode, vec![vm::Expr::Var(raw)])?;
                let (offset, value) = if matches!(self.boundary, Boundary::CorruptGeneration) {
                    (4, -1.0)
                } else {
                    (3, 4.0)
                };
                heap_cycle_write(
                    &mut ir,
                    &ctx,
                    ctx.carrier,
                    vm::Expr::Add(
                        Box::new(vm::Expr::Var(page)),
                        Box::new(vm::Expr::Int(offset)),
                    ),
                    vm::Expr::Float(value),
                    "corrupt-free",
                )?;
            }
        }
        if matches!(self.boundary, Boundary::NewPage) {
            // 64 live nodes in one page, through actual managed begin/publish.
            // Existing source is one array; 63 private empty strings use a
            // real 63-entry root frame. No synthetic directory metadata.
            let frame = call_int(
                &mut ir,
                ctx.root_enter,
                vec![vm::Expr::Var(ctx.carrier), vm::Expr::Int(63)],
            )?;
            for entry in 0..63 {
                managed_node(&mut ir, &ctx, frame, entry, 2, &[])?;
            }
        }
        if matches!(self.boundary, Boundary::Pending) {
            call_allocation(&mut ir, &ctx, 6, 1)?;
        }
        if matches!(self.boundary, Boundary::RootCumulative) {
            call_int(
                &mut ir,
                ctx.root_enter,
                vec![vm::Expr::Var(ctx.carrier), vm::Expr::Int(4096)],
            )?;
        }
        if matches!(self.boundary, Boundary::NewPage) {
            heap_cycle_collect(&mut ir, &ctx, ctx.carrier, "quota-preflight")?;
        }
        // Snapshot before the fallible reservation, including every raw header,
        // directory row, root and old payload. Backing bytes/capacity and source
        // facts are independently inspected by the embedding after run.
        let snapshot = heap_helper_index(&ir, "__compat_ref_snapshot_values")?;
        ir.stmts.push(vm::Stmt::Expr {
            expr: heap_guest_call(snapshot, vec![vm::Expr::Var(ctx.carrier)]),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::String("boundary-ready".to_owned()),
            line: 1,
        });
        if self.refuse {
            match self.boundary {
                Boundary::RootSingle => {
                    call_int(
                        &mut ir,
                        ctx.root_enter,
                        vec![vm::Expr::Var(ctx.carrier), vm::Expr::Int(8193)],
                    )?;
                }
                Boundary::RootCumulative => {
                    call_int(
                        &mut ir,
                        ctx.root_enter,
                        vec![vm::Expr::Var(ctx.carrier), vm::Expr::Int(4096)],
                    )?;
                }
                Boundary::Physical => {
                    call_allocation(&mut ir, &ctx, 65536, 1)?;
                }
                Boundary::Pending => {
                    call_allocation(&mut ir, &ctx, 6, 1)?;
                }
                Boundary::CorruptGeneration | Boundary::CorruptState => {
                    call_allocation(&mut ir, &ctx, 6, 1)?;
                }
                _ => {
                    call_allocation(&mut ir, &ctx, 8197, 1)?;
                }
            }
        }
        Ok(ir)
    }
}

fn call_int(
    ir: &mut vm::FrontendIr,
    helper: u16,
    args: Vec<vm::Expr>,
) -> Result<u16, vm::ParseError> {
    let slot = heap_cycle_take(ir, "boundary")?;
    ir.stmts.push(vm::Stmt::Let {
        index: slot,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: heap_guest_call(helper, args),
        line: 1,
    });
    Ok(slot)
}
fn call_allocation(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    cells: i64,
    kind: i64,
) -> Result<u16, vm::ParseError> {
    let slot = heap_cycle_take(ir, "boundary")?;
    ir.stmts.push(vm::Stmt::Let {
        index: slot,
        declared_schema: Some(vm::TypeSchema::Named(
            "__CompatEs5Allocation".to_owned(),
            Vec::new(),
        )),
        expr: heap_guest_call(
            ctx.node_begin,
            vec![
                vm::Expr::Var(ctx.carrier),
                vm::Expr::Int(cells),
                vm::Expr::Int(kind),
            ],
        ),
        line: 1,
    });
    Ok(slot)
}

// Private node initializer: exact real begin/write/publish protocol, immutable
// string or numeric array payload only. Returns real allocation slot/gen/base.
fn managed_node(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    frame: u16,
    entry: i64,
    kind: i64,
    payload: &[f64],
) -> Result<(u16, u16, u16), vm::ParseError> {
    let header = if kind == 2 { 3 } else { 5 };
    let allocation = call_allocation(ir, ctx, header + payload.len() as i64, kind)?;
    let mut fields = Vec::new();
    for field in ["slot", "generation", "base"] {
        let slot = heap_cycle_take(ir, "boundary")?;
        ir.stmts.push(vm::Stmt::Let {
            index: slot,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_field_get(ctx.get, allocation, field),
            line: 1,
        });
        fields.push(slot);
    }
    let address = |offset| {
        vm::Expr::Add(
            Box::new(vm::Expr::Var(fields[2])),
            Box::new(vm::Expr::Int(offset)),
        )
    };
    heap_cycle_write(
        ir,
        ctx,
        ctx.carrier,
        address(2),
        vm::Expr::Float(payload.len() as f64),
        "boundary",
    )?;
    if kind == 1 {
        for offset in [3, 4] {
            heap_cycle_write(
                ir,
                ctx,
                ctx.carrier,
                address(offset),
                vm::Expr::Float(0.0),
                "boundary",
            )?;
        }
    }
    for (offset, value) in payload.iter().enumerate() {
        heap_cycle_write(
            ir,
            ctx,
            ctx.carrier,
            address(header + offset as i64),
            vm::Expr::Float(*value),
            "boundary",
        )?;
    }
    let publish = heap_helper_index(ir, "__compat_es5_node_publish")?;
    let boxed = heap_cycle_take(ir, "boundary")?;
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
                vm::Expr::Int(entry),
            ],
        ),
        line: 1,
    });
    Ok((fields[0], fields[1], fields[2]))
}

fn boundary_run(boundary: Boundary, refuse: bool) -> (Vec<Value>, Vec<Value>, usize) {
    let plugin = Box::leak(Box::new(BoundaryPlugin { boundary, refuse }));
    let catalog = heap_catalog();
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        "var a=[7];",
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-heap-boundary")),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(plugin)
            .with_host_api_catalog(catalog),
    )
    .expect("real boundary compilation");
    let mut machine = compilation.into_vm().expect("certified boundary VM");
    // Derived prior to execution: prefix80 + source frame14 + page451 +
    // source node9 =554 cells or less for this simple source; one extra
    // page of64 nodes and roots fits 8192 cells. Requested8197 data cells
    // exceeds the fixed8192-cell B1 budget even before metadata.
    let limits = pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::new(
        8,
        64,
        if matches!(
            boundary,
            Boundary::RootCumulative
                | Boundary::Physical
                | Boundary::Pending
                | Boundary::RootSingle
        ) {
            1 << 20
        } else {
            8192 * std::mem::size_of::<Value>()
        },
    )
    .expect("layout-derived byte quota");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        limits,
    )
    .expect("initialize B1");
    transaction_registry()
        .bind_vm_cached(&mut machine)
        .expect("bind unchanged catalog");
    machine.set_fuel(100_000_000);
    let outcome = machine.run();
    if refuse {
        assert!(
            outcome.is_err(),
            "intended refusal {boundary:?}: {outcome:?}"
        );
    } else {
        assert_eq!(*outcome.as_ref().expect("control halt"), VmStatus::Halted);
    }
    let phase = machine
        .stack()
        .iter()
        .position(|v| *v == Value::string("boundary-ready"))
        .expect("real pre-reservation phase");
    let Value::Array(before) = &machine.stack()[phase - 1] else {
        panic!("actual pre-reservation snapshot")
    };
    let before = before.to_vec();
    let after = backing_snapshot(&mut machine);
    let stats = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
        .expect("B1 remains accounted");
    assert_eq!((stats.pending, stats.registered), (0, 1));
    assert_eq!(machine.host_context().resource_count(), 1);
    assert!(
        machine
            .capture_certification_stats()
            .expect("source facts")
            .valid
    );
    eprintln!(
        "P2_BOUNDARY {boundary:?} refuse={refuse} outcome={outcome:?} before_header={:?} after_header={:?} lengths={}/{} reserved={} capacity={}",
        &before[..16],
        &after[..16],
        before.len(),
        after.len(),
        stats.reserved_bytes,
        stats.reserved_bytes / std::mem::size_of::<Value>()
    );
    // Physical-pressure collection is legitimate maintenance: marks/gray,
    // allocations_since_gc and collection count may change independently.
    if matches!(boundary, Boundary::Physical) {
        for index in [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 12, 14, 15] {
            assert_eq!(
                before[index], after[index],
                "physical quota must preserve accounting cell {index}"
            );
        }
        assert_eq!(before.len(), after.len());
    } else {
        assert_eq!(
            before, after,
            "refusal before any logical/free-list/row/payload commit: {boundary:?}"
        );
    }
    machine
        .reset_for_reuse()
        .expect("reset after refusal/control");
    assert_eq!(machine.host_context().resource_count(), 0);
    assert!(
        pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine).is_none()
    );
    assert!(
        !machine
            .capture_certification_stats()
            .expect("revoked facts")
            .valid
    );
    (before, after, stats.reserved_bytes)
}

#[test]
fn p2_static_budget_includes_known_directory_and_root_cells() {
    // 80 + 4*(16300+6) =65304 fits payload-only accounting; one page451
    // and script roots4+4*2*3 make65783, exceeding65536 before emission.
    let source = (0..4)
        .map(|i| format!("var s{i}=\"{}\";", "a".repeat(16300)))
        .collect::<String>();
    let result = vm::compile_source_with_flavor_and_options_with_capture_certification(
        &source,
        pd_vm_compat_frontends::javascript_es5_flavor(),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(&JAVASCRIPT_ES5_PLUGIN)
            .with_host_api_catalog(heap_catalog()),
    );
    let error = match result {
        Ok(_) => panic!("metadata-inclusive excess must refuse"),
        Err(error) => format!("{error:?}"),
    };
    eprintln!("P2_STATIC_METADATA_BUDGET {error}");
    assert!(
        error.contains("65536 cell budget"),
        "must reject before local-slot/codegen excess: {error}"
    );
}

#[test]
fn p2_quota_fresh_slot_refusal_is_precommit() {
    let c = boundary_run(Boundary::Fresh, false);
    let r = boundary_run(Boundary::Fresh, true);
    assert_eq!(c.2, r.2);
}
#[test]
fn p2_quota_reused_slot_refusal_is_precommit() {
    let c = boundary_run(Boundary::Reuse, false);
    let r = boundary_run(Boundary::Reuse, true);
    assert_eq!(c.2, r.2);
}
#[test]
fn p2_quota_new_page_and_node_refusal_is_precommit() {
    let c = boundary_run(Boundary::NewPage, false);
    let r = boundary_run(Boundary::NewPage, true);
    assert_eq!(c.2, r.2);
}
#[test]
fn p2_quota_physical_refusal_preserves_slot_accounting() {
    boundary_run(Boundary::Physical, true);
}
#[test]
fn p2_quota_second_pending_begin_refuses() {
    boundary_run(Boundary::Pending, true);
}
#[test]
fn p2_corrupt_free_generation_is_not_normalized() {
    boundary_run(Boundary::CorruptGeneration, true);
}
#[test]
fn p2_corrupt_free_row_state_is_rejected() {
    boundary_run(Boundary::CorruptState, true);
}
#[test]
fn p2_quota_root_single_and_cumulative_refuse_precommit() {
    boundary_run(Boundary::RootSingle, true);
    boundary_run(Boundary::RootCumulative, true);
}
