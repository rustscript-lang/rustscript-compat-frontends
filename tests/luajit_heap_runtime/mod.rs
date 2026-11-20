//! Test-owned SourcePlugin and typed host adapters.
//!
//! This is not a production plugin and is not wired to the `.lua` entry.

use std::sync::atomic::{AtomicU64, Ordering};

use vm::compiler::ir::{ResolvedHostCall, ResolvedHostParam};
use vm::{
    CallOutcome, CallReturn, CompileSourceFileOptions, Expr, FrontendImportSyntax, FrontendIr,
    HostApiBuilder, HostApiCatalog, HostFunctionRegistry, HostFunctionSchema, HostImportSchema,
    HostOwnedFunction, HostParamPassing, HostParamSchema, HostStructField,
    HostStructSchema, HostTypeSchema, LocalIrBuilder, LocalSlot, OwnedHostCall, ParseError,
    ResourceTypeKey, ResourceTypeSchema, SourceFlavor, SourcePlugin, Stmt, TypeSchema, Value,
    VmError, VmStatus,
};

use super::heap::{self, HeapLimits, LuaTableResource, TableRef};

static BODY_ENTRIES: AtomicU64 = AtomicU64::new(0);
static ALIVE_ENTRIES: AtomicU64 = AtomicU64::new(0);
static FORGED_SLOT: AtomicU64 = AtomicU64::new(0);
static FORGED_GENERATION: AtomicU64 = AtomicU64::new(0);

pub fn body_entries() -> u64 {
    BODY_ENTRIES.load(Ordering::SeqCst)
}

pub fn alive_entries() -> u64 {
    ALIVE_ENTRIES.load(Ordering::SeqCst)
}

#[derive(Clone, Copy, Debug)]
pub enum Probe {
    Transfer,
    Publish,
    Alias,
    RejectInt,
    RejectForeign { raw: i64 },
    Expired,
    NamedCarrier,
    NestedRooted,
    NestedUnrooted,
}

#[derive(Clone, Debug)]
pub struct ProbeResult {
    pub stack: Vec<Value>,
    pub body_entries: u64,
    pub published_raw: Option<i64>,
    pub forged_slot: Option<u64>,
    pub forged_generation: Option<u64>,
}

pub struct PublishStats {
    pub before_live: u32,
    pub after_live: u32,
    pub before_slots: u32,
    pub after_slots: u32,
    pub publish_failed: bool,
}

struct ProbePlugin;

impl SourcePlugin for ProbePlugin {
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
        build_ir(source.trim())
    }
}

pub fn run_probe(probe: Probe) -> Result<ProbeResult, String> {
    let catalog = catalog();
    let source = probe_source(probe);
    let compiled = vm::compile_source_with_flavor_and_options(
        &source,
        SourceFlavor::Lua,
        CompileSourceFileOptions::new()
            .with_source_plugin(&ProbePlugin)
            .with_host_api_catalog(std::sync::Arc::new(catalog.clone())),
    )
    .map_err(|error| error.to_string())?;
    let mut registry = HostFunctionRegistry::empty();
    register_hosts(&mut registry, &catalog).map_err(|error| error.to_string())?;
    let mut vm = compiled.into_vm().map_err(|error| error.to_string())?;
    registry
        .bind_vm_cached(&mut vm)
        .map_err(|error| error.to_string())?;
    let before = body_entries();
    let status = vm.run().map_err(|error| error.to_string())?;
    if status != VmStatus::Halted {
        return Err(format!("probe did not halt: {status:?}"));
    }
    let stack = vm.stack().to_vec();
    let published_raw = stack.iter().find_map(|value| match value {
        Value::Int(raw) if *raw > 1 => Some(*raw),
        _ => None,
    });
    Ok(ProbeResult {
        stack,
        body_entries: body_entries().saturating_sub(before),
        published_raw,
        forged_slot: non_zero(FORGED_SLOT.load(Ordering::SeqCst)),
        forged_generation: non_zero(FORGED_GENERATION.load(Ordering::SeqCst)),
    })
}

pub fn fill_resource_table_then_fail_publish() -> PublishStats {
    let catalog = catalog();
    let compiled = vm::compile_source_with_flavor_and_options(
        "idle",
        SourceFlavor::Lua,
        CompileSourceFileOptions::new().with_source_plugin(&ProbePlugin),
    )
    .expect("idle program");
    let mut vm = compiled.into_vm().expect("vm");
    let mut host = vm.host_context();
    heap::install(
        &mut host,
        HeapLimits {
            nodes: 2048,
            roots: 4,
            edges: 4,
            max_generation: 8,
        },
    )
    .expect("install");
    for _ in 0..vm::resource::handle::DEFAULT_MAX_RESOURCES {
        heap::publish(&mut host).expect("fill resource table");
    }
    let before = heap::stats(&mut host).expect("stats");
    let failed = heap::publish(&mut host).is_err();
    let after = heap::stats(&mut host).expect("stats after");
    let _ = catalog;
    PublishStats {
        before_live: before.live_nodes,
        after_live: after.live_nodes,
        before_slots: before.node_slots,
        after_slots: after.node_slots,
        publish_failed: failed,
    }
}

fn non_zero(value: u64) -> Option<u64> {
    (value != 0).then_some(value)
}

fn probe_source(probe: Probe) -> String {
    match probe {
        Probe::Transfer => "transfer".to_string(),
        Probe::Publish => "publish".to_string(),
        Probe::Alias => "alias".to_string(),
        Probe::RejectInt => "reject-int".to_string(),
        Probe::RejectForeign { raw } => format!("reject-foreign:{raw}"),
        Probe::Expired => "expired".to_string(),
        Probe::NamedCarrier => "named".to_string(),
        Probe::NestedRooted => "nested-rooted".to_string(),
        Probe::NestedUnrooted => "nested-unrooted".to_string(),
    }
}

fn catalog() -> HostApiCatalog {
    let key = LuaTableResource::key();
    let named = named_schema();
    let mut builder = HostApiBuilder::new();
    builder.resource(ResourceTypeSchema::new(key.clone(), "Lua table"));
    builder.named_struct(named.clone());
    builder.function(func(
        "lua_heap_publish",
        vec![],
        HostTypeSchema::Resource(key.clone()),
    ));
    builder.function(func(
        "lua_heap_alive",
        vec![borrow("table", &key)],
        HostTypeSchema::Int,
    ));
    builder.function(func(
        "lua_heap_observe",
        vec![
            borrow("table", &key),
            HostParamSchema::value("marker", HostTypeSchema::Int),
        ],
        HostTypeSchema::Int,
    ));
    builder.function(func(
        "lua_heap_set_edge",
        vec![
            borrow("from", &key),
            HostParamSchema::value("key", HostTypeSchema::Int),
            borrow("to", &key),
        ],
        HostTypeSchema::Int,
    ));
    builder.function(func(
        "lua_heap_edge_is",
        vec![
            borrow("from", &key),
            HostParamSchema::value("key", HostTypeSchema::Int),
            borrow("to", &key),
        ],
        HostTypeSchema::Int,
    ));
    builder.function(func(
        "lua_heap_enter",
        vec![HostParamSchema::value("width", HostTypeSchema::Int)],
        HostTypeSchema::Int,
    ));
    builder.function(func(
        "lua_heap_root",
        vec![
            HostParamSchema::value("slot", HostTypeSchema::Int),
            borrow("table", &key),
        ],
        HostTypeSchema::Resource(key.clone()),
    ));
    builder.function(func(
        "lua_heap_collect",
        vec![],
        HostTypeSchema::Int,
    ));
    builder.function(func(
        "lua_heap_close",
        vec![HostParamSchema::with_passing(
            "table",
            HostTypeSchema::Resource(key),
            HostParamPassing::TakeOwned,
        )],
        HostTypeSchema::Int,
    ));
    builder.function(func(
        "lua_heap_forge_named",
        vec![],
        named.as_type(),
    ));
    builder.function(func(
        "lua_heap_accept_named",
        vec![HostParamSchema::value("record", named.as_type())],
        HostTypeSchema::Int,
    ));
    builder.build().expect("catalog")
}

fn named_schema() -> HostStructSchema {
    HostStructSchema::new(
        "lua_heap_private_carrier",
        vec![
            HostStructField::new("slot", HostTypeSchema::Int),
            HostStructField::new("generation", HostTypeSchema::Int),
        ],
    )
}

fn borrow(name: &str, key: &ResourceTypeKey) -> HostParamSchema {
    HostParamSchema::with_passing(
        name,
        HostTypeSchema::Resource(key.clone()),
        HostParamPassing::Borrow,
    )
}

fn func(name: &str, params: Vec<HostParamSchema>, ret: HostTypeSchema) -> HostFunctionSchema {
    HostFunctionSchema::with_return(name, params, ret)
}

fn register_hosts(registry: &mut HostFunctionRegistry, catalog: &HostApiCatalog) -> Result<(), VmError> {
    register(registry, catalog, "lua_heap_publish", 0, |_| Box::new(Publish))?;
    register(registry, catalog, "lua_heap_alive", 1, |_| Box::new(Alive))?;
    register(registry, catalog, "lua_heap_observe", 2, |_| Box::new(Observe))?;
    register(registry, catalog, "lua_heap_set_edge", 3, |_| Box::new(SetEdge))?;
    register(registry, catalog, "lua_heap_edge_is", 3, |_| Box::new(EdgeIs))?;
    register(registry, catalog, "lua_heap_enter", 1, |_| Box::new(Enter))?;
    register(registry, catalog, "lua_heap_root", 2, |_| Box::new(Root))?;
    register(registry, catalog, "lua_heap_collect", 0, |_| Box::new(Collect))?;
    register(registry, catalog, "lua_heap_close", 1, |_| Box::new(Close))?;
    register(registry, catalog, "lua_heap_forge_named", 0, |_| Box::new(ForgeNamed))?;
    register(registry, catalog, "lua_heap_accept_named", 1, |_| Box::new(AcceptNamed))?;
    Ok(())
}

fn register(
    registry: &mut HostFunctionRegistry,
    catalog: &HostApiCatalog,
    name: &str,
    arity: u8,
    factory: impl Fn(vm::OwnedHostContext<'_>) -> Box<dyn HostOwnedFunction> + Send + Sync + 'static,
) -> Result<(), VmError> {
    let function = catalog.function(name).expect("catalog function");
    let schema = HostImportSchema::from_function(catalog, function);
    registry.register_exact_owned(name, arity, schema, factory)?;
    Ok(())
}

struct Publish;
struct Alive;
struct Observe;
struct SetEdge;
struct EdgeIs;
struct Enter;
struct Root;
struct Collect;
struct Close;
struct ForgeNamed;
struct AcceptNamed;

impl HostOwnedFunction for Publish {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> Result<CallOutcome, VmError> {
        note_body();
        let mut host = call.vm().host_context();
        let value = heap::publish(&mut host).map_err(host_error)?;
        Ok(CallOutcome::Return(CallReturn::one(value)))
    }
}

impl HostOwnedFunction for Alive {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> Result<CallOutcome, VmError> {
        note_body();
        note_alive();
        let table = recover(call, 0)?;
        let mut host = call.vm().host_context();
        let alive = heap::with_heap(&mut host, |heap| Ok(heap.alive(table))).map_err(host_error)?;
        Ok(CallOutcome::Return(CallReturn::one(Value::Int(i64::from(alive)))))
    }
}

impl HostOwnedFunction for Observe {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> Result<CallOutcome, VmError> {
        note_body();
        note_alive();
        let table = recover(call, 0)?;
        let _marker = int_arg(&call, 1)?;
        let mut host = call.vm().host_context();
        let alive = heap::with_heap(&mut host, |heap| Ok(heap.alive(table))).map_err(host_error)?;
        Ok(CallOutcome::Return(CallReturn::one(Value::Int(i64::from(
            alive,
        )))))
    }
}

impl HostOwnedFunction for SetEdge {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> Result<CallOutcome, VmError> {
        note_body();
        let from = recover(call, 0)?;
        let key = int_arg(&call, 1)?;
        let to = recover(call, 2)?;
        let mut host = call.vm().host_context();
        heap::with_heap(&mut host, |heap| heap.set_edge(from, key, to)).map_err(host_error)?;
        Ok(CallOutcome::Return(CallReturn::one(Value::Int(1))))
    }
}

impl HostOwnedFunction for EdgeIs {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> Result<CallOutcome, VmError> {
        note_body();
        let from = recover(call, 0)?;
        let key = int_arg(&call, 1)?;
        let to = recover(call, 2)?;
        let mut host = call.vm().host_context();
        let same = heap::with_heap(&mut host, |heap| Ok(heap.edge(from, key) == Ok(Some(to))))
            .map_err(host_error)?;
        Ok(CallOutcome::Return(CallReturn::one(Value::Int(i64::from(same)))))
    }
}

impl HostOwnedFunction for Enter {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> Result<CallOutcome, VmError> {
        note_body();
        let width = int_arg(&call, 0)?;
        let mut host = call.vm().host_context();
        heap::with_heap(&mut host, |heap| heap.enter_roots(width)).map_err(host_error)?;
        Ok(CallOutcome::Return(CallReturn::one(Value::Int(1))))
    }
}

impl HostOwnedFunction for Root {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> Result<CallOutcome, VmError> {
        note_body();
        let slot = int_arg(&call, 0)?;
        let table = recover(call, 1)?;
        let returned = call.args()[1].clone();
        let mut host = call.vm().host_context();
        heap::with_heap(&mut host, |heap| {
            let frame = heap.current_frame().ok_or(heap::HeapError::NoRootFrame)?;
            heap.write_root(frame, slot, table)
        })
        .map_err(host_error)?;
        Ok(CallOutcome::Return(CallReturn::one(returned)))
    }
}

impl HostOwnedFunction for Collect {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> Result<CallOutcome, VmError> {
        note_body();
        let mut host = call.vm().host_context();
        let marked = heap::with_heap(&mut host, |heap| Ok(heap.collect())).map_err(host_error)?;
        Ok(CallOutcome::Return(CallReturn::one(Value::Int(
            i64::from(marked.live_nodes),
        ))))
    }
}

impl HostOwnedFunction for Close {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> Result<CallOutcome, VmError> {
        note_body();
        let _taken = call.take_arg(0)?;
        Ok(CallOutcome::Return(CallReturn::one(Value::Int(1))))
    }
}

impl HostOwnedFunction for ForgeNamed {
    fn call(&mut self, _call: &mut OwnedHostCall<'_>) -> Result<CallOutcome, VmError> {
        note_body();
        let mut map = vm::bytecode::VmMap::new();
        map.insert(Value::string("slot"), Value::Int(7));
        map.insert(Value::string("generation"), Value::Int(9));
        Ok(CallOutcome::Return(CallReturn::one(Value::Map(
            std::sync::Arc::new(map),
        ))))
    }
}

impl HostOwnedFunction for AcceptNamed {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> Result<CallOutcome, VmError> {
        note_body();
        let Value::Map(map) = &call.args()[0] else {
            return Err(VmError::HostError("named carrier was not a map".to_string()));
        };
        let slot = map_int(map, "slot")?;
        let generation = map_int(map, "generation")?;
        FORGED_SLOT.store(slot as u64, Ordering::SeqCst);
        FORGED_GENERATION.store(generation as u64, Ordering::SeqCst);
        Ok(CallOutcome::Return(CallReturn::one(Value::Int(1))))
    }
}

fn map_int(map: &vm::bytecode::VmMap, key: &str) -> Result<i64, VmError> {
    match map.get(&Value::string(key)) {
        Some(Value::Int(value)) => Ok(*value),
        _ => Err(VmError::HostError(format!("missing named field {key}"))),
    }
}

fn recover(call: &mut OwnedHostCall<'_>, index: usize) -> Result<TableRef, VmError> {
    let value = call.args().get(index).cloned().ok_or_else(|| {
        VmError::HostError(format!("missing argument {index}"))
    })?;
    let Value::Int(raw) = value else {
        return Err(VmError::HostError(
            "table carrier is not a resource handle".to_string(),
        ));
    };
    let raw = u64::try_from(raw).map_err(|_| {
        VmError::HostError("table carrier is not a positive resource handle".to_string())
    })?;
    let handle = vm::ResourceHandle::from_raw(raw)
        .map_err(|error| VmError::HostError(error.to_string()))?;
    let host = call.vm().host_context();
    let token = host
        .typed_resource_with_key::<LuaTableResource>(handle, &LuaTableResource::key())
        .map_err(host_error)?;
    Ok(host.resource(&token).map_err(host_error)?.get().table())
}

fn int_arg(call: &OwnedHostCall<'_>, index: usize) -> Result<u32, VmError> {
    match call.args().get(index) {
        Some(Value::Int(value)) if *value >= 0 && *value <= i64::from(u32::MAX) => {
            Ok(*value as u32)
        }
        _ => Err(VmError::HostError(format!("argument {index} is not a field index"))),
    }
}

fn note_body() {
    BODY_ENTRIES.fetch_add(1, Ordering::SeqCst);
}

fn note_alive() {
    ALIVE_ENTRIES.fetch_add(1, Ordering::SeqCst);
}

fn host_error(error: impl ToString) -> VmError {
    VmError::HostError(error.to_string())
}

fn build_ir(source: &str) -> Result<FrontendIr, ParseError> {
    if source == "idle" {
        return Ok(LocalIrBuilder::new().finish(Vec::new()));
    }
    let catalog = catalog();
    let mut builder = LocalIrBuilder::new();
    let mut next_index = 0u16;
    let mut indices = std::collections::HashMap::new();
    for (name, arity) in hosts_for(source) {
        builder.declare_function(name, Some(*arity))?;
        indices.insert(*name, next_index);
        next_index += 1;
    }
    let call = |name: &str, args: Vec<Expr>| {
        let index = indices[name];
        Expr::Call(
            index,
            Vec::new(),
            args,
            Some(Box::new(resolution(&catalog, name))),
            None,
        )
    };
    let local = |builder: &mut LocalIrBuilder, name: &str| builder.alloc_local_named(name);
    let bind = |slot: LocalSlot, expr: Expr| Stmt::Let {
        index: slot,
        declared_schema: None,
        expr,
        line: 1,
    };
    let mut stmts = Vec::new();
    match source {
        "publish" => stmts.push(Stmt::Expr {
            expr: call("lua_heap_publish", Vec::new()),
            line: 1,
        }),
        "transfer" => {
            let table = local(&mut builder, "table")?;
            stmts.push(bind(table, call("lua_heap_publish", Vec::new())));
            stmts.push(Stmt::Expr {
                expr: call("lua_heap_alive", vec![Expr::Var(table)]),
                line: 1,
            });
        }
        "alias" => {
            let left = local(&mut builder, "left")?;
            let right = local(&mut builder, "right")?;
            let wrote = local(&mut builder, "wrote")?;
            stmts.push(bind(left, call("lua_heap_publish", Vec::new())));
            stmts.push(bind(right, call("lua_heap_publish", Vec::new())));
            stmts.push(bind(
                wrote,
                call(
                    "lua_heap_set_edge",
                    vec![Expr::Var(left), Expr::Int(1), Expr::Var(right)],
                ),
            ));
            stmts.push(Stmt::Expr {
                expr: call(
                    "lua_heap_edge_is",
                    vec![Expr::Var(left), Expr::Int(1), Expr::Var(right)],
                ),
                line: 1,
            });
        }
        "reject-int" => stmts.push(Stmt::Expr {
            expr: call("lua_heap_alive", vec![Expr::Int(1)]),
            line: 1,
        }),
        source if source.starts_with("reject-foreign:") => {
            let raw = source["reject-foreign:".len()..]
                .parse::<i64>()
                .map_err(|_| ParseError::new("foreign handle literal"))?;
            stmts.push(Stmt::Expr {
                expr: call("lua_heap_alive", vec![Expr::Int(raw)]),
                line: 1,
            });
        }
        "expired" => {
            let table = local(&mut builder, "table")?;
            let closed = local(&mut builder, "closed")?;
            stmts.push(bind(table, call("lua_heap_publish", Vec::new())));
            stmts.push(bind(closed, call("lua_heap_close", vec![Expr::Var(table)])));
            stmts.push(Stmt::Expr {
                expr: call("lua_heap_alive", vec![Expr::Var(table)]),
                line: 1,
            });
        }
        "named" => {
            FORGED_SLOT.store(0, Ordering::SeqCst);
            FORGED_GENERATION.store(0, Ordering::SeqCst);
            let record = local(&mut builder, "record")?;
            stmts.push(bind(record, call("lua_heap_forge_named", Vec::new())));
            stmts.push(Stmt::Expr {
                expr: call("lua_heap_accept_named", vec![Expr::Var(record)]),
                line: 1,
            });
        }
        "nested-rooted" | "nested-unrooted" => {
            let entered = local(&mut builder, "entered")?;
            let table = local(&mut builder, "table")?;
            stmts.push(bind(entered, call("lua_heap_enter", vec![Expr::Int(1)])));
            stmts.push(bind(table, call("lua_heap_publish", Vec::new())));
            let first = if source == "nested-rooted" {
                call("lua_heap_root", vec![Expr::Int(0), Expr::Var(table)])
            } else {
                Expr::Var(table)
            };
            stmts.push(Stmt::Expr {
                expr: call(
                    "lua_heap_observe",
                    vec![first, call("lua_heap_collect", Vec::new())],
                ),
                line: 1,
            });
        }
        _ => return Err(ParseError::new(format!("unknown heap probe {source}"))),
    }
    Ok(builder.finish(stmts))
}

fn hosts_for(source: &str) -> &'static [(&'static str, u8)] {
    match source {
        "publish" => &[("lua_heap_publish", 0)],
        "transfer" => &[("lua_heap_publish", 0), ("lua_heap_alive", 1)],
        "alias" => &[
            ("lua_heap_publish", 0),
            ("lua_heap_set_edge", 3),
            ("lua_heap_edge_is", 3),
        ],
        "reject-int" => &[("lua_heap_alive", 1)],
        source if source.starts_with("reject-foreign:") => &[("lua_heap_alive", 1)],
        "expired" => &[
            ("lua_heap_publish", 0),
            ("lua_heap_close", 1),
            ("lua_heap_alive", 1),
        ],
        "named" => &[
            ("lua_heap_forge_named", 0),
            ("lua_heap_accept_named", 1),
        ],
        "nested-rooted" => &[
            ("lua_heap_enter", 1),
            ("lua_heap_publish", 0),
            ("lua_heap_root", 2),
            ("lua_heap_observe", 2),
            ("lua_heap_collect", 0),
        ],
        "nested-unrooted" => &[
            ("lua_heap_enter", 1),
            ("lua_heap_publish", 0),
            ("lua_heap_observe", 2),
            ("lua_heap_collect", 0),
        ],
        _ => &[],
    }
}

fn resolution(catalog: &HostApiCatalog, name: &str) -> ResolvedHostCall {
    let function = catalog.function(name).expect("function");
    ResolvedHostCall {
        name: function.name.clone(),
        params: function
            .params
            .iter()
            .map(|param| ResolvedHostParam {
                name: param.name.clone(),
                schema: type_schema(&param.ty),
            })
            .collect(),
        return_type: type_schema(&function.return_type),
        passing: function.params.iter().map(|param| param.passing).collect(),
        fingerprint: catalog.fingerprint(),
        host_params: function.params.iter().map(|param| param.ty.clone()).collect(),
        host_return_type: function.return_type.clone(),
    }
}

fn type_schema(schema: &HostTypeSchema) -> TypeSchema {
    match schema {
        HostTypeSchema::Int => TypeSchema::Int,
        HostTypeSchema::Resource(key) => TypeSchema::Resource(key.clone()),
        HostTypeSchema::Named { name, .. } => TypeSchema::Named(name.clone(), Vec::new()),
        _ => TypeSchema::Unknown,
    }
}
