//! Public Object.setPrototypeOf on the certified, JIT-disabled interpreter.
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
    eprintln!("PROTO_SOURCE {source}");
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
    eprintln!("PROTO_COMPILED");
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
    // The ordinary realm's bootstrap publishes its fixed ASCII strings through
    // the Core bytes initializer, so this capability is part of every emitted
    // realm program (the Object.keys harness allows the same builtin).
    registry
        .allow_builtin("bytes::from_utf8")
        .expect("bootstrap strings");
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
                "PROTO_VM status={status:?} jit_enabled={} aot={} jit_native={} fuel_before={before:?} fuel_after={after:?} stack={:?}",
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
    match field(&result, "number") {
        Value::Float(number) => number as i64,
        Value::Int(number) => number,
        other => panic!("case: {other:?}\n{source}"),
    }
}

#[test]
fn object_set_prototype_public_identity_missing() {
    let code = case_code(
        "var p={n:1};var o={m:2};var r=Object.setPrototypeOf(o,p);var n=0;if(!(r===o))n=1;else if(!(Object.getPrototypeOf(o)===p))n=2;else if(!(o.n===1))n=3;else if(!(\"n\" in o))n=4;else if(\"n\" in Object.prototype)n=5;else if(!(p.n===1))n=6;n;",
    );
    assert_eq!(code, 0, "public setPrototypeOf identity and inherited get");
}

#[test]
fn object_set_prototype_restore_null_and_old_link() {
    let code = case_code(
        "var old={n:1};var neu={n:2};var o={};var n=0;if(!(Object.setPrototypeOf(o,old)===o&&o.n===1&&(\"n\" in o)))n=1;else if(!(Object.setPrototypeOf(o,neu)===o&&o.n===2&&Object.getPrototypeOf(o)===neu&&Object.getPrototypeOf(o)!==old))n=2;else if(!(Object.setPrototypeOf(o,old)===o&&o.n===1&&Object.getPrototypeOf(o)===old))n=3;else if(!(Object.setPrototypeOf(o,null)===o&&Object.getPrototypeOf(o)===null&&o.n===undefined&&!(\"n\" in o)))n=4;else if(!(Object.setPrototypeOf(o,neu)===o&&o.n===2&&Object.setPrototypeOf(o,neu)===o))n=5;n;",
    );
    assert_eq!(code, 0, "restore, null, and disconnected old link");
}

#[test]
fn object_set_prototype_cycle_type_error_chain() {
    let code = case_code(
        "var o={};var p={};var q={};var before=Object.getPrototypeOf(o);var known;try{var x=null;x.y;}catch(t){known=t;}var n=0;try{Object.setPrototypeOf(o,o);n=1;}catch(e){if(!(e.name===\"TypeError\"&&Object.getPrototypeOf(e)===Object.getPrototypeOf(known)&&Object.getPrototypeOf(o)===before&&o.n===undefined))n=1;}Object.setPrototypeOf(p,o);try{Object.setPrototypeOf(o,p);if(n===0)n=2;}catch(e){if(!(n===0&&e.name===\"TypeError\"&&Object.getPrototypeOf(o)===before&&Object.getPrototypeOf(p)===o&&p.n===undefined))n=2;}Object.setPrototypeOf(q,p);try{Object.setPrototypeOf(o,q);if(n===0)n=3;}catch(e){if(!(n===0&&e.name===\"TypeError\"&&Object.getPrototypeOf(e)===Object.getPrototypeOf(known)&&Object.getPrototypeOf(o)===before&&Object.getPrototypeOf(q)===p))n=3;}n;",
    );
    assert_eq!(code, 0, "self, two-node, and three-node cycles");
}

#[test]
fn object_set_prototype_prevent_extensions_same_and_change() {
    let code = case_code(
        "var o={};var before=Object.getPrototypeOf(o);Object.preventExtensions(o);var n=0;if(!(Object.setPrototypeOf(o,before)===o&&Object.getPrototypeOf(o)===before))n=1;var p={x:1};try{Object.setPrototypeOf(o,p);if(n===0)n=2;}catch(e){if(!(n===0&&e.name===\"TypeError\"&&Object.getPrototypeOf(o)===before&&o.x===undefined))n=2;}var z=Object.create(null);Object.preventExtensions(z);if(n===0&&!(Object.setPrototypeOf(z,null)===z&&Object.getPrototypeOf(z)===null))n=3;try{Object.setPrototypeOf(z,{});if(n===0)n=4;}catch(e){if(!(n===0&&e.name===\"TypeError\"&&Object.getPrototypeOf(z)===null))n=4;}function f(){\"use strict\";return 7;}var fb=Object.getPrototypeOf(f);Object.preventExtensions(f);if(n===0&&!(Object.setPrototypeOf(f,fb)===f&&Object.getPrototypeOf(f)===fb&&f()===7))n=5;try{Object.setPrototypeOf(f,p);if(n===0)n=6;}catch(e){if(!(n===0&&e.name===\"TypeError\"&&Object.getPrototypeOf(f)===fb&&f()===7))n=6;}n;",
    );
    assert_eq!(code, 0, "nonextensible same pair succeeds, change fails");
}

#[test]
fn object_set_prototype_array_function_and_setter() {
    let code = case_code(
        "var n=0;var a=[7,8];var p={z:3};if(!(Object.setPrototypeOf(a,p)===a&&a[0]===7&&a[1]===8&&Object.getPrototypeOf(a)===p))n=1;else if(!(Object.setPrototypeOf(a,null)===a&&a[0]===7&&a[1]===8&&Object.getPrototypeOf(a)===null))n=2;function g(){\"use strict\";return 8;}var gp={x:1};if(n===0&&!(Object.setPrototypeOf(g,gp)===g&&g.x===1&&g()===8&&g.length===0&&Object.getPrototypeOf(g)===gp))n=3;var host={};if(n===0&&!(Object.setPrototypeOf(host,g)===host&&Object.getPrototypeOf(host)===g))n=4;if(n===0&&!(Object.setPrototypeOf(host,a)===host&&Object.getPrototypeOf(host)===a))n=5;var acc={set x(v){\"use strict\";this.y=v;}};var recv={};Object.setPrototypeOf(recv,acc);recv.x=7;if(n===0&&!(recv.y===7&&Object.getOwnPropertyDescriptor(recv,\"x\")===undefined))n=6;var ro={};Object.defineProperty(ro,\"z\",{value:1});Object.setPrototypeOf(recv,ro);recv.z=2;if(n===0&&!(recv.z===1&&Object.getOwnPropertyDescriptor(recv,\"z\")===undefined&&recv.y===7))n=7;n;",
    );
    assert_eq!(code, 0, "array payload, function, setter, readonly");
}

#[test]
fn object_set_prototype_invalid_eval_and_extra() {
    let primitives = case_code(
        "var n=0;var hits={n:0};var box={owner:hits,valueOf:function(){\"use strict\";this.owner.n=1;return {};},toString:function(){\"use strict\";this.owner.n=2;return \"x\";}};try{Object.setPrototypeOf(1,box);}catch(e){if(!(e.name===\"TypeError\"))n=1;}try{Object.setPrototypeOf(\"x\",box);}catch(e){if(n===0&&!(e.name===\"TypeError\"))n=2;}try{Object.setPrototypeOf(true,box);}catch(e){if(n===0&&!(e.name===\"TypeError\"))n=3;}try{Object.setPrototypeOf(undefined,box);}catch(e){if(n===0&&!(e.name===\"TypeError\"))n=4;}try{Object.setPrototypeOf(null,box);}catch(e){if(n===0&&!(e.name===\"TypeError\"))n=5;}if(n===0&&!(hits.n===0))n=6;n;",
    );
    assert_eq!(primitives, 0, "primitive targets reject without valueOf");
    let proto = case_code(
        "var n=0;var o={};var before=Object.getPrototypeOf(o);try{Object.setPrototypeOf(o,undefined);}catch(e){if(!(e.name===\"TypeError\"&&Object.getPrototypeOf(o)===before))n=1;}try{Object.setPrototypeOf(o,1);}catch(e){if(n===0&&!(e.name===\"TypeError\"&&Object.getPrototypeOf(o)===before))n=2;}try{Object.setPrototypeOf(o,\"p\");}catch(e){if(n===0&&!(e.name===\"TypeError\"&&Object.getPrototypeOf(o)===before))n=3;}try{Object.setPrototypeOf(o,true);}catch(e){if(n===0&&!(e.name===\"TypeError\"))n=4;}try{Object.setPrototypeOf();}catch(e){if(n===0&&!(e.name===\"TypeError\"))n=5;}try{Object.setPrototypeOf(o);}catch(e){if(n===0&&!(e.name===\"TypeError\"&&Object.getPrototypeOf(o)===before))n=6;}n;",
    );
    assert_eq!(proto, 0, "invalid and missing prototype");
    let order = case_code(
        "var n=0;var seen={n:0};var bad={get boom(){\"use strict\";throw 9;}};try{Object.setPrototypeOf(bad.boom,(seen.n=1));}catch(e){if(!(e===9&&seen.n===0))n=1;}var o={};var before=Object.getPrototypeOf(o);var extra={n:0,valueOf:function(){\"use strict\";this.n=1;return 5;}};var ran={n:0};if(n===0&&!(Object.setPrototypeOf(o,before,(ran.n=1,extra))===o&&ran.n===1&&extra.n===0&&Object.getPrototypeOf(o)===before))n=2;n;",
    );
    assert_eq!(order, 0, "argument order and unused extra");
}

#[test]
fn object_set_prototype_alias_delete_length_descriptor() {
    let code = case_code(
        "var d=Object.getOwnPropertyDescriptor(Object,\"setPrototypeOf\");var n=0;if(!(d.writable===true&&d.enumerable===false&&d.configurable===true&&d.value.length===2))n=1;var saved=d.value;Object.setPrototypeOf=function(){\"use strict\";return 0;};var a={};var b={x:1};if(n===0&&!(saved(a,b)===a&&a.x===1&&Object.getPrototypeOf(a)===b))n=2;delete Object.setPrototypeOf;if(n===0&&!(Object.setPrototypeOf===undefined&&saved.length===2&&saved(a,b)===a))n=3;try{var ks=Object.keys({});if(n===0&&!(ks.length===0))n=4;}catch(e){if(n===0)n=4;}if(n===0&&!(Object.getPrototypeOf(a)===b))n=5;n;",
    );
    assert_eq!(code, 0, "alias, delete, length, descriptor, keys available");
}

struct ProtoObserver {
    mode: u8,
}

fn proto_call(index: u16, args: Vec<vm::Expr>) -> vm::Expr {
    vm::Expr::Call(index, Vec::new(), args, None, None)
}

fn proto_helper(ir: &vm::FrontendIr, name: &str) -> u16 {
    ir.functions
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("missing helper {name}"))
        .index
}

fn proto_fresh(ir: &mut vm::FrontendIr) -> u16 {
    let index = u16::try_from(ir.locals).expect("observer slot");
    ir.locals += 1;
    index
}

fn proto_binding(ir: &vm::FrontendIr, name: &str) -> u16 {
    ir.local_bindings
        .iter()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("missing binding {name}"))
        .1
}

fn proto_field(index: u16, name: &str) -> vm::Expr {
    proto_call(
        vm::BuiltinFunction::Get.call_index(),
        vec![
            vm::Expr::Borrow(Box::new(vm::Expr::Var(index))),
            vm::Expr::String(name.into()),
        ],
    )
}

fn proto_undefined() -> vm::Expr {
    let mut value = proto_call(vm::BuiltinFunction::MapNew.call_index(), Vec::new());
    for (key, field) in [
        ("tag", vm::Expr::Int(0)),
        ("number", vm::Expr::Float(0.0)),
        ("flag", vm::Expr::Bool(false)),
        ("ref_slot", vm::Expr::Int(0)),
        ("generation", vm::Expr::Int(0)),
    ] {
        value = proto_call(
            vm::BuiltinFunction::Set.call_index(),
            vec![value, vm::Expr::String(key.into()), field],
        );
    }
    value
}

fn note_root(id: u16, args: &[vm::Expr], root_write: u16, found: &mut Vec<(u16, u16, i64, u16)>) {
    if id != root_write {
        return;
    }
    if let [
        vm::Expr::Var(carrier),
        vm::Expr::Var(frame),
        vm::Expr::Int(entry),
        vm::Expr::Var(payload),
    ] = args
    {
        found.push((*carrier, *frame, *entry, *payload));
    }
}

fn walk_roots(stmts: &[vm::Stmt], root_write: u16, found: &mut Vec<(u16, u16, i64, u16)>) {
    for stmt in stmts {
        match stmt {
            vm::Stmt::Let {
                expr: vm::Expr::Call(id, _, args, _, _),
                ..
            }
            | vm::Stmt::Assign {
                expr: vm::Expr::Call(id, _, args, _, _),
                ..
            } => note_root(*id, args, root_write, found),
            vm::Stmt::IfElse {
                then_branch,
                else_branch,
                ..
            } => {
                walk_roots(then_branch, root_write, found);
                walk_roots(else_branch, root_write, found);
            }
            vm::Stmt::While { body, .. } => walk_roots(body, root_write, found),
            _ => {}
        }
    }
}

fn root_entry(ir: &vm::FrontendIr, binding: u16) -> (u16, u16, i64) {
    let root_write = proto_helper(ir, "__compat_es5_root_write");
    let mut found = Vec::new();
    walk_roots(&ir.stmts, root_write, &mut found);
    for body in ir.function_impls.values() {
        walk_roots(&body.body_stmts, root_write, &mut found);
    }
    found
        .iter()
        .rev()
        .find(|(_, _, _, payload)| *payload == binding)
        .map(|(carrier, frame, entry, _)| (*carrier, *frame, *entry))
        .unwrap_or_else(|| panic!("no root entry for {binding}; writes={found:?}"))
}

fn let_int(index: u16, expr: vm::Expr) -> vm::Stmt {
    vm::Stmt::Let {
        index,
        declared_schema: Some(vm::TypeSchema::Int),
        expr,
        line: 1,
    }
}

fn clear_root(ir: &mut vm::FrontendIr, binding: u16) -> (u16, u16) {
    let (carrier, frame, entry) = root_entry(ir, binding);
    let ack = proto_fresh(ir);
    ir.stmts.push(let_int(
        ack,
        proto_call(
            proto_helper(ir, "__compat_es5_root_write"),
            vec![
                vm::Expr::Var(carrier),
                vm::Expr::Var(frame),
                vm::Expr::Int(entry),
                proto_undefined(),
            ],
        ),
    ));
    (carrier, binding)
}

fn push_expr(ir: &mut vm::FrontendIr, expr: vm::Expr) {
    ir.stmts.push(vm::Stmt::Expr { expr, line: 1 });
}

fn observe_reclaim(ir: &mut vm::FrontendIr, kept: &str, dropped: &str, kept_name: &str) {
    let dropped_slot = proto_binding(ir, dropped);
    let kept_slot = proto_binding(ir, kept);
    let anchor = proto_binding(ir, kept_name);
    let (carrier, _, _) = root_entry(ir, dropped_slot);
    let saved_drop_slot = proto_fresh(ir);
    let saved_drop_gen = proto_fresh(ir);
    let saved_kept_slot = proto_fresh(ir);
    let saved_kept_gen = proto_fresh(ir);
    let before = proto_fresh(ir);
    ir.stmts.push(let_int(
        saved_drop_slot,
        proto_field(dropped_slot, "ref_slot"),
    ));
    ir.stmts.push(let_int(
        saved_drop_gen,
        proto_field(dropped_slot, "generation"),
    ));
    ir.stmts
        .push(let_int(saved_kept_slot, proto_field(kept_slot, "ref_slot")));
    ir.stmts.push(let_int(
        saved_kept_gen,
        proto_field(kept_slot, "generation"),
    ));
    let observe = proto_helper(ir, "__compat_es5_heap_observe");
    ir.stmts.push(let_int(
        before,
        proto_call(
            observe,
            vec![vm::Expr::Var(carrier), vm::Expr::Int(5), vm::Expr::Int(0)],
        ),
    ));
    clear_root(ir, dropped_slot);
    if kept_slot != anchor {
        clear_root(ir, kept_slot);
    }
    let collect = proto_helper(ir, "__compat_es5_heap_collect");
    let ack = proto_fresh(ir);
    ir.stmts.push(let_int(
        ack,
        proto_call(collect, vec![vm::Expr::Var(carrier)]),
    ));
    push_expr(ir, vm::Expr::String("PROTO_GC".into()));
    for expr in [
        vm::Expr::Var(before),
        proto_call(
            observe,
            vec![vm::Expr::Var(carrier), vm::Expr::Int(5), vm::Expr::Int(0)],
        ),
        vm::Expr::Var(saved_kept_slot),
        vm::Expr::Var(saved_kept_gen),
        proto_call(
            observe,
            vec![
                vm::Expr::Var(carrier),
                vm::Expr::Int(13),
                vm::Expr::Var(saved_kept_slot),
            ],
        ),
        proto_call(
            observe,
            vec![
                vm::Expr::Var(carrier),
                vm::Expr::Int(14),
                vm::Expr::Var(saved_kept_slot),
            ],
        ),
        vm::Expr::Var(saved_drop_slot),
        vm::Expr::Var(saved_drop_gen),
        proto_call(
            observe,
            vec![
                vm::Expr::Var(carrier),
                vm::Expr::Int(13),
                vm::Expr::Var(saved_drop_slot),
            ],
        ),
        proto_call(
            observe,
            vec![
                vm::Expr::Var(carrier),
                vm::Expr::Int(14),
                vm::Expr::Var(saved_drop_slot),
            ],
        ),
    ] {
        push_expr(ir, expr);
    }
    let checked = proto_helper(ir, "__compat_es5_checked_base");
    let read = proto_helper(ir, "__compat_ref_read");
    let base = proto_fresh(ir);
    ir.stmts.push(let_int(
        base,
        proto_call(
            checked,
            vec![
                vm::Expr::Var(carrier),
                proto_field(anchor, "ref_slot"),
                proto_field(anchor, "generation"),
            ],
        ),
    ));
    for offset in [3, 4] {
        push_expr(
            ir,
            proto_call(
                read,
                vec![
                    vm::Expr::Var(carrier),
                    vm::Expr::Div(
                        Box::new(vm::Expr::Add(
                            Box::new(vm::Expr::Var(base)),
                            Box::new(vm::Expr::Int(offset)),
                        )),
                        Box::new(vm::Expr::Float(1.0)),
                    ),
                ],
            ),
        );
    }
}

impl vm::SourcePlugin for ProtoObserver {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut descriptor = vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN);
        descriptor.id = vm::FrontendId::new("javascript-es5-prototype-observer");
        descriptor
    }

    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
        match self.mode {
            0 => observe_reclaim(&mut ir, "fresh", "stale", "child"),
            1 => observe_reclaim(&mut ir, "holder", "gone", "holder"),
            3 | 4 => observe_string_alias(&mut ir, self.mode),
            5 => observe_case_result(&mut ir),
            _ => {
                let value = ir
                    .stmts
                    .iter()
                    .rev()
                    .find_map(|stmt| match stmt {
                        vm::Stmt::Expr {
                            expr: vm::Expr::Var(index),
                            ..
                        } => Some(*index),
                        _ => None,
                    })
                    .expect("array completion");
                let carrier = ir
                    .stmts
                    .iter()
                    .find_map(|stmt| match stmt {
                        vm::Stmt::Let {
                            index,
                            expr: vm::Expr::Call(target, _, _, _, _),
                            ..
                        } if *target == proto_helper(&ir, "__compat_ref_make") => Some(*index),
                        _ => None,
                    })
                    .expect("carrier");
                let base = proto_fresh(&mut ir);
                ir.stmts.push(let_int(
                    base,
                    proto_call(
                        proto_helper(&ir, "__compat_es5_checked_base"),
                        vec![
                            vm::Expr::Var(carrier),
                            proto_field(value, "ref_slot"),
                            proto_field(value, "generation"),
                        ],
                    ),
                ));
                push_expr(&mut ir, vm::Expr::String("PROTO_ARRAY".into()));
                let read = proto_helper(&ir, "__compat_ref_read");
                for offset in [2, 3, 4, 5, 6] {
                    push_expr(
                        &mut ir,
                        proto_call(
                            read,
                            vec![
                                vm::Expr::Var(carrier),
                                vm::Expr::Div(
                                    Box::new(vm::Expr::Add(
                                        Box::new(vm::Expr::Var(base)),
                                        Box::new(vm::Expr::Int(offset)),
                                    )),
                                    Box::new(vm::Expr::Float(1.0)),
                                ),
                            ],
                        ),
                    );
                }
            }
        }
        Ok(ir)
    }
}

fn execute_stack(source: &str, mode: u8) -> Vec<Value> {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            free_floor();
            eprintln!("PROTO_GC_SOURCE mode={mode} {source}");
            let plugin = Box::leak(Box::new(ProtoObserver { mode }));
            let catalog =
                pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
            let options = vm::CompileSourceFileOptions::new()
                .with_host_api_catalog(catalog.clone())
                .with_source_plugin(plugin);
            let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
                &source,
                vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-prototype-observer")),
                options,
            )
            .unwrap_or_else(|error| panic!("observer compile: {error:?}"));
            eprintln!("PROTO_GC_COMPILED");
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
            // The ordinary realm's bootstrap publishes its fixed ASCII strings through
            // the Core bytes initializer, so this capability is part of every emitted
            // realm program (the Object.keys harness allows the same builtin).
            registry
                .allow_builtin("bytes::from_utf8")
                .expect("bootstrap strings");
            registry
                .allow_builtin("math::is_finite")
                .expect("String adapter capability");
            registry
                .allow_builtin("math::powf")
                .expect("String adapter capability");
            registry
                .allow_builtin("math::trunc")
                .expect("String adapter capability");
            registry.bind_vm_cached(&mut machine).expect("bind");
            machine.set_fuel(10_000_000);
            let status = machine.run();
            eprintln!(
                "PROTO_GC_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack()
            );
            assert_eq!(
                status.unwrap_or_else(|error| panic!("{source}: {error:?}")),
                VmStatus::Halted
            );
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.aot_exec_count(), machine.jit_native_exec_count()),
                (0, 0)
            );
            let stack = machine.stack().to_vec();
            machine.reset_for_reuse().expect("reset");
            stack
        })
        .expect("stack")
        .join()
        .expect("test thread")
}

fn stack_i64(value: &Value) -> i64 {
    match value {
        Value::Int(number) => *number,
        Value::Float(number) => *number as i64,
        other => panic!("gc number: {other:?}"),
    }
}

fn after_marker(stack: &[Value], marker: &str) -> Vec<i64> {
    let at = stack
        .iter()
        .position(|value| matches!(value, Value::String(text) if text.as_str() == marker))
        .unwrap_or_else(|| panic!("missing {marker} in {stack:?}"));
    stack[at + 1..].iter().map(stack_i64).collect()
}

#[test]
fn object_set_prototype_collector_edges() {
    let survive = execute_stack(
        "var child={};var fresh={v:1};var stale={v:2};Object.setPrototypeOf(child,stale);Object.setPrototypeOf(child,fresh);child;",
        0,
    );
    let live = after_marker(&survive, "PROTO_GC");
    eprintln!("PROTO_GC_SURVIVE {live:?}");
    assert!(live[1] > live[0], "collection ran: {live:?}");
    assert_eq!(live[4], 2, "fresh stays active via the new edge: {live:?}");
    assert_eq!(live[5], live[3], "fresh generation unchanged: {live:?}");
    assert!(
        live[8] != 2 || live[9] != live[7],
        "stale reclaimed after the old edge dropped: {live:?}"
    );
    assert_eq!(
        (live[10], live[11]),
        (live[2], live[3]),
        "child proto pair is fresh: {live:?}"
    );
    let dropped = execute_stack(
        "var holder={};var gone={v:9};Object.setPrototypeOf(holder,gone);Object.setPrototypeOf(holder,null);holder;",
        1,
    );
    let nulls = after_marker(&dropped, "PROTO_GC");
    eprintln!("PROTO_GC_NULL {nulls:?}");
    assert!(nulls[1] > nulls[0], "null collection ran: {nulls:?}");
    assert!(
        nulls[8] != 2 || nulls[9] != nulls[7],
        "gone reclaimed: {nulls:?}"
    );
    assert_eq!((nulls[10], nulls[11]), (0, 0), "null pair: {nulls:?}");
    let array = execute_stack(
        "var items=[7,8];var proto={};Object.setPrototypeOf(items,proto);Object.setPrototypeOf(items,null);items;",
        2,
    );
    let cells = after_marker(&array, "PROTO_ARRAY");
    eprintln!("PROTO_ARRAY_CELLS {cells:?}");
    assert_eq!(cells, vec![2, 0, 0, 7, 8], "length, null pair, elements");
}

const D08_SOURCE: &str = "var o=Object.create(null,{x:{value:7}});o.x===7&&Object.getOwnPropertyDescriptor(o,\"x\").writable===false;";

struct D08Probe {
    mode: u8,
}

fn force_gc_begin(ir: &mut vm::FrontendIr) {
    let begin = proto_helper(ir, "__compat_es5_node_begin");
    let write = proto_helper(ir, "__compat_ref_write");
    let carrier = ir.function_impls[&begin].param_slots[0];
    let sink = proto_fresh(ir);
    ir.function_impls
        .get_mut(&begin)
        .expect("node begin")
        .body_stmts
        .insert(
            0,
            vm::Stmt::Let {
                index: sink,
                declared_schema: Some(vm::TypeSchema::Number),
                expr: proto_call(
                    write,
                    vec![
                        vm::Expr::Var(carrier),
                        vm::Expr::Float(10.0),
                        vm::Expr::Float(32.0),
                    ],
                ),
                line: 1,
            },
        );
}

fn bootstrap_index(ir: &vm::FrontendIr) -> usize {
    let boot = proto_helper(ir, "__compat_es5_intrinsic_bootstrap");
    ir.stmts
        .iter()
        .position(|stmt| {
            matches!(
                stmt,
                vm::Stmt::Let {
                    expr: vm::Expr::Call(id, _, _, _, _),
                    ..
                } if *id == boot
            )
        })
        .expect("bootstrap call")
}

fn bootstrap_carrier(ir: &vm::FrontendIr, index: usize) -> u16 {
    match &ir.stmts[index] {
        vm::Stmt::Let {
            expr: vm::Expr::Call(_, _, args, _, _),
            ..
        } => match args.first() {
            Some(vm::Expr::Var(slot)) => *slot,
            other => panic!("bootstrap carrier: {other:?}"),
        },
        other => panic!("bootstrap stmt: {other:?}"),
    }
}

fn observe_lets(ir: &mut vm::FrontendIr, carrier: u16, marker: i64) -> Vec<vm::Stmt> {
    let observe = proto_helper(ir, "__compat_es5_heap_observe");
    let mut slots = Vec::new();
    for _ in 0..6 {
        slots.push(proto_fresh(ir));
    }
    let selectors = [0, 5, 6, 7, 2, 3];
    let mut stmts = vec![let_int(slots[0], vm::Expr::Int(marker))];
    for (slot, selector) in slots[1..].iter().zip(selectors) {
        stmts.push(let_int(
            *slot,
            proto_call(
                observe,
                vec![
                    vm::Expr::Var(carrier),
                    vm::Expr::Int(selector),
                    vm::Expr::Int(0),
                ],
            ),
        ));
    }
    stmts
}

impl vm::SourcePlugin for D08Probe {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut descriptor = vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN);
        descriptor.id = vm::FrontendId::new("javascript-es5-d08-fuel-probe");
        descriptor
    }

    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
        if self.mode == 0 {
            let names = ir
                .functions
                .iter()
                .map(|function| format!("{}={}", function.index, function.name))
                .collect::<Vec<_>>()
                .join(",");
            eprintln!("D08_FUNCS {names}");
        }
        force_gc_begin(&mut ir);
        match self.mode {
            1 => {
                let index = bootstrap_index(&ir);
                let carrier = bootstrap_carrier(&ir, index);
                let rest = ir.stmts.split_off(index + 1);
                ir.stmts.push(vm::Stmt::Expr {
                    expr: vm::Expr::String("D08_BOOT".into()),
                    line: 1,
                });
                let observe = proto_helper(&ir, "__compat_es5_heap_observe");
                for selector in [0, 5, 6, 7, 2, 3, 12] {
                    push_expr(
                        &mut ir,
                        proto_call(
                            observe,
                            vec![
                                vm::Expr::Var(carrier),
                                vm::Expr::Int(selector),
                                vm::Expr::Int(0),
                            ],
                        ),
                    );
                }
                ir.stmts.push(vm::Stmt::IfElse {
                    condition: vm::Expr::Eq(Box::new(vm::Expr::Int(0)), Box::new(vm::Expr::Int(1))),
                    then_branch: rest,
                    else_branch: Vec::new(),
                    line: 1,
                });
            }
            2 => {
                let index = bootstrap_index(&ir);
                let script_carrier = bootstrap_carrier(&ir, index);
                let script_probe = observe_lets(&mut ir, script_carrier, 900100);
                let at = index + 1;
                ir.stmts.splice(at..at, script_probe);
                let collect = proto_helper(&ir, "__compat_es5_heap_collect");
                let collect_carrier = ir.function_impls[&collect].param_slots[0];
                let collect_probe = observe_lets(&mut ir, collect_carrier, 424242);
                ir.function_impls
                    .get_mut(&collect)
                    .expect("collect")
                    .body_stmts
                    .splice(0..0, collect_probe);
            }
            _ => {}
        }
        Ok(ir)
    }
}

fn ints_after(locals: &[Value], marker: i64) -> Option<(usize, Vec<i64>)> {
    let index = locals
        .iter()
        .position(|value| value == &Value::Int(marker))?;
    let window = locals
        .iter()
        .skip(index)
        .take(6)
        .map(|value| match value {
            Value::Int(n) => *n,
            Value::Float(n) => *n as i64,
            other => panic!("probe window {marker} {other:?}"),
        })
        .collect();
    Some((index, window))
}

fn run_d08(mode: u8) -> (VmStatus, Option<u64>, Vec<Value>) {
    run_d08_source(mode, D08_SOURCE)
}

fn run_d08_source(mode: u8, source: &str) -> (VmStatus, Option<u64>, Vec<Value>) {
    free_floor();
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let plugin = Box::leak(Box::new(D08Probe { mode }));
    let options = vm::CompileSourceFileOptions::new()
        .with_host_api_catalog(catalog.clone())
        .with_source_plugin(&*plugin);
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-d08-fuel-probe")),
        options,
    )
    .unwrap_or_else(|error| panic!("d08 mode {mode} compile: {error:?}"));
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
            .expect("catalog");
    }
    registry.allow_builtin("math::floor").expect("floor");
    // The ordinary realm's bootstrap publishes its fixed ASCII strings through
    // the Core bytes initializer, so this capability is part of every emitted
    // realm program (the Object.keys harness allows the same builtin).
    registry
        .allow_builtin("bytes::from_utf8")
        .expect("bootstrap strings");
    registry.bind_vm_cached(&mut machine).expect("bind");
    machine.set_fuel(10_000_000);
    let before = machine.get_fuel();
    let status = machine
        .run()
        .unwrap_or_else(|error| panic!("d08 mode {mode}: {error:?}"));
    let after = machine.get_fuel();
    let frames: Vec<_> = machine
        .execution_frames()
        .iter()
        .map(|frame| {
            let label = frame.prototype_id.and_then(|id| {
                machine
                    .program()
                    .callable_prototypes
                    .get(id as usize)
                    .map(|prototype| {
                        let text = format!("{:?}", prototype.target);
                        text.chars().take(90).collect::<String>()
                    })
            });
            (
                frame.prototype_id,
                frame.local_base,
                frame.local_count,
                label,
            )
        })
        .collect();
    let boot = ints_after(machine.locals(), 900100);
    let collect = ints_after(machine.locals(), 424242);
    eprintln!(
        "D08_PROBE mode={mode} status={status:?} yield={:?} ip={} fuel_before={before:?} fuel_after={after:?} frames={frames:?} boot={boot:?} collect={collect:?} stack={:?}",
        machine.last_yield_reason(),
        machine.ip(),
        machine.stack()
    );
    assert!(!machine.jit_config().enabled, "diagnostic jit");
    assert_eq!(
        (machine.aot_exec_count(), machine.jit_native_exec_count()),
        (0, 0)
    );
    assert!(matches!(before, Some(fuel) if fuel <= 10_000_000));
    assert!(matches!(after, Some(fuel) if fuel <= 10_000_000));
    let stack = machine.stack().to_vec();
    machine.reset_for_reuse().expect("reset");
    assert!(!machine.jit_config().enabled);
    assert_eq!(
        (machine.aot_exec_count(), machine.jit_native_exec_count()),
        (0, 0)
    );
    (status, after, stack)
}

#[test]
fn d08_forced_gc_fuel_probe() {
    let (clean, boot, probed, create, read) = std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(|| {
            let clean = run_d08(0);
            let boot = run_d08(1);
            let probed = run_d08(2);
            let create = run_d08_source(0, "var o=Object.create(null,{x:{value:7}});1;");
            let read = run_d08_source(0, "var o=Object.create(null,{x:{value:7}});o.x===7;1;");
            (clean, boot, probed, create, read)
        })
        .expect("stack")
        .join()
        .expect("probe thread");
    eprintln!(
        "D08_PROBE_DONE clean={:?} boot_status={:?} boot_fuel={:?} probed_status={:?} probed_fuel={:?} create={:?}/{:?} read={:?}/{:?}",
        clean.0, boot.0, boot.1, probed.0, probed.1, create.0, create.1, read.0, read.1
    );
    assert!(
        boot.2
            .iter()
            .any(|value| value == &Value::string("D08_BOOT")),
        "bootstrap projection did not complete"
    );
}

#[test]
fn object_set_prototype_strict_readonly_inherited_data() {
    let code = case_code(
        "\"use strict\";var n=0;var recv={};var ro={};Object.defineProperty(ro,\"z\",{value:1});Object.setPrototypeOf(recv,ro);try{recv.z=2;n=1;}catch(e){if(!(e.name===\"TypeError\"&&recv.z===1))n=2;}n;",
    );
    assert_eq!(code, 0, "strict inherited readonly assignment throws");
}

fn let_num(index: u16, expr: vm::Expr) -> vm::Stmt {
    vm::Stmt::Let {
        index,
        declared_schema: Some(vm::TypeSchema::Number),
        expr,
        line: 1,
    }
}

fn assign_num(index: u16, expr: vm::Expr) -> vm::Stmt {
    vm::Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index,
        expr,
        line: 1,
    }
}

fn expr_stmt(expr: vm::Expr) -> vm::Stmt {
    vm::Stmt::Expr { expr, line: 1 }
}

fn number_index(base: u16, offset: i64) -> vm::Expr {
    vm::Expr::Div(
        Box::new(vm::Expr::Add(
            Box::new(vm::Expr::Var(base)),
            Box::new(vm::Expr::Int(offset)),
        )),
        Box::new(vm::Expr::Float(1.0)),
    )
}

fn ref_at(read: u16, carrier: u16, index: vm::Expr) -> vm::Expr {
    proto_call(read, vec![vm::Expr::Var(carrier), index])
}

fn expr_has_throw_text(expr: &vm::Expr) -> bool {
    match expr {
        vm::Expr::String(text) => text == "uncaught JavaScript throw",
        vm::Expr::Call(_, _, args, _, _) => args.iter().any(expr_has_throw_text),
        _ => false,
    }
}

fn stmt_has_throw_text(stmt: &vm::Stmt) -> bool {
    match stmt {
        vm::Stmt::Let { expr, .. }
        | vm::Stmt::Assign { expr, .. }
        | vm::Stmt::Expr { expr, .. } => expr_has_throw_text(expr),
        vm::Stmt::IfElse {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            expr_has_throw_text(condition)
                || then_branch.iter().any(stmt_has_throw_text)
                || else_branch.iter().any(stmt_has_throw_text)
        }
        vm::Stmt::While {
            condition, body, ..
        } => expr_has_throw_text(condition) || body.iter().any(stmt_has_throw_text),
        vm::Stmt::For {
            init,
            condition,
            post,
            body,
            ..
        } => {
            stmt_has_throw_text(init)
                || expr_has_throw_text(condition)
                || stmt_has_throw_text(post)
                || body.iter().any(stmt_has_throw_text)
        }
        _ => false,
    }
}

fn walk_script_writes(stmts: &[vm::Stmt], root_write: u16, found: &mut Vec<(u16, u16, i64, u16)>) {
    for stmt in stmts {
        match stmt {
            vm::Stmt::Let {
                expr: vm::Expr::Call(id, _, args, _, _),
                ..
            }
            | vm::Stmt::Assign {
                expr: vm::Expr::Call(id, _, args, _, _),
                ..
            } => note_root(*id, args, root_write, found),
            vm::Stmt::IfElse {
                then_branch,
                else_branch,
                ..
            } => {
                walk_script_writes(then_branch, root_write, found);
                walk_script_writes(else_branch, root_write, found);
            }
            vm::Stmt::While { body, .. } => walk_script_writes(body, root_write, found),
            vm::Stmt::For {
                init, post, body, ..
            } => {
                walk_script_writes(std::slice::from_ref(init.as_ref()), root_write, found);
                walk_script_writes(std::slice::from_ref(post.as_ref()), root_write, found);
                walk_script_writes(body, root_write, found);
            }
            _ => {}
        }
    }
}

fn script_frame(ir: &vm::FrontendIr) -> (u16, u16, i64) {
    let enter = proto_helper(ir, "__compat_es5_root_enter");
    let mut found = Vec::new();
    for stmt in &ir.stmts {
        if let vm::Stmt::Let {
            index,
            expr: vm::Expr::Call(id, _, args, _, _),
            ..
        } = stmt
        {
            if *id == enter {
                found.push((*index, args.clone()));
            }
        }
    }
    if found.len() != 1 {
        panic!(
            "script root_enter count {} ; expected the one frame whose leave belongs to teardown",
            found.len()
        );
    }
    match found[0].1.as_slice() {
        [vm::Expr::Var(carrier), vm::Expr::Int(capacity)] if *capacity > 0 && *capacity <= 256 => {
            (found[0].0, *carrier, *capacity)
        }
        [vm::Expr::Var(_), vm::Expr::Int(capacity)] => {
            panic!("script root capacity {capacity} outside 1..=256")
        }
        other => panic!("script root_enter args {other:?}"),
    }
}

fn script_root_entry(ir: &vm::FrontendIr, frame: u16, binding: u16) -> i64 {
    let root_write = proto_helper(ir, "__compat_es5_root_write");
    let mut found = Vec::new();
    walk_script_writes(&ir.stmts, root_write, &mut found);
    found
        .iter()
        .rev()
        .find(|(_, write_frame, _, payload)| *write_frame == frame && *payload == binding)
        .map(|(_, _, entry, _)| *entry)
        .unwrap_or_else(|| {
            panic!("no script-frame root_write for binding {binding} on frame {frame}")
        })
}

fn load_entry(
    stmts: &mut Vec<vm::Stmt>,
    read: u16,
    carrier: u16,
    frame: u16,
    entry: i64,
    cat: u16,
    slot_cell: u16,
    gen_cell: u16,
) {
    let base = 4 + entry * 3;
    stmts.push(assign_num(
        cat,
        ref_at(read, carrier, number_index(frame, base)),
    ));
    stmts.push(assign_num(
        slot_cell,
        ref_at(read, carrier, number_index(frame, base + 1)),
    ));
    stmts.push(assign_num(
        gen_cell,
        ref_at(read, carrier, number_index(frame, base + 2)),
    ));
}

fn push_entry(stmts: &mut Vec<vm::Stmt>, cat: u16, slot_cell: u16, gen_cell: u16) {
    stmts.push(expr_stmt(vm::Expr::Var(cat)));
    stmts.push(expr_stmt(vm::Expr::Var(slot_cell)));
    stmts.push(expr_stmt(vm::Expr::Var(gen_cell)));
}

fn when_same_node(
    slot_cell: u16,
    gen_cell: u16,
    want_slot: u16,
    want_gen: u16,
    body: Vec<vm::Stmt>,
) -> vm::Stmt {
    vm::Stmt::IfElse {
        condition: vm::Expr::Eq(
            Box::new(vm::Expr::Var(slot_cell)),
            Box::new(vm::Expr::Var(want_slot)),
        ),
        then_branch: vec![vm::Stmt::IfElse {
            condition: vm::Expr::Eq(
                Box::new(vm::Expr::Var(gen_cell)),
                Box::new(vm::Expr::Var(want_gen)),
            ),
            then_branch: body,
            else_branch: Vec::new(),
            line: 1,
        }],
        else_branch: Vec::new(),
        line: 1,
    }
}

fn observe_string_alias(ir: &mut vm::FrontendIr, mode: u8) {
    let retain = match mode {
        3 => true,
        4 => false,
        other => panic!("string alias mode {other}"),
    };
    let (frame, carrier, capacity) = script_frame(ir);
    let src = proto_binding(ir, "src");
    let keep = proto_binding(ir, "keep");
    let dst = proto_binding(ir, "dst");
    let src_entry = script_root_entry(ir, frame, src);
    let keep_entry = script_root_entry(ir, frame, keep);
    let dst_entry = script_root_entry(ir, frame, dst);
    if src_entry == keep_entry || src_entry == dst_entry || keep_entry == dst_entry {
        panic!(
            "alias bindings share a root entry src={src_entry} keep={keep_entry} dst={dst_entry}"
        );
    }
    let guards = ir
        .stmts
        .iter()
        .enumerate()
        .filter(|(_, stmt)| stmt_has_throw_text(stmt))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if guards.len() != 1 {
        panic!(
            "uncaught-guard count {} in script stmts; publication seam is not unique",
            guards.len()
        );
    }
    let insert_at = guards[0] + 1;
    let read = proto_helper(ir, "__compat_ref_read");
    let clear = proto_helper(ir, "__compat_es5_root_clear");
    let observe = proto_helper(ir, "__compat_es5_heap_observe");
    let collect = proto_helper(ir, "__compat_es5_heap_collect");
    let checked = proto_helper(ir, "__compat_es5_checked_base");
    let mut slots = [0u16; 3];
    let mut gens = [0u16; 3];
    let mut stmts = Vec::new();
    for (index, binding) in [src, keep, dst].into_iter().enumerate() {
        slots[index] = proto_fresh(ir);
        gens[index] = proto_fresh(ir);
        stmts.push(let_int(slots[index], proto_field(binding, "ref_slot")));
        stmts.push(let_int(gens[index], proto_field(binding, "generation")));
    }
    let dst_slot = slots[2];
    let dst_gen = gens[2];
    let cat = proto_fresh(ir);
    let slot_cell = proto_fresh(ir);
    let gen_cell = proto_fresh(ir);
    let want_slot = proto_fresh(ir);
    let want_gen = proto_fresh(ir);
    let cap_cell = proto_fresh(ir);
    let cleared = proto_fresh(ir);
    let remain = proto_fresh(ir);
    stmts.push(let_num(cat, vm::Expr::Float(0.0)));
    stmts.push(let_num(slot_cell, vm::Expr::Float(0.0)));
    stmts.push(let_num(gen_cell, vm::Expr::Float(0.0)));
    stmts.push(let_num(
        want_slot,
        vm::Expr::Div(
            Box::new(vm::Expr::Var(dst_slot)),
            Box::new(vm::Expr::Float(1.0)),
        ),
    ));
    stmts.push(let_num(
        want_gen,
        vm::Expr::Div(
            Box::new(vm::Expr::Var(dst_gen)),
            Box::new(vm::Expr::Float(1.0)),
        ),
    ));
    stmts.push(let_num(cap_cell, vm::Expr::Float(0.0)));
    stmts.push(let_int(cleared, vm::Expr::Int(0)));
    stmts.push(let_int(remain, vm::Expr::Int(0)));
    stmts.push(expr_stmt(vm::Expr::String("ALIAS_GC".into())));
    for (index, binding) in [src, keep, dst].into_iter().enumerate() {
        for name in ["tag", "number", "flag"] {
            stmts.push(expr_stmt(proto_field(binding, name)));
        }
        stmts.push(expr_stmt(vm::Expr::Var(slots[index])));
        stmts.push(expr_stmt(vm::Expr::Var(gens[index])));
    }
    for entry in [src_entry, keep_entry, dst_entry] {
        load_entry(
            &mut stmts, read, carrier, frame, entry, cat, slot_cell, gen_cell,
        );
        push_entry(&mut stmts, cat, slot_cell, gen_cell);
    }
    stmts.push(assign_num(
        cap_cell,
        ref_at(read, carrier, number_index(frame, 3)),
    ));
    stmts.push(expr_stmt(vm::Expr::Var(cap_cell)));
    stmts.push(expr_stmt(vm::Expr::Int(capacity)));
    stmts.push(expr_stmt(vm::Expr::Int(dst_entry)));
    stmts.push(expr_stmt(vm::Expr::Int(src_entry)));
    stmts.push(expr_stmt(vm::Expr::Int(keep_entry)));
    for entry in 0..capacity {
        load_entry(
            &mut stmts, read, carrier, frame, entry, cat, slot_cell, gen_cell,
        );
        let mut body = Vec::new();
        if !(retain && entry == dst_entry) {
            let ack = proto_fresh(ir);
            body.push(let_int(
                ack,
                proto_call(
                    clear,
                    vec![
                        vm::Expr::Var(carrier),
                        vm::Expr::Var(frame),
                        vm::Expr::Int(entry),
                    ],
                ),
            ));
            body.push(assign_num(
                cleared,
                vm::Expr::Add(Box::new(vm::Expr::Var(cleared)), Box::new(vm::Expr::Int(1))),
            ));
        }
        stmts.push(when_same_node(
            slot_cell, gen_cell, want_slot, want_gen, body,
        ));
    }
    let before = proto_fresh(ir);
    stmts.push(let_int(
        before,
        proto_call(
            observe,
            vec![vm::Expr::Var(carrier), vm::Expr::Int(5), vm::Expr::Int(0)],
        ),
    ));
    let ack = proto_fresh(ir);
    stmts.push(let_int(
        ack,
        proto_call(collect, vec![vm::Expr::Var(carrier)]),
    ));
    for entry in 0..capacity {
        load_entry(
            &mut stmts, read, carrier, frame, entry, cat, slot_cell, gen_cell,
        );
        stmts.push(when_same_node(
            slot_cell,
            gen_cell,
            want_slot,
            want_gen,
            vec![assign_num(
                remain,
                vm::Expr::Add(Box::new(vm::Expr::Var(remain)), Box::new(vm::Expr::Int(1))),
            )],
        ));
    }
    for entry in [src_entry, keep_entry, dst_entry] {
        load_entry(
            &mut stmts, read, carrier, frame, entry, cat, slot_cell, gen_cell,
        );
        push_entry(&mut stmts, cat, slot_cell, gen_cell);
    }
    stmts.push(expr_stmt(vm::Expr::Var(cleared)));
    stmts.push(expr_stmt(vm::Expr::Var(remain)));
    stmts.push(expr_stmt(vm::Expr::Var(before)));
    stmts.push(expr_stmt(proto_call(
        observe,
        vec![vm::Expr::Var(carrier), vm::Expr::Int(5), vm::Expr::Int(0)],
    )));
    stmts.push(expr_stmt(proto_call(
        observe,
        vec![
            vm::Expr::Var(carrier),
            vm::Expr::Int(13),
            vm::Expr::Var(dst_slot),
        ],
    )));
    stmts.push(expr_stmt(proto_call(
        observe,
        vec![
            vm::Expr::Var(carrier),
            vm::Expr::Int(14),
            vm::Expr::Var(dst_slot),
        ],
    )));
    if retain {
        let base = proto_fresh(ir);
        stmts.push(let_int(
            base,
            proto_call(
                checked,
                vec![
                    vm::Expr::Var(carrier),
                    vm::Expr::Var(dst_slot),
                    vm::Expr::Var(dst_gen),
                ],
            ),
        ));
        for offset in [0, 2, 3, 4] {
            stmts.push(expr_stmt(ref_at(read, carrier, number_index(base, offset))));
        }
    }
    let tail = ir.stmts.split_off(insert_at);
    ir.stmts.extend(stmts);
    ir.stmts.extend(tail);
}

fn observe_case_result(ir: &mut vm::FrontendIr) {
    let (frame, carrier, capacity) = script_frame(ir);
    let src = proto_binding(ir, "src");
    let keep = proto_binding(ir, "keep");
    let dst = proto_binding(ir, "dst");
    let src_entry = script_root_entry(ir, frame, src);
    let keep_entry = script_root_entry(ir, frame, keep);
    let dst_entry = script_root_entry(ir, frame, dst);
    let guards = ir
        .stmts
        .iter()
        .enumerate()
        .filter(|(_, stmt)| stmt_has_throw_text(stmt))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if guards.len() != 1 {
        panic!("case result publication seam is not unique");
    }
    let insert_at = guards[0] + 1;
    let read = proto_helper(ir, "__compat_ref_read");
    let clear = proto_helper(ir, "__compat_es5_root_clear");
    let observe = proto_helper(ir, "__compat_es5_heap_observe");
    let collect = proto_helper(ir, "__compat_es5_heap_collect");
    let checked = proto_helper(ir, "__compat_es5_checked_base");
    let src_slot = proto_fresh(ir);
    let src_gen = proto_fresh(ir);
    let dst_slot = proto_fresh(ir);
    let dst_gen = proto_fresh(ir);
    let cat = proto_fresh(ir);
    let slot_cell = proto_fresh(ir);
    let gen_cell = proto_fresh(ir);
    let want_slot = proto_fresh(ir);
    let want_gen = proto_fresh(ir);
    let cleared = proto_fresh(ir);
    let before = proto_fresh(ir);
    let base = proto_fresh(ir);
    let mut stmts = vec![
        expr_stmt(vm::Expr::String("CASE_GC".into())),
        let_int(src_slot, proto_field(src, "ref_slot")),
        let_int(src_gen, proto_field(src, "generation")),
        let_int(dst_slot, proto_field(dst, "ref_slot")),
        let_int(dst_gen, proto_field(dst, "generation")),
        expr_stmt(vm::Expr::Var(src_slot)),
        expr_stmt(vm::Expr::Var(dst_slot)),
        let_num(cat, vm::Expr::Float(0.0)),
        let_num(slot_cell, vm::Expr::Float(0.0)),
        let_num(gen_cell, vm::Expr::Float(0.0)),
        let_num(
            want_slot,
            vm::Expr::Div(Box::new(vm::Expr::Var(src_slot)), Box::new(vm::Expr::Float(1.0))),
        ),
        let_num(
            want_gen,
            vm::Expr::Div(Box::new(vm::Expr::Var(src_gen)), Box::new(vm::Expr::Float(1.0))),
        ),
        let_int(cleared, vm::Expr::Int(0)),
    ];
    for entry in 0..capacity {
        load_entry(&mut stmts, read, carrier, frame, entry, cat, slot_cell, gen_cell);
        let mut body = Vec::new();
        if entry != dst_entry {
            let ack = proto_fresh(ir);
            body.push(let_int(
                ack,
                proto_call(
                    clear,
                    vec![
                        vm::Expr::Var(carrier),
                        vm::Expr::Var(frame),
                        vm::Expr::Int(entry),
                    ],
                ),
            ));
            body.push(assign_num(
                cleared,
                vm::Expr::Add(Box::new(vm::Expr::Var(cleared)), Box::new(vm::Expr::Int(1))),
            ));
        }
        stmts.push(when_same_node(slot_cell, gen_cell, want_slot, want_gen, body));
    }
    let _ = (src_entry, keep_entry);
    stmts.push(let_int(
        before,
        proto_call(observe, vec![vm::Expr::Var(carrier), vm::Expr::Int(5), vm::Expr::Int(0)]),
    ));
    stmts.push(let_int(proto_fresh(ir), proto_call(collect, vec![vm::Expr::Var(carrier)])));
    stmts.push(expr_stmt(vm::Expr::Var(cleared)));
    stmts.push(expr_stmt(vm::Expr::Var(before)));
    stmts.push(expr_stmt(proto_call(
        observe,
        vec![vm::Expr::Var(carrier), vm::Expr::Int(5), vm::Expr::Int(0)],
    )));
    stmts.push(expr_stmt(proto_call(
        observe,
        vec![vm::Expr::Var(carrier), vm::Expr::Int(13), vm::Expr::Var(dst_slot)],
    )));
    stmts.push(let_int(
        base,
        proto_call(
            checked,
            vec![vm::Expr::Var(carrier), vm::Expr::Var(dst_slot), vm::Expr::Var(dst_gen)],
        ),
    ));
    let read_cell = read;
    for offset in [0, 2, 3, 4] {
        stmts.push(expr_stmt(ref_at(read_cell, carrier, number_index(base, offset))));
    }
    let tail = ir.stmts.split_off(insert_at);
    ir.stmts.extend(stmts);
    ir.stmts.extend(tail);
}

fn alias_tail(stack: &[Value]) -> &[Value] {
    let at = stack
        .iter()
        .position(|value| matches!(value, Value::String(text) if text.as_str() == "ALIAS_GC"))
        .unwrap_or_else(|| panic!("missing ALIAS_GC in {stack:?}"));
    assert_eq!(
        stack
            .iter()
            .filter(|value| matches!(value, Value::String(text) if text.as_str() == "ALIAS_GC"))
            .count(),
        1,
        "ALIAS_GC injected more than once"
    );
    &stack[at + 1..]
}

fn assert_int(value: &Value, expected: i64, what: &str) {
    match value {
        Value::Int(number) if *number == expected => {}
        other => panic!("{what}: expected Int({expected}), got {other:?}"),
    }
}

fn assert_int_at_least(value: &Value, minimum: i64, what: &str) -> i64 {
    match value {
        Value::Int(number) if *number >= minimum => *number,
        other => panic!("{what}: expected Int >= {minimum}, got {other:?}"),
    }
}

fn assert_float(value: &Value, expected: f64, what: &str) {
    match value {
        Value::Float(number) if *number == expected => {}
        other => panic!("{what}: expected Float({expected}), got {other:?}"),
    }
}

fn assert_alias_proof(stack: &[Value], retain: bool) {
    let tail = alias_tail(stack);
    let expect = if retain { 48 } else { 44 };
    assert_eq!(
        tail.len(),
        expect,
        "alias evidence len {expect}, got {tail:?}"
    );
    let src_slot = match tail[3] {
        Value::Int(number) if number > 0 => number,
        ref other => panic!("src ref_slot: {other:?}"),
    };
    let src_gen = match tail[4] {
        Value::Int(number) if number > 0 => number,
        ref other => panic!("src generation: {other:?}"),
    };
    for (at, name) in [(0, "src"), (5, "keep"), (10, "dst")] {
        assert_int(&tail[at], 4, &format!("{name} tag"));
        assert_float(&tail[at + 1], 0.0, &format!("{name} number"));
        match &tail[at + 2] {
            Value::Bool(false) => {}
            other => panic!("{name} flag: expected Bool(false), got {other:?}"),
        }
        assert_int(&tail[at + 3], src_slot, &format!("{name} ref_slot"));
        assert_int(&tail[at + 4], src_gen, &format!("{name} generation"));
    }
    for (at, name) in [(15, "src"), (18, "keep"), (21, "dst")] {
        assert_float(&tail[at], 4.0, &format!("pre {name} category"));
        assert_float(
            &tail[at + 1],
            src_slot as f64,
            &format!("pre {name} slot cell"),
        );
        assert_float(
            &tail[at + 2],
            src_gen as f64,
            &format!("pre {name} generation cell"),
        );
    }
    let static_cap = match tail[25] {
        Value::Int(number) if number > 0 => number,
        ref other => panic!("static capacity: {other:?}"),
    };
    assert_float(&tail[24], static_cap as f64, "runtime frame capacity");
    let dst_entry = match tail[26] {
        Value::Int(number) if number >= 0 && number < static_cap => number,
        ref other => panic!("dst entry: {other:?}"),
    };
    let src_entry = match tail[27] {
        Value::Int(number) if number >= 0 && number < static_cap => number,
        ref other => panic!("src entry: {other:?}"),
    };
    let keep_entry = match tail[28] {
        Value::Int(number) if number >= 0 && number < static_cap => number,
        ref other => panic!("keep entry: {other:?}"),
    };
    assert!(
        src_entry != keep_entry && src_entry != dst_entry && keep_entry != dst_entry,
        "root entries not distinct: src={src_entry} keep={keep_entry} dst={dst_entry}"
    );
    for (at, name) in [(29, "src"), (32, "keep")] {
        assert_float(&tail[at], 0.0, &format!("post {name} category"));
        assert_float(&tail[at + 1], 0.0, &format!("post {name} slot cell"));
        assert_float(&tail[at + 2], 0.0, &format!("post {name} generation cell"));
    }
    if retain {
        assert_float(&tail[35], 4.0, "post dst category");
        assert_float(&tail[36], src_slot as f64, "post dst slot cell");
        assert_float(&tail[37], src_gen as f64, "post dst generation cell");
    } else {
        assert_float(&tail[35], 0.0, "post dst category");
        assert_float(&tail[36], 0.0, "post dst slot cell");
        assert_float(&tail[37], 0.0, "post dst generation cell");
    }
    let cleared_min = if retain { 2 } else { 3 };
    assert_int_at_least(&tail[38], cleared_min, "cleared same-node entries");
    assert_int(
        &tail[39],
        if retain { 1 } else { 0 },
        "remaining same-node entries",
    );
    let before = match tail[40] {
        Value::Int(number) if number >= 0 => number,
        ref other => panic!("collections before: {other:?}"),
    };
    assert_int(
        &tail[41],
        before + 1,
        "collections after one forced collect",
    );
    if retain {
        assert_int(&tail[42], 2, "selector 13 active");
        assert_int(&tail[43], src_gen, "selector 14 generation");
        assert_float(&tail[44], 2.0, "string kind at offset 0");
        assert_float(&tail[45], 2.0, "string length at offset 2");
        assert_float(&tail[46], 65.0, "unit 0x41 at offset 3");
        assert_float(&tail[47], 90.0, "unit 0x5A at offset 4");
    } else {
        assert_int(
            &tail[42],
            3,
            "selector 13 free after the sole roots were cleared",
        );
        match tail[43] {
            Value::Int(_) => {}
            ref other => {
                panic!("selector 14 should still be the Int row generation, got {other:?}")
            }
        }
    }
}

#[test]
fn es5_string_concat_zero_arg_alias_survives_single_dst_root() {
    let stack = execute_stack(
        "var src=\"A\".concat(\"Z\");var keep=src;var dst=src.concat();dst;",
        3,
    );
    assert_alias_proof(&stack, true);
}

#[test]
fn es5_string_concat_zero_arg_alias_dies_without_roots() {
    let stack = execute_stack(
        "var src=\"A\".concat(\"Z\");var keep=src;var dst=src.concat();dst;",
        4,
    );
    assert_alias_proof(&stack, false);
}

#[test]
fn es5_string_to_string_alias_survives_single_dst_root() {
    let stack = execute_stack(
        "var src=\"A\".concat(\"Z\");var keep=src;var dst=src.toString();dst;",
        3,
    );
    assert_alias_proof(&stack, true);
}

#[test]
fn es5_string_case_result_survives_after_source_roots_drop() {
    let stack = execute_stack(
        "var src=\"Az\";var keep=src;var dst=src.toLowerCase();dst;",
        5,
    );
    let at = stack
        .iter()
        .position(|value| matches!(value, Value::String(text) if text.as_str() == "CASE_GC"))
        .unwrap_or_else(|| panic!("missing CASE_GC in {stack:?}"));
    let tail = &stack[at + 1..];
    assert!(tail.len() >= 10, "case evidence {tail:?}");
    let src_slot = match tail[0] {
        Value::Int(number) if number > 0 => number,
        ref other => panic!("src slot {other:?}"),
    };
    let dst_slot = match tail[1] {
        Value::Int(number) if number > 0 => number,
        ref other => panic!("dst slot {other:?}"),
    };
    assert_ne!(src_slot, dst_slot, "case result must be a new string node");
    let cleared = match tail[2] {
        Value::Int(number) if number >= 2 => number,
        ref other => panic!("cleared source roots {other:?}"),
    };
    let before = match tail[3] {
        Value::Int(number) => number,
        ref other => panic!("collections before {other:?}"),
    };
    let after = match tail[4] {
        Value::Int(number) => number,
        ref other => panic!("collections after {other:?}"),
    };
    assert_eq!(after, before + 1, "collection counter must increase; cleared={cleared}");
    match tail[5] {
        Value::Int(2) => {}
        ref other => panic!("selector 13 must stay active, not free: {other:?}"),
    }
    assert_eq!(
        &tail[6..10],
        &[
            Value::Float(2.0),
            Value::Float(2.0),
            Value::Float(97.0),
            Value::Float(122.0),
        ],
        "surviving case result kind/length/units"
    );
}
