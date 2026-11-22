//! Issued envelope in a chunk-local slot, reached through source collect.
//!
//! The host only connects the existing private closure, cell, and table edges.
//! It does not publish a heap root and does not collect. `collectgarbage` is
//! the candidate `collect_expr` / `GATHER_ROOT` / `collect_at_safepoint` path.
//! This does not add a production closure type, and it does not claim source
//! closures, dispatch, proxy lookup, or a full safepoint scan.

use std::sync::Arc;

use vm::{
    CallOutcome, CallReturn, CompileSourceFileOptions, FrontendImportSyntax, FrontendIr,
    HostFunctionRegistry, HostImportSchema, HostOwnedFunction, HostParamPassing, HostParamSchema,
    HostTypeSchema, OwnedHostCall, ParseError, SourceFlavor, SourcePlugin, Value, VmError,
    VmStatus,
};

use super::candidate::{self, CollectNote, ReturnWitness};
use super::heap::{self, ClosureRef, TableRef, UpvalueRef};
use super::{ObservedTable, Witness};

/// Private field id. Not a Lua number key and not a string field.
const CLOSURE_FIELD: u32 = 1;
const LINK: &str = "lua_host::link_envelope";
const OBSERVE: &str = "lua_host::observe_table";

/// Description handles only. Pointers are addresses, not retained carriers.
#[derive(Clone, Copy, Debug)]
struct LinkNote {
    envelope: TableRef,
    envelope_ptr: usize,
    envelope_slot: u32,
    envelope_generation: u32,
    closure: ClosureRef,
    closure_slot: u32,
    closure_generation: u32,
    cell: UpvalueRef,
    cell_slot: u32,
    cell_generation: u32,
    target: TableRef,
    target_ptr: usize,
    target_slot: u32,
    target_generation: u32,
    roots_at_link: u32,
    collections_at_link: u32,
}

struct LinkEnvelope;

impl HostOwnedFunction for LinkEnvelope {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> Result<CallOutcome, VmError> {
        let [Value::Int(raw)] = call.args() else {
            return Err(host_fault("link_envelope needs the issued envelope"));
        };
        let raw = u64::try_from(*raw).map_err(|_| host_fault("envelope token out of range"))?;
        let token = vm::ResourceHandle::from_raw(raw)
            .map_err(|_| host_fault("envelope token is not a resource handle"))?;
        let mut host = call.vm().host_context();
        if host.scope_state::<LinkNote>().is_some() {
            return Err(host_fault("envelope graph was linked twice"));
        }
        let key = heap::LuaTableResource::key();
        let envelope = host
            .borrow_resource_with_key::<heap::LuaTableResource>(token, &key)
            .map_err(|error| host_fault(&error.to_string()))?
            .table();
        let before = heap::stats(&mut host).map_err(heap_fault)?;
        if before.live_roots != 0 {
            return Err(host_fault("link saw a heap root it did not publish"));
        }
        let note = heap::with_heap(&mut host, |heap| {
            let closure = heap.alloc_closure()?;
            let cell = heap.alloc_upvalue()?;
            let target = heap.alloc()?;
            let target_issued = heap.issue_carrier(target)?;
            heap.set_closure_edge(envelope, CLOSURE_FIELD, closure)?;
            heap.set_upvalue_edge(closure, cell)?;
            heap.write_upvalue(cell, target)?;
            let envelope_issued = heap.issued_value(envelope)?;
            Ok(LinkNote {
                envelope,
                envelope_ptr: carrier_ptr(&envelope_issued),
                envelope_slot: envelope.slot(),
                envelope_generation: envelope.generation(),
                closure,
                closure_slot: closure.slot(),
                closure_generation: closure.generation(),
                cell,
                cell_slot: cell.slot(),
                cell_generation: cell.generation(),
                target,
                target_ptr: carrier_ptr(&target_issued),
                target_slot: target.slot(),
                target_generation: target.generation(),
                roots_at_link: before.live_roots,
                collections_at_link: before.collections,
            })
        })
        .map_err(heap_fault)?;
        let after = heap::stats(&mut host).map_err(heap_fault)?;
        if after.live_roots != before.live_roots {
            return Err(host_fault("link published a heap root"));
        }
        if after.collections != before.collections {
            return Err(host_fault("link collected"));
        }
        host.scope_state_or_insert_with(|| note)
            .map_err(|error| host_fault(&error.to_string()))?;
        Ok(CallOutcome::Return(CallReturn::one(Value::Bool(true))))
    }
}

fn host_fault(message: &str) -> VmError {
    VmError::HostError(message.to_string())
}

fn heap_fault(error: heap::HeapError) -> VmError {
    VmError::HostError(error.to_string())
}

fn carrier_ptr(value: &Value) -> usize {
    let Value::Array(issued) = value else {
        panic!("issued carrier was not an array");
    };
    Arc::as_ptr(issued) as usize
}

fn catalog() -> &'static Arc<vm::HostApiCatalog> {
    static CATALOG: std::sync::OnceLock<Arc<vm::HostApiCatalog>> = std::sync::OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut builder = vm::HostApiBuilder::new();
        candidate::install_schemas(&mut builder);
        let table = HostTypeSchema::Resource(heap::LuaTableResource::key());
        builder.function(vm::HostFunctionSchema::with_return(
            OBSERVE,
            vec![
                HostParamSchema::with_passing("table", table, HostParamPassing::Borrow),
                HostParamSchema::value("number", HostTypeSchema::Float),
            ],
            HostTypeSchema::Bool,
        ));
        builder.function(vm::HostFunctionSchema::with_return(
            LINK,
            vec![HostParamSchema::with_passing(
                "envelope",
                HostTypeSchema::Resource(heap::LuaTableResource::key()),
                HostParamPassing::Borrow,
            )],
            HostTypeSchema::Bool,
        ));
        Arc::new(builder.build().expect("envelope local-root catalog"))
    })
}

struct EnvelopePlugin;

impl SourcePlugin for EnvelopePlugin {
    fn flavor(&self) -> SourceFlavor {
        SourceFlavor::Lua
    }

    fn extensions(&self) -> &'static [&'static str] {
        &[]
    }

    fn import_syntax(&self) -> FrontendImportSyntax {
        FrontendImportSyntax::Lua
    }

    fn parse_source(&self, source: &str) -> Result<FrontendIr, ParseError> {
        candidate::lower(source, catalog())
    }
}

fn run(source: &str) -> vm::Vm {
    let catalog = Arc::clone(catalog());
    let options = CompileSourceFileOptions::new()
        .with_source_plugin(&EnvelopePlugin)
        .with_host_api_catalog(Arc::clone(&catalog));
    let mut vm = vm::compile_source_with_flavor_and_options(source, SourceFlavor::Lua, options)
        .expect("compile envelope local-root source")
        .into_vm()
        .expect("create VM");
    let mut registry = HostFunctionRegistry::empty();
    candidate::register(&mut registry, &catalog).expect("register candidate runtime");
    register(&mut registry, &catalog);
    registry
        .bind_vm_cached(&mut vm)
        .expect("bind envelope local-root runtime");
    vm
}

fn register(registry: &mut HostFunctionRegistry, catalog: &vm::HostApiCatalog) {
    let observe = catalog.function(OBSERVE).expect("observe schema");
    registry
        .register_exact_owned(
            OBSERVE,
            observe.params.len() as u8,
            HostImportSchema::from_function(catalog, observe),
            |_| Box::new(Witness::TableAndNumber(0)),
        )
        .expect("register observe_table");
    let link = catalog.function(LINK).expect("link schema");
    registry
        .register_exact_owned(
            LINK,
            link.params.len() as u8,
            HostImportSchema::from_function(catalog, link),
            |_| Box::new(LinkEnvelope),
        )
        .expect("register link_envelope");
}

const SOURCE: &str = r#"
local mt = {}
mt.__mode = "v"
local watch = {}
setmetatable(watch, mt)
observe_table(watch, 0)
do
  local envelope = {}
  watch.env = envelope
  link_envelope(envelope)
  local orphan = {}
  watch.orphan = orphan
  observe_table(orphan, 0)
  orphan = nil
  collectgarbage("collect")
end
@after@
"#;

const COLLECT_AFTER: &str = "\
collectgarbage(\"collect\")
return type(watch.env)
";

const OMIT_AFTER: &str = "\
return type(watch.env)
";

#[test]
fn lua_candidate_local_envelope_roots_the_closure_graph_until_scope_exit() {
    // The second arm omits only the post-exit collect. Scope exit has already
    // dropped the local from the next gather list; that is not collection.
    let mut collect_twice = run_arm(COLLECT_AFTER, "nil");
    assert_collected(&mut collect_twice);
    let mut omit_second = run_arm(OMIT_AFTER, "table");
    assert_unrooted_but_live(&mut omit_second);
}

struct Arm {
    vm: vm::Vm,
    note: LinkNote,
    watch: ObservedTable,
    orphan: ObservedTable,
    collects: Vec<CollectNote>,
}

fn run_arm(after: &str, weak_env: &str) -> Arm {
    let source = SOURCE.replace("@after@", after);
    let mut vm = run(&source);
    assert_eq!(
        vm.run().expect("run issued envelope local"),
        VmStatus::Halted,
        "{weak_env}"
    );
    assert_eq!(vm.stack(), &[Value::string(weak_env)], "{weak_env}");
    let (note, watch, orphan, collects) = {
        let host = vm.host_context();
        let note = host.scope_state::<LinkNote>().copied().expect("link note");
        assert_eq!(note.roots_at_link, 0, "the host did not publish a root");
        assert_eq!(note.collections_at_link, 0, "the host did not collect");
        let observed = host
            .scope_state::<Vec<ObservedTable>>()
            .expect("weak observations")
            .clone();
        assert_eq!(observed.len(), 2, "watch and the independent orphan");
        let collects = host
            .scope_state::<ReturnWitness>()
            .expect("source collects")
            .collects
            .clone();
        (note, observed[0], observed[1], collects)
    };
    Arm {
        vm,
        note,
        watch,
        orphan,
        collects,
    }
}

fn assert_collected(arm: &mut Arm) {
    assert_eq!(
        arm.collects.len(),
        2,
        "in-block collect, then post-exit collect"
    );
    let inside = &arm.collects[0];
    let after = &arm.collects[1];
    assert_eq!(inside.swept, 1, "the in-block collect reclaims the orphan");
    assert_eq!(
        inside.live_nodes, 6,
        "envelope, closure, cell, and table stay"
    );
    assert_eq!(
        inside.live_roots, 3,
        "safepoint roots are the mode table, the watch, and the envelope"
    );
    assert_eq!(after.swept, 4, "post-exit collect reclaims the whole chain");
    assert_eq!(
        after.live_nodes, 2,
        "only the mode table and the watch remain"
    );
    assert_eq!(
        after.live_roots, 2,
        "the exited envelope is not published on the second gather"
    );
    assert!(after.live_roots < inside.live_roots);
    let mut host = arm.vm.host_context();
    assert!(
        !alive(&mut host, arm.orphan.table),
        "orphan identity is dead"
    );
    assert!(alive(&mut host, arm.watch.table), "watch stays rooted");
    assert_eq!(
        named(&mut host, arm.watch.table, "orphan"),
        None,
        "the orphan weak entry is gone"
    );
    assert_eq!(
        named(&mut host, arm.watch.table, "env"),
        None,
        "the envelope weak entry is gone"
    );
    assert_chain_dead(&mut host, &arm.note);
    let stats = heap::stats(&mut host).expect("stats");
    assert_eq!(stats.live_nodes, 2);
    assert_eq!(stats.live_roots, 0, "the safepoint frame was left");
    assert_eq!(
        stats.root_slots, 0,
        "no root slot remains after leave_roots"
    );
    assert_eq!(stats.collections, 2);
}

fn assert_unrooted_but_live(arm: &mut Arm) {
    assert_eq!(
        arm.collects.len(),
        1,
        "the post-exit collect is absent from source"
    );
    let inside = &arm.collects[0];
    assert_eq!(
        inside.swept, 1,
        "the in-block collect still reclaims the orphan"
    );
    assert_eq!(inside.live_nodes, 6);
    assert_eq!(inside.live_roots, 3);
    let mut host = arm.vm.host_context();
    assert!(!alive(&mut host, arm.orphan.table));
    assert_eq!(named(&mut host, arm.watch.table, "orphan"), None);
    assert_eq!(
        named(&mut host, arm.watch.table, "env"),
        Some(arm.note.envelope),
        "the weak entry still names the original envelope"
    );
    assert_chain_live(&mut host, &arm.note);
    let stats = heap::stats(&mut host).expect("stats");
    assert_eq!(
        stats.live_nodes, 6,
        "leaving the block did not sweep the chain"
    );
    assert_eq!(
        stats.live_roots, 0,
        "the in-block safepoint root is already closed; that is not collection"
    );
    assert_eq!(stats.root_slots, 0);
    assert_eq!(stats.collections, 1);
}

fn assert_chain_live(host: &mut vm::HostContext<'_>, note: &LinkNote) {
    assert!(alive(host, note.envelope));
    assert!(closure_alive(host, note.closure));
    assert!(cell_alive(host, note.cell));
    assert!(alive(host, note.target));
    assert_eq!(issued_ptr(host, note.envelope), Some(note.envelope_ptr));
    assert_eq!(issued_ptr(host, note.target), Some(note.target_ptr));
    assert_eq!(note.envelope.slot(), note.envelope_slot);
    assert_eq!(note.envelope.generation(), note.envelope_generation);
    assert_eq!(note.target.slot(), note.target_slot);
    assert_eq!(note.target.generation(), note.target_generation);
    let (closure, cell, current) = heap::with_heap(host, |heap| {
        let closure = heap.closure_edge(note.envelope, CLOSURE_FIELD)?;
        let cell = heap.upvalue_of(closure)?;
        let current = heap.read_upvalue(closure)?;
        Ok((closure, cell, current))
    })
    .expect("live closure graph");
    assert_eq!(closure, note.closure);
    assert_eq!(closure.slot(), note.closure_slot);
    assert_eq!(closure.generation(), note.closure_generation);
    assert_eq!(cell, note.cell);
    assert_eq!(cell.slot(), note.cell_slot);
    assert_eq!(cell.generation(), note.cell_generation);
    assert_eq!(current, note.target);
}

fn assert_chain_dead(host: &mut vm::HostContext<'_>, note: &LinkNote) {
    assert!(!alive(host, note.envelope));
    assert!(!closure_alive(host, note.closure));
    assert!(!cell_alive(host, note.cell));
    assert!(!alive(host, note.target));
    assert_eq!(
        issued_ptr(host, note.envelope),
        None,
        "envelope issuance is stale"
    );
    assert_eq!(
        issued_ptr(host, note.target),
        None,
        "cell table issuance is stale"
    );
    let stale = heap::with_heap(host, |heap| {
        Ok((
            heap.closure_edge(note.envelope, CLOSURE_FIELD).is_err(),
            heap.upvalue_of(note.closure).is_err(),
            heap.read_upvalue(note.closure).is_err(),
        ))
    })
    .expect("stale queries");
    assert!(
        stale.0 && stale.1 && stale.2,
        "recorded identities no longer resolve"
    );
}

fn alive(host: &mut vm::HostContext<'_>, table: TableRef) -> bool {
    heap::with_heap(host, |heap| Ok(heap.alive(table))).expect("alive")
}

fn closure_alive(host: &mut vm::HostContext<'_>, closure: ClosureRef) -> bool {
    heap::with_heap(host, |heap| Ok(heap.closure_alive(closure))).expect("closure")
}

fn cell_alive(host: &mut vm::HostContext<'_>, cell: UpvalueRef) -> bool {
    heap::with_heap(host, |heap| Ok(heap.upvalue_alive(cell))).expect("cell")
}

fn named(host: &mut vm::HostContext<'_>, table: TableRef, key: &str) -> Option<TableRef> {
    heap::with_heap(host, |heap| heap.named_edge(table, key)).expect("named edge")
}

fn issued_ptr(host: &mut vm::HostContext<'_>, table: TableRef) -> Option<usize> {
    heap::with_heap(host, |heap| {
        Ok(heap
            .issued_value(table)
            .ok()
            .map(|value| carrier_ptr(&value)))
    })
    .expect("issued identity")
}
