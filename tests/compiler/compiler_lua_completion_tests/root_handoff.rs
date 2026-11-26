//! Real LuaHeap root handoff across an ordinary call, a tail forward, and the
//! protected receive.
//!
//! This is the second private entry of the same completion lowerer. It feeds
//! the same real lexer/parser/resolver, compiles through the fixed Core and
//! runs one interpreter VM. The difference is that every Lua function enters a
//! real `LuaHeap` root frame, every admitted Lua call publishes the error
//! payload into the caller's reserved root slot before the callee frame is
//! retired, and `pcall` moves that payload from the temp slot into its formal
//! slot.
//!
//! The error object and its strong child are canonical issued table carriers,
//! never Core arrays standing in for a table. The observer holds only
//! `TableRef` values and carrier pointers; a guest carrier does not keep the
//! heap node alive. Collection is the real synchronous full sweep.

use std::sync::{Arc, Mutex};

use vm::compiler::HostCallResolver;
use vm::vm::VmFrameContinuation;
use vm::{
    CallOutcome, CallReturn, CompileSourceFileOptions, FrontendImportSyntax, FrontendIr,
    HostFunction, ParseError, SourceFlavor, SourcePlugin, Value, Vm, VmError,
};

use super::{
    CompletionExit, FUEL_BUDGET, FUEL_CHECK_INTERVAL, catalog, consume_completion_exit,
    decoded_call_sites, fixture_path, lower,
};
use crate::heap;

/// One test arm. The positive arm publishes at every handoff. Each negative
/// arm omits exactly one publish while keeping that handoff's retirements and
/// the real full collection in the same window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arm {
    Positive,
    OmitFirstPublish,
    OmitSecondPublish,
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

#[derive(Clone, Debug)]
struct WindowRecord {
    collections_before: u32,
    orphan: TableWitness,
    orphan_alive_before: bool,
    original_alive: bool,
    child_alive: bool,
    child_edge_same: bool,
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
    published: bool,
    parent_slot_after: Option<heap::TableRef>,
    retired: bool,
    top_after_parent: bool,
    completion_is_error: bool,
    completion_is_unissued: bool,
    payload_is_original: bool,
    payload_carrier_is_original: bool,
    caller_core_prototype: Option<u32>,
    core_depth: usize,
    window: Option<WindowRecord>,
}

#[derive(Clone, Debug)]
struct FormalRecord {
    temp_slot: u32,
    formal_slot: u32,
    temp_before: Option<heap::TableRef>,
    formal_before: Option<heap::TableRef>,
    value_is_original: bool,
    temp_after: Option<heap::TableRef>,
    formal_after: Option<heap::TableRef>,
}

#[derive(Clone, Debug)]
struct FinishRecord {
    formal_before: Option<heap::TableRef>,
    formal_slot: u32,
    retired: bool,
    top_after: Option<(u32, u32)>,
    original_alive_after: bool,
    child_alive_after: bool,
    orphan: TableWitness,
    orphan_alive_before: bool,
    orphan_alive_after: bool,
    collections_before: u32,
    collections_after: u32,
    report: heap::CollectReport,
}

#[derive(Clone, Debug)]
struct NoteRecord {
    value_is_original: bool,
    carrier_is_original: bool,
    temp_empty: bool,
    formal_holds_original: bool,
}

#[derive(Clone, Debug)]
struct RootWitness {
    arm: Arm,
    original: Option<TableWitness>,
    child: Option<TableWitness>,
    enters: Vec<EnterRecord>,
    handoffs: Vec<HandoffRecord>,
    formal: Option<FormalRecord>,
    finish: Option<FinishRecord>,
    note: Option<NoteRecord>,
    marks: Vec<super::MarkRecord>,
    failures: Vec<String>,
}

#[derive(Debug)]
struct RootRun {
    stack: Vec<Value>,
    exit: CompletionExit,
    fuel_consumed: u64,
    marks: Vec<super::MarkRecord>,
    call_sites: Vec<super::DecodedCallSite>,
    witness: RootWitness,
}

impl RootRun {
    /// P0 witness view over this run's real values so the already accepted
    /// owner/return-IP/argc/script-target checks apply unchanged.
    fn as_p0_run(&self) -> super::Run {
        super::Run {
            stack: self.stack.clone(),
            fuel_consumed: self.fuel_consumed,
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
        let original = self
            .witness
            .lock()
            .expect("root witness")
            .original
            .ok_or_else(|| host_fault("note ran before the original table was created"))?;
        let mut host = vm.host_context();
        let table = classify_table(&mut host, value)?;
        let frame = heap::with_heap(&mut host, |heap| {
            heap.current_frame().ok_or(heap::HeapError::NoRootFrame)
        })
        .map_err(heap_fault)?;
        let (temp, formal) = heap::with_heap(&mut host, |heap| {
            Ok((heap.rooted_table(frame, 0)?, heap.rooted_table(frame, 1)?))
        })
        .map_err(heap_fault)?;
        self.witness.lock().expect("root witness").note = Some(NoteRecord {
            value_is_original: table == original.table,
            carrier_is_original: carrier_ptr(value) == Some(original.carrier),
            temp_empty: temp.is_none(),
            formal_holds_original: formal == Some(original.table),
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
            if state.original.is_none() {
                state.original = Some(witness);
            } else if state.child.is_none() {
                state.child = Some(witness);
            } else {
                return Err(host_fault(
                    "the fixture must create exactly one original and one child",
                ));
            }
        }
        Ok(CallOutcome::Return(CallReturn::one(issued)))
    }

    fn set_child(&self, vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
        let [original_value, child_value] = args else {
            return Err(host_fault("setchild expects the original and the child"));
        };
        let (original, child) = {
            let state = self.witness.lock().expect("root witness");
            (
                state
                    .original
                    .ok_or_else(|| host_fault("setchild ran before the original table"))?,
                state
                    .child
                    .ok_or_else(|| host_fault("setchild ran before the child table"))?,
            )
        };
        let mut host = vm.host_context();
        let original_found = classify_table(&mut host, original_value)?;
        let child_found = classify_table(&mut host, child_value)?;
        if original_found != original.table || child_found != child.table {
            return Err(host_fault("setchild received a different issued table"));
        }
        heap::with_heap(&mut host, |heap| {
            heap.set_named_edge(original.table, "child", child.table)
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
        let parent =
            heap::with_heap(&mut host, |heap| Ok(heap.current_frame())).map_err(heap_fault)?;
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
        let (arm, index, original, child) = {
            let state = self.witness.lock().expect("root witness");
            (
                state.arm,
                state.handoffs.len(),
                state
                    .original
                    .ok_or_else(|| host_fault("handoff ran before the original table"))?,
                state
                    .child
                    .ok_or_else(|| host_fault("handoff ran before the child table"))?,
            )
        };

        let mut host = vm.host_context();
        let child_frame = heap::with_heap(&mut host, |heap| {
            heap.current_frame().ok_or(heap::HeapError::NoRootFrame)
        })
        .map_err(heap_fault)?;
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
        let parent_slot_before =
            heap::with_heap(&mut host, |heap| heap.rooted_table(own, 0)).map_err(heap_fault)?;
        if parent_slot_before.is_some() {
            return Err(self.fail("the reserved parent handoff root slot is not empty"));
        }
        let completion_is_unissued =
            !matches!(classify(&mut host, completion)?, heap::GuestClass::Table(_));
        let Value::Array(items) = completion else {
            return Err(self.fail("root handoff completion is not a private array"));
        };
        let completion_is_error =
            items.len() == 2 && matches!(items.first(), Some(Value::Bool(false)));
        if !completion_is_error {
            return Err(self.fail("root handoff completion is not exactly [false, payload]"));
        }
        let payload = &items[1];
        let payload_table = match classify(&mut host, payload) {
            Ok(heap::GuestClass::Table(table)) => table,
            Ok(other) => {
                return Err(self.fail(format!(
                    "root handoff payload is not an issued table: {other:?}"
                )));
            }
            Err(error) => return Err(error),
        };
        let payload_carrier_is_original = carrier_ptr(payload) == Some(original.carrier);
        let payload_is_original = payload_table == original.table;
        if !payload_is_original || !payload_carrier_is_original {
            return Err(self.fail("root handoff payload is not the same issued original table"));
        }

        let skip_publish = matches!(
            (arm, index),
            (Arm::OmitFirstPublish, 0) | (Arm::OmitSecondPublish, 1)
        );
        let child_top_at_publish = heap::with_heap(&mut host, |heap| {
            Ok(heap.current_frame() == Some(child_frame))
        })
        .map_err(heap_fault)?;
        if !child_top_at_publish {
            return Err(self.fail("the callee frame is not the top frame at the publish point"));
        }
        if !skip_publish {
            heap::with_heap(&mut host, |heap| heap.write_root(own, 0, payload_table))
                .map_err(heap_fault)?;
        }
        let parent_slot_after =
            heap::with_heap(&mut host, |heap| heap.rooted_table(own, 0)).map_err(heap_fault)?;
        if !skip_publish && parent_slot_after != Some(payload_table) {
            return Err(self.fail("the parent temp root did not receive the payload"));
        }
        if skip_publish && parent_slot_after.is_some() {
            return Err(self.fail("the omitted handoff still wrote a parent root"));
        }

        // Publish (or the deliberate omission) is complete while the callee
        // frame is still the top frame. Only now retire it.
        heap::with_heap(&mut host, |heap| heap.leave_roots(child_frame)).map_err(heap_fault)?;
        let top_after =
            heap::with_heap(&mut host, |heap| Ok(heap.current_frame())).map_err(heap_fault)?;
        let top_after_parent = top_after == Some(own);

        let window = if index < 2 {
            // Each window publishes exactly one fresh table with no root
            // immediately before the mark. It is not part of the payload
            // graph, so it can only disappear when this real collection sweeps.
            let stats_before =
                heap::with_heap(&mut host, |heap| Ok(heap.stats())).map_err(heap_fault)?;
            let orphan_value = heap::publish_guest(&mut host).map_err(heap_fault)?;
            let orphan_table = classify_table(&mut host, &orphan_value)?;
            let orphan = TableWitness {
                table: orphan_table,
                carrier: carrier_ptr(&orphan_value)
                    .ok_or_else(|| host_fault("fresh window orphan has no carrier"))?,
            };
            let orphan_alive_before =
                heap::with_heap(&mut host, |heap| Ok(heap.alive(orphan_table)))
                    .map_err(heap_fault)?;
            let report = heap::collect(&mut host).map_err(heap_fault)?;
            let stats_after =
                heap::with_heap(&mut host, |heap| Ok(heap.stats())).map_err(heap_fault)?;
            let original_alive = heap::with_heap(&mut host, |heap| Ok(heap.alive(original.table)))
                .map_err(heap_fault)?;
            let child_alive = heap::with_heap(&mut host, |heap| Ok(heap.alive(child.table)))
                .map_err(heap_fault)?;
            let child_edge_same = heap::with_heap(&mut host, |heap| {
                if !heap.alive(original.table) {
                    return Ok(false);
                }
                match heap.named_edge(original.table, "child") {
                    Ok(Some(found)) => Ok(found == child.table),
                    _ => Ok(false),
                }
            })
            .map_err(heap_fault)?;
            let orphan_alive_after =
                heap::with_heap(&mut host, |heap| Ok(heap.alive(orphan_table)))
                    .map_err(heap_fault)?;
            Some(WindowRecord {
                collections_before: stats_before.collections,
                orphan,
                orphan_alive_before,
                original_alive,
                child_alive,
                child_edge_same,
                orphan_alive_after,
                collections_after: stats_after.collections,
                report,
            })
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
                published: !skip_publish,
                parent_slot_after,
                retired: top_after_parent,
                top_after_parent,
                completion_is_error,
                completion_is_unissued,
                payload_is_original,
                payload_carrier_is_original,
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
        let original = self
            .witness
            .lock()
            .expect("root witness")
            .original
            .ok_or_else(|| host_fault("formal ran before the original table"))?;
        let mut host = vm.host_context();
        let current =
            heap::with_heap(&mut host, |heap| Ok(heap.current_frame())).map_err(heap_fault)?;
        if current != Some(own) {
            return Err(self.fail("the formal receiver is not the current parent frame"));
        }
        let (temp_before, formal_before) = heap::with_heap(&mut host, |heap| {
            Ok((
                heap.rooted_table(own, temp)?,
                heap.rooted_table(own, formal)?,
            ))
        })
        .map_err(heap_fault)?;
        let value_table = classify_table(&mut host, value)?;
        if value_table != original.table
            || temp_before != Some(original.table)
            || formal_before.is_some()
        {
            return Err(self.fail("the formal receive does not match the reserved temp root"));
        }
        heap::with_heap(&mut host, |heap| {
            heap.write_root(own, formal, original.table)
        })
        .map_err(heap_fault)?;
        heap::with_heap(&mut host, |heap| heap.clear_root(own, temp)).map_err(heap_fault)?;
        let (temp_after, formal_after) = heap::with_heap(&mut host, |heap| {
            Ok((
                heap.rooted_table(own, temp)?,
                heap.rooted_table(own, formal)?,
            ))
        })
        .map_err(heap_fault)?;
        self.witness.lock().expect("root witness").formal = Some(FormalRecord {
            temp_slot: temp,
            formal_slot: formal,
            temp_before,
            formal_before,
            value_is_original: value_table == original.table,
            temp_after,
            formal_after,
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
        let (original, child) = {
            let state = self.witness.lock().expect("root witness");
            (
                state
                    .original
                    .ok_or_else(|| host_fault("finish ran before the original table"))?,
                state
                    .child
                    .ok_or_else(|| host_fault("finish ran before the child table"))?,
            )
        };
        let mut host = vm.host_context();
        let current =
            heap::with_heap(&mut host, |heap| Ok(heap.current_frame())).map_err(heap_fault)?;
        if current != Some(own) {
            return Err(self.fail("the finishing chunk is not the current frame"));
        }
        let Value::Array(items) = completion else {
            return Err(self.fail("chunk completion is not a private array"));
        };
        if items.len() != 3
            || !matches!(items.first(), Some(Value::Bool(true)))
            || !matches!(items.get(1), Some(Value::Bool(false)))
        {
            return Err(self.fail("chunk completion is not [true, false, payload]"));
        }
        let payload_table = classify_table(&mut host, &items[2])?;
        if payload_table != original.table || carrier_ptr(&items[2]) != Some(original.carrier) {
            return Err(self.fail("chunk payload is not the same issued original table"));
        }
        let formal_before = heap::with_heap(&mut host, |heap| heap.rooted_table(own, formal))
            .map_err(heap_fault)?;
        if formal_before != Some(original.table) {
            return Err(self.fail("the last formal root does not hold the original table"));
        }
        heap::with_heap(&mut host, |heap| heap.leave_roots(own)).map_err(heap_fault)?;
        let top_after =
            heap::with_heap(&mut host, |heap| Ok(heap.current_frame())).map_err(heap_fault)?;
        let stats_before =
            heap::with_heap(&mut host, |heap| Ok(heap.stats())).map_err(heap_fault)?;
        let orphan_value = heap::publish_guest(&mut host).map_err(heap_fault)?;
        let orphan_table = classify_table(&mut host, &orphan_value)?;
        let orphan = TableWitness {
            table: orphan_table,
            carrier: carrier_ptr(&orphan_value)
                .ok_or_else(|| host_fault("final orphan has no carrier"))?,
        };
        let orphan_alive_before =
            heap::with_heap(&mut host, |heap| Ok(heap.alive(orphan_table))).map_err(heap_fault)?;
        let report = heap::collect(&mut host).map_err(heap_fault)?;
        let stats_after =
            heap::with_heap(&mut host, |heap| Ok(heap.stats())).map_err(heap_fault)?;
        let original_alive_after =
            heap::with_heap(&mut host, |heap| Ok(heap.alive(original.table)))
                .map_err(heap_fault)?;
        let child_alive_after =
            heap::with_heap(&mut host, |heap| Ok(heap.alive(child.table))).map_err(heap_fault)?;
        let orphan_alive_after =
            heap::with_heap(&mut host, |heap| Ok(heap.alive(orphan_table))).map_err(heap_fault)?;
        self.witness.lock().expect("root witness").finish = Some(FinishRecord {
            formal_before,
            formal_slot: formal,
            retired: top_after.is_none(),
            top_after: top_after.map(heap::RootFrameId::parts),
            original_alive_after,
            child_alive_after,
            orphan,
            orphan_alive_before,
            orphan_alive_after,
            collections_before: stats_before.collections,
            collections_after: stats_after.collections,
            report,
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
}

fn drive(arm: Arm) -> RootRun {
    let path = fixture_path("root-handoff.lua");
    let compiled = vm::compile_source_file_with_options(&path, root_options())
        .unwrap_or_else(|error| panic!("root handoff compile failed: {error:?}"));
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
        original: None,
        child: None,
        enters: Vec::new(),
        handoffs: Vec::new(),
        formal: None,
        finish: None,
        note: None,
        marks: Vec::new(),
        failures: Vec::new(),
    }));
    bind_hosts(&mut vm, &witness);
    vm.set_fuel_check_interval(FUEL_CHECK_INTERVAL)
        .unwrap_or_else(|error| panic!("root handoff fuel interval: {error}"));
    vm.set_fuel(FUEL_BUDGET);
    let status = vm.run();
    let remaining = vm
        .get_fuel()
        .unwrap_or_else(|| panic!("root handoff fuel metering missing"));
    let fuel_consumed = FUEL_BUDGET
        .checked_sub(remaining)
        .unwrap_or_else(|| panic!("root handoff fuel increased"));
    let stack = vm.stack().to_vec();
    let exit = consume_completion_exit(status, stack.clone());
    let witness = witness.lock().expect("root witness lock").clone();
    eprintln!(
        "LUA_ROOT_HANDOFF arm={arm:?} fuel={fuel_consumed} exit={exit:?} enters={:?} handoffs={:?} formal={:?} finish={:?} failures={:?}",
        witness.enters, witness.handoffs, witness.formal, witness.finish, witness.failures
    );
    RootRun {
        stack,
        exit,
        fuel_consumed,
        marks: witness.marks.clone(),
        call_sites,
        witness,
    }
}

fn labels(run: &RootRun) -> Vec<&str> {
    run.marks.iter().map(|mark| mark.label.as_str()).collect()
}

fn mark_prototype(run: &RootRun, label: &str) -> Option<u32> {
    run.marks
        .iter()
        .find(|mark| mark.label == label)?
        .last_prototype
}

#[test]
fn positive_handoff_keeps_one_issued_table_through_all_three_paths() {
    let run = drive(Arm::Positive);
    let witness = &run.witness;
    let original = witness.original.expect("original table witness");
    let child = witness.child.expect("child table witness");
    assert_ne!(child.table, original.table, "the child is a distinct node");
    assert_eq!(
        labels(&run),
        ["outer", "relay", "middle", "leaf", "outer-tail"],
        "the ordinary tail is skipped and the outer tail runs"
    );
    assert_eq!(witness.handoffs.len(), 3, "ordinary, tail, protected");
    assert!(witness.formal.is_some(), "pcall must adopt its formal");
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
        assert!(handoff.published, "{what}: publish");
        assert_eq!(
            handoff.parent_slot_after,
            Some(original.table),
            "{what}: parent temp root"
        );
        assert!(
            handoff.retired && handoff.top_after_parent,
            "{what}: retire"
        );
        assert!(
            handoff.completion_is_error,
            "{what}: exact [false, payload]"
        );
        assert!(
            handoff.completion_is_unissued,
            "{what}: completion array is not a table"
        );
        assert!(handoff.payload_is_original, "{what}: original TableRef");
        assert!(
            handoff.payload_carrier_is_original,
            "{what}: issued carrier identity"
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
        assert!(window.original_alive, "the published payload stays alive");
        assert!(window.child_alive, "the strong child stays reachable");
        assert!(
            window.child_edge_same,
            "the strong edge keeps the child identity"
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

    let formal = witness.formal.as_ref().expect("formal record");
    assert_eq!(formal.temp_slot, 0);
    assert_eq!(formal.formal_slot, 1);
    assert_eq!(formal.temp_before, Some(original.table));
    assert_eq!(formal.formal_before, None);
    assert!(formal.value_is_original);
    assert_eq!(formal.temp_after, None, "the temp root is cleared");
    assert_eq!(
        formal.formal_after,
        Some(original.table),
        "the formal root holds it"
    );

    let note = witness.note.as_ref().expect("note record");
    assert!(note.value_is_original && note.carrier_is_original);
    assert!(note.temp_empty && note.formal_holds_original);

    let finish = witness.finish.as_ref().expect("finish record");
    assert_eq!(
        finish.formal_slot, 1,
        "the formal slot is the second reserved slot"
    );
    assert_eq!(finish.formal_before, Some(original.table));
    assert!(
        finish.retired,
        "the receiving chunk frame is really retired"
    );
    assert_eq!(finish.top_after, None, "no Lua root frame remains");
    assert!(
        !finish.original_alive_after,
        "the last formal root release collects the original"
    );
    assert!(
        !finish.child_alive_after,
        "the strong child dies with the original"
    );
    assert!(finish.orphan_alive_before && !finish.orphan_alive_after);
    assert_ne!(finish.orphan.table, original.table);
    assert_eq!(
        finish.report.live_nodes, 0,
        "no heap node survives the last root"
    );
    assert_eq!(finish.collections_after, finish.collections_before + 1);

    let CompletionExit::Values(values) = &run.exit else {
        panic!("positive run must halt with values: {:?}", run.exit);
    };
    assert_eq!(
        values.len(),
        2,
        "pcall returns exactly false and the payload"
    );
    assert_eq!(values[0], Value::Bool(false));
    assert_eq!(
        carrier_ptr(&values[1]),
        Some(original.carrier),
        "the returned payload is the same issued carrier"
    );
}

#[test]
fn omitting_the_first_publish_collects_the_payload_before_middle_returns() {
    let run = drive(Arm::OmitFirstPublish);
    let witness = &run.witness;
    let original = witness.original.expect("original table witness");
    assert_eq!(labels(&run), ["outer", "relay", "middle", "leaf"]);
    assert_eq!(
        witness.handoffs.len(),
        1,
        "the stale payload stops the next handoff: {:?}",
        witness.failures
    );
    let handoff = &witness.handoffs[0];
    assert!(!handoff.published, "this arm omits exactly this publish");
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
    assert!(handoff.completion_is_error && handoff.payload_is_original);
    assert!(handoff.payload_carrier_is_original);
    super::assert_dynamic_call_witness(&run.as_p0_run(), "leaf", 0);
    assert_eq!(
        handoff.caller_core_prototype,
        mark_prototype(&run, "middle"),
        "the window runs in the actual Core caller of the leaf call"
    );

    let window = handoff.window.as_ref().expect("first window");
    assert!(window.orphan_alive_before);
    assert_ne!(window.orphan.table, original.table);
    assert!(
        !window.original_alive,
        "the unpubished payload is collected"
    );
    assert!(!window.child_alive, "the strong child dies with it");
    assert!(
        !window.orphan_alive_after,
        "the fresh orphan is collected in this sweep"
    );
    assert_eq!(window.collections_after, window.collections_before + 1);
    assert_eq!(
        window.report.live_nodes, 0,
        "the original graph and the fresh orphan are both swept"
    );
    let _ = original;

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
    let run = drive(Arm::OmitSecondPublish);
    let witness = &run.witness;
    let original = witness.original.expect("original table witness");
    let child = witness.child.expect("child table witness");
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
    assert!(
        first_window.original_alive && first_window.child_alive,
        "the second control's first window already succeeded"
    );
    assert!(first_window.child_edge_same);
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
        !second.published,
        "this arm omits exactly the second publish"
    );
    assert!(second.parent_slot_before_empty);
    assert!(second.child_top_at_publish);
    assert_eq!(second.parent_slot_after, None);
    assert!(second.retired && second.top_after_parent);
    assert!(second.payload_is_original && second.payload_carrier_is_original);
    let second_window = second.window.as_ref().expect("second window");
    assert!(second_window.orphan_alive_before);
    assert_ne!(second_window.orphan.table, original.table);
    assert_ne!(second_window.orphan.table, child.table);
    assert!(
        !second_window.original_alive,
        "the unpubished payload is collected in the second window"
    );
    assert!(!second_window.child_alive);
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

#[test]
fn root_entry_matches_the_p0_identity_and_exit_contract() {
    let run = drive(Arm::Positive);
    assert!(run.fuel_consumed > 0 && run.fuel_consumed < FUEL_BUDGET);
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
    let CompletionExit::Values(values) = &run.exit else {
        panic!("expected a value exit: {:?}", run.exit);
    };
    assert_eq!(values.first(), Some(&Value::Bool(false)));
    assert!(matches!(values.get(1), Some(Value::Array(_))));
}
