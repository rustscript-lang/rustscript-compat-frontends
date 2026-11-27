//! Bounded closed non-strict activations through the certified public VM.
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
            eprintln!("SLOPPY107_SOURCE {}", serde_json::json!({"source":source}));
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
                "SLOPPY107_VM source={source:?} status={status:?} stack={:?}",
                machine.stack()
            );
            if status.is_err() {
                eprintln!(
                    "SLOPPY107_ROOT_FAILURE ip={} frames={:?}",
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
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!(
                "SLOPPY107_RESET source={source:?} pending=false resources=0 guest_invocations=1"
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
    assert_eq!(map.len(), 5);
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
    let expected = match tag {
        0 => serde_json::json!({"kind":"undefined"}),
        2 => serde_json::json!({"kind":"boolean","value":flag}),
        3 => serde_json::json!({"kind":"number","value":number}),
        _ => unreachable!(),
    };
    oracle(source, expected);
}

#[test]
fn sloppy107_public_declaration() {
    boxed("function f(){return 7;}f();", 3, 7.0, false);
}

fn oracle(source: &str, expected: serde_json::Value) {
    let version = std::process::Command::new("timeout")
        .args(["15", "node", "--version"])
        .output()
        .unwrap();
    let script = format!(
        "const source={};try{{const v=(0,eval)(source);console.log(JSON.stringify(v===undefined?{{kind:'undefined'}}:{{kind:typeof v,value:v}}));}}catch(e){{console.log(JSON.stringify({{kind:'throw',value:e}}));}}",
        serde_json::to_string(source).unwrap()
    );
    let output = std::process::Command::new("timeout")
        .args(["15", "node", "-e", &script])
        .output()
        .unwrap();
    eprintln!(
        "SLOPPY107_NODE {}",
        serde_json::json!({
            "source":source,"version_rc":version.status.code(),"version":String::from_utf8_lossy(&version.stdout),
            "argv":["timeout","15","node","-e",&script],"rc":output.status.code(),
            "stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr)
        })
    );
    assert!(version.status.success() && output.status.success());
    let actual: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    if expected["kind"] == "number" {
        assert_eq!(actual["kind"], expected["kind"]);
        assert_eq!(actual.as_object().unwrap().len(), 2);
        assert_eq!(
            actual["value"].as_f64(),
            expected["value"].as_f64(),
            "{source}"
        );
    } else {
        assert_eq!(actual, expected, "{source}");
    }
}

#[test]
fn sloppy107_public_alias_arithmetic_formals() {
    for source in [
        "var f=function(){return 7;};var alias=f;alias();",
        "function f(){var a=3;var b=4;return a+b;}f();",
        "function f(a,b){var a;return a;}f(7,9);",
        "function f(){var o={x:7};return o.x;}f();",
    ] {
        boxed(source, 3, 7.0, false);
    }
}

#[test]
fn sloppy107_public_missing_empty_and_return() {
    for source in [
        "function f(a,b){return b;}f(7);",
        "function f(){}f();",
        "function f(){return;}f();",
    ] {
        boxed(source, 0, 0.0, false);
    }
}

#[test]
fn sloppy107_public_managed_throw_nesting_methods_hoist() {
    for source in [
        "function f(){return 'hello';}f()==='hello';",
        "function f(a){throw a;}var caught=false;try{f('payload');}catch(e){caught=e==='payload';}caught;",
        "function f(){function g(){return 7;}return g();}f()===7;",
        "var o={f:function(){return 7;}};var alias=o.f;o.f()===7&&alias()===7;",
        "f()===7;function f(){return 7;}",
        "function f(a,b){return a===b;}var o={x:7};f(o,o);",
    ] {
        boxed(source, 2, 0.0, true);
    }
}

#[test]
fn sloppy107_original_refusals_retired() {
    for source in [
        "function f(){switch(1){case 1:5;}}f();",
        "function f(){switch(1){case 1:'use strict';}}f();",
        "function f(){'banner';switch(1){case 1:break;}}f();",
    ] {
        boxed(source, 0, 0.0, false);
    }
}

#[test]
fn sloppy107_context_sloppy_literals_and_nonobservations() {
    for source in [
        "function f(){return '\\141';}f()==='a';",
        "function f(){var o={'\\141':1,a:7};return o.a;}f()===7;",
        "function f(){var o={x:1,x:7};return o.x;}f()===7;",
        "function f(){var o={arguments:7,eval:9};return o.arguments===7&&o.eval===9&&'this arguments eval'==='this arguments eval';}f();",
        "function f(){arguments:{break arguments;}eval:{break eval;}return 7;}f()===7;",
        "function f(){'use\\x20strict';return '\\141';}f()==='a';",
    ] {
        boxed(source, 2, 0.0, true);
    }
}

#[test]
fn sloppy107_context_nested_strict_and_restore() {
    for source in [
        "function f(){function g(){'use strict';return this;}return g()===undefined;}f();",
        "function f(){function g(){'use strict';return this;}return 7;}f();var o={'\\141':9};o.a===9;",
        "function f(){'use strict';return this;}f()===undefined;",
        "'use strict';function f(){return this;}f()===undefined;",
        "function f(){function g(){'use strict';var k=1.5;var o={'1.5':7};return o[k];}return g();}f()===7;",
    ] {
        boxed(source, 2, 0.0, true);
    }
}

fn refuse(source: &str, diagnostic: &str) {
    let error = match pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
    {
        Ok(_) => panic!("must refuse source={source:?}"),
        Err(error) => error,
    };
    let text = format!("{error:?}");
    eprintln!(
        "SLOPPY107_REFUSAL {}",
        serde_json::json!({"source":source,"diagnostic":text,"expected":diagnostic,"guest_invocations":0,"node_grammar_oracle":false})
    );
    assert!(text.contains(diagnostic), "{source}: {text}");
}

#[test]
fn sloppy107_refusals_own_activation() {
    let bodies = [
        "return this;",
        "if(this){return 7;}return 0;",
        "while(this){break;}",
        "do{break;}while(this);",
        "for(var i=this;false;){break;}",
        "for(;this;){break;}",
        "for(;false;this){break;}",
        "try{return this;}catch(e){return 0;}",
        "try{throw 1;}catch(e){return this;}",
        "try{return 7;}finally{this;}",
        "switch(this){case 1:return 7;}",
        "switch(1){case this:return 7;}",
        "return [this];",
        "var o={x:this};return o;",
        "return !this;",
        "return this===undefined;",
        "return true?7:this;",
        "return (7,this);",
        "var x; x=this;return x;",
        "var o={};o.x=this;return 7;",
        "return arguments;",
        "return arguments[0];",
        "return typeof arguments;",
        "return eval;",
        "eval('1');",
        "var x; x=eval;return 7;",
        "eval=7;return 7;",
        "arguments=7;return 7;",
        "return \\u0065val;",
    ];
    for prefix in ["", "var k=1.5;var p={'1.5':9};p[k];"] {
        for body in bodies {
            let source = format!("{prefix}function f(){{{body}}}f();");
            refuse(&source, "P0 non-strict functions reject");
        }
    }
}

const SLOPPY107_PREFIXES: &[&str] = &["", "var k=1.5;var p={'1.5':9};p[k];"];
const SLOPPY107_REFUSALS: &[(&str, &str)] = &[
    (
        "function f(arguments){return 7;}f();",
        "arguments/eval formals",
    ),
    ("function f(eval){return 7;}f();", "arguments/eval formals"),
    (
        "function f(){function g(){return this;}return 7;}f();",
        "own this",
    ),
    (
        "function f(){function g(){return arguments;}return 7;}f();",
        "arguments/eval observations",
    ),
    ("function f(){return this.x;}f();", "own this"),
];
const SLOPPY107_BOXED: &str = "function f(){var k=1;var o={};o[k]=7;return 7;}f();";
/// Original `sloppy107_refusals_formals_keys_and_nested` covers both shards.
const SLOPPY107_SHARDS: &[(&str, usize, usize)] = &[
    ("sloppy107_refusals_formals_keys_and_nested", 0, 6),
    ("sloppy107_refusals_formals_keys_and_nested_01", 6, 12),
];

fn sloppy107_row(index: usize) {
    let prefix = SLOPPY107_PREFIXES[index / 6];
    let item = index % 6;
    if item < 5 {
        let (source, diagnostic) = SLOPPY107_REFUSALS[item];
        refuse(&format!("{prefix}{source}"), diagnostic);
    } else {
        boxed(&format!("{prefix}{SLOPPY107_BOXED}"), 3, 7.0, false);
    }
}

fn sloppy107_shard(name: &str) {
    let mut covered = [false; 12];
    for (_, start, end) in SLOPPY107_SHARDS {
        assert!(*end - *start <= 8 && *start < *end && *end <= 12);
        for (index, seen) in covered.iter_mut().enumerate().take(*end).skip(*start) {
            assert!(!*seen, "duplicate sloppy107 row {index}");
            *seen = true;
        }
    }
    assert!(covered.iter().all(|seen| *seen), "sloppy107 omission");
    let (start, end) = SLOPPY107_SHARDS
        .iter()
        .find(|(id, _, _)| *id == name)
        .map(|(_, start, end)| (*start, *end))
        .expect(name);
    for index in start..end {
        sloppy107_row(index);
    }
}

#[test]
fn sloppy107_refusals_formals_keys_and_nested() {
    sloppy107_shard("sloppy107_refusals_formals_keys_and_nested");
}

#[test]
fn sloppy107_refusals_formals_keys_and_nested_01() {
    sloppy107_shard("sloppy107_refusals_formals_keys_and_nested_01");
}

#[test]
fn sloppy107_refusals_existing_closed_boundaries() {
    for (source, diagnostic) in [
        (
            "var x=7;function f(){return x;}f();",
            "P4 free-variable capture",
        ),
        (
            "var f=function self(){return 7;};f();",
            "anonymous function expressions",
        ),
        ("function f(){return f;}f();", "P4 free-variable capture"),
        ("function f(){with({x:7}){return x;}}f();", "with"),
        (
            "function f(){for(var k in {}){}return 7;}f();",
            "unsupported JavaScript ES5 P0 statement",
        ),
        ("function f(a){return a+1;}f(6);", "arithmetic"),
    ] {
        refuse(source, diagnostic);
    }
}

#[test]
fn sloppy107_context_strict_early_errors() {
    for source in [
        "function f(){'use strict';return '\\141';}f();",
        "'use strict';function f(){return '\\141';}f();",
        "function f(){'use strict';var o={'\\141':7};return o.a;}f();",
        "'use strict';function f(){var o={'\\141':7};return o.a;}f();",
    ] {
        refuse(source, "LegacyOctal");
    }
    for source in [
        "function f(){'use strict';var o={x:1,x:7};return o.x;}f();",
        "'use strict';function f(){var o={x:1,x:7};return o.x;}f();",
    ] {
        refuse(source, "duplicate");
    }
}

const ERROR_PREFIX: &str = "function $ERROR(message) { throw message; }\n";
const FILE_SUFFIX: &str = "\n;1;\n";
fn file_observe(id: &'static str, body: &str, throwing: bool) {
    use pd_vm_compat_frontends::javascript::corpus::{
        CorpusFileError, observe_javascript_es5_corpus_file,
    };
    let source = format!("{ERROR_PREFIX}{body}{FILE_SUFFIX}");
    let oracle_source = source.clone();
    std::thread::Builder::new().stack_size(33_554_432).spawn(move || {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path=std::env::temp_dir().join(format!("sloppy107-{id}-{}-{}.js",std::process::id(),NEXT.fetch_add(1,std::sync::atomic::Ordering::Relaxed)));
        use std::io::Write;
        let mut file=std::fs::OpenOptions::new().create_new(true).write(true).open(&path).unwrap();
        file.write_all(source.as_bytes()).unwrap();
        eprintln!("SLOPPY107_FILE_SOURCE {}",serde_json::json!({"id":id,"source":source,"path":path,"adapter":"original104_non_strict"}));
        let observed=observe_javascript_es5_corpus_file(&path);
        std::fs::remove_file(&path).unwrap();
        match observed {
            Ok(done) => {
                assert!(!throwing);
                assert_eq!(done.cookie,1);
                assert_eq!((done.aot_exec_count,done.jit_native_exec_count),(0,0));
                assert!(done.certification_valid_before_reset && !done.certification_valid_after_reset);
                assert_eq!(done.resources_after_reset,0);
                assert!(done.owned_arrays_retired);
                eprintln!("SLOPPY107_FILE_RESULT {}",serde_json::json!({"id":id,"cookie":done.cookie,"aot":done.aot_exec_count,"jit":done.jit_native_exec_count,"cert_before":done.certification_valid_before_reset,"cert_after":done.certification_valid_after_reset,"resources_after":done.resources_after_reset,"arrays_retired":done.owned_arrays_retired,"guest_invocations":1,"removed":!path.exists()}));
            }
            Err(CorpusFileError::Execution{error,rendered}) => {
                assert!(throwing);
                assert!(matches!(error.as_ref(),vm::VmError::HostError(message) if message=="uncaught JavaScript throw"),"{error:?}");
                eprintln!("SLOPPY107_FILE_THROW {}",serde_json::json!({"id":id,"source":source,"error":format!("{error:?}"),"rendered":rendered,"guest_invocations":1,"removed":!path.exists()}));
            }
            other => panic!("unexpected {other:?}"),
        }
    }).unwrap().join().unwrap();
    oracle(
        &oracle_source,
        if throwing {
            serde_json::json!({"kind":"throw","value":"TEST262104_FAIL_SENTINEL"})
        } else {
            serde_json::json!({"kind":"number","value":1})
        },
    );
}

#[test]
#[allow(non_snake_case)]
fn sloppy107_old_ERROR_adapter_trap() {
    file_observe("error_trap", "$ERROR(\"TEST262104_FAIL_SENTINEL\");", true);
}

macro_rules! old_body_variant {
    ($name:ident, $id:literal) => {
        #[test]
        #[allow(non_snake_case)]
        fn $name() {
            file_observe(
                $id,
                include_str!(concat!("fixtures/test262104/", $id, ".js")),
                false,
            );
        }
    };
}
old_body_variant!(sloppy107_old_ERROR_adapter_encode_hash, "S15.1.3.4_A3.3_T1");
old_body_variant!(sloppy107_old_ERROR_adapter_encode_ascii, "S15.1.3.4_A4_T1");
old_body_variant!(sloppy107_old_ERROR_adapter_encode_lines, "S15.1.3.4_A4_T3");
old_body_variant!(sloppy107_old_ERROR_adapter_encode_urls, "S15.1.3.4_A4_T4");
old_body_variant!(sloppy107_old_ERROR_adapter_decode_ascii, "S15.1.3.2_A4_T1");
old_body_variant!(
    sloppy107_old_ERROR_adapter_decode_cyrillic,
    "S15.1.3.2_A4_T2"
);
old_body_variant!(sloppy107_old_ERROR_adapter_decode_lines, "S15.1.3.2_A4_T3");
old_body_variant!(sloppy107_old_ERROR_adapter_decode_urls, "S15.1.3.2_A4_T4");

#[path = "javascript_es5/object_observer.rs"]
mod object_observer;
#[path = "javascript_es5/arity_observer.rs"]
#[allow(dead_code)]
mod pending_observer;

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
        d.id = vm::FrontendId::new("sloppy107-roots");
        d
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        use vm::{Expr as E, Stmt as S, TypeSchema as T};
        if self.string {
            // The existing in-finally witness checks the actual saved caller
            // payload around natural GC. Do not collect after Return has
            // begun the established NoAlloc handoff.
            let observer = pending_observer::FinallyWindowObserver {
                inner: pending_observer::Observer {
                    reads: vec!["s".into()],
                    string: Some(("s".into(), 2)),
                    extra_selectors: vec![0, 4, 8, 12],
                    ..Default::default()
                },
                missing_b: false,
            };
            return vm::SourcePlugin::parse_source(&observer, source, context);
        }
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
            .map(|f| f.index)
            .collect();
        assert_eq!(functions.len(), if self.string { 2 } else { 1 });
        for index in functions {
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
                                    "sloppy107 function={index} root check={check}"
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
                "SLOPPY107_ROOT_SITE source={source:?} function={index} carrier={carrier} active_frame={frame} completion={completion} payload={payload} gc_before_NoAlloc=true checked_current_generation=true"
            );
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
    for name in ["math::floor", "math::copysign"] {
        registry.allow_builtin(name).unwrap();
    }
    registry.bind_vm_cached(&mut machine).unwrap();
    machine.set_fuel(10_000_000);
    machine
}

struct FlagObserver;
static FLAGS: FlagObserver = FlagObserver;
impl vm::SourcePlugin for FlagObserver {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut d = vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
        d.id = vm::FrontendId::new("sloppy107-flags");
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
        let make = helper(&ir, "__compat_ref_make");
        let checked = helper(&ir, "__compat_es5_checked_base");
        let read = helper(&ir, "__compat_ref_read");
        let carrier = ir
            .stmts
            .iter()
            .find_map(|s| match s {
                S::Let {
                    index,
                    expr: E::Call(id, _, _, _, _),
                    ..
                } if *id == make => Some(*index),
                _ => None,
            })
            .unwrap();
        let function = ir
            .local_bindings
            .iter()
            .find(|(name, _)| name == "f")
            .unwrap()
            .1;
        let base = u16::try_from(ir.locals).unwrap();
        ir.locals += 1;
        ir.stmts.push(S::Let {
            index: base,
            declared_schema: Some(T::Int),
            expr: call(
                checked,
                vec![
                    E::Var(carrier),
                    get(function, "ref_slot"),
                    get(function, "generation"),
                ],
            ),
            line: 1,
        });
        ir.stmts.push(S::Expr {
            expr: call(
                read,
                vec![
                    E::Var(carrier),
                    num(E::Add(Box::new(E::Var(base)), Box::new(E::Int(8)))),
                ],
            ),
            line: 1,
        });
        Ok(ir)
    }
}
#[test]
fn sloppy107_context_actual_function_strict_flags() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(|| {
            for (source, expected) in [
                ("function f(){return 7;}f;", 0.0),
                ("function f(){'use\\x20strict';return 7;}f;", 0.0),
                ("function f(){'use strict';return 7;}f;", 1.0),
                ("'use strict';function f(){return 7;}f;", 1.0),
            ] {
                let mut machine = probe_vm(source, &FLAGS, "sloppy107-flags");
                assert_eq!(machine.run().unwrap(), vm::VmStatus::Halted);
                eprintln!(
                    "SLOPPY107_FLAG source={source:?} actual={:?} expected={expected}",
                    machine.stack().last()
                );
                assert_eq!(machine.stack().last(), Some(&vm::Value::Float(expected)));
                assert_eq!(
                    (machine.aot_exec_count(), machine.jit_native_exec_count()),
                    (0, 0)
                );
                reset(&mut machine);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

fn root_run(source: &str, observer: &'static RootsObserver) {
    let source = source.to_owned();
    std::thread::Builder::new().stack_size(33_554_432).spawn(move|| {
        let mut machine=probe_vm(&source,observer,"sloppy107-roots");
        let status=machine.run();
        eprintln!("SLOPPY107_ROOT_VM source={source:?} status={status:?} stack={:?}",machine.stack());
        if status.is_err() {eprintln!("SLOPPY107_ROOT_FAILURE ip={} frames={:?} regions={:?}",machine.ip(),machine.execution_frames(),machine.program().function_regions);}
        assert_eq!(status.unwrap(),vm::VmStatus::Halted);
        assert_eq!((machine.aot_exec_count(),machine.jit_native_exec_count()),(0,0));
        assert!(machine.capture_certification_stats().unwrap().valid);
        let stack=machine.stack();
        let at=if observer.string {stack.len()-4} else {stack.iter().rposition(|v|*v==vm::Value::string("P7_OBSERVED")).unwrap()+1};
        let vm::Value::Map(value)=&stack[at] else {panic!("actual full box")};
        assert_eq!(value.len(),5);
        assert_eq!(value.get(&vm::Value::string("tag")),Some(&vm::Value::Int(if observer.string{4}else{3})));
        assert_eq!(value.get(&vm::Value::string("number")),Some(&vm::Value::Float(if observer.string{0.0}else{7.0})));
        assert_eq!(value.get(&vm::Value::string("flag")),Some(&vm::Value::Bool(false)));
        if observer.string {
            assert!(matches!(value.get(&vm::Value::string("ref_slot")),Some(vm::Value::Int(n)) if *n>0));
            assert!(matches!(value.get(&vm::Value::string("generation")),Some(vm::Value::Int(n)) if *n>0));
            assert_eq!(&stack[at+1..at+4],&[vm::Value::Float(2.0),vm::Value::Float(55296.0),vm::Value::Float(65.0)]);
            assert_eq!(stack[at-2],vm::Value::Int(0),"pending nodes retired");
        }else{
            for name in ["ref_slot","generation"]{assert_eq!(value.get(&vm::Value::string(name)),Some(&vm::Value::Int(0)));}
        }
        if !observer.string {assert_eq!(stack[stack.len()-2],vm::Value::Int(0),"pending nodes retired");}
        reset(&mut machine);
        assert!(!machine.scope_reset_pending());
        assert_eq!(machine.host_context().resource_count(),0);
        assert!(!machine.capture_certification_stats().unwrap().valid);
        eprintln!("SLOPPY107_ROOT_RESET source={source:?} cert_retired=true pending=false resources=0 guest_invocations=1");
    }).unwrap().join().unwrap();
}
#[test]
fn sloppy107_roots_object_active_return() {
    root_run(
        "function f(){var o={x:7};try{return o;}finally{var k={z:9};k.z;}}var r=f();r.x;",
        &OBJECT_ROOTS,
    );
}
#[test]
fn sloppy107_roots_string_throw_caller_pending_return() {
    root_run(
        "function caller(){function f(){throw '\\uD800\\u0041';}var r=1;try{try{f();}finally{var k={m:2};r=k.m;}}catch(e){return e;}return 'no';}var s=caller();s;",
        &STRING_ROOTS,
    );
}
