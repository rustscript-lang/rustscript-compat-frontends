// Exercise candidate lowering with Core without registering a second entry.
#[path = "../../src/frontends/lua/ast.rs"]
#[allow(dead_code)]
mod ast;
mod candidate;
#[path = "../../src/frontends/lua/lexer.rs"]
#[allow(dead_code)]
mod lexer;
#[path = "../../src/frontends/lua/parser.rs"]
mod parser;
#[path = "../../src/frontends/lua/resolve.rs"]
mod resolve;
mod return_frame;

use super::heap;
use super::support;
use std::sync::{Arc, OnceLock};
use vm::{
    CompileSourceFileOptions, FrontendImportSyntax, FrontendIr, HostFunctionRegistry, ParseError,
    SourceFlavor, SourcePlugin, Value, VmStatus,
};

struct CandidatePlugin;

impl SourcePlugin for CandidatePlugin {
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

fn candidate(source: &str) -> vm::Vm {
    let catalog = Arc::clone(catalog());
    let options = CompileSourceFileOptions::new()
        .with_source_plugin(&CandidatePlugin)
        .with_host_api_catalog(Arc::clone(&catalog));
    let mut vm = vm::compile_source_with_flavor_and_options(source, SourceFlavor::Lua, options)
        .expect("compile Lua text")
        .into_vm()
        .expect("create VM");
    let mut registry = HostFunctionRegistry::empty();
    candidate::register(&mut registry, &catalog).expect("register candidate runtime");
    register_witnesses(&mut registry, &catalog);
    registry
        .bind_vm_cached(&mut vm)
        .expect("bind candidate runtime");
    vm
}

#[test]
fn lua_candidate_type_preserves_table_category_after_local_copy() {
    let mut vm = candidate("local t = {}; local alias = t; return type(alias)");
    assert_eq!(vm.run().unwrap(), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::string("table")]);
}

#[test]
fn lua_candidate_type_preserves_number_category_after_local_copy() {
    let mut vm = candidate("local n = 42; local alias = n; return type(alias)");
    assert_eq!(vm.run().unwrap(), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::string("number")]);
}

#[test]
fn lua_candidate_addition_returns_a_number() {
    let mut vm = candidate("local n = 40; return n + 1 + 1");
    assert_eq!(vm.run().unwrap(), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::Float(42.0)]);
}

#[test]
fn lua_candidate_addition_rejects_table_at_runtime() {
    let mut vm = candidate("local t = {}; local alias = t; return alias + 1");
    assert!(matches!(vm.run(), Err(vm::VmError::HostError(message))
        if message == "attempt to perform arithmetic on a table value"));
}

#[test]
fn lua_candidate_typed_host_observes_table_identity() {
    for (source, same) in [
        (
            "local t = {}; local alias = t; return same_table(t, alias)",
            true,
        ),
        ("return same_table({}, {})", false),
    ] {
        let mut vm = candidate(source);
        assert_eq!(vm.run().unwrap(), VmStatus::Halted);
        assert_eq!(vm.stack(), &[Value::Bool(same)]);
        assert_eq!(entries(&mut vm).tables, 1);
    }
}

#[test]
fn lua_candidate_host_guards_both_value_categories() {
    for (source, expected) in [
        ("return same_table(1, {})", "expected Lua table"),
        ("return observe_number({})", "expected Lua number"),
    ] {
        let mut vm = candidate(source);
        assert!(matches!(vm.run(), Err(vm::VmError::HostError(message)) if message == expected));
        assert_eq!(entries(&mut vm), Entries::default());
    }

    let mut vm = candidate("return observe_number(42)");
    assert_eq!(vm.run().unwrap(), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::Float(42.0)]);
    assert_eq!(entries(&mut vm).numbers, 1);
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Entries {
    tables: usize,
    numbers: usize,
}

// Identity metadata only: recording a borrowed table does not keep its carrier alive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ObservedTable {
    table: heap::TableRef,
    token: vm::ResourceHandle,
}

fn entries(vm: &mut vm::Vm) -> Entries {
    vm.host_context()
        .scope_state::<Entries>()
        .cloned()
        .unwrap_or_default()
}

pub(super) fn catalog() -> &'static Arc<vm::HostApiCatalog> {
    static CATALOG: OnceLock<Arc<vm::HostApiCatalog>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        use vm::{HostFunctionSchema, HostParamPassing, HostParamSchema, HostTypeSchema};
        let mut builder = vm::HostApiBuilder::new();
        candidate::install_schemas(&mut builder);
        let table = HostTypeSchema::Resource(heap::LuaTableResource::key());
        builder.function(HostFunctionSchema::with_return(
            "lua_host::same_table",
            vec![
                HostParamSchema::with_passing("left", table.clone(), HostParamPassing::Borrow),
                HostParamSchema::with_passing("right", table, HostParamPassing::Borrow),
            ],
            HostTypeSchema::Bool,
        ));
        builder.function(HostFunctionSchema::with_return(
            "lua_host::observe_number",
            vec![HostParamSchema::value("number", HostTypeSchema::Float)],
            HostTypeSchema::Float,
        ));
        builder.function(HostFunctionSchema::with_return(
            "lua_host::caller_reads",
            vec![
                HostParamSchema::value("field_same", HostTypeSchema::Bool),
                HostParamSchema::value("gap", HostTypeSchema::String),
                HostParamSchema::value("missing", HostTypeSchema::String),
                HostParamSchema::value("single", HostTypeSchema::String),
                HostParamSchema::value("total", HostTypeSchema::Float),
                HostParamSchema::value("cleared", HostTypeSchema::String),
            ],
            HostTypeSchema::String,
        ));
        let table = HostTypeSchema::Resource(heap::LuaTableResource::key());
        builder.function(HostFunctionSchema::with_return(
            "lua_host::record_binding",
            vec![
                HostParamSchema::value("tag", HostTypeSchema::Float),
                HostParamSchema::with_passing("table", table, HostParamPassing::Borrow),
            ],
            HostTypeSchema::Bool,
        ));
        builder.function(HostFunctionSchema::with_return(
            "lua_host::note_bool",
            vec![
                HostParamSchema::value("tag", HostTypeSchema::Float),
                HostParamSchema::value("value", HostTypeSchema::Bool),
            ],
            HostTypeSchema::Bool,
        ));
        builder.function(HostFunctionSchema::with_return(
            "lua_host::note_text",
            vec![
                HostParamSchema::value("tag", HostTypeSchema::Float),
                HostParamSchema::value("value", HostTypeSchema::String),
            ],
            HostTypeSchema::Bool,
        ));
        for name in ["lua_host::note_stage", "lua_host::snapshot_bindings"] {
            builder.function(HostFunctionSchema::with_return(
                name,
                vec![HostParamSchema::value("tag", HostTypeSchema::Float)],
                HostTypeSchema::Bool,
            ));
        }
        for (name, reverse) in [
            ("lua_host::observe_table", false),
            ("lua_host::observe_table_after", true),
        ] {
            let table = HostParamSchema::with_passing(
                "table",
                HostTypeSchema::Resource(heap::LuaTableResource::key()),
                HostParamPassing::Borrow,
            );
            let number = HostParamSchema::value("number", HostTypeSchema::Float);
            let params = if reverse {
                vec![number, table]
            } else {
                vec![table, number]
            };
            builder.function(HostFunctionSchema::with_return(
                name,
                params,
                HostTypeSchema::Bool,
            ));
        }
        Arc::new(builder.build().unwrap())
    })
}

pub(super) fn register_witnesses(
    registry: &mut HostFunctionRegistry,
    catalog: &vm::HostApiCatalog,
) {
    for (name, op) in [
        ("lua_host::same_table", Witness::Tables),
        ("lua_host::observe_number", Witness::Number),
        ("lua_host::observe_table", Witness::TableAndNumber(0)),
        ("lua_host::observe_table_after", Witness::TableAndNumber(1)),
    ] {
        let function = catalog.function(name).unwrap();
        registry
            .register_exact_owned(
                name,
                function.params.len() as u8,
                vm::HostImportSchema::from_function(catalog, function),
                move |_| Box::new(op),
            )
            .unwrap();
    }
}

#[derive(Clone, Copy)]
enum Witness {
    Tables,
    Number,
    TableAndNumber(usize),
}

impl vm::HostOwnedFunction for Witness {
    fn call(&mut self, call: &mut vm::OwnedHostCall<'_>) -> Result<vm::CallOutcome, vm::VmError> {
        let args = call.args().to_vec();
        let mut host = call.vm().host_context();
        let counters = host.scope_state_or_insert_with(Entries::default).unwrap();
        let value = match self {
            Self::Tables => {
                counters.tables += 1;
                let [left, right] = args.as_slice() else {
                    panic!("table arity")
                };
                let key = heap::LuaTableResource::key();
                let get = |value: &Value| {
                    let Value::Int(raw) = value else {
                        panic!("expected Core resource carrier")
                    };
                    let handle =
                        vm::ResourceHandle::from_raw(u64::try_from(*raw).unwrap()).unwrap();
                    host.borrow_resource_with_key::<heap::LuaTableResource>(handle, &key)
                        .unwrap()
                        .table()
                };
                Value::Bool(get(left) == get(right))
            }
            Self::TableAndNumber(index) => {
                counters.tables += 1;
                let Value::Int(raw) = args[*index] else {
                    panic!("expected Core resource carrier")
                };
                let handle = vm::ResourceHandle::from_raw(u64::try_from(raw).unwrap()).unwrap();
                let key = heap::LuaTableResource::key();
                let table = host
                    .borrow_resource_with_key::<heap::LuaTableResource>(handle, &key)
                    .unwrap()
                    .table();
                let live = heap::with_heap(&mut host, |heap| Ok(heap.alive(table))).unwrap();
                host.scope_state_or_insert_with(Vec::<ObservedTable>::new)
                    .unwrap()
                    .push(ObservedTable {
                        table,
                        token: handle,
                    });
                assert_eq!(args[1 - *index], Value::Float(0.0));
                Value::Bool(live)
            }
            Self::Number => {
                counters.numbers += 1;
                let [Value::Float(number)] = args.as_slice() else {
                    panic!("expected float argument")
                };
                Value::Float(*number)
            }
        };
        Ok(vm::CallOutcome::Return(vm::CallReturn::one(value)))
    }
}

#[test]
fn lua_candidate_collect_preserves_an_already_evaluated_host_argument() {
    for (source, collections, marked, live, dead) in [
        (
            r#"local discarded = type({}); return observe_table({}, collectgarbage("collect"))"#,
            1,
            1,
            1,
            1,
        ),
        (
            r#"local discarded = type({}); return observe_table_after(collectgarbage("collect"), {})"#,
            1,
            0,
            1,
            1,
        ),
        (
            r#"local discarded = type({}); return observe_table({}, 0)"#,
            0,
            0,
            2,
            0,
        ),
    ] {
        let mut vm = candidate(source);
        assert_eq!(vm.run().unwrap(), VmStatus::Halted, "{source}");
        assert_eq!(vm.stack(), &[Value::Bool(true)], "{source}");
        assert_eq!(entries(&mut vm).tables, 1);
        let (stats, meter) = heap::with_heap(&mut vm.host_context(), |heap| {
            Ok((heap.stats(), heap.take_scan_meter()))
        })
        .unwrap();
        assert_eq!(stats.collections, collections, "{source}");
        assert_eq!(stats.last_marked, marked, "{source}");
        assert_eq!(stats.live_nodes, live, "{source}");
        assert_eq!(stats.live_roots, 0, "{source}");
        assert_eq!(meter.scratch.dead_slots_pushed, dead, "{source}");
        println!(
            "{source}: collections={} marked={} live={} swept={}",
            stats.collections, stats.last_marked, stats.live_nodes, meter.scratch.dead_slots_pushed,
        );
    }
}

#[test]
fn lua_candidate_type_rejects_an_ordinary_external_array() {
    let mut vm = candidate("return type(external_shape())");
    let error = vm
        .run()
        .expect_err("an ordinary array is not an issued table");
    assert!(
        matches!(error, vm::VmError::HostError(ref message) if message == "invalid private Lua value"),
        "{error}"
    );
}

#[test]
fn lua_candidate_type_rejects_a_structural_copy_of_an_issued_table() {
    let mut vm = candidate("local t = {}; return type(copy_fields(t))");
    let error = vm.run().expect_err("copied fields are not an issued table");
    assert!(
        matches!(error, vm::VmError::HostError(ref message) if message == "invalid private Lua value"),
        "{error}"
    );
}

#[test]
fn lua_candidate_addition_rejects_a_structural_copy_as_a_non_table() {
    let mut vm = candidate("local t = {}; return copy_fields(t) + 1");
    let error = vm
        .run()
        .expect_err("a structural copy is not a table operand");
    assert!(
        matches!(error, vm::VmError::HostError(ref message)
            if message == "unsupported Lua arithmetic operand"),
        "{error}"
    );
}

#[test]
fn lua_candidate_adapter_rejects_a_structural_copy_after_the_second_argument() {
    // Argument Lets run before the outer call. The table adapter is a
    // subexpression of that call, so the second `{}` allocates before the
    // first argument is rejected. This is the source order, not a handwritten
    // call schedule.
    let mut vm = candidate("return same_table(copy_fields({}), {})");
    let error = vm
        .run()
        .expect_err("a structural copy must not enter the typed host");
    assert!(
        matches!(error, vm::VmError::HostError(ref message) if message == "expected Lua table"),
        "{error}"
    );
    assert_eq!(entries(&mut vm).tables, 0, "typed host body did not run");
    assert_eq!(
        live_nodes(&mut vm),
        2,
        "the second argument allocated before the first argument was rejected"
    );
}

#[test]
fn lua_candidate_collect_refuses_a_structural_copy_as_a_root() {
    let mut vm = candidate("local shape = copy_fields({}); return collectgarbage(\"collect\")");
    let error = vm.run().expect_err("a structural copy is not a table root");
    assert!(
        matches!(error, vm::VmError::HostError(ref message)
            if message == "invalid private Lua root candidate"),
        "{error}"
    );
}

#[test]
fn lua_candidate_alias_remains_the_issued_table_across_collection() {
    let source =
        "local t = {}; local alias = t; return observe_table(alias, collectgarbage(\"collect\"))";
    let mut vm = candidate(source);
    assert_eq!(vm.run().unwrap(), VmStatus::Halted, "{source}");
    assert_eq!(vm.stack(), &[Value::Bool(true)]);
    assert_eq!(entries(&mut vm).tables, 1);
    let stats = heap::with_heap(&mut vm.host_context(), |heap| Ok(heap.stats())).unwrap();
    assert_eq!(stats.collections, 1);
    assert_eq!(stats.live_nodes, 1);
    assert_eq!(stats.live_roots, 0);
}

#[test]
fn lua_candidate_reads_original_table_kept_only_by_a_strong_field() {
    let source = r#"
local function make()
  local owner = {}
  local child = {}
  observe_table(child, 0)
  owner.field = child
  child = nil
  return owner
end
local owner = make()
local unrelated = {}
local discarded = type({})
collectgarbage("collect")
local read_ok = observe_table(owner.field, 0)
owner = nil
collectgarbage("collect")
observe_table(unrelated, 0)
return read_ok
"#;
    let mut vm = candidate(source);
    let status = vm.run();
    let mut host = vm.host_context();
    let witness = host.scope_state::<candidate::ReturnWitness>().unwrap();
    let tables = host.scope_state::<Vec<ObservedTable>>().unwrap();
    let original = tables[0];
    assert!(witness.callee_left);
    assert_eq!(witness.discarded, Some(original.table));
    let first = &witness.collects[0];
    assert_eq!(
        first.returned_edge,
        candidate::EdgeView::Live(original.table)
    );
    assert!(first.returned_alive && first.discarded_alive);
    assert_eq!(
        (first.marked_nodes, first.live_nodes, first.swept),
        (3, 3, 1)
    );
    assert_eq!(
        status.expect("read the field after a real collection"),
        VmStatus::Halted
    );
    assert_eq!(tables.len(), 3);
    assert_eq!(
        tables[1], original,
        "the same slot, generation and Core token"
    );

    assert_eq!(witness.collects.len(), 2);
    let second = &witness.collects[1];
    assert!(!second.returned_alive && !second.discarded_alive);
    assert_eq!(
        (second.marked_nodes, second.live_nodes, second.swept),
        (1, 1, 2)
    );
    assert_eq!(host.resource_count(), 1);
    let key = heap::LuaTableResource::key();
    assert!(
        host.borrow_resource_with_key::<heap::LuaTableResource>(original.token, &key)
            .is_err()
    );
    assert!(
        host.borrow_resource_with_key::<heap::LuaTableResource>(tables[2].token, &key)
            .is_ok()
    );
    assert_eq!(heap::stats(&mut host).unwrap().live_roots, 0);
    assert_eq!(vm.stack(), &[Value::Bool(true)]);
}

fn live_nodes(vm: &mut vm::Vm) -> u32 {
    heap::with_heap(&mut vm.host_context(), |heap| Ok(heap.stats().live_nodes)).unwrap()
}

fn class_of(vm: &mut vm::Vm, value: &Value) -> heap::GuestClass {
    heap::with_heap(&mut vm.host_context(), |heap| Ok(heap.guest_class(value))).unwrap()
}

#[test]
fn issued_carrier_keeps_identity_without_adding_a_root() {
    // Test-only fixture. It calls the heap directly and is not a second entry.
    support::with_idle_vm(|vm| {
        let carrier = heap::publish_guest(&mut vm.host_context()).unwrap();
        assert_eq!(
            heap::take_scan_meter(&mut vm.host_context()).unwrap(),
            heap::ScanMeter::default(),
            "issuing a carrier is not collection work"
        );
        assert_eq!(heap::stats(&mut vm.host_context()).unwrap().live_roots, 0);
        assert_eq!(vm.host_context().resource_count(), 1);
        let class = class_of(vm, &carrier);
        assert!(matches!(class, heap::GuestClass::Table(_)));
        assert_eq!(
            heap::take_scan_meter(&mut vm.host_context()).unwrap(),
            heap::ScanMeter::default(),
            "classification is not collection work"
        );
        let alias = carrier.clone();
        assert_eq!(class_of(vm, &carrier), class_of(vm, &alias));
        let Value::Array(fields) = &carrier else {
            panic!("issued carrier is an array");
        };
        let copy = Value::array(fields.iter().cloned().collect());
        assert_eq!(carrier, copy, "the copy has the same fields");
        assert_eq!(class_of(vm, &copy), heap::GuestClass::Unissued);
        let heap::GuestClass::Table(table) = class else {
            panic!("issued carrier");
        };
        let token =
            heap::with_heap(&mut vm.host_context(), |heap| heap.published_token(table)).unwrap();
        assert_eq!(
            class_of(vm, &Value::Int(i64::try_from(token).unwrap())),
            heap::GuestClass::Unissued,
            "a raw resource token is not a table"
        );
        assert_eq!(
            class_of(vm, &Value::Float(token as f64)),
            heap::GuestClass::Number
        );
        let map = Value::map(vec![(Value::string("slot"), Value::Int(0))]);
        assert_eq!(class_of(vm, &map), heap::GuestClass::Unissued);
        let Value::Array(issued) = &carrier else {
            panic!("issued carrier is an array");
        };
        let weak = Arc::downgrade(issued);
        drop(carrier);
        drop(alias);
        {
            let original = weak.upgrade().expect("the live slot owns the issued array");
            let read = heap::with_heap(&mut vm.host_context(), |heap| heap.issued_value(table))
                .expect("read the original carrier");
            let Value::Array(read) = read else {
                panic!("issued carrier is an array");
            };
            assert!(Arc::ptr_eq(&original, &read));
        }
        let report = heap::collect(&mut vm.host_context()).unwrap();
        assert_eq!(report.marked_nodes, 0);
        assert_eq!(report.live_nodes, 0);
        assert_eq!(report.live_roots, 0);
        assert_eq!(vm.host_context().resource_count(), 0);
        assert!(weak.upgrade().is_none(), "sweeping drops the slot's array");
    });
}

#[test]
fn collection_retires_an_unrooted_carrier() {
    support::with_idle_vm(|vm| {
        let carrier = heap::publish_guest(&mut vm.host_context()).unwrap();
        assert!(matches!(class_of(vm, &carrier), heap::GuestClass::Table(_)));
        heap::collect(&mut vm.host_context()).unwrap();
        assert_eq!(class_of(vm, &carrier), heap::GuestClass::Unissued);
        assert_eq!(heap::stats(&mut vm.host_context()).unwrap().live_nodes, 0);
    });
}

#[test]
fn reused_slot_does_not_accept_the_previous_carrier() {
    support::with_idle_vm(|vm| {
        let first_value = heap::publish_guest(&mut vm.host_context()).unwrap();
        let heap::GuestClass::Table(first) = class_of(vm, &first_value) else {
            panic!("issued table");
        };
        heap::collect(&mut vm.host_context()).unwrap();
        let second_value = heap::publish_guest(&mut vm.host_context()).unwrap();
        let heap::GuestClass::Table(second) = class_of(vm, &second_value) else {
            panic!("reissued table");
        };
        assert_eq!(second.slot(), first.slot());
        assert_ne!(second.generation(), first.generation());
        assert_eq!(class_of(vm, &first_value), heap::GuestClass::Unissued);
    });
}

#[test]
fn retired_generation_does_not_keep_the_carrier() {
    support::with_idle_vm(|vm| {
        heap::install(
            &mut vm.host_context(),
            heap::HeapLimits {
                nodes: 4,
                roots: 1,
                edges: 1,
                max_generation: 1,
            },
        )
        .unwrap();
        let carrier = heap::publish_guest(&mut vm.host_context()).unwrap();
        assert!(matches!(class_of(vm, &carrier), heap::GuestClass::Table(_)));
        heap::collect(&mut vm.host_context()).unwrap();
        assert_eq!(class_of(vm, &carrier), heap::GuestClass::Unissued);
    });
}

#[test]
fn another_vm_rejects_a_structurally_equal_carrier() {
    support::with_idle_vm(|left| {
        let from_left = heap::publish_guest(&mut left.host_context()).unwrap();
        support::with_idle_vm(|right| {
            let from_right = heap::publish_guest(&mut right.host_context()).unwrap();
            assert_eq!(
                from_left, from_right,
                "equal fields are not a heap identity"
            );
            assert_eq!(class_of(right, &from_left), heap::GuestClass::Unissued);
            assert!(matches!(
                class_of(right, &from_right),
                heap::GuestClass::Table(_)
            ));
        });
        assert!(matches!(
            class_of(left, &from_left),
            heap::GuestClass::Table(_)
        ));
    });
}

#[test]
fn reset_drops_the_old_carrier_even_when_fields_match_again() {
    support::with_idle_vm(|vm| {
        let old = heap::publish_guest(&mut vm.host_context()).unwrap();
        vm.reset_for_reuse().unwrap();
        let waker = std::task::Waker::noop();
        let mut context = std::task::Context::from_waker(waker);
        match vm.poll_reset_for_reuse(&mut context) {
            std::task::Poll::Ready(Ok(())) => {}
            other => panic!("reset did not finish: {other:?}"),
        }
        assert!(vm.host_context().scope_state::<heap::LuaHeap>().is_none());
        let fresh = heap::publish_guest(&mut vm.host_context()).unwrap();
        assert_eq!(old, fresh, "reissue after reset has no random domain");
        assert_eq!(class_of(vm, &old), heap::GuestClass::Unissued);
        assert!(matches!(class_of(vm, &fresh), heap::GuestClass::Table(_)));
    });
}

// IR and scope for the metatable graph. `make` publishes only `owner` into the
// caller root frame, then retire drops the callee frame. `mt` and `payload`
// were assigned nil in that frame, so they are not chunk live locals. `orphan`
// drops its frame before the chunk collect, so the self-cycle has no root.
// `collectgarbage` gathers `live_locals` and `active_temps` only. The read-back
// names are removed from `live_locals` by `= nil` before detach and the second
// collect. `host_call` argument slots and `field_read`'s base slot are not
// added to those lists, so they are not safepoint roots. `observe_table`
// stores a `TableRef` and a Core token, not a VM `Value`. `read_mt.__index` is
// the existing physical field read, not `__index` dispatch.
fn strong_metatable_source(second_collect: bool) -> String {
    let tail = if second_collect {
        "collectgarbage(\"collect\")\nlocal unrelated_ok = observe_table(unrelated, 0)\nreturn mt_same\n"
    } else {
        "return type(getmetatable(owner))\n"
    };
    let source = r#"
local function orphan()
  local cycle = {}
  observe_table(cycle, 0)
  cycle.self = cycle
  cycle = nil
end
orphan()
local function make()
  local owner = {}
  local mt = {}
  local payload = {}
  observe_table(mt, 0)
  observe_table(payload, 0)
  mt.__index = payload
  setmetatable(owner, mt)
  mt = nil
  payload = nil
  return owner
end
local owner = make()
local unrelated = {}
collectgarbage("collect")
local read_mt = getmetatable(owner)
local mt_same = observe_table(read_mt, 0)
local read_payload = read_mt.__index
local payload_same = observe_table(read_payload, 0)
read_mt = nil
read_payload = nil
setmetatable(owner, nil)
"#;
    format!("{source}{tail}")
}

fn first_collect_matrix(note: &candidate::CollectNote) -> (u64, u64, u64, u64, u32, u32, u64) {
    (
        note.metatable_edge_checks,
        note.index_edge_checks,
        note.outgoing_edge_checks,
        note.sweep_edge_checks,
        note.marked_nodes,
        note.live_nodes,
        note.swept,
    )
}

#[test]
fn lua_candidate_reads_metatable_identity_after_collect_then_reclaims_on_detach() {
    let source = strong_metatable_source(true);
    let mut vm = candidate(&source);
    let status = vm.run();
    let stack = vm.stack().to_vec();
    let mut host = vm.host_context();
    let witness = host.scope_state::<candidate::ReturnWitness>().unwrap();
    let tables = host.scope_state::<Vec<ObservedTable>>().unwrap();
    assert!(
        witness.callee_left,
        "make returned through the real callee frame"
    );
    assert_eq!(
        witness.discarded,
        Some(tables[0].table),
        "the self-cycle was witnessed"
    );
    assert!(!witness.collects.is_empty(), "collection did not run");
    let first = &witness.collects[0];
    assert!(!first.discarded_alive && first.returned_alive);
    assert_eq!(first.returned_edge, candidate::EdgeView::Absent);
    // Field edges are cycle.self and mt.__index. The metatable ref is not one
    // of them: index is 2 passes x 2 field edges, outgoing follows only the
    // marked mt field, sweep filters both because the cycle died. The
    // metatable follow is the separate 1.
    assert_eq!(
        first_collect_matrix(first),
        (1, 4, 1, 2, 4, 4, 1),
        "metatable follow must stay out of the field-edge counters"
    );
    assert_eq!(
        status.expect("read the metatable after a real collection"),
        VmStatus::Halted
    );
    assert_eq!(tables.len(), 6);
    let cycle = tables[0];
    let mt = tables[1];
    let payload = tables[2];
    assert_eq!(
        tables[3], mt,
        "getmetatable returns the same slot, generation and token"
    );
    assert_eq!(
        tables[4], payload,
        "the payload identity is the physical __index field"
    );
    assert_eq!(stack, [Value::Bool(true)]);

    assert_eq!(witness.collects.len(), 2);
    let second = &witness.collects[1];
    assert!(
        second.returned_alive,
        "owner stays rooted across the reclaim collect"
    );
    assert_eq!(second.returned_edge, candidate::EdgeView::Absent);
    // Detach removed the only metatable ref. One field edge remains, and the
    // marked nodes do not own it, so outgoing is 0 and the metatable counter
    // is 0. Sweep still filters that field edge.
    assert_eq!(
        first_collect_matrix(second),
        (0, 2, 0, 1, 2, 2, 2),
        "detach does not add a field edge, and the second collect reclaims both targets"
    );
    let key = heap::LuaTableResource::key();
    assert!(
        host.borrow_resource_with_key::<heap::LuaTableResource>(cycle.token, &key)
            .is_err()
    );
    assert!(
        host.borrow_resource_with_key::<heap::LuaTableResource>(mt.token, &key)
            .is_err()
    );
    assert!(
        host.borrow_resource_with_key::<heap::LuaTableResource>(payload.token, &key)
            .is_err()
    );
    assert!(
        host.borrow_resource_with_key::<heap::LuaTableResource>(tables[5].token, &key)
            .is_ok()
    );
    assert_eq!(host.resource_count(), 2);
    assert_eq!(heap::stats(&mut host).unwrap().live_roots, 0);
}

#[test]
fn lua_candidate_omitting_the_collect_after_detach_keeps_the_metatable_target() {
    let source = strong_metatable_source(false);
    let mut vm = candidate(&source);
    let status = vm.run();
    let stack = vm.stack().to_vec();
    let mut host = vm.host_context();
    let witness = host.scope_state::<candidate::ReturnWitness>().unwrap();
    let tables = host.scope_state::<Vec<ObservedTable>>().unwrap();
    assert_eq!(
        witness.collects.len(),
        1,
        "the omitted collect must not run"
    );
    let first = &witness.collects[0];
    assert_eq!(
        first_collect_matrix(first),
        (1, 4, 1, 2, 4, 4, 1),
        "the kept graph is the same one the reclaim test collects a second time"
    );
    assert_eq!(
        status.expect("detach without a second collection"),
        VmStatus::Halted
    );
    assert_eq!(stack, [Value::string("nil")]);
    assert_eq!(tables.len(), 5);
    let cycle = tables[0];
    let mt = tables[1];
    let payload = tables[2];
    assert_eq!(tables[3], mt);
    assert_eq!(tables[4], payload);
    assert!(
        heap::with_heap(&mut host, |heap| Ok(heap.alive(mt.table))).unwrap(),
        "removing the edge does not release the metatable node"
    );
    assert!(
        heap::with_heap(&mut host, |heap| Ok(heap.alive(payload.table))).unwrap(),
        "removing the edge does not release the payload node"
    );
    assert!(!heap::with_heap(&mut host, |heap| Ok(heap.alive(cycle.table))).unwrap());
    let key = heap::LuaTableResource::key();
    assert!(
        host.borrow_resource_with_key::<heap::LuaTableResource>(mt.token, &key)
            .is_ok()
    );
    assert!(
        host.borrow_resource_with_key::<heap::LuaTableResource>(payload.token, &key)
            .is_ok()
    );
    assert!(
        host.borrow_resource_with_key::<heap::LuaTableResource>(cycle.token, &key)
            .is_err()
    );
    let stats = heap::stats(&mut host).unwrap();
    assert_eq!(stats.collections, 1);
    assert_eq!(stats.live_nodes, 4);
}

fn assert_weak_probe(source: &str, result: &str, live: &[bool], edges: u32, collections: u32) {
    let mut vm = candidate(source);
    assert_eq!(
        vm.run().expect("execute weak-table probe"),
        VmStatus::Halted
    );
    assert_eq!(vm.stack(), &[Value::string(result)]);
    let mut host = vm.host_context();
    let observed = host.scope_state::<Vec<ObservedTable>>().unwrap().clone();
    assert_eq!(observed.len(), live.len());
    let resource_key = heap::LuaTableResource::key();
    for (index, (table, &expected)) in observed.iter().zip(live).enumerate() {
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.alive(table.table))).unwrap(),
            expected,
            "table {index} heap liveness"
        );
        assert_eq!(
            host.borrow_resource_with_key::<heap::LuaTableResource>(table.token, &resource_key)
                .is_ok(),
            expected,
            "table {index} token liveness"
        );
    }
    let stats = heap::stats(&mut host).unwrap();
    assert_eq!(stats.live_edges, edges, "stored entries after collection");
    assert_eq!(stats.collections, collections);
}

#[test]
fn lua_candidate_weak_values_clear_entries_but_trace_keys() {
    let source = r#"
local mt = {}
mt.__mode = "v"
local owner = {}
setmetatable(owner, mt)
local key = {}
local extra_key = {}
local cycle = {}
cycle.self = cycle
local other = {}
local rooted = {}
owner[key] = cycle
owner[extra_key] = other
owner.keep = rooted
observe_table(mt, 0)
observe_table(key, 0)
observe_table(extra_key, 0)
observe_table(cycle, 0)
observe_table(other, 0)
observe_table(rooted, 0)
mt = nil
extra_key = nil
cycle = nil
other = nil
collectgarbage("collect")
observe_table(owner.keep, 0)
return type(owner[key])
"#;
    assert_weak_probe(
        source,
        "nil",
        &[true, true, true, false, false, true, true],
        2,
        1,
    );
    let control = source.replace("collectgarbage(\"collect\")", "");
    assert_weak_probe(&control, "table", &[true; 7], 5, 0);
}

#[test]
fn lua_candidate_weak_keys_trace_values_with_or_without_a_back_reference() {
    let source = r#"
local mt = {}
mt.__mode = "k"
local owner = {}
setmetatable(owner, mt)
local dead_key = {}
local traced_value = {}
local back_key = {}
local back_value = {}
local rooted_key = {}
local rooted_value = {}
owner[dead_key] = traced_value
owner[back_key] = back_value
back_value.back = back_key
owner[rooted_key] = rooted_value
observe_table(dead_key, 0)
observe_table(traced_value, 0)
observe_table(back_key, 0)
observe_table(back_value, 0)
observe_table(rooted_key, 0)
observe_table(rooted_value, 0)
dead_key = nil
traced_value = nil
back_key = nil
back_value = nil
collectgarbage("collect")
return type(owner[rooted_key])
"#;
    assert_weak_probe(
        source,
        "table",
        &[false, true, true, true, true, true],
        4,
        1,
    );
    let both = source.replace(r#""k""#, r#""kv""#);
    assert_weak_probe(
        &both,
        "table",
        &[false, false, false, false, true, true],
        2,
        1,
    );
    let control = both.replace(r#"collectgarbage("collect")"#, "");
    assert_weak_probe(&control, "table", &[true; 6], 5, 0);
}

#[test]
fn lua_candidate_shared_metatable_uses_only_its_current_direct_mode() {
    let source = r#"
local inherited = {}
inherited.__mode = "v"
local proxy = {}
proxy.__index = inherited
local mt = {}
setmetatable(mt, proxy)
local one = {}
local two = {}
setmetatable(one, mt)
setmetatable(two, mt)
local key = {}
local first = {}
local second = {}
local orphan = {}
one[key] = first
two[key] = second
observe_table(first, 0)
observe_table(second, 0)
observe_table(orphan, 0)
first = nil
second = nil
orphan = nil
collectgarbage("collect")
observe_table(one[key], 0)
observe_table(two[key], 0)
@change@
@collect@
return type(one[key])
"#;
    for (change, collect, result, live, edges) in [
        (
            r#"mt.__mode = "v""#,
            true,
            "nil",
            [false, false, false, false, false],
            3,
        ),
        (
            r#"mt.__mode = "v""#,
            false,
            "table",
            [true, true, false, true, true],
            5,
        ),
        (
            "setmetatable(one, inherited)",
            true,
            "nil",
            [false, true, false, false, true],
            3,
        ),
        (
            "mt.__mode = 118",
            true,
            "table",
            [true, true, false, true, true],
            5,
        ),
        (
            "mt.__mode = inherited",
            true,
            "table",
            [true, true, false, true, true],
            5,
        ),
    ] {
        let source = source.replace("@change@", change).replace(
            "@collect@",
            if collect {
                r#"collectgarbage("collect")"#
            } else {
                ""
            },
        );
        assert_weak_probe(&source, result, &live, edges, 1 + u32::from(collect));
    }
}

#[test]
fn lua_candidate_deleting_shared_mode_restores_strong_edges() {
    let source = r#"
local mt = {}
mt.__mode = "v"
setmetatable(mt, mt)
local one = {}
local two = {}
setmetatable(one, mt)
setmetatable(two, mt)
local key = {}
local first = {}
local second = {}
one[key] = first
two[key] = second
observe_table(first, 0)
observe_table(second, 0)
collectgarbage("collect")
@change@
first = nil
second = nil
collectgarbage("collect")
return type(one[key])
"#;
    let read_mode = source
        .replace("@change@", "")
        .replace("type(one[key])", "mt.__mode");
    assert_weak_probe(&read_mode, "v", &[false, false], 1, 2);
    let removed = source.replace("@change@", "mt.__mode = nil");
    assert_weak_probe(&removed, "table", &[true, true], 2, 2);
    let unchanged = source.replace("@change@", "");
    assert_weak_probe(&unchanged, "nil", &[false, false], 1, 2);
}

#[test]
fn shared_weak_mode_is_parsed_once_per_collection() {
    let mut heap = heap::LuaHeap::new(heap::HeapLimits::production());
    let mt = heap.alloc().unwrap();
    let value = heap.alloc().unwrap();
    let mode = "x".repeat(1024) + "v";
    heap.set_mode_scalar(mt, Value::string(&mode)).unwrap();
    let count = 16;
    let frame = heap.enter_roots(count).unwrap();
    for slot in 0..count {
        let owner = heap.alloc().unwrap();
        heap.set_metatable(owner, Some(mt)).unwrap();
        heap.set_edge(owner, 1, value).unwrap();
        heap.write_root(frame, slot, owner).unwrap();
    }

    assert_eq!(heap.collect().marked_nodes, count + 1);
    let meter = heap.take_scan_meter();
    assert_eq!(meter.weak_mode_bytes, mode.len() as u64);
    assert_eq!(meter.weak_mode_lookups, u64::from(count));
    assert_eq!(meter.scratch.weak_modes_inited, u64::from(count + 2));
    assert_eq!(meter.outgoing_edge_checks, u64::from(count + 1));
    assert_eq!(meter.sweep_edge_checks, u64::from(count + 1));
    assert_eq!(heap.stats().live_edges, 1);

    heap.set_mode_scalar(mt, Value::Null).unwrap();
    assert_eq!(heap.collect().marked_nodes, count + 1);
    let meter = heap.take_scan_meter();
    assert_eq!(meter.weak_mode_bytes, 0);
    assert_eq!(meter.weak_mode_lookups, 0);
    assert_eq!(meter.scratch.weak_modes_inited, 0);
    assert_eq!(meter.sweep_edge_checks, 0);
}

/// `owner[key] = value`, a real bracket read, and `owner[key] = nil`.
/// The key is a live table identity, not a field name or a guest number.
#[test]
fn lua_candidate_assigns_reads_and_clears_a_table_object_key() {
    let source = r#"
local owner = {}
local alias_owner = owner
local key = {}
local alias_key = key
local other_key = {}
local value = {}
local replacement = {}
observe_table(value, 0)
observe_table(replacement, 0)
owner[key] = value
alias_owner[other_key] = nil
observe_table(alias_owner[alias_key], 0)
owner[other_key] = value
alias_owner[alias_key] = replacement
observe_table(owner[key], 0)
alias_owner[alias_key] = nil
observe_table(owner[other_key], 0)
return type(owner[alias_key])
"#;
    let mut vm = candidate(source);
    assert_eq!(
        vm.run().expect("assign, read, and nil a table object key"),
        VmStatus::Halted
    );
    assert_eq!(vm.stack(), &[Value::string("nil")]);
    let tables = vm
        .host_context()
        .scope_state::<Vec<ObservedTable>>()
        .unwrap()
        .clone();
    let [value, replacement, via_alias, overwritten, other] = tables.as_slice() else {
        panic!("expected five observed tables");
    };
    assert_eq!(via_alias, value, "aliases address the same entry");
    assert_eq!(overwritten, replacement, "an overwrite replaces the value");
    assert_eq!(other, value, "deletion leaves the distinct key alone");
    assert_eq!(heap::stats(&mut vm.host_context()).unwrap().live_edges, 1);
}

#[test]
fn lua_candidate_bracket_numbers_do_not_alias_private_field_ids() {
    let source = r#"
local function make()
  local owner = {}
  local value = {}
  owner.field = value
  return owner
end
local owner = make()
local number = 1
return type(owner[number])
"#;
    let mut vm = candidate(source);
    assert!(matches!(vm.run(), Err(vm::VmError::HostError(message))
        if message == "edge key is not a table"));
}

#[test]
fn lua_candidate_collect_marks_object_key_and_value_from_owner_alone() {
    let source = r#"
local function make()
  local owner = {}
  local key = {}
  local value = {}
  local child = {}
  local orphan = {}
  key.child = child
  value.self = value
  orphan.self = orphan
  owner[key] = value
  observe_table(owner, 0)
  observe_table(key, 0)
  observe_table(value, 0)
  observe_table(child, 0)
  observe_table(orphan, 0)
  return owner
end
local owner = make()
collectgarbage("collect")
return type(owner)
"#;
    let mut vm = candidate(source);
    assert_eq!(vm.run().unwrap(), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::string("table")]);
    let mut host = vm.host_context();
    let tables = host.scope_state::<Vec<ObservedTable>>().unwrap().clone();
    let [owner, key, value, child, orphan] = tables.as_slice() else {
        panic!("expected five observed tables");
    };
    let resource_key = heap::LuaTableResource::key();
    for table in [owner, key, value, child] {
        assert!(
            host.borrow_resource_with_key::<heap::LuaTableResource>(table.token, &resource_key)
                .is_ok(),
            "only the owner root should keep {table:?} alive"
        );
    }
    assert!(
        host.borrow_resource_with_key::<heap::LuaTableResource>(orphan.token, &resource_key)
            .is_err()
    );
    assert_eq!(
        heap::with_heap(&mut host, |heap| heap.object_edge(owner.table, key.table)).unwrap(),
        Some(value.table)
    );
    let witness = host.scope_state::<candidate::ReturnWitness>().unwrap();
    let [first] = witness.collects.as_slice() else {
        panic!("expected one completed collection");
    };
    assert_eq!(first_collect_matrix(first), (0, 8, 3, 4, 4, 4, 1));
    assert_eq!(host.resource_count(), 4);
    let stats = heap::stats(&mut host).unwrap();
    assert_eq!(stats.live_edges, 3);
    assert_eq!(stats.live_roots, 0);
}

#[test]
fn lua_candidate_collection_keeps_pending_index_receiver_and_key() {
    let source = r#"
local orphan = {}
observe_table(orphan, 0)
orphan = nil
({})[({})] = collectgarbage("collect")
"#;
    let mut vm = candidate(source);
    // The collector returns a number. Reaching the value error proves both
    // already evaluated table operands survived validation.
    assert!(matches!(vm.run(), Err(vm::VmError::HostError(message))
        if message == "edge target is not a table"));
    let mut host = vm.host_context();
    let orphan = host.scope_state::<Vec<ObservedTable>>().unwrap()[0];
    assert!(
        host.borrow_resource_with_key::<heap::LuaTableResource>(
            orphan.token,
            &heap::LuaTableResource::key()
        )
        .is_err()
    );
    let stats = heap::stats(&mut host).unwrap();
    assert_eq!(
        (stats.collections, stats.live_nodes, stats.live_edges),
        (1, 2, 0)
    );
}

#[test]
fn lua_candidate_nil_deletion_reclaims_key_and_value_only_on_next_collect() {
    let source = r#"
local function payload()
  local value = {}
  local orphan = {}
  value.self = value
  orphan.self = orphan
  observe_table(value, 0)
  observe_table(orphan, 0)
  return value
end
local owner = {}
local key = {}
observe_table(owner, 0)
observe_table(key, 0)
local value = payload()
owner[key] = value
value = nil
collectgarbage("collect")
local read = owner[key]
observe_table(read, 0)
owner[key] = nil
local missing = type(owner[key])
read = nil
key = nil
"#;
    // The key is needed for guest indexing and is explicitly released
    // before the second collect. The value has no direct root at the first.
    for second_collect in [false, true] {
        let collect = if second_collect {
            "collectgarbage(\"collect\")\n"
        } else {
            ""
        };
        let mut vm = candidate(&format!("{source}{collect}return missing\n"));
        assert_eq!(vm.run().unwrap(), VmStatus::Halted, "{second_collect}");
        assert_eq!(vm.stack(), &[Value::string("nil")]);
        let mut host = vm.host_context();
        let tables = host.scope_state::<Vec<ObservedTable>>().unwrap().clone();
        let [owner, key, value, orphan, via_owner] = tables.as_slice() else {
            panic!("expected five observed tables");
        };
        assert_eq!(via_owner, value, "the edge retained the issued VM carrier");
        let witness = host.scope_state::<candidate::ReturnWitness>().unwrap();
        assert_eq!(witness.collects.len(), if second_collect { 2 } else { 1 });
        assert_eq!(
            first_collect_matrix(&witness.collects[0]),
            (0, 6, 2, 3, 3, 3, 1)
        );
        if second_collect {
            assert_eq!(
                first_collect_matrix(&witness.collects[1]),
                (0, 2, 0, 1, 1, 1, 2)
            );
        }
        let resource_key = heap::LuaTableResource::key();
        for (table, alive) in [
            (owner, true),
            (key, !second_collect),
            (value, !second_collect),
            (orphan, false),
        ] {
            assert_eq!(
                host.borrow_resource_with_key::<heap::LuaTableResource>(table.token, &resource_key)
                    .is_ok(),
                alive,
                "resource lifetime after deletion for {table:?}"
            );
            assert_eq!(
                heap::with_heap(&mut host, |heap| Ok(heap.alive(table.table))).unwrap(),
                alive,
                "heap lifetime after deletion for {table:?}"
            );
        }
        let stats = heap::stats(&mut host).unwrap();
        assert_eq!(stats.live_edges, u32::from(!second_collect));
        assert_eq!(stats.live_roots, 0);
        assert_eq!(host.resource_count(), if second_collect { 1 } else { 3 });
    }
}
