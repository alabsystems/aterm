// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Public scheduler contracts around console ownership: pending notes owe a
//! consumption tick, an existing flight keeps its cadence, and event freshness
//! is separate from the duration of the resulting visible response.

use std::time::{Duration, Instant};

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::Terminal;
use aterm_effects::kitty_pet::{PetAttention, PetBrain, PetFrame, PetInputKind, PetSense};
use aterm_effects::pet_world::{PetPane, PetRect, PetWorld, PetWorldFacts};

struct Scene {
    term: Terminal,
    brain: PetBrain,
    now: Instant,
}

impl Scene {
    fn new() -> Self {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b]11;#000000\x07\x1b[12;40H");
        Self {
            term,
            brain: PetBrain::default(),
            now: Instant::now(),
        }
    }

    fn tick(&mut self, millis: u64) -> PetFrame {
        self.now += Duration::from_millis(millis);
        let input = self.term.cell_frame(24, 80);
        let facts = PetWorldFacts::read(&self.term, 7);
        self.brain
            .observe_console(&input, &facts, PetPane::full(&input));
        self.brain.set_console_presentable(true);
        self.brain.tick(PetSense {
            now: self.now,
            caret: (input.cursor_visible && input.display_offset == 0)
                .then_some((input.cursor_row as u16, input.cursor_col as u16)),
            wrapped: false,
            rows: 24,
            cols: 80,
            cell_w: 8,
            cell_h: 16,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
        })
    }

    fn watching() -> Self {
        let mut s = Self::new();
        s.term
            .process(b"\x1b[8;10H\x1b]133;A\x07$ \x1b]133;B\x07build\r\n\x1b]133;C\x07working");
        let mut frame = s.tick(0);
        for _ in 0..25 {
            frame = s.tick(100);
        }
        assert_eq!(s.brain.console_attention(), PetAttention::Output);
        assert!(frame.alpha > 0, "the idle watcher is actually displayed");
        assert!(!s.brain.needs_frames());
        assert_eq!(s.brain.next_change_deadline(s.now), None);
        s
    }

    fn flying() -> (Self, PetFrame) {
        let mut s = Self::new();
        for _ in 0..30 {
            s.tick(100);
        }
        // Exercise an actual local hop. A screen-wide relocation now carries
        // the base home immediately instead of leaving the pet across the pane.
        s.term.process(b"\x1b[9;43H");
        for _ in 0..80 {
            let frame = s.tick(16);
            if frame.action.airborne() && frame.lift > 0.1 && frame.alpha > 0 {
                return (s, frame);
            }
        }
        panic!("fixture must start an actual visible legacy flight");
    }

    fn select_whole_viewport(&mut self) {
        self.term.text_selection_mut().start_selection(
            0,
            0,
            SelectionSide::Left,
            SelectionType::Simple,
        );
        self.term
            .text_selection_mut()
            .update_selection(23, 79, SelectionSide::Right);
    }

    fn complete(&mut self, failed: bool) {
        self.term.process(if failed {
            b"\r\n\x1b]133;D;1\x07"
        } else {
            b"\r\n\x1b]133;D;0\x07"
        });
        // A fast command intentionally creates no legacy cheer latch. The
        // console completion itself must supply the consumption wake.
        self.brain.note_command_done(self.now, failed, Some(100));
    }

    fn assert_prompt_wake(&self) {
        assert!(
            self.brain.needs_frames()
                || self
                    .brain
                    .next_change_deadline(self.now)
                    .is_some_and(|at| at <= self.now + Duration::from_millis(20)),
            "a pending note must not wait for its expiry or unrelated redraw"
        );
    }
}

#[test]
fn settled_console_resident_offers_a_consumption_tick_for_pending_notes() {
    let mut input = Scene::watching();
    input
        .brain
        .note_console_input(input.now, PetInputKind::Text);
    input.assert_prompt_wake();
    input.tick(16);
    assert_eq!(input.brain.console_input_consumed_seq(), 1);
    assert_eq!(input.brain.console_attention(), PetAttention::Typing);

    let mut completion = Scene::watching();
    completion.complete(false);
    completion.assert_prompt_wake();
    completion.tick(16);
    assert_eq!(completion.brain.console_attention(), PetAttention::Result);

    let mut pet = Scene::watching();
    let content = pet.brain.content();
    pet.brain.note_petted(pet.now);
    pet.assert_prompt_wake();
    pet.tick(16);
    // THE OFFER IS THE CONTRACT; the walk home is not a stranding. A console
    // resident no longer parks the pet at the ESCORT's own station (the
    // escort chooses that with its own ink ladder now), so handing the pet
    // back for affection can leave a real gap between the watching perch and
    // the caret — here a row hop back to the prompt. Stimulus consumption
    // sits deliberately BELOW every caret-travel intent (see the
    // `consume_stimuli` call site in `kitty_pet.rs`: "on the ground, below
    // every caret-travel intent"), so the touch is spent when the pet lands,
    // well inside PET_LATCH_TTL — measured at 33 frames here. What must
    // never happen is the latch expiring unconsumed, and that is what this
    // bound checks.
    let mut spent_after = None;
    for i in 0..60 {
        if pet.brain.pending_pets() == 0 {
            spent_after = Some(i);
            break;
        }
        pet.tick(16);
    }
    assert!(
        spent_after.is_some(),
        "the touch was never spent: {} still pending",
        pet.brain.pending_pets(),
    );
    assert!(pet.brain.content() > content, "the touch was consumed");
}

#[test]
fn a_fresh_but_delayed_completion_gets_its_full_visible_hold() {
    for failed in [false, true] {
        let mut s = Scene::watching();
        s.complete(failed);
        // Freshness is two seconds. Arriving after 1.3s must still buy the 1.2s
        // visible response; it must not be born already past its own hold.
        let frame = s.tick(1300);
        assert_eq!(s.brain.console_attention(), PetAttention::Result);
        assert_eq!(
            s.brain.grieving(),
            failed,
            "a failed visible result hushes celebration"
        );
        assert!(frame.alpha > 0);
        s.tick(1100);
        assert_eq!(s.brain.console_attention(), PetAttention::Result);
        assert_eq!(s.brain.grieving(), failed);
        s.tick(200);
        assert_ne!(s.brain.console_attention(), PetAttention::Result);
        assert!(!s.brain.grieving());
        s.tick(10_000);
        assert_ne!(s.brain.console_attention(), PetAttention::Result);
    }
}

#[test]
fn a_completion_past_its_admission_ttl_never_starts_a_response() {
    let mut s = Scene::watching();
    s.complete(false);
    s.tick(2100);
    assert_ne!(s.brain.console_attention(), PetAttention::Result);
    s.tick(10_000);
    assert_ne!(s.brain.console_attention(), PetAttention::Result);
    // IDLE-TO-ZERO IS A CLAIM ABOUT WHERE IT ENDS. The surviving content
    // perch is a visit, not a tenancy: it is spent after `PERCH_DWELL` and
    // hands the pet back to its own life, so the walk home and the settle
    // ladder are genuinely owed frames. Both are finite, and the pet has to
    // arrive at silence on its own — which is the thing worth asserting, and
    // the thing a decal could never do either.
    for _ in 0..(45 * 63) {
        s.tick(16);
    }
    assert_ne!(s.brain.console_attention(), PetAttention::Result);
    assert!(!s.brain.needs_frames());
    assert_eq!(s.brain.next_change_deadline(s.now), None);
}

#[test]
fn a_selection_arriving_midflight_preserves_the_arc_and_landing_cadence() {
    let (mut s, airborne) = Scene::flying();
    s.term
        .text_selection_mut()
        .start_selection(18, 2, SelectionSide::Left, SelectionType::Simple);
    s.term
        .text_selection_mut()
        .update_selection(18, 7, SelectionSide::Right);
    // Zero elapsed isolates ownership admission from flight integration.
    let selected = s.tick(0);
    assert_eq!(s.brain.console_attention(), PetAttention::Reading);
    assert!(selected.action.airborne());
    assert_eq!(
        selected.lift, airborne.lift,
        "selection cannot flatten an existing arc"
    );
    assert!(
        s.brain.needs_frames(),
        "the committed flight still owes its landing"
    );

    let mut frame = selected;
    let mut landed_body = None;
    for _ in 0..240 {
        if !s.brain.needs_frames() {
            break;
        }
        frame = s.tick(16);
        // Ownership ends when the actual flight/recovery work ends. The
        // normal follower may still emit Walk for the small home gap on this
        // frame; Sit is the resident's NEXT tick, which consumes the handoff.
        // Waiting for a settled pose would observe that consumption too late.
        if landed_body.is_none() && !s.brain.console_legacy_motion_pending() {
            assert!(frame.alpha > 0, "the completed landing stays visible");
            assert_eq!(frame.lift, 0.0, "the completed landing has grounded paws");
            let body = frame.body_px(8, 16, 80, 24).expect("visible landing");
            let input = s.term.cell_frame(24, 80);
            let facts = PetWorldFacts::read(&s.term, 7);
            let mut world = PetWorld::default();
            assert!(world.observe(&input, &facts, PetPane::full(&input)));
            let (x0, x1, y0, y1) = body;
            // THE CARET'S OWN BLANK RING IS NOT INK. `STATION_LEAD` seats
            // the escort one cell past the caret, inside that 3x3 ring by
            // construction, so the strict reading calls the escort's own
            // station occupied. Glyphs, selections, images and uncertified
            // geometry still fail here.
            assert!(
                world.clear_past_caret(
                    PetRect::new(
                        y0 as f32 / 16.0,
                        x0 as f32 / 8.0,
                        (y1 - y0) as f32 / 16.0,
                        (x1 - x0) as f32 / 8.0,
                    ),
                    0.1,
                ),
                "the actual landed body is already clear, including its margin"
            );
            assert!(
                s.brain.console_resident_handoff_pending(),
                "legacy landing owes its reading owner an actual consumption tick"
            );
            assert!(
                s.brain.needs_frames(),
                "legacy landing owes one reading handoff before cadence can stop"
            );
            landed_body = Some(body);
            frame = s.tick(0);
            assert!(!s.brain.console_resident_handoff_pending());
            assert_eq!(
                frame.body_px(8, 16, 80, 24),
                Some(body),
                "the handoff tick preserves the already-clear landing body"
            );
        }
    }
    assert!(!frame.action.airborne(), "the flight must finish");
    assert!(
        frame.action.settled(),
        "landing recovery must finish before its frame lane stops"
    );
    assert_eq!(frame.lift, 0.0);
    assert!(frame.alpha > 0, "reading remains visible after landing");
    assert_eq!(s.brain.console_attention(), PetAttention::Reading);
    assert_eq!(
        frame.body_px(8, 16, 80, 24),
        Some(landed_body.expect("fixture must expose the completed legacy landing")),
        "reading must preserve the already-clear pixel body, not reenter elsewhere"
    );
    assert!(!s.brain.needs_frames());
    assert_eq!(s.brain.next_change_deadline(s.now), None);
    let later = s.tick(30_000);
    assert_eq!(
        later.fp(),
        frame.fp(),
        "idle reading cannot finish a stranded landing"
    );
}

#[test]
fn a_landing_handoff_with_no_clear_reading_space_releases_its_frame_lane() {
    let (mut s, _) = Scene::flying();
    s.select_whole_viewport();
    let mut frame = s.tick(0);
    assert_eq!(s.brain.console_attention(), PetAttention::Reading);
    assert!(
        frame.action.airborne(),
        "protection arrived during a genuine flight"
    );
    assert_eq!(frame.alpha, 0, "selection protects even the airborne body");
    for _ in 0..300 {
        if !s.brain.needs_frames() {
            break;
        }
        frame = s.tick(16);
    }
    assert!(
        !s.brain.needs_frames(),
        "a hidden landing cannot retain cadence forever"
    );
    assert!(!frame.action.airborne());
    assert_eq!(frame.alpha, 0);
    assert_eq!(s.brain.console_attention(), PetAttention::Yielding);
    assert_eq!(s.brain.next_change_deadline(s.now), None);
    let later = s.tick(30_000);
    assert_eq!(later.alpha, 0);
    assert!(!s.brain.needs_frames());
    assert_eq!(s.brain.next_change_deadline(s.now), None);
}

#[test]
fn petting_suppressed_by_full_viewport_selection_is_consumed_without_polling() {
    let mut s = Scene::watching();
    let content = s.brain.content();
    // A valid visible-body touch is queued; selection then claims its cells
    // before the next frame. Losing custody must consume the note, not leave
    // an immortal pending latch behind the no-clear-target early return.
    s.brain.note_petted(s.now);
    assert_eq!(s.brain.pending_pets(), 1);
    s.select_whole_viewport();
    let hidden = s.tick(16);
    assert_eq!(hidden.alpha, 0);
    assert_eq!(s.brain.console_attention(), PetAttention::Yielding);
    assert_eq!(s.brain.pending_pets(), 0);
    assert_eq!(
        s.brain.content(),
        content,
        "suppression awards no affection"
    );
    assert!(!s.brain.needs_frames());
    assert_eq!(s.brain.next_change_deadline(s.now), None);
    s.tick(30_000);
    assert_eq!(s.brain.pending_pets(), 0);
    assert_eq!(s.brain.content(), content);
    assert!(!s.brain.needs_frames());
    assert_eq!(s.brain.next_change_deadline(s.now), None);
}
