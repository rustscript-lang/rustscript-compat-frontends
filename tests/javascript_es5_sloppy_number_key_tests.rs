//! Flow-proved Number-key reads inside accepted non-strict closed functions.
fn reset(machine: &mut vm::Vm) {
    assert!(machine.capture_certification_stats().unwrap().valid);
    machine.reset_for_reuse().unwrap();
    if machine.scope_reset_pending() {
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(matches!(
            machine.poll_reset_for_reuse(&mut cx),
            std::task::Poll::Ready(Ok(()))
        ));
    }
    assert!(!machine.scope_reset_pending());
    assert_eq!(machine.host_context().resource_count(), 0);
    assert!(!machine.capture_certification_stats().unwrap().valid);
    assert!(pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(machine).is_none());
}

fn execute(source: &str) -> vm::Value {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            eprintln!("SLOPPY113_SOURCE {}", serde_json::json!({"source": source}));
            let mut machine =
                pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source)
                    .unwrap_or_else(|error| {
                        panic!("source={source:?} guest_invocations=0 {error:?}")
                    });
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let status = machine.run();
            eprintln!(
                "SLOPPY113_VM source={source:?} status={status:?} stack={:?}",
                machine.stack()
            );
            if status.is_err() {
                eprintln!(
                    "SLOPPY113_ROOT_FAILURE ip={} frames={:?}",
                    machine.ip(),
                    machine.execution_frames()
                );
            }
            assert_eq!(status.unwrap(), vm::VmStatus::Halted);
            assert_eq!(
                (machine.aot_exec_count(), machine.jit_native_exec_count()),
                (0, 0)
            );
            let result = machine.stack().last().unwrap().clone();
            reset(&mut machine);
            eprintln!(
                "SLOPPY113_RESET source={source:?} pending=false resources=0 guest_invocations=1 native=0"
            );
            result
        })
        .unwrap()
        .join()
        .unwrap()
}

fn boxed(source: &str, tag: i64, number: f64, flag: bool) {
    let result = execute(source);
    let vm::Value::Map(map) = result else {
        panic!("full box: {result:?}")
    };
    assert_eq!(map.len(), 5, "{source}");
    for (name, expected) in [
        ("tag", vm::Value::Int(tag)),
        ("number", vm::Value::Float(number)),
        ("flag", vm::Value::Bool(flag)),
        ("ref_slot", vm::Value::Int(0)),
        ("generation", vm::Value::Int(0)),
    ] {
        assert_eq!(
            map.get(&vm::Value::string(name)),
            Some(&expected),
            "{source}"
        );
    }
    if tag == 3 {
        let Some(vm::Value::Float(actual)) = map.get(&vm::Value::string("number")) else {
            panic!("number payload {source}")
        };
        assert_eq!(actual.to_bits(), number.to_bits(), "{source}");
    }
    let expected = match tag {
        0 => serde_json::json!({"kind": "undefined"}),
        2 => serde_json::json!({"kind": "boolean", "value": flag}),
        3 => serde_json::json!({"kind": "number", "value": number}),
        _ => unreachable!(),
    };
    oracle(source, expected);
}

fn oracle(source: &str, expected: serde_json::Value) {
    let version = std::process::Command::new("timeout")
        .args(["15", "node", "--version"])
        .output()
        .unwrap();
    let script = format!(
        "const source={};try{{const v=(0,eval)(source);console.log(JSON.stringify(v===undefined?{{kind:'undefined'}}:{{kind:typeof v,value:v,...(typeof v==='number'?{{negative_zero:Object.is(v,-0)}}:{{}})}}));}}catch(e){{console.log(JSON.stringify({{kind:'throw',value:String(e)}}));}}",
        serde_json::to_string(source).unwrap()
    );
    let output = std::process::Command::new("timeout")
        .args(["15", "node", "-e", &script])
        .output()
        .unwrap();
    eprintln!(
        "SLOPPY113_NODE {}",
        serde_json::json!({
            "source": source,
            "version_rc": version.status.code(),
            "version": String::from_utf8_lossy(&version.stdout),
            "argv": ["timeout", "15", "node", "-e", &script],
            "rc": output.status.code(),
            "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr)
        })
    );
    assert!(
        version.status.success() && output.status.success(),
        "{source}"
    );
    let actual: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    if expected["kind"] == "number" {
        assert_eq!(actual["kind"], "number", "{source}");
        assert_eq!(actual.as_object().unwrap().len(), 3, "{source}");
        let value = expected["value"].as_f64().unwrap();
        assert_eq!(
            actual["negative_zero"],
            value.is_sign_negative() && value == 0.0
        );
        assert_eq!(actual["value"].as_f64(), Some(value), "{source}");
    } else {
        assert_eq!(actual, expected, "{source}");
    }
}

fn refuse(source: &str, diagnostic: &str) {
    let error = match pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
    {
        Ok(_) => panic!("unsupported source executed: {source}"),
        Err(error) => error,
    };
    let text = format!("{error:?}");
    eprintln!(
        "SLOPPY113_REFUSAL {}",
        serde_json::json!({"source": source, "diagnostic": text, "expected": diagnostic, "guest_invocations": 0})
    );
    assert!(text.contains(diagnostic), "{source}: {text}");
    assert!(
        !text.contains("P0 non-strict functions reject possible Number-key members"),
        "obsolete scanner owner: {text}"
    );
}

const EXACT: &str = "function f(){var k=1.5;var o={'1.5':7};return o[k];}f();";
const DEMAND: &str = "var k=1.5;var p={'1.5':9};p[k];";

#[test]
fn sloppy_number113_exact_retired_read() {
    boxed(EXACT, 3, 7.0, false);
}

#[test]
fn sloppy_number113_retired_originals() {
    for source in [
        EXACT,
        &format!("{DEMAND}{EXACT}"),
        "function f(a,a){var k=1.5;var o={'1.5':7};return o[k];}f(1,7);",
        "(function(){var k=1.5;var o={'1.5':7};return o[k];})();",
        "function f(){var key=1.5;var o={\"1.5\":7};return o[key];}var r=f();r;",
    ] {
        boxed(source, 3, 7.0, false);
    }
}

#[test]
fn sloppy_number113_formats() {
    for (source, number) in [
        (
            "function f(){var key=1.5;var o={'1.5':7};return o[key];}f();",
            7.0,
        ),
        (
            "function f(){var key=-0;var o={'0':7};return o[key];}f();",
            7.0,
        ),
        (
            "function f(){var key=NaN;var o={'NaN':11};return o[key];}f();",
            11.0,
        ),
        (
            "function f(){var key=Infinity;var o={'Infinity':12};return o[key];}f();",
            12.0,
        ),
        (
            "function f(){var key=-Infinity;var o={'-Infinity':13};return o[key];}f();",
            13.0,
        ),
        (
            "function f(){var key=1e21;var o={'1e+21':14};return o[key];}f();",
            14.0,
        ),
        (
            "function f(){var key=1e-7;var o={'1e-7':15};return o[key];}f();",
            15.0,
        ),
        (
            "function f(){var key=0;var o={'0':16};return o[key];}f();",
            16.0,
        ),
    ] {
        boxed(source, 3, number, false);
    }
    boxed(
        "function f(){var key=1.5;var o={};return o[key];}f();",
        0,
        0.0,
        false,
    );
}

#[test]
fn sloppy_number113_once_getter_and_facts() {
    for source in [
        "function f(){var n=0;var o={'1.5':7};var r=((n=n===0?1:99)===1?o:o)[(n=n===1?2:99)===2?1.5:0];return r===7&&n===2;}f();",
        "function f(){var key=1.5;var o={get '1.5'(){'use strict';return this;}};var got=o[key];return got===o;}f();",
        "function f(){var key=1.5;key=1.5;var o={'1.5':7};return o[key]===7;}f();",
        "function f(){var key=true?1.5:0;var o={'1.5':7};return o[key]===7;}f();",
        "function f(){var key=1.5;if(false){key=0;}var o={'1.5':7};return o[key]===7;}f();",
    ] {
        boxed(source, 2, 0.0, true);
    }
}

#[test]
fn sloppy_number113_context_restore() {
    boxed(
        "function f(){var k=1.5;var o={'\\61.5':7};return o[k];}f();",
        3,
        7.0,
        false,
    );
    boxed(
        "function f(){function g(){var k=1.5;var o={'1.5':7};return o[k];}return g();}f();",
        3,
        7.0,
        false,
    );
    boxed(
        "function f(){function g(){'use strict';var k=1.5;var o={'1.5':7};return o[k];}return g();}f();",
        3,
        7.0,
        false,
    );
    refuse(
        "function f(){'use strict';var k=1.5;var o={'\\61.5':7};return o[k];}f();",
        "LegacyOctal",
    );
    refuse(
        "function f(){'use strict';var o={x:1,x:7};return o.x;}f();",
        "duplicate",
    );
    for prefix in ["", DEMAND] {
        let source = format!("{prefix}function f(key){{var o={{'1.5':7}};return o[key];}}f(1.5);");
        boxed(&source, 3, 7.0, false);
    }
}

#[test]
fn sloppy_number113_getter_throw() {
    for source in [
        "function f(){var key=1.5;var o={get '1.5'(){throw '\\uD800\\0';}};return o[key];}var verdict=false;try{f();}catch(e){verdict=e==='\\uD800\\0';}verdict;",
        "function f(){var n=0;var key=1.5;var o={get '1.5'(){throw '\\uD800\\0';}};try{var x=o[key];n=9;}catch(e){return e==='\\uD800\\0'&&n===0;}return false;}f();",
        "var verdict=false;try{(function(){var key=1.5;var o={get '1.5'(){throw '\\uD800\\0';}};try{return o[key];}finally{var sink=1;sink;}})();}catch(e){verdict=e==='\\uD800\\0';}verdict;",
    ] {
        boxed(source, 2, 0.0, true);
    }
}

const SLOPPY113_CASES: &[(&str, &str)] = &[
    ("function f(){return this;}f();", "own this"),
    (
        "function f(){return arguments;}f();",
        "arguments/eval observations",
    ),
    (
        "function f(){return eval;}f();",
        "arguments/eval observations",
    ),
    (
        "function f(arguments){return 7;}f();",
        "arguments/eval formals",
    ),
    ("function f(eval){return 7;}f();", "arguments/eval formals"),
    (
        "var k=1.5;function f(){var o={'1.5':7};return o[k];}f();",
        "P4 free-variable capture",
    ),
    (
        "function f(){var key=[1];var o={};return o[key];}f();",
        "primitive computed keys reject object ToPrimitive",
    ),
    ("function f(){return f;}f();", "P4 free-variable capture"),
    (
        "var f=function self(){return 7;};f();",
        "anonymous function expressions",
    ),
];
const SLOPPY113_POSITIVES: &[(&str, i64, f64)] = &[
    (
        "function f(key){var o={'1.5':7};return o[key];}f(1.5);",
        3,
        7.0,
    ),
    (
        "function f(){var o={};try{throw 1.5;}catch(key){return o[key];}}f();",
        0,
        0.0,
    ),
    (
        "function f(){function g(){return 1.5;}var key=g();var o={'1.5':7};return o[key];}f();",
        3,
        7.0,
    ),
    (
        "function f(){var key=true;var o={};return o[key];}f();",
        0,
        0.0,
    ),
    (
        "function f(){var key=1.5;if(true){key=true;}var o={};return o[key];}f();",
        0,
        0.0,
    ),
    (
        "function f(){var k=1;var o={};o[k]=7;return 7;}f();",
        3,
        7.0,
    ),
    (
        "function f(){var k=1;var o={'1':7};delete o[k];return 7;}f();",
        3,
        7.0,
    ),
    (
        "function f(){var k=1.5;var o={'1.5':function(){return 7;}};return o[k]();}f();",
        3,
        7.0,
    ),
];
/// Original `sloppy_number113_refusals` covers every shard below.
const SLOPPY113_SHARDS: &[(&str, usize, usize)] = &[
    ("sloppy_number113_refusals", 0, 8),
    ("sloppy_number113_refusals_01", 8, 16),
    ("sloppy_number113_refusals_02", 16, 24),
    ("sloppy_number113_refusals_03", 24, 32),
    ("sloppy_number113_refusals_04", 32, 40),
];

fn sloppy113_row(index: usize) {
    let prefix = if index < 20 { "" } else { DEMAND };
    let within = index % 20;
    if within < 10 {
        let (source, diagnostic) = SLOPPY113_CASES[within];
        refuse(&format!("{prefix}{source}"), diagnostic);
    } else if within < 18 {
        let (source, tag, number) = SLOPPY113_POSITIVES[within - 10];
        boxed(&format!("{prefix}{source}"), tag, number, false);
    } else if within == 18 {
        let params = vec!["a"; 65].join(",");
        refuse(
            &format!("{prefix}function f({params}){{return 7;}}f();"),
            "64 formal",
        );
    } else {
        let args = vec!["1"; 65].join(",");
        refuse(
            &format!("{prefix}function f(){{return 7;}}f({args});"),
            "64 actual",
        );
    }
}

fn sloppy113_shard(name: &str) {
    let mut covered = [false; 40];
    for (_, start, end) in SLOPPY113_SHARDS {
        assert!(*end - *start <= 8 && *start < *end && *end <= 40);
        for (index, seen) in covered.iter_mut().enumerate().take(*end).skip(*start) {
            assert!(!*seen, "duplicate sloppy113 row {index}");
            *seen = true;
        }
    }
    assert!(covered.iter().all(|seen| *seen), "sloppy113 omission");
    let (start, end) = SLOPPY113_SHARDS
        .iter()
        .find(|(id, _, _)| *id == name)
        .map(|(_, start, end)| (*start, *end))
        .expect(name);
    for index in start..end {
        sloppy113_row(index);
    }
}

#[test]
fn sloppy_number113_refusals() {
    sloppy113_shard("sloppy_number113_refusals");
}

#[test]
fn sloppy_number113_refusals_01() {
    sloppy113_shard("sloppy_number113_refusals_01");
}

#[test]
fn sloppy_number113_refusals_02() {
    sloppy113_shard("sloppy_number113_refusals_02");
}

#[test]
fn sloppy_number113_refusals_03() {
    sloppy113_shard("sloppy_number113_refusals_03");
}

#[test]
fn sloppy_number113_refusals_04() {
    sloppy113_shard("sloppy_number113_refusals_04");
}

const ERROR_PREFIX: &str = "function $ERROR(message) { throw message; }\n";
const FILE_SUFFIX: &str = "\n;1;\n";

#[test]
fn sloppy_number113_file_cookie() {
    let body = "function f(){var k=1.5;var o={'1.5':7};return o[k];}if(f()!==7){$ERROR('SLOPPY113_FAIL');}";
    let source = format!("{ERROR_PREFIX}{body}{FILE_SUFFIX}");
    let oracle_source = source.clone();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let path = std::env::temp_dir().join(format!(
                "sloppy113-file-{}-{}.js",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::write(&path, source.as_bytes()).unwrap();
            let observed =
                pd_vm_compat_frontends::javascript::corpus::observe_javascript_es5_corpus_file(
                    &path,
                );
            std::fs::remove_file(&path).unwrap();
            let done = observed.unwrap_or_else(|error| panic!("file guest failure: {error:?}"));
            assert_eq!(done.cookie, 1);
            assert_eq!((done.aot_exec_count, done.jit_native_exec_count), (0, 0));
            assert!(done.certification_valid_before_reset && !done.certification_valid_after_reset);
            assert_eq!(done.resources_after_reset, 0);
            assert!(done.owned_arrays_retired);
            assert!(!path.exists());
            eprintln!(
                "SLOPPY113_FILE cookie=1 native=0 cert_retired=true resources=0 removed=true"
            );
        })
        .unwrap()
        .join()
        .unwrap();
    oracle(
        &oracle_source,
        serde_json::json!({"kind": "number", "value": 1}),
    );
}

#[path = "javascript_es5/arity_observer.rs"]
mod arity_observer;
#[path = "javascript_es5/object_observer.rs"]
mod object_observer;

struct RootsObserver {
    string: bool,
}
static OBJECT_ROOTS: RootsObserver = RootsObserver { string: false };
static STRING_ROOTS: RootsObserver = RootsObserver { string: true };

fn call(index: u16, args: Vec<vm::Expr>) -> vm::Expr {
    vm::Expr::Call(index, vec![], args, None, None)
}
fn get(slot: u16, field: &str) -> vm::Expr {
    call(
        vm::BuiltinFunction::Get.call_index(),
        vec![
            vm::Expr::Borrow(Box::new(vm::Expr::Var(slot))),
            vm::Expr::String(field.into()),
        ],
    )
}
fn num(expr: vm::Expr) -> vm::Expr {
    vm::Expr::Div(Box::new(expr), Box::new(vm::Expr::Float(1.0)))
}
fn assertion(condition: vm::Expr) -> vm::Stmt {
    vm::Stmt::Expr {
        expr: call(vm::BuiltinFunction::Assert.call_index(), vec![condition]),
        line: 1,
    }
}
fn helper(ir: &vm::FrontendIr, name: &str) -> u16 {
    ir.functions
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("missing {name}"))
        .index
}

impl vm::SourcePlugin for RootsObserver {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut d = vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
        d.id = vm::FrontendId::new("sloppy113-roots");
        d
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        use vm::{Expr as E, Stmt as S, TypeSchema as T};
        let observer = object_observer::Observer {
            units: self.string.then_some(2),
            force_gc: true,
            ..Default::default()
        };
        let mut ir = vm::SourcePlugin::parse_source(&observer, source, context)?;
        let collect = helper(&ir, "__compat_es5_heap_collect");
        let observe = helper(&ir, "__compat_es5_heap_observe");
        let checked = helper(&ir, "__compat_es5_checked_base");
        let read = helper(&ir, "__compat_ref_read");
        let enter = helper(&ir, "__compat_es5_root_enter");
        let throw_error = helper(&ir, "typescript::throw_error");
        let functions: Vec<_> = ir
            .functions
            .iter()
            .filter(|f| f.name.starts_with("__compat_es5_fn_"))
            .map(|f| (f.index, f.name.clone()))
            .collect();
        assert_eq!(functions.len(), 1, "user functions: {functions:?}");
        for (index, _) in functions {
            let first = u16::try_from(ir.locals).unwrap();
            ir.locals += 5;
            let (payload, before, ack, base) = (first, first + 1, first + 2, first + 3);
            let f = ir.function_impls.get_mut(&index).unwrap();
            let carrier = f.param_slots[0];
            let frame = f
                .body_stmts
                .iter()
                .find_map(|s| match s {
                    S::Let {
                        index,
                        expr: E::Call(id, _, _, _, _),
                        ..
                    } if *id == enter => Some(*index),
                    _ => None,
                })
                .unwrap();
            let (at, completion) = f
                .body_stmts
                .iter()
                .enumerate()
                .find_map(|(at, s)| match s {
                    S::Let {
                        expr: E::ResultConstruct { payload, .. },
                        ..
                    } => match payload.as_ref() {
                        E::Var(c) => Some((at, *c)),
                        _ => None,
                    },
                    _ => None,
                })
                .expect("real Result producer before frame leave");
            let eq = |a: E, b: E| E::Eq(Box::new(num(a)), Box::new(num(b)));
            let observed =
                |selector| call(observe, vec![E::Var(carrier), E::Int(selector), E::Int(0)]);
            let checks = vec![
                S::Let {
                    index: payload,
                    declared_schema: Some(T::Named("__CompatEs5Primitive".into(), vec![])),
                    expr: get(completion, "value"),
                    line: 1,
                },
                assertion(eq(
                    get(payload, "tag"),
                    E::Int(if self.string { 4 } else { 5 }),
                )),
                assertion(eq(
                    call(read, vec![E::Var(carrier), E::Float(3.0)]),
                    E::Var(frame),
                )),
                S::Let {
                    index: before,
                    declared_schema: Some(T::Int),
                    expr: observed(5),
                    line: 1,
                },
                assertion(E::Gt(Box::new(E::Var(before)), Box::new(E::Int(0)))),
                S::Let {
                    index: ack,
                    declared_schema: Some(T::Int),
                    expr: call(collect, vec![E::Var(carrier)]),
                    line: 1,
                },
                assertion(eq(
                    observed(5),
                    E::Add(Box::new(E::Var(before)), Box::new(E::Int(1))),
                )),
                S::Let {
                    index: base,
                    declared_schema: Some(T::Int),
                    expr: call(
                        checked,
                        vec![
                            E::Var(carrier),
                            get(payload, "ref_slot"),
                            get(payload, "generation"),
                        ],
                    ),
                    line: 1,
                },
                assertion(E::Gt(Box::new(E::Var(base)), Box::new(E::Int(0)))),
                assertion(eq(get(payload, "number"), E::Float(0.0))),
                assertion(E::Eq(
                    Box::new(get(payload, "flag")),
                    Box::new(E::Bool(false)),
                )),
                assertion(eq(observed(8), E::Int(0))),
            ];
            let checks = checks.into_iter().enumerate().map(|(check, stmt)| {
                if let S::Expr {
                    expr: E::Call(id, _, args, _, _),
                    ..
                } = &stmt
                    && *id == vm::BuiltinFunction::Assert.call_index()
                {
                    return S::IfElse {
                        condition: args[0].clone(),
                        then_branch: vec![],
                        else_branch: vec![S::Let {
                            index: first + 4,
                            declared_schema: Some(T::Null),
                            expr: call(
                                throw_error,
                                vec![E::String(format!(
                                    "sloppy113 function={index} root check={check}"
                                ))],
                            ),
                            line: 1,
                        }],
                        line: 1,
                    };
                }
                stmt
            });
            f.body_stmts.splice(at..at, checks);
            eprintln!(
                "SLOPPY113_ROOT_SITE function={index} carrier={carrier} frame={frame} completion={completion} gc_before_NoAlloc=true"
            );
        }
        let result = ir.local_bindings.iter().find(|(n, _)| n == "r").unwrap().1;
        let last = ir.local_bindings.iter().find(|(n, _)| n == "o").unwrap().1;
        for name in ["tag", "ref_slot", "generation"] {
            ir.stmts.push(assertion(E::Eq(
                Box::new(num(get(result, name))),
                Box::new(num(get(last, name))),
            )));
        }
        Ok(ir)
    }
}

fn probe_vm(source: &str, observer: &'static dyn vm::SourcePlugin, id: &'static str) -> vm::Vm {
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let issued = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new(id)),
        vm::CompileSourceFileOptions::new()
            .with_host_api_catalog(catalog.clone())
            .with_source_plugin(observer),
    )
    .unwrap();
    let mut machine = issued.into_vm().unwrap();
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..*machine.jit_config()
    });
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        Default::default(),
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
    for name in [
        "math::floor",
        "math::copysign",
        "math::sqrt",
        "math::round",
        "math::powf",
        "math::is_nan",
        "math::is_finite",
        "bytes::from_utf8",
    ] {
        registry.allow_builtin(name).unwrap();
    }
    registry.bind_vm_cached(&mut machine).unwrap();
    machine.set_fuel(10_000_000);
    machine
}

fn managed(source: &str, observer: &'static RootsObserver) {
    let source = source.to_owned();
    let string = observer.string;
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let mut machine = probe_vm(&source, observer, "sloppy113-roots");
            let status = machine.run();
            eprintln!(
                "SLOPPY113_MANAGED_VM source={source:?} status={status:?} stack={:?}",
                machine.stack()
            );
            assert_eq!(status.unwrap(), vm::VmStatus::Halted);
            assert_eq!(
                (machine.aot_exec_count(), machine.jit_native_exec_count()),
                (0, 0)
            );
            let stack = machine.stack();
            let at = stack
                .iter()
                .rposition(|v| *v == vm::Value::string("P7_OBSERVED"))
                .unwrap()
                + 1;
            let vm::Value::Map(value) = &stack[at] else {
                panic!("actual full managed box")
            };
            assert_eq!(value.len(), 5);
            assert_eq!(
                value.get(&vm::Value::string("tag")),
                Some(&vm::Value::Int(if string { 4 } else { 5 }))
            );
            assert_eq!(
                value.get(&vm::Value::string("number")),
                Some(&vm::Value::Float(0.0))
            );
            assert_eq!(
                value.get(&vm::Value::string("flag")),
                Some(&vm::Value::Bool(false))
            );
            for key in ["ref_slot", "generation"] {
                assert!(
                    matches!(value.get(&vm::Value::string(key)), Some(vm::Value::Int(n)) if *n > 0)
                );
            }
            if string {
                assert_eq!(
                    &stack[at + 1..at + 4],
                    &[
                        vm::Value::Float(2.0),
                        vm::Value::Float(65.0),
                        vm::Value::Float(66.0)
                    ]
                );
            }
            reset(&mut machine);
            eprintln!("SLOPPY113_MANAGED_RESET resources=0 arrays_retired=true");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn sloppy_number113_managed_object() {
    let source = "var o={x:7};var r=(function(a){var k=1.5;var p={'1.5':a};return p[k];})(o);if(!(r===o&&r.x===7)){throw 'identity';}r;";
    managed(source, &OBJECT_ROOTS);
    oracle(
        source,
        serde_json::json!({"kind": "object", "value": {"x": 7}}),
    );
}

#[test]
fn sloppy_number113_managed_string() {
    let source = "var o;var r=(function(a){var k=1.5;var p={'1.5':a};return p[k];})(o='AB');if(!(r===o)){throw 'identity';}r;";
    managed(source, &STRING_ROOTS);
    oracle(source, serde_json::json!({"kind": "string", "value": "AB"}));
}

#[test]
fn sloppy_number113_throw_collection() {
    let source = "var r=false;try{try{(function(){var key=1.5;var o={get '1.5'(){throw '\\uD800\\0';}};return o[key];})();}finally{var sink={s:'later'};}}catch(e){r=e==='\\uD800\\0';}r;";
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            static OBSERVER: std::sync::LazyLock<arity_observer::Observer> =
                std::sync::LazyLock::new(|| arity_observer::Observer {
                    reads: vec!["r".into()],
                    force_gc: true,
                    stats: true,
                    ..Default::default()
                });
            let mut machine = probe_vm(source, &*OBSERVER, "javascript-es5-arity-observe");
            assert_eq!(machine.run().unwrap(), vm::VmStatus::Halted);
            assert_eq!(
                (machine.aot_exec_count(), machine.jit_native_exec_count()),
                (0, 0)
            );
            let stack = machine.stack();
            let at = stack.len() - 4;
            let vm::Value::Map(v) = &stack[at] else {
                panic!("full Boolean box {stack:?}")
            };
            assert_eq!(v.len(), 5);
            for (k, x) in [
                ("tag", vm::Value::Int(2)),
                ("number", vm::Value::Float(0.0)),
                ("flag", vm::Value::Bool(true)),
                ("ref_slot", vm::Value::Int(0)),
                ("generation", vm::Value::Int(0)),
            ] {
                assert_eq!(v.get(&vm::Value::string(k)), Some(&x));
            }
            assert!(
                matches!(stack.last(), Some(vm::Value::Int(n)) if *n > 0),
                "real collections {stack:?}"
            );
            eprintln!("SLOPPY113_PENDING_ROOT actual_collection=true caught_payload=true");
            reset(&mut machine);
        })
        .unwrap()
        .join()
        .unwrap();
    oracle(
        source,
        serde_json::json!({"kind": "boolean", "value": true}),
    );
}

#[test]
fn sloppy_number113_object_key_boundary_rows() {
    assert_eq!(
        SLOPPY113_CASES[6].0,
        "function f(){var key=[1];var o={};return o[key];}f();"
    );
    sloppy113_row(6);
    sloppy113_row(26);
}
