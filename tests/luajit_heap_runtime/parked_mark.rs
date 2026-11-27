//! Parked mark on a private heap. The driver keeps the only `&mut LuaHeap`.
//!
//! A pulse of one stops on a real pending record, not on a guessed step count.
//! The bound below is only a harness limit. Sweep stays outside the pulse.

use super::mark::{OpenTrace, clear_open_trace, open_trace};
use super::{
    ClosureRef, HeapError, HeapLimits, HeapStats, LuaHeap, ParkedMark, PausedOp, PendingScan,
    RootFrameId, ScanMeter, TableRef, UpvalueRef, UserdataRef,
};
use crate::heap;
use vm::Value;

const KEY_X: u32 = 1;
/// Ordinary field. Not the private cell field stored by `set_cell_edge`.
const KEY_Y: u32 = 5;
const KEY_Z: u32 = 3;
const KEY_W: u32 = 1;
const HARNESS_BOUND: u32 = 64;

fn limits(nodes: u32, roots: u32, edges: u32) -> HeapLimits {
    HeapLimits {
        nodes,
        roots,
        edges,
        max_generation: 8,
    }
}

struct Chain {
    a: TableRef,
    x: TableRef,
    y: TableRef,
    z: TableRef,
    d: TableRef,
    w: TableRef,
}

fn chain(heap: &mut LuaHeap) -> Chain {
    let a = heap.alloc().expect("A");
    let x = heap.alloc().expect("X");
    let y = heap.alloc().expect("Y");
    let z = heap.alloc().expect("Z");
    let d = heap.alloc().expect("D");
    let w = heap.alloc().expect("W");
    let frame = heap.enter_roots(1).expect("frame");
    heap.write_root(frame, 0, a).expect("root A");
    heap.set_edge(a, KEY_X, x).expect("A.X");
    heap.set_edge(a, KEY_Y, y).expect("A.Y");
    heap.set_edge(a, KEY_Z, z).expect("A.Z");
    Chain { a, x, y, z, d, w }
}

fn pause_at(parked: &mut ParkedMark<'_>, owner: u32, cursor: u32) -> PendingScan {
    for _ in 0..HARNESS_BOUND {
        let view = parked.pulse(1);
        assert!(
            !view.finished,
            "mark finished before owner {owner} record {cursor} was pending"
        );
        assert_eq!(view.meter.sweep_edge_checks, 0, "a pulse must not sweep");
        assert_eq!(
            view.meter.mark_passes, 1,
            "pause stays in the original pass"
        );
        if let Some(pending) = parked.pending()
            && pending.owner == owner
            && pending.cursor == cursor
        {
            assert_eq!(parked.phase_name(), "gray");
            assert!(parked.mutation_open());
            return pending;
        }
    }
    panic!("pending record {cursor} on owner {owner} was not observed");
}

fn finish(parked: &mut ParkedMark<'_>) -> ScanMeter {
    for _ in 0..HARNESS_BOUND {
        let view = parked.pulse(1);
        assert_eq!(view.meter.sweep_edge_checks, 0, "a pulse must not sweep");
        assert_eq!(view.meter.mark_passes, 1);
        if view.finished {
            return view.meter;
        }
    }
    panic!("mark did not finish within the harness bound");
}

fn string_bytes(value: &Value) -> Option<&[u8]> {
    match value {
        Value::String(text) => Some(text.as_bytes()),
        _ => None,
    }
}

struct Picture {
    stats: HeapStats,
    meter: ScanMeter,
    bytes: u64,
}

fn picture(parked: &ParkedMark<'_>) -> Picture {
    Picture {
        stats: parked.stats(),
        meter: parked.meter(),
        bytes: parked.logical_bytes(),
    }
}

fn assert_same(before: &Picture, after: &Picture) {
    assert_eq!(before.stats, after.stats, "refusal changed occupancy");
    assert_eq!(before.meter, after.meter, "refusal changed the scan meter");
    assert_eq!(before.bytes, after.bytes, "refusal changed payload");
}

#[test]
fn same_slot_reuse_keeps_the_owner_suffix() {
    let mut heap = LuaHeap::new(limits(8, 2, 8));
    let chain = chain(&mut heap);
    let x_slot = chain.x.slot();
    let x_generation = chain.x.generation();
    let z_slot = chain.z.slot();
    let z_generation = chain.z.generation();
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    let y_index = parked.field_index(chain.a, KEY_Y).expect("Y record");
    let z_index = parked.field_index(chain.a, KEY_Z).expect("Z record");
    let pending = pause_at(&mut parked, chain.a.slot(), y_index);
    assert_eq!(pending.owner, chain.a.slot());
    assert_eq!(pending.cursor, y_index);
    assert!(!parked.marked(chain.z), "Z is still ahead of the cursor");
    assert!(!parked.marked(chain.w), "W has no root");
    assert!(
        parked.write_allowed(chain.d),
        "D is live, unmarked, and the phase accepts a field write"
    );
    assert_ne!(chain.d.slot(), chain.a.slot());

    let before = parked.meter();
    parked.delete_field(chain.a, KEY_Y).expect("delete Y");
    parked
        .write_field(chain.d, KEY_W, chain.w)
        .expect("reuse the freed record");
    assert_eq!(
        parked.field_index(chain.d, KEY_W),
        Some(y_index),
        "D must reuse Y's record, not a new slot"
    );
    assert_eq!(parked.field_index(chain.a, KEY_Y), None);
    assert_eq!(parked.edge(chain.a, KEY_Y).expect("Y edge"), None);
    let after = parked.meter();
    assert_eq!(after.outgoing_edge_checks, before.outgoing_edge_checks);
    assert_eq!(after.parked_edge_writes, before.parked_edge_writes + 1);
    let pending_after = parked.pending();
    let repairs = after.parked_cursor_repairs;

    let _meter = finish(&mut parked);
    let report = parked.complete().expect("sweep the finished mark");
    drop(parked);
    assert!(
        heap.alive(chain.z),
        "Z must stay reachable from A; pending after the edit was {pending_after:?}, Y record {y_index}, Z record {z_index}, repairs {repairs}"
    );
    assert!(
        !heap.alive(chain.w),
        "W must not be marked through the reused slot; pending after the edit was {pending_after:?}, repairs {repairs}"
    );
    assert_eq!(heap.edge(chain.a, KEY_X).expect("X edge"), Some(chain.x));
    assert_eq!(chain.x.slot(), x_slot);
    assert_eq!(chain.x.generation(), x_generation);
    assert_eq!(heap.edge(chain.a, KEY_Z).expect("Z edge"), Some(chain.z));
    assert_eq!(chain.z.slot(), z_slot);
    assert_eq!(chain.z.generation(), z_generation);
    assert!(!heap.alive(chain.y));
    assert!(!heap.alive(chain.d));
    assert_eq!(
        pending_after.map(|scan| (scan.owner, scan.cursor)),
        Some((chain.a.slot(), z_index)),
        "unlink advances A's cursor to Z"
    );
    let meter = heap.take_scan_meter();
    assert_eq!(meter.mark_passes, 1, "one mark round");
    assert_eq!(
        meter.parked_cursor_repairs, 1,
        "the delete repairs the cursor once"
    );
    assert_eq!(meter.parked_edge_writes, 1);
    assert_eq!(report.live_nodes, 3, "A, X, and Z remain");
    assert_eq!(heap.stats().collections, 1);
}

#[test]
fn delete_only_keeps_the_successor() {
    let mut heap = LuaHeap::new(limits(8, 2, 8));
    let chain = chain(&mut heap);
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    let y_index = parked.field_index(chain.a, KEY_Y).expect("Y record");
    let z_index = parked.field_index(chain.a, KEY_Z).expect("Z record");
    pause_at(&mut parked, chain.a.slot(), y_index);
    parked.delete_field(chain.a, KEY_Y).expect("delete Y");
    assert_eq!(parked.field_index(chain.a, KEY_Y), None);
    let pending_after = parked.pending();
    let repairs = parked.meter().parked_cursor_repairs;

    // Without a repair the next charge reads the freed record and hits the
    // occupied-record check. That is the behavior failure, not a compile error.
    let _meter = finish(&mut parked);
    parked.complete().expect("sweep");
    drop(parked);
    assert!(
        heap.alive(chain.z),
        "deleting Y must keep Z; pending was {pending_after:?}, Z record {z_index}, repairs {repairs}"
    );
    assert_eq!(heap.edge(chain.a, KEY_X).expect("X edge"), Some(chain.x));
    assert_eq!(heap.edge(chain.a, KEY_Z).expect("Z edge"), Some(chain.z));
    assert!(!heap.alive(chain.y));
    assert!(!heap.alive(chain.w));
    assert_eq!(
        pending_after.map(|scan| (scan.owner, scan.cursor)),
        Some((chain.a.slot(), z_index))
    );
    let meter = heap.take_scan_meter();
    assert_eq!(meter.mark_passes, 1);
    assert_eq!(meter.parked_cursor_repairs, 1);
    assert_eq!(meter.parked_edge_writes, 0);
    assert_eq!(heap.stats().collections, 1);
}

#[test]
fn omitted_pause_keeps_the_pending_edge() {
    let mut heap = LuaHeap::new(limits(8, 2, 8));
    let chain = chain(&mut heap);
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    let y_index = parked.field_index(chain.a, KEY_Y).expect("Y record");
    pause_at(&mut parked, chain.a.slot(), y_index);
    assert_eq!(parked.meter().parked_cursor_repairs, 0);
    assert_eq!(parked.meter().parked_edge_writes, 0);
    finish(&mut parked);
    parked.complete().expect("sweep");
    drop(parked);
    assert_eq!(heap.edge(chain.a, KEY_Y).expect("Y edge"), Some(chain.y));
    assert!(heap.alive(chain.y));
    assert!(heap.alive(chain.x));
    assert!(heap.alive(chain.z));
    assert!(!heap.alive(chain.d));
    assert!(!heap.alive(chain.w));
    let meter = heap.take_scan_meter();
    assert_eq!(meter.parked_cursor_repairs, 0);
    assert_eq!(meter.parked_edge_writes, 0);
    assert_eq!(meter.mark_passes, 1);
}

#[derive(Clone, Copy)]
enum NewNamedWrite {
    Strong,
    WeakValue,
    Omitted,
}

fn new_named_write_after_owner_scan(write: NewNamedWrite) {
    use crate::heap::{self, GuestClass, StepOutcome};

    const NAME: &str = "新键";
    const CHILD: &str = "child";
    let retained = matches!(write, NewNamedWrite::Strong);
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(8, 2, 8)).expect("install");
        let raw = crate::publish_raw(&mut host);
        let target = crate::table_ref(&host, raw);
        let (owner, sentinel, child, carrier) = heap::with_heap(&mut host, |heap| {
            let owner = heap.alloc()?;
            let sentinel = heap.alloc()?;
            let child = heap.alloc()?;
            let unrelated = heap.alloc()?; // Forces a real published tail in all arms.
            if matches!(write, NewNamedWrite::WeakValue) {
                let meta = heap.alloc()?;
                heap.set_mode_scalar(meta, Value::string("v"))?;
                heap.set_metatable(owner, Some(meta))?;
            }
            let frame = heap.enter_roots(2)?;
            // The Gray stack pops the owner first, then the sentinel.
            heap.write_root(frame, 0, sentinel)?;
            heap.write_root(frame, 1, owner)?;
            heap.set_named_scalar(owner, "old", Value::Bool(true))?;
            heap.set_named_scalar(sentinel, "pending", Value::Bool(true))?;
            // Exercise a normal insertion and then an in-place replacement before marking.
            heap.set_named_edge(target, CHILD, unrelated)?;
            assert_eq!(heap.named_edge(target, CHILD)?, Some(unrelated));
            heap.set_named_edge(target, CHILD, child)?;
            assert_eq!(heap.named_edge(target, CHILD)?, Some(child));
            Ok((owner, sentinel, child, heap.issue_carrier(target)?))
        })
        .expect("graph");
        heap::open_fixed_step(&mut host).expect("start a saved mark");
        let (pending, before) = heap::with_heap(&mut host, |heap| {
            let (_, old_record) = heap.locate_named(owner.slot(), "old").expect("old field");
            let (_, sentinel_record) = heap
                .locate_named(sentinel.slot(), "pending")
                .expect("sentinel field");
            let mut parked = ParkedMark::resume(heap).expect("resume saved mark");
            pause_at(&mut parked, owner.slot(), old_record);
            let before_owner_edge = parked.meter().outgoing_edge_checks;
            let pending = pause_at(&mut parked, sentinel.slot(), sentinel_record);
            let before = parked.meter();
            assert!(before.outgoing_edge_checks > before_owner_edge);
            assert!(parked.marked(owner));
            assert!(!parked.marked(target) && !parked.marked(child));
            parked.park()?;
            assert!(heap.locate_named(owner.slot(), NAME).is_none());
            if !matches!(write, NewNamedWrite::Omitted) {
                heap.set_named_edge(owner, NAME, target)?;
                assert_eq!(heap.named_edge(owner, NAME)?, Some(target));
            }
            Ok((pending, before))
        })
        .expect("ordinary write while Gray is parked");
        let after = heap::with_heap(&mut host, |heap| {
            let parked = ParkedMark::resume(heap).expect("write kept the saved mark");
            assert_eq!(parked.phase_name(), "gray");
            let cursor = parked.pending().expect("sentinel still pending");
            assert_eq!(
                (cursor.owner, cursor.cursor),
                (pending.owner, pending.cursor)
            );
            assert!(!parked.marked(child), "the write must not scan descendants");
            let after = parked.meter();
            assert_eq!(after.outgoing_edge_checks, before.outgoing_edge_checks);
            assert_eq!(after.owner_head_checks, before.owner_head_checks);
            parked.park()?;
            Ok(after)
        })
        .expect("cursor observation");

        // The private fixed-frame path passes an empty supplemental root batch.
        let published = heap::step_fixed_frames(&mut host, 128).expect("publish saved mark");
        assert!(
            matches!(published, StepOutcome::Published { .. }),
            "{published:?}"
        );
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 0);
        heap::with_heap(&mut host, |heap| {
            assert_eq!(
                heap.set_named_edge(owner, "blocked", sentinel),
                Err(HeapError::SweepPending)
            );
            assert!(heap.locate_named(owner.slot(), "blocked").is_none());
            if !retained {
                assert_eq!(heap.named_value(owner, NAME)?, Value::Null);
                assert_eq!(
                    heap.locate_named(owner.slot(), NAME).is_some(),
                    matches!(write, NewNamedWrite::WeakValue),
                    "a weak-value write leaves a cleared cursor; omission adds no record"
                );
            }
            Ok(())
        })
        .expect("read during publication");
        let done = heap::step_fixed_frames(&mut host, 128).expect("finish saved tail");
        assert!(matches!(done, StepOutcome::Completed(_)), "{done:?}");
        assert!(heap::fixed_step_idle(&mut host).expect("idle"));
        heap::with_heap(&mut host, |heap| {
            assert_eq!(
                heap.alive(target),
                retained,
                "a fresh strong UTF-8 field on a scanned owner must keep its target alive"
            );
            assert_eq!(
                heap.alive(child),
                retained,
                "transitive marking in the same pass"
            );
            assert_eq!(heap.stats().collections, 1);
            assert_eq!(heap.scan_meter().mark_passes, 1);
            if retained {
                let value = heap.named_value(owner, NAME)?;
                assert_eq!(value, carrier);
                assert_eq!(heap.guest_class(&value), GuestClass::Table(target));
                assert_eq!(heap.named_edge(target, CHILD)?, Some(child));
                heap.set_named_scalar(owner, NAME, Value::Null)?;
            }
            Ok(())
        })
        .expect("same-cycle reachability");
        assert_eq!(
            after.parked_barrier_marks - before.parked_barrier_marks,
            u64::from(retained)
        );
        if retained {
            heap::collect(&mut host).expect("next collection after removing the only strong edge");
            heap::with_heap(&mut host, |heap| {
                assert!(!heap.alive(target) && !heap.alive(child));
                assert_eq!(heap.stats().collections, 2);
                Ok(())
            })
            .expect("later reclamation");
        }
    });
}

#[test]
fn gray_new_named_strong_edge_keeps_target_and_descendant() {
    new_named_write_after_owner_scan(NewNamedWrite::Strong);
}

#[test]
fn gray_new_named_weak_value_clears_at_publication() {
    new_named_write_after_owner_scan(NewNamedWrite::WeakValue);
}

#[test]
fn gray_new_named_write_omitted_leaves_target_unreachable() {
    new_named_write_after_owner_scan(NewNamedWrite::Omitted);
}

#[derive(Clone, Copy)]
enum ExistingNamedWrite {
    Strong,
    WeakValue,
    Omitted,
}

const FRESH_UTF8: &str = "新键";

fn assert_ordinary_utf8_hit(heap: &LuaHeap, owner: u32, name: &str, index: u32) {
    let key = super::EdgeKey::named(name);
    let fresh = super::EdgeKey::named(FRESH_UTF8);
    assert_ne!(name, FRESH_UTF8);
    assert!(matches!(key, super::EdgeKey::Utf8(ref text) if text == name));
    assert!(matches!(fresh, super::EdgeKey::Utf8(_)));
    assert_ne!(key, fresh);
    assert_eq!(
        heap.find_key(owner, &key),
        Some(index),
        "find_key hits the existing ordinary UTF-8 record"
    );
    assert_eq!(
        heap.find_key(owner, &fresh),
        None,
        "the replacement key is not a new key"
    );
    let stored = heap.edge_slots[index as usize]
        .edge
        .as_ref()
        .expect("occupied record");
    assert!(matches!(&stored.key, super::EdgeKey::Utf8(text) if text == name));
    assert!(!stored.value.is_cursor_gap());
}

/// Replaces an already scanned ordinary UTF-8 record in place.
fn existing_named_replacement_after_owner_scan(write: ExistingNamedWrite) {
    use crate::heap::{self, GuestClass, StepOutcome};

    const OLD: &str = "旧键";
    const CHILD: &str = "child";
    let retained = matches!(write, ExistingNamedWrite::Strong);
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(8, 2, 8)).expect("install");
        let raw = crate::publish_raw(&mut host);
        let target = crate::table_ref(&host, raw);
        let (owner, sentinel, child, carrier) = heap::with_heap(&mut host, |heap| {
            let owner = heap.alloc()?;
            let sentinel = heap.alloc()?;
            let child = heap.alloc()?;
            let unrelated = heap.alloc()?; // Forces a real published tail in all arms.
            if matches!(write, ExistingNamedWrite::WeakValue) {
                let meta = heap.alloc()?;
                heap.set_mode_scalar(meta, Value::string("v"))?;
                heap.set_metatable(owner, Some(meta))?;
            }
            let frame = heap.enter_roots(2)?;
            // The Gray stack pops the owner first, then the sentinel.
            heap.write_root(frame, 0, sentinel)?;
            heap.write_root(frame, 1, owner)?;
            heap.set_named_scalar(owner, OLD, Value::Bool(true))?;
            heap.set_named_scalar(sentinel, "pending", Value::Bool(true))?;
            heap.set_named_edge(target, CHILD, unrelated)?;
            heap.set_named_edge(target, CHILD, child)?;
            assert_eq!(heap.named_edge(target, CHILD)?, Some(child));
            assert_eq!(heap.named_value(owner, OLD)?, Value::Bool(true));
            Ok((owner, sentinel, child, heap.issue_carrier(target)?))
        })
        .expect("graph");
        heap::open_fixed_step(&mut host).expect("start a saved mark");
        let old_record = heap::with_heap(&mut host, |heap| {
            let (_, old_record) = heap.locate_named(owner.slot(), OLD).expect("old field");
            let (_, sentinel_record) = heap
                .locate_named(sentinel.slot(), "pending")
                .expect("sentinel field");
            let mut parked = ParkedMark::resume(heap).expect("resume saved mark");
            pause_at(&mut parked, owner.slot(), old_record);
            let pending = pause_at(&mut parked, sentinel.slot(), sentinel_record);
            assert_eq!(pending.owner, sentinel.slot());
            assert!(parked.marked(owner), "the scanned owner is already marked");
            assert!(!parked.marked(target) && !parked.marked(child));
            let before = parked.meter();
            parked.park()?;
            assert_ordinary_utf8_hit(heap, owner.slot(), OLD, old_record);
            let edges_before = (heap.stats().live_edges, heap.stats().edge_slots);
            if !matches!(write, ExistingNamedWrite::Omitted) {
                heap.set_named_edge(owner, OLD, target)?;
                assert_eq!(heap.named_edge(owner, OLD)?, Some(target));
            } else {
                assert_eq!(heap.named_value(owner, OLD)?, Value::Bool(true));
            }
            assert_ordinary_utf8_hit(heap, owner.slot(), OLD, old_record);
            assert_eq!(
                heap.locate_named(owner.slot(), OLD).map(|(_, index)| index),
                Some(old_record),
                "the old key record index is unchanged"
            );
            assert_eq!(
                (heap.stats().live_edges, heap.stats().edge_slots),
                edges_before,
                "the write does not change the edge count"
            );
            if !matches!(write, ExistingNamedWrite::Omitted) {
                let stored = heap.edge_slots[old_record as usize]
                    .edge
                    .as_ref()
                    .expect("replaced record");
                assert!(matches!(
                    stored.value,
                    super::EdgeValue::Table { slot, generation }
                        if slot == target.slot() && generation == target.generation()
                ));
            }
            let parked = ParkedMark::resume(heap).expect("write kept the saved mark");
            assert_eq!(parked.phase_name(), "gray");
            let cursor = parked.pending().expect("sentinel still pending");
            assert_eq!(
                (cursor.owner, cursor.cursor),
                (pending.owner, pending.cursor),
                "the pending scan cursor is unchanged"
            );
            assert!(
                !parked.marked(child),
                "the write must not scan the child edge"
            );
            assert_eq!(
                parked.marked(target),
                retained,
                "only a strong replacement enters the gray queue immediately"
            );
            let after = parked.meter();
            assert_write_did_not_scan(&before, &after);
            assert_eq!(after.parked_edge_writes, before.parked_edge_writes);
            assert_eq!(
                after.parked_barrier_marks - before.parked_barrier_marks,
                u64::from(retained)
            );
            assert_eq!(
                after.scratch.work_pushed - before.scratch.work_pushed,
                u64::from(retained)
            );
            assert_eq!(
                after.node_slot_visits - before.node_slot_visits,
                u64::from(retained) * 2
            );
            parked.park()?;
            Ok(old_record)
        })
        .expect("in-place replacement while Gray is parked");

        let published = heap::step_fixed_frames(&mut host, 128).expect("publish saved mark");
        assert!(
            matches!(published, StepOutcome::Published { .. }),
            "{published:?}"
        );
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 0);
        heap::with_heap(&mut host, |heap| {
            assert_eq!(
                heap.set_named_edge(owner, "blocked", sentinel),
                Err(HeapError::SweepPending)
            );
            assert!(heap.locate_named(owner.slot(), "blocked").is_none());
            if matches!(write, ExistingNamedWrite::WeakValue) {
                assert_eq!(heap.named_value(owner, OLD)?, Value::Null);
                let (_, index) = heap
                    .locate_named(owner.slot(), OLD)
                    .expect("a weak-value replacement leaves the cleared cursor");
                assert_eq!(index, old_record);
                let stored = heap.edge_slots[index as usize]
                    .edge
                    .as_ref()
                    .expect("retained gap");
                assert!(stored.value.is_cursor_gap());
                assert!(matches!(&stored.key, super::EdgeKey::Utf8(text) if text == OLD));
            }
            if matches!(write, ExistingNamedWrite::Omitted) {
                assert_eq!(heap.named_value(owner, OLD)?, Value::Bool(true));
            }
            Ok(())
        })
        .expect("read during publication");
        let done = heap::step_fixed_frames(&mut host, 128).expect("finish saved tail");
        assert!(matches!(done, StepOutcome::Completed(_)), "{done:?}");
        assert!(heap::fixed_step_idle(&mut host).expect("idle"));
        heap::with_heap(&mut host, |heap| {
            assert_eq!(
                heap.alive(target),
                retained,
                "an in-place strong UTF-8 replacement on a scanned owner must keep its target alive"
            );
            assert_eq!(
                heap.alive(child),
                retained,
                "the child is reached only by the continued scan"
            );
            assert_eq!(heap.stats().collections, 1);
            assert_eq!(heap.scan_meter().mark_passes, 1);
            if matches!(write, ExistingNamedWrite::Omitted) {
                assert_eq!(heap.named_value(owner, OLD)?, Value::Bool(true));
            }
            if retained {
                let value = heap.named_value(owner, OLD)?;
                assert_eq!(value, carrier);
                assert_eq!(heap.guest_class(&value), GuestClass::Table(target));
                assert_eq!(heap.named_edge(target, CHILD)?, Some(child));
                heap.set_named_scalar(owner, OLD, Value::Null)?;
            }
            Ok(())
        })
        .expect("same-cycle reachability");
        if retained {
            heap::collect(&mut host).expect("next collection after removing the only strong edge");
            heap::with_heap(&mut host, |heap| {
                assert!(!heap.alive(target) && !heap.alive(child));
                assert_eq!(heap.stats().collections, 2);
                Ok(())
            })
            .expect("later reclamation");
        }
    });
}

#[test]
fn gray_existing_utf8_strong_replacement_keeps_the_new_graph() {
    existing_named_replacement_after_owner_scan(ExistingNamedWrite::Strong);
}

#[test]
fn gray_existing_utf8_weak_replacement_clears_at_publication() {
    existing_named_replacement_after_owner_scan(ExistingNamedWrite::WeakValue);
}

#[test]
fn gray_existing_utf8_omitted_replacement_keeps_the_original_scalar() {
    existing_named_replacement_after_owner_scan(ExistingNamedWrite::Omitted);
}

#[derive(Clone, Copy)]
enum ExistingNumberWrite {
    Strong,
    WeakValue,
    Omitted,
}

/// Ordinary finite key. `EdgeKey::number` already stores `+0` and `-0` as the
/// same bits, so this witness does not pair those spellings.
const NUMBER_KEY: f64 = 7.0;
const ORIGINAL_NUMBER_SCALAR: &str = "原值";

/// Replaces an already scanned ordinary number record in place.
fn existing_number_replacement_after_owner_scan(write: ExistingNumberWrite) {
    use crate::heap::{self, GuestClass, StepOutcome};

    const CHILD: &str = "child";
    const OTHER_KEY: f64 = 9.0;
    let retained = matches!(write, ExistingNumberWrite::Strong);
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(8, 2, 8)).expect("install");
        let raw = crate::publish_raw(&mut host);
        let target = crate::table_ref(&host, raw);
        let (owner, sentinel, child, stale, carrier) = heap::with_heap(&mut host, |heap| {
            let owner = heap.alloc()?;
            let sentinel = heap.alloc()?;
            let child = heap.alloc()?;
            let unrelated = heap.alloc()?; // Forces a real published tail in all arms.
            let stale = heap.alloc()?;
            heap.rollback(stale)?;
            if matches!(write, ExistingNumberWrite::WeakValue) {
                let meta = heap.alloc()?;
                heap.set_mode_scalar(meta, Value::string("v"))?;
                heap.set_metatable(owner, Some(meta))?;
            }
            let frame = heap.enter_roots(2)?;
            // The Gray stack pops the owner first, then the sentinel.
            heap.write_root(frame, 0, sentinel)?;
            heap.write_root(frame, 1, owner)?;
            heap.set_number_scalar(owner, NUMBER_KEY, Value::string(ORIGINAL_NUMBER_SCALAR))?;
            heap.set_named_scalar(sentinel, "pending", Value::Bool(true))?;
            heap.set_named_edge(target, CHILD, unrelated)?;
            heap.set_named_edge(target, CHILD, child)?;
            assert_eq!(heap.named_edge(target, CHILD)?, Some(child));
            assert_eq!(
                heap.number_value(owner, NUMBER_KEY)?,
                Value::string(ORIGINAL_NUMBER_SCALAR)
            );
            Ok((owner, sentinel, child, stale, heap.issue_carrier(target)?))
        })
        .expect("graph");
        heap::open_fixed_step(&mut host).expect("start a saved mark");
        let old_record = heap::with_heap(&mut host, |heap| {
            let number_key = super::EdgeKey::number(NUMBER_KEY);
            let old_record = heap
                .find_key(owner.slot(), &number_key)
                .expect("existing number record");
            let stored = heap.edge_slots[old_record as usize]
                .edge
                .as_ref()
                .expect("occupied number record");
            assert_eq!(&stored.key, &number_key);
            assert!(matches!(stored.key, super::EdgeKey::Number(_)));
            assert!(!matches!(stored.key, super::EdgeKey::Field(_)));
            assert!(!stored.value.is_cursor_gap());
            let (_, sentinel_record) = heap
                .locate_named(sentinel.slot(), "pending")
                .expect("sentinel field");
            let mut parked = ParkedMark::resume(heap).expect("resume saved mark");
            pause_at(&mut parked, owner.slot(), old_record);
            let pending = pause_at(&mut parked, sentinel.slot(), sentinel_record);
            assert_eq!(pending.owner, sentinel.slot());
            assert!(parked.marked(owner), "the scanned owner is already marked");
            assert!(
                !parked.marked(target),
                "the replacement target has no supplemental root"
            );
            let before = parked.meter();
            parked.park()?;
            let edges_before = (heap.stats().live_edges, heap.stats().edge_slots);
            if !matches!(write, ExistingNumberWrite::Omitted) {
                heap.set_number_edge(owner, NUMBER_KEY, target)?;
                assert_eq!(heap.number_value(owner, NUMBER_KEY)?, carrier);
            } else {
                assert_eq!(
                    heap.number_value(owner, NUMBER_KEY)?,
                    Value::string(ORIGINAL_NUMBER_SCALAR)
                );
            }
            assert_eq!(
                heap.find_key(owner.slot(), &number_key),
                Some(old_record),
                "the old number record index is unchanged"
            );
            assert_eq!(
                (heap.stats().live_edges, heap.stats().edge_slots),
                edges_before,
                "the write does not change the edge count"
            );
            if !matches!(write, ExistingNumberWrite::Omitted) {
                let stored = heap.edge_slots[old_record as usize]
                    .edge
                    .as_ref()
                    .expect("replaced record");
                assert_eq!(&stored.key, &number_key);
                assert!(matches!(
                    stored.value,
                    super::EdgeValue::Table { slot, generation }
                        if slot == target.slot() && generation == target.generation()
                ));
            }
            let parked = ParkedMark::resume(heap).expect("write kept the saved mark");
            assert_eq!(parked.phase_name(), "gray");
            let cursor = parked.pending().expect("sentinel still pending");
            assert_eq!(
                (cursor.owner, cursor.cursor),
                (pending.owner, pending.cursor),
                "the pending scan cursor is unchanged"
            );
            assert!(
                !parked.marked(child),
                "the write must not scan the child edge"
            );
            assert_eq!(
                parked.marked(target),
                retained,
                "only a strong number replacement enters the gray queue immediately"
            );
            let after = parked.meter();
            assert_write_did_not_scan(&before, &after);
            assert_eq!(after.parked_edge_writes, before.parked_edge_writes);
            assert_eq!(
                after.parked_barrier_marks - before.parked_barrier_marks,
                u64::from(retained)
            );
            assert_eq!(
                after.scratch.work_pushed - before.scratch.work_pushed,
                u64::from(retained)
            );
            assert_eq!(
                after.node_slot_visits - before.node_slot_visits,
                u64::from(retained) * 2
            );
            parked.park()?;
            Ok(old_record)
        })
        .expect("in-place number replacement while Gray is parked");

        let published = heap::step_fixed_frames(&mut host, 128).expect("publish saved mark");
        assert!(
            matches!(published, StepOutcome::Published { .. }),
            "{published:?}"
        );
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 0);
        heap::with_heap(&mut host, |heap| {
            // NaN and a stale handle stay ahead of the sweep refusal.
            assert_eq!(
                heap.set_number_edge(owner, f64::NAN, sentinel),
                Err(HeapError::NanIndex)
            );
            assert_eq!(
                heap.clear_number_edge(owner, f64::NAN),
                Err(HeapError::NanIndex)
            );
            assert_eq!(
                heap.set_number_edge(stale, NUMBER_KEY, sentinel),
                Err(HeapError::StaleHandle)
            );
            assert_eq!(
                heap.set_number_edge(owner, NUMBER_KEY, stale),
                Err(HeapError::StaleHandle)
            );
            assert_eq!(
                heap.set_number_edge(owner, OTHER_KEY, sentinel),
                Err(HeapError::SweepPending)
            );
            assert!(
                heap.find_key(owner.slot(), &super::EdgeKey::number(OTHER_KEY))
                    .is_none()
            );
            if matches!(write, ExistingNumberWrite::WeakValue) {
                assert_eq!(
                    heap.number_value(owner, NUMBER_KEY)?,
                    Value::Null,
                    "a weak number replacement publishes nil"
                );
                let index = heap
                    .find_key(owner.slot(), &super::EdgeKey::number(NUMBER_KEY))
                    .expect("a weak-value replacement leaves the cleared cursor");
                assert_eq!(index, old_record);
                let stored = heap.edge_slots[index as usize]
                    .edge
                    .as_ref()
                    .expect("retained gap");
                assert!(stored.value.is_cursor_gap());
                assert_eq!(&stored.key, &super::EdgeKey::number(NUMBER_KEY));
            }
            if matches!(write, ExistingNumberWrite::Omitted) {
                assert_eq!(
                    heap.number_value(owner, NUMBER_KEY)?,
                    Value::string(ORIGINAL_NUMBER_SCALAR)
                );
            }
            if matches!(write, ExistingNumberWrite::Strong) {
                assert_eq!(heap.number_value(owner, NUMBER_KEY)?, carrier);
            }
            Ok(())
        })
        .expect("read during publication");
        let done = heap::step_fixed_frames(&mut host, 128).expect("finish saved tail");
        assert!(matches!(done, StepOutcome::Completed(_)), "{done:?}");
        assert!(heap::fixed_step_idle(&mut host).expect("idle"));
        heap::with_heap(&mut host, |heap| {
            assert_eq!(
                heap.alive(target),
                retained,
                "an in-place strong number replacement on a scanned owner must keep its target alive"
            );
            assert_eq!(
                heap.alive(child),
                retained,
                "the child is reached only by the continued scan"
            );
            assert_eq!(heap.stats().collections, 1);
            assert_eq!(heap.scan_meter().mark_passes, 1);
            if matches!(write, ExistingNumberWrite::Omitted) {
                assert_eq!(
                    heap.number_value(owner, NUMBER_KEY)?,
                    Value::string(ORIGINAL_NUMBER_SCALAR),
                    "an omitted number replacement keeps the original string"
                );
            }
            if retained {
                let value = heap.number_value(owner, NUMBER_KEY)?;
                assert_eq!(value, carrier);
                assert_eq!(heap.guest_class(&value), GuestClass::Table(target));
                assert_eq!(heap.named_edge(target, CHILD)?, Some(child));
                heap.clear_number_edge(owner, NUMBER_KEY)?;
            }
            Ok(())
        })
        .expect("same-cycle reachability");
        if retained {
            heap::collect(&mut host).expect("next collection after removing the only strong edge");
            heap::with_heap(&mut host, |heap| {
                assert!(!heap.alive(target) && !heap.alive(child));
                assert_eq!(heap.stats().collections, 2);
                Ok(())
            })
            .expect("later reclamation");
        }
    });
}

#[test]
fn gray_existing_number_strong_replacement_keeps_the_new_graph() {
    existing_number_replacement_after_owner_scan(ExistingNumberWrite::Strong);
}

#[test]
fn gray_existing_number_weak_replacement_clears_at_publication() {
    existing_number_replacement_after_owner_scan(ExistingNumberWrite::WeakValue);
}

#[test]
fn gray_existing_number_omitted_replacement_keeps_the_original_string() {
    existing_number_replacement_after_owner_scan(ExistingNumberWrite::Omitted);
}

#[derive(Clone, Copy)]
enum ExistingObjectWrite {
    Strong,
    WeakValue,
    /// Key mode `k`. The value stays strong. The key has its own root.
    WeakKey,
    Omitted,
}

const ORIGINAL_OBJECT_SCALAR: &str = "原值";
const OBJECT_SHAPE: &str = "key";

fn object_write_retains(write: ExistingObjectWrite) -> bool {
    matches!(
        write,
        ExistingObjectWrite::Strong | ExistingObjectWrite::WeakKey
    )
}

/// Replaces an already scanned table-object record in place.
///
/// The record is built with `set_object_scalar`, not a gap and not a new key.
/// The key and a same-shape lookalike are both issued and independently rooted.
/// An unmarked weak key is outside this slice.
fn existing_object_replacement_after_owner_scan(write: ExistingObjectWrite) {
    use crate::heap::{self, GuestClass, StepOutcome};

    const CHILD: &str = "child";
    let retained = object_write_retains(write);
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(16, 8, 16)).expect("install");
        let raw = crate::publish_raw(&mut host);
        let target = crate::table_ref(&host, raw);
        let (owner, sentinel, child, stale, key, other, closure, frame, meta, carrier) =
            heap::with_heap(&mut host, |heap| {
                let owner = heap.alloc()?;
                let sentinel = heap.alloc()?;
                let child = heap.alloc()?;
                let unrelated = heap.alloc()?;
                let stale = heap.alloc()?;
                heap.rollback(stale)?;
                let key = heap.alloc()?;
                let other = heap.alloc()?;
                let closure = heap.alloc_closure()?;
                let meta = match write {
                    ExistingObjectWrite::WeakValue | ExistingObjectWrite::WeakKey => {
                        let meta = heap.alloc()?;
                        let mode = match write {
                            ExistingObjectWrite::WeakValue => "v",
                            ExistingObjectWrite::WeakKey => "k",
                            ExistingObjectWrite::Strong | ExistingObjectWrite::Omitted => {
                                unreachable!("only the weak arms store a mode")
                            }
                        };
                        heap.set_mode_scalar(meta, Value::string(mode))?;
                        heap.set_metatable(owner, Some(meta))?;
                        Some(meta)
                    }
                    ExistingObjectWrite::Strong | ExistingObjectWrite::Omitted => None,
                };
                let frame = heap.enter_roots(5)?;
                // Pushed in order, popped in reverse. The owner is scanned
                // before the sentinel. The key is already a root.
                heap.write_root(frame, 0, sentinel)?;
                heap.write_root(frame, 1, owner)?;
                heap.write_root(frame, 2, key)?;
                heap.write_root(frame, 3, other)?;
                heap.write_closure_root(frame, 4, closure)?;
                heap.set_named_scalar(key, "tag", Value::string(OBJECT_SHAPE))?;
                heap.set_named_scalar(other, "tag", Value::string(OBJECT_SHAPE))?;
                heap.set_object_scalar(owner, key, Value::string(ORIGINAL_OBJECT_SCALAR))?;
                heap.set_named_scalar(sentinel, "pending", Value::Bool(true))?;
                heap.set_named_edge(target, CHILD, unrelated)?;
                heap.set_named_edge(target, CHILD, child)?;
                assert_eq!(heap.named_edge(target, CHILD)?, Some(child));
                assert_eq!(
                    heap.named_value(key, "tag")?,
                    heap.named_value(other, "tag")?
                );
                assert_ne!(key, other);
                assert_eq!(
                    heap.object_value(owner, key)?,
                    Value::string(ORIGINAL_OBJECT_SCALAR)
                );
                assert_eq!(heap.object_value(owner, other)?, Value::Null);
                let carrier = heap.issue_carrier(target)?;
                let key_carrier = heap.issue_carrier(key)?;
                let other_carrier = heap.issue_carrier(other)?;
                assert_eq!(heap.guest_class(&key_carrier), GuestClass::Table(key));
                assert_eq!(heap.guest_class(&other_carrier), GuestClass::Table(other));
                assert_ne!(key_carrier, other_carrier);
                Ok((
                    owner, sentinel, child, stale, key, other, closure, frame, meta, carrier,
                ))
            })
            .expect("graph");
        heap::open_fixed_step(&mut host).expect("start a saved mark");
        let old_record = heap::with_heap(&mut host, |heap| {
            let object_key = super::EdgeKey::object(key);
            let old_record = heap
                .find_key(owner.slot(), &object_key)
                .expect("existing object record");
            let stored = heap.edge_slots[old_record as usize]
                .edge
                .as_ref()
                .expect("occupied object record");
            assert_eq!(&stored.key, &object_key);
            assert!(matches!(
                stored.key,
                super::EdgeKey::Object {
                    slot,
                    generation,
                    kind: super::HandleKind::Table,
                } if slot == key.slot() && generation == key.generation()
            ));
            assert!(!stored.value.is_cursor_gap());
            assert!(matches!(
                stored.value,
                super::EdgeValue::Scalar(Value::String(_))
            ));
            let (_, sentinel_record) = heap
                .locate_named(sentinel.slot(), "pending")
                .expect("sentinel field");
            for slot in 0..4 {
                let rooted = heap.rooted_table(frame, slot)?;
                assert_ne!(rooted, Some(target), "target has no fixed root");
                assert_ne!(rooted, Some(child), "child has no fixed root");
            }
            let values_weak = if let Some(meta) = meta {
                let stored = heap.named_value(meta, "__mode")?;
                let Value::String(text) = &stored else {
                    panic!("stored mode is not a string: {stored:?}");
                };
                let expected = match write {
                    ExistingObjectWrite::WeakValue => "v",
                    ExistingObjectWrite::WeakKey => "k",
                    ExistingObjectWrite::Strong | ExistingObjectWrite::Omitted => {
                        unreachable!("a metatable is only stored for a weak arm")
                    }
                };
                assert_eq!(
                    text.as_str(),
                    expected,
                    "the barrier reads this stored mode"
                );
                text.as_bytes().contains(&b'v')
            } else {
                false
            };
            assert_eq!(
                retained,
                !matches!(write, ExistingObjectWrite::Omitted) && !values_weak,
                "retention follows strong values, not weak keys"
            );
            let mut parked = ParkedMark::resume(heap).expect("resume saved mark");
            assert_eq!(parked.meter().parked_root_batch_accepts, 0);
            pause_at(&mut parked, owner.slot(), old_record);
            assert!(
                parked.marked(key),
                "the independent root marked the key before this record was charged"
            );
            assert!(!parked.marked(target) && !parked.marked(child));
            let checks_on_record = parked.meter().outgoing_edge_checks;
            let pending = pause_at(&mut parked, sentinel.slot(), sentinel_record);
            assert!(
                parked.meter().outgoing_edge_checks > checks_on_record,
                "the owner scan charged this object record"
            );
            assert_eq!(pending.owner, sentinel.slot());
            assert_eq!(pending.cursor, sentinel_record);
            assert!(parked.marked(owner), "the scanned owner is already marked");
            assert!(parked.marked(key) && parked.marked(other));
            let before = parked.meter();
            parked.park()?;
            let edges_before = (heap.stats().live_edges, heap.stats().edge_slots);
            if !matches!(write, ExistingObjectWrite::Omitted) {
                heap.set_object_edge(owner, key, target)?;
                assert_eq!(heap.object_value(owner, key)?, carrier);
                assert_eq!(heap.guest_class(&carrier), GuestClass::Table(target));
                assert_eq!(heap.object_value(owner, other)?, Value::Null);
            } else {
                assert_eq!(
                    heap.object_value(owner, key)?,
                    Value::string(ORIGINAL_OBJECT_SCALAR)
                );
            }
            assert_eq!(
                heap.find_key(owner.slot(), &object_key),
                Some(old_record),
                "the old object record index is unchanged"
            );
            assert!(
                heap.find_key(owner.slot(), &super::EdgeKey::object(other))
                    .is_none()
            );
            assert_eq!(
                (heap.stats().live_edges, heap.stats().edge_slots),
                edges_before,
                "the write does not change the edge count"
            );
            let stored = heap.edge_slots[old_record as usize]
                .edge
                .as_ref()
                .expect("replaced record");
            assert!(matches!(
                stored.key,
                super::EdgeKey::Object {
                    slot,
                    generation,
                    kind: super::HandleKind::Table,
                } if slot == key.slot() && generation == key.generation()
            ));
            if !matches!(write, ExistingObjectWrite::Omitted) {
                assert!(matches!(
                    stored.value,
                    super::EdgeValue::Table { slot, generation }
                        if slot == target.slot() && generation == target.generation()
                ));
            }
            let parked = ParkedMark::resume(heap).expect("write kept the saved mark");
            assert_eq!(parked.phase_name(), "gray");
            let cursor = parked.pending().expect("sentinel still pending");
            assert_eq!(
                (cursor.owner, cursor.cursor),
                (pending.owner, pending.cursor),
                "the pending scan cursor is unchanged"
            );
            assert!(
                parked.marked(key),
                "the write must not recolor the existing object key"
            );
            assert!(
                !parked.marked(child),
                "the write must not scan the child edge"
            );
            assert_eq!(
                parked.marked(target),
                retained,
                "only a strong object-key replacement enters the gray queue immediately"
            );
            let after = parked.meter();
            assert_write_did_not_scan(&before, &after);
            assert_eq!(after.parked_edge_writes, before.parked_edge_writes);
            assert_eq!(
                after.parked_root_batch_accepts,
                before.parked_root_batch_accepts
            );
            assert_eq!(
                after.parked_barrier_marks - before.parked_barrier_marks,
                u64::from(retained)
            );
            assert_eq!(
                after.scratch.work_pushed - before.scratch.work_pushed,
                u64::from(retained)
            );
            assert_eq!(
                after.node_slot_visits - before.node_slot_visits,
                u64::from(retained) * 2,
                "the barrier follows the table value, not the object key"
            );
            assert_eq!(after.metatable_edge_checks, before.metatable_edge_checks);
            assert_eq!(after.weak_mode_lookups, before.weak_mode_lookups);
            parked.park()?;
            Ok(old_record)
        })
        .expect("in-place object replacement while Gray is parked");

        let published = heap::step_fixed_frames(&mut host, 128).expect("publish saved mark");
        assert!(
            matches!(published, StepOutcome::Published { .. }),
            "{published:?}"
        );
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 0);
        heap::with_heap(&mut host, |heap| {
            let before_refuse = heap.object_value(owner, key)?;
            let forged =
                super::TableRef::table(heap.domain(), closure.slot(), closure.generation());
            assert!(
                heap.closure_alive(closure),
                "the closure key control is still live"
            );
            assert_eq!(
                heap.set_object_edge(stale, forged, sentinel),
                Err(HeapError::WrongKind),
                "object-key kind is checked before the owner and the sweep tail"
            );
            assert_eq!(
                heap.set_object_edge(owner, stale, sentinel),
                Err(HeapError::StaleHandle),
                "a stale object key is refused before the sweep tail"
            );
            assert_eq!(
                heap.set_object_edge(stale, key, sentinel),
                Err(HeapError::StaleHandle),
                "the owner is checked after the key and before the sweep tail"
            );
            assert_eq!(
                heap.set_object_edge(owner, key, stale),
                Err(HeapError::StaleHandle),
                "the value is checked after the key and owner"
            );
            assert_eq!(
                heap.set_object_edge(owner, key, forged),
                Err(HeapError::WrongKind),
                "a closure-shaped value is not a table edge"
            );
            assert_eq!(
                heap.set_object_edge(owner, key, sentinel),
                Err(HeapError::SweepPending),
                "a live in-place object replacement still stops at the saved tail"
            );
            assert_eq!(
                heap.set_object_edge(owner, other, sentinel),
                Err(HeapError::SweepPending)
            );
            assert!(
                heap.find_key(owner.slot(), &super::EdgeKey::object(other))
                    .is_none()
            );
            assert_eq!(
                heap.clear_object_edge(stale, key),
                Err(HeapError::StaleHandle)
            );
            assert_eq!(
                heap.clear_object_edge(owner, forged),
                Err(HeapError::WrongKind),
                "clear checks the owner, then the key kind"
            );
            assert_eq!(
                heap.clear_object_edge(owner, key),
                Err(HeapError::SweepPending)
            );
            assert_eq!(heap.object_value(owner, key)?, before_refuse);
            assert_eq!(
                heap.find_key(owner.slot(), &super::EdgeKey::object(key)),
                Some(old_record)
            );
            if matches!(write, ExistingObjectWrite::WeakValue) {
                assert_eq!(
                    heap.object_value(owner, key)?,
                    Value::Null,
                    "a weak object replacement publishes nil"
                );
                let index = heap
                    .find_key(owner.slot(), &super::EdgeKey::object(key))
                    .expect("a weak-value replacement leaves the cleared cursor");
                assert_eq!(index, old_record);
                let stored = heap.edge_slots[index as usize]
                    .edge
                    .as_ref()
                    .expect("retained gap");
                assert!(stored.value.is_cursor_gap());
                assert!(matches!(
                    stored.key,
                    super::EdgeKey::Object {
                        slot,
                        generation,
                        kind: super::HandleKind::Table,
                    } if slot == key.slot() && generation == key.generation()
                ));
            }
            if matches!(write, ExistingObjectWrite::Omitted) {
                assert_eq!(
                    heap.object_value(owner, key)?,
                    Value::string(ORIGINAL_OBJECT_SCALAR)
                );
            }
            if retained {
                assert_eq!(heap.object_value(owner, key)?, carrier);
                assert_eq!(heap.guest_class(&carrier), GuestClass::Table(target));
            }
            assert_eq!(heap.object_value(owner, other)?, Value::Null);
            Ok(())
        })
        .expect("read during publication");
        let done = heap::step_fixed_frames(&mut host, 128).expect("finish saved tail");
        assert!(matches!(done, StepOutcome::Completed(_)), "{done:?}");
        assert!(heap::fixed_step_idle(&mut host).expect("idle"));
        heap::with_heap(&mut host, |heap| {
            assert_eq!(
                heap.alive(target),
                retained,
                "an in-place strong object replacement on a scanned owner must keep its target alive"
            );
            assert_eq!(
                heap.alive(child),
                retained,
                "the child is reached only by the continued scan"
            );
            assert!(heap.alive(key) && heap.alive(other));
            assert_eq!(heap.stats().collections, 1);
            assert_eq!(heap.scan_meter().mark_passes, 1);
            if matches!(write, ExistingObjectWrite::Omitted) {
                assert_eq!(
                    heap.object_value(owner, key)?,
                    Value::string(ORIGINAL_OBJECT_SCALAR),
                    "an omitted object replacement keeps the original string"
                );
            }
            if retained {
                let value = heap.object_value(owner, key)?;
                assert_eq!(value, carrier);
                assert_eq!(heap.guest_class(&value), GuestClass::Table(target));
                assert_eq!(heap.named_edge(target, CHILD)?, Some(child));
                heap.clear_object_edge(owner, key)?;
            }
            Ok(())
        })
        .expect("same-cycle reachability");
        if retained {
            heap::collect(&mut host).expect("next collection after removing the only strong edge");
            heap::with_heap(&mut host, |heap| {
                assert!(!heap.alive(target) && !heap.alive(child));
                assert!(heap.alive(key) && heap.alive(other));
                assert_eq!(heap.object_value(owner, key)?, Value::Null);
                assert_eq!(heap.stats().collections, 2);
                Ok(())
            })
            .expect("later reclamation");
        }
    });
}

#[test]
fn gray_existing_object_strong_replacement_keeps_the_new_graph() {
    existing_object_replacement_after_owner_scan(ExistingObjectWrite::Strong);
}

#[test]
fn gray_existing_object_weak_replacement_clears_at_publication() {
    existing_object_replacement_after_owner_scan(ExistingObjectWrite::WeakValue);
}

#[test]
fn gray_existing_object_weak_key_strong_value_keeps_the_new_graph() {
    existing_object_replacement_after_owner_scan(ExistingObjectWrite::WeakKey);
}

#[test]
fn gray_existing_object_omitted_replacement_keeps_the_original_string() {
    existing_object_replacement_after_owner_scan(ExistingObjectWrite::Omitted);
}

#[derive(Clone, Copy)]
enum NewNumberWrite {
    Strong,
    WeakValue,
    Omitted,
}

/// Fresh numeric key. `-0` is the write spelling and `+0` is the lookup.
/// `EdgeKey::number` stores both as the zero bits, so a hit would be a
/// replacement. The independent string `"0"` stays a text record.
const NEW_NUMBER_WRITE: f64 = -0.0;
const NEW_NUMBER_READ: f64 = 0.0;
const NUMBER_TEXT_KEY: &str = "0";
const NUMBER_TEXT_VALUE: &str = "text";

fn number_record_indexes(heap: &LuaHeap, owner: u32) -> Vec<u32> {
    let mut found = Vec::new();
    let mut cursor = heap.slots[owner as usize].edge_head;
    while let Some(index) = cursor {
        let record = &heap.edge_slots[index as usize];
        cursor = record.next;
        if record
            .edge
            .as_ref()
            .is_some_and(|edge| matches!(edge.key, super::EdgeKey::Number(_)))
        {
            found.push(index);
        }
    }
    found
}

fn assert_fresh_number_key(heap: &LuaHeap, owner: u32) {
    let written = super::EdgeKey::number(NEW_NUMBER_WRITE);
    let read = super::EdgeKey::number(NEW_NUMBER_READ);
    let text = super::EdgeKey::named(NUMBER_TEXT_KEY);
    assert!(NEW_NUMBER_WRITE.is_sign_negative());
    assert_eq!(NEW_NUMBER_WRITE, 0.0);
    assert_ne!(NEW_NUMBER_WRITE.to_bits(), NEW_NUMBER_READ.to_bits());
    assert_eq!(written, read);
    assert!(matches!(written, super::EdgeKey::Number(0)));
    assert!(matches!(text, super::EdgeKey::Utf8(ref stored) if stored == NUMBER_TEXT_KEY));
    assert_ne!(written, text);
    assert_eq!(
        heap.find_key(owner, &written),
        None,
        "the numeric slot is missing before the write"
    );
    assert!(
        number_record_indexes(heap, owner).is_empty(),
        "no number record is present before the write"
    );
}

/// Inserts an unmatched ordinary number key after the owner has been scanned.
fn new_number_insert_after_owner_scan(write: NewNumberWrite) {
    use crate::heap::{self, GuestClass, StepOutcome};

    const CHILD: &str = "child";
    const BLOCKED_KEY: f64 = 11.0;
    let retained = matches!(write, NewNumberWrite::Strong);
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(8, 2, 8)).expect("install");
        let raw = crate::publish_raw(&mut host);
        let target = crate::table_ref(&host, raw);
        let (owner, sentinel, child, stale, carrier) = heap::with_heap(&mut host, |heap| {
            let owner = heap.alloc()?;
            let sentinel = heap.alloc()?;
            let child = heap.alloc()?;
            let unrelated = heap.alloc()?; // Forces a real published tail in all arms.
            let stale = heap.alloc()?;
            heap.rollback(stale)?;
            if matches!(write, NewNumberWrite::WeakValue) {
                let meta = heap.alloc()?;
                heap.set_mode_scalar(meta, Value::string("v"))?;
                heap.set_metatable(owner, Some(meta))?;
            }
            let frame = heap.enter_roots(2)?;
            // The Gray stack pops the owner first, then the sentinel.
            heap.write_root(frame, 0, sentinel)?;
            heap.write_root(frame, 1, owner)?;
            heap.set_named_scalar(owner, NUMBER_TEXT_KEY, Value::string(NUMBER_TEXT_VALUE))?;
            heap.set_named_scalar(sentinel, "pending", Value::Bool(true))?;
            heap.set_named_edge(target, CHILD, unrelated)?;
            heap.set_named_edge(target, CHILD, child)?;
            assert_eq!(heap.named_edge(target, CHILD)?, Some(child));
            assert_eq!(
                heap.named_value(owner, NUMBER_TEXT_KEY)?,
                Value::string(NUMBER_TEXT_VALUE)
            );
            assert_eq!(heap.number_value(owner, NEW_NUMBER_READ)?, Value::Null);
            assert_fresh_number_key(heap, owner.slot());
            Ok((owner, sentinel, child, stale, heap.issue_carrier(target)?))
        })
        .expect("graph");
        heap::open_fixed_step(&mut host).expect("start a saved mark");
        heap::with_heap(&mut host, |heap| {
            let (_, text_record) = heap
                .locate_named(owner.slot(), NUMBER_TEXT_KEY)
                .expect("text field");
            let (_, sentinel_record) = heap
                .locate_named(sentinel.slot(), "pending")
                .expect("sentinel field");
            let mut parked = ParkedMark::resume(heap).expect("resume saved mark");
            pause_at(&mut parked, owner.slot(), text_record);
            let before_owner_edge = parked.meter().outgoing_edge_checks;
            let pending = pause_at(&mut parked, sentinel.slot(), sentinel_record);
            assert_eq!(pending.owner, sentinel.slot());
            assert!(
                parked.meter().outgoing_edge_checks > before_owner_edge,
                "the owner scan charged its text edge"
            );
            assert!(parked.marked(owner), "the scanned owner is already marked");
            assert!(
                !parked.marked(target) && !parked.marked(child),
                "the new target has no supplemental root"
            );
            let before = parked.meter();
            parked.park()?;
            assert_fresh_number_key(heap, owner.slot());
            let edges_before = (heap.stats().live_edges, heap.stats().edge_slots);
            if !matches!(write, NewNumberWrite::Omitted) {
                heap.set_number_edge(owner, NEW_NUMBER_WRITE, target)?;
                assert_eq!(heap.number_value(owner, NEW_NUMBER_READ)?, carrier);
                assert_eq!(
                    (heap.stats().live_edges, heap.stats().edge_slots),
                    (edges_before.0 + 1, edges_before.1 + 1),
                    "the new number key adds one record"
                );
                let indexes = number_record_indexes(heap, owner.slot());
                assert_eq!(indexes.len(), 1, "the same number key has one record");
                let index = indexes[0];
                assert_eq!(
                    heap.find_key(owner.slot(), &super::EdgeKey::number(NEW_NUMBER_READ)),
                    Some(index)
                );
                assert_eq!(
                    heap.find_key(owner.slot(), &super::EdgeKey::number(NEW_NUMBER_WRITE)),
                    Some(index)
                );
                let stored = heap.edge_slots[index as usize]
                    .edge
                    .as_ref()
                    .expect("inserted number record");
                assert!(matches!(stored.key, super::EdgeKey::Number(0)));
                assert!(!stored.value.is_cursor_gap());
                assert!(matches!(
                    stored.value,
                    super::EdgeValue::Table { slot, generation }
                        if slot == target.slot() && generation == target.generation()
                ));
            } else {
                assert_eq!(
                    (heap.stats().live_edges, heap.stats().edge_slots),
                    edges_before,
                    "omission adds no record"
                );
                assert_fresh_number_key(heap, owner.slot());
            }
            assert_eq!(
                heap.named_value(owner, NUMBER_TEXT_KEY)?,
                Value::string(NUMBER_TEXT_VALUE),
                "the independent string key keeps its original value"
            );
            let parked = ParkedMark::resume(heap).expect("write kept the saved mark");
            assert_eq!(parked.phase_name(), "gray");
            let cursor = parked.pending().expect("sentinel still pending");
            assert_eq!(
                (cursor.owner, cursor.cursor),
                (pending.owner, pending.cursor),
                "the pending scan cursor is unchanged"
            );
            assert!(
                !parked.marked(child),
                "the write must not scan the child edge"
            );
            assert_eq!(
                parked.marked(target),
                retained,
                "only a strong new number key enters the gray queue immediately"
            );
            let after = parked.meter();
            assert_write_did_not_scan(&before, &after);
            assert_eq!(after.parked_edge_writes, before.parked_edge_writes);
            assert_eq!(
                after.parked_barrier_marks - before.parked_barrier_marks,
                u64::from(retained)
            );
            assert_eq!(
                after.scratch.work_pushed - before.scratch.work_pushed,
                u64::from(retained)
            );
            assert_eq!(
                after.node_slot_visits - before.node_slot_visits,
                u64::from(retained) * 2
            );
            assert_eq!(after.metatable_edge_checks, before.metatable_edge_checks);
            assert_eq!(after.weak_mode_lookups, before.weak_mode_lookups);
            assert_eq!(after.weak_mode_slot_checks, before.weak_mode_slot_checks);
            parked.park()?;
            Ok(())
        })
        .expect("fresh number insert while Gray is parked");

        let published = heap::step_fixed_frames(&mut host, 128).expect("publish saved mark");
        assert!(
            matches!(published, StepOutcome::Published { .. }),
            "{published:?}"
        );
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 0);
        heap::with_heap(&mut host, |heap| {
            // NaN and a stale handle stay ahead of the sweep refusal.
            assert_eq!(
                heap.set_number_edge(owner, f64::NAN, sentinel),
                Err(HeapError::NanIndex)
            );
            assert_eq!(
                heap.clear_number_edge(owner, f64::NAN),
                Err(HeapError::NanIndex)
            );
            assert_eq!(
                heap.set_number_edge(stale, NEW_NUMBER_WRITE, sentinel),
                Err(HeapError::StaleHandle)
            );
            assert_eq!(
                heap.set_number_edge(owner, NEW_NUMBER_WRITE, stale),
                Err(HeapError::StaleHandle)
            );
            assert_eq!(
                heap.set_number_edge(owner, BLOCKED_KEY, sentinel),
                Err(HeapError::SweepPending)
            );
            assert!(
                heap.find_key(owner.slot(), &super::EdgeKey::number(BLOCKED_KEY))
                    .is_none()
            );
            assert_eq!(
                heap.named_value(owner, NUMBER_TEXT_KEY)?,
                Value::string(NUMBER_TEXT_VALUE)
            );
            if matches!(write, NewNumberWrite::WeakValue) {
                assert_eq!(
                    heap.number_value(owner, NEW_NUMBER_READ)?,
                    Value::Null,
                    "a weak number insert publishes nil"
                );
                let indexes = number_record_indexes(heap, owner.slot());
                assert_eq!(indexes.len(), 1, "a weak insert leaves one cleared cursor");
                let stored = heap.edge_slots[indexes[0] as usize]
                    .edge
                    .as_ref()
                    .expect("retained gap");
                assert!(stored.value.is_cursor_gap());
                assert!(matches!(stored.key, super::EdgeKey::Number(0)));
            }
            if matches!(write, NewNumberWrite::Omitted) {
                assert_eq!(heap.number_value(owner, NEW_NUMBER_READ)?, Value::Null);
                assert!(number_record_indexes(heap, owner.slot()).is_empty());
            }
            if matches!(write, NewNumberWrite::Strong) {
                assert_eq!(heap.number_value(owner, NEW_NUMBER_READ)?, carrier);
            }
            Ok(())
        })
        .expect("read during publication");
        let done = heap::step_fixed_frames(&mut host, 128).expect("finish saved tail");
        assert!(matches!(done, StepOutcome::Completed(_)), "{done:?}");
        assert!(heap::fixed_step_idle(&mut host).expect("idle"));
        heap::with_heap(&mut host, |heap| {
            assert_eq!(
                heap.alive(target),
                retained,
                "a fresh strong number key on a scanned owner must keep its target alive"
            );
            assert_eq!(
                heap.alive(child),
                retained,
                "the child is reached only by the continued scan"
            );
            assert_eq!(heap.stats().collections, 1);
            assert_eq!(heap.scan_meter().mark_passes, 1);
            assert_eq!(
                heap.named_value(owner, NUMBER_TEXT_KEY)?,
                Value::string(NUMBER_TEXT_VALUE)
            );
            if matches!(write, NewNumberWrite::Omitted) {
                assert_eq!(heap.number_value(owner, NEW_NUMBER_READ)?, Value::Null);
            }
            if retained {
                let value = heap.number_value(owner, NEW_NUMBER_READ)?;
                assert_eq!(value, carrier);
                assert_eq!(heap.guest_class(&value), GuestClass::Table(target));
                assert_eq!(heap.named_edge(target, CHILD)?, Some(child));
                heap.clear_number_edge(owner, NEW_NUMBER_READ)?;
            }
            Ok(())
        })
        .expect("same-cycle reachability");
        if retained {
            heap::collect(&mut host).expect("next collection after removing the only strong edge");
            heap::with_heap(&mut host, |heap| {
                assert!(!heap.alive(target) && !heap.alive(child));
                assert_eq!(heap.stats().collections, 2);
                Ok(())
            })
            .expect("later reclamation");
        }
    });
}

#[test]
fn gray_new_number_strong_insert_keeps_target_and_descendant() {
    new_number_insert_after_owner_scan(NewNumberWrite::Strong);
}

#[test]
fn gray_new_number_weak_insert_clears_at_publication() {
    new_number_insert_after_owner_scan(NewNumberWrite::WeakValue);
}

#[test]
fn gray_new_number_write_omitted_leaves_target_unreachable() {
    new_number_insert_after_owner_scan(NewNumberWrite::Omitted);
}

/// Node and edge caps refuse before a parked number insert can enqueue.
#[test]
fn gray_new_number_insert_refuses_at_node_and_edge_limits() {
    use crate::heap::{self, StepOutcome};

    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(3, 2, 2)).expect("install");
        let raw = crate::publish_raw(&mut host);
        let target = crate::table_ref(&host, raw);
        let (owner, sentinel) = heap::with_heap(&mut host, |heap| {
            let owner = heap.alloc()?;
            let sentinel = heap.alloc()?;
            let frame = heap.enter_roots(2)?;
            heap.write_root(frame, 0, sentinel)?;
            heap.write_root(frame, 1, owner)?;
            heap.set_named_scalar(owner, NUMBER_TEXT_KEY, Value::string(NUMBER_TEXT_VALUE))?;
            heap.set_named_scalar(sentinel, "pending", Value::Bool(true))?;
            assert_eq!(heap.stats().node_slots, 3);
            assert_eq!(heap.stats().edge_slots, 2);
            assert_fresh_number_key(heap, owner.slot());
            Ok((owner, sentinel))
        })
        .expect("tight graph");
        heap::open_fixed_step(&mut host).expect("start a saved mark");
        heap::with_heap(&mut host, |heap| {
            let (_, text_record) = heap
                .locate_named(owner.slot(), NUMBER_TEXT_KEY)
                .expect("text field");
            let (_, sentinel_record) = heap
                .locate_named(sentinel.slot(), "pending")
                .expect("sentinel field");
            let mut parked = ParkedMark::resume(heap).expect("resume saved mark");
            pause_at(&mut parked, owner.slot(), text_record);
            let pending = pause_at(&mut parked, sentinel.slot(), sentinel_record);
            assert!(parked.marked(owner));
            assert!(!parked.marked(target));
            let before = parked.meter();
            parked.park()?;
            assert_eq!(
                heap.set_number_edge(owner, f64::NAN, target),
                Err(HeapError::NanIndex)
            );
            assert_eq!(
                heap.set_number_edge(owner, NEW_NUMBER_WRITE, target),
                Err(HeapError::EdgeCapacity)
            );
            assert_eq!(heap.alloc(), Err(HeapError::NodeCapacity));
            assert_fresh_number_key(heap, owner.slot());
            assert_eq!(
                heap.named_value(owner, NUMBER_TEXT_KEY)?,
                Value::string(NUMBER_TEXT_VALUE)
            );
            let parked = ParkedMark::resume(heap).expect("refusal kept the saved mark");
            assert_eq!(parked.phase_name(), "gray");
            let cursor = parked.pending().expect("sentinel still pending");
            assert_eq!(
                (cursor.owner, cursor.cursor),
                (pending.owner, pending.cursor)
            );
            assert!(!parked.marked(target), "a refused insert does not enqueue");
            let after = parked.meter();
            assert_write_did_not_scan(&before, &after);
            assert_eq!(after.parked_barrier_marks, before.parked_barrier_marks);
            assert_eq!(after.scratch.work_pushed, before.scratch.work_pushed);
            assert_eq!(after.metatable_edge_checks, before.metatable_edge_checks);
            assert_eq!(after.weak_mode_lookups, before.weak_mode_lookups);
            assert_eq!(after.weak_mode_slot_checks, before.weak_mode_slot_checks);
            parked.park()?;
            Ok(())
        })
        .expect("capacity refusal while Gray is parked");
        let published = heap::step_fixed_frames(&mut host, 128).expect("publish");
        assert!(matches!(published, StepOutcome::Published { .. }));
        let done = heap::step_fixed_frames(&mut host, 128).expect("finish");
        assert!(matches!(done, StepOutcome::Completed(_)), "{done:?}");
        heap::with_heap(&mut host, |heap| {
            assert!(!heap.alive(target), "the refused target is collected");
            assert!(heap.alive(owner));
            assert_eq!(
                heap.named_value(owner, NUMBER_TEXT_KEY)?,
                Value::string(NUMBER_TEXT_VALUE)
            );
            assert!(number_record_indexes(heap, owner.slot()).is_empty());
            assert_eq!(heap.stats().collections, 1);
            Ok(())
        })
        .expect("refused target did not survive");
    });
}

#[test]
fn tail_slot_reuse_does_not_keep_the_new_target() {
    let mut heap = LuaHeap::new(limits(8, 2, 8));
    let a = heap.alloc().expect("A");
    let x = heap.alloc().expect("X");
    let y = heap.alloc().expect("Y");
    let d = heap.alloc().expect("D");
    let w = heap.alloc().expect("W");
    let frame = heap.enter_roots(1).expect("frame");
    heap.write_root(frame, 0, a).expect("root A");
    heap.set_edge(a, KEY_X, x).expect("A.X");
    heap.set_edge(a, KEY_Y, y).expect("A.Y");
    let x_slot = x.slot();
    let x_generation = x.generation();
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    let y_index = parked.field_index(a, KEY_Y).expect("tail record");
    pause_at(&mut parked, a.slot(), y_index);
    parked.delete_field(a, KEY_Y).expect("delete tail");
    parked
        .write_field(d, KEY_W, w)
        .expect("reuse the tail record");
    assert_eq!(parked.field_index(d, KEY_W), Some(y_index));
    assert_ne!(d.slot(), a.slot());
    let pending_after = parked.pending();
    let repairs = parked.meter().parked_cursor_repairs;
    finish(&mut parked);
    parked.complete().expect("sweep");
    drop(parked);
    assert!(
        !heap.alive(w),
        "a tail cursor must not follow the reused slot; pending was {pending_after:?}, repairs {repairs}"
    );
    assert_eq!(heap.edge(a, KEY_X).expect("X edge"), Some(x));
    assert_eq!(x.slot(), x_slot);
    assert_eq!(x.generation(), x_generation);
    assert!(!heap.alive(y));
    assert!(pending_after.is_none(), "no successor ends A's scan");
    let meter = heap.take_scan_meter();
    assert_eq!(meter.parked_cursor_repairs, 1);
    assert_eq!(meter.mark_passes, 1);
    assert_eq!(heap.stats().collections, 1);
}

#[test]
fn closed_phase_refuses_before_a_write() {
    let mut heap = LuaHeap::new(limits(4, 2, 4));
    let owner = heap.alloc().expect("owner");
    let frame = heap.enter_roots(1).expect("frame");
    heap.write_root(frame, 0, owner).expect("root");
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    let idle = parked.pulse(0);
    assert!(!idle.finished);
    assert_eq!(idle.meter, ScanMeter::default());
    assert_eq!(parked.phase_name(), "start");
    let before = picture(&parked);
    assert_eq!(
        parked.delete_field(owner, KEY_X),
        Err(HeapError::MarkDenied)
    );
    assert_eq!(
        parked.write_field(owner, KEY_X, owner),
        Err(HeapError::MarkDenied)
    );
    assert_same(&before, &picture(&parked));
    let step = parked.pulse(1);
    assert!(!step.finished);
    assert_eq!(parked.phase_name(), "reset");
    let before = picture(&parked);
    assert_eq!(
        parked.delete_field(owner, KEY_X),
        Err(HeapError::MarkDenied)
    );
    assert_same(&before, &picture(&parked));
    parked.cancel();

    let mut heap = LuaHeap::new(limits(4, 2, 4));
    let mt = heap.alloc().expect("metatable");
    let owner = heap.alloc().expect("owner");
    let mode = "v\0".to_string();
    let frame = heap.enter_roots(2).expect("frame");
    heap.write_root(frame, 0, mt).expect("root mt");
    heap.write_root(frame, 1, owner).expect("root owner");
    heap.set_metatable(owner, Some(mt)).expect("metatable");
    heap.set_mode_scalar(mt, Value::string(mode.clone()))
        .expect("mode");
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    let mut saw_mode = false;
    for _ in 0..HARNESS_BOUND {
        let view = parked.pulse(1);
        assert!(!view.finished);
        if matches!(
            parked.phase_name(),
            "mode-open" | "mode-bytes" | "mode-init"
        ) {
            saw_mode = true;
            break;
        }
    }
    assert!(saw_mode, "a mode string must open a mode phase");
    assert!(!parked.mutation_open());
    let before = picture(&parked);
    let stored = parked.field_scalar(mt, LuaHeap::MODE_FIELD).expect("mode");
    assert_eq!(string_bytes(&stored), Some(mode.as_bytes()));
    let mt_ref = parked.metatable(owner).expect("mt read");
    assert_eq!(
        parked.delete_field(owner, KEY_X),
        Err(HeapError::MarkDenied)
    );
    assert_eq!(
        parked.write_field(owner, KEY_X, owner),
        Err(HeapError::MarkDenied)
    );
    assert_eq!(parked.deny(PausedOp::Mode), Err(HeapError::MarkDenied));
    assert_same(&before, &picture(&parked));
    let stored = parked.field_scalar(mt, LuaHeap::MODE_FIELD).expect("mode");
    assert_eq!(string_bytes(&stored), Some(mode.as_bytes()));
    assert_eq!(parked.metatable(owner).expect("mt read"), mt_ref);
    parked.cancel();
}

#[test]
fn open_phase_refuses_marked_owner_and_forbidden_ops() {
    let mut heap = LuaHeap::new(limits(8, 2, 8));
    let chain = chain(&mut heap);
    let extra = heap.alloc().expect("extra");
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    let y_index = parked.field_index(chain.a, KEY_Y).expect("Y record");
    pause_at(&mut parked, chain.a.slot(), y_index);
    assert!(parked.marked(chain.a));
    assert!(!parked.write_allowed(chain.a));
    let before = picture(&parked);
    let y_edge = parked.edge(chain.a, KEY_Y).expect("Y edge");
    assert_eq!(
        parked.write_field(chain.a, 4, extra),
        Err(HeapError::MarkDenied),
        "a marked owner cannot publish a new edge"
    );
    assert_eq!(
        parked.write_field(chain.a, LuaHeap::MODE_FIELD, extra),
        Err(HeapError::MarkDenied)
    );
    assert_eq!(
        parked.delete_field(chain.a, LuaHeap::MODE_FIELD),
        Err(HeapError::MarkDenied)
    );
    for op in [
        PausedOp::Alloc,
        PausedOp::Rollback,
        PausedOp::Metatable,
        PausedOp::Mode,
        PausedOp::Root,
        PausedOp::Token,
    ] {
        assert_eq!(parked.deny(op), Err(HeapError::MarkDenied));
    }
    assert_eq!(
        parked.ordinary_collect().expect_err("private collect"),
        HeapError::MarkActive
    );
    assert_eq!(
        parked.restart().expect_err("second start"),
        HeapError::MarkActive
    );
    assert_same(&before, &picture(&parked));
    assert_eq!(parked.edge(chain.a, KEY_Y).expect("Y edge"), y_edge);
    assert_eq!(parked.edge(chain.a, 4).expect("new edge"), None);
    assert_eq!(parked.edge(extra, KEY_X).expect("extra"), None);
    parked.cancel();
    assert_eq!(heap.stats().collections, 0);
    assert!(heap.alive(chain.a));
    assert!(heap.alive(chain.y));
    assert!(heap.alive(extra));
}

/// Mark bits and gray pushes the refusal must not change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GraphGuards {
    owner_marked: bool,
    successor_marked: bool,
    intruder_marked: bool,
    work_pushed: u64,
    barrier_marks: u64,
    scanning: Option<u32>,
}

struct CellRefusal {
    phase_open: bool,
    owner_marked: bool,
    result: Result<(), HeapError>,
    edge: Result<Option<TableRef>, HeapError>,
    index_before: Option<u32>,
    index_after: Option<u32>,
    pending_before: (u32, u32),
    pending_after: Option<(u32, u32)>,
    guards_before: GraphGuards,
    guards_after: GraphGuards,
    meter_before: ScanMeter,
    meter_after: ScanMeter,
    stats_before: HeapStats,
    stats_after: HeapStats,
    bytes_before: u64,
    bytes_after: u64,
    cell: UpvalueRef,
    successor: TableRef,
    kept: Option<KeptCell>,
}

struct KeptCell {
    cell_edge: Result<UpvalueRef, HeapError>,
    current: Result<Option<TableRef>, HeapError>,
    owner_alive: bool,
    cell_alive: bool,
    successor_alive: bool,
    intruder_alive: bool,
    orphan_alive: bool,
}

fn graph_guards(
    parked: &ParkedMark<'_>,
    owner: TableRef,
    successor: TableRef,
    intruder: TableRef,
) -> GraphGuards {
    GraphGuards {
        owner_marked: parked.marked(owner),
        successor_marked: parked.marked(successor),
        intruder_marked: parked.marked(intruder),
        work_pushed: parked.meter().scratch.work_pushed,
        barrier_marks: parked.meter().parked_barrier_marks,
        scanning: parked.scanning_owner(),
    }
}

/// One fresh heap. A write on this heap cannot change the delete heap.
fn cell_edge_refusal(write: bool) -> CellRefusal {
    let mut heap = LuaHeap::new(limits(8, 2, 8));
    let owner = heap.alloc().expect("owner");
    let successor = heap.alloc().expect("successor");
    let intruder = heap.alloc().expect("intruder");
    let orphan = heap.alloc().expect("orphan");
    let cell = heap.alloc_upvalue().expect("cell");
    heap.write_upvalue(cell, successor).expect("current");
    heap.set_cell_edge(owner, cell).expect("cell edge");
    assert_eq!(heap.cell_edge(owner).expect("stored cell"), cell);
    let frame = heap.enter_roots(1).expect("frame");
    heap.write_root(frame, 0, owner).expect("root owner");
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    let index = parked
        .field_index(owner, super::CELL_EDGE)
        .expect("cell record");
    let pending = pause_at(&mut parked, owner.slot(), index);
    let phase_open = parked.mutation_open() && parked.phase_name() == "gray";
    let owner_marked = parked.marked(owner);
    assert!(
        phase_open,
        "cell edge refusal must run while mutation is open"
    );
    assert!(owner_marked, "the owner is already marked");
    assert!(
        !parked.marked(successor),
        "the cell edge has not been charged"
    );
    assert_eq!(
        parked
            .edge(owner, super::CELL_EDGE)
            .expect_err("cell edge is an upvalue"),
        HeapError::WrongKind
    );
    let guards_before = graph_guards(&parked, owner, successor, intruder);
    let before = picture(&parked);
    let result = if write {
        parked.write_field(owner, super::CELL_EDGE, intruder)
    } else {
        parked.delete_field(owner, super::CELL_EDGE)
    };
    let edge = parked.edge(owner, super::CELL_EDGE);
    let index_after = parked.field_index(owner, super::CELL_EDGE);
    let guards_after = graph_guards(&parked, owner, successor, intruder);
    let pending_after = parked.pending().map(|scan| (scan.owner, scan.cursor));
    let after = picture(&parked);
    let kept = if result.is_err() {
        finish(&mut parked);
        parked.complete().expect("sweep");
        drop(parked);
        Some(KeptCell {
            cell_edge: heap.cell_edge(owner),
            current: heap.current_table(cell),
            owner_alive: heap.alive(owner),
            cell_alive: heap.upvalue_alive(cell),
            successor_alive: heap.alive(successor),
            intruder_alive: heap.alive(intruder),
            orphan_alive: heap.alive(orphan),
        })
    } else {
        drop(parked);
        None
    };
    CellRefusal {
        phase_open,
        owner_marked,
        result,
        edge,
        index_before: Some(index),
        index_after,
        pending_before: (pending.owner, pending.cursor),
        pending_after,
        guards_before,
        guards_after,
        meter_before: before.meter,
        meter_after: after.meter,
        stats_before: before.stats,
        stats_after: after.stats,
        bytes_before: before.bytes,
        bytes_after: after.bytes,
        cell,
        successor,
        kept,
    }
}

#[test]
fn open_phase_refuses_marked_owner_cell_edge() {
    let write = cell_edge_refusal(true);
    let delete = cell_edge_refusal(false);
    assert!(
        write.phase_open && write.owner_marked && delete.phase_open && delete.owner_marked,
        "both refusals run on a marked owner in an open phase"
    );
    assert_eq!(
        (&write.result, &delete.result),
        (&Err(HeapError::MarkDenied), &Err(HeapError::MarkDenied)),
        "write edge {:?} index {:?} -> {:?}; delete edge {:?} index {:?} -> {:?}",
        write.edge,
        write.index_before,
        write.index_after,
        delete.edge,
        delete.index_before,
        delete.index_after,
    );
    assert_eq!(
        write.guards_before, write.guards_after,
        "write graph guards"
    );
    assert_eq!(
        delete.guards_before, delete.guards_after,
        "delete graph guards"
    );
    assert_eq!(
        write.pending_after,
        Some(write.pending_before),
        "write pending cursor"
    );
    assert_eq!(
        delete.pending_after,
        Some(delete.pending_before),
        "delete pending cursor"
    );
    assert_eq!(write.index_after, write.index_before, "write record");
    assert_eq!(delete.index_after, delete.index_before, "delete record");
    assert_eq!(write.meter_before, write.meter_after, "write meter");
    assert_eq!(delete.meter_before, delete.meter_after, "delete meter");
    assert_eq!(write.stats_before, write.stats_after, "write stats");
    assert_eq!(delete.stats_before, delete.stats_after, "delete stats");
    assert_eq!(write.bytes_before, write.bytes_after, "write bytes");
    assert_eq!(delete.bytes_before, delete.bytes_after, "delete bytes");
    let write_kept = write.kept.expect("write swept after refusal");
    let delete_kept = delete.kept.expect("delete swept after refusal");
    assert_eq!(
        write_kept.cell_edge,
        Ok(write.cell),
        "write kept the upvalue"
    );
    assert_eq!(
        delete_kept.cell_edge,
        Ok(delete.cell),
        "delete kept the upvalue"
    );
    assert_eq!(
        write_kept.current,
        Ok(Some(write.successor)),
        "write kept the cell current"
    );
    assert_eq!(
        delete_kept.current,
        Ok(Some(delete.successor)),
        "delete kept the cell current"
    );
    assert!(write_kept.owner_alive && write_kept.cell_alive && write_kept.successor_alive);
    assert!(delete_kept.owner_alive && delete_kept.cell_alive && delete_kept.successor_alive);
    assert!(!write_kept.intruder_alive && !write_kept.orphan_alive);
    assert!(!delete_kept.intruder_alive && !delete_kept.orphan_alive);
}

#[test]
fn cancel_restarts_a_full_collect() {
    let mut heap = LuaHeap::new(limits(8, 2, 8));
    let chain = chain(&mut heap);
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    let y_index = parked.field_index(chain.a, KEY_Y).expect("Y record");
    pause_at(&mut parked, chain.a.slot(), y_index);
    parked.cancel();
    assert_eq!(heap.stats().collections, 0);
    assert!(heap.alive(chain.y), "cancel does not sweep");
    let _partial = heap.take_scan_meter();
    heap.collect();
    let meter = heap.take_scan_meter();
    assert_eq!(meter.mark_passes, 1, "the next collect starts a new pass");
    assert_eq!(meter.parked_cursor_repairs, 0);
    assert_eq!(meter.parked_edge_writes, 0);
    assert!(heap.alive(chain.a));
    assert!(heap.alive(chain.x));
    assert!(heap.alive(chain.y));
    assert!(heap.alive(chain.z));
    assert!(!heap.alive(chain.d));
    assert!(!heap.alive(chain.w));
    assert_eq!(heap.edge(chain.a, KEY_Y).expect("Y edge"), Some(chain.y));

    let mut again = ParkedMark::start(&mut heap).expect("restart after cancel");
    let idle = again.pulse(0);
    assert!(!idle.finished);
    assert_eq!(idle.meter.mark_passes, 0);
    assert_eq!(idle.meter, ScanMeter::default());
    again.cancel();
}

#[test]
fn zero_budget_does_not_read_or_finish() {
    let mut heap = LuaHeap::new(limits(4, 2, 4));
    let owner = heap.alloc().expect("owner");
    let frame = heap.enter_roots(1).expect("frame");
    heap.write_root(frame, 0, owner).expect("root");
    heap.set_edge(owner, KEY_X, owner).expect("edge");
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    let idle = parked.pulse(0);
    assert!(!idle.finished);
    assert_eq!(idle.meter, ScanMeter::default());
    assert_eq!(parked.phase_name(), "start");
    assert!(parked.pending().is_none());
    assert_eq!(parked.complete(), Err(HeapError::MarkDenied));
    assert_eq!(parked.stats().collections, 0);
    assert_eq!(parked.meter().sweep_edge_checks, 0);
    parked.cancel();
}

#[test]
fn small_budget_stays_inside_the_pulse() {
    let edges = 8u32;
    let budget = 3u64;
    let mut heap = LuaHeap::new(limits(2, 1, edges));
    let owner = heap.alloc().expect("owner");
    let frame = heap.enter_roots(1).expect("frame");
    heap.write_root(frame, 0, owner).expect("root");
    for key in 1..=edges {
        heap.set_edge(owner, key, owner).expect("edge");
    }
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    let first = parked.pulse(budget);
    assert!(!first.finished, "budget {budget} must not walk the owner");
    assert!(first.meter.outgoing_edge_checks <= budget);
    assert_eq!(first.meter.sweep_edge_checks, 0);
    assert_eq!(parked.complete(), Err(HeapError::MarkDenied));
    assert_eq!(parked.stats().collections, 0);
    let mut pulses = 0u32;
    loop {
        let before = parked.meter();
        let view = parked.pulse(budget);
        let edge_delta = (view.meter.outgoing_edge_checks + view.meter.outgoing_gap_checks)
            - (before.outgoing_edge_checks + before.outgoing_gap_checks);
        assert!(edge_delta <= budget);
        assert_eq!(view.meter.sweep_edge_checks, 0);
        assert_eq!(view.meter.mark_passes, 1);
        assert_eq!(view.meter.parked_edge_writes, 0);
        assert_eq!(view.meter.parked_cursor_repairs, 0);
        if view.finished {
            break;
        }
        pulses += 1;
        assert!(pulses < HARNESS_BOUND);
    }
    let report = parked.complete().expect("sweep after the mark");
    drop(parked);
    let parked_meter = heap.take_scan_meter();
    assert_eq!(parked_meter.outgoing_edge_checks, u64::from(edges));
    assert_eq!(parked_meter.mark_passes, 1);
    assert_eq!(parked_meter.parked_edge_writes, 0);
    assert_eq!(parked_meter.parked_cursor_repairs, 0);
    assert_eq!(report.live_nodes, 1);

    let mut twin = LuaHeap::new(limits(2, 1, edges));
    let twin_owner = twin.alloc().expect("twin");
    let frame = twin.enter_roots(1).expect("frame");
    twin.write_root(frame, 0, twin_owner).expect("root");
    for key in 1..=edges {
        twin.set_edge(twin_owner, key, twin_owner).expect("edge");
    }
    let twin_report = twin.collect();
    let twin_meter = twin.take_scan_meter();
    assert_eq!(parked_meter, twin_meter);
    assert_eq!(report.marked_nodes, twin_report.marked_nodes);
    assert_eq!(report.live_nodes, twin_report.live_nodes);
}

#[test]
fn weak_graph_budget_one_matches_full_collect() {
    fn build(heap: &mut LuaHeap) -> (TableRef, TableRef) {
        let mt = heap.alloc().expect("metatable");
        let owner = heap.alloc().expect("owner");
        let orphan = heap.alloc().expect("orphan");
        let frame = heap.enter_roots(2).expect("frame");
        heap.write_root(frame, 0, mt).expect("root mt");
        heap.write_root(frame, 1, owner).expect("root owner");
        heap.set_metatable(owner, Some(mt)).expect("metatable");
        heap.set_mode_scalar(mt, Value::string("v\0".to_string()))
            .expect("mode");
        heap.set_edge(owner, KEY_X, orphan).expect("weak value");
        heap.set_edge(orphan, KEY_X, orphan).expect("cycle");
        (owner, orphan)
    }

    let mut heap = LuaHeap::new(limits(8, 4, 8));
    let (owner, orphan) = build(&mut heap);
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    let mark = finish(&mut parked);
    assert_eq!(mark.parked_edge_writes, 0);
    assert_eq!(mark.parked_cursor_repairs, 0);
    assert_eq!(mark.weak_mode_bytes, 2, "v and the NUL, not a second walk");
    let report = parked.complete().expect("sweep");
    drop(parked);
    assert!(heap.alive(owner));
    assert!(
        !heap.alive(orphan),
        "a weak value with no other root is collected"
    );
    let parked_meter = heap.take_scan_meter();

    let mut twin = LuaHeap::new(limits(8, 4, 8));
    let (twin_owner, twin_orphan) = build(&mut twin);
    let twin_report = twin.collect();
    assert!(twin.alive(twin_owner));
    assert!(!twin.alive(twin_orphan));
    assert_eq!(twin.take_scan_meter(), parked_meter);
    assert_eq!(twin_report.live_nodes, report.live_nodes);
    assert_eq!(twin_report.marked_nodes, report.marked_nodes);
    assert_eq!(parked_meter.parked_cursor_repairs, 0);
    assert_eq!(parked_meter.parked_edge_writes, 0);
    assert_eq!(parked_meter.mark_passes, 1);
}

const KEY_KEPT: u32 = 1;
const KEY_REPLACED: u32 = 3;
const KEY_SUCCESSOR: u32 = 4;
const PASSED_SLOT: u32 = 1;

#[derive(Clone, Copy)]
struct Cycle {
    mt: TableRef,
    watch: TableRef,
    p: TableRef,
    q: TableRef,
    r: TableRef,
    orphan: TableRef,
}

fn plant_cycle(heap: &mut LuaHeap) -> Cycle {
    let mt = heap.alloc().expect("metatable");
    let watch = heap.alloc().expect("weak watch");
    let p = heap.alloc().expect("P");
    let q = heap.alloc().expect("Q");
    let r = heap.alloc().expect("R");
    let orphan = heap.alloc().expect("orphan");
    heap.set_mode_scalar(mt, Value::string("v\0"))
        .expect("weak values");
    heap.set_metatable(watch, Some(mt))
        .expect("watch metatable");
    heap.set_edge(watch, KEY_KEPT, p).expect("weak P");
    heap.set_edge(watch, KEY_REPLACED, orphan)
        .expect("weak orphan");
    heap.set_edge(p, KEY_KEPT, q).expect("P to Q");
    heap.set_edge(q, KEY_KEPT, r).expect("Q to R");
    heap.set_edge(r, KEY_KEPT, p).expect("R to P");
    Cycle {
        mt,
        watch,
        p,
        q,
        r,
        orphan,
    }
}

fn pause_after_root_visits(parked: &mut ParkedMark<'_>, visits: u64) {
    for _ in 0..HARNESS_BOUND {
        let view = parked.pulse(1);
        assert!(!view.finished, "mark finished before {visits} root visits");
        assert_eq!(view.meter.mark_passes, 1);
        if parked.phase_name() == "roots" && parked.meter().root_slot_visits == visits {
            return;
        }
    }
    panic!(
        "did not stop after {visits} root visits, phase {}",
        parked.phase_name()
    );
}

fn pulse_until_gray(parked: &mut ParkedMark<'_>) {
    for _ in 0..HARNESS_BOUND {
        if parked.phase_name() == "gray" {
            return;
        }
        let view = parked.pulse(1);
        assert!(!view.finished, "mark finished before gray");
        assert_eq!(view.meter.mark_passes, 1);
    }
    panic!("did not enter gray, phase {}", parked.phase_name());
}

fn charge_pending(parked: &mut ParkedMark<'_>) -> u32 {
    let owner = parked.pending().expect("edge still pending").owner;
    let view = parked.pulse(1);
    assert!(
        !view.finished,
        "charging the last pending edge finished the mark"
    );
    assert_eq!(view.meter.mark_passes, 1);
    assert!(parked.pending().is_none(), "owner still has a pending edge");
    assert_eq!(parked.scanning_owner(), Some(owner));
    assert_eq!(parked.phase_name(), "gray");
    owner
}

fn same_pending(left: Option<PendingScan>, right: Option<PendingScan>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => left.owner == right.owner && left.cursor == right.cursor,
        _ => false,
    }
}

fn assert_write_did_not_scan(before: &ScanMeter, after: &ScanMeter) {
    assert_eq!(after.mark_passes, 1);
    assert_eq!(after.mark_passes, before.mark_passes);
    assert_eq!(after.outgoing_edge_checks, before.outgoing_edge_checks);
    assert_eq!(after.outgoing_gap_checks, before.outgoing_gap_checks);
    assert_eq!(after.owner_head_checks, before.owner_head_checks);
    assert_eq!(after.root_slot_visits, before.root_slot_visits);
    assert_eq!(after.parked_cursor_repairs, before.parked_cursor_repairs);
}

fn assert_one_more_pulse_unit(parked: &mut ParkedMark<'_>) {
    let before = parked.meter();
    let view = parked.pulse(1);
    assert!(!view.finished);
    assert_eq!(view.meter.mark_passes, 1);
    let edges = (view.meter.outgoing_edge_checks + view.meter.outgoing_gap_checks)
        - (before.outgoing_edge_checks + before.outgoing_gap_checks);
    let roots = view.meter.root_slot_visits - before.root_slot_visits;
    let heads = view.meter.owner_head_checks - before.owner_head_checks;
    assert!(
        edges + roots + heads <= 1,
        "pulse(1) charged edges {edges} roots {roots} heads {heads}"
    );
}

fn cycle_kept(heap: &LuaHeap, cycle: &Cycle) -> bool {
    heap.alive(cycle.p) && heap.alive(cycle.q) && heap.alive(cycle.r)
}

fn cycle_lost_message(heap: &LuaHeap, cycle: &Cycle, index: u32) -> String {
    format!(
        "record {index} stores P but the cycle was collected; P {} Q {} R {} orphan {} barrier_marks {} edge_writes {}",
        heap.alive(cycle.p),
        heap.alive(cycle.q),
        heap.alive(cycle.r),
        heap.alive(cycle.orphan),
        heap.scan.parked_barrier_marks,
        heap.scan.parked_edge_writes,
    )
}

fn assert_cycle_identity(heap: &LuaHeap, cycle: &Cycle) {
    assert_eq!(heap.edge(cycle.p, KEY_KEPT).expect("P to Q"), Some(cycle.q));
    assert_eq!(heap.edge(cycle.q, KEY_KEPT).expect("Q to R"), Some(cycle.r));
    assert_eq!(heap.edge(cycle.r, KEY_KEPT).expect("R to P"), Some(cycle.p));
    assert!(
        !heap.alive(cycle.orphan),
        "independent weak orphan is collected"
    );
}

struct StrongOwner {
    later: TableRef,
    child: TableRef,
    owner: TableRef,
    kept: TableRef,
    successor: Option<TableRef>,
    cycle: Cycle,
}

fn plant_strong_owner(heap: &mut LuaHeap, with_successor: bool) -> StrongOwner {
    let later = heap.alloc().expect("unprocessed owner");
    let child = heap.alloc().expect("unprocessed child");
    heap.set_edge(later, KEY_KEPT, child)
        .expect("unprocessed edge");
    let owner = heap.alloc().expect("owner");
    let kept = heap.alloc().expect("kept successor");
    let old = heap.alloc().expect("old value");
    heap.set_edge(owner, KEY_KEPT, kept).expect("kept field");
    heap.set_edge(owner, KEY_REPLACED, old)
        .expect("field to replace");
    let successor = if with_successor {
        let successor = heap.alloc().expect("pending successor");
        heap.set_edge(owner, KEY_SUCCESSOR, successor)
            .expect("pending field");
        Some(successor)
    } else {
        None
    };
    let cycle = plant_cycle(heap);
    let frame = heap.enter_roots(4).expect("frame");
    heap.write_root(frame, 0, later).expect("root later");
    heap.write_root(frame, 1, owner).expect("root owner");
    heap.write_root(frame, 2, cycle.watch).expect("root watch");
    heap.write_root(frame, 3, cycle.mt).expect("root metatable");
    StrongOwner {
        later,
        child,
        owner,
        kept,
        successor,
        cycle,
    }
}

fn pause_on_owner_edge<'a>(
    heap: &'a mut LuaHeap,
    owner: TableRef,
    key: u32,
) -> (ParkedMark<'a>, u32) {
    let mut parked = ParkedMark::start(heap).expect("park");
    let index = parked.field_index(owner, key).expect("owner field");
    pause_at(&mut parked, owner.slot(), index);
    (parked, index)
}

#[test]
fn barrier_black_owner_keeps_the_cycle_until_release() {
    let mut heap = LuaHeap::new(limits(24, 8, 24));
    let graph = plant_strong_owner(&mut heap, false);
    let (mut parked, index) = pause_on_owner_edge(&mut heap, graph.owner, KEY_REPLACED);
    let scanning = charge_pending(&mut parked);
    assert_eq!(scanning, graph.owner.slot());
    assert!(parked.marked(graph.owner), "black owner is already marked");
    assert!(
        !parked.marked(graph.child),
        "the unprocessed child is not marked by setup"
    );
    assert!(!parked.marked(graph.cycle.p), "P is only weakly reachable");
    let pending = parked.pending();
    let before = parked.meter();
    parked
        .write_field(graph.owner, KEY_REPLACED, graph.cycle.p)
        .expect("in-place replace on a black owner");
    assert_eq!(parked.field_index(graph.owner, KEY_REPLACED), Some(index));
    assert_eq!(
        parked.edge(graph.owner, KEY_REPLACED).expect("replaced"),
        Some(graph.cycle.p)
    );
    assert_eq!(
        parked
            .edge(graph.owner, KEY_KEPT)
            .expect("original successor"),
        Some(graph.kept)
    );
    assert!(
        same_pending(parked.pending(), pending),
        "the owner edge cursor moved"
    );
    assert_eq!(parked.scanning_owner(), Some(graph.owner.slot()));
    assert_eq!(
        parked.meter().parked_edge_writes,
        before.parked_edge_writes + 1
    );
    assert_write_did_not_scan(&before, &parked.meter());
    assert_eq!(
        parked.meter().node_slot_visits,
        before.node_slot_visits + 2,
        "the shared writer followed the new target once"
    );
    assert!(
        !parked.marked(graph.child),
        "the write marked an unprocessed child"
    );
    assert_one_more_pulse_unit(&mut parked);
    let view = finish(&mut parked);
    parked.complete().expect("sweep");
    drop(parked);
    assert!(heap.alive(graph.owner) && heap.alive(graph.kept) && heap.alive(graph.child));
    assert!(
        cycle_kept(&heap, &graph.cycle) && heap.scan.parked_barrier_marks == 1,
        "{}",
        cycle_lost_message(&heap, &graph.cycle, index)
    );
    assert_cycle_identity(&heap, &graph.cycle);
    assert_eq!(view.mark_passes, 1);
    assert_eq!(heap.scan.mark_passes, 1);

    heap.clear_edge(graph.owner, &super::EdgeKey::Field(KEY_REPLACED))
        .expect("clear");
    heap.collect();
    assert!(
        !heap.alive(graph.cycle.p) && !heap.alive(graph.cycle.q) && !heap.alive(graph.cycle.r),
        "clearing the replaced field did not release the cycle"
    );
    assert!(heap.alive(graph.owner) && heap.alive(graph.kept));
}

#[test]
fn barrier_gray_owner_keeps_the_cycle_and_the_pending_successor() {
    let mut heap = LuaHeap::new(limits(24, 8, 24));
    let graph = plant_strong_owner(&mut heap, true);
    let successor = graph.successor.expect("pending successor");
    let (mut parked, index) = pause_on_owner_edge(&mut heap, graph.owner, KEY_SUCCESSOR);
    assert_eq!(parked.phase_name(), "gray");
    assert!(parked.marked(graph.owner));
    assert!(!parked.marked(graph.cycle.p));
    assert!(
        !parked.marked(successor),
        "the pending target is not marked yet"
    );
    let pending = parked.pending();
    let before = parked.meter();
    parked
        .write_field(graph.owner, KEY_REPLACED, graph.cycle.p)
        .expect("in-place replace on the current gray owner");
    let replaced = parked
        .field_index(graph.owner, KEY_REPLACED)
        .expect("replaced record");
    assert_ne!(
        Some(replaced),
        pending.map(|cursor| cursor.cursor),
        "the replace reused the pending successor record"
    );
    assert!(
        same_pending(parked.pending(), pending),
        "the owner edge cursor moved"
    );
    assert_eq!(parked.scanning_owner(), Some(graph.owner.slot()));
    assert_eq!(
        parked
            .edge(graph.owner, KEY_SUCCESSOR)
            .expect("pending edge"),
        Some(successor)
    );
    assert_eq!(
        parked
            .edge(graph.owner, KEY_KEPT)
            .expect("original successor"),
        Some(graph.kept)
    );
    assert_eq!(
        parked.meter().parked_edge_writes,
        before.parked_edge_writes + 1
    );
    assert_write_did_not_scan(&before, &parked.meter());
    assert_eq!(
        parked.meter().node_slot_visits,
        before.node_slot_visits + 2,
        "the shared writer followed the new target once"
    );
    assert!(
        !parked.marked(successor),
        "the write marked the pending successor"
    );
    assert_one_more_pulse_unit(&mut parked);
    let view = finish(&mut parked);
    parked.complete().expect("sweep");
    drop(parked);
    assert_eq!(
        heap.edge(graph.owner, KEY_SUCCESSOR).expect("successor"),
        Some(successor)
    );
    assert!(heap.alive(successor) && heap.alive(graph.kept));
    assert!(
        cycle_kept(&heap, &graph.cycle) && heap.scan.parked_barrier_marks == 1,
        "{}",
        cycle_lost_message(&heap, &graph.cycle, index)
    );
    assert_cycle_identity(&heap, &graph.cycle);
    assert_eq!(view.mark_passes, 1);
    assert_eq!(heap.scan.mark_passes, 1);
}

#[test]
fn barrier_omitted_write_collects_the_shared_cycle() {
    let mut heap = LuaHeap::new(limits(24, 8, 24));
    let graph = plant_strong_owner(&mut heap, false);
    let (mut parked, _) = pause_on_owner_edge(&mut heap, graph.owner, KEY_REPLACED);
    charge_pending(&mut parked);
    assert_eq!(parked.meter().parked_edge_writes, 0);
    assert_eq!(parked.meter().parked_barrier_marks, 0);
    finish(&mut parked);
    parked.complete().expect("sweep");
    drop(parked);
    assert!(
        !cycle_kept(&heap, &graph.cycle),
        "omitting the write kept the weakly held cycle"
    );
    assert!(!heap.alive(graph.cycle.orphan));
    assert_eq!(heap.scan.parked_barrier_marks, 0);
    assert_eq!(heap.scan.parked_edge_writes, 0);
    assert_eq!(heap.scan.mark_passes, 1);
}

#[test]
fn barrier_weak_owner_replace_does_not_keep_the_cycle() {
    let mut heap = LuaHeap::new(limits(24, 8, 24));
    let owner = heap.alloc().expect("weak owner");
    let old = heap.alloc().expect("old");
    let successor = heap.alloc().expect("successor");
    let cycle = plant_cycle(&mut heap);
    heap.set_metatable(owner, Some(cycle.mt))
        .expect("weak owner");
    heap.set_edge(owner, KEY_REPLACED, old).expect("weak field");
    heap.set_edge(owner, KEY_SUCCESSOR, successor)
        .expect("weak successor");
    let frame = heap.enter_roots(4).expect("frame");
    heap.write_root(frame, 0, owner).expect("root owner");
    heap.write_root(frame, 1, cycle.watch).expect("root watch");
    heap.write_root(frame, 2, cycle.mt).expect("root metatable");
    heap.write_root(frame, 3, successor)
        .expect("keep the successor readable");
    let (mut parked, index) = pause_on_owner_edge(&mut heap, owner, KEY_SUCCESSOR);
    let pending = parked.pending();
    parked
        .write_field(owner, KEY_REPLACED, cycle.p)
        .expect("in-place replace on a weak owner");
    assert_eq!(
        parked.edge(owner, KEY_REPLACED).expect("replaced"),
        Some(cycle.p)
    );
    assert_eq!(
        parked
            .edge(owner, KEY_SUCCESSOR)
            .expect("successor still readable"),
        Some(successor)
    );
    assert!(
        same_pending(parked.pending(), pending),
        "the owner edge cursor moved"
    );
    let replaced = parked
        .field_index(owner, KEY_REPLACED)
        .expect("replaced record");
    assert_ne!(replaced, index, "the replace reused the pending record");
    finish(&mut parked);
    parked.complete().expect("sweep");
    drop(parked);
    assert!(
        !cycle_kept(&heap, &cycle),
        "a weak-value replace kept the cycle at record {index}"
    );
    assert!(!heap.alive(cycle.orphan));
    assert!(heap.alive(owner) && heap.alive(successor));
    assert_eq!(heap.scan.parked_barrier_marks, 0);
    assert_eq!(heap.scan.parked_edge_writes, 1);
    assert_eq!(heap.scan.mark_passes, 1);
}

#[test]
fn barrier_already_marked_target_is_not_enqueued_again() {
    fn run(write: bool) -> (u64, u64, u64) {
        let mut heap = LuaHeap::new(limits(24, 8, 24));
        let graph = plant_strong_owner(&mut heap, false);
        let (mut parked, _) = pause_on_owner_edge(&mut heap, graph.owner, KEY_REPLACED);
        charge_pending(&mut parked);
        assert!(
            parked.marked(graph.later),
            "Later was rooted before the write"
        );
        let before = parked.meter();
        if write {
            parked
                .write_field(graph.owner, KEY_REPLACED, graph.later)
                .expect("replace with an already marked target");
            assert_eq!(
                parked.meter().scratch.work_pushed,
                before.scratch.work_pushed
            );
            assert_eq!(
                parked.meter().parked_barrier_marks,
                before.parked_barrier_marks
            );
        }
        finish(&mut parked);
        let meter = parked.meter();
        parked.cancel();
        (
            meter.outgoing_edge_checks,
            meter.owner_head_checks,
            meter.scratch.work_pushed,
        )
    }

    let written = run(true);
    let omitted = run(false);
    assert_eq!(
        written.0, omitted.0,
        "an already marked target was scanned again"
    );
    assert_eq!(
        written.1, omitted.1,
        "an already marked target opened another owner"
    );
    assert_eq!(
        written.2, omitted.2,
        "an already marked target was queued again"
    );
}

#[test]
fn barrier_full_edges_replace_leaves_the_other_gap() {
    let mut heap = LuaHeap::new(limits(8, 8, 3));
    let mt = heap.alloc().expect("metatable");
    let gap_owner = heap.alloc().expect("gap owner");
    let key_obj = heap.alloc().expect("object key");
    let dead = heap.alloc().expect("dead value");
    let owner = heap.alloc().expect("owner");
    let old = heap.alloc().expect("old value");
    heap.set_mode_scalar(mt, Value::string("v\0"))
        .expect("weak values");
    heap.set_metatable(gap_owner, Some(mt))
        .expect("gap metatable");
    heap.set_object_edge(gap_owner, key_obj, dead)
        .expect("object edge");
    heap.set_edge(owner, KEY_REPLACED, old).expect("full field");
    let frame = heap.enter_roots(4).expect("frame");
    heap.write_root(frame, 0, mt).expect("root metatable");
    heap.write_root(frame, 1, gap_owner)
        .expect("root gap owner");
    heap.write_root(frame, 2, key_obj).expect("root key");
    heap.write_root(frame, 3, owner).expect("root owner");
    heap.collect();
    assert!(!heap.alive(dead));
    assert!(heap.alive(gap_owner) && heap.alive(key_obj) && heap.alive(owner));
    assert_eq!(
        heap.stats().edge_slots,
        3,
        "mode, gap, and field fill the edge cap"
    );
    let _ = heap.take_scan_meter();
    let target = heap.alloc().expect("replacement");
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    let field = parked
        .field_index(owner, KEY_REPLACED)
        .expect("field at the cap");
    let gap = parked
        .object_index(gap_owner, key_obj)
        .expect("other owner's gap");
    assert!(parked.record_is_gap(gap), "prepared record is not a gap");
    assert_ne!(field, gap);
    pause_at(&mut parked, owner.slot(), field);
    charge_pending(&mut parked);
    assert!(parked.marked(owner));
    assert!(!parked.marked(target));
    let occupied = parked.meter();
    parked
        .write_field(owner, KEY_REPLACED, target)
        .expect("in-place replace at the edge cap");
    assert_eq!(parked.field_index(owner, KEY_REPLACED), Some(field));
    assert_eq!(parked.object_index(gap_owner, key_obj), Some(gap));
    assert!(
        parked.record_is_gap(gap),
        "the other gap was reclaimed or rewritten"
    );
    assert_eq!(
        parked.stats().edge_slots,
        3,
        "the replace allocated another edge"
    );
    assert_eq!(
        parked.meter().parked_cursor_repairs,
        occupied.parked_cursor_repairs
    );
    assert_eq!(
        parked.meter().parked_edge_writes,
        occupied.parked_edge_writes + 1
    );
    finish(&mut parked);
    parked.complete().expect("sweep");
    drop(parked);
    assert!(
        heap.alive(target) && heap.scan.parked_barrier_marks == 1,
        "record {field} stores the target but it was collected; barrier_marks {} gap {gap}",
        heap.scan.parked_barrier_marks
    );
    assert!(heap.alive(gap_owner) && heap.alive(key_obj));
    assert_eq!(heap.scan.mark_passes, 1);
}

struct PassedSlot {
    frame: RootFrameId,
    child: TableRef,
    cycle: Cycle,
}

fn pause_passed_slot(heap: &mut LuaHeap, gray: bool) -> (ParkedMark<'_>, PassedSlot) {
    let later = heap.alloc().expect("later");
    let child = heap.alloc().expect("later child");
    heap.set_edge(later, KEY_KEPT, child).expect("later edge");
    let tail = heap.alloc().expect("tail");
    let cycle = plant_cycle(heap);
    let frame = heap.enter_roots(4).expect("frame");
    heap.write_root(frame, 0, later).expect("root later");
    heap.write_root(frame, 2, tail).expect("root tail");
    heap.write_root(frame, 3, cycle.watch).expect("root watch");
    let mut parked = ParkedMark::start(heap).expect("park");
    if gray {
        pause_after_root_visits(&mut parked, 4);
        let orphan = parked
            .field_index(cycle.watch, KEY_REPLACED)
            .expect("watch orphan edge");
        pulse_until_gray(&mut parked);
        pause_at(&mut parked, cycle.watch.slot(), orphan);
        charge_pending(&mut parked);
        assert_eq!(parked.phase_name(), "gray");
        assert_eq!(parked.meter().root_slot_visits, 4);
    } else {
        pause_after_root_visits(&mut parked, 2);
        assert_eq!(parked.phase_name(), "roots");
    }
    assert!(
        parked.meter().root_slot_visits > u64::from(PASSED_SLOT),
        "the empty root slot has not been passed"
    );
    assert_eq!(
        parked.root_table(frame, PASSED_SLOT).expect("slot"),
        None,
        "the passed slot was not empty"
    );
    assert!(!parked.marked(cycle.p), "P is marked before the root write");
    (
        parked,
        PassedSlot {
            frame,
            child,
            cycle,
        },
    )
}

fn assert_table_root_kept(gray: bool) {
    let mut heap = LuaHeap::new(limits(24, 8, 24));
    let (mut parked, slot) = pause_passed_slot(&mut heap, gray);
    let before = parked.meter();
    let visits = before.root_slot_visits;
    let pending = parked.pending();
    let scanning = parked.scanning_owner();
    parked
        .write_table_root(slot.frame, PASSED_SLOT, slot.cycle.p)
        .expect("fixed table root update");
    assert_eq!(
        parked.root_table(slot.frame, PASSED_SLOT).expect("stored"),
        Some(slot.cycle.p)
    );
    assert_eq!(parked.meter().root_slot_visits, visits);
    assert_eq!(
        parked.meter().parked_root_writes,
        before.parked_root_writes + 1
    );
    assert!(
        same_pending(parked.pending(), pending),
        "the owner edge cursor moved"
    );
    assert_eq!(parked.scanning_owner(), scanning);
    assert_write_did_not_scan(&before, &parked.meter());
    assert_one_more_pulse_unit(&mut parked);
    let view = finish(&mut parked);
    parked.complete().expect("sweep");
    drop(parked);
    assert!(
        cycle_kept(&heap, &slot.cycle) && heap.scan.parked_barrier_marks == 1,
        "{}",
        cycle_lost_message(&heap, &slot.cycle, PASSED_SLOT)
    );
    assert_cycle_identity(&heap, &slot.cycle);
    assert!(
        heap.alive(slot.child),
        "the resumed gray queue lost Later's child"
    );
    assert_eq!(view.mark_passes, 1);
    assert_eq!(heap.scan.parked_root_writes, 1);
    assert_eq!(heap.scan.mark_passes, 1);
    heap.clear_root(slot.frame, PASSED_SLOT)
        .expect("clear the retaining root");
    heap.collect();
    assert!(
        !heap.alive(slot.cycle.p),
        "clearing the root did not release P"
    );
}

#[test]
fn barrier_roots_passed_slot_keeps_the_table() {
    assert_table_root_kept(false);
}

#[test]
fn barrier_gray_passed_slot_keeps_the_table() {
    assert_table_root_kept(true);
}

#[test]
fn barrier_passed_slot_keeps_the_closure_chain() {
    let mut heap = LuaHeap::new(limits(16, 8, 16));
    let later = heap.alloc().expect("later");
    let tail = heap.alloc().expect("tail");
    let mt = heap.alloc().expect("metatable");
    let watch = heap.alloc().expect("watch");
    let orphan = heap.alloc().expect("orphan");
    let closure = heap.alloc_closure().expect("closure");
    let cell = heap.alloc_upvalue().expect("upvalue");
    let successor = heap.alloc().expect("successor");
    heap.set_mode_scalar(mt, Value::string("v\0"))
        .expect("mode");
    heap.set_metatable(watch, Some(mt))
        .expect("watch metatable");
    heap.set_upvalue_edge(closure, cell).expect("closure cell");
    heap.write_upvalue(cell, successor).expect("cell current");
    heap.set_closure_edge(watch, KEY_KEPT, closure)
        .expect("weak closure");
    heap.set_edge(watch, KEY_REPLACED, orphan)
        .expect("weak orphan");
    let frame = heap.enter_roots(4).expect("frame");
    heap.write_root(frame, 0, later).expect("root later");
    heap.write_root(frame, 2, tail).expect("root tail");
    heap.write_root(frame, 3, watch).expect("root watch");
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    pause_after_root_visits(&mut parked, 2);
    assert_eq!(parked.phase_name(), "roots");
    let before = parked.meter();
    parked
        .write_closure_root(frame, PASSED_SLOT, closure)
        .expect("fixed closure root update");
    assert_eq!(
        parked.root_closure(frame, PASSED_SLOT).expect("stored"),
        Some(closure)
    );
    assert_eq!(parked.meter().root_slot_visits, before.root_slot_visits);
    assert_eq!(parked.meter().parked_root_writes, 1);
    assert_write_did_not_scan(&before, &parked.meter());
    finish(&mut parked);
    parked.complete().expect("sweep");
    drop(parked);
    assert!(
        heap.closure_alive(closure)
            && heap.upvalue_alive(cell)
            && heap.alive(successor)
            && heap.scan.parked_barrier_marks == 1,
        "closure chain collected after a successful root write; closure {} cell {} successor {} barrier_marks {}",
        heap.closure_alive(closure),
        heap.upvalue_alive(cell),
        heap.alive(successor),
        heap.scan.parked_barrier_marks,
    );
    assert_eq!(heap.read_upvalue(closure).expect("chain"), successor);
    assert!(!heap.alive(orphan), "weak orphan was retained");
    assert_eq!(heap.scan.mark_passes, 1);
    assert_eq!(heap.scan.parked_root_writes, 1);
}

#[test]
fn barrier_passed_slot_keeps_the_upvalue_chain() {
    let mut heap = LuaHeap::new(limits(16, 8, 16));
    let later = heap.alloc().expect("later");
    let tail = heap.alloc().expect("tail");
    let mt = heap.alloc().expect("metatable");
    let watch = heap.alloc().expect("watch");
    let orphan = heap.alloc().expect("orphan");
    let cell = heap.alloc_upvalue().expect("upvalue");
    let successor = heap.alloc().expect("successor");
    heap.set_mode_scalar(mt, Value::string("v\0"))
        .expect("mode");
    heap.set_metatable(watch, Some(mt))
        .expect("watch metatable");
    heap.write_upvalue(cell, successor).expect("cell current");
    heap.set_cell_edge(watch, cell).expect("weak cell");
    heap.set_edge(watch, KEY_REPLACED, orphan)
        .expect("weak orphan");
    let frame = heap.enter_roots(4).expect("frame");
    heap.write_root(frame, 0, later).expect("root later");
    heap.write_root(frame, 2, tail).expect("root tail");
    heap.write_root(frame, 3, watch).expect("root watch");
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    pause_after_root_visits(&mut parked, 4);
    let orphan_edge = parked
        .field_index(watch, KEY_REPLACED)
        .expect("orphan edge");
    pulse_until_gray(&mut parked);
    pause_at(&mut parked, watch.slot(), orphan_edge);
    charge_pending(&mut parked);
    assert_eq!(parked.phase_name(), "gray");
    assert!(
        !parked.marked(successor),
        "weak scan marked the cell current"
    );
    let before = parked.meter();
    let scanning = parked.scanning_owner();
    parked
        .write_upvalue_root(frame, PASSED_SLOT, cell)
        .expect("fixed upvalue root update");
    assert_eq!(
        parked.root_upvalue(frame, PASSED_SLOT).expect("stored"),
        Some(cell)
    );
    assert_eq!(parked.meter().root_slot_visits, before.root_slot_visits);
    assert_eq!(parked.scanning_owner(), scanning);
    assert_eq!(parked.meter().parked_root_writes, 1);
    assert_write_did_not_scan(&before, &parked.meter());
    finish(&mut parked);
    parked.complete().expect("sweep");
    drop(parked);
    assert!(
        heap.upvalue_alive(cell) && heap.alive(successor) && heap.scan.parked_barrier_marks == 1,
        "upvalue chain collected after a successful root write; cell {} successor {} barrier_marks {}",
        heap.upvalue_alive(cell),
        heap.alive(successor),
        heap.scan.parked_barrier_marks,
    );
    assert_eq!(heap.current_table(cell).expect("current"), Some(successor));
    assert!(!heap.alive(orphan));
    assert_eq!(heap.scan.mark_passes, 1);
    assert_eq!(heap.scan.parked_root_writes, 1);
}

#[test]
fn barrier_refused_root_write_leaves_the_slot_and_meter() {
    let mut heap = LuaHeap::new(limits(4, 4, 4));
    let frame = heap.enter_roots(2).expect("frame");
    let target = heap.alloc().expect("target");
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    let before = picture(&parked);
    let err = parked
        .write_table_root(frame, 0, target)
        .expect_err("closed phase");
    assert_eq!(err, HeapError::MarkDenied);
    assert_same(&picture(&parked), &before);
    assert_eq!(parked.meter().parked_root_writes, 0);
    assert_eq!(parked.meter().parked_barrier_marks, 0);
    parked.cancel();

    let mut heap = LuaHeap::new(limits(8, 4, 4));
    let stale = heap.alloc().expect("stale");
    heap.rollback(stale).expect("rollback");
    let later = heap.alloc().expect("later");
    let tail = heap.alloc().expect("tail");
    let frame = heap.enter_roots(3).expect("frame");
    heap.write_root(frame, 0, later).expect("root later");
    heap.write_root(frame, 2, tail).expect("root tail");
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    pause_after_root_visits(&mut parked, 2);
    let before = picture(&parked);
    let stale_err = parked
        .write_table_root(frame, PASSED_SLOT, stale)
        .expect_err("stale handle");
    assert_eq!(stale_err, HeapError::StaleHandle);
    assert_same(&picture(&parked), &before);
    assert_eq!(parked.root_table(frame, PASSED_SLOT).expect("slot"), None);
    let slot_err = parked
        .write_table_root(frame, 3, later)
        .expect_err("invalid slot");
    assert_eq!(slot_err, HeapError::StaleFrame);
    assert_same(&picture(&parked), &before);
    assert_eq!(parked.root_table(frame, PASSED_SLOT).expect("slot"), None);
    assert_eq!(parked.meter().parked_root_writes, 0);
    assert_eq!(parked.meter().parked_barrier_marks, 0);
    assert_eq!(parked.meter().mark_passes, 1);
}

/// Raw `set_edge` on a scanned owner. The write must succeed in place.
/// Keeping the white cycle is the product result; collecting it is the miss.
#[test]
fn raw_set_edge_on_scanned_owner_keeps_the_strong_cycle() {
    let mut heap = LuaHeap::new(limits(24, 8, 24));
    let graph = plant_strong_owner(&mut heap, false);
    let (mut parked, index) = pause_on_owner_edge(&mut heap, graph.owner, KEY_REPLACED);
    let scanning = charge_pending(&mut parked);
    assert_eq!(scanning, graph.owner.slot());
    assert_eq!(parked.phase_name(), "gray");
    assert!(parked.mutation_open());
    assert!(
        parked.marked(graph.owner),
        "the scanned owner is already marked"
    );
    assert!(!parked.marked(graph.cycle.p), "P is only weakly reachable");
    assert!(
        !parked.marked(graph.child),
        "the unprocessed child is not marked by setup"
    );
    let pending = parked.pending();
    let before = parked.meter();
    parked.with_saved_heap(|heap| {
        heap.set_edge(graph.owner, KEY_REPLACED, graph.cycle.p)
            .expect("raw in-place replace");
    });
    assert_eq!(parked.field_index(graph.owner, KEY_REPLACED), Some(index));
    assert_eq!(
        parked.edge(graph.owner, KEY_REPLACED).expect("replaced"),
        Some(graph.cycle.p)
    );
    assert_eq!(
        parked
            .edge(graph.owner, KEY_KEPT)
            .expect("original successor"),
        Some(graph.kept)
    );
    assert!(
        same_pending(parked.pending(), pending),
        "the owner edge cursor moved"
    );
    assert_eq!(parked.scanning_owner(), Some(graph.owner.slot()));
    assert_eq!(parked.meter().parked_edge_writes, before.parked_edge_writes);
    assert_write_did_not_scan(&before, &parked.meter());
    assert!(
        !parked.marked(graph.child),
        "the write marked an unprocessed child"
    );
    let visit_delta = parked.meter().node_slot_visits - before.node_slot_visits;
    let pushed_delta = parked.meter().scratch.work_pushed - before.scratch.work_pushed;
    let barrier_delta = parked.meter().parked_barrier_marks - before.parked_barrier_marks;
    assert_one_more_pulse_unit(&mut parked);
    let view = finish(&mut parked);
    parked.complete().expect("sweep");
    drop(parked);
    assert!(
        cycle_kept(&heap, &graph.cycle),
        "{}",
        cycle_lost_message(&heap, &graph.cycle, index)
    );
    assert_cycle_identity(&heap, &graph.cycle);
    assert_eq!(
        barrier_delta, 1,
        "the shared gray queue marked the new target once"
    );
    assert_eq!(
        visit_delta, 2,
        "one barrier follow reads the target slot twice"
    );
    assert_eq!(
        pushed_delta, 1,
        "only the new target joined gray at the write"
    );
    assert_eq!(heap.scan.parked_barrier_marks, 1);
    assert_eq!(heap.scan.parked_edge_writes, 0);
    assert_eq!(view.mark_passes, 1);
    assert_eq!(heap.scan.mark_passes, 1);
    assert!(heap.alive(graph.owner) && heap.alive(graph.kept) && heap.alive(graph.child));
    heap.clear_edge(graph.owner, &super::EdgeKey::Field(KEY_REPLACED))
        .expect("clear");
    heap.collect();
    assert!(
        !heap.alive(graph.cycle.p) && !heap.alive(graph.cycle.q) && !heap.alive(graph.cycle.r),
        "clearing the replaced field did not release the cycle"
    );
    assert!(heap.alive(graph.owner) && heap.alive(graph.kept));
}

#[test]
fn raw_set_edge_on_a_weak_owner_does_not_keep_the_cycle() {
    let mut heap = LuaHeap::new(limits(24, 8, 24));
    let owner = heap.alloc().expect("weak owner");
    let old = heap.alloc().expect("old");
    let successor = heap.alloc().expect("successor");
    let cycle = plant_cycle(&mut heap);
    heap.set_metatable(owner, Some(cycle.mt))
        .expect("weak owner");
    heap.set_edge(owner, KEY_REPLACED, old).expect("weak field");
    heap.set_edge(owner, KEY_SUCCESSOR, successor)
        .expect("weak successor");
    let frame = heap.enter_roots(4).expect("frame");
    heap.write_root(frame, 0, owner).expect("root owner");
    heap.write_root(frame, 1, cycle.watch).expect("root watch");
    heap.write_root(frame, 2, cycle.mt).expect("root metatable");
    heap.write_root(frame, 3, successor)
        .expect("keep the successor readable");
    let (mut parked, index) = pause_on_owner_edge(&mut heap, owner, KEY_SUCCESSOR);
    charge_pending(&mut parked);
    assert!(parked.marked(owner));
    assert_eq!(parked.phase_name(), "gray");
    let field = parked
        .field_index(owner, KEY_REPLACED)
        .expect("replaced field");
    assert_ne!(field, index);
    let before = parked.meter();
    let pending = parked.pending();
    parked.with_saved_heap(|heap| {
        heap.set_edge(owner, KEY_REPLACED, cycle.p)
            .expect("raw replace");
    });
    assert_eq!(parked.field_index(owner, KEY_REPLACED), Some(field));
    assert_eq!(
        parked.edge(owner, KEY_REPLACED).expect("replaced"),
        Some(cycle.p)
    );
    assert!(
        same_pending(parked.pending(), pending),
        "the owner edge cursor moved"
    );
    assert_eq!(
        parked.meter().parked_barrier_marks,
        before.parked_barrier_marks
    );
    assert_eq!(parked.meter().parked_edge_writes, before.parked_edge_writes);
    assert_eq!(
        parked.meter().scratch.work_pushed,
        before.scratch.work_pushed
    );
    assert_write_did_not_scan(&before, &parked.meter());
    finish(&mut parked);
    parked.complete().expect("sweep");
    drop(parked);
    assert!(
        !cycle_kept(&heap, &cycle),
        "a weak raw replace kept the cycle at record {field}"
    );
    assert!(!heap.alive(cycle.orphan));
    assert!(heap.alive(owner) && heap.alive(successor));
    assert_eq!(heap.scan.parked_barrier_marks, 0);
    assert_eq!(heap.scan.parked_edge_writes, 0);
    assert_eq!(heap.scan.mark_passes, 1);
}

#[test]
fn raw_set_edge_on_an_unmarked_owner_waits_for_normal_discovery() {
    let mut heap = LuaHeap::new(limits(16, 8, 16));
    let later = heap.alloc().expect("later");
    let owner = heap.alloc().expect("unmarked owner");
    let old = heap.alloc().expect("old");
    heap.set_edge(later, KEY_KEPT, owner)
        .expect("later to owner");
    heap.set_edge(owner, KEY_REPLACED, old).expect("field");
    let cycle = plant_cycle(&mut heap);
    let frame = heap.enter_roots(3).expect("frame");
    heap.write_root(frame, 0, later).expect("root later");
    heap.write_root(frame, 1, cycle.watch).expect("root watch");
    heap.write_root(frame, 2, cycle.mt).expect("root metatable");
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    let later_edge = parked.field_index(later, KEY_KEPT).expect("later edge");
    pause_at(&mut parked, later.slot(), later_edge);
    assert_eq!(parked.phase_name(), "gray");
    assert!(
        !parked.marked(owner),
        "the owner is still ahead of the cursor"
    );
    assert!(!parked.marked(cycle.p));
    let field = parked
        .field_index(owner, KEY_REPLACED)
        .expect("owner field");
    let pending = parked.pending();
    let before = parked.meter();
    parked.with_saved_heap(|heap| {
        heap.set_edge(owner, KEY_REPLACED, cycle.p)
            .expect("raw replace");
    });
    assert_eq!(parked.field_index(owner, KEY_REPLACED), Some(field));
    assert_eq!(
        parked.edge(owner, KEY_REPLACED).expect("replaced"),
        Some(cycle.p)
    );
    assert!(!parked.marked(owner));
    assert!(!parked.marked(cycle.p));
    assert!(
        same_pending(parked.pending(), pending),
        "the pending cursor moved"
    );
    assert_eq!(
        parked.meter().parked_barrier_marks,
        before.parked_barrier_marks
    );
    assert_eq!(parked.meter().parked_edge_writes, before.parked_edge_writes);
    assert_eq!(
        parked.meter().scratch.work_pushed,
        before.scratch.work_pushed
    );
    assert_write_did_not_scan(&before, &parked.meter());
    finish(&mut parked);
    parked.complete().expect("sweep");
    drop(parked);
    assert!(
        cycle_kept(&heap, &cycle) && heap.scan.parked_barrier_marks == 0,
        "unmarked owner was not discovered by the later scan, or the write barrier ran; {}",
        cycle_lost_message(&heap, &cycle, field)
    );
    assert_cycle_identity(&heap, &cycle);
    assert!(heap.alive(later) && heap.alive(owner));
    assert_eq!(heap.scan.parked_edge_writes, 0);
    assert_eq!(heap.scan.mark_passes, 1);
}

/// New table-object key inserted into an already scanned owner.
///
/// `midcycle-existing-object-key` is an old record and is not this slice.
/// LuaJIT `evidence/lua52-gc-step/midcycle-reference-publication.lua` key/omit
/// is only the external scalar-payload control for a new object key.
enum NewTableKeyArm {
    Strong,
    WeakValue,
    WeakKey,
    WeakBoth,
    StringValue,
    SameTarget,
    Omitted,
}

#[test]
fn gray_new_table_key_strong_retains_both_ends() {
    exercise_new_table_key(NewTableKeyArm::Strong);
}

#[test]
fn gray_new_table_key_weak_values_follow_key_only() {
    exercise_new_table_key(NewTableKeyArm::WeakValue);
}

#[test]
fn gray_new_table_key_weak_keys_follow_value_only() {
    exercise_new_table_key(NewTableKeyArm::WeakKey);
}

#[test]
fn gray_new_table_key_weak_both_keeps_physical_rules() {
    exercise_new_table_key(NewTableKeyArm::WeakBoth);
}

#[test]
fn gray_new_table_key_string_value_follows_key_only() {
    exercise_new_table_key(NewTableKeyArm::StringValue);
}

#[test]
fn gray_new_table_key_same_target_pushes_once() {
    exercise_new_table_key(NewTableKeyArm::SameTarget);
}

#[test]
fn gray_new_table_key_omitted_collects_unmarked() {
    exercise_new_table_key(NewTableKeyArm::Omitted);
}

fn exercise_new_table_key(arm: NewTableKeyArm) {
    let mode = match arm {
        NewTableKeyArm::WeakValue => Some("v"),
        NewTableKeyArm::WeakKey => Some("k"),
        NewTableKeyArm::WeakBoth => Some("kv"),
        NewTableKeyArm::Strong
        | NewTableKeyArm::StringValue
        | NewTableKeyArm::SameTarget
        | NewTableKeyArm::Omitted => None,
    };
    let pin_key = matches!(arm, NewTableKeyArm::WeakKey);
    let string_value = matches!(arm, NewTableKeyArm::StringValue);
    let same_target = matches!(arm, NewTableKeyArm::SameTarget);
    let omitted = matches!(arm, NewTableKeyArm::Omitted);
    crate::with_idle_vm(|vm| {
        use crate::heap::{self, GuestClass, StepOutcome};

        let mut host = vm.host_context();
        heap::install(&mut host, limits(16, 8, 16)).expect("install");
        let planted = heap::with_heap(&mut host, |heap| {
            let prior = heap.alloc()?;
            let prior_slot = prior.slot();
            let prior_generation = prior.generation();
            heap.rollback(prior)?;
            let key = heap.alloc()?;
            assert_eq!(
                key.slot(),
                prior_slot,
                "the new key reuses the released slot"
            );
            assert_ne!(
                key.generation(),
                prior_generation,
                "the new key is a different generation"
            );
            let lookalike = heap.alloc()?;
            assert_ne!(
                lookalike.slot(),
                key.slot(),
                "lookalike is an independent key"
            );
            let key_child = heap.alloc()?;
            let target = if string_value || same_target {
                None
            } else {
                Some(heap.alloc()?)
            };
            let value_child = if target.is_some() {
                Some(heap.alloc()?)
            } else {
                None
            };
            let owner = heap.alloc()?;
            let sentinel = heap.alloc()?;
            let closure = heap.alloc_closure()?;
            let unrelated = heap.alloc()?;
            heap.set_named_scalar(key, "tag", Value::string("key"))?;
            heap.set_named_scalar(lookalike, "tag", Value::string("key"))?;
            heap.set_named_edge(key, "next", key_child)?;
            if let (Some(target), Some(child)) = (target, value_child) {
                heap.set_named_edge(target, "next", child)?;
            }
            heap.set_named_scalar(owner, "old", Value::string("kept"))?;
            heap.set_named_scalar(sentinel, "pending", Value::Bool(true))?;
            if let Some(mode) = mode {
                let meta = heap.alloc()?;
                heap.set_mode_scalar(meta, Value::string(mode))?;
                heap.set_metatable(owner, Some(meta))?;
                assert_eq!(heap.named_value(meta, "__mode")?, Value::string(mode));
            }
            let stale = super::TableRef::table(heap.domain(), prior_slot, prior_generation);
            let key_carrier = heap.issue_carrier(key)?;
            assert_eq!(heap.guest_class(&key_carrier), GuestClass::Table(key));
            let target_carrier = target.map(|table| heap.issue_carrier(table)).transpose()?;
            if let (Some(table), Some(carrier)) = (target, target_carrier.as_ref()) {
                assert_eq!(heap.guest_class(carrier), GuestClass::Table(table));
            }
            let frame = heap.enter_roots(4)?;
            heap.write_root(frame, 0, sentinel)?;
            heap.write_root(frame, 1, owner)?;
            heap.write_closure_root(frame, 2, closure)?;
            if pin_key {
                heap.write_root(frame, 3, key)?;
            }
            Ok((
                key,
                lookalike,
                key_child,
                target,
                value_child,
                owner,
                sentinel,
                closure,
                unrelated,
                stale,
                key_carrier,
                target_carrier,
            ))
        })
        .expect("plant");
        let (
            key,
            lookalike,
            key_child,
            target,
            value_child,
            owner,
            sentinel,
            closure,
            _unrelated,
            stale,
            key_carrier,
            target_carrier,
        ) = planted;
        let _ = closure;
        heap::open_fixed_step(&mut host).expect("open");
        heap::with_heap(&mut host, |heap| {
            let (_, old) = heap.locate_named(owner.slot(), "old").expect("old record");
            let (_, sentinel_record) = heap
                .locate_named(sentinel.slot(), "pending")
                .expect("pending record");
            let mut parked = ParkedMark::resume(heap).expect("resume");
            pause_at(&mut parked, owner.slot(), old);
            let before_owner = parked.meter().outgoing_edge_checks;
            let pending = pause_at(&mut parked, sentinel.slot(), sentinel_record);
            assert!(
                parked.meter().outgoing_edge_checks > before_owner,
                "the owner record was not scanned before the new key"
            );
            assert!(
                parked.marked(owner),
                "the owner color is from the scan, not the insert"
            );
            if pin_key {
                assert!(
                    parked.marked(key),
                    "the pinned key was marked before the insert"
                );
            } else {
                assert!(!parked.marked(key), "the new key starts unmarked");
                assert!(
                    !parked.marked(key_child),
                    "the key successor starts unmarked"
                );
            }
            if let Some(target) = target {
                assert!(!parked.marked(target), "the new value starts unmarked");
            }
            let before = parked.meter();
            parked.with_saved_heap(|heap| {
                assert!(
                    heap.find_key(owner.slot(), &super::EdgeKey::object(key))
                        .is_none(),
                    "the new key has no physical record"
                );
                assert!(
                    heap.find_key(owner.slot(), &super::EdgeKey::object(lookalike))
                        .is_none(),
                    "the lookalike has no physical record"
                );
                assert!(
                    heap.find_key(owner.slot(), &super::EdgeKey::object(stale))
                        .is_none(),
                    "the previous generation has no physical record"
                );
                if !omitted {
                    if string_value {
                        heap.set_object_scalar(owner, key, Value::string("标量"))?;
                    } else if same_target {
                        heap.set_object_edge(owner, key, key)?;
                    } else if let Some(target) = target {
                        heap.set_object_edge(owner, key, target)?;
                    }
                    let record = heap
                        .find_key(owner.slot(), &super::EdgeKey::object(key))
                        .expect("inserted record");
                    assert!(
                        heap.find_key(owner.slot(), &super::EdgeKey::object(lookalike))
                            .is_none(),
                        "same shape does not alias a different generation identity"
                    );
                    assert!(
                        heap.find_key(owner.slot(), &super::EdgeKey::object(stale))
                            .is_none(),
                        "the previous generation does not alias the new key"
                    );
                    let edge = heap.edge_slots[record as usize]
                        .edge
                        .as_ref()
                        .expect("occupied");
                    assert_eq!(edge.key, super::EdgeKey::object(key));
                    if string_value {
                        assert_eq!(
                            heap.object_value(owner, key)?,
                            Value::string("标量"),
                            "the scalar payload is stored"
                        );
                    } else if same_target {
                        assert_eq!(heap.object_value(owner, key)?, key_carrier);
                    } else if let Some(carrier) = target_carrier.clone() {
                        assert_eq!(heap.object_value(owner, key)?, carrier);
                    }
                }
                Ok(())
            })?;
            assert!(
                same_pending(parked.pending(), Some(pending)),
                "the pending cursor moved"
            );
            assert_eq!(
                parked.scanning_owner(),
                Some(sentinel.slot()),
                "the write moved the gray owner"
            );
            assert_write_did_not_scan(&before, &parked.meter());
            let after = parked.meter();
            assert_eq!(after.metatable_edge_checks, before.metatable_edge_checks);
            assert_eq!(after.weak_mode_lookups, before.weak_mode_lookups);
            let visits = after.node_slot_visits - before.node_slot_visits;
            let pushed = after.scratch.work_pushed - before.scratch.work_pushed;
            let barriers = after.parked_barrier_marks - before.parked_barrier_marks;
            match arm {
                NewTableKeyArm::Strong => {
                    let target = target.expect("strong value");
                    let child = value_child.expect("value successor");
                    assert!(parked.marked(key), "strong key is enqueued immediately");
                    assert!(
                        parked.marked(target),
                        "strong value is enqueued immediately"
                    );
                    assert!(
                        !parked.marked(key_child),
                        "key successor waits for the continued scan"
                    );
                    assert!(
                        !parked.marked(child),
                        "value successor waits for the continued scan"
                    );
                    assert_eq!(visits, 4, "strong insert follows the key and the value");
                    assert_eq!(pushed, 2, "strong insert pushes each unmarked end once");
                    assert_eq!(barriers, 2);
                }
                NewTableKeyArm::WeakValue => {
                    assert!(parked.marked(key), "weak values still enqueue the new key");
                    assert!(
                        !parked.marked(target.expect("weak value")),
                        "weak values do not enqueue the value"
                    );
                    assert!(
                        !parked.marked(key_child),
                        "key successor waits for the continued scan"
                    );
                    assert_eq!(visits, 2, "weak values follow only the new key");
                    assert_eq!(pushed, 1);
                    assert_eq!(barriers, 1);
                }
                NewTableKeyArm::WeakKey => {
                    assert!(parked.marked(key), "the pinned key was already marked");
                    assert!(
                        parked.marked(target.expect("weak-key value")),
                        "weak keys still enqueue the unmarked value"
                    );
                    assert_eq!(visits, 2, "weak keys do not attempt to follow the key");
                    assert_eq!(pushed, 1, "only the value is newly pushed");
                    assert_eq!(barriers, 1);
                }
                NewTableKeyArm::WeakBoth => {
                    assert!(!parked.marked(key), "weak keys are not enqueued");
                    assert!(
                        !parked.marked(target.expect("weak value")),
                        "weak values are not enqueued"
                    );
                    assert_eq!(visits, 0, "kv does not follow either end");
                    assert_eq!(pushed, 0);
                    assert_eq!(barriers, 0);
                }
                NewTableKeyArm::StringValue => {
                    assert!(
                        parked.marked(key),
                        "a string value still enqueues the new key"
                    );
                    assert!(
                        !parked.marked(key_child),
                        "key successor waits for the continued scan"
                    );
                    assert_eq!(visits, 2, "a string value follows only the new key");
                    assert_eq!(pushed, 1);
                    assert_eq!(barriers, 1);
                }
                NewTableKeyArm::SameTarget => {
                    assert!(
                        parked.marked(key),
                        "the shared target is enqueued immediately"
                    );
                    assert!(
                        !parked.marked(key_child),
                        "the successor waits for the continued scan"
                    );
                    assert_eq!(visits, 4, "both ends are attempted");
                    assert_eq!(pushed, 1, "the same target is pushed once");
                    assert_eq!(barriers, 1);
                }
                NewTableKeyArm::Omitted => {
                    assert!(!parked.marked(key), "omit does not mark the key");
                    assert_eq!(visits, 0);
                    assert_eq!(pushed, 0);
                    assert_eq!(barriers, 0);
                }
            }
            parked.park().expect("park");
            Ok(())
        })
        .expect("write");
        let published = heap::step_fixed_frames(&mut host, 128).expect("publish");
        assert!(
            matches!(published, StepOutcome::Published { .. }),
            "{published:?}"
        );
        let done = heap::step_fixed_frames(&mut host, 128).expect("finish");
        let StepOutcome::Completed(_report) = done else {
            panic!("expected completion, got {done:?}");
        };
        heap::with_heap(&mut host, |heap| {
            assert_eq!(
                heap.named_value(owner, "old")?,
                Value::string("kept"),
                "the already scanned record stays"
            );
            assert_eq!(
                heap.named_value(sentinel, "pending")?,
                Value::Bool(true),
                "the sentinel record stays"
            );
            assert!(!heap.alive(lookalike), "the lookalike was not rooted");
            match arm {
                NewTableKeyArm::Strong => {
                    assert!(heap.alive(key), "new table key stays live this cycle");
                    let target = target.expect("strong value");
                    assert!(heap.alive(target), "new table value stays live this cycle");
                    assert!(heap.alive(key_child), "key successor stays live this cycle");
                    assert!(
                        heap.alive(value_child.expect("value successor")),
                        "value successor stays live this cycle"
                    );
                }
                NewTableKeyArm::WeakValue => {
                    assert!(
                        heap.alive(key),
                        "weak-value new table key stays live this cycle"
                    );
                    assert!(
                        !heap.alive(target.expect("weak value")),
                        "weak value is cleared this cycle"
                    );
                    assert!(heap.alive(key_child), "key successor stays live this cycle");
                }
                NewTableKeyArm::WeakKey => {
                    assert!(heap.alive(key), "pinned weak key stays live");
                    assert!(
                        heap.alive(target.expect("weak-key value")),
                        "unmarked table value stays live this cycle"
                    );
                    assert!(
                        heap.alive(value_child.expect("value successor")),
                        "value successor stays live this cycle"
                    );
                }
                NewTableKeyArm::WeakBoth => {
                    assert!(!heap.alive(key), "weak key is not kept by the table");
                    assert!(
                        !heap.alive(target.expect("weak value")),
                        "weak value is not kept by the table"
                    );
                }
                NewTableKeyArm::StringValue => {
                    assert!(heap.alive(key), "string payload key stays live this cycle");
                    assert!(
                        heap.alive(key_child),
                        "string payload key successor stays live this cycle"
                    );
                }
                NewTableKeyArm::SameTarget => {
                    assert!(heap.alive(key), "same target stays live this cycle");
                    assert!(
                        heap.alive(key_child),
                        "same target successor stays live this cycle"
                    );
                }
                NewTableKeyArm::Omitted => {
                    assert!(!heap.alive(key), "omitted key is collected");
                    assert!(
                        !heap.alive(target.expect("omitted value")),
                        "omitted value is collected"
                    );
                    assert!(
                        heap.find_key(owner.slot(), &super::EdgeKey::object(key))
                            .is_none(),
                        "omit leaves no record"
                    );
                }
            }
            if matches!(
                arm,
                NewTableKeyArm::Strong
                    | NewTableKeyArm::WeakValue
                    | NewTableKeyArm::StringValue
                    | NewTableKeyArm::SameTarget
            ) {
                heap.clear_object_edge(owner, key)?;
                let _again = heap.collect();
                assert!(!heap.alive(key), "deleting the new edge collects the key");
                assert!(
                    !heap.alive(key_child),
                    "the key successor dies with the key"
                );
                if let Some(target) = target {
                    assert!(
                        !heap.alive(target),
                        "deleting the new edge collects the value"
                    );
                }
            }
            Ok(())
        })
        .expect("cycle end");
    });
}

#[test]
fn gray_new_table_key_full_edges_refuse_without_gap() {
    crate::with_idle_vm(|vm| {
        use crate::heap;

        let mut host = vm.host_context();
        heap::install(&mut host, limits(4, 2, 2)).expect("install");
        let (owner, sentinel, key, target) = heap::with_heap(&mut host, |heap| {
            let owner = heap.alloc()?;
            let sentinel = heap.alloc()?;
            let key = heap.alloc()?;
            let target = heap.alloc()?;
            heap.set_named_scalar(owner, "old", Value::string("kept"))?;
            heap.set_named_scalar(sentinel, "pending", Value::Bool(true))?;
            assert!(
                !owner_chain_has_gap(heap, owner.slot()),
                "the capacity arm starts with no recyclable gap"
            );
            let frame = heap.enter_roots(2)?;
            heap.write_root(frame, 0, sentinel)?;
            heap.write_root(frame, 1, owner)?;
            Ok((owner, sentinel, key, target))
        })
        .expect("plant");
        heap::open_fixed_step(&mut host).expect("open");
        heap::with_heap(&mut host, |heap| {
            let (_, old) = heap.locate_named(owner.slot(), "old").expect("old record");
            let (_, sentinel_record) = heap
                .locate_named(sentinel.slot(), "pending")
                .expect("pending record");
            let mut parked = ParkedMark::resume(heap).expect("resume");
            pause_at(&mut parked, owner.slot(), old);
            let pending = pause_at(&mut parked, sentinel.slot(), sentinel_record);
            let before = parked.meter();
            parked.with_saved_heap(|heap| {
                assert!(!owner_chain_has_gap(heap, owner.slot()));
                assert!(
                    heap.find_key(owner.slot(), &super::EdgeKey::object(key))
                        .is_none()
                );
                assert_eq!(
                    heap.set_object_edge(owner, key, target),
                    Err(HeapError::EdgeCapacity),
                    "a full chain with no gap does not insert"
                );
                assert!(
                    heap.find_key(owner.slot(), &super::EdgeKey::object(key))
                        .is_none(),
                    "capacity refusal leaves no record"
                );
                assert_eq!(heap.alloc(), Err(HeapError::NodeCapacity));
                Ok(())
            })?;
            assert!(
                !parked.marked(key),
                "capacity refusal does not mark the key"
            );
            assert!(
                !parked.marked(target),
                "capacity refusal does not mark the value"
            );
            assert_eq!(
                parked.meter().parked_barrier_marks,
                before.parked_barrier_marks
            );
            assert_eq!(
                parked.meter().node_slot_visits,
                before.node_slot_visits,
                "capacity refusal does not enqueue"
            );
            assert!(same_pending(parked.pending(), Some(pending)));
            assert_write_did_not_scan(&before, &parked.meter());
            parked.park().expect("park");
            Ok(())
        })
        .expect("refuse");
    });
}

fn owner_chain_has_gap(heap: &LuaHeap, owner: u32) -> bool {
    let mut cursor = heap.slots[owner as usize].edge_head;
    while let Some(index) = cursor {
        let slot = &heap.edge_slots[index as usize];
        if slot
            .edge
            .as_ref()
            .is_some_and(|edge| edge.value.is_cursor_gap())
        {
            return true;
        }
        cursor = slot.next;
    }
    false
}

const GAP_TEXT: &str = "gap";
const PEER_TEXT: &str = "peer";
const GAP_NUMBER: f64 = -7.0;
const PEER_NUMBER: f64 = -8.0;
const NUMBER_SPELLING: &str = "-7";
const GAP_CHILD: &str = "child";

#[derive(Clone, Copy)]
enum OrdinaryGapKey {
    Text,
    Number,
}

#[derive(Clone, Copy)]
enum OrdinaryGapWrite {
    Strong,
    WeakValue,
    Omitted,
}

fn ordinary_gap_retains(write: OrdinaryGapWrite) -> bool {
    matches!(write, OrdinaryGapWrite::Strong)
}

fn locate_ordinary(
    heap: &LuaHeap,
    owner: TableRef,
    key: OrdinaryGapKey,
    peer: bool,
) -> Option<u32> {
    match (key, peer) {
        (OrdinaryGapKey::Text, false) => heap
            .locate_named(owner.slot(), GAP_TEXT)
            .map(|(_, index)| index),
        (OrdinaryGapKey::Text, true) => heap
            .locate_named(owner.slot(), PEER_TEXT)
            .map(|(_, index)| index),
        (OrdinaryGapKey::Number, false) => {
            heap.find_key(owner.slot(), &super::EdgeKey::number(GAP_NUMBER))
        }
        (OrdinaryGapKey::Number, true) => {
            heap.find_key(owner.slot(), &super::EdgeKey::number(PEER_NUMBER))
        }
    }
}

fn read_ordinary(heap: &LuaHeap, owner: TableRef, key: OrdinaryGapKey) -> Result<Value, HeapError> {
    match key {
        OrdinaryGapKey::Text => heap.named_value(owner, GAP_TEXT),
        OrdinaryGapKey::Number => heap.number_value(owner, GAP_NUMBER),
    }
}

fn write_ordinary_table(
    heap: &mut LuaHeap,
    owner: TableRef,
    key: OrdinaryGapKey,
    target: TableRef,
) -> Result<(), HeapError> {
    match key {
        OrdinaryGapKey::Text => heap.set_named_edge(owner, GAP_TEXT, target),
        OrdinaryGapKey::Number => heap.set_number_edge(owner, GAP_NUMBER, target),
    }
}

fn write_ordinary_peer(
    heap: &mut LuaHeap,
    owner: TableRef,
    key: OrdinaryGapKey,
) -> Result<(), HeapError> {
    match key {
        OrdinaryGapKey::Text => heap.set_named_scalar(owner, PEER_TEXT, Value::string("peer")),
        OrdinaryGapKey::Number => heap.set_number_scalar(owner, PEER_NUMBER, Value::string("peer")),
    }
}

fn clear_ordinary(
    heap: &mut LuaHeap,
    owner: TableRef,
    key: OrdinaryGapKey,
) -> Result<(), HeapError> {
    match key {
        OrdinaryGapKey::Text => heap.set_named_scalar(owner, GAP_TEXT, Value::Null),
        OrdinaryGapKey::Number => heap.clear_number_edge(owner, GAP_NUMBER),
    }
}

fn stored_table(heap: &LuaHeap, index: u32) -> Option<TableRef> {
    let edge = heap.edge_slots[index as usize].edge.as_ref()?;
    match &edge.value {
        super::EdgeValue::Table { slot, generation } => {
            Some(TableRef::table(heap.domain, *slot, *generation))
        }
        _ => None,
    }
}

fn record_is_gap(heap: &LuaHeap, index: u32) -> bool {
    heap.edge_slots[index as usize]
        .edge
        .as_ref()
        .is_some_and(|edge| edge.value.is_cursor_gap())
}

fn pause_on_later_pass(parked: &mut ParkedMark<'_>, owner: u32, cursor: u32) -> PendingScan {
    let sweep = parked.meter().sweep_edge_checks;
    for _ in 0..HARNESS_BOUND {
        let view = parked.pulse(1);
        assert!(
            !view.finished,
            "mark finished before owner {owner} record {cursor} was pending"
        );
        assert_eq!(
            view.meter.sweep_edge_checks, sweep,
            "a later-pass pulse must not sweep"
        );
        if let Some(pending) = parked.pending()
            && pending.owner == owner
            && pending.cursor == cursor
        {
            assert_eq!(parked.phase_name(), "gray");
            assert_eq!(
                view.meter.mark_passes, 2,
                "the pending record is in the new pass"
            );
            assert!(parked.mutation_open());
            return pending;
        }
    }
    panic!("pending record {cursor} on owner {owner} was not observed");
}

fn slot_generation(heap: &LuaHeap, table: TableRef) -> u32 {
    heap.slots[table.slot() as usize].generation
}

fn root_holds(slot: &Option<super::Rooted>, table: TableRef) -> bool {
    matches!(
        slot,
        Some(super::Rooted::Table(found))
            if found.slot() == table.slot() && found.generation() == table.generation()
    )
}

/// Refills a physical gap left by a real weak-value cycle.
///
/// The first collection clears the old value and keeps the record. The second
/// cycle is a saved Gray pause after that record has been charged. The refill
/// uses the ordinary text or number writer, not the published tail entry.
fn ordinary_gap_refill_after_real_clear(key: OrdinaryGapKey, write: OrdinaryGapWrite) {
    use crate::heap::{self, StepOutcome};

    let retained = ordinary_gap_retains(write);
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(16, 4, 16)).expect("install");
        let built = heap::with_heap(&mut host, |heap| {
            let mode = heap.alloc()?;
            heap.set_mode_scalar(mode, Value::string("v"))?;
            let owner = heap.alloc()?;
            let sentinel = heap.alloc()?;
            let weak = heap.alloc()?;
            let old = heap.alloc()?;
            heap.set_metatable(owner, Some(mode))?;
            heap.set_metatable(weak, Some(mode))?;
            let frame = heap.enter_roots(4)?;
            // Gray pops the highest root first: owner, then the pending sentinel.
            heap.write_root(frame, 0, mode)?;
            heap.write_root(frame, 1, weak)?;
            heap.write_root(frame, 2, sentinel)?;
            heap.write_root(frame, 3, owner)?;
            write_ordinary_table(heap, owner, key, old)?;
            write_ordinary_peer(heap, owner, key)?;
            heap.issue_carrier(old)?;
            heap.set_named_scalar(sentinel, "pending", Value::Bool(true))?;
            let gap_key = heap
                .next_guest(owner, &Value::Null)?
                .expect("the old value is the first cursor")
                .0;
            let peer = heap
                .next_guest(owner, &gap_key)?
                .expect("the sibling follows the old value");
            assert!(heap.next_guest(owner, &peer.0)?.is_none());
            let gap_index = locate_ordinary(heap, owner, key, false).expect("gap record");
            let peer_index = locate_ordinary(heap, owner, key, true).expect("peer record");
            let sentinel_index = heap
                .locate_named(sentinel.slot(), "pending")
                .map(|(_, index)| index)
                .expect("sentinel record");
            Ok((
                mode,
                owner,
                sentinel,
                weak,
                old,
                gap_key,
                peer,
                gap_index,
                peer_index,
                sentinel_index,
            ))
        })
        .expect("first-cycle graph");
        let (
            mode,
            owner,
            sentinel,
            weak,
            old,
            gap_key,
            peer,
            gap_index,
            peer_index,
            sentinel_index,
        ) = built;
        let old_generation = old.generation();

        heap::collect(&mut host).expect("weak values clear in a full cycle");
        heap::with_heap(&mut host, |heap| {
            assert!(!heap.alive(old), "the old node is not live");
            assert_ne!(
                slot_generation(heap, old),
                old_generation,
                "the old node generation was released"
            );
            assert!(
                record_is_gap(heap, gap_index),
                "the original record is a physical gap"
            );
            assert_eq!(
                locate_ordinary(heap, owner, key, false),
                Some(gap_index),
                "the gap kept its record index"
            );
            assert_eq!(
                locate_ordinary(heap, owner, key, true),
                Some(peer_index),
                "the other same-owner record remains"
            );
            assert_eq!(read_ordinary(heap, owner, key)?, Value::Null);
            assert_eq!(heap.next_guest(owner, &Value::Null)?, Some(peer.clone()));
            assert_eq!(
                heap.next_guest(owner, &gap_key)?,
                Some(peer.clone()),
                "the saved next cursor continues"
            );
            assert_eq!(heap.stats().collections, 1);
            assert_eq!(heap.scan_meter().mark_passes, 1);
            if matches!(key, OrdinaryGapKey::Number) {
                let spelling = super::EdgeKey::named(NUMBER_SPELLING);
                assert_ne!(
                    super::EdgeKey::number(GAP_NUMBER),
                    spelling,
                    "a negative integer does not alias its spelling"
                );
                assert!(heap.find_key(owner.slot(), &spelling).is_none());
                assert_ne!(gap_key, Value::string(NUMBER_SPELLING));
            }
            Ok(())
        })
        .expect("physical gap after the first cycle");

        assert!(
            heap::fixed_step_idle(&mut host).expect("between cycles"),
            "the mode change is idle"
        );
        let (target, child, orphan, owner_carrier, target_carrier) =
            heap::with_heap(&mut host, |heap| {
                if !matches!(write, OrdinaryGapWrite::WeakValue) {
                    heap.set_metatable(owner, None)?;
                    assert_eq!(heap.metatable(owner)?, None);
                } else {
                    assert_eq!(heap.metatable(owner)?, Some(mode));
                }
                let target = heap.alloc()?;
                let child = heap.alloc()?;
                let orphan = heap.alloc()?;
                heap.set_named_edge(target, GAP_CHILD, child)?;
                heap.set_named_edge(weak, "watch", target)?;
                heap.set_named_edge(weak, GAP_CHILD, child)?;
                heap.set_named_edge(weak, "orphan", orphan)?;
                assert!(
                    heap.roots.iter().all(|slot| !root_holds(slot, target)
                        && !root_holds(slot, child)
                        && !root_holds(slot, orphan)),
                    "the new objects have no fixed root"
                );
                assert_eq!(heap.stats().live_roots, 4);
                let owner_carrier = heap.issue_carrier(owner)?;
                let target_carrier = heap.issue_carrier(target)?;
                Ok((target, child, orphan, owner_carrier, target_carrier))
            })
            .expect("weak observation only");

        heap::open_fixed_step(&mut host).expect("second Gray cycle");
        heap::with_heap(&mut host, |heap| {
            let mut parked = ParkedMark::resume(heap).expect("resume second mark");
            let seen = pause_on_later_pass(&mut parked, owner.slot(), gap_index);
            assert_eq!(parked.meter().mark_passes, 2, "this is a new mark pass");
            assert_eq!((seen.owner, seen.cursor), (owner.slot(), gap_index));
            let gaps_before = parked.meter().outgoing_gap_checks;
            let pending = pause_on_later_pass(&mut parked, sentinel.slot(), sentinel_index);
            assert_eq!(
                parked.meter().outgoing_gap_checks,
                gaps_before + 1,
                "the original gap record was charged"
            );
            assert!(parked.marked(owner), "the owner was scanned");
            assert!(
                !parked.marked(target) && !parked.marked(child),
                "weak observation does not mark the refill"
            );
            assert_eq!(
                (pending.owner, pending.cursor),
                (sentinel.slot(), sentinel_index),
                "another owner is still pending"
            );
            parked.park()?;
            assert!(
                record_is_gap(heap, gap_index),
                "scanning the gap did not rewrite the record"
            );
            let chain_next = heap.edge_slots[gap_index as usize].next;
            let occupied = heap.stats().edge_slots;
            let live = heap.stats().live_edges;
            let before = heap.scan_meter();
            if !matches!(write, OrdinaryGapWrite::Omitted) {
                write_ordinary_table(heap, owner, key, target)?;
            }
            assert_eq!(locate_ordinary(heap, owner, key, false), Some(gap_index));
            assert_eq!(locate_ordinary(heap, owner, key, true), Some(peer_index));
            assert_eq!(heap.edge_slots[gap_index as usize].next, chain_next);
            assert_eq!(heap.issued_value(owner)?, owner_carrier);
            assert_eq!(heap.issued_value(target)?, target_carrier);
            assert_eq!(
                heap.stats().edge_slots,
                occupied,
                "replace does not allocate"
            );
            let live_delta = u32::from(!matches!(write, OrdinaryGapWrite::Omitted));
            assert_eq!(heap.stats().live_edges, live + live_delta);
            if !matches!(write, OrdinaryGapWrite::Omitted) {
                let index = locate_ordinary(heap, owner, key, false).expect("refilled record");
                assert_eq!(stored_table(heap, index), Some(target));
            } else {
                assert!(
                    record_is_gap(heap, gap_index),
                    "an omitted write leaves the gap"
                );
                assert_eq!(read_ordinary(heap, owner, key)?, Value::Null);
            }
            let parked = ParkedMark::resume(heap).expect("mark survived the ordinary write");
            let cursor = parked.pending().expect("sentinel still pending");
            assert_eq!(
                (cursor.owner, cursor.cursor),
                (pending.owner, pending.cursor)
            );
            assert!(!parked.marked(child), "the write must not scan the child");
            let after = parked.meter();
            assert_eq!(after.mark_passes, 2);
            assert_eq!(after.mark_passes, before.mark_passes);
            assert_eq!(after.outgoing_edge_checks, before.outgoing_edge_checks);
            assert_eq!(after.outgoing_gap_checks, before.outgoing_gap_checks);
            assert_eq!(after.owner_head_checks, before.owner_head_checks);
            assert_eq!(after.root_slot_visits, before.root_slot_visits);
            assert_eq!(after.parked_cursor_repairs, before.parked_cursor_repairs);
            parked.park()?;
            Ok(())
        })
        .expect("ordinary refill while Gray is parked");

        let published = heap::step_fixed_frames(&mut host, 128).expect("publish");
        assert!(
            matches!(published, StepOutcome::Published { .. }),
            "{published:?}"
        );
        assert_eq!(heap::stats(&mut host).expect("published").collections, 1);
        heap::with_heap(&mut host, |heap| {
            assert_eq!(
                heap.set_named_edge(owner, "blocked", sentinel),
                Err(HeapError::SweepPending)
            );
            if matches!(write, OrdinaryGapWrite::WeakValue) {
                assert_eq!(read_ordinary(heap, owner, key)?, Value::Null);
                assert!(
                    record_is_gap(heap, gap_index),
                    "weak publication restores the gap"
                );
                assert_eq!(locate_ordinary(heap, owner, key, false), Some(gap_index));
            }
            if matches!(write, OrdinaryGapWrite::Omitted) {
                assert!(record_is_gap(heap, gap_index));
                assert_eq!(read_ordinary(heap, owner, key)?, Value::Null);
            }
            Ok(())
        })
        .expect("publication");
        let done = heap::step_fixed_frames(&mut host, 128).expect("finish this cycle");
        assert!(matches!(done, StepOutcome::Completed(_)), "{done:?}");
        assert!(heap::fixed_step_idle(&mut host).expect("idle"));
        heap::with_heap(&mut host, |heap| {
            assert_eq!(
                heap.alive(target),
                retained,
                "ordinary gap refill must keep a strong value alive through this cycle"
            );
            assert_eq!(
                heap.alive(child),
                retained,
                "the child is reached only by the continued scan"
            );
            assert!(
                !heap.alive(orphan),
                "the weak orphan proves this cycle released nodes"
            );
            assert_ne!(slot_generation(heap, orphan), orphan.generation());
            assert_eq!(heap.stats().collections, 2);
            assert_eq!(heap.scan_meter().mark_passes, 2);
            assert_eq!(locate_ordinary(heap, owner, key, true), Some(peer_index));
            if retained {
                assert_eq!(slot_generation(heap, target), target.generation());
                assert_eq!(read_ordinary(heap, owner, key)?, target_carrier);
                assert_eq!(heap.named_edge(target, GAP_CHILD)?, Some(child));
                clear_ordinary(heap, owner, key)?;
            } else {
                assert_ne!(slot_generation(heap, target), target.generation());
                assert!(record_is_gap(heap, gap_index));
            }
            Ok(())
        })
        .expect("this cycle's release");
        if retained {
            heap::collect(&mut host).expect("next cycle after the only strong edge is removed");
            heap::with_heap(&mut host, |heap| {
                assert!(!heap.alive(target) && !heap.alive(child));
                assert_ne!(slot_generation(heap, target), target.generation());
                assert_ne!(slot_generation(heap, child), child.generation());
                assert_eq!(heap.stats().collections, 3);
                assert_eq!(heap.scan_meter().mark_passes, 3);
                assert!(locate_ordinary(heap, owner, key, false).is_none());
                Ok(())
            })
            .expect("later reclamation");
        }
    });
}

#[test]
fn gray_ordinary_gap_text_strong_refill_keeps_the_new_graph() {
    ordinary_gap_refill_after_real_clear(OrdinaryGapKey::Text, OrdinaryGapWrite::Strong);
}

#[test]
fn gray_ordinary_gap_number_strong_refill_keeps_the_new_graph() {
    ordinary_gap_refill_after_real_clear(OrdinaryGapKey::Number, OrdinaryGapWrite::Strong);
}

#[test]
fn gray_ordinary_gap_text_weak_refill_publishes_nil() {
    ordinary_gap_refill_after_real_clear(OrdinaryGapKey::Text, OrdinaryGapWrite::WeakValue);
}

#[test]
fn gray_ordinary_gap_number_weak_refill_publishes_nil() {
    ordinary_gap_refill_after_real_clear(OrdinaryGapKey::Number, OrdinaryGapWrite::WeakValue);
}

#[test]
fn gray_ordinary_gap_text_omitted_write_keeps_the_gap() {
    ordinary_gap_refill_after_real_clear(OrdinaryGapKey::Text, OrdinaryGapWrite::Omitted);
}

#[test]
fn gray_ordinary_gap_number_omitted_write_keeps_the_gap() {
    ordinary_gap_refill_after_real_clear(OrdinaryGapKey::Number, OrdinaryGapWrite::Omitted);
}

#[derive(Clone, Copy, Debug)]
enum MetatableWrite {
    Attach,
    Replace,
    OmitAttach,
    OmitReplace,
    WeakReplace,
}

fn metatable_write_publishes(arm: MetatableWrite) -> bool {
    matches!(
        arm,
        MetatableWrite::Attach | MetatableWrite::Replace | MetatableWrite::WeakReplace
    )
}

fn metatable_write_replaces(arm: MetatableWrite) -> bool {
    matches!(
        arm,
        MetatableWrite::Replace | MetatableWrite::OmitReplace | MetatableWrite::WeakReplace
    )
}

struct MetatablePlant {
    frame: RootFrameId,
    sentinel: TableRef,
    owner: TableRef,
    witness: TableRef,
    old: Option<TableRef>,
    fresh: TableRef,
    child: TableRef,
    orphan: TableRef,
    owner_record: u32,
    sentinel_record: u32,
}

struct MetatableWriteSnap {
    fresh_marked: bool,
    child_marked: bool,
    visits: u64,
    pushed: u64,
    barriers: u64,
}

fn gray_metatable_after_owner_scan(arm: MetatableWrite) {
    let publishes = metatable_write_publishes(arm);
    let replaces = metatable_write_replaces(arm);
    let weak_owner = matches!(arm, MetatableWrite::WeakReplace);
    crate::with_idle_vm(|vm| {
        use crate::heap::{self, StepOutcome};
        let mut host = vm.host_context();
        heap::install(&mut host, limits(16, 4, 16)).expect("install");
        let planted = heap::with_heap(&mut host, |heap| -> Result<MetatablePlant, HeapError> {
            let sentinel = heap.alloc()?;
            let owner = heap.alloc()?;
            let witness = heap.alloc()?;
            let witness_meta = heap.alloc()?;
            let fresh = heap.alloc()?;
            let child = heap.alloc()?;
            let orphan = heap.alloc()?;
            let dust = heap.alloc()?;
            let old = if replaces { Some(heap.alloc()?) } else { None };
            heap.set_named_scalar(sentinel, "pending", Value::Bool(true))?;
            heap.set_named_scalar(owner, "kept", Value::string("kept"))?;
            heap.set_metatable(witness, Some(witness_meta))?;
            heap.set_mode_scalar(witness_meta, Value::string("v"))?;
            heap.set_named_edge(fresh, "__index", child)?;
            heap.set_named_scalar(child, "field", Value::string("41"))?;
            heap.set_named_edge(witness, "fresh", fresh)?;
            heap.set_named_edge(witness, "child", child)?;
            heap.set_named_edge(witness, "orphan", orphan)?;
            if let Some(old) = old {
                if weak_owner {
                    heap.set_mode_scalar(old, Value::string("v"))?;
                    heap.set_mode_scalar(fresh, Value::string("v"))?;
                }
                heap.set_metatable(owner, Some(old))?;
            }
            let frame = heap.enter_roots(3)?;
            heap.write_root(frame, 0, sentinel)?;
            heap.write_root(frame, 1, owner)?;
            heap.write_root(frame, 2, witness)?;
            let owner_record = heap
                .locate_named(owner.slot(), "kept")
                .expect("owner record")
                .1;
            let sentinel_record = heap
                .locate_named(sentinel.slot(), "pending")
                .expect("sentinel record")
                .1;
            let _ = dust;
            Ok(MetatablePlant {
                frame,
                sentinel,
                owner,
                witness,
                old,
                fresh,
                child,
                orphan,
                owner_record,
                sentinel_record,
            })
        })
        .expect("plant");
        heap::open_fixed_step(&mut host).expect("open");
        let snap = heap::with_heap(&mut host, |heap| -> Result<MetatableWriteSnap, HeapError> {
            let mut parked = ParkedMark::resume(heap).expect("resume saved mark");
            let before_edges = parked.meter().outgoing_edge_checks;
            pause_at(&mut parked, planted.owner.slot(), planted.owner_record);
            pause_at(
                &mut parked,
                planted.sentinel.slot(),
                planted.sentinel_record,
            );
            assert_eq!(parked.phase_name(), "gray", "collector is parked in Gray");
            assert!(parked.mutation_open(), "Gray still accepts a write");
            assert!(
                parked.meter().outgoing_edge_checks > before_edges,
                "the owner record was charged, so the owner scan has moved on"
            );
            assert_eq!(parked.phase_name(), "gray");
            assert!(parked.mutation_open());
            assert!(parked.pending().is_some(), "sentinel work is still pending");
            assert!(parked.marked(planted.owner), "the owner is already marked");
            assert!(
                !parked.marked(planted.fresh),
                "the new metatable starts white"
            );
            assert!(
                !parked.marked(planted.child),
                "the index child starts white"
            );
            if let Some(old) = planted.old {
                assert!(
                    parked.marked(old),
                    "the old metatable was followed when the owner scan began"
                );
            }
            parked.with_saved_heap(|heap| -> Result<(), HeapError> {
                assert_eq!(
                    heap.stats().live_roots,
                    3,
                    "only the three frame roots exist"
                );
                for slot in 0..3 {
                    let rooted = heap.rooted_table(planted.frame, slot)?;
                    assert_ne!(rooted, Some(planted.fresh), "fresh has no independent root");
                    assert_ne!(rooted, Some(planted.child), "child has no independent root");
                    assert_ne!(
                        rooted,
                        Some(planted.orphan),
                        "orphan has no independent root"
                    );
                }
                match planted.old {
                    Some(old) => assert_eq!(heap.metatable(planted.owner)?, Some(old)),
                    None => assert_eq!(heap.metatable(planted.owner)?, None),
                }
                if weak_owner {
                    let old = planted
                        .old
                        .expect("weak arm replaces an existing metatable");
                    let old_mode = heap.named_value(old, "__mode")?;
                    let new_mode = heap.named_value(planted.fresh, "__mode")?;
                    assert_eq!(old_mode, Value::string("v"));
                    assert_eq!(new_mode, old_mode, "old and new weak modes stay equal");
                }
                Ok(())
            })?;
            let before = parked.meter();
            let pending = parked.pending();
            if publishes {
                parked.with_saved_heap(|heap| -> Result<(), HeapError> {
                    heap.set_metatable(planted.owner, Some(planted.fresh))?;
                    let read = heap.metatable(planted.owner)?.expect("immediate readback");
                    assert_eq!(read.slot(), planted.fresh.slot());
                    assert_eq!(
                        read.generation(),
                        planted.fresh.generation(),
                        "the setter stores the original generation"
                    );
                    Ok(())
                })?;
            }
            let after = parked.meter();
            assert_write_did_not_scan(&before, &after);
            assert!(
                same_pending(pending, parked.pending()),
                "the write does not move the pending cursor"
            );
            assert_eq!(
                parked.phase_name(),
                "gray",
                "the write does not finish the mark"
            );
            assert!(
                parked.pending().is_some(),
                "sentinel work remains after the write"
            );
            assert_eq!(
                after.metatable_edge_checks, before.metatable_edge_checks,
                "the setter does not scan the owner metatable chain"
            );
            assert_eq!(
                after.weak_mode_lookups, before.weak_mode_lookups,
                "the setter does not consult the weak-mode cache"
            );
            let snap = MetatableWriteSnap {
                fresh_marked: parked.marked(planted.fresh),
                child_marked: parked.marked(planted.child),
                visits: after.node_slot_visits - before.node_slot_visits,
                pushed: after.scratch.work_pushed - before.scratch.work_pushed,
                barriers: after.parked_barrier_marks - before.parked_barrier_marks,
            };
            parked.park()?;
            Ok(snap)
        })
        .expect("pause");
        let published = heap::step_fixed_frames(&mut host, 128).expect("publish");
        assert!(
            matches!(published, StepOutcome::Published { .. }),
            "the cycle publishes weak clears before release: {published:?}"
        );
        let finished = heap::step_fixed_frames(&mut host, 128).expect("finish");
        assert!(
            matches!(finished, StepOutcome::Completed(_)),
            "the continuation finishes the cycle: {finished:?}"
        );
        assert!(heap::fixed_step_idle(&mut host).expect("idle"));
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            assert!(
                !heap.alive(planted.orphan),
                "the weak orphan is cleared in this cycle"
            );
            assert_eq!(heap.named_edge(planted.witness, "orphan")?, None);
            assert_eq!(
                heap.alive(planted.fresh),
                publishes,
                "after this cycle the new metatable is alive only through the strong metatable edge"
            );
            assert_eq!(heap.alive(planted.child), publishes);
            if publishes {
                let mounted = heap.metatable(planted.owner)?.expect("mounted");
                assert_eq!(mounted.slot(), planted.fresh.slot());
                assert_eq!(mounted.generation(), planted.fresh.generation());
                let index = heap.named_edge(mounted, "__index")?.expect("index child");
                assert_eq!(index.slot(), planted.child.slot());
                assert_eq!(index.generation(), planted.child.generation());
                assert_eq!(heap.named_value(index, "field")?, Value::string("41"));
                assert_eq!(
                    heap.named_edge(planted.witness, "fresh")?,
                    Some(planted.fresh),
                    "the weak witness reads the original object and does not root it"
                );
                assert!(
                    snap.fresh_marked,
                    "the write grays only the direct metatable"
                );
                assert!(
                    !snap.child_marked,
                    "the index child stays white until the continued scan"
                );
                assert_eq!(snap.visits, 2, "the setter visits only the new metatable");
                assert_eq!(snap.pushed, 1);
                assert_eq!(snap.barriers, 1);
                heap.set_metatable(planted.owner, None)?;
                assert_eq!(heap.metatable(planted.owner)?, None);
                let _ = heap.collect();
                assert!(
                    !heap.alive(planted.fresh),
                    "removing the only metatable edge collects the new metatable"
                );
                assert!(
                    !heap.alive(planted.child),
                    "the index child has no other root"
                );
                assert!(heap.alive(planted.owner));
                assert!(heap.alive(planted.sentinel));
            } else {
                assert!(!snap.fresh_marked);
                assert!(!snap.child_marked);
                assert_eq!(snap.visits, 0);
                assert_eq!(snap.pushed, 0);
                assert_eq!(snap.barriers, 0);
                match planted.old {
                    Some(old) => {
                        let still = heap.metatable(planted.owner)?.expect("old edge remains");
                        assert_eq!(still.slot(), old.slot());
                        assert_eq!(still.generation(), old.generation());
                    }
                    None => assert_eq!(heap.metatable(planted.owner)?, None),
                }
                assert_eq!(heap.named_edge(planted.witness, "fresh")?, None);
                assert_eq!(heap.named_edge(planted.witness, "child")?, None);
            }
            Ok(())
        })
        .expect("observe");
    });
}

#[test]
fn gray_metatable_attach_keeps_the_new_graph() {
    gray_metatable_after_owner_scan(MetatableWrite::Attach);
}

#[test]
fn gray_metatable_replace_keeps_the_new_graph() {
    gray_metatable_after_owner_scan(MetatableWrite::Replace);
}

#[test]
fn gray_metatable_attach_omitted_collects_the_new_graph() {
    gray_metatable_after_owner_scan(MetatableWrite::OmitAttach);
}

#[test]
fn gray_metatable_replace_omitted_keeps_the_old_edge() {
    gray_metatable_after_owner_scan(MetatableWrite::OmitReplace);
}

#[test]
fn gray_weak_value_owner_keeps_an_equal_mode_metatable() {
    gray_metatable_after_owner_scan(MetatableWrite::WeakReplace);
}

#[test]
fn published_tail_set_metatable_refuses_without_changing_identity() {
    crate::with_idle_vm(|vm| {
        use crate::heap::{self, StepOutcome};
        let mut host = vm.host_context();
        heap::install(&mut host, limits(8, 2, 8)).expect("install");
        let (owner, old, fresh) = heap::with_heap(&mut host, |heap| -> Result<_, HeapError> {
            let sentinel = heap.alloc()?;
            let owner = heap.alloc()?;
            let old = heap.alloc()?;
            let fresh = heap.alloc()?;
            let dust = heap.alloc()?;
            heap.set_metatable(owner, Some(old))?;
            heap.set_named_scalar(owner, "kept", Value::string("kept"))?;
            heap.set_named_scalar(sentinel, "pending", Value::Bool(true))?;
            let frame = heap.enter_roots(2)?;
            heap.write_root(frame, 0, sentinel)?;
            heap.write_root(frame, 1, owner)?;
            let _ = (dust, frame);
            Ok((owner, old, fresh))
        })
        .expect("plant");
        heap::open_fixed_step(&mut host).expect("open");
        let published = heap::step_fixed_frames(&mut host, 128).expect("publish");
        assert!(
            matches!(published, StepOutcome::Published { .. }),
            "{published:?}"
        );
        let tail_before = heap::saved_tail_state(&mut host)
            .expect("tail read")
            .expect("published tail");
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let stored = heap.slots[owner.slot() as usize].metatable;
            let edges = heap.metatable_edges;
            let stats = heap.stats();
            let meter = heap.scan_meter();
            assert_eq!(heap.metatable(owner)?, Some(old));
            assert_eq!(
                heap.set_metatable(owner, Some(fresh)),
                Err(HeapError::SweepPending)
            );
            assert_eq!(
                heap.set_metatable(owner, None),
                Err(HeapError::SweepPending)
            );
            assert_eq!(heap.metatable(owner)?, Some(old));
            assert_eq!(heap.slots[owner.slot() as usize].metatable, stored);
            assert_eq!(heap.metatable_edges, edges);
            assert_eq!(heap.stats(), stats);
            assert_eq!(heap.scan_meter(), meter);
            assert_eq!(heap.named_value(owner, "kept")?, Value::string("kept"));
            assert!(
                heap.alive(fresh),
                "refusal does not release the rejected target"
            );
            assert!(heap.alive(old));
            Ok(())
        })
        .expect("refusal");
        let tail_after = heap::saved_tail_state(&mut host)
            .expect("tail read")
            .expect("tail still saved");
        assert_eq!(tail_before.phase, tail_after.phase);
        assert_eq!(tail_before.cursor, tail_after.cursor);
        assert_eq!(tail_before.live_roots, tail_after.live_roots);
        assert_eq!(tail_before.dead_len, tail_after.dead_len);
        let finished = heap::step_fixed_frames(&mut host, 128).expect("finish");
        assert!(
            matches!(finished, StepOutcome::Completed(_)),
            "{finished:?}"
        );
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let still = heap.metatable(owner)?.expect("old edge remains");
            assert_eq!(still.slot(), old.slot());
            assert_eq!(still.generation(), old.generation());
            assert!(
                !heap.alive(fresh),
                "the rejected metatable was never stored"
            );
            Ok(())
        })
        .expect("after release");
    });
}

struct ClosedCellPlant {
    frame: RootFrameId,
    sentinel: TableRef,
    sentinel_record: u32,
    getter: ClosureRef,
    setter: ClosureRef,
    setter_edge: u32,
    cell: UpvalueRef,
    current_edge: Option<u32>,
    witness: TableRef,
    fresh: TableRef,
    child: TableRef,
    orphan: TableRef,
    old: Option<TableRef>,
}

fn plant_closed_cell(heap: &mut LuaHeap, replaces: bool) -> Result<ClosedCellPlant, HeapError> {
    let frame = heap.enter_roots(5)?;
    let sentinel = heap.alloc()?;
    heap.set_named_scalar(sentinel, "pending", Value::string("wait"))?;
    let (_, sentinel_record) = heap
        .locate_named(sentinel.slot(), "pending")
        .expect("sentinel record");
    let getter = heap.alloc_closure()?;
    let setter = heap.alloc_closure()?;
    let cell = heap.alloc_upvalue()?;
    heap.set_upvalue_edge(getter, cell)?;
    heap.set_upvalue_edge(setter, cell)?;
    let setter_edge = heap
        .find_key(setter.slot(), &super::EdgeKey::Field(super::UPVALUE_EDGE))
        .expect("setter upvalue edge");
    let old = if replaces {
        let old = heap.alloc()?;
        heap.write_upvalue(cell, old)?;
        Some(old)
    } else {
        None
    };
    let current_edge = old.map(|_| {
        heap.find_key(cell.slot(), &super::EdgeKey::Field(super::CURRENT_EDGE))
            .expect("cell value edge")
    });
    let witness = heap.alloc()?;
    let witness_meta = heap.alloc()?;
    heap.set_mode_scalar(witness_meta, Value::string("v"))?;
    heap.set_metatable(witness, Some(witness_meta))?;
    let fresh = heap.alloc()?;
    let child = heap.alloc()?;
    let orphan = heap.alloc()?;
    heap.set_named_edge(fresh, "child", child)?;
    heap.set_named_scalar(child, "id", Value::string("child"))?;
    heap.set_named_edge(witness, "fresh", fresh)?;
    heap.set_named_edge(witness, "child", child)?;
    heap.set_named_edge(witness, "orphan", orphan)?;
    // Sentinel is rooted first so it is scanned last. The closures are scanned,
    // and the shared cell is finished, before that pending record is reached.
    heap.write_root(frame, 0, sentinel)?;
    heap.write_closure_root(frame, 1, getter)?;
    heap.write_closure_root(frame, 2, setter)?;
    heap.write_root(frame, 3, witness)?;
    if let Some(old) = old {
        heap.write_root(frame, 4, old)?;
    }
    Ok(ClosedCellPlant {
        frame,
        sentinel,
        sentinel_record,
        getter,
        setter,
        setter_edge,
        cell,
        current_edge,
        witness,
        fresh,
        child,
        orphan,
        old,
    })
}

fn slot_is_marked(heap: &LuaHeap, slot: u32, generation: u32, kind: super::HandleKind) -> bool {
    heap.slots.get(slot as usize).is_some_and(|stored| {
        stored.generation == generation
            && stored.kind == kind
            && matches!(stored.state, super::SlotState::Live { marked: true })
    })
}

fn assert_no_independent_strong_root(
    heap: &LuaHeap,
    fresh: TableRef,
    child: TableRef,
    orphan: TableRef,
) {
    for rooted in &heap.roots {
        if let Some(super::Rooted::Table(handle)) = rooted {
            assert_ne!(*handle, fresh, "fresh has no independent strong root");
            assert_ne!(*handle, child, "child has no independent strong root");
            assert_ne!(*handle, orphan, "orphan has no independent strong root");
        }
    }
}

struct ClosedCellSnap {
    fresh_marked: bool,
    child_marked: bool,
    orphan_marked: bool,
    visits: u64,
    pushed: u64,
    barriers: u64,
    roots: u32,
}

fn run_closed_cell(publishes: bool, replaces: bool) {
    let label = match (publishes, replaces) {
        (true, false) => "nil first write",
        (true, true) => "table value replacement",
        (false, false) => "omitted nil write",
        (false, true) => "omitted replacement",
    };
    crate::with_idle_vm(|vm| {
        use crate::heap::{self, StepOutcome};
        let mut host = vm.host_context();
        heap::install(&mut host, limits(16, 8, 16)).expect("install");
        let planted = heap::with_heap(&mut host, |heap| -> Result<ClosedCellPlant, HeapError> {
            plant_closed_cell(heap, replaces)
        })
        .expect("plant");
        heap::open_fixed_step(&mut host).expect("open");
        let snap = heap::with_heap(&mut host, |heap| -> Result<ClosedCellSnap, HeapError> {
            let mut parked = ParkedMark::resume(heap).expect("resume saved mark");
            let before_heads = parked.meter().owner_head_checks;
            pause_at(&mut parked, planted.setter.slot(), planted.setter_edge);
            parked.with_saved_heap(|heap| {
                assert!(
                    !slot_is_marked(
                        heap,
                        planted.cell.slot(),
                        planted.cell.generation(),
                        super::HandleKind::Upvalue,
                    ),
                    "{label}: the shared cell is not marked before its closure edge is charged"
                );
                assert!(!parked_table_marked(heap, planted.fresh));
                assert!(!parked_table_marked(heap, planted.child));
            });
            let before_edges = parked.meter().outgoing_edge_checks;
            if let Some(current_edge) = planted.current_edge {
                pause_at(&mut parked, planted.cell.slot(), current_edge);
                parked.with_saved_heap(|heap| {
                    assert!(
                        slot_is_marked(
                            heap,
                            planted.cell.slot(),
                            planted.cell.generation(),
                            super::HandleKind::Upvalue,
                        ),
                        "{label}: replacement reaches the cell before its value edge is charged"
                    );
                });
            }
            pause_at(
                &mut parked,
                planted.sentinel.slot(),
                planted.sentinel_record,
            );
            let pending = parked.pending().expect("sentinel still pending");
            assert_eq!(pending.owner, planted.sentinel.slot());
            assert_eq!(pending.cursor, planted.sentinel_record);
            assert_eq!(parked.phase_name(), "gray");
            assert!(
                parked.mutation_open(),
                "{label}: mutation stays open in gray"
            );
            assert_eq!(parked.scanning_owner(), Some(planted.sentinel.slot()));
            assert_ne!(parked.scanning_owner(), Some(planted.cell.slot()));
            assert!(
                parked.meter().owner_head_checks > before_heads,
                "{label}: the cell owner was begun before the sentinel record"
            );
            if replaces {
                assert!(
                    parked.meter().outgoing_edge_checks > before_edges,
                    "{label}: the cell value edge was charged before the write"
                );
            }
            let before = parked.meter();
            let roots_before = parked.stats().live_roots;
            let getter_root = parked.root_closure(planted.frame, 1)?;
            let setter_root = parked.root_closure(planted.frame, 2)?;
            parked
                .with_saved_heap(|heap| -> Result<(), HeapError> {
                    assert!(
                        slot_is_marked(
                            heap,
                            planted.cell.slot(),
                            planted.cell.generation(),
                            super::HandleKind::Upvalue,
                        ),
                        "{label}: the shared cell is already scanned"
                    );
                    assert_eq!(heap.upvalue_of(planted.getter)?, planted.cell);
                    assert_eq!(heap.upvalue_of(planted.setter)?, planted.cell);
                    assert_ne!(planted.getter.slot(), planted.setter.slot());
                    assert_eq!(
                        getter_root,
                        Some(planted.getter),
                        "{label}: at least one closure is a strong root"
                    );
                    assert_eq!(setter_root, Some(planted.setter));
                    assert_no_independent_strong_root(
                        heap,
                        planted.fresh,
                        planted.child,
                        planted.orphan,
                    );
                    assert!(!parked_table_marked(heap, planted.fresh));
                    assert!(!parked_table_marked(heap, planted.child));
                    assert!(!parked_table_marked(heap, planted.orphan));
                    match planted.old {
                        Some(old) => {
                            assert_eq!(heap.current_table(planted.cell)?, Some(old));
                            assert_eq!(heap.read_upvalue(planted.getter)?, old);
                            assert_eq!(heap.read_upvalue(planted.setter)?, old);
                            assert!(parked_table_marked(heap, old));
                        }
                        None => {
                            assert_eq!(heap.current_table(planted.cell)?, None);
                            assert!(matches!(
                                heap.read_upvalue(planted.getter),
                                Err(HeapError::StaleHandle)
                            ));
                            assert!(matches!(
                                heap.read_upvalue(planted.setter),
                                Err(HeapError::StaleHandle)
                            ));
                        }
                    }
                    assert_eq!(
                        heap.stats().live_roots,
                        if replaces { 5 } else { 4 },
                        "{label}: fresh, child, and orphan are not roots"
                    );
                    Ok(())
                })
                .expect("scanned cell");
            let snap = parked
                .with_saved_heap(|heap| -> Result<ClosedCellSnap, HeapError> {
                    if publishes {
                        heap.write_upvalue(planted.cell, planted.fresh)?;
                        let stored = heap.current_table(planted.cell)?.expect("stored table");
                        assert_eq!(stored.slot(), planted.fresh.slot());
                        assert_eq!(stored.generation(), planted.fresh.generation());
                        assert_eq!(heap.read_upvalue(planted.getter)?, planted.fresh);
                        assert_eq!(heap.read_upvalue(planted.setter)?, planted.fresh);
                    }
                    Ok(ClosedCellSnap {
                        fresh_marked: parked_table_marked(heap, planted.fresh),
                        child_marked: parked_table_marked(heap, planted.child),
                        orphan_marked: parked_table_marked(heap, planted.orphan),
                        visits: heap
                            .scan
                            .node_slot_visits
                            .saturating_sub(before.node_slot_visits),
                        pushed: heap
                            .scan
                            .scratch
                            .work_pushed
                            .saturating_sub(before.scratch.work_pushed),
                        barriers: heap
                            .scan
                            .parked_barrier_marks
                            .saturating_sub(before.parked_barrier_marks),
                        roots: heap.stats().live_roots,
                    })
                })
                .expect("write");
            let after = parked.meter();
            assert_write_did_not_scan(&before, &after);
            assert_eq!(after.weak_mode_lookups, before.weak_mode_lookups);
            assert_eq!(after.metatable_edge_checks, before.metatable_edge_checks);
            assert!(same_pending(Some(pending), parked.pending()));
            assert_eq!(parked.scanning_owner(), Some(planted.sentinel.slot()));
            assert_eq!(snap.roots, roots_before, "{label}: the write adds no root");
            assert_eq!(parked.phase_name(), "gray");
            assert!(
                !snap.child_marked,
                "{label}: the child is not scanned at the write"
            );
            assert!(!snap.orphan_marked);
            parked.park()?;
            Ok(snap)
        })
        .expect("pause");
        let paused = heap::step_fixed_frames(&mut host, 64).expect("publish");
        assert!(
            matches!(paused, StepOutcome::Published { .. }),
            "{label}: publish the weak clear, got {paused:?}"
        );
        let finished = heap::step_fixed_frames(&mut host, 128).expect("finish");
        assert!(
            matches!(finished, StepOutcome::Completed(_)),
            "{label}: {finished:?}"
        );
        assert!(heap::fixed_step_idle(&mut host).expect("idle"));
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            assert!(
                !heap.alive(planted.orphan),
                "{label}: the weak orphan clears this cycle"
            );
            assert_eq!(
                heap.alive(planted.fresh),
                publishes,
                "{label}: a scanned closed cell did not keep the table written by write_upvalue"
            );
            assert_eq!(
                heap.alive(planted.child),
                publishes,
                "{label}: the child follows only if the direct table was retained"
            );
            assert!(heap.alive(planted.sentinel));
            assert!(heap.closure_alive(planted.getter));
            assert!(heap.closure_alive(planted.setter));
            assert!(heap.upvalue_alive(planted.cell));
            if publishes {
                let stored = heap
                    .current_table(planted.cell)?
                    .expect("cell still stores the table");
                assert_eq!(stored, planted.fresh);
                assert_eq!(heap.read_upvalue(planted.getter)?, planted.fresh);
                assert_eq!(heap.read_upvalue(planted.setter)?, planted.fresh);
                assert_eq!(heap.named_edge(planted.fresh, "child")?, Some(planted.child));
                assert_eq!(
                    heap.named_value(planted.child, "id")?,
                    Value::string("child")
                );
                assert_eq!(heap.named_edge(planted.witness, "fresh")?, Some(planted.fresh));
                assert_eq!(heap.named_edge(planted.witness, "orphan")?, None);
                assert!(
                    snap.fresh_marked,
                    "{label}: the write grays only the direct table"
                );
                assert_eq!(snap.visits, 2, "{label}: one target visit, not a child scan");
                assert_eq!(snap.pushed, 1, "{label}: the direct table is queued once");
                assert_eq!(snap.barriers, 1, "{label}: one strong barrier mark");
                heap.clear_upvalue(planted.cell)?;
                assert_eq!(heap.current_table(planted.cell)?, None);
                let _ = heap.collect();
                assert!(
                    !heap.alive(planted.fresh),
                    "{label}: clearing the cell edge releases the new graph next cycle"
                );
                assert!(!heap.alive(planted.child));
                assert!(heap.closure_alive(planted.getter));
                assert!(heap.upvalue_alive(planted.cell));
            } else {
                assert!(!snap.fresh_marked, "{label}: omitting the write leaves the table white");
                assert_eq!(snap.visits, 0);
                assert_eq!(snap.pushed, 0);
                assert_eq!(snap.barriers, 0);
                match planted.old {
                    Some(old) => {
                        let stored = heap
                            .current_table(planted.cell)?
                            .expect("old edge remains");
                        assert_eq!(stored, old);
                        assert_eq!(heap.read_upvalue(planted.getter)?, old);
                        assert!(heap.alive(old));
                    }
                    None => assert_eq!(heap.current_table(planted.cell)?, None),
                }
            }
            if let Some(old) = planted.old {
                assert!(
                    heap.alive(old),
                    "{label}: the old table has its own root and is not this cycle's collection witness"
                );
            }
            Ok(())
        })
        .expect("observe");
    });
}

fn parked_table_marked(heap: &LuaHeap, table: TableRef) -> bool {
    slot_is_marked(
        heap,
        table.slot(),
        table.generation(),
        super::HandleKind::Table,
    )
}

#[test]
fn gray_closed_cell_insert_keeps_the_new_graph() {
    run_closed_cell(true, false);
}

#[test]
fn gray_closed_cell_replace_keeps_the_new_graph() {
    run_closed_cell(true, true);
}

#[test]
fn gray_closed_cell_insert_omitted_collects_the_new_graph() {
    run_closed_cell(false, false);
}

#[test]
fn gray_closed_cell_replace_omitted_keeps_the_old_edge() {
    run_closed_cell(false, true);
}

#[test]
fn published_tail_write_upvalue_refuses_without_changing_identity() {
    crate::with_idle_vm(|vm| {
        use crate::heap::{self, StepOutcome};
        let mut host = vm.host_context();
        heap::install(&mut host, limits(16, 4, 16)).expect("install");
        let planted = heap::with_heap(&mut host, |heap| -> Result<_, HeapError> {
            let frame = heap.enter_roots(4)?;
            let sentinel = heap.alloc()?;
            heap.set_named_scalar(sentinel, "pending", Value::string("wait"))?;
            let owner = heap.alloc()?;
            heap.set_named_scalar(owner, "kept", Value::string("kept"))?;
            let old = heap.alloc()?;
            let fresh = heap.alloc()?;
            let cell = heap.alloc_upvalue()?;
            let nil_cell = heap.alloc_upvalue()?;
            let getter = heap.alloc_closure()?;
            let nil_getter = heap.alloc_closure()?;
            heap.set_upvalue_edge(getter, cell)?;
            heap.set_upvalue_edge(nil_getter, nil_cell)?;
            heap.write_upvalue(cell, old)?;
            let current_edge = heap
                .find_key(cell.slot(), &super::EdgeKey::Field(super::CURRENT_EDGE))
                .expect("current edge");
            let dust = heap.alloc()?;
            heap.write_root(frame, 0, sentinel)?;
            heap.write_root(frame, 1, owner)?;
            heap.write_closure_root(frame, 2, getter)?;
            heap.write_closure_root(frame, 3, nil_getter)?;
            let _ = dust;
            Ok((cell, nil_cell, old, fresh, current_edge))
        })
        .expect("plant");
        let (cell, nil_cell, old, fresh, current_edge) = planted;
        heap::open_fixed_step(&mut host).expect("open");
        let published = heap::step_fixed_frames(&mut host, 128).expect("publish");
        assert!(
            matches!(published, StepOutcome::Published { .. }),
            "{published:?}"
        );
        let before_tail = heap::saved_tail_state(&mut host)
            .expect("tail read")
            .expect("published tail");
        let (before_stats, before_meter, before_current, before_nil) =
            heap::with_heap(&mut host, |heap| -> Result<_, HeapError> {
                Ok((
                    heap.stats(),
                    heap.scan_meter(),
                    heap.current_table(cell)?,
                    heap.current_table(nil_cell)?,
                ))
            })
            .expect("before");
        let refused = heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let replace = heap.write_upvalue(cell, fresh);
            assert!(
                matches!(replace, Err(HeapError::SweepPending)),
                "published tail refuses a closed-cell replacement, got {replace:?}"
            );
            let insert = heap.write_upvalue(nil_cell, fresh);
            assert!(
                matches!(insert, Err(HeapError::SweepPending)),
                "published tail refuses a closed-cell nil write, got {insert:?}"
            );
            let stored = heap.current_table(cell)?.expect("old identity remains");
            assert_eq!(stored.slot(), old.slot());
            assert_eq!(stored.generation(), old.generation());
            assert_eq!(
                heap.find_key(cell.slot(), &super::EdgeKey::Field(super::CURRENT_EDGE)),
                Some(current_edge),
                "the refusal does not move the cell record"
            );
            assert_eq!(heap.current_table(nil_cell)?, None);
            assert_eq!(
                heap.find_key(nil_cell.slot(), &super::EdgeKey::Field(super::CURRENT_EDGE)),
                None,
                "the refusal does not insert a nil cell edge"
            );
            assert_eq!(heap.current_table(cell)?, before_current);
            assert_eq!(heap.current_table(nil_cell)?, before_nil);
            assert!(
                heap.alive(fresh),
                "refusal does not release the rejected table"
            );
            assert_eq!(heap.stats(), before_stats, "accounting is untouched");
            assert_eq!(
                heap.scan_meter(),
                before_meter,
                "the cursor accounts are untouched"
            );
            heap.write_upvalue(nil_cell, fresh)
        });
        assert!(
            matches!(refused, Err(HeapError::SweepPending)),
            "the tail gate is the refusal, got {refused:?}"
        );
        let after_tail = heap::saved_tail_state(&mut host)
            .expect("tail read")
            .expect("tail still saved");
        assert_eq!(before_tail.phase, after_tail.phase);
        assert_eq!(before_tail.cursor, after_tail.cursor);
        assert_eq!(before_tail.live_roots, after_tail.live_roots);
        assert_eq!(before_tail.dead_len, after_tail.dead_len);
        let finished = heap::step_fixed_frames(&mut host, 128).expect("finish");
        assert!(
            matches!(finished, StepOutcome::Completed(_)),
            "{finished:?}"
        );
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let stored = heap.current_table(cell)?.expect("old edge remains");
            assert_eq!(stored.slot(), old.slot());
            assert_eq!(stored.generation(), old.generation());
            assert_eq!(heap.current_table(nil_cell)?, None);
            assert!(!heap.alive(fresh), "the rejected table was never stored");
            Ok(())
        })
        .expect("after release");
    });
}

struct ClosedCellClosurePlant {
    frame: RootFrameId,
    sentinel: TableRef,
    sentinel_record: u32,
    getter: ClosureRef,
    setter: ClosureRef,
    setter_edge: u32,
    cell: UpvalueRef,
    current_edge: Option<u32>,
    witness: TableRef,
    fresh: ClosureRef,
    capture: UpvalueRef,
    child: TableRef,
    orphan: TableRef,
    old: Option<ClosureRef>,
}

fn plant_closed_cell_closure(
    heap: &mut LuaHeap,
    replaces: bool,
) -> Result<ClosedCellClosurePlant, HeapError> {
    let frame = heap.enter_roots(5)?;
    let sentinel = heap.alloc()?;
    heap.set_named_scalar(sentinel, "pending", Value::string("wait"))?;
    let (_, sentinel_record) = heap
        .locate_named(sentinel.slot(), "pending")
        .expect("sentinel record");
    let getter = heap.alloc_closure()?;
    let setter = heap.alloc_closure()?;
    let cell = heap.alloc_upvalue()?;
    heap.set_upvalue_edge(getter, cell)?;
    heap.set_upvalue_edge(setter, cell)?;
    let setter_edge = heap
        .find_key(setter.slot(), &super::EdgeKey::Field(super::UPVALUE_EDGE))
        .expect("setter upvalue edge");
    let old = if replaces {
        let old = heap.alloc_closure()?;
        heap.write_closure_upvalue(cell, old)?;
        Some(old)
    } else {
        None
    };
    let current_edge = old.map(|_| {
        heap.find_key(cell.slot(), &super::EdgeKey::Field(super::CURRENT_EDGE))
            .expect("cell value edge")
    });
    let witness = heap.alloc()?;
    let witness_meta = heap.alloc()?;
    heap.set_mode_scalar(witness_meta, Value::string("v"))?;
    heap.set_metatable(witness, Some(witness_meta))?;
    // Private handles only. This heap does not offer a guest weak closure.
    // The captured table is the independent weak-value observation.
    let fresh = heap.alloc_closure()?;
    let capture = heap.alloc_upvalue()?;
    let child = heap.alloc()?;
    let orphan = heap.alloc()?;
    heap.set_upvalue_edge(fresh, capture)?;
    heap.write_upvalue(capture, child)?;
    heap.set_named_scalar(child, "id", Value::string("child"))?;
    heap.set_named_edge(witness, "child", child)?;
    heap.set_named_edge(witness, "orphan", orphan)?;
    // Sentinel is rooted first so it is scanned last. The closures are scanned,
    // and the shared cell is finished, before that pending record is reached.
    heap.write_root(frame, 0, sentinel)?;
    heap.write_closure_root(frame, 1, getter)?;
    heap.write_closure_root(frame, 2, setter)?;
    heap.write_root(frame, 3, witness)?;
    if let Some(old) = old {
        heap.write_closure_root(frame, 4, old)?;
    }
    Ok(ClosedCellClosurePlant {
        frame,
        sentinel,
        sentinel_record,
        getter,
        setter,
        setter_edge,
        cell,
        current_edge,
        witness,
        fresh,
        capture,
        child,
        orphan,
        old,
    })
}

struct ClosedCellClosureSnap {
    fresh_marked: bool,
    capture_marked: bool,
    child_marked: bool,
    orphan_marked: bool,
    visits: u64,
    pushed: u64,
    barriers: u64,
    roots: u32,
    gray_before: u32,
    gray_len: u32,
}

fn closure_marked(heap: &LuaHeap, closure: ClosureRef) -> bool {
    slot_is_marked(
        heap,
        closure.slot(),
        closure.generation(),
        super::HandleKind::Closure,
    )
}

fn upvalue_marked(heap: &LuaHeap, cell: UpvalueRef) -> bool {
    slot_is_marked(
        heap,
        cell.slot(),
        cell.generation(),
        super::HandleKind::Upvalue,
    )
}

fn assert_closure_graph_unrooted(heap: &LuaHeap, planted: &ClosedCellClosurePlant, label: &str) {
    for rooted in &heap.roots {
        match rooted {
            Some(super::Rooted::Table(handle)) => {
                assert_ne!(
                    *handle, planted.child,
                    "{label}: the captured table has no strong root"
                );
                assert_ne!(
                    *handle, planted.orphan,
                    "{label}: the orphan has no strong root"
                );
            }
            Some(super::Rooted::Closure(handle)) => {
                assert_ne!(
                    *handle, planted.fresh,
                    "{label}: the new closure has no strong root"
                );
            }
            Some(super::Rooted::Upvalue(handle)) => {
                assert_ne!(
                    *handle, planted.capture,
                    "{label}: the capture cell has no strong root"
                );
            }
            // This check names the planted closure graph. A userdata root is
            // another kind and is not one of those nodes.
            Some(super::Rooted::Userdata(_handle)) => {}
            None => {}
        }
    }
}

fn stored_closure(heap: &LuaHeap, cell: UpvalueRef) -> Result<Option<ClosureRef>, HeapError> {
    match heap.current_edge(cell)? {
        Some(super::CellValue::Closure(found)) => Ok(Some(found)),
        None => Ok(None),
        Some(other) => panic!("cell current edge is not a closure or nil, got {other:?}"),
    }
}

fn run_closed_cell_closure(publishes: bool, replaces: bool) {
    let label = match (publishes, replaces) {
        (true, false) => "nil first write",
        (true, true) => "closure replacement",
        (false, false) => "omitted nil write",
        (false, true) => "omitted replacement",
    };
    crate::with_idle_vm(|vm| {
        use crate::heap::{self, StepOutcome};
        let mut host = vm.host_context();
        heap::install(&mut host, limits(16, 8, 16)).expect("install");
        let planted = heap::with_heap(&mut host, |heap| -> Result<_, HeapError> {
            plant_closed_cell_closure(heap, replaces)
        })
        .expect("plant");
        heap::open_fixed_step(&mut host).expect("open");
        let snap = heap::with_heap(
            &mut host,
            |heap| -> Result<ClosedCellClosureSnap, HeapError> {
                let mut parked = ParkedMark::resume(heap).expect("resume saved mark");
                let before_heads = parked.meter().owner_head_checks;
                pause_at(&mut parked, planted.setter.slot(), planted.setter_edge);
                parked.with_saved_heap(|heap| {
                    assert!(
                        !upvalue_marked(heap, planted.cell),
                        "{label}: the shared cell is not marked before its closure edge is charged"
                    );
                    assert!(!closure_marked(heap, planted.fresh));
                    assert!(!upvalue_marked(heap, planted.capture));
                    assert!(!parked_table_marked(heap, planted.child));
                });
                let before_edges = parked.meter().outgoing_edge_checks;
                if let Some(current_edge) = planted.current_edge {
                    pause_at(&mut parked, planted.cell.slot(), current_edge);
                    parked.with_saved_heap(|heap| {
                        assert!(
                            upvalue_marked(heap, planted.cell),
                            "{label}: replacement reaches the cell before its value edge is charged"
                        );
                    });
                }
                pause_at(
                    &mut parked,
                    planted.sentinel.slot(),
                    planted.sentinel_record,
                );
                let pending = parked.pending().expect("sentinel still pending");
                assert_eq!(pending.owner, planted.sentinel.slot());
                assert_eq!(pending.cursor, planted.sentinel_record);
                assert_eq!(parked.phase_name(), "gray");
                assert!(
                    parked.mutation_open(),
                    "{label}: mutation stays open in gray"
                );
                assert_eq!(parked.scanning_owner(), Some(planted.sentinel.slot()));
                assert_ne!(parked.scanning_owner(), Some(planted.cell.slot()));
                assert!(
                    parked.meter().owner_head_checks > before_heads,
                    "{label}: the cell owner was begun before the sentinel record"
                );
                if replaces {
                    assert!(
                        parked.meter().outgoing_edge_checks > before_edges,
                        "{label}: the cell value edge was charged before the write"
                    );
                }
                let before = parked.meter();
                let roots_before = parked.stats().live_roots;
                let getter_root = parked.root_closure(planted.frame, 1)?;
                let setter_root = parked.root_closure(planted.frame, 2)?;
                let old_root = parked.root_closure(planted.frame, 4)?;
                parked
                    .with_saved_heap(|heap| -> Result<(), HeapError> {
                        assert!(
                            upvalue_marked(heap, planted.cell),
                            "{label}: the shared cell is already scanned"
                        );
                        assert_eq!(heap.upvalue_of(planted.getter)?, planted.cell);
                        assert_eq!(heap.upvalue_of(planted.setter)?, planted.cell);
                        assert_ne!(planted.getter.slot(), planted.setter.slot());
                        assert_eq!(
                            getter_root,
                            Some(planted.getter),
                            "{label}: at least one closure is a strong root"
                        );
                        assert_eq!(setter_root, Some(planted.setter));
                        let capture = heap.upvalue_of(planted.fresh)?;
                        assert_eq!(capture.slot(), planted.capture.slot());
                        assert_eq!(capture.generation(), planted.capture.generation());
                        let captured = heap.current_table(planted.capture)?.expect("capture table");
                        assert_eq!(captured.slot(), planted.child.slot());
                        assert_eq!(captured.generation(), planted.child.generation());
                        assert_closure_graph_unrooted(heap, &planted, label);
                        assert!(!closure_marked(heap, planted.fresh));
                        assert!(!upvalue_marked(heap, planted.capture));
                        assert!(!parked_table_marked(heap, planted.child));
                        assert!(!parked_table_marked(heap, planted.orphan));
                        match planted.old {
                            Some(old) => {
                                let stored =
                                    stored_closure(heap, planted.cell)?.expect("old closure");
                                assert_eq!(stored.slot(), old.slot());
                                assert_eq!(stored.generation(), old.generation());
                                assert_eq!(old_root, Some(old));
                                assert!(closure_marked(heap, old));
                            }
                            None => {
                                assert_eq!(stored_closure(heap, planted.cell)?, None);
                                assert_eq!(old_root, None);
                            }
                        }
                        assert_eq!(
                            heap.stats().live_roots,
                            if replaces { 5 } else { 4 },
                            "{label}: fresh, capture, child, and orphan are not roots"
                        );
                        Ok(())
                    })
                    .expect("scanned cell");
                let gray_before = parked
                    .with_saved_heap(|heap| heap.saved_mark_watch().expect("saved mark").gray_len);
                let snap = parked
                    .with_saved_heap(|heap| -> Result<ClosedCellClosureSnap, HeapError> {
                        if publishes {
                            heap.write_closure_upvalue(planted.cell, planted.fresh)?;
                            let stored =
                                stored_closure(heap, planted.cell)?.expect("stored closure");
                            assert_eq!(stored.slot(), planted.fresh.slot());
                            assert_eq!(stored.generation(), planted.fresh.generation());
                            assert_eq!(
                                heap.read_cell(planted.getter)?,
                                Some(super::CellValue::Closure(planted.fresh))
                            );
                            assert_eq!(
                                heap.read_cell(planted.setter)?,
                                Some(super::CellValue::Closure(planted.fresh))
                            );
                        }
                        Ok(ClosedCellClosureSnap {
                            fresh_marked: closure_marked(heap, planted.fresh),
                            capture_marked: upvalue_marked(heap, planted.capture),
                            child_marked: parked_table_marked(heap, planted.child),
                            orphan_marked: parked_table_marked(heap, planted.orphan),
                            visits: heap
                                .scan
                                .node_slot_visits
                                .saturating_sub(before.node_slot_visits),
                            pushed: heap
                                .scan
                                .scratch
                                .work_pushed
                                .saturating_sub(before.scratch.work_pushed),
                            barriers: heap
                                .scan
                                .parked_barrier_marks
                                .saturating_sub(before.parked_barrier_marks),
                            roots: heap.stats().live_roots,
                            gray_before,
                            gray_len: heap.saved_mark_watch().expect("saved mark").gray_len,
                        })
                    })
                    .expect("write");
                let after = parked.meter();
                assert_write_did_not_scan(&before, &after);
                assert_eq!(after.weak_mode_lookups, before.weak_mode_lookups);
                assert_eq!(after.metatable_edge_checks, before.metatable_edge_checks);
                assert!(same_pending(Some(pending), parked.pending()));
                assert_eq!(parked.scanning_owner(), Some(planted.sentinel.slot()));
                assert_eq!(snap.roots, roots_before, "{label}: the write adds no root");
                assert_eq!(parked.phase_name(), "gray");
                assert!(
                    !snap.capture_marked,
                    "{label}: the capture is not scanned at the write"
                );
                assert!(
                    !snap.child_marked,
                    "{label}: the captured table is not scanned at the write"
                );
                assert!(!snap.orphan_marked);
                let gray_delta = snap.gray_len.saturating_sub(gray_before);
                if !publishes {
                    assert_eq!(
                        gray_delta, 0,
                        "{label}: omitting the write adds no gray object"
                    );
                }
                parked.park()?;
                Ok(snap)
            },
        )
        .expect("pause");
        let paused = heap::step_fixed_frames(&mut host, 64).expect("publish");
        assert!(
            matches!(paused, StepOutcome::Published { .. }),
            "{label}: publish the weak clear, got {paused:?}"
        );
        let finished = heap::step_fixed_frames(&mut host, 128).expect("finish");
        assert!(
            matches!(finished, StepOutcome::Completed(_)),
            "{label}: {finished:?}"
        );
        assert!(heap::fixed_step_idle(&mut host).expect("idle"));
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            assert!(
                !heap.alive(planted.orphan),
                "{label}: the weak orphan clears this cycle"
            );
            assert_eq!(
                heap.closure_alive(planted.fresh),
                publishes,
                "{label}: a scanned closed cell did not keep the closure written by write_closure_upvalue"
            );
            assert_eq!(
                heap.upvalue_alive(planted.capture),
                publishes,
                "{label}: the capture follows only if the direct closure was retained"
            );
            assert_eq!(
                heap.alive(planted.child),
                publishes,
                "{label}: the captured table follows only through that closure"
            );
            assert!(heap.alive(planted.sentinel));
            assert!(heap.closure_alive(planted.getter));
            assert!(heap.closure_alive(planted.setter));
            assert!(heap.upvalue_alive(planted.cell));
            if publishes {
                let stored = stored_closure(heap, planted.cell)?.expect("cell still stores the closure");
                assert_eq!(stored.slot(), planted.fresh.slot());
                assert_eq!(stored.generation(), planted.fresh.generation());
                assert_eq!(
                    heap.read_cell(planted.getter)?,
                    Some(super::CellValue::Closure(planted.fresh))
                );
                assert_eq!(
                    heap.read_cell(planted.setter)?,
                    Some(super::CellValue::Closure(planted.fresh))
                );
                let capture = heap.upvalue_of(planted.fresh)?;
                assert_eq!(capture.slot(), planted.capture.slot());
                assert_eq!(capture.generation(), planted.capture.generation());
                let child = heap.current_table(capture)?.expect("capture still stores the table");
                assert_eq!(child.slot(), planted.child.slot());
                assert_eq!(child.generation(), planted.child.generation());
                assert_eq!(
                    heap.named_value(planted.child, "id")?,
                    Value::string("child")
                );
                assert_eq!(heap.named_edge(planted.witness, "child")?, Some(planted.child));
                assert_eq!(heap.named_edge(planted.witness, "orphan")?, None);
                assert!(
                    snap.fresh_marked,
                    "{label}: the write grays only the direct closure"
                );
                assert_eq!(snap.visits, 2, "{label}: one target visit, not a child scan");
                assert_eq!(snap.pushed, 1, "{label}: the direct closure is queued once");
                assert_eq!(snap.barriers, 1, "{label}: one strong barrier mark");
                assert_eq!(
                    snap.gray_len.saturating_sub(snap.gray_before),
                    1,
                    "{label}: only the direct closure joins gray"
                );
                heap.clear_upvalue(planted.cell)?;
                assert_eq!(stored_closure(heap, planted.cell)?, None);
                let _ = heap.collect();
                assert!(
                    !heap.closure_alive(planted.fresh),
                    "{label}: clearing the cell edge releases the closure next cycle"
                );
                assert!(!heap.upvalue_alive(planted.capture));
                assert!(!heap.alive(planted.child));
                assert!(heap.closure_alive(planted.getter));
                assert!(heap.upvalue_alive(planted.cell));
            } else {
                assert!(
                    !snap.fresh_marked,
                    "{label}: omitting the write leaves the closure white"
                );
                assert_eq!(snap.visits, 0);
                assert_eq!(snap.pushed, 0);
                assert_eq!(snap.barriers, 0);
                assert_eq!(heap.named_edge(planted.witness, "child")?, None);
                assert_eq!(heap.named_edge(planted.witness, "orphan")?, None);
                match planted.old {
                    Some(old) => {
                        let stored = stored_closure(heap, planted.cell)?.expect("old edge remains");
                        assert_eq!(stored.slot(), old.slot());
                        assert_eq!(stored.generation(), old.generation());
                        assert!(heap.closure_alive(old));
                    }
                    None => assert_eq!(stored_closure(heap, planted.cell)?, None),
                }
            }
            if let Some(old) = planted.old {
                assert!(
                    heap.closure_alive(old),
                    "{label}: the old closure has its own root and is not this cycle's collection witness"
                );
            }
            Ok(())
        })
        .expect("observe");
    });
}

#[test]
fn gray_closed_cell_closure_insert_keeps_the_new_graph() {
    run_closed_cell_closure(true, false);
}

#[test]
fn gray_closed_cell_closure_replace_keeps_the_new_graph() {
    run_closed_cell_closure(true, true);
}

#[test]
fn gray_closed_cell_closure_insert_omitted_collects_the_new_graph() {
    run_closed_cell_closure(false, false);
}

#[test]
fn gray_closed_cell_closure_replace_omitted_keeps_the_old_edge() {
    run_closed_cell_closure(false, true);
}

#[test]
fn published_tail_closed_cell_closure_refuses_without_changing_identity() {
    crate::with_idle_vm(|vm| {
        use crate::heap::{self, StepOutcome};
        let mut host = vm.host_context();
        heap::install(&mut host, limits(16, 4, 16)).expect("install");
        let planted = heap::with_heap(&mut host, |heap| -> Result<_, HeapError> {
            let frame = heap.enter_roots(4)?;
            let sentinel = heap.alloc()?;
            heap.set_named_scalar(sentinel, "pending", Value::string("wait"))?;
            let owner = heap.alloc()?;
            heap.set_named_scalar(owner, "kept", Value::string("kept"))?;
            let old = heap.alloc_closure()?;
            let fresh = heap.alloc_closure()?;
            let cell = heap.alloc_upvalue()?;
            let nil_cell = heap.alloc_upvalue()?;
            let getter = heap.alloc_closure()?;
            let nil_getter = heap.alloc_closure()?;
            heap.set_upvalue_edge(getter, cell)?;
            heap.set_upvalue_edge(nil_getter, nil_cell)?;
            heap.write_closure_upvalue(cell, old)?;
            let current_edge = heap
                .find_key(cell.slot(), &super::EdgeKey::Field(super::CURRENT_EDGE))
                .expect("current edge");
            let dust = heap.alloc()?;
            heap.write_root(frame, 0, sentinel)?;
            heap.write_root(frame, 1, owner)?;
            heap.write_closure_root(frame, 2, getter)?;
            heap.write_closure_root(frame, 3, nil_getter)?;
            let _ = dust;
            Ok((cell, nil_cell, old, fresh, current_edge))
        })
        .expect("plant");
        let (cell, nil_cell, old, fresh, current_edge) = planted;
        assert_eq!(
            fresh.generation(),
            old.generation(),
            "the refused target is a live closure of this generation"
        );
        heap::open_fixed_step(&mut host).expect("open");
        let published = heap::step_fixed_frames(&mut host, 128).expect("publish");
        assert!(
            matches!(published, StepOutcome::Published { .. }),
            "{published:?}"
        );
        let before_tail = heap::saved_tail_state(&mut host)
            .expect("tail read")
            .expect("published tail");
        let (before_stats, before_meter, before_current, before_nil) =
            heap::with_heap(&mut host, |heap| -> Result<_, HeapError> {
                Ok((
                    heap.stats(),
                    heap.scan_meter(),
                    stored_closure(heap, cell)?,
                    stored_closure(heap, nil_cell)?,
                ))
            })
            .expect("before");
        let refused = heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let replace = heap.write_closure_upvalue(cell, fresh);
            assert!(
                matches!(replace, Err(HeapError::SweepPending)),
                "published tail refuses a closed-cell closure replacement, got {replace:?}"
            );
            let insert = heap.write_closure_upvalue(nil_cell, fresh);
            assert!(
                matches!(insert, Err(HeapError::SweepPending)),
                "published tail refuses a closed-cell closure nil write, got {insert:?}"
            );
            let stored = stored_closure(heap, cell)?.expect("old identity remains");
            assert_eq!(stored.slot(), old.slot());
            assert_eq!(stored.generation(), old.generation());
            assert_eq!(
                heap.find_key(cell.slot(), &super::EdgeKey::Field(super::CURRENT_EDGE)),
                Some(current_edge),
                "the refusal does not move the cell record"
            );
            assert_eq!(stored_closure(heap, nil_cell)?, None);
            assert_eq!(
                heap.find_key(nil_cell.slot(), &super::EdgeKey::Field(super::CURRENT_EDGE)),
                None,
                "the refusal does not insert a nil cell edge"
            );
            assert_eq!(stored_closure(heap, cell)?, before_current);
            assert_eq!(stored_closure(heap, nil_cell)?, before_nil);
            assert!(
                heap.closure_alive(fresh),
                "refusal does not release the rejected closure"
            );
            assert_eq!(heap.stats(), before_stats, "accounting is untouched");
            assert_eq!(
                heap.scan_meter(),
                before_meter,
                "the cursor accounts are untouched"
            );
            heap.write_closure_upvalue(nil_cell, fresh)
        });
        assert!(
            matches!(refused, Err(HeapError::SweepPending)),
            "the tail gate is the refusal, got {refused:?}"
        );
        let after_tail = heap::saved_tail_state(&mut host)
            .expect("tail read")
            .expect("tail still saved");
        assert_eq!(before_tail.phase, after_tail.phase);
        assert_eq!(before_tail.cursor, after_tail.cursor);
        assert_eq!(before_tail.live_roots, after_tail.live_roots);
        assert_eq!(before_tail.dead_len, after_tail.dead_len);
        let finished = heap::step_fixed_frames(&mut host, 128).expect("finish");
        assert!(
            matches!(finished, StepOutcome::Completed(_)),
            "{finished:?}"
        );
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let stored = stored_closure(heap, cell)?.expect("old edge remains");
            assert_eq!(stored.slot(), old.slot());
            assert_eq!(stored.generation(), old.generation());
            assert_eq!(stored_closure(heap, nil_cell)?, None);
            assert!(
                !heap.closure_alive(fresh),
                "the rejected closure was never stored"
            );
            Ok(())
        })
        .expect("after release");
    });
}

/// Where the weak-value owner sits when the metatable write runs.
#[derive(Clone, Copy)]
enum WeakOwnerPlace {
    /// Gray already walked the owner. A later owner still has a real cursor.
    Scanned,
    /// The owner's second record is the pending cursor. An earlier edge was charged.
    Scanning,
    /// The owner is marked and still under the sentinel in the gray stack.
    Queued,
}

struct WeakGraph {
    sentinel: TableRef,
    left: TableRef,
    right: TableRef,
    witness: TableRef,
    old_mt: TableRef,
    strong_mt: TableRef,
    subject: TableRef,
    child: TableRef,
    right_val: TableRef,
    orphan: TableRef,
    dust: TableRef,
    sentinel_record: u32,
    left_scalar: u32,
    left_subject: u32,
    old_mode_record: u32,
}

fn plant_weak_graph(heap: &mut LuaHeap, place: WeakOwnerPlace) -> Result<WeakGraph, HeapError> {
    let sentinel = heap.alloc()?;
    let left = heap.alloc()?;
    let right = heap.alloc()?;
    let witness = heap.alloc()?;
    let old_mt = heap.alloc()?;
    let strong_mt = heap.alloc()?;
    let subject = heap.alloc()?;
    let child = heap.alloc()?;
    let right_val = heap.alloc()?;
    let orphan = heap.alloc()?;
    let dust = heap.alloc()?;
    heap.set_mode_scalar(old_mt, Value::string("v"))?;
    heap.set_metatable(left, Some(old_mt))?;
    heap.set_metatable(right, Some(old_mt))?;
    heap.set_metatable(witness, Some(old_mt))?;
    heap.set_named_scalar(sentinel, "pending", Value::Bool(true))?;
    heap.set_named_scalar(left, "charged", Value::Bool(true))?;
    heap.set_named_edge(left, "subj", subject)?;
    heap.set_named_edge(subject, "next", child)?;
    heap.set_named_edge(right, "gone", right_val)?;
    heap.set_named_edge(witness, "child", child)?;
    heap.set_named_edge(witness, "orphan", orphan)?;
    heap.issue_carrier(subject)?;
    let frame = heap.enter_roots(5)?;
    // Last written root is popped first. The sentinel stays pending until the
    // owners written after it have been scanned, except the queued arm.
    match place {
        WeakOwnerPlace::Scanned | WeakOwnerPlace::Scanning => {
            heap.write_root(frame, 0, sentinel)?;
            heap.write_root(frame, 1, right)?;
            heap.write_root(frame, 2, witness)?;
            heap.write_root(frame, 3, strong_mt)?;
            heap.write_root(frame, 4, left)?;
        }
        WeakOwnerPlace::Queued => {
            heap.write_root(frame, 0, left)?;
            heap.write_root(frame, 1, right)?;
            heap.write_root(frame, 2, witness)?;
            heap.write_root(frame, 3, strong_mt)?;
            heap.write_root(frame, 4, sentinel)?;
        }
    }
    let (_, sentinel_record) = heap
        .locate_named(sentinel.slot(), "pending")
        .expect("sentinel record");
    let (_, left_scalar) = heap
        .locate_named(left.slot(), "charged")
        .expect("charged record");
    let (_, left_subject) = heap
        .locate_named(left.slot(), "subj")
        .expect("subject record");
    let old_mode_record = heap.slots[old_mt.slot() as usize]
        .edge_head
        .expect("mode record");
    let _ = dust;
    let _ = frame;
    Ok(WeakGraph {
        sentinel,
        left,
        right,
        witness,
        old_mt,
        strong_mt,
        subject,
        child,
        right_val,
        orphan,
        dust,
        sentinel_record,
        left_scalar,
        left_subject,
        old_mode_record,
    })
}

fn open_weak_graph(host: &mut vm::HostContext<'_>, place: WeakOwnerPlace) -> WeakGraph {
    crate::heap::install(host, limits(16, 8, 24)).expect("install");
    let graph = crate::heap::with_heap(host, |heap| plant_weak_graph(heap, place)).expect("plant");
    crate::heap::open_fixed_step(host).expect("open");
    graph
}

fn resume_weak(host: &mut vm::HostContext<'_>, body: impl FnOnce(&mut ParkedMark<'_>)) {
    crate::heap::with_heap(host, |heap| {
        let mut parked = ParkedMark::resume(heap).expect("resume saved mark");
        body(&mut parked);
        parked.park().expect("park");
        Ok(())
    })
    .expect("resume");
}

fn write_metatable(
    parked: &mut ParkedMark<'_>,
    owner: TableRef,
    mt: Option<TableRef>,
) -> Result<(), HeapError> {
    parked.with_saved_heap(|heap| heap.set_metatable(owner, mt))
}

fn assert_setter_did_not_move_scan(before: &ScanMeter, after: &ScanMeter) {
    assert_eq!(before.outgoing_edge_checks, after.outgoing_edge_checks);
    assert_eq!(before.owner_head_checks, after.owner_head_checks);
    assert_eq!(before.weak_mode_lookups, after.weak_mode_lookups);
    assert_eq!(before.root_slot_visits, after.root_slot_visits);
}

fn assert_subject_marked(parked: &mut ParkedMark<'_>, subject: TableRef) {
    if parked.marked(subject) {
        return;
    }
    for _ in 0..HARNESS_BOUND {
        let view = parked.pulse(1);
        if parked.marked(subject) {
            return;
        }
        if view.finished {
            break;
        }
    }
    assert!(parked.marked(subject), "弱扫描跳过的值没有被标活");
}

fn finish_fixed_cycle(host: &mut vm::HostContext<'_>) -> crate::heap::StepOutcome {
    let mut outcome = heap::step_fixed_frames(host, 128).expect("step fixed frames");
    for _ in 0..HARNESS_BOUND {
        if matches!(outcome, crate::heap::StepOutcome::Completed(_)) {
            return outcome;
        }
        outcome = heap::step_fixed_frames(host, 128).expect("resume published tail");
    }
    outcome
}

fn finish_kept_cycle(host: &mut vm::HostContext<'_>, graph: &WeakGraph) {
    let done = finish_fixed_cycle(host);
    assert!(
        matches!(done, crate::heap::StepOutcome::Completed(_)),
        "{done:?}"
    );
    assert!(heap::fixed_step_idle(host).expect("idle"));
    heap::with_heap(host, |heap| -> Result<(), HeapError> {
        assert!(
            heap.alive(graph.subject),
            "the strengthened edge keeps the value"
        );
        assert!(
            heap.alive(graph.child),
            "the continued scan keeps the successor"
        );
        assert_eq!(heap.named_edge(graph.left, "subj")?, Some(graph.subject));
        assert_eq!(heap.named_edge(graph.subject, "next")?, Some(graph.child));
        let carrier = heap.issued_value(graph.subject)?;
        let mut cursor = Value::Null;
        let mut saw = false;
        while let Some((key, value)) = heap.next_guest(graph.left, &cursor)? {
            if value == carrier {
                saw = true;
            }
            cursor = key;
        }
        assert!(saw, "iteration still yields the original subject");
        assert_eq!(heap.named_edge(graph.right, "gone")?, None);
        assert!(
            !heap.alive(graph.right_val),
            "the shared weak table still clears"
        );
        assert_eq!(heap.named_edge(graph.witness, "orphan")?, None);
        assert!(
            !heap.alive(graph.orphan),
            "the weak orphan proves this cycle released nodes"
        );
        assert_eq!(heap.named_edge(graph.witness, "child")?, Some(graph.child));
        assert!(!heap.alive(graph.dust), "an unrooted node was released");
        heap.set_named_scalar(graph.left, "subj", Value::Null)?;
        Ok(())
    })
    .expect("this cycle");
    heap::collect(host).expect("next cycle after the keep-alive edge is removed");
    heap::with_heap(host, |heap| {
        assert!(
            !heap.alive(graph.subject),
            "the next cycle clears the value"
        );
        assert!(
            !heap.alive(graph.child),
            "the next cycle clears the successor"
        );
        assert_eq!(heap.named_edge(graph.witness, "child").expect("read"), None);
        Ok(())
    })
    .expect("next cycle");
}

#[test]
fn gray_scanned_weak_owner_strengthen_marks_skipped_value() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = open_weak_graph(&mut host, WeakOwnerPlace::Scanned);
        resume_weak(&mut host, |parked| {
            let pending = pause_at(parked, graph.sentinel.slot(), graph.sentinel_record);
            assert_eq!(pending.owner, graph.sentinel.slot());
            assert!(
                parked.marked(graph.left),
                "the left table was already scanned"
            );
            assert!(
                !parked.marked(graph.subject),
                "the weak scan left the value white"
            );
            assert!(
                !parked.marked(graph.child),
                "the successor has no other strong root"
            );
            assert!(parked.meter().outgoing_edge_checks > 0);
            let before = parked.meter();
            let roots = parked.stats().live_roots;
            write_metatable(parked, graph.left, Some(graph.strong_mt))
                .expect("replace with the preallocated strong metatable");
            assert!(same_pending(Some(pending), parked.pending()));
            assert_setter_did_not_move_scan(&before, &parked.meter());
            assert_eq!(parked.stats().live_roots, roots);
            assert!(!parked.marked(graph.subject));
            assert_subject_marked(parked, graph.subject);
            assert_subject_marked(parked, graph.child);
        });
        finish_kept_cycle(&mut host, &graph);
    });
}

#[test]
fn gray_scanned_weak_owner_clear_metatable_marks_skipped_value() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = open_weak_graph(&mut host, WeakOwnerPlace::Scanned);
        resume_weak(&mut host, |parked| {
            pause_at(parked, graph.sentinel.slot(), graph.sentinel_record);
            assert!(parked.marked(graph.left));
            assert!(!parked.marked(graph.subject));
            let before = parked.meter();
            let pending = parked.pending();
            let roots = parked.stats().live_roots;
            write_metatable(parked, graph.left, None).expect("clear the weak metatable");
            assert!(same_pending(pending, parked.pending()));
            assert_setter_did_not_move_scan(&before, &parked.meter());
            assert_eq!(parked.stats().live_roots, roots);
            assert_eq!(parked.meter().node_slot_visits, before.node_slot_visits);
            assert_subject_marked(parked, graph.subject);
        });
        finish_kept_cycle(&mut host, &graph);
    });
}

#[test]
fn gray_scanned_weak_owner_sole_edge_clear_still_reads_old_weakness() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(12, 4, 16)).expect("install");
        let graph = heap::with_heap(&mut host, |heap| -> Result<_, HeapError> {
            let sentinel = heap.alloc()?;
            let left = heap.alloc()?;
            let old_mt = heap.alloc()?;
            let subject = heap.alloc()?;
            let child = heap.alloc()?;
            let dust = heap.alloc()?;
            heap.set_mode_scalar(old_mt, Value::string("v"))?;
            heap.set_metatable(left, Some(old_mt))?;
            heap.set_named_scalar(sentinel, "pending", Value::Bool(true))?;
            heap.set_named_scalar(left, "charged", Value::Bool(true))?;
            heap.set_named_edge(left, "subj", subject)?;
            heap.set_named_edge(subject, "next", child)?;
            let frame = heap.enter_roots(2)?;
            heap.write_root(frame, 0, sentinel)?;
            heap.write_root(frame, 1, left)?;
            let (_, sentinel_record) = heap
                .locate_named(sentinel.slot(), "pending")
                .expect("sentinel");
            Ok((sentinel, left, subject, child, dust, sentinel_record, frame))
        })
        .expect("plant");
        let (sentinel, left, subject, child, dust, sentinel_record, _frame) = graph;
        heap::open_fixed_step(&mut host).expect("open");
        resume_weak(&mut host, |parked| {
            pause_at(parked, sentinel.slot(), sentinel_record);
            assert!(parked.marked(left));
            assert!(!parked.marked(subject));
            let edges = parked.with_saved_heap(|heap| heap.metatable_edges);
            assert_eq!(edges, 1, "clearing this edge drops the count to zero");
            write_metatable(parked, left, None).expect("clear the only metatable edge");
            assert_eq!(parked.with_saved_heap(|heap| heap.metatable_edges), 0);
            assert_subject_marked(parked, subject);
            let _ = child;
        });
        let done = finish_fixed_cycle(&mut host);
        assert!(
            matches!(done, crate::heap::StepOutcome::Completed(_)),
            "{done:?}"
        );
        heap::with_heap(&mut host, |heap| {
            assert!(
                heap.alive(subject),
                "old weakness was read before the count hit zero"
            );
            assert!(heap.alive(child));
            assert!(!heap.alive(dust), "sweep still released an unrooted node");
            Ok(())
        })
        .expect("sole edge");
    });
}

#[test]
fn gray_scanned_weak_owner_omitted_or_still_weak_clears_value() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = open_weak_graph(&mut host, WeakOwnerPlace::Scanned);
        resume_weak(&mut host, |parked| {
            pause_at(parked, graph.sentinel.slot(), graph.sentinel_record);
            assert!(!parked.marked(graph.subject));
        });
        let done = finish_fixed_cycle(&mut host);
        assert!(
            matches!(done, crate::heap::StepOutcome::Completed(_)),
            "{done:?}"
        );
        heap::with_heap(&mut host, |heap| {
            assert!(
                !heap.alive(graph.subject),
                "omission still clears the weak value"
            );
            assert!(!heap.alive(graph.right_val));
            assert!(!heap.alive(graph.orphan));
            Ok(())
        })
        .expect("omitted");
    });
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = open_weak_graph(&mut host, WeakOwnerPlace::Scanned);
        resume_weak(&mut host, |parked| {
            pause_at(parked, graph.sentinel.slot(), graph.sentinel_record);
            write_metatable(parked, graph.left, Some(graph.old_mt)).expect("equal weak metatable");
            assert!(!parked.marked(graph.subject));
        });
        let done = finish_fixed_cycle(&mut host);
        assert!(
            matches!(done, crate::heap::StepOutcome::Completed(_)),
            "{done:?}"
        );
        heap::with_heap(&mut host, |heap| {
            assert!(
                !heap.alive(graph.subject),
                "an equal weak mode does not keep the value"
            );
            Ok(())
        })
        .expect("still weak");
    });
}

#[test]
fn gray_scanning_weak_owner_continues_then_rescans_from_head() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = open_weak_graph(&mut host, WeakOwnerPlace::Scanning);
        resume_weak(&mut host, |parked| {
            let at_scalar = pause_at(parked, graph.left.slot(), graph.left_scalar);
            assert_eq!(at_scalar.cursor, graph.left_scalar);
            let before_edge = parked.meter().outgoing_edge_checks;
            let _charged = parked.pulse(1);
            assert_eq!(parked.meter().outgoing_edge_checks, before_edge + 1);
            assert!(
                !parked.marked(graph.subject),
                "the charged edge was not the value"
            );
            let pending = parked.pending().expect("subject record is still pending");
            assert_eq!(pending.owner, graph.left.slot());
            assert_eq!(pending.cursor, graph.left_subject);
            let before = parked.meter();
            let roots = parked.stats().live_roots;
            write_metatable(parked, graph.left, Some(graph.strong_mt))
                .expect("strengthen while the weak scan is in progress");
            assert!(same_pending(Some(pending), parked.pending()));
            assert_setter_did_not_move_scan(&before, &parked.meter());
            assert_eq!(parked.stats().live_roots, roots);
            let continued = parked.pulse(1);
            let _ = continued;
            assert_eq!(
                parked.meter().outgoing_edge_checks,
                before.outgoing_edge_checks + 1
            );
            assert_eq!(parked.meter().owner_head_checks, before.owner_head_checks);
            assert!(
                !parked.marked(graph.subject),
                "the original scan still uses the weak mode"
            );
            let next = parked.pulse(1);
            let _ = next;
            let queued = parked.pending().expect("another owner is still pending");
            assert_eq!(queued.owner, graph.old_mt.slot());
            assert_eq!(queued.cursor, graph.old_mode_record);
            assert!(
                !parked.marked(graph.subject),
                "the queued owner is not a chain-head rescan"
            );
            assert_subject_marked(parked, graph.subject);
            assert_subject_marked(parked, graph.child);
        });
        finish_kept_cycle(&mut host, &graph);
    });
}

#[test]
fn gray_queued_weak_owner_reads_new_mode_without_extra_rescan() {
    fn run(change_during_gray: bool) -> ScanMeter {
        let mut meter = ScanMeter::default();
        crate::with_idle_vm(|vm| {
            let mut host = vm.host_context();
            if change_during_gray {
                let graph = open_weak_graph(&mut host, WeakOwnerPlace::Queued);
                resume_weak(&mut host, |parked| {
                    pause_at(parked, graph.sentinel.slot(), graph.sentinel_record);
                    assert!(parked.marked(graph.left), "the root visit queued the owner");
                    assert_ne!(
                        parked.pending().expect("sentinel").owner,
                        graph.left.slot(),
                        "the left table has not started"
                    );
                    assert!(!parked.marked(graph.subject));
                    write_metatable(parked, graph.left, Some(graph.strong_mt))
                        .expect("strengthen before the ordinary scan");
                    assert!(!parked.marked(graph.subject));
                });
                let done = finish_fixed_cycle(&mut host);
                assert!(
                    matches!(done, crate::heap::StepOutcome::Completed(_)),
                    "{done:?}"
                );
                meter = heap::with_heap(&mut host, |heap| {
                    assert!(
                        heap.alive(graph.subject),
                        "the ordinary scan reads the new mode"
                    );
                    assert!(heap.alive(graph.child));
                    Ok(heap.scan_meter())
                })
                .expect("queued");
            } else {
                heap::install(&mut host, limits(16, 8, 24)).expect("install");
                heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
                    let graph = plant_weak_graph(heap, WeakOwnerPlace::Queued)?;
                    heap.set_metatable(graph.left, Some(graph.strong_mt))?;
                    Ok(())
                })
                .expect("pre-cycle");
                heap::open_fixed_step(&mut host).expect("open");
                let done = finish_fixed_cycle(&mut host);
                assert!(
                    matches!(done, crate::heap::StepOutcome::Completed(_)),
                    "{done:?}"
                );
                meter = heap::take_scan_meter(&mut host).expect("meter");
            }
        });
        meter
    }
    let during = run(true);
    let before = run(false);
    assert_eq!(
        during.owner_head_checks, before.owner_head_checks,
        "a not-yet-started owner is not rescanned"
    );
    assert_eq!(
        during.outgoing_edge_checks, before.outgoing_edge_checks,
        "the ordinary scan walks the chain once"
    );
}

#[test]
fn gray_scanned_weak_owner_repeated_strengthen_schedules_one_rescan() {
    fn heads_until_marked(repeats: u32) -> u64 {
        let mut heads = 0;
        crate::with_idle_vm(|vm| {
            let mut host = vm.host_context();
            let graph = open_weak_graph(&mut host, WeakOwnerPlace::Scanned);
            resume_weak(&mut host, |parked| {
                pause_at(parked, graph.sentinel.slot(), graph.sentinel_record);
                let before = parked.meter();
                let pending = parked.pending();
                let roots = parked.stats().live_roots;
                for _ in 0..repeats {
                    write_metatable(parked, graph.left, Some(graph.strong_mt)).expect("attach");
                    write_metatable(parked, graph.left, None).expect("clear");
                    write_metatable(parked, graph.left, Some(graph.strong_mt))
                        .expect("attach again");
                }
                assert!(same_pending(pending, parked.pending()));
                assert_setter_did_not_move_scan(&before, &parked.meter());
                assert_eq!(parked.stats().live_roots, roots);
                assert_subject_marked(parked, graph.subject);
                heads = parked.meter().owner_head_checks;
            });
        });
        heads
    }
    let once = heads_until_marked(1);
    let again = heads_until_marked(3);
    assert_eq!(once, again, "repeated attach or clear leaves one rescan");
}

#[test]
fn published_tail_refuses_live_weak_strengthen_without_moving_state() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = open_weak_graph(&mut host, WeakOwnerPlace::Scanned);
        let published = heap::step_fixed_frames(&mut host, 128).expect("publish");
        assert!(
            matches!(published, crate::heap::StepOutcome::Published { .. }),
            "{published:?}"
        );
        let before_tail = heap::saved_tail_state(&mut host)
            .expect("tail read")
            .expect("published tail");
        let refused = heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let before_stats = heap.stats();
            let before_meter = heap.scan_meter();
            let before_mt = heap.metatable(graph.left)?;
            assert_eq!(before_mt.unwrap().slot(), graph.old_mt.slot());
            assert_eq!(before_mt.unwrap().generation(), graph.old_mt.generation());
            let write = heap.set_metatable(graph.left, Some(graph.strong_mt));
            assert!(
                matches!(write, Err(HeapError::SweepPending)),
                "a live same-generation write is refused, got {write:?}"
            );
            let after_mt = heap.metatable(graph.left)?;
            assert_eq!(after_mt.unwrap().slot(), graph.old_mt.slot());
            assert_eq!(after_mt.unwrap().generation(), graph.old_mt.generation());
            assert_eq!(heap.stats(), before_stats);
            assert_eq!(heap.scan_meter(), before_meter);
            heap.set_metatable(graph.left, Some(graph.strong_mt))
        });
        assert!(matches!(refused, Err(HeapError::SweepPending)));
        let after_tail = heap::saved_tail_state(&mut host)
            .expect("tail read")
            .expect("tail still saved");
        assert_eq!(before_tail.phase, after_tail.phase);
        assert_eq!(before_tail.cursor, after_tail.cursor);
        assert_eq!(before_tail.live_roots, after_tail.live_roots);
        assert_eq!(before_tail.dead_len, after_tail.dead_len);
    });
}

/// One weak-value owner, paused on the rescan record that `open_rescan` just
/// installed. The successor, donor, and foreign target are not roots.
struct RescanOwner {
    owner: TableRef,
    weak_mt: TableRef,
    strong_mt: TableRef,
    sentinel: TableRef,
    pending_value: TableRef,
    suffix: Option<TableRef>,
    donor: TableRef,
    foreign: TableRef,
    dust: TableRef,
}

fn plant_rescan_owner(heap: &mut LuaHeap, successor: bool) -> Result<RescanOwner, HeapError> {
    let owner = heap.alloc()?;
    let weak_mt = heap.alloc()?;
    let strong_mt = heap.alloc()?;
    let sentinel = heap.alloc()?;
    let pending_value = heap.alloc()?;
    let suffix = if successor { Some(heap.alloc()?) } else { None };
    let donor = heap.alloc()?;
    let foreign = heap.alloc()?;
    let dust = heap.alloc()?;
    heap.set_mode_scalar(weak_mt, Value::string("v"))?;
    heap.set_metatable(owner, Some(weak_mt))?;
    heap.set_edge(owner, KEY_Y, pending_value)?;
    if let Some(suffix) = suffix {
        heap.set_edge(owner, KEY_Z, suffix)?;
    }
    heap.set_named_scalar(sentinel, "pending", Value::string("keep"))?;
    let frame = heap.enter_roots(4)?;
    heap.write_root(frame, 0, sentinel)?;
    heap.write_root(frame, 1, weak_mt)?;
    heap.write_root(frame, 2, strong_mt)?;
    heap.write_root(frame, 3, owner)?;
    Ok(RescanOwner {
        owner,
        weak_mt,
        strong_mt,
        sentinel,
        pending_value,
        suffix,
        donor,
        foreign,
        dust,
    })
}

fn open_rescan_owner(host: &mut vm::HostContext<'_>, successor: bool) -> RescanOwner {
    heap::install(host, limits(16, 8, 24)).expect("install");
    let graph = heap::with_heap(host, |heap| plant_rescan_owner(heap, successor)).expect("plant");
    heap::open_fixed_step(host).expect("open");
    graph
}

fn strengthen_open_owner(parked: &mut ParkedMark<'_>, graph: &RescanOwner, record: u32) {
    let mt = parked.metatable(graph.owner).expect("metatable");
    assert_eq!(
        mt.expect("owner metatable").slot(),
        graph.weak_mt.slot(),
        "the owner is still weak when the ordinary scan reaches the record"
    );
    let ordinary = pause_at(parked, graph.owner.slot(), record);
    assert!(
        !parked.marked(graph.pending_value),
        "the weak scan has not charged the pending value"
    );
    let before = parked.meter();
    let roots = parked.stats().live_roots;
    write_metatable(parked, graph.owner, Some(graph.strong_mt)).expect("strengthen");
    assert!(
        same_pending(Some(ordinary), parked.pending()),
        "the setter does not move the ordinary cursor"
    );
    assert_setter_did_not_move_scan(&before, &parked.meter());
    assert_eq!(
        parked.meter().owner_rescan_heads,
        before.owner_rescan_heads,
        "the setter does not open the rescan"
    );
    assert_eq!(
        parked.meter().owner_rescan_schedules,
        before.owner_rescan_schedules + 1
    );
    assert_eq!(parked.stats().live_roots, roots, "the setter adds no root");
}

/// Pulses until the real rescan cursor is this record. Does not invent a step count.
fn pause_on_rescan(parked: &mut ParkedMark<'_>, owner: u32, cursor: u32) -> PendingScan {
    for _ in 0..HARNESS_BOUND {
        let view = parked.pulse(1);
        assert!(
            !view.finished,
            "mark finished before rescan owner {owner} record {cursor}"
        );
        assert_eq!(view.meter.sweep_edge_checks, 0, "a pulse must not sweep");
        assert_eq!(
            view.meter.mark_passes, 1,
            "pause stays in the original pass"
        );
        if let Some(pending) = parked.pending_rescan()
            && pending.owner == owner
            && pending.cursor == cursor
        {
            assert!(
                parked.pending().is_none(),
                "the ordinary cursor is not this rescan"
            );
            assert_eq!(parked.phase_name(), "gray");
            assert!(parked.mutation_open());
            return pending;
        }
    }
    panic!("rescan record {cursor} on owner {owner} was not observed");
}

fn pause_rescan(parked: &mut ParkedMark<'_>, graph: &RescanOwner) -> (u32, Option<u32>) {
    let record = parked
        .field_index(graph.owner, KEY_Y)
        .expect("pending record");
    let suffix = graph.suffix.map(|_| {
        parked
            .field_index(graph.owner, KEY_Z)
            .expect("suffix record")
    });
    strengthen_open_owner(parked, graph, record);
    let pending = pause_on_rescan(parked, graph.owner.slot(), record);
    assert_eq!(pending.cursor, record);
    assert!(!parked.marked(graph.pending_value));
    if let Some(suffix) = graph.suffix {
        assert!(!parked.marked(suffix), "rescan has not charged the suffix");
    }
    assert!(!parked.marked(graph.foreign));
    assert!(!parked.marked(graph.donor));
    assert!(!parked.marked(graph.dust));
    (record, suffix)
}

fn finish_published(host: &mut vm::HostContext<'_>) {
    let done = finish_fixed_cycle(host);
    assert!(
        matches!(done, crate::heap::StepOutcome::Completed(_)),
        "{done:?}"
    );
    assert!(heap::fixed_step_idle(host).expect("idle"));
}

#[test]
fn rescan_delete_keeps_the_successor() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = open_rescan_owner(&mut host, true);
        let suffix = graph.suffix.expect("suffix");
        resume_weak(&mut host, |parked| {
            let (record, suffix_index) = pause_rescan(parked, &graph);
            let suffix_index = suffix_index.expect("suffix record");
            let next = parked.with_saved_heap(|heap| heap.edge_slots[record as usize].next);
            assert_eq!(
                next,
                Some(suffix_index),
                "the pending record has a successor"
            );
            parked
                .delete_field(graph.owner, KEY_Y)
                .expect("delete the pending record");
            let after_delete = parked.pending_rescan();
            let repairs = parked.meter().parked_rescan_cursor_repairs;
            // The next pulse reads this record. A stale cursor hits the empty-record expect.
            let checks = parked.meter().outgoing_edge_checks;
            let view = parked.pulse(1);
            assert!(
                parked.marked(suffix),
                "this pulse marks the successor; cursor after delete {after_delete:?}, repairs {repairs}"
            );
            assert!(!view.finished, "one edge charge does not finish the mark");
            assert_eq!(
                view.meter.outgoing_edge_checks,
                checks + 1,
                "pulse(1) charges one edge"
            );
            assert_eq!(view.meter.sweep_edge_checks, 0, "a pulse must not sweep");
            assert_eq!(
                after_delete.map(|scan| (scan.owner, scan.cursor)),
                Some((graph.owner.slot(), suffix_index))
            );
            assert_eq!(parked.meter().parked_rescan_cursor_repairs, 1);
            assert_eq!(parked.meter().parked_cursor_repairs, 0);
        });
        finish_published(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            assert!(
                heap.alive(suffix),
                "the published sweep keeps the marked suffix"
            );
            assert!(
                !heap.alive(graph.pending_value),
                "the deleted value is not kept"
            );
            assert!(!heap.alive(graph.dust), "sweep released an unmarked node");
            assert!(!heap.alive(graph.foreign));
            assert_eq!(heap.edge(graph.owner, KEY_Z)?, Some(suffix));
            assert_eq!(heap.edge(graph.owner, KEY_Y)?, None);
            Ok(())
        })
        .expect("published");
    });
}

#[test]
fn rescan_reuse_keeps_the_owner_suffix() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = open_rescan_owner(&mut host, true);
        let suffix = graph.suffix.expect("suffix");
        resume_weak(&mut host, |parked| {
            let (record, suffix_index) = pause_rescan(parked, &graph);
            let suffix_index = suffix_index.expect("suffix record");
            parked
                .delete_field(graph.owner, KEY_Y)
                .expect("delete the pending record");
            let barriers = parked.meter().parked_barrier_marks;
            let writes = parked.meter().parked_edge_writes;
            let checks = parked.meter().outgoing_edge_checks;
            parked
                .write_field(graph.donor, KEY_W, graph.foreign)
                .expect("another owner reuses the freed record");
            let reused = parked
                .field_index(graph.donor, KEY_W)
                .expect("reused record");
            assert_eq!(
                reused, record,
                "the free list reuses the deleted physical index"
            );
            assert!(
                !parked.marked(graph.foreign),
                "the ordinary write does not mark the foreign target"
            );
            assert_eq!(
                parked.meter().parked_barrier_marks,
                barriers,
                "the insert is not a strong barrier"
            );
            assert_eq!(parked.meter().parked_edge_writes, writes + 1);
            assert_eq!(
                parked.meter().outgoing_edge_checks,
                checks,
                "the write does not scan"
            );
            assert_eq!(
                parked.stats().live_roots,
                4,
                "the foreign target is not a root"
            );
            let after_delete = parked.pending_rescan();
            let view = parked.pulse(1);
            assert!(
                parked.marked(suffix),
                "this pulse marks the original suffix, not the reused owner; foreign marked={}, cursor {after_delete:?}, reused {reused}",
                parked.marked(graph.foreign)
            );
            assert!(
                !parked.marked(graph.foreign),
                "the foreign target stays unmarked"
            );
            assert!(!view.finished, "one edge charge does not finish the mark");
            assert_eq!(
                view.meter.outgoing_edge_checks,
                checks + 1,
                "pulse(1) charges one edge"
            );
            assert_eq!(
                after_delete.map(|scan| (scan.owner, scan.cursor)),
                Some((graph.owner.slot(), suffix_index))
            );
            assert_eq!(parked.meter().parked_rescan_cursor_repairs, 1);
            assert_eq!(parked.meter().parked_cursor_repairs, 0);
        });
        finish_published(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            assert!(
                heap.alive(suffix),
                "the published sweep keeps the marked suffix"
            );
            assert!(!heap.alive(graph.foreign), "the foreign target is cleared");
            assert!(!heap.alive(graph.donor));
            assert!(!heap.alive(graph.pending_value));
            assert!(!heap.alive(graph.dust), "sweep released an unmarked node");
            assert_eq!(heap.edge(graph.owner, KEY_Z)?, Some(suffix));
            Ok(())
        })
        .expect("published");
    });
}

#[test]
fn rescan_tail_delete_ends_the_scan() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = open_rescan_owner(&mut host, false);
        let mut after_delete = None;
        let mut repairs = 0u64;
        let mut ordinary = 0u64;
        resume_weak(&mut host, |parked| {
            let (record, suffix) = pause_rescan(parked, &graph);
            assert!(suffix.is_none(), "the tail fixture has no successor");
            let next = parked.with_saved_heap(|heap| heap.edge_slots[record as usize].next);
            assert_eq!(next, None, "the pending record is the chain tail");
            parked
                .delete_field(graph.owner, KEY_Y)
                .expect("delete the tail record");
            after_delete = parked.pending_rescan();
            repairs = parked.meter().parked_rescan_cursor_repairs;
            ordinary = parked.meter().parked_cursor_repairs;
            // Leave the mark parked. The host pulse is the one that would read the freed tail.
        });
        finish_published(&mut host);
        assert!(
            after_delete.is_none(),
            "deleting the tail ends the rescan, got {after_delete:?}"
        );
        assert_eq!(repairs, 1, "the tail repair is one rescan cursor repair");
        assert_eq!(ordinary, 0, "the ordinary cursor was not on this record");
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            assert!(
                !heap.alive(graph.pending_value),
                "the deleted tail is not kept"
            );
            assert!(!heap.alive(graph.dust), "sweep released an unmarked node");
            assert!(
                !heap.alive(graph.foreign),
                "the tail delete marks nothing extra"
            );
            assert!(!heap.alive(graph.donor));
            Ok(())
        })
        .expect("published");
    });
}

#[test]
fn rescan_unrelated_delete_leaves_the_cursor() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = open_rescan_owner(&mut host, true);
        let suffix = graph.suffix.expect("suffix");
        resume_weak(&mut host, |parked| {
            let (record, _) = pause_rescan(parked, &graph);
            let before = parked.pending_rescan();
            parked
                .with_saved_heap(|heap| {
                    heap.set_named_scalar(graph.sentinel, "pending", Value::Null)
                })
                .expect("ordinary nil write on another owner");
            let after = parked.pending_rescan();
            assert_eq!(after.map(|scan| scan.cursor), Some(record));
            assert!(
                same_pending(before, after),
                "a miss does not move the rescan cursor"
            );
            assert_eq!(parked.meter().parked_rescan_cursor_repairs, 0);
            assert_eq!(parked.meter().parked_cursor_repairs, 0);
            let checks = parked.meter().outgoing_edge_checks;
            let view = parked.pulse(1);
            assert!(
                parked.marked(graph.pending_value),
                "the pending record is still charged"
            );
            assert!(!parked.marked(graph.foreign));
            assert!(!view.finished, "one edge charge does not finish the mark");
            assert_eq!(view.meter.outgoing_edge_checks, checks + 1);
            assert_eq!(view.meter.sweep_edge_checks, 0);
        });
        finish_published(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            assert!(heap.alive(graph.pending_value));
            assert!(
                heap.alive(suffix),
                "the missed delete does not drop the suffix"
            );
            assert!(!heap.alive(graph.dust));
            assert!(!heap.alive(graph.foreign));
            Ok(())
        })
        .expect("published");
    });
}

/// Bare userdata under numeric key 1 of a weak-value table. The value has no
/// independent strong root. A sibling shares the original `v` metatable and
/// holds a different userdata. A third weak table observes the same userdata.
/// A separate weak table holds an orphan userdata.
struct BareUserdata {
    sentinel: TableRef,
    left: TableRef,
    right: TableRef,
    witness: TableRef,
    orphan_table: TableRef,
    weak_mt: TableRef,
    strong_mt: TableRef,
    subject: UserdataRef,
    sibling: UserdataRef,
    orphan: UserdataRef,
    dust: TableRef,
    sentinel_record: u32,
    subject_record: u32,
}

#[derive(Clone, Copy)]
enum BareArm {
    Swap,
    Clear,
    Omit,
}

fn plant_bare_userdata(heap: &mut LuaHeap) -> Result<BareUserdata, HeapError> {
    let sentinel = heap.alloc()?;
    let left = heap.alloc()?;
    let right = heap.alloc()?;
    let witness = heap.alloc()?;
    let orphan_table = heap.alloc()?;
    let weak_mt = heap.alloc()?;
    let strong_mt = heap.alloc()?;
    let subject = heap.alloc_userdata()?;
    let sibling = heap.alloc_userdata()?;
    let orphan = heap.alloc_userdata()?;
    let dust = heap.alloc()?;
    heap.set_mode_scalar(weak_mt, Value::string("v"))?;
    heap.set_metatable(left, Some(weak_mt))?;
    heap.set_metatable(right, Some(weak_mt))?;
    heap.set_metatable(witness, Some(weak_mt))?;
    heap.set_metatable(orphan_table, Some(weak_mt))?;
    heap.set_named_scalar(sentinel, "pending", Value::Bool(true))?;
    heap.set_named_scalar(left, "charged", Value::Bool(true))?;
    heap.set_number_userdata(left, 1.0, subject)?;
    heap.set_number_userdata(right, 1.0, sibling)?;
    heap.set_number_userdata(witness, 1.0, subject)?;
    heap.set_number_userdata(orphan_table, 1.0, orphan)?;
    let frame = heap.enter_roots(6)?;
    // Last written root is scanned first. The sentinel stays pending until
    // the owners written after it have been scanned.
    heap.write_root(frame, 0, sentinel)?;
    heap.write_root(frame, 1, right)?;
    heap.write_root(frame, 2, witness)?;
    heap.write_root(frame, 3, orphan_table)?;
    heap.write_root(frame, 4, strong_mt)?;
    heap.write_root(frame, 5, left)?;
    assert_userdata_unrooted(heap, subject);
    assert_userdata_unrooted(heap, sibling);
    assert_userdata_unrooted(heap, orphan);
    let (_, sentinel_record) = heap
        .locate_named(sentinel.slot(), "pending")
        .expect("sentinel record");
    let subject_record = heap
        .number_edge_watch(left, 1.0)
        .expect("numeric userdata edge")
        .0;
    let _ = frame;
    let _ = dust;
    Ok(BareUserdata {
        sentinel,
        left,
        right,
        witness,
        orphan_table,
        weak_mt,
        strong_mt,
        subject,
        sibling,
        orphan,
        dust,
        sentinel_record,
        subject_record,
    })
}

fn assert_userdata_unrooted(heap: &LuaHeap, value: UserdataRef) {
    for rooted in &heap.roots {
        if let Some(super::Rooted::Userdata(handle)) = rooted {
            assert_ne!(*handle, value, "userdata has no independent strong root");
        }
    }
}

fn userdata_marked(parked: &mut ParkedMark<'_>, value: UserdataRef) -> bool {
    parked.with_saved_heap(|heap| {
        slot_is_marked(
            heap,
            value.slot(),
            value.generation(),
            super::HandleKind::Userdata,
        )
    })
}

fn open_bare_userdata(host: &mut vm::HostContext<'_>) -> BareUserdata {
    heap::install(host, limits(16, 8, 24)).expect("install");
    let graph = heap::with_heap(host, plant_bare_userdata).expect("plant");
    heap::open_fixed_step(host).expect("open");
    graph
}

fn prove_scanned_white(parked: &mut ParkedMark<'_>, graph: &BareUserdata) {
    let pending = pause_at(parked, graph.sentinel.slot(), graph.sentinel_record);
    assert_eq!(pending.owner, graph.sentinel.slot());
    assert_eq!(pending.cursor, graph.sentinel_record);
    assert_ne!(
        pending.cursor, graph.subject_record,
        "the pending record is not the userdata edge"
    );
    assert!(parked.marked(graph.left), "the owner weak edge was scanned");
    assert!(
        parked.meter().outgoing_edge_checks > 0,
        "the owner scan charged an edge"
    );
    assert!(
        !userdata_marked(parked, graph.subject),
        "the weak scan left the userdata white"
    );
    assert!(!userdata_marked(parked, graph.sibling));
    assert!(!userdata_marked(parked, graph.orphan));
    let record = parked
        .with_saved_heap(|heap| heap.number_edge_watch(graph.left, 1.0))
        .expect("userdata edge remains")
        .0;
    assert_eq!(record, graph.subject_record);
    parked.with_saved_heap(|heap| {
        assert_userdata_unrooted(heap, graph.subject);
        assert_eq!(
            heap.metatable(graph.right).expect("sibling metatable"),
            Some(graph.weak_mt)
        );
        assert_eq!(
            heap.metatable(graph.left).expect("owner metatable"),
            Some(graph.weak_mt)
        );
    });
}

fn refuse_parked_userdata_write(parked: &mut ParkedMark<'_>, graph: &BareUserdata) {
    let pending = parked.pending();
    let before = parked.meter();
    let refused =
        parked.with_saved_heap(|heap| heap.set_number_userdata(graph.left, 1.0, graph.sibling));
    assert!(
        matches!(refused, Err(HeapError::MarkDenied)),
        "a paused mark refuses an unbarriered userdata write, got {refused:?}"
    );
    assert!(same_pending(pending, parked.pending()));
    assert_setter_did_not_move_scan(&before, &parked.meter());
    assert!(!userdata_marked(parked, graph.subject));
    let stored =
        parked.with_saved_heap(|heap| heap.number_userdata(graph.left, 1.0).expect("read"));
    assert_eq!(stored, Some(graph.subject));
}

fn strengthen_bare(parked: &mut ParkedMark<'_>, graph: &BareUserdata, arm: BareArm) {
    let pending = parked.pending();
    let before = parked.meter();
    let roots = parked.stats().live_roots;
    match arm {
        BareArm::Swap => {
            write_metatable(parked, graph.left, Some(graph.strong_mt))
                .expect("replace with the preallocated strong metatable");
        }
        BareArm::Clear => {
            write_metatable(parked, graph.left, None).expect("clear the weak metatable");
        }
        BareArm::Omit => unreachable!("omit does not call the setter"),
    }
    assert!(same_pending(pending, parked.pending()));
    assert_setter_did_not_move_scan(&before, &parked.meter());
    assert_eq!(parked.stats().live_roots, roots);
    assert_eq!(
        parked.meter().owner_rescan_heads,
        before.owner_rescan_heads,
        "the setter does not open the rescan"
    );
    assert_eq!(
        parked.meter().owner_rescan_schedules,
        before.owner_rescan_schedules + 1
    );
    assert!(!userdata_marked(parked, graph.subject));
    let rescan = pause_on_rescan(parked, graph.left.slot(), graph.subject_record);
    assert_eq!(rescan.cursor, graph.subject_record);
    assert!(
        parked.meter().owner_rescan_heads > before.owner_rescan_heads,
        "a budgeted pulse opened the rescan"
    );
    assert!(
        !userdata_marked(parked, graph.subject),
        "the rescan cursor has not charged the userdata yet"
    );
    let checks = parked.meter().outgoing_edge_checks;
    let _view = parked.pulse(1);
    assert_eq!(parked.meter().outgoing_edge_checks, checks + 1);
    assert!(
        userdata_marked(parked, graph.subject),
        "budgeted rescan did not keep the same userdata alive"
    );
    let stored =
        parked.with_saved_heap(|heap| heap.number_userdata(graph.left, 1.0).expect("read"));
    assert_eq!(stored, Some(graph.subject));
}

fn finish_bare_kept(host: &mut vm::HostContext<'_>, graph: &BareUserdata) {
    let done = finish_fixed_cycle(host);
    assert!(
        matches!(done, crate::heap::StepOutcome::Completed(_)),
        "{done:?}"
    );
    assert!(heap::fixed_step_idle(host).expect("idle"));
    heap::with_heap(host, |heap| -> Result<(), HeapError> {
        let found = heap
            .number_userdata(graph.left, 1.0)?
            .expect("original userdata");
        assert_eq!(found.slot(), graph.subject.slot());
        assert_eq!(found.generation(), graph.subject.generation());
        assert!(heap.userdata_alive(found));
        assert_eq!(
            heap.number_userdata(graph.witness, 1.0)?,
            Some(graph.subject)
        );
        assert_eq!(heap.number_userdata(graph.right, 1.0)?, None);
        assert!(!heap.userdata_alive(graph.sibling));
        assert_eq!(heap.number_userdata(graph.orphan_table, 1.0)?, None);
        assert!(!heap.userdata_alive(graph.orphan));
        assert!(!heap.alive(graph.dust), "sweep released an unrooted node");
        heap.set_metatable(graph.left, Some(graph.weak_mt))?;
        Ok(())
    })
    .expect("kept cycle");
    heap::collect(host).expect("next cycle after restoring v");
    heap::with_heap(host, |heap| -> Result<(), HeapError> {
        assert!(
            !heap.userdata_alive(graph.subject),
            "the next cycle clears the restored weak value"
        );
        assert_eq!(heap.number_userdata(graph.left, 1.0)?, None);
        assert_eq!(heap.number_userdata(graph.witness, 1.0)?, None);
        Ok(())
    })
    .expect("released");
}

fn finish_bare_omitted(host: &mut vm::HostContext<'_>, graph: &BareUserdata) {
    let done = finish_fixed_cycle(host);
    assert!(
        matches!(done, crate::heap::StepOutcome::Completed(_)),
        "{done:?}"
    );
    assert!(heap::fixed_step_idle(host).expect("idle"));
    heap::with_heap(host, |heap| -> Result<(), HeapError> {
        assert_eq!(heap.number_userdata(graph.left, 1.0)?, None);
        assert!(!heap.userdata_alive(graph.subject));
        assert_eq!(heap.number_userdata(graph.witness, 1.0)?, None);
        assert_eq!(heap.number_userdata(graph.right, 1.0)?, None);
        assert!(!heap.userdata_alive(graph.sibling));
        assert_eq!(heap.number_userdata(graph.orphan_table, 1.0)?, None);
        assert!(!heap.userdata_alive(graph.orphan));
        assert!(!heap.alive(graph.dust));
        heap.set_metatable(graph.left, Some(graph.weak_mt))?;
        Ok(())
    })
    .expect("omitted cycle");
    heap::collect(host).expect("next cycle");
    heap::with_heap(host, |heap| {
        assert!(!heap.userdata_alive(graph.subject));
        assert_eq!(heap.number_userdata(graph.left, 1.0)?, None);
        Ok(())
    })
    .expect("still clear");
}

fn run_bare_userdata(arm: BareArm) {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = open_bare_userdata(&mut host);
        resume_weak(&mut host, |parked| {
            prove_scanned_white(parked, &graph);
            refuse_parked_userdata_write(parked, &graph);
            if !matches!(arm, BareArm::Omit) {
                strengthen_bare(parked, &graph, arm);
            }
        });
        match arm {
            BareArm::Omit => finish_bare_omitted(&mut host, &graph),
            BareArm::Swap | BareArm::Clear => finish_bare_kept(&mut host, &graph),
        }
    });
}

#[test]
fn gray_scanned_weak_owner_strengthen_marks_bare_userdata() {
    run_bare_userdata(BareArm::Swap);
}

#[test]
fn gray_scanned_weak_owner_clear_metatable_marks_bare_userdata() {
    run_bare_userdata(BareArm::Clear);
}

#[test]
fn gray_scanned_weak_owner_omitted_clears_bare_userdata() {
    run_bare_userdata(BareArm::Omit);
}

const LEFT_SCALAR: &str = "101";
const RIGHT_SCALAR: &str = "202";
const KEY_PAUSE_BOUND: u32 = 256;

/// Stops on a real pending record. Checks the cursor before pulsing, so a
/// record that is already pending is not charged by the search itself.
fn pause_record(parked: &mut ParkedMark<'_>, owner: u32, cursor: u32) -> PendingScan {
    for _ in 0..KEY_PAUSE_BOUND {
        if let Some(pending) = parked.pending()
            && pending.owner == owner
            && pending.cursor == cursor
        {
            assert_eq!(parked.phase_name(), "gray");
            assert!(parked.mutation_open());
            assert_eq!(parked.meter().sweep_edge_checks, 0);
            assert_eq!(parked.meter().mark_passes, 1);
            return pending;
        }
        let view = parked.pulse(1);
        assert!(
            !view.finished,
            "mark finished before owner {owner} record {cursor} was pending"
        );
        assert_eq!(view.meter.sweep_edge_checks, 0, "a pulse must not sweep");
        assert_eq!(
            view.meter.mark_passes, 1,
            "pause stays in the original pass"
        );
    }
    panic!("pending record {cursor} on owner {owner} was not observed");
}

/// Charges the pending record and requires every named target to stay white.
/// `marked(owner)` is not this proof: the cursor was that record, and the
/// charge incremented the occupied-edge meter.
fn charge_white(parked: &mut ParkedMark<'_>, owner: u32, record: u32, white: &[TableRef]) {
    let pending = pause_record(parked, owner, record);
    assert_eq!(pending.owner, owner);
    assert_eq!(pending.cursor, record);
    let edges = parked.meter().outgoing_edge_checks;
    let gaps = parked.meter().outgoing_gap_checks;
    let view = parked.pulse(1);
    assert!(!view.finished, "charging one record finished the mark");
    assert_eq!(
        parked.meter().outgoing_edge_checks,
        edges + 1,
        "the pending record was not charged"
    );
    assert_eq!(parked.meter().outgoing_gap_checks, gaps);
    for table in white {
        assert!(
            !parked.marked(*table),
            "weak scan marked slot {}",
            table.slot()
        );
    }
}

fn assert_still_white(parked: &ParkedMark<'_>, white: &[TableRef]) {
    for table in white {
        assert!(
            !parked.marked(*table),
            "slot {} was marked before the rescan budget",
            table.slot()
        );
    }
}

fn assert_setter_reserved(
    parked: &ParkedMark<'_>,
    pending: Option<PendingScan>,
    before: &ScanMeter,
    roots: u32,
) {
    assert!(
        same_pending(pending, parked.pending()),
        "set_metatable moved the ordinary pending cursor"
    );
    assert_setter_did_not_move_scan(before, &parked.meter());
    assert_eq!(parked.stats().live_roots, roots);
}

fn see_object_scalar(heap: &LuaHeap, owner: TableRef, key: TableRef, scalar: &str) -> bool {
    let Ok(found) = heap.object_value(owner, key) else {
        return false;
    };
    if found != Value::string(scalar) {
        return false;
    }
    let Ok(carrier) = heap.issued_value(key) else {
        return false;
    };
    let mut cursor = Value::Null;
    let mut saw = false;
    while let Ok(Some((next_key, next_value))) = heap.next_guest(owner, &cursor) {
        if next_key == carrier && next_value == Value::string(scalar) {
            saw = true;
        }
        cursor = next_key;
    }
    saw
}

struct KeyGraph {
    sentinel: TableRef,
    left: TableRef,
    right: TableRef,
    witness: TableRef,
    old_mt: TableRef,
    strong_mt: TableRef,
    left_key: TableRef,
    successor: TableRef,
    right_key: TableRef,
    orphan: TableRef,
    dust: TableRef,
    key_record: u32,
    sentinel_record: u32,
}

fn plant_key_graph(heap: &mut LuaHeap) -> Result<KeyGraph, HeapError> {
    let sentinel = heap.alloc()?;
    let left = heap.alloc()?;
    let right = heap.alloc()?;
    let witness = heap.alloc()?;
    let old_mt = heap.alloc()?;
    let witness_mt = heap.alloc()?;
    let strong_mt = heap.alloc()?;
    let left_key = heap.alloc()?;
    let successor = heap.alloc()?;
    let right_key = heap.alloc()?;
    let orphan = heap.alloc()?;
    let dust = heap.alloc()?;
    heap.set_mode_scalar(old_mt, Value::string("k"))?;
    heap.set_mode_scalar(witness_mt, Value::string("v"))?;
    heap.set_metatable(left, Some(old_mt))?;
    heap.set_metatable(right, Some(old_mt))?;
    heap.set_metatable(witness, Some(witness_mt))?;
    heap.set_object_scalar(left, left_key, Value::string(LEFT_SCALAR))?;
    heap.set_named_edge(left_key, "next", successor)?;
    heap.set_object_scalar(right, right_key, Value::string(RIGHT_SCALAR))?;
    heap.set_named_edge(witness, "key", left_key)?;
    heap.set_named_edge(witness, "succ", successor)?;
    heap.set_named_edge(witness, "orphan", orphan)?;
    heap.issue_carrier(left_key)?;
    heap.set_named_scalar(sentinel, "pending", Value::Bool(true))?;
    assert_eq!(heap.object_edge(strong_mt, left_key)?, None);
    assert_eq!(heap.named_edge(strong_mt, "next")?, None);
    let frame = heap.enter_roots(5)?;
    heap.write_root(frame, 0, sentinel)?;
    heap.write_root(frame, 1, right)?;
    heap.write_root(frame, 2, witness)?;
    heap.write_root(frame, 3, strong_mt)?;
    heap.write_root(frame, 4, left)?;
    let key_record = heap
        .find_key(left.slot(), &super::EdgeKey::object(left_key))
        .expect("left object-key record");
    let (_, sentinel_record) = heap
        .locate_named(sentinel.slot(), "pending")
        .expect("sentinel record");
    let _ = (witness_mt, frame);
    Ok(KeyGraph {
        sentinel,
        left,
        right,
        witness,
        old_mt,
        strong_mt,
        left_key,
        successor,
        right_key,
        orphan,
        dust,
        key_record,
        sentinel_record,
    })
}

fn open_key_graph(host: &mut vm::HostContext<'_>) -> KeyGraph {
    heap::install(host, limits(16, 8, 24)).expect("install");
    let graph = heap::with_heap(host, plant_key_graph).expect("plant");
    heap::open_fixed_step(host).expect("open");
    graph
}

fn prove_key_then_sentinel(parked: &mut ParkedMark<'_>, graph: &KeyGraph) {
    let white = [
        graph.left_key,
        graph.successor,
        graph.right_key,
        graph.orphan,
    ];
    charge_white(parked, graph.left.slot(), graph.key_record, &white);
    let pending = pause_record(parked, graph.sentinel.slot(), graph.sentinel_record);
    assert_eq!(pending.owner, graph.sentinel.slot());
    assert_eq!(pending.cursor, graph.sentinel_record);
    assert!(
        parked.marked(graph.left),
        "the owner is marked, which alone does not prove the key record was charged"
    );
    assert_still_white(parked, &white);
}

fn finish_key_cycle(host: &mut vm::HostContext<'_>, graph: &KeyGraph, keep: bool) {
    let done = finish_fixed_cycle(host);
    assert!(
        matches!(done, crate::heap::StepOutcome::Completed(_)),
        "{done:?}"
    );
    assert!(heap::fixed_step_idle(host).expect("idle"));
    heap::with_heap(host, |heap| -> Result<(), HeapError> {
        assert!(!heap.alive(graph.right_key), "the sibling weak key clears");
        assert!(heap.next_guest(graph.right, &Value::Null)?.is_none());
        assert!(!heap.alive(graph.orphan), "the independent orphan clears");
        assert_eq!(heap.named_edge(graph.witness, "orphan")?, None);
        assert!(!heap.alive(graph.dust), "an unrooted node was released");
        if keep {
            assert!(heap.alive(graph.left_key), "the strengthened key stays");
            assert!(heap.alive(graph.successor), "the key successor stays");
            assert_eq!(heap.object_edge(graph.strong_mt, graph.left_key)?, None);
            assert!(see_object_scalar(
                heap,
                graph.left,
                graph.left_key,
                LEFT_SCALAR
            ));
            assert_eq!(
                heap.named_edge(graph.left_key, "next")?,
                Some(graph.successor)
            );
            assert_eq!(heap.named_edge(graph.witness, "key")?, Some(graph.left_key));
            assert_eq!(
                heap.named_edge(graph.witness, "succ")?,
                Some(graph.successor)
            );
        } else {
            assert!(!heap.alive(graph.left_key), "omission clears the weak key");
            assert!(!heap.alive(graph.successor));
            assert!(heap.next_guest(graph.left, &Value::Null)?.is_none());
            assert_eq!(heap.named_edge(graph.witness, "key")?, None);
            assert_eq!(heap.named_edge(graph.witness, "succ")?, None);
        }
        Ok(())
    })
    .expect("this cycle");
    if keep {
        heap::with_heap(host, |heap| -> Result<(), HeapError> {
            heap.set_metatable(graph.left, Some(graph.old_mt))?;
            assert!(
                heap.alive(graph.left_key),
                "restoring k does not drop the key"
            );
            assert!(see_object_scalar(
                heap,
                graph.left,
                graph.left_key,
                LEFT_SCALAR
            ));
            Ok(())
        })
        .expect("restore k");
        heap::collect(host).expect("next cycle");
        heap::with_heap(host, |heap| {
            assert!(
                !heap.alive(graph.left_key),
                "the next cycle releases the key"
            );
            assert!(
                !heap.alive(graph.successor),
                "the next cycle releases the successor"
            );
            assert!(
                heap.next_guest(graph.left, &Value::Null)
                    .expect("next")
                    .is_none()
            );
            Ok(())
        })
        .expect("released");
    } else {
        heap::with_heap(host, |heap| -> Result<(), HeapError> {
            heap.set_metatable(graph.left, Some(graph.strong_mt))?;
            Ok(())
        })
        .expect("late strong metatable");
        heap::collect(host).expect("collect after a cleared key");
        heap::with_heap(host, |heap| {
            assert!(
                !heap.alive(graph.left_key),
                "a cleared key does not come back"
            );
            assert!(!heap.alive(graph.successor));
            Ok(())
        })
        .expect("no resurrection");
    }
}

#[derive(Clone, Copy)]
enum KeyArm {
    Swap,
    Clear,
    Omit,
}

fn run_key_arm(arm: KeyArm) {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = open_key_graph(&mut host);
        resume_weak(&mut host, |parked| {
            prove_key_then_sentinel(parked, &graph);
            let pending = parked.pending();
            let before = parked.meter();
            let roots = parked.stats().live_roots;
            match arm {
                KeyArm::Swap => {
                    write_metatable(parked, graph.left, Some(graph.strong_mt))
                        .expect("preallocated strong metatable");
                }
                KeyArm::Clear => {
                    write_metatable(parked, graph.left, None).expect("clear the weak metatable");
                }
                KeyArm::Omit => {}
            }
            if !matches!(arm, KeyArm::Omit) {
                assert_setter_reserved(parked, pending, &before, roots);
                let right_mt = parked.with_saved_heap(|heap| heap.metatable(graph.right));
                assert!(matches!(right_mt, Ok(Some(mt)) if mt == graph.old_mt));
                assert_still_white(
                    parked,
                    &[
                        graph.left_key,
                        graph.successor,
                        graph.right_key,
                        graph.orphan,
                    ],
                );
                assert_subject_marked(parked, graph.left_key);
                assert_subject_marked(parked, graph.successor);
            }
        });
        finish_key_cycle(&mut host, &graph, !matches!(arm, KeyArm::Omit));
    });
}

#[test]
fn gray_scanned_weak_key_swap_keeps_object_key_and_successor() {
    run_key_arm(KeyArm::Swap);
}

#[test]
fn gray_scanned_weak_key_clear_keeps_object_key_and_successor() {
    run_key_arm(KeyArm::Clear);
}

#[test]
fn gray_scanned_weak_key_omit_clears_object_key() {
    run_key_arm(KeyArm::Omit);
}

#[test]
fn gray_scanned_weak_key_sole_clear_keeps_object_key() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(8, 4, 8)).expect("install");
        let planted = heap::with_heap(&mut host, |heap| -> Result<_, HeapError> {
            let sentinel = heap.alloc()?;
            let left = heap.alloc()?;
            let old_mt = heap.alloc()?;
            let left_key = heap.alloc()?;
            let successor = heap.alloc()?;
            let dust = heap.alloc()?;
            heap.set_mode_scalar(old_mt, Value::string("k"))?;
            heap.set_metatable(left, Some(old_mt))?;
            heap.set_object_scalar(left, left_key, Value::string(LEFT_SCALAR))?;
            heap.set_named_edge(left_key, "next", successor)?;
            heap.issue_carrier(left_key)?;
            heap.set_named_scalar(sentinel, "pending", Value::Bool(true))?;
            let frame = heap.enter_roots(2)?;
            heap.write_root(frame, 0, sentinel)?;
            heap.write_root(frame, 1, left)?;
            let record = heap
                .find_key(left.slot(), &super::EdgeKey::object(left_key))
                .expect("object-key record");
            let (_, sentinel_record) = heap
                .locate_named(sentinel.slot(), "pending")
                .expect("sentinel");
            let _ = frame;
            Ok((
                sentinel,
                left,
                left_key,
                successor,
                dust,
                record,
                sentinel_record,
            ))
        })
        .expect("plant");
        let (sentinel, left, left_key, successor, dust, record, sentinel_record) = planted;
        heap::open_fixed_step(&mut host).expect("open");
        resume_weak(&mut host, |parked| {
            charge_white(parked, left.slot(), record, &[left_key, successor]);
            pause_record(parked, sentinel.slot(), sentinel_record);
            assert_still_white(parked, &[left_key, successor]);
            let edges = parked.with_saved_heap(|heap| heap.metatable_edges);
            assert_eq!(edges, 1, "this clear drops the only metatable edge");
            let pending = parked.pending();
            let before = parked.meter();
            let roots = parked.stats().live_roots;
            write_metatable(parked, left, None).expect("clear the only metatable");
            assert_eq!(parked.with_saved_heap(|heap| heap.metatable_edges), 0);
            assert_setter_reserved(parked, pending, &before, roots);
            assert_eq!(parked.meter().node_slot_visits, before.node_slot_visits);
            assert_still_white(parked, &[left_key, successor]);
            assert_subject_marked(parked, left_key);
            assert_subject_marked(parked, successor);
        });
        let done = finish_fixed_cycle(&mut host);
        assert!(
            matches!(done, crate::heap::StepOutcome::Completed(_)),
            "{done:?}"
        );
        heap::with_heap(&mut host, |heap| {
            assert!(
                heap.alive(left_key),
                "old key weakness was read before the count hit zero"
            );
            assert!(heap.alive(successor));
            assert!(see_object_scalar(heap, left, left_key, LEFT_SCALAR));
            assert!(!heap.alive(dust), "sweep still released an unrooted node");
            Ok(())
        })
        .expect("sole edge");
    });
}

struct KvGraph {
    sentinel: TableRef,
    left: TableRef,
    right: TableRef,
    witness: TableRef,
    kv_mt: TableRef,
    value_mt: TableRef,
    key_mt: TableRef,
    strong_mt: TableRef,
    left_key: TableRef,
    key_succ: TableRef,
    value: TableRef,
    value_succ: TableRef,
    right_key: TableRef,
    right_value: TableRef,
    orphan: TableRef,
    dust: TableRef,
    key_record: u32,
    value_record: u32,
    sentinel_record: u32,
}

fn plant_kv_graph(heap: &mut LuaHeap) -> Result<KvGraph, HeapError> {
    let sentinel = heap.alloc()?;
    let left = heap.alloc()?;
    let right = heap.alloc()?;
    let witness = heap.alloc()?;
    let kv_mt = heap.alloc()?;
    let value_mt = heap.alloc()?;
    let key_mt = heap.alloc()?;
    let strong_mt = heap.alloc()?;
    let left_key = heap.alloc()?;
    let key_succ = heap.alloc()?;
    let value = heap.alloc()?;
    let value_succ = heap.alloc()?;
    let right_key = heap.alloc()?;
    let right_key_succ = heap.alloc()?;
    let right_value = heap.alloc()?;
    let right_value_succ = heap.alloc()?;
    let orphan = heap.alloc()?;
    let dust = heap.alloc()?;
    heap.set_mode_scalar(kv_mt, Value::string("kv"))?;
    heap.set_mode_scalar(value_mt, Value::string("v"))?;
    heap.set_mode_scalar(key_mt, Value::string("k"))?;
    heap.set_metatable(left, Some(kv_mt))?;
    heap.set_metatable(right, Some(kv_mt))?;
    heap.set_metatable(witness, Some(value_mt))?;
    heap.set_object_scalar(left, left_key, Value::string(LEFT_SCALAR))?;
    heap.set_named_edge(left_key, "next", key_succ)?;
    heap.set_named_edge(left, "value", value)?;
    heap.set_named_edge(value, "next", value_succ)?;
    heap.set_object_scalar(right, right_key, Value::string(RIGHT_SCALAR))?;
    heap.set_named_edge(right_key, "next", right_key_succ)?;
    heap.set_named_edge(right, "value", right_value)?;
    heap.set_named_edge(right_value, "next", right_value_succ)?;
    heap.set_named_edge(witness, "key", left_key)?;
    heap.set_named_edge(witness, "key_succ", key_succ)?;
    heap.set_named_edge(witness, "val", value)?;
    heap.set_named_edge(witness, "val_succ", value_succ)?;
    heap.set_named_edge(witness, "orphan", orphan)?;
    heap.issue_carrier(left_key)?;
    heap.issue_carrier(value)?;
    heap.set_named_scalar(sentinel, "pending", Value::Bool(true))?;
    for mt in [value_mt, key_mt, strong_mt] {
        assert_eq!(heap.object_edge(mt, left_key)?, None);
        assert_eq!(heap.named_edge(mt, "value")?, None);
        assert_eq!(heap.object_edge(mt, value)?, None);
    }
    let frame = heap.enter_roots(7)?;
    heap.write_root(frame, 0, sentinel)?;
    heap.write_root(frame, 1, right)?;
    heap.write_root(frame, 2, witness)?;
    heap.write_root(frame, 3, strong_mt)?;
    heap.write_root(frame, 4, value_mt)?;
    heap.write_root(frame, 5, key_mt)?;
    heap.write_root(frame, 6, left)?;
    let key_record = heap
        .find_key(left.slot(), &super::EdgeKey::object(left_key))
        .expect("object-key record");
    let (_, value_record) = heap
        .locate_named(left.slot(), "value")
        .expect("string-key record");
    let (_, sentinel_record) = heap
        .locate_named(sentinel.slot(), "pending")
        .expect("sentinel record");
    let _ = frame;
    Ok(KvGraph {
        sentinel,
        left,
        right,
        witness,
        kv_mt,
        value_mt,
        key_mt,
        strong_mt,
        left_key,
        key_succ,
        value,
        value_succ,
        right_key,
        right_value,
        orphan,
        dust,
        key_record,
        value_record,
        sentinel_record,
    })
}

fn open_kv_graph(host: &mut vm::HostContext<'_>) -> KvGraph {
    heap::install(host, limits(24, 8, 40)).expect("install");
    let graph = heap::with_heap(host, plant_kv_graph).expect("plant");
    heap::open_fixed_step(host).expect("open");
    graph
}

fn prove_both_records(parked: &mut ParkedMark<'_>, graph: &KvGraph) {
    let white = [
        graph.left_key,
        graph.key_succ,
        graph.value,
        graph.value_succ,
        graph.right_key,
        graph.right_value,
        graph.orphan,
    ];
    charge_white(parked, graph.left.slot(), graph.key_record, &white);
    charge_white(parked, graph.left.slot(), graph.value_record, &white);
    let pending = pause_record(parked, graph.sentinel.slot(), graph.sentinel_record);
    assert_eq!(pending.cursor, graph.sentinel_record);
    assert!(parked.marked(graph.left));
    assert_still_white(parked, &white);
}

fn see_string_value(heap: &LuaHeap, owner: TableRef, value: TableRef) -> bool {
    let Ok(Some(found)) = heap.named_edge(owner, "value") else {
        return false;
    };
    if found != value {
        return false;
    }
    let Ok(carrier) = heap.issued_value(value) else {
        return false;
    };
    let mut cursor = Value::Null;
    let mut saw = false;
    while let Ok(Some((key, next_value))) = heap.next_guest(owner, &cursor) {
        if key == Value::string("value") && next_value == carrier {
            saw = true;
        }
        cursor = key;
    }
    saw
}

fn finish_kv_cycle(
    host: &mut vm::HostContext<'_>,
    graph: &KvGraph,
    keep_key: bool,
    keep_value: bool,
) {
    let done = finish_fixed_cycle(host);
    assert!(
        matches!(done, crate::heap::StepOutcome::Completed(_)),
        "{done:?}"
    );
    heap::with_heap(host, |heap| -> Result<(), HeapError> {
        assert!(!heap.alive(graph.right_key), "the sibling key clears");
        assert!(!heap.alive(graph.right_value), "the sibling value clears");
        assert!(heap.next_guest(graph.right, &Value::Null)?.is_none());
        assert!(!heap.alive(graph.orphan));
        assert_eq!(heap.named_edge(graph.witness, "orphan")?, None);
        assert!(!heap.alive(graph.dust));
        if keep_key {
            assert!(heap.alive(graph.left_key));
            assert!(heap.alive(graph.key_succ));
            assert!(see_object_scalar(
                heap,
                graph.left,
                graph.left_key,
                LEFT_SCALAR
            ));
            assert_eq!(
                heap.named_edge(graph.left_key, "next")?,
                Some(graph.key_succ)
            );
            assert_eq!(heap.named_edge(graph.witness, "key")?, Some(graph.left_key));
            assert_eq!(
                heap.named_edge(graph.witness, "key_succ")?,
                Some(graph.key_succ)
            );
        } else {
            assert!(!heap.alive(graph.left_key));
            assert!(!heap.alive(graph.key_succ));
            assert_eq!(heap.named_edge(graph.witness, "key")?, None);
            assert_eq!(heap.named_edge(graph.witness, "key_succ")?, None);
        }
        if keep_value {
            assert!(heap.alive(graph.value));
            assert!(heap.alive(graph.value_succ));
            assert!(see_string_value(heap, graph.left, graph.value));
            assert_eq!(
                heap.named_edge(graph.value, "next")?,
                Some(graph.value_succ)
            );
            assert_eq!(heap.named_edge(graph.witness, "val")?, Some(graph.value));
            assert_eq!(
                heap.named_edge(graph.witness, "val_succ")?,
                Some(graph.value_succ)
            );
        } else {
            assert!(!heap.alive(graph.value));
            assert!(!heap.alive(graph.value_succ));
            assert_eq!(heap.named_edge(graph.left, "value")?, None);
            assert_eq!(heap.named_edge(graph.witness, "val")?, None);
            assert_eq!(heap.named_edge(graph.witness, "val_succ")?, None);
        }
        Ok(())
    })
    .expect("this cycle");
    if keep_key || keep_value {
        heap::with_heap(host, |heap| -> Result<(), HeapError> {
            heap.set_metatable(graph.left, Some(graph.kv_mt))?;
            if keep_key {
                assert!(
                    heap.alive(graph.left_key),
                    "restoring kv does not drop the key"
                );
            }
            if keep_value {
                assert!(
                    heap.alive(graph.value),
                    "restoring kv does not drop the value"
                );
            }
            Ok(())
        })
        .expect("restore kv");
        heap::collect(host).expect("next cycle");
        heap::with_heap(host, |heap| {
            assert!(!heap.alive(graph.left_key));
            assert!(!heap.alive(graph.key_succ));
            assert!(!heap.alive(graph.value));
            assert!(!heap.alive(graph.value_succ));
            Ok(())
        })
        .expect("released");
    }
}

#[derive(Clone, Copy)]
enum KvArm {
    ToValue,
    ToKey,
    ToStrong,
    Omit,
}

fn run_kv_arm(arm: KvArm) {
    let keep_key = matches!(arm, KvArm::ToValue | KvArm::ToStrong);
    let keep_value = matches!(arm, KvArm::ToKey | KvArm::ToStrong);
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = open_kv_graph(&mut host);
        resume_weak(&mut host, |parked| {
            prove_both_records(parked, &graph);
            let pending = parked.pending();
            let before = parked.meter();
            let roots = parked.stats().live_roots;
            let replacement = match arm {
                KvArm::ToValue => Some(graph.value_mt),
                KvArm::ToKey => Some(graph.key_mt),
                KvArm::ToStrong => Some(graph.strong_mt),
                KvArm::Omit => None,
            };
            if let Some(mt) = replacement {
                write_metatable(parked, graph.left, Some(mt)).expect("replace one owner metatable");
                assert_setter_reserved(parked, pending, &before, roots);
                let right_mt = parked.with_saved_heap(|heap| heap.metatable(graph.right));
                assert!(matches!(right_mt, Ok(Some(found)) if found == graph.kv_mt));
                assert_still_white(
                    parked,
                    &[
                        graph.left_key,
                        graph.key_succ,
                        graph.value,
                        graph.value_succ,
                    ],
                );
            }
            if keep_key {
                assert_subject_marked(parked, graph.left_key);
                assert_subject_marked(parked, graph.key_succ);
            }
            if keep_value {
                assert_subject_marked(parked, graph.value);
                assert_subject_marked(parked, graph.value_succ);
            }
            if replacement.is_some() && !keep_key {
                assert!(!parked.marked(graph.left_key));
                assert!(!parked.marked(graph.key_succ));
            }
            if replacement.is_some() && !keep_value {
                assert!(!parked.marked(graph.value));
                assert!(!parked.marked(graph.value_succ));
            }
        });
        finish_kv_cycle(&mut host, &graph, keep_key, keep_value);
    });
}

#[test]
fn gray_scanned_kv_to_value_keeps_only_the_object_key() {
    run_kv_arm(KvArm::ToValue);
}

#[test]
fn gray_scanned_kv_to_key_keeps_only_the_value() {
    run_kv_arm(KvArm::ToKey);
}

#[test]
fn gray_scanned_kv_to_strong_keeps_both_edges() {
    run_kv_arm(KvArm::ToStrong);
}

#[test]
fn gray_scanned_kv_omit_clears_both_edges() {
    run_kv_arm(KvArm::Omit);
}

const GROWN_ITEM: &str = "item";
const GROWN_ROOTS: u32 = 7;
const GROWN_ROOT_SLOT: u32 = 6;

/// Pre-cycle objects for the slot-growth witness.
///
/// `left` is the first weak-value owner scanned. Its physical record is charged
/// before the new owner exists, so the rescan bitmaps are sized to the old slot
/// vector. `value` and `successor` are old, unrooted targets: only the new
/// owner will link them, after the allocation and after the setter gates.
struct SlotGrowthGraph {
    frame: RootFrameId,
    sentinel: TableRef,
    witness: TableRef,
    old_mt: TableRef,
    strong_mt: TableRef,
    left: TableRef,
    left_value: TableRef,
    left_successor: TableRef,
    peer: TableRef,
    peer_value: TableRef,
    orphan: TableRef,
    dust: TableRef,
    value: TableRef,
    successor: TableRef,
    left_record: u32,
    peer_record: u32,
    sentinel_record: u32,
}

fn plant_slot_growth_graph(heap: &mut LuaHeap) -> Result<SlotGrowthGraph, HeapError> {
    let sentinel = heap.alloc()?;
    let witness = heap.alloc()?;
    let old_mt = heap.alloc()?;
    let strong_mt = heap.alloc()?;
    let left = heap.alloc()?;
    let left_value = heap.alloc()?;
    let left_successor = heap.alloc()?;
    let peer = heap.alloc()?;
    let peer_value = heap.alloc()?;
    let value = heap.alloc()?;
    let successor = heap.alloc()?;
    let orphan = heap.alloc()?;
    let dust = heap.alloc()?;

    heap.set_mode_scalar(old_mt, Value::string("v"))?;
    heap.set_metatable(left, Some(old_mt))?;
    heap.set_metatable(peer, Some(old_mt))?;
    heap.set_metatable(witness, Some(old_mt))?;
    heap.set_named_edge(left, GROWN_ITEM, left_value)?;
    heap.set_named_edge(left_value, "next", left_successor)?;
    heap.set_named_edge(peer, GROWN_ITEM, peer_value)?;
    heap.set_named_edge(witness, "orphan", orphan)?;
    heap.set_named_edge(witness, "peer", peer_value)?;
    heap.set_named_scalar(sentinel, "pending", Value::Bool(true))?;
    heap.issue_carrier(left_value)?;
    heap.issue_carrier(value)?;
    heap.issue_carrier(successor)?;
    assert_eq!(heap.named_edge(strong_mt, GROWN_ITEM)?, None);
    assert_eq!(heap.named_edge(value, "next")?, None);

    let left_record = heap
        .find_key(left.slot(), &super::EdgeKey::named(GROWN_ITEM))
        .expect("left item record");
    let peer_record = heap
        .find_key(peer.slot(), &super::EdgeKey::named(GROWN_ITEM))
        .expect("peer item record");
    let (_, sentinel_record) = heap
        .locate_named(sentinel.slot(), "pending")
        .expect("sentinel record");
    let frame = heap.enter_roots(GROWN_ROOTS)?;
    // Root order is gray push order. `left` is popped first and the sentinel
    // stays behind it. Slot 6 remains empty for the typed root write that
    // enters the grown owner after the allocation.
    heap.write_root(frame, 0, sentinel)?;
    heap.write_root(frame, 1, witness)?;
    heap.write_root(frame, 2, strong_mt)?;
    heap.write_root(frame, 3, old_mt)?;
    heap.write_root(frame, 4, peer)?;
    heap.write_root(frame, 5, left)?;
    Ok(SlotGrowthGraph {
        frame,
        sentinel,
        witness,
        old_mt,
        strong_mt,
        left,
        left_value,
        left_successor,
        peer,
        peer_value,
        orphan,
        dust,
        value,
        successor,
        left_record,
        peer_record,
        sentinel_record,
    })
}

fn open_slot_growth_graph(host: &mut vm::HostContext<'_>) -> SlotGrowthGraph {
    open_slot_growth_graph_with(host, 16)
}

fn open_slot_growth_graph_with(host: &mut vm::HostContext<'_>, nodes: u32) -> SlotGrowthGraph {
    heap::install(host, limits(nodes, 8, 32)).expect("install");
    let graph = heap::with_heap(host, plant_slot_growth_graph).expect("plant");
    heap::open_fixed_step(host).expect("open");
    graph
}

/// Grows the live slot vector by real table allocations. The returned handles
/// stay unrooted, so these slots are unrelated filler: they must not be
/// traversed, tagged, or copied by a weak-start or setter call.
fn grow_plain_slots(parked: &mut ParkedMark<'_>, count: u32) -> Vec<TableRef> {
    let mut grown = Vec::new();
    for _ in 0..count {
        grown.push(
            parked
                .with_saved_heap(|heap| heap.alloc())
                .expect("a real allocation grows the slot vector"),
        );
    }
    grown
}

fn grown_white(graph: &SlotGrowthGraph) -> [TableRef; 6] {
    [
        graph.left_value,
        graph.left_successor,
        graph.peer_value,
        graph.orphan,
        graph.value,
        graph.successor,
    ]
}

#[derive(Clone, Copy)]
enum GrownArm {
    Swap,
    Clear,
    Omit,
}

fn run_grown_arm(arm: GrownArm) {
    let keep = !matches!(arm, GrownArm::Omit);
    let label = arm_name(arm);
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = open_slot_growth_graph(&mut host);
        let mut grown_owner = None;
        let mut grown_slot = 0u32;
        let mut old_slots = 0u32;
        let mut initial_bits = 0u64;
        let mut bits_at_scan = 0u64;
        let mut schedules = 0u64;
        let mut pushes = 0u64;
        let mut roots_after_setter = 0u32;
        resume_weak(&mut host, |parked| {
            let white = grown_white(&graph);
            charge_white(parked, graph.left.slot(), graph.left_record, &white);
            let before_alloc = parked.stats();
            old_slots = before_alloc.node_slots;
            initial_bits = parked.meter().scratch.owner_rescan_inited;
            assert_eq!(
                initial_bits, 1,
                "the first weak owner tags only its own target slot"
            );
            let watch = parked
                .with_saved_heap(|heap| heap.saved_mark_watch())
                .expect("saved mark");
            assert!(
                parked.with_saved_heap(|heap| heap.table_in_gray(graph.sentinel)),
                "the independent sentinel must still be on the gray stack"
            );
            assert!(watch.pending);
            assert_eq!(watch.cursor_owner, Some(graph.left.slot()));
            assert!(watch.gray_len >= 1);

            assert!(
                parked.with_saved_heap(|heap| heap.free_list.is_empty()),
                "a free slot would hide a real slot growth"
            );
            let records = parked.with_saved_heap(|heap| {
                (
                    heap.locate_named(graph.peer.slot(), GROWN_ITEM)
                        .map(|(_, index)| index),
                    heap.locate_named(graph.sentinel.slot(), "pending")
                        .map(|(_, index)| index),
                )
            });
            assert_eq!(
                records,
                (Some(graph.peer_record), Some(graph.sentinel_record)),
                "the pre-cycle records stay where the fixture found them"
            );
            let before_growth = parked.meter();
            let owner = parked
                .with_saved_heap(|heap| heap.alloc())
                .expect("a real alloc grows the slot vector");
            grown_slot = owner.slot();
            grown_owner = Some(owner);
            assert_eq!(
                grown_slot, old_slots,
                "the new table must be the next physical slot"
            );
            let after_alloc = parked.stats();
            assert_eq!(after_alloc.node_slots, old_slots + 1);
            assert_eq!(after_alloc.live_nodes, before_alloc.live_nodes + 1);
            assert_eq!(
                parked.meter().parked_root_writes,
                before_growth.parked_root_writes
            );

            // Mount the cached weak metatable and both edges with the ordinary
            // setters. The owner is not marked, so neither the new key nor the
            // metatable enters the gray queue.
            parked
                .with_saved_heap(|heap| -> Result<(), HeapError> {
                    heap.set_metatable(owner, Some(graph.old_mt))?;
                    heap.set_named_edge(owner, GROWN_ITEM, graph.value)?;
                    heap.set_named_edge(graph.value, "next", graph.successor)?;
                    Ok(())
                })
                .expect("mount the cached weak metatable and the weak value");
            assert!(!parked.marked(owner), "the new owner is not gray yet");
            assert_eq!(
                parked.with_saved_heap(|heap| heap.named_edge(owner, GROWN_ITEM)),
                Ok(Some(graph.value))
            );
            assert_still_white(parked, &[graph.value, graph.successor]);
            assert_eq!(
                parked.meter().owner_rescan_schedules,
                before_growth.owner_rescan_schedules
            );
            assert_eq!(
                parked.meter().parked_barrier_marks,
                before_growth.parked_barrier_marks
            );

            let record = parked
                .with_saved_heap(|heap| {
                    heap.find_key(owner.slot(), &super::EdgeKey::named(GROWN_ITEM))
                })
                .expect("grown owner item record");
            parked
                .write_table_root(graph.frame, GROWN_ROOT_SLOT, owner)
                .expect("root the grown owner into the current gray");
            assert!(parked.marked(owner), "the root write marks the new table");
            assert_eq!(
                parked.meter().parked_root_writes,
                before_growth.parked_root_writes + 1
            );
            assert_eq!(
                parked.meter().parked_barrier_marks,
                before_growth.parked_barrier_marks + 1
            );
            assert_still_white(parked, &[graph.value, graph.successor]);

            // The grown owner is popped before the still-queued sentinel. Its
            // weak value record is charged while the original value and the
            // strong successor remain white.
            charge_white(
                parked,
                owner.slot(),
                record,
                &[graph.value, graph.successor],
            );
            assert!(parked.marked(owner));
            assert_eq!(parked.stats().node_slots, old_slots + 1);
            bits_at_scan = parked.meter().scratch.owner_rescan_inited;

            let pending = parked.pending();
            let before_setter = parked.meter();
            let roots = parked.stats().live_roots;
            match arm {
                GrownArm::Swap => {
                    write_metatable(parked, owner, Some(graph.strong_mt))
                        .expect("replace the weak metatable with a prebuilt strong one");
                }
                GrownArm::Clear => {
                    write_metatable(parked, owner, None).expect("clear the weak metatable");
                }
                GrownArm::Omit => {}
            }
            let current_mt = parked
                .with_saved_heap(|heap| heap.metatable(owner))
                .expect("read the owner metatable");
            match arm {
                GrownArm::Swap => assert_eq!(current_mt, Some(graph.strong_mt)),
                GrownArm::Clear => assert_eq!(current_mt, None),
                GrownArm::Omit => assert_eq!(current_mt, Some(graph.old_mt)),
            }
            if keep {
                assert_setter_reserved(parked, pending, &before_setter, roots);
                assert_still_white(parked, &[graph.value, graph.successor]);
                schedules =
                    parked.meter().owner_rescan_schedules - before_setter.owner_rescan_schedules;
                pushes = parked.meter().scratch.owner_rescan_pushed
                    - before_setter.scratch.owner_rescan_pushed;
                roots_after_setter = parked.stats().live_roots;
            } else {
                assert_eq!(
                    parked.meter().owner_rescan_schedules,
                    before_setter.owner_rescan_schedules
                );
                roots_after_setter = parked.stats().live_roots;
            }
        });
        let done = finish_fixed_cycle(&mut host);
        assert!(
            matches!(done, crate::heap::StepOutcome::Completed(_)),
            "{done:?}"
        );
        let owner = grown_owner.expect("the grown owner");
        let meter = heap::take_scan_meter(&mut host).expect("meter");
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            assert!(heap.alive(owner), "the rooted grown owner survives");
            assert!(heap.alive(graph.left));
            assert!(heap.alive(graph.peer));
            assert!(heap.alive(graph.witness));
            assert!(heap.alive(graph.old_mt));
            assert!(heap.alive(graph.strong_mt));
            assert!(
                !heap.alive(graph.orphan),
                "the independent weak orphan clears"
            );
            assert!(
                !heap.alive(graph.peer_value),
                "the continuing weak peer value clears"
            );
            assert!(
                !heap.alive(graph.left_value),
                "the initializer weak value clears"
            );
            assert!(!heap.alive(graph.left_successor));
            assert!(!heap.alive(graph.dust), "an unrooted node is released");
            assert_eq!(heap.named_edge(graph.witness, "orphan")?, None);
            assert_eq!(heap.named_edge(graph.witness, "peer")?, None);
            assert_eq!(heap.named_edge(graph.left, GROWN_ITEM)?, None);
            assert_eq!(heap.named_edge(graph.peer, GROWN_ITEM)?, None);
            assert!(heap.next_guest(graph.witness, &Value::Null)?.is_none());
            assert!(heap.next_guest(graph.left, &Value::Null)?.is_none());
            assert!(heap.next_guest(graph.peer, &Value::Null)?.is_none());
            if keep {
                assert!(
                    heap.alive(graph.value),
                    "`{label}` kept weak value was swept (old_slots={old_slots}, initial_bits={initial_bits}, bits_at_scan={bits_at_scan}, schedules={schedules}, pushes={pushes})"
                );
                assert!(
                    heap.alive(graph.successor),
                    "`{label}` strong successor was swept (old_slots={old_slots}, initial_bits={initial_bits}, bits_at_scan={bits_at_scan}, schedules={schedules})"
                );
                assert_eq!(heap.named_edge(owner, GROWN_ITEM)?, Some(graph.value));
                assert_eq!(
                    heap.named_edge(graph.value, "next")?,
                    Some(graph.successor)
                );
                let carrier = heap.issued_value(graph.value)?;
                let mut cursor = Value::Null;
                let mut saw = false;
                while let Some((key, next_value)) = heap.next_guest(owner, &cursor)? {
                    if key == Value::string(GROWN_ITEM) && next_value == carrier {
                        saw = true;
                    }
                    cursor = key;
                }
                assert!(saw, "iteration returns the original value carrier");
            } else {
                assert!(
                    !heap.alive(graph.value),
                    "keeping v must clear the old value"
                );
                assert!(!heap.alive(graph.successor));
                assert_eq!(heap.named_edge(owner, GROWN_ITEM)?, None);
                assert!(heap.next_guest(owner, &Value::Null)?.is_none());
            }
            Ok(())
        })
        .expect("this cycle");

        if keep {
            assert_eq!(initial_bits, 1);
            assert_eq!(
                bits_at_scan,
                initial_bits + 1,
                "the new slot's weak start tags exactly one more slot"
            );
            assert_eq!(schedules, 1, "exactly one owner rescan is requested");
            assert_eq!(pushes, 1, "one queue entry, no duplicate");
            assert_eq!(meter.owner_rescan_heads, 1);
            assert_eq!(meter.scratch.owner_rescan_pushed, 1);
            assert_eq!(roots_after_setter, GROWN_ROOTS);
        } else {
            assert_eq!(schedules, 0);
            assert_eq!(pushes, 0);
            assert_eq!(meter.owner_rescan_heads, 0);
            assert_eq!(meter.scratch.owner_rescan_pushed, 0);
        }

        if keep {
            heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
                heap.set_metatable(owner, Some(graph.old_mt))?;
                Ok(())
            })
            .expect("restore v");
            heap::collect(&mut host).expect("next cycle");
            heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
                assert!(
                    !heap.alive(graph.value),
                    "the next cycle releases the restored weak value"
                );
                assert!(
                    !heap.alive(graph.successor),
                    "the next cycle releases its successor"
                );
                assert_eq!(heap.named_edge(owner, GROWN_ITEM)?, None);
                assert!(heap.alive(owner));
                Ok(())
            })
            .expect("released");
        } else {
            heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
                heap.set_metatable(owner, Some(graph.strong_mt))?;
                Ok(())
            })
            .expect("late strong metatable");
            heap::collect(&mut host).expect("collect after the weak clear");
            heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
                assert!(!heap.alive(graph.value));
                assert!(!heap.alive(graph.successor));
                Ok(())
            })
            .expect("no resurrection");
        }
    });
}

fn arm_name(arm: GrownArm) -> &'static str {
    match arm {
        GrownArm::Swap => "swap",
        GrownArm::Clear => "clear",
        GrownArm::Omit => "omit",
    }
}

#[test]
fn gray_grown_slot_weak_owner_swap_keeps_value_and_successor() {
    run_grown_arm(GrownArm::Swap);
}

#[test]
fn gray_grown_slot_weak_owner_clear_keeps_value_and_successor() {
    run_grown_arm(GrownArm::Clear);
}

#[test]
fn gray_grown_slot_weak_owner_omit_clears_value_and_successor() {
    run_grown_arm(GrownArm::Omit);
}

/// Matched pre-cycle allocation control for the grown-slot arm. The owner is
/// inside the first bitmap and must take the existing rescan path on the base
/// implementation.
#[test]
fn gray_precycle_weak_owner_swap_keeps_value_and_successor() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = open_slot_growth_graph(&mut host);
        let mut initial_bits = 0u64;
        let mut schedules = 0u64;
        let mut slots = 0u32;
        resume_weak(&mut host, |parked| {
            let white = [
                graph.left_value,
                graph.left_successor,
                graph.peer_value,
                graph.orphan,
            ];
            charge_white(parked, graph.left.slot(), graph.left_record, &white);
            slots = parked.stats().node_slots;
            initial_bits = parked.meter().scratch.owner_rescan_inited;
            assert_eq!(
                initial_bits, 1,
                "the pre-cycle owner tags only its own slot"
            );
            let pending = parked.pending();
            let before = parked.meter();
            let roots = parked.stats().live_roots;
            write_metatable(parked, graph.left, Some(graph.strong_mt))
                .expect("strengthen the pre-cycle owner");
            assert_setter_reserved(parked, pending, &before, roots);
            assert_still_white(parked, &[graph.left_value, graph.left_successor]);
            schedules = parked.meter().owner_rescan_schedules - before.owner_rescan_schedules;
            assert_subject_marked(parked, graph.left_value);
            assert_subject_marked(parked, graph.left_successor);
        });
        let done = finish_fixed_cycle(&mut host);
        assert!(
            matches!(done, crate::heap::StepOutcome::Completed(_)),
            "{done:?}"
        );
        let meter = heap::take_scan_meter(&mut host).expect("meter");
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            assert!(heap.alive(graph.left));
            assert!(
                heap.alive(graph.left_value),
                "the matched pre-cycle value stays"
            );
            assert!(heap.alive(graph.left_successor));
            assert_eq!(
                heap.named_edge(graph.left, GROWN_ITEM)?,
                Some(graph.left_value)
            );
            assert_eq!(
                heap.named_edge(graph.left_value, "next")?,
                Some(graph.left_successor)
            );
            let carrier = heap.issued_value(graph.left_value)?;
            let mut cursor = Value::Null;
            let mut saw = false;
            while let Some((key, value)) = heap.next_guest(graph.left, &cursor)? {
                if key == Value::string(GROWN_ITEM) && value == carrier {
                    saw = true;
                }
                cursor = key;
            }
            assert!(saw, "iteration returns the pre-cycle value carrier");
            assert!(!heap.alive(graph.peer_value));
            assert!(!heap.alive(graph.orphan));
            assert!(!heap.alive(graph.value), "an unlinked old target clears");
            assert!(!heap.alive(graph.successor));
            assert!(!heap.alive(graph.dust));
            assert_eq!(heap.named_edge(graph.peer, GROWN_ITEM)?, None);
            assert_eq!(heap.named_edge(graph.witness, "orphan")?, None);
            Ok(())
        })
        .expect("pre-cycle cycle");
        assert_eq!(schedules, 1);
        assert_eq!(meter.owner_rescan_heads, 1);
        assert_eq!(meter.scratch.owner_rescan_pushed, 1);
        assert_eq!(initial_bits, 1);
        let slots_after = heap::with_heap(&mut host, |heap| -> Result<u32, HeapError> {
            Ok(heap.stats().node_slots)
        })
        .expect("slots");
        assert_eq!(slots_after, slots, "the control does not grow the slots");

        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            heap.set_metatable(graph.left, Some(graph.old_mt))?;
            Ok(())
        })
        .expect("restore v");
        heap::collect(&mut host).expect("next cycle");
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            assert!(!heap.alive(graph.left_value));
            assert!(!heap.alive(graph.left_successor));
            assert!(heap.alive(graph.left));
            Ok(())
        })
        .expect("released");
    });
}

/// A: stop immediately before a new weak owner opens, then spend exactly one
/// pulse unit on that head. The pulse may initialize state for that one target
/// slot; it must not initialize the whole grown suffix. `delta` is the number
/// of unrelated real slot allocations made after the first weak owner started.
fn run_weak_start_budget(delta: u32) {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = open_slot_growth_graph_with(&mut host, 16 + delta);
        let mut grown_owner = None;
        let mut owner_slot = u32::MAX;
        let mut old_slots = 0;
        let mut before = ScanMeter::default();
        let mut after = ScanMeter::default();
        let mut inited_delta = 0u64;
        let mut roots_before = 0;
        resume_weak(&mut host, |parked| {
            let white = grown_white(&graph);
            charge_white(parked, graph.left.slot(), graph.left_record, &white);
            old_slots = parked.stats().node_slots;
            assert!(
                parked.with_saved_heap(|heap| heap.table_in_gray(graph.sentinel)),
                "the independent sentinel must still be on the gray stack"
            );
            assert!(
                parked.with_saved_heap(|heap| heap.free_list.is_empty()),
                "a free slot would hide a real slot growth"
            );

            let fillers = grow_plain_slots(parked, delta);
            assert_eq!(
                u32::try_from(fillers.len()).expect("delta fits u32"),
                delta,
                "every filler is a real allocation"
            );
            let grown = parked
                .with_saved_heap(|heap| heap.alloc())
                .expect("the new weak owner is a real allocation");
            owner_slot = grown.slot();
            grown_owner = Some(grown);
            assert_eq!(
                owner_slot,
                old_slots + delta,
                "the owner is the last grown slot"
            );
            assert_eq!(parked.stats().node_slots, old_slots + delta + 1);

            parked
                .with_saved_heap(|heap| -> Result<(), HeapError> {
                    heap.set_metatable(grown, Some(graph.old_mt))?;
                    heap.set_named_edge(grown, GROWN_ITEM, graph.value)?;
                    heap.set_named_edge(graph.value, "next", graph.successor)?;
                    Ok(())
                })
                .expect("mount the cached weak metatable and weak value");
            assert!(!parked.marked(grown), "the new owner is not scanned yet");
            assert_still_white(parked, &[graph.value, graph.successor]);
            let record = parked
                .with_saved_heap(|heap| {
                    heap.find_key(grown.slot(), &super::EdgeKey::named(GROWN_ITEM))
                })
                .expect("grown owner item record");

            parked
                .write_table_root(graph.frame, GROWN_ROOT_SLOT, grown)
                .expect("root the grown owner into the current gray");
            assert!(parked.marked(grown));
            assert!(
                parked.with_saved_heap(|heap| heap.table_in_gray(grown)),
                "the grown owner is the last gray entry pushed"
            );
            assert!(
                parked.pending().is_none(),
                "the first weak owner's last record is already charged"
            );

            roots_before = parked.stats().live_roots;
            before = parked.meter();
            let view = parked.pulse(1);
            after = parked.meter();
            inited_delta = after
                .scratch
                .owner_rescan_inited
                .saturating_sub(before.scratch.owner_rescan_inited);
            assert!(!view.finished, "one head unit must not finish the mark");
            assert_eq!(
                after.owner_head_checks,
                before.owner_head_checks + 1,
                "opening the new weak owner must spend exactly one head unit"
            );
            assert_eq!(
                after.outgoing_edge_checks, before.outgoing_edge_checks,
                "a head unit must not charge an edge"
            );
            assert_eq!(after.outgoing_gap_checks, before.outgoing_gap_checks);
            assert_eq!(after.root_slot_visits, before.root_slot_visits);
            assert_eq!(parked.stats().live_roots, roots_before);
            let pending = parked
                .pending()
                .expect("the new weak owner must be the owner just opened");
            assert_eq!(pending.owner, owner_slot);
            assert_eq!(pending.cursor, record);
            assert!(
                parked.with_saved_heap(|heap| heap.table_in_gray(graph.sentinel)),
                "the independent sentinel is still queued after the pulse"
            );
            assert_still_white(parked, &[graph.value, graph.successor]);
            assert!(
                inited_delta <= 1,
                "one weak-start pulse initialized {inited_delta} logical slots; \
                 delta={delta} old_slots={old_slots} owner_slot={owner_slot} \
                 before={before:?} after={after:?}"
            );

            let setter_before = parked.meter();
            write_metatable(parked, grown, Some(graph.strong_mt))
                .expect("strengthen the grown owner on its pending item record");
            assert_setter_reserved(parked, Some(pending), &setter_before, roots_before);
            assert_still_white(parked, &[graph.value, graph.successor]);
            assert_eq!(
                parked.meter().owner_rescan_schedules,
                setter_before.owner_rescan_schedules + 1
            );
            assert_eq!(
                parked.meter().scratch.owner_rescan_pushed,
                setter_before.scratch.owner_rescan_pushed + 1
            );
        });

        let done = finish_fixed_cycle(&mut host);
        assert!(
            matches!(done, crate::heap::StepOutcome::Completed(_)),
            "{done:?}"
        );
        let owner = grown_owner.expect("the grown owner");
        let meter = heap::take_scan_meter(&mut host).expect("meter");
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            assert!(heap.alive(owner), "the rooted grown owner survives");
            assert!(heap.alive(graph.sentinel));
            assert!(heap.alive(graph.left));
            assert!(heap.alive(graph.peer));
            assert!(heap.alive(graph.witness));
            assert!(heap.alive(graph.old_mt));
            assert!(heap.alive(graph.strong_mt));
            assert!(
                heap.alive(graph.value),
                "the strengthened grown value survives (delta={delta})"
            );
            assert!(
                heap.alive(graph.successor),
                "its strong successor survives (delta={delta})"
            );
            assert_eq!(heap.named_edge(owner, GROWN_ITEM)?, Some(graph.value));
            assert_eq!(heap.named_edge(graph.value, "next")?, Some(graph.successor));
            let carrier = heap.issued_value(graph.value)?;
            let mut cursor = Value::Null;
            let mut saw = false;
            while let Some((key, next_value)) = heap.next_guest(owner, &cursor)? {
                if key == Value::string(GROWN_ITEM) && next_value == carrier {
                    saw = true;
                }
                cursor = key;
            }
            assert!(saw, "iteration returns the original grown value carrier");
            assert!(!heap.alive(graph.left_value));
            assert!(!heap.alive(graph.left_successor));
            assert!(!heap.alive(graph.peer_value), "the weak peer value clears");
            assert!(!heap.alive(graph.orphan), "the weak orphan clears");
            assert!(!heap.alive(graph.dust));
            assert_eq!(heap.named_edge(graph.peer, GROWN_ITEM)?, None);
            assert_eq!(heap.named_edge(graph.witness, "orphan")?, None);
            Ok(())
        })
        .expect("this cycle");
        assert_eq!(meter.owner_rescan_heads, 1, "delta={delta}");
        assert_eq!(meter.scratch.owner_rescan_pushed, 1, "delta={delta}");

        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            heap.set_metatable(owner, Some(graph.old_mt))?;
            Ok(())
        })
        .expect("restore v");
        heap::collect(&mut host).expect("next cycle");
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            assert!(
                !heap.alive(graph.value),
                "the restored weak value is released (delta={delta})"
            );
            assert!(
                !heap.alive(graph.successor),
                "the restored successor is released (delta={delta})"
            );
            assert_eq!(heap.named_edge(owner, GROWN_ITEM)?, None);
            assert!(heap.alive(owner));
            Ok(())
        })
        .expect("released");
    });
}

/// B: the old owner already started a weak-value scan. Every grown slot is
/// still unscanned. A real metatable swap must only register that old owner:
/// no pulse, no traversal, no initialization of the unrelated suffix, and no
/// immediate recursion along the old weak value.
fn run_old_owner_setter_budget(delta: u32) {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = open_slot_growth_graph_with(&mut host, 16 + delta);
        let mut old_slots = 0;
        let mut before = ScanMeter::default();
        let mut after = ScanMeter::default();
        let mut inited_delta = 0u64;
        let mut schedules = 0u64;
        let mut pushes = 0u64;
        resume_weak(&mut host, |parked| {
            let white = grown_white(&graph);
            charge_white(parked, graph.left.slot(), graph.left_record, &white);
            old_slots = parked.stats().node_slots;
            let fillers = grow_plain_slots(parked, delta);
            assert_eq!(u32::try_from(fillers.len()).expect("delta fits u32"), delta);
            assert_eq!(parked.stats().node_slots, old_slots + delta);
            assert!(
                parked.with_saved_heap(|heap| heap.table_in_gray(graph.sentinel)),
                "the independent sentinel must still be on the gray stack"
            );
            assert!(
                parked.pending().is_none(),
                "the old owner's ordinary cursor is exhausted before the setter"
            );
            let pending_before = parked.pending();
            let roots_before = parked.stats().live_roots;
            before = parked.meter();
            write_metatable(parked, graph.left, Some(graph.strong_mt))
                .expect("strengthen the already scanned old owner");
            after = parked.meter();
            schedules = after
                .owner_rescan_schedules
                .saturating_sub(before.owner_rescan_schedules);
            pushes = after
                .scratch
                .owner_rescan_pushed
                .saturating_sub(before.scratch.owner_rescan_pushed);
            inited_delta = after
                .scratch
                .owner_rescan_inited
                .saturating_sub(before.scratch.owner_rescan_inited);
            assert_setter_reserved(parked, pending_before, &before, roots_before);
            assert_eq!(schedules, 1, "one request for the old owner");
            assert_eq!(pushes, 1, "one queue entry, no duplicate");
            assert!(
                parked.pending_rescan().is_none(),
                "the setter must not open the rescan inside its own call"
            );
            assert_still_white(
                parked,
                &[
                    graph.left_value,
                    graph.left_successor,
                    graph.value,
                    graph.successor,
                ],
            );
            assert!(
                parked.with_saved_heap(|heap| heap.table_in_gray(graph.sentinel)),
                "the sentinel is still queued after the setter"
            );
            assert!(
                inited_delta == 0,
                "setter initialized {inited_delta} unrelated rescan slots; \
                 delta={delta} old_slots={old_slots} before={before:?} after={after:?}"
            );
        });

        let done = finish_fixed_cycle(&mut host);
        assert!(
            matches!(done, crate::heap::StepOutcome::Completed(_)),
            "{done:?}"
        );
        let meter = heap::take_scan_meter(&mut host).expect("meter");
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            assert!(heap.alive(graph.sentinel));
            assert!(heap.alive(graph.left));
            assert!(
                heap.alive(graph.left_value),
                "the rescanned old owner keeps its value (delta={delta})"
            );
            assert!(
                heap.alive(graph.left_successor),
                "and its strong successor (delta={delta})"
            );
            assert_eq!(
                heap.named_edge(graph.left, GROWN_ITEM)?,
                Some(graph.left_value)
            );
            assert_eq!(
                heap.named_edge(graph.left_value, "next")?,
                Some(graph.left_successor)
            );
            let carrier = heap.issued_value(graph.left_value)?;
            let mut cursor = Value::Null;
            let mut saw = false;
            while let Some((key, value)) = heap.next_guest(graph.left, &cursor)? {
                if key == Value::string(GROWN_ITEM) && value == carrier {
                    saw = true;
                }
                cursor = key;
            }
            assert!(saw, "iteration returns the original value carrier");
            assert!(!heap.alive(graph.peer_value), "the weak peer value clears");
            assert!(!heap.alive(graph.orphan), "the weak orphan clears");
            assert!(!heap.alive(graph.value));
            assert!(!heap.alive(graph.successor));
            assert!(!heap.alive(graph.dust));
            assert_eq!(heap.named_edge(graph.peer, GROWN_ITEM)?, None);
            assert_eq!(heap.named_edge(graph.witness, "orphan")?, None);
            Ok(())
        })
        .expect("this cycle");
        assert_eq!(meter.owner_rescan_heads, 1, "delta={delta}");
        assert_eq!(meter.scratch.owner_rescan_pushed, 1, "delta={delta}");

        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            heap.set_metatable(graph.left, Some(graph.old_mt))?;
            Ok(())
        })
        .expect("restore v");
        heap::collect(&mut host).expect("next cycle");
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            assert!(
                !heap.alive(graph.left_value),
                "the restored weak value is released (delta={delta})"
            );
            assert!(
                !heap.alive(graph.left_successor),
                "the restored successor is released (delta={delta})"
            );
            assert!(heap.alive(graph.left));
            Ok(())
        })
        .expect("released");
    });
}

#[test]
fn budget_gray_multiple_grown_slots_weak_start_spends_one_head_unit() {
    run_weak_start_budget(4);
}

#[test]
fn budget_gray_no_filler_slot_weak_start_spends_one_head_unit() {
    run_weak_start_budget(0);
}

/// The grown suffix is far past the 13-slot vectors the old path sized once.
/// The fixed path keeps no slot-indexed rescan vector, so there is no capacity
/// boundary whose copy could hide inside this pulse; this arm exercises the
/// long suffix without claiming a reallocation measurement.
#[test]
fn budget_gray_large_grown_suffix_weak_start_spends_one_head_unit() {
    run_weak_start_budget(57);
}

#[test]
fn budget_gray_multiple_unscanned_slots_setter_registers_only_owner() {
    run_old_owner_setter_budget(5);
}

#[test]
fn budget_gray_no_growth_setter_registers_only_owner() {
    run_old_owner_setter_budget(0);
}

const QUEUE_ITEM: &str = "held";
const QUEUE_ROOTS: u32 = 4;

struct QueueGraph {
    frame: RootFrameId,
    weak_mt: TableRef,
    strong_mt: TableRef,
    a: TableRef,
    b: TableRef,
    c: TableRef,
    va: TableRef,
    vb: TableRef,
    vc: TableRef,
    dust: TableRef,
    a_record: u32,
    b_record: u32,
    c_record: u32,
}

fn plant_queue_graph(heap: &mut LuaHeap) -> Result<QueueGraph, HeapError> {
    let weak_mt = heap.alloc()?;
    let strong_mt = heap.alloc()?;
    let a = heap.alloc()?;
    let b = heap.alloc()?;
    let c = heap.alloc()?;
    let va = heap.alloc()?;
    let vb = heap.alloc()?;
    let vc = heap.alloc()?;
    let dust = heap.alloc()?;
    heap.set_mode_scalar(weak_mt, Value::string("v"))?;
    for (owner, value) in [(a, va), (b, vb), (c, vc)] {
        heap.set_metatable(owner, Some(weak_mt))?;
        heap.set_named_edge(owner, QUEUE_ITEM, value)?;
    }
    let a_record = heap
        .find_key(a.slot(), &super::EdgeKey::named(QUEUE_ITEM))
        .expect("A record");
    let b_record = heap
        .find_key(b.slot(), &super::EdgeKey::named(QUEUE_ITEM))
        .expect("B record");
    let c_record = heap
        .find_key(c.slot(), &super::EdgeKey::named(QUEUE_ITEM))
        .expect("C record");
    let frame = heap.enter_roots(QUEUE_ROOTS)?;
    // Write order is gray push order; C is popped first. The charge order below
    // therefore starts at A and ends at C, leaving each pending record visible.
    heap.write_root(frame, 0, c)?;
    heap.write_root(frame, 1, b)?;
    heap.write_root(frame, 2, a)?;
    Ok(QueueGraph {
        frame,
        weak_mt,
        strong_mt,
        a,
        b,
        c,
        va,
        vb,
        vc,
        dust,
        a_record,
        b_record,
        c_record,
    })
}

#[derive(Clone, Copy)]
enum QueueArm {
    Head,
    Middle,
    Tail,
}

fn run_queue_release(arm: QueueArm) {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(16, 8, 16)).expect("install");
        let graph = heap::with_heap(&mut host, plant_queue_graph).expect("plant");
        heap::open_fixed_step(&mut host).expect("open");
        let (removed, root_slot, kept, dead_value) = match arm {
            QueueArm::Head => (
                graph.c,
                0u32,
                (graph.a, graph.b, graph.va, graph.vb),
                graph.vc,
            ),
            QueueArm::Middle => (graph.b, 1, (graph.a, graph.c, graph.va, graph.vc), graph.vb),
            QueueArm::Tail => (graph.a, 2, (graph.b, graph.c, graph.vb, graph.vc), graph.va),
        };
        let (kept_left, kept_right, kept_left_value, kept_right_value) = kept;
        let mut revived = None;
        let mut revived_value = None;
        resume_weak(&mut host, |parked| {
            charge_white(
                parked,
                graph.a.slot(),
                graph.a_record,
                &[graph.va, graph.vb, graph.vc],
            );
            charge_white(
                parked,
                graph.b.slot(),
                graph.b_record,
                &[graph.va, graph.vb, graph.vc],
            );
            charge_white(
                parked,
                graph.c.slot(),
                graph.c_record,
                &[graph.va, graph.vb, graph.vc],
            );
            for owner in [graph.a, graph.b, graph.c] {
                write_metatable(parked, owner, Some(graph.strong_mt))
                    .expect("queue the already scanned owner");
            }
            assert_eq!(
                parked.meter().scratch.owner_rescan_pushed,
                3,
                "three weak owners are linked before the release"
            );

            // The removed owner is the middle, head, or tail of [C, B, A].
            let released_slot = removed.slot();
            let released_generation = removed.generation();
            parked
                .with_saved_heap(|heap| heap.rollback(removed))
                .expect("rollback the queued owner");
            assert!(
                !parked.with_saved_heap(|heap| heap.alive(removed)),
                "the queued owner was released"
            );
            assert!(
                parked.with_saved_heap(|heap| !heap.free_list.is_empty()),
                "the released slot is reusable"
            );

            // Reuse the same physical slot for a new generation. Mounting the
            // weak metatable must not look like an inherited weak start, and
            // strengthening it after its own scan must enqueue it once.
            let fresh = parked
                .with_saved_heap(|heap| heap.alloc())
                .expect("reuse the released slot");
            assert_eq!(
                fresh.slot(),
                released_slot,
                "the free slot is reused in place"
            );
            assert_ne!(
                fresh.generation(),
                released_generation,
                "the reused slot has a new generation"
            );
            let value = parked
                .with_saved_heap(|heap| heap.alloc())
                .expect("a fresh weak value");
            let schedules_before = parked.meter().owner_rescan_schedules;
            let pushes_before = parked.meter().scratch.owner_rescan_pushed;
            parked
                .with_saved_heap(|heap| -> Result<(), HeapError> {
                    heap.set_metatable(fresh, Some(graph.weak_mt))?;
                    heap.set_named_edge(fresh, QUEUE_ITEM, value)?;
                    Ok(())
                })
                .expect("mount the fresh weak owner");
            assert!(!parked.marked(fresh), "the fresh owner is not scanned yet");
            assert_eq!(
                parked.meter().owner_rescan_schedules,
                schedules_before,
                "the fresh unmarked owner inherits no weak-start tag"
            );
            assert_eq!(
                parked.meter().scratch.owner_rescan_pushed,
                pushes_before,
                "the fresh unmarked owner inherits no queue membership"
            );
            let value_record = parked
                .with_saved_heap(|heap| {
                    heap.find_key(fresh.slot(), &super::EdgeKey::named(QUEUE_ITEM))
                })
                .expect("fresh owner record");
            parked
                .write_table_root(graph.frame, root_slot, fresh)
                .expect("root the reused owner");
            assert!(parked.marked(fresh));
            charge_white(parked, fresh.slot(), value_record, &[value]);
            let strengthen_before = parked.meter();
            write_metatable(parked, fresh, Some(graph.strong_mt))
                .expect("strengthen the reused owner");
            assert_eq!(
                parked.meter().owner_rescan_schedules,
                strengthen_before.owner_rescan_schedules + 1,
                "the reused owner gets its own queue entry"
            );
            assert_eq!(
                parked.meter().scratch.owner_rescan_pushed,
                strengthen_before.scratch.owner_rescan_pushed + 1
            );
            assert!(
                !parked.marked(value),
                "the setter does not recurse along the old value"
            );
            revived = Some(fresh);
            revived_value = Some(value);
        });

        let done = finish_fixed_cycle(&mut host);
        assert!(
            matches!(done, crate::heap::StepOutcome::Completed(_)),
            "{done:?}"
        );
        let meter = heap::take_scan_meter(&mut host).expect("meter");
        let revived = revived.expect("the reused owner");
        let revived_value = revived_value.expect("the reused value");
        assert_eq!(
            meter.owner_rescan_heads, 3,
            "the three surviving queue entries are all rescanned"
        );
        assert_eq!(
            meter.scratch.owner_rescan_pushed, 4,
            "the released entry is gone; the reused owner is a new push"
        );
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            assert!(heap.alive(graph.weak_mt));
            assert!(heap.alive(graph.strong_mt));
            assert!(heap.alive(kept_left));
            assert!(heap.alive(kept_right));
            assert!(heap.alive(revived));
            assert_eq!(
                heap.named_edge(kept_left, QUEUE_ITEM)?,
                Some(kept_left_value)
            );
            assert_eq!(
                heap.named_edge(kept_right, QUEUE_ITEM)?,
                Some(kept_right_value)
            );
            assert_eq!(heap.named_edge(revived, QUEUE_ITEM)?, Some(revived_value));
            assert!(
                !heap.alive(dead_value),
                "the released owner's value is swept"
            );
            assert!(!heap.alive(graph.dust));
            Ok(())
        })
        .expect("this cycle");

        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            heap.set_metatable(kept_left, Some(graph.weak_mt))?;
            heap.set_metatable(kept_right, Some(graph.weak_mt))?;
            heap.set_metatable(revived, Some(graph.weak_mt))?;
            Ok(())
        })
        .expect("restore v");
        heap::collect(&mut host).expect("next cycle");
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            assert!(!heap.alive(kept_left_value));
            assert!(!heap.alive(kept_right_value));
            assert!(!heap.alive(revived_value));
            assert!(heap.alive(kept_left));
            assert!(heap.alive(kept_right));
            assert!(heap.alive(revived));
            Ok(())
        })
        .expect("released");
    });
}

#[test]
fn queue_release_head_keeps_the_suffix_and_reuse_is_clean() {
    run_queue_release(QueueArm::Head);
}

#[test]
fn queue_release_middle_keeps_both_sides_and_reuse_is_clean() {
    run_queue_release(QueueArm::Middle);
}

#[test]
fn queue_release_tail_keeps_the_prefix_and_reuse_is_clean() {
    run_queue_release(QueueArm::Tail);
}

/// Two owners share one direct `v` metatable. Each value and its strong child
/// is reachable only through that owner's edge and an independent weak witness.
/// The witness has its own weak metatable, so deleting the shared field cannot
/// keep the orphan. `strong_mt` is allocated before the cycle and is not attached.
struct SharedModeGraph {
    sentinel: TableRef,
    left: TableRef,
    right: TableRef,
    shared: TableRef,
    witness: TableRef,
    witness_mt: TableRef,
    strong_mt: TableRef,
    left_value: TableRef,
    left_child: TableRef,
    right_value: TableRef,
    right_child: TableRef,
    orphan: TableRef,
    dust: TableRef,
    sentinel_record: u32,
    left_value_record: u32,
    right_value_record: u32,
}

fn plant_shared_mode_graph(heap: &mut LuaHeap) -> Result<SharedModeGraph, HeapError> {
    let sentinel = heap.alloc()?;
    let left = heap.alloc()?;
    let right = heap.alloc()?;
    let shared = heap.alloc()?;
    let witness = heap.alloc()?;
    let witness_mt = heap.alloc()?;
    let strong_mt = heap.alloc()?;
    let left_value = heap.alloc()?;
    let left_child = heap.alloc()?;
    let right_value = heap.alloc()?;
    let right_child = heap.alloc()?;
    let orphan = heap.alloc()?;
    let dust = heap.alloc()?;
    heap.set_mode_scalar(shared, Value::string("v"))?;
    heap.set_mode_scalar(witness_mt, Value::string("v"))?;
    heap.set_metatable(left, Some(shared))?;
    heap.set_metatable(right, Some(shared))?;
    heap.set_metatable(witness, Some(witness_mt))?;
    heap.set_named_scalar(sentinel, "pending", Value::Bool(true))?;
    heap.set_named_scalar(left, "charged", Value::Bool(true))?;
    heap.set_named_edge(left, "subj", left_value)?;
    heap.set_named_edge(left_value, "next", left_child)?;
    heap.set_named_scalar(right, "charged", Value::Bool(true))?;
    heap.set_named_edge(right, "subj", right_value)?;
    heap.set_named_edge(right_value, "next", right_child)?;
    heap.set_named_edge(witness, "lv", left_value)?;
    heap.set_named_edge(witness, "lc", left_child)?;
    heap.set_named_edge(witness, "rv", right_value)?;
    heap.set_named_edge(witness, "rc", right_child)?;
    heap.set_named_edge(witness, "orphan", orphan)?;
    heap.issue_carrier(left_value)?;
    heap.issue_carrier(left_child)?;
    heap.issue_carrier(right_value)?;
    heap.issue_carrier(right_child)?;
    let frame = heap.enter_roots(5)?;
    // Last written root is popped first: left, strong_mt, right, witness, sentinel.
    heap.write_root(frame, 0, sentinel)?;
    heap.write_root(frame, 1, witness)?;
    heap.write_root(frame, 2, right)?;
    heap.write_root(frame, 3, strong_mt)?;
    heap.write_root(frame, 4, left)?;
    let (_, sentinel_record) = heap
        .locate_named(sentinel.slot(), "pending")
        .expect("sentinel record");
    let (_, left_value_record) = heap
        .locate_named(left.slot(), "subj")
        .expect("left value record");
    let (_, right_value_record) = heap
        .locate_named(right.slot(), "subj")
        .expect("right value record");
    assert_ne!(left_value_record, right_value_record);
    assert_ne!(left_value_record, sentinel_record);
    let _ = dust;
    let _ = frame;
    Ok(SharedModeGraph {
        sentinel,
        left,
        right,
        shared,
        witness,
        witness_mt,
        strong_mt,
        left_value,
        left_child,
        right_value,
        right_child,
        orphan,
        dust,
        sentinel_record,
        left_value_record,
        right_value_record,
    })
}

fn open_shared_mode_graph(host: &mut vm::HostContext<'_>) -> SharedModeGraph {
    heap::install(host, limits(16, 8, 24)).expect("install");
    let graph = heap::with_heap(host, plant_shared_mode_graph).expect("plant");
    heap::open_fixed_step(host).expect("open");
    graph
}

fn assert_owners_share_metatable(parked: &ParkedMark<'_>, graph: &SharedModeGraph) {
    for owner in [graph.left, graph.right] {
        let mt = parked
            .metatable(owner)
            .expect("metatable")
            .expect("owner metatable");
        assert_eq!(mt.slot(), graph.shared.slot());
        assert_eq!(
            mt.generation(),
            graph.shared.generation(),
            "both owners still point at the original shared metatable"
        );
    }
}

/// Charges one already-pending weak value record. The value stays white.
fn charge_pending_weak_value(
    parked: &mut ParkedMark<'_>,
    owner: TableRef,
    record: u32,
    value: TableRef,
) {
    let pending = pause_at(parked, owner.slot(), record);
    assert_eq!(pending.owner, owner.slot());
    assert_eq!(pending.cursor, record);
    assert!(
        !parked.marked(value),
        "the weak edge has not charged this value"
    );
    let checks = parked.meter().outgoing_edge_checks;
    let view = parked.pulse(1);
    assert!(
        !view.finished,
        "charging one weak edge does not finish the mark"
    );
    assert_eq!(view.meter.sweep_edge_checks, 0, "a pulse must not sweep");
    assert_eq!(
        view.meter.outgoing_edge_checks,
        checks + 1,
        "the pending weak record advanced by one edge charge"
    );
    assert!(!parked.marked(value), "the weak scan left the value white");
    assert_ne!(
        parked.pending().map(|scan| (scan.owner, scan.cursor)),
        Some((owner.slot(), record)),
        "the cursor left this weak record"
    );
}

/// Both owner value records have advanced. Stop on the sentinel's real record.
/// Mark-state mode bits are not visible here; this is the read-only substitute.
fn pause_after_both_weak_edges(
    parked: &mut ParkedMark<'_>,
    graph: &SharedModeGraph,
) -> PendingScan {
    let roots = parked.stats().live_roots;
    charge_pending_weak_value(
        parked,
        graph.left,
        graph.left_value_record,
        graph.left_value,
    );
    assert!(
        !parked.marked(graph.left_child),
        "the left successor has no independent strong root"
    );
    charge_pending_weak_value(
        parked,
        graph.right,
        graph.right_value_record,
        graph.right_value,
    );
    assert!(
        !parked.marked(graph.left_value),
        "the left value stayed white after its record advanced"
    );
    assert!(
        !parked.marked(graph.right_child),
        "the right successor has no independent strong root"
    );
    let witness_mt = parked
        .metatable(graph.witness)
        .expect("witness metatable")
        .expect("witness has a metatable");
    assert_eq!(witness_mt.slot(), graph.witness_mt.slot());
    assert_eq!(witness_mt.generation(), graph.witness_mt.generation());
    assert_ne!(
        witness_mt.slot(),
        graph.shared.slot(),
        "the observer does not share the owners' metatable"
    );
    assert_owners_share_metatable(parked, graph);
    let pending = pause_at(parked, graph.sentinel.slot(), graph.sentinel_record);
    assert_eq!(pending.owner, graph.sentinel.slot());
    assert_eq!(pending.cursor, graph.sentinel_record);
    assert_ne!(pending.owner, graph.left.slot());
    assert_ne!(pending.owner, graph.right.slot());
    assert!(!parked.marked(graph.left_value));
    assert!(!parked.marked(graph.right_value));
    assert!(!parked.marked(graph.left_child));
    assert!(!parked.marked(graph.right_child));
    assert!(!parked.marked(graph.orphan));
    assert!(!parked.marked(graph.dust));
    assert_eq!(parked.stats().live_roots, roots, "the stop adds no root");
    assert!(parked.meter().outgoing_edge_checks > 0);
    assert_eq!(parked.meter().sweep_edge_checks, 0);
    pending
}

struct SharedModeOutcome {
    left_value: bool,
    right_value: bool,
    left_child: bool,
    right_child: bool,
    orphan: bool,
    dust: bool,
    left_edge: Option<TableRef>,
    right_edge: Option<TableRef>,
    witness_left: Option<TableRef>,
    witness_right: Option<TableRef>,
    witness_orphan: Option<TableRef>,
}

fn read_shared_mode_outcome(
    heap: &LuaHeap,
    graph: &SharedModeGraph,
) -> Result<SharedModeOutcome, HeapError> {
    Ok(SharedModeOutcome {
        left_value: heap.alive(graph.left_value),
        right_value: heap.alive(graph.right_value),
        left_child: heap.alive(graph.left_child),
        right_child: heap.alive(graph.right_child),
        orphan: heap.alive(graph.orphan),
        dust: heap.alive(graph.dust),
        left_edge: heap.named_edge(graph.left, "subj")?,
        right_edge: heap.named_edge(graph.right, "subj")?,
        witness_left: heap.named_edge(graph.witness, "lv")?,
        witness_right: heap.named_edge(graph.witness, "rv")?,
        witness_orphan: heap.named_edge(graph.witness, "orphan")?,
    })
}

fn chain_yields(heap: &LuaHeap, owner: TableRef, target: TableRef) -> Result<bool, HeapError> {
    let carrier = heap.issued_value(target)?;
    let mut cursor = Value::Null;
    let mut saw = false;
    while let Some((key, value)) = heap.next_guest(owner, &cursor)? {
        if value == carrier {
            saw = true;
        }
        cursor = key;
    }
    Ok(saw)
}

fn finish_shared_mode_cycle(host: &mut vm::HostContext<'_>) {
    let done = finish_fixed_cycle(host);
    assert!(
        matches!(done, crate::heap::StepOutcome::Completed(_)),
        "{done:?}"
    );
    assert!(heap::fixed_step_idle(host).expect("idle"));
}

#[test]
fn shared_mode_scalar_delete_keeps_both_graphs() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = open_shared_mode_graph(&mut host);
        let mut schedules_before = 0u64;
        let mut schedules_after = 0u64;
        let mut lookups_before = 0u64;
        let mut lookups_after = 0u64;
        let mut heads_after = 0u64;
        let mut still_white = false;
        let mut pending_same = false;
        resume_weak(&mut host, |parked| {
            let pending = pause_after_both_weak_edges(parked, &graph);
            let before_mode = parked
                .with_saved_heap(|heap| heap.named_value(graph.shared, "__mode"))
                .expect("read mode");
            assert_eq!(
                before_mode,
                Value::string("v"),
                "the shared direct field is v before the delete"
            );
            schedules_before = parked.meter().owner_rescan_schedules;
            lookups_before = parked.meter().weak_mode_lookups;
            let deleted = parked
                .with_saved_heap(|heap| heap.set_named_scalar(graph.shared, "__mode", Value::Null));
            assert!(
                deleted.is_ok(),
                "saved-mark direct __mode delete succeeds, got {deleted:?}"
            );
            let cleared = parked
                .with_saved_heap(|heap| heap.named_value(graph.shared, "__mode"))
                .expect("read cleared mode");
            assert_eq!(
                cleared,
                Value::Null,
                "the direct __mode field is cleared, got {cleared:?}"
            );
            assert_owners_share_metatable(parked, &graph);
            pending_same = same_pending(Some(pending), parked.pending());
            schedules_after = parked.meter().owner_rescan_schedules;
            lookups_after = parked.meter().weak_mode_lookups;
            heads_after = parked.meter().owner_rescan_heads;
            still_white = !parked.marked(graph.left_value)
                && !parked.marked(graph.left_child)
                && !parked.marked(graph.right_value)
                && !parked.marked(graph.right_child);
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let seen = read_shared_mode_outcome(heap, &graph)?;
            eprintln!(
                "shared_mode_scalar_delete observation: left_value={} right_value={} left_child={} right_child={} orphan={} dust={} left_edge={:?} right_edge={:?} witness_left={:?} witness_right={:?} witness_orphan={:?} still_white_after_write={} pending_same={} rescan_schedules {}->{} weak_mode_lookups {}->{} owner_rescan_heads={} mode_cache_field_visible=false",
                seen.left_value,
                seen.right_value,
                seen.left_child,
                seen.right_child,
                seen.orphan,
                seen.dust,
                seen.left_edge,
                seen.right_edge,
                seen.witness_left,
                seen.witness_right,
                seen.witness_orphan,
                still_white,
                pending_same,
                schedules_before,
                schedules_after,
                lookups_before,
                lookups_after,
                heads_after
            );
            assert!(
                seen.left_value && seen.right_value && seen.left_child && seen.right_child,
                "shared __mode delete before weak clear keeps both owner graphs and their strong successors; left_value={} right_value={} left_child={} right_child={} orphan={} dust={} left_edge={:?} right_edge={:?} witness_left={:?} witness_right={:?} witness_orphan={:?} still_white_after_write={} pending_same={} rescan_schedules {}->{} weak_mode_lookups {}->{} owner_rescan_heads={} mode_cache_not_visible",
                seen.left_value,
                seen.right_value,
                seen.left_child,
                seen.right_child,
                seen.orphan,
                seen.dust,
                seen.left_edge,
                seen.right_edge,
                seen.witness_left,
                seen.witness_right,
                seen.witness_orphan,
                still_white,
                pending_same,
                schedules_before,
                schedules_after,
                lookups_before,
                lookups_after,
                heads_after
            );
            assert!(
                !seen.orphan,
                "the independent weak orphan proves this cycle released nodes"
            );
            assert!(!seen.dust, "an unrooted node was released");
            assert_eq!(seen.left_edge, Some(graph.left_value));
            assert_eq!(seen.right_edge, Some(graph.right_value));
            assert_eq!(seen.witness_left, Some(graph.left_value));
            assert_eq!(seen.witness_right, Some(graph.right_value));
            assert_eq!(seen.witness_orphan, None);
            assert!(
                chain_yields(heap, graph.left, graph.left_value)?,
                "next still yields the original left value"
            );
            assert!(
                chain_yields(heap, graph.right, graph.right_value)?,
                "next still yields the original right value"
            );
            assert!(
                chain_yields(heap, graph.left_value, graph.left_child)?,
                "the left successor keeps its identity"
            );
            assert!(
                chain_yields(heap, graph.right_value, graph.right_child)?,
                "the right successor keeps its identity"
            );
            heap.set_named_scalar(graph.shared, "__mode", Value::string("v"))?;
            Ok(())
        })
        .expect("this cycle");
        heap::collect(&mut host).expect("next cycle after weak mode is restored");
        heap::with_heap(&mut host, |heap| {
            assert!(
                !heap.alive(graph.left_value),
                "restoring direct v clears the left value on the next collect"
            );
            assert!(
                !heap.alive(graph.right_value),
                "restoring direct v clears the right value on the next collect"
            );
            assert!(!heap.alive(graph.left_child));
            assert!(!heap.alive(graph.right_child));
            Ok(())
        })
        .expect("next cycle");
    });
}

#[test]
fn shared_mode_scalar_left_swap_keeps_only_left() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = open_shared_mode_graph(&mut host);
        resume_weak(&mut host, |parked| {
            pause_after_both_weak_edges(parked, &graph);
            write_metatable(parked, graph.left, Some(graph.strong_mt))
                .expect("replace only the left metatable with the preallocated strong table");
            let left_mt = parked
                .metatable(graph.left)
                .expect("left metatable")
                .expect("left has a metatable");
            assert_eq!(left_mt.slot(), graph.strong_mt.slot());
            assert_eq!(left_mt.generation(), graph.strong_mt.generation());
            let right_mt = parked
                .metatable(graph.right)
                .expect("right metatable")
                .expect("right has a metatable");
            assert_eq!(right_mt.slot(), graph.shared.slot());
            assert_eq!(right_mt.generation(), graph.shared.generation());
            let mode = parked
                .with_saved_heap(|heap| heap.named_value(graph.shared, "__mode"))
                .expect("read shared mode");
            assert_eq!(mode, Value::string("v"), "the shared field stays v");
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let seen = read_shared_mode_outcome(heap, &graph)?;
            assert!(
                seen.left_value && seen.left_child && !seen.right_value && !seen.right_child,
                "replacing only the left metatable keeps the left graph and clears the right graph; left_value={} right_value={} left_child={} right_child={} orphan={} left_edge={:?} right_edge={:?} witness_left={:?} witness_right={:?}",
                seen.left_value,
                seen.right_value,
                seen.left_child,
                seen.right_child,
                seen.orphan,
                seen.left_edge,
                seen.right_edge,
                seen.witness_left,
                seen.witness_right
            );
            assert!(
                !seen.orphan,
                "the independent weak orphan proves this cycle released nodes"
            );
            assert!(!seen.dust, "an unrooted node was released");
            assert_eq!(seen.left_edge, Some(graph.left_value));
            assert_eq!(seen.right_edge, None);
            assert_eq!(seen.witness_left, Some(graph.left_value));
            assert_eq!(seen.witness_right, None);
            assert!(
                chain_yields(heap, graph.left, graph.left_value)?,
                "next still yields the original left value"
            );
            assert!(
                chain_yields(heap, graph.left_value, graph.left_child)?,
                "the left successor keeps its identity"
            );
            heap.set_metatable(graph.left, Some(graph.shared))?;
            Ok(())
        })
        .expect("this cycle");
        heap::collect(&mut host).expect("next cycle after the left owner is weak again");
        heap::with_heap(&mut host, |heap| {
            assert!(
                !heap.alive(graph.left_value),
                "putting the left owner back on the shared weak metatable clears its value"
            );
            assert!(!heap.alive(graph.left_child));
            assert!(!heap.alive(graph.right_value));
            Ok(())
        })
        .expect("next cycle");
    });
}

#[test]
fn shared_mode_scalar_omitted_clears_both() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = open_shared_mode_graph(&mut host);
        resume_weak(&mut host, |parked| {
            pause_after_both_weak_edges(parked, &graph);
            assert_owners_share_metatable(parked, &graph);
            let mode = parked
                .with_saved_heap(|heap| heap.named_value(graph.shared, "__mode"))
                .expect("read mode");
            assert_eq!(mode, Value::string("v"));
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let seen = read_shared_mode_outcome(heap, &graph)?;
            assert!(
                !seen.left_value && !seen.right_value && !seen.left_child && !seen.right_child,
                "omitting the shared __mode delete clears both owner graphs; left_value={} right_value={} left_child={} right_child={} orphan={} dust={}",
                seen.left_value,
                seen.right_value,
                seen.left_child,
                seen.right_child,
                seen.orphan,
                seen.dust
            );
            assert!(
                !seen.orphan,
                "the independent weak orphan proves this cycle released nodes"
            );
            assert!(!seen.dust, "an unrooted node was released");
            assert_eq!(seen.left_edge, None);
            assert_eq!(seen.right_edge, None);
            assert_eq!(seen.witness_left, None);
            assert_eq!(seen.witness_right, None);
            assert_eq!(seen.witness_orphan, None);
            Ok(())
        })
        .expect("omitted");
    });
}

#[test]
fn shared_mode_scalar_published_tail_refuses_delete() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = open_shared_mode_graph(&mut host);
        let published = heap::step_fixed_frames(&mut host, 128).expect("publish");
        assert!(
            matches!(published, crate::heap::StepOutcome::Published { .. }),
            "{published:?}"
        );
        let before_tail = heap::saved_tail_state(&mut host)
            .expect("tail read")
            .expect("published tail");
        let refused = heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let before_mode = heap.named_value(graph.shared, "__mode")?;
            assert_eq!(before_mode, Value::string("v"));
            let write = heap.set_named_scalar(graph.shared, "__mode", Value::Null);
            assert!(
                matches!(write, Err(HeapError::SweepPending)),
                "the existing published tail refuses this delete, got {write:?}"
            );
            assert_eq!(
                heap.named_value(graph.shared, "__mode")?,
                Value::string("v")
            );
            let left_mt = heap.metatable(graph.left)?.expect("left metatable");
            let right_mt = heap.metatable(graph.right)?.expect("right metatable");
            assert_eq!(left_mt.slot(), graph.shared.slot());
            assert_eq!(left_mt.generation(), graph.shared.generation());
            assert_eq!(right_mt.slot(), graph.shared.slot());
            assert_eq!(right_mt.generation(), graph.shared.generation());
            heap.set_named_scalar(graph.shared, "__mode", Value::Null)
        });
        assert!(
            matches!(refused, Err(HeapError::SweepPending)),
            "the tail gate is the refusal, got {refused:?}"
        );
        let after_tail = heap::saved_tail_state(&mut host)
            .expect("tail read")
            .expect("tail still saved");
        assert_eq!(before_tail.phase, after_tail.phase);
        assert_eq!(before_tail.cursor, after_tail.cursor);
        assert_eq!(before_tail.live_roots, after_tail.live_roots);
        assert_eq!(before_tail.dead_len, after_tail.dead_len);
    });
}

struct ModeProbePeer {
    peer: TableRef,
    peer_mt: TableRef,
    peer_value: TableRef,
    peer_child: TableRef,
    peer_value_record: u32,
}

fn attach_mode_probe_peer(
    heap: &mut LuaHeap,
    graph: &SharedModeGraph,
) -> Result<ModeProbePeer, HeapError> {
    let peer = heap.alloc()?;
    let peer_mt = heap.alloc()?;
    let peer_value = heap.alloc()?;
    let peer_child = heap.alloc()?;
    heap.set_mode_scalar(peer_mt, Value::string("v"))?;
    heap.set_metatable(peer, Some(peer_mt))?;
    heap.set_named_edge(peer, "subj", peer_value)?;
    heap.set_named_edge(peer_value, "next", peer_child)?;
    heap.set_named_edge(graph.sentinel, "peer", peer)?;
    heap.issue_carrier(peer_value)?;
    heap.issue_carrier(peer_child)?;
    let (_, peer_value_record) = heap
        .locate_named(peer.slot(), "subj")
        .expect("peer value record");
    Ok(ModeProbePeer {
        peer,
        peer_mt,
        peer_value,
        peer_child,
        peer_value_record,
    })
}

fn open_shared_mode_with_peer(host: &mut vm::HostContext<'_>) -> (SharedModeGraph, ModeProbePeer) {
    heap::install(host, limits(24, 8, 32)).expect("install");
    let planted = heap::with_heap(host, |heap| -> Result<_, HeapError> {
        let graph = plant_shared_mode_graph(heap)?;
        let peer = attach_mode_probe_peer(heap, &graph)?;
        Ok((graph, peer))
    })
    .expect("plant");
    heap::open_fixed_step(host).expect("open");
    planted
}

fn assert_delete_does_not_walk(before: &ScanMeter, after: &ScanMeter) {
    assert_eq!(after.mode_owner_probes, before.mode_owner_probes);
    assert!(after.mode_discover_compares - before.mode_discover_compares <= 1);
    assert_eq!(after.mode_discover_writes, before.mode_discover_writes + 1);
    assert_eq!(
        after.scratch.mode_discover_inited,
        before.scratch.mode_discover_inited
    );
    assert_eq!(after.node_slot_visits, before.node_slot_visits);
    assert_eq!(after.root_slot_visits, before.root_slot_visits);
    assert_eq!(after.owner_rescan_schedules, before.owner_rescan_schedules);
    assert_eq!(
        after.scratch.owner_rescan_inited,
        before.scratch.owner_rescan_inited
    );
    assert_eq!(
        after.scratch.owner_rescan_pushed,
        before.scratch.owner_rescan_pushed
    );
    assert_eq!(
        after.scratch.weak_modes_inited,
        before.scratch.weak_modes_inited
    );
    assert_eq!(after.outgoing_edge_checks, before.outgoing_edge_checks);
    assert_eq!(after.mark_passes, before.mark_passes);
    assert_eq!(after.mode_cache_updates, before.mode_cache_updates + 1);
}

/// The slot this probe unit actually read. A pass boundary spends the unit on
/// slot 0 after a non-counting finish, so the cursor is not simply previous+1.
fn probed_slot(before: Option<u32>, after: Option<u32>) -> u32 {
    match (before, after) {
        (Some(start), Some(end)) if end == start + 1 => start,
        (Some(start), Some(1)) if start > 1 => 0,
        other => panic!("one budget unit did not read one slot, got {other:?}"),
    }
}

#[test]
fn shared_mode_scalar_delete_probes_one_slot_per_step() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let (graph, peer) = open_shared_mode_with_peer(&mut host);
        resume_weak(&mut host, |parked| {
            pause_after_both_weak_edges(parked, &graph);
            let opened = pause_at(parked, peer.peer.slot(), peer.peer_value_record);
            assert_eq!(opened.owner, peer.peer.slot());
            assert_eq!(opened.cursor, peer.peer_value_record);
            assert!(!parked.marked(peer.peer_value));
            let before_swap = parked.meter().owner_rescan_schedules;
            write_metatable(parked, peer.peer, Some(graph.strong_mt)).expect("swap peer");
            assert_eq!(
                parked.meter().owner_rescan_schedules,
                before_swap + 1,
                "the existing single-table rescan is recorded before the shared delete"
            );
            let pending = parked.pending();
            let before = parked.meter();
            assert_eq!(parked.discovery_slot(), None);
            let deleted = parked
                .with_saved_heap(|heap| heap.set_named_scalar(graph.shared, "__mode", Value::Null));
            assert!(deleted.is_ok(), "direct delete succeeds, got {deleted:?}");
            assert_delete_does_not_walk(&before, &parked.meter());
            assert!(same_pending(pending, parked.pending()));
            assert_eq!(parked.discovery_slot(), Some(0));
            assert_eq!(parked.phase_name(), "gray");
            assert!(!parked.marked(graph.left_value));
            assert!(!parked.marked(graph.right_value));
            let mut left_step = None;
            let mut right_step = None;
            let mut probe_steps = 0u32;
            let mut saw_peer_head = false;
            let mut left_marked_on_edge = false;
            let mut right_marked_on_edge = false;
            for _ in 0..512 {
                let before_step = parked.meter();
                let before_slot = parked.discovery_slot();
                let before_pending = parked.pending();
                let before_rescan = parked.pending_rescan();
                let left_was_white = !parked.marked(graph.left_value);
                let right_was_white = !parked.marked(graph.right_value);
                let view = parked.pulse(1);
                assert_eq!(
                    parked.meter().mark_passes,
                    1,
                    "discovery must not restart mark"
                );
                assert_eq!(parked.phase_name(), "gray");
                assert!(!view.finished);
                let after_step = parked.meter();
                if after_step.owner_rescan_heads > before_step.owner_rescan_heads && !saw_peer_head
                {
                    assert_eq!(
                        before_step.mode_owner_probes, 0,
                        "the existing rescan opens before any deleted-mode probe"
                    );
                    assert_eq!(
                        parked.pending_rescan().map(|scan| scan.owner),
                        Some(peer.peer.slot())
                    );
                    saw_peer_head = true;
                }
                if after_step.mode_owner_probes != before_step.mode_owner_probes {
                    assert_eq!(
                        after_step.mode_owner_probes,
                        before_step.mode_owner_probes + 1
                    );
                    assert!(same_pending(before_pending, parked.pending()));
                    assert_eq!(
                        parked
                            .pending_rescan()
                            .map(|scan| (scan.owner, scan.cursor)),
                        before_rescan.map(|scan| (scan.owner, scan.cursor))
                    );
                    let read = probed_slot(before_slot, parked.discovery_slot());
                    let scheduled =
                        after_step.owner_rescan_schedules - before_step.owner_rescan_schedules;
                    assert!(scheduled <= 1, "one probe reserves at most one owner");
                    probe_steps += 1;
                    if read == graph.left.slot() {
                        assert_eq!(scheduled, 1);
                        left_step = Some(probe_steps);
                        assert!(!parked.marked(graph.left_value));
                    } else if read == graph.right.slot() {
                        assert_eq!(scheduled, 1);
                        right_step = Some(probe_steps);
                        assert!(!parked.marked(graph.right_value));
                    } else {
                        assert_eq!(scheduled, 0, "a non-owner slot does not reserve anyone");
                    }
                }
                if left_was_white && parked.marked(graph.left_value) {
                    assert!(
                        parked.meter().outgoing_edge_checks > before_step.outgoing_edge_checks,
                        "the left value is marked by a charged edge, not by the setter"
                    );
                    left_marked_on_edge = true;
                }
                if right_was_white && parked.marked(graph.right_value) {
                    assert!(
                        parked.meter().outgoing_edge_checks > before_step.outgoing_edge_checks,
                        "the right value is marked by a charged edge, not by the setter"
                    );
                    right_marked_on_edge = true;
                }
                if left_step.is_some()
                    && right_step.is_some()
                    && saw_peer_head
                    && left_marked_on_edge
                    && right_marked_on_edge
                {
                    break;
                }
            }
            assert_ne!(
                left_step, right_step,
                "the two owners are reserved on different steps"
            );
            assert!(left_step.is_some() && right_step.is_some());
            assert!(saw_peer_head, "the existing rescan was not swallowed");
            assert!(left_marked_on_edge && right_marked_on_edge);
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let seen = read_shared_mode_outcome(heap, &graph)?;
            assert!(seen.left_value && seen.right_value);
            assert!(seen.left_child && seen.right_child);
            assert!(!seen.orphan, "the independent weak orphan still clears");
            assert!(!seen.dust);
            assert!(
                heap.alive(peer.peer_value),
                "the pre-existing rescan kept its value"
            );
            assert!(heap.alive(peer.peer_child));
            assert!(chain_yields(heap, graph.left, graph.left_value)?);
            assert!(chain_yields(heap, graph.right, graph.right_value)?);
            Ok(())
        })
        .expect("budget cycle");
    });
}

#[test]
fn shared_mode_scalar_delete_same_batch_has_bounded_slot_work() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let (graph, peer) = open_shared_mode_with_peer(&mut host);
        let capacity = heap::with_heap(&mut host, |heap| Ok(heap.slots.len())).expect("capacity");
        resume_weak(&mut host, |parked| {
            pause_after_both_weak_edges(parked, &graph);
            pause_at(parked, peer.peer.slot(), peer.peer_value_record);
            let pending = parked.pending();
            for meta in [graph.shared, peer.peer_mt] {
                let before = parked.meter();
                parked
                    .with_saved_heap(|heap| heap.set_named_scalar(meta, "__mode", Value::Null))
                    .expect("delete before first probe");
                assert_delete_does_not_walk(&before, &parked.meter());
                assert!(same_pending(pending, parked.pending()));
                assert_eq!(parked.discovery_slot(), Some(0));
            }
            let mut scheduled = Vec::new();
            let mut next_slot = 0;
            for _ in 0..512 {
                let before = parked.meter();
                let cursor = parked.discovery_slot();
                assert!(!parked.pulse(1).finished);
                let after = parked.meter();
                let probes = after.mode_owner_probes - before.mode_owner_probes;
                assert!(probes <= 1);
                assert!(
                    after.mode_discover_compares - before.mode_discover_compares <= probes,
                    "one capacity-slot probe must not walk the pending identities (cursor {cursor:?})"
                );
                assert_eq!(after.mode_discover_writes, before.mode_discover_writes);
                assert_eq!(
                    after.scratch.mode_discover_inited,
                    before.scratch.mode_discover_inited
                );
                if probes == 1 {
                    let read = probed_slot(cursor, parked.discovery_slot());
                    assert_eq!(
                        read as usize, next_slot,
                        "both deletes belong to the first pass"
                    );
                    next_slot += 1;
                    let added = after.owner_rescan_schedules - before.owner_rescan_schedules;
                    assert!(added <= 1);
                    if added == 1 {
                        scheduled.push(read);
                    }
                }
                if next_slot == capacity {
                    break;
                }
            }
            assert_eq!(next_slot, capacity);
            assert_eq!(parked.discovery_slot(), Some(capacity as u32));
            assert_eq!(
                scheduled,
                [graph.left.slot(), graph.right.slot(), peer.peer.slot()]
            );
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let seen = read_shared_mode_outcome(heap, &graph)?;
            assert!(seen.left_value && seen.left_child && seen.right_value && seen.right_child);
            assert!(heap.alive(peer.peer_value) && heap.alive(peer.peer_child));
            assert!(!seen.orphan && !seen.dust);
            Ok(())
        })
        .expect("same batch");
    });
}

#[test]
fn shared_mode_scalar_delete_second_metatable_is_not_missed() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let (graph, peer) = open_shared_mode_with_peer(&mut host);
        resume_weak(&mut host, |parked| {
            pause_after_both_weak_edges(parked, &graph);
            pause_at(parked, peer.peer.slot(), peer.peer_value_record);
            let deleted = parked
                .with_saved_heap(|heap| heap.set_named_scalar(graph.shared, "__mode", Value::Null));
            assert!(deleted.is_ok(), "{deleted:?}");
            assert_eq!(parked.meter().mode_cache_updates, 1);
            let mut peer_visits = 0u32;
            let mut left_seen = false;
            let mut right_seen = false;
            for _ in 0..512 {
                let before = parked.meter();
                let slot = parked.discovery_slot();
                let view = parked.pulse(1);
                let after = parked.meter();
                assert_eq!(after.mark_passes, 1);
                assert_eq!(
                    after.mode_discover_writes, before.mode_discover_writes,
                    "the next pass must start without moving a batch of identities"
                );
                assert!(!view.finished, "discovery must finish before Done");
                if after.mode_owner_probes != before.mode_owner_probes {
                    let read = probed_slot(slot, parked.discovery_slot());
                    let scheduled =
                        parked.meter().owner_rescan_schedules - before.owner_rescan_schedules;
                    if read == graph.left.slot() && scheduled == 1 {
                        left_seen = true;
                    }
                    if read == graph.right.slot() && scheduled == 1 {
                        right_seen = true;
                    }
                    if read == peer.peer.slot() {
                        peer_visits += 1;
                        if peer_visits == 1 {
                            assert_eq!(scheduled, 0);
                            assert!(left_seen && right_seen);
                            let cursor = parked.discovery_slot();
                            let second = parked.with_saved_heap(|heap| {
                                heap.set_named_scalar(peer.peer_mt, "__mode", Value::Null)
                            });
                            assert!(second.is_ok(), "{second:?}");
                            assert_eq!(parked.meter().mode_cache_updates, 2);
                            let again = parked.with_saved_heap(|heap| {
                                heap.set_named_scalar(graph.shared, "__mode", Value::Null)
                            });
                            assert!(again.is_ok(), "a missing field delete is not refused");
                            assert_eq!(parked.meter().mode_cache_updates, 2);
                            assert_eq!(parked.discovery_slot(), cursor);
                            assert_eq!(parked.phase_name(), "gray");
                        }
                        if peer_visits == 2 {
                            assert_eq!(scheduled, 1);
                            assert!(!parked.marked(peer.peer_value));
                            break;
                        }
                    }
                }
            }
            assert_eq!(
                peer_visits, 2,
                "the later metatable waits for the next pass"
            );
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            assert!(heap.alive(graph.left_value));
            assert!(heap.alive(graph.right_value));
            assert!(
                heap.alive(peer.peer_value),
                "the second pass kept the later owner"
            );
            assert!(heap.alive(peer.peer_child));
            assert!(!heap.alive(graph.orphan));
            assert_eq!(heap.scan_meter().mode_cache_updates, 2);
            assert_eq!(heap.scan_meter().owner_rescan_schedules, 3);
            Ok(())
        })
        .expect("second metatable");
    });
}

/// Same-cycle reinsertion of `v` is accepted and does not restart mark. This
/// narrow cache stays missing-field until the next collect reads the field.
#[test]
fn shared_mode_scalar_reinsert_stays_strong_until_next_collect() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = open_shared_mode_graph(&mut host);
        resume_weak(&mut host, |parked| {
            let pending = pause_after_both_weak_edges(parked, &graph);
            let deleted = parked
                .with_saved_heap(|heap| heap.set_named_scalar(graph.shared, "__mode", Value::Null));
            assert!(deleted.is_ok(), "{deleted:?}");
            let before = parked.meter();
            let restored = parked.with_saved_heap(|heap| {
                heap.set_named_scalar(graph.shared, "__mode", Value::string("v"))
            });
            assert!(
                restored.is_ok(),
                "reinsertion is not refused, got {restored:?}"
            );
            assert_eq!(parked.meter().mode_owner_probes, before.mode_owner_probes);
            assert_eq!(parked.meter().mark_passes, 1);
            assert_eq!(parked.meter().mode_cache_updates, 1);
            assert!(same_pending(Some(pending), parked.pending()));
            assert_eq!(parked.discovery_slot(), Some(0));
            assert_eq!(parked.phase_name(), "gray");
            assert!(!parked.marked(graph.left_value));
            assert!(!parked.marked(graph.right_value));
            let again = parked
                .with_saved_heap(|heap| heap.set_named_scalar(graph.shared, "__mode", Value::Null));
            assert!(again.is_ok(), "{again:?}");
            assert_eq!(parked.meter().mode_cache_updates, 1);
            assert_eq!(parked.discovery_slot(), Some(0));
            let rewritten = parked.with_saved_heap(|heap| {
                heap.set_named_scalar(graph.shared, "__mode", Value::string("v"))
            });
            assert!(rewritten.is_ok(), "{rewritten:?}");
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            assert!(heap.alive(graph.left_value));
            assert!(heap.alive(graph.right_value));
            assert_eq!(
                heap.named_value(graph.shared, "__mode")?,
                Value::string("v")
            );
            assert!(!heap.alive(graph.orphan));
            Ok(())
        })
        .expect("this cycle stays strong");
        heap::collect(&mut host).expect("next collect reads the restored field");
        heap::with_heap(&mut host, |heap| {
            assert!(!heap.alive(graph.left_value));
            assert!(!heap.alive(graph.right_value));
            Ok(())
        })
        .expect("next cycle");
    });
}

#[test]
fn shared_mode_scalar_key_and_value_delete_does_not_arm_probe() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(8, 4, 8)).expect("install");
        let (owner, mt, record) = heap::with_heap(&mut host, |heap| -> Result<_, HeapError> {
            let owner = heap.alloc()?;
            let mt = heap.alloc()?;
            let sentinel = heap.alloc()?;
            heap.set_mode_scalar(mt, Value::string("kv"))?;
            heap.set_metatable(owner, Some(mt))?;
            heap.set_named_scalar(owner, "charged", Value::Bool(true))?;
            heap.set_named_scalar(sentinel, "pending", Value::Bool(true))?;
            let frame = heap.enter_roots(2)?;
            heap.write_root(frame, 0, sentinel)?;
            heap.write_root(frame, 1, owner)?;
            let (_, record) = heap.locate_named(owner.slot(), "charged").expect("record");
            Ok((owner, mt, record))
        })
        .expect("plant");
        heap::open_fixed_step(&mut host).expect("open");
        resume_weak(&mut host, |parked| {
            pause_at(parked, owner.slot(), record);
            let before = parked.meter();
            let deleted =
                parked.with_saved_heap(|heap| heap.set_named_scalar(mt, "__mode", Value::Null));
            assert!(deleted.is_ok(), "{deleted:?}");
            assert_eq!(parked.meter().mode_cache_updates, before.mode_cache_updates);
            assert_eq!(parked.meter().mode_owner_probes, before.mode_owner_probes);
            assert_eq!(parked.discovery_slot(), None);
            assert_eq!(parked.phase_name(), "gray");
        });
        finish_shared_mode_cycle(&mut host);
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.scan_meter().mode_owner_probes))
                .expect("meter"),
            0
        );
    });
}

#[derive(Debug)]
enum SharedModeReplacement {
    Strong,
    Clear,
    Mode(&'static str),
}

#[derive(Debug)]
struct SharedModeSwitchObservation {
    old_left_mt: Option<TableRef>,
    new_left_mt: Option<TableRef>,
    discovery_after_delete: Option<u32>,
    probe_delta: u64,
    setter_schedule_delta: u64,
    pending_before: PendingScan,
    pending_after: Option<PendingScan>,
    left_still_white: bool,
}

fn assert_shared_mode_side(
    heap: &LuaHeap,
    side: &str,
    (owner, value, child): (TableRef, TableRef, TableRef),
    keep: bool,
    observation: &SharedModeSwitchObservation,
) -> Result<(), HeapError> {
    let value_alive = heap.alive(value);
    let child_alive = heap.alive(child);
    let subject = heap.named_edge(owner, "subj")?;
    let successor = if value_alive {
        heap.named_edge(value, "next")?
    } else {
        None
    };
    assert_eq!(
        (subject, successor, value_alive, child_alive),
        (keep.then_some(value), keep.then_some(child), keep, keep),
        "{side}: this cycle must preserve or clear the original value and its successor together (keep={keep})\n\
         mutation diagnostics: {observation:?}"
    );
    if keep {
        assert!(chain_yields(heap, owner, value)?, "next: {side} value");
        assert!(chain_yields(heap, value, child)?, "next: {side} successor");
    } else {
        assert_eq!(heap.named_value(owner, "subj")?, Value::Null);
    }
    Ok(())
}

fn assert_shared_mode_witness(
    heap: &LuaHeap,
    graph: &SharedModeGraph,
    (keep_left, keep_right): (bool, bool),
) -> Result<(), HeapError> {
    for (name, expected) in [
        ("lv", keep_left.then_some(graph.left_value)),
        ("lc", keep_left.then_some(graph.left_child)),
        ("rv", keep_right.then_some(graph.right_value)),
        ("rc", keep_right.then_some(graph.right_child)),
        ("orphan", None),
    ] {
        assert_eq!(heap.named_edge(graph.witness, name)?, expected, "{name}");
    }
    assert!(!heap.alive(graph.orphan), "independent weak orphan cleared");
    Ok(())
}

fn check_shared_mode_left_replacement(
    delete_shared: bool,
    replacement: SharedModeReplacement,
    expected: (bool, bool),
) {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(16, 8, 24)).expect("install");
        let graph = heap::with_heap(&mut host, |heap| {
            let graph = plant_shared_mode_graph(heap)?;
            // The fixture's spare metatable is already rooted. Set its direct
            // mode before opening the cycle so ModeInit sees the replacement.
            if let SharedModeReplacement::Mode(mode) = replacement {
                heap.set_mode_scalar(graph.strong_mt, Value::string(mode))?;
            }
            Ok(graph)
        })
        .expect("plant");
        let new_mt = match replacement {
            SharedModeReplacement::Strong | SharedModeReplacement::Mode(_) => Some(graph.strong_mt),
            SharedModeReplacement::Clear => None,
        };
        heap::open_fixed_step(&mut host).expect("open");

        let observation = heap::with_heap(&mut host, |heap| {
            let mut parked = ParkedMark::resume(heap).expect("resume saved mark");
            let pending_before = pause_after_both_weak_edges(&mut parked, &graph);
            let old_left_mt = parked.metatable(graph.left)?;
            assert_eq!(
                parked.with_saved_heap(|heap| heap.named_value(graph.shared, "__mode"))?,
                Value::string("v")
            );
            let probes_before = parked.meter().mode_owner_probes;
            if delete_shared {
                parked.with_saved_heap(|heap| {
                    heap.set_named_scalar(graph.shared, "__mode", Value::Null)
                })?;
            }
            // Only read counters/cursor between the real unlink and setter.
            let discovery_after_delete = parked.discovery_slot();
            let schedules_before = parked.meter().owner_rescan_schedules;
            write_metatable(&mut parked, graph.left, new_mt)?;
            let observation = SharedModeSwitchObservation {
                old_left_mt,
                new_left_mt: parked.metatable(graph.left)?,
                discovery_after_delete,
                probe_delta: parked.meter().mode_owner_probes - probes_before,
                setter_schedule_delta: parked.meter().owner_rescan_schedules - schedules_before,
                pending_before,
                pending_after: parked.pending(),
                left_still_white: !parked.marked(graph.left_value)
                    && !parked.marked(graph.left_child),
            };
            if delete_shared {
                assert!(
                    parked.with_saved_heap(|heap| {
                        heap.locate_named(graph.shared.slot(), "__mode").is_none()
                    }),
                    "the shared mode record must be unlinked, not left as a gap"
                );
            }
            assert_eq!(parked.metatable(graph.right)?, Some(graph.shared));
            parked.park()?;
            Ok(observation)
        })
        .expect("mutate while parked in Gray");

        finish_shared_mode_cycle(&mut host);
        let left = (graph.left, graph.left_value, graph.left_child);
        let right = (graph.right, graph.right_value, graph.right_child);
        heap::with_heap(&mut host, |heap| {
            assert_shared_mode_side(heap, "left", left, expected.0, &observation)?;
            assert_shared_mode_side(heap, "right", right, expected.1, &observation)?;
            assert_shared_mode_witness(heap, &graph, expected)?;

            // Assert the fixture window only after checking the business result.
            // Scheduling is diagnostic; a correct repair need not use a setter queue.
            eprintln!(
                "mutation diagnostics: setter rescan schedule delta={}, pending {:?} -> {:?}, discovery at unlink {:?}, left metatable {:?} -> {:?}",
                observation.setter_schedule_delta,
                observation.pending_before,
                observation.pending_after,
                observation.discovery_after_delete,
                observation.old_left_mt,
                observation.new_left_mt
            );
            assert_eq!(observation.old_left_mt, Some(graph.shared));
            assert_eq!(observation.new_left_mt, new_mt);
            assert_eq!(observation.probe_delta, 0);
            if delete_shared {
                assert!(
                    observation
                        .discovery_after_delete
                        .is_some_and(|slot| slot <= graph.left.slot()),
                    "deleted-mode discovery is armed before the left slot"
                );
            }
            assert!(same_pending(
                Some(observation.pending_before),
                observation.pending_after
            ));
            assert!(
                observation.left_still_white,
                "setter must not trace the left graph inline"
            );

            heap.set_mode_scalar(graph.shared, Value::string("v"))?;
            heap.set_metatable(graph.left, Some(graph.shared))?;
            Ok(())
        })
        .expect("this cycle");
        heap::collect(&mut host).expect("collect after restoring shared v");
        heap::with_heap(&mut host, |heap| {
            assert_shared_mode_side(heap, "left after restoring v", left, false, &observation)?;
            assert_shared_mode_side(heap, "right after restoring v", right, false, &observation)?;
            assert_shared_mode_witness(heap, &graph, (false, false))
        })
        .expect("next cycle");
    });
}

#[test]
fn shared_mode_scalar_delete_then_switch_strong_keeps_both_graphs() {
    check_shared_mode_left_replacement(true, SharedModeReplacement::Strong, (true, true));
}

#[test]
fn shared_mode_scalar_delete_then_clear_keeps_both_graphs() {
    check_shared_mode_left_replacement(true, SharedModeReplacement::Clear, (true, true));
}

#[test]
fn shared_mode_scalar_delete_then_replace_k_keeps_both_graphs() {
    check_shared_mode_left_replacement(true, SharedModeReplacement::Mode("k"), (true, true));
}

#[test]
fn shared_mode_scalar_delete_then_replace_v_clears_left_graph() {
    check_shared_mode_left_replacement(true, SharedModeReplacement::Mode("v"), (false, true));
}

#[test]
fn shared_mode_scalar_delete_then_replace_kv_clears_left_graph() {
    check_shared_mode_left_replacement(true, SharedModeReplacement::Mode("kv"), (false, true));
}

#[test]
fn shared_mode_scalar_left_clear_keeps_only_left() {
    check_shared_mode_left_replacement(false, SharedModeReplacement::Clear, (true, false));
}

/// The matching control for the same-tuple setter: discovery has been armed
/// but has not yet probed the owner slot. Re-setting the same metatable must
/// not become a setter rescan; the probe still makes the normal first schedule
/// for that left owner. The end state keeps both shared owners and clears the
/// independent orphan.
#[test]
fn shared_mode_same_metatable_reset_before_probe_schedules_only_discovery() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = open_shared_mode_graph(&mut host);
        let mut probed = None;
        resume_weak(&mut host, |parked| {
            pause_after_both_weak_edges(parked, &graph);
            parked
                .with_saved_heap(|heap| heap.set_named_scalar(graph.shared, "__mode", Value::Null))
                .expect("delete the shared direct mode in place");
            assert_eq!(parked.discovery_slot(), Some(0));
            let before = parked.meter();
            write_metatable(parked, graph.left, Some(graph.shared))
                .expect("re-set the same metatable before the probe runs");
            assert_eq!(
                parked.meter().scratch.owner_rescan_pushed,
                before.scratch.owner_rescan_pushed,
                "a same-tuple setter must not link the owner"
            );
            assert_eq!(
                parked.meter().owner_rescan_schedules,
                before.owner_rescan_schedules,
                "a same-tuple setter is not a weak-to-strong transition"
            );
            assert_eq!(
                parked.meter().owner_rescan_heads,
                before.owner_rescan_heads,
                "the setter must not open a rescan"
            );
            let mut seen = false;
            for _ in 0..HARNESS_BOUND {
                let slot = parked.discovery_slot();
                let before = parked.meter();
                let view = parked.pulse(1);
                let after = parked.meter();
                if after.scratch.owner_rescan_pushed > before.scratch.owner_rescan_pushed {
                    assert_eq!(
                        after.scratch.owner_rescan_pushed - before.scratch.owner_rescan_pushed,
                        1
                    );
                    assert_eq!(
                        after.mode_owner_probes - before.mode_owner_probes,
                        1,
                        "one capacity-slot probe made the first schedule"
                    );
                    probed = slot;
                    seen = true;
                    break;
                }
                assert!(
                    !view.finished,
                    "mark finished before discovery probed the left slot"
                );
            }
            assert!(seen, "discovery never scheduled the left owner");
            assert_eq!(
                probed,
                Some(graph.left.slot()),
                "the first schedule for the same target comes from its capacity-slot probe"
            );
            assert!(
                !parked.marked(graph.left_value) && !parked.marked(graph.left_child),
                "the setter did not trace the left graph inline"
            );
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let seen = read_shared_mode_outcome(heap, &graph)?;
            assert!(seen.left_value && seen.left_child);
            assert!(seen.right_value && seen.right_child);
            assert!(!seen.orphan && !seen.dust);
            assert_eq!(heap.scan_meter().scratch.owner_rescan_pushed, 2);
            assert_eq!(heap.scan_meter().owner_rescan_heads, 2);
            Ok(())
        })
        .expect("same metatable before probe");
    });
}

/// The owner already left the intrusive rescan queue: `open_rescan` popped it
/// and the rescan is charging its value record. Repeating the same
/// `(slot, generation)` setter must not link it again; a link here lets the
/// rescan queue stay ahead of the deleted-mode probe indefinitely. The probe
/// must still reach the right owner, and both shared owners keep their value
/// and successor while the independent orphan clears.
#[test]
fn shared_mode_same_metatable_reset_during_rescan_does_not_requeue_owner() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = open_shared_mode_graph(&mut host);
        let mut repeats = 0u32;
        let mut advanced = false;
        resume_weak(&mut host, |parked| {
            pause_after_both_weak_edges(parked, &graph);
            parked
                .with_saved_heap(|heap| heap.set_named_scalar(graph.shared, "__mode", Value::Null))
                .expect("delete the shared direct mode in place");
            let mut popped = false;
            for _ in 0..HARNESS_BOUND {
                if let Some(pending) = parked.pending_rescan()
                    && pending.owner == graph.left.slot()
                    && pending.cursor == graph.left_value_record
                {
                    popped = true;
                    break;
                }
                assert!(
                    !parked.pulse(1).finished,
                    "mark finished before the left rescan opened"
                );
            }
            assert!(
                popped,
                "the left owner never left the queue into a live rescan"
            );
            let before = parked.meter();
            write_metatable(parked, graph.left, Some(graph.shared))
                .expect("re-set the same metatable while the rescan is active");
            let after_setter = parked.meter();

            for _ in 0..HARNESS_BOUND {
                if parked
                    .discovery_slot()
                    .is_some_and(|slot| slot > graph.right.slot())
                {
                    advanced = true;
                    break;
                }
                if let Some(pending) = parked.pending_rescan()
                    && pending.owner == graph.left.slot()
                    && pending.cursor == graph.left_value_record
                {
                    repeats += 1;
                    write_metatable(parked, graph.left, Some(graph.shared))
                        .expect("repeat the same metatable during the rescan");
                }
                if parked.pulse(1).finished {
                    break;
                }
            }
            assert!(
                advanced,
                "repeated same-tuple setters kept the left owner ahead of discovery (repeats={repeats})"
            );
            assert_eq!(
                after_setter.scratch.owner_rescan_pushed, before.scratch.owner_rescan_pushed,
                "a popped owner must not be linked by a same-tuple setter"
            );
            assert_eq!(
                after_setter.owner_rescan_schedules, before.owner_rescan_schedules,
                "a same-tuple setter is not a weak-to-strong transition"
            );
            assert_eq!(
                after_setter.owner_rescan_heads, before.owner_rescan_heads,
                "the setter must not open another rescan"
            );
            assert_eq!(
                parked.meter().scratch.owner_rescan_pushed,
                before.scratch.owner_rescan_pushed + 1,
                "only the right owner is left for discovery to schedule"
            );
            assert!(
                !parked.marked(graph.right_value),
                "the right owner was scheduled by the probe but its rescan has not run"
            );
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let seen = read_shared_mode_outcome(heap, &graph)?;
            assert!(seen.left_value && seen.left_child);
            assert!(seen.right_value && seen.right_child);
            assert!(!seen.orphan && !seen.dust);
            assert_eq!(heap.scan_meter().scratch.owner_rescan_pushed, 2);
            assert_eq!(heap.scan_meter().owner_rescan_heads, 2);
            Ok(())
        })
        .expect("same metatable during rescan");
    });
}

// ---------------------------------------------------------------------------
// Gray slot growth + shared direct-mode delete witness.
// ---------------------------------------------------------------------------

/// Allocates one real table during a saved Gray mark. Its slot is the next
/// physical slot, past the slot-vector length that sized the ModeInit sidecar.
fn grow_one_table(parked: &mut ParkedMark<'_>) -> TableRef {
    parked
        .with_saved_heap(|heap| heap.alloc())
        .expect("a real allocation grows the slot vector")
}

/// A metatable allocated after ModeInit has no entry in the reset-time mode
/// cache or in the discovery sidecar. Writing a real `v` field and connecting
/// both owners registers a late read, but deleting the field before any pulse
/// can parse it means the delete sees no cache entry: the indexed update and
/// stamp stay outside that sidecar. After the late read finds only the missing
/// field, the deferred owner check restores the strong side and keeps the
/// already weakly scanned values. The parsed grown-slot delete is covered by
/// `shared_mode_delete_parsed_grown_metatable_rescans_owners`.
#[test]
fn shared_mode_delete_grown_metatable_stays_outside_sidecar() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = open_shared_mode_graph(&mut host);
        let mut grown_slot = 0u32;
        let mut old_slots = 0u32;
        resume_weak(&mut host, |parked| {
            pause_after_both_weak_edges(parked, &graph);
            old_slots = parked.stats().node_slots;
            let before = parked.meter();
            let mt = grow_one_table(parked);
            grown_slot = mt.slot();
            assert_eq!(
                grown_slot, old_slots,
                "the new metatable is the next physical slot"
            );
            assert_eq!(parked.stats().node_slots, old_slots + 1);
            parked
                .with_saved_heap(|heap| -> Result<(), HeapError> {
                    heap.set_mode_scalar(mt, Value::string("v"))?;
                    heap.set_metatable(graph.left, Some(mt))?;
                    heap.set_metatable(graph.right, Some(mt))?;
                    Ok(())
                })
                .expect("write the grown direct mode and connect both owners");
            assert_eq!(
                parked.with_saved_heap(|heap| heap.named_value(mt, "__mode")),
                Ok(Value::string("v"))
            );
            assert_eq!(parked.metatable(graph.left), Ok(Some(mt)));
            assert_eq!(parked.metatable(graph.right), Ok(Some(mt)));
            assert_eq!(
                parked.meter().scratch.mode_discover_inited,
                u64::from(old_slots),
                "the sidecar was sized from the pre-growth slot vector"
            );
            parked
                .with_saved_heap(|heap| heap.set_named_scalar(mt, "__mode", Value::Null))
                .expect("delete the grown direct mode in place");
            assert!(
                parked.with_saved_heap(|heap| heap.locate_named(mt.slot(), "__mode").is_none()),
                "the grown mode record is unlinked, not left as a gap"
            );
            assert_eq!(
                parked.discovery_slot(),
                None,
                "a grown metatable has no sidecar entry to arm"
            );
            assert_eq!(
                parked.meter().mode_cache_updates,
                before.mode_cache_updates,
                "the missing cache entry refuses the direct-delete update"
            );
            assert!(!parked.marked(graph.left_value));
            assert!(!parked.marked(graph.left_child));
            assert!(!parked.marked(graph.right_value));
            assert!(!parked.marked(graph.right_child));
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            assert_eq!(heap.slots.len() as u32, old_slots + 1);
            assert!(heap.alive(graph.left_value));
            assert!(heap.alive(graph.left_child));
            assert!(heap.alive(graph.right_value));
            assert!(heap.alive(graph.right_child));
            assert_eq!(heap.named_edge(graph.left, "subj")?, Some(graph.left_value));
            assert_eq!(
                heap.named_edge(graph.right, "subj")?,
                Some(graph.right_value)
            );
            assert!(!heap.alive(graph.orphan));
            assert!(!heap.alive(graph.dust));
            Ok(())
        })
        .expect("grown metatable");
    });
}

/// Same shape as the growth arm, but the replacement metatable exists before
/// ModeInit. Its `v` entry is therefore in the same slot-indexed domain as the
/// discovery sidecar. Deleting the field in place arms the probe at slot 0, and
/// the two already-started owners are found and rescanned as strong.
#[test]
fn shared_mode_delete_preallocated_weak_metatable_arms_sidecar() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(16, 8, 24)).expect("install");
        let graph = heap::with_heap(&mut host, |heap| -> Result<SharedModeGraph, HeapError> {
            let graph = plant_shared_mode_graph(heap)?;
            heap.set_mode_scalar(graph.strong_mt, Value::string("v"))?;
            Ok(graph)
        })
        .expect("plant");
        heap::open_fixed_step(&mut host).expect("open");
        let mut old_slots = 0u32;
        resume_weak(&mut host, |parked| {
            pause_after_both_weak_edges(parked, &graph);
            old_slots = parked.stats().node_slots;
            assert!(graph.strong_mt.slot() < old_slots);
            parked
                .with_saved_heap(|heap| -> Result<(), HeapError> {
                    heap.set_metatable(graph.left, Some(graph.strong_mt))?;
                    heap.set_metatable(graph.right, Some(graph.strong_mt))?;
                    Ok(())
                })
                .expect("switch both owners to the prebuilt weak metatable");
            let before = parked.meter();
            parked
                .with_saved_heap(|heap| {
                    heap.set_named_scalar(graph.strong_mt, "__mode", Value::Null)
                })
                .expect("delete the prebuilt direct mode in place");
            assert_eq!(
                parked.meter().mode_cache_updates,
                before.mode_cache_updates + 1
            );
            assert_eq!(
                parked.meter().mode_discover_writes,
                before.mode_discover_writes + 1,
                "the in-domain metatable stamps its sidecar entry"
            );
            assert_eq!(
                parked.meter().scratch.mode_discover_inited,
                u64::from(old_slots)
            );
            assert_eq!(parked.discovery_slot(), Some(0));
            assert!(!parked.marked(graph.left_value));
            assert!(!parked.marked(graph.left_child));
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            assert!(heap.alive(graph.left_value));
            assert!(heap.alive(graph.right_value));
            assert!(heap.alive(graph.left_child));
            assert!(heap.alive(graph.right_child));
            assert_eq!(heap.named_edge(graph.left, "subj")?, Some(graph.left_value));
            assert_eq!(
                heap.named_edge(graph.right, "subj")?,
                Some(graph.right_value)
            );
            assert!(!heap.alive(graph.orphan));
            assert!(!heap.alive(graph.dust));
            Ok(())
        })
        .expect("preallocated metatable");
    });
}

/// The metatable exists before ModeInit but the second owner is allocated after
/// both original owners were weakly scanned. The discovery walk reads the live
/// slot vector, so it must reach the grown owner slot and queue that owner too.
#[test]
fn shared_mode_delete_original_metatable_walks_grown_owner_slots() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = open_slot_growth_graph(&mut host);
        let mut old_slots = 0u32;
        let mut grown_owner = None;
        resume_weak(&mut host, |parked| {
            let white = [
                graph.left_value,
                graph.left_successor,
                graph.peer_value,
                graph.orphan,
                graph.value,
                graph.successor,
            ];
            charge_white(parked, graph.left.slot(), graph.left_record, &white);
            charge_white(parked, graph.peer.slot(), graph.peer_record, &white);
            old_slots = parked.stats().node_slots;
            let owner = grow_one_table(parked);
            grown_owner = Some(owner);
            assert_eq!(owner.slot(), old_slots);
            parked
                .with_saved_heap(|heap| -> Result<(), HeapError> {
                    heap.set_metatable(owner, Some(graph.old_mt))?;
                    heap.set_named_edge(owner, GROWN_ITEM, graph.value)?;
                    heap.set_named_edge(graph.value, "next", graph.successor)?;
                    Ok(())
                })
                .expect("mount the in-domain metatable on the grown owner");
            let record = parked
                .with_saved_heap(|heap| {
                    heap.find_key(owner.slot(), &super::EdgeKey::named(GROWN_ITEM))
                })
                .expect("grown owner item record");
            parked
                .write_table_root(graph.frame, GROWN_ROOT_SLOT, owner)
                .expect("root the grown owner into the current gray");
            assert!(parked.marked(owner));
            charge_white(
                parked,
                owner.slot(),
                record,
                &[graph.value, graph.successor],
            );
            let before = parked.meter();
            parked
                .with_saved_heap(|heap| heap.set_named_scalar(graph.old_mt, "__mode", Value::Null))
                .expect("delete the original direct mode in place");
            assert_eq!(
                parked.meter().mode_cache_updates,
                before.mode_cache_updates + 1
            );
            assert_eq!(parked.discovery_slot(), Some(0));
            let mut reached = false;
            for _ in 0..512 {
                if parked.discovery_slot().is_some_and(|slot| slot > old_slots) {
                    reached = true;
                    break;
                }
                assert!(!parked.pulse(1).finished);
            }
            assert!(
                reached,
                "the probe must read the slot allocated after ModeInit"
            );
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let owner = grown_owner.expect("grown owner");
            assert_eq!(heap.slots.len() as u32, old_slots + 1);
            assert!(heap.alive(owner));
            assert_eq!(heap.named_edge(owner, GROWN_ITEM)?, Some(graph.value));
            assert!(
                heap.alive(graph.value),
                "the grown owner's weak value follows the rescan, not the setter"
            );
            assert!(heap.alive(graph.successor));
            assert!(heap.alive(graph.left_value));
            assert!(heap.alive(graph.left_successor));
            assert!(heap.alive(graph.peer_value));
            assert!(
                heap.alive(graph.orphan),
                "the witness shares the deleted metatable and becomes strong"
            );
            assert_eq!(
                heap.named_edge(graph.witness, "orphan")?,
                Some(graph.orphan)
            );
            assert!(!heap.alive(graph.dust));
            Ok(())
        })
        .expect("grown owner slots");
    });
}

const ORPHAN_SCALAR: &str = "303";

/// One weak-key owner on each side plus an independent weak-only orphan.
/// The left and right owners share the initial `k` or `kv` metatable, their
/// keys are bare userdata with distinct scalar values, and no value points
/// back at any key. The witness observes all three userdata identities only
/// through a `v` metatable, so it adds no strong root. The orphan table holds
/// the third userdata under the same initial weak metatable. A dust table is
/// unrooted and proves that this cycle really sweeps.
struct BareKeyGraph {
    sentinel: TableRef,
    left: TableRef,
    right: TableRef,
    witness: TableRef,
    orphan_table: TableRef,
    k_mt: TableRef,
    v_mt: TableRef,
    strong_mt: TableRef,
    left_key: UserdataRef,
    right_key: UserdataRef,
    orphan: UserdataRef,
    dust: TableRef,
    old_mt: TableRef,
    key_record: u32,
    sentinel_record: u32,
}

fn plant_bare_key_graph(heap: &mut LuaHeap, mode: &str) -> Result<BareKeyGraph, HeapError> {
    let sentinel = heap.alloc()?;
    let left = heap.alloc()?;
    let right = heap.alloc()?;
    let witness = heap.alloc()?;
    let orphan_table = heap.alloc()?;
    let k_mt = heap.alloc()?;
    let v_mt = heap.alloc()?;
    let kv_mt = heap.alloc()?;
    let strong_mt = heap.alloc()?;
    let left_key = heap.alloc_userdata()?;
    let right_key = heap.alloc_userdata()?;
    let orphan = heap.alloc_userdata()?;
    let dust = heap.alloc()?;
    assert_ne!(left_key, right_key, "left and right keys share an identity");
    assert_ne!(left_key, orphan, "left and orphan keys share an identity");
    assert_ne!(right_key, orphan, "right and orphan keys share an identity");
    heap.set_mode_scalar(k_mt, Value::string("k"))?;
    heap.set_mode_scalar(v_mt, Value::string("v"))?;
    heap.set_mode_scalar(kv_mt, Value::string("kv"))?;
    heap.set_named_scalar(strong_mt, "tag", Value::string("preallocated"))?;
    let old_mt = match mode {
        "k" => k_mt,
        "kv" => kv_mt,
        other => panic!("unexpected weak mode {other}"),
    };
    heap.set_metatable(left, Some(old_mt))?;
    heap.set_metatable(right, Some(old_mt))?;
    heap.set_metatable(witness, Some(v_mt))?;
    heap.set_metatable(orphan_table, Some(old_mt))?;
    heap.set_userdata_key_scalar(left, left_key, Value::string(LEFT_SCALAR))?;
    heap.set_userdata_key_scalar(right, right_key, Value::string(RIGHT_SCALAR))?;
    heap.set_userdata_key_scalar(orphan_table, orphan, Value::string(ORPHAN_SCALAR))?;
    heap.set_number_userdata(witness, 1.0, left_key)?;
    heap.set_number_userdata(witness, 2.0, right_key)?;
    heap.set_number_userdata(witness, 3.0, orphan)?;
    heap.set_named_scalar(sentinel, "pending", Value::Bool(true))?;
    let frame = heap.enter_roots(9)?;
    // Last written root is scanned first. The sentinel stays pending until
    // every owner written after it has been scanned.
    heap.write_root(frame, 0, sentinel)?;
    heap.write_root(frame, 1, right)?;
    heap.write_root(frame, 2, witness)?;
    heap.write_root(frame, 3, orphan_table)?;
    heap.write_root(frame, 4, strong_mt)?;
    heap.write_root(frame, 5, v_mt)?;
    heap.write_root(frame, 6, k_mt)?;
    heap.write_root(frame, 7, kv_mt)?;
    heap.write_root(frame, 8, left)?;
    assert_userdata_unrooted(heap, left_key);
    assert_userdata_unrooted(heap, right_key);
    assert_userdata_unrooted(heap, orphan);
    let key_record = heap
        .find_key(left.slot(), &super::EdgeKey::userdata_key(left_key))
        .expect("left userdata-key record");
    let (_, sentinel_record) = heap
        .locate_named(sentinel.slot(), "pending")
        .expect("sentinel record");
    let _ = frame;
    Ok(BareKeyGraph {
        sentinel,
        left,
        right,
        witness,
        orphan_table,
        k_mt,
        v_mt,
        strong_mt,
        left_key,
        right_key,
        orphan,
        dust,
        old_mt,
        key_record,
        sentinel_record,
    })
}

fn open_bare_key_graph(host: &mut vm::HostContext<'_>, mode: &str) -> BareKeyGraph {
    heap::install(host, limits(20, 12, 24)).expect("install");
    let graph = heap::with_heap(host, |heap| plant_bare_key_graph(heap, mode)).expect("plant");
    heap::open_fixed_step(host).expect("open");
    graph
}

/// The rawget-shaped typed read plus the next-shaped visible-entry walk.
///
/// This heap has no guest carrier for a bare userdata key, so `next_guest`
/// returns `NextUnsupported` for that record. The test therefore checks the
/// same visible facts at the typed boundary: `userdata_key_value` is the
/// rawget-analogue, `find_key` keeps the original slot/generation/kind
/// identity, and the owner has exactly one visible record whose stored key is
/// that identity and whose scalar is unchanged. It does not use a fixed scan
/// order and does not claim a guest `next`.
fn see_userdata_key_scalar(
    parked: &mut ParkedMark<'_>,
    owner: TableRef,
    key: UserdataRef,
    scalar: &str,
) -> bool {
    parked.with_saved_heap(|heap| {
        let Ok(found) = heap.userdata_key_value(owner, key) else {
            return false;
        };
        if found != Value::string(scalar) {
            return false;
        }
        if heap
            .find_key(owner.slot(), &super::EdgeKey::userdata_key(key))
            .is_none()
        {
            return false;
        }
        let Some(stored) = heap.slots.get(owner.slot() as usize) else {
            return false;
        };
        let mut cursor = stored.edge_head;
        let mut visible = 0;
        while let Some(index) = cursor {
            let record = &heap.edge_slots[index as usize];
            cursor = record.next;
            let Some(edge) = record.edge.as_ref() else {
                continue;
            };
            if edge.value.is_cursor_gap() {
                continue;
            }
            visible += 1;
            if edge.key != super::EdgeKey::userdata_key(key) {
                return false;
            }
            let super::EdgeValue::Scalar(Value::String(text)) = &edge.value else {
                return false;
            };
            if text.as_str() != scalar {
                return false;
            }
        }
        visible == 1
    })
}

/// Charges the left owner's only physical edge and proves that the weak scan
/// neither marks the userdata key nor runs out of independent work. The
/// sentinel is the independent record that stays pending after the charge.
fn prove_bare_key_white(parked: &mut ParkedMark<'_>, graph: &BareKeyGraph) {
    let white = [graph.left_key, graph.right_key, graph.orphan];
    let pending = pause_record(parked, graph.left.slot(), graph.key_record);
    assert_eq!(pending.owner, graph.left.slot());
    assert_eq!(pending.cursor, graph.key_record);
    let edges = parked.meter().outgoing_edge_checks;
    let gaps = parked.meter().outgoing_gap_checks;
    let view = parked.pulse(1);
    assert!(
        !view.finished,
        "the weak-key charge must leave independent work"
    );
    assert_eq!(
        parked.meter().outgoing_edge_checks,
        edges + 1,
        "the physical weak-key record was budgeted once"
    );
    assert_eq!(parked.meter().outgoing_gap_checks, gaps);
    for key in white {
        assert!(
            !userdata_marked(parked, key),
            "the weak key scan marked userdata slot {}",
            key.slot()
        );
    }
    let pending = pause_record(parked, graph.sentinel.slot(), graph.sentinel_record);
    assert_eq!(pending.owner, graph.sentinel.slot());
    assert_eq!(pending.cursor, graph.sentinel_record);
    assert!(
        parked.marked(graph.left),
        "the owner is marked, which alone does not prove the key record was charged"
    );
    for key in white {
        assert!(
            !userdata_marked(parked, key),
            "independent pending work marked userdata slot {}",
            key.slot()
        );
    }
    parked.with_saved_heap(|heap| {
        assert_userdata_unrooted(heap, graph.left_key);
        assert_userdata_unrooted(heap, graph.right_key);
        assert_userdata_unrooted(heap, graph.orphan);
        assert_eq!(
            heap.metatable(graph.right).expect("right metatable"),
            Some(graph.old_mt)
        );
        assert_eq!(
            heap.metatable(graph.left).expect("left metatable"),
            Some(graph.old_mt)
        );
        assert_eq!(
            heap.userdata_key_value(graph.left, graph.left_key)
                .expect("left scalar"),
            Value::string(LEFT_SCALAR)
        );
        assert_eq!(
            heap.userdata_key_value(graph.right, graph.right_key)
                .expect("right scalar"),
            Value::string(RIGHT_SCALAR)
        );
        assert_eq!(
            heap.userdata_key_value(graph.orphan_table, graph.orphan)
                .expect("orphan scalar"),
            Value::string(ORPHAN_SCALAR)
        );
        assert_eq!(
            heap.number_userdata(graph.witness, 1.0)
                .expect("witness left"),
            Some(graph.left_key)
        );
        assert_eq!(
            heap.number_userdata(graph.witness, 2.0)
                .expect("witness right"),
            Some(graph.right_key)
        );
        assert_eq!(
            heap.number_userdata(graph.witness, 3.0)
                .expect("witness orphan"),
            Some(graph.orphan)
        );
    });
}

/// Runs one real metatable setter after the owner was weak-scanned. The
/// setter reserves a chain-head rescan but must not move the ordinary cursor,
/// open the rescan, add a root, or mark/clear the original key by itself.
fn replace_bare_key(
    parked: &mut ParkedMark<'_>,
    graph: &BareKeyGraph,
    replacement: Option<TableRef>,
) {
    let pending = parked.pending();
    let before = parked.meter();
    let roots = parked.stats().live_roots;
    write_metatable(parked, graph.left, replacement).expect("replace the left metatable");
    assert_setter_reserved(parked, pending, &before, roots);
    assert_eq!(
        parked.meter().owner_rescan_heads,
        before.owner_rescan_heads,
        "the setter does not open the rescan"
    );
    assert_eq!(
        parked.meter().owner_rescan_schedules,
        before.owner_rescan_schedules + 1,
        "one real setter schedules one chain-head rescan"
    );
    assert!(
        !userdata_marked(parked, graph.left_key),
        "the setter did not synchronously mark the userdata key"
    );
    assert!(see_userdata_key_scalar(
        parked,
        graph.left,
        graph.left_key,
        LEFT_SCALAR
    ));
    assert!(see_userdata_key_scalar(
        parked,
        graph.right,
        graph.right_key,
        RIGHT_SCALAR
    ));
    assert!(see_userdata_key_scalar(
        parked,
        graph.orphan_table,
        graph.orphan,
        ORPHAN_SCALAR
    ));
    let right_mt = parked.with_saved_heap(|heap| heap.metatable(graph.right));
    assert!(matches!(right_mt, Ok(Some(mt)) if mt == graph.old_mt));
    let left_mt = parked.with_saved_heap(|heap| heap.metatable(graph.left));
    match replacement {
        Some(mt) => assert!(matches!(left_mt, Ok(Some(found)) if found == mt)),
        None => assert!(matches!(left_mt, Ok(None))),
    }
}

/// Pauses on the rescan record that the setter scheduled, charges exactly one
/// edge, and requires the key marking to follow the new weak mode only. A
/// `k`-to-`k` replacement still gets a rescan because its value side changed,
/// but its key stays weak and must remain white.
fn charge_bare_key_rescan(parked: &mut ParkedMark<'_>, graph: &BareKeyGraph, follows_key: bool) {
    let pending = pause_on_rescan(parked, graph.left.slot(), graph.key_record);
    assert_eq!(pending.owner, graph.left.slot());
    assert_eq!(pending.cursor, graph.key_record);
    assert!(
        !userdata_marked(parked, graph.left_key),
        "the rescan cursor has not charged the userdata key yet"
    );
    let checks = parked.meter().outgoing_edge_checks;
    let _view = parked.pulse(1);
    assert_eq!(
        parked.meter().outgoing_edge_checks,
        checks + 1,
        "the rescan charged exactly one physical edge for the key record"
    );
    assert!(
        parked.pending_rescan().is_none(),
        "the key record was consumed by the rescan pulse"
    );
    if follows_key {
        assert!(
            userdata_marked(parked, graph.left_key),
            "the budgeted rescan did not mark the strengthened userdata key"
        );
    } else {
        assert!(
            !userdata_marked(parked, graph.left_key),
            "k still leaves the userdata key weak during the rescan"
        );
    }
    assert!(see_userdata_key_scalar(
        parked,
        graph.left,
        graph.left_key,
        LEFT_SCALAR
    ));
}

fn finish_bare_key_cycle(host: &mut vm::HostContext<'_>, graph: &BareKeyGraph, keep: bool) {
    let done = finish_fixed_cycle(host);
    assert!(
        matches!(done, crate::heap::StepOutcome::Completed(_)),
        "{done:?}"
    );
    assert!(heap::fixed_step_idle(host).expect("idle"));
    heap::with_heap(host, |heap| -> Result<(), HeapError> {
        assert!(
            !heap.userdata_alive(graph.right_key),
            "the sibling weak key clears"
        );
        assert!(heap.next_guest(graph.right, &Value::Null)?.is_none());
        assert!(
            !heap.userdata_alive(graph.orphan),
            "the independent orphan clears"
        );
        assert!(heap.next_guest(graph.orphan_table, &Value::Null)?.is_none());
        assert_eq!(heap.number_userdata(graph.witness, 2.0)?, None);
        assert_eq!(heap.number_userdata(graph.witness, 3.0)?, None);
        assert!(!heap.alive(graph.dust), "sweep released an unrooted node");
        if keep {
            assert!(
                heap.userdata_alive(graph.left_key),
                "the strengthened key stays"
            );
            assert_eq!(
                heap.userdata_key_value(graph.left, graph.left_key)?,
                Value::string(LEFT_SCALAR)
            );
            assert_eq!(
                heap.number_userdata(graph.witness, 1.0)?,
                Some(graph.left_key)
            );
            assert!(
                heap.find_key(
                    graph.left.slot(),
                    &super::EdgeKey::userdata_key(graph.left_key)
                )
                .is_some()
            );
        } else {
            assert!(
                !heap.userdata_alive(graph.left_key),
                "the weak key clears this cycle"
            );
            assert_eq!(
                heap.userdata_key_value(graph.left, graph.left_key),
                Err(HeapError::StaleHandle)
            );
            assert!(heap.next_guest(graph.left, &Value::Null)?.is_none());
            assert_eq!(heap.number_userdata(graph.witness, 1.0)?, None);
        }
        Ok(())
    })
    .expect("this cycle");
    if keep {
        heap::with_heap(host, |heap| -> Result<(), HeapError> {
            heap.set_metatable(graph.left, Some(graph.old_mt))?;
            assert!(
                heap.userdata_alive(graph.left_key),
                "restoring k does not drop the key"
            );
            assert_eq!(
                heap.userdata_key_value(graph.left, graph.left_key)?,
                Value::string(LEFT_SCALAR)
            );
            Ok(())
        })
        .expect("restore the weak mode");
        heap::collect(host).expect("next cycle");
        heap::with_heap(host, |heap| {
            assert!(
                !heap.userdata_alive(graph.left_key),
                "the next cycle releases the kept key"
            );
            assert_eq!(
                heap.userdata_key_value(graph.left, graph.left_key),
                Err(HeapError::StaleHandle)
            );
            assert_eq!(heap.number_userdata(graph.witness, 1.0)?, None);
            Ok(())
        })
        .expect("released");
    } else {
        heap::with_heap(host, |heap| -> Result<(), HeapError> {
            heap.set_metatable(graph.left, Some(graph.strong_mt))?;
            Ok(())
        })
        .expect("late strong metatable");
        heap::collect(host).expect("collect after a cleared key");
        heap::with_heap(host, |heap| {
            assert!(
                !heap.userdata_alive(graph.left_key),
                "a cleared key does not come back"
            );
            assert!(heap.next_guest(graph.left, &Value::Null)?.is_none());
            Ok(())
        })
        .expect("no resurrection");
    }
}

#[derive(Clone, Copy)]
enum BareKeyArm {
    KSwap,
    KClear,
    KOmit,
    KvToValue,
    KvToKey,
    KvOmit,
}

#[derive(Clone, Copy)]
enum BareKeyChange {
    Omit,
    Clear,
    Replace(TableRef),
}

impl BareKeyArm {
    fn mode(self) -> &'static str {
        match self {
            Self::KSwap | Self::KClear | Self::KOmit => "k",
            Self::KvToValue | Self::KvToKey | Self::KvOmit => "kv",
        }
    }

    fn keeps_key(self) -> bool {
        matches!(self, Self::KSwap | Self::KClear | Self::KvToValue)
    }
}

fn run_bare_key_arm(arm: BareKeyArm) {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = open_bare_key_graph(&mut host, arm.mode());
        let change = match arm {
            BareKeyArm::KSwap => BareKeyChange::Replace(graph.strong_mt),
            BareKeyArm::KClear => BareKeyChange::Clear,
            BareKeyArm::KOmit | BareKeyArm::KvOmit => BareKeyChange::Omit,
            BareKeyArm::KvToValue => BareKeyChange::Replace(graph.v_mt),
            BareKeyArm::KvToKey => BareKeyChange::Replace(graph.k_mt),
        };
        resume_weak(&mut host, |parked| {
            prove_bare_key_white(parked, &graph);
            match change {
                BareKeyChange::Omit => {
                    assert_eq!(parked.meter().owner_rescan_schedules, 0);
                }
                BareKeyChange::Clear => {
                    replace_bare_key(parked, &graph, None);
                    charge_bare_key_rescan(parked, &graph, arm.keeps_key());
                }
                BareKeyChange::Replace(mt) => {
                    replace_bare_key(parked, &graph, Some(mt));
                    charge_bare_key_rescan(parked, &graph, arm.keeps_key());
                }
            }
        });
        finish_bare_key_cycle(&mut host, &graph, arm.keeps_key());
    });
}

#[test]
fn gray_scanned_k_swap_keeps_the_bare_userdata_key() {
    run_bare_key_arm(BareKeyArm::KSwap);
}

#[test]
fn gray_scanned_k_clear_keeps_the_bare_userdata_key() {
    run_bare_key_arm(BareKeyArm::KClear);
}

#[test]
fn gray_scanned_k_omit_clears_the_bare_userdata_key() {
    run_bare_key_arm(BareKeyArm::KOmit);
}

#[test]
fn gray_scanned_kv_to_value_keeps_the_bare_userdata_key() {
    run_bare_key_arm(BareKeyArm::KvToValue);
}

#[test]
fn gray_scanned_kv_to_key_clears_the_bare_userdata_key() {
    run_bare_key_arm(BareKeyArm::KvToKey);
}

#[test]
fn gray_scanned_kv_omit_clears_the_bare_userdata_key() {
    run_bare_key_arm(BareKeyArm::KvOmit);
}

// ---------------------------------------------------------------------------
// Mid-round shared `__mode = v` metatable: keep arm.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum KeepMetatable {
    Prebuilt,
    Grown,
}

/// LuaJIT keeps the current `__mode` of a shared metatable created after the
/// round started but before the owners are rescanned: both original values and
/// their successors clear in the same unfinished step. The grown arm is the
/// business witness: its slot is past the reset-time cache, so the setter must
/// register a budgeted late read instead of treating the new mode as strong.
/// The prebuilt arm is the unchanged positive control.
#[test]
fn shared_mode_keep_grown_metatable_v_clears_both_graphs() {
    keep_shared_mode_v_metatable(KeepMetatable::Grown);
}

/// Positive control. The metatable has the same shape and `v` field but exists
/// before ModeInit. It is deliberately not rooted: reset still parses every
/// live slot's mode edge, so its mode is cached without making it a strong
/// root. Switching both owners to it must not schedule a strong rescan.
#[test]
fn shared_mode_keep_prebuilt_metatable_v_clears_both_graphs() {
    keep_shared_mode_v_metatable(KeepMetatable::Prebuilt);
}

fn keep_shared_mode_v_metatable(arm: KeepMetatable) {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(16, 8, 24)).expect("install");
        let (graph, prebuilt) = heap::with_heap(
            &mut host,
            |heap| -> Result<(SharedModeGraph, Option<TableRef>), HeapError> {
                let graph = plant_shared_mode_graph(heap)?;
                let prebuilt = if matches!(arm, KeepMetatable::Prebuilt) {
                    let before = heap.stats().node_slots;
                    let mt = heap.alloc()?;
                    assert_eq!(
                        mt.slot(),
                        before,
                        "the prebuilt metatable is the next physical slot"
                    );
                    assert_eq!(heap.stats().node_slots, before + 1);
                    heap.set_mode_scalar(mt, Value::string("v"))?;
                    Some(mt)
                } else {
                    None
                };
                Ok((graph, prebuilt))
            },
        )
        .expect("plant");
        heap::open_fixed_step(&mut host).expect("open");

        let left_identity = (graph.left.slot(), graph.left.generation());
        let right_identity = (graph.right.slot(), graph.right.generation());
        assert_ne!(left_identity, right_identity, "owners are different tables");
        let mut old_slots = 0u32;
        let mut mt_slot = 0u32;
        let mut mt_generation = 0u32;
        let mut pending_at_pause = None;
        let mut meter_at_pause = None;
        // WRITE-BARRIER MARKER: the old values are already planted and weakly
        // charged. Moving these writes after the attach would mark them early
        // through `barrier_strong_field`'s mode read, not through this rescan.
        // The attach-then-write path has its own witness below.

        resume_weak(&mut host, |parked| {
            let pending = pause_after_both_weak_edges(parked, &graph);
            old_slots = parked.stats().node_slots;
            let mt = match arm {
                KeepMetatable::Prebuilt => {
                    let mt = prebuilt.expect("prebuilt metatable");
                    assert!(
                        mt.slot() < old_slots,
                        "the prebuilt metatable predates this round"
                    );
                    assert!(
                        !parked.marked(mt),
                        "the prebuilt metatable must not already be a strong root"
                    );
                    mt
                }
                KeepMetatable::Grown => {
                    let mt = grow_one_table(parked);
                    assert!(
                        !parked.marked(mt),
                        "a fresh allocation starts outside the strong roots"
                    );
                    assert_eq!(
                        mt.slot(),
                        old_slots,
                        "the grown metatable is the next physical slot"
                    );
                    assert_eq!(
                        parked.stats().node_slots,
                        old_slots + 1,
                        "the grown metatable grows the slot vector instead of reusing a free slot"
                    );
                    parked
                        .with_saved_heap(|heap| heap.set_mode_scalar(mt, Value::string("v")))
                        .expect("write the grown raw __mode = v");
                    mt
                }
            };
            mt_slot = mt.slot();
            mt_generation = mt.generation();
            assert_eq!(
                parked.with_saved_heap(|heap| heap.named_value(mt, "__mode")),
                Ok(Value::string("v")),
                "the metatable must carry a real raw __mode field"
            );
            assert!(
                parked.with_saved_heap(|heap| heap.locate_named(mt.slot(), "__mode").is_some()),
                "the raw __mode record must exist, not only a cached string"
            );
            assert_ne!(graph.left.slot(), mt_slot);
            assert_ne!(graph.right.slot(), mt_slot);
            assert_eq!(
                parked.with_saved_heap(|heap| heap.named_value(graph.witness_mt, "__mode")),
                Ok(Value::string("v")),
                "the independent observer is weak-valued"
            );
            assert_eq!(
                parked.with_saved_heap(|heap| heap.named_edge(graph.witness, "orphan")),
                Ok(Some(graph.orphan)),
                "the orphan is only referenced by the independent weak observer"
            );

            parked
                .with_saved_heap(|heap| -> Result<(), HeapError> {
                    heap.set_metatable(graph.left, Some(mt))?;
                    heap.set_metatable(graph.right, Some(mt))?;
                    Ok(())
                })
                .expect("switch both original owners to the kept metatable");
            for owner in [graph.left, graph.right] {
                let stored = parked
                    .metatable(owner)
                    .expect("metatable read")
                    .expect("owner metatable");
                assert_eq!(
                    (stored.slot(), stored.generation()),
                    (mt.slot(), mt.generation()),
                    "the owner must point at the kept metatable itself"
                );
            }
            assert_eq!(
                (graph.left.slot(), graph.left.generation()),
                left_identity,
                "the left owner keeps its original identity"
            );
            assert_eq!(
                (graph.right.slot(), graph.right.generation()),
                right_identity,
                "the right owner keeps its original identity"
            );
            assert_eq!(
                parked.pending().map(|scan| (scan.owner, scan.cursor)),
                Some((pending.owner, pending.cursor)),
                "the setter must not move the independent pending scan"
            );
            assert!(!parked.marked(graph.left_value));
            assert!(!parked.marked(graph.left_child));
            assert!(!parked.marked(graph.right_value));
            assert!(!parked.marked(graph.right_child));
            assert!(!parked.marked(graph.orphan));
            pending_at_pause = Some(pending);
            meter_at_pause = Some((
                parked.meter().mode_cache_updates,
                parked.meter().owner_rescan_schedules,
                parked.meter().weak_mode_lookups,
            ));
        });
        finish_shared_mode_cycle(&mut host);

        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let outcome = read_shared_mode_outcome(heap, &graph)?;
            let left_successor = if outcome.left_value {
                heap.named_edge(graph.left_value, "next")?
            } else {
                None
            };
            let right_successor = if outcome.right_value {
                heap.named_edge(graph.right_value, "next")?
            } else {
                None
            };
            assert_eq!(
                pending_at_pause.map(|pending| (pending.owner, pending.cursor)),
                Some((graph.sentinel.slot(), graph.sentinel_record)),
                "the pause must be the independent sentinel record"
            );
            assert_eq!(
                (
                    outcome.left_value,
                    outcome.left_child,
                    outcome.right_value,
                    outcome.right_child,
                    outcome.orphan,
                ),
                (false, false, false, false, false),
                "visible originals after the unfinished step: left_value={} left_subject={:?} left_successor={:?} right_value={} right_subject={:?} right_successor={:?} witness_left={:?} witness_right={:?} witness_orphan={:?} orphan_alive={} dust_alive={} pending_before={:?} meter_at_pause={:?} metatable_slot={} old_slots={}",
                outcome.left_value,
                outcome.left_edge,
                left_successor,
                outcome.right_value,
                outcome.right_edge,
                right_successor,
                outcome.witness_left,
                outcome.witness_right,
                outcome.witness_orphan,
                outcome.orphan,
                outcome.dust,
                pending_at_pause,
                meter_at_pause,
                mt_slot,
                old_slots,
            );
            assert_eq!(
                outcome.left_edge,
                None,
                "the left original value must clear in this cycle"
            );
            assert_eq!(
                outcome.right_edge,
                None,
                "the right original value must clear in this cycle"
            );
            assert_eq!(
                (outcome.left_child, outcome.right_child),
                (false, false),
                "both successors must clear with their values"
            );
            assert_eq!(
                (
                    outcome.witness_left,
                    outcome.witness_right,
                    outcome.witness_orphan,
                ),
                (None, None, None),
                "the independent observer must publish nil for both values and the orphan"
            );
            assert!(!outcome.orphan, "the independent weak orphan must clear");
            assert!(!outcome.dust, "unreachable dust must clear");

            assert_eq!(
                heap.slots.len() as u32,
                match arm {
                    KeepMetatable::Grown => old_slots + 1,
                    KeepMetatable::Prebuilt => old_slots,
                },
                "the round must not reuse a free slot for the metatable"
            );
            assert!(heap.alive(graph.left), "left owner stays");
            assert!(heap.alive(graph.right), "right owner stays");
            assert_eq!(
                (graph.left.slot(), graph.left.generation()),
                left_identity,
                "left owner identity survives the cycle"
            );
            assert_eq!(
                (graph.right.slot(), graph.right.generation()),
                right_identity,
                "right owner identity survives the cycle"
            );
            for owner in [graph.left, graph.right] {
                let stored = heap.metatable(owner)?.expect("owner metatable");
                assert_eq!(
                    (stored.slot(), stored.generation()),
                    (mt_slot, mt_generation),
                    "the owner still points at the kept metatable"
                );
                assert!(heap.alive(stored), "the kept metatable survives the cycle");
            }
            Ok(())
        })
        .expect("keep v cycle");
    });
}

// ---------------------------------------------------------------------------
// Mid-round shared `__mode = v`: first read of a not-yet-scanned owner.
// ---------------------------------------------------------------------------

struct LateReadGraph {
    gate: TableRef,
    gate_record: u32,
    target: TableRef,
    shared: TableRef,
    witness: TableRef,
    witness_mt: TableRef,
    value: TableRef,
    child: TableRef,
    orphan: TableRef,
    dust: TableRef,
}

/// The gate is rooted last, so its chain is scanned before the target's first
/// mode read. The target is already marked but has not opened its chain yet.
fn plant_late_read_graph(heap: &mut LuaHeap) -> Result<LateReadGraph, HeapError> {
    let gate = heap.alloc()?;
    let target = heap.alloc()?;
    let shared = heap.alloc()?;
    let witness = heap.alloc()?;
    let witness_mt = heap.alloc()?;
    let value = heap.alloc()?;
    let child = heap.alloc()?;
    let orphan = heap.alloc()?;
    let dust = heap.alloc()?;
    heap.set_mode_scalar(shared, Value::string("v"))?;
    heap.set_mode_scalar(witness_mt, Value::string("v"))?;
    heap.set_metatable(target, Some(shared))?;
    heap.set_metatable(witness, Some(witness_mt))?;
    heap.set_named_scalar(gate, "first", Value::Bool(true))?;
    heap.set_named_scalar(gate, "second", Value::Bool(true))?;
    heap.set_named_edge(target, "subj", value)?;
    heap.set_named_edge(value, "next", child)?;
    heap.set_named_edge(witness, "value", value)?;
    heap.set_named_edge(witness, "child", child)?;
    heap.set_named_edge(witness, "orphan", orphan)?;
    heap.issue_carrier(value)?;
    heap.issue_carrier(child)?;
    let frame = heap.enter_roots(3)?;
    // Gray is LIFO: the gate opens first, so the target has not been scanned.
    heap.write_root(frame, 0, target)?;
    heap.write_root(frame, 1, witness)?;
    heap.write_root(frame, 2, gate)?;
    let (_, gate_record) = heap
        .locate_named(gate.slot(), "second")
        .expect("gate second record");
    let _ = dust;
    Ok(LateReadGraph {
        gate,
        gate_record,
        target,
        shared,
        witness,
        witness_mt,
        value,
        child,
        orphan,
        dust,
    })
}

/// A first mode read must use the current `__mode` of a metatable that grew
/// after the cache was sized. The target is still in the gray queue, so the
/// first read happens after the attach. Its value and successor stay white and
/// clear in the same cycle. This is the complement of the already-scanned
/// rescan witness.
#[test]
fn shared_mode_keep_grown_metatable_v_first_read_clears_both_graphs() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(16, 8, 24)).expect("install");
        let graph = heap::with_heap(&mut host, plant_late_read_graph).expect("plant");
        heap::open_fixed_step(&mut host).expect("open");
        let mut mt_copy = None;
        resume_weak(&mut host, |parked| {
            pause_at(parked, graph.gate.slot(), graph.gate_record);
            assert_eq!(parked.phase_name(), "gray");
            assert_eq!(
                parked.pending().map(|scan| (scan.owner, scan.cursor)),
                Some((graph.gate.slot(), graph.gate_record)),
                "the gate is the current owner; the target is still queued"
            );
            assert!(parked.marked(graph.target), "the target root is marked");
            assert!(
                !parked.marked(graph.value),
                "the target has not charged its value yet"
            );
            assert_eq!(
                parked.with_saved_heap(|heap| heap.named_value(graph.witness_mt, "__mode")),
                Ok(Value::string("v")),
                "the independent observer stays weak-valued"
            );
            let mt = grow_one_table(parked);
            parked
                .with_saved_heap(|heap| heap.set_mode_scalar(mt, Value::string("v")))
                .expect("write the grown raw __mode = v");
            parked
                .with_saved_heap(|heap| heap.set_metatable(graph.target, Some(mt)))
                .expect("attach the grown metatable before the first read");
            assert_eq!(
                parked.with_saved_heap(|heap| heap.named_value(graph.shared, "__mode")),
                Ok(Value::string("v")),
                "the old metatable still has v; the new one is read later"
            );
            assert!(
                !parked.marked(graph.value),
                "the attach itself does not mark the uncharged value"
            );
            mt_copy = Some(mt);
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let mt = mt_copy.expect("grown metatable");
            assert_eq!(heap.metatable(graph.target)?, Some(mt));
            assert!(heap.alive(graph.target));
            assert!(
                !heap.alive(graph.value),
                "the first mode read must keep the value weak"
            );
            assert!(
                !heap.alive(graph.child),
                "the successor must clear with its weak value"
            );
            assert!(!heap.alive(graph.orphan), "the independent orphan clears");
            assert!(!heap.alive(graph.dust), "unreachable dust clears");
            assert_eq!(heap.named_edge(graph.target, "subj")?, None);
            assert_eq!(heap.named_edge(graph.witness, "value")?, None);
            assert_eq!(heap.named_edge(graph.witness, "child")?, None);
            assert_eq!(heap.named_edge(graph.witness, "orphan")?, None);
            assert!(
                heap.alive(mt),
                "the grown metatable is marked by the attach"
            );
            Ok(())
        })
        .expect("first mode read");
    });
}

// ---------------------------------------------------------------------------
// Mid-round shared `__mode = v`: attach, then a real missing-key barrier write.
// ---------------------------------------------------------------------------

const LATE_ITEM: &str = "late_item";

/// Both owners are already weakly scanned before the grown metatable is
/// attached. A missing ordinary key then receives a table value through the
/// real `write_from` insertion branch, which is the only caller of
/// `barrier_strong_field` there. The mode is still pending, so the barrier
/// must defer rather than mark the target early. The value stays white and
/// clears with its successor in the same cycle.
#[test]
fn shared_mode_keep_grown_metatable_v_after_attach_write_stays_white() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(20, 8, 32)).expect("install");
        let (graph, late_value, late_child) = heap::with_heap(
            &mut host,
            |heap| -> Result<(SharedModeGraph, TableRef, TableRef), HeapError> {
                let graph = plant_shared_mode_graph(heap)?;
                let late_value = heap.alloc()?;
                let late_child = heap.alloc()?;
                heap.set_named_edge(late_value, "next", late_child)?;
                Ok((graph, late_value, late_child))
            },
        )
        .expect("plant");
        heap::open_fixed_step(&mut host).expect("open");
        let mut mt_copy = None;
        resume_weak(&mut host, |parked| {
            pause_after_both_weak_edges(parked, &graph);
            assert!(
                !parked.marked(late_value) && !parked.marked(late_child),
                "the late target has no strong root and is still white"
            );
            let mt = grow_one_table(parked);
            parked
                .with_saved_heap(|heap| heap.set_mode_scalar(mt, Value::string("v")))
                .expect("write the grown raw __mode = v");
            parked
                .with_saved_heap(|heap| -> Result<(), HeapError> {
                    heap.set_metatable(graph.left, Some(mt))?;
                    heap.set_metatable(graph.right, Some(mt))?;
                    heap.set_named_edge(graph.left, LATE_ITEM, late_value)
                })
                .expect("attach both owners and insert the missing key");
            assert_eq!(
                parked.with_saved_heap(|heap| heap.named_edge(graph.left, LATE_ITEM)),
                Ok(Some(late_value)),
                "the real insertion stored the target"
            );
            assert!(
                !parked.marked(late_value),
                "a pending weak mode must not mark the inserted value early"
            );
            assert!(!parked.marked(late_child), "the barrier never cascades");
            mt_copy = Some(mt);
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let mt = mt_copy.expect("grown metatable");
            assert!(heap.alive(graph.left) && heap.alive(graph.right));
            assert_eq!(heap.metatable(graph.left)?, Some(mt));
            assert_eq!(heap.named_edge(graph.left, LATE_ITEM)?, None);
            assert!(
                !heap.alive(late_value),
                "the weak inserted value clears in this cycle"
            );
            assert!(!heap.alive(late_child), "its strong successor clears too");
            assert!(!heap.alive(graph.left_value) && !heap.alive(graph.left_child));
            assert!(!heap.alive(graph.right_value) && !heap.alive(graph.right_child));
            assert_eq!(heap.named_edge(graph.left, "subj")?, None);
            assert_eq!(heap.named_edge(graph.right, "subj")?, None);
            assert!(!heap.alive(graph.orphan));
            assert!(!heap.alive(graph.dust));
            Ok(())
        })
        .expect("attach write stays white");
    });
}

/// Same missing-key insertion entry with a real strong metatable. A grown
/// metatable without `__mode` needs no late read, so the barrier marks the
/// direct target immediately. The successor is not enqueued by the barrier;
/// the continued scan reaches it. Both owners are rescanned strong, so their
/// original graphs survive this cycle.
#[test]
fn shared_mode_keep_grown_strong_after_attach_write_marks_target_only() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(20, 8, 32)).expect("install");
        let (graph, late_value, late_child) = heap::with_heap(
            &mut host,
            |heap| -> Result<(SharedModeGraph, TableRef, TableRef), HeapError> {
                let graph = plant_shared_mode_graph(heap)?;
                let late_value = heap.alloc()?;
                let late_child = heap.alloc()?;
                heap.set_named_edge(late_value, "next", late_child)?;
                Ok((graph, late_value, late_child))
            },
        )
        .expect("plant");
        heap::open_fixed_step(&mut host).expect("open");
        let mut mt_copy = None;
        resume_weak(&mut host, |parked| {
            pause_after_both_weak_edges(parked, &graph);
            let schedules_before = parked.meter().owner_rescan_schedules;
            let mt = grow_one_table(parked);
            assert!(
                parked
                    .with_saved_heap(|heap| heap.locate_named(mt.slot(), "__mode"))
                    .is_none(),
                "the strong control has no raw __mode field"
            );
            parked
                .with_saved_heap(|heap| -> Result<(), HeapError> {
                    heap.set_metatable(graph.left, Some(mt))?;
                    heap.set_metatable(graph.right, Some(mt))?;
                    heap.set_named_edge(graph.left, LATE_ITEM, late_value)
                })
                .expect("attach both owners and insert the missing key");
            assert_eq!(
                parked.meter().owner_rescan_schedules,
                schedules_before + 2,
                "both already-started owners are scheduled synchronously"
            );
            assert!(
                parked.marked(late_value),
                "a known strong mode marks the direct target immediately"
            );
            assert!(
                !parked.marked(late_child),
                "the immediate barrier does not cascade to the successor"
            );
            mt_copy = Some(mt);
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let mt = mt_copy.expect("grown metatable");
            assert_eq!(heap.metatable(graph.left)?, Some(mt));
            assert_eq!(heap.metatable(graph.right)?, Some(mt));
            assert_eq!(heap.named_edge(graph.left, LATE_ITEM)?, Some(late_value));
            assert!(
                heap.alive(late_value),
                "the strongly inserted value survives"
            );
            assert!(
                heap.alive(late_child),
                "the continued scan reaches its strong successor"
            );
            assert!(heap.alive(graph.left_value) && heap.alive(graph.left_child));
            assert!(heap.alive(graph.right_value) && heap.alive(graph.right_child));
            assert_eq!(heap.named_edge(graph.left, "subj")?, Some(graph.left_value));
            assert_eq!(
                heap.named_edge(graph.right, "subj")?,
                Some(graph.right_value)
            );
            assert!(!heap.alive(graph.orphan));
            assert!(!heap.alive(graph.dust));
            Ok(())
        })
        .expect("strong attach write");
    });
}

// ---------------------------------------------------------------------------
// Mid-round shared `__mode = v`: delete after the grown mode has been parsed.
// ---------------------------------------------------------------------------

/// Unlike the older grown-delete arm, this witness lets the budgeted late read
/// finish before unlinking the direct field. The cache and the discovery
/// sidecar now cover the grown slot, so the delete must update the cache, arm
/// the indexed probe at that slot, and let discovery queue both already-started
/// owners for a strong rescan. Their original values and successors survive,
/// and no indexed access is out of bounds.
#[test]
fn shared_mode_delete_parsed_grown_metatable_rescans_owners() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(20, 8, 32)).expect("install");
        let graph = heap::with_heap(&mut host, plant_shared_mode_graph).expect("plant");
        heap::open_fixed_step(&mut host).expect("open");
        let mut mt_copy = None;
        let mut cache_updates_before = 0u64;
        let mut discover_writes_before = 0u64;
        let mut schedules_before_delete = 0u64;
        resume_weak(&mut host, |parked| {
            pause_after_both_weak_edges(parked, &graph);
            let mt = grow_one_table(parked);
            parked
                .with_saved_heap(|heap| heap.set_mode_scalar(mt, Value::string("v")))
                .expect("write the grown raw __mode = v");
            parked
                .with_saved_heap(|heap| -> Result<(), HeapError> {
                    heap.set_metatable(graph.left, Some(mt))?;
                    heap.set_metatable(graph.right, Some(mt))?;
                    Ok(())
                })
                .expect("attach both owners to the grown v");
            let bytes_before = parked.meter().weak_mode_bytes;
            let short_mode_bytes = long_mode_read_bytes("v");
            let mut pulses = 0u32;
            loop {
                let view = parked.pulse(1);
                assert!(
                    !view.finished,
                    "the cycle must not finish before the late read: pulse={pulses} bytes={} registrations={} phase={}",
                    parked.meter().weak_mode_bytes - bytes_before,
                    parked.meter().mode_late_registrations,
                    parked.phase_name()
                );
                if parked.meter().weak_mode_bytes - bytes_before >= short_mode_bytes {
                    break;
                }
                pulses += 1;
                assert!(pulses < 256, "the short late mode did not complete");
            }
            assert_eq!(
                parked.meter().weak_mode_bytes - bytes_before,
                short_mode_bytes,
                "v is the whole late parse; its length ends the read"
            );
            assert_eq!(
                parked.with_saved_heap(|heap| heap.named_value(mt, "__mode")),
                Ok(Value::string("v"))
            );
            let schedules_before = parked.meter().owner_rescan_schedules;
            let deferred_before = parked.meter().mode_late_deferred;
            pulse_until_late_quiet(parked);
            assert_eq!(
                parked.meter().owner_rescan_schedules,
                schedules_before,
                "the deferred v -> v owner checks must not schedule a strong rescan"
            );
            assert_eq!(
                parked.meter().mode_late_deferred - deferred_before,
                2,
                "both deferred owner checks finished while the metatable was still v"
            );
            assert!(!parked.marked(graph.left_value));
            assert!(!parked.marked(graph.right_value));
            schedules_before_delete = schedules_before;
            cache_updates_before = parked.meter().mode_cache_updates;
            discover_writes_before = parked.meter().mode_discover_writes;
            parked
                .with_saved_heap(|heap| heap.set_named_scalar(mt, "__mode", Value::Null))
                .expect("delete the parsed grown direct mode in place");
            assert_eq!(
                parked.meter().mode_cache_updates,
                cache_updates_before + 1,
                "the parsed grown cache entry accepts the direct-delete update"
            );
            assert_eq!(
                parked.meter().mode_discover_writes,
                discover_writes_before + 1,
                "the discovery stamp is written at the grown slot, not skipped"
            );
            assert_eq!(
                parked.discovery_slot(),
                Some(0),
                "the indexed probe starts at the first slot"
            );
            assert!(
                parked.with_saved_heap(|heap| heap.locate_named(mt.slot(), "__mode").is_none()),
                "the grown mode record is unlinked, not left as a gap"
            );
            assert!(!parked.marked(graph.left_value));
            assert!(!parked.marked(graph.right_value));
            mt_copy = Some(mt);
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let mt = mt_copy.expect("grown metatable");
            assert!(heap.alive(graph.left) && heap.alive(graph.right));
            assert_eq!(heap.metatable(graph.left)?, Some(mt));
            assert_eq!(heap.metatable(graph.right)?, Some(mt));
            assert!(heap.alive(graph.left_value) && heap.alive(graph.left_child));
            assert!(heap.alive(graph.right_value) && heap.alive(graph.right_child));
            assert_eq!(heap.named_edge(graph.left, "subj")?, Some(graph.left_value));
            assert_eq!(heap.named_edge(graph.right, "subj")?, Some(graph.right_value));
            assert!(!heap.alive(graph.orphan));
            assert!(!heap.alive(graph.dust));
            let meter = heap.scan_meter();
            assert!(meter.mode_owner_probes > 0, "the discovery walk ran");
            assert!(
                meter.owner_rescan_heads >= 2,
                "both owners were rescanned; the probe may revisit a slot already rescheduled by the late check"
            );
            assert_eq!(
                meter.owner_rescan_schedules,
                schedules_before_delete + 2,
                "the discovery probe schedules each intended owner exactly once"
            );
            assert_eq!(meter.mode_cache_updates, cache_updates_before + 1);
            assert_eq!(meter.mode_discover_writes, discover_writes_before + 1);
            Ok(())
        })
        .expect("parsed grown delete");
    });
}

// ---------------------------------------------------------------------------
// Mid-round shared `__mode` with a long prefix: NUL truncation and budget.
// ---------------------------------------------------------------------------

const LONG_MODE_PAD: usize = 16_384;

/// Bytes the direct reader must inspect: through the first NUL, or through the
/// final byte when there is no NUL. A second NUL is never read.
fn long_mode_read_bytes(mode: &str) -> u64 {
    let bytes = mode.as_bytes();
    match bytes.iter().position(|byte| *byte == 0) {
        Some(at) => (at + 1) as u64,
        None => bytes.len() as u64,
    }
}

/// One arm: grow a metatable mid-round, write a long mode, attach both already
/// weakly scanned owners, and drive the late window with budget one. Every
/// pulse is checked separately for bytes, cache growth, discovery growth,
/// registration, edge, and head work. The setter must do none of the reads.
fn run_grown_long_mode(mode: &str, expect_weak: bool) {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(20, 8, 32)).expect("install");
        let graph = heap::with_heap(&mut host, plant_shared_mode_graph).expect("plant");
        heap::open_fixed_step(&mut host).expect("open");
        let expected_bytes = long_mode_read_bytes(mode);
        let mut mt_copy = None;
        let mut old_slots = 0u32;
        let mut bytes_before_total = 0u64;
        let mut inited_before_total = 0u64;
        resume_weak(&mut host, |parked| {
            pause_after_both_weak_edges(parked, &graph);
            old_slots = parked.stats().node_slots;
            let before = parked.meter();
            bytes_before_total = before.weak_mode_bytes;
            inited_before_total = before.scratch.weak_modes_inited;
            let mt = grow_one_table(parked);
            assert_eq!(mt.slot(), old_slots);
            parked
                .with_saved_heap(|heap| heap.set_mode_scalar(mt, Value::string(mode)))
                .expect("write the long raw __mode");
            parked
                .with_saved_heap(|heap| -> Result<(), HeapError> {
                    heap.set_metatable(graph.left, Some(mt))?;
                    heap.set_metatable(graph.right, Some(mt))?;
                    Ok(())
                })
                .expect("attach both owners");
            let setter = parked.meter();
            assert_eq!(
                setter.weak_mode_bytes, before.weak_mode_bytes,
                "the setter must not read any mode byte"
            );
            assert_eq!(
                setter.scratch.weak_modes_inited, before.scratch.weak_modes_inited,
                "the setter must not grow the mode cache"
            );
            assert_eq!(
                setter.scratch.mode_discover_inited, before.scratch.mode_discover_inited,
                "the setter must not grow the discovery sidecar"
            );
            assert_eq!(
                setter.mode_late_registrations,
                before.mode_late_registrations + 1,
                "the setter registers exactly one late metatable"
            );
            assert_eq!(
                setter.mode_late_events,
                before.mode_late_events + 2,
                "each already-scanned owner gets one deferred transition check"
            );
            assert_eq!(
                setter.outgoing_edge_checks, before.outgoing_edge_checks,
                "the setter must not scan an edge"
            );
            assert_eq!(
                setter.owner_head_checks, before.owner_head_checks,
                "the setter must not open an owner"
            );

            let mut pulses = 0u32;
            loop {
                let before_pulse = parked.meter();
                let view = parked.pulse(1);
                let after = view.meter;
                let bytes = after.weak_mode_bytes - before_pulse.weak_mode_bytes;
                let inited =
                    after.scratch.weak_modes_inited - before_pulse.scratch.weak_modes_inited;
                let discover =
                    after.scratch.mode_discover_inited - before_pulse.scratch.mode_discover_inited;
                let registered =
                    after.mode_late_registrations - before_pulse.mode_late_registrations;
                let parsed = after.mode_late_parsed - before_pulse.mode_late_parsed;
                let deferred = after.mode_late_deferred - before_pulse.mode_late_deferred;
                let edges = after.outgoing_edge_checks - before_pulse.outgoing_edge_checks;
                let heads = after.owner_head_checks - before_pulse.owner_head_checks;
                assert!(
                    bytes <= 1
                        && inited <= 1
                        && discover <= 1
                        && registered <= 1
                        && parsed <= 1
                        && deferred <= 1
                        && edges <= 1
                        && heads <= 1,
                    "pulse {pulses} exceeded budget 1: bytes={bytes} init={inited} discover={discover} registered={registered} parsed={parsed} deferred={deferred} edges={edges} heads={heads}"
                );
                assert_eq!(after.sweep_edge_checks, 0, "a pulse must not sweep");
                if after.weak_mode_bytes - before.weak_mode_bytes >= expected_bytes {
                    break;
                }
                assert!(
                    !view.finished,
                    "the cycle finished before the late mode was read"
                );
                pulses += 1;
                assert!(
                    u64::from(pulses) < expected_bytes + 64,
                    "the late mode did not finish within its byte budget"
                );
            }
            let after = parked.meter();
            assert_eq!(
                after.weak_mode_bytes - before.weak_mode_bytes,
                expected_bytes,
                "the reader stops at the first NUL and never walks the suffix"
            );
            assert_eq!(
                after.scratch.weak_modes_inited - before.scratch.weak_modes_inited,
                0,
                "the late parse must not grow the indexed mode cache"
            );
            assert_eq!(
                after.scratch.mode_discover_inited - before.scratch.mode_discover_inited,
                0,
                "the late parse must not grow the discovery sidecar"
            );
            assert_eq!(
                after.mode_late_parsed - before.mode_late_parsed,
                1,
                "one parsed late slot is stored outside the reset-sized vector"
            );
            assert!(
                parked.with_saved_heap(|heap| heap.locate_named(mt.slot(), "__mode").is_some()),
                "the long raw field stays a real record"
            );
            assert_eq!(
                parked.with_saved_heap(|heap| heap.named_value(mt, "__mode")),
                Ok(Value::string(mode)),
                "the stored field bytes must not be truncated or rewritten"
            );
            mt_copy = Some(mt);
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let mt = mt_copy.expect("grown metatable");
            assert_eq!(heap.slots.len() as u32, old_slots + 1);
            assert_eq!(heap.metatable(graph.left)?, Some(mt));
            assert_eq!(
                heap.named_value(mt, "__mode"),
                Ok(Value::string(mode)),
                "the full original field survives the cycle"
            );
            let outcome = read_shared_mode_outcome(heap, &graph)?;
            if expect_weak {
                assert_eq!(
                    (
                        outcome.left_value,
                        outcome.left_child,
                        outcome.right_value,
                        outcome.right_child,
                    ),
                    (false, false, false, false),
                    "a v before the first NUL clears both graphs"
                );
                assert_eq!(outcome.left_edge, None);
                assert_eq!(outcome.right_edge, None);
                assert_eq!(
                    (
                        outcome.witness_left,
                        outcome.witness_right,
                        outcome.witness_orphan,
                    ),
                    (None, None, None)
                );
            } else {
                assert_eq!(
                    (
                        outcome.left_value,
                        outcome.left_child,
                        outcome.right_value,
                        outcome.right_child,
                    ),
                    (true, true, true, true),
                    "a NUL before v leaves the mode strong and keeps both graphs"
                );
                assert_eq!(outcome.left_edge, Some(graph.left_value));
                assert_eq!(outcome.right_edge, Some(graph.right_value));
                assert_eq!(outcome.witness_left, Some(graph.left_value));
                assert_eq!(outcome.witness_right, Some(graph.right_value));
            }
            assert!(!outcome.orphan, "the independent observer is still weak");
            assert_eq!(outcome.witness_orphan, None);
            assert!(!outcome.dust);
            assert!(heap.alive(mt));
            let meter = heap.scan_meter();
            assert_eq!(
                meter.weak_mode_bytes - bytes_before_total,
                expected_bytes,
                "sweep must not read or extend the long mode"
            );
            assert_eq!(
                meter.scratch.weak_modes_inited - inited_before_total,
                0,
                "no indexed slot is initialized by the late parse or sweep"
            );
            Ok(())
        })
        .expect("long mode arm");
    });
}

/// Three real long modes on the same fixture. `pad .. v` and `pad .. v .. NUL`
/// must clear (v before any NUL); `pad .. NUL .. v` must keep (the NUL ends the
/// interpretation). The field bytes stay exact in all three. The per-pulse
/// checks in `run_grown_long_mode` bound the late reader to one byte, one
/// parsed store, one deferred event, one registration, one edge, and one head,
/// and prove the late path does not grow the reset-sized index vectors.
#[test]
fn shared_mode_keep_grown_metatable_long_v_tail_is_budgeted() {
    let pad = "x".repeat(LONG_MODE_PAD);
    run_grown_long_mode(&format!("{pad}v"), true);
    run_grown_long_mode(&format!("{pad}v\0"), true);
    run_grown_long_mode(&format!("{pad}\0v"), false);
}

// ---------------------------------------------------------------------------
// Follow-up review: in-place `__mode` rewrites, owner transition, root batch,
// bounded late work, and deferred identity.
// ---------------------------------------------------------------------------

/// Drive one late parse with budget one until it has read exactly `want`
/// bytes. A late read never charges more than one byte per pulse.
fn pulse_until_late_bytes(parked: &mut ParkedMark<'_>, base: u64, want: u64) {
    let mut pulses = 0u32;
    while parked.meter().weak_mode_bytes - base < want {
        let view = parked.pulse(1);
        assert!(
            !view.finished,
            "the cycle finished before the late read reached {want} bytes"
        );
        pulses += 1;
        assert!(pulses < 512, "the late read did not reach {want} bytes");
    }
    assert_eq!(
        parked.meter().weak_mode_bytes - base,
        want,
        "one late byte per pulse"
    );
}

/// Run the saved mark until the late window has no mode or deferred work
/// left. The pulse that leaves the window also spends its unit on the
/// resumed roots/gray step, so the caller must not expect a byte count.
fn pulse_until_late_quiet(parked: &mut ParkedMark<'_>) {
    let mut pulses = 0u32;
    while parked.phase_name() == "mode-late" {
        let view = parked.pulse(1);
        assert!(!view.finished, "the cycle finished inside the late window");
        pulses += 1;
        assert!(pulses < 512, "the late window did not become quiet");
    }
}

fn attach_shared_metatable(parked: &mut ParkedMark<'_>, graph: &SharedModeGraph, mt: TableRef) {
    parked
        .with_saved_heap(|heap| -> Result<(), HeapError> {
            heap.set_metatable(graph.left, Some(mt))?;
            heap.set_metatable(graph.right, Some(mt))?;
            Ok(())
        })
        .expect("attach both owners");
}

#[derive(Clone, Copy, Debug)]
enum RewriteWhen {
    None,
    BeforePulse,
    AfterFirstByte,
    AfterQuiet,
    UnlinkReinsert,
}

/// One real grown metatable, one in-place `__mode` rewrite at the requested
/// phase, and the end-of-cycle outcome for both original graphs.
fn run_rewrite_arm(
    label: &'static str,
    initial: &'static str,
    rewritten: &'static str,
    when: RewriteWhen,
    expect_weak: bool,
) {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(20, 8, 32)).expect("install");
        let graph = heap::with_heap(&mut host, plant_shared_mode_graph).expect("plant");
        heap::open_fixed_step(&mut host).expect("open");
        let mut mt_copy = None;
        let mut bytes_before_rewrite = 0u64;
        resume_weak(&mut host, |parked| {
            pause_after_both_weak_edges(parked, &graph);
            let mt = grow_one_table(parked);
            parked
                .with_saved_heap(|heap| heap.set_mode_scalar(mt, Value::string(initial)))
                .expect("write the initial raw __mode");
            attach_shared_metatable(parked, &graph, mt);
            let base = parked.meter().weak_mode_bytes;
            match when {
                RewriteWhen::None | RewriteWhen::BeforePulse => {}
                RewriteWhen::AfterFirstByte | RewriteWhen::UnlinkReinsert => {
                    pulse_until_late_bytes(parked, base, 1);
                    assert_eq!(
                        parked.phase_name(),
                        "mode-late",
                        "{label}: the rewrite must land in the in-flight parse"
                    );
                }
                RewriteWhen::AfterQuiet => {
                    pulse_until_late_bytes(parked, base, long_mode_read_bytes(initial));
                    pulse_until_late_quiet(parked);
                    assert_ne!(
                        parked.phase_name(),
                        "mode-late",
                        "{label}: the first parse must be cached before the rewrite"
                    );
                }
            }
            bytes_before_rewrite = parked.meter().weak_mode_bytes;
            let registrations_before_rewrite = parked.meter().mode_late_registrations;
            let old_record = parked
                .with_saved_heap(|heap| heap.locate_named(mt.slot(), "__mode"))
                .expect("the initial mode record");
            match when {
                RewriteWhen::None => {}
                RewriteWhen::UnlinkReinsert => {
                    parked
                        .with_saved_heap(|heap| -> Result<(), HeapError> {
                            heap.set_mode_scalar(mt, Value::Null)?;
                            assert!(
                                heap.locate_named(mt.slot(), "__mode").is_none(),
                                "{label}: the first record must really unlink"
                            );
                            heap.set_mode_scalar(mt, Value::string(rewritten))
                        })
                        .expect("unlink and reinsert the raw __mode");
                }
                _ => {
                    parked
                        .with_saved_heap(|heap| heap.set_mode_scalar(mt, Value::string(rewritten)))
                        .expect("rewrite the same raw __mode record");
                    let new_record = parked
                        .with_saved_heap(|heap| heap.locate_named(mt.slot(), "__mode"))
                        .expect("the rewritten mode record");
                    assert_eq!(
                        new_record, old_record,
                        "{label}: an in-place rewrite keeps the same record index"
                    );
                }
            }
            assert_eq!(
                parked.with_saved_heap(|heap| heap.named_value(mt, "__mode")),
                Ok(Value::string(rewritten)),
                "{label}: the stored field carries the final bytes"
            );
            if matches!(when, RewriteWhen::AfterQuiet) {
                assert_eq!(
                    parked.meter().mode_late_registrations,
                    registrations_before_rewrite,
                    "{label}: a cached mode rewrite does not queue a second parse"
                );
                assert_eq!(
                    parked.meter().weak_mode_bytes,
                    bytes_before_rewrite,
                    "{label}: a cached mode rewrite does not read the new bytes in this cycle"
                );
            }
            mt_copy = Some(mt);
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let mt = mt_copy.expect("grown metatable");
            assert_eq!(
                heap.named_value(mt, "__mode"),
                Ok(Value::string(rewritten)),
                "{label}: the final field bytes survive the cycle"
            );
            let outcome = read_shared_mode_outcome(heap, &graph)?;
            let cases = (
                outcome.left_value,
                outcome.left_child,
                outcome.right_value,
                outcome.right_child,
            );
            if expect_weak {
                assert_eq!(
                    cases,
                    (false, false, false, false),
                    "{label}: a value-weak final mode must clear both original graphs"
                );
                assert_eq!(outcome.left_edge, None, "{label}: left subj cleared");
                assert_eq!(outcome.right_edge, None, "{label}: right subj cleared");
            } else {
                assert_eq!(
                    cases,
                    (true, true, true, true),
                    "{label}: a strong final mode must keep both original graphs and successors"
                );
                assert_eq!(
                    outcome.left_edge,
                    Some(graph.left_value),
                    "{label}: left subj kept"
                );
                assert_eq!(
                    outcome.right_edge,
                    Some(graph.right_value),
                    "{label}: right subj kept"
                );
            }
            assert!(
                !outcome.orphan,
                "{label}: the independent weak observer still clears its orphan"
            );
            assert_eq!(
                heap.scan_meter().weak_mode_bytes - bytes_before_rewrite,
                if matches!(when, RewriteWhen::AfterQuiet) {
                    0
                } else {
                    long_mode_read_bytes(rewritten)
                },
                "{label}: the reader stops at the first NUL or at the final byte"
            );
            Ok(())
        })
        .expect("rewrite arm");
    });
}

#[test]
fn shared_mode_rewrite_vx_to_x_after_first_byte_keeps_both_graphs() {
    run_rewrite_arm(
        "vx -> x after the first byte",
        "vx",
        "x",
        RewriteWhen::AfterFirstByte,
        false,
    );
}

#[test]
fn shared_mode_rewrite_xx_to_vx_after_first_byte_clears_both_graphs() {
    run_rewrite_arm(
        "xx -> vx after the first byte",
        "xx",
        "vx",
        RewriteWhen::AfterFirstByte,
        true,
    );
}

#[test]
fn shared_mode_rewrite_vx_to_x_nul_v_after_first_byte_keeps_both_graphs() {
    run_rewrite_arm(
        "vx -> x NUL v after the first byte",
        "vx",
        "x\0v",
        RewriteWhen::AfterFirstByte,
        false,
    );
}

#[test]
fn shared_mode_rewrite_xx_to_vx_before_the_first_byte_clears_both_graphs() {
    run_rewrite_arm(
        "xx -> vx before the first byte",
        "xx",
        "vx",
        RewriteWhen::BeforePulse,
        true,
    );
}

#[test]
fn shared_mode_unlink_reinsert_x_after_first_byte_keeps_both_graphs() {
    run_rewrite_arm(
        "vx unlink reinsert x after the first byte",
        "vx",
        "x",
        RewriteWhen::UnlinkReinsert,
        false,
    );
}

#[test]
fn shared_mode_rewrite_cached_v_to_x_keeps_the_cached_weak_mode() {
    run_rewrite_arm("cached v -> x", "v", "x", RewriteWhen::AfterQuiet, true);
}

#[test]
fn shared_mode_grown_xx_metatable_keeps_both_graphs() {
    run_rewrite_arm(
        "grown xx without rewrite",
        "xx",
        "xx",
        RewriteWhen::None,
        false,
    );
}

// ---------------------------------------------------------------------------
// Owner transition through an unresolved weak metatable.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum TransitionArm {
    DirectStrong,
    LateOnly,
    LateThenStrong,
}

fn run_owner_transition(arm: TransitionArm) {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(20, 8, 32)).expect("install");
        let graph = heap::with_heap(&mut host, plant_shared_mode_graph).expect("plant");
        heap::open_fixed_step(&mut host).expect("open");
        let mut final_mt = None;
        resume_weak(&mut host, |parked| {
            pause_after_both_weak_edges(parked, &graph);
            let mt = match arm {
                TransitionArm::DirectStrong => {
                    attach_shared_metatable(parked, &graph, graph.strong_mt);
                    graph.strong_mt
                }
                TransitionArm::LateOnly | TransitionArm::LateThenStrong => {
                    let late = grow_one_table(parked);
                    parked
                        .with_saved_heap(|heap| heap.set_mode_scalar(late, Value::string("v")))
                        .expect("write the late v");
                    attach_shared_metatable(parked, &graph, late);
                    if matches!(arm, TransitionArm::LateThenStrong) {
                        parked
                            .with_saved_heap(|heap| -> Result<(), HeapError> {
                                heap.set_metatable(graph.left, Some(graph.strong_mt))?;
                                heap.set_metatable(graph.right, Some(graph.strong_mt))?;
                                Ok(())
                            })
                            .expect("move off the unresolved weak metatable before any pulse");
                    }
                    late
                }
            };
            final_mt = Some(if matches!(arm, TransitionArm::LateOnly) {
                mt
            } else {
                graph.strong_mt
            });
            assert_eq!(parked.metatable(graph.left), Ok(final_mt));
            assert_eq!(parked.metatable(graph.right), Ok(final_mt));
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let mt = final_mt.expect("final metatable");
            assert_eq!(heap.metatable(graph.left)?, Some(mt));
            assert_eq!(heap.metatable(graph.right)?, Some(mt));
            let outcome = read_shared_mode_outcome(heap, &graph)?;
            let cases = (
                outcome.left_value,
                outcome.left_child,
                outcome.right_value,
                outcome.right_child,
            );
            match arm {
                TransitionArm::DirectStrong | TransitionArm::LateThenStrong => {
                    assert_eq!(
                        cases,
                        (true, true, true, true),
                        "{arm:?}: the final strong metatable keeps both original graphs"
                    );
                    assert_eq!(outcome.left_edge, Some(graph.left_value));
                    assert_eq!(outcome.right_edge, Some(graph.right_value));
                }
                TransitionArm::LateOnly => {
                    assert_eq!(
                        cases,
                        (false, false, false, false),
                        "{arm:?}: the single late weak transition stays weak"
                    );
                    assert_eq!(outcome.left_edge, None);
                    assert_eq!(outcome.right_edge, None);
                }
            }
            assert!(!outcome.orphan, "{arm:?}: the independent orphan clears");
            Ok(())
        })
        .expect("transition arm");
    });
}

#[test]
fn shared_mode_owner_weak_late_then_strong_keeps_both_graphs() {
    run_owner_transition(TransitionArm::LateThenStrong);
}

#[test]
fn shared_mode_owner_weak_late_only_clears_both_graphs() {
    run_owner_transition(TransitionArm::LateOnly);
}

#[test]
fn shared_mode_owner_direct_strong_keeps_both_graphs() {
    run_owner_transition(TransitionArm::DirectStrong);
}

// ---------------------------------------------------------------------------
// Safepoint root batch at an active late-mode byte.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum BatchArm {
    WithBatch,
    WithoutBatch,
    StaleRejected,
}

fn run_late_root_batch(arm: BatchArm) {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(24, 8, 40)).expect("install");
        let graph = heap::with_heap(&mut host, plant_shared_mode_graph).expect("plant");
        heap::open_fixed_step(&mut host).expect("open");
        let mut target_copy = None;
        resume_weak(&mut host, |parked| {
            pause_after_both_weak_edges(parked, &graph);
            let target = grow_one_table(parked);
            let mt = grow_one_table(parked);
            parked
                .with_saved_heap(|heap| heap.set_mode_scalar(mt, Value::string("vv")))
                .expect("write the late mode");
            attach_shared_metatable(parked, &graph, mt);
            assert!(!parked.marked(target), "the batch target starts white");
            let base = parked.meter().weak_mode_bytes;
            pulse_until_late_bytes(parked, base, 1);
            assert_eq!(parked.phase_name(), "mode-late");
            match arm {
                BatchArm::WithBatch => {
                    let before = parked.meter();
                    let view = parked
                        .pulse_with_root_batch(1, &[super::Rooted::Table(target)])
                        .expect("a live white target is a valid batch");
                    assert_eq!(
                        view.meter.parked_root_batch_accepts,
                        before.parked_root_batch_accepts + 1,
                        "the same call accepts the submitted root once"
                    );
                    assert_eq!(
                        view.meter.weak_mode_bytes,
                        before.weak_mode_bytes + 1,
                        "the late byte is the unit this pulse charged"
                    );
                    assert!(
                        parked.marked(target),
                        "the accepted target is marked in the same call, before any gray step"
                    );
                }
                BatchArm::WithoutBatch => {
                    let before = parked.meter();
                    let view = parked.pulse(1);
                    assert_eq!(
                        view.meter.weak_mode_bytes,
                        before.weak_mode_bytes + 1,
                        "the late byte is the ordinary unit"
                    );
                    assert!(
                        !parked.marked(target),
                        "an unsubmitted white target stays white"
                    );
                }
                BatchArm::StaleRejected => {
                    let stale = grow_one_table(parked);
                    parked
                        .with_saved_heap(|heap| heap.rollback(stale))
                        .expect("rollback the stale target");
                    let before = parked.meter();
                    let phase = parked.phase_name();
                    let error =
                        match parked.pulse_with_root_batch(1, &[super::Rooted::Table(stale)]) {
                            Ok(_) => panic!("a stale root batch must be refused"),
                            Err(error) => error,
                        };
                    assert_eq!(error, HeapError::StaleHandle);
                    assert_eq!(
                        parked.meter().weak_mode_bytes,
                        before.weak_mode_bytes,
                        "a refused batch does not spend the byte unit"
                    );
                    assert_eq!(
                        parked.meter().parked_root_batch_accepts,
                        before.parked_root_batch_accepts,
                        "a refused batch does not count as accepted"
                    );
                    assert_eq!(
                        parked.phase_name(),
                        phase,
                        "a refusal does not move the phase"
                    );
                    let view = parked.pulse(1);
                    assert!(!view.finished);
                }
            }
            target_copy = Some(target);
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let target = target_copy.expect("batch target");
            match arm {
                BatchArm::WithBatch => {
                    assert!(
                        heap.alive(target),
                        "the accepted root survives without repeating the batch"
                    );
                }
                BatchArm::WithoutBatch | BatchArm::StaleRejected => {
                    assert!(!heap.alive(target), "an unaccepted white target clears");
                }
            }
            let outcome = read_shared_mode_outcome(heap, &graph)?;
            assert_eq!(
                (
                    outcome.left_value,
                    outcome.left_child,
                    outcome.right_value,
                    outcome.right_child,
                ),
                (false, false, false, false),
                "{arm:?}: the late v clears both original graphs"
            );
            assert!(!outcome.orphan);
            Ok(())
        })
        .expect("batch arm");
    });
}

#[test]
fn shared_mode_root_batch_during_late_byte_marks_target_once() {
    run_late_root_batch(BatchArm::WithBatch);
}

#[test]
fn shared_mode_root_batch_absent_during_late_byte_leaves_target_white() {
    run_late_root_batch(BatchArm::WithoutBatch);
}

#[test]
fn shared_mode_root_batch_stale_during_late_byte_is_refused() {
    run_late_root_batch(BatchArm::StaleRejected);
}

// ---------------------------------------------------------------------------
// Bounded late work: repeated writes on one live edge, deferred identity.
// ---------------------------------------------------------------------------

#[test]
fn shared_mode_rewrite_one_live_edge_keeps_only_the_latest_value() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(24, 8, 40)).expect("install");
        let graph = heap::with_heap(&mut host, plant_shared_mode_graph).expect("plant");
        heap::open_fixed_step(&mut host).expect("open");
        let mut values = None;
        resume_weak(&mut host, |parked| {
            pause_after_both_weak_edges(parked, &graph);
            let mt = grow_one_table(parked);
            parked
                .with_saved_heap(|heap| heap.set_mode_scalar(mt, Value::string("x")))
                .expect("write the pending strong mode");
            attach_shared_metatable(parked, &graph, mt);
            let first = grow_one_table(parked);
            let second = grow_one_table(parked);
            let last = grow_one_table(parked);
            for value in [first, second, last] {
                assert!(!parked.marked(value));
            }
            let live_edges = parked.stats().live_edges;
            let events_before = parked.meter().mode_late_events;
            let reuses_before = parked.meter().mode_late_event_reuses;
            parked
                .with_saved_heap(|heap| -> Result<(), HeapError> {
                    heap.set_named_edge(graph.left, "subj", first)?;
                    assert_eq!(
                        heap.stats().live_edges,
                        live_edges,
                        "first overwrite keeps one edge"
                    );
                    heap.set_named_edge(graph.left, "subj", second)?;
                    assert_eq!(
                        heap.stats().live_edges,
                        live_edges,
                        "second overwrite keeps one edge"
                    );
                    heap.set_named_edge(graph.left, "subj", last)?;
                    assert_eq!(
                        heap.stats().live_edges,
                        live_edges,
                        "third overwrite keeps one edge"
                    );
                    Ok(())
                })
                .expect("overwrite the same live edge three times");
            assert_eq!(
                parked.meter().mode_late_events - events_before,
                1,
                "one live edge keeps one deferred site"
            );
            assert_eq!(
                parked.meter().mode_late_event_reuses - reuses_before,
                2,
                "the later overwrites update that site in place"
            );
            assert_eq!(
                parked.with_saved_heap(|heap| heap.named_edge(graph.left, "subj")),
                Ok(Some(last))
            );
            values = Some((first, second, last));
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let (first, second, last) = values.expect("written values");
            assert!(
                heap.alive(last),
                "the live final value on a strong owner edge survives"
            );
            assert!(
                !heap.alive(first),
                "an overwritten value must not be retained by a deferred replay"
            );
            assert!(
                !heap.alive(second),
                "an overwritten value must not be retained by a deferred replay"
            );
            assert_eq!(heap.named_edge(graph.left, "subj")?, Some(last));
            assert!(heap.alive(graph.right_value) && heap.alive(graph.right_child));
            assert!(!heap.alive(graph.orphan));
            Ok(())
        })
        .expect("live-edge rewrite");
    });
}

#[test]
fn shared_mode_late_work_is_slot_keyed_and_coalesced() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(40, 8, 64)).expect("install");
        let graph = heap::with_heap(&mut host, plant_shared_mode_graph).expect("plant");
        heap::open_fixed_step(&mut host).expect("open");
        resume_weak(&mut host, |parked| {
            pause_after_both_weak_edges(parked, &graph);
            let before = parked.meter();
            let mut filler = Vec::new();
            for _ in 0..8 {
                filler.push(grow_one_table(parked));
            }
            let far = grow_one_table(parked);
            let near = grow_one_table(parked);
            parked
                .with_saved_heap(|heap| -> Result<(), HeapError> {
                    heap.set_mode_scalar(far, Value::string("v"))?;
                    heap.set_mode_scalar(near, Value::string("x"))?;
                    heap.set_metatable(graph.left, Some(far))?;
                    heap.set_metatable(graph.right, Some(far))?;
                    heap.set_metatable(graph.left, Some(near))?;
                    heap.set_metatable(graph.left, Some(graph.strong_mt))?;
                    heap.set_metatable(graph.right, Some(graph.strong_mt))?;
                    Ok(())
                })
                .expect("far then near then strong before any pulse");
            let after = parked.meter();
            assert_eq!(
                after.mode_late_registrations - before.mode_late_registrations,
                2,
                "the far and near metatables are each queued once"
            );
            assert_eq!(
                after.mode_late_events - before.mode_late_events,
                2,
                "one deferred owner check per owner, not per setter"
            );
            assert_eq!(
                after.mode_late_event_reuses - before.mode_late_event_reuses,
                3,
                "the later setters coalesce into the existing owner checks"
            );
            assert_eq!(
                after.mode_late_lookups - before.mode_late_lookups,
                7,
                "seven setters perform seven constant route lookups"
            );
            assert_eq!(
                after.weak_mode_bytes, before.weak_mode_bytes,
                "no setter reads a mode byte"
            );
            assert_eq!(
                after.scratch.weak_modes_inited, before.scratch.weak_modes_inited,
                "no setter grows the indexed mode cache"
            );
            assert_eq!(
                after.scratch.mode_discover_inited, before.scratch.mode_discover_inited,
                "no setter grows the discovery sidecar"
            );
            let _ = filler;

            let mut pulses = 0u32;
            while parked.meter().mode_late_deferred - before.mode_late_deferred < 2 {
                let prior = parked.meter();
                let view = parked.pulse(1);
                let now = view.meter;
                let bytes = now.weak_mode_bytes - prior.weak_mode_bytes;
                let opened = now.weak_mode_edge_checks - prior.weak_mode_edge_checks;
                let parsed = now.mode_late_parsed - prior.mode_late_parsed;
                let deferred = now.mode_late_deferred - prior.mode_late_deferred;
                assert!(
                    bytes <= 1 && opened <= 1 && parsed <= 1 && deferred <= 1,
                    "pulse {pulses} exceeded budget 1: bytes={bytes} open={opened} parsed={parsed} deferred={deferred}"
                );
                assert!(!view.finished, "the cycle finished inside the late window");
                pulses += 1;
                assert!(pulses < 256, "the late window did not drain");
            }
            assert_eq!(
                parked.meter().owner_rescan_schedules - before.owner_rescan_schedules,
                2,
                "the final strong transition schedules both started owners"
            );
            assert_eq!(
                parked.meter().mode_late_parsed - before.mode_late_parsed,
                2,
                "both far and near modes are parsed once"
            );
            pulse_until_late_quiet(parked);
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            assert_eq!(heap.metatable(graph.left)?, Some(graph.strong_mt));
            assert_eq!(heap.metatable(graph.right)?, Some(graph.strong_mt));
            let outcome = read_shared_mode_outcome(heap, &graph)?;
            assert_eq!(
                (
                    outcome.left_value,
                    outcome.left_child,
                    outcome.right_value,
                    outcome.right_child,
                ),
                (true, true, true, true),
                "the coalesced final strong transition keeps both graphs"
            );
            assert_eq!(outcome.left_edge, Some(graph.left_value));
            assert_eq!(outcome.right_edge, Some(graph.right_value));
            assert!(!outcome.orphan);
            Ok(())
        })
        .expect("slot-keyed late work");
    });
}

#[test]
fn shared_mode_deferred_object_key_delete_does_not_replay() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(24, 8, 40)).expect("install");
        let graph = heap::with_heap(&mut host, plant_shared_mode_graph).expect("plant");
        heap::open_fixed_step(&mut host).expect("open");
        let mut objects = None;
        resume_weak(&mut host, |parked| {
            pause_after_both_weak_edges(parked, &graph);
            let mt = grow_one_table(parked);
            parked
                .with_saved_heap(|heap| heap.set_mode_scalar(mt, Value::string("x")))
                .expect("write the pending strong mode");
            attach_shared_metatable(parked, &graph, mt);
            let key = grow_one_table(parked);
            let value = grow_one_table(parked);
            parked
                .with_saved_heap(|heap| -> Result<(), HeapError> {
                    heap.set_object_edge(graph.left, key, value)
                })
                .expect("insert the object key while the mode is pending");
            assert!(
                parked.object_index(graph.left, key).is_some(),
                "the object key is present before the delete"
            );
            parked
                .with_saved_heap(|heap| -> Result<(), HeapError> {
                    heap.clear_object_edge(graph.left, key)
                })
                .expect("delete the object key before any late work");
            assert!(
                parked.object_index(graph.left, key).is_none(),
                "the object key is really unlinked"
            );
            objects = Some((key, value));
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let (key, value) = objects.expect("object key pair");
            assert!(
                !heap.alive(key),
                "a deleted object key must not be replayed by a deferred write"
            );
            assert!(
                !heap.alive(value),
                "a deleted object-key value must not be replayed"
            );
            assert!(heap.alive(graph.left_value) && heap.alive(graph.left_child));
            assert!(heap.alive(graph.right_value) && heap.alive(graph.right_child));
            assert!(!heap.alive(graph.orphan));
            Ok(())
        })
        .expect("deferred object-key delete");
    });
}

#[test]
fn shared_mode_object_key_value_overwrite_keeps_only_the_latest_value() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(24, 8, 40)).expect("install");
        let graph = heap::with_heap(&mut host, plant_shared_mode_graph).expect("plant");
        heap::open_fixed_step(&mut host).expect("open");
        let mut objects = None;
        resume_weak(&mut host, |parked| {
            pause_after_both_weak_edges(parked, &graph);
            let mt = grow_one_table(parked);
            parked
                .with_saved_heap(|heap| heap.set_mode_scalar(mt, Value::string("x")))
                .expect("write the pending strong mode");
            attach_shared_metatable(parked, &graph, mt);
            let key = grow_one_table(parked);
            let first = grow_one_table(parked);
            let last = grow_one_table(parked);
            let events_before = parked.meter().mode_late_events;
            let reuses_before = parked.meter().mode_late_event_reuses;
            parked
                .with_saved_heap(|heap| -> Result<(), HeapError> {
                    heap.set_object_edge(graph.left, key, first)?;
                    heap.set_object_edge(graph.left, key, last)
                })
                .expect("insert then overwrite the same object key");
            assert_eq!(
                parked.meter().mode_late_events - events_before,
                1,
                "the object-key edge keeps one deferred site"
            );
            assert_eq!(
                parked.meter().mode_late_event_reuses - reuses_before,
                1,
                "the value overwrite updates that site in place"
            );
            objects = Some((key, first, last));
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let (key, first, last) = objects.expect("object key values");
            assert!(heap.alive(key), "the live object key survives");
            assert!(heap.alive(last), "the live final value survives");
            assert!(
                !heap.alive(first),
                "an overwritten object-key value must not be retained"
            );
            assert!(heap.alive(graph.left_value) && heap.alive(graph.left_child));
            assert!(heap.alive(graph.right_value) && heap.alive(graph.right_child));
            assert!(!heap.alive(graph.orphan));
            Ok(())
        })
        .expect("object-key overwrite");
    });
}

#[test]
fn shared_mode_reused_slot_uses_the_current_generation() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(20, 8, 32)).expect("install");
        let graph = heap::with_heap(&mut host, plant_shared_mode_graph).expect("plant");
        heap::open_fixed_step(&mut host).expect("open");
        let mut mt_copy = None;
        resume_weak(&mut host, |parked| {
            pause_after_both_weak_edges(parked, &graph);
            let old = grow_one_table(parked);
            parked
                .with_saved_heap(|heap| heap.set_mode_scalar(old, Value::string("kv")))
                .expect("write the first generation mode");
            let base = parked.meter().weak_mode_bytes;
            pulse_until_late_bytes(parked, base, 2);
            assert_eq!(parked.meter().weak_mode_bytes - base, 2);
            parked
                .with_saved_heap(|heap| heap.rollback(old))
                .expect("release the first generation");
            let reused = grow_one_table(parked);
            assert_eq!(
                reused.slot(),
                old.slot(),
                "the second metatable reuses the released slot"
            );
            assert_ne!(
                reused.generation(),
                old.generation(),
                "a released slot is reused with a new generation"
            );
            parked
                .with_saved_heap(|heap| heap.set_mode_scalar(reused, Value::string("x")))
                .expect("write the second generation strong mode");
            attach_shared_metatable(parked, &graph, reused);
            mt_copy = Some(reused);
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let mt = mt_copy.expect("reused metatable");
            assert_eq!(heap.metatable(graph.left)?, Some(mt));
            let outcome = read_shared_mode_outcome(heap, &graph)?;
            assert_eq!(
                (
                    outcome.left_value,
                    outcome.left_child,
                    outcome.right_value,
                    outcome.right_child,
                ),
                (true, true, true, true),
                "the reused slot's final x must not inherit the old generation's kv mode"
            );
            assert_eq!(outcome.left_edge, Some(graph.left_value));
            assert_eq!(outcome.right_edge, Some(graph.right_value));
            assert!(!outcome.orphan);
            Ok(())
        })
        .expect("reused-slot generation");
    });
}

// ---------------------------------------------------------------------------
// Re-review 0248456: cache identity must include the current round.
// ---------------------------------------------------------------------------

struct CrossRoundGraph {
    mt: TableRef,
    owner: TableRef,
    gate: TableRef,
    gate_record: u32,
}

/// One metatable parsed by a real reset walk, one already-marked owner that is
/// queued but not opened, and a gate record that pauses Gray before the owner.
fn plant_cross_round_graph(heap: &mut LuaHeap) -> Result<CrossRoundGraph, HeapError> {
    let mt = heap.alloc()?;
    heap.set_mode_scalar(mt, Value::string("v"))?;
    let owner = heap.alloc()?;
    heap.set_metatable(owner, Some(mt))?;
    let gate = heap.alloc()?;
    heap.set_named_scalar(gate, "first", Value::Bool(true))?;
    heap.set_named_scalar(gate, "second", Value::Bool(true))?;
    let (_, gate_record) = heap
        .locate_named(gate.slot(), "second")
        .expect("gate second record");
    let frame = heap.enter_roots(2)?;
    // Gray is LIFO: the gate is popped first, so the owner is marked but not
    // opened when the test writes the mode.
    heap.write_root(frame, 0, owner)?;
    heap.write_root(frame, 1, gate)?;
    Ok(CrossRoundGraph {
        mt,
        owner,
        gate,
        gate_record,
    })
}

fn attach_cross_round_value(
    heap: &mut LuaHeap,
    owner: TableRef,
) -> Result<(TableRef, TableRef), HeapError> {
    let value = heap.alloc()?;
    let child = heap.alloc()?;
    heap.set_named_edge(owner, "subj", value)?;
    heap.set_named_edge(value, "next", child)?;
    Ok((value, child))
}

#[derive(Clone, Copy, Debug)]
enum CrossRoundArm {
    /// Business arm: the direct `__mode` was parsed in round 1, removed
    /// between rounds, and a fresh `v` is written in round 2 Gray.
    StaleCacheWrite,
    /// Control: the same metatable has its field at reset of this round.
    ParsedCurrentRound,
    /// Control: the metatable is live but unattached at reset; it is attached
    /// during Gray and was already parsed by the reset walk.
    UnpublishedParse,
    /// Control: a second, later round parses the live field again.
    Rerun,
}

fn run_cross_round_mode(arm: CrossRoundArm) {
    let mut heap = LuaHeap::new(limits(16, 8, 24));
    let graph = plant_cross_round_graph(&mut heap).expect("plant cross-round graph");
    heap.collect();
    assert!(
        heap.alive(graph.mt),
        "the metatable is marked through its owner"
    );
    assert!(heap.alive(graph.owner));
    assert!(heap.alive(graph.gate));
    // `pause_at` identifies the round by `mark_passes`; restart the counters
    // after the first round so this one is pass one again.
    heap.take_scan_meter();

    if matches!(arm, CrossRoundArm::StaleCacheWrite) {
        heap.set_mode_scalar(graph.mt, Value::Null)
            .expect("remove the direct __mode between rounds");
        assert!(
            heap.locate_named(graph.mt.slot(), "__mode").is_none(),
            "the direct mode record must be really unlinked"
        );
    }
    if matches!(arm, CrossRoundArm::UnpublishedParse) {
        // Keep one attached metatable without `__mode` so the reset walk still
        // parses every live slot's mode edge; the `v` metatable itself stays
        // unattached until Gray.
        let keeper = heap.alloc().expect("keeper metatable");
        heap.set_metatable(graph.owner, Some(keeper))
            .expect("attach the strong keeper before the next round");
        assert_eq!(heap.metatable(graph.owner), Ok(Some(keeper)));
    }

    let (value, child) = attach_cross_round_value(&mut heap, graph.owner)
        .expect("insert this round's value and successor");

    let (before, after_mark) = {
        let mut parked = ParkedMark::start(&mut heap).expect("start the cross-round mark");
        let pending = pause_at(&mut parked, graph.gate.slot(), graph.gate_record);
        assert_eq!(parked.phase_name(), "gray");
        assert_eq!(pending.owner, graph.gate.slot());
        assert_eq!(pending.cursor, graph.gate_record);
        assert!(
            parked.marked(graph.owner),
            "the owner is queued, not opened"
        );
        assert!(!parked.marked(value), "the value is still white");
        assert!(
            parked.pending().map(|scan| scan.owner) != Some(graph.owner.slot()),
            "the owner's ordinary scan has not started"
        );

        let before = parked.meter();
        match arm {
            CrossRoundArm::StaleCacheWrite => {
                parked
                    .with_saved_heap(|heap| heap.set_mode_scalar(graph.mt, Value::string("v")))
                    .expect("write the fresh v in the second round");
                let after = parked.meter();
                assert_eq!(
                    after.mode_late_registrations - before.mode_late_registrations,
                    1,
                    "a stale same-generation cache must not block the late registration"
                );
                assert_eq!(
                    parked.with_saved_heap(|heap| heap.named_value(graph.mt, "__mode")),
                    Ok(Value::string("v")),
                    "the raw field must really hold v"
                );
            }
            CrossRoundArm::UnpublishedParse => {
                parked
                    .with_saved_heap(|heap| heap.set_metatable(graph.owner, Some(graph.mt)))
                    .expect("attach the already-parsed metatable during Gray");
                let after = parked.meter();
                assert_eq!(
                    after.mode_late_registrations, before.mode_late_registrations,
                    "a parsed current-round mode needs no late registration"
                );
                assert_eq!(
                    after.mode_late_parsed, before.mode_late_parsed,
                    "an unpublished but parsed metatable is not re-read late"
                );
                assert_eq!(
                    parked.metatable(graph.owner),
                    Ok(Some(graph.mt)),
                    "the final attachment is the parsed metatable"
                );
            }
            CrossRoundArm::ParsedCurrentRound | CrossRoundArm::Rerun => {
                assert_eq!(
                    parked.meter().mode_late_registrations,
                    before.mode_late_registrations,
                    "this arm writes no mode; the reset cache is the only source"
                );
            }
        }

        let mut view = parked.pulse(64);
        while !view.finished {
            view = parked.pulse(64);
        }
        let after_mark = parked.meter();
        parked.complete().expect("sweep the cross-round mark");
        (before, after_mark)
    };

    assert!(
        !heap.alive(value),
        "the weak value must clear in this cycle"
    );
    assert!(!heap.alive(child), "its strong successor clears with it");
    assert_eq!(
        heap.named_edge(graph.owner, "subj"),
        Ok(None),
        "the weak entry clears or becomes a cursor gap"
    );
    assert!(heap.alive(graph.owner) && heap.alive(graph.mt));
    match arm {
        CrossRoundArm::StaleCacheWrite => {
            assert_eq!(
                after_mark.weak_mode_bytes - before.weak_mode_bytes,
                1,
                "the fresh v is read exactly once, on the late path"
            );
            assert_eq!(
                after_mark.mode_late_parsed - before.mode_late_parsed,
                1,
                "one parsed late mode is stored"
            );
            assert_eq!(
                after_mark.scratch.weak_modes_inited, before.scratch.weak_modes_inited,
                "the second-round late read must not size the indexed cache"
            );
            assert_eq!(
                after_mark.scratch.mode_discover_inited, before.scratch.mode_discover_inited,
                "the second-round late read must not size the discovery sidecar"
            );
        }
        CrossRoundArm::ParsedCurrentRound => {
            assert_eq!(
                after_mark.mode_late_registrations, before.mode_late_registrations,
                "a reset-parsed mode does not register a late read"
            );
            assert_eq!(
                after_mark.weak_mode_bytes, before.weak_mode_bytes,
                "the reset parse finished before Gray"
            );
        }
        CrossRoundArm::UnpublishedParse => {
            assert_eq!(
                after_mark.mode_late_registrations, before.mode_late_registrations,
                "the attach did not register a late read"
            );
        }
        CrossRoundArm::Rerun => {}
    }

    if matches!(arm, CrossRoundArm::Rerun) {
        let (value2, child2) =
            attach_cross_round_value(&mut heap, graph.owner).expect("insert the rerun value");
        heap.take_scan_meter();
        {
            let mut parked = ParkedMark::start(&mut heap).expect("start the rerun mark");
            pause_at(&mut parked, graph.gate.slot(), graph.gate_record);
            let before = parked.meter();
            let mut view = parked.pulse(64);
            while !view.finished {
                view = parked.pulse(64);
            }
            let after_mark = parked.meter();
            parked.complete().expect("sweep the rerun mark");
            assert_eq!(
                after_mark.mode_late_registrations, before.mode_late_registrations,
                "the rerun uses the reset cache, not a late registration"
            );
        }
        assert!(
            !heap.alive(value2),
            "a later round still parses the live field"
        );
        assert!(!heap.alive(child2));
        assert_eq!(heap.named_edge(graph.owner, "subj"), Ok(None));
    }
}

#[test]
fn cross_round_stale_cache_does_not_block_a_gray_mode_write() {
    run_cross_round_mode(CrossRoundArm::StaleCacheWrite);
}

#[test]
fn cross_round_parsed_current_round_clears_the_weak_value() {
    run_cross_round_mode(CrossRoundArm::ParsedCurrentRound);
}

#[test]
fn cross_round_unpublished_metatable_uses_the_reset_parse() {
    run_cross_round_mode(CrossRoundArm::UnpublishedParse);
}

#[test]
fn cross_round_rerun_parses_the_live_field_again() {
    run_cross_round_mode(CrossRoundArm::Rerun);
}

/// A cached rewrite is qualified by who has already scanned and by the next
/// round. Both original owners are scanned weak under v before the record is
/// rewritten to x; a new owner sharing the same metatable is not scanned yet.
/// This round keeps the already parsed v for both: the unscanned owner's
/// first read also uses the cache, so its new value clears. The next round
/// parses the live x field and keeps fresh values. This is the documented
/// cached-rewrite boundary, not a claim that public step counts identify a
/// phase.
#[test]
fn cached_mode_rewrite_qualifies_scanned_unscanned_and_the_next_round() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(32, 8, 48)).expect("install");
        let graph = heap::with_heap(&mut host, plant_shared_mode_graph).expect("plant");
        heap::open_fixed_step(&mut host).expect("open");
        let mut late_copy = None;
        let mut fresh_copy = None;
        resume_weak(&mut host, |parked| {
            pause_after_both_weak_edges(parked, &graph);
            let late = grow_one_table(parked);
            parked
                .with_saved_heap(|heap| heap.set_mode_scalar(late, Value::string("v")))
                .expect("write the initial v");
            attach_shared_metatable(parked, &graph, late);
            let base = parked.meter().weak_mode_bytes;
            pulse_until_late_bytes(parked, base, 1);
            pulse_until_late_quiet(parked);
            assert_eq!(
                parked.meter().mode_late_parsed,
                1,
                "the current-round v is already parsed"
            );
            let registrations = parked.meter().mode_late_registrations;
            let bytes = parked.meter().weak_mode_bytes;
            parked
                .with_saved_heap(|heap| heap.set_mode_scalar(late, Value::string("x")))
                .expect("rewrite the same record to x");
            assert_eq!(
                parked.meter().mode_late_registrations,
                registrations,
                "a current-round cached rewrite does not queue a second parse"
            );
            assert_eq!(
                parked.meter().weak_mode_bytes,
                bytes,
                "a cached rewrite reads no more bytes in this round"
            );
            let fresh = grow_one_table(parked);
            let fresh_value = grow_one_table(parked);
            parked
                .with_saved_heap(|heap| -> Result<(), HeapError> {
                    heap.set_metatable(fresh, Some(late))?;
                    heap.set_named_edge(fresh, "subj", fresh_value)?;
                    Ok(())
                })
                .expect("attach and fill the unscanned owner");
            assert!(
                !parked.marked(fresh_value),
                "the unscanned owner's value starts white"
            );
            parked
                .accept_root_batch(&[super::Rooted::Table(fresh)])
                .expect("mark the unscanned owner");
            fresh_copy = Some((fresh, fresh_value));
            late_copy = Some(late);
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let (fresh, fresh_value) = fresh_copy.expect("fresh owner");
            let late = late_copy.expect("late metatable");
            assert!(heap.alive(fresh), "the marked fresh owner survives");
            assert!(
                !heap.alive(fresh_value),
                "the unscanned owner's first read uses the cached v and clears"
            );
            let outcome = read_shared_mode_outcome(heap, &graph)?;
            assert_eq!(
                (
                    outcome.left_value,
                    outcome.left_child,
                    outcome.right_value,
                    outcome.right_child,
                ),
                (false, false, false, false),
                "the already scanned owners keep the cached v this round"
            );
            assert_eq!(heap.named_value(late, "__mode"), Ok(Value::string("x")));
            assert_eq!(heap.metatable(graph.left)?, Some(late));
            assert_eq!(heap.metatable(graph.right)?, Some(late));
            assert_eq!(heap.metatable(fresh)?, Some(late));
            Ok(())
        })
        .expect("cached rewrite this round");

        let next_values = heap::with_heap(&mut host, |heap| -> Result<_, HeapError> {
            let late = late_copy.expect("late metatable");
            assert_eq!(heap.named_value(late, "__mode"), Ok(Value::string("x")));
            let left = heap.alloc()?;
            let right = heap.alloc()?;
            heap.set_named_edge(graph.left, "again", left)?;
            heap.set_named_edge(graph.right, "again", right)?;
            Ok((left, right))
        })
        .expect("next-round values");
        heap::collect(&mut host).expect("next full collection");
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let (left, right) = next_values;
            assert!(
                heap.alive(left) && heap.alive(right),
                "the next round parses the live x field and keeps fresh values"
            );
            assert_eq!(heap.named_edge(graph.left, "again")?, Some(left));
            assert_eq!(heap.named_edge(graph.right, "again")?, Some(right));
            Ok(())
        })
        .expect("next round strong");
    });
}

// ---------------------------------------------------------------------------
// Re-review 0248456: the deferred owner check keeps the observed old mode.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum ReleasedOldArm {
    FinalStrong,
    FinalWeak,
}

fn run_released_old_metatable(arm: ReleasedOldArm) {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(24, 8, 40)).expect("install");
        let graph = heap::with_heap(&mut host, plant_shared_mode_graph).expect("plant");
        heap::open_fixed_step(&mut host).expect("open");
        let mut final_mt = None;
        let mut schedules_before = 0u64;
        resume_weak(&mut host, |parked| {
            pause_after_both_weak_edges(parked, &graph);
            schedules_before = parked.meter().owner_rescan_schedules;
            let late = grow_one_table(parked);
            parked
                .with_saved_heap(|heap| heap.set_mode_scalar(late, Value::string("v")))
                .expect("write the late v");
            attach_shared_metatable(parked, &graph, late);
            assert_eq!(
                parked.meter().owner_rescan_schedules,
                schedules_before,
                "the unknown late mode only records owner checks"
            );
            parked
                .with_saved_heap(|heap| heap.rollback(graph.shared))
                .expect("release the old metatable while it is still recorded as old");
            assert!(
                !parked.with_saved_heap(|heap| heap.alive(graph.shared)),
                "the old metatable is really released"
            );
            let reused = grow_one_table(parked);
            assert_eq!(
                reused.slot(),
                graph.shared.slot(),
                "the released old metatable slot is reused"
            );
            assert_ne!(
                reused.generation(),
                graph.shared.generation(),
                "reuse gets a new generation"
            );
            let final_mt_value = match arm {
                ReleasedOldArm::FinalStrong => graph.strong_mt,
                ReleasedOldArm::FinalWeak => {
                    let weak = grow_one_table(parked);
                    parked
                        .with_saved_heap(|heap| heap.set_mode_scalar(weak, Value::string("v")))
                        .expect("write the final weak v");
                    weak
                }
            };
            parked
                .with_saved_heap(|heap| -> Result<(), HeapError> {
                    heap.set_metatable(graph.left, Some(final_mt_value))?;
                    heap.set_metatable(graph.right, Some(final_mt_value))?;
                    Ok(())
                })
                .expect("attach the final metatable");
            assert_eq!(
                parked.metatable(graph.left),
                Ok(Some(final_mt_value)),
                "the recorded final tuple is the actual left attachment"
            );
            assert_eq!(
                parked.metatable(graph.right),
                Ok(Some(final_mt_value)),
                "the recorded final tuple is the actual right attachment"
            );
            assert!(!parked.marked(graph.left_value));
            assert!(!parked.marked(graph.right_value));
            final_mt = Some(final_mt_value);
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let final_mt = final_mt.expect("final metatable");
            assert_eq!(heap.metatable(graph.left)?, Some(final_mt));
            assert_eq!(heap.metatable(graph.right)?, Some(final_mt));
            assert!(heap.alive(graph.left) && heap.alive(graph.right));
            let outcome = read_shared_mode_outcome(heap, &graph)?;
            match arm {
                ReleasedOldArm::FinalStrong => {
                    assert_eq!(
                        (
                            outcome.left_value,
                            outcome.left_child,
                            outcome.right_value,
                            outcome.right_child,
                        ),
                        (true, true, true, true),
                        "the released old v must still drive a strong rescan"
                    );
                    assert_eq!(
                        outcome.left_edge,
                        Some(graph.left_value),
                        "the left original value keeps its identity"
                    );
                    assert_eq!(
                        outcome.right_edge,
                        Some(graph.right_value),
                        "the right original value keeps its identity"
                    );
                    assert_eq!(
                        heap.locate_named(graph.left.slot(), "subj")
                            .map(|(_, index)| index),
                        Some(graph.left_value_record),
                        "the current strong edge keeps its original record"
                    );
                    assert_eq!(
                        heap.scan_meter().owner_rescan_schedules,
                        schedules_before + 2,
                        "both started owners are scheduled after the old release"
                    );
                }
                ReleasedOldArm::FinalWeak => {
                    assert_eq!(
                        (
                            outcome.left_value,
                            outcome.left_child,
                            outcome.right_value,
                            outcome.right_child,
                        ),
                        (false, false, false, false),
                        "an old v to final v transition stays weak"
                    );
                    assert_eq!(outcome.left_edge, None);
                    assert_eq!(outcome.right_edge, None);
                    assert_eq!(
                        heap.scan_meter().owner_rescan_schedules,
                        schedules_before,
                        "no strong rescan is scheduled for the weak arm"
                    );
                }
            }
            assert!(
                !outcome.orphan,
                "the independent orphan clears in both arms"
            );
            assert!(!outcome.dust);
            Ok(())
        })
        .expect("released old metatable");
    });
}

#[test]
fn shared_mode_released_old_metatable_final_strong_keeps_both_graphs() {
    run_released_old_metatable(ReleasedOldArm::FinalStrong);
}

#[test]
fn shared_mode_released_old_metatable_final_weak_clears_both_graphs() {
    run_released_old_metatable(ReleasedOldArm::FinalWeak);
}

// Trace for the two tests above: the old metatable is the fixture's preexisting
// `shared` slot, which lies inside the reset-sized discovery sidecar. `rollback`
// unlinks its direct `__mode` edge while mutation is open, so the existing
// delete-discovery rule arms the same-generation stamp; `mode_before_direct_delete`
// restores the value bit even after the slot is reused. That path is already
// covered by the discovery mechanism and is a control. A metatable grown after
// ModeInit has no sidecar entry; its release clears the slot stamp, so the old
// observation must be captured when the deferred check is queued. The next
// fixture is the business witness for that grown-slot path.

#[derive(Clone, Copy, Debug)]
enum GrownOldFinal {
    Strong,
    Weak,
}

fn run_grown_old_release(final_mode: GrownOldFinal) {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(28, 8, 48)).expect("install");
        let graph = heap::with_heap(&mut host, plant_shared_mode_graph).expect("plant");
        heap::open_fixed_step(&mut host).expect("open");
        let mut final_mt = None;
        let mut schedules_before = 0u64;
        let mut handles = None;
        resume_weak(&mut host, |parked| {
            pause_after_both_weak_edges(parked, &graph);
            schedules_before = parked.meter().owner_rescan_schedules;
            let old_mt = grow_one_table(parked);
            parked
                .with_saved_heap(|heap| heap.set_mode_scalar(old_mt, Value::string("v")))
                .expect("write the grown old v");
            let left = grow_one_table(parked);
            let right = grow_one_table(parked);
            let left_value = grow_one_table(parked);
            let left_child = grow_one_table(parked);
            let right_value = grow_one_table(parked);
            let right_child = grow_one_table(parked);
            parked
                .with_saved_heap(|heap| -> Result<(), HeapError> {
                    heap.set_metatable(left, Some(old_mt))?;
                    heap.set_metatable(right, Some(old_mt))?;
                    heap.set_named_edge(left, "subj", left_value)?;
                    heap.set_named_edge(left_value, "next", left_child)?;
                    heap.set_named_edge(right, "subj", right_value)?;
                    heap.set_named_edge(right_value, "next", right_child)?;
                    Ok(())
                })
                .expect("plant the grown-old owners");
            let left_subj = parked
                .with_saved_heap(|heap| heap.locate_named(left.slot(), "subj").map(|(_, i)| i))
                .expect("left subj record");
            let right_subj = parked
                .with_saved_heap(|heap| heap.locate_named(right.slot(), "subj").map(|(_, i)| i))
                .expect("right subj record");
            parked
                .accept_root_batch(&[super::Rooted::Table(left), super::Rooted::Table(right)])
                .expect("mark both grown-old owners");
            let mut opened = false;
            for _ in 0..HARNESS_BOUND {
                if let Some(pending) = parked.pending()
                    && pending.owner == left.slot()
                    && pending.cursor == left_subj
                {
                    opened = true;
                    break;
                }
                assert!(
                    !parked.pulse(1).finished,
                    "the cycle finished before the grown-old left owner opened"
                );
            }
            assert!(
                opened,
                "the left grown-old owner must be open before its value target"
            );
            assert!(
                !parked.marked(left_value) && !parked.marked(right_value),
                "both grown-old owners were scanned weakly"
            );
            let late = grow_one_table(parked);
            let mode = match final_mode {
                GrownOldFinal::Strong => "x",
                GrownOldFinal::Weak => "v",
            };
            parked
                .with_saved_heap(|heap| heap.set_mode_scalar(late, Value::string(mode)))
                .expect("write the final late mode");
            parked
                .with_saved_heap(|heap| -> Result<(), HeapError> {
                    heap.set_metatable(left, Some(late))?;
                    heap.set_metatable(right, Some(late))?;
                    Ok(())
                })
                .expect("attach the unresolved final metatable");
            assert_eq!(
                parked.meter().owner_rescan_schedules,
                schedules_before,
                "the unresolved final mode only records owner checks"
            );
            parked
                .with_saved_heap(|heap| heap.rollback(old_mt))
                .expect("release the grown old metatable before its parse was recorded");
            assert!(
                !parked.with_saved_heap(|heap| heap.alive(old_mt)),
                "the grown old metatable is really released"
            );
            let reused = grow_one_table(parked);
            assert_eq!(
                reused.slot(),
                old_mt.slot(),
                "the grown old metatable slot is reused"
            );
            assert_ne!(reused.generation(), old_mt.generation());
            for owner in [left, right] {
                assert_eq!(
                    parked.metatable(owner),
                    Ok(Some(late)),
                    "the final attachment is the late metatable"
                );
            }
            assert!(!parked.marked(left_value) && !parked.marked(right_value));
            final_mt = Some(late);
            handles = Some((
                left,
                right,
                left_value,
                left_child,
                right_value,
                right_child,
                left_subj,
                right_subj,
            ));
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let late = final_mt.expect("final metatable");
            let (
                left,
                right,
                left_value,
                left_child,
                right_value,
                right_child,
                left_subj,
                right_subj,
            ) = handles.expect("grown handles");
            assert_eq!(heap.metatable(left)?, Some(late));
            assert_eq!(heap.metatable(right)?, Some(late));
            assert!(heap.alive(left) && heap.alive(right));
            match final_mode {
                GrownOldFinal::Strong => {
                    assert!(
                        heap.alive(left_value)
                            && heap.alive(left_child)
                            && heap.alive(right_value)
                            && heap.alive(right_child),
                        "the captured old v must drive a strong rescan after the old release"
                    );
                    assert_eq!(
                        heap.named_edge(left, "subj")?,
                        Some(left_value),
                        "the left original value keeps its identity"
                    );
                    assert_eq!(
                        heap.named_edge(right, "subj")?,
                        Some(right_value),
                        "the right original value keeps its identity"
                    );
                    assert_eq!(
                        heap.locate_named(left.slot(), "subj").map(|(_, i)| i),
                        Some(left_subj),
                        "the current strong edge keeps its original record"
                    );
                    assert_eq!(
                        heap.locate_named(right.slot(), "subj").map(|(_, i)| i),
                        Some(right_subj)
                    );
                    assert_eq!(
                        heap.scan_meter().owner_rescan_schedules,
                        schedules_before + 2,
                        "both grown-old owners are rescanned strong"
                    );
                }
                GrownOldFinal::Weak => {
                    assert!(
                        !heap.alive(left_value)
                            && !heap.alive(left_child)
                            && !heap.alive(right_value)
                            && !heap.alive(right_child),
                        "a final weak mode clears the grown-old graphs"
                    );
                    assert_eq!(heap.named_edge(left, "subj")?, None);
                    assert_eq!(heap.named_edge(right, "subj")?, None);
                    assert_eq!(
                        heap.scan_meter().owner_rescan_schedules,
                        schedules_before,
                        "the final weak arm schedules no rescan"
                    );
                }
            }
            assert!(
                !heap.alive(graph.orphan),
                "the independent weak orphan clears"
            );
            Ok(())
        })
        .expect("grown old release");
    });
}

#[test]
fn shared_mode_grown_old_metatable_release_final_strong_keeps_the_captured_graphs() {
    run_grown_old_release(GrownOldFinal::Strong);
}

#[test]
fn shared_mode_grown_old_metatable_release_final_weak_clears_the_captured_graphs() {
    run_grown_old_release(GrownOldFinal::Weak);
}

// ---------------------------------------------------------------------------
// Re-review 0248456: release/reuse invalidates queued owners and edge records.
// ---------------------------------------------------------------------------

#[test]
fn queued_owner_release_under_saved_mark_clears_tags_and_keeps_the_current_graphs() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(28, 8, 48)).expect("install");
        let graph = heap::with_heap(&mut host, plant_shared_mode_graph).expect("plant");
        heap::open_fixed_step(&mut host).expect("open");
        let mut fresh = None;
        let mut fresh_value = None;
        let mut schedules_before = 0u64;
        let mut deferred_before = 0u64;
        resume_weak(&mut host, |parked| {
            pause_after_both_weak_edges(parked, &graph);
            schedules_before = parked.meter().owner_rescan_schedules;
            deferred_before = parked.meter().mode_late_deferred;
            let late = grow_one_table(parked);
            parked
                .with_saved_heap(|heap| heap.set_mode_scalar(late, Value::string("x")))
                .expect("write the strong late mode");
            attach_shared_metatable(parked, &graph, late);
            // The left owner check is the current head of the intrusive queue.
            parked
                .with_saved_heap(|heap| heap.rollback(graph.left))
                .expect("release the queued head owner");
            assert!(
                !parked.with_saved_heap(|heap| heap.alive(graph.left)),
                "the queued owner is really released"
            );
            let replaced = grow_one_table(parked);
            assert_eq!(
                replaced.slot(),
                graph.left.slot(),
                "the released owner slot is reused"
            );
            assert_ne!(replaced.generation(), graph.left.generation());
            let (mode_epoch, owner_epoch) = parked.with_saved_heap(|heap| {
                let slot = &heap.slots[replaced.slot() as usize];
                (slot.late_mode_epoch, slot.late_owner_epoch)
            });
            assert_eq!(mode_epoch, 0, "reuse inherits no late-mode tag");
            assert_eq!(owner_epoch, 0, "reuse inherits no queued-owner tag");
            parked
                .with_saved_heap(|heap| -> Result<(), HeapError> {
                    heap.set_metatable(replaced, Some(graph.strong_mt))?;
                    let value = heap.alloc()?;
                    heap.set_named_edge(replaced, "subj", value)?;
                    Ok(())
                })
                .expect("attach the reused owner strong and add its value");
            let value = parked
                .with_saved_heap(|heap| heap.named_edge(replaced, "subj"))
                .expect("fresh subj")
                .expect("fresh value");
            let frame = parked
                .with_saved_heap(|heap| heap.current_frame())
                .expect("root frame");
            parked
                .write_table_root(frame, 4, replaced)
                .expect("the released owner's root slot is free");
            assert_eq!(
                parked.metatable(replaced),
                Ok(Some(graph.strong_mt)),
                "the reused owner keeps its own strong attachment"
            );
            assert!(!parked.marked(value));
            fresh = Some(replaced);
            fresh_value = Some(value);
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let replaced = fresh.expect("reused owner");
            let value = fresh_value.expect("fresh value");
            assert!(!heap.alive(graph.left), "the queued owner is swept");
            assert!(!heap.alive(graph.left_value) && !heap.alive(graph.left_child));
            assert_eq!(
                heap.named_edge(graph.left, "subj"),
                Err(HeapError::StaleHandle)
            );
            assert!(heap.alive(replaced), "the reused owner survives");
            assert!(heap.alive(value), "its strong value survives");
            assert_eq!(
                heap.named_edge(replaced, "subj")?,
                Some(value),
                "the current strong edge keeps its value"
            );
            assert_eq!(heap.metatable(replaced)?, Some(graph.strong_mt));
            assert!(heap.alive(graph.right_value) && heap.alive(graph.right_child));
            assert_eq!(
                heap.named_edge(graph.right, "subj")?,
                Some(graph.right_value)
            );
            assert!(!heap.alive(graph.orphan));
            assert_eq!(
                heap.scan_meter().owner_rescan_schedules,
                schedules_before + 1,
                "only the surviving right owner is rescanned; the released check is gone"
            );
            assert_eq!(
                heap.scan_meter().mode_late_deferred,
                deferred_before + 1,
                "only the surviving owner check is consumed"
            );
            Ok(())
        })
        .expect("queued owner release");
    });
}

#[test]
fn queued_edge_record_release_and_reuse_keeps_only_the_current_strong_value() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(28, 8, 48)).expect("install");
        let graph = heap::with_heap(&mut host, plant_shared_mode_graph).expect("plant");
        heap::open_fixed_step(&mut host).expect("open");
        let mut objects = None;
        let mut record_copy = None;
        resume_weak(&mut host, |parked| {
            pause_after_both_weak_edges(parked, &graph);
            let late = grow_one_table(parked);
            parked
                .with_saved_heap(|heap| heap.set_mode_scalar(late, Value::string("x")))
                .expect("write the strong late mode");
            attach_shared_metatable(parked, &graph, late);
            let doomed = grow_one_table(parked);
            parked
                .with_saved_heap(|heap| heap.set_named_edge(graph.left, "pending", doomed))
                .expect("insert the queued edge target");
            let record = parked
                .with_saved_heap(|heap| heap.locate_named(graph.left.slot(), "pending"))
                .expect("pending record")
                .1;
            assert!(
                !parked.marked(doomed),
                "the pending strong mode defers the inserted target"
            );
            parked
                .with_saved_heap(|heap| heap.set_named_scalar(graph.left, "pending", Value::Null))
                .expect("delete the queued edge record");
            assert!(
                parked.with_saved_heap(|heap| heap
                    .locate_named(graph.left.slot(), "pending")
                    .is_none()),
                "the queued record is really unlinked"
            );
            let (late_epoch, late_owner, late_value, late_key) = parked.with_saved_heap(|heap| {
                let slot = &heap.edge_slots[record as usize];
                (
                    slot.late_epoch,
                    slot.late_owner,
                    slot.late_value,
                    slot.late_key,
                )
            });
            assert_eq!(late_epoch, 0, "released edge record clears its late tag");
            assert_eq!(late_owner, 0);
            assert_eq!(late_value, None);
            assert_eq!(late_key, None);
            let kept = grow_one_table(parked);
            parked
                .with_saved_heap(|heap| heap.set_named_edge(graph.left, "pending", kept))
                .expect("reinsert the record with the current strong target");
            let reused = parked
                .with_saved_heap(|heap| heap.locate_named(graph.left.slot(), "pending"))
                .expect("reused record")
                .1;
            assert_eq!(
                reused, record,
                "the free record is reused for the current edge"
            );
            assert!(
                !parked.marked(kept),
                "the reused event defers the new target"
            );
            objects = Some((doomed, kept));
            record_copy = Some(reused);
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let (doomed, kept) = objects.expect("edge targets");
            let record = record_copy.expect("record");
            assert!(!heap.alive(doomed), "the obsolete target clears");
            assert!(heap.alive(kept), "the current strong target survives");
            assert_eq!(
                heap.named_edge(graph.left, "pending")?,
                Some(kept),
                "the current strong edge keeps its value"
            );
            assert_eq!(
                heap.locate_named(graph.left.slot(), "pending")
                    .map(|(_, i)| i),
                Some(record),
                "the current edge keeps the original new record and key"
            );
            assert!(heap.alive(graph.left_value) && heap.alive(graph.left_child));
            assert!(heap.alive(graph.right_value) && heap.alive(graph.right_child));
            let slot = &heap.edge_slots[record as usize];
            assert_eq!(
                slot.late_epoch, 0,
                "a kept edge record ends with no late tag"
            );
            assert_eq!(slot.late_value, None);
            assert_eq!(slot.late_key, None);
            assert!(!heap.alive(graph.orphan));
            Ok(())
        })
        .expect("queued edge release");
    });
}

#[test]
fn queued_owner_and_edge_tags_are_cleared_after_the_mark_is_abandoned() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(28, 8, 56)).expect("install");
        let graph = heap::with_heap(&mut host, plant_shared_mode_graph).expect("plant");
        heap::open_fixed_step(&mut host).expect("open");
        let mut doomed = None;
        let mut late_slot = None;
        let mut pending_record = None;
        let mut replaced_after = None;
        let mut kept_after = None;
        {
            heap::with_heap(&mut host, |heap| {
                let mut parked = ParkedMark::resume(heap).expect("resume saved mark");
                parking_for_abandon(
                    &mut parked,
                    &graph,
                    &mut doomed,
                    &mut late_slot,
                    &mut pending_record,
                );
                // Dropping without `park` abandons the saved mark.
                Ok::<(), HeapError>(())
            })
            .expect("abandon");
        }
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            assert!(
                heap.mark.is_none(),
                "the abandoned mark really left the heap"
            );
            assert!(
                heap.slots[graph.left.slot() as usize].late_owner_epoch != 0
                    || heap.slots[graph.right.slot() as usize].late_owner_epoch != 0,
                "queue tags exist before release"
            );
            let late = late_slot.expect("late metatable");
            assert_ne!(
                heap.slots[late.slot() as usize].late_mode_epoch,
                0,
                "the late-mode tag exists before release"
            );
            heap.rollback(graph.left)?;
            assert!(
                !heap.alive(graph.left),
                "the queued owner is released under no mark"
            );
            assert_eq!(
                heap.slots[graph.left.slot() as usize].late_owner_epoch,
                0,
                "release clears the abandoned owner tag"
            );
            assert!(
                heap.named_edge(graph.left, "subj").is_err(),
                "the released owner handle is no longer readable"
            );
            let record = pending_record.expect("pending record index");
            assert_eq!(
                heap.edge_slots[record as usize].late_epoch, 0,
                "releasing the owner clears the abandoned edge tag"
            );
            let replaced = heap.alloc()?;
            assert_eq!(
                replaced.slot(),
                graph.left.slot(),
                "the abandoned owner slot is reused"
            );
            assert_eq!(
                heap.slots[replaced.slot() as usize].late_owner_epoch,
                0,
                "reuse starts with no inherited owner tag"
            );
            let target = doomed.expect("doomed edge target");
            let kept = heap.alloc()?;
            heap.set_metatable(replaced, Some(graph.strong_mt))?;
            heap.set_named_edge(replaced, "subj", kept)?;
            heap.rollback(late)?;
            assert_eq!(
                heap.slots[late.slot() as usize].late_mode_epoch,
                0,
                "release clears the abandoned mode tag"
            );
            let reused_mt = heap.alloc()?;
            assert_eq!(reused_mt.slot(), late.slot());
            assert_eq!(
                heap.slots[reused_mt.slot() as usize].late_mode_epoch,
                0,
                "the reused metatable slot inherits no mode tag"
            );
            heap.set_metatable(graph.right, Some(graph.strong_mt))?;
            let frame = heap.current_frame().expect("root frame");
            heap.write_root(frame, 4, replaced)?;
            let _ = (target, reused_mt);
            replaced_after = Some(replaced);
            kept_after = Some(kept);
            Ok(())
        })
        .expect("abandoned release");
        heap::collect(&mut host).expect("fresh full collection");
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let target = doomed.expect("doomed target");
            let replaced = replaced_after.expect("reused owner");
            let kept = kept_after.expect("kept value");
            assert!(!heap.alive(graph.left));
            assert!(!heap.alive(target), "the obsolete queued target clears");
            assert!(!heap.alive(graph.left_value) && !heap.alive(graph.left_child));
            assert!(heap.alive(replaced), "the reused owner survives");
            assert!(heap.alive(kept), "its strong value survives");
            assert_eq!(
                heap.named_edge(replaced, "subj")?,
                Some(kept),
                "the current strong edge keeps its value"
            );
            assert!(heap.alive(graph.right_value) && heap.alive(graph.right_child));
            assert_eq!(
                heap.named_edge(graph.right, "subj")?,
                Some(graph.right_value)
            );
            Ok(())
        })
        .expect("abandoned outcome");
    });
}

fn parking_for_abandon(
    parked: &mut ParkedMark<'_>,
    graph: &SharedModeGraph,
    doomed: &mut Option<TableRef>,
    late_slot: &mut Option<TableRef>,
    pending_record: &mut Option<u32>,
) {
    pause_after_both_weak_edges(parked, graph);
    let late = grow_one_table(parked);
    parked
        .with_saved_heap(|heap| heap.set_mode_scalar(late, Value::string("v")))
        .expect("write the abandoned late v");
    attach_shared_metatable(parked, graph, late);
    let target = grow_one_table(parked);
    parked
        .with_saved_heap(|heap| heap.set_named_edge(graph.left, "pending", target))
        .expect("insert the abandoned queued edge target");
    parked.with_saved_heap(|heap| {
        let left = &heap.slots[graph.left.slot() as usize];
        let right = &heap.slots[graph.right.slot() as usize];
        assert_ne!(left.late_owner_epoch, 0);
        assert_ne!(right.late_owner_epoch, 0);
        let late_slot_stored = &heap.slots[late.slot() as usize];
        assert_ne!(late_slot_stored.late_mode_epoch, 0);
        let (_, pending) = heap
            .locate_named(graph.left.slot(), "pending")
            .expect("pending");
        assert_ne!(
            heap.edge_slots[pending as usize].late_epoch, 0,
            "the abandoned edge record carries a queue tag before release"
        );
        *pending_record = Some(pending);
    });
    *doomed = Some(target);
    *late_slot = Some(late);
}

// ---------------------------------------------------------------------------
// Re-review 0248456: a rescan opened strong must re-read a late weak switch.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum RescanSwitchArm {
    OrdinaryWeak,
    ResumedStrong,
    LateWeakAfterHead,
}

fn run_rescan_switch(arm: RescanSwitchArm) {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(24, 8, 40)).expect("install");
        let graph = heap::with_heap(&mut host, plant_shared_mode_graph).expect("plant");
        heap::open_fixed_step(&mut host).expect("open");
        let mut switched = None;
        let mut paused_at_left_value = false;
        resume_weak(&mut host, |parked| {
            pause_after_both_weak_edges(parked, &graph);
            if matches!(arm, RescanSwitchArm::OrdinaryWeak) {
                return;
            }
            attach_shared_metatable(parked, &graph, graph.strong_mt);
            let mut opened = false;
            for _ in 0..HARNESS_BOUND {
                if let Some(pending) = parked.pending_rescan()
                    && pending.owner == graph.left.slot()
                    && pending.cursor == graph.left_value_record
                {
                    opened = true;
                    break;
                }
                assert!(
                    !parked.pulse(1).finished,
                    "the cycle finished before the left rescan reached its first target"
                );
            }
            assert!(
                opened,
                "the left rescan head must be open right before its value target"
            );
            paused_at_left_value = true;
            assert!(
                parked.marked(graph.right_value) && parked.marked(graph.right_child),
                "the already-rescanned right owner was marked strong before the switch"
            );
            assert!(
                !parked.marked(graph.left_value),
                "the left target is still white when the head is open"
            );
            if matches!(arm, RescanSwitchArm::LateWeakAfterHead) {
                let late = grow_one_table(parked);
                parked
                    .with_saved_heap(|heap| heap.set_mode_scalar(late, Value::string("v")))
                    .expect("write the late weak v");
                attach_shared_metatable(parked, &graph, late);
                assert_eq!(
                    parked.metatable(graph.left),
                    Ok(Some(late)),
                    "the final attachment really is the late weak metatable"
                );
                assert!(
                    !parked.marked(graph.left_value),
                    "the switch itself must not mark the white target"
                );
                switched = Some(late);
            }
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let outcome = read_shared_mode_outcome(heap, &graph)?;
            match arm {
                RescanSwitchArm::OrdinaryWeak => {
                    assert_eq!(
                        (
                            outcome.left_value,
                            outcome.left_child,
                            outcome.right_value,
                            outcome.right_child,
                        ),
                        (false, false, false, false),
                        "the ordinary weak control clears both graphs"
                    );
                    assert_eq!(outcome.left_edge, None);
                    assert_eq!(outcome.right_edge, None);
                }
                RescanSwitchArm::ResumedStrong => {
                    assert_eq!(
                        (
                            outcome.left_value,
                            outcome.left_child,
                            outcome.right_value,
                            outcome.right_child,
                        ),
                        (true, true, true, true),
                        "a rescan that keeps its strength marks both graphs"
                    );
                    assert_eq!(outcome.left_edge, Some(graph.left_value));
                    assert_eq!(outcome.right_edge, Some(graph.right_value));
                }
                RescanSwitchArm::LateWeakAfterHead => {
                    assert!(
                        paused_at_left_value,
                        "the late switch must happen while the left head is open"
                    );
                    assert_eq!(
                        (outcome.left_value, outcome.left_child),
                        (false, false),
                        "resuming with old strength must not mark the white left target or child"
                    );
                    assert_eq!(
                        outcome.left_edge, None,
                        "the weak left entry clears after the late switch"
                    );
                    assert_eq!(
                        (outcome.right_value, outcome.right_child),
                        (true, true),
                        "the already-rescanned right graph keeps its strong marks"
                    );
                }
            }
            let late = switched;
            if let Some(late) = late {
                assert_eq!(heap.metatable(graph.left)?, Some(late));
                assert_eq!(heap.metatable(graph.right)?, Some(late));
            }
            assert!(!outcome.orphan, "the independent orphan always clears");
            assert!(!outcome.dust);
            Ok(())
        })
        .expect("rescan switch arm");
    });
}

#[test]
fn shared_mode_late_weak_switch_after_open_rescan_does_not_mark_white_targets() {
    run_rescan_switch(RescanSwitchArm::LateWeakAfterHead);
}

#[test]
fn shared_mode_ordinary_weak_scan_clears_both_graphs() {
    run_rescan_switch(RescanSwitchArm::OrdinaryWeak);
}

#[test]
fn shared_mode_resumed_strong_rescan_keeps_both_graphs() {
    run_rescan_switch(RescanSwitchArm::ResumedStrong);
}

// ---------------------------------------------------------------------------
// Re-review 0248456: known-mode overwrites must not starve ordinary Gray work.
// ---------------------------------------------------------------------------

#[test]
fn known_mode_live_edge_overwrites_advance_one_ordinary_gray_unit_per_pulse() {
    let mut heap = LuaHeap::new(limits(12, 8, 24));
    let strong_mt = heap.alloc().expect("strong metatable");
    let a = heap.alloc().expect("first marked value");
    let b = heap.alloc().expect("second marked value");
    let target = heap.alloc().expect("marked owner");
    heap.set_metatable(target, Some(strong_mt))
        .expect("attach the known strong metatable");
    heap.set_edge(target, KEY_X, a)
        .expect("install the fixed live edge");
    let gate = heap.alloc().expect("unrelated owner");
    heap.set_named_scalar(gate, "g0", Value::Bool(true))
        .expect("gate record 0");
    heap.set_named_scalar(gate, "g1", Value::Bool(true))
        .expect("gate record 1");
    heap.set_named_scalar(gate, "g2", Value::Bool(true))
        .expect("gate record 2");
    let g0_record = heap.locate_named(gate.slot(), "g0").expect("gate g0").1;
    let g1_record = heap.locate_named(gate.slot(), "g1").expect("gate g1").1;
    let g2_record = heap.locate_named(gate.slot(), "g2").expect("gate g2").1;
    let frame = heap.enter_roots(4).expect("roots");
    heap.write_root(frame, 0, gate).expect("root gate");
    heap.write_root(frame, 1, a).expect("root a");
    heap.write_root(frame, 2, b).expect("root b");
    heap.write_root(frame, 3, target).expect("root target");

    {
        let mut parked = ParkedMark::start(&mut heap).expect("start mark");
        pause_at(&mut parked, gate.slot(), g0_record);
        assert!(
            parked.marked(target),
            "the target owner is marked before the writes"
        );
        assert!(
            !parked
                .pending()
                .map(|scan| scan.owner == target.slot())
                .unwrap_or(false),
            "the target owner is already fully scanned"
        );
        let before = parked.meter();
        let mut expected_cursor = g1_record;
        for (index, value) in [b, a].into_iter().enumerate() {
            parked
                .with_saved_heap(|heap| heap.set_edge(target, KEY_X, value))
                .expect("overwrite the fixed live edge");
            let before_pulse = parked.meter();
            assert_eq!(
                before_pulse.mode_late_events, before.mode_late_events,
                "write {index}: a known-mode overwrite must not add a deferred event"
            );
            assert_eq!(
                before_pulse.mode_late_lookups, before.mode_late_lookups,
                "write {index}: a known-mode overwrite must not route through the late lookup"
            );
            let view = parked.pulse(1);
            assert!(!view.finished, "the unrelated gate still has records");
            let now = view.meter;
            assert_eq!(
                now.outgoing_edge_checks - before_pulse.outgoing_edge_checks,
                1,
                "write {index}: the pulse must advance one unrelated ordinary Gray edge"
            );
            assert_eq!(
                now.mode_late_events, before.mode_late_events,
                "write {index}: no late work is created by the overwrite"
            );
            assert_eq!(
                now.mode_late_deferred, before.mode_late_deferred,
                "write {index}: no deferred late unit is consumed"
            );
            assert_eq!(
                parked.pending().map(|scan| (scan.owner, scan.cursor)),
                Some((gate.slot(), expected_cursor)),
                "write {index}: the gate cursor advances in the same pass"
            );
            if index == 0 {
                expected_cursor = g2_record;
            }
        }
        assert_eq!(parked.phase_name(), "gray", "no ModeLate window intervened");
        assert!(
            parked.marked(a) && parked.marked(b),
            "both values were already marked roots"
        );
        let mut view = parked.pulse(64);
        while !view.finished {
            view = parked.pulse(64);
        }
        parked.complete().expect("sweep the starvation mark");
    }

    assert!(heap.alive(target) && heap.alive(gate));
    assert!(heap.alive(a) && heap.alive(b));
    assert_eq!(
        heap.edge(target, KEY_X),
        Ok(Some(a)),
        "the latest written value stays on the original live edge"
    );
    assert_eq!(
        heap.locate_named(target.slot(), "__mode"),
        None,
        "the known strong metatable has no mode record"
    );
    let meter = heap.scan_meter();
    assert_eq!(meter.mode_late_events, 0, "no event was ever queued");
    assert_eq!(meter.mode_late_deferred, 0, "no deferred unit was consumed");
}

/// Late parses are stored outside the reset-sized vector. Three live
/// metatables are queued: a high slot first, then a reused lower slot, then a
/// third higher slot. The intrusive queue is write-ordered, the map insert is
/// one charged store per parse, and no indexed vector or discovery sidecar is
/// sized. Two owners receive the deferred transition checks; repeated writes
/// coalesce instead of extending the backlog.
#[test]
fn late_mode_queue_is_bounded_by_live_sites_and_stores_outside_the_vectors() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits(32, 8, 48)).expect("install");
        let graph = heap::with_heap(&mut host, plant_shared_mode_graph).expect("plant");
        heap::open_fixed_step(&mut host).expect("open");
        let mut spaces = None;
        resume_weak(&mut host, |parked| {
            pause_after_both_weak_edges(parked, &graph);
            let before = parked.meter();
            let high = grow_one_table(parked);
            parked
                .with_saved_heap(|heap| heap.set_mode_scalar(high, Value::string("v")))
                .expect("write the high-slot mode first");
            // `dust` is an early, unrooted slot. Releasing it and allocating
            // again gives the second metatable a lower slot than the first.
            parked
                .with_saved_heap(|heap| heap.rollback(graph.dust))
                .expect("release the early slot");
            let low = grow_one_table(parked);
            assert!(
                low.slot() < high.slot(),
                "the second metatable reuses a lower slot after the first is queued"
            );
            parked
                .with_saved_heap(|heap| heap.set_mode_scalar(low, Value::string("v")))
                .expect("write the lower-slot mode second");
            let third = grow_one_table(parked);
            parked
                .with_saved_heap(|heap| heap.set_mode_scalar(third, Value::string("v")))
                .expect("write a third unattached mode");
            attach_shared_metatable(parked, &graph, low);
            parked
                .with_saved_heap(|heap| heap.set_metatable(graph.left, Some(high)))
                .expect("attach the left owner to the high-slot mode");
            // Repeats must coalesce on the slot tags and owner records.
            parked
                .with_saved_heap(|heap| heap.set_mode_scalar(high, Value::string("v")))
                .expect("rewrite the queued mode in place");
            parked
                .with_saved_heap(|heap| -> Result<(), HeapError> {
                    heap.set_metatable(graph.left, Some(high))?;
                    heap.set_metatable(graph.right, Some(low))?;
                    Ok(())
                })
                .expect("reapply the same attachments");
            let after = parked.meter();
            assert_eq!(
                after.mode_late_registrations - before.mode_late_registrations,
                3,
                "three live metatables queue one parse each, not one per write"
            );
            assert_eq!(
                after.mode_late_cancels - before.mode_late_cancels,
                1,
                "the repeat write restarts the queued high-slot parse in place"
            );
            assert_eq!(
                after.mode_late_events - before.mode_late_events,
                2,
                "one deferred owner check per live owner, not per setter"
            );
            assert_eq!(
                after.mode_late_event_reuses - before.mode_late_event_reuses,
                1,
                "the later high-slot setter reuses the left owner check; a same-tuple setter does not touch the queue"
            );
            assert_eq!(
                after.weak_mode_bytes, before.weak_mode_bytes,
                "the setter reads no mode byte"
            );
            assert_eq!(
                after.scratch.weak_modes_inited, before.scratch.weak_modes_inited,
                "the setter does not size the indexed vector"
            );
            assert_eq!(
                after.scratch.mode_discover_inited, before.scratch.mode_discover_inited,
                "the setter does not size the discovery sidecar"
            );

            let mut pulses = 0u32;
            loop {
                let prior = parked.meter();
                let view = parked.pulse(1);
                let now = view.meter;
                let bytes = now.weak_mode_bytes - prior.weak_mode_bytes;
                let opened = now.weak_mode_edge_checks - prior.weak_mode_edge_checks;
                let parsed = now.mode_late_parsed - prior.mode_late_parsed;
                let deferred = now.mode_late_deferred - prior.mode_late_deferred;
                let heads = now.owner_head_checks - prior.owner_head_checks;
                let edges = now.outgoing_edge_checks - prior.outgoing_edge_checks;
                assert!(
                    bytes <= 1
                        && opened <= 1
                        && parsed <= 1
                        && deferred <= 1
                        && heads <= 1
                        && edges <= 1,
                    "pulse {pulses} exceeded budget 1: bytes={bytes} open={opened} parsed={parsed} deferred={deferred} heads={heads} edges={edges}"
                );
                if parked.phase_name() != "mode-late" {
                    break;
                }
                assert!(
                    !view.finished,
                    "the cycle finished inside the bounded late window"
                );
                pulses += 1;
                assert!(pulses < 512, "the late window did not drain");
            }
            let end = parked.meter();
            assert_eq!(
                end.mode_late_parsed - before.mode_late_parsed,
                3,
                "each live metatable stores exactly one parsed mode"
            );
            assert_eq!(
                end.scratch.weak_modes_inited, before.scratch.weak_modes_inited,
                "the late parses store outside the reset-sized vector"
            );
            assert_eq!(
                end.scratch.mode_discover_inited, before.scratch.mode_discover_inited,
                "the late parses store outside the discovery sidecar"
            );
            assert_eq!(
                end.mode_late_deferred - before.mode_late_deferred,
                2,
                "only the two queued owner checks are consumed"
            );
            assert!(
                parked
                    .metatable(graph.left)
                    .expect("left metatable")
                    .is_some()
            );
            assert!(
                parked
                    .metatable(graph.right)
                    .expect("right metatable")
                    .is_some()
            );
            spaces = Some((high, low, third));
        });
        finish_shared_mode_cycle(&mut host);
        heap::with_heap(&mut host, |heap| -> Result<(), HeapError> {
            let (high, low, third) = spaces.expect("metatable spaces");
            assert!(
                heap.alive(high) && heap.alive(low),
                "the attached metatables are marked through their owners"
            );
            assert!(
                !heap.alive(third),
                "an unattached metatable is not rooted by its queued mode parse"
            );
            assert_eq!(heap.metatable(graph.left)?, Some(high));
            assert_eq!(heap.metatable(graph.right)?, Some(low));
            let outcome = read_shared_mode_outcome(heap, &graph)?;
            assert_eq!(
                (
                    outcome.left_value,
                    outcome.left_child,
                    outcome.right_value,
                    outcome.right_child,
                ),
                (false, false, false, false),
                "both owners stay weak under their final metatables"
            );
            assert_eq!(outcome.left_edge, None);
            assert_eq!(outcome.right_edge, None);
            assert!(!outcome.orphan);
            let meter = heap.scan_meter();
            assert!(meter.mode_late_parsed >= 3, "three late parses were stored");
            Ok(())
        })
        .expect("bounded late queue");
    });
}

// ---------------------------------------------------------------------------
// Re-review 0248456: deferred replay movement of the ordinary Gray chain is
// measured.
// ---------------------------------------------------------------------------

struct GrayChain {
    nodes: Vec<(u32, u32)>,
    access: (u64, u64, u64),
    arena_capacity: usize,
}

/// The exact ordinary Gray chain, the actual link accesses that built it, and
/// the capacity of the slot arena that stores the nodes. This replaces the old
/// Vec length/capacity shape: every reading is a real chain node, not a
/// constant.
fn gray_chain(parked: &mut ParkedMark<'_>) -> GrayChain {
    parked.with_saved_heap(|heap| {
        let nodes = heap
            .saved_mark_gray_snapshot()
            .expect("saved mark ordinary Gray chain");
        let access = heap
            .saved_mark_gray_access()
            .expect("saved mark ordinary Gray access");
        let arena_capacity = heap
            .saved_mark_gray_arena_capacity()
            .expect("saved mark ordinary Gray arena");
        GrayChain {
            nodes,
            access,
            arena_capacity,
        }
    })
}

/// Drive the late byte reader with budget one until one whole mode parse is
/// stored. The pulse that completes the parse does not also consume a deferred
/// owner or edge check.
fn pulse_until_late_parsed(parked: &mut ParkedMark<'_>) {
    let base = parked.meter().mode_late_parsed;
    let mut pulses = 0u32;
    while parked.meter().mode_late_parsed == base {
        let view = parked.pulse(1);
        assert!(!view.finished, "the cycle finished before the mode parse");
        pulses += 1;
        assert!(pulses < 256, "the mode parse did not finish");
    }
    assert_eq!(
        parked.meter().mode_late_parsed - base,
        1,
        "one parse store per completed late mode"
    );
}

/// Consume exactly one deferred owner check. The ordinary Gray chain must not
/// move: the replay targets use their own slot-linked queue.
fn consume_one_deferred(parked: &mut ParkedMark<'_>, label: &str) {
    let before = parked.meter();
    let chain = gray_chain(parked);
    let view = parked.pulse(1);
    assert!(!view.finished, "{label}: the gate work remains");
    let now = view.meter;
    assert_eq!(
        now.mode_late_deferred - before.mode_late_deferred,
        1,
        "{label}: one deferred unit is consumed"
    );
    assert_eq!(
        now.scratch.work_pushed, before.scratch.work_pushed,
        "{label}: an owner check does not push to Gray"
    );
    assert_eq!(
        gray_chain(parked).nodes,
        chain.nodes,
        "{label}: the ordinary Gray chain did not move"
    );
}

fn fill_gray_to_len(parked: &mut ParkedMark<'_>, target: usize) -> GrayChain {
    loop {
        let chain = gray_chain(parked);
        if chain.nodes.len() >= target {
            return chain;
        }
        let need = target - chain.nodes.len();
        let mut batch = Vec::with_capacity(need);
        for _ in 0..need {
            let filler = parked
                .with_saved_heap(|heap| heap.alloc())
                .expect("gray filler");
            batch.push(super::Rooted::Table(filler));
        }
        parked
            .accept_root_batch(&batch)
            .expect("fill the ordinary Gray chain");
    }
}

// The former `late_deferred_gray_push_reports_the_actual_capacity_boundaries`
// test encoded the rejected tradeoff: a deferred replay pushing the shared
// Gray Vec and relocating it at a capacity boundary. The deferred replay now
// uses an intrusive list on the slot records, so that behavior is replaced by
// `late_deferred_key_and_value_replay_does_not_move_the_gray_vec_at_a_real_boundary`.

// ---------------------------------------------------------------------------
// Review ba3c944, item 1: a mode first attached after a zero-attachment Reset.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum FirstAttachArm {
    /// Business: no metatable is attached at Reset; the metatable carries a
    /// prewritten direct `v`; that first attachment happens while Gray is
    /// parked before the unscanned owner opens.
    ZeroAttachmentAttach,
    /// Control: same heap, no attachment at all; the owner scan stays strong
    /// and keeps its value.
    OmittedAttachment,
    /// Control: the same metatable is attached before Reset, so the reset
    /// walk parses its direct mode.
    ParsedAtReset,
}

struct FirstAttachGraph {
    mt: TableRef,
    owner: TableRef,
    gate: TableRef,
    gate_record: u32,
    value: TableRef,
    child: TableRef,
}

fn plant_first_attach(heap: &mut LuaHeap, attach_at_reset: bool) -> FirstAttachGraph {
    let mt = heap.alloc().expect("first-attach metatable");
    heap.set_mode_scalar(mt, Value::string("v"))
        .expect("prewrite the direct v");
    let owner = heap.alloc().expect("first-attach owner");
    let value = heap.alloc().expect("first-attach value");
    let child = heap.alloc().expect("first-attach child");
    heap.set_named_edge(owner, "subj", value)
        .expect("owner subj");
    heap.set_named_edge(value, "next", child)
        .expect("value next");
    let gate = heap.alloc().expect("first-attach gate");
    heap.set_named_scalar(gate, "g0", Value::Bool(true))
        .expect("gate g0");
    heap.set_named_scalar(gate, "g1", Value::Bool(true))
        .expect("gate g1");
    let (_, gate_record) = heap
        .locate_named(gate.slot(), "g1")
        .expect("gate g1 record");
    if attach_at_reset {
        heap.set_metatable(owner, Some(mt))
            .expect("attach before Reset");
    }
    let frame = heap.enter_roots(2).expect("first-attach roots");
    heap.write_root(frame, 0, owner).expect("root owner");
    heap.write_root(frame, 1, gate).expect("root gate");
    FirstAttachGraph {
        mt,
        owner,
        gate,
        gate_record,
        value,
        child,
    }
}

fn run_first_attach(arm: FirstAttachArm) {
    let mut heap = LuaHeap::new(limits(20, 8, 32));
    let graph = plant_first_attach(&mut heap, matches!(arm, FirstAttachArm::ParsedAtReset));
    match arm {
        FirstAttachArm::ZeroAttachmentAttach | FirstAttachArm::OmittedAttachment => {
            assert_eq!(
                heap.metatable_edges, 0,
                "no metatable is attached when Reset runs"
            );
        }
        FirstAttachArm::ParsedAtReset => assert_eq!(heap.metatable_edges, 1),
    }
    assert!(
        heap.slots[graph.mt.slot() as usize].mode_edge.is_some(),
        "the metatable carries a direct __mode record"
    );
    heap.take_scan_meter();
    let mut registrations = 0u64;
    let mut events = 0u64;
    {
        let mut parked = ParkedMark::start(&mut heap).expect("start first-attach mark");
        pause_at(&mut parked, graph.gate.slot(), graph.gate_record);
        assert!(parked.marked(graph.owner), "the owner is queued");
        assert!(!parked.marked(graph.value), "its weak value starts white");
        assert!(
            parked.pending().map(|scan| scan.owner) != Some(graph.owner.slot()),
            "the owner has not opened yet"
        );
        let before = parked.meter();
        match arm {
            FirstAttachArm::ZeroAttachmentAttach => {
                parked
                    .with_saved_heap(|heap| heap.set_metatable(graph.owner, Some(graph.mt)))
                    .expect("first attachment during Gray");
                assert_eq!(
                    parked.with_saved_heap(|heap| heap.named_value(graph.mt, "__mode")),
                    Ok(Value::string("v")),
                    "the raw field really holds v"
                );
                let after = parked.meter();
                registrations = after.mode_late_registrations - before.mode_late_registrations;
                events = after.mode_late_events - before.mode_late_events;
            }
            FirstAttachArm::OmittedAttachment | FirstAttachArm::ParsedAtReset => {
                assert_eq!(
                    parked.meter().mode_late_registrations,
                    before.mode_late_registrations,
                    "this arm attaches nothing during Gray"
                );
            }
        }
        let _ = finish(&mut parked);
        parked.complete().expect("sweep first-attach mark");
    }
    match arm {
        FirstAttachArm::ZeroAttachmentAttach => {
            assert!(
                !heap.alive(graph.value),
                "a mode first attached after a zero-attachment Reset must clear the weak value"
            );
            assert!(!heap.alive(graph.child), "its strong successor clears");
            assert_eq!(heap.named_edge(graph.owner, "subj"), Ok(None));
            assert_eq!(
                registrations, 1,
                "the first attachment registers exactly one late parse"
            );
            assert_eq!(
                events, 1,
                "the first attachment records the owner transition while the mode is unknown"
            );
        }
        FirstAttachArm::OmittedAttachment => {
            assert!(
                heap.alive(graph.value),
                "without an attachment the owner scan stays strong"
            );
            assert!(heap.alive(graph.child));
            assert_eq!(heap.named_edge(graph.owner, "subj"), Ok(Some(graph.value)));
        }
        FirstAttachArm::ParsedAtReset => {
            assert!(
                !heap.alive(graph.value),
                "an attachment present at Reset is parsed there"
            );
            assert!(!heap.alive(graph.child));
            assert_eq!(heap.named_edge(graph.owner, "subj"), Ok(None));
        }
    }
    assert!(heap.alive(graph.owner));
    if !matches!(arm, FirstAttachArm::OmittedAttachment) {
        assert!(heap.alive(graph.mt));
    }
}

#[test]
fn late_first_attachment_after_a_zero_attachment_reset_reads_the_prewritten_v() {
    run_first_attach(FirstAttachArm::ZeroAttachmentAttach);
}

#[test]
fn late_first_attachment_omitted_control_keeps_the_strong_value() {
    run_first_attach(FirstAttachArm::OmittedAttachment);
}

#[test]
fn late_first_attachment_parsed_at_reset_control_clears_the_weak_value() {
    run_first_attach(FirstAttachArm::ParsedAtReset);
}

// ---------------------------------------------------------------------------
// Review 6ca8874, item 1: a strong `__mode` consumed by Reset must not be
// queued again by the first attachment. The inspected field version and an
// effective weak-mode cache are different observations.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum ResetCheckedStrongArm {
    /// Business: an unrelated metatable is attached at Reset, so the slot walk
    /// consumes the prewritten `x` on a metatable that is still unattached.
    /// That metatable is then attached to a queued owner during Gray, its
    /// field is rewritten to `v` in place, and the owner has not opened yet.
    UnrelatedAttachThenFirstAttach,
    /// Control: the same metatable is attached to the owner at Reset. The
    /// walk consumes `x` there, and the Gray rewrite to `v` is read without a
    /// first-attachment registration.
    AttachedAtReset,
}

struct ResetCheckedStrongGraph {
    mt: TableRef,
    owner: TableRef,
    gate: TableRef,
    gate_record: u32,
    value: TableRef,
    child: TableRef,
    orphan: TableRef,
    /// The independent Reset-time attachment. Both slots stay unrooted: they
    /// only make the Reset slot walk read every direct mode edge.
    keeper: Option<(TableRef, TableRef)>,
}

fn plant_reset_checked_strong(
    heap: &mut LuaHeap,
    arm: ResetCheckedStrongArm,
) -> Result<ResetCheckedStrongGraph, HeapError> {
    let mt = heap.alloc()?;
    heap.set_mode_scalar(mt, Value::string("x"))?;
    let owner = heap.alloc()?;
    let value = heap.alloc()?;
    let child = heap.alloc()?;
    let orphan = heap.alloc()?;
    heap.set_named_edge(owner, "subj", value)?;
    heap.set_named_edge(value, "next", child)?;
    let keeper = if matches!(arm, ResetCheckedStrongArm::UnrelatedAttachThenFirstAttach) {
        let keeper_owner = heap.alloc()?;
        let keeper_mt = heap.alloc()?;
        heap.set_metatable(keeper_owner, Some(keeper_mt))?;
        Some((keeper_owner, keeper_mt))
    } else {
        heap.set_metatable(owner, Some(mt))?;
        None
    };
    let gate = heap.alloc()?;
    heap.set_named_scalar(gate, "g0", Value::Bool(true))?;
    heap.set_named_scalar(gate, "g1", Value::Bool(true))?;
    let (_, gate_record) = heap
        .locate_named(gate.slot(), "g1")
        .expect("gate g1 record");
    let frame = heap.enter_roots(2)?;
    heap.write_root(frame, 0, owner)?;
    heap.write_root(frame, 1, gate)?;
    Ok(ResetCheckedStrongGraph {
        mt,
        owner,
        gate,
        gate_record,
        value,
        child,
        orphan,
        keeper,
    })
}

fn run_reset_checked_strong(arm: ResetCheckedStrongArm) {
    let mut heap = LuaHeap::new(limits(24, 8, 32));
    let graph = plant_reset_checked_strong(&mut heap, arm).expect("plant reset-checked strong");
    assert_eq!(
        heap.metatable_edges, 1,
        "exactly one metatable is attached when Reset runs"
    );
    assert!(
        heap.slots[graph.mt.slot() as usize].mode_edge.is_some(),
        "the metatable carries a direct __mode record"
    );
    heap.take_scan_meter();
    let mut parked = ParkedMark::start(&mut heap).expect("start reset-checked strong mark");
    pause_at(&mut parked, graph.gate.slot(), graph.gate_record);
    assert!(parked.marked(graph.owner), "the owner is queued");
    assert!(
        parked.pending().map(|scan| scan.owner) != Some(graph.owner.slot()),
        "the owner has not opened yet"
    );
    assert!(
        !parked.marked(graph.value) && !parked.marked(graph.child),
        "the weak value and its successor start white"
    );
    assert!(
        parked.meter().weak_mode_bytes >= 1,
        "the Reset slot walk really consumed the prewritten x byte"
    );
    assert_eq!(
        parked.with_saved_heap(|heap| heap.named_value(graph.mt, "__mode")),
        Ok(Value::string("x")),
        "the raw field really holds x before the attach or rewrite"
    );
    let attach_registrations = match arm {
        ResetCheckedStrongArm::UnrelatedAttachThenFirstAttach => {
            let before = parked.meter().mode_late_registrations;
            parked
                .with_saved_heap(|heap| heap.set_metatable(graph.owner, Some(graph.mt)))
                .expect("first attachment during Gray");
            let added = parked.meter().mode_late_registrations - before;
            assert!(added <= 1, "one attachment queues at most one late read");
            if added > 0 {
                // Freeze the `6ca8874` path: let the redundant read finish so
                // the in-place rewrite is attempted after its strong stamp.
                pulse_until_late_parsed(&mut parked);
            }
            added
        }
        ResetCheckedStrongArm::AttachedAtReset => 0,
    };
    assert_eq!(
        parked.with_saved_heap(|heap| heap.named_value(graph.mt, "__mode")),
        Ok(Value::string("x")),
        "the attach did not change the raw field"
    );
    let registrations_before_write = parked.meter().mode_late_registrations;
    parked
        .with_saved_heap(|heap| heap.set_mode_scalar(graph.mt, Value::string("v")))
        .expect("rewrite the direct field to v in place");
    assert_eq!(
        parked.with_saved_heap(|heap| heap.named_value(graph.mt, "__mode")),
        Ok(Value::string("v")),
        "the raw field really holds the rewritten v"
    );
    let write_registrations = parked.meter().mode_late_registrations - registrations_before_write;
    assert!(
        write_registrations <= 1,
        "one rewrite queues at most one late read"
    );
    let parsed_before_write = parked.meter().mode_late_parsed;
    if write_registrations > 0 {
        pulse_until_late_parsed(&mut parked);
        assert_eq!(
            parked.meter().mode_late_parsed - parsed_before_write,
            1,
            "the in-place rewrite is read exactly once"
        );
    }
    assert!(
        parked.pending().map(|scan| scan.owner) != Some(graph.owner.slot()),
        "the parse did not open the owner"
    );
    assert!(
        !parked.marked(graph.value) && !parked.marked(graph.child),
        "the unscanned owner's weak value and successor stay white"
    );
    let _ = finish(&mut parked);
    parked.complete().expect("sweep reset-checked strong mark");
    drop(parked);

    assert!(heap.alive(graph.owner), "the rooted owner survives");
    assert!(heap.alive(graph.mt), "the attached metatable survives");
    assert!(
        !heap.alive(graph.value),
        "the unscanned owner must read the late v and clear its value \
         (attach registrations {attach_registrations}, write registrations {write_registrations})"
    );
    assert!(
        !heap.alive(graph.child),
        "the strong successor clears with it"
    );
    assert!(!heap.alive(graph.orphan), "the independent orphan clears");
    assert_eq!(
        heap.named_edge(graph.owner, "subj"),
        Ok(None),
        "the weak entry clears at entry level"
    );
    assert_eq!(
        attach_registrations, 0,
        "a Reset-inspected field version needs no first-attachment re-read"
    );
    assert_eq!(
        write_registrations, 1,
        "the x -> v rewrite must be read before the owner opens"
    );
    if let Some((keeper_owner, keeper_mt)) = graph.keeper {
        assert!(
            !heap.alive(keeper_owner) && !heap.alive(keeper_mt),
            "the independent Reset-time attachment is not rooted"
        );
    }
}

#[test]
fn late_first_attachment_does_not_reread_a_reset_checked_strong_mode() {
    run_reset_checked_strong(ResetCheckedStrongArm::UnrelatedAttachThenFirstAttach);
}

#[test]
fn late_first_attachment_reset_control_reads_an_in_place_rewrite() {
    run_reset_checked_strong(ResetCheckedStrongArm::AttachedAtReset);
}

// ---------------------------------------------------------------------------
// Whole-slice review deleg_f0957a44: a late strong result at a zero-attachment
// Reset is a field observation, not an authoritative weak cache. The next
// in-place rewrite must be read before the unscanned owner opens.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum LateStrongArm {
    /// Business: attach after a zero-attachment Reset, finish the late `x`
    /// read, then rewrite the same record to `v` before the owner opens.
    StrongThenWeak,
    /// Control: the same stop point keeps `x` for this round; a later round
    /// that reads `v` clears the same original graph.
    StrongKeep,
}

struct LateStrongGraph {
    mt: TableRef,
    owner: TableRef,
    gate: TableRef,
    gate_record: u32,
    value: TableRef,
    child: TableRef,
    orphan: TableRef,
}

fn plant_late_strong(heap: &mut LuaHeap) -> LateStrongGraph {
    let mt = heap.alloc().expect("late-strong metatable");
    heap.set_mode_scalar(mt, Value::string("x"))
        .expect("prewrite the direct x");
    let owner = heap.alloc().expect("late-strong owner");
    let value = heap.alloc().expect("late-strong value");
    let child = heap.alloc().expect("late-strong child");
    heap.set_named_edge(owner, "subj", value)
        .expect("owner subj");
    heap.set_named_edge(value, "next", child)
        .expect("value next");
    let orphan = heap.alloc().expect("late-strong orphan");
    let gate = heap.alloc().expect("late-strong gate");
    heap.set_named_scalar(gate, "g0", Value::Bool(true))
        .expect("gate g0");
    heap.set_named_scalar(gate, "g1", Value::Bool(true))
        .expect("gate g1");
    let (_, gate_record) = heap
        .locate_named(gate.slot(), "g1")
        .expect("gate g1 record");
    let frame = heap.enter_roots(2).expect("late-strong roots");
    heap.write_root(frame, 0, owner).expect("root owner");
    heap.write_root(frame, 1, gate).expect("root gate");
    LateStrongGraph {
        mt,
        owner,
        gate,
        gate_record,
        value,
        child,
        orphan,
    }
}

fn run_late_strong(arm: LateStrongArm) {
    let mut heap = LuaHeap::new(limits(24, 8, 32));
    let graph = plant_late_strong(&mut heap);
    assert_eq!(
        heap.metatable_edges, 0,
        "no metatable is attached when Reset runs"
    );
    assert!(
        heap.slots[graph.mt.slot() as usize].mode_edge.is_some(),
        "the metatable carries a direct __mode record"
    );
    assert_eq!(
        heap.slots[graph.mt.slot() as usize].mode_checked_generation,
        0,
        "the prewritten field has no observation yet"
    );
    heap.take_scan_meter();
    let mut write_registrations = 0;
    let mut rewrite_parses = 0;
    {
        let mut parked = ParkedMark::start(&mut heap).expect("start late-strong mark");
        pause_at(&mut parked, graph.gate.slot(), graph.gate_record);
        assert!(parked.marked(graph.owner), "the owner is queued");
        assert!(
            parked.pending().map(|scan| scan.owner) != Some(graph.owner.slot()),
            "the owner has not opened yet"
        );
        assert!(
            !parked.marked(graph.value) && !parked.marked(graph.child),
            "the weak value and its successor start white"
        );
        assert!(
            !parked.marked(graph.orphan),
            "the independent orphan starts white"
        );
        assert_eq!(
            parked.meter().weak_mode_bytes,
            0,
            "a zero-attachment Reset reads no __mode bytes"
        );
        assert_eq!(
            parked.with_saved_heap(|heap| heap.named_value(graph.mt, "__mode")),
            Ok(Value::string("x")),
            "the raw field really holds the unread x"
        );
        let before_attach = parked.meter();
        parked
            .with_saved_heap(|heap| heap.set_metatable(graph.owner, Some(graph.mt)))
            .expect("first attachment during Gray");
        let after_attach = parked.meter();
        assert_eq!(
            after_attach.mode_late_registrations - before_attach.mode_late_registrations,
            1,
            "the unread field registers one late read"
        );
        assert_eq!(
            after_attach.mode_late_events - before_attach.mode_late_events,
            1,
            "the attach records one deferred transition while the mode is unknown"
        );
        pulse_until_late_parsed(&mut parked);
        assert_eq!(
            parked.with_saved_heap(|heap| heap.named_value(graph.mt, "__mode")),
            Ok(Value::string("x")),
            "the late read sees the raw x"
        );
        assert!(
            parked.pending().map(|scan| scan.owner) != Some(graph.owner.slot()),
            "finishing the late read did not open the owner"
        );
        assert!(
            !parked.marked(graph.value) && !parked.marked(graph.child),
            "a finished strong read does not mark the value or its successor"
        );
        let before_reattach = parked.meter();
        parked
            .with_saved_heap(|heap| -> Result<(), HeapError> {
                heap.set_metatable(graph.owner, None)?;
                heap.set_metatable(graph.owner, Some(graph.mt))
            })
            .expect("detach and reattach the same mode version");
        assert_eq!(
            parked.meter().mode_late_registrations,
            before_reattach.mode_late_registrations,
            "a same-version reattachment must not requeue the inspected field"
        );
        let before_write = parked.meter();
        match arm {
            LateStrongArm::StrongThenWeak => {
                parked
                    .with_saved_heap(|heap| heap.set_mode_scalar(graph.mt, Value::string("v")))
                    .expect("rewrite x -> v in place");
                assert_eq!(
                    parked.with_saved_heap(|heap| heap.named_value(graph.mt, "__mode")),
                    Ok(Value::string("v")),
                    "the raw field really holds the rewritten v"
                );
                let after_write = parked.meter();
                write_registrations =
                    after_write.mode_late_registrations - before_write.mode_late_registrations;
                let parsed_before = after_write.mode_late_parsed;
                if write_registrations > 0 {
                    pulse_until_late_parsed(&mut parked);
                }
                rewrite_parses = parked.meter().mode_late_parsed - parsed_before;
                assert_eq!(
                    parked.with_saved_heap(|heap| heap.named_value(graph.mt, "__mode")),
                    Ok(Value::string("v")),
                    "the late rewrite bytes stay exact"
                );
                assert!(
                    parked.pending().map(|scan| scan.owner) != Some(graph.owner.slot()),
                    "the rewritten read did not open the owner"
                );
                assert!(
                    !parked.marked(graph.value) && !parked.marked(graph.child),
                    "the rewrite did not mark the value before the owner scan"
                );
            }
            LateStrongArm::StrongKeep => {}
        }
        let _ = finish(&mut parked);
        parked.complete().expect("sweep late-strong mark");
    }
    assert!(heap.alive(graph.owner), "the rooted owner survives");
    assert!(heap.alive(graph.mt), "the attached metatable survives");
    assert!(
        !heap.alive(graph.orphan),
        "the independent orphan clears in both arms"
    );
    match arm {
        LateStrongArm::StrongThenWeak => {
            assert!(
                !heap.alive(graph.value),
                "the unscanned owner must read the late v and clear its value \
                 (write registrations {write_registrations}, rewrite parses {rewrite_parses})"
            );
            assert!(
                !heap.alive(graph.child),
                "the strong successor clears with the weak value"
            );
            assert_eq!(
                heap.named_edge(graph.owner, "subj"),
                Ok(None),
                "the weak entry clears at entry level"
            );
            assert_eq!(
                write_registrations, 1,
                "a rewrite after a finished strong read must register its own late read"
            );
            assert_eq!(
                rewrite_parses, 1,
                "the rewritten field is read exactly once"
            );
        }
        LateStrongArm::StrongKeep => {
            assert!(
                heap.alive(graph.value),
                "keeping x leaves the original value strongly reachable"
            );
            assert!(
                heap.alive(graph.child),
                "and the successor behind that value"
            );
            assert_eq!(
                heap.named_edge(graph.owner, "subj"),
                Ok(Some(graph.value)),
                "the strong entry stays live"
            );
            heap.set_mode_scalar(graph.mt, Value::string("v"))
                .expect("re-weaken the kept field after the round");
            heap.take_scan_meter();
            let mut second = ParkedMark::start(&mut heap).expect("start the re-weaken round");
            let _ = finish(&mut second);
            second.complete().expect("sweep the re-weaken round");
            drop(second);
            assert!(
                !heap.alive(graph.value),
                "the next round reads v and clears the kept original value"
            );
            assert!(
                !heap.alive(graph.child),
                "the successor clears with the kept value"
            );
            assert_eq!(
                heap.named_edge(graph.owner, "subj"),
                Ok(None),
                "the kept entry clears in the re-weaken round"
            );
        }
    }
}

#[test]
fn late_strong_result_does_not_block_a_same_round_weak_rewrite() {
    run_late_strong(LateStrongArm::StrongThenWeak);
}

#[test]
fn late_strong_keep_control_survives_and_the_next_round_reweak_clears() {
    run_late_strong(LateStrongArm::StrongKeep);
}

// ---------------------------------------------------------------------------
// Review ba3c944, item 2: a same-tuple rescan must re-read the mode before the
// next record is charged.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum SameTupleRescanArm {
    /// Business: the rescan opened strong; `v` is written on the same
    /// metatable tuple and parsed before the first record is charged.
    LateWeakSameTuple,
    /// Control: the same write is strong (`x`); the rescan stays strong.
    LateStrongSameTuple,
    /// Control: no rewrite; the strong rescan keeps its cached strength.
    NoRewrite,
}

fn run_same_tuple_rescan(arm: SameTupleRescanArm) {
    let mut heap = LuaHeap::new(limits(16, 8, 24));
    let graph = plant_shared_mode_graph(&mut heap).expect("plant shared mode graph");
    let mut parked = ParkedMark::start(&mut heap).expect("start same-tuple rescan mark");
    pause_after_both_weak_edges(&mut parked, &graph);
    parked
        .with_saved_heap(|heap| -> Result<(), HeapError> {
            heap.set_metatable(graph.left, Some(graph.strong_mt))?;
            heap.set_metatable(graph.right, Some(graph.strong_mt))?;
            Ok(())
        })
        .expect("switch both owners to the strong metatable");
    let mut opened = false;
    for _ in 0..HARNESS_BOUND {
        if let Some(pending) = parked.pending_rescan()
            && pending.owner == graph.left.slot()
            && pending.cursor == graph.left_value_record
        {
            opened = true;
            break;
        }
        assert!(
            !parked.pulse(1).finished,
            "the cycle finished before the left rescan reached its first record"
        );
    }
    assert!(
        opened,
        "the left rescan head must be open right before its value record"
    );
    assert!(
        !parked.marked(graph.left_value),
        "the left target is white when the rescan head opens"
    );
    assert!(
        parked.marked(graph.right_value) && parked.marked(graph.right_child),
        "the right owner already resumed strong before the same-tuple write"
    );

    let parsed_before = parked.meter().mode_late_parsed;
    if !matches!(arm, SameTupleRescanArm::NoRewrite) {
        let mode = match arm {
            SameTupleRescanArm::LateWeakSameTuple => "v",
            SameTupleRescanArm::LateStrongSameTuple => "x",
            SameTupleRescanArm::NoRewrite => unreachable!(),
        };
        parked
            .with_saved_heap(|heap| heap.set_mode_scalar(graph.strong_mt, Value::string(mode)))
            .expect("write the same metatable tuple");
        while parked.meter().mode_late_parsed == parsed_before {
            let view = parked.pulse(1);
            assert!(!view.finished, "the cycle finished inside the mode parse");
        }
        assert_eq!(
            parked.meter().mode_late_parsed - parsed_before,
            1,
            "one parse store for the same-tuple write"
        );
        assert_eq!(
            parked
                .pending_rescan()
                .map(|scan| (scan.owner, scan.cursor)),
            Some((graph.left.slot(), graph.left_value_record)),
            "the parse did not consume the rescan record"
        );
    }

    let edges_before = parked.meter().outgoing_edge_checks;
    let view = parked.pulse(1);
    assert_eq!(
        view.meter.outgoing_edge_checks - edges_before,
        1,
        "the rescan record is the single charged unit"
    );
    let expect_weak = matches!(arm, SameTupleRescanArm::LateWeakSameTuple);
    if expect_weak {
        assert!(
            !parked.marked(graph.left_value),
            "the same-tuple v must be re-read; the white record target stays white"
        );
    } else {
        assert!(
            parked.marked(graph.left_value),
            "a strong rescan keeps its target marked"
        );
    }
    let _ = finish(&mut parked);
    parked.complete().expect("sweep same-tuple rescan mark");
    drop(parked);

    assert!(heap.alive(graph.left) && heap.alive(graph.right));
    assert!(heap.alive(graph.right_value) && heap.alive(graph.right_child));
    if expect_weak {
        assert!(
            !heap.alive(graph.left_value) && !heap.alive(graph.left_child),
            "the strong rescan must resume weak after the same-tuple v"
        );
        assert_eq!(heap.named_edge(graph.left, "subj"), Ok(None));
        assert_eq!(
            heap.named_edge(graph.right, "subj"),
            Ok(Some(graph.right_value)),
            "the already-rescanned right side keeps its marked value"
        );
    } else {
        assert!(
            heap.alive(graph.left_value) && heap.alive(graph.left_child),
            "the left graph survives a strong same-tuple mode"
        );
        assert_eq!(
            heap.named_edge(graph.left, "subj"),
            Ok(Some(graph.left_value))
        );
    }
    assert!(!heap.alive(graph.orphan));
}

#[test]
fn same_tuple_rescan_reads_a_late_v_before_the_next_record() {
    run_same_tuple_rescan(SameTupleRescanArm::LateWeakSameTuple);
}

#[test]
fn same_tuple_rescan_late_strong_control_keeps_the_target() {
    run_same_tuple_rescan(SameTupleRescanArm::LateStrongSameTuple);
}

#[test]
fn same_tuple_rescan_no_rewrite_control_keeps_the_target() {
    run_same_tuple_rescan(SameTupleRescanArm::NoRewrite);
}

// ---------------------------------------------------------------------------
// Review ba3c944, item 3: a deletion stamp from an earlier round must not
// shadow a same-generation k -> v transition.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum DeletionStampArm {
    /// Business: round 1 arms the deletion stamp on the live metatable; in
    /// round 2 its direct field is `k` before the owner scan, then the owner
    /// is moved to a `v` metatable.
    StaleStampSameMetatable,
    /// Control: the same transition uses a fresh `k` metatable created in
    /// round 2, so no earlier stamp applies.
    FreshKMetatable,
}

struct StampFixture {
    m: TableRef,
    owner: TableRef,
    gate: TableRef,
    gate_record: u32,
}

fn plant_stamp_fixture(heap: &mut LuaHeap) -> StampFixture {
    let m = heap.alloc().expect("stamp metatable");
    let owner = heap.alloc().expect("stamp owner");
    heap.set_metatable(owner, Some(m))
        .expect("attach the empty mode metatable before round 1");
    let gate = heap.alloc().expect("stamp gate");
    heap.set_named_scalar(gate, "g0", Value::Bool(true))
        .expect("gate g0");
    heap.set_named_scalar(gate, "g1", Value::Bool(true))
        .expect("gate g1");
    let (_, gate_record) = heap
        .locate_named(gate.slot(), "g1")
        .expect("gate g1 record");
    let frame = heap.enter_roots(3).expect("stamp roots");
    heap.write_root(frame, 0, owner).expect("root owner");
    heap.write_root(frame, 1, m).expect("root metatable");
    heap.write_root(frame, 2, gate).expect("root gate");
    StampFixture {
        m,
        owner,
        gate,
        gate_record,
    }
}

/// Round 1: with no metatable attached at Reset, a prewritten `v` is parsed
/// late on `m`, then the direct field is deleted. The delete arms the
/// slot-fallback discovery stamp for `m`'s generation.
fn arm_stale_deletion_stamp(heap: &mut LuaHeap, fixture: &StampFixture) {
    let mut parked = ParkedMark::start(heap).expect("start stamp round");
    pause_at(&mut parked, fixture.gate.slot(), fixture.gate_record);
    assert!(parked.marked(fixture.owner) && parked.marked(fixture.m));
    parked
        .with_saved_heap(|heap| heap.set_mode_scalar(fixture.m, Value::string("v")))
        .expect("write the round-1 v");
    let parsed_before = parked.meter().mode_late_parsed;
    while parked.meter().mode_late_parsed == parsed_before {
        let view = parked.pulse(1);
        assert!(
            !view.finished,
            "the cycle finished inside the round-1 parse"
        );
    }
    parked
        .with_saved_heap(|heap| heap.set_mode_scalar(fixture.m, Value::Null))
        .expect("delete the direct mode");
    parked.with_saved_heap(|heap| {
        let slot = &heap.slots[fixture.m.slot() as usize];
        assert_eq!(slot.discover_generation, fixture.m.generation());
        assert!(
            slot.discover_batch != 0,
            "the deleted-mode stamp is armed on the slot fallback"
        );
    });
    let _ = finish(&mut parked);
    parked.complete().expect("sweep stamp round");
    drop(parked);
    assert!(heap.alive(fixture.m) && heap.alive(fixture.owner) && heap.alive(fixture.gate));
}

fn run_deletion_stamp(arm: DeletionStampArm) {
    let mut heap = LuaHeap::new(limits(32, 8, 48));
    let fixture = plant_stamp_fixture(&mut heap);
    arm_stale_deletion_stamp(&mut heap, &fixture);
    heap.take_scan_meter();

    let key = heap.alloc().expect("stamp object key");
    let child = heap.alloc().expect("stamp key child");
    heap.set_named_edge(key, "next", child)
        .expect("key successor");

    let current = match arm {
        DeletionStampArm::StaleStampSameMetatable => fixture.m,
        DeletionStampArm::FreshKMetatable => heap.alloc().expect("fresh k metatable"),
    };
    let mut parked = ParkedMark::start(&mut heap).expect("start stamp round two");
    pause_at(&mut parked, fixture.gate.slot(), fixture.gate_record);
    assert!(
        parked.marked(fixture.owner),
        "the owner is queued before the round-two mode write"
    );
    assert!(
        parked.pending().map(|scan| scan.owner) != Some(fixture.owner.slot()),
        "the owner has not opened yet"
    );
    match arm {
        DeletionStampArm::StaleStampSameMetatable => {
            parked
                .with_saved_heap(|heap| heap.set_mode_scalar(fixture.m, Value::string("k")))
                .expect("write the round-two k on the stamped metatable");
        }
        DeletionStampArm::FreshKMetatable => {
            parked
                .with_saved_heap(|heap| -> Result<(), HeapError> {
                    heap.set_mode_scalar(current, Value::string("k"))?;
                    heap.set_metatable(fixture.owner, Some(current))
                })
                .expect("write and attach the fresh k metatable while its mode is queued");
        }
    }
    let parsed_before = parked.meter().mode_late_parsed;
    while parked.meter().mode_late_parsed == parsed_before {
        let view = parked.pulse(1);
        assert!(
            !view.finished,
            "the cycle finished inside the round-two k parse"
        );
    }
    if matches!(arm, DeletionStampArm::FreshKMetatable) {
        // The attach queued one deferred owner check. It is not the k -> v
        // transition we are measuring.
        let deferred_now = parked.meter().mode_late_deferred;
        while parked.meter().mode_late_deferred == deferred_now {
            let view = parked.pulse(1);
            assert!(!view.finished, "the cycle finished inside the attach check");
        }
    }
    let schedules_before = parked.meter().owner_rescan_schedules;

    parked
        .with_saved_heap(|heap| heap.set_object_scalar(fixture.owner, key, Value::string("ok")))
        .expect("write the object key while the k mode is known");
    let record = parked
        .object_index(fixture.owner, key)
        .expect("object key record");
    assert!(
        !parked.marked(key),
        "a keys-weak mode must leave the object key white before the owner opens"
    );

    let mut opened = false;
    for _ in 0..HARNESS_BOUND {
        if let Some(pending) = parked.pending()
            && pending.owner == fixture.owner.slot()
            && pending.cursor == record
        {
            opened = true;
            break;
        }
        assert!(
            !parked.pulse(1).finished,
            "the cycle finished before the owner opened on its key record"
        );
    }
    assert!(
        opened,
        "the owner must open on the object-key record under k"
    );
    let _ = parked.pulse(1);
    assert!(
        !parked.marked(key),
        "charging the key record under k must not mark the weak key"
    );

    let final_mt = parked
        .with_saved_heap(|heap| heap.alloc())
        .expect("final v metatable");
    parked
        .with_saved_heap(|heap| heap.set_mode_scalar(final_mt, Value::string("v")))
        .expect("prewrite the final v");
    parked
        .with_saved_heap(|heap| heap.set_metatable(fixture.owner, Some(final_mt)))
        .expect("move the owner to the v metatable");
    assert_eq!(
        parked.meter().owner_rescan_schedules,
        schedules_before,
        "an unresolved final mode defers the transition check"
    );
    let deferred_now = parked.meter().mode_late_deferred;
    while parked.meter().mode_late_deferred == deferred_now {
        let view = parked.pulse(1);
        assert!(
            !view.finished,
            "the cycle finished inside the final v parse"
        );
    }
    let schedules_after_check = parked.meter().owner_rescan_schedules - schedules_before;

    let _ = finish(&mut parked);
    parked.complete().expect("sweep stamp round two");
    drop(parked);

    assert_eq!(heap.metatable(fixture.owner), Ok(Some(final_mt)));
    assert!(heap.alive(fixture.owner) && heap.alive(fixture.m) && heap.alive(final_mt));
    let keep_key = matches!(arm, DeletionStampArm::FreshKMetatable) || schedules_after_check == 1;
    if keep_key {
        assert!(
            heap.alive(key),
            "{arm:?}: the original strong key must survive the k -> v transition"
        );
        assert!(
            heap.alive(child),
            "{arm:?}: the key's descendant must survive"
        );
        assert_eq!(
            heap.find_key(fixture.owner.slot(), &super::EdgeKey::object(key)),
            Some(record),
            "{arm:?}: the current edge keeps the original record"
        );
    } else {
        assert!(
            !heap.alive(key),
            "{arm:?}: the stale deletion stamp shadowed the k -> v transition"
        );
        assert!(!heap.alive(child));
        assert_eq!(
            heap.find_key(fixture.owner.slot(), &super::EdgeKey::object(key)),
            None
        );
    }
    if matches!(arm, DeletionStampArm::StaleStampSameMetatable) {
        assert_eq!(
            schedules_after_check, 1,
            "the same-generation k -> v transition must schedule one strong rescan"
        );
    }
    assert!(heap.alive(fixture.gate), "the gate is rooted");
}

#[test]
fn stale_deletion_stamp_does_not_shadow_a_same_generation_k_to_v_transition() {
    run_deletion_stamp(DeletionStampArm::StaleStampSameMetatable);
}

#[test]
fn fresh_k_metatable_control_keeps_the_original_key_across_k_to_v() {
    run_deletion_stamp(DeletionStampArm::FreshKMetatable);
}

// ---------------------------------------------------------------------------
// Review ba3c944, item 4: a deferred edge payload queued while the mode was
// unknown must be updated or cancelled by a later known-mode overwrite.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum ObsoleteEventArm {
    ReplaceWithTable,
    ReplaceWithScalar,
    KeepTheOldTarget,
}

fn run_obsolete_deferred_event(arm: ObsoleteEventArm) {
    let mut heap = LuaHeap::new(limits(20, 8, 32));
    let owner = heap.alloc().expect("event owner");
    let old = heap.alloc().expect("event old value");
    let first = heap.alloc().expect("event first replacement");
    let second = heap.alloc().expect("event second replacement");
    heap.set_named_edge(owner, "subj", old)
        .expect("fixed live event edge");
    let mt = heap.alloc().expect("event metatable");
    let gate = heap.alloc().expect("event gate");
    heap.set_named_scalar(gate, "g0", Value::Bool(true))
        .expect("gate g0");
    heap.set_named_scalar(gate, "g1", Value::Bool(true))
        .expect("gate g1");
    let (_, gate_record) = heap
        .locate_named(gate.slot(), "g1")
        .expect("gate g1 record");
    let frame = heap.enter_roots(2).expect("event roots");
    heap.write_root(frame, 0, owner).expect("root owner");
    heap.write_root(frame, 1, gate).expect("root gate");
    let record = heap
        .locate_named(owner.slot(), "subj")
        .expect("event subj record")
        .1;

    let mut parked = ParkedMark::start(&mut heap).expect("start event mark");
    pause_at(&mut parked, gate.slot(), gate_record);
    assert!(parked.marked(owner));
    // Queue the live target while the owner's metatable mode is unknown.
    parked
        .with_saved_heap(|heap| heap.set_mode_scalar(mt, Value::string("x")))
        .expect("write the pending strong mode");
    parked
        .with_saved_heap(|heap| heap.set_metatable(owner, Some(mt)))
        .expect("attach the pending strong metatable");
    parked
        .with_saved_heap(|heap| heap.set_named_edge(owner, "subj", first))
        .expect("write the first target while the mode is unknown");
    assert!(
        !parked.marked(first),
        "the unknown mode defers the first target"
    );
    assert_eq!(
        parked.with_saved_heap(|heap| heap.edge_slots[record as usize].late_value),
        Some((first.slot(), first.generation())),
        "the first target really sits on the queued record"
    );

    // Finish the strong parse, but do not consume the deferred edge event yet.
    let parsed_before = parked.meter().mode_late_parsed;
    while parked.meter().mode_late_parsed == parsed_before {
        let view = parked.pulse(1);
        assert!(!view.finished, "the cycle finished inside the event parse");
    }
    assert_eq!(
        parked.meter().mode_late_deferred,
        0,
        "the deferred edge event has not been consumed yet"
    );
    let events_before = parked.meter().mode_late_events;
    match arm {
        ObsoleteEventArm::ReplaceWithTable => {
            parked
                .with_saved_heap(|heap| heap.set_named_edge(owner, "subj", second))
                .expect("replace the live edge with the second table");
        }
        ObsoleteEventArm::ReplaceWithScalar => {
            parked
                .with_saved_heap(|heap| {
                    heap.set_named_scalar(owner, "subj", Value::string("scalar"))
                })
                .expect("replace the live edge with a non-nil scalar");
        }
        ObsoleteEventArm::KeepTheOldTarget => {}
    }
    let events_after = parked.meter().mode_late_events;
    let payload_after_rewrite =
        parked.with_saved_heap(|heap| heap.edge_slots[record as usize].late_value);

    pulse_until_late_quiet(&mut parked);
    let _ = finish(&mut parked);
    parked.complete().expect("sweep obsolete-event mark");
    drop(parked);

    match arm {
        ObsoleteEventArm::KeepTheOldTarget => {
            assert!(
                heap.alive(first),
                "the live first target is retained when nothing overwrites it"
            );
            assert!(!heap.alive(second));
            assert_eq!(heap.named_edge(owner, "subj"), Ok(Some(first)));
            assert_eq!(
                payload_after_rewrite,
                Some((first.slot(), first.generation()))
            );
        }
        ObsoleteEventArm::ReplaceWithTable => {
            assert!(
                !heap.alive(first),
                "an overwritten deferred target must not be replayed after its parse"
            );
            assert!(heap.alive(second), "the live replacement survives");
            assert_eq!(heap.named_edge(owner, "subj"), Ok(Some(second)));
            assert_eq!(
                payload_after_rewrite,
                Some((second.slot(), second.generation())),
                "the queued payload is updated in place to the live replacement"
            );
        }
        ObsoleteEventArm::ReplaceWithScalar => {
            assert!(
                !heap.alive(first),
                "a table payload replaced by a non-nil scalar must not be replayed"
            );
            assert!(!heap.alive(second));
            assert_eq!(
                heap.named_value(owner, "subj"),
                Ok(Value::string("scalar")),
                "the live scalar edge keeps its value"
            );
            assert_eq!(
                payload_after_rewrite, None,
                "the queued table payload is cancelled"
            );
        }
    }
    assert_eq!(
        events_after, events_before,
        "{arm:?}: a known-mode overwrite creates no second deferred event"
    );
    assert_eq!(
        heap.locate_named(owner.slot(), "subj")
            .map(|(_, index)| index),
        Some(record),
        "{arm:?}: the live edge keeps its record identity"
    );
    assert!(!heap.alive(old), "the overwritten original value clears");
    assert!(heap.alive(owner) && heap.alive(gate) && heap.alive(mt));
}

#[test]
fn known_mode_rewrite_cancels_an_obsolete_deferred_table_payload() {
    run_obsolete_deferred_event(ObsoleteEventArm::ReplaceWithTable);
}

#[test]
fn known_mode_scalar_rewrite_cancels_an_obsolete_deferred_table_payload() {
    run_obsolete_deferred_event(ObsoleteEventArm::ReplaceWithScalar);
}

#[test]
fn known_mode_no_rewrite_control_keeps_the_queued_live_target() {
    run_obsolete_deferred_event(ObsoleteEventArm::KeepTheOldTarget);
}

// ---------------------------------------------------------------------------
// Review ba3c944, budget: deferred key+value replay must not push the shared
// Gray Vec at a real capacity boundary.
// ---------------------------------------------------------------------------

struct DeferredBoundaryFixture {
    owner: TableRef,
    gate: TableRef,
    gate_record: u32,
}

fn plant_deferred_boundary(heap: &mut LuaHeap) -> (DeferredBoundaryFixture, ParkedMark<'_>) {
    let owner = heap.alloc().expect("boundary owner");
    let old = heap.alloc().expect("boundary old value");
    heap.set_named_edge(owner, "subj", old)
        .expect("fixed boundary edge");
    let gate = heap.alloc().expect("boundary gate");
    heap.set_named_scalar(gate, "g0", Value::Bool(true))
        .expect("gate g0");
    heap.set_named_scalar(gate, "g1", Value::Bool(true))
        .expect("gate g1");
    let (_, gate_record) = heap
        .locate_named(gate.slot(), "g1")
        .expect("gate g1 record");
    let frame = heap.enter_roots(2).expect("boundary roots");
    heap.write_root(frame, 0, owner).expect("root owner");
    heap.write_root(frame, 1, gate).expect("root gate");
    let _ = heap.take_scan_meter();
    (
        DeferredBoundaryFixture {
            owner,
            gate,
            gate_record,
        },
        ParkedMark::start(heap).expect("start boundary mark"),
    )
}

#[test]
fn late_deferred_key_and_value_replay_does_not_move_the_gray_vec_at_a_real_boundary() {
    let mut heap = LuaHeap::new(limits(64, 16, 96));
    let (fixture, mut parked) = plant_deferred_boundary(&mut heap);
    pause_at(&mut parked, fixture.gate.slot(), fixture.gate_record);
    assert!(parked.marked(fixture.owner));
    let mt = parked
        .with_saved_heap(|heap| heap.alloc())
        .expect("boundary metatable");
    parked
        .with_saved_heap(|heap| heap.set_mode_scalar(mt, Value::string("x")))
        .expect("write the pending strong mode");
    parked
        .with_saved_heap(|heap| heap.set_metatable(fixture.owner, Some(mt)))
        .expect("attach the pending strong metatable");
    let key = parked
        .with_saved_heap(|heap| heap.alloc())
        .expect("boundary object key");
    let value = parked
        .with_saved_heap(|heap| heap.alloc())
        .expect("boundary table value");
    parked
        .with_saved_heap(|heap| heap.set_object_edge(fixture.owner, key, value))
        .expect("insert the table key and value while the mode is pending");
    let record = parked
        .object_index(fixture.owner, key)
        .expect("queued object record");
    assert!(
        !parked.marked(key) && !parked.marked(value),
        "both object targets start white"
    );
    assert_eq!(
        parked.with_saved_heap(|heap| heap.edge_slots[record as usize].late_key),
        Some((key.slot(), key.generation()))
    );
    assert_eq!(
        parked.with_saved_heap(|heap| heap.edge_slots[record as usize].late_value),
        Some((value.slot(), value.generation()))
    );

    pulse_until_late_parsed(&mut parked);
    consume_one_deferred(&mut parked, "boundary owner check");
    assert!(
        !parked.marked(key) && !parked.marked(value),
        "the edge event is still queued after the owner check"
    );

    let before_chain = fill_gray_to_len(&mut parked, 6);
    assert_eq!(
        before_chain.nodes.len(),
        6,
        "the ordinary Gray chain holds six real marked nodes before the replay"
    );
    assert!(
        before_chain
            .nodes
            .contains(&(fixture.owner.slot(), fixture.owner.generation())),
        "the pre-replay chain contains the real rooted owner"
    );
    let before = parked.meter();
    let view = parked.pulse(1);
    assert_eq!(
        view.meter.mode_late_deferred - before.mode_late_deferred,
        1,
        "the pulse consumes exactly one deferred edge record"
    );
    assert_eq!(
        view.meter.scratch.work_pushed - before.scratch.work_pushed,
        2,
        "the key and the value are both newly marked"
    );
    assert!(parked.marked(key) && parked.marked(value));
    let (key_tag, value_tag) = parked.with_saved_heap(|heap| {
        (
            heap.slots[key.slot() as usize].late_target_epoch,
            heap.slots[value.slot() as usize].late_target_epoch,
        )
    });
    assert_ne!(key_tag, 0, "the replay links the key target on its slot");
    assert_ne!(
        value_tag, 0,
        "the replay links the value target on its slot"
    );
    let after_chain = gray_chain(&mut parked);
    assert_eq!(
        after_chain.nodes, before_chain.nodes,
        "the deferred key+value replay must not push or move the ordinary Gray chain; \
         the replay targets use their own slot links"
    );
    assert_eq!(
        after_chain.access, before_chain.access,
        "the deferred key+value replay performs no ordinary Gray link access"
    );
    assert_eq!(
        after_chain.arena_capacity, before_chain.arena_capacity,
        "the deferred key+value replay does not grow the slot arena"
    );

    let edges_before = parked.meter().outgoing_edge_checks;
    let view = parked.pulse(1);
    assert_eq!(
        view.meter.outgoing_edge_checks - edges_before,
        1,
        "ordinary Gray still advances one edge after the replay"
    );

    let _ = finish(&mut parked);
    parked.complete().expect("sweep boundary mark");
    drop(parked);
    assert!(heap.alive(fixture.owner) && heap.alive(fixture.gate) && heap.alive(mt));
    assert!(
        heap.alive(key) && heap.alive(value),
        "both deferred targets are marked before Done"
    );
    assert_eq!(
        heap.find_key(fixture.owner.slot(), &super::EdgeKey::object(key)),
        Some(record),
        "the live object edge keeps its record"
    );
}

#[test]
fn late_deferred_replay_skips_an_already_marked_value_target() {
    let mut heap = LuaHeap::new(limits(48, 16, 72));
    let (fixture, mut parked) = plant_deferred_boundary(&mut heap);
    pause_at(&mut parked, fixture.gate.slot(), fixture.gate_record);
    let mt = parked
        .with_saved_heap(|heap| heap.alloc())
        .expect("marked metatable");
    parked
        .with_saved_heap(|heap| heap.set_mode_scalar(mt, Value::string("x")))
        .expect("pending strong mode");
    parked
        .with_saved_heap(|heap| heap.set_metatable(fixture.owner, Some(mt)))
        .expect("attach");
    let key = parked
        .with_saved_heap(|heap| heap.alloc())
        .expect("marked object key");
    let value = parked
        .with_saved_heap(|heap| heap.alloc())
        .expect("premarked value");
    parked
        .with_saved_heap(|heap| heap.set_object_edge(fixture.owner, key, value))
        .expect("queued insert");
    parked
        .accept_root_batch(&[super::Rooted::Table(value)])
        .expect("mark the value target through the root batch");
    assert!(parked.marked(value));
    pulse_until_late_parsed(&mut parked);
    consume_one_deferred(&mut parked, "marked owner check");
    let before = parked.meter();
    let view = parked.pulse(1);
    assert_eq!(view.meter.mode_late_deferred - before.mode_late_deferred, 1);
    assert_eq!(
        view.meter.scratch.work_pushed - before.scratch.work_pushed,
        1,
        "only the key is newly marked; the value target was already marked"
    );
    assert!(parked.marked(key) && parked.marked(value));
    let _ = finish(&mut parked);
    parked.complete().expect("sweep marked-target mark");
    drop(parked);
    assert!(heap.alive(key) && heap.alive(value));
}

#[test]
fn release_of_a_pending_replay_target_clears_its_queue_tag_and_reuse_is_clean() {
    let mut heap = LuaHeap::new(limits(48, 16, 72));
    let (fixture, mut parked) = plant_deferred_boundary(&mut heap);
    pause_at(&mut parked, fixture.gate.slot(), fixture.gate_record);
    let mt = parked
        .with_saved_heap(|heap| heap.alloc())
        .expect("release metatable");
    parked
        .with_saved_heap(|heap| heap.set_mode_scalar(mt, Value::string("x")))
        .expect("pending strong mode");
    parked
        .with_saved_heap(|heap| heap.set_metatable(fixture.owner, Some(mt)))
        .expect("attach");
    let key = parked
        .with_saved_heap(|heap| heap.alloc())
        .expect("release object key");
    let doomed = parked
        .with_saved_heap(|heap| heap.alloc())
        .expect("pending doomed value");
    parked
        .with_saved_heap(|heap| heap.set_object_edge(fixture.owner, key, doomed))
        .expect("queued insert");
    pulse_until_late_parsed(&mut parked);
    consume_one_deferred(&mut parked, "release owner check");
    let view = parked.pulse(1);
    assert_eq!(
        view.meter.mode_late_deferred, 2,
        "the owner check and the edge event are consumed before the target release"
    );
    assert!(parked.marked(key) && parked.marked(doomed));
    parked
        .with_saved_heap(|heap| heap.rollback(doomed))
        .expect("release the pending target before it opens");
    assert!(
        !parked.with_saved_heap(|heap| heap.alive(doomed)),
        "the pending target is really released"
    );
    let released_tags = parked.with_saved_heap(|heap| {
        let slot = &heap.slots[doomed.slot() as usize];
        (
            slot.late_target_epoch,
            slot.late_target_prev,
            slot.late_target_next,
        )
    });
    assert_eq!(
        released_tags,
        (0, None, None),
        "release unlinks the pending target and clears its queue tag"
    );
    let reused = parked
        .with_saved_heap(|heap| heap.alloc())
        .expect("reuse the released slot");
    assert_eq!(reused.slot(), doomed.slot());
    assert_ne!(reused.generation(), doomed.generation());
    let reused_tag =
        parked.with_saved_heap(|heap| heap.slots[reused.slot() as usize].late_target_epoch);
    assert_eq!(reused_tag, 0, "reuse inherits no replay tag");
    parked
        .accept_root_batch(&[super::Rooted::Table(reused)])
        .expect("root the reused slot");
    let _ = finish(&mut parked);
    parked.complete().expect("sweep released-target mark");
    drop(parked);
    assert!(!heap.alive(doomed), "the released pending target clears");
    assert!(heap.alive(reused), "the reused slot survives");
    assert!(heap.alive(key), "the key marked by the replay survives");
}

#[test]
fn abandoned_mark_clears_a_pending_replay_target_tag() {
    let mut heap = LuaHeap::new(limits(48, 16, 72));
    let (fixture, mut parked) = plant_deferred_boundary(&mut heap);
    pause_at(&mut parked, fixture.gate.slot(), fixture.gate_record);
    let mt = parked
        .with_saved_heap(|heap| heap.alloc())
        .expect("abandon metatable");
    parked
        .with_saved_heap(|heap| heap.set_mode_scalar(mt, Value::string("x")))
        .expect("pending strong mode");
    parked
        .with_saved_heap(|heap| heap.set_metatable(fixture.owner, Some(mt)))
        .expect("attach");
    let key = parked
        .with_saved_heap(|heap| heap.alloc())
        .expect("abandon object key");
    let doomed = parked
        .with_saved_heap(|heap| heap.alloc())
        .expect("abandon doomed value");
    parked
        .with_saved_heap(|heap| heap.set_object_edge(fixture.owner, key, doomed))
        .expect("queued insert");
    pulse_until_late_parsed(&mut parked);
    consume_one_deferred(&mut parked, "abandon owner check");
    let _ = parked.pulse(1);
    assert!(parked.marked(key) && parked.marked(doomed));
    let linked = parked.with_saved_heap(|heap| {
        (
            heap.slots[key.slot() as usize].late_target_epoch,
            heap.slots[doomed.slot() as usize].late_target_epoch,
        )
    });
    assert_ne!(linked.0, 0);
    assert_ne!(linked.1, 0);
    // Dropping without `park` abandons the saved mark. The old-epoch target
    // tags stay inert and release must clear them.
    drop(parked);
    heap.rollback(doomed)
        .expect("release the abandoned pending target");
    let released = (
        heap.slots[doomed.slot() as usize].late_target_epoch,
        heap.slots[doomed.slot() as usize].late_target_prev,
        heap.slots[doomed.slot() as usize].late_target_next,
    );
    assert_eq!(
        released,
        (0, None, None),
        "release under no mark clears the abandoned replay tag"
    );
    let reused = heap.alloc().expect("reuse the abandoned slot");
    assert_eq!(reused.slot(), doomed.slot());
    assert_eq!(
        heap.slots[reused.slot() as usize].late_target_epoch,
        0,
        "reuse starts with no inherited replay tag"
    );
}

// ---------------------------------------------------------------------------
// Review 6ca8874, late replay capacity: a replay-marked target is not a
// subgraph scan. Its own out-edge must still charge and expand the ordinary
// queue, including after a peer target is unlinked from the replay list.
// ---------------------------------------------------------------------------

#[test]
fn late_replay_target_expands_its_out_edge_after_a_peer_is_released() {
    let mut heap = LuaHeap::new(limits(48, 16, 72));
    let (fixture, mut parked) = plant_deferred_boundary(&mut heap);
    pause_at(&mut parked, fixture.gate.slot(), fixture.gate_record);
    let mt = parked
        .with_saved_heap(|heap| heap.alloc())
        .expect("out-edge metatable");
    parked
        .with_saved_heap(|heap| heap.set_mode_scalar(mt, Value::string("x")))
        .expect("pending strong mode");
    parked
        .with_saved_heap(|heap| heap.set_metatable(fixture.owner, Some(mt)))
        .expect("attach the pending metatable");
    let peer = parked
        .with_saved_heap(|heap| heap.alloc())
        .expect("released peer target");
    let target = parked
        .with_saved_heap(|heap| heap.alloc())
        .expect("surviving late target");
    let successor = parked
        .with_saved_heap(|heap| heap.alloc())
        .expect("white successor");
    parked
        .with_saved_heap(|heap| heap.set_named_edge(target, "next", successor))
        .expect("the target's only out-edge");
    parked
        .with_saved_heap(|heap| heap.set_object_edge(fixture.owner, peer, target))
        .expect("queue the object key and table value");
    let target_record = parked
        .with_saved_heap(|heap| {
            heap.locate_named(target.slot(), "next")
                .map(|(_, index)| index)
        })
        .expect("target next record");
    assert!(
        !parked.marked(peer) && !parked.marked(target) && !parked.marked(successor),
        "both replay targets and the successor start white"
    );

    pulse_until_late_parsed(&mut parked);
    consume_one_deferred(&mut parked, "out-edge owner check");
    assert!(
        !parked.marked(successor),
        "the queued edge record does not scan the target"
    );
    let before = parked.meter();
    let view = parked.pulse(1);
    assert!(!view.finished, "the cycle finished on the replay unit");
    assert_eq!(
        view.meter.mode_late_deferred - before.mode_late_deferred,
        1,
        "the object edge is one deferred replay unit"
    );
    assert_eq!(
        view.meter.scratch.work_pushed - before.scratch.work_pushed,
        2,
        "the key and the value are both newly marked"
    );
    assert!(parked.marked(peer) && parked.marked(target));
    assert!(
        !parked.marked(successor),
        "replay marking the late target is not a subgraph scan"
    );
    let tags = parked.with_saved_heap(|heap| {
        (
            heap.slots[peer.slot() as usize].late_target_epoch,
            heap.slots[target.slot() as usize].late_target_epoch,
        )
    });
    assert_ne!(tags.0, 0, "the peer is linked on the replay list");
    assert_ne!(tags.1, 0, "the late target is linked on the replay list");

    parked
        .with_saved_heap(|heap| heap.rollback(peer))
        .expect("release the peer target before it opens");
    let released = parked.with_saved_heap(|heap| {
        let slot = &heap.slots[peer.slot() as usize];
        (
            slot.late_target_epoch,
            slot.late_target_prev,
            slot.late_target_next,
        )
    });
    assert_eq!(
        released,
        (0, None, None),
        "release unlinks the peer from the replay list"
    );
    let survivor = parked.with_saved_heap(|heap| {
        let slot = &heap.slots[target.slot() as usize];
        (
            slot.late_target_epoch,
            slot.late_target_prev,
            slot.late_target_next,
        )
    });
    assert!(
        survivor.0 != 0 && survivor.1.is_none() && survivor.2.is_none(),
        "the surviving target becomes the repaired head and tail"
    );

    // Ordinary work runs first; the surviving replay target must still open
    // after the ordinary queue drains.
    let mut opened = false;
    for _ in 0..HARNESS_BOUND {
        if parked.pending().map(|scan| (scan.owner, scan.cursor))
            == Some((target.slot(), target_record))
        {
            opened = true;
            break;
        }
        assert!(
            !parked.marked(successor),
            "nothing marks the successor before the target opens"
        );
        let view = parked.pulse(1);
        assert!(
            !view.finished,
            "the cycle finished before the target opened"
        );
    }
    assert!(opened, "the surviving replay target actually opens");
    assert!(parked.marked(target));
    assert!(
        !parked.marked(successor),
        "opening the owner has not charged its out-edge yet"
    );
    let chain_before = gray_chain(&mut parked);
    let gray_len = chain_before.nodes.len();
    let before = parked.meter();
    let view = parked.pulse(1);
    assert!(
        !view.finished,
        "the cycle finished on the target's out-edge"
    );
    assert_eq!(
        view.meter.outgoing_edge_checks - before.outgoing_edge_checks,
        1,
        "the surviving target charges exactly one out-edge"
    );
    assert_eq!(
        view.meter.scratch.work_pushed - before.scratch.work_pushed,
        1,
        "the white successor is newly marked by that out-edge"
    );
    let chain_after = gray_chain(&mut parked);
    assert_eq!(
        chain_after.nodes.len(),
        gray_len + 1,
        "the ordinary queue actually expands"
    );
    assert_eq!(
        chain_after.nodes.first(),
        Some(&(successor.slot(), successor.generation())),
        "the white successor is pushed at the LIFO head"
    );
    assert_eq!(
        chain_after.access.0 - chain_before.access.0,
        1,
        "one push reads exactly one node link"
    );
    assert_eq!(
        chain_after.access.1 - chain_before.access.1,
        if chain_before.nodes.is_empty() { 1 } else { 2 },
        "one push writes the new node and, past an old head, one predecessor link"
    );
    assert_eq!(
        chain_after.access.2 - chain_before.access.2,
        1,
        "one push updates the head header once"
    );
    assert_eq!(
        chain_after.arena_capacity, chain_before.arena_capacity,
        "one push does not grow the slot arena"
    );
    assert!(parked.marked(successor));

    let _ = finish(&mut parked);
    parked.complete().expect("sweep out-edge replay mark");
    drop(parked);
    assert!(!heap.alive(peer), "the released peer clears");
    assert!(
        heap.alive(target) && heap.alive(successor),
        "the surviving target and its scanned successor are marked"
    );
    assert!(heap.alive(fixture.owner) && heap.alive(fixture.gate) && heap.alive(mt));
    assert_eq!(
        heap.named_edge(target, "next"),
        Ok(Some(successor)),
        "the strong successor edge stays live"
    );
}

// ---------------------------------------------------------------------------
// Ordinary Gray LIFO: real pushes at a filled chain, middle and neighbour
// retirement, and round boundaries.
// ---------------------------------------------------------------------------

fn trace_position(trace: &[OpenTrace], slot: u32, generation: u32) -> Option<usize> {
    trace
        .iter()
        .position(|entry| entry.slot == slot && entry.generation == generation)
}

fn assert_unique_chain(chain: &GrayChain, label: &str) {
    for (index, node) in chain.nodes.iter().enumerate() {
        assert!(
            !chain.nodes[index + 1..].contains(node),
            "{label}: one slot joined the ordinary Gray chain twice: {node:?}"
        );
    }
}

/// A real named strong edge at the head of a filled ordinary chain. The
/// measured pulse marks one white target and pushes it at the LIFO head with
/// a fixed number of link accesses; it does not allocate, copy, or relocate a
/// work vector. The gate owner is open on the record before a scalar
/// sentinel, so the measured unit is exactly that edge.
#[test]
fn ordinary_gray_strong_edge_pushes_without_growing_the_slot_arena() {
    let mut heap = LuaHeap::new(limits(64, 8, 64));
    let gate = heap.alloc().expect("chain gate");
    let target = heap.alloc().expect("chain target");
    heap.set_named_edge(gate, "strong", target)
        .expect("gate named strong edge");
    heap.set_named_scalar(gate, "sentinel", Value::Bool(true))
        .expect("gate scalar sentinel");
    let (_, strong_record) = heap
        .locate_named(gate.slot(), "strong")
        .expect("named strong record");
    let (_, sentinel_record) = heap
        .locate_named(gate.slot(), "sentinel")
        .expect("scalar sentinel record");
    assert_eq!(
        heap.edge_slots[strong_record as usize].next,
        Some(sentinel_record),
        "the scalar sentinel is the record after the named strong edge"
    );
    let frame = heap.enter_roots(1).expect("chain roots");
    heap.write_root(frame, 0, gate).expect("root chain gate");
    let _ = heap.take_scan_meter();

    let mut parked = ParkedMark::start(&mut heap).expect("chain mark");
    pause_at(&mut parked, gate.slot(), strong_record);
    assert!(parked.marked(gate));
    assert!(
        !parked.marked(target),
        "the named strong target is white before the measured pulse"
    );
    let pending = parked.pending().expect("strong edge is pending");
    assert_eq!(pending.owner, gate.slot());
    assert_eq!(pending.cursor, strong_record);

    let filled = fill_gray_to_len(&mut parked, 6);
    assert_eq!(
        filled.nodes.len(),
        6,
        "six real marked nodes fill the ordinary chain"
    );
    assert_unique_chain(&filled, "filled chain");
    assert!(
        !filled.nodes.contains(&(target.slot(), target.generation())),
        "the measured target has not joined the chain yet"
    );
    let tail_before = *filled.nodes.last().expect("filled chain has a tail");
    let before = parked.meter();

    let view = parked.pulse(1);
    assert!(
        !view.finished,
        "the measured pulse stops on the scalar sentinel, not Done"
    );
    assert!(
        parked.marked(target),
        "the real named strong edge marks the white target"
    );
    let pending = parked.pending().expect("the gate cursor is still open");
    assert_eq!(
        pending.owner,
        gate.slot(),
        "the measured edge keeps the owner"
    );
    assert_eq!(
        pending.cursor, sentinel_record,
        "one unit moves the cursor from the strong edge to the scalar sentinel"
    );
    assert_eq!(
        view.meter.outgoing_edge_checks - before.outgoing_edge_checks,
        1,
        "the named strong edge is exactly one charged outgoing unit"
    );
    assert_eq!(
        view.meter.scratch.work_pushed - before.scratch.work_pushed,
        1,
        "the white target is exactly one newly pushed mark target"
    );
    assert_eq!(
        view.meter.owner_head_checks, before.owner_head_checks,
        "the measured edge does not open another owner head"
    );

    let after = gray_chain(&mut parked);
    assert_eq!(
        after.nodes.len(),
        7,
        "the new white target joins the ordinary Gray chain"
    );
    assert_eq!(
        after.nodes.first(),
        Some(&(target.slot(), target.generation())),
        "the new target is pushed at the LIFO head"
    );
    assert_eq!(
        after.nodes.last(),
        Some(&tail_before),
        "the old tail is not moved or copied by the push"
    );
    assert_unique_chain(&after, "measured push");
    assert_eq!(
        after.access.0 - filled.access.0,
        1,
        "one push reads exactly one node link"
    );
    assert_eq!(
        after.access.1 - filled.access.1,
        2,
        "one push writes the new node and the old head's predecessor link"
    );
    assert_eq!(
        after.access.2 - filled.access.2,
        1,
        "one push updates the head header once"
    );
    assert_eq!(
        after.arena_capacity, filled.arena_capacity,
        "one push does not grow the slot arena"
    );

    let _ = finish(&mut parked);
    parked.complete().expect("chain sweep");
    drop(parked);
    assert!(
        heap.alive(gate) && heap.alive(target),
        "the rooted gate and the marked target stay live"
    );
}

/// Same `[tail, mid, top, gate]` roots with no rollback. The original middle
/// item opens normally after `top` and before `tail`.
fn ordinary_gray_middle_without_rollback_control() {
    let mut heap = LuaHeap::new(limits(32, 8, 32));
    let tail = heap.alloc().expect("control tail");
    let tail_child = heap.alloc().expect("control tail child");
    heap.set_named_edge(tail, "child", tail_child)
        .expect("control tail edge");
    let mid = heap.alloc().expect("control mid");
    let top = heap.alloc().expect("control top");
    let gate = heap.alloc().expect("control gate");
    heap.set_named_scalar(gate, "g0", Value::Bool(true))
        .expect("control gate scalar");
    let (_, gate_record) = heap
        .locate_named(gate.slot(), "g0")
        .expect("control gate record");
    let frame = heap.enter_roots(4).expect("control roots");
    heap.write_root(frame, 0, tail).expect("control root tail");
    heap.write_root(frame, 1, mid).expect("control root mid");
    heap.write_root(frame, 2, top).expect("control root top");
    heap.write_root(frame, 3, gate).expect("control root gate");
    let _ = heap.take_scan_meter();

    let mut parked = ParkedMark::start(&mut heap).expect("control mark");
    clear_open_trace();
    pause_at(&mut parked, gate.slot(), gate_record);
    assert_eq!(
        open_trace().len(),
        1,
        "only gate opens before its scalar record"
    );
    assert!(parked.marked(top) && parked.marked(mid) && parked.marked(tail));
    assert!(
        !parked.marked(tail_child),
        "the tail child is white before the tail opens"
    );

    for _ in 0..HARNESS_BOUND {
        if parked.pulse(1).finished {
            break;
        }
    }
    let trace = open_trace();
    let top_pos = trace_position(&trace, top.slot(), top.generation()).expect("control top opens");
    let mid_pos = trace_position(&trace, mid.slot(), mid.generation())
        .unwrap_or_else(|| panic!("control mid did not open: {trace:?}"));
    let tail_pos =
        trace_position(&trace, tail.slot(), tail.generation()).expect("control tail opens");
    assert!(
        top_pos < mid_pos && mid_pos < tail_pos,
        "the no-rollback control opens top, then the original mid, then tail"
    );
    assert!(trace[mid_pos].live && trace[mid_pos].marked);
    assert!(
        parked.marked(tail_child),
        "the control tail scan marks its exclusive child"
    );

    parked.complete().expect("control sweep");
    drop(parked);
    assert!(
        heap.alive(tail) && heap.alive(mid) && heap.alive(top) && heap.alive(gate),
        "the no-rollback control keeps every original root"
    );
    assert!(
        heap.alive(tail_child),
        "the no-rollback control keeps the tail child"
    );
}

/// Rolling back a middle ordinary Gray item unlinks its exact identity before
/// the slot generation advances, stitches its neighbours, and never opens the
/// reused generation. The original tail and its exclusive child still scan.
#[test]
fn ordinary_gray_middle_rollback_does_not_open_reused_generation() {
    ordinary_gray_middle_without_rollback_control();

    let mut heap = LuaHeap::new(limits(32, 8, 32));
    let tail = heap.alloc().expect("tail");
    let tail_child = heap.alloc().expect("tail child");
    heap.set_named_edge(tail, "child", tail_child)
        .expect("exclusive tail child edge");
    let mid = heap.alloc().expect("middle Gray item");
    let top = heap.alloc().expect("top Gray item");
    let gate = heap.alloc().expect("gate");
    heap.set_named_scalar(gate, "g0", Value::Bool(true))
        .expect("gate scalar record");
    let (_, gate_record) = heap
        .locate_named(gate.slot(), "g0")
        .expect("gate record identity");
    let weak_mt = heap.alloc().expect("weak metatable");
    heap.set_mode_scalar(weak_mt, Value::string("v"))
        .expect("weak value mode");
    let weak_owner = heap.alloc().expect("weak owner");
    heap.set_metatable(weak_owner, Some(weak_mt))
        .expect("weak metatable attach");
    let orphan = heap.alloc().expect("weak orphan");
    heap.set_named_edge(weak_owner, "dead", orphan)
        .expect("weak orphan edge");

    let frame = heap.enter_roots(6).expect("rollback roots");
    heap.write_root(frame, 0, tail).expect("root tail");
    heap.write_root(frame, 1, mid).expect("root mid");
    heap.write_root(frame, 2, top).expect("root top");
    heap.write_root(frame, 3, gate).expect("root gate");
    heap.write_root(frame, 4, weak_mt)
        .expect("root weak metatable");
    heap.write_root(frame, 5, weak_owner)
        .expect("root weak owner");
    let _ = heap.take_scan_meter();

    let mut parked = ParkedMark::start(&mut heap).expect("rollback mark");
    clear_open_trace();
    pause_at(&mut parked, gate.slot(), gate_record);
    assert!(parked.marked(top) && parked.marked(mid) && parked.marked(tail));
    assert!(
        !parked.marked(tail_child),
        "the tail child is white before the tail is scanned"
    );
    assert!(
        !parked.marked(orphan),
        "the weak orphan is white before sweep"
    );
    let opened_before = open_trace();
    assert_eq!(
        opened_before.len(),
        3,
        "weak owner, weak metatable, and gate opened before the scalar"
    );
    assert!(
        !opened_before.iter().any(|entry| {
            entry.slot == tail.slot() || entry.slot == mid.slot() || entry.slot == top.slot()
        }),
        "top, mid, and tail are still unopened on the ordinary chain"
    );
    let (top_id, mid_id, tail_id) = (
        (top.slot(), top.generation()),
        (mid.slot(), mid.generation()),
        (tail.slot(), tail.generation()),
    );
    let chain_before = gray_chain(&mut parked);
    assert_eq!(
        chain_before.nodes,
        vec![top_id, mid_id, tail_id],
        "the ordinary chain is head top, middle mid, tail tail"
    );
    assert_unique_chain(&chain_before, "middle rollback before");

    let owners = parked.with_saved_heap(|heap| heap.slots.len());
    let visits_before = parked.with_saved_heap(|heap| heap.rollback_owner_visits());
    parked
        .with_saved_heap(|heap| heap.rollback(mid))
        .expect("real table rollback of the middle Gray item");
    let visits_after = parked.with_saved_heap(|heap| heap.rollback_owner_visits());
    assert_eq!(
        visits_after - visits_before,
        owners as u64,
        "the existing rollback walk still visits every owner independently of the chain unlink"
    );
    assert!(
        !parked.with_saved_heap(|heap| heap.alive(mid)),
        "rollback really releases the middle item"
    );
    let chain_after_rollback = gray_chain(&mut parked);
    assert_eq!(
        chain_after_rollback.nodes,
        vec![top_id, tail_id],
        "the middle item is unlinked and its predecessor now reaches the tail"
    );
    assert_eq!(
        (
            chain_after_rollback.access.0 - chain_before.access.0,
            chain_after_rollback.access.1 - chain_before.access.1,
            chain_after_rollback.access.2 - chain_before.access.2,
        ),
        (1, 3, 0),
        "a middle release reads one node, writes two neighbours plus its own clear, and moves no header"
    );
    assert_eq!(
        chain_after_rollback.arena_capacity, chain_before.arena_capacity,
        "the middle release does not grow the slot arena"
    );

    let fresh = parked
        .with_saved_heap(|heap| heap.alloc())
        .expect("reuse the released middle slot");
    assert_eq!(
        fresh.slot(),
        mid.slot(),
        "the new generation reuses the slot"
    );
    assert_ne!(
        fresh.generation(),
        mid.generation(),
        "the generation differs"
    );
    let probe = parked
        .with_saved_heap(|heap| heap.alloc())
        .expect("white probe");
    parked
        .with_saved_heap(|heap| heap.set_named_edge(fresh, "child", probe))
        .expect("the new generation's exclusive white probe edge");
    assert!(
        parked.with_saved_heap(|heap| heap.alive(fresh)),
        "the reused generation is Live"
    );
    assert!(
        !parked.marked(fresh) && !parked.marked(probe),
        "the reused generation and its probe both start white"
    );
    assert!(
        !parked.with_saved_heap(|heap| heap.table_in_gray(fresh)),
        "the released middle item no longer names the reused slot"
    );
    assert_eq!(
        gray_chain(&mut parked).nodes,
        vec![top_id, tail_id],
        "reuse does not inherit a chain position"
    );

    let mut pulses = 0u32;
    let mut trace_seen = opened_before.len();
    loop {
        let view = parked.pulse(1);
        pulses += 1;
        let trace = open_trace();
        for entry in &trace[trace_seen..] {
            assert!(
                !(entry.slot == fresh.slot() && entry.generation == fresh.generation()),
                "the ordinary chain must not open the reused generation: {entry:?}"
            );
        }
        trace_seen = trace.len();
        assert!(
            !parked.marked(fresh),
            "the reused white generation never becomes marked"
        );
        assert!(
            !parked.marked(probe),
            "the white probe is never reached through the released middle item"
        );
        if view.finished {
            break;
        }
        let chain = gray_chain(&mut parked);
        assert!(
            !chain.nodes.contains(&(fresh.slot(), fresh.generation())),
            "the reused generation never joins the ordinary chain"
        );
        assert!(
            !chain.nodes.contains(&mid_id),
            "the released middle identity is gone from the ordinary chain"
        );
        assert!(
            pulses < HARNESS_BOUND,
            "the business mark did not finish within the harness bound"
        );
    }

    let trace = open_trace();
    let top_pos = trace_position(&trace, top.slot(), top.generation()).expect("top opens");
    let tail_pos = trace_position(&trace, tail.slot(), tail.generation()).expect("tail opens");
    assert!(
        top_pos < tail_pos,
        "the surviving ordinary chain still opens top before tail: {trace:?}"
    );
    assert!(
        trace_position(&trace, fresh.slot(), fresh.generation()).is_none(),
        "the reused generation is never opened"
    );
    assert!(
        parked.marked(tail_child),
        "the original tail and its exclusive child are still scanned"
    );

    let report = parked.complete().expect("rollback sweep");
    drop(parked);
    let fresh_alive = heap.alive(fresh);
    let probe_alive = heap.alive(probe);
    let tail_alive = heap.alive(tail);
    let tail_child_alive = heap.alive(tail_child);
    let orphan_alive = heap.alive(orphan);
    let weak_value = heap
        .named_value(weak_owner, "dead")
        .expect("weak entry after sweep");
    eprintln!(
        "ordinary_gray_middle result report_live={} fresh_alive={fresh_alive} \
         probe_alive={probe_alive} tail_alive={tail_alive} \
         tail_child_alive={tail_child_alive} orphan_alive={orphan_alive} \
         weak_value={weak_value:?} stale_generation={} mid_generation={}",
        report.live_nodes,
        fresh.generation(),
        mid.generation(),
    );

    assert!(
        tail_alive && tail_child_alive,
        "the original chain tail and its exclusive strong child stay reachable"
    );
    assert!(
        !orphan_alive && weak_value == Value::Null,
        "the independent weak orphan is really cleared"
    );
    assert!(
        !fresh_alive,
        "the white reused generation is reclaimed in this round"
    );
    assert!(
        !probe_alive,
        "a white probe reachable only through the rolled-back generation must not be \
         retained by the stale ordinary Gray item"
    );
}

/// Retiring the head and then the tail of the ordinary chain repairs the
/// neighbour links without walking it; the surviving middle item is still
/// reached and opened, and the reused slots are not inherited.
#[test]
fn ordinary_gray_head_and_tail_retirement_keeps_the_successor_reachable() {
    let mut heap = LuaHeap::new(limits(32, 8, 32));
    let tail = heap.alloc().expect("retire tail");
    let mid = heap.alloc().expect("retire mid");
    let mid_child = heap.alloc().expect("retire mid child");
    heap.set_named_edge(mid, "child", mid_child)
        .expect("mid exclusive child");
    let top = heap.alloc().expect("retire top");
    let gate = heap.alloc().expect("retire gate");
    heap.set_named_scalar(gate, "g0", Value::Bool(true))
        .expect("retire gate scalar");
    let (_, gate_record) = heap
        .locate_named(gate.slot(), "g0")
        .expect("retire gate record");
    let frame = heap.enter_roots(4).expect("retire roots");
    heap.write_root(frame, 0, tail).expect("retire root tail");
    heap.write_root(frame, 1, mid).expect("retire root mid");
    heap.write_root(frame, 2, top).expect("retire root top");
    heap.write_root(frame, 3, gate).expect("retire root gate");
    let _ = heap.take_scan_meter();

    let mut parked = ParkedMark::start(&mut heap).expect("retire mark");
    pause_at(&mut parked, gate.slot(), gate_record);
    let (mid_id, top_id, tail_id) = (
        (mid.slot(), mid.generation()),
        (top.slot(), top.generation()),
        (tail.slot(), tail.generation()),
    );
    let chain = gray_chain(&mut parked);
    assert_eq!(chain.nodes, vec![top_id, mid_id, tail_id]);

    parked
        .with_saved_heap(|heap| heap.rollback(top))
        .expect("retire the chain head");
    let after_head = gray_chain(&mut parked);
    assert_eq!(
        after_head.nodes,
        vec![mid_id, tail_id],
        "the middle item becomes the head and still reaches the tail"
    );
    assert_eq!(
        (
            after_head.access.0 - chain.access.0,
            after_head.access.1 - chain.access.1,
            after_head.access.2 - chain.access.2,
        ),
        (2, 2, 1),
        "a head release reads the node and its successor and writes two links plus the header"
    );
    assert_eq!(
        after_head.arena_capacity, chain.arena_capacity,
        "a head release does not grow the slot arena"
    );

    parked
        .with_saved_heap(|heap| heap.rollback(tail))
        .expect("retire the chain tail");
    let after_tail = gray_chain(&mut parked);
    assert_eq!(
        after_tail.nodes,
        vec![mid_id],
        "the surviving middle item becomes both head and tail"
    );
    assert_eq!(
        (
            after_tail.access.0 - after_head.access.0,
            after_tail.access.1 - after_head.access.1,
            after_tail.access.2 - after_head.access.2,
        ),
        (2, 2, 1),
        "a tail release reads the node and its predecessor and writes two links plus the header"
    );

    let fresh_top = parked
        .with_saved_heap(|heap| heap.alloc())
        .expect("reuse the retired head slot");
    let fresh_tail = parked
        .with_saved_heap(|heap| heap.alloc())
        .expect("reuse the retired tail slot");
    assert!(
        !parked.with_saved_heap(|heap| heap.table_in_gray(fresh_top))
            && !parked.with_saved_heap(|heap| heap.table_in_gray(fresh_tail)),
        "the reused slots inherit no chain position"
    );

    clear_open_trace();
    let _ = finish(&mut parked);
    let trace = open_trace();
    assert!(
        trace_position(&trace, mid.slot(), mid.generation()).is_some(),
        "the surviving successor is actually opened: {trace:?}"
    );
    assert!(
        trace_position(&trace, fresh_top.slot(), fresh_top.generation()).is_none()
            && trace_position(&trace, fresh_tail.slot(), fresh_tail.generation()).is_none(),
        "the reused slots are never opened"
    );
    assert!(
        parked.marked(mid_child),
        "the surviving middle item scans its exclusive child"
    );

    parked.complete().expect("retire sweep");
    drop(parked);
    assert!(heap.alive(mid) && heap.alive(mid_child));
    assert!(!heap.alive(top) && !heap.alive(tail));
    assert!(
        !heap.alive(fresh_top) && !heap.alive(fresh_tail),
        "the unrooted reused generations are reclaimed"
    );
}

/// The ordinary Gray pop advances the head by reading the old head and its
/// successor, clearing the popped node, and writing one predecessor link. It
/// allocates nothing and leaves the tail untouched.
#[test]
fn ordinary_gray_pop_reads_and_writes_a_local_constant() {
    let mut heap = LuaHeap::new(limits(16, 4, 16));
    let first = heap.alloc().expect("pop first");
    let second = heap.alloc().expect("pop second");
    let gate = heap.alloc().expect("pop gate");
    heap.set_named_scalar(gate, "g0", Value::Bool(true))
        .expect("pop gate scalar");
    let (_, gate_record) = heap
        .locate_named(gate.slot(), "g0")
        .expect("pop gate record");
    let frame = heap.enter_roots(3).expect("pop roots");
    heap.write_root(frame, 0, first).expect("pop root first");
    heap.write_root(frame, 1, second).expect("pop root second");
    heap.write_root(frame, 2, gate).expect("pop root gate");
    let _ = heap.take_scan_meter();

    let mut parked = ParkedMark::start(&mut heap).expect("pop mark");
    pause_at(&mut parked, gate.slot(), gate_record);
    let charging = parked.pulse(1);
    assert!(!charging.finished, "the gate scalar is charged first");
    assert!(
        parked.pending().is_none(),
        "the gate owner has no record left to charge"
    );
    let chain_before = gray_chain(&mut parked);
    assert_eq!(
        chain_before.nodes,
        vec![
            (second.slot(), second.generation()),
            (first.slot(), first.generation()),
        ],
        "the ordinary chain is head second, tail first"
    );
    let tail_before = *chain_before.nodes.last().expect("pop chain has a tail");

    let view = parked.pulse(1);
    assert!(!view.finished);
    let chain_after = gray_chain(&mut parked);
    assert_eq!(
        chain_after.nodes,
        vec![(first.slot(), first.generation())],
        "the pop advances the head to the recorded successor"
    );
    assert_eq!(
        chain_after.nodes.last(),
        Some(&tail_before),
        "the pop leaves the tail identity unchanged"
    );
    assert_eq!(
        (
            chain_after.access.0 - chain_before.access.0,
            chain_after.access.1 - chain_before.access.1,
            chain_after.access.2 - chain_before.access.2,
        ),
        (2, 2, 1),
        "one pop reads the head and its successor, writes two links, and moves the header"
    );
    assert_eq!(
        chain_after.arena_capacity, chain_before.arena_capacity,
        "one pop does not grow the slot arena"
    );
    assert!(
        !parked.with_saved_heap(|heap| heap.table_in_gray(second)),
        "the popped head is no longer chained"
    );
    assert!(
        parked.with_saved_heap(|heap| heap.table_in_gray(first)),
        "the recorded successor stays chained"
    );

    let _ = finish(&mut parked);
    parked.complete().expect("pop sweep");
    drop(parked);
    assert!(heap.alive(first) && heap.alive(second) && heap.alive(gate));
}

/// The abandoned first round holds a real four-node ordinary Gray chain under
/// stamp one. The helper then advances the heap stamp to `u64::MAX`, so the
/// next mark really takes one again. Every link was built by a real push; the
/// helper never writes a link field.
struct ReusedStampFixture {
    gate: TableRef,
    gate_record: u32,
    old_a: TableRef,
    old_b: TableRef,
    old_c: TableRef,
    old_d: TableRef,
    weak_owner: TableRef,
    orphan: TableRef,
    keep: TableRef,
    kept_child: TableRef,
}

fn gray_link_fields(heap: &LuaHeap, table: TableRef) -> (u64, u32, Option<u32>, Option<u32>) {
    let stored = &heap.slots[table.slot() as usize];
    (
        stored.gray_epoch,
        stored.gray_generation,
        stored.gray_prev,
        stored.gray_next,
    )
}

fn plant_reused_stamp_chain() -> (LuaHeap, ReusedStampFixture) {
    let mut heap = LuaHeap::new(limits(64, 16, 64));
    let gate = heap.alloc().expect("reuse gate");
    heap.set_named_scalar(gate, "g0", Value::Bool(true))
        .expect("reuse gate scalar");
    let (_, gate_record) = heap
        .locate_named(gate.slot(), "g0")
        .expect("reuse gate record");
    let old_a = heap.alloc().expect("reuse old a");
    let old_b = heap.alloc().expect("reuse old b");
    let old_c = heap.alloc().expect("reuse old c");
    let old_d = heap.alloc().expect("reuse old d");
    let weak_mt = heap.alloc().expect("reuse weak metatable");
    heap.set_mode_scalar(weak_mt, Value::string("v"))
        .expect("reuse weak mode");
    let weak_owner = heap.alloc().expect("reuse weak owner");
    heap.set_metatable(weak_owner, Some(weak_mt))
        .expect("reuse weak metatable attach");
    let orphan = heap.alloc().expect("reuse weak orphan");
    heap.set_named_edge(weak_owner, "dead", orphan)
        .expect("reuse weak orphan edge");
    let keep = heap.alloc().expect("reuse keep owner");
    let kept_child = heap.alloc().expect("reuse keep child");
    heap.set_named_edge(keep, "keep", kept_child)
        .expect("reuse keep edge");
    let frame = heap.enter_roots(8).expect("reuse roots");
    for (slot, table) in [old_a, old_b, old_c, old_d, gate].into_iter().enumerate() {
        heap.write_root(frame, slot as u32, table)
            .expect("first-round root");
    }
    let _ = heap.take_scan_meter();
    // A fresh heap starts at zero, so this round really takes stamp one.
    {
        let mut parked = ParkedMark::start(&mut heap).expect("first-round mark");
        assert_eq!(
            parked.with_saved_heap(|heap| heap.rescan_epoch),
            1,
            "the first round takes stamp one"
        );
        pause_at(&mut parked, gate.slot(), gate_record);
        let chain = gray_chain(&mut parked);
        assert_eq!(
            chain.nodes,
            vec![
                (old_d.slot(), old_d.generation()),
                (old_c.slot(), old_c.generation()),
                (old_b.slot(), old_b.generation()),
                (old_a.slot(), old_a.generation()),
            ],
            "the abandoned round really pushed four nodes and stopped before opening them"
        );
    }
    assert!(
        heap.saved_mark_query().is_none(),
        "dropping the driver abandons the first round"
    );
    for slot in 0..5 {
        heap.clear_root(frame, slot)
            .expect("clear a first-round root");
    }
    heap.write_root(frame, 0, keep).expect("reuse root keep");
    heap.write_root(frame, 1, gate).expect("reuse root gate");
    heap.write_root(frame, 2, weak_mt)
        .expect("reuse root weak metatable");
    heap.write_root(frame, 3, weak_owner)
        .expect("reuse root weak owner");
    heap.rescan_epoch = u64::MAX;
    let _ = heap.take_scan_meter();
    (
        heap,
        ReusedStampFixture {
            gate,
            gate_record,
            old_a,
            old_b,
            old_c,
            old_d,
            weak_owner,
            orphan,
            keep,
            kept_child,
        },
    )
}

/// The heap stamp is advanced to `u64::MAX`, so the next mark really reuses
/// stamp one. A rollback before Reset has visited the old slot must clear only
/// that slot: the abandoned one-stamp link is not a member of the empty
/// current chain, and the old neighbour link must not be followed. Reset then
/// clears every remaining reusable link before any push, and the different
/// owner's graph is scanned and swept from real marks.
#[test]
fn ordinary_gray_reused_stamp_rollback_before_reset_visit_clears_only_the_old_slot() {
    let (mut heap, fixture) = plant_reused_stamp_chain();
    let mut parked = ParkedMark::start(&mut heap).expect("reused-stamp mark");
    assert_eq!(
        parked.with_saved_heap(|heap| heap.rescan_epoch),
        1,
        "advancing to u64::MAX makes the next real round take one again"
    );
    assert!(
        gray_chain(&mut parked).nodes.is_empty(),
        "the abandoned round's header is gone"
    );
    let old_b_before = parked.with_saved_heap(|heap| gray_link_fields(heap, fixture.old_b));
    assert_eq!(
        old_b_before,
        (
            1,
            fixture.old_b.generation(),
            Some(fixture.old_c.slot()),
            Some(fixture.old_a.slot()),
        ),
        "the abandoned one-stamp chain is still really linked on the old slots"
    );
    let access_before =
        parked.with_saved_heap(|heap| heap.saved_mark_gray_access().expect("saved mark"));
    let view = parked.pulse(1);
    assert!(!view.finished, "Reset still has slots to visit");
    assert_eq!(
        parked.phase_name(),
        "reset",
        "the pulse visited slot zero only"
    );

    parked
        .with_saved_heap(|heap| heap.rollback(fixture.old_a))
        .expect("real rollback before Reset reaches the old slot");
    assert!(
        !parked.with_saved_heap(|heap| heap.alive(fixture.old_a)),
        "the rollback really released the old tail"
    );
    assert!(
        gray_chain(&mut parked).nodes.is_empty(),
        "a reused stamp on an empty current chain must not join or unlink anything"
    );
    let old_b_after = parked.with_saved_heap(|heap| gray_link_fields(heap, fixture.old_b));
    assert_eq!(
        old_b_after, old_b_before,
        "the empty-chain release clears only the released slot; it never follows the old neighbour link"
    );
    let access_after =
        parked.with_saved_heap(|heap| heap.saved_mark_gray_access().expect("saved mark"));
    assert_eq!(
        (
            access_after.0 - access_before.0,
            access_after.1 - access_before.1,
            access_after.2 - access_before.2,
        ),
        (1, 0, 0),
        "the empty-chain release reads one old tag, writes no counted link, and moves no header"
    );
    assert_eq!(
        parked.with_saved_heap(|heap| gray_link_fields(heap, fixture.old_a)),
        (0, 0, None, None),
        "the released old slot itself is cleared"
    );

    clear_open_trace();
    pause_at(&mut parked, fixture.gate.slot(), fixture.gate_record);
    for old in [fixture.old_b, fixture.old_c, fixture.old_d] {
        assert_eq!(
            parked.with_saved_heap(|heap| gray_link_fields(heap, old)),
            (0, 0, None, None),
            "every slot Reset visited no longer carries the abandoned stamp or its neighbours"
        );
    }
    assert_eq!(
        gray_chain(&mut parked).nodes,
        vec![(fixture.keep.slot(), fixture.keep.generation())],
        "the current chain names only the different owner"
    );
    let opened = open_trace();
    for old in [fixture.old_a, fixture.old_b, fixture.old_c, fixture.old_d] {
        assert!(
            trace_position(&opened, old.slot(), old.generation()).is_none(),
            "no old chain member opens in the reused-stamp round: {opened:?}"
        );
    }

    let _ = finish(&mut parked);
    let opened = open_trace();
    let keep_pos = trace_position(&opened, fixture.keep.slot(), fixture.keep.generation())
        .unwrap_or_else(|| panic!("the different owner never opened: {opened:?}"));
    assert!(
        opened[keep_pos].live && opened[keep_pos].marked,
        "the different owner opens after the cleared Reset"
    );
    assert!(
        parked.marked(fixture.kept_child),
        "the different owner scans its exclusive strong child"
    );
    parked.complete().expect("reused-stamp sweep");
    drop(parked);
    assert!(
        heap.alive(fixture.keep) && heap.alive(fixture.kept_child),
        "the different owner and its exclusive strong child survive the real scan"
    );
    assert!(
        !heap.alive(fixture.old_a) && !heap.alive(fixture.old_d),
        "the released old items stay released"
    );
    assert!(
        !heap.alive(fixture.old_b) && !heap.alive(fixture.old_c),
        "the unrooted old chain members are collected, never retained by the abandoned links"
    );
    assert!(
        !heap.alive(fixture.orphan),
        "the independent weak orphan is cleared"
    );
    assert_eq!(
        heap.named_value(fixture.weak_owner, "dead"),
        Ok(Value::Null),
        "the weak edge is cleared in the same round"
    );
}

/// Same reused stamp, but the stale rollback happens while the current round's
/// Gray queue holds a different owner. The stale head must not clear or
/// replace that owner: its exclusive strong child has to be reached by the
/// real scan. The old generation is then legitimately offered again, so the
/// same stamp must not be mistaken for a duplicate membership.
#[test]
fn ordinary_gray_reused_stamp_rollback_under_a_different_owner_keeps_it_scanned() {
    let (mut heap, fixture) = plant_reused_stamp_chain();
    let mut parked = ParkedMark::start(&mut heap).expect("reused-stamp Gray mark");
    assert_eq!(
        parked.with_saved_heap(|heap| heap.rescan_epoch),
        1,
        "the wrapping stamp is really reused"
    );
    clear_open_trace();
    pause_at(&mut parked, fixture.gate.slot(), fixture.gate_record);
    let before = gray_chain(&mut parked);
    assert_eq!(
        before.nodes,
        vec![(fixture.keep.slot(), fixture.keep.generation())],
        "the current chain holds the different owner before the stale release"
    );
    let old_d_at_pause = parked.with_saved_heap(|heap| gray_link_fields(heap, fixture.old_d));
    assert!(
        parked.with_saved_heap(|heap| heap.table_in_gray(fixture.keep)),
        "the different owner is the current chain"
    );
    let access_before =
        parked.with_saved_heap(|heap| heap.saved_mark_gray_access().expect("saved mark"));

    parked
        .with_saved_heap(|heap| heap.rollback(fixture.old_d))
        .expect("real rollback of the reused old head during Gray");
    assert!(
        !parked.with_saved_heap(|heap| heap.alive(fixture.old_d)),
        "the rollback really released the old head"
    );
    let old_d_after = parked.with_saved_heap(|heap| gray_link_fields(heap, fixture.old_d));
    let keep_linked_after = parked.with_saved_heap(|heap| heap.table_in_gray(fixture.keep));
    let access_after =
        parked.with_saved_heap(|heap| heap.saved_mark_gray_access().expect("saved mark"));
    let release_access = (
        access_after.0 - access_before.0,
        access_after.1 - access_before.1,
        access_after.2 - access_before.2,
    );
    assert_eq!(
        old_d_after,
        (0, 0, None, None),
        "only the released old slot is cleared"
    );

    parked
        .accept_root_batch(&[super::Rooted::Table(fixture.old_b)])
        .expect("the legitimate re-offer of an old generation");
    let old_b_marked_now = parked.marked(fixture.old_b);

    let _ = finish(&mut parked);
    let opened = open_trace();
    let keep_pos = trace_position(&opened, fixture.keep.slot(), fixture.keep.generation());
    let old_b_pos = trace_position(&opened, fixture.old_b.slot(), fixture.old_b.generation());
    let old_c_pos = trace_position(&opened, fixture.old_c.slot(), fixture.old_c.generation());
    parked.complete().expect("reused-stamp Gray sweep");
    drop(parked);
    let kept_child_alive = heap.alive(fixture.kept_child);
    let keep_alive = heap.alive(fixture.keep);
    let old_b_alive = heap.alive(fixture.old_b);
    let old_c_alive = heap.alive(fixture.old_c);
    let orphan_alive = heap.alive(fixture.orphan);
    let weak_value = heap
        .named_value(fixture.weak_owner, "dead")
        .expect("weak entry after sweep");
    eprintln!(
        "ordinary_gray_reused_stamp result keep_linked_after={keep_linked_after} \
         release_access={release_access:?} kept_child_alive={kept_child_alive} \
         keep_alive={keep_alive} old_b_alive={old_b_alive} old_c_alive={old_c_alive} \
         orphan_alive={orphan_alive} weak_value={weak_value:?}"
    );

    assert!(
        kept_child_alive,
        "the different owner's exclusive strong child is really scanned and survives the reused-stamp release"
    );
    assert!(keep_alive, "the different owner stays marked and live");
    assert!(
        keep_linked_after,
        "rolling back the reused old head must not clear the current owner from the chain"
    );
    let keep_pos =
        keep_pos.unwrap_or_else(|| panic!("the different owner never opened: {opened:?}"));
    assert!(
        opened[keep_pos].live && opened[keep_pos].marked,
        "the different owner really opens and scans"
    );
    assert!(
        old_b_marked_now && old_b_pos.is_some_and(|at| opened[at].live && opened[at].marked),
        "the legitimate old-generation re-offer opens under the current round"
    );
    assert!(
        old_c_pos.is_none(),
        "the unrooted old member is never opened through an abandoned link: {opened:?}"
    );
    assert!(!old_c_alive, "the unrooted old member is collected");
    assert!(
        !orphan_alive && weak_value == Value::Null,
        "the independent weak orphan is cleared"
    );
    assert_eq!(
        release_access,
        (1, 0, 0),
        "the stale release reads one tag and moves no link or header"
    );
    assert_eq!(
        old_d_at_pause,
        (0, 0, None, None),
        "Reset had already cleared the abandoned stamp before this Gray rollback"
    );
}

/// An abandoned mark drops its head with the saved state. A new round starts
/// empty, never picks up an old chain item, and the old unrooted roots die.
#[test]
fn abandoned_ordinary_gray_links_do_not_cross_into_the_next_round() {
    let mut heap = LuaHeap::new(limits(32, 8, 32));
    let first = heap.alloc().expect("abandon first");
    let second = heap.alloc().expect("abandon second");
    let third = heap.alloc().expect("abandon third");
    let gate = heap.alloc().expect("abandon gate");
    heap.set_named_scalar(gate, "g0", Value::Bool(true))
        .expect("abandon gate scalar");
    let (_, gate_record) = heap
        .locate_named(gate.slot(), "g0")
        .expect("abandon gate record");
    let frame = heap.enter_roots(4).expect("abandon roots");
    heap.write_root(frame, 0, first)
        .expect("abandon root first");
    heap.write_root(frame, 1, second)
        .expect("abandon root second");
    heap.write_root(frame, 2, third)
        .expect("abandon root third");
    heap.write_root(frame, 3, gate).expect("abandon root gate");
    let _ = heap.take_scan_meter();

    {
        let mut parked = ParkedMark::start(&mut heap).expect("abandon mark");
        pause_at(&mut parked, gate.slot(), gate_record);
        let chain = gray_chain(&mut parked);
        assert_eq!(
            chain.nodes,
            vec![
                (third.slot(), third.generation()),
                (second.slot(), second.generation()),
                (first.slot(), first.generation()),
            ],
            "the first round holds third, second, and first in LIFO order"
        );
    }
    assert!(
        heap.saved_mark_query().is_none(),
        "dropping the driver abandons the saved mark"
    );

    for slot in 0..3 {
        heap.clear_root(frame, slot).expect("clear an old root");
    }

    let mut parked = ParkedMark::start(&mut heap).expect("next round mark");
    clear_open_trace();
    assert!(
        gray_chain(&mut parked).nodes.is_empty(),
        "a new round starts with an empty ordinary chain"
    );
    for _ in 0..HARNESS_BOUND {
        let view = parked.pulse(1);
        assert_eq!(view.meter.sweep_edge_checks, 0, "a pulse must not sweep");
        assert_eq!(
            view.meter.mark_passes, 2,
            "the heap is in its second real mark pass"
        );
        if view.finished {
            break;
        }
    }
    let trace = open_trace();
    assert!(
        trace_position(&trace, gate.slot(), gate.generation()).is_some(),
        "the only remaining root opens in the new round: {trace:?}"
    );
    for old in [first, second, third] {
        assert!(
            trace_position(&trace, old.slot(), old.generation()).is_none(),
            "an old chain item is not opened in the next round"
        );
    }

    parked.complete().expect("next round sweep");
    drop(parked);
    assert!(heap.alive(gate));
    assert!(
        !heap.alive(first) && !heap.alive(second) && !heap.alive(third),
        "the old unrooted items are collected in the next round"
    );
}
