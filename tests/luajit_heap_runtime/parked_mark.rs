//! Parked mark on a private heap. The driver keeps the only `&mut LuaHeap`.
//!
//! A pulse of one stops on a real pending record, not on a guessed step count.
//! The bound below is only a harness limit. Sweep stays outside the pulse.

use super::{
    HeapError, HeapLimits, HeapStats, LuaHeap, ParkedMark, PausedOp, PendingScan, RootFrameId,
    ScanMeter, TableRef, UpvalueRef,
};
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
