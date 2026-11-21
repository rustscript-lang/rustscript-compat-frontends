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

fn register_witnesses(registry: &mut HostFunctionRegistry, catalog: &vm::HostApiCatalog) {
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
        assert!(
            weak.upgrade().is_none(),
            "the slot weak pointer must not keep the array alive"
        );
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
