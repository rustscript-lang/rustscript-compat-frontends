//! Standard Lua numeric fragments through the source seam.
//!
//! The trial plugin lives here. It is not a product entry and it does not
//! register hosts. Each executed sample is real Lua source, lowered by the
//! seam, then compiled with `SourcePlugin` and run on the VM.

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
    layer: &'static str,
    detail: &'static str,
}

#[test]
fn proven_binary64_sources_run_through_the_plugin() {
    let cases = execute_cases();
    let mut ran = 0usize;
    for case in &cases {
        let outcome = run_plugin(case.source);
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
            }
            (Expect::Class(class), Ok(run)) => {
                assert_eq!(run.stack.len(), 1, "{}", case.name);
                assert_class(&run.stack[0], class, case.name);
            }
            (_, Err(failure)) => panic!(
                "{} failed at {}: {}",
                case.name, failure.layer, failure.detail
            ),
        }
        ran += 1;
    }
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
        assert_eq!(
            failure.layer, case.layer,
            "{} detail={}",
            case.name, failure.detail
        );
        assert!(
            failure.detail.contains(case.detail),
            "{} detail={} missing {}",
            case.name,
            failure.detail,
            case.detail
        );
        match case.layer {
            "syntax" => syntax += 1,
            "classify-not-float" => not_float += 1,
            "compile-unsupported" => unsupported += 1,
            other => panic!("unexpected negative layer {other}"),
        }
    }
    println!(
        "negative cases={} syntax={syntax} classify-not-float={not_float} compile-unsupported={unsupported}",
        cases.len()
    );
    assert_eq!(syntax, 3);
    assert_eq!(not_float, 3);
    assert_eq!(unsupported, 8);
}

#[test]
fn sequential_mods_use_core_slots_and_keep_the_result() {
    let source = sequential_mod_source(12);
    let lowered = lower_source(&source).expect("sequential mods are proven floats");
    let run = run_plugin(&source).expect("sequential mods compile and run");
    assert_bits(&run.stack[0], 1.0f64.to_bits(), "10 % 3");
    assert!(
        lowered.logical_slots > 8,
        "sequential sample should sit above the pinned compaction threshold, logical={}",
        lowered.logical_slots
    );
    assert!(
        run.frame_slots < lowered.logical_slots,
        "Core slot allocation should shorten sequential temps: logical={} frame={}",
        lowered.logical_slots,
        run.frame_slots
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
        "slot boundary logical={} frame={} temps={}",
        lowered.logical_slots,
        run.frame_slots,
        lowered.binding_names.len()
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
        assert_eq!(failure.layer, case.layer, "{}", case.name);
        match case.layer {
            "syntax" => {
                assert!(
                    !oracle_accepted,
                    "{} should be a LuaJIT source rejection",
                    case.name
                );
                syntax_reject += 1;
            }
            "classify-not-float" => {
                assert!(oracle_accepted, "{} is legal LuaJIT syntax", case.name);
                not_float += 1;
            }
            "compile-unsupported" => {
                assert!(oracle_accepted, "{} is legal LuaJIT syntax", case.name);
                unsupported += 1;
            }
            other => panic!("unexpected layer {other}"),
        }
    }
    println!(
        "oracle differential execute={execute} signed-zero={signed_zero} nonfinite={nonfinite} empty={empty} not-float={not_float} compile-unsupported={unsupported} syntax-reject={syntax_reject}"
    );
    assert_eq!(execute, 27);
    assert_eq!(signed_zero, 4);
    assert_eq!(nonfinite, 4);
    assert_eq!(empty, 1);
    assert_eq!(not_float, 3);
    assert_eq!(unsupported, 8);
    assert_eq!(syntax_reject, 3);
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
        ir: lowered.ir,
    })
}

fn source_error(error: NumberSourceError) -> ParseError {
    match error {
        NumberSourceError::Syntax { message } => {
            ParseError::new(format!("lua number syntax: {message}"))
        }
        NumberSourceError::CompileUnsupported { reason } => {
            ParseError::new(format!("lua number compile unsupported: {reason}"))
        }
        NumberSourceError::NotFloat { kind } => {
            ParseError::new(format!("lua number not float: {kind}"))
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

fn negative_cases() -> Vec<NegativeCase> {
    vec![
        NegativeCase {
            name: "bare-number",
            source: "42",
            layer: "syntax",
            detail: "lua number syntax:",
        },
        NegativeCase {
            name: "goto-keyword",
            source: "local goto = 1",
            layer: "syntax",
            detail: "lua number syntax:",
        },
        NegativeCase {
            name: "idiv",
            source: "return 1 // 2",
            layer: "syntax",
            detail: "lua number syntax:",
        },
        NegativeCase {
            name: "signed64",
            source: "return 1LL",
            layer: "classify-not-float",
            detail: "Signed64",
        },
        NegativeCase {
            name: "unsigned64",
            source: "return 1ULL",
            layer: "classify-not-float",
            detail: "Unsigned64",
        },
        NegativeCase {
            name: "imaginary",
            source: "return 1i",
            layer: "classify-not-float",
            detail: "Imaginary",
        },
        NegativeCase {
            name: "global",
            source: "return x",
            layer: "compile-unsupported",
            detail: "proven local",
        },
        NegativeCase {
            name: "call",
            source: "return math.abs(-2)",
            layer: "compile-unsupported",
            detail: "numeric subset",
        },
        NegativeCase {
            name: "string-add",
            source: "return 1 + \"2\"",
            layer: "compile-unsupported",
            detail: "numeric subset",
        },
        NegativeCase {
            name: "hex-float",
            source: "return 0x1.8p1",
            layer: "compile-unsupported",
            detail: "literal form",
        },
        NegativeCase {
            name: "assign",
            source: "local x = 1\nx = 2\nreturn x",
            layer: "compile-unsupported",
            detail: "numeric subset",
        },
        NegativeCase {
            name: "do-block",
            source: "do\n  local x = 7 / 2\n  return x\nend",
            layer: "compile-unsupported",
            detail: "numeric subset",
        },
        NegativeCase {
            name: "multi-return",
            source: "return 1, 2",
            layer: "compile-unsupported",
            detail: "single proven",
        },
        NegativeCase {
            name: "bare-return",
            source: "return",
            layer: "compile-unsupported",
            detail: "single proven",
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
        magnitude = magnitude * 16 + u64::from(digit);
        if seen_dot {
            fraction_digits += 1;
        }
    }
    let scale = exponent - fraction_digits * 4;
    let mut value = ldexp_u64(magnitude, scale);
    if negative {
        value = -value;
    }
    value
}

fn ldexp_u64(magnitude: u64, scale: i32) -> f64 {
    if magnitude == 0 {
        return 0.0;
    }
    let mut digits = magnitude;
    let mut exponent = scale;
    while digits > (1u64 << 53) {
        digits >>= 1;
        exponent += 1;
    }
    (digits as f64) * 2f64.powi(exponent)
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
