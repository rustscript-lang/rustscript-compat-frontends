//! Static ES5.1 §15.10 pattern compiler and private typed guest entrypoint.
//! No public RegExp object or intrinsic root is installed by this module.
#![allow(dead_code)] // Private integration entrypoint; public consumers follow separately.
use vm::{FrontendIr, ParseError};

pub(super) const PATTERN_LIMIT: usize = 4096;
pub(super) const CAPTURE_LIMIT: usize = 256;
pub(super) const TRANSITION_LIMIT: i64 = 1_000_000;
pub(super) const SCRATCH_LIMIT: i64 = 65536;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PatternError {
    pub part: PatternPart,
    pub offset: usize,
    pub message: &'static str,
}
impl std::fmt::Display for PatternError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "RegExp {:?} UTF-16 offset {}: {}",
            self.part, self.offset, self.message
        )
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PatternPart {
    Pattern,
    Flags,
}
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Flags {
    pub global: bool,
    pub ignore_case: bool,
    pub multiline: bool,
}
#[derive(Clone, Debug)]
pub(super) struct Pattern {
    pub flags: Flags,
    pub captures: usize,
    pub repeats: usize,
    pub code: Vec<[i64; 10]>,
    pub ranges: Vec<i64>,
    pub entry: i64,
}
#[derive(Clone)]
enum Node {
    Empty,
    Set(Vec<(u16, u16)>),
    Anchor(i64, bool),
    Ref(usize),
    Seq(Vec<usize>),
    Alt(Vec<usize>),
    Group(usize, usize),
    Look(bool, usize),
    Repeat {
        child: usize,
        min: i64,
        max: i64,
        greedy: bool,
        first: usize,
        end: usize,
    },
}
struct Frame {
    kind: u8,
    capture: usize,
    offset: usize,
    seq: Vec<usize>,
    alts: Vec<usize>,
}
fn err(offset: usize, message: &'static str) -> PatternError {
    PatternError {
        part: PatternPart::Pattern,
        offset,
        message,
    }
}
fn flag_err(offset: usize, message: &'static str) -> PatternError {
    PatternError {
        part: PatternPart::Flags,
        offset,
        message,
    }
}
fn add(nodes: &mut Vec<Node>, n: Node) -> usize {
    let id = nodes.len();
    nodes.push(n);
    id
}
fn sequence(nodes: &mut Vec<Node>, seq: &mut Vec<usize>) -> usize {
    add(nodes, Node::Seq(std::mem::take(seq)))
}
fn finish(nodes: &mut Vec<Node>, f: &mut Frame) -> usize {
    let s = sequence(nodes, &mut f.seq);
    f.alts.push(s);
    if f.alts.len() == 1 {
        s
    } else {
        add(nodes, Node::Alt(std::mem::take(&mut f.alts)))
    }
}
// Unicode 3.0 default uppercase (not the running host's Unicode/locale).
// Multi-unit uppercase and non-ASCII -> ASCII mappings remain unchanged,
// exactly as ES5.1 §15.10.2.8 Canonicalize requires.
const UNICODE: &str = include_str!("../../tests/fixtures/regexp/unicode-3.0-es5.txt");
fn unicode_tables() -> (Vec<u16>, Vec<bool>, Vec<bool>) {
    let mut fold = (0..=u16::MAX).collect::<Vec<_>>();
    let mut ident = vec![false; 65536];
    let mut spaces = vec![false; 65536];
    for line in UNICODE
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let mut p = line.split_whitespace();
        let kind = p.next().unwrap();
        let a = usize::from_str_radix(p.next().unwrap(), 16).unwrap();
        let b = usize::from_str_radix(p.next().unwrap(), 16).unwrap();
        match kind {
            "U" => fold[a] = b as u16,
            "I" => ident[a..=b].fill(true),
            "S" => spaces[a..=b].fill(true),
            _ => unreachable!(),
        }
    }
    (fold, ident, spaces)
}
fn ranges(bits: &[bool]) -> Vec<(u16, u16)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < 65536 {
        if !bits[i] {
            i += 1;
            continue;
        }
        let a = i;
        while i + 1 < 65536 && bits[i + 1] {
            i += 1
        }
        out.push((a as u16, i as u16));
        i += 1;
    }
    out
}
fn set_of(units: impl IntoIterator<Item = u16>) -> Vec<bool> {
    let mut b = vec![false; 65536];
    for u in units {
        b[usize::from(u)] = true
    }
    b
}
fn predefined(c: u16, spaces: &[bool]) -> Option<Vec<bool>> {
    let lower = if (65..=90).contains(&c) { c + 32 } else { c };
    let mut b = match lower {
        100 => set_of(48..=57),
        119 => set_of((48..=57).chain(65..=90).chain(97..=122).chain([95])),
        115 => spaces.to_vec(),
        _ => return None,
    };
    if c < 97 {
        for x in &mut b {
            *x = !*x
        }
    }
    Some(b)
}
struct Escape {
    bits: Vec<bool>,
    single: Option<u16>,
    reference: Option<usize>,
}
fn escape(
    p: &[u16],
    i: &mut usize,
    in_class: bool,
    total: usize,
    ident: &[bool],
    spaces: &[bool],
) -> Result<Escape, PatternError> {
    let start = *i - 1;
    let c = *p.get(*i).ok_or_else(|| err(start, "trailing escape"))?;
    *i += 1;
    if let Some(bits) = predefined(c, spaces) {
        return Ok(Escape {
            bits,
            single: None,
            reference: None,
        });
    }
    if (49..=57).contains(&c) && !in_class {
        let mut n = usize::from(c - 48);
        while let Some(&d) = p.get(*i) {
            if !(48..=57).contains(&d) {
                break;
            }
            n = n.saturating_mul(10).saturating_add(usize::from(d - 48));
            *i += 1;
        }
        if n > total {
            return Err(err(start, "decimal backreference exceeds capture count"));
        }
        return Ok(Escape {
            bits: Vec::new(),
            single: None,
            reference: Some(n),
        });
    }
    let u = match c {
        48 => {
            if p.get(*i).is_some_and(|d| (48..=57).contains(d)) {
                return Err(err(start, "invalid decimal escape"));
            }
            0
        }
        98 if in_class => 8,
        102 => 12,
        110 => 10,
        114 => 13,
        116 => 9,
        118 => 11,
        99 => {
            let d = *p
                .get(*i)
                .ok_or_else(|| err(start, "incomplete control escape"))?;
            if !(65..=90).contains(&d) && !(97..=122).contains(&d) {
                return Err(err(*i, "control escape needs ASCII letter"));
            }
            *i += 1;
            d % 32
        }
        120 | 117 => {
            let count = if c == 120 { 2 } else { 4 };
            let mut n = 0;
            for _ in 0..count {
                let d = *p
                    .get(*i)
                    .ok_or_else(|| err(start, "incomplete hex escape"))?;
                let v = match d {
                    48..=57 => d - 48,
                    65..=70 => d - 55,
                    97..=102 => d - 87,
                    _ => return Err(err(*i, "invalid hex escape")),
                };
                n = n * 16 + v;
                *i += 1;
            }
            n
        }
        _ => {
            if ident[usize::from(c)] && c != 0x200c && c != 0x200d {
                return Err(err(start, "invalid ES5 identity escape"));
            }
            c
        }
    };
    Ok(Escape {
        bits: set_of([u]),
        single: Some(u),
        reference: None,
    })
}
fn count_captures(p: &[u16]) -> Result<usize, PatternError> {
    let mut i = 0;
    let mut class = false;
    let mut n = 0;
    while i < p.len() {
        match p[i] {
            92 => i += 1,
            91 => class = true,
            93 => class = false,
            40 if !class && p.get(i + 1) != Some(&63) => {
                n += 1;
                if n > CAPTURE_LIMIT {
                    return Err(err(i, "capture quota exceeded"));
                }
            }
            _ => {}
        }
        i += 1;
    }
    Ok(n)
}
fn class(
    p: &[u16],
    i: &mut usize,
    total: usize,
    ident: &[bool],
    spaces: &[bool],
    fold: &[u16],
    ignore: bool,
) -> Result<Vec<(u16, u16)>, PatternError> {
    let start = *i - 1;
    let neg = p.get(*i) == Some(&94);
    if neg {
        *i += 1
    }
    let mut bits = vec![false; 65536];
    while let Some(&c) = p.get(*i) {
        if c == 93 {
            *i += 1;
            let mut canonical = vec![false; 65536];
            for u in 0..65536 {
                if bits[u] {
                    canonical[if ignore { usize::from(fold[u]) } else { u }] = true
                }
            }
            for u in 0..65536 {
                bits[u] = canonical[if ignore { usize::from(fold[u]) } else { u }] ^ neg;
            }
            return Ok(ranges(&bits));
        }
        *i += 1;
        let left = if c == 92 {
            escape(p, i, true, total, ident, spaces)?
        } else {
            Escape {
                bits: set_of([c]),
                single: Some(c),
                reference: None,
            }
        };
        if p.get(*i) == Some(&45) && p.get(*i + 1).is_some_and(|&u| u != 93) {
            *i += 1;
            let r = *p.get(*i).ok_or_else(|| err(start, "unclosed class"))?;
            *i += 1;
            let right = if r == 92 {
                escape(p, i, true, total, ident, spaces)?
            } else {
                Escape {
                    bits: set_of([r]),
                    single: Some(r),
                    reference: None,
                }
            };
            let (Some(a), Some(b)) = (left.single, right.single) else {
                return Err(err(*i - 1, "class range endpoints must be characters"));
            };
            if a > b {
                return Err(err(*i - 1, "descending class range"));
            }
            bits[usize::from(a)..=usize::from(b)].fill(true);
        } else {
            for (a, b) in bits.iter_mut().zip(left.bits) {
                *a |= b
            }
        }
    }
    Err(err(start, "unclosed class"))
}
pub(super) fn compile_pattern(p: &[u16], flags: &str) -> Result<Pattern, PatternError> {
    if p.len() > PATTERN_LIMIT {
        return Err(err(PATTERN_LIMIT, "pattern quota exceeded"));
    }
    let mut f = Flags::default();
    for (i, c) in flags.encode_utf16().enumerate() {
        let flag = match c {
            103 => &mut f.global,
            105 => &mut f.ignore_case,
            109 => &mut f.multiline,
            _ => return Err(flag_err(i, "unsupported ES5 flag")),
        };
        if *flag {
            return Err(flag_err(i, "duplicate flag"));
        }
        *flag = true;
    }
    let total = count_captures(p)?;
    let (fold, ident, spaces) = unicode_tables();
    let mut nodes = Vec::new();
    let mut frames = vec![Frame {
        kind: 0,
        capture: 0,
        offset: 0,
        seq: vec![],
        alts: vec![],
    }];
    let mut captures = 0;
    let mut i = 0;
    let mut quantifiable = false;
    let mut repeats = 0;
    while i < p.len() {
        let at = i;
        let c = p[i];
        i += 1;
        if c == 40 {
            let mut kind = 1;
            let mut cap = 0;
            if p.get(i) == Some(&63) {
                i += 1;
                kind = match p.get(i) {
                    Some(58) => 2,
                    Some(61) => 3,
                    Some(33) => 4,
                    _ => return Err(err(i, "unsupported group syntax")),
                };
                i += 1;
            } else {
                captures += 1;
                cap = captures
            }
            frames.push(Frame {
                kind,
                capture: cap,
                offset: at,
                seq: vec![],
                alts: vec![],
            });
            quantifiable = false;
            continue;
        }
        if c == 41 {
            if frames.len() == 1 {
                return Err(err(at, "unmatched closing parenthesis"));
            }
            let mut frame = frames.pop().unwrap();
            let child = finish(&mut nodes, &mut frame);
            let node = match frame.kind {
                1 => Node::Group(frame.capture, child),
                2 => Node::Seq(vec![child]),
                3 => Node::Look(true, child),
                4 => Node::Look(false, child),
                _ => unreachable!(),
            };
            let id = add(&mut nodes, node);
            frames.last_mut().unwrap().seq.push(id);
            quantifiable = frame.kind < 3;
            continue;
        }
        if c == 124 {
            let frame = frames.last_mut().unwrap();
            let s = sequence(&mut nodes, &mut frame.seq);
            frame.alts.push(s);
            quantifiable = false;
            continue;
        }
        if [42, 43, 63, 123].contains(&c) {
            if !quantifiable {
                return Err(err(at, "quantifier has no quantifiable atom"));
            }
            let (min, max) = match c {
                42 => (0, -1),
                43 => (1, -1),
                63 => (0, 1),
                _ => {
                    // Compare decimal MVs exactly before capping guest counters.
                    // A counter above the transition policy can never be reached.
                    let read_num = |i: &mut usize| -> Result<(i64, Vec<u16>), PatternError> {
                        let start = *i;
                        let mut n = 0i64;
                        while let Some(&d) = p.get(*i) {
                            if !(48..=57).contains(&d) {
                                break;
                            }
                            n = (n * 10 + i64::from(d - 48)).min(TRANSITION_LIMIT + 1);
                            *i += 1;
                        }
                        if *i == start {
                            return Err(err(*i, "expected quantifier decimal"));
                        }
                        let digits = &p[start..*i];
                        let first = digits
                            .iter()
                            .position(|&d| d != 48)
                            .unwrap_or(digits.len() - 1);
                        Ok((n, digits[first..].to_vec()))
                    };
                    let (a, exact_a) = read_num(&mut i)?;
                    let (b, exact_b) = if p.get(i) == Some(&44) {
                        i += 1;
                        if p.get(i) == Some(&125) {
                            (-1, None)
                        } else {
                            let (b, exact) = read_num(&mut i)?;
                            (b, Some(exact))
                        }
                    } else {
                        (a, Some(exact_a.clone()))
                    };
                    if p.get(i) != Some(&125) {
                        return Err(err(i, "unclosed quantifier"));
                    }
                    i += 1;
                    if exact_b.is_some_and(|b| (b.len(), &b) < (exact_a.len(), &exact_a)) {
                        return Err(err(at, "quantifier maximum below minimum"));
                    }
                    (a, b)
                }
            };
            let greedy = p.get(i) != Some(&63);
            if !greedy {
                i += 1
            }
            let seq = &mut frames.last_mut().unwrap().seq;
            let child = seq.pop().unwrap();
            // Node arena is postorder. A finite iterative walk finds the capture
            // interval reset before every repetition (ES5 RepeatMatcher step 4).
            let mut todo = vec![child];
            let mut first = captures + 1;
            let mut end = first;
            while let Some(n) = todo.pop() {
                match &nodes[n] {
                    Node::Group(cap, n) => {
                        first = first.min(*cap);
                        end = end.max(cap + 1);
                        todo.push(*n)
                    }
                    Node::Seq(ns) | Node::Alt(ns) => todo.extend(ns),
                    Node::Look(_, n) | Node::Repeat { child: n, .. } => todo.push(*n),
                    _ => {}
                }
            }
            let id = add(
                &mut nodes,
                Node::Repeat {
                    child,
                    min,
                    max,
                    greedy,
                    first,
                    end,
                },
            );
            seq.push(id);
            repeats += 1;
            quantifiable = false;
            continue;
        }
        let node = match c {
            91 => Node::Set(class(
                p,
                &mut i,
                total,
                &ident,
                &spaces,
                &fold,
                f.ignore_case,
            )?),
            46 => {
                let mut b = vec![true; 65536];
                for u in [10, 13, 0x2028, 0x2029] {
                    b[u] = false
                }
                Node::Set(ranges(&b))
            }
            94 => Node::Anchor(5, false),
            36 => Node::Anchor(6, false),
            92 => {
                if p.get(i) == Some(&98) || p.get(i) == Some(&66) {
                    let neg = p[i] == 66;
                    i += 1;
                    Node::Anchor(7, neg)
                } else {
                    let e = escape(p, &mut i, false, total, &ident, &spaces)?;
                    if let Some(n) = e.reference {
                        Node::Ref(n)
                    } else {
                        let mut b = e.bits;
                        if f.ignore_case {
                            let mut canon = vec![false; 65536];
                            for u in 0..65536 {
                                if b[u] {
                                    canon[usize::from(fold[u])] = true
                                }
                            }
                            for u in 0..65536 {
                                b[u] = canon[usize::from(fold[u])]
                            }
                        }
                        Node::Set(ranges(&b))
                    }
                }
            }
            93 | 125 => return Err(err(at, "unescaped pattern syntax character")),
            _ => {
                let mut b = set_of([c]);
                if f.ignore_case {
                    let canonical = fold[usize::from(c)];
                    for u in 0..65536 {
                        b[u] = fold[u] == canonical
                    }
                }
                Node::Set(ranges(&b))
            }
        };
        quantifiable = !matches!(node, Node::Anchor(..));
        let id = add(&mut nodes, node);
        frames.last_mut().unwrap().seq.push(id);
    }
    if frames.len() != 1 {
        return Err(err(frames.last().unwrap().offset, "unclosed group"));
    }
    let root = finish(&mut nodes, &mut frames.pop().unwrap());
    let mut result = Pattern {
        flags: f,
        captures,
        repeats,
        code: vec![],
        ranges: vec![],
        entry: 0,
    };
    struct Fragment {
        entry: i64,
        exits: Vec<(usize, usize)>,
    }
    fn emit(p: &mut Pattern, op: i64) -> usize {
        let i = p.code.len();
        let mut row = [0; 10];
        row[0] = op;
        row[9] = -1;
        p.code.push(row);
        i
    }
    fn patch(p: &mut Pattern, exits: Vec<(usize, usize)>, next: i64) {
        for (i, j) in exits {
            p.code[i][j] = next
        }
    }
    let mut fragments: Vec<Option<Fragment>> = (0..nodes.len()).map(|_| None).collect();
    let mut repeat = 0;
    for (id, node) in nodes.into_iter().enumerate() {
        let fragment = match node {
            Node::Empty => {
                let i = emit(&mut result, 4);
                Fragment {
                    entry: i as i64,
                    exits: vec![(i, 9)],
                }
            }
            Node::Set(rs) => {
                let i = emit(&mut result, 1);
                result.code[i][1] = result.ranges.len() as i64;
                result.code[i][2] = rs.len() as i64;
                for (a, b) in rs {
                    result.ranges.extend([i64::from(a), i64::from(b)])
                }
                Fragment {
                    entry: i as i64,
                    exits: vec![(i, 9)],
                }
            }
            Node::Anchor(op, neg) => {
                let i = emit(&mut result, op);
                result.code[i][1] = i64::from(neg);
                Fragment {
                    entry: i as i64,
                    exits: vec![(i, 9)],
                }
            }
            Node::Ref(n) => {
                let i = emit(&mut result, 8);
                result.code[i][1] = (n * 2) as i64;
                Fragment {
                    entry: i as i64,
                    exits: vec![(i, 9)],
                }
            }
            Node::Seq(ns) => {
                let mut out: Option<Fragment> = None;
                for n in ns {
                    let child = fragments[n].take().unwrap();
                    if let Some(mut old) = out {
                        patch(&mut result, old.exits, child.entry);
                        old.exits = child.exits;
                        out = Some(old)
                    } else {
                        out = Some(child)
                    }
                }
                out.unwrap_or_else(|| {
                    let i = emit(&mut result, 4);
                    Fragment {
                        entry: i as i64,
                        exits: vec![(i, 9)],
                    }
                })
            }
            Node::Alt(ns) => {
                let mut iter = ns.into_iter().rev();
                let mut out = fragments[iter.next().unwrap()].take().unwrap();
                for n in iter {
                    let left = fragments[n].take().unwrap();
                    let i = emit(&mut result, 3);
                    result.code[i][1] = out.entry;
                    result.code[i][9] = left.entry;
                    out.exits.extend(left.exits);
                    out.entry = i as i64;
                }
                out
            }
            Node::Group(n, child) => {
                let child = fragments[child].take().unwrap();
                let a = emit(&mut result, 2);
                let b = emit(&mut result, 2);
                result.code[a][1] = (2 * n) as i64;
                result.code[a][9] = child.entry;
                result.code[b][1] = (2 * n + 1) as i64;
                patch(&mut result, child.exits, b as i64);
                Fragment {
                    entry: a as i64,
                    exits: vec![(b, 9)],
                }
            }
            Node::Look(positive, child) => {
                let child = fragments[child].take().unwrap();
                let a = emit(&mut result, 9);
                let b = emit(&mut result, 10);
                result.code[a][1] = if positive { 1 } else { 2 };
                result.code[a][9] = child.entry;
                patch(&mut result, child.exits, b as i64);
                Fragment {
                    entry: a as i64,
                    exits: vec![(a, 2)],
                }
            }
            Node::Repeat {
                child,
                min,
                max,
                greedy,
                first,
                end,
            } => {
                let child = fragments[child].take().unwrap();
                let init = emit(&mut result, 11);
                let check = emit(&mut result, 12);
                let body = emit(&mut result, 14);
                let done = emit(&mut result, 13);
                let state = (2 * (captures + 1) + 2 * repeat) as i64;
                repeat += 1;
                result.code[init][1] = state;
                result.code[init][9] = check as i64;
                result.code[check][1] = state;
                result.code[check][2] = min;
                result.code[check][3] = max;
                result.code[check][4] = i64::from(greedy);
                result.code[check][9] = body as i64;
                result.code[body][1] = (2 * first) as i64;
                result.code[body][2] = (2 * end) as i64;
                result.code[body][3] = state;
                result.code[body][9] = child.entry;
                result.code[done][1] = state;
                result.code[done][2] = check as i64;
                result.code[done][3] = min;
                patch(&mut result, child.exits, done as i64);
                Fragment {
                    entry: init as i64,
                    exits: vec![(check, 5)],
                }
            }
        };
        fragments[id] = Some(fragment);
    }
    let root = fragments[root].take().unwrap();
    let accept = emit(&mut result, 0);
    patch(&mut result, root.exits, accept as i64);
    result.entry = root.entry;
    Ok(result)
}
/// Append a finite typed matcher. Signature: (UTF16 Int array, search start,
/// transition budget, scratch cell budget) -> named KernelResult.
/// Status 0=no match, 1=match, 2=transition quota, 3=scratch quota, 4=bad input.
/// `captures` includes group zero; each pair is start/end or -1/-1.
pub(super) fn append_matcher(
    ir: &mut FrontendIr,
    pattern: &[u16],
    flags: &str,
) -> Result<u16, PatternError> {
    let p = compile_pattern(pattern, flags)?;
    super::prelude::regexp::append(ir, &p)
        .map_err(|_: ParseError| err(0, "guest kernel emission failed"))
}
#[cfg(test)]
#[path = "regexp_kernel_tests.rs"]
mod tests;
