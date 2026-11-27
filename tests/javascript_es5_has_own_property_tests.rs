//! Public `Object.prototype.hasOwnProperty` on the certified, JIT-disabled
//! interpreter.
//!
//! Every case is real guest source through the production JS frontend: the
//! returned value is the guest boolean/Number the source computes, so a case
//! only reports success when the complete expected behavior holds. The method
//! is reached through the ordinary public member Reference and dispatcher, not
//! through a private helper call.
use pd_vm_compat_frontends::{JAVASCRIPT_ES5_PLUGIN, javascript_es5_flavor};
use vm::{HostFunctionRegistry, Value, VmStatus};
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

fn prepare(source: &str) -> vm::Vm {
    free_floor();
    eprintln!("HAS_OWN_SOURCE {source}");
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_host_api_catalog(catalog.clone())
        .with_source_plugin(&JAVASCRIPT_ES5_PLUGIN);
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        javascript_es5_flavor(),
        options,
    )
    .unwrap_or_else(|error| panic!("source must compile: {source}\n{error:?}"));
    eprintln!("HAS_OWN_COMPILED");
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
    registry.allow_builtin("math::trunc").expect("trunc");
    registry
        .allow_builtin("math::is_finite")
        .expect("is_finite");
    registry.allow_builtin("math::powf").expect("powf");
    registry.bind_vm_cached(&mut machine).expect("bind");
    machine.set_fuel(10_000_000);
    machine
}

fn execute_on(source: &str) -> (vm::Vm, Result<VmStatus, vm::VmError>) {
    let owned = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let mut machine = prepare(&owned);
            let before = machine.get_fuel();
            assert!(
                matches!(before, Some(fuel) if fuel <= 10_000_000),
                "interpreter fuel {before:?}"
            );
            let status = machine.run();
            let after = machine.get_fuel();
            eprintln!(
                "HAS_OWN_VM status={status:?} jit_enabled={} aot={} jit_native={} fuel_before={before:?} fuel_after={after:?}",
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
                    "HAS_OWN_SITE ip={ip} op={op:?} owner={owner:?} stack={:?}",
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
            (machine, status)
        })
        .expect("stack")
        .join()
        .expect("test thread")
}

fn execute(source: &str) -> Value {
    let (machine, status) = execute_on(source);
    let status = status.unwrap_or_else(|error| panic!("{source}: {error:?}"));
    assert_eq!(status, VmStatus::Halted, "{source}");
    machine.stack().last().expect("completion").clone()
}

fn execute_true(source: &str) {
    let result = execute(source);
    assert_eq!(
        field(&result, "tag"),
        Value::Int(2),
        "{source} -> {result:?}"
    );
    assert_eq!(
        field(&result, "flag"),
        Value::Bool(true),
        "{source} -> {result:?}"
    );
}

#[test]
fn has_own_property_object_own_literal_call() {
    execute_true("var o={x:1};o.hasOwnProperty('x')&&!o.hasOwnProperty('y');");
}

#[test]
fn has_own_property_array_inherited_call() {
    execute_true("var a=[1,2];a.hasOwnProperty('1')&&!a.hasOwnProperty('2');");
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

#[test]
fn has_own_property_object_own_inherited_and_nonenumerable() {
    let code = case_code(
        "var n=0;var p={x:1};var o=Object.create(p);o.y=2;if(!o.hasOwnProperty('y'))n=1;if(o.hasOwnProperty('x'))n=2;if(!p.hasOwnProperty('x'))n=3;var q={a:1};Object.defineProperty(q,'h',{value:2,enumerable:false,writable:true,configurable:true});if(n===0&&!q.hasOwnProperty('h'))n=4;n;",
    );
    assert_eq!(code, 0, "own rows only; non-enumerable own stays own");
}

#[test]
fn has_own_property_getter_never_executes() {
    let code = case_code(
        "var n=0;var o={h:0,get g(){'use strict';this.h=this.h+1;return 1;}};if(!o.hasOwnProperty('g'))n=1;if(n===0&&o.h!==0)n=2;var t={get boom(){'use strict';throw 9;}};if(n===0&&!t.hasOwnProperty('boom'))n=3;t=null;n;",
    );
    assert_eq!(code, 0, "querying an accessor row never invokes it");
}

#[test]
fn has_own_property_delete_and_readd() {
    let code = case_code(
        "var n=0;var o={a:1};if(!o.hasOwnProperty('a'))n=1;delete o.a;if(n===0&&o.hasOwnProperty('a'))n=2;o.a=2;if(n===0&&!o.hasOwnProperty('a'))n=3;n;",
    );
    assert_eq!(code, 0, "delete then re-add changes the own answer");
}

#[test]
fn has_own_property_null_prototype_object() {
    let code = case_code(
        "var n=0;var z=Object.create(null);z.a=1;var t=0;try{z.hasOwnProperty('a');}catch(e){if(e.name==='TypeError')t=1;}if(t!==1)n=1;n;",
    );
    assert_eq!(
        code, 0,
        "a null-prototype object has no inherited method; the call throws"
    );
}

#[test]
fn has_own_property_function_own_and_borrowed_receiver() {
    // Function values carry exactly one own metadata row, `length`; this
    // frontend's Function model has no own `prototype` row.
    let code = case_code(
        "var n=0;function f(a,b){return a+b;}if(!f.hasOwnProperty('length'))n=1;if(n===0&&f.hasOwnProperty('prototype'))n=2;if(n===0&&f.hasOwnProperty('toString'))n=3;var m=Object.prototype.hasOwnProperty;var box={f:m,a:1};if(n===0&&!box.f('a'))n=4;if(n===0&&box.f('b'))n=5;if(n===0&&!box.f('f'))n=6;n;",
    );
    assert_eq!(code, 0, "Function own metadata and a borrowed receiver");
}

#[test]
fn has_own_property_arity_alias_and_descriptor() {
    let code = case_code(
        "var n=0;if(Object.prototype.hasOwnProperty.length!==1)n=1;var d=Object.getOwnPropertyDescriptor(Object.prototype,'hasOwnProperty');if(n===0&&!(d.writable===true&&d.enumerable===false&&d.configurable===true&&d.value.length===1))n=2;var g=Object.prototype.hasOwnProperty;var t=0;try{g('a');}catch(e){if(e.name==='TypeError')t=1;}if(n===0&&t!==1)n=3;var box={f:g,a:1};if(n===0&&!box.f('a'))n=4;n;",
    );
    assert_eq!(
        code, 0,
        "arity 1, non-enumerable configurable method, plain call has nullish this"
    );
}

#[test]
fn has_own_property_override_delete_and_restore() {
    let code = case_code(
        "var n=0;var o={a:1};o.hasOwnProperty=function(){return 'own';};if(o.hasOwnProperty('a')!=='own')n=1;delete o.hasOwnProperty;if(n===0&&!o.hasOwnProperty('a'))n=2;var p={};Object.defineProperty(p,'hasOwnProperty',{value:function(){return 'p';},enumerable:false,writable:true,configurable:true});if(n===0&&p.hasOwnProperty('a')!=='p')n=3;n;",
    );
    assert_eq!(code, 0, "own override, delete and restored inheritance");
}

#[test]
fn has_own_property_packed_tagged_and_empty_arrays() {
    let code = case_code(
        "var n=0;var p=[10,20,30];if(!p.hasOwnProperty('0'))n=1;if(n===0&&!p.hasOwnProperty('2'))n=2;if(n===0&&p.hasOwnProperty('3'))n=3;if(n===0&&!p.hasOwnProperty('length'))n=4;if(n===0&&p.hasOwnProperty('01'))n=5;if(n===0&&p.hasOwnProperty('hasOwnProperty'))n=6;var t=[1,'x'];if(n===0&&!t.hasOwnProperty('1'))n=7;var e=[];if(n===0&&!e.hasOwnProperty('length'))n=8;if(n===0&&e.hasOwnProperty('0'))n=9;n;",
    );
    assert_eq!(
        code, 0,
        "dense index and length own, out-of-range and inherited not own"
    );
}

#[test]
fn has_own_property_shared_real_array_prototype() {
    let code = case_code(
        "var n=0;var a=[1,2];var k=Object.keys({a:1});var ap=Object.getPrototypeOf(a);if(n===0&&Object.getPrototypeOf(k)!==ap)n=1;if(n===0&&Object.getPrototypeOf(ap)!==Object.prototype)n=2;if(n===0&&!ap.hasOwnProperty('length'))n=3;if(n===0&&ap.hasOwnProperty('0'))n=4;if(n===0&&a.hasOwnProperty!==Object.prototype.hasOwnProperty)n=5;n;",
    );
    assert_eq!(
        code, 0,
        "source arrays and keys output share one real Array prototype"
    );
}

#[test]
fn has_own_property_set_prototype_of_switches() {
    let code = case_code(
        "var n=0;var a=[1,2];Object.setPrototypeOf(a,null);var t=0;try{a.hasOwnProperty('0');}catch(e){if(e.name==='TypeError')t=1;}if(t!==1)n=1;var b=[1,2];var q={};Object.setPrototypeOf(b,q);if(n===0&&!b.hasOwnProperty('1'))n=2;var c=[1,2];var r={get hasOwnProperty(){'use strict';return this.length;}};Object.setPrototypeOf(c,r);if(n===0&&c.hasOwnProperty!==2)n=3;n;",
    );
    assert_eq!(
        code, 0,
        "an explicit prototype change removes or replaces inheritance"
    );
}

#[test]
fn has_own_property_primitive_and_object_key_coercion() {
    let code = case_code(
        "var n=0;var o={'1':1,'true':2,'null':3,'undefined':4,'0':5,'NaN':6,'Infinity':7};if(!o.hasOwnProperty(1))n=1;if(n===0&&!o.hasOwnProperty(true))n=2;if(n===0&&!o.hasOwnProperty(null))n=3;if(n===0&&!o.hasOwnProperty(undefined))n=4;if(n===0&&!o.hasOwnProperty(-0))n=5;if(n===0&&!o.hasOwnProperty(NaN))n=6;if(n===0&&!o.hasOwnProperty(Infinity))n=7;if(n===0&&!o.hasOwnProperty('Infinity'))n=8;n;",
    );
    assert_eq!(
        code, 0,
        "Number, -0, NaN, Infinity, Boolean, Null and Undefined keys"
    );
}

#[test]
fn has_own_property_object_key_string_hint_order() {
    let code = case_code(
        "var n=0;var k={log:'',toString:function(){'use strict';this.log=this.log+'t';return 'a';},valueOf:function(){'use strict';this.log=this.log+'v';return 'b';}};var o={a:1,b:2};if(!o.hasOwnProperty(k))n=1;if(n===0&&k.log!=='t')n=2;var k2={log:'',toString:function(){'use strict';this.log=this.log+'t';return {};},valueOf:function(){'use strict';this.log=this.log+'v';return 'b';}};if(n===0&&!o.hasOwnProperty(k2))n=3;if(n===0&&k2.log!=='tv')n=4;var k3={toString:5,valueOf:function(){return 'a';}};if(n===0&&!o.hasOwnProperty(k3))n=5;var bad={toString:function(){return {};},valueOf:function(){return {};}};var e1=0;try{o.hasOwnProperty(bad);}catch(e){if(e.name==='TypeError')e1=1;}if(n===0&&e1!==1)n=6;var e2=0;var boom={toString:function(){throw 7;}};try{o.hasOwnProperty(boom);}catch(e){if(e===7)e2=1;}if(n===0&&e2!==1)n=7;n;",
    );
    assert_eq!(
        code, 0,
        "String-hint order, skipping non-callables, TypeError and throw payload"
    );
}

#[test]
fn has_own_property_default_methods_available_without_math() {
    // No Math/Number/URI use anywhere in this source: the default
    // Object.prototype conversion methods and the method itself are part of
    // the ordinary prototype, not a numeric realm side effect.
    let code = case_code(
        "var n=0;var o={a:1};if(!Object.prototype.hasOwnProperty('toString'))n=1;if(n===0&&!Object.prototype.hasOwnProperty('valueOf'))n=2;if(n===0&&!Object.prototype.hasOwnProperty('hasOwnProperty'))n=3;if(n===0&&o.hasOwnProperty({}))n=4;if(n===0&&!Object.prototype.toString())n=5;n;",
    );
    assert_eq!(
        code, 0,
        "default conversion methods exist without the Math realm"
    );
}

#[test]
fn has_own_property_argument_and_base_evaluate_once() {
    let code = case_code(
        "var n=0;var o={a:1};var box={calls:0};function key(b){b.calls=b.calls+1;return 'a';}if(!o.hasOwnProperty(key(box)))n=1;if(n===0&&box.calls!==1)n=2;function ident(b,x){b.calls=b.calls+1;return x;}if(n===0&&!ident(box,o).hasOwnProperty('a'))n=3;if(n===0&&box.calls!==2)n=4;function side(b){b.calls=b.calls+1;return 0;}if(n===0&&!o.hasOwnProperty('a',side(box)))n=5;if(n===0&&box.calls!==3)n=6;n;",
    );
    assert_eq!(code, 0, "base, key and extra actual effects run once");
}

#[test]
fn has_own_property_nullish_receiver_checked_after_key_conversion() {
    let code = case_code(
        "var n=0;var g=Object.prototype.hasOwnProperty;var k={hits:0,toString:function(){'use strict';this.hits=this.hits+1;return 'a';}};var t=0;try{g(k);}catch(e){if(e.name==='TypeError')t=1;}if(t!==1)n=1;if(n===0&&k.hits!==1)n=2;var h={hits:0,toString:function(){'use strict';this.hits=this.hits+1;return 'b';}};var u=0;try{g(h);}catch(e){if(e.name==='TypeError')u=1;}if(n===0&&u!==1)n=3;if(n===0&&h.hits!==1)n=4;n;",
    );
    assert_eq!(
        code, 0,
        "the key conversion runs before the nullish receiver TypeError"
    );
}

#[test]
fn has_own_property_coercion_survives_real_collection() {
    // The key object allocates 40 ordinary objects inside its toString, which
    // crosses the 32-allocation interval while the adapter frame must keep the
    // receiver, raw key and produced String reachable. A queried accessor row
    // must still be reported without invoking its getter.
    let code = case_code(
        "var n=0;var o={a:1};var k={toString:function(){var i=0;while(i<40){var t={v:i};i=i+1;}return 'a';}};if(!o.hasOwnProperty(k))n=1;if(n===0){var p={get a(){throw 3;}};if(!p.hasOwnProperty(k))n=2;}n;",
    );
    assert_eq!(code, 0, "managed key coercion survives real collections");
}

#[test]
fn has_own_property_dynamic_string_receiver_is_public() {
    // Preserve the exact former gap source as the positive qualification.
    // A real inherited method must query the dynamic String's virtual own index.
    execute_true("function id(x){return x;}var s=id('ab');s.hasOwnProperty('0');");
}

#[test]
fn array_computed_switch_preserves_numeric_read_route() {
    execute_true("var a=[7];var key='0';var result=0;switch(1){case 1:key=0;case 2:result=a[key];}result===7;");
}

#[test]
fn array_computed_switch_preserves_numeric_read_route_reverse_walk() {
    execute_true("var a=[7];var key=0;var result=0;switch(2){case 1:key='0';case 2:result=a[key];}result===7;");
}

#[test]
fn array_computed_switch_preserves_object_string_read_route() {
    execute_true("var a=[7];var key='0';var result=0;switch(1){case 1:a={'0':7};case 2:result=a[key];}result===7;");
}

#[test]
fn array_computed_switch_preserves_object_string_read_route_reverse_walk() {
    execute_true("var a={'0':7};var key='0';var result=0;switch(2){case 1:a=[7];case 2:result=a[key];}result===7;");
}

#[test]
fn array_computed_finally_preserves_numeric_read_route() {
    execute_true("var box={seen:0};function f(box,flag){var a=[7];var key='0';try{if(flag){key=0;throw 9;}}finally{box.seen=a[key];}}var caught=0;try{f(box,true);}catch(e){caught=e;}box.seen===7&&caught===9;");
}

#[test]
fn array_computed_switch_does_not_admit_boolean_receiver() {
    free_floor();
    let source = "var a=[1];var key='hasOwnProperty';switch(1){case 1:a=true;case 2:a[key]('0');}";
    eprintln!("ARRAY_STRING_RECEIVER_BOUNDARY_SOURCE {source}");
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_host_api_catalog(catalog)
        .with_source_plugin(&JAVASCRIPT_ES5_PLUGIN);
    match vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        javascript_es5_flavor(),
        options,
    ) {
        Ok(_) => panic!("mixed Boolean receiver must not acquire a proved-Array String site"),
        Err(error) => {
            let text = format!("{error:?}");
            assert!(
                text.contains("P0 plain-object member reads require an object receiver")
                    || text.contains("P0 plain-object member reads admit only identifier and string-literal keys")
                    || text.contains("primitive key site receiver mode changed"),
                "expected receiver/admission refusal, got {text}"
            );
            eprintln!("ARRAY_STRING_RECEIVER_BOUNDARY_REFUSAL {text}");
        }
    }
}

#[test]
fn has_own_property_array_computed_string_name_call() {
    for source in [
        "var a=[1,2];a['hasOwnProperty']('0');",
        "var a=[1,2];var key='hasOwnProperty';a[key]('0');",
    ] {
        execute_true(source);
    }
}

#[test]
fn has_own_property_array_computed_string_reference_semantics() {
    // Six source/VM pipelines; use its exact selector, not the whole target.
    for source in [
        "var a=[1,2];var key='hasOwnProperty';a[key]===Object.prototype.hasOwnProperty;",
        "var a=[1,2];var key='hasOwnProperty';var box={n:0};a[(box.n=box.n+1,key)]('0')&&box.n===1;",
        "var a=[1,2];var key='hasOwnProperty';var result=a[(a=[],key)]('1');result===true&&a.length===0;",
        "var a=[1,2];var p={f:function(){'use strict';return this.length;}};Object.setPrototypeOf(a,p);var key='f';a[key]()===2;",
        "var a=[1,2];var p={get f(){'use strict';throw 7;}};Object.setPrototypeOf(a,p);var key='f';var box={n:0};function side(b){b.n=b.n+1;return '0';}var caught=0;var finalized=0;try{a[key](side(box));}catch(e){caught=e;}finally{finalized=1;}caught===7&&box.n===0&&finalized===1;",
        "var a=[1,2];var key='missing';var box={n:0};function side(b){b.n=b.n+1;return '0';}var caught=0;try{a[key](side(box));}catch(e){if(e.name==='TypeError')caught=1;}caught===1&&box.n===1;",
    ] {
        execute_true(source);
    }
}

#[test]
fn has_own_property_array_computed_string_virtual_own_and_prototypes() {
    // Six sources; this identity is selected separately from other batches.
    for source in [
        "var a=[3,4];var key='0';var len='length';a[key]===3&&a[len]===2;",
        "var a=[1,'x',true];var first='0';var second='1';var third='2';var len='length';a[first]===1&&a[second]==='x'&&a[third]===true&&a[len]===3;",
        "var a=[1,2];Object.setPrototypeOf(a,null);var key='hasOwnProperty';var missing=a[key];var caught=0;try{a[key]('0');}catch(e){if(e.name==='TypeError')caught=1;}missing===undefined&&caught===1&&a.length===2;",
        "var a=[];var key='hasOwnProperty';a[key]('0')===false&&a[key]('length')===true;",
        "var a=[1,2];Object.setPrototypeOf(a,{'7':9});var key='7';a[key]===9;",
        "var a=[1,2];var key='0';var i=0;var ok=true;while(i<2){ok=ok&&a[key]===(i+1);key='1';i=i+1;}ok&&i===2;",
    ] {
        execute_true(source);
    }
}

#[test]
fn has_own_property_private_primitive_prototypes_public_cases() {
    // Seven distinct source/VM pipelines; run this identity separately from
    // multi-source neighbours so each command retains the pipeline ceiling.
    for source in [
        "'ab'.hasOwnProperty('0')&&'ab'.hasOwnProperty('length')&&!'ab'.hasOwnProperty('2')&&!'ab'.hasOwnProperty('hasOwnProperty');",
        "function id(x){return x;}var s=id('ab');s.length===2&&s.hasOwnProperty('length')&&!s.hasOwnProperty('2')&&!s.hasOwnProperty('hasOwnProperty');",
        "function id(x){return x;}var n=id(1);!n.hasOwnProperty('0');",
        "function id(x){return x;}var b=id(true);!b.hasOwnProperty('0');",
        "(1).hasOwnProperty('0')===false&&(true).hasOwnProperty('0')===false;",
        "function id(x){return x;}var s=id('ab');s.hasOwnProperty===Object.prototype.hasOwnProperty;",
        "function id(x){return x;}var b=id(true);b.missing===undefined;",
    ] {
        execute_true(source);
    }
}

#[test]
fn has_own_property_dynamic_string_tostring_caught_is_false() {
    // Original source bytes. After String.prototype.toString, caught===1 is
    // false. execute returns the boxed completion; tag 2 and flag false are
    // the existing field asserts. One source, no new helper.
    let source = "function id(x){return x;}var s=id('ab');var caught=0;try{s.toString();}catch(e){if(e.name==='TypeError')caught=1;}caught===1;";
    let result = execute(source);
    assert_eq!(
        field(&result, "tag"),
        Value::Int(2),
        "{source} -> {result:?}"
    );
    assert_eq!(
        field(&result, "flag"),
        Value::Bool(false),
        "{source} -> {result:?}"
    );
}

#[test]
fn has_own_property_dynamic_string_tostring_returns_ab() {
    execute_true("function id(x){return x;}id('ab').toString()==='ab';");
}

#[test]
fn has_own_property_dynamic_number_tostring_returns_digits() {
    // `Number.prototype.toString` is now installed on the private Number
    // prototype for any source that names `toString`, so a dynamically typed
    // Number receiver converts instead of throwing. The previous `caught===1`
    // expectation pinned the missing method, not a property rule; it is kept
    // here inverted so the old shape stays covered.
    execute_true(
        "function id(x){return x;}var caught=0;try{id(1).toString();}catch(e){if(e.name==='TypeError')caught=1;}caught===0;",
    );
    execute_true("function id(x){return x;}id(1).toString()==='1';");
}

#[test]
fn has_own_property_get_prototype_of_dynamic_string_still_throws() {
    execute_true(
        "function id(x){return x;}var caught=0;try{Object.getPrototypeOf(id('ab'));}catch(e){if(e.name==='TypeError')caught=1;}caught===1;",
    );
}

#[test]
fn has_own_property_dynamic_string_index_still_throws() {
    execute_true(
        "function id(x){return x;}var s=id('ab');var caught=0;try{s['0'];}catch(e){if(e.name==='TypeError')caught=1;}caught===1;",
    );
}

#[test]
fn has_own_property_null_receiver_side_effect_still_throws() {
    execute_true(
        "function id(x){return x;}var box={n:0};function side(b){b.n=b.n+1;return '0';}var caught=0;try{id(null).hasOwnProperty(side(box));}catch(e){if(e.name==='TypeError')caught=1;}caught===1&&box.n===0;",
    );
}

#[test]
fn has_own_property_key_getter_method_this_and_order() {
    // The `toString` lookup itself is a getter: it logs `g` on the key and
    // returns the method that logs `m` and answers `a`. Both calls must see
    // the original key object as `this` (otherwise `k.log` never reaches
    // "gm"), and the `valueOf` lookup getter must not run at all.
    execute_true(
        "var k={log:'',get toString(){'use strict';this.log=this.log+'g';return function(){'use strict';this.log=this.log+'m';return 'a';};},get valueOf(){'use strict';this.log=this.log+'v';return 'b';}};var o={a:1};o.hasOwnProperty(k)&&k.log==='gm';",
    );
}

#[test]
fn has_own_property_key_getter_skips_noncallable_then_value_of() {
    // A non-callable `toString` result is skipped, then `valueOf` is looked up
    // through its own getter, and only its returned method logs `m`. The
    // distinct `g`, `v`, `m` order cannot be produced by swapped candidates.
    execute_true(
        "var k={log:'',get toString(){'use strict';this.log=this.log+'g';return 5;},get valueOf(){'use strict';this.log=this.log+'v';return function(){'use strict';this.log=this.log+'m';return 'a';};}};var o={a:1};o.hasOwnProperty(k)&&k.log==='gvm';",
    );
}

#[test]
fn has_own_property_key_getter_throw_preserves_result_and_finally() {
    // A throwing `toString` lookup getter keeps the caller's previous value:
    // the assignment never completes, the original payload reaches `catch`, a
    // single `finally` runs, and `valueOf` is never reached. State lives on the
    // key itself, so the strict getter needs no captured outer binding.
    execute_true(
        "var k={hits:0,visits:0,get toString(){'use strict';this.hits=this.hits+1;throw 7;},get valueOf(){'use strict';this.visits=this.visits+1;return 'a';}};var o={a:1};var result=99;var caught=0;var fin=0;try{result=o.hasOwnProperty(k);}catch(e){caught=e;}finally{fin=fin+1;}result===99&&k.hits===1&&k.visits===0&&caught===7&&fin===1;",
    );
}

#[test]
fn has_own_property_key_getter_value_of_lookup_throw_after_skip() {
    // `toString` returns a non-callable that is skipped, so the second lookup
    // is a `valueOf` getter which throws. The `t` then `v` log proves that
    // order, the payload stays 9, the pending assignment keeps `result` at 99,
    // and `finally` still runs exactly once.
    execute_true(
        "var k={hits:0,visits:0,log:'',get toString(){'use strict';this.hits=this.hits+1;this.log=this.log+'t';return 5;},get valueOf(){'use strict';this.visits=this.visits+1;this.log=this.log+'v';throw 9;}};var o={a:1};var result=99;var caught=0;var fin=0;try{result=o.hasOwnProperty(k);}catch(e){caught=e;}finally{fin=fin+1;}result===99&&k.hits===1&&k.visits===1&&k.log==='tv'&&caught===9&&fin===1;",
    );
}

#[test]
fn has_own_property_key_getter_borrowed_nullish_conversion_first() {
    // A borrowed plain call has a nullish `this`, but the key conversion is
    // ordered before the receiver check: the `toString` lookup getter runs
    // once and its payload 7 escapes, instead of the nullish TypeError that a
    // member Get refusal would raise first.
    execute_true(
        "var g=Object.prototype.hasOwnProperty;var k={hits:0,get toString(){'use strict';this.hits=this.hits+1;throw 7;}};var caught=0;var fin=0;try{g(k);}catch(e){caught=e;}finally{fin=fin+1;}k.hits===1&&caught===7&&fin===1;",
    );
}

#[test]
fn has_own_property_key_getter_actuals_before_adapter_conversion() {
    // All actuals are evaluated before the adapter converts the key: the
    // ignored second argument logs `a`, the `toString` getter logs `g` and
    // allocates inside its loop, and the returned method logs `m` with the key
    // as `this`. The order `agm` is observable regardless of any collection.
    execute_true(
        "var k={log:'',get toString(){'use strict';this.log=this.log+'g';var i=0;while(i<20){var t={x:i};i=i+1;}return function(){'use strict';this.log=this.log+'m';return 'a';};}};var o={a:1};function side(x){'use strict';x.log=x.log+'a';return 0;}o.hasOwnProperty(k,side(k))&&k.log==='agm';",
    );
}

#[test]
fn array_computed_loop_probe_dead_operand_does_not_mask_live_read() {
    execute_true(
        "var t=null;var a=[7];var key='0';var p='x';var r=0;var i=0;while(i<2){if(i===0){t={x:1};}else{r=t[p]?a[key]:0;}i++;}r===7;",
    );
}
