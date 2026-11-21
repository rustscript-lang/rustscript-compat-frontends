//! Chunk scalar control flow through the real Lua parser and resolver.
//!
//! The plugin is test-only. Production `LuaPlugin` stays on the line lowerer.
//! Fixtures are legal Lua 5.2-compat chunks written to real `.lua` files and
//! run through SourcePlugin, Core, and the interpreter. Observation is the
//! single chunk return on the VM stack. Ordinary Cargo does not spawn LuaJIT.
//! `LUAJIT_ORACLE` compares that same file when it is set. A configured binary
//! that cannot start is an infrastructure failure, not a differential result.
//! An unset variable is not differential evidence.

#[allow(dead_code)]
#[path = "../common/mod.rs"]
mod common;
use common::*;

#[allow(dead_code)]
#[path = "../../src/frontends/lua/ast.rs"]
mod ast;
#[path = "../../src/frontends/lua/control.rs"]
mod control;
#[allow(dead_code)]
#[path = "../../src/frontends/lua/lexer.rs"]
mod lexer;
#[allow(dead_code)]
#[path = "../../src/frontends/lua/parser.rs"]
mod parser;
#[allow(dead_code)]
#[path = "../../src/frontends/lua/resolve.rs"]
mod resolve;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};

use vm::compiler::HostCallResolver;
use vm::{
    CallOutcome, CallReturn, CompileSourceFileOptions, Expr, FrontendImportSyntax, FrontendIr,
    HostApiBuilder, HostApiCatalog, HostFunctionRegistry, HostFunctionSchema, HostImportSchema,
    HostOwnedFunction, HostParamSchema, HostTypeSchema, JitConfig, LocalSlot, OwnedHostCall,
    ParseError, SourceFlavor, SourcePlugin, Stmt, Value, Vm, VmError, VmStatus,
};

const FUEL_BUDGET: u64 = 8_192;
const FUEL_CHECK_INTERVAL: u32 = 1;
const ORACLE_VERSION_BASELINE: &str = "LuaJIT 2.1.1795119671";

const FORWARD_GOTO: &str = "\
local skipped = 0\n\
local hit = 0\n\
local after = 0\n\
goto target\n\
skipped = 1\n\
::target::\n\
hit = 1\n\
after = 1\n\
return skipped + hit * 10 + after * 100\n\
";

struct ControlLuaPlugin;

static CONTROL_LUA_PLUGIN: ControlLuaPlugin = ControlLuaPlugin;

impl SourcePlugin for ControlLuaPlugin {
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
        let resolver = HostCallResolver::new(holds_catalog());
        control::lower_source_with(source, Some(&resolver))
    }

    fn parse_module_imports(
        &self,
        _source: &str,
        _path: &Path,
    ) -> Result<Vec<vm::ModuleImport>, vm::SourcePathError> {
        Ok(Vec::new())
    }
}

fn candidate_options() -> CompileSourceFileOptions {
    CompileSourceFileOptions::new().with_source_plugin(&CONTROL_LUA_PLUGIN)
}

fn numeric_options() -> CompileSourceFileOptions {
    candidate_options().with_host_api_catalog(Arc::clone(holds_catalog()))
}

struct NumericForHolds;

impl HostOwnedFunction for NumericForHolds {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> Result<CallOutcome, VmError> {
        let [Value::Float(index), Value::Float(limit), Value::Float(step)] = call.args() else {
            return Err(VmError::HostError(
                "lua_control::numeric_for_holds requires three floats".into(),
            ));
        };
        // `>=` / `<=` are ordered: NaN makes both false. That is not
        // `!(index < limit)`, which would be true for NaN. Sign comes from
        // the step bit, so +0 and -0 stay distinct.
        let holds = if step.is_sign_negative() {
            *index >= *limit
        } else {
            *index <= *limit
        };
        Ok(CallOutcome::Return(CallReturn::one(Value::Bool(holds))))
    }
}

fn holds_schema() -> HostFunctionSchema {
    HostFunctionSchema::with_return(
        control::NUMERIC_FOR_HOLDS,
        vec![
            HostParamSchema::value("index", HostTypeSchema::Float),
            HostParamSchema::value("limit", HostTypeSchema::Float),
            HostParamSchema::value("step", HostTypeSchema::Float),
        ],
        HostTypeSchema::Bool,
    )
}

fn holds_catalog() -> &'static Arc<HostApiCatalog> {
    static CATALOG: OnceLock<Arc<HostApiCatalog>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut builder = HostApiBuilder::new();
        builder.function(holds_schema());
        Arc::new(builder.build().expect("numeric for predicate catalog"))
    })
}

fn bind_numeric_for_holds(vm: &mut Vm) -> Result<(), VmError> {
    let catalog = holds_catalog();
    let mut registry = HostFunctionRegistry::empty();
    let function = catalog
        .function(control::NUMERIC_FOR_HOLDS)
        .expect("numeric for predicate schema");
    registry.register_exact_owned(
        control::NUMERIC_FOR_HOLDS,
        function.params.len() as u8,
        HostImportSchema::from_function(catalog, function),
        move |_| Box::new(NumericForHolds),
    )?;
    registry.authorize_registered_builtin_import(control::NUMERIC_FOR_HOLDS);
    registry.bind_vm_cached(vm)
}

fn write_fixture(label: &str, source: &str) -> PathBuf {
    let root = namespace_case_root(&format!("lua_control_{label}"));
    let path = root.join("chunk.lua");
    fs::write(&path, source).unwrap_or_else(|error| panic!("{label}: write fixture: {error}"));
    path
}

fn run_fixture(label: &str, source: &str) -> (Vec<Value>, u64) {
    run_compiled(label, source, false)
}

fn run_numeric(label: &str, source: &str) -> (Vec<Value>, u64) {
    run_compiled(label, source, true)
}

fn run_compiled(label: &str, source: &str, numeric_for: bool) -> (Vec<Value>, u64) {
    let path = write_fixture(label, source);
    let options = if numeric_for {
        numeric_options()
    } else {
        candidate_options()
    };
    let compiled = compile_source_file_with_options(&path, options)
        .unwrap_or_else(|error| panic!("{label}: compile failed: {error}"));
    let mut vm = Vm::new_with_jit_config(
        compiled.program,
        JitConfig {
            enabled: false,
            hot_loop_threshold: 8,
            max_trace_len: 256,
        },
    );
    if numeric_for {
        bind_numeric_for_holds(&mut vm)
            .unwrap_or_else(|error| panic!("{label}: bind numeric for predicate: {error}"));
    }
    vm.set_fuel_check_interval(FUEL_CHECK_INTERVAL)
        .unwrap_or_else(|error| panic!("{label}: fuel interval: {error}"));
    vm.set_fuel(FUEL_BUDGET);
    let status = vm
        .run()
        .unwrap_or_else(|error| panic!("{label}: vm run failed: {error}"));
    assert_eq!(status, VmStatus::Halted, "{label}: status");
    let remaining = vm
        .get_fuel()
        .unwrap_or_else(|| panic!("{label}: fuel metering missing"));
    let consumed = FUEL_BUDGET
        .checked_sub(remaining)
        .unwrap_or_else(|| panic!("{label}: fuel increased"));
    let stack = vm.stack().to_vec();
    eprintln!(
        "LUA_CONTROL label={label} fuel_budget={FUEL_BUDGET} check_interval={FUEL_CHECK_INTERVAL} consumed={consumed} remaining={remaining} jit=disabled stack={stack:?}"
    );
    (stack, consumed)
}

fn expect_numeric(label: &str, source: &str, expected: f64) {
    let (stack, consumed) = run_numeric(label, source);
    assert_eq!(
        stack,
        vec![Value::Float(expected)],
        "{label}: numeric for mismatch; fuel records a budget exhaustion separately from this value"
    );
    assert!(consumed > 0, "{label}: recorded no fuel");
    assert!(
        consumed < FUEL_BUDGET,
        "{label}: exhausted the fuel budget, so this is not a numeric-for result"
    );
    if let Ok(oracle) = std::env::var("LUAJIT_ORACLE") {
        let observed = oracle_return(&oracle, &write_fixture(label, source));
        assert_eq!(
            observed, expected,
            "{label}: LuaJIT returned {observed}, candidate returned {expected}"
        );
    }
}

fn expect_number(label: &str, source: &str, expected: f64) {
    let (stack, consumed) = run_fixture(label, source);
    assert_eq!(
        stack.len(),
        1,
        "{label}: stack regression, expected one chunk return, stack={stack:?}"
    );
    assert!(
        matches!(stack.first(), Some(Value::Float(_))),
        "{label}: stack regression, chunk return is not a float: {stack:?}"
    );
    assert_eq!(
        stack,
        vec![Value::Float(expected)],
        "{label}: branch or jump mismatch; a wrong then/else, continue, or goto changes a different digit"
    );
    assert!(consumed > 0, "{label}: recorded no fuel");
    assert!(consumed < FUEL_BUDGET, "{label}: exhausted the fuel budget");
    if let Ok(oracle) = std::env::var("LUAJIT_ORACLE") {
        let observed = oracle_return(&oracle, &write_fixture(label, source));
        assert_eq!(
            observed, expected,
            "{label}: LuaJIT returned {observed}, candidate returned {expected}"
        );
    }
}

const ORACLE_DRIVER: &str = r#"
assert(not jit.status())
io.write("jit=off\n")
local path = os.getenv("LUA_CONTROL_FIXTURE")
if type(path) ~= "string" or path == "" then
  io.stderr:write("FIXTURE_PATH_MISSING\n")
  os.exit(3)
end
if arg[0] == path then
  io.stderr:write("FIXTURE_IS_MAIN_SCRIPT\n")
  os.exit(4)
end
local file, open_err = io.open(path, "rb")
if not file then
  io.stderr:write("FIXTURE_OPEN_FAILED\n")
  io.stderr:write(open_err or "", "\n")
  os.exit(5)
end
local source = file:read("*a")
file:close()
local chunk, err = load(source, "@control-fixture", "t")
if not chunk then
  io.stderr:write("LOAD_REJECT\n")
  io.stderr:write(err or "", "\n")
  os.exit(2)
end
local repeats = tonumber(os.getenv("LUA_CONTROL_REPEATS") or "1")
if repeats ~= 1 and repeats ~= 2 then
  io.stderr:write("BAD_REPEATS\n")
  os.exit(6)
end
local hits = 0
debug.sethook(function()
  local info = debug.getinfo(2, "S")
  if info and info.source == "@control-fixture" then
    hits = hits + 1
  end
end, "l")
local value
for _ = 1, repeats do
  value = chunk()
end
debug.sethook()
io.write("calls=", repeats, "\n")
io.write("lines=", hits, "\n")
io.write("argc=", #arg, "\n")
io.write("arg0=", tostring(arg[0]), "\n")
if value == nil then
  io.write("nil\n")
else
  io.write(string.format("%.17g\n", value), "\n")
end
"#;

struct OracleRun {
    calls: u32,
    lines: u64,
    argc: u32,
    arg0: String,
    value: f64,
}

fn oracle_return(oracle: &str, path: &Path) -> f64 {
    let run = oracle_run(oracle, path, 1).unwrap_or_else(|error| {
        panic!(
            "LUAJIT_ORACLE at {oracle:?} failed to run {}: {error}",
            path.display()
        )
    });
    assert_eq!(run.calls, 1, "business fixture must be called once");
    assert!(run.lines > 0, "line hook saw no fixture execution");
    assert_eq!(run.argc, 0, "fixture path was passed as a script argument");
    assert_eq!(run.arg0, "nil", "fixture path leaked into arg[0]");
    run.value
}

fn oracle_run(oracle: &str, path: &Path, repeats: u32) -> Result<OracleRun, String> {
    let output = spawn_oracle(oracle, &["-e", ORACLE_DRIVER], Some(path), Some(repeats))
        .map_err(|error| format!("failed to start: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if !output.status.success() {
        return Err(format!(
            "status={} stdout={stdout:?} stderr={stderr:?}",
            output.status
        ));
    }
    parse_oracle_stdout(&stdout).map_err(|error| format!("{error}; stdout={stdout:?}"))
}

fn parse_oracle_stdout(stdout: &str) -> Result<OracleRun, String> {
    let mut calls = None;
    let mut lines = None;
    let mut argc = None;
    let mut arg0 = None;
    let mut value = None;
    let mut saw_jit_off = false;
    for line in stdout.lines() {
        if line == "jit=off" {
            saw_jit_off = true;
        } else if let Some(rest) = line.strip_prefix("calls=") {
            calls = rest.parse::<u32>().ok();
        } else if let Some(rest) = line.strip_prefix("lines=") {
            lines = rest.parse::<u64>().ok();
        } else if let Some(rest) = line.strip_prefix("argc=") {
            argc = rest.parse::<u32>().ok();
        } else if let Some(rest) = line.strip_prefix("arg0=") {
            arg0 = Some(rest.to_string());
        } else if line == "nil" {
            return Err("fixture returned nil".to_string());
        } else if value.is_none() {
            value = line.parse::<f64>().ok();
        }
    }
    if !saw_jit_off {
        return Err("jit.status() was not asserted off".to_string());
    }
    Ok(OracleRun {
        calls: calls.ok_or_else(|| "missing calls".to_string())?,
        lines: lines.ok_or_else(|| "missing lines".to_string())?,
        argc: argc.ok_or_else(|| "missing argc".to_string())?,
        arg0: arg0.ok_or_else(|| "missing arg0".to_string())?,
        value: value.ok_or_else(|| "missing value".to_string())?,
    })
}

fn oracle_load_rejection(oracle: &str, path: &Path) -> std::process::Output {
    spawn_oracle(oracle, &["-e", ORACLE_DRIVER], Some(path), Some(1)).unwrap_or_else(|error| {
        panic!(
            "LUAJIT_ORACLE at {oracle:?} failed to start while checking a load rejection: {error}"
        )
    })
}

fn try_spawn_oracle(
    path: &str,
    args: &[&str],
    fixture: Option<&Path>,
    repeats: Option<u32>,
) -> std::io::Result<std::process::Output> {
    let mut cmd = Command::new(path);
    cmd.arg("-joff");
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(fixture) = fixture {
        cmd.env("LUA_CONTROL_FIXTURE", fixture);
    } else {
        cmd.env_remove("LUA_CONTROL_FIXTURE");
    }
    if let Some(repeats) = repeats {
        cmd.env("LUA_CONTROL_REPEATS", repeats.to_string());
    } else {
        cmd.env_remove("LUA_CONTROL_REPEATS");
    }
    #[cfg(target_os = "linux")]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(limit_oracle_child);
    }
    cmd.output()
}

fn spawn_oracle(
    path: &str,
    args: &[&str],
    fixture: Option<&Path>,
    repeats: Option<u32>,
) -> std::io::Result<std::process::Output> {
    try_spawn_oracle(path, args, fixture, repeats)
}

#[cfg(target_os = "linux")]
fn limit_oracle_child() -> std::io::Result<()> {
    #[repr(C)]
    struct RLimit {
        cur: u64,
        max: u64,
    }
    unsafe extern "C" {
        fn setrlimit(resource: i32, rlim: *const RLimit) -> i32;
        fn alarm(seconds: u32) -> u32;
        fn _exit(code: i32) -> !;
    }
    const RLIMIT_CPU: i32 = 0;
    const RLIMIT_AS: i32 = 9;
    let cpu = RLimit { cur: 2, max: 2 };
    let address = RLimit {
        cur: 128 * 1024 * 1024,
        max: 128 * 1024 * 1024,
    };
    unsafe {
        if setrlimit(RLIMIT_CPU, &cpu) != 0 || setrlimit(RLIMIT_AS, &address) != 0 {
            _exit(126);
        }
        alarm(5);
    }
    Ok(())
}

#[test]
fn forward_goto_skips_the_write_and_runs_the_target_once() {
    // skipped=0, hit=1, after=1. A missed skip, a missed target, or a second
    // pass changes a different decimal place.
    expect_number("forward_goto", FORWARD_GOTO, 110.0);
}

const BACKWARD_GOTO: &str = "\
local n = 0\n\
local trips = 0\n\
goto check\n\
::check::\n\
if n < 3 then\n\
  n = n + 1\n\
  trips = trips + 1\n\
  goto check\n\
end\n\
return n * 10 + trips\n\
";

const CROSS_DO: &str = "\
local x = 1\n\
local seen = 0\n\
do\n\
  ::out::\n\
  seen = 5\n\
  local x = 10\n\
end\n\
do\n\
  local x = 100\n\
  x = 1000\n\
  goto out\n\
  seen = 9\n\
end\n\
::out::\n\
return x * 100 + seen\n\
";

const OUTER_X_ONLY: &str = "\
local x = 1\n\
do\n\
  ::out::\n\
  local x = 10\n\
end\n\
do\n\
  local x = 100\n\
  x = 1000\n\
  goto out\n\
end\n\
::out::\n\
return x\n\
";

const NESTED_CONTINUE: &str = "\
local skipped = 0\n\
local n = 0\n\
local tails = 0\n\
local after = 0\n\
goto enter\n\
skipped = 1\n\
::enter::\n\
while n < 3 do\n\
  n = n + 1\n\
  if n < 3 then\n\
    continue\n\
  end\n\
  tails = tails + 1\n\
end\n\
after = 1\n\
return skipped * 1000 + n * 100 + tails * 10 + after\n\
";

const NESTED_BREAK: &str = "\
local skipped = 0\n\
local m = 0\n\
local inner = 0\n\
local after = 0\n\
goto enter\n\
skipped = 1\n\
::enter::\n\
while m < 5 do\n\
  m = m + 1\n\
  if m > 1 then\n\
    break\n\
  end\n\
  inner = inner + 1\n\
end\n\
after = 1\n\
return skipped * 1000 + m * 100 + inner * 10 + after\n\
";

const EARLY_RETURN: &str = "\
local tail = 0\n\
do\n\
  return 7\n\
end\n\
tail = 1\n\
return tail\n\
";

const DIRECT_LOOP: &str = "\
local i = 0\n\
local n = 0\n\
while i < 4 do\n\
  i = i + 1\n\
  n = n + 1\n\
end\n\
return n\n\
";

const DISPATCH_LOOP: &str = "\
local i = 0\n\
local n = 0\n\
::loop::\n\
if i < 4 then\n\
  i = i + 1\n\
  n = n + 1\n\
  goto loop\n\
end\n\
return n\n\
";

const JUMP_INTO_LOCAL: &str = "\
goto target\n\
local hidden = 1\n\
::target::\n\
return hidden\n\
";

const BASELINE_DRIVER: &str = "\
assert(not jit.status())\n\
io.write(jit.version, '\\n')\n\
io.write('jit=off\\n')\n\
local g = load('local goto = 1', '@g', 't')\n\
local b = load('for i=1,2 do break print(i) end', '@b', 't')\n\
io.write(g and 'BAD_GOTO' or 'GOTO_KW', '\\n')\n\
io.write(b and 'BREAK_OK' or 'BREAK_BAD', '\\n')\n\
io.write(type(table.pack), '\\n')\n\
";

fn lowered(source: &str) -> FrontendIr {
    control::lower_source(source).unwrap_or_else(|error| panic!("lower failed: {error}"))
}

fn lowered_numeric(source: &str) -> FrontendIr {
    let resolver = HostCallResolver::new(holds_catalog());
    control::lower_source_with(source, Some(&resolver))
        .unwrap_or_else(|error| panic!("lower failed: {error}"))
}

fn slot_named(ir: &FrontendIr, name: &str) -> Option<LocalSlot> {
    ir.local_bindings
        .iter()
        .find(|(binding, _)| binding == name)
        .map(|(_, slot)| *slot)
}

fn is_lua_ident(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    let head = first == b'_' || first.is_ascii_alphabetic() || first >= 128;
    head && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric() || byte >= 128)
}

fn walk_stmts(stmts: &[Stmt], visit: &mut impl FnMut(&Stmt)) {
    for stmt in stmts {
        visit(stmt);
        match stmt {
            Stmt::While { body, .. } => walk_stmts(body, visit),
            Stmt::IfElse {
                then_branch,
                else_branch,
                ..
            } => {
                walk_stmts(then_branch, visit);
                walk_stmts(else_branch, visit);
            }
            Stmt::For {
                init, post, body, ..
            } => {
                walk_stmts(std::slice::from_ref(init.as_ref()), visit);
                walk_stmts(std::slice::from_ref(post.as_ref()), visit);
                walk_stmts(body, visit);
            }
            _ => {}
        }
    }
}

fn is_dispatch_state_while(condition: &Expr, state: LocalSlot) -> bool {
    matches!(
        condition,
        Expr::Not(inner)
            if matches!(
                inner.as_ref(),
                Expr::Eq(lhs, rhs)
                    if matches!(lhs.as_ref(), Expr::Var(slot) if *slot == state)
                        && matches!(rhs.as_ref(), Expr::Int(0))
            )
    )
}

fn source_while_count(stmts: &[Stmt], state: Option<LocalSlot>) -> usize {
    let mut count = 0;
    walk_stmts(stmts, &mut |stmt| {
        if let Stmt::While { condition, .. } = stmt
            && !state.is_some_and(|slot| is_dispatch_state_while(condition, slot))
        {
            count += 1;
        }
    });
    count
}

fn dispatch_while_count(stmts: &[Stmt], state: LocalSlot) -> usize {
    let mut count = 0;
    walk_stmts(stmts, &mut |stmt| {
        if let Stmt::While { condition, .. } = stmt
            && matches!(
                condition,
                Expr::Not(inner)
                    if matches!(
                        inner.as_ref(),
                        Expr::Eq(lhs, rhs)
                            if matches!(lhs.as_ref(), Expr::Var(slot) if *slot == state)
                                && matches!(rhs.as_ref(), Expr::Int(0))
                    )
            )
        {
            count += 1;
        }
    });
    count
}

fn assert_private_slots(ir: &FrontendIr, guest_names: &[&str]) {
    let names: Vec<&str> = ir
        .local_bindings
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    for guest in guest_names {
        assert!(
            !names.contains(guest),
            "guest name {guest} entered the Core namespace: {names:?}"
        );
    }
    let state = names.iter().find(|name| name.contains("dispatch")).copied();
    if let Some(state) = state {
        assert!(!is_lua_ident(state), "{state} is a guest identifier");
    }
    let user: Vec<LocalSlot> = ir
        .local_bindings
        .iter()
        .filter(|(name, _)| name.starts_with("$binding_"))
        .map(|(_, slot)| *slot)
        .collect();
    let mut unique = user.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), user.len(), "user slots alias: {user:?}");
}

fn repeat_flag_bindings(ir: &FrontendIr) -> Vec<(&str, LocalSlot)> {
    ir.local_bindings
        .iter()
        .filter(|(name, _)| name.starts_with("$lua_repeat_again_"))
        .map(|(name, slot)| (name.as_str(), *slot))
        .collect()
}

fn assert_repeat_fors(ir: &FrontendIr, expected: usize) {
    let flags = repeat_flag_bindings(ir);
    assert_eq!(
        flags.len(),
        expected,
        "one private flag per repeat, bindings={flags:?}"
    );
    for (name, _) in &flags {
        assert!(!is_lua_ident(name), "{name} is a guest identifier");
    }
    let mut slots: Vec<LocalSlot> = flags.iter().map(|(_, slot)| *slot).collect();
    slots.sort_unstable();
    slots.dedup();
    assert_eq!(slots.len(), expected, "repeat flags alias: {flags:?}");
    let mut seen = 0;
    let mut total_fors = 0;
    walk_stmts(&ir.stmts, &mut |stmt| {
        if let Stmt::For {
            init,
            condition,
            post,
            ..
        } = stmt
        {
            total_fors += 1;
            let Stmt::Let {
                index,
                expr: init_expr,
                ..
            } = init.as_ref()
            else {
                return;
            };
            if !flags.iter().any(|(_, slot)| slot == index) {
                return;
            }
            seen += 1;
            assert!(
                matches!(init_expr, Expr::Bool(true)),
                "repeat flag must start true so the body runs before until: {init_expr:?}"
            );
            assert!(
                flags.iter().any(|(_, slot)| slot == index),
                "repeat flag {index:?} is not a private repeat slot: {flags:?}"
            );
            assert!(
                matches!(condition, Expr::Var(slot) if slot == index),
                "repeat condition must be the flag, not a second until: {condition:?}"
            );
            let Stmt::Assign {
                index: assigned,
                expr: post_expr,
                ..
            } = post.as_ref()
            else {
                panic!("repeat post is not the until store: {post:?}");
            };
            assert_eq!(assigned, index, "until must update the same repeat flag");
            assert!(
                matches!(
                    post_expr,
                    Expr::Not(outer)
                        if matches!(
                            outer.as_ref(),
                            Expr::Not(inner)
                                if matches!(
                                    inner.as_ref(),
                                    Expr::Eq(_, rhs) if matches!(rhs.as_ref(), Expr::Bool(false))
                                )
                        )
                ),
                "until must be stored once as not core_truthy: {post_expr:?}"
            );
        }
    });
    assert_eq!(seen, expected, "each repeat is one for, not a copied body");
    // These call sites are pure repeat shapes. The same walk counts every
    // For, including one whose init is not the private flag.
    assert_eq!(
        total_fors, expected,
        "pure repeat shape must count every for; flag matching alone misses a non-flag for, flags={flags:?}"
    );
}

#[test]
fn backward_goto_stops_after_three_trips() {
    // One trip is 11, two is 22, four is 44. An open loop exhausts fuel.
    expect_number("backward_goto", BACKWARD_GOTO, 33.0);
}

#[test]
fn cross_do_uses_resolver_identity_not_the_first_same_name() {
    let resolution = {
        let chunk = parser::parse_chunk(CROSS_DO.as_bytes()).expect("parse cross do");
        resolve::resolve(&chunk)
    };
    assert!(
        resolution.errors.is_empty(),
        "cross do is legal: {:?}",
        resolution.errors
    );
    assert_eq!(resolution.gotos.len(), 1);
    let target = resolution.gotos[0].target.expect("resolved target");
    assert_ne!(target, 0, "the first ::out:: is a different label");
    assert_eq!(resolution.labels[target].name, resolution.labels[0].name);
    assert!(resolution.labels[target].span.start > resolution.labels[0].span.start);
    let ir = lowered(CROSS_DO);
    assert_private_slots(&ir, &["x", "seen", "out"]);
    let x_slots: Vec<LocalSlot> = ir
        .local_bindings
        .iter()
        .filter(|(name, _)| name == "$binding_0" || name == "$binding_2" || name == "$binding_3")
        .map(|(_, slot)| *slot)
        .collect();
    assert_eq!(
        x_slots.len(),
        3,
        "three x bindings: {:?}",
        ir.local_bindings
    );
    assert_ne!(x_slots[0], x_slots[1]);
    assert_ne!(x_slots[0], x_slots[2]);
    // 105: outer x stays 1, first do sets seen=5, jump skips seen=9.
    // Falling through is 109. Aliasing the inner write onto outer x is 100005.
    // Jumping at the earlier same-name label loops until fuel runs out.
    expect_number("cross_do", CROSS_DO, 105.0);
    expect_number("outer_x", OUTER_X_ONLY, 1.0);
}

#[test]
fn nested_source_loop_break_and_continue_stay_inside_the_state() {
    // continue ignored writes tails three times (331). continue leaving the
    // dispatcher leaves after at 0 (100). break leaving the dispatcher leaves
    // after at 0 (210). The taken path is 311 and 211.
    expect_number("nested_continue", NESTED_CONTINUE, 311.0);
    expect_number("nested_break", NESTED_BREAK, 211.0);
    let ir = lowered(NESTED_CONTINUE);
    let state = slot_named(&ir, "$lua_dispatch_state").expect("dispatch state");
    assert_eq!(dispatch_while_count(&ir.stmts, state), 1);
    assert_eq!(source_while_count(&ir.stmts, Some(state)), 1);
    assert!(!is_lua_ident("$lua_dispatch_state"));
}

#[test]
fn early_return_skips_the_tail_write_and_pushes_one_value() {
    let (stack, consumed) = run_fixture("early_return", EARLY_RETURN);
    assert_eq!(stack.len(), 1, "stack={stack:?}");
    assert_eq!(
        stack,
        vec![Value::Float(7.0)],
        "tail return would be 1 or 0"
    );
    assert!(consumed > 0 && consumed < FUEL_BUDGET);
    if let Ok(oracle) = std::env::var("LUAJIT_ORACLE") {
        let observed = oracle_return(&oracle, &write_fixture("early_return", EARLY_RETURN));
        assert_eq!(observed, 7.0);
    }
}

#[test]
fn direct_loop_stays_structured_and_matches_the_dispatched_loop() {
    let (direct_stack, direct_fuel) = run_fixture("direct_loop", DIRECT_LOOP);
    let (dispatch_stack, dispatch_fuel) = run_fixture("dispatch_loop", DISPATCH_LOOP);
    assert_eq!(direct_stack, vec![Value::Float(4.0)]);
    assert_eq!(dispatch_stack, direct_stack);
    let direct = lowered(DIRECT_LOOP);
    let dispatch = lowered(DISPATCH_LOOP);
    assert!(slot_named(&direct, "$lua_dispatch_state").is_none());
    assert_eq!(source_while_count(&direct.stmts, None), 1);
    assert_eq!(dispatch_while_count(&direct.stmts, 0), 0);
    let state = slot_named(&dispatch, "$lua_dispatch_state").expect("dispatch state");
    assert!(!is_lua_ident("$lua_dispatch_state"));
    assert_eq!(source_while_count(&dispatch.stmts, Some(state)), 0);
    assert_eq!(dispatch_while_count(&dispatch.stmts, state), 1);
    assert_private_slots(&direct, &["i", "n"]);
    assert_private_slots(&dispatch, &["i", "n", "loop"]);
    eprintln!(
        "LUA_CONTROL cost iters=4 result=4 direct_fuel={direct_fuel} dispatch_fuel={dispatch_fuel} direct_shape=source_while dispatch_shape=state_while interpreter=core-vm jit=disabled fuel_budget={FUEL_BUDGET} check_interval={FUEL_CHECK_INTERVAL}"
    );
    if let Ok(oracle) = std::env::var("LUAJIT_ORACLE") {
        assert_eq!(
            oracle_return(&oracle, &write_fixture("direct_loop", DIRECT_LOOP)),
            4.0
        );
        assert_eq!(
            oracle_return(&oracle, &write_fixture("dispatch_loop", DISPATCH_LOOP)),
            4.0
        );
    }
}

#[test]
fn jump_into_local_is_a_static_rejection() {
    let error = control::lower_source(JUMP_INTO_LOCAL).expect_err("resolver rejection");
    let text = error.to_string();
    assert!(
        text.contains("jump into the scope of local hidden"),
        "{text}"
    );
    let path = write_fixture("jump_into_local", JUMP_INTO_LOCAL);
    let compiled = compile_source_file_with_options(&path, candidate_options());
    assert!(compiled.is_err(), "invalid jump must not produce a program");
    let Ok(oracle) = std::env::var("LUAJIT_ORACLE") else {
        eprintln!("LUAJIT_ORACLE is unset; jump-into-local is not a differential result");
        return;
    };
    let output = oracle_load_rejection(&oracle, &path);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let code = output.status.code();
    assert_eq!(
        code,
        Some(2),
        "text-mode load rejection is exit 2; a process failure has a different status={} stderr={stderr}",
        output.status
    );
    assert!(
        stderr.contains("LOAD_REJECT"),
        "load rejection marker missing; this is not a full-text match: {stderr}"
    );
    let detail = stderr
        .lines()
        .skip_while(|line| *line != "LOAD_REJECT")
        .skip(1)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !detail.trim().is_empty(),
        "load rejection had no oracle diagnostic"
    );
}

#[test]
fn oracle_control_config_matches_locked_52_build() {
    let Ok(path) = std::env::var("LUAJIT_ORACLE") else {
        eprintln!("LUAJIT_ORACLE is unset; this is not a differential GREEN");
        return;
    };
    let output = spawn_oracle(&path, &["-e", BASELINE_DRIVER], None, None)
        .unwrap_or_else(|error| panic!("LUAJIT_ORACLE at {path:?} failed to start: {error}"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "LUAJIT_ORACLE at {path:?} baseline failed: status={} stdout={stdout:?} stderr={stderr:?}",
        output.status
    );
    let expected = format!("{ORACLE_VERSION_BASELINE}\njit=off\nGOTO_KW\nBREAK_OK\nfunction\n");
    assert_eq!(
        stdout, expected,
        "oracle is not the locked 5.2-compat build"
    );
    eprintln!("oracle baseline {ORACLE_VERSION_BASELINE} GOTO_KW BREAK_OK table.pack");
}

// Digits name the taken branch. A wrong then/else changes one place and
// cannot be confused with a missed goto, a continue that falls through, or
// a stack that is not one float.
//
// 1 if 0 then; 2 would mean 0 was falsy.
// 10 if 7 then; 20 would mean a nonzero number was falsy.
// 200 if false else; 100 would mean false was truthy.
// 1000 if true then; 2000 would mean true was falsy.
// 20000 elseif of the true local; 10000 is the false arm, 40000 is else.
// 200000 not 0 stored, then used as a condition, takes else.
// 1000000 not false stored, then takes then.
// 20000000 not true stored, then takes else.
// 200000000 not 4 stored, then takes else.
// 2000000000 !0 takes else.
// Sum 2221221211.
const TRUTH_STEPS: &str = "\
local n = 0\n\
local x = 0\n\
if x then\n\
  n = n + 1\n\
else\n\
  n = n + 2\n\
end\n\
x = 7\n\
if x then\n\
  n = n + 10\n\
else\n\
  n = n + 20\n\
end\n\
x = false\n\
if x then\n\
  n = n + 100\n\
else\n\
  n = n + 200\n\
end\n\
x = true\n\
if x then\n\
  n = n + 1000\n\
else\n\
  n = n + 2000\n\
end\n\
local y = false\n\
if y then\n\
  n = n + 10000\n\
elseif x then\n\
  n = n + 20000\n\
else\n\
  n = n + 40000\n\
end\n\
x = 0\n\
local flag = not x\n\
if flag then\n\
  n = n + 100000\n\
else\n\
  n = n + 200000\n\
end\n\
flag = not false\n\
if flag then\n\
  n = n + 1000000\n\
else\n\
  n = n + 2000000\n\
end\n\
flag = not true\n\
if flag then\n\
  n = n + 10000000\n\
else\n\
  n = n + 20000000\n\
end\n\
flag = not 4\n\
if flag then\n\
  n = n + 100000000\n\
else\n\
  n = n + 200000000\n\
end\n\
if !0 then\n\
  n = n + 1000000000\n\
else\n\
  n = n + 2000000000\n\
end\n\
";

const TRUTH_EXPECTED: f64 = 2_221_221_211.0;

fn truth_direct() -> String {
    format!("{TRUTH_STEPS}return n\n")
}

fn truth_dispatch() -> String {
    format!(
        "local skipped = 0\n\
goto body\n\
skipped = 9\n\
::body::\n\
{TRUTH_STEPS}\
return n + skipped * 10000000000\n"
    )
}

// 1 entered while x was 0. 10 continued after x became 5.
// 100 continue skipped the tail until the last trip; 300 means continue fell through.
// 1000 reached the statement after the bool loop; 0 means continue left the dispatcher.
// 10000 would mean while false entered.
// 300000 is three trips of the bool loop.
// 1000000 is one trip of a while whose condition is a stored not result.
// Sum 1301111. A missed dispatch goto adds 70000000.
const WHILE_STEPS: &str = "\
local seen_zero = 0\n\
local seen_five = 0\n\
local x = 0\n\
while x do\n\
  if x == 0 then\n\
    seen_zero = 1\n\
    x = 5\n\
  else\n\
    seen_five = 1\n\
    break\n\
  end\n\
end\n\
local tails = 0\n\
local after = 0\n\
local trips = 0\n\
local b = true\n\
while b do\n\
  trips = trips + 1\n\
  if trips < 3 then\n\
    continue\n\
  end\n\
  tails = tails + 1\n\
  b = false\n\
end\n\
after = 1\n\
local blocked = 0\n\
b = false\n\
while b do\n\
  blocked = blocked + 1\n\
  break\n\
end\n\
local flag = not false\n\
local not_trips = 0\n\
while flag do\n\
  not_trips = not_trips + 1\n\
  flag = not true\n\
end\n\
";

const WHILE_EXPECTED: f64 = 1_301_111.0;

fn while_direct() -> String {
    format!(
        "{WHILE_STEPS}\
return seen_zero + seen_five * 10 + tails * 100 + after * 1000 + blocked * 10000 + trips * 100000 + not_trips * 1000000\n"
    )
}

fn while_dispatch() -> String {
    format!(
        "local skipped = 0\n\
goto body\n\
skipped = 7\n\
::body::\n\
{WHILE_STEPS}\
return seen_zero + seen_five * 10 + tails * 100 + after * 1000 + blocked * 10000 + trips * 100000 + not_trips * 1000000 + skipped * 10000000\n"
    )
}

// 1 if x + y then. 20 if not (x + y) else. 100 while x + y entered once.
// 2000 elseif x + y. Sum 2121. Each of those four conditions contains one Add.
const ONCE_STEPS: &str = "\
local x = 1\n\
local y = 2\n\
local n = 0\n\
if x + y then\n\
  n = n + 1\n\
else\n\
  n = n + 2\n\
end\n\
if not (x + y) then\n\
  n = n + 10\n\
else\n\
  n = n + 20\n\
end\n\
while x + y do\n\
  n = n + 100\n\
  break\n\
end\n\
if false then\n\
  n = n + 1000\n\
elseif x + y then\n\
  n = n + 2000\n\
else\n\
  n = n + 4000\n\
end\n\
";

const ONCE_EXPECTED: f64 = 2_121.0;

fn once_direct() -> String {
    format!("{ONCE_STEPS}return n\n")
}

fn once_dispatch() -> String {
    format!(
        "local skipped = 0\n\
goto body\n\
skipped = 3\n\
::body::\n\
{ONCE_STEPS}\
return n + skipped * 10000\n"
    )
}

fn walk_expr(expr: &Expr, visit: &mut impl FnMut(&Expr)) {
    visit(expr);
    match expr {
        Expr::Not(inner) | Expr::Neg(inner) => walk_expr(inner, visit),
        Expr::Eq(lhs, rhs)
        | Expr::Lt(lhs, rhs)
        | Expr::Gt(lhs, rhs)
        | Expr::Add(lhs, rhs)
        | Expr::Sub(lhs, rhs)
        | Expr::Mul(lhs, rhs)
        | Expr::Div(lhs, rhs) => {
            walk_expr(lhs, visit);
            walk_expr(rhs, visit);
        }
        _ => {}
    }
}

fn add_count(expr: &Expr) -> usize {
    let mut count = 0;
    walk_expr(expr, &mut |item| {
        if matches!(item, Expr::Add(_, _)) {
            count += 1;
        }
    });
    count
}

fn condition_add_counts(stmts: &[Stmt]) -> Vec<usize> {
    let mut counts = Vec::new();
    walk_stmts(stmts, &mut |stmt| match stmt {
        Stmt::IfElse { condition, .. } | Stmt::While { condition, .. } => {
            counts.push(add_count(condition));
        }
        _ => {}
    });
    counts
}

fn assert_each_add_condition_is_once(label: &str, source: &str) {
    let ir = lowered(source);
    let counts = condition_add_counts(&ir.stmts);
    let with_add: Vec<usize> = counts.into_iter().filter(|count| *count > 0).collect();
    assert_eq!(
        with_add,
        vec![1, 1, 1, 1],
        "{label}: each condition expression must be lowered once, counts={with_add:?}"
    );
}

#[test]
fn direct_truthiness_uses_zero_as_true_and_false_as_false() {
    let source = truth_direct();
    let path = write_fixture("direct_truthiness", &source);
    assert!(path.extension().is_some_and(|ext| ext == "lua"));
    expect_number("direct_truthiness", &source, TRUTH_EXPECTED);
}

#[test]
fn dispatch_truthiness_matches_direct_after_a_goto() {
    expect_number("dispatch_truthiness", &truth_dispatch(), TRUTH_EXPECTED);
}

#[test]
fn direct_while_treats_zero_as_true_and_keeps_continue() {
    expect_number("direct_while_truth", &while_direct(), WHILE_EXPECTED);
}

#[test]
fn dispatch_while_treats_zero_as_true_and_keeps_continue() {
    expect_number("dispatch_while_truth", &while_dispatch(), WHILE_EXPECTED);
}

#[test]
fn direct_condition_expression_is_lowered_once() {
    let source = once_direct();
    expect_number("direct_once", &source, ONCE_EXPECTED);
    assert_each_add_condition_is_once("direct_once", &source);
}

#[test]
fn dispatch_condition_expression_is_lowered_once() {
    let source = once_dispatch();
    expect_number("dispatch_once", &source, ONCE_EXPECTED);
    assert_each_add_condition_is_once("dispatch_once", &source);
}

#[test]
fn missing_configured_oracle_binary_is_an_infrastructure_failure() {
    let missing = "/mnt/workspace/rustscript-lua/tmp/lua52-syntax/missing-luajit";
    let error = try_spawn_oracle(missing, &["-e", "return 1"], None, None)
        .expect_err("a missing configured binary must fail to start");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn oracle_harness_runs_the_fixture_once_under_joff() {
    let Ok(oracle) = std::env::var("LUAJIT_ORACLE") else {
        eprintln!("LUAJIT_ORACLE is unset; harness execution count is not a differential result");
        return;
    };
    let sentinel = write_fixture("oracle_once_sentinel", "return 1\n");
    let once = oracle_run(&oracle, &sentinel, 1).expect("sentinel once");
    let twice = oracle_run(&oracle, &sentinel, 2).expect("sentinel twice");
    assert_eq!(once.calls, 1);
    assert_eq!(twice.calls, 2);
    assert_eq!(once.argc, 0);
    assert_eq!(once.arg0, "nil");
    assert!(once.lines > 0, "line hook saw no sentinel execution");
    assert_eq!(
        twice.lines,
        once.lines * 2,
        "line hook must track each execution"
    );
    assert_eq!(once.value, 1.0);
    assert_eq!(twice.value, 1.0);

    let business = write_fixture("oracle_once_business", &truth_direct());
    let business_once = oracle_run(&oracle, &business, 1).expect("business once");
    let business_twice = oracle_run(&oracle, &business, 2).expect("business twice");
    assert_eq!(business_once.calls, 1);
    assert_eq!(business_once.argc, 0);
    assert_eq!(business_twice.lines, business_once.lines * 2);
    assert_eq!(business_once.value, business_twice.value);
    assert_eq!(business_once.value, TRUTH_EXPECTED);
}

// 921: outer scoped stays 9, two rounds, continue skips the second observed write.
// The low digits are the scoped combination 21. 911 means until read the outer
// scoped. 922 means continue fell through. 221 means the inner scoped leaked.
// Fuel exhaustion means until was skipped or stayed false after round two.
const REPEAT_SHADOW_LOOP: &str = "\
repeat\n\
  n = n + 1\n\
  local scoped = n\n\
  if n == 2 then\n\
    continue\n\
  end\n\
  observed = observed + 1\n\
until scoped > 1\n\
";

fn repeat_shadow_direct() -> String {
    [
        "local n = 0\n",
        "local observed = 0\n",
        "local scoped = 9\n",
        REPEAT_SHADOW_LOOP,
        "return scoped * 100 + n * 10 + observed\n",
    ]
    .concat()
}

fn repeat_shadow_dispatch() -> String {
    [
        "local skipped = 0\n",
        "local n = 0\n",
        "local observed = 0\n",
        "local scoped = 9\n",
        "goto enter\n",
        "skipped = 1\n",
        "::enter::\n",
        REPEAT_SHADOW_LOOP,
        "return skipped * 1000 + scoped * 100 + n * 10 + observed\n",
    ]
    .concat()
}

const REPEAT_COMBINATION: &str = "\
local n = 0\n\
local observed = 0\n\
repeat\n\
  n = n + 1\n\
  local scoped = n\n\
  if n == 2 then\n\
    continue\n\
  end\n\
  observed = observed + 1\n\
until scoped > 1\n\
return n * 10 + observed\n\
";

const REPEAT_UNTIL_ZERO: &str = "\
local n = 0\n\
repeat\n\
  n = n + 1\n\
until 0\n\
return n\n\
";

const REPEAT_BREAK_FALSE: &str = "\
local n = 0\n\
local after = 0\n\
repeat\n\
  n = n + 1\n\
  if true then\n\
    break\n\
  end\n\
  n = n + 50\n\
until false\n\
after = 1\n\
return n * 10 + after\n\
";

const REPEAT_BREAK_FALSE_DISPATCH: &str = "\
local skipped = 0\n\
local n = 0\n\
local after = 0\n\
goto enter\n\
skipped = 1\n\
::enter::\n\
repeat\n\
  n = n + 1\n\
  if true then\n\
    break\n\
  end\n\
  n = n + 50\n\
until false\n\
after = 1\n\
return skipped * 100 + n * 10 + after\n\
";

const NESTED_REPEAT_BREAK_ELEVEN: &str = "\
local n = 0\n\
local inner = 0\n\
repeat\n\
  n = n + 1\n\
  repeat\n\
    inner = inner + 1\n\
    break\n\
  until false\n\
until n > 0\n\
return n * 10 + inner\n\
";

const NESTED_REPEAT_BREAK: &str = "\
local outer = 0\n\
local inner = 0\n\
local after_inner = 0\n\
local after = 0\n\
repeat\n\
  outer = outer + 1\n\
  repeat\n\
    inner = inner + 1\n\
    break\n\
  until false\n\
  after_inner = 1\n\
  break\n\
until false\n\
after = 1\n\
return outer * 1000 + inner * 100 + after_inner * 10 + after\n\
";

const NESTED_REPEAT_BREAK_DISPATCH: &str = "\
local skipped = 0\n\
goto enter\n\
skipped = 1\n\
::enter::\n\
local outer = 0\n\
local inner = 0\n\
local after_inner = 0\n\
local after = 0\n\
repeat\n\
  outer = outer + 1\n\
  repeat\n\
    inner = inner + 1\n\
    break\n\
  until false\n\
  after_inner = 1\n\
  break\n\
until false\n\
after = 1\n\
return skipped * 10000 + outer * 1000 + inner * 100 + after_inner * 10 + after\n\
";

const REPEAT_CONTINUE_PAIR: &str = "\
local a = 0\n\
local atails = 0\n\
repeat\n\
  a = a + 1\n\
  if a == 2 then\n\
    continue\n\
  end\n\
  atails = atails + 1\n\
until a > 1\n\
local b = 0\n\
local btails = 0\n\
repeat\n\
  b = b + 1\n\
  if b == 1 then\n\
    continue\n\
  end\n\
  btails = btails + 1\n\
until b > 2\n\
return a * 1000 + atails * 100 + b * 10 + btails\n\
";

const REPEAT_CONTINUE_PAIR_DISPATCH: &str = "\
local skipped = 0\n\
goto enter\n\
skipped = 1\n\
::enter::\n\
local a = 0\n\
local atails = 0\n\
repeat\n\
  a = a + 1\n\
  if a == 2 then\n\
    continue\n\
  end\n\
  atails = atails + 1\n\
until a > 1\n\
local b = 0\n\
local btails = 0\n\
repeat\n\
  b = b + 1\n\
  if b == 1 then\n\
    continue\n\
  end\n\
  btails = btails + 1\n\
until b > 2\n\
return skipped * 10000 + a * 1000 + atails * 100 + b * 10 + btails\n\
";

const NESTED_WHILE_IN_REPEAT: &str = "\
local outer = 0\n\
local inner = 0\n\
local inner_tails = 0\n\
local outer_tails = 0\n\
local after = 0\n\
repeat\n\
  outer = outer + 1\n\
  while inner < 2 do\n\
    inner = inner + 1\n\
    if inner == 1 then\n\
      continue\n\
    end\n\
    inner_tails = inner_tails + 1\n\
  end\n\
  if outer == 1 then\n\
    continue\n\
  end\n\
  outer_tails = outer_tails + 1\n\
until outer > 1\n\
after = 1\n\
return after * 10000 + outer * 1000 + inner * 100 + inner_tails * 10 + outer_tails\n\
";

const REPEAT_IN_WHILE: &str = "\
local w = 0\n\
local r = 0\n\
local rtail = 0\n\
while w < 1 do\n\
  w = w + 1\n\
  repeat\n\
    r = r + 1\n\
    if r == 1 then\n\
      continue\n\
    end\n\
    rtail = rtail + 1\n\
  until r > 1\n\
end\n\
return w * 100 + r * 10 + rtail\n\
";

const REPEAT_SKIP_LOCAL: &str = "\
repeat if true then continue end; local skipped = 1 until skipped == 1; return 0\n\
";

#[test]
fn direct_repeat_until_reads_the_round_local_and_continue_skips_the_tail() {
    let source = repeat_shadow_direct();
    let path = write_fixture("repeat_shadow", &source);
    assert!(path.extension().is_some_and(|ext| ext == "lua"));
    expect_number("repeat_shadow", &source, 921.0);
    expect_number("repeat_combination", REPEAT_COMBINATION, 21.0);
    let ir = lowered(&source);
    assert!(slot_named(&ir, "$lua_dispatch_state").is_none());
    assert_eq!(source_while_count(&ir.stmts, None), 0);
    assert_repeat_fors(&ir, 1);
    assert_private_slots(&ir, &["n", "observed", "scoped"]);
}

#[test]
fn same_repeat_block_runs_direct_and_after_an_outer_goto() {
    let direct_source = repeat_shadow_direct();
    let dispatch_source = repeat_shadow_dispatch();
    assert!(
        direct_source.contains(REPEAT_SHADOW_LOOP) && dispatch_source.contains(REPEAT_SHADOW_LOOP)
    );
    expect_number("repeat_shadow_direct", &direct_source, 921.0);
    expect_number("repeat_shadow_dispatch", &dispatch_source, 921.0);
    let direct = lowered(&direct_source);
    let dispatch = lowered(&dispatch_source);
    assert!(slot_named(&direct, "$lua_dispatch_state").is_none());
    assert_repeat_fors(&direct, 1);
    let state = slot_named(&dispatch, "$lua_dispatch_state").expect("dispatch state");
    assert!(!is_lua_ident("$lua_dispatch_state"));
    assert_eq!(dispatch_while_count(&dispatch.stmts, state), 1);
    assert_eq!(source_while_count(&dispatch.stmts, Some(state)), 0);
    assert_repeat_fors(&dispatch, 1);
    assert_private_slots(&dispatch, &["skipped", "n", "observed", "scoped", "enter"]);
}

#[test]
fn until_zero_is_truthy_and_the_body_runs_once() {
    expect_number("repeat_until_zero", REPEAT_UNTIL_ZERO, 1.0);
    let ir = lowered(REPEAT_UNTIL_ZERO);
    assert_repeat_fors(&ir, 1);
    assert!(slot_named(&ir, "$lua_dispatch_state").is_none());
}

#[test]
fn repeat_break_skips_a_false_until_and_stays_inside_dispatch() {
    // 11: one body, break, then the statement after the repeat. 10 means the
    // break also skipped that statement. 61 means break did not fire. A
    // dispatch break that leaves the state loop does not reach `after`.
    expect_number("repeat_break_false", REPEAT_BREAK_FALSE, 11.0);
    expect_number(
        "repeat_break_false_dispatch",
        REPEAT_BREAK_FALSE_DISPATCH,
        11.0,
    );
    let direct = lowered(REPEAT_BREAK_FALSE);
    let dispatch = lowered(REPEAT_BREAK_FALSE_DISPATCH);
    assert_repeat_fors(&direct, 1);
    assert_repeat_fors(&dispatch, 1);
    let state = slot_named(&dispatch, "$lua_dispatch_state").expect("dispatch state");
    assert_eq!(dispatch_while_count(&dispatch.stmts, state), 1);
}

#[test]
fn nested_repeat_break_stays_in_the_inner_loop() {
    // 11: inner break then outer until. 1111: inner break still runs
    // after_inner, then the outer break still runs after. 1101 means the
    // inner break left the outer repeat. 1110 means the outer break left a
    // dispatcher before `after`.
    expect_number(
        "nested_repeat_break_eleven",
        NESTED_REPEAT_BREAK_ELEVEN,
        11.0,
    );
    expect_number("nested_repeat_break", NESTED_REPEAT_BREAK, 1111.0);
    expect_number(
        "nested_repeat_break_dispatch",
        NESTED_REPEAT_BREAK_DISPATCH,
        1111.0,
    );
    let direct = lowered(NESTED_REPEAT_BREAK);
    let dispatch = lowered(NESTED_REPEAT_BREAK_DISPATCH);
    assert_repeat_fors(&direct, 2);
    assert_repeat_fors(&dispatch, 2);
    assert_eq!(source_while_count(&direct.stmts, None), 0);
    let state = slot_named(&dispatch, "$lua_dispatch_state").expect("dispatch state");
    assert_eq!(dispatch_while_count(&dispatch.stmts, state), 1);
    assert_eq!(source_while_count(&dispatch.stmts, Some(state)), 0);
}

#[test]
fn repeat_continue_reaches_until_instead_of_skipping_or_exiting() {
    // 2132: continue on a true until stops, continue on a false until loops.
    // 3232 means continue skipped until and took an extra round. 2110 means
    // continue exited before a false until. Dispatch adds no ten-thousands
    // digit when the outer goto is taken.
    expect_number("repeat_continue_pair", REPEAT_CONTINUE_PAIR, 2132.0);
    expect_number(
        "repeat_continue_pair_dispatch",
        REPEAT_CONTINUE_PAIR_DISPATCH,
        2132.0,
    );
    let direct = lowered(REPEAT_CONTINUE_PAIR);
    let dispatch = lowered(REPEAT_CONTINUE_PAIR_DISPATCH);
    assert_repeat_fors(&direct, 2);
    assert_repeat_fors(&dispatch, 2);
    assert!(slot_named(&direct, "$lua_dispatch_state").is_none());
    let state = slot_named(&dispatch, "$lua_dispatch_state").expect("dispatch state");
    assert_eq!(dispatch_while_count(&dispatch.stmts, state), 1);
}

#[test]
fn nested_while_and_repeat_keep_transfers_on_the_nearest_loop() {
    // 12211: inner while continue does not consume the outer repeat continue.
    // 121: repeat continue inside while does not restart the while.
    expect_number("nested_while_in_repeat", NESTED_WHILE_IN_REPEAT, 12211.0);
    expect_number("repeat_in_while", REPEAT_IN_WHILE, 121.0);
    let outer = lowered(NESTED_WHILE_IN_REPEAT);
    let inner = lowered(REPEAT_IN_WHILE);
    assert!(slot_named(&outer, "$lua_dispatch_state").is_none());
    assert!(slot_named(&inner, "$lua_dispatch_state").is_none());
    assert_repeat_fors(&outer, 1);
    assert_repeat_fors(&inner, 1);
    assert_eq!(source_while_count(&outer.stmts, None), 1);
    assert_eq!(source_while_count(&inner.stmts, None), 1);
}

const NUMERIC_FOR_UP_AND_EMPTY: &str = "\
local sum = 0
for i = 1, 3 do
  sum = sum * 10 + i
end
for i = 3, 1 do
  sum = -1
end
return sum
";

#[test]
fn numeric_for_counts_up_and_skips_an_empty_range() {
    // 123 is 1 then 2 then 3. The descending header with the default step
    // must not enter, so a wrong direction or a copied body cannot yield 123.
    expect_numeric("numeric_for_up_empty", NUMERIC_FOR_UP_AND_EMPTY, 123.0);
    let ir = lowered_numeric(NUMERIC_FOR_UP_AND_EMPTY);
    assert_eq!(numeric_for_count(&ir), 2);
    assert_eq!(default_step_assigns(&ir), 2);
}

const NUMERIC_FOR_DOWN: &str = "\
local sum = 0
for i = 3, 1, -1 do
  sum = sum * 10 + i
end
return sum
";

const NUMERIC_FOR_FRACTION: &str = "\
local code = 0
for i = 0.5, 1.5, 0.5 do
  code = code * 10 + i * 2
end
return code
";

const NUMERIC_FOR_HEADER: &str = "\
local i = 7
local limit = 1
local step = -2
local packed = 0
local effects = 0
local n = 0
for i = i, limit, step do
  n = n + 1
  packed = packed * 10 + i
  if n == 2 then
    continue
  end
  i = 99
  limit = 100
  step = 100
  effects = effects + 1
end
return i * 1000000 + packed * 10 + effects
";

const NUMERIC_FOR_HEADER_DISPATCH: &str = "\
local skipped = 0
goto enter
skipped = 1
::enter::
local i = 7
local limit = 1
local step = -2
local packed = 0
local effects = 0
local n = 0
for i = i, limit, step do
  n = n + 1
  packed = packed * 10 + i
  if n == 2 then
    continue
  end
  i = 99
  limit = 100
  step = 100
  effects = effects + 1
end
return skipped * 10000000 + i * 1000000 + packed * 10 + effects
";

const NUMERIC_FOR_NESTED: &str = "\
local outer = 0
local inner = 0
local after = 0
for i = 1, 3 do
  outer = outer + 1
  for j = 1, 3 do
    inner = inner + 1
    if j == 2 then
      break
    end
  end
  if i == 2 then
    continue
  end
  after = after + 1
end
return outer * 100 + inner * 10 + after
";

const NUMERIC_FOR_NESTED_DISPATCH: &str = "\
local skipped = 0
goto enter
skipped = 1
::enter::
local outer = 0
local inner = 0
local after = 0
for i = 1, 3 do
  outer = outer + 1
  for j = 1, 3 do
    inner = inner + 1
    if j == 2 then
      break
    end
  end
  if i == 2 then
    continue
  end
  after = after + 1
end
return skipped * 1000 + outer * 100 + inner * 10 + after
";

const NUMERIC_FOR_FLOATS: &str = "\
local huge = 1e308 * 10
local nnan = huge - huge
local pnan = -nnan
local neg0 = -0
local pos0 = 0
local code = 0
local n = 0
local ok = 1
n = 0
ok = 1
for i = 1.5, 2.5, pos0 do
  n = n + 1
  if i ~= 1.5 then
    ok = 0
  end
  if n == 3 then
    break
  end
end
if ok == 0 then
  n = 9
end
code = code * 10 + n
n = 0
ok = 1
for i = 1.5, 2.5, neg0 do
  n = n + 1
  if n == 3 then
    break
  end
end
if ok == 0 then
  n = 9
end
code = code * 10 + n
n = 0
ok = 1
for i = 2.5, 1.5, pos0 do
  n = n + 1
  if n == 3 then
    break
  end
end
if ok == 0 then
  n = 9
end
code = code * 10 + n
n = 0
ok = 1
for i = 2.5, 1.5, neg0 do
  n = n + 1
  if i ~= 2.5 then
    ok = 0
  end
  if n == 3 then
    break
  end
end
if ok == 0 then
  n = 9
end
code = code * 10 + n
n = 0
ok = 1
for i = 1.5, 2.5, pnan do
  n = n + 1
  if n == 1 then
    if i ~= 1.5 then
      ok = 0
    end
  end
  if n == 3 then
    break
  end
end
if ok == 0 then
  n = 9
end
code = code * 10 + n
n = 0
ok = 1
for i = 1.5, 2.5, nnan do
  n = n + 1
  if n == 3 then
    break
  end
end
if ok == 0 then
  n = 9
end
code = code * 10 + n
n = 0
ok = 1
for i = 2.5, 1.5, pnan do
  n = n + 1
  if n == 3 then
    break
  end
end
if ok == 0 then
  n = 9
end
code = code * 10 + n
n = 0
ok = 1
for i = 2.5, 1.5, nnan do
  n = n + 1
  if n == 1 then
    if i ~= 2.5 then
      ok = 0
    end
  end
  if n == 3 then
    break
  end
end
if ok == 0 then
  n = 9
end
code = code * 10 + n
n = 0
ok = 1
for i = nnan, 2.5, 1 do
  n = n + 1
  if n == 3 then
    break
  end
end
if ok == 0 then
  n = 9
end
code = code * 10 + n
n = 0
ok = 1
for i = 1.5, nnan, 1 do
  n = n + 1
  if n == 3 then
    break
  end
end
if ok == 0 then
  n = 9
end
code = code * 10 + n
n = 0
ok = 1
for i = 9007199254740992, 9007199254740996, 1 do
  n = n + 1
  if i ~= 9007199254740992 then
    ok = 0
  end
  if n == 3 then
    break
  end
end
if ok == 0 then
  n = 9
end
code = code * 10 + n
n = 0
ok = 1
for i = 1, huge, huge do
  n = n + 1
  if n == 1 then
    if i ~= 1 then
      ok = 0
    end
  end
  if n > 1 then
    if i ~= i then
      ok = 0
    end
    if not (i > 1e308) then
      ok = 0
    end
  end
  if n == 3 then
    break
  end
end
if ok == 0 then
  n = 9
end
code = code * 10 + n
return code
";

const NUMERIC_FOR_ONCE: &str = "\
local a = 1
local b = 4
local c = 1
local sum = 0
for i = a + 1, b, c do
  sum = sum + i
end
return sum
";

const NUMERIC_FOR_ONCE_DISPATCH: &str = "\
local skipped = 0
goto enter
skipped = 1
::enter::
local a = 1
local b = 4
local c = 1
local sum = 0
for i = a + 1, b, c do
  sum = sum + i
end
return skipped * 10 + sum
";

const NUMERIC_FOR_ARM_LOOP: &str = "\
  for i = base + 1, 3 do
    sum = sum * 10 + i
  end
";

const NUMERIC_FOR_ARM_DIRECT: &str = "\
local base = 0
local mark = 0
local sum = 0
local tail = 0
if true then
  for i = base + 1, 3 do
    sum = sum * 10 + i
  end
  mark = 7
end
tail = 1
return mark * 10000 + sum * 10 + tail
";

const NUMERIC_FOR_ARM_JUMP: &str = "\
local base = 0
local mark = 0
local sum = 0
local tail = 0
if true then
  for i = base + 1, 3 do
    sum = sum * 10 + i
  end
  goto leave
  mark = 7
end
tail = 1
::leave::
return mark * 10000 + sum * 10 + tail
";

const NUMERIC_FOR_BAD_STEP: &str = "\
local bad = false
local seen = 0
for i = 1, 2, bad do
  seen = 1
end
return seen
";

const NUMERIC_FOR_BAD_START: &str = "\
local bad = false
local seen = 0
for i = bad, 2, 1 do
  seen = 1
end
return seen
";

const NUMERIC_FOR_GENERIC: &str = "\
for i in 1 do
end
return 0
";

#[test]
fn numeric_for_counts_down_and_uses_a_fractional_step() {
    expect_numeric("numeric_for_down", NUMERIC_FOR_DOWN, 321.0);
    expect_numeric("numeric_for_fraction", NUMERIC_FOR_FRACTION, 123.0);
}

#[test]
fn numeric_for_header_is_a_snapshot_and_continue_still_updates() {
    // 7075313: outer i stays 7, visits are 7,5,3,1, and the second visit
    // continues past the writes. A live limit or step would not pack 7531.
    expect_numeric("numeric_for_header", NUMERIC_FOR_HEADER, 7_075_313.0);
    expect_numeric(
        "numeric_for_header_dispatch",
        NUMERIC_FOR_HEADER_DISPATCH,
        7_075_313.0,
    );
}

#[test]
fn numeric_for_break_and_continue_stay_on_the_source_loop() {
    // 362: three outer trips, two inner trips each, and the middle continue
    // skips one tail. An inner break that left the outer loop cannot pack 362.
    expect_numeric("numeric_for_nested", NUMERIC_FOR_NESTED, 362.0);
    expect_numeric(
        "numeric_for_nested_dispatch",
        NUMERIC_FOR_NESTED_DISPATCH,
        362.0,
    );
    let direct = lowered_numeric(NUMERIC_FOR_NESTED);
    let dispatch = lowered_numeric(NUMERIC_FOR_NESTED_DISPATCH);
    assert!(slot_named(&direct, "$lua_dispatch_state").is_none());
    assert_eq!(
        numeric_for_count(&direct),
        2,
        "nested source is two fors, not a copied body"
    );
    assert_eq!(
        numeric_for_count(&dispatch),
        2,
        "dispatch must not copy the numeric-for body"
    );
    let state = slot_named(&dispatch, "$lua_dispatch_state").expect("dispatch state");
    assert_eq!(dispatch_while_count(&dispatch.stmts, state), 1);
    assert_eq!(break_count(&direct), 1);
    // Dispatch keeps one unknown-state sentinel break outside the numeric fors.
    assert_eq!(break_count(&dispatch), 2);
    assert_eq!(breaks_inside_numeric_for(&direct), 1);
    assert_eq!(breaks_inside_numeric_for(&dispatch), 1);
}

#[test]
fn numeric_for_float_boundaries_match_the_locked_sign_bit() {
    // Digits, each capped by a source break so a stuck back edge is a wrong
    // digit rather than an exhausted fuel budget:
    // +0 up 3, -0 up 0, +0 down 0, -0 down 3,
    // +NaN up 1, -NaN up 0, +NaN down 0, -NaN down 1,
    // NaN start 0, NaN limit 0, 2^53 step 3, inf step 3.
    expect_numeric("numeric_for_floats", NUMERIC_FOR_FLOATS, 300_310_010_033.0);
}

#[test]
fn numeric_for_header_expression_is_materialized_once() {
    expect_numeric("numeric_for_once", NUMERIC_FOR_ONCE, 9.0);
    expect_numeric("numeric_for_once_dispatch", NUMERIC_FOR_ONCE_DISPATCH, 9.0);
    assert_header_once("direct", &lowered_numeric(NUMERIC_FOR_ONCE));
    assert_header_once("dispatch", &lowered_numeric(NUMERIC_FOR_ONCE_DISPATCH));
    let dispatch = lowered_numeric(NUMERIC_FOR_ONCE_DISPATCH);
    let state = slot_named(&dispatch, "$lua_dispatch_state").expect("dispatch state");
    assert_eq!(dispatch_while_count(&dispatch.stmts, state), 1);
    assert_eq!(numeric_for_count(&dispatch), 1);
}

#[test]
fn numeric_for_in_a_transferred_if_arm_skips_the_following_write() {
    // 71231 is the un-jumped arm: the loop packs 123, then the arm stores 7
    // and the statement after the if stores 1. 1230 is the same loop with the
    // goto after it skipping both writes. 0 means the loop was dropped.
    assert!(
        NUMERIC_FOR_ARM_DIRECT.contains(NUMERIC_FOR_ARM_LOOP)
            && NUMERIC_FOR_ARM_JUMP.contains(NUMERIC_FOR_ARM_LOOP),
        "direct and jumped sources must share the numeric loop text"
    );
    expect_numeric("numeric_for_arm_direct", NUMERIC_FOR_ARM_DIRECT, 71_231.0);
    expect_numeric("numeric_for_arm_jump", NUMERIC_FOR_ARM_JUMP, 1_230.0);
    let direct = lowered_numeric(NUMERIC_FOR_ARM_DIRECT);
    let jumped = lowered_numeric(NUMERIC_FOR_ARM_JUMP);
    assert!(
        slot_named(&direct, "$lua_dispatch_state").is_none(),
        "direct control must stay off the dispatch path"
    );
    assert_eq!(
        numeric_for_count(&direct),
        1,
        "direct arm copied the numeric for"
    );
    assert_eq!(
        numeric_for_count(&jumped),
        1,
        "transferred arm copied the numeric for"
    );
    let state = slot_named(&jumped, "$lua_dispatch_state").expect("dispatch state");
    assert_eq!(dispatch_while_count(&jumped.stmts, state), 1);
    assert!(
        numeric_for_sits_in_transferring_if_arm(&jumped.stmts, state),
        "numeric for is not in the if arm that transfers"
    );
}

#[test]
fn numeric_for_rejects_a_non_float_header_instead_of_an_empty_loop() {
    assert_numeric_header_rejected("numeric_for_bad_step", NUMERIC_FOR_BAD_STEP);
    assert_numeric_header_rejected("numeric_for_bad_start", NUMERIC_FOR_BAD_START);
}

#[test]
fn generic_for_stays_outside_this_slice() {
    let error = lowered_numeric_err(NUMERIC_FOR_GENERIC);
    assert!(
        error.contains("statement is not in this chunk control slice"),
        "{error}"
    );
}

fn lowered_numeric_err(source: &str) -> String {
    let resolver = HostCallResolver::new(holds_catalog());
    control::lower_source_with(source, Some(&resolver))
        .expect_err("generic for must not lower")
        .to_string()
}

fn slots_with_prefix(ir: &FrontendIr, prefix: &str) -> Vec<LocalSlot> {
    ir.local_bindings
        .iter()
        .filter(|(name, _)| name.starts_with(prefix))
        .map(|(name, slot)| {
            assert!(!is_lua_ident(name), "{name} is a guest identifier");
            *slot
        })
        .collect()
}

fn default_step_assigns(ir: &FrontendIr) -> usize {
    let steps = slots_with_prefix(ir, "$lua_nfor_step_");
    let mut seen = 0;
    walk_stmts(&ir.stmts, &mut |stmt| {
        if let Stmt::Assign { index, expr, .. } = stmt
            && steps.contains(index)
            && matches!(expr, Expr::Float(value) if *value == 1.0)
        {
            seen += 1;
        }
    });
    seen
}

fn numeric_for_count(ir: &FrontendIr) -> usize {
    let mut seen = 0;
    walk_stmts(&ir.stmts, &mut |stmt| {
        if let Stmt::For { init, .. } = stmt
            && matches!(init.as_ref(), Stmt::Noop { .. })
        {
            seen += 1;
        }
    });
    seen
}

fn numeric_for_sits_in_transferring_if_arm(stmts: &[Stmt], state: LocalSlot) -> bool {
    fn is_true_condition(condition: &Expr) -> bool {
        matches!(
            condition,
            Expr::Not(outer)
                if matches!(
                    outer.as_ref(),
                    Expr::Eq(lhs, rhs)
                        if matches!(lhs.as_ref(), Expr::Bool(true))
                            && matches!(rhs.as_ref(), Expr::Bool(false))
                )
        )
    }

    fn is_numeric_for(stmt: &Stmt) -> bool {
        matches!(
            stmt,
            Stmt::For { init, .. } if matches!(init.as_ref(), Stmt::Noop { .. })
        )
    }

    fn transfers_after_for(arm: &[Stmt], state: LocalSlot) -> bool {
        let Some(at) = arm.iter().position(is_numeric_for) else {
            return false;
        };
        // The goto is the state store and Continue immediately after the for.
        // A later unreachable store is not part of this shape.
        matches!(
            arm.get(at + 1),
            Some(Stmt::Assign {
                index,
                expr: Expr::Int(target),
                ..
            }) if *index == state && *target != 0
        ) && matches!(arm.get(at + 2), Some(Stmt::Continue { .. }))
    }

    fn walk(stmts: &[Stmt], state: LocalSlot, in_dispatch: bool) -> bool {
        for stmt in stmts {
            match stmt {
                Stmt::While {
                    condition, body, ..
                } => {
                    let nested = in_dispatch || is_dispatch_state_while(condition, state);
                    if walk(body, state, nested) {
                        return true;
                    }
                }
                Stmt::IfElse {
                    condition,
                    then_branch,
                    else_branch,
                    ..
                } => {
                    if in_dispatch
                        && is_true_condition(condition)
                        && transfers_after_for(then_branch, state)
                    {
                        return true;
                    }
                    if walk(then_branch, state, in_dispatch)
                        || walk(else_branch, state, in_dispatch)
                    {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }

    walk(stmts, state, false)
}

fn break_count(ir: &FrontendIr) -> usize {
    break_count_stmts(&ir.stmts)
}

fn breaks_inside_numeric_for(ir: &FrontendIr) -> usize {
    fn walk(stmts: &[Stmt], inside: bool) -> usize {
        let mut seen = 0;
        for stmt in stmts {
            match stmt {
                Stmt::Break { .. } if inside => seen += 1,
                Stmt::For {
                    init, post, body, ..
                } => {
                    let numeric = matches!(init.as_ref(), Stmt::Noop { .. });
                    seen += walk(std::slice::from_ref(init.as_ref()), inside);
                    seen += walk(std::slice::from_ref(post.as_ref()), inside);
                    seen += walk(body, inside || numeric);
                }
                Stmt::While { body, .. } => seen += walk(body, inside),
                Stmt::IfElse {
                    then_branch,
                    else_branch,
                    ..
                } => {
                    seen += walk(then_branch, inside);
                    seen += walk(else_branch, inside);
                }
                _ => {}
            }
        }
        seen
    }
    walk(&ir.stmts, false)
}

fn break_count_stmts(stmts: &[Stmt]) -> usize {
    let mut seen = 0;
    walk_stmts(stmts, &mut |stmt| {
        if matches!(stmt, Stmt::Break { .. }) {
            seen += 1;
        }
    });
    seen
}

fn assert_header_once(label: &str, ir: &FrontendIr) {
    let indexes = slots_with_prefix(ir, "$lua_nfor_index_");
    let limits = slots_with_prefix(ir, "$lua_nfor_limit_");
    let steps = slots_with_prefix(ir, "$lua_nfor_step_");
    assert_eq!(indexes.len(), 1, "{label}: one hidden counter");
    assert_eq!(limits.len(), 1, "{label}: one cached limit");
    assert_eq!(steps.len(), 1, "{label}: one cached step");
    let index = indexes[0];
    let limit = limits[0];
    let step = steps[0];
    let start = header_start_slot(ir, index).unwrap_or_else(|| {
        panic!(
            "{label}: hidden counter is not assigned from a + 1; bindings={:?}",
            ir.local_bindings
        )
    });
    let mut start_adds = 0;
    let mut condition_calls = 0;
    let mut post_adds = 0;
    walk_stmts(&ir.stmts, &mut |stmt| {
        if let Stmt::Assign {
            index: slot, expr, ..
        } = stmt
            && *slot == index
            && is_start_add(expr, start)
        {
            start_adds += 1;
        }
        if let Stmt::For {
            condition, post, ..
        } = stmt
        {
            if is_holds_call(condition, index, limit, step) {
                condition_calls += 1;
            }
            if let Stmt::Assign {
                index: slot, expr, ..
            } = post.as_ref()
                && *slot == index
                && is_slot_add(expr, index, step)
            {
                post_adds += 1;
            }
        }
    });
    assert_eq!(start_adds, 1, "{label}: header start must be assigned once");
    assert_eq!(
        condition_calls, 1,
        "{label}: one predicate call, not a copied loop"
    );
    assert_eq!(
        post_adds, 1,
        "{label}: post adds the saved step, not the source step"
    );
    assert!(
        !for_interior_has_start_add(&ir.stmts, start),
        "{label}: start expression leaked into the condition, post, or body"
    );
    assert_eq!(
        ir.functions.len(),
        1,
        "{label}: one predicate declaration, functions={:?}",
        ir.functions
    );
    assert_eq!(ir.functions[0].name, control::NUMERIC_FOR_HOLDS);
}

fn header_start_slot(ir: &FrontendIr, index: LocalSlot) -> Option<LocalSlot> {
    let mut found = None;
    walk_stmts(&ir.stmts, &mut |stmt| {
        if let Stmt::Assign {
            index: slot,
            expr: Expr::Add(lhs, rhs),
            ..
        } = stmt
            && *slot == index
            && matches!(rhs.as_ref(), Expr::Float(value) if *value == 1.0)
            && let Expr::Var(start) = lhs.as_ref()
        {
            found = Some(*start);
        }
    });
    found
}

fn is_start_add(expr: &Expr, start: LocalSlot) -> bool {
    matches!(
        expr,
        Expr::Add(lhs, rhs)
            if matches!(lhs.as_ref(), Expr::Var(slot) if *slot == start)
                && matches!(rhs.as_ref(), Expr::Float(value) if *value == 1.0)
    )
}

fn is_slot_add(expr: &Expr, left: LocalSlot, right: LocalSlot) -> bool {
    matches!(
        expr,
        Expr::Add(lhs, rhs)
            if matches!(lhs.as_ref(), Expr::Var(slot) if *slot == left)
                && matches!(rhs.as_ref(), Expr::Var(slot) if *slot == right)
    )
}

fn is_holds_call(expr: &Expr, index: LocalSlot, limit: LocalSlot, step: LocalSlot) -> bool {
    matches!(
        expr,
        Expr::Call(_, _, args, Some(_), None)
            if matches!(
                args.as_slice(),
                [
                    Expr::Var(got_index),
                    Expr::Var(got_limit),
                    Expr::Var(got_step),
                ] if *got_index == index && *got_limit == limit && *got_step == step
            )
    )
}

fn for_interior_has_start_add(stmts: &[Stmt], start: LocalSlot) -> bool {
    let mut found = false;
    walk_stmts(stmts, &mut |stmt| {
        if let Stmt::For {
            condition,
            post,
            body,
            ..
        } = stmt
        {
            found |= expr_contains_start_add(condition, start);
            found |= stmt_contains_start_add(post, start);
            found |= stmts_contain_start_add(body, start);
        }
    });
    found
}

fn stmts_contain_start_add(stmts: &[Stmt], start: LocalSlot) -> bool {
    stmts.iter().any(|stmt| match stmt {
        Stmt::Assign { expr, .. } | Stmt::Let { expr, .. } => expr_contains_start_add(expr, start),
        Stmt::For {
            condition,
            post,
            body,
            ..
        } => {
            expr_contains_start_add(condition, start)
                || stmt_contains_start_add(post, start)
                || stmts_contain_start_add(body, start)
        }
        Stmt::IfElse {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            expr_contains_start_add(condition, start)
                || stmts_contain_start_add(then_branch, start)
                || stmts_contain_start_add(else_branch, start)
        }
        Stmt::While {
            condition, body, ..
        } => expr_contains_start_add(condition, start) || stmts_contain_start_add(body, start),
        _ => false,
    })
}

fn stmt_contains_start_add(stmt: &Stmt, start: LocalSlot) -> bool {
    match stmt {
        Stmt::Assign { expr, .. } | Stmt::Let { expr, .. } => expr_contains_start_add(expr, start),
        _ => false,
    }
}

fn expr_contains_start_add(expr: &Expr, start: LocalSlot) -> bool {
    if is_start_add(expr, start) {
        return true;
    }
    match expr {
        Expr::Add(lhs, rhs)
        | Expr::Sub(lhs, rhs)
        | Expr::Mul(lhs, rhs)
        | Expr::Div(lhs, rhs)
        | Expr::Eq(lhs, rhs)
        | Expr::Gt(lhs, rhs) => {
            expr_contains_start_add(lhs, start) || expr_contains_start_add(rhs, start)
        }
        Expr::Not(inner) => expr_contains_start_add(inner, start),
        Expr::Call(_, _, args, _, _) => args.iter().any(|arg| expr_contains_start_add(arg, start)),
        _ => false,
    }
}

fn assert_numeric_header_rejected(label: &str, source: &str) {
    let path = write_fixture(label, source);
    let compiled = compile_source_file_with_options(&path, numeric_options());
    let compiled = match compiled {
        Err(error) => {
            let text = error.to_string();
            assert!(
                !text.contains("not in this chunk control slice"),
                "{label}: non-float header was rejected as unsupported syntax: {text}"
            );
            assert!(
                text.to_ascii_lowercase().contains("float")
                    || text.to_ascii_lowercase().contains("type"),
                "{label}: non-float header must fail closed, not as an empty loop: {text}"
            );
            return;
        }
        Ok(compiled) => compiled,
    };
    let mut vm = Vm::new_with_jit_config(
        compiled.program,
        JitConfig {
            enabled: false,
            hot_loop_threshold: 8,
            max_trace_len: 256,
        },
    );
    bind_numeric_for_holds(&mut vm)
        .unwrap_or_else(|error| panic!("{label}: bind numeric for predicate: {error}"));
    vm.set_fuel_check_interval(FUEL_CHECK_INTERVAL)
        .unwrap_or_else(|error| panic!("{label}: fuel interval: {error}"));
    vm.set_fuel(FUEL_BUDGET);
    match vm.run() {
        Ok(status) => panic!(
            "{label}: non-float header halted ({status:?}) with stack {:?}; an empty loop would be 0",
            vm.stack()
        ),
        Err(error) => {
            let text = error.to_string();
            assert!(
                text.to_ascii_lowercase().contains("float")
                    || text.to_ascii_lowercase().contains("type")
                    || text.contains("HostError"),
                "{label}: expected a float header failure, got {text}"
            );
        }
    }
}

#[test]
fn repeat_continue_past_a_local_is_rejected_at_load() {
    let error = control::lower_source(REPEAT_SKIP_LOCAL).expect_err("resolver rejection");
    let text = error.to_string();
    assert!(
        text.contains("jump into the scope of local skipped"),
        "{text}"
    );
    let path = write_fixture("repeat_skip_local", REPEAT_SKIP_LOCAL);
    let compiled = compile_source_file_with_options(&path, candidate_options());
    assert!(
        compiled.is_err(),
        "continue past a local must not produce a program"
    );
    let Ok(oracle) = std::env::var("LUAJIT_ORACLE") else {
        eprintln!("LUAJIT_ORACLE is unset; repeat skip-local is not a differential result");
        return;
    };
    let output = oracle_load_rejection(&oracle, &path);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let code = output.status.code();
    assert_eq!(
        code,
        Some(2),
        "text-mode load rejection is exit 2; a process failure has a different status={} stderr={stderr}",
        output.status
    );
    assert!(
        stderr.contains("LOAD_REJECT"),
        "load rejection marker missing; this is not a full-text match: {stderr}"
    );
}

fn private_binding_count(ir: &FrontendIr, name: &str) -> usize {
    ir.local_bindings
        .iter()
        .filter(|(binding, _)| binding == name)
        .count()
}

fn assert_one_dispatch_and_result(ir: &FrontendIr) -> LocalSlot {
    assert_eq!(
        private_binding_count(ir, "$lua_dispatch_state"),
        1,
        "dispatch state was duplicated: {:?}",
        ir.local_bindings
    );
    assert_eq!(
        private_binding_count(ir, "$lua_return_value"),
        1,
        "return result was duplicated: {:?}",
        ir.local_bindings
    );
    let state = slot_named(ir, "$lua_dispatch_state").expect("dispatch state");
    assert_eq!(
        dispatch_while_count(&ir.stmts, state),
        1,
        "source return must reuse the one dispatch loop"
    );
    state
}

// 12 is the taken return n + 10. 4 is the finished loop's tail return.
// 2 would mean the add was dropped. An open back-edge cannot hide in 12:
// the condition stops at 4.
const WHILE_RETURN_HIT: &str = "\
local n = 0
while n < 4 do
  n = n + 1
  if n == 2 then
    return n + 10
  end
end
return n
";

const WHILE_RETURN_MISS: &str = "\
local n = 0
while n < 4 do
  n = n + 1
  if n == 9 then
    return n + 10
  end
end
return n
";

#[test]
fn while_return_leaves_the_chunk_and_a_false_condition_continues() {
    expect_number("while_return_hit", WHILE_RETURN_HIT, 12.0);
    expect_number("while_return_miss", WHILE_RETURN_MISS, 4.0);
    let hit = lowered(WHILE_RETURN_HIT);
    let state = assert_one_dispatch_and_result(&hit);
    assert_eq!(
        source_while_count(&hit.stmts, Some(state)),
        1,
        "while return copied the source loop"
    );
    let miss = lowered(WHILE_RETURN_MISS);
    let miss_state = assert_one_dispatch_and_result(&miss);
    assert_eq!(source_while_count(&miss.stmts, Some(miss_state)), 1);
}

// 21 is n + 20 on the first body. 1 is the tail return after leaving the
// repeat without using that expression. until false repeats forever if the
// return continues this loop; fuel exhaustion is that back-edge, not a value.
// 7 is two finished trips plus the tail add, with the return condition false.
const REPEAT_RETURN_HIT: &str = "\
local n = 0
repeat
  n = n + 1
  if n == 1 then
    return n + 20
  end
until false
return n
";

const REPEAT_RETURN_MISS: &str = "\
local n = 0
repeat
  n = n + 1
  if n == 9 then
    return n + 20
  end
until n == 2
return n + 5
";

#[test]
fn repeat_return_leaves_before_until_false_and_a_finished_repeat_continues() {
    expect_number("repeat_return_hit", REPEAT_RETURN_HIT, 21.0);
    expect_number("repeat_return_miss", REPEAT_RETURN_MISS, 7.0);
    let hit = lowered(REPEAT_RETURN_HIT);
    assert_one_dispatch_and_result(&hit);
    assert_repeat_fors(&hit, 1);
    let miss = lowered(REPEAT_RETURN_MISS);
    assert_one_dispatch_and_result(&miss);
    assert_repeat_fors(&miss, 1);
}

// 33 is s + 30 after i = 1 then i = 2 (s = 3). 10 is the finished 1+2+3+4.
// 3 would drop the add. A step after the return would reach the tail 10.
const NUMERIC_FOR_RETURN_HIT: &str = "\
local s = 0
for i = 1, 4 do
  s = s + i
  if i == 2 then
    return s + 30
  end
end
return s
";

const NUMERIC_FOR_RETURN_MISS: &str = "\
local s = 0
for i = 1, 4 do
  s = s + i
  if i == 9 then
    return s + 30
  end
end
return s
";

#[test]
fn numeric_for_return_leaves_before_the_step_and_a_finished_for_continues() {
    expect_numeric("numeric_for_return_hit", NUMERIC_FOR_RETURN_HIT, 33.0);
    expect_numeric("numeric_for_return_miss", NUMERIC_FOR_RETURN_MISS, 10.0);
    let hit = lowered_numeric(NUMERIC_FOR_RETURN_HIT);
    assert_one_dispatch_and_result(&hit);
    assert_eq!(
        numeric_for_count(&hit),
        1,
        "numeric for return copied the source loop"
    );
    let miss = lowered_numeric(NUMERIC_FOR_RETURN_MISS);
    assert_one_dispatch_and_result(&miss);
    assert_eq!(numeric_for_count(&miss), 1);
}

// 42 is wave + i + 40 on the first numeric trip (1 + 1 + 40).
// 1 is the chunk tail if the while exits before its own trailing add.
// 51 is that trailing add plus the chunk tail, so the repeat body after the
// for ran. until false exhausts fuel if the numeric return only breaks the
// inner for and the repeat back-edge still runs.
const NESTED_LOOP_RETURN: &str = "\
local wave = 0
while wave < 3 do
  wave = wave + 1
  repeat
    for i = 1, 2 do
      if wave == 1 then
        if i == 1 then
          return wave + i + 40
        end
      end
    end
  until false
  wave = wave + 50
end
return wave
";

#[test]
fn inner_return_skips_outer_loop_body_and_chunk_tail() {
    expect_numeric("nested_loop_return", NESTED_LOOP_RETURN, 42.0);
    let ir = lowered_numeric(NESTED_LOOP_RETURN);
    let state = assert_one_dispatch_and_result(&ir);
    assert_eq!(
        source_while_count(&ir.stmts, Some(state)),
        1,
        "nested return copied the while"
    );
    assert_eq!(
        repeat_flag_bindings(&ir).len(),
        1,
        "nested return copied the repeat"
    );
    assert_eq!(
        numeric_for_count(&ir),
        1,
        "nested return copied the numeric for"
    );
}

// 61 is the loop return n + 60. 141 is n + 40 then goto done then n + 100,
// which is the following write replacing the return. 108 is the else goto.
// 143 is the same arm with the return condition false: the while finishes,
// the following write runs, and the external goto still reaches done.
const LOOP_RETURN_GOTO_HIT: &str = "\
local pick = 1
local n = 0
if pick == 1 then
  while n < 3 do
    n = n + 1
    if n == 1 then
      return n + 60
    end
  end
  n = n + 40
  goto done
else
  n = 8
  goto done
end
::done::
return n + 100
";

const LOOP_RETURN_GOTO_MISS: &str = "\
local pick = 1
local n = 0
if pick == 1 then
  while n < 3 do
    n = n + 1
    if n == 9 then
      return n + 60
    end
  end
  n = n + 40
  goto done
else
  n = 8
  goto done
end
::done::
return n + 100
";

#[test]
fn return_inside_dispatch_if_arm_is_not_replaced_by_the_following_goto() {
    expect_number("loop_return_goto_hit", LOOP_RETURN_GOTO_HIT, 61.0);
    expect_number("loop_return_goto_miss", LOOP_RETURN_GOTO_MISS, 143.0);
    let hit = lowered(LOOP_RETURN_GOTO_HIT);
    let state = assert_one_dispatch_and_result(&hit);
    assert_eq!(
        source_while_count(&hit.stmts, Some(state)),
        1,
        "goto arm copied the while"
    );
    let miss = lowered(LOOP_RETURN_GOTO_MISS);
    let miss_state = assert_one_dispatch_and_result(&miss);
    assert_eq!(source_while_count(&miss.stmts, Some(miss_state)), 1);
}
