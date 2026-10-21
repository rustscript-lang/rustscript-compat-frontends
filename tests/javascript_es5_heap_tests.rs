//! P2-B managed logical heap proofs for the javascript-es5 frontend.
//!
//! `p2_heap_history_*` is the pre-product behavioral RED: a real source
//! zero-argument object factory through its real existing dispatch/static
//! compiled body, plus a private bounded IR history driver (64 vs 4096
//! iterations) with the existing B1 length observation. The original
//! append-only producer failed the history-independent warmed bound in the
//! preserved RED receipt. Current fixtures also dereference actual payloads
//! and independently observe logical, physical, B1 and Core metadata.

use pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN;
use vm::{Value, VmStatus};

#[path = "javascript_es5_heap/acceptance.rs"]
mod acceptance;

// ---- Shared realm harness (production catalog/adapters, no JS oracle) ----

fn heap_catalog() -> std::sync::Arc<vm::HostApiCatalog> {
    pd_vm_compat_frontends::reference_array_effective_catalog()
}

fn install_heap_production(registry: &mut vm::HostFunctionRegistry, catalog: &vm::HostApiCatalog) {
    pd_vm_compat_frontends::runtime::array::typescript_array_host_module()
        .install_from_catalog(registry, catalog)
        .expect("manual arrays adapters should match the effective catalog");
    pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module()
        .install_from_catalog(registry, catalog)
        .expect("owned adapters should match the effective catalog");
    pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module()
        .install_from_catalog(registry, catalog)
        .expect("snapshot adapter should match the effective catalog");
    pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module()
        .install_from_catalog(registry, catalog)
        .expect("scalar-math adapter should match the effective catalog");
    registry
        .allow_builtin("math::floor")
        .expect("math::floor should be allow-listable");
}

// ---- RED driver: bounded factory-call history with B1 length readback ----
// Delegates the original real JS source to the REAL JS plugin (real B1
// prelude, carrier, source policy, catalog and certification) and appends a
// bounded `While` loop replaying the real `var o = f();` dispatch region
// (same slots as `Assign`s, no new allocation sites) plus one genuine B1
// length observation through the EXISTING length helper on the ACTUAL
// carrier discovered structurally from real JS lowering. No synthetic
// allocator, no new helper/schema: this must RED on append-only storage.

const HEAP_HISTORY_SOURCE: &str = "var keep={n:7,t:true,f:false,s:\"\\uD800\\0\"};var alias=keep;var a=[7,8];var f=function(){\"use strict\";return {n:7};};var o=f();var r=o.n;var old=alias.n;var st=alias.s;var bt=alias.t;var bf=alias.f;var av=a[1];";

const HEAP_MAKE_NAME: &str = "__compat_ref_make";
const HEAP_LENGTH_NAME: &str = "__compat_ref_length";

/// History-independent warmed bound, in backing cells: one spare directory
/// page (64 rows x 7 cells + 3 header = 451), at most 32 raw 15-cell
/// one-property objects, and a spare 4-entry source frame (4+3*4=16):947
/// cells rounded up to the original1024 ceiling before measurement.
/// Append-only growth over 4032 extra factory calls exceeds this by an
/// order of magnitude (each `{}` commits a fresh multi-cell extent).
const HEAP_HISTORY_BOUND_CELLS: f64 = 1024.0;

fn heap_rejection(message: String) -> vm::ParseError {
    vm::ParseError::at_span(vm::compiler::source_map::Span::new(0, 0, 0), message)
}

fn heap_helper_index(ir: &vm::FrontendIr, name: &str) -> Result<u16, vm::ParseError> {
    ir.functions
        .iter()
        .find(|f| f.name == name)
        .map(|f| f.index)
        .ok_or_else(|| heap_rejection(format!("heap fixture requires helper '{name}'")))
}

struct HeapHistoryPlugin {
    iters: u64,
}

/// The real bare-call sequence starts at its callee snapshot and ends
/// after binding refresh and result-temp cleanup. Invocation-owned roots
/// now clear inside the call block, after the completion handoff.
fn heap_call_replay_bounds(
    ir: &vm::FrontendIr,
    store: usize,
) -> Result<(usize, usize), vm::ParseError> {
    let dispatch = heap_helper_index(ir, "__compat_es5_dispatch")?;
    let root_write = heap_helper_index(ir, HEAP_ROOT_WRITE_NAME)?;
    let callee = ir.stmts[..store]
        .iter()
        .rev()
        .find_map(|stmt| match stmt {
            vm::Stmt::Let {
                expr: vm::Expr::Call(target, _, args, _, _),
                ..
            } if *target == dispatch => match args.get(1) {
                Some(vm::Expr::Var(slot)) => Some(*slot),
                _ => None,
            },
            _ => None,
        })
        .ok_or_else(|| heap_rejection("actual call dispatch and callee".to_owned()))?;
    let start = ir.stmts[..store]
        .iter()
        .rposition(|stmt| matches!(stmt, vm::Stmt::Let { index, .. } if *index == callee))
        .ok_or_else(|| heap_rejection("actual callee snapshot".to_owned()))?;
    let mut end = store + 1;
    while matches!(ir.stmts.get(end), Some(vm::Stmt::Let {
        expr: vm::Expr::Call(target, _, _, _, _), ..
    }) if *target == root_write)
    {
        end += 1;
    }
    let callee_entry = ir.stmts[start..store]
        .iter()
        .find_map(|stmt| match stmt {
            vm::Stmt::Let {
                expr: vm::Expr::Call(target, _, args, _, _),
                ..
            } if *target == root_write
                && matches!(args.get(3), Some(vm::Expr::Var(slot)) if *slot == callee) =>
            {
                match args.get(2) {
                    Some(vm::Expr::Int(entry)) => Some(*entry),
                    _ => None,
                }
            }
            _ => None,
        })
        .ok_or_else(|| heap_rejection("actual saved-callee root entry".to_owned()))?;
    let cleared_in_call = ir.stmts[start..store].iter().any(|stmt| {
        matches!(stmt,
            vm::Stmt::Let {expr: vm::Expr::Call(target, _, args, _, _), ..}
            if *target == root_write
                && matches!(args.get(2), Some(vm::Expr::Int(entry)) if *entry == callee_entry)
                && matches!(args.get(3), Some(vm::Expr::Call(..)))
        )
    });
    if !cleared_in_call || end < store + 3 {
        return Err(heap_rejection(
            "in-call callee cleanup, binding refresh and result cleanup required".to_owned(),
        ));
    }
    Ok((start, end))
}

fn append_heap_history_driver(ir: &mut vm::FrontendIr, iters: u64) -> Result<(), vm::ParseError> {
    let make = heap_helper_index(ir, HEAP_MAKE_NAME)?;
    let length = heap_helper_index(ir, HEAP_LENGTH_NAME)?;
    let root_enter = heap_helper_index(ir, HEAP_ROOT_ENTER_NAME)?;
    let max_source_capacity = ir
        .functions
        .iter()
        .filter(|function| function.name.starts_with("__compat_es5_fn_"))
        .filter_map(|function| ir.function_impls.get(&function.index))
        .flat_map(|implementation| &implementation.body_stmts)
        .filter_map(|stmt| match stmt {
            vm::Stmt::Let {
                expr: vm::Expr::Call(target, _, args, _, _),
                ..
            } if *target == root_enter => match args.get(1) {
                Some(vm::Expr::Int(capacity)) => Some(*capacity),
                _ => None,
            },
            _ => None,
        })
        .max()
        .ok_or_else(|| {
            heap_rejection("history requires the actual compiled source frame".to_owned())
        })?;
    let derived_slack = 3 + 64 * 7 + 32 * (3 + 5 + 7) + 4 + 3 * max_source_capacity;
    if derived_slack as f64 > HEAP_HISTORY_BOUND_CELLS {
        return Err(heap_rejection(format!(
            "source-derived history bound {derived_slack} exceeds original1024"
        )));
    }
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
        return Err(heap_rejection(format!(
            "heap fixture requires exactly one realm carrier, found {}",
            carriers.len()
        )));
    };
    let carrier = *carrier;
    // The real `o` binding: a top-level `Let` whose value is the dispatched
    // call block. Every inner statement must be a plain `Let` so the loop
    // can replay the identical emission code with `Assign`s on the same
    // slots (balanced stacks on loop edges, no new sites).
    let binding = ir
        .local_bindings
        .iter()
        .rfind(|(name, _)| name == "o")
        .map(|(_, slot)| *slot)
        .ok_or_else(|| heap_rejection("heap fixture requires the real `o` binding".to_owned()))?;
    let boxed_at = ir
        .stmts
        .iter()
        .position(|stmt| matches!(stmt, vm::Stmt::Assign { index, .. } if *index == binding))
        .ok_or_else(|| heap_rejection("heap fixture requires the real `o` store".to_owned()))?;
    let tail = match &ir.stmts[boxed_at] {
        vm::Stmt::Assign { expr, .. } => expr.clone(),
        other => {
            return Err(heap_rejection(format!(
                "heap fixture requires the dispatched `o` store, got {other:?}"
            )));
        }
    };
    let (run_start, suffix_start) = heap_call_replay_bounds(ir, boxed_at)?;
    let inner = ir.stmts[run_start..boxed_at].to_vec();
    let mut replay = Vec::with_capacity(inner.len() + 2);
    for stmt in &inner {
        match stmt {
            vm::Stmt::Let {
                index, expr, line, ..
            } => replay.push(vm::Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: *index,
                expr: expr.clone(),
                line: *line,
            }),
            vm::Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                ..
            } => replay.push(stmt.clone()),
            other => {
                return Err(heap_rejection(format!(
                    "heap fixture replays only Let bindings, got {other:?}"
                )));
            }
        }
    }
    replay.push(vm::Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: binding,
        expr: tail,
        line: 1,
    });
    for stmt in &ir.stmts[boxed_at + 1..suffix_start] {
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
    let suffix = ir.stmts.split_off(suffix_start);
    let mut fresh = || -> Result<u16, vm::ParseError> {
        let slot = u16::try_from(ir.locals)
            .map_err(|_| heap_rejection("heap fixture slot space exhausted".to_owned()))?;
        ir.locals += 1;
        Ok(slot)
    };
    let iter = fresh()?;
    let observed = fresh()?;
    ir.stmts.push(vm::Stmt::Let {
        index: iter,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: vm::Expr::Int(0),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::While {
        condition: vm::Expr::Lt(
            Box::new(vm::Expr::Div(
                Box::new(vm::Expr::Var(iter)),
                Box::new(vm::Expr::Float(1.0)),
            )),
            Box::new(vm::Expr::Float(iters as f64)),
        ),
        body: {
            let mut body = replay;
            body.push(vm::Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: iter,
                expr: vm::Expr::Add(Box::new(vm::Expr::Var(iter)), Box::new(vm::Expr::Int(1))),
                line: 1,
            });
            body
        },
        line: 1,
    });
    ir.stmts.extend(suffix);
    for name in ["r", "old", "bt", "bf", "av"] {
        let slot = heap_binding_index(ir, name)?;
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(slot),
            line: 1,
        });
    }
    let mut ctx = heap_cycle_ctx(ir)?;
    ctx.carrier = carrier;
    let st = heap_binding_index(ir, "st")?;
    let (_, st_slot, st_gen) = heap_cycle_identity(ir, &ctx, st, "history-string")?;
    let st_base = heap_cycle_base(ir, &ctx, carrier, st_slot, st_gen, "history-string")?;
    for offset in [2, 3, 4] {
        let raw = heap_cycle_read_number(
            ir,
            &ctx,
            carrier,
            vm::Expr::Add(
                Box::new(vm::Expr::Var(st_base)),
                Box::new(vm::Expr::Int(offset)),
            ),
            "history-string",
        )?;
        heap_cycle_decode(ir, &ctx, raw, "history-string")?;
    }
    heap_cycle_collect(ir, &ctx, carrier, "history-final")?;
    for which in 0..=12 {
        heap_cycle_observe(ir, &ctx, carrier, which, 0, "history-final")?;
    }
    // Existing B1 length observation on the actual carrier: the warmed
    // history bound compares these lengths, never a synthetic allocator.
    ir.stmts.push(vm::Stmt::Let {
        index: observed,
        declared_schema: Some(vm::TypeSchema::Number),
        expr: vm::Expr::Call(length, Vec::new(), vec![vm::Expr::Var(carrier)], None, None),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::Var(observed),
        line: 1,
    });
    Ok(())
}

impl vm::SourcePlugin for HeapHistoryPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-heap-history"),
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
        append_heap_history_driver(&mut ir, self.iters)?;
        Ok(ir)
    }
}

#[derive(Debug)]
struct HeapHistoryEvidence {
    guest: Vec<i64>,
    backing: f64,
    capacity: usize,
    capture_entries: usize,
    capture_capacity: usize,
    identity_entries: usize,
    identity_capacity: usize,
}

fn heap_history_run(iters: u64, native: bool) -> HeapHistoryEvidence {
    heap_history_run_source(HEAP_HISTORY_SOURCE, iters, native)
}

fn heap_history_run_source(source: &str, iters: u64, native: bool) -> HeapHistoryEvidence {
    let plugin: &'static HeapHistoryPlugin = Box::leak(Box::new(HeapHistoryPlugin { iters }));
    let catalog = heap_catalog();
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-heap-history")),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(plugin)
            .with_host_api_catalog(std::sync::Arc::clone(&catalog)),
    )
    .expect("real history source must compile");
    let mut machine = compilation.into_vm().expect("certified history VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("initialize B1");
    let mut registry = vm::HostFunctionRegistry::restricted();
    install_heap_production(&mut registry, &catalog);
    registry
        .bind_vm_cached(&mut machine)
        .expect("bind production");
    machine.set_fuel(1_000_000_000);
    if native {
        eprintln!("P2_HEAP_NATIVE_PHASE compile_start iters={iters}");
        let started = std::time::Instant::now();
        machine
            .compile_aot()
            .expect("real history native compilation");
        eprintln!(
            "P2_HEAP_NATIVE_PHASE compile_done iters={iters} elapsed={:?}",
            started.elapsed()
        );
    }
    let run_started = std::time::Instant::now();
    if native {
        eprintln!("P2_HEAP_NATIVE_PHASE run_start iters={iters}");
    }
    assert_eq!(
        machine.run().expect("real history must execute"),
        VmStatus::Halted
    );
    if native {
        eprintln!(
            "P2_HEAP_NATIVE_PHASE run_done iters={iters} elapsed={:?}",
            run_started.elapsed()
        );
        let count = machine.aot_exec_count();
        let info = machine.dump_aot_info();
        eprintln!("P2_HEAP_HISTORY_NATIVE iters={iters} count={count}\n{info}");
        assert!(count > 0 && info.contains("lowering=ssa"));
    }
    // Five actual source consumer values, st slot/gen, UTF16 length/units,
    // thirteen independent guest observations and the real B1 length.
    let stack = machine.stack();
    assert!(stack.len() >= 24, "complete history readback: {stack:?}");
    let tail = &stack[stack.len() - 24..];
    assert_eq!(
        heap_box_number(&tail[0]),
        7.0,
        "latest returned object payload"
    );
    assert_eq!(
        heap_box_number(&tail[1]),
        7.0,
        "old surviving alias payload"
    );
    for (index, flag) in [(2, true), (3, false)] {
        assert_eq!(heap_box_tag(&tail[index]), 2);
        let Value::Map(fields) = &tail[index] else {
            unreachable!()
        };
        assert_eq!(fields.get(&Value::string("flag")), Some(&Value::Bool(flag)));
    }
    assert_eq!(
        heap_box_number(&tail[4]),
        8.0,
        "globally retained array payload"
    );
    assert_eq!(
        heap_stale_ints(&tail[7..10]),
        [2, 0xD800, 0],
        "exact UTF16/NUL after history"
    );
    let guest = heap_stale_ints(&tail[10..23]);
    let backing = match tail[23] {
        Value::Float(n) => n,
        Value::Int(n) => n as f64,
        ref other => panic!("B1 length {other:?}"),
    };
    assert_eq!(guest[3] as f64, backing);
    assert_eq!(guest[10], guest[3]);
    assert_eq!(
        guest[0] + guest[1] + guest[8] + guest[11],
        guest[2],
        "all directory states accounted"
    );
    assert_eq!((guest[1], guest[8], guest[6]), (0, 0, 0));
    assert!(guest[11] > 0 && guest[5] > 0 && guest[12] > 0);
    let capture = machine
        .capture_certification_stats()
        .expect("capture metadata");
    let identity = machine
        .callable_identity_maintenance_stats()
        .expect("independent callable metadata");
    assert!(capture.valid);
    assert_eq!(machine.host_context().resource_count(), 1);
    let b1 = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
        .expect("B1 metadata");
    assert_eq!((b1.pending, b1.registered), (0, 1));
    let capacity = b1.reserved_bytes / std::mem::size_of::<Value>();
    assert!(capacity >= backing as usize);
    let evidence = HeapHistoryEvidence {
        guest,
        backing,
        capacity,
        capture_entries: capture.entries,
        capture_capacity: capture.capacity,
        identity_entries: identity.entries,
        identity_capacity: identity.capacity,
    };
    eprintln!(
        "P2_HEAP_HISTORY_EVIDENCE iters={iters} native={native} evidence={evidence:?} b1={b1:?} capture={capture:?} identity={identity:?}"
    );
    machine.reset_for_reuse().expect("reset real history VM");
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
    evidence
}

fn heap_assert_history_bound(short: HeapHistoryEvidence, long: HeapHistoryEvidence) {
    // Original 1024-cell differential ceiling is retained. Capacity uses the
    // B1 Vec growth factor on that same layout bound, not a global heap cap.
    assert!(long.backing <= short.backing + HEAP_HISTORY_BOUND_CELLS);
    assert!(long.capacity <= 2 * (short.backing as usize + HEAP_HISTORY_BOUND_CELLS as usize));
    for which in [0, 1, 4, 7, 8, 12] {
        assert_eq!(
            long.guest[which], short.guest[which],
            "history-invariant selector {which}"
        );
    }
    assert!(long.guest[2] <= short.guest[2] + 64);
    assert!(long.guest[9] <= short.guest[9] + 1);
    assert!(long.guest[5] > short.guest[5]);
    assert_eq!(
        (long.capture_entries, long.capture_capacity),
        (short.capture_entries, short.capture_capacity)
    );
    assert_eq!(
        (long.identity_entries, long.identity_capacity),
        (short.identity_entries, short.identity_capacity)
    );
}

#[test]
fn p2_heap_history_reuses_backing_across_factory_calls() {
    heap_assert_history_bound(heap_history_run(64, false), heap_history_run(4096, false));
}

#[test]
fn p4_argument_history_retains_aliases_and_reclaims_unused_extras() {
    let source = "var keep={n:7,t:true,f:false,s:\"\\uD800\\0\"};var alias=keep;var a=[7,8];var f=function(x){\"use strict\";var local=[9];return x;};var o=f({n:7},\"unused\");var r=o.n;var old=alias.n;var st=alias.s;var bt=alias.t;var bf=alias.f;var av=a[1];";
    heap_assert_history_bound(
        heap_history_run_source(source, 64, false),
        heap_history_run_source(source, 4096, false),
    );
}

#[test]
fn p2_heap_history_reuses_backing_native_ssa() {
    // One native compilation per finite command. The separate native64
    // fixture preserves the former warm phase; its complete receipt is
    // compared independently with this native256 receipt by the recorder.
    heap_assert_history_bound(heap_history_run(64, false), heap_history_run(256, true));
}

#[test]
fn helper_heap_history_native_warm64() {
    // Extracted warm phase, not an additional language feature or business
    // count. All actual payload/UTF16/B1/Core assertions execute in run.
    heap_history_run(64, true);
}

// ---- Safepoint readback harness (test-only IR instrumentation, labeled) ----
// Parses real admitted source through the production plugin, then appends
// plain value reads of the named source bindings plus genuine
// `__compat_es5_heap_observe` counter observations. Every allocation in
// the source is production; only the trailing readback is fixture IR.

const HEAP_OBSERVE_NAME: &str = "__compat_es5_heap_observe";

/// Observe selectors: 5=collections, 0=live_nodes, 3=heap_end.
const HEAP_SAFEPOINT_OBSERVES: [i64; 3] = [5, 0, 3];

struct HeapReadbackPlugin {
    reads: Vec<String>,
}

fn heap_binding_index(ir: &vm::FrontendIr, name: &str) -> Result<u16, vm::ParseError> {
    ir.local_bindings
        .iter()
        .rfind(|(binding, _)| binding == name)
        .map(|(_, slot)| *slot)
        .ok_or_else(|| heap_rejection(format!("heap fixture requires binding `{name}'")))
}

impl vm::SourcePlugin for HeapReadbackPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-heap-readback"),
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
        for name in &self.reads {
            let slot = heap_binding_index(&ir, name)?;
            ir.stmts.push(vm::Stmt::Expr {
                expr: vm::Expr::Var(slot),
                line: 1,
            });
        }
        let observe = heap_helper_index(&ir, HEAP_OBSERVE_NAME)?;
        let mut carriers = Vec::new();
        for stmt in &ir.stmts {
            if let vm::Stmt::Let {
                index,
                declared_schema: None,
                expr: vm::Expr::Call(target, _, args, _, _),
                ..
            } = stmt
                && *target == heap_helper_index(&ir, HEAP_MAKE_NAME)?
                && args.len() == 1
            {
                carriers.push(*index);
            }
        }
        let [carrier] = carriers.as_slice() else {
            return Err(heap_rejection(format!(
                "heap fixture requires exactly one realm carrier, found {}",
                carriers.len()
            )));
        };
        let carrier = *carrier;
        let mut fresh = || -> Result<u16, vm::ParseError> {
            let slot = u16::try_from(ir.locals)
                .map_err(|_| heap_rejection("heap fixture slot space exhausted".to_owned()))?;
            ir.locals += 1;
            Ok(slot)
        };
        for which in HEAP_SAFEPOINT_OBSERVES {
            let out = fresh()?;
            ir.stmts.push(vm::Stmt::Let {
                index: out,
                declared_schema: Some(vm::TypeSchema::Int),
                expr: vm::Expr::Call(
                    observe,
                    Vec::new(),
                    vec![
                        vm::Expr::Var(carrier),
                        vm::Expr::Int(which),
                        vm::Expr::Int(0),
                    ],
                    None,
                    None,
                ),
                line: 1,
            });
            ir.stmts.push(vm::Stmt::Expr {
                expr: vm::Expr::Var(out),
                line: 1,
            });
        }
        Ok(ir)
    }
}

fn heap_readback_run(source: &str, reads: Vec<String>) -> (Vec<Value>, i64, i64, i64) {
    let plugin: &'static HeapReadbackPlugin = Box::leak(Box::new(HeapReadbackPlugin {
        reads: reads.clone(),
    }));
    let catalog = heap_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_source_plugin(plugin)
        .with_host_api_catalog(std::sync::Arc::clone(&catalog));
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-heap-readback")),
        options,
    )
    .expect("safepoint compilation should succeed");
    let mut machine = compilation
        .into_vm()
        .expect("certified compilation should create a source-certified VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = vm::HostFunctionRegistry::restricted();
    install_heap_production(&mut registry, &catalog);
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind to this VM");
    match machine.run() {
        Ok(status) => assert_eq!(status, VmStatus::Halted, "safepoint program should halt"),
        Err(error) => panic!(
            "SAFEPOINT RUN-FAIL ip={} error={error:?} stack={:?} frames={:?}",
            machine.ip(),
            machine.stack(),
            machine.execution_frames()
        ),
    }
    let stack = machine.stack().to_vec();
    assert!(
        stack.len() >= reads.len() + HEAP_SAFEPOINT_OBSERVES.len(),
        "binding readback plus observations"
    );
    let values = stack[stack.len() - reads.len() - HEAP_SAFEPOINT_OBSERVES.len()
        ..stack.len() - HEAP_SAFEPOINT_OBSERVES.len()]
        .to_vec();
    let tail = &stack[stack.len() - HEAP_SAFEPOINT_OBSERVES.len()..];
    let counter = |v: &Value| match v {
        Value::Int(n) => *n,
        Value::Float(n) => *n as i64,
        other => panic!("observe must return Int, got {other:?}"),
    };
    (
        values,
        counter(&tail[0]),
        counter(&tail[1]),
        counter(&tail[2]),
    )
}

fn heap_box_tag(v: &Value) -> i64 {
    if let Value::Map(fields) = v
        && let Some(Value::Int(tag)) = fields.get(&Value::string("tag"))
    {
        return *tag;
    }
    panic!("expected a boxed JsValue, got {v:?}");
}

fn heap_box_number(v: &Value) -> f64 {
    assert_eq!(heap_box_tag(v), 3, "expected NUMBER, got {v:?}");
    if let Value::Map(fields) = v {
        return match fields.get(&Value::string("number")) {
            Some(Value::Float(n)) => *n,
            Some(Value::Int(n)) => *n as f64,
            other => panic!("NUMBER must carry a numeric payload, got {other:?}"),
        };
    }
    unreachable!();
}

#[test]
fn p2_heap_safepoint_object_values_across_collects() {
    // 61 production allocations inside one object literal (40 keys, 10
    // nested objects with their keys, the containing object) plus 50 key
    // allocations across the readback sequence force repeated interval
    // collections mid-literal and mid-reads. Every key/value/receiver root
    // must survive; payloads must read back exactly.
    let mut source = String::from("var s = \"hi\"; var o = {");
    for i in 0..10 {
        source.push_str(&format!("k{i}: {{v: {i}}}, "));
    }
    for i in 10..40 {
        source.push_str(&format!("k{i}: {i}, "));
    }
    source.push_str("}; ");
    let mut reads = vec!["s".to_owned()];
    let mut expected: Vec<f64> = Vec::new();
    for i in 0..10 {
        source.push_str(&format!("var t{i} = o.k{i}.v; "));
        reads.push(format!("t{i}"));
        expected.push(i as f64);
    }
    for i in 10..40 {
        source.push_str(&format!("var t{i} = o.k{i}; "));
        reads.push(format!("t{i}"));
        expected.push(i as f64);
    }
    let (values, collections, live, heap_end) = heap_readback_run(&source, reads);
    eprintln!("P2_SAFEPOINT_OBJECT collections={collections} live={live} heap_end={heap_end}");
    assert!(
        collections >= 2,
        "literal plus reads must span collects, got {collections}"
    );
    assert_eq!(
        heap_box_tag(&values[0]),
        4,
        "string node must survive, got {:?}",
        values[0]
    );
    for (position, want) in expected.iter().enumerate() {
        let got = heap_box_number(&values[position + 1]);
        assert_eq!(got, *want, "readback t{position} must be {want}, got {got}");
    }
}

#[test]
fn p2_heap_safepoint_bare_call_results_across_collects() {
    // 41 production allocations (one function plus 40 fresh objects
    // returned through real dispatch/Completion handoff) force an
    // interval collection mid-sequence. Every caller-pinned result,
    // including the earliest, must stay a valid OBJECT afterwards.
    let mut source = String::from("var f = function() { \"use strict\"; return {}; }; ");
    let mut reads = Vec::new();
    for i in 0..40 {
        source.push_str(&format!("var o{i} = f(); "));
        reads.push(format!("o{i}"));
    }
    let (values, collections, live, heap_end) = heap_readback_run(&source, reads);
    eprintln!("P2_SAFEPOINT_CALLS collections={collections} live={live} heap_end={heap_end}");
    assert!(
        collections >= 1,
        "40 calls must span a collect, got {collections}"
    );
    assert!(live >= 41, "function plus 40 results stay live, got {live}");
    for (position, value) in values.iter().enumerate() {
        assert_eq!(
            heap_box_tag(value),
            5,
            "result o{position} must stay OBJECT, got {value:?}"
        );
    }
}

#[test]
fn p2_heap_safepoint_method_this_across_collects() {
    // Receiver, key, callee, `this` and the nested member read inside the
    // body stay rooted while 16 method calls (2 key allocations each plus
    // the 4-node fixture) force an interval collection mid-sequence.
    let mut source =
        String::from("var o = {n: 7, m: function() { \"use strict\"; return this.n; }}; ");
    let mut reads = Vec::new();
    for i in 0..16 {
        source.push_str(&format!("var r{i} = o.m(); "));
        reads.push(format!("r{i}"));
    }
    let (values, collections, live, heap_end) = heap_readback_run(&source, reads);
    eprintln!("P2_SAFEPOINT_METHOD collections={collections} live={live} heap_end={heap_end}");
    assert!(
        collections >= 1,
        "16 method calls must span a collect, got {collections}"
    );
    for (position, value) in values.iter().enumerate() {
        let got = heap_box_number(value);
        assert_eq!(got, 7.0, "method result r{position} must be 7, got {got}");
    }
}

// ---- Stale-generation harness (test-only IR instrumentation, labeled) ----
// Single production allocation, then in-fixture: stash the live box,
// replay the real binding emission (unrooting the first node), run a real
// collection, replay once more (reusing the freed slot with a bumped
// generation), publish the old/new slot+generation evidence, and finally
// dereference either the stale or the fresh box through the production
// `__compat_es5_checked_base` gate. Stale must refuse; fresh must work.

const HEAP_DECODE_INT_NAME: &str = "__compat_es5_decode_int";
const HEAP_CHECKED_BASE_NAME: &str = "__compat_es5_checked_base";
const HEAP_COLLECT_NAME: &str = "__compat_es5_heap_collect";
const HEAP_NODE_BEGIN_NAME: &str = "__compat_es5_node_begin";
const HEAP_ROOT_ENTER_NAME: &str = "__compat_es5_root_enter";
const HEAP_ROOT_WRITE_NAME: &str = "__compat_es5_root_write";

struct HeapStalePlugin {
    binding: String,
    probe_stale: bool,
}

fn heap_field_get(get: u16, binding: u16, field: &str) -> vm::Expr {
    vm::Expr::Call(
        get,
        Vec::new(),
        vec![vm::Expr::Var(binding), vm::Expr::String(field.to_owned())],
        None,
        None,
    )
}

fn heap_as_number(expr: vm::Expr) -> vm::Expr {
    vm::Expr::Div(Box::new(expr), Box::new(vm::Expr::Float(1.0)))
}

fn heap_guest_call(index: u16, args: Vec<vm::Expr>) -> vm::Expr {
    vm::Expr::Call(index, Vec::new(), args, None, None)
}

fn heap_replay_binding(
    ir: &mut vm::FrontendIr,
    binding: u16,
    node_begin: u16,
    root_enter: u16,
    root_write: u16,
) -> Result<(), vm::ParseError> {
    let boxed_at = ir
        .stmts
        .iter()
        .position(|stmt| matches!(stmt, vm::Stmt::Assign { index, .. } if *index == binding))
        .ok_or_else(|| heap_rejection("heap fixture requires the real binding store".to_owned()))?;
    let publish_expr = match &ir.stmts[boxed_at] {
        vm::Stmt::Assign { expr, .. } => expr.clone(),
        other => {
            return Err(heap_rejection(format!(
                "heap fixture requires the replayable binding store, got {other:?}"
            )));
        }
    };
    // Hoisted bindings are predeclared, then stored by Assign. Replay the
    // contiguous reservation Lets immediately before that store, then the
    // store and its production root-write ack.
    let mut run_start = boxed_at;
    while run_start > 0 {
        match &ir.stmts[run_start - 1] {
            vm::Stmt::Let { expr: prev, .. } => {
                if let vm::Expr::Call(target, _, _, _, _) = prev
                    && *target == root_enter
                {
                    break;
                }
                run_start -= 1;
            }
            _ => break,
        }
    }
    let run: Vec<vm::Stmt> = ir.stmts[run_start..boxed_at].to_vec();
    let ack = ir.stmts.get(boxed_at + 1).cloned();
    let mut saw_begin = false;
    for stmt in &run {
        match stmt {
            vm::Stmt::Let {
                index, expr, line, ..
            } => {
                if let vm::Expr::Call(target, _, _, _, _) = expr
                    && *target == node_begin
                {
                    saw_begin = true;
                }
                ir.stmts.push(vm::Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: *index,
                    expr: expr.clone(),
                    line: *line,
                });
            }
            other => {
                return Err(heap_rejection(format!(
                    "heap fixture replays only Let bindings, got {other:?}"
                )));
            }
        }
    }
    if !saw_begin {
        return Err(heap_rejection(
            "heap fixture replay must span a node reservation".to_owned(),
        ));
    }
    ir.stmts.push(vm::Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: binding,
        expr: publish_expr,
        line: 1,
    });
    if let Some(vm::Stmt::Let {
        index, expr, line, ..
    }) = ack
        && let vm::Expr::Call(target, _, _, _, _) = &expr
        && *target == root_write
    {
        ir.stmts.push(vm::Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index,
            expr,
            line,
        });
    }
    Ok(())
}

impl vm::SourcePlugin for HeapStalePlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-heap-stale"),
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
        let get = vm::BuiltinFunction::Get.call_index();
        let make = heap_helper_index(&ir, HEAP_MAKE_NAME)?;
        let decode = heap_helper_index(&ir, HEAP_DECODE_INT_NAME)?;
        let checked_base = heap_helper_index(&ir, HEAP_CHECKED_BASE_NAME)?;
        let collect = heap_helper_index(&ir, HEAP_COLLECT_NAME)?;
        let node_begin = heap_helper_index(&ir, HEAP_NODE_BEGIN_NAME)?;
        let root_enter = heap_helper_index(&ir, HEAP_ROOT_ENTER_NAME)?;
        let root_write = heap_helper_index(&ir, HEAP_ROOT_WRITE_NAME)?;
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
            return Err(heap_rejection(format!(
                "heap fixture requires exactly one realm carrier, found {}",
                carriers.len()
            )));
        };
        let carrier = *carrier;
        let binding = heap_binding_index(&ir, &self.binding)?;
        // Reserve all fixture slots up front: no closure borrows the IR.
        let (old_slot, old_gen, ack, new_slot, new_gen) = {
            let mut take = || -> Result<u16, vm::ParseError> {
                let slot = u16::try_from(ir.locals)
                    .map_err(|_| heap_rejection("heap fixture slot space exhausted".to_owned()))?;
                ir.locals += 1;
                Ok(slot)
            };
            (take()?, take()?, take()?, take()?, take()?)
        };
        let stash = |ir: &mut vm::FrontendIr, out: u16, field: &str| {
            ir.stmts.push(vm::Stmt::Let {
                index: out,
                declared_schema: Some(vm::TypeSchema::Int),
                expr: heap_guest_call(
                    decode,
                    vec![heap_as_number(heap_field_get(get, binding, field))],
                ),
                line: 1,
            });
            ir.stmts.push(vm::Stmt::Expr {
                expr: vm::Expr::Var(out),
                line: 1,
            });
        };
        // Stash the live box identity in plain (unrooted) diagnostic locals.
        stash(&mut ir, old_slot, "ref_slot");
        stash(&mut ir, old_gen, "generation");
        // First replay unroots the original node; the explicit collection
        // frees it; the second replay reuses the freed slot with a bumped
        // generation. The entry now names the newest box throughout.
        heap_replay_binding(&mut ir, binding, node_begin, root_enter, root_write)?;
        ir.stmts.push(vm::Stmt::Let {
            index: ack,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(collect, vec![vm::Expr::Var(carrier)]),
            line: 1,
        });
        heap_replay_binding(&mut ir, binding, node_begin, root_enter, root_write)?;
        stash(&mut ir, new_slot, "ref_slot");
        stash(&mut ir, new_gen, "generation");
        let base = {
            let slot = u16::try_from(ir.locals)
                .map_err(|_| heap_rejection("heap fixture slot space exhausted".to_owned()))?;
            ir.locals += 1;
            slot
        };
        let (probe_slot, probe_gen) = if self.probe_stale {
            (old_slot, old_gen)
        } else {
            (new_slot, new_gen)
        };
        ir.stmts.push(vm::Stmt::Let {
            index: base,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(
                checked_base,
                vec![
                    vm::Expr::Var(carrier),
                    vm::Expr::Var(probe_slot),
                    vm::Expr::Var(probe_gen),
                ],
            ),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(base),
            line: 1,
        });
        Ok(ir)
    }
}

fn heap_stale_run(
    source: &str,
    binding: &str,
    probe_stale: bool,
) -> (Vec<Value>, Option<vm::VmError>) {
    let plugin: &'static HeapStalePlugin = Box::leak(Box::new(HeapStalePlugin {
        binding: binding.to_owned(),
        probe_stale,
    }));
    let catalog = heap_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_source_plugin(plugin)
        .with_host_api_catalog(std::sync::Arc::clone(&catalog));
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-heap-stale")),
        options,
    )
    .expect("stale compilation should succeed");
    let mut machine = compilation
        .into_vm()
        .expect("certified compilation should create a source-certified VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = vm::HostFunctionRegistry::restricted();
    install_heap_production(&mut registry, &catalog);
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind to this VM");
    let error = match machine.run() {
        Ok(status) => {
            assert_eq!(status, VmStatus::Halted, "stale program should halt");
            None
        }
        Err(error) => Some(error),
    };
    (machine.stack().to_vec(), error)
}

fn heap_stale_ints(stack: &[Value]) -> Vec<i64> {
    stack
        .iter()
        .map(|v| match v {
            Value::Int(n) => *n,
            Value::Float(n) => *n as i64,
            other => panic!("stale evidence must be Int, got {other:?}"),
        })
        .collect()
}

/// Stale refusal plus slot-reuse evidence: the run must fail exactly at the
/// stale dereference with the full four-cell evidence below it proving the
/// slot was reused with a bumped generation. The failing gate leaves its
/// false condition on the stack, which locates the refusal at the probe.
fn heap_expect_stale_refusal(source: &str, binding: &str, expect_slot_zero: bool) {
    let (stack, error) = heap_stale_run(source, binding, true);
    let error = error.expect("stale dereference must refuse");
    eprintln!("P2_STALE_REFUSE {binding} error={error:?} stack={stack:?}");
    assert!(
        format!("{error:?}").contains("assertion failed"),
        "refusal must come from the checked gate, got {error:?}"
    );
    assert_eq!(
        stack.len(),
        5,
        "refusal must leave evidence plus the false gate, got {stack:?}"
    );
    let evidence = heap_stale_ints(&stack[..4]);
    assert!(
        matches!(stack[4], Value::Bool(false)),
        "refusal must leave the false gate condition, got {:?}",
        stack[4]
    );
    let (old_slot, old_gen, new_slot, new_gen) =
        (evidence[0], evidence[1], evidence[2], evidence[3]);
    assert_eq!(
        new_slot, old_slot,
        "freed slot must be reused, got {evidence:?}"
    );
    assert_eq!(
        new_gen,
        old_gen + 1,
        "reuse must bump the generation, got {evidence:?}"
    );
    if expect_slot_zero {
        assert_eq!(
            old_slot, 0,
            "first node must live in slot0, got {evidence:?}"
        );
    }
}

/// Fresh dereference after the identical reuse: must halt with the same
/// slot-reuse evidence plus a valid physical base past the fixed prefix.
fn heap_expect_fresh_success(source: &str, binding: &str, expect_slot_zero: bool) {
    let (stack, error) = heap_stale_run(source, binding, false);
    assert!(
        error.is_none(),
        "fresh dereference must succeed, got {error:?}"
    );
    let evidence = heap_stale_ints(&stack);
    assert_eq!(
        evidence.len(),
        5,
        "success must leave evidence plus base, got {evidence:?}"
    );
    let (old_slot, old_gen, new_slot, new_gen, base) = (
        evidence[0],
        evidence[1],
        evidence[2],
        evidence[3],
        evidence[4],
    );
    eprintln!("P2_STALE_FRESH {binding} evidence={evidence:?}");
    assert_eq!(
        new_slot, old_slot,
        "freed slot must be reused, got {evidence:?}"
    );
    assert_eq!(
        new_gen,
        old_gen + 1,
        "reuse must bump the generation, got {evidence:?}"
    );
    if expect_slot_zero {
        assert_eq!(
            old_slot, 0,
            "first node must live in slot0, got {evidence:?}"
        );
    }
    assert!(
        base >= 80,
        "fresh base must clear the fixed prefix, got {base}"
    );
}

#[test]
fn p2_heap_stale_array_refuses_and_fresh_works() {
    heap_expect_stale_refusal("var a = [1];", "a", true);
    heap_expect_fresh_success("var a = [1];", "a", true);
}

#[test]
fn p2_heap_stale_string_refuses_and_fresh_works() {
    heap_expect_stale_refusal("var s = \"hi\";", "s", true);
    heap_expect_fresh_success("var s = \"hi\";", "s", true);
}

#[test]
fn p2_heap_stale_object_refuses_and_fresh_works() {
    heap_expect_stale_refusal("var o = {};", "o", true);
    heap_expect_fresh_success("var o = {};", "o", true);
}

#[test]
fn p2_heap_stale_function_refuses_and_fresh_works() {
    heap_expect_stale_refusal(
        "var f = function() { \"use strict\"; return 1; };",
        "f",
        true,
    );
    heap_expect_fresh_success(
        "var f = function() { \"use strict\"; return 1; };",
        "f",
        true,
    );
}

// ---- Stale-key harness: the stored row key_gen must not be ignored ----
// `var o = {k: 1}` replays through the same unroot/collect/reuse cycle;
// the forged Reference naming the ORIGINAL key generation must refuse in
// `__compat_es5_ref_get` while the same slot with the CURRENT generation
// reads back 1. Only key_slot/key_gen travel in the forged value; the base
// stays the live object and strictness stays false.

const HEAP_READ_NAME: &str = "__compat_ref_read";
const HEAP_REF_GET_NAME: &str = "__compat_es5_ref_get";

struct HeapKeyPlugin {
    probe_stale: bool,
    corrupt_stored: bool,
}

fn heap_object_row_key(
    ir: &mut vm::FrontendIr,
    read: u16,
    decode: u16,
    carrier: u16,
    base: u16,
    cell: i64,
    tag: &str,
) -> Result<u16, vm::ParseError> {
    let raw = {
        let slot = u16::try_from(ir.locals)
            .map_err(|_| heap_rejection(format!("heap {tag} slot space exhausted")))?;
        ir.locals += 1;
        slot
    };
    ir.stmts.push(vm::Stmt::Let {
        index: raw,
        declared_schema: Some(vm::TypeSchema::Number),
        expr: heap_guest_call(
            read,
            vec![
                vm::Expr::Var(carrier),
                heap_as_number(vm::Expr::Add(
                    Box::new(vm::Expr::Var(base)),
                    Box::new(vm::Expr::Int(5 + cell)),
                )),
            ],
        ),
        line: 1,
    });
    let out = {
        let slot = u16::try_from(ir.locals)
            .map_err(|_| heap_rejection(format!("heap {tag} slot space exhausted")))?;
        ir.locals += 1;
        slot
    };
    ir.stmts.push(vm::Stmt::Let {
        index: out,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: heap_guest_call(decode, vec![vm::Expr::Var(raw)]),
        line: 1,
    });
    Ok(out)
}

impl vm::SourcePlugin for HeapKeyPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-heap-key"),
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
        let get = vm::BuiltinFunction::Get.call_index();
        let map_new = vm::BuiltinFunction::MapNew.call_index();
        let set = vm::BuiltinFunction::Set.call_index();
        let make = heap_helper_index(&ir, HEAP_MAKE_NAME)?;
        let read = heap_helper_index(&ir, HEAP_READ_NAME)?;
        let decode = heap_helper_index(&ir, HEAP_DECODE_INT_NAME)?;
        let checked_base = heap_helper_index(&ir, HEAP_CHECKED_BASE_NAME)?;
        let collect = heap_helper_index(&ir, HEAP_COLLECT_NAME)?;
        let node_begin = heap_helper_index(&ir, HEAP_NODE_BEGIN_NAME)?;
        let root_enter = heap_helper_index(&ir, HEAP_ROOT_ENTER_NAME)?;
        let root_write = heap_helper_index(&ir, HEAP_ROOT_WRITE_NAME)?;
        let ref_get = heap_helper_index(&ir, HEAP_REF_GET_NAME)?;
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
            return Err(heap_rejection(format!(
                "heap fixture requires exactly one realm carrier, found {}",
                carriers.len()
            )));
        };
        let carrier = *carrier;
        let binding = heap_binding_index(&ir, "o")?;
        let (base_old, ack, base_new, forged, got) = {
            let mut take = || -> Result<u16, vm::ParseError> {
                let slot = u16::try_from(ir.locals)
                    .map_err(|_| heap_rejection("heap key slot space exhausted".to_owned()))?;
                ir.locals += 1;
                Ok(slot)
            };
            (take()?, take()?, take()?, take()?, take()?)
        };
        // Whole-box copy before any field read, mirroring the production
        // receiver copy: later field moves must never poison whole uses.
        // Taken after the final replay so it names the newest live box.
        let box_copy = {
            let slot = u16::try_from(ir.locals)
                .map_err(|_| heap_rejection("heap key slot space exhausted".to_owned()))?;
            ir.locals += 1;
            slot
        };
        // Live base plus the stored row key pair, pushed as evidence.
        ir.stmts.push(vm::Stmt::Let {
            index: base_old,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(
                checked_base,
                vec![
                    vm::Expr::Var(carrier),
                    heap_field_get(get, binding, "ref_slot"),
                    heap_field_get(get, binding, "generation"),
                ],
            ),
            line: 1,
        });
        let key_slot_old = heap_object_row_key(&mut ir, read, decode, carrier, base_old, 0, "key")?;
        let key_gen_old = heap_object_row_key(&mut ir, read, decode, carrier, base_old, 1, "key")?;
        for slot in [key_slot_old, key_gen_old] {
            ir.stmts.push(vm::Stmt::Expr {
                expr: vm::Expr::Var(slot),
                line: 1,
            });
        }
        heap_replay_binding(&mut ir, binding, node_begin, root_enter, root_write)?;
        ir.stmts.push(vm::Stmt::Let {
            index: ack,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(collect, vec![vm::Expr::Var(carrier)]),
            line: 1,
        });
        // Valid free-list ordering for the two reclaimed rows: key0 then
        // object1, so same-kind slot reuse isolates the generation check.
        let mut key_ctx = heap_cycle_ctx(&ir)?;
        key_ctx.carrier = carrier;
        let page_raw =
            heap_cycle_read_number(&mut ir, &key_ctx, carrier, vm::Expr::Int(16), "key-order")?;
        let page = heap_cycle_take(&mut ir, "key-order")?;
        ir.stmts.push(vm::Stmt::Let {
            index: page,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(decode, vec![vm::Expr::Var(page_raw)]),
            line: 1,
        });
        for (address, link) in [
            (vm::Expr::Int(5), 1),
            (
                vm::Expr::Add(Box::new(vm::Expr::Var(page)), Box::new(vm::Expr::Int(9))),
                2,
            ),
            (
                vm::Expr::Add(Box::new(vm::Expr::Var(page)), Box::new(vm::Expr::Int(16))),
                0,
            ),
        ] {
            heap_cycle_write(
                &mut ir,
                &key_ctx,
                carrier,
                address,
                vm::Expr::Float(link as f64),
                "key-order",
            )?;
        }
        heap_replay_binding(&mut ir, binding, node_begin, root_enter, root_write)?;
        ir.stmts.push(vm::Stmt::Let {
            index: box_copy,
            declared_schema: Some(vm::TypeSchema::Named(
                "__CompatEs5Primitive".to_owned(),
                Vec::new(),
            )),
            expr: vm::Expr::Var(binding),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Let {
            index: base_new,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(
                checked_base,
                vec![
                    vm::Expr::Var(carrier),
                    heap_field_get(get, binding, "ref_slot"),
                    heap_field_get(get, binding, "generation"),
                ],
            ),
            line: 1,
        });
        let key_slot_new = heap_object_row_key(&mut ir, read, decode, carrier, base_new, 0, "key")?;
        let key_gen_new = heap_object_row_key(&mut ir, read, decode, carrier, base_new, 1, "key")?;
        for slot in [key_slot_new, key_gen_new] {
            ir.stmts.push(vm::Stmt::Expr {
                expr: vm::Expr::Var(slot),
                line: 1,
            });
        }
        if self.corrupt_stored {
            // The Reference below names the fresh live key correctly. Only
            // the object's stored key generation is stale in this control.
            heap_cycle_write(
                &mut ir,
                &key_ctx,
                carrier,
                vm::Expr::Add(
                    Box::new(vm::Expr::Var(base_new)),
                    Box::new(vm::Expr::Int(6)),
                ),
                heap_as_number(vm::Expr::Var(key_gen_old)),
                "stored-key-generation",
            )?;
        }
        // Forge the Reference with either the original or the current key
        // generation; the slot travels from the matching read.
        let (key_slot, key_gen) = if self.probe_stale {
            (key_slot_old, key_gen_old)
        } else {
            (key_slot_new, key_gen_new)
        };
        let mut reference = heap_guest_call(map_new, Vec::new());
        for (field, value) in [
            ("kind", vm::Expr::Int(0)),
            ("base", vm::Expr::Var(box_copy)),
            ("key_slot", vm::Expr::Var(key_slot)),
            ("key_generation", vm::Expr::Var(key_gen)),
            ("strict", vm::Expr::Bool(false)),
        ] {
            reference = heap_guest_call(
                set,
                vec![reference, vm::Expr::String(field.to_owned()), value],
            );
        }
        ir.stmts.push(vm::Stmt::Let {
            index: forged,
            declared_schema: Some(vm::TypeSchema::Named(
                "__CompatEs5Reference".to_owned(),
                Vec::new(),
            )),
            expr: reference,
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Let {
            index: got,
            declared_schema: Some(vm::TypeSchema::Named(
                "__CompatEs5Primitive".to_owned(),
                Vec::new(),
            )),
            expr: heap_guest_call(ref_get, vec![vm::Expr::Var(carrier), vm::Expr::Var(forged)]),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(got),
            line: 1,
        });
        Ok(ir)
    }
}

fn heap_key_run(probe_stale: bool) -> (Vec<Value>, Option<vm::VmError>) {
    heap_key_run_variant(probe_stale, false)
}

fn heap_key_run_variant(
    probe_stale: bool,
    corrupt_stored: bool,
) -> (Vec<Value>, Option<vm::VmError>) {
    let plugin: &'static HeapKeyPlugin = Box::leak(Box::new(HeapKeyPlugin {
        probe_stale,
        corrupt_stored,
    }));
    let catalog = heap_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_source_plugin(plugin)
        .with_host_api_catalog(std::sync::Arc::clone(&catalog));
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        "var o = {k: 1};",
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-heap-key")),
        options,
    )
    .expect("key compilation should succeed");
    let mut machine = compilation
        .into_vm()
        .expect("certified compilation should create a source-certified VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = vm::HostFunctionRegistry::restricted();
    install_heap_production(&mut registry, &catalog);
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind to this VM");
    let error = match machine.run() {
        Ok(status) => {
            assert_eq!(status, VmStatus::Halted, "key program should halt");
            None
        }
        Err(error) => Some(error),
    };
    (machine.stack().to_vec(), error)
}

#[test]
fn p2_heap_stale_key_refuses_and_fresh_reads() {
    let (stack, error) = heap_key_run(true);
    let error = error.expect("stale key generation must refuse");
    eprintln!("P2_STALE_KEY_REFUSE error={error:?} stack={stack:?}");
    assert!(
        format!("{error:?}").contains("assertion failed"),
        "key refusal must come from the checked gate, got {error:?}"
    );
    assert_eq!(
        stack.len(),
        5,
        "key refusal must leave key evidence plus the false gate, got {stack:?}"
    );
    assert!(
        matches!(stack[4], Value::Bool(false)),
        "key refusal must leave the false gate condition, got {:?}",
        stack[4]
    );
    let (stack, error) = heap_key_run(false);
    assert!(
        error.is_none(),
        "current key generation must read, got {error:?}"
    );
    let value = stack.last().expect("ref_get must leave the value");
    eprintln!("P2_STALE_KEY_FRESH stack={stack:?}");
    assert_eq!(
        heap_box_number(value),
        1.0,
        "current key must read back 1, got {value:?}"
    );
}

#[test]
fn p2_heap_stored_key_generation_is_independently_checked() {
    let (stack, error) = heap_key_run_variant(false, true);
    let error = error.expect("correct fresh Reference cannot authenticate a stale stored row key");
    assert!(format!("{error:?}").contains("assertion failed"));
    let evidence = heap_stale_ints(&stack[..4]);
    assert_eq!(evidence, [0, 1, 0, 2]);
    assert_eq!(stack.last(), Some(&Value::Bool(false)));
    eprintln!("P2_STORED_KEY_STALE error={error:?} evidence={evidence:?}");
}

// ---- Wrong-kind harness: kind gates are distinct from generation gates ----
// Mirrors the production numeric-array read gate bit for bit (live base,
// kind cell, 17-bit decode, assert against KIND_ARRAY) but presents a live
// OBJECT node. The gate must refuse with the true kind as evidence; the
// same gate against KIND_OBJECT must pass. A generation failure would show
// a different evidence shape, so this distinguishes the two checks.

struct HeapKindPlugin {
    expect_kind: i64,
}

impl vm::SourcePlugin for HeapKindPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-heap-kind"),
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
        let assert_index = vm::BuiltinFunction::Assert.call_index();
        let make = heap_helper_index(&ir, HEAP_MAKE_NAME)?;
        let read = heap_helper_index(&ir, HEAP_READ_NAME)?;
        let decode = heap_helper_index(&ir, HEAP_DECODE_INT_NAME)?;
        let checked_base = heap_helper_index(&ir, HEAP_CHECKED_BASE_NAME)?;
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
            return Err(heap_rejection(format!(
                "heap fixture requires exactly one realm carrier, found {}",
                carriers.len()
            )));
        };
        let carrier = *carrier;
        let binding = heap_binding_index(&ir, "o")?;
        let get = vm::BuiltinFunction::Get.call_index();
        let take = |ir: &mut vm::FrontendIr, tag: &str| -> Result<u16, vm::ParseError> {
            let slot = u16::try_from(ir.locals)
                .map_err(|_| heap_rejection(format!("heap {tag} slot space exhausted")))?;
            ir.locals += 1;
            Ok(slot)
        };
        let base = take(&mut ir, "kind")?;
        ir.stmts.push(vm::Stmt::Let {
            index: base,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(
                checked_base,
                vec![
                    vm::Expr::Var(carrier),
                    heap_field_get(get, binding, "ref_slot"),
                    heap_field_get(get, binding, "generation"),
                ],
            ),
            line: 1,
        });
        let kind_num = take(&mut ir, "kind")?;
        ir.stmts.push(vm::Stmt::Let {
            index: kind_num,
            declared_schema: Some(vm::TypeSchema::Number),
            expr: heap_guest_call(
                read,
                vec![vm::Expr::Var(carrier), heap_as_number(vm::Expr::Var(base))],
            ),
            line: 1,
        });
        let kind = take(&mut ir, "kind")?;
        ir.stmts.push(vm::Stmt::Let {
            index: kind,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(decode, vec![vm::Expr::Var(kind_num)]),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(kind),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: heap_guest_call(
                assert_index,
                vec![vm::Expr::Eq(
                    Box::new(vm::Expr::Var(kind)),
                    Box::new(vm::Expr::Int(self.expect_kind)),
                )],
            ),
            line: 1,
        });
        Ok(ir)
    }
}

fn heap_kind_run(expect_kind: i64) -> (Vec<Value>, Option<vm::VmError>) {
    let plugin: &'static HeapKindPlugin = Box::leak(Box::new(HeapKindPlugin { expect_kind }));
    let catalog = heap_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_source_plugin(plugin)
        .with_host_api_catalog(std::sync::Arc::clone(&catalog));
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        "var o = {};",
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-heap-kind")),
        options,
    )
    .expect("kind compilation should succeed");
    let mut machine = compilation
        .into_vm()
        .expect("certified compilation should create a source-certified VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = vm::HostFunctionRegistry::restricted();
    install_heap_production(&mut registry, &catalog);
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind to this VM");
    let error = match machine.run() {
        Ok(status) => {
            assert_eq!(status, VmStatus::Halted, "kind program should halt");
            None
        }
        Err(error) => Some(error),
    };
    (machine.stack().to_vec(), error)
}

#[test]
fn p2_heap_kind_gate_refuses_object_as_array() {
    // KIND_ARRAY = 1 against a live OBJECT node (kind 3): must refuse with
    // the true kind pushed as evidence plus the false gate.
    let (stack, error) = heap_kind_run(1);
    let error = error.expect("object presented as array must refuse");
    eprintln!("P2_KIND_REFUSE error={error:?} stack={stack:?}");
    assert!(
        format!("{error:?}").contains("assertion failed"),
        "kind refusal must come from the gate, got {error:?}"
    );
    assert_eq!(
        stack.len(),
        2,
        "kind refusal must leave the kind plus the false gate, got {stack:?}"
    );
    assert!(
        matches!(stack[0], Value::Int(3)),
        "refused node must report OBJECT kind 3, got {:?}",
        stack[0]
    );
    assert!(
        matches!(stack[1], Value::Bool(false)),
        "kind refusal must leave the false gate condition, got {:?}",
        stack[1]
    );
    // KIND_OBJECT = 3 against the same node: must pass.
    let (stack, error) = heap_kind_run(3);
    assert!(
        error.is_none(),
        "object presented as object must pass, got {error:?}"
    );
    eprintln!("P2_KIND_FRESH stack={stack:?}");
    assert!(
        matches!(stack.last(), Some(Value::Int(3))),
        "passing gate must confirm kind 3, got {stack:?}"
    );
}

// ---- Max-generation retirement: sweep retires, never wraps ----
// A consistent ACTIVE max-generation row (slot0, proven first node) is
// unrooted by replay, swept by an explicit collection, then observed
// retired while the next allocation lands on a different slot. The slot-0
// fact is re-asserted in-run; a moved first node would fail loudly.

const HEAP_WRITE_NAME: &str = "__compat_ref_write";

struct HeapMaxgenPlugin;

impl vm::SourcePlugin for HeapMaxgenPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-heap-maxgen"),
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
        let get = vm::BuiltinFunction::Get.call_index();
        let make = heap_helper_index(&ir, HEAP_MAKE_NAME)?;
        let read = heap_helper_index(&ir, HEAP_READ_NAME)?;
        let write = heap_helper_index(&ir, HEAP_WRITE_NAME)?;
        let decode = heap_helper_index(&ir, HEAP_DECODE_INT_NAME)?;
        let checked_base = heap_helper_index(&ir, HEAP_CHECKED_BASE_NAME)?;
        let collect = heap_helper_index(&ir, HEAP_COLLECT_NAME)?;
        let node_begin = heap_helper_index(&ir, HEAP_NODE_BEGIN_NAME)?;
        let root_enter = heap_helper_index(&ir, HEAP_ROOT_ENTER_NAME)?;
        let root_write = heap_helper_index(&ir, HEAP_ROOT_WRITE_NAME)?;
        let observe = heap_helper_index(&ir, HEAP_OBSERVE_NAME)?;
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
            return Err(heap_rejection(format!(
                "heap fixture requires exactly one realm carrier, found {}",
                carriers.len()
            )));
        };
        let carrier = *carrier;
        let binding = heap_binding_index(&ir, "a")?;
        let take = |ir: &mut vm::FrontendIr, tag: &str| -> Result<u16, vm::ParseError> {
            let slot = u16::try_from(ir.locals)
                .map_err(|_| heap_rejection(format!("heap {tag} slot space exhausted")))?;
            ir.locals += 1;
            Ok(slot)
        };
        // Stash the first-node slot; slot0 is asserted in-test below.
        // Both identity fields are stashed (mirroring the stale fixture
        // shape the move checker accepts).
        let first = take(&mut ir, "maxgen")?;
        ir.stmts.push(vm::Stmt::Let {
            index: first,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(
                decode,
                vec![heap_as_number(heap_field_get(get, binding, "ref_slot"))],
            ),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(first),
            line: 1,
        });
        let first_gen = take(&mut ir, "maxgen")?;
        ir.stmts.push(vm::Stmt::Let {
            index: first_gen,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(
                decode,
                vec![heap_as_number(heap_field_get(get, binding, "generation"))],
            ),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(first_gen),
            line: 1,
        });
        // Keep slot0 active and rooted while making the diagnostic maximum
        // generation consistent across row, node header and actual root.
        let original_base = take(&mut ir, "maxgen")?;
        ir.stmts.push(vm::Stmt::Let {
            index: original_base,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(
                checked_base,
                vec![
                    vm::Expr::Var(carrier),
                    vm::Expr::Var(first),
                    vm::Expr::Var(first_gen),
                ],
            ),
            line: 1,
        });
        // Overwrite the original row generation with u32::MAX. Slot0 lives
        // in directory page 0 row 0: row address = page_base + 3, and the
        // generation cell is one past the row start.
        let page_ptr = take(&mut ir, "maxgen")?;
        ir.stmts.push(vm::Stmt::Let {
            index: page_ptr,
            declared_schema: Some(vm::TypeSchema::Number),
            expr: heap_guest_call(
                read,
                vec![vm::Expr::Var(carrier), heap_as_number(vm::Expr::Int(16))],
            ),
            line: 1,
        });
        let page_base = take(&mut ir, "maxgen")?;
        ir.stmts.push(vm::Stmt::Let {
            index: page_base,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(decode, vec![vm::Expr::Var(page_ptr)]),
            line: 1,
        });
        let gen_cell = take(&mut ir, "maxgen")?;
        ir.stmts.push(vm::Stmt::Let {
            index: gen_cell,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: vm::Expr::Add(
                Box::new(vm::Expr::Var(page_base)),
                Box::new(vm::Expr::Int(3 + 1)),
            ),
            line: 1,
        });
        let wack = take(&mut ir, "maxgen")?;
        ir.stmts.push(vm::Stmt::Let {
            index: wack,
            declared_schema: Some(vm::TypeSchema::Number),
            expr: heap_guest_call(
                write,
                vec![
                    vm::Expr::Var(carrier),
                    heap_as_number(vm::Expr::Var(gen_cell)),
                    heap_as_number(vm::Expr::Int(u32::MAX as i64)),
                ],
            ),
            line: 1,
        });
        let header_ack = take(&mut ir, "maxgen")?;
        ir.stmts.push(vm::Stmt::Let {
            index: header_ack,
            declared_schema: Some(vm::TypeSchema::Number),
            expr: heap_guest_call(
                write,
                vec![
                    vm::Expr::Var(carrier),
                    heap_as_number(vm::Expr::Add(
                        Box::new(vm::Expr::Var(original_base)),
                        Box::new(vm::Expr::Int(1)),
                    )),
                    vm::Expr::Float(f64::from(u32::MAX)),
                ],
            ),
            line: 1,
        });
        let frame = ir
            .stmts
            .iter()
            .find_map(|stmt| match stmt {
                vm::Stmt::Let {
                    index,
                    expr: vm::Expr::Call(target, _, args, _, _),
                    ..
                } if *target == root_enter && matches!(args.get(1), Some(vm::Expr::Int(1))) => {
                    Some(*index)
                }
                _ => None,
            })
            .ok_or_else(|| {
                heap_rejection(
                    "consistent maxgen requires the one-entry actual source frame".to_owned(),
                )
            })?;
        let root_node = heap_helper_index(&ir, "__compat_es5_root_write_node")?;
        let rooted_ack = take(&mut ir, "maxgen")?;
        ir.stmts.push(vm::Stmt::Let {
            index: rooted_ack,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(
                root_node,
                vec![
                    vm::Expr::Var(carrier),
                    vm::Expr::Var(frame),
                    vm::Expr::Int(0),
                    vm::Expr::Int(5),
                    vm::Expr::Var(first),
                    vm::Expr::Int(i64::from(u32::MAX)),
                ],
            ),
            line: 1,
        });
        let retained = take(&mut ir, "maxgen")?;
        ir.stmts.push(vm::Stmt::Let {
            index: retained,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(collect, vec![vm::Expr::Var(carrier)]),
            line: 1,
        });
        let retained_base = take(&mut ir, "maxgen")?;
        ir.stmts.push(vm::Stmt::Let {
            index: retained_base,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(
                checked_base,
                vec![
                    vm::Expr::Var(carrier),
                    vm::Expr::Var(first),
                    vm::Expr::Int(i64::from(u32::MAX)),
                ],
            ),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: heap_guest_call(
                vm::BuiltinFunction::Assert.call_index(),
                vec![vm::Expr::Eq(
                    Box::new(vm::Expr::Var(original_base)),
                    Box::new(vm::Expr::Var(retained_base)),
                )],
            ),
            line: 1,
        });
        // A real replacement now clears the sole old root. The maxgen node
        // was live and checked above; only this subsequent sweep retires it.
        heap_replay_binding(&mut ir, binding, node_begin, root_enter, root_write)?;
        // Sweep must retire the max-generation row: observe the retired
        // counter plus the row state/generation through the production
        // selector chain, then allocate once more.
        let ack = take(&mut ir, "maxgen")?;
        ir.stmts.push(vm::Stmt::Let {
            index: ack,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(collect, vec![vm::Expr::Var(carrier)]),
            line: 1,
        });
        // which, slot-arg: 1=retired counter, 13=row state, 14=row gen.
        for (which, slot_arg) in [(1i64, 0i64), (13, 0), (14, 0)] {
            let out = take(&mut ir, "maxgen")?;
            ir.stmts.push(vm::Stmt::Let {
                index: out,
                declared_schema: Some(vm::TypeSchema::Int),
                expr: heap_guest_call(
                    observe,
                    vec![
                        vm::Expr::Var(carrier),
                        vm::Expr::Int(which),
                        vm::Expr::Int(slot_arg),
                    ],
                ),
                line: 1,
            });
            ir.stmts.push(vm::Stmt::Expr {
                expr: vm::Expr::Var(out),
                line: 1,
            });
        }
        heap_replay_binding(&mut ir, binding, node_begin, root_enter, root_write)?;
        let second = take(&mut ir, "maxgen")?;
        ir.stmts.push(vm::Stmt::Let {
            index: second,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(
                decode,
                vec![heap_as_number(heap_field_get(get, binding, "ref_slot"))],
            ),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(second),
            line: 1,
        });
        let second_gen = take(&mut ir, "maxgen")?;
        ir.stmts.push(vm::Stmt::Let {
            index: second_gen,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(
                decode,
                vec![heap_as_number(heap_field_get(get, binding, "generation"))],
            ),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(second_gen),
            line: 1,
        });
        // Fresh dereference of the newest box must still work. Reuse the
        // stashed identity (mirroring the stale fixture) rather than
        // reading the same fields a third time.
        let base = take(&mut ir, "maxgen")?;
        ir.stmts.push(vm::Stmt::Let {
            index: base,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(
                checked_base,
                vec![
                    vm::Expr::Var(carrier),
                    vm::Expr::Var(second),
                    vm::Expr::Var(second_gen),
                ],
            ),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(base),
            line: 1,
        });
        Ok(ir)
    }
}

#[test]
fn p2_heap_maxgen_retires_without_wrap() {
    let plugin: &'static HeapMaxgenPlugin = Box::leak(Box::new(HeapMaxgenPlugin));
    let catalog = heap_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_source_plugin(plugin)
        .with_host_api_catalog(std::sync::Arc::clone(&catalog));
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        "var a = [1];",
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-heap-maxgen")),
        options,
    )
    .expect("maxgen compilation should succeed");
    let mut machine = compilation
        .into_vm()
        .expect("certified compilation should create a source-certified VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = vm::HostFunctionRegistry::restricted();
    install_heap_production(&mut registry, &catalog);
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind to this VM");
    match machine.run() {
        Ok(status) => assert_eq!(status, VmStatus::Halted, "maxgen program should halt"),
        Err(error) => panic!(
            "MAXGEN RUN-FAIL ip={} error={error:?} stack={:?} frames={:?}",
            machine.ip(),
            machine.stack(),
            machine.execution_frames()
        ),
    }
    let stack = machine.stack().to_vec();
    eprintln!("P2_MAXGEN stack={stack:?}");
    // first slot/gen, retired counter, row state, row generation, second
    // slot/gen, fresh base: eight trailing observations.
    assert!(stack.len() >= 8, "maxgen evidence plus base, got {stack:?}");
    let tail = &stack[stack.len() - 8..];
    let counter = |v: &Value| match v {
        Value::Int(n) => *n,
        Value::Float(n) => *n as i64,
        other => panic!("maxgen evidence must be Int, got {other:?}"),
    };
    let (first, first_gen, retired, row_state, row_gen, second, second_gen, base) = (
        counter(&tail[0]),
        counter(&tail[1]),
        counter(&tail[2]),
        counter(&tail[3]),
        counter(&tail[4]),
        counter(&tail[5]),
        counter(&tail[6]),
        counter(&tail[7]),
    );
    assert_eq!(first, 0, "first node must live in slot0, got {first}");
    assert_eq!(
        first_gen, 1,
        "first node must start at generation 1, got {first_gen}"
    );
    assert_eq!(
        retired, 1,
        "sweep must retire exactly the max-generation row, got {retired}"
    );
    assert_eq!(
        row_state, 4,
        "retired row must report state 4, got {row_state}"
    );
    assert_eq!(
        row_gen,
        u32::MAX as i64,
        "retired row must keep its generation without wrapping, got {row_gen}"
    );
    assert_ne!(
        second, first,
        "post-retirement allocation must avoid the retired slot"
    );
    assert!(
        second_gen >= 1,
        "replacement slot must carry a live generation, got {second_gen}"
    );
    assert!(
        base >= 80,
        "fresh base must clear the fixed prefix, got {base}"
    );
}

// ---- Cyclic-graph harness (test-only IR instrumentation, labeled) ----
// Object value edges are forged with raw B1 writes into real managed rows
// (tag 5 plus the peer slot+generation), then exercised through the actual
// root helpers and the production collector. Self and mutual garbage must
// be swept with slot recycling and bounded live sets (the marker must
// terminate on cycles); retained cycles must survive with edges intact.

const HEAP_SET_PROTOTYPE_NAME: &str = "__compat_es5_set_prototype";

struct HeapCyclePlugin {
    /// 0 = self garbage, 1 = mutual retained, 2 = mutual garbage,
    /// 3 = function/environment/parent-environment graph.
    mode: u8,
}

struct HeapCycleCtx {
    carrier: u16,
    get: u16,
    read: u16,
    write: u16,
    decode: u16,
    checked_base: u16,
    collect: u16,
    observe: u16,
    node_begin: u16,
    root_enter: u16,
    root_write: u16,
}

/// Whole-box copy plus one field stash each; every box field is read at
/// most once per binding, mirroring production receiver discipline.
fn heap_cycle_take(ir: &mut vm::FrontendIr, tag: &str) -> Result<u16, vm::ParseError> {
    let slot = u16::try_from(ir.locals)
        .map_err(|_| heap_rejection(format!("heap {tag} slot space exhausted")))?;
    ir.locals += 1;
    Ok(slot)
}

fn heap_cycle_identity(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    binding: u16,
    tag: &str,
) -> Result<(u16, u16, u16), vm::ParseError> {
    let boxed = heap_cycle_take(ir, tag)?;
    ir.stmts.push(vm::Stmt::Let {
        index: boxed,
        declared_schema: Some(vm::TypeSchema::Named(
            "__CompatEs5Primitive".to_owned(),
            Vec::new(),
        )),
        expr: vm::Expr::Var(binding),
        line: 1,
    });
    let slot = heap_cycle_take(ir, tag)?;
    ir.stmts.push(vm::Stmt::Let {
        index: slot,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: heap_guest_call(
            ctx.decode,
            vec![heap_as_number(heap_field_get(ctx.get, binding, "ref_slot"))],
        ),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::Var(slot),
        line: 1,
    });
    let generation = heap_cycle_take(ir, tag)?;
    ir.stmts.push(vm::Stmt::Let {
        index: generation,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: heap_guest_call(
            ctx.decode,
            vec![heap_as_number(heap_field_get(
                ctx.get,
                binding,
                "generation",
            ))],
        ),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::Var(generation),
        line: 1,
    });
    Ok((boxed, slot, generation))
}

fn heap_cycle_base(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    carrier: u16,
    slot: u16,
    generation: u16,
    tag: &str,
) -> Result<u16, vm::ParseError> {
    let base = {
        let slot = u16::try_from(ir.locals)
            .map_err(|_| heap_rejection(format!("heap {tag} slot space exhausted")))?;
        ir.locals += 1;
        slot
    };
    ir.stmts.push(vm::Stmt::Let {
        index: base,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: heap_guest_call(
            ctx.checked_base,
            vec![
                vm::Expr::Var(carrier),
                vm::Expr::Var(slot),
                vm::Expr::Var(generation),
            ],
        ),
        line: 1,
    });
    Ok(base)
}

fn heap_cycle_write(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    carrier: u16,
    addr: vm::Expr,
    payload: vm::Expr,
    tag: &str,
) -> Result<(), vm::ParseError> {
    let ack = {
        let slot = u16::try_from(ir.locals)
            .map_err(|_| heap_rejection(format!("heap {tag} slot space exhausted")))?;
        ir.locals += 1;
        slot
    };
    ir.stmts.push(vm::Stmt::Let {
        index: ack,
        declared_schema: Some(vm::TypeSchema::Number),
        expr: heap_guest_call(
            ctx.write,
            vec![vm::Expr::Var(carrier), heap_as_number(addr), payload],
        ),
        line: 1,
    });
    Ok(())
}

fn heap_cycle_read_number(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    carrier: u16,
    addr: vm::Expr,
    tag: &str,
) -> Result<u16, vm::ParseError> {
    let raw = {
        let slot = u16::try_from(ir.locals)
            .map_err(|_| heap_rejection(format!("heap {tag} slot space exhausted")))?;
        ir.locals += 1;
        slot
    };
    ir.stmts.push(vm::Stmt::Let {
        index: raw,
        declared_schema: Some(vm::TypeSchema::Number),
        expr: heap_guest_call(ctx.read, vec![vm::Expr::Var(carrier), heap_as_number(addr)]),
        line: 1,
    });
    Ok(raw)
}

fn heap_cycle_decode(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    raw: u16,
    tag: &str,
) -> Result<u16, vm::ParseError> {
    let out = {
        let slot = u16::try_from(ir.locals)
            .map_err(|_| heap_rejection(format!("heap {tag} slot space exhausted")))?;
        ir.locals += 1;
        slot
    };
    ir.stmts.push(vm::Stmt::Let {
        index: out,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: heap_guest_call(ctx.decode, vec![vm::Expr::Var(raw)]),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::Var(out),
        line: 1,
    });
    Ok(out)
}

fn heap_cycle_observe(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    carrier: u16,
    which: i64,
    slot_arg: i64,
    tag: &str,
) -> Result<(), vm::ParseError> {
    let out = {
        let slot = u16::try_from(ir.locals)
            .map_err(|_| heap_rejection(format!("heap {tag} slot space exhausted")))?;
        ir.locals += 1;
        slot
    };
    ir.stmts.push(vm::Stmt::Let {
        index: out,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: heap_guest_call(
            ctx.observe,
            vec![
                vm::Expr::Var(carrier),
                vm::Expr::Int(which),
                vm::Expr::Int(slot_arg),
            ],
        ),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::Var(out),
        line: 1,
    });
    Ok(())
}

fn heap_cycle_collect(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    carrier: u16,
    tag: &str,
) -> Result<(), vm::ParseError> {
    let ack = {
        let slot = u16::try_from(ir.locals)
            .map_err(|_| heap_rejection(format!("heap {tag} slot space exhausted")))?;
        ir.locals += 1;
        slot
    };
    ir.stmts.push(vm::Stmt::Let {
        index: ack,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: heap_guest_call(ctx.collect, vec![vm::Expr::Var(carrier)]),
        line: 1,
    });
    Ok(())
}

/// Forge one object value edge: row value cells become (OBJECT tag, 0.0,
/// flag 0, peer slot, peer generation). The peer must be live with a
/// current generation at collection time.
fn heap_cycle_forge_edge(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    carrier: u16,
    base: u16,
    peer_slot: u16,
    peer_gen: u16,
    tag: &str,
) -> Result<(), vm::ParseError> {
    let cell = |offset: i64| {
        vm::Expr::Add(
            Box::new(vm::Expr::Var(base)),
            Box::new(vm::Expr::Int(5 + offset)),
        )
    };
    heap_cycle_write(
        ir,
        ctx,
        carrier,
        cell(2),
        heap_as_number(vm::Expr::Int(5)),
        tag,
    )?;
    heap_cycle_write(ir, ctx, carrier, cell(3), vm::Expr::Float(0.0), tag)?;
    heap_cycle_write(
        ir,
        ctx,
        carrier,
        cell(4),
        heap_as_number(vm::Expr::Int(0)),
        tag,
    )?;
    heap_cycle_write(
        ir,
        ctx,
        carrier,
        cell(5),
        heap_as_number(vm::Expr::Var(peer_slot)),
        tag,
    )?;
    heap_cycle_write(
        ir,
        ctx,
        carrier,
        cell(6),
        heap_as_number(vm::Expr::Var(peer_gen)),
        tag,
    )?;
    Ok(())
}

fn heap_cycle_ctx(ir: &vm::FrontendIr) -> Result<HeapCycleCtx, vm::ParseError> {
    Ok(HeapCycleCtx {
        carrier: 0,
        get: vm::BuiltinFunction::Get.call_index(),
        read: heap_helper_index(ir, HEAP_READ_NAME)?,
        write: heap_helper_index(ir, HEAP_WRITE_NAME)?,
        decode: heap_helper_index(ir, HEAP_DECODE_INT_NAME)?,
        checked_base: heap_helper_index(ir, HEAP_CHECKED_BASE_NAME)?,
        collect: heap_helper_index(ir, HEAP_COLLECT_NAME)?,
        observe: heap_helper_index(ir, HEAP_OBSERVE_NAME)?,
        node_begin: heap_helper_index(ir, HEAP_NODE_BEGIN_NAME)?,
        root_enter: heap_helper_index(ir, HEAP_ROOT_ENTER_NAME)?,
        root_write: heap_helper_index(ir, HEAP_ROOT_WRITE_NAME)?,
    })
}

fn heap_cycle_carrier(ir: &vm::FrontendIr) -> Result<u16, vm::ParseError> {
    let make = heap_helper_index(ir, HEAP_MAKE_NAME)?;
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
        return Err(heap_rejection(format!(
            "heap fixture requires exactly one realm carrier, found {}",
            carriers.len()
        )));
    };
    Ok(*carrier)
}

impl vm::SourcePlugin for HeapCyclePlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-heap-cycle"),
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
        let carrier = heap_cycle_carrier(&ir)?;
        ctx.carrier = carrier;
        match self.mode {
            0 => heap_cycle_self_garbage(&mut ir, &ctx, carrier)?,
            1 => heap_cycle_mutual(&mut ir, &ctx, carrier, true)?,
            3 => heap_environment_cycle(&mut ir, &ctx)?,
            _ => heap_cycle_mutual(&mut ir, &ctx, carrier, false)?,
        }
        Ok(ir)
    }
}

/// Self-edge garbage: forge s->s, unroot by replay, collect, observe the
/// freed slot set, replay into the recycled slots, and prove the fresh box
/// still dereferences.
fn heap_cycle_self_garbage(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    carrier: u16,
) -> Result<(), vm::ParseError> {
    let binding = heap_binding_index(ir, "s")?;
    let (_boxed, slot, generation) = heap_cycle_identity(ir, ctx, binding, "cycle")?;
    let base = heap_cycle_base(ir, ctx, carrier, slot, generation, "cycle")?;
    heap_cycle_forge_edge(ir, ctx, carrier, base, slot, generation, "cycle")?;
    heap_replay_binding(ir, binding, ctx.node_begin, ctx.root_enter, ctx.root_write)?;
    heap_cycle_collect(ir, ctx, carrier, "cycle")?;
    // free slots, next slot, live nodes, collections.
    for which in [11i64, 2, 0, 5] {
        heap_cycle_observe(ir, ctx, carrier, which, 0, "cycle")?;
    }
    heap_replay_binding(ir, binding, ctx.node_begin, ctx.root_enter, ctx.root_write)?;
    // Re-stash the recycled identity: the new box may live in a swapped
    // slot, so the fresh dereference must use current values.
    let (_boxed2, slot2, generation2) = heap_cycle_identity(ir, ctx, binding, "cycle")?;
    for which in [2i64, 0, 11] {
        heap_cycle_observe(ir, ctx, carrier, which, 0, "cycle")?;
    }
    let base2 = heap_cycle_base(ir, ctx, carrier, slot2, generation2, "cycle")?;
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::Var(base2),
        line: 1,
    });
    Ok(())
}

/// Mutual edges: forge a<->b, then either keep both rooted (retained) or
/// unroot both by replay and sweep the cyclic garbage (collected).
fn heap_cycle_mutual(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    carrier: u16,
    retained: bool,
) -> Result<(), vm::ParseError> {
    let a_binding = heap_binding_index(ir, "a")?;
    let b_binding = heap_binding_index(ir, "b")?;
    let (_a_boxed, a_slot, a_gen) = heap_cycle_identity(ir, ctx, a_binding, "cycle")?;
    let (_b_boxed, b_slot, b_gen) = heap_cycle_identity(ir, ctx, b_binding, "cycle")?;
    let a_base = heap_cycle_base(ir, ctx, carrier, a_slot, a_gen, "cycle")?;
    let b_base = heap_cycle_base(ir, ctx, carrier, b_slot, b_gen, "cycle")?;
    heap_cycle_forge_edge(ir, ctx, carrier, a_base, b_slot, b_gen, "cycle")?;
    heap_cycle_forge_edge(ir, ctx, carrier, b_base, a_slot, a_gen, "cycle")?;
    if !retained {
        heap_replay_binding(
            ir,
            a_binding,
            ctx.node_begin,
            ctx.root_enter,
            ctx.root_write,
        )?;
        heap_replay_binding(
            ir,
            b_binding,
            ctx.node_begin,
            ctx.root_enter,
            ctx.root_write,
        )?;
    }
    heap_cycle_collect(ir, ctx, carrier, "cycle")?;
    // free slots, next slot, live nodes, collections.
    for which in [11i64, 2, 0, 5] {
        heap_cycle_observe(ir, ctx, carrier, which, 0, "cycle")?;
    }
    if retained {
        // Edge cells must still name the live peers; both bases must still
        // resolve through the production gate.
        for base in [a_base, b_base] {
            for cell in [10i64, 11] {
                let raw = heap_cycle_read_number(
                    ir,
                    ctx,
                    carrier,
                    vm::Expr::Add(Box::new(vm::Expr::Var(base)), Box::new(vm::Expr::Int(cell))),
                    "cycle",
                )?;
                heap_cycle_decode(ir, ctx, raw, "cycle")?;
            }
        }
        let ba = heap_cycle_base(ir, ctx, carrier, a_slot, a_gen, "cycle")?;
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(ba),
            line: 1,
        });
        let bb = heap_cycle_base(ir, ctx, carrier, b_slot, b_gen, "cycle")?;
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(bb),
            line: 1,
        });
    } else {
        heap_replay_binding(
            ir,
            a_binding,
            ctx.node_begin,
            ctx.root_enter,
            ctx.root_write,
        )?;
        heap_replay_binding(
            ir,
            b_binding,
            ctx.node_begin,
            ctx.root_enter,
            ctx.root_write,
        )?;
        for which in [2i64, 0, 11] {
            heap_cycle_observe(ir, ctx, carrier, which, 0, "cycle")?;
        }
        let (_a_boxed2, a_slot2, a_gen2) = heap_cycle_identity(ir, ctx, a_binding, "cycle")?;
        let (_b_boxed2, b_slot2, b_gen2) = heap_cycle_identity(ir, ctx, b_binding, "cycle")?;
        let ba = heap_cycle_base(ir, ctx, carrier, a_slot2, a_gen2, "cycle")?;
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(ba),
            line: 1,
        });
        let bb = heap_cycle_base(ir, ctx, carrier, b_slot2, b_gen2, "cycle")?;
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(bb),
            line: 1,
        });
    }
    Ok(())
}

fn heap_cycle_run(source: &str, mode: u8) -> (Vec<Value>, Option<vm::VmError>) {
    let plugin: &'static HeapCyclePlugin = Box::leak(Box::new(HeapCyclePlugin { mode }));
    let catalog = heap_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_source_plugin(plugin)
        .with_host_api_catalog(std::sync::Arc::clone(&catalog));
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-heap-cycle")),
        options,
    )
    .expect("cycle compilation should succeed");
    let mut machine = compilation
        .into_vm()
        .expect("certified compilation should create a source-certified VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = vm::HostFunctionRegistry::restricted();
    install_heap_production(&mut registry, &catalog);
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind to this VM");
    let error = match machine.run() {
        Ok(status) => {
            assert_eq!(status, VmStatus::Halted, "cycle program should halt");
            None
        }
        Err(error) => Some(error),
    };
    (machine.stack().to_vec(), error)
}

fn heap_cycle_ints(stack: &[Value]) -> Vec<i64> {
    stack
        .iter()
        .map(|v| match v {
            Value::Int(n) => *n,
            Value::Float(n) => *n as i64,
            other => panic!("cycle evidence must be Int, got {other:?}"),
        })
        .collect()
}

#[test]
fn p2_heap_cycle_self_garbage_is_swept() {
    // Slots: key 0, obj 1, replay key/obj 2-3. The self-edge garbage (0,1)
    // must be swept (free 2, next stays 4, live drops to the replay pair);
    // the recycled slots back the fresh box afterwards.
    let (stack, error) = heap_cycle_run("var s = {k: 0};", 0);
    assert!(
        error.is_none(),
        "self-cycle collection must halt, got {error:?}"
    );
    eprintln!("P2_CYCLE_SELF stack={stack:?}");
    let v = heap_cycle_ints(&stack);
    assert_eq!(v.len(), 12, "self-cycle evidence shape, got {v:?}");
    let (old_slot, old_gen, free, next, live, coll, new_slot, new_gen, next2, live2, free2, base) = (
        v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7], v[8], v[9], v[10], v[11],
    );
    assert_eq!(
        (free, next, live, coll),
        (2, 4, 2, 1),
        "self garbage must be swept, got {v:?}"
    );
    assert_eq!(
        (next2, live2, free2),
        (4, 4, 0),
        "recycled slots must back the fresh nodes, got {v:?}"
    );
    assert!(
        new_slot < 2 && old_slot < 2,
        "cycle slots stay in the low directory, got {v:?}"
    );
    assert!(
        base >= 80,
        "fresh base must clear the fixed prefix, got {base}"
    );
    assert!(
        new_gen >= 1 && old_gen >= 1,
        "generations stay live, got {v:?}"
    );
}

#[test]
fn p2_heap_cycle_mutual_retained_survives() {
    // Slots: a-key 0, a-obj 1, b-key 2, b-obj 3. Nothing is unrooted: the
    // collection must free nothing and both forged edges must still name
    // the live peers afterwards.
    let (stack, error) = heap_cycle_run("var a = {k: 0}; var b = {k: 0};", 1);
    assert!(
        error.is_none(),
        "retained-cycle collection must halt, got {error:?}"
    );
    eprintln!("P2_CYCLE_RETAINED stack={stack:?}");
    let v = heap_cycle_ints(&stack);
    assert_eq!(v.len(), 14, "retained-cycle evidence shape, got {v:?}");
    let (a_slot, a_gen, b_slot, b_gen, free, next, live, coll) =
        (v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7]);
    assert_eq!(
        (free, next, live, coll),
        (0, 4, 4, 1),
        "retained graph must survive intact, got {v:?}"
    );
    assert_eq!(
        (v[8], v[9]),
        (b_slot, b_gen),
        "a must still edge to live b, got {v:?}"
    );
    assert_eq!(
        (v[10], v[11]),
        (a_slot, a_gen),
        "b must still edge to live a, got {v:?}"
    );
    assert!(
        v[12] >= 80 && v[13] >= 80,
        "both bases must still resolve, got {v:?}"
    );
}

#[test]
fn p2_heap_cycle_mutual_garbage_is_swept() {
    // Both old rows unrooted by replay: the collection must free all four
    // original slots (free 4, next stays 8, live drops to the replay set);
    // the second replay round recycles without fresh claims.
    let (stack, error) = heap_cycle_run("var a = {k: 0}; var b = {k: 0};", 2);
    assert!(
        error.is_none(),
        "mutual-garbage collection must halt, got {error:?}"
    );
    eprintln!("P2_CYCLE_GARBAGE stack={stack:?}");
    let v = heap_cycle_ints(&stack);
    assert_eq!(v.len(), 17, "mutual-garbage evidence shape, got {v:?}");
    let (free, next, live, coll) = (v[4], v[5], v[6], v[7]);
    assert_eq!(
        (free, next, live, coll),
        (4, 8, 4, 1),
        "mutual garbage must be swept, got {v:?}"
    );
    let (next2, live2, free2) = (v[8], v[9], v[10]);
    assert_eq!(
        (next2, free2),
        (8, 0),
        "recycled slots must back the fresh nodes, got {v:?}"
    );
    assert_eq!(
        live2, 8,
        "both generations stay charged until the next sweep, got {v:?}"
    );
    assert!(
        v[15] >= 80 && v[16] >= 80,
        "fresh bases must resolve, got {v:?}"
    );
}

// ---- Prototype-link harness (test-only IR instrumentation, labeled) ----
// The production `__compat_es5_set_prototype` helper returns 1 on success
// and 0 on a refused cycle, leaving the old pair unchanged. One halted run
// covers a valid link, a refused self cycle, and a refused indirect cycle,
// with the stored pairs read back through the production checked gate.

struct HeapProtoPlugin;

fn heap_proto_stash(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    binding: u16,
    field: &str,
) -> Result<u16, vm::ParseError> {
    let out = heap_cycle_take(ir, "proto")?;
    ir.stmts.push(vm::Stmt::Let {
        index: out,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: heap_guest_call(
            ctx.decode,
            vec![heap_as_number(heap_field_get(ctx.get, binding, field))],
        ),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::Var(out),
        line: 1,
    });
    Ok(out)
}

fn heap_proto_link(
    ir: &mut vm::FrontendIr,
    set_prototype: u16,
    carrier: u16,
    obj_box: u16,
    proto_box: u16,
) -> Result<(), vm::ParseError> {
    let verdict = heap_cycle_take(ir, "proto")?;
    ir.stmts.push(vm::Stmt::Let {
        index: verdict,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: heap_guest_call(
            set_prototype,
            vec![
                vm::Expr::Var(carrier),
                vm::Expr::Var(obj_box),
                vm::Expr::Var(proto_box),
            ],
        ),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::Var(verdict),
        line: 1,
    });
    Ok(())
}

fn heap_proto_pair(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    carrier: u16,
    slot: u16,
    generation: u16,
) -> Result<(), vm::ParseError> {
    let base = heap_cycle_base(ir, ctx, carrier, slot, generation, "proto")?;
    for cell in [3i64, 4] {
        let raw = heap_cycle_read_number(
            ir,
            ctx,
            carrier,
            vm::Expr::Add(Box::new(vm::Expr::Var(base)), Box::new(vm::Expr::Int(cell))),
            "proto",
        )?;
        heap_cycle_decode(ir, ctx, raw, "proto")?;
    }
    Ok(())
}

impl vm::SourcePlugin for HeapProtoPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-heap-proto"),
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
        let carrier = heap_cycle_carrier(&ir)?;
        ctx.carrier = carrier;
        let set_prototype = heap_helper_index(&ir, HEAP_SET_PROTOTYPE_NAME)?;
        // Whole-box copies first; field reads follow per binding.
        let mut boxes = std::collections::HashMap::new();
        let mut bindings = std::collections::HashMap::new();
        for name in ["p", "o", "a", "b"] {
            let binding = heap_binding_index(&ir, name)?;
            let boxed = heap_cycle_take(&mut ir, "proto")?;
            ir.stmts.push(vm::Stmt::Let {
                index: boxed,
                declared_schema: Some(vm::TypeSchema::Named(
                    "__CompatEs5Primitive".to_owned(),
                    Vec::new(),
                )),
                expr: vm::Expr::Var(binding),
                line: 1,
            });
            bindings.insert(name, binding);
            boxes.insert(name, boxed);
        }
        // Identity stashes (each field read once), pushed as evidence.
        let mut identities = std::collections::HashMap::new();
        for name in ["p", "o", "a", "b"] {
            let binding = bindings[name];
            let slot = heap_proto_stash(&mut ir, &ctx, binding, "ref_slot")?;
            let generation = heap_proto_stash(&mut ir, &ctx, binding, "generation")?;
            identities.insert(name, (slot, generation));
        }
        heap_proto_link(&mut ir, set_prototype, carrier, boxes["o"], boxes["p"])?;
        let (o_slot, o_gen) = identities["o"];
        heap_proto_pair(&mut ir, &ctx, carrier, o_slot, o_gen)?;
        heap_proto_link(&mut ir, set_prototype, carrier, boxes["o"], boxes["o"])?;
        heap_proto_pair(&mut ir, &ctx, carrier, o_slot, o_gen)?;
        heap_proto_link(&mut ir, set_prototype, carrier, boxes["a"], boxes["b"])?;
        heap_proto_link(&mut ir, set_prototype, carrier, boxes["b"], boxes["a"])?;
        let (b_slot, b_gen) = identities["b"];
        heap_proto_pair(&mut ir, &ctx, carrier, b_slot, b_gen)?;
        let (a_slot, a_gen) = identities["a"];
        heap_proto_pair(&mut ir, &ctx, carrier, a_slot, a_gen)?;
        Ok(ir)
    }
}

#[test]
fn p2_heap_prototype_links_and_cycle_refusals() {
    let plugin: &'static HeapProtoPlugin = Box::leak(Box::new(HeapProtoPlugin));
    let catalog = heap_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_source_plugin(plugin)
        .with_host_api_catalog(std::sync::Arc::clone(&catalog));
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        "var p = {}; var o = {}; var a = {}; var b = {};",
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-heap-proto")),
        options,
    )
    .expect("proto compilation should succeed");
    let mut machine = compilation
        .into_vm()
        .expect("certified compilation should create a source-certified VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = vm::HostFunctionRegistry::restricted();
    install_heap_production(&mut registry, &catalog);
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind to this VM");
    match machine.run() {
        Ok(status) => assert_eq!(status, VmStatus::Halted, "proto program should halt"),
        Err(error) => panic!(
            "PROTO RUN-FAIL ip={} error={error:?} stack={:?} frames={:?}",
            machine.ip(),
            machine.stack(),
            machine.execution_frames()
        ),
    }
    let stack = machine.stack().to_vec();
    eprintln!("P2_PROTO stack={stack:?}");
    // identities p,o,a,b (8) then: r1, o-pair(2), r2, o-pair(2), r3, r4,
    // b-pair(2), a-pair(2): 8 + 12 = 20 trailing observations.
    assert!(stack.len() >= 20, "proto evidence shape, got {stack:?}");
    let tail = &stack[stack.len() - 20..];
    let counter = |v: &Value| match v {
        Value::Int(n) => *n,
        Value::Float(n) => *n as i64,
        other => panic!("proto evidence must be Int, got {other:?}"),
    };
    let v: Vec<i64> = tail.iter().map(counter).collect();
    let (p_slot, p_gen, o_slot, o_gen, a_slot, a_gen, b_slot, b_gen) =
        (v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7]);
    assert_eq!(v[8], 1, "valid prototype link must succeed, got {v:?}");
    assert_eq!(
        (v[9], v[10]),
        (p_slot, p_gen),
        "o must link to p, got {v:?}"
    );
    assert_eq!(v[11], 0, "self prototype cycle must refuse, got {v:?}");
    assert_eq!(
        (v[12], v[13]),
        (p_slot, p_gen),
        "refused self link must leave o unchanged, got {v:?}"
    );
    assert_eq!(v[14], 1, "valid a->b link must succeed, got {v:?}");
    assert_eq!(v[15], 0, "indirect prototype cycle must refuse, got {v:?}");
    assert_eq!(
        (v[16], v[17]),
        (0, 0),
        "refused indirect link must leave b unchanged, got {v:?}"
    );
    assert_eq!(
        (v[18], v[19]),
        (b_slot, b_gen),
        "a->b link must stay intact, got {v:?}"
    );
    let _ = (o_slot, o_gen, a_slot, a_gen);
}

// Private IR interruption fixture, not source loop syntax: reserve a real
// unpublished array node, observe its charged state, then fail before any
// initialization/publication can expose it as a JS value.
struct HeapPendingPlugin {
    collect_pending: bool,
}

impl vm::SourcePlugin for HeapPendingPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-heap-pending"),
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
        for which in [0, 7, 8, 3] {
            heap_cycle_observe(&mut ir, &ctx, ctx.carrier, which, 0, "pending")?;
        }
        let allocation = heap_cycle_take(&mut ir, "pending")?;
        ir.stmts.push(vm::Stmt::Let {
            index: allocation,
            declared_schema: Some(vm::TypeSchema::Named(
                "__CompatEs5Allocation".to_owned(),
                Vec::new(),
            )),
            expr: heap_guest_call(
                ctx.node_begin,
                vec![
                    vm::Expr::Var(ctx.carrier),
                    vm::Expr::Int(6),
                    vm::Expr::Int(1),
                ],
            ),
            line: 1,
        });
        for which in [0, 7, 8, 3] {
            heap_cycle_observe(&mut ir, &ctx, ctx.carrier, which, 0, "pending")?;
        }
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::String("pending-window-ready".to_owned()),
            line: 1,
        });
        if self.collect_pending {
            heap_cycle_collect(&mut ir, &ctx, ctx.carrier, "pending")?;
        } else {
            ir.stmts.push(vm::Stmt::While {
                condition: vm::Expr::Bool(true),
                body: Vec::new(),
                line: 1,
            });
        }
        Ok(ir)
    }
}

fn heap_assert_pending_failure_reset(collect_pending: bool) {
    let plugin: &'static HeapPendingPlugin =
        Box::leak(Box::new(HeapPendingPlugin { collect_pending }));
    let catalog = heap_catalog();
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        "var a=[1];a[0];",
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-heap-pending")),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(plugin)
            .with_host_api_catalog(std::sync::Arc::clone(&catalog)),
    )
    .expect("pending fixture must compile the real source and helpers");
    let mut machine = compilation.into_vm().expect("source-certified pending VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("initialize actual owned arrays");
    let mut registry = vm::HostFunctionRegistry::restricted();
    install_heap_production(&mut registry, &catalog);
    registry
        .bind_vm_cached(&mut machine)
        .expect("bind production adapters");
    machine.set_fuel(1_000_000);
    let outcome = machine.run();
    if collect_pending {
        assert!(
            matches!(&outcome, Err(vm::VmError::HostError(message)) if message.contains("assertion failed")),
            "collect must refuse pending publication, got {outcome:?}"
        );
    } else {
        // This SDK translates its fuel interrupt into a Fuel yield. The
        // embedding aborts here and resets; it never resumes this opaque
        // partially initialized pending node as normal source execution.
        assert!(
            matches!(&outcome, Ok(VmStatus::Yielded)),
            "actual VM fuel must interrupt the pending window, got {outcome:?}"
        );
        assert_eq!(machine.last_yield_reason(), Some(vm::VmYieldReason::Fuel));
    }
    let phase = machine
        .stack()
        .iter()
        .position(|value| *value == Value::string("pending-window-ready"))
        .expect("failure must occur after the real pending allocation and observations");
    assert!(phase >= 8);
    let counters = heap_stale_ints(&machine.stack()[phase - 8..phase]);
    assert_eq!(counters.len(), 8);
    assert_eq!(
        (counters[0], counters[2]),
        (1, 0),
        "one published source array, no pending node"
    );
    assert_eq!(
        (counters[4], counters[6]),
        (1, 1),
        "pending is charged but not published"
    );
    assert_eq!(
        counters[5],
        counters[1] + 3 + 6,
        "full raw node extent remains charged"
    );
    assert!(
        counters[7] >= counters[3],
        "backing cannot disappear on fatal interruption"
    );
    assert_eq!(machine.host_context().resource_count(), 1);
    let stats = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
        .expect("B1 directory remains until reset");
    assert_eq!(
        (stats.pending, stats.registered),
        (0, 1),
        "logical pending differs from B1 owner pending"
    );
    assert!(stats.reserved_bytes > 0);
    assert!(
        machine
            .capture_certification_stats()
            .expect("source facts")
            .valid
    );
    eprintln!(
        "P2_PENDING collect={collect_pending} outcome={outcome:?} reason={:?} counters={counters:?} bytes={}",
        machine.last_yield_reason(),
        stats.reserved_bytes
    );
    machine.reset_for_reuse().expect("reset fatal pending VM");
    assert_eq!(machine.host_context().resource_count(), 0);
    assert!(
        pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine).is_none()
    );
    assert!(
        !machine
            .capture_certification_stats()
            .expect("revoked source facts")
            .valid
    );
}

#[test]
fn p2_heap_pending_fuel_exhaustion_remains_charged_until_reset() {
    heap_assert_pending_failure_reset(false);
}

#[test]
fn p2_heap_pending_collect_refuses_until_reset() {
    heap_assert_pending_failure_reset(true);
}

struct HeapEnvironmentNode {
    slot: u16,
    generation: u16,
    base: u16,
}

// Internal graph fixture only: these are real kind-5 managed nodes, not
// source closures or an implementation of source EnvironmentRecords.
fn heap_environment_node(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
    frame: u16,
    entry: i64,
    values: Vec<[vm::Expr; 5]>,
) -> Result<HeapEnvironmentNode, vm::ParseError> {
    let allocation = heap_cycle_take(ir, "environment")?;
    ir.stmts.push(vm::Stmt::Let {
        index: allocation,
        declared_schema: Some(vm::TypeSchema::Named(
            "__CompatEs5Allocation".to_owned(),
            Vec::new(),
        )),
        expr: heap_guest_call(
            ctx.node_begin,
            vec![
                vm::Expr::Var(ctx.carrier),
                vm::Expr::Int(5 + 5 * values.len() as i64),
                vm::Expr::Int(5),
            ],
        ),
        line: 1,
    });
    let mut fields = Vec::new();
    for name in ["slot", "generation", "base"] {
        let field = heap_cycle_take(ir, "environment")?;
        ir.stmts.push(vm::Stmt::Let {
            index: field,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_field_get(ctx.get, allocation, name),
            line: 1,
        });
        fields.push(field);
    }
    let node = HeapEnvironmentNode {
        slot: fields[0],
        generation: fields[1],
        base: fields[2],
    };
    let address = |offset| {
        vm::Expr::Add(
            Box::new(vm::Expr::Var(node.base)),
            Box::new(vm::Expr::Int(offset)),
        )
    };
    for (offset, value) in [(2, values.len() as i64), (3, 0), (4, 0)] {
        heap_cycle_write(
            ir,
            ctx,
            ctx.carrier,
            address(offset),
            heap_as_number(vm::Expr::Int(value)),
            "environment",
        )?;
    }
    for (row, value) in values.into_iter().enumerate() {
        for (cell, scalar) in value.into_iter().enumerate() {
            heap_cycle_write(
                ir,
                ctx,
                ctx.carrier,
                address(5 + 5 * row as i64 + cell as i64),
                heap_as_number(scalar),
                "environment",
            )?;
        }
    }
    let publish = heap_helper_index(ir, "__compat_es5_node_publish_edge")?;
    let ack = heap_cycle_take(ir, "environment")?;
    ir.stmts.push(vm::Stmt::Let {
        index: ack,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: heap_guest_call(
            publish,
            vec![
                vm::Expr::Var(ctx.carrier),
                vm::Expr::Var(node.slot),
                vm::Expr::Var(node.generation),
                vm::Expr::Var(node.base),
                vm::Expr::Var(frame),
                vm::Expr::Int(entry),
                vm::Expr::Int(6),
            ],
        ),
        line: 1,
    });
    Ok(node)
}

fn heap_environment_cycle(
    ir: &mut vm::FrontendIr,
    ctx: &HeapCycleCtx,
) -> Result<(), vm::ParseError> {
    let function = heap_binding_index(ir, "f")?;
    let (_, fslot, fgen) = heap_cycle_identity(ir, ctx, function, "environment")?;
    let fbase = heap_cycle_base(ir, ctx, ctx.carrier, fslot, fgen, "environment")?;
    let frame = heap_cycle_take(ir, "environment")?;
    ir.stmts.push(vm::Stmt::Let {
        index: frame,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: heap_guest_call(
            ctx.root_enter,
            vec![vm::Expr::Var(ctx.carrier), vm::Expr::Int(2)],
        ),
        line: 1,
    });
    let child = heap_environment_node(ir, ctx, frame, 0, Vec::new())?;
    let parent = heap_environment_node(
        ir,
        ctx,
        frame,
        1,
        vec![
            [
                vm::Expr::Int(5),
                vm::Expr::Float(0.0),
                vm::Expr::Int(0),
                vm::Expr::Var(fslot),
                vm::Expr::Var(fgen),
            ],
            [
                vm::Expr::Int(3),
                vm::Expr::Float(7.0),
                vm::Expr::Int(0),
                vm::Expr::Int(0),
                vm::Expr::Int(0),
            ],
        ],
    )?;
    // f -> child.env -> parent.env -> f, plus a primitive entry with no edge.
    for (base, offset, value) in [
        (fbase, 6, child.slot),
        (fbase, 7, child.generation),
        (child.base, 3, parent.slot),
        (child.base, 4, parent.generation),
    ] {
        heap_cycle_write(
            ir,
            ctx,
            ctx.carrier,
            vm::Expr::Add(
                Box::new(vm::Expr::Var(base)),
                Box::new(vm::Expr::Int(offset)),
            ),
            heap_as_number(vm::Expr::Var(value)),
            "environment",
        )?;
    }
    // Drop every direct environment root: only the source function keeps
    // this graph alive, so the following collect proves transitive tracing.
    let leave = heap_helper_index(ir, "__compat_es5_root_leave")?;
    let ack = heap_cycle_take(ir, "environment")?;
    ir.stmts.push(vm::Stmt::Let {
        index: ack,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: heap_guest_call(
            leave,
            vec![vm::Expr::Var(ctx.carrier), vm::Expr::Var(frame)],
        ),
        line: 1,
    });
    for slot in [
        fslot,
        fgen,
        child.slot,
        child.generation,
        parent.slot,
        parent.generation,
    ] {
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(slot),
            line: 1,
        });
    }
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::String("env-identities".to_owned()),
        line: 1,
    });
    heap_cycle_collect(ir, ctx, ctx.carrier, "environment")?;
    for which in [0, 7, 11, 8] {
        heap_cycle_observe(ir, ctx, ctx.carrier, which, 0, "environment")?;
    }
    for (base, offsets) in [
        (fbase, vec![6, 7, 5]),
        (child.base, vec![3, 4]),
        (parent.base, vec![8, 9, 11]),
    ] {
        for offset in offsets {
            let raw = heap_cycle_read_number(
                ir,
                ctx,
                ctx.carrier,
                vm::Expr::Add(
                    Box::new(vm::Expr::Var(base)),
                    Box::new(vm::Expr::Int(offset)),
                ),
                "environment",
            )?;
            heap_cycle_decode(ir, ctx, raw, "environment")?;
        }
    }
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::String("env-retained".to_owned()),
        line: 1,
    });
    heap_replay_binding(ir, function, ctx.node_begin, ctx.root_enter, ctx.root_write)?;
    heap_cycle_collect(ir, ctx, ctx.carrier, "environment")?;
    for which in [0, 11, 7] {
        heap_cycle_observe(ir, ctx, ctx.carrier, which, 0, "environment")?;
    }
    for slot in [fslot, child.slot, parent.slot] {
        let state = heap_cycle_take(ir, "environment")?;
        ir.stmts.push(vm::Stmt::Let {
            index: state,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: heap_guest_call(
                ctx.observe,
                vec![
                    vm::Expr::Var(ctx.carrier),
                    vm::Expr::Int(13),
                    vm::Expr::Var(slot),
                ],
            ),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(state),
            line: 1,
        });
    }
    let (_, fresh_slot, fresh_gen) = heap_cycle_identity(ir, ctx, function, "environment")?;
    let fresh_base = heap_cycle_base(ir, ctx, ctx.carrier, fresh_slot, fresh_gen, "environment")?;
    let code = heap_cycle_read_number(
        ir,
        ctx,
        ctx.carrier,
        vm::Expr::Add(
            Box::new(vm::Expr::Var(fresh_base)),
            Box::new(vm::Expr::Int(5)),
        ),
        "environment",
    )?;
    heap_cycle_decode(ir, ctx, code, "environment")?;
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::String("env-swept".to_owned()),
        line: 1,
    });
    Ok(())
}

#[test]
fn p2_heap_function_environment_parent_cycle_traces_and_sweeps() {
    let (stack, error) = heap_cycle_run("var f=function(){\"use strict\";return 7;};", 3);
    assert!(
        error.is_none(),
        "real environment helpers must complete, got {error:?}"
    );
    let phase = |marker: &str, count: usize| {
        let end = stack
            .iter()
            .position(|value| *value == Value::string(marker))
            .expect("environment phase reached");
        assert!(end >= count);
        heap_stale_ints(&stack[end - count..end])
    };
    let ids = phase("env-identities", 6);
    let kept = phase("env-retained", 12);
    let swept = phase("env-swept", 9);
    eprintln!("P2_ENV_GRAPH ids={ids:?} retained={kept:?} swept={swept:?}");
    // Function10 + raw3; child environment5 + raw3; parent15 + raw3.
    assert_eq!(&kept[..4], &[3, 10 + 3 + 5 + 3 + 15 + 3, 0, 0]);
    assert_eq!(
        &kept[4..6],
        &ids[2..4],
        "function must keep the child environment"
    );
    assert_eq!(
        &kept[7..9],
        &ids[4..6],
        "child must keep its parent environment"
    );
    assert_eq!(
        &kept[9..11],
        &ids[..2],
        "parent boxed entry must retain the cycle edge"
    );
    assert_eq!(kept[11], 7, "primitive environment payload stays intact");
    assert_eq!(
        &swept[..6],
        &[1, 3, 10 + 3, 3, 3, 3],
        "unrooted cyclic component must be swept"
    );
    assert_ne!(
        swept[6], ids[0],
        "fresh function allocated while the old component was live"
    );
    assert!(swept[7] >= 1);
    assert_eq!(
        swept[8], kept[6],
        "source code identity survives instance replacement"
    );
}
