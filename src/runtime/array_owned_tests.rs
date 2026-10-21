//! Behavioral coverage for the production opt-in owned-array runtime.
//!
//! Every test here drives the real production descriptors from
//! [`super::typescript_owned_array_host_module`] (composed with the
//! unchanged manual arrays module into one effective catalog), explicit
//! [`super::initialize_typescript_owned_arrays`] setup, and certified
//! guest factories. No A4 test-only handler, authorizer, or copied Core
//! registration path is used.

use std::sync::Arc;

use vm::resource::ResourceHandle;
use vm::{CaptureRootState, HostApiCatalog, HostFunctionRegistry, Value, Vm, VmStatus};

use super::super::{NumberArray, array_key, typescript_array_host_module};
use super::{
    OwnedArrayDirectory, OwnedArrayLimits, initialize_typescript_owned_arrays,
    owned_array_runtime_stats, typescript_owned_array_host_module,
};

/// Guest factory exercising every production entry point. The dispatcher
/// registers itself (operation 0) before `makeReader` returns, so a bare
/// `makeReader()` with no user operation must already show
/// pending 0 / registered 1.
const OWNED_FACTORY_PREAMBLE: &str = r#"
import { allocate, collect, register, resize } from "__compat_array_owner";
import { close, length, write } from "arrays";
import type { Resource } from "rustscript:types";
type OwnedReader = (operation: number, arg: number) => number;
export function makeReader(): OwnedReader {
    let owner: Resource<"compat.number_array"> = allocate([10]);
    const reader: OwnedReader = (operation: number, arg: number): number => operation === 0
        ? register(owner)
        : operation === 1
        ? close(owner)
        : operation === 2
        ? write(owner, 0, 7)
        : operation === 3
        ? resize(owner, arg)
        : operation === 7
        ? collect()
        : length(owner);
    const registration = reader(0, 0);
    return reader;
}
export function sweep(): number {
    return collect();
}
"#;

/// Minimal factory for the native guest loop: only the calls the loop
/// needs, so the loop source stays small.
const OWNED_LOOP_PREAMBLE: &str = r#"
import { allocate, collect, register } from "__compat_array_owner";
import { close } from "arrays";
import type { Resource } from "rustscript:types";
type OwnedReader = (operation: number) => number;
export function makeReader(): OwnedReader {
    let owner: Resource<"compat.number_array"> = allocate([10]);
    const reader: OwnedReader = (operation: number): number => operation === 0
        ? register(owner)
        : close(owner);
    const registration = reader(0);
    return reader;
}
"#;

const OWNED_LOOP_TAIL: &str = r#"
let step = 0;
let swept: number = 0;
while (step < 2048) {
    const abandoned = makeReader();
    swept = collect();
    step = step + 1;
}
collect();
"#;

/// Factory whose dispatcher registers a *different* pending allocation
/// than its own certified owner: the candidate is a fresh allocation made
/// inside the dispatcher, so it can never equal the Move-captured owner.
const OWNED_WRONG_OWNER_PREAMBLE: &str = r#"
import { allocate, register } from "__compat_array_owner";
import { close } from "arrays";
import type { Resource } from "rustscript:types";
type OwnedReader = (operation: number) => number;
export function makeReader(): OwnedReader {
    let owner: Resource<"compat.number_array"> = allocate([10]);
    const reader: OwnedReader = (operation: number): number => {
        const other: Resource<"compat.number_array"> = allocate([11]);
        return operation === 0 ? register(other) : close(owner);
    };
    const registration = reader(0);
    return reader;
}
"#;

fn owned_factory_source(tail: &str) -> String {
    format!("{OWNED_FACTORY_PREAMBLE}\n{tail}\n")
}

fn owned_effective_catalog() -> Arc<HostApiCatalog> {
    let arrays_catalog = typescript_array_host_module()
        .catalog()
        .expect("production arrays catalog should build");
    let owned_catalog = typescript_owned_array_host_module()
        .catalog()
        .expect("production owned-array catalog should build");
    let mut builder = HostApiCatalog::builder();
    for resource in arrays_catalog.resources() {
        builder.resource(resource.clone());
    }
    for function in arrays_catalog.functions() {
        builder.function(function.clone());
    }
    for function in owned_catalog.functions() {
        builder.function(function.clone());
    }
    Arc::new(
        builder
            .build()
            .expect("combined owned-array effective catalog should build"),
    )
}

fn install_owned_production(registry: &mut HostFunctionRegistry, catalog: &HostApiCatalog) {
    typescript_array_host_module()
        .install_from_catalog(registry, catalog)
        .expect("production arrays adapters should match the effective catalog");
    typescript_owned_array_host_module()
        .install_from_catalog(registry, catalog)
        .expect("production owned adapters should match the effective catalog");
}

fn owned_production_vm(
    source: &str,
    limits: OwnedArrayLimits,
    native: bool,
) -> (Vm, Arc<HostApiCatalog>) {
    let catalog = owned_effective_catalog();
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        crate::typescript_flavor(),
        crate::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("certified owned-array compilation should succeed");
    let mut machine = compilation
        .into_vm()
        .expect("certified compilation should create a source-certified VM");
    initialize_typescript_owned_arrays(&mut machine, limits)
        .expect("explicit owned-array initialization should succeed");
    let mut registry = HostFunctionRegistry::restricted();
    install_owned_production(&mut registry, &catalog);
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

fn run_to_halt(machine: &mut Vm) {
    assert_eq!(
        machine.run().expect("owned-array execution should finish"),
        VmStatus::Halted,
        "owned-array program should halt"
    );
}

fn sweep_callable(machine: &Vm) -> Value {
    machine
        .resolve_exported_callable("sweep")
        .expect("sweep export should resolve")
}

fn invoke_sweep(machine: &mut Vm) -> Value {
    let sweep = sweep_callable(machine);
    machine
        .invoke_callable(sweep, &[])
        .expect("guest safety-point call should finish")
}

fn registered_handle(machine: &mut Vm) -> ResourceHandle {
    machine
        .host_context()
        .scope_state::<OwnedArrayDirectory>()
        .expect("owned-array directory should exist")
        .entries[0]
        .handle
}

fn payload_values(machine: &mut Vm, handle: ResourceHandle) -> Vec<Value> {
    let context = machine.host_context();
    let token = context
        .typed_resource_with_key::<NumberArray>(handle, &array_key())
        .expect("registered handle should stay typed");
    context
        .resource(&token)
        .expect("registered payload should be readable")
        .get()
        .0
        .clone()
}

#[test]
fn owned_module_composes_through_real_descriptors_and_catalog() {
    let owned = typescript_owned_array_host_module();
    let catalog = owned.catalog().expect("owned module catalog");
    let names: Vec<&str> = catalog
        .functions()
        .iter()
        .map(|function| function.name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "__compat_array_owner::allocate",
            "__compat_array_owner::register",
            "__compat_array_owner::collect",
            "__compat_array_owner::resize",
        ],
        "production owned module must expose exactly the four decided imports"
    );
    assert!(
        catalog
            .resources()
            .iter()
            .any(|resource| resource.key == array_key()),
        "owned module catalog must declare the shared number-array key"
    );
    for factory in owned.functions {
        let descriptor = factory();
        assert_eq!(
            descriptor.binding.kind,
            vm::host_extension::HostBindingKind::Owned,
            "owned descriptor {} must not route through static dispatch",
            descriptor.schema.name
        );
        assert!(
            matches!(
                descriptor.adapter,
                vm::host_extension::HostAdapterDescriptor::Owned(_)
            ),
            "owned descriptor {} must retain its owned adapter factory",
            descriptor.schema.name
        );
    }
    let mut registry = HostFunctionRegistry::restricted();
    owned
        .install(&mut registry)
        .expect("owned module should install transactionally on its own catalog");
    for name in names {
        assert!(
            registry.contains_name(name),
            "installed registry should contain {name}"
        );
    }
    let effective = owned_effective_catalog();
    assert_eq!(
        effective.functions().len(),
        11,
        "effective catalog must hold manual arrays (7) plus owned (4)"
    );
}

#[test]
fn owned_factory_return_registers_before_any_user_access_interpreted() {
    let (mut machine, _) = owned_production_vm(
        &owned_factory_source("const abandoned = makeReader(); 0;"),
        OwnedArrayLimits::default(),
        false,
    );
    run_to_halt(&mut machine);
    assert_eq!(machine.host_context().resource_count(), 1);
    let stats = owned_array_runtime_stats(&mut machine).expect("directory should exist");
    assert_eq!(stats.pending, 0, "factory must register before returning");
    assert_eq!(
        stats.registered, 1,
        "factory must register before returning"
    );
    assert!(stats.reserved_bytes > 0);
    let capture = machine
        .capture_certification_stats()
        .expect("certified program should carry capture facts");
    assert!(capture.valid);
}

#[test]
fn owned_factory_return_registers_before_any_user_access_native() {
    let (mut machine, _) = owned_production_vm(
        &owned_factory_source("const abandoned = makeReader(); 0;"),
        OwnedArrayLimits::default(),
        true,
    );
    // Native evidence is recorded independently of capture authority.
    assert!(
        machine.aot_exec_count() == 0,
        "no native execution should be recorded before run"
    );
    run_to_halt(&mut machine);
    assert!(
        machine.aot_exec_count() > 1,
        "actual native execution must exceed one dispatch, got {}",
        machine.aot_exec_count()
    );
    assert!(
        machine.dump_aot_info().contains("lowering=ssa"),
        "native lowering must stay ssa"
    );
    assert_eq!(machine.host_context().resource_count(), 1);
    let stats = owned_array_runtime_stats(&mut machine).expect("directory should exist");
    assert_eq!(
        stats.pending, 0,
        "native factory must register before returning"
    );
    assert_eq!(
        stats.registered, 1,
        "native factory must register before returning"
    );
}

#[test]
fn owned_allocation_requires_explicit_initialization() {
    let catalog = owned_effective_catalog();
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        &owned_factory_source("const abandoned = makeReader(); 0;"),
        crate::typescript_flavor(),
        crate::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("certified compilation should succeed");
    let mut machine = compilation.into_vm().expect("certified VM");
    let mut registry = HostFunctionRegistry::restricted();
    install_owned_production(&mut registry, &catalog);
    registry.bind_vm_cached(&mut machine).expect("bind");
    assert!(
        machine.run().is_err(),
        "allocation without explicit initialization must fail closed"
    );
    assert_eq!(machine.host_context().resource_count(), 0);
}

#[test]
fn owned_allocation_requires_source_certification() {
    let catalog = owned_effective_catalog();
    let compilation = vm::compile_source_with_flavor_and_options(
        &owned_factory_source("const abandoned = makeReader(); 0;"),
        crate::typescript_flavor(),
        crate::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("uncertified compilation should succeed");
    let mut machine = Vm::new(compilation.program);
    initialize_typescript_owned_arrays(&mut machine, OwnedArrayLimits::default())
        .expect("initialization itself needs no certification");
    let mut registry = HostFunctionRegistry::restricted();
    install_owned_production(&mut registry, &catalog);
    registry.bind_vm_cached(&mut machine).expect("bind");
    assert!(
        machine.capture_certification_stats().is_none(),
        "uncertified VM must carry no capture facts"
    );
    assert!(
        machine.run().is_err(),
        "uncertified allocation must fail closed"
    );
    assert_eq!(machine.host_context().resource_count(), 0);
}

#[test]
fn owned_artifact_program_without_facts_fails_closed_on_the_exact_catalog() {
    let catalog = owned_effective_catalog();
    let certified = vm::compile_source_with_flavor_and_options_with_capture_certification(
        &owned_factory_source("const abandoned = makeReader(); 0;"),
        crate::typescript_flavor(),
        crate::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("certified compilation should succeed");
    let program = certified.into_uncertified().program;
    let bytes = vm::encode_program(&program).expect("program should encode");
    let decoded = vm::decode_program(&bytes).expect("program should decode");
    let mut machine = Vm::new(decoded);
    assert!(
        machine.capture_certification_stats().is_none(),
        "decoded artifact must carry no capture facts"
    );
    initialize_typescript_owned_arrays(&mut machine, OwnedArrayLimits::default())
        .expect("initialization itself needs no certification");
    let mut registry = HostFunctionRegistry::restricted();
    install_owned_production(&mut registry, &catalog);
    registry.bind_vm_cached(&mut machine).expect("bind");
    assert!(
        machine.run().is_err(),
        "artifact allocation without facts must fail closed"
    );
    assert_eq!(machine.host_context().resource_count(), 0);
}

#[test]
fn owned_live_alias_retains_payload_across_the_safety_point() {
    let (mut machine, _) = owned_production_vm(
        &owned_factory_source("const reader = makeReader(); const alias = reader; alias;"),
        OwnedArrayLimits::default(),
        false,
    );
    run_to_halt(&mut machine);
    let held = machine.stack().last().cloned().expect("alias on stack");
    {
        let context = machine.host_context();
        let directory = context
            .scope_state::<OwnedArrayDirectory>()
            .expect("directory should exist");
        assert_eq!(
            directory.entries[0].root.observe(&context),
            CaptureRootState::Live
        );
    }
    assert_eq!(invoke_sweep(&mut machine), Value::Int(0));
    assert_eq!(machine.host_context().resource_count(), 1);
    drop(held);
    machine.reset_for_reuse().expect("reset");
    assert_eq!(machine.host_context().resource_count(), 0);
    assert!(
        owned_array_runtime_stats(&mut machine).is_none(),
        "reset must leave no stale stats"
    );
}

#[test]
fn owned_no_self_dispatcher_keeps_a_live_root_under_an_active_frame() {
    let (mut machine, _) = owned_production_vm(
        &owned_factory_source("const reader = makeReader(); reader;"),
        OwnedArrayLimits::default(),
        false,
    );
    run_to_halt(&mut machine);
    let alias = machine.stack().last().cloned().expect("reader on stack");
    let Value::Callable(callable) = &alias else {
        panic!("owning dispatcher should be a callable");
    };
    assert!(
        machine.program().callable_prototypes[callable.prototype_id as usize]
            .self_slot
            .is_none(),
        "factory dispatcher carries no self slot"
    );
    assert_eq!(
        machine
            .invoke_callable(alias.clone(), &[Value::Int(99), Value::Int(0)])
            .expect("length probe through the active dispatcher"),
        Value::Int(1)
    );
    {
        let context = machine.host_context();
        assert_eq!(
            context
                .scope_state::<OwnedArrayDirectory>()
                .expect("directory")
                .entries[0]
                .root
                .observe(&context),
            CaptureRootState::Live
        );
    }
    assert_eq!(invoke_sweep(&mut machine), Value::Int(0));
    assert_eq!(machine.host_context().resource_count(), 1);
    drop(alias);
    machine.reset_for_reuse().expect("reset");
    assert_eq!(machine.host_context().resource_count(), 0);
}

#[test]
fn owned_released_owner_is_closed_at_the_safety_point() {
    let (mut machine, _) = owned_production_vm(
        &owned_factory_source("const abandoned = makeReader(); 0;"),
        OwnedArrayLimits::default(),
        false,
    );
    run_to_halt(&mut machine);
    {
        let context = machine.host_context();
        assert_eq!(
            context
                .scope_state::<OwnedArrayDirectory>()
                .expect("directory")
                .entries[0]
                .root
                .observe(&context),
            CaptureRootState::Released
        );
    }
    assert_eq!(invoke_sweep(&mut machine), Value::Int(1));
    assert_eq!(machine.host_context().resource_count(), 0);
    let stats = owned_array_runtime_stats(&mut machine).expect("directory should exist");
    assert_eq!(stats.registered, 0);
    assert_eq!(stats.reserved_bytes, 0);
    assert_eq!(stats.last_closed, 1);
    assert_eq!(stats.total_closed, 1);
}

#[test]
fn owned_unavailable_root_retains_payload_and_accounting() {
    let (mut machine, _) = owned_production_vm(
        &owned_factory_source("const abandoned = makeReader(); 0;"),
        OwnedArrayLimits::default(),
        false,
    );
    run_to_halt(&mut machine);
    let (handle, root, bytes) = {
        let context = machine.host_context();
        let entry = &context
            .scope_state::<OwnedArrayDirectory>()
            .expect("directory")
            .entries[0];
        (entry.handle, entry.root.clone(), entry.reserved_bytes)
    };
    machine
        .set_local(0, Value::Int(0))
        .expect("epoch revocation write");
    assert_eq!(
        root.observe(&machine.host_context()),
        CaptureRootState::Unavailable
    );
    assert_eq!(invoke_sweep(&mut machine), Value::Int(0));
    assert_eq!(payload_values(&mut machine, handle), [Value::Int(10)]);
    let stats = owned_array_runtime_stats(&mut machine).expect("directory should exist");
    assert_eq!(stats.registered, 1);
    assert_eq!(stats.reserved_bytes, bytes);
    machine.reset_for_reuse().expect("reset");
    assert_eq!(machine.host_context().resource_count(), 0);
}

#[test]
fn owned_same_value_write_revokes_the_source_root_without_closing() {
    let (mut machine, _) = owned_production_vm(
        &owned_factory_source("const reader = makeReader(); reader;"),
        OwnedArrayLimits::default(),
        false,
    );
    run_to_halt(&mut machine);
    let handle = registered_handle(&mut machine);
    let raw = i64::try_from(handle.raw()).expect("handle should fit the guest integer range");
    machine
        .set_local(0, Value::Int(raw))
        .expect("same-value invalidation write");
    {
        let context = machine.host_context();
        assert_eq!(
            context
                .scope_state::<OwnedArrayDirectory>()
                .expect("directory")
                .entries[0]
                .root
                .observe(&context),
            CaptureRootState::Unavailable,
            "rewriting the owner slot with the same value must invalidate the source root"
        );
    }
    assert_eq!(invoke_sweep(&mut machine), Value::Int(0));
    assert_eq!(payload_values(&mut machine, handle), [Value::Int(10)]);
    assert_eq!(machine.host_context().resource_count(), 1);
}

#[test]
fn owned_explicit_close_retires_weak_metadata_at_the_safety_point() {
    let (mut machine, _) = owned_production_vm(
        &owned_factory_source("const reader = makeReader(); reader(1, 0); 0;"),
        OwnedArrayLimits::default(),
        false,
    );
    run_to_halt(&mut machine);
    assert_eq!(machine.host_context().resource_count(), 0);
    assert_eq!(invoke_sweep(&mut machine), Value::Int(0));
    let stats = owned_array_runtime_stats(&mut machine).expect("directory should exist");
    assert!(
        stats.registered == 0,
        "a completed typed take must retire synchronous NumberArray metadata"
    );
    assert_eq!(stats.reserved_bytes, 0);
}

#[test]
fn owned_duplicate_registration_refuses_without_closing() {
    let (mut machine, _) = owned_production_vm(
        &owned_factory_source("const reader = makeReader(); reader(0, 0);"),
        OwnedArrayLimits::default(),
        false,
    );
    assert!(
        machine.run().is_err(),
        "registering the same owner twice must fail"
    );
    assert_eq!(machine.host_context().resource_count(), 1);
    let stats = owned_array_runtime_stats(&mut machine).expect("directory should exist");
    assert_eq!(stats.registered, 1);
    assert_eq!(stats.pending, 0);
}

#[test]
fn owned_wrong_candidate_cannot_register_a_different_owner() {
    let source = format!("{OWNED_WRONG_OWNER_PREAMBLE}\nconst broken = makeReader(); 0;\n");
    let (mut machine, _) = owned_production_vm(&source, OwnedArrayLimits::default(), false);
    assert!(
        machine.run().is_err(),
        "a non-owner candidate must fail registration"
    );
    assert_eq!(machine.host_context().resource_count(), 2);
    let stats = owned_array_runtime_stats(&mut machine).expect("directory should exist");
    assert_eq!(stats.registered, 0);
    assert_eq!(stats.pending, 2);
}

#[test]
fn owned_stale_handle_never_closes_the_reused_slot() {
    let (mut machine, _) = owned_production_vm(
        &owned_factory_source("const reader = makeReader(); reader(1, 0); 0;"),
        OwnedArrayLimits::default(),
        false,
    );
    run_to_halt(&mut machine);
    let old = registered_handle(&mut machine);
    let replacement = machine
        .host_context()
        .push_resource(NumberArray(vec![Value::Int(99)]))
        .expect("slot reuse allocation")
        .handle();
    let error = machine
        .host_context()
        .typed_resource_with_key::<NumberArray>(old, &array_key())
        .expect_err("old handle must be stale");
    assert!(
        matches!(error.kind(), vm::HostContextErrorKind::Resource(code) if code.code() == vm::resource::ResourceErrorCode::ResourceStale)
    );
    assert_eq!(invoke_sweep(&mut machine), Value::Int(0));
    let stats = owned_array_runtime_stats(&mut machine).expect("directory should exist");
    assert_eq!(stats.registered, 0);
    assert_eq!(stats.reserved_bytes, 0);
    assert_eq!(payload_values(&mut machine, replacement), [Value::Int(99)]);
    assert_eq!(machine.host_context().resource_count(), 1);
}

#[test]
fn owned_foreign_handle_failure_retains_payload_and_metadata() {
    let (mut machine, _) = owned_production_vm(
        &owned_factory_source("const abandoned = makeReader(); 0;"),
        OwnedArrayLimits::default(),
        false,
    );
    run_to_halt(&mut machine);
    let (mut foreign, _) = owned_production_vm("0;", OwnedArrayLimits::default(), false);
    let foreign_token = foreign
        .host_context()
        .push_resource(NumberArray(vec![Value::Int(88)]))
        .expect("foreign allocation");
    let original = registered_handle(&mut machine);
    machine
        .host_context()
        .scope_state_mut::<OwnedArrayDirectory>()
        .expect("directory")
        .entries[0]
        .handle = foreign_token.handle();
    // The directory index still maps the original handle; refresh it so the
    // production O(1) lookup resolves the overwritten entry.
    {
        let mut context = machine.host_context();
        let directory = context
            .scope_state_mut::<OwnedArrayDirectory>()
            .expect("directory");
        directory.index.remove(&original);
        directory.index.insert(foreign_token.handle(), 0);
    }
    let sweep_outcome = {
        let sweep = sweep_callable(&machine);
        machine.invoke_callable(sweep, &[])
    };
    assert!(
        sweep_outcome.is_err() || machine.host_context().resource_count() == 1,
        "foreign lookup must not retire local metadata"
    );
    let context = machine.host_context();
    let directory = context
        .scope_state::<OwnedArrayDirectory>()
        .expect("directory");
    assert_eq!(directory.entries.len(), 1);
    assert!(directory.payload_bytes > 0);
    assert!(
        context
            .typed_resource_with_key::<NumberArray>(original, &array_key())
            .is_ok(),
        "the original local payload must stay usable"
    );
    assert_eq!(context.resource_count(), 1);
    assert_eq!(foreign.host_context().resource_count(), 1);
}

#[test]
fn owned_pending_quota_refuses_before_a_second_open_payload() {
    let source = "import { allocate } from \"__compat_array_owner\";\nconst first = allocate([1]);\nconst second = allocate([2]);\n0;\n";
    let limits = OwnedArrayLimits::new(1, 64, 1 << 20).expect("positive quotas");
    let (mut machine, _) = owned_production_vm(source, limits, false);
    assert!(
        machine.run().is_err(),
        "the second pending allocation must be refused"
    );
    assert_eq!(machine.host_context().resource_count(), 1);
    machine.reset_for_reuse().expect("reset");
    assert_eq!(machine.host_context().resource_count(), 0);
}

#[test]
fn owned_metadata_quota_refuses_a_second_factory() {
    let limits = OwnedArrayLimits::new(8, 1, 1 << 20).expect("positive quotas");
    let (mut machine, _) = owned_production_vm(
        &owned_factory_source("const first = makeReader(); const second = makeReader(); 0;"),
        limits,
        false,
    );
    assert!(
        machine.run().is_err(),
        "the second factory must be refused by the registered quota"
    );
    let stats = owned_array_runtime_stats(&mut machine).expect("directory should exist");
    assert_eq!(stats.registered, 1);
    assert_eq!(stats.pending, 1);
    machine.reset_for_reuse().expect("reset");
    assert_eq!(machine.host_context().resource_count(), 0);
}

#[test]
fn owned_payload_quota_refuses_before_resource_insertion() {
    let source = "import { allocate } from \"__compat_array_owner\";\nconst big = allocate([1, 2, 3, 4]);\n0;\n";
    let limits = OwnedArrayLimits::new(8, 64, 1).expect("positive quotas");
    let (mut machine, _) = owned_production_vm(source, limits, false);
    assert!(
        machine.run().is_err(),
        "reserved payload capacity must obey the byte quota"
    );
    assert_eq!(machine.host_context().resource_count(), 0);
    let stats = owned_array_runtime_stats(&mut machine).expect("directory should exist");
    assert_eq!(stats.pending, 0);
    assert_eq!(stats.reserved_bytes, 0);
}

#[test]
fn owned_resize_grows_then_shrinks_in_place_with_stable_accounting() {
    let (mut machine, _) = owned_production_vm(
        &owned_factory_source("const reader = makeReader(); const alias = reader; alias;"),
        OwnedArrayLimits::default(),
        false,
    );
    run_to_halt(&mut machine);
    let alias = machine.stack().last().cloned().expect("alias on stack");
    let handle = registered_handle(&mut machine);
    let before_bytes = owned_array_runtime_stats(&mut machine)
        .expect("stats")
        .reserved_bytes;
    assert_eq!(
        machine
            .invoke_callable(alias.clone(), &[Value::Int(3), Value::Int(8192)])
            .expect("growth to 8192"),
        Value::Int(8192)
    );
    let grown = payload_values(&mut machine, handle);
    assert_eq!(grown.len(), 8192);
    assert_eq!(grown[0], Value::Int(10));
    assert!(grown[1..].iter().all(|value| *value == Value::Int(0)));
    let (grown_pointer, grown_capacity, grown_bytes) = {
        let context = machine.host_context();
        let token = context
            .typed_resource_with_key::<NumberArray>(handle, &array_key())
            .expect("typed handle");
        let payload = context.resource(&token).expect("payload");
        (
            payload.get().0.as_ptr(),
            payload.get().0.capacity(),
            context
                .scope_state::<OwnedArrayDirectory>()
                .expect("directory")
                .payload_bytes,
        )
    };
    assert!(grown_capacity >= 8192);
    assert!(grown_bytes >= before_bytes);
    assert_eq!(
        machine
            .invoke_callable(alias.clone(), &[Value::Int(3), Value::Int(32)])
            .expect("shrink to 32 stays in capacity"),
        Value::Int(32)
    );
    {
        let context = machine.host_context();
        let token = context
            .typed_resource_with_key::<NumberArray>(handle, &array_key())
            .expect("typed handle");
        let payload = context.resource(&token).expect("payload");
        assert_eq!(
            payload.get().0.as_ptr(),
            grown_pointer,
            "in-capacity resize must keep the same buffer"
        );
        assert_eq!(payload.get().0.capacity(), grown_capacity);
        assert_eq!(payload.get().0.len(), 32);
        assert_eq!(
            context
                .scope_state::<OwnedArrayDirectory>()
                .expect("directory")
                .payload_bytes,
            grown_bytes,
            "in-capacity resize must not alter reserved accounting"
        );
    }
    drop(alias);
    machine.reset_for_reuse().expect("reset");
    assert_eq!(machine.host_context().resource_count(), 0);
}

#[test]
fn owned_resize_refusal_preserves_buffer_and_accounting() {
    let (mut machine, _) = owned_production_vm(
        &owned_factory_source("const reader = makeReader(); const alias = reader; alias;"),
        OwnedArrayLimits::default(),
        false,
    );
    run_to_halt(&mut machine);
    let alias = machine.stack().last().cloned().expect("alias on stack");
    let handle = registered_handle(&mut machine);
    let (pointer, capacity, bytes) = {
        let context = machine.host_context();
        let token = context
            .typed_resource_with_key::<NumberArray>(handle, &array_key())
            .expect("typed handle");
        let payload = context.resource(&token).expect("payload");
        (
            payload.get().0.as_ptr(),
            payload.get().0.capacity(),
            context
                .scope_state::<OwnedArrayDirectory>()
                .expect("directory")
                .payload_bytes,
        )
    };
    assert!(
        machine
            .invoke_callable(alias.clone(), &[Value::Int(3), Value::Int(4_000_000)])
            .is_err(),
        "over-quota growth must be refused"
    );
    {
        let context = machine.host_context();
        let token = context
            .typed_resource_with_key::<NumberArray>(handle, &array_key())
            .expect("typed handle");
        let payload = context.resource(&token).expect("payload");
        assert_eq!(payload.get().0.as_ptr(), pointer);
        assert_eq!(payload.get().0.capacity(), capacity);
        assert_eq!(payload.get().0, [Value::Int(10)]);
        assert_eq!(
            context
                .scope_state::<OwnedArrayDirectory>()
                .expect("directory")
                .payload_bytes,
            bytes
        );
    }
    assert_eq!(
        machine
            .invoke_callable(alias.clone(), &[Value::Int(3), Value::Int(32)])
            .expect("in-budget growth still works after refusal"),
        Value::Int(32)
    );
    drop(alias);
    machine.reset_for_reuse().expect("reset");
    assert_eq!(machine.host_context().resource_count(), 0);
}

#[test]
fn owned_resize_rejects_invalid_lengths_without_mutation() {
    let (mut machine, _) = owned_production_vm(
        &owned_factory_source("const reader = makeReader(); const alias = reader; alias;"),
        OwnedArrayLimits::default(),
        false,
    );
    run_to_halt(&mut machine);
    let alias = machine.stack().last().cloned().expect("alias on stack");
    let handle = registered_handle(&mut machine);
    for bad in [
        Value::Int(-1),
        Value::Float(0.5),
        Value::Float(f64::NAN),
        Value::Float(f64::INFINITY),
        Value::Float(f64::NEG_INFINITY),
        Value::Bool(true),
        Value::Null,
    ] {
        assert!(
            machine
                .invoke_callable(alias.clone(), &[Value::Int(3), bad])
                .is_err(),
            "invalid resize lengths must be rejected without mutation"
        );
    }
    assert_eq!(payload_values(&mut machine, handle), [Value::Int(10)]);
    drop(alias);
    machine.reset_for_reuse().expect("reset");
    assert_eq!(machine.host_context().resource_count(), 0);
}

#[test]
fn owned_manual_arrays_are_never_opportunistically_collected() {
    let source = "import { create } from \"arrays\";\nimport { collect } from \"__compat_array_owner\";\nconst manual = create([10]);\ncollect();\n";
    let (mut machine, _) = owned_production_vm(source, OwnedArrayLimits::default(), false);
    run_to_halt(&mut machine);
    assert_eq!(
        machine.stack().last().cloned().expect("collect result"),
        Value::Int(0)
    );
    assert_eq!(machine.host_context().resource_count(), 1);
    machine.reset_for_reuse().expect("reset");
    assert_eq!(machine.host_context().resource_count(), 0);
}

#[test]
fn owned_cross_vm_state_is_fully_isolated() {
    let (mut machine, _) = owned_production_vm(
        &owned_factory_source("const abandoned = makeReader(); 0;"),
        OwnedArrayLimits::default(),
        false,
    );
    run_to_halt(&mut machine);
    let (mut other, _) = owned_production_vm("0;", OwnedArrayLimits::default(), false);
    assert_eq!(other.host_context().resource_count(), 0);
    assert!(
        owned_array_runtime_stats(&mut other).is_none(),
        "an idle scope must report absent stats"
    );
    assert_eq!(machine.host_context().resource_count(), 1);
    machine.reset_for_reuse().expect("reset");
    assert_eq!(machine.host_context().resource_count(), 0);
}

#[test]
fn owned_aborted_factory_leaves_no_stale_state_after_reset() {
    let source = format!("{OWNED_WRONG_OWNER_PREAMBLE}\nconst broken = makeReader(); 0;\n");
    let (mut machine, _) = owned_production_vm(&source, OwnedArrayLimits::default(), false);
    assert!(machine.run().is_err(), "wrong-owner factory must abort");
    assert_eq!(machine.host_context().resource_count(), 2);
    machine.reset_for_reuse().expect("reset");
    assert_eq!(machine.host_context().resource_count(), 0);
    assert!(
        owned_array_runtime_stats(&mut machine).is_none(),
        "reset must destroy the aborted scope directory"
    );
}

#[test]
fn owned_terminal_reset_destroys_the_directory_and_invalidates_roots() {
    let (mut machine, _) = owned_production_vm(
        &owned_factory_source("const reader = makeReader(); reader;"),
        OwnedArrayLimits::default(),
        false,
    );
    run_to_halt(&mut machine);
    let root = machine
        .host_context()
        .scope_state::<OwnedArrayDirectory>()
        .expect("directory")
        .entries[0]
        .root
        .clone();
    assert_eq!(
        root.observe(&machine.host_context()),
        CaptureRootState::Live
    );
    machine.reset_for_reuse().expect("reset");
    assert_eq!(machine.host_context().resource_count(), 0);
    assert!(
        owned_array_runtime_stats(&mut machine).is_none(),
        "stats must be absent after terminal reset, never stale"
    );
    assert_eq!(
        root.observe(&machine.host_context()),
        CaptureRootState::Unavailable
    );
}

#[test]
fn owned_four_entry_scan_budget_progresses_after_swap_removal() {
    let tail = (0..9)
        .map(|index| format!("const owned{index} = makeReader();"))
        .collect::<String>()
        + "0;";
    let (mut machine, _) = owned_production_vm(
        &owned_factory_source(&tail),
        OwnedArrayLimits::default(),
        false,
    );
    run_to_halt(&mut machine);
    for (remaining, expected_closed) in [(5, 4), (1, 4), (0, 1)] {
        assert_eq!(
            invoke_sweep(&mut machine),
            Value::Int(expected_closed),
            "each safety point closes what its fixed scan budget reclaims"
        );
        let context = machine.host_context();
        let directory = context
            .scope_state::<OwnedArrayDirectory>()
            .expect("directory");
        assert!(directory.last_scanned <= 4);
        assert_eq!(directory.entries.len(), remaining);
        assert_eq!(context.resource_count(), remaining);
    }
    let stats = owned_array_runtime_stats(&mut machine).expect("directory should exist");
    assert_eq!(stats.total_scanned, 9);
    assert_eq!(stats.total_closed, 9);
    assert_eq!(stats.registered, 0);
    assert_eq!(stats.reserved_bytes, 0);
}

#[test]
fn owned_limits_default_and_reject_zero_bounds() {
    let defaults = OwnedArrayLimits::default();
    assert_eq!(defaults.pending_quota(), 8);
    assert_eq!(defaults.registered_quota(), 64);
    assert_eq!(defaults.reserved_bytes_quota(), 1 << 20);
    assert!(OwnedArrayLimits::new(0, 64, 1 << 20).is_none());
    assert!(OwnedArrayLimits::new(8, 0, 1 << 20).is_none());
    assert!(OwnedArrayLimits::new(8, 64, 0).is_none());
    let explicit = OwnedArrayLimits::new(2, 3, 4096).expect("positive quotas");
    assert_eq!(explicit.pending_quota(), 2);
    assert_eq!(explicit.registered_quota(), 3);
    assert_eq!(explicit.reserved_bytes_quota(), 4096);
}

#[test]
fn owned_initialization_is_idempotent_and_guards_tracked_changes() {
    let (mut machine, _) = owned_production_vm("0;", OwnedArrayLimits::default(), false);
    initialize_typescript_owned_arrays(&mut machine, OwnedArrayLimits::default())
        .expect("identical setup must be idempotent");
    let tight = OwnedArrayLimits::new(1, 1, 1 << 20).expect("positive quotas");
    initialize_typescript_owned_arrays(&mut machine, tight)
        .expect("a clean scope may adopt new limits");
    let (mut tracked, _) = owned_production_vm(
        &owned_factory_source("const abandoned = makeReader(); 0;"),
        OwnedArrayLimits::default(),
        false,
    );
    run_to_halt(&mut tracked);
    assert!(
        initialize_typescript_owned_arrays(&mut tracked, tight).is_err(),
        "unsafe policy changes with tracked allocations must be rejected"
    );
    let stats = owned_array_runtime_stats(&mut tracked).expect("directory should exist");
    assert_eq!(
        stats.registered, 1,
        "rejected change must keep prior accounting"
    );
}

#[test]
fn owned_initialization_enables_identity_policy_without_overriding_callers() {
    let (machine, _) = owned_production_vm("0;", OwnedArrayLimits::default(), false);
    let policy = machine
        .callable_identity_maintenance_stats()
        .expect("initialization should enable the existing budget-4 policy");
    assert_eq!(policy.probes_per_registration, 4);
    let (mut custom, _) = owned_production_vm("0;", OwnedArrayLimits::default(), false);
    custom.set_callable_identity_maintenance(vm::CallableIdentityMaintenanceConfig::new(3));
    initialize_typescript_owned_arrays(&mut custom, OwnedArrayLimits::default())
        .expect("idempotent setup");
    assert_eq!(
        custom
            .callable_identity_maintenance_stats()
            .expect("policy should stay enabled")
            .probes_per_registration,
        3,
        "a caller-enabled policy must be preserved"
    );
}

#[test]
fn owned_one_vm_2048_factories_keep_metadata_payload_and_probe_work_bounded() {
    let (mut machine, _) = owned_production_vm(
        &owned_factory_source("makeReader;"),
        OwnedArrayLimits::default(),
        false,
    );
    run_to_halt(&mut machine);
    let identity = machine
        .callable_identity_maintenance_stats()
        .expect("independent callable identity measurement must be enabled");
    assert_eq!(identity.probes_per_registration, 4);
    let factory = machine.stack().last().cloned().expect("factory on stack");
    let sweep = sweep_callable(&machine);
    let mut max_capture_entries = 0;
    let mut max_capture_capacity = 0;
    for _ in 0..2048 {
        let reader = machine
            .invoke_callable(factory.clone(), &[])
            .expect("factory call");
        assert_eq!(machine.host_context().resource_count(), 1);
        drop(reader);
        assert_eq!(
            machine
                .invoke_callable(sweep.clone(), &[])
                .expect("safety-point call"),
            Value::Int(1)
        );
        {
            let context = machine.host_context();
            let directory = context
                .scope_state::<OwnedArrayDirectory>()
                .expect("directory");
            assert!(directory.entries.is_empty() && directory.pending.is_empty());
            assert_eq!(directory.payload_bytes, 0);
            assert!(directory.last_scanned <= 4);
            assert!(directory.entries.capacity() <= 64);
            assert_eq!(context.resource_count(), 0);
        }
        let capture = machine
            .capture_certification_stats()
            .expect("capture facts stay valid");
        max_capture_entries = max_capture_entries.max(capture.entries);
        max_capture_capacity = max_capture_capacity.max(capture.capacity);
        assert!(capture.probes <= capture.registrations * 4);
        assert!(capture.entries <= 12 && capture.capacity <= 16);
        let identity = machine
            .callable_identity_maintenance_stats()
            .expect("independent callable identity measurement must be enabled");
        assert_eq!(identity.probes_per_registration, 4);
        assert!(identity.entries <= 32 && identity.capacity <= 64);
        assert!(identity.probes <= identity.registrations * 4);
    }
    let identity = machine
        .callable_identity_maintenance_stats()
        .expect("identity stats");
    assert!(identity.registrations >= 2048 && identity.retired > 0);
    let context = machine.host_context();
    let directory = context
        .scope_state::<OwnedArrayDirectory>()
        .expect("directory");
    assert_eq!(directory.total_scanned, 2048);
    eprintln!(
        "B1 interpreted history: resource=0 directory=0/0 payload=0 capture_entries={max_capture_entries} capture_capacity={max_capture_capacity} identity={{entries={} capacity={} registrations={} probes={} retired={}}} inspected={}",
        identity.entries,
        identity.capacity,
        identity.registrations,
        identity.probes,
        identity.retired,
        directory.total_scanned,
    );
}

#[test]
fn owned_native_guest_2048_factory_cycles_close_everything_with_bounded_scans() {
    let source = format!("{OWNED_LOOP_PREAMBLE}\n{OWNED_LOOP_TAIL}\n");
    let (mut machine, _) = owned_production_vm(&source, OwnedArrayLimits::default(), true);
    // Native evidence first, independently of capture authority.
    run_to_halt(&mut machine);
    assert!(
        machine.aot_exec_count() > 1,
        "actual native execution must exceed one dispatch, got {}",
        machine.aot_exec_count()
    );
    assert!(
        machine.dump_aot_info().contains("lowering=ssa"),
        "native lowering must stay ssa"
    );
    let stats = owned_array_runtime_stats(&mut machine).expect("directory should exist");
    assert_eq!(stats.pending, 0);
    assert_eq!(stats.registered, 0);
    assert_eq!(stats.reserved_bytes, 0);
    assert_eq!(machine.host_context().resource_count(), 0);
    assert!(
        stats.total_scanned >= 2048 && stats.total_scanned <= 2048 * 4 + 4,
        "scan work must stay proportional to cycles, got {}",
        stats.total_scanned
    );
    let capture = machine
        .capture_certification_stats()
        .expect("capture facts stay valid");
    assert!(capture.entries <= 12 && capture.capacity <= 16);
    assert!(capture.probes <= capture.registrations * 4);
    let identity = machine
        .callable_identity_maintenance_stats()
        .expect("independent callable identity measurement must be enabled");
    assert_eq!(identity.probes_per_registration, 4);
    assert!(identity.entries <= 32 && identity.capacity <= 64);
    assert!(identity.registrations >= 2048 && identity.retired > 0);
    assert!(identity.probes <= identity.registrations * 4);
    eprintln!(
        "B1 native guest history: native_count={} inspected={} capture={{entries={} capacity={} registrations={} probes={}}} identity={{entries={} capacity={} registrations={} probes={} retired={}}}",
        machine.aot_exec_count(),
        stats.total_scanned,
        capture.entries,
        capture.capacity,
        capture.registrations,
        capture.probes,
        identity.entries,
        identity.capacity,
        identity.registrations,
        identity.probes,
        identity.retired,
    );
}

#[test]
fn owned_typed_borrowmut_writes_share_one_buffer_at_two_backing_sizes() {
    for size in [32, 8192] {
        let (mut machine, _) = owned_production_vm(
            &owned_factory_source("const reader = makeReader(); const alias = reader; alias;"),
            OwnedArrayLimits::default(),
            false,
        );
        run_to_halt(&mut machine);
        let alias = machine.stack().last().cloned().expect("alias on stack");
        assert_eq!(
            machine
                .invoke_callable(alias.clone(), &[Value::Int(3), Value::Int(size)])
                .expect("guest resize through the owning dispatcher"),
            Value::Int(size),
            "resize to {size} must report the resulting length"
        );
        let handle = registered_handle(&mut machine);
        let (pointer, capacity, accounted) = {
            let context = machine.host_context();
            let token = context
                .typed_resource_with_key::<NumberArray>(handle, &array_key())
                .expect("typed handle");
            let payload = context.resource(&token).expect("payload");
            (
                payload.get().0.as_ptr(),
                payload.get().0.capacity(),
                context
                    .scope_state::<OwnedArrayDirectory>()
                    .expect("directory")
                    .payload_bytes,
            )
        };
        assert_eq!(payload_values(&mut machine, handle).len() as i64, size);
        for _ in 0..1024 {
            assert_eq!(
                machine
                    .invoke_callable(alias.clone(), &[Value::Int(2), Value::Int(0)])
                    .expect("alias write through the owning dispatcher"),
                Value::Int(7)
            );
            assert_eq!(invoke_sweep(&mut machine), Value::Int(0));
            let context = machine.host_context();
            let token = context
                .typed_resource_with_key::<NumberArray>(handle, &array_key())
                .expect("typed handle");
            let payload = context.resource(&token).expect("payload");
            assert_eq!(payload.get().0.as_ptr(), pointer);
            assert_eq!(payload.get().0.capacity(), capacity);
            assert_eq!(payload.get().0.len() as i64, size);
            assert_eq!(payload.get().0[0], Value::Int(7));
            assert!(
                payload.get().0[1..]
                    .iter()
                    .all(|value| *value == Value::Int(0))
            );
            assert_eq!(
                context
                    .scope_state::<OwnedArrayDirectory>()
                    .expect("directory")
                    .payload_bytes,
                accounted
            );
        }
        eprintln!(
            "B1 typed BorrowMut writes: backing={size} writes=1024 unchanged_buffer=true capacity={capacity} reserved_bytes={accounted}"
        );
        drop(alias);
        machine.reset_for_reuse().expect("reset");
        assert_eq!(machine.host_context().resource_count(), 0);
        assert!(
            owned_array_runtime_stats(&mut machine).is_none(),
            "reset must leave no stale stats"
        );
    }
}
