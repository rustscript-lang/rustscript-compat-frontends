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

use super::heap;
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

fn catalog() -> &'static Arc<vm::HostApiCatalog> {
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
