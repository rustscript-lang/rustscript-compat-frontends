//! Cross-window park and resume for one private mark.
//!
//! `park` consumes the exclusive driver. Drop and cancel still discard.
//! A full collect drops saved progress and marks again from the current roots.

use super::{
    HeapError, HeapLimits, LuaHeap, ParkedMark, PendingScan, RootFrameId, ScanMeter, TableRef,
};
use vm::Value;

const HARNESS_BOUND: u32 = 64;
const KEY_X: u32 = 1;
const KEY_Y: u32 = 5;
const KEY_Z: u32 = 3;
const WEAK_KEY: u32 = 1;

fn limits(nodes: u32, roots: u32, edges: u32) -> HeapLimits {
    HeapLimits {
        nodes,
        roots,
        edges,
        max_generation: 8,
    }
}

fn pause_at(parked: &mut ParkedMark<'_>, owner: u32, cursor: u32) -> PendingScan {
    for _ in 0..HARNESS_BOUND {
        let view = parked.pulse(1);
        assert!(!view.finished, "mark finished before the pending record");
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
            return pending;
        }
    }
    panic!("pending record {cursor} on owner {owner} was not observed");
}

fn pulse_until_marked(parked: &mut ParkedMark<'_>, target: TableRef) {
    for _ in 0..HARNESS_BOUND {
        if parked.marked(target) {
            assert_eq!(parked.meter().mark_passes, 1);
            assert_eq!(parked.meter().sweep_edge_checks, 0);
            return;
        }
        let view = parked.pulse(1);
        assert!(!view.finished, "mark finished before the target was marked");
        assert_eq!(view.meter.sweep_edge_checks, 0);
    }
    panic!("target was not marked");
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

struct Chain {
    anchor: TableRef,
    reached: TableRef,
    pending_target: TableRef,
    ahead: TableRef,
    extra: TableRef,
    y_index: u32,
}

fn chain(heap: &mut LuaHeap) -> Chain {
    let anchor = heap.alloc().expect("anchor");
    let reached = heap.alloc().expect("reached");
    let pending_target = heap.alloc().expect("pending");
    let ahead = heap.alloc().expect("ahead");
    let extra = heap.alloc().expect("extra");
    let frame = heap.enter_roots(1).expect("frame");
    heap.write_root(frame, 0, anchor).expect("root");
    heap.set_edge(anchor, KEY_X, reached).expect("reached edge");
    heap.set_edge(anchor, KEY_Y, pending_target)
        .expect("pending edge");
    heap.set_edge(anchor, KEY_Z, ahead).expect("ahead edge");
    let y_index = heap
        .find_key(anchor.slot(), &super::EdgeKey::Field(KEY_Y))
        .expect("pending record");
    Chain {
        anchor,
        reached,
        pending_target,
        ahead,
        extra,
        y_index,
    }
}

fn pause_chain(heap: &mut LuaHeap) -> (ParkedMark<'_>, Chain) {
    let chain = chain(heap);
    let mut parked = ParkedMark::start(heap).expect("start");
    let pending = pause_at(&mut parked, chain.anchor.slot(), chain.y_index);
    assert_eq!(pending.cursor, chain.y_index);
    assert!(
        parked.marked(chain.reached),
        "the edge before the cursor is marked"
    );
    assert!(
        !parked.marked(chain.pending_target),
        "the pending edge is not marked yet"
    );
    assert!(
        !parked.marked(chain.ahead),
        "the edge after the cursor is not marked"
    );
    assert!(
        !parked.marked(chain.extra),
        "an unrooted node is not marked"
    );
    (parked, chain)
}

#[test]
fn second_start_is_refused_while_a_mark_is_parked() {
    let mut heap = LuaHeap::new(limits(8, 2, 8));
    let (parked, chain) = pause_chain(&mut heap);
    let expected = (chain.anchor.slot(), chain.y_index);
    parked.park().expect("park incomplete");

    assert_eq!(
        ParkedMark::start(&mut heap).err(),
        Some(HeapError::MarkActive),
        "a second start must be refused while parked progress remains"
    );
    let resumed = ParkedMark::resume(&mut heap)
        .expect("the refused start must leave the parked mark in place");
    assert_eq!(
        resumed.pending().map(|scan| (scan.owner, scan.cursor)),
        Some(expected),
        "resume must see the same pending cursor"
    );
}

#[test]
fn resume_keeps_the_pending_cursor_and_does_not_remark() {
    let mut heap = LuaHeap::new(limits(8, 2, 8));
    let (parked, chain) = pause_chain(&mut heap);
    let expected = (chain.anchor.slot(), chain.y_index);
    let before = parked.meter();
    assert_eq!(before.mark_passes, 1);
    assert_eq!(parked.phase_name(), "gray");
    parked.park().expect("park incomplete");

    let mut resumed = ParkedMark::resume(&mut heap).expect("resume the parked mark");
    assert_eq!(
        resumed.pending().map(|scan| (scan.owner, scan.cursor)),
        Some(expected),
        "resume must find the same pending cursor"
    );
    assert_eq!(resumed.phase_name(), "gray");
    assert_eq!(
        resumed.meter().mark_passes,
        1,
        "resume must not start another pass"
    );
    assert!(resumed.marked(chain.reached));
    assert!(!resumed.marked(chain.pending_target));
    assert!(!resumed.marked(chain.ahead));

    let held = resumed.meter();
    let view = resumed.pulse(0);
    assert!(!view.finished, "a zero budget must not finish the mark");
    assert_eq!(
        resumed.meter(),
        held,
        "a zero budget must not read the graph"
    );
    assert_eq!(
        resumed.pending().map(|scan| (scan.owner, scan.cursor)),
        Some(expected),
        "a zero budget must not move the cursor"
    );

    let _meter = finish(&mut resumed);
    let report = resumed.complete().expect("sweep the finished mark");
    let swept = resumed.meter().sweep_edge_checks;
    assert!(swept > 0, "the completion window sweeps");
    assert!(
        resumed.park().is_err(),
        "an already swept mark must not park"
    );
    assert!(ParkedMark::resume(&mut heap).is_none());
    assert!(heap.alive(chain.anchor));
    assert!(heap.alive(chain.reached));
    assert!(heap.alive(chain.pending_target));
    assert!(heap.alive(chain.ahead));
    assert!(
        !heap.alive(chain.extra),
        "sweep must collect the unrooted node"
    );
    assert_eq!(report.live_nodes, 4, "anchor and its three fields remain");
    assert_eq!(
        heap.stats().collections,
        1,
        "the completion window sweeps once"
    );
    assert_eq!(
        heap.take_scan_meter().sweep_edge_checks,
        swept,
        "refusing to park a swept mark must not sweep again"
    );
}

#[test]
fn drop_cancel_and_completed_park_do_not_resume() {
    let mut dropped = LuaHeap::new(limits(8, 2, 8));
    {
        let (parked, _) = pause_chain(&mut dropped);
        drop(parked);
    }
    assert!(ParkedMark::resume(&mut dropped).is_none());
    assert!(
        ParkedMark::start(&mut dropped).is_ok(),
        "drop must not leave a mark"
    );

    let mut cancelled = LuaHeap::new(limits(8, 2, 8));
    let (parked, _) = pause_chain(&mut cancelled);
    parked.cancel();
    assert!(ParkedMark::resume(&mut cancelled).is_none());
    assert_eq!(cancelled.stats().collections, 0);

    let mut finished = LuaHeap::new(limits(8, 2, 8));
    let (mut parked, chain) = pause_chain(&mut finished);
    let _meter = finish(&mut parked);
    assert_eq!(parked.phase_name(), "done");
    assert_eq!(parked.meter().sweep_edge_checks, 0);
    assert!(
        parked.park().is_err(),
        "a finished mark must not park, and refusal must not be a collect report"
    );
    assert!(ParkedMark::resume(&mut finished).is_none());
    assert!(finished.alive(chain.extra), "refusing Done must not sweep");
    assert_eq!(finished.stats().collections, 0);
    assert_eq!(finished.take_scan_meter().sweep_edge_checks, 0);
    assert!(
        ParkedMark::start(&mut finished).is_ok(),
        "Done must not escape as a saved mark"
    );
}

struct Observed {
    frame: RootFrameId,
    keeper: TableRef,
    watch: TableRef,
    metatable: TableRef,
    target: TableRef,
}

fn plant(heap: &mut LuaHeap, root_target: bool) -> Observed {
    let keeper = heap.alloc().expect("keeper");
    let target = heap.alloc().expect("target");
    let metatable = heap.alloc().expect("metatable");
    let watch = heap.alloc().expect("watch");
    heap.set_mode_scalar(metatable, Value::string("v"))
        .expect("weak values");
    heap.set_metatable(watch, Some(metatable))
        .expect("metatable");
    heap.set_edge(watch, WEAK_KEY, target).expect("weak target");
    let frame = heap.enter_roots(3).expect("frame");
    heap.write_root(frame, 0, keeper).expect("root keeper");
    heap.write_root(frame, 1, watch).expect("root watch");
    if root_target {
        heap.write_root(frame, 2, target).expect("root target");
    }
    Observed {
        frame,
        keeper,
        watch,
        metatable,
        target,
    }
}

fn assert_current_roots(heap: &LuaHeap, observed: &Observed, target_alive: bool) {
    assert!(heap.alive(observed.keeper));
    assert!(heap.alive(observed.watch));
    assert!(heap.alive(observed.metatable));
    assert_eq!(heap.alive(observed.target), target_alive);
    assert_eq!(
        heap.edge(observed.watch, WEAK_KEY)
            .expect("weak edge")
            .is_some(),
        target_alive,
        "the weak observer must not keep a target the current roots did not mark"
    );
}

#[test]
fn full_collect_discards_parked_marks_and_matches_current_roots() {
    let mut heap = LuaHeap::new(limits(8, 4, 8));
    let observed = plant(&mut heap, true);
    let mut parked = ParkedMark::start(&mut heap).expect("start");
    pulse_until_marked(&mut parked, observed.target);
    parked.park().expect("park incomplete");
    heap.clear_root(observed.frame, 2)
        .expect("drop the last strong root");
    let parked_work = heap.take_scan_meter();
    assert_eq!(
        parked_work.mark_passes, 1,
        "parked work is a separate meter read"
    );
    assert_eq!(parked_work.sweep_edge_checks, 0, "park did not sweep");

    let report = heap.collect();
    assert_current_roots(&heap, &observed, false);
    assert!(ParkedMark::resume(&mut heap).is_none());
    assert_eq!(heap.stats().collections, 1);
    let fresh = heap.take_scan_meter();
    assert_eq!(fresh.mark_passes, 1, "collect starts one new pass");
    assert!(fresh.sweep_edge_checks > 0, "collect sweeps");

    let mut twin = LuaHeap::new(limits(8, 4, 8));
    let twin_observed = plant(&mut twin, false);
    let twin_report = twin.collect();
    assert_current_roots(&twin, &twin_observed, false);
    assert_eq!(twin_report.live_nodes, report.live_nodes);
    assert_eq!(twin_report.marked_nodes, report.marked_nodes);
    assert_eq!(
        twin.take_scan_meter(),
        fresh,
        "re-mark matches a heap that never parked"
    );

    let mut contrast = LuaHeap::new(limits(8, 4, 8));
    let contrast_observed = plant(&mut contrast, true);
    let mut contrast_parked = ParkedMark::start(&mut contrast).expect("start");
    pulse_until_marked(&mut contrast_parked, contrast_observed.target);
    contrast_parked.park().expect("park incomplete");
    contrast
        .clear_root(contrast_observed.frame, 2)
        .expect("drop the last strong root");
    let mut resumed = ParkedMark::resume(&mut contrast).expect("resume keeps the old mark");
    let _meter = finish(&mut resumed);
    let contrast_report = resumed.complete().expect("sweep the resumed mark");
    drop(resumed);
    assert!(
        contrast.alive(contrast_observed.target),
        "resume must not clear a mark that already reached the target"
    );
    assert_eq!(
        contrast_report.live_nodes, 4,
        "keeper, watch, metatable, and target"
    );
    assert_eq!(contrast.stats().collections, 1);
}

#[test]
fn host_collect_after_park_sweeps_from_current_roots() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        crate::heap::install(&mut host, crate::sweep_limits()).expect("install");
        let keeper_raw = crate::publish_raw(&mut host);
        let keeper = crate::table_ref(&host, keeper_raw);
        crate::root_table(&mut host, keeper);
        let target_raw = crate::publish_raw(&mut host);
        let target = crate::table_ref(&host, target_raw);
        let frame = crate::heap::with_heap(&mut host, |heap| heap.enter_roots(1)).expect("frame");
        crate::heap::with_heap(&mut host, |heap| heap.write_root(frame, 0, target)).expect("root");
        crate::heap::with_heap(&mut host, |heap| {
            let ahead = heap.alloc()?;
            heap.set_edge(target, KEY_Y, ahead)?;
            Ok(())
        })
        .expect("outgoing edge");
        let watch = crate::heap::with_heap(&mut host, |heap| {
            let metatable = heap.alloc()?;
            let watch = heap.alloc()?;
            heap.set_mode_scalar(metatable, Value::string("v"))?;
            heap.set_metatable(watch, Some(metatable))?;
            heap.set_edge(watch, WEAK_KEY, target)?;
            let watch_frame = heap.enter_roots(1)?;
            heap.write_root(watch_frame, 0, watch)?;
            Ok(watch)
        })
        .expect("weak watch");

        let saved = crate::heap::with_heap(&mut host, |heap| {
            let mut parked = ParkedMark::start(heap)?;
            let cursor = parked.field_index(target, KEY_Y).expect("outgoing record");
            let pending = pause_at(&mut parked, target.slot(), cursor);
            assert!(
                parked.marked(target),
                "the scanning owner is already marked"
            );
            let saved = (pending.owner, pending.cursor);
            parked.park()?;
            Ok(saved)
        })
        .expect("park from the host window");

        let refused =
            crate::heap::with_heap(&mut host, |heap| Ok(ParkedMark::start(heap).is_err()))
                .expect("start probe");
        assert!(
            refused,
            "a second start must be refused while the host window is parked"
        );
        let same = crate::heap::with_heap(&mut host, |heap| {
            let Some(parked) = ParkedMark::resume(heap) else {
                return Ok(false);
            };
            let pending = parked.pending().map(|scan| (scan.owner, scan.cursor));
            let same =
                pending == Some(saved) && parked.marked(target) && parked.meter().mark_passes == 1;
            parked.park()?;
            Ok(same)
        })
        .expect("resume probe");
        assert!(same, "resume must return the parked cursor as a scalar");

        crate::heap::with_heap(&mut host, |heap| heap.clear_root(frame, 0)).expect("unroot");
        let parked_work = crate::heap::take_scan_meter(&mut host).expect("parked meter");
        assert_eq!(parked_work.mark_passes, 1);
        assert_eq!(parked_work.sweep_edge_checks, 0);

        let report = crate::heap::collect(&mut host).expect("host collect");
        let collected = crate::heap::take_scan_meter(&mut host).expect("collect meter");
        assert_eq!(collected.mark_passes, 1, "host collect starts a new pass");
        assert!(collected.sweep_edge_checks > 0);
        assert_eq!(crate::heap::stats(&mut host).expect("stats").collections, 1);
        assert!(crate::heap::with_heap(&mut host, |heap| Ok(heap.alive(keeper))).expect("keeper"));
        assert!(crate::heap::with_heap(&mut host, |heap| Ok(heap.alive(watch))).expect("watch"));
        assert!(!crate::heap::with_heap(&mut host, |heap| Ok(heap.alive(target))).expect("target"));
        assert!(
            host.typed_resource_with_key::<crate::heap::LuaTableResource>(
                crate::resource_handle(keeper_raw),
                &crate::heap::LuaTableResource::key(),
            )
            .is_ok(),
            "the rooted token stays open"
        );
        assert!(
            host.typed_resource_with_key::<crate::heap::LuaTableResource>(
                crate::resource_handle(target_raw),
                &crate::heap::LuaTableResource::key(),
            )
            .is_err(),
            "host collect closes the unrooted token"
        );
        let resumable =
            crate::heap::with_heap(&mut host, |heap| Ok(ParkedMark::resume(heap).is_some()))
                .expect("resume after collect");
        assert!(
            !resumable,
            "a finished host collect must not leave a parked mark"
        );
        assert!(report.live_nodes >= 2, "keeper and the weak watch stay");
    });
}

#[test]
fn host_close_failure_drops_parked_progress_and_retries_from_roots() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        crate::heap::install(&mut host, crate::sweep_limits()).expect("install");
        let unrelated = host
            .push_resource(crate::UnrelatedHostResource)
            .expect("unrelated");
        let unrelated_raw = unrelated.handle().raw();
        let keeper_raw = crate::publish_raw(&mut host);
        let keeper = crate::table_ref(&host, keeper_raw);
        crate::root_table(&mut host, keeper);
        let (doomed, frame) = crate::heap::with_heap(&mut host, |heap| {
            let node = heap.alloc()?;
            heap.bind_token(node, unrelated_raw)?;
            let ahead = heap.alloc()?;
            heap.set_edge(node, KEY_Y, ahead)?;
            let frame = heap.enter_roots(1)?;
            heap.write_root(frame, 0, node)?;
            Ok((node, frame))
        })
        .expect("bind a foreign token");

        let saved = crate::heap::with_heap(&mut host, |heap| {
            let mut parked = ParkedMark::start(heap)?;
            let cursor = parked.field_index(doomed, KEY_Y).expect("outgoing record");
            let pending = pause_at(&mut parked, doomed.slot(), cursor);
            assert!(
                parked.marked(doomed),
                "the scanning owner is already marked"
            );
            let saved = (pending.owner, pending.cursor);
            parked.park()?;
            Ok(saved)
        })
        .expect("park before the failing collect");
        let same = crate::heap::with_heap(&mut host, |heap| {
            let Some(parked) = ParkedMark::resume(heap) else {
                return Ok(false);
            };
            let pending = parked.pending().map(|scan| (scan.owner, scan.cursor));
            let same = pending == Some(saved) && parked.marked(doomed);
            parked.park()?;
            Ok(same)
        })
        .expect("resume probe");
        assert!(same, "the parked cursor must still be there before collect");

        crate::heap::with_heap(&mut host, |heap| heap.clear_root(frame, 0)).expect("unroot");
        let before_nodes = crate::heap::stats(&mut host).expect("before").live_nodes;
        let parked_work = crate::heap::take_scan_meter(&mut host).expect("parked meter");
        assert_eq!(parked_work.sweep_edge_checks, 0);

        let error = crate::heap::collect(&mut host).expect_err("wrong resource type");
        crate::assert_type_mismatch(&error);
        let failed = crate::heap::take_scan_meter(&mut host).expect("failed meter");
        assert_eq!(failed.sweep_edge_checks, 0, "close failure must not sweep");
        assert_eq!(failed.mark_passes, 1, "the failed collect marked again");
        assert_eq!(
            crate::heap::stats(&mut host)
                .expect("collections")
                .collections,
            0
        );
        assert_eq!(
            crate::heap::stats(&mut host).expect("nodes").live_nodes,
            before_nodes
        );
        assert!(crate::heap::with_heap(&mut host, |heap| Ok(heap.alive(keeper))).expect("keeper"));
        assert!(crate::heap::with_heap(&mut host, |heap| Ok(heap.alive(doomed))).expect("doomed"));
        assert!(
            host.resource(&unrelated).is_ok(),
            "the foreign resource stays open"
        );
        let resumable =
            crate::heap::with_heap(&mut host, |heap| Ok(ParkedMark::resume(heap).is_some()))
                .expect("resume after failure");
        assert!(!resumable, "close failure must not restore the parked mark");

        crate::heap::with_heap(&mut host, |heap| heap.unbind_token(doomed))
            .expect("fixture removes the foreign binding");
        let report = crate::heap::collect(&mut host).expect("retry after the obstacle is gone");
        assert_eq!(crate::heap::stats(&mut host).expect("retry").collections, 1);
        assert!(crate::heap::with_heap(&mut host, |heap| Ok(heap.alive(keeper))).expect("keeper"));
        assert!(!crate::heap::with_heap(&mut host, |heap| Ok(heap.alive(doomed))).expect("swept"));
        assert!(
            host.resource(&unrelated).is_ok(),
            "the foreign resource is not a lua token"
        );
        assert!(
            host.typed_resource_with_key::<crate::heap::LuaTableResource>(
                crate::resource_handle(keeper_raw),
                &crate::heap::LuaTableResource::key(),
            )
            .is_ok()
        );
        assert_eq!(report.live_nodes, 1, "only the rooted keeper remains");
    });
}
