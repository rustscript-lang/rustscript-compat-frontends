// Exercise candidate lowering with Core without registering a second entry.
#[path = "../../src/frontends/lua/ast.rs"]
#[allow(dead_code)]
mod ast;
mod candidate;
#[path = "closure_graph.rs"]
mod closure_graph;
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
            "lua_host::float_bits",
            vec![HostParamSchema::value("tag", HostTypeSchema::Float)],
            HostTypeSchema::Float,
        ));
        let table = HostTypeSchema::Resource(heap::LuaTableResource::key());
        builder.function(HostFunctionSchema::with_return(
            "lua_host::plant_private_field",
            vec![
                HostParamSchema::with_passing("owner", table.clone(), HostParamPassing::Borrow),
                HostParamSchema::with_passing("value", table, HostParamPassing::Borrow),
            ],
            HostTypeSchema::Bool,
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
        ("lua_host::float_bits", Witness::FloatBits),
        ("lua_host::plant_private_field", Witness::PlantField),
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
    FloatBits,
    PlantField,
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
            Self::FloatBits => {
                let [Value::Float(tag)] = args.as_slice() else {
                    panic!("expected float tag")
                };
                Value::Float(special_float(*tag))
            }
            Self::PlantField => {
                let [owner, planted] = args.as_slice() else {
                    panic!("plant arity")
                };
                let key = heap::LuaTableResource::key();
                let table_of = |value: &Value| {
                    let Value::Int(raw) = value else {
                        panic!("expected Core resource carrier")
                    };
                    let handle =
                        vm::ResourceHandle::from_raw(u64::try_from(*raw).unwrap()).unwrap();
                    host.borrow_resource_with_key::<heap::LuaTableResource>(handle, &key)
                        .unwrap()
                        .table()
                };
                let owner = table_of(owner);
                let planted = table_of(planted);
                heap::with_heap(&mut host, |heap| heap.set_edge(owner, 1, planted))
                    .expect("plant private field 1");
                Value::Bool(true)
            }
        };
        Ok(vm::CallOutcome::Return(vm::CallReturn::one(value)))
    }
}

fn special_float(tag: f64) -> f64 {
    // `1e400` is +Inf in this lowerer. `-0`, NaN, and `-Inf` still enter
    // through these tags. The tags themselves are ordinary numbers. The
    // returned binary64 values are the typed inputs.
    match tag {
        1.0 => f64::from_bits(0x8000_0000_0000_0000),
        2.0 => f64::NAN,
        3.0 => f64::INFINITY,
        4.0 => f64::NEG_INFINITY,
        _ => panic!("unknown float tag {tag}"),
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

fn assert_weak_probe(
    source: &str,
    result: &str,
    live: &[bool],
    edges: u32,
    collections: u32,
) -> Vec<ObservedTable> {
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
    observed
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
fn lua_candidate_weak_value_in_a_dot_field_is_released() {
    let source = r#"
local mt = {}
mt.__mode = "v"
local owner = {}
setmetatable(owner, mt)
local value = {}
owner.keep = value
observe_table(value, 0)
value = nil
collectgarbage("collect")
return type(owner.keep)
"#;
    assert_weak_probe(source, "nil", &[false], 1, 1);

    let control = source.replace(r#"collectgarbage("collect")"#, "");
    assert_weak_probe(&control, "table", &[true], 2, 0);
    let readback = control.replace(
        "return type(owner.keep)",
        "observe_table(owner.keep, 0)\nreturn type(owner.keep)",
    );
    let observed = assert_weak_probe(&readback, "table", &[true, true], 2, 0);
    assert_eq!(observed[0], observed[1], "original weak field referent");
}

/// Ordinary dot fields store one UTF-8 string in the same edge record as a table.
///
/// The candidate lowerer has no concatenation, so the string is a literal placed
/// in a local and then cleared. That is the runnable stand-in for a dynamic
/// string that must not remain a local root. Field 0 stays the only `__mode` key.
#[test]
fn lua_candidate_weak_collection_keeps_an_ordinary_utf8_string_field() {
    let kept = "kv-caf\u{e9}-42";
    let source = format!(
        r#"
local mt = {{}}
mt.__mode = "@mode@"
local owner = {{}}
setmetatable(owner, mt)
local kept = "{kept}"
local drop = {{}}
owner.keep = kept
owner.drop = drop
observe_table(drop, 0)
kept = nil
drop = nil
collectgarbage("collect")
return owner.keep
"#
    );
    for mode in ["v", "kv"] {
        let source = source.replace("@mode@", mode);
        assert_weak_probe(&source, kept, &[false], 2, 1);
    }
    let control = source
        .replace("@mode@", "v")
        .replace(r#"collectgarbage("collect")"#, "");
    assert_weak_probe(&control, kept, &[true], 3, 0);

    let ordinary_mode_text = r#"
local mt = {}
mt.note = "kv"
local owner = {}
setmetatable(owner, mt)
local drop = {}
owner.drop = drop
observe_table(drop, 0)
drop = nil
collectgarbage("collect")
return type(owner.drop)
"#;
    assert_weak_probe(ordinary_mode_text, "table", &[true], 2, 1);

    let mode_stays_field_zero = format!(
        r#"
local mt = {{}}
mt.__mode = "v"
mt.note = "k"
local owner = {{}}
setmetatable(owner, mt)
local kept = "{kept}"
local drop = {{}}
owner.keep = kept
owner.drop = drop
observe_table(drop, 0)
kept = nil
drop = nil
collectgarbage("collect")
return owner.keep
"#
    );
    assert_weak_probe(&mode_stays_field_zero, kept, &[false], 3, 1);

    let deleted = format!(
        r#"
local owner = {{}}
owner.keep = "{kept}"
owner.keep = nil
return type(owner.keep)
"#
    );
    let mut vm = candidate(&deleted);
    assert_eq!(
        vm.run().expect("delete an ordinary string field"),
        VmStatus::Halted
    );
    assert_eq!(vm.stack(), &[Value::string("nil")]);
    let stats = heap::stats(&mut vm.host_context()).unwrap();
    assert_eq!(stats.live_edges, 0, "nil removes the string record");
    assert_eq!(stats.collections, 0);

    let string_over_table = format!(
        r#"
local owner = {{}}
local drop = {{}}
owner.keep = drop
owner.keep = "{kept}"
observe_table(drop, 0)
return owner.keep
"#
    );
    assert_weak_probe(&string_over_table, kept, &[true], 1, 0);

    let table_over_string = format!(
        r#"
local owner = {{}}
local drop = {{}}
owner.keep = "{kept}"
owner.keep = drop
observe_table(drop, 0)
observe_table(owner.keep, 0)
return type(owner.keep)
"#
    );
    let overwritten = assert_weak_probe(&table_over_string, "table", &[true, true], 1, 0);
    assert_eq!(
        overwritten[0], overwritten[1],
        "table replaces the string record"
    );
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
fn lua_candidate_deleting_shared_mode_does_not_restore_a_cleared_entry() {
    let setup = r#"
local mt = {}
mt.__mode = "v"
local owner = {}
local peer = {}
setmetatable(owner, mt)
setmetatable(peer, mt)
local key = {}
local value = {}
local kept = {}
owner[key] = value
peer[key] = kept
observe_table(value, 0)
observe_table(kept, 0)
value = nil
"#;
    let collect = r#"collectgarbage("collect")"#;
    let read = "return type(owner[key])";
    let first = format!("{setup}{collect}\nobserve_table(kept, 0)\n{read}");
    assert_weak_probe(&first, "nil", &[false, true, true], 2, 1);

    // The peer stays rooted through the first collection, then relies on its now strong entry.
    let change = r#"
mt.__mode = nil
kept = nil
collectgarbage("collect")
observe_table(peer[key], 0)
"#;
    let source = format!("{setup}{collect}{change}{read}");
    let observed = assert_weak_probe(&source, "nil", &[false, true, true], 1, 2);
    assert_eq!(observed[1], observed[2], "original peer referent");

    let control = format!("{setup}{change}{read}");
    let observed = assert_weak_probe(&control, "table", &[true, true, true], 2, 1);
    assert_eq!(observed[1], observed[2], "original peer referent");
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

/// Object-key strings share the table edge record. They are not table tokens.
///
/// Upstream LuaJIT `96f7b297` with `-joff` and Lua 5.2 compat, after the helper
/// returns and an explicit collect: `rawget(owner, kept_key)` is `stored-42` in
/// `k`/`v`/`kv`; `rawget(watch, "drop")` is nil and `owner` has 1 entry in `k`
/// and `kv`, and is a table with 2 entries in `v`. Omitting collect leaves the
/// drop key as a table and 2 owner entries in all three modes.
///
/// Candidate adaptations, same literals: index reads stand in for `rawget`
/// because this lowerer has no metamethods; `live_edges` stands in for the
/// owner entry count (two `__mode` fields, `kept_key.owner`, `kept_key.watch`,
/// and the kept string are the five records that remain when the drop entry
/// and `watch.drop` are cleared); `collectgarbage("stop")` is omitted because
/// collection runs only at the explicit call. The candidate cannot capture
/// upvalues or return three tables, so `seed` returns `kept_key` and that key
/// holds `owner` and the weak-value watch by strong fields. Concatenation is
/// not lowered here; the string local is cleared so it is not a root.
#[test]
fn lua_candidate_weak_object_keys_retain_string_values() {
    let source = r#"
local function seed()
  local mt = {}
  mt.__mode = "@mode@"
  local owner = {}
  setmetatable(owner, mt)
  local kept_key = {}
  local drop = {}
  local watch_mt = {}
  watch_mt.__mode = "v"
  local watch = {}
  setmetatable(watch, watch_mt)
  local kept = "stored-42"
  owner[kept_key] = kept
  kept = nil
  local stored = "drop-7"
  owner[drop] = stored
  stored = nil
  watch.drop = drop
  kept_key.owner = owner
  kept_key.watch = watch
  observe_table(drop, 0)
  return kept_key
end
local kept_key = seed()
@collect@
local owner = kept_key.owner
local watch = kept_key.watch
@return@
"#;
    let collect = "collectgarbage(\"collect\")\n";
    for mode in ["k", "v", "kv"] {
        for (do_collect, drop_result, drop_alive, edges, collections) in [
            (
                true,
                if mode == "v" { "table" } else { "nil" },
                mode == "v",
                if mode == "v" { 7 } else { 5 },
                1,
            ),
            (false, "table", true, 7, 0),
        ] {
            let body = source
                .replace("@mode@", mode)
                .replace("@collect@", if do_collect { collect } else { "" });
            assert_object_string(
                &body.replace("@return@", "return owner[kept_key]\n"),
                "stored-42",
                drop_alive,
                edges,
                collections,
            );
            assert_object_string(
                &body.replace("@return@", "return type(watch.drop)\n"),
                drop_result,
                drop_alive,
                edges,
                collections,
            );
        }
    }
}

/// One object key reuses one edge for a table, a string, and nil.
///
/// A later generation of the same slot must not read the cleared string.
/// `live_edges` is the shared quota: overwrite does not add a record, nil
/// releases it, and a second key consumes a second record.
#[test]
fn lua_candidate_object_key_string_reuses_one_edge_record() {
    let overwrite = r#"
local owner = {}
local key = {}
local other = {}
local value = {}
observe_table(value, 0)
owner[key] = "stored-42"
owner[other] = value
owner[key] = value
observe_table(owner[key], 0)
owner[key] = "stored-42"
owner[other] = nil
return owner[key]
"#;
    let mut vm = candidate(overwrite);
    assert_eq!(
        vm.run()
            .expect("overwrite an object key between table and string"),
        VmStatus::Halted
    );
    assert_eq!(vm.stack(), &[Value::string("stored-42")]);
    let tables = vm
        .host_context()
        .scope_state::<Vec<ObservedTable>>()
        .unwrap()
        .clone();
    let [value, via_key] = tables.as_slice() else {
        panic!("expected the table value and its readback");
    };
    assert_eq!(via_key, value, "string to table keeps the issued identity");
    assert_eq!(heap::stats(&mut vm.host_context()).unwrap().live_edges, 1);

    let reused = r#"
local owner = {}
local key = {}
observe_table(key, 0)
owner[key] = "stored-42"
owner[key] = nil
key = nil
collectgarbage("collect")
local reused = {}
observe_table(reused, 0)
return type(owner[reused])
"#;
    let mut vm = candidate(reused);
    assert_eq!(
        vm.run()
            .expect("a reused object-key slot does not revive the string"),
        VmStatus::Halted
    );
    assert_eq!(vm.stack(), &[Value::string("nil")]);
    let mut host = vm.host_context();
    let tables = host.scope_state::<Vec<ObservedTable>>().unwrap().clone();
    let [key, reused] = tables.as_slice() else {
        panic!("expected the old key and the reused slot");
    };
    assert_eq!(reused.table.domain(), key.table.domain());
    assert_eq!(reused.table.kind(), key.table.kind());
    assert_eq!(reused.table.slot(), key.table.slot());
    assert_ne!(reused.table.generation(), key.table.generation());
    let resource_key = heap::LuaTableResource::key();
    assert!(
        host.borrow_resource_with_key::<heap::LuaTableResource>(key.token, &resource_key)
            .is_err()
    );
    assert!(
        host.borrow_resource_with_key::<heap::LuaTableResource>(reused.token, &resource_key)
            .is_ok()
    );
    let stats = heap::stats(&mut host).unwrap();
    assert_eq!(stats.live_edges, 0);
    assert_eq!(stats.collections, 1);
}

fn assert_object_string(
    source: &str,
    result: &str,
    drop_alive: bool,
    edges: u32,
    collections: u32,
) {
    let mut vm = candidate(source);
    assert_eq!(vm.run().expect("object-key string"), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::string(result)]);
    let mut host = vm.host_context();
    let observed = host.scope_state::<Vec<ObservedTable>>().unwrap().clone();
    let [drop] = observed.as_slice() else {
        panic!("expected the dropped object key");
    };
    let resource_key = heap::LuaTableResource::key();
    assert_eq!(
        heap::with_heap(&mut host, |heap| Ok(heap.alive(drop.table))).unwrap(),
        drop_alive,
        "dropped key heap node"
    );
    assert_eq!(
        host.borrow_resource_with_key::<heap::LuaTableResource>(drop.token, &resource_key)
            .is_ok(),
        drop_alive,
        "dropped key Core token"
    );
    let stats = heap::stats(&mut host).unwrap();
    assert_eq!(stats.live_edges, edges, "stored entries after collection");
    assert_eq!(stats.collections, collections);
}

#[test]
fn lua_candidate_bracket_numbers_do_not_alias_private_field_ids() {
    let dynamic = r#"
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
    let mut vm = candidate(dynamic);
    assert_eq!(vm.run().expect("dynamic numeric read"), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::string("nil")]);

    let read = r#"
local owner = {}
local planted = {}
local named = {}
plant_private_field(owner, planted)
owner.field = named
owner["1"] = "text"
observe_table(owner, 0)
observe_table(planted, 0)
observe_table(named, 0)
return type(owner[1])
"#;
    let mut vm = candidate(read);
    assert_eq!(
        vm.run().expect("numeric read beside field 1"),
        VmStatus::Halted
    );
    assert_eq!(vm.stack(), &[Value::string("nil")]);
    assert_private_field_untouched(&mut vm, 3);

    let written = r#"
local owner = {}
local planted = {}
local named = {}
plant_private_field(owner, planted)
owner.field = named
owner["1"] = "text"
owner[1] = "num"
owner[1.0] = "num"
observe_table(owner, 0)
observe_table(planted, 0)
observe_table(named, 0)
return owner[1]
"#;
    let mut vm = candidate(written);
    assert_eq!(vm.run().expect("numeric write"), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::string("num")]);
    assert_private_field_untouched(&mut vm, 4);
    let owner = observed_tables(&mut vm)[0];
    let text = heap::with_heap(&mut vm.host_context(), |heap| {
        heap.named_value(owner.table, "1")
    })
    .unwrap();
    assert_eq!(text, Value::string("text"));
}

fn assert_private_field_untouched(vm: &mut vm::Vm, edges: u32) {
    let tables = observed_tables(vm);
    let [owner, planted, named] = tables.as_slice() else {
        panic!("expected owner, planted field, and named field");
    };
    let field = heap::with_heap(&mut vm.host_context(), |heap| heap.edge(owner.table, 1)).unwrap();
    assert_eq!(
        field,
        Some(planted.table),
        "field id 1 stays the planted table"
    );
    let by_name = heap::with_heap(&mut vm.host_context(), |heap| {
        heap.named_edge(owner.table, "field")
    })
    .unwrap();
    assert_eq!(by_name, Some(named.table));
    assert_eq!(
        heap::stats(&mut vm.host_context()).unwrap().live_edges,
        edges
    );
}

fn observed_tables(vm: &mut vm::Vm) -> Vec<ObservedTable> {
    vm.host_context()
        .scope_state::<Vec<ObservedTable>>()
        .unwrap()
        .clone()
}

#[test]
fn lua_candidate_numeric_key_reuses_one_record() {
    let overwrite = r#"
local owner = {}
local value = {}
owner[1] = "first"
local k = 1.0
owner[k] = value
observe_table(value, 0)
observe_table(owner[1], 0)
owner[1] = "second"
return owner[1.0]
"#;
    let mut vm = candidate(overwrite);
    assert_eq!(
        vm.run().expect("overwrite one numeric record"),
        VmStatus::Halted
    );
    assert_eq!(vm.stack(), &[Value::string("second")]);
    let tables = observed_tables(&mut vm);
    assert_eq!(
        tables[0], tables[1],
        "literal and dynamic read the same table"
    );
    assert_eq!(heap::stats(&mut vm.host_context()).unwrap().live_edges, 1);

    let missing = "local owner = {}\nowner[2] = nil\nreturn type(owner[2])\n";
    let mut vm = candidate(missing);
    assert_eq!(
        vm.run().expect("nil write to a missing numeric key"),
        VmStatus::Halted
    );
    assert_eq!(vm.stack(), &[Value::string("nil")]);
    assert_eq!(heap::stats(&mut vm.host_context()).unwrap().live_edges, 0);

    let deleted = r#"
local owner = {}
local value = {}
value.self = value
observe_table(value, 0)
owner[1] = value
value = nil
owner[1] = nil
return type(owner[1])
"#;
    for collect in [false, true] {
        let source = if collect {
            deleted.replace(
                "return type(owner[1])",
                "collectgarbage(\"collect\")\nreturn type(owner[1])",
            )
        } else {
            deleted.to_string()
        };
        let mut vm = candidate(&source);
        assert_eq!(vm.run().expect("nil delete"), VmStatus::Halted, "{collect}");
        assert_eq!(vm.stack(), &[Value::string("nil")]);
        let value = observed_tables(&mut vm)[0];
        let alive =
            heap::with_heap(&mut vm.host_context(), |heap| Ok(heap.alive(value.table))).unwrap();
        let token_alive = vm
            .host_context()
            .borrow_resource_with_key::<heap::LuaTableResource>(
                value.token,
                &heap::LuaTableResource::key(),
            )
            .is_ok();
        assert_eq!(alive, !collect, "heap node after nil delete");
        assert_eq!(token_alive, !collect, "token after nil delete");
        let stats = heap::stats(&mut vm.host_context()).unwrap();
        // The numeric record is gone. The value's own self edge remains until
        // the explicit collection that reclaims the unrooted node.
        assert_eq!(stats.live_edges, u32::from(!collect));
        assert_eq!(stats.collections, u32::from(collect));
    }
}

#[test]
fn lua_candidate_numeric_fraction_does_not_alias_one_or_field_id() {
    let source = r#"
local owner = {}
local planted = {}
plant_private_field(owner, planted)
owner[1.5] = "half"
local k = 1.5
observe_table(owner, 0)
observe_table(planted, 0)
return owner[k]
"#;
    let mut vm = candidate(source);
    assert_eq!(vm.run().expect("fraction key"), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::string("half")]);
    let tables = observed_tables(&mut vm);
    let field =
        heap::with_heap(&mut vm.host_context(), |heap| heap.edge(tables[0].table, 1)).unwrap();
    assert_eq!(field, Some(tables[1].table));
    assert_eq!(heap::stats(&mut vm.host_context()).unwrap().live_edges, 2);

    let other = source.replace("return owner[k]", "return type(owner[1])");
    let mut vm = candidate(&other);
    assert_eq!(
        vm.run().expect("integer key stays absent"),
        VmStatus::Halted
    );
    assert_eq!(vm.stack(), &[Value::string("nil")]);
    let text = source.replace("return owner[k]", "return type(owner[\"1.5\"])");
    let mut vm = candidate(&text);
    assert_eq!(vm.run().expect("text key stays absent"), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::string("nil")]);
}

#[test]
fn lua_candidate_numeric_zero_does_not_alias_mode_field() {
    let mode = r#"
local mt = {}
mt.__mode = "v"
mt[0] = "zero"
return mt.__mode
"#;
    let mut vm = candidate(mode);
    assert_eq!(vm.run().expect("mode field"), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::string("v")]);
    assert_eq!(heap::stats(&mut vm.host_context()).unwrap().live_edges, 2);

    let zero = mode.replace("return mt.__mode", "return mt[0]");
    let mut vm = candidate(&zero);
    assert_eq!(vm.run().expect("numeric zero"), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::string("zero")]);
    assert_eq!(heap::stats(&mut vm.host_context()).unwrap().live_edges, 2);
}

#[test]
fn lua_candidate_weak_numeric_value_clears_only_after_collect() {
    let source = r#"
local mt = {}
mt.__mode = "v"
local owner = {}
setmetatable(owner, mt)
local value = {}
value.self = value
local orphan = {}
orphan.self = orphan
owner[1] = value
observe_table(orphan, 0)
observe_table(value, 0)
orphan = nil
@collect@
return type(owner[1])
"#;
    let omitted = source.replace("@collect@\n", "");
    assert_weak_probe(&omitted, "table", &[true, true], 4, 0);

    let rooted = source.replace("@collect@", "collectgarbage(\"collect\")");
    assert_weak_probe(&rooted, "table", &[false, true], 3, 1);

    let released = r#"
local mt = {}
mt.__mode = "v"
local owner = {}
setmetatable(owner, mt)
local value = {}
value.self = value
owner[1] = value
observe_table(value, 0)
value = nil
@collect@
return type(owner[1])
"#;
    assert_weak_probe(&released.replace("@collect@\n", ""), "table", &[true], 3, 0);
    assert_weak_probe(
        &released.replace("@collect@", "collectgarbage(\"collect\")"),
        "nil",
        &[false],
        1,
        1,
    );
}

#[test]
fn lua_candidate_signed_zero_number_keys_share_one_record() {
    let through_zero = r#"
local neg = float_bits(1)
local owner = {}
owner[neg] = "neg"
return owner[0]
"#;
    let mut vm = candidate(through_zero);
    assert_eq!(vm.run().expect("read +0 after -0 write"), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::string("neg")]);
    assert_eq!(heap::stats(&mut vm.host_context()).unwrap().live_edges, 1);

    let through_neg = r#"
local neg = float_bits(1)
local owner = {}
owner[0] = "pos"
return owner[neg]
"#;
    let mut vm = candidate(through_neg);
    assert_eq!(vm.run().expect("read -0 after +0 write"), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::string("pos")]);
    assert_eq!(heap::stats(&mut vm.host_context()).unwrap().live_edges, 1);

    let overwrite = r#"
local neg = float_bits(1)
local owner = {}
owner[0] = "pos"
owner[neg] = "neg"
return owner[0]
"#;
    let mut vm = candidate(overwrite);
    assert_eq!(vm.run().expect("signed zero overwrite"), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::string("neg")]);
    assert_eq!(heap::stats(&mut vm.host_context()).unwrap().live_edges, 1);
}

#[test]
fn lua_candidate_infinities_are_distinct_number_keys() {
    let source = r#"
local pos = float_bits(3)
local neg = float_bits(4)
local owner = {}
owner[pos] = "pos"
owner[neg] = "neg"
return owner[@which@]
"#;
    for (which, expected) in [("pos", "pos"), ("neg", "neg")] {
        let mut vm = candidate(&source.replace("@which@", which));
        assert_eq!(vm.run().expect(which), VmStatus::Halted);
        assert_eq!(vm.stack(), &[Value::string(expected)]);
        assert_eq!(heap::stats(&mut vm.host_context()).unwrap().live_edges, 2);
    }
}

#[test]
fn lua_candidate_nan_number_key_read_is_nil() {
    let source = r#"
local owner = {}
owner[1] = "keep"
local k = float_bits(2)
return type(owner[k])
"#;
    let mut vm = candidate(source);
    assert_eq!(vm.run().expect("NaN key read"), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::string("nil")]);
    assert_eq!(heap::stats(&mut vm.host_context()).unwrap().live_edges, 1);
}

#[test]
fn lua_candidate_nan_number_key_string_write_is_refused() {
    let source = r#"
local owner = {}
owner[1] = "keep"
local k = float_bits(2)
observe_table(owner, 0)
owner[k] = "nope"
"#;
    assert_nan_index(source);
}

#[test]
fn lua_candidate_nan_number_key_nil_write_is_refused() {
    let source = r#"
local owner = {}
owner[1] = "keep"
local k = float_bits(2)
observe_table(owner, 0)
owner[k] = nil
"#;
    assert_nan_index(source);
}

fn assert_nan_index(source: &str) {
    let mut vm = candidate(source);
    let error = vm.run().expect_err("NaN key write");
    assert!(
        matches!(error, vm::VmError::HostError(ref message) if message == "table index is NaN"),
        "{error}"
    );
    assert_eq!(heap::stats(&mut vm.host_context()).unwrap().live_edges, 1);
    assert_eq!(heap::stats(&mut vm.host_context()).unwrap().collections, 0);
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

fn assert_table_lifetime(host: &mut vm::HostContext<'_>, table: ObservedTable, alive: bool) {
    let resource_key = heap::LuaTableResource::key();
    assert_eq!(
        heap::with_heap(host, |heap| Ok(heap.alive(table.table))).unwrap(),
        alive,
        "heap node {table:?}"
    );
    assert_eq!(
        host.borrow_resource_with_key::<heap::LuaTableResource>(table.token, &resource_key)
            .is_ok(),
        alive,
        "resource token {table:?}"
    );
}

// Chunk `do` normal exit. The inner `value` is a different binding from the
// outer one. An explicit collect inside the block must keep both, because the
// outer binding is still in its lifetime while shadowed. Leaving the block
// removes only the inner slot from the safepoint roots. It does not assign nil
// and does not roll the root list back. The weak `drop` entry therefore dies
// only on the next explicit collect; omitting that collect leaves the same table.
#[test]
fn lua_candidate_chunk_do_normal_exit_releases_shadow() {
    let source = r#"
local mt = {}
mt.__mode = "v"
local owner = {}
setmetatable(owner, mt)
local value = {}
owner.keep = value
observe_table(value, 0)
do
  local value = {}
  owner.drop = value
  observe_table(value, 0)
  collectgarbage("collect")
  observe_table(owner.keep, 0)
  observe_table(owner.drop, 0)
end
@after@
"#;
    let collect_after = "\
collectgarbage(\"collect\")
observe_table(value, 0)
observe_table(owner.keep, 0)
return type(owner.drop)
";
    let omit_after = "\
observe_table(value, 0)
observe_table(owner.keep, 0)
observe_table(owner.drop, 0)
return type(owner.drop)
";
    for (after, result, collections, live_nodes, live_edges, resources) in [
        (collect_after, "nil", 2, 3, 2, 3),
        (omit_after, "table", 1, 4, 3, 4),
    ] {
        let mut vm = candidate(&source.replace("@after@", after));
        assert_eq!(vm.run().expect("run chunk do"), VmStatus::Halted);
        assert_eq!(vm.stack(), &[Value::string(result)], "{result}");
        let mut host = vm.host_context();
        let tables = host.scope_state::<Vec<ObservedTable>>().unwrap().clone();
        let outer = tables[0];
        let inner = tables[1];
        let keep_inside = tables[2];
        let drop_inside = tables[3];
        let outer_after = tables[4];
        let keep_after = tables[5];
        assert_eq!(
            keep_inside, outer,
            "in-block collect keeps the outer object"
        );
        assert_eq!(
            drop_inside, inner,
            "in-block collect keeps the inner object"
        );
        assert_eq!(outer_after, outer, "the restored name is the outer object");
        assert_eq!(keep_after, outer, "owner.keep is the outer object");
        let drop_alive = result == "table";
        if drop_alive {
            assert_eq!(tables[6], inner, "omitted collect leaves the inner object");
        }
        for (table, alive) in [
            (outer, true),
            (inner, drop_alive),
            (keep_inside, true),
            (drop_inside, drop_alive),
            (outer_after, true),
            (keep_after, true),
        ] {
            assert_table_lifetime(&mut host, table, alive);
        }
        if drop_alive {
            assert_table_lifetime(&mut host, tables[6], true);
        }
        let stats = heap::stats(&mut host).unwrap();
        assert_eq!(stats.collections, collections, "{result}");
        assert_eq!(stats.live_nodes, live_nodes, "{result}");
        assert_eq!(stats.live_edges, live_edges, "{result}");
        assert_eq!(stats.live_roots, 0, "{result}");
        assert_eq!(host.resource_count(), resources, "{result}");
    }
}

// Nested normal exit retires only that block's slot. Clearing a non-shadowed
// outer local stays cleared: leaving the block must not put that slot back.
#[test]
fn lua_candidate_chunk_do_normal_exit_keeps_cleared_outer_dead() {
    let source = r#"
local mt = {}
mt.__mode = "v"
local owner = {}
setmetatable(owner, mt)
local value = {}
local cleared = {}
owner.keep = value
owner.cleared = cleared
observe_table(value, 0)
observe_table(cleared, 0)
do
  cleared = nil
  local mid = {}
  owner.mid = mid
  observe_table(mid, 0)
  do
    local value = {}
    owner.drop = value
    observe_table(value, 0)
  end
  collectgarbage("collect")
  observe_table(owner.keep, 0)
  observe_table(owner.mid, 0)
end
collectgarbage("collect")
observe_table(value, 0)
observe_table(owner.keep, 0)
return type(owner.mid)
"#;
    let mut vm = candidate(source);
    assert_eq!(vm.run().expect("run nested chunk do"), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::string("nil")]);
    let mut host = vm.host_context();
    let tables = host.scope_state::<Vec<ObservedTable>>().unwrap().clone();
    let [
        outer,
        cleared,
        mid,
        inner,
        keep_inside,
        mid_inside,
        outer_after,
        keep_after,
    ] = tables.as_slice()
    else {
        panic!("expected eight observed tables, got {}", tables.len());
    };
    assert_eq!(
        *keep_inside, *outer,
        "shadowed outer stays live inside the block"
    );
    assert_eq!(*mid_inside, *mid, "the mid binding is still in scope");
    assert_eq!(*outer_after, *outer);
    assert_eq!(*keep_after, *outer);
    for (table, alive) in [
        (*outer, true),
        (*cleared, false),
        (*mid, false),
        (*inner, false),
        (*keep_inside, true),
        (*mid_inside, false),
        (*outer_after, true),
        (*keep_after, true),
    ] {
        assert_table_lifetime(&mut host, table, alive);
    }
    let stats = heap::stats(&mut host).unwrap();
    // First collect drops cleared and the nested value. Second collect drops mid.
    assert_eq!(
        (
            stats.collections,
            stats.live_nodes,
            stats.live_edges,
            stats.live_roots
        ),
        (2, 3, 2, 0)
    );
    assert_eq!(host.resource_count(), 3);
}

// Chunk `do` receives two tables from one synchronous call. The caller frame
// roots stay live through an explicit collect inside the block, so both weak
// entries keep the same identity. Leaving the block drops those two slots.
// The next collect clears the entries and closes their nodes and tokens.
// Omitting that collect leaves both entries. The outer call result is a
// different slot and stays. This is not a function body and not a goto.
#[test]
fn lua_candidate_chunk_do_releases_multi_return_roots() {
    let source = r#"
local function pair()
  local left = {}
  local right = {}
  local a = left
  local b = right
  return a, b
end
local function hold()
  local kept = {}
  local alias = kept
  return alias
end
local mt = {}
mt.__mode = "v"
local owner = {}
setmetatable(owner, mt)
local kept = hold()
owner.keep = kept
observe_table(kept, 0)
do
  local left, right = pair()
  owner.left = left
  owner.right = right
  observe_table(left, 0)
  observe_table(right, 0)
  collectgarbage("collect")
  observe_table(owner.left, 0)
  observe_table(owner.right, 0)
  observe_table(owner.keep, 0)
end
@after@
"#;
    let collect_after = "\
collectgarbage(\"collect\")
observe_table(kept, 0)
observe_table(owner.keep, 0)
return type(owner.@field@)
";
    let omit_after = "\
observe_table(owner.left, 0)
observe_table(owner.right, 0)
observe_table(kept, 0)
observe_table(owner.keep, 0)
return type(owner.@field@)
";
    for (after, result, collections, live_nodes, live_edges, resources) in [
        (collect_after, "nil", 2u32, 3u32, 2u32, 3usize),
        (omit_after, "table", 1, 5, 4, 5),
    ] {
        for field in ["left", "right"] {
            let mut vm = candidate(&source.replace("@after@", &after.replace("@field@", field)));
            assert_eq!(
                vm.run().expect("run do multi-return"),
                VmStatus::Halted,
                "{result} {field}"
            );
            assert_eq!(vm.stack(), &[Value::string(result)], "{result} {field}");
            let mut host = vm.host_context();
            let tables = host.scope_state::<Vec<ObservedTable>>().unwrap().clone();
            let kept = tables[0];
            let left = tables[1];
            let right = tables[2];
            let left_inside = tables[3];
            let right_inside = tables[4];
            let keep_inside = tables[5];
            assert_eq!(left_inside, left, "in-block collect keeps the left result");
            assert_eq!(
                right_inside, right,
                "in-block collect keeps the right result"
            );
            assert_eq!(keep_inside, kept, "in-block collect keeps the outer result");
            let drop_alive = result == "table";
            let (kept_after, keep_after) = if drop_alive {
                assert_eq!(tables[6], left, "omitted collect leaves the left entry");
                assert_eq!(tables[7], right, "omitted collect leaves the right entry");
                (tables[8], tables[9])
            } else {
                (tables[6], tables[7])
            };
            assert_eq!(kept_after, kept, "the outer local is the held result");
            assert_eq!(keep_after, kept, "owner.keep is the outer call result");
            for (table, alive) in [
                (kept, true),
                (left, drop_alive),
                (right, drop_alive),
                (left_inside, drop_alive),
                (right_inside, drop_alive),
                (keep_inside, true),
                (kept_after, true),
                (keep_after, true),
            ] {
                assert_table_lifetime(&mut host, table, alive);
            }
            if drop_alive {
                assert_table_lifetime(&mut host, tables[6], true);
                assert_table_lifetime(&mut host, tables[7], true);
            }
            let stats = heap::stats(&mut host).unwrap();
            assert_eq!(stats.collections, collections, "{result} {field}");
            assert_eq!(stats.live_nodes, live_nodes, "{result} {field}");
            assert_eq!(stats.live_edges, live_edges, "{result} {field}");
            assert_eq!(stats.live_roots, 0, "{result} {field}");
            assert_eq!(host.resource_count(), resources, "{result} {field}");
        }
    }
}

// Nested chunk `do` retires only that block's caller slots. An explicit nil
// inside the inner block has already cleared that slot; the sibling result
// stays until the inner block ends. The enclosing block's call results stay
// until that block ends. The outer call result stays through both exits.
#[test]
fn lua_candidate_chunk_do_releases_nested_multi_return_roots() {
    let setup = r#"
local function pair()
  local left = {}
  local right = {}
  local a = left
  local b = right
  return a, b
end
local function hold()
  local kept = {}
  local alias = kept
  return alias
end
local mt = {}
mt.__mode = "v"
local owner = {}
setmetatable(owner, mt)
local kept = hold()
owner.keep = kept
observe_table(kept, 0)
do
  local mid_left, mid_right = pair()
  owner.mid_left = mid_left
  owner.mid_right = mid_right
  observe_table(mid_left, 0)
  observe_table(mid_right, 0)
  do
    local left, right = pair()
    owner.left = left
    owner.right = right
    observe_table(left, 0)
    observe_table(right, 0)
    left = nil
    collectgarbage("collect")
    observe_table(owner.right, 0)
"#;
    let nil_inside = r#"
    observe_table(owner.mid_left, 0)
    observe_table(owner.mid_right, 0)
    observe_table(owner.keep, 0)
    return type(owner.left)
  end
end
"#;
    let after_inner = r#"
  end
  collectgarbage("collect")
  observe_table(owner.mid_left, 0)
  observe_table(owner.mid_right, 0)
  observe_table(owner.keep, 0)
  return type(owner.@field@)
end
"#;
    let after_mid = r#"
  end
  collectgarbage("collect")
  observe_table(owner.mid_left, 0)
  observe_table(owner.mid_right, 0)
  observe_table(owner.keep, 0)
end
collectgarbage("collect")
observe_table(kept, 0)
observe_table(owner.keep, 0)
return type(owner.@field@)
"#;
    let nil_source = format!("{setup}{nil_inside}");
    let mut vm = candidate(&nil_source);
    assert_eq!(vm.run().expect("run nil inside do"), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::string("nil")]);
    let mut host = vm.host_context();
    let tables = host.scope_state::<Vec<ObservedTable>>().unwrap().clone();
    let [
        kept,
        mid_left,
        mid_right,
        left,
        right,
        right_inside,
        mid_left_inside,
        mid_right_inside,
        keep_inside,
    ] = tables.as_slice()
    else {
        panic!("expected nine observed tables, got {}", tables.len());
    };
    assert_eq!(
        *right_inside, *right,
        "nil of left does not drop the sibling"
    );
    assert_eq!(*mid_left_inside, *mid_left);
    assert_eq!(*mid_right_inside, *mid_right);
    assert_eq!(*keep_inside, *kept);
    for (table, alive) in [
        (*kept, true),
        (*mid_left, true),
        (*mid_right, true),
        (*left, false),
        (*right, true),
        (*right_inside, true),
        (*mid_left_inside, true),
        (*mid_right_inside, true),
        (*keep_inside, true),
    ] {
        assert_table_lifetime(&mut host, table, alive);
    }

    for field in ["left", "right"] {
        let mut vm = candidate(&format!("{setup}{}", after_inner.replace("@field@", field)));
        assert_eq!(
            vm.run().expect("run after inner do"),
            VmStatus::Halted,
            "{field}"
        );
        assert_eq!(vm.stack(), &[Value::string("nil")], "{field}");
        let mut host = vm.host_context();
        let tables = host.scope_state::<Vec<ObservedTable>>().unwrap().clone();
        let [
            kept,
            mid_left,
            mid_right,
            left,
            right,
            right_inside,
            mid_left_after,
            mid_right_after,
            keep_after,
        ] = tables.as_slice()
        else {
            panic!("expected nine observed tables, got {}", tables.len());
        };
        assert_eq!(
            *right_inside, *right,
            "sibling stays through the in-block collect"
        );
        assert_eq!(
            *mid_left_after, *mid_left,
            "enclosing result stays after inner exit"
        );
        assert_eq!(*mid_right_after, *mid_right);
        assert_eq!(*keep_after, *kept);
        for (table, alive) in [
            (*kept, true),
            (*mid_left, true),
            (*mid_right, true),
            (*left, false),
            (*right, false),
            (*right_inside, false),
            (*mid_left_after, true),
            (*mid_right_after, true),
            (*keep_after, true),
        ] {
            assert_table_lifetime(&mut host, table, alive);
        }
    }

    for field in ["mid_left", "mid_right"] {
        let mut vm = candidate(&format!("{setup}{}", after_mid.replace("@field@", field)));
        assert_eq!(
            vm.run().expect("run after mid do"),
            VmStatus::Halted,
            "{field}"
        );
        assert_eq!(vm.stack(), &[Value::string("nil")], "{field}");
        let mut host = vm.host_context();
        let tables = host.scope_state::<Vec<ObservedTable>>().unwrap().clone();
        let kept = tables[0];
        let mid_left = tables[1];
        let mid_right = tables[2];
        let left = tables[3];
        let right = tables[4];
        let mid_left_inside = tables[6];
        let keep_inside = tables[8];
        let kept_after = tables[9];
        let keep_after = tables[10];
        assert_eq!(
            mid_left_inside, mid_left,
            "mid stays until its own block ends"
        );
        assert_eq!(keep_inside, kept);
        assert_eq!(kept_after, kept);
        assert_eq!(
            keep_after, kept,
            "outer call result stays after both blocks"
        );
        for (table, alive) in [
            (kept, true),
            (mid_left, false),
            (mid_right, false),
            (left, false),
            (right, false),
            (mid_left_inside, false),
            (keep_inside, true),
            (kept_after, true),
            (keep_after, true),
        ] {
            assert_table_lifetime(&mut host, table, alive);
        }
        let stats = heap::stats(&mut host).unwrap();
        assert_eq!(
            (stats.collections, stats.live_nodes, stats.live_roots),
            (3, 3, 0),
            "{field}"
        );
        assert_eq!(host.resource_count(), 3, "{field}");
    }
}

// Chunk `do` receives two tables from one call into one repeated local name.
// Resolver records each name token as its own binding: both share
// `visible_from` at the statement end, and `Binding.span` is that token's
// span. The visible name is the second receiver. An explicit collect inside
// the block keeps both caller-frame roots. Leaving the block drops both
// roots; the next explicit collect closes both nodes and Core tokens. The
// outer local of the same name is visible again. Omitting the post-block
// collect leaves both receivers, so a dead witness is not a missing
// allocation. `observe_table` inside `pair` records issuance and does not
// retain a VM Value. This candidate does not capture the chunk weak table.
// It is not a goto, a production `.lua` entry, a closure, or JIT/AOT.
#[test]
fn lua_candidate_chunk_do_releases_duplicate_receiver_roots() {
    let source = r#"
local function pair()
  local left = {}
  local right = {}
  observe_table(left, 0)
  observe_table(right, 0)
  local a = left
  local b = right
  return a, b
end
local mt = {}
mt.__mode = "v"
local owner = {}
setmetatable(owner, mt)
local x = {}
owner.keep = x
observe_table(x, 0)
do
  local x, x = pair()
  owner.seen = x
  observe_table(x, 0)
  collectgarbage("collect")
  observe_table(owner.seen, 0)
  observe_table(owner.keep, 0)
end
@after@
"#;
    let collect_after = "\
collectgarbage(\"collect\")
observe_table(x, 0)
observe_table(owner.keep, 0)
return type(owner.seen)
";
    let omit_after = "\
observe_table(owner.seen, 0)
observe_table(x, 0)
observe_table(owner.keep, 0)
return type(owner.seen)
";
    for (after, result, collections, live_nodes, live_edges, resources) in [
        (omit_after, "table", 1u32, 5u32, 3u32, 5usize),
        (collect_after, "nil", 2, 3, 2, 3),
    ] {
        let mut vm = candidate(&source.replace("@after@", after));
        assert_eq!(
            vm.run().expect("run duplicate receivers"),
            VmStatus::Halted,
            "{result}"
        );
        assert_eq!(vm.stack(), &[Value::string(result)], "{result}");
        let mut host = vm.host_context();
        let tables = host.scope_state::<Vec<ObservedTable>>().unwrap().clone();
        // pair runs inside the block, so its issuance witnesses follow the
        // outer observation recorded before the call.
        let outer = tables[0];
        let left = tables[1];
        let right = tables[2];
        let visible = tables[3];
        let seen_inside = tables[4];
        let keep_inside = tables[5];
        assert_ne!(left, right, "pair returns two tables");
        assert_eq!(visible, right, "the visible name is the second receiver");
        assert_ne!(
            visible, left,
            "the shadowed receiver is not the visible name"
        );
        assert_eq!(
            seen_inside, right,
            "in-block collect keeps the visible receiver"
        );
        assert_eq!(
            keep_inside, outer,
            "in-block collect keeps the outer object"
        );
        let drop_alive = result == "table";
        let (seen_after, restored, keep_after) = if drop_alive {
            assert_eq!(
                tables[6], right,
                "omitted collect leaves the visible receiver"
            );
            (tables[6], tables[7], tables[8])
        } else {
            (right, tables[6], tables[7])
        };
        assert_eq!(restored, outer, "the restored name is the outer object");
        assert_ne!(restored, right, "block exit does not keep the inner name");
        assert_eq!(keep_after, outer, "owner.keep is the outer object");
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.alive(left.table))).unwrap(),
            drop_alive,
            "shadowed receiver after the block; result={result}"
        );
        for (table, alive) in [
            (left, drop_alive),
            (right, drop_alive),
            (outer, true),
            (visible, drop_alive),
            (seen_inside, drop_alive),
            (keep_inside, true),
            (seen_after, drop_alive),
            (restored, true),
            (keep_after, true),
        ] {
            assert_table_lifetime(&mut host, table, alive);
        }
        let stats = heap::stats(&mut host).unwrap();
        assert_eq!(stats.collections, collections, "{result}");
        assert_eq!(stats.live_nodes, live_nodes, "{result}");
        assert_eq!(stats.live_edges, live_edges, "{result}");
        assert_eq!(stats.live_roots, 0, "{result}");
        assert_eq!(host.resource_count(), resources, "{result}");
    }
}

fn assert_halted_string(source: &str, result: &str, edges: u32) {
    let mut vm = candidate(source);
    assert_eq!(vm.run().expect("string-key program"), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::string(result)]);
    let stats = heap::stats(&mut vm.host_context()).unwrap();
    assert_eq!(stats.live_edges, edges, "stored entries");
    assert_eq!(stats.collections, 0);
}

/// `t.name`, `t["name"]`, and `t[k]` are one record and one edge quota.
#[test]
fn lua_candidate_string_key_spellings_share_one_record() {
    let shared = r#"
local owner = {}
local value = {}
owner.name = value
local bracket = owner["name"]
local k = "name"
local dynamic = owner[k]
observe_table(value, 0)
observe_table(bracket, 0)
observe_table(dynamic, 0)
return type(owner.name)
"#;
    let mut vm = candidate(shared);
    assert_eq!(vm.run().expect("shared string key"), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::string("table")]);
    let observed = vm
        .host_context()
        .scope_state::<Vec<ObservedTable>>()
        .unwrap()
        .clone();
    assert_eq!(observed.len(), 3);
    assert_eq!(
        observed[0], observed[1],
        "bracket literal reads the dot record"
    );
    assert_eq!(
        observed[0], observed[2],
        "dynamic string reads the dot record"
    );
    let stats = heap::stats(&mut vm.host_context()).unwrap();
    assert_eq!(stats.live_edges, 1, "three spellings share one edge");
    assert_eq!(stats.collections, 0);

    assert_halted_string(
        r#"
local owner = {}
local value = {}
owner["name"] = value
local k = "name"
owner[k] = value
owner.name = value
return type(owner["name"])
"#,
        "table",
        1,
    );
    assert_halted_string(
        r#"
local owner = {}
local k = "name"
owner.name = {}
owner[k] = nil
return type(owner["name"])
"#,
        "nil",
        0,
    );
    assert_halted_string(
        r#"
local owner = {}
owner["name"] = {}
owner.name = nil
local k = "name"
return type(owner[k])
"#,
        "nil",
        0,
    );
    assert_halted_string(
        r#"
local owner = {}
local key = {}
local value = {}
owner[key] = value
owner.name = value
owner["other"] = value
return type(owner.name)
"#,
        "table",
        3,
    );
}

/// A string local used as a key is the same record as the dot field.
#[test]
fn lua_candidate_string_key_dynamic_read_uses_the_same_record() {
    assert_halted_string(
        r#"
local owner = {}
local value = {}
owner.name = value
local k = "name"
return type(owner[k])
"#,
        "table",
        1,
    );
    assert_halted_string(
        r#"
local owner = {}
local k = "name"
owner[k] = {}
return type(owner.name)
"#,
        "table",
        1,
    );
    assert_halted_string(
        r#"
local owner = {}
local k = "name"
owner.name = {}
owner[k] = nil
return type(owner.name)
"#,
        "nil",
        0,
    );
}

/// Full UTF-8 text is the key. A NUL does not end the key or collapse a prefix.
#[test]
fn lua_candidate_string_key_full_bytes_stay_distinct() {
    let accent = "caf\u{e9}";
    assert_halted_string(
        &format!(
            r#"
local owner = {{}}
local accent = "{accent}"
owner[accent] = "accented"
owner["{accent}"] = "accented"
owner.cafe = "plain"
return owner["{accent}"]
"#
        ),
        "accented",
        2,
    );
    assert_halted_string(
        r#"
local owner = {}
owner.cafe = "plain"
return owner.cafe
"#,
        "plain",
        1,
    );
    assert_halted_string(
        r#"
local owner = {}
local prefix = "ab\0"
owner[prefix] = "cut"
owner["ab"] = "short"
owner["ab\0c"] = "full"
return owner[prefix]
"#,
        "cut",
        3,
    );
    assert_halted_string(
        r#"
local owner = {}
local prefix = "ab\0"
owner[prefix] = "cut"
owner["ab"] = "short"
owner["ab\0c"] = "full"
return owner["ab"]
"#,
        "short",
        3,
    );
    assert_halted_string(
        r#"
local owner = {}
local prefix = "ab\0"
owner[prefix] = "cut"
owner["ab"] = "short"
owner["ab\0c"] = "full"
return owner["ab\0c"]
"#,
        "full",
        3,
    );
    assert_halted_string(
        r#"
local owner = {}
local prefix = "ab\0"
owner[prefix] = "cut"
owner["ab"] = "short"
owner["ab\0c"] = "full"
return type(owner["ab\0d"])
"#,
        "nil",
        3,
    );
}

/// `mt.__mode` and `mt[k]` with `k == "__mode"` are one scanned record.
/// A longer key that only starts with `__mode` is not a mode.
#[test]
fn lua_candidate_string_key_mode_enters_the_weak_scan() {
    let collect = r#"
local mt = {}
local k = "__mode"
mt[k] = "v"
local owner = {}
setmetatable(owner, mt)
local drop = {}
local rooted = {}
local name = "name"
owner[name] = drop
observe_table(drop, 0)
observe_table(rooted, 0)
drop = nil
name = nil
collectgarbage("collect")
return type(owner.name)
"#;
    assert_weak_probe(collect, "nil", &[false, true], 1, 1);
    let omitted = collect.replace(r#"collectgarbage("collect")"#, "");
    assert_weak_probe(&omitted, "table", &[true, true], 2, 0);

    let keep_value = collect.replace("mt[k] = \"v\"", "mt[k] = \"k\"");
    assert_weak_probe(&keep_value, "table", &[true, true], 2, 1);
    let clear_value = collect.replace("mt[k] = \"v\"", "mt[k] = \"kv\"");
    assert_weak_probe(&clear_value, "nil", &[false, true], 1, 1);

    let note = r#"
local mt = {}
local k = "note"
mt[k] = "v"
local owner = {}
setmetatable(owner, mt)
local drop = {}
owner.name = drop
observe_table(drop, 0)
drop = nil
collectgarbage("collect")
return type(owner.name)
"#;
    assert_weak_probe(note, "table", &[true], 2, 1);

    let prefix = r#"
local mt = {}
local k = "__mode\0x"
mt[k] = "v"
local owner = {}
setmetatable(owner, mt)
local drop = {}
owner.name = drop
observe_table(drop, 0)
drop = nil
collectgarbage("collect")
return type(owner.name)
"#;
    assert_weak_probe(prefix, "table", &[true], 2, 1);

    let overwritten = r#"
local mt = {}
mt.__mode = "x"
local k = "__mode"
mt[k] = "v"
local owner = {}
setmetatable(owner, mt)
local drop = {}
local rooted = {}
owner.name = drop
observe_table(drop, 0)
observe_table(rooted, 0)
drop = nil
collectgarbage("collect")
return type(owner.name)
"#;
    assert_weak_probe(overwritten, "nil", &[false, true], 1, 1);
    let overwritten_omitted = overwritten.replace(r#"collectgarbage("collect")"#, "");
    assert_weak_probe(&overwritten_omitted, "table", &[true, true], 2, 0);

    let restored = r#"
local mt = {}
local k = "__mode"
mt[k] = "v"
mt.__mode = "x"
local owner = {}
setmetatable(owner, mt)
local drop = {}
owner.name = drop
observe_table(drop, 0)
drop = nil
collectgarbage("collect")
return type(owner["name"])
"#;
    assert_weak_probe(restored, "table", &[true], 2, 1);
    assert_halted_string(
        r#"
local mt = {}
mt.__mode = "v"
local k = "__mode"
mt[k] = "kv"
return mt.__mode
"#,
        "kv",
        1,
    );
    assert_halted_string(
        r#"
local mt = {}
local k = "__mode"
mt[k] = "v"
return mt["__mode"]
"#,
        "v",
        1,
    );
}

// Chunk `do` jumps to its own trailing end label. The resolver target is that
// label, not a same-named label in another block. Statements after the goto
// are not lowered, so `marker.ran = x` does not publish a strong edge. Both
// receivers from `pair` stay live through the in-block collect, including the
// shadowed first slot. Leaving the block retires those executed bindings.
// The next collect closes both nodes and Core tokens. Omitting it leaves
// them. The outer `x` is a different binding and stays. An adjacent label
// would not show this: the skipped field write is what the jump must avoid.
#[test]
fn lua_candidate_chunk_do_tail_goto_releases_executed_roots() {
    let source = r#"
local function pair()
  local left = {}
  local right = {}
  observe_table(left, 0)
  observe_table(right, 0)
  local a = left
  local b = right
  return a, b
end
local mt = {}
mt.__mode = "v"
local owner = {}
setmetatable(owner, mt)
local marker = {}
local x = {}
owner.keep = x
observe_table(x, 0)
observe_table(marker, 0)
do
  local x, x = pair()
  owner.seen = x
  observe_table(x, 0)
  collectgarbage("collect")
  observe_table(owner.seen, 0)
  observe_table(owner.keep, 0)
  goto tail
  marker.ran = x
  ::tail::
end
@after@
"#;
    let collect_after = "\
collectgarbage(\"collect\")
observe_table(x, 0)
observe_table(owner.keep, 0)
observe_table(marker, 0)
return type(owner.seen)
";
    let omit_after = "\
observe_table(owner.seen, 0)
observe_table(x, 0)
observe_table(owner.keep, 0)
observe_table(marker, 0)
return type(owner.seen)
";
    // marker is an extra rooted table and adds no edge when the jump skips
    // `marker.ran = x`. The duplicate-receiver counts are otherwise unchanged.
    for (after, result, collections, live_nodes, live_edges, resources) in [
        (omit_after, "table", 1u32, 6u32, 3u32, 6usize),
        (collect_after, "nil", 2, 4, 2, 4),
    ] {
        let mut vm = candidate(&source.replace("@after@", after));
        assert_eq!(
            vm.run().expect("run tail goto"),
            VmStatus::Halted,
            "{result}"
        );
        assert_eq!(vm.stack(), &[Value::string(result)], "{result}");
        let mut host = vm.host_context();
        let tables = host.scope_state::<Vec<ObservedTable>>().unwrap().clone();
        let outer = tables[0];
        let marker = tables[1];
        let left = tables[2];
        let right = tables[3];
        let visible = tables[4];
        let seen_inside = tables[5];
        let keep_inside = tables[6];
        assert_ne!(left, right, "pair returns two tables");
        assert_ne!(marker, outer, "marker is not the outer local");
        assert_eq!(visible, right, "the visible name is the second receiver");
        assert_ne!(
            visible, left,
            "the shadowed receiver is not the visible name"
        );
        assert_eq!(
            seen_inside, right,
            "in-block collect keeps the visible receiver"
        );
        assert_eq!(
            keep_inside, outer,
            "in-block collect keeps the outer object"
        );
        let drop_alive = result == "table";
        let (seen_after, restored, keep_after, marker_after) = if drop_alive {
            assert_eq!(
                tables[7], right,
                "omitted collect leaves the visible receiver"
            );
            (tables[7], tables[8], tables[9], tables[10])
        } else {
            (right, tables[7], tables[8], tables[9])
        };
        assert_eq!(restored, outer, "the restored name is the outer object");
        assert_ne!(restored, right, "block exit does not keep the inner name");
        assert_eq!(keep_after, outer, "owner.keep is the outer object");
        assert_eq!(marker_after, marker, "the jump does not replace marker");
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.alive(left.table))).unwrap(),
            drop_alive,
            "shadowed receiver after the block; result={result}"
        );
        for (table, alive) in [
            (left, drop_alive),
            (right, drop_alive),
            (outer, true),
            (marker, true),
            (visible, drop_alive),
            (seen_inside, drop_alive),
            (keep_inside, true),
            (seen_after, drop_alive),
            (restored, true),
            (keep_after, true),
            (marker_after, true),
        ] {
            assert_table_lifetime(&mut host, table, alive);
        }
        let stats = heap::stats(&mut host).unwrap();
        assert_eq!(stats.collections, collections, "{result}");
        assert_eq!(stats.live_nodes, live_nodes, "{result}");
        assert_eq!(stats.live_edges, live_edges, "{result}");
        assert_eq!(stats.live_roots, 0, "{result}");
        assert_eq!(host.resource_count(), resources, "{result}");
    }
}

// Same trailing-label cut as the replayed goto. The weak write is
// `owner[k]` with a text local. Leaving the block still drops both
// same-name receiver roots. `owner.seen` and `owner["seen"]` read that
// one record. Collecting after the block retires the receiver token;
// omitting that collect leaves the same table. `marker.ran = x` remains
// after the goto, so a missed cut would keep the receiver.
#[test]
fn lua_candidate_chunk_do_tail_goto_string_key_releases_receiver() {
    let source = r#"
local function pair()
  local left = {}
  local right = {}
  observe_table(left, 0)
  observe_table(right, 0)
  local a = left
  local b = right
  return a, b
end
local mt = {}
mt.__mode = "v"
local owner = {}
setmetatable(owner, mt)
observe_table(owner, 0)
local marker = {}
local x = {}
owner.keep = x
observe_table(x, 0)
do
  local x, x = pair()
  local k = "seen"
  owner[k] = x
  collectgarbage("collect")
  goto tail
  marker.ran = x
  ::tail::
end
@after@
"#;
    let reads = ["owner.seen", r#"owner["seen"]"#];
    for collect in [false, true] {
        for read in reads {
            let after = if collect {
                format!(
                    "collectgarbage(\"collect\")\n\
                     return type({read})\n"
                )
            } else {
                format!(
                    "observe_table(owner.seen, 0)\n\
                     observe_table(owner[\"seen\"], 0)\n\
                     return type({read})\n"
                )
            };
            let label = format!("collect={collect} read={read}");
            let mut vm = candidate(&source.replace("@after@", &after));
            assert_eq!(
                vm.run().expect("run tail goto string key"),
                VmStatus::Halted,
                "{label}"
            );
            let result = if collect { "nil" } else { "table" };
            assert_eq!(vm.stack(), &[Value::string(result)], "{label}");
            let mut host = vm.host_context();
            let tables = host.scope_state::<Vec<ObservedTable>>().unwrap().clone();
            let owner = tables[0];
            let outer = tables[1];
            let left = tables[2];
            let right = tables[3];
            assert_ne!(left, right, "pair returns two tables; {label}");
            assert_ne!(owner, outer, "owner is not the kept object; {label}");
            if !collect {
                let dot = tables[4];
                let bracket = tables[5];
                assert_eq!(
                    dot, bracket,
                    "dot and literal bracket read the dynamic key; {label}"
                );
                assert_eq!(dot, right, "that key is the visible receiver; {label}");
                assert_ne!(
                    dot, left,
                    "the shadowed receiver is not the stored value; {label}"
                );
            }
            let (seen, keep) = heap::with_heap(&mut host, |heap| {
                Ok((
                    heap.named_edge(owner.table, "seen")?,
                    heap.named_edge(owner.table, "keep")?,
                ))
            })
            .unwrap();
            assert_eq!(
                keep,
                Some(outer.table),
                "owner.keep stays the outer object; {label}"
            );
            if collect {
                assert_eq!(seen, None, "the weak text key is cleared; {label}");
            } else {
                assert_eq!(
                    seen,
                    Some(right.table),
                    "the dynamic write and both reads are one key; {label}"
                );
            }
            for (table, alive) in [
                (left, !collect),
                (right, !collect),
                (owner, true),
                (outer, true),
            ] {
                assert_table_lifetime(&mut host, table, alive);
            }
            let stats = heap::stats(&mut host).unwrap();
            assert_eq!(stats.collections, if collect { 2 } else { 1 }, "{label}");
            assert_eq!(stats.live_nodes, if collect { 4 } else { 6 }, "{label}");
            assert_eq!(stats.live_edges, if collect { 2 } else { 3 }, "{label}");
            assert_eq!(stats.live_roots, 0, "{label}");
            assert_eq!(
                host.resource_count(),
                if collect { 4 } else { 6 },
                "{label}"
            );
        }
    }
}

/// Three tails share this goto arm. `::tail::` then `::other::` then `;`,
/// `::tail::` then only `;`, and a jump to that second label followed by two
/// `;`, are all in the block's trailing set. The goto is real: the local
/// after it would publish a receiver and run `fresh`, whose RHS observes two
/// tables and writes `box.kept`. An adjacent label or a second `;` does not
/// itself skip that local; the locked `-joff` fall arm does run it. This
/// candidate only has the goto arm. `omit` leaves the prefix receivers
/// allocated, so a skipped root cannot hide in the sweep.
#[test]
fn lua_candidate_chunk_do_trailing_label_set_skips_receiver_local() {
    for (shape, target, suffix) in [
        ("label-and-empty", "tail", "  ::tail::\n  ::other::\n  ;\n"),
        ("empty-only", "tail", "  ::tail::\n  ;\n"),
        (
            "second-label",
            "other",
            "  ::tail::\n  ::other::\n  ;\n  ;\n",
        ),
    ] {
        assert_trailing_label_set_skips_receiver(shape, target, suffix);
    }
}

fn assert_trailing_label_set_skips_receiver(shape: &str, target: &str, suffix: &str) {
    let source = r#"
local function fresh()
  local made = {}
  local box = {}
  box.kept = made
  observe_table(made, 0)
  observe_table(box, 0)
  local out = made
  return out
end
local function pair()
  local left = {}
  local right = {}
  observe_table(left, 0)
  observe_table(right, 0)
  local a = left
  local b = right
  return a, b
end
local mt = {}
mt.__mode = "v"
local owner = {}
setmetatable(owner, mt)
local marker = {}
local x = {}
owner.keep = x
observe_table(x, 0)
observe_table(marker, 0)
do
  local x, x = pair()
  owner.seen = x
  observe_table(x, 0)
  collectgarbage("collect")
  observe_table(owner.seen, 0)
  observe_table(owner.keep, 0)
  goto @jump@
  local x = fresh()
  marker.ran = x
@suffix@end
@after@
"#
    .replace("@jump@", target)
    .replace("@suffix@", suffix);
    let collect_after = "\
collectgarbage(\"collect\")
observe_table(x, 0)
observe_table(owner.keep, 0)
observe_table(marker, 0)
return type(owner.seen)
";
    let omit_after = "\
observe_table(owner.seen, 0)
observe_table(x, 0)
observe_table(owner.keep, 0)
observe_table(marker, 0)
return type(owner.seen)
";
    for (after, result, collections, live_nodes, live_edges, resources, observations) in [
        (omit_after, "table", 1u32, 6u32, 3u32, 6usize, 11usize),
        (collect_after, "nil", 2, 4, 2, 4, 10),
    ] {
        let mut vm = candidate(&source.replace("@after@", after));
        assert_eq!(
            vm.run().expect("run trailing label set"),
            VmStatus::Halted,
            "{shape}/{result}"
        );
        assert_eq!(vm.stack(), &[Value::string(result)], "{shape}/{result}");
        let mut host = vm.host_context();
        let tables = host.scope_state::<Vec<ObservedTable>>().unwrap().clone();
        assert_eq!(
            tables.len(),
            observations,
            "{shape}/{result}: fresh() must not publish made or box"
        );
        let outer = tables[0];
        let marker = tables[1];
        let left = tables[2];
        let right = tables[3];
        let visible = tables[4];
        let seen_inside = tables[5];
        let keep_inside = tables[6];
        assert_ne!(left, right, "{shape}/{result}");
        assert_ne!(marker, outer, "{shape}/{result}");
        assert_eq!(visible, right, "{shape}/{result}");
        assert_eq!(seen_inside, right, "{shape}/{result}");
        assert_eq!(keep_inside, outer, "{shape}/{result}");
        let drop_alive = result == "table";
        let (seen_after, restored, keep_after, marker_after) = if drop_alive {
            (tables[7], tables[8], tables[9], tables[10])
        } else {
            (right, tables[7], tables[8], tables[9])
        };
        assert_eq!(restored, outer, "{shape}/{result}: restored name is outer");
        assert_ne!(restored, right, "{shape}/{result}");
        assert_eq!(keep_after, outer, "{shape}/{result}");
        assert_eq!(
            marker_after, marker,
            "{shape}/{result}: marker.ran was not written"
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.alive(left.table))).unwrap(),
            drop_alive,
            "{shape}/{result}"
        );
        for (table, alive) in [
            (left, drop_alive),
            (right, drop_alive),
            (outer, true),
            (marker, true),
            (visible, drop_alive),
            (seen_inside, drop_alive),
            (keep_inside, true),
            (seen_after, drop_alive),
            (restored, true),
            (keep_after, true),
            (marker_after, true),
        ] {
            assert_table_lifetime(&mut host, table, alive);
        }
        let stats = heap::stats(&mut host).unwrap();
        let label = format!("{shape}/{result}");
        assert_eq!(stats.collections, collections, "{label}");
        assert_eq!(stats.live_nodes, live_nodes, "{label}");
        assert_eq!(stats.live_edges, live_edges, "{label}");
        assert_eq!(stats.live_roots, 0, "{label}");
        assert_eq!(host.resource_count(), resources, "{label}");
    }
}

/// Two same-name source blocks. The statement between them still runs, and
/// neither `fresh` runs. Matching a target by span rather than by name is a
/// resolver structural assertion; this VM case does not show that.
#[test]
fn lua_candidate_chunk_do_trailing_label_set_keeps_outer_statement() {
    let source = r#"
local function fresh()
  local made = {}
  observe_table(made, 0)
  local out = made
  return out
end
local live = {}
observe_table(live, 0)
do
  goto tail
  local x = fresh()
  ::tail::
  ::other::
  ;
end
observe_table(live, 0)
do
  goto tail
  local x = fresh()
  ::tail::
  ;
end
observe_table(live, 0)
return type(live)
"#;
    let mut vm = candidate(source);
    assert_eq!(
        vm.run().expect("run same-name trailing labels"),
        VmStatus::Halted
    );
    assert_eq!(vm.stack(), &[Value::string("table")]);
    let host = vm.host_context();
    let tables = host.scope_state::<Vec<ObservedTable>>().unwrap().clone();
    assert_eq!(
        tables.len(),
        3,
        "fresh must not publish, and the outer local must"
    );
    assert_eq!(tables[0], tables[1]);
    assert_eq!(tables[1], tables[2]);
}

/// Resolver identity the cut uses. This does not pin a lowerer error.
#[test]
fn trailing_label_set_keeps_resolved_identity() {
    let set = parser::parse_chunk(
        b"do\n  goto tail\n  local x = 1\n  ::tail::\n  ::other::\n  ;\nend\ndo\n  goto tail\n  local x = 1\n  ::tail::\n  ;\nend\n",
    )
    .expect("parse trailing set");
    let resolution = resolve::resolve(&set);
    assert!(resolution.errors.is_empty(), "{:?}", resolution.errors);
    let first = resolution.gotos[0].target.expect("first tail");
    let second = resolution.gotos[1].target.expect("second tail");
    assert_ne!(
        first, second,
        "same name in another block is a different label"
    );
    assert_eq!(resolution.labels[first].name, b"tail");
    assert_eq!(resolution.labels[second].name, b"tail");
    assert!(resolution.labels[first].trailing);
    assert!(resolution.labels[second].trailing);
    let other = resolution
        .labels
        .iter()
        .find(|label| label.name == b"other")
        .expect("other");
    assert!(other.trailing);
    assert_ne!(other.span, resolution.labels[first].span);

    let nontail = parser::parse_chunk(
        b"do\n  goto mid\n  ::mid::\n  local after = 1\n  ::tail::\n  ;\nend\n",
    )
    .expect("parse non-tail");
    let resolution = resolve::resolve(&nontail);
    assert!(resolution.errors.is_empty(), "{:?}", resolution.errors);
    let mid = resolution.gotos[0].target.expect("mid");
    assert_eq!(resolution.labels[mid].name, b"mid");
    assert!(!resolution.labels[mid].trailing);

    let cross = parser::parse_chunk(
        b"do\n  do\n    goto tail\n    local skipped = 1\n  end\n  local between = 1\n  ::tail::\n  ;\nend\n",
    )
    .expect("parse cross-block");
    let resolution = resolve::resolve(&cross);
    assert!(resolution.errors.is_empty(), "{:?}", resolution.errors);
    let target = resolution.gotos[0].target.expect("outer tail");
    let label = &resolution.labels[target];
    assert!(label.trailing);
    assert_eq!(label.name, b"tail");
    let ast::Stmt::Do { body: outer, .. } = &cross.body.stmts[0] else {
        panic!("outer do");
    };
    let ast::Stmt::Do { body: inner, .. } = &outer.stmts[0] else {
        panic!("inner do");
    };
    assert!(
        inner.stmts.iter().all(|stmt| {
            !matches!(stmt, ast::Stmt::Label { name, .. } if name.span == label.span)
        })
    );
    assert!(
        outer.stmts.iter().any(|stmt| {
            matches!(stmt, ast::Stmt::Label { name, .. } if name.span == label.span)
        })
    );
}

/// `rawget(weak, 1)` reads the numeric weak value without a caller binding.
///
/// The helper records the value, stores it only under `weak[1]`, and returns
/// the weak table. Omit keeps that same object. `type(rawget(...))` observes
/// only the first value, so collect yields the string `"nil"`. The original
/// token and heap node are retired. One nil on the VM stack is the separate
/// `return rawget(owner, k)` case. The string key `"1"` is another entry;
/// its type wrapper also sees only the first value.
#[test]
fn lua_candidate_rawget_reads_a_weak_numeric_value_until_collect() {
    let source = r#"
local function seed()
  local mt = {}
  mt.__mode = "v"
  local weak = {}
  setmetatable(weak, mt)
  local value = {}
  value.self = value
  weak[1] = value
  observe_table(value, 0)
  return weak
end
local weak = seed()
observe_table(rawget(weak, 1), 0)
@collect@
return type(rawget(weak, 1))
"#;
    let omitted = source.replace("@collect@\n", "");
    let observed = assert_weak_probe(&omitted, "table", &[true, true], 3, 0);
    assert_eq!(
        observed[0], observed[1],
        "rawget returns the helper's original object"
    );

    let collected = source.replace("@collect@", "collectgarbage(\"collect\")");
    let observed = assert_weak_probe(&collected, "nil", &[false, false], 1, 1);
    assert_eq!(
        observed[0], observed[1],
        "the retired token is the object rawget returned"
    );

    let text_key = omitted.replace(
        "return type(rawget(weak, 1))",
        "return type(rawget(weak, \"1\"))",
    );
    let observed = assert_weak_probe(&text_key, "nil", &[true, true], 3, 0);
    assert_eq!(
        observed[0], observed[1],
        "numeric observations match; type() on \"1\" only saw the first value"
    );
}

/// A nil key is one miss after the table is accepted. The numeric entry stays.
#[test]
fn lua_candidate_rawget_nil_key_reads_one_nil() {
    let source = r#"
local owner = {}
owner[1] = "kept"
local k = 1
k = nil
return rawget(owner, k)
"#;
    let mut vm = candidate(source);
    assert_eq!(vm.run().expect("nil key readback"), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::Null]);
    assert_eq!(heap::stats(&mut vm.host_context()).unwrap().live_edges, 1);

    let kept = source.replace("return rawget(owner, k)", "return owner[1]");
    let mut vm = candidate(&kept);
    assert_eq!(
        vm.run().expect("entry survives a nil key"),
        VmStatus::Halted
    );
    assert_eq!(vm.stack(), &[Value::string("kept")]);
}

/// When a boolean reaches `rawget`, the read misses. The numeric entry stays.
#[test]
fn lua_candidate_rawget_bool_key_is_a_miss() {
    let source = r#"
local a = {}
local owner = {}
owner[1] = "kept"
local k = same_table(a, a)
return rawget(owner, k)
"#;
    let mut vm = candidate(source);
    assert_eq!(vm.run().expect("bool key readback"), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::Null]);

    let kept = source.replace("return rawget(owner, k)", "return owner[1]");
    let mut vm = candidate(&kept);
    assert_eq!(
        vm.run().expect("bool key does not delete"),
        VmStatus::Halted
    );
    assert_eq!(vm.stack(), &[Value::string("kept")]);
}

/// A nil table is rejected before a nil key can become a miss.
#[test]
fn lua_candidate_rawget_rejects_a_nil_table_before_the_key() {
    let source = r#"
local owner = 1
owner = nil
local k = 1
k = nil
return rawget(owner, k)
"#;
    let mut vm = candidate(source);
    let error = vm.run().expect_err("nil table");
    assert!(
        matches!(error, vm::VmError::HostError(ref message) if message == "field source is not a table"),
        "{error}"
    );
}

/// An unissued array is not a nil miss, as a table or as a key.
#[test]
fn lua_candidate_rawget_keeps_unissued_values_as_errors() {
    let table = "return rawget(external_shape(), 1)";
    let mut vm = candidate(table);
    let error = vm.run().expect_err("unissued table");
    assert!(
        matches!(error, vm::VmError::HostError(ref message) if message == "field source is not a table"),
        "{error}"
    );

    let key = r#"
local owner = {}
owner[1] = "kept"
local bad = external_shape()
return rawget(owner, bad)
"#;
    let mut vm = candidate(key);
    let error = vm.run().expect_err("unissued key");
    assert!(
        matches!(error, vm::VmError::HostError(ref message) if message == "edge key is not a table"),
        "{error}"
    );
    assert_eq!(heap::stats(&mut vm.host_context()).unwrap().live_edges, 1);

    let copy = r#"
local owner = {}
owner[1] = "kept"
local bad = copy_fields(owner)
return rawget(owner, bad)
"#;
    let mut vm = candidate(copy);
    let error = vm.run().expect_err("structural copy key");
    assert!(
        matches!(error, vm::VmError::HostError(ref message) if message == "edge key is not a table"),
        "{error}"
    );
    assert_eq!(heap::stats(&mut vm.host_context()).unwrap().live_edges, 1);
}

/// The fresh table is rooted while the key expression collects.
///
/// The key is parenthesized because index keys already accept that form.
/// A table constructor is not an index key, so the reversed case uses a name.
/// Collect in the first argument does not root a table that has not been
/// evaluated, and that number is still not a table.
///
/// `last_marked` is that collect's marked-node count. The orphan is retired,
/// so the marked node is the fresh table the first argument already created.
/// A second run records that table before the call and checks the same handle
/// is still alive. Its local also roots it; argument-temp rooting stays on
/// the first run.
#[test]
fn lua_candidate_rawget_roots_a_fresh_table_while_the_key_collects() {
    let source = r#"
local orphan = {}
orphan.self = orphan
observe_table(orphan, 0)
orphan = nil
return type(rawget({}, (collectgarbage("collect"))))
"#;
    let mut vm = candidate(source);
    assert_eq!(vm.run().expect("collect inside the key"), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::string("nil")]);
    let observed = observed_tables(&mut vm);
    assert_eq!(observed.len(), 1);
    assert!(
        !heap::with_heap(&mut vm.host_context(), |heap| {
            Ok(heap.alive(observed[0].table))
        })
        .unwrap()
    );
    let stats = heap::stats(&mut vm.host_context()).unwrap();
    assert_eq!(stats.collections, 1);
    assert_eq!(
        stats.last_marked, 1,
        "the collector marked the fresh table already created as the first argument"
    );
    assert_eq!(
        stats.live_nodes, 1,
        "the fresh table survived the key collect"
    );
    assert_eq!(stats.live_edges, 0);

    let named = r#"
local fresh = {}
observe_table(fresh, 0)
return type(rawget(fresh, (collectgarbage("collect"))))
"#;
    let mut vm = candidate(named);
    assert_eq!(
        vm.run().expect("record the fresh table before collect"),
        VmStatus::Halted
    );
    assert_eq!(vm.stack(), &[Value::string("nil")]);
    let observed = observed_tables(&mut vm);
    assert_eq!(observed.len(), 1);
    assert!(
        heap::with_heap(&mut vm.host_context(), |heap| {
            Ok(heap.alive(observed[0].table))
        })
        .unwrap(),
        "the table recorded before the key collect is still alive"
    );
    let stats = heap::stats(&mut vm.host_context()).unwrap();
    assert_eq!(stats.collections, 1);
    assert_eq!(stats.last_marked, 1);
    assert_eq!(stats.live_nodes, 1);

    let omitted = source.replace("(collectgarbage(\"collect\"))", "0");
    let mut vm = candidate(&omitted);
    assert_eq!(vm.run().expect("omit key collect"), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::string("nil")]);
    let observed = observed_tables(&mut vm);
    assert!(
        heap::with_heap(&mut vm.host_context(), |heap| {
            Ok(heap.alive(observed[0].table))
        })
        .unwrap()
    );
    let stats = heap::stats(&mut vm.host_context()).unwrap();
    assert_eq!(stats.collections, 0);
    assert_eq!(stats.live_nodes, 2);

    let reversed = r#"
local orphan = {}
orphan.self = orphan
observe_table(orphan, 0)
orphan = nil
local key = {}
return type(rawget((collectgarbage("collect")), key))
"#;
    let mut vm = candidate(reversed);
    let error = vm.run().expect_err("collect before the table exists");
    assert!(
        matches!(error, vm::VmError::HostError(ref message) if message == "field source is not a table"),
        "{error}"
    );
    let observed = observed_tables(&mut vm);
    assert!(
        !heap::with_heap(&mut vm.host_context(), |heap| {
            Ok(heap.alive(observed[0].table))
        })
        .unwrap()
    );
    assert_eq!(heap::stats(&mut vm.host_context()).unwrap().collections, 1);
}

/// A block-local `rawget` does not replace the unbound call after the block.
#[test]
fn lua_candidate_rawget_after_a_block_shadow_is_the_builtin() {
    let source = r#"
do
  local rawget = 1
end
local owner = {}
owner[1] = "kept"
return rawget(owner, 1)
"#;
    let mut vm = candidate(source);
    assert_eq!(vm.run().expect("builtin after the block"), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::string("kept")]);
}

/// A local function named `rawget` returns 7. Legal call shapes this subset
/// does not lower are not frozen as refusals.
#[test]
fn lua_candidate_local_rawget_is_not_the_builtin() {
    let function = r#"
local function rawget()
  return 7
end
local n = rawget()
return n
"#;
    let mut vm = candidate(function);
    assert_eq!(vm.run().expect("local function"), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::Float(7.0)]);
}

/// `1e400` is +Inf in this lowerer. It shares the injected +Inf key.
/// `-0` and NaN still need `float_bits`.
#[test]
fn lua_candidate_literal_overflow_shares_positive_infinity_key() {
    let from_literal = r#"
local inf = float_bits(3)
local owner = {}
owner[1e400] = "literal"
return owner[inf]
"#;
    let mut vm = candidate(from_literal);
    assert_eq!(vm.run().expect("read injected +Inf"), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::string("literal")]);
    assert_eq!(heap::stats(&mut vm.host_context()).unwrap().live_edges, 1);

    let from_bits = r#"
local inf = float_bits(3)
local owner = {}
owner[inf] = "bits"
return rawget(owner, 1e400)
"#;
    let mut vm = candidate(from_bits);
    assert_eq!(vm.run().expect("read literal +Inf"), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::string("bits")]);
    assert_eq!(heap::stats(&mut vm.host_context()).unwrap().live_edges, 1);
}

// Chunk `do` jumps forward to its own non-trailing label. The label is not in
// the trailing set: `collectgarbage` follows it. The goto is the block's only
// direct jump, so the statements between it and that label are not lowered.
// `marker.skipped = {}` therefore publishes no node. The statements after the
// label do run, and the in-block collect happens while `t` is still that
// block's root. Leaving the block is the one `retire_block`. The next collect
// clears the weak value and retires the original token. Omitting it leaves
// the same token. Omitting the goto runs the skipped write. A nested block
// with the same label name is a different span. A jump over a same-block
// local is a resolver error, not a lowered path. This does not pin an
// unimplemented goto's diagnostic text.
#[test]
fn lua_candidate_chunk_do_forward_goto_keeps_root_until_block_exit() {
    let nested = parser::parse_chunk(
        b"do\n  goto resume\n  do\n    ::resume::\n  end\n  marker.skipped = {}\n  ::resume::\n  collectgarbage(\"collect\")\nend\n",
    )
    .expect("parse nested same name");
    let resolution = resolve::resolve(&nested);
    assert!(
        resolution.errors.is_empty(),
        "a nested same name is not this block's label: {:?}",
        resolution.errors
    );
    let target = resolution.gotos[0].target.expect("outer resume");
    let label = &resolution.labels[target];
    assert_eq!(label.name, b"resume");
    assert!(!label.trailing, "collectgarbage follows the label");
    let ast::Stmt::Do { body, .. } = &nested.body.stmts[0] else {
        panic!("outer do");
    };
    let ast::Stmt::Do { body: inner, .. } = &body.stmts[1] else {
        panic!("nested do");
    };
    assert!(
        inner.stmts.iter().all(|stmt| {
            !matches!(stmt, ast::Stmt::Label { name, .. } if name.span == label.span)
        }),
        "the nested name is a different label"
    );
    assert!(
        body.stmts.iter().any(|stmt| {
            matches!(stmt, ast::Stmt::Label { name, .. } if name.span == label.span)
        }),
        "the resolved target is a direct child"
    );

    let into_local = parser::parse_chunk(
        b"do\n  goto resume\n  local hidden = {}\n  ::resume::\n  local after = 1\nend\n",
    )
    .expect("parse jump into local");
    let resolution = resolve::resolve(&into_local);
    assert!(
        matches!(
            resolution.errors.first(),
            Some(resolve::ResolveError::JumpIntoLocal { local_name, .. })
                if local_name == b"hidden"
        ),
        "a non-trailing label cannot enter a same-block local: {:?}",
        resolution.errors
    );

    let source = r#"
local mt = {}
mt.__mode = "v"
local weak = {}
setmetatable(weak, mt)
local marker = {}
observe_table(weak, 0)
observe_table(marker, 0)
do
  local t = {}
  t.self = t
  weak.seen = t
  observe_table(t, 0)
@jump@
  marker.skipped = {}
  ::resume::
  observe_table(t, 0)
  observe_table(weak.seen, 0)
  collectgarbage("collect")
  observe_table(t, 0)
  observe_table(weak.seen, 0)
end
@after@
"#;
    let collect_after = "\
collectgarbage(\"collect\")
observe_table(weak, 0)
observe_table(marker, 0)
return type(weak.seen)
";
    let omit_after = "\
observe_table(weak.seen, 0)
observe_table(weak, 0)
observe_table(marker, 0)
return type(weak.seen)
";
    let fall_after = "\
collectgarbage(\"collect\")
observe_table(marker.skipped, 0)
observe_table(weak, 0)
observe_table(marker, 0)
return type(weak.seen)
";
    for (jump, after, result, collections, live_nodes, live_edges, resources, observations) in [
        (
            "  goto resume\n",
            omit_after,
            "table",
            1u32,
            4u32,
            3u32,
            4usize,
            10usize,
        ),
        ("  goto resume\n", collect_after, "nil", 2, 3, 1, 3, 9),
        ("", fall_after, "nil", 2, 4, 2, 4, 10),
    ] {
        let mut vm = candidate(&source.replace("@jump@", jump).replace("@after@", after));
        assert_eq!(
            vm.run().expect("run forward goto"),
            VmStatus::Halted,
            "{result}/{collections}"
        );
        assert_eq!(
            vm.stack(),
            &[Value::string(result)],
            "{result}/{collections}"
        );
        let mut host = vm.host_context();
        let tables = host.scope_state::<Vec<ObservedTable>>().unwrap().clone();
        assert_eq!(
            tables.len(),
            observations,
            "{result}/{collections}: skipped write must not add an observation on the goto arms"
        );
        let weak = tables[0];
        let marker = tables[1];
        let created = tables[2];
        let after_label = tables[3];
        let seen_before = tables[4];
        let after_collect = tables[5];
        let seen_after = tables[6];
        assert_ne!(weak, marker, "{result}/{collections}");
        assert_ne!(created, weak, "{result}/{collections}");
        assert_ne!(created, marker, "{result}/{collections}");
        assert_eq!(
            after_label, created,
            "{result}/{collections}: the label's following read is t"
        );
        assert_eq!(
            seen_before, created,
            "{result}/{collections}: weak.seen before the in-block collect is t"
        );
        assert_eq!(
            after_collect, created,
            "{result}/{collections}: in-block collect keeps the same token"
        );
        assert_eq!(
            seen_after, created,
            "{result}/{collections}: the weak read after that collect is still t"
        );
        let dropped = result == "nil";
        if jump.is_empty() {
            let skipped = tables[7];
            let weak_after = tables[8];
            let marker_after = tables[9];
            assert_ne!(skipped, created, "the skipped write publishes a new table");
            assert_ne!(skipped, marker, "marker.skipped is not marker");
            assert_ne!(skipped, weak, "marker.skipped is not weak");
            assert_eq!(weak_after, weak);
            assert_eq!(marker_after, marker);
            assert_table_lifetime(&mut host, skipped, true);
        } else {
            let (weak_after, marker_after) = if dropped {
                (tables[7], tables[8])
            } else {
                let seen_outside = tables[7];
                assert_eq!(
                    seen_outside, created,
                    "omitted collect leaves the same token in weak.seen"
                );
                assert_table_lifetime(&mut host, seen_outside, true);
                (tables[8], tables[9])
            };
            assert_eq!(weak_after, weak, "{result}");
            assert_eq!(marker_after, marker, "{result}");
        }
        assert_table_lifetime(&mut host, created, !dropped);
        assert_table_lifetime(&mut host, weak, true);
        assert_table_lifetime(&mut host, marker, true);
        let stats = heap::stats(&mut host).unwrap();
        let label = format!("{result}/{collections}");
        assert_eq!(stats.collections, collections, "{label}");
        assert_eq!(stats.live_nodes, live_nodes, "{label}");
        assert_eq!(stats.live_edges, live_edges, "{label}");
        assert_eq!(stats.live_roots, 0, "{label}");
        assert_eq!(host.resource_count(), resources, "{label}");
    }
}
