//! Parked mark on a private heap. The driver keeps the only `&mut LuaHeap`.
//!
//! A pulse of one stops on a real pending record, not on a guessed step count.
//! The bound below is only a harness limit. Sweep stays outside the pulse.

use super::{
    ClosureRef, HeapError, HeapLimits, HeapStats, LuaHeap, ParkedMark, PausedOp, PendingScan,
    RootFrameId, ScanMeter, TableRef, UpvalueRef,
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
