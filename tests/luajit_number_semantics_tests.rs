//! Classified Lua number arithmetic, compiled and run through Core.
//!
//! Case 06 values `3.5,1,-1,-2.5` are the frozen protocol bytes. The other
//! expected bits were observed on locked LuaJIT 96f7b297326e9fd454b7186c6858b2773f94a0dc
//! with `-joff`. NaN is compared by category only.

#[path = "../src/frontends/lua/number.rs"]
mod number;

use number::{ClassifiedNumber, NumberLowering};
use vm::{AssignmentKind, Compiler, Expr, LocalIrBuilder, LocalSlot, Stmt, Value, Vm};

const PROTOCOL_06: [&str; 4] = ["3.5", "1", "-1", "-2.5"];
const PROTOCOL_06_BITS: [u64; 4] = [
    0x400c_0000_0000_0000, // 3.5
    0x3ff0_0000_0000_0000, // 1
    0xbff0_0000_0000_0000, // -1
    0xc004_0000_0000_0000, // -2.5
];

#[test]
fn case06_business_results_match_frozen_protocol() {
    let mut builder = LocalIrBuilder::new();
    let mut lowering = NumberLowering::new(&mut builder);
    let quotient = ClassifiedNumber::literal(7.0).div(ClassifiedNumber::literal(2.0));
    let positive_remainder = lowering
        .floor_mod(
            ClassifiedNumber::literal(-5.0),
            ClassifiedNumber::literal(3.0),
        )
        .expect("bind remainder temps");
    let negative_remainder = lowering
        .floor_mod(
            ClassifiedNumber::literal(5.0),
            ClassifiedNumber::literal(-3.0),
        )
        .expect("bind remainder temps");
    let negative_quotient = ClassifiedNumber::literal(-5.0).div(ClassifiedNumber::literal(2.0));
    drop(lowering);

    let ran = run(
        &builder,
        vec![
            expr_stmt(quotient),
            expr_stmt(positive_remainder),
            expr_stmt(negative_remainder),
            expr_stmt(negative_quotient),
        ],
    );

    assert_eq!(ran.stack.len(), 4, "{:?}", ran.stack);
    for (index, value) in ran.stack.iter().enumerate() {
        assert_protocol_number(value, PROTOCOL_06[index], PROTOCOL_06_BITS[index]);
    }
}

#[test]
fn float_division_matches_observed_luajit_specials() {
    // Divisor zero, signed zero, and infinities. Not part of frozen case 06.
    let cases = [
        (1.0, 0.0, Expect::Bits(0x7ff0_0000_0000_0000)),
        (-1.0, 0.0, Expect::Bits(0xfff0_0000_0000_0000)),
        (0.0, 0.0, Expect::Nan),
        (1.0, -0.0, Expect::Bits(0xfff0_0000_0000_0000)),
        (-0.0, 1.0, Expect::Bits(0x8000_0000_0000_0000)),
        (0.0, -1.0, Expect::Bits(0x8000_0000_0000_0000)),
        (f64::INFINITY, 0.0, Expect::Bits(0x7ff0_0000_0000_0000)),
        (1.0, f64::NAN, Expect::Nan),
    ];
    for (lhs, rhs, expected) in cases {
        let value = eval_binary(|left, right| left.div(right), lhs, rhs);
        assert_expect(&value, expected, lhs, rhs);
    }
}

#[test]
fn floor_remainder_matches_observed_luajit_specials() {
    // Negative divisor, divisor zero, inf divisor, and signed-zero results.
    let cases = [
        (-5.0, -3.0, Expect::Bits(0xc000_0000_0000_0000)),
        (1.0, 0.0, Expect::Nan),
        (1.0, f64::INFINITY, Expect::Nan),
        (-2.5, 2.5, Expect::Bits(0x0000_0000_0000_0000)),
        (-0.0, -1.0, Expect::Bits(0x0000_0000_0000_0000)),
        (-0.0, 1.0, Expect::Bits(0x0000_0000_0000_0000)),
        (f64::NAN, 1.0, Expect::Nan),
    ];
    for (lhs, rhs, expected) in cases {
        let value = eval_floor_mod(lhs, rhs);
        assert_expect(&value, expected, lhs, rhs);
    }
}

#[test]
fn power_matches_observed_luajit_specials() {
    let cases = [
        (2.0, 3.0, Expect::Bits(0x4020_0000_0000_0000)),
        (0.0, 0.0, Expect::Bits(0x3ff0_0000_0000_0000)),
        (-0.0, -1.0, Expect::Bits(0xfff0_0000_0000_0000)),
        (1.0, f64::NAN, Expect::Bits(0x3ff0_0000_0000_0000)),
        (-2.5, 2.5, Expect::Nan),
        (f64::INFINITY, 0.0, Expect::Bits(0x3ff0_0000_0000_0000)),
    ];
    for (lhs, rhs, expected) in cases {
        let value = eval_binary(|left, right| left.pow(right), lhs, rhs);
        assert_expect(&value, expected, lhs, rhs);
    }
}

#[test]
fn add_sub_mul_and_negation_match_observed_luajit() {
    let binary = [
        (-0.0, 0.0, Expect::Bits(0x0000_0000_0000_0000), Bin::Add),
        (-0.0, -0.0, Expect::Bits(0x8000_0000_0000_0000), Bin::Add),
        (f64::INFINITY, f64::NEG_INFINITY, Expect::Nan, Bin::Add),
        (-0.0, 0.0, Expect::Bits(0x8000_0000_0000_0000), Bin::Sub),
        (0.0, -0.0, Expect::Bits(0x0000_0000_0000_0000), Bin::Sub),
        (-0.0, 1.0, Expect::Bits(0x8000_0000_0000_0000), Bin::Mul),
        (-0.0, -1.0, Expect::Bits(0x0000_0000_0000_0000), Bin::Mul),
        (f64::INFINITY, 0.0, Expect::Nan, Bin::Mul),
        (1.5, 2.0, Expect::Bits(0x400c_0000_0000_0000), Bin::Add),
        (3.5, 1.0, Expect::Bits(0x4004_0000_0000_0000), Bin::Sub),
        (7.0, 2.0, Expect::Bits(0x402c_0000_0000_0000), Bin::Mul),
    ];
    for (lhs, rhs, expected, op) in binary {
        let value = eval_binary(
            match op {
                Bin::Add => |left, right| left.add(right),
                Bin::Sub => |left, right| left.sub(right),
                Bin::Mul => |left, right| left.mul(right),
            },
            lhs,
            rhs,
        );
        assert_expect(&value, expected, lhs, rhs);
    }

    let negated_zero = eval_unary(|operand| operand.neg(), 0.0);
    assert_expect(&negated_zero, Expect::Bits(0x8000_0000_0000_0000), 0.0, 0.0);
    let negated_negative_zero = eval_unary(|operand| operand.neg(), -0.0);
    assert_expect(
        &negated_negative_zero,
        Expect::Bits(0x0000_0000_0000_0000),
        -0.0,
        0.0,
    );
    let negated_nan = eval_unary(|operand| operand.neg(), f64::NAN);
    assert_expect(&negated_nan, Expect::Nan, f64::NAN, 0.0);
}

#[test]
fn operands_are_evaluated_once_left_then_right() {
    // -5 % 3 is 1 under Lua and -2 under Core Mod, so this also rejects the old path.
    assert_binary_eval_once(
        "floor_mod",
        -5.0,
        3.0,
        "1",
        PROTOCOL_06_BITS[1],
        |lowering, lhs, rhs| lowering.floor_mod(lhs, rhs).expect("bind remainder temps"),
    );
    assert_binary_eval_once(
        "add",
        7.0,
        2.0,
        "9",
        0x4022_0000_0000_0000,
        |_, lhs, rhs| lhs.add(rhs),
    );
    assert_binary_eval_once(
        "div",
        7.0,
        2.0,
        "3.5",
        PROTOCOL_06_BITS[0],
        |_, lhs, rhs| lhs.div(rhs),
    );
    assert_binary_eval_once(
        "pow",
        7.0,
        2.0,
        "49",
        0x4048_8000_0000_0000,
        |_, lhs, rhs| lhs.pow(rhs),
    );
    assert_unary_eval_once();
}

#[derive(Clone, Copy)]
enum Expect {
    Bits(u64),
    Nan,
}

#[derive(Clone, Copy)]
enum Bin {
    Add,
    Sub,
    Mul,
}

struct Ran {
    stack: Vec<Value>,
    locals: Vec<Value>,
}

fn eval_binary(op: fn(ClassifiedNumber, ClassifiedNumber) -> Expr, lhs: f64, rhs: f64) -> Value {
    let expr = op(
        ClassifiedNumber::literal(lhs),
        ClassifiedNumber::literal(rhs),
    );
    let builder = LocalIrBuilder::new();
    run(&builder, vec![expr_stmt(expr)])
        .stack
        .into_iter()
        .next()
        .expect("one result")
}

fn eval_floor_mod(lhs: f64, rhs: f64) -> Value {
    let mut builder = LocalIrBuilder::new();
    let expr = {
        let mut lowering = NumberLowering::new(&mut builder);
        lowering
            .floor_mod(
                ClassifiedNumber::literal(lhs),
                ClassifiedNumber::literal(rhs),
            )
            .expect("bind remainder temps")
    };
    run(&builder, vec![expr_stmt(expr)])
        .stack
        .into_iter()
        .next()
        .expect("one result")
}

fn eval_unary(op: fn(ClassifiedNumber) -> Expr, operand: f64) -> Value {
    let expr = op(ClassifiedNumber::literal(operand));
    let builder = LocalIrBuilder::new();
    run(&builder, vec![expr_stmt(expr)])
        .stack
        .into_iter()
        .next()
        .expect("one result")
}

fn assert_binary_eval_once(
    label: &str,
    lhs: f64,
    rhs: f64,
    text: &str,
    bits: u64,
    op: fn(&mut NumberLowering<'_>, ClassifiedNumber, ClassifiedNumber) -> Expr,
) {
    let mut builder = LocalIrBuilder::new();
    let sequence = builder.alloc_local_named("sequence").expect("sequence");
    let left_mark = builder.alloc_local_named("left_mark").expect("left mark");
    let right_mark = builder.alloc_local_named("right_mark").expect("right mark");
    let expr = {
        let mut lowering = NumberLowering::new(&mut builder);
        op(
            &mut lowering,
            ClassifiedNumber::from_f64_expr(counted_literal(sequence, left_mark, lhs)),
            ClassifiedNumber::from_f64_expr(counted_literal(sequence, right_mark, rhs)),
        )
    };
    let ran = run(
        &builder,
        vec![
            let_int(sequence, 0),
            let_int(left_mark, 0),
            let_int(right_mark, 0),
            expr_stmt(expr),
        ],
    );
    assert_eq!(
        local_int(&ran, sequence),
        2,
        "{label} evaluated an operand more than once: {:?}",
        ran.locals
    );
    assert_eq!(
        local_int(&ran, left_mark),
        1,
        "{label} did not evaluate the left operand first: {:?}",
        ran.locals
    );
    assert_eq!(
        local_int(&ran, right_mark),
        2,
        "{label} did not evaluate the right operand second: {:?}",
        ran.locals
    );
    assert_protocol_number(&ran.stack[0], text, bits);
}

fn assert_unary_eval_once() {
    let mut builder = LocalIrBuilder::new();
    let sequence = builder.alloc_local_named("sequence").expect("sequence");
    let mark = builder.alloc_local_named("mark").expect("mark");
    let expr = ClassifiedNumber::from_f64_expr(counted_literal(sequence, mark, 0.0)).neg();
    let ran = run(
        &builder,
        vec![let_int(sequence, 0), let_int(mark, 0), expr_stmt(expr)],
    );
    assert_eq!(local_int(&ran, sequence), 1, "{:?}", ran.locals);
    assert_eq!(local_int(&ran, mark), 1, "{:?}", ran.locals);
    assert_expect(&ran.stack[0], Expect::Bits(0x8000_0000_0000_0000), 0.0, 0.0);
}

fn counted_literal(sequence: LocalSlot, mark: LocalSlot, value: f64) -> Expr {
    Expr::Block {
        stmts: vec![
            Stmt::Assign {
                kind: AssignmentKind::Set,
                index: sequence,
                expr: Expr::Add(Box::new(Expr::Var(sequence)), Box::new(Expr::Int(1))),
                line: 1,
            },
            Stmt::Assign {
                kind: AssignmentKind::Set,
                index: mark,
                expr: Expr::Var(sequence),
                line: 1,
            },
        ],
        expr: Box::new(Expr::Float(value)),
    }
}

fn run(_builder: &LocalIrBuilder, stmts: Vec<Stmt>) -> Ran {
    // LocalIrBuilder does not expose its next local. Every allocated slot is
    // referenced by the statements, so the frame size is one past the highest index.
    let local_count = local_count_in(&stmts);
    run_stmts(local_count, stmts)
}

fn run_stmts(local_count: usize, stmts: Vec<Stmt>) -> Ran {
    let mut compiler = Compiler::new();
    compiler.set_root_local_count(local_count);
    let program = compiler
        .compile_program(&stmts)
        .unwrap_or_else(|error| panic!("compile failed: {error}"));
    let mut vm = Vm::new(program);
    vm.run()
        .unwrap_or_else(|error| panic!("run failed: {error}"));
    Ran {
        stack: vm.stack().to_vec(),
        locals: vm.locals().to_vec(),
    }
}

fn local_count_in(stmts: &[Stmt]) -> usize {
    let mut highest = 0usize;
    for stmt in stmts {
        note_stmt(stmt, &mut highest);
    }
    highest
}

fn note_stmt(stmt: &Stmt, highest: &mut usize) {
    match stmt {
        Stmt::Let { index, expr, .. } | Stmt::Assign { index, expr, .. } => {
            bump(highest, *index);
            note_expr(expr, highest);
        }
        Stmt::Expr { expr, .. } => note_expr(expr, highest),
        other => panic!("unexpected statement in number probe: {other:?}"),
    }
}

fn note_expr(expr: &Expr, highest: &mut usize) {
    match expr {
        Expr::Var(slot) => bump(highest, *slot),
        Expr::Neg(inner) => note_expr(inner, highest),
        Expr::Add(lhs, rhs)
        | Expr::Sub(lhs, rhs)
        | Expr::Mul(lhs, rhs)
        | Expr::Div(lhs, rhs)
        | Expr::Mod(lhs, rhs) => {
            note_expr(lhs, highest);
            note_expr(rhs, highest);
        }
        Expr::Call(_, _, arguments, _, _) => {
            for argument in arguments {
                note_expr(argument, highest);
            }
        }
        Expr::Block { stmts, expr } => {
            for stmt in stmts {
                note_stmt(stmt, highest);
            }
            note_expr(expr, highest);
        }
        Expr::Float(_) | Expr::Int(_) => {}
        other => panic!("unexpected expression in number probe: {other:?}"),
    }
}

fn bump(highest: &mut usize, slot: LocalSlot) {
    let next = usize::from(slot) + 1;
    if next > *highest {
        *highest = next;
    }
}

fn expr_stmt(expr: Expr) -> Stmt {
    Stmt::Expr { expr, line: 1 }
}

fn let_int(slot: LocalSlot, value: i64) -> Stmt {
    Stmt::Let {
        index: slot,
        declared_schema: None,
        expr: Expr::Int(value),
        line: 1,
    }
}

fn local_int(ran: &Ran, slot: LocalSlot) -> i64 {
    match ran.locals.get(usize::from(slot)) {
        Some(Value::Int(value)) => *value,
        other => panic!("local {slot:?} was not an int: {other:?}"),
    }
}

fn assert_protocol_number(value: &Value, text: &str, bits: u64) {
    let Value::Float(number) = value else {
        panic!("protocol number {text} was not binary64: {value:?}");
    };
    assert_eq!(number.to_bits(), bits, "bits for {text}");
    assert_eq!(format!("{number}"), text);
}

fn assert_expect(value: &Value, expected: Expect, lhs: f64, rhs: f64) {
    match (value, expected) {
        (Value::Float(number), Expect::Nan) => {
            assert!(
                number.is_nan(),
                "expected NaN for {lhs} ? {rhs}, got {number}"
            );
        }
        (Value::Float(number), Expect::Bits(bits)) => {
            assert_eq!(
                number.to_bits(),
                bits,
                "bits for {lhs} ? {rhs}, value {number}"
            );
        }
        (other, _) => panic!("expected binary64 for {lhs} ? {rhs}, got {other:?}"),
    }
}
