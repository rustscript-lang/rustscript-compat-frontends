//! Chunk scalar control flow through the real Lua parser and resolver.
//!
//! The plugin is test-only. Production `LuaPlugin` stays on the line lowerer.
//! Fixtures are legal Lua 5.2-compat chunks. A bare expression statement is
//! not used: observation is the single chunk return value on the VM stack.
//! Ordinary Cargo does not spawn LuaJIT. `LUAJIT_ORACLE` compares that same
//! file when it is set; a configured binary that cannot run is a failure.

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
        stack,
        vec![Value::Float(expected)],
        "{label}: stack must be the single chunk return, not a bare Halted"
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

fn oracle_return(oracle: &str, path: &Path) -> f64 {
    let driver = "\
local chunk, err = loadfile(arg[0])\n\
if not chunk then\n\
  io.stderr:write(err or 'load failed', '\\n')\n\
  os.exit(1)\n\
end\n\
local value = chunk()\n\
if value == nil then\n\
  io.write('nil\\n')\n\
else\n\
  io.write(string.format('%.17g\\n', value))\n\
end\n\
";
    let output = spawn_oracle(oracle, &["-e", driver, path.to_str().expect("utf-8 path")]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "LUAJIT_ORACLE at {oracle:?} failed to run {}: status={} stdout={stdout:?} stderr={stderr:?}",
        path.display(),
        output.status,
    );
    let text = stdout.trim();
    text.parse::<f64>()
        .unwrap_or_else(|error| panic!("LUAJIT_ORACLE returned {text:?}: {error}"))
}

fn spawn_oracle(path: &str, args: &[&str]) -> std::process::Output {
    let mut cmd = Command::new(path);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "linux")]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(limit_oracle_child);
    }
    cmd.output()
        .unwrap_or_else(|error| panic!("LUAJIT_ORACLE at {path:?} failed to start: {error}"))
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
io.write(jit.version, '\\n')\n\
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

fn source_while_count(stmts: &[Stmt]) -> usize {
    let mut count = 0;
    walk_stmts(stmts, &mut |stmt| {
        if let Stmt::While { condition, .. } = stmt
            && matches!(condition, Expr::Lt(_, _) | Expr::Gt(_, _))
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
    assert_eq!(source_while_count(&ir.stmts), 1);
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
    assert_eq!(source_while_count(&direct.stmts), 1);
    assert_eq!(dispatch_while_count(&direct.stmts, 0), 0);
    let state = slot_named(&dispatch, "$lua_dispatch_state").expect("dispatch state");
    assert!(!is_lua_ident("$lua_dispatch_state"));
    assert_eq!(source_while_count(&dispatch.stmts), 0);
    assert_eq!(dispatch_while_count(&dispatch.stmts, state), 1);
    assert_private_slots(&direct, &["i", "n"]);
    assert_private_slots(&dispatch, &["i", "n", "loop"]);
    eprintln!(
        "LUA_CONTROL cost iters=4 result=4 direct_fuel={direct_fuel} dispatch_fuel={dispatch_fuel} direct_shape=source_while dispatch_shape=state_while jit=disabled fuel_budget={FUEL_BUDGET} check_interval={FUEL_CHECK_INTERVAL}"
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
}

#[test]
fn oracle_control_config_matches_locked_52_build() {
    let Ok(path) = std::env::var("LUAJIT_ORACLE") else {
        eprintln!("LUAJIT_ORACLE is unset; this is not a differential GREEN");
        return;
    };
    let output = spawn_oracle(&path, &["-e", BASELINE_DRIVER]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "LUAJIT_ORACLE at {path:?} baseline failed: status={} stdout={stdout:?} stderr={stderr:?}",
        output.status
    );
    let expected = format!("{ORACLE_VERSION_BASELINE}\nGOTO_KW\nBREAK_OK\nfunction\n");
    assert_eq!(
        stdout, expected,
        "oracle is not the locked 5.2-compat build"
    );
    eprintln!("oracle baseline {ORACLE_VERSION_BASELINE} GOTO_KW BREAK_OK table.pack");
}
