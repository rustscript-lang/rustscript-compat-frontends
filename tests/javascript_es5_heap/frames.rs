//! Root-only history uses the actual compiled strict function returning7.
//! No node allocation in its body can mask a lost frame by triggering GC.
use super::*;
struct PrimitiveHistoryPlugin {
    iterations: i64,
}
impl vm::SourcePlugin for PrimitiveHistoryPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-primitive-history"),
            extensions: &["js"],
            typing_policy: vm::FrontendTypingPolicy::Strict,
            supports_ownership_checking: true,
        }
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        let mut ctx = heap_cycle_ctx(&ir)?;
        ctx.carrier = heap_cycle_carrier(&ir)?;
        let binding = heap_binding_index(&ir, "o")?;
        let root = heap_helper_index(&ir, HEAP_ROOT_WRITE_NAME)?;
        let sites = pending_store_site::sites(&ir, binding, root)
            .map_err(|message| heap_rejection(message.into()))?;
        let site = pending_store_site::unique(&sites)
            .ok_or_else(|| heap_rejection("actual primitive result".to_owned()))?;
        let (run_start, run_end) = heap_guarded_primitive_replay(&ir, site)?;
        // Clone the real whole top-level span: call guard, the one common
        // callee-root retirement and binding store. The immediate handoff
        // publishes no result root, so no result clear follows. Never
        // flatten guarded locals.
        let mut replay = ir.stmts[run_start..run_end].to_vec();
        for stmt in &mut replay {
            heap_replay_assign(stmt);
        }
        let counter = heap_cycle_take(&mut ir, "primitive-history")?;
        ir.stmts.push(vm::Stmt::Let {
            index: counter,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: vm::Expr::Int(0),
            line: 1,
        });
        replay.push(vm::Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: counter,
            expr: vm::Expr::Add(Box::new(vm::Expr::Var(counter)), Box::new(vm::Expr::Int(1))),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::While {
            condition: vm::Expr::Lt(
                Box::new(vm::Expr::Var(counter)),
                Box::new(vm::Expr::Int(self.iterations)),
            ),
            body: replay,
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(binding),
            line: 1,
        });
        for which in [0, 2, 3, 4, 5] {
            heap_cycle_observe(&mut ir, &ctx, ctx.carrier, which, 0, "primitive-history")?;
        }
        Ok(ir)
    }
}

/// Independent pure-IR witness of the real guarded dispatch call. The store
/// selector never proves its own correctness: this walk requires the same
/// normal guard, the unique four-argument dispatch, its real empty ArrayNew
/// actuals and the unique ResultMatch of that raw result.
fn primitive_call_guard(
    ir: &vm::FrontendIr,
    before: usize,
    normal: u16,
    result: u16,
    dispatch: u16,
) -> Option<usize> {
    let mut found = None;
    for (at, stmt) in ir.stmts[..before].iter().enumerate() {
        let vm::Stmt::IfElse {
            condition: vm::Expr::Var(slot),
            then_branch,
            else_branch,
            ..
        } = stmt
        else {
            continue;
        };
        if *slot != normal || !else_branch.is_empty() {
            continue;
        }
        let mut dispatched = None;
        for (inner_at, inner) in then_branch.iter().enumerate() {
            let vm::Stmt::Let {
                index: dispatch_result,
                expr: vm::Expr::Call(target, types, args, None, None),
                ..
            } = inner
            else {
                continue;
            };
            if *target != dispatch || !types.is_empty() || args.len() != 4 {
                continue;
            }
            let (vm::Expr::Var(callee), vm::Expr::Var(actuals)) = (&args[1], &args[3]) else {
                continue;
            };
            let allocated = then_branch[..inner_at].iter().any(|candidate| {
                matches!(candidate,
                    vm::Stmt::Let { index, expr: vm::Expr::Call(target, _, args, _, _), .. }
                        if *index == *actuals
                            && *target == vm::BuiltinFunction::ArrayNew.call_index()
                            && args.is_empty())
            });
            if !allocated {
                continue;
            }
            if dispatched
                .replace((inner_at, *dispatch_result, *callee))
                .is_some()
            {
                return None;
            }
        }
        let Some((dispatch_at, raw, _callee)) = dispatched else {
            continue;
        };
        let mut assignments = 0usize;
        for candidate in then_branch {
            if matches!(candidate, vm::Stmt::Assign { index, .. } if *index == result) {
                assignments += 1;
            }
        }
        if assignments != 1 {
            continue;
        }
        let mut consumers = 0usize;
        for candidate in &then_branch[dispatch_at + 1..] {
            let vm::Stmt::Assign {
                index,
                expr: vm::Expr::ResultMatch { value, value_slot, .. },
                ..
            } = candidate
            else {
                continue;
            };
            if *index == result
                && *value_slot == raw
                && matches!(&**value, vm::Expr::Var(provider) if *provider == raw)
            {
                consumers += 1;
            }
        }
        if consumers != 1 {
            continue;
        }
        if found.replace(at).is_some() {
            return None;
        }
    }
    found
}

#[test]
fn pending_store_primitive_source_contract() {
    use vm::SourcePlugin;
    let source = "var f=function(){\"use strict\";return 7;};var o=f();";
    let catalog = heap_catalog();
    let context = vm::SourceFrontendContext {
        source_id: 0,
        original_path: None,
        phase: vm::compiler::ParsePhase::Standalone,
        host_api_catalog: Some(&catalog),
        imports: &[],
    };
    let original = JAVASCRIPT_ES5_PLUGIN
        .parse_source(source, &context)
        .unwrap();
    let binding = heap_binding_index(&original, "o").unwrap();
    let root = heap_helper_index(&original, HEAP_ROOT_WRITE_NAME).unwrap();
    let clear = heap_helper_index(&original, "__compat_es5_root_clear").unwrap();
    let dispatch = heap_helper_index(&original, "__compat_es5_dispatch").unwrap();
    let witnesses: Vec<_> = original.stmts.iter().enumerate().filter_map(|(top, stmt)| {
        let vm::Stmt::IfElse { condition: vm::Expr::Var(normal), then_branch, else_branch, .. } = stmt else { return None; };
        let [vm::Stmt::Assign { index, expr: vm::Expr::Var(result), .. }, vm::Stmt::Let { expr: vm::Expr::Call(target, _, args, _, _), .. }] = then_branch.as_slice() else { return None; };
        if *index != binding || !else_branch.is_empty() || *target != root { return None; }
        if args.len() != 4 || !matches!(args.get(2), Some(vm::Expr::Int(_))) { return None; }
        if !matches!(args.get(3), Some(vm::Expr::Var(slot)) if *slot == binding) { return None; }
        if !original.stmts.iter().any(|s| matches!(s, vm::Stmt::Let { index, declared_schema: Some(vm::TypeSchema::Bool), expr: vm::Expr::Bool(true), .. } if index == normal)) { return None; }
        Some((top, *normal, *result))
    }).collect();
    assert_eq!(witnesses.len(), 1, "unique actual primitive store");
    let (top, normal, result) = witnesses[0];
    let call_at = primitive_call_guard(&original, top, normal, result, dispatch)
        .expect("unique real primitive call guard before the store");
    assert_eq!(call_at + 2, top, "guarded call, common retirement, store");
    let vm::Stmt::IfElse { then_branch: call, .. } = &original.stmts[call_at] else {
        panic!("real call guard");
    };
    let callee = call.iter().find_map(|candidate| {
        let vm::Stmt::Let { expr: vm::Expr::Call(target, _, args, _, _), .. } = candidate else { return None; };
        if *target != dispatch || args.len() != 4 { return None; }
        match args.get(1) { Some(vm::Expr::Var(slot)) => Some(*slot), _ => None }
    }).expect("real dispatch callee");
    let snapshot = original.stmts[..call_at].iter().rposition(|candidate| matches!(candidate,
        vm::Stmt::Let { index, expr: vm::Expr::Var(_), .. } if *index == callee))
        .expect("callee snapshot");
    let callee_entry = pending_store_site::root_entry(&original.stmts[snapshot + 1], root, callee)
        .expect("snapshot callee root entry");
    let vm::Stmt::Let { declared_schema: Some(vm::TypeSchema::Int), expr: vm::Expr::Call(target, types, args, None, None), .. } = &original.stmts[call_at + 1] else {
        panic!("common callee retirement");
    };
    // The common retirement clears the actual callee entry through the
    // canonical three-slot helper: no boxed payload and no four-argument
    // publication remain at this site.
    assert_eq!(*target, clear);
    assert!(types.is_empty() && args.len() == 3);
    assert!(matches!(args[0], vm::Expr::Var(_)) && matches!(args[1], vm::Expr::Var(_)));
    assert!(
        matches!(args[2], vm::Expr::Int(entry) if entry == callee_entry),
        "retirement clears the actual callee root"
    );
    let vm::Stmt::IfElse {
        then_branch: store, ..
    } = &original.stmts[top]
    else {
        panic!("real primitive store guard");
    };
    let binding_entry = pending_store_site::root_entry(&store[1], root, binding)
        .expect("binding refresh root");
    assert!(matches!(&store[0], vm::Stmt::Assign { index, expr: vm::Expr::Var(slot), .. } if *index == binding && *slot == result));
    assert!(matches!(&store[1], vm::Stmt::Let { expr: vm::Expr::Call(target, types, args, None, None), .. }
        if *target == root && types.is_empty() && args.len() == 4
            && matches!(args[3], vm::Expr::Var(slot) if slot == binding)), "binding refresh follows the binding assignment");
    let (ok, err) = call.iter().find_map(|candidate| {
        let vm::Stmt::Assign { index, expr: vm::Expr::ResultMatch { ok_expr, err_expr, .. }, .. } = candidate else { return None; };
        (*index == result).then(|| (&**ok_expr, &**err_expr))
    }).expect("real ResultMatch consumer");
    let vm::Expr::Block { stmts: ok_stmts, .. } = ok else { panic!("real Ok arm"); };
    let vm::Expr::Block { stmts: err_stmts, .. } = err else { panic!("real Err arm"); };
    let entry_of = |stmt: &vm::Stmt| -> Option<i64> {
        let vm::Stmt::Let {
            expr: vm::Expr::Call(target, types, args, None, None),
            ..
        } = stmt
        else {
            return None;
        };
        if !types.is_empty() {
            return None;
        }
        // Either protocol counts as a clear obligation here: the canonical
        // three-slot clear and the legacy four-argument publication.
        let canonical =
            (*target == clear && args.len() == 3) || (*target == root && args.len() == 4);
        if !canonical {
            return None;
        }
        match &args[2] {
            vm::Expr::Int(entry) => Some(*entry),
            _ => None,
        }
    };
    for arm in [ok_stmts, err_stmts] {
        assert!(!arm.iter().any(|candidate| entry_of(candidate) == Some(callee_entry) || entry_of(candidate) == Some(binding_entry)),
            "neither Result arm clears the callee or binding root");
    }
    // A plain-name call initializer binds the dispatch result directly. The
    // result slot is private to the consumer, so a temporary root publication
    // in either arm is redundant once every step up to the binding write is
    // known non-collecting. Neither arm publishes a temporary result root.
    let ok_result_publications = ok_stmts
        .iter()
        .filter(|stmt| pending_store_site::root_entry(stmt, root, result).is_some())
        .count();
    let err_result_publications = err_stmts
        .iter()
        .filter(|stmt| pending_store_site::root_entry(stmt, root, result).is_some())
        .count();
    eprintln!(
        "P9_LOCAL_CALL_INITIALIZER_HANDOFF result_slot={result} result_publish_calls={ok_result_publications}"
    );
    assert_eq!(
        ok_result_publications, 0,
        "Ok arm publishes no temporary result root for a plain-name call initializer"
    );
    assert_eq!(
        err_result_publications, 0,
        "Err arm publishes no temporary result root for a plain-name call initializer"
    );
    eprintln!("PRIMITIVE_ACTUAL_SITE top={top} call={call_at} normal={normal} binding={binding} result={result} callee={callee} callee_root={callee_entry} binding_root={binding_entry} ok_result_publishes={ok_result_publications} err_result_publishes={err_result_publications} store={:?} retirement={:?}",
        &original.stmts[top], &original.stmts[call_at + 1]);
    let observed = PrimitiveHistoryPlugin { iterations: 64 }
        .parse_source(source, &context)
        .unwrap_or_else(|error| panic!("real primitive store refused: {error}"));
    assert!(observed.stmts.len() > original.stmts.len());
    let sites = pending_store_site::sites(&original, binding, root).unwrap();
    let selected = pending_store_site::unique(&sites).unwrap();
    assert_eq!(selected.top, top, "selector agrees with the independent store witness");
    let (start, end) = heap_guarded_primitive_replay(&original, selected).unwrap();
    assert_eq!(end, selected.top + 1, "no result-root clear follows the common store");
    assert_eq!(
        format!("{:?}", &observed.stmts[..original.stmts.len()]),
        format!("{:?}", original.stmts)
    );
    let vm::Stmt::While { body, .. } = &observed.stmts[original.stmts.len() + 1] else {
        panic!("real replay loop");
    };
    let mut expected = original.stmts[start..end].to_vec();
    for stmt in &mut expected {
        heap_replay_assign(stmt);
    }
    assert_eq!(body.len(), expected.len() + 1);
    assert_eq!(
        format!("{:?}", &body[..expected.len()]),
        format!("{expected:?}")
    );
    for at in [call_at, selected.top] {
        assert_eq!(
            format!("{:?}", body[at - start]),
            format!("{:?}", original.stmts[at]),
            "call guard, ResultMatch, nested Lets and store preserved exactly"
        );
    }
    let mut retirement = original.stmts[call_at + 1].clone();
    heap_replay_assign(&mut retirement);
    assert_eq!(
        format!("{:?}", body[call_at + 1 - start]),
        format!("{retirement:?}"),
        "only the top-level common retirement Let becomes Assign"
    );
    assert!(
        matches!(&observed.stmts[original.stmts.len()+2],vm::Stmt::Expr {expr:vm::Expr::Var(slot),..} if *slot==binding)
    );
    eprintln!(
        "PRIMITIVE_REPLAY_PROOF original-prefix-exact top-only-let-conversion real-guard-common-retirement-and-ResultMatch-exact final-o={binding}"
    );
}

fn primitive_history_registry() -> vm::HostFunctionRegistry {
    let mut registry = transaction_registry();
    for name in ["math::is_finite", "math::powf", "math::trunc"] {
        registry
            .allow_builtin(name)
            .unwrap_or_else(|_| panic!("measured primitive-history grant {name}"));
    }
    registry
}

#[test]
fn p2_primitive_return_history_reuses_frames_without_any_node_gc() {
    let mut warm = None;
    for iterations in [64, 4096] {
        let plugin = Box::leak(Box::new(PrimitiveHistoryPlugin { iterations }));
        let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
            "var f=function(){\"use strict\";return 7;};var o=f();",
            vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-primitive-history")),
            vm::CompileSourceFileOptions::new()
                .with_source_plugin(plugin)
                .with_host_api_catalog(heap_catalog()),
        )
        .expect("primitive history source");
        let mut machine = compilation.into_vm().expect("certified history VM");
        machine.set_jit_config(vm::JitConfig {
            enabled: false,
            ..vm::JitConfig::default()
        });
        pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
            &mut machine,
            pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
        )
        .expect("B1");
        primitive_history_registry()
            .bind_vm_cached(&mut machine)
            .expect("bind");
        machine.set_fuel(10_000_000);
        let status = machine.run().expect("primitive history must halt");
        eprintln!(
            "P2_PRIMITIVE_HISTORY_POST_RUN iterations={iterations} status={status:?} fuel={:?} ip={} jit_enabled={} aot={} jit_native={} stack_len={}",
            machine.get_fuel(),
            machine.ip(),
            machine.jit_config().enabled,
            machine.aot_exec_count(),
            machine.jit_native_exec_count(),
            machine.stack().len(),
        );
        if status != VmStatus::Halted {
            let program = machine.program();
            let ip = machine.ip();
            let op = program
                .code
                .get(ip)
                .and_then(|byte| vm::OpCode::try_from(*byte).ok());
            let owner = program
                .script_functions
                .iter()
                .enumerate()
                .find(|(_, function)| {
                    function.entry_ip as usize <= ip && ip < function.end_ip as usize
                })
                .map(|(index, function)| (index, function.entry_ip, function.end_ip));
            eprintln!(
                "P2_PRIMITIVE_HISTORY_YIELD_SITE iterations={iterations} ip={ip} op={op:?} owner={owner:?}"
            );
        }
        assert_eq!(status, VmStatus::Halted);
        assert!(!machine.jit_config().enabled);
        assert_eq!(machine.aot_exec_count(), 0);
        assert_eq!(machine.jit_native_exec_count(), 0);
        let stack = machine.stack();
        let tail = &stack[stack.len() - 6..];
        assert_eq!(heap_box_number(&tail[0]), 7.0);
        let counters = heap_stale_ints(&tail[1..]);
        // The realm bootstrap keeps all 47 intrinsic nodes live plus the one
        // source function node, and its 47 reservations complete one interval
        // collection before the first user call.
        assert_eq!(
            (counters[0], counters[1], counters[4]),
            (HEAP_INTRINSIC_NODES + 1, HEAP_INTRINSIC_NODES + 1, 1)
        );
        eprintln!("P2_PRIMITIVE_HISTORY iterations={iterations} counters={counters:?}");
        // After the first warmed source call, every frame is immediately
        // available for the next equal-size activation. Differential bound0.
        if let Some(expected) = &warm {
            assert_eq!(
                &counters, expected,
                "root-only history cannot rely on future node collection"
            );
        } else {
            warm = Some(counters);
        }
        machine.reset_for_reuse().expect("reset history");
        assert_eq!(machine.host_context().resource_count(), 0);
    }
}

// ---- Closed primitive activation: no unused activation root frame -------
// A proven-primitive controlled return omits its compiler-generated source
// activation root frame entirely: no root edge, no frame entry, no reserved
// capacity and no frame exit survive lowering, while a managed object return
// keeps every real register/clear obligation. Both sources below are the
// actual production JavaScript ES5 plugin lowering; the inspection only
// counts real root helper calls in the actual lowered activation body.
// Nothing here may pass by special-casing today's emission.

const EMPTY_ROOT_IR_SOURCE_FUNCTION_PREFIX: &str = "__compat_es5_fn_";
const EMPTY_ROOT_IR_ROOT_LEAVE_NAME: &str = "__compat_es5_root_leave";

/// Root protocol of the one admitted source function, selected by its
/// production-generated `__compat_es5_fn_<code_id>` name and verified against
/// its real activation metadata. The existing `heap_tree_stmts` walker supplies
/// every statement nested in the actual lowered body, so this counts real
/// calls only: no helper index, statement position or bytecode address is
/// assumed, and no synthetic IR is inserted. Publications stay on the
/// four-argument `root_write`; canonical undefined clears use the
/// three-argument `root_clear`, so both are counted apart.
fn empty_root_ir_protocol(ir: &vm::FrontendIr) -> (u16, usize, usize, Option<i64>, usize, usize) {
    let root_write = heap_helper_index(ir, HEAP_ROOT_WRITE_NAME).expect("actual root_write helper");
    let root_clear =
        heap_helper_index(ir, "__compat_es5_root_clear").expect("actual root_clear helper");
    let root_enter = heap_helper_index(ir, HEAP_ROOT_ENTER_NAME).expect("actual root_enter helper");
    let root_leave =
        heap_helper_index(ir, EMPTY_ROOT_IR_ROOT_LEAVE_NAME).expect("actual root_leave helper");
    let functions: Vec<_> = ir
        .functions
        .iter()
        .filter(|function| function.name.starts_with(EMPTY_ROOT_IR_SOURCE_FUNCTION_PREFIX))
        .collect();
    assert_eq!(
        functions.len(),
        1,
        "this closed source admits exactly one lowered source function, saw {:?}",
        functions
            .iter()
            .map(|function| function.name.as_str())
            .collect::<Vec<_>>()
    );
    let function = functions[0];
    assert!(
        function.name[EMPTY_ROOT_IR_SOURCE_FUNCTION_PREFIX.len()..]
            .parse::<i64>()
            .is_ok(),
        "the generated name carries its source-function code identity: {}",
        function.name
    );
    assert_eq!(function.arity, 5, "actual activation signature of {}", function.name);
    let implementation = ir
        .function_impls
        .get(&function.index)
        .expect("the actual source function has one lowered implementation");
    let mut tree = Vec::new();
    heap_tree_stmts(&implementation.body_stmts, &mut tree);
    let mut writes = 0usize;
    let mut clears = 0usize;
    let mut enters = 0usize;
    let mut capacity = None;
    let mut leaves = 0usize;
    for stmt in tree.iter().copied() {
        let expr = match stmt {
            vm::Stmt::Let { expr, .. }
            | vm::Stmt::Assign { expr, .. }
            | vm::Stmt::Expr { expr, .. } => expr,
            _ => continue,
        };
        let vm::Expr::Call(target, types, args, None, None) = expr else {
            continue;
        };
        if !types.is_empty() {
            continue;
        }
        if *target == root_write && args.len() == 4 {
            writes += 1;
        } else if *target == root_clear && args.len() == 3 {
            clears += 1;
        } else if *target == root_enter {
            enters += 1;
            capacity = match args.get(1) {
                Some(vm::Expr::Int(n)) => Some(*n),
                _ => None,
            };
        } else if *target == root_leave {
            leaves += 1;
        }
    }
    (function.index, writes, clears, capacity, enters, leaves)
}

#[test]
fn primitive_return_omits_unused_activation_root_frame() {
    use vm::SourcePlugin;
    let catalog = heap_catalog();
    let context = vm::SourceFrontendContext {
        source_id: 0,
        original_path: None,
        phase: vm::compiler::ParsePhase::Standalone,
        host_api_catalog: Some(&catalog),
        imports: &[],
    };
    // First real production lowering: the exact closed primitive source.
    let primitive_source = "var f=function(){\"use strict\";return 7;};var o=f();";
    let primitive = JAVASCRIPT_ES5_PLUGIN
        .parse_source(primitive_source, &context)
        .expect("real closed primitive activation source");
    let (index, writes, clears, capacity, enters, leaves) = empty_root_ir_protocol(&primitive);
    eprintln!("UNUSED_ACTIVATION_ROOT_FRAME_IR_FIRST_SOURCE source={primitive_source}");
    eprintln!(
        "UNUSED_ACTIVATION_ROOT_FRAME_IR_OBSERVATION fn={index} root_write_calls={writes} root_clear_calls={clears} capacity={capacity:?} root_enter_calls={enters} root_leave_calls={leaves}"
    );
    // The compiler-generated source activation root frame is unused once the
    // body, completion, escape and publish obligations are fully proven: no
    // root edge, no frame entry, no reserved capacity and no frame exit.
    assert_eq!(
        writes, 0,
        "a proven primitive activation must publish no root edge, observed {writes} root_write calls"
    );
    // A stray canonical clear would prove a frame just as well as a
    // publication, so both protocols must stay at zero here.
    assert_eq!(
        clears, 0,
        "a proven primitive activation must clear no root edge, observed {clears} root_clear calls"
    );
    assert_eq!(
        enters, 0,
        "the unused activation root frame must not be entered, observed {enters} root_enter calls"
    );
    assert_eq!(capacity, Option::<i64>::None, "no reserved capacity, observed {capacity:?}");
    assert_eq!(leaves, 0, "no frame exit, observed {leaves} root_leave calls");
    eprintln!("UNUSED_ACTIVATION_ROOT_FRAME_IR_PRIMITIVE_PROTOCOL fn={index} no-root-frame");
    // Second real production lowering: the managed retention control. It
    // runs only after the primitive protocol holds, so a RED stops before it.
    let managed_source = "var f=function(){\"use strict\";return {v:7};};var o=f();";
    let managed = JAVASCRIPT_ES5_PLUGIN
        .parse_source(managed_source, &context)
        .expect("real managed activation control source");
    let (
        managed_index,
        managed_writes,
        managed_clears,
        managed_capacity,
        managed_enters,
        managed_leaves,
    ) = empty_root_ir_protocol(&managed);
    let managed_capacity =
        managed_capacity.expect("managed control reports its real frame capacity");
    eprintln!(
        "UNUSED_ACTIVATION_ROOT_FRAME_IR_MANAGED_CONTROL fn={managed_index} root_write_calls={managed_writes} root_clear_calls={managed_clears} capacity={managed_capacity} root_enter_calls={managed_enters} root_leave_calls={managed_leaves}"
    );
    // A managed return publishes the reserved pending identity's real edge
    // and clears the consumed value temps, so the same five root obligations
    // remain, now split across the publication and canonical clear
    // protocols: exactly two variable-valued publications and three
    // canonical clears.
    assert_eq!(
        managed_writes + managed_clears,
        5,
        "managed returns keep their five real root obligations, observed {managed_writes} publications and {managed_clears} canonical clears"
    );
    assert_eq!(
        managed_writes, 2,
        "managed returns retain exactly two variable-valued publications"
    );
    assert_eq!(
        managed_clears, 3,
        "managed returns retain exactly three canonical clears"
    );
    assert_eq!(
        managed_capacity, 4,
        "managed returns keep the payload and value entries, observed {managed_capacity}"
    );
    assert_eq!(
        (managed_enters, managed_leaves),
        (1, 1),
        "one real activation frame and exit"
    );
    eprintln!("UNUSED_ACTIVATION_ROOT_FRAME_IR_PROTOCOL primitive=no-root-frame managed=root-retained");
}

// ---- Runtime control for the reserved pending identity ----------------
// One bounded runtime control: four real production cases, each one compile,
// one fresh certified VM and one Halted 10M-fuel run. The EXISTING parent
// HeapReadbackPlugin supplies the stack observation, namely the `result`
// binding read once by its appended IR plus the already-existing 5/0/3
// observe tail (collections, live nodes, heap_end). No guest readback, no
// reentry, no refuel, no new frontend id and no new framework.

fn empty_root_runtime_case(case: &str, source: &str) -> (Value, [i64; 3]) {
    let reads = vec!["result".to_owned()];
    let plugin: &'static HeapReadbackPlugin = Box::leak(Box::new(HeapReadbackPlugin { reads }));
    let catalog = heap_catalog();
    eprintln!("EMPTY_ROOT_RUNTIME_CASE {case} phase=source source={source}");
    eprintln!(
        "EMPTY_ROOT_RUNTIME_CASE {case} phase=compile plugin=javascript-es5-heap-readback compiles=1"
    );
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-heap-readback")),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(plugin)
            .with_host_api_catalog(std::sync::Arc::clone(&catalog)),
    )
    .expect("real runtime control source must compile");
    let mut machine = compilation
        .into_vm()
        .expect("one certified fresh VM per case");
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..vm::JitConfig::default()
    });
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("default owned arrays");
    let mut registry = vm::HostFunctionRegistry::restricted();
    install_heap_production(&mut registry, &catalog, machine.program());
    registry
        .bind_vm_cached(&mut machine)
        .expect("restricted production adapters bind to this VM");
    machine.set_fuel(10_000_000);
    eprintln!("EMPTY_ROOT_RUNTIME_CASE {case} phase=run fuel=10000000 runs=1 jit=off");
    let status = machine
        .run()
        .expect("real runtime control must not fail");
    assert_eq!(
        status,
        VmStatus::Halted,
        "runtime control must halt, got {status:?}"
    );
    assert!(!machine.jit_config().enabled);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    let observes = HEAP_SAFEPOINT_OBSERVES.len();
    let stack = machine.stack().to_vec();
    let result = stack[stack.len() - observes - 1].clone();
    let tail = heap_stale_ints(&stack[stack.len() - observes..]);
    let counters = [tail[0], tail[1], tail[2]];
    eprintln!(
        "EMPTY_ROOT_RUNTIME_CASE {case} phase=outcome status=Halted aot=0 jit_native=0 collections={} live={} heap_end={} result={result:?}",
        counters[0], counters[1], counters[2]
    );
    machine.reset_for_reuse().expect("reset after observations");
    assert_eq!(machine.host_context().resource_count(), 0);
    assert!(
        pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine).is_none()
    );
    eprintln!("EMPTY_ROOT_RUNTIME_CASE {case} phase=cleanup resources=0 owned_arrays=none");
    (result, counters)
}

#[test]
fn empty_root_publication_runtime_controls() {
    // (1) Managed object return under real GC pressure. The 40 loop
    // iterations allocate real nodes, so more than the bootstrap collection
    // proves real collection, and the returned object payload must read back.
    let (managed, managed_counters) = empty_root_runtime_case(
        "managed-object-under-pressure",
        "var f=function(){\"use strict\";var a={v:7};var i=0;while(i<40){var x={q:i};i=i+1;}return a;};var o=f();var result=o.v;",
    );
    assert_eq!(
        heap_box_number(&managed),
        7.0,
        "managed object payload must survive pressure: {managed:?}"
    );
    assert!(
        managed_counters[0] > 1,
        "real collection beyond the bootstrap, got {}",
        managed_counters[0]
    );
    // (2) Managed string return: the equality result must be an exact boxed
    // Boolean true, i.e. the reserved pending identity published no EMPTY
    // edge and the string node still reached the comparison.
    let (text, _) = empty_root_runtime_case(
        "managed-string-return",
        "var f=function(){\"use strict\";return \"ok\";};var s=f();var result=s===\"ok\";",
    );
    assert_eq!(
        heap_box_tag(&text),
        2,
        "equality result must be a boxed Boolean, got {text:?}"
    );
    let Value::Map(fields) = &text else {
        panic!("boxed Boolean fields")
    };
    assert_eq!(fields.get(&Value::string("flag")), Some(&Value::Bool(true)));
    // (3) Thrown managed object: the throw handoff must still publish its
    // pending edge, so the caught payload reads back exactly.
    let (caught, _) = empty_root_runtime_case(
        "thrown-object",
        "var f=function(){\"use strict\";throw {v:7};};var result=0;try{f();}catch(e){result=e.v;}",
    );
    assert_eq!(
        heap_box_number(&caught),
        7.0,
        "caught throw payload must be boxed NUMBER 7: {caught:?}"
    );
    // (4) Nested Err propagation: a callee throw through a caller return
    // keeps the pending payload owned until the script catch consumes it.
    let (nested, _) = empty_root_runtime_case(
        "nested-err-propagation",
        "function f(){\"use strict\";function g(){\"use strict\";throw {v:7};}return g();}var result=0;try{f();}catch(e){result=e.v;}",
    );
    assert_eq!(
        heap_box_number(&nested),
        7.0,
        "nested Err payload must be boxed NUMBER 7: {nested:?}"
    );
    eprintln!("EMPTY_ROOT_RUNTIME_CONTROL cases=4 compiles=4 vms=4 runs=4");
}

// ---- Bounded IR eligibility for the immediate local call-result handoff ---
// Each source below is one real production frontend parse. No VM, no Core and
// no source-keyword filter: every assertion reads the actual dispatch
// ResultMatch, its real Ok-arm result slot and the real root_write/clear
// statements that reference that slot.

/// Canonical Ok-arm observations of every real dispatch `ResultMatch` in one
/// lowered program, in actual IR order (inner argument calls precede the
/// outer call whose actuals they are).
struct CallResultArm<'a> {
    result: u16,
    ok_stmts: &'a [vm::Stmt],
}

fn call_result_match_nodes<'a>(statements: &'a [vm::Stmt], out: &mut Vec<&'a vm::Expr>) {
    for statement in statements {
        match statement {
            vm::Stmt::Let { expr, .. }
            | vm::Stmt::Assign { expr, .. }
            | vm::Stmt::Expr { expr, .. } => call_result_match_expr(expr, out),
            vm::Stmt::IfElse {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                call_result_match_expr(condition, out);
                call_result_match_nodes(then_branch, out);
                call_result_match_nodes(else_branch, out);
            }
            _ => {}
        }
    }
}

fn call_result_match_expr<'a>(expr: &'a vm::Expr, out: &mut Vec<&'a vm::Expr>) {
    match expr {
        vm::Expr::ResultMatch {
            value,
            ok_expr,
            err_expr,
            ..
        } => {
            out.push(expr);
            call_result_match_expr(value, out);
            call_result_match_expr(ok_expr, out);
            call_result_match_expr(err_expr, out);
        }
        vm::Expr::Block { stmts, expr } => {
            call_result_match_nodes(stmts, out);
            call_result_match_expr(expr, out);
        }
        vm::Expr::IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            call_result_match_expr(condition, out);
            call_result_match_expr(then_expr, out);
            call_result_match_expr(else_expr, out);
        }
        vm::Expr::Call(_, _, args, _, _) => {
            for arg in args {
                call_result_match_expr(arg, out);
            }
        }
        vm::Expr::ResultTry { value, .. } => call_result_match_expr(value, out),
        _ => {}
    }
}

/// The real dispatch `ResultMatch` extracts the completion value through the
/// one private int-identity/return-kind `IfElse`; helper Result consumers
/// store the already-boxed success value with a plain assignment.
fn is_call_dispatch_arm(ok_stmts: &[vm::Stmt]) -> bool {
    ok_stmts.iter().any(|stmt| {
        matches!(stmt,
            vm::Stmt::Assign {
                expr: vm::Expr::IfElse { condition, .. },
                ..
            } if matches!(&**condition,
                vm::Expr::Eq(left, right)
                    if matches!(&**left, vm::Expr::Call(..))
                        && matches!(&**right, vm::Expr::Int(_)))
        )
    })
}

fn call_result_arms<'a>(ir: &'a vm::FrontendIr) -> Vec<CallResultArm<'a>> {
    let mut nodes = Vec::new();
    call_result_match_nodes(&ir.stmts, &mut nodes);
    nodes
        .into_iter()
        .filter_map(|node| {
            let vm::Expr::ResultMatch { ok_expr, .. } = node else {
                return None;
            };
            let vm::Expr::Block { stmts, expr } = &**ok_expr else {
                return None;
            };
            let vm::Expr::Var(result) = &**expr else {
                return None;
            };
            if !is_call_dispatch_arm(stmts) {
                return None;
            }
            Some(CallResultArm {
                result: *result,
                ok_stmts: stmts,
            })
        })
        .collect()
}

/// `None` means the private result slot was never published into the frame
/// from its own Ok arm; `Some(entry)` is the actual published root entry.
fn call_result_rootings(ir: &vm::FrontendIr) -> Vec<Option<i64>> {
    let root = heap_helper_index(ir, HEAP_ROOT_WRITE_NAME).expect("actual root_write helper");
    call_result_arms(ir)
        .into_iter()
        .map(|arm| {
            let mut publications: Vec<_> = arm
                .ok_stmts
                .iter()
                .filter_map(|stmt| pending_store_site::root_entry(stmt, root, arm.result))
                .collect();
            assert!(
                publications.len() <= 1,
                "one Ok arm publishes the private result slot at most once"
            );
            publications.pop()
        })
        .collect()
}

/// Actual canonical `__compat_es5_root_clear` statements that clear one known
/// root entry in the whole lowered program: three arguments, the real frame
/// slot and the Int entry, with no boxed payload. The retired four-argument
/// `root_write` plus `__compat_es5_undefined` form is deliberately not
/// accepted here, so an obsolete clear cannot satisfy this count.
fn canonical_root_clears(ir: &vm::FrontendIr, entry: i64) -> usize {
    let clear = heap_helper_index(ir, "__compat_es5_root_clear").expect("actual root_clear helper");
    let mut tree = Vec::new();
    heap_tree_stmts(&ir.stmts, &mut tree);
    tree.iter()
        .copied()
        .filter(|stmt| {
            let vm::Stmt::Let {
                expr: vm::Expr::Call(target, types, args, None, None),
                ..
            } = stmt
            else {
                return false;
            };
            *target == clear
                && types.is_empty()
                && args.len() == 3
                && matches!(args[0], vm::Expr::Var(_))
                && matches!(args[1], vm::Expr::Var(_))
                && matches!(args[2], vm::Expr::Int(value) if value == entry)
        })
        .count()
}

fn parse_call_initializer_ir(source: &str) -> vm::FrontendIr {
    use vm::SourcePlugin;
    let catalog = heap_catalog();
    let context = vm::SourceFrontendContext {
        source_id: 0,
        original_path: None,
        phase: vm::compiler::ParsePhase::Standalone,
        host_api_catalog: Some(&catalog),
        imports: &[],
    };
    JAVASCRIPT_ES5_PLUGIN
        .parse_source(source, &context)
        .unwrap_or_else(|error| panic!("real call-initializer source refused: {source}: {error}"))
}

#[test]
fn call_initializer_ir_eligibility() {
    // Four real production frontend parses; zero Core programs, zero VMs.
    let plain = "var f=function(){\"use strict\";return 7;};var o=f();";
    let ir = parse_call_initializer_ir(plain);
    eprintln!("CALL_INITIALIZER_IR_ELIGIBILITY source=plain frontends=1");
    assert_eq!(
        call_result_rootings(&ir),
        vec![None],
        "plain local identifier call initializer publishes no result root"
    );
    let root = heap_helper_index(&ir, HEAP_ROOT_WRITE_NAME).expect("actual root_write helper");
    let binding = heap_binding_index(&ir, "o").expect("actual o binding");
    let sites = pending_store_site::sites(&ir, binding, root).expect("bounded store sites");
    let site = pending_store_site::unique(&sites).expect("unique actual store");
    let (_, end) = heap_guarded_primitive_replay(&ir, site)
        .expect("canonical local initializer replay without a temporary result root");
    assert_eq!(
        end,
        site.after_anchor(),
        "replay ends at the binding store, before the program postlude"
    );

    let paren = "var f=function(){\"use strict\";return 7;};var o=(f());";
    let ir = parse_call_initializer_ir(paren);
    eprintln!("CALL_INITIALIZER_IR_ELIGIBILITY source=paren frontends=1");
    assert_eq!(
        call_result_rootings(&ir),
        vec![None],
        "a parenthesized outermost call forwards the immediate policy"
    );

    let nested = "var f=function(){\"use strict\";return 7;};var g=function(){\"use strict\";return 7;};var o=f(g());";
    let ir = parse_call_initializer_ir(nested);
    eprintln!("CALL_INITIALIZER_IR_ELIGIBILITY source=nested frontends=1");
    let rootings = call_result_rootings(&ir);
    let [inner, outer] = rootings.as_slice() else {
        panic!("nested actual retains both real ResultMatches: {rootings:?}");
    };
    let inner_entry = (*inner).expect("the nested argument call stays Temporary");
    assert_eq!(
        canonical_root_clears(&ir, inner_entry),
        1,
        "inner Temporary clear"
    );
    assert_eq!(
        *outer, None,
        "the eligible outer call publishes no result root"
    );

    let speculative = "var f=function(){\"use strict\";return 7;};var c=0;if(c){var o=f();}c===0;";
    let ir = parse_call_initializer_ir(speculative);
    eprintln!("CALL_INITIALIZER_IR_ELIGIBILITY source=speculative frontends=1");
    assert_eq!(
        call_result_rootings(&ir),
        vec![None],
        "a guarded speculative arm keeps the same immediate policy and guards"
    );
    eprintln!("CALL_INITIALIZER_IR_ELIGIBILITY frontends=4 cores=0 vms=0");
}

#[test]
fn call_initializer_ir_exclusions() {
    // Six real production frontend parses; zero Core programs, zero VMs.
    let cases: [(&str, &str); 5] = [
        (
            "member-callee",
            "var o={m:function(){\"use strict\";return 7;}};var r=o.m();",
        ),
        (
            "computed-member-callee",
            "var o={m:function(){\"use strict\";return 7;}};var r=o[\"m\"]();",
        ),
        (
            "noncall-rhs-member",
            "var f=function(){\"use strict\";return {v:7};};var o=f().v;",
        ),
        (
            "object-literal-field",
            "var f=function(){\"use strict\";return 7;};var o={v:f()};",
        ),
        (
            "allocation-other-operand",
            "var f=function(){\"use strict\";return 0;};var o=f()||{v:1};",
        ),
    ];
    for (case, source) in cases {
        let ir = parse_call_initializer_ir(source);
        let rootings = call_result_rootings(&ir);
        let [entry] = rootings.as_slice() else {
            panic!("{case}: exactly one real ResultMatch, saw {rootings:?}");
        };
        let entry = (*entry).unwrap_or_else(|| panic!("{case}: Temporary keeps the result root"));
        assert_eq!(
            canonical_root_clears(&ir, entry),
            1,
            "{case}: the Temporary result root is cleared exactly once"
        );
        eprintln!("CALL_INITIALIZER_IR_EXCLUSION case={case} temporary=1 frontends=1");
    }

    // Global owner: an already admitted deferred-access fixture (a function
    // writes the canonical writable name) makes the destination a shared
    // global, whose `.set` consumer roots the value in a different owner.
    let global = "var f=function(){\"use strict\";return 7;};var Math=false;function set(){Math=true;}var Math=f();Math;";
    let ir = parse_call_initializer_ir(global);
    let rootings = call_result_rootings(&ir);
    let [entry] = rootings.as_slice() else {
        panic!("global: exactly one real ResultMatch, saw {rootings:?}");
    };
    let entry = (*entry).expect("global destination keeps the Temporary result root");
    assert_eq!(
        canonical_root_clears(&ir, entry),
        1,
        "global destination keeps the Temporary result clear"
    );
    assert!(
        heap_binding_index(&ir, "Math").is_err(),
        "the shared global destination has no local binding slot"
    );
    let set = heap_helper_index(&ir, "__compat_es5_global_env_set")
        .expect("shared environment exposes its actual set consumer");
    let mut tree = Vec::new();
    heap_tree_stmts(&ir.stmts, &mut tree);
    let sets = tree
        .iter()
        .copied()
        .filter(|stmt| {
            let vm::Stmt::Let {
                expr: vm::Expr::Call(target, types, args, None, None),
                ..
            } = stmt
            else {
                return false;
            };
            *target == set
                && types.is_empty()
                && args.len() == 4
                && matches!(args[0], vm::Expr::Var(_))
                && matches!(args[1], vm::Expr::Var(_))
                && matches!(args[2], vm::Expr::Int(_))
                && matches!(args[3], vm::Expr::Var(_))
        })
        .count();
    assert!(
        sets >= 1,
        "the global destination stores through the actual `.set` consumer, saw {sets}"
    );
    eprintln!("CALL_INITIALIZER_IR_EXCLUSION case=global temporary=1 frontends=1 sets={sets}");
    eprintln!("CALL_INITIALIZER_IR_EXCLUSIONS frontends=6 cores=0 vms=0");
}

// ---- Bounded runtime handoff controls ------------------------------------
// Each case is one real production compile, one fresh certified VM and one
// Halted 10M-fuel run through the existing restricted production adapters and
// the existing HeapReadbackPlugin stack observation (JIT off, AOT native 0,
// capture valid, reset reuse with resource count 0). No new framework.

fn heap_result_true(v: &Value) -> bool {
    matches!(v, Value::Map(fields)
        if fields.get(&Value::string("tag")) == Some(&Value::Int(2))
            && fields.get(&Value::string("flag")) == Some(&Value::Bool(true)))
}

#[test]
fn call_initializer_handoff_runtime_controls() {
    // (1) Managed object return, allocation pressure AFTER the initializer.
    let (managed, managed_counters) = empty_root_runtime_case(
        "call-initializer-object-after-pressure",
        "var f=function(){\"use strict\";return {v:7};};var o=f();var i=0;while(i<40){var x={q:i};i=i+1;}var result=o.v;",
    );
    assert_eq!(
        heap_box_number(&managed),
        7.0,
        "managed object payload survives post-initializer pressure: {managed:?}"
    );
    assert!(
        managed_counters[0] > 1,
        "real post-initializer collection beyond the bootstrap, got {}",
        managed_counters[0]
    );
    // (2) Aliased managed argument returned after callee/input retirement.
    let (aliased, aliased_counters) = empty_root_runtime_case(
        "call-initializer-aliased-argument",
        "var f=function(s){\"use strict\";var t=s;return t;};var a={v:7};var o=f(a);a=null;var i=0;while(i<40){var x={q:i};i=i+1;}var result=o.v;",
    );
    assert_eq!(
        heap_box_number(&aliased),
        7.0,
        "aliased managed argument survives input retirement and GC: {aliased:?}"
    );
    assert!(
        aliased_counters[0] > 1,
        "real collection after input retirement, got {}",
        aliased_counters[0]
    );
    // (3) Managed string return later under pressure.
    let (text, text_counters) = empty_root_runtime_case(
        "call-initializer-string-after-pressure",
        "var f=function(){\"use strict\";return \"ok\";};var s=f();var i=0;while(i<40){var x={q:i};i=i+1;}var result=s===\"ok\";",
    );
    assert!(
        heap_result_true(&text),
        "managed String return survives later pressure: {text:?}"
    );
    assert!(
        text_counters[0] > 1,
        "real collection after the String initializer, got {}",
        text_counters[0]
    );
    // (4) A repeated loop initializer replaces the managed binding while the
    // old alias stays held: both payloads must read back after real GC.
    let (replaced, replaced_counters) = empty_root_runtime_case(
        "call-initializer-loop-replaces-binding",
        "var f=function(k){\"use strict\";return {v:k};};var o=f(7);var old=o;var i=0;while(i<40){var o=f(8);i=i+1;}var result=old.v===7&&o.v===8;",
    );
    assert!(
        heap_result_true(&replaced),
        "old alias and new payload both survive the loop GC: {replaced:?}"
    );
    assert!(
        replaced_counters[0] > 1,
        "real collection across the repeated initializer, got {}",
        replaced_counters[0]
    );
    eprintln!("CALL_INITIALIZER_HANDOFF_RUNTIME_CONTROL cases=4 compiles=4 vms=4 runs=4");
}

#[test]
fn call_initializer_handoff_abrupt_runtime_controls() {
    // (1) A throwing callee must leave the old managed binding untouched and
    // keep the pending object payload rooted through catch-side pressure.
    let (kept, kept_counters) = empty_root_runtime_case(
        "call-initializer-callee-throw-preserves-binding",
        "var keep={v:8};var f=function(){\"use strict\";throw {v:7};};var result=false;try{var keep=f();}catch(e){var i=0;while(i<40){var t={q:i};i=i+1;}result=keep.v===8&&e.v===7;}",
    );
    assert!(
        heap_result_true(&kept),
        "callee throw keeps old binding and payload: {kept:?}"
    );
    assert!(
        kept_counters[0] > 1,
        "real catch-side collection, got {}",
        kept_counters[0]
    );
    // (2) A throwing argument before the outer call skips dispatch, retires
    // the input roots and keeps the pending payload plus the old binding.
    let (skipped, skipped_counters) = empty_root_runtime_case(
        "call-initializer-argument-throw-skips-dispatch",
        "var keep={v:8};var state={n:0};var g=function(){\"use strict\";throw {v:7};};var f=function(s,a){\"use strict\";s.n=7;return 0;};var result=false;try{var keep=f(state,g());}catch(e){var i=0;while(i<40){var t={q:i};i=i+1;}result=keep.v===8&&state.n===0&&e.v===7;}",
    );
    assert!(
        heap_result_true(&skipped),
        "argument throw skips dispatch and preserves roots: {skipped:?}"
    );
    assert!(
        skipped_counters[0] > 1,
        "real catch-side collection after the argument throw, got {}",
        skipped_counters[0]
    );
    // (3) A finally-window managed reassignment replaces the old binding while
    // the Err payload stays rooted until the catch consumes it.
    let (finalized, finalized_counters) = empty_root_runtime_case(
        "call-initializer-finally-reassignment-preserves-err",
        "var keep={v:8};var f=function(){\"use strict\";throw {v:7};};var result=false;try{try{var keep=f();}finally{keep={v:9};}}catch(e){var i=0;while(i<40){var t={q:i};i=i+1;}result=keep.v===9&&e.v===7;}",
    );
    assert!(
        heap_result_true(&finalized),
        "finally reassignment and Err payload both survive: {finalized:?}"
    );
    assert!(
        finalized_counters[0] > 1,
        "real collection after the finally window, got {}",
        finalized_counters[0]
    );
    eprintln!("CALL_INITIALIZER_HANDOFF_ABRUPT_RUNTIME_CONTROL cases=3 compiles=3 vms=3 runs=3");
}

// ---- Canonical undefined root clear -------------------------------------
// Witnesses for the compiler-owned canonical undefined clear. Each test
// parses the real production JavaScript ES5 lowering once, then counts the
// actual program-level three-argument `__compat_es5_root_clear` statements
// and requires the retired four-argument `root_write` plus
// `__compat_es5_undefined` form to be gone. Helper bodies live in
// `function_impls` and never in `ir.stmts`, so `heap_tree_stmts` counts real
// call sites only. Nothing below compiles or runs.

/// Post-target contract of one emitted `__compat_es5_root_clear` and its
/// real call sites: a private `(carrier, frame, entry) -> Int` helper with no
/// captures that reuses the generic root write's complete frame check prefix
/// and then publishes the three cells of `entry` through the existing native
/// cell writer with as_number payloads, keeping at least one actual
/// program-level call with a real frame slot and an Int entry.
/// `generic_publications` keeps the main case's variable-valued
/// four-argument publications asserted; the fresh RegExp case has none to
/// require.
fn known_undefined_root_clear_contract(
    ir: &vm::FrontendIr,
    case: &str,
    generic_publications: bool,
) {
    // Statement classification only: the canonical clear is shape-checked
    // against the exact canonical expressions the producer builds, never by
    // evaluating an arbitrary expression at sample points.
    fn top_call(expr: &vm::Expr) -> Option<u16> {
        match expr {
            vm::Expr::Call(target, _, _, None, None) => Some(*target),
            _ => None,
        }
    }
    fn statement_call(stmt: &vm::Stmt) -> Option<u16> {
        if let vm::Stmt::Let { expr, .. } = stmt {
            return top_call(expr);
        }
        if let vm::Stmt::Assign { expr, .. } = stmt {
            return top_call(expr);
        }
        if let vm::Stmt::Expr { expr, .. } = stmt {
            return top_call(expr);
        }
        None
    }
    fn statement_kind(stmt: &vm::Stmt) -> u8 {
        if matches!(stmt, vm::Stmt::Let { .. }) {
            0
        } else if matches!(stmt, vm::Stmt::Assign { .. }) {
            1
        } else if matches!(stmt, vm::Stmt::Expr { .. }) {
            2
        } else {
            3
        }
    }
    fn prefix_signature(body: &[vm::Stmt]) -> Vec<(u8, Option<u16>)> {
        body.iter()
            .map(|stmt| (statement_kind(stmt), statement_call(stmt)))
            .collect()
    }

    let root =
        heap_helper_index(ir, HEAP_ROOT_WRITE_NAME).expect("actual generic root_write helper");
    let write = heap_helper_index(ir, "__compat_ref_write").expect("actual native cell writer");
    let undefined =
        heap_helper_index(ir, "__compat_es5_undefined").expect("actual undefined constructor");
    let clear = heap_helper_index(ir, "__compat_es5_root_clear")
        .expect("the production plugin must declare the private three-slot canonical clear");
    let clear_decl = ir
        .functions
        .iter()
        .find(|function| function.index == clear)
        .expect("canonical clear declaration");
    assert!(
        !clear_decl.exported,
        "{case}: the canonical clear stays private"
    );
    assert_eq!(clear_decl.arity, 3, "{case}: carrier, frame and entry");
    assert_eq!(
        clear_decl.arg_schemas,
        vec![
            Some(vm::TypeSchema::Named(
                "__CompatRefCarrier".to_owned(),
                Vec::new()
            )),
            Some(vm::TypeSchema::Int),
            Some(vm::TypeSchema::Int),
        ],
        "{case}: the ref carrier, then the frame and entry Int slots"
    );
    assert_eq!(
        clear_decl.return_schema,
        Some(vm::TypeSchema::Int),
        "{case}: the canonical clear reports its ack as Int"
    );
    assert!(
        matches!(&clear_decl.return_type, vm::ValueType::Int),
        "{case}: the canonical clear result type is Int, never Int-or-Float"
    );

    let root_impl = ir
        .function_impls
        .get(&root)
        .expect("actual generic root_write body");
    // The generic ack stays the literal Int 1 the emitter returns.
    assert!(
        matches!(&root_impl.body_expr, vm::Expr::Int(1)),
        "{case}: the generic root write completes with Int 1, saw {:?}",
        root_impl.body_expr
    );
    let clear_impl = ir
        .function_impls
        .get(&clear)
        .expect("actual canonical clear body");
    assert!(
        clear_impl.capture_copies.is_empty(),
        "{case}: the canonical clear captures nothing"
    );
    assert_eq!(
        clear_impl.param_slots.len(),
        3,
        "{case}: three real parameter slots"
    );
    let [carrier, frame, entry] = clear_impl.param_slots.as_slice() else {
        panic!("{case}: carrier, frame and entry parameter slots");
    };
    // The generic ack, not the last store result: the sink below is Number.
    assert!(
        matches!(&clear_impl.body_expr, vm::Expr::Int(1)),
        "{case}: the canonical clear completes with the generic Int 1, saw {:?}",
        clear_impl.body_expr
    );
    let Some(vm::Stmt::Let {
        index: sink,
        declared_schema: Some(vm::TypeSchema::Number),
        expr: sink_init,
        ..
    }) = clear_impl.body_stmts.first()
    else {
        panic!("{case}: the canonical clear opens with the Number sink cell");
    };
    let vm::Expr::Float(zero) = sink_init else {
        panic!("{case}: the sink starts at the Float zero, saw {sink_init:?}");
    };
    assert_eq!(*zero, 0.0, "{case}: the sink starts at 0.0");

    let first_cell_write = |body: &[vm::Stmt]| {
        body.iter().position(|stmt| {
            let expr = match stmt {
                vm::Stmt::Let { expr, .. }
                | vm::Stmt::Assign { expr, .. }
                | vm::Stmt::Expr { expr, .. } => expr,
                _ => return false,
            };
            matches!(expr, vm::Expr::Call(target, _, _, None, None) if *target == write)
        })
    };
    let generic_write = first_cell_write(&root_impl.body_stmts)
        .expect("the generic root write keeps its native cell writes");
    let clear_write = first_cell_write(&clear_impl.body_stmts)
        .expect("the canonical clear writes through the native cell writer");

    // The generic root write first loads the boxed tag only after the shared
    // frame checks; its first native write is much later, past the tag,
    // ref_slot, generation, category, out_slot and out_gen preparation. The
    // three-slot clear has no boxed value at all, so its prefix must end
    // exactly where the frame checks end, before that tag load.
    let value_slot = *root_impl
        .param_slots
        .get(3)
        .expect("the generic root write keeps its payload parameter");
    let boxed_field = |stmt: &vm::Stmt| -> Option<String> {
        let vm::Stmt::Let {
            expr: vm::Expr::Call(target, types, args, None, None),
            ..
        } = stmt
        else {
            return None;
        };
        if *target != vm::BuiltinFunction::Get.call_index() || !types.is_empty() || args.len() != 2
        {
            return None;
        }
        match (&args[0], &args[1]) {
            (vm::Expr::Var(slot), vm::Expr::String(name)) if *slot == value_slot => {
                Some(name.clone())
            }
            _ => None,
        }
    };
    let frame_checks_end = root_impl
        .body_stmts
        .iter()
        .position(|stmt| boxed_field(stmt).as_deref() == Some("tag"))
        .expect("the generic root write loads the boxed tag after its frame checks");
    assert!(
        frame_checks_end > 0,
        "{case}: the generic root write proves its frame before it loads the payload"
    );
    assert_eq!(
        root_impl
            .body_stmts
            .get(frame_checks_end + 1)
            .and_then(boxed_field)
            .as_deref(),
        Some("ref_slot"),
        "{case}: the generic root write loads the boxed ref_slot directly after the tag"
    );
    assert_eq!(
        root_impl
            .body_stmts
            .get(frame_checks_end + 2)
            .and_then(boxed_field)
            .as_deref(),
        Some("generation"),
        "{case}: the generic root write loads the boxed generation directly after ref_slot"
    );
    assert!(
        frame_checks_end + 2 < generic_write,
        "{case}: the three direct field loads stay before the generic native writes"
    );
    assert_eq!(
        clear_write, frame_checks_end,
        "{case}: the canonical clear reuses the frame checks and stops before the boxed tag load"
    );
    assert_eq!(
        prefix_signature(&clear_impl.body_stmts[..clear_write]),
        prefix_signature(&root_impl.body_stmts[..frame_checks_end]),
        "{case}: both prefixes keep the same ordered frame checks, five asserts included"
    );
    assert_eq!(
        clear_impl.body_stmts.len(),
        clear_write + 3,
        "{case}: exactly the three entry cells follow the frame checks"
    );
    let clear_calls: Vec<u16> = clear_impl
        .body_stmts
        .iter()
        .filter_map(statement_call)
        .collect();
    assert!(
        !clear_calls.contains(&vm::BuiltinFunction::Get.call_index()),
        "{case}: the three-slot clear loads no boxed fields"
    );
    assert!(
        !clear_calls.contains(&vm::BuiltinFunction::MapNew.call_index()),
        "{case}: the three-slot clear allocates no record"
    );
    assert!(
        !clear_calls.contains(&vm::BuiltinFunction::Set.call_index()),
        "{case}: the three-slot clear stores no record fields"
    );
    assert!(
        !clear_calls.contains(&undefined),
        "{case}: the three-slot clear rebuilds no undefined record"
    );

    // Canonical producer forms: the same entry_base expression and the same
    // as_number write_call the generic helper builds (heap.rs entry_base and
    // write_call). The exact expression is required, not a value at samples.
    let entry_base = vm::Expr::Add(
        Box::new(vm::Expr::Add(
            Box::new(vm::Expr::Var(*frame)),
            Box::new(vm::Expr::Int(HEAP_FRAME_ENTRY_BASE)),
        )),
        Box::new(vm::Expr::Mul(
            Box::new(vm::Expr::Var(*entry)),
            Box::new(vm::Expr::Int(HEAP_FRAME_ENTRY_CELLS)),
        )),
    );
    for cell in 0..HEAP_FRAME_ENTRY_CELLS {
        let stmt = &clear_impl.body_stmts[clear_write + cell as usize];
        let vm::Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index,
            expr: vm::Expr::Call(target, types, args, None, None),
            ..
        } = stmt
        else {
            panic!("{case}: cell {cell} is published by one Set assignment");
        };
        assert_eq!(*index, *sink, "{case}: every cell reuses the one sink");
        assert_eq!(
            *target, write,
            "{case}: cell {cell} goes through the native cell writer"
        );
        assert!(
            types.is_empty(),
            "{case}: the native write takes no type arguments"
        );
        assert_eq!(args.len(), 3, "{case}: carrier, address and payload");
        assert!(
            matches!(&args[0], vm::Expr::Var(slot) if slot == carrier),
            "{case}: cell {cell} writes through the carrier parameter"
        );
        let expected_address = vm::Expr::Div(
            Box::new(vm::Expr::Add(
                Box::new(entry_base.clone()),
                Box::new(vm::Expr::Int(cell)),
            )),
            Box::new(vm::Expr::Float(1.0)),
        );
        assert!(
            matches!(&args[1], vm::Expr::Div(..)),
            "{case}: cell {cell} addresses through as_number, not a raw Int"
        );
        assert_eq!(
            format!("{:?}", args[1]),
            format!("{:?}", expected_address),
            "{case}: cell {cell} addresses exactly the canonical entry cell"
        );
        // Every cell clears through as_number of the canonical ROOT_EMPTY Int
        // zero: no raw Int cell and no optional peel of a boxed record.
        let expected_payload =
            vm::Expr::Div(Box::new(vm::Expr::Int(0)), Box::new(vm::Expr::Float(1.0)));
        assert_eq!(
            format!("{:?}", args[2]),
            format!("{:?}", expected_payload),
            "{case}: cell {cell} publishes the canonical empty root payload"
        );
    }

    // The real call sites, again only from the program tree.
    let mut tree = Vec::new();
    heap_tree_stmts(&ir.stmts, &mut tree);
    let canonical_clears = tree
        .iter()
        .copied()
        .filter(|stmt| {
            let vm::Stmt::Let {
                expr: vm::Expr::Call(target, types, args, None, None),
                ..
            } = stmt
            else {
                return false;
            };
            *target == clear
                && types.is_empty()
                && args.len() == 3
                && matches!(args[0], vm::Expr::Var(_))
                && matches!(args[1], vm::Expr::Var(_))
                && matches!(args[2], vm::Expr::Int(_))
        })
        .count();
    assert!(
        canonical_clears >= 1,
        "{case}: the canonical clear keeps its actual call site with a real frame slot and Int entry, saw {canonical_clears}"
    );
    if generic_publications {
        let publications = tree
            .iter()
            .copied()
            .filter(|stmt| {
                let vm::Stmt::Let {
                    expr: vm::Expr::Call(target, types, args, None, None),
                    ..
                } = stmt
                else {
                    return false;
                };
                *target == root
                    && types.is_empty()
                    && args.len() == 4
                    && matches!(args[3], vm::Expr::Var(_))
            })
            .count();
        assert!(
            publications >= 1,
            "{case}: variable-valued publications stay on the generic four-argument root write, saw {publications}"
        );
        eprintln!("KNOWN_UNDEFINED_ROOT_CLEAR_IR case={case} generic_publications={publications}");
    }
    eprintln!(
        "KNOWN_UNDEFINED_ROOT_CLEAR_IR case={case} canonical_clears={canonical_clears} frame_prefix={clear_write}"
    );
}

#[test]
fn known_undefined_root_clear_main_source_contract() {
    // One production frontend parse: zero Core programs, zero VMs.
    let source = "var f=function(){\"use strict\";return 7;};var o=f();";
    let ir = parse_call_initializer_ir(source);
    let root =
        heap_helper_index(&ir, HEAP_ROOT_WRITE_NAME).expect("actual generic root_write helper");
    let undefined =
        heap_helper_index(&ir, "__compat_es5_undefined").expect("actual undefined constructor");
    // Accepted original: the generic four-argument retirement protocol.
    let root_decl = ir
        .functions
        .iter()
        .find(|function| function.index == root)
        .expect("generic root_write declaration");
    assert!(!root_decl.exported, "the generic root_write stays private");
    assert_eq!(
        root_decl.arity, 4,
        "the generic root_write keeps its four slots"
    );
    assert_eq!(
        root_decl.return_schema,
        Some(vm::TypeSchema::Int),
        "the generic root_write returns Int"
    );
    assert!(
        matches!(&root_decl.return_type, vm::ValueType::Int),
        "the generic root_write result type is Int, never Int-or-Float"
    );
    assert_eq!(
        root_decl.arg_schemas,
        vec![
            Some(vm::TypeSchema::Named(
                "__CompatRefCarrier".to_owned(),
                Vec::new()
            )),
            Some(vm::TypeSchema::Int),
            Some(vm::TypeSchema::Int),
            Some(vm::TypeSchema::Named(
                "__CompatEs5Primitive".to_owned(),
                Vec::new()
            )),
        ],
        "the ref carrier, frame and entry Int slots, then the private primitive record"
    );
    let root_impl = ir
        .function_impls
        .get(&root)
        .expect("actual generic root_write body");
    assert_eq!(root_impl.param_slots.len(), 4, "four real parameter slots");
    assert!(
        root_impl.capture_copies.is_empty(),
        "the generic root_write captures nothing"
    );
    // Accepted original: the inlined undefined record constructor.
    let undefined_decl = ir
        .functions
        .iter()
        .find(|function| function.index == undefined)
        .expect("undefined constructor declaration");
    assert_eq!(
        undefined_decl.arity, 0,
        "the undefined constructor takes no parameters"
    );
    assert!(
        undefined_decl.arg_schemas.is_empty(),
        "the undefined constructor declares no arguments"
    );
    assert_eq!(
        undefined_decl.return_schema,
        Some(vm::TypeSchema::Named(
            "__CompatEs5Primitive".to_owned(),
            Vec::new()
        )),
        "the undefined constructor returns the private primitive record schema"
    );
    let undefined_impl = ir
        .function_impls
        .get(&undefined)
        .expect("actual undefined constructor body");
    assert!(undefined_impl.param_slots.is_empty(), "no parameter slots");
    assert!(undefined_impl.capture_copies.is_empty(), "no captures");
    assert!(
        undefined_impl.body_stmts.is_empty(),
        "the record stays inlined, not built by statements"
    );
    // Exactly the literal undefined record: MapNew plus the five literal Sets
    // the representation layer already emits, in emission order.
    let mut sets = Vec::new();
    let mut node = &undefined_impl.body_expr;
    while let vm::Expr::Call(target, types, args, None, None) = node {
        if *target != vm::BuiltinFunction::Set.call_index() || !types.is_empty() || args.len() != 3
        {
            break;
        }
        let vm::Expr::String(key) = &args[1] else {
            break;
        };
        sets.push((key.as_str(), &args[2]));
        node = &args[0];
    }
    let keys: Vec<&str> = sets.iter().map(|(key, _)| *key).collect();
    assert_eq!(
        keys,
        vec!["generation", "ref_slot", "flag", "number", "tag"],
        "five literal fields in emission order"
    );
    assert!(
        matches!(sets[0].1, vm::Expr::Int(0)),
        "generation starts at 0"
    );
    assert!(
        matches!(sets[1].1, vm::Expr::Int(0)),
        "ref_slot starts at 0"
    );
    assert!(
        matches!(sets[2].1, vm::Expr::Bool(false)),
        "the undefined flag is false"
    );
    let vm::Expr::Float(number) = sets[3].1 else {
        panic!("the undefined number is a Float literal");
    };
    // Canonical +0.0: IEEE bits must be all zero, so a negative zero cannot
    // stand in for the canonical number cell.
    assert_eq!(
        number.to_bits(),
        0,
        "the undefined number is canonical +0.0"
    );
    assert!(
        matches!(sets[4].1, vm::Expr::Int(0)),
        "the undefined tag is UNDEFINED (0)"
    );
    assert!(
        matches!(node, vm::Expr::Call(target, _, args, None, None)
            if *target == vm::BuiltinFunction::MapNew.call_index() && args.is_empty()),
        "the literal record starts from MapNew"
    );
    // Real call sites: only statements of the lowered program are walked, so
    // the helper bodies above can never stand in for a caller.
    let mut tree = Vec::new();
    heap_tree_stmts(&ir.stmts, &mut tree);
    let old_generic_clears = tree
        .iter()
        .copied()
        .filter(|stmt| {
            let vm::Stmt::Let {
                expr: vm::Expr::Call(target, types, args, None, None),
                ..
            } = stmt
            else {
                return false;
            };
            *target == root
                && types.is_empty()
                && args.len() == 4
                && matches!(&args[3], vm::Expr::Call(helper, helper_types, helper_args, None, None)
                    if *helper == undefined
                        && helper_types.is_empty()
                        && helper_args.is_empty())
        })
        .count();
    eprintln!("KNOWN_UNDEFINED_ROOT_CLEAR_IR case=main old_generic_clears={old_generic_clears}");
    assert_eq!(
        old_generic_clears, 0,
        "compiler-owned canonical undefined clears use the 3-arg clear helper"
    );
    known_undefined_root_clear_contract(&ir, "main", true);
}

#[test]
fn known_undefined_root_clear_regex_source_contract() {
    // One production frontend parse: zero Core programs, zero VMs. A fresh
    // RegExp slot is initialized here, so its canonical clear must not be
    // dropped by the root guard.
    let source = "var r=/a/;";
    let ir = parse_call_initializer_ir(source);
    let root =
        heap_helper_index(&ir, HEAP_ROOT_WRITE_NAME).expect("actual generic root_write helper");
    let undefined =
        heap_helper_index(&ir, "__compat_es5_undefined").expect("actual undefined constructor");
    // Accepted original: the generic four-argument retirement protocol.
    let root_decl = ir
        .functions
        .iter()
        .find(|function| function.index == root)
        .expect("generic root_write declaration");
    assert!(!root_decl.exported, "the generic root_write stays private");
    assert_eq!(
        root_decl.arity, 4,
        "the generic root_write keeps its four slots"
    );
    assert_eq!(
        root_decl.return_schema,
        Some(vm::TypeSchema::Int),
        "the generic root_write returns Int"
    );
    assert!(
        matches!(&root_decl.return_type, vm::ValueType::Int),
        "the generic root_write result type is Int, never Int-or-Float"
    );
    assert_eq!(
        root_decl.arg_schemas,
        vec![
            Some(vm::TypeSchema::Named(
                "__CompatRefCarrier".to_owned(),
                Vec::new()
            )),
            Some(vm::TypeSchema::Int),
            Some(vm::TypeSchema::Int),
            Some(vm::TypeSchema::Named(
                "__CompatEs5Primitive".to_owned(),
                Vec::new()
            )),
        ],
        "the ref carrier, frame and entry Int slots, then the private primitive record"
    );
    let root_impl = ir
        .function_impls
        .get(&root)
        .expect("actual generic root_write body");
    assert_eq!(root_impl.param_slots.len(), 4, "four real parameter slots");
    assert!(
        root_impl.capture_copies.is_empty(),
        "the generic root_write captures nothing"
    );
    // Accepted original: the inlined undefined record constructor.
    let undefined_decl = ir
        .functions
        .iter()
        .find(|function| function.index == undefined)
        .expect("undefined constructor declaration");
    assert_eq!(
        undefined_decl.arity, 0,
        "the undefined constructor takes no parameters"
    );
    assert!(
        undefined_decl.arg_schemas.is_empty(),
        "the undefined constructor declares no arguments"
    );
    assert_eq!(
        undefined_decl.return_schema,
        Some(vm::TypeSchema::Named(
            "__CompatEs5Primitive".to_owned(),
            Vec::new()
        )),
        "the undefined constructor returns the private primitive record schema"
    );
    let undefined_impl = ir
        .function_impls
        .get(&undefined)
        .expect("actual undefined constructor body");
    assert!(undefined_impl.param_slots.is_empty(), "no parameter slots");
    assert!(undefined_impl.capture_copies.is_empty(), "no captures");
    assert!(
        undefined_impl.body_stmts.is_empty(),
        "the record stays inlined, not built by statements"
    );
    let mut sets = Vec::new();
    let mut node = &undefined_impl.body_expr;
    while let vm::Expr::Call(target, types, args, None, None) = node {
        if *target != vm::BuiltinFunction::Set.call_index() || !types.is_empty() || args.len() != 3
        {
            break;
        }
        let vm::Expr::String(key) = &args[1] else {
            break;
        };
        sets.push((key.as_str(), &args[2]));
        node = &args[0];
    }
    let keys: Vec<&str> = sets.iter().map(|(key, _)| *key).collect();
    assert_eq!(
        keys,
        vec!["generation", "ref_slot", "flag", "number", "tag"],
        "five literal fields in emission order"
    );
    assert!(
        matches!(sets[0].1, vm::Expr::Int(0)),
        "generation starts at 0"
    );
    assert!(
        matches!(sets[1].1, vm::Expr::Int(0)),
        "ref_slot starts at 0"
    );
    assert!(
        matches!(sets[2].1, vm::Expr::Bool(false)),
        "the undefined flag is false"
    );
    let vm::Expr::Float(number) = sets[3].1 else {
        panic!("the undefined number is a Float literal");
    };
    // Canonical +0.0: IEEE bits must be all zero, so a negative zero cannot
    // stand in for the canonical number cell.
    assert_eq!(
        number.to_bits(),
        0,
        "the undefined number is canonical +0.0"
    );
    assert!(
        matches!(sets[4].1, vm::Expr::Int(0)),
        "the undefined tag is UNDEFINED (0)"
    );
    assert!(
        matches!(node, vm::Expr::Call(target, _, args, None, None)
            if *target == vm::BuiltinFunction::MapNew.call_index() && args.is_empty()),
        "the literal record starts from MapNew"
    );
    let mut tree = Vec::new();
    heap_tree_stmts(&ir.stmts, &mut tree);
    let old_generic_clears = tree
        .iter()
        .copied()
        .filter(|stmt| {
            let vm::Stmt::Let {
                expr: vm::Expr::Call(target, types, args, None, None),
                ..
            } = stmt
            else {
                return false;
            };
            *target == root
                && types.is_empty()
                && args.len() == 4
                && matches!(&args[3], vm::Expr::Call(helper, helper_types, helper_args, None, None)
                    if *helper == undefined
                        && helper_types.is_empty()
                        && helper_args.is_empty())
        })
        .count();
    eprintln!("KNOWN_UNDEFINED_ROOT_CLEAR_IR case=regex old_generic_clears={old_generic_clears}");
    assert_eq!(
        old_generic_clears, 0,
        "compiler-owned canonical undefined clears use the 3-arg clear helper"
    );
    known_undefined_root_clear_contract(&ir, "regex", false);
}
