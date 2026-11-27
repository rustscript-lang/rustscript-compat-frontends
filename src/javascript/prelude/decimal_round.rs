//! Correctly rounded binary64 magnitude of a decimal or exact integer token.
#![allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    clippy::type_complexity
)]
//!
//! Decimal: `coeff * 10^scale`, or the digits in `[sig_start, sig_end)` when
//! the coefficient does not fit in i64. `10^scale = 2^scale * 5^|scale|`.
//! `floor(log2)` of the ratio is a bit-length compare. The 53-bit significand
//! is the integer division of the aligned ratio; the remainder against half
//! the denominator is ties-to-even. Subnormals clamp `q` to -1022 before that
//! alignment. `q >= 1024` is infinity, `q < -1075` is zero, and a round-up to
//! `2^53` renormalizes.
//!
//! Any binary64 midpoint has at most 768 significant decimal digits. The scan
//! keeps that prefix and a sticky bit for a nonzero discarded tail, then adds
//! the discarded count to the scale. Leading zeros are not significant and do
//! not change the scale. Power-of-two radices do not inherit that 768 cap or
//! the decimal exponent gate. Overflow is the bit length of the first nonzero
//! digit plus log2(radix) times the remaining significant digits. A bit length
//! of 1025 or more is +Infinity. Every shorter nonzero token enters the limb
//! rounder, including a 1024-bit value that rounds up to infinity. Zero stays
//! positive zero so the caller can apply its sign.
//! After zero and obvious overflow/underflow, the retained decimal scale is in
//! [-1091, 308]. Numerator bits are at most 3267, denominator bits 2534, plus
//! 54 alignment bits: 3321. 128 limbs of 30 bits is 3840 and is the finite
//! maximum.
//!
//! The three `Array<Int>` buffers stay in this function. Every Get, Borrow,
//! Set, and Push writes the same slot back. No scratch array is passed to or
//! returned from a guest helper. One top-level While dispatches a snapshotted
//! phase; each limb primitive has one shared step and a fixed resume local.
//! Invalid phases abort. Fuel stays the caller's 10_000_000.
//!
//! Each buffer has a conservative active-limb count and a zero suffix.
//! MUL sweeps `used + 2` because `(2^30 - 1) * 5^13` occupies three limbs;
//! a one-limb guard is unsound. ADD sweeps `used + 1`. CMP and SUB sweep
//! the wider operand. COPY clears a stale destination suffix. SHR treats
//! the bit above the extent as zero. SHL_CHECK keeps the original top-limb
//! abort. SHL_COPY starts at `used + limb_shift` and the following bit MUL
//! uses that updated extent. Pair sweeps round up to an even bound, capped
//! at 128. Identity stays on `acc`, `carry`, and `hi`: those locals are
//! stored from array Get, whose return schema is not Int. Every other
//! integer local is a proven Int, including the public parameter copies.
//!
//! Carry: a limb is `< 2^30` and `0 <= carry < factor`. `5^13 = 1220703125`
//! is greater than `2^30` and less than `2^31`. Chunk factors are `10^9`,
//! that `5^13`, or `2^k` with `k <= 30`. Then
//! `acc = limb * factor + carry < 2^61`, inside signed i64. A carry out of
//! limb 127 aborts. A shift that would discard a nonzero high limb aborts.
//! Neither is a rounded result.
//!
//! Radix 2/4/8/16/32 integers use the same limbs, scale 0, and the radix-aware
//! loader. Leading zeros are skipped. They do not use the decimal 768 cap.
use super::super::super::lowerer::assert_stmt;
use super::super::object::*;
use vm::{BuiltinFunction as B, Expr as E, FrontendIr, ParseError, Stmt as S, TypeSchema as T};

const BASE: i64 = 1 << 30;
const MAX_LIMBS: i64 = 128;
const RETAINED_DIGITS: i64 = 768;
const POW5_CHUNK: i64 = 13;
const POW5_CHUNK_VALUE: i64 = 1_220_703_125;
const POW10_CHUNK: i64 = 9;
const POW10_CHUNK_VALUE: i64 = 1_000_000_000;
const SHIFT_BIAS: i64 = 54;
const DIV_STEPS: i64 = 55;

const SCAN_SETUP: i64 = 1;
const SCAN_DIGIT: i64 = 2;
const COUNT_I64: i64 = 3;
const SCAN_DONE: i64 = 4;
const ADMIT_INIT: i64 = 5;
const ADMIT: i64 = 6;
const CHUNK_SETUP: i64 = 7;
const CHUNK_FACTOR: i64 = 8;
const LOAD_ENTER: i64 = 9;
const LOAD_DIGIT: i64 = 10;
const FLUSH_ADD: i64 = 11;
const FLUSH_CLEAR: i64 = 12;
const LOAD_TAIL: i64 = 13;
const TAIL_FACTOR: i64 = 14;
const TAIL_ADD: i64 = 15;
const SETI64: i64 = 16;
const DEN_POW: i64 = 17;
const POW5_SETUP: i64 = 18;
const POW5_GROW: i64 = 19;
const POW5_GROW_DEC: i64 = 20;
const PH_POW5_CHUNK: i64 = 21;
const POW5_CHUNK_DEC: i64 = 22;
const ROUND_BL_NUM_DONE: i64 = 23;
const ROUND_BL_DEN_DONE: i64 = 24;
const ROUND_ALIGN_HI: i64 = 25;
const ROUND_ALIGN_LO: i64 = 26;
const ROUND_CMP_ENTER: i64 = 27;
const ROUND_Q: i64 = 28;
const ROUND_SHIFT_DONE: i64 = 29;
const ROUND_BIAS: i64 = 30;
const DIV_INIT: i64 = 31;
const DIV_STEP: i64 = 32;
const DIV_AFTER_CMP: i64 = 33;
const DIV_AFTER_SUB: i64 = 34;
const DIV_AFTER_SHR: i64 = 35;
const ROUND_REM: i64 = 36;
const ROUND_REM_MUL: i64 = 37;
const ROUND_REM_CMP: i64 = 38;
const ROUND_TIE: i64 = 39;
const ROUND_MAT: i64 = 40;
const POW2_POS: i64 = 41;
const POW2_NEG: i64 = 42;
const BL_SCAN: i64 = 43;
const BL_COUNT: i64 = 44;
const SHL_ENTER: i64 = 45;
const SHL_CHECK: i64 = 46;
const SHL_COPY: i64 = 47;
const SHL_ZERO: i64 = 48;
const SHL_FACTOR: i64 = 49;
const SHL_AFTER: i64 = 50;
const MUL: i64 = 51;
const ADD: i64 = 52;
const COPY: i64 = 53;
const CMP: i64 = 54;
const SUB: i64 = 55;
const SHR: i64 = 56;
const PHASE_LAST: i64 = SHR;

struct G {
    num: u16,
    den: u16,
    scratch: u16,
    num_used: u16,
    den_used: u16,
    scratch_used: u16,
    sweep: u16,
    coeff: u16,
    scale: u16,
    acc: u16,
    carry: u16,
    borrow: u16,
    step: u16,
    cmp: u16,
    hi: u16,
    a_len: u16,
    b_len: u16,
    log_q: u16,
    q: u16,
    quot: u16,
    rounded: u16,
    exp_out: u16,
    digits: u16,
    sci: u16,
    use_span: u16,
    base: u16,
    unit: u16,
    idx: u16,
    sticky: u16,
    dropped: u16,
    chunk: u16,
    chunk_n: u16,
    radix: u16,
    factor: u16,
    limb_shift: u16,
    bit_shift: u16,
    nonzero: u16,
    chunk_len: u16,
    out: u16,
    pow2: u16,
    phase: u16,
    snap: u16,
    resume_mul: u16,
    resume_add: u16,
    resume_shl: u16,
    resume_copy: u16,
    resume_cmp: u16,
    resume_sub: u16,
    resume_shr: u16,
    resume_bl: u16,
    arr_sel: u16,
    src_sel: u16,
    cmp_mode: u16,
    mul_factor: u16,
    shl_arg: u16,
    bl_bits: u16,
    pow_exp: u16,
    sig_start: u16,
    sig_end: u16,
    carrier: u16,
    read: u16,
    decode: u16,
    identity: u16,
    l: u32,
}

fn p(identity: u16, slot: u16) -> E {
    call(identity, vec![var(slot)])
}

fn iv(g: &G, slot: u16) -> E {
    // Get's compiler return schema is not Int. These locals are assigned from
    // Get, so each read keeps the identity boundary. Every other integer local
    // is declared Int and written only by Int literals, Int arithmetic, decode,
    // or an identity call.
    if slot == g.acc || slot == g.carry || slot == g.hi {
        call(g.identity, vec![var(slot)])
    } else {
        var(slot)
    }
}

fn cap_limbs(value: E) -> E {
    choose(lt(value.clone(), int(MAX_LIMBS)), value, int(MAX_LIMBS))
}

fn even_bound(value: E) -> E {
    let capped = cap_limbs(value);
    add(capped.clone(), imod(capped, int(2)))
}

fn wider(left: E, right: E) -> E {
    choose(lt(left.clone(), right.clone()), right, left)
}

fn set_sweep(g: &G, used: E) -> S {
    assign(g.sweep, even_bound(used), g.l)
}

fn selected_used(g: &G) -> E {
    choose(
        eq(var(g.arr_sel), int(0)),
        var(g.num_used),
        choose(
            eq(var(g.arr_sel), int(1)),
            var(g.den_used),
            var(g.scratch_used),
        ),
    )
}

fn set_mul_sweep(g: &G) -> Vec<S> {
    let l = g.l;
    vec![branch(
        eq(var(g.arr_sel), int(0)),
        vec![set_sweep(g, add(var(g.num_used), int(2)))],
        vec![branch(
            eq(var(g.arr_sel), int(1)),
            vec![set_sweep(g, add(var(g.den_used), int(2)))],
            vec![
                assert_stmt(eq(var(g.arr_sel), int(2)), l),
                set_sweep(g, add(var(g.scratch_used), int(2))),
            ],
            l,
        )],
        l,
    )]
}

fn set_add_sweep(g: &G) -> S {
    set_sweep(g, add(var(g.num_used), int(1)))
}

fn set_copy_sweep(g: &G) -> Vec<S> {
    let l = g.l;
    vec![branch(
        eq(var(g.src_sel), int(0)),
        vec![set_sweep(g, wider(var(g.num_used), var(g.scratch_used)))],
        vec![
            assert_stmt(eq(var(g.src_sel), int(1)), l),
            set_sweep(g, wider(var(g.den_used), var(g.scratch_used))),
        ],
        l,
    )]
}

fn set_cmp_sweep(g: &G) -> Vec<S> {
    let l = g.l;
    vec![branch(
        eq(var(g.cmp_mode), int(0)),
        vec![set_sweep(g, wider(var(g.num_used), var(g.scratch_used)))],
        vec![
            assert_stmt(eq(var(g.cmp_mode), int(1)), l),
            set_sweep(g, wider(var(g.scratch_used), var(g.den_used))),
        ],
        l,
    )]
}

fn set_sub_sweep(g: &G) -> S {
    set_sweep(g, wider(var(g.num_used), var(g.scratch_used)))
}

fn active_src(g: &G) -> E {
    choose(eq(var(g.src_sel), int(0)), var(g.num_used), var(g.den_used))
}

fn bounded_pairs(g: &G, phase: i64, prepare: Vec<S>, step: Vec<S>, done: Vec<S>) -> Vec<S> {
    let l = g.l;
    let mut running = step;
    running.push(assign(g.idx, add(iv(g, g.idx), int(2)), l));
    running.push(go(g, phase));
    let compare = branch(lt(iv(g, g.idx), var(g.sweep)), running, done, l);
    vec![
        branch(eq(iv(g, g.idx), int(0)), prepare, vec![], l),
        compare,
    ]
}

fn tighten_two(g: &G, slot: u16, used: u16) -> Vec<S> {
    let l = g.l;
    let grown = assign(used, cap_limbs(add(var(used), int(2))), l);
    let plus = assign(used, cap_limbs(add(var(used), int(1))), l);
    vec![branch(
        lt(var(used), int(MAX_LIMBS)),
        vec![branch(
            lt(add(var(used), int(1)), int(MAX_LIMBS)),
            vec![branch(
                not(eq(aget(slot, add(var(used), int(1))), int(0))),
                vec![grown],
                vec![branch(
                    not(eq(aget(slot, var(used)), int(0))),
                    vec![plus],
                    vec![],
                    l,
                )],
                l,
            )],
            vec![branch(
                not(eq(aget(slot, var(used)), int(0))),
                vec![assign(used, cap_limbs(add(var(used), int(1))), l)],
                vec![],
                l,
            )],
            l,
        )],
        vec![],
        l,
    )]
}

fn tighten_mul(g: &G) -> Vec<S> {
    let l = g.l;
    vec![branch(
        eq(var(g.arr_sel), int(0)),
        tighten_two(g, g.num, g.num_used),
        vec![branch(
            eq(var(g.arr_sel), int(1)),
            tighten_two(g, g.den, g.den_used),
            {
                let mut arm = vec![assert_stmt(eq(var(g.arr_sel), int(2)), l)];
                arm.extend(tighten_two(g, g.scratch, g.scratch_used));
                arm
            },
            l,
        )],
        l,
    )]
}

fn tighten_add(g: &G) -> Vec<S> {
    let l = g.l;
    let used = g.num_used;
    vec![branch(
        lt(var(used), int(MAX_LIMBS)),
        vec![branch(
            not(eq(aget(g.num, var(used)), int(0))),
            vec![assign(used, cap_limbs(add(var(used), int(1))), l)],
            vec![],
            l,
        )],
        vec![],
        l,
    )]
}

fn bind_scratch_used(g: &G) -> Vec<S> {
    let l = g.l;
    vec![branch(
        eq(var(g.src_sel), int(0)),
        vec![assign(g.scratch_used, var(g.num_used), l)],
        vec![
            assert_stmt(eq(var(g.src_sel), int(1)), l),
            assign(g.scratch_used, var(g.den_used), l),
        ],
        l,
    )]
}

fn bump_selected(g: &G) -> Vec<S> {
    let l = g.l;
    let extra = var(g.limb_shift);
    vec![branch(
        eq(var(g.arr_sel), int(0)),
        vec![assign(
            g.num_used,
            cap_limbs(add(var(g.num_used), extra.clone())),
            l,
        )],
        vec![branch(
            eq(var(g.arr_sel), int(1)),
            vec![assign(
                g.den_used,
                cap_limbs(add(var(g.den_used), extra.clone())),
                l,
            )],
            {
                let mut arm = vec![assert_stmt(eq(var(g.arr_sel), int(2)), l)];
                arm.push(assign(
                    g.scratch_used,
                    cap_limbs(add(var(g.scratch_used), extra)),
                    l,
                ));
                arm
            },
            l,
        )],
        l,
    )]
}

fn shl_copy_start(g: &G) -> Vec<S> {
    let start = sub(add(selected_used(g), var(g.limb_shift)), int(1));
    vec![
        assign(
            g.idx,
            choose(
                lt(start.clone(), int(MAX_LIMBS - 1)),
                start,
                int(MAX_LIMBS - 1),
            ),
            g.l,
        ),
        go(g, SHL_COPY),
    ]
}

fn shr_into(g: &G, index: E, incoming: E) -> Vec<S> {
    let l = g.l;
    vec![
        assign(g.acc, idiv(aget(g.scratch, index.clone()), int(2)), l),
        assign(g.carry, mul(imod(incoming, int(2)), int(1 << 29)), l),
        aput(g.scratch, index, add(iv(g, g.acc), iv(g, g.carry)), l),
    ]
}

fn finish_shr(g: &G) -> Vec<S> {
    let l = g.l;
    vec![
        branch(
            lt(int(1), var(g.scratch_used)),
            vec![branch(
                eq(aget(g.scratch, sub(var(g.scratch_used), int(1))), int(0)),
                vec![assign(g.scratch_used, sub(var(g.scratch_used), int(1)), l)],
                vec![],
                l,
            )],
            vec![],
            l,
        ),
        resume(g, g.resume_shr),
    ]
}

fn aget(slot: u16, index: E) -> E {
    call(
        B::Get.call_index(),
        vec![E::Borrow(Box::new(var(slot))), index],
    )
}

fn alen(slot: u16) -> E {
    call(B::Len.call_index(), vec![E::Borrow(Box::new(var(slot)))])
}

fn aput(slot: u16, index: E, value: E, l: u32) -> S {
    assign(
        slot,
        call(B::Set.call_index(), vec![var(slot), index, value]),
        l,
    )
}

fn apush(slot: u16, value: E, l: u32) -> S {
    assign(
        slot,
        call(B::ArrayPush.call_index(), vec![var(slot), value]),
        l,
    )
}

fn wh(condition: E, body: Vec<S>, l: u32) -> S {
    S::While {
        condition,
        body,
        line: l,
    }
}

fn idiv(a: E, b: E) -> E {
    E::Div(Box::new(a), Box::new(b))
}

fn imod(a: E, b: E) -> E {
    E::Mod(Box::new(a), Box::new(b))
}

fn go(g: &G, phase: i64) -> S {
    assign(g.phase, int(phase), g.l)
}

fn resume(g: &G, slot: u16) -> S {
    assign(g.phase, iv(g, slot), g.l)
}

fn finish_out(g: &G, value: E) -> Vec<S> {
    vec![
        assign(g.out, value, g.l),
        assign(g.cmp, int(0), g.l),
        go(g, 0),
    ]
}

fn decoded_unit(g: &G) -> E {
    call(
        g.decode,
        vec![num(call(
            g.read,
            vec![
                var(g.carrier),
                num(add(add(iv(g, g.base), int(3)), iv(g, g.step))),
            ],
        ))],
    )
}

fn decimal_digit(g: &G) -> Vec<S> {
    let l = g.l;
    vec![
        assign(g.acc, int(-1), l),
        branch(
            and(not(lt(iv(g, g.unit), int(48))), lt(iv(g, g.unit), int(58))),
            vec![assign(g.acc, sub(iv(g, g.unit), int(48)), l)],
            vec![],
            l,
        ),
    ]
}

fn radix_digit(g: &G) -> Vec<S> {
    let l = g.l;
    let mut stmts = decimal_digit(g);
    stmts.push(branch(
        and(not(lt(iv(g, g.unit), int(97))), lt(iv(g, g.unit), int(123))),
        vec![assign(g.acc, sub(iv(g, g.unit), int(87)), l)],
        vec![],
        l,
    ));
    stmts.push(branch(
        and(not(lt(iv(g, g.unit), int(65))), lt(iv(g, g.unit), int(91))),
        vec![assign(g.acc, sub(iv(g, g.unit), int(55)), l)],
        vec![],
        l,
    ));
    stmts.push(branch(
        or(
            lt(iv(g, g.acc), int(0)),
            not(lt(iv(g, g.acc), iv(g, g.radix))),
        ),
        vec![assign(g.acc, int(-1), l)],
        vec![],
        l,
    ));
    stmts
}

fn ingest(g: &G) -> Vec<S> {
    let l = g.l;
    let mut stmts = vec![assign(g.unit, decoded_unit(g), l)];
    stmts.extend(decimal_digit(g));
    stmts.push(branch(
        not(eq(iv(g, g.radix), int(10))),
        radix_digit(g),
        vec![],
        l,
    ));
    stmts
}

fn on_arr(g: &G, each: impl Fn(u16) -> Vec<S>) -> Vec<S> {
    let l = g.l;
    vec![branch(
        eq(iv(g, g.arr_sel), int(0)),
        each(g.num),
        vec![branch(
            eq(iv(g, g.arr_sel), int(1)),
            each(g.den),
            {
                let mut arm = vec![assert_stmt(eq(iv(g, g.arr_sel), int(2)), l)];
                arm.extend(each(g.scratch));
                arm
            },
            l,
        )],
        l,
    )]
}

fn read_sel(g: &G, dest: u16, index: E) -> Vec<S> {
    on_arr(g, |slot| vec![assign(dest, aget(slot, index.clone()), g.l)])
}

fn enter_mul(g: &G, factor: E, resume_at: i64) -> Vec<S> {
    let l = g.l;
    vec![
        assign(g.mul_factor, factor, l),
        assign(g.carry, int(0), l),
        assign(g.idx, int(0), l),
        assign(g.resume_mul, int(resume_at), l),
        go(g, MUL),
    ]
}

fn enter_add(g: &G, amount: E, resume_at: i64) -> Vec<S> {
    let l = g.l;
    vec![
        assign(g.carry, amount, l),
        assign(g.idx, int(0), l),
        assign(g.resume_add, int(resume_at), l),
        go(g, ADD),
    ]
}

fn enter_copy(g: &G, src: i64, resume_at: i64) -> Vec<S> {
    let l = g.l;
    vec![
        assign(g.src_sel, int(src), l),
        assign(g.idx, int(0), l),
        assign(g.resume_copy, int(resume_at), l),
        go(g, COPY),
    ]
}

fn enter_shl(g: &G, bits: E, arr: i64, resume_at: i64) -> Vec<S> {
    let l = g.l;
    vec![
        assign(g.shl_arg, bits, l),
        assign(g.arr_sel, int(arr), l),
        assign(g.resume_shl, int(resume_at), l),
        go(g, SHL_ENTER),
    ]
}

fn enter_bl(g: &G, arr: i64, resume_at: i64) -> Vec<S> {
    let l = g.l;
    vec![
        assign(g.arr_sel, int(arr), l),
        assign(g.resume_bl, int(resume_at), l),
        assign(g.hi, int(-1), l),
        assign(g.idx, int(0), l),
        go(g, BL_SCAN),
    ]
}

fn enter_cmp(g: &G, mode: i64, resume_at: i64) -> Vec<S> {
    let l = g.l;
    vec![
        assign(g.cmp_mode, int(mode), l),
        assign(g.idx, int(0), l),
        assign(g.cmp, int(0), l),
        assign(g.resume_cmp, int(resume_at), l),
        go(g, CMP),
    ]
}

fn count_digit(g: &G) -> Vec<S> {
    let l = g.l;
    vec![branch(
        and(eq(iv(g, g.digits), int(0)), eq(iv(g, g.acc), int(0))),
        vec![],
        vec![branch(
            or(
                lt(iv(g, g.digits), int(RETAINED_DIGITS)),
                not(eq(iv(g, g.radix), int(10))),
            ),
            vec![
                branch(
                    eq(iv(g, g.digits), int(0)),
                    vec![assign(g.bl_bits, iv(g, g.acc), l)],
                    vec![],
                    l,
                ),
                assign(g.digits, add(iv(g, g.digits), int(1)), l),
            ],
            vec![
                assign(g.dropped, add(iv(g, g.dropped), int(1)), l),
                branch(
                    not(eq(iv(g, g.acc), int(0))),
                    vec![assign(g.sticky, int(1), l)],
                    vec![],
                    l,
                ),
            ],
            l,
        )],
        l,
    )]
}

fn ph_scan_setup(g: &G) -> Vec<S> {
    let l = g.l;
    vec![
        assign(g.digits, int(0), l),
        assign(g.dropped, int(0), l),
        assign(g.sticky, int(0), l),
        branch(
            not(eq(iv(g, g.use_span), int(0))),
            vec![assign(g.step, iv(g, g.sig_start), l), go(g, SCAN_DIGIT)],
            vec![
                assign(g.acc, iv(g, g.coeff), l),
                assign(
                    g.nonzero,
                    choose(eq(iv(g, g.coeff), int(0)), int(0), int(1)),
                    l,
                ),
                go(g, COUNT_I64),
            ],
            l,
        ),
    ]
}

fn ph_scan_digit(g: &G) -> Vec<S> {
    let l = g.l;
    let mut take = ingest(g);
    take.push(branch(
        not(lt(iv(g, g.acc), int(0))),
        count_digit(g),
        vec![],
        l,
    ));
    take.push(assign(g.step, add(iv(g, g.step), int(1)), l));
    take.push(go(g, SCAN_DIGIT));
    vec![branch(
        lt(iv(g, g.step), iv(g, g.sig_end)),
        take,
        vec![go(g, SCAN_DONE)],
        l,
    )]
}

fn ph_count_i64(g: &G) -> Vec<S> {
    let l = g.l;
    vec![branch(
        lt(int(0), iv(g, g.acc)),
        vec![
            assign(g.acc, idiv(iv(g, g.acc), int(10)), l),
            assign(g.digits, add(iv(g, g.digits), int(1)), l),
            go(g, COUNT_I64),
        ],
        vec![go(g, SCAN_DONE)],
        l,
    )]
}

fn ph_scan_done(g: &G) -> Vec<S> {
    let l = g.l;
    vec![
        branch(
            not(eq(iv(g, g.use_span), int(0))),
            vec![assign(
                g.nonzero,
                choose(eq(iv(g, g.digits), int(0)), int(0), int(1)),
                l,
            )],
            vec![],
            l,
        ),
        branch(
            eq(iv(g, g.radix), int(10)),
            decimal_exponent_gate(g),
            pow2_bitlength_gate(g),
            l,
        ),
    ]
}

fn decimal_exponent_gate(g: &G) -> Vec<S> {
    let l = g.l;
    vec![
        assign(g.scale, add(iv(g, g.scale), iv(g, g.dropped)), l),
        assign(g.sci, sub(add(iv(g, g.scale), iv(g, g.digits)), int(1)), l),
        branch(
            or(eq(iv(g, g.nonzero), int(0)), lt(iv(g, g.sci), int(-324))),
            finish_out(g, E::Float(0.0)),
            vec![branch(
                lt(int(308), iv(g, g.sci)),
                finish_out(g, E::Float(f64::INFINITY)),
                vec![go(g, ADMIT_INIT)],
                l,
            )],
            l,
        ),
    ]
}

fn pow2_bitlength_gate(g: &G) -> Vec<S> {
    let l = g.l;
    vec![
        assert_stmt(not(eq(iv(g, g.use_span), int(0))), l),
        assert_stmt(
            or(
                eq(iv(g, g.radix), int(2)),
                or(
                    eq(iv(g, g.radix), int(4)),
                    or(
                        eq(iv(g, g.radix), int(8)),
                        or(eq(iv(g, g.radix), int(16)), eq(iv(g, g.radix), int(32))),
                    ),
                ),
            ),
            l,
        ),
        branch(
            eq(iv(g, g.nonzero), int(0)),
            finish_out(g, E::Float(0.0)),
            vec![branch(
                lt(int(1025), iv(g, g.digits)),
                finish_out(g, E::Float(f64::INFINITY)),
                pow2_classified_bits(g),
                l,
            )],
            l,
        ),
    ]
}

fn pow2_classified_bits(g: &G) -> Vec<S> {
    let l = g.l;
    let first = iv(g, g.bl_bits);
    let digits = iv(g, g.digits);
    vec![
        assert_stmt(not(lt(digits.clone(), int(1))), l),
        assert_stmt(not(lt(int(1025), digits.clone())), l),
        assert_stmt(lt(int(0), first.clone()), l),
        assert_stmt(lt(first.clone(), iv(g, g.radix)), l),
        assign(
            g.sci,
            add(
                choose(
                    lt(first.clone(), int(2)),
                    int(1),
                    choose(
                        lt(first.clone(), int(4)),
                        int(2),
                        choose(
                            lt(first.clone(), int(8)),
                            int(3),
                            choose(lt(first, int(16)), int(4), int(5)),
                        ),
                    ),
                ),
                mul(
                    choose(
                        eq(iv(g, g.radix), int(2)),
                        int(1),
                        choose(
                            eq(iv(g, g.radix), int(4)),
                            int(2),
                            choose(
                                eq(iv(g, g.radix), int(8)),
                                int(3),
                                choose(eq(iv(g, g.radix), int(16)), int(4), int(5)),
                            ),
                        ),
                    ),
                    sub(digits, int(1)),
                ),
            ),
            l,
        ),
        branch(
            not(lt(iv(g, g.sci), int(1025))),
            finish_out(g, E::Float(f64::INFINITY)),
            vec![go(g, ADMIT_INIT)],
            l,
        ),
    ]
}

fn ph_admit_init(g: &G) -> Vec<S> {
    let l = g.l;
    vec![
        assert_stmt(
            and(
                not(lt(iv(g, g.scale), int(-1091))),
                not(lt(int(308), iv(g, g.scale))),
            ),
            l,
        ),
        assert_stmt(eq(alen(g.num), int(0)), l),
        assert_stmt(eq(alen(g.den), int(0)), l),
        assert_stmt(eq(alen(g.scratch), int(0)), l),
        assign(g.idx, int(0), l),
        go(g, ADMIT),
    ]
}

fn ph_admit(g: &G) -> Vec<S> {
    let l = g.l;
    vec![branch(
        lt(iv(g, g.idx), int(MAX_LIMBS)),
        vec![
            assert_stmt(lt(alen(g.num), int(MAX_LIMBS)), l),
            apush(g.num, int(0), l),
            assert_stmt(lt(alen(g.den), int(MAX_LIMBS)), l),
            apush(g.den, int(0), l),
            assert_stmt(lt(alen(g.scratch), int(MAX_LIMBS)), l),
            apush(g.scratch, int(0), l),
            assign(g.idx, add(iv(g, g.idx), int(1)), l),
            go(g, ADMIT),
        ],
        vec![
            assert_stmt(eq(alen(g.num), int(MAX_LIMBS)), l),
            assert_stmt(eq(alen(g.den), int(MAX_LIMBS)), l),
            assert_stmt(eq(alen(g.scratch), int(MAX_LIMBS)), l),
            assign(g.num_used, int(1), l),
            assign(g.den_used, int(1), l),
            assign(g.scratch_used, int(1), l),
            go(g, CHUNK_SETUP),
        ],
        l,
    )]
}

fn ph_chunk_setup(g: &G) -> Vec<S> {
    let l = g.l;
    vec![branch(
        eq(iv(g, g.radix), int(10)),
        vec![
            assign(g.factor, int(POW10_CHUNK_VALUE), l),
            assign(g.chunk_len, int(POW10_CHUNK), l),
            go(g, LOAD_ENTER),
        ],
        vec![
            assign(g.chunk_len, int(0), l),
            assign(g.factor, int(1), l),
            go(g, CHUNK_FACTOR),
        ],
        l,
    )]
}

fn ph_chunk_factor(g: &G) -> Vec<S> {
    let l = g.l;
    vec![branch(
        and(
            lt(iv(g, g.chunk_len), int(30)),
            not(lt(idiv(int(BASE), iv(g, g.radix)), iv(g, g.factor))),
        ),
        vec![
            assign(g.factor, mul(iv(g, g.factor), iv(g, g.radix)), l),
            assign(g.chunk_len, add(iv(g, g.chunk_len), int(1)), l),
            go(g, CHUNK_FACTOR),
        ],
        vec![go(g, LOAD_ENTER)],
        l,
    )]
}

fn ph_load_enter(g: &G) -> Vec<S> {
    let l = g.l;
    vec![branch(
        not(eq(iv(g, g.use_span), int(0))),
        vec![
            assign(g.step, iv(g, g.sig_start), l),
            assign(g.chunk, int(0), l),
            assign(g.chunk_n, int(0), l),
            assign(g.hi, int(0), l),
            go(g, LOAD_DIGIT),
        ],
        vec![
            assign(g.acc, iv(g, g.coeff), l),
            assign(g.idx, int(0), l),
            go(g, SETI64),
        ],
        l,
    )]
}

fn take_significant(g: &G) -> Vec<S> {
    let l = g.l;
    let mut keep = vec![
        assign(g.hi, add(iv(g, g.hi), int(1)), l),
        assign(
            g.chunk,
            add(mul(iv(g, g.chunk), iv(g, g.radix)), iv(g, g.acc)),
            l,
        ),
        assign(g.chunk_n, add(iv(g, g.chunk_n), int(1)), l),
    ];
    let mut flush = vec![assign(g.arr_sel, int(0), l)];
    flush.extend(enter_mul(g, iv(g, g.factor), FLUSH_ADD));
    keep.push(branch(
        eq(iv(g, g.chunk_n), iv(g, g.chunk_len)),
        flush,
        vec![
            assign(g.step, add(iv(g, g.step), int(1)), l),
            go(g, LOAD_DIGIT),
        ],
        l,
    ));
    vec![branch(
        and(eq(iv(g, g.hi), int(0)), eq(iv(g, g.acc), int(0))),
        vec![
            assign(g.step, add(iv(g, g.step), int(1)), l),
            go(g, LOAD_DIGIT),
        ],
        keep,
        l,
    )]
}

fn ph_load_digit(g: &G) -> Vec<S> {
    let l = g.l;
    let mut take = ingest(g);
    take.push(branch(
        and(
            not(lt(iv(g, g.acc), int(0))),
            or(
                lt(iv(g, g.hi), int(RETAINED_DIGITS)),
                not(eq(iv(g, g.radix), int(10))),
            ),
        ),
        take_significant(g),
        vec![
            assign(g.step, add(iv(g, g.step), int(1)), l),
            go(g, LOAD_DIGIT),
        ],
        l,
    ));
    vec![branch(
        lt(iv(g, g.step), iv(g, g.sig_end)),
        take,
        vec![go(g, LOAD_TAIL)],
        l,
    )]
}

fn ph_flush_add(g: &G) -> Vec<S> {
    enter_add(g, iv(g, g.chunk), FLUSH_CLEAR)
}

fn ph_flush_clear(g: &G) -> Vec<S> {
    let l = g.l;
    vec![
        assign(g.chunk, int(0), l),
        assign(g.chunk_n, int(0), l),
        assign(g.step, add(iv(g, g.step), int(1)), l),
        go(g, LOAD_DIGIT),
    ]
}

fn ph_load_tail(g: &G) -> Vec<S> {
    let l = g.l;
    vec![branch(
        lt(int(0), iv(g, g.chunk_n)),
        vec![
            assign(g.bit_shift, int(1), l),
            assign(g.idx, int(0), l),
            go(g, TAIL_FACTOR),
        ],
        vec![go(g, DEN_POW)],
        l,
    )]
}

fn ph_tail_factor(g: &G) -> Vec<S> {
    let l = g.l;
    vec![branch(
        lt(iv(g, g.idx), iv(g, g.chunk_n)),
        vec![
            assign(g.bit_shift, mul(iv(g, g.bit_shift), iv(g, g.radix)), l),
            assign(g.idx, add(iv(g, g.idx), int(1)), l),
            go(g, TAIL_FACTOR),
        ],
        {
            let mut arm = vec![assign(g.arr_sel, int(0), l)];
            arm.extend(enter_mul(g, iv(g, g.bit_shift), TAIL_ADD));
            arm
        },
        l,
    )]
}

fn ph_tail_add(g: &G) -> Vec<S> {
    enter_add(g, iv(g, g.chunk), DEN_POW)
}

fn ph_seti64(g: &G) -> Vec<S> {
    let l = g.l;
    vec![branch(
        lt(iv(g, g.idx), int(3)),
        vec![
            aput(g.num, iv(g, g.idx), imod(iv(g, g.acc), int(BASE)), l),
            assign(g.acc, idiv(iv(g, g.acc), int(BASE)), l),
            assign(g.idx, add(iv(g, g.idx), int(1)), l),
            go(g, SETI64),
        ],
        vec![
            assert_stmt(eq(iv(g, g.acc), int(0)), l),
            assign(g.num_used, int(3), l),
            go(g, DEN_POW),
        ],
        l,
    )]
}

fn ph_den_pow(g: &G) -> Vec<S> {
    let l = g.l;
    vec![
        aput(g.den, int(0), int(1), l),
        assign(g.den_used, int(1), l),
        branch(
            not(lt(iv(g, g.scale), int(0))),
            vec![
                assign(g.arr_sel, int(0), l),
                assign(g.pow_exp, iv(g, g.scale), l),
            ],
            vec![
                assign(g.arr_sel, int(1), l),
                assign(g.pow_exp, E::Neg(Box::new(iv(g, g.scale))), l),
            ],
            l,
        ),
        go(g, POW5_SETUP),
    ]
}

fn ph_pow5_setup(g: &G) -> Vec<S> {
    let l = g.l;
    vec![
        assign(g.factor, iv(g, g.pow_exp), l),
        assert_stmt(not(lt(iv(g, g.factor), int(0))), l),
        assert_stmt(not(lt(int(1091), iv(g, g.factor))), l),
        assign(g.step, imod(iv(g, g.factor), int(POW5_CHUNK)), l),
        assign(g.factor, idiv(iv(g, g.factor), int(POW5_CHUNK)), l),
        go(g, POW5_GROW),
    ]
}

fn ph_pow5_grow(g: &G) -> Vec<S> {
    vec![branch(
        lt(int(0), iv(g, g.step)),
        enter_mul(g, int(5), POW5_GROW_DEC),
        vec![go(g, PH_POW5_CHUNK)],
        g.l,
    )]
}

fn ph_pow5_grow_dec(g: &G) -> Vec<S> {
    vec![
        assign(g.step, sub(iv(g, g.step), int(1)), g.l),
        go(g, POW5_GROW),
    ]
}

fn ph_pow5_chunk(g: &G) -> Vec<S> {
    let l = g.l;
    vec![branch(
        lt(int(0), iv(g, g.factor)),
        enter_mul(g, int(POW5_CHUNK_VALUE), POW5_CHUNK_DEC),
        {
            let mut arm = vec![assign(g.cmp, int(1), l)];
            arm.extend(enter_bl(g, 0, ROUND_BL_NUM_DONE));
            arm
        },
        l,
    )]
}

fn ph_pow5_chunk_dec(g: &G) -> Vec<S> {
    vec![
        assign(g.factor, sub(iv(g, g.factor), int(1)), g.l),
        go(g, PH_POW5_CHUNK),
    ]
}

fn ph_round_bl_num_done(g: &G) -> Vec<S> {
    let mut arm = vec![assign(g.a_len, sub(iv(g, g.bl_bits), int(1)), g.l)];
    arm.extend(enter_bl(g, 1, ROUND_BL_DEN_DONE));
    arm
}

fn ph_round_bl_den_done(g: &G) -> Vec<S> {
    let l = g.l;
    vec![
        assign(g.b_len, sub(iv(g, g.bl_bits), int(1)), l),
        branch(
            not(lt(iv(g, g.a_len), iv(g, g.b_len))),
            enter_copy(g, 1, ROUND_ALIGN_HI),
            enter_copy(g, 0, ROUND_ALIGN_LO),
            l,
        ),
    ]
}

fn ph_round_align_hi(g: &G) -> Vec<S> {
    let mut arm = vec![assign(g.cmp_mode, int(0), g.l)];
    arm.extend(enter_shl(
        g,
        sub(iv(g, g.a_len), iv(g, g.b_len)),
        2,
        ROUND_CMP_ENTER,
    ));
    arm
}

fn ph_round_align_lo(g: &G) -> Vec<S> {
    let l = g.l;
    let mut arm = vec![assign(g.cmp_mode, int(1), l)];
    arm.extend(enter_shl(
        g,
        sub(iv(g, g.b_len), iv(g, g.a_len)),
        2,
        ROUND_CMP_ENTER,
    ));
    arm
}

fn ph_round_cmp_enter(g: &G) -> Vec<S> {
    let l = g.l;
    // High arm compares num with shifted den. Low arm compares shifted num with den.
    vec![branch(
        eq(iv(g, g.cmp_mode), int(1)),
        enter_cmp(g, 1, ROUND_Q),
        enter_cmp(g, 0, ROUND_Q),
        l,
    )]
}

fn ph_round_q(g: &G) -> Vec<S> {
    let l = g.l;
    let in_range = vec![
        branch(
            lt(iv(g, g.q), int(-1022)),
            vec![assign(g.q, int(-1022), l)],
            vec![],
            l,
        ),
        assign(g.scale, sub(iv(g, g.scale), sub(iv(g, g.q), int(52))), l),
        branch(
            not(lt(iv(g, g.scale), int(0))),
            enter_shl(g, iv(g, g.scale), 0, ROUND_SHIFT_DONE),
            enter_shl(g, E::Neg(Box::new(iv(g, g.scale))), 1, ROUND_SHIFT_DONE),
            l,
        ),
    ];
    let mut body = vec![assign(
        g.log_q,
        choose(
            not(lt(iv(g, g.cmp), int(0))),
            sub(iv(g, g.a_len), iv(g, g.b_len)),
            sub(sub(iv(g, g.a_len), iv(g, g.b_len)), int(1)),
        ),
        l,
    )];
    body.push(assign(g.q, add(iv(g, g.log_q), iv(g, g.scale)), l));
    body.push(branch(
        not(lt(iv(g, g.q), int(1024))),
        finish_out(g, E::Float(f64::INFINITY)),
        vec![branch(
            lt(iv(g, g.q), int(-1075)),
            finish_out(g, E::Float(0.0)),
            in_range,
            l,
        )],
        l,
    ));
    body
}

fn ph_round_shift_done(g: &G) -> Vec<S> {
    enter_copy(g, 1, ROUND_BIAS)
}

fn ph_round_bias(g: &G) -> Vec<S> {
    enter_shl(g, int(SHIFT_BIAS), 2, DIV_INIT)
}

fn ph_div_init(g: &G) -> Vec<S> {
    vec![
        assign(g.quot, int(0), g.l),
        assign(g.step, int(0), g.l),
        go(g, DIV_STEP),
    ]
}

fn ph_div_step(g: &G) -> Vec<S> {
    let l = g.l;
    vec![branch(
        lt(iv(g, g.step), int(DIV_STEPS)),
        {
            let mut arm = vec![assign(g.quot, mul(iv(g, g.quot), int(2)), l)];
            arm.extend(enter_cmp(g, 0, DIV_AFTER_CMP));
            arm
        },
        vec![go(g, ROUND_REM)],
        l,
    )]
}

fn ph_div_after_cmp(g: &G) -> Vec<S> {
    let l = g.l;
    vec![branch(
        not(lt(iv(g, g.cmp), int(0))),
        vec![
            assign(g.borrow, int(0), l),
            assign(g.idx, int(0), l),
            assign(g.resume_sub, int(DIV_AFTER_SUB), l),
            go(g, SUB),
        ],
        vec![
            assign(g.idx, int(0), l),
            assign(g.resume_shr, int(DIV_AFTER_SHR), l),
            go(g, SHR),
        ],
        l,
    )]
}

fn ph_div_after_sub(g: &G) -> Vec<S> {
    let l = g.l;
    vec![
        assign(g.quot, add(iv(g, g.quot), int(1)), l),
        assign(g.idx, int(0), l),
        assign(g.resume_shr, int(DIV_AFTER_SHR), l),
        go(g, SHR),
    ]
}

fn ph_div_after_shr(g: &G) -> Vec<S> {
    vec![
        assign(g.step, add(iv(g, g.step), int(1)), g.l),
        go(g, DIV_STEP),
    ]
}

fn ph_round_rem(g: &G) -> Vec<S> {
    enter_copy(g, 0, ROUND_REM_MUL)
}

fn ph_round_rem_mul(g: &G) -> Vec<S> {
    let mut arm = vec![assign(g.arr_sel, int(2), g.l)];
    arm.extend(enter_mul(g, int(2), ROUND_REM_CMP));
    arm
}

fn ph_round_rem_cmp(g: &G) -> Vec<S> {
    enter_cmp(g, 1, ROUND_TIE)
}

fn ph_round_tie(g: &G) -> Vec<S> {
    let l = g.l;
    vec![
        assign(
            g.rounded,
            choose(
                or(
                    lt(iv(g, g.cmp), int(0)),
                    and(
                        eq(iv(g, g.cmp), int(0)),
                        eq(imod(iv(g, g.quot), int(2)), int(0)),
                    ),
                ),
                iv(g, g.quot),
                add(iv(g, g.quot), int(1)),
            ),
            l,
        ),
        branch(
            and(not(eq(iv(g, g.sticky), int(0))), eq(iv(g, g.cmp), int(0))),
            vec![assign(g.rounded, add(iv(g, g.quot), int(1)), l)],
            vec![],
            l,
        ),
        branch(
            eq(iv(g, g.rounded), int(0)),
            finish_out(g, E::Float(0.0)),
            vec![go(g, ROUND_MAT)],
            l,
        ),
    ]
}

fn ph_round_mat(g: &G) -> Vec<S> {
    let l = g.l;
    vec![
        branch(
            and(
                lt(int(-1022), iv(g, g.q)),
                eq(iv(g, g.rounded), int(1 << 53)),
            ),
            vec![
                assign(g.rounded, int(1 << 52), l),
                assign(g.q, add(iv(g, g.q), int(1)), l),
            ],
            vec![],
            l,
        ),
        branch(
            not(lt(iv(g, g.q), int(1024))),
            finish_out(g, E::Float(f64::INFINITY)),
            vec![
                assign(g.exp_out, sub(iv(g, g.q), int(52)), l),
                assign(g.pow2, E::Float(1.0), l),
                assign(g.step, int(0), l),
                branch(
                    not(lt(iv(g, g.exp_out), int(0))),
                    vec![go(g, POW2_POS)],
                    vec![go(g, POW2_NEG)],
                    l,
                ),
            ],
            l,
        ),
    ]
}

fn ph_pow2_pos(g: &G) -> Vec<S> {
    let l = g.l;
    vec![branch(
        lt(iv(g, g.step), iv(g, g.exp_out)),
        vec![
            assign(g.pow2, mul(var(g.pow2), E::Float(2.0)), l),
            assign(g.step, add(iv(g, g.step), int(1)), l),
            go(g, POW2_POS),
        ],
        vec![
            assign(g.out, mul(num(iv(g, g.rounded)), var(g.pow2)), l),
            go(g, 0),
        ],
        l,
    )]
}

fn ph_pow2_neg(g: &G) -> Vec<S> {
    let l = g.l;
    vec![branch(
        lt(iv(g, g.step), E::Neg(Box::new(iv(g, g.exp_out)))),
        vec![
            assign(g.pow2, idiv(var(g.pow2), E::Float(2.0)), l),
            assign(g.step, add(iv(g, g.step), int(1)), l),
            go(g, POW2_NEG),
        ],
        vec![
            assign(g.out, mul(num(iv(g, g.rounded)), var(g.pow2)), l),
            go(g, 0),
        ],
        l,
    )]
}

fn ph_bl_scan(g: &G) -> Vec<S> {
    let l = g.l;
    let mut step = read_sel(g, g.acc, iv(g, g.idx));
    step.push(branch(
        lt(int(0), iv(g, g.acc)),
        vec![assign(g.hi, iv(g, g.idx), l)],
        vec![],
        l,
    ));
    step.push(assign(g.idx, add(iv(g, g.idx), int(1)), l));
    step.push(go(g, BL_SCAN));
    let mut count = vec![assign(g.idx, iv(g, g.hi), l)];
    count.extend(read_sel(g, g.acc, iv(g, g.idx)));
    count.push(assign(g.bl_bits, int(0), l));
    count.push(go(g, BL_COUNT));
    vec![branch(
        lt(iv(g, g.idx), selected_used(g)),
        step,
        vec![branch(
            lt(iv(g, g.hi), int(0)),
            vec![assign(g.bl_bits, int(0), l), resume(g, g.resume_bl)],
            count,
            l,
        )],
        l,
    )]
}

fn ph_bl_count(g: &G) -> Vec<S> {
    let l = g.l;
    vec![branch(
        lt(int(0), iv(g, g.acc)),
        vec![
            assign(g.acc, idiv(iv(g, g.acc), int(2)), l),
            assign(g.bl_bits, add(iv(g, g.bl_bits), int(1)), l),
            go(g, BL_COUNT),
        ],
        vec![
            assign(
                g.bl_bits,
                add(mul(iv(g, g.hi), int(30)), iv(g, g.bl_bits)),
                l,
            ),
            resume(g, g.resume_bl),
        ],
        l,
    )]
}

fn ph_shl_enter(g: &G) -> Vec<S> {
    let l = g.l;
    vec![
        assign(g.limb_shift, idiv(iv(g, g.shl_arg), int(30)), l),
        assign(g.bit_shift, imod(iv(g, g.shl_arg), int(30)), l),
        assert_stmt(lt(iv(g, g.limb_shift), int(MAX_LIMBS)), l),
        assign(g.idx, int(0), l),
        go(g, SHL_CHECK),
    ]
}

fn ph_shl_check(g: &G) -> Vec<S> {
    let l = g.l;
    let index = add(sub(int(MAX_LIMBS), iv(g, g.limb_shift)), iv(g, g.idx));
    let mut check = on_arr(g, |slot| {
        vec![assert_stmt(eq(aget(slot, index.clone()), int(0)), l)]
    });
    check.push(assign(g.idx, add(iv(g, g.idx), int(1)), l));
    check.push(go(g, SHL_CHECK));
    vec![branch(
        lt(iv(g, g.idx), iv(g, g.limb_shift)),
        check,
        shl_copy_start(g),
        l,
    )]
}

fn ph_shl_copy(g: &G) -> Vec<S> {
    let l = g.l;
    let mut copy = on_arr(g, |slot| {
        vec![aput(
            slot,
            iv(g, g.idx),
            aget(slot, sub(iv(g, g.idx), iv(g, g.limb_shift))),
            l,
        )]
    });
    copy.push(assign(g.idx, sub(iv(g, g.idx), int(1)), l));
    copy.push(go(g, SHL_COPY));
    vec![branch(
        not(lt(iv(g, g.idx), iv(g, g.limb_shift))),
        copy,
        vec![go(g, SHL_ZERO)],
        l,
    )]
}

fn ph_shl_zero(g: &G) -> Vec<S> {
    let l = g.l;
    let mut zero = on_arr(g, |slot| vec![aput(slot, iv(g, g.idx), int(0), l)]);
    zero.push(assign(g.idx, sub(iv(g, g.idx), int(1)), l));
    zero.push(go(g, SHL_ZERO));
    vec![branch(
        not(lt(iv(g, g.idx), int(0))),
        zero,
        {
            let mut arm = bump_selected(g);
            arm.extend([
                assign(g.mul_factor, int(1), l),
                assign(g.idx, int(0), l),
                go(g, SHL_FACTOR),
            ]);
            arm
        },
        l,
    )]
}

fn ph_shl_factor(g: &G) -> Vec<S> {
    let l = g.l;
    vec![branch(
        lt(iv(g, g.idx), iv(g, g.bit_shift)),
        vec![
            assign(g.mul_factor, mul(iv(g, g.mul_factor), int(2)), l),
            assign(g.idx, add(iv(g, g.idx), int(1)), l),
            go(g, SHL_FACTOR),
        ],
        vec![
            assign(g.carry, int(0), l),
            assign(g.idx, int(0), l),
            assign(g.resume_mul, int(SHL_AFTER), l),
            go(g, MUL),
        ],
        l,
    )]
}

fn ph_shl_after(g: &G) -> Vec<S> {
    vec![resume(g, g.resume_shl)]
}

fn mul_limb(g: &G, slot: u16, offset: i64) -> Vec<S> {
    let l = g.l;
    let index = add(iv(g, g.idx), int(offset));
    vec![
        assign(
            g.acc,
            add(
                mul(aget(slot, index.clone()), iv(g, g.mul_factor)),
                iv(g, g.carry),
            ),
            l,
        ),
        aput(slot, index, imod(iv(g, g.acc), int(BASE)), l),
        assign(g.carry, idiv(iv(g, g.acc), int(BASE)), l),
    ]
}

fn ph_mul(g: &G) -> Vec<S> {
    let l = g.l;
    let step = on_arr(g, |slot| {
        let mut arm = Vec::new();
        for offset in 0..2 {
            arm.extend(mul_limb(g, slot, offset));
        }
        arm
    });
    let mut done = vec![assert_stmt(eq(iv(g, g.carry), int(0)), l)];
    done.extend(tighten_mul(g));
    done.push(resume(g, g.resume_mul));
    bounded_pairs(g, MUL, set_mul_sweep(g), step, done)
}

fn add_limb(g: &G, offset: i64) -> Vec<S> {
    let l = g.l;
    let index = add(iv(g, g.idx), int(offset));
    vec![
        assign(g.acc, add(aget(g.num, index.clone()), iv(g, g.carry)), l),
        aput(g.num, index, imod(iv(g, g.acc), int(BASE)), l),
        assign(g.carry, idiv(iv(g, g.acc), int(BASE)), l),
    ]
}

fn ph_add(g: &G) -> Vec<S> {
    let l = g.l;
    let mut step = Vec::new();
    for offset in 0..2 {
        step.extend(add_limb(g, offset));
    }
    let mut done = vec![assert_stmt(eq(iv(g, g.carry), int(0)), l)];
    done.extend(tighten_add(g));
    done.push(resume(g, g.resume_add));
    bounded_pairs(g, ADD, vec![set_add_sweep(g)], step, done)
}

fn copy_limb(g: &G, offset: i64) -> S {
    let l = g.l;
    let index = add(iv(g, g.idx), int(offset));
    branch(
        lt(index.clone(), active_src(g)),
        vec![branch(
            eq(var(g.src_sel), int(0)),
            vec![aput(
                g.scratch,
                index.clone(),
                aget(g.num, index.clone()),
                l,
            )],
            vec![
                assert_stmt(eq(var(g.src_sel), int(1)), l),
                aput(g.scratch, index.clone(), aget(g.den, index.clone()), l),
            ],
            l,
        )],
        vec![aput(g.scratch, index, int(0), l)],
        l,
    )
}

fn ph_copy(g: &G) -> Vec<S> {
    let mut step = Vec::new();
    for offset in 0..2 {
        step.push(copy_limb(g, offset));
    }
    let mut done = bind_scratch_used(g);
    done.push(resume(g, g.resume_copy));
    bounded_pairs(g, COPY, set_copy_sweep(g), step, done)
}

fn cmp_limb(g: &G, offset: i64) -> Vec<S> {
    let l = g.l;
    let index = add(iv(g, g.idx), int(offset));
    vec![
        branch(
            eq(iv(g, g.cmp_mode), int(0)),
            vec![
                assign(g.acc, aget(g.num, index.clone()), l),
                assign(g.carry, aget(g.scratch, index.clone()), l),
            ],
            vec![
                assert_stmt(eq(iv(g, g.cmp_mode), int(1)), l),
                assign(g.acc, aget(g.scratch, index.clone()), l),
                assign(g.carry, aget(g.den, index), l),
            ],
            l,
        ),
        branch(
            not(eq(iv(g, g.acc), iv(g, g.carry))),
            vec![assign(
                g.cmp,
                choose(lt(iv(g, g.acc), iv(g, g.carry)), int(-1), int(1)),
                l,
            )],
            vec![],
            l,
        ),
    ]
}

fn ph_cmp(g: &G) -> Vec<S> {
    let mut step = Vec::new();
    for offset in 0..2 {
        step.extend(cmp_limb(g, offset));
    }
    bounded_pairs(
        g,
        CMP,
        set_cmp_sweep(g),
        step,
        vec![resume(g, g.resume_cmp)],
    )
}

fn sub_limb(g: &G, offset: i64) -> Vec<S> {
    let l = g.l;
    let index = add(iv(g, g.idx), int(offset));
    vec![
        assign(g.hi, aget(g.num, index.clone()), l),
        assign(
            g.acc,
            add(aget(g.scratch, index.clone()), iv(g, g.borrow)),
            l,
        ),
        branch(
            not(lt(iv(g, g.hi), iv(g, g.acc))),
            vec![
                aput(g.num, index.clone(), sub(iv(g, g.hi), iv(g, g.acc)), l),
                assign(g.borrow, int(0), l),
            ],
            vec![
                aput(
                    g.num,
                    index,
                    sub(add(iv(g, g.hi), int(BASE)), iv(g, g.acc)),
                    l,
                ),
                assign(g.borrow, int(1), l),
            ],
            l,
        ),
    ]
}

fn ph_sub(g: &G) -> Vec<S> {
    let l = g.l;
    let mut step = Vec::new();
    for offset in 0..2 {
        step.extend(sub_limb(g, offset));
    }
    let done = vec![
        assert_stmt(eq(iv(g, g.borrow), int(0)), l),
        assign(g.num_used, wider(var(g.num_used), var(g.scratch_used)), l),
        resume(g, g.resume_sub),
    ];
    bounded_pairs(g, SUB, vec![set_sub_sweep(g)], step, done)
}

fn ph_shr(g: &G) -> Vec<S> {
    let l = g.l;
    let mut pair = shr_into(g, iv(g, g.idx), aget(g.scratch, add(iv(g, g.idx), int(1))));
    pair.extend(shr_into(
        g,
        add(iv(g, g.idx), int(1)),
        aget(g.scratch, add(iv(g, g.idx), int(2))),
    ));
    pair.push(assign(g.idx, add(iv(g, g.idx), int(2)), l));
    pair.push(go(g, SHR));
    let mut two = shr_into(g, iv(g, g.idx), aget(g.scratch, add(iv(g, g.idx), int(1))));
    two.extend(shr_into(g, add(iv(g, g.idx), int(1)), int(0)));
    two.extend(finish_shr(g));
    let mut one = shr_into(g, iv(g, g.idx), int(0));
    one.extend(finish_shr(g));
    let body = branch(
        lt(add(iv(g, g.idx), int(2)), var(g.sweep)),
        pair,
        vec![branch(
            lt(add(iv(g, g.idx), int(1)), var(g.sweep)),
            two,
            vec![branch(
                lt(iv(g, g.idx), var(g.sweep)),
                one,
                finish_shr(g),
                l,
            )],
            l,
        )],
        l,
    );
    vec![branch(
        eq(iv(g, g.idx), int(0)),
        vec![assign(g.sweep, var(g.scratch_used), l), body.clone()],
        vec![body],
        l,
    )]
}

fn phase_body(g: &G, id: i64) -> Vec<S> {
    match id {
        SCAN_SETUP => ph_scan_setup(g),
        SCAN_DIGIT => ph_scan_digit(g),
        COUNT_I64 => ph_count_i64(g),
        SCAN_DONE => ph_scan_done(g),
        ADMIT_INIT => ph_admit_init(g),
        ADMIT => ph_admit(g),
        CHUNK_SETUP => ph_chunk_setup(g),
        CHUNK_FACTOR => ph_chunk_factor(g),
        LOAD_ENTER => ph_load_enter(g),
        LOAD_DIGIT => ph_load_digit(g),
        FLUSH_ADD => ph_flush_add(g),
        FLUSH_CLEAR => ph_flush_clear(g),
        LOAD_TAIL => ph_load_tail(g),
        TAIL_FACTOR => ph_tail_factor(g),
        TAIL_ADD => ph_tail_add(g),
        SETI64 => ph_seti64(g),
        DEN_POW => ph_den_pow(g),
        POW5_SETUP => ph_pow5_setup(g),
        POW5_GROW => ph_pow5_grow(g),
        POW5_GROW_DEC => ph_pow5_grow_dec(g),
        PH_POW5_CHUNK => ph_pow5_chunk(g),
        POW5_CHUNK_DEC => ph_pow5_chunk_dec(g),
        ROUND_BL_NUM_DONE => ph_round_bl_num_done(g),
        ROUND_BL_DEN_DONE => ph_round_bl_den_done(g),
        ROUND_ALIGN_HI => ph_round_align_hi(g),
        ROUND_ALIGN_LO => ph_round_align_lo(g),
        ROUND_CMP_ENTER => ph_round_cmp_enter(g),
        ROUND_Q => ph_round_q(g),
        ROUND_SHIFT_DONE => ph_round_shift_done(g),
        ROUND_BIAS => ph_round_bias(g),
        DIV_INIT => ph_div_init(g),
        DIV_STEP => ph_div_step(g),
        DIV_AFTER_CMP => ph_div_after_cmp(g),
        DIV_AFTER_SUB => ph_div_after_sub(g),
        DIV_AFTER_SHR => ph_div_after_shr(g),
        ROUND_REM => ph_round_rem(g),
        ROUND_REM_MUL => ph_round_rem_mul(g),
        ROUND_REM_CMP => ph_round_rem_cmp(g),
        ROUND_TIE => ph_round_tie(g),
        ROUND_MAT => ph_round_mat(g),
        POW2_POS => ph_pow2_pos(g),
        POW2_NEG => ph_pow2_neg(g),
        BL_SCAN => ph_bl_scan(g),
        BL_COUNT => ph_bl_count(g),
        SHL_ENTER => ph_shl_enter(g),
        SHL_CHECK => ph_shl_check(g),
        SHL_COPY => ph_shl_copy(g),
        SHL_ZERO => ph_shl_zero(g),
        SHL_FACTOR => ph_shl_factor(g),
        SHL_AFTER => ph_shl_after(g),
        MUL => ph_mul(g),
        ADD => ph_add(g),
        COPY => ph_copy(g),
        CMP => ph_cmp(g),
        SUB => ph_sub(g),
        SHR => ph_shr(g),
        _ => unreachable!("phase {id}"),
    }
}

fn dispatch(g: &G, arms: &[(i64, Vec<S>)]) -> S {
    fn rec(g: &G, arms: &[(i64, Vec<S>)]) -> S {
        if arms.len() == 1 {
            return branch(
                eq(var(g.snap), int(arms[0].0)),
                arms[0].1.clone(),
                vec![assert_stmt(eq(int(0), int(1)), g.l)],
                g.l,
            );
        }
        let mid = arms.len() / 2;
        branch(
            lt(var(g.snap), int(arms[mid].0)),
            vec![rec(g, &arms[..mid])],
            vec![rec(g, &arms[mid..])],
            g.l,
        )
    }
    rec(g, arms)
}

fn zero_int(slot: u16, l: u32) -> S {
    assign(slot, int(0), l)
}

#[allow(
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    clippy::too_many_arguments
)]
pub(super) fn append(
    lower: &mut super::super::super::lowerer::Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
    index: u16,
    name: &str,
) -> Result<(), ParseError> {
    let carrier = lower.slot(span)?;
    let coeff_arg = lower.slot(span)?;
    let scale_arg = lower.slot(span)?;
    let span_arg = lower.slot(span)?;
    let base_arg = lower.slot(span)?;
    let sig_start_arg = lower.slot(span)?;
    let sig_end_arg = lower.slot(span)?;
    let radix_arg = lower.slot(span)?;
    let identity = lower.helper_int_identity.expect("Int boundary");
    let mut e = super::super::object::Emit::new(lower, span, carrier)?;
    let l = e.line;
    let g = G {
        num: e.temp(
            T::Array(Box::new(T::Int)),
            call(B::ArrayNew.call_index(), Vec::new()),
        )?,
        den: e.temp(
            T::Array(Box::new(T::Int)),
            call(B::ArrayNew.call_index(), Vec::new()),
        )?,
        scratch: e.temp(
            T::Array(Box::new(T::Int)),
            call(B::ArrayNew.call_index(), Vec::new()),
        )?,
        num_used: e.integer()?,
        den_used: e.integer()?,
        scratch_used: e.integer()?,
        sweep: e.integer()?,
        coeff: e.integer()?,
        scale: e.integer()?,
        acc: e.integer()?,
        carry: e.integer()?,
        borrow: e.integer()?,
        step: e.integer()?,
        cmp: e.integer()?,
        hi: e.integer()?,
        a_len: e.integer()?,
        b_len: e.integer()?,
        log_q: e.integer()?,
        q: e.integer()?,
        quot: e.integer()?,
        rounded: e.integer()?,
        exp_out: e.integer()?,
        digits: e.integer()?,
        sci: e.integer()?,
        use_span: e.integer()?,
        base: e.integer()?,
        unit: e.integer()?,
        idx: e.integer()?,
        sticky: e.integer()?,
        dropped: e.integer()?,
        chunk: e.integer()?,
        chunk_n: e.integer()?,
        radix: e.integer()?,
        factor: e.integer()?,
        limb_shift: e.integer()?,
        bit_shift: e.integer()?,
        nonzero: e.integer()?,
        chunk_len: e.integer()?,
        out: e.temp(T::Float, E::Float(0.0))?,
        pow2: e.temp(T::Float, E::Float(1.0))?,
        phase: e.integer()?,
        snap: e.integer()?,
        resume_mul: e.integer()?,
        resume_add: e.integer()?,
        resume_shl: e.integer()?,
        resume_copy: e.integer()?,
        resume_cmp: e.integer()?,
        resume_sub: e.integer()?,
        resume_shr: e.integer()?,
        resume_bl: e.integer()?,
        arr_sel: e.integer()?,
        src_sel: e.integer()?,
        cmp_mode: e.integer()?,
        mul_factor: e.integer()?,
        shl_arg: e.integer()?,
        bl_bits: e.integer()?,
        pow_exp: e.integer()?,
        sig_start: sig_start_arg,
        sig_end: sig_end_arg,
        carrier,
        read: e.read,
        decode: e.decode,
        identity,
        l,
    };
    let mut body = vec![
        assign(g.coeff, p(identity, coeff_arg), l),
        assign(g.scale, p(identity, scale_arg), l),
        assign(g.use_span, p(identity, span_arg), l),
        assign(g.base, p(identity, base_arg), l),
        assign(g.radix, p(identity, radix_arg), l),
        assign(g.out, E::Float(0.0), l),
        assign(g.pow2, E::Float(1.0), l),
        assign(g.phase, int(SCAN_SETUP), l),
        assign(g.num_used, int(1), l),
        assign(g.den_used, int(1), l),
        assign(g.scratch_used, int(1), l),
        assign(g.sweep, int(0), l),
    ];
    for slot in [
        g.acc,
        g.carry,
        g.borrow,
        g.step,
        g.cmp,
        g.hi,
        g.a_len,
        g.b_len,
        g.log_q,
        g.q,
        g.quot,
        g.rounded,
        g.exp_out,
        g.digits,
        g.sci,
        g.unit,
        g.idx,
        g.sticky,
        g.dropped,
        g.chunk,
        g.chunk_n,
        g.factor,
        g.limb_shift,
        g.bit_shift,
        g.nonzero,
        g.chunk_len,
        g.snap,
        g.arr_sel,
        g.src_sel,
        g.cmp_mode,
        g.mul_factor,
        g.shl_arg,
        g.bl_bits,
        g.pow_exp,
    ] {
        body.push(zero_int(slot, l));
    }
    for slot in [
        g.resume_mul,
        g.resume_add,
        g.resume_shl,
        g.resume_copy,
        g.resume_cmp,
        g.resume_sub,
        g.resume_shr,
        g.resume_bl,
    ] {
        body.push(assign(slot, int(-1), l));
    }
    let arms: Vec<(i64, Vec<S>)> = (1..=PHASE_LAST)
        .map(|id| (id, phase_body(&g, id)))
        .collect();
    body.push(wh(
        not(eq(var(g.phase), int(0))),
        vec![assign(g.snap, var(g.phase), l), dispatch(&g, &arms)],
        l,
    ));
    let body = e.finish(body);
    declare(
        ir,
        stmts,
        name,
        index,
        vec![
            carrier,
            coeff_arg,
            scale_arg,
            span_arg,
            base_arg,
            sig_start_arg,
            sig_end_arg,
            radix_arg,
        ],
        vec![
            carrier_schema.clone(),
            T::Int,
            T::Int,
            T::Int,
            T::Int,
            T::Int,
            T::Int,
            T::Int,
        ],
        T::Float,
        body,
        var(g.out),
        l,
    );
    Ok(())
}
