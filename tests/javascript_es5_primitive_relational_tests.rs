//! Complete primitive relational family through public interpreter VM and Node.
fn boolean(source: &str, expected: bool) {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            eprintln!(
                "REL145_SOURCE {}",
                serde_json::json!({"source":source,"expected":expected})
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
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.aot_exec_count(), machine.jit_native_exec_count()),
                (0, 0)
            );
            machine.set_fuel(10_000_000);
            let status = machine.run();
            eprintln!("REL145_VM status={status:?} stack={:?}", machine.stack());
            assert_eq!(status.unwrap(), vm::VmStatus::Halted);
            assert_eq!(
                (machine.aot_exec_count(), machine.jit_native_exec_count()),
                (0, 0)
            );
            let vm::Value::Map(map) = machine.stack().last().unwrap() else {
                panic!("boxed Boolean")
            };
            assert_eq!(map.len(), 5);
            for (name, value) in [
                ("tag", vm::Value::Int(2)),
                ("number", vm::Value::Float(0.0)),
                ("flag", vm::Value::Bool(expected)),
                ("ref_slot", vm::Value::Int(0)),
                ("generation", vm::Value::Int(0)),
            ] {
                assert_eq!(map.get(&vm::Value::string(name)), Some(&value), "{source}");
            }
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
            assert!(
                pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
                    .is_none()
            );
            eprintln!("REL145_RESET resources=0 owned_retired=true cert=false native=0");
            oracle(
                &source,
                serde_json::json!({"kind":"boolean","value":expected}),
            );
        })
        .unwrap()
        .join()
        .unwrap();
}
fn oracle(source: &str, expected: serde_json::Value) {
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
        "REL145_NODE {}",
        serde_json::json!({"source":source,"source_utf16":source.encode_utf16().collect::<Vec<_>>(),"argv":["timeout","15","node","-e",script],"rc":output.status.code(),"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr),"version_rc":version.status.code(),"version_stdout":String::from_utf8_lossy(&version.stdout),"version_stderr":String::from_utf8_lossy(&version.stderr)})
    );
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        expected
    );
}

#[test]
fn primitive145_red_mixed() {
    boolean("'1'<2;", true);
}
#[test]
fn primitive145_red_scalar() {
    boolean("true>null;", true);
}
#[test]
fn primitive145_red_formal() {
    boolean("function f(x){return x<=null;}f(false);", true);
}
#[test]
fn primitive145_red_runtime_string() {
    boolean(
        "function f(){return 'x';}var caught=false;try{f()<2;}catch(e){caught=true;}caught;",
        false,
    );
}

// Exact selected rows are bound to this test source and REL145_SHARD receipt.
#[test]
fn primitive145_main400() {
    let shard: usize = std::env::var("REL145_SHARD")
        .expect("finite shard required")
        .parse()
        .unwrap();
    assert!(shard < 50);
    let rows: &[(&str, bool)] = &[
        ("2<2;", false),
        ("function f(a){return a<2;}f(2);", false),
        ("function f(b){return 2<b;}f(2);", false),
        ("function f(a,b){return a<b;}f(2,2);", false),
        ("2>2;", false),
        ("function f(a){return a>2;}f(2);", false),
        ("function f(b){return 2>b;}f(2);", false),
        ("function f(a,b){return a>b;}f(2,2);", false),
        ("2<=2;", true),
        ("function f(a){return a<=2;}f(2);", true),
        ("function f(b){return 2<=b;}f(2);", true),
        ("function f(a,b){return a<=b;}f(2,2);", true),
        ("2>=2;", true),
        ("function f(a){return a>=2;}f(2);", true),
        ("function f(b){return 2>=b;}f(2);", true),
        ("function f(a,b){return a>=b;}f(2,2);", true),
        ("2<true;", false),
        ("function f(a){return a<true;}f(2);", false),
        ("function f(b){return 2<b;}f(true);", false),
        ("function f(a,b){return a<b;}f(2,true);", false),
        ("2>true;", true),
        ("function f(a){return a>true;}f(2);", true),
        ("function f(b){return 2>b;}f(true);", true),
        ("function f(a,b){return a>b;}f(2,true);", true),
        ("2<=true;", false),
        ("function f(a){return a<=true;}f(2);", false),
        ("function f(b){return 2<=b;}f(true);", false),
        ("function f(a,b){return a<=b;}f(2,true);", false),
        ("2>=true;", true),
        ("function f(a){return a>=true;}f(2);", true),
        ("function f(b){return 2>=b;}f(true);", true),
        ("function f(a,b){return a>=b;}f(2,true);", true),
        ("2<null;", false),
        ("function f(a){return a<null;}f(2);", false),
        ("function f(b){return 2<b;}f(null);", false),
        ("function f(a,b){return a<b;}f(2,null);", false),
        ("2>null;", true),
        ("function f(a){return a>null;}f(2);", true),
        ("function f(b){return 2>b;}f(null);", true),
        ("function f(a,b){return a>b;}f(2,null);", true),
        ("2<=null;", false),
        ("function f(a){return a<=null;}f(2);", false),
        ("function f(b){return 2<=b;}f(null);", false),
        ("function f(a,b){return a<=b;}f(2,null);", false),
        ("2>=null;", true),
        ("function f(a){return a>=null;}f(2);", true),
        ("function f(b){return 2>=b;}f(null);", true),
        ("function f(a,b){return a>=b;}f(2,null);", true),
        ("2<undefined;", false),
        ("function f(a){return a<undefined;}f(2);", false),
        ("function f(b){return 2<b;}f(undefined);", false),
        ("function f(a,b){return a<b;}f(2,undefined);", false),
        ("2>undefined;", false),
        ("function f(a){return a>undefined;}f(2);", false),
        ("function f(b){return 2>b;}f(undefined);", false),
        ("function f(a,b){return a>b;}f(2,undefined);", false),
        ("2<=undefined;", false),
        ("function f(a){return a<=undefined;}f(2);", false),
        ("function f(b){return 2<=b;}f(undefined);", false),
        ("function f(a,b){return a<=b;}f(2,undefined);", false),
        ("2>=undefined;", false),
        ("function f(a){return a>=undefined;}f(2);", false),
        ("function f(b){return 2>=b;}f(undefined);", false),
        ("function f(a,b){return a>=b;}f(2,undefined);", false),
        ("2<'10';", true),
        ("function f(a){return a<'10';}f(2);", true),
        ("function f(b){return 2<b;}f('10');", true),
        ("function f(a,b){return a<b;}f(2,'10');", true),
        ("2>'10';", false),
        ("function f(a){return a>'10';}f(2);", false),
        ("function f(b){return 2>b;}f('10');", false),
        ("function f(a,b){return a>b;}f(2,'10');", false),
        ("2<='10';", true),
        ("function f(a){return a<='10';}f(2);", true),
        ("function f(b){return 2<=b;}f('10');", true),
        ("function f(a,b){return a<=b;}f(2,'10');", true),
        ("2>='10';", false),
        ("function f(a){return a>='10';}f(2);", false),
        ("function f(b){return 2>=b;}f('10');", false),
        ("function f(a,b){return a>=b;}f(2,'10');", false),
        ("true<2;", true),
        ("function f(a){return a<2;}f(true);", true),
        ("function f(b){return true<b;}f(2);", true),
        ("function f(a,b){return a<b;}f(true,2);", true),
        ("true>2;", false),
        ("function f(a){return a>2;}f(true);", false),
        ("function f(b){return true>b;}f(2);", false),
        ("function f(a,b){return a>b;}f(true,2);", false),
        ("true<=2;", true),
        ("function f(a){return a<=2;}f(true);", true),
        ("function f(b){return true<=b;}f(2);", true),
        ("function f(a,b){return a<=b;}f(true,2);", true),
        ("true>=2;", false),
        ("function f(a){return a>=2;}f(true);", false),
        ("function f(b){return true>=b;}f(2);", false),
        ("function f(a,b){return a>=b;}f(true,2);", false),
        ("true<true;", false),
        ("function f(a){return a<true;}f(true);", false),
        ("function f(b){return true<b;}f(true);", false),
        ("function f(a,b){return a<b;}f(true,true);", false),
        ("true>true;", false),
        ("function f(a){return a>true;}f(true);", false),
        ("function f(b){return true>b;}f(true);", false),
        ("function f(a,b){return a>b;}f(true,true);", false),
        ("true<=true;", true),
        ("function f(a){return a<=true;}f(true);", true),
        ("function f(b){return true<=b;}f(true);", true),
        ("function f(a,b){return a<=b;}f(true,true);", true),
        ("true>=true;", true),
        ("function f(a){return a>=true;}f(true);", true),
        ("function f(b){return true>=b;}f(true);", true),
        ("function f(a,b){return a>=b;}f(true,true);", true),
        ("true<null;", false),
        ("function f(a){return a<null;}f(true);", false),
        ("function f(b){return true<b;}f(null);", false),
        ("function f(a,b){return a<b;}f(true,null);", false),
        ("true>null;", true),
        ("function f(a){return a>null;}f(true);", true),
        ("function f(b){return true>b;}f(null);", true),
        ("function f(a,b){return a>b;}f(true,null);", true),
        ("true<=null;", false),
        ("function f(a){return a<=null;}f(true);", false),
        ("function f(b){return true<=b;}f(null);", false),
        ("function f(a,b){return a<=b;}f(true,null);", false),
        ("true>=null;", true),
        ("function f(a){return a>=null;}f(true);", true),
        ("function f(b){return true>=b;}f(null);", true),
        ("function f(a,b){return a>=b;}f(true,null);", true),
        ("true<undefined;", false),
        ("function f(a){return a<undefined;}f(true);", false),
        ("function f(b){return true<b;}f(undefined);", false),
        ("function f(a,b){return a<b;}f(true,undefined);", false),
        ("true>undefined;", false),
        ("function f(a){return a>undefined;}f(true);", false),
        ("function f(b){return true>b;}f(undefined);", false),
        ("function f(a,b){return a>b;}f(true,undefined);", false),
        ("true<=undefined;", false),
        ("function f(a){return a<=undefined;}f(true);", false),
        ("function f(b){return true<=b;}f(undefined);", false),
        ("function f(a,b){return a<=b;}f(true,undefined);", false),
        ("true>=undefined;", false),
        ("function f(a){return a>=undefined;}f(true);", false),
        ("function f(b){return true>=b;}f(undefined);", false),
        ("function f(a,b){return a>=b;}f(true,undefined);", false),
        ("true<'10';", true),
        ("function f(a){return a<'10';}f(true);", true),
        ("function f(b){return true<b;}f('10');", true),
        ("function f(a,b){return a<b;}f(true,'10');", true),
        ("true>'10';", false),
        ("function f(a){return a>'10';}f(true);", false),
        ("function f(b){return true>b;}f('10');", false),
        ("function f(a,b){return a>b;}f(true,'10');", false),
        ("true<='10';", true),
        ("function f(a){return a<='10';}f(true);", true),
        ("function f(b){return true<=b;}f('10');", true),
        ("function f(a,b){return a<=b;}f(true,'10');", true),
        ("true>='10';", false),
        ("function f(a){return a>='10';}f(true);", false),
        ("function f(b){return true>=b;}f('10');", false),
        ("function f(a,b){return a>=b;}f(true,'10');", false),
        ("null<2;", true),
        ("function f(a){return a<2;}f(null);", true),
        ("function f(b){return null<b;}f(2);", true),
        ("function f(a,b){return a<b;}f(null,2);", true),
        ("null>2;", false),
        ("function f(a){return a>2;}f(null);", false),
        ("function f(b){return null>b;}f(2);", false),
        ("function f(a,b){return a>b;}f(null,2);", false),
        ("null<=2;", true),
        ("function f(a){return a<=2;}f(null);", true),
        ("function f(b){return null<=b;}f(2);", true),
        ("function f(a,b){return a<=b;}f(null,2);", true),
        ("null>=2;", false),
        ("function f(a){return a>=2;}f(null);", false),
        ("function f(b){return null>=b;}f(2);", false),
        ("function f(a,b){return a>=b;}f(null,2);", false),
        ("null<true;", true),
        ("function f(a){return a<true;}f(null);", true),
        ("function f(b){return null<b;}f(true);", true),
        ("function f(a,b){return a<b;}f(null,true);", true),
        ("null>true;", false),
        ("function f(a){return a>true;}f(null);", false),
        ("function f(b){return null>b;}f(true);", false),
        ("function f(a,b){return a>b;}f(null,true);", false),
        ("null<=true;", true),
        ("function f(a){return a<=true;}f(null);", true),
        ("function f(b){return null<=b;}f(true);", true),
        ("function f(a,b){return a<=b;}f(null,true);", true),
        ("null>=true;", false),
        ("function f(a){return a>=true;}f(null);", false),
        ("function f(b){return null>=b;}f(true);", false),
        ("function f(a,b){return a>=b;}f(null,true);", false),
        ("null<null;", false),
        ("function f(a){return a<null;}f(null);", false),
        ("function f(b){return null<b;}f(null);", false),
        ("function f(a,b){return a<b;}f(null,null);", false),
        ("null>null;", false),
        ("function f(a){return a>null;}f(null);", false),
        ("function f(b){return null>b;}f(null);", false),
        ("function f(a,b){return a>b;}f(null,null);", false),
        ("null<=null;", true),
        ("function f(a){return a<=null;}f(null);", true),
        ("function f(b){return null<=b;}f(null);", true),
        ("function f(a,b){return a<=b;}f(null,null);", true),
        ("null>=null;", true),
        ("function f(a){return a>=null;}f(null);", true),
        ("function f(b){return null>=b;}f(null);", true),
        ("function f(a,b){return a>=b;}f(null,null);", true),
        ("null<undefined;", false),
        ("function f(a){return a<undefined;}f(null);", false),
        ("function f(b){return null<b;}f(undefined);", false),
        ("function f(a,b){return a<b;}f(null,undefined);", false),
        ("null>undefined;", false),
        ("function f(a){return a>undefined;}f(null);", false),
        ("function f(b){return null>b;}f(undefined);", false),
        ("function f(a,b){return a>b;}f(null,undefined);", false),
        ("null<=undefined;", false),
        ("function f(a){return a<=undefined;}f(null);", false),
        ("function f(b){return null<=b;}f(undefined);", false),
        ("function f(a,b){return a<=b;}f(null,undefined);", false),
        ("null>=undefined;", false),
        ("function f(a){return a>=undefined;}f(null);", false),
        ("function f(b){return null>=b;}f(undefined);", false),
        ("function f(a,b){return a>=b;}f(null,undefined);", false),
        ("null<'10';", true),
        ("function f(a){return a<'10';}f(null);", true),
        ("function f(b){return null<b;}f('10');", true),
        ("function f(a,b){return a<b;}f(null,'10');", true),
        ("null>'10';", false),
        ("function f(a){return a>'10';}f(null);", false),
        ("function f(b){return null>b;}f('10');", false),
        ("function f(a,b){return a>b;}f(null,'10');", false),
        ("null<='10';", true),
        ("function f(a){return a<='10';}f(null);", true),
        ("function f(b){return null<=b;}f('10');", true),
        ("function f(a,b){return a<=b;}f(null,'10');", true),
        ("null>='10';", false),
        ("function f(a){return a>='10';}f(null);", false),
        ("function f(b){return null>=b;}f('10');", false),
        ("function f(a,b){return a>=b;}f(null,'10');", false),
        ("undefined<2;", false),
        ("function f(a){return a<2;}f(undefined);", false),
        ("function f(b){return undefined<b;}f(2);", false),
        ("function f(a,b){return a<b;}f(undefined,2);", false),
        ("undefined>2;", false),
        ("function f(a){return a>2;}f(undefined);", false),
        ("function f(b){return undefined>b;}f(2);", false),
        ("function f(a,b){return a>b;}f(undefined,2);", false),
        ("undefined<=2;", false),
        ("function f(a){return a<=2;}f(undefined);", false),
        ("function f(b){return undefined<=b;}f(2);", false),
        ("function f(a,b){return a<=b;}f(undefined,2);", false),
        ("undefined>=2;", false),
        ("function f(a){return a>=2;}f(undefined);", false),
        ("function f(b){return undefined>=b;}f(2);", false),
        ("function f(a,b){return a>=b;}f(undefined,2);", false),
        ("undefined<true;", false),
        ("function f(a){return a<true;}f(undefined);", false),
        ("function f(b){return undefined<b;}f(true);", false),
        ("function f(a,b){return a<b;}f(undefined,true);", false),
        ("undefined>true;", false),
        ("function f(a){return a>true;}f(undefined);", false),
        ("function f(b){return undefined>b;}f(true);", false),
        ("function f(a,b){return a>b;}f(undefined,true);", false),
        ("undefined<=true;", false),
        ("function f(a){return a<=true;}f(undefined);", false),
        ("function f(b){return undefined<=b;}f(true);", false),
        ("function f(a,b){return a<=b;}f(undefined,true);", false),
        ("undefined>=true;", false),
        ("function f(a){return a>=true;}f(undefined);", false),
        ("function f(b){return undefined>=b;}f(true);", false),
        ("function f(a,b){return a>=b;}f(undefined,true);", false),
        ("undefined<null;", false),
        ("function f(a){return a<null;}f(undefined);", false),
        ("function f(b){return undefined<b;}f(null);", false),
        ("function f(a,b){return a<b;}f(undefined,null);", false),
        ("undefined>null;", false),
        ("function f(a){return a>null;}f(undefined);", false),
        ("function f(b){return undefined>b;}f(null);", false),
        ("function f(a,b){return a>b;}f(undefined,null);", false),
        ("undefined<=null;", false),
        ("function f(a){return a<=null;}f(undefined);", false),
        ("function f(b){return undefined<=b;}f(null);", false),
        ("function f(a,b){return a<=b;}f(undefined,null);", false),
        ("undefined>=null;", false),
        ("function f(a){return a>=null;}f(undefined);", false),
        ("function f(b){return undefined>=b;}f(null);", false),
        ("function f(a,b){return a>=b;}f(undefined,null);", false),
        ("undefined<undefined;", false),
        ("function f(a){return a<undefined;}f(undefined);", false),
        ("function f(b){return undefined<b;}f(undefined);", false),
        ("function f(a,b){return a<b;}f(undefined,undefined);", false),
        ("undefined>undefined;", false),
        ("function f(a){return a>undefined;}f(undefined);", false),
        ("function f(b){return undefined>b;}f(undefined);", false),
        ("function f(a,b){return a>b;}f(undefined,undefined);", false),
        ("undefined<=undefined;", false),
        ("function f(a){return a<=undefined;}f(undefined);", false),
        ("function f(b){return undefined<=b;}f(undefined);", false),
        (
            "function f(a,b){return a<=b;}f(undefined,undefined);",
            false,
        ),
        ("undefined>=undefined;", false),
        ("function f(a){return a>=undefined;}f(undefined);", false),
        ("function f(b){return undefined>=b;}f(undefined);", false),
        (
            "function f(a,b){return a>=b;}f(undefined,undefined);",
            false,
        ),
        ("undefined<'10';", false),
        ("function f(a){return a<'10';}f(undefined);", false),
        ("function f(b){return undefined<b;}f('10');", false),
        ("function f(a,b){return a<b;}f(undefined,'10');", false),
        ("undefined>'10';", false),
        ("function f(a){return a>'10';}f(undefined);", false),
        ("function f(b){return undefined>b;}f('10');", false),
        ("function f(a,b){return a>b;}f(undefined,'10');", false),
        ("undefined<='10';", false),
        ("function f(a){return a<='10';}f(undefined);", false),
        ("function f(b){return undefined<=b;}f('10');", false),
        ("function f(a,b){return a<=b;}f(undefined,'10');", false),
        ("undefined>='10';", false),
        ("function f(a){return a>='10';}f(undefined);", false),
        ("function f(b){return undefined>=b;}f('10');", false),
        ("function f(a,b){return a>=b;}f(undefined,'10');", false),
        ("'10'<2;", false),
        ("function f(a){return a<2;}f('10');", false),
        ("function f(b){return '10'<b;}f(2);", false),
        ("function f(a,b){return a<b;}f('10',2);", false),
        ("'10'>2;", true),
        ("function f(a){return a>2;}f('10');", true),
        ("function f(b){return '10'>b;}f(2);", true),
        ("function f(a,b){return a>b;}f('10',2);", true),
        ("'10'<=2;", false),
        ("function f(a){return a<=2;}f('10');", false),
        ("function f(b){return '10'<=b;}f(2);", false),
        ("function f(a,b){return a<=b;}f('10',2);", false),
        ("'10'>=2;", true),
        ("function f(a){return a>=2;}f('10');", true),
        ("function f(b){return '10'>=b;}f(2);", true),
        ("function f(a,b){return a>=b;}f('10',2);", true),
        ("'10'<true;", false),
        ("function f(a){return a<true;}f('10');", false),
        ("function f(b){return '10'<b;}f(true);", false),
        ("function f(a,b){return a<b;}f('10',true);", false),
        ("'10'>true;", true),
        ("function f(a){return a>true;}f('10');", true),
        ("function f(b){return '10'>b;}f(true);", true),
        ("function f(a,b){return a>b;}f('10',true);", true),
        ("'10'<=true;", false),
        ("function f(a){return a<=true;}f('10');", false),
        ("function f(b){return '10'<=b;}f(true);", false),
        ("function f(a,b){return a<=b;}f('10',true);", false),
        ("'10'>=true;", true),
        ("function f(a){return a>=true;}f('10');", true),
        ("function f(b){return '10'>=b;}f(true);", true),
        ("function f(a,b){return a>=b;}f('10',true);", true),
        ("'10'<null;", false),
        ("function f(a){return a<null;}f('10');", false),
        ("function f(b){return '10'<b;}f(null);", false),
        ("function f(a,b){return a<b;}f('10',null);", false),
        ("'10'>null;", true),
        ("function f(a){return a>null;}f('10');", true),
        ("function f(b){return '10'>b;}f(null);", true),
        ("function f(a,b){return a>b;}f('10',null);", true),
        ("'10'<=null;", false),
        ("function f(a){return a<=null;}f('10');", false),
        ("function f(b){return '10'<=b;}f(null);", false),
        ("function f(a,b){return a<=b;}f('10',null);", false),
        ("'10'>=null;", true),
        ("function f(a){return a>=null;}f('10');", true),
        ("function f(b){return '10'>=b;}f(null);", true),
        ("function f(a,b){return a>=b;}f('10',null);", true),
        ("'10'<undefined;", false),
        ("function f(a){return a<undefined;}f('10');", false),
        ("function f(b){return '10'<b;}f(undefined);", false),
        ("function f(a,b){return a<b;}f('10',undefined);", false),
        ("'10'>undefined;", false),
        ("function f(a){return a>undefined;}f('10');", false),
        ("function f(b){return '10'>b;}f(undefined);", false),
        ("function f(a,b){return a>b;}f('10',undefined);", false),
        ("'10'<=undefined;", false),
        ("function f(a){return a<=undefined;}f('10');", false),
        ("function f(b){return '10'<=b;}f(undefined);", false),
        ("function f(a,b){return a<=b;}f('10',undefined);", false),
        ("'10'>=undefined;", false),
        ("function f(a){return a>=undefined;}f('10');", false),
        ("function f(b){return '10'>=b;}f(undefined);", false),
        ("function f(a,b){return a>=b;}f('10',undefined);", false),
        ("'10'<'10';", false),
        ("function f(a){return a<'10';}f('10');", false),
        ("function f(b){return '10'<b;}f('10');", false),
        ("function f(a,b){return a<b;}f('10','10');", false),
        ("'10'>'10';", false),
        ("function f(a){return a>'10';}f('10');", false),
        ("function f(b){return '10'>b;}f('10');", false),
        ("function f(a,b){return a>b;}f('10','10');", false),
        ("'10'<='10';", true),
        ("function f(a){return a<='10';}f('10');", true),
        ("function f(b){return '10'<=b;}f('10');", true),
        ("function f(a,b){return a<=b;}f('10','10');", true),
        ("'10'>='10';", true),
        ("function f(a){return a>='10';}f('10');", true),
        ("function f(b){return '10'>=b;}f('10');", true),
        ("function f(a,b){return a>=b;}f('10','10');", true),
    ];
    assert_eq!(rows.len(), 400);
    for (id, (source, expected)) in rows.iter().enumerate().skip(shard * 8).take(8) {
        eprintln!(
            "REL145_MAIN_ROW {}",
            serde_json::json!({"id":id,"shard":shard,"source":source,"expected":expected})
        );
        boolean(source, *expected);
    }
}

#[test]
fn primitive145_extensions() {
    let shard: usize = std::env::var("REL145_EXT_SHARD")
        .expect("finite extension shard required")
        .parse()
        .unwrap();
    assert!(shard < 7);
    let rows: &[(&str, bool)] = &[
        (
            "function f(a,b){return (a<b)===(false<null)&&(a>b)===(false>null)&&(a<=b)===(false<=null)&&(a>=b)===(false>=null);}f(false,null);",
            true,
        ),
        (
            "function f(a,b){return (a<b)===(true<null)&&(a>b)===(true>null)&&(a<=b)===(true<=null)&&(a>=b)===(true>=null);}f(true,null);",
            true,
        ),
        (
            "function f(a,b){return (a<b)===(-0<false)&&(a>b)===(-0>false)&&(a<=b)===(-0<=false)&&(a>=b)===(-0>=false);}f(-0,false);",
            true,
        ),
        (
            "function f(a,b){return (a<b)===((0/0)<1)&&(a>b)===((0/0)>1)&&(a<=b)===((0/0)<=1)&&(a>=b)===((0/0)>=1);}f((0/0),1);",
            true,
        ),
        (
            "function f(a,b){return (a<b)===(1<(0/0))&&(a>b)===(1>(0/0))&&(a<=b)===(1<=(0/0))&&(a>=b)===(1>=(0/0));}f(1,(0/0));",
            true,
        ),
        (
            "function f(a,b){return (a<b)===(Infinity<-Infinity)&&(a>b)===(Infinity>-Infinity)&&(a<=b)===(Infinity<=-Infinity)&&(a>=b)===(Infinity>=-Infinity);}f(Infinity,-Infinity);",
            true,
        ),
        (
            "function f(a,b){return (a<b)===(''<false)&&(a>b)===(''>false)&&(a<=b)===(''<=false)&&(a>=b)===(''>=false);}f('',false);",
            true,
        ),
        (
            "function f(a,b){return (a<b)===('  2  '<3)&&(a>b)===('  2  '>3)&&(a<=b)===('  2  '<=3)&&(a>=b)===('  2  '>=3);}f('  2  ',3);",
            true,
        ),
        (
            "function f(a,b){return (a<b)===('x'<2)&&(a>b)===('x'>2)&&(a<=b)===('x'<=2)&&(a>=b)===('x'>=2);}f('x',2);",
            true,
        ),
        (
            "function f(a,b){return (a<b)===('Infinity'<Infinity)&&(a>b)===('Infinity'>Infinity)&&(a<=b)===('Infinity'<=Infinity)&&(a>=b)===('Infinity'>=Infinity);}f('Infinity',Infinity);",
            true,
        ),
        (
            "function f(a,b){return (a<b)===('0x10'<16)&&(a>b)===('0x10'>16)&&(a<=b)===('0x10'<=16)&&(a>=b)===('0x10'>=16);}f('0x10',16);",
            true,
        ),
        (
            "function f(a,b){return (a<b)===('\\u00a0\\u2028\\u3000'<false)&&(a>b)===('\\u00a0\\u2028\\u3000'>false)&&(a<=b)===('\\u00a0\\u2028\\u3000'<=false)&&(a>=b)===('\\u00a0\\u2028\\u3000'>=false);}f('\\u00a0\\u2028\\u3000',false);",
            true,
        ),
        (
            "function f(a,b){return (a<b)===('a\\0'<'a\\0b')&&(a>b)===('a\\0'>'a\\0b')&&(a<=b)===('a\\0'<='a\\0b')&&(a>=b)===('a\\0'>='a\\0b');}f('a\\0','a\\0b');",
            true,
        ),
        (
            "function f(a,b){return (a<b)===('\\ud800'<'\\ud801')&&(a>b)===('\\ud800'>'\\ud801')&&(a<=b)===('\\ud800'<='\\ud801')&&(a>=b)===('\\ud800'>='\\ud801');}f('\\ud800','\\ud801');",
            true,
        ),
        (
            "function f(a,b){return (a<b)===('\\ud83d\\ude00'<'\\ue000')&&(a>b)===('\\ud83d\\ude00'>'\\ue000')&&(a<=b)===('\\ud83d\\ude00'<='\\ue000')&&(a>=b)===('\\ud83d\\ude00'>='\\ue000');}f('\\ud83d\\ude00','\\ue000');",
            true,
        ),
        (
            "function f(a,b){return (a<b)===('10'<'2')&&(a>b)===('10'>'2')&&(a<=b)===('10'<='2')&&(a>=b)===('10'>='2');}f('10','2');",
            true,
        ),
        ("function f(a,b){'use strict';return a<b;}f('2',3);", true),
        (
            "'use strict';function f(a,b){return a>=b;}f(false,null);",
            true,
        ),
        ("function f(undefined){return undefined<2;}f(1);", true),
        (
            "function f(){var undefined='2';return undefined<3;}f();",
            true,
        ),
        (
            "function f(a,b){var x=a;var y=x;return (y=b)<3;}f(99,'2');",
            true,
        ),
        ("var x=true?'2':1;x<3;", true),
        ("var x=false||'2';x<=2;", true),
        ("var x=true&&'2';x>=2;", true),
        ("var x=0;(x=1,'2')<(x=x+1,3)&&x===2;", true),
        (
            "function f(){try{throw '2';}catch(e){return e<3;}}f();",
            true,
        ),
        (
            "function f(){try{throw false;}catch(e){return e>=null;}}f();",
            true,
        ),
        ("function f(){return '2';}var g=f;g()<3;", true),
        ("var o={a:'2',b:3};o.a<o.b;", true),
        (
            "var o={n:0,get a(){'use strict';this.n=this.n+1;return '2';},get b(){'use strict';this.n=this.n*10+2;return 3;}};var r=o.a<o.b;r&&o.n===12;",
            true,
        ),
        ("var x=1;var n=0;while(x<2){n=n+1;x='x';}n===1;", true),
        ("function f(a,b){return a<b;}f('10','2');", true),
        ("function f(a,b){return a<=b;}f('a\\0','a\\0');", true),
        (
            "function fail(){throw 'left';}var n=0;var ok=false;try{fail()<(n=99,'2');}catch(e){ok=e==='left';}ok&&n===0;",
            true,
        ),
        (
            "function object(){return {};}function fail(){throw 'right';}var ok=false;try{object()<fail();}catch(e){ok=e==='right';}ok;",
            true,
        ),
        ("var n=0;var r=(n=n*10+1,'2')<(n=n*10+2,3);n===12;", true),
        (
            "function f(a,b){return a<b;}var done=false;try{done=f('x',3)===false;}finally{done=done&&true;}done;",
            true,
        ),
        (
            "function fail(){throw 'left';}var n=0;var ok=false;try{fail()>(n=99,'2');}catch(e){ok=e==='left';}ok&&n===0;",
            true,
        ),
        (
            "function object(){return {};}function fail(){throw 'right';}var ok=false;try{object()>fail();}catch(e){ok=e==='right';}ok;",
            true,
        ),
        ("var n=0;var r=(n=n*10+1,'2')>(n=n*10+2,3);n===12;", true),
        (
            "function f(a,b){return a>b;}var done=false;try{done=f('x',3)===false;}finally{done=done&&true;}done;",
            true,
        ),
        (
            "function fail(){throw 'left';}var n=0;var ok=false;try{fail()<=(n=99,'2');}catch(e){ok=e==='left';}ok&&n===0;",
            true,
        ),
        (
            "function object(){return {};}function fail(){throw 'right';}var ok=false;try{object()<=fail();}catch(e){ok=e==='right';}ok;",
            true,
        ),
        ("var n=0;var r=(n=n*10+1,'2')<=(n=n*10+2,3);n===12;", true),
        (
            "function f(a,b){return a<=b;}var done=false;try{done=f('x',3)===false;}finally{done=done&&true;}done;",
            true,
        ),
        (
            "function fail(){throw 'left';}var n=0;var ok=false;try{fail()>=(n=99,'2');}catch(e){ok=e==='left';}ok&&n===0;",
            true,
        ),
        (
            "function object(){return {};}function fail(){throw 'right';}var ok=false;try{object()>=fail();}catch(e){ok=e==='right';}ok;",
            true,
        ),
        ("var n=0;var r=(n=n*10+1,'2')>=(n=n*10+2,3);n===12;", true),
        (
            "function f(a,b){return a>=b;}var done=false;try{done=f('x',3)===false;}finally{done=done&&true;}done;",
            true,
        ),
    ];
    for (id, (source, expected)) in rows.iter().enumerate().skip(shard * 8).take(8) {
        eprintln!(
            "REL145_EXTENSION_ROW {}",
            serde_json::json!({"id":id,"shard":shard,"source":source,"expected":expected})
        );
        boolean(source, *expected);
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
fn primitive145_demand_and_refusals() {
    for (source, parser, compare) in [
        ("true<null;", 0, 0),
        ("false>=null;", 0, 0),
        ("1<2;", 0, 0),
        ("'1'<'2';", 0, 1),
        ("'1'<2;", 1, 0),
        ("function f(a){return a<2;}f(1);", 1, 0),
        ("function f(a){return a<'2';}f(1);", 1, 1),
        ("function f(a,b){return a<b;}f(1,2);", 1, 1),
        ("Math.abs('1');function f(a){return a<'2';}f(1);", 1, 1),
        ("+'1';function f(a){return a<'2';}f(1);", 1, 1),
    ] {
        let ir = parse(source).unwrap();
        let count = |name| ir.functions.iter().filter(|f| f.name == name).count();
        assert_eq!(count("__compat_es5_numeric_scan"), parser, "{source}");
        assert_eq!(count("__compat_es5_string_compare"), compare, "{source}");
        assert_eq!(count("__compat_es5_number_string_managed"), 0, "{source}");
        assert_eq!(count("__compat_es5_managed_to_units"), 0, "{source}");
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
        if parser == 0 && compare == 0 {
            assert!(ir.functions.is_empty());
        }
        eprintln!(
            "REL145_DEMAND source={source:?} parser={parser} compare={compare} unique_indices=true locals={}",
            ir.locals
        );
    }
    for source in [
        "({})<1;",
        "[]<=1;",
        "(function(){})>1;",
        "function f(){var undefined={};return undefined>=1;}f();",
        "if(false){({})<1;}",
        "false&&(({})<1);",
        "true?(1<2):(({})<1);",
        "function f(x){return x==1;}f(true);",
    ] {
        for prefix in ["", "'unrelated';"] {
            let equality = source == "function f(x){return x==1;}f(true);";
            let source = format!("{prefix}{source}");
            if equality {
                boolean(&source, true);
                continue;
            }
            let e = parse(&source).expect_err("nonprimitive/skipped/equality source refusal");
            assert!(e.span.is_some());
            assert!(
                e.message.contains("nonprimitive")
                    || e.message == "unsupported JavaScript ES5 P0 binary operator"
            );
            eprintln!("REL145_REFUSED source={source:?} error={e:?}");
        }
    }
}
#[test]
fn primitive145_float_typemap() {
    for source in ["true<null;", "false>=undefined;"] {
        eprintln!("REL145_TYPEMAP_SOURCE source={source:?}");
        let m =
            pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source).unwrap();
        let p = m.program();
        let pairs: Vec<_> = p
            .type_map
            .as_ref()
            .unwrap()
            .operand_types
            .iter()
            .filter(|(ip, _)| p.code[**ip] == vm::bytecode::OpCode::Clt as u8)
            .collect();
        assert_eq!(pairs.len(), 1);
        assert_eq!(*pairs[0].1, (vm::ValueType::Float, vm::ValueType::Float));
        eprintln!("REL145_FLOAT_TYPEMAP source={source:?} operands={pairs:?}");
    }
}

struct ParserObserver {
    gc: bool,
    fatal: bool,
}
impl vm::SourcePlugin for ParserObserver {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut d = vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
        d.id = vm::FrontendId::new("relational145-parser-observer");
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
            expr: call(mark, vec![E::String("REL145_PARSER_ENTER".into())]),
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

struct ParserCounter(std::sync::Arc<std::sync::atomic::AtomicUsize>);
impl vm::HostFunction for ParserCounter {
    fn call(&mut self, _vm: &mut vm::Vm, args: &[vm::Value]) -> vm::VmResult<vm::CallOutcome> {
        assert_eq!(args.len(), 1);
        if args[0] == vm::Value::string("REL145_PARSER_ENTER") {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(vm::CallOutcome::Return(vm::CallReturn::one(
                vm::Value::Null,
            )))
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
) -> (vm::Vm, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    eprintln!("REL145_OBSERVER_SOURCE source={source:?} observer={id}");
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
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
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
        vec![
            vm::Expr::Borrow(Box::new(vm::Expr::Var(slot))),
            vm::Expr::String(name.into()),
        ],
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

struct RelationObserver(u8);
impl vm::SourcePlugin for RelationObserver {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut d = vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
        d.id = vm::FrontendId::new("relational145-comparator-observer");
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
        let cmp = helper(&ir, "__compat_es5_string_compare");
        if self.0 == 1 {
            ir.function_impls
                .get_mut(&cmp)
                .unwrap()
                .body_stmts
                .insert(0, check(E::Bool(false)));
            eprintln!("REL145_ABRUPT_HELPER_MUTANT source={source:?} comparator_entry_fatal=true");
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
        let params = ir.function_impls[&cmp].param_slots.clone();
        assert_eq!(params.len(), 3);
        let (c, l, r) = (params[0], params[1], params[2]);
        let original = &ir.function_impls[&cmp];
        let debug = format!("{original:?}");
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
        for expr in [
            E::String("REL145_INPUT_ROOTS".into()),
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
            _ => {}
        }
        // Insert after scalar declarations, immediately before original checks.
        let f = ir.function_impls.get_mut(&cmp).unwrap();
        let at = f
            .body_stmts
            .iter()
            .position(|s| matches!(s, S::Expr { .. }))
            .unwrap();
        f.body_stmts.splice(at..at, pre);
        eprintln!(
            "REL145_ROOT_OBSERVER source={source:?} mode={} input_rows_exact_tag_slot_generation=true rhs_node_begin_gc=true comparator_no_private_frame=true",
            self.0
        );
        Ok(ir)
    }
}

fn observed_relation(
    source: &str,
    observer: &'static dyn vm::SourcePlugin,
    id: &'static str,
    success: bool,
    parser_calls: usize,
) {
    let source = source.to_owned();
    std::thread::Builder::new().stack_size(33_554_432).spawn(move || {
        let (mut m,counter)=probe_vm(&source,observer,id);
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst),0);
        let status=m.run();
        let calls=counter.load(std::sync::atomic::Ordering::SeqCst);
        eprintln!("REL145_OBSERVER_VM source={source:?} status={status:?} parser_calls={calls} aot={} jit={} stack={:?}",m.aot_exec_count(),m.jit_native_exec_count(),m.stack());
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
#[test]
fn primitive145_parser_both_strings_bypass() {
    static OBS: ParserObserver = ParserObserver {
        gc: false,
        fatal: true,
    };
    for (op, wanted) in [("<", true), (">", false), ("<=", true), (">=", false)] {
        let source = format!("function f(a,b){{return a{op}b;}}f('10','2')==={wanted};");
        observed_relation(&source, &OBS, "relational145-parser-observer", true, 0);
        oracle(&source, serde_json::json!({"kind":"boolean","value":true}));
    }
}
#[test]
fn primitive145_guard_conversion_order() {
    static OBS: ParserObserver = ParserObserver {
        gc: false,
        fatal: false,
    };
    for op in ["<", ">", "<=", ">="] {
        for (args, calls) in [("'2',{}", 1), ("{},'2'", 0)] {
            let source = format!("function f(a,b){{return a{op}b;}}f({args});");
            observed_relation(&source, &OBS, "relational145-parser-observer", false, calls);
        }
    }
}
#[test]
fn primitive145_parser_gc_roots() {
    static OBS: ParserObserver = ParserObserver {
        gc: true,
        fatal: false,
    };
    for (op, wanted) in [("<", true), (">", false), ("<=", true), (">=", false)] {
        let source = format!(
            "function f(a,b){{return a{op}b;}}var s='1';var alias=s;var r=f(s,2);r==={wanted}&&s===alias;"
        );
        observed_relation(&source, &OBS, "relational145-parser-observer", true, 1);
        oracle(&source, serde_json::json!({"kind":"boolean","value":true}));
    }
}
#[test]
fn primitive145_comparator_gc_roots() {
    static OBS: RelationObserver = RelationObserver(0);
    for (op, wanted) in [("<", true), (">", false), ("<=", true), (">=", false)] {
        let source = format!(
            "function f(){{return '10';}}function g(){{return '2';}}(f(){op}g())==={wanted};"
        );
        observed_relation(&source, &OBS, "relational145-comparator-observer", true, 0);
        oracle(&source, serde_json::json!({"kind":"boolean","value":true}));
    }
}
#[test]
fn primitive145_runtime_nonprimitive_guards() {
    static OBS: ParserObserver = ParserObserver {
        gc: false,
        fatal: false,
    };
    for source in [
        "function f(a,b){return a<b;}f({},1);",
        "function f(a,b){return a>b;}f([],1);",
        "function f(a,b){return a>=b;}function g(){}f(g,1);",
    ] {
        observed_relation(source, &OBS, "relational145-parser-observer", false, 0);
    }
}
#[test]
fn primitive145_unicode3_whitespace_qualification() {
    // ES5.1 WhiteSpace uses Unicode3 category Zs, including U+200B. Current
    // Node excludes U+200B. Preserve both observations with distinct expected values.
    let source = "function f(a,b){return a<b;}f('\\u200b',1);";
    oracle(source, serde_json::json!({"kind":"boolean","value":false}));
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            eprintln!("REL145_NORMATIVE_SOURCE source={source:?} es51=true modern_node=false");
            let mut m =
                pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
                    .unwrap();
            m.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*m.jit_config()
            });
            assert!(!m.jit_config().enabled);
            assert_eq!((m.aot_exec_count(), m.jit_native_exec_count()), (0, 0));
            m.set_fuel(10_000_000);
            let status = m.run();
            eprintln!(
                "REL145_NORMATIVE_VM source={source:?} status={status:?} stack={:?}",
                m.stack()
            );
            assert_eq!(status.unwrap(), vm::VmStatus::Halted);
            let vm::Value::Map(b) = m.stack().last().unwrap() else {
                panic!("Boolean")
            };
            assert_eq!(
                b.get(&vm::Value::string("flag")),
                Some(&vm::Value::Bool(true))
            );
            assert_eq!((m.aot_exec_count(), m.jit_native_exec_count()), (0, 0));
            reset(&mut m);
        })
        .unwrap()
        .join()
        .unwrap();
}
