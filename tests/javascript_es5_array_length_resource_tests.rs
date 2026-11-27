//! Host-side witnesses for the Array length extent and collection findings.
//!
//! The existing guest Halted flags do not record host capacity or
//! collection_count. These tests use the public JavaScript compile path for
//! the store assertion, and a test-only observer plugin for the counters.
//! N06 has no selector in this worktree and is not invented here.

use pd_vm_compat_frontends::{
    JAVASCRIPT_ES5_PLUGIN, javascript_es5_cli_compile_options,
};
use vm::{Value, VmStatus};

const HEAP_MAKE_NAME: &str = "__compat_ref_make";
const HEAP_OBSERVE_NAME: &str = "__compat_es5_heap_observe";
const HEAP_COLLECT_NAME: &str = "__compat_es5_heap_collect";
const OBSERVE_HEAP_END: i64 = 3;
const OBSERVE_ACTIVE_ROOT_CAPACITY: i64 = 4;
const OBSERVE_COLLECTIONS: i64 = 5;
const OBSERVE_NODE_CELLS: i64 = 7;
const OBSERVE_BACKING_LENGTH: i64 = 10;
const PLUGIN_ID: &str = "javascript-es5-array-length-resource";

struct ResourcePlugin {
    force_collect: bool,
}

fn rejection(message: String) -> vm::ParseError {
    vm::ParseError::at_span(vm::compiler::source_map::Span::new(0, 0, 0), message)
}

fn helper_index(ir: &vm::FrontendIr, name: &str) -> Result<u16, vm::ParseError> {
    ir.functions
        .iter()
        .find(|function| function.name == name)
        .map(|function| function.index)
        .ok_or_else(|| rejection(format!("array resource witness requires helper '{name}'")))
}

fn fresh_slot(ir: &mut vm::FrontendIr) -> Result<u16, vm::ParseError> {
    let slot = u16::try_from(ir.locals)
        .map_err(|_| rejection("array resource witness slot space exhausted".to_owned()))?;
    ir.locals += 1;
    Ok(slot)
}

fn observe_call(observe: u16, carrier: u16, which: i64) -> vm::Expr {
    vm::Expr::Call(
        observe,
        Vec::new(),
        vec![
            vm::Expr::Var(carrier),
            vm::Expr::Int(which),
            vm::Expr::Int(0),
        ],
        None,
        None,
    )
}

fn push_observe(ir: &mut vm::FrontendIr, observe: u16, carrier: u16, which: i64) -> Result<(), vm::ParseError> {
    let out = fresh_slot(ir)?;
    ir.stmts.push(vm::Stmt::Let {
        index: out,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: observe_call(observe, carrier, which),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::Var(out),
        line: 1,
    });
    Ok(())
}

impl vm::SourcePlugin for ResourcePlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new(PLUGIN_ID),
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
        let observe = helper_index(&ir, HEAP_OBSERVE_NAME)?;
        let make = helper_index(&ir, HEAP_MAKE_NAME)?;
        let mut carriers = Vec::new();
        for stmt in &ir.stmts {
            if let vm::Stmt::Let {
                index,
                declared_schema: None,
                expr: vm::Expr::Call(target, _, args, _, _),
                ..
            } = stmt
                && *target == make
                && args.len() == 1
            {
                carriers.push(*index);
            }
        }
        let [carrier] = carriers.as_slice() else {
            return Err(rejection(format!(
                "array resource witness requires exactly one realm carrier, found {}",
                carriers.len()
            )));
        };
        let carrier = *carrier;
        if self.force_collect {
            if ir.stmts.is_empty() {
                return Err(rejection(
                    "array resource witness has no statement to follow collection".to_owned(),
                ));
            }
            let collect = helper_index(&ir, HEAP_COLLECT_NAME)?;
            let before = fresh_slot(&mut ir)?;
            let ack = fresh_slot(&mut ir)?;
            let after = fresh_slot(&mut ir)?;
            let insert_at = ir.stmts.len() - 1;
            let forced = vec![
                vm::Stmt::Let {
                    index: before,
                    declared_schema: Some(vm::TypeSchema::Int),
                    expr: observe_call(observe, carrier, OBSERVE_COLLECTIONS),
                    line: 1,
                },
                vm::Stmt::Expr {
                    expr: vm::Expr::Var(before),
                    line: 1,
                },
                vm::Stmt::Let {
                    index: ack,
                    declared_schema: Some(vm::TypeSchema::Int),
                    expr: vm::Expr::Call(
                        collect,
                        Vec::new(),
                        vec![vm::Expr::Var(carrier)],
                        None,
                        None,
                    ),
                    line: 1,
                },
                vm::Stmt::Expr {
                    expr: vm::Expr::Var(ack),
                    line: 1,
                },
                vm::Stmt::Let {
                    index: after,
                    declared_schema: Some(vm::TypeSchema::Int),
                    expr: observe_call(observe, carrier, OBSERVE_COLLECTIONS),
                    line: 1,
                },
                vm::Stmt::Expr {
                    expr: vm::Expr::Var(after),
                    line: 1,
                },
            ];
            ir.stmts.splice(insert_at..insert_at, forced);
        }
        for which in [
            OBSERVE_HEAP_END,
            OBSERVE_ACTIVE_ROOT_CAPACITY,
            OBSERVE_COLLECTIONS,
            OBSERVE_NODE_CELLS,
            OBSERVE_BACKING_LENGTH,
        ] {
            push_observe(&mut ir, observe, carrier, which)?;
        }
        Ok(ir)
    }
}

fn with_stack<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .name("array-length-resource".to_owned())
        .stack_size(33 * 1024 * 1024)
        .spawn(work)
        .expect("resource witness thread")
        .join()
        .expect("resource witness thread panicked")
}

fn install_public_cli(machine: &mut vm::Vm) {
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let mut registry = vm::HostFunctionRegistry::new();
    pd_vm_compat_frontends::runtime::array::typescript_array_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("array host module");
    pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("owned array host module");
    pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("array snapshot host module");
    pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("scalar math host module");
    pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("scalar error host module");
    for builtin in [
        "math::floor",
        "math::sqrt",
        "math::round",
        "math::powf",
        "math::is_nan",
        "math::is_finite",
    ] {
        registry
            .allow_builtin(builtin)
            .unwrap_or_else(|error| panic!("public CLI grant {builtin} failed: {error}"));
    }
    registry
        .bind_vm_cached(machine)
        .expect("public CLI registry should bind");
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..*machine.jit_config()
    });
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("owned array initialization");
    machine.set_fuel(10_000_000);
}

fn run_observed(source: &str, force_collect: bool) -> (Vec<Value>, vm::Vm) {
    let plugin: &'static ResourcePlugin = Box::leak(Box::new(ResourcePlugin { force_collect }));
    let options = javascript_es5_cli_compile_options().with_source_plugin(plugin);
    let issued = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new(PLUGIN_ID)),
        options,
    )
    .expect("resource witness compilation");
    let mut machine = issued.into_vm().expect("resource witness VM");
    install_public_cli(&mut machine);
    let status = machine.run();
    match status {
        Ok(VmStatus::Halted) => {}
        Ok(other) => panic!("resource witness did not halt: {other:?}"),
        Err(error) => panic!(
            "resource witness run failed: {error:?} ip={} stack={:?}",
            machine.ip(),
            machine.stack()
        ),
    }
    assert!(!machine.jit_config().enabled);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    (machine.stack().to_vec(), machine)
}

fn int_at(stack: &[Value], index: usize) -> i64 {
    match &stack[index] {
        Value::Int(value) => *value,
        Value::Float(value) => *value as i64,
        other => panic!("observe selector returned {other:?}"),
    }
}

fn guest_flag(value: &Value) -> bool {
    let Value::Map(fields) = value else {
        panic!("guest result is not a boxed value: {value:?}");
    };
    let tag = fields
        .get(&Value::string("tag"))
        .and_then(|item| match item {
            Value::Float(number) => Some(*number as i64),
            Value::Int(number) => Some(*number),
            _ => None,
        })
        .expect("boxed tag");
    let flag = match fields.get(&Value::string("flag")) {
        Some(Value::Bool(value)) => *value,
        Some(Value::Float(number)) => *number != 0.0,
        Some(Value::Int(number)) => *number != 0,
        other => panic!("boxed flag missing or unexpected: {other:?}"),
    };
    assert_eq!(tag, 2, "guest result tag");
    flag
}

struct HostCounters {
    heap_end: i64,
    active_root_capacity: i64,
    collections: i64,
    node_cells: i64,
    backing_length: i64,
}

fn tail_counters(stack: &[Value]) -> (bool, HostCounters) {
    assert!(
        stack.len() >= 6,
        "guest result plus five observe values, stack={}",
        stack.len()
    );
    let base = stack.len() - 6;
    (
        guest_flag(&stack[base]),
        HostCounters {
            heap_end: int_at(stack, base + 1),
            active_root_capacity: int_at(stack, base + 2),
            collections: int_at(stack, base + 3),
            node_cells: int_at(stack, base + 4),
            backing_length: int_at(stack, base + 5),
        },
    )
}

/// Same-shape host capacity gate. The sources differ by one length literal.
/// `n` is assigned and read back, so a no-op setter cannot pass the guest flag.
/// A length read versus a length write, or a hole read on only one side, is not
/// this gate: those shapes move host capacity by themselves.
#[test]
fn array_length165_f06_host_capacity() {
    with_stack(|| {
        let baseline = "var n=1;var a=[1];a.length=n;a.length===n&&a[0]===1;";
        let extent = "var n=13105;var a=[1];a.length=n;a.length===n&&a[0]===1;";
        let (base_stack, _) = run_observed(baseline, false);
        let (extent_stack, _) = run_observed(extent, false);
        let (base_flag, base) = tail_counters(&base_stack);
        let (extent_flag, extent_counters) = tail_counters(&extent_stack);
        let heap_delta = extent_counters.heap_end - base.heap_end;
        println!(
            "F06_HOST_CAPACITY base_heap_end={} extent_heap_end={} delta={} base_capacity={} extent_capacity={} base_collections={} extent_collections={} base_node_cells={} extent_node_cells={} base_backing={} extent_backing={}",
            base.heap_end,
            extent_counters.heap_end,
            heap_delta,
            base.active_root_capacity,
            extent_counters.active_root_capacity,
            base.collections,
            extent_counters.collections,
            base.node_cells,
            extent_counters.node_cells,
            base.backing_length,
            extent_counters.backing_length
        );
        assert!(base_flag, "baseline guest flag");
        assert!(extent_flag, "extent guest flag");
        assert_eq!(
            extent_counters.active_root_capacity, base.active_root_capacity,
            "length 13105 must not change host active root capacity"
        );
        assert!(
            heap_delta < 256,
            "length 13105 same-shape assignment allocated host heap cells: delta={heap_delta}"
        );
    });
}

#[test]
fn array_length165_f06_store_host_assert() {
    with_stack(|| {
        let mut machine = pd_vm_compat_frontends::compile_javascript_es5_source(
            "var a=[1];a.length=13105;a[13104]=1;",
        )
        .expect("public CLI compile");
        machine.set_fuel(10_000_000);
        let error = machine
            .run()
            .expect_err("store past the physical extent must host-halt");
        let text = format!("{error:?}");
        println!("F06_STORE_HOST_ASSERT error={text}");
        assert!(
            text.contains("assertion failed"),
            "extent store must be the host assertion, got {text}"
        );
        assert!(
            !text.contains("RangeError"),
            "extent store must not be reported as RangeError, got {text}"
        );
    });
}

#[test]
fn array_length165_f10_collection_count() {
    with_stack(|| {
        let source = "var a=[\"\"+\"kept\",{v:7}];var t={};var i=0;while(i<40){t={k:i};i=i+1;}a[0]===\"kept\"&&a[1].v===7;";
        let (stack, _) = run_observed(source, true);
        assert!(
            stack.len() >= 9,
            "guest result, collect before/ack/after, and five observes, stack={}",
            stack.len()
        );
        let guest_at = stack.len() - 9;
        let before = int_at(&stack, guest_at + 1);
        let after = int_at(&stack, guest_at + 3);
        let survived = guest_flag(&stack[guest_at]);
        let heap_end = int_at(&stack, stack.len() - 5);
        let active_root_capacity = int_at(&stack, stack.len() - 4);
        let collections = int_at(&stack, stack.len() - 3);
        let node_cells = int_at(&stack, stack.len() - 2);
        let backing_length = int_at(&stack, stack.len() - 1);
        println!(
            "F10_COLLECTION_COUNT before={} after={} delta={} heap_end={} active_root_capacity={} collections={} node_cells={} backing_length={}",
            before,
            after,
            after - before,
            heap_end,
            active_root_capacity,
            collections,
            node_cells,
            backing_length
        );
        assert!(survived, "array-only roots must survive the forced collection");
        assert_eq!(
            after,
            before + 1,
            "forced collection must increment host collection_count by 1"
        );
        assert_eq!(collections, after, "tail collection_count");
    });
}

fn summarize(value: &Value) -> String {
    let text = format!("{value:?}");
    if text.len() > 500 {
        format!("{}...", &text[..500])
    } else {
        text
    }
}

#[test]
fn array_length165_resource_stack_shape() {
    with_stack(|| {
        let (stack, _) = run_observed("var a=[1];a[0]===1;", false);
        println!("STACK_LEN {}", stack.len());
        for (index, value) in stack.iter().enumerate().rev().take(12) {
            println!("STACK {index} {}", summarize(value));
        }
        let (flag, counters) = tail_counters(&stack);
        assert!(flag, "tiny guest flag");
        assert!(counters.heap_end > 0, "heap_end");
        assert!(counters.active_root_capacity > 0, "host capacity");
    });
}

#[test]
fn array_length165_resource_collect_shape() {
    with_stack(|| {
        let (stack, _) = run_observed("var a=[1];a[0]===1;", true);
        println!("COLLECT_STACK_LEN {}", stack.len());
        for (index, value) in stack.iter().enumerate().rev().take(16) {
            println!("COLLECT_STACK {index} {}", summarize(value));
        }
    });
}

