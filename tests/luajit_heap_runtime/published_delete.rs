//! Delete one ordinary named field while a published sweep tail is saved.
//!
//! Not a guest `collectgarbage` step. The saved tail stays Release/Roots.
//! A successful delete removes one live UTF-8 record and does not mark,
//! resume, or close a token.

use vm::Value;

use super::{EdgeKey, EdgeValue, SlotState};
use crate::heap::{
    self, HeapError, HeapLimits, LuaHeap, PublishedBoundary, PublishedResume, ScanMeter, TableRef,
};

fn limits() -> HeapLimits {
    HeapLimits {
        nodes: 16,
        roots: 4,
        edges: 24,
        max_generation: 8,
    }
}

fn publish(host: &mut vm::HostContext<'_>) -> (TableRef, i64) {
    let raw = crate::publish_raw(host);
    (crate::table_ref(host, raw), raw)
}

fn token_open(host: &mut vm::HostContext<'_>, raw: i64) -> bool {
    host.typed_resource_with_key::<heap::LuaTableResource>(
        crate::resource_handle(raw),
        &heap::LuaTableResource::key(),
    )
    .is_ok()
}

fn slot_generation(heap: &LuaHeap, table: TableRef) -> u32 {
    heap.slots[table.slot() as usize].generation
}

struct Ring {
    owner: TableRef,
    owner_token: i64,
    weak: TableRef,
    nodes: [TableRef; 3],
    tokens: [i64; 3],
    carriers: [Value; 3],
    orphan: TableRef,
    orphan_token: i64,
    orphan_generation: u32,
    meta: TableRef,
}

fn build_ring(host: &mut vm::HostContext<'_>) -> Ring {
    heap::install(host, limits()).expect("install");
    let meta = heap::with_heap(host, LuaHeap::alloc).expect("meta");
    let (owner, owner_token) = publish(host);
    let (weak, _) = publish(host);
    let (a, a_token) = publish(host);
    let (b, b_token) = publish(host);
    let (c, c_token) = publish(host);
    let (orphan, orphan_token) = publish(host);
    let (carriers, orphan_generation) = heap::with_heap(host, |heap| {
        heap.set_mode_scalar(meta, Value::string("v"))?;
        heap.set_metatable(weak, Some(meta))?;
        heap.set_named_edge(owner, "entry", a)?;
        heap.set_named_edge(a, "link", b)?;
        heap.set_named_edge(b, "link", c)?;
        heap.set_named_edge(c, "link", a)?;
        heap.set_named_edge(orphan, "link", orphan)?;
        heap.set_named_edge(weak, "o", orphan)?;
        heap.set_named_edge(weak, "a", a)?;
        heap.set_named_edge(weak, "b", b)?;
        heap.set_named_edge(weak, "c", c)?;
        let carriers = [
            heap.issue_carrier(a)?,
            heap.issue_carrier(b)?,
            heap.issue_carrier(c)?,
        ];
        let frame = heap.enter_roots(2)?;
        heap.write_root(frame, 0, owner)?;
        heap.write_root(frame, 1, weak)?;
        Ok((carriers, slot_generation(heap, orphan)))
    })
    .expect("graph");
    Ring {
        owner,
        owner_token,
        weak,
        nodes: [a, b, c],
        tokens: [a_token, b_token, c_token],
        carriers,
        orphan,
        orphan_token,
        orphan_generation,
        meta,
    }
}

fn assert_ring(heap: &LuaHeap, graph: &Ring, rooted: bool) -> Result<(), HeapError> {
    assert_eq!(
        heap.named_edge(graph.nodes[0], "link")?,
        Some(graph.nodes[1])
    );
    assert_eq!(
        heap.named_edge(graph.nodes[1], "link")?,
        Some(graph.nodes[2])
    );
    assert_eq!(
        heap.named_edge(graph.nodes[2], "link")?,
        Some(graph.nodes[0])
    );
    assert_eq!(heap.named_edge(graph.weak, "a")?, Some(graph.nodes[0]));
    assert_eq!(heap.named_edge(graph.weak, "b")?, Some(graph.nodes[1]));
    assert_eq!(
        heap.named_edge(graph.weak, "c")?,
        Some(graph.nodes[2]),
        "the third weak observation keeps the original node"
    );
    assert_eq!(heap.named_edge(graph.owner, "entry")?.is_some(), rooted);
    if rooted {
        assert_eq!(heap.named_edge(graph.owner, "entry")?, Some(graph.nodes[0]));
    }
    Ok(())
}

#[test]
fn published_delete_keeps_the_marked_ring_until_the_next_cycle() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = build_ring(&mut host);
        let resources_before = host.resource_count();
        let _ = heap::take_scan_meter(&mut host).expect("clear meter");
        let published = heap::collect_until_published(&mut host).expect("publish");
        let PublishedBoundary::Published {
            window_heads,
            window_edges,
        } = published
        else {
            panic!("publish must stop before release, got {published:?}");
        };
        assert_eq!(window_heads, 7, "one owner head per slot");
        assert_eq!(window_edges, 10, "mode, entry, four links, four weak edges");
        assert!(
            !token_open(&mut host, graph.orphan_token),
            "orphan token closed"
        );
        assert!(token_open(&mut host, graph.tokens[0]));
        assert!(token_open(&mut host, graph.tokens[1]));
        assert!(token_open(&mut host, graph.tokens[2]));
        assert_eq!(host.resource_count(), resources_before - 1);
        heap::with_heap(&mut host, |heap| {
            assert!(heap.alive(graph.orphan), "orphan slot still physical");
            assert_eq!(slot_generation(heap, graph.orphan), graph.orphan_generation);
            assert!(heap.alive(graph.meta));
            assert_eq!(
                heap.named_edge(graph.weak, "o")?,
                None,
                "weak orphan became a gap"
            );
            let continued = heap
                .next_guest(graph.weak, &Value::string("o"))?
                .expect("gap cursor continues");
            assert_eq!(continued.0, Value::string("a"));
            assert_eq!(continued.1, graph.carriers[0]);
            assert_ring(heap, &graph, true)?;
            Ok(())
        })
        .expect("published graph");
        let _ = heap::take_scan_meter(&mut host).expect("publish meter");
        let before = heap::stats(&mut host).expect("stats before delete");
        assert_eq!(before.collections, 0);
        assert_eq!(before.live_nodes, 7);
        let tail = heap::with_heap(&mut host, |heap| Ok(format!("{:?}", heap.sweep_tail)))
            .expect("saved tail");

        assert_eq!(
            heap::delete_published_named(&mut host, graph.owner, "entry"),
            Ok(true),
            "published tail deletes the last strong edge"
        );
        heap::with_heap(&mut host, |heap| {
            assert_eq!(
                format!("{:?}", heap.sweep_tail),
                tail,
                "tail progress unchanged"
            );
            assert_eq!(heap.named_edge(graph.owner, "entry")?, None);
            assert_ring(heap, &graph, false)?;
            assert_eq!(
                heap.next_guest(graph.weak, &Value::string("o"))?
                    .expect("gap cursor still continues")
                    .1,
                graph.carriers[0]
            );
            Ok(())
        })
        .expect("delete");

        let after = heap::stats(&mut host).expect("stats after delete");
        assert_eq!(
            before.live_edges - 1,
            after.live_edges,
            "only the deleted edge"
        );
        assert_eq!(before.edge_slots - 1, after.edge_slots);
        assert_eq!(before.live_nodes, after.live_nodes);
        assert_eq!(before.live_roots, after.live_roots);
        assert_eq!(before.collections, after.collections);
        assert_eq!(
            heap::take_scan_meter(&mut host).expect("delete meter"),
            ScanMeter::default(),
            "delete is not mark, Owners, or tail progress"
        );
        let paused = heap::resume_published_sweep(&mut host, 0).expect("tail still saved");
        assert!(matches!(paused, PublishedResume::Paused));

        let released = heap::resume_published_sweep(&mut host, 1).expect("release orphan");
        assert!(matches!(released, PublishedResume::Paused));
        heap::with_heap(&mut host, |heap| {
            assert!(
                !heap.alive(graph.orphan),
                "the saved dead slot was released"
            );
            assert_ne!(slot_generation(heap, graph.orphan), graph.orphan_generation);
            for node in graph.nodes {
                assert!(heap.alive(node), "a marked ring node is not in this tail");
                assert_eq!(slot_generation(heap, node), node.generation());
            }
            Ok(())
        })
        .expect("release facts");
        for token in graph.tokens {
            assert!(token_open(&mut host, token));
        }

        let mut reports = 0u32;
        let mut report_nodes = 0u32;
        for _ in 0..8 {
            match heap::resume_published_sweep(&mut host, 1).expect("small budget") {
                PublishedResume::Paused => {}
                PublishedResume::Completed(report) => {
                    reports += 1;
                    report_nodes = report.live_nodes;
                    break;
                }
            }
        }
        assert_eq!(reports, 1, "the tail finishes once");
        assert_eq!(
            report_nodes, 6,
            "orphan released; the ring is still physical"
        );
        assert_eq!(heap::stats(&mut host).expect("finished").collections, 1);
        for token in graph.tokens {
            assert!(
                token_open(&mut host, token),
                "same-cycle completion keeps the token"
            );
        }
        heap::with_heap(&mut host, |heap| {
            for node in graph.nodes {
                assert!(heap.alive(node));
                assert_eq!(slot_generation(heap, node), node.generation());
            }
            assert_ring(heap, &graph, false)?;
            Ok(())
        })
        .expect("retained ring");
        let again = heap::resume_published_sweep(&mut host, 1).expect_err("tail is gone");
        assert_eq!(again, HeapError::SweepDenied);

        let collected = heap::collect(&mut host).expect("next cycle");
        assert_eq!(
            collected.live_nodes, 3,
            "owner, weak observer, and mode table"
        );
        assert!(!token_open(&mut host, graph.tokens[0]));
        assert!(!token_open(&mut host, graph.tokens[1]));
        assert!(!token_open(&mut host, graph.tokens[2]));
        assert!(token_open(&mut host, graph.owner_token));
        assert_eq!(host.resource_count(), resources_before - 4);
        heap::with_heap(&mut host, |heap| {
            for node in graph.nodes {
                assert!(!heap.alive(node), "the next cycle releases the ring");
                assert_ne!(slot_generation(heap, node), node.generation());
            }
            assert_eq!(heap.named_edge(graph.weak, "a")?, None);
            assert_eq!(heap.named_edge(graph.weak, "b")?, None);
            assert_eq!(
                heap.named_edge(graph.weak, "c")?,
                None,
                "three weak edges cleared"
            );
            assert!(heap.alive(graph.owner));
            assert!(heap.alive(graph.weak));
            assert!(heap.alive(graph.meta));
            Ok(())
        })
        .expect("second cycle");
    });
}

/// One resume finishes the tail. The ring stays, and this test does not start
/// the next cycle. That is the normal-completion and omitted-followup arms.
#[test]
fn omitted_followup_after_delete_keeps_the_ring() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = build_ring(&mut host);
        let resources_before = host.resource_count();
        let published = heap::collect_until_published(&mut host).expect("publish");
        assert!(matches!(published, PublishedBoundary::Published { .. }));
        let _ = heap::take_scan_meter(&mut host);
        assert_eq!(
            heap::delete_published_named(&mut host, graph.owner, "entry"),
            Ok(true)
        );
        let resumed = heap::resume_published_sweep(&mut host, 8).expect("one resume");
        let PublishedResume::Completed(report) = resumed else {
            panic!("one budget finishes the saved tail, got {resumed:?}");
        };
        assert_eq!(
            report.live_nodes, 6,
            "one report; the ring is still physical"
        );
        assert_eq!(heap::stats(&mut host).expect("finished").collections, 1);
        assert_eq!(host.resource_count(), resources_before - 1);
        for token in graph.tokens {
            assert!(
                token_open(&mut host, token),
                "no followup, token stays open"
            );
        }
        heap::with_heap(&mut host, |heap| {
            for node in graph.nodes {
                assert!(heap.alive(node));
                assert_eq!(slot_generation(heap, node), node.generation());
            }
            assert_ring(heap, &graph, false)?;
            Ok(())
        })
        .expect("omitted followup");
    });
}

/// The same graph with no post-publish delete. The strong entry keeps the ring
/// through the tail and the next cycle.
#[test]
fn omitted_delete_keeps_the_ring_through_the_next_cycle() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = build_ring(&mut host);
        let resources_before = host.resource_count();
        let published = heap::collect_until_published(&mut host).expect("publish");
        assert!(matches!(published, PublishedBoundary::Published { .. }));
        let resumed = heap::resume_published_sweep(&mut host, 8).expect("finish");
        let PublishedResume::Completed(report) = resumed else {
            panic!("tail finishes, got {resumed:?}");
        };
        assert_eq!(report.live_nodes, 6);
        assert!(token_open(&mut host, graph.owner_token));
        for token in graph.tokens {
            assert!(token_open(&mut host, token), "entry still roots the ring");
        }
        let collected = heap::collect(&mut host).expect("next cycle");
        assert_eq!(
            collected.live_nodes, 6,
            "orphan already released; ring stays"
        );
        assert_eq!(host.resource_count(), resources_before - 1);
        heap::with_heap(&mut host, |heap| {
            assert_eq!(heap.named_edge(graph.owner, "entry")?, Some(graph.nodes[0]));
            assert_ring(heap, &graph, true)?;
            for (name, node) in [("a", 0), ("b", 1), ("c", 2)] {
                assert_eq!(heap.named_edge(graph.weak, name)?, Some(graph.nodes[node]));
            }
            Ok(())
        })
        .expect("kept ring");
    });
}

/// Deleting the strong entry before publish closes the ring at that boundary.
/// The slots are still physical until the tail releases them.
#[test]
fn delete_before_publish_closes_the_ring_at_the_boundary() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = build_ring(&mut host);
        let resources_before = host.resource_count();
        heap::with_heap(&mut host, |heap| {
            heap.set_named_scalar(graph.owner, "entry", Value::Null)?;
            Ok(())
        })
        .expect("ordinary delete");
        let published = heap::collect_until_published(&mut host).expect("publish");
        let PublishedBoundary::Published {
            window_heads,
            window_edges,
        } = published
        else {
            panic!("a weak orphan still publishes, got {published:?}");
        };
        assert_eq!(window_heads, 7);
        assert_eq!(window_edges, 9, "the entry edge is already gone");
        assert!(!token_open(&mut host, graph.orphan_token));
        for token in graph.tokens {
            assert!(
                !token_open(&mut host, token),
                "no strong entry, token closes now"
            );
        }
        assert_eq!(host.resource_count(), resources_before - 4);
        heap::with_heap(&mut host, |heap| {
            assert_eq!(heap.named_edge(graph.owner, "entry")?, None);
            for node in graph.nodes {
                assert!(heap.alive(node), "closed token, physical slot");
                assert_eq!(slot_generation(heap, node), node.generation());
            }
            assert!(heap.alive(graph.orphan));
            for name in ["a", "b", "c", "o"] {
                assert_eq!(heap.named_edge(graph.weak, name)?, None);
            }
            Ok(())
        })
        .expect("boundary");
        let resumed = heap::resume_published_sweep(&mut host, 8).expect("release");
        let PublishedResume::Completed(report) = resumed else {
            panic!("tail finishes, got {resumed:?}");
        };
        assert_eq!(report.live_nodes, 3, "only the rooted tables stay");
        heap::with_heap(&mut host, |heap| {
            for node in graph.nodes {
                assert!(!heap.alive(node));
                assert_ne!(slot_generation(heap, node), node.generation());
            }
            Ok(())
        })
        .expect("released");
    });
}

/// No installed heap, and an installed heap with no saved tail. A present
/// field is not removed. A stale or wrong-kind receiver is not SweepDenied.
#[test]
fn published_delete_rejects_before_a_tail_without_installing() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let ghost = TableRef::table(1, 0, 1);
        assert_eq!(
            heap::delete_published_named(&mut host, ghost, "entry"),
            Err(HeapError::SweepDenied)
        );
        assert_eq!(
            heap::delete_published_named(&mut host, ghost, "entry"),
            Err(HeapError::SweepDenied),
            "the refusal does not install a heap"
        );

        let graph = build_ring(&mut host);
        let stats = heap::stats(&mut host).expect("installed");
        assert_eq!(
            heap::delete_published_named(&mut host, graph.owner, "entry"),
            Err(HeapError::SweepDenied)
        );
        heap::with_heap(&mut host, |heap| {
            assert_eq!(heap.named_edge(graph.owner, "entry")?, Some(graph.nodes[0]));
            Ok(())
        })
        .expect("field remains");
        assert_eq!(heap::stats(&mut host).expect("unchanged"), stats);

        let stale = heap::with_heap(&mut host, |heap| {
            let rolled = heap.alloc()?;
            heap.rollback(rolled)?;
            Ok(rolled)
        })
        .expect("stale");
        assert_eq!(
            heap::delete_published_named(&mut host, stale, "entry"),
            Err(HeapError::StaleHandle)
        );
        let closure = heap::with_heap(&mut host, |heap| heap.alloc_closure()).expect("closure");
        let forged = TableRef::table(closure.domain(), closure.slot(), closure.generation());
        assert_eq!(
            heap::delete_published_named(&mut host, forged, "entry"),
            Err(HeapError::WrongKind)
        );
        let mut other = LuaHeap::new(limits());
        let foreign = other.alloc().expect("foreign");
        assert_eq!(
            heap::delete_published_named(&mut host, foreign, "entry"),
            Err(HeapError::WrongDomain)
        );
        assert!(other.alive(foreign), "the other heap is not touched");
    });
}

/// Every refused shape leaves the saved tail, roots, and edge counts alone.
/// The ordinary setter and `clear_edge` still stop at SweepPending. `next`
/// continues from the original weak gap.
#[test]
fn published_delete_refuses_shapes_without_moving_the_tail() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = build_ring(&mut host);
        heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(2)?;
            let closure = heap.alloc_closure()?;
            let upvalue = heap.alloc_upvalue()?;
            heap.write_closure_root(frame, 0, closure)?;
            heap.write_upvalue_root(frame, 1, upvalue)?;
            heap.write_from(
                graph.owner.slot(),
                EdgeKey::named("hidden"),
                EdgeValue::Closure {
                    slot: closure.slot(),
                    generation: closure.generation(),
                },
            )?;
            heap.write_from(
                graph.owner.slot(),
                EdgeKey::named("box"),
                EdgeValue::Upvalue {
                    slot: upvalue.slot(),
                    generation: upvalue.generation(),
                },
            )?;
            heap.write_from(
                graph.owner.slot(),
                EdgeKey::named("kind"),
                EdgeValue::Table {
                    slot: closure.slot(),
                    generation: closure.generation(),
                },
            )?;
            heap.write_from(
                graph.owner.slot(),
                EdgeKey::named("extra"),
                EdgeValue::Table {
                    slot: graph.nodes[0].slot(),
                    generation: graph.nodes[0].generation(),
                },
            )?;
            Ok(())
        })
        .expect("plant");
        let published = heap::collect_until_published(&mut host).expect("publish");
        assert!(matches!(published, PublishedBoundary::Published { .. }));
        let _ = heap::take_scan_meter(&mut host);
        let before = heap::stats(&mut host).expect("before");
        let tail =
            heap::with_heap(&mut host, |heap| Ok(format!("{:?}", heap.sweep_tail))).expect("tail");

        assert_eq!(
            heap::delete_published_named(&mut host, graph.orphan, "link"),
            Err(HeapError::SweepPending),
            "unmarked live owner"
        );
        assert_eq!(
            heap::delete_published_named(&mut host, graph.meta, "__mode"),
            Err(HeapError::SweepPending)
        );
        assert_eq!(
            heap::delete_published_named(&mut host, graph.owner, "__mode"),
            Err(HeapError::SweepPending),
            "absent mode name is not a missing ordinary field"
        );
        assert_eq!(
            heap::delete_published_named(&mut host, graph.weak, "o"),
            Err(HeapError::SweepPending),
            "cursor gap"
        );
        assert_eq!(
            heap::delete_published_named(&mut host, graph.owner, "hidden"),
            Err(HeapError::SweepPending),
            "internal closure value"
        );
        assert_eq!(
            heap::delete_published_named(&mut host, graph.owner, "box"),
            Err(HeapError::SweepPending),
            "internal upvalue value"
        );
        heap::with_heap(&mut host, |heap| {
            heap.slots[graph.nodes[0].slot() as usize].state = SlotState::Live { marked: false };
            Ok(())
        })
        .expect("poke mark");
        assert_eq!(
            heap::delete_published_named(&mut host, graph.owner, "entry"),
            Err(HeapError::SweepPending),
            "unmarked live table target"
        );
        heap::with_heap(&mut host, |heap| {
            heap.slots[graph.nodes[0].slot() as usize].state = SlotState::Live { marked: true };
            let (_, index) = heap
                .locate_named(graph.owner.slot(), "extra")
                .expect("extra");
            if let EdgeValue::Table { generation, .. } =
                &mut heap.edge_slots[index as usize].edge.as_mut().unwrap().value
            {
                *generation = 0;
            }
            Ok(())
        })
        .expect("poke generation");
        assert_eq!(
            heap::delete_published_named(&mut host, graph.owner, "extra"),
            Err(HeapError::StaleHandle)
        );
        assert_eq!(
            heap::delete_published_named(&mut host, graph.owner, "kind"),
            Err(HeapError::WrongKind)
        );
        heap::with_heap(&mut host, |heap| {
            assert_eq!(
                heap.clear_edge(graph.owner, &EdgeKey::named("entry")),
                Err(HeapError::SweepPending)
            );
            assert_eq!(
                heap.set_named_scalar(graph.owner, "entry", Value::Null),
                Err(HeapError::SweepPending)
            );
            assert_eq!(heap.named_edge(graph.owner, "entry")?, Some(graph.nodes[0]));
            assert!(heap.slots[graph.meta.slot() as usize].mode_edge.is_some());
            assert!(heap.locate_named(graph.owner.slot(), "hidden").is_some());
            assert!(heap.locate_named(graph.owner.slot(), "box").is_some());
            assert!(heap.locate_named(graph.weak.slot(), "o").is_some());
            assert_eq!(
                heap.named_edge(graph.owner, "extra")?
                    .expect("stale target edge remains")
                    .generation(),
                0
            );
            assert!(heap.named_edge(graph.owner, "kind")?.is_some());
            assert_eq!(
                heap.next_guest(graph.weak, &Value::string("o"))?
                    .expect("gap cursor still continues")
                    .1,
                graph.carriers[0]
            );
            assert!(heap.alive(graph.orphan));
            assert_eq!(
                slot_generation(heap, graph.orphan),
                graph.orphan.generation()
            );
            Ok(())
        })
        .expect("zero effect");
        assert_eq!(
            heap::delete_published_named(&mut host, graph.owner, "missing"),
            Ok(false)
        );
        let mut other = LuaHeap::new(limits());
        let foreign = other.alloc().expect("foreign");
        assert_eq!(
            heap::delete_published_named(&mut host, foreign, "entry"),
            Err(HeapError::WrongDomain)
        );
        assert!(other.alive(foreign));

        assert_eq!(heap::stats(&mut host).expect("after"), before);
        assert_eq!(
            heap::take_scan_meter(&mut host).expect("meter"),
            ScanMeter::default()
        );
        heap::with_heap(&mut host, |heap| {
            assert_eq!(format!("{:?}", heap.sweep_tail), tail);
            Ok(())
        })
        .expect("tail unchanged");

        assert!(matches!(
            heap::resume_published_sweep(&mut host, 0).expect("zero"),
            PublishedResume::Paused
        ));
        assert!(matches!(
            heap::resume_published_sweep(&mut host, 1).expect("one release"),
            PublishedResume::Paused
        ));
        assert!(!token_open(&mut host, graph.orphan_token));
        assert_eq!(
            heap::delete_published_named(&mut host, graph.orphan, "link"),
            Err(HeapError::StaleHandle),
            "released handle"
        );
        for token in graph.tokens {
            assert!(token_open(&mut host, token));
        }
        heap::with_heap(&mut host, |heap| {
            assert!(!heap.alive(graph.orphan));
            assert!(heap.alive(graph.nodes[0]));
            assert_eq!(heap.named_edge(graph.owner, "entry")?, Some(graph.nodes[0]));
            Ok(())
        })
        .expect("only the original dead slot moved");
    });
}
