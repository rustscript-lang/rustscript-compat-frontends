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

use vm::{
    Expr, FrontendImportSyntax, FrontendIr, JitConfig, LocalSlot, ParseError, SourceFlavor,
    SourcePlugin, Stmt, Value, VmStatus,
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
        control::lower_source(source)
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

fn write_fixture(label: &str, source: &str) -> PathBuf {
    let root = namespace_case_root(&format!("lua_control_{label}"));
    let path = root.join("chunk.lua");
    fs::write(&path, source).unwrap_or_else(|error| panic!("{label}: write fixture: {error}"));
    path
}

fn run_fixture(label: &str, source: &str) -> (Vec<Value>, u64) {
    let path = write_fixture(label, source);
    let compiled = compile_source_file_with_options(&path, candidate_options())
        .unwrap_or_else(|error| panic!("{label}: compile failed: {error}"));
    let mut vm = Vm::new_with_jit_config(
        compiled.program,
        JitConfig {
            enabled: false,
            hot_loop_threshold: 8,
            max_trace_len: 256,
        },
    );
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
