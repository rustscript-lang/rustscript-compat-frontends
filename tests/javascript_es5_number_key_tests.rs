//! Flow-proved script and strict closed-function Number-key reads.
use vm::{Value, VmStatus};

fn field(value: &Value, name: &str) -> Value {
    let Value::Map(map) = value else {
        panic!("boxed value: {value:?}")
    };
    map.get(&Value::string(name)).expect("boxed field").clone()
}

fn execute_public(source: &str) -> Value {
    let source = source.to_owned();
    std::thread::Builder::new().stack_size(33_554_432).spawn(move || {
        eprintln!("NUMBER88_SOURCE {source}");
        let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source)
            .unwrap_or_else(|error| panic!("production compilation refused; guest_invocations=0 source={source:?} error={error:?}"));
        machine.set_jit_config(vm::JitConfig { enabled: false, ..*machine.jit_config() });
        machine.set_fuel(10_000_000);
        assert_eq!(machine.run().unwrap(), VmStatus::Halted);
        assert_eq!((machine.jit_native_exec_count(), machine.aot_exec_count()), (0, 0));
        let stack=machine.stack().to_vec();
        eprintln!("NUMBER88_PAYLOAD stack={stack:?} native=0");
        let result=stack.last().expect("completion").clone();
        machine.reset_for_reuse().unwrap();
        assert!(!machine.scope_reset_pending());
        assert_eq!(machine.host_context().resource_count(),0);
        eprintln!("NUMBER88_RESET resource_count=0 guest_invocations=1");
        result
    }).unwrap().join().unwrap()
}
fn assert_box(result: &Value, tag: i64, number: f64, flag: bool) {
    assert_eq!(field(result, "tag"), Value::Int(tag));
    assert_eq!(field(result, "number"), Value::Float(number));
    assert_eq!(field(result, "flag"), Value::Bool(flag));
    assert_eq!(field(result, "ref_slot"), Value::Int(0));
    assert_eq!(field(result, "generation"), Value::Int(0));
    let Value::Map(map) = result else {
        unreachable!()
    };
    assert_eq!(map.len(), 5);
}
fn oracle(source: &str, expected: serde_json::Value) {
    let version = std::process::Command::new("timeout")
        .args(["15", "node", "--version"])
        .output()
        .expect("Node oracle");
    assert!(version.status.success());
    let script = format!(
        "const source={};const value=eval(source);console.log(JSON.stringify({{value,negativeZero:Object.is(value,-0)}}));",
        serde_json::to_string(source).unwrap()
    );
    let output = std::process::Command::new("timeout")
        .args(["15", "node", "-e", &script])
        .output()
        .unwrap();
    eprintln!(
        "NUMBER88_NODE version_rc={:?} version={:?} argv={:?} rc={:?} stdout={:?} stderr={:?}",
        version.status.code(),
        String::from_utf8_lossy(&version.stdout),
        ["timeout", "15", "node", "-e", &script],
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success());
    let actual = serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap();
    if expected.is_null() {
        assert_eq!(actual.as_object().unwrap().len(), 1);
        assert!(
            !actual.as_object().unwrap().contains_key("value"),
            "Undefined must be omitted by JSON.stringify"
        );
    } else {
        assert_eq!(actual.as_object().unwrap().len(), 2);
    }
    assert_eq!(actual["negativeZero"], serde_json::json!(false));
    if expected.is_number() {
        assert_eq!(actual["value"].as_f64(), expected.as_f64());
    } else {
        assert_eq!(actual["value"], expected);
    }
}
fn public_number(source: &str, expected: f64) {
    assert_box(&execute_public(source), 3, expected, false);
    oracle(source, serde_json::json!(expected));
}
fn public_true(source: &str) {
    assert_box(&execute_public(source), 2, 0.0, true);
    oracle(source, serde_json::json!(true));
}

#[test]
fn number_key_function_own_strict() {
    public_number(
        "function f(){\"use strict\";var key=1.5;var o={\"1.5\":7};return o[key];}var r=f();r;",
        7.0,
    );
}

#[test]
fn number_key_function_strict_variants() {
    for source in [
        "\"use strict\";function f(){var key=1.5;var o={\"1.5\":7};return o[key];}var r=f();r;",
        "function f(){\"use strict\";var key=-0;var o={\"0\":7};return o[key];}f();",
        "function f(){\"use strict\";var o={\"0\":7};return o[-0];}f();",
        "function f(){\"use strict\";function g(){var key=1.5;var o={\"1.5\":7};return o[key];}return g();}f();",
    ] {
        public_number(source, 7.0);
    }
}

#[test]
fn number_key_function_catch_and_order() {
    public_number(
        "function f(){\"use strict\";try{throw 1;}catch(e){var key=1.5;var o={\"1.5\":7};return o[key];}}f();",
        7.0,
    );
    for source in [
        "function f(){\"use strict\";var holder={x:0};var o={\"1.5\":9};return o[holder.x=1.5]===9&&holder.x===1.5;}f();",
        "function f(){\"use strict\";var n=0;var o={\"1.5\":7};var r=((n=n===0?1:99)===1?o:o)[(n=n===1?2:99)===2?1.5:0];return r===7&&n===2;}f();",
    ] {
        public_true(source);
    }
}

const RETIRED_FUNCTION_REFUSALS: &[&str] = &[
    "function f(){\"use strict\";var key=1.5;var o={};return o[key];}f();",
    "var key=1.5;var o={\"1.5\":7};o[key];function f(){\"use strict\";var k=1.5;var p={};return p[k];}f();",
    "var key=1.5;var o={\"1.5\":7};o[key];function f(){\"use strict\";try{throw 1;}catch(e){var k=1.5;var p={};return p[k];}}f();",
];

#[test]
fn number_key_function_retired_refusals_are_undefined() {
    for source in RETIRED_FUNCTION_REFUSALS {
        assert_box(&execute_public(source), 0, 0.0, false);
        oracle(source, serde_json::Value::Null);
    }
}

#[test]
fn number_key_function_unproved_refusals() {
    // Free capture and object keys stay outside the profile. The other six
    // shapes in the old table are admitted and executed below.
    let cases = [
        "function f(){'use strict';var key=1.5;function g(){var o={};return o[key];}return g();}f();",
        "function f(){'use strict';var key={};var o={};return o[key];}f();",
    ];
    for prefix in ["", "var key=1.5;var o={\"1.5\":7};o[key];"] {
        for source in cases {
            let source = format!("{prefix}{source}");
            let error = match pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(
                &source,
            ) {
                Ok(_) => panic!("unproved function must refuse: {source}"),
                Err(error) => error,
            };
            eprintln!(
                "NUMBER96_UNPROVED source={source:?} diagnostic={error:?} guest_invocations=0"
            );
            let text = format!("{error:?}");
            assert!(
                text.contains("key") || text.contains("capture") || text.contains("outer"),
                "{text}"
            );
        }
    }
}

const ADMITTED_FUNCTION_KEYS: &[&str] = &[
    "function f(key){'use strict';var o={};return o[key];}f(1.5);",
    "function f(){'use strict';function g(){return 1.5;}var key=g();var o={};return o[key];}f();",
    "function f(){'use strict';var o={};try{throw 1.5;}catch(key){return o[key];}}f();",
    "function f(){'use strict';var key=true;var o={};return o[key];}f();",
    "function f(){'use strict';var key=1.5;if(true){key=true;}var o={};return o[key];}f();",
    "function f(){'use strict';var o={};var x={k:1.5};return o[x.k];}f();",
];

fn assert_undefined(source: &str) {
    assert_box(&execute_public(source), 0, 0.0, false);
    oracle(source, serde_json::Value::Null);
}

#[test]
fn number_key_function_admitted_dynamic_reads_are_undefined() {
    for source in ADMITTED_FUNCTION_KEYS {
        assert_undefined(source);
    }
}

#[test]
fn number_key_function_admitted_dynamic_reads_after_script_prefix() {
    let prefix = "var key=1.5;var o={\"1.5\":7};o[key];";
    for source in ADMITTED_FUNCTION_KEYS {
        assert_undefined(&format!("{prefix}{source}"));
    }
}

#[test]
fn number_key_function_strict_context_restored() {
    const SOURCE: &str = "function f(){'use strict';var key=1.5;var o={'1.5':7};return o[key];}f();var p={'\\141':9};p.a;";
    public_number(SOURCE, 9.0);
    let source = "function f(){'use strict';var key=1.5;var o={'\\141':7};return o[key];}f();";
    let error = parse(source).expect_err("strict octal parser refusal");
    assert!(error.message.contains("LegacyOctal"), "{error:?}");
    assert_eq!(error.span, Some(vm::Span::new(0, 46, 48)));
    eprintln!("NUMBER96_STRICT_PARSE source={source:?} diagnostic={error:?} guest_invocations=0");
}

#[test]
fn number_key_public_decimal() {
    public_number("var key=1.5;var o={\"1.5\":7};var r=o[key];r;", 7.0);
}

#[test]
fn number_key_public_negative_zero() {
    public_number("var key=-0;var o={\"0\":7};var r=o[key];r;", 7.0);
}

const SPECIAL: &[(&str, f64)] = &[
    ("var key=NaN;var o={\"NaN\":11};o[key];", 11.0),
    ("var key=Infinity;var o={\"Infinity\":12};o[key];", 12.0),
    ("var key=-Infinity;var o={\"-Infinity\":13};o[key];", 13.0),
    ("var key=1e21;var o={\"1e+21\":14};o[key];", 14.0),
    ("var key=1e-7;var o={\"1e-7\":15};o[key];", 15.0),
    ("var key=0;var o={\"0\":16};o[key];", 16.0),
];
const ORDER: &[&str] = &[
    "var n=0;var o={\"1.5\":7};var r=((n=n===0?1:99)===1?o:o)[(n=n===1?2:99)===2?1.5:0];r===7&&n===2;",
    "var holder={x:0};var o={\"1.5\":9};o[holder.x=1.5]===9;",
    "try{throw 1;}catch(e){var key=1.5;var o={\"1.5\":7};o[key]===7;}",
];
const ABRUPT: &[(&str, bool)] = &[
    (
        "var key=1.5;var o={get \"1.5\"(){\"use strict\";throw \"\\uD800\\0\";}};var verdict=false;try{var result=o[key];}catch(e){verdict=e===\"\\uD800\\0\";}verdict;",
        true,
    ),
    (
        "function f(){\"use strict\";throw \"callee\";}var key=1.5;var o={get \"1.5\"(){\"use strict\";throw \"\\uD800\\0\";}};var verdict=false;try{f(o[key]);}catch(e){verdict=e===\"\\uD800\\0\";}verdict;",
        true,
    ),
    (
        "var key=1.5;var o={\"1.5\":7};var q={get x(){\"use strict\";throw \"key\";}};var verdict=false;try{o[(q.x,key)];}catch(e){verdict=e===\"key\";}verdict;",
        false,
    ),
    (
        "var key=1.5;var o={\"1.5\":7};var q={get x(){\"use strict\";throw \"base\";}};var verdict=false;try{(q.x,o)[key];}catch(e){verdict=e===\"base\";}verdict;",
        false,
    ),
];
fn all_cases(sources: impl IntoIterator<Item = impl AsRef<str>>, mut run: impl FnMut(&str)) {
    let mut failed = Vec::new();
    for source in sources {
        let source = source.as_ref();
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(source))).is_err() {
            failed.push(source.to_owned());
        }
    }
    assert!(failed.is_empty(), "failed exact sources: {failed:?}");
}
#[test]
fn number_key_public_special_values() {
    all_cases(SPECIAL.iter().map(|(s, _)| *s), |source| {
        public_number(
            source,
            SPECIAL.iter().find(|(s, _)| *s == source).unwrap().1,
        )
    });
}
#[test]
fn number_key_public_evaluation_order() {
    all_cases(ORDER.iter().copied(), public_true);
}
#[test]
fn number_key_abrupt_cleanup_and_collection() {
    all_cases(ABRUPT.iter().map(|(s, _)| *s), |source| {
        public_true(source);
        observe_cleanup(source, ABRUPT.iter().find(|(s, _)| *s == source).unwrap().1);
    });
    observe_cleanup("var key=1.5;var o={\"1.5\":7};o[key]===7;", true);
}
#[test]
fn number_key_scope_refusals() {
    let cases = ["var key={};var o={};o[key];", "var key=0;var a=[7];a[key];"];
    for (i, source) in cases.iter().enumerate() {
        let error = match parse(source) {
            Err(error) => error,
            Ok(_) => panic!("must refuse {source}"),
        };
        eprintln!(
            "NUMBER88_REFUSAL subcase={i} source={source:?} diagnostic={error:?} guest_invocations=0"
        );
        assert!(
            error.message.contains("key")
                || error.message.contains("receiver")
                || error.message.contains("indexed")
                || error.message.contains("subscript"),
            "{error:?}"
        );
    }
}

#[test]
fn number_key_scope_admitted_reads_are_undefined() {
    for source in [
        "var key=true;var o={};o[key];",
        "var key=1.5;if(true){key=true;}var o={};o[key];",
        "var key=1.5;var o={};var x={k:1.5};o[x.k];",
        "var o={};o[0];",
        "var o={};o[1.5];",
    ] {
        assert_undefined(source);
    }
}

#[test]
fn number_key_scope_admitted_put_and_delete() {
    assert_runs("var key=1.5;var o={};o[key]=7;");
    oracle("var key=1.5;var o={};o[key]=7;", serde_json::json!(7.0));
    public_number("var key=1.5;var o={};var r=(o[key]=7);r;", 7.0);
    public_number("var key=1.5;var o={};o[key]=7;o[\"1.5\"];", 7.0);
    assert_runs("var key=1.5;var o={};delete o[key];");
    public_true("var key=1.5;var o={};var r=delete o[key];r;");
}

fn assert_runs(source: &str) {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let mut machine =
                pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source)
                    .unwrap_or_else(|error| panic!("admitted source must compile: {error:?}"));
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            assert_eq!(machine.run().unwrap(), VmStatus::Halted);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn number_key_scope_admitted_call_throws() {
    let source = "var key=1.5;var o={};o[key]();";
    let source_owned = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let mut machine =
                pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(
                    &source_owned,
                )
                .unwrap_or_else(|error| panic!("admitted call must compile: {error:?}"));
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let error = machine.run().expect_err("missing computed callee throws");
            let text = format!("{error:?}");
            assert!(
                text.contains("throw"),
                "expected a JavaScript throw, got {text}"
            );
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
        })
        .unwrap()
        .join()
        .unwrap();
    let script = format!("eval({});", serde_json::to_string(source).unwrap());
    let output = std::process::Command::new("timeout")
        .args(["15", "node", "-e", &script])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "node must throw stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn number_key_dead_literal_put_is_admitted() {
    assert_runs("function f(){\"use strict\";var o={};return 1;o[0]=1;}");
    oracle(
        "function f(){\"use strict\";var o={};return 1;o[0]=1;}",
        serde_json::Value::Null,
    );
    public_number(
        "function f(){\"use strict\";var o={};return 1;o[0]=1;}f();",
        1.0,
    );
    assert_runs("var o={};o[0]=1;");
    oracle("var o={};o[0]=1;", serde_json::json!(1.0));
    public_number("var o={};var r=(o[0]=1);r;", 1.0);
}

fn call(index: u16, args: Vec<vm::Expr>) -> vm::Expr {
    vm::Expr::Call(index, vec![], args, None, None)
}
fn num(e: vm::Expr) -> vm::Expr {
    vm::Expr::Div(Box::new(e), Box::new(vm::Expr::Float(1.0)))
}
fn get(slot: u16, name: &str) -> vm::Expr {
    call(
        vm::BuiltinFunction::Get.call_index(),
        vec![
            vm::Expr::Borrow(Box::new(vm::Expr::Var(slot))),
            vm::Expr::String(name.into()),
        ],
    )
}
fn helper(ir: &vm::FrontendIr, name: &str) -> u16 {
    let found: Vec<_> = ir.functions.iter().filter(|f| f.name == name).collect();
    assert_eq!(found.len(), 1, "{name}");
    found[0].index
}
fn walk_stmts(ss: &mut [vm::Stmt], f: &mut impl FnMut(&mut Vec<vm::Stmt>)) {
    use vm::Stmt as S;
    for s in ss {
        match s {
            S::Let { expr, .. } | S::Assign { expr, .. } | S::Expr { expr, .. } => {
                walk_expr(expr, f)
            }
            S::IfElse {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                walk_expr(condition, f);
                walk_stmts(then_branch, f);
                walk_stmts(else_branch, f);
                f(then_branch);
                f(else_branch);
            }
            S::While {
                condition, body, ..
            } => {
                walk_expr(condition, f);
                walk_stmts(body, f);
            }
            S::For {
                init,
                condition,
                post,
                body,
                ..
            } => {
                walk_stmts(std::slice::from_mut(init), f);
                walk_expr(condition, f);
                walk_stmts(std::slice::from_mut(post), f);
                walk_stmts(body, f);
            }
            S::ClosureLet { closure, .. } => walk_expr(&mut closure.body, f),
            S::Noop { .. }
            | S::FuncDecl { .. }
            | S::Break { .. }
            | S::Continue { .. }
            | S::Drop { .. } => {}
        }
    }
}
fn walk_expr(e: &mut vm::Expr, f: &mut impl FnMut(&mut Vec<vm::Stmt>)) {
    use vm::Expr as E;
    match e {
        E::Block { stmts, expr } => {
            walk_stmts(stmts, f);
            walk_expr(expr, f);
            f(stmts);
        }
        E::Call(_, _, args, _, _) | E::LocalCall(_, _, args, _) | E::ModuleCall(_, _, args, _) => {
            for a in args {
                walk_expr(a, f);
            }
        }
        E::Add(a, b)
        | E::Sub(a, b)
        | E::Mul(a, b)
        | E::Div(a, b)
        | E::Mod(a, b)
        | E::Eq(a, b)
        | E::Lt(a, b)
        | E::Gt(a, b)
        | E::And(a, b)
        | E::Or(a, b) => {
            walk_expr(a, f);
            walk_expr(b, f);
        }
        E::Neg(a) | E::Not(a) | E::ToOwned(a) | E::Borrow(a) | E::BorrowMut(a) => walk_expr(a, f),
        E::IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            walk_expr(condition, f);
            walk_expr(then_expr, f);
            walk_expr(else_expr, f);
        }
        E::ResultConstruct { payload, .. } => walk_expr(payload, f),
        E::ResultQuery { value, .. } | E::ResultTry { value, .. } => walk_expr(value, f),
        E::ResultMatch {
            value,
            ok_expr,
            err_expr,
            ..
        } => {
            walk_expr(value, f);
            walk_expr(ok_expr, f);
            walk_expr(err_expr, f);
        }
        E::OptionalGet { container, key, .. } => {
            walk_expr(container, f);
            walk_expr(key, f);
        }
        E::OptionUnwrapOr {
            value, fallback, ..
        } => {
            walk_expr(value, f);
            walk_expr(fallback, f);
        }
        E::Match {
            value,
            arms,
            default,
            ..
        } => {
            walk_expr(value, f);
            for (_, a) in arms {
                walk_expr(a, f);
            }
            walk_expr(default, f);
        }
        E::Closure(c) => walk_expr(&mut c.body, f),
        E::ClosureCall(c, args) => {
            walk_expr(&mut c.body, f);
            for a in args {
                walk_expr(a, f);
            }
        }
        E::Null
        | E::Int(_)
        | E::Bool(_)
        | E::Float(_)
        | E::String(_)
        | E::Bytes(_)
        | E::Var(_)
        | E::MoveVar(_)
        | E::MoveField { .. }
        | E::MoveIndex { .. }
        | E::FunctionRef(..)
        | E::ModuleFunctionRef(..)
        | E::UnresolvedFunctionRef { .. } => {}
    }
}
fn managed_sites(ss: &[vm::Stmt], managed: u16, out: &mut Vec<(u16, u16, u16, i64)>) {
    use vm::{Expr as E, Stmt as S};
    for s in ss {
        match s {
            S::Assign {
                index,
                expr: E::Call(id, _, args, _, _),
                ..
            } if *id == managed => {
                let [E::Var(carrier), _, E::Var(frame), E::Int(entry)] = args.as_slice() else {
                    panic!("actual managed ABI {args:?}")
                };
                out.push((*index, *carrier, *frame, *entry));
            }
            S::IfElse {
                then_branch,
                else_branch,
                ..
            } => {
                managed_sites(then_branch, managed, out);
                managed_sites(else_branch, managed, out);
            }
            _ => {}
        }
    }
}
struct CleanupObserver {
    function: bool,
}
static CLEANUP_OBSERVER: CleanupObserver = CleanupObserver { function: false };
static FUNCTION_CLEANUP_OBSERVER: CleanupObserver = CleanupObserver { function: true };
impl vm::SourcePlugin for CleanupObserver {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut d = vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
        d.id = vm::FrontendId::new("javascript-es5-number-key-observer");
        d
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        use vm::{Expr as E, Stmt as S, TypeSchema as T};
        let mut ir = vm::SourcePlugin::parse_source(
            &pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN,
            source,
            context,
        )?;
        if self.function {
            std::fs::write(
                std::path::PathBuf::from(std::env::var_os("TMPDIR").unwrap())
                    .join(format!("number96-original-{}.debug", source.len())),
                snapshot(&ir),
            )
            .unwrap();
            instrument_function_cleanup(&mut ir);
            return Ok(ir);
        }
        let managed = helper(&ir, "__compat_es5_number_string_managed");
        let read = helper(&ir, "__compat_ref_read");
        let root_write = helper(&ir, "__compat_es5_root_write");
        let observe = helper(&ir, "__compat_es5_heap_observe");
        let collect = helper(&ir, "__compat_es5_heap_collect");
        let start = u16::try_from(ir.locals).unwrap();
        ir.locals += 17;
        let mut hits = 0;
        walk_stmts(&mut ir.stmts, &mut |ss| {
            let mut sites = vec![];
            managed_sites(ss, managed, &mut sites);
            if sites.is_empty() {
                return;
            }
            assert_eq!(sites.len(), 1);
            let (key, carrier, frame, entry) = sites[0];
            // Initializers can splice the operand Block into the guarded
            // store list. Require its own dominating key-root write, so a
            // containing statement guard is never mistaken for this site.
            if !ss
                .iter()
                .any(|s| matches!(s,S::Let{index,..} if *index==key))
            {
                return;
            }
            if !ss.iter().any(|s|matches!(s,S::Let{expr:E::Call(id,_,args,_,_),..} if *id==root_write && matches!(args.get(2),Some(E::Int(e)) if *e==entry) && matches!(args.get(3),Some(E::Var(v)) if *v==key))) {return;}
            hits += 1;
            let Some(S::Let { index: base, .. }) = ss.first() else {
                panic!("rooted receiver first")
            };
            let base = *base;
            let base_entry = ss
                .iter()
                .find_map(|s| match s {
                    S::Let {
                        expr: E::Call(id, _, args, _, _),
                        ..
                    } if *id == root_write && matches!(args.get(3),Some(E::Var(v)) if *v==base) => {
                        match args.get(2) {
                            Some(E::Int(e)) => Some(*e),
                            _ => None,
                        }
                    }
                    _ => None,
                })
                .expect("exact base copy entry");
            assert_ne!(base, key);
            assert_ne!(base_entry, entry);
            let mut values = vec![];
            for e in [base_entry, entry] {
                for offset in 0..3 {
                    values.push(call(
                        read,
                        vec![
                            E::Var(carrier),
                            num(E::Add(
                                Box::new(E::Var(frame)),
                                Box::new(E::Int(4 + e * 3 + offset)),
                            )),
                        ],
                    ));
                }
            }
            let counters = || {
                [0, 7, 8, 13, 5]
                    .into_iter()
                    .map(|selector| {
                        if selector == 13 {
                            E::IfElse {
                                condition: Box::new(E::Eq(
                                    Box::new(get(key, "tag")),
                                    Box::new(E::Int(4)),
                                )),
                                then_expr: Box::new(num(call(
                                    observe,
                                    vec![E::Var(carrier), E::Int(selector), get(key, "ref_slot")],
                                ))),
                                else_expr: Box::new(E::Float(-1.0)),
                            }
                        } else {
                            num(call(
                                observe,
                                vec![E::Var(carrier), E::Int(selector), E::Int(0)],
                            ))
                        }
                    })
                    .collect::<Vec<_>>()
            };
            values.extend(counters());
            for (i, expr) in values.into_iter().enumerate() {
                ss.push(S::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: start + u16::try_from(i).unwrap(),
                    expr,
                    line: 1,
                });
            }
            ss.push(S::Let {
                index: start + 16,
                declared_schema: Some(T::Int),
                expr: call(collect, vec![E::Var(carrier)]),
                line: 1,
            });
            for (i, expr) in counters().into_iter().enumerate() {
                ss.push(S::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: start + 11 + u16::try_from(i).unwrap(),
                    expr,
                    line: 1,
                });
            }
            eprintln!(
                "NUMBER88_OBSERVER_SITE base_slot={base} key_slot={key} frame={frame} base_entry={base_entry} key_entry={entry} cleanup_sibling_before_collection=true"
            );
        });
        if hits == 0 {
            std::fs::write(
                std::path::PathBuf::from(std::env::var_os("TMPDIR").unwrap())
                    .join("number88-observer-missing-site.debug"),
                snapshot(&ir),
            )
            .unwrap();
        }
        assert_eq!(hits, 1, "one actual numeric operand block");
        for i in 0..16 {
            ir.stmts.insert(
                i,
                S::Let {
                    index: start + u16::try_from(i).unwrap(),
                    declared_schema: Some(T::Number),
                    expr: E::Float(-2.0),
                    line: 1,
                },
            );
        }
        ir.stmts.push(S::Expr {
            expr: E::String("NUMBER88_CLEANUP".into()),
            line: 1,
        });
        for i in 0..16 {
            ir.stmts.push(S::Expr {
                expr: E::Var(start + i),
                line: 1,
            });
        }
        Ok(ir)
    }
}
fn observe_cleanup(source: &str, allocated: bool) {
    observe_cleanup_mode(source, allocated, false);
}
fn observe_cleanup_mode(source: &str, allocated: bool, function: bool) {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let catalog =
                pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
            let options = vm::CompileSourceFileOptions::new()
                .with_host_api_catalog(catalog.clone())
                .with_source_plugin(if function { &FUNCTION_CLEANUP_OBSERVER } else { &CLEANUP_OBSERVER });
            let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
                &source,
                vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-number-key-observer")),
                options,
            )
            .unwrap_or_else(|e| panic!("observer source={source:?}: {e:?}"));
            let mut machine = compiled.into_vm().unwrap();
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
                &mut machine,
                pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
            )
            .unwrap();
            let mut registry = vm::HostFunctionRegistry::restricted();
            for module in [
                pd_vm_compat_frontends::runtime::array::typescript_array_host_module(),
                pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module(),
                pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module(),
                pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module(),
                pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module(),
            ] {
                module
                    .install_from_catalog(&mut registry, &catalog)
                    .unwrap();
            }
            for name in ["math::floor", "math::copysign", "bytes::from_utf8"] {
                registry.allow_builtin(name).unwrap();
            }
            registry.bind_vm_cached(&mut machine).unwrap();
            machine.set_fuel(10_000_000);
            assert_eq!(machine.run().unwrap(), VmStatus::Halted);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            let stack = machine.stack().to_vec();
            eprintln!(
                "NUMBER88_CLEANUP_RAW source={source:?} allocated={allocated} stack={stack:?}"
            );
            if function {
                assert_box(stack.last().expect("original function completion"), 2, 0.0, true);
                machine.reset_for_reuse().unwrap();
                assert!(!machine.scope_reset_pending());
                assert_eq!(machine.host_context().resource_count(), 0);
                eprintln!("NUMBER96_FUNCTION_OBSERVER_HALTED assertions_passed=true native=0 reset_pending=false resource_count=0 guest_invocations=1 source={source:?}");
                return;
            }
            let marker = stack
                .iter()
                .position(|v| *v == Value::string("NUMBER88_CLEANUP"))
                .unwrap();
            assert_eq!(stack.len(), marker + 17);
            assert_box(&stack[marker - 1], 2, 0.0, true);
            let values = &stack[marker + 1..];
            assert_eq!(&values[..6], &vec![Value::Float(0.0); 6]);
            let before = &values[6..11];
            let after = &values[11..16];
            assert_eq!(before[2], Value::Float(0.0));
            assert_eq!(after[2], Value::Float(0.0));
            assert_eq!(before[3], Value::Float(if allocated { 2.0 } else { -1.0 }));
            assert_eq!(after[3], Value::Float(if allocated { 3.0 } else { -1.0 }));
            let number = |v: &Value| match v {
                Value::Float(n) => *n,
                other => panic!("{other:?}"),
            };
            assert_eq!(number(&after[4]), number(&before[4]) + 1.0);
            if allocated {
                assert!(number(&after[0]) < number(&before[0]));
                assert!(number(&after[1]) < number(&before[1]));
            }
            machine.reset_for_reuse().unwrap();
        assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("NUMBER88_OBSERVER_RESET guest_invocations=1 native=0 resource_count=0");
        })
        .unwrap()
        .join()
        .unwrap();
}

// In-activation scalar assertions only. No values leave the callee frame.
fn instrument_function_cleanup(ir: &mut vm::FrontendIr) {
    use vm::{Expr as E, Stmt as S, TypeSchema as T};
    let managed = helper(ir, "__compat_es5_number_string_managed");
    let read = helper(ir, "__compat_ref_read");
    let root_write = helper(ir, "__compat_es5_root_write");
    let observe = helper(ir, "__compat_es5_heap_observe");
    let collect = helper(ir, "__compat_es5_heap_collect");
    let start = u16::try_from(ir.locals).unwrap();
    ir.locals += 2;
    let assertion = |condition| S::Expr {
        expr: call(vm::BuiltinFunction::Assert.call_index(), vec![condition]),
        line: 1,
    };
    let equal = |a, b| E::Eq(Box::new(a), Box::new(b));
    let mut hits = 0;
    for (id, body) in &mut ir.function_impls {
        let mut inspect = |ss: &mut Vec<S>| {
            let mut sites = vec![];
            managed_sites(ss, managed, &mut sites);
            if sites.is_empty() {
                return;
            }
            assert_eq!(sites.len(), 1);
            let (key, carrier, frame, entry) = sites[0];
            if !ss
                .iter()
                .any(|s| matches!(s,S::Let{index,..} if *index==key))
            {
                return;
            }
            let roots: Vec<_> = ss
                .iter()
                .filter_map(|s| match s {
                    S::Let {
                        expr: E::Call(i, _, args, _, _),
                        ..
                    } if *i == root_write => match args.as_slice() {
                        [E::Var(c), E::Var(f), E::Int(e), E::Var(v)]
                            if *c == carrier && *f == frame =>
                        {
                            Some((*v, *e))
                        }
                        _ => None,
                    },
                    _ => None,
                })
                .collect();
            if !roots.contains(&(key, entry)) {
                return;
            }
            let Some(S::Let { index: base, .. }) = ss.first() else {
                panic!("actual base Let");
            };
            let base = *base;
            let base_entry = roots
                .iter()
                .find(|(v, _)| *v == base)
                .expect("actual base root")
                .1;
            // The existing emitter initializes base, key, then result before access.
            assert_eq!(
                roots.len(),
                3,
                "exact dominating base/key/result root writes"
            );
            assert_eq!(roots[0], (base, base_entry));
            assert_eq!(roots[1], (key, entry));
            let (result, result_entry) = roots[2];
            assert_ne!(result, key);
            assert_ne!(result, base);
            let undefined = ss
                .iter()
                .find_map(|s| match s {
                    S::Let {
                        index,
                        expr: E::Call(id, _, args, _, _),
                        ..
                    } if *index == key && args.is_empty() => Some(*id),
                    _ => None,
                })
                .expect("actual dominating Undefined initializer");
            let clears: Vec<_> = ss
                .iter()
                .enumerate()
                .filter_map(|(position, s)| match s {
                    S::Let {
                        expr: E::Call(id, _, args, _, _),
                        ..
                    } if *id == root_write => match args.as_slice() {
                        [E::Var(c), E::Var(f), E::Int(e), E::Call(u, _, a, _, _)]
                            if *c == carrier
                                && *f == frame
                                && (*e == base_entry || *e == entry)
                                && *u == undefined
                                && a.is_empty() =>
                        {
                            Some((position, *e))
                        }
                        _ => None,
                    },
                    _ => None,
                })
                .collect();
            assert_eq!(clears.len(), 2, "the real read clears both operands once");
            assert_eq!([clears[0].1, clears[1].1], [base_entry, entry]);
            // An initializer may splice the read into its statement list.
            // Insert immediately after the read's clears, before the caller's
            // store/return guard and result-root consumption, not at list end.
            let insertion = clears[1].0 + 1;
            let consumer_tail = ss.split_off(insertion);
            let cell = |e, offset| {
                num(call(
                    read,
                    vec![
                        E::Var(carrier),
                        num(E::Add(
                            Box::new(E::Var(frame)),
                            Box::new(E::Int(4 + e * 3 + offset)),
                        )),
                    ],
                ))
            };
            for e in [base_entry, entry] {
                for offset in 0..3 {
                    ss.push(assertion(equal(cell(e, offset), E::Float(0.0))));
                }
            }
            // A normal managed result keeps its own exact token; on Get Err
            // the initialized result is Undefined and its entry remains empty.
            for offset in 0..3 {
                let expected = E::IfElse {
                    condition: Box::new(equal(get(result, "tag"), E::Int(5))),
                    then_expr: Box::new(if offset == 0 {
                        E::Float(5.0)
                    } else {
                        num(get(
                            result,
                            if offset == 1 {
                                "ref_slot"
                            } else {
                                "generation"
                            },
                        ))
                    }),
                    else_expr: Box::new(E::Float(0.0)),
                };
                ss.push(assertion(equal(cell(result_entry, offset), expected)));
            }
            let row = || {
                num(call(
                    observe,
                    vec![E::Var(carrier), E::Int(13), get(key, "ref_slot")],
                ))
            };
            let collections = || num(call(observe, vec![E::Var(carrier), E::Int(5), E::Int(0)]));
            ss.push(assertion(equal(get(key, "tag"), E::Int(4))));
            ss.push(assertion(equal(row(), E::Float(2.0))));
            ss.push(S::Let {
                index: start,
                declared_schema: Some(T::Number),
                expr: collections(),
                line: 1,
            });
            ss.push(S::Let {
                index: start + 1,
                declared_schema: Some(T::Int),
                expr: call(collect, vec![E::Var(carrier)]),
                line: 1,
            });
            ss.push(assertion(equal(row(), E::Float(3.0))));
            ss.push(assertion(equal(
                collections(),
                E::Add(Box::new(E::Var(start)), Box::new(E::Float(1.0))),
            )));
            ss.extend(consumer_tail);
            hits += 1;
            eprintln!(
                "NUMBER96_FUNCTION_SITE impl={id} carrier={carrier} frame={frame} base={base}:{base_entry} key={key}:{entry} result={result}:{result_entry} asserts_inside_read_block_before_return_guard=true clear_predicates=6 result_predicates=3 key_row=2->3 collection_delta=1"
            );
        };
        walk_stmts(&mut body.body_stmts, &mut inspect);
        walk_expr(&mut body.body_expr, &mut inspect);
    }
    assert_eq!(hits, 1, "one real function numeric-read Block");
}

const FUNCTION_ABRUPT: &[&str] = &[
    "function f(){'use strict';var key=1.5;var o={get '1.5'(){throw '\\uD800\\0';}};return o[key];}var verdict=false;try{f();}catch(e){verdict=e==='\\uD800\\0';}verdict;",
    "function f(){'use strict';function h(a,b){throw 'callee';}var n=0;var key=1.5;var o={get '1.5'(){throw '\\uD800\\0';}};try{h(o[key],(n=9));}catch(e){return e==='\\uD800\\0'&&n===0;}return false;}f();",
];
const FUNCTION_ALIAS: &str = "function f(){'use strict';var child={x:7};var key=1.5;var o={'1.5':child};var r=o[key];return r===child&&r.x===7;}f();";

#[test]
fn number_key_function_abrupt_public() {
    for source in FUNCTION_ABRUPT {
        public_true(source);
    }
}

#[test]
fn number_key_function_alias_public() {
    public_true(FUNCTION_ALIAS);
}

#[test]
fn number_key_function_roots_and_collection() {
    for source in FUNCTION_ABRUPT.iter().copied().chain([FUNCTION_ALIAS]) {
        observe_cleanup_mode(source, true, true);
    }
}

fn parse(source: &str) -> Result<vm::FrontendIr, vm::ParseError> {
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    vm::SourcePlugin::parse_source(
        &pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN,
        source,
        &vm::SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::compiler::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        },
    )
}

#[test]
fn number_key_inactive_ir() {
    debug_observer::self_checks();
    let root =
        std::path::PathBuf::from(std::env::var_os("TMPDIR").unwrap()).join("number88-inactive");
    std::fs::create_dir(&root).unwrap();
    for (i, source) in [
        "var x=1;x;",
        "var o={x:7};o.x;",
        "var o={\"x\":7};o[\"x\"];",
        "var a=[7];a[0];",
        "var realm=Object;realm;",
        "'use strict';function f(){var o={x:7};return o.x;}f();",
        "function f(){'use strict';var o={x:7};return o['x'];}f();",
        "function f(){'use strict';var a=[7];return a[0];}f();",
        "function f(){'use strict';function g(){var o={x:7};return o.x;}return g();}f();",
    ]
    .iter()
    .enumerate()
    {
        let ir = parse(source).unwrap_or_else(|e| panic!("{source}: {e:?}"));
        let text = snapshot(&ir);
        let current = debug_observer::checked(text.as_bytes());
        std::fs::write(root.join(format!("{i:02}.js")), source).unwrap();
        std::fs::write(root.join(format!("{i:02}.debug")), &text).unwrap();
        std::fs::write(root.join(format!("{i:02}.observer")), current.receipt()).unwrap();
        if let Some(before) = std::env::var_os("NUMBER88_INACTIVE_BASELINE") {
            let before = std::path::PathBuf::from(before);
            assert_eq!(
                std::fs::read_to_string(before.join(format!("{i:02}.js"))).unwrap(),
                *source
            );
            let raw = std::fs::read(before.join(format!("{i:02}.debug"))).unwrap();
            let previous = debug_observer::checked(&raw);
            if i == 0 {
                debug_observer::actual_checks(&raw, &previous);
            }
            assert_eq!(
                previous.data, current.data,
                "full canonical inactive {source}"
            );
            eprintln!(
                "NUMBER88_INACTIVE_EQUAL subject={i:02} before_bytes={} current_bytes={} full_canonical_equal=true",
                raw.len(),
                text.len()
            );
        }
        eprintln!(
            "NUMBER88_INACTIVE_CAPTURE subject={i:02} accepted=true bytes={}",
            text.len()
        );
    }
}

// Complete parser output, with only unordered maps presented in key order.
// Float leaves additionally carry raw bits at their deterministic AST path.
fn snapshot(ir: &vm::FrontendIr) -> String {
    use std::collections::BTreeMap;
    use std::fmt::Write;
    let mut rest = ir.clone();
    let implementations: BTreeMap<_, _> = std::mem::take(&mut rest.function_impls)
        .into_iter()
        .collect();
    let schemas: BTreeMap<_, _> = std::mem::take(&mut rest.struct_schemas)
        .into_iter()
        .collect();
    let sources: BTreeMap<_, _> = std::mem::take(&mut rest.function_sources)
        .into_iter()
        .collect();
    let mut text = format!(
        "{rest:#?}\nimplementations={implementations:#?}\nschemas={schemas:#?}\nsources={sources:#?}\n"
    );
    fn expr(e: &vm::Expr, path: &str, out: &mut String) {
        use vm::Expr as E;
        match e {
            E::Float(n) => {
                writeln!(out, "{path} FloatBits={:016x}", n.to_bits()).unwrap();
            }
            E::Add(a, b)
            | E::Sub(a, b)
            | E::Mul(a, b)
            | E::Div(a, b)
            | E::Mod(a, b)
            | E::Eq(a, b)
            | E::Lt(a, b)
            | E::Gt(a, b)
            | E::And(a, b)
            | E::Or(a, b) => {
                expr(a, &format!("{path}/left"), out);
                expr(b, &format!("{path}/right"), out);
            }
            E::Neg(a) | E::Not(a) | E::ToOwned(a) | E::Borrow(a) | E::BorrowMut(a) => {
                expr(a, &format!("{path}/operand"), out)
            }
            E::Call(_, _, args, _, _)
            | E::LocalCall(_, _, args, _)
            | E::ModuleCall(_, _, args, _) => {
                for (i, a) in args.iter().enumerate() {
                    expr(a, &format!("{path}/arg{i}"), out);
                }
            }
            E::IfElse {
                condition,
                then_expr,
                else_expr,
            } => {
                expr(condition, &format!("{path}/condition"), out);
                expr(then_expr, &format!("{path}/then"), out);
                expr(else_expr, &format!("{path}/else"), out);
            }
            E::Block { stmts, expr: e } => {
                statements(stmts, path, out);
                expr(e, &format!("{path}/tail"), out);
            }
            E::ResultConstruct { payload, .. } => expr(payload, &format!("{path}/payload"), out),
            E::ResultQuery { value, .. } | E::ResultTry { value, .. } => {
                expr(value, &format!("{path}/value"), out)
            }
            E::ResultMatch {
                value,
                ok_expr,
                err_expr,
                ..
            } => {
                expr(value, &format!("{path}/value"), out);
                expr(ok_expr, &format!("{path}/ok"), out);
                expr(err_expr, &format!("{path}/err"), out);
            }
            E::OptionalGet { container, key, .. } => {
                expr(container, &format!("{path}/container"), out);
                expr(key, &format!("{path}/key"), out);
            }
            E::OptionUnwrapOr {
                value, fallback, ..
            } => {
                expr(value, &format!("{path}/value"), out);
                expr(fallback, &format!("{path}/fallback"), out);
            }
            E::Match {
                value,
                arms,
                default,
                ..
            } => {
                expr(value, &format!("{path}/value"), out);
                for (i, (_, a)) in arms.iter().enumerate() {
                    expr(a, &format!("{path}/arm{i}"), out);
                }
                expr(default, &format!("{path}/default"), out);
            }
            E::Null
            | E::Int(_)
            | E::Bool(_)
            | E::String(_)
            | E::Bytes(_)
            | E::Var(_)
            | E::MoveVar(_)
            | E::MoveField { .. }
            | E::MoveIndex { .. }
            | E::FunctionRef(..)
            | E::ModuleFunctionRef(..)
            | E::UnresolvedFunctionRef { .. } => {}
            E::Closure(c) => expr(&c.body, &format!("{path}/closure/body"), out),
            E::ClosureCall(c, args) => {
                expr(&c.body, &format!("{path}/closure/body"), out);
                for (i, a) in args.iter().enumerate() {
                    expr(a, &format!("{path}/arg{i}"), out);
                }
            }
        }
    }
    fn statements(ss: &[vm::Stmt], path: &str, out: &mut String) {
        use vm::Stmt as S;
        for (i, s) in ss.iter().enumerate() {
            let p = format!("{path}/stmt{i}");
            match s {
                S::Let { expr: e, .. } | S::Assign { expr: e, .. } | S::Expr { expr: e, .. } => {
                    expr(e, &p, out)
                }
                S::IfElse {
                    condition,
                    then_branch,
                    else_branch,
                    ..
                } => {
                    expr(condition, &format!("{p}/condition"), out);
                    statements(then_branch, &format!("{p}/then"), out);
                    statements(else_branch, &format!("{p}/else"), out);
                }
                S::While {
                    condition, body, ..
                } => {
                    expr(condition, &format!("{p}/condition"), out);
                    statements(body, &format!("{p}/body"), out);
                }
                S::For {
                    init,
                    condition,
                    post,
                    body,
                    ..
                } => {
                    statements(std::slice::from_ref(init), &format!("{p}/init"), out);
                    expr(condition, &format!("{p}/condition"), out);
                    statements(std::slice::from_ref(post), &format!("{p}/post"), out);
                    statements(body, &format!("{p}/body"), out);
                }
                S::ClosureLet { closure, .. } => {
                    expr(&closure.body, &format!("{p}/closure/body"), out)
                }
                S::Noop { .. }
                | S::FuncDecl { .. }
                | S::Break { .. }
                | S::Continue { .. }
                | S::Drop { .. } => {}
            }
        }
    }
    statements(&ir.stmts, "script", &mut text);
    for (id, body) in implementations {
        statements(&body.body_stmts, &format!("function{id}"), &mut text);
        expr(&body.body_expr, &format!("function{id}/tail"), &mut text);
    }
    text
}

// Narrow byte-preserving observer for Rust Debug Object({quoted_key: value}).
// Only complete entries move. Commas/whitespace remain in their original slots;
// every emitted range refers to the input, so an exact inverse is checkable.
mod debug_observer {
    #[derive(Default)]
    struct Map {
        start: usize,
        end: usize,
        commas: Vec<usize>,
        children: Vec<usize>,
        entries: Vec<Entry>,
        tail: (usize, usize),
        permutation: Vec<usize>,
    }
    struct Entry {
        lo: usize,
        key: usize,
        end: usize,
        hi: usize,
        decoded: Vec<u8>,
    }
    struct Delimiter {
        kind: u8,
        map: Option<usize>,
        object: Option<usize>,
    }
    pub(super) struct Normalized {
        pub data: Vec<u8>,
        provenance: Vec<(usize, usize)>,
        maps: Vec<Map>,
        nonmap_objects: usize,
    }
    fn ws(b: u8) -> bool {
        matches!(b, b' ' | b'\t' | b'\r' | b'\n')
    }
    fn blank(data: &[u8]) -> bool {
        data.iter().all(|&b| ws(b))
    }
    fn quoted_end(data: &[u8], start: usize) -> Result<usize, String> {
        let mut i = start + 1;
        while i < data.len() {
            match data[i] {
                b'"' => return Ok(i + 1),
                b'\\' => i += 2,
                _ => i += 1,
            }
        }
        Err(format!("unterminated quoted string at {start}"))
    }
    fn decode(raw: &[u8]) -> Result<Vec<u8>, String> {
        let s = std::str::from_utf8(&raw[1..raw.len() - 1]).map_err(|e| e.to_string())?;
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                if (c as u32) < 32 {
                    return Err("unescaped key control character".into());
                }
                out.push(c);
                continue;
            }
            let c = match chars.next() {
                Some('n') => '\n',
                Some('r') => '\r',
                Some('t') => '\t',
                Some('0') => '\0',
                Some('\\') => '\\',
                Some('"') => '"',
                Some('\'') => '\'',
                Some('u') => {
                    if chars.next() != Some('{') {
                        return Err("missing unicode opener".into());
                    }
                    let mut digits = String::new();
                    loop {
                        match chars.next() {
                            Some('}') if !digits.is_empty() => break,
                            Some(d) if d.is_ascii_hexdigit() && digits.len() < 6 => digits.push(d),
                            _ => return Err("invalid Rust unicode escape".into()),
                        }
                    }
                    let n = u32::from_str_radix(&digits, 16).map_err(|e| e.to_string())?;
                    char::from_u32(n).ok_or("invalid Unicode scalar")?
                }
                _ => return Err("unsupported Rust key escape".into()),
            };
            out.push(c);
        }
        Ok(out.into_bytes())
    }
    fn entries(data: &[u8], node: &mut Map) -> Result<(), String> {
        let mut start = node.start + 1;
        let limit = node.end - 1;
        let mut ends: Vec<_> = node.commas.iter().map(|&p| (p, p + 1)).collect();
        if ends.last().is_none_or(|&(_, hi)| !blank(&data[hi..limit])) {
            ends.push((limit, limit));
        }
        let mut seen = std::collections::BTreeSet::new();
        for (end, hi) in ends {
            if blank(&data[start..end]) {
                if !node.commas.is_empty() {
                    return Err("missing map entry/key".into());
                }
                node.tail = (start, limit);
                return Ok(());
            }
            let mut left = start;
            let mut right = end;
            while ws(data[left]) {
                left += 1;
            }
            while ws(data[right - 1]) {
                right -= 1;
            }
            if data[left] != b'"' {
                return Err("Object map entry lacks quoted key".into());
            }
            let key_end = quoted_end(data, left)?;
            if key_end > right {
                return Err("key exceeds map entry".into());
            }
            let decoded = decode(&data[left..key_end])?;
            if !seen.insert(decoded.clone()) {
                return Err("duplicate decoded map key".into());
            }
            let mut colon = key_end;
            while colon < right && ws(data[colon]) {
                colon += 1;
            }
            if colon >= right || data[colon] != b':' || blank(&data[colon + 1..right]) {
                return Err("missing map colon/value".into());
            }
            node.entries.push(Entry {
                lo: start,
                key: left,
                end: right,
                hi,
                decoded,
            });
            start = hi;
        }
        node.tail = (start, limit);
        Ok(())
    }
    fn emit(pieces: &mut Vec<(usize, usize)>, a: usize, b: usize) {
        if a != b {
            pieces.push((a, b));
        }
    }
    fn region(
        a: usize,
        b: usize,
        children: &[usize],
        maps: &[Map],
        pieces: &mut Vec<(usize, usize)>,
    ) {
        let mut pos = a;
        for &id in children {
            let child = &maps[id];
            if a <= child.start && child.end <= b {
                emit(pieces, pos, child.start);
                render(id, maps, pieces);
                pos = child.end;
            }
        }
        emit(pieces, pos, b);
    }
    fn render(id: usize, maps: &[Map], pieces: &mut Vec<(usize, usize)>) {
        let node = &maps[id];
        emit(pieces, node.start, node.start + 1);
        for (slot, &original) in node.entries.iter().zip(&node.permutation) {
            let picked = &node.entries[original];
            emit(pieces, slot.lo, slot.key);
            region(picked.key, picked.end, &node.children, maps, pieces);
            emit(pieces, slot.end, slot.hi);
        }
        emit(pieces, node.tail.0, node.tail.1);
        emit(pieces, node.end - 1, node.end);
    }
    pub(super) fn normalize(data: &[u8], reverse: bool) -> Result<Normalized, String> {
        let mut stack: Vec<Delimiter> = Vec::new();
        let mut maps: Vec<Map> = Vec::new();
        let mut roots = Vec::new();
        let mut active: Vec<usize> = Vec::new();
        let mut recent: Vec<(u8, usize, usize)> = Vec::new();
        let mut objects = 0;
        let mut a = 0;
        while a < data.len() {
            let raw = data[a];
            let (kind, b) = if raw == b'"' {
                (b'Q', quoted_end(data, a)?)
            } else if data[a..].starts_with(b"Object")
                && (a == 0 || !(data[a - 1].is_ascii_alphanumeric() || data[a - 1] == b'_'))
                && data
                    .get(a + 6)
                    .is_none_or(|c| !(c.is_ascii_alphanumeric() || *c == b'_'))
            {
                (b'O', a + 6)
            } else if b"()[]{},:".contains(&raw) {
                (raw, a + 1)
            } else {
                a += 1;
                continue;
            };
            match kind {
                b'(' | b'[' | b'{' => {
                    if kind == b'('
                        && recent
                            .last()
                            .is_some_and(|&(k, _, end)| k == b'O' && blank(&data[end..a]))
                    {
                        objects += 1;
                    }
                    let mut map = None;
                    if kind == b'{'
                        && recent.len() == 2
                        && recent[0].0 == b'O'
                        && recent[1].0 == b'('
                        && blank(&data[recent[0].2..recent[1].1])
                        && blank(&data[recent[1].2..a])
                    {
                        let id = maps.len();
                        if let Some(&parent) = active.last() {
                            maps[parent].children.push(id);
                        } else {
                            roots.push(id);
                        }
                        maps.push(Map {
                            start: a,
                            ..Map::default()
                        });
                        active.push(id);
                        let payload = stack.last_mut().ok_or("missing Object payload opener")?;
                        if payload.kind != b'(' {
                            return Err("missing Object payload opener".into());
                        }
                        payload.object = Some(id);
                        map = Some(id);
                    }
                    stack.push(Delimiter {
                        kind,
                        map,
                        object: None,
                    });
                }
                b')' | b']' | b'}' => {
                    let opening = stack.pop().ok_or("unmatched closer")?;
                    if !matches!(
                        (opening.kind, kind),
                        (b'(', b')') | (b'[', b']') | (b'{', b'}')
                    ) {
                        return Err("mismatched delimiter".into());
                    }
                    if let Some(id) = opening.map {
                        maps[id].end = b;
                        assert_eq!(active.pop(), Some(id));
                        entries(data, &mut maps[id])?;
                    }
                    if let Some(id) = opening.object {
                        let gap = &data[maps[id].end..a];
                        let nonwhite: Vec<_> = gap.iter().copied().filter(|&c| !ws(c)).collect();
                        if !nonwhite.is_empty() && nonwhite != b"," {
                            return Err("ambiguous Object tuple payload".into());
                        }
                    }
                }
                b',' => {
                    if let Some(id) = stack.last().and_then(|d| d.map) {
                        maps[id].commas.push(a);
                    }
                }
                _ => {}
            }
            recent.push((kind, a, b));
            if recent.len() > 2 {
                recent.remove(0);
            }
            a = b;
        }
        if !stack.is_empty() {
            return Err("unclosed delimiter".into());
        }
        for map in &mut maps {
            let mut order: Vec<_> = (0..map.entries.len()).collect();
            order.sort_by(|&a, &b| map.entries[a].decoded.cmp(&map.entries[b].decoded));
            if reverse {
                order.reverse();
            }
            map.permutation = order;
        }
        let mut provenance = Vec::new();
        region(0, data.len(), &roots, &maps, &mut provenance);
        let mut canonical = Vec::with_capacity(data.len());
        for &(a, b) in &provenance {
            canonical.extend_from_slice(&data[a..b]);
        }
        assert_eq!(canonical.len(), data.len(), "byte length changed");
        let nonmap_objects = objects - maps.len();
        Ok(Normalized {
            data: canonical,
            provenance,
            maps,
            nonmap_objects,
        })
    }
    impl Normalized {
        fn inverse(&self) -> Vec<u8> {
            let mut chunks = Vec::new();
            let mut offset = 0;
            for &(a, b) in &self.provenance {
                assert!(b >= a);
                chunks.push((a, b, &self.data[offset..offset + b - a]));
                offset += b - a;
            }
            assert_eq!(offset, self.data.len());
            chunks.sort_by_key(|c| c.0);
            let mut expected = 0;
            let mut original = Vec::with_capacity(offset);
            for (a, b, bytes) in chunks {
                assert_eq!(a, expected, "not an exact disjoint input partition");
                expected = b;
                original.extend_from_slice(bytes);
            }
            assert_eq!(expected, offset);
            original
        }
        pub(super) fn receipt(&self) -> String {
            let permutations: Vec<_> = self.maps.iter().map(|m| &m.permutation).collect();
            format!(
                "bytes={} maps={} entries={} nonmap_objects={}\nprovenance={:?}\npermutations={permutations:?}\n",
                self.data.len(),
                self.maps.len(),
                self.maps.iter().map(|m| m.entries.len()).sum::<usize>(),
                self.nonmap_objects,
                self.provenance
            )
        }
    }
    pub(super) fn checked(data: &[u8]) -> Normalized {
        let normalized = normalize(data, false).expect("frozen Debug grammar");
        assert_eq!(normalized.inverse(), data, "exact byte inverse");
        assert_eq!(
            normalize(&normalized.data, false).unwrap().data,
            normalized.data,
            "idempotence"
        );
        let reversed = normalize(data, true).unwrap();
        assert_eq!(reversed.inverse(), data, "reverse exact inverse");
        assert_eq!(
            normalize(&reversed.data, false).unwrap().data,
            normalized.data,
            "reverse permutation invariance"
        );
        normalized
    }
    pub(super) fn self_checks() {
        let x = "pre [1, 2] Object({\"z\": Int, \"a\": Object({\"y\": Float, \"b\": Bool})}) post FloatBits=0000000000000000\n";
        let y = "pre [1, 2] Object({\"a\": Object({\"b\": Bool, \"y\": Float}), \"z\": Int}) post FloatBits=0000000000000000\n";
        let n = checked(x.as_bytes());
        assert_eq!(n.data, checked(y.as_bytes()).data);
        checked(br#"Object({"q\"}\\": Object({"\u{61}": Int, "\0": Float}), "a": Bool})"#);
        let mut count = 4;
        for (name, from, to) in [
            ("field_type", "\"z\": Int", "\"z\": Float"),
            ("field_name", "\"z\": Int", "\"zz\": Int"),
            ("field_value", "\"z\": Int", "\"z\": Int(9)"),
            ("missing_field", "\"z\": Int, ", ""),
            ("vector_order", "[1, 2]", "[2, 1]"),
            (
                "signed_zero_sidecar",
                "FloatBits=0000000000000000",
                "FloatBits=8000000000000000",
            ),
            (
                "nan_payload_sidecar",
                "FloatBits=0000000000000000",
                "FloatBits=7ff8123456789abc",
            ),
            ("whitespace", "pre ", "pre  "),
            ("nonmap_punctuation", "post ", "post; "),
        ] {
            assert_ne!(
                n.data,
                checked(x.replace(from, to).as_bytes()).data,
                "{name}"
            );
            count += 1;
        }
        for (name, a, b) in [
            ("parameter_order", "params: [2, 7]", "params: [7, 2]"),
            (
                "statement_order",
                "stmts: [Int(1), Int(2)]",
                "stmts: [Int(2), Int(1)]",
            ),
            (
                "argument_order",
                "Call(1, [Int(2), Int(7)])",
                "Call(1, [Int(7), Int(2)])",
            ),
            (
                "nan_payloads",
                "Float(NaN) FloatBits=7ff8123456789abc",
                "Float(NaN) FloatBits=7ff8123456789abd",
            ),
            (
                "nan_sign",
                "Float(NaN) FloatBits=7ff8123456789abc",
                "Float(NaN) FloatBits=fff8123456789abc",
            ),
            (
                "object_literal_vector",
                "Object([(\"z\",Int), (\"a\",Bool)])",
                "Object([(\"a\",Bool), (\"z\",Int)])",
            ),
            (
                "unrelated_map",
                "{\"z\": Int, \"a\": Bool}",
                "{\"a\": Bool, \"z\": Int}",
            ),
        ] {
            assert_ne!(
                checked(a.as_bytes()).data,
                checked(b.as_bytes()).data,
                "{name}"
            );
            count += 1;
        }
        for bad in [
            r#"Object({"a": Int, "a": Bool})"#,
            r#"Object({"a": Int, "\u{61}": Bool})"#,
            r#"Object({: Int})"#,
            r#"Object({"a": })"#,
            r#"Object({"a" Int})"#,
            r#"Object({,"a": Int})"#,
            r#"Object({"a": Int)"#,
            r#"Object({"a": Int}, 7)"#,
            r#"Object({"\w": Int})"#,
            r#"Object({"a: Int})"#,
            r#"Object({"\u{d800}": Int})"#,
            r#"Object({"\u{110000}": Int})"#,
            r#"Object({"\u{}": Int})"#,
        ] {
            assert!(
                normalize(bad.as_bytes(), false).is_err(),
                "must reject {bad}"
            );
            count += 1;
        }
        let opaque = br#"String("Object({\"b\": Int, \"a\": Bool})")"#;
        assert_eq!(checked(opaque).data, opaque);
        checked(b"Object( {  } , )");
        count += 2;
        eprintln!("GLOBAL40_OBSERVER_SELF_CHECKS count={count} all_passed=true");
    }
    pub(super) fn actual_checks(data: &[u8], n: &Normalized) {
        let text = std::str::from_utf8(data).unwrap();
        for (name, changed) in [
            (
                "field_type",
                text.replacen("\"number\": Float", "\"number\": Int", 1),
            ),
            (
                "missing_field",
                text.replacen("                \"number\": Float,\n", "", 1),
            ),
        ] {
            assert_ne!(changed.as_bytes(), data, "mutation applied {name}");
            assert_ne!(
                normalize(changed.as_bytes(), false).unwrap().data,
                n.data,
                "actual00 {name}"
            );
        }
        let at = text.find("FloatBits=").unwrap() + "FloatBits=".len();
        let bits = u64::from_str_radix(&text[at..at + 16], 16).unwrap();
        let changed = format!(
            "{}{:016x}{}",
            &text[..at],
            bits ^ (1 << 63),
            &text[at + 16..]
        );
        assert_ne!(
            normalize(changed.as_bytes(), false).unwrap().data,
            n.data,
            "actual00 FloatBits sign"
        );
        let map = &n.maps[0];
        assert_eq!(map.entries.len(), 5);
        fn permutations(order: &mut [usize], at: usize, f: &mut impl FnMut(&[usize])) {
            if at == order.len() {
                f(order);
            } else {
                for i in at..order.len() {
                    order.swap(at, i);
                    permutations(order, at + 1, f);
                    order.swap(at, i);
                }
            }
        }
        let mut count = 0;
        permutations(&mut [0, 1, 2, 3, 4], 0, &mut |order| {
            let mut permuted = data[..map.start + 1].to_vec();
            for (slot, &picked) in map.entries.iter().zip(order) {
                permuted.extend_from_slice(&data[slot.lo..slot.key]);
                let entry = &map.entries[picked];
                permuted.extend_from_slice(&data[entry.key..entry.end]);
                permuted.extend_from_slice(&data[slot.end..slot.hi]);
            }
            permuted.extend_from_slice(&data[map.tail.0..map.tail.1]);
            permuted.extend_from_slice(&data[map.end - 1..]);
            let p = normalize(&permuted, false).unwrap();
            assert_eq!(p.data, n.data);
            assert_eq!(p.inverse(), permuted);
            count += 1;
        });
        assert_eq!(count, 120);
        eprintln!("GLOBAL40_ACTUAL00 mutations=3 permutations={count} inverse_exact=true");
    }
}
