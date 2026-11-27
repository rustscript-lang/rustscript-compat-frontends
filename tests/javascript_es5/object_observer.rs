//! Observe actual generated values and real carrier cells, never JS sources.
use vm::{BuiltinFunction as B, Expr as E, Stmt as S, TypeSchema as T, ValueType as V};

/// Distinct debug-line ids for the collector's internal phases. These are
/// test-only labels: they are never JS source positions, they carry no
/// counter or heap cell, and they exist only in the diagnostic IR clone.
pub const DIAG_PHASE_INIT: u32 = 900_010;
pub const DIAG_PHASE_CLEAR: u32 = 900_011;
pub const DIAG_PHASE_ROOTS: u32 = 900_012;
pub const DIAG_PHASE_DRAIN: u32 = 900_013;
pub const DIAG_PHASE_SWEEP: u32 = 900_014;
pub const DIAG_PHASE_COALESCE: u32 = 900_015;
pub const DIAG_PHASE_COMMIT: u32 = 900_016;
/// Test-only locator for the sole typed bootstrap completion boundary.
pub const DIAG_PHASE_BOOTSTRAP: u32 = 900_017;
/// Test-only locator for entry into the frame that later exhausts fuel.
pub const DIAG_PHASE_FRAME_ENTRY: u32 = 900_018;
/// Test-only locator for the boundary immediately before the unique typed
/// bootstrap call executes (distinct from its completion boundary).
pub const DIAG_PHASE_BOOTSTRAP_CALL: u32 = 900_019;
/// Deliberate non-fuel stop label: `DIAG_STOP_BASE + phase - DIAG_PHASE_INIT`.
pub const DIAG_STOP_BASE: u32 = 910_000;
/// Test-only ROOTS cursor stop label: distinct from every phase stop label so
/// the eight-slot prefix owns its stop, while still attributing `root-scan`.
pub const DIAG_STOP_ROOTS_CURSOR: u32 = 910_100;
/// Test-only Gray exit stop label: distinct from the ROOTS cursor stop and from
/// every phase-derived label, so the gray sweep boundary owns its own stop
/// while still attributing `sweep`.
pub const DIAG_STOP_GRAY_SWEEP: u32 = 910_101;
/// Test-only Gray entry stop label: the boundary before the gray-drain prelude
/// of the target collection. It is derived from the drain phase, so it
/// attributes `gray-drain` while owning a label distinct from every other
/// stop, and it is never used for a nested phase-derived stop.
pub const DIAG_STOP_GRAY_ENTRY: u32 = 910_003;
/// Name of the single matched-layout Gray diagnostic helper. All three
/// vehicles share this one name; no vehicle-specific helper, module or debug
/// name exists.
pub const GRAY_DIAG_NAME: &str = "__compat_es5_diag_collect_gray";
/// Completed root slots after which the cursor prefix stops inside the target
/// collection, counted across the actual frame chain.
pub const ROOTS_CURSOR_SLOTS: i64 = 8;
/// Site sentinel carried by the private descriptor frame clone. It is passed
/// only by the retargeted `__compat_es5_define_properties` callsite, so the
/// active frame is identified by a mapped parameter value instead of a magic
/// prototype id or a shared capacity literal.
pub const DIAG_DESCRIPTOR_SITE: i64 = 1445;

pub fn diag_phase(label: u32) -> Option<&'static str> {
    match label {
        DIAG_PHASE_INIT => Some("init"),
        DIAG_PHASE_CLEAR => Some("clear-marks"),
        DIAG_PHASE_ROOTS => Some("root-scan"),
        DIAG_PHASE_DRAIN => Some("gray-drain"),
        DIAG_PHASE_SWEEP => Some("sweep"),
        DIAG_PHASE_COALESCE => Some("coalesce"),
        DIAG_PHASE_COMMIT => Some("counter-commit"),
        DIAG_PHASE_BOOTSTRAP => Some("typed-bootstrap-complete"),
        DIAG_PHASE_BOOTSTRAP_CALL => Some("typed-bootstrap-call"),
        DIAG_PHASE_FRAME_ENTRY => Some("frame-entry"),
        _ => None,
    }
}

/// Map a deliberate-stop line back to the phase boundary where it sits.
pub fn diag_stop_phase(label: u32) -> Option<&'static str> {
    if label == DIAG_STOP_ROOTS_CURSOR {
        return Some("root-scan");
    }
    if label == DIAG_STOP_GRAY_SWEEP {
        return Some("sweep");
    }
    label
        .checked_sub(DIAG_STOP_BASE)
        .map(|delta| DIAG_PHASE_INIT + delta)
        .and_then(diag_phase)
}

#[derive(Default)]
pub struct Observer {
    pub units: Option<usize>,
    pub force_gc: bool,
    pub uncaught: bool,
    pub type_error: bool,
    pub realm_header: bool,
    /// Default off. Enables the test-only collector wrapper/private
    /// diagnostic helper split and distinct phase line metadata in the
    /// generated IR. Ordinary gate IR is unchanged while this is false.
    pub collect_phases: bool,
    /// `(completed_at_entry, phase label)`; inserts the existing assert
    /// builtin with a literal false at that completed-collection/phase
    /// boundary. The phase label is one of the `DIAG_PHASE_*` ids.
    pub collect_stop: Option<(i64, u32)>,
}
fn call(index: u16, args: Vec<E>) -> E {
    E::Call(index, Vec::new(), args, None, None)
}
fn number(expr: E) -> E {
    E::Div(Box::new(expr), Box::new(E::Float(1.0)))
}
fn field(index: u16, name: &str) -> E {
    call(
        B::Get.call_index(),
        vec![E::Borrow(Box::new(E::Var(index))), E::String(name.into())],
    )
}
fn helper(ir: &vm::FrontendIr, name: &str) -> u16 {
    ir.functions
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("missing helper {name}"))
        .index
}
fn fresh(ir: &mut vm::FrontendIr) -> u16 {
    let index = u16::try_from(ir.locals).expect("observer slot");
    ir.locals += 1;
    index
}

/// One line of structural evidence for the generated collector body.
fn body_shape(body: &[S]) -> String {
    let mut out = Vec::with_capacity(body.len());
    for stmt in body {
        let tag = match stmt {
            S::Noop { .. } => "Noop",
            S::Let { .. } => "Let",
            S::Assign { .. } => "Assign",
            S::ClosureLet { .. } => "ClosureLet",
            S::FuncDecl { .. } => "FuncDecl",
            S::Expr { .. } => "Expr",
            S::IfElse { .. } => "IfElse",
            S::For { .. } => "For",
            S::While { .. } => "While",
            S::Break { .. } => "Break",
            S::Continue { .. } => "Continue",
            S::Drop { .. } => "Drop",
        };
        let call = match stmt {
            S::Assign { expr, .. } | S::Let { expr, .. } | S::Expr { expr, .. } => match expr {
                E::Call(target, _, args, _, _) => format!(":Call({target},{})", args.len()),
                _ => String::new(),
            },
            _ => String::new(),
        };
        out.push(format!("{tag}{call}"));
    }
    out.join(",")
}

fn zero_number(expr: &E) -> bool {
    match expr {
        E::Float(value) => *value == 0.0,
        E::Int(value) => *value == 0,
        E::Div(left, right) => zero_number(left) && matches!(**right, E::Float(1.0)),
        _ => false,
    }
}

/// True when `stmt` assigns the write helper a zero Number at a header cell
/// of the carrier. This is the strict shape of the two phase preludes we
/// rely on as locators; no line value is assumed.
fn assigns_header_zero(stmt: &S, write: u16, carrier: u16) -> bool {
    let S::Assign { expr, .. } = stmt else {
        return false;
    };
    let E::Call(target, _, args, _, _) = expr else {
        return false;
    };
    *target == write
        && args.len() == 3
        && matches!(args.first(), Some(E::Var(slot)) if *slot == carrier)
        && args.get(2).is_some_and(zero_number)
}

/// Rewrite the debug line of every statement in a region, recursing into
/// nested branches and loops so a paused ip inside the region maps back to
/// the same diagnostic phase id.
fn retag(stmt: &mut S, line: u32) {
    match stmt {
        S::Noop { line: at }
        | S::Let { line: at, .. }
        | S::Assign { line: at, .. }
        | S::ClosureLet { line: at, .. }
        | S::FuncDecl { line: at, .. }
        | S::Expr { line: at, .. }
        | S::Drop { line: at, .. }
        | S::Break { line: at }
        | S::Continue { line: at } => *at = line,
        S::IfElse {
            then_branch,
            else_branch,
            line: at,
            ..
        } => {
            *at = line;
            for inner in then_branch.iter_mut().chain(else_branch.iter_mut()) {
                retag(inner, line);
            }
        }
        S::For {
            init,
            post,
            body,
            line: at,
            ..
        } => {
            *at = line;
            retag(init, line);
            retag(post, line);
            for inner in body {
                retag(inner, line);
            }
        }
        S::While { body, line: at, .. } => {
            *at = line;
            for inner in body {
                retag(inner, line);
            }
        }
    }
}

fn stop_stmt(completed: u16, at: i64, line: u32) -> S {
    S::IfElse {
        condition: E::Eq(Box::new(E::Var(completed)), Box::new(E::Int(at))),
        then_branch: vec![S::Expr {
            expr: E::Call(
                B::Assert.call_index(),
                Vec::new(),
                vec![E::Bool(false)],
                None,
                None,
            ),
            line,
        }],
        else_branch: Vec::new(),
        line,
    }
}

/// Split the real collector into a public wrapper with its original
/// signature/index and one private diagnostic helper carrying the original
/// body plus the completed-at-entry parameter. Only the generated IR is
/// touched; no production helper, layout or counter changes. Phase
/// boundaries are located by the current typed structural contract, and
/// `sample` redirects the requested stop to a private stop-and-sample helper
/// so scalar counters can be read from that helper's own Int parameters.
fn install_collector_diag(ir: &mut vm::FrontendIr, stop: Option<(i64, u32)>, sample: Option<u16>) {
    let collect = helper(ir, "__compat_es5_heap_collect");
    let observe = helper(ir, "__compat_es5_heap_observe");
    let write = helper(ir, "__compat_ref_write");
    let declaration = ir
        .functions
        .iter()
        .find(|function| function.index == collect)
        .expect("collector declaration")
        .clone();
    let original = ir
        .function_impls
        .remove(&collect)
        .expect("collector implementation");
    let carrier = original.param_slots[0];
    let body = &original.body_stmts;
    let shape = body_shape(body);
    // The five phase loops and their preludes are structural; only the number
    // of declaration lets depends on the realm. Phase boundaries are located
    // by the current typed structural contract.
    let clear_at = body
        .iter()
        .position(|stmt| assigns_header_zero(stmt, write, carrier))
        .expect("clear-marks header reset");
    assert!(
        body[..clear_at]
            .iter()
            .enumerate()
            .all(|(at, stmt)| at == 4 || matches!(stmt, S::Let { .. })),
        "collector prologue is not the bootstrap + typed temp lets: {shape}"
    );
    assert!(
        matches!(&body[4], S::Expr { expr: E::Call(target, _, _, _, _), .. } if *target == B::Assert.call_index()),
        "collector pending-node guard is not the fourth prologue statement: {shape}"
    );
    assert!(
        body[clear_at..]
            .iter()
            .filter(|stmt| matches!(stmt, S::Assign { .. }))
            .count()
            == 16,
        "collector phase prelude/commit assignment count changed: {shape}"
    );
    assert!(
        body.iter()
            .filter(|stmt| matches!(stmt, S::While { .. }))
            .count()
            == 5,
        "collector must keep exactly five top-level phase loops: {shape}"
    );
    let clear_while = clear_at + 1;
    assert!(
        matches!(body[clear_while], S::While { .. }),
        "clear-marks loop must follow its header reset: {shape}"
    );
    let roots_at = clear_while + 1;
    assert!(
        matches!(&body[roots_at], S::Assign { expr: E::Call(target, _, _, _, _), .. } if *target != write),
        "root-scan must start at the root-top read, not a header reset: {shape}"
    );
    assert!(
        matches!(body[roots_at + 1], S::Assign { .. })
            && matches!(body[roots_at + 2], S::While { .. }),
        "root-scan read/decode/loop structure changed: {shape}"
    );
    let roots_while = roots_at + 2;
    let drain_at = roots_while + 1;
    let drain_while = (drain_at..body.len())
        .find(|at| matches!(body[*at], S::While { .. }))
        .expect("gray-drain loop");
    assert!(
        body[drain_at..drain_while - 2]
            .iter()
            .all(|stmt| matches!(stmt, S::Let { .. }))
            && matches!(body[drain_while - 2], S::Assign { .. })
            && matches!(body[drain_while - 1], S::Assign { .. }),
        "gray-drain declarations/prelude/loop structure changed: {shape}"
    );
    let sweep_at = drain_while + 1;
    let sweep_while = sweep_at + 3;
    assert!(
        (sweep_at..sweep_while).all(|at| matches!(body[at], S::Assign { .. }))
            && matches!(body[sweep_while], S::While { .. }),
        "sweep prelude/loop structure changed: {shape}"
    );
    let coalesce_at = sweep_while + 1;
    let coalesce_while = coalesce_at + 4;
    assert!(
        (coalesce_at..coalesce_while).all(|at| matches!(body[at], S::Assign { .. }))
            && matches!(body[coalesce_while], S::While { .. }),
        "coalesce prelude/loop structure changed: {shape}"
    );
    let commit_at = coalesce_while + 1;
    assert!(
        assigns_header_zero(&body[commit_at], write, carrier),
        "counter-commit must start by resetting the allocation interval: {shape}"
    );
    assert!(
        (commit_at + 1..commit_at + 4).all(|at| matches!(body[at], S::Assign { .. })),
        "counter-commit tail structure changed: {shape}"
    );
    let completed = fresh(ir);
    let stats: Vec<u16> = if sample.is_some() {
        (0..4).map(|_| fresh(ir)).collect()
    } else {
        Vec::new()
    };
    let diag = ir
        .functions
        .iter()
        .map(|function| function.index)
        .max()
        .expect("helper index")
        .checked_add(1)
        .expect("diagnostic helper index");
    let boundaries = [
        (0usize, DIAG_PHASE_INIT),
        (clear_at, DIAG_PHASE_CLEAR),
        (roots_at, DIAG_PHASE_ROOTS),
        (drain_at, DIAG_PHASE_DRAIN),
        (sweep_at, DIAG_PHASE_SWEEP),
        (coalesce_at, DIAG_PHASE_COALESCE),
        (commit_at, DIAG_PHASE_COMMIT),
    ];
    let mut rewritten = Vec::with_capacity(body.len() + 9);
    let mut next_boundary = 0usize;
    if sample.is_some() {
        for slot in &stats {
            rewritten.push(S::Let {
                index: *slot,
                declared_schema: Some(T::Int),
                expr: E::Int(0),
                line: DIAG_PHASE_INIT,
            });
        }
    }
    for (at, mut stmt) in original.body_stmts.clone().into_iter().enumerate() {
        while next_boundary < boundaries.len() && at == boundaries[next_boundary].0 {
            let (_, phase) = boundaries[next_boundary];
            if stop.is_some_and(|(_, wanted)| wanted == phase) {
                let (count, _) = stop.expect("matched stop");
                if let Some(sample) = sample {
                    let mut branch = Vec::with_capacity(5);
                    for (slot, selector) in stats.iter().zip([5, 0, 4, 8]) {
                        branch.push(S::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: *slot,
                            expr: call(observe, vec![E::Var(carrier), E::Int(selector), E::Int(0)]),
                            line: phase,
                        });
                    }
                    branch.push(S::Expr {
                        expr: E::Call(
                            sample,
                            Vec::new(),
                            stats.iter().map(|slot| E::Var(*slot)).collect(),
                            None,
                            None,
                        ),
                        line: stop_label(phase),
                    });
                    rewritten.push(S::IfElse {
                        condition: E::Eq(Box::new(E::Var(completed)), Box::new(E::Int(count))),
                        then_branch: branch,
                        else_branch: Vec::new(),
                        line: stop_label(phase),
                    });
                } else {
                    rewritten.push(stop_stmt(
                        completed,
                        count,
                        DIAG_STOP_BASE + (phase - DIAG_PHASE_INIT),
                    ));
                }
            }
            next_boundary += 1;
        }
        let line = if at < clear_at {
            DIAG_PHASE_INIT
        } else {
            boundaries
                .iter()
                .rev()
                .find(|(start, _)| *start <= at)
                .map(|(_, phase)| *phase)
                .expect("phase region")
        };
        retag(&mut stmt, line);
        rewritten.push(stmt);
    }
    // Keep the completed-at-entry parameter live through the original body:
    // a final nonnegative check before the reclaimed count is returned. It is
    // a control check only and never substitutes for the original result.
    rewritten.push(S::Expr {
        expr: E::Call(
            B::Assert.call_index(),
            Vec::new(),
            vec![E::Not(Box::new(E::Lt(
                Box::new(E::Var(completed)),
                Box::new(E::Int(0)),
            )))],
            None,
            None,
        ),
        line: DIAG_PHASE_COMMIT,
    });
    let mut diag_declaration = declaration.clone();
    diag_declaration.name = "__compat_es5_diag_collect".to_owned();
    diag_declaration.index = diag;
    diag_declaration.arity = 2;
    diag_declaration.args = vec!["carrier".to_owned(), "completed_at_entry".to_owned()];
    diag_declaration.arg_schemas = vec![declaration.arg_schemas[0].clone(), Some(T::Int)];
    diag_declaration.return_schema = Some(T::Int);
    diag_declaration.return_type = V::Int;
    diag_declaration.exported = false;
    ir.functions.push(diag_declaration);
    ir.function_impls.insert(
        diag,
        vm::compiler::FunctionImpl {
            param_slots: vec![carrier, completed],
            capture_copies: Vec::new(),
            body_stmts: rewritten,
            body_expr: original.body_expr.clone(),
            body_expr_line: original.body_expr_line,
        },
    );
    let mut wrapper = original;
    wrapper.param_slots = vec![carrier];
    wrapper.body_stmts = Vec::new();
    wrapper.body_expr = E::Call(
        diag,
        Vec::new(),
        vec![
            E::Var(carrier),
            call(observe, vec![E::Var(carrier), E::Int(5), E::Int(0)]),
        ],
        None,
        None,
    );
    ir.function_impls.insert(collect, wrapper);
}

/// One deliberate Gray boundary stop. The outer condition is the target
/// collection (`completed_at_entry == target`); the inner one is this
/// vehicle's own mode, so every run takes the same path until the target
/// collection is reached and a full-run vehicle never evaluates an assert.
/// Both guard levels and the assert carry the vehicle's own stop label, so a
/// paused ip anywhere in the guard maps back to this boundary's phase through
/// the stop-label/phase table rather than to a neighbouring phase.
fn gray_stop_guard(completed: u16, mode: u16, target: u16, wanted: i64, label: u32) -> S {
    S::IfElse {
        condition: E::Eq(Box::new(E::Var(completed)), Box::new(E::Var(target))),
        then_branch: vec![S::IfElse {
            condition: E::Eq(Box::new(E::Var(mode)), Box::new(E::Int(wanted))),
            then_branch: vec![S::Expr {
                expr: E::Call(
                    B::Assert.call_index(),
                    Vec::new(),
                    vec![E::Bool(false)],
                    None,
                    None,
                ),
                line: label,
            }],
            else_branch: Vec::new(),
            line: label,
        }],
        else_branch: Vec::new(),
        line: label,
    }
}

/// Matched-layout Gray diagnostic: the same IR for the full run, the Gray-entry
/// prefix and the Gray-exit prefix. Only the wrapper's last two Int seed
/// constants differ; the helper body, both guards, the fresh-variable order and
/// every original collector statement keep their order.
///
/// The diagnostic helper carries four mapped Int parameters
/// `[carrier, completed_at_entry, mode, target]`; the original collector body
/// moves into it unchanged apart from phase retagging and the two guarded
/// deliberate stops. Mode selects the stop at runtime: `0` never stops (full
/// run), `1` stops at the Gray-entry boundary of the target collection (before
/// the gray-drain prelude), `2` stops at the Gray-exit boundary (after the
/// gray-drain loop, before the sweep prelude). Mode and target stay dynamic
/// parameters, never compile-time folds, so all three pipelines share one
/// control topology. No ROOTS frame/cursor/capacity field is introduced.
pub fn install_gray_collector_diag(ir: &mut vm::FrontendIr, mode: i64, target: i64) {
    let collect = helper(ir, "__compat_es5_heap_collect");
    let observe = helper(ir, "__compat_es5_heap_observe");
    let write = helper(ir, "__compat_ref_write");
    let declaration = ir
        .functions
        .iter()
        .find(|function| function.index == collect)
        .expect("collector declaration")
        .clone();
    let original = ir
        .function_impls
        .remove(&collect)
        .expect("collector implementation");
    let carrier = original.param_slots[0];
    let body = &original.body_stmts;
    let shape = body_shape(body);
    let clear_at = body
        .iter()
        .position(|stmt| assigns_header_zero(stmt, write, carrier))
        .expect("clear-marks header reset");
    assert!(
        body[..clear_at]
            .iter()
            .enumerate()
            .all(|(at, stmt)| at == 4 || matches!(stmt, S::Let { .. })),
        "collector prologue is not the bootstrap + typed temp lets: {shape}"
    );
    assert!(
        matches!(&body[4], S::Expr { expr: E::Call(target, _, _, _, _), .. } if *target == B::Assert.call_index()),
        "collector pending-node guard is not the fourth prologue statement: {shape}"
    );
    assert!(
        body.iter()
            .filter(|stmt| matches!(stmt, S::While { .. }))
            .count()
            == 5,
        "collector must keep exactly five top-level phase loops: {shape}"
    );
    assert!(
        matches!(body[clear_at + 1], S::While { .. }),
        "clear-marks loop must follow its header reset: {shape}"
    );
    let roots_at = clear_at + 2;
    assert!(
        matches!(&body[roots_at], S::Assign { expr: E::Call(target, _, _, _, _), .. } if *target != write),
        "root-scan must start at the root-top read, not a header reset: {shape}"
    );
    assert!(
        matches!(body[roots_at + 1], S::Assign { .. })
            && matches!(body[roots_at + 2], S::While { .. }),
        "root-scan read/decode/loop structure changed: {shape}"
    );
    let roots_while = roots_at + 2;
    let drain_at = roots_while + 1;
    let drain_while = (drain_at..body.len())
        .find(|at| matches!(body[*at], S::While { .. }))
        .expect("gray-drain loop");
    assert!(
        body[drain_at..drain_while - 2]
            .iter()
            .all(|stmt| matches!(stmt, S::Let { .. }))
            && matches!(body[drain_while - 2], S::Assign { .. })
            && matches!(body[drain_while - 1], S::Assign { .. }),
        "gray-drain declarations/prelude/loop structure changed: {shape}"
    );
    let sweep_at = drain_while + 1;
    let sweep_while = sweep_at + 3;
    assert!(
        (sweep_at..sweep_while).all(|at| matches!(body[at], S::Assign { .. }))
            && matches!(body[sweep_while], S::While { .. }),
        "sweep prelude/loop structure changed: {shape}"
    );
    let coalesce_at = sweep_while + 1;
    let coalesce_while = coalesce_at + 4;
    assert!(
        (coalesce_at..coalesce_while).all(|at| matches!(body[at], S::Assign { .. }))
            && matches!(body[coalesce_while], S::While { .. }),
        "coalesce prelude/loop structure changed: {shape}"
    );
    let commit_at = coalesce_while + 1;
    assert!(
        assigns_header_zero(&body[commit_at], write, carrier),
        "counter-commit must start by resetting the allocation interval: {shape}"
    );
    assert!(
        (commit_at + 1..commit_at + 4).all(|at| matches!(body[at], S::Assign { .. })),
        "counter-commit tail structure changed: {shape}"
    );
    let completed = fresh(ir);
    let mode_slot = fresh(ir);
    let target_slot = fresh(ir);
    let diag = ir
        .functions
        .iter()
        .map(|function| function.index)
        .max()
        .expect("helper index")
        .checked_add(1)
        .expect("diagnostic helper index");
    let boundaries = [
        (0usize, DIAG_PHASE_INIT),
        (clear_at, DIAG_PHASE_CLEAR),
        (roots_at, DIAG_PHASE_ROOTS),
        (drain_at, DIAG_PHASE_DRAIN),
        (sweep_at, DIAG_PHASE_SWEEP),
        (coalesce_at, DIAG_PHASE_COALESCE),
        (commit_at, DIAG_PHASE_COMMIT),
    ];
    let mut rewritten = Vec::with_capacity(body.len() + 2);
    for (at, mut stmt) in original.body_stmts.clone().into_iter().enumerate() {
        let line = if at < clear_at {
            DIAG_PHASE_INIT
        } else {
            boundaries
                .iter()
                .rev()
                .find(|(start, _)| *start <= at)
                .map(|(_, phase)| *phase)
                .expect("phase region")
        };
        retag(&mut stmt, line);
        rewritten.push(stmt);
    }
    // The two deliberate stops are inserted only after the phase retagging, so
    // no later retag can clobber their own stop tags. The higher index goes in
    // first, keeping the located Gray-entry boundary valid.
    rewritten.insert(
        sweep_at,
        gray_stop_guard(completed, mode_slot, target_slot, 2, DIAG_STOP_GRAY_SWEEP),
    );
    rewritten.insert(
        drain_at,
        gray_stop_guard(completed, mode_slot, target_slot, 1, DIAG_STOP_GRAY_ENTRY),
    );
    // Keep completed-at-entry, mode and target live through the original
    // result: three simple individual nonnegative control checks, never a
    // combined condition, so every mode/target takes the same branch outcome
    // and no interval cost is introduced.
    for slot in [completed, mode_slot, target_slot] {
        rewritten.push(S::Expr {
            expr: E::Call(
                B::Assert.call_index(),
                Vec::new(),
                vec![E::Not(Box::new(E::Lt(
                    Box::new(E::Var(slot)),
                    Box::new(E::Int(0)),
                )))],
                None,
                None,
            ),
            line: DIAG_PHASE_COMMIT,
        });
    }
    let mut diag_declaration = declaration.clone();
    diag_declaration.name = GRAY_DIAG_NAME.to_owned();
    diag_declaration.index = diag;
    diag_declaration.arity = 4;
    diag_declaration.args = ["carrier", "completed_at_entry", "mode", "target"]
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    diag_declaration.arg_schemas = vec![
        declaration.arg_schemas[0].clone(),
        Some(T::Int),
        Some(T::Int),
        Some(T::Int),
    ];
    diag_declaration.return_schema = Some(T::Int);
    diag_declaration.return_type = V::Int;
    diag_declaration.exported = false;
    ir.functions.push(diag_declaration.clone());
    let diag_shape = body_shape(&rewritten);
    ir.function_impls.insert(
        diag,
        vm::compiler::FunctionImpl {
            param_slots: vec![carrier, completed, mode_slot, target_slot],
            capture_copies: Vec::new(),
            body_stmts: rewritten,
            body_expr: original.body_expr.clone(),
            body_expr_line: original.body_expr_line,
        },
    );
    let mut wrapper = original;
    wrapper.param_slots = vec![carrier];
    wrapper.body_stmts = Vec::new();
    wrapper.body_expr = E::Call(
        diag,
        Vec::new(),
        vec![
            E::Var(carrier),
            call(observe, vec![E::Var(carrier), E::Int(5), E::Int(0)]),
            E::Int(mode),
            E::Int(target),
        ],
        None,
        None,
    );
    ir.function_impls.insert(collect, wrapper);
    let mut sorted_keys: Vec<u16> = ir.function_impls.keys().copied().collect();
    sorted_keys.sort_unstable();
    GRAY_LEDGER
        .lock()
        .expect("gray ledger")
        .push(GrayLedgerEntry {
            mode,
            target,
            collect,
            diag,
            diag_arity: diag_declaration.arity,
            diag_params: diag_declaration.args.len(),
            diag_shape,
            return_schema: diag_declaration.return_schema.clone(),
            arg_schemas: diag_declaration.arg_schemas.clone(),
            wrapper_seeds: (mode, target),
            sorted_keys,
            locals: ir.locals,
            functions: ir.functions.len(),
            protos: ir
                .functions
                .iter()
                .map(|function| (function.arity, function.args.len()))
                .collect(),
        });
}

/// One captured matched-layout ROOTS pipeline: the mode/target seeds plus the
/// full diagnostic helper shape and the emitted program topology, recorded
/// during `parse_source` before any VM runs.
#[derive(Clone, Debug)]
pub struct RootsLedgerEntry {
    pub mode: i64,
    pub target: i64,
    pub diag_arity: u8,
    pub diag_params: usize,
    pub diag_shape: String,
    pub wrapper_seeds: (i64, i64),
    pub locals: usize,
    pub functions: usize,
    pub protos: Vec<(u8, usize)>,
}

static ROOTS_LEDGER: std::sync::Mutex<Vec<RootsLedgerEntry>> = std::sync::Mutex::new(Vec::new());

/// Clear the matched-layout ledger before the single focused ROOTS test.
pub fn clear_roots_ledger() {
    ROOTS_LEDGER.lock().expect("roots ledger").clear();
}

/// Snapshot the matched-layout ledger recorded so far.
pub fn roots_ledger() -> Vec<RootsLedgerEntry> {
    ROOTS_LEDGER.lock().expect("roots ledger").clone()
}

/// One captured matched-layout Gray pipeline: the mode/target seeds plus the
/// diagnostic helper's logical identity, declared schema, body shape and the
/// sorted IR emission list, recorded during `parse_source` before any VM runs.
/// The runtime prototype id is never assumed to equal the logical index; the
/// consumer derives it from the actual compiled program against this ordered
/// list.
#[derive(Clone, Debug)]
pub struct GrayLedgerEntry {
    pub mode: i64,
    pub target: i64,
    pub collect: u16,
    pub diag: u16,
    pub diag_arity: u8,
    pub diag_params: usize,
    pub diag_shape: String,
    pub return_schema: Option<T>,
    pub arg_schemas: Vec<Option<T>>,
    pub wrapper_seeds: (i64, i64),
    pub sorted_keys: Vec<u16>,
    pub locals: usize,
    pub functions: usize,
    pub protos: Vec<(u8, usize)>,
}

static GRAY_LEDGER: std::sync::Mutex<Vec<GrayLedgerEntry>> = std::sync::Mutex::new(Vec::new());

/// Clear the matched-layout Gray ledger before the single focused Gray test.
pub fn clear_gray_ledger() {
    GRAY_LEDGER.lock().expect("gray ledger").clear();
}

/// Snapshot the matched-layout Gray ledger recorded so far.
pub fn gray_ledger() -> Vec<GrayLedgerEntry> {
    GRAY_LEDGER.lock().expect("gray ledger").clone()
}

/// Restore the two deliberate ROOTS stop lines after the retagging rewrite.
/// The rewrite stamps every nested statement with its region phase line, so
/// both stops are found by their unique mode/target/scanned condition shape
/// and given back their own stop lines. Counter bumps keep region lines.
#[allow(clippy::too_many_arguments)]
fn restore_roots_stop_lines(
    stmts: &mut [S],
    mode_slot: u16,
    completed: u16,
    target_slot: u16,
    scanned: u16,
    entry_line: u32,
    cursor_line: u32,
    entry_found: &mut bool,
    cursor_found: &mut bool,
) {
    for stmt in stmts.iter_mut() {
        match stmt {
            S::IfElse {
                condition,
                then_branch,
                else_branch,
                line,
            } => {
                if let E::And(left, right) = condition {
                    let is_entry = matches!(left.as_ref(), E::Eq(l, r) if matches!((l.as_ref(), r.as_ref()), (E::Var(m), E::Int(1)) if *m == mode_slot))
                        && matches!(right.as_ref(), E::Eq(l, r) if matches!((l.as_ref(), r.as_ref()), (E::Var(c), E::Var(t)) if *c == completed && *t == target_slot));
                    let is_cursor = matches!(left.as_ref(), E::And(a, b) if matches!(a.as_ref(), E::Eq(l, r) if matches!((l.as_ref(), r.as_ref()), (E::Var(m), E::Int(2)) if *m == mode_slot)) && matches!(b.as_ref(), E::Eq(l, r) if matches!((l.as_ref(), r.as_ref()), (E::Var(c), E::Var(t)) if *c == completed && *t == target_slot)))
                        && matches!(right.as_ref(), E::Eq(l, r) if matches!((l.as_ref(), r.as_ref()), (E::Var(s), E::Int(ROOTS_CURSOR_SLOTS)) if *s == scanned));
                    if is_entry || is_cursor {
                        *line = if is_entry { entry_line } else { cursor_line };
                        for inner in then_branch.iter_mut() {
                            if let S::Expr {
                                line: inner_line, ..
                            } = inner
                            {
                                *inner_line = *line;
                            }
                        }
                        if is_entry {
                            assert!(!*entry_found, "exactly one entry stop");
                            *entry_found = true;
                        } else {
                            assert!(!*cursor_found, "exactly one cursor stop");
                            *cursor_found = true;
                        }
                    }
                }
                restore_roots_stop_lines(
                    then_branch,
                    mode_slot,
                    completed,
                    target_slot,
                    scanned,
                    entry_line,
                    cursor_line,
                    entry_found,
                    cursor_found,
                );
                restore_roots_stop_lines(
                    else_branch,
                    mode_slot,
                    completed,
                    target_slot,
                    scanned,
                    entry_line,
                    cursor_line,
                    entry_found,
                    cursor_found,
                );
            }
            S::While { body, .. } => restore_roots_stop_lines(
                body,
                mode_slot,
                completed,
                target_slot,
                scanned,
                entry_line,
                cursor_line,
                entry_found,
                cursor_found,
            ),
            _ => {}
        }
    }
}

/// Matched-layout ROOTS diagnostic: the same IR for the full run, the
/// ROOTS-entry prefix and the eight-slot cursor prefix. Only the wrapper's
/// mode/target seed constants differ; the helper body, its branches and every
/// original collector statement/check/marker call keep their order.
///
/// The diagnostic helper carries nine mapped Int parameters
/// `[carrier, completed_at_entry, frame, cursor, capacity, scanned, nonempty,
/// mode, target]` so the actual frame pointer `f`, cursor `e`, capacity `cap`
/// and the per-collection `scanned`/`nonempty` counters are read live through
/// the real `parameter_slots`/`frame.local_base` mapping. The existing `f`/`e`
/// /`cap` locals become parameters (their zero `Let` inits are removed); the
/// original per-frame/per-slot `Assign` updates are untouched. `scanned`
/// increments after the slot is fully processed and `e` incremented;
/// `nonempty` increments after the successful marker call. Both counters reset
/// at collection entry only; no managed heap state changes.
///
/// Mode selects the stop at runtime: `0` never stops (full run), `1` stops
/// before the first slot of the target collection (after the first frame's
/// capacity decode and cursor zeroing, so the reported frame/capacity/cursor
/// are valid), `2` stops after eight completed slots across the actual frame
/// chain of the target collection. Mode/target stay dynamic parameters, never
/// compile-time folds, so all three pipelines share one control topology.
pub fn install_roots_collector_diag(ir: &mut vm::FrontendIr, mode: i64, target: i64) {
    let collect = helper(ir, "__compat_es5_heap_collect");
    let observe = helper(ir, "__compat_es5_heap_observe");
    let write = helper(ir, "__compat_ref_write");
    let declaration = ir
        .functions
        .iter()
        .find(|function| function.index == collect)
        .expect("collector declaration")
        .clone();
    let original = ir
        .function_impls
        .remove(&collect)
        .expect("collector implementation");
    let carrier = original.param_slots[0];
    // The existing frame/cursor/capacity locals become diagnostic parameters
    // below; their zero `Let` inits are dropped during the rewrite by slot,
    // never by position, so every located region index stays valid throughout.
    // Slot identities come from the independently located root-scan loop, not
    // from a global loop search: another phase loop also tests Not(Eq(.., 0)).
    let body = &original.body_stmts;
    let shape = body_shape(body);
    let clear_at = body
        .iter()
        .position(|stmt| assigns_header_zero(stmt, write, carrier))
        .expect("clear-marks header reset");
    assert!(
        body[..clear_at]
            .iter()
            .enumerate()
            .all(|(at, stmt)| at == 4 || matches!(stmt, S::Let { .. })),
        "collector prologue is not the bootstrap + typed temp lets: {shape}"
    );
    assert!(
        matches!(&body[4], S::Expr { expr: E::Call(target, _, _, _, _), .. } if *target == B::Assert.call_index()),
        "collector pending-node guard is not the fourth prologue statement: {shape}"
    );
    assert!(
        body[clear_at..]
            .iter()
            .filter(|stmt| matches!(stmt, S::Assign { .. }))
            .count()
            == 16,
        "collector phase prelude/commit assignment count changed: {shape}"
    );
    assert!(
        body.iter()
            .filter(|stmt| matches!(stmt, S::While { .. }))
            .count()
            == 5,
        "collector must keep exactly five top-level phase loops: {shape}"
    );
    let clear_while = clear_at + 1;
    assert!(
        matches!(body[clear_while], S::While { .. }),
        "clear-marks loop must follow its header reset: {shape}"
    );
    let roots_at = clear_while + 1;
    assert!(
        matches!(&body[roots_at], S::Assign { expr: E::Call(target, _, _, _, _), .. } if *target != write),
        "root-scan must start at the root-top read, not a header reset: {shape}"
    );
    assert!(
        matches!(body[roots_at + 1], S::Assign { .. })
            && matches!(body[roots_at + 2], S::While { .. }),
        "root-scan read/decode/loop structure changed: {shape}"
    );
    let roots_while = roots_at + 2;
    let drain_at = roots_while + 1;
    let drain_while = (drain_at..body.len())
        .find(|at| matches!(body[*at], S::While { .. }))
        .expect("gray-drain loop");
    assert!(
        body[drain_at..drain_while - 2]
            .iter()
            .all(|stmt| matches!(stmt, S::Let { .. }))
            && matches!(body[drain_while - 2], S::Assign { .. })
            && matches!(body[drain_while - 1], S::Assign { .. }),
        "gray-drain declarations/prelude/loop structure changed: {shape}"
    );
    let sweep_at = drain_while + 1;
    let sweep_while = sweep_at + 3;
    assert!(
        (sweep_at..sweep_while).all(|at| matches!(body[at], S::Assign { .. }))
            && matches!(body[sweep_while], S::While { .. }),
        "sweep prelude/loop structure changed: {shape}"
    );
    let coalesce_at = sweep_while + 1;
    let coalesce_while = coalesce_at + 4;
    assert!(
        (coalesce_at..coalesce_while).all(|at| matches!(body[at], S::Assign { .. }))
            && matches!(body[coalesce_while], S::While { .. }),
        "coalesce prelude/loop structure changed: {shape}"
    );
    let commit_at = coalesce_while + 1;
    assert!(
        assigns_header_zero(&body[commit_at], write, carrier),
        "counter-commit must start by resetting the allocation interval: {shape}"
    );
    assert!(
        (commit_at + 1..commit_at + 4).all(|at| matches!(body[at], S::Assign { .. })),
        "counter-commit tail structure changed: {shape}"
    );
    let (frame_slot, cursor_slot, capacity_slot) = match &body[roots_while] {
        S::While {
            condition: E::Not(boxed),
            body: outer_body,
            ..
        } => {
            let frame = match boxed.as_ref() {
                E::Eq(left, right) => match (left.as_ref(), right.as_ref()) {
                    (E::Var(slot), E::Int(0)) | (E::Int(0), E::Var(slot)) => *slot,
                    _ => panic!("root-scan loop tests its frame against zero: {shape}"),
                },
                _ => panic!("root-scan loop tests its frame against zero: {shape}"),
            };
            let mut found: Option<(u16, u16)> = None;
            for (at, stmt) in outer_body.iter().enumerate() {
                let S::While {
                    condition: E::Lt(left, right),
                    ..
                } = stmt
                else {
                    continue;
                };
                let (E::Var(cursor), E::Var(capacity)) = (left.as_ref(), right.as_ref()) else {
                    continue;
                };
                if matches!(
                    outer_body.get(at.checked_sub(1).expect("cursor seed")),
                    Some(S::Assign { index, expr: E::Int(0), .. }) if *index == *cursor
                ) {
                    assert!(found.is_none(), "the scan loop owns exactly one slot loop");
                    found = Some((*cursor, *capacity));
                }
            }
            let (cursor, capacity) = found.expect("ROOTS slot loop");
            (frame, cursor, capacity)
        }
        other => panic!("root-scan loop must stay a while loop: {other:?}"),
    };
    let completed = fresh(ir);
    let scanned = fresh(ir);
    let nonempty = fresh(ir);
    let mode_slot = fresh(ir);
    let target_slot = fresh(ir);
    let diag = ir
        .functions
        .iter()
        .map(|function| function.index)
        .max()
        .expect("helper index")
        .checked_add(1)
        .expect("diagnostic helper index");
    let boundaries = [
        (0usize, DIAG_PHASE_INIT),
        (clear_at, DIAG_PHASE_CLEAR),
        (roots_at, DIAG_PHASE_ROOTS),
        (drain_at, DIAG_PHASE_DRAIN),
        (sweep_at, DIAG_PHASE_SWEEP),
        (coalesce_at, DIAG_PHASE_COALESCE),
        (commit_at, DIAG_PHASE_COMMIT),
    ];
    // Nested ROOTS surgery on a working clone: the top-level rewrite below
    // keeps these indices stable because it only inserts at the top level.
    let mut nested = body.clone();
    let nested_outer = match &mut nested[roots_while] {
        S::While { body, .. } => body,
        other => panic!("root-scan loop must stay a while loop: {other:?}"),
    };
    let seed_at = nested_outer
        .iter()
        .position(|stmt| {
            matches!(stmt, S::Assign { index, expr: E::Int(0), .. } if *index == cursor_slot)
        })
        .expect("cursor zero seed");
    let nested_inner_at = seed_at + 1;
    match &nested_outer[nested_inner_at] {
        S::While {
            condition: E::Lt(left, right),
            ..
        } if matches!((left.as_ref(), right.as_ref()), (E::Var(cursor), E::Var(capacity)) if *cursor == cursor_slot && *capacity == capacity_slot) =>
            {}
        other => panic!("slot loop must follow its cursor seed: {other:?}"),
    }
    // Entry prefix stop: after the first frame's capacity decode and cursor
    // zeroing, before the first slot. Fires only for the target collection in
    // entry mode; the frame/capacity/cursor parameters are valid here.
    let entry_stop = S::IfElse {
        condition: E::And(
            Box::new(E::Eq(Box::new(E::Var(mode_slot)), Box::new(E::Int(1)))),
            Box::new(E::Eq(
                Box::new(E::Var(completed)),
                Box::new(E::Var(target_slot)),
            )),
        ),
        then_branch: vec![S::Expr {
            expr: E::Call(
                B::Assert.call_index(),
                Vec::new(),
                vec![E::Bool(false)],
                None,
                None,
            ),
            line: DIAG_STOP_BASE + (DIAG_PHASE_ROOTS - DIAG_PHASE_INIT),
        }],
        else_branch: Vec::new(),
        line: DIAG_STOP_BASE + (DIAG_PHASE_ROOTS - DIAG_PHASE_INIT),
    };
    nested_outer.insert(nested_inner_at, entry_stop);
    // The slot loop shifted by the entry stop; borrow it again for the
    // scanned/cursor surgery below.
    let slot_loop = match &mut nested_outer[nested_inner_at + 1] {
        S::While { body, .. } => body,
        other => panic!("slot loop must stay a while loop: {other:?}"),
    };
    let marker_at = slot_loop
        .iter()
        .position(|stmt| match stmt {
            S::IfElse {
                condition: E::Not(boxed),
                then_branch,
                ..
            } => match boxed.as_ref() {
                E::Eq(left, right) => {
                    matches!(
                        (left.as_ref(), right.as_ref()),
                        (E::Var(_), E::Int(0)) | (E::Int(0), E::Var(_))
                    ) && then_branch.len() == 4
                }
                _ => false,
            },
            _ => false,
        })
        .expect("nonempty marker branch");
    match &mut slot_loop[marker_at] {
        S::IfElse { then_branch, .. } => then_branch.push(S::Assign {
            kind: vm::AssignmentKind::Set,
            index: nonempty,
            expr: E::Add(Box::new(E::Var(nonempty)), Box::new(E::Int(1))),
            line: DIAG_PHASE_ROOTS,
        }),
        other => panic!("marker branch must stay a conditional: {other:?}"),
    }
    let bump_at = slot_loop
        .iter()
        .position(|stmt| {
            matches!(stmt, S::Assign { index, expr: E::Add(..), .. } if *index == cursor_slot)
        })
        .expect("cursor increment");
    // Scanned counts after the slot is fully processed and the cursor moved;
    // the cursor stop reports the live frame/cursor/capacity after eight
    // completed slots of the target collection.
    slot_loop.insert(
        bump_at + 1,
        S::Assign {
            kind: vm::AssignmentKind::Set,
            index: scanned,
            expr: E::Add(Box::new(E::Var(scanned)), Box::new(E::Int(1))),
            line: DIAG_PHASE_ROOTS,
        },
    );
    slot_loop.insert(
        bump_at + 2,
        S::IfElse {
            condition: E::And(
                Box::new(E::And(
                    Box::new(E::Eq(Box::new(E::Var(mode_slot)), Box::new(E::Int(2)))),
                    Box::new(E::Eq(
                        Box::new(E::Var(completed)),
                        Box::new(E::Var(target_slot)),
                    )),
                )),
                Box::new(E::Eq(
                    Box::new(E::Var(scanned)),
                    Box::new(E::Int(ROOTS_CURSOR_SLOTS)),
                )),
            ),
            then_branch: vec![S::Expr {
                expr: E::Call(
                    B::Assert.call_index(),
                    Vec::new(),
                    vec![E::Bool(false)],
                    None,
                    None,
                ),
                line: DIAG_STOP_ROOTS_CURSOR,
            }],
            else_branch: Vec::new(),
            line: DIAG_STOP_ROOTS_CURSOR,
        },
    );
    let mut rewritten = Vec::with_capacity(nested.len() + 4);
    let mut dropped = 0usize;
    for (at, mut stmt) in nested.into_iter().enumerate() {
        if matches!(&stmt, S::Let { index, expr: E::Int(0), .. } if *index == frame_slot || *index == cursor_slot || *index == capacity_slot)
        {
            dropped += 1;
            continue;
        }
        if at == 0 {
            // Per-collection counter reset at entry only; the completed
            // ordinal, frame, cursor, capacity, mode and target parameters
            // ride through untouched.
            for slot in [scanned, nonempty] {
                rewritten.push(S::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: slot,
                    expr: E::Int(0),
                    line: DIAG_PHASE_INIT,
                });
            }
        }
        let line = if at < clear_at {
            DIAG_PHASE_INIT
        } else {
            boundaries
                .iter()
                .rev()
                .find(|(start, _)| *start <= at)
                .map(|(_, phase)| *phase)
                .expect("phase region")
        };
        retag(&mut stmt, line);
        rewritten.push(stmt);
    }
    assert_eq!(
        dropped, 3,
        "exactly the frame/cursor/capacity zero lets become parameters"
    );
    // The rewrite stamps every nested statement with its region phase line.
    // Give both deliberate stops back their own stop lines by shape before
    // any keep-alive check is appended.
    let mut entry_restored = false;
    let mut cursor_restored = false;
    restore_roots_stop_lines(
        &mut rewritten,
        mode_slot,
        completed,
        target_slot,
        scanned,
        DIAG_STOP_BASE + (DIAG_PHASE_ROOTS - DIAG_PHASE_INIT),
        DIAG_STOP_ROOTS_CURSOR,
        &mut entry_restored,
        &mut cursor_restored,
    );
    assert!(
        entry_restored && cursor_restored,
        "both deliberate stops keep their own lines"
    );
    // Keep the completed-at-entry parameter live through the original body and
    // keep every carried ROOTS parameter observable at a fuel stop: control
    // checks only, never substitutes for the original result.
    rewritten.push(S::Expr {
        expr: E::Call(
            B::Assert.call_index(),
            Vec::new(),
            vec![E::Not(Box::new(E::Lt(
                Box::new(E::Var(completed)),
                Box::new(E::Int(0)),
            )))],
            None,
            None,
        ),
        line: DIAG_PHASE_COMMIT,
    });
    rewritten.push(S::Expr {
        expr: E::Call(
            B::Assert.call_index(),
            Vec::new(),
            vec![E::And(
                Box::new(E::Not(Box::new(E::Lt(
                    Box::new(E::Var(frame_slot)),
                    Box::new(E::Int(0)),
                )))),
                Box::new(E::And(
                    Box::new(E::Not(Box::new(E::Lt(
                        Box::new(E::Var(cursor_slot)),
                        Box::new(E::Int(0)),
                    )))),
                    Box::new(E::And(
                        Box::new(E::Not(Box::new(E::Lt(
                            Box::new(E::Var(capacity_slot)),
                            Box::new(E::Int(0)),
                        )))),
                        Box::new(E::And(
                            Box::new(E::Not(Box::new(E::Lt(
                                Box::new(E::Var(scanned)),
                                Box::new(E::Int(0)),
                            )))),
                            Box::new(E::Not(Box::new(E::Lt(
                                Box::new(E::Var(nonempty)),
                                Box::new(E::Int(0)),
                            )))),
                        )),
                    )),
                )),
            )],
            None,
            None,
        ),
        line: DIAG_PHASE_COMMIT,
    });
    let mut diag_declaration = declaration.clone();
    diag_declaration.name = "__compat_es5_diag_collect_roots".to_owned();
    diag_declaration.index = diag;
    diag_declaration.arity = 9;
    diag_declaration.args = [
        "carrier",
        "completed_at_entry",
        "frame",
        "cursor",
        "capacity",
        "scanned",
        "nonempty",
        "mode",
        "target",
    ]
    .iter()
    .map(|name| (*name).to_owned())
    .collect();
    diag_declaration.arg_schemas = vec![
        declaration.arg_schemas[0].clone(),
        Some(T::Int),
        Some(T::Int),
        Some(T::Int),
        Some(T::Int),
        Some(T::Int),
        Some(T::Int),
        Some(T::Int),
        Some(T::Int),
    ];
    diag_declaration.return_schema = Some(T::Int);
    diag_declaration.return_type = V::Int;
    diag_declaration.exported = false;
    ir.functions.push(diag_declaration);
    let diag_shape = body_shape(&rewritten);
    ir.function_impls.insert(
        diag,
        vm::compiler::FunctionImpl {
            param_slots: vec![
                carrier,
                completed,
                frame_slot,
                cursor_slot,
                capacity_slot,
                scanned,
                nonempty,
                mode_slot,
                target_slot,
            ],
            capture_copies: Vec::new(),
            body_stmts: rewritten,
            body_expr: original.body_expr.clone(),
            body_expr_line: original.body_expr_line,
        },
    );
    let mut wrapper = original;
    wrapper.param_slots = vec![carrier];
    wrapper.body_stmts = Vec::new();
    wrapper.body_expr = E::Call(
        diag,
        Vec::new(),
        vec![
            E::Var(carrier),
            call(observe, vec![E::Var(carrier), E::Int(5), E::Int(0)]),
            E::Int(0),
            E::Int(0),
            E::Int(0),
            E::Int(0),
            E::Int(0),
            E::Int(mode),
            E::Int(target),
        ],
        None,
        None,
    );
    ir.function_impls.insert(collect, wrapper);
    ROOTS_LEDGER
        .lock()
        .expect("roots ledger")
        .push(RootsLedgerEntry {
            mode,
            target,
            diag_arity: 9,
            diag_params: 9,
            diag_shape,
            wrapper_seeds: (mode, target),
            locals: ir.locals,
            functions: ir.functions.len(),
            protos: ir
                .functions
                .iter()
                .map(|function| (function.arity, function.args.len()))
                .collect(),
        });
}
/// True for the collector's emitted last-key cache decision: the pair compare
/// (two decoded key cells against the cached pair) whose hit arm reads the one
/// cached base and whose miss arm is the original checked-base/brand/latch
/// sequence ending in a literal `true`. The enclosing validity gate is matched
/// separately because it also owns the never-validated miss path.
fn is_cache_decision(stmt: &S) -> bool {
    let S::IfElse {
        condition,
        then_branch,
        else_branch,
        ..
    } = stmt
    else {
        return false;
    };
    let condition: &E = condition;
    matches!(condition, E::And(..))
        && then_branch.len() == 2
        && else_branch.len() == 7
        && then_branch.iter().all(|stmt| {
            matches!(
                stmt,
                S::Assign {
                    expr: E::Var(_),
                    ..
                }
            )
        })
        && matches!(
            else_branch.last(),
            Some(S::Assign {
                expr: E::Bool(true),
                ..
            })
        )
}

/// True for the cache validity gate: `if valid { if pair==cached { .. } else {
/// miss } } else { miss }`. The gate owns both miss paths, so the miss counter
/// belongs to this shape rather than to the inner decision alone.
fn is_cache_gate(stmt: &S) -> bool {
    let S::IfElse {
        condition,
        then_branch,
        else_branch,
        ..
    } = stmt
    else {
        return false;
    };
    let condition: &E = condition;
    matches!(condition, E::Var(_))
        && then_branch.len() == 1
        && is_cache_decision(&then_branch[0])
        && else_branch.len() == 7
        && matches!(
            else_branch.last(),
            Some(S::Assign {
                expr: E::Bool(true),
                ..
            })
        )
}

/// Arm discriminator of a `kind == Int` dispatch branch, if this statement is
/// one. The collector's object/function arms are selected by exactly this
/// shape; nothing here assumes a slot number.
fn kind_dispatch(stmt: &S) -> Option<i64> {
    let S::IfElse { condition, .. } = stmt else {
        return None;
    };
    let condition: &E = condition;
    let E::Eq(left, right) = condition else {
        return None;
    };
    match (left.as_ref(), right.as_ref()) {
        (E::Var(_), E::Int(kind)) => Some(*kind),
        _ => None,
    }
}

fn bump(slot: u16) -> S {
    S::Assign {
        kind: vm::AssignmentKind::Set,
        index: slot,
        expr: E::Add(Box::new(E::Var(slot)), Box::new(E::Int(1))),
        line: DIAG_PHASE_DRAIN,
    }
}

/// Insert the hit/miss counters into every emitted cache decision, tagging
/// each by the kind arm (`3` object, `4` function) that encloses it.
fn patch_cache_branches(
    stmts: &mut [S],
    arm: Option<i64>,
    object: (u16, u16),
    function: (u16, u16),
) -> usize {
    let mut patched = 0;
    for stmt in stmts.iter_mut() {
        let gate = is_cache_gate(stmt);
        let nested = kind_dispatch(stmt);
        if gate {
            let (hit, miss) = match arm {
                Some(3) => object,
                Some(4) => function,
                other => panic!("cache decision in unexpected arm {other:?}"),
            };
            let S::IfElse {
                then_branch,
                else_branch,
                ..
            } = stmt
            else {
                unreachable!("matched as a cache gate")
            };
            // Both miss paths are the same emitted block: invalid, or valid
            // with a different pair.
            else_branch.insert(0, bump(miss));
            let S::IfElse {
                then_branch: hit_arm,
                else_branch: miss_arm,
                ..
            } = &mut then_branch[0]
            else {
                unreachable!("gate then-branch is the pair decision")
            };
            hit_arm.insert(0, bump(hit));
            miss_arm.insert(0, bump(miss));
            patched += 1;
            continue;
        }
        match stmt {
            S::IfElse {
                then_branch,
                else_branch,
                ..
            } => {
                let inner = nested.or(arm);
                patched += patch_cache_branches(then_branch, inner, object, function);
                patched += patch_cache_branches(else_branch, arm, object, function);
            }
            S::While { body, .. } => {
                patched += patch_cache_branches(body, arm, object, function);
            }
            _ => {}
        }
    }
    patched
}

/// Static contract of the emitted cache decision, checked on the real
/// production IR (no VM run, no counters):
/// * the gate condition is the dedicated validity Bool, never a slot/gen value;
/// * the pair compare is exactly two `slot == slot` equalities, so no zero
///   literal can act as an "empty" sentinel and slot 0 with a positive
///   generation takes the same path as any other slot;
/// * the hit arm reads only the base the miss arm latched;
/// * the miss arm keeps the original checked-base/brand proof first and only
///   then latches slot, generation, base and, last, validity.
pub fn assert_cache_shape(ir: &vm::FrontendIr) {
    let collect = helper(ir, "__compat_es5_heap_collect");
    let body = &ir
        .function_impls
        .get(&collect)
        .expect("collector implementation")
        .body_stmts;
    let mut gates = Vec::new();
    walk_cache_gates(body, None, &mut gates);
    assert_eq!(
        gates.iter().map(|(arm, _)| *arm).collect::<Vec<_>>(),
        vec![3, 4],
        "exactly one cache decision in the object arm and one in the function arm"
    );
    for (arm, gate) in gates {
        let S::IfElse {
            condition,
            then_branch,
            else_branch: _,
            ..
        } = gate
        else {
            unreachable!("walked cache gates are decisions")
        };
        let condition: &E = condition;
        let E::Var(valid_slot) = condition else {
            panic!("arm {arm}: cache gate must be the dedicated validity Bool, got {condition:?}");
        };
        let valid = *valid_slot;
        let S::IfElse {
            condition: pair,
            then_branch: hit,
            else_branch: miss,
            ..
        } = &then_branch[0]
        else {
            panic!("arm {arm}: validity gate must contain the pair decision");
        };
        let pair: &E = pair;
        let E::And(left, right) = pair else {
            panic!("arm {arm}: pair compare must be And(Eq,Eq), got {pair:?}");
        };
        for half in [left.as_ref(), right.as_ref()] {
            let E::Eq(a, b) = half else {
                panic!("arm {arm}: pair compare half must be Eq, got {half:?}");
            };
            assert!(
                matches!(a.as_ref(), E::Var(_)) && matches!(b.as_ref(), E::Var(_)),
                "arm {arm}: pair compare must compare two slots without any literal, got {half:?}"
            );
        }
        assert_eq!(miss.len(), 7, "arm {arm}: miss arm shape");
        assert!(
            matches!(&miss[0], S::Assign { expr: E::Call(target, _, args, _, _), .. }
                if *target == ir
                    .functions
                    .iter()
                    .find(|f| f.name == "__compat_es5_checked_base")
                    .map(|f| f.index)
                    .unwrap_or(u16::MAX)
                    && args.len() == 3),
            "arm {arm}: the miss arm still runs the original checked_base first"
        );
        assert!(
            matches!(
                &miss[1],
                S::Expr {
                    expr: E::Call(..),
                    ..
                }
            ),
            "arm {arm}: the miss arm keeps the String-brand assert after checked_base"
        );
        let latched: Vec<u16> = miss[2..]
            .iter()
            .filter_map(|stmt| match stmt {
                S::Assign { index, .. } => Some(*index),
                _ => None,
            })
            .collect();
        assert_eq!(
            latched.len(),
            5,
            "arm {arm}: miss arm latches slot/generation/base/valid"
        );
        assert!(
            matches!(&miss[6], S::Assign { index, expr: E::Bool(true), .. } if *index == latched[4]),
            "arm {arm}: validity is latched last and only after the proof"
        );
        // latched = [key_base, slot, generation, base, valid]
        let base = latched[3];
        assert_ne!(
            base, valid,
            "arm {arm}: the cached base is a dedicated slot"
        );
        assert_eq!(hit.len(), 2, "arm {arm}: hit arm shape");
        assert!(
            hit.iter()
                .all(|stmt| matches!(stmt, S::Assign { expr: E::Var(slot), .. } if *slot == base)),
            "arm {arm}: both hit outputs must read the latched cached base"
        );
    }
}

fn walk_cache_gates<'a>(stmts: &'a [S], arm: Option<i64>, out: &mut Vec<(i64, &'a S)>) {
    for stmt in stmts {
        let nested = kind_dispatch(stmt);
        if is_cache_gate(stmt) {
            out.push((arm.expect("cache gate outside a kind arm"), stmt));
            continue;
        }
        match stmt {
            S::IfElse {
                then_branch,
                else_branch,
                ..
            } => {
                walk_cache_gates(then_branch, nested.or(arm), out);
                walk_cache_gates(else_branch, arm, out);
            }
            S::While { body, .. } => walk_cache_gates(body, arm, out),
            _ => {}
        }
    }
}

/// Default-off instrumentation of the production collector IR: four Int
/// counters live in the helper (so they start at zero for every collection),
/// the actual emitted hit/miss decisions increment them per kind arm, and a
/// private stop helper carries them out at a chosen completed-collection
/// boundary. No production helper, field or counter is added.
pub fn install_cache_probe(ir: &mut vm::FrontendIr, stop_at: i64) -> usize {
    let collect = helper(ir, "__compat_es5_heap_collect");
    let observe = helper(ir, "__compat_es5_heap_observe");
    let template = ir
        .functions
        .iter()
        .find(|function| function.index == observe)
        .expect("observer declaration")
        .clone();
    let mut declaration = template;
    declaration.name = "__compat_es5_cache_stop_sample".to_owned();
    declaration.index = ir
        .functions
        .iter()
        .map(|function| function.index)
        .max()
        .expect("helper index")
        .checked_add(1)
        .expect("cache sample index");
    declaration.arity = 5;
    declaration.args = [
        "collections",
        "object_hits",
        "object_misses",
        "function_hits",
        "function_misses",
    ]
    .iter()
    .map(|name| (*name).to_owned())
    .collect();
    declaration.arg_schemas = vec![Some(T::Int); 5];
    declaration.return_schema = Some(T::Int);
    declaration.return_type = V::Int;
    declaration.exported = false;
    let sample = declaration.index;
    let sample_slots: Vec<u16> = (0..5).map(|_| fresh(ir)).collect();
    ir.functions.push(declaration);
    ir.function_impls.insert(
        sample,
        vm::compiler::FunctionImpl {
            param_slots: sample_slots,
            capture_copies: Vec::new(),
            body_stmts: vec![S::Expr {
                expr: E::Call(
                    B::Assert.call_index(),
                    Vec::new(),
                    vec![E::Bool(false)],
                    None,
                    None,
                ),
                line: DIAG_PHASE_COMMIT,
            }],
            body_expr: E::Int(0),
            body_expr_line: DIAG_PHASE_COMMIT,
        },
    );
    let carrier = ir
        .function_impls
        .get(&collect)
        .expect("collector implementation")
        .param_slots[0];
    let slots: Vec<u16> = (0..4).map(|_| fresh(ir)).collect();
    let implementation = ir
        .function_impls
        .get_mut(&collect)
        .expect("collector implementation");
    for slot in &slots {
        implementation.body_stmts.insert(
            0,
            S::Let {
                index: *slot,
                declared_schema: Some(T::Int),
                expr: E::Int(0),
                line: DIAG_PHASE_INIT,
            },
        );
    }
    let patched = patch_cache_branches(
        &mut implementation.body_stmts,
        None,
        (slots[0], slots[1]),
        (slots[2], slots[3]),
    );
    let collections = call(observe, vec![E::Var(carrier), E::Int(5), E::Int(0)]);
    implementation.body_stmts.push(S::IfElse {
        condition: E::Not(Box::new(E::Lt(
            Box::new(collections.clone()),
            Box::new(E::Int(stop_at)),
        ))),
        then_branch: vec![S::Expr {
            expr: E::Call(
                sample,
                Vec::new(),
                vec![
                    collections,
                    E::Var(slots[0]),
                    E::Var(slots[1]),
                    E::Var(slots[2]),
                    E::Var(slots[3]),
                ],
                None,
                None,
            ),
            line: DIAG_PHASE_COMMIT,
        }],
        else_branch: Vec::new(),
        line: DIAG_PHASE_COMMIT,
    });
    patched
}

impl vm::SourcePlugin for Observer {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut d = vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
        d.id = vm::FrontendId::new("javascript-es5-object-observer");
        d
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = vm::SourcePlugin::parse_source(
            &pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN,
            source,
            context,
        )?;
        let make = helper(&ir, "__compat_ref_make");
        let read = helper(&ir, "__compat_ref_read");
        let write = helper(&ir, "__compat_ref_write");
        let checked = helper(&ir, "__compat_es5_checked_base");
        let carrier = ir
            .stmts
            .iter()
            .find_map(|s| match s {
                S::Let {
                    index,
                    expr: E::Call(target, _, _, _, _),
                    ..
                } if *target == make => Some(*index),
                _ => None,
            })
            .expect("real carrier");
        if self.force_gc {
            let begin = helper(&ir, "__compat_es5_node_begin");
            let sink = fresh(&mut ir);
            let body = ir.function_impls.get_mut(&begin).expect("real begin");
            let c = body.param_slots[0];
            body.body_stmts.insert(
                0,
                S::Let {
                    index: sink,
                    declared_schema: Some(T::Number),
                    expr: call(write, vec![E::Var(c), E::Float(10.0), E::Float(32.0)]),
                    line: 1,
                },
            );
        }
        assert!(
            self.collect_phases || self.collect_stop.is_none(),
            "collector stop requires the default-off diagnostic mode"
        );
        if self.collect_phases {
            install_collector_diag(&mut ir, self.collect_stop, None);
        }
        let mut uncaught_at = None;
        let value = if self.uncaught {
            let root_write = helper(&ir, "__compat_es5_root_write");
            ir.stmts
                .iter()
                .enumerate()
                .rev()
                .find_map(|(i, s)| match s {
                    S::IfElse { then_branch, .. } => then_branch.iter().find_map(|s| match s {
                        S::Let {
                            expr: E::Call(target, _, args, _, _),
                            ..
                        } if *target == root_write => match args.get(3) {
                            Some(E::Var(payload)) => {
                                uncaught_at = Some(i);
                                Some(*payload)
                            }
                            _ => None,
                        },
                        _ => None,
                    }),
                    _ => None,
                })
                .expect("real uncaught pending payload")
        } else {
            ir.stmts
                .iter()
                .rev()
                .find_map(|s| match s {
                    S::Expr {
                        expr: E::Var(index),
                        ..
                    } => Some(*index),
                    _ => None,
                })
                .expect("real script completion value")
        };
        let mut observations = vec![
            S::Expr {
                expr: E::String("P7_OBSERVED".into()),
                line: 1,
            },
            S::Expr {
                expr: E::Var(value),
                line: 1,
            },
        ];
        if self.realm_header {
            observations.push(S::Expr {
                expr: E::String("P7_REALM_HEADER".into()),
                line: 1,
            });
            for offset in [0, 15, 9] {
                observations.push(S::Expr {
                    expr: call(read, vec![E::Var(carrier), E::Float(offset as f64)]),
                    line: 1,
                });
            }
        }
        if self.units.is_some() || self.type_error {
            let base = fresh(&mut ir);
            observations.push(S::Let {
                index: base,
                declared_schema: Some(T::Int),
                expr: call(
                    checked,
                    vec![
                        E::Var(carrier),
                        field(value, "ref_slot"),
                        field(value, "generation"),
                    ],
                ),
                line: 1,
            });
            let offsets: Vec<i64> = if let Some(units) = self.units {
                std::iter::once(2)
                    .chain((0..units).map(|i| 3 + i as i64))
                    .collect()
            } else {
                vec![0, 2]
            };
            for offset in offsets {
                observations.push(S::Expr {
                    expr: call(
                        read,
                        vec![
                            E::Var(carrier),
                            number(E::Add(Box::new(E::Var(base)), Box::new(E::Int(offset)))),
                        ],
                    ),
                    line: 1,
                });
            }
            if self.type_error {
                let root = helper(&ir, "__compat_es5_intrinsic_root");
                let prototype = fresh(&mut ir);
                observations.push(S::Let {
                    index: prototype,
                    declared_schema: Some(T::Named("__CompatEs5Primitive".into(), Vec::new())),
                    expr: call(root, vec![E::Var(carrier), E::Int(3)]),
                    line: 1,
                });
                for (offset, name) in [(3, "ref_slot"), (4, "generation")] {
                    observations.push(S::Expr {
                        expr: E::Eq(
                            Box::new(number(call(
                                read,
                                vec![
                                    E::Var(carrier),
                                    number(E::Add(
                                        Box::new(E::Var(base)),
                                        Box::new(E::Int(offset)),
                                    )),
                                ],
                            ))),
                            Box::new(number(field(prototype, name))),
                        ),
                        line: 1,
                    });
                }
            }
        }
        if let Some(at) = uncaught_at {
            let S::IfElse { then_branch, .. } = &mut ir.stmts[at] else {
                unreachable!()
            };
            then_branch.splice(0..0, observations);
            return Ok(ir);
        }
        ir.stmts.extend(observations);
        let observe = helper(&ir, "__compat_es5_heap_observe");
        for selector in [0, 4, 8, 12] {
            ir.stmts.push(S::Expr {
                expr: call(observe, vec![E::Var(carrier), E::Int(selector), E::Int(0)]),
                line: 1,
            });
        }
        Ok(ir)
    }
}

/// Test-only D01 boundaries. The variant names are measurements, not
/// production states; `CollectEntry` and `CollectCommit` carry the completed
/// collection ordinal observed at collector entry. The `Roots*` variants share
/// one matched-layout ROOTS diagnostic: `RootsFull` never stops, `RootsEntry`
/// stops before the first slot of the target collection, `RootsCursor8` stops
/// after eight completed slots of the target collection. The `Gray*` variants
/// share one matched-layout Gray diagnostic: `GrayFull` never stops,
/// `GrayEntry` stops at the Gray-entry boundary of the target collection and
/// `GrayExit` stops at its Gray-exit boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum D01Stop {
    Bootstrap,
    BootstrapCall,
    FrameEntry,
    CollectEntry,
    CollectCommit,
    RootsFull,
    RootsEntry,
    RootsCursor8,
    GrayFull,
    GrayEntry,
    GrayExit,
}

fn stop_label(phase: u32) -> u32 {
    DIAG_STOP_BASE + (phase - DIAG_PHASE_INIT)
}

/// Conditional deliberate stop for the descriptor frame only, keyed on a
/// mapped diagnostic parameter instead of a capacity literal. The private
/// clone is reachable only from the descriptor callsite (flag 1) and from the
/// shared wrapper (flag 0), so no other frame can match whatever capacity the
/// bounded algorithm requests.
fn parameter_stop(flag: u16, phase: u32) -> S {
    S::IfElse {
        condition: E::Not(Box::new(E::Eq(Box::new(E::Var(flag)), Box::new(E::Int(0))))),
        then_branch: vec![S::Expr {
            expr: E::Call(
                B::Assert.call_index(),
                Vec::new(),
                vec![E::Bool(false)],
                None,
                None,
            ),
            line: stop_label(phase),
        }],
        else_branch: Vec::new(),
        line: stop_label(phase),
    }
}

/// Locate the untouched `let cursor = 0;` seed immediately before the real
/// zeroing loop and its mutable cursor slot. No statement index or capacity
/// constant is assumed beyond the capacity parameter itself.
fn root_cursor_seed(body: &[S], capacity: u16) -> (usize, u16) {
    for (at, stmt) in body.iter().enumerate() {
        let S::While {
            condition: E::Lt(left, right),
            ..
        } = stmt
        else {
            continue;
        };
        let E::Var(cursor) = left.as_ref() else {
            continue;
        };
        if !matches!(right.as_ref(), E::Var(slot) if *slot == capacity) {
            continue;
        }
        let seed_at = at
            .checked_sub(1)
            .expect("the cursor seed precedes its loop");
        let S::Let {
            index,
            expr: E::Int(0),
            ..
        } = &body[seed_at]
        else {
            panic!("root_enter cursor seed is not a zero Int let");
        };
        assert_eq!(*index, *cursor, "root_enter cursor seed slot mismatch");
        return (seed_at, *cursor);
    }
    panic!("root_enter cursor loop not located");
}

/// Split `__compat_es5_root_enter` into a wrapper with its original
/// signature/index/return and a private diagnostic body. The cursor becomes a
/// mutable Int parameter, a mapped Int stop flag selects the deliberate stop,
/// and the four scalar counters are entry snapshots sampled by the caller.
/// Capacity checks, raw reservation, the three zero writes per entry, the
/// link and both root-header publishes keep their order. The wrapper always
/// passes flag 0; only the retargeted descriptor callsite can pass 1.
fn install_d01_root_probe(ir: &mut vm::FrontendIr) {
    let enter = helper(ir, "__compat_es5_root_enter");
    let observe = helper(ir, "__compat_es5_heap_observe");
    let declaration = ir
        .functions
        .iter()
        .find(|function| function.index == enter)
        .expect("root_enter declaration")
        .clone();
    let original = ir
        .function_impls
        .remove(&enter)
        .expect("root_enter implementation");
    let carrier = original.param_slots[0];
    let capacity = original.param_slots[1];
    let line = original.body_expr_line;
    let mut body = original.body_stmts.clone();
    let (seed_at, cursor) = root_cursor_seed(&body, capacity);
    body.remove(seed_at);
    assert!(
        matches!(body.get(seed_at), Some(S::While { .. })),
        "the zeroing loop must follow the removed cursor seed"
    );
    assert_eq!(
        body.iter()
            .filter(|stmt| matches!(stmt, S::While { .. }))
            .count(),
        1,
        "root_enter keeps exactly one zeroing loop"
    );
    let stop_flag = fresh(ir);
    let site = fresh(ir);
    // A real zero store gives the diagnostic region a stable start line while
    // staying nonallocating and root-free; the stop run uses its own
    // conditional line. The Noop form emits no instruction, which would leave
    // the region start mapped to the first original body line.
    let marker = fresh(ir);
    body.insert(
        0,
        S::Let {
            index: marker,
            declared_schema: Some(T::Int),
            expr: E::Int(0),
            line: DIAG_PHASE_FRAME_ENTRY,
        },
    );
    body.insert(0, parameter_stop(stop_flag, DIAG_PHASE_FRAME_ENTRY));
    let loop_at = body
        .iter()
        .position(|stmt| matches!(stmt, S::While { .. }))
        .expect("zeroing loop");
    // Keep the mutable cursor parameter live after the loop with a
    // nonallocating bound check; the original link/publish order is untouched.
    body.insert(
        loop_at + 1,
        S::Expr {
            expr: E::Call(
                B::Assert.call_index(),
                Vec::new(),
                vec![E::And(
                    Box::new(E::Not(Box::new(E::Lt(
                        Box::new(E::Var(cursor)),
                        Box::new(E::Int(0)),
                    )))),
                    Box::new(E::Not(Box::new(E::Gt(
                        Box::new(E::Var(cursor)),
                        Box::new(E::Var(capacity)),
                    )))),
                )],
                None,
                None,
            ),
            line,
        },
    );
    let collections = fresh(ir);
    let live = fresh(ir);
    let root_capacity = fresh(ir);
    let pending = fresh(ir);
    let diag = ir
        .functions
        .iter()
        .map(|function| function.index)
        .max()
        .expect("helper index")
        .checked_add(1)
        .expect("diagnostic helper index");
    let mut diag_declaration = declaration.clone();
    diag_declaration.name = "__compat_es5_diag_root_enter".to_owned();
    diag_declaration.index = diag;
    diag_declaration.arity = 9;
    diag_declaration.args = [
        "carrier",
        "capacity",
        "cursor",
        "collections",
        "live_nodes",
        "root_capacity",
        "pending_nodes",
        "frame_stop",
        "site",
    ]
    .iter()
    .map(|name| (*name).to_owned())
    .collect();
    diag_declaration.arg_schemas = vec![
        declaration.arg_schemas[0].clone(),
        Some(T::Int),
        Some(T::Int),
        Some(T::Int),
        Some(T::Int),
        Some(T::Int),
        Some(T::Int),
        Some(T::Int),
        Some(T::Int),
    ];
    diag_declaration.return_schema = Some(T::Int);
    diag_declaration.return_type = V::Int;
    diag_declaration.exported = false;
    ir.functions.push(diag_declaration);
    ir.function_impls.insert(
        diag,
        vm::compiler::FunctionImpl {
            param_slots: vec![
                carrier,
                capacity,
                cursor,
                collections,
                live,
                root_capacity,
                pending,
                stop_flag,
                site,
            ],
            capture_copies: Vec::new(),
            body_stmts: body,
            body_expr: original.body_expr.clone(),
            body_expr_line: original.body_expr_line,
        },
    );
    let mut wrapper = original;
    wrapper.param_slots = vec![carrier, capacity];
    let mut wrapper_body = Vec::with_capacity(5);
    for slot in [collections, live, root_capacity, pending] {
        wrapper_body.push(S::Let {
            index: slot,
            declared_schema: Some(T::Int),
            expr: E::Int(0),
            line,
        });
    }
    wrapper_body.push(S::IfElse {
        condition: E::Eq(Box::new(E::Var(capacity)), Box::new(E::Int(1032))),
        then_branch: vec![
            S::Assign {
                kind: vm::AssignmentKind::Set,
                index: collections,
                expr: call(observe, vec![E::Var(carrier), E::Int(5), E::Int(0)]),
                line,
            },
            S::Assign {
                kind: vm::AssignmentKind::Set,
                index: live,
                expr: call(observe, vec![E::Var(carrier), E::Int(0), E::Int(0)]),
                line,
            },
            S::Assign {
                kind: vm::AssignmentKind::Set,
                index: root_capacity,
                expr: call(observe, vec![E::Var(carrier), E::Int(4), E::Int(0)]),
                line,
            },
            S::Assign {
                kind: vm::AssignmentKind::Set,
                index: pending,
                expr: call(observe, vec![E::Var(carrier), E::Int(8), E::Int(0)]),
                line,
            },
        ],
        else_branch: Vec::new(),
        line,
    });
    wrapper.body_stmts = wrapper_body;
    wrapper.body_expr = E::Call(
        diag,
        Vec::new(),
        vec![
            E::Var(carrier),
            E::Var(capacity),
            E::Int(0),
            E::Var(collections),
            E::Var(live),
            E::Var(root_capacity),
            E::Var(pending),
            E::Int(0),
            E::Int(0),
        ],
        None,
        None,
    );
    ir.function_impls.insert(enter, wrapper);
}

/// Redirect only the named `__compat_es5_define_properties` helper's single
/// `root_enter` callsite to the private diagnostic clone built above. The
/// shared helper's own IR and every other caller keep their ordinary path.
/// Both original actual arguments (carrier, requested capacity) are preserved
/// verbatim; only the diagnostic tail is added, with the four counters sampled
/// fresh at this callsite and the clearing cursor at its unchanged zero seed.
/// Because the stop flag is a parameter, the descriptor frame is identified by
/// its caller and stops regardless of which capacity it requests.
fn retarget_define_properties_frame(ir: &mut vm::FrontendIr, frame_stop: bool) {
    let enter = helper(ir, "__compat_es5_root_enter");
    let diag = helper(ir, "__compat_es5_diag_root_enter");
    let observe = helper(ir, "__compat_es5_heap_observe");
    let index = ir
        .functions
        .iter()
        .find(|function| function.name == "__compat_es5_define_properties")
        .expect("named defineProperties helper")
        .index;
    let declared = ir
        .functions
        .iter()
        .filter(|function| function.name == "__compat_es5_define_properties")
        .count();
    // Structural evidence: exactly one named declaration and one implementation
    // exist before the single callsite is redirected.
    eprintln!(
        "DEFINE_PROPERTIES_RETARGET declared={declared} index={index} has_impl={}",
        ir.function_impls.contains_key(&index)
    );
    let mut implementation = ir
        .function_impls
        .remove(&index)
        .expect("no defineProperties implementation to remove");
    let mut body = std::mem::take(&mut implementation.body_stmts);
    let sites: Vec<usize> = body
        .iter()
        .enumerate()
        .filter_map(|(at, stmt)| match stmt {
            S::Assign {
                expr: E::Call(target, _, args, _, _),
                ..
            } if *target == enter && args.len() == 2 => Some(at),
            _ => None,
        })
        .collect();
    assert_eq!(
        sites.len(),
        1,
        "expected one original root_enter callsite in defineProperties, found {}",
        sites.len()
    );
    let at = sites[0];
    let S::Assign {
        expr: E::Call(callee, generics, args, tail, tail_line),
        kind,
        index: frame,
        line,
    } = body[at].clone()
    else {
        unreachable!()
    };
    assert_eq!(
        callee, enter,
        "the descriptor callsite callee is root_enter"
    );
    let original_capacity = args[1].clone();
    let carrier_arg = args[0].clone();
    let counter_slots: Vec<u16> = (0..4).map(|_| fresh(ir)).collect();
    let label = stop_label(DIAG_PHASE_FRAME_ENTRY);
    let mut inserted = Vec::with_capacity(8);
    for slot in &counter_slots {
        inserted.push(S::Let {
            index: *slot,
            declared_schema: Some(T::Int),
            expr: E::Int(0),
            line: label,
        });
    }
    for (slot, selector) in counter_slots.iter().zip([5, 0, 4, 8]) {
        inserted.push(S::Assign {
            kind: vm::AssignmentKind::Set,
            index: *slot,
            expr: call(
                observe,
                vec![carrier_arg.clone(), E::Int(selector), E::Int(0)],
            ),
            line: label,
        });
    }
    assert_eq!(inserted.len(), 8, "four slots plus four entry samples");
    let mut diagnostic_args = Vec::with_capacity(9);
    diagnostic_args.push(carrier_arg);
    diagnostic_args.push(original_capacity);
    diagnostic_args.push(E::Int(0));
    diagnostic_args.extend(counter_slots.iter().map(|slot| E::Var(*slot)));
    diagnostic_args.push(E::Int(if frame_stop { 1 } else { 0 }));
    diagnostic_args.push(E::Int(DIAG_DESCRIPTOR_SITE));
    let rewritten = S::Assign {
        kind,
        index: frame,
        expr: E::Call(diag, generics, diagnostic_args, tail, tail_line),
        line,
    };
    for (offset, stmt) in inserted.into_iter().enumerate() {
        body.insert(at + offset, stmt);
    }
    body[at + 8] = rewritten;
    implementation.body_stmts = body;
    assert!(
        ir.function_impls.insert(index, implementation).is_none(),
        "defineProperties implementation index was unexpectedly reoccupied"
    );
}

/// One private stop-and-sample helper for a diagnostic boundary. It takes
/// four Int counters (already sampled through the real observer) and never
/// returns, so the caller's frame carries the entry samples at the stop.
fn install_d01_stop_sample(ir: &mut vm::FrontendIr, phase: u32) -> u16 {
    let observe = helper(ir, "__compat_es5_heap_observe");
    let template = ir
        .functions
        .iter()
        .find(|function| function.index == observe)
        .expect("observer declaration")
        .clone();
    let mut declaration = template;
    declaration.name = "__compat_es5_d01_stop_sample".to_owned();
    declaration.index = ir
        .functions
        .iter()
        .map(|function| function.index)
        .max()
        .expect("helper index")
        .checked_add(1)
        .expect("diagnostic helper index");
    declaration.arity = 4;
    declaration.args = [
        "collections",
        "live_nodes",
        "root_capacity",
        "pending_nodes",
    ]
    .iter()
    .map(|name| (*name).to_owned())
    .collect();
    declaration.arg_schemas = vec![Some(T::Int); 4];
    declaration.return_schema = Some(T::Int);
    declaration.return_type = V::Int;
    declaration.exported = false;
    let index = declaration.index;
    let slots: Vec<u16> = (0..4).map(|_| fresh(ir)).collect();
    ir.functions.push(declaration);
    ir.function_impls.insert(
        index,
        vm::compiler::FunctionImpl {
            param_slots: slots,
            capture_copies: Vec::new(),
            body_stmts: vec![S::Expr {
                expr: E::Call(
                    B::Assert.call_index(),
                    Vec::new(),
                    vec![E::Bool(false)],
                    None,
                    None,
                ),
                line: stop_label(phase),
            }],
            body_expr: E::Int(0),
            body_expr_line: stop_label(phase),
        },
    );
    index
}

/// Insert the stop-and-sample call immediately after the single typed
/// bootstrap statement. Samples are taken through the real nonallocating
/// observer before the deliberate stop.
fn insert_bootstrap_stop(ir: &mut vm::FrontendIr, sample: u16, carrier: u16) {
    let bootstrap = helper(ir, "__compat_es5_intrinsic_bootstrap");
    let observe = helper(ir, "__compat_es5_heap_observe");
    let sites: Vec<usize> = ir
        .stmts
        .iter()
        .enumerate()
        .filter_map(|(at, stmt)| match stmt {
            S::Let {
                expr: E::Call(target, _, args, _, _),
                ..
            } if *target == bootstrap && args.len() == 1 => Some(at),
            _ => None,
        })
        .collect();
    assert_eq!(
        sites.len(),
        1,
        "expected one typed bootstrap completion site, found {}",
        sites.len()
    );
    let at = sites[0];
    let slots: Vec<u16> = (0..4).map(|_| fresh(ir)).collect();
    let line = stop_label(DIAG_PHASE_BOOTSTRAP);
    let mut inserted = Vec::with_capacity(9);
    for slot in &slots {
        inserted.push(S::Let {
            index: *slot,
            declared_schema: Some(T::Int),
            expr: E::Int(0),
            line,
        });
    }
    for (slot, selector) in slots.iter().zip([5, 0, 4, 8]) {
        inserted.push(S::Assign {
            kind: vm::AssignmentKind::Set,
            index: *slot,
            expr: call(observe, vec![E::Var(carrier), E::Int(selector), E::Int(0)]),
            line,
        });
    }
    inserted.push(S::Expr {
        expr: E::Call(
            sample,
            Vec::new(),
            slots.iter().map(|slot| E::Var(*slot)).collect(),
            None,
            None,
        ),
        line,
    });
    for (offset, stmt) in inserted.into_iter().enumerate() {
        ir.stmts.insert(at + 1 + offset, stmt);
    }
}

/// Insert the same four-counter sample and deliberate stop immediately
/// *before* the single typed bootstrap statement. The locator is the unique
/// typed call site used by `insert_bootstrap_stop`; only the position differs,
/// so no bootstrap work (and no prior collection attributable to it) has
/// happened when the counters are read. The owning carrier must already be
/// defined and initialized at this point; that ordering is asserted rather
/// than assumed.
fn insert_bootstrap_call_stop(ir: &mut vm::FrontendIr, sample: u16, carrier: u16) {
    let bootstrap = helper(ir, "__compat_es5_intrinsic_bootstrap");
    let make = helper(ir, "__compat_ref_make");
    let observe = helper(ir, "__compat_es5_heap_observe");
    let sites: Vec<usize> = ir
        .stmts
        .iter()
        .enumerate()
        .filter_map(|(at, stmt)| match stmt {
            S::Let {
                expr: E::Call(target, _, args, _, _),
                ..
            } if *target == bootstrap && args.len() == 1 => Some(at),
            _ => None,
        })
        .collect();
    assert_eq!(
        sites.len(),
        1,
        "expected one typed bootstrap call site, found {}",
        sites.len()
    );
    let at = sites[0];
    let carrier_at = ir.stmts.iter().position(|stmt| {
        matches!(
            stmt,
            S::Let {
                index,
                expr: E::Call(target, _, _, _, _),
                ..
            } if *index == carrier && *target == make
        )
    });
    let carrier_at = carrier_at.expect("the sampled interval requires the real carrier");
    assert!(
        carrier_at < at,
        "the owning carrier must be defined before the pre-bootstrap sample"
    );
    let slots: Vec<u16> = (0..4).map(|_| fresh(ir)).collect();
    let line = stop_label(DIAG_PHASE_BOOTSTRAP_CALL);
    let mut inserted = Vec::with_capacity(9);
    for slot in &slots {
        inserted.push(S::Let {
            index: *slot,
            declared_schema: Some(T::Int),
            expr: E::Int(0),
            line,
        });
    }
    for (slot, selector) in slots.iter().zip([5, 0, 4, 8]) {
        inserted.push(S::Assign {
            kind: vm::AssignmentKind::Set,
            index: *slot,
            expr: call(observe, vec![E::Var(carrier), E::Int(selector), E::Int(0)]),
            line,
        });
    }
    inserted.push(S::Expr {
        expr: E::Call(
            sample,
            Vec::new(),
            slots.iter().map(|slot| E::Var(*slot)).collect(),
            None,
            None,
        ),
        line,
    });
    ir.stmts.splice(at..at, inserted);
}

/// Default-off D01 measurement frontend. It reuses the accepted object
/// observer verbatim and post-processes only the generated IR, so no existing
/// `Observer` literal gains a field and every ordinary test keeps its bytes.
pub struct D01Observer {
    pub force_gc: bool,
    pub stop: Option<(D01Stop, i64)>,
}
impl vm::SourcePlugin for D01Observer {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut d = vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
        d.id = vm::FrontendId::new("javascript-es5-object-observer");
        d
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let base = Observer {
            force_gc: self.force_gc,
            ..Default::default()
        };
        let mut ir = vm::SourcePlugin::parse_source(&base, source, context)?;
        let make = helper(&ir, "__compat_ref_make");
        let carrier = ir
            .stmts
            .iter()
            .find_map(|s| match s {
                S::Let {
                    index,
                    expr: E::Call(target, _, _, _, _),
                    ..
                } if *target == make => Some(*index),
                _ => None,
            })
            .expect("real carrier");
        let (collect_stop, collect_phase) = match self.stop {
            Some((D01Stop::CollectEntry, count)) => {
                (Some((count, DIAG_PHASE_INIT)), Some(DIAG_PHASE_INIT))
            }
            Some((D01Stop::CollectCommit, count)) => {
                (Some((count, DIAG_PHASE_COMMIT)), Some(DIAG_PHASE_COMMIT))
            }
            _ => (None, None),
        };
        // Gray family: one matched-layout diagnostic, selected only by the
        // wrapper's two Int seed constants. It is routed before the other
        // collector instruments so the IR differs only in those seeds.
        let gray_mode: Option<(i64, i64)> = match self.stop {
            Some((D01Stop::GrayFull, _)) => Some((0, 0)),
            Some((D01Stop::GrayEntry, target)) => Some((1, target)),
            Some((D01Stop::GrayExit, target)) => Some((2, target)),
            _ => None,
        };
        if let Some((mode, target)) = gray_mode {
            install_gray_collector_diag(&mut ir, mode, target);
            return Ok(ir);
        }
        let roots_mode: Option<(i64, i64)> = match self.stop {
            Some((D01Stop::RootsFull, _)) => Some((0, 0)),
            Some((D01Stop::RootsEntry, target)) => Some((1, target)),
            Some((D01Stop::RootsCursor8, target)) => Some((2, target)),
            _ => None,
        };
        if let Some((mode, target)) = roots_mode {
            install_roots_collector_diag(&mut ir, mode, target);
            return Ok(ir);
        }
        let collect_sample = collect_phase.map(|phase| install_d01_stop_sample(&mut ir, phase));
        install_collector_diag(&mut ir, collect_stop, collect_sample);
        install_d01_root_probe(&mut ir);
        retarget_define_properties_frame(
            &mut ir,
            matches!(self.stop, Some((D01Stop::FrameEntry, _))),
        );
        if matches!(self.stop, Some((D01Stop::Bootstrap, _))) {
            let sample = install_d01_stop_sample(&mut ir, DIAG_PHASE_BOOTSTRAP);
            insert_bootstrap_stop(&mut ir, sample, carrier);
        }
        if matches!(self.stop, Some((D01Stop::BootstrapCall, _))) {
            let sample = install_d01_stop_sample(&mut ir, DIAG_PHASE_BOOTSTRAP_CALL);
            insert_bootstrap_call_stop(&mut ir, sample, carrier);
        }
        Ok(ir)
    }
}

/// Test-only precise collection control for a descriptor callback.
///
/// The only anchor is the production `delete` helper call, which must occur in
/// exactly one guest script function: the callback that removes the earlier
/// props rows. Immediately after that call the emitted callback body gains
///
/// ```text
/// before = heap_observe(carrier, collections)
/// heap_collect(carrier)                      // one real full collection
/// after  = heap_observe(carrier, collections)
/// assert(after == before + 1)                // the collection ran here
/// ```
///
/// Nothing else changes: no production helper, counter, heap field or host
/// capability is added, no other frame gains a statement, and the block exists
/// only for a caller that asks for this default-off control. Because the
/// assertion lives inside the callback, reaching `Halted` proves the
/// collection executed at that exact point, not merely that IR was inserted.
pub struct CallbackCollectSites {
    pub function: u16,
    pub name: String,
    pub before: u16,
    pub after: u16,
}

fn delete_call_statement(stmt: &S, delete: u16) -> bool {
    match stmt {
        S::Let {
            expr: E::Call(target, _, _, _, _),
            ..
        }
        | S::Assign {
            expr: E::Call(target, _, _, _, _),
            ..
        }
        | S::Expr {
            expr: E::Call(target, _, _, _, _),
            ..
        } => *target == delete,
        _ => false,
    }
}

fn stmt_line(stmt: &S) -> u32 {
    match stmt {
        S::Let { line, .. }
        | S::Assign { line, .. }
        | S::Expr { line, .. }
        | S::IfElse { line, .. }
        | S::While { line, .. }
        | S::For { line, .. }
        | S::Noop { line } => *line,
        _ => 1,
    }
}

/// Insert `block` immediately after the delete call in this statement list,
/// returning the source line of the anchor statement when it was found there.
fn insert_in_statements(stmts: &mut Vec<S>, delete: u16, block: &[S]) -> Option<u32> {
    if let Some(at) = stmts
        .iter()
        .position(|stmt| delete_call_statement(stmt, delete))
    {
        let line = stmt_line(&stmts[at]);
        stmts.splice(at + 1..at + 1, block.iter().cloned());
        return Some(line);
    }
    stmts
        .iter_mut()
        .find_map(|stmt| insert_in_statement(stmt, delete, block))
}

fn insert_in_statement(stmt: &mut S, delete: u16, block: &[S]) -> Option<u32> {
    match stmt {
        S::Let { expr, .. } | S::Assign { expr, .. } | S::Expr { expr, .. } => {
            insert_in_expr(expr, delete, block)
        }
        S::ClosureLet { closure, .. } => insert_in_expr(closure.body.as_mut(), delete, block),
        S::IfElse {
            condition,
            then_branch,
            else_branch,
            ..
        } => insert_in_expr(condition, delete, block)
            .or_else(|| insert_in_statements(then_branch, delete, block))
            .or_else(|| insert_in_statements(else_branch, delete, block)),
        S::While {
            condition, body, ..
        } => insert_in_expr(condition, delete, block)
            .or_else(|| insert_in_statements(body, delete, block)),
        S::For {
            init,
            condition,
            post,
            body,
            ..
        } => insert_in_statement(init, delete, block)
            .or_else(|| insert_in_expr(condition, delete, block))
            .or_else(|| insert_in_statement(post, delete, block))
            .or_else(|| insert_in_statements(body, delete, block)),
        _ => None,
    }
}

/// The emitted user-function body is a completion expression tree, so the
/// delete call can sit inside nested blocks and matches. Walk the whole tree
/// exhaustively (the match is over every expression variant) and insert the
/// observation block directly after the delete itself.
fn insert_in_expr(expr: &mut E, delete: u16, block: &[S]) -> Option<u32> {
    match expr {
        E::Block { stmts, expr } => insert_in_statements(stmts, delete, block)
            .or_else(|| insert_in_expr(expr, delete, block)),
        E::IfElse {
            condition,
            then_expr,
            else_expr,
        } => insert_in_expr(condition, delete, block)
            .or_else(|| insert_in_expr(then_expr, delete, block))
            .or_else(|| insert_in_expr(else_expr, delete, block)),
        E::Call(_, _, args, _, _)
        | E::ModuleCall(_, _, args, _)
        | E::LocalCall(_, _, args, _)
        | E::ClosureCall(_, args) => args
            .iter_mut()
            .find_map(|argument| insert_in_expr(argument, delete, block)),
        E::Match {
            value,
            arms,
            default,
            ..
        } => insert_in_expr(value, delete, block)
            .or_else(|| {
                arms.iter_mut()
                    .find_map(|(_, arm)| insert_in_expr(arm, delete, block))
            })
            .or_else(|| insert_in_expr(default, delete, block)),
        E::OptionalGet { container, key, .. } => {
            insert_in_expr(container, delete, block).or_else(|| insert_in_expr(key, delete, block))
        }
        E::OptionUnwrapOr {
            value, fallback, ..
        } => {
            insert_in_expr(value, delete, block).or_else(|| insert_in_expr(fallback, delete, block))
        }
        E::Add(left, right)
        | E::Sub(left, right)
        | E::Mul(left, right)
        | E::Div(left, right)
        | E::Mod(left, right)
        | E::And(left, right)
        | E::Or(left, right)
        | E::Eq(left, right)
        | E::Lt(left, right)
        | E::Gt(left, right) => {
            insert_in_expr(left, delete, block).or_else(|| insert_in_expr(right, delete, block))
        }
        E::Neg(inner)
        | E::Not(inner)
        | E::ToOwned(inner)
        | E::Borrow(inner)
        | E::BorrowMut(inner) => insert_in_expr(inner, delete, block),
        E::Closure(closure) => insert_in_expr(closure.body.as_mut(), delete, block),
        _ => None,
    }
}

pub fn install_callback_collect(ir: &mut vm::FrontendIr) -> CallbackCollectSites {
    let delete = helper(ir, "__compat_es5_delete_property");
    let observe = helper(ir, "__compat_es5_heap_observe");
    let collect = helper(ir, "__compat_es5_heap_collect");
    let names: std::collections::BTreeMap<u16, String> = ir
        .functions
        .iter()
        .map(|function| (function.index, function.name.clone()))
        .collect();
    let mut found = Vec::new();
    let marker = format!("Call({delete},");
    for (index, implementation) in ir.function_impls.iter() {
        if format!("{:?}", implementation.body_stmts).contains(&marker)
            || format!("{:?}", implementation.body_expr).contains(&marker)
        {
            found.push((
                *index,
                names
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| "unnamed".into()),
                implementation.param_slots.first().copied(),
            ));
        }
    }
    assert_eq!(
        found.len(),
        1,
        "exactly one emitted function may delete a props row, found {found:?}"
    );
    let (index, name, carrier) = found.pop().expect("single delete callback");
    let carrier = carrier.expect("the delete callback carries its carrier parameter");
    let before = fresh(ir);
    let after = fresh(ir);
    let implementation = ir
        .function_impls
        .get_mut(&index)
        .expect("callback implementation");
    let block = vec![
        S::Let {
            index: before,
            declared_schema: Some(T::Int),
            expr: call(observe, vec![E::Var(carrier), E::Int(5), E::Int(0)]),
            line: 1,
        },
        S::Expr {
            expr: call(collect, vec![E::Var(carrier)]),
            line: 1,
        },
        S::Let {
            index: after,
            declared_schema: Some(T::Int),
            expr: call(observe, vec![E::Var(carrier), E::Int(5), E::Int(0)]),
            line: 1,
        },
        S::Expr {
            expr: call(
                B::Assert.call_index(),
                vec![E::Eq(
                    Box::new(E::Var(after)),
                    Box::new(E::Add(Box::new(E::Var(before)), Box::new(E::Int(1)))),
                )],
            ),
            line: 1,
        },
    ];
    let line = insert_in_statements(&mut implementation.body_stmts, delete, &block)
        .or_else(|| insert_in_expr(&mut implementation.body_expr, delete, &block))
        .expect("the delete statement must be reachable in the callback body");
    eprintln!("CALLBACK_COLLECT anchor_line={line}");
    CallbackCollectSites {
        function: index,
        name,
        before,
        after,
    }
}

/// Printable-ASCII text encoded by the numeric constants of the production
/// `ToPropertyDescriptor` body, in emitted order.
///
/// `literal_key` inlines each descriptor field name as one numeric constant per
/// UTF-16 unit, and the surrounding IR interleaves other constants (unit
/// indices, row strides, `Div(_, 1.0)` normalisers). Filtering to the printable
/// ASCII range keeps exactly the unit constants, so the returned text reads as
/// the concatenation of the compared field names in comparison order. This is a
/// read-only structural derivation of the current IR: no slot number,
/// instruction size or prototype id is assumed, and nothing is modified. Slots
/// are deliberately *not* mapped to physical frame offsets here; the helper's
/// cursor/row/literal-key temporaries are locals with no public offset
/// metadata.
pub fn to_desc_constant_text(ir: &vm::FrontendIr) -> (u16, usize, String) {
    let index = helper(ir, "__compat_es5_to_property_descriptor");
    let implementation = ir
        .function_impls
        .get(&index)
        .expect("to_property_descriptor implementation");
    let rendered = format!("{:?}", implementation.body_stmts);
    let mut text = String::new();
    let mut from = 0usize;
    while from < rendered.len() {
        let next = ["Float(", "Int("]
            .iter()
            .filter_map(|marker| rendered[from..].find(marker).map(|at| (from + at, *marker)))
            .min();
        let Some((at, marker)) = next else {
            break;
        };
        let tail = &rendered[at + marker.len()..];
        let integer: String = tail
            .chars()
            .take_while(|character| character.is_ascii_digit())
            .collect();
        if let Ok(value) = integer.parse::<u16>()
            && (0x20..=0x7e).contains(&value)
        {
            text.push(char::from_u32(u32::from(value)).unwrap_or('?'));
        }
        from = at + marker.len() + integer.len();
    }
    (index, implementation.body_stmts.len(), text)
}
