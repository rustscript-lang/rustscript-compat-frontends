//! Numeric dense-Array `for-of` on the certified, JIT-disabled interpreter.
#[path = "javascript_es5/arity_observer.rs"]
mod arity_observer;
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
        "free floor below 8589934592 bytes: {kb}kB"
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
    eprintln!("FOR_OF_SOURCE {source}");
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
    eprintln!("FOR_OF_COMPILED");
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
    for builtin in [
        "math::floor",
        "math::sqrt",
        "math::round",
        "math::powf",
        "math::is_nan",
        "math::is_finite",
        "math::trunc",
        "bytes::from_utf8",
    ] {
        registry.allow_builtin(builtin).expect(builtin);
    }
    registry.bind_vm_cached(&mut machine).expect("bind");
    machine.set_fuel(10_000_000);
    machine
}

fn execute(source: &str) -> Value {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let mut machine = prepare(&source);
            let before = machine.get_fuel();
            assert!(
                matches!(before, Some(fuel) if fuel <= 10_000_000),
                "interpreter fuel {before:?}"
            );
            let status = machine.run();
            let after = machine.get_fuel();
            eprintln!(
                "FOR_OF_VM status={status:?} jit_enabled={} aot={} jit_native={} fuel_before={before:?} fuel_after={after:?} stack={:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack()
            );
            assert_eq!(
                status.unwrap_or_else(|error| panic!("{source}: {error:?}")),
                VmStatus::Halted,
                "{source}"
            );
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
            let value = machine.stack().last().expect("completion").clone();
            machine.reset_for_reuse().expect("reset");
            assert_eq!(machine.host_context().resource_count(), 0);
            assert!(!machine.jit_config().enabled);
            assert_eq!(machine.aot_exec_count(), 0);
            assert_eq!(machine.jit_native_exec_count(), 0);
            value
        })
        .expect("stack")
        .join()
        .expect("test thread")
}

fn case_code(source: &str) -> i64 {
    let result = execute(source);
    let tag = field(&result, "tag");
    match field(&result, "number") {
        Value::Float(number) => {
            eprintln!("FOR_OF_TAG {tag:?} number={number}");
            number as i64
        }
        Value::Int(number) => {
            eprintln!("FOR_OF_TAG {tag:?} number={number}");
            number
        }
        other => panic!("case: {other:?} tag={tag:?}\n{source}"),
    }
}

#[test]
fn numeric_array_for_of_sum() {
    let code = case_code("var s=0;var a=[1,2,3];for(var x of a){s=s+x;}s;");
    assert_eq!(code, 6, "numeric dense array for-of sum");
}

fn completion_tag(source: &str) -> i64 {
    let result = execute(source);
    let tag = field(&result, "tag");
    let tag = match tag {
        Value::Int(tag) => tag,
        Value::Float(tag) => tag as i64,
        other => panic!("tag: {other:?}\n{source}"),
    };
    eprintln!("FOR_OF_COMPLETION_TAG {tag}");
    tag
}

fn refuse(source: &str) -> String {
    match pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source) {
        Err(error) => format!("{error:?}"),
        Ok(_) => panic!("dead unsupported source must refuse: {source}"),
    }
}

#[test]
fn numeric_array_for_of_shapes() {
    assert_eq!(
        case_code("var x=7;for(var x of []){}x;"),
        7,
        "empty keeps prior"
    );
    assert_eq!(
        case_code("var s=0;for(var x of [4]){s=s+x;}s;"),
        4,
        "single"
    );
    assert_eq!(
        case_code("var s=0;for(var x of [1,2,3]){s=s+x;}s;"),
        6,
        "multi"
    );
    assert_eq!(
        case_code("var x=0;for(x of [1,2]){}x;"),
        2,
        "existing ident"
    );
    assert_eq!(
        case_code("var o={x:0};for(o.x of [4,5]){}o.x;"),
        5,
        "member"
    );
    assert_eq!(
        case_code("var y=[1,2,3];var s=0;for(var x of y){s=s+x;}s;"),
        6,
        "declared y"
    );
}

#[test]
fn numeric_array_for_of_modes() {
    assert_eq!(
        case_code("\"use strict\";var s=0;for(var x of [1,2]){s=s+x;}s;"),
        3,
        "strict"
    );
    assert_eq!(
        case_code("function f(a){var s=0;for(var x of a){s=s+x;}return s;}f([1,2,3]);"),
        6,
        "function param"
    );
    assert_eq!(
        case_code(
            "function f(n){if(n===9)throw 1;return [1,2,3];}var n=1;var s=0;try{for(var x of f(n)){n=9;s=s+x;}}catch(e){s=99;}s;"
        ),
        6,
        "rhs once"
    );
    assert_eq!(
        case_code("var a=[1,2,3];var s=0;for(var x of a){a=[9];s=s+x;}s;"),
        6,
        "rebind holds original"
    );
    assert_eq!(
        case_code("var a=[1,2,3];var s=0;for(var x of a){if(x===1)a[1]=10;s=s+x;}s;"),
        14,
        "later index seen"
    );
    assert_eq!(
        case_code("function f(a){var x=0;for(x of a){}return x+1;}f([]);"),
        1,
        "post-loop zero"
    );
    assert_eq!(
        case_code("function f(a){var x=0;for(x of a){}return x+1;}f([4]);"),
        5,
        "post-loop positive"
    );
}

#[test]
fn numeric_array_for_of_errors() {
    assert_eq!(
        case_code(
            "var n=0;try{for(var x of null){n=1;}}catch(e){if(e.name===\"TypeError\")n=2;}n;"
        ),
        2,
        "null"
    );
    assert_eq!(
        case_code(
            "var n=0;try{for(var x of undefined){n=1;}}catch(e){if(e.name===\"TypeError\")n=2;}n;"
        ),
        2,
        "undefined"
    );
    assert_eq!(
        case_code("var n=0;try{for(var x of 1){n=1;}}catch(e){if(e.name===\"TypeError\")n=2;}n;"),
        2,
        "number"
    );
    assert_eq!(
        case_code("var n=0;try{for(var x of {}){n=1;}}catch(e){if(e.name===\"TypeError\")n=2;}n;"),
        2,
        "object"
    );
    assert_eq!(
        case_code(
            "var n=0;try{for(var x of function(){}){n=1;}}catch(e){if(e.name===\"TypeError\")n=2;}n;"
        ),
        2,
        "function"
    );
    assert_eq!(
        case_code(
            "var x=7;var n=0;function f(){throw 1;}try{for(x of f()){n=9;}}catch(e){if(e===1)n=1;}if(x===7)n=n+10;n;"
        ),
        11,
        "rhs throw keeps payload 1 and prior x"
    );
    assert_eq!(
        case_code("var o={set x(v){throw v;}};var n=0;try{for(o.x of [4]){n=1;}}catch(e){n=e;}n;"),
        4,
        "setter throw"
    );
    assert_eq!(
        case_code("function b(){throw 1;}var n=0;try{for(b().x of [4]){n=9;}}catch(e){n=e;}n;"),
        1,
        "base throw"
    );
}

#[test]
fn numeric_array_for_of_dead_body() {
    let dead = "function f(){try{for(var x of 1){null[{}]=(x=7);}}catch(e){}return 1;}f();";
    let live = "function f(){var a=[1];for(var x of a){null[{}]=(x=7);}return 1;}f();";
    let dead_text = refuse(dead);
    let live_text = refuse(live);
    eprintln!("FOR_OF_DEAD {dead_text}");
    eprintln!("FOR_OF_LIVE {live_text}");
    assert!(
        dead_text.contains("key")
            || dead_text.contains("object")
            || dead_text.contains("subscript")
            || dead_text.contains("array"),
        "{dead_text}"
    );
    assert!(
        live_text.contains("key")
            || live_text.contains("object")
            || live_text.contains("subscript")
            || live_text.contains("array"),
        "{live_text}"
    );
    // Legal dead conversion is admitted: the non-array RHS throws at runtime,
    // and the skipped numeric body is not a header/syntax refusal.
    assert_eq!(
        case_code("function f(){var n=0;try{for(var x of 1){n=x+1;}}catch(e){n=2;}return n;}f();"),
        2,
        "admitted dead conversion"
    );
    // Empty array still capability-checks the body. This is not a header refusal.
    let empty_unsupported = "function f(){for(var x of []){null[{}]=(x=7);}return 1;}f();";
    let empty_text = refuse(empty_unsupported);
    eprintln!("FOR_OF_EMPTY_UNSUPPORTED {empty_text}");
    assert!(
        empty_text.contains("key")
            || empty_text.contains("object")
            || empty_text.contains("subscript")
            || empty_text.contains("array"),
        "{empty_text}"
    );
}

/// Computed primitive-key member heads use the same prepared Put as `o[k()]=3`.
#[test]
fn numeric_array_for_of_computed_red_numeric_put() {
    assert_eq!(
        case_code("var o={};function k(){return 0;}o[k()]=3;o[0];"),
        3,
        "ordinary computed put control"
    );
    assert_eq!(
        case_code("var o={};for(o[0] of [4,5]){}o[0];"),
        5,
        "numeric object computed put"
    );
}

#[test]
fn numeric_array_for_of_computed_red_throwing_key() {
    assert_eq!(
        case_code(
            "function k(){throw 1;}var o={};var n=7;try{for(o[k()] of [4,5]){n=9;}}catch(e){n=e;}n;"
        ),
        1,
        "throwing key skips body and keeps payload"
    );
}

#[test]
fn numeric_array_for_of_computed_red_base_key_once() {
    assert_eq!(
        case_code(
            "function f(){var bn=0;var kn=0;var o={x:0};var s=0;for(((bn=bn+1)?o:o)[kn=kn+1] of [4,5]){s=s+1;}return bn*100+kn*10+s;}f();"
        ),
        222,
        "base and key once each iteration"
    );
}

/// Empty, rejected RHS, and strict own-this getter controls for the same head.
#[test]
fn numeric_array_for_of_computed_effects() {
    assert_eq!(
        case_code(
            "function f(){var bn=0;var kn=0;var o={x:0};for(((bn=bn+1)?o:o)[kn=kn+1] of []){}return bn*10+kn;}f();"
        ),
        0,
        "empty array does not evaluate key or base"
    );
    assert_eq!(
        case_code(
            "function k(){throw 1;}var o={};var n=0;try{for(o[k()] of 1){n=9;}}catch(e){if(e.name===\"TypeError\")n=2;else n=e;}n;"
        ),
        2,
        "non-array TypeError precedes key throw"
    );
    assert_eq!(
        case_code(
            "function f(){\"use strict\";var o={hit:false,get a(){if(this.hit){throw 1;}this.hit=true;return [1,2,3];}};var s=0;try{for(var x of o.a){s=s+x;}}catch(e){s=100;}return s;}f();"
        ),
        6,
        "strict own-this getter once"
    );
}

#[test]
fn numeric_array_for_of_control() {
    assert_eq!(
        case_code("var s=0;for(var x of [1,2,3,4]){if(x===3)break;s=s+x;}s;"),
        3,
        "break"
    );
    assert_eq!(
        case_code("var s=0;for(var x of [1,2,3,4]){if(x===2)continue;s=s+x;}s;"),
        8,
        "continue"
    );
    assert_eq!(
        case_code(
            "var s=0;outer:for(var x of [1,2]){for(var y of [10,20]){if(y===10)continue outer;s=s+y;}s=s+100;}s;"
        ),
        0,
        "labelled continue"
    );
    assert_eq!(
        case_code(
            "var s=0;outer:for(var x of [1,2,3]){for(var y of [10]){if(x===2)break outer;}s=s+x;}s;"
        ),
        1,
        "label break"
    );
}

#[test]
fn numeric_array_for_of_finally() {
    assert_eq!(
        case_code(
            "function f(){var s=0;for(var x of [1,2,3]){try{if(x===2)return s;s=s+x;}finally{s=s+10;}}return 99;}f();"
        ),
        11,
        "return through finally"
    );
    assert_eq!(
        case_code(
            "function f(){var n=0;try{for(var x of [1]){try{throw 1;}finally{n=2;}}}catch(e){n=n+10;}return n;}f();"
        ),
        12,
        "throw through finally"
    );
    assert_eq!(
        case_code(
            "function f(){var n=0;for(var x of [1,2]){try{if(x===1)throw 1;}catch(e){n=1;}finally{n=n+10;}}return n;}f();"
        ),
        21,
        "catch then finally continues"
    );
    assert_eq!(
        case_code("var s=0;for(var x of [1,2]){try{s=s+x;}finally{s=s+10;}}s;"),
        23,
        "finally continuation"
    );
    assert_eq!(
        completion_tag("1;for(var x of [2]){}"),
        3,
        "empty body keeps prior value"
    );
    assert_eq!(
        case_code("1;for(var x of [2]){}"),
        1,
        "empty body value is 1"
    );
    assert_eq!(
        completion_tag("1;for(var x of [2]){undefined;}"),
        0,
        "explicit undefined is a value"
    );
}

#[test]
fn numeric_array_for_of_roots() {
    assert_eq!(
        case_code("var s=0;for(var x of [1,2]){for(var y of [10,20]){s=s+x+y;}}s;"),
        66,
        "nested"
    );
    assert_eq!(
        case_code("var a=[1,2];var s=0;for(var x of a){s=s+x;}for(var y of a){s=s+y;}s;"),
        6,
        "repeat"
    );
    assert_eq!(
        case_code(
            "var a=[1,2,3];var s=0;var seen=0;for(var x of a){if(seen===0){a=null;var n=0;while(n<64){var o={v:n};n=n+1;}}s=s+x;seen=seen+1;}s*10+seen;"
        ),
        63,
        "iterator root across allocs"
    );
}

#[derive(Debug)]
struct Observed {
    refused: Option<String>,
    stack: Vec<Value>,
}

fn observe(source: &str, observer: arity_observer::Observer) -> Observed {
    free_floor();
    eprintln!("FOR_OF_OBSERVE {source}");
    let observer = Box::leak(Box::new(observer));
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_host_api_catalog(catalog.clone())
        .with_source_plugin(observer);
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-arity-observe")),
        options,
    )
    .unwrap_or_else(|error| panic!("observed source must compile: {source}\\n{error:?}"));
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
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
            for builtin in [
                "math::floor",
                "math::sqrt",
                "math::round",
                "math::powf",
                "math::is_nan",
                "math::is_finite",
                "math::trunc",
                "bytes::from_utf8",
            ] {
                registry.allow_builtin(builtin).expect(builtin);
            }
            registry.bind_vm_cached(&mut machine).expect("bind");
            machine.set_fuel(10_000_000);
            let status = machine.run();
            let stack = machine.stack().to_vec();
            eprintln!(
                "FOR_OF_OBSERVE_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={stack:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count()
            );
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.aot_exec_count(), machine.jit_native_exec_count()),
                (0, 0)
            );
            let refused = match status {
                Ok(VmStatus::Halted) => None,
                Ok(other) => Some(format!("{other:?}")),
                Err(error) => Some(format!("{error:?}")),
            };
            machine.reset_for_reuse().expect("reset");
            assert_eq!(machine.host_context().resource_count(), 0);
            Observed { refused, stack }
        })
        .expect("stack")
        .join()
        .unwrap_or_else(|_| panic!("observe thread failed: {source}"))
}

fn stack_int(value: &Value) -> i64 {
    match value {
        Value::Int(number) => *number,
        Value::Float(number) if number.fract() == 0.0 => *number as i64,
        other => panic!("integer evidence expected, got {other:?}"),
    }
}

/// Sole iterator root: after `a=null` a real collect still sees the captured
/// array active at the same generation; after the normal clear the same pair
/// is reclaimed and the production checked_base refuses.
#[test]
fn numeric_array_for_of_gc_iterator_lifetime() {
    let observed = observe(
        "var a=[1,2,3];var s=0;for(var x of a){a=null;s=s+x;}s;",
        arity_observer::Observer {
            array_iterator: true,
            ..arity_observer::Observer::default()
        },
    );
    let text = observed
        .refused
        .expect("cleared iterator token must be refused");
    assert!(
        text.contains("assertion failed"),
        "production checked_base must refuse, got {text}"
    );
    assert_eq!(
        observed.stack.last(),
        Some(&Value::Bool(false)),
        "refusing gate leaves Bool(false): {:?}",
        observed.stack
    );
    assert!(
        observed.stack.len() >= 13,
        "positive and negative evidence missing: {:?}",
        observed.stack
    );
    let positive = &observed.stack[..8];
    let before = stack_int(&positive[0]);
    let after = stack_int(&positive[1]);
    let state = stack_int(&positive[2]);
    let row_gen = stack_int(&positive[3]);
    let length = stack_int(&positive[5]);
    let token_slot = stack_int(&positive[6]);
    let token_gen = stack_int(&positive[7]);
    assert!(
        after > before,
        "real collect must grow collections: {before} -> {after}"
    );
    assert_eq!(
        state, 2,
        "iterator root keeps the array active: {positive:?}"
    );
    assert_eq!(row_gen, token_gen, "same generation while rooted");
    assert_eq!(length, 3, "header length still readable after collect");
    assert!(token_slot >= 0, "captured ref_slot");
    let negative = &observed.stack[8..12];
    let neg_before = stack_int(&negative[0]);
    let neg_after = stack_int(&negative[1]);
    let neg_state = stack_int(&negative[2]);
    let neg_gen = stack_int(&negative[3]);
    assert!(
        neg_after > neg_before,
        "post-clear collect must grow collections: {neg_before} -> {neg_after}"
    );
    assert!(
        neg_state == 3 || neg_gen != token_gen,
        "old pair must be free or reused, state={neg_state} gen={neg_gen} token={token_gen}"
    );
}

/// Natural GC on the existing 64-allocation loop, plus root census across
/// nested, repeated, and abrupt for-of calls.
#[test]
fn numeric_array_for_of_gc_natural_and_capacity() {
    let natural = observe(
        "var a=[1,2,3];var s=0;var seen=0;for(var x of a){if(seen===0){a=null;var n=0;while(n<64){var o={v:n};n=n+1;}}s=s+x;seen=seen+1;}s*10+seen;",
        arity_observer::Observer {
            array_root_census: true,
            ..arity_observer::Observer::default()
        },
    );
    assert!(
        natural.refused.is_none(),
        "natural census must halt: {natural:?}"
    );
    let evidence = census_tail(&natural.stack);
    eprintln!(
        "FOR_OF_GC natural start=({},{},{}) count={} samples={:?} end=({},{},{})",
        evidence.start_collections,
        evidence.start_active,
        evidence.start_capacity,
        evidence.count,
        evidence.samples,
        evidence.end_collections,
        evidence.end_active,
        evidence.end_capacity
    );
    assert!(
        evidence.end_collections > evidence.start_collections,
        "64 allocations must trigger at least one natural collection, not a budget constant"
    );
    assert_eq!(
        evidence.start_active,
        evidence.end_active + 1,
        "sole iterator root must be released after the loop, not leaked"
    );
    assert_eq!(
        evidence.end_capacity, evidence.start_capacity,
        "capacity must stay flat across the iteration"
    );
    assert!(
        (1..16).contains(&evidence.count),
        "for-of samples must stay within iterations, not the inner alloc loop: {}",
        evidence.count
    );
    let during_active: Vec<i64> = evidence.samples[..evidence.count as usize]
        .iter()
        .map(|pair| pair.0)
        .collect();
    let peak = *during_active.iter().max().expect("peak");
    assert!(
        peak <= evidence.start_active + 4,
        "active roots must stay within syntactic depth, peak={peak} samples={during_active:?}"
    );
    assert!(
        during_active.windows(2).all(|pair| pair[1] <= pair[0]),
        "active roots must not grow across iterations: {during_active:?}"
    );

    let capacity = observe(
        "function box(){return [1];}var s=0;for(var x of [1,2]){for(var y of box()){s=s+x+y;}}for(var x of box()){s=s+x;}for(var x of box()){s=s+x;}try{for(var x of box()){throw 1;}}catch(e){s=10;}finally{s=s+100;}s;",
        arity_observer::Observer {
            array_root_census: true,
            ..arity_observer::Observer::default()
        },
    );
    assert!(
        capacity.refused.is_none(),
        "capacity census must halt: {:?}",
        capacity.refused
    );
    let evidence = census_tail(&capacity.stack);
    eprintln!(
        "FOR_OF_GC capacity start=({},{},{}) count={} end=({},{},{})",
        evidence.start_collections,
        evidence.start_active,
        evidence.start_capacity,
        evidence.count,
        evidence.end_collections,
        evidence.end_active,
        evidence.end_capacity
    );
    let samples: Vec<(i64, i64)> = evidence.samples[..evidence.count as usize].to_vec();
    eprintln!("FOR_OF_GC capacity samples={samples:?}");
    assert!(
        evidence.end_active < evidence.start_active
            && evidence.start_active - evidence.end_active <= 2,
        "abrupt and return must release iterator roots without leaking: start={} end={}",
        evidence.start_active,
        evidence.end_active
    );
    assert!(
        evidence.end_capacity <= evidence.start_capacity + 8,
        "capacity did not restore: start={} end={}",
        evidence.start_capacity,
        evidence.end_capacity
    );
    assert!(
        evidence.count >= 4 && evidence.count <= 16,
        "nested/repeat/abrupt samples missing or unbounded: {samples:?}"
    );
    let peak = samples.iter().map(|pair| pair.0).max().expect("peak");
    assert!(
        peak <= evidence.start_active + 4,
        "nested active roots exceed syntactic depth: peak={peak} start={} {samples:?}",
        evidence.start_active
    );
    assert_eq!(
        evidence.fn_counts,
        [0, 0, 0],
        "script census must not publish function-local counters"
    );
    let cap_peak = samples.iter().map(|pair| pair.1).max().expect("cap");
    assert!(
        cap_peak <= evidence.start_capacity + 8,
        "root capacity grew with calls rather than depth: peak={cap_peak} start={} {samples:?}",
        evidence.start_capacity
    );

    let returned = observe(
        "function nested(){var s=0;for(var x of [1,2]){for(var y of [10]){s=s+x+y;}}return s;}function again(){var s=0;for(var x of [1]){s=s+x;}return s;}function abrupt(){var s=0;try{for(var x of [1,2]){throw 1;}}catch(e){s=10;}finally{s=s+100;}return s;}var s=0;for(var x of [1]){s=s+x;}s=s+nested();s=s+again();s=s+again();s=s+abrupt();for(var x of [1]){s=s+x;}s;",
        arity_observer::Observer {
            array_root_census: true,
            ..arity_observer::Observer::default()
        },
    );
    assert!(
        returned.refused.is_none(),
        "return census must halt: {:?}",
        returned.refused
    );
    let completion = returned
        .stack
        .iter()
        .find(|value| matches!(value, Value::Map(_)))
        .expect("completion");
    let number = field(completion, "number");
    assert!(
        matches!(number, Value::Float(value) if value == 137.0)
            || matches!(number, Value::Int(137)),
        "function for-of bodies must run before the second bookend, got {number:?}"
    );
    let evidence = census_tail(&returned.stack);
    let samples: Vec<(i64, i64)> = evidence.samples[..evidence.count as usize].to_vec();
    eprintln!(
        "FOR_OF_GC return start=({},{},{}) count={} samples={samples:?} end=({},{},{}) fn_counts={:?}",
        evidence.start_collections,
        evidence.start_active,
        evidence.start_capacity,
        evidence.count,
        evidence.end_collections,
        evidence.end_active,
        evidence.end_capacity,
        evidence.fn_counts
    );
    assert!(
        evidence.count >= 2 && evidence.count <= 8,
        "bookend samples missing: {samples:?}"
    );
    assert_eq!(
        samples.first().map(|pair| pair.0),
        samples.last().map(|pair| pair.0),
        "return/reentry must not leak iterator roots: {samples:?}"
    );
    assert!(
        evidence.end_active < evidence.start_active
            && evidence.start_active - evidence.end_active <= 2,
        "bookend root must be released: start={} end={}",
        evidence.start_active,
        evidence.end_active
    );
    assert_eq!(
        evidence.end_capacity, evidence.start_capacity,
        "capacity must stay flat across calls"
    );
    assert!(
        samples
            .iter()
            .all(|pair| pair.0 <= evidence.start_active + 1),
        "bookend active roots grew with calls: {samples:?}"
    );
}

struct CensusEvidence {
    start_collections: i64,
    start_active: i64,
    start_capacity: i64,
    count: i64,
    fn_counts: [i64; 3],
    samples: Vec<(i64, i64)>,
    end_collections: i64,
    end_active: i64,
    end_capacity: i64,
}

fn census_tail(stack: &[Value]) -> CensusEvidence {
    assert!(stack.len() >= 42, "census tail missing: {stack:?}");
    let end = stack.len();
    let end_capacity = stack_int(&stack[end - 1]);
    let end_active = stack_int(&stack[end - 2]);
    let end_collections = stack_int(&stack[end - 3]);
    let mut pairs = Vec::new();
    for index in 0..16 {
        let base = end - 3 - 32 + index * 2;
        pairs.push((stack_int(&stack[base]), stack_int(&stack[base + 1])));
    }
    let fn_counts = [
        stack_int(&stack[end - 38]),
        stack_int(&stack[end - 37]),
        stack_int(&stack[end - 36]),
    ];
    let count = stack_int(&stack[end - 39]);
    let start_capacity = stack_int(&stack[end - 40]);
    let start_active = stack_int(&stack[end - 41]);
    let start_collections = stack_int(&stack[end - 42]);
    assert!(
        (0..=16).contains(&count),
        "sample count out of the stored window: {count}"
    );
    CensusEvidence {
        start_collections,
        start_active,
        start_capacity,
        count,
        fn_counts,
        samples: pairs,
        end_collections,
        end_active,
        end_capacity,
    }
}
