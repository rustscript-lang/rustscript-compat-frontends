//! `Function.prototype.name` and `Function.prototype.toString`.
//!
//! The repository has no `Function` global binding, so the Function prototype
//! object is reached through `Object.getPrototypeOf(function(){})`. Both
//! features are gated on the source spelling the static member name; a source
//! that never spells `.name`/`.toString` keeps the pre-feature surface.

const PROBE_FUEL: u64 = 40_000_000;

fn execute(source: &str) -> vm::Value {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let mut machine =
                pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source)
                    .unwrap_or_else(|error| {
                        panic!("source={source:?} guest_invocations=0 {error:?}")
                    });
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(PROBE_FUEL);
            let status = machine.run();
            eprintln!(
                "FNNAME_VM source={source:?} status={status:?} stack={:?}",
                machine.stack()
            );
            match status {
                Ok(vm::VmStatus::Halted) => machine.stack().last().unwrap().clone(),
                other => vm::Value::string(format!("STATUS {other:?}")),
            }
        })
        .unwrap()
        .join()
        .unwrap()
}

/// `(tag, number, flag)` of the boxed JS value left on the stack.
fn boxed(source: &str) -> (i64, f64, bool) {
    let result = execute(source);
    let vm::Value::Map(map) = result else {
        panic!("full box: {result:?}")
    };
    let tag = match map.get(&vm::Value::string("tag")) {
        Some(vm::Value::Int(tag)) => *tag,
        other => panic!("tag: {other:?}"),
    };
    let number = match map.get(&vm::Value::string("number")) {
        Some(vm::Value::Float(n)) => *n,
        other => panic!("number: {other:?}"),
    };
    let flag = match map.get(&vm::Value::string("flag")) {
        Some(vm::Value::Bool(b)) => *b,
        other => panic!("flag: {other:?}"),
    };
    (tag, number, flag)
}

fn assert_true(source: &str) {
    let (tag, _, flag) = boxed(source);
    assert_eq!(tag, 2, "expected Boolean box: {source}");
    assert!(flag, "expression must be true: {source}");
}

#[test]
fn name_declaration_and_anonymous() {
    // Upstream: `function f(a,b){}` reports "f"; an anonymous expression
    // reports the empty String. Named function expressions are refused by
    // this frontend at admission, not silently renamed.
    assert_true(r#"function f(a,b){};f.name==="f"&&f.name.length===1;"#);
    assert_true(r#"var f=function(){};f.name===""&&f.name.length===0;"#);
    assert_true("var f=function(a,b){return a;};f.name.length===0;");
    // Declarations inside an activation keep their binding name, and the
    // computed string-literal spelling opens the same gate as `.name`.
    assert_true(
        r#"function outer(){"use strict";function inner(){}return inner;}outer().name==="inner";"#,
    );
    assert_true(r#"var f=function(){};f["name"]==="";"#);
}

#[test]
fn name_is_a_getter_on_function_prototype() {
    // The realm has no `Function` binding: the prototype object is reached
    // through `Object.getPrototypeOf`. Upstream `Function.prototype.name`
    // throws `not a function` because the getter's receiver is not callable.
    assert_true(
        "var caught=false;try{Object.getPrototypeOf(function(){}).name;}catch(e){caught=e.name==='TypeError';}caught;",
    );
    // The getter exists as one own accessor on that object.
    assert_true(r#"function f(){};f.name;Object.getPrototypeOf(f).hasOwnProperty("name");"#);
}

#[test]
fn name_descriptor_own_and_attributes() {
    // Divergence from upstream: upstream stores `name` on the prototype as a
    // getter (functions have no own row); this realm stores one own data row
    // per gated function node. Attributes match the upstream accessor:
    // non-writable, non-enumerable, non-configurable.
    assert_true(r#"function f(){};f.name;f.hasOwnProperty("name");"#);
    assert_true(
        r#"function f(){};f.name;var d=Object.getOwnPropertyDescriptor(f,"name");d.writable===false&&d.enumerable===false&&d.configurable===false;"#,
    );
}

#[test]
fn tostring_named_anonymous_and_extra_arguments() {
    assert_true(r#"function f(a,b){};f.toString()==="function f() {\n    [native code]\n}";"#);
    assert_true(
        r#"function named(a){};named.toString()==="function named() {\n    [native code]\n}";"#,
    );
    assert_true(r#"var f=function(){};f.toString()==="function () {\n    [native code]\n}";"#);
    // `toString` has arity 0: extra arguments are ignored.
    assert_true(r#"function g(){};g.toString(1,2)==="function g() {\n    [native code]\n}";"#);
}

#[test]
fn tostring_borrowed_receivers() {
    // `.call({})` and `.call(null)` reach the adapter with a non-function
    // receiver and throw the realm TypeError; `.call(f,1,2)` forwards one
    // thisArg and drops the extra method arguments.
    assert_true(
        "var t=Object.getPrototypeOf(function(){}).toString;var caught=false;try{t.call({});}catch(e){caught=e.name==='TypeError';}caught;",
    );
    assert_true(
        "var t=Object.getPrototypeOf(function(){}).toString;var caught=false;try{t.call(null);}catch(e){caught=e.name==='TypeError';}caught;",
    );
    assert_true(
        r#"function f(){};var t=Object.getPrototypeOf(f).toString;t.call(f,1,2)==="function f() {\n    [native code]\n}";"#,
    );
    // The prototype object itself is not callable upstream.
    assert_true(
        "var caught=false;try{Object.getPrototypeOf(function(){}).toString();}catch(e){caught=e.name==='TypeError';}caught;",
    );
}

#[test]
fn bind_product_name_length_and_tostring() {
    // The bound product keeps the target in its prototype pair; with names
    // gated it carries its own `length` (0) and `name` ("bound") rows.
    assert_true(r#"function f(a,b){};f.bind(null).name==="bound";"#);
    assert_true("function f(a,b){};f.name;f.bind(null).length===0;");
    assert_true(
        r#"function f(){};f.bind(null).toString()==="function bound() {\n    [native code]\n}";"#,
    );
}

#[test]
fn error_message_rewrite_and_delete_keep_blank_names() {
    // `Error.prototype.message` is a writable, configurable public data row.
    // Anonymous names read a private permanent empty String root instead, so
    // neither a rewrite nor a delete can rename anonymous functions, and a
    // named function's `toString()` still scans its own row (the old helper
    // asserted on that public row before every scan).
    assert_true(
        r#"Object.getPrototypeOf(TypeError.prototype).message="shadow";var f=function(){};function g(){} f.name===""&&g.toString()==="function g() {\n    [native code]\n}";"#,
    );
    assert_true(
        r#"delete Object.getPrototypeOf(TypeError.prototype).message;var f=function(){};function g(){} f.name===""&&g.toString()==="function g() {\n    [native code]\n}";"#,
    );
}

#[test]
fn bound_call_shapes_without_name_gate() {
    // Name gate off: the bound product keeps its single `length` row. Row
    // shape is not identity: later arguments append after the bound vector
    // (`bind(null)` fixes a null this and binds none), and `.call` forwards
    // them the same way while its own thisArg stays ignored.
    assert_true(
        r#"function f(a){"use strict";return [this===null,a].join("|");}var b=f.bind(null);var a=b()==="true|";var c=b(1,2)==="true|1";var fwd=b.call(null,1)==="true|1";var len=b.length===0;a&&c&&fwd&&len;"#,
    );
}

#[test]
fn bound_call_shapes_with_name_gate() {
    // Name gate on: the bound product carries its own `length` and `name`
    // rows. Row shape is not identity: zero-argument calls still reach the
    // bound target, later arguments append, and the gated `name` row cannot
    // drop a bound argument.
    assert_true(
        r#"function f(a){"use strict";return [this===null,a].join("|");}var ng=f.name;var b=f.bind(null);var a=b()==="true|";var c=b(1,2)==="true|1";var d=b.call(null,1)==="true|1";var len=b.length===0;var named=b.name==="bound";a&&c&&d&&len&&named;"#,
    );
}

#[test]
fn bind_method_identity_is_the_private_code_cell() {
    // Forgery: add one own row to the original bind method and rewrite its
    // prototype pair to a source function. Both are guest-mutable, but the
    // private code cell (354 for the method, 270 for bound products) is not,
    // so the forged node stays on the ordinary method path: it can neither
    // reach the bound invocation arm nor stop `f.bind(null)` from creating.
    assert_true(
        r#"function f(){return 7;}var b=f.bind;Object.defineProperty(b,"x",{value:1,enumerable:false,writable:true,configurable:true});Object.setPrototypeOf(b,function(){return 9;});var created=f.bind(null)()===7;var n=0;try{b.call(null);}catch(e){if(e.name==="TypeError")n=1;}created&&n===1;"#,
    );
}

#[test]
fn arrow_functions_remain_post_es5_refusals() {
    let source = "var f=(x)=>x+1;";
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let compilation = vm::compile_source_with_flavor_and_options(
        source,
        pd_vm_compat_frontends::javascript_es5_flavor(),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN)
            .with_host_api_catalog(catalog),
    );
    let error = match compilation {
        Ok(_) => panic!("arrow functions remain post-ES5 refusals"),
        Err(error) => error,
    };
    assert!(format!("{error:?}").contains("arrow functions are post-ES5"));
}
