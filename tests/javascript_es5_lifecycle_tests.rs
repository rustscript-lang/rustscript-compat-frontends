//! P0 source-owned runtime safety envelope for the javascript-es5 frontend.
//!
//! Business tests (`js_*`, `artifact_*`, `cost_*`) prove the envelope using
//! existing production B1 handlers and published Core APIs. `helper_*` tests
//! are fixture/counter validators or fresh-process entry points, never
//! business proofs. This target never imports the 41 bridge tests; its small
//! recipes are narrowly duplicated and labeled.
//!
//! Outcome 1 (this cycle): original raw-JS alias write/read observing Float7,
//! then real guest close + collect through the actual carrier and the real
//! production dispatcher. Payload destruction (`resource_count() == 0`) is
//! reported independently from directory retirement (stats `registered == 0`).

use std::sync::Arc;

use std::cell::{Cell, RefCell};

use pd_vm_compat_frontends::{JAVASCRIPT_ES5_PLUGIN, javascript_es5_flavor};
use vm::{HostApiCatalog, HostFunctionRegistry, Value, Vm, VmStatus};

// ---- Narrow test-owned realm recipe (helper, not a business proof) ----

fn lifecycle_catalog() -> Arc<HostApiCatalog> {
    pd_vm_compat_frontends::reference_array_effective_catalog()
}

fn lifecycle_file_options(catalog: &Arc<HostApiCatalog>) -> vm::CompileSourceFileOptions {
    vm::CompileSourceFileOptions::new()
        .with_source_plugin(&JAVASCRIPT_ES5_PLUGIN)
        .with_host_api_catalog(Arc::clone(catalog))
}

struct LifecycleScratch(std::path::PathBuf);

impl LifecycleScratch {
    fn new(label: &str) -> Self {
        let base = std::env::var_os("TMPDIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let path = base.join(format!("es51-lifecycle-{}-{label}", std::process::id()));
        if path.exists() {
            std::fs::remove_dir_all(&path).expect("clear stale lifecycle scratch");
        }
        std::fs::create_dir_all(&path).expect("create lifecycle scratch");
        Self(path)
    }
}

impl Drop for LifecycleScratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn lifecycle_options_for(
    plugin: &'static dyn vm::SourcePlugin,
    catalog: &Arc<HostApiCatalog>,
) -> vm::CompileSourceFileOptions {
    vm::CompileSourceFileOptions::new()
        .with_source_plugin(plugin)
        .with_host_api_catalog(Arc::clone(catalog))
}

fn install_lifecycle_production(registry: &mut HostFunctionRegistry, catalog: &HostApiCatalog) {
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

fn lifecycle_vm_with_plugin(
    source: &str,
    plugin: &'static dyn vm::SourcePlugin,
    flavor: vm::SourceFlavor,
    native: bool,
) -> (Vm, Arc<HostApiCatalog>) {
    let catalog = lifecycle_catalog();
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        flavor,
        lifecycle_options_for(plugin, &catalog),
    )
    .expect("JS lifecycle compilation should succeed");
    let mut machine = compilation
        .into_vm()
        .expect("certified compilation should create a source-certified VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = HostFunctionRegistry::restricted();
    install_lifecycle_production(&mut registry, &catalog);
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind to this VM");
    if native {
        machine
            .compile_aot()
            .expect("native compilation should succeed");
    }
    (machine, catalog)
}

fn lifecycle_vm(source: &str, native: bool) -> (Vm, Arc<HostApiCatalog>) {
    lifecycle_vm_with_plugin(
        source,
        &JAVASCRIPT_ES5_PLUGIN,
        javascript_es5_flavor(),
        native,
    )
}

/// Unbox one boxed JS Number from an already-run machine's stack slot.
fn boxed_float_at(machine: &Vm, index: usize) -> Value {
    let value = machine
        .stack()
        .get(index)
        .cloned()
        .expect("stack slot should exist");
    let Value::Map(fields) = value else {
        panic!("expected the actual boxed JS value, got {value:?}");
    };
    assert_eq!(
        fields.get(&Value::string("tag")),
        Some(&Value::Int(3)),
        "realm numeric result must carry the Number tag"
    );
    let number = fields
        .get(&Value::string("number"))
        .expect("number payload")
        .clone();
    assert!(
        matches!(number, Value::Float(_)),
        "JS number must remain Float, got {number:?}"
    );
    number
}

// ---- Private lifecycle observation driver (test-only) ----
// Delegates the original real JS source to the REAL JS plugin (real B1
// prelude, carrier, source policy, catalog and certification) and only
// appends genuine calls to the EXISTING generated guest helpers
// `__compat_ref_close` / `__compat_ref_collect` through the ACTUAL carrier
// discovered structurally from real JS lowering. No magic JS syntax, no
// product source recognition: the carrier is the single `Let` whose value is
// a call to the prelude make helper; helper indices come from the generated
// `ir.functions` table, never from user identifier text.

const LIFECYCLE_CLOSE_NAME: &str = "__compat_ref_close";
const LIFECYCLE_COLLECT_NAME: &str = "__compat_ref_collect";
const LIFECYCLE_MAKE_NAME: &str = "__compat_ref_make";
const LIFECYCLE_LENGTH_NAME: &str = "__compat_ref_length";

fn lifecycle_rejection(message: String) -> vm::ParseError {
    vm::ParseError::at_span(vm::compiler::source_map::Span::new(0, 0, 0), message)
}

fn lifecycle_helper_index(ir: &vm::FrontendIr, name: &str) -> Result<u16, vm::ParseError> {
    ir.functions
        .iter()
        .find(|f| f.name == name)
        .map(|f| f.index)
        .ok_or_else(|| lifecycle_rejection(format!("lifecycle fixture requires helper '{name}'")))
}

fn append_lifecycle_driver(
    ir: &mut vm::FrontendIr,
    double_close: bool,
) -> Result<(), vm::ParseError> {
    let make = lifecycle_helper_index(ir, LIFECYCLE_MAKE_NAME)?;
    let close = lifecycle_helper_index(ir, LIFECYCLE_CLOSE_NAME)?;
    let collect = lifecycle_helper_index(ir, LIFECYCLE_COLLECT_NAME)?;
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
        return Err(lifecycle_rejection(format!(
            "lifecycle fixture requires exactly one realm carrier, found {}",
            carriers.len()
        )));
    };
    let carrier = *carrier;
    // Bare value-producing calls leave their results on the stack: the
    // source's own boxed Float7 stays below, then the synchronous close
    // result, then the safety-point collect result on top.
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::Call(close, Vec::new(), vec![vm::Expr::Var(carrier)], None, None),
        line: 1,
    });
    if double_close {
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Call(close, Vec::new(), vec![vm::Expr::Var(carrier)], None, None),
            line: 1,
        });
    }
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::Call(collect, Vec::new(), Vec::new(), None, None),
        line: 1,
    });
    Ok(())
}

struct LifecycleClosePlugin;
struct LifecycleDoubleClosePlugin;

static LIFECYCLE_CLOSE_PLUGIN: LifecycleClosePlugin = LifecycleClosePlugin;
static LIFECYCLE_DOUBLE_CLOSE_PLUGIN: LifecycleDoubleClosePlugin = LifecycleDoubleClosePlugin;

fn lifecycle_flavor(id: &'static str) -> vm::SourceFlavor {
    vm::SourceFlavor::Plugin(vm::FrontendId::new(id))
}

fn lifecycle_descriptor(id: &'static str) -> vm::SourceFrontendDescriptor {
    vm::SourceFrontendDescriptor {
        id: vm::FrontendId::new(id),
        extensions: &["js"],
        typing_policy: vm::FrontendTypingPolicy::Strict,
        supports_ownership_checking: true,
    }
}

impl vm::SourcePlugin for LifecycleClosePlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        lifecycle_descriptor("javascript-es5-lifecycle-close")
    }

    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        append_lifecycle_driver(&mut ir, false)?;
        Ok(ir)
    }
}

impl vm::SourcePlugin for LifecycleDoubleClosePlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        lifecycle_descriptor("javascript-es5-lifecycle-double-close")
    }

    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        append_lifecycle_driver(&mut ir, true)?;
        Ok(ir)
    }
}

const LIFECYCLE_ALIAS_SOURCE: &str = "var a=[1,2];var b=a;b[0]=7;a[0];";

fn run_lifecycle(machine: &mut Vm, native: bool) {
    if native {
        assert_eq!(machine.aot_exec_count(), 0, "no execution before run");
    }
    assert_eq!(
        machine.run().expect("JS lifecycle run should finish"),
        VmStatus::Halted,
        "JS lifecycle program should halt"
    );
}

fn report_lifecycle(machine: &mut Vm, label: &str) {
    let stack = machine.stack().to_vec();
    let resources = machine.host_context().resource_count();
    let stats = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(machine);
    let facts = machine.capture_certification_stats();
    eprintln!("LIFECYCLE_{label} stack={stack:?}");
    eprintln!("LIFECYCLE_{label} resources={resources} stats={stats:?}");
    eprintln!(
        "LIFECYCLE_{label} facts_valid={} facts_entries={}",
        facts.as_ref().is_some_and(|f| f.valid),
        facts.as_ref().map(|f| f.entries).unwrap_or(usize::MAX),
    );
}

#[test]
fn js_lifecycle_close_collect_releases_payload_interpreted() {
    let (mut machine, _) = lifecycle_vm_with_plugin(
        LIFECYCLE_ALIAS_SOURCE,
        &LIFECYCLE_CLOSE_PLUGIN,
        lifecycle_flavor("javascript-es5-lifecycle-close"),
        false,
    );
    run_lifecycle(&mut machine, false);
    report_lifecycle(&mut machine, "CLOSE_INTERPRETED");
    let stack_len = machine.stack().len();
    assert!(stack_len >= 3, "source value plus close plus collect");
    // The source-derived Float7 survives below the lifecycle observations:
    // no data is recovered from stale logical IDs after close.
    assert_eq!(
        boxed_float_at(&machine, stack_len - 3),
        Value::Float(7.0),
        "original raw-JS alias write/read must observe Float7 before cleanup"
    );
    // The guest dispatcher passes the host close result through as a raw
    // guest number: Int(1) is the actual synchronous close result.
    assert_eq!(
        machine.stack()[stack_len - 2],
        Value::Int(1),
        "actual synchronous close result through the production dispatcher"
    );
    assert_eq!(
        machine.stack()[stack_len - 1],
        Value::Int(0),
        "safety-point collect retires metadata without another synchronous close"
    );
    // Payload destruction and directory retirement are independent facts.
    assert_eq!(
        machine.host_context().resource_count(),
        0,
        "close must synchronously destroy the production payload"
    );
    let stats = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
        .expect("directory should exist after the safety point");
    assert_eq!(stats.pending, 0);
    assert_eq!(
        stats.registered, 0,
        "released record must retire at the safety point"
    );
    assert!(
        machine
            .capture_certification_stats()
            .is_some_and(|facts| facts.valid),
        "compiler facts stay valid; they are not the cleanup proof"
    );
}

#[test]
fn js_lifecycle_close_collect_releases_payload_native() {
    let (mut machine, _) = lifecycle_vm_with_plugin(
        LIFECYCLE_ALIAS_SOURCE,
        &LIFECYCLE_CLOSE_PLUGIN,
        lifecycle_flavor("javascript-es5-lifecycle-close"),
        true,
    );
    run_lifecycle(&mut machine, true);
    report_lifecycle(&mut machine, "CLOSE_NATIVE");
    let count = machine.aot_exec_count();
    let info = machine.dump_aot_info();
    eprintln!("LIFECYCLE_CLOSE_NATIVE count={count}\n{info}");
    assert!(
        count > 1,
        "actual native execution must exceed one dispatch"
    );
    assert!(
        info.contains("lowering=ssa"),
        "native lowering must stay ssa"
    );
    let stack_len = machine.stack().len();
    assert_eq!(
        boxed_float_at(&machine, stack_len - 3),
        Value::Float(7.0),
        "native alias write/read must observe Float7 before cleanup"
    );
    assert_eq!(machine.stack()[stack_len - 2], Value::Int(1));
    assert_eq!(machine.stack()[stack_len - 1], Value::Int(0));
    assert_eq!(machine.host_context().resource_count(), 0);
    let stats = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
        .expect("directory should exist after the safety point");
    assert_eq!(stats.registered, 0);
}

#[test]
fn js_lifecycle_second_close_reports_already_closed() {
    let (mut machine, _) = lifecycle_vm_with_plugin(
        LIFECYCLE_ALIAS_SOURCE,
        &LIFECYCLE_DOUBLE_CLOSE_PLUGIN,
        lifecycle_flavor("javascript-es5-lifecycle-double-close"),
        false,
    );
    let outcome = machine.run();
    assert!(
        outcome.is_err(),
        "second close must refuse, got {outcome:?}"
    );
    let text = format!("{outcome:?}");
    eprintln!("LIFECYCLE_DOUBLE_CLOSE refusal: {text}");
    assert!(
        text.contains("AlreadyClosed") || text.contains("already closed"),
        "second close must report AlreadyClosed, distinct from the synchronous close result, got {text}"
    );
}

// ---- Outcome 2: abort, quota, reset and independent VM state ----
// A real source-backed heap program runs to success (or to a legitimate
// runtime abort, never a compiler rejection); resource_count proves payload
// openness independently from the scope directory, and reset_for_reuse must
// drive both to zero/absent while compiler facts are recorded separately.

fn report_reset(machine: &mut Vm, label: &str) {
    let resources = machine.host_context().resource_count();
    let stats = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(machine);
    let facts = machine.capture_certification_stats();
    eprintln!("RESET_{label} resources={resources} stats={stats:?} facts={facts:?}");
}

#[test]
fn js_reset_clears_heap_after_successful_run() {
    let (mut machine, _) = lifecycle_vm(LIFECYCLE_ALIAS_SOURCE, false);
    assert_eq!(
        machine.run().expect("heap program should succeed"),
        VmStatus::Halted
    );
    assert_eq!(
        boxed_float_at(&machine, machine.stack().len() - 1),
        Value::Float(7.0)
    );
    // Independent pre-reset observations: open payload, live directory,
    // valid compiler facts. Directory presence alone is not the proof.
    assert_eq!(
        machine.host_context().resource_count(),
        1,
        "one open realm payload before reset"
    );
    let before = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
        .expect("directory should exist before reset");
    assert_eq!((before.pending, before.registered), (0, 1));
    assert!(
        machine
            .capture_certification_stats()
            .is_some_and(|f| f.valid),
        "compiler facts valid before reset"
    );
    machine.reset_for_reuse().expect("reset should clean up");
    report_reset(&mut machine, "SUCCESS");
    assert_eq!(machine.host_context().resource_count(), 0);
    assert!(
        pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine).is_none(),
        "scope directory must be absent after terminal reset"
    );
    let facts = machine.capture_certification_stats();
    assert!(
        facts.as_ref().is_some_and(|f| !f.valid),
        "reset must revoke source facts, got {facts:?}"
    );
}

#[test]
fn js_two_vms_keep_independent_state_across_reset() {
    let (mut first, _) = lifecycle_vm(LIFECYCLE_ALIAS_SOURCE, false);
    let (mut second, _) = lifecycle_vm("var a=[3,4];var b=a;b[0]=9;a[0];", false);
    assert_eq!(first.run().expect("first VM runs"), VmStatus::Halted);
    assert_eq!(second.run().expect("second VM runs"), VmStatus::Halted);
    assert_eq!(
        boxed_float_at(&first, first.stack().len() - 1),
        Value::Float(7.0)
    );
    assert_eq!(
        boxed_float_at(&second, second.stack().len() - 1),
        Value::Float(9.0)
    );
    assert_eq!(first.host_context().resource_count(), 1);
    assert_eq!(second.host_context().resource_count(), 1);
    // Separate-instance cleanup: resetting one VM must not close or mutate
    // the other. No same-instance reexecution is claimed.
    first.reset_for_reuse().expect("first reset");
    assert_eq!(first.host_context().resource_count(), 0);
    assert_eq!(
        second.host_context().resource_count(),
        1,
        "second VM payload must survive the first VM reset"
    );
    let stats = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut second)
        .expect("second directory must survive");
    assert_eq!((stats.pending, stats.registered), (0, 1));
    assert_eq!(
        boxed_float_at(&second, second.stack().len() - 1),
        Value::Float(9.0),
        "second VM value must survive the first VM reset"
    );
    second.reset_for_reuse().expect("second reset");
    assert_eq!(second.host_context().resource_count(), 0);
}

// Private abort driver (test-only): delegates the original real JS source
// to the REAL JS plugin, then appends one genuine call to the REAL product
// allocator with a zero count. The existing `assertion failed` guard must
// abort BEFORE any resize: this is a runtime abort proof, never a
// compiler/parser rejection (compilation succeeds past source parsing).
struct LifecycleAbortPlugin;

static LIFECYCLE_ABORT_PLUGIN: LifecycleAbortPlugin = LifecycleAbortPlugin;

impl vm::SourcePlugin for LifecycleAbortPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        lifecycle_descriptor("javascript-es5-lifecycle-abort")
    }

    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        let make = lifecycle_helper_index(&ir, LIFECYCLE_MAKE_NAME)?;
        let begin = lifecycle_helper_index(&ir, "__compat_es5_node_begin")?;
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
            return Err(lifecycle_rejection(format!(
                "abort fixture requires exactly one realm carrier, found {}",
                carriers.len()
            )));
        };
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Call(
                begin,
                Vec::new(),
                vec![vm::Expr::Var(*carrier), vm::Expr::Int(0), vm::Expr::Int(1)],
                None,
                None,
            ),
            line: 1,
        });
        Ok(ir)
    }
}

#[test]
fn js_abort_keeps_payload_open_until_reset() {
    let (mut machine, _) = lifecycle_vm_with_plugin(
        "var a=[1];a[0];",
        &LIFECYCLE_ABORT_PLUGIN,
        lifecycle_flavor("javascript-es5-lifecycle-abort"),
        false,
    );
    let error = machine.run().expect_err("zero-count allocation must abort");
    let text = format!("{error:?}");
    eprintln!("ABORT refusal: {text}");
    assert!(
        text.contains("assertion failed"),
        "abort must come from the allocator guard, got {text}"
    );
    // Payload and records survive the abort: cleanup happens only at reset.
    assert_eq!(
        machine.host_context().resource_count(),
        1,
        "realm payload stays open across the aborted call"
    );
    let before = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
        .expect("directory must survive the abort");
    assert_eq!((before.pending, before.registered), (0, 1));
    machine.reset_for_reuse().expect("reset after abort");
    report_reset(&mut machine, "ABORT");
    assert_eq!(machine.host_context().resource_count(), 0);
    assert!(
        pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine).is_none()
    );
}

// Private quota driver (test-only): delegates the original real JS source,
// then issues one genuine resize far beyond a tight existing
// OwnedArrayLimits quota through the ACTUAL carrier. The refusal must cite
// quota while the pre-existing directory stays transactional.
struct LifecycleQuotaPlugin;

static LIFECYCLE_QUOTA_PLUGIN: LifecycleQuotaPlugin = LifecycleQuotaPlugin;

impl vm::SourcePlugin for LifecycleQuotaPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        lifecycle_descriptor("javascript-es5-lifecycle-quota")
    }

    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        let make = lifecycle_helper_index(&ir, LIFECYCLE_MAKE_NAME)?;
        let resize = lifecycle_helper_index(&ir, "__compat_ref_resize")?;
        let length = lifecycle_helper_index(&ir, LIFECYCLE_LENGTH_NAME)?;
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
            return Err(lifecycle_rejection(format!(
                "quota fixture requires exactly one realm carrier, found {}",
                carriers.len()
            )));
        };
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Call(
                length,
                Vec::new(),
                vec![vm::Expr::Var(*carrier)],
                None,
                None,
            ),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::String("lifecycle-resize-ready".to_owned()),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Call(
                resize,
                Vec::new(),
                vec![vm::Expr::Var(*carrier), vm::Expr::Int(200_000)],
                None,
                None,
            ),
            line: 1,
        });
        Ok(ir)
    }
}

#[test]
fn js_tight_quota_refuses_resize_transactionally() {
    // Independent completed-source control supplies the actual pre-resize
    // capacity accounting; quotas are unchanged, not tuned after refusal.
    let (mut control, _) = lifecycle_vm("var a=[1];a[0];", false);
    assert_eq!(
        control.run().expect("quota control should finish"),
        VmStatus::Halted
    );
    assert_eq!(
        boxed_float_at(&control, control.stack().len() - 1),
        Value::Float(1.0)
    );
    let before = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut control)
        .expect("completed-source directory");
    assert_eq!((before.pending, before.registered), (0, 1));
    control.reset_for_reuse().expect("reset quota control");
    assert_eq!(control.host_context().resource_count(), 0);
    let catalog = lifecycle_catalog();
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        "var a=[1];a[0];",
        lifecycle_flavor("javascript-es5-lifecycle-quota"),
        lifecycle_options_for(&LIFECYCLE_QUOTA_PLUGIN, &catalog),
    )
    .expect("quota probe compilation should succeed past source parsing");
    let mut machine = compilation
        .into_vm()
        .expect("certified probe compilation should create a VM");
    let tight = pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::new(8, 64, 1 << 16)
        .expect("tight but positive quotas");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(&mut machine, tight)
        .expect("tight initialization should succeed");
    let mut registry = HostFunctionRegistry::restricted();
    install_lifecycle_production(&mut registry, &catalog);
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind");
    let error = machine.run().expect_err("over-quota resize must refuse");
    let text = format!("{error:?}");
    eprintln!("QUOTA refusal: {text}");
    assert!(
        text.contains("quota"),
        "refusal must cite quota transactionally, got {text}"
    );
    // Failed host-call operands remain above earlier source results.
    let phase = machine
        .stack()
        .iter()
        .position(|value| *value == Value::string("lifecycle-resize-ready"))
        .expect("quota probe must finish its source and reach the intended resize");
    assert!(
        phase >= 2,
        "source result and actual B1 length precede the phase"
    );
    assert_eq!(boxed_float_at(&machine, phase - 2), Value::Float(1.0));
    let length = match &machine.stack()[phase - 1] {
        Value::Float(length) => *length,
        Value::Int(length) => *length as f64,
        other => {
            panic!("quota phase must contain the actual numeric backing length, got {other:?}")
        }
    };
    // Initial layout, derived independently of the failing request:
    // prefix80 + script frame(4+2*3) + page(3+64*7) + array(3+5+1).
    const INITIAL_CELLS: usize = 80 + (4 + 2 * 3) + (3 + 64 * 7) + (3 + 5 + 1);
    assert_eq!(length, INITIAL_CELLS as f64);
    assert!(before.reserved_bytes >= INITIAL_CELLS * std::mem::size_of::<Value>());
    assert!(before.reserved_bytes <= 1 << 16);
    // The pre-existing owner survives with old accounting intact.
    assert_eq!(machine.host_context().resource_count(), 1);
    let stats = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
        .expect("directory must survive the refused resize");
    assert_eq!((stats.pending, stats.registered), (0, 1));
    assert_eq!(
        stats.reserved_bytes, before.reserved_bytes,
        "refused resize must preserve independently observed capacity accounting"
    );
    eprintln!(
        "QUOTA_TRANSACTION length={length} before_bytes={} after_bytes={}",
        before.reserved_bytes, stats.reserved_bytes
    );
    machine
        .reset_for_reuse()
        .expect("reset after quota refusal");
    assert_eq!(machine.host_context().resource_count(), 0);
}

// Private registration-quota driver (test-only): delegates the original real
// JS source to the REAL JS plugin, then appends a SECOND genuine call to the
// UNCHANGED B1 `__compat_ref_make` factory through existing typed IR. The
// second factory receives a real nonempty numeric Core staging array built
// with the real `array_new`/`array_push` builtins (Float9, no raw user JS to
// the TS parser). A real `__compat_ref_length` use of the FIRST carrier
// follows the second factory call so the normal lifetime analysis sees a
// genuine future use of the first owner; it is unreachable at runtime once
// the expected refusal fires. Ordinary two-literal JS can never trigger this
// path: every literal in one script shares the single realm carrier and its
// one registered owner, so the second factory here is an honestly labeled
// private protocol stress observation, not a second JS realm.
struct LifecycleRegisterQuotaPlugin;

static LIFECYCLE_REGISTER_QUOTA_PLUGIN: LifecycleRegisterQuotaPlugin = LifecycleRegisterQuotaPlugin;

impl vm::SourcePlugin for LifecycleRegisterQuotaPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        lifecycle_descriptor("javascript-es5-lifecycle-register-quota")
    }

    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        append_register_quota_driver(&mut ir)?;
        Ok(ir)
    }
}

fn append_register_quota_driver(ir: &mut vm::FrontendIr) -> Result<(), vm::ParseError> {
    let make = lifecycle_helper_index(ir, LIFECYCLE_MAKE_NAME)?;
    let length = lifecycle_helper_index(ir, LIFECYCLE_LENGTH_NAME)?;
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
        return Err(lifecycle_rejection(format!(
            "register-quota fixture requires exactly one realm carrier, found {}",
            carriers.len()
        )));
    };
    let carrier = *carrier;
    let base = u16::try_from(ir.locals).map_err(|_| {
        lifecycle_rejection("register-quota fixture exhausted local slot space".to_owned())
    })?;
    let staging = base.checked_add(0).ok_or_else(|| {
        lifecycle_rejection("register-quota fixture exhausted local slot space".to_owned())
    })?;
    let staged = base.checked_add(1).ok_or_else(|| {
        lifecycle_rejection("register-quota fixture exhausted local slot space".to_owned())
    })?;
    let second = base.checked_add(2).ok_or_else(|| {
        lifecycle_rejection("register-quota fixture exhausted local slot space".to_owned())
    })?;
    ir.locals = usize::from(base).checked_add(3).ok_or_else(|| {
        lifecycle_rejection("register-quota fixture exhausted local slot space".to_owned())
    })?;
    let array_new = vm::BuiltinFunction::ArrayNew.call_index();
    let array_push = vm::BuiltinFunction::ArrayPush.call_index();
    ir.stmts.push(vm::Stmt::Let {
        index: staging,
        declared_schema: None,
        expr: vm::Expr::Call(array_new, Vec::new(), Vec::new(), None, None),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::Let {
        index: staged,
        declared_schema: None,
        expr: vm::Expr::Call(
            array_push,
            Vec::new(),
            vec![vm::Expr::Var(staging), vm::Expr::Float(9.0)],
            None,
            None,
        ),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::Let {
        index: second,
        declared_schema: None,
        expr: vm::Expr::Call(make, Vec::new(), vec![vm::Expr::Var(staged)], None, None),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::Call(length, Vec::new(), vec![vm::Expr::Var(carrier)], None, None),
        line: 1,
    });
    Ok(())
}

#[test]
fn js_make_second_registration_refuses_with_pending_live() {
    for native in [false, true] {
        let label = if native {
            "REGISTER_QUOTA_NATIVE"
        } else {
            "REGISTER_QUOTA"
        };
        let catalog = lifecycle_catalog();
        let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
            "var a=[1];a[0];",
            lifecycle_flavor("javascript-es5-lifecycle-register-quota"),
            lifecycle_options_for(&LIFECYCLE_REGISTER_QUOTA_PLUGIN, &catalog),
        )
        .expect("register-quota probe compilation should succeed past source parsing");
        let mut machine = compilation
            .into_vm()
            .expect("certified probe compilation should create a VM");
        let tight = pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::new(8, 1, 1 << 20)
            .expect("positive pending/registered/payload quotas");
        pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
            &mut machine,
            tight,
        )
        .expect("tight initialization should succeed");
        let mut registry = HostFunctionRegistry::restricted();
        install_lifecycle_production(&mut registry, &catalog);
        registry
            .bind_vm_cached(&mut machine)
            .expect("production adapters should bind");
        if native {
            machine
                .compile_aot()
                .expect("native compilation should succeed");
        }
        let error = machine
            .run()
            .expect_err("second factory registration must refuse");
        let text = format!("{error:?}");
        eprintln!("{label} refusal: {text}");
        assert!(
            text.contains("metadata quota"),
            "refusal must cite the registered metadata quota, got {text}"
        );
        assert!(
            !text.contains("owner mismatch")
                && !text.contains("payload quota")
                && !text.contains("pending quota")
                && !text.contains("catalog")
                && !text.contains("host import"),
            "refusal must not be a validation/setup error, got {text}"
        );
        // The second factory allocated and pushed a genuine typed resource,
        // then its register refused: two live resources, one pending orphan
        // plus the first registered owner.
        assert_eq!(
            machine.host_context().resource_count(),
            2,
            "allocated-but-unregistered second resource must survive, native={native}"
        );
        let stats = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
            .expect("directory must survive the refused registration");
        assert_eq!((stats.pending, stats.registered), (1, 1));
        assert!(
            stats.reserved_bytes > 0,
            "reserved bytes must stay meaningful, got {stats:?}"
        );
        eprintln!(
            "{label} resources=2 pending={} registered={} reserved_bytes={}",
            stats.pending, stats.registered, stats.reserved_bytes
        );
        let facts = machine.capture_certification_stats();
        assert!(
            facts.as_ref().is_some_and(|f| f.valid),
            "compiler facts stay valid; they are not the refusal proof, got {facts:?}"
        );
        eprintln!(
            "{label} facts_valid={} facts_entries={}",
            facts.as_ref().is_some_and(|f| f.valid),
            facts.as_ref().map(|f| f.entries).unwrap_or(usize::MAX),
        );
        machine
            .reset_for_reuse()
            .expect("reset after registration refusal");
        report_reset(&mut machine, label);
        assert_eq!(machine.host_context().resource_count(), 0);
        assert!(
            pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
                .is_none(),
            "scope directory must be absent after terminal reset"
        );
    }
}

// Strengthened authority-negative observer: an uncertified Program reaches
// the runner (compile succeeds), then fails CLOSED at runtime. Error phase,
// unconditional directory stats, capture validity and resource count are all
// recorded; setup/catalog errors can never satisfy these assertions.
#[test]
fn js_uncertified_program_fails_closed_without_source_authority() {
    let catalog = lifecycle_catalog();
    let uncertified = vm::compile_source_with_flavor_and_options(
        LIFECYCLE_ALIAS_SOURCE,
        javascript_es5_flavor(),
        lifecycle_file_options(&catalog),
    )
    .expect("uncertified compile should succeed past source parsing");
    let mut machine = Vm::new(uncertified.program);
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("init should succeed");
    let mut registry = HostFunctionRegistry::restricted();
    install_lifecycle_production(&mut registry, &catalog);
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind to this VM");
    let outcome = machine.run();
    assert!(
        outcome.is_err(),
        "uncertified source must fail closed, got {outcome:?}"
    );
    let text = format!("{outcome:?}");
    eprintln!("NEGATIVE refusal: {text}");
    assert!(
        text.contains("certif") || text.contains("source"),
        "refusal must cite source authority, got {text}"
    );
    assert!(
        !text.contains("catalog") && !text.contains("host import"),
        "must not be an import/catalog setup refusal: {text}"
    );
    // Unconditional observations: allocation itself refuses, so no owner
    // registers, no scope directory is ever created, no payload exists and
    // no source facts are present. Directory absence is only one of four
    // independent facts, never the sole proof.
    let stats = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine);
    assert!(
        stats.is_none(),
        "uncertified allocation must fail before any directory exists, got {stats:?}"
    );
    assert_eq!(machine.host_context().resource_count(), 0);
    assert!(
        machine.capture_certification_stats().is_none(),
        "uncertified Program must carry no source facts"
    );
}

// ---- Outcome 3: artifact authority ----
// Exact existing VMBC/AOT/compiler/loader APIs with the exact effective
// catalog. Artifact setup/catalog/loader errors are never counted as
// source-authority results.

fn write_js_file(dir: &std::path::Path, name: &str, source: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, source).expect("write JS source file");
    assert_eq!(
        std::fs::read(&path).expect("read back JS source"),
        source.as_bytes()
    );
    path
}

fn file_certified_vm(path: &std::path::Path) -> (Vm, Arc<HostApiCatalog>) {
    let catalog = lifecycle_catalog();
    let compilation = vm::compile_source_file_with_capture_certification_and_options(
        path,
        lifecycle_file_options(&catalog),
    )
    .expect("source-certified JS file compilation should succeed");
    let mut machine = compilation
        .into_vm()
        .expect("certified file compilation should create a source-certified VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = HostFunctionRegistry::restricted();
    install_lifecycle_production(&mut registry, &catalog);
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind to this VM");
    (machine, catalog)
}

#[test]
fn artifact_source_certified_file_executes_positive() {
    let scratch = LifecycleScratch::new("artifact-positive");
    let main = write_js_file(&scratch.0, "main.js", LIFECYCLE_ALIAS_SOURCE);
    let (mut machine, _) = file_certified_vm(&main);
    assert_eq!(
        machine.run().expect("certified file should run"),
        VmStatus::Halted
    );
    assert_eq!(
        boxed_float_at(&machine, machine.stack().len() - 1),
        Value::Float(7.0),
        "source-certified file execution is the positive control"
    );
    assert_eq!(machine.host_context().resource_count(), 1);
    assert!(
        machine
            .capture_certification_stats()
            .is_some_and(|f| f.valid),
        "certified file VM must carry valid source facts"
    );
    eprintln!("ARTIFACT_POSITIVE ok resources=1 facts_valid=true");
}

#[test]
fn artifact_aot_companion_load_revokes_facts_but_keeps_open_payload() {
    let (mut machine, _) = lifecycle_vm(LIFECYCLE_ALIAS_SOURCE, false);
    assert_eq!(
        machine.run().expect("certified run should halt"),
        VmStatus::Halted
    );
    assert_eq!(
        boxed_float_at(&machine, machine.stack().len() - 1),
        Value::Float(7.0)
    );
    assert_eq!(machine.host_context().resource_count(), 1);
    assert!(
        machine
            .capture_certification_stats()
            .is_some_and(|f| f.valid)
    );
    machine
        .compile_aot()
        .expect("native compilation should succeed");
    let scratch = LifecycleScratch::new("artifact-aot");
    let companion = scratch.0.join("realm.pat");
    machine
        .save_aot_artifact_to_file(&companion)
        .expect("real AOT companion save should succeed");
    let bytes = std::fs::read(&companion).expect("companion file must exist");
    assert!(!bytes.is_empty(), "companion must carry a real artifact");
    eprintln!("ARTIFACT_AOT saved bytes={}", bytes.len());
    // Safe nonexecuting phase: the VM is halted, never inside a host
    // callback. Loading revokes source facts without releasing the open
    // payload; Released must not be inferred from revoked facts.
    machine
        .load_aot_artifact_from_file(&companion)
        .expect("companion load at a safe phase should succeed");
    eprintln!("ARTIFACT_AOT loaded at halt");
    let facts = machine.capture_certification_stats();
    assert!(
        facts.as_ref().is_some_and(|f| !f.valid),
        "companion load must revoke source facts, got {facts:?}"
    );
    assert_eq!(
        machine.host_context().resource_count(),
        1,
        "open payload with unavailable authority: facts revoked, payload NOT released"
    );
    let stats = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
        .expect("directory must survive the companion load");
    assert_eq!((stats.pending, stats.registered), (0, 1));
    machine.reset_for_reuse().expect("actual reset cleanup");
    assert_eq!(machine.host_context().resource_count(), 0);
    eprintln!("ARTIFACT_AOT reset cleaned the open payload");
}

// Fresh-process fixture entry (helper identity, never a business proof).
// The business test below self-spawns this exact test binary with
// `--exact helper_fresh_process_vmbc_entry` plus ES51_FRESH_FIXTURE set; the
// fixture then loads a source-deleted standalone VMBC through the real exact
// services/catalog and reports machine-readable markers before exiting.
#[test]
fn helper_fresh_process_vmbc_entry() {
    if std::env::var("ES51_FRESH_FIXTURE").as_deref() != Ok("vmbc-deny") {
        return;
    }
    let artifact = std::env::var("ES51_FIXTURE_ARTIFACT").expect("fixture artifact path");
    let bytes = std::fs::read(&artifact).expect("fixture must read the artifact bytes");
    let outcome: String;
    let resources: usize;
    let stack_len: usize;
    let stack_top: String;
    let facts: String;
    match vm::decode_program(&bytes) {
        Ok(program) => {
            let mut machine = Vm::new(program);
            pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
                &mut machine,
                pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
            )
            .expect("fixture init should succeed");
            let catalog = lifecycle_catalog();
            let mut registry = HostFunctionRegistry::restricted();
            install_lifecycle_production(&mut registry, &catalog);
            registry
                .bind_vm_cached(&mut machine)
                .expect("fixture bind should succeed");
            match machine.run() {
                Ok(status) => {
                    outcome = format!("halted-unexpected:{status:?}");
                }
                Err(error) => {
                    outcome = format!("denied:{error:?}");
                }
            }
            resources = machine.host_context().resource_count();
            stack_len = machine.stack().len();
            stack_top = machine
                .stack()
                .last()
                .map(|v| format!("{v:?}"))
                .unwrap_or_else(|| "empty".to_owned());
            facts = match machine.capture_certification_stats() {
                None => "none".to_owned(),
                Some(f) if f.valid => "valid".to_owned(),
                Some(_) => "invalid".to_owned(),
            };
        }
        Err(error) => {
            outcome = format!("setup-decode-failed:{error:?}");
            resources = usize::MAX;
            stack_len = usize::MAX;
            stack_top = "unknown".to_owned();
            facts = "unknown".to_owned();
        }
    }
    let flat = outcome.replace('\n', "|");
    println!("ES51_FIXTURE_OUTCOME={flat}");
    println!("ES51_FIXTURE_RESOURCES={resources}");
    println!("ES51_FIXTURE_STACK={stack_len}");
    println!("ES51_FIXTURE_STACK_TOP={stack_top}");
    println!("ES51_FIXTURE_FACTS={facts}");
    std::process::exit(0);
}

fn run_fresh_fixture(artifact: &std::path::Path) -> std::process::Output {
    let exe = std::env::current_exe().expect("current test executable");
    let mut child = std::process::Command::new(exe)
        .arg("--exact")
        .arg("helper_fresh_process_vmbc_entry")
        .arg("--nocapture")
        .env("ES51_FRESH_FIXTURE", "vmbc-deny")
        .env("ES51_FIXTURE_ARTIFACT", artifact)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn fresh fixture process");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll fixture process") {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("fresh fixture process exceeded its bound");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let output = child.wait_with_output().expect("collect fixture output");
    assert!(
        status.success(),
        "fixture process must report, got {status:?}"
    );
    output
}

fn fixture_marker(stdout: &str, name: &str) -> String {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{name}=")).map(str::to_owned))
        .unwrap_or_else(|| panic!("fixture must print {name}; stdout:\n{stdout}"))
}

#[test]
fn p5_logical_fuel_interrupt_keeps_owner_charged_until_reset() {
    // A finite admitted source, not a recursive/capture extension. Its actual
    // alias comparison precedes a labeled test-only numeric phase observation.
    let source = "function f(a){'use strict';return a&&a;}var o={s:'live'};var r=f(o);r===o;4242.5;var after='later';";
    let (mut machine, _) = lifecycle_vm(source, false);
    let tag = Value::string("tag");
    let number = Value::string("number");
    let flag = Value::string("flag");
    let mut reached = false;
    machine.set_fuel(0);
    for _ in 0..200_000 {
        // Preserve unpaid metering debt across yields; replacing the balance
        // with one repeatedly cannot fund a multi-unit existing host operation.
        machine.add_fuel(1).expect("bounded cumulative fuel credit");
        let status = machine.run().expect("bounded fuel step");
        let phase = machine.stack().iter().position(|value| {
            matches!(value, Value::Map(fields) if fields.len()==5 && fields.get(&tag)==Some(&Value::Int(3)) && fields.get(&number)==Some(&Value::Float(4242.5)))
        });
        if let Some(position) = phase {
            let Some(Value::Map(result)) = position
                .checked_sub(1)
                .and_then(|index| machine.stack().get(index))
            else {
                panic!("actual logical alias comparison before the phase")
            };
            assert_eq!(result.get(&tag), Some(&Value::Int(2)));
            assert_eq!(result.get(&flag), Some(&Value::Bool(true)));
            assert_eq!(status, VmStatus::Yielded);
            assert_eq!(machine.last_yield_reason(), Some(vm::VmYieldReason::Fuel));
            reached = true;
            break;
        }
        assert_eq!(status, VmStatus::Yielded, "phase must precede halt");
    }
    assert!(
        reached,
        "logical source phase within the original 200000 total fuel bound"
    );
    assert_eq!(machine.host_context().resource_count(), 1);
    let stats = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
        .expect("charged owner");
    assert_eq!((stats.pending, stats.registered), (0, 1));
    eprintln!("P5_FUEL actual_alias=true reason=Fuel resources=1 pending=0 registered=1");
    machine.reset_for_reuse().expect("fuel reset");
    assert_eq!(machine.host_context().resource_count(), 0);
    assert!(
        pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine).is_none()
    );
}

fn p5_assert_artifact_authority_refusal(stdout: &str) {
    // The existing VMBC fixture prints its first marker after the test harness
    // status prefix; preserve the fixture and parse only that exact prefix.
    let outcome = stdout
        .lines()
        .find_map(|line| {
            line.strip_prefix("test helper_fresh_process_vmbc_entry ... ")
                .unwrap_or(line)
                .strip_prefix("ES51_FIXTURE_OUTCOME=")
        })
        .expect("fixture authority outcome marker");
    assert!(outcome.starts_with("denied:"), "{outcome}");
    assert!(
        outcome.contains("certif") || outcome.contains("source"),
        "{outcome}"
    );
    assert!(
        !outcome.contains("catalog")
            && !outcome.contains("host import")
            && !outcome.contains("No such file"),
        "{outcome}"
    );
    assert_eq!(fixture_marker(stdout, "ES51_FIXTURE_RESOURCES"), "0");
    assert_eq!(fixture_marker(stdout, "ES51_FIXTURE_STACK"), "1");
    assert_eq!(
        fixture_marker(stdout, "ES51_FIXTURE_STACK_TOP"),
        "Array([])"
    );
}

#[test]
fn p5_logical_source_deleted_artifacts_do_not_gain_realm_authority() {
    let scratch = LifecycleScratch::new("p5-logical-artifact");
    let source = "function f(a){'use strict';return a&&a;}var a=[7];var b=f(a);false&&(a=[]);var r=b||a;r===b;";
    let main = write_js_file(&scratch.0, "main.js", source);
    let (mut machine, _) = file_certified_vm(&main);
    assert_eq!(
        machine.run().expect("logical source positive"),
        VmStatus::Halted
    );
    let Value::Map(result) = machine.stack().last().expect("logical result") else {
        panic!("boxed result")
    };
    assert_eq!(result.get(&Value::string("flag")), Some(&Value::Bool(true)));
    assert_eq!(machine.host_context().resource_count(), 1);
    let artifact = scratch.0.join("logical.vmbc");
    std::fs::write(
        &artifact,
        vm::encode_program(machine.program()).expect("VMBC"),
    )
    .expect("artifact write");
    std::fs::remove_file(&main).expect("source deletion");
    assert!(!main.exists());
    let output = run_fresh_fixture(&artifact);
    let stdout = String::from_utf8(output.stdout).expect("fixture UTF8");
    let stderr = String::from_utf8(output.stderr).expect("fixture UTF8");
    eprintln!("P5_ARTIFACT_FRESH stdout={stdout}\nstderr={stderr}");
    p5_assert_artifact_authority_refusal(&stdout);
    assert_eq!(fixture_marker(&stdout, "ES51_FIXTURE_RESOURCES"), "0");
    assert_eq!(fixture_marker(&stdout, "ES51_FIXTURE_FACTS"), "none");
    machine.reset_for_reuse().expect("positive reset");
    assert_eq!(machine.host_context().resource_count(), 0);
}

#[test]
fn p5_logical_source_deleted_aot_pair_does_not_gain_realm_authority() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(|| {
            let scratch = LifecycleScratch::new("p5-logical-aot-artifact");
            let source =
                "function f(a){'use strict';return a||a;}var o={s:'live'};var r=f(o);r===o;";
            let main = write_js_file(&scratch.0, "main.js", source);
            let (mut machine, _) = file_certified_vm(&main);
            machine
                .compile_aot()
                .expect("logical source native compilation");
            assert_eq!(
                machine.run().expect("logical native positive"),
                VmStatus::Halted
            );
            let Value::Map(result) = machine.stack().last().expect("logical result") else {
                panic!("boxed result")
            };
            assert_eq!(result.get(&Value::string("flag")), Some(&Value::Bool(true)));
            let count = machine.aot_exec_count();
            let info = machine.dump_aot_info();
            assert!(count > 0 && info.contains("lowering=ssa"));
            eprintln!("P5_ARTIFACT_NATIVE actual_vm_aot_exec_count={count}\n{info}");
            let artifact = scratch.0.join("logical.vmbc");
            std::fs::write(
                &artifact,
                vm::encode_program(machine.program()).expect("VMBC"),
            )
            .expect("artifact write");
            let aot = scratch.0.join("logical.pat");
            machine.save_aot_artifact_to_file(&aot).expect("AOT save");
            std::fs::remove_file(&main).expect("source deletion");
            assert!(!main.exists());
            let output = run_fresh_aot_fixture(&artifact, &aot);
            let stdout = String::from_utf8(output.stdout).expect("fixture UTF8");
            let stderr = String::from_utf8(output.stderr).expect("fixture UTF8");
            eprintln!("P5_ARTIFACT_FRESH_AOT stdout={stdout}\nstderr={stderr}");
            assert_eq!(fixture_marker(&stdout, "ES51_AOT_LOAD"), "ok");
            assert_eq!(
                fixture_marker(&stdout, "ES51_AOT_RESOURCES_AFTER_LOAD"),
                "0"
            );
            assert_ne!(
                fixture_marker(&stdout, "ES51_AOT_FACTS_AFTER_LOAD"),
                "valid"
            );
            p5_assert_artifact_authority_refusal(&stdout);
            assert_eq!(fixture_marker(&stdout, "ES51_FIXTURE_RESOURCES"), "0");
            assert_ne!(fixture_marker(&stdout, "ES51_FIXTURE_FACTS"), "valid");
            machine.reset_for_reuse().expect("positive reset");
            assert_eq!(machine.host_context().resource_count(), 0);
        })
        .expect("native stack worker")
        .join()
        .expect("artifact assertions");
}

#[test]
fn artifact_source_deleted_vmbc_denied_in_fresh_process() {
    let scratch = LifecycleScratch::new("artifact-fresh");
    let main = write_js_file(&scratch.0, "main.js", LIFECYCLE_ALIAS_SOURCE);
    // Positive control first: the same file executes source-certified.
    let (mut machine, _) = file_certified_vm(&main);
    assert_eq!(
        machine.run().expect("positive control runs"),
        VmStatus::Halted
    );
    assert_eq!(
        boxed_float_at(&machine, machine.stack().len() - 1),
        Value::Float(7.0)
    );
    // Emit a real standalone VMBC from an uncertified compilation of the
    // same bytes (bytecode carries no source authority either way).
    let catalog = lifecycle_catalog();
    let plain = vm::compile_source_file_with_options(&main, lifecycle_file_options(&catalog))
        .expect("plain file compilation for VMBC emission");
    let bytes = vm::encode_program(&plain.program).expect("real VMBC emission");
    assert!(!bytes.is_empty());
    let artifact = scratch.0.join("realm.vmbc");
    std::fs::write(&artifact, &bytes).expect("write standalone artifact");
    eprintln!("ARTIFACT_FRESH emitted bytes={}", bytes.len());
    // Delete the original source: the fresh process sees only the artifact.
    std::fs::remove_file(&main).expect("delete original source");
    assert!(!main.exists(), "source must be gone before the fresh run");
    let output = run_fresh_fixture(&artifact);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let _stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let outcome = fixture_marker(&stdout, "ES51_FIXTURE_OUTCOME");
    let resources = fixture_marker(&stdout, "ES51_FIXTURE_RESOURCES");
    let stack = fixture_marker(&stdout, "ES51_FIXTURE_STACK");
    let stack_top = fixture_marker(&stdout, "ES51_FIXTURE_STACK_TOP");
    let facts = fixture_marker(&stdout, "ES51_FIXTURE_FACTS");
    eprintln!("ARTIFACT_FRESH outcome={outcome}");
    eprintln!("ARTIFACT_FRESH resources={resources} stack={stack} top={stack_top} facts={facts}");
    assert!(
        outcome.starts_with("denied:"),
        "source-deleted artifact must be safely denied, got {outcome}"
    );
    assert!(
        outcome.contains("certif") || outcome.contains("source"),
        "denial must cite source authority, got {outcome}"
    );
    assert!(
        !outcome.contains("catalog")
            && !outcome.contains("host import")
            && !outcome.contains("No such file"),
        "denial must not be a setup/catalog error, got {outcome}"
    );
    assert_eq!(
        resources, "0",
        "no payload may be created without authority"
    );
    // The denied allocation gate leaves exactly the unevaluated builtin
    // argument (the empty initial array, an authority-free Core value) on
    // the stack: no owner, no payload (resources 0) and no source facts.
    assert_eq!(
        stack, "1",
        "exactly the residual builtin argument, got top={stack_top}"
    );
    assert_eq!(
        stack_top, "Array([])",
        "residual must be the authority-free empty array, got {stack_top}"
    );
    assert_eq!(facts, "none", "standalone artifact carries no source facts");
}

// Fresh-process AOT fixture entry (helper identity, never a business proof).
// The business test below self-spawns this exact test binary with `--exact
// helper_fresh_process_aot_entry` plus ES51_FRESH_FIXTURE=aot-deny; the fixture
// decodes a source-deleted matched VMBC, binds the exact production
// services/catalog, loads the matched AOT companion at a safe nonexecuting
// phase (recording load success independently), and only then runs the actual
// heap program, reporting the authority refusal before exiting.
#[test]
fn helper_fresh_process_aot_entry() {
    if std::env::var("ES51_FRESH_FIXTURE").as_deref() != Ok("aot-deny") {
        return;
    }
    let vmbc_path = std::env::var("ES51_FIXTURE_ARTIFACT").expect("fixture VMBC path");
    let aot_path = std::env::var("ES51_FIXTURE_AOT").expect("fixture AOT path");
    let vmbc_bytes = std::fs::read(&vmbc_path).expect("fixture must read the VMBC bytes");
    let aot_bytes = std::fs::read(&aot_path).expect("fixture must read the AOT bytes");
    println!("ES51_AOT_BYTES={}", aot_bytes.len());
    let outcome: String;
    let resources: String;
    let stack_len: String;
    let stack_top: String;
    let facts: String;
    let mut load_marker = String::from("skipped-decode-failed");
    let mut load_facts = String::from("unknown");
    let mut load_resources = String::from("unknown");
    let mut load_info = String::from("unknown");
    match vm::decode_program(&vmbc_bytes) {
        Ok(program) => {
            let mut machine = Vm::new(program);
            pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
                &mut machine,
                pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
            )
            .expect("fixture init should succeed");
            let catalog = lifecycle_catalog();
            let mut registry = HostFunctionRegistry::restricted();
            install_lifecycle_production(&mut registry, &catalog);
            registry
                .bind_vm_cached(&mut machine)
                .expect("fixture bind should succeed");
            match machine.load_aot_artifact_from_file(&aot_path) {
                Ok(()) => {
                    load_marker = String::from("ok");
                    load_facts = match machine.capture_certification_stats() {
                        None => "none".to_owned(),
                        Some(f) if f.valid => "valid".to_owned(),
                        Some(_) => "invalid".to_owned(),
                    };
                    load_resources = machine.host_context().resource_count().to_string();
                    load_info = machine
                        .dump_aot_info()
                        .replace('\n', "|")
                        .chars()
                        .take(512)
                        .collect();
                    match machine.run() {
                        Ok(status) => {
                            outcome = format!("halted-unexpected:{status:?}");
                        }
                        Err(error) => {
                            outcome = format!("denied:{error:?}");
                        }
                    }
                    resources = machine.host_context().resource_count().to_string();
                    stack_len = machine.stack().len().to_string();
                    stack_top = machine
                        .stack()
                        .last()
                        .map(|v| format!("{v:?}"))
                        .unwrap_or_else(|| "empty".to_owned());
                    facts = match machine.capture_certification_stats() {
                        None => "none".to_owned(),
                        Some(f) if f.valid => "valid".to_owned(),
                        Some(_) => "invalid".to_owned(),
                    };
                }
                Err(error) => {
                    load_marker = format!("load-failed:{error:?}").replace('\n', "|");
                    outcome = format!("setup-aot-load-failed:{error:?}").replace('\n', "|");
                    resources = String::from("unknown");
                    stack_len = String::from("unknown");
                    stack_top = String::from("unknown");
                    facts = String::from("unknown");
                }
            }
        }
        Err(error) => {
            outcome = format!("setup-decode-failed:{error:?}").replace('\n', "|");
            resources = String::from("unknown");
            stack_len = String::from("unknown");
            stack_top = String::from("unknown");
            facts = String::from("unknown");
        }
    }
    let flat = outcome.replace('\n', "|");
    println!("ES51_AOT_LOAD={load_marker}");
    println!("ES51_AOT_FACTS_AFTER_LOAD={load_facts}");
    println!("ES51_AOT_RESOURCES_AFTER_LOAD={load_resources}");
    println!("ES51_AOT_INFO={load_info}");
    println!("ES51_FIXTURE_OUTCOME={flat}");
    println!("ES51_FIXTURE_RESOURCES={resources}");
    println!("ES51_FIXTURE_STACK={stack_len}");
    println!("ES51_FIXTURE_STACK_TOP={stack_top}");
    println!("ES51_FIXTURE_FACTS={facts}");
    std::process::exit(0);
}

fn run_fresh_aot_fixture(vmbc: &std::path::Path, aot: &std::path::Path) -> std::process::Output {
    let exe = std::env::current_exe().expect("current test executable");
    let mut child = std::process::Command::new(exe)
        .arg("--exact")
        .arg("helper_fresh_process_aot_entry")
        .arg("--nocapture")
        .env("ES51_FRESH_FIXTURE", "aot-deny")
        .env("ES51_FIXTURE_ARTIFACT", vmbc)
        .env("ES51_FIXTURE_AOT", aot)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn fresh AOT fixture process");
    // Companion loading recompiles native code; keep its host deadline finite
    // and distinct from the VMBC-only fixture's deadline.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(900);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll fixture process") {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("fresh AOT fixture process exceeded its bound");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let output = child.wait_with_output().expect("collect fixture output");
    assert!(
        status.success(),
        "fixture process must report, got {status:?}"
    );
    output
}

#[test]
fn artifact_source_deleted_vmbc_aot_pair_denied_in_fresh_process() {
    let scratch = LifecycleScratch::new("artifact-fresh-aot");
    let main = write_js_file(&scratch.0, "main.js", LIFECYCLE_ALIAS_SOURCE);
    // Positive control first: the same file executes source-certified.
    let (mut machine, _) = file_certified_vm(&main);
    assert_eq!(
        machine.run().expect("positive control runs"),
        VmStatus::Halted
    );
    assert_eq!(
        boxed_float_at(&machine, machine.stack().len() - 1),
        Value::Float(7.0)
    );
    assert_eq!(machine.host_context().resource_count(), 1);
    assert!(
        machine
            .capture_certification_stats()
            .is_some_and(|f| f.valid),
        "positive control must carry valid source facts"
    );
    eprintln!("ARTIFACT_FRESH_AOT positive ok resources=1 facts_valid=true");
    // Matched pair from the SAME program/VM: VMBC encoded from the positive
    // program, AOT compiled and saved from the same machine.
    let vmbc = vm::encode_program(machine.program())
        .expect("real VMBC emission from the positive program");
    assert!(!vmbc.is_empty());
    let vmbc_path = scratch.0.join("realm.vmbc");
    std::fs::write(&vmbc_path, &vmbc).expect("write VMBC companion");
    machine
        .compile_aot()
        .expect("native compilation of the positive program should succeed");
    let aot_path = scratch.0.join("realm.pat");
    machine
        .save_aot_artifact_to_file(&aot_path)
        .expect("real AOT companion save should succeed");
    let aot_bytes = std::fs::read(&aot_path).expect("companion file must exist");
    assert!(
        !aot_bytes.is_empty(),
        "companion must carry a real artifact"
    );
    eprintln!(
        "ARTIFACT_FRESH_AOT emitted vmbc_bytes={} aot_bytes={}",
        vmbc.len(),
        aot_bytes.len()
    );
    // Delete the original source: the fresh process sees only the artifacts.
    std::fs::remove_file(&main).expect("delete original source");
    assert!(!main.exists(), "source must be gone before the fresh run");
    let output = run_fresh_aot_fixture(&vmbc_path, &aot_path);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let _stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let aot_load = fixture_marker(&stdout, "ES51_AOT_LOAD");
    let aot_facts = fixture_marker(&stdout, "ES51_AOT_FACTS_AFTER_LOAD");
    let aot_resources = fixture_marker(&stdout, "ES51_AOT_RESOURCES_AFTER_LOAD");
    let aot_info = fixture_marker(&stdout, "ES51_AOT_INFO");
    let aot_bytes_marker = fixture_marker(&stdout, "ES51_AOT_BYTES");
    let outcome = fixture_marker(&stdout, "ES51_FIXTURE_OUTCOME");
    let resources = fixture_marker(&stdout, "ES51_FIXTURE_RESOURCES");
    let stack = fixture_marker(&stdout, "ES51_FIXTURE_STACK");
    let stack_top = fixture_marker(&stdout, "ES51_FIXTURE_STACK_TOP");
    let facts = fixture_marker(&stdout, "ES51_FIXTURE_FACTS");
    eprintln!(
        "ARTIFACT_FRESH_AOT load={aot_load} load_facts={aot_facts} load_resources={aot_resources} bytes={aot_bytes_marker}"
    );
    eprintln!("ARTIFACT_FRESH_AOT info={aot_info}");
    eprintln!("ARTIFACT_FRESH_AOT outcome={outcome}");
    eprintln!(
        "ARTIFACT_FRESH_AOT resources={resources} stack={stack} top={stack_top} facts={facts}"
    );
    // Load success and run refusal are independent phases: a missing file,
    // catalog mismatch, program mismatch or loader error fails here and can
    // never count as the authority guard below.
    assert_eq!(
        aot_load, "ok",
        "matched AOT companion must load at a safe phase, got {aot_load}"
    );
    assert_eq!(
        aot_resources, "0",
        "companion load must allocate nothing by itself"
    );
    assert!(
        outcome.starts_with("denied:"),
        "source-deleted pair must be safely denied after load, got {outcome}"
    );
    assert!(
        outcome.contains("certif") || outcome.contains("source"),
        "denial must cite source authority, got {outcome}"
    );
    assert!(
        !outcome.contains("catalog")
            && !outcome.contains("host import")
            && !outcome.contains("No such file"),
        "denial must not be a setup/catalog error, got {outcome}"
    );
    assert_eq!(
        resources, "0",
        "no payload may be created without authority"
    );
    // The denied allocation gate leaves exactly the unevaluated builtin
    // argument (the empty initial array, an authority-free Core value) on
    // the stack: no owner, no payload (resources 0) and no source facts.
    assert_eq!(
        stack, "1",
        "exactly the residual builtin argument, got top={stack_top}"
    );
    assert_eq!(
        stack_top, "Array([])",
        "residual must be the authority-free empty array, got {stack_top}"
    );
    assert!(
        facts == "none" || facts == "invalid",
        "loaded pair must carry no usable source facts, got {facts}"
    );
}

// ---- Outcome 4: two backing sizes and ordinary-write cost ----
// TWO actual realm backing extents (32 and 8192 cells) run the same ordinary
// alias write workload through the actual B1 callbacks with real
// cost/accounting observations. Initialization/growth is explicitly separate
// from the post-initialization workload: `a=[0]` (4 cells) plus a padding
// string of length `backing-7` (header 3) grows the SAME realm backing
// without deep numeric literals, and supported original JS `b[0]=sentinel;`
// statements form the workload. A test-only adapter wrapper delegates the
// EXACT production `arrays::write` (schema, binding kind, effects, resource
// types, call style and ORIGINAL handler all preserved) while a separate
// test-binary thread-local System counter measures allocation work ONLY
// during the real original callback. Reserved-bytes flatness alone is never
// claimed as no-copy proof: per-write bytes, snapshot use and static routing
// are independent facts. The cost conclusion is scoped to the original
// ordinary-write path and the fixed frontend wrapper.

std::thread_local! {
    static COST_ENABLED: Cell<bool> = const { Cell::new(false) };
    static COST_BYTES: Cell<u64> = const { Cell::new(0) };
    static COST_ALLOCS: Cell<u64> = const { Cell::new(0) };
    static WRITE_OBS: RefCell<Vec<CostWriteObs>> = const { RefCell::new(Vec::new()) };
    static SNAPSHOT_TRIPS: Cell<u64> = const { Cell::new(0) };
}

struct CostCountingAlloc;

unsafe impl std::alloc::GlobalAlloc for CostCountingAlloc {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        if COST_ENABLED.get() {
            COST_BYTES.set(COST_BYTES.get().saturating_add(layout.size() as u64));
            COST_ALLOCS.set(COST_ALLOCS.get().saturating_add(1));
        }
        unsafe { std::alloc::System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        unsafe { std::alloc::System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
        if COST_ENABLED.get() {
            COST_BYTES.set(COST_BYTES.get().saturating_add(new_size as u64));
            COST_ALLOCS.set(COST_ALLOCS.get().saturating_add(1));
        }
        unsafe { std::alloc::System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static COST_ALLOC: CostCountingAlloc = CostCountingAlloc;

#[derive(Clone, Debug)]
struct CostWriteObs {
    index: Option<f64>,
    value: Option<f64>,
    ok: bool,
    bytes: u64,
    allocs: u64,
    cert_valid: Option<bool>,
    registered_before: Option<usize>,
    registered_after: Option<usize>,
    reserved_before: Option<usize>,
    reserved_after: Option<usize>,
    resources_before: usize,
    resources_after: usize,
}

static ORIGINAL_WRITE: std::sync::OnceLock<vm::StaticHostFunction> = std::sync::OnceLock::new();
static ORIGINAL_WRITE_ARGS: std::sync::OnceLock<vm::StaticHostArgsFunction> =
    std::sync::OnceLock::new();
static ORIGINAL_SNAPSHOT: std::sync::OnceLock<vm::StaticHostFunction> = std::sync::OnceLock::new();
static ORIGINAL_SNAPSHOT_ARGS: std::sync::OnceLock<vm::StaticHostArgsFunction> =
    std::sync::OnceLock::new();

fn cost_number(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Float(f)) => Some(*f),
        Some(Value::Int(i)) => Some(*i as f64),
        _ => None,
    }
}

fn cost_write_wrapper(machine: &mut Vm, args: &[Value]) -> vm::VmResult<vm::CallOutcome> {
    let original = *ORIGINAL_WRITE
        .get()
        .expect("production arrays::write must be installed");
    // Decode arguments BEFORE the counting window: observer work is never
    // attributed to the original handler.
    let index = cost_number(args.get(1));
    let value = cost_number(args.get(2));
    let cert_before = machine.capture_certification_stats().map(|s| s.valid);
    let stats_before = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(machine);
    let res_before = machine.host_context().resource_count();
    let (bytes_before, allocs_before) = (COST_BYTES.get(), COST_ALLOCS.get());
    COST_ENABLED.set(true);
    let outcome = original(machine, args);
    COST_ENABLED.set(false);
    let stats_after = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(machine);
    let obs = CostWriteObs {
        index,
        value,
        ok: outcome.is_ok(),
        bytes: COST_BYTES.get().saturating_sub(bytes_before),
        allocs: COST_ALLOCS.get().saturating_sub(allocs_before),
        cert_valid: cert_before,
        registered_before: stats_before.map(|s| s.registered),
        registered_after: stats_after.map(|s| s.registered),
        reserved_before: stats_before.map(|s| s.reserved_bytes),
        reserved_after: stats_after.map(|s| s.reserved_bytes),
        resources_before: res_before,
        resources_after: machine.host_context().resource_count(),
    };
    WRITE_OBS.with(|seen| seen.borrow_mut().push(obs));
    outcome
}

fn cost_write_args_wrapper(args: &[Value]) -> vm::VmResult<vm::CallOutcome> {
    let original = *ORIGINAL_WRITE_ARGS
        .get()
        .expect("production arrays::write (args form) must be installed");
    let index = cost_number(args.get(1));
    let value = cost_number(args.get(2));
    let (bytes_before, allocs_before) = (COST_BYTES.get(), COST_ALLOCS.get());
    COST_ENABLED.set(true);
    let outcome = original(args);
    COST_ENABLED.set(false);
    WRITE_OBS.with(|seen| {
        seen.borrow_mut().push(CostWriteObs {
            index,
            value,
            ok: outcome.is_ok(),
            bytes: COST_BYTES.get().saturating_sub(bytes_before),
            allocs: COST_ALLOCS.get().saturating_sub(allocs_before),
            cert_valid: None,
            registered_before: None,
            registered_after: None,
            reserved_before: None,
            reserved_after: None,
            resources_before: usize::MAX,
            resources_after: usize::MAX,
        });
    });
    outcome
}

fn cost_snapshot_tripwire(machine: &mut Vm, args: &[Value]) -> vm::VmResult<vm::CallOutcome> {
    SNAPSHOT_TRIPS.with(|trips| trips.set(trips.get().saturating_add(1)));
    let original = *ORIGINAL_SNAPSHOT
        .get()
        .expect("production arrays::snapshot must be installed");
    original(machine, args)
}

fn cost_snapshot_args_tripwire(args: &[Value]) -> vm::VmResult<vm::CallOutcome> {
    SNAPSHOT_TRIPS.with(|trips| trips.set(trips.get().saturating_add(1)));
    let original = *ORIGINAL_SNAPSHOT_ARGS
        .get()
        .expect("production arrays::snapshot (args form) must be installed");
    original(args)
}

fn cost_variant_name(adapter: &vm::HostAdapterDescriptor) -> &'static str {
    match adapter {
        vm::HostAdapterDescriptor::Static(_) => "Static",
        vm::HostAdapterDescriptor::StaticStack(_) => "StaticStack",
        vm::HostAdapterDescriptor::StaticStackRuntimeOwned(_) => "StaticStackRuntimeOwned",
        vm::HostAdapterDescriptor::StaticArgs(_) => "StaticArgs",
        vm::HostAdapterDescriptor::StaticNonYieldingArgs(_) => "StaticNonYieldingArgs",
        vm::HostAdapterDescriptor::Owned(_) => "Owned",
    }
}

struct CostInstall {
    write_variant: String,
    snapshot_variant: String,
    write_binding: String,
    write_effects: usize,
    write_resource_types: usize,
    fingerprint_matches_production: bool,
}

// Test-only adapter swap: every descriptor keeps its EXACT production
// schema, binding kind, effects and resource types; only the `arrays::write`
// and `arrays::snapshot` adapters delegate through the wrappers above to
// the ORIGINAL production functions. The guest-visible catalog fingerprint
// must equal the production effective catalog.
fn install_cost_registry(registry: &mut HostFunctionRegistry) -> CostInstall {
    let modules = [
        pd_vm_compat_frontends::runtime::array::typescript_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module(),
        pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module(),
    ];
    let mut descriptors = Vec::new();
    for module in modules {
        descriptors.extend(module.descriptors());
    }
    let mut install = CostInstall {
        write_variant: String::new(),
        snapshot_variant: String::new(),
        write_binding: String::new(),
        write_effects: 0,
        write_resource_types: 0,
        fingerprint_matches_production: false,
    };
    for descriptor in &mut descriptors {
        if descriptor.schema.name == "arrays::write" {
            assert!(
                !descriptor.schema.name.is_empty() && install.write_variant.is_empty(),
                "exactly one arrays::write descriptor must be wrapped"
            );
            install.write_variant = cost_variant_name(&descriptor.adapter).to_owned();
            install.write_binding = format!("{:?}", descriptor.binding.kind);
            install.write_effects = descriptor.effects.len();
            install.write_resource_types = descriptor.resource_types.len();
            match descriptor.adapter {
                vm::HostAdapterDescriptor::Static(original) => {
                    ORIGINAL_WRITE.set(original).ok();
                    descriptor.adapter = vm::HostAdapterDescriptor::Static(cost_write_wrapper);
                }
                vm::HostAdapterDescriptor::StaticStack(original) => {
                    ORIGINAL_WRITE.set(original).ok();
                    descriptor.adapter = vm::HostAdapterDescriptor::StaticStack(cost_write_wrapper);
                }
                vm::HostAdapterDescriptor::StaticStackRuntimeOwned(original) => {
                    ORIGINAL_WRITE.set(original).ok();
                    descriptor.adapter =
                        vm::HostAdapterDescriptor::StaticStackRuntimeOwned(cost_write_wrapper);
                }
                vm::HostAdapterDescriptor::StaticArgs(original) => {
                    ORIGINAL_WRITE_ARGS.set(original).ok();
                    descriptor.adapter =
                        vm::HostAdapterDescriptor::StaticArgs(cost_write_args_wrapper);
                }
                vm::HostAdapterDescriptor::StaticNonYieldingArgs(original) => {
                    ORIGINAL_WRITE_ARGS.set(original).ok();
                    descriptor.adapter =
                        vm::HostAdapterDescriptor::StaticNonYieldingArgs(cost_write_args_wrapper);
                }
                vm::HostAdapterDescriptor::Owned(_) => {
                    panic!("arrays::write must stay a static adapter for exact delegation");
                }
            }
        }
        if descriptor.schema.name == "arrays::snapshot" {
            assert!(
                install.snapshot_variant.is_empty(),
                "exactly one arrays::snapshot descriptor must be wrapped"
            );
            install.snapshot_variant = cost_variant_name(&descriptor.adapter).to_owned();
            match descriptor.adapter {
                vm::HostAdapterDescriptor::Static(original) => {
                    ORIGINAL_SNAPSHOT.set(original).ok();
                    descriptor.adapter = vm::HostAdapterDescriptor::Static(cost_snapshot_tripwire);
                }
                vm::HostAdapterDescriptor::StaticStack(original) => {
                    ORIGINAL_SNAPSHOT.set(original).ok();
                    descriptor.adapter =
                        vm::HostAdapterDescriptor::StaticStack(cost_snapshot_tripwire);
                }
                vm::HostAdapterDescriptor::StaticStackRuntimeOwned(original) => {
                    ORIGINAL_SNAPSHOT.set(original).ok();
                    descriptor.adapter =
                        vm::HostAdapterDescriptor::StaticStackRuntimeOwned(cost_snapshot_tripwire);
                }
                vm::HostAdapterDescriptor::StaticArgs(original) => {
                    ORIGINAL_SNAPSHOT_ARGS.set(original).ok();
                    descriptor.adapter =
                        vm::HostAdapterDescriptor::StaticArgs(cost_snapshot_args_tripwire);
                }
                vm::HostAdapterDescriptor::StaticNonYieldingArgs(original) => {
                    ORIGINAL_SNAPSHOT_ARGS.set(original).ok();
                    descriptor.adapter = vm::HostAdapterDescriptor::StaticNonYieldingArgs(
                        cost_snapshot_args_tripwire,
                    );
                }
                vm::HostAdapterDescriptor::Owned(_) => {
                    panic!("arrays::snapshot must stay a static adapter for exact delegation");
                }
            }
        }
    }
    assert!(
        !install.write_variant.is_empty() && !install.snapshot_variant.is_empty(),
        "both production adapters must be found and wrapped"
    );
    let aggregated = vm::HostModuleDescriptor::install_descriptors(registry, &descriptors)
        .expect("cost registry must install the production-identical descriptors");
    // Same authorizations the production `install_from_catalog` path applies
    // per descriptor; `install_descriptors` leaves them to the caller.
    for descriptor in &descriptors {
        registry.authorize_registered_builtin_import(&descriptor.schema.name);
    }
    registry
        .allow_builtin("math::floor")
        .expect("math::floor should be allow-listable");
    install.fingerprint_matches_production = format!("{:?}", aggregated.fingerprint())
        == format!("{:?}", lifecycle_catalog().fingerprint());
    install
}

// Parse-time routing probe (test-only observation, never product source
// recognition): delegates the original real JS source to the REAL JS plugin
// unchanged, and counts calls to the snapshot guest helper in the emitted
// IR. Initialization header/data writes are ordinary write-helper calls;
// only the snapshot helper would indicate a whole-buffer copy path.
static ROUTE_SNAPSHOT_CALLS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

struct CostRouteProbePlugin {
    managed_payload: usize,
}

// Explicit private IR fixture: enlarge the real pending array reservation,
// initialize its additional numeric cells and length before real publication.
// This does not claim source loop syntax or successful 8192-literal codegen.
fn cost_extend_managed_array(
    ir: &mut vm::FrontendIr,
    payload: usize,
) -> Result<(), vm::ParseError> {
    let begin = lifecycle_helper_index(ir, "__compat_es5_node_begin")?;
    let write = lifecycle_helper_index(ir, "__compat_ref_write")?;
    let get = vm::BuiltinFunction::Get.call_index();
    let binding = ir
        .local_bindings
        .iter()
        .rfind(|(name, _)| name == "a")
        .map(|(_, slot)| *slot)
        .ok_or_else(|| lifecycle_rejection("managed cost fixture requires a".to_owned()))?;
    let publish_at = ir
        .stmts
        .iter()
        .position(
            |statement| matches!(statement, vm::Stmt::Assign { index, .. } if *index == binding),
        )
        .ok_or_else(|| {
            lifecycle_rejection("managed cost fixture requires array publication".to_owned())
        })?;
    let mut reservation = None;
    for statement in &mut ir.stmts[..publish_at] {
        if let vm::Stmt::Let {
            index,
            expr: vm::Expr::Call(target, _, args, _, _),
            ..
        } = statement
            && *target == begin
            && args.len() == 3
        {
            let (vm::Expr::Var(carrier), vm::Expr::Int(cells), vm::Expr::Int(1)) =
                (&args[0], &args[1], &args[2])
            else {
                return Err(lifecycle_rejection(
                    "cost fixture requires a real array reservation".to_owned(),
                ));
            };
            let old_payload = usize::try_from(*cells - 5)
                .map_err(|_| lifecycle_rejection("cost fixture array layout".to_owned()))?;
            if payload < old_payload || payload > 8192 || reservation.is_some() {
                return Err(lifecycle_rejection(
                    "cost fixture requires one bounded managed array".to_owned(),
                ));
            }
            reservation = Some((*index, *carrier, old_payload));
            args[1] = vm::Expr::Int(5 + payload as i64);
        }
    }
    let (allocation, carrier, old_payload) = reservation
        .ok_or_else(|| lifecycle_rejection("cost fixture requires node-begin".to_owned()))?;
    if payload == old_payload {
        return Ok(());
    }
    let base = ir.stmts[..publish_at].iter().find_map(|statement| match statement {
        vm::Stmt::Let { index, expr: vm::Expr::Call(target, _, args, _, _), .. }
            if *target == get && matches!(args.as_slice(),
                [vm::Expr::Var(owner), vm::Expr::String(field)] if *owner == allocation && field == "base") => Some(*index),
        _ => None,
    }).ok_or_else(|| lifecycle_rejection("cost fixture requires the real allocation base".to_owned()))?;
    let iter = u16::try_from(ir.locals)
        .map_err(|_| lifecycle_rejection("cost fixture slot space".to_owned()))?;
    let ack = iter
        .checked_add(1)
        .ok_or_else(|| lifecycle_rejection("cost fixture slot space".to_owned()))?;
    ir.locals += 2;
    let as_number =
        |expression| vm::Expr::Div(Box::new(expression), Box::new(vm::Expr::Float(1.0)));
    let store = |offset, number| {
        vm::Expr::Call(
            write,
            Vec::new(),
            vec![
                vm::Expr::Var(carrier),
                as_number(vm::Expr::Add(
                    Box::new(vm::Expr::Var(base)),
                    Box::new(offset),
                )),
                number,
            ],
            None,
            None,
        )
    };
    ir.stmts.splice(
        publish_at..publish_at,
        vec![
            vm::Stmt::Let {
                index: iter,
                declared_schema: Some(vm::TypeSchema::Int),
                expr: vm::Expr::Int(old_payload as i64),
                line: 1,
            },
            vm::Stmt::Let {
                index: ack,
                declared_schema: Some(vm::TypeSchema::Number),
                expr: vm::Expr::Float(0.0),
                line: 1,
            },
            vm::Stmt::While {
                condition: vm::Expr::Lt(
                    Box::new(as_number(vm::Expr::Var(iter))),
                    Box::new(vm::Expr::Float(payload as f64)),
                ),
                body: vec![
                    vm::Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: ack,
                        expr: store(
                            vm::Expr::Add(
                                Box::new(vm::Expr::Int(5)),
                                Box::new(vm::Expr::Var(iter)),
                            ),
                            vm::Expr::Float(0.0),
                        ),
                        line: 1,
                    },
                    vm::Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: iter,
                        expr: vm::Expr::Add(
                            Box::new(vm::Expr::Var(iter)),
                            Box::new(vm::Expr::Int(1)),
                        ),
                        line: 1,
                    },
                ],
                line: 1,
            },
            vm::Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: ack,
                expr: store(vm::Expr::Int(2), vm::Expr::Float(payload as f64)),
                line: 1,
            },
        ],
    );
    Ok(())
}

fn route_count_snapshot(expr: &vm::Expr, snapshot: Option<u16>) -> usize {
    let Some(target) = snapshot else {
        return 0;
    };
    match expr {
        vm::Expr::Call(index, _, args, _, _) => {
            usize::from(*index == target)
                + args
                    .iter()
                    .map(|arg| route_count_snapshot(arg, snapshot))
                    .sum::<usize>()
        }
        vm::Expr::Block { stmts, expr } => {
            stmts
                .iter()
                .map(|stmt| route_count_snapshot_stmt(stmt, snapshot))
                .sum::<usize>()
                + route_count_snapshot(expr, snapshot)
        }
        vm::Expr::Add(l, r)
        | vm::Expr::Sub(l, r)
        | vm::Expr::Mul(l, r)
        | vm::Expr::Div(l, r)
        | vm::Expr::Eq(l, r)
        | vm::Expr::Lt(l, r)
        | vm::Expr::Gt(l, r)
        | vm::Expr::And(l, r)
        | vm::Expr::Or(l, r) => {
            route_count_snapshot(l, snapshot) + route_count_snapshot(r, snapshot)
        }
        vm::Expr::Not(inner) | vm::Expr::Neg(inner) | vm::Expr::ToOwned(inner) => {
            route_count_snapshot(inner, snapshot)
        }
        vm::Expr::IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            route_count_snapshot(condition, snapshot)
                + route_count_snapshot(then_expr, snapshot)
                + route_count_snapshot(else_expr, snapshot)
        }
        _ => 0,
    }
}

fn route_count_snapshot_stmt(stmt: &vm::Stmt, snapshot: Option<u16>) -> usize {
    match stmt {
        vm::Stmt::Let { expr, .. } | vm::Stmt::Expr { expr, .. } => {
            route_count_snapshot(expr, snapshot)
        }
        vm::Stmt::Assign { expr, .. } => route_count_snapshot(expr, snapshot),
        vm::Stmt::IfElse {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            route_count_snapshot(condition, snapshot)
                + then_branch
                    .iter()
                    .map(|s| route_count_snapshot_stmt(s, snapshot))
                    .sum::<usize>()
                + else_branch
                    .iter()
                    .map(|s| route_count_snapshot_stmt(s, snapshot))
                    .sum::<usize>()
        }
        vm::Stmt::While {
            condition, body, ..
        } => {
            route_count_snapshot(condition, snapshot)
                + body
                    .iter()
                    .map(|s| route_count_snapshot_stmt(s, snapshot))
                    .sum::<usize>()
        }
        _ => 0,
    }
}

impl vm::SourcePlugin for CostRouteProbePlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        lifecycle_descriptor("javascript-es5-cost-route")
    }

    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        cost_extend_managed_array(&mut ir, self.managed_payload)?;
        let snapshot = ir
            .functions
            .iter()
            .find(|f| f.name == "__compat_ref_snapshot_values")
            .map(|f| f.index);
        let calls = ir
            .stmts
            .iter()
            .map(|stmt| route_count_snapshot_stmt(stmt, snapshot))
            .sum::<usize>();
        // The guest prelude's own dispatcher body may reference snapshot for
        // operation 6; only calls in the APPENDED realm statements (past the
        // prelude function bodies, i.e. top-level stmts) matter, and the
        // top-level scan above covers exactly those.
        ROUTE_SNAPSHOT_CALLS.store(calls, std::sync::atomic::Ordering::SeqCst);
        // Test-only readback: actual checked physical base and current
        // backing length, rather than a retired fixed address/layout.
        let make = lifecycle_helper_index(&ir, LIFECYCLE_MAKE_NAME)?;
        let checked = lifecycle_helper_index(&ir, "__compat_es5_checked_base")?;
        let length = lifecycle_helper_index(&ir, LIFECYCLE_LENGTH_NAME)?;
        let binding = ir
            .local_bindings
            .iter()
            .rfind(|(name, _)| name == "a")
            .map(|(_, slot)| *slot)
            .ok_or_else(|| lifecycle_rejection("cost fixture requires array a".to_owned()))?;
        let carriers: Vec<u16> = ir
            .stmts
            .iter()
            .filter_map(|statement| match statement {
                vm::Stmt::Let {
                    index,
                    expr: vm::Expr::Call(target, _, args, _, _),
                    ..
                } if *target == make && args.len() == 1 => Some(*index),
                _ => None,
            })
            .collect();
        let [carrier] = carriers.as_slice() else {
            return Err(lifecycle_rejection(
                "cost fixture requires one real carrier".to_owned(),
            ));
        };
        let carrier = *carrier;
        let base = u16::try_from(ir.locals)
            .map_err(|_| lifecycle_rejection("cost observer slot space exhausted".to_owned()))?;
        ir.locals += 1;
        let get = vm::BuiltinFunction::Get.call_index();
        let field = |name: &str| {
            vm::Expr::Call(
                get,
                Vec::new(),
                vec![vm::Expr::Var(binding), vm::Expr::String(name.to_owned())],
                None,
                None,
            )
        };
        ir.stmts.push(vm::Stmt::Let {
            index: base,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: vm::Expr::Call(
                checked,
                Vec::new(),
                vec![
                    vm::Expr::Var(carrier),
                    field("ref_slot"),
                    field("generation"),
                ],
                None,
                None,
            ),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(base),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Call(length, Vec::new(), vec![vm::Expr::Var(carrier)], None, None),
            line: 1,
        });
        Ok(ir)
    }
}

fn cost_vm(source: &str, payload: usize) -> (Vm, CostInstall) {
    let catalog = lifecycle_catalog();
    let plugin: &'static CostRouteProbePlugin = Box::leak(Box::new(CostRouteProbePlugin {
        managed_payload: payload,
    }));
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        lifecycle_flavor("javascript-es5-cost-route"),
        lifecycle_options_for(plugin, &catalog),
    )
    .expect("cost workload compilation should succeed");
    let mut machine = compilation
        .into_vm()
        .expect("certified cost compilation should create a source-certified VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = HostFunctionRegistry::restricted();
    let install = install_cost_registry(&mut registry);
    registry
        .bind_vm_cached(&mut machine)
        .expect("cost adapters should bind to this VM");
    machine.set_fuel(100_000_000);
    (machine, install)
}

#[test]
fn helper_cost_counter_observes_real_allocations() {
    // Fixture validator (helper identity): proves the thread-local System
    // counter is live, so a measured zero is a real zero, never a fake.
    COST_ENABLED.set(false);
    COST_BYTES.set(0);
    COST_ALLOCS.set(0);
    let quiet = vec![0u8; 512];
    std::hint::black_box(&quiet);
    assert_eq!(COST_BYTES.get(), 0, "disabled window must count nothing");
    COST_ENABLED.set(true);
    let loud = vec![0u8; 1024];
    COST_ENABLED.set(false);
    std::hint::black_box(&loud);
    assert!(
        COST_BYTES.get() >= 1024,
        "enabled window must observe the real allocation, got {}",
        COST_BYTES.get()
    );
    assert!(
        COST_ALLOCS.get() >= 1,
        "enabled window must observe the allocation event"
    );
    COST_BYTES.set(0);
    COST_ALLOCS.set(0);
}

#[test]
fn cost_ordinary_writes_do_not_copy_backing() {
    const WORKLOAD_WRITES: usize = 32;
    const FIRST_SENTINEL: i64 = 1001;
    let sentinels: Vec<f64> = (0..WORKLOAD_WRITES)
        .map(|i| (FIRST_SENTINEL + i as i64) as f64)
        .collect();
    let mut summary: Vec<(usize, u64, u64, u64, usize)> = Vec::new();
    let mut variants = Vec::new();
    for payload in [32usize, 8192usize] {
        WRITE_OBS.with(|seen| seen.borrow_mut().clear());
        SNAPSHOT_TRIPS.with(|trips| trips.set(0));
        // Both cases delegate this same admitted source. The large array
        // is an explicitly IR-initialized real managed node, not raw padding.
        let mut source = format!("var a=[{}];var b=a;", vec!["0"; 32].join(","));
        for i in 0..WORKLOAD_WRITES {
            source.push_str(&format!("b[0]={};", FIRST_SENTINEL + i as i64));
        }
        source.push_str("a[0];");
        let (mut machine, install) = cost_vm(&source, payload);
        assert!(
            install.fingerprint_matches_production,
            "adapter swap must stay invisible to the guest contract"
        );
        variants.push(install.write_variant.clone());
        assert_eq!(
            machine.run().expect("cost workload should halt"),
            VmStatus::Halted
        );
        assert_eq!(
            boxed_float_at(&machine, machine.stack().len() - 3),
            Value::Float((FIRST_SENTINEL + WORKLOAD_WRITES as i64 - 1) as f64),
            "final alias read must observe the last sentinel"
        );
        let stack = machine.stack();
        let base = cost_number(stack.get(stack.len() - 2)).expect("checked managed array base");
        let backing = cost_number(stack.last()).expect("actual metadata-inclusive backing length");
        let expected_index = base + 5.0;
        assert!(base >= 80.0);
        assert!(backing >= expected_index + payload as f64);
        let seen = WRITE_OBS.with(|obs| obs.borrow().clone());
        let trips = SNAPSHOT_TRIPS.with(|trips| trips.get());
        let routed = ROUTE_SNAPSHOT_CALLS.load(std::sync::atomic::Ordering::SeqCst);
        let workload: Vec<&CostWriteObs> = seen
            .iter()
            .filter(|obs| obs.value.is_some_and(|v| sentinels.contains(&v)))
            .collect();
        let init: Vec<&CostWriteObs> = seen
            .iter()
            .filter(|obs| !obs.value.is_some_and(|v| sentinels.contains(&v)))
            .collect();
        eprintln!(
            "COST payload={payload} backing={backing} expected_index={expected_index} total_write_callbacks={} init={} workload={} snapshot_trips={trips} route_scan={routed} variant={}",
            seen.len(),
            init.len(),
            workload.len(),
            install.write_variant,
        );
        assert_eq!(
            workload.len(),
            WORKLOAD_WRITES,
            "every ordinary write must reach the real handler"
        );
        assert!(
            !init.is_empty(),
            "initialization must pass through the same production path"
        );
        for write in &workload {
            assert_eq!(
                write.index,
                Some(expected_index),
                "workload targets the first payload cell, got {write:?}"
            );
            assert!(write.ok, "original handler must succeed, got {write:?}");
            assert_eq!(write.cert_valid, Some(true));
            assert_eq!((write.resources_before, write.resources_after), (1, 1));
            assert_eq!(
                (write.registered_before, write.registered_after),
                (Some(1), Some(1))
            );
            assert_eq!(
                write.reserved_before, write.reserved_after,
                "no write may grow reserved bytes, got {write:?}"
            );
        }
        let reserved: Vec<usize> = workload
            .iter()
            .map(|w| w.reserved_before.unwrap_or(usize::MAX))
            .collect();
        assert!(
            reserved.windows(2).all(|pair| pair[0] == pair[1]),
            "reserved bytes must stay flat across the workload, got {reserved:?}"
        );
        assert_eq!(
            trips, 0,
            "arrays::snapshot must never run on the ordinary-write path"
        );
        assert_eq!(
            routed, 0,
            "frontend routing must never emit a snapshot call"
        );
        let total_bytes: u64 = workload.iter().map(|w| w.bytes).sum();
        let total_allocs: u64 = workload.iter().map(|w| w.allocs).sum();
        let max_bytes: u64 = workload.iter().map(|w| w.bytes).max().unwrap_or(0);
        let init_bytes: u64 = init.iter().map(|w| w.bytes).sum();
        let capacity_cells = reserved[0] / std::mem::size_of::<Value>();
        assert!(capacity_cells >= backing as usize);
        eprintln!(
            "COST payload={payload} backing={backing} capacity_cells={capacity_cells} workload_bytes_total={total_bytes} workload_allocs_total={total_allocs} max_per_write={max_bytes} reserved={} init_callbacks={} init_bytes={init_bytes}",
            reserved[0],
            init.len(),
        );
        summary.push((payload, total_bytes, total_allocs, max_bytes, reserved[0]));
    }
    assert_eq!(
        variants[0], variants[1],
        "both extents must use the same call style"
    );
    eprintln!("COST summary={summary:?} variants={variants:?}");
    // The no-copy conclusion is scoped: per-write allocation work must not
    // grow with the backing extent (a temporary full-backing copy would add
    // ~backing-sized bytes and fail here honestly).
    assert!(
        summary[1].3 <= summary[0].3,
        "8192-element managed-array write cost must not exceed 32-element cost, got {summary:?}"
    );
}
