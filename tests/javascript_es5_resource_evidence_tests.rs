//! Interpreter-only minimum proofs for metadata fatals, growth quota, and roots.
//! Fuel stays the ordinary object bound. No native path and no history-fuel change.
use pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN;
use vm::{BuiltinFunction, HostFunctionRegistry, Value, VmStatus};

const FUEL: u64 = 10_000_000;
const PACKED_ILLEGAL: f64 = 1028.0;
const DATA_META_ILLEGAL: f64 = 88.0;
/// Tag 0, bits 9. Legal accessor bits are {8, 10, 12, 14}; 9 is within the
/// meta ceiling, so the caller union assert fires rather than the max assert.
const ACCESSOR_RESIDUE_ILLEGAL: f64 = 9.0;
const GROW_ROWS: i64 = 64;
const QUOTA_ROWS: i64 = 257;
const ROOT_OBJECT: f64 = 5.0;
const STATE_ACTIVE: i64 = 2;
const STATE_FREE: i64 = 3;
const GC_INTERVAL_ALLOCATIONS: i64 = 32;

#[derive(Clone, Copy)]
enum Proof {
    Packed,
    DataMeta,
    AccessorResidue,
    DecodeInt(f64),
    DecodeU32(f64),
    Growth,
    Refusal,
    Control,
    Retain,
    Cycle,
    Snapshot,
}

struct Evidence(Proof);

fn call(index: u16, args: Vec<vm::Expr>) -> vm::Expr {
    vm::Expr::Call(index, Vec::new(), args, None, None)
}

fn number(expr: vm::Expr) -> vm::Expr {
    vm::Expr::Div(Box::new(expr), Box::new(vm::Expr::Float(1.0)))
}

fn field(index: u16, name: &str) -> vm::Expr {
    call(
        BuiltinFunction::Get.call_index(),
        vec![
            vm::Expr::Borrow(Box::new(vm::Expr::Var(index))),
            vm::Expr::String(name.into()),
        ],
    )
}

fn helper(ir: &vm::FrontendIr, name: &str) -> u16 {
    ir.functions
        .iter()
        .find(|function| function.name == name)
        .unwrap_or_else(|| panic!("missing helper {name}"))
        .index
}

fn fresh(ir: &mut vm::FrontendIr) -> u16 {
    let index = u16::try_from(ir.locals).expect("slot");
    ir.locals += 1;
    index
}

fn binding(ir: &vm::FrontendIr, name: &str) -> u16 {
    ir.local_bindings
        .iter()
        .find(|(found, _)| found == name)
        .unwrap_or_else(|| panic!("missing binding {name}"))
        .1
}

fn carrier_slot(ir: &vm::FrontendIr) -> u16 {
    if let Some((_, index)) = ir
        .local_bindings
        .iter()
        .find(|(name, _)| name == "__compat_es5_realm")
    {
        return *index;
    }
    let make = helper(ir, "__compat_ref_make");
    ir.stmts
        .iter()
        .find_map(|stmt| match stmt {
            vm::Stmt::Let {
                index,
                expr: vm::Expr::Call(target, _, _, _, _),
                ..
            } if *target == make => Some(*index),
            _ => None,
        })
        .expect("real carrier")
}

fn push(stmts: &mut Vec<vm::Stmt>, expr: vm::Expr) {
    stmts.push(vm::Stmt::Expr { expr, line: 1 });
}

fn bind(ir: &mut vm::FrontendIr, schema: vm::TypeSchema, expr: vm::Expr) -> u16 {
    let index = fresh(ir);
    ir.stmts.push(vm::Stmt::Let {
        index,
        declared_schema: Some(schema),
        expr,
        line: 1,
    });
    index
}

fn read_at(read: u16, carrier: u16, addr: vm::Expr) -> vm::Expr {
    call(read, vec![vm::Expr::Var(carrier), number(addr)])
}

fn dec(decode: u16, expr: vm::Expr) -> vm::Expr {
    call(decode, vec![expr])
}

fn add(left: vm::Expr, right: i64) -> vm::Expr {
    vm::Expr::Add(Box::new(left), Box::new(vm::Expr::Int(right)))
}

fn undefined_value() -> vm::Expr {
    let mut result = call(BuiltinFunction::MapNew.call_index(), Vec::new());
    for (key, value) in [
        ("tag", vm::Expr::Int(0)),
        ("number", vm::Expr::Float(0.0)),
        ("flag", vm::Expr::Bool(false)),
        ("ref_slot", vm::Expr::Int(0)),
        ("generation", vm::Expr::Int(0)),
    ] {
        result = call(
            BuiltinFunction::Set.call_index(),
            vec![result, vm::Expr::String(key.into()), value],
        );
    }
    result
}

fn object_base(ir: &mut vm::FrontendIr, checked: u16, carrier: u16, object: u16) -> u16 {
    bind(
        ir,
        vm::TypeSchema::Int,
        call(
            checked,
            vec![
                vm::Expr::Var(carrier),
                field(object, "ref_slot"),
                field(object, "generation"),
            ],
        ),
    )
}

fn header_int(ir: &mut vm::FrontendIr, read: u16, decode: u16, carrier: u16, cell: f64) -> u16 {
    let raw = read_at(read, carrier, vm::Expr::Float(cell));
    bind(ir, vm::TypeSchema::Int, dec(decode, raw))
}

fn observe_int(
    ir: &mut vm::FrontendIr,
    observe: u16,
    carrier: u16,
    selector: i64,
    slot: vm::Expr,
) -> u16 {
    bind(
        ir,
        vm::TypeSchema::Int,
        call(
            observe,
            vec![vm::Expr::Var(carrier), vm::Expr::Int(selector), slot],
        ),
    )
}

struct Tools {
    read: u16,
    write: u16,
    checked: u16,
    decode: u16,
    decode_u32: u16,
    observe: u16,
    reserve: u16,
    own: u16,
    collect: u16,
    root_write: u16,
    carrier: u16,
}

fn tools(ir: &vm::FrontendIr) -> Tools {
    Tools {
        read: helper(ir, "__compat_ref_read"),
        write: helper(ir, "__compat_ref_write"),
        checked: helper(ir, "__compat_es5_checked_base"),
        decode: helper(ir, "__compat_es5_decode_int"),
        decode_u32: helper(ir, "__compat_es5_decode_u32"),
        observe: helper(ir, "__compat_es5_heap_observe"),
        reserve: helper(ir, "__compat_es5_object_reserve_rows"),
        own: helper(ir, "__compat_es5_get_own_property"),
        collect: helper(ir, "__compat_es5_heap_collect"),
        root_write: helper(ir, "__compat_es5_root_write"),
        carrier: carrier_slot(ir),
    }
}

fn emit_metadata(ir: &mut vm::FrontendIr, proof: Proof) {
    let tools = tools(ir);
    let object = binding(ir, "o");
    let key = binding(ir, "k");
    let illegal = match proof {
        Proof::Packed => PACKED_ILLEGAL,
        Proof::DataMeta => DATA_META_ILLEGAL,
        Proof::AccessorResidue => ACCESSOR_RESIDUE_ILLEGAL,
        _ => unreachable!(),
    };
    let offset = if matches!(proof, Proof::Packed) { 2 } else { 7 };
    push(&mut ir.stmts, vm::Expr::String("P7_EV_METADATA".into()));
    push(&mut ir.stmts, vm::Expr::Float(illegal));
    let base = object_base(ir, tools.checked, tools.carrier, object);
    push(&mut ir.stmts, vm::Expr::Var(base));
    let sink = fresh(ir);
    ir.stmts.push(vm::Stmt::Let {
        index: sink,
        declared_schema: Some(vm::TypeSchema::Number),
        expr: call(
            tools.write,
            vec![
                vm::Expr::Var(tools.carrier),
                number(add(vm::Expr::Var(base), offset)),
                vm::Expr::Float(illegal),
            ],
        ),
        line: 1,
    });
    let owned = fresh(ir);
    ir.stmts.push(vm::Stmt::Let {
        index: owned,
        declared_schema: None,
        expr: call(
            tools.own,
            vec![
                vm::Expr::Var(tools.carrier),
                vm::Expr::Var(object),
                vm::Expr::Var(key),
            ],
        ),
        line: 1,
    });
}

fn emit_decode(ir: &mut vm::FrontendIr, name: &str, value: f64) {
    let decode = helper(ir, name);
    push(&mut ir.stmts, vm::Expr::String("P7_EV_DECODE".into()));
    push(&mut ir.stmts, vm::Expr::String(name.into()));
    let sink = fresh(ir);
    ir.stmts.push(vm::Stmt::Let {
        index: sink,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: call(decode, vec![vm::Expr::Float(value)]),
        line: 1,
    });
}

fn emit_accounting(ir: &mut vm::FrontendIr, tools: &Tools, object: u16) -> u16 {
    push(&mut ir.stmts, vm::Expr::String("P7_EV_GROWTH".into()));
    let slot = bind(
        ir,
        vm::TypeSchema::Int,
        dec(tools.decode, number(field(object, "ref_slot"))),
    );
    let generation = bind(
        ir,
        vm::TypeSchema::Int,
        dec(tools.decode, number(field(object, "generation"))),
    );
    let base = object_base(ir, tools.checked, tools.carrier, object);
    let size = bind(
        ir,
        vm::TypeSchema::Int,
        dec(
            tools.decode,
            read_at(tools.read, tools.carrier, add(vm::Expr::Var(base), -3)),
        ),
    );
    let cells = observe_int(ir, tools.observe, tools.carrier, 7, vm::Expr::Int(0));
    let free_head = header_int(ir, tools.read, tools.decode, tools.carrier, 2.0);
    let page = header_int(ir, tools.read, tools.decode, tools.carrier, 16.0);
    let row = bind(
        ir,
        vm::TypeSchema::Int,
        vm::Expr::Add(
            Box::new(vm::Expr::Add(
                Box::new(vm::Expr::Var(page)),
                Box::new(vm::Expr::Int(3)),
            )),
            Box::new(vm::Expr::Mul(
                Box::new(vm::Expr::Var(slot)),
                Box::new(vm::Expr::Int(7)),
            )),
        ),
    );
    let phys = bind(
        ir,
        vm::TypeSchema::Int,
        dec(
            tools.decode,
            read_at(tools.read, tools.carrier, add(vm::Expr::Var(row), 2)),
        ),
    );
    let data = bind(
        ir,
        vm::TypeSchema::Int,
        dec(
            tools.decode,
            read_at(tools.read, tools.carrier, add(vm::Expr::Var(row), 3)),
        ),
    );
    let collections = observe_int(ir, tools.observe, tools.carrier, 5, vm::Expr::Int(0));
    let live = observe_int(ir, tools.observe, tools.carrier, 0, vm::Expr::Int(0));
    let pages = observe_int(ir, tools.observe, tools.carrier, 9, vm::Expr::Int(0));
    let backing = observe_int(ir, tools.observe, tools.carrier, 10, vm::Expr::Int(0));
    for index in [
        slot,
        generation,
        base,
        size,
        cells,
        free_head,
        phys,
        data,
        collections,
        live,
        pages,
        backing,
    ] {
        push(&mut ir.stmts, vm::Expr::Var(index));
    }
    base
}

fn emit_growth(ir: &mut vm::FrontendIr, proof: Proof) {
    let tools = tools(ir);
    let object = binding(ir, "o");
    let alias = binding(ir, "alias");
    let base = emit_accounting(ir, &tools, object);
    if matches!(proof, Proof::Control) {
        push(&mut ir.stmts, vm::Expr::String("P7_EV_CONTROL_DONE".into()));
        return;
    }
    let required = if matches!(proof, Proof::Refusal) {
        QUOTA_ROWS
    } else {
        GROW_ROWS
    };
    let sink = fresh(ir);
    ir.stmts.push(vm::Stmt::Let {
        index: sink,
        declared_schema: Some(vm::TypeSchema::Null),
        expr: call(
            tools.reserve,
            vec![
                vm::Expr::Var(tools.carrier),
                vm::Expr::Var(object),
                vm::Expr::Int(required),
            ],
        ),
        line: 1,
    });
    push(&mut ir.stmts, vm::Expr::String("P7_EV_GROWTH_POST".into()));
    let new_base = object_base(ir, tools.checked, tools.carrier, object);
    let new_size = bind(
        ir,
        vm::TypeSchema::Int,
        dec(
            tools.decode,
            read_at(tools.read, tools.carrier, add(vm::Expr::Var(new_base), -3)),
        ),
    );
    let new_cells = observe_int(ir, tools.observe, tools.carrier, 7, vm::Expr::Int(0));
    let old_class = bind(
        ir,
        vm::TypeSchema::Int,
        dec(
            tools.decode,
            read_at(tools.read, tools.carrier, add(vm::Expr::Var(base), -2)),
        ),
    );
    let new_free = header_int(ir, tools.read, tools.decode, tools.carrier, 2.0);
    let alias_slot = bind(
        ir,
        vm::TypeSchema::Int,
        dec(tools.decode, number(field(alias, "ref_slot"))),
    );
    let alias_gen = bind(
        ir,
        vm::TypeSchema::Int,
        dec(tools.decode, number(field(alias, "generation"))),
    );
    let slot = bind(
        ir,
        vm::TypeSchema::Int,
        dec(tools.decode, number(field(object, "ref_slot"))),
    );
    let page = header_int(ir, tools.read, tools.decode, tools.carrier, 16.0);
    let row = bind(
        ir,
        vm::TypeSchema::Int,
        vm::Expr::Add(
            Box::new(vm::Expr::Add(
                Box::new(vm::Expr::Var(page)),
                Box::new(vm::Expr::Int(3)),
            )),
            Box::new(vm::Expr::Mul(
                Box::new(vm::Expr::Var(slot)),
                Box::new(vm::Expr::Int(7)),
            )),
        ),
    );
    let phys = bind(
        ir,
        vm::TypeSchema::Int,
        dec(
            tools.decode,
            read_at(tools.read, tools.carrier, add(vm::Expr::Var(row), 2)),
        ),
    );
    let data = bind(
        ir,
        vm::TypeSchema::Int,
        dec(
            tools.decode,
            read_at(tools.read, tools.carrier, add(vm::Expr::Var(row), 3)),
        ),
    );
    for index in [
        new_base, new_size, new_cells, old_class, new_free, alias_slot, alias_gen, slot, phys, data,
    ] {
        push(&mut ir.stmts, vm::Expr::Var(index));
    }
}

fn string_units(ir: &mut vm::FrontendIr, tools: &Tools, object_base_slot: u16, row_offset: i64) {
    let value_slot = bind(
        ir,
        vm::TypeSchema::Int,
        dec(
            tools.decode,
            read_at(
                tools.read,
                tools.carrier,
                add(vm::Expr::Var(object_base_slot), row_offset + 5),
            ),
        ),
    );
    let value_gen = bind(
        ir,
        vm::TypeSchema::Int,
        call(
            tools.decode_u32,
            vec![read_at(
                tools.read,
                tools.carrier,
                add(vm::Expr::Var(object_base_slot), row_offset + 6),
            )],
        ),
    );
    let text = bind(
        ir,
        vm::TypeSchema::Int,
        call(
            tools.checked,
            vec![
                vm::Expr::Var(tools.carrier),
                vm::Expr::Var(value_slot),
                vm::Expr::Var(value_gen),
            ],
        ),
    );
    let length = bind(
        ir,
        vm::TypeSchema::Int,
        dec(
            tools.decode,
            read_at(tools.read, tools.carrier, add(vm::Expr::Var(text), 2)),
        ),
    );
    push(&mut ir.stmts, vm::Expr::Var(length));
    for unit in 0..4 {
        let cell = bind(
            ir,
            vm::TypeSchema::Int,
            dec(
                tools.decode,
                read_at(
                    tools.read,
                    tools.carrier,
                    add(vm::Expr::Var(text), 3 + unit),
                ),
            ),
        );
        push(&mut ir.stmts, vm::Expr::Var(cell));
    }
}

fn clear_matching_roots(ir: &mut vm::FrontendIr, tools: &Tools, slot: u16, generation: u16) {
    let frame = header_int(ir, tools.read, tools.decode, tools.carrier, 3.0);
    let capacity = bind(
        ir,
        vm::TypeSchema::Int,
        dec(
            tools.decode,
            read_at(tools.read, tools.carrier, add(vm::Expr::Var(frame), 3)),
        ),
    );
    let empty = bind(
        ir,
        vm::TypeSchema::Named("__CompatEs5Primitive".into(), Vec::new()),
        undefined_value(),
    );
    for entry in 0..32 {
        let origin = vm::Expr::Add(
            Box::new(vm::Expr::Var(frame)),
            Box::new(vm::Expr::Int(4 + 3 * entry)),
        );
        let category = read_at(tools.read, tools.carrier, origin.clone());
        let stored_slot = read_at(tools.read, tools.carrier, add(origin.clone(), 1));
        let stored_gen = read_at(tools.read, tools.carrier, add(origin, 2));
        let condition = vm::Expr::And(
            Box::new(vm::Expr::Lt(
                Box::new(vm::Expr::Int(entry)),
                Box::new(vm::Expr::Var(capacity)),
            )),
            Box::new(vm::Expr::And(
                Box::new(vm::Expr::Eq(
                    Box::new(category),
                    Box::new(vm::Expr::Float(ROOT_OBJECT)),
                )),
                Box::new(vm::Expr::And(
                    Box::new(vm::Expr::Eq(
                        Box::new(stored_slot),
                        Box::new(number(vm::Expr::Var(slot))),
                    )),
                    Box::new(vm::Expr::Eq(
                        Box::new(stored_gen),
                        Box::new(number(vm::Expr::Var(generation))),
                    )),
                )),
            )),
        );
        let ack = fresh(ir);
        ir.stmts.push(vm::Stmt::IfElse {
            condition,
            then_branch: vec![vm::Stmt::Let {
                index: ack,
                declared_schema: Some(vm::TypeSchema::Int),
                expr: call(
                    tools.root_write,
                    vec![
                        vm::Expr::Var(tools.carrier),
                        vm::Expr::Var(frame),
                        vm::Expr::Int(entry),
                        vm::Expr::Var(empty),
                    ],
                ),
                line: 1,
            }],
            else_branch: Vec::new(),
            line: 1,
        });
    }
}

fn emit_retain(ir: &mut vm::FrontendIr, cycle: bool) {
    let tools = tools(ir);
    let kept = binding(ir, "kept");
    push(&mut ir.stmts, vm::Expr::String("P7_EV_ROOT".into()));
    let slot = bind(
        ir,
        vm::TypeSchema::Int,
        dec(tools.decode, number(field(kept, "ref_slot"))),
    );
    let generation = bind(
        ir,
        vm::TypeSchema::Int,
        dec(tools.decode, number(field(kept, "generation"))),
    );
    let base = object_base(ir, tools.checked, tools.carrier, kept);
    push(&mut ir.stmts, vm::Expr::Var(slot));
    push(&mut ir.stmts, vm::Expr::Var(generation));
    push(&mut ir.stmts, vm::Expr::Var(base));
    if !cycle {
        string_units(ir, &tools, base, 5);
    } else {
        let peer_slot = bind(
            ir,
            vm::TypeSchema::Int,
            dec(
                tools.decode,
                read_at(tools.read, tools.carrier, add(vm::Expr::Var(base), 17)),
            ),
        );
        let peer_gen = bind(
            ir,
            vm::TypeSchema::Int,
            call(
                tools.decode_u32,
                vec![read_at(
                    tools.read,
                    tools.carrier,
                    add(vm::Expr::Var(base), 18),
                )],
            ),
        );
        let peer_base = bind(
            ir,
            vm::TypeSchema::Int,
            call(
                tools.checked,
                vec![
                    vm::Expr::Var(tools.carrier),
                    vm::Expr::Var(peer_slot),
                    vm::Expr::Var(peer_gen),
                ],
            ),
        );
        for index in [peer_slot, peer_gen, peer_base] {
            push(&mut ir.stmts, vm::Expr::Var(index));
        }
    }
    let live = observe_int(ir, tools.observe, tools.carrier, 0, vm::Expr::Int(0));
    let collections = observe_int(ir, tools.observe, tools.carrier, 5, vm::Expr::Int(0));
    let state = observe_int(ir, tools.observe, tools.carrier, 13, vm::Expr::Var(slot));
    let roots = observe_int(ir, tools.observe, tools.carrier, 12, vm::Expr::Int(0));
    for index in [live, collections, state, roots] {
        push(&mut ir.stmts, vm::Expr::Var(index));
    }
    let held = fresh(ir);
    ir.stmts.push(vm::Stmt::Let {
        index: held,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: call(tools.collect, vec![vm::Expr::Var(tools.carrier)]),
        line: 1,
    });
    push(&mut ir.stmts, vm::Expr::String("P7_EV_ROOT_HELD".into()));
    let held_base = object_base(ir, tools.checked, tools.carrier, kept);
    let held_live = observe_int(ir, tools.observe, tools.carrier, 0, vm::Expr::Int(0));
    let held_collections = observe_int(ir, tools.observe, tools.carrier, 5, vm::Expr::Int(0));
    let held_state = observe_int(ir, tools.observe, tools.carrier, 13, vm::Expr::Var(slot));
    for index in [held_base, held_live, held_collections, held_state] {
        push(&mut ir.stmts, vm::Expr::Var(index));
    }
    clear_matching_roots(ir, &tools, slot, generation);
    let reclaimed = fresh(ir);
    ir.stmts.push(vm::Stmt::Let {
        index: reclaimed,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: call(tools.collect, vec![vm::Expr::Var(tools.carrier)]),
        line: 1,
    });
    push(&mut ir.stmts, vm::Expr::String("P7_EV_ROOT_DROPPED".into()));
    let dropped_live = observe_int(ir, tools.observe, tools.carrier, 0, vm::Expr::Int(0));
    let dropped_collections = observe_int(ir, tools.observe, tools.carrier, 5, vm::Expr::Int(0));
    let dropped_state = observe_int(ir, tools.observe, tools.carrier, 13, vm::Expr::Var(slot));
    for index in [dropped_live, dropped_collections, dropped_state, reclaimed] {
        push(&mut ir.stmts, vm::Expr::Var(index));
    }
}

const SCRATCH_STARTED: i64 = 61;
const SCRATCH_ARM: i64 = 62;
const SCRATCH_PEAK: i64 = 63;

fn inject_snapshot(ir: &mut vm::FrontendIr) {
    let read = helper(ir, "__compat_ref_read");
    let write = helper(ir, "__compat_ref_write");
    let decode = helper(ir, "__compat_es5_decode_int");
    let observe = helper(ir, "__compat_es5_heap_observe");
    let collect = helper(ir, "__compat_es5_heap_collect");
    let root_write = helper(ir, "__compat_es5_root_write");
    let define = helper(ir, "__compat_es5_define_properties");
    let to_desc = helper(ir, "__compat_es5_to_property_descriptor");
    let leave = helper(ir, "__compat_es5_root_leave");
    let write_carrier = ir
        .function_impls
        .get(&root_write)
        .expect("root write")
        .param_slots[0];
    let update = scratch_peak_update(ir, write_carrier, read, write, decode, observe);
    ir.function_impls
        .get_mut(&root_write)
        .expect("root write")
        .body_stmts
        .extend(update);
    for function in [define, to_desc] {
        let carrier = ir
            .function_impls
            .get(&function)
            .expect("helper")
            .param_slots[0];
        let leaves: Vec<usize> = ir
            .function_impls
            .get(&function)
            .expect("helper")
            .body_stmts
            .iter()
            .enumerate()
            .filter_map(|(index, stmt)| match stmt {
                vm::Stmt::Assign {
                    expr: vm::Expr::Call(target, _, _, _, _),
                    ..
                }
                | vm::Stmt::Expr {
                    expr: vm::Expr::Call(target, _, _, _, _),
                    ..
                } if *target == leave => Some(index),
                _ => None,
            })
            .collect();
        assert!(!leaves.is_empty(), "helper must leave its root frame");
        let samples: Vec<_> = leaves
            .iter()
            .map(|_| scratch_peak_update(ir, carrier, read, write, decode, observe))
            .collect::<Vec<_>>();
        let disarms: Vec<_> = leaves
            .iter()
            .map(|_| scratch_set(ir, carrier, read, write, decode, observe, SCRATCH_ARM, 0))
            .collect::<Vec<_>>();
        let collects: Vec<_> = leaves
            .iter()
            .map(|_| {
                let sink = fresh(ir);
                vm::Stmt::Let {
                    index: sink,
                    declared_schema: Some(vm::TypeSchema::Int),
                    expr: call(collect, vec![vm::Expr::Var(carrier)]),
                    line: 1,
                }
            })
            .collect::<Vec<_>>();
        let body = &mut ir
            .function_impls
            .get_mut(&function)
            .expect("helper")
            .body_stmts;
        for (at, ((sample, disarm), collect_stmt)) in leaves
            .into_iter()
            .zip(samples.into_iter().zip(disarms).zip(collects))
            .rev()
        {
            for stmt in disarm.into_iter().rev() {
                body.insert(at + 1, stmt);
            }
            body.insert(at, collect_stmt);
            for stmt in sample.into_iter().rev() {
                body.insert(at, stmt);
            }
        }
    }
    let define_carrier = ir.function_impls.get(&define).expect("define").param_slots[0];
    let mut prologue = scratch_set(
        ir,
        define_carrier,
        read,
        write,
        decode,
        observe,
        SCRATCH_ARM,
        1,
    );
    prologue.extend(scratch_set(
        ir,
        define_carrier,
        read,
        write,
        decode,
        observe,
        SCRATCH_PEAK,
        0,
    ));
    prologue.extend(scratch_copy_collections(
        ir,
        define_carrier,
        read,
        write,
        decode,
        observe,
    ));
    let body = &mut ir
        .function_impls
        .get_mut(&define)
        .expect("define")
        .body_stmts;
    for (offset, stmt) in prologue.into_iter().enumerate() {
        body.insert(offset, stmt);
    }
}

fn page_base(
    ir: &mut vm::FrontendIr,
    carrier: u16,
    read: u16,
    decode: u16,
) -> (Vec<vm::Stmt>, u16) {
    let raw = fresh(ir);
    let page = fresh(ir);
    (
        vec![
            vm::Stmt::Let {
                index: raw,
                declared_schema: Some(vm::TypeSchema::Number),
                expr: read_at(read, carrier, vm::Expr::Float(16.0)),
                line: 1,
            },
            vm::Stmt::Let {
                index: page,
                declared_schema: Some(vm::TypeSchema::Int),
                expr: call(decode, vec![vm::Expr::Var(raw)]),
                line: 1,
            },
        ],
        page,
    )
}

fn mark_addr(page: u16, slot: i64) -> vm::Expr {
    vm::Expr::Add(
        Box::new(vm::Expr::Var(page)),
        Box::new(vm::Expr::Int(7 + slot * 7)),
    )
}

#[allow(clippy::too_many_arguments)]
fn scratch_set(
    ir: &mut vm::FrontendIr,
    carrier: u16,
    read: u16,
    write: u16,
    decode: u16,
    observe: u16,
    slot: i64,
    value: i64,
) -> Vec<vm::Stmt> {
    let (mut stmts, page) = page_base(ir, carrier, read, decode);
    let state = fresh(ir);
    let sink = fresh(ir);
    stmts.push(vm::Stmt::Let {
        index: state,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: call(
            observe,
            vec![
                vm::Expr::Var(carrier),
                vm::Expr::Int(13),
                vm::Expr::Int(slot),
            ],
        ),
        line: 1,
    });
    stmts.push(vm::Stmt::Expr {
        expr: call(
            BuiltinFunction::Assert.call_index(),
            vec![vm::Expr::Eq(
                Box::new(vm::Expr::Var(state)),
                Box::new(vm::Expr::Int(0)),
            )],
        ),
        line: 1,
    });
    stmts.push(vm::Stmt::Let {
        index: sink,
        declared_schema: Some(vm::TypeSchema::Number),
        expr: call(
            write,
            vec![
                vm::Expr::Var(carrier),
                number(mark_addr(page, slot)),
                vm::Expr::Float(value as f64),
            ],
        ),
        line: 1,
    });
    stmts
}

#[allow(clippy::too_many_arguments)]
fn scratch_copy_collections(
    ir: &mut vm::FrontendIr,
    carrier: u16,
    read: u16,
    write: u16,
    decode: u16,
    observe: u16,
) -> Vec<vm::Stmt> {
    let (mut stmts, page) = page_base(ir, carrier, read, decode);
    let state = fresh(ir);
    let started = fresh(ir);
    let sink = fresh(ir);
    stmts.push(vm::Stmt::Let {
        index: state,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: call(
            observe,
            vec![
                vm::Expr::Var(carrier),
                vm::Expr::Int(13),
                vm::Expr::Int(SCRATCH_STARTED),
            ],
        ),
        line: 1,
    });
    stmts.push(vm::Stmt::Expr {
        expr: call(
            BuiltinFunction::Assert.call_index(),
            vec![vm::Expr::Eq(
                Box::new(vm::Expr::Var(state)),
                Box::new(vm::Expr::Int(0)),
            )],
        ),
        line: 1,
    });
    stmts.push(vm::Stmt::Let {
        index: started,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: call(
            observe,
            vec![vm::Expr::Var(carrier), vm::Expr::Int(5), vm::Expr::Int(0)],
        ),
        line: 1,
    });
    stmts.push(vm::Stmt::Let {
        index: sink,
        declared_schema: Some(vm::TypeSchema::Number),
        expr: call(
            write,
            vec![
                vm::Expr::Var(carrier),
                number(mark_addr(page, SCRATCH_STARTED)),
                number(vm::Expr::Var(started)),
            ],
        ),
        line: 1,
    });
    stmts
}

#[allow(clippy::too_many_arguments)]
fn scratch_peak_update(
    ir: &mut vm::FrontendIr,
    carrier: u16,
    read: u16,
    write: u16,
    decode: u16,
    observe: u16,
) -> Vec<vm::Stmt> {
    let (mut stmts, page) = page_base(ir, carrier, read, decode);
    let arm = fresh(ir);
    let current = fresh(ir);
    let stored = fresh(ir);
    let sink = fresh(ir);
    stmts.push(vm::Stmt::Let {
        index: arm,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: call(
            decode,
            vec![read_at(read, carrier, mark_addr(page, SCRATCH_ARM))],
        ),
        line: 1,
    });
    stmts.push(vm::Stmt::IfElse {
        condition: vm::Expr::Eq(Box::new(vm::Expr::Var(arm)), Box::new(vm::Expr::Int(1))),
        then_branch: vec![
            vm::Stmt::Let {
                index: current,
                declared_schema: Some(vm::TypeSchema::Int),
                expr: call(
                    observe,
                    vec![vm::Expr::Var(carrier), vm::Expr::Int(12), vm::Expr::Int(0)],
                ),
                line: 1,
            },
            vm::Stmt::Let {
                index: stored,
                declared_schema: Some(vm::TypeSchema::Int),
                expr: call(
                    decode,
                    vec![read_at(read, carrier, mark_addr(page, SCRATCH_PEAK))],
                ),
                line: 1,
            },
            vm::Stmt::IfElse {
                condition: vm::Expr::Lt(
                    Box::new(vm::Expr::Var(stored)),
                    Box::new(vm::Expr::Var(current)),
                ),
                then_branch: vec![vm::Stmt::Let {
                    index: sink,
                    declared_schema: Some(vm::TypeSchema::Number),
                    expr: call(
                        write,
                        vec![
                            vm::Expr::Var(carrier),
                            number(mark_addr(page, SCRATCH_PEAK)),
                            number(vm::Expr::Var(current)),
                        ],
                    ),
                    line: 1,
                }],
                else_branch: Vec::new(),
                line: 1,
            },
        ],
        else_branch: Vec::new(),
        line: 1,
    });
    stmts
}

fn emit_snapshot(ir: &mut vm::FrontendIr) {
    let tools = tools(ir);
    let target = binding(ir, "target");
    inject_snapshot(ir);
    push(&mut ir.stmts, vm::Expr::String("P7_EV_SNAP".into()));
    let page = bind(
        ir,
        vm::TypeSchema::Int,
        dec(
            tools.decode,
            read_at(tools.read, tools.carrier, vm::Expr::Float(16.0)),
        ),
    );
    let peak = bind(
        ir,
        vm::TypeSchema::Int,
        dec(
            tools.decode,
            read_at(tools.read, tools.carrier, mark_addr(page, SCRATCH_PEAK)),
        ),
    );
    let started = bind(
        ir,
        vm::TypeSchema::Int,
        dec(
            tools.decode,
            read_at(tools.read, tools.carrier, mark_addr(page, SCRATCH_STARTED)),
        ),
    );
    let peak_state = observe_int(
        ir,
        tools.observe,
        tools.carrier,
        13,
        vm::Expr::Int(SCRATCH_PEAK),
    );
    push(&mut ir.stmts, vm::Expr::Var(peak));
    push(&mut ir.stmts, vm::Expr::Var(started));
    push(&mut ir.stmts, vm::Expr::Var(page));
    push(&mut ir.stmts, vm::Expr::Var(peak_state));
    let capacity = observe_int(ir, tools.observe, tools.carrier, 4, vm::Expr::Int(0));
    let active = observe_int(ir, tools.observe, tools.carrier, 12, vm::Expr::Int(0));
    let collections = observe_int(ir, tools.observe, tools.carrier, 5, vm::Expr::Int(0));
    let slot = bind(
        ir,
        vm::TypeSchema::Int,
        dec(tools.decode, number(field(target, "ref_slot"))),
    );
    let state = observe_int(ir, tools.observe, tools.carrier, 13, vm::Expr::Var(slot));
    let base = object_base(ir, tools.checked, tools.carrier, target);
    for index in [capacity, active, collections, slot, state, base] {
        push(&mut ir.stmts, vm::Expr::Var(index));
    }
}

impl vm::SourcePlugin for Evidence {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut descriptor = vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN);
        descriptor.id = vm::FrontendId::new("javascript-es5-resource-evidence");
        descriptor
    }

    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
        match self.0 {
            Proof::Packed | Proof::DataMeta | Proof::AccessorResidue => {
                emit_metadata(&mut ir, self.0)
            }
            Proof::DecodeInt(_) => {
                emit_decode(&mut ir, "__compat_es5_decode_int", decode_value(self.0))
            }
            Proof::DecodeU32(_) => {
                emit_decode(&mut ir, "__compat_es5_decode_u32", decode_value(self.0))
            }
            Proof::Growth | Proof::Refusal | Proof::Control => emit_growth(&mut ir, self.0),
            Proof::Retain => emit_retain(&mut ir, false),
            Proof::Cycle => emit_retain(&mut ir, true),
            Proof::Snapshot => emit_snapshot(&mut ir),
        }
        Ok(ir)
    }
}

fn decode_value(proof: Proof) -> f64 {
    match proof {
        Proof::DecodeInt(value) | Proof::DecodeU32(value) => value,
        _ => unreachable!(),
    }
}

struct Outcome {
    stack: Vec<Value>,
    error: Option<vm::VmError>,
    reserved_bytes: Option<usize>,
}

fn run(source: &str, proof: Proof) -> Outcome {
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-resource-evidence")),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(Box::leak(Box::new(Evidence(proof))))
            .with_host_api_catalog(catalog.clone()),
    )
    .unwrap_or_else(|error| panic!("evidence source must compile: {source}\n{error:?}"));
    let mut machine = compiled.into_vm().expect("certified VM");
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..vm::JitConfig::default()
    });
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        Default::default(),
    )
    .expect("owner");
    let mut registry = HostFunctionRegistry::restricted();
    pd_vm_compat_frontends::runtime::array::typescript_array_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("arrays");
    pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("owners");
    pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("snapshots");
    pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("math");
    pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("errors");
    registry.allow_builtin("math::floor").expect("floor");
    // The ordinary realm publishes its fixed ASCII strings through the Core
    // bytes initializer, so the bootstrap call needs this exact capability.
    // This is one explicit permission on this test's restricted registry; no
    // other capability, refusal case or fixture expectation changes.
    registry
        .allow_builtin("bytes::from_utf8")
        .expect("bootstrap strings");
    registry.bind_vm_cached(&mut machine).expect("bind");
    machine.set_fuel(FUEL);
    let status = machine.run();
    let aot = machine.aot_exec_count();
    assert_eq!(aot, 0, "interpreter only");
    assert_eq!(machine.jit_native_exec_count(), 0, "no trace execution");
    let reserved_bytes =
        pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
            .map(|stats| stats.reserved_bytes);
    let stack = machine.stack().to_vec();
    let error = match status {
        Ok(VmStatus::Halted) => None,
        Ok(other) => panic!("unexpected status {other:?} stack={stack:?}"),
        Err(error) => Some(error),
    };
    Outcome {
        stack,
        error,
        reserved_bytes,
    }
}

fn assertion_failed(error: &vm::VmError) -> bool {
    match error {
        vm::VmError::HostError(message) => {
            message.contains("assertion failed")
                && !message.contains("TypeError")
                && message != "uncaught JavaScript throw"
        }
        _ => false,
    }
}

fn marker<'a>(stack: &'a [Value], text: &str) -> &'a [Value] {
    let at = stack
        .iter()
        .position(|value| *value == Value::string(text))
        .unwrap_or_else(|| panic!("missing {text} in {stack:?}"));
    &stack[at + 1..]
}

fn ints_before_tail(values: &[Value]) -> Vec<i64> {
    values
        .iter()
        .take_while(|value| matches!(value, Value::Int(_)))
        .map(expect_int)
        .collect()
}

fn boxed_field(value: &Value, name: &str) -> Value {
    let Value::Map(map) = value else {
        panic!("boxed value: {value:?}")
    };
    map.get(&Value::string(name)).expect("boxed field").clone()
}

fn expect_int(value: &Value) -> i64 {
    match value {
        Value::Int(number) => *number,
        other => panic!("expected int, got {other:?}"),
    }
}

fn expect_fatal(source: &str, proof: Proof, label: &str) {
    let outcome = run(source, proof);
    let error = outcome.error.expect("VM fatal");
    assert!(
        assertion_failed(&error),
        "{label} must be VM assertion, not TypeError or harness panic: {error:?}\nstack={:?}",
        outcome.stack
    );
    assert!(
        outcome
            .stack
            .iter()
            .any(|value| *value == Value::string("P7_EV_METADATA")
                || *value == Value::string("P7_EV_DECODE")
                || *value == Value::string("P7_EV_GROWTH")),
        "{label} must reach VM execution before the fatal, stack={:?}",
        outcome.stack
    );
    eprintln!(
        "P7_EV2_FATAL label={label} error={error:?} reserved={:?} tail={:?}",
        outcome.reserved_bytes,
        outcome.stack.iter().rev().take(8).collect::<Vec<_>>()
    );
}

#[test]
fn p7_ev2_metadata_fatal_interpreter() {
    let object = "var o={x:1};var k=\"x\";";
    expect_fatal(object, Proof::Packed, "packed-1028");
    expect_fatal(object, Proof::DataMeta, "data-meta-88");
    expect_fatal(object, Proof::AccessorResidue, "accessor-residue-9");
    let decode = "var o={};";
    for (name, value) in [
        ("fraction", 1.5),
        ("nan", f64::NAN),
        ("pos-inf", f64::INFINITY),
        ("neg-inf", f64::NEG_INFINITY),
    ] {
        expect_fatal(
            decode,
            Proof::DecodeInt(value),
            &format!("decode-int-{name}"),
        );
        expect_fatal(
            decode,
            Proof::DecodeU32(value),
            &format!("decode-u32-{name}"),
        );
    }
}

#[test]
fn p7_ev2_growth_quota_interpreter() {
    let source = "var o={x:1};var alias=o;";
    let grown = run(source, Proof::Growth);
    assert!(
        grown.error.is_none(),
        "growth must commit, got {:?} stack={:?}",
        grown.error,
        grown.stack
    );
    let before = marker(&grown.stack, "P7_EV_GROWTH");
    let after = marker(&grown.stack, "P7_EV_GROWTH_POST");
    let (slot, generation, base, old_size, old_cells, _free, old_phys, _old_data) = (
        expect_int(&before[0]),
        expect_int(&before[1]),
        expect_int(&before[2]),
        expect_int(&before[3]),
        expect_int(&before[4]),
        expect_int(&before[5]),
        expect_int(&before[6]),
        expect_int(&before[7]),
    );
    let (
        new_base,
        new_size,
        new_cells,
        old_class,
        new_free,
        alias_slot,
        alias_gen,
        slot_after,
        phys,
        data,
    ) = (
        expect_int(&after[0]),
        expect_int(&after[1]),
        expect_int(&after[2]),
        expect_int(&after[3]),
        expect_int(&after[4]),
        expect_int(&after[5]),
        expect_int(&after[6]),
        expect_int(&after[7]),
        expect_int(&after[8]),
        expect_int(&after[9]),
    );
    assert!(slot < 64, "page-0 directory formula, slot={slot}");
    assert_eq!(
        (alias_slot, alias_gen, slot_after),
        (slot, generation, slot),
        "identity alias"
    );
    assert_ne!(
        new_base, base,
        "physical base must change; old_size={old_size} new_size={new_size}"
    );
    assert_eq!(old_phys, base, "directory base before growth");
    assert_eq!(phys, new_base, "directory publishes the new base");
    assert_eq!(old_class, 0, "old block class is free");
    assert_eq!(new_free, base - 3, "free head is the old raw block");
    assert_eq!(
        new_cells - old_cells,
        new_size - old_size,
        "node-cell delta equals raw extent delta"
    );
    assert_eq!(
        data,
        new_size - 3,
        "published data cells equal the new payload"
    );
    eprintln!(
        "P7_EV2_GROWTH slot={slot} gen={generation} base={base}->{new_base} size={old_size}->{new_size} cells={old_cells}->{new_cells} class={old_class} free={new_free} data={data} reserved={:?}",
        grown.reserved_bytes
    );

    let control = run(source, Proof::Control);
    let refusal = run(source, Proof::Refusal);
    assert!(
        control.error.is_none(),
        "control must halt: {:?}",
        control.error
    );
    let refusal_error = refusal.error.as_ref().expect("quota refusal fatal");
    assert!(
        assertion_failed(refusal_error),
        "quota refusal must be VM assertion, not TypeError: {refusal_error:?}"
    );
    assert!(
        refusal
            .stack
            .iter()
            .any(|value| *value == Value::string("P7_EV_GROWTH"))
    );
    assert!(
        !refusal
            .stack
            .iter()
            .any(|value| *value == Value::string("P7_EV_GROWTH_POST")),
        "refusal must not reach post-commit"
    );
    let control_prefix = marker(&control.stack, "P7_EV_GROWTH");
    let refusal_prefix = marker(&refusal.stack, "P7_EV_GROWTH");
    let control_ints = ints_before_tail(control_prefix);
    let refusal_ints = ints_before_tail(refusal_prefix);
    assert_eq!(
        refusal_ints, control_ints,
        "refusal must leave object, directory, and request accounting unchanged"
    );
    assert_eq!(
        refusal.reserved_bytes, control.reserved_bytes,
        "post-refusal owner bytes must match the completed control"
    );
    eprintln!(
        "P7_EV2_REFUSAL reserved_control={:?} reserved_refusal={:?} prefix={refusal_prefix:?}",
        control.reserved_bytes, refusal.reserved_bytes
    );
}

#[test]
fn p7_ev2_root_gc_interpreter() {
    let retained = run(
        "var holder={get x(){\"use strict\";return {only:\"edge\"};}};var kept=holder.x;",
        Proof::Retain,
    );
    assert!(
        retained.error.is_none(),
        "retain path: {:?} stack={:?}",
        retained.error,
        retained.stack
    );
    let root = marker(&retained.stack, "P7_EV_ROOT");
    let held = marker(&retained.stack, "P7_EV_ROOT_HELD");
    let dropped = marker(&retained.stack, "P7_EV_ROOT_DROPPED");
    let (slot, generation, base, length) = (
        expect_int(&root[0]),
        expect_int(&root[1]),
        expect_int(&root[2]),
        expect_int(&root[3]),
    );
    let units: Vec<i64> = (0..4).map(|index| expect_int(&root[4 + index])).collect();
    assert_eq!(length, 4, "UTF-16 length of the only-edge string");
    assert_eq!(units, vec![101, 100, 103, 101], "UTF-16 units of edge");
    assert_eq!(expect_int(&root[10]), STATE_ACTIVE);
    assert_eq!(
        expect_int(&held[3]),
        STATE_ACTIVE,
        "return edge retains the object across collection"
    );
    assert_eq!(
        expect_int(&held[0]),
        base,
        "checked base still resolves while the return edge is held"
    );
    assert!(
        expect_int(&held[2]) > expect_int(&root[9]),
        "held collection is a real increment"
    );
    assert_eq!(
        expect_int(&dropped[2]),
        STATE_FREE,
        "dropped only-edge is reclaimed"
    );
    assert!(
        expect_int(&dropped[0]) < expect_int(&held[1]),
        "live nodes drop after the only edge is cleared"
    );
    assert!(
        expect_int(&dropped[1]) > expect_int(&held[2]),
        "collection count increases"
    );
    assert!(
        expect_int(&dropped[3]) >= 1,
        "collector reports reclaimed nodes"
    );
    eprintln!(
        "P7_EV2_ROOT_RETAIN slot={slot} gen={generation} base={base} units={units:?} live={}=>{}=>{} collections={}=>{}=>{} reclaimed={}",
        expect_int(&root[8]),
        expect_int(&held[1]),
        expect_int(&dropped[0]),
        expect_int(&root[9]),
        expect_int(&held[2]),
        expect_int(&dropped[1]),
        expect_int(&dropped[3])
    );

    let cycle = run(
        "var holder={get x(){\"use strict\";var a={n:1};var b={n:2};a.p=b;b.p=a;return a;}};var kept=holder.x;",
        Proof::Cycle,
    );
    assert!(
        cycle.error.is_none(),
        "cycle path: {:?} stack={:?}",
        cycle.error,
        cycle.stack
    );
    let before = marker(&cycle.stack, "P7_EV_ROOT");
    let after = marker(&cycle.stack, "P7_EV_ROOT_DROPPED");
    let peer_base = expect_int(&before[5]);
    assert!(
        peer_base > 0,
        "getter-created peer resolves while the return edge is held"
    );
    assert_eq!(
        expect_int(&after[2]),
        STATE_FREE,
        "getter cycle is reclaimed with the return edge"
    );
    assert!(expect_int(&after[0]) < expect_int(&marker(&cycle.stack, "P7_EV_ROOT_HELD")[1]));
    eprintln!(
        "P7_EV2_ROOT_CYCLE kept={} peer_slot={} peer_gen={} peer_base={} dropped_live={} dropped_state={}",
        expect_int(&before[0]),
        expect_int(&before[3]),
        expect_int(&before[4]),
        peer_base,
        expect_int(&after[0]),
        expect_int(&after[2])
    );

    let snapshot = run(
        "var target={keep:1};var desc={get value(){\"use strict\";return 7;}};Object.defineProperties(target,{added:desc});target.added===7&&target.keep===1;",
        Proof::Snapshot,
    );
    assert!(
        snapshot.error.is_none(),
        "snapshot path: {:?} stack={:?}",
        snapshot.error,
        snapshot.stack
    );
    let snap_at = snapshot
        .stack
        .iter()
        .position(|value| *value == Value::string("P7_EV_SNAP"))
        .expect("snapshot marker");
    let completion = &snapshot.stack[snap_at - 1];
    assert_eq!(
        boxed_field(completion, "tag"),
        Value::Int(2),
        "defineProperties completion is boolean"
    );
    assert_eq!(
        boxed_field(completion, "flag"),
        Value::Bool(true),
        "snapshot key and target survived the getter"
    );
    let tail = marker(&snapshot.stack, "P7_EV_SNAP");
    let (peak, started, page, peak_state, capacity, active, collections, state) = (
        expect_int(&tail[0]),
        expect_int(&tail[1]),
        expect_int(&tail[2]),
        expect_int(&tail[3]),
        expect_int(&tail[4]),
        expect_int(&tail[5]),
        expect_int(&tail[6]),
        expect_int(&tail[8]),
    );
    assert_eq!(
        peak_state, 0,
        "peak scratch row must stay never-allocated; page={page}"
    );
    assert!(
        collections > started,
        "getter must cross the {GC_INTERVAL_ALLOCATIONS}-allocation interval; started={started} collections={collections}"
    );
    assert_eq!(
        state, STATE_ACTIVE,
        "target remains after collection inside to_desc"
    );
    // The frame capacity here is the one-row `defineProperties` request,
    // 8 + 4 * 1 = 12; `assert_ne!(peak, capacity)` below is the effective
    // check, so pinning a 1032 figure here would only suggest that the
    // 256-row runtime maximum is this frame's enter capacity.
    assert_ne!(peak, capacity, "peak is not the capacity counter");
    assert!(
        peak > active,
        "helper-chain peak exceeds the post-return active count"
    );
    assert!(peak > 0, "measured peak");
    eprintln!(
        "P7_EV2_ROOT_PEAK peak={peak} started_collections={started} page={page} scratch_state={peak_state} capacity={capacity} active_after={active} collections={collections} state={state} reserved={:?}",
        snapshot.reserved_bytes
    );
}
