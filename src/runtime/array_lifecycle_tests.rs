use std::sync::Arc;

use vm::resource::ResourceHandle;
use vm::{
    CallOutcome, CallReturn, CaptureBindingMode, HostApiCatalog, HostFunctionRegistry,
    HostFunctionSchema, HostOwnedFunction, HostParamPassing, HostParamSchema, HostTypeSchema,
    OwnedHostCall, TypeSchema, Value, Vm, VmError, VmResult,
};

use super::{NumberArray, array_key, typescript_array_host_module};

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
    let production_module = typescript_array_host_module();
    let production_catalog = production_module
        .catalog()
        .expect("production array catalog should build");
    let mut builder = HostApiCatalog::builder();
    for resource in production_catalog.resources() {
        builder.resource(resource.clone());
    }
    for function in production_catalog.functions() {
        builder.function(function.clone());
    }
    builder.function(owner_check_schema());
    Arc::new(builder.build().expect("owner proof catalog should build"))
}

// Fixture-only experiment: the aggregate TypeMap merges physical slots from
// different frames. Its source-slot schema is not declaring-frame provenance.
// This handler must not be used as a general checked owner API or collector.
struct FixtureAggregateSelfOwnerAssociation;

impl HostOwnedFunction for FixtureAggregateSelfOwnerAssociation {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> VmResult<CallOutcome> {
        let candidate_raw = match call.arg(0) {
            Some(Value::Int(raw)) => {
                u64::try_from(*raw).map_err(|_| VmError::TypeMismatch("proof::check resource"))?
            }
            _ => return Err(VmError::TypeMismatch("proof::check resource")),
        };
        let candidate = ResourceHandle::from_raw(candidate_raw)
            .map_err(|error| VmError::HostError(error.to_string()))?;

        let (active_frame, prototype_id, self_callable) = {
            let machine = call.vm_ref();
            let frames = machine.execution_frames();
            let frame = frames.last().cloned().ok_or_else(|| {
                VmError::HostError("proof::check requires an active dispatcher frame".to_owned())
            })?;
            let prototype_id = frame.prototype_id.ok_or_else(|| {
                VmError::HostError("proof::check requires a script dispatcher frame".to_owned())
            })?;
            let prototype = machine
                .program()
                .callable_prototypes
                .get(prototype_id as usize)
                .ok_or(VmError::InvalidCallablePrototype(prototype_id))?;
            let self_slot = prototype.self_slot.ok_or_else(|| {
                VmError::HostError("proof::check requires a self-bearing dispatcher".to_owned())
            })?;
            let self_relative = usize::from(self_slot);
            if self_relative >= frame.local_count {
                return Err(VmError::HostError(
                    "proof::check self slot is outside its active frame".to_owned(),
                ));
            }
            let self_index = frame.local_base.checked_add(self_relative).ok_or_else(|| {
                VmError::HostError("proof::check self slot offset overflow".to_owned())
            })?;
            let self_callable = match machine.locals().get(self_index) {
                Some(Value::Callable(value))
                    if value.prototype_id == prototype_id && value.env.is_some() =>
                {
                    Arc::clone(value)
                }
                _ => {
                    return Err(VmError::HostError(
                        "proof::check active self slot is not its callable".to_owned(),
                    ));
                }
            };
            (frame, prototype_id, self_callable)
        };

        let candidate = {
            let context = call.vm().host_context();
            let borrowed = context
                .borrow_resource_with_key::<NumberArray>(candidate, &array_key())
                .map_err(|error| VmError::HostError(error.to_string()))?;
            let handle = borrowed.handle();
            drop(borrowed);
            handle
        };

        let captured_owner = {
            let machine = call.vm_ref();
            let frames = machine.execution_frames();
            let frame = frames.last().ok_or_else(|| {
                VmError::HostError("proof::check lost its active dispatcher frame".to_owned())
            })?;
            if frame != &active_frame || frame.prototype_id != Some(prototype_id) {
                return Err(VmError::HostError(
                    "proof::check active dispatcher changed during typed borrow".to_owned(),
                ));
            }
            let prototype = machine
                .program()
                .callable_prototypes
                .get(prototype_id as usize)
                .ok_or(VmError::InvalidCallablePrototype(prototype_id))?;
            let self_slot = prototype.self_slot.ok_or_else(|| {
                VmError::HostError("proof::check requires a self-bearing dispatcher".to_owned())
            })?;
            let self_relative = usize::from(self_slot);
            if self_relative >= frame.local_count {
                return Err(VmError::HostError(
                    "proof::check self slot is outside its active frame".to_owned(),
                ));
            }
            let self_index = frame.local_base.checked_add(self_relative).ok_or_else(|| {
                VmError::HostError("proof::check self slot offset overflow".to_owned())
            })?;
            match machine.locals().get(self_index) {
                Some(Value::Callable(value)) if Arc::ptr_eq(value, &self_callable) => {}
                _ => {
                    return Err(VmError::HostError(
                        "proof::check active self callable changed during typed borrow".to_owned(),
                    ));
                }
            }

            if prototype.capture_slots.len() != prototype.capture_source_slots.len()
                || prototype.capture_slots.len() != prototype.capture_modes.len()
            {
                return Err(VmError::HostError(
                    "proof::check dispatcher capture metadata is inconsistent".to_owned(),
                ));
            }
            let type_map = machine.program().type_map.as_ref().ok_or_else(|| {
                VmError::HostError("proof::check requires checked capture schemas".to_owned())
            })?;
            let expected_schema = TypeSchema::Resource(array_key());
            let owner_slots = prototype
                .capture_slots
                .iter()
                .zip(&prototype.capture_source_slots)
                .zip(&prototype.capture_modes)
                .filter_map(|((target, source), mode)| {
                    let source_schema = type_map
                        .local_schemas
                        .get(usize::from(*source))
                        .and_then(Option::as_ref);
                    eprintln!(
                        "fixture aggregate capture: prototype={prototype_id} source={source} target={target} mode={mode:?} source_schema={source_schema:?}; candidate typed borrow passed"
                    );
                    (*mode == CaptureBindingMode::Move
                        && Some(*target) != prototype.self_slot
                        && source_schema == Some(&expected_schema))
                    .then_some(*target)
                })
                .collect::<Vec<_>>();
            let [owner_slot] = owner_slots.as_slice() else {
                return Err(VmError::HostError(
                    "proof::check requires exactly one checked Move resource capture".to_owned(),
                ));
            };
            let owner_relative = usize::from(*owner_slot);
            if owner_relative >= frame.local_count {
                return Err(VmError::HostError(
                    "proof::check owner capture is outside its active frame".to_owned(),
                ));
            }
            let owner_index = frame
                .local_base
                .checked_add(owner_relative)
                .ok_or_else(|| {
                    VmError::HostError("proof::check owner capture offset overflow".to_owned())
                })?;
            match machine.locals().get(owner_index) {
                Some(Value::Int(raw)) => ResourceHandle::from_raw(
                    u64::try_from(*raw)
                        .map_err(|_| VmError::TypeMismatch("active owner resource"))?,
                )
                .map_err(|error| VmError::HostError(error.to_string()))?,
                _ => {
                    return Err(VmError::HostError(
                        "proof::check active Move capture is not a resource handle".to_owned(),
                    ));
                }
            }
        };

        let captured_owner = {
            let context = call.vm().host_context();
            let borrowed = context
                .borrow_resource_with_key::<NumberArray>(captured_owner, &array_key())
                .map_err(|error| VmError::HostError(error.to_string()))?;
            let handle = borrowed.handle();
            drop(borrowed);
            handle
        };
        eprintln!(
            "fixture typed owner comparison: equal={}",
            candidate == captured_owner
        );
        if candidate != captured_owner {
            return Err(VmError::HostError(
                "proof::check candidate is not the active callable's Move-captured resource"
                    .to_owned(),
            ));
        }
        Ok(CallOutcome::Return(CallReturn::one(Value::Int(1))))
    }
}

fn owner_registry(catalog: &HostApiCatalog) -> HostFunctionRegistry {
    let mut registry = HostFunctionRegistry::restricted();
    typescript_array_host_module()
        .install_from_catalog(&mut registry, catalog)
        .expect("production array adapters should match the extended catalog");
    let schema = vm::catalog_import_schemas(catalog, "proof::check")
        .into_iter()
        .next()
        .expect("typed owner proof import schema");
    registry
        .register_exact_owned("proof::check", 1, schema, |_context| {
            Box::new(FixtureAggregateSelfOwnerAssociation)
        })
        .expect("guarded owned adapter should register against the extended catalog schema");
    registry.authorize_registered_builtin_import("proof::check");
    registry
}

const WRONG_PAIR_SOURCE: &str = r#"
import { create, close } from "arrays";
import { check } from "proof";
import type { Resource } from "rustscript:types";
type OwnerDispatcher = (candidate: Resource<"compat.number_array">, operation: number) => number;
function makeDispatcher(): OwnerDispatcher {
    let owner: Resource<"compat.number_array"> = create([10]);
    const dispatcher: OwnerDispatcher = (candidate, operation): number =>
        operation === 0
            ? (dispatcher === dispatcher ? check(candidate) : check(candidate))
            : close(owner);
    return dispatcher;
}
function main(): number {
    const other: Resource<"compat.number_array"> = create([20]);
    const dispatcher = makeDispatcher();
    return dispatcher(other, 0);
}
main();
"#;

#[test]
fn typed_resource_parameter_is_local_to_an_arrow_without_owner_capture() {
    let source = r#"
import { create, length } from "arrays";
import type { Resource } from "rustscript:types";
type Reader = (candidate: Resource<"compat.number_array">) => number;
function main(): number {
    const owner: Resource<"compat.number_array"> = create([31]);
    const reader: Reader = (candidate: Resource<"compat.number_array">): number => length(candidate);
    return reader(owner);
}
main();
"#;
    let catalog = owner_catalog();
    let compiled = vm::compile_source_with_flavor_and_options(
        source,
        crate::typescript_flavor(),
        crate::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("a typed Resource parameter should remain a callable-local slot");
    let mut machine = Vm::new(compiled.program);
    owner_registry(&catalog)
        .bind_vm_cached(&mut machine)
        .expect("typed array imports should bind for a Resource-parameter control");
    assert_eq!(
        machine
            .run()
            .expect("Resource parameter call should execute"),
        vm::VmStatus::Halted
    );
    assert_eq!(machine.stack().last(), Some(&Value::Int(1)));
    assert_eq!(machine.host_context().resource_count(), 1);
    machine
        .reset_for_reuse()
        .expect("Resource parameter control should reset");
    assert_eq!(machine.host_context().resource_count(), 0);
}

#[test]
fn self_bearing_move_owner_with_another_resource_parameter_hits_pinned_barrier() {
    let catalog = owner_catalog();
    let error = match crate::collect_typescript_type_hints_with_options(
        WRONG_PAIR_SOURCE,
        crate::compile_options_with_catalog(Arc::clone(&catalog)),
    ) {
        Ok(_) => panic!(
            "pinned Core unexpectedly admitted a second Resource argument alongside the Move-captured owner"
        ),
        Err(error) => error,
    };
    let diagnostic = format!("{error:?}");
    assert!(
        diagnostic.contains("E_OWNERSHIP_COPY_RESOURCE"),
        "the real TypeScript wrong-pair control must stop at the pinned ownership barrier: {diagnostic}"
    );
    assert!(
        diagnostic.contains("closure capture of resource value")
            && diagnostic.contains("resources cannot be cloned into a closure cell"),
        "the barrier must be the concrete move-only dispatcher diagnostic: {diagnostic}"
    );
}

const LEGITIMATE_DISPATCHER_SOURCE: &str = r#"
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
fn self_bearing_move_owner_dispatcher_executes_through_an_alias() {
    let source = LEGITIMATE_DISPATCHER_SOURCE;
    let catalog = owner_catalog();
    let compiled = vm::compile_source_with_flavor_and_options(
        source,
        crate::typescript_flavor(),
        crate::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("real TypeScript should compile a self-bearing single-Move owner dispatcher");
    let dispatcher_schema = TypeSchema::Callable {
        params: vec![TypeSchema::Number],
        result: Box::new(TypeSchema::Number),
    };
    let dispatcher = compiled
        .program
        .callable_prototypes
        .iter()
        .find(|prototype| {
            prototype.schema.as_ref() == Some(&dispatcher_schema)
                && prototype.capture_modes.contains(&CaptureBindingMode::Move)
        })
        .expect("Program should contain the one-Move dispatcher prototype");
    let self_slot = dispatcher
        .self_slot
        .expect("self-referential dispatcher must have the real self slot");
    assert!(dispatcher.capture_slots.contains(&self_slot));
    assert_eq!(
        dispatcher.capture_slots.len(),
        dispatcher.capture_modes.len()
    );
    assert_eq!(
        dispatcher
            .capture_modes
            .iter()
            .filter(|mode| **mode == CaptureBindingMode::Move)
            .count(),
        1,
        "the dispatcher must move exactly one Resource owner"
    );
    let move_capture_source = dispatcher
        .capture_source_slots
        .iter()
        .zip(&dispatcher.capture_modes)
        .find_map(|(slot, mode)| (*mode == CaptureBindingMode::Move).then_some(*slot))
        .expect("one Move capture source slot");
    assert_eq!(
        compiled
            .program
            .type_map
            .as_ref()
            .and_then(|type_map| type_map.local_schemas.get(usize::from(move_capture_source)))
            .and_then(Option::as_ref),
        Some(&TypeSchema::Resource(array_key())),
        "the actual Move capture must remain the checked Resource schema"
    );

    assert_eq!(
        compiled
            .program
            .host_import_schemas()
            .iter()
            .flatten()
            .find(|schema| schema.name == "proof::check")
            .expect("compiled Program should contain the owner-check import")
            .params[0]
            .passing,
        HostParamPassing::Borrow
    );

    let mut machine = Vm::new(compiled.program);
    owner_registry(&catalog)
        .bind_vm_cached(&mut machine)
        .expect("checked array catalog should bind to the actual VM");
    assert_eq!(
        machine.run().expect("self-bearing dispatcher should run"),
        vm::VmStatus::Halted
    );
    assert_eq!(machine.stack().last(), Some(&Value::Int(14)));
    assert_eq!(machine.host_context().resource_count(), 0);
}

#[test]
fn ordinary_typed_caller_cannot_claim_a_self_owned_resource() {
    let source = r#"
import { create } from "arrays";
import { check } from "proof";
import type { Resource } from "rustscript:types";
function main(): number {
    const candidate: Resource<"compat.number_array"> = create([30]);
    return check(candidate);
}
main();
"#;
    let catalog = owner_catalog();
    let compiled = vm::compile_source_with_flavor_and_options(
        source,
        crate::typescript_flavor(),
        crate::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("ordinary caller should be admitted by the exact typed catalog");
    assert!(
        compiled
            .program
            .callable_prototypes
            .iter()
            .all(|prototype| prototype.self_slot.is_none()),
        "ordinary function prototypes must not acquire synthetic self slots"
    );

    let mut machine = Vm::new(compiled.program);
    owner_registry(&catalog)
        .bind_vm_cached(&mut machine)
        .expect("ordinary caller's typed import should bind");
    let error = machine
        .run()
        .expect_err("an admitted Resource Borrow from an ordinary caller must be rejected");
    assert!(
        error
            .to_string()
            .contains("requires a self-bearing dispatcher"),
        "ordinary caller rejection must come from missing owner provenance: {error}"
    );
    assert_eq!(machine.host_context().resource_count(), 1);
    machine
        .reset_for_reuse()
        .expect("ordinary caller rejection should leave the resource resettable");
    assert_eq!(machine.host_context().resource_count(), 0);
}

fn fixture_program(source: &str) -> (vm::CompiledProgram, Arc<HostApiCatalog>) {
    let catalog = owner_catalog();
    crate::collect_typescript_type_hints_with_options(
        source,
        crate::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("owner probe must pass public typehints");
    vm::analyze_source_from_string_with_options(
        "main.ts",
        source,
        crate::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("owner probe must pass public analysis");
    let compiled = vm::compile_source_with_flavor_and_options(
        source,
        crate::typescript_flavor(),
        crate::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("owner probe must compile through the real catalog");
    for (id, p) in compiled.program.callable_prototypes.iter().enumerate() {
        assert_eq!(p.capture_slots.len(), p.capture_source_slots.len());
        assert_eq!(p.capture_slots.len(), p.capture_modes.len());
        eprintln!(
            "fixture prototype={id} schema={:?} params={:?} self={:?} source={:?} target={:?} modes={:?}",
            p.schema,
            p.parameter_slots,
            p.self_slot,
            p.capture_source_slots,
            p.capture_slots,
            p.capture_modes
        );
        for source in &p.capture_source_slots {
            eprintln!(
                "fixture source={source} aggregate_schema={:?}",
                compiled
                    .program
                    .type_map
                    .as_ref()
                    .and_then(|m| m.local_schemas.get(usize::from(*source)))
            );
        }
    }
    (compiled, catalog)
}

#[test]
fn aggregate_schema_perturbation_exposes_fixture_false_rejection() {
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
    let (baseline, catalog) = fixture_program(LEGITIMATE_DISPATCHER_SOURCE);
    let baseline_dispatcher = baseline
        .program
        .callable_prototypes
        .iter()
        .find(|p| p.self_slot.is_some() && p.capture_modes.contains(&CaptureBindingMode::Move))
        .expect("baseline self-bearing Move dispatcher");
    let baseline_signature = baseline_dispatcher.schema.clone();
    let baseline_modes = baseline_dispatcher.capture_modes.clone();
    let baseline_source = baseline_dispatcher
        .capture_source_slots
        .iter()
        .zip(&baseline_dispatcher.capture_modes)
        .find_map(|(source, mode)| (*mode == CaptureBindingMode::Move).then_some(*source))
        .expect("one baseline Move source");
    assert_eq!(
        baseline.program.type_map.as_ref().unwrap().local_schemas[usize::from(baseline_source)],
        Some(TypeSchema::Resource(array_key()))
    );
    let mut baseline_machine = Vm::new(baseline.program);
    owner_registry(&catalog)
        .bind_vm_cached(&mut baseline_machine)
        .expect("baseline imports bind");
    baseline_machine
        .run()
        .expect("the unchanged dispatcher must pass the unperturbed fixture");
    assert_eq!(baseline_machine.stack().last(), Some(&Value::Int(14)));
    assert_eq!(baseline_machine.host_context().resource_count(), 0);
    baseline_machine.reset_for_reuse().expect("baseline reset");

    let source = format!("{LEGITIMATE_DISPATCHER_SOURCE}\n{perturbation}");
    let (compiled, catalog) = fixture_program(&source);
    let dispatcher = compiled
        .program
        .callable_prototypes
        .iter()
        .find(|p| p.self_slot.is_some() && p.capture_modes.contains(&CaptureBindingMode::Move))
        .expect("perturbed self-bearing Move dispatcher");
    assert_eq!(dispatcher.schema, baseline_signature);
    assert_eq!(dispatcher.capture_modes, baseline_modes);
    let source_slot = dispatcher
        .capture_source_slots
        .iter()
        .zip(&dispatcher.capture_modes)
        .find_map(|(source, mode)| (*mode == CaptureBindingMode::Move).then_some(*source))
        .expect("one perturbed Move source");
    assert_eq!(
        compiled.program.type_map.as_ref().unwrap().local_schemas[usize::from(source_slot)],
        Some(TypeSchema::Bool),
        "the unrelated boolean parameter replaced the aggregate source schema"
    );
    let mut machine = Vm::new(compiled.program);
    owner_registry(&catalog)
        .bind_vm_cached(&mut machine)
        .expect("probe imports bind");
    let outcome = machine.run();
    eprintln!("same legitimate dispatcher with independent typed frames: {outcome:?}");
    let error =
        outcome.expect_err("aggregate schema fixture falsely rejects this legitimate owner");
    assert!(
        matches!(error, VmError::HostError(ref message) if message == "proof::check requires exactly one checked Move resource capture")
    );
    assert_eq!(machine.host_context().resource_count(), 1);
    machine
        .reset_for_reuse()
        .expect("schema perturbation reset");
    assert_eq!(machine.host_context().resource_count(), 0);
}

const NUMERIC_WRONG_RESOURCE_SOURCE: &str = r#"
import { create, length, close } from "arrays";
import { check } from "proof";
import type { Resource } from "rustscript:types";
type Dispatcher = (operation: number) => number;
function makeDispatcher(): Dispatcher {
    let owner: Resource<"compat.number_array"> = create([10]);
    const dispatcher: Dispatcher = (operation: number): number => {
        const other: Resource<"compat.number_array"> = create([20]);
        return operation === 0
            ? (dispatcher === dispatcher ? check(other) + length(owner) : check(other) + length(owner))
            : close(owner);
    };
    return dispatcher;
}
function main(): number {
    const dispatcher = makeDispatcher();
    return dispatcher(0);
}
main();
"#;

#[test]
fn numeric_signature_wrong_resource_reaches_typed_owner_comparison() {
    let (compiled, catalog) = fixture_program(NUMERIC_WRONG_RESOURCE_SOURCE);
    let mut machine = Vm::new(compiled.program);
    owner_registry(&catalog)
        .bind_vm_cached(&mut machine)
        .expect("wrong-resource probe imports bind");
    let error = machine
        .run()
        .expect_err("two valid resources must not prove owner association");
    eprintln!("numeric-signature wrong-resource error: {error:?}");
    assert!(
        error
            .to_string()
            .contains("candidate is not the active callable's Move-captured resource"),
        "comparison not reached: {error}"
    );
    assert_eq!(machine.host_context().resource_count(), 2);
    machine
        .reset_for_reuse()
        .expect("wrong-resource rejection reset");
    assert_eq!(machine.host_context().resource_count(), 0);
}

#[test]
fn resource_signature_self_copy_without_owner_keeps_core_rejection() {
    let source = r#"
import { create, length } from "arrays";
import type { Resource } from "rustscript:types";
type Reader = (candidate: Resource<"compat.number_array">) => number;
function main(): number {
    const reader: Reader = (candidate: Resource<"compat.number_array">): number =>
        reader === reader ? length(candidate) : length(candidate);
    const candidate: Resource<"compat.number_array"> = create([1]);
    return reader(candidate);
}
main();
"#;
    let catalog = owner_catalog();
    let error = match vm::compile_source_with_flavor_and_options(
        source,
        crate::typescript_flavor(),
        crate::compile_options_with_catalog(catalog),
    ) {
        Ok(_) => panic!("resource-containing callable self Copy must remain rejected"),
        Err(error) => error,
    };
    eprintln!("resource-signature self Copy without owner: {error:?}");
    assert!(format!("{error:?}").contains("E_OWNERSHIP_COPY_RESOURCE"));
}

#[test]
fn resource_signature_move_owner_without_self_executes() {
    let source = r#"
import { create, length, close } from "arrays";
import type { Resource } from "rustscript:types";
type Reader = (candidate: Resource<"compat.number_array">) => number;
function makeReader(): Reader {
    let owner: Resource<"compat.number_array"> = create([1]);
    const reader: Reader = (candidate: Resource<"compat.number_array">): number => length(candidate) + close(owner);
    return reader;
}
function main(): number {
    const reader = makeReader();
    const candidate: Resource<"compat.number_array"> = create([2]);
    return reader(candidate);
}
main();
"#;
    let (compiled, catalog) = fixture_program(source);
    assert!(
        compiled
            .program
            .callable_prototypes
            .iter()
            .any(|p| p.self_slot.is_none() && p.capture_modes == [CaptureBindingMode::Move])
    );
    let mut machine = Vm::new(compiled.program);
    owner_registry(&catalog)
        .bind_vm_cached(&mut machine)
        .expect("Resource-signature imports bind");
    machine
        .run()
        .expect("Resource signature with Move owner and no self must execute");
    assert_eq!(machine.stack().last(), Some(&Value::Int(2)));
    assert_eq!(machine.host_context().resource_count(), 1);
    machine.reset_for_reuse().expect("Resource signature reset");
    assert_eq!(machine.host_context().resource_count(), 0);
}

#[test]
fn native_resource_signature_self_copy_and_move_owner_controls() {
    let catalog = owner_catalog();
    let self_source = r#"
use arrays;
let reader: fn(resource<compat.number_array>) -> number = |candidate| if reader == reader => {
    arrays::length(candidate)
} else => { arrays::length(candidate) };
let candidate: resource<compat.number_array> = arrays::create([1]);
reader(candidate);
"#;
    let error = match vm::compile_source_with_flavor_and_options(
        self_source,
        vm::compiler::SourceFlavor::RustScript,
        crate::compile_options_with_catalog(Arc::clone(&catalog)),
    ) {
        Ok(_) => panic!("native resource-containing self Copy must remain rejected"),
        Err(error) => error,
    };
    eprintln!("native resource-signature self Copy: {error:?}");
    assert!(format!("{error:?}").contains("E_OWNERSHIP_COPY_RESOURCE"));

    let move_source = r#"
use arrays;
let owner: resource<compat.number_array> = arrays::create([1]);
let reader: fn(resource<compat.number_array>) -> number = |candidate| arrays::length(candidate) + arrays::close(owner);
let candidate: resource<compat.number_array> = arrays::create([2]);
reader(candidate);
"#;
    let compiled = vm::compile_source_with_flavor_and_options(
        move_source,
        vm::compiler::SourceFlavor::RustScript,
        crate::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("native Resource signature and Move owner without self should compile");
    assert!(
        compiled
            .program
            .callable_prototypes
            .iter()
            .any(|p| p.self_slot.is_none() && p.capture_modes == [CaptureBindingMode::Move])
    );
    let mut machine = Vm::new(compiled.program);
    owner_registry(&catalog)
        .bind_vm_cached(&mut machine)
        .expect("native imports bind");
    machine
        .run()
        .expect("native Resource signature and owner Move should execute");
    assert_eq!(machine.stack().last(), Some(&Value::Int(2)));
    assert_eq!(machine.host_context().resource_count(), 1);
    machine
        .reset_for_reuse()
        .expect("native owner control reset");
    assert_eq!(machine.host_context().resource_count(), 0);
}
