//! Real LuaHeap root handoff across an ordinary call, a tail forward, and the
//! protected receive.
//!
//! This is the second private entry of the same completion lowerer. It feeds
//! the same real lexer/parser/resolver, compiles through the fixed Core and
//! runs one interpreter VM. The difference is that every Lua function enters a
//! real `LuaHeap` root frame, every admitted Lua call publishes an issued error
//! table into the caller's reserved root slot before the callee frame is
//! retired, and `pcall` either adopts that table into its formal slot or
//! clears the exact temp root when the object is discarded.
//!
//! The completion protocol is decoded by its own shape, never by a fixture
//! singleton: `[true, ...values]` retires the callee frame with its original
//! arity and typed values, `[false, scalar]` retires it with no root, and
//! `[false, issued_table]` publishes that same-heap table. A table in a normal
//! completion, or an array that is not a live issued table, is refused with a
//! precise host error. The error object and its strong child are canonical
//! issued table carriers, never Core arrays standing in for a table.
//!
//! A normal chunk completion must never carry a table. The chunk exit has no
//! root slot, so a carrier there would be a value the frame retirement is
//! about to reclaim. The chunk finish refuses every table in a normal
//! completion before it retires anything: the formal root still holds the
//! table, the chunk frame stays on top, and the caller is the one that
//! releases that frame and collects the reclaimed carrier.
//!
//! The protected receiver adopts a table only from the exact reserved temp
//! root: the temp slot must already hold that same issued table when the
//! formal root is written. The temp slot is cleared after the formal write. A
//! missing third publish is refused at the receiver, never repaired by
//! fabricating a formal root. That refusal also keeps the chunk frame open
//! for the caller to release and collect.
//!
//! An uncaught issued table at the chunk boundary keeps the chunk frame open:
//! the caller receives the table together with an explicit live root, then
//! releases the frame and collects. The boundary records both states, and a
//! reclaimed carrier is never classified as a usable table.
//!
//! The observer holds only `TableRef` values and carrier pointers; a guest
//! carrier does not keep the heap node alive. A refusal record additionally
//! keeps the offending carrier value so the test can classify it after the
//! run; that clone is not a root. Collection is the real synchronous full
//! sweep.

use std::sync::{Arc, Mutex};

use vm::compiler::HostCallResolver;
use vm::vm::VmFrameContinuation;
use vm::{
    CallOutcome, CallReturn, CompileSourceFileOptions, FrontendImportSyntax, FrontendIr,
    HostFunction, ParseError, SourceError, SourceFlavor, SourcePathError, SourcePlugin, Value, Vm,
    VmError, VmStatus, VmYieldReason,
};

use super::{
    CompletionExit, FUEL_BUDGET, FUEL_CHECK_INTERVAL, catalog, consume_completion_exit,
    decoded_call_sites, fixture_path, lower,
};
use crate::heap;

const POSITIVE_FIXTURE: &str = "root-handoff.lua";
const MAX_FIXTURE_TABLES: usize = 8;

/// One test arm. The positive arm publishes at every handoff. Each negative
/// arm omits exactly one publish while keeping that handoff's frame retirement
/// (and, for the first two, the real full collection in the same window).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arm {
    Positive,
    OmitFirstPublish,
    OmitSecondPublish,
    OmitThirdPublish,
}

/// A run budget. `Normal` is the fixed production-shaped fuel budget used by
/// every behavioral arm; the other two are real VM interrupt arms so the
/// bottom-level status is observed instead of synthesized.
#[derive(Clone, Copy, Debug)]
enum RunMode {
    Normal,
    Fuel(u64),
    Epoch(u64),
}

#[derive(Clone, Copy, Debug)]
struct TableWitness {
    table: heap::TableRef,
    carrier: usize,
}

#[derive(Clone, Debug)]
struct EnterRecord {
    child: (u32, u32),
    parent: Option<(u32, u32)>,
    core_depth: usize,
}

/// The typed decode of one completion value. Carrier arrays are recorded as a
/// kind only: the observer must not clone a carrier and keep its heap node
/// reachable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TypedValue {
    Nil,
    Bool(bool),
    Int(i64),
    Float(u64),
    String,
    Bytes(usize),
    Array,
    Map,
    Callable,
}

fn typed_value(value: &Value) -> TypedValue {
    match value {
        Value::Null => TypedValue::Nil,
        Value::Bool(inner) => TypedValue::Bool(*inner),
        Value::Int(inner) => TypedValue::Int(*inner),
        Value::Float(inner) => TypedValue::Float(inner.to_bits()),
        Value::String(_) => TypedValue::String,
        Value::Bytes(inner) => TypedValue::Bytes(inner.len()),
        Value::Array(_) => TypedValue::Array,
        Value::Map(_) => TypedValue::Map,
        Value::Callable(_) => TypedValue::Callable,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CompletionClass {
    Normal {
        arity: usize,
        values: Vec<TypedValue>,
    },
    Error {
        payload: TypedValue,
    },
}

fn decode_completion(value: &Value) -> Result<CompletionClass, String> {
    let Value::Array(items) = value else {
        return Err("root completion is not a private array".to_string());
    };
    let Some(Value::Bool(status)) = items.first() else {
        return Err("root completion has no Boolean status".to_string());
    };
    if *status {
        let values: Vec<_> = items[1..].iter().map(typed_value).collect();
        return Ok(CompletionClass::Normal {
            arity: values.len(),
            values,
        });
    }
    if items.len() != 2 {
        return Err(format!(
            "error completion must carry exactly one payload, got {} items",
            items.len()
        ));
    }
    Ok(CompletionClass::Error {
        payload: typed_value(&items[1]),
    })
}

#[derive(Clone, Debug)]
struct TableAlive {
    table: heap::TableRef,
    before: bool,
    after: bool,
}

#[derive(Clone, Debug)]
struct WindowRecord {
    collections_before: u32,
    orphan: TableWitness,
    orphan_alive_before: bool,
    tables: Vec<TableAlive>,
    orphan_alive_after: bool,
    collections_after: u32,
    report: heap::CollectReport,
}

#[derive(Clone, Debug)]
struct HandoffRecord {
    index: usize,
    child: (u32, u32),
    parent: (u32, u32),
    recorded_parent: Option<(u32, u32)>,
    parent_slot: u32,
    parent_slot_before_empty: bool,
    child_top_at_publish: bool,
    completion: CompletionClass,
    skipped_publish: bool,
    published: bool,
    payload_table: Option<heap::TableRef>,
    payload_carrier: Option<usize>,
    parent_slot_after: Option<heap::TableRef>,
    retired: bool,
    top_after_parent: bool,
    caller_core_prototype: Option<u32>,
    core_depth: usize,
    window: Option<WindowRecord>,
}

#[derive(Clone, Debug)]
struct FormalRecord {
    frame: (u32, u32),
    temp_slot: u32,
    formal_slot: u32,
    temp_before: Option<heap::TableRef>,
    formal_before: Option<heap::TableRef>,
    value: TypedValue,
    value_table: Option<heap::TableRef>,
    rooted: bool,
    temp_after: Option<heap::TableRef>,
    formal_after: Option<heap::TableRef>,
}

#[derive(Clone, Debug)]
struct DiscardRecord {
    frame: (u32, u32),
    temp_slot: u32,
    formal_slot: u32,
    completion: CompletionClass,
    payload_table: Option<heap::TableRef>,
    payload_carrier: Option<usize>,
    temp_before: Option<heap::TableRef>,
    temp_after: Option<heap::TableRef>,
    formal_before: Option<heap::TableRef>,
    formal_after: Option<heap::TableRef>,
    cleared_exact_root: bool,
}

#[derive(Clone, Debug)]
struct UncaughtRecord {
    payload: TypedValue,
    table: Option<heap::TableRef>,
    carrier: Option<usize>,
    frame: Option<(u32, u32)>,
    root_slot: Option<u32>,
    rooted_in_temp: bool,
    rooted_in_formal: bool,
    frame_retained: bool,
    alive_before_finish: bool,
}

#[derive(Clone, Debug)]
struct FinishCollection {
    tables: Vec<TableAlive>,
    orphan: TableWitness,
    orphan_alive_before: bool,
    orphan_alive_after: bool,
    collections_before: u32,
    collections_after: u32,
    report: heap::CollectReport,
}

#[derive(Clone, Debug)]
struct FinishRecord {
    frame: (u32, u32),
    formal_slot: u32,
    formal_before: Option<heap::TableRef>,
    completion: CompletionClass,
    retired: bool,
    top_after: Option<(u32, u32)>,
    collection: Option<FinishCollection>,
    uncaught: Option<UncaughtRecord>,
    refused: Option<FinishRefusal>,
}

/// One refusal at a host protocol seam. The reason string is the exact host
/// error; the frame and slot fields are what the host observed before it
/// refused. The frame is never retired here, so the caller still owns it.
#[derive(Clone, Debug)]
struct FinishRefusal {
    site: &'static str,
    frame: (u32, u32),
    temp_slot: u32,
    formal_slot: u32,
    temp_before: Option<heap::TableRef>,
    formal_before: Option<heap::TableRef>,
    value: TypedValue,
    table: Option<heap::TableRef>,
    carrier: Option<usize>,
    carrier_value: Option<Value>,
    frame_retained: bool,
    reason: String,
}

/// One refusal of a protected table receive. It is recorded before any formal
/// root is written: `formal_after` and `temp_after` are still empty.
#[derive(Clone, Debug)]
struct FormalRefusal {
    site: &'static str,
    frame: (u32, u32),
    temp_slot: u32,
    formal_slot: u32,
    temp_before: Option<heap::TableRef>,
    formal_before: Option<heap::TableRef>,
    value: TypedValue,
    table: Option<heap::TableRef>,
    carrier: Option<usize>,
    carrier_value: Option<Value>,
    temp_after: Option<heap::TableRef>,
    formal_after: Option<heap::TableRef>,
    frame_retained: bool,
    reason: String,
}

#[derive(Clone, Debug)]
struct NoteRecord {
    frame: (u32, u32),
    table: Option<heap::TableRef>,
    table_index: Option<usize>,
    carrier: Option<usize>,
    temp_empty: bool,
    formal_table: Option<heap::TableRef>,
    table_alive: bool,
    child: Option<heap::TableRef>,
    child_alive: bool,
    rooted: bool,
}

#[derive(Clone, Debug)]
struct RootWitness {
    arm: Arm,
    tables: Vec<TableWitness>,
    enters: Vec<EnterRecord>,
    handoffs: Vec<HandoffRecord>,
    formals: Vec<FormalRecord>,
    formal_refusals: Vec<FormalRefusal>,
    discards: Vec<DiscardRecord>,
    finish: Option<FinishRecord>,
    note: Option<NoteRecord>,
    marks: Vec<super::MarkRecord>,
    failures: Vec<String>,
}

/// The explicit uncaught boundary. `live_root` is true when the chunk frame
/// was retained with the payload in a real root slot; `carrier_is_table_*`
/// proves whether the carrier is still a usable table before and after the
/// caller releases that frame and collects.
#[derive(Clone, Debug)]
struct UncaughtBoundary {
    table: Option<heap::TableRef>,
    carrier: Option<usize>,
    live_root: bool,
    alive_at_boundary: bool,
    carrier_is_table_at_boundary: bool,
    frame: Option<(u32, u32)>,
    temp_after_run: Option<heap::TableRef>,
    formal_after_run: Option<heap::TableRef>,
    released: bool,
    alive_after_release: bool,
    carrier_is_table_after_release: bool,
    report: Option<heap::CollectReport>,
}

/// One exit carrier classified on the scope heap after `Vm::run`.
///
/// A refused or reclaimed carrier is not an issued table: the classification
/// is the evidence, not the pointer identity.
#[derive(Clone, Copy, Debug)]
struct ExitCarrier {
    carrier: usize,
    class: heap::GuestClass,
}

/// The post-run state of one refused protocol seam.
///
/// Both refusals happen before any frame retirement, so the chunk frame, its
/// slots and the carrier are still real after `Vm::run` returns. This records
/// that state and then performs the caller-side release and collection: the
/// refusing host does not retire or collect the chunk frame.
#[derive(Clone, Debug)]
struct RefusalBoundary {
    site: &'static str,
    frame: (u32, u32),
    top_after_run: Option<(u32, u32)>,
    frame_retained: bool,
    temp_after_run: Option<heap::TableRef>,
    formal_after_run: Option<heap::TableRef>,
    carrier_is_table_before_release: bool,
    table_alive_before_release: bool,
    child: Option<heap::TableRef>,
    child_alive_before_release: bool,
    released_by_caller: bool,
    carrier_is_table_after_release: bool,
    table_alive_after_release: bool,
    child_alive_after_release: bool,
    report: Option<heap::CollectReport>,
}

#[derive(Debug)]
struct RootRun {
    stack: Vec<Value>,
    exit: CompletionExit,
    fuel_budget: Option<u64>,
    fuel_remaining: Option<u64>,
    last_yield_reason: Option<VmYieldReason>,
    epoch_deadline: Option<u64>,
    marks: Vec<super::MarkRecord>,
    call_sites: Vec<super::DecodedCallSite>,
    witness: RootWitness,
    boundary: Option<UncaughtBoundary>,
    exit_carriers: Vec<ExitCarrier>,
    heap_after_run: Option<heap::HeapStats>,
    refusal: Option<RefusalBoundary>,
}

impl RootRun {
    /// P0 witness view over this run's real values so the already accepted
    /// owner/return-IP/argc/script-target checks apply unchanged.
    fn as_p0_run(&self) -> super::Run {
        super::Run {
            stack: self.stack.clone(),
            fuel_consumed: self
                .fuel_budget
                .zip(self.fuel_remaining)
                .map(|(budget, remaining)| budget.saturating_sub(remaining))
                .unwrap_or(0),
            marks: self.marks.clone(),
            seen: None,
            ticks: Vec::new(),
            exit: CompletionExit::MalformedTransport {
                detail: "root run does not reuse the P0 exit label".to_string(),
            },
            call_sites: self.call_sites.clone(),
            capture_plans: Vec::new(),
        }
    }
}

/// Resolves the private root transport and the two guest-visible table hosts
/// through the same test catalog. Production `LuaPlugin` never calls this.
struct RootHandoffPlugin;

static ROOT_HANDOFF_PLUGIN: RootHandoffPlugin = RootHandoffPlugin;

impl SourcePlugin for RootHandoffPlugin {
    fn flavor(&self) -> SourceFlavor {
        SourceFlavor::Lua
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["lua"]
    }

    fn import_syntax(&self) -> FrontendImportSyntax {
        FrontendImportSyntax::Lua
    }

    fn parse_source(&self, source: &str) -> Result<FrontendIr, ParseError> {
        let resolver = HostCallResolver::new(catalog());
        lower::lower_source_with_roots(source, Some(&resolver))
    }
}

fn root_options() -> CompileSourceFileOptions {
    CompileSourceFileOptions::new()
        .with_source_plugin(&ROOT_HANDOFF_PLUGIN)
        .with_host_api_catalog(Arc::clone(catalog()))
}

#[derive(Clone, Copy, Debug)]
enum RootOp {
    Mark,
    Note,
    NewTable,
    SetChild,
    Enter,
    Handoff,
    Formal,
    Discard,
    Finish,
}

struct RootHost {
    op: RootOp,
    witness: Arc<Mutex<RootWitness>>,
}

fn host_fault(message: impl Into<String>) -> VmError {
    VmError::HostError(message.into())
}

fn heap_fault(error: heap::HeapError) -> VmError {
    host_fault(format!("lua root handoff: {error}"))
}

fn carrier_ptr(value: &Value) -> Option<usize> {
    match value {
        Value::Array(issued) => Some(Arc::as_ptr(issued) as usize),
        _ => None,
    }
}

fn frame_token(frame: heap::RootFrameId) -> Value {
    let (index, generation) = frame.parts();
    Value::array(vec![
        Value::Int(i64::from(index)),
        Value::Int(i64::from(generation)),
    ])
}

fn decode_frame_token(value: &Value) -> Result<heap::RootFrameId, VmError> {
    let Value::Array(parts) = value else {
        return Err(host_fault("root frame token is not a private array"));
    };
    let [Value::Int(index), Value::Int(generation)] = parts.as_slice() else {
        return Err(host_fault("root frame token is not index plus generation"));
    };
    let index = u32::try_from(*index).map_err(|_| host_fault("root frame index is not a u32"))?;
    let generation =
        u32::try_from(*generation).map_err(|_| host_fault("root frame generation is not a u32"))?;
    Ok(heap::RootFrameId::from_parts(index, generation))
}

fn u32_arg(value: &Value, what: &str) -> Result<u32, VmError> {
    let Value::Int(number) = value else {
        return Err(host_fault(format!("{what} is not an integer")));
    };
    u32::try_from(*number).map_err(|_| host_fault(format!("{what} is not a u32")))
}

fn classify(host: &mut vm::HostContext<'_>, value: &Value) -> Result<heap::GuestClass, VmError> {
    heap::with_heap(host, |heap| Ok(heap.guest_class(value))).map_err(heap_fault)
}

fn classify_table(
    host: &mut vm::HostContext<'_>,
    value: &Value,
) -> Result<heap::TableRef, VmError> {
    match classify(host, value)? {
        heap::GuestClass::Table(table) => Ok(table),
        other => Err(host_fault(format!(
            "value is not an issued table: {other:?}"
        ))),
    }
}

fn alive_table(host: &mut vm::HostContext<'_>, table: heap::TableRef) -> Result<bool, VmError> {
    heap::with_heap(host, |heap| Ok(heap.alive(table))).map_err(heap_fault)
}

fn current_frame(host: &mut vm::HostContext<'_>) -> Result<Option<heap::RootFrameId>, VmError> {
    heap::with_heap(host, |heap| Ok(heap.current_frame())).map_err(heap_fault)
}

fn rooted_table(
    host: &mut vm::HostContext<'_>,
    frame: heap::RootFrameId,
    slot: u32,
) -> Result<Option<heap::TableRef>, VmError> {
    heap::with_heap(host, |heap| heap.rooted_table(frame, slot)).map_err(heap_fault)
}

/// One real full collection. The observed tables are read around the sweep so
/// each window carries independent reclamation evidence, and a fresh unrooted
/// orphan proves the sweep actually ran.
fn collect_window(
    host: &mut vm::HostContext<'_>,
    observed: &[heap::TableRef],
) -> Result<WindowRecord, VmError> {
    let stats_before = heap::with_heap(host, |heap| Ok(heap.stats())).map_err(heap_fault)?;
    let orphan_value = heap::publish_guest(host).map_err(heap_fault)?;
    let orphan_table = classify_table(host, &orphan_value)?;
    let orphan = TableWitness {
        table: orphan_table,
        carrier: carrier_ptr(&orphan_value)
            .ok_or_else(|| host_fault("fresh window orphan has no carrier"))?,
    };
    let orphan_alive_before = alive_table(host, orphan_table)?;
    let mut before = Vec::with_capacity(observed.len());
    for table in observed {
        before.push(alive_table(host, *table)?);
    }
    let report = heap::collect(host).map_err(heap_fault)?;
    let stats_after = heap::with_heap(host, |heap| Ok(heap.stats())).map_err(heap_fault)?;
    let mut tables = Vec::with_capacity(observed.len());
    for (table, before) in observed.iter().zip(before) {
        let after = alive_table(host, *table)?;
        tables.push(TableAlive {
            table: *table,
            before,
            after,
        });
    }
    let orphan_alive_after = alive_table(host, orphan_table)?;
    Ok(WindowRecord {
        collections_before: stats_before.collections,
        orphan,
        orphan_alive_before,
        tables,
        orphan_alive_after,
        collections_after: stats_after.collections,
        report,
    })
}

impl RootHost {
    fn fail(&self, message: impl Into<String>) -> VmError {
        let message = message.into();
        self.witness
            .lock()
            .expect("root witness")
            .failures
            .push(message.clone());
        host_fault(message)
    }

    fn mark(&self, vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
        let [Value::String(label)] = args else {
            return Err(host_fault("mark expects one string label"));
        };
        let frames = vm.execution_frames();
        let parent = frames.iter().rev().nth(1);
        let record = super::MarkRecord {
            label: label.as_str().to_string(),
            frame_len: frames.len(),
            last_prototype: frames.last().and_then(|frame| frame.prototype_id),
            last_return_ip: frames.last().and_then(|frame| match &frame.continuation {
                VmFrameContinuation::ResumeBytecode { return_ip } => Some(*return_ip),
                _ => None,
            }),
            parent_prototype: parent.and_then(|frame| frame.prototype_id),
            held: super::observe_held_parameter(vm, &frames),
        };
        self.witness
            .lock()
            .expect("root witness")
            .marks
            .push(record);
        Ok(CallOutcome::Return(CallReturn::one(Value::Int(0))))
    }

    fn note(&self, vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
        let [value] = args else {
            return Err(host_fault("note expects one value"));
        };
        let mut host = vm.host_context();
        let frame =
            current_frame(&mut host)?.ok_or_else(|| host_fault("note has no root frame"))?;
        let temp = rooted_table(&mut host, frame, 0)?;
        let formal = rooted_table(&mut host, frame, 1)?;
        let (table, carrier) = match value {
            Value::Array(_) => match classify(&mut host, value)? {
                heap::GuestClass::Table(table) => (Some(table), carrier_ptr(value)),
                other => {
                    return Err(self.fail(format!(
                        "note value is an array but not a live issued table: {other:?}"
                    )));
                }
            },
            _ => (None, None),
        };
        let table_index = table.and_then(|table| {
            self.witness
                .lock()
                .expect("root witness")
                .tables
                .iter()
                .position(|witness| witness.table == table)
        });
        let table_alive = match table {
            Some(table) => alive_table(&mut host, table)?,
            None => false,
        };
        let child = match table {
            Some(table) => heap::with_heap(&mut host, |heap| heap.named_edge(table, "child"))
                .map_err(heap_fault)?,
            None => None,
        };
        let child_alive = match child {
            Some(child) => alive_table(&mut host, child)?,
            None => false,
        };
        if table.is_some() && (!table_alive || (child.is_some() && !child_alive)) {
            return Err(self.fail(
                "note cannot use the issued payload: the table or its strong child is not live",
            ));
        }
        let rooted = table.is_some_and(|table| temp == Some(table) || formal == Some(table));
        self.witness.lock().expect("root witness").note = Some(NoteRecord {
            frame: frame.parts(),
            table,
            table_index,
            carrier,
            temp_empty: temp.is_none(),
            formal_table: formal,
            table_alive,
            child,
            child_alive,
            rooted,
        });
        Ok(CallOutcome::Return(CallReturn::one(Value::Bool(true))))
    }

    fn new_table(&self, vm: &mut Vm) -> vm::VmResult<CallOutcome> {
        let mut host = vm.host_context();
        let issued = heap::publish_guest(&mut host).map_err(heap_fault)?;
        let table = classify_table(&mut host, &issued)?;
        let witness = TableWitness {
            table,
            carrier: carrier_ptr(&issued)
                .ok_or_else(|| host_fault("issued table has no carrier"))?,
        };
        {
            let mut state = self.witness.lock().expect("root witness");
            state.tables.push(witness);
            if state.tables.len() > MAX_FIXTURE_TABLES {
                return Err(host_fault("the fixture created too many issued tables"));
            }
        }
        Ok(CallOutcome::Return(CallReturn::one(issued)))
    }

    fn set_child(&self, vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
        let [original_value, child_value] = args else {
            return Err(host_fault("setchild expects the original and the child"));
        };
        let mut host = vm.host_context();
        let original = classify_table(&mut host, original_value)?;
        let child = classify_table(&mut host, child_value)?;
        heap::with_heap(&mut host, |heap| {
            heap.set_named_edge(original, "child", child)
        })
        .map_err(heap_fault)?;
        Ok(CallOutcome::Return(CallReturn::one(Value::Int(0))))
    }

    fn enter(&self, vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
        let [width] = args else {
            return Err(host_fault("root enter expects one width"));
        };
        let width = u32_arg(width, "root frame width")?;
        let mut host = vm.host_context();
        let parent = current_frame(&mut host)?;
        let frame =
            heap::with_heap(&mut host, |heap| heap.enter_roots(width)).map_err(heap_fault)?;
        let depth = vm.call_depth();
        self.witness
            .lock()
            .expect("root witness")
            .enters
            .push(EnterRecord {
                child: frame.parts(),
                parent: parent.map(heap::RootFrameId::parts),
                core_depth: depth,
            });
        Ok(CallOutcome::Return(CallReturn::one(frame_token(frame))))
    }

    fn handoff(&self, vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
        let [completion, frame_value] = args else {
            return Err(host_fault(
                "root handoff expects completion and caller frame",
            ));
        };
        let own = decode_frame_token(frame_value)?;
        let caller_core_prototype = vm
            .execution_frames()
            .last()
            .and_then(|frame| frame.prototype_id);
        let core_depth = vm.call_depth();
        let (arm, index, observed) = {
            let state = self.witness.lock().expect("root witness");
            (
                state.arm,
                state.handoffs.len(),
                state
                    .tables
                    .iter()
                    .map(|witness| witness.table)
                    .collect::<Vec<_>>(),
            )
        };

        let mut host = vm.host_context();
        let child_frame = current_frame(&mut host)?
            .ok_or_else(|| host_fault("handoff has no child root frame"))?;
        let enter = {
            let state = self.witness.lock().expect("root witness");
            state
                .enters
                .iter()
                .find(|record| record.child == child_frame.parts())
                .cloned()
        }
        .ok_or_else(|| host_fault("callee root frame has no enter witness"))?;
        let child_parts = child_frame.parts();
        let own_parts = own.parts();
        if enter.parent != Some(own_parts) {
            return Err(self.fail(format!(
                "handoff parent {own_parts:?} is not the parent recorded at enter {:?}",
                enter.parent
            )));
        }
        if child_parts.0 != own_parts.0.saturating_add(1) {
            return Err(self.fail(format!(
                "callee frame {child_parts:?} is not adjacent to caller {own_parts:?}"
            )));
        }
        let parent_slot_before = rooted_table(&mut host, own, 0)?;
        if parent_slot_before.is_some() {
            return Err(self.fail("the reserved parent handoff root slot is not empty"));
        }

        let completion_class =
            decode_completion(completion).map_err(|message| self.fail(message))?;
        let mut payload_table = None;
        let mut payload_carrier = None;
        match &completion_class {
            CompletionClass::Normal { .. } => {
                let Value::Array(items) = completion else {
                    unreachable!("decode_completion accepted a non-array");
                };
                if items[1..]
                    .iter()
                    .any(|value| matches!(value, Value::Array(_)))
                {
                    return Err(
                        self.fail("a table value in a normal completion has no root in this slice")
                    );
                }
            }
            CompletionClass::Error { .. } => {
                let Value::Array(items) = completion else {
                    unreachable!("decode_completion accepted a non-array");
                };
                let payload = &items[1];
                if matches!(payload, Value::Array(_)) {
                    match classify(&mut host, payload)? {
                        heap::GuestClass::Table(table) => {
                            payload_table = Some(table);
                            payload_carrier = carrier_ptr(payload);
                        }
                        other => {
                            return Err(self.fail(format!(
                                "root handoff payload is not an issued table: {other:?}"
                            )));
                        }
                    }
                }
            }
        }

        let child_top_at_publish = current_frame(&mut host)? == Some(child_frame);
        if !child_top_at_publish {
            return Err(self.fail("the callee frame is not the top frame at the publish point"));
        }
        let skipped_publish = payload_table.is_some()
            && matches!(
                (arm, index),
                (Arm::OmitFirstPublish, 0)
                    | (Arm::OmitSecondPublish, 1)
                    | (Arm::OmitThirdPublish, 2)
            );
        let mut published = false;
        if let Some(table) = payload_table
            && !skipped_publish
        {
            heap::with_heap(&mut host, |heap| heap.write_root(own, 0, table))
                .map_err(heap_fault)?;
            published = true;
        }
        let parent_slot_after = rooted_table(&mut host, own, 0)?;
        if published && parent_slot_after != payload_table {
            return Err(self.fail("the parent temp root did not receive the payload"));
        }
        if skipped_publish && parent_slot_after.is_some() {
            return Err(self.fail("the omitted handoff still wrote a parent root"));
        }

        // Publishing (or the deliberate omission) is complete while the callee
        // frame is still the top frame. Only now retire it.
        heap::with_heap(&mut host, |heap| heap.leave_roots(child_frame)).map_err(heap_fault)?;
        let top_after = current_frame(&mut host)?;
        let top_after_parent = top_after == Some(own);

        let window = if index < 2 {
            Some(collect_window(&mut host, &observed)?)
        } else {
            None
        };

        self.witness
            .lock()
            .expect("root witness")
            .handoffs
            .push(HandoffRecord {
                index,
                child: child_parts,
                parent: own_parts,
                recorded_parent: enter.parent,
                parent_slot: 0,
                parent_slot_before_empty: parent_slot_before.is_none(),
                child_top_at_publish,
                completion: completion_class,
                skipped_publish,
                published,
                payload_table,
                payload_carrier,
                parent_slot_after,
                retired: top_after_parent,
                top_after_parent,
                caller_core_prototype,
                core_depth,
                window,
            });
        Ok(CallOutcome::Return(CallReturn::one(completion.clone())))
    }

    fn formal(&self, vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
        let [value, frame_value, temp, formal] = args else {
            return Err(host_fault(
                "root formal expects value, frame, temp and formal slots",
            ));
        };
        let own = decode_frame_token(frame_value)?;
        let temp = u32_arg(temp, "temp root slot")?;
        let formal = u32_arg(formal, "formal root slot")?;
        let mut host = vm.host_context();
        let current = current_frame(&mut host)?;
        if current != Some(own) {
            return Err(self.fail("the formal receiver is not the current parent frame"));
        }
        let temp_before = rooted_table(&mut host, own, temp)?;
        let formal_before = rooted_table(&mut host, own, formal)?;
        let value_kind = typed_value(value);
        let mut value_table = None;
        let mut rooted = false;
        match value {
            Value::Array(_) => match classify(&mut host, value)? {
                heap::GuestClass::Table(table) => {
                    value_table = Some(table);
                    // The table may only be adopted from the exact reserved
                    // temp root. A missing publish (for example an omitted
                    // third handoff) is a refusal here; writing the formal
                    // root anyway would fabricate the root the handoff never
                    // published.
                    let Some(temp_table) = temp_before else {
                        let temp_after = rooted_table(&mut host, own, temp)?;
                        let formal_after = rooted_table(&mut host, own, formal)?;
                        let reason = "the formal receive requires the exact reserved temp root before it may write the formal root";
                        self.witness
                            .lock()
                            .expect("root witness")
                            .formal_refusals
                            .push(FormalRefusal {
                                site: "formal_receive",
                                frame: own.parts(),
                                temp_slot: temp,
                                formal_slot: formal,
                                temp_before,
                                formal_before,
                                value: value_kind,
                                table: Some(table),
                                carrier: carrier_ptr(value),
                                carrier_value: Some(value.clone()),
                                temp_after,
                                formal_after,
                                frame_retained: current == Some(own),
                                reason: reason.to_string(),
                            });
                        return Err(self.fail(reason));
                    };
                    if temp_table != table {
                        return Err(
                            self.fail("the formal receive does not match the reserved temp root")
                        );
                    }
                    if formal_before.is_some() {
                        return Err(self.fail("the reserved formal root slot is not empty"));
                    }
                    heap::with_heap(&mut host, |heap| heap.write_root(own, formal, table))
                        .map_err(heap_fault)?;
                    heap::with_heap(&mut host, |heap| heap.clear_root(own, temp))
                        .map_err(heap_fault)?;
                    rooted = true;
                }
                other => {
                    return Err(self.fail(format!(
                        "the protected receive is not an issued table: {other:?}"
                    )));
                }
            },
            _ => {
                if temp_before.is_some() {
                    return Err(
                        self.fail("the formal receive did not consume the reserved temp root")
                    );
                }
            }
        }
        let temp_after = rooted_table(&mut host, own, temp)?;
        let formal_after = rooted_table(&mut host, own, formal)?;
        self.witness
            .lock()
            .expect("root witness")
            .formals
            .push(FormalRecord {
                frame: own.parts(),
                temp_slot: temp,
                formal_slot: formal,
                temp_before,
                formal_before,
                value: value_kind,
                value_table,
                rooted,
                temp_after,
                formal_after,
            });
        Ok(CallOutcome::Return(CallReturn::one(Value::Int(0))))
    }

    fn discard(&self, vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
        let [sequence, frame_value, temp] = args else {
            return Err(host_fault(
                "root discard expects sequence, frame and temp slot",
            ));
        };
        let own = decode_frame_token(frame_value)?;
        let temp = u32_arg(temp, "temp root slot")?;
        let formal_slot = 1u32;
        let mut host = vm.host_context();
        let current = current_frame(&mut host)?;
        if current != Some(own) {
            return Err(self.fail("the discard receiver is not the current parent frame"));
        }
        let temp_before = rooted_table(&mut host, own, temp)?;
        let formal_before = rooted_table(&mut host, own, formal_slot)?;
        let completion = decode_completion(sequence).map_err(|message| self.fail(message))?;
        let mut payload_table = None;
        let mut payload_carrier = None;
        let mut cleared = false;
        match &completion {
            CompletionClass::Normal { .. } => {
                if temp_before.is_some() {
                    return Err(self.fail("the discarded protected result still holds a temp root"));
                }
            }
            CompletionClass::Error { .. } => {
                let Value::Array(items) = sequence else {
                    unreachable!("decode_completion accepted a non-array");
                };
                let payload = &items[1];
                if matches!(payload, Value::Array(_)) {
                    match classify(&mut host, payload)? {
                        heap::GuestClass::Table(table) => {
                            payload_table = Some(table);
                            payload_carrier = carrier_ptr(payload);
                            if temp_before != Some(table) {
                                return Err(self
                                    .fail("the discarded payload is not the reserved temp root"));
                            }
                            heap::with_heap(&mut host, |heap| heap.clear_root(own, temp))
                                .map_err(heap_fault)?;
                            cleared = true;
                        }
                        other => {
                            return Err(self.fail(format!(
                                "the discarded payload is not an issued table: {other:?}"
                            )));
                        }
                    }
                } else if temp_before.is_some() {
                    return Err(
                        self.fail("the discarded scalar did not consume the reserved temp root")
                    );
                }
            }
        }
        let temp_after = rooted_table(&mut host, own, temp)?;
        let formal_after = rooted_table(&mut host, own, formal_slot)?;
        self.witness
            .lock()
            .expect("root witness")
            .discards
            .push(DiscardRecord {
                frame: own.parts(),
                temp_slot: temp,
                formal_slot,
                completion,
                payload_table,
                payload_carrier,
                temp_before,
                temp_after,
                formal_before,
                formal_after,
                cleared_exact_root: cleared,
            });
        Ok(CallOutcome::Return(CallReturn::one(Value::Int(0))))
    }

    fn finish(&self, vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
        let [completion, frame_value, formal] = args else {
            return Err(host_fault(
                "root finish expects completion, frame and formal slot",
            ));
        };
        let own = decode_frame_token(frame_value)?;
        let formal = u32_arg(formal, "formal root slot")?;
        let observed = {
            let state = self.witness.lock().expect("root witness");
            state
                .tables
                .iter()
                .map(|witness| witness.table)
                .collect::<Vec<_>>()
        };
        let mut host = vm.host_context();
        let current = current_frame(&mut host)?;
        if current != Some(own) {
            return Err(self.fail("the finishing chunk is not the current frame"));
        }
        let formal_before = rooted_table(&mut host, own, formal)?;
        let completion_class =
            decode_completion(completion).map_err(|message| self.fail(message))?;
        let mut uncaught = None;
        let mut retained = false;
        match &completion_class {
            CompletionClass::Normal { .. } => {
                let Value::Array(items) = completion else {
                    unreachable!("decode_completion accepted a non-array");
                };
                // A chunk exit has no root slot. Returning a table carrier
                // here would hand back a value that the retirement is about to
                // reclaim, so every table is refused before anything retires.
                if let Some(item) = items[1..]
                    .iter()
                    .find(|item| matches!(item, Value::Array(_)))
                {
                    let table = match classify(&mut host, item)? {
                        heap::GuestClass::Table(table) => Some(table),
                        _ => None,
                    };
                    let temp_before = rooted_table(&mut host, own, 0)?;
                    let top = current_frame(&mut host)?;
                    let reason = "the chunk finish refuses a table in a normal completion: a normal chunk exit has no root slot and must not return a carrier";
                    self.witness.lock().expect("root witness").finish = Some(FinishRecord {
                        frame: own.parts(),
                        formal_slot: formal,
                        formal_before,
                        completion: completion_class.clone(),
                        retired: false,
                        top_after: top.map(heap::RootFrameId::parts),
                        collection: None,
                        uncaught: None,
                        refused: Some(FinishRefusal {
                            site: "chunk_finish",
                            frame: own.parts(),
                            temp_slot: 0,
                            formal_slot: formal,
                            temp_before,
                            formal_before,
                            value: typed_value(item),
                            table,
                            carrier: carrier_ptr(item),
                            carrier_value: Some(item.clone()),
                            frame_retained: top == Some(own),
                            reason: reason.to_string(),
                        }),
                    });
                    return Err(self.fail(reason));
                }
            }
            CompletionClass::Error { .. } => {
                let Value::Array(items) = completion else {
                    unreachable!("decode_completion accepted a non-array");
                };
                let payload = &items[1];
                match payload {
                    Value::Array(_) => match classify(&mut host, payload)? {
                        heap::GuestClass::Table(table) => {
                            let temp = rooted_table(&mut host, own, 0)?;
                            let (root_slot, rooted_in_temp, rooted_in_formal) = if temp
                                == Some(table)
                            {
                                if let Some(other) = formal_before
                                    && other != table
                                {
                                    return Err(self.fail(
                                            "the uncaught payload is not the only table in the chunk roots",
                                        ));
                                }
                                (Some(0), true, formal_before == Some(table))
                            } else if formal_before == Some(table) {
                                (Some(formal), false, true)
                            } else if formal_before.is_none() {
                                heap::with_heap(&mut host, |heap| {
                                    heap.write_root(own, formal, table)
                                })
                                .map_err(heap_fault)?;
                                (Some(formal), false, true)
                            } else {
                                return Err(self.fail(
                                    "the uncaught payload table does not match the reserved roots",
                                ));
                            };
                            let alive_before_finish = alive_table(&mut host, table)?;
                            uncaught = Some(UncaughtRecord {
                                payload: typed_value(payload),
                                table: Some(table),
                                carrier: carrier_ptr(payload),
                                frame: Some(own.parts()),
                                root_slot,
                                rooted_in_temp,
                                rooted_in_formal,
                                frame_retained: true,
                                alive_before_finish,
                            });
                            retained = true;
                        }
                        other => {
                            return Err(self.fail(format!(
                                "the uncaught payload is not an issued table: {other:?}"
                            )));
                        }
                    },
                    _ => {
                        uncaught = Some(UncaughtRecord {
                            payload: typed_value(payload),
                            table: None,
                            carrier: None,
                            frame: None,
                            root_slot: None,
                            rooted_in_temp: false,
                            rooted_in_formal: false,
                            frame_retained: false,
                            alive_before_finish: false,
                        });
                    }
                }
            }
        }

        let stats_before =
            heap::with_heap(&mut host, |heap| Ok(heap.stats())).map_err(heap_fault)?;
        if retained {
            let top_after = current_frame(&mut host)?;
            self.witness.lock().expect("root witness").finish = Some(FinishRecord {
                frame: own.parts(),
                formal_slot: formal,
                formal_before,
                completion: completion_class,
                retired: false,
                top_after: top_after.map(heap::RootFrameId::parts),
                collection: None,
                uncaught,
                refused: None,
            });
            return Ok(CallOutcome::Return(CallReturn::one(completion.clone())));
        }

        heap::with_heap(&mut host, |heap| heap.leave_roots(own)).map_err(heap_fault)?;
        let top_after = current_frame(&mut host)?;
        let orphan_value = heap::publish_guest(&mut host).map_err(heap_fault)?;
        let orphan_table = classify_table(&mut host, &orphan_value)?;
        let orphan = TableWitness {
            table: orphan_table,
            carrier: carrier_ptr(&orphan_value)
                .ok_or_else(|| host_fault("final orphan has no carrier"))?,
        };
        let orphan_alive_before = alive_table(&mut host, orphan_table)?;
        let report = heap::collect(&mut host).map_err(heap_fault)?;
        let stats_after =
            heap::with_heap(&mut host, |heap| Ok(heap.stats())).map_err(heap_fault)?;
        let mut tables = Vec::with_capacity(observed.len());
        for table in &observed {
            tables.push(TableAlive {
                table: *table,
                before: true,
                after: alive_table(&mut host, *table)?,
            });
        }
        let orphan_alive_after = alive_table(&mut host, orphan_table)?;
        self.witness.lock().expect("root witness").finish = Some(FinishRecord {
            frame: own.parts(),
            formal_slot: formal,
            formal_before,
            completion: completion_class,
            retired: top_after.is_none(),
            top_after: top_after.map(heap::RootFrameId::parts),
            collection: Some(FinishCollection {
                tables,
                orphan,
                orphan_alive_before,
                orphan_alive_after,
                collections_before: stats_before.collections,
                collections_after: stats_after.collections,
                report,
            }),
            uncaught,
            refused: None,
        });
        Ok(CallOutcome::Return(CallReturn::one(completion.clone())))
    }
}

impl HostFunction for RootHost {
    fn call(&mut self, vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
        match self.op {
            RootOp::Mark => self.mark(vm, args),
            RootOp::Note => self.note(vm, args),
            RootOp::NewTable => self.new_table(vm),
            RootOp::SetChild => self.set_child(vm, args),
            RootOp::Enter => self.enter(vm, args),
            RootOp::Handoff => self.handoff(vm, args),
            RootOp::Formal => self.formal(vm, args),
            RootOp::Discard => self.discard(vm, args),
            RootOp::Finish => self.finish(vm, args),
        }
    }
}

fn bind_hosts(vm: &mut Vm, witness: &Arc<Mutex<RootWitness>>) {
    for (name, op) in [
        ("mark", RootOp::Mark),
        ("note", RootOp::Note),
        ("newtable", RootOp::NewTable),
        ("setchild", RootOp::SetChild),
        ("lua_root_enter", RootOp::Enter),
        ("lua_root_handoff", RootOp::Handoff),
        ("lua_root_formal", RootOp::Formal),
        ("lua_root_discard", RootOp::Discard),
        ("lua_root_finish", RootOp::Finish),
    ] {
        vm.bind_function(
            name,
            Box::new(RootHost {
                op,
                witness: Arc::clone(witness),
            }),
        );
    }
    vm.bind_function("fault", Box::new(RootFaultHost));
}

/// The real host fault arm. It is a normal host import in the same catalog,
/// and its `VmError` must abort the VM instead of becoming a guest completion.
struct RootFaultHost;

impl HostFunction for RootFaultHost {
    fn call(&mut self, _vm: &mut Vm, _args: &[Value]) -> vm::VmResult<CallOutcome> {
        Err(host_fault("completion host fault"))
    }
}

fn drive(fixture: &str, arm: Arm) -> RootRun {
    drive_mode(fixture, arm, RunMode::Normal)
}

fn drive_mode(fixture: &str, arm: Arm, mode: RunMode) -> RootRun {
    let path = fixture_path(fixture);
    let compiled = vm::compile_source_file_with_options(&path, root_options())
        .unwrap_or_else(|error| panic!("{fixture}: root handoff compile failed: {error:?}"));
    let call_sites = decoded_call_sites(&compiled.program);
    let mut vm = Vm::new_with_jit_config(
        compiled.program,
        vm::JitConfig {
            enabled: false,
            hot_loop_threshold: 8,
            max_trace_len: 256,
        },
    );
    let witness = Arc::new(Mutex::new(RootWitness {
        arm,
        tables: Vec::new(),
        enters: Vec::new(),
        handoffs: Vec::new(),
        formals: Vec::new(),
        formal_refusals: Vec::new(),
        discards: Vec::new(),
        finish: None,
        note: None,
        marks: Vec::new(),
        failures: Vec::new(),
    }));
    bind_hosts(&mut vm, &witness);
    match mode {
        RunMode::Normal => {
            vm.set_fuel_check_interval(FUEL_CHECK_INTERVAL)
                .unwrap_or_else(|error| panic!("{fixture}: fuel interval: {error}"));
            vm.set_fuel(FUEL_BUDGET);
        }
        RunMode::Fuel(budget) => {
            vm.set_fuel_check_interval(FUEL_CHECK_INTERVAL)
                .unwrap_or_else(|error| panic!("{fixture}: fuel interval: {error}"));
            vm.set_fuel(budget);
        }
        RunMode::Epoch(delta) => {
            vm.set_epoch_check_interval(FUEL_CHECK_INTERVAL)
                .unwrap_or_else(|error| panic!("{fixture}: epoch interval: {error}"));
            vm.set_epoch_deadline(delta)
                .unwrap_or_else(|error| panic!("{fixture}: epoch deadline: {error}"));
        }
    }
    let fuel_budget = match mode {
        RunMode::Normal => Some(FUEL_BUDGET),
        RunMode::Fuel(budget) => Some(budget),
        RunMode::Epoch(_) => None,
    };
    let status = vm.run();
    let fuel_remaining = vm.get_fuel();
    let last_yield_reason = vm.last_yield_reason();
    let epoch_deadline = vm.epoch_deadline();
    let stack = vm.stack().to_vec();
    let exit = consume_completion_exit(status, stack.clone());
    let exit_carriers = inspect_exit_carriers(&mut vm, &exit);
    let heap_after_run = heap::with_heap(&mut vm.host_context(), |heap| Ok(heap.stats())).ok();
    let witness = witness.lock().expect("root witness lock").clone();
    let boundary = resolve_uncaught_boundary(&mut vm, &witness, &exit);
    let refusal = resolve_refusal_boundary(&mut vm, &witness);
    eprintln!(
        "LUA_ROOT_HANDOFF fixture={fixture} arm={arm:?} fuel_budget={fuel_budget:?} fuel_remaining={fuel_remaining:?} epoch_deadline={epoch_deadline:?} yield_reason={last_yield_reason:?} exit={exit:?} exit_carriers={exit_carriers:?} heap_after_run={heap_after_run:?} table_count={} enters={:?} handoffs={:?} formals={:?} formal_refusals={:?} discards={:?} finish={:?} note={:?} boundary={boundary:?} refusal={refusal:?} failures={:?}",
        witness.tables.len(),
        witness.enters,
        witness.handoffs,
        witness.formals,
        witness.formal_refusals,
        witness.discards,
        witness.finish,
        witness.note,
        witness.failures
    );
    RootRun {
        stack,
        exit,
        fuel_budget,
        fuel_remaining,
        last_yield_reason,
        epoch_deadline,
        marks: witness.marks.clone(),
        call_sites,
        witness,
        boundary,
        exit_carriers,
        heap_after_run,
        refusal,
    }
}

/// Classifies every table carrier on the completion exit after `Vm::run`.
///
/// A carrier whose heap slot was swept classifies as `Unissued`; pointer
/// identity alone would not show that. This is a read-only observation on the
/// still-live scope heap.
fn inspect_exit_carriers(vm: &mut Vm, exit: &CompletionExit) -> Vec<ExitCarrier> {
    let values: &[Value] = match exit {
        CompletionExit::Values(values) => values,
        CompletionExit::Uncaught { payload } => std::slice::from_ref(payload),
        _ => return Vec::new(),
    };
    let mut host = vm.host_context();
    let mut carriers = Vec::new();
    for value in values {
        let Value::Array(_) = value else {
            continue;
        };
        let Some(carrier) = carrier_ptr(value) else {
            continue;
        };
        let Ok(class) = classify(&mut host, value) else {
            continue;
        };
        carriers.push(ExitCarrier { carrier, class });
    }
    carriers
}

/// Resolves the uncaught boundary against the still-live scope heap.
///
/// The finish host retained the chunk frame when the completion was an
/// uncaught issued table. This reads the payload with a root, records that
/// state, then performs the caller-side release and collection. The same
/// carrier must stop classifying as a table afterwards.
fn resolve_uncaught_boundary(
    vm: &mut Vm,
    witness: &RootWitness,
    exit: &CompletionExit,
) -> Option<UncaughtBoundary> {
    let finish = witness.finish.as_ref()?;
    let uncaught = finish.uncaught.as_ref()?;
    let CompletionExit::Uncaught { payload } = exit else {
        return None;
    };
    let mut host = vm.host_context();
    let mut boundary = UncaughtBoundary {
        table: uncaught.table,
        carrier: uncaught.carrier,
        live_root: uncaught.root_slot.is_some(),
        alive_at_boundary: false,
        carrier_is_table_at_boundary: false,
        frame: uncaught.frame,
        temp_after_run: None,
        formal_after_run: None,
        released: false,
        alive_after_release: false,
        carrier_is_table_after_release: false,
        report: None,
    };
    let Some(table) = uncaught.table else {
        return Some(boundary);
    };
    let Some(frame_parts) = uncaught.frame else {
        return Some(boundary);
    };
    let frame = heap::RootFrameId::from_parts(frame_parts.0, frame_parts.1);
    // Read the real frame slots after `Vm::run` instead of trusting the
    // finish record: the retained chunk frame must be the one that still
    // roots the payload.
    boundary.temp_after_run = rooted_table(&mut host, frame, 0).ok()?;
    boundary.formal_after_run = rooted_table(&mut host, frame, 1).ok()?;
    let before = classify(&mut host, payload).ok()?;
    boundary.alive_at_boundary = matches!(before, heap::GuestClass::Table(found) if found == table);
    boundary.carrier_is_table_at_boundary = matches!(before, heap::GuestClass::Table(_));
    heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).ok()?;
    boundary.released = true;
    let report = heap::collect(&mut host).ok()?;
    boundary.report = Some(report);
    let after = classify(&mut host, payload).ok()?;
    boundary.alive_after_release =
        matches!(after, heap::GuestClass::Table(found) if found == table);
    boundary.carrier_is_table_after_release = matches!(after, heap::GuestClass::Table(_));
    Some(boundary)
}

/// Resolves one refused protocol seam against the still-live scope heap.
///
/// Both refusals happen before any frame retirement, so after `Vm::run`
/// returns the chunk frame, its temp/formal slots and the offending carrier
/// are still real. This reads that state, records that the refusing seam kept
/// the chunk frame open, then performs the caller-side release and collection.
/// The refusing host neither retires nor collects: the caller is responsible
/// for that cleanup, and the same carrier must stop classifying as a table.
fn resolve_refusal_boundary(vm: &mut Vm, witness: &RootWitness) -> Option<RefusalBoundary> {
    struct Refused {
        site: &'static str,
        frame: (u32, u32),
        temp_slot: u32,
        formal_slot: u32,
        table: Option<heap::TableRef>,
        carrier_value: Option<Value>,
    }
    let refused = if let Some(refusal) = witness
        .finish
        .as_ref()
        .and_then(|finish| finish.refused.as_ref())
    {
        Refused {
            site: refusal.site,
            frame: refusal.frame,
            temp_slot: refusal.temp_slot,
            formal_slot: refusal.formal_slot,
            table: refusal.table,
            carrier_value: refusal.carrier_value.clone(),
        }
    } else {
        let refusal = witness.formal_refusals.last()?;
        Refused {
            site: refusal.site,
            frame: refusal.frame,
            temp_slot: refusal.temp_slot,
            formal_slot: refusal.formal_slot,
            table: refusal.table,
            carrier_value: refusal.carrier_value.clone(),
        }
    };
    let frame = heap::RootFrameId::from_parts(refused.frame.0, refused.frame.1);
    let child = witness.tables.iter().map(|table| table.table).nth(1);
    let mut host = vm.host_context();
    let top = current_frame(&mut host).ok()?;
    let mut boundary = RefusalBoundary {
        site: refused.site,
        frame: refused.frame,
        top_after_run: top.map(heap::RootFrameId::parts),
        frame_retained: top == Some(frame),
        temp_after_run: rooted_table(&mut host, frame, refused.temp_slot).ok()?,
        formal_after_run: rooted_table(&mut host, frame, refused.formal_slot).ok()?,
        carrier_is_table_before_release: false,
        table_alive_before_release: false,
        child,
        child_alive_before_release: false,
        released_by_caller: false,
        carrier_is_table_after_release: false,
        table_alive_after_release: false,
        child_alive_after_release: false,
        report: None,
    };
    if let Some(table) = refused.table {
        if let Some(carrier) = refused.carrier_value.as_ref() {
            boundary.carrier_is_table_before_release = matches!(
                classify(&mut host, carrier).ok()?,
                heap::GuestClass::Table(found) if found == table
            );
        }
        boundary.table_alive_before_release = alive_table(&mut host, table).ok()?;
    }
    if let Some(child) = child {
        boundary.child_alive_before_release = alive_table(&mut host, child).ok()?;
    }
    if !boundary.frame_retained {
        return Some(boundary);
    }
    heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).ok()?;
    boundary.released_by_caller = true;
    boundary.report = Some(heap::collect(&mut host).ok()?);
    if let Some(table) = refused.table {
        if let Some(carrier) = refused.carrier_value.as_ref() {
            boundary.carrier_is_table_after_release = matches!(
                classify(&mut host, carrier).ok()?,
                heap::GuestClass::Table(found) if found == table
            );
        }
        boundary.table_alive_after_release = alive_table(&mut host, table).ok()?;
    }
    if let Some(child) = child {
        boundary.child_alive_after_release = alive_table(&mut host, child).ok()?;
    }
    Some(boundary)
}

fn labels(run: &RootRun) -> Vec<&str> {
    run.marks.iter().map(|mark| mark.label.as_str()).collect()
}

fn mark_prototype(run: &RootRun, label: &str) -> Option<u32> {
    run.marks
        .iter()
        .find(|mark| mark.label == label)
        .and_then(|mark| mark.last_prototype)
}

fn table_witness(run: &RootRun, index: usize) -> TableWitness {
    *run.witness
        .tables
        .get(index)
        .unwrap_or_else(|| panic!("{index}: missing table witness"))
}

fn completed<'a>(name: &str, run: &'a RootRun) -> &'a [Value] {
    match &run.exit {
        CompletionExit::Values(values) => values,
        other => panic!("{name}: expected a value exit, got {other:?}"),
    }
}

#[test]
fn positive_handoff_keeps_one_issued_table_through_all_three_paths() {
    let run = drive(POSITIVE_FIXTURE, Arm::Positive);
    let witness = &run.witness;
    assert_eq!(
        witness.tables.len(),
        2,
        "the fixture issues exactly two tables"
    );
    let original = table_witness(&run, 0);
    let child = table_witness(&run, 1);
    assert_ne!(child.table, original.table, "the child is a distinct node");
    assert_eq!(
        labels(&run),
        ["outer", "relay", "middle", "leaf", "outer-tail"],
        "the ordinary tail is skipped and the outer tail runs"
    );
    assert_eq!(witness.handoffs.len(), 3, "ordinary, tail, protected");
    assert_eq!(witness.formals.len(), 1, "pcall must adopt its formal");
    assert!(
        witness.finish.is_some(),
        "the chunk must retire and collect"
    );
    assert!(
        witness.failures.is_empty(),
        "positive arm faults: {:?}",
        witness.failures
    );

    super::assert_dynamic_call_witness(&run.as_p0_run(), "leaf", 0);
    super::assert_dynamic_call_witness(&run.as_p0_run(), "middle", 1);
    assert_eq!(
        mark_prototype(&run, "middle"),
        witness.handoffs[0].caller_core_prototype
    );
    assert_eq!(
        mark_prototype(&run, "relay"),
        witness.handoffs[1].caller_core_prototype
    );
    assert_eq!(
        mark_prototype(&run, "outer"),
        witness.handoffs[2].caller_core_prototype
    );

    let [first, second, third] = witness.handoffs.as_slice() else {
        panic!("exactly three handoffs");
    };
    let relay_mark = run
        .marks
        .iter()
        .find(|mark| mark.label == "relay")
        .expect("relay marker");
    let protected_sites: Vec<_> = run
        .call_sites
        .iter()
        .filter(|site| {
            site.owner == relay_mark.parent_prototype
                && Some(site.return_ip) == relay_mark.last_return_ip
                && site.argc == 2
        })
        .collect();
    assert_eq!(
        protected_sites.len(),
        1,
        "the pcall target call must be one decoded CallValue: {:?} relay={relay_mark:?}",
        run.call_sites
    );
    assert_eq!(
        protected_sites[0].ip + 2,
        protected_sites[0].return_ip,
        "the protected CallValue carries one argc operand byte"
    );
    for (handoff, what) in [
        (first, "leaf -> middle"),
        (second, "middle -> relay"),
        (third, "relay -> pcall"),
    ] {
        assert_eq!(handoff.recorded_parent, Some(handoff.parent), "{what}");
        assert_eq!(
            handoff.child.0,
            handoff.parent.0 + 1,
            "{what}: frames must be adjacent"
        );
        assert!(handoff.parent_slot_before_empty, "{what}: reserved slot");
        assert!(
            handoff.child_top_at_publish,
            "{what}: publish precedes the callee retire"
        );
        assert_eq!(handoff.parent_slot, 0, "{what}: temp slot");
        assert_eq!(
            handoff.completion,
            CompletionClass::Error {
                payload: TypedValue::Array
            },
            "{what}: exact [false, payload]"
        );
        assert!(handoff.published, "{what}: publish");
        assert_eq!(
            handoff.payload_table,
            Some(original.table),
            "{what}: issued TableRef"
        );
        assert_eq!(
            handoff.payload_carrier,
            Some(original.carrier),
            "{what}: issued carrier identity"
        );
        assert_eq!(
            handoff.parent_slot_after,
            Some(original.table),
            "{what}: parent temp root"
        );
        assert!(
            handoff.retired && handoff.top_after_parent,
            "{what}: retire"
        );
    }
    assert_eq!(
        second.child, first.parent,
        "frame continuity leaf -> middle"
    );
    assert_eq!(
        third.child, second.parent,
        "frame continuity middle -> relay"
    );
    assert_eq!(
        witness.enters[0].parent, None,
        "the chunk is the bottom frame"
    );
    assert_eq!(witness.enters[1].parent, Some(witness.enters[0].child));
    assert_eq!(witness.enters[2].parent, Some(witness.enters[1].child));
    assert_eq!(witness.enters[3].parent, Some(witness.enters[2].child));
    assert!(
        witness
            .enters
            .windows(2)
            .all(|pair| pair[0].core_depth < pair[1].core_depth),
        "each nested Lua function enters at a deeper Core depth: {:?}",
        witness.enters
    );
    assert_eq!(first.index, 0);
    assert_eq!(second.index, 1);
    assert_eq!(third.index, 2);
    assert!(
        first.core_depth > second.core_depth && second.core_depth > third.core_depth,
        "the handoff runs in middle, then relay, then the chunk: {:?}",
        [first.core_depth, second.core_depth, third.core_depth]
    );

    for window in [
        first.window.as_ref().expect("first window"),
        second.window.as_ref().expect("second window"),
    ] {
        assert!(window.orphan_alive_before, "fresh orphan must start live");
        assert_ne!(window.orphan.table, original.table);
        assert_ne!(window.orphan.table, child.table);
        assert_eq!(window.tables.len(), 2);
        assert!(
            window
                .tables
                .iter()
                .all(|table| table.before && table.after),
            "the published payload and its strong child stay alive: {:?}",
            window.tables
        );
        assert!(
            !window.orphan_alive_after,
            "the fresh orphan must be swept in this window"
        );
        assert_eq!(
            window.collections_after,
            window.collections_before + 1,
            "one real full collection per window"
        );
        assert_eq!(
            window.report.live_nodes, 2,
            "only the rooted original and its strong child survive"
        );
        assert!(window.report.marked_nodes >= 2, "the mark saw the roots");
    }
    assert!(
        third.window.is_none(),
        "no extra window after the protected receive"
    );

    let formal = witness.formals.first().expect("formal record");
    assert_eq!(formal.frame, witness.enters[0].child);
    assert_eq!(formal.temp_slot, 0);
    assert_eq!(formal.formal_slot, 1);
    assert_eq!(formal.temp_before, Some(original.table));
    assert_eq!(formal.formal_before, None);
    assert_eq!(formal.value, TypedValue::Array);
    assert_eq!(formal.value_table, Some(original.table));
    assert!(formal.rooted);
    assert_eq!(formal.temp_after, None, "the temp root is cleared");
    assert_eq!(
        formal.formal_after,
        Some(original.table),
        "the formal root holds it"
    );

    let note = witness.note.as_ref().expect("note record");
    assert_eq!(note.frame, witness.enters[0].child);
    assert_eq!(note.table, Some(original.table));
    assert_eq!(note.table_index, Some(0));
    assert_eq!(note.carrier, Some(original.carrier));
    assert!(
        note.table_alive,
        "the original table must still be a usable issued table in note"
    );
    assert_eq!(
        note.child,
        Some(child.table),
        "note must read the strong child edge"
    );
    assert!(
        note.child_alive,
        "the strong child must still be a usable table in note"
    );
    assert!(
        note.temp_empty && note.formal_table == Some(original.table),
        "the formal root still holds the payload in note"
    );
    assert!(note.rooted, "the payload is in a reserved root in note");

    let finish = witness.finish.as_ref().expect("finish record");
    assert_eq!(
        finish.formal_slot, 1,
        "the formal slot is the second reserved slot"
    );
    assert_eq!(finish.formal_before, Some(original.table));
    assert_eq!(
        finish.completion,
        CompletionClass::Normal {
            arity: 1,
            values: vec![TypedValue::Bool(false)]
        },
        "the positive chunk returns the scalar pcall status only"
    );
    assert!(
        finish.retired,
        "the receiving chunk frame is really retired"
    );
    assert_eq!(finish.top_after, None, "no Lua root frame remains");
    assert!(
        finish.refused.is_none(),
        "a scalar chunk completion is not refused"
    );
    let collection = finish.collection.as_ref().expect("final collection");
    assert_eq!(collection.tables.len(), 2);
    assert!(
        collection.tables.iter().all(|table| !table.after),
        "the last formal root release collects the original and child"
    );
    assert!(collection.orphan_alive_before && !collection.orphan_alive_after);
    assert_ne!(collection.orphan.table, original.table);
    assert_eq!(
        collection.report.live_nodes, 0,
        "no heap node survives the last root"
    );
    assert_eq!(
        collection.collections_after,
        collection.collections_before + 1
    );
    assert!(
        run.heap_after_run
            .is_some_and(|stats| stats.live_nodes == 0),
        "the scope heap is empty after the chunk retirement: {:?}",
        run.heap_after_run
    );

    let values = completed(POSITIVE_FIXTURE, &run);
    assert_eq!(
        values.len(),
        1,
        "the chunk returns only the scalar pcall status"
    );
    assert_eq!(values[0], Value::Bool(false));
    assert!(
        run.exit_carriers.is_empty(),
        "a normal chunk exit carries no table: {:?}",
        run.exit_carriers
    );
    assert!(
        run.exit_carriers.iter().all(|carrier| {
            carrier.carrier != original.carrier
                && !matches!(carrier.class, heap::GuestClass::Table(_))
        }),
        "the scalar exit must not return the issued carrier or any live table"
    );
    assert!(run.boundary.is_none() && run.refusal.is_none());
}

#[test]
fn omitting_the_first_publish_collects_the_payload_before_middle_returns() {
    let run = drive(POSITIVE_FIXTURE, Arm::OmitFirstPublish);
    let witness = &run.witness;
    assert_eq!(witness.tables.len(), 2);
    let original = table_witness(&run, 0);
    let child = table_witness(&run, 1);
    assert_eq!(labels(&run), ["outer", "relay", "middle", "leaf"]);
    assert_eq!(
        witness.handoffs.len(),
        1,
        "the stale payload stops the next handoff: {:?}",
        witness.failures
    );
    let handoff = &witness.handoffs[0];
    assert!(
        handoff.skipped_publish,
        "this arm omits exactly this publish"
    );
    assert!(!handoff.published);
    assert!(handoff.parent_slot_before_empty);
    assert!(
        handoff.child_top_at_publish,
        "the publish decision is made while the leaf frame is still top"
    );
    assert_eq!(handoff.parent_slot_after, None, "no temp root was written");
    assert!(
        handoff.retired && handoff.top_after_parent,
        "the retire is kept"
    );
    assert_eq!(
        handoff.completion,
        CompletionClass::Error {
            payload: TypedValue::Array
        }
    );
    assert_eq!(handoff.payload_table, Some(original.table));
    assert_eq!(handoff.payload_carrier, Some(original.carrier));
    super::assert_dynamic_call_witness(&run.as_p0_run(), "leaf", 0);
    assert_eq!(
        handoff.caller_core_prototype,
        mark_prototype(&run, "middle"),
        "the window runs in the actual Core caller of the leaf call"
    );

    let window = handoff.window.as_ref().expect("first window");
    assert!(window.orphan_alive_before);
    assert_ne!(window.orphan.table, original.table);
    assert_eq!(window.tables.len(), 2);
    assert!(
        window.tables.iter().all(|table| !table.after),
        "the unpublished payload and the child are collected"
    );
    assert!(
        !window.orphan_alive_after,
        "the fresh orphan is collected in this sweep"
    );
    assert_eq!(window.collections_after, window.collections_before + 1);
    assert_eq!(
        window.report.live_nodes, 0,
        "the original graph and the fresh orphan are both swept"
    );
    let _ = child;

    let CompletionExit::VmFailure(VmError::HostError(message)) = &run.exit else {
        panic!(
            "a collected payload must stop the next handoff: {:?}",
            run.exit
        );
    };
    assert!(
        message.contains("not an issued table"),
        "the refusal must be the stale payload: {message}"
    );
    assert!(
        witness.failures.iter().any(|failure| failure == message),
        "the refused handoff is recorded: {:?}",
        witness.failures
    );
}

#[test]
fn omitting_the_second_publish_collects_after_middle_and_before_relay_returns() {
    let run = drive(POSITIVE_FIXTURE, Arm::OmitSecondPublish);
    let witness = &run.witness;
    assert_eq!(witness.tables.len(), 2);
    let original = table_witness(&run, 0);
    let child = table_witness(&run, 1);
    assert_eq!(labels(&run), ["outer", "relay", "middle", "leaf"]);
    assert_eq!(
        witness.handoffs.len(),
        2,
        "the second window is reached before the refusal: {:?}",
        witness.failures
    );
    let first = &witness.handoffs[0];
    assert!(first.published, "the first handoff still publishes");
    super::assert_dynamic_call_witness(&run.as_p0_run(), "leaf", 0);
    super::assert_dynamic_call_witness(&run.as_p0_run(), "middle", 1);
    assert_eq!(
        first.caller_core_prototype,
        mark_prototype(&run, "middle"),
        "the first window runs in middle"
    );
    let first_window = first.window.as_ref().expect("first window control");
    assert_eq!(first_window.tables.len(), 2);
    assert!(
        first_window.tables.iter().all(|table| table.after),
        "the second control's first window already succeeded"
    );
    assert!(first_window.orphan_alive_before && !first_window.orphan_alive_after);
    assert_ne!(first_window.orphan.table, original.table);
    assert_eq!(
        first_window.collections_after,
        first_window.collections_before + 1
    );
    assert_eq!(
        first_window.report.live_nodes, 2,
        "the first window still sees the rooted original and child"
    );

    let second = &witness.handoffs[1];
    assert_eq!(
        second.child, first.parent,
        "middle is the second window's callee"
    );
    assert_eq!(
        second.caller_core_prototype,
        mark_prototype(&run, "relay"),
        "the second window runs in the actual Core caller of the middle call"
    );
    assert!(
        second.skipped_publish && !second.published,
        "this arm omits exactly the second publish"
    );
    assert!(second.parent_slot_before_empty);
    assert!(second.child_top_at_publish);
    assert_eq!(second.parent_slot_after, None);
    assert!(second.retired && second.top_after_parent);
    assert_eq!(second.payload_table, Some(original.table));
    assert_eq!(second.payload_carrier, Some(original.carrier));
    let second_window = second.window.as_ref().expect("second window");
    assert!(second_window.orphan_alive_before);
    assert_ne!(second_window.orphan.table, original.table);
    assert_ne!(second_window.orphan.table, child.table);
    assert!(
        second_window.tables.iter().all(|table| !table.after),
        "the unpublished payload and the child are collected in the second window"
    );
    assert!(!second_window.orphan_alive_after);
    assert_eq!(
        second_window.collections_after,
        second_window.collections_before + 1
    );
    assert_eq!(
        second_window.report.live_nodes, 0,
        "the original graph and the fresh orphan are both swept"
    );

    let CompletionExit::VmFailure(VmError::HostError(message)) = &run.exit else {
        panic!(
            "a collected payload must stop the protected handoff: {:?}",
            run.exit
        );
    };
    assert!(
        message.contains("not an issued table"),
        "the refusal must be the stale payload: {message}"
    );
    assert!(witness.note.is_none());
    let _ = original;
    let _ = child;
}

/// The frozen original `root-handoff.lua` bytes are kept as a separate
/// fixture. It really reaches the chunk finish through the first two
/// collection windows, the third (protected) handoff and the formal root
/// adoption, so this refusal is the chunk exit and not a sub-call refusal.
#[test]
fn root_normal_chunk_table_exit_is_refused_at_the_finish() {
    let run = drive("root-chunk-table-exit.lua", Arm::Positive);
    let witness = &run.witness;
    assert_eq!(
        labels(&run),
        ["outer", "relay", "middle", "leaf", "outer-tail"]
    );
    assert_eq!(witness.tables.len(), 2);
    let original = table_witness(&run, 0);
    let child = table_witness(&run, 1);
    assert_eq!(
        witness.handoffs.len(),
        3,
        "two ordinary windows, the protected handoff: {:?}",
        witness.failures
    );
    for (handoff, what) in [
        (&witness.handoffs[0], "leaf -> middle"),
        (&witness.handoffs[1], "middle -> relay"),
    ] {
        assert!(handoff.published && !handoff.skipped_publish, "{what}");
        assert!(
            handoff.child_top_at_publish && handoff.retired && handoff.top_after_parent,
            "{what}: the publish precedes the real retire"
        );
        assert_eq!(handoff.parent_slot_after, Some(original.table), "{what}");
        let window = handoff.window.as_ref().expect("collection window");
        assert!(
            window.orphan_alive_before && !window.orphan_alive_after,
            "{what}: the fresh orphan is really swept"
        );
        assert_eq!(window.tables.len(), 2, "{what}");
        assert!(
            window
                .tables
                .iter()
                .all(|table| table.before && table.after),
            "{what}: the payload graph survives the window"
        );
        assert_eq!(window.report.live_nodes, 2, "{what}");
        assert_eq!(
            window.collections_after,
            window.collections_before + 1,
            "{what}: one real collection"
        );
    }
    let third = &witness.handoffs[2];
    assert_eq!(
        third.completion,
        CompletionClass::Error {
            payload: TypedValue::Array
        }
    );
    assert_eq!(third.payload_table, Some(original.table));
    assert_eq!(third.payload_carrier, Some(original.carrier));
    assert!(
        third.published && third.retired && third.top_after_parent,
        "the protected handoff really publishes and retires"
    );
    assert!(third.window.is_none(), "the third handoff adds no window");

    let formal = witness.formals.first().expect("formal record");
    assert_eq!(formal.temp_before, Some(original.table));
    assert_eq!(formal.formal_after, Some(original.table));
    assert!(formal.rooted && formal.temp_after.is_none());

    let note = witness.note.as_ref().expect("note record");
    assert_eq!(note.table, Some(original.table));
    assert_eq!(note.table_index, Some(0));
    assert_eq!(note.carrier, Some(original.carrier));
    assert!(
        note.table_alive && note.child == Some(child.table) && note.child_alive,
        "note verifies the payload graph before the chunk exit"
    );
    assert!(note.temp_empty && note.formal_table == Some(original.table));
    assert!(note.rooted);

    let finish = witness
        .finish
        .as_ref()
        .expect("the chunk finish must be reached");
    assert_eq!(
        finish.completion,
        CompletionClass::Normal {
            arity: 2,
            values: vec![TypedValue::Bool(false), TypedValue::Array]
        }
    );
    assert_eq!(finish.formal_before, Some(original.table));
    assert!(
        !finish.retired,
        "a refused chunk exit must not retire the chunk frame"
    );
    assert_eq!(
        finish.top_after,
        Some(finish.frame),
        "the refused chunk frame stays on top"
    );
    assert!(finish.collection.is_none() && finish.uncaught.is_none());
    let refusal = finish.refused.as_ref().expect("the table exit is refused");
    assert_eq!(refusal.site, "chunk_finish");
    assert_eq!(refusal.value, TypedValue::Array);
    assert_eq!(refusal.table, Some(original.table));
    assert_eq!(refusal.carrier, Some(original.carrier));
    assert_eq!(refusal.temp_before, None);
    assert_eq!(refusal.formal_before, Some(original.table));
    assert!(refusal.frame_retained);
    assert_eq!(
        refusal.reason,
        "the chunk finish refuses a table in a normal completion: a normal chunk exit has no root slot and must not return a carrier"
    );

    let CompletionExit::VmFailure(VmError::HostError(message)) = &run.exit else {
        panic!(
            "a table in a normal chunk completion must be refused, got {:?}",
            run.exit
        );
    };
    assert_eq!(
        message,
        "the chunk finish refuses a table in a normal completion: a normal chunk exit has no root slot and must not return a carrier"
    );
    assert!(
        witness.failures.iter().any(|failure| failure == message),
        "the refusal is recorded: {:?}",
        witness.failures
    );
    assert!(
        run.exit_carriers.is_empty(),
        "a refused exit returns no carrier at all: {:?}",
        run.exit_carriers
    );
    assert!(run.boundary.is_none());

    let boundary = run.refusal.as_ref().expect("refusal boundary");
    assert_eq!(boundary.site, "chunk_finish");
    assert_eq!(boundary.frame, finish.frame);
    assert!(
        boundary.frame_retained && boundary.top_after_run == Some(finish.frame),
        "the refusing host leaves the chunk frame to its caller"
    );
    assert_eq!(boundary.formal_after_run, Some(original.table));
    assert_eq!(boundary.temp_after_run, None);
    assert!(
        boundary.carrier_is_table_before_release
            && boundary.table_alive_before_release
            && boundary.child_alive_before_release,
        "the refused payload is still a usable table before the caller cleans up"
    );
    assert_eq!(boundary.child, Some(child.table));
    assert!(
        boundary.released_by_caller,
        "the caller releases the retained chunk frame"
    );
    let report = boundary.report.expect("caller-side collection");
    assert_eq!(
        report.live_nodes, 0,
        "the caller's collection reclaims the graph"
    );
    assert!(
        !boundary.carrier_is_table_after_release
            && !boundary.table_alive_after_release
            && !boundary.child_alive_after_release,
        "the reclaimed carrier is not a table"
    );
}

/// The third publish is the only thing this control changes: the protected
/// handoff retires the relay frame without writing the chunk temp root. Both
/// earlier windows complete and keep the table graph alive, so the receiver
/// must refuse instead of fabricating a formal root.
#[test]
fn third_publish_omission_is_refused_at_the_formal_receiver() {
    let run = drive(POSITIVE_FIXTURE, Arm::OmitThirdPublish);
    let witness = &run.witness;
    assert_eq!(labels(&run), ["outer", "relay", "middle", "leaf"]);
    assert_eq!(witness.tables.len(), 2);
    let original = table_witness(&run, 0);
    let child = table_witness(&run, 1);
    assert_eq!(
        witness.handoffs.len(),
        3,
        "the two ordinary handoffs still run: {:?}",
        witness.failures
    );
    assert!(
        witness.formals.is_empty(),
        "the receiver must refuse before recording an adoption"
    );
    assert!(
        witness.note.is_none(),
        "note must not run after the refusal"
    );
    assert!(
        witness.finish.is_none(),
        "the chunk finish must not run after the refusal"
    );
    for (handoff, what) in [
        (&witness.handoffs[0], "leaf -> middle"),
        (&witness.handoffs[1], "middle -> relay"),
    ] {
        assert!(handoff.published && !handoff.skipped_publish, "{what}");
        assert!(
            handoff.child_top_at_publish && handoff.retired && handoff.top_after_parent,
            "{what}"
        );
        assert_eq!(handoff.parent_slot_after, Some(original.table), "{what}");
        assert_eq!(handoff.payload_table, Some(original.table), "{what}");
        let window = handoff.window.as_ref().expect("collection window");
        assert!(
            window.orphan_alive_before && !window.orphan_alive_after,
            "{what}: the independent orphan is cleared"
        );
        assert_eq!(window.tables.len(), 2, "{what}");
        assert!(
            window
                .tables
                .iter()
                .all(|table| table.before && table.after),
            "{what}: the original and its child stay alive"
        );
        assert_eq!(window.report.live_nodes, 2, "{what}");
        assert_eq!(
            window.collections_after,
            window.collections_before + 1,
            "{what}"
        );
    }
    let third = &witness.handoffs[2];
    assert!(
        third.skipped_publish && !third.published,
        "this control changes only the third publish"
    );
    assert!(third.parent_slot_before_empty);
    assert!(
        third.child_top_at_publish,
        "the decision happens while the relay frame is still top"
    );
    assert_eq!(
        third.parent_slot_after, None,
        "the third temp root is not written"
    );
    assert!(
        third.retired && third.top_after_parent,
        "the third handoff really retires the relay frame"
    );
    assert_eq!(
        third.parent, witness.enters[0].child,
        "the chunk frame is the parent of the third handoff"
    );
    assert_eq!(
        third.child, witness.enters[1].child,
        "the relay frame is the retired child"
    );
    assert!(
        third.window.is_none(),
        "the third handoff adds no collection"
    );
    assert_eq!(third.payload_table, Some(original.table));
    assert_eq!(third.payload_carrier, Some(original.carrier));

    let refusal = witness
        .formal_refusals
        .first()
        .expect("the formal receiver refuses");
    assert_eq!(refusal.site, "formal_receive");
    assert_eq!(refusal.frame, witness.enters[0].child);
    assert_eq!(refusal.temp_before, None);
    assert_eq!(refusal.formal_before, None);
    assert_eq!(refusal.value, TypedValue::Array);
    assert_eq!(refusal.table, Some(original.table));
    assert_eq!(refusal.carrier, Some(original.carrier));
    assert_eq!(refusal.temp_after, None);
    assert_eq!(
        refusal.formal_after, None,
        "no formal root is fabricated after the refusal"
    );
    assert!(refusal.frame_retained);
    assert_eq!(
        refusal.reason,
        "the formal receive requires the exact reserved temp root before it may write the formal root"
    );

    let CompletionExit::VmFailure(VmError::HostError(message)) = &run.exit else {
        panic!(
            "a missing third publish must be refused by the formal receive, got {:?}",
            run.exit
        );
    };
    assert_eq!(
        message,
        "the formal receive requires the exact reserved temp root before it may write the formal root"
    );
    assert!(
        witness.failures.iter().any(|failure| failure == message),
        "the refusal is recorded: {:?}",
        witness.failures
    );
    assert!(run.exit_carriers.is_empty());

    let boundary = run.refusal.as_ref().expect("refusal boundary");
    assert_eq!(boundary.site, "formal_receive");
    assert_eq!(boundary.frame, witness.enters[0].child);
    assert!(
        boundary.frame_retained && boundary.top_after_run == Some(witness.enters[0].child),
        "the original chunk frame is the top frame after the refusal"
    );
    assert_eq!(boundary.temp_after_run, None);
    assert_eq!(boundary.formal_after_run, None, "both slots stay empty");
    assert!(
        boundary.carrier_is_table_before_release,
        "the unswept original carrier is still classifiable"
    );
    assert_eq!(boundary.child, Some(child.table));
    assert!(
        boundary.table_alive_before_release && boundary.child_alive_before_release,
        "the original table and its strong child are still live"
    );
    assert!(boundary.released_by_caller);
    let report = boundary.report.expect("caller-side collection");
    assert_eq!(
        report.live_nodes, 0,
        "the caller's collection reclaims the unrooted graph"
    );
    assert!(
        !boundary.carrier_is_table_after_release
            && !boundary.table_alive_after_release
            && !boundary.child_alive_after_release,
        "the reclaimed carrier and child are not tables"
    );
}

#[test]
fn root_entry_matches_the_p0_identity_and_exit_contract() {
    let run = drive(POSITIVE_FIXTURE, Arm::Positive);
    assert!(
        run.fuel_budget
            .zip(run.fuel_remaining)
            .is_some_and(|(budget, remaining)| remaining > 0 && remaining < budget)
    );
    assert_eq!(
        run.witness
            .marks
            .iter()
            .filter(|mark| mark.label == "leaf")
            .count(),
        1
    );
    assert_eq!(
        run.witness
            .marks
            .iter()
            .filter(|mark| mark.label == "middle")
            .count(),
        1
    );
    let values = completed(POSITIVE_FIXTURE, &run);
    assert_eq!(values.first(), Some(&Value::Bool(false)));
    assert_eq!(values.len(), 1, "the chunk exit is the scalar status only");
    assert!(
        run.exit_carriers.is_empty(),
        "the entry contract never returns a table carrier"
    );
}

struct ScalarTailCase {
    fixture: &'static str,
    subject_label: &'static str,
    values: Vec<Value>,
    arity: usize,
    finish_values: Vec<TypedValue>,
}

#[test]
fn root_tail_scalars_retire_frames_with_the_original_arity() {
    let cases = [
        ScalarTailCase {
            fixture: "root-tail-zero.lua",
            subject_label: "zero",
            values: Vec::new(),
            arity: 0,
            finish_values: Vec::new(),
        },
        ScalarTailCase {
            fixture: "root-tail-one-nil.lua",
            subject_label: "one-nil",
            values: vec![Value::Null],
            arity: 1,
            finish_values: vec![TypedValue::Nil],
        },
        ScalarTailCase {
            fixture: "root-tail-trailing-nil.lua",
            subject_label: "pair",
            values: vec![Value::Int(1), Value::Null],
            arity: 2,
            finish_values: vec![TypedValue::Int(1), TypedValue::Nil],
        },
        ScalarTailCase {
            fixture: "root-tail-false.lua",
            subject_label: "data-false",
            values: vec![Value::Bool(false)],
            arity: 1,
            finish_values: vec![TypedValue::Bool(false)],
        },
    ];
    for case in &cases {
        let fixture = case.fixture;
        let expected_arity = case.arity;
        let expected_finish_values = case.finish_values.clone();
        let expected_values = &case.values;
        let subject_label = case.subject_label;
        let run = drive(fixture, Arm::Positive);
        assert_eq!(
            labels(&run),
            ["outer", "table-arm", "after-table", subject_label],
            "{fixture}"
        );
        assert_eq!(run.witness.tables.len(), 2, "{fixture}");
        let original = table_witness(&run, 0);
        let child = table_witness(&run, 1);
        assert_eq!(run.witness.handoffs.len(), 2, "{fixture}");
        let table_handoff = &run.witness.handoffs[0];
        assert_eq!(
            table_handoff.completion,
            CompletionClass::Error {
                payload: TypedValue::Array
            },
            "{fixture}: the table arm still hands off"
        );
        assert_eq!(
            table_handoff.payload_table,
            Some(original.table),
            "{fixture}"
        );
        assert!(table_handoff.published, "{fixture}");
        let tail = &run.witness.handoffs[1];
        assert_eq!(
            tail.completion,
            CompletionClass::Normal {
                arity: expected_arity,
                values: expected_finish_values.clone()
            },
            "{fixture}: the tail completion keeps its arity and typed values"
        );
        assert!(
            !tail.published,
            "{fixture}: a scalar tail publishes no root"
        );
        assert!(
            tail.payload_table.is_none(),
            "{fixture}: no table payload in a normal completion"
        );
        assert!(tail.retired && tail.top_after_parent, "{fixture}");
        assert_eq!(run.witness.formals.len(), 1, "{fixture}");
        assert_eq!(
            run.witness.formals[0].formal_after,
            Some(original.table),
            "{fixture}"
        );

        let finish = run.witness.finish.as_ref().expect("finish record");
        assert_eq!(
            finish.completion,
            CompletionClass::Normal {
                arity: expected_arity,
                values: expected_finish_values
            },
            "{fixture}: the chunk keeps the tail arity"
        );
        assert!(finish.retired && finish.top_after.is_none(), "{fixture}");
        let collection = finish.collection.as_ref().expect("final collection");
        assert_eq!(collection.tables.len(), 2, "{fixture}");
        assert!(
            collection
                .tables
                .iter()
                .all(|table| table.table != original.table || !table.after),
            "{fixture}"
        );
        assert!(
            collection
                .tables
                .iter()
                .all(|table| table.table != child.table || !table.after),
            "{fixture}"
        );
        assert_eq!(collection.report.live_nodes, 0, "{fixture}");
        assert_eq!(
            completed(fixture, &run),
            expected_values.as_slice(),
            "{fixture}: exit values"
        );
    }
}

#[test]
fn root_scalar_error_retires_non_table_error_frames() {
    let run = drive("root-scalar-error.lua", Arm::Positive);
    assert_eq!(
        labels(&run),
        [
            "outer",
            "table-arm",
            "after-table",
            "relay",
            "scalar-fail",
            "after-scalar"
        ]
    );
    assert_eq!(run.witness.tables.len(), 2);
    let original = table_witness(&run, 0);
    let handoffs = &run.witness.handoffs;
    assert_eq!(handoffs.len(), 3);
    assert_eq!(
        handoffs[0].completion,
        CompletionClass::Error {
            payload: TypedValue::Array
        }
    );
    assert!(handoffs[0].published);
    for handoff in &handoffs[1..] {
        assert_eq!(
            handoff.completion,
            CompletionClass::Error {
                payload: TypedValue::String
            },
            "a non-table error keeps its scalar payload"
        );
        assert!(!handoff.published, "a scalar error publishes no root");
        assert!(handoff.payload_table.is_none());
        assert!(handoff.retired && handoff.top_after_parent);
        assert!(handoff.parent_slot_before_empty);
    }
    assert_eq!(run.witness.formals.len(), 2);
    let scalar_formal = &run.witness.formals[1];
    assert_eq!(scalar_formal.value, TypedValue::String);
    assert_eq!(scalar_formal.temp_before, None);
    assert_eq!(scalar_formal.temp_after, None);
    assert!(scalar_formal.value_table.is_none());
    assert!(!scalar_formal.rooted);
    assert_eq!(
        scalar_formal.formal_before,
        Some(original.table),
        "the earlier table formal stays rooted"
    );

    let values = completed("root-scalar-error.lua", &run);
    assert_eq!(values.len(), 3);
    assert_eq!(values[0], Value::Bool(false));
    assert_eq!(values[1], Value::Bool(false));
    assert_eq!(values[2], Value::string("boom"));
    assert!(
        run.exit_carriers.is_empty(),
        "the scalar-error chunk exit carries no table: {:?}",
        run.exit_carriers
    );

    let note = run.witness.note.as_ref().expect("caught-table note");
    assert_eq!(note.frame, run.witness.enters[0].child);
    assert_eq!(note.table, Some(original.table));
    assert_eq!(note.table_index, Some(0));
    assert_eq!(note.carrier, Some(original.carrier));
    assert!(
        note.table_alive && note.child_alive,
        "the table graph is usable in note"
    );
    assert!(note.temp_empty && note.formal_table == Some(original.table));
    assert!(note.rooted, "the table formal is still rooted in note");

    let finish = run.witness.finish.as_ref().expect("finish record");
    assert_eq!(
        finish.completion,
        CompletionClass::Normal {
            arity: 3,
            values: vec![
                TypedValue::Bool(false),
                TypedValue::Bool(false),
                TypedValue::String
            ]
        }
    );
    assert_eq!(finish.formal_before, Some(original.table));
    assert!(finish.retired && finish.top_after.is_none());
    assert!(finish.refused.is_none());
    let collection = finish.collection.as_ref().expect("final collection");
    assert_eq!(collection.report.live_nodes, 0);
    assert!(
        collection.tables.iter().all(|table| !table.after),
        "the final collection reclaims the table arm graph"
    );
}

#[test]
fn root_uncaught_issued_table_reaches_the_typed_boundary_with_a_live_root() {
    let run = drive("root-uncaught-table.lua", Arm::Positive);
    assert_eq!(labels(&run), ["outer", "leaf"]);
    assert_eq!(run.witness.tables.len(), 2);
    let original = table_witness(&run, 0);
    let child = table_witness(&run, 1);
    assert_eq!(run.witness.handoffs.len(), 1);
    let handoff = &run.witness.handoffs[0];
    assert_eq!(
        handoff.completion,
        CompletionClass::Error {
            payload: TypedValue::Array
        }
    );
    assert_eq!(handoff.payload_table, Some(original.table));
    assert!(handoff.published && handoff.retired);
    assert_eq!(run.witness.formals.len(), 0);

    let note = run.witness.note.as_ref().expect("leaf note record");
    assert_eq!(note.table, Some(original.table));
    assert_eq!(note.table_index, Some(0));
    assert_eq!(note.carrier, Some(original.carrier));
    assert!(
        note.table_alive && note.child == Some(child.table) && note.child_alive,
        "the leaf-local payload and its strong child are usable in note"
    );
    assert!(note.temp_empty && note.formal_table.is_none());
    assert!(
        !note.rooted,
        "the leaf has not published the payload into a reserved root yet"
    );

    let finish = run.witness.finish.as_ref().expect("finish record");
    assert!(
        !finish.retired,
        "the uncaught boundary keeps the chunk frame"
    );
    assert_eq!(finish.top_after, Some(finish.frame));
    assert!(
        finish.collection.is_none(),
        "the boundary does not sweep yet"
    );
    let uncaught = finish.uncaught.as_ref().expect("uncaught record");
    assert_eq!(uncaught.payload, TypedValue::Array);
    assert_eq!(uncaught.table, Some(original.table));
    assert_eq!(uncaught.carrier, Some(original.carrier));
    assert_eq!(uncaught.root_slot, Some(0));
    assert!(uncaught.rooted_in_temp && !uncaught.rooted_in_formal);
    assert!(uncaught.frame_retained && uncaught.alive_before_finish);

    let payload = match &run.exit {
        CompletionExit::Uncaught { payload } => payload,
        other => panic!("expected the typed uncaught boundary, got {other:?}"),
    };
    assert_eq!(carrier_ptr(payload), Some(original.carrier));
    let boundary = run.boundary.as_ref().expect("uncaught boundary");
    assert_eq!(boundary.table, Some(original.table));
    assert_eq!(boundary.carrier, Some(original.carrier));
    assert_eq!(boundary.frame, Some(finish.frame));
    assert!(boundary.live_root, "the caller receives a real live root");
    assert!(
        boundary.temp_after_run == Some(original.table) && boundary.formal_after_run.is_none(),
        "the real chunk slot still roots the payload after the run: {boundary:?}"
    );
    assert!(boundary.alive_at_boundary);
    assert!(boundary.carrier_is_table_at_boundary);
    assert!(boundary.released, "the caller releases the retained frame");
    assert!(
        !boundary.alive_after_release,
        "the released payload is really collected"
    );
    assert!(
        !boundary.carrier_is_table_after_release,
        "the reclaimed carrier is not a usable table"
    );
    let report = boundary.report.expect("caller-side collection report");
    assert_eq!(report.live_nodes, 0);
    let _ = child;
}

#[test]
fn root_status_only_pcall_clears_the_exact_temp_root_and_next_handoff_runs() {
    let run = drive("root-pcall-status-discard.lua", Arm::Positive);
    assert_eq!(
        labels(&run),
        ["outer", "first", "after-first", "second", "after-second"]
    );
    assert_eq!(run.witness.tables.len(), 2);
    let discarded = table_witness(&run, 0);
    let payload = table_witness(&run, 1);
    assert_ne!(discarded.table, payload.table);
    assert!(run.witness.failures.is_empty());
    assert_eq!(run.witness.handoffs.len(), 2);
    assert_eq!(
        run.witness.handoffs[0].completion,
        CompletionClass::Error {
            payload: TypedValue::Array
        }
    );
    assert_eq!(run.witness.handoffs[0].payload_table, Some(discarded.table));
    assert_eq!(
        run.witness.handoffs[1].completion,
        CompletionClass::Error {
            payload: TypedValue::Array
        }
    );
    assert_eq!(run.witness.handoffs[1].payload_table, Some(payload.table));

    let discard = run.witness.discards.first().expect("discard record");
    assert_eq!(discard.frame, run.witness.enters[0].child);
    assert_eq!(discard.temp_slot, 0);
    assert_eq!(discard.formal_slot, 1);
    assert_eq!(
        discard.completion,
        CompletionClass::Error {
            payload: TypedValue::Array
        }
    );
    assert_eq!(discard.payload_table, Some(discarded.table));
    assert_eq!(discard.payload_carrier, Some(discarded.carrier));
    assert_eq!(discard.temp_before, Some(discarded.table));
    assert_eq!(discard.temp_after, None, "the exact temp root is cleared");
    assert!(discard.formal_before.is_none() && discard.formal_after.is_none());
    assert!(discard.cleared_exact_root);

    let formal = run.witness.formals.first().expect("formal record");
    assert_eq!(formal.temp_before, Some(payload.table));
    assert_eq!(formal.value_table, Some(payload.table));
    assert_eq!(formal.formal_before, None);
    assert_eq!(formal.formal_after, Some(payload.table));
    assert_eq!(formal.temp_after, None);
    assert!(formal.rooted);

    let first_window = run.witness.handoffs[0]
        .window
        .as_ref()
        .expect("first window");
    assert_eq!(first_window.tables.len(), 1);
    assert_eq!(first_window.tables[0].table, discarded.table);
    assert!(first_window.tables[0].before && first_window.tables[0].after);
    assert_eq!(first_window.report.live_nodes, 1);

    let second_window = run.witness.handoffs[1]
        .window
        .as_ref()
        .expect("second window");
    assert_eq!(second_window.tables.len(), 2);
    let discarded_after = second_window
        .tables
        .iter()
        .find(|table| table.table == discarded.table)
        .expect("discarded table observed in the second window");
    assert!(
        discarded_after.before && !discarded_after.after,
        "the discarded table is really collected by the next window"
    );
    let payload_after = second_window
        .tables
        .iter()
        .find(|table| table.table == payload.table)
        .expect("payload table observed in the second window");
    assert!(
        payload_after.before && payload_after.after,
        "the newly published payload stays rooted"
    );
    assert_eq!(second_window.report.live_nodes, 1);

    let note = run.witness.note.as_ref().expect("note record");
    assert_eq!(note.table, Some(payload.table));
    assert_eq!(note.table_index, Some(1));
    assert!(note.temp_empty && note.formal_table == Some(payload.table));
    assert!(note.table_alive && note.rooted);

    let finish = run.witness.finish.as_ref().expect("finish record");
    assert_eq!(
        finish.completion,
        CompletionClass::Normal {
            arity: 2,
            values: vec![TypedValue::Bool(false), TypedValue::Bool(false)]
        }
    );
    assert_eq!(finish.formal_before, Some(payload.table));
    assert!(finish.refused.is_none());
    let collection = finish.collection.as_ref().expect("final collection");
    assert!(
        collection.tables.iter().all(|table| !table.after),
        "both issued tables are reclaimed after the chunk retirement"
    );
    assert_eq!(collection.report.live_nodes, 0);

    let values = completed("root-pcall-status-discard.lua", &run);
    assert_eq!(values.len(), 2);
    assert_eq!(values[0], Value::Bool(false));
    assert_eq!(values[1], Value::Bool(false));
    assert!(
        run.exit_carriers.is_empty(),
        "the discard chunk exit carries no table: {:?}",
        run.exit_carriers
    );
}

#[test]
fn root_statement_pcall_discard_clears_the_exact_temp_root() {
    let run = drive("root-pcall-statement-discard.lua", Arm::Positive);
    assert_eq!(
        labels(&run),
        ["outer", "first", "after-first", "second", "after-second"]
    );
    assert_eq!(run.witness.tables.len(), 2);
    let discarded = table_witness(&run, 0);
    let payload = table_witness(&run, 1);
    assert!(run.witness.failures.is_empty());
    assert_eq!(run.witness.handoffs.len(), 2);
    let discard = run.witness.discards.first().expect("discard record");
    assert_eq!(discard.frame, run.witness.enters[0].child);
    assert_eq!(discard.temp_slot, 0);
    assert_eq!(discard.formal_slot, 1);
    assert_eq!(discard.temp_before, Some(discarded.table));
    assert_eq!(discard.temp_after, None);
    assert!(discard.cleared_exact_root);
    let formal = run.witness.formals.first().expect("formal record");
    assert_eq!(formal.value_table, Some(payload.table));
    assert_eq!(formal.formal_after, Some(payload.table));

    let second_window = run.witness.handoffs[1]
        .window
        .as_ref()
        .expect("second window");
    let discarded_after = second_window
        .tables
        .iter()
        .find(|table| table.table == discarded.table)
        .expect("discarded table observed in the second window");
    assert!(discarded_after.before && !discarded_after.after);
    let payload_after = second_window
        .tables
        .iter()
        .find(|table| table.table == payload.table)
        .expect("payload table observed in the second window");
    assert!(payload_after.before && payload_after.after);

    let note = run.witness.note.as_ref().expect("note record");
    assert_eq!(note.table, Some(payload.table));
    assert_eq!(note.table_index, Some(1));
    assert!(note.table_alive && note.rooted);

    let values = completed("root-pcall-statement-discard.lua", &run);
    assert_eq!(values.len(), 1);
    assert_eq!(values[0], Value::Bool(false));
    assert!(
        run.exit_carriers.is_empty(),
        "the statement-discard chunk exit carries no table: {:?}",
        run.exit_carriers
    );
}

#[test]
fn root_shared_table_status_discard_reaches_the_second_handoff() {
    let run = drive("root-pcall-shared-table.lua", Arm::Positive);
    assert_eq!(
        labels(&run),
        ["outer", "first", "after-first", "second", "after-second"]
    );
    assert_eq!(run.witness.tables.len(), 2);
    let original = table_witness(&run, 0);
    let child = table_witness(&run, 1);
    assert!(run.witness.failures.is_empty());
    assert_eq!(run.witness.handoffs.len(), 2);
    for handoff in &run.witness.handoffs {
        assert_eq!(
            handoff.completion,
            CompletionClass::Error {
                payload: TypedValue::Array
            }
        );
        assert_eq!(handoff.payload_table, Some(original.table));
        assert!(handoff.published);
    }
    let discard = run.witness.discards.first().expect("discard record");
    assert_eq!(discard.frame, run.witness.enters[0].child);
    assert_eq!(discard.temp_slot, 0);
    assert_eq!(discard.formal_slot, 1);
    assert_eq!(discard.payload_table, Some(original.table));
    assert_eq!(discard.temp_before, Some(original.table));
    assert_eq!(discard.temp_after, None);
    assert!(discard.cleared_exact_root);
    let formal = run.witness.formals.first().expect("formal record");
    assert_eq!(formal.temp_before, Some(original.table));
    assert_eq!(formal.formal_after, Some(original.table));

    let first_window = run.witness.handoffs[0]
        .window
        .as_ref()
        .expect("first window");
    assert!(
        first_window
            .tables
            .iter()
            .all(|table| table.before && table.after),
        "the shared original and its child survive the first window"
    );
    let second_window = run.witness.handoffs[1]
        .window
        .as_ref()
        .expect("second window");
    assert!(
        second_window
            .tables
            .iter()
            .all(|table| table.before && table.after),
        "the republished original is rooted again in the second window"
    );
    assert_eq!(second_window.report.live_nodes, 2);

    let note = run.witness.note.as_ref().expect("note record");
    assert_eq!(note.table, Some(original.table));
    assert_eq!(note.table_index, Some(0));
    assert!(
        note.table_alive && note.child == Some(child.table) && note.child_alive,
        "the republished original and its child are usable in note"
    );
    assert!(note.temp_empty && note.formal_table == Some(original.table));
    assert!(note.rooted);

    let values = completed("root-pcall-shared-table.lua", &run);
    assert_eq!(values.len(), 2);
    assert_eq!(values[0], Value::Bool(false));
    assert_eq!(values[1], Value::Bool(false));
    assert!(
        run.exit_carriers.is_empty(),
        "the shared-table chunk exit carries no table: {:?}",
        run.exit_carriers
    );
}

#[test]
fn root_normal_completion_with_a_table_is_refused_at_the_handoff() {
    let run = drive("root-normal-table.lua", Arm::Positive);
    assert_eq!(labels(&run), ["outer", "make"]);
    assert_eq!(run.witness.tables.len(), 2);
    assert!(
        run.witness.handoffs.is_empty(),
        "the refusal happens before the callee frame is retired"
    );
    let CompletionExit::VmFailure(VmError::HostError(message)) = &run.exit else {
        panic!(
            "a table in a normal completion must be refused, got {:?}",
            run.exit
        );
    };
    assert!(
        message.contains("normal completion"),
        "unexpected refusal: {message}"
    );
    assert!(
        run.witness
            .failures
            .iter()
            .any(|failure| failure == message),
        "the refusal is recorded: {:?}",
        run.witness.failures
    );
}

#[test]
fn root_tail_protected_call_is_refused_at_lowering() {
    let path = fixture_path("protected-zero.lua");
    let roots = vm::compile_source_file_with_options(&path, root_options());
    match roots {
        Err(SourcePathError::SourceWithMap {
            error: SourceError::Parse(parse),
            ..
        })
        | Err(SourcePathError::Source(SourceError::Parse(parse))) => {
            eprintln!("LUA_ROOT_REFUSAL tail-pcall message={}", parse.message);
            assert!(
                parse
                    .message
                    .contains("protected call in tail-return position"),
                "unexpected refusal: {}",
                parse.message
            );
        }
        Err(other) => panic!("expected a roots-mode lowering refusal, got {other:?}"),
        Ok(_) => panic!("protected-zero.lua must be refused by the roots-mode lowering"),
    }
    assert!(
        vm::compile_source_file_with_options(&path, super::candidate_options(),).is_ok(),
        "the same fixture must stay supported on the P0 non-roots path"
    );
}

#[test]
fn root_table_literal_is_refused_at_lowering() {
    let path = fixture_path("root-table-literal.lua");
    let roots = vm::compile_source_file_with_options(&path, root_options());
    match roots {
        Err(SourcePathError::SourceWithMap {
            error: SourceError::Parse(parse),
            ..
        })
        | Err(SourcePathError::Source(SourceError::Parse(parse))) => {
            eprintln!("LUA_ROOT_REFUSAL table-literal message={}", parse.message);
            assert!(
                parse.message.contains("table literal"),
                "unexpected refusal: {}",
                parse.message
            );
        }
        Err(other) => panic!("expected a roots-mode lowering refusal, got {other:?}"),
        Ok(_) => panic!("root-table-literal.lua must be refused by the roots-mode lowering"),
    }
    assert!(
        vm::compile_source_file_with_options(&path, super::candidate_options(),).is_ok(),
        "a table literal must stay supported on the P0 non-roots path"
    );
}

#[test]
fn root_host_fault_aborts_without_guest_recovery() {
    let run = drive("root-host-fault.lua", Arm::Positive);
    let CompletionExit::VmFailure(VmError::HostError(message)) = &run.exit else {
        panic!("a real host error must abort the VM, got {:?}", run.exit);
    };
    assert_eq!(message, "completion host fault");
    assert_eq!(labels(&run), ["outer", "leaf", "after-catch"]);
    assert_eq!(run.witness.handoffs.len(), 1);
    assert_eq!(run.witness.formals.len(), 1);
    assert!(
        run.witness.finish.is_none(),
        "the chunk boundary is never reached after the host fault"
    );
    assert!(
        !run.stack.iter().any(|value| matches!(
            value,
            Value::Array(items) if matches!(items.first(), Some(Value::Bool(_)))
        )),
        "the host error must not turn into a guest completion tag: {:?}",
        run.stack
    );
}

#[test]
fn root_real_fuel_exhaustion_is_not_a_guest_error() {
    let run = drive_mode(POSITIVE_FIXTURE, Arm::Positive, RunMode::Fuel(1));
    eprintln!(
        "LUA_ROOT_FUEL budget={:?} remaining={:?} yield_reason={:?} exit={:?}",
        run.fuel_budget, run.fuel_remaining, run.last_yield_reason, run.exit
    );
    assert!(
        matches!(run.exit, CompletionExit::VmStatus(VmStatus::Yielded)),
        "the real fuel stop is the Core yielded status: {:?}",
        run.exit
    );
    assert_eq!(
        run.last_yield_reason,
        Some(VmYieldReason::Fuel),
        "the Core reports the actual fuel reason"
    );
    assert_eq!(run.fuel_remaining, Some(0), "the tiny budget is consumed");
    assert!(
        !matches!(run.exit, CompletionExit::Uncaught { .. }),
        "fuel must not be mapped to a guest error"
    );
}

#[test]
fn root_real_epoch_deadline_is_not_a_guest_error() {
    let run = drive_mode(POSITIVE_FIXTURE, Arm::Positive, RunMode::Epoch(0));
    eprintln!(
        "LUA_ROOT_EPOCH deadline={:?} yield_reason={:?} exit={:?}",
        run.epoch_deadline, run.last_yield_reason, run.exit
    );
    assert_eq!(run.epoch_deadline, Some(0));
    assert!(
        matches!(run.exit, CompletionExit::VmStatus(VmStatus::Yielded)),
        "the real epoch deadline is the Core yielded status: {:?}",
        run.exit
    );
    assert_eq!(
        run.last_yield_reason,
        Some(VmYieldReason::Epoch),
        "the Core reports the actual epoch reason"
    );
    assert!(
        !matches!(run.exit, CompletionExit::Uncaught { .. }),
        "the epoch stop must not be mapped to a guest error"
    );
}
