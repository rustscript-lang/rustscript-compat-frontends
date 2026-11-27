//! One host call starts its own mark, closes doomed tokens, and observes the
//! published weak graph once before that same sweep releases dead nodes.

use std::cell::Cell;

use super::{HeapError, HeapLimits, LuaHeap, ParkedMark, TableRef};
use vm::Value;

const TABLES: u32 = 32;
const TEXT_TABLES: u32 = 16;
const NUMBER_TABLES: u32 = 8;

struct Counts {
    nodes: u32,
    roots: u32,
    edges: u32,
    rooted: u32,
    weak: u32,
}

fn counts(shared_metatable: bool) -> Counts {
    let object_tables = TABLES - TEXT_TABLES - NUMBER_TABLES;
    let metatables = if shared_metatable { 1 } else { TABLES };
    let object_keys = object_tables * 2;
    Counts {
        nodes: metatables + TABLES * 4 + object_keys,
        roots: metatables + TABLES * 2 + object_keys,
        edges: metatables + TABLES * 3 + TABLES * 2,
        rooted: metatables + TABLES * 2 + object_keys,
        weak: TABLES * 2,
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Text,
    Number,
    Object,
}

struct Owner {
    owner: TableRef,
    owner_raw: i64,
    live: TableRef,
    live_carrier: Value,
    first: TableRef,
    first_carrier: Value,
    second: TableRef,
    kind: Kind,
    keys: Vec<TableRef>,
    key_carriers: Vec<Value>,
}

struct Graph {
    owners: Vec<Owner>,
    weak: Vec<TableRef>,
    weak_raws: Vec<i64>,
    counts: Counts,
}

struct Seen {
    heads: u64,
    edges: u64,
    weak_left: u32,
    strong_kept: u32,
    dead_physical: u32,
    occupying: u32,
    finished: bool,
    release_started: bool,
    judged_dead: bool,
    rawlen_cleared: bool,
    next_kept: bool,
}

fn limits(counts: &Counts) -> HeapLimits {
    HeapLimits {
        nodes: counts.nodes,
        roots: counts.roots,
        edges: counts.edges,
        max_generation: 8,
    }
}

fn alloc_node(host: &mut vm::HostContext<'_>) -> TableRef {
    crate::heap::with_heap(host, LuaHeap::alloc).expect("node")
}

fn publish_node(host: &mut vm::HostContext<'_>) -> (TableRef, i64) {
    let raw = crate::publish_raw(host);
    (crate::table_ref(host, raw), raw)
}

fn build_graph(host: &mut vm::HostContext<'_>, shared_metatable: bool) -> Graph {
    let counts = counts(shared_metatable);
    crate::heap::install(host, limits(&counts)).expect("limits");
    let shared = shared_metatable.then(|| alloc_node(host));
    let mut owners = Vec::new();
    let mut weak = Vec::new();
    let mut weak_raws = Vec::new();
    let mut rooted = Vec::new();
    if let Some(metatable) = shared {
        rooted.push(metatable);
    }
    for index in 0..TABLES {
        let metatable = shared.unwrap_or_else(|| {
            let table = alloc_node(host);
            rooted.push(table);
            table
        });
        let (owner, owner_raw) = publish_node(host);
        let (live, _live_raw) = publish_node(host);
        let (first, first_raw) = publish_node(host);
        let (second, second_raw) = publish_node(host);
        rooted.push(owner);
        rooted.push(live);
        weak.push(first);
        weak.push(second);
        weak_raws.push(first_raw);
        weak_raws.push(second_raw);
        let kind = if index < TEXT_TABLES {
            Kind::Text
        } else if index < TEXT_TABLES + NUMBER_TABLES {
            Kind::Number
        } else {
            Kind::Object
        };
        let mut key_carriers = Vec::new();
        let mut keys = Vec::new();
        if matches!(kind, Kind::Object) {
            for _ in 0..2 {
                let key = alloc_node(host);
                rooted.push(key);
                keys.push(key);
            }
        }
        let carriers = crate::heap::with_heap(host, |heap| {
            heap.set_metatable(owner, Some(metatable))
                .expect("metatable");
            if shared.is_none() || index == 0 {
                heap.set_mode_scalar(metatable, Value::string("v"))
                    .expect("mode");
            }
            let live_carrier = heap.issue_carrier(live).expect("live carrier");
            let first_carrier = heap.issue_carrier(first).expect("first carrier");
            let _second_carrier = heap.issue_carrier(second).expect("second carrier");
            for key in &keys {
                key_carriers.push(heap.issue_carrier(*key).expect("key carrier"));
            }
            match kind {
                Kind::Text => {
                    heap.set_named_edge(owner, "a", first).expect("a");
                    heap.set_named_edge(owner, "stay", live).expect("stay");
                    heap.set_named_edge(owner, "b", second).expect("b");
                }
                Kind::Number => {
                    heap.set_number_edge(owner, 1.0, first).expect("1");
                    heap.set_number_edge(owner, 2.0, second).expect("2");
                    heap.set_named_edge(owner, "stay", live).expect("stay");
                }
                Kind::Object => {
                    heap.set_object_edge(owner, keys[0], first).expect("left");
                    heap.set_named_edge(owner, "stay", live).expect("stay");
                    heap.set_object_edge(owner, keys[1], second).expect("right");
                }
            }
            heap.set_named_edge(first, "self", first)
                .expect("first self");
            heap.set_named_edge(second, "self", second)
                .expect("second self");
            Ok((live_carrier, first_carrier))
        })
        .expect("edges");
        owners.push(Owner {
            owner,
            owner_raw,
            live,
            live_carrier: carriers.0,
            first,
            first_carrier: carriers.1,
            second,
            kind,
            keys,
            key_carriers,
        });
    }
    crate::heap::with_heap(host, |heap| {
        let frame = heap.enter_roots(rooted.len() as u32).expect("roots");
        for (index, table) in rooted.iter().enumerate() {
            heap.write_root(frame, index as u32, *table).expect("root");
        }
        let stats = heap.stats();
        assert_eq!(stats.node_slots, counts.nodes, "node accounting");
        assert_eq!(
            stats.live_nodes, counts.nodes,
            "all nodes live before collect"
        );
        assert_eq!(stats.live_roots, counts.roots, "root accounting");
        assert_eq!(stats.edge_slots, counts.edges, "edge accounting");
        assert_eq!(weak.len() as u32, counts.weak, "weak node accounting");
        Ok(())
    })
    .expect("root");
    Graph {
        owners,
        weak,
        weak_raws,
        counts,
    }
}

fn token_open(host: &vm::HostContext<'_>, raw: i64) -> bool {
    host.typed_resource_with_key::<crate::heap::LuaTableResource>(
        crate::resource_handle(raw),
        &crate::heap::LuaTableResource::key(),
    )
    .is_ok()
}

fn same_table(found: Option<TableRef>, expected: TableRef) -> bool {
    found.is_some_and(|table| table == expected)
}

fn read_published(view: &crate::heap::PublishedRead<'_>, graph: &Graph) -> Seen {
    assert!(!view.finished(), "observation is not a finished sweep");
    assert!(
        !view.release_started(),
        "release has not started at the published boundary"
    );
    assert_eq!(view.window_heads(), u64::from(graph.counts.nodes));
    assert_eq!(view.window_edges(), u64::from(graph.counts.edges));
    assert_eq!(view.occupying_dead(), graph.counts.weak);
    let stale = TableRef::table(
        graph.owners[0].owner.domain(),
        graph.owners[0].owner.slot(),
        graph.owners[0].owner.generation().wrapping_add(1),
    );
    match view.named(stale, "stay") {
        Err(crate::heap::ObserveFault::Heap(error)) => {
            assert_eq!(
                error,
                HeapError::StaleHandle,
                "a stale handle is not a slot index"
            );
        }
        other => panic!("stale handle must be a heap refusal, got {other:?}"),
    }
    assert_eq!(
        view.dead_still_physical(),
        graph.counts.weak,
        "dead slots are still physical before release"
    );
    let mut weak_left = 0;
    let mut strong_kept = 0;
    let mut rawlen_cleared = true;
    let mut next_kept = true;
    let mut judged_dead = true;
    for owner in &graph.owners {
        let stay = view
            .named(owner.owner, "stay")
            .expect("stay lookup")
            .expect("strong value stays");
        assert_eq!(stay, owner.live);
        strong_kept += 1;
        match owner.kind {
            Kind::Text => {
                let left = view.named(owner.owner, "a").expect("text left");
                let right = view.named(owner.owner, "b").expect("text right");
                weak_left += u32::from(same_table(left, owner.first));
                weak_left += u32::from(same_table(right, owner.second));
                let continued = view
                    .next(owner.owner, &Value::string("a"))
                    .expect("text next");
                let Some((key, value)) = continued else {
                    next_kept = false;
                    continue;
                };
                if key != Value::string("stay") || value != owner.live_carrier {
                    next_kept = false;
                }
                let dead_cursor = view.next(owner.owner, &owner.first_carrier);
                judged_dead &= matches!(dead_cursor, Err(crate::heap::ObserveFault::JudgedDead));
            }
            Kind::Number => {
                let first = view.number(owner.owner, 1.0).expect("number 1");
                let second = view.number(owner.owner, 2.0).expect("number 2");
                weak_left += u32::from(first != Value::Null);
                weak_left += u32::from(second != Value::Null);
                let length = view.raw_sequence_len(owner.owner).expect("rawlen");
                rawlen_cleared &= length == 0.0;
            }
            Kind::Object => {
                let left = view
                    .object(owner.owner, owner.keys[0])
                    .expect("object left");
                let right = view
                    .object(owner.owner, owner.keys[1])
                    .expect("object right");
                weak_left += u32::from(same_table(left, owner.first));
                weak_left += u32::from(same_table(right, owner.second));
                let continued = view
                    .next(owner.owner, &owner.key_carriers[0])
                    .expect("object next");
                let Some((key, value)) = continued else {
                    next_kept = false;
                    continue;
                };
                if key != Value::string("stay") || value != owner.live_carrier {
                    next_kept = false;
                }
            }
        }
    }
    for table in &graph.weak {
        assert!(
            view.still_physical(*table),
            "doomed table is physical at the published boundary"
        );
        let alive = view.alive(*table);
        judged_dead &= matches!(alive, Err(crate::heap::ObserveFault::JudgedDead));
    }
    Seen {
        heads: view.window_heads(),
        edges: view.window_edges(),
        weak_left,
        strong_kept,
        dead_physical: view.dead_still_physical(),
        occupying: view.occupying_dead(),
        finished: view.finished(),
        release_started: view.release_started(),
        judged_dead,
        rawlen_cleared,
        next_kept,
    }
}

fn assert_cycle(host: &mut vm::HostContext<'_>, graph: &Graph, seen: &Seen) {
    assert_eq!(seen.weak_left, 0, "no partial owner kept a weak value");
    assert_eq!(seen.strong_kept, TABLES, "strong values stayed");
    assert!(seen.judged_dead, "dead handles are rejected before release");
    assert!(seen.rawlen_cleared, "weak number sequence was cleared");
    assert!(seen.next_kept, "next continued past cleared weak entries");
    assert!(!seen.finished);
    assert!(!seen.release_started);
    assert_eq!(seen.dead_physical, graph.counts.weak);
    assert_eq!(seen.occupying, graph.counts.weak);
    let report_nodes =
        crate::heap::with_heap(host, |heap| Ok(heap.stats().live_nodes)).expect("stats");
    assert_eq!(
        report_nodes, graph.counts.rooted,
        "release removed the weak nodes after observation"
    );
    for table in &graph.weak {
        assert!(!crate::heap::with_heap(host, |heap| Ok(heap.alive(*table))).expect("alive"));
    }
    for raw in &graph.weak_raws {
        assert!(!token_open(host, *raw), "doomed token stays closed");
    }
    assert!(
        token_open(host, graph.owners[0].owner_raw),
        "rooted token stays open"
    );
}

fn run_shared(shared_metatable: bool) {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = build_graph(&mut host, shared_metatable);
        let before_resources = host.resource_count();
        let events = Cell::new(0u32);
        let (report, seen) = crate::heap::collect_observing(&mut host, |view| {
            events.set(events.get() + 1);
            read_published(view, &graph)
        })
        .expect("private cycle returns one report");
        assert_eq!(
            report.live_nodes, graph.counts.rooted,
            "collect finished; observation is the missing boundary"
        );
        assert_eq!(
            events.get(),
            1,
            "published observation missing after collect"
        );
        let seen = seen.expect("published observation missing");
        assert_eq!(events.get(), 1, "observer ran more than once");
        assert_cycle(&mut host, &graph, &seen);
        let meter =
            crate::heap::with_heap(&mut host, |heap| Ok(heap.take_scan_meter())).expect("meter");
        assert_eq!(meter.mark_passes, 1, "one fresh mark");
        assert_eq!(meter.token_closes, u64::from(graph.counts.weak));
        assert_eq!(
            meter.sweep_edge_checks, seen.edges,
            "observation added no sweep edges"
        );
        assert_eq!(
            meter.owner_head_checks,
            seen.heads + u64::from(report.marked_nodes),
            "mark heads and the atomic owner walk are separate"
        );
        assert_eq!(
            meter.scratch.pending_close_pushed,
            u64::from(graph.counts.weak)
        );
        assert_eq!(
            meter.scratch.dead_slots_pushed,
            u64::from(graph.counts.weak)
        );
        assert_eq!(
            meter.scratch.sweep_bitmap_inited,
            u64::from(graph.counts.nodes)
        );
        assert_eq!(crate::heap::stats(&mut host).expect("stats").collections, 1);
        assert_eq!(
            host.resource_count(),
            before_resources - graph.counts.weak as usize,
            "closed weak quota was returned"
        );
        assert_eq!(report.marked_nodes, graph.counts.rooted);
        assert_eq!(report.live_roots, graph.counts.roots);
    });
}

#[test]
fn shared_metatable_cycle_observes_published_weak_graph_once() {
    run_shared(true);
}

#[test]
fn separate_metatables_cycle_observes_published_weak_graph_once() {
    run_shared(false);
}

#[test]
fn private_cycle_without_dead_slots_skips_publish() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        crate::heap::install(
            &mut host,
            HeapLimits {
                nodes: 4,
                roots: 1,
                edges: 2,
                max_generation: 8,
            },
        )
        .expect("limits");
        let (table, raw) = publish_node(&mut host);
        crate::heap::with_heap(&mut host, |heap| {
            heap.set_named_edge(table, "self", table).expect("self");
            let frame = heap.enter_roots(1).expect("frame");
            heap.write_root(frame, 0, table).expect("root");
            Ok(())
        })
        .expect("root");
        let events = Cell::new(0u32);
        let (report, seen) = crate::heap::collect_observing(&mut host, |_view| {
            events.set(events.get() + 1);
        })
        .expect("no-dead cycle");
        assert!(seen.is_none(), "no published event without dead slots");
        assert_eq!(events.get(), 0);
        assert_eq!(report.live_nodes, 1);
        let meter =
            crate::heap::with_heap(&mut host, |heap| Ok(heap.take_scan_meter())).expect("meter");
        assert_eq!(
            meter.sweep_edge_checks, 0,
            "roots phase does not walk edges"
        );
        assert_eq!(meter.token_closes, 0);
        assert_eq!(meter.scratch.dead_slots_pushed, 0);
        assert_eq!(crate::heap::stats(&mut host).expect("stats").collections, 1);
        crate::heap::with_heap(&mut host, |heap| {
            assert!(heap.alive(table));
            Ok(())
        })
        .expect("survivor");
        assert!(token_open(&host, raw), "rooted token stays open");
    });
}

#[test]
fn foreign_token_failure_publishes_nothing_and_retries_from_roots() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        crate::heap::install(&mut host, crate::sweep_limits()).expect("limits");
        let unrelated = host
            .push_resource(crate::UnrelatedHostResource)
            .expect("unrelated");
        let unrelated_raw = unrelated.handle().raw();
        let keeper = crate::publish_raw(&mut host);
        let keeper_table = crate::table_ref(&host, keeper);
        let prefix = crate::publish_raw(&mut host);
        let other = crate::publish_raw(&mut host);
        let doomed = crate::heap::with_heap(&mut host, LuaHeap::alloc).expect("doomed");
        let ahead = crate::heap::with_heap(&mut host, LuaHeap::alloc).expect("ahead");
        let suffix = crate::publish_raw(&mut host);
        let metatable = alloc_node(&mut host);
        let watch = alloc_node(&mut host);
        let weak_value = alloc_node(&mut host);
        crate::heap::with_heap(&mut host, |heap| {
            heap.bind_token(doomed, unrelated_raw)
                .expect("foreign token");
            heap.set_named_edge(doomed, "y", ahead).expect("edge");
            heap.set_mode_scalar(metatable, Value::string("v"))
                .expect("mode");
            heap.set_metatable(watch, Some(metatable))
                .expect("metatable");
            heap.set_named_edge(watch, "gone", weak_value)
                .expect("weak edge");
            let frame = heap.enter_roots(3).expect("frame");
            heap.write_root(frame, 0, keeper_table).expect("keeper");
            heap.write_root(frame, 1, metatable).expect("meta root");
            heap.write_root(frame, 2, watch).expect("watch root");
            Ok(())
        })
        .expect("weak graph");
        let before_resources = host.resource_count();
        let nodes_before = crate::heap::stats(&mut host).expect("stats").node_slots;
        let events = Cell::new(0u32);
        let error = crate::heap::collect_observing(&mut host, |_view| {
            events.set(events.get() + 1);
        })
        .expect_err("foreign token blocks the cycle");
        crate::assert_type_mismatch(&error);
        assert_eq!(events.get(), 0, "no published event before close succeeds");
        let stats = crate::heap::stats(&mut host).expect("stats");
        assert_eq!(
            stats.collections, 0,
            "close failure is not a completed collect"
        );
        assert_eq!(stats.node_slots, nodes_before, "no release");
        let meter =
            crate::heap::with_heap(&mut host, |heap| Ok(heap.take_scan_meter())).expect("meter");
        assert_eq!(meter.sweep_edge_checks, 0, "no sweep before close succeeds");
        assert_eq!(
            meter.token_closes, 3,
            "prefix closes, then the foreign token stops"
        );
        assert_eq!(meter.scratch.dead_slots_pushed, 0, "select has not run");
        assert_eq!(meter.scratch.sweep_bitmap_inited, 0, "snapshot has not run");
        crate::heap::with_heap(&mut host, |heap| {
            assert_eq!(
                heap.named_edge(watch, "gone").expect("weak edge"),
                Some(weak_value),
                "weak graph is unchanged"
            );
            assert!(heap.alive(weak_value));
            assert!(heap.alive(doomed));
            Ok(())
        })
        .expect("graph");
        assert!(
            host.resource(&unrelated).is_ok(),
            "foreign resource stays open"
        );
        assert!(token_open(&host, suffix), "suffix stays open");
        assert!(!token_open(&host, prefix), "prefix stays closed");
        assert!(!token_open(&host, other), "other stays closed");
        let resources_after_failure = host.resource_count();
        assert_eq!(resources_after_failure, before_resources - 2);

        let retry = Cell::new(0u32);
        let retry_error = crate::heap::collect_observing(&mut host, |_view| {
            retry.set(retry.get() + 1);
        })
        .expect_err("obstacle still blocks a fresh mark");
        crate::assert_type_mismatch(&retry_error);
        assert_eq!(retry.get(), 0);
        crate::heap::with_heap(&mut host, |heap| {
            assert_eq!(
                heap.named_edge(watch, "gone").expect("edge"),
                Some(weak_value)
            );
            Ok(())
        })
        .expect("still weak");

        crate::heap::with_heap(&mut host, |heap| heap.unbind_token(doomed))
            .expect("fixture drops the foreign token binding");
        let events = Cell::new(0u32);
        let (report, seen) = crate::heap::collect_observing(&mut host, |_view| {
            events.set(events.get() + 1);
            true
        })
        .expect("fresh mark after the obstacle is gone");
        assert_eq!(events.get(), 1, "recovered cycle publishes once");
        assert_eq!(seen, Some(true));
        crate::heap::with_heap(&mut host, |heap| {
            assert!(heap.named_edge(watch, "gone").expect("cleared").is_none());
            assert!(
                !heap.alive(weak_value),
                "fresh mark collected the weak value"
            );
            assert!(heap.alive(watch));
            assert!(heap.alive(keeper_table));
            assert!(!heap.alive(doomed));
            Ok(())
        })
        .expect("recovered graph");
        assert!(
            !token_open(&host, suffix),
            "suffix closes on the recovered cycle"
        );
        assert_eq!(host.resource_count(), resources_after_failure - 1);
        assert!(report.live_nodes >= 1);
        let _ = ahead;
    });
}

#[test]
fn parked_mark_refuses_private_cycle_and_resumes() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        crate::heap::install(&mut host, crate::sweep_limits()).expect("limits");
        let anchor = alloc_node(&mut host);
        let target = alloc_node(&mut host);
        let ahead = alloc_node(&mut host);
        let extra = alloc_node(&mut host);
        const KEY_Y: u32 = 5;
        crate::heap::with_heap(&mut host, |heap| {
            heap.set_edge(target, KEY_Y, ahead).expect("edge");
            let frame = heap.enter_roots(2).expect("frame");
            heap.write_root(frame, 0, anchor).expect("anchor");
            heap.write_root(frame, 1, target).expect("target");
            Ok(())
        })
        .expect("graph");
        let saved = crate::heap::with_heap(&mut host, |heap| {
            let mut parked = ParkedMark::start(heap).expect("start");
            let cursor = parked.field_index(target, KEY_Y).expect("edge index");
            let mut saved = None;
            for _ in 0..16 {
                let view = parked.pulse(1);
                if view.finished {
                    break;
                }
                if parked
                    .pending()
                    .is_some_and(|scan| scan.owner == target.slot() && scan.cursor == cursor)
                {
                    saved = Some((target.slot(), cursor));
                    break;
                }
            }
            let saved = saved.expect("parked on the outgoing edge");
            assert_eq!(parked.phase_name(), "gray");
            assert!(parked.marked(target));
            parked.park().expect("park");
            Ok(saved)
        })
        .expect("park");
        let nodes_before = crate::heap::stats(&mut host).expect("stats").node_slots;
        let error = crate::heap::collect_observing(&mut host, |_view| ())
            .expect_err("parked mark must be refused");
        assert!(
            matches!(error, HeapError::MarkActive),
            "parked mark refused, got {error:?}"
        );
        assert_eq!(crate::heap::stats(&mut host).expect("stats").collections, 0);
        assert_eq!(
            crate::heap::stats(&mut host).expect("stats").node_slots,
            nodes_before
        );
        crate::heap::with_heap(&mut host, |heap| {
            let parked = ParkedMark::resume(heap).expect("original mark still parked");
            assert_eq!(parked.phase_name(), "gray");
            assert_eq!(
                parked.pending().map(|scan| (scan.owner, scan.cursor)),
                Some(saved),
                "pending cursor is unchanged"
            );
            assert!(parked.marked(target));
            assert_eq!(
                parked.meter().mark_passes,
                1,
                "refusal did not start another mark"
            );
            assert_eq!(parked.meter().sweep_edge_checks, 0);
            parked.park().expect("park again");
            Ok(())
        })
        .expect("resume");
        crate::heap::with_heap(&mut host, |heap| {
            let mut parked = ParkedMark::resume(heap).expect("resume to finish");
            for _ in 0..16 {
                if parked.pulse(1).finished {
                    break;
                }
            }
            let _report = parked.complete().expect("original sweep");
            drop(parked);
            assert!(heap.alive(anchor));
            assert!(heap.alive(target));
            assert!(heap.alive(ahead));
            assert!(
                !heap.alive(extra),
                "the original mark still sweeps unmarked nodes"
            );
            Ok(())
        })
        .expect("original driver");
    });
}
