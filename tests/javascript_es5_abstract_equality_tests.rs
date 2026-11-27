//! Primitive abstract equality through the public interpreter VM and Node.
fn boolean(source: &str, expected: bool) {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            eprintln!("EQ147_SOURCE {}", serde_json::json!({"source":source,"expected":expected,"operator":"equality","roles":["left","right"]}));
            let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source)
                .unwrap_or_else(|error| panic!("source={source:?} guest_invocations=0 {error:?}"));
            machine.set_jit_config(vm::JitConfig { enabled: false, ..*machine.jit_config() });
            assert!(!machine.jit_config().enabled);
            assert_eq!((machine.aot_exec_count(), machine.jit_native_exec_count()), (0, 0));
            machine.set_fuel(10_000_000);
            let status = machine.run();
            eprintln!("EQ147_VM status={status:?} stack={:?}", machine.stack());
            assert_eq!(status.unwrap(), vm::VmStatus::Halted);
            assert_eq!((machine.aot_exec_count(), machine.jit_native_exec_count()), (0, 0));
            let vm::Value::Map(map) = machine.stack().last().unwrap() else { panic!("boxed Boolean") };
            assert_eq!(map.len(), 5);
            for (name, value) in [("tag",vm::Value::Int(2)),("number",vm::Value::Float(0.0)),("flag",vm::Value::Bool(expected)),("ref_slot",vm::Value::Int(0)),("generation",vm::Value::Int(0))] {
                assert_eq!(map.get(&vm::Value::string(name)), Some(&value), "{source}");
            }
            assert!(machine.capture_certification_stats().unwrap().valid);
            machine.reset_for_reuse().unwrap();
            if machine.scope_reset_pending() {
                let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
                assert!(matches!(machine.poll_reset_for_reuse(&mut cx), std::task::Poll::Ready(Ok(()))));
            }
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            assert!(!machine.capture_certification_stats().unwrap().valid);
            assert!(pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine).is_none());
            eprintln!("EQ147_RESET resources=0 owned_retired=true cert=false native=0");
            oracle(&source, expected);
        }).unwrap().join().unwrap();
}
fn oracle(source: &str, expected: bool) {
    let script = format!(
        "const source={};try{{let v=(0,eval)(source);console.log(JSON.stringify({{kind:typeof v,value:v}}));}}catch(e){{console.log(JSON.stringify({{kind:'throw',value:String(e)}}));}}",
        serde_json::to_string(source).unwrap()
    );
    let version = std::process::Command::new("timeout")
        .args(["15", "node", "--version"])
        .output()
        .unwrap();
    assert!(version.status.success());
    let output = std::process::Command::new("timeout")
        .args(["15", "node", "-e", &script])
        .output()
        .unwrap();
    eprintln!(
        "EQ147_NODE {}",
        serde_json::json!({"source":source,"source_utf16":source.encode_utf16().collect::<Vec<_>>(),"argv":["timeout","15","node","-e",script],"rc":output.status.code(),"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr),"version_stdout":String::from_utf8_lossy(&version.stdout)})
    );
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        serde_json::json!({"kind":"boolean","value":expected})
    );
}
#[test]
fn primitive147_red_same_type() {
    boolean("1==1;", true);
}
#[test]
fn primitive147_red_nullish() {
    boolean("null==undefined;", true);
}
#[test]
fn primitive147_red_mixed() {
    boolean("'0'==false;", true);
}
#[test]
fn primitive147_red_dynamic() {
    boolean("function f(a,b){return a==b;}f('1',true);", true);
}
#[test]
fn primitive147_red_inequality() {
    boolean("null!=0;", true);
}

// The finite manifest is generated before selecting a shard. Each ordered pair
// has two operators and four independently compiled operand-provenance styles.
fn matrix_rows(strict: bool) -> Vec<(String, String, bool)> {
    let values = ["1", "true", "null", "undefined", "'1'"];
    let mut rows = Vec::new();
    for (i, left) in values.iter().enumerate() {
        for (j, right) in values.iter().enumerate() {
            for (op, invert) in if strict {
                [("===", false), ("!==", true)]
            } else {
                [("==", false), ("!=", true)]
            } {
                for style in ["proved", "leftDynamic", "rightDynamic", "bothDynamic"] {
                    let source = match style {
                        "proved" => format!("var a={left};var b={right};a{op}b;"),
                        "leftDynamic" => format!("function f(a){{return a{op}{right};}}f({left});"),
                        "rightDynamic" => {
                            format!("function f(b){{return {left}{op}b;}}f({right});")
                        }
                        "bothDynamic" => {
                            format!("function f(a,b){{return a{op}b;}}f({left},{right});")
                        }
                        _ => unreachable!(),
                    };
                    let equal = if strict {
                        i == j
                    } else {
                        matches!(i, 2 | 3) == matches!(j, 2 | 3)
                    };
                    rows.push((
                        format!(
                            "{}-{i}-{j}-{op}-{style}",
                            if strict { "strict" } else { "abstract" }
                        ),
                        source,
                        equal != invert,
                    ));
                }
            }
        }
    }
    assert_eq!(rows.len(), 200);
    assert_eq!(
        rows.iter()
            .map(|r| &r.0)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        200
    );
    rows
}
fn matrix(strict: bool) {
    let shard: usize = std::env::var("EQ147_SHARD")
        .expect("explicit finite shard required")
        .parse()
        .unwrap();
    assert!(shard < 25);
    let rows = matrix_rows(strict);
    eprintln!(
        "EQ147_MANIFEST strict={strict} rows={} shards=25 shard={shard}",
        rows.len()
    );
    for (id, source, expected) in &rows[shard * 8..shard * 8 + 8] {
        eprintln!(
            "EQ147_ROW {}",
            serde_json::json!({"id":id,"source":source,"expected":expected,"strict":strict,"shard":shard})
        );
        boolean(source, *expected);
        eprintln!("EQ147_ROW_PASS id={id}");
    }
}
#[test]
fn primitive147_abstract_matrix_shard() {
    matrix(false);
}
#[test]
fn primitive147_strict_matrix_shard() {
    matrix(true);
}

fn extension_rows() -> Vec<(&'static str, bool)> {
    vec![
        ("-0==0;", true),
        ("0/0==0/0;", false),
        ("1/0==1/0;", true),
        ("-1/0!=1/0;", true),
        ("false==0;", true),
        ("true=='1';", true),
        ("''==false;", true),
        ("'invalid'==0;", false),
        ("'NaN'=='NaN';", true),
        ("null==0;", false),
        ("undefined==0;", false),
        ("null=='0';", false),
        ("undefined=='';", false),
        ("' \\t\\r\\n'==0;", true),
        ("'\\u00a0\\ufeff1\\u2028'==true;", true),
        ("'\\u0000'=='\\0';", true),
        ("'\\ud800'=='\\ud800';", true),
        ("'\\ud800'!='\\ud801';", true),
        ("'😀'=='\\ud83d\\ude00';", true),
        ("var a='same';var b='same';a==b;", true),
        ("var a='1';var b=a;b==true;", true),
        ("var a=true?'1':1;a==true;", true),
        ("var a=0;(a='1',a)==true;", true),
        (
            "var a=1;var i=0;var r=true;while(i<2){r=r&&(a=='1');a='1';i=i+1;}r;",
            true,
        ),
        ("var a=1;for(var i=0;i<2;i=i+1){a='1';}a==true;", true),
        ("function f(a){'use strict';return a==1;}f(true);", true),
        ("'use strict';function f(a){return a==1;}f('1');", true),
        (
            "function f(a){'use strict';return a==1;}f(true);'\\141'=='a';",
            true,
        ),
        (
            "function outer(a){function inner(b){return b==1;}return inner(a);}outer(true);",
            true,
        ),
        ("function f(undefined){return undefined==0;}f(null);", false),
        (
            "function f(a){try{throw a;}catch(e){return e==true;}}f('1');",
            true,
        ),
        ("var o={x:'1'};o.x==true;", true),
        (
            "var o={n:0,get x(){'use strict';this.n=this.n+1;return '1';}};var r=o.x==true;r&&o.n===1;",
            true,
        ),
        (
            "var n=0;var q=(n=n*10+1,'1')==(n=n*10+2,true);q&&n===12;",
            true,
        ),
        (
            "var n=0;function l(){throw 'left';}try{(n=n*10+1,l())==(n=n*10+2,true);}catch(e){n=n*10+3;}n===13;",
            true,
        ),
        (
            "var n=0;function l(){return {};}function r(){throw 'right';}try{(n=n*10+1,l())==(n=n*10+2,r());}catch(e){n=n*10+3;}n===123;",
            true,
        ),
        (
            "function f(a){try{return a==1;}finally{var z='live';}}f('1');",
            true,
        ),
        (
            "function f(){try{throw '\\ud800\\0';}finally{var live='finally';}}var r=false;try{f()==1;}catch(e){r=e=='\\ud800\\0';}r;",
            true,
        ),
        (
            "var o={};var a=[];function f(){}o===o&&a===a&&f===f&&o!=={}&&a!==[]&&f!==(function(){});",
            true,
        ),
        ("function f(a,b){return a!=b;}f('0',false);", false),
    ]
}
#[test]
fn primitive147_extensions_shard() {
    let rows = extension_rows();
    let shard: usize = std::env::var("EQ147_EXT_SHARD")
        .expect("explicit finite shard required")
        .parse()
        .unwrap();
    assert!(shard < rows.len().div_ceil(8));
    for (i, (source, expected)) in rows.iter().enumerate().skip(shard * 8).take(8) {
        eprintln!("EQ147_EXTENSION id={i} source={source:?} expected={expected}");
        boolean(source, *expected);
        eprintln!("EQ147_EXTENSION_PASS id={i}");
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
fn primitive147_demand_and_refusals() {
    for (source, parser) in [
        ("1==true;", 0),
        ("null==undefined;", 0),
        ("'a'=='a';", 0),
        ("null=='0';", 0),
        ("undefined=='0';", 0),
        ("function f(x){return null==x;}f('0');", 0),
        ("function f(x){return undefined==x;}f('0');", 0),
        ("'0'==false;", 1),
        ("function f(x){return x=='0';}f(false);", 1),
        ("function f(a,b){return a==b;}f('0',false);", 1),
        ("Math.abs(-1);'0'==false;", 1),
    ] {
        let ir = parse(source).unwrap();
        let count = |part: &str| {
            ir.functions
                .iter()
                .filter(|f| f.name.contains(part))
                .count()
        };
        assert_eq!(count("numeric_scan"), parser, "{source}");
        assert_eq!(count("round_decimal"), parser, "{source}");
        assert_eq!(
            count("string_compare"),
            0,
            "reuse equality, not relational comparator"
        );
        assert_eq!(count("number_string_managed"), 0);
        assert_eq!(count("managed_to_units"), 0);
        assert_eq!(count("units_to_managed"), 0);
        assert_eq!(
            ir.functions
                .iter()
                .map(|f| f.index)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            ir.functions.len()
        );
        assert!(
            ir.function_impls
                .values()
                .all(|f| f.param_slots.iter().all(|s| usize::from(*s) < ir.locals))
        );
        if matches!(source, "1==true;" | "null==undefined;") {
            assert!(
                ir.functions
                    .iter()
                    .all(|f| f.name == "__compat_es5_undefined")
            );
        }
        let both_string = matches!(
            source,
            "'a'=='a';"
                | "function f(x){return x=='0';}f(false);"
                | "function f(a,b){return a==b;}f('0',false);"
        );
        assert_eq!(
            count("abstract_string_equal"),
            usize::from(both_string),
            "{source}"
        );
        eprintln!(
            "EQ147_DEMAND source={source:?} parser={parser} wrapper={} locals={}",
            count("abstract_string_equal"),
            ir.locals
        );
    }
    for op in ["==", "!="] {
        for value in ["({})", "[]", "(function(){})"] {
            for source in [
                format!("{value}{op}null;"),
                format!("null{op}{value};"),
                format!("false&&({value}{op}1);"),
            ] {
                let e = parse(&source).expect_err("proved nonprimitive source refusal");
                assert!(e.message.contains("nonprimitive") && e.span.is_some());
                eprintln!("EQ147_REFUSED source={source:?} error={e:?}");
            }
        }
    }
}
#[test]
fn primitive147_float_typemap() {
    for source in ["true==1;", "false!='0';"] {
        let m =
            pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source).unwrap();
        let p = m.program();
        let pairs: Vec<_> = p
            .type_map
            .as_ref()
            .unwrap()
            .operand_types
            .iter()
            .filter(|(ip, _)| p.code[**ip] == vm::bytecode::OpCode::Ceq as u8)
            .collect();
        assert!(
            pairs
                .iter()
                .any(|(_, t)| **t == (vm::ValueType::Float, vm::ValueType::Float))
        );
        assert!(!pairs.iter().any(|(_, t)| matches!(
            **t,
            (vm::ValueType::Int, vm::ValueType::Float) | (vm::ValueType::Float, vm::ValueType::Int)
        )));
        eprintln!("EQ147_FLOAT_TYPEMAP source={source:?} pairs={pairs:?}");
    }
}

struct ParserObserver {
    gc: bool,
    fatal: bool,
}
impl vm::SourcePlugin for ParserObserver {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut d = vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
        d.id = vm::FrontendId::new("equality147-parser-observer");
        d
    }
    fn parse_source(
        &self,
        source: &str,
        cx: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        use vm::{Expr as E, Stmt as S, TypeSchema as T};
        let mut ir = vm::SourcePlugin::parse_source(
            &pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN,
            source,
            cx,
        )?;
        let mark = cx
            .install_host_function(&mut ir, "typescript::throw_error", 1)
            .unwrap();
        let h = |name: &str| ir.functions.iter().find(|f| f.name == name).unwrap().index;
        let scan = h("__compat_es5_numeric_scan");
        let checked = h("__compat_es5_checked_base");
        let read = h("__compat_ref_read");
        let decode = h("__compat_es5_decode_int");
        let collect = h("__compat_es5_heap_collect");
        let p = ir.function_impls[&scan].param_slots.clone();
        assert_eq!(p.len(), 4);
        let (c, input, mode, radix) = (p[0], p[1], p[2], p[3]);
        let mut fresh = || {
            let i = u16::try_from(ir.locals).unwrap();
            ir.locals += 1;
            i
        };
        let (before, base, top, cursor, row, found) =
            (fresh(), fresh(), fresh(), fresh(), fresh(), fresh());
        let call = |i, args| E::Call(i, vec![], args, None, None);
        let num = |e| E::Div(Box::new(e), Box::new(E::Float(1.0)));
        let add = |a, b| E::Add(Box::new(a), Box::new(b));
        let eq = |a, b| E::Eq(Box::new(a), Box::new(b));
        let get = |s, k: &str| {
            call(
                vm::BuiltinFunction::Get.call_index(),
                vec![E::Var(s), E::String(k.into())],
            )
        };
        let rd = |a| call(decode, vec![num(call(read, vec![E::Var(c), num(a)]))]);
        let check = |e| S::Expr {
            expr: call(vm::BuiltinFunction::Assert.call_index(), vec![e]),
            line: 1,
        };
        let let_ = |index, t, expr| S::Let {
            index,
            declared_schema: Some(t),
            expr,
            line: 1,
        };
        let set = |index, expr| S::Assign {
            kind: vm::AssignmentKind::Set,
            index,
            expr,
            line: 1,
        };
        let base_expr = || {
            call(
                checked,
                vec![E::Var(c), get(input, "ref_slot"), get(input, "generation")],
            )
        };
        let mut pre = vec![
            check(eq(E::Var(mode), E::Int(0))),
            check(eq(E::Var(radix), E::Float(0.0))),
        ];
        if self.fatal {
            pre.push(check(E::Bool(false)));
        }
        if self.gc {
            pre.extend([
                let_(before, T::Int, rd(E::Int(11))),
                let_(base, T::Int, base_expr()),
                let_(top, T::Int, rd(E::Int(3))),
                let_(cursor, T::Int, E::Int(0)),
                let_(row, T::Int, E::Int(0)),
                let_(found, T::Bool, E::Bool(false)),
                S::While {
                    condition: E::Lt(
                        Box::new(E::Var(cursor)),
                        Box::new(rd(add(E::Var(top), E::Int(3)))),
                    ),
                    body: vec![
                        set(
                            row,
                            add(
                                add(E::Var(top), E::Int(4)),
                                E::Mul(Box::new(E::Var(cursor)), Box::new(E::Int(3))),
                            ),
                        ),
                        set(
                            found,
                            E::Or(
                                Box::new(E::Var(found)),
                                Box::new(E::And(
                                    Box::new(eq(rd(E::Var(row)), E::Int(4))),
                                    Box::new(E::And(
                                        Box::new(eq(
                                            rd(add(E::Var(row), E::Int(1))),
                                            get(input, "ref_slot"),
                                        )),
                                        Box::new(eq(
                                            rd(add(E::Var(row), E::Int(2))),
                                            get(input, "generation"),
                                        )),
                                    )),
                                )),
                            ),
                        ),
                        set(cursor, add(E::Var(cursor), E::Int(1))),
                    ],
                    line: 1,
                },
                check(E::Var(found)),
                S::Expr {
                    expr: call(collect, vec![E::Var(c)]),
                    line: 1,
                },
                check(E::Gt(Box::new(rd(E::Int(11))), Box::new(E::Var(before)))),
                check(eq(base_expr(), E::Var(base))),
            ]);
        }
        pre.push(S::Expr {
            expr: call(mark, vec![E::String("EQ147_PARSER_ENTER".into())]),
            line: 1,
        });
        let f = ir.function_impls.get_mut(&scan).unwrap();
        pre.append(&mut f.body_stmts);
        f.body_stmts = pre;
        Ok(ir)
    }
}

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

struct ProbeCounts {
    parser: std::sync::atomic::AtomicUsize,
    wrapper: std::sync::atomic::AtomicUsize,
    inner: std::sync::atomic::AtomicUsize,
}
struct ParserCounter(std::sync::Arc<ProbeCounts>);
impl vm::HostFunction for ParserCounter {
    fn call(&mut self, _vm: &mut vm::Vm, args: &[vm::Value]) -> vm::VmResult<vm::CallOutcome> {
        assert_eq!(args.len(), 1);
        let bump = |slot: &std::sync::atomic::AtomicUsize| {
            slot.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(vm::CallOutcome::Return(vm::CallReturn::one(
                vm::Value::Null,
            )))
        };
        if args[0] == vm::Value::string("EQ147_PARSER_ENTER") {
            bump(&self.0.parser)
        } else if args[0] == vm::Value::string("EQ147_WRAPPER_ENTER") {
            bump(&self.0.wrapper)
        } else if args[0] == vm::Value::string("EQ147_INNER_ENTER") {
            bump(&self.0.inner)
        } else {
            let vm::Value::String(message) = &args[0] else {
                panic!("typed message")
            };
            Err(vm::VmError::HostError(message.to_string()))
        }
    }
}
fn probe_vm(
    source: &str,
    observer: &'static dyn vm::SourcePlugin,
    id: &'static str,
) -> (vm::Vm, std::sync::Arc<ProbeCounts>) {
    eprintln!("EQ147_OBSERVER_SOURCE source={source:?} observer={id}");
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let issued = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new(id)),
        vm::CompileSourceFileOptions::new()
            .with_host_api_catalog(catalog.clone())
            .with_source_plugin(observer),
    )
    .unwrap_or_else(|error| panic!("source={source:?} guest_invocations=0 {error:?}"));
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
    registry.set_capability_profile(
        vm::CapabilityProfile::builder()
            .allow_host_import("typescript::throw_error")
            .build(),
    );
    for module in [
        pd_vm_compat_frontends::runtime::array::typescript_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module(),
        pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module(),
    ] {
        module
            .install_from_catalog(&mut registry, &catalog)
            .unwrap();
    }
    let counter = std::sync::Arc::new(ProbeCounts {
        parser: std::sync::atomic::AtomicUsize::new(0),
        wrapper: std::sync::atomic::AtomicUsize::new(0),
        inner: std::sync::atomic::AtomicUsize::new(0),
    });
    let calls = counter.clone();
    let schema = vm::catalog_import_schemas(&catalog, "typescript::throw_error")
        .into_iter()
        .next()
        .unwrap();
    registry
        .register_catalog(schema, move || Box::new(ParserCounter(calls.clone())))
        .unwrap();
    for name in [
        "math::floor",
        "math::abs",
        "math::acos",
        "math::asin",
        "math::atan",
        "math::atan2",
        "math::ceil",
        "math::cos",
        "math::exp",
        "math::ln",
        "math::sin",
        "math::tan",
        "math::trunc",
        "math::is_infinite",
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
    assert!(!machine.jit_config().enabled);
    assert_eq!(
        (machine.aot_exec_count(), machine.jit_native_exec_count()),
        (0, 0)
    );
    machine.set_fuel(10_000_000);
    (machine, counter)
}

fn call(index: u16, args: Vec<vm::Expr>) -> vm::Expr {
    vm::Expr::Call(index, vec![], args, None, None)
}
fn get(slot: u16, name: &str) -> vm::Expr {
    call(
        vm::BuiltinFunction::Get.call_index(),
        vec![vm::Expr::Var(slot), vm::Expr::String(name.into())],
    )
}
fn num(e: vm::Expr) -> vm::Expr {
    vm::Expr::Div(Box::new(e), Box::new(vm::Expr::Float(1.0)))
}
fn add(a: vm::Expr, b: vm::Expr) -> vm::Expr {
    vm::Expr::Add(Box::new(a), Box::new(b))
}
fn eq(a: vm::Expr, b: vm::Expr) -> vm::Expr {
    vm::Expr::Eq(Box::new(num(a)), Box::new(num(b)))
}
fn check(e: vm::Expr) -> vm::Stmt {
    vm::Stmt::Expr {
        expr: call(vm::BuiltinFunction::Assert.call_index(), vec![e]),
        line: 1,
    }
}
fn let_(i: u16, t: vm::TypeSchema, e: vm::Expr) -> vm::Stmt {
    vm::Stmt::Let {
        index: i,
        declared_schema: Some(t),
        expr: e,
        line: 1,
    }
}
fn set(i: u16, e: vm::Expr) -> vm::Stmt {
    vm::Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: i,
        expr: e,
        line: 1,
    }
}
fn fresh(ir: &mut vm::FrontendIr) -> u16 {
    let i = u16::try_from(ir.locals).unwrap();
    ir.locals += 1;
    i
}

fn helper(ir: &vm::FrontendIr, name: &str) -> u16 {
    ir.functions
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("missing {name}"))
        .index
}

// The observer changes only test IR; production comparison has no collection,
// allocation, bridge or publication frame. Force-GC is at node_begin entry.

struct EqualityObserver(u8);
impl vm::SourcePlugin for EqualityObserver {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut d = vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
        d.id = vm::FrontendId::new("equality147-string-observer");
        d
    }
    fn parse_source(
        &self,
        source: &str,
        cx: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        use vm::{Expr as E, Stmt as S, TypeSchema as T};
        let mut ir = vm::SourcePlugin::parse_source(
            &pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN,
            source,
            cx,
        )?;
        let wrapper = helper(&ir, "__compat_es5_abstract_string_equal");
        let inner = helper(&ir, "__compat_es5_string_equal");
        let mark = cx
            .install_host_function(&mut ir, "typescript::throw_error", 1)
            .unwrap();
        let enter = |name: &str| S::Expr {
            expr: call(mark, vec![E::String(name.into())]),
            line: 1,
        };
        // Selected-entry mark on the unchanged helper, so a skipped comparison
        // stays at zero and a reached comparison is counted.
        ir.function_impls
            .get_mut(&inner)
            .unwrap()
            .body_stmts
            .insert(0, enter("EQ147_INNER_ENTER"));
        if self.0 == 1 {
            let body = &mut ir.function_impls.get_mut(&wrapper).unwrap().body_stmts;
            body.insert(0, check(E::Bool(false)));
            body.insert(0, enter("EQ147_WRAPPER_ENTER"));
            eprintln!("EQ147_ABRUPT_WRAPPER_MUTANT source={source:?} wrapper_entry_fatal=true");
            return Ok(ir);
        }
        let begin = helper(&ir, "__compat_es5_node_begin");
        let write = helper(&ir, "__compat_ref_write");
        let ack = fresh(&mut ir);
        let c = ir.function_impls[&begin].param_slots[0];
        ir.function_impls
            .get_mut(&begin)
            .unwrap()
            .body_stmts
            .insert(
                0,
                let_(
                    ack,
                    T::Number,
                    call(write, vec![E::Var(c), E::Float(10.0), E::Float(32.0)]),
                ),
            );
        let checked = helper(&ir, "__compat_es5_checked_base");
        let read = helper(&ir, "__compat_ref_read");
        let decode = helper(&ir, "__compat_es5_decode_int");
        let params = ir.function_impls[&wrapper].param_slots.clone();
        assert_eq!(params.len(), 3);
        let (c, l, r) = (params[0], params[1], params[2]);
        let original = &ir.function_impls[&wrapper];
        let debug = format!("{original:?}");
        assert!(
            debug.contains(&format!("Call({inner},")),
            "production wrapper must call the unchanged equality helper"
        );
        for name in [
            "__compat_es5_heap_collect",
            "__compat_es5_node_begin",
            "__compat_es5_root_enter",
            "__compat_es5_managed_to_units",
            "__compat_es5_units_to_managed",
        ] {
            if let Some(f) = ir.functions.iter().find(|f| f.name == name) {
                assert!(
                    !debug.contains(&format!("Call({},", f.index)),
                    "production helper invokes {name}"
                );
            }
        }
        let top = fresh(&mut ir);
        let cursor = fresh(&mut ir);
        let row = fresh(&mut ir);
        let lf = fresh(&mut ir);
        let rf = fresh(&mut ir);
        let lb = fresh(&mut ir);
        let rb = fresh(&mut ir);
        let rd = |a: E| call(decode, vec![num(call(read, vec![E::Var(c), num(a)]))]);
        let base = |v| {
            call(
                checked,
                vec![E::Var(c), get(v, "ref_slot"), get(v, "generation")],
            )
        };
        let matched = |v| {
            E::And(
                Box::new(eq(rd(E::Var(row)), E::Int(4))),
                Box::new(E::And(
                    Box::new(eq(rd(add(E::Var(row), E::Int(1))), get(v, "ref_slot"))),
                    Box::new(eq(rd(add(E::Var(row), E::Int(2))), get(v, "generation"))),
                )),
            )
        };
        let body = vec![
            set(
                row,
                add(
                    add(E::Var(top), E::Int(4)),
                    E::Mul(Box::new(E::Var(cursor)), Box::new(E::Int(3))),
                ),
            ),
            set(lf, E::Or(Box::new(E::Var(lf)), Box::new(matched(l)))),
            set(rf, E::Or(Box::new(E::Var(rf)), Box::new(matched(r)))),
            set(cursor, add(E::Var(cursor), E::Int(1))),
        ];
        let mut pre = vec![
            let_(top, T::Int, rd(E::Int(3))),
            let_(cursor, T::Int, E::Int(0)),
            let_(row, T::Int, E::Int(0)),
            let_(lf, T::Bool, E::Bool(false)),
            let_(rf, T::Bool, E::Bool(false)),
            let_(lb, T::Int, base(l)),
            let_(rb, T::Int, base(r)),
            check(E::Gt(Box::new(rd(E::Int(11))), Box::new(E::Int(0)))),
            S::While {
                condition: E::Lt(
                    Box::new(E::Var(cursor)),
                    Box::new(rd(add(E::Var(top), E::Int(3)))),
                ),
                body,
                line: 1,
            },
            check(E::Var(lf)),
            check(E::Var(rf)),
        ];
        let epoch = fresh(&mut ir);
        let collect = helper(&ir, "__compat_es5_heap_collect");
        pre.extend([
            let_(epoch, T::Int, rd(E::Int(11))),
            S::Expr {
                expr: call(collect, vec![E::Var(c)]),
                line: 1,
            },
            check(E::Gt(Box::new(rd(E::Int(11))), Box::new(E::Var(epoch)))),
            check(eq(base(l), E::Var(lb))),
            check(eq(base(r), E::Var(rb))),
        ]);
        for expr in [
            E::String("EQ147_INPUT_ROOTS".into()),
            E::Var(l),
            E::Var(r),
            E::Var(top),
            rd(E::Int(11)),
            E::Var(lb),
            E::Var(rb),
        ] {
            pre.push(S::Expr { expr, line: 1 });
        }
        match self.0 {
            2 => pre.push(set(
                l,
                call(
                    vm::BuiltinFunction::Set.call_index(),
                    vec![
                        E::Var(l),
                        E::String("generation".into()),
                        add(get(l, "generation"), E::Int(1)),
                    ],
                ),
            )),
            3 => pre.push(S::Expr {
                expr: call(
                    write,
                    vec![
                        E::Var(c),
                        num(add(E::Var(lb), E::Int(2))),
                        E::Float(999999.0),
                    ],
                ),
                line: 1,
            }),
            4 => pre.push(S::Expr {
                expr: call(
                    write,
                    vec![
                        E::Var(c),
                        num(add(E::Var(lb), E::Int(3))),
                        E::Float(65536.0),
                    ],
                ),
                line: 1,
            }),
            5 => pre.push(S::Expr {
                expr: call(write, vec![E::Var(c), num(E::Var(lb)), E::Float(3.0)]),
                line: 1,
            }),
            6 | 7 => pre.push(S::Expr {
                expr: call(
                    write,
                    vec![
                        E::Var(c),
                        num(add(E::Var(lb), E::Int(if self.0 == 6 { 2 } else { 3 }))),
                        E::Float(if self.0 == 6 { 999999.0 } else { 65536.0 }),
                    ],
                ),
                line: 1,
            }),
            _ => {}
        }
        pre.insert(0, enter("EQ147_WRAPPER_ENTER"));
        let f = ir.function_impls.get_mut(&wrapper).unwrap();
        if self.0 == 6 || self.0 == 7 {
            // Derived guard-disabled mutant: same injection, validation removed,
            // unchanged helper still performs the comparison.
            f.body_stmts = pre;
            f.body_expr = call(inner, vec![E::Var(c), E::Var(l), E::Var(r)]);
            eprintln!(
                "EQ147_GUARD_BYPASS source={source:?} mode={} same_injection=true validation_removed=true",
                self.0
            );
            return Ok(ir);
        }
        // Injection is at the wrapper entry, before production validation.
        f.body_stmts.splice(0..0, pre);
        eprintln!(
            "EQ147_WRAPPER_OBSERVER source={source:?} mode={} injection_before_validation=true rhs_node_begin_gc=true",
            self.0
        );
        Ok(ir)
    }
}

fn observed_equality(
    source: &str,
    observer: &'static dyn vm::SourcePlugin,
    id: &'static str,
    success: bool,
    parser_calls: usize,
) {
    let source = source.to_owned();
    std::thread::Builder::new().stack_size(33_554_432).spawn(move || {
        let (mut m, counts) = probe_vm(&source, observer, id);
        assert_eq!(counts.parser.load(std::sync::atomic::Ordering::SeqCst), 0);
        let status = m.run();
        let calls = counts.parser.load(std::sync::atomic::Ordering::SeqCst);
        eprintln!("EQ147_OBSERVER_VM source={source:?} status={status:?} parser_calls={calls} aot={} jit={} stack={:?}",m.aot_exec_count(),m.jit_native_exec_count(),m.stack());
        assert_eq!(calls,parser_calls,"actual runtime parser entry count");
        if success {
            assert_eq!(status.unwrap(),vm::VmStatus::Halted);
            let vm::Value::Map(b)=m.stack().last().unwrap()else{panic!("Boolean box")};
            assert_eq!(b.get(&vm::Value::string("tag")),Some(&vm::Value::Int(2)));
            assert_eq!(b.get(&vm::Value::string("flag")),Some(&vm::Value::Bool(true)));
        } else {
            assert!(matches!(status,Err(vm::VmError::HostError(ref e)) if e=="assertion failed"));
        }
        assert!(!m.jit_config().enabled);
        assert_eq!((m.aot_exec_count(),m.jit_native_exec_count()),(0,0));
        reset(&mut m);
    }).unwrap().join().unwrap();
}
fn observed_guard(
    source: &str,
    observer: &'static dyn vm::SourcePlugin,
    halted_flag: Option<bool>,
    wrapper_calls: usize,
    inner_calls: usize,
) {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let (mut m, counts) = probe_vm(&source, observer, "equality147-string-observer");
            let status = m.run();
            let wrapper = counts.wrapper.load(std::sync::atomic::Ordering::SeqCst);
            let inner = counts.inner.load(std::sync::atomic::Ordering::SeqCst);
            let parser = counts.parser.load(std::sync::atomic::Ordering::SeqCst);
            eprintln!(
                "EQ147_GUARD source={source:?} status={status:?} wrapper={wrapper} inner={inner} parser={parser} stack={:?}",
                m.stack()
            );
            // This observer does not install a parser mark. parser==0 is not
            // scanner-absence proof; ParserObserver owns that counter.
            assert_eq!(parser, 0);
            assert_eq!(wrapper, wrapper_calls, "wrapper entry count");
            assert_eq!(inner, inner_calls, "unchanged helper entry count");
            match halted_flag {
                Some(flag) => {
                    assert_eq!(status.unwrap(), vm::VmStatus::Halted);
                    let vm::Value::Map(b) = m.stack().last().unwrap() else {
                        panic!("Boolean box")
                    };
                    assert_eq!(b.get(&vm::Value::string("flag")), Some(&vm::Value::Bool(flag)));
                }
                None => {
                    assert!(matches!(status, Err(vm::VmError::HostError(ref e)) if e == "assertion failed"));
                }
            }
            assert!(!m.jit_config().enabled);
            assert_eq!((m.aot_exec_count(), m.jit_native_exec_count()), (0, 0));
            reset(&mut m);
        })
        .unwrap()
        .join()
        .unwrap();
}
#[test]
fn primitive147_parser_bypass_and_guard_order() {
    static BYPASS: ParserObserver = ParserObserver {
        gc: false,
        fatal: true,
    };
    for args in [
        "'10','10'",
        "null,'0'",
        "'0',undefined",
        "null,0",
        "undefined,false",
    ] {
        let source = format!("function f(a,b){{return a==b;}}f({args});");
        // Preserve the actual expected Boolean without converting the result.
        let expected = args == "'10','10'";
        observed_equality(
            &format!("function f(a,b){{return a==b;}}f({args})==={expected};"),
            &BYPASS,
            "equality147-parser-observer",
            true,
            0,
        );
        oracle(&source, expected);
    }
    static COUNTER: ParserObserver = ParserObserver {
        gc: false,
        fatal: false,
    };
    for args in [
        "'2',{}",
        "{},'2'",
        "null,{}",
        "{},null",
        "undefined,[]",
        "(function(){}),undefined",
    ] {
        let source = format!("function f(a,b){{return a==b;}}f({args});");
        observed_equality(&source, &COUNTER, "equality147-parser-observer", false, 0);
    }
}
#[test]
fn primitive147_parser_gc_roots() {
    static OBS: ParserObserver = ParserObserver {
        gc: true,
        fatal: false,
    };
    for args in ["'1',true", "true,'1'"] {
        let source = format!("function f(a,b){{return a==b;}}f({args});");
        observed_equality(&source, &OBS, "equality147-parser-observer", true, 1);
        oracle(&source, true);
    }
}
#[test]
fn primitive147_string_equality_gc_and_guards() {
    static OBS: EqualityObserver = EqualityObserver(0);
    let source = "function f(a,b){return a==b;}f('\\ud800\\0','\\ud800\\0');";
    observed_guard(source, &OBS, Some(true), 1, 1);
    oracle(source, true);
    static GENERATION: EqualityObserver = EqualityObserver(2);
    static EXTENT: EqualityObserver = EqualityObserver(3);
    static UNIT: EqualityObserver = EqualityObserver(4);
    static HEADER: EqualityObserver = EqualityObserver(5);
    for observer in [&GENERATION, &EXTENT, &UNIT, &HEADER] {
        observed_guard(source, observer, None, 1, 0);
    }
    static FATAL: EqualityObserver = EqualityObserver(1);
    let skipped =
        "var n=0;function l(){throw 'left';}try{l()==(n=n+1,'right');}catch(e){n=n+10;}n===10;";
    observed_guard(skipped, &FATAL, Some(true), 0, 0);
    oracle(skipped, true);
    // Same mutant on a string comparison proves the selected wrapper entry
    // fatals before the unchanged helper runs.
    observed_guard(source, &FATAL, None, 1, 0);
    static BYPASS_LENGTH: EqualityObserver = EqualityObserver(6);
    static BYPASS_UNIT: EqualityObserver = EqualityObserver(7);
    // Guard disabled: the same injection recovers the old helper's normal false.
    observed_guard(source, &BYPASS_LENGTH, Some(false), 1, 1);
    observed_guard(source, &BYPASS_UNIT, Some(false), 1, 1);
}
#[test]
fn primitive147_checked_string_values_and_strict_split() {
    let strict = parse("'a'==='a';").unwrap();
    assert!(
        strict
            .functions
            .iter()
            .all(|f| !f.name.contains("abstract_string_equal"))
    );
    let mixed = parse("'a'=='a'&&'a'==='a';").unwrap();
    let wrapper = helper(&mixed, "__compat_es5_abstract_string_equal");
    let inner = helper(&mixed, "__compat_es5_string_equal");
    let script = format!("{:?}", mixed.stmts);
    let wrapper_body = format!("{:?}", mixed.function_impls[&wrapper]);
    assert!(
        script.contains(&format!("Call({wrapper},")),
        "abstract path selects the wrapper"
    );
    assert!(
        script.contains(&format!("Call({inner},")),
        "strict path keeps the original helper"
    );
    assert!(
        wrapper_body.contains(&format!("Call({inner},")),
        "wrapper calls the unchanged helper"
    );
    for (source, expected) in [
        ("''=='';", true),
        ("'a'=='a';", true),
        ("'a'!='ab';", true),
        ("'ab'!='a';", true),
        ("'\\0'=='\\0';", true),
        ("'\\ud800'=='\\ud800';", true),
        ("'\\uffff'=='\\uffff';", true),
        ("'\\0'!='\\uffff';", true),
        ("'a'!='\\0';", true),
        ("'a'=='a'&&'a'==='a';", true),
        ("'a'==='b';", false),
    ] {
        boolean(source, expected);
    }
}
