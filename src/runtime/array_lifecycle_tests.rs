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

struct CheckedSelfOwnerAssociation;

impl HostOwnedFunction for CheckedSelfOwnerAssociation {
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
            Box::new(CheckedSelfOwnerAssociation)
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

#[test]
fn self_bearing_move_owner_dispatcher_executes_through_an_alias() {
    let source = r#"
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
