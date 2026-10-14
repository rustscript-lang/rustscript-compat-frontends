use std::sync::{Arc, Mutex, Weak};

use vm::resource::{HostResource, ResourceHandle};
use vm::{
    CallOutcome, CallReturn, CallableEnvironment, CaptureBindingMode, CaptureQueryError,
    CaptureRootState, HostApiCatalog, HostFunctionRegistry, HostFunctionSchema, HostOwnedFunction,
    HostParamPassing, HostParamSchema, HostTypeSchema, OwnedHostCall, ResourceCaptureRoot,
    ResourceTypeKey, TypeSchema, Value, Vm, VmError, VmResult, VmStatus,
};

use super::{NumberArray, array_key, typescript_array_host_module};

const LEGACY_LIFECYCLE_SOURCE: &str = include_str!("array_lifecycle_tests.rs");

fn legacy_raw_source(name: &str) -> &'static str {
    let marker = format!("const {name}: &str = r#\"");
    let start = LEGACY_LIFECYCLE_SOURCE.find(&marker).unwrap_or_else(|| {
        panic!("legacy source fixture {name} must remain in its original test file")
    });
    let body = &LEGACY_LIFECYCLE_SOURCE[start + marker.len()..];
    let end = body.find("\n\"#;").unwrap_or_else(|| {
        panic!("legacy source fixture {name} must retain its raw-string boundary")
    });
    &body[..end + 1]
}

fn owner_check_schema() -> HostFunctionSchema {
    HostFunctionSchema::with_return(
        "proof::check",
        vec![HostParamSchema::with_passing(
            "candidate",
            HostTypeSchema::Resource(array_key()),
            HostParamPassing::Borrow,
        )],
        HostTypeSchema::Number,
    )
}

fn owner_catalog() -> Arc<HostApiCatalog> {
    let production_catalog = typescript_array_host_module()
        .catalog()
        .expect("production TypeScript array catalog should build");
    let mut builder = HostApiCatalog::builder();
    for resource in production_catalog.resources() {
        builder.resource(resource.clone());
    }
    for function in production_catalog.functions() {
        builder.function(function.clone());
    }
    builder.function(owner_check_schema());
    Arc::new(
        builder
            .build()
            .expect("complete owner-proof catalog should build"),
    )
}

fn array_registry(catalog: &HostApiCatalog) -> HostFunctionRegistry {
    let mut registry = HostFunctionRegistry::restricted();
    typescript_array_host_module()
        .install_from_catalog(&mut registry, catalog)
        .expect("production array handlers should match their exact catalog");
    registry
}

fn owner_registry(catalog: &HostApiCatalog) -> HostFunctionRegistry {
    let mut registry = array_registry(catalog);
    let schema = vm::catalog_import_schemas(catalog, "proof::check")
        .into_iter()
        .next()
        .expect("owner-check catalog schema");
    registry
        .register_exact_owned("proof::check", 1, schema, |_context| {
            Box::new(CertifiedOwnerProbe)
        })
        .expect("owner probe must bind only against the complete exact schema");
    registry.authorize_registered_builtin_import("proof::check");
    registry
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProbePlan {
    MatchOwner,
    MismatchCandidate,
    NoMatchingCapture,
    AmbiguousCapture,
    Unavailable,
    WrongKey,
    WrongConcreteType,
    DropLastAliasInActiveFrame,
    PublicSameValueWrite,
}

#[derive(Default)]
struct ProbeState {
    plan: Option<ProbePlan>,
    calls: usize,
    candidate_borrows: usize,
    owner_borrows: usize,
    candidate_handles: Vec<ResourceHandle>,
    owner_handles: Vec<ResourceHandle>,
    candidate_values: Vec<Value>,
    owner_values: Vec<Value>,
    comparisons: Vec<bool>,
    query_errors: Vec<CaptureQueryError>,
    roots: Vec<ResourceCaptureRoot>,
    root_during_call: Vec<CaptureRootState>,
    root_after_alias_drop: Vec<CaptureRootState>,
    root_after_public_write: Vec<CaptureRootState>,
    replacement_handle: Option<ResourceHandle>,
    environment: Option<Weak<CallableEnvironment>>,
    environment_expired_during_call: Vec<bool>,
    held_alias: Option<Value>,
}

impl ProbeState {
    fn with_plan(plan: ProbePlan) -> Self {
        Self {
            plan: Some(plan),
            ..Self::default()
        }
    }
}

struct OtherConcreteArray;

impl HostResource for OtherConcreteArray {
    fn resource_type_key() -> Option<ResourceTypeKey> {
        Some(ResourceTypeKey::new("compat.other_number_array").expect("valid test resource key"))
    }
}

struct OwnerObservation {
    handle: ResourceHandle,
    payload: Value,
    root: ResourceCaptureRoot,
    state_during_call: CaptureRootState,
}

enum OwnerQuery {
    Captured(OwnerObservation),
    Rejected(CaptureQueryError),
    WrongConcreteTypeAccepted,
}

fn first_number_array_value(
    context: &vm::HostContext<'_>,
    token: &vm::resource::Resource<NumberArray>,
) -> VmResult<Value> {
    let resource = context
        .resource(token)
        .map_err(|error| VmError::HostError(error.to_string()))?;
    Ok(resource.get().0.first().cloned().unwrap_or(Value::Null))
}

fn read_candidate(call: &mut OwnedHostCall<'_>) -> VmResult<(ResourceHandle, Value)> {
    let raw = match call.arg(0) {
        Some(Value::Int(raw)) => u64::try_from(*raw)
            .map_err(|_| VmError::TypeMismatch("proof::check candidate resource"))?,
        _ => return Err(VmError::TypeMismatch("proof::check candidate resource")),
    };
    let handle =
        ResourceHandle::from_raw(raw).map_err(|error| VmError::HostError(error.to_string()))?;
    let mut context = call.vm().host_context();
    // The explicit candidate argument is independently checked; it is never
    // used as evidence for the callable's source owner.
    let token = context
        .typed_resource_with_key::<NumberArray>(handle, &array_key())
        .map_err(|error| VmError::HostError(error.to_string()))?;
    let value = first_number_array_value(&context, &token)?;
    let typed_handle = token.handle();
    let state = context
        .module_state_mut::<ProbeState>()
        .expect("each VM has its own installed proof state");
    state.candidate_borrows += 1;
    state.candidate_handles.push(typed_handle);
    state.candidate_values.push(value.clone());
    Ok((typed_handle, value))
}

fn query_owner(call: &mut OwnedHostCall<'_>, plan: ProbePlan) -> VmResult<OwnerQuery> {
    let context = call.vm().host_context();
    if plan == ProbePlan::WrongConcreteType {
        return Ok(
            match context.checked_move_resource_capture::<OtherConcreteArray>(&array_key()) {
                Ok(_) => OwnerQuery::WrongConcreteTypeAccepted,
                Err(error) => OwnerQuery::Rejected(error),
            },
        );
    }
    let wrong_key =
        ResourceTypeKey::new("compat.unrelated_number_array").expect("valid test resource key");
    let expected_key = if plan == ProbePlan::WrongKey {
        &wrong_key
    } else {
        &array_key()
    };
    Ok(
        match context.checked_move_resource_capture::<NumberArray>(expected_key) {
            Ok(capture) => {
                let handle = capture.token().handle();
                let payload = first_number_array_value(&context, capture.token())?;
                let root = capture.root().clone();
                let state_during_call = root.observe(&context);
                OwnerQuery::Captured(OwnerObservation {
                    handle,
                    payload,
                    root,
                    state_during_call,
                })
            }
            Err(error) => OwnerQuery::Rejected(error),
        },
    )
}

fn expected_query_error(plan: ProbePlan, error: &CaptureQueryError) -> bool {
    match plan {
        ProbePlan::NoMatchingCapture | ProbePlan::WrongKey => {
            matches!(error, CaptureQueryError::NoMatchingCapture)
        }
        ProbePlan::AmbiguousCapture => matches!(error, CaptureQueryError::AmbiguousCapture),
        ProbePlan::Unavailable => matches!(error, CaptureQueryError::Unavailable),
        ProbePlan::WrongConcreteType => matches!(error, CaptureQueryError::Resource(_)),
        ProbePlan::MatchOwner
        | ProbePlan::MismatchCandidate
        | ProbePlan::DropLastAliasInActiveFrame
        | ProbePlan::PublicSameValueWrite => false,
    }
}

fn record_query_error(call: &mut OwnedHostCall<'_>, error: &CaptureQueryError) {
    let mut context = call.vm().host_context();
    let state = context
        .module_state_mut::<ProbeState>()
        .expect("per-VM proof state");
    state.calls += 1;
    state.query_errors.push(error.clone());
}

fn record_owner_observation(
    call: &mut OwnedHostCall<'_>,
    candidate_handle: ResourceHandle,
    owner: &OwnerObservation,
) -> bool {
    let mut context = call.vm().host_context();
    let state = context
        .module_state_mut::<ProbeState>()
        .expect("per-VM proof state");
    state.calls += 1;
    state.owner_borrows += 1;
    state.owner_handles.push(owner.handle);
    state.owner_values.push(owner.payload.clone());
    state.comparisons.push(candidate_handle == owner.handle);
    state.roots.push(owner.root.clone());
    state.root_during_call.push(owner.state_during_call);
    candidate_handle == owner.handle
}

fn active_single_move_local(call: &OwnedHostCall<'_>) -> VmResult<u8> {
    let vm = call.vm_ref();
    let frame = vm.execution_frames().last().cloned().ok_or_else(|| {
        VmError::HostError("owner proof requires an active script frame".to_owned())
    })?;
    let prototype_id = frame
        .prototype_id
        .ok_or_else(|| VmError::HostError("owner proof requires a script frame".to_owned()))?;
    let prototype = vm
        .program()
        .callable_prototypes
        .get(prototype_id as usize)
        .ok_or(VmError::InvalidCallablePrototype(prototype_id))?;
    let move_slots = prototype
        .capture_modes
        .iter()
        .enumerate()
        .filter_map(|(ordinal, mode)| {
            (*mode == CaptureBindingMode::Move)
                .then(|| prototype.capture_slots.get(ordinal).copied())
                .flatten()
        })
        .collect::<Vec<_>>();
    let [slot] = move_slots.as_slice() else {
        return Err(VmError::HostError(
            "same-value invalidation control requires one Move capture".to_owned(),
        ));
    };
    u8::try_from(*slot)
        .map_err(|_| VmError::HostError("Move capture local exceeds public index range".to_owned()))
}

struct CertifiedOwnerProbe;

impl HostOwnedFunction for CertifiedOwnerProbe {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> VmResult<CallOutcome> {
        let plan = call
            .vm()
            .host_context()
            .module_state::<ProbeState>()
            .and_then(|state| state.plan)
            .ok_or_else(|| VmError::HostError("proof plan is not installed".to_owned()))?;
        let (candidate_handle, _candidate_value) = read_candidate(call)?;
        match query_owner(call, plan)? {
            OwnerQuery::Rejected(error) => {
                record_query_error(call, &error);
                if expected_query_error(plan, &error) {
                    Ok(CallOutcome::Return(CallReturn::one(Value::Int(1))))
                } else {
                    Err(VmError::HostError(format!(
                        "proof::check unexpected certified-owner query result: {error}"
                    )))
                }
            }
            OwnerQuery::WrongConcreteTypeAccepted => Err(VmError::HostError(
                "proof::check accepted a concrete resource type with a different key".to_owned(),
            )),
            OwnerQuery::Captured(owner) => {
                let equal = record_owner_observation(call, candidate_handle, &owner);
                if owner.state_during_call != CaptureRootState::Live {
                    return Err(VmError::HostError(format!(
                        "proof::check returned a non-live active-frame root: {:?}",
                        owner.state_during_call
                    )));
                }
                if plan == ProbePlan::MismatchCandidate {
                    return if equal {
                        Err(VmError::HostError(
                            "proof::check wrong-candidate control unexpectedly matched".to_owned(),
                        ))
                    } else {
                        Err(VmError::HostError(
                            "proof::check candidate is not the certified Move owner".to_owned(),
                        ))
                    };
                }
                if !equal {
                    return Err(VmError::HostError(
                        "proof::check source candidate differs from the certified Move owner"
                            .to_owned(),
                    ));
                }
                if plan == ProbePlan::DropLastAliasInActiveFrame {
                    let (environment, held_alias) = {
                        let mut context = call.vm().host_context();
                        let state = context
                            .module_state_mut::<ProbeState>()
                            .expect("per-VM proof state");
                        (state.environment.clone(), state.held_alias.take())
                    };
                    let alias_was_present = held_alias.is_some();
                    drop(held_alias);
                    let environment_expired = environment
                        .as_ref()
                        .is_some_and(|weak| weak.upgrade().is_none());
                    let root_state = owner.root.observe(&call.vm().host_context());
                    let mut context = call.vm().host_context();
                    let state = context
                        .module_state_mut::<ProbeState>()
                        .expect("per-VM proof state");
                    state
                        .environment_expired_during_call
                        .push(environment_expired);
                    state.root_after_alias_drop.push(root_state);
                    if !alias_was_present
                        || !environment_expired
                        || root_state != CaptureRootState::Live
                    {
                        return Err(VmError::HostError(format!(
                            "active frame failed independent root control: alias={alias_was_present}, environment_expired={environment_expired}, root={root_state:?}"
                        )));
                    }
                }
                if plan == ProbePlan::PublicSameValueWrite {
                    let local = active_single_move_local(call)?;
                    let replacement = {
                        let context = call.vm().host_context();
                        let handle = context
                            .module_state::<ProbeState>()
                            .expect("per-VM proof state")
                            .replacement_handle
                            .unwrap_or(owner.handle);
                        context
                            .typed_resource_with_key::<NumberArray>(handle, &array_key())
                            .map_err(|error| VmError::HostError(error.to_string()))?;
                        handle
                    };
                    let raw = i64::try_from(replacement.raw()).map_err(|_| {
                        VmError::HostError("resource handle exceeds Int".to_owned())
                    })?;
                    call.vm().set_local(local, Value::Int(raw))?;
                    let root_state = owner.root.observe(&call.vm().host_context());
                    let mut context = call.vm().host_context();
                    let state = context
                        .module_state_mut::<ProbeState>()
                        .expect("per-VM proof state");
                    state.root_after_public_write.push(root_state);
                    if root_state != CaptureRootState::Unavailable {
                        return Err(VmError::HostError(format!(
                            "public same-value write left a source root usable: {root_state:?}"
                        )));
                    }
                }
                Ok(CallOutcome::Return(CallReturn::one(Value::Int(1))))
            }
        }
    }
}

fn bind_probe(mut machine: Vm, catalog: &HostApiCatalog, plan: ProbePlan) -> Vm {
    assert!(
        !machine
            .host_context()
            .set_module_state(ProbeState::with_plan(plan))
    );
    owner_registry(catalog)
        .bind_vm_cached(&mut machine)
        .expect("exact array and proof handlers should bind to this VM");
    machine
}

fn certified_vm(source: &str, plan: ProbePlan) -> Vm {
    let catalog = owner_catalog();
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        crate::typescript_flavor(),
        crate::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .unwrap_or_else(|error| panic!("certified TypeScript compilation failed: {error}"));
    let machine = compilation
        .into_vm()
        .expect("published certified compilation should create a source-certified VM");
    bind_probe(machine, &catalog, plan)
}

fn regular_vm(source: &str, plan: ProbePlan) -> (Vm, Arc<HostApiCatalog>) {
    let catalog = owner_catalog();
    let compilation = vm::compile_source_with_flavor_and_options(
        source,
        crate::typescript_flavor(),
        crate::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .unwrap_or_else(|error| panic!("ordinary TypeScript compilation failed: {error}"));
    let machine = bind_probe(Vm::new(compilation.program), &catalog, plan);
    (machine, catalog)
}

fn run_to_halt(machine: &mut Vm, label: &str) {
    assert_eq!(
        machine
            .run()
            .unwrap_or_else(|error| panic!("{label} execution failed: {error}")),
        VmStatus::Halted,
        "{label} should halt"
    );
}

fn move_capture_aggregate(vm: &Vm) -> TypeSchema {
    let expected = TypeSchema::Callable {
        params: vec![TypeSchema::Number],
        result: Box::new(TypeSchema::Number),
    };
    let prototype = vm
        .program()
        .callable_prototypes
        .iter()
        .find(|prototype| {
            prototype.schema.as_ref() == Some(&expected)
                && prototype.self_slot.is_some()
                && prototype.capture_modes.contains(&CaptureBindingMode::Move)
        })
        .expect("legacy dispatcher prototype should have a single callable schema");
    let source = prototype
        .capture_source_slots
        .iter()
        .zip(&prototype.capture_modes)
        .find_map(|(source, mode)| (*mode == CaptureBindingMode::Move).then_some(*source))
        .expect("dispatcher should retain its physical Move slot");
    vm.program()
        .type_map
        .as_ref()
        .and_then(|type_map| type_map.local_schemas.get(usize::from(source)))
        .and_then(Option::as_ref)
        .cloned()
        .expect("aggregate TypeMap should expose the physical slot schema")
}

const ACTIVE_FRAME_ROOT_SOURCE: &str = r#"
import { create, length, close } from "arrays";
import { check } from "proof";
import type { Resource } from "rustscript:types";
type Reader = (operation: number) => number;
function allocateNested(): number {
    const identity = (value: number): number => value + 1;
    return identity(1);
}
export function makeReader(): Reader {
    let owner: Resource<"compat.number_array"> = create([41]);
    const reader: Reader = (operation: number): number => operation === 0
        ? allocateNested() + check(owner) + length(owner)
        : close(owner);
    return reader;
}
"#;

const REPEATED_FACTORY_SOURCE: &str = r#"
import { create, length, close } from "arrays";
import { check } from "proof";
import type { Resource } from "rustscript:types";
type Reader = (operation: number) => number;
export function factory(): Reader {
    let owner: Resource<"compat.number_array"> = create([37]);
    const reader: Reader = (operation: number): number => operation === 0
        ? check(owner) + length(owner)
        : close(owner);
    return reader;
}
"#;

const ORDINARY_CALL_SOURCE: &str = r#"
import { create, length, close } from "arrays";
import { check } from "proof";
import type { Resource } from "rustscript:types";
type Reader = (operation: number) => number;
function main(): number {
    let owner: Resource<"compat.number_array"> = create([41]);
    const reader: Reader = (operation: number): number => operation === 0
        ? check(owner) + length(owner)
        : close(owner);
    return reader(0);
}
main();
"#;

#[test]
fn certified_typescript_dispatcher_associates_the_real_move_owner_through_an_alias() {
    let source = legacy_raw_source("LEGITIMATE_DISPATCHER_SOURCE");
    assert!(source.contains("const alias = dispatcher;"));
    assert!(source.contains("check(owner) + write(owner, 0, 11) + length(owner)"));
    let mut machine = certified_vm(source, ProbePlan::MatchOwner);
    assert!(
        machine
            .capture_certification_stats()
            .is_some_and(|stats| stats.valid && stats.capture_facts > 0)
    );
    run_to_halt(&mut machine, "legacy self-bearing owner dispatcher");
    assert_eq!(machine.stack().last(), Some(&Value::Int(14)));
    {
        let context = machine.host_context();
        let state = context.module_state::<ProbeState>().expect("proof state");
        assert_eq!(state.calls, 1);
        assert_eq!(state.candidate_borrows, 1);
        assert_eq!(state.owner_borrows, 1);
        assert_eq!(state.candidate_handles, state.owner_handles);
        assert_eq!(state.comparisons, [true]);
        assert_eq!(state.candidate_values, [Value::Int(10)]);
        assert_eq!(state.owner_values, [Value::Int(10)]);
        assert_eq!(state.root_during_call, [CaptureRootState::Live]);
        eprintln!(
            "certified TypeScript association event: candidate={:?} owner={:?} typed_borrows={}/{} payload={:?} root={:?}",
            state.candidate_handles[0],
            state.owner_handles[0],
            state.candidate_borrows,
            state.owner_borrows,
            state.owner_values[0],
            state.root_during_call[0],
        );
    }
    assert_eq!(machine.host_context().resource_count(), 0);
    let closed_root = machine
        .host_context()
        .module_state::<ProbeState>()
        .and_then(|state| state.roots.first())
        .cloned()
        .expect("owner query stored only a weak root observer");
    assert_eq!(
        closed_root.observe(&machine.host_context()),
        CaptureRootState::Unavailable,
        "explicitly closed payload must be unavailable, not reported as a released cell"
    );
    eprintln!(
        "explicit close event: root={:?} payload_resources={}",
        closed_root.observe(&machine.host_context()),
        machine.host_context().resource_count(),
    );
}

#[test]
fn unrelated_typed_frames_change_physical_slot_schema_but_not_certified_source_owner() {
    let source = legacy_raw_source("LEGITIMATE_DISPATCHER_SOURCE");
    let base_machine = certified_vm(source, ProbePlan::MatchOwner);
    assert_eq!(
        move_capture_aggregate(&base_machine),
        TypeSchema::Resource(array_key())
    );
    let perturbation = r#"
function unrelated(value: string): string {
    const local: string = value;
    return local;
}
function independent(flag: boolean, value: string): string {
    const local: boolean = flag;
    return local ? value : "unused";
}
"#;
    let perturbed_source = format!("{source}\n{perturbation}");
    let mut machine = certified_vm(&perturbed_source, ProbePlan::MatchOwner);
    assert_eq!(move_capture_aggregate(&machine), TypeSchema::Bool);
    run_to_halt(&mut machine, "perturbed source-owned dispatcher");
    assert_eq!(machine.stack().last(), Some(&Value::Int(14)));
    let context = machine.host_context();
    let state = context.module_state::<ProbeState>().expect("proof state");
    assert_eq!(state.candidate_borrows, 1);
    assert_eq!(state.owner_borrows, 1);
    assert_eq!(state.comparisons, [true]);
    assert_eq!(state.root_during_call, [CaptureRootState::Live]);
}

#[test]
fn certified_wrong_candidate_is_typed_and_rejected_against_full_owner_handle() {
    let source = legacy_raw_source("NUMERIC_WRONG_RESOURCE_SOURCE");
    let mut machine = certified_vm(source, ProbePlan::MismatchCandidate);
    let error = machine
        .run()
        .expect_err("another live NumberArray must not become the captured owner");
    assert!(
        error
            .to_string()
            .contains("candidate is not the certified Move owner")
    );
    {
        let context = machine.host_context();
        let state = context.module_state::<ProbeState>().expect("proof state");
        assert_eq!(state.calls, 1);
        assert_eq!(state.candidate_borrows, 1);
        assert_eq!(state.owner_borrows, 1);
        assert_ne!(state.candidate_handles[0], state.owner_handles[0]);
        assert_eq!(state.comparisons, [false]);
        assert_eq!(state.candidate_values, [Value::Int(20)]);
        assert_eq!(state.owner_values, [Value::Int(10)]);
        assert_eq!(state.root_during_call, [CaptureRootState::Live]);
        eprintln!(
            "typed wrong-candidate event: candidate={:?} owner={:?} typed_borrows={}/{} equal={}",
            state.candidate_handles[0],
            state.owner_handles[0],
            state.candidate_borrows,
            state.owner_borrows,
            state.comparisons[0],
        );
    }
    assert_eq!(machine.host_context().resource_count(), 2);
    machine
        .reset_for_reuse()
        .expect("explicit reset closes both resources");
    assert_eq!(machine.host_context().resource_count(), 0);
}

#[test]
fn certified_query_distinguishes_wrong_key_wrong_type_no_match_and_ambiguity() {
    let source = legacy_raw_source("LEGITIMATE_DISPATCHER_SOURCE");
    let mut wrong_key = certified_vm(source, ProbePlan::WrongKey);
    run_to_halt(&mut wrong_key, "wrong-key query control");
    assert!(matches!(
        wrong_key
            .host_context()
            .module_state::<ProbeState>()
            .and_then(|state| state.query_errors.first()),
        Some(CaptureQueryError::NoMatchingCapture)
    ));
    assert_eq!(wrong_key.host_context().resource_count(), 0);

    let mut wrong_type = certified_vm(source, ProbePlan::WrongConcreteType);
    run_to_halt(&mut wrong_type, "wrong-concrete-type query control");
    assert!(matches!(
        wrong_type
            .host_context()
            .module_state::<ProbeState>()
            .and_then(|state| state.query_errors.first()),
        Some(CaptureQueryError::Resource(_))
    ));
    assert_eq!(wrong_type.host_context().resource_count(), 0);

    let no_matching_source = r#"
import { create } from "arrays";
import { check } from "proof";
import type { Resource } from "rustscript:types";
function main(): number {
    const candidate: Resource<"compat.number_array"> = create([31]);
    return check(candidate);
}
main();
"#;
    let mut no_matching = certified_vm(no_matching_source, ProbePlan::NoMatchingCapture);
    run_to_halt(&mut no_matching, "ordinary caller with no Move capture");
    assert!(matches!(
        no_matching
            .host_context()
            .module_state::<ProbeState>()
            .and_then(|state| state.query_errors.first()),
        Some(CaptureQueryError::NoMatchingCapture)
    ));
    let no_matching_resources_before_reset = no_matching.host_context().resource_count();
    assert_eq!(no_matching_resources_before_reset, 1);
    no_matching
        .reset_for_reuse()
        .expect("explicit reset closes ordinary caller candidate");

    let ambiguous_source = r#"
import { create, close } from "arrays";
import { check } from "proof";
import type { Resource } from "rustscript:types";
type Dispatcher = (operation: number) => number;
function makeDispatcher(): Dispatcher {
    let first: Resource<"compat.number_array"> = create([10]);
    let second: Resource<"compat.number_array"> = create([20]);
    const dispatcher: Dispatcher = (operation: number): number =>
        operation === 0
            ? check(first) + check(second)
            : close(first) + close(second);
    return dispatcher;
}
function main(): number {
    const dispatcher = makeDispatcher();
    return dispatcher(0) + dispatcher(1);
}
main();
"#;
    let mut ambiguous = certified_vm(ambiguous_source, ProbePlan::AmbiguousCapture);
    run_to_halt(&mut ambiguous, "two direct same-key Move captures");
    assert_eq!(ambiguous.stack().last(), Some(&Value::Int(4)));
    {
        let context = ambiguous.host_context();
        let state = context.module_state::<ProbeState>().expect("proof state");
        assert_eq!(state.calls, 2);
        assert_eq!(state.candidate_borrows, 2);
        assert_eq!(state.owner_borrows, 0);
        assert_eq!(state.query_errors.len(), 2);
        assert!(
            state
                .query_errors
                .iter()
                .all(|error| matches!(error, CaptureQueryError::AmbiguousCapture))
        );
    }
    assert_eq!(ambiguous.host_context().resource_count(), 0);
    let wrong_key_error = wrong_key
        .host_context()
        .module_state::<ProbeState>()
        .expect("wrong-key state")
        .query_errors[0]
        .clone();
    let wrong_type_error = wrong_type
        .host_context()
        .module_state::<ProbeState>()
        .expect("wrong-type state")
        .query_errors[0]
        .clone();
    let no_matching_error = no_matching
        .host_context()
        .module_state::<ProbeState>()
        .expect("no-match state")
        .query_errors[0]
        .clone();
    let ambiguous_errors = ambiguous
        .host_context()
        .module_state::<ProbeState>()
        .expect("ambiguous state")
        .query_errors
        .clone();
    eprintln!(
        "capture-query controls: wrong_key={wrong_key_error:?}/resources={} wrong_concrete={wrong_type_error:?}/resources={} no_move={no_matching_error:?}/resources_before_reset={no_matching_resources_before_reset}/after_reset={} ambiguous={ambiguous_errors:?}/owner_borrows=0/resources_after_close={}",
        wrong_key.host_context().resource_count(),
        wrong_type.host_context().resource_count(),
        no_matching.host_context().resource_count(),
        ambiguous.host_context().resource_count(),
    );
}

#[test]
fn source_compiler_self_copy_resource_rejection_remains_a_compile_diagnostic() {
    let catalog = owner_catalog();
    let source = legacy_raw_source("WRONG_PAIR_SOURCE");
    let error = match vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        crate::typescript_flavor(),
        crate::compile_options_with_catalog(catalog),
    ) {
        Ok(_) => panic!("resource-signature self-copy must remain rejected by the compiler"),
        Err(error) => error,
    };
    let diagnostic = format!("{error:?}");
    assert!(
        diagnostic.contains("E_OWNERSHIP_COPY_RESOURCE"),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains("closure capture of resource value"),
        "{diagnostic}"
    );
    eprintln!("compile-time resource self-copy rejection: {diagnostic}");
}

#[test]
fn same_named_guest_function_has_no_imported_owner_authority() {
    let source = r#"
import { create } from "arrays";
import type { Resource } from "rustscript:types";
function check(candidate: Resource<"compat.number_array">): number {
    return 7;
}
function main(): number {
    const candidate: Resource<"compat.number_array"> = create([31]);
    return check(candidate);
}
main();
"#;
    let (mut machine, _) = regular_vm(source, ProbePlan::MatchOwner);
    run_to_halt(&mut machine, "same-named guest function");
    assert_eq!(machine.stack().last(), Some(&Value::Int(7)));
    assert_eq!(
        machine
            .host_context()
            .module_state::<ProbeState>()
            .expect("proof state")
            .calls,
        0,
        "a guest function named check must not enter the guarded host handler"
    );
    assert_eq!(machine.host_context().resource_count(), 1);
    machine
        .reset_for_reuse()
        .expect("explicit reset closes the guest resource");
    assert_eq!(machine.host_context().resource_count(), 0);
}

#[test]
fn explicit_proof_import_without_exact_runtime_registration_is_unbound() {
    let source = r#"
import { create } from "arrays";
import { check as importedCheck } from "proof";
import type { Resource } from "rustscript:types";
function check(candidate: Resource<"compat.number_array">): number {
    return 7;
}
function main(): number {
    const candidate: Resource<"compat.number_array"> = create([31]);
    return importedCheck(candidate);
}
main();
"#;
    let catalog = owner_catalog();
    let compiled = vm::compile_source_with_flavor_and_options(
        source,
        crate::typescript_flavor(),
        crate::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("explicit host import should compile against the declared catalog schema");
    let mut machine = Vm::new(compiled.program);
    let error = array_registry(&catalog)
        .bind_vm_cached(&mut machine)
        .expect_err("catalog declaration alone must not install the guarded owner handler");
    assert!(matches!(error, VmError::UnboundImport(ref name) if name == "proof::check"));
}

#[test]
fn active_no_self_frame_keeps_the_real_cell_root_after_environment_expiry() {
    let mut machine = certified_vm(
        ACTIVE_FRAME_ROOT_SOURCE,
        ProbePlan::DropLastAliasInActiveFrame,
    );
    run_to_halt(&mut machine, "source-owned factory declaration");
    let factory = machine
        .resolve_exported_callable("makeReader")
        .expect("source-defined TypeScript reader factory");
    let closure = machine
        .invoke_callable(factory, &[])
        .expect("factory should allocate and return its reader");
    let Value::Callable(callable) = &closure else {
        panic!("factory result should be a callable");
    };
    let prototype = &machine.program().callable_prototypes[callable.prototype_id as usize];
    assert!(
        prototype.self_slot.is_none(),
        "this is a no-self active-frame control"
    );
    assert_eq!(
        prototype
            .capture_modes
            .iter()
            .filter(|mode| **mode == CaptureBindingMode::Move)
            .count(),
        1,
        "the source dispatcher must have one actual Move owner"
    );
    let environment = Arc::downgrade(
        callable
            .env
            .as_ref()
            .expect("capturing TypeScript reader has a callable environment"),
    );
    {
        let mut context = machine.host_context();
        let state = context
            .module_state_mut::<ProbeState>()
            .expect("proof state");
        state.environment = Some(environment.clone());
        state.held_alias = Some(closure.clone());
    }
    assert_eq!(
        machine
            .invoke_callable(closure, &[Value::Int(0)])
            .expect("nested allocation and active Move dispatch should execute"),
        Value::Int(4)
    );
    assert!(environment.upgrade().is_none());
    let (root, handle) = {
        let context = machine.host_context();
        let state = context.module_state::<ProbeState>().expect("proof state");
        assert_eq!(state.calls, 1);
        assert_eq!(state.candidate_borrows, 1);
        assert_eq!(state.owner_borrows, 1);
        assert_eq!(state.comparisons, [true]);
        assert_eq!(state.environment_expired_during_call, [true]);
        assert_eq!(state.root_during_call, [CaptureRootState::Live]);
        assert_eq!(state.root_after_alias_drop, [CaptureRootState::Live]);
        eprintln!(
            "active no-self root event: environment_expired={} root_during_call={:?} root_after_alias_drop={:?}",
            state.environment_expired_during_call[0],
            state.root_during_call[0],
            state.root_after_alias_drop[0],
        );
        (state.roots[0].clone(), state.owner_handles[0])
    };
    assert_eq!(
        root.observe(&machine.host_context()),
        CaptureRootState::Released
    );
    assert_eq!(machine.host_context().resource_count(), 1);
    {
        let context = machine.host_context();
        let token = context
            .typed_resource_with_key::<NumberArray>(handle, &array_key())
            .expect("released capture cell does not close the still-open payload");
        let payload = context
            .resource(&token)
            .expect("typed payload borrow")
            .get()
            .0
            .clone();
        assert_eq!(payload, vec![Value::Int(41)]);
        eprintln!(
            "actual active-frame release event: owner={handle:?} environment_expired=true root=Released payload_resources=1 typed_payload={payload:?}"
        );
    }
    machine
        .reset_for_reuse()
        .expect("explicit VM reset performs resource cleanup");
    assert_eq!(machine.host_context().resource_count(), 0);
    assert_eq!(
        root.observe(&machine.host_context()),
        CaptureRootState::Unavailable
    );
    eprintln!(
        "active-frame explicit reset event: root={:?} payload_resources={}",
        root.observe(&machine.host_context()),
        machine.host_context().resource_count(),
    );
}

#[test]
fn public_same_value_local_write_revokes_root_without_closing_payload() {
    let source = r#"
import { create, length, close } from "arrays";
import { check } from "proof";
import type { Resource } from "rustscript:types";
type Reader = (operation: number) => number;
export function factory(): Reader {
    let owner: Resource<"compat.number_array"> = create([19]);
    const reader: Reader = (operation: number): number => operation === 0
        ? check(owner) + length(owner)
        : close(owner);
    return reader;
}
"#;
    let mut machine = certified_vm(source, ProbePlan::PublicSameValueWrite);
    run_to_halt(&mut machine, "same-value source factory declaration");
    let factory = machine
        .resolve_exported_callable("factory")
        .expect("factory export");
    let closure = machine
        .invoke_callable(factory, &[])
        .expect("factory result");
    let Value::Callable(callable) = &closure else {
        panic!("factory result should be callable");
    };
    let environment = Arc::downgrade(callable.env.as_ref().expect("Move closure environment"));
    {
        let mut context = machine.host_context();
        context
            .module_state_mut::<ProbeState>()
            .expect("proof state")
            .environment = Some(environment);
    }
    assert_eq!(
        machine
            .invoke_callable(closure, &[Value::Int(0)])
            .expect("same-value setter control"),
        Value::Int(2)
    );
    let (root, handle) = {
        let context = machine.host_context();
        let state = context.module_state::<ProbeState>().expect("proof state");
        assert_eq!(state.comparisons, [true]);
        assert_eq!(state.root_during_call, [CaptureRootState::Live]);
        assert_eq!(
            state.root_after_public_write,
            [CaptureRootState::Unavailable]
        );
        (state.roots[0].clone(), state.owner_handles[0])
    };
    assert!(
        machine
            .capture_certification_stats()
            .is_some_and(|stats| !stats.valid)
    );
    assert_eq!(
        root.observe(&machine.host_context()),
        CaptureRootState::Unavailable
    );
    assert_eq!(machine.host_context().resource_count(), 1);
    eprintln!(
        "same-value invalidation event: owner={handle:?} root_during_call=Live root_after_write=Unavailable payload_resources=1 stats={:?}",
        machine
            .capture_certification_stats()
            .expect("invalidated source stats")
    );
    assert!(
        machine
            .host_context()
            .typed_resource_with_key::<NumberArray>(handle, &array_key())
            .is_ok()
    );
    machine
        .reset_for_reuse()
        .expect("explicit reset closes payload after invalidation");
    assert_eq!(machine.host_context().resource_count(), 0);
}

#[test]
fn public_different_value_write_revokes_owner_root_and_keeps_both_payloads_open() {
    let source = r#"
import { create, length, close } from "arrays";
import { check } from "proof";
import type { Resource } from "rustscript:types";
type Reader = (operation: number) => number;
export function factory(): Reader {
    let owner: Resource<"compat.number_array"> = create([19]);
    const reader: Reader = (operation: number): number => operation === 0
        ? check(owner) + length(owner)
        : close(owner);
    return reader;
}
"#;
    let mut machine = certified_vm(source, ProbePlan::PublicSameValueWrite);
    run_to_halt(&mut machine, "different-value source factory declaration");
    let factory = machine
        .resolve_exported_callable("factory")
        .expect("factory export");
    let closure = machine
        .invoke_callable(factory, &[])
        .expect("factory result");
    let replacement = {
        let mut context = machine.host_context();
        let token = context
            .push_resource(NumberArray(vec![Value::Int(29), Value::Int(31)]))
            .expect("independently typed replacement array");
        let handle = token.handle();
        context
            .typed_resource_with_key::<NumberArray>(handle, &array_key())
            .expect("replacement key, arena, generation and concrete type");
        context
            .module_state_mut::<ProbeState>()
            .expect("proof state")
            .replacement_handle = Some(handle);
        handle
    };
    assert_eq!(
        machine
            .invoke_callable(closure.clone(), &[Value::Int(0)])
            .expect("replace captured value"),
        Value::Int(3),
        "the continued source call must read the two-element replacement payload"
    );
    let (root, original) = {
        let context = machine.host_context();
        let state = context.module_state::<ProbeState>().expect("proof state");
        assert_eq!(state.comparisons, [true]);
        assert_eq!(state.root_during_call, [CaptureRootState::Live]);
        assert_eq!(
            state.root_after_public_write,
            [CaptureRootState::Unavailable]
        );
        (state.roots[0].clone(), state.owner_handles[0])
    };
    assert_ne!(original, replacement);
    assert_eq!(
        root.observe(&machine.host_context()),
        CaptureRootState::Unavailable
    );
    assert!(
        machine
            .capture_certification_stats()
            .is_some_and(|stats| !stats.valid)
    );
    assert_eq!(machine.host_context().resource_count(), 2);
    for (handle, values) in [
        (original, vec![Value::Int(19)]),
        (replacement, vec![Value::Int(29), Value::Int(31)]),
    ] {
        let context = machine.host_context();
        let token = context
            .typed_resource_with_key::<NumberArray>(handle, &array_key())
            .expect("both typed payloads remain open");
        assert_eq!(
            context
                .resource(&token)
                .expect("typed payload borrow")
                .get()
                .0,
            values
        );
    }
    machine
        .host_context()
        .module_state_mut::<ProbeState>()
        .expect("proof state")
        .plan = Some(ProbePlan::Unavailable);
    assert_eq!(
        machine
            .invoke_callable(closure, &[Value::Int(0)])
            .expect("ordinary access after authority revocation"),
        Value::Int(3)
    );
    {
        let context = machine.host_context();
        let state = context.module_state::<ProbeState>().expect("proof state");
        assert_eq!(state.query_errors, [CaptureQueryError::Unavailable]);
        assert_eq!(
            state.owner_borrows, 1,
            "invalidated relation must not issue another owner token"
        );
        eprintln!(
            "different-value replacement: original={original:?} replacement={replacement:?} Live→Unavailable, payloads=2, continued=Int(3), subsequent_query={:?}",
            state.query_errors
        );
    }
    machine
        .reset_for_reuse()
        .expect("explicit reset closes both payloads");
    assert_eq!(machine.host_context().resource_count(), 0);
    assert_eq!(
        root.observe(&machine.host_context()),
        CaptureRootState::Unavailable
    );
}

#[test]
fn repeated_certified_factories_record_live_roots_expiry_and_explicit_cleanup() {
    const FACTORIES: usize = 8;
    let mut machine = certified_vm(REPEATED_FACTORY_SOURCE, ProbePlan::MatchOwner);
    run_to_halt(&mut machine, "repeated source-factory declaration");
    let factory = machine
        .resolve_exported_callable("factory")
        .expect("factory export");
    let mut closures = Vec::with_capacity(FACTORIES);
    let mut environments = Vec::with_capacity(FACTORIES);
    for _ in 0..FACTORIES {
        let closure = machine
            .invoke_callable(factory.clone(), &[])
            .expect("factory should create one source-owned closure");
        let Value::Callable(callable) = &closure else {
            panic!("factory result should be callable");
        };
        environments.push(Arc::downgrade(
            callable
                .env
                .as_ref()
                .expect("one Move resource capture has an environment"),
        ));
        closures.push(closure);
    }
    assert_eq!(machine.host_context().resource_count(), FACTORIES);
    for closure in &closures {
        assert_eq!(
            machine
                .invoke_callable(closure.clone(), &[Value::Int(0)])
                .expect("captured closure should query its own owner"),
            Value::Int(2)
        );
    }
    let (roots, handles) = {
        let context = machine.host_context();
        let state = context.module_state::<ProbeState>().expect("proof state");
        assert_eq!(state.calls, FACTORIES);
        assert_eq!(state.candidate_borrows, FACTORIES);
        assert_eq!(state.owner_borrows, FACTORIES);
        assert!(state.comparisons.iter().all(|matched| *matched));
        assert_eq!(state.roots.len(), FACTORIES);
        assert!(
            state
                .root_during_call
                .iter()
                .all(|state| *state == CaptureRootState::Live)
        );
        assert_eq!(state.owner_values, vec![Value::Int(37); FACTORIES]);
        (state.roots.clone(), state.owner_handles.clone())
    };
    for root in &roots {
        assert_eq!(
            root.observe(&machine.host_context()),
            CaptureRootState::Live
        );
    }
    let mut foreign = certified_vm(REPEATED_FACTORY_SOURCE, ProbePlan::MatchOwner);
    run_to_halt(&mut foreign, "foreign VM root-observation control");
    assert_eq!(
        roots[0].observe(&foreign.host_context()),
        CaptureRootState::Unavailable,
        "a weak root cannot be observed in another VM's resource scope"
    );
    assert_eq!(
        roots[0].observe(&machine.host_context()),
        CaptureRootState::Live
    );
    for environment in &environments {
        assert!(environment.upgrade().is_some());
    }
    let stats = machine
        .capture_certification_stats()
        .expect("source compilation attaches compiler-owned capture facts");
    assert!(stats.valid);
    assert_eq!(stats.registrations, FACTORIES as u64);
    assert_eq!(stats.entries, FACTORIES);
    eprintln!(
        "source-owned repeated factories={FACTORIES} payload_resources={} weak_roots_live={} callable_capture_stats={stats:?}",
        machine.host_context().resource_count(),
        roots
            .iter()
            .filter(|root| root.observe(&machine.host_context()) == CaptureRootState::Live)
            .count(),
    );
    drop(closures);
    for environment in &environments {
        assert!(environment.upgrade().is_none());
    }
    for root in &roots {
        assert_eq!(
            root.observe(&machine.host_context()),
            CaptureRootState::Released
        );
    }
    assert_eq!(machine.host_context().resource_count(), FACTORIES);
    let released = roots
        .iter()
        .filter(|root| root.observe(&machine.host_context()) == CaptureRootState::Released)
        .count();
    let stats_after_environment_drop = machine
        .capture_certification_stats()
        .expect("source capture statistics remain observable after cell release");
    eprintln!(
        "source-owned repeated factories after alias drop: owner_handles={handles:?} payload_resources={} weak_roots_released={released} callable_capture_stats={stats_after_environment_drop:?}",
        machine.host_context().resource_count(),
    );
    for handle in &handles {
        let context = machine.host_context();
        let token = context
            .typed_resource_with_key::<NumberArray>(*handle, &array_key())
            .expect("released cells leave explicitly managed payloads open");
        assert_eq!(
            context
                .resource(&token)
                .expect("payload remains borrowable")
                .get()
                .0,
            &[Value::Int(37)]
        );
    }
    eprintln!(
        "released repeated-cell payload control: roots_released={released} payload_resources={} all_typed_payloads_readable=true value=Int(37)",
        machine.host_context().resource_count(),
    );
    machine
        .reset_for_reuse()
        .expect("explicit reset closes every repeated factory payload");
    assert_eq!(machine.host_context().resource_count(), 0);
    for root in &roots {
        assert_eq!(
            root.observe(&machine.host_context()),
            CaptureRootState::Unavailable
        );
    }
    eprintln!(
        "repeated-factory explicit reset: roots_unavailable={} payload_resources={}",
        roots
            .iter()
            .filter(|root| root.observe(&machine.host_context()) == CaptureRootState::Unavailable)
            .count(),
        machine.host_context().resource_count(),
    );
}

#[test]
fn uncertified_compilation_and_safe_vmbc_roundtrip_execute_without_capture_authority() {
    let (mut ordinary, _) = regular_vm(ORDINARY_CALL_SOURCE, ProbePlan::Unavailable);
    assert!(ordinary.capture_certification_stats().is_none());
    run_to_halt(&mut ordinary, "ordinary uncertified source");
    assert_eq!(ordinary.stack().last(), Some(&Value::Int(2)));
    assert!(matches!(
        ordinary
            .host_context()
            .module_state::<ProbeState>()
            .and_then(|state| state.query_errors.first()),
        Some(CaptureQueryError::Unavailable)
    ));
    assert_eq!(ordinary.host_context().resource_count(), 1);
    eprintln!(
        "ordinary source authority control: capture_stats={:?} query={:?} payload_resources=1",
        ordinary.capture_certification_stats(),
        ordinary
            .host_context()
            .module_state::<ProbeState>()
            .expect("ordinary state")
            .query_errors[0],
    );
    ordinary
        .reset_for_reuse()
        .expect("explicit reset closes ordinary source resource");

    let catalog = owner_catalog();
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        ORDINARY_CALL_SOURCE,
        crate::typescript_flavor(),
        crate::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("certified producer compiles before authority is deliberately discarded");
    let uncertified = compilation.into_uncertified();
    let bytes = vm::encode_program(&uncertified.program).expect("safe VMBC encoding");
    let decoded = vm::decode_program(&bytes).expect("safe VMBC decoding");
    assert_eq!(
        vm::encode_program(&decoded).expect("stable VMBC re-encoding"),
        bytes
    );
    let mut artifact_vm = bind_probe(Vm::new(decoded), &catalog, ProbePlan::Unavailable);
    assert!(artifact_vm.capture_certification_stats().is_none());
    run_to_halt(&mut artifact_vm, "uncertified VMBC roundtrip");
    assert_eq!(artifact_vm.stack().last(), Some(&Value::Int(2)));
    assert!(matches!(
        artifact_vm
            .host_context()
            .module_state::<ProbeState>()
            .and_then(|state| state.query_errors.first()),
        Some(CaptureQueryError::Unavailable)
    ));
    assert_eq!(artifact_vm.host_context().resource_count(), 1);
    eprintln!(
        "safe VMBC authority control: capture_stats={:?} query={:?} payload_resources=1 program_bytes={}",
        artifact_vm.capture_certification_stats(),
        artifact_vm
            .host_context()
            .module_state::<ProbeState>()
            .expect("VMBC state")
            .query_errors[0],
        bytes.len(),
    );
    artifact_vm
        .reset_for_reuse()
        .expect("explicit reset closes VMBC resource");
    assert_eq!(artifact_vm.host_context().resource_count(), 0);
}

#[derive(Default)]
struct FileCliObservation {
    init_certified: Vec<bool>,
    candidate_handles: Vec<ResourceHandle>,
    owner_handles: Vec<ResourceHandle>,
    comparisons: Vec<bool>,
    candidate_values: Vec<Value>,
    owner_values: Vec<Value>,
    roots_live_during_call: Vec<bool>,
    query_errors: Vec<CaptureQueryError>,
    query_certified: Vec<bool>,
    native_counts: Vec<u64>,
    native_ssa: Vec<bool>,
}

struct FileCliObserver {
    inner: CertifiedOwnerProbe,
    observations: Arc<Mutex<FileCliObservation>>,
}

impl HostOwnedFunction for FileCliObserver {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> VmResult<CallOutcome> {
        let outcome = self.inner.call(call);
        let query_certified = call
            .vm()
            .capture_certification_stats()
            .is_some_and(|stats| stats.valid);
        let native_count = call.vm().aot_exec_count();
        let native_ssa = call.vm().dump_aot_info().contains("lowering=ssa");
        let context = call.vm().host_context();
        let state = context
            .module_state::<ProbeState>()
            .expect("file-CLI VM installs proof state before guest execution");
        let mut observed = self
            .observations
            .lock()
            .expect("file-CLI observation record");
        observed.query_certified.push(query_certified);
        observed.native_counts.push(native_count);
        observed.native_ssa.push(native_ssa);
        observed
            .candidate_handles
            .extend(state.candidate_handles.iter().copied());
        observed
            .owner_handles
            .extend(state.owner_handles.iter().copied());
        observed
            .comparisons
            .extend(state.comparisons.iter().copied());
        observed
            .candidate_values
            .extend(state.candidate_values.iter().cloned());
        observed
            .owner_values
            .extend(state.owner_values.iter().cloned());
        observed.roots_live_during_call.extend(
            state
                .root_during_call
                .iter()
                .map(|root| *root == CaptureRootState::Live),
        );
        observed
            .query_errors
            .extend(state.query_errors.iter().cloned());
        outcome
    }
}

struct FileCliServices {
    services: vm::cli::CliHostServices<'static>,
    observations: Arc<Mutex<FileCliObservation>>,
}

fn file_cli_services(plan: ProbePlan) -> FileCliServices {
    let catalog = owner_catalog();
    let check_schema = vm::catalog_import_schemas(&catalog, "proof::check")
        .into_iter()
        .next()
        .expect("owner-check catalog schema");
    let observations = Arc::new(Mutex::new(FileCliObservation::default()));
    let init_observations = Arc::clone(&observations);
    let returned_observations = Arc::clone(&observations);
    let registry_catalog = Arc::clone(&catalog);
    let services = vm::cli::CliHostServices::new(catalog, move || {
        let mut registry = array_registry(&registry_catalog);
        let schema = check_schema.clone();
        let probe_observations = Arc::clone(&observations);
        registry
            .register_exact_owned("proof::check", 1, schema, move |_context| {
                Box::new(FileCliObserver {
                    inner: CertifiedOwnerProbe,
                    observations: Arc::clone(&probe_observations),
                })
            })
            .expect("guarded owner probe must bind only against the complete exact schema");
        registry.authorize_registered_builtin_import("proof::check");
        Ok(registry)
    })
    .with_capture_certification()
    .with_vm_init(move |machine| {
        init_observations
            .lock()
            .expect("file-CLI init observation record")
            .init_certified
            .push(
                machine
                    .capture_certification_stats()
                    .is_some_and(|stats| stats.valid),
            );
        assert!(
            !machine
                .host_context()
                .set_module_state(ProbeState::with_plan(plan))
        );
        Ok(())
    });
    FileCliServices {
        services,
        observations: returned_observations,
    }
}

fn file_cli_runtime() -> vm::cli::CliRuntime {
    vm::cli::CliRuntime {
        binary_name: "compat-file-cli-owner-test",
        default_source: "unused.ts",
        compile_options: file_cli_compile_options,
    }
}

fn file_cli_compile_options() -> vm::CompileSourceFileOptions {
    crate::compile_options_with_catalog(owner_catalog())
}

const FILE_CLI_OWNER_SOURCE: &str = r#"
import { create, length, write, close } from "arrays";
import { check } from "proof";
import type { Resource } from "rustscript:types";
type Dispatcher = (operation: number) => number;
function makeDispatcher(): Dispatcher {
    let owner: Resource<"compat.number_array"> = create([10]);
    const dispatcher: Dispatcher = (operation: number): number =>
        operation === 0
            ? (dispatcher === dispatcher
                ? check(owner) + write(owner, 0, 11) + length(owner)
                : check(owner) + write(owner, 0, 11) + length(owner))
            : close(owner);
    return dispatcher;
}
function main(): number {
    const dispatcher = makeDispatcher();
    const alias = dispatcher;
    const first = alias(0);
    return first + dispatcher(1);
}
main();
"#;

#[test]
fn public_file_cli_retains_source_certification_and_real_move_owner_through_alias() {
    let legacy = legacy_raw_source("LEGITIMATE_DISPATCHER_SOURCE");
    assert!(
        FILE_CLI_OWNER_SOURCE.trim() == legacy.trim(),
        "the public file-CLI owner source must match the unchanged legacy dispatcher fixture"
    );
    let scratch = std::env::var_os("TMPDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(format!("compat-file-cli-owner-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("create file-CLI owner scratch tree");
    let entry = scratch.join("owner-entry.ts");
    std::fs::write(&entry, FILE_CLI_OWNER_SOURCE).expect("write file-CLI owner entry");
    assert_eq!(
        std::fs::read_to_string(&entry).expect("read file-CLI owner entry"),
        FILE_CLI_OWNER_SOURCE,
        "the file CLI must consume the exact owner-dispatcher source bytes"
    );
    let cli = file_cli_services(ProbePlan::MatchOwner);
    let runtime = file_cli_runtime();
    vm::cli::run_with_args(
        &runtime,
        &[
            "--fuel".to_owned(),
            "100000".to_owned(),
            entry.display().to_string(),
        ],
        &cli.services,
    )
    .expect("public file CLI must execute the source-owned dispatcher through an alias");
    let observed = cli.observations.lock().expect("read file-CLI observations");
    assert_eq!(observed.init_certified, [true]);
    assert_eq!(observed.candidate_handles.len(), 1);
    assert_eq!(observed.owner_handles.len(), 1);
    assert_eq!(observed.candidate_handles, observed.owner_handles);
    assert_eq!(observed.comparisons, [true]);
    assert_eq!(observed.candidate_values, [Value::Int(10)]);
    assert_eq!(observed.owner_values, [Value::Int(10)]);
    assert_eq!(observed.roots_live_during_call, [true]);
    assert!(
        observed.query_errors.is_empty(),
        "MatchOwner must not record query errors: {:?}",
        observed.query_errors
    );
    eprintln!(
        "public file-CLI owner event: candidate={:?} owner={:?} payload={:?} live_root={} init_certified={:?}",
        observed.candidate_handles[0],
        observed.owner_handles[0],
        observed.owner_values[0],
        observed.roots_live_during_call[0],
        observed.init_certified,
    );
    let _ = std::fs::remove_dir_all(&scratch);
}

#[test]
fn public_file_cli_without_opt_in_keeps_valid_binding_but_no_owner_authority() {
    let scratch = std::env::var_os("TMPDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(format!("compat-file-cli-no-optin-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("create no-opt-in scratch tree");
    let entry = scratch.join("owner-entry.ts");
    std::fs::write(&entry, FILE_CLI_OWNER_SOURCE).expect("write no-opt-in owner entry");
    let catalog = owner_catalog();
    let check_schema = vm::catalog_import_schemas(&catalog, "proof::check")
        .into_iter()
        .next()
        .expect("owner-check catalog schema");
    let registry_catalog = Arc::clone(&catalog);
    let plain = vm::cli::CliHostServices::new(Arc::clone(&catalog), move || {
        let mut registry = array_registry(&registry_catalog);
        registry
            .register_exact_owned("proof::check", 1, check_schema.clone(), |_context| {
                Box::new(CertifiedOwnerProbe)
            })
            .expect("guarded owner probe must bind only against the complete exact schema");
        registry.authorize_registered_builtin_import("proof::check");
        Ok(registry)
    })
    .with_vm_init(|machine| {
        assert!(
            machine.capture_certification_stats().is_none(),
            "no-opt-in file CLI must not attach source capture facts"
        );
        assert!(
            !machine
                .host_context()
                .set_module_state(ProbeState::with_plan(ProbePlan::Unavailable))
        );
        Ok(())
    });
    let runtime = file_cli_runtime();
    vm::cli::run_with_args(
        &runtime,
        &[
            "--fuel".to_owned(),
            "100000".to_owned(),
            entry.display().to_string(),
        ],
        &plain,
    )
    .expect("the same bound file must execute without source certification");
    let _ = std::fs::remove_dir_all(&scratch);
}

struct FileCliScratch(std::path::PathBuf);
impl FileCliScratch {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::var_os("TMPDIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(format!("compat-owner-boundary-{}-{id}", std::process::id()));
        std::fs::create_dir(&root).expect("create unique source boundary tree");
        Self(root)
    }
    fn owner_project(&self) -> std::path::PathBuf {
        let split = FILE_CLI_OWNER_SOURCE
            .find("function main(): number {")
            .expect("legacy root main boundary");
        let dependency = FILE_CLI_OWNER_SOURCE[..split].replace(
            "function makeDispatcher()",
            "export function makeDispatcher()",
        );
        std::fs::create_dir(self.0.join("dep")).unwrap();
        std::fs::write(self.0.join("dep/owner.ts"), dependency).unwrap();
        let entry = self.0.join("entry.ts");
        std::fs::write(&entry, "import { makeDispatcher as allocate } from './dep/owner.ts';\nfunction main(): number { const dispatcher = allocate(); const alias = dispatcher; const first = alias(0); return first + dispatcher(1); }\nmain();\n").unwrap();
        entry
    }
}
impl Drop for FileCliScratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn assert_file_cli_unavailable(
    cli: &FileCliServices,
    initial: &[bool],
    native: bool,
    query_valid: bool,
) {
    let observed = cli.observations.lock().unwrap();
    assert_eq!(observed.init_certified, initial);
    assert_eq!(observed.candidate_values, [Value::Int(10)]);
    assert_eq!(observed.candidate_handles.len(), 1);
    assert!(observed.owner_handles.is_empty());
    assert!(observed.roots_live_during_call.is_empty());
    assert_eq!(observed.query_errors, [CaptureQueryError::Unavailable]);
    assert_eq!(observed.query_certified, [query_valid]);
    if native {
        assert!(observed.native_counts[0] > 1);
        assert_eq!(observed.native_ssa, [true]);
    }
    eprintln!(
        "file CLI unavailable: init={:?} candidate={:?} native_counts={:?} native_ssa={:?}",
        observed.init_certified,
        observed.candidate_handles,
        observed.native_counts,
        observed.native_ssa
    );
}

#[test]
fn public_file_cli_dependency_owner_and_vmbc_boundaries_are_observed() {
    let scratch = FileCliScratch::new();
    let entry = scratch.owner_project();
    let cli = file_cli_services(ProbePlan::MatchOwner);
    vm::cli::run_with_args(
        &file_cli_runtime(),
        &[entry.display().to_string()],
        &cli.services,
    )
    .unwrap();
    let observed = cli.observations.lock().unwrap();
    assert_eq!(observed.init_certified, [true]);
    assert_eq!(observed.candidate_values, [Value::Int(10)]);
    assert_eq!(observed.owner_values, [Value::Int(10)]);
    assert_eq!(observed.candidate_handles, observed.owner_handles);
    assert_eq!(observed.comparisons, [true]);
    assert_eq!(observed.roots_live_during_call, [true]);
    assert!(observed.query_errors.is_empty());
    drop(observed);
    let emitted = scratch.0.join("owner.vmbc");
    let untrusted = file_cli_services(ProbePlan::Unavailable);
    vm::cli::run_with_args(
        &file_cli_runtime(),
        &[
            "--emit-vmbc".into(),
            emitted.display().to_string(),
            entry.display().to_string(),
        ],
        &untrusted.services,
    )
    .unwrap();
    let ordinary =
        vm::compile_source_file_with_options(&entry, file_cli_compile_options()).unwrap();
    assert_eq!(
        std::fs::read(&emitted).unwrap(),
        vm::encode_program(&ordinary.program).unwrap()
    );
    vm::cli::run_with_args(
        &file_cli_runtime(),
        &[emitted.display().to_string()],
        &untrusted.services,
    )
    .unwrap();
    assert_file_cli_unavailable(&untrusted, &[true, false], false, false);
}

const FILE_CLI_NATIVE_BOUNDARY_SOURCE: &str = r#"
import { create, close } from "arrays";
import { check } from "proof";
import type { Resource } from "rustscript:types";
type Reader = (operation: number) => number;
function makeReader(): Reader {
    let owner: Resource<"compat.number_array"> = create([10]);
    const reader: Reader = (operation: number): number => operation === 0 ? check(owner) : close(owner);
    return reader;
}
const reader = makeReader();
const alias = reader;
const result = alias(0);
reader(1);
result;
"#;

#[test]
#[ignore = "fresh artifact child invoked by maintained outer control"]
fn file_cli_native_artifact_child() {
    let artifact = std::env::var("COMPAT_OWNER_ARTIFACT").expect("artifact child path");
    let cli = file_cli_services(ProbePlan::Unavailable);
    vm::cli::run_with_args(
        &file_cli_runtime(),
        &["--aot-load".into(), artifact],
        &cli.services,
    )
    .unwrap();
    assert_file_cli_unavailable(&cli, &[false], true, false);
}

#[cfg(test)]
#[path = "array_factory_lifecycle_tests.rs"]
mod factory_lifecycle_tests;

fn assert_file_cli_live(cli: &FileCliServices, native: bool) {
    let observed = cli.observations.lock().unwrap();
    assert_eq!(observed.init_certified, [true]);
    assert_eq!(observed.query_certified, [true]);
    assert_eq!(observed.candidate_values, [Value::Int(10)]);
    assert_eq!(observed.owner_values, [Value::Int(10)]);
    assert_eq!(observed.candidate_handles.len(), 1);
    assert_eq!(observed.candidate_handles, observed.owner_handles);
    assert_eq!(observed.comparisons, [true]);
    assert_eq!(observed.roots_live_during_call, [true]);
    assert!(observed.query_errors.is_empty());
    if native {
        assert!(observed.native_counts[0] > 1);
        assert_eq!(observed.native_ssa, [true]);
    }
    eprintln!(
        "file CLI checked live: init={:?} query={:?} candidate={:?} owner={:?} native_counts={:?} native_ssa={:?}",
        observed.init_certified,
        observed.query_certified,
        observed.candidate_handles,
        observed.owner_handles,
        observed.native_counts,
        observed.native_ssa
    );
}

#[test]
fn public_file_cli_source_native_checked_owner_through_dependency_and_alias() {
    let scratch = FileCliScratch::new();
    let split = FILE_CLI_NATIVE_BOUNDARY_SOURCE
        .find("\nconst reader = makeReader();")
        .unwrap();
    let dependency = FILE_CLI_NATIVE_BOUNDARY_SOURCE[..split]
        .replace("function makeReader()", "export function makeReader()");
    std::fs::create_dir(scratch.0.join("dep")).unwrap();
    std::fs::write(scratch.0.join("dep/native-owner.ts"), dependency).unwrap();
    let entry = scratch.0.join("entry.ts");
    std::fs::write(&entry, "import { makeReader as allocate } from './dep/native-owner.ts';\nconst reader = allocate(); const alias = reader; const result = alias(0); reader(1); result;\n").unwrap();
    let cli = file_cli_services(ProbePlan::MatchOwner);
    let result = vm::cli::run_with_args(
        &file_cli_runtime(),
        &[
            "--aot-save".into(),
            scratch.0.join("owner.aot").display().to_string(),
            entry.display().to_string(),
        ],
        &cli.services,
    );
    {
        let observed = cli.observations.lock().unwrap();
        eprintln!(
            "native dependency owner result={result:?} init={:?} query={:?} candidate={:?} errors={:?} native_counts={:?} native_ssa={:?}",
            observed.init_certified,
            observed.query_certified,
            observed.candidate_values,
            observed.query_errors,
            observed.native_counts,
            observed.native_ssa
        );
        assert_eq!(observed.init_certified, [true]);
        assert_eq!(observed.query_certified, [true]);
        assert_eq!(observed.candidate_values, [Value::Int(10)]);
        assert!(observed.native_counts[0] > 1);
        assert_eq!(observed.native_ssa, [true]);
    }
    result.expect("source-native dependency/alias must expose a genuine checked Live owner");
    assert_file_cli_live(&cli, true);
}

#[test]
fn public_file_cli_source_native_owner_is_live_while_loaded_artifacts_fail_closed() {
    let scratch = FileCliScratch::new();
    let entry = scratch.0.join("native-owner.ts");
    let artifact = scratch.0.join("owner.aot");
    std::fs::write(&entry, FILE_CLI_NATIVE_BOUNDARY_SOURCE).unwrap();
    let cli = file_cli_services(ProbePlan::MatchOwner);
    vm::cli::run_with_args(
        &file_cli_runtime(),
        &[
            "--aot-save".into(),
            artifact.display().to_string(),
            entry.display().to_string(),
        ],
        &cli.services,
    )
    .unwrap();
    assert_file_cli_live(&cli, true);
    let companion = file_cli_services(ProbePlan::Unavailable);
    vm::cli::run_with_args(
        &file_cli_runtime(),
        &[
            "--aot-load".into(),
            artifact.display().to_string(),
            entry.display().to_string(),
        ],
        &companion.services,
    )
    .unwrap();
    assert_file_cli_unavailable(&companion, &[true], true, false);
    std::fs::remove_file(&entry).unwrap();
    let stdout = std::fs::File::create(scratch.0.join("child.stdout")).unwrap();
    let stderr = std::fs::File::create(scratch.0.join("child.stderr")).unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "runtime::array::certified_tests::file_cli_native_artifact_child",
            "--nocapture",
        ])
        .env("COMPAT_OWNER_ARTIFACT", &artifact)
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("fresh artifact child exceeded deadline");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let stdout = std::fs::read_to_string(scratch.0.join("child.stdout")).unwrap();
    let stderr = std::fs::read_to_string(scratch.0.join("child.stderr")).unwrap();
    assert!(
        status.success(),
        "fresh standalone failed: {stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("1 passed; 0 failed"),
        "child filter must execute its control: {stdout}"
    );
    assert!(stderr.contains("file CLI unavailable: init=[false]"));
    eprintln!("fresh owner artifact child:\n{stdout}\n{stderr}");
}

#[derive(Default)]
struct A3ConsumptionObservation {
    calls: usize,
    before: Option<CaptureRootState>,
    after_take: Option<CaptureRootState>,
    payload: Vec<Value>,
    certified_before: bool,
    live_payloads_before: usize,
}
struct A3Consume;
impl HostOwnedFunction for A3Consume {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> VmResult<CallOutcome> {
        let raw = match call.arg(0) {
            Some(Value::Int(raw)) => {
                u64::try_from(*raw).map_err(|_| VmError::TypeMismatch("owned resource argument"))?
            }
            _ => return Err(VmError::TypeMismatch("owned resource argument")),
        };
        let handle =
            ResourceHandle::from_raw(raw).map_err(|e| VmError::HostError(e.to_string()))?;
        let certified = call
            .vm_ref()
            .capture_certification_stats()
            .is_some_and(|s| s.valid);
        let mut context = call.vm().host_context();
        let root = context
            .module_state::<ProbeState>()
            .unwrap()
            .roots
            .first()
            .unwrap()
            .clone();
        let issued_handle = context.module_state::<ProbeState>().unwrap().owner_handles[0];
        assert_eq!(handle, issued_handle);
        let token = context
            .typed_resource_with_key::<NumberArray>(handle, &array_key())
            .map_err(|e| VmError::HostError(e.to_string()))?;
        let payload = context
            .resource(&token)
            .map_err(|e| VmError::HostError(e.to_string()))?
            .get()
            .0
            .clone();
        let before = root.observe(&context);
        let resources = context.resource_count();
        let owned = context
            .take_resource_with_key::<NumberArray>(handle, &array_key())
            .map_err(|e| VmError::HostError(e.to_string()))?;
        assert_eq!(owned.0, payload);
        let after = root.observe(&context);
        drop(owned);
        let state = context
            .module_state_mut::<A3ConsumptionObservation>()
            .unwrap();
        state.calls += 1;
        state.before = Some(before);
        state.after_take = Some(after);
        state.payload = payload;
        state.certified_before = certified;
        state.live_payloads_before = resources;
        eprintln!(
            "A3 guest TakeOwned before={before:?} after_payload_take={after:?} resources_before={resources} certified={certified}"
        );
        Ok(CallOutcome::Return(CallReturn::one(Value::Int(1))))
    }
}
fn a3_consume_catalog() -> Arc<HostApiCatalog> {
    let original = owner_catalog();
    let mut builder = HostApiCatalog::builder();
    for resource in original.resources() {
        builder.resource(resource.clone());
    }
    for function in original.functions() {
        builder.function(function.clone());
    }
    builder.function(HostFunctionSchema::with_return(
        "proof::consume",
        vec![HostParamSchema::with_passing(
            "owner",
            HostTypeSchema::Resource(array_key()),
            HostParamPassing::TakeOwned,
        )],
        HostTypeSchema::Number,
    ));
    Arc::new(builder.build().unwrap())
}
const A3_GUEST_CONSUMPTION: &str = r#"
import { create } from "arrays";
import { check, consume } from "proof";
import type { Resource } from "rustscript:types";
type Dispatcher = () => number;
function factory(): Dispatcher {
    let owner: Resource<"compat.number_array"> = create([31]);
    const dispatcher: Dispatcher = (): number => { check(owner); return consume(owner); };
    return dispatcher;
}
const dispatcher = factory();
dispatcher();
"#;
#[test]
fn a3_guest_owned_transfer_observes_old_root_before_payload_consumption() {
    let catalog = a3_consume_catalog();
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        A3_GUEST_CONSUMPTION,
        crate::typescript_flavor(),
        crate::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .unwrap();
    let mut machine = compilation.into_vm().unwrap();
    let emitted = vm::disassemble_program(machine.program());
    let checked = emitted.find("import proof::check/1 (argc=1)").unwrap();
    let consuming = emitted.find("import proof::consume/1 (argc=1)").unwrap();
    assert!(
        checked < consuming && emitted[checked..consuming].contains("builtin __detach_local/1")
    );
    eprintln!("A3 emitted guest transfer:\n{emitted}");
    machine
        .host_context()
        .set_module_state(ProbeState::with_plan(ProbePlan::MatchOwner));
    machine
        .host_context()
        .set_module_state(A3ConsumptionObservation::default());
    let mut registry = owner_registry(&catalog);
    let schema = vm::catalog_import_schemas(&catalog, "proof::consume")
        .into_iter()
        .next()
        .unwrap();
    registry
        .register_exact_owned("proof::consume", 1, schema, |_context| Box::new(A3Consume))
        .unwrap();
    registry.authorize_registered_builtin_import("proof::consume");
    registry.bind_vm_cached(&mut machine).unwrap();
    run_to_halt(&mut machine, "actual guest ownership transfer");
    let context = machine.host_context();
    let state = context.module_state::<A3ConsumptionObservation>().unwrap();
    assert_eq!(state.calls, 1);
    let checked = context.module_state::<ProbeState>().unwrap();
    assert_eq!(checked.root_during_call, [CaptureRootState::Live]);
    assert_eq!(checked.candidate_handles, checked.owner_handles);
    assert_eq!(checked.owner_borrows, 1);
    assert_eq!(state.payload, [Value::Int(31)]);
    assert_eq!(state.live_payloads_before, 1);
    assert!(state.certified_before);
    assert_eq!(state.before, Some(CaptureRootState::Unavailable));
    assert_eq!(state.after_take, Some(CaptureRootState::Unavailable));
    assert_eq!(context.resource_count(), 0);
    machine.reset_for_reuse().unwrap();
    assert_eq!(machine.host_context().resource_count(), 0);
}

const A3_ARTIFACT_READER: &str = r#"
import { create, close } from "arrays";
import { check } from "proof";
import type { Resource } from "rustscript:types";
type Reader = (operation: number) => number;
export function factory(): Reader {
    let owner: Resource<"compat.number_array"> = create([41]);
    const reader: Reader = (operation: number): number => operation === 0 ? check(owner) : close(owner);
    return reader;
}
"#;
#[test]
fn a3_same_vm_artifact_binding_revokes_issued_root_and_preserves_program_and_payload() {
    let catalog = owner_catalog();
    let mut machine = certified_vm(A3_ARTIFACT_READER, ProbePlan::MatchOwner);
    let program_before = vm::encode_program(machine.program()).unwrap();
    run_to_halt(&mut machine, "initialize exported factory module");
    let factory = machine.resolve_exported_callable("factory").unwrap();
    let reader = machine.invoke_callable(factory, &[]).unwrap();
    let result = machine
        .invoke_callable(reader.clone(), &[Value::Int(0)])
        .unwrap();
    assert_eq!(result, Value::Int(1));
    let (root, handle) = {
        let context = machine.host_context();
        let state = context.module_state::<ProbeState>().unwrap();
        assert_eq!(state.comparisons, [true]);
        (state.roots[0].clone(), state.owner_handles[0])
    };
    assert_eq!(
        root.observe(&machine.host_context()),
        CaptureRootState::Live
    );
    let ordinary = vm::compile_source_with_flavor_and_options(
        A3_ARTIFACT_READER,
        crate::typescript_flavor(),
        crate::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .unwrap();
    assert_eq!(
        vm::encode_program(&ordinary.program).unwrap(),
        program_before
    );
    let mut writer = bind_probe(Vm::new(ordinary.program), &catalog, ProbePlan::Unavailable);
    let artifact = writer.encode_aot_artifact().unwrap();
    let mut corrupt = artifact.clone();
    corrupt[0] ^= 0xff;
    let error = machine
        .load_aot_artifact(&corrupt)
        .expect_err("malformed real artifact must be refused transactionally");
    eprintln!("A3 rejected malformed artifact: {error}");
    assert_eq!(
        vm::encode_program(machine.program()).unwrap(),
        program_before
    );
    assert!(machine.capture_certification_stats().unwrap().valid);
    assert_eq!(
        root.observe(&machine.host_context()),
        CaptureRootState::Live
    );
    {
        let context = machine.host_context();
        let token = context
            .typed_resource_with_key::<NumberArray>(handle, &array_key())
            .unwrap();
        assert_eq!(context.resource(&token).unwrap().get().0, [Value::Int(41)]);
        assert_eq!(context.resource_count(), 1);
    }
    machine.load_aot_artifact(&artifact).unwrap();
    assert_eq!(
        vm::encode_program(machine.program()).unwrap(),
        program_before
    );
    assert!(!machine.capture_certification_stats().unwrap().valid);
    assert_eq!(
        root.observe(&machine.host_context()),
        CaptureRootState::Unavailable
    );
    {
        let context = machine.host_context();
        let token = context
            .typed_resource_with_key::<NumberArray>(handle, &array_key())
            .unwrap();
        assert_eq!(context.resource(&token).unwrap().get().0, [Value::Int(41)]);
        assert_eq!(context.resource_count(), 1);
    }
    eprintln!(
        "A3 same VM engine binding: old_root=Unavailable program_bytes_unchanged=true payload=[Int(41)] resources=1"
    );
    drop(reader);
    machine.reset_for_reuse().unwrap();
    assert_eq!(machine.host_context().resource_count(), 0);
}
