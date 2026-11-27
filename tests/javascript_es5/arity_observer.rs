//! Test-only reads of real produced IR locals/vectors; never evaluate JS ASTs.
use vm::{BuiltinFunction as B, Expr, FrontendIr, ParseError, SourcePlugin, Stmt, TypeSchema};

#[derive(Default)]
pub struct Observer {
    pub reads: Vec<String>,
    pub string: Option<(String, usize)>,
    pub vector_numbers: Option<Vec<f64>>,
    pub collect: bool,
    pub force_gc: bool,
    pub stats: bool,
    /// Push each source-call Completion kind/has_value from the real call
    /// block. Default off so existing tails stay unchanged.
    pub completion: bool,
    /// Extra `__compat_es5_heap_observe` selectors pushed at the script tail,
    /// in the given order. Selector 6 is allocations-since-GC, 5 collections,
    /// 0 live nodes, 12 active roots; see `src/javascript/heap.rs`.
    pub extra_selectors: Vec<i64>,
    /// Abort through the real builtin `assert` as soon as the real
    /// `__compat_es5_node_begin` helper is entered for this node kind. Node
    /// begin is observed at helper entry, before its guards and before any
    /// collection, so this sees a reservation *attempt* rather than a
    /// post-collection live count. The control arm proves the trigger fires;
    /// the same trigger over a throwing source proves that kind is never
    /// reserved after an abrupt completion, even transiently, because entry
    /// itself would abort.
    pub fail_on_kind: Option<i64>,
    /// Test-only lifetime proof: push `OBSERVE_ROW_STATE` (13) and
    /// `OBSERVE_ROW_GENERATION` (14) for the named script binding's own
    /// `ref_slot`, plus that `ref_slot`/`generation` pair as plain tail
    /// values. The identity originates from the real node producer (the
    /// live value's own fields); the surviving control qualifies it by
    /// units, and the recorded pair is then inspected in the paired
    /// reclamation/owner-drop arm. Emitted after the `string` block so
    /// existing tails are unchanged when this is unset.
    pub row_of_binding: Option<String>,
    /// Tail `ref_slot`/`generation` pushes for the named script binding.
    /// Paired with `row_of_binding`; parsed from the very end of the
    /// stack by the new lifetime tests only.
    pub push_binding_identity: Option<String>,
    /// Reclamation/owner-drop arm for a producer-measured old identity:
    /// push row-state/row-generation for `slot`, then run the existing
    /// `__compat_es5_checked_base` plus `units` UTF-16 reads on
    /// (`slot`, `generation`). On a reclaimed generation the checked
    /// behavior refuses; the test distinguishes that expected rejection
    /// from ordinary Halted. Script-level only: no function-body edits.
    pub old_identity: Option<(i64, i64, usize)>,
    /// Test-only lifetime measurement in ONE Script activation: the real
    /// token (`ref_slot`, `generation`) of `binding`'s box is copied into two
    /// numeric Core locals immediately AFTER that binding's initializer,
    /// qualified against the real UTF-16 units, and then used for the row and
    /// `checked_base` observations at the script tail of this very execution.
    /// No boxed alias, no additional guest root, no cross-function channel and
    /// no host callback. The optional survivor names a legitimate binding
    /// whose own token/units are read at the tail (kept-binding control).
    pub captured: Option<CapturedToken>,
    /// Test-only owner-drop negative control: rewrite the two
    /// finally-snapshot `__compat_es5_root_write` entries (identified via
    /// the emitted abrupt-restore arm, never by guessed arity or a fixed
    /// slot) to the existing `root_write(undefined)` shape. Must pair
    /// with `force_gc`. This mutation is test-only and must never become
    /// production behavior.
    pub drop_saved_roots: bool,
    /// Default-off for-of iterator lifetime probe. Requires exactly one
    /// numeric-array iterator slot (self-refresh `root_write` plus
    /// `checked_base` of that box's `ref_slot`/`generation` beside a
    /// kind==1 compare). Copies that pair into non-root Int locals after
    /// the kind check, then records a real collect while the source
    /// binding is null and a second collect after the normal clear.
    /// Unset leaves every existing tail unchanged.
    pub array_iterator: bool,
    /// Default-off census of collections (5), active roots (12) and root
    /// capacity (4) at script entry, each numeric-array for-of while
    /// head, and the script tail. Does not require a unique iterator.
    pub array_root_census: bool,
    /// Default-off dense-array header probe: push the raw node cells
    /// (kind, length, prototype slot, prototype generation) of the named
    /// script binding through the real checked_base/read helpers. Used to
    /// prove the physical kind/header/stride of tagged arrays without a
    /// source-visible host shortcut.
    pub array_header: Option<String>,
    /// Default-off: run one real full collection at the script tail BEFORE
    /// the extra selectors, so live-node counts describe the post-collect
    /// reachable set rather than uncollected garbage.
    pub pre_collect: bool,
}

/// One same-program token measurement. The expected units are qualified at
/// capture time; a wrong identity (for example the predeclared Undefined)
/// fails loudly instead of silently proving the wrong slot.
pub struct CapturedToken {
    pub binding: String,
    pub units: Vec<u16>,
    pub survivor: Option<(String, Vec<u16>)>,
}

fn error(message: &str) -> ParseError {
    ParseError::at_span(vm::compiler::source_map::Span::new(0, 0, 0), message)
}
fn call(index: u16, args: Vec<Expr>) -> Expr {
    Expr::Call(index, Vec::new(), args, None, None)
}
fn field(slot: u16, name: &str) -> Expr {
    call(
        B::Get.call_index(),
        vec![Expr::Var(slot), Expr::String(name.into())],
    )
}
/// Field read through a borrow, so the source binding stays usable as a whole
/// afterwards (a plain literal-key `Get` on a named local is registered as a
/// partial field move by the ownership pass).
fn borrowed_field(slot: u16, name: &str) -> Expr {
    call(
        B::Get.call_index(),
        vec![
            Expr::Borrow(Box::new(Expr::Var(slot))),
            Expr::String(name.into()),
        ],
    )
}
fn helper(ir: &FrontendIr, name: &str) -> Result<u16, ParseError> {
    ir.functions
        .iter()
        .find(|f| f.name == name)
        .map(|f| f.index)
        .ok_or_else(|| error(name))
}
fn binding(ir: &FrontendIr, name: &str) -> Result<u16, ParseError> {
    ir.local_bindings
        .iter()
        .rfind(|(n, _)| n == name)
        .map(|(_, s)| *s)
        .ok_or_else(|| error(name))
}
fn fresh(ir: &mut FrontendIr) -> Result<u16, ParseError> {
    let slot = u16::try_from(ir.locals).map_err(|_| error("observer local limit"))?;
    ir.locals += 1;
    Ok(slot)
}
fn carrier(ir: &FrontendIr) -> Result<u16, ParseError> {
    let make = helper(ir, "__compat_ref_make")?;
    ir.stmts
        .iter()
        .find_map(|s| match s {
            Stmt::Let {
                index,
                expr: Expr::Call(target, _, _, _, _),
                ..
            } if *target == make => Some(*index),
            _ => None,
        })
        .ok_or_else(|| error("realm carrier"))
}
fn number(e: Expr) -> Expr {
    Expr::Div(Box::new(e), Box::new(Expr::Float(1.0)))
}
fn assert(e: Expr) -> Stmt {
    Stmt::Expr {
        expr: call(B::Assert.call_index(), vec![e]),
        line: 1,
    }
}

struct ArrayProbe {
    slot: u16,
    entry: i64,
    carrier: u16,
    clears: usize,
}

fn apply_array_iterator_probe(ir: &mut FrontendIr) -> Result<(), ParseError> {
    let root_write = helper(ir, "__compat_es5_root_write")?;
    let checked = helper(ir, "__compat_es5_checked_base")?;
    let observe = helper(ir, "__compat_es5_heap_observe")?;
    let collect = helper(ir, "__compat_es5_heap_collect")?;
    let read = helper(ir, "__compat_ref_read")?;
    let probe = find_array_probe(ir, root_write, checked)?;
    let ref_copy = fresh(ir)?;
    let gen_copy = fresh(ir)?;
    let flag = fresh(ir)?;
    let mut inject = ArrayInject {
        probe,
        ref_copy,
        gen_copy,
        flag,
        observe,
        collect,
        checked,
        read,
        root_write,
        clears_seen: 0,
        nulls: 0,
        captures: 0,
    };
    let make = helper(ir, "__compat_ref_make")?;
    let at = ir
        .stmts
        .iter()
        .position(|stmt| {
            matches!(
                stmt,
                Stmt::Let {
                    expr: Expr::Call(target, _, _, _, _),
                    ..
                } if *target == make
            )
        })
        .ok_or_else(|| error("realm carrier"))?;
    ir.stmts.insert(
        at + 1,
        Stmt::Let {
            index: flag,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line: 1,
        },
    );
    ir.stmts.insert(
        at + 1,
        Stmt::Let {
            index: gen_copy,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line: 1,
        },
    );
    ir.stmts.insert(
        at + 1,
        Stmt::Let {
            index: ref_copy,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line: 1,
        },
    );
    let mut stmts = std::mem::take(&mut ir.stmts);
    inject_array_probe(&mut stmts, ir, &mut inject)?;
    ir.stmts = stmts;
    if inject.captures != 1 || inject.nulls != 1 || inject.clears_seen != inject.probe.clears {
        return Err(error(
            "array iterator probe did not land on the unique refresh, null, and clear",
        ));
    }
    Ok(())
}

struct ArrayInject {
    probe: ArrayProbe,
    ref_copy: u16,
    gen_copy: u16,
    flag: u16,
    observe: u16,
    collect: u16,
    checked: u16,
    read: u16,
    root_write: u16,
    clears_seen: usize,
    nulls: usize,
    captures: usize,
}

fn find_array_probe(
    ir: &FrontendIr,
    root_write: u16,
    checked: u16,
) -> Result<ArrayProbe, ParseError> {
    let mut refreshes = Vec::new();
    let mut guarded = Vec::new();
    collect_array_hits(&ir.stmts, root_write, checked, &mut refreshes, &mut guarded);
    let mut slots: Vec<u16> = refreshes
        .iter()
        .filter(|(slot, _, _)| guarded.contains(slot))
        .map(|(slot, _, _)| *slot)
        .collect();
    slots.sort_unstable();
    slots.dedup();
    if slots.len() != 1 {
        return Err(error(
            "array iterator probe requires exactly one numeric-array for-of root",
        ));
    }
    let slot = slots[0];
    let mut entries: Vec<(i64, u16)> = refreshes
        .iter()
        .filter(|(found, _, _)| *found == slot)
        .map(|(_, entry, carrier)| (*entry, *carrier))
        .collect();
    entries.sort_unstable();
    entries.dedup();
    if entries.len() != 1 {
        return Err(error("array iterator probe: ambiguous root entry"));
    }
    let (entry, carrier) = entries[0];
    let clears = count_clears(&ir.stmts, root_write, entry);
    if clears < 1 {
        return Err(error(
            "array iterator probe: missing the post-loop root clear",
        ));
    }
    if count_nulls(&ir.stmts) != 1 {
        return Err(error(
            "array iterator probe requires exactly one null assignment",
        ));
    }
    Ok(ArrayProbe {
        slot,
        entry,
        carrier,
        clears,
    })
}

fn collect_array_hits(
    stmts: &[Stmt],
    root_write: u16,
    checked: u16,
    refreshes: &mut Vec<(u16, i64, u16)>,
    guarded: &mut Vec<u16>,
) {
    let mut bases = Vec::new();
    let mut kind = false;
    for stmt in stmts {
        if let Some(hit) = self_refresh(stmt, root_write) {
            refreshes.push(hit);
        }
        if let Some(slot) = checked_slot(stmt, checked) {
            bases.push(slot);
        }
        if is_array_kind_guard(stmt) {
            kind = true;
        }
        match stmt {
            Stmt::IfElse {
                then_branch,
                else_branch,
                ..
            } => {
                collect_array_hits(then_branch, root_write, checked, refreshes, guarded);
                collect_array_hits(else_branch, root_write, checked, refreshes, guarded);
            }
            Stmt::For { body, .. } | Stmt::While { body, .. } => {
                collect_array_hits(body, root_write, checked, refreshes, guarded);
            }
            Stmt::Let { expr, .. } | Stmt::Assign { expr, .. } | Stmt::Expr { expr, .. } => {
                collect_array_hits_expr(expr, root_write, checked, refreshes, guarded);
            }
            _ => {}
        }
    }
    if kind {
        guarded.extend(bases);
    }
}

fn collect_array_hits_expr(
    expr: &Expr,
    root_write: u16,
    checked: u16,
    refreshes: &mut Vec<(u16, i64, u16)>,
    guarded: &mut Vec<u16>,
) {
    match expr {
        Expr::Block { stmts, .. } => {
            collect_array_hits(stmts, root_write, checked, refreshes, guarded);
        }
        Expr::IfElse {
            then_expr,
            else_expr,
            ..
        } => {
            collect_array_hits_expr(then_expr, root_write, checked, refreshes, guarded);
            collect_array_hits_expr(else_expr, root_write, checked, refreshes, guarded);
        }
        Expr::Call(_, _, args, _, _) => {
            for arg in args {
                collect_array_hits_expr(arg, root_write, checked, refreshes, guarded);
            }
        }
        _ => {}
    }
}

fn self_refresh(stmt: &Stmt, root_write: u16) -> Option<(u16, i64, u16)> {
    let Stmt::Let {
        expr: Expr::Call(target, _, args, _, _),
        ..
    } = stmt
    else {
        return None;
    };
    if *target != root_write || args.len() != 4 {
        return None;
    }
    match (&args[0], &args[2], &args[3]) {
        (Expr::Var(carrier), Expr::Int(entry), Expr::Var(slot)) => Some((*slot, *entry, *carrier)),
        _ => None,
    }
}

fn checked_slot(stmt: &Stmt, checked: u16) -> Option<u16> {
    let Stmt::Let {
        expr: Expr::Call(target, _, args, _, _),
        ..
    } = stmt
    else {
        return None;
    };
    if *target != checked || args.len() != 3 {
        return None;
    }
    let Expr::Call(_, _, key_args, _, _) = &args[1] else {
        return None;
    };
    let Expr::Call(_, _, gen_args, _, _) = &args[2] else {
        return None;
    };
    match (key_args.as_slice(), gen_args.as_slice()) {
        ([Expr::Var(slot), Expr::String(key)], [Expr::Var(gen_slot), Expr::String(generation)])
            if key == "ref_slot" && generation == "generation" && slot == gen_slot =>
        {
            Some(*slot)
        }
        _ => None,
    }
}

fn is_array_kind_guard(stmt: &Stmt) -> bool {
    let Stmt::IfElse {
        condition,
        then_branch,
        ..
    } = stmt
    else {
        return false;
    };
    let Expr::Eq(left, right) = condition else {
        return false;
    };
    matches!(left.as_ref(), Expr::Var(_))
        && matches!(right.as_ref(), Expr::Int(1))
        && then_branch.iter().any(|item| {
            matches!(
                item,
                Stmt::Assign {
                    expr: Expr::Int(0),
                    ..
                }
            )
        })
}

fn is_null_assign(stmt: &Stmt) -> bool {
    let Stmt::Assign { expr, .. } = stmt else {
        return false;
    };
    let mut tags = Vec::new();
    tag_sets(expr, &mut tags);
    tags == [1]
}

fn tag_sets(expr: &Expr, tags: &mut Vec<i64>) {
    let Expr::Call(_, _, args, _, _) = expr else {
        return;
    };
    if args.len() == 3
        && matches!(&args[1], Expr::String(name) if name == "tag")
        && let Expr::Int(tag) = &args[2]
    {
        tags.push(*tag);
    }
    for arg in args {
        tag_sets(arg, tags);
    }
}

fn count_nulls(stmts: &[Stmt]) -> usize {
    let mut count = 0;
    for stmt in stmts {
        if is_null_assign(stmt) {
            count += 1;
        }
        match stmt {
            Stmt::IfElse {
                then_branch,
                else_branch,
                ..
            } => {
                count += count_nulls(then_branch) + count_nulls(else_branch);
            }
            Stmt::For { body, .. } | Stmt::While { body, .. } => count += count_nulls(body),
            Stmt::Let { expr, .. } | Stmt::Assign { expr, .. } | Stmt::Expr { expr, .. } => {
                count += count_nulls_expr(expr);
            }
            _ => {}
        }
    }
    count
}

fn count_nulls_expr(expr: &Expr) -> usize {
    match expr {
        Expr::Block { stmts, .. } => count_nulls(stmts),
        Expr::IfElse {
            then_expr,
            else_expr,
            ..
        } => count_nulls_expr(then_expr) + count_nulls_expr(else_expr),
        Expr::Call(_, _, args, _, _) => args.iter().map(count_nulls_expr).sum(),
        _ => 0,
    }
}

fn count_clears(stmts: &[Stmt], root_write: u16, entry: i64) -> usize {
    let mut count = 0;
    for stmt in stmts {
        if is_entry_clear(stmt, root_write, entry) {
            count += 1;
        }
        match stmt {
            Stmt::IfElse {
                then_branch,
                else_branch,
                ..
            } => {
                count += count_clears(then_branch, root_write, entry)
                    + count_clears(else_branch, root_write, entry);
            }
            Stmt::For { body, .. } | Stmt::While { body, .. } => {
                count += count_clears(body, root_write, entry);
            }
            Stmt::Let { expr, .. } | Stmt::Assign { expr, .. } | Stmt::Expr { expr, .. } => {
                count += count_clears_expr(expr, root_write, entry);
            }
            _ => {}
        }
    }
    count
}

fn count_clears_expr(expr: &Expr, root_write: u16, entry: i64) -> usize {
    match expr {
        Expr::Block { stmts, .. } => count_clears(stmts, root_write, entry),
        Expr::IfElse {
            then_expr,
            else_expr,
            ..
        } => {
            count_clears_expr(then_expr, root_write, entry)
                + count_clears_expr(else_expr, root_write, entry)
        }
        Expr::Call(_, _, args, _, _) => args
            .iter()
            .map(|arg| count_clears_expr(arg, root_write, entry))
            .sum(),
        _ => 0,
    }
}

fn is_entry_clear(stmt: &Stmt, root_write: u16, entry: i64) -> bool {
    let Stmt::Let {
        expr: Expr::Call(target, _, args, _, _),
        ..
    } = stmt
    else {
        return false;
    };
    *target == root_write
        && args.len() == 4
        && matches!(&args[2], Expr::Int(found) if *found == entry)
        && !matches!(&args[3], Expr::Var(_))
}

fn inject_array_probe(
    stmts: &mut Vec<Stmt>,
    ir: &mut FrontendIr,
    inject: &mut ArrayInject,
) -> Result<(), ParseError> {
    let sibling_base = stmts
        .iter()
        .any(|stmt| checked_slot(stmt, inject.checked) == Some(inject.probe.slot));
    let mut index = 0;
    while index < stmts.len() {
        if sibling_base && is_array_kind_guard(&stmts[index]) {
            let capture = capture_stmts(ir, inject)?;
            if let Stmt::IfElse { then_branch, .. } = &mut stmts[index] {
                then_branch.splice(0..0, capture);
            }
            inject.captures += 1;
        }
        inject_children(&mut stmts[index], ir, inject)?;
        let mut extra = 0;
        if is_null_assign(&stmts[index]) {
            let probe = positive_probe(ir, inject)?;
            extra = probe.len();
            stmts.splice(index + 1..index + 1, probe);
            inject.nulls += 1;
        } else if is_entry_clear(&stmts[index], inject.root_write, inject.probe.entry) {
            inject.clears_seen += 1;
            if inject.clears_seen == inject.probe.clears {
                let probe = negative_probe(ir, inject)?;
                extra = probe.len();
                stmts.splice(index + 1..index + 1, probe);
            }
        }
        index += 1 + extra;
    }
    Ok(())
}

fn inject_children(
    stmt: &mut Stmt,
    ir: &mut FrontendIr,
    inject: &mut ArrayInject,
) -> Result<(), ParseError> {
    match stmt {
        Stmt::IfElse {
            then_branch,
            else_branch,
            ..
        } => {
            inject_array_probe(then_branch, ir, inject)?;
            inject_array_probe(else_branch, ir, inject)?;
        }
        Stmt::For { body, .. } | Stmt::While { body, .. } => {
            inject_array_probe(body, ir, inject)?;
        }
        Stmt::Let { expr, .. } | Stmt::Assign { expr, .. } | Stmt::Expr { expr, .. } => {
            inject_expr(expr, ir, inject)?;
        }
        _ => {}
    }
    Ok(())
}

fn inject_expr(
    expr: &mut Expr,
    ir: &mut FrontendIr,
    inject: &mut ArrayInject,
) -> Result<(), ParseError> {
    match expr {
        Expr::Block { stmts, .. } => inject_array_probe(stmts, ir, inject)?,
        Expr::IfElse {
            then_expr,
            else_expr,
            ..
        } => {
            inject_expr(then_expr, ir, inject)?;
            inject_expr(else_expr, ir, inject)?;
        }
        Expr::Call(_, _, args, _, _) => {
            for arg in args {
                inject_expr(arg, ir, inject)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn capture_stmts(ir: &mut FrontendIr, inject: &ArrayInject) -> Result<Vec<Stmt>, ParseError> {
    let copied_slot = fresh(ir)?;
    let copied_gen = fresh(ir)?;
    let base = fresh(ir)?;
    Ok(vec![
        Stmt::Let {
            index: copied_slot,
            declared_schema: Some(TypeSchema::Int),
            expr: borrowed_field(inject.probe.slot, "ref_slot"),
            line: 1,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: inject.ref_copy,
            expr: Expr::Var(copied_slot),
            line: 1,
        },
        Stmt::Let {
            index: copied_gen,
            declared_schema: Some(TypeSchema::Int),
            expr: borrowed_field(inject.probe.slot, "generation"),
            line: 1,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: inject.gen_copy,
            expr: Expr::Var(copied_gen),
            line: 1,
        },
        Stmt::Let {
            index: base,
            declared_schema: Some(TypeSchema::Int),
            expr: call(
                inject.checked,
                vec![
                    Expr::Var(inject.probe.carrier),
                    Expr::Var(inject.ref_copy),
                    Expr::Var(inject.gen_copy),
                ],
            ),
            line: 1,
        },
        assert(Expr::Eq(
            Box::new(number(call(
                inject.read,
                vec![Expr::Var(inject.probe.carrier), number(Expr::Var(base))],
            ))),
            Box::new(Expr::Float(1.0)),
        )),
    ])
}

fn positive_probe(ir: &mut FrontendIr, inject: &ArrayInject) -> Result<Vec<Stmt>, ParseError> {
    let mut body = vec![Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: inject.flag,
        expr: Expr::Int(1),
        line: 1,
    }];
    push_observe(
        &mut body,
        ir,
        inject.observe,
        inject.probe.carrier,
        5,
        Expr::Int(0),
    )?;
    let ack = fresh(ir)?;
    body.push(Stmt::Let {
        index: ack,
        declared_schema: Some(TypeSchema::Int),
        expr: call(inject.collect, vec![Expr::Var(inject.probe.carrier)]),
        line: 1,
    });
    push_observe(
        &mut body,
        ir,
        inject.observe,
        inject.probe.carrier,
        5,
        Expr::Int(0),
    )?;
    push_observe(
        &mut body,
        ir,
        inject.observe,
        inject.probe.carrier,
        13,
        Expr::Var(inject.ref_copy),
    )?;
    push_observe(
        &mut body,
        ir,
        inject.observe,
        inject.probe.carrier,
        14,
        Expr::Var(inject.gen_copy),
    )?;
    let base = fresh(ir)?;
    body.push(Stmt::Let {
        index: base,
        declared_schema: Some(TypeSchema::Int),
        expr: call(
            inject.checked,
            vec![
                Expr::Var(inject.probe.carrier),
                Expr::Var(inject.ref_copy),
                Expr::Var(inject.gen_copy),
            ],
        ),
        line: 1,
    });
    body.push(Stmt::Expr {
        expr: Expr::Var(base),
        line: 1,
    });
    let length = fresh(ir)?;
    body.push(Stmt::Let {
        index: length,
        declared_schema: Some(TypeSchema::Number),
        expr: number(call(
            inject.read,
            vec![
                Expr::Var(inject.probe.carrier),
                number(Expr::Add(Box::new(Expr::Var(base)), Box::new(Expr::Int(2)))),
            ],
        )),
        line: 1,
    });
    body.push(Stmt::Expr {
        expr: Expr::Var(length),
        line: 1,
    });
    body.push(Stmt::Expr {
        expr: Expr::Var(inject.ref_copy),
        line: 1,
    });
    body.push(Stmt::Expr {
        expr: Expr::Var(inject.gen_copy),
        line: 1,
    });
    Ok(vec![Stmt::IfElse {
        condition: Expr::Eq(Box::new(Expr::Var(inject.flag)), Box::new(Expr::Int(0))),
        then_branch: body,
        else_branch: Vec::new(),
        line: 1,
    }])
}

fn negative_probe(ir: &mut FrontendIr, inject: &ArrayInject) -> Result<Vec<Stmt>, ParseError> {
    let mut body = Vec::new();
    push_observe(
        &mut body,
        ir,
        inject.observe,
        inject.probe.carrier,
        5,
        Expr::Int(0),
    )?;
    let ack = fresh(ir)?;
    body.push(Stmt::Let {
        index: ack,
        declared_schema: Some(TypeSchema::Int),
        expr: call(inject.collect, vec![Expr::Var(inject.probe.carrier)]),
        line: 1,
    });
    push_observe(
        &mut body,
        ir,
        inject.observe,
        inject.probe.carrier,
        5,
        Expr::Int(0),
    )?;
    push_observe(
        &mut body,
        ir,
        inject.observe,
        inject.probe.carrier,
        13,
        Expr::Var(inject.ref_copy),
    )?;
    push_observe(
        &mut body,
        ir,
        inject.observe,
        inject.probe.carrier,
        14,
        Expr::Var(inject.gen_copy),
    )?;
    let base = fresh(ir)?;
    body.push(Stmt::Let {
        index: base,
        declared_schema: Some(TypeSchema::Int),
        expr: call(
            inject.checked,
            vec![
                Expr::Var(inject.probe.carrier),
                Expr::Var(inject.ref_copy),
                Expr::Var(inject.gen_copy),
            ],
        ),
        line: 1,
    });
    Ok(body)
}

fn let_observe(
    out: &mut Vec<Stmt>,
    ir: &mut FrontendIr,
    observe: u16,
    carrier: u16,
    selector: i64,
) -> Result<u16, ParseError> {
    let slot = fresh(ir)?;
    out.push(Stmt::Let {
        index: slot,
        declared_schema: Some(TypeSchema::Int),
        expr: call(
            observe,
            vec![Expr::Var(carrier), Expr::Int(selector), Expr::Int(0)],
        ),
        line: 1,
    });
    Ok(slot)
}

struct CensusSlots {
    start_collections: u16,
    start_active: u16,
    start_capacity: u16,
    count: u16,
    fn_counts: [u16; 3],
    active: [u16; 16],
    capacity: [u16; 16],
}

struct SampleCursor {
    count: u16,
    base: usize,
    limit: usize,
    capture_start: bool,
    published: Option<u16>,
}

fn declare_census(ir: &mut FrontendIr) -> Result<CensusSlots, ParseError> {
    let start_collections = fresh(ir)?;
    let start_active = fresh(ir)?;
    let start_capacity = fresh(ir)?;
    let count = fresh(ir)?;
    let mut fn_counts = [0; 3];
    for slot in &mut fn_counts {
        *slot = fresh(ir)?;
    }
    let mut active = [0; 16];
    let mut capacity = [0; 16];
    for index in 0..16 {
        active[index] = fresh(ir)?;
        capacity[index] = fresh(ir)?;
    }
    Ok(CensusSlots {
        start_collections,
        start_active,
        start_capacity,
        count,
        fn_counts,
        active,
        capacity,
    })
}

fn zero_local(index: u16) -> Stmt {
    Stmt::Let {
        index,
        declared_schema: Some(TypeSchema::Int),
        expr: Expr::Int(0),
        line: 1,
    }
}

fn record_sample(
    out: &mut Vec<Stmt>,
    ir: &mut FrontendIr,
    slots: &CensusSlots,
    observe: u16,
    carrier: u16,
    cursor: &SampleCursor,
) -> Result<(), ParseError> {
    let active = let_observe(out, ir, observe, carrier, 12)?;
    let capacity = let_observe(out, ir, observe, carrier, 4)?;
    if cursor.capture_start {
        let collections = let_observe(out, ir, observe, carrier, 5)?;
        out.push(Stmt::IfElse {
            condition: Expr::Eq(Box::new(Expr::Var(cursor.count)), Box::new(Expr::Int(0))),
            then_branch: vec![
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: slots.start_collections,
                    expr: Expr::Var(collections),
                    line: 1,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: slots.start_active,
                    expr: Expr::Var(active),
                    line: 1,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: slots.start_capacity,
                    expr: Expr::Var(capacity),
                    line: 1,
                },
            ],
            else_branch: Vec::new(),
            line: 1,
        });
    }
    let mut store = Vec::new();
    for index in (0..cursor.limit).rev() {
        let slot = cursor.base + index;
        store = vec![Stmt::IfElse {
            condition: Expr::Eq(
                Box::new(Expr::Var(cursor.count)),
                Box::new(Expr::Int(index as i64)),
            ),
            then_branch: vec![
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: slots.active[slot],
                    expr: Expr::Var(active),
                    line: 1,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: slots.capacity[slot],
                    expr: Expr::Var(capacity),
                    line: 1,
                },
            ],
            else_branch: store,
            line: 1,
        }];
    }
    out.push(Stmt::IfElse {
        condition: Expr::Lt(
            Box::new(Expr::Var(cursor.count)),
            Box::new(Expr::Int(cursor.limit as i64)),
        ),
        then_branch: store,
        else_branch: Vec::new(),
        line: 1,
    });
    out.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: cursor.count,
        expr: Expr::Add(Box::new(Expr::Var(cursor.count)), Box::new(Expr::Int(1))),
        line: 1,
    });
    if let Some(published) = cursor.published {
        out.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: published,
            expr: Expr::Var(cursor.count),
            line: 1,
        });
    }
    Ok(())
}
fn push_observe(
    out: &mut Vec<Stmt>,
    ir: &mut FrontendIr,
    observe: u16,
    carrier: u16,
    selector: i64,
    slot_expr: Expr,
) -> Result<(), ParseError> {
    let slot = fresh(ir)?;
    out.push(Stmt::Let {
        index: slot,
        declared_schema: Some(TypeSchema::Int),
        expr: call(
            observe,
            vec![Expr::Var(carrier), Expr::Int(selector), slot_expr],
        ),
        line: 1,
    });
    out.push(Stmt::Expr {
        expr: Expr::Var(slot),
        line: 1,
    });
    Ok(())
}

fn apply_array_root_census(ir: &mut FrontendIr) -> Result<(), ParseError> {
    let checked = helper(ir, "__compat_es5_checked_base")?;
    let observe = helper(ir, "__compat_es5_heap_observe")?;
    let script_carrier = carrier(ir)?;
    let slots = declare_census(ir)?;
    let mut guarded = Vec::new();
    let mut ignored = Vec::new();
    collect_array_hits(&ir.stmts, 0, checked, &mut ignored, &mut guarded);
    let function_indexes: Vec<u16> = ir.function_impls.keys().copied().collect();
    for index in &function_indexes {
        if let Some(body) = ir.function_impls.get(index) {
            collect_array_hits(&body.body_stmts, 0, checked, &mut ignored, &mut guarded);
        }
    }
    guarded.sort_unstable();
    guarded.dedup();
    if guarded.is_empty() {
        return Err(error("array root census: no numeric-array for-of guard"));
    }
    let script_cursor = SampleCursor {
        count: slots.count,
        base: 0,
        limit: 16,
        capture_start: true,
        published: None,
    };
    let mut instrumented = 0usize;
    let mut script = std::mem::take(&mut ir.stmts);
    instrument_census(
        &mut script,
        ir,
        observe,
        script_carrier,
        checked,
        &guarded,
        &slots,
        &script_cursor,
        &mut instrumented,
    )?;
    ir.stmts = script;
    if instrumented == 0 {
        return Err(error("array root census: no numeric-array for-of while"));
    }
    let make = helper(ir, "__compat_ref_make")?;
    let at = ir
        .stmts
        .iter()
        .position(|stmt| {
            matches!(
                stmt,
                Stmt::Let {
                    expr: Expr::Call(target, _, _, _, _),
                    ..
                } if *target == make
            )
        })
        .ok_or_else(|| error("realm carrier"))?;
    let mut header = vec![
        zero_local(slots.start_collections),
        zero_local(slots.start_active),
        zero_local(slots.start_capacity),
        zero_local(slots.count),
    ];
    for slot in slots.fn_counts {
        header.push(zero_local(slot));
    }
    for index in 0..16 {
        header.push(zero_local(slots.active[index]));
        header.push(zero_local(slots.capacity[index]));
    }
    ir.stmts.splice(at + 1..at + 1, header);
    let mut tail = Vec::new();
    for slot in [
        slots.start_collections,
        slots.start_active,
        slots.start_capacity,
        slots.count,
    ] {
        tail.push(Stmt::Expr {
            expr: Expr::Var(slot),
            line: 1,
        });
    }
    for slot in slots.fn_counts {
        tail.push(Stmt::Expr {
            expr: Expr::Var(slot),
            line: 1,
        });
    }
    for index in 0..16 {
        tail.push(Stmt::Expr {
            expr: Expr::Var(slots.active[index]),
            line: 1,
        });
        tail.push(Stmt::Expr {
            expr: Expr::Var(slots.capacity[index]),
            line: 1,
        });
    }
    for selector in [5, 12, 4] {
        push_observe(
            &mut tail,
            ir,
            observe,
            script_carrier,
            selector,
            Expr::Int(0),
        )?;
    }
    ir.stmts.extend(tail);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn instrument_census(
    stmts: &mut [Stmt],
    ir: &mut FrontendIr,
    observe: u16,
    carrier: u16,
    checked: u16,
    guarded: &[u16],
    slots: &CensusSlots,
    cursor: &SampleCursor,
    instrumented: &mut usize,
) -> Result<(), ParseError> {
    let mut index = 0;
    while index < stmts.len() {
        let sample = if let Stmt::While { body, .. } = &stmts[index] {
            body_uses_guarded(body, checked, guarded)
        } else {
            false
        };
        if sample {
            let mut recorded = Vec::new();
            record_sample(&mut recorded, ir, slots, observe, carrier, cursor)?;
            if let Stmt::While { body, .. } = &mut stmts[index] {
                let mut combined = recorded;
                combined.append(body);
                *body = combined;
            }
            *instrumented += 1;
        }
        match &mut stmts[index] {
            Stmt::IfElse {
                then_branch,
                else_branch,
                ..
            } => {
                instrument_census(
                    then_branch,
                    ir,
                    observe,
                    carrier,
                    checked,
                    guarded,
                    slots,
                    cursor,
                    instrumented,
                )?;
                instrument_census(
                    else_branch,
                    ir,
                    observe,
                    carrier,
                    checked,
                    guarded,
                    slots,
                    cursor,
                    instrumented,
                )?;
            }
            Stmt::For { body, .. } | Stmt::While { body, .. } => {
                instrument_census(
                    body,
                    ir,
                    observe,
                    carrier,
                    checked,
                    guarded,
                    slots,
                    cursor,
                    instrumented,
                )?;
            }
            Stmt::Let { expr, .. } | Stmt::Assign { expr, .. } | Stmt::Expr { expr, .. } => {
                instrument_census_expr(
                    expr,
                    ir,
                    observe,
                    carrier,
                    checked,
                    guarded,
                    slots,
                    cursor,
                    instrumented,
                )?;
            }
            _ => {}
        }
        index += 1;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn instrument_census_expr(
    expr: &mut Expr,
    ir: &mut FrontendIr,
    observe: u16,
    carrier: u16,
    checked: u16,
    guarded: &[u16],
    slots: &CensusSlots,
    cursor: &SampleCursor,
    instrumented: &mut usize,
) -> Result<(), ParseError> {
    match expr {
        Expr::Block { stmts, .. } => {
            instrument_census(
                stmts,
                ir,
                observe,
                carrier,
                checked,
                guarded,
                slots,
                cursor,
                instrumented,
            )?;
        }
        Expr::IfElse {
            then_expr,
            else_expr,
            ..
        } => {
            instrument_census_expr(
                then_expr,
                ir,
                observe,
                carrier,
                checked,
                guarded,
                slots,
                cursor,
                instrumented,
            )?;
            instrument_census_expr(
                else_expr,
                ir,
                observe,
                carrier,
                checked,
                guarded,
                slots,
                cursor,
                instrumented,
            )?;
        }
        Expr::Call(_, _, args, _, _) => {
            for arg in args {
                instrument_census_expr(
                    arg,
                    ir,
                    observe,
                    carrier,
                    checked,
                    guarded,
                    slots,
                    cursor,
                    instrumented,
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn body_uses_guarded(stmts: &[Stmt], checked: u16, guarded: &[u16]) -> bool {
    stmts.iter().any(|stmt| {
        checked_slot(stmt, checked).is_some_and(|slot| guarded.contains(&slot))
            || match stmt {
                Stmt::IfElse {
                    then_branch,
                    else_branch,
                    ..
                } => {
                    body_uses_guarded(then_branch, checked, guarded)
                        || body_uses_guarded(else_branch, checked, guarded)
                }
                Stmt::For { body, .. } | Stmt::While { body, .. } => {
                    body_uses_guarded(body, checked, guarded)
                }
                Stmt::Let { expr, .. } | Stmt::Assign { expr, .. } | Stmt::Expr { expr, .. } => {
                    expr_uses_guarded(expr, checked, guarded)
                }
                _ => false,
            }
    })
}

fn expr_uses_guarded(expr: &Expr, checked: u16, guarded: &[u16]) -> bool {
    match expr {
        Expr::Block { stmts, .. } => body_uses_guarded(stmts, checked, guarded),
        Expr::IfElse {
            then_expr,
            else_expr,
            ..
        } => {
            expr_uses_guarded(then_expr, checked, guarded)
                || expr_uses_guarded(else_expr, checked, guarded)
        }
        Expr::Call(_, _, args, _, _) => args
            .iter()
            .any(|arg| expr_uses_guarded(arg, checked, guarded)),
        _ => false,
    }
}

impl SourcePlugin for Observer {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-arity-observe"),
            extensions: &["js"],
            typing_policy: vm::FrontendTypingPolicy::Strict,
            supports_ownership_checking: true,
        }
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<FrontendIr, ParseError> {
        let mut ir = pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        if self.force_gc {
            // Force the existing allocation quantum at entry to the real
            // node_begin helper, matching the maintained safepoint control.
            let begin = helper(&ir, "__compat_es5_node_begin")?;
            let write = helper(&ir, "__compat_ref_write")?;
            let slot = fresh(&mut ir)?;
            let body = ir
                .function_impls
                .get_mut(&begin)
                .ok_or_else(|| error("node begin"))?;
            let c = body.param_slots[0];
            body.body_stmts.insert(
                0,
                Stmt::Let {
                    index: slot,
                    declared_schema: Some(TypeSchema::Number),
                    expr: call(
                        write,
                        vec![Expr::Var(c), Expr::Float(10.0), Expr::Float(32.0)],
                    ),
                    line: 1,
                },
            );
        }
        if let Some(kind) = self.fail_on_kind {
            // Trigger on entry to the real reservation helper for one node
            // kind, before its guards and before any collection. Entry is the
            // earliest possible observation of an allocation attempt, so this
            // sees a transient reservation that a post-GC live count hides.
            let begin = helper(&ir, "__compat_es5_node_begin")?;
            let param_kind = ir
                .function_impls
                .get(&begin)
                .ok_or_else(|| error("node begin"))?
                .param_slots
                .get(2)
                .copied()
                .ok_or_else(|| error("node begin arity"))?;
            let body = ir
                .function_impls
                .get_mut(&begin)
                .ok_or_else(|| error("node begin"))?;
            body.body_stmts.insert(
                0,
                Stmt::IfElse {
                    condition: Expr::Eq(Box::new(Expr::Var(param_kind)), Box::new(Expr::Int(kind))),
                    then_branch: vec![Stmt::Expr {
                        expr: call(B::Assert.call_index(), vec![Expr::Bool(false)]),
                        line: 1,
                    }],
                    else_branch: Vec::new(),
                    line: 1,
                },
            );
        }
        if self.drop_saved_roots {
            apply_drop_saved_roots(&mut ir)?;
        }
        if self.array_iterator {
            apply_array_iterator_probe(&mut ir)?;
        }
        if self.array_root_census {
            apply_array_root_census(&mut ir)?;
        }
        if let Some((slot, generation, units)) = self.old_identity {
            // Row evidence first, so refusal arms keep it on the stack.
            let c = carrier(&ir)?;
            let observe = helper(&ir, "__compat_es5_heap_observe")?;
            for selector in [13, 14] {
                let observed = fresh(&mut ir)?;
                ir.stmts.push(Stmt::Let {
                    index: observed,
                    declared_schema: Some(TypeSchema::Int),
                    expr: call(
                        observe,
                        vec![Expr::Var(c), Expr::Int(selector), Expr::Int(slot)],
                    ),
                    line: 1,
                });
                ir.stmts.push(Stmt::Expr {
                    expr: Expr::Var(observed),
                    line: 1,
                });
            }
            let checked = helper(&ir, "__compat_es5_checked_base")?;
            let read = helper(&ir, "__compat_ref_read")?;
            let base = fresh(&mut ir)?;
            ir.stmts.push(Stmt::Let {
                index: base,
                declared_schema: Some(TypeSchema::Int),
                expr: call(
                    checked,
                    vec![Expr::Var(c), Expr::Int(slot), Expr::Int(generation)],
                ),
                line: 1,
            });
            for offset in 2..3 + units {
                let slot = fresh(&mut ir)?;
                ir.stmts.push(Stmt::Let {
                    index: slot,
                    declared_schema: Some(TypeSchema::Number),
                    expr: number(call(
                        read,
                        vec![
                            Expr::Var(c),
                            number(Expr::Add(
                                Box::new(Expr::Var(base)),
                                Box::new(Expr::Int(offset as i64)),
                            )),
                        ],
                    )),
                    line: 1,
                });
                ir.stmts.push(Stmt::Expr {
                    expr: Expr::Var(slot),
                    line: 1,
                });
            }
        }
        if let Some(captured) = &self.captured {
            apply_captured_token(&mut ir, captured)?;
        }
        if self.pre_collect {
            let c = carrier(&ir)?;
            let collect = helper(&ir, "__compat_es5_heap_collect")?;
            let slot = fresh(&mut ir)?;
            ir.stmts.push(Stmt::Let {
                index: slot,
                declared_schema: Some(TypeSchema::Int),
                expr: call(collect, vec![Expr::Var(c)]),
                line: 1,
            });
        }
        if !self.extra_selectors.is_empty() {
            let c = carrier(&ir)?;
            let observe = helper(&ir, "__compat_es5_heap_observe")?;
            for selector in self.extra_selectors.clone() {
                let slot = fresh(&mut ir)?;
                ir.stmts.push(Stmt::Let {
                    index: slot,
                    declared_schema: Some(TypeSchema::Int),
                    expr: call(
                        observe,
                        vec![Expr::Var(c), Expr::Int(selector), Expr::Int(0)],
                    ),
                    line: 1,
                });
                ir.stmts.push(Stmt::Expr {
                    expr: Expr::Var(slot),
                    line: 1,
                });
            }
        }
        if let Some(name) = &self.array_header {
            let c = carrier(&ir)?;
            let checked = helper(&ir, "__compat_es5_checked_base")?;
            let read = helper(&ir, "__compat_ref_read")?;
            let source = binding(&ir, name)?;
            let slot = fresh(&mut ir)?;
            let generation = fresh(&mut ir)?;
            let base = fresh(&mut ir)?;
            ir.stmts.push(Stmt::Let {
                index: slot,
                declared_schema: Some(TypeSchema::Int),
                expr: field(source, "ref_slot"),
                line: 1,
            });
            ir.stmts.push(Stmt::Let {
                index: generation,
                declared_schema: Some(TypeSchema::Int),
                expr: field(source, "generation"),
                line: 1,
            });
            ir.stmts.push(Stmt::Let {
                index: base,
                declared_schema: Some(TypeSchema::Int),
                expr: call(
                    checked,
                    vec![Expr::Var(c), Expr::Var(slot), Expr::Var(generation)],
                ),
                line: 1,
            });
            for offset in [0i64, 2, 3, 4] {
                let observed = fresh(&mut ir)?;
                ir.stmts.push(Stmt::Let {
                    index: observed,
                    declared_schema: Some(TypeSchema::Number),
                    expr: number(call(
                        read,
                        vec![
                            Expr::Var(c),
                            number(Expr::Add(
                                Box::new(Expr::Var(base)),
                                Box::new(Expr::Int(offset)),
                            )),
                        ],
                    )),
                    line: 1,
                });
                ir.stmts.push(Stmt::Expr {
                    expr: Expr::Var(observed),
                    line: 1,
                });
            }
        }
        if let Some(expected) = &self.vector_numbers {
            let index = helper(&ir, "__compat_es5_fn_0")?;
            let args = ir
                .function_impls
                .get(&index)
                .ok_or_else(|| error("source impl"))?
                .param_slots[3];
            let mut checks = vec![assert(Expr::Eq(
                Box::new(call(B::Len.call_index(), vec![Expr::Var(args)])),
                Box::new(Expr::Int(expected.len() as i64)),
            ))];
            for (ordinal, n) in expected.iter().enumerate() {
                let slot = fresh(&mut ir)?;
                checks.push(Stmt::Let {
                    index: slot,
                    declared_schema: Some(TypeSchema::Named(
                        "__CompatEs5Primitive".into(),
                        Vec::new(),
                    )),
                    expr: call(
                        B::Get.call_index(),
                        vec![
                            Expr::Borrow(Box::new(Expr::Var(args))),
                            Expr::Int(ordinal as i64),
                        ],
                    ),
                    line: 1,
                });
                checks.push(assert(Expr::Eq(
                    Box::new(field(slot, "tag")),
                    Box::new(Expr::Int(3)),
                )));
                checks.push(assert(Expr::Eq(
                    Box::new(number(field(slot, "number"))),
                    Box::new(Expr::Float(*n)),
                )));
            }
            let implementation = ir
                .function_impls
                .get_mut(&index)
                .ok_or_else(|| error("source impl"))?;
            let pos = implementation
                .body_stmts
                .len()
                .checked_sub(2)
                .ok_or_else(|| error("source epilogue"))?;
            implementation.body_stmts.splice(pos..pos, checks);
        }
        if self.collect {
            let c = carrier(&ir)?;
            let collect = helper(&ir, "__compat_es5_heap_collect")?;
            let slot = fresh(&mut ir)?;
            ir.stmts.push(Stmt::Let {
                index: slot,
                declared_schema: Some(TypeSchema::Int),
                expr: call(collect, vec![Expr::Var(c)]),
                line: 1,
            });
        }
        for name in &self.reads {
            let slot = binding(&ir, name)?;
            ir.stmts.push(Stmt::Expr {
                expr: Expr::Var(slot),
                line: 1,
            });
        }
        if let Some((name, units)) = &self.string {
            let s = binding(&ir, name)?;
            let c = carrier(&ir)?;
            let checked = helper(&ir, "__compat_es5_checked_base")?;
            let read = helper(&ir, "__compat_ref_read")?;
            // Read each identity field exactly once: struct field moves
            // are linear, so every later use goes through these locals.
            let held_slot = fresh(&mut ir)?;
            ir.stmts.push(Stmt::Let {
                index: held_slot,
                declared_schema: None,
                expr: field(s, "ref_slot"),
                line: 1,
            });
            let held_generation = fresh(&mut ir)?;
            ir.stmts.push(Stmt::Let {
                index: held_generation,
                declared_schema: None,
                expr: field(s, "generation"),
                line: 1,
            });
            let base = fresh(&mut ir)?;
            ir.stmts.push(Stmt::Let {
                index: base,
                declared_schema: Some(TypeSchema::Int),
                expr: call(
                    checked,
                    vec![
                        Expr::Var(c),
                        Expr::Var(held_slot),
                        Expr::Var(held_generation),
                    ],
                ),
                line: 1,
            });
            for offset in 2..3 + *units {
                let slot = fresh(&mut ir)?;
                ir.stmts.push(Stmt::Let {
                    index: slot,
                    declared_schema: Some(TypeSchema::Number),
                    expr: number(call(
                        read,
                        vec![
                            Expr::Var(c),
                            number(Expr::Add(
                                Box::new(Expr::Var(base)),
                                Box::new(Expr::Int(offset as i64)),
                            )),
                        ],
                    )),
                    line: 1,
                });
                ir.stmts.push(Stmt::Expr {
                    expr: Expr::Var(slot),
                    line: 1,
                });
            }
            // Lifetime proof tails for the same binding, sharing the single
            // field reads above.
            if self.row_of_binding.as_ref() == Some(name) {
                let observe = helper(&ir, "__compat_es5_heap_observe")?;
                for selector in [13, 14] {
                    let observed = fresh(&mut ir)?;
                    ir.stmts.push(Stmt::Let {
                        index: observed,
                        declared_schema: Some(TypeSchema::Int),
                        expr: call(
                            observe,
                            vec![Expr::Var(c), Expr::Int(selector), Expr::Var(held_slot)],
                        ),
                        line: 1,
                    });
                    ir.stmts.push(Stmt::Expr {
                        expr: Expr::Var(observed),
                        line: 1,
                    });
                }
            }
            if self.push_binding_identity.as_ref() == Some(name) {
                for held in [held_slot, held_generation] {
                    let pushed = fresh(&mut ir)?;
                    ir.stmts.push(Stmt::Let {
                        index: pushed,
                        declared_schema: None,
                        expr: Expr::Var(held),
                        line: 1,
                    });
                    ir.stmts.push(Stmt::Expr {
                        expr: Expr::Var(pushed),
                        line: 1,
                    });
                }
            }
        }
        if self.row_of_binding.is_some() && self.string.is_none() {
            return Err(error(
                "row_of_binding needs the matching string binding to share its single field reads",
            ));
        }
        if self
            .row_of_binding
            .as_ref()
            .is_some_and(|row| self.string.as_ref().is_none_or(|(name, _)| name != row))
        {
            return Err(error(
                "row_of_binding must name the same binding as string to share its single field reads",
            ));
        }
        if self.push_binding_identity.is_some() && self.string.is_none() {
            return Err(error(
                "push_binding_identity needs the matching string binding to share its single field reads",
            ));
        }
        if self.push_binding_identity.as_ref().is_some_and(|identity| {
            self.string
                .as_ref()
                .is_none_or(|(name, _)| name != identity)
        }) {
            return Err(error(
                "push_binding_identity must name the same binding as string to share its single field reads",
            ));
        }
        if self.captured.is_some()
            && (self.string.is_some()
                || self.row_of_binding.is_some()
                || self.push_binding_identity.is_some()
                || self.old_identity.is_some()
                || self.drop_saved_roots)
        {
            return Err(error(
                "captured owns its own same-program tail; do not combine it with the other lifetime tails",
            ));
        }
        if self.captured.is_some() && !self.force_gc {
            return Err(error(
                "captured needs force_gc to trigger the intended freeing collection",
            ));
        }
        if self.completion {
            probe_completion(&mut ir.stmts);
        }
        if self.stats {
            let c = carrier(&ir)?;
            let observe = helper(&ir, "__compat_es5_heap_observe")?;
            // live nodes, occupied node cells, completed collections.
            for selector in [0, 7, 5] {
                let slot = fresh(&mut ir)?;
                ir.stmts.push(Stmt::Let {
                    index: slot,
                    declared_schema: Some(TypeSchema::Int),
                    expr: call(
                        observe,
                        vec![Expr::Var(c), Expr::Int(selector), Expr::Int(0)],
                    ),
                    line: 1,
                });
                ir.stmts.push(Stmt::Expr {
                    expr: Expr::Var(slot),
                    line: 1,
                });
            }
        }
        Ok(ir)
    }
}

fn bound_root_clear(ir: &FrontendIr) -> Result<u16, ParseError> {
    let found = ir
        .functions
        .iter()
        .filter(|f| f.name == "__compat_es5_root_clear")
        .collect::<Vec<_>>();
    if found.len() != 1 {
        return Err(error(
            "drop_saved_roots: root_clear helper is not a unique declaration",
        ));
    }
    let decl = found[0];
    let carrier = Some(TypeSchema::Named("__CompatRefCarrier".into(), vec![]));
    let parameters = matches!(
        decl.args.as_slice(),
        [carrier_name, frame_name, entry_name]
            if carrier_name == "carrier" && frame_name == "frame" && entry_name == "entry"
    );
    if decl.arity != 3
        || !parameters
        || decl.arg_schemas != vec![carrier, Some(TypeSchema::Int), Some(TypeSchema::Int)]
        || decl.return_schema != Some(TypeSchema::Int)
    {
        return Err(error("drop_saved_roots: root_clear declaration schema"));
    }
    let Some(body) = ir.function_impls.get(&decl.index) else {
        return Err(error("drop_saved_roots: root_clear implementation missing"));
    };
    if body.param_slots.len() != 3 || !body.capture_copies.is_empty() {
        return Err(error("drop_saved_roots: root_clear parameter slots"));
    }
    Ok(decl.index)
}

fn canonical_undefined_body(expr: &Expr) -> bool {
    // Captured helper body: five field sets over one empty record call,
    // outer to inner generation, ref_slot, flag, number, tag.
    let mut layers = Vec::new();
    let mut current = expr;
    for _ in 0..5 {
        let Expr::Call(target, type_args, args, resolved, node) = current else {
            return false;
        };
        if !type_args.is_empty() || resolved.is_some() || node.is_some() || args.len() != 3 {
            return false;
        }
        let Expr::String(key) = &args[1] else {
            return false;
        };
        layers.push((*target, key.as_str(), &args[2]));
        current = &args[0];
    }
    let Expr::Call(base, type_args, args, resolved, node) = current else {
        return false;
    };
    if !type_args.is_empty() || resolved.is_some() || node.is_some() || !args.is_empty() {
        return false;
    }
    let set_target = layers[0].0;
    if *base == set_target || layers.iter().any(|(target, _, _)| *target != set_target) {
        return false;
    }
    let keys = layers.iter().map(|(_, key, _)| *key).collect::<Vec<_>>();
    if keys != ["generation", "ref_slot", "flag", "number", "tag"] {
        return false;
    }
    matches!(layers[0].2, Expr::Int(0))
        && matches!(layers[1].2, Expr::Int(0))
        && matches!(layers[2].2, Expr::Bool(false))
        && matches!(layers[3].2, Expr::Float(number) if number.to_bits() == 0.0f64.to_bits())
        && matches!(layers[4].2, Expr::Int(0))
}

fn canonical_undefined_call(ir: &FrontendIr) -> Result<Expr, ParseError> {
    let found = ir
        .functions
        .iter()
        .filter(|f| f.name == "__compat_es5_undefined")
        .collect::<Vec<_>>();
    if found.len() != 1 {
        return Err(error(
            "drop_saved_roots: undefined helper is not a unique declaration",
        ));
    }
    let decl = found[0];
    if decl.arity != 0
        || !decl.args.is_empty()
        || !decl.arg_schemas.is_empty()
        || !decl.type_params.is_empty()
        || decl.return_schema != Some(TypeSchema::Named("__CompatEs5Primitive".into(), vec![]))
    {
        return Err(error("drop_saved_roots: undefined helper schema"));
    }
    let Some(body) = ir.function_impls.get(&decl.index) else {
        return Err(error(
            "drop_saved_roots: undefined helper implementation missing",
        ));
    };
    if !body.param_slots.is_empty()
        || !body.capture_copies.is_empty()
        || !body.body_stmts.is_empty()
        || !canonical_undefined_body(&body.body_expr)
    {
        return Err(error("drop_saved_roots: undefined helper body"));
    }
    Ok(call(decl.index, Vec::new()))
}

/// Test-only owner-drop: find the finally-restore arm by its emitted
/// shape (an `IfElse` whose else branch is exactly two
/// `__compat_es5_root_clear` calls and whose then branch restores the
/// payload from a saved local through one of those entries), then
/// rewrite every snapshot write to those two entries in the same body
/// to a validated zero-arg `__compat_es5_undefined` call.
fn apply_drop_saved_roots(ir: &mut FrontendIr) -> Result<(), ParseError> {
    let root_write = helper(ir, "__compat_es5_root_write")?;
    let root_clear = bound_root_clear(ir)?;
    if root_write == root_clear {
        return Err(error(
            "drop_saved_roots: root_clear collided with root_write",
        ));
    }
    let undefined = canonical_undefined_call(ir)?;
    let mut hits = 0;
    if drop_in_vec(&mut ir.stmts, root_write, root_clear, &undefined)? {
        hits += 1;
    }
    for implementation in ir.function_impls.values_mut() {
        if drop_in_vec(
            &mut implementation.body_stmts,
            root_write,
            root_clear,
            &undefined,
        )? {
            hits += 1;
        }
    }
    if hits == 0 {
        return Err(error(
            "drop_saved_roots: no finally-restore arm found; refusing a blind drop",
        ));
    }
    if hits > 1 {
        return Err(error(
            "drop_saved_roots: ambiguous restore arms; refusing a blind drop",
        ));
    }
    Ok(())
}

fn drop_in_vec(
    stmts: &mut [Stmt],
    root_write: u16,
    root_clear: u16,
    undefined: &Expr,
) -> Result<bool, ParseError> {
    let found = find_restore_arm(stmts, root_write, root_clear);
    let Some((frame, first, second, saved)) = found else {
        return Ok(false);
    };
    if !has_snapshot_write(stmts, root_write, frame, &[first, second], saved) {
        return Err(error(
            "drop_saved_roots: restore arm without a matching snapshot write; refusing a blind drop",
        ));
    }
    mute_saved_writes(stmts, root_write, frame, first, second, undefined);
    Ok(true)
}

fn find_restore_arm(
    stmts: &[Stmt],
    root_write: u16,
    root_clear: u16,
) -> Option<(u16, i64, i64, u16)> {
    for stmt in stmts {
        match stmt {
            Stmt::IfElse {
                then_branch,
                else_branch,
                ..
            } => {
                if let Some(hit) =
                    check_restore_arm(then_branch, else_branch, root_write, root_clear)
                {
                    return Some(hit);
                }
                if let Some(hit) = find_restore_arm(then_branch, root_write, root_clear) {
                    return Some(hit);
                }
                if let Some(hit) = find_restore_arm(else_branch, root_write, root_clear) {
                    return Some(hit);
                }
            }
            Stmt::For { body, .. } | Stmt::While { body, .. } => {
                if let Some(hit) = find_restore_arm(body, root_write, root_clear) {
                    return Some(hit);
                }
            }
            Stmt::Let { expr, .. } | Stmt::Assign { expr, .. } | Stmt::Expr { expr, .. } => {
                if let Some(hit) = find_restore_expr(expr, root_write, root_clear) {
                    return Some(hit);
                }
            }
            _ => {}
        }
    }
    None
}

fn find_restore_expr(
    expr: &Expr,
    root_write: u16,
    root_clear: u16,
) -> Option<(u16, i64, i64, u16)> {
    match expr {
        Expr::Block { stmts, .. } => find_restore_arm(stmts, root_write, root_clear),
        Expr::ResultMatch {
            ok_expr, err_expr, ..
        } => find_restore_expr(ok_expr, root_write, root_clear)
            .or_else(|| find_restore_expr(err_expr, root_write, root_clear)),
        Expr::IfElse {
            then_expr,
            else_expr,
            ..
        } => find_restore_expr(then_expr, root_write, root_clear)
            .or_else(|| find_restore_expr(else_expr, root_write, root_clear)),
        _ => None,
    }
}

fn check_restore_arm(
    then_branch: &[Stmt],
    else_branch: &[Stmt],
    root_write: u16,
    root_clear: u16,
) -> Option<(u16, i64, i64, u16)> {
    if root_write == root_clear || else_branch.len() != 2 {
        return None;
    }
    let mut carrier = None;
    let mut frame = None;
    let mut entries = Vec::new();
    for stmt in else_branch {
        let Stmt::Let {
            declared_schema,
            expr: Expr::Call(target, type_args, args, resolved, node),
            ..
        } = stmt
        else {
            return None;
        };
        if *target != root_clear
            || declared_schema != &Some(TypeSchema::Int)
            || !type_args.is_empty()
            || resolved.is_some()
            || node.is_some()
            || args.len() != 3
        {
            return None;
        }
        // A four-argument root_write, including a refresh of a boxed local,
        // is the wrong callee and arity for this phase's clear.
        let (entry_carrier, entry_frame, entry) = match (&args[0], &args[1], &args[2]) {
            (Expr::Var(entry_carrier), Expr::Var(entry_frame), Expr::Int(entry)) => {
                (*entry_carrier, *entry_frame, *entry)
            }
            _ => return None,
        };
        if carrier.is_some_and(|current| current != entry_carrier)
            || frame.is_some_and(|current| current != entry_frame)
        {
            return None;
        }
        carrier = Some(entry_carrier);
        frame = Some(entry_frame);
        entries.push(entry);
    }
    let frame = frame?;
    if entries[0] == entries[1] {
        return None;
    }
    // The matching restore head: assign the payload slot from a saved
    // local, then refresh-root the payload slot itself (so the refresh
    // boxes the assign *target*, not the saved local). The saved local is
    // returned so the caller can verify its snapshot write.
    let mut restored = None;
    for window in then_branch.windows(2) {
        let (
            Stmt::Assign {
                index: target,
                expr: Expr::Var(saved),
                ..
            },
            Stmt::Let {
                expr: Expr::Call(call, _, args, _, _),
                ..
            },
        ) = (&window[0], &window[1])
        else {
            continue;
        };
        if *call != root_write || args.len() != 4 {
            continue;
        }
        if let (Expr::Var(entry_frame), Expr::Var(refreshed)) = (&args[1], &args[3])
            && *entry_frame == frame
            && target == refreshed
        {
            restored = Some(*saved);
            break;
        }
    }
    Some((frame, entries[0], entries[1], restored?))
}

/// Verify that the candidate saved local was actually snapshot-rooted in
/// the same body: a `root_write` of one of the two abrupt-arm entries
/// boxing that exact local. The restore refresh boxes the payload slot
/// instead, so only the snapshot write matches.
fn has_snapshot_write(
    stmts: &[Stmt],
    root_write: u16,
    frame: u16,
    entries: &[i64],
    saved: u16,
) -> bool {
    for stmt in stmts {
        match stmt {
            Stmt::IfElse {
                then_branch,
                else_branch,
                ..
            } => {
                if has_snapshot_write(then_branch, root_write, frame, entries, saved)
                    || has_snapshot_write(else_branch, root_write, frame, entries, saved)
                {
                    return true;
                }
            }
            Stmt::For { body, .. } | Stmt::While { body, .. } => {
                if has_snapshot_write(body, root_write, frame, entries, saved) {
                    return true;
                }
            }
            Stmt::Let { expr, .. } | Stmt::Assign { expr, .. } | Stmt::Expr { expr, .. } => {
                if has_snapshot_write_expr(expr, root_write, frame, entries, saved) {
                    return true;
                }
                if let Expr::Call(target, _, args, _, _) = expr
                    && *target == root_write
                    && args.len() == 4
                    && let (Expr::Var(entry_frame), Expr::Int(entry), Expr::Var(boxed)) =
                        (&args[1], &args[2], &args[3])
                    && *entry_frame == frame
                    && entries.contains(entry)
                    && *boxed == saved
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

fn has_snapshot_write_expr(
    expr: &Expr,
    root_write: u16,
    frame: u16,
    entries: &[i64],
    saved: u16,
) -> bool {
    match expr {
        Expr::Block { stmts, .. } => has_snapshot_write(stmts, root_write, frame, entries, saved),
        Expr::ResultMatch {
            value,
            ok_expr,
            err_expr,
            ..
        } => {
            has_snapshot_write_expr(value, root_write, frame, entries, saved)
                || has_snapshot_write_expr(ok_expr, root_write, frame, entries, saved)
                || has_snapshot_write_expr(err_expr, root_write, frame, entries, saved)
        }
        Expr::IfElse {
            condition,
            then_expr,
            else_expr,
            ..
        } => {
            has_snapshot_write_expr(condition, root_write, frame, entries, saved)
                || has_snapshot_write_expr(then_expr, root_write, frame, entries, saved)
                || has_snapshot_write_expr(else_expr, root_write, frame, entries, saved)
        }
        Expr::Call(_, _, args, _, _) => args
            .iter()
            .any(|arg| has_snapshot_write_expr(arg, root_write, frame, entries, saved)),
        _ => false,
    }
}

fn mute_saved_writes(
    stmts: &mut [Stmt],
    root_write: u16,
    frame: u16,
    first: i64,
    second: i64,
    undefined: &Expr,
) {
    for stmt in stmts.iter_mut() {
        match stmt {
            Stmt::IfElse {
                then_branch,
                else_branch,
                ..
            } => {
                mute_saved_writes(then_branch, root_write, frame, first, second, undefined);
                mute_saved_writes(else_branch, root_write, frame, first, second, undefined);
            }
            Stmt::For { body, .. } | Stmt::While { body, .. } => {
                mute_saved_writes(body, root_write, frame, first, second, undefined);
            }
            Stmt::Let { expr, .. } | Stmt::Assign { expr, .. } | Stmt::Expr { expr, .. } => {
                mute_saved_expr(expr, root_write, frame, first, second, undefined);
                if let Expr::Call(target, _, args, _, _) = expr
                    && *target == root_write
                    && args.len() == 4
                    && let (Expr::Var(entry_frame), Expr::Int(entry)) = (&args[1], &args[2])
                    && *entry_frame == frame
                    && (*entry == first || *entry == second)
                {
                    args[3] = undefined.clone();
                }
            }
            _ => {}
        }
    }
}

fn mute_saved_expr(
    expr: &mut Expr,
    root_write: u16,
    frame: u16,
    first: i64,
    second: i64,
    undefined: &Expr,
) {
    match expr {
        Expr::Block { stmts, expr } => {
            mute_saved_writes(stmts, root_write, frame, first, second, undefined);
            mute_saved_expr(expr, root_write, frame, first, second, undefined);
        }
        Expr::ResultMatch {
            value,
            ok_expr,
            err_expr,
            ..
        } => {
            mute_saved_expr(value, root_write, frame, first, second, undefined);
            mute_saved_expr(ok_expr, root_write, frame, first, second, undefined);
            mute_saved_expr(err_expr, root_write, frame, first, second, undefined);
        }
        Expr::IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            mute_saved_expr(condition, root_write, frame, first, second, undefined);
            mute_saved_expr(then_expr, root_write, frame, first, second, undefined);
            mute_saved_expr(else_expr, root_write, frame, first, second, undefined);
        }
        Expr::Call(_, _, args, _, _) => {
            for arg in args {
                mute_saved_expr(arg, root_write, frame, first, second, undefined);
            }
        }
        _ => {}
    }
}

fn probe_completion(stmts: &mut Vec<Stmt>) {
    let mut index = 0;
    while index < stmts.len() {
        let slot = match &stmts[index] {
            Stmt::Let {
                index: slot,
                declared_schema: Some(TypeSchema::Named(name, _)),
                ..
            } if name == "__CompatEs5Completion" => Some(*slot),
            _ => None,
        };
        if let Some(slot) = slot {
            stmts.insert(
                index + 1,
                Stmt::Expr {
                    expr: field(slot, "has_value"),
                    line: 1,
                },
            );
            stmts.insert(
                index + 1,
                Stmt::Expr {
                    expr: field(slot, "kind"),
                    line: 1,
                },
            );
            index += 3;
            continue;
        }
        match &mut stmts[index] {
            Stmt::Let { expr, .. } | Stmt::Assign { expr, .. } | Stmt::Expr { expr, .. } => {
                probe_expr(expr);
            }
            Stmt::IfElse {
                then_branch,
                else_branch,
                ..
            } => {
                probe_completion(then_branch);
                probe_completion(else_branch);
            }
            _ => {}
        }
        index += 1;
    }
}

fn probe_expr(expr: &mut Expr) {
    match expr {
        Expr::Block { stmts, expr } => {
            probe_completion(stmts);
            probe_expr(expr);
        }
        Expr::Call(_, _, args, _, _) => {
            for arg in args {
                probe_expr(arg);
            }
        }
        Expr::ResultMatch {
            ok_binding,
            ok_expr,
            err_expr,
            ..
        } => {
            if let (Some(slot), Expr::Block { stmts, .. }) = (*ok_binding, ok_expr.as_mut()) {
                stmts.insert(
                    0,
                    Stmt::Expr {
                        expr: field(slot, "has_value"),
                        line: 1,
                    },
                );
                stmts.insert(
                    0,
                    Stmt::Expr {
                        expr: field(slot, "kind"),
                        line: 1,
                    },
                );
            }
            probe_expr(ok_expr);
            probe_expr(err_expr);
        }
        _ => {}
    }
}

/// Record the real old token BEFORE any freeing collection in the same Script
/// activation, then observe that exact token after the collection. The two
/// numeric locals are the only carrier: no boxed alias, no new guest root,
/// no cross-function channel.
fn apply_captured_token(ir: &mut FrontendIr, captured: &CapturedToken) -> Result<(), ParseError> {
    let c = carrier(ir)?;
    let observe = helper(ir, "__compat_es5_heap_observe")?;
    let checked = helper(ir, "__compat_es5_checked_base")?;
    let read = helper(ir, "__compat_ref_read")?;
    let source = binding(ir, &captured.binding)?;
    let token_slot = fresh(ir)?;
    let token_generation = fresh(ir)?;
    let at = token_initializer_index(ir, source)?;
    let mut capture = vec![
        Stmt::Let {
            index: token_slot,
            declared_schema: Some(TypeSchema::Int),
            expr: borrowed_field(source, "ref_slot"),
            line: 1,
        },
        Stmt::Let {
            index: token_generation,
            declared_schema: Some(TypeSchema::Int),
            expr: borrowed_field(source, "generation"),
            line: 1,
        },
    ];
    // Qualify the identity before accepting it: the captured token must read
    // back the expected real UTF-16 units. A predeclared Undefined or an
    // unrelated box fails here instead of silently proving the wrong slot.
    // String nodes carry the kind cell at offset 2 and the units from offset
    // 3 on (same layout the maintained survivor reader uses).
    for (ordinal, unit) in captured.units.iter().enumerate() {
        capture.push(assert(Expr::Eq(
            Box::new(number(call(
                read,
                vec![
                    Expr::Var(c),
                    number(Expr::Add(
                        Box::new(call(
                            checked,
                            vec![
                                Expr::Var(c),
                                Expr::Var(token_slot),
                                Expr::Var(token_generation),
                            ],
                        )),
                        Box::new(Expr::Int(3 + ordinal as i64)),
                    )),
                ],
            ))),
            Box::new(Expr::Float(f64::from(*unit))),
        )));
    }
    // Capture marker: the measurement ran before the freeing collection.
    capture.push(Stmt::Expr {
        expr: Expr::Int(1),
        line: 1,
    });
    ir.stmts.splice(at + 1..at + 1, capture);

    let mut tail = Vec::new();
    // The intended freeing collection really ran in this execution.
    let collections = fresh(ir)?;
    tail.push(Stmt::Let {
        index: collections,
        declared_schema: Some(TypeSchema::Int),
        expr: call(observe, vec![Expr::Var(c), Expr::Int(5), Expr::Int(0)]),
        line: 1,
    });
    tail.push(Stmt::Expr {
        expr: Expr::Var(collections),
        line: 1,
    });
    if let Some((name, units)) = &captured.survivor {
        let survivor = binding(ir, name)?;
        let slot = fresh(ir)?;
        let generation = fresh(ir)?;
        for (held, field_name) in [(slot, "ref_slot"), (generation, "generation")] {
            tail.push(Stmt::Let {
                index: held,
                declared_schema: Some(TypeSchema::Int),
                expr: field(survivor, field_name),
                line: 1,
            });
        }
        // The survivor's own checked read must succeed; it is the control's
        // proof that the payload is still a genuine live value.
        let survivor_base = fresh(ir)?;
        tail.push(Stmt::Let {
            index: survivor_base,
            declared_schema: Some(TypeSchema::Int),
            expr: call(
                checked,
                vec![Expr::Var(c), Expr::Var(slot), Expr::Var(generation)],
            ),
            line: 1,
        });
        for ordinal in 0..units.len() {
            let observed = fresh(ir)?;
            tail.push(Stmt::Let {
                index: observed,
                declared_schema: Some(TypeSchema::Number),
                expr: number(call(
                    read,
                    vec![
                        Expr::Var(c),
                        number(Expr::Add(
                            Box::new(Expr::Var(survivor_base)),
                            Box::new(Expr::Int(3 + ordinal as i64)),
                        )),
                    ],
                )),
                line: 1,
            });
            tail.push(Stmt::Expr {
                expr: Expr::Var(observed),
                line: 1,
            });
        }
        for held in [slot, generation] {
            tail.push(Stmt::Expr {
                expr: Expr::Var(held),
                line: 1,
            });
        }
    }
    // Row state/generation of exactly the recorded slot AFTER that collection.
    for selector in [13, 14] {
        let observed = fresh(ir)?;
        tail.push(Stmt::Let {
            index: observed,
            declared_schema: Some(TypeSchema::Int),
            expr: call(
                observe,
                vec![Expr::Var(c), Expr::Int(selector), Expr::Var(token_slot)],
            ),
            line: 1,
        });
        tail.push(Stmt::Expr {
            expr: Expr::Var(observed),
            line: 1,
        });
    }
    // The recorded token itself, as plain numeric values.
    for held in [token_slot, token_generation] {
        tail.push(Stmt::Expr {
            expr: Expr::Var(held),
            line: 1,
        });
    }
    // The existing checked read on exactly the recorded token. A reclaimed
    // generation refuses right here, before any unit read (the failing
    // operand is the tail `false`); a still-live token keeps every
    // observation above and continues into the unit reads.
    let base = fresh(ir)?;
    tail.push(Stmt::Let {
        index: base,
        declared_schema: Some(TypeSchema::Int),
        expr: call(
            checked,
            vec![
                Expr::Var(c),
                Expr::Var(token_slot),
                Expr::Var(token_generation),
            ],
        ),
        line: 1,
    });
    // Units of the recorded token: reached only while it is still genuine.
    for ordinal in 0..captured.units.len() {
        let observed = fresh(ir)?;
        tail.push(Stmt::Let {
            index: observed,
            declared_schema: Some(TypeSchema::Number),
            expr: number(call(
                read,
                vec![
                    Expr::Var(c),
                    number(Expr::Add(
                        Box::new(Expr::Var(base)),
                        Box::new(Expr::Int(3 + ordinal as i64)),
                    )),
                ],
            )),
            line: 1,
        });
        tail.push(Stmt::Expr {
            expr: Expr::Var(observed),
            line: 1,
        });
    }
    ir.stmts.extend(tail);
    Ok(())
}

#[test]
fn token_initializer_index_controls() {
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let context = vm::SourceFrontendContext {
        source_id: 0,
        original_path: None,
        phase: vm::compiler::ParsePhase::Standalone,
        host_api_catalog: Some(&catalog),
        imports: &[],
    };
    let parse = |source| {
        pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN
            .parse_source(source, &context)
            .expect("real token-control source must lower")
    };
    let actual =
        parse("var s='\\uD800\\u0041';try{throw s;}catch(e){s=null;var kept=e;}var k={m:1};kept;");
    let slot = binding(&actual, "s").expect("real source binding");
    // Read all three producer guards independently of the matcher under test:
    // statement, declarator and store use the same Var with empty else arms.
    let (at, triple, double, single, store, normal) = actual
        .stmts
        .iter()
        .enumerate()
        .find_map(|(at, outer)| {
            let Stmt::IfElse {
                condition: Expr::Var(normal),
                then_branch,
                else_branch,
                ..
            } = outer
            else {
                return None;
            };
            for inner in then_branch {
                if let Stmt::IfElse {
                    condition: Expr::Var(inner_normal),
                    then_branch: declarator,
                    else_branch: inner_else,
                    ..
                } = inner
                {
                    for store_guard in declarator {
                        if let Stmt::IfElse {
                            condition: Expr::Var(store_normal),
                            then_branch: stores,
                            else_branch: store_else,
                            ..
                        } = store_guard
                            && let Some(store) = stores.iter().find(
                                |stmt| matches!(stmt, Stmt::Assign { index, .. } if *index == slot),
                            )
                        {
                            assert_eq!(normal, inner_normal, "actual declarator guard Var");
                            assert_eq!(normal, store_normal, "actual store guard Var");
                            assert!(
                                else_branch.is_empty()
                                    && inner_else.is_empty()
                                    && store_else.is_empty()
                            );
                            return Some((
                                at,
                                outer.clone(),
                                inner.clone(),
                                store_guard.clone(),
                                store.clone(),
                                *normal,
                            ));
                        }
                    }
                }
            }
            None
        })
        .unwrap_or_else(|| {
            panic!(
                "actual enhanced initializer triple guard missing; slot={slot}; stmts={:#?}",
                actual.stmts
            )
        });
    eprintln!(
        "TOKEN_INITIALIZER_PRODUCER top_index={at} pending_normal={normal} slot={slot} guards=3 same_var=true empty_else=true"
    );
    // These are matcher-only structural fixtures made from real emitted IR
    // fragments. They are never compiled or run as business programs.
    let guard = |then_branch| Stmt::IfElse {
        condition: Expr::Var(normal),
        then_branch,
        else_branch: Vec::new(),
        line: 1,
    };
    let fixture = |stmts| vm::LocalIrBuilder::new().finish(stmts);
    let prefix = || Stmt::Noop { line: 1 };
    // Core has no Try statement variant: retain the actual lowered Try/catch
    // control flow, where this binding is stored only inside the catch arm.
    let try_only = parse("var s;try{throw 0;}catch(e){s='\\uD800\\u0041';}s;");
    let try_slot = binding(&try_only, "s").expect("try-only binding");
    let cases = [
        (
            "plain_unique",
            fixture(vec![prefix(), store.clone()]),
            slot,
            Some(1),
        ),
        (
            "single_guard_unique",
            fixture(vec![prefix(), single.clone()]),
            slot,
            Some(1),
        ),
        (
            "double_guard_unique",
            fixture(vec![prefix(), double.clone()]),
            slot,
            Some(1),
        ),
        (
            "missing_slot",
            fixture(vec![Stmt::Let {
                index: slot,
                declared_schema: None,
                expr: Expr::Null,
                line: 1,
            }]),
            slot,
            None,
        ),
        (
            "duplicate_top_level",
            fixture(vec![single, double]),
            slot,
            None,
        ),
        (
            "duplicate_inside_one_wrapper",
            fixture(vec![guard(vec![store.clone(), store.clone()])]),
            slot,
            None,
        ),
        ("try_only_store", try_only, try_slot, None),
        (
            "too_deep_guard",
            fixture(vec![guard(vec![triple])]),
            slot,
            None,
        ),
        (
            "else_only_store",
            fixture(vec![Stmt::IfElse {
                condition: Expr::Var(normal),
                then_branch: Vec::new(),
                else_branch: vec![store.clone()],
                line: 1,
            }]),
            slot,
            None,
        ),
        ("triple_guard_unique", actual, slot, Some(at)),
        (
            "mismatched_guard_var",
            fixture(vec![guard(vec![Stmt::IfElse {
                condition: Expr::Var(normal.wrapping_add(1)),
                then_branch: vec![store.clone()],
                else_branch: Vec::new(),
                line: 1,
            }])]),
            slot,
            None,
        ),
        (
            "nonempty_guard_else",
            fixture(vec![Stmt::IfElse {
                condition: Expr::Var(normal),
                then_branch: vec![store],
                else_branch: vec![prefix()],
                line: 1,
            }]),
            slot,
            None,
        ),
    ];
    let mut mismatches = Vec::new();
    for (name, ir, slot, expected) in cases {
        let result = token_initializer_index(&ir, slot);
        let actual = result.as_ref().ok().copied();
        eprintln!(
            "TOKEN_INITIALIZER_CONTROL name={name} expected={expected:?} actual={actual:?} matched={}",
            actual == expected
        );
        if actual != expected {
            mismatches.push((name, expected, actual));
        }
    }
    assert!(
        mismatches.is_empty(),
        "initializer control mismatches: {mismatches:?}"
    );
}

/// The `var <binding> = <initializer>` statement, located by its emitted
/// shape: a plain assign or up to three same-Var pending-normal guards from
/// statement, declarator and store lowering. Every else arm must be empty.
/// Count stores, including duplicates in one wrapper, and return the outer
/// statement index only for exactly one match. Never descend into other IR.
fn token_initializer_index(ir: &FrontendIr, slot: u16) -> Result<usize, ParseError> {
    fn count(stmts: &[Stmt], slot: u16, normal: Option<u16>, depth: u8) -> usize {
        stmts
            .iter()
            .map(|stmt| match stmt {
                Stmt::Assign { index, .. } if *index == slot => 1,
                Stmt::IfElse {
                    condition: Expr::Var(guard),
                    then_branch,
                    else_branch,
                    ..
                } if depth < 3
                    && normal.is_none_or(|normal| normal == *guard)
                    && else_branch.is_empty() =>
                {
                    count(then_branch, slot, Some(*guard), depth + 1)
                }
                _ => 0,
            })
            .sum()
    }
    let mut occurrences = 0;
    let mut outer = None;
    for (index, stmt) in ir.stmts.iter().enumerate() {
        let stores = count(std::slice::from_ref(stmt), slot, None, 0);
        if stores != 0 {
            occurrences += stores;
            outer = Some(index);
        }
    }
    match (occurrences, outer) {
        (1, Some(index)) => Ok(index),
        _ => Err(error(
            "captured: expected exactly one initializer for the recorded binding",
        )),
    }
}

#[allow(unused_imports)]
pub use finally_window::FinallyWindowObserver;

#[allow(dead_code)]
mod finally_window {
    use super::*;

    // Opt-in scalar witness for the natural finally collection window.
    // The default Observer and its emitted IR are unchanged.
    pub struct FinallyWindowObserver {
        pub inner: Observer,
        pub missing_b: bool,
    }
    const FINALLY_WINDOW_LINE: u32 = 900090;
    impl SourcePlugin for FinallyWindowObserver {
        fn descriptor(&self) -> vm::SourceFrontendDescriptor {
            self.inner.descriptor()
        }
        fn parse_source(
            &self,
            source: &str,
            context: &vm::SourceFrontendContext<'_>,
        ) -> Result<FrontendIr, ParseError> {
            if self.inner.force_gc || self.inner.drop_saved_roots || self.inner.collect {
                return Err(error("finally_window: incompatible mutation"));
            }
            let mut ir = self.inner.parse_source(source, context)?;
            apply_finally_window(&mut ir, self.missing_b)?;
            Ok(ir)
        }
    }
    fn finally_window_target(
        ir: &FrontendIr,
        name: &str,
        args: Vec<TypeSchema>,
        result: TypeSchema,
    ) -> u16 {
        let found: Vec<_> = ir.functions.iter().filter(|f| f.name == name).collect();
        assert_eq!(found.len(), 1, "finally_window unique helper {name}");
        assert_eq!(
            found[0].arg_schemas,
            args.into_iter().map(Some).collect::<Vec<_>>()
        );
        assert_eq!(found[0].return_schema, Some(result));
        found[0].index
    }
    fn finally_window_expr(s: &Stmt) -> Option<&Expr> {
        match s {
            Stmt::Let { expr, .. } | Stmt::Assign { expr, .. } | Stmt::Expr { expr, .. } => {
                Some(expr)
            }
            _ => None,
        }
    }
    fn finally_window_call(s: &Stmt, target: u16) -> Option<&Vec<Expr>> {
        match finally_window_expr(s) {
            Some(Expr::Call(t, _, args, _, _)) if *t == target => Some(args),
            _ => None,
        }
    }
    fn finally_window_eq(a: &Expr, b: &Expr) -> bool {
        format!("{a:?}") == format!("{b:?}")
    }
    fn finally_window_set(index: u16, expr: Expr) -> Stmt {
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index,
            expr,
            line: FINALLY_WINDOW_LINE,
        }
    }
    // Only the actual caller's nested statement If wrappers are needed to reach
    // its restore-containing vector; no broad transformation framework.
    fn finally_window_regions(
        stmts: &[Stmt],
        root: u16,
        root_clear: u16,
        path: &mut Vec<(usize, bool)>,
        out: &mut Vec<Vec<(usize, bool)>>,
    ) {
        if stmts.iter().any(|s| matches!(s,Stmt::IfElse {then_branch,else_branch,..} if check_restore_arm(then_branch,else_branch,root,root_clear).is_some())) {
        out.push(path.clone());
    }
        for (i, s) in stmts.iter().enumerate() {
            if let Stmt::IfElse {
                then_branch,
                else_branch,
                ..
            } = s
            {
                path.push((i, true));
                finally_window_regions(then_branch, root, root_clear, path, out);
                path.pop();
                path.push((i, false));
                finally_window_regions(else_branch, root, root_clear, path, out);
                path.pop();
            }
        }
    }
    fn finally_window_region_mut<'a>(
        mut v: &'a mut Vec<Stmt>,
        path: &[(usize, bool)],
    ) -> &'a mut Vec<Stmt> {
        for &(i, then) in path {
            let Stmt::IfElse {
                then_branch,
                else_branch,
                ..
            } = &mut v[i]
            else {
                panic!("finally_window wrapper")
            };
            v = if then { then_branch } else { else_branch };
        }
        v
    }
    fn apply_finally_window(ir: &mut FrontendIr, missing_b: bool) -> Result<(), ParseError> {
        let c_schema = TypeSchema::Named("__CompatRefCarrier".into(), vec![]);
        let js = TypeSchema::Named("__CompatEs5Primitive".into(), vec![]);
        let reference = TypeSchema::Named("__CompatEs5Reference".into(), vec![]);
        let allocation = TypeSchema::Named("__CompatEs5Allocation".into(), vec![]);
        let root = finally_window_target(
            ir,
            "__compat_es5_root_write",
            vec![
                c_schema.clone(),
                TypeSchema::Int,
                TypeSchema::Int,
                js.clone(),
            ],
            TypeSchema::Int,
        );
        let root_clear = finally_window_target(
            ir,
            "__compat_es5_root_clear",
            vec![c_schema.clone(), TypeSchema::Int, TypeSchema::Int],
            TypeSchema::Int,
        );
        assert_ne!(
            root_clear, root,
            "finally_window root_clear is not root_write"
        );
        let clear_decl = ir.functions.iter().find(|f| f.index == root_clear).unwrap();
        assert_eq!(clear_decl.arity, 3, "finally_window root_clear arity");
        assert!(
            matches!(
                clear_decl.args.as_slice(),
                [carrier, frame, entry]
                    if carrier == "carrier" && frame == "frame" && entry == "entry"
            ),
            "finally_window root_clear parameters"
        );
        let clear_impl = &ir.function_impls[&root_clear];
        assert_eq!(
            clear_impl.param_slots.len(),
            3,
            "finally_window root_clear slots"
        );
        assert!(
            clear_impl.capture_copies.is_empty(),
            "finally_window root_clear captures"
        );
        let enter = finally_window_target(
            ir,
            "__compat_es5_root_enter",
            vec![c_schema.clone(), TypeSchema::Int],
            TypeSchema::Int,
        );
        let leave = finally_window_target(
            ir,
            "__compat_es5_root_leave",
            vec![c_schema.clone(), TypeSchema::Int],
            TypeSchema::Int,
        );
        let begin = finally_window_target(
            ir,
            "__compat_es5_node_begin",
            vec![c_schema.clone(), TypeSchema::Int, TypeSchema::Int],
            allocation,
        );
        let publish = finally_window_target(
            ir,
            "__compat_es5_node_publish",
            vec![
                c_schema.clone(),
                TypeSchema::Int,
                TypeSchema::Int,
                TypeSchema::Int,
                TypeSchema::Int,
                TypeSchema::Int,
            ],
            js.clone(),
        );
        let observe = finally_window_target(
            ir,
            "__compat_es5_heap_observe",
            vec![c_schema.clone(), TypeSchema::Int, TypeSchema::Int],
            TypeSchema::Int,
        );
        let get = helper(ir, "__compat_es5_ref_get")?;
        let get_decl = ir
            .functions
            .iter()
            .filter(|f| f.name == "__compat_es5_ref_get")
            .collect::<Vec<_>>();
        assert_eq!(get_decl.len(), 1);
        assert_eq!(
            get_decl[0].arg_schemas,
            vec![Some(c_schema), Some(reference)]
        );
        assert!(
            matches!(&get_decl[0].return_schema,Some(TypeSchema::Result {ok,err}) if **ok==js && **err==js)
        );
        let owners = ir
            .function_impls
            .iter()
            .filter(|(_, b)| find_restore_arm(&b.body_stmts, root, root_clear).is_some())
            .map(|(i, _)| *i)
            .collect::<Vec<_>>();
        assert_eq!(
            owners.len(),
            1,
            "finally_window unique actual snapshot owner"
        );
        assert!(find_restore_arm(&ir.stmts, root, root_clear).is_none());
        let id = owners[0];
        let decl = ir.functions.iter().find(|f| f.index == id).unwrap();
        assert!(decl.name.starts_with("__compat_es5_fn_"));
        let before = ir.function_impls[&id].clone();
        let raw = format!("{before:#?}");
        // No lexical early Ret (ResultTry) or opaque expression control flow in
        // this actual caller. Source Return only assigns completion/normal.
        for forbidden in [
            "ResultTry",
            "Closure",
            "LocalCall",
            "ModuleCall",
            "expr: Match {",
            "For {",
            "While {",
            "Break {",
            "Continue {",
        ] {
            assert!(
                !raw.contains(forbidden),
                "finally_window unproved exit/control {forbidden}"
            );
        }
        let mut paths = vec![];
        finally_window_regions(
            &before.body_stmts,
            root,
            root_clear,
            &mut vec![],
            &mut paths,
        );
        assert_eq!(
            paths.len(),
            1,
            "finally_window exact restore containing vector"
        );
        let (frame, first, second, saved) =
            find_restore_arm(&before.body_stmts, root, root_clear).unwrap();
        assert!(has_snapshot_write(
            &before.body_stmts,
            root,
            frame,
            &[first, second],
            saved
        ));
        let c = before.param_slots[0];
        let enter_args = finally_window_call(&before.body_stmts[0], enter)
            .expect("finally_window entry root_enter");
        assert!(matches!(&before.body_stmts[0],Stmt::Let {index,..} if *index==frame));
        assert!(matches!(enter_args.as_slice(),[Expr::Var(x),Expr::Int(cap)] if *x==c && *cap>0));
        let last = before.body_stmts.len() - 1;
        let leave_args = finally_window_call(&before.body_stmts[last], leave)
            .expect("finally_window mandatory final leave");
        assert_eq!(
            format!("{leave_args:?}"),
            format!("{:?}", vec![Expr::Var(c), Expr::Var(frame)])
        );
        let Expr::Var(boundary) = before.body_expr else {
            panic!("finally_window result boundary")
        };
        assert!(
            matches!(&before.body_stmts[last-1],Stmt::Let {expr:Expr::Call(t,_,args,_,_),..} if *t==B::Len.call_index() && matches!(args.as_slice(),[Expr::Var(x)] if *x==before.param_slots[3]))
        );
        assert!(
            matches!(&before.body_stmts[last-3],Stmt::Let {index,expr:Expr::ResultConstruct {ok:true,..},..} if *index==boundary)
        );
        assert!(
            matches!(&before.body_stmts[last-2],Stmt::IfElse {then_branch,else_branch,..} if else_branch.is_empty() && matches!(then_branch.last(),Some(Stmt::Assign {index,expr:Expr::ResultConstruct {ok:false,..},..}) if *index==boundary))
        );
        let mut samples = [0u16; 6];
        for s in &mut samples {
            *s = fresh(ir)?;
        }
        let sample = |phase: usize| {
            let mut v = vec![
                finally_window_set(
                    samples[phase * 2],
                    call(observe, vec![Expr::Var(c), Expr::Int(5), Expr::Int(0)]),
                ),
                finally_window_set(
                    samples[phase * 2 + 1],
                    call(observe, vec![Expr::Var(c), Expr::Int(6), Expr::Int(0)]),
                ),
            ];
            if phase == 0 || !missing_b {
                v.push(finally_window_set(samples[4 + phase], Expr::Int(1)));
            }
            v
        };
        let body = ir.function_impls.get_mut(&id).unwrap();
        let region = finally_window_region_mut(&mut body.body_stmts, &paths[0]);
        let restores = region
            .iter()
            .enumerate()
            .filter_map(|(i, s)| match s {
                Stmt::IfElse {
                    then_branch,
                    else_branch,
                    ..
                } if check_restore_arm(then_branch, else_branch, root, root_clear).is_some() => {
                    Some(i)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(restores.len(), 1);
        let restore = restores[0];
        let snapshots=region.iter().enumerate().filter_map(|(i,s)| {
        let a=finally_window_call(s,root)?;
        (matches!(a.as_slice(),[Expr::Var(x),Expr::Var(f),Expr::Int(e),Expr::Var(s)] if *x==c && *f==frame && [first,second].contains(e) && *s==saved)).then_some(i)
    }).collect::<Vec<_>>();
        assert_eq!(snapshots.len(), 1);
        let a = snapshots[0];
        assert!(a < restore);
        // Resolve the actual r=k.m containing wrappers by the unique real Get.
        let mut gets = vec![];
        for (i, s) in region.iter().enumerate().take(restore).skip(a + 1) {
            if let Stmt::IfElse { then_branch, .. } = s {
                for (j, s) in then_branch.iter().enumerate() {
                    if let Stmt::Let {
                        expr: Expr::Block { stmts, .. },
                        ..
                    } = s
                    {
                        for (k, s) in stmts.iter().enumerate() {
                            if let Stmt::IfElse { then_branch, .. } = s {
                                for (n, s) in then_branch.iter().enumerate() {
                                    if finally_window_call(s, get).is_some() {
                                        gets.push((i, j, k, n));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(gets.len(), 1, "finally_window unique finalizer RefGet");
        let (i, j, k, n) = gets[0];
        let Stmt::IfElse { then_branch, .. } = &mut region[i] else {
            unreachable!()
        };
        let Stmt::Let {
            expr: Expr::Block { stmts, .. },
            ..
        } = &mut then_branch[j]
        else {
            unreachable!()
        };
        let Stmt::IfElse { then_branch: b, .. } = &mut stmts[k] else {
            unreachable!()
        };
        let begins = b[..n]
            .iter()
            .enumerate()
            .filter(|(_, s)| finally_window_call(s, begin).is_some())
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        let publishes = b[..n]
            .iter()
            .enumerate()
            .filter(|(_, s)| finally_window_call(s, publish).is_some())
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        assert_eq!(begins.len(), 1);
        assert_eq!(publishes.len(), 1);
        let bi = begins[0];
        let pi = publishes[0];
        assert_eq!(bi, 0);
        assert!(bi < pi && pi < n);
        assert!(
            matches!(finally_window_call(&b[bi],begin).unwrap().as_slice(),[Expr::Var(x),Expr::Int(4),Expr::Int(2)] if *x==c)
        );
        let Stmt::Let { index: newborn, .. } = &b[bi] else {
            unreachable!()
        };
        let mut fields = vec![];
        for name in ["slot", "generation", "base"] {
            let hits = b[bi + 1..pi]
                .iter()
                .filter_map(|s| match s {
                    Stmt::Let { index, expr, .. }
                        if finally_window_eq(expr, &field(*newborn, name)) =>
                    {
                        Some(*index)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(hits.len(), 1);
            fields.push(Expr::Var(hits[0]));
        }
        let p_args = finally_window_call(&b[pi], publish).unwrap();
        assert_eq!(
            format!("{:?}", &p_args[..5]),
            format!(
                "{:?}",
                vec![
                    Expr::Var(c),
                    fields[0].clone(),
                    fields[1].clone(),
                    fields[2].clone(),
                    Expr::Var(frame)
                ]
            )
        );
        let Stmt::Let { index: key, .. } = &b[pi] else {
            unreachable!()
        };
        let get_args = finally_window_call(&b[n], get).unwrap();
        let [Expr::Var(get_c), Expr::Var(ref_slot)] = get_args.as_slice() else {
            panic!("finally_window Get operands")
        };
        assert_eq!(*get_c, c);
        let references = b[pi + 1..n]
            .iter()
            .filter_map(|s| match s {
                Stmt::Let { index, expr, .. } if index == ref_slot => Some(expr),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(references.len(), 1);
        let ref_text = format!("{:?}", references[0]);
        assert!(ref_text.contains("key_slot") && ref_text.contains("key_generation"));
        assert!(
            ref_text.contains(&format!("{:?}", field(*key, "ref_slot")))
                && ref_text.contains(&format!("{:?}", field(*key, "generation")))
        );
        // Preserve the original reference constructor and nonallocating root writes.
        b.splice(n..n, sample(1));
        region.splice(a + 1..a + 1, sample(0));
        let mut checks = vec![];
        for (slot, value) in [
            (samples[4], 1),
            (samples[5], 1),
            (samples[0], 0),
            (samples[2], 1),
            (samples[3], 1),
        ] {
            checks.push(assert(Expr::Eq(
                Box::new(Expr::Var(slot)),
                Box::new(Expr::Int(value)),
            )));
        }
        checks.push(assert(Expr::Not(Box::new(Expr::Lt(
            Box::new(Expr::Var(samples[1])),
            Box::new(Expr::Int(0)),
        )))));
        checks.push(assert(Expr::Lt(
            Box::new(Expr::Var(samples[1])),
            Box::new(Expr::Int(32)),
        )));
        for s in &mut checks {
            let Stmt::Expr { line, .. } = s else {
                unreachable!()
            };
            *line = FINALLY_WINDOW_LINE;
        }
        body.body_stmts.splice(last..last, checks);
        body.body_stmts.splice(
            0..0,
            samples.iter().map(|s| Stmt::Let {
                index: *s,
                declared_schema: Some(TypeSchema::Int),
                expr: Expr::Int(0),
                line: FINALLY_WINDOW_LINE,
            }),
        );
        let mut inverse = body.clone();
        let removed = finally_window_strip(&mut inverse.body_stmts);
        assert_eq!(removed, if missing_b { 18 } else { 19 });
        assert_eq!(
            format!("{inverse:#?}"),
            format!("{before:#?}"),
            "finally_window inverse preserves every original subtree"
        );
        eprintln!(
            "FINALLY_WINDOW_IR missing_b={missing_b} id={id} frame={frame} capacity={enter_args:?} removed_scalar_statements={removed} complete_caller_inverse=true unchanged_result=true snapshot=1 Get=1 common_exit=1"
        );
        Ok(())
    }

    fn finally_window_strip(v: &mut Vec<Stmt>) -> usize {
        let old = v.len();
        v.retain(|s| {
            !matches!(
                s,
                Stmt::Let {
                    line: FINALLY_WINDOW_LINE,
                    ..
                } | Stmt::Assign {
                    line: FINALLY_WINDOW_LINE,
                    ..
                } | Stmt::Expr {
                    line: FINALLY_WINDOW_LINE,
                    ..
                }
            )
        });
        let mut removed = old - v.len();
        for s in v {
            match s {
                Stmt::IfElse {
                    condition,
                    then_branch,
                    else_branch,
                    ..
                } => {
                    removed += finally_window_strip_expr(condition)
                        + finally_window_strip(then_branch)
                        + finally_window_strip(else_branch);
                }
                Stmt::Let { expr, .. } | Stmt::Assign { expr, .. } | Stmt::Expr { expr, .. } => {
                    removed += finally_window_strip_expr(expr)
                }
                _ => {}
            }
        }
        removed
    }
    fn finally_window_strip_expr(e: &mut Expr) -> usize {
        match e {
            Expr::Block { stmts, expr } => {
                finally_window_strip(stmts) + finally_window_strip_expr(expr)
            }
            Expr::ResultMatch {
                value,
                ok_expr,
                err_expr,
                ..
            } => {
                finally_window_strip_expr(value)
                    + finally_window_strip_expr(ok_expr)
                    + finally_window_strip_expr(err_expr)
            }
            Expr::IfElse {
                condition,
                then_expr,
                else_expr,
            } => {
                finally_window_strip_expr(condition)
                    + finally_window_strip_expr(then_expr)
                    + finally_window_strip_expr(else_expr)
            }
            Expr::Call(_, _, args, _, _) => args.iter_mut().map(finally_window_strip_expr).sum(),
            Expr::ResultConstruct { payload, .. } => finally_window_strip_expr(payload),
            Expr::Add(a, b)
            | Expr::Sub(a, b)
            | Expr::Mul(a, b)
            | Expr::Div(a, b)
            | Expr::Eq(a, b)
            | Expr::Lt(a, b)
            | Expr::Gt(a, b)
            | Expr::And(a, b)
            | Expr::Or(a, b) => finally_window_strip_expr(a) + finally_window_strip_expr(b),
            Expr::Borrow(a) | Expr::BorrowMut(a) | Expr::Not(a) | Expr::Neg(a) => {
                finally_window_strip_expr(a)
            }
            _ => 0,
        }
    }
}

#[test]
fn finally_restore_clear_shape_controls() {
    const WRITE: u16 = 1;
    const CLEAR: u16 = 2;
    const OTHER: u16 = 3;
    const FRAME: u16 = 4;
    const CARRIER: u16 = 7;
    const SAVED: u16 = 8;
    const SLOT: u16 = 11;

    fn clear_let(ack: u16, target: u16, frame: u16, entry: i64) -> Stmt {
        Stmt::Let {
            index: ack,
            declared_schema: Some(TypeSchema::Int),
            expr: call(
                target,
                vec![Expr::Var(CARRIER), Expr::Var(frame), Expr::Int(entry)],
            ),
            line: 1,
        }
    }
    fn refresh(ack: u16, frame: u16, slot: u16) -> Stmt {
        Stmt::Let {
            index: ack,
            declared_schema: Some(TypeSchema::Int),
            expr: call(
                WRITE,
                vec![
                    Expr::Var(CARRIER),
                    Expr::Var(frame),
                    Expr::Int(3),
                    Expr::Var(slot),
                ],
            ),
            line: 1,
        }
    }
    fn assign(slot: u16, saved: u16) -> Stmt {
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: slot,
            expr: Expr::Var(saved),
            line: 1,
        }
    }
    fn arm(then_branch: Vec<Stmt>, else_branch: Vec<Stmt>) -> Stmt {
        Stmt::IfElse {
            condition: Expr::Var(9),
            then_branch,
            else_branch,
            line: 1,
        }
    }
    let positive = arm(
        vec![assign(SLOT, SAVED), refresh(12, FRAME, SLOT)],
        vec![
            clear_let(13, CLEAR, FRAME, 10),
            clear_let(14, CLEAR, FRAME, 9),
        ],
    );
    let snapshot = Stmt::Let {
        index: 15,
        declared_schema: Some(TypeSchema::Int),
        expr: call(
            WRITE,
            vec![
                Expr::Var(CARRIER),
                Expr::Var(FRAME),
                Expr::Int(10),
                Expr::Var(SAVED),
            ],
        ),
        line: 1,
    };
    let nested = vec![arm(vec![positive.clone()], vec![])];
    let body = vec![snapshot, positive];
    assert_eq!(
        find_restore_arm(&body, WRITE, CLEAR),
        Some((FRAME, 10, 9, SAVED))
    );
    assert!(has_snapshot_write(&body, WRITE, FRAME, &[10, 9], SAVED));
    assert_eq!(
        find_restore_arm(&nested, WRITE, CLEAR),
        Some((FRAME, 10, 9, SAVED))
    );

    let reject = |else_branch: Vec<Stmt>| {
        let candidate = vec![arm(
            vec![assign(SLOT, SAVED), refresh(12, FRAME, SLOT)],
            else_branch,
        )];
        assert!(find_restore_arm(&candidate, WRITE, CLEAR).is_none());
    };
    reject(vec![
        clear_let(13, OTHER, FRAME, 10),
        clear_let(14, OTHER, FRAME, 9),
    ]);
    reject(vec![
        clear_let(13, WRITE, FRAME, 10),
        clear_let(14, WRITE, FRAME, 9),
    ]);
    let old_payload = |value: Expr, entry: i64, ack: u16| Stmt::Let {
        index: ack,
        declared_schema: Some(TypeSchema::Int),
        expr: call(
            WRITE,
            vec![
                Expr::Var(CARRIER),
                Expr::Var(FRAME),
                Expr::Int(entry),
                value,
            ],
        ),
        line: 1,
    };
    reject(vec![
        old_payload(call(99, Vec::new()), 10, 13),
        old_payload(call(99, Vec::new()), 9, 14),
    ]);
    reject(vec![
        old_payload(Expr::Int(0), 10, 13),
        old_payload(Expr::Int(0), 9, 14),
    ]);
    reject(vec![
        clear_let(13, CLEAR, FRAME, 10),
        clear_let(14, CLEAR, FRAME, 10),
    ]);
    reject(vec![
        clear_let(13, CLEAR, FRAME, 10),
        clear_let(14, CLEAR, 5, 9),
    ]);
    reject(vec![clear_let(13, CLEAR, FRAME, 10)]);
    reject(vec![
        Stmt::Let {
            index: 13,
            declared_schema: Some(TypeSchema::Int),
            expr: call(
                CLEAR,
                vec![
                    Expr::Var(CARRIER),
                    Expr::Var(FRAME),
                    Expr::Int(10),
                    Expr::Int(0),
                ],
            ),
            line: 1,
        },
        clear_let(14, CLEAR, FRAME, 9),
    ]);
    reject(vec![
        Stmt::Let {
            index: 13,
            declared_schema: Some(TypeSchema::Int),
            expr: call(
                CLEAR,
                vec![Expr::Var(CARRIER), Expr::Var(FRAME), call(99, Vec::new())],
            ),
            line: 1,
        },
        clear_let(14, CLEAR, FRAME, 9),
    ]);
    reject(vec![
        Stmt::Let {
            index: 13,
            declared_schema: Some(TypeSchema::Int),
            expr: call(CLEAR, vec![Expr::Int(1), Expr::Var(FRAME), Expr::Int(10)]),
            line: 1,
        },
        clear_let(14, CLEAR, FRAME, 9),
    ]);
    reject(vec![
        Stmt::Let {
            index: 13,
            declared_schema: Some(TypeSchema::Int),
            expr: call(CLEAR, vec![Expr::Var(CARRIER), Expr::Int(4), Expr::Int(10)]),
            line: 1,
        },
        clear_let(14, CLEAR, FRAME, 9),
    ]);
    reject(vec![
        Stmt::Let {
            index: 13,
            declared_schema: Some(TypeSchema::Int),
            expr: call(
                CLEAR,
                vec![Expr::Var(CARRIER), Expr::Var(99), Expr::Int(10)],
            ),
            line: 1,
        },
        clear_let(14, CLEAR, FRAME, 9),
    ]);
    reject(vec![
        Stmt::Let {
            index: 13,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Call(
                CLEAR,
                vec![TypeSchema::Int],
                vec![Expr::Var(CARRIER), Expr::Var(FRAME), Expr::Int(10)],
                None,
                None,
            ),
            line: 1,
        },
        clear_let(14, CLEAR, FRAME, 9),
    ]);
    reject(vec![
        Stmt::Let {
            index: 13,
            declared_schema: None,
            expr: call(
                CLEAR,
                vec![Expr::Var(CARRIER), Expr::Var(FRAME), Expr::Int(10)],
            ),
            line: 1,
        },
        clear_let(14, CLEAR, FRAME, 9),
    ]);
    let missing_head = vec![arm(
        vec![refresh(12, FRAME, SLOT)],
        vec![
            clear_let(13, CLEAR, FRAME, 10),
            clear_let(14, CLEAR, FRAME, 9),
        ],
    )];
    assert!(find_restore_arm(&missing_head, WRITE, CLEAR).is_none());
    let wrong_refresh = vec![arm(
        vec![assign(SLOT, SAVED), refresh(12, 6, SLOT)],
        vec![
            clear_let(13, CLEAR, FRAME, 10),
            clear_let(14, CLEAR, FRAME, 9),
        ],
    )];
    assert!(find_restore_arm(&wrong_refresh, WRITE, CLEAR).is_none());
    assert!(find_restore_arm(&body, CLEAR, CLEAR).is_none());
    assert!(!has_snapshot_write(&body, WRITE, FRAME, &[10, 9], 99));
}
