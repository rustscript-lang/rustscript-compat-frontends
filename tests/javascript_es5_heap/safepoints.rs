//! Explicit safepoint and actual source-resource isolation observations.
use super::*;

const FORCED_SOURCE: &str = "var a=[7,8];var aa=a;var f=function(){\"use strict\";return \"\\uD800\\0\";};var g=function(){\"use strict\";return this;};var o={n:7,t:true,f:false,m:g};var r=o.m();var nested={a:{v:r.n},b:{s:f()},t:r.t,f:r.f};var n=nested.a.v;var yes=nested.t;var no=nested.f;var ar=aa[1];var s=nested.b.s;var eq=s===\"\\uD800\\0\";";
struct ForcedPlugin;
impl vm::SourcePlugin for ForcedPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-forced-gc"),
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
        let reads = ["n", "yes", "no", "ar", "eq"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let mut ir = HeapReadbackPlugin { reads }.parse_source(source, context)?;
        // Test-only scheduling instrumentation: set the existing allocation
        // quantum at entry, then execute the entire unmodified production
        // node_begin body. Thus EVERY allocation is an actual GC safepoint,
        // including receiver->key, initializer keys/values and return calls.
        let begin = heap_helper_index(&ir, HEAP_NODE_BEGIN_NAME)?;
        let write = heap_helper_index(&ir, HEAP_WRITE_NAME)?;
        let ack = heap_cycle_take(&mut ir, "forced-gc")?;
        let body = ir
            .function_impls
            .get_mut(&begin)
            .ok_or_else(|| heap_rejection("actual node_begin implementation".to_owned()))?;
        let carrier = body.param_slots[0];
        body.body_stmts.insert(
            0,
            vm::Stmt::Let {
                index: ack,
                declared_schema: Some(vm::TypeSchema::Number),
                expr: heap_guest_call(
                    write,
                    vec![
                        vm::Expr::Var(carrier),
                        vm::Expr::Float(10.0),
                        vm::Expr::Float(32.0),
                    ],
                ),
                line: 1,
            },
        );
        Ok(ir)
    }
}
fn forced_run(native: bool) {
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        FORCED_SOURCE,
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-forced-gc")),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(&ForcedPlugin)
            .with_host_api_catalog(heap_catalog()),
    )
    .expect("source with every allocation safepointed");
    let mut machine = compilation.into_vm().expect("certified forced VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("B1");
    transaction_registry()
        .bind_vm_cached(&mut machine)
        .expect("bind");
    machine.set_fuel(100_000_000);
    if native {
        machine.compile_aot().expect("forced source native SSA");
    }
    assert_eq!(
        machine.run().expect("actual forced-GC source execution"),
        VmStatus::Halted
    );
    let stack = machine.stack();
    let tail = &stack[stack.len() - 8..];
    assert_eq!(heap_box_number(&tail[0]), 7.0);
    assert_eq!(heap_box_number(&tail[3]), 8.0);
    for (index, flag) in [(1, true), (2, false), (4, true)] {
        assert_eq!(heap_box_tag(&tail[index]), 2);
        let Value::Map(fields) = &tail[index] else {
            unreachable!()
        };
        assert_eq!(fields.get(&Value::string("flag")), Some(&Value::Bool(flag)));
    }
    let counters = heap_stale_ints(&tail[5..]);
    // Source-derived allocation count: initial array/functions/object8,
    // method key1, nested construction13, numeric read2, Bool reads2,
    // string member2 and equality RHS1 =29 real reservations.
    assert_eq!(
        counters[0], 29,
        "every actual reservation must collect: {counters:?}"
    );
    let raw = backing_snapshot(&mut machine);
    let cells = heap_stale_ints(&raw);
    let mut found = false;
    let mut cursor = 80usize;
    while cursor < cells.len() {
        let size = cells[cursor] as usize;
        assert!(size >= 4);
        if cells[cursor + 1] == 1
            && cells[cursor + 3] == 2
            && cells[cursor + 5] == 2
            && cells[cursor + 6..cursor + 8] == [0xD800, 0]
        {
            found = true;
        }
        cursor += size;
    }
    assert!(
        found,
        "exact UTF16/NUL source payload remains in actual live backing"
    );
    let count = machine.aot_exec_count();
    eprintln!("P2_FORCED_SAFEPOINT native={native} counters={counters:?} count={count}");
    if native {
        let info = machine.dump_aot_info();
        eprintln!("{info}");
        assert!(count > 0 && info.contains("lowering=ssa"));
    }
    machine.reset_for_reuse().expect("reset forced VM");
    assert_eq!(machine.host_context().resource_count(), 0);
}
#[test]
fn p2_forced_every_allocation_preserves_source_payloads() {
    forced_run(false);
}
#[test]
fn p2_forced_every_allocation_preserves_source_payloads_native() {
    forced_run(true);
}

#[test]
fn p2_actual_js_resource_is_rejected_by_foreign_vm_without_mutation() {
    let make = |number| {
        let source = format!("var a=[{number}];a[0];");
        let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
            &source,
            pd_vm_compat_frontends::javascript_es5_flavor(),
            vm::CompileSourceFileOptions::new()
                .with_source_plugin(&JAVASCRIPT_ES5_PLUGIN)
                .with_host_api_catalog(heap_catalog()),
        )
        .expect("independent JS source");
        let mut machine = compilation.into_vm().expect("certified VM");
        pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
            &mut machine,
            pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
        )
        .expect("B1");
        transaction_registry()
            .bind_vm_cached(&mut machine)
            .expect("bind");
        machine.set_fuel(10_000_000);
        assert_eq!(machine.run().expect("actual source"), VmStatus::Halted);
        assert_eq!(
            heap_box_number(machine.stack().last().unwrap()),
            number as f64
        );
        let handle = LAST_HANDLE.with(|v| {
            v.borrow()
                .clone()
                .expect("actual resource handle from typed source read")
        });
        let payload = backing_snapshot(&mut machine);
        (machine, handle, payload)
    };
    let (mut first, handle_a, payload_a) = make(7);
    let (mut second, handle_b, payload_b) = make(9);
    let before =
        pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut second).unwrap();
    let error = ORIGINAL_READ.get().unwrap()(&mut second, &[handle_a.clone(), Value::Float(0.0)])
        .expect_err("actual foreign JS resource must refuse at typed arena boundary");
    eprintln!("P2_JS_FOREIGN_RESOURCE {error:?}");
    assert!(
        format!("{error:?}").contains("arena"),
        "exact foreign-arena refusal: {error:?}"
    );
    LAST_HANDLE.with(|v| *v.borrow_mut() = Some(handle_b));
    assert_eq!(backing_snapshot(&mut second), payload_b);
    assert_eq!(
        pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut second),
        Some(before)
    );
    LAST_HANDLE.with(|v| *v.borrow_mut() = Some(handle_a));
    assert_eq!(backing_snapshot(&mut first), payload_a);
    first.reset_for_reuse().expect("reset A");
    assert_eq!(first.host_context().resource_count(), 0);
    assert_eq!(second.host_context().resource_count(), 1);
    second.reset_for_reuse().expect("reset B");
    assert_eq!(second.host_context().resource_count(), 0);
}
