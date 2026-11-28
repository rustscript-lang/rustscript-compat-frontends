//! Math.min/max argument forwarding through Function.prototype.call/apply.
//! Each script contains one entry form only, so a second same-feature
//! occurrence cannot open the intrinsic gate for it.

use vm::{Value, VmStatus};

fn run_cookie(source: &str) -> bool {
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
        .unwrap_or_else(|error| {
            panic!("production compilation refused; source={source:?} error={error:?}")
        });
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..*machine.jit_config()
    });
    machine.set_fuel(10_000_000);
    let status = machine.run().unwrap_or_else(|error| {
        panic!("guest run refused; source={source:?} error={error:?}")
    });
    assert_eq!(status, VmStatus::Halted, "source={source:?}");
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}; source={source:?}");
    };
    assert_eq!(
        fields.get(&Value::string("tag")),
        Some(&Value::Int(2)),
        "cookie must be a guest Boolean: {source:?} value={value:?}"
    );
    match fields.get(&Value::string("flag")) {
        Some(Value::Bool(flag)) => *flag,
        other => panic!("expected Boolean flag, got {other:?}: {source:?}"),
    }
}

/// The third numeric actual must decide. Truncation to the declared arity 2
/// would answer 2, and the intrinsic argument gate answers with TypeError
/// before dispatch.
#[test]
fn math_max_call_third_argument_decides() {
    let source = "Math.max.call(null,1,2,9)===9;";
    assert!(
        run_cookie(source),
        "Math.max.call must forward the decisive third actual"
    );
}

/// The third numeric actual must decide. Truncation to the declared arity 2
/// would answer 1.
#[test]
fn math_min_call_third_argument_decides() {
    let source = "Math.min.call(null,1,2,-9)===-9;";
    assert!(
        run_cookie(source),
        "Math.min.call must forward the decisive third actual"
    );
}

/// The third array element must decide. Truncation to the declared arity 2
/// would answer 2.
#[test]
fn math_max_apply_third_argument_decides() {
    let source = "Math.max.apply(null,[1,2,9])===9;";
    assert!(
        run_cookie(source),
        "Math.max.apply must expand the decisive third element"
    );
}

/// The third array element must decide. Truncation to the declared arity 2
/// would answer 1.
#[test]
fn math_min_apply_third_argument_decides() {
    let source = "Math.min.apply(null,[1,2,-9])===-9;";
    assert!(
        run_cookie(source),
        "Math.min.apply must expand the decisive third element"
    );
}

#[test]
fn math_max_call_empty_is_negative_infinity() {
    assert!(run_cookie("Math.max.call(null)===-Infinity;"));
}

#[test]
fn math_min_call_empty_is_positive_infinity() {
    assert!(run_cookie("Math.min.call(null)===Infinity;"));
}

/// Third actual selects +0. The first two are -0, so truncation would keep -0.
#[test]
fn math_max_call_third_actual_selects_positive_zero() {
    assert!(run_cookie("1/Math.max.call(null,-0,-0,0)===Infinity;"));
}

/// Third actual selects -0. The first two are +0, so truncation would keep +0.
#[test]
fn math_min_call_third_actual_selects_negative_zero() {
    assert!(run_cookie("1/Math.min.call(null,0,0,-0)===-Infinity;"));
}

/// Third actual is NaN, so the result must be NaN rather than 2.
#[test]
fn math_max_call_third_actual_is_nan() {
    assert!(run_cookie("var x=Math.max.call(null,1,2,NaN);x!==x;"));
}

#[test]
fn math_max_apply_empty_is_negative_infinity() {
    assert!(run_cookie("Math.max.apply(null,[])===-Infinity;"));
}

#[test]
fn math_min_apply_empty_is_positive_infinity() {
    assert!(run_cookie("Math.min.apply(null,[])===Infinity;"));
}

#[test]
fn math_max_apply_third_element_selects_positive_zero() {
    assert!(run_cookie("1/Math.max.apply(null,[-0,-0,0])===Infinity;"));
}

#[test]
fn math_min_apply_third_element_selects_negative_zero() {
    assert!(run_cookie("1/Math.min.apply(null,[0,0,-0])===-Infinity;"));
}

#[test]
fn math_max_apply_third_element_is_nan() {
    assert!(run_cookie("var x=Math.max.apply(null,[1,2,NaN]);x!==x;"));
}

/// Non-array apply stays on the pinned TypeError path. One `.apply` only.
#[test]
fn math_max_apply_array_like_is_type_error() {
    let source = "try{Math.max.apply(null,{length:3,0:1,1:2,2:9});}catch(e){e.name===\"TypeError\";}";
    assert!(run_cookie(source), "array-like apply must stay a TypeError");
}

/// The shared 65535 ceiling still refuses before expansion.
#[test]
fn math_max_apply_over_limit_is_type_error() {
    let source = "try{Math.max.apply(null,new Array(65536));}catch(e){e.name===\"TypeError\";}";
    assert!(run_cookie(source), "65536 apply arguments must stay a TypeError");
}

/// Ordinary numeric intrinsics keep the old extra-argument refusal.
#[test]
fn math_abs_call_extra_argument_stays_refused() {
    let source = "try{Math.abs.call(null,-3,9);}catch(e){e.name===\"TypeError\";}";
    assert!(run_cookie(source), "Math.abs.call must keep the old argument gate");
}

/// Object methods are not extrema and keep the old extra-argument refusal.
#[test]
fn object_to_string_call_extra_argument_stays_refused() {
    let source = "try{Object.prototype.toString.call({},1);}catch(e){e.name===\"TypeError\";}";
    assert!(
        run_cookie(source),
        "Object.prototype.toString.call must keep the old argument gate"
    );
}

/// The existing Error.prototype.toString carve-out still admits extra arguments.
/// `Error` is not a value binding here, so the method is taken from an instance.
#[test]
fn error_to_string_call_extra_arguments_stay_admitted() {
    let source = "var e=new Error(\"boom\");e.toString.call(e,1,2)===\"Error: boom\";";
    assert!(run_cookie(source), "Error toString call carve-out must stay");
}

/// Function.prototype itself stays non-callable.
#[test]
fn function_prototype_call_stays_refused() {
    let source = "function f(){return 1;}var p=Object.getPrototypeOf(f);try{p.call(null,1);}catch(e){e.name===\"TypeError\";}";
    assert!(run_cookie(source), "Function.prototype.call must stay a TypeError");
}

/// A typed array is not a dense/tagged Array argument vector.
#[test]
fn math_min_apply_typed_array_is_type_error() {
    let source = "try{Math.min.apply(null,new Uint8Array(0));}catch(e){e.name===\"TypeError\";}";
    assert!(run_cookie(source), "typed-array apply must stay a TypeError");
}
