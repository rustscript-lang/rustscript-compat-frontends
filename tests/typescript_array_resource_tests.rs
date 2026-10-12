use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pd_host_function::pd_host_function;
use vm::resource::{CloseProgress, HostResource, ResourceCloseReason, ResourceMut, ResourceRef};
use vm::{
    self, HostApiCatalog, HostFunctionDescriptor, HostFunctionRegistry, HostModuleDescriptor,
    HostParamPassing, HostParamSchema, HostResourceType, HostResourceTypeMeta, HostTypeSchema,
    Resource, ResourceTypeKey, TypeSchema, Value, Vm, VmError, VmResult, VmStatus,
};

static CLOSE_COUNT: AtomicUsize = AtomicUsize::new(0);
static DROP_COUNT: AtomicUsize = AtomicUsize::new(0);
static TEST_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug)]
struct NumberArray(Vec<Value>);

impl Drop for NumberArray {
    fn drop(&mut self) {
        DROP_COUNT.fetch_add(1, Ordering::SeqCst);
    }
}

impl HostResource for NumberArray {
    fn resource_type_key() -> Option<ResourceTypeKey> {
        Some(ResourceTypeKey::new(NumberArray::KEY).expect("valid array resource key"))
    }

    fn begin_close(
        &mut self,
        _reason: ResourceCloseReason,
    ) -> vm::resource::ResourceResult<CloseProgress> {
        CLOSE_COUNT.fetch_add(1, Ordering::SeqCst);
        Ok(CloseProgress::Ready)
    }
}

impl HostResourceType for NumberArray {
    const KEY: &'static str = "compat.number_array";
    const DESCRIPTION: &'static str = "Compat-owned numeric array storage";
}

fn array_key() -> ResourceTypeKey {
    ResourceTypeKey::new(NumberArray::KEY).expect("valid array resource key")
}

fn create_schema() -> vm::HostFunctionSchema {
    vm::HostFunctionSchema::with_return(
        "arrays::create",
        vec![HostParamSchema::value(
            "initial",
            HostTypeSchema::Array(Box::new(HostTypeSchema::Number)),
        )],
        HostTypeSchema::Resource(array_key()),
    )
}

fn read_schema() -> vm::HostFunctionSchema {
    vm::HostFunctionSchema::with_return(
        "arrays::read",
        vec![
            HostParamSchema::with_passing(
                "array",
                HostTypeSchema::Resource(array_key()),
                HostParamPassing::Borrow,
            ),
            HostParamSchema::value("index", HostTypeSchema::Number),
        ],
        HostTypeSchema::Number,
    )
}

fn write_schema() -> vm::HostFunctionSchema {
    vm::HostFunctionSchema::with_return(
        "arrays::write",
        vec![
            HostParamSchema::with_passing(
                "array",
                HostTypeSchema::Resource(array_key()),
                HostParamPassing::BorrowMut,
            ),
            HostParamSchema::value("index", HostTypeSchema::Number),
            HostParamSchema::value("value", HostTypeSchema::Number),
        ],
        HostTypeSchema::Number,
    )
}

fn number_value(value: &Value) -> bool {
    matches!(value, Value::Int(_) | Value::Float(_))
}

fn checked_index(value: &Value, len: usize) -> VmResult<usize> {
    let invalid_index =
        || VmError::HostError("array index must be integral and in bounds".to_owned());
    let index = match value {
        Value::Int(value) => usize::try_from(*value).map_err(|_| invalid_index())?,
        Value::Float(value)
            if value.is_finite()
                && value.fract() == 0.0
                && *value >= 0.0
                && *value < len as f64 =>
        {
            *value as usize
        }
        _ => return Err(invalid_index()),
    };
    if index >= len {
        return Err(invalid_index());
    }
    Ok(index)
}

mod generated_parent {
    use super::*;

    pub use vm::{arg, borrow_arg, take_arg};

    pub mod typed_array_host {
        use super::*;

        /// Create a compat-owned numeric array from a checked TypeScript array.
        #[pd_host_function(name = "arrays::create", contract = super::super::create_schema)]
        fn create(vm: &mut Vm, initial: Value) -> VmResult<Resource<NumberArray>> {
            let Value::Array(values) = initial else {
                return Err(VmError::HostError("expected numeric array".to_owned()));
            };
            if values.iter().any(|value| !number_value(value)) {
                return Err(VmError::HostError("expected numeric array".to_owned()));
            }
            vm.host_context()
                .push_resource(NumberArray(values.iter().cloned().collect()))
                .map_err(|error| VmError::HostError(error.to_string()))
        }

        /// Read one numeric element through a scoped immutable resource borrow.
        #[pd_host_function(name = "arrays::read", contract = super::super::read_schema)]
        fn read(
            #[pd_host_resource(passing = "borrow", key = "compat.number_array")] array: ResourceRef<
                '_,
                NumberArray,
            >,
            index: Value,
        ) -> VmResult<Value> {
            let index = checked_index(&index, array.0.len())?;
            Ok(array.0[index].clone())
        }

        /// Mutate one numeric element through a scoped mutable resource borrow.
        #[pd_host_function(name = "arrays::write", contract = super::super::write_schema)]
        fn write(
            mut array: ResourceMut<'_, NumberArray>,
            index: Value,
            value: Value,
        ) -> VmResult<Value> {
            if !number_value(&value) {
                return Err(VmError::HostError("expected numeric value".to_owned()));
            }
            let index = checked_index(&index, array.0.len())?;
            array.0[index] = value.clone();
            Ok(value)
        }
    }
}

const ARRAY_FUNCTIONS: &[fn() -> HostFunctionDescriptor] = &[
    generated_parent::typed_array_host::create_descriptor,
    generated_parent::typed_array_host::read_descriptor,
    generated_parent::typed_array_host::write_descriptor,
];
const ARRAY_RESOURCES: &[fn() -> HostResourceTypeMeta] = &[HostResourceTypeMeta::of::<NumberArray>];

fn array_module() -> HostModuleDescriptor {
    HostModuleDescriptor {
        name: "compat-arrays",
        functions: ARRAY_FUNCTIONS,
        resources: ARRAY_RESOURCES,
    }
}

fn catalog() -> Arc<HostApiCatalog> {
    Arc::new(array_module().catalog().expect("valid array catalog"))
}

fn typed_array_program(source: &str, catalog: Arc<HostApiCatalog>) -> vm::CompiledProgram {
    vm::compile_source_with_flavor_and_options(
        source,
        pd_vm_compat_frontends::typescript_flavor(),
        pd_vm_compat_frontends::compile_options_with_catalog(catalog),
    )
    .expect("typed array TypeScript should compile against its catalog")
}

fn typed_array_registry(catalog: &HostApiCatalog) -> HostFunctionRegistry {
    let mut registry = HostFunctionRegistry::restricted();
    array_module()
        .install_from_catalog(&mut registry, catalog)
        .expect("descriptors should bind to the exact catalog");
    registry
}

fn production_array_catalog() -> Arc<HostApiCatalog> {
    Arc::new(
        pd_vm_compat_frontends::typescript_array_host_module()
            .catalog()
            .expect("production array module should expose a typed catalog"),
    )
}

fn production_array_program(source: &str, catalog: Arc<HostApiCatalog>) -> vm::CompiledProgram {
    vm::compile_source_with_flavor_and_options(
        source,
        pd_vm_compat_frontends::typescript_flavor(),
        pd_vm_compat_frontends::compile_options_with_catalog(catalog),
    )
    .expect("reference-array source should compile against the production adapter catalog")
}

fn production_array_registry(catalog: &HostApiCatalog) -> HostFunctionRegistry {
    let mut registry = HostFunctionRegistry::restricted();
    pd_vm_compat_frontends::typescript_array_host_module()
        .install_from_catalog(&mut registry, catalog)
        .expect("production array adapters should bind to their exact catalog");
    registry
}

const ARRAY_DISPATCHER_PRELUDE: &str = r#"
import { create, read, write, length, close } from "arrays";
import type { Resource } from "rustscript:types";
type ArrayDispatcher = (operation: number, index: number, value: number) => number;
function makeArray(initial: number[]): ArrayDispatcher {
    let owner: Resource<"compat.number_array"> = create(initial);
    const dispatcher: ArrayDispatcher = (operation: number, index: number, value: number): number =>
        operation === 0 ? length(owner)
        : operation === 1 ? read(owner, index)
        : operation === 2 ? write(owner, index, value)
        : close(owner);
    return dispatcher;
}
function recursiveWrite(array: ArrayDispatcher, value: number, depth: number): number {
    array(2, 0, value);
    return depth > 0 ? recursiveWrite(array, value + 1, depth - 1) : array(1, 0, 0);
}
"#;

fn array_dispatcher_source(main_body: &str) -> String {
    format!("{ARRAY_DISPATCHER_PRELUDE}\nfunction main(): number {{\n{main_body}\n}}\nmain();\n")
}

#[test]
fn typescript_array_resource_factory_storage_conversion_and_reset_are_typed() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    CLOSE_COUNT.store(0, Ordering::SeqCst);
    DROP_COUNT.store(0, Ordering::SeqCst);

    let source = r#"
import { create, read, write } from "arrays";
import type { Resource } from "rustscript:types";
function main(): number {
const leftInput: number[] = [1, 2, 3];
const rightInput: number[] = [100];
let left: Resource<"compat.number_array"> = create(leftInput);
let right: Resource<"compat.number_array"> = create(rightInput);
write(left, 0, 5);
write(left, 0, 7);
const leftResult = read(left, 0);
write(right, 0, 30);
const rightResult = read(right, 0);
write(left, 1, 10);
const leftAgain = read(left, 1);
return leftResult * 100 + rightResult + leftAgain;
}
main();
"#;

    let catalog = catalog();
    let hints = pd_vm_compat_frontends::collect_typescript_type_hints_with_options(
        source,
        pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("source and storage hints should use the same catalog");
    let hint = |name: &str| {
        hints
            .iter()
            .find(|hint| hint.name == name)
            .unwrap_or_else(|| panic!("missing hint for {name}"))
    };
    assert_eq!(
        hint("leftInput").schema,
        Some(TypeSchema::Array(Box::new(TypeSchema::Number)))
    );
    assert_eq!(hint("left").schema, Some(TypeSchema::Resource(array_key())));

    let compiled = typed_array_program(source, Arc::clone(&catalog));
    for name in ["arrays::create", "arrays::read", "arrays::write"] {
        let actual = compiled
            .program
            .host_import_schemas()
            .iter()
            .flatten()
            .find(|schema| schema.name == name)
            .unwrap_or_else(|| panic!("missing compiled schema for {name}"));
        assert_eq!(
            actual,
            &vm::catalog_import_schemas(&catalog, name)
                .into_iter()
                .next()
                .expect("catalog import schema")
        );
    }
    assert_eq!(
        vm::catalog_import_schemas(&catalog, "arrays::read")[0].params[0].passing,
        HostParamPassing::Borrow
    );
    assert_eq!(
        vm::catalog_import_schemas(&catalog, "arrays::write")[0].params[0].passing,
        HostParamPassing::BorrowMut
    );

    let registry = typed_array_registry(&catalog);
    let mut machine = Vm::new(compiled.program);
    registry
        .bind_vm_cached(&mut machine)
        .expect("typed array adapters should bind");
    assert_eq!(
        machine.run().expect("typed array program should run"),
        VmStatus::Halted
    );
    assert_eq!(machine.stack().last(), Some(&Value::Int(740)));
    assert_eq!(machine.host_context().resource_count(), 2);

    machine
        .reset_for_reuse()
        .expect("reset should close array owners");
    assert_eq!(machine.host_context().resource_count(), 0);
    assert_eq!(CLOSE_COUNT.load(Ordering::SeqCst), 2);
    assert_eq!(DROP_COUNT.load(Ordering::SeqCst), 2);
}

#[test]
fn typescript_array_resource_errors_preserve_message_and_reset_lifecycle() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    CLOSE_COUNT.store(0, Ordering::SeqCst);
    DROP_COUNT.store(0, Ordering::SeqCst);

    let source = r#"
import { create, read } from "arrays";
import type { Resource } from "rustscript:types";
function main(): number {
let owner: Resource<"compat.number_array"> = create([1, 2]);
return read(owner, 1.5);
}
main();
"#;
    let catalog = catalog();
    let compiled = typed_array_program(source, Arc::clone(&catalog));
    let registry = typed_array_registry(&catalog);
    let mut machine = Vm::new(compiled.program);
    registry
        .bind_vm_cached(&mut machine)
        .expect("typed array adapters should bind");

    let error = machine.run().expect_err("fractional index should abort");
    assert!(
        error
            .to_string()
            .contains("array index must be integral and in bounds")
    );
    assert_eq!(machine.host_context().resource_count(), 1);
    machine
        .reset_for_reuse()
        .expect("reset should close failed-call owner");
    assert_eq!(machine.host_context().resource_count(), 0);
    assert_eq!(CLOSE_COUNT.load(Ordering::SeqCst), 1);
    assert_eq!(DROP_COUNT.load(Ordering::SeqCst), 1);
}

#[test]
fn typescript_array_index_write_and_read_with_integer_literal() {
    let source = r#"
function main(): number {
    const values: number[] = [1, 2];
    values[1] = 5;
    return values[1];
}
main();
"#;
    let compiled = vm::compile_source_with_flavor_and_options(
        source,
        pd_vm_compat_frontends::typescript_flavor(),
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("integer-literal array indexing should compile");
    let mut machine = Vm::new(compiled.program);
    assert_eq!(
        machine.run().expect("indexed array program should run"),
        VmStatus::Halted
    );
    assert_eq!(machine.stack().last(), Some(&Value::Int(5)));
}

#[test]
fn typescript_array_capture_aliases_share_binding_across_recursion_and_factories() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let source = r#"
function repeatWrite(setter: (value: number) => void, value: number, depth: number): number {
    setter(value);
    return depth > 0 ? repeatWrite(setter, value + 1, depth - 1) : value;
}
function main(): number {
    let values: number[] = [1, 2, 3];
    const setValue = (value: number): void => {
        values[0] = value;
    };
    const getValue = (): number => values[0];
    const setAlias = setValue;
    const getAlias = getValue;
    const recursiveResult = repeatWrite(setAlias, 5, 2);
    const observed = getAlias();
    setValue(8);
    const originalObserved = getValue();
    values = [10, 20, 30];
    const reboundResult = repeatWrite(setAlias, 4, 1);
    const current = getValue();
    let other: number[] = [100];
    const setOther = (value: number): void => {
        other[0] = value;
    };
    const getOther = (): number => other[0];
    const independentResult = repeatWrite(setOther, 30, 0);
    return recursiveResult * 100000 + observed * 10000 + reboundResult * 100 + current * 10 + originalObserved + getOther() + independentResult;
}
main();
"#;
    let hints = pd_vm_compat_frontends::collect_typescript_type_hints_with_options(
        source,
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("capture array source hints should remain static");
    assert!(hints.iter().any(|hint| {
        hint.name == "values"
            && hint.schema == Some(TypeSchema::Array(Box::new(TypeSchema::Number)))
    }));

    let compiled = vm::compile_source_with_flavor_and_options(
        source,
        pd_vm_compat_frontends::typescript_flavor(),
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("typed array capture closure should compile");
    let mut capture_modes_by_source = HashMap::<u16, Vec<vm::CaptureBindingMode>>::new();
    for prototype in &compiled.program.callable_prototypes {
        for (source_slot, mode) in prototype
            .capture_source_slots
            .iter()
            .zip(&prototype.capture_modes)
        {
            capture_modes_by_source
                .entry(*source_slot)
                .or_default()
                .push(*mode);
        }
    }
    assert_eq!(
        capture_modes_by_source.len(),
        2,
        "the mutable array bindings should remain two distinct source cells"
    );
    for (source_slot, modes) in &capture_modes_by_source {
        assert!(
            modes.contains(&vm::CaptureBindingMode::Borrow),
            "read-only getters for source slot {source_slot} must borrow the shared cell; modes={modes:?}"
        );
        assert!(
            modes.contains(&vm::CaptureBindingMode::BorrowMut),
            "setters for source slot {source_slot} must borrow the shared cell mutably; modes={modes:?}"
        );
    }
    let mut machine = Vm::new(compiled.program);
    assert_eq!(
        machine.run().expect("capture closure should run"),
        VmStatus::Halted
    );
    assert_eq!(machine.stack().last(), Some(&Value::Int(770618)));
}

#[test]
fn typescript_read_only_scalar_getter_shares_mutable_capture_across_factory_calls() {
    let source = r#"
function makeReader(start: number): () => number {
    let value = start;
    const getValue = (): number => value;
    const setValue = (next: number): void => {
        value = next;
    };
    setValue(start + 1);
    return getValue;
}
const first = makeReader(1);
const second = makeReader(40);
first() * 100 + second();
"#;
    let hints = pd_vm_compat_frontends::collect_typescript_type_hints(source)
        .expect("factory return and captured scalar hints should remain typed");
    assert!(hints.iter().any(|hint| {
        hint.name == "first"
            && hint.schema
                == Some(TypeSchema::Callable {
                    params: Vec::new(),
                    result: Box::new(TypeSchema::Number),
                })
    }));

    let compiled = vm::compile_source_with_flavor_and_options(
        source,
        pd_vm_compat_frontends::typescript_flavor(),
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("typed scalar capture factory should compile");
    let mut capture_modes_by_source = HashMap::<u16, Vec<vm::CaptureBindingMode>>::new();
    for prototype in &compiled.program.callable_prototypes {
        for (source_slot, mode) in prototype
            .capture_source_slots
            .iter()
            .zip(&prototype.capture_modes)
        {
            capture_modes_by_source
                .entry(*source_slot)
                .or_default()
                .push(*mode);
        }
    }
    assert_eq!(capture_modes_by_source.len(), 1);
    let modes = capture_modes_by_source
        .values()
        .next()
        .expect("factory has captured value source slot");
    assert!(modes.contains(&vm::CaptureBindingMode::Borrow));
    assert!(modes.contains(&vm::CaptureBindingMode::BorrowMut));

    let mut machine = Vm::new(compiled.program);
    assert_eq!(
        machine.run().expect("getter factories should execute"),
        VmStatus::Halted
    );
    assert_eq!(machine.stack().last(), Some(&Value::Int(241)));
}

#[test]
fn typescript_reference_array_dispatcher_shares_owners_across_aliases_and_factories() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let source = array_dispatcher_source(
        r#"
    let first = makeArray([4, 5, 6]);
    const firstAlias = first;
    const second = makeArray([30]);
    const secondAlias = second;
    const before = firstAlias(1, 1, 0);
    first(2, 1, 9);
    const after = firstAlias(1, 1, 0);
    const recursiveResult = recursiveWrite(firstAlias, 7, 2);
    const secondBefore = secondAlias(1, 0, 0);
    second(2, 0, 40);
    const secondAfter = secondAlias(1, 0, 0);
    const oldLength = firstAlias(0, 0, 0);
    first = makeArray([70]);
    const rebound = first(1, 0, 0);
    const oldAfterRebind = firstAlias(1, 1, 0);
    const firstClosed = firstAlias(3, 0, 0);
    const reboundClosed = first(3, 0, 0);
    const secondClosed = secondAlias(3, 0, 0);
    return oldLength * 1000000 + before * 100000 + after * 10000
        + secondBefore * 1000 + secondAfter * 100 + rebound * 10
        + recursiveResult * 10 + oldAfterRebind + firstClosed + reboundClosed + secondClosed;
"#,
    );
    let catalog = production_array_catalog();
    let options = pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog));
    let hints =
        pd_vm_compat_frontends::collect_typescript_type_hints_with_options(&source, options)
            .expect("source array type and callable storage hints should remain typed");
    let hint_schema = |name: &str| {
        hints
            .iter()
            .find(|hint| hint.name == name)
            .unwrap_or_else(|| panic!("missing type hint for {name}"))
            .schema
            .clone()
            .unwrap_or_else(|| panic!("missing schema for type hint {name}"))
    };
    assert_eq!(
        hint_schema("initial"),
        TypeSchema::Array(Box::new(TypeSchema::Number))
    );
    assert_eq!(hint_schema("owner"), TypeSchema::Resource(array_key()));
    let dispatcher_schema = TypeSchema::Callable {
        params: vec![TypeSchema::Number, TypeSchema::Number, TypeSchema::Number],
        result: Box::new(TypeSchema::Number),
    };
    assert_eq!(hint_schema("first"), dispatcher_schema);

    let compiled = production_array_program(&source, Arc::clone(&catalog));
    let owner_captures = compiled
        .program
        .callable_prototypes
        .iter()
        .filter(|prototype| {
            prototype
                .capture_modes
                .contains(&vm::CaptureBindingMode::Move)
        })
        .collect::<Vec<_>>();
    assert_eq!(owner_captures.len(), 1);
    assert_eq!(
        owner_captures[0].capture_modes,
        [vm::CaptureBindingMode::Move],
        "one dispatcher prototype must move one owner into its callable environment"
    );
    assert_eq!(owner_captures[0].capture_source_slots.len(), 1);
    assert_eq!(owner_captures[0].schema.as_ref(), Some(&dispatcher_schema));
    assert_eq!(
        compiled
            .program
            .debug
            .as_ref()
            .and_then(|debug| debug.source.as_deref()),
        Some(source.as_str()),
        "capture mode evidence must remain tied to the compiled source"
    );

    for name in [
        "arrays::create",
        "arrays::read",
        "arrays::write",
        "arrays::length",
        "arrays::close",
    ] {
        let program_schema = compiled
            .program
            .host_import_schemas()
            .iter()
            .flatten()
            .find(|schema| schema.name == name)
            .unwrap_or_else(|| panic!("missing compiled import schema {name}"));
        assert_eq!(
            program_schema,
            &vm::catalog_import_schemas(&catalog, name)
                .into_iter()
                .next()
                .expect("typed array catalog schema")
        );
    }
    assert_eq!(
        vm::catalog_import_schemas(&catalog, "arrays::read")[0].params[0].passing,
        HostParamPassing::Borrow
    );
    assert_eq!(
        vm::catalog_import_schemas(&catalog, "arrays::write")[0].params[0].passing,
        HostParamPassing::BorrowMut
    );
    assert_eq!(
        vm::catalog_import_schemas(&catalog, "arrays::close")[0].params[0].passing,
        HostParamPassing::TakeOwned
    );

    let registry = production_array_registry(&catalog);
    let mut machine = Vm::new(compiled.program);
    registry
        .bind_vm_cached(&mut machine)
        .expect("the production typed array adapters should bind");
    assert_eq!(
        machine
            .run()
            .expect("shared array dispatchers should execute"),
        VmStatus::Halted
    );
    assert_eq!(machine.stack().last(), Some(&Value::Int(3_624_802)));
    assert_eq!(machine.host_context().resource_count(), 0);
}

#[test]
fn typescript_reference_array_optional_read_preserves_integral_missing_values() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let catalog = production_array_catalog();
    let registry = production_array_registry(&catalog);

    for (index, expected) in [(-1, Value::Null), (4, Value::Null), (0, Value::Int(11))] {
        let source = format!(
            r#"
import {{ create, try_read, close }} from "arrays";
import type {{ Resource }} from "rustscript:types";
const owner: Resource<"compat.number_array"> = create([11]);
const result = try_read(owner, {index});
close(owner);
result;
"#
        );
        let hints = pd_vm_compat_frontends::collect_typescript_type_hints_with_options(
            &source,
            pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog)),
        )
        .expect("optional read source should preserve its checked result schema");
        assert_eq!(
            hints
                .iter()
                .find(|hint| hint.name == "result")
                .and_then(|hint| hint.schema.clone()),
            Some(TypeSchema::Optional(Box::new(TypeSchema::Number)))
        );
        let compiled = production_array_program(&source, Arc::clone(&catalog));
        let actual = compiled
            .program
            .host_import_schemas()
            .iter()
            .flatten()
            .find(|schema| schema.name == "arrays::try_read")
            .expect("compiled optional read import schema");
        assert_eq!(
            actual,
            &vm::catalog_import_schemas(&catalog, "arrays::try_read")[0]
        );

        let mut machine = Vm::new(compiled.program);
        registry
            .bind_vm_cached(&mut machine)
            .expect("optional read adapter should bind");
        assert_eq!(
            machine.run().expect("integral misses are values"),
            VmStatus::Halted
        );
        assert_eq!(machine.stack().last(), Some(&expected));
        assert_eq!(machine.host_context().resource_count(), 0);
    }
}

#[test]
fn typescript_reference_array_has_index_returns_false_for_integral_misses() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let source = r#"
import { create, has_index, close } from "arrays";
import type { Resource } from "rustscript:types";
const owner: Resource<"compat.number_array"> = create([11]);
const negativeMissing = has_index(owner, -1);
const pastEnd = has_index(owner, 4);
const present = has_index(owner, 0);
close(owner);
!negativeMissing && !pastEnd && present;
"#;
    let catalog = production_array_catalog();
    let hints = pd_vm_compat_frontends::collect_typescript_type_hints_with_options(
        source,
        pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("presence result should retain a boolean checker type");
    assert_eq!(
        hints
            .iter()
            .find(|hint| hint.name == "negativeMissing")
            .and_then(|hint| hint.schema.clone()),
        Some(TypeSchema::Bool)
    );
    let compiled = production_array_program(source, Arc::clone(&catalog));
    let actual = compiled
        .program
        .host_import_schemas()
        .iter()
        .flatten()
        .find(|schema| schema.name == "arrays::has_index")
        .expect("compiled presence import schema");
    assert_eq!(
        actual,
        &vm::catalog_import_schemas(&catalog, "arrays::has_index")[0]
    );

    let registry = production_array_registry(&catalog);
    let mut machine = Vm::new(compiled.program);
    registry
        .bind_vm_cached(&mut machine)
        .expect("presence adapter should bind");
    assert_eq!(
        machine.run().expect("presence checks should run"),
        VmStatus::Halted
    );
    assert_eq!(machine.stack().last(), Some(&Value::Bool(true)));
    assert_eq!(machine.host_context().resource_count(), 0);
}

#[test]
fn typescript_reference_array_optional_read_rejects_fractional_indices() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let source = r#"
import { create, try_read } from "arrays";
import type { Resource } from "rustscript:types";
const owner: Resource<"compat.number_array"> = create([11]);
try_read(owner, 0.5);
"#;
    let catalog = production_array_catalog();
    let compiled = production_array_program(&source, Arc::clone(&catalog));
    let registry = production_array_registry(&catalog);
    let mut machine = Vm::new(compiled.program);
    registry
        .bind_vm_cached(&mut machine)
        .expect("optional read adapter should bind");
    let error = machine
        .run()
        .expect_err("fractional indexes are invalid rather than missing");
    assert!(
        error
            .to_string()
            .contains("array index must be finite and integral"),
        "unexpected error: {error}"
    );
    assert_eq!(machine.host_context().resource_count(), 1);
    machine
        .reset_for_reuse()
        .expect("reset should release owner after invalid index");
    assert_eq!(machine.host_context().resource_count(), 0);
}

#[test]
fn typescript_reference_array_dispatcher_rejects_use_after_explicit_close() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let source = array_dispatcher_source(
        r#"
    const array = makeArray([1, 2]);
    const alias = array;
    array(3, 0, 0);
    return alias(1, 0, 0);
"#,
    );
    let catalog = production_array_catalog();
    let compiled = production_array_program(&source, Arc::clone(&catalog));
    let registry = production_array_registry(&catalog);
    let mut machine = Vm::new(compiled.program);
    registry
        .bind_vm_cached(&mut machine)
        .expect("production array adapters should bind");

    let error = machine
        .run()
        .expect_err("an aliased dispatcher must reject access after close");
    assert!(
        error.to_string().contains("resource"),
        "unexpected error: {error}"
    );
    assert_eq!(machine.host_context().resource_count(), 0);
}

#[test]
fn typescript_reference_array_dispatcher_reset_and_vm_states_are_isolated() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let catalog = production_array_catalog();
    let source_for = |initial: i64, write: bool| {
        let body = if write {
            format!(
                "    const array = makeArray([{initial}]);\n    array(2, 0, 42);\n    return array(1, 0, 0);"
            )
        } else {
            format!("    const array = makeArray([{initial}]);\n    return array(1, 0, 0);")
        };
        array_dispatcher_source(&body)
    };
    let first_source = source_for(1, true);
    let second_source = source_for(9, false);
    let first_program = production_array_program(&first_source, Arc::clone(&catalog));
    let second_program = production_array_program(&second_source, Arc::clone(&catalog));
    let registry = production_array_registry(&catalog);
    let mut first = Vm::new(first_program.program);
    let mut second = Vm::new(second_program.program);
    registry
        .bind_vm_cached(&mut first)
        .expect("first VM gets its own compatible adapter bindings");
    registry
        .bind_vm_cached(&mut second)
        .expect("second VM gets its own compatible adapter bindings");

    assert_eq!(
        first.run().expect("first VM should execute"),
        VmStatus::Halted
    );
    assert_eq!(
        second.run().expect("second VM should execute"),
        VmStatus::Halted
    );
    assert_eq!(first.stack().last(), Some(&Value::Int(42)));
    assert_eq!(second.stack().last(), Some(&Value::Int(9)));
    assert_eq!(first.host_context().resource_count(), 1);
    assert_eq!(second.host_context().resource_count(), 1);
    first
        .reset_for_reuse()
        .expect("reset closes the first VM owner");
    second
        .reset_for_reuse()
        .expect("reset closes the second VM owner");
    assert_eq!(first.host_context().resource_count(), 0);
    assert_eq!(second.host_context().resource_count(), 0);
}

#[test]
fn typescript_reference_array_dispatcher_abort_releases_owner_on_reset() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let source = array_dispatcher_source(
        r#"
    const array = makeArray([1, 2]);
    return array(1, 1.5, 0);
"#,
    );
    let catalog = production_array_catalog();
    let compiled = production_array_program(&source, Arc::clone(&catalog));
    let registry = production_array_registry(&catalog);
    let mut machine = Vm::new(compiled.program);
    registry
        .bind_vm_cached(&mut machine)
        .expect("production array adapters should bind");

    let error = machine
        .run()
        .expect_err("fractional indexes must not be truncated by the native adapter");
    assert!(
        error
            .to_string()
            .contains("array index must be integral and in bounds"),
        "unexpected error: {error}"
    );
    assert_eq!(machine.host_context().resource_count(), 1);
    machine
        .reset_for_reuse()
        .expect("reset closes resource after aborted dispatcher execution");
    assert_eq!(machine.host_context().resource_count(), 0);
}

#[test]
fn typescript_resource_capture_keeps_core_ownership_rejection() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let source = r#"
import { create, read } from "arrays";
import type { Resource } from "rustscript:types";
function main(): number {
    let owner: Resource<"compat.number_array"> = create([1, 2]);
    const getValue = (index: number): number => read(owner, index);
    return getValue(0);
}
main();
"#;
    let catalog = catalog();
    let result = vm::compile_source_with_flavor_and_options(
        source,
        pd_vm_compat_frontends::typescript_flavor(),
        pd_vm_compat_frontends::compile_options_with_catalog(catalog),
    );
    let diagnostic = match result {
        Err(error) => format!("{error:?}"),
        Ok(_) => panic!("borrowed resource capture must remain rejected"),
    };
    assert!(diagnostic.contains("E_OWNERSHIP_BORROW_ESCAPE"));
    assert!(diagnostic.contains("must move it"));
}
