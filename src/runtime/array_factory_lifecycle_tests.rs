use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QuotaKind {
    Pending,
    Metadata,
    Payload,
}

struct ScopeOwners {
    entries: Vec<(ResourceHandle, ResourceCaptureRoot, usize)>,
    cursor: usize,
    max_entries: usize,
    max_pending: usize,
    pending: std::collections::HashMap<ResourceHandle, usize>,
    max_payload_bytes: usize,
    payload_bytes: usize,
    last_scanned: usize,
    total_scanned: usize,
    guest_iterations: usize,
    quota_refusal: Option<QuotaKind>,
    lookup_failure: Option<vm::resource::ResourceErrorCode>,
}

#[test]
fn reserved_payload_byte_quota_refuses_before_resource_insertion() {
    let mut machine =
        scope_factory_vm("import { create } from 'arrays'; const owner = create([1, 2]); 0;");
    machine
        .host_context()
        .scope_state_or_insert_with(ScopeOwners::default)
        .unwrap()
        .max_payload_bytes = std::mem::size_of::<Value>();
    assert!(
        machine.run().is_err(),
        "reserved payload capacity must obey the independent byte quota"
    );
    assert_eq!(machine.host_context().resource_count(), 0);
    let context = machine.host_context();
    let owners = context.scope_state::<ScopeOwners>().unwrap();
    assert_eq!(owners.quota_refusal, Some(QuotaKind::Payload));
    assert!(owners.pending.is_empty());
}

#[test]
fn pending_quota_refuses_unregistered_allocations() {
    let mut machine = scope_factory_vm(
        "import { create } from 'arrays'; const first = create([1]); const second = create([2]); 0;",
    );
    machine
        .host_context()
        .scope_state_or_insert_with(ScopeOwners::default)
        .unwrap()
        .max_pending = 1;
    assert!(
        machine.run().is_err(),
        "unregistered factory allocations must obey a separate pending quota"
    );
    assert_eq!(machine.host_context().resource_count(), 1);
    assert_eq!(
        machine
            .host_context()
            .scope_state::<ScopeOwners>()
            .unwrap()
            .quota_refusal,
        Some(QuotaKind::Pending)
    );
    machine.reset_for_reuse().unwrap();
    assert_eq!(machine.host_context().resource_count(), 0);
}

impl Default for ScopeOwners {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            cursor: 0,
            max_entries: 64,
            max_pending: 8,
            pending: Default::default(),
            max_payload_bytes: 1 << 20,
            payload_bytes: 0,
            last_scanned: 0,
            total_scanned: 0,
            guest_iterations: 0,
            quota_refusal: None,
            lookup_failure: None,
        }
    }
}

#[test]
fn registered_metadata_quota_refuses_a_second_factory() {
    let source = FILE_CLI_NATIVE_BOUNDARY_SOURCE
        .replace("const result = alias(0);\nreader(1);\nresult;", "0;")
        .replace(
            "    return reader;",
            "    const registration = reader(0);\n    return reader;",
        )
        .replace(
            "const alias = reader;",
            "const second = makeReader(); const alias = reader;",
        );
    let mut machine = scope_factory_vm(&source);
    machine
        .host_context()
        .scope_state_or_insert_with(ScopeOwners::default)
        .unwrap()
        .max_entries = 1;
    let result = machine.run();
    assert!(
        result.is_err(),
        "second factory must be refused by the independent registered-metadata quota"
    );
    assert_eq!(
        machine
            .host_context()
            .scope_state::<ScopeOwners>()
            .unwrap()
            .quota_refusal,
        Some(QuotaKind::Metadata)
    );
    assert_eq!(
        machine
            .host_context()
            .scope_state::<ScopeOwners>()
            .unwrap()
            .entries
            .len(),
        1
    );
    machine.reset_for_reuse().unwrap();
    assert_eq!(machine.host_context().resource_count(), 0);
}

fn collect_scope_owners(machine: &mut Vm) -> VmResult<usize> {
    let mut context = machine.host_context();
    let mut owners = context
        .take_scope_state::<ScopeOwners>()
        .unwrap_or_default();
    let result = (|| {
        let budget = owners.entries.len().min(4);
        let mut closed = 0;
        owners.last_scanned = 0;
        for _ in 0..budget {
            if owners.entries.is_empty() {
                break;
            }
            owners.cursor %= owners.entries.len();
            owners.last_scanned += 1;
            owners.total_scanned += 1;
            let (handle, root, bytes) = owners.entries[owners.cursor].clone();
            match context.typed_resource_with_key::<NumberArray>(handle, &array_key()) {
                Ok(_) => {}
                Err(error) => {
                    if let vm::HostContextErrorKind::Resource(resource_error) = error.kind() {
                        owners.lookup_failure = Some(resource_error.code());
                        match resource_error.code() {
                            // This private directory admits only the exact NumberArray
                            // with unchanged synchronous Ready close/default typed take.
                            // AlreadyClosed is NOT a destruction proof for general resources.
                            vm::resource::ResourceErrorCode::ResourceAlreadyClosed
                            | vm::resource::ResourceErrorCode::ResourceStale => {
                                owners.entries.swap_remove(owners.cursor);
                                owners.payload_bytes -= bytes;
                                continue;
                            }
                            vm::resource::ResourceErrorCode::ResourceAccessConflict => {
                                owners.cursor += 1;
                                continue;
                            }
                            _ => {}
                        }
                    }
                    return Err(VmError::HostError(error.to_string()));
                }
            }
            if root.observe(&context) == CaptureRootState::Released {
                context
                    .typed_resource_with_key::<NumberArray>(handle, &array_key())
                    .map_err(|error| VmError::HostError(error.to_string()))?;
                let progress = context
                    .close_resource::<NumberArray>(
                        handle,
                        vm::resource::ResourceCloseReason::Requested,
                    )
                    .map_err(|error| VmError::HostError(error.to_string()))?;
                if progress == vm::resource::CloseProgress::Ready {
                    owners.entries.swap_remove(owners.cursor);
                    owners.payload_bytes -= bytes;
                    closed += 1;
                    continue;
                }
            }
            owners.cursor += 1;
        }
        Ok(closed)
    })();
    context
        .scope_state_or_insert_with(|| owners)
        .map_err(|error| VmError::HostError(error.to_string()))?;
    result
}

struct ScopeActive;
impl HostOwnedFunction for ScopeActive {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> VmResult<CallOutcome> {
        let result = CertifiedOwnerProbe.call(call)?;
        if collect_scope_owners(call.vm())? != 0 || call.vm().host_context().resource_count() != 1 {
            return Err(VmError::HostError(
                "active capture cell was reclaimed".into(),
            ));
        }
        Ok(result)
    }
}

struct ScopeMore;
impl HostOwnedFunction for ScopeMore {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> VmResult<CallOutcome> {
        let mut context = call.vm().host_context();
        let owners = context
            .scope_state_or_insert_with(ScopeOwners::default)
            .map_err(|error| VmError::HostError(error.to_string()))?;
        let proceed = owners.guest_iterations < 2048;
        if proceed {
            owners.guest_iterations += 1;
        }
        Ok(CallOutcome::Return(CallReturn::one(Value::Bool(proceed))))
    }
}

struct ScopeCollect;
impl HostOwnedFunction for ScopeCollect {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> VmResult<CallOutcome> {
        let closed = collect_scope_owners(call.vm())?;
        Ok(CallOutcome::Return(CallReturn::one(Value::Int(
            i64::try_from(closed).map_err(|_| VmError::TypeMismatch("factory collection count"))?,
        ))))
    }
}

struct ScopeAllocate;
impl HostOwnedFunction for ScopeAllocate {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> VmResult<CallOutcome> {
        let values = match call.arg(0) {
            Some(Value::Array(values)) => values,
            _ => return Err(VmError::TypeMismatch("factory numeric array")),
        };
        let values: Vec<Value> = values.iter().cloned().collect();
        let bytes = values
            .capacity()
            .checked_mul(std::mem::size_of::<Value>())
            .ok_or(VmError::TypeMismatch("factory payload capacity"))?;
        let mut context = call.vm().host_context();
        {
            let owners = context
                .scope_state_or_insert_with(ScopeOwners::default)
                .map_err(|error| VmError::HostError(error.to_string()))?;
            if owners.pending.len() >= owners.max_pending {
                owners.quota_refusal = Some(QuotaKind::Pending);
                return Err(VmError::HostError("factory pending quota exceeded".into()));
            }
            if owners
                .payload_bytes
                .checked_add(bytes)
                .is_none_or(|total| total > owners.max_payload_bytes)
            {
                owners.quota_refusal = Some(QuotaKind::Payload);
                return Err(VmError::HostError("factory payload quota exceeded".into()));
            }
        }
        let token = context
            .push_resource(NumberArray(values))
            .map_err(|error| VmError::HostError(error.to_string()))?;
        let owners = context
            .scope_state_mut::<ScopeOwners>()
            .ok_or(VmError::TypeMismatch("factory scope state"))?;
        owners.pending.insert(token.handle(), bytes);
        owners.payload_bytes += bytes;
        Ok(CallOutcome::Return(CallReturn::one(Value::Int(
            i64::try_from(token.handle().raw())
                .map_err(|_| VmError::TypeMismatch("factory resource handle"))?,
        ))))
    }
}

struct ScopeRegister;
impl HostOwnedFunction for ScopeRegister {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> VmResult<CallOutcome> {
        let candidate = match call.arg(0) {
            Some(Value::Int(raw)) => ResourceHandle::from_raw(
                u64::try_from(*raw).map_err(|_| VmError::TypeMismatch("factory typed resource"))?,
            )
            .map_err(|error| VmError::HostError(error.to_string()))?,
            _ => return Err(VmError::TypeMismatch("factory typed resource")),
        };
        let (handle, root) = {
            let context = call.vm().host_context();
            context
                .typed_resource_with_key::<NumberArray>(candidate, &array_key())
                .map_err(|error| VmError::HostError(error.to_string()))?;
            let capture = context
                .checked_move_resource_capture::<NumberArray>(&array_key())
                .map_err(|error| VmError::HostError(error.to_string()))?;
            let root = capture.root().clone();
            if capture.token().handle() != candidate
                || root.observe(&context) != CaptureRootState::Live
            {
                return Err(VmError::HostError(
                    "factory registration owner mismatch".into(),
                ));
            }
            (capture.token().handle(), root)
        };
        let mut context = call.vm().host_context();
        let owners = context
            .scope_state_or_insert_with(ScopeOwners::default)
            .map_err(|error| VmError::HostError(error.to_string()))?;
        if owners.entries.len() >= owners.max_entries {
            owners.quota_refusal = Some(QuotaKind::Metadata);
            return Err(VmError::HostError("factory metadata quota exceeded".into()));
        }
        if !owners.pending.contains_key(&handle) {
            return Err(VmError::HostError(
                "factory owner is not a pending allocation".into(),
            ));
        }
        let bytes = owners
            .pending
            .remove(&handle)
            .ok_or_else(|| VmError::HostError("factory pending allocation missing".into()))?;
        owners.entries.push((handle, root, bytes));
        Ok(CallOutcome::Return(CallReturn::one(Value::Int(1))))
    }
}

fn scope_factory_vm(source: &str) -> Vm {
    let base = owner_catalog();
    let mut builder = HostApiCatalog::builder();
    for resource in base.resources() {
        builder.resource(resource.clone());
    }
    for function in base.functions() {
        builder.function(function.clone());
    }
    builder.function(HostFunctionSchema::with_return(
        "factory::allocate",
        vec![HostParamSchema::value(
            "initial",
            HostTypeSchema::Array(Box::new(HostTypeSchema::Number)),
        )],
        HostTypeSchema::Resource(array_key()),
    ));
    builder.function(HostFunctionSchema::with_return(
        "factory::register",
        vec![HostParamSchema::with_passing(
            "candidate",
            HostTypeSchema::Resource(array_key()),
            HostParamPassing::Borrow,
        )],
        HostTypeSchema::Number,
    ));
    builder.function(HostFunctionSchema::with_return(
        "factory::collect",
        vec![],
        HostTypeSchema::Number,
    ));
    builder.function(HostFunctionSchema::with_return(
        "factory::more",
        vec![],
        HostTypeSchema::Bool,
    ));
    builder.function(HostFunctionSchema::with_return(
        "factory::active",
        vec![HostParamSchema::with_passing(
            "candidate",
            HostTypeSchema::Resource(array_key()),
            HostParamPassing::Borrow,
        )],
        HostTypeSchema::Number,
    ));
    let catalog = Arc::new(builder.build().unwrap());
    let source = source
        .replace(
            "import { create, close } from \"arrays\";",
            "import { close } from \"arrays\"; import { allocate as create } from \"factory\";",
        )
        .replace(
            "import { create } from 'arrays';",
            "import { allocate as create } from 'factory';",
        )
        .replace(
            "import { check } from \"proof\";",
            "import { register as check } from \"factory\";",
        );
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        &source,
        crate::typescript_flavor(),
        crate::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .unwrap();
    let mut machine = compilation.into_vm().unwrap();
    let mut registry = array_registry(&catalog);
    registry
        .register_exact_owned(
            "proof::check",
            1,
            vm::catalog_import_schemas(&catalog, "proof::check")
                .into_iter()
                .next()
                .unwrap(),
            |_| Box::new(ScopeRegister),
        )
        .unwrap();
    registry.authorize_registered_builtin_import("proof::check");
    registry
        .register_exact_owned(
            "factory::allocate",
            1,
            vm::catalog_import_schemas(&catalog, "factory::allocate")
                .into_iter()
                .next()
                .unwrap(),
            |_| Box::new(ScopeAllocate),
        )
        .unwrap();
    registry
        .register_exact_owned(
            "factory::register",
            1,
            vm::catalog_import_schemas(&catalog, "factory::register")
                .into_iter()
                .next()
                .unwrap(),
            |_| Box::new(ScopeRegister),
        )
        .unwrap();
    registry.authorize_registered_builtin_import("factory::allocate");
    registry.authorize_registered_builtin_import("factory::register");
    registry
        .register_exact_owned(
            "factory::collect",
            0,
            vm::catalog_import_schemas(&catalog, "factory::collect")
                .into_iter()
                .next()
                .unwrap(),
            |_| Box::new(ScopeCollect),
        )
        .unwrap();
    registry.authorize_registered_builtin_import("factory::collect");
    registry
        .register_exact_owned(
            "factory::more",
            0,
            vm::catalog_import_schemas(&catalog, "factory::more")
                .into_iter()
                .next()
                .unwrap(),
            |_| Box::new(ScopeMore),
        )
        .unwrap();
    registry.authorize_registered_builtin_import("factory::more");
    registry
        .register_exact_owned(
            "factory::active",
            1,
            vm::catalog_import_schemas(&catalog, "factory::active")
                .into_iter()
                .next()
                .unwrap(),
            |_| Box::new(ScopeActive),
        )
        .unwrap();
    registry.authorize_registered_builtin_import("factory::active");
    registry.bind_vm_cached(&mut machine).unwrap();
    machine
}

#[test]
fn factory_registration_is_recorded_in_scope_state() {
    let source = FILE_CLI_NATIVE_BOUNDARY_SOURCE
        .replace("const result = alias(0);\nreader(1);\nresult;", "0;")
        .replace(
            "    return reader;",
            "    const registration = reader(0);\n    return reader;",
        );
    let mut machine = scope_factory_vm(&source);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(machine.host_context().resource_count(), 1);
    assert_eq!(
        machine
            .host_context()
            .scope_state::<ScopeOwners>()
            .map(|state| state.entries.len()),
        Some(1),
        "factory registration must create scope-local weak-root ownership state"
    );
}

#[test]
fn released_registered_owner_is_reclaimed_at_safety_point() {
    let source = FILE_CLI_NATIVE_BOUNDARY_SOURCE
        .replace("const result = alias(0);\nreader(1);\nresult;", "0;")
        .replace(
            "    return reader;",
            "    const registration = reader(0);\n    return reader;",
        );
    let mut machine = scope_factory_vm(&source);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    {
        let context = machine.host_context();
        let root = &context.scope_state::<ScopeOwners>().unwrap().entries[0].1;
        assert_eq!(root.observe(&context), CaptureRootState::Released);
    }
    assert_eq!(collect_scope_owners(&mut machine).unwrap(), 1);
    assert_eq!(
        machine.host_context().resource_count(),
        0,
        "released registered payload must be reclaimed at the explicit safety point"
    );
}

fn registered_source(tail: &str) -> String {
    let split = FILE_CLI_NATIVE_BOUNDARY_SOURCE
        .find("\nconst reader = makeReader();")
        .unwrap();
    format!(
        "{}\n{tail}\n",
        FILE_CLI_NATIVE_BOUNDARY_SOURCE[..split].replace(
            "    return reader;",
            "    const registration = reader(0);\n    return reader;"
        )
    )
}

#[test]
fn scope_registered_factory_is_native_and_released_without_owner_retention() {
    let mut machine = scope_factory_vm(&registered_source("const abandoned = makeReader(); 0;"));
    machine.compile_aot().unwrap();
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert!(machine.aot_exec_count() > 1);
    assert!(machine.dump_aot_info().contains("lowering=ssa"));
    assert_eq!(
        machine
            .host_context()
            .scope_state::<ScopeOwners>()
            .unwrap()
            .pending
            .len(),
        0
    );
    assert_eq!(collect_scope_owners(&mut machine).unwrap(), 1);
    assert_eq!(machine.host_context().resource_count(), 0);
    assert_eq!(
        machine
            .host_context()
            .scope_state::<ScopeOwners>()
            .unwrap()
            .payload_bytes,
        0
    );
}

#[test]
fn live_aliases_retain_payload_and_collection_does_not_close_live_roots() {
    let mut machine = scope_factory_vm(&registered_source(
        "const reader = makeReader(); const alias = reader; alias;",
    ));
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    let held = machine.stack().last().unwrap().clone();
    {
        let context = machine.host_context();
        assert_eq!(
            context.scope_state::<ScopeOwners>().unwrap().entries[0]
                .1
                .observe(&context),
            CaptureRootState::Live
        );
    }
    assert_eq!(collect_scope_owners(&mut machine).unwrap(), 0);
    assert_eq!(machine.host_context().resource_count(), 1);
    drop(held);
    machine.reset_for_reuse().unwrap();
    assert!(
        machine
            .host_context()
            .scope_state::<ScopeOwners>()
            .is_none()
    );
}

#[test]
fn epoch_revocation_keeps_an_open_payload_and_scope_accounting() {
    let mut machine = scope_factory_vm(&registered_source("const abandoned = makeReader(); 0;"));
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    let (handle, root, bytes) = machine
        .host_context()
        .scope_state::<ScopeOwners>()
        .unwrap()
        .entries[0]
        .clone();
    machine.set_local(0, Value::Int(0)).unwrap();
    assert_eq!(
        root.observe(&machine.host_context()),
        CaptureRootState::Unavailable
    );
    assert_eq!(collect_scope_owners(&mut machine).unwrap(), 0);
    let context = machine.host_context();
    let token = context
        .typed_resource_with_key::<NumberArray>(handle, &array_key())
        .unwrap();
    assert_eq!(context.resource(&token).unwrap().get().0, [Value::Int(10)]);
    assert_eq!(
        context.scope_state::<ScopeOwners>().unwrap().payload_bytes,
        bytes
    );
    machine.reset_for_reuse().unwrap();
    assert_eq!(machine.host_context().resource_count(), 0);
}

#[test]
fn four_entry_scan_budget_progresses_after_swap_removal() {
    let tail = (0..9)
        .map(|index| format!("const a{index} = makeReader();"))
        .collect::<String>()
        + "0;";
    let mut machine = scope_factory_vm(&registered_source(&tail));
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    for remaining in [5, 1, 0] {
        collect_scope_owners(&mut machine).unwrap();
        let context = machine.host_context();
        let owners = context.scope_state::<ScopeOwners>().unwrap();
        assert!(owners.last_scanned <= 4);
        assert_eq!(owners.entries.len(), remaining);
        assert_eq!(context.resource_count(), remaining);
    }
    assert_eq!(
        machine
            .host_context()
            .scope_state::<ScopeOwners>()
            .unwrap()
            .total_scanned,
        9
    );
}

#[test]
fn duplicate_registration_refuses_without_closing_the_owner() {
    let source = registered_source("const reader = makeReader(); reader(0);");
    let mut machine = scope_factory_vm(&source);
    assert!(machine.run().is_err());
    let context = machine.host_context();
    assert_eq!(context.resource_count(), 1);
    assert_eq!(
        context.scope_state::<ScopeOwners>().unwrap().entries.len(),
        1
    );
    assert!(
        context
            .scope_state::<ScopeOwners>()
            .unwrap()
            .pending
            .is_empty()
    );
}

#[test]
fn a_wrong_typed_candidate_cannot_register_a_different_capture_owner() {
    let source = registered_source("const abandoned = makeReader(); 0;").replace("operation === 0 ? check(owner) : close(owner)", "{ const other: Resource<\"compat.number_array\"> = create([11]); return operation === 0 ? check(other) : close(owner); }");
    let mut machine = scope_factory_vm(&source);
    assert!(machine.run().is_err());
    let context = machine.host_context();
    assert_eq!(context.resource_count(), 2);
    assert!(
        context
            .scope_state::<ScopeOwners>()
            .unwrap()
            .entries
            .is_empty()
    );
    assert_eq!(
        context.scope_state::<ScopeOwners>().unwrap().pending.len(),
        2
    );
}

#[test]
fn independently_closed_number_array_retires_weak_metadata_and_bytes() {
    let mut machine = scope_factory_vm(&registered_source(
        "const reader = makeReader(); reader(1); 0;",
    ));
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(machine.host_context().resource_count(), 0);
    collect_scope_owners(&mut machine).unwrap();
    let context = machine.host_context();
    let owners = context.scope_state::<ScopeOwners>().unwrap();
    assert!(
        owners.entries.is_empty(),
        "a separately completed typed take must retire synchronous NumberArray metadata"
    );
    assert_eq!(owners.payload_bytes, 0);
}

#[test]
fn ordinary_manual_arrays_are_never_opportunistically_collected() {
    let mut machine = certified_vm(
        "import { create } from 'arrays'; const manual = create([10]); 0;",
        ProbePlan::MatchOwner,
    );
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(collect_scope_owners(&mut machine).unwrap(), 0);
    assert_eq!(machine.host_context().resource_count(), 1);
    machine.reset_for_reuse().unwrap();
    assert_eq!(machine.host_context().resource_count(), 0);
}

#[test]
fn one_vm_2048_factories_keep_scope_metadata_payload_and_probe_work_bounded() {
    {
        let native = false;
        let mut machine = scope_factory_vm(&registered_source("makeReader;"));
        if native {
            machine.compile_aot().unwrap();
        }
        assert_eq!(machine.run().unwrap(), VmStatus::Halted);
        let factory = machine.stack().last().unwrap().clone();
        let mut max_entries = 0;
        let mut max_capacity = 0;
        for _ in 0..2048 {
            let reader = machine.invoke_callable(factory.clone(), &[]).unwrap();
            assert_eq!(machine.host_context().resource_count(), 1);
            drop(reader);
            assert_eq!(collect_scope_owners(&mut machine).unwrap(), 1);
            let context = machine.host_context();
            let owners = context.scope_state::<ScopeOwners>().unwrap();
            assert!(owners.entries.is_empty() && owners.pending.is_empty());
            assert_eq!(owners.payload_bytes, 0);
            assert!(owners.last_scanned <= 4);
            assert!(owners.entries.capacity() <= 64);
            assert_eq!(context.resource_count(), 0);
            let stats = machine.capture_certification_stats().unwrap();
            max_entries = max_entries.max(stats.entries);
            max_capacity = max_capacity.max(stats.capacity);
            assert!(stats.probes <= stats.registrations * 4);
            assert!(stats.entries <= 12 && stats.capacity <= 16);
        }
        eprintln!(
            "A4 one-VM history: native={native} iterations=2048 max_capture_entries={max_entries} max_capture_capacity={max_capacity} stats={:?} inspected={}",
            machine.capture_certification_stats(),
            machine
                .host_context()
                .scope_state::<ScopeOwners>()
                .unwrap()
                .total_scanned
        );
        assert_eq!(
            machine
                .host_context()
                .scope_state::<ScopeOwners>()
                .unwrap()
                .total_scanned,
            2048
        );
        if native {
            assert!(machine.aot_exec_count() > 1);
        }
    }
}

#[test]
fn native_guest_2048_factory_cycles_have_actual_native_and_bounded_scan_evidence() {
    let source = registered_source(
        "import { collect, more } from 'factory'; while (more()) { const abandoned = makeReader(); const swept = collect(); } 0;",
    );
    let mut machine = scope_factory_vm(&source);
    eprintln!("A4 native guest history before compile");
    machine.compile_aot().unwrap();
    eprintln!("A4 native guest history compiled, before run");
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    eprintln!("A4 native guest history after run");
    assert!(machine.aot_exec_count() > 1);
    collect_scope_owners(&mut machine).unwrap();
    let context = machine.host_context();
    let owners = context.scope_state::<ScopeOwners>().unwrap();
    assert!(owners.pending.is_empty() && owners.entries.is_empty());
    assert_eq!(owners.payload_bytes, 0);
    assert_eq!(context.resource_count(), 0);
    assert!(owners.total_scanned >= 2048 && owners.total_scanned <= 2048 * 4 + 4);
    assert_eq!(owners.guest_iterations, 2048);
    let inspected = owners.total_scanned;
    let stats = machine.capture_certification_stats().unwrap();
    assert!(stats.entries <= 12 && stats.capacity <= 16 && stats.probes <= stats.registrations * 4);
    eprintln!(
        "A4 actual native guest history: native_count={} inspected={inspected} stats={stats:?}",
        machine.aot_exec_count()
    );
}

#[test]
fn stale_generation_metadata_never_closes_the_reused_manual_slot() {
    let mut machine = scope_factory_vm(&registered_source(
        "const reader = makeReader(); reader(1); 0;",
    ));
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    let old = machine
        .host_context()
        .scope_state::<ScopeOwners>()
        .unwrap()
        .entries[0]
        .0;
    let replacement = machine
        .host_context()
        .push_resource(NumberArray(vec![Value::Int(99)]))
        .unwrap();
    let error = machine
        .host_context()
        .typed_resource_with_key::<NumberArray>(old, &array_key())
        .unwrap_err();
    assert!(
        matches!(error.kind(), vm::HostContextErrorKind::Resource(resource_error) if resource_error.code() == vm::resource::ResourceErrorCode::ResourceStale)
    );
    collect_scope_owners(&mut machine).unwrap();
    let context = machine.host_context();
    assert!(
        context
            .scope_state::<ScopeOwners>()
            .unwrap()
            .entries
            .is_empty()
    );
    assert_eq!(
        context.scope_state::<ScopeOwners>().unwrap().payload_bytes,
        0
    );
    assert_eq!(
        context.resource(&replacement).unwrap().get().0,
        [Value::Int(99)]
    );
    assert_eq!(context.resource_count(), 1);
}

#[test]
fn a_foreign_arena_lookup_failure_retains_the_local_payload_and_metadata() {
    let mut machine = scope_factory_vm(&registered_source("const abandoned = makeReader(); 0;"));
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    let mut foreign = scope_factory_vm("0;");
    let token = foreign
        .host_context()
        .push_resource(NumberArray(vec![Value::Int(88)]))
        .unwrap();
    let original = machine
        .host_context()
        .scope_state::<ScopeOwners>()
        .unwrap()
        .entries[0]
        .0;
    machine
        .host_context()
        .scope_state_mut::<ScopeOwners>()
        .unwrap()
        .entries[0]
        .0 = token.handle();
    assert!(collect_scope_owners(&mut machine).is_err());
    let context = machine.host_context();
    assert_eq!(
        context.scope_state::<ScopeOwners>().unwrap().entries.len(),
        1
    );
    assert!(context.scope_state::<ScopeOwners>().unwrap().payload_bytes > 0);
    assert!(
        context
            .typed_resource_with_key::<NumberArray>(original, &array_key())
            .is_ok()
    );
    assert_eq!(context.resource_count(), 1);
    assert_eq!(foreign.host_context().resource_count(), 1);
}

#[test]
fn capacity_growth_refusal_preserves_payload_buffer_and_byte_accounting() {
    let mut machine = scope_factory_vm(&registered_source("const reader = makeReader(); reader;"));
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    let handle = machine
        .host_context()
        .scope_state::<ScopeOwners>()
        .unwrap()
        .entries[0]
        .0;
    let before = machine
        .host_context()
        .scope_state::<ScopeOwners>()
        .unwrap()
        .payload_bytes;
    machine
        .host_context()
        .scope_state_mut::<ScopeOwners>()
        .unwrap()
        .max_payload_bytes = before;
    assert!(resize_registered_payload(&mut machine, 0, 2).is_err());
    let context = machine.host_context();
    let token = context
        .typed_resource_with_key::<NumberArray>(handle, &array_key())
        .unwrap();
    assert_eq!(
        context.resource(&token).unwrap().get().0,
        [Value::Int(10)],
        "over-quota protocol growth must leave the previous payload intact"
    );
    assert_eq!(
        context.scope_state::<ScopeOwners>().unwrap().payload_bytes,
        before
    );
}

fn resize_registered_payload(machine: &mut Vm, index: usize, new_len: usize) -> VmResult<()> {
    let mut context = machine.host_context();
    let (handle, old_bytes, total, limit) = {
        let owners = context
            .scope_state::<ScopeOwners>()
            .ok_or(VmError::TypeMismatch("factory scope state"))?;
        let entry = owners
            .entries
            .get(index)
            .ok_or(VmError::TypeMismatch("factory registered index"))?;
        (
            entry.0,
            entry.2,
            owners.payload_bytes,
            owners.max_payload_bytes,
        )
    };
    let requested = new_len
        .checked_mul(std::mem::size_of::<Value>())
        .ok_or(VmError::TypeMismatch("factory growth size"))?;
    let other_bytes = total
        .checked_sub(old_bytes)
        .ok_or(VmError::TypeMismatch("factory byte accounting"))?;
    if other_bytes
        .checked_add(requested)
        .is_none_or(|bytes| bytes > limit)
    {
        return Err(VmError::HostError("factory payload quota exceeded".into()));
    }
    let token = context
        .typed_resource_with_key::<NumberArray>(handle, &array_key())
        .map_err(|error| VmError::HostError(error.to_string()))?;
    let mut replacement = Vec::new();
    replacement
        .try_reserve_exact(new_len)
        .map_err(|_| VmError::HostError("factory growth allocation refused".into()))?;
    let reserved = replacement
        .capacity()
        .checked_mul(std::mem::size_of::<Value>())
        .ok_or(VmError::TypeMismatch("factory growth capacity"))?;
    let new_total = other_bytes
        .checked_add(reserved)
        .filter(|bytes| *bytes <= limit)
        .ok_or_else(|| VmError::HostError("factory payload quota exceeded".into()))?;
    {
        let payload = context
            .resource(&token)
            .map_err(|error| VmError::HostError(error.to_string()))?;
        replacement.extend(payload.get().0.iter().take(new_len).cloned());
    }
    replacement.resize(new_len, Value::Int(0));
    context
        .resource_mut(&token)
        .map_err(|error| VmError::HostError(error.to_string()))?
        .get()
        .0 = replacement;
    let owners = context
        .scope_state_mut::<ScopeOwners>()
        .ok_or(VmError::TypeMismatch("factory scope state"))?;
    owners.entries[index].2 = reserved;
    owners.payload_bytes = new_total;
    Ok(())
}

#[test]
fn typed_borrowmut_writes_share_one_buffer_at_two_backing_sizes() {
    for size in [32, 8192] {
        let source = format!("import {{ write }} from 'arrays';\n{}", registered_source("const reader = makeReader(); const alias = reader; alias;").replace("operation === 0 ? check(owner) : close(owner)", "operation === 0 ? check(owner) : operation === 1 ? write(owner, 0, 7) : close(owner)"));
        let mut machine = scope_factory_vm(&source);
        assert_eq!(machine.run().unwrap(), VmStatus::Halted);
        let alias = machine.stack().last().unwrap().clone();
        resize_registered_payload(&mut machine, 0, size).unwrap();
        let handle = machine
            .host_context()
            .scope_state::<ScopeOwners>()
            .unwrap()
            .entries[0]
            .0;
        let (pointer, capacity, accounted) = {
            let context = machine.host_context();
            let token = context
                .typed_resource_with_key::<NumberArray>(handle, &array_key())
                .unwrap();
            let payload = context.resource(&token).unwrap();
            (
                payload.get().0.as_ptr(),
                payload.get().0.capacity(),
                context.scope_state::<ScopeOwners>().unwrap().payload_bytes,
            )
        };
        for _ in 0..1024 {
            assert_eq!(
                machine
                    .invoke_callable(alias.clone(), &[Value::Int(1)])
                    .unwrap(),
                Value::Int(7)
            );
            assert_eq!(collect_scope_owners(&mut machine).unwrap(), 0);
            let context = machine.host_context();
            let token = context
                .typed_resource_with_key::<NumberArray>(handle, &array_key())
                .unwrap();
            let payload = context.resource(&token).unwrap();
            assert_eq!(payload.get().0.as_ptr(), pointer);
            assert_eq!(payload.get().0.capacity(), capacity);
            assert_eq!(payload.get().0.len(), size);
            assert_eq!(payload.get().0[0], Value::Int(7));
            assert!(
                payload.get().0[1..]
                    .iter()
                    .all(|value| *value == Value::Int(0))
            );
            assert_eq!(
                context.scope_state::<ScopeOwners>().unwrap().last_scanned,
                1
            );
            assert_eq!(
                context.scope_state::<ScopeOwners>().unwrap().payload_bytes,
                accounted
            );
        }
        eprintln!(
            "A4 typed BorrowMut writes: backing={size} writes=1024 unchanged_buffer=true unchanged_capacity={capacity} reserved_bytes={accounted} inspected={}",
            machine
                .host_context()
                .scope_state::<ScopeOwners>()
                .unwrap()
                .total_scanned
        );
        drop(alias);
        machine.reset_for_reuse().unwrap();
        assert_eq!(machine.host_context().resource_count(), 0);
        assert!(
            machine
                .host_context()
                .scope_state::<ScopeOwners>()
                .is_none()
        );
    }
}

#[test]
fn a_fault_injected_leaked_borrow_conflict_retains_payload_and_accounting() {
    let mut machine = scope_factory_vm(&registered_source("const abandoned = makeReader(); 0;"));
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    let handle = machine
        .host_context()
        .scope_state::<ScopeOwners>()
        .unwrap()
        .entries[0]
        .0;
    let bytes = machine
        .host_context()
        .scope_state::<ScopeOwners>()
        .unwrap()
        .payload_bytes;
    {
        let mut context = machine.host_context();
        let token = context
            .typed_resource_with_key::<NumberArray>(handle, &array_key())
            .unwrap();
        // Deliberately model a defective host extension that forgets its guard.
        // No reference to the payload escapes this test boundary.
        let guard = context.resource_mut(&token).unwrap();
        std::mem::forget(guard);
    }
    let error = machine
        .host_context()
        .typed_resource_with_key::<NumberArray>(handle, &array_key())
        .unwrap_err();
    assert!(
        matches!(error.kind(), vm::HostContextErrorKind::Resource(error) if error.code() == vm::resource::ResourceErrorCode::ResourceAccessConflict)
    );
    assert_eq!(collect_scope_owners(&mut machine).unwrap(), 0);
    let context = machine.host_context();
    assert_eq!(context.resource_count(), 1);
    assert_eq!(
        context.scope_state::<ScopeOwners>().unwrap().entries.len(),
        1
    );
    assert_eq!(
        context.scope_state::<ScopeOwners>().unwrap().payload_bytes,
        bytes
    );
    // This deliberate guard leak cannot be repaired through the safe SDK.
    // Verify the existing debug leak detector rather than claiming cleanup.
    let drop_outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(machine)));
    if cfg!(debug_assertions) {
        assert!(drop_outcome.is_err());
    }
}

struct DelayedPayload(Arc<std::sync::atomic::AtomicUsize>);
impl HostResource for DelayedPayload {
    fn resource_type_key() -> Option<ResourceTypeKey> {
        Some(array_key())
    }
    fn begin_close(
        &mut self,
        _: vm::resource::ResourceCloseReason,
    ) -> vm::resource::ResourceResult<vm::resource::CloseProgress> {
        Ok(vm::resource::CloseProgress::Pending)
    }
    fn poll_close(
        &mut self,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<vm::resource::ResourceResult<()>> {
        std::task::Poll::Pending
    }
}
impl Drop for DelayedPayload {
    fn drop(&mut self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[test]
fn already_closed_error_for_async_closing_does_not_prove_payload_destruction() {
    let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut machine = scope_factory_vm("0;");
    let token = machine
        .host_context()
        .push_resource(DelayedPayload(Arc::clone(&drops)))
        .unwrap();
    {
        let context = machine.host_context();
        let error = context
            .typed_resource_with_key::<NumberArray>(token.handle(), &array_key())
            .unwrap_err();
        assert!(
            matches!(error.kind(), vm::HostContextErrorKind::Resource(error) if error.code() == vm::resource::ResourceErrorCode::ResourceTypeMismatch)
        );
    }
    assert_eq!(
        machine
            .host_context()
            .close_resource::<DelayedPayload>(
                token.handle(),
                vm::resource::ResourceCloseReason::Requested
            )
            .unwrap(),
        vm::resource::CloseProgress::Pending
    );
    let error = machine
        .host_context()
        .typed_resource_with_key::<DelayedPayload>(token.handle(), &array_key())
        .unwrap_err();
    assert!(
        matches!(error.kind(), vm::HostContextErrorKind::Resource(error) if error.code() == vm::resource::ResourceErrorCode::ResourceAlreadyClosed)
    );
    assert_eq!(machine.host_context().resource_count(), 1);
    assert_eq!(drops.load(std::sync::atomic::Ordering::SeqCst), 0);
    drop(machine);
    assert_eq!(drops.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn terminal_reset_destroys_scope_directory_and_invalidates_weak_old_roots() {
    let mut machine = scope_factory_vm(&registered_source("const reader = makeReader(); reader;"));
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    let root = machine
        .host_context()
        .scope_state::<ScopeOwners>()
        .unwrap()
        .entries[0]
        .1
        .clone();
    assert_eq!(
        root.observe(&machine.host_context()),
        CaptureRootState::Live
    );
    machine.reset_for_reuse().unwrap();
    assert_eq!(machine.host_context().resource_count(), 0);
    assert!(
        machine
            .host_context()
            .scope_state::<ScopeOwners>()
            .is_none()
    );
    assert_eq!(
        root.observe(&machine.host_context()),
        CaptureRootState::Unavailable
    );
}

#[test]
fn expired_environment_with_live_no_self_frame_survives_scope_collection() {
    let source = format!(
        "import {{ active }} from 'factory';\n{}",
        registered_source("0;")
            .replace("function makeReader()", "export function makeReader()")
            .replace(
                "operation === 0 ? check(owner) : close(owner)",
                "operation === 0 ? check(owner) : operation === 1 ? active(owner) : close(owner)"
            )
    );
    let mut machine = scope_factory_vm(&source);
    machine
        .host_context()
        .set_module_state(ProbeState::with_plan(ProbePlan::DropLastAliasInActiveFrame));
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    let factory = machine.resolve_exported_callable("makeReader").unwrap();
    let closure = machine.invoke_callable(factory, &[]).unwrap();
    let Value::Callable(callable) = &closure else {
        panic!("owning dispatcher");
    };
    assert!(
        machine.program().callable_prototypes[callable.prototype_id as usize]
            .self_slot
            .is_none()
    );
    let environment = Arc::downgrade(callable.env.as_ref().unwrap());
    {
        let mut context = machine.host_context();
        let state = context.module_state_mut::<ProbeState>().unwrap();
        state.environment = Some(environment.clone());
        state.held_alias = Some(closure.clone());
    }
    assert_eq!(
        machine.invoke_callable(closure, &[Value::Int(1)]).unwrap(),
        Value::Int(1)
    );
    assert!(environment.upgrade().is_none());
    {
        let context = machine.host_context();
        let state = context.module_state::<ProbeState>().unwrap();
        assert_eq!(state.environment_expired_during_call, [true]);
        assert_eq!(state.root_after_alias_drop, [CaptureRootState::Live]);
        assert_eq!(
            context.scope_state::<ScopeOwners>().unwrap().entries[0]
                .1
                .observe(&context),
            CaptureRootState::Released
        );
        assert_eq!(context.resource_count(), 1);
    }
    assert_eq!(collect_scope_owners(&mut machine).unwrap(), 1);
    assert_eq!(machine.host_context().resource_count(), 0);
}

fn factory_return_registration_control(native: bool) {
    // Retain the established owning factory bytes; omit user operations.
    let source = FILE_CLI_NATIVE_BOUNDARY_SOURCE
        .replace("const result = alias(0);\nreader(1);\nresult;", "0;")
        .replace(
            "    return reader;",
            "    const registration = reader(0);\n    return reader;",
        );
    assert!(source.ends_with("0;\n"));
    let mut machine = certified_vm(&source, ProbePlan::MatchOwner);
    if native {
        machine.compile_aot().unwrap();
    }
    assert_eq!(machine.run().unwrap(), vm::VmStatus::Halted);
    assert_eq!(machine.host_context().resource_count(), 1);
    assert!(machine.capture_certification_stats().unwrap().valid);
    if native {
        assert!(machine.aot_exec_count() > 1);
        assert!(machine.dump_aot_info().contains("lowering=ssa"));
    }
    let context = machine.host_context();
    let observed = context.module_state::<ProbeState>().unwrap();
    eprintln!(
        "factory return without user operations: native={native} registered_owner_observations={} resources=1",
        observed.owner_handles.len()
    );
    assert_eq!(
        observed.owner_handles.len(),
        1,
        "factory must register its completed owning dispatcher before returning, without first user access",
    );
}

#[test]
fn factory_return_is_registered_before_any_user_access_interpreted() {
    factory_return_registration_control(false);
}

#[test]
fn factory_return_is_registered_before_any_user_access_native() {
    factory_return_registration_control(true);
}
