//! Private weak-publish boundaries.
//!
//! The unit walker can rewrite one edge and leave the next edge of the same
//! table. That partial graph is recorded outside this file. These tests only
//! read the observation entry, which returns the original graph or the fully
//! published graph.

use super::super::{HeapLimits, LuaHeap, ScanMeter};
use super::{ObserveError, SweepSession, WeakPublish};
use vm::Value;

const TABLES: u32 = 32;
const TEXT_TABLES: u32 = 16;
const NUMBER_TABLES: u32 = 8;
const MIXED_OWNERS: u32 = 4;

fn limits(nodes: u32, roots: u32, edges: u32) -> HeapLimits {
    HeapLimits {
        nodes,
        roots,
        edges,
        max_generation: 8,
    }
}

fn root_all(heap: &mut LuaHeap, tables: &[super::super::TableRef]) {
    let frame = heap.enter_roots(tables.len() as u32).expect("frame");
    for (index, table) in tables.iter().copied().enumerate() {
        heap.write_root(frame, index as u32, table).expect("root");
    }
}

struct Graph {
    heap: LuaHeap,
    owners: Vec<Owner>,
    nodes: u32,
    edges: u32,
    weak: Vec<super::super::TableRef>,
}

struct Owner {
    owner: super::super::TableRef,
    live: super::super::TableRef,
    live_carrier: Value,
    kind: Kind,
}

enum Kind {
    Text {
        first: super::super::TableRef,
        second: super::super::TableRef,
        first_carrier: Value,
    },
    Number {
        first_carrier: Value,
    },
    Object {
        keys: [super::super::TableRef; 2],
        key_carriers: [Value; 2],
        first_carrier: Value,
    },
}

fn build(shared_metatable: bool) -> Graph {
    let object_tables = TABLES - TEXT_TABLES - NUMBER_TABLES;
    let metatables = if shared_metatable { 1 } else { TABLES };
    let object_keys = object_tables * 2;
    let nodes = metatables + TABLES * 4 + object_keys;
    let roots = metatables + TABLES * 2 + object_keys;
    let edges = metatables + TABLES * 3 + TABLES * 2;
    let mut heap = LuaHeap::new(limits(nodes, roots, edges));
    let mut rooted = Vec::new();
    let shared = if shared_metatable {
        let table = heap.alloc().expect("metatable");
        heap.set_mode_scalar(table, Value::string("v"))
            .expect("mode");
        rooted.push(table);
        Some(table)
    } else {
        None
    };
    let mut owners = Vec::new();
    let mut weak = Vec::new();
    for index in 0..TABLES {
        let metatable = if let Some(shared) = shared {
            shared
        } else {
            let table = heap.alloc().expect("metatable");
            heap.set_mode_scalar(table, Value::string("v"))
                .expect("mode");
            rooted.push(table);
            table
        };
        let owner = heap.alloc().expect("owner");
        let live = heap.alloc().expect("live");
        let first = heap.alloc().expect("first weak");
        let second = heap.alloc().expect("second weak");
        let live_carrier = heap.issue_carrier(live).expect("live carrier");
        let first_carrier = heap.issue_carrier(first).expect("first carrier");
        let _second_carrier = heap.issue_carrier(second).expect("second carrier");
        heap.set_metatable(owner, Some(metatable))
            .expect("metatable");
        heap.set_named_edge(first, "self", first).expect("cycle");
        heap.set_named_edge(second, "self", second).expect("cycle");
        let kind = if index < TEXT_TABLES {
            heap.set_named_edge(owner, "a", first).expect("a");
            heap.set_named_edge(owner, "stay", live).expect("stay");
            heap.set_named_edge(owner, "b", second).expect("b");
            Kind::Text {
                first,
                second,
                first_carrier,
            }
        } else if index < TEXT_TABLES + NUMBER_TABLES {
            heap.set_number_edge(owner, 1.0, first).expect("one");
            heap.set_number_edge(owner, 2.0, second).expect("two");
            heap.set_named_edge(owner, "stay", live).expect("stay");
            Kind::Number { first_carrier }
        } else {
            let left = heap.alloc().expect("left key");
            let right = heap.alloc().expect("right key");
            let left_carrier = heap.issue_carrier(left).expect("left carrier");
            let right_carrier = heap.issue_carrier(right).expect("right carrier");
            heap.set_object_edge(owner, left, first).expect("left");
            heap.set_named_edge(owner, "stay", live).expect("stay");
            heap.set_object_edge(owner, right, second).expect("right");
            rooted.push(left);
            rooted.push(right);
            Kind::Object {
                keys: [left, right],
                key_carriers: [left_carrier, right_carrier],
                first_carrier,
            }
        };
        owners.push(Owner {
            owner,
            live,
            live_carrier,
            kind,
        });
        rooted.push(owner);
        rooted.push(live);
        weak.push(first);
        weak.push(second);
    }
    assert_eq!(weak.len(), 64);
    root_all(&mut heap, &rooted);
    let stats = heap.stats();
    assert_eq!(stats.node_slots, nodes, "node accounting");
    assert_eq!(stats.edge_slots, edges, "edge accounting");
    assert_eq!(rooted.len() as u32, roots, "root accounting");
    Graph {
        heap,
        owners,
        nodes,
        edges,
        weak,
    }
}

fn visible_weak(view: &super::WeakView<'_>, owners: &[Owner]) -> u32 {
    owners.iter().map(|owner| weak_present(view, owner)).sum()
}

fn weak_present(view: &super::WeakView<'_>, owner: &Owner) -> u32 {
    match &owner.kind {
        Kind::Text { first, second, .. } => {
            u32::from(view.named(owner.owner, "a").expect("a") == Some(*first))
                + u32::from(view.named(owner.owner, "b").expect("b") == Some(*second))
        }
        Kind::Number { first_carrier } => {
            let one = view.number(owner.owner, 1.0).expect("one");
            let two = view.number(owner.owner, 2.0).expect("two");
            u32::from(one == *first_carrier) + u32::from(two != Value::Null)
        }
        Kind::Object {
            keys,
            first_carrier,
            ..
        } => {
            let left = view.object(owner.owner, keys[0]).expect("left");
            let right = view.object(owner.owner, keys[1]).expect("right");
            u32::from(left.is_some() && view_matches_carrier(left, first_carrier))
                + u32::from(right.is_some())
        }
    }
}

fn view_matches_carrier(found: Option<super::super::TableRef>, carrier: &Value) -> bool {
    let Some(found) = found else {
        return false;
    };
    matches!(carrier, Value::Array(_)) && found.slot() == array_slot(carrier)
}

fn array_slot(carrier: &Value) -> u32 {
    match carrier {
        Value::Array(values) => match values.first() {
            Some(Value::Int(slot)) => u32::try_from(*slot).expect("slot"),
            _ => panic!("carrier has no slot"),
        },
        _ => panic!("expected carrier"),
    }
}

fn assert_boundary(view: &super::WeakView<'_>, owners: &[Owner], present: bool) {
    let expected = if present { 64 } else { 0 };
    assert_eq!(visible_weak(view, owners), expected);
    for owner in owners {
        let found = weak_present(view, owner);
        assert_eq!(found, if present { 2 } else { 0 }, "no partial owner");
        assert_eq!(
            view.named(owner.owner, "stay").expect("stay"),
            Some(owner.live),
            "live identity"
        );
        match &owner.kind {
            Kind::Text { first_carrier, .. } => {
                let continued = view.next(owner.owner, &Value::string("a")).expect("next a");
                let (key, value) = continued.expect("continuation pair");
                assert_eq!(key, Value::string("stay"));
                assert_eq!(value, owner.live_carrier);
                assert!(
                    view.next(owner.owner, &Value::string("b"))
                        .expect("last cursor")
                        .is_none(),
                    "last cursor tail is one nil"
                );
                if !present {
                    assert!(
                        view.next(owner.owner, &Value::string("stay"))
                            .expect("published tail")
                            .is_none(),
                        "survivor tail is one nil after publish"
                    );
                }
                assert!(matches!(
                    view.next(owner.owner, first_carrier),
                    Err(ObserveError::JudgedDead)
                ));
            }
            Kind::Number { .. } => {
                let len = view.raw_sequence_len(owner.owner).expect("rawlen");
                assert_eq!(len, if present { 2.0 } else { 0.0 });
            }
            Kind::Object {
                keys, key_carriers, ..
            } => {
                let continued = view
                    .next(owner.owner, &key_carriers[0])
                    .expect("object next");
                let (key, value) = continued.expect("object continuation pair");
                assert_eq!(key, Value::string("stay"));
                assert_eq!(value, owner.live_carrier);
                assert!(
                    view.next(owner.owner, &key_carriers[1])
                        .expect("object last cursor")
                        .is_none(),
                    "object last cursor tail is one nil"
                );
                if !present {
                    assert!(
                        view.next(owner.owner, &Value::string("stay"))
                            .expect("object published tail")
                            .is_none(),
                        "object survivor tail is one nil after publish"
                    );
                }
                assert!(view.alive(keys[0]).expect("key"));
                assert!(view.alive(keys[1]).expect("key"));
                assert_ne!(keys[0], owner.live);
                assert_ne!(keys[1], owner.live);
            }
        }
    }
}

fn reject_judged_dead(view: &super::WeakView<'_>, dead: super::super::TableRef) {
    assert!(matches!(
        view.named(dead, "self"),
        Err(ObserveError::JudgedDead)
    ));
    assert!(matches!(view.alive(dead), Err(ObserveError::JudgedDead)));
    assert!(matches!(
        view.raw_sequence_len(dead),
        Err(ObserveError::JudgedDead)
    ));
}

fn finish_release(session: &mut SweepSession<'_>) {
    for _ in 0..10_000 {
        if session.pulse(1).finished {
            return;
        }
    }
    panic!("release did not finish");
}

fn sweep_delta(before: &ScanMeter, after: &ScanMeter) -> (u64, u64, u64, u64) {
    (
        after.owner_head_checks - before.owner_head_checks,
        after.sweep_edge_checks - before.sweep_edge_checks,
        after.weak_mode_lookups - before.weak_mode_lookups,
        after.weak_mode_bytes - before.weak_mode_bytes,
    )
}

fn assert_paired(shared_metatable: bool) {
    let mut graph = build(shared_metatable);
    let mut twin_graph = build(shared_metatable);
    let dead = match &graph.owners[0].kind {
        Kind::Text { first, .. } => *first,
        _ => panic!("first owner is a text table"),
    };
    let modes = graph.heap.drain_mark();
    let twin_modes = twin_graph.heap.drain_mark();
    let mut session = SweepSession::start(&mut graph.heap, modes);
    let mut twin = SweepSession::start(&mut twin_graph.heap, twin_modes);
    let before = session.meter();
    let twin_before = twin.meter();
    assert_eq!(before, twin_before, "mark meters diverged before sweep");

    assert!(session.publish_owners().is_none(), "prep is not a window");
    let inner = session.pulse(1);
    assert_eq!(inner.spent.total(), 1, "pulse(1) is one unit");
    assert_eq!(inner.spent.selection, 1);
    assert_ne!(inner.spent.total(), u64::from(graph.nodes + graph.edges));
    let rest = session.pulse(u64::from(graph.nodes) - 1);
    assert_eq!(rest.spent.selection, u64::from(graph.nodes) - 1);
    let snapshot = session.pulse(u64::from(graph.nodes));
    assert_eq!(snapshot.spent.snapshot, u64::from(graph.nodes));
    assert_eq!(snapshot.spent.heads, 0);
    assert_eq!(snapshot.spent.edges, 0);

    let untouched = session.pulse(0);
    assert_eq!(untouched.spent.total(), 0);
    assert!(!untouched.finished);
    {
        let original = session.weak_view().expect("original");
        assert_eq!(original.publish(), WeakPublish::Original);
        assert!(!original.finished());
        assert!(!original.release_started());
        assert_boundary(&original, &graph.owners, true);
        reject_judged_dead(&original, dead);
    }

    let (window_heads, window_edges) = {
        let window = session.publish_owners().expect("published");
        assert_eq!(window.view.publish(), WeakPublish::Published);
        assert!(!window.view.finished());
        assert!(!window.view.release_started());
        assert_eq!(window.spent.heads, u64::from(graph.nodes));
        assert_eq!(window.spent.edges, u64::from(graph.edges));
        assert_eq!(window.spent.selection, 0);
        assert_eq!(window.spent.snapshot, 0);
        assert_eq!(window.spent.releases, 0);
        assert_eq!(window.spent.roots, 0);
        assert_ne!(window.spent.total(), 1, "window is not a pulse unit");
        assert!(window.spent.total() > inner.spent.total());
        assert_boundary(&window.view, &graph.owners, false);
        assert_eq!(window.view.occupying_dead(), 64);
        assert_eq!(window.view.dead_still_physical(), 64);
        reject_judged_dead(&window.view, dead);
        (window.spent.heads, window.spent.edges)
    };

    let held = session.pulse(0);
    assert_eq!(held.spent.total(), 0);
    assert!(!held.finished);
    {
        let published = session.weak_view().expect("still published");
        assert_eq!(published.publish(), WeakPublish::Published);
        assert!(!published.finished());
        assert!(!published.release_started());
        assert_eq!(visible_weak(&published, &graph.owners), 0);
        assert_eq!(published.dead_still_physical(), 64);
    }
    assert_eq!(session.stats().collections, 0);

    let first = graph
        .weak
        .iter()
        .copied()
        .min_by_key(|table| table.slot())
        .expect("dead");
    let release = session.pulse(1);
    assert_eq!(release.spent.total(), 1);
    assert_eq!(release.spent.releases, 1);
    assert!(!release.finished);
    assert!(
        session.weak_view().is_none(),
        "release is not a publish boundary"
    );
    assert_eq!(
        session.slot_physical(first.slot()),
        Some((false, first.generation().saturating_add(1)))
    );
    let mut still_physical = 0u32;
    for table in &graph.weak {
        if table.slot() == first.slot() {
            continue;
        }
        assert_eq!(
            session.slot_physical(table.slot()),
            Some((true, table.generation()))
        );
        still_physical += 1;
    }
    assert_eq!(still_physical, 63);
    finish_release(&mut session);
    let report = session.report();
    let after = session.meter();
    let stats = session.stats();
    let physical: Vec<_> = (0..graph.nodes)
        .map(|slot| session.slot_physical(slot))
        .collect();
    drop(session);

    twin.drain();
    let twin_report = twin.report();
    assert_eq!(report, twin_report);
    assert_eq!(after, twin.meter());
    assert_eq!(stats.collections, twin.stats().collections);
    assert_eq!(stats.collections, 1);
    assert_eq!(
        sweep_delta(&before, &after),
        (window_heads, window_edges, 0, 0)
    );
    assert_eq!(
        sweep_delta(&twin_before, &twin.meter()),
        sweep_delta(&before, &after),
        "paired sweep walked once"
    );
    for slot in 0..graph.nodes {
        assert_eq!(physical[slot as usize], twin.slot_physical(slot));
    }
    drop(twin);
    assert_guest_pair(&graph, &twin_graph);
}

fn assert_guest_pair(left: &Graph, right: &Graph) {
    assert_eq!(left.owners.len(), right.owners.len());
    for (left_owner, right_owner) in left.owners.iter().zip(&right.owners) {
        assert_eq!(
            left.heap.alive(left_owner.live),
            right.heap.alive(right_owner.live)
        );
        assert!(left.heap.alive(left_owner.live));
        assert_eq!(
            left.heap
                .named_edge(left_owner.owner, "stay")
                .expect("stay")
                .map(|table| table.slot()),
            right
                .heap
                .named_edge(right_owner.owner, "stay")
                .expect("stay")
                .map(|table| table.slot())
        );
        match (&left_owner.kind, &right_owner.kind) {
            (Kind::Text { .. }, Kind::Text { .. }) => {
                assert!(
                    left.heap
                        .named_edge(left_owner.owner, "a")
                        .expect("a")
                        .is_none()
                );
                assert!(
                    right
                        .heap
                        .named_edge(right_owner.owner, "a")
                        .expect("a")
                        .is_none()
                );
            }
            (Kind::Number { .. }, Kind::Number { .. }) => {
                assert_eq!(
                    left.heap
                        .raw_sequence_len(left_owner.owner)
                        .expect("rawlen"),
                    right
                        .heap
                        .raw_sequence_len(right_owner.owner)
                        .expect("rawlen")
                );
                assert_eq!(
                    left.heap
                        .raw_sequence_len(left_owner.owner)
                        .expect("rawlen"),
                    0.0
                );
            }
            (
                Kind::Object {
                    keys, key_carriers, ..
                },
                Kind::Object {
                    keys: twin_keys,
                    key_carriers: twin_carriers,
                    ..
                },
            ) => {
                assert!(left.heap.alive(keys[0]));
                assert!(right.heap.alive(twin_keys[0]));
                assert_ne!(keys[0].slot(), left_owner.live.slot());
                let left_next = left
                    .heap
                    .next_guest(left_owner.owner, &key_carriers[0])
                    .expect("next");
                let right_next = right
                    .heap
                    .next_guest(right_owner.owner, &twin_carriers[0])
                    .expect("next");
                assert!(left_next.is_some());
                assert_eq!(left_next.is_some(), right_next.is_some());
                assert!(
                    left.heap
                        .object_edge(left_owner.owner, keys[0])
                        .expect("cleared")
                        .is_none()
                );
            }
            _ => panic!("paired graphs diverged"),
        }
    }
}

#[test]
fn shared_metatable_publishes_every_weak_value_at_one_boundary() {
    assert_paired(true);
}

#[test]
fn separate_metatables_publish_every_weak_value_at_one_boundary() {
    assert_paired(false);
}

#[test]
fn marked_graph_does_not_open_a_publish_window() {
    let mut heap = LuaHeap::new(limits(1, 1, 1));
    let table = heap.alloc().expect("table");
    heap.set_named_edge(table, "stay", table).expect("edge");
    root_all(&mut heap, &[table]);
    let modes = heap.drain_mark();
    let mut session = SweepSession::start(&mut heap, modes);
    let zero = session.pulse(0);
    assert_eq!(zero.spent.total(), 0);
    assert!(session.publish_owners().is_none());
    assert_ne!(
        session.weak_view().map(|view| view.publish()),
        Some(WeakPublish::Published)
    );
    loop {
        assert!(session.publish_owners().is_none());
        assert_ne!(
            session.weak_view().map(|view| view.publish()),
            Some(WeakPublish::Published)
        );
        if session.pulse(1).finished {
            break;
        }
    }
    assert_eq!(session.meter().sweep_edge_checks, 0);
    let _ = session.report();
}

/// One sweep, owners interleaved as `k`, `v`, `kv`, `kv`.
///
/// The two `kv` rows are a dead object key with a rooted value, and a rooted
/// object key with a dead value. Each owner also keeps a separate strong key
/// and value. Dead carriers exist so `next` can name those objects; they are
/// not written into root slots, and a judged-dead handle is never a lookup
/// key or a `next` cursor. Group counts stay separate. The owners window
/// reports the heads and edges that walk visited. `pulse(1)` is one unit and
/// does not describe that window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Group {
    Keys,
    Values,
    KvDeadKey,
    KvDeadValue,
}

impl Group {
    fn mode(self) -> &'static str {
        match self {
            Group::Keys => "k",
            Group::Values => "v",
            Group::KvDeadKey | Group::KvDeadValue => "kv",
        }
    }

    fn index(self) -> usize {
        match self {
            Group::Keys => 0,
            Group::Values => 1,
            Group::KvDeadKey => 2,
            Group::KvDeadValue => 3,
        }
    }

    fn metatable_slot(self) -> usize {
        match self {
            Group::Keys => 0,
            Group::Values => 1,
            Group::KvDeadKey | Group::KvDeadValue => 2,
        }
    }
}

enum WeakEdge {
    DeadKey {
        carrier: Value,
    },
    NamedDead {
        value: super::super::TableRef,
        carrier: Value,
    },
    RootedKeyDeadValue {
        key: super::super::TableRef,
        key_carrier: Value,
        value: super::super::TableRef,
        value_carrier: Value,
    },
}

struct MixedOwner {
    owner: super::super::TableRef,
    keep_key: super::super::TableRef,
    keep_key_carrier: Value,
    keep_value: super::super::TableRef,
    keep_value_carrier: Value,
    group: Group,
    weak: WeakEdge,
}

struct Mixed {
    heap: LuaHeap,
    owners: Vec<MixedOwner>,
    dead: Vec<super::super::TableRef>,
    nodes: u32,
    edges: u32,
}

fn build_mixed(shared_metatable: bool) -> Mixed {
    let owners_n = MIXED_OWNERS * 4;
    let metatables = if shared_metatable { 3 } else { owners_n };
    let nodes = metatables + owners_n * 4 + MIXED_OWNERS;
    let roots = metatables + owners_n * 3 + MIXED_OWNERS;
    let edges = metatables + owners_n * 3;
    let mut heap = LuaHeap::new(limits(nodes, roots, edges));
    let mut rooted = Vec::new();
    let mut shared = [None, None, None];
    let mut owners = Vec::new();
    let mut dead = Vec::new();
    let order = [
        Group::Keys,
        Group::Values,
        Group::KvDeadKey,
        Group::KvDeadValue,
    ];
    for _ in 0..MIXED_OWNERS {
        for group in order {
            let metatable = if let Some(existing) = shared[group.metatable_slot()]
                && shared_metatable
            {
                existing
            } else {
                let table = heap.alloc().expect("metatable");
                heap.set_mode_scalar(table, Value::string(group.mode()))
                    .expect("mode");
                rooted.push(table);
                if shared_metatable {
                    shared[group.metatable_slot()] = Some(table);
                }
                table
            };
            let owner = heap.alloc().expect("owner");
            let keep_key = heap.alloc().expect("keep key");
            let keep_value = heap.alloc().expect("keep value");
            let dead_table = heap.alloc().expect("dead");
            let keep_key_carrier = heap.issue_carrier(keep_key).expect("keep key carrier");
            let keep_value_carrier = heap.issue_carrier(keep_value).expect("keep value carrier");
            let dead_carrier = heap.issue_carrier(dead_table).expect("dead carrier");
            assert_ne!(keep_key, dead_table);
            assert_ne!(keep_value, dead_table);
            heap.set_metatable(owner, Some(metatable))
                .expect("metatable");
            heap.set_object_edge(owner, keep_key, keep_value)
                .expect("keep");
            heap.set_named_edge(dead_table, "self", dead_table)
                .expect("cycle");
            let weak = match group {
                Group::Keys | Group::KvDeadKey => {
                    heap.set_object_edge(owner, dead_table, keep_value)
                        .expect("dead key");
                    WeakEdge::DeadKey {
                        carrier: dead_carrier,
                    }
                }
                Group::Values => {
                    heap.set_named_edge(owner, "orphan", dead_table)
                        .expect("orphan");
                    WeakEdge::NamedDead {
                        value: dead_table,
                        carrier: dead_carrier,
                    }
                }
                Group::KvDeadValue => {
                    let held = heap.alloc().expect("held key");
                    let held_carrier = heap.issue_carrier(held).expect("held carrier");
                    assert_ne!(held, dead_table);
                    assert_ne!(held, keep_key);
                    heap.set_object_edge(owner, held, dead_table)
                        .expect("dead value");
                    rooted.push(held);
                    WeakEdge::RootedKeyDeadValue {
                        key: held,
                        key_carrier: held_carrier,
                        value: dead_table,
                        value_carrier: dead_carrier,
                    }
                }
            };
            rooted.push(owner);
            rooted.push(keep_key);
            rooted.push(keep_value);
            dead.push(dead_table);
            owners.push(MixedOwner {
                owner,
                keep_key,
                keep_key_carrier,
                keep_value,
                keep_value_carrier,
                group,
                weak,
            });
        }
    }
    assert_eq!(owners.len() as u32, owners_n);
    assert_eq!(dead.len() as u32, owners_n);
    root_all(&mut heap, &rooted);
    let stats = heap.stats();
    assert_eq!(stats.node_slots, nodes, "node accounting");
    assert_eq!(stats.edge_slots, edges, "edge accounting");
    assert_eq!(rooted.len() as u32, roots, "root accounting");
    Mixed {
        heap,
        owners,
        dead,
        nodes,
        edges,
    }
}

fn finish_snapshot(session: &mut SweepSession<'_>, nodes: u32) {
    let one = session.pulse(1);
    assert_eq!(one.spent.total(), 1, "one pulse is one unit");
    assert_eq!(one.spent.selection, 1);
    let rest = session.pulse(u64::from(nodes) - 1);
    assert_eq!(rest.spent.selection, u64::from(nodes) - 1);
    let snapshot = session.pulse(u64::from(nodes));
    assert_eq!(snapshot.spent.snapshot, u64::from(nodes));
    assert_eq!(snapshot.spent.heads, 0);
    assert_eq!(snapshot.spent.edges, 0);
    let held = session.pulse(0);
    assert_eq!(held.spent.total(), 0);
    assert!(!held.finished);
}

fn assert_keep(view: &super::WeakView<'_>, owner: &MixedOwner) {
    assert_eq!(
        view.object(owner.owner, owner.keep_key).expect("keep"),
        Some(owner.keep_value),
        "kept key and value"
    );
    assert!(view.alive(owner.keep_key).expect("keep key"));
    assert!(view.alive(owner.keep_value).expect("keep value"));
    let started = view.next(owner.owner, &Value::Null).expect("keep next");
    let (key, value) = started.expect("keep pair");
    assert_eq!(key, owner.keep_key_carrier);
    assert_eq!(value, owner.keep_value_carrier);
}

fn weak_visible(view: &super::WeakView<'_>, owner: &MixedOwner) -> bool {
    match &owner.weak {
        WeakEdge::DeadKey { carrier } => {
            let found = view
                .next(owner.owner, &owner.keep_key_carrier)
                .expect("dead key next");
            match found {
                Some((key, value)) => {
                    assert_eq!(key, *carrier);
                    assert_eq!(value, owner.keep_value_carrier);
                    true
                }
                None => false,
            }
        }
        WeakEdge::NamedDead { value, carrier } => {
            let named = view.named(owner.owner, "orphan").expect("orphan");
            let found = view
                .next(owner.owner, &owner.keep_key_carrier)
                .expect("orphan next");
            match (named, found) {
                (Some(found_value), Some((key, got))) => {
                    assert_eq!(found_value, *value);
                    assert_eq!(key, Value::string("orphan"));
                    assert_eq!(got, *carrier);
                    true
                }
                (None, None) => false,
                _ => panic!("orphan lookup and next diverged"),
            }
        }
        WeakEdge::RootedKeyDeadValue {
            key,
            key_carrier,
            value,
            value_carrier,
        } => {
            assert!(view.alive(*key).expect("rooted key"));
            let got = view.object(owner.owner, *key).expect("rooted key lookup");
            let found = view
                .next(owner.owner, &owner.keep_key_carrier)
                .expect("dead value next");
            match (got, found) {
                (Some(found_value), Some((next_key, next_value))) => {
                    assert_eq!(found_value, *value);
                    assert_eq!(next_key, *key_carrier);
                    assert_eq!(next_value, *value_carrier);
                    true
                }
                (None, None) => false,
                _ => panic!("object lookup and next diverged"),
            }
        }
    }
}

fn group_counts(view: &super::WeakView<'_>, owners: &[MixedOwner]) -> [u32; 4] {
    let mut counts = [0u32; 4];
    for owner in owners {
        assert_keep(view, owner);
        if weak_visible(view, owner) {
            counts[owner.group.index()] += 1;
        }
    }
    counts
}

fn reject_dead(view: &super::WeakView<'_>, dead: &[super::super::TableRef]) {
    for table in dead {
        assert!(matches!(view.alive(*table), Err(ObserveError::JudgedDead)));
        assert!(matches!(
            view.named(*table, "self"),
            Err(ObserveError::JudgedDead)
        ));
    }
}

fn publish_mixed(shared_metatable: bool) {
    let mut graph = build_mixed(shared_metatable);
    let modes = graph.heap.drain_mark();
    let mut session = SweepSession::start(&mut graph.heap, modes);
    let before = session.meter();
    finish_snapshot(&mut session, graph.nodes);
    {
        let original = session.weak_view().expect("original");
        assert_eq!(original.publish(), WeakPublish::Original);
        assert!(!original.finished());
        assert!(!original.release_started());
        assert_eq!(group_counts(&original, &graph.owners), [MIXED_OWNERS; 4]);
        reject_dead(&original, &graph.dead);
    }

    let (window_heads, window_edges) = {
        let window = session.publish_owners().expect("published");
        assert_eq!(window.view.publish(), WeakPublish::Published);
        assert!(!window.view.finished());
        assert!(!window.view.release_started());
        assert_eq!(window.spent.heads, u64::from(graph.nodes));
        assert_eq!(window.spent.edges, u64::from(graph.edges));
        assert_eq!(window.spent.selection, 0);
        assert_eq!(window.spent.snapshot, 0);
        assert_eq!(window.spent.releases, 0);
        assert_eq!(window.spent.roots, 0);
        assert_eq!(
            window.spent.heads + window.spent.edges,
            window.spent.total()
        );
        assert_ne!(window.spent.total(), 1, "owners window is not one pulse");
        assert_eq!(group_counts(&window.view, &graph.owners), [0; 4]);
        assert_eq!(window.view.occupying_dead(), graph.dead.len() as u32);
        assert_eq!(window.view.dead_still_physical(), graph.dead.len() as u32);
        reject_dead(&window.view, &graph.dead);
        (window.spent.heads, window.spent.edges)
    };

    let held = session.pulse(0);
    assert_eq!(held.spent.total(), 0);
    assert!(!held.finished);
    {
        let published = session.weak_view().expect("still published");
        assert_eq!(published.publish(), WeakPublish::Published);
        assert!(!published.release_started());
        assert_eq!(group_counts(&published, &graph.owners), [0; 4]);
        assert_eq!(published.dead_still_physical(), graph.dead.len() as u32);
    }
    assert_eq!(session.stats().collections, 0);

    let first = graph
        .dead
        .iter()
        .copied()
        .min_by_key(|table| table.slot())
        .expect("dead");
    let release = session.pulse(1);
    assert_eq!(release.spent.total(), 1);
    assert_eq!(release.spent.releases, 1);
    assert!(!release.finished);
    assert!(
        session.weak_view().is_none(),
        "release is not a publish boundary"
    );
    assert_eq!(
        session.slot_physical(first.slot()),
        Some((false, first.generation().saturating_add(1)))
    );
    let mut still_physical = 0u32;
    for table in &graph.dead {
        if table.slot() == first.slot() {
            continue;
        }
        assert_eq!(
            session.slot_physical(table.slot()),
            Some((true, table.generation()))
        );
        still_physical += 1;
    }
    assert_eq!(still_physical, graph.dead.len() as u32 - 1);
    finish_release(&mut session);
    let after = session.meter();
    assert_eq!(
        sweep_delta(&before, &after),
        (window_heads, window_edges, 0, 0)
    );
    assert_eq!(session.stats().collections, 1);
    let _ = session.report();
}

#[test]
fn shared_modes_publish_interleaved_weak_groups_at_one_boundary() {
    publish_mixed(true);
}

#[test]
fn separate_modes_publish_interleaved_weak_groups_at_one_boundary() {
    publish_mixed(false);
}

#[test]
fn mixed_weak_observer_counts_one_deleted_entry() {
    let mut graph = build_mixed(true);
    let deleted = graph
        .owners
        .iter()
        .find(|owner| owner.group == Group::Values)
        .expect("value owner")
        .owner;
    graph
        .heap
        .set_named_scalar(deleted, "orphan", Value::Null)
        .expect("delete one entry");
    let modes = graph.heap.drain_mark();
    let mut session = SweepSession::start(&mut graph.heap, modes);
    finish_snapshot(&mut session, graph.nodes);
    let original = session.weak_view().expect("original");
    assert_eq!(original.publish(), WeakPublish::Original);
    assert_eq!(
        group_counts(&original, &graph.owners),
        [MIXED_OWNERS, MIXED_OWNERS - 1, MIXED_OWNERS, MIXED_OWNERS]
    );
    let mut hidden = 0u32;
    for owner in &graph.owners {
        if owner.group != Group::Values {
            continue;
        }
        let visible = weak_visible(&original, owner);
        if owner.owner == deleted {
            assert!(!visible);
            hidden += 1;
        } else {
            assert!(visible);
        }
    }
    assert_eq!(hidden, 1);
}
