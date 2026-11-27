//! Dense JsValue Array values: mixed/reference literals, index/alias/dynamic
//! consumers, function call-return handles, for-of elements and GC lifetime.
//! Interpreter-only, JIT-disabled acceptance; no host NumberArray shortcut.
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

fn tag_of(value: &Value) -> i64 {
    match field(value, "tag") {
        Value::Int(tag) => tag,
        Value::Float(tag) => tag as i64,
        other => panic!("tag: {other:?}"),
    }
}

fn number_of(value: &Value) -> f64 {
    match field(value, "number") {
        Value::Float(number) => number,
        Value::Int(number) => number as f64,
        other => panic!("number: {other:?}"),
    }
}

fn flag_of(value: &Value) -> bool {
    match field(value, "flag") {
        Value::Bool(flag) => flag,
        other => panic!("flag: {other:?}"),
    }
}

fn stack_number(value: &Value) -> f64 {
    match value {
        Value::Int(number) => *number as f64,
        Value::Float(number) => *number,
        other => panic!("stack number: {other:?}"),
    }
}

fn prepare_vm(
    source: &str,
    plugin: &'static dyn vm::SourcePlugin,
    flavor: vm::SourceFlavor,
) -> vm::Vm {
    free_floor();
    eprintln!("ARRAY_VALUES_SOURCE {source}");
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_host_api_catalog(catalog.clone())
        .with_source_plugin(plugin);
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source, flavor, options,
    )
    .unwrap_or_else(|error| panic!("source must compile: {source}\n{error:?}"));
    eprintln!("ARRAY_VALUES_COMPILED");
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

fn run_capture(machine: &mut vm::Vm, _source: &str) -> (Option<String>, Vec<Value>) {
    let before = machine.get_fuel();
    assert!(
        matches!(before, Some(fuel) if fuel <= 10_000_000),
        "interpreter fuel {before:?}"
    );
    let status = machine.run();
    let after = machine.get_fuel();
    eprintln!(
        "ARRAY_VALUES_VM status={status:?} jit_enabled={} aot={} jit_native={} fuel_before={before:?} fuel_after={after:?} stack={:?}",
        machine.jit_config().enabled,
        machine.aot_exec_count(),
        machine.jit_native_exec_count(),
        machine.stack()
    );
    assert!(!machine.jit_config().enabled);
    assert_eq!(
        (machine.aot_exec_count(), machine.jit_native_exec_count()),
        (0, 0)
    );
    assert!(
        matches!((before, after), (Some(left), Some(right)) if right <= left && right <= 10_000_000)
    );
    assert!(
        machine
            .capture_certification_stats()
            .is_some_and(|stats| stats.valid)
    );
    let resources = machine.host_context().resource_count();
    assert!(resources <= 1, "at most one real B1 realm");
    let refused = match status {
        Ok(VmStatus::Halted) => None,
        Ok(other) => Some(format!("{other:?}")),
        Err(error) => Some(format!("{error:?}")),
    };
    let stack = machine.stack().to_vec();
    machine.reset_for_reuse().expect("reset");
    assert_eq!(machine.host_context().resource_count(), 0);
    assert!(!machine.jit_config().enabled);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    (refused, stack)
}

fn run(machine: &mut vm::Vm, source: &str) -> Vec<Value> {
    let (refused, stack) = run_capture(machine, source);
    assert!(refused.is_none(), "{source}: {refused:?}");
    stack
}

fn execute(source: &str) -> Value {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let mut machine = prepare_vm(&source, &JAVASCRIPT_ES5_PLUGIN, javascript_es5_flavor());
            let mut stack = run(&mut machine, &source);
            stack.pop().expect("completion")
        })
        .expect("stack")
        .join()
        .expect("test thread")
}

fn observe(source: &str, observer: arity_observer::Observer) -> Vec<Value> {
    observe_refusable(source, observer).1
}

fn observe_refusable(
    source: &str,
    observer: arity_observer::Observer,
) -> (Option<String>, Vec<Value>) {
    let source = source.to_owned();
    let observer = Box::leak(Box::new(observer));
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let mut machine = prepare_vm(
                &source,
                observer,
                vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-arity-observe")),
            );
            run_capture(&mut machine, &source)
        })
        .expect("stack")
        .join()
        .expect("test thread")
}

fn assert_true(value: &Value, label: &str) {
    assert_eq!(tag_of(value), 2, "{label}: boolean JsValue, got {value:?}");
    assert!(flag_of(value), "{label}: expected true, got {value:?}");
}

fn assert_number(value: &Value, expected: f64, label: &str) {
    assert_eq!(tag_of(value), 3, "{label}: number JsValue, got {value:?}");
    assert_eq!(number_of(value), expected, "{label}");
}

/// RED tracer: a mixed string/number literal must allocate and read back the
/// string element as a real JsValue with its actual contents.
#[test]
fn dense_tagged_string_literal_index_reads() {
    let result = execute("var a=[\"x\",1];a[0]===\"x\";");
    assert_true(&result, "mixed literal element read");
    let raw = execute("var a=[\"x\",1];a[0];");
    assert_eq!(tag_of(&raw), 4, "a[0] is a String JsValue: {raw:?}");
}

/// The packed numeric fast path and the tagged writer share one layout
/// header; the tagged node charges five cells per element (8 more than a
/// packed pair), and its header is kind 6, length 2, null prototype.
#[test]
fn dense_tagged_header_charge_and_stride() {
    let observed = observe(
        "var a=[null,1];0;",
        arity_observer::Observer {
            array_header: Some("a".to_owned()),
            extra_selectors: vec![7, 0],
            ..arity_observer::Observer::default()
        },
    );
    let count = observed.len();
    let completion = &observed[count - 7];
    assert_number(completion, 0.0, "completion");
    let kind = stack_number(&observed[count - 4]);
    let length = stack_number(&observed[count - 3]);
    let proto_slot = stack_number(&observed[count - 2]);
    let proto_gen = stack_number(&observed[count - 1]);
    assert_eq!(kind, 6.0, "tagged array private kind");
    assert_eq!(length, 2.0, "header length");
    assert_eq!((proto_slot, proto_gen), (0.0, 0.0), "null prototype pair");
    // Tail selectors: [node cells, live nodes].
    let tagged_cells = stack_number(&observed[count - 6]);
    let tagged_live = stack_number(&observed[count - 5]);
    eprintln!("ARRAY_VALUES tagged cells={tagged_cells} live={tagged_live}");

    let packed = observe(
        "var a=[0,1];0;",
        arity_observer::Observer {
            extra_selectors: vec![7, 0],
            ..arity_observer::Observer::default()
        },
    );
    let count = packed.len();
    let packed_cells = stack_number(&packed[count - 2]);
    let packed_live = stack_number(&packed[count - 1]);
    eprintln!("ARRAY_VALUES packed cells={packed_cells} live={packed_live}");
    assert_eq!(tagged_live, packed_live, "one array node in each arm");
    assert_eq!(
        tagged_cells - packed_cells,
        8.0,
        "tagged pairs charge 5+5 cells vs packed 5+1 plus the same raw header"
    );
}

/// All seven element kinds survive allocation and index reads, including an
/// alias and a non-literal integer index.
#[test]
fn dense_tagged_seven_values_and_dynamic_index() {
    let result = execute(
        "var o={n:5};var f=function(){return 7;};var a=[1,\"s\",true,null,undefined,o,f];var i=1;a[0]===1&&a[i]===\"s\"&&a[2]===true&&a[3]===null&&a[4]===undefined&&a[5]===o&&a[6]===f;",
    );
    assert_true(&result, "seven JsValue element kinds");
}

/// Alias identity, element writes of reference values and `.length`.
#[test]
fn dense_tagged_alias_write_and_length() {
    let result = execute(
        "var o={n:1};var a=[\"x\",1];var b=a;b[0]=\"y\";b[1]=o;a.length===2&&a[0]===\"y\"&&b[1]===o;",
    );
    assert_true(&result, "alias write and length");
    let length = execute("var a=[\"x\",1];a.length;");
    assert_number(&length, 2.0, "length read");
}

/// Function parameters and call results carry the same Array handle; the
/// caller-side dynamic receiver reads and writes real elements.
#[test]
fn dense_tagged_function_parameter_and_call_result() {
    let parameter = execute("function f(a){a[1]=\"z\";return a[0];}var r=f([\"w\",2]);r===\"w\";");
    assert_true(&parameter, "parameter element write");
    let returned = execute("function g(){return [\"q\",3];}var x=g();x[0]===\"q\"&&x[1]===3;");
    assert_true(&returned, "call-result handle indexing");
    let dynamic = execute("function g(){return [\"q\",3];}var i=1;g()[i]===3;");
    assert_true(&dynamic, "dynamic receiver and index");
}

/// for-of reads actual JsValue elements from tagged and call-result arrays.
#[test]
fn dense_tagged_for_of_values() {
    let literal = execute(
        "var a=[\"x\",1,true];var ok=0;for(var v of a){if(v===\"x\"){ok=ok+1;}if(v===1){ok=ok+10;}if(v===true){ok=ok+100;}}ok;",
    );
    assert_number(&literal, 111.0, "tagged for-of values");
    let dynamic = execute(
        "function f(){return [\"x\",2];}var ok=0;for(var v of f()){if(v===\"x\"){ok=ok+1;}if(v===2){ok=ok+10;}}ok;",
    );
    assert_number(&dynamic, 11.0, "dynamic for-of values");
}

/// A loop back edge widens the right operand from Number to Dynamic at a
/// site first proved as the exact String+Number pair. The sealed joined
/// primitive-addition site must select dispatch, so the second iteration
/// concatenates the real String element instead of reusing the stale exact
/// Number converter.
#[test]
fn dense_tagged_add_backedge_reopens_dynamic_concat() {
    // Control: one iteration keeps the exact String+Number pair and is "a1".
    let control = execute("var s=\"a\",t=1;for(var x of [\"b\"]){s=s+t;t=x;}s===\"a1\";");
    assert_true(&control, "single-iteration String+Number control");
    // Two iterations: Number concatenation first, then a real String element
    // arrives on the back edge and must concatenate as "a1b".
    let result = execute("var s=\"a\",t=1;for(var x of [\"b\",\"c\"]){s=s+t;t=x;}s===\"a1b\";");
    assert_true(&result, "back-edge String+Dynamic concatenation");
}

fn gc_source(retain_array: bool) -> String {
    if retain_array {
        "var o={n:5};var a=[o];o=null;function g(){var i=0;while(i<80){var t={v:i};i=i+1;}return 0;}g();a[0].n;".to_owned()
    } else {
        "var o={n:5};var a=[o];o=null;a=null;function g(){var i=0;while(i<80){var t={v:i};i=i+1;}return 0;}g();7;".to_owned()
    }
}

/// A tagged element is a real GC edge: with only the array rooted, a natural
/// collection during 80 allocations must keep the element object alive.
#[test]
fn dense_tagged_gc_element_edge_retains_child() {
    let retained = observe(
        &gc_source(true),
        arity_observer::Observer {
            pre_collect: true,
            extra_selectors: vec![5, 0],
            ..arity_observer::Observer::default()
        },
    );
    let count = retained.len();
    let completion = &retained[count - 3];
    assert_number(completion, 5.0, "element object survived collection");
    let retained_collections = stack_number(&retained[count - 2]);
    let retained_live = stack_number(&retained[count - 1]);
    eprintln!("ARRAY_VALUES GC retained collections={retained_collections} live={retained_live}");
    assert!(
        retained_collections >= 1.0,
        "80 allocations must trigger a real collection"
    );

    let dropped = observe(
        &gc_source(false),
        arity_observer::Observer {
            pre_collect: true,
            extra_selectors: vec![5, 0],
            ..arity_observer::Observer::default()
        },
    );
    let count = dropped.len();
    let completion = &dropped[count - 3];
    assert_number(completion, 7.0, "dropped arm completion");
    let dropped_live = stack_number(&dropped[count - 1]);
    eprintln!("ARRAY_VALUES GC dropped live={dropped_live}");
    // The retained arm keeps the array, its element object and the object's
    // own traced key string; the dropped arm keeps none of them. At least the
    // array and the element object must remain reachable.
    assert!(
        retained_live >= dropped_live + 2.0,
        "sole array root must retain its element (retained={retained_live} dropped={dropped_live})"
    );
}

/// An element/prototype cycle with no roots is still garbage: the collector
/// sweeps both nodes, so the tail live count matches the acyclic dropped arm.
#[test]
fn dense_tagged_gc_element_cycle_collected() {
    let cycle = observe(
        "var o={};var a=[o];function link(x,y){Object.setPrototypeOf(x,y);Object.setPrototypeOf(x,y);return 0;}link(o,a);o=null;a=null;function g(){var i=0;while(i<80){var t={v:i};i=i+1;}return 0;}g();7;",
        arity_observer::Observer {
            pre_collect: true,
            extra_selectors: vec![5, 0],
            ..arity_observer::Observer::default()
        },
    );
    let count = cycle.len();
    let completion = &cycle[count - 3];
    assert_number(completion, 7.0, "cycle arm completion");
    let cycle_live = stack_number(&cycle[count - 1]);

    // Same two link calls, but the second unlinks the prototype edge, so the
    // object and array are unreachable garbage exactly as in the cycle arm.
    let acyclic = observe(
        "var o={};var a=[o];function link(x,y){Object.setPrototypeOf(x,y);Object.setPrototypeOf(x,null);return 0;}link(o,a);o=null;a=null;function g(){var i=0;while(i<80){var t={v:i};i=i+1;}return 0;}g();7;",
        arity_observer::Observer {
            pre_collect: true,
            extra_selectors: vec![5, 0],
            ..arity_observer::Observer::default()
        },
    );
    let count = acyclic.len();
    let acyclic_live = stack_number(&acyclic[count - 1]);
    eprintln!("ARRAY_VALUES GC cycle live={cycle_live} acyclic live={acyclic_live}");
    assert_eq!(
        cycle_live, acyclic_live,
        "unreachable element/prototype cycle must be collected, not retained"
    );
}

/// Object.setPrototypeOf cycle rejection follows [[Prototype]] links only: an
/// array element pointing at the object is a legal, non-prototype backedge.
#[test]
fn dense_tagged_element_backedge_is_not_proto_cycle() {
    let result =
        execute("var o={};var a=[o];Object.setPrototypeOf(o,a);Object.getPrototypeOf(o)===a;");
    assert_true(&result, "element backedge is not a prototype cycle");
}

/// The subsequent reverse prototype link is a true cycle: it must raise a
/// managed TypeError and leave the previous prototype pairs untouched.
#[test]
fn dense_tagged_proto_cycle_rejected_keeps_old_pair() {
    let result = execute(
        "var o={};var a=[o];Object.setPrototypeOf(o,a);var bad=0;try{Object.setPrototypeOf(a,o);}catch(e){bad=1;}bad===1&&Object.getPrototypeOf(a)===null&&Object.getPrototypeOf(o)===a;",
    );
    assert_true(&result, "prototype cycle rejected with old pair preserved");
}

/// After the sole array root is dropped and a real collection reclaims it,
/// the production checked_base on the captured identity refuses instead of
/// resurrecting the old node.
#[test]
fn dense_tagged_gc_stale_checked_refusal() {
    let (refused, observed) = observe_refusable(
        "var o={n:1};var a=[o];o=null;a=null;var z={};7;",
        arity_observer::Observer {
            force_gc: true,
            captured: Some(arity_observer::CapturedToken {
                binding: "a".to_owned(),
                units: Vec::new(),
                survivor: None,
            }),
            ..arity_observer::Observer::default()
        },
    );
    assert!(
        refused
            .as_deref()
            .is_some_and(|text| text.contains("assertion failed")),
        "production checked_base must refuse the stale identity: {refused:?}"
    );
    let count = observed.len();
    let failure = &observed[count - 1];
    assert!(
        matches!(failure, Value::Bool(false)),
        "checked_base refusal leaves the failing operand: {:?}",
        &observed[count.saturating_sub(6)..]
    );
    let token_gen = stack_number(&observed[count - 2]);
    let token_slot = stack_number(&observed[count - 3]);
    let row_gen = stack_number(&observed[count - 4]);
    let row_state = stack_number(&observed[count - 5]);
    let collections = stack_number(&observed[count - 6]);
    eprintln!(
        "ARRAY_VALUES GC stale collections={collections} state={row_state} row_gen={row_gen} slot={token_slot} token_gen={token_gen}"
    );
    assert!(collections >= 1.0, "a real collection must have run");
    assert!(
        row_state == 3.0 || row_gen != token_gen,
        "old array identity must be free or regenerated: state={row_state} row_gen={row_gen} token_gen={token_gen}"
    );
}

#[test]
fn dense_tagged_for_of_item_root_across_source_delete() {
    let result = execute(
        "var src=[{n:1}];var dst=[\"a\",\"b\"];function churn(s){s[0]=null;var i=0;while(i<80){var t={v:i};i=i+1;}return 0;}for(dst[0+churn(src)] of src){}dst[0].n===1;",
    );
    assert_true(&result, "current item survives source deletion and GC");
}
