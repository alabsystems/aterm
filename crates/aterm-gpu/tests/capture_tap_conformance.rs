// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for the presented-destination capture lifecycles.
//!
//! The tests drive the pure transition gates used by the shipping GPU taps,
//! project each decision onto the corresponding Rust-derived model, and require
//! its exact deterministic successor. Explicit old-behavior mutants prove that
//! failed maps cannot publish pixels/leak slots and accepted invalid metadata
//! cannot disappear without incrementing `dropped`.

use std::collections::{BTreeMap, VecDeque};

use aterm_gpu::video_tap::{
    CapturedFrame, PresentedFrameDecision, PresentedFrameEvent, PresentedFrameOutcome,
    PresentedFramePhase, VideoPresentDecision, VideoSlotDecision, VideoSlotEvent, VideoSlotPhase,
    ordered_capture_store_push, presented_frame_transition, video_present_decision,
    video_slot_transition,
};
use aterm_spec::derive::{Model, presented_frame_tap_model, video_tap_slot_model};

type State = BTreeMap<&'static str, i64>;

fn assert_exact_step(model: &Model, prev: &State, post: &State, action: &str) {
    assert!(
        model.action_enabled(action, prev),
        "{}.{action} disabled at {prev:?}",
        model.name
    );
    let successors = model.successors(action, prev);
    assert_eq!(
        successors.len(),
        1,
        "{}.{action} must be deterministic",
        model.name
    );
    assert_eq!(
        post, &successors[0],
        "shipping projection diverged from {}.{action}",
        model.name
    );
}

fn presented_phase(raw: i64) -> PresentedFramePhase {
    match raw {
        0 => PresentedFramePhase::Armed,
        1 => PresentedFramePhase::Pending,
        2 => PresentedFramePhase::InFlight,
        3 => PresentedFramePhase::Complete,
        _ => panic!("invalid PresentedFrameTap phase {raw}"),
    }
}

fn presented_phase_code(phase: PresentedFramePhase) -> i64 {
    match phase {
        PresentedFramePhase::Armed => 0,
        PresentedFramePhase::Pending => 1,
        PresentedFramePhase::InFlight => 2,
        PresentedFramePhase::Complete => 3,
    }
}

fn presented_outcome_code(outcome: PresentedFrameOutcome) -> i64 {
    match outcome {
        PresentedFrameOutcome::None => 0,
        PresentedFrameOutcome::Frame => 1,
        PresentedFrameOutcome::Error => 2,
    }
}

/// Project one gate decision. `phase` and `result` are the DECISION's (a
/// non-terminal decision carries no outcome, and no result exists before the
/// terminal); the event supplies only what the gate does not decide — whether
/// a copy was encoded (`accepted`) and whether the map succeeded (`mapped`).
fn project_presented(
    prev: &State,
    event: PresentedFrameEvent,
    decision: PresentedFrameDecision,
) -> State {
    let mut post = prev.clone();
    post.insert("phase", presented_phase_code(decision.phase));
    match event {
        PresentedFrameEvent::EnqueueValid => {
            post.insert("accepted", 1);
            post.insert("mapped", 0);
        }
        PresentedFrameEvent::RejectEnqueue => {
            post.insert("accepted", 0);
            post.insert("mapped", 0);
        }
        PresentedFrameEvent::StartMap => {}
        PresentedFrameEvent::CompleteMap => {
            post.insert("mapped", 1);
        }
        PresentedFrameEvent::MapError => {
            post.insert("mapped", 0);
        }
    }
    post.insert("result", presented_outcome_code(decision.outcome));
    post
}

fn presented_post(prev: &State, event: PresentedFrameEvent) -> (PresentedFrameDecision, State) {
    let decision = presented_frame_transition(presented_phase(prev["phase"]), event)
        .unwrap_or_else(|| panic!("shipping gate rejected enabled event {event:?}"));
    (decision, project_presented(prev, event, decision))
}

#[test]
fn presented_frame_shipping_gate_conforms_on_success_and_every_error_edge() {
    let model = presented_frame_tap_model();

    let initial = model.init_state();
    let (enqueued, pending) = presented_post(&initial, PresentedFrameEvent::EnqueueValid);
    assert_eq!(
        enqueued,
        PresentedFrameDecision {
            phase: PresentedFramePhase::Pending,
            outcome: PresentedFrameOutcome::None,
        }
    );
    assert_exact_step(&model, &initial, &pending, "EnqueueValid");

    let (started, in_flight) = presented_post(&pending, PresentedFrameEvent::StartMap);
    assert_eq!(started.phase, PresentedFramePhase::InFlight);
    assert_eq!(started.outcome, PresentedFrameOutcome::None);
    assert_exact_step(&model, &pending, &in_flight, "StartMap");

    let (completed, frame) = presented_post(&in_flight, PresentedFrameEvent::CompleteMap);
    assert_eq!(completed.phase, PresentedFramePhase::Complete);
    assert_eq!(completed.outcome, PresentedFrameOutcome::Frame);
    assert_exact_step(&model, &in_flight, &frame, "CompleteMap");

    let (rejected, validation_error) = presented_post(&initial, PresentedFrameEvent::RejectEnqueue);
    assert_eq!(rejected.phase, PresentedFramePhase::Complete);
    assert_eq!(rejected.outcome, PresentedFrameOutcome::Error);
    assert_exact_step(&model, &initial, &validation_error, "RejectEnqueue");

    let (failed, map_error) = presented_post(&in_flight, PresentedFrameEvent::MapError);
    assert_eq!(failed.phase, PresentedFramePhase::Complete);
    assert_eq!(failed.outcome, PresentedFrameOutcome::Error);
    assert_exact_step(&model, &in_flight, &map_error, "MapError");

    // A stale/duplicate callback cannot complete an Armed/Pending/Complete
    // generation. These are the production gate's guard decisions.
    assert_eq!(
        presented_frame_transition(PresentedFramePhase::Armed, PresentedFrameEvent::CompleteMap),
        None
    );
    assert_eq!(
        presented_frame_transition(PresentedFramePhase::Pending, PresentedFrameEvent::MapError),
        None
    );
    assert_eq!(
        presented_frame_transition(PresentedFramePhase::Complete, PresentedFrameEvent::StartMap),
        None
    );

    // NEGATIVE CONTROL: the old fail-open decision published a frame on map
    // failure. It is not the modeled successor and violates the proof invariant.
    let mut fail_open = map_error;
    fail_open.insert("result", 1);
    assert!(
        !model
            .successors("MapError", &in_flight)
            .contains(&fail_open),
        "failed-map frame publication must not conform"
    );
    assert!(
        !model.check_invariant("SuccessRequiresMappedCopy", &fail_open),
        "negative control must expose a frame without a successful map"
    );

    // NEGATIVE CONTROL: the reject arm copied from EnqueueValid. Projected like
    // any shipping decision, it reserves the staging buffer with no copy.
    let copied_arm = project_presented(
        &initial,
        PresentedFrameEvent::RejectEnqueue,
        PresentedFrameDecision {
            phase: PresentedFramePhase::Pending,
            outcome: PresentedFrameOutcome::None,
        },
    );
    assert!(
        !model
            .successors("RejectEnqueue", &initial)
            .contains(&copied_arm)
    );
    assert!(!model.check_invariant("ReservedPhaseRequiresAcceptedCopy", &copied_arm));

    // NEGATIVE CONTROL: a failed map whose arm forgot its outcome ends the
    // one-shot with no error for the waiter to take.
    let silent = project_presented(
        &in_flight,
        PresentedFrameEvent::MapError,
        PresentedFrameDecision {
            phase: PresentedFramePhase::Complete,
            outcome: PresentedFrameOutcome::None,
        },
    );
    assert!(!model.successors("MapError", &in_flight).contains(&silent));
    assert!(!model.check_invariant("TerminalPhaseHasResult", &silent));
}

fn video_phase(raw: i64) -> VideoSlotPhase {
    match raw {
        0 => VideoSlotPhase::Free,
        1 => VideoSlotPhase::Pending,
        2 => VideoSlotPhase::InFlight,
        _ => panic!("invalid VideoTapSlot phase {raw}"),
    }
}

fn video_phase_code(phase: VideoSlotPhase) -> i64 {
    match phase {
        VideoSlotPhase::Free => 0,
        VideoSlotPhase::Pending => 1,
        VideoSlotPhase::InFlight => 2,
    }
}

fn video_slot_post(prev: &State, event: VideoSlotEvent) -> (VideoSlotDecision, State) {
    let decision = video_slot_transition(video_phase(prev["phase"]), event)
        .unwrap_or_else(|| panic!("shipping gate rejected enabled event {event:?}"));
    let mut post = prev.clone();
    post.insert("phase", video_phase_code(decision.phase));
    match event {
        VideoSlotEvent::Enqueue | VideoSlotEvent::MapOk => {
            post.insert("last_error", 0);
        }
        VideoSlotEvent::StartMap => {}
        VideoSlotEvent::MapError | VideoSlotEvent::Abort => {
            assert!(decision.count_drop);
            post.insert("dropped", prev["dropped"] + 1);
            post.insert("errors", prev["errors"] + 1);
            post.insert("last_error", 1);
        }
    }
    (decision, post)
}

#[test]
fn video_slot_shipping_gate_conforms_and_invalid_metadata_is_counted() {
    let model = video_tap_slot_model();
    let initial = model.init_state();

    let (enqueued, pending) = video_slot_post(&initial, VideoSlotEvent::Enqueue);
    assert_eq!(enqueued.phase, VideoSlotPhase::Pending);
    assert!(!enqueued.count_drop);
    assert_exact_step(&model, &initial, &pending, "Enqueue");

    let (started, in_flight) = video_slot_post(&pending, VideoSlotEvent::StartMap);
    assert_eq!(started.phase, VideoSlotPhase::InFlight);
    assert!(!started.count_drop);
    assert_exact_step(&model, &pending, &in_flight, "StartMap");

    let (mapped, free) = video_slot_post(&in_flight, VideoSlotEvent::MapOk);
    assert_eq!(mapped.phase, VideoSlotPhase::Free);
    assert!(!mapped.count_drop);
    assert_exact_step(&model, &in_flight, &free, "MapOk");

    let (failed, failed_free) = video_slot_post(&in_flight, VideoSlotEvent::MapError);
    assert_eq!(failed.phase, VideoSlotPhase::Free);
    assert!(failed.count_drop);
    assert_exact_step(&model, &in_flight, &failed_free, "MapError");

    for (state, label) in [(&pending, "pending abort"), (&in_flight, "in-flight abort")] {
        let (aborted, post) = video_slot_post(state, VideoSlotEvent::Abort);
        assert_eq!(aborted.phase, VideoSlotPhase::Free, "{label}");
        assert!(aborted.count_drop, "{label}");
        assert_exact_step(&model, state, &post, "Abort");
    }

    assert_eq!(
        video_present_decision(true, true),
        VideoPresentDecision::Capture
    );
    assert_eq!(
        video_present_decision(false, false),
        VideoPresentDecision::Decimate,
        "unrequested frames never inflate dropped"
    );
    assert_eq!(
        video_present_decision(true, false),
        VideoPresentDecision::DropInvalidMetadata
    );
    let mut invalid = initial.clone();
    invalid.insert("invalid", 1);
    invalid.insert("dropped", 1);
    assert_exact_step(&model, &initial, &invalid, "RejectInvalidMetadata");

    // Stale/duplicate completions cannot release or corrupt a reused slot.
    assert_eq!(
        video_slot_transition(VideoSlotPhase::Free, VideoSlotEvent::MapOk),
        None
    );
    assert_eq!(
        video_slot_transition(VideoSlotPhase::Pending, VideoSlotEvent::MapError),
        None
    );

    // NEGATIVE CONTROL 1: map failure counted a loss but leaked the slot.
    let mut leaked = failed_free.clone();
    leaked.insert("phase", 2);
    assert!(
        !model.successors("MapError", &in_flight).contains(&leaked),
        "failed-map slot leak must not conform"
    );
    assert!(
        !model.check_invariant("ErrorResolutionFreesSlot", &leaked),
        "negative control must expose the slot leak"
    );

    // NEGATIVE CONTROL 2: accepted invalid metadata disappeared as though it had
    // been client decimation, so coverage was understated — also after a map
    // error the take had already counted.
    for before in [initial, failed_free] {
        let mut silent_invalid = before.clone();
        silent_invalid.insert("invalid", 1);
        assert!(
            !model
                .successors("RejectInvalidMetadata", &before)
                .contains(&silent_invalid),
            "uncounted invalid-metadata loss must not conform"
        );
        assert!(
            !model.check_invariant("InvalidMetadataIsCounted", &silent_invalid),
            "negative control must expose understated dropped"
        );
    }
}

fn captured(seq: u64) -> CapturedFrame {
    CapturedFrame {
        seq,
        t_us: seq * 16_667,
        w: 2,
        h: 2,
        rgba: vec![seq as u8; 16],
    }
}

/// One capture-store push: the shipping `ordered_capture_store_push`, or a
/// one-line mis-write of it that a negative control replays.
type Push = fn(&mut VecDeque<CapturedFrame>, &mut usize, usize, CapturedFrame) -> u64;

/// Drive callback arrival 3,1,2 through `push` under a two-frame budget and
/// project each step the way the model reads the store: its two lowest
/// entries and the running eviction count.
fn harvest_3_1_2(model: &Model, push: Push) -> [State; 3] {
    let mut state = model.init_state();
    let mut store = VecDeque::new();
    let mut store_bytes = 0usize;
    let mut evicted = 0u64;
    [(3, 1), (1, 2), (2, 3)].map(|(seq, phase)| {
        evicted += push(&mut store, &mut store_bytes, 2 * 16, captured(seq));
        state.insert("harvest_phase", phase);
        state.insert(
            "store_first",
            store.front().map_or(0, |frame| frame.seq as i64),
        );
        state.insert(
            "store_second",
            store.get(1).map_or(0, |frame| frame.seq as i64),
        );
        state.insert("evicted", evicted as i64);
        state.clone()
    })
}

/// Tier-1 binding for the harvested-store projection in `VideoTapSlot`.
/// Complete callbacks in the adversarial order 3,1,2 and require shipping
/// insertion plus byte-budget eviction to match each exact modeled successor.
#[test]
fn harvested_store_sorts_callbacks_and_evicts_lowest_sequence() {
    let model = video_tap_slot_model();
    let steps = harvest_3_1_2(&model, ordered_capture_store_push);
    let mut state = model.init_state();
    for (post, action) in steps
        .iter()
        .zip(["HarvestThree", "HarvestOne", "HarvestTwo"])
    {
        assert_exact_step(&model, &state, post, action);
        state = post.clone();
    }
    assert_eq!((state["store_first"], state["store_second"]), (2, 3));
    assert_eq!(state["evicted"], 1);

    // NEGATIVE CONTROLS: the same arrival through the push written wrong, one
    // line each, projected exactly like the shipping one. Each lands on no
    // model successor and is refused by its own law.
    let three = model.successors("HarvestThree", &model.init_state())[0].clone();
    let one = model.successors("HarvestOne", &three)[0].clone();

    // The historical defect: append in callback order, evict the front.
    let callback_order: Push = |store, bytes, budget, frame| {
        *bytes += frame.rgba.len();
        store.push_back(frame);
        let mut evicted = 0;
        while *bytes > budget {
            let Some(oldest) = store.pop_front() else {
                break;
            };
            *bytes -= oldest.rgba.len();
            evicted += 1;
        }
        evicted
    };
    let [_, unsorted, wrong_tail] = harvest_3_1_2(&model, callback_order);
    assert!(!model.successors("HarvestOne", &three).contains(&unsorted));
    assert!(!model.check_invariant("HarvestedStoreSorted", &unsorted));
    assert_eq!(
        (wrong_tail["store_first"], wrong_tail["store_second"]),
        (1, 2)
    );
    assert!(!model.check_invariant("BudgetKeepsNewestTail", &wrong_tail));

    // Sorted insertion, but the budget pops the NEWEST frame.
    let evict_newest: Push = |store, bytes, budget, frame| {
        *bytes += frame.rgba.len();
        let at = store
            .iter()
            .position(|existing| existing.seq > frame.seq)
            .unwrap_or(store.len());
        store.insert(at, frame);
        let mut evicted = 0;
        while *bytes > budget {
            let Some(newest) = store.pop_back() else {
                break;
            };
            *bytes -= newest.rgba.len();
            evicted += 1;
        }
        evicted
    };
    let [_, sorted, newest_dropped] = harvest_3_1_2(&model, evict_newest);
    assert_eq!(sorted, one, "the slip is invisible until the budget bites");
    assert!(
        !model
            .successors("HarvestTwo", &one)
            .contains(&newest_dropped)
    );
    assert!(model.check_invariant("EvictionMatchesOverflow", &newest_dropped));
    assert!(!model.check_invariant("BudgetKeepsNewestTail", &newest_dropped));

    // The right tail, but the push never reports what it truncated, so the
    // take's `head_truncated` reads false.
    let unreported: Push = |store, bytes, budget, frame| {
        let _ = ordered_capture_store_push(store, bytes, budget, frame);
        0
    };
    let [_, _, silent] = harvest_3_1_2(&model, unreported);
    assert_eq!((silent["store_first"], silent["store_second"]), (2, 3));
    assert!(!model.successors("HarvestTwo", &one).contains(&silent));
    assert!(model.check_invariant("BudgetKeepsNewestTail", &silent));
    assert!(!model.check_invariant("EvictionMatchesOverflow", &silent));
}
