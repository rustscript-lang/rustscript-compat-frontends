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
            eprintln!(
                "DUPLICATE109_SOURCE {}",
                serde_json::json!({"source":source})
            );
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
                "DUPLICATE109_VM source={source:?} status={status:?} stack={:?}",
                machine.stack()
            );
            if status.is_err() {
                eprintln!(
                    "DUPLICATE109_ROOT_FAILURE ip={} frames={:?}",
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
                "DUPLICATE109_RESET source={source:?} pending=false resources=0 guest_invocations=1"
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
        "DUPLICATE109_NODE {}",
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
        let path=std::env::temp_dir().join(format!("duplicate109-{id}-{}-{}.js",std::process::id(),NEXT.fetch_add(1,std::sync::atomic::Ordering::Relaxed)));
        use std::io::Write;
        let mut file=std::fs::OpenOptions::new().create_new(true).write(true).open(&path).unwrap();
        file.write_all(source.as_bytes()).unwrap();
        eprintln!("DUPLICATE109_FILE_SOURCE {}",serde_json::json!({"id":id,"source":source,"path":path,"adapter":"original109_non_strict"}));
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
                eprintln!("DUPLICATE109_FILE_RESULT {}",serde_json::json!({"id":id,"cookie":done.cookie,"aot":done.aot_exec_count,"jit":done.jit_native_exec_count,"cert_before":done.certification_valid_before_reset,"cert_after":done.certification_valid_after_reset,"resources_after":done.resources_after_reset,"arrays_retired":done.owned_arrays_retired,"guest_invocations":1,"removed":!path.exists()}));
            }
            Err(CorpusFileError::Execution{error,rendered}) => {
                assert!(throwing);
                assert!(matches!(error.as_ref(),vm::VmError::HostError(message) if message=="uncaught JavaScript throw"),"{error:?}");
                eprintln!("DUPLICATE109_FILE_THROW {}",serde_json::json!({"id":id,"source":source,"error":format!("{error:?}"),"rendered":rendered,"guest_invocations":1,"removed":!path.exists()}));
            }
            other => panic!("unexpected {other:?}"),
        }
    }).unwrap().join().unwrap();
    oracle(
        &oracle_source,
        if throwing {
            serde_json::json!({"kind":"throw","value": if id=="mutant" {"#1: f1(1, 2) === undefined"} else {"TEST262109_FAIL_SENTINEL"}})
        } else {
            serde_json::json!({"kind":"number","value":1})
        },
    );
}

#[test]
fn duplicate109_last_actual_red() {
    boxed("function f(a,a){return a;}f(1,7);", 3, 7.0, false);
}
#[test]
fn duplicate109_original_upstream_red() {
    file_observe(
        "S10.2.1_A3",
        include_str!("fixtures/test262109/S10.2.1_A3.js"),
        false,
    );
}
fn refuse(source: &str, diagnostic: &str) {
    let error = match pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
    {
        Ok(_) => panic!("must refuse source={source:?}"),
        Err(error) => error,
    };
    let text = format!("{error:?}");
    eprintln!(
        "DUPLICATE109_REFUSAL {}",
        serde_json::json!({"source":source,"diagnostic":text,"expected":diagnostic,"guest_invocations":0,"node_grammar_oracle":false})
    );
    assert!(text.contains(diagnostic), "{source}: {text}");
}

#[test]
fn duplicate109_positions_and_original_prefix() {
    for source in [
        "var k=1.5;var p={'1.5':9};p[k];function f(a,a){return a;}f(1,7);",
        "function f(a,a,a){return a;}f(1,2,7);",
        "function f(x,a,b,x){return x;}f(1,2,3,7);",
        "var f=function(a,a){return a;};f(1,7);",
        "function f(a,\\u0061){return a;}f(1,7);",
    ] {
        boxed(source, 3, 7.0, false);
    }
}
#[test]
fn duplicate109_missing_explicit_extra() {
    for source in [
        "function f(a,a){return a;}f(7);",
        "function f(x,a,b,x){return x;}f(1,2);",
        "function f(a,a,a){return a;}f(1,7);",
        "function f(a,a){return a;}f({x:7});",
        "function f(a,a){return a;}f(7,undefined);",
    ] {
        boxed(source, 0, 0.0, false);
    }
    boxed("function f(a,a){return a;}f(1,7,9);", 3, 7.0, false);
}
#[test]
fn duplicate109_bindings_length_and_argument_order() {
    for source in [
        "function f(a,a){var a;return a;}f(1,7);",
        "function f(a,a){var a=7;return a;}f(1,2);",
        "function f(a,a){a=7;return a;}f(1,2);",
    ] {
        boxed(source, 3, 7.0, false);
    }
    boxed("function f(a,a,a){return a;}f.length;", 3, 3.0, false);
    boxed("function f(x,a,b,x){return x;}f['length'];", 3, 4.0, false);
    boxed(
        "function f(a,a){return a;}var n=0;var r=f(n=n+1,n=n+1,n=n+1);r===2&&n===3;",
        2,
        0.0,
        true,
    );
    boxed(
        "function f(a,a){var o={arguments:7,eval:9};return o.arguments===7&&o.eval===9&&'this arguments eval'==='this arguments eval';}f(1,2);",
        2,
        0.0,
        true,
    );
}
#[test]
fn duplicate109_limit64_metadata() {
    let params = vec!["a"; 64].join(",");
    boxed(
        &format!("function f({params}){{return a;}}f.length;"),
        3,
        64.0,
        false,
    );
    boxed(
        &format!("function f({params}){{return a;}}f(7);"),
        0,
        0.0,
        false,
    );
    let params65 = vec!["a"; 65].join(",");
    refuse(
        &format!("function f({params65}){{return a;}}f();"),
        "P4 source functions exceed the 64 formal parameter profile limit",
    );
}
#[test]
fn duplicate109_limit64_supplied() {
    let params = vec!["a"; 64].join(",");
    let mut args = vec!["1"; 64];
    args[63] = "7";
    boxed(
        &format!("function f({params}){{return a;}}f({});", args.join(",")),
        3,
        7.0,
        false,
    );
}
#[test]
fn duplicate109_strict_and_closed_refusals() {
    for source in [
        "function f(a,a){'use strict';return a;}f(1,7);",
        "'use strict';function f(a,a){return a;}f(1,7);",
        "function outer(){'use strict';function f(a,a){return a;}return 7;}outer();",
        "function f(a,\\u0061){'use strict';return a;}f(1,7);",
    ] {
        refuse(source, "duplicate");
    }
    for (source, diagnostic) in [
        (
            "function f(a,a,arguments){return a;}f(1,7);",
            "arguments/eval formals",
        ),
        (
            "function f(a,a,eval){return a;}f(1,7);",
            "arguments/eval formals",
        ),
        (
            "function f(a,a){return arguments;}f(1,7);",
            "arguments/eval observations",
        ),
        (
            "function f(a,a){return eval;}f(1,7);",
            "arguments/eval observations",
        ),
        ("function f(a,a){return this;}f(1,7);", "own this"),
        (
            "var x=7;function f(a,a){return x;}f(1,7);",
            "P4 free-variable capture",
        ),
    ] {
        refuse(source, diagnostic);
    }
}
#[test]
fn duplicate109_error_sentinel() {
    file_observe("sentinel", "$ERROR('TEST262109_FAIL_SENTINEL');", true);
}
#[test]
fn duplicate109_expected_undefined_mutant() {
    let original = include_str!("fixtures/test262109/S10.2.1_A3.js");
    assert_eq!(original.matches("=== undefined").count(), 2);
    let mutant = original.replacen("=== undefined", "=== 1", 1);
    assert_ne!(mutant, original);
    file_observe("mutant", &mutant, true);
}
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
        d.id = vm::FrontendId::new("duplicate109-roots");
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
            .map(|f| f.index)
            .collect();
        assert_eq!(functions.len(), 1);
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
                                    "duplicate109 function={index} root check={check}"
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
                "DUPLICATE109_ROOT_SITE source={source:?} function={index} carrier={carrier} active_frame={frame} completion={completion} payload={payload} gc_before_NoAlloc=true checked_current_generation=true"
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

        eprintln!(
            "DUPLICATE109_MANAGED_SITE source={source:?} result={result} last={last} token_identity_assertions=true"
        );
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

fn managed(source: &str, observer: &'static RootsObserver) {
    let source = source.to_owned();
    std::thread::Builder::new().stack_size(33_554_432).spawn(move || {
        let mut machine=probe_vm(&source,observer,"duplicate109-roots");
        let status=machine.run();
        eprintln!("DUPLICATE109_MANAGED_VM source={source:?} status={status:?} stack={:?}",machine.stack());
        assert_eq!(status.unwrap(),vm::VmStatus::Halted);
        assert_eq!((machine.aot_exec_count(),machine.jit_native_exec_count()),(0,0));
        let stack=machine.stack();
        let at=stack.iter().rposition(|v|*v==vm::Value::string("P7_OBSERVED")).unwrap()+1;
        let vm::Value::Map(value)=&stack[at] else {panic!("actual full managed box")};
        assert_eq!(value.len(),5);
        assert_eq!(value.get(&vm::Value::string("tag")),Some(&vm::Value::Int(if observer.string{4}else{5})));
        assert_eq!(value.get(&vm::Value::string("number")),Some(&vm::Value::Float(0.0)));
        assert_eq!(value.get(&vm::Value::string("flag")),Some(&vm::Value::Bool(false)));
        for key in ["ref_slot","generation"] {assert!(matches!(value.get(&vm::Value::string(key)),Some(vm::Value::Int(n)) if *n>0));}
        if observer.string {assert_eq!(&stack[at+1..at+4],&[vm::Value::Float(2.0),vm::Value::Float(55296.0),vm::Value::Float(65.0)]);}
        reset(&mut machine);
        eprintln!("DUPLICATE109_MANAGED_RESET source={source:?} certification_retired=true resources=0 arrays_retired=true");
    }).unwrap().join().unwrap();
}
#[test]
fn duplicate109_managed_object_active_return() {
    let source = "function f(a,a){return a;}var p={x:1};var o={x:7};var r=f(p,o);if(!(r===o&&r.x===7&&o.x===7&&p.x===1)){throw 'identity';}r;";
    managed(source, &OBJECT_ROOTS);
    oracle(
        &format!("{source}r===o&&p.x===1&&o.x===7;"),
        serde_json::json!({"kind":"boolean","value":true}),
    );
}
#[test]
fn duplicate109_managed_string_active_return() {
    let source = "function f(a,a){return a;}var p='old';var o='\\uD800\\u0041';var r=f(p,o);if(!(r===o&&p==='old'&&o==='\\uD800\\u0041')){throw 'identity';}r;";
    managed(source, &STRING_ROOTS);
    oracle(
        &format!("{source}r===o&&p==='old';"),
        serde_json::json!({"kind":"boolean","value":true}),
    );
}
