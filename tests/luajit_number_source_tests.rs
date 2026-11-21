//! Standard Lua numeric fragments through the source seam.
//!
//! The trial plugin lives here. It is not a product entry and it does not
//! register hosts. Each executed sample is real Lua source, lowered by the
//! seam, then compiled with `SourcePlugin` and run on the VM.

// This test compiles the production AST through a path module. Four helpers
// (`Name::is`, `ExprList::tail_multi`, `AssignTarget::span`/`has_nav`,
// `Expr::is_multi`) have no caller on this path. The allow stays on this
// module: it does not change the production AST and it is not a crate lint.
#[allow(dead_code)]
#[path = "../src/frontends/lua/ast.rs"]
mod ast;
#[path = "../src/frontends/lua/lexer.rs"]
mod lexer;
#[path = "../src/frontends/lua/number.rs"]
mod number;
#[path = "../src/frontends/lua/number_source.rs"]
mod number_source;
#[path = "../src/frontends/lua/parser.rs"]
mod parser;
#[path = "../src/frontends/lua/resolve.rs"]
mod resolve;

use std::io::Write;
use std::process::{Command, Stdio};

use number_source::NumberSourceError;
use vm::{
    CompileSourceFileOptions, FrontendImportSyntax, FrontendIr, ParseError, SourceFlavor,
    SourcePlugin, Value, Vm, VmStatus,
};

const ORACLE_VERSION: &str = "LuaJIT 2.1.1795119671";

struct ExecuteCase {
    name: &'static str,
    source: &'static str,
    category: Category,
    expect: Expect,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Category {
    Finite,
    SignedZero,
    NonFinite,
    Empty,
}

enum Expect {
    Bits(u64),
    Class(&'static str),
    Empty,
}

struct NegativeCase {
    name: &'static str,
    source: &'static str,
    layer: RejectLayer,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RejectLayer {
    Syntax,
    NotFloat(ast::NumberKind),
    Unsupported(number_source::UnsupportedReason),
}

#[test]
fn hex_float_decoder_keeps_nonzero_subnormal_bits() {
    let min_positive = parse_lua_hex_float("0x1p-1074");
    assert_eq!(
        min_positive.to_bits(),
        0x0000_0000_0000_0001,
        "min positive subnormal was misdecoded as {min_positive:?}"
    );
    assert!(min_positive.is_finite() && min_positive != 0.0);
    let next = parse_lua_hex_float("0x1p-1073");
    assert_eq!(next.to_bits(), 0x0000_0000_0000_0002);
    let negative = parse_lua_hex_float("-0x1p-1074");
    assert_eq!(negative.to_bits(), 0x8000_0000_0000_0001);
    assert!(negative.is_finite() && negative.is_sign_negative() && negative != 0.0);
    let tenth = parse_lua_hex_float("0x1.999999999999ap-4");
    assert_eq!(tenth.to_bits(), 0x3fb9_9999_9999_999a);
    assert_eq!(
        parse_lua_hex_float("0x1.cp+1").to_bits(),
        0x400c_0000_0000_0000
    );
    assert_eq!(
        parse_lua_hex_float("-0x1p+1").to_bits(),
        0xc000_0000_0000_0000
    );
    assert!(matches!(
        parse_oracle_value("nan"),
        OracleNumber::Class("nan")
    ));
    assert!(matches!(
        parse_oracle_value("inf"),
        OracleNumber::Class("inf")
    ));
    assert!(matches!(
        parse_oracle_value("-inf"),
        OracleNumber::Class("-inf")
    ));
    assert!(matches!(
        parse_oracle_value("-0"),
        OracleNumber::Bits(bits) if bits == (-0.0f64).to_bits()
    ));
    assert!(matches!(
        parse_oracle_value("+0"),
        OracleNumber::Bits(bits) if bits == 0.0f64.to_bits()
    ));
}

#[test]
fn proven_binary64_sources_run_through_the_plugin() {
    let cases = execute_cases();
    assert_hex_expectations_differ(&cases);
    let mut ran = 0usize;
    let mut failed = Vec::new();
    for case in &cases {
        let outcome = run_plugin(case.source);
        let mut failed_this = false;
        match (&case.expect, outcome) {
            (Expect::Empty, Ok(run)) => {
                assert!(
                    run.stack.is_empty(),
                    "{} should return no values, stack={:?}",
                    case.name,
                    run.stack
                );
            }
            (Expect::Bits(bits), Ok(run)) => {
                assert_eq!(run.stack.len(), 1, "{}", case.name);
                assert_bits(&run.stack[0], *bits, case.name);
                if case.name.starts_with("hex-") {
                    println!("hex {} bits={bits:#018x}", case.name);
                }
            }
            (Expect::Class(class), Ok(run)) => {
                assert_eq!(run.stack.len(), 1, "{}", case.name);
                assert_class(&run.stack[0], class, case.name);
                if case.name.starts_with("hex-") {
                    println!("hex {} class={class}", case.name);
                }
            }
            (_, Err(failure)) => {
                let note = if failure.layer == "syntax" {
                    "; syntax rejection is not a lowering RED"
                } else {
                    ""
                };
                failed.push(format!(
                    "{} at {}: {} expect={}{note}",
                    case.name,
                    failure.layer,
                    failure.detail,
                    expect_label(&case.expect)
                ));
                failed_this = true;
            }
        }
        if !failed_this {
            ran += 1;
        }
    }
    assert!(
        failed.is_empty(),
        "execute failures ({} of {}):\n{}",
        failed.len(),
        cases.len(),
        failed.join("\n")
    );
    assert_eq!(ran, cases.len());
    println!("execute cases={ran}");
}

#[test]
fn legal_uncovered_sources_are_not_syntax_rejections() {
    let cases = negative_cases();
    let mut syntax = 0usize;
    let mut not_float = 0usize;
    let mut unsupported = 0usize;
    for case in &cases {
        let failure = run_plugin(case.source).expect_err(case.name);
        let layer = reject_layer(case.source);
        assert_eq!(&layer, &case.layer, "{}", case.name);
        match &layer {
            RejectLayer::Syntax => {
                assert_eq!(failure.layer, "syntax", "{}", case.name);
                syntax += 1;
            }
            RejectLayer::NotFloat(_) => {
                assert_eq!(failure.layer, "classify-not-float", "{}", case.name);
                not_float += 1;
            }
            RejectLayer::Unsupported(_) => {
                assert_eq!(failure.layer, "compile-unsupported", "{}", case.name);
                unsupported += 1;
            }
        }
    }
    let syntax_names: Vec<_> = cases
        .iter()
        .filter(|case| matches!(case.layer, RejectLayer::Syntax))
        .map(|case| case.name)
        .collect();
    let not_float_names: Vec<_> = cases
        .iter()
        .filter(|case| matches!(case.layer, RejectLayer::NotFloat(_)))
        .map(|case| case.name)
        .collect();
    println!(
        "negative cases={} syntax={syntax} classify-not-float={not_float} compile-unsupported={unsupported}",
        cases.len()
    );
    println!("syntax-reject names={syntax_names:?}");
    println!("not-float names={not_float_names:?}");
    assert_eq!(syntax, 9);
    assert_eq!(not_float, 3);
    assert_eq!(unsupported, 7);
}

fn reject_layer(source: &str) -> RejectLayer {
    match parser::parse_chunk(source.as_bytes()) {
        Err(_) => RejectLayer::Syntax,
        Ok(chunk) => {
            let resolution = resolve::resolve(&chunk);
            match number_source::lower_proven_chunk(source, &chunk, &resolution) {
                Err(NumberSourceError::Syntax { .. }) => RejectLayer::Syntax,
                Err(NumberSourceError::NotFloat { kind }) => RejectLayer::NotFloat(kind),
                Err(NumberSourceError::CompileUnsupported { reason }) => {
                    RejectLayer::Unsupported(reason)
                }
                Ok(_) => panic!("lowered a negative source: {source}"),
            }
        }
    }
}

#[test]
fn sequential_mods_use_core_slots_and_keep_the_result() {
    let source = sequential_mod_source(12);
    let lowered = lower_source(&source).expect("sequential mods are proven floats");
    let run = run_plugin(&source).expect("sequential mods compile and run");
    assert_bits(&run.stack[0], 1.0f64.to_bits(), "10 % 3");
    let guest_bindings = lowered
        .binding_names
        .iter()
        .filter(|name| !name.starts_with("$lua.number."))
        .count();
    let temps = lowered
        .binding_names
        .iter()
        .filter(|name| name.starts_with("$lua.number."))
        .count();
    assert_eq!(lowered.logical_slots, 36, "logical slots");
    assert_eq!(guest_bindings, 12, "guest bindings r0..r11");
    assert_eq!(temps, 24, "two temps per remainder");
    assert_eq!(guest_bindings + temps, lowered.logical_slots);
    assert_eq!(run.frame_slots, 3, "Core frame slots after compaction");
    let depth = expr_depth_of(&lowered.ir.stmts);
    println!(
        "slot boundary name_probes={} temps={} mod_lhs={} mod_rhs={} value={} logical_slots={} frame_slots={} expr_depth={}; {}→{}",
        lowered.name_probes,
        temps,
        count_role(&lowered.binding_names, "mod_lhs"),
        count_role(&lowered.binding_names, "mod_rhs"),
        count_role(&lowered.binding_names, "value"),
        lowered.logical_slots,
        run.frame_slots,
        depth,
        lowered.logical_slots,
        run.frame_slots
    );
    assert!(
        depth < 32,
        "short remainder sample expr depth {depth} is not bounded"
    );
    assert_eq!(
        lowered.name_probes, temps as u64,
        "name probes rescanned occupied ordinals from 0"
    );
    assert!(
        lowered
            .binding_names
            .iter()
            .filter(|name| name.starts_with("$lua.number."))
            .all(|name| !is_lua_identifier(name)),
        "number temps must stay outside the Lua identifier grammar: {:?}",
        lowered.binding_names
    );
    println!(
        "slot boundary logical_slots={} guest_bindings={} temps={} frame_slots={}; {}→{}",
        lowered.logical_slots,
        guest_bindings,
        temps,
        run.frame_slots,
        lowered.logical_slots,
        run.frame_slots
    );
    if let Ok(path) = std::env::var("LUAJIT_ORACLE") {
        let oracle = oracle_execute(&path, &source);
        assert!(oracle.accepted, "sequential mods are legal LuaJIT");
        assert_eq!(oracle.values.len(), 1, "{:?}", oracle.values);
        assert_same_number(&oracle.values[0], &run.stack[0], "sequential");
        println!("slot boundary oracle compared=1");
    }
}

#[test]
fn configured_oracle_matches_the_same_sources() {
    let Some(path) = std::env::var("LUAJIT_ORACLE").ok() else {
        println!(
            "oracle differential not run: LUAJIT_ORACLE unset; execute=0 signed-zero=0 nonfinite=0 not-float=0 unsupported=0 syntax-reject=0; not a differential GREEN"
        );
        return;
    };
    assert_oracle_baseline(&path);
    let mut execute = 0usize;
    let mut signed_zero = 0usize;
    let mut nonfinite = 0usize;
    let mut empty = 0usize;
    for case in execute_cases() {
        let ours = run_plugin(case.source).unwrap_or_else(|failure| {
            panic!("{} rust {}: {}", case.name, failure.layer, failure.detail)
        });
        let oracle = oracle_execute(&path, case.source);
        assert!(
            oracle.accepted,
            "{} LuaJIT rejected source that the seam executed",
            case.name
        );
        assert_eq!(
            oracle.values.len(),
            ours.stack.len(),
            "{} return count oracle={:?} rust={:?}",
            case.name,
            oracle.values,
            ours.stack
        );
        for (index, (oracle_value, rust_value)) in
            oracle.values.iter().zip(ours.stack.iter()).enumerate()
        {
            assert_same_number(oracle_value, rust_value, &format!("{}[{index}]", case.name));
        }
        match case.category {
            Category::Finite => execute += 1,
            Category::SignedZero => signed_zero += 1,
            Category::NonFinite => nonfinite += 1,
            Category::Empty => empty += 1,
        }
    }
    let mut not_float = 0usize;
    let mut unsupported = 0usize;
    let mut syntax_reject = 0usize;
    for case in negative_cases() {
        let oracle_accepted = oracle_load(&path, case.source);
        let failure = run_plugin(case.source).expect_err(case.name);
        match &case.layer {
            RejectLayer::Syntax => {
                assert_eq!(failure.layer, "syntax", "{}", case.name);
                assert!(
                    !oracle_accepted,
                    "{} should be a LuaJIT source rejection",
                    case.name
                );
                syntax_reject += 1;
            }
            RejectLayer::NotFloat(_) => {
                assert_eq!(failure.layer, "classify-not-float", "{}", case.name);
                let executed = oracle_execute(&path, case.source);
                assert!(
                    executed.accepted,
                    "{} is legal LuaJIT and must be executed for type()",
                    case.name
                );
                assert_eq!(
                    executed.values.len(),
                    1,
                    "{} {:?}",
                    case.name,
                    executed.values
                );
                match &executed.values[0] {
                    OracleNumber::Type(ty) => {
                        assert_eq!(ty, "cdata", "{} type category", case.name);
                    }
                    other => panic!("{} expected cdata type, got {other:?}", case.name),
                }
                not_float += 1;
            }
            RejectLayer::Unsupported(_) => {
                assert_eq!(failure.layer, "compile-unsupported", "{}", case.name);
                assert!(oracle_accepted, "{} is legal LuaJIT syntax", case.name);
                unsupported += 1;
            }
        }
    }
    println!(
        "oracle differential execute={execute} signed-zero={signed_zero} nonfinite={nonfinite} empty={empty} not-float={not_float} compile-unsupported={unsupported} syntax-reject={syntax_reject}"
    );
    assert_eq!(execute, 51);
    assert_eq!(signed_zero, 8);
    assert_eq!(nonfinite, 7);
    assert_eq!(empty, 1);
    assert_eq!(not_float, 3);
    assert_eq!(unsupported, 7);
    assert_eq!(syntax_reject, 9);
}

#[test]
fn flat_remainder_chain_probes_once_per_temp_and_matches_oracle() {
    let short = measure_source(&sequential_mod_source(12));
    assert_eq!(
        short.logical_slots, 36,
        "12-remainder control logical slots"
    );
    assert_eq!(short.frame_slots, 3, "12-remainder control frame slots");
    println!(
        "control 36→3 name_probes={} temps={} logical_slots={} frame_slots={}",
        short.name_probes, short.temps, short.logical_slots, short.frame_slots
    );

    let nested = measure_source("return 7%(10%4)\n");
    assert_eq!(nested.stack_bits, Some(1.0f64.to_bits()), "nested 7%(10%4)");
    println!(
        "nested name_probes={} temps={} mod_lhs={} mod_rhs={} value={} logical_slots={} frame_slots={} expr_depth={}",
        nested.name_probes,
        nested.temps,
        nested.mod_lhs,
        nested.mod_rhs,
        nested.value_temps,
        nested.logical_slots,
        nested.frame_slots,
        nested.expr_depth
    );

    let chain_a = measure_source(&flat_mod_chain(100, 7, 128));
    let chain_b = measure_source(&flat_mod_chain(101, 7, 128));
    println!(
        "flat chain mods=128 name_probes={} temps={} mod_lhs={} mod_rhs={} value={} logical_slots={} frame_slots={} expr_depth={} bits_a={:?} bits_b={:?}",
        chain_a.name_probes,
        chain_a.temps,
        chain_a.mod_lhs,
        chain_a.mod_rhs,
        chain_a.value_temps,
        chain_a.logical_slots,
        chain_a.frame_slots,
        chain_a.expr_depth,
        chain_a.stack_bits,
        chain_b.stack_bits
    );
    assert!(
        chain_a.temps >= 128,
        "flat chain did not lower 128 remainders, temps={}",
        chain_a.temps
    );
    assert_ne!(
        chain_a.stack_bits, chain_b.stack_bits,
        "two seeds of a 128-remainder chain collapsed"
    );
    assert_ne!(chain_a.stack_bits, Some(0), "seed 100 collapsed to +0");
    assert_ne!(chain_b.stack_bits, Some(0), "seed 101 collapsed to +0");
    assert!(
        chain_a.expr_depth < 32,
        "flat chain expr depth {} grew with the source spine",
        chain_a.expr_depth
    );

    let cycle_128 = measure_source(&cycling_rebind(128));
    let cycle_127 = measure_source(&cycling_rebind(127));
    assert_ne!(
        cycle_128.stack_bits, cycle_127.stack_bits,
        "dropping one rebind step left the result unchanged"
    );
    assert_ne!(
        cycle_128.stack_bits,
        Some(0),
        "cycling chain collapsed to +0"
    );
    println!(
        "cycling rebind steps=128 name_probes={} temps={} logical_slots={} frame_slots={} expr_depth={} bits_128={:?} bits_127={:?}",
        cycle_128.name_probes,
        cycle_128.temps,
        cycle_128.logical_slots,
        cycle_128.frame_slots,
        cycle_128.expr_depth,
        cycle_128.stack_bits,
        cycle_127.stack_bits
    );

    if let Ok(path) = std::env::var("LUAJIT_ORACLE") {
        assert_oracle_baseline(&path);
        compare_oracle(&path, &flat_mod_chain(100, 7, 128), "flat-100");
        compare_oracle(&path, &flat_mod_chain(101, 7, 128), "flat-101");
        compare_oracle(&path, &cycling_rebind(128), "cycle-128");
        compare_oracle(&path, &cycling_rebind(127), "cycle-127");
        compare_oracle(&path, "return 7%(10%4)\n", "nested");
        println!("chain oracle compared=5");
    } else {
        println!(
            "chain oracle differential not run: LUAJIT_ORACLE unset; compared=0; not a differential GREEN"
        );
    }

    assert_one_probe_per_temp(&short, "12-remainder control");
    assert_one_probe_per_temp(&nested, "nested");
    assert_one_probe_per_temp(&chain_a, "flat-100");
    assert_one_probe_per_temp(&chain_b, "flat-101");
    assert_one_probe_per_temp(&cycle_128, "cycle-128");
    assert_one_probe_per_temp(&cycle_127, "cycle-127");
}

struct Measured {
    name_probes: u64,
    temps: usize,
    mod_lhs: usize,
    mod_rhs: usize,
    value_temps: usize,
    logical_slots: usize,
    frame_slots: usize,
    expr_depth: usize,
    stack_bits: Option<u64>,
}

fn measure_source(source: &str) -> Measured {
    let lowered = lower_source(source).unwrap_or_else(|error| {
        panic!("lower failed: {error}");
    });
    let run = run_plugin(source).unwrap_or_else(|failure| {
        panic!(
            "run failed at {} logical_slots={}: {}",
            failure.layer, lowered.logical_slots, failure.detail
        );
    });
    let stack_bits = match run.stack.as_slice() {
        [Value::Float(number)] => Some(number.to_bits()),
        other => panic!("expected one binary64 result, stack={other:?}"),
    };
    Measured {
        name_probes: lowered.name_probes,
        temps: count_prefix(&lowered.binding_names, "$lua.number."),
        mod_lhs: count_role(&lowered.binding_names, "mod_lhs"),
        mod_rhs: count_role(&lowered.binding_names, "mod_rhs"),
        value_temps: count_role(&lowered.binding_names, "value"),
        logical_slots: lowered.logical_slots,
        frame_slots: run.frame_slots,
        expr_depth: expr_depth_of(&lowered.ir.stmts),
        stack_bits,
    }
}

fn assert_one_probe_per_temp(measured: &Measured, label: &str) {
    assert_eq!(
        measured.name_probes, measured.temps as u64,
        "{label} name probes rescanned occupied ordinals from 0: probes={} temps={} logical_slots={} frame_slots={}",
        measured.name_probes, measured.temps, measured.logical_slots, measured.frame_slots
    );
}

fn compare_oracle(path: &str, source: &str, label: &str) {
    let ours = run_plugin(source).unwrap_or_else(|failure| {
        panic!("{label} rust {}: {}", failure.layer, failure.detail);
    });
    let oracle = oracle_execute(path, source);
    assert!(oracle.accepted, "{label} LuaJIT rejected the source");
    assert_eq!(oracle.values.len(), ours.stack.len(), "{label}");
    for (index, (oracle_value, rust_value)) in
        oracle.values.iter().zip(ours.stack.iter()).enumerate()
    {
        assert_same_number(oracle_value, rust_value, &format!("{label}[{index}]"));
    }
}

fn flat_mod_chain(seed: i64, divisor: i64, mods: usize) -> String {
    let mut source = format!("return {seed}");
    for _ in 0..mods {
        source.push_str(&format!(" % {divisor}"));
    }
    source.push('\n');
    source
}

fn cycling_rebind(steps: usize) -> String {
    let mut source = String::from("local x = 17\n");
    for _ in 0..steps {
        source.push_str("local x = (x % 5) + 3\n");
    }
    source.push_str("return x\n");
    source
}

fn count_role(names: &[String], role: &str) -> usize {
    count_prefix(names, &format!("$lua.number.{role}."))
}

fn count_prefix(names: &[String], prefix: &str) -> usize {
    names.iter().filter(|name| name.starts_with(prefix)).count()
}

fn expr_depth_of(stmts: &[vm::Stmt]) -> usize {
    stmts.iter().map(stmt_depth).max().unwrap_or(0)
}

fn stmt_depth(stmt: &vm::Stmt) -> usize {
    match stmt {
        vm::Stmt::Let { expr, .. } | vm::Stmt::Expr { expr, .. } => expr_depth(expr),
        other => panic!("number seam emitted an unexpected statement: {other:?}"),
    }
}

fn expr_depth(expr: &vm::Expr) -> usize {
    match expr {
        vm::Expr::Float(_) | vm::Expr::Int(_) | vm::Expr::Var(_) => 1,
        vm::Expr::Neg(inner) => 1 + expr_depth(inner),
        vm::Expr::Add(lhs, rhs)
        | vm::Expr::Sub(lhs, rhs)
        | vm::Expr::Mul(lhs, rhs)
        | vm::Expr::Div(lhs, rhs) => 1 + expr_depth(lhs).max(expr_depth(rhs)),
        vm::Expr::Call(_, _, args, _, _) => 1 + args.iter().map(expr_depth).max().unwrap_or(0),
        vm::Expr::Block { stmts, expr } => 1 + stmt_depth_max(stmts).max(expr_depth(expr)),
        other => panic!("number seam emitted an unexpected expression: {other:?}"),
    }
}

fn stmt_depth_max(stmts: &[vm::Stmt]) -> usize {
    stmts.iter().map(stmt_depth).max().unwrap_or(0)
}

#[derive(Debug)]
struct Run {
    stack: Vec<Value>,
    frame_slots: usize,
}

#[derive(Debug)]
struct LayerFailure {
    layer: &'static str,
    detail: String,
}

struct NumericPlugin;

static NUMERIC_PLUGIN: NumericPlugin = NumericPlugin;

impl SourcePlugin for NumericPlugin {
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
        lower_source(source).map(|lowered| lowered.ir)
    }
}

struct OwnedLowered {
    ir: FrontendIr,
    logical_slots: usize,
    binding_names: Vec<String>,
    name_probes: u64,
}

fn lower_source(source: &str) -> Result<OwnedLowered, ParseError> {
    let chunk = parser::parse_chunk(source.as_bytes())
        .map_err(|error| ParseError::new(format!("lua number syntax: {}", error.message)))?;
    let resolution = resolve::resolve(&chunk);
    let lowered =
        number_source::lower_proven_chunk(source, &chunk, &resolution).map_err(source_error)?;
    Ok(OwnedLowered {
        logical_slots: lowered.logical_slots,
        binding_names: lowered.binding_names,
        name_probes: lowered.name_probes,
        ir: lowered.ir,
    })
}

fn source_error(error: NumberSourceError) -> ParseError {
    match error {
        NumberSourceError::Syntax { message } => {
            ParseError::new(format!("lua number syntax: {message}"))
        }
        NumberSourceError::CompileUnsupported { reason } => {
            ParseError::new(format!("lua number compile unsupported: {reason:?}"))
        }
        NumberSourceError::NotFloat { kind } => {
            ParseError::new(format!("lua number not float: {kind:?}"))
        }
    }
}

fn run_plugin(source: &str) -> Result<Run, LayerFailure> {
    let compiled = vm::compile_source_with_flavor_and_options(
        source,
        SourceFlavor::Lua,
        CompileSourceFileOptions::new().with_source_plugin(&NUMERIC_PLUGIN),
    );
    let compiled = match compiled {
        Ok(compiled) => compiled,
        Err(error) => return Err(label_compile_error(&error.to_string())),
    };
    let frame_slots = compiled.program.local_count;
    let mut vm = Vm::new(compiled.program);
    match vm.run() {
        Err(error) => Err(LayerFailure {
            layer: "run",
            detail: error.to_string(),
        }),
        Ok(status) => {
            if status != VmStatus::Halted {
                return Err(LayerFailure {
                    layer: "run",
                    detail: format!("{status:?}"),
                });
            }
            Ok(Run {
                stack: vm.stack().to_vec(),
                frame_slots,
            })
        }
    }
}

fn label_compile_error(display: &str) -> LayerFailure {
    let layer = if display.contains("lua number syntax:") {
        "syntax"
    } else if display.contains("lua number not float:") {
        "classify-not-float"
    } else if display.contains("lua number compile unsupported:") {
        "compile-unsupported"
    } else {
        "compile-bind"
    };
    LayerFailure {
        layer,
        detail: display.to_string(),
    }
}

fn execute_cases() -> Vec<ExecuteCase> {
    vec![
        case("empty", "", Category::Empty, Expect::Empty),
        case(
            "div",
            "return 7/2",
            Category::Finite,
            Expect::Bits(3.5f64.to_bits()),
        ),
        case(
            "int-literal",
            "return 7",
            Category::Finite,
            Expect::Bits(7.0f64.to_bits()),
        ),
        case(
            "exponent",
            "return 1.5e1",
            Category::Finite,
            Expect::Bits(15.0f64.to_bits()),
        ),
        case(
            "leading-dot",
            "return .5",
            Category::Finite,
            Expect::Bits(0.5f64.to_bits()),
        ),
        case(
            "trailing-dot",
            "return 5.",
            Category::Finite,
            Expect::Bits(5.0f64.to_bits()),
        ),
        case(
            "hex-int",
            "return 0x10",
            Category::Finite,
            Expect::Bits(16.0f64.to_bits()),
        ),
        case(
            "binary",
            "return 0b11",
            Category::Finite,
            Expect::Bits(3.0f64.to_bits()),
        ),
        case(
            "mod-neg-pos",
            "return -5%3",
            Category::Finite,
            Expect::Bits(1.0f64.to_bits()),
        ),
        case(
            "mod-pos-neg",
            "return 5%-3",
            Category::Finite,
            Expect::Bits((-1.0f64).to_bits()),
        ),
        case(
            "mod-neg-neg",
            "return -5%-3",
            Category::Finite,
            Expect::Bits((-2.0f64).to_bits()),
        ),
        case(
            "pow-right",
            "return 2^3^2",
            Category::Finite,
            Expect::Bits(512.0f64.to_bits()),
        ),
        case(
            "neg-pow",
            "return -2^2",
            Category::Finite,
            Expect::Bits((-4.0f64).to_bits()),
        ),
        case(
            "paren-neg-pow",
            "return (-2)^2",
            Category::Finite,
            Expect::Bits(4.0f64.to_bits()),
        ),
        case(
            "neg-mul",
            "return -2*3",
            Category::Finite,
            Expect::Bits((-6.0f64).to_bits()),
        ),
        case(
            "nested-mod",
            "return 7%(10%4)",
            Category::Finite,
            Expect::Bits(1.0f64.to_bits()),
        ),
        case(
            "mixed",
            "return (7+1)/2",
            Category::Finite,
            Expect::Bits(4.0f64.to_bits()),
        ),
        case(
            "precedence",
            "return 1+2*3-4",
            Category::Finite,
            Expect::Bits(3.0f64.to_bits()),
        ),
        case(
            "add-chain",
            "return 1+2+3+4",
            Category::Finite,
            Expect::Bits(10.0f64.to_bits()),
        ),
        case(
            "local",
            "local x = 7/2\nreturn x",
            Category::Finite,
            Expect::Bits(3.5f64.to_bits()),
        ),
        case(
            "rebind-reads-old",
            "local x = 7/2\nlocal x = x + 1\nreturn x",
            Category::Finite,
            Expect::Bits(4.5f64.to_bits()),
        ),
        case(
            "rebind-commits",
            "local x = 7/2\nlocal x = x + 1\nlocal x = x + 1\nreturn x",
            Category::Finite,
            Expect::Bits(5.5f64.to_bits()),
        ),
        case(
            "rebind-keeps-other-name",
            "local x = 7/2\nlocal y = x\nlocal x = x + 1\nreturn x + y",
            Category::Finite,
            Expect::Bits(8.0f64.to_bits()),
        ),
        case(
            "two-locals",
            "local a = -5\nlocal b = 3\nreturn a % b",
            Category::Finite,
            Expect::Bits(1.0f64.to_bits()),
        ),
        case(
            "guest-slot",
            "local guest = 42\nreturn 7 % 5 * 1000 + guest",
            Category::Finite,
            Expect::Bits(2042.0f64.to_bits()),
        ),
        case(
            "nested-guest-slot",
            "local guest = 42\nreturn 7 % (10 % 4) * 1000 + guest",
            Category::Finite,
            Expect::Bits(1042.0f64.to_bits()),
        ),
        case(
            "comment",
            "return 7/2 -- half",
            Category::Finite,
            Expect::Bits(3.5f64.to_bits()),
        ),
        case(
            "semicolon",
            "return 7/2;",
            Category::Finite,
            Expect::Bits(3.5f64.to_bits()),
        ),
        case(
            "large-mod",
            "return (2^53)%3",
            Category::Finite,
            Expect::Bits(2.0f64.to_bits()),
        ),
        case(
            "large-mod-neg-divisor",
            "return (2^53)%-3",
            Category::Finite,
            Expect::Bits(0.0f64.to_bits()),
        ),
        case(
            "large-mod-neg-dividend",
            "return (-2^53)%3",
            Category::Finite,
            Expect::Bits(0.0f64.to_bits()),
        ),
        case(
            "neg-zero",
            "return -0",
            Category::SignedZero,
            Expect::Bits((-0.0f64).to_bits()),
        ),
        case(
            "neg-neg-zero",
            "return -(-0)",
            Category::SignedZero,
            Expect::Bits(0.0f64.to_bits()),
        ),
        case(
            "div-signed-zero",
            "return 0/-1",
            Category::SignedZero,
            Expect::Bits((-0.0f64).to_bits()),
        ),
        case(
            "add-signed-zero",
            "return -0+0",
            Category::SignedZero,
            Expect::Bits(0.0f64.to_bits()),
        ),
        case(
            "add-neg-zeros",
            "return -0+-0",
            Category::SignedZero,
            Expect::Bits((-0.0f64).to_bits()),
        ),
        case(
            "div-inf",
            "return 1/0",
            Category::NonFinite,
            Expect::Class("inf"),
        ),
        case(
            "div-neg-inf",
            "return -1/0",
            Category::NonFinite,
            Expect::Class("-inf"),
        ),
        case(
            "div-nan",
            "return 0/0",
            Category::NonFinite,
            Expect::Class("nan"),
        ),
        case(
            "mod-nan",
            "return 1%0",
            Category::NonFinite,
            Expect::Class("nan"),
        ),
        case(
            "dec-tenth",
            "return 0.1",
            Category::Finite,
            Expect::Bits(0x3fb9_9999_9999_999a),
        ),
        case(
            "int-past-mantissa",
            "return 9007199254740993",
            Category::Finite,
            Expect::Bits(0x4340_0000_0000_0000),
        ),
        case(
            "subnormal",
            "return 1e-323",
            Category::Finite,
            Expect::Bits(0x0000_0000_0000_0002),
        ),
        case(
            "overflow",
            "return 1e400",
            Category::NonFinite,
            Expect::Class("inf"),
        ),
        case(
            "hex-past-mantissa",
            "return 0x20000000000001",
            Category::Finite,
            Expect::Bits(0x4340_0000_0000_0000),
        ),
        case(
            "hex-u64-max",
            "return 0xFFFFFFFFFFFFFFFF",
            Category::Finite,
            Expect::Bits(0x43f0_0000_0000_0000),
        ),
        case(
            "mod-i32-max",
            "return 2147483647 % 7",
            Category::Finite,
            Expect::Bits(1.0f64.to_bits()),
        ),
        case(
            "mod-i32-min",
            "return -2147483648 % -3",
            Category::Finite,
            Expect::Bits((-2.0f64).to_bits()),
        ),
        case(
            "dot-exponent",
            "return 1.e5",
            Category::Finite,
            Expect::Bits(0x40f8_6a00_0000_0000),
        ),
        // Ordinary hex float and the wide hex integer are different literal forms.
        // Expected bits come from the pinned LuaJIT `%a` oracle, not this parser.
        case(
            "hex-float",
            "return 0x1.8p1",
            Category::Finite,
            Expect::Bits(0x4008_0000_0000_0000),
        ),
        case(
            "hex-upper-omit-int",
            "return 0X.8P+2",
            Category::Finite,
            Expect::Bits(0x4000_0000_0000_0000),
        ),
        case(
            "hex-omit-exp",
            "return 0x1.8",
            Category::Finite,
            Expect::Bits(0x3ff8_0000_0000_0000),
        ),
        case(
            "hex-min-subnormal",
            "return 0x1p-1074",
            Category::Finite,
            Expect::Bits(0x0000_0000_0000_0001),
        ),
        case(
            "hex-above-half-ulp",
            "return 0x1.0000000000000001p-1075",
            Category::Finite,
            Expect::Bits(0x0000_0000_0000_0001),
        ),
        case(
            "hex-max-subnormal",
            "return 0x0.fffffffffffffp-1022",
            Category::Finite,
            Expect::Bits(0x000f_ffff_ffff_ffff),
        ),
        case(
            "hex-subnormal-to-normal",
            "return 0x0.fffffffffffff8p-1022",
            Category::Finite,
            Expect::Bits(0x0010_0000_0000_0000),
        ),
        case(
            "hex-tie-down",
            "return 0x1.00000000000008p0",
            Category::Finite,
            Expect::Bits(0x3ff0_0000_0000_0000),
        ),
        case(
            "hex-tie-sticky",
            "return 0x1.000000000000080001p0",
            Category::Finite,
            Expect::Bits(0x3ff0_0000_0000_0001),
        ),
        case(
            "hex-tie-up",
            "return 0x1.00000000000018p0",
            Category::Finite,
            Expect::Bits(0x3ff0_0000_0000_0002),
        ),
        case(
            "hex-tie-zero-tail",
            "return 0x1.000000000000080000p0",
            Category::Finite,
            Expect::Bits(0x3ff0_0000_0000_0000),
        ),
        case(
            "hex-max-finite",
            "return 0x1.fffffffffffffp1023",
            Category::Finite,
            Expect::Bits(0x7fef_ffff_ffff_ffff),
        ),
        case(
            "hex-wide",
            "return 0x10000000000000000",
            Category::Finite,
            Expect::Bits(0x43f0_0000_0000_0000),
        ),
        case(
            "hex-half-ulp-pos-zero",
            "return 0x1p-1075",
            Category::SignedZero,
            Expect::Bits(0.0f64.to_bits()),
        ),
        case(
            "hex-half-ulp-neg-zero",
            "return -0x1p-1075",
            Category::SignedZero,
            Expect::Bits((-0.0f64).to_bits()),
        ),
        case(
            "hex-zero-large-exp",
            "return 0x0.000p1048575",
            Category::SignedZero,
            Expect::Bits(0.0f64.to_bits()),
        ),
        case(
            "hex-overflow",
            "return 0x1.fffffffffffff8p1023",
            Category::NonFinite,
            Expect::Class("inf"),
        ),
        case(
            "hex-large-exp",
            "return 0x1.000p1048575",
            Category::NonFinite,
            Expect::Class("inf"),
        ),
    ]
}

fn case(
    name: &'static str,
    source: &'static str,
    category: Category,
    expect: Expect,
) -> ExecuteCase {
    ExecuteCase {
        name,
        source,
        category,
        expect,
    }
}

fn assert_hex_expectations_differ(cases: &[ExecuteCase]) {
    let float = cases
        .iter()
        .find(|case| case.name == "hex-float")
        .expect("ordinary hex float case");
    let wide = cases
        .iter()
        .find(|case| case.name == "hex-wide")
        .expect("wide hex integer case");
    match (&float.expect, &wide.expect) {
        (Expect::Bits(float_bits), Expect::Bits(wide_bits)) => {
            assert_ne!(
                float_bits, wide_bits,
                "ordinary hex float and wide hex integer share an expected value"
            );
        }
        _ => panic!("hex-float and hex-wide must be finite bit patterns"),
    }
    let mut seen = Vec::new();
    for case in cases.iter().filter(|case| case.name.starts_with("hex-")) {
        if let Expect::Bits(bits) = case.expect
            && !seen.contains(&bits)
        {
            seen.push(bits);
        }
    }
    assert!(
        seen.len() >= 10,
        "hex finite expectations collapsed to {} values",
        seen.len()
    );
}

fn expect_label(expect: &Expect) -> String {
    match expect {
        Expect::Bits(bits) => format!("bits={bits:#018x}"),
        Expect::Class(class) => format!("class={class}"),
        Expect::Empty => "empty".to_string(),
    }
}

fn negative_cases() -> Vec<NegativeCase> {
    vec![
        NegativeCase {
            name: "bare-number",
            source: "42",
            layer: RejectLayer::Syntax,
        },
        NegativeCase {
            name: "goto-keyword",
            source: "local goto = 1",
            layer: RejectLayer::Syntax,
        },
        NegativeCase {
            name: "idiv",
            source: "return 1 // 2",
            layer: RejectLayer::Syntax,
        },
        // Huge text exponents and a 65-bit binary significand never reach lowering.
        NegativeCase {
            name: "hex-exp-huge",
            source: "return 0x1p9999999",
            layer: RejectLayer::Syntax,
        },
        NegativeCase {
            name: "hex-exp-huge-zero",
            source: "return 0x0p9999999",
            layer: RejectLayer::Syntax,
        },
        NegativeCase {
            name: "hex-exp-huge-neg",
            source: "return 0x1p-9999999",
            layer: RejectLayer::Syntax,
        },
        NegativeCase {
            name: "binary-past-64",
            source: "return 0b10000000000000000000000000000000000000000000000000000000000000000",
            layer: RejectLayer::Syntax,
        },
        NegativeCase {
            name: "hex-exp-truncated",
            source: "return 0x1p+",
            layer: RejectLayer::Syntax,
        },
        NegativeCase {
            name: "hex-dot-only",
            source: "return 0x.p1",
            layer: RejectLayer::Syntax,
        },
        NegativeCase {
            name: "signed64",
            source: "return 1LL",
            layer: RejectLayer::NotFloat(ast::NumberKind::Signed64),
        },
        NegativeCase {
            name: "unsigned64",
            source: "return 1ULL",
            layer: RejectLayer::NotFloat(ast::NumberKind::Unsigned64),
        },
        NegativeCase {
            name: "imaginary",
            source: "return 1i",
            layer: RejectLayer::NotFloat(ast::NumberKind::Imaginary),
        },
        NegativeCase {
            name: "global",
            source: "return x",
            layer: RejectLayer::Unsupported(number_source::UnsupportedReason::UnprovenName),
        },
        NegativeCase {
            name: "call",
            source: "return math.abs(-2)",
            layer: RejectLayer::Unsupported(number_source::UnsupportedReason::Expression),
        },
        NegativeCase {
            name: "string-add",
            source: "return 1 + \"2\"",
            layer: RejectLayer::Unsupported(number_source::UnsupportedReason::Expression),
        },
        NegativeCase {
            name: "assign",
            source: "local x = 1\nx = 2\nreturn x",
            layer: RejectLayer::Unsupported(number_source::UnsupportedReason::Statement),
        },
        NegativeCase {
            name: "do-block",
            source: "do\n  local x = 7 / 2\n  return x\nend",
            layer: RejectLayer::Unsupported(number_source::UnsupportedReason::Statement),
        },
        NegativeCase {
            name: "multi-return",
            source: "return 1, 2",
            layer: RejectLayer::Unsupported(number_source::UnsupportedReason::NotSingle),
        },
        NegativeCase {
            name: "bare-return",
            source: "return",
            layer: RejectLayer::Unsupported(number_source::UnsupportedReason::NotSingle),
        },
    ]
}

fn sequential_mod_source(count: usize) -> String {
    let mut source = String::new();
    for index in 0..count {
        source.push_str(&format!("local r{index} = {} % 3\n", 10 + index));
    }
    source.push_str("return r0\n");
    source
}

fn assert_bits(value: &Value, bits: u64, name: &str) {
    let Value::Float(number) = value else {
        panic!("{name} was not binary64: {value:?}");
    };
    assert_eq!(number.to_bits(), bits, "{name} value={number}");
}

fn assert_class(value: &Value, class: &str, name: &str) {
    let Value::Float(number) = value else {
        panic!("{name} was not binary64: {value:?}");
    };
    assert_eq!(float_class(*number), class, "{name} value={number}");
}

fn float_class(number: f64) -> &'static str {
    if number.is_nan() {
        "nan"
    } else if number.is_infinite() && number.is_sign_positive() {
        "inf"
    } else if number.is_infinite() {
        "-inf"
    } else if number == 0.0 && number.is_sign_negative() {
        "-0"
    } else if number == 0.0 {
        "+0"
    } else {
        "finite"
    }
}

fn is_lua_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

struct OracleRun {
    accepted: bool,
    values: Vec<OracleNumber>,
}

#[derive(Debug)]
enum OracleNumber {
    Class(&'static str),
    Bits(u64),
    Type(String),
}

fn assert_oracle_baseline(path: &str) {
    let output = spawn_oracle(path, &["-joff", "-e", BASELINE_DRIVER], None);
    require_success(path, &output, "baseline");
    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some(ORACLE_VERSION), "oracle version: {text}");
    assert_eq!(lines.next(), Some("GOTO_KW"), "5.2 goto keyword: {text}");
    assert_eq!(lines.next(), Some("BREAK_OK"), "5.2 break: {text}");
    assert_eq!(lines.next(), Some("function"), "table.pack: {text}");
}

fn oracle_load(path: &str, source: &str) -> bool {
    let output = spawn_oracle(path, &["-joff", "-e", LOAD_DRIVER], Some(source.as_bytes()));
    require_success(path, &output, "load");
    let text = String::from_utf8_lossy(&output.stdout);
    match text.lines().next().unwrap_or("") {
        "ACCEPT" => true,
        "REJECT" => false,
        other => panic!("oracle load protocol for {source:?}: {other}"),
    }
}

fn oracle_execute(path: &str, source: &str) -> OracleRun {
    let output = spawn_oracle(path, &["-joff", "-e", EXEC_DRIVER], Some(source.as_bytes()));
    require_success(path, &output, "execute");
    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines = text.lines();
    let marker = lines.next().unwrap_or("");
    if marker == "REJECT" {
        return OracleRun {
            accepted: false,
            values: Vec::new(),
        };
    }
    assert_eq!(marker, "ACCEPT", "oracle protocol for {source:?}: {text}");
    let mut values = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        values.push(parse_oracle_value(line));
    }
    OracleRun {
        accepted: true,
        values,
    }
}

fn parse_oracle_value(line: &str) -> OracleNumber {
    if let Some(ty) = line.strip_prefix("TYPE:") {
        return OracleNumber::Type(ty.to_string());
    }
    match line {
        "nan" => OracleNumber::Class("nan"),
        "inf" => OracleNumber::Class("inf"),
        "-inf" => OracleNumber::Class("-inf"),
        "-0" => OracleNumber::Bits((-0.0f64).to_bits()),
        "+0" => OracleNumber::Bits(0.0f64.to_bits()),
        other => OracleNumber::Bits(parse_lua_hex_float(other).to_bits()),
    }
}

fn assert_same_number(oracle: &OracleNumber, rust: &Value, label: &str) {
    let Value::Float(number) = rust else {
        panic!("{label} rust value was not binary64: {rust:?}");
    };
    match oracle {
        OracleNumber::Class(class) => assert_eq!(float_class(*number), *class, "{label}"),
        OracleNumber::Bits(bits) => assert_eq!(number.to_bits(), *bits, "{label} rust={number}"),
        OracleNumber::Type(ty) => panic!("{label} oracle value was type {ty}, not a number"),
    }
}

fn parse_lua_hex_float(text: &str) -> f64 {
    let (negative, rest) = if let Some(rest) = text.strip_prefix('-') {
        (true, rest)
    } else {
        (false, text)
    };
    let rest = rest
        .strip_prefix("0x")
        .or_else(|| rest.strip_prefix("0X"))
        .unwrap_or_else(|| panic!("oracle finite value was not %a: {text}"));
    let (digits, exponent) = rest
        .split_once('p')
        .or_else(|| rest.split_once('P'))
        .unwrap_or_else(|| panic!("oracle finite value was not %a: {text}"));
    let exponent: i32 = exponent.parse().expect("oracle exponent");
    let mut magnitude: u64 = 0;
    let mut fraction_digits = 0i32;
    let mut seen_dot = false;
    for ch in digits.chars() {
        if ch == '.' {
            seen_dot = true;
            continue;
        }
        let digit = ch
            .to_digit(16)
            .unwrap_or_else(|| panic!("oracle hex digit in {text}"));
        magnitude = magnitude
            .checked_mul(16)
            .and_then(|value| value.checked_add(u64::from(digit)))
            .unwrap_or_else(|| panic!("oracle %a magnitude overflow: {text}"));
        if seen_dot {
            fraction_digits += 1;
        }
    }
    let scale = exponent - fraction_digits * 4;
    hex_magnitude_to_f64(negative, magnitude, scale)
}

/// Exact integer times `2^scale`, including a non-zero subnormal.
///
/// `1u64 << n` as `f64` is zero once `n` passes 63, so `0x1p-1074` cannot be
/// built by scaling a float. The bit field is the value.
fn hex_magnitude_to_f64(negative: bool, magnitude: u64, scale: i32) -> f64 {
    let sign = if negative { 1u64 << 63 } else { 0 };
    if magnitude == 0 {
        return f64::from_bits(sign);
    }
    let msb = 63 - i32::try_from(magnitude.leading_zeros()).expect("msb");
    let mut unbiased = msb + scale;
    let mut significand = magnitude;
    if msb > 52 {
        significand = shift_right_round_even(significand, u32::try_from(msb - 52).expect("shift"));
        if significand == 1u64 << 53 {
            significand = 1u64 << 52;
            unbiased += 1;
        }
    } else {
        significand <<= u32::try_from(52 - msb).expect("align");
    }
    if unbiased > 1023 {
        return f64::from_bits(sign | 0x7ff0_0000_0000_0000);
    }
    if unbiased < -1022 {
        let extra = u32::try_from(-1022 - unbiased).expect("subnormal shift");
        significand = shift_right_round_even(significand, extra);
        if significand == 0 {
            return f64::from_bits(sign);
        }
        if significand >= 1u64 << 52 {
            return f64::from_bits(sign | (1u64 << 52));
        }
        return f64::from_bits(sign | significand);
    }
    let exp_field = u64::try_from(unbiased + 1023).expect("biased exponent");
    let fraction = significand & ((1u64 << 52) - 1);
    f64::from_bits(sign | (exp_field << 52) | fraction)
}

fn shift_right_round_even(value: u64, shift: u32) -> u64 {
    if shift == 0 {
        return value;
    }
    if shift >= 64 {
        return 0;
    }
    let kept = value >> shift;
    let remainder = value & ((1u64 << shift) - 1);
    let half = 1u64 << (shift - 1);
    let round_up = if remainder > half {
        true
    } else if remainder < half {
        false
    } else {
        kept & 1 == 1
    };
    if round_up { kept + 1 } else { kept }
}

fn spawn_oracle(path: &str, args: &[&str], stdin_bytes: Option<&[u8]>) -> std::process::Output {
    let mut cmd = Command::new(path);
    cmd.args(args)
        .stdin(if stdin_bytes.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "linux")]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(limit_oracle_child);
    }
    let mut child = cmd.spawn().unwrap_or_else(|err| {
        panic!("LUAJIT_ORACLE at {path:?} failed to start: {err}");
    });
    if let Some(bytes) = stdin_bytes {
        let mut input = child.stdin.take().unwrap_or_else(|| {
            panic!("LUAJIT_ORACLE at {path:?} has no stdin pipe");
        });
        input.write_all(bytes).unwrap_or_else(|err| {
            panic!("LUAJIT_ORACLE at {path:?} rejected stdin: {err}");
        });
    }
    child.wait_with_output().unwrap_or_else(|err| {
        panic!("LUAJIT_ORACLE at {path:?} failed while waiting: {err}");
    })
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

fn require_success(path: &str, output: &std::process::Output, what: &str) {
    if output.status.success() && output.stderr.is_empty() {
        return;
    }
    panic!(
        "LUAJIT_ORACLE at {path:?} {what} failed: status={} stdout={:?} stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

const BASELINE_DRIVER: &str = "\
io.write(jit.version, '\\n')\n\
local g = load('local goto = 1', '@g', 't')\n\
local b = load('for i=1,2 do break print(i) end', '@b', 't')\n\
io.write(g and 'BAD_GOTO' or 'GOTO_KW', '\\n')\n\
io.write(b and 'BREAK_OK' or 'BREAK_BAD', '\\n')\n\
io.write(type(table.pack), '\\n')\n\
";

const LOAD_DRIVER: &str = "\
local src = io.read('*a')\n\
local fn = load(src, '@probe', 't')\n\
io.write(fn and 'ACCEPT\\n' or 'REJECT\\n')\n\
";

const EXEC_DRIVER: &str = "\
local src = io.read('*a')\n\
local fn, err = load(src, '@probe', 't')\n\
if not fn then\n\
  io.write('REJECT\\n')\n\
  return\n\
end\n\
io.write('ACCEPT\\n')\n\
local packed = table.pack(fn())\n\
for i = 1, packed.n do\n\
  local v = packed[i]\n\
  if type(v) ~= 'number' then\n\
    io.write('TYPE:', type(v), '\\n')\n\
  elseif v ~= v then\n\
    io.write('nan\\n')\n\
  elseif v == math.huge then\n\
    io.write('inf\\n')\n\
  elseif v == -math.huge then\n\
    io.write('-inf\\n')\n\
  elseif v == 0 and 1/v < 0 then\n\
    io.write('-0\\n')\n\
  elseif v == 0 then\n\
    io.write('+0\\n')\n\
  else\n\
    io.write(string.format('%a', v), '\\n')\n\
  end\n\
end\n\
";
