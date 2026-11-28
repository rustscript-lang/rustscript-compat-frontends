//! Finite witness set for MicroQuickJS `Math.fround` in the certified ES5 interpreter.
use pd_vm_compat_frontends::{JAVASCRIPT_ES5_PLUGIN, javascript_es5_flavor};
use vm::{HostFunctionRegistry, Value, VmStatus};

#[derive(Clone, Copy, Debug)]
enum Expected {
    Bits(u64),
    Nan,
}

use Expected::{Bits, Nan};

fn execute(source: &str) -> Value {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn_scoped(scope, || {
                let catalog =
                    pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
                let options = vm::CompileSourceFileOptions::new()
                    .with_host_api_catalog(catalog.clone())
                    .with_source_plugin(&JAVASCRIPT_ES5_PLUGIN);
                let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
                    source,
                    javascript_es5_flavor(),
                    options,
                )
                .unwrap_or_else(|error| panic!("compile {source}: {error:?}"));
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
                // Selecting Math installs the whole namespace, including shared kernels.
                for name in [
                    "abs", "acos", "asin", "atan", "atan2", "ceil", "copysign", "cos", "exp",
                    "exp2", "floor", "is_finite", "is_infinite", "is_nan", "ln", "log10", "log2",
                    "powf", "signum", "sin", "sqrt", "tan", "trunc",
                ] {
                    registry
                        .allow_builtin(format!("math::{name}"))
                        .unwrap_or_else(|error| panic!("math::{name}: {error:?}"));
                }
                registry
                    .allow_builtin("bytes::from_utf8")
                    .expect("numeric conversion UTF-8 builtin");
                registry.bind_vm_cached(&mut machine).expect("bind catalog");

                const FUEL: u64 = 10_000_000;
                machine.set_fuel(FUEL);
                assert_eq!(machine.get_fuel(), Some(FUEL));
                let status = machine
                    .run()
                    .unwrap_or_else(|error| panic!("run {source}: {error:?}"));
                assert_eq!(status, VmStatus::Halted, "{source}");
                assert!(!machine.jit_config().enabled);
                assert_eq!(machine.aot_exec_count(), 0);
                assert_eq!(machine.jit_native_exec_count(), 0);
                assert!(machine.get_fuel().is_some_and(|left| left < FUEL));
                assert!(machine.capture_certification_stats().is_some_and(|s| s.valid));
                assert!(machine.host_context().resource_count() <= 1);
                let result = machine.stack().last().expect("completion").clone();
                machine.reset_for_reuse().expect("reset VM");
                assert_eq!(machine.host_context().resource_count(), 0);
                result
            })
            .expect("interpreter thread")
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}

fn expect(prelude: &str, expression: &str, expected: Expected) {
    let source = format!("{prelude}{expression};");
    // `--nocapture` exposes the same program bytes for the pinned mqjs run.
    let oracle = match expected {
        Bits(bits) => format!("{bits:016x}"),
        Nan => "NaN".to_owned(),
    };
    eprintln!("FROUND_ORACLE {oracle} {source}");
    let Value::Map(fields) = execute(&source) else {
        panic!("{source}: expected a Number box");
    };
    assert_eq!(
        fields.get(&Value::string("tag")),
        Some(&Value::Int(3)),
        "{source}"
    );
    let Some(Value::Float(number)) = fields.get(&Value::string("number")) else {
        panic!("{source}: Number box requires a Float payload");
    };
    match expected {
        Bits(bits) => assert_eq!(
            number.to_bits(),
            bits,
            "{source}: actual {:016x}, expected {bits:016x}",
            number.to_bits(),
        ),
        Nan => assert!(number.is_nan(), "{source}: expected NaN, got {number}"),
    }
}

const ONE: u64 = 0x3ff0_0000_0000_0000;
const POS_INF: u64 = 0x7ff0_0000_0000_0000;
const NEG_INF: u64 = 0xfff0_0000_0000_0000;
const NEG_ZERO: u64 = 0x8000_0000_0000_0000;
const MID_A: &str = "var a = 1 + 1/16777216;";

type Witness = (&'static str, &'static str, Expected);

fn expect_all(cases: &[Witness]) {
    // Each witness needs its own certified program. Bound the concurrent
    // compiles so the exact selectors do not serialize the entire workload.
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(8)
        .min(cases.len());
    std::thread::scope(|scope| {
        for worker in 0..workers {
            scope.spawn(move || {
                for &(prelude, expression, expected) in cases.iter().skip(worker).step_by(workers) {
                    expect(prelude, expression, expected);
                }
            });
        }
    });
}

#[test]
fn p8_math_fround_interpreter() {
    let specials = [
        ("Math.fround(0.1)", Bits(0x3fb9_9999_a000_0000)),
        ("Math.fround(0)", Bits(0)),
        ("Math.fround(-0)", Bits(NEG_ZERO)),
        ("Math.fround(1/0)", Bits(POS_INF)),
        ("Math.fround(-1/0)", Bits(NEG_INF)),
        ("Math.fround()", Nan),
        ("Math.fround(0/0)", Nan),
    ];
    let cases: Vec<_> = call_cases()
        .into_iter()
        .chain(specials.map(|(expression, expected)| ("", expression, expected)))
        .collect();
    expect_all(&cases);
}

fn call_cases() -> [Witness; 5] {
    // Every call must evaluate both actuals before invoking the first valueOf.
    // The borrowed receiver and the ignored tail have independent tripwires.
    let shape = (
        concat!(
            "'use strict'; var f = Math.fround;",
            "var trace = {order:0, tail:0, receiver:0};",
            "var first = {owner:trace, marker:17, valueOf:function(){",
            "if(this.marker !== 17 || this.owner.order !== 12) throw 1;",
            "this.owner.order = 123; return 1 + 1/16777216;}};",
            "var tail = {owner:trace, valueOf:function(){this.owner.tail++; throw 2;},",
            "toString:function(){this.owner.tail++; throw 3;}};",
            "var borrowed = {owner:trace, round:f, valueOf:function(){this.owner.receiver++; throw 4;}};",
            "if(f.length !== 1) throw 5;",
            "var r = Math.fround((trace.order = 1, first), (trace.order = trace.order*10+2, tail));",
            "if(r !== 1 || trace.order !== 123) throw 6;",
            "r = f((trace.order = 1, first), (trace.order = trace.order*10+2, tail));",
            "if(r !== 1 || trace.order !== 123) throw 7;",
            "r = borrowed.round((trace.order = 1, first), (trace.order = trace.order*10+2, tail));",
            "if(r !== 1 || trace.order !== 123) throw 8;",
            "if(trace.tail !== 0 || trace.receiver !== 0) throw 9;",
        ),
        "r",
        Bits(ONE),
    );

    // Direct primitive String, valueOf returning String, then Number-hint fallback.
    let string = ("", "Math.fround('-2.5')", Bits(0xc004_0000_0000_0000));
    let fallback = (
        concat!(
            "'use strict'; var o = {n:0, valueOf:function(){this.n++; return '-2.5';},",
            "toString:function(){throw 1;}};",
            "if(Math.fround(o) !== -2.5 || o.n !== 1) throw 2;",
            "var fallback = {order:0, valueOf:function(){this.order = this.order*10+1; return this;},",
            "toString:function(){this.order = this.order*10+2; return 1 + 1/16777216;}};",
            "var r = Math.fround(fallback); if(fallback.order !== 12) throw 3;",
        ),
        "r",
        Bits(ONE),
    );

    let type_error = (
        concat!(
            "'use strict'; var a = 1 + 1/16777216;",
            "var o = {order:0, valueOf:function(){this.order = this.order*10+1; return this;},",
            "toString:function(){this.order = this.order*10+2; return this;}};",
            "var caught = false; try { Math.fround(o); } catch(e) { caught = e.name === 'TypeError'; }",
            "if(!caught || o.order !== 12) throw 1;",
        ),
        "Math.fround(a)",
        Bits(ONE),
    );

    let throws = (
        concat!(
            "'use strict'; var a = 1 + 1/16777216;",
            "var trace = {expr:0, coerced:0, fallback:0};",
            "var tail = {owner:trace, valueOf:function(){this.owner.coerced++; throw 1;},",
            "toString:function(){this.owner.coerced++; throw 2;}};",
            "var getError = {}, callError = {};",
            "var getter = {owner:trace, token:getError, n:0, get valueOf(){this.n++; throw this.token;},",
            "toString:function(){this.owner.fallback++; throw 3;}};",
            "var method = {owner:trace, token:callError, n:0, valueOf:function(){this.n++; throw this.token;},",
            "toString:function(){this.owner.fallback++; throw 4;}};",
            "var gotGet = false, gotCall = false;",
            "try { Math.fround(getter, (trace.expr++, tail)); } catch(getThrown) { gotGet = getThrown === getError; }",
            "if(!gotGet || getter.n !== 1 || trace.expr !== 1) throw 5;",
            "try { Math.fround(method, (trace.expr++, tail)); } catch(callThrown) { gotCall = callThrown === callError; }",
            "if(!gotCall || method.n !== 1 || trace.expr !== 2) throw 6;",
            "if(trace.coerced !== 0 || trace.fallback !== 0) throw 7;",
        ),
        "Math.fround(a)",
        Bits(ONE),
    );
    [shape, string, fallback, type_error, throws]
}

#[test]
fn p8_math_fround_ties() {
    // Here A's even neighbor is lower and B's even neighbor is upper.
    expect(MID_A, "a", Bits(0x3ff0_0000_1000_0000));
    expect(MID_A, "Math.fround(a)", Bits(ONE));
    let mid_b = "var b = 2 - 1/16777216;";
    expect(mid_b, "b", Bits(0x3fff_ffff_f000_0000));
    expect(mid_b, "Math.fround(b)", Bits(0x4000_0000_0000_0000));
}

#[test]
fn p8_math_fround_subnormals() {
    // `up` and `down` are the adjacent binary64 values on either side of half.
    let prelude = concat!(
        "var u = 1/8388608; var u6 = u*u*u*u*u*u;",
        "var half = u6/4096; var minSub = u6/2048;",
        "var up = half + half/4503599627370496; var down = half - half/9007199254740992;",
    );
    let cases = [
        ("Math.fround(half)", 0),
        ("Math.fround(-half)", NEG_ZERO),
        ("Math.fround(down)", 0),
        ("Math.fround(-down)", NEG_ZERO),
        ("Math.fround(up)", 0x36a0_0000_0000_0000),
        ("Math.fround(-up)", 0xb6a0_0000_0000_0000),
        ("Math.fround(minSub)", 0x36a0_0000_0000_0000),
        ("Math.fround(-minSub)", 0xb6a0_0000_0000_0000),
    ]
    .map(|(expression, bits)| (prelude, expression, Bits(bits)));
    expect_all(&cases);
}

#[test]
fn p8_math_fround_overflow() {
    let prelude = concat!(
        "var p20 = 1048576; var p40 = p20*p20; var p80 = p40*p40; var p120 = p80*p40;",
        "var p127 = p120*128; var two128 = p127*2;",
        "var maxF = two128 - p127/8388608; var ov = two128 - p127/16777216;",
        "var below = ov - p127/4503599627370496;",
    );
    let cases = [
        ("Math.fround(maxF)", 0x47ef_ffff_e000_0000),
        ("Math.fround(-maxF)", 0xc7ef_ffff_e000_0000),
        ("Math.fround(below)", 0x47ef_ffff_e000_0000),
        ("Math.fround(-below)", 0xc7ef_ffff_e000_0000),
        ("Math.fround(ov)", POS_INF),
        ("Math.fround(-ov)", NEG_INF),
        ("Math.fround(two128)", POS_INF),
    ]
    .map(|(expression, bits)| (prelude, expression, Bits(bits)));
    expect_all(&cases);
}
