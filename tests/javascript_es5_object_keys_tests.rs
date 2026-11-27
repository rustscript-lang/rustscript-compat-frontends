//! Public `Object.keys` on the certified, JIT-disabled interpreter.
//!
//! Every case is a real guest source through the production JS frontend; the
//! returned numeric code is 0 only when the complete expected behavior holds.
//! Dense-Array key enumeration, own-row order, enumerable filtering and the
//! reference-retention rules are checked through actual VM execution. Result
//! sizes are asserted with a direct `.length` read and, additionally, with
//! `for-of` consumption: a dynamically produced dense Array (call result,
//! method result, parameter alias) dispatches `.length` on the runtime boxed
//! tag and live kind-1/kind-6 header, while String, Function, ordinary-object
//! getter and nullish-error `length` reads keep their established paths.
use pd_vm_compat_frontends::{JAVASCRIPT_ES5_PLUGIN, javascript_es5_flavor};
use vm::{HostFunctionRegistry, Value, VmStatus};
#[path = "javascript_es5/object_keys_observer.rs"]
mod object_keys_observer;

fn free_floor() {
    let text = std::fs::read_to_string("/proc/meminfo").expect("meminfo");
    let kb = text
        .lines()
        .find_map(|line| line.strip_prefix("MemAvailable:"))
        .expect("MemAvailable")
        .split_whitespace()
        .next()
        .expect("kb")
        .parse::<u64>()
        .expect("kb number");
    assert!(
        kb.saturating_mul(1024) >= 8_589_934_592,
        "memory free floor below 8589934592 bytes: {kb}kB"
    );
    let output = std::process::Command::new("df")
        .args(["-B1", "/mnt/TEMP"])
        .output()
        .expect("df");
    let text = String::from_utf8(output.stdout).expect("df utf8");
    let available = text
        .lines()
        .last()
        .and_then(|line| line.split_whitespace().nth(3))
        .and_then(|field| field.parse::<u64>().ok())
        .expect("df available bytes");
    assert!(
        available >= 8_589_934_592,
        "allocated filesystem free floor below 8589934592 bytes: {available}"
    );
}

fn field(value: &Value, name: &str) -> Value {
    let Value::Map(map) = value else {
        panic!("boxed value: {value:?}")
    };
    map.get(&Value::string(name))
        .unwrap_or_else(|| panic!("boxed field {name}"))
        .clone()
}

fn prepare(
    source: &str,
    plugin: &'static dyn vm::SourcePlugin,
    flavor: vm::SourceFlavor,
) -> vm::Vm {
    free_floor();
    eprintln!("OBJECT_KEYS_SOURCE {source}");
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_host_api_catalog(catalog.clone())
        .with_source_plugin(plugin);
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source, flavor, options,
    )
    .unwrap_or_else(|error| panic!("source must compile: {source}\n{error:?}"));
    eprintln!("OBJECT_KEYS_COMPILED");
    let mut machine = compiled.into_vm().expect("certified VM");
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..vm::JitConfig::default()
    });
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("owned arrays");
    let mut registry = HostFunctionRegistry::restricted();
    for module in [
        pd_vm_compat_frontends::runtime::array::typescript_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module(),
        pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module(),
        pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module(),
    ] {
        module
            .install_from_catalog(&mut registry, &catalog)
            .expect("exact catalog");
    }
    registry.allow_builtin("math::floor").expect("floor");
    registry.allow_builtin("bytes::from_utf8").expect("utf8");
    registry.bind_vm_cached(&mut machine).expect("bind");
    machine.set_fuel(10_000_000);
    machine
}

fn execute_on(
    source: &str,
    plugin: &'static dyn vm::SourcePlugin,
    flavor: vm::SourceFlavor,
) -> (vm::Vm, Result<VmStatus, vm::VmError>) {
    let owned = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let mut machine = prepare(&owned, plugin, flavor);
            let before = machine.get_fuel();
            assert!(
                matches!(before, Some(fuel) if fuel <= 10_000_000),
                "interpreter fuel {before:?}"
            );
            let status = machine.run();
            let after = machine.get_fuel();
            eprintln!(
                "OBJECT_KEYS_VM status={status:?} jit_enabled={} aot={} jit_native={} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
            );
            if status.is_err() || matches!(status, Ok(VmStatus::Yielded)) {
                let program = machine.program();
                let ip = machine.ip();
                let op = vm::OpCode::try_from(program.code[ip]).ok();
                let owner = program
                    .script_functions
                    .iter()
                    .enumerate()
                    .find(|(_, function)| {
                        function.entry_ip as usize <= ip && ip < function.end_ip as usize
                    })
                    .map(|(index, function)| (index, function.entry_ip, function.end_ip));
                eprintln!(
                    "OBJECT_KEYS_SITE ip={ip} op={op:?} owner={owner:?} stack={:?}",
                    machine.stack()
                );
            }
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.aot_exec_count(), machine.jit_native_exec_count()),
                (0, 0)
            );
            assert!(
                matches!((before, after), (Some(left), Some(right)) if right <= left && right <= 10_000_000)
            );
            assert!(machine
                .capture_certification_stats()
                .is_some_and(|stats| stats.valid));
            let resources = machine.host_context().resource_count();
            assert!(resources <= 1, "at most one real B1 realm");
            (machine, status)
        })
        .expect("stack")
        .join()
        .expect("test thread")
}

fn execute(source: &str) -> Value {
    let (machine, status) = execute_on(source, &JAVASCRIPT_ES5_PLUGIN, javascript_es5_flavor());
    let status = status.unwrap_or_else(|error| panic!("{source}: {error:?}"));
    assert_eq!(status, VmStatus::Halted, "{source}");
    machine.stack().last().expect("completion").clone()
}

fn case_code(source: &str) -> i64 {
    let result = execute(source);
    let tag = field(&result, "tag");
    match field(&result, "number") {
        Value::Float(number) => number as i64,
        Value::Int(number) => number,
        other => panic!("case: {other:?} tag={tag:?}\n{source}"),
    }
}

fn execute_true(source: &str) {
    let result = execute(source);
    assert_eq!(field(&result, "tag"), Value::Int(2), "{source}");
    assert_eq!(field(&result, "flag"), Value::Bool(true), "{source}");
}

#[test]
fn object_keys_basic_own_enumerable() {
    let code = case_code(
        "var n=0;var k=Object.keys({a:1,b:2});if(!(k.length===2&&k[0]===\"a\"&&k[1]===\"b\"))n=1;var c=0;for(var x of k){c=c+1;}if(n===0&&c!==2)n=2;n;",
    );
    assert_eq!(code, 0, "public Object.keys own enumerable string keys");
}

#[test]
fn object_keys_empty_object() {
    let code = case_code(
        "var n=0;var k=Object.keys({});if(!(k.length===0&&k[0]===undefined))n=1;var c=0;for(var x of k){c=c+1;}if(n===0&&c!==0)n=2;n;",
    );
    assert_eq!(code, 0, "empty object yields the empty String array");
}

#[test]
fn object_keys_own_vs_inherited_and_null_proto() {
    let code = case_code(
        "var n=0;var p={x:1};var o=Object.create(p);o.y=2;var k=Object.keys(o);if(!(k[0]===\"y\"&&k[1]===undefined))n=1;var z=Object.create(null);z.a=1;var kz=Object.keys(z);if(n===0&&!(kz[0]===\"a\"))n=2;n;",
    );
    assert_eq!(code, 0, "own rows only, null prototype allowed");
}

#[test]
fn object_keys_enumerable_false_excluded() {
    let code = case_code(
        "var n=0;var o={a:1};Object.defineProperty(o,\"h\",{value:2,enumerable:false,writable:true,configurable:true});var k=Object.keys(o);if(!(k[0]===\"a\"&&k[1]===undefined))n=1;var c=0;for(var x of k){c=c+1;}if(n===0&&c!==1)n=2;n;",
    );
    assert_eq!(code, 0, "non-enumerable own rows are skipped");
}

#[test]
fn object_keys_getters_never_run() {
    let code = case_code(
        "var n=0;var o={h:0,get g(){\"use strict\";this.h=this.h+1;return 1;}};var k=Object.keys(o);if(!(k[0]===\"h\"&&k[1]===\"g\"&&o.h===0))n=1;var t={get boom(){\"use strict\";throw 9;}};var kt=Object.keys(t);if(n===0&&!(kt[0]===\"boom\"))n=2;n;",
    );
    assert_eq!(code, 0, "accessor rows are reported without invocation");
}

#[test]
fn object_keys_delete_and_readd_order() {
    let code = case_code(
        "var n=0;var o={a:1,b:2};delete o.a;o.c=3;var k=Object.keys(o);if(!(k[0]===\"b\"&&k[1]===\"c\"&&k[2]===undefined))n=1;var q={a:1,b:2};delete q.b;q.b=9;var kq=Object.keys(q);if(n===0&&!(kq[0]===\"a\"&&kq[1]===\"b\"))n=2;n;",
    );
    assert_eq!(code, 0, "own-row order after delete and re-add");
}

#[test]
fn object_keys_numeric_name_order_basis() {
    // Own-row storage order: the pinned mquickjs iterates property slots in
    // insertion order and skips deleted rows, so numeric-looking names are not
    // sorted into a modern integer-first block. This is the recorded basis; the
    // divergence from modern Node ordering is deliberate and is not permuted
    // into the expected value.
    let code = case_code(
        "var n=0;var o={b:1,2:2,a:3};var k=Object.keys(o);if(!(k[0]===\"b\"&&k[1]===\"2\"&&k[2]===\"a\"))n=1;n;",
    );
    assert_eq!(code, 0, "numeric names keep own-row insertion order");
}

#[test]
fn object_keys_utf16_keys() {
    let code = case_code(
        "var n=0;var o={};o[\"\\u00e9\"]=1;o[\"\\ud834\\udd1e\"]=2;var k=Object.keys(o);if(!(k[0]===\"\\u00e9\"&&k[1]===\"\\ud834\\udd1e\"&&k[1].length===2))n=1;n;",
    );
    assert_eq!(code, 0, "existing String key nodes are returned intact");
}

#[test]
fn object_keys_function_own_metadata() {
    let code = case_code(
        "var n=0;function f(a,b){return a+b;}var k=Object.keys(f);if(!(k[0]===undefined))n=1;f.x=1;var kx=Object.keys(f);if(n===0&&!(kx[0]===\"x\"))n=2;var ko=Object.keys(Object);if(n===0&&!(ko[0]===undefined))n=3;var kp=Object.keys(Object.prototype);if(n===0&&!(kp[0]===undefined))n=4;n;",
    );
    assert_eq!(code, 0, "non-enumerable function metadata never leaks");
}

#[test]
fn object_keys_method_attributes_and_alias() {
    let code = case_code(
        "var n=0;var d=Object.getOwnPropertyDescriptor(Object,\"keys\");if(!(d.writable===true&&d.enumerable===false&&d.configurable===true&&d.value.length===1))n=1;var f=d.value;var k=f({a:1});if(n===0&&!(k[0]===\"a\"))n=2;var g=Object.keys;if(n===0&&!(g({b:2})[0]===\"b\"))n=3;n;",
    );
    assert_eq!(code, 0, "method attributes, saved and aliased calls");
}

#[test]
fn object_keys_argument_evaluation_and_errors() {
    let code = case_code(
        "var n=0;var w={n:0,get v(){\"use strict\";this.n=this.n+1;return {a:1};}};var k=Object.keys(w.v);if(!(k[0]===\"a\"&&w.n===1))n=1;var e1=0;try{Object.keys();}catch(e){if(e.name===\"TypeError\")e1=1;}if(n===0&&e1!==1)n=2;var e2=0;try{Object.keys(null);}catch(e){if(e.name===\"TypeError\")e2=1;}if(n===0&&e2!==1)n=3;n;",
    );
    assert_eq!(
        code, 0,
        "single evaluation, zero argument and null TypeError"
    );
}

#[test]
fn object_keys_primitive_arguments_throw() {
    let code = case_code(
        "var n=0;var bad=0;try{Object.keys(1);}catch(e){if(e.name===\"TypeError\")bad=bad+1;}try{Object.keys(\"s\");}catch(e){if(e.name===\"TypeError\")bad=bad+1;}try{Object.keys(true);}catch(e){if(e.name===\"TypeError\")bad=bad+1;}try{Object.keys(undefined);}catch(e){if(e.name===\"TypeError\")bad=bad+1;}if(bad!==4)n=1;var extra=0;try{Object.keys({a:1},bad,{b:2});}catch(e){extra=1;}if(n===0&&extra!==0)n=2;n;",
    );
    assert_eq!(code, 0, "managed TypeError for every primitive argument");
}

#[test]
fn object_keys_throwing_argument_propagates() {
    let code = case_code(
        "var n=0;var seen=0;function boom(){throw 7;}try{Object.keys(boom());}catch(e){if(e===7)seen=1;}if(seen!==1)n=1;n;",
    );
    assert_eq!(code, 0, "argument exceptions propagate unchanged");
}

#[test]
fn object_keys_packed_and_tagged_arrays() {
    let code = case_code(
        "var n=0;var p=[10,20,30];var kp=Object.keys(p);if(!(kp.length===3&&kp[0]===\"0\"&&kp[1]===\"1\"&&kp[2]===\"2\"&&kp[3]===undefined))n=1;var t=[1,\"x\"];var kt=Object.keys(t);if(n===0&&!(kt.length===2&&kt[0]===\"0\"&&kt[1]===\"1\"))n=2;var z=[];var kz=Object.keys(z);if(n===0&&!(kz.length===0&&kz[0]===undefined))n=3;n;",
    );
    assert_eq!(
        code, 0,
        "packed and tagged dense arrays yield index Strings"
    );
}

#[test]
fn object_keys_array_index_strings_are_fresh() {
    let code = case_code(
        "var n=0;var a=[7];var k=Object.keys(a);if(!(k[0]===\"0\"))n=1;if(n===0&&!(a[0]===7))n=2;if(n===0&&!(k[0]!==7))n=3;n;",
    );
    assert_eq!(code, 0, "index keys are Strings, not source elements");
}

#[test]
fn object_keys_result_snapshot_independent() {
    let code = case_code(
        "var n=0;var o={a:1};var k=Object.keys(o);o.b=2;delete o.a;var c=0;for(var x of k){c=c+1;}if(!(k[0]===\"a\"&&c===1))n=1;n;",
    );
    assert_eq!(code, 0, "later source mutation cannot change the snapshot");
}

#[test]
fn object_keys_result_retains_keys_across_gc() {
    // 40 object allocations after the input is dropped cross the 32-allocation
    // interval, so the collector runs while the result keys are reachable only
    // through the sole output array root.
    let code = case_code(
        "var n=0;var o={a:1,b:2};var k=Object.keys(o);o=null;function g(){var i=0;while(i<40){var t={v:i};i=i+1;}return 0;}g();if(!(k[0]===\"a\"&&k[1]===\"b\"))n=1;n;",
    );
    assert_eq!(code, 0, "result String keys survive real collections");
}

#[test]
fn object_keys_wide_array_indices_across_gc() {
    // 40 fresh index Strings cross the interval while the output array must
    // trace every key already stored.
    let code = case_code(
        "var a=[0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39];var n=0;var k=Object.keys(a);if(!(k.length===40&&k[0]===\"0\"&&k[39]===\"39\"))n=1;var c=0;for(var x of k){c=c+1;}if(n===0&&c!==40)n=2;n;",
    );
    assert_eq!(
        code, 0,
        "wide dense array keys survive interval collections"
    );
}

#[test]
fn object_keys_result_consumed_dynamically() {
    let code = case_code(
        "var n=0;function pick(keys){return keys[1];}var k=Object.keys({a:1,b:2});if(!(pick(k)===\"b\"))n=1;var s=\"\";for(var x of k){s=s+x;}if(n===0&&!(s===\"ab\"))n=2;var q=Object.keys({z:9});if(n===0&&!(q[0]===\"z\"))n=3;n;",
    );
    assert_eq!(code, 0, "index, for-of and alias consumption");
}

#[test]
fn object_keys_repeated_calls_are_independent() {
    let code = case_code(
        "var n=0;var a=Object.keys({p:1});var b=Object.keys({q:2});if(!(a[0]===\"p\"&&b[0]===\"q\"))n=1;var i=0;var s=\"\";while(i<3){var t=Object.keys({k:i});s=s+t[0];i=i+1;}if(n===0&&!(s===\"kkk\"))n=2;n;",
    );
    assert_eq!(code, 0, "each call yields a fresh independent result");
}

#[test]
fn object_keys_dynamic_array_length_consumer() {
    // A dynamically produced dense Array (call result, method result, alias
    // returned through a parameter) already dispatches element reads on the
    // runtime node kind. `length` must take the same real Array metadata path
    // instead of the ordinary String/Object Get.
    execute_true("function f(){return [1,2];}f().length===2;");
    execute_true("Object.keys({a:1,b:2}).length===2;");
    execute_true("function id(x){return x;}var k=Object.keys({a:1});id(k).length===1;");
}

#[test]
fn object_keys_dynamic_length_preserves_other_length_semantics() {
    // String, Function, ordinary-object getter, nullish error and single
    // evaluation controls must all keep their established behavior through the
    // same emitted member-read sites.
    execute_true("var s=\"ab\";s.length===2;");
    execute_true("function g(a,b){return a+b;}g.length===2;");
    execute_true(
        "function h(){return {n:0,get length(){\"use strict\";this.n=this.n+1;return 4;}};}var o=h();var x=o.length;x===4&&o.n===1;",
    );
    execute_true(
        "function z(){return null;}var n=0;try{z().length;}catch(e){if(e.name===\"TypeError\")n=1;}n===1;",
    );
    execute_true(
        "function a(x){x.n=x.n+1;return [1,2,3];}var box={n:0};var x=a(box).length;x===3&&box.n===1;",
    );
    execute_true("function t(){return [1,\"x\"];}t().length===2;");
    execute_true("function f(){return [4,5];}var a=f();var b=a;b.length===2&&a.length===2;");
}

#[test]
fn object_keys_dynamic_string_function_length_branch() {
    // A call result that is a String or a Function must take the same
    // dynamic member dispatch as a call-result Array; this is the missing
    // combined source for those two runtime kinds.
    execute_true(
        "function id(x){return x;}function g(a,b){return a;}id(\"ab\").length===2&&id(g).length===2;",
    );
}

#[test]
fn object_keys_output_root_is_the_only_retainer() {
    let plugin: &'static object_keys_observer::KeysObserver =
        Box::leak(Box::new(object_keys_observer::KeysObserver));
    let (machine, status) = execute_on(
        "var o={a:1,b:2};var k=Object.keys(o);k=null;o=null;",
        plugin,
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-object-keys-observer")),
    );
    let status =
        status.expect_err("stale checked_base must refuse after the real drop and collect");
    eprintln!("KEYS_STALE error={status:?} stack={:?}", machine.stack());
    assert!(
        format!("{status:?}").contains("assertion failed"),
        "refusal must come from the checked gate, got {status:?}"
    );
}
