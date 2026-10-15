use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use vm::{CaptureBindingMode, HostFunctionRegistry, SourceFlavor, Value, Vm, VmStatus};

static ALLOCATION_TEST_LOCK: Mutex<()> = Mutex::new(());

struct CountingAllocator;

static COUNT_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static ALLOCATION_COUNT: AtomicUsize = AtomicUsize::new(0);
static ALLOCATED_BYTES: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

// This isolated integration-test binary runs the filtered probe serially. The
// counters are enabled only around VM execution, after parsing and setup.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() && COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() && COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, old: Layout, new_size: usize) -> *mut u8 {
        let pointer = unsafe { System.realloc(pointer, old, new_size) };
        if !pointer.is_null() && COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(new_size, Ordering::Relaxed);
        }
        pointer
    }
}

fn captured_array_write_probe(array_len: usize, writes: usize) -> (usize, usize) {
    let values = std::iter::repeat_n("0", array_len)
        .collect::<Vec<_>>()
        .join(", ");
    let calls = std::iter::repeat_n("update(0);", writes)
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!(
        "let mut values = [{values}];\n\
         let update = |index| if true => {{\n\
             values[index] = 1;\n\
             values[index]\n\
         }} else => {{ 0 }};\n\
         {calls}\n\
         values[0];\n"
    );
    let compiled = vm::compile_source_with_flavor(&source, SourceFlavor::RustScript)
        .expect("captured array write fixture should compile");
    assert!(
        compiled
            .program
            .callable_prototypes
            .iter()
            .any(|prototype| {
                prototype
                    .capture_modes
                    .contains(&CaptureBindingMode::BorrowMut)
            })
    );
    let mut machine = Vm::new(compiled.program);

    ALLOCATION_COUNT.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    COUNT_ALLOCATIONS.store(true, Ordering::SeqCst);
    let result = machine.run();
    COUNT_ALLOCATIONS.store(false, Ordering::SeqCst);

    assert_eq!(
        result.expect("captured array writes should run"),
        VmStatus::Halted
    );
    assert_eq!(machine.stack().last(), Some(&Value::Int(1)));
    (
        ALLOCATION_COUNT.load(Ordering::Relaxed),
        ALLOCATED_BYTES.load(Ordering::Relaxed),
    )
}

fn reference_array_dispatcher_write_probe(array_len: usize, writes: usize) -> (usize, usize) {
    let initial = std::iter::repeat_n("0", array_len)
        .collect::<Vec<_>>()
        .join(", ");
    let calls = std::iter::repeat_n("array(2, 0, 1);", writes)
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!(
        r#"
import {{ create, read, write, length, close }} from "arrays";
import type {{ Resource }} from "rustscript:types";
type ArrayDispatcher = (operation: number, index: number, value: number) => number;
function makeArray(initial: number[]): ArrayDispatcher {{
    let owner: Resource<"compat.number_array"> = create(initial);
    const dispatcher: ArrayDispatcher = (operation: number, index: number, value: number): number =>
        operation === 0 ? length(owner)
        : operation === 1 ? read(owner, index)
        : operation === 2 ? write(owner, index, value)
        : close(owner);
    return dispatcher;
}}
function main(): number {{
    const array = makeArray([{initial}]);
    {calls}
    const result = array(1, 0, 0);
    array(3, 0, 0);
    return result;
}}
main();
"#
    );
    let module = pd_vm_compat_frontends::runtime::array::typescript_array_host_module();
    let catalog = Arc::new(module.catalog().expect("typed array catalog should build"));
    let compiled = vm::compile_source_with_flavor_and_options(
        &source,
        pd_vm_compat_frontends::typescript_flavor(),
        pd_vm_compat_frontends::compile_options_with_catalog(Arc::clone(&catalog)),
    )
    .expect("typed dispatcher allocation fixture should compile");
    let mut registry = HostFunctionRegistry::restricted();
    module
        .install_from_catalog(&mut registry, &catalog)
        .expect("typed array adapter should install from its catalog");
    let mut machine = Vm::new(compiled.program);
    registry
        .bind_vm_cached(&mut machine)
        .expect("typed array adapter should bind to VM");

    ALLOCATION_COUNT.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    COUNT_ALLOCATIONS.store(true, Ordering::SeqCst);
    let result = machine.run();
    COUNT_ALLOCATIONS.store(false, Ordering::SeqCst);

    assert_eq!(
        result.expect("reference array dispatch should run"),
        VmStatus::Halted
    );
    assert_eq!(
        machine.stack().last(),
        Some(&Value::Int(if writes > 0 { 1 } else { 0 }))
    );
    assert_eq!(
        machine.host_context().resource_count(),
        0,
        "explicit close should release owner"
    );
    (
        ALLOCATION_COUNT.load(Ordering::Relaxed),
        ALLOCATED_BYTES.load(Ordering::Relaxed),
    )
}

#[test]
fn mutable_array_capture_write_cost_scales_with_backing_array_size() {
    let _guard = ALLOCATION_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let writes = 8;
    let value_bytes = std::mem::size_of::<Value>();
    let small_len = 16;
    let large_len = 128;
    let (small_allocs, small_bytes) = captured_array_write_probe(small_len, writes);
    let (large_allocs, large_bytes) = captured_array_write_probe(large_len, writes);
    let expected_growth = (large_len - small_len) * value_bytes * writes;

    println!(
        "capture-array-cow process-local allocations writes={writes} value_bytes={value_bytes} small_len={small_len} small_allocs={small_allocs} small_allocated_bytes={small_bytes} large_len={large_len} large_allocs={large_allocs} large_allocated_bytes={large_bytes} expected_copy_growth_bytes={expected_growth}"
    );
    assert!(
        large_bytes.saturating_sub(small_bytes) >= expected_growth / 2,
        "captured BorrowMut writes should expose per-write COW copy growth: small={small_bytes}, large={large_bytes}, expected at least {}",
        expected_growth / 2
    );
}

#[test]
fn resource_dispatcher_writes_avoid_per_write_array_copy_growth() {
    let _guard = ALLOCATION_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let writes = 8;
    let value_bytes = std::mem::size_of::<Value>();
    let small_len = 16;
    let large_len = 128;
    let (small_read_allocs, small_read_bytes) =
        reference_array_dispatcher_write_probe(small_len, 0);
    let (small_write_allocs, small_write_bytes) =
        reference_array_dispatcher_write_probe(small_len, writes);
    let (large_read_allocs, large_read_bytes) =
        reference_array_dispatcher_write_probe(large_len, 0);
    let (large_write_allocs, large_write_bytes) =
        reference_array_dispatcher_write_probe(large_len, writes);
    let small_write_growth = small_write_bytes.saturating_sub(small_read_bytes);
    let large_write_growth = large_write_bytes.saturating_sub(large_read_bytes);
    let size_scaled_write_growth = large_write_growth.saturating_sub(small_write_growth);
    let expected_copy_growth = (large_len - small_len) * value_bytes * writes;

    println!(
        "reference-array-dispatcher process-local allocations writes={writes} value_bytes={value_bytes} small_len={small_len} read_allocs={small_read_allocs} read_bytes={small_read_bytes} write_allocs={small_write_allocs} write_bytes={small_write_bytes} small_write_growth={small_write_growth} large_len={large_len} read_allocs={large_read_allocs} read_bytes={large_read_bytes} write_allocs={large_write_allocs} write_bytes={large_write_bytes} large_write_growth={large_write_growth} size_scaled_write_growth_bytes={size_scaled_write_growth} expected_cow_copy_growth_bytes={expected_copy_growth}"
    );
    assert!(
        size_scaled_write_growth < expected_copy_growth / 2,
        "resource writes should not add backing-array-size allocation per write: observed={size_scaled_write_growth}, expected COW growth={expected_copy_growth}"
    );
}
