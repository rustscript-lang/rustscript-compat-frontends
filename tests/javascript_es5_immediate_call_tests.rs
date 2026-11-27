//! Direct closed non-strict expression calls reuse the existing value-call protocol.
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
                "IMMEDIATE111_SOURCE {}",
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
                "IMMEDIATE111_VM source={source:?} status={status:?} stack={:?}",
                machine.stack()
            );
            if status.is_err() {
                eprintln!(
                    "IMMEDIATE111_ROOT_FAILURE ip={} frames={:?}",
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
                "IMMEDIATE111_RESET source={source:?} pending=false resources=0 guest_invocations=1"
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
    if tag == 3 {
        let Some(vm::Value::Float(actual)) = map.get(&vm::Value::string("number")) else {
            panic!("number payload")
        };
        assert_eq!(
            actual.to_bits(),
            number.to_bits(),
            "signed numeric payload: {source}"
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
        "const source={};try{{const v=(0,eval)(source);console.log(JSON.stringify(v===undefined?{{kind:'undefined'}}:{{kind:typeof v,value:v,...(typeof v==='number'?{{negative_zero:Object.is(v,-0)}}:{{}})}}));}}catch(e){{console.log(JSON.stringify({{kind:'throw',value:e}}));}}",
        serde_json::to_string(source).unwrap()
    );
    let output = std::process::Command::new("timeout")
        .args(["15", "node", "-e", &script])
        .output()
        .unwrap();
    eprintln!(
        "IMMEDIATE111_NODE {}",
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
        assert_eq!(actual.as_object().unwrap().len(), 3);
        assert_eq!(
            actual["negative_zero"],
            expected["value"].as_f64().unwrap().is_sign_negative()
                && expected["value"].as_f64().unwrap() == 0.0
        );
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
fn immediate111_exact_retired_source() {
    boxed("(function(){return 7;})();", 3, 7.0, false);
}

#[test]
fn immediate111_forms_and_local_arithmetic() {
    for source in [
        "(((function(){return 7;})))();",
        "var r=function(){return 7;}();r;",
        "(function(){var a=2;var b=5;return a+b;})();",
        "(function(){function f(){return 7;}return f();})();",
    ] {
        boxed(source, 3, 7.0, false);
    }
    boxed("(function(){return -0;})();", 3, -0.0, false);
}
#[test]
fn immediate111_undefined_and_formals() {
    for source in [
        "(function(){})();",
        "(function(){return;})();",
        "(function(){return undefined;})();",
        "(function(a,a){return a;})(7);",
        "(function(a,a){return a;})(7,undefined);",
    ] {
        boxed(source, 0, 0.0, false);
    }
    for source in [
        "(function(a,b){return b;})(1,7);",
        "(function(a,a){return a;})(1,7,9);",
        "(function(a,a){var a;return a;})(1,7);",
    ] {
        boxed(source, 3, 7.0, false);
    }
}
#[test]
fn immediate111_once_order_and_abrupt_actual() {
    boxed(
        "var n=0;var r=(function(a,a){return a;})(n=n+1,n=n+1,n=n+1);r===2&&n===3;",
        2,
        0.0,
        true,
    );
    boxed(
        "var n=0;var r=(function(k){return function(a){return a;};})(n=n+1)(n=n+1);r===2&&n===2;",
        2,
        0.0,
        true,
    );
    // A closed throwing argument avoids a capture. A wrong body entry throws 99;
    // an evaluated later actual changes n from 1 to 9.
    boxed(
        "var n=0;var caught=false;try{(function(a,b,c){throw 99;})(n=n+1,(function(){throw 7;})(),n=9);}catch(e){caught=e===7;}caught&&n===1;",
        2,
        0.0,
        true,
    );
}
#[test]
fn immediate111_nested_and_switch() {
    boxed(
        "var r=0;switch(1){case (function(){return 2;})():r=2;break;case 1:r=1;}r;",
        3,
        1.0,
        false,
    );
    boxed(
        "var r=0;switch((function(){return 2;})()){case 2:r=7;break;default:r=9;}r;",
        3,
        7.0,
        false,
    );
    boxed(
        "function outer(){return (function(){return 7;})();}outer();",
        3,
        7.0,
        false,
    );
}
#[test]
fn immediate111_strict_receiver_and_value_controls() {
    for source in [
        "(function(){'use strict';return this;})();",
        "'use strict';(function(){return this;})();",
        "function outer(){'use strict';return (function(){return this;})();}outer();",
    ] {
        boxed(source, 0, 0.0, false);
    }
    boxed(
        "function m(){'use strict';return this.x;}var o={m:m,x:7};(o.m)();",
        3,
        7.0,
        false,
    );
    for source in [
        "function m(){'use strict';return this;}var o={m:m};(0,o.m)();",
        "function m(){'use strict';return this;}var o={m:m};(true?o.m:o.m)();",
    ] {
        boxed(source, 0, 0.0, false);
    }
}
#[test]
fn immediate111_throw_finally() {
    boxed(
        "var r=0;try{try{(function(){throw 7;})();}finally{r=1;}}catch(e){if(e===7){r=r+6;}}r;",
        3,
        7.0,
        false,
    );
    boxed(
        "var r=false;try{try{(function(){throw 'payload';})();}finally{var o={s:'later'};}}catch(e){r=e==='payload';}r;",
        2,
        0.0,
        true,
    );
    boxed(
        "(function(){try{return 1;}finally{return 7;}})();",
        3,
        7.0,
        false,
    );
}
#[test]
fn immediate111_file_public_cookie() {
    let source = "var n=0;var r=(function(a,a){return a;})(n=n+1,n=n+1,n=n+1);if(!(n===3&&r===2)){throw 'order';}7;";
    let source_owned = source.to_owned();
    std::thread::Builder::new().stack_size(33_554_432).spawn(move || {
        let path=std::env::temp_dir().join(format!("immediate111-{}.js",std::process::id()));
        use std::io::Write;
        let mut f=std::fs::OpenOptions::new().create_new(true).write(true).open(&path).unwrap();
        f.write_all(source_owned.as_bytes()).unwrap(); drop(f);
        let result=pd_vm_compat_frontends::javascript::corpus::observe_javascript_es5_corpus_file(&path);
        std::fs::remove_file(&path).unwrap();
        let done=result.unwrap();
        assert_eq!(done.cookie,7);
        assert_eq!((done.aot_exec_count,done.jit_native_exec_count),(0,0));
        assert!(done.certification_valid_before_reset&&!done.certification_valid_after_reset);
        assert_eq!(done.resources_after_reset,0);assert!(done.owned_arrays_retired);
        eprintln!("IMMEDIATE111_FILE_RESULT {}",serde_json::json!({"source":source_owned,"cookie":done.cookie,"aot":done.aot_exec_count,"jit":done.jit_native_exec_count,"cert_before":done.certification_valid_before_reset,"cert_after":done.certification_valid_after_reset,"resources_after":done.resources_after_reset,"arrays_retired":done.owned_arrays_retired,"removed":!path.exists()}));
    }).unwrap().join().unwrap();
    oracle(source, serde_json::json!({"kind":"number","value":7}));
}
fn refuse(source: &str, diagnostic: &str) {
    let error = match pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
    {
        Ok(_) => panic!("must refuse source={source:?}"),
        Err(error) => error,
    };
    assert!(
        matches!(
            error.downcast_ref::<vm::SourcePathError>(),
            Some(vm::SourcePathError::SourceWithMap {
                error: vm::SourceError::Parse(_),
                ..
            })
        ),
        "mapped source diagnostic: {error:?}"
    );
    let text = format!("{error:?}");
    eprintln!(
        "IMMEDIATE111_REFUSAL {}",
        serde_json::json!({"source":source,"diagnostic":text,"expected":diagnostic,"guest_invocations":0,"node_grammar_oracle":false})
    );
    assert!(text.contains(diagnostic), "{source}: {text}");
}

#[test]
fn immediate111_context_and_shape_refusals() {
    for (source, diagnostic) in [
        ("(function(){return this;})();", "own this"),
        (
            "(function(){return arguments;})();",
            "arguments/eval observations",
        ),
        (
            "(function(){return eval;})();",
            "arguments/eval observations",
        ),
        (
            "(function(arguments){return 7;})();",
            "arguments/eval formals",
        ),
        (
            "var x=7;(function(){return x;})();",
            "P4 free-variable capture",
        ),
        (
            "(function self(){return 7;})();",
            "anonymous function expressions",
        ),
        (
            "new (function(){return 7;})();",
            "source-function admission requires predeclaration",
        ),
        ("(async function(){return 7;})();", "post-ES5"),
        ("(function*(){return 7;})();", "post-ES5"),
        ("(function(a){return a;})(...[7]);", "spread call arguments"),
        ("(function(){return 7;})<number>();", "ES5"),
        ("(function(a,a){'use strict';return a;})(1,7);", "duplicate"),
    ] {
        refuse(source, diagnostic);
    }
}
#[test]
fn immediate111_limit64() {
    let params = vec!["a"; 64].join(",");
    let mut args = vec!["1"; 64];
    args[63] = "7";
    boxed(
        &format!("(function({params}){{return a;}})({});", args.join(",")),
        3,
        7.0,
        false,
    );
    boxed(
        &format!("(function({params}){{return a;}})(7);"),
        0,
        0.0,
        false,
    );
    refuse(
        &format!("(function({}){{return a;}})();", vec!["a"; 65].join(",")),
        "64 formal parameter profile limit",
    );
    refuse(
        &format!("(function(a){{return a;}})({});", vec!["1"; 65].join(",")),
        "64 actual argument profile limit",
    );
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
        d.id = vm::FrontendId::new("immediate111-roots");
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
            .take(1)
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
                                    "immediate111 function={index} root check={check}"
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
                "IMMEDIATE111_ROOT_SITE source={source:?} function={index} carrier={carrier} active_frame={frame} completion={completion} payload={payload} gc_before_NoAlloc=true checked_current_generation=true"
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
            "IMMEDIATE111_MANAGED_SITE source={source:?} result={result} last={last} token_identity_assertions=true"
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
        let mut machine=probe_vm(&source,observer,"immediate111-roots");
        let status=machine.run();
        eprintln!("IMMEDIATE111_MANAGED_VM source={source:?} status={status:?} stack={:?}",machine.stack());
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
        if observer.string {assert_eq!(&stack[at+1..at+4],&[vm::Value::Float(2.0),vm::Value::Float(65.0),vm::Value::Float(66.0)]);}
        reset(&mut machine);
        eprintln!("IMMEDIATE111_MANAGED_RESET source={source:?} certification_retired=true resources=0 arrays_retired=true");
    }).unwrap().join().unwrap();
}

#[test]
fn immediate111_managed_object_active_return() {
    let source = "var o;var r=(function(a){return a;})(o={x:7},{s:'later'});if(!(r===o&&r.x===7)){throw 'identity';}r;";
    managed(source, &OBJECT_ROOTS);
    oracle(source, serde_json::json!({"kind":"object","value":{"x":7}}));
}
#[test]
fn immediate111_managed_string_active_return() {
    let source = "var o;var r=(function(a){return a;})(o='AB',{s:'later'});if(!(r===o)){throw 'identity';}r;";
    managed(source, &STRING_ROOTS);
    oracle(source, serde_json::json!({"kind":"string","value":"AB"}));
}
#[test]
fn immediate111_managed_function_active_return() {
    let source = "var o;var r=(function(a){return a;})(o=function(){return 7;},{s:'later'});if(!(r===o&&r()===7)){throw 'identity';}r;";
    managed(source, &OBJECT_ROOTS);
    oracle(source, serde_json::json!({"kind":"function"}));
}
#[path = "javascript_es5/arity_observer.rs"]
mod arity_observer;
#[test]
fn immediate111_managed_throw_finally_collection() {
    let source = "var r=false;try{try{(function(){throw 'payload';})();}finally{var sink={s:'later'};}}catch(e){r=e==='payload';}r;";
    std::thread::Builder::new().stack_size(33_554_432).spawn(move||{
        static OBSERVER:std::sync::LazyLock<arity_observer::Observer>=std::sync::LazyLock::new(||arity_observer::Observer{reads:vec!["r".into()],force_gc:true,stats:true,..Default::default()});
        let mut machine=probe_vm(source,&*OBSERVER,"javascript-es5-arity-observe");
        assert_eq!(machine.run().unwrap(),vm::VmStatus::Halted);
        assert_eq!((machine.aot_exec_count(),machine.jit_native_exec_count()),(0,0));
        let stack=machine.stack();let at=stack.len()-4;
        let vm::Value::Map(v)=&stack[at] else{panic!("full Boolean box {stack:?}")};
        assert_eq!(v.len(),5);
        for(k,x)in [("tag",vm::Value::Int(2)),("number",vm::Value::Float(0.0)),("flag",vm::Value::Bool(true)),("ref_slot",vm::Value::Int(0)),("generation",vm::Value::Int(0))]{assert_eq!(v.get(&vm::Value::string(k)),Some(&x));}
        assert!(matches!(stack.last(),Some(vm::Value::Int(n))if *n>0),"real collections {stack:?}");
        eprintln!("IMMEDIATE111_PENDING_ROOT source={source:?} stack={stack:?} actual_collection=true caught_payload=true");
        reset(&mut machine);
    }).unwrap().join().unwrap();
    oracle(source, serde_json::json!({"kind":"boolean","value":true}));
}
