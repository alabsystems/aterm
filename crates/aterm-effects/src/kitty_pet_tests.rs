// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn every_cat_pose_has_a_distinct_dog_counterpart() {
    let cats: Vec<PetGlyphId> = PET_GLYPH_IDS
        .iter()
        .copied()
        .filter(|id| !PET_GLYPHS[*id as usize].id.starts_with("pet_dog_"))
        .collect();
    assert!(!cats.is_empty(), "the cat roster must not be empty");
    let mut seen = std::collections::HashSet::new();
    for cat in cats {
        let dog = PetSpecies::Dog.skin(cat);
        let (cat_id, dog_id) = (PET_GLYPHS[cat as usize].id, PET_GLYPHS[dog as usize].id);
        assert_ne!(
            dog, cat,
            "{cat_id} fell back to the cat frame — no dog counterpart"
        );
        assert_eq!(
            dog_id,
            &format!("pet_dog_{}", cat_id.strip_prefix("pet_").unwrap()),
            "{cat_id} mapped to the wrong dog pose"
        );
        assert!(seen.insert(dog), "{dog_id} claimed by two cat poses");
    }
    for &id in PET_GLYPH_IDS {
        assert_eq!(PetSpecies::Cat.skin(id), id, "the cat skin is identity");
        let dog = PetSpecies::Dog.skin(id);
        assert_eq!(PetSpecies::Dog.skin(dog), dog, "re-skinning moved {id:?}");
    }
}

#[test]
fn changing_species_preserves_the_pets_state() {
    let t = Instant::now();
    let run = |dog: bool| {
        let mut pet = PetBrain::default();
        for i in 0..40 {
            pet.tick(sense(
                t + Duration::from_millis(i * 40),
                Some((4, 10 + i as u16 % 7)),
            ));
        }
        let before = pet.tick(sense(t + Duration::from_millis(1600), Some((4, 20))));
        if dog {
            pet.set_species(PetSpecies::Dog);
        }
        let after = pet.tick(sense(t + Duration::from_millis(1640), Some((4, 20))));
        (pet, before, after)
    };
    let (_, _, cat) = run(false);
    let (pet, before, after) = run(true);
    assert_eq!(pet.species(), PetSpecies::Dog);
    assert_eq!(after.action, before.action, "the action must not reset");
    assert!(
        (after.col - cat.col).abs() < 1e-4 && (after.row - cat.row).abs() < 1e-4,
        "the pet must not move differently when its coat changes: {} against {}",
        after.col,
        cat.col
    );
    assert!(
        PET_GLYPHS[after.pose as usize].id.starts_with("pet_dog_"),
        "the very next frame must already be a dog"
    );
}

#[test]
fn coordinate_retirement_is_dark_idempotent_and_preserves_identity() {
    let mut pet = PetBrain::default();
    let old_now = Instant::now();
    pet.species = PetSpecies::Dog;
    pet.content = 0.65;
    pet.quiet = 4.0;
    pet.clock = 19.0;
    pet.worn = Some((2, 3));
    pet.mote_serial = 7;
    pet.mote_mark = 11;
    pet.last_frolic = 13.0;
    pet.bored_cool = 2.0;
    pet.twitch_cool = 1.5;
    pet.twitch_spent = true;

    pet.alpha = 1.0;
    pet.col = 72.0;
    pet.row = 18.0;
    pet.action = PetAction::Run;
    pet.last_caret = Some((18, 70));
    pet.last_now = Some(old_now);
    pet.ink_base = 10;
    pet.ink_spans.extend_from_slice(&[(2, 20), (0, 0)]);
    pet.ink_live = Some(10);
    pet.last_pointer = Some((71.0, 18.0));
    pet.pointer_heat = 1.0;
    pet.pending_pounce = true;
    pet.pending_cheer = Some(true);
    pet.pending_sulk = true;
    pet.deferred_hidden_landing = Some((74.0, 18.0));
    pet.retired_flight_lift = Some((0.75, 2.5));

    pet.retire_coordinate_space();
    assert_eq!(pet.species(), PetSpecies::Dog);
    assert_eq!(pet.content, 0.65);
    assert_eq!(pet.quiet, 4.0);
    assert_eq!(pet.clock, 19.0);
    assert_eq!(pet.worn, Some((2, 3)));
    assert_eq!(pet.mote_serial, 7);
    assert_eq!(pet.mote_mark, 11);
    assert_eq!(pet.last_frolic, 13.0);
    assert_eq!(pet.bored_cool, 2.0);
    assert_eq!(pet.twitch_cool, 1.5);
    assert!(!pet.twitch_spent);
    assert_eq!(pet.action(), PetAction::Sit);
    assert!(!pet.is_active());
    assert!(!pet.needs_frames());
    assert!(pet.last_caret.is_none() && pet.last_now.is_none());
    assert!(pet.flight.is_none());
    assert!(pet.deferred_hidden_landing.is_none());
    assert!(pet.retired_flight_lift.is_none());
    assert!(pet.ink_spans.is_empty() && pet.ink_live.is_none());
    assert!(pet.last_pointer.is_none());
    assert_eq!(pet.pointer_heat, 0.0);
    assert!(!pet.pending_pounce);
    assert!(pet.pending_cheer.is_none() && !pet.pending_sulk);

    pet.retire_coordinate_space();
    assert_eq!(
        (pet.content, pet.worn, pet.species()),
        (0.65, Some((2, 3)), PetSpecies::Dog)
    );
    let new_now = old_now + Duration::from_secs(1);
    let expected_col = pet.station_now(12, 100, art_cols(10, 20));
    let frame = pet.tick_static_capture(sense(new_now, Some((3, 12))));
    assert_eq!(frame.alpha, 255);
    assert_eq!(frame.row, 3.0);
    assert!((frame.col - expected_col).abs() < f32::EPSILON);
    assert!(
        (frame.col - 72.0).abs() > 1.0,
        "old-pane position was retired"
    );
}

#[test]
fn coordinate_retirement_lands_the_latest_parked_identity() {
    let mut pet = PetBrain::default();
    let now = Instant::now();
    assert_eq!(pet.sync_look((2, 3), PetArrival::Ceremony).worn, (2, 3));
    pet.alpha = 1.0;
    assert_eq!(
        pet.sync_look((7, 8), PetArrival::Ceremony).worn,
        (2, 3),
        "the visible outgoing appearance keeps its old look"
    );
    assert_eq!(pet.worn, Some((2, 3)));
    assert_eq!(pet.pending_worn, Some((7, 8)));

    pet.retire_coordinate_space();
    assert_eq!(pet.worn, Some((7, 8)));
    assert!(pet.pending_worn.is_none());
    assert!(!pet.is_active());

    let frame = pet.tick_static_capture(sense(now, Some((3, 12))));
    assert!(frame.alpha > 0);
    assert_eq!(
        pet.sync_look((7, 8), PetArrival::Ceremony).worn,
        (7, 8),
        "the next lawful sighting wears the latest parked identity"
    );
    assert!(pet.pending_worn.is_none());
}

fn at_the_door(t0: Instant) -> (PetBrain, Instant) {
    let mut pet = PetBrain::default();
    let (mut t, _) = type_run(&mut pet, t0, 10, 20, 6, 0.12);
    let end = t + Duration::from_secs_f32(VIGIL_AFTER + 1.0);
    while t < end {
        t += Duration::from_millis(50);
        pet.note_executing(t, true);
        let mut sn = sense(t, Some((10, 20)));
        sn.output_burst = true;
        let _ = pet.tick(sn);
    }
    for _ in 0..20 {
        t += Duration::from_millis(100);
        pet.note_executing(t, true);
        let _ = pet.tick(sense(t, Some((10, 20))));
    }
    assert!(
        pet.vigil && pet.action == PetAction::Crouch,
        "fixture: the cat is at the door ({:?})",
        pet.action
    );
    (pet, t)
}

#[test]
fn a_shed_frame_after_a_verdict_never_strands_the_frame_lane() {
    fn release_frame(t0: Instant, shed_at: Option<usize>, hidden: bool) -> Option<usize> {
        let (mut pet, t) = at_the_door(t0);
        pet.note_command_done(t, false, Some(40_000));
        let mut t_g = t;
        for k in 0..2_000 {
            t_g += Duration::from_millis(16);
            pet.note_executing(t_g, false);
            let mut sn = sense(t_g, Some((10, 20)));
            if shed_at == Some(k) {
                if hidden {
                    sn.caret = None;
                } else {
                    sn.reduced_motion = true;
                }
            }
            let _ = pet.tick(sn);
            if !pet.needs_frames() {
                return Some(k);
            }
        }
        None
    }

    let t0 = Instant::now();
    assert!(
        release_frame(t0, None, false).is_some(),
        "fixture: the live path releases the lane"
    );
    for k in [0usize, 1, 5, 17, 33] {
        assert!(
            release_frame(t0, Some(k), false).is_some(),
            "one REDUCED frame at {k} pinned the frame lane for good"
        );
        assert!(
            release_frame(t0, Some(k), true).is_some(),
            "one HIDDEN frame at {k} pinned the frame lane for good"
        );
    }
}

#[test]
fn a_red_verdict_on_the_vigil_droops() {
    let (mut pet, t) = at_the_door(Instant::now());
    pet.note_command_done(t, true, Some(40_000));
    let mut t_r = t;
    let mut drooped = false;
    for _ in 0..40 {
        t_r += Duration::from_millis(16);
        pet.note_executing(t_r, false);
        let f = pet.tick(sense(t_r, Some((10, 20))));
        assert!(!f.action.airborne(), "a failure never flies");
        if f.action == PetAction::Droop {
            drooped = true;
            break;
        }
    }
    assert!(drooped, "a non-zero exit droops");
}

#[test]
fn a_success_soon_after_a_sulk_is_a_big_cheer() {
    let t = Instant::now();
    let mut pet = PetBrain::default();
    pet.note_command_done(t, true, Some(9_000));
    assert!(pet.pending_sulk, "the failure sulks");
    pet.pending_sulk = false;
    pet.note_command_done(t, false, Some(4_000));
    assert_eq!(
        pet.pending_cheer,
        Some(true),
        "the fix is the BIG cheer, at four seconds, because of what came \
         four seconds before it"
    );

    let mut cold = PetBrain::default();
    cold.note_command_done(t, false, Some(4_000));
    assert_eq!(
        cold.pending_cheer,
        Some(false),
        "with nothing to forgive, four seconds is an ordinary cheer"
    );
}

const fn sense(now: Instant, caret: Option<(u16, u16)>) -> PetSense {
    PetSense {
        caret_drawn: true,
        now,
        caret,
        wrapped: false,
        rows: 30,
        cols: 100,
        cell_w: 10,
        cell_h: 20,
        reduced_motion: false,
        output_burst: false,
        pointer: None,
    }
}

fn ptick(pet: &mut PetBrain, t: Instant, caret: (u16, u16), ptr: Option<(f32, f32)>) -> PetFrame {
    let mut s = sense(t, Some(caret));
    s.pointer = ptr;
    pet.tick(s)
}

fn stream(pet: &mut PetBrain, start: Instant, caret: (u16, u16), secs: f32) -> (Instant, PetFrame) {
    let mut t = start;
    let end = start + Duration::from_secs_f32(secs);
    let mut s = sense(t, Some(caret));
    s.output_burst = true;
    let mut frame = pet.tick(s);
    while t < end {
        t += Duration::from_millis(16);
        let mut s = sense(t, Some(caret));
        s.output_burst = true;
        frame = pet.tick(s);
    }
    (t, frame)
}

fn type_run(
    pet: &mut PetBrain,
    start: Instant,
    row: u16,
    col0: u16,
    keys: u16,
    gap: f32,
) -> (Instant, PetFrame) {
    let mut t = start;
    let mut frame = pet.tick(sense(t, Some((row, col0))));
    for k in 1..=keys {
        let until = t + Duration::from_secs_f32(gap);
        while t < until {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, Some((row, col0 + k - 1))));
        }
        t = until;
        frame = pet.tick(sense(t, Some((row, col0 + k))));
    }
    (t, frame)
}

fn awake(pet: &mut PetBrain, start: Instant, row: u16, col: u16) -> Instant {
    let (t, _) = type_run(pet, start, row, col, 2, 0.05);
    let (t, _) = idle(pet, t, (row, col + 2), SIT_AFTER + 0.2);
    t
}

fn idle(pet: &mut PetBrain, start: Instant, caret: (u16, u16), secs: f32) -> (Instant, PetFrame) {
    let mut t = start;
    let end = start + Duration::from_secs_f32(secs);
    let mut frame = pet.tick(sense(t, Some(caret)));
    while t < end {
        t += Duration::from_millis(16);
        frame = pet.tick(sense(t, Some(caret)));
    }
    (t, frame)
}

fn veiled_over(pet: &mut PetBrain, t: &mut Instant, action: PetAction, ink: u16, secs: f32) -> u8 {
    let mut spans = vec![(0u16, 0u16); 30];
    spans[29] = (0, ink);
    pet.sense_ink(0, &spans, Some(29));
    let mut faintest = 255u8;
    let end = *t + Duration::from_secs_f32(secs);
    while *t < end {
        *t += Duration::from_millis(16);
        // Each frame is one the pet asked for: a tick that ends a sleep eases no veil
        // ([`PetBrain::tick`]).
        pet.asked_frames = true;
        let mut frame = PetFrame {
            alpha: 255,
            lane_alpha: 255,
            action,
            pose: PetGlyphId::PetWalk0,
            col: 20.0,
            row: 29.0,
            lift: 0.0,
            facing_left: false,
            scale_x: 1.0,
            scale_y: 1.0,
            stretch_from: StretchFrom::Centre,
            purr: 0.0,
            contented: false,
            under_ink: false,
            motes: [None; PET_MOTES_MAX],
            departures: [None; PET_DEPARTURES_MAX],
        };
        pet.veil_over_letters(&mut frame, sense(*t, Some((29, 70))));
        faintest = faintest.min(frame.alpha);
    }
    faintest
}

#[test]
fn the_pet_never_walks_on_a_line_of_letters() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = start;
    assert_eq!(
        veiled_over(&mut pet, &mut t, PetAction::Walk, 0, 0.6),
        255,
        "a walk along a blank line is solid"
    );
    assert_eq!(
        veiled_over(
            &mut pet,
            &mut t,
            PetAction::Walk,
            60,
            LETTER_VEIL_DWELL - 0.04
        ),
        255,
        "a step over letters shorter than the dwell is crossed full"
    );
    let _ = veiled_over(&mut pet, &mut t, PetAction::Walk, 0, 0.3);
    let walked = veiled_over(&mut pet, &mut t, PetAction::Run, 60, 0.6);
    assert!(
        u32::from(walked) * 2 < 255,
        "the cat running on letters is not a ghost: {walked}"
    );
    let _ = veiled_over(&mut pet, &mut t, PetAction::Sit, 60, 0.6);
    assert_eq!(
        veiled_over(&mut pet, &mut t, PetAction::Sit, 60, 0.1),
        255,
        "the settled cat stands full"
    );
    for action in [PetAction::Crouch, PetAction::Leap, PetAction::Land] {
        assert_eq!(
            veiled_over(&mut pet, &mut t, action, 60, 0.6),
            255,
            "a flight over letters keeps its body ({action:?})"
        );
    }
}

#[test]
fn one_leg_in_three_crosses_the_letters_in_front() {
    let mut pet = PetBrain::default();
    let mut sides = Vec::new();
    for _ in 0..2 * FRONT_PASS_EVERY {
        pet.deal_front_pass(PetAction::Walk);
        pet.deal_front_pass(PetAction::Run);
        sides.push(pet.front_pass());
        pet.deal_front_pass(PetAction::Stand);
        assert!(!pet.front_pass(), "no side is dealt between legs");
    }
    assert_eq!(sides, [false, false, true, false, false, true]);

    let mut t = Instant::now();
    let mut behind = PetBrain::default();
    behind.deal_front_pass(PetAction::Walk);
    assert!(
        veiled_over(&mut behind, &mut t, PetAction::Walk, 60, 0.6) < 255,
        "fixture: a leg behind dims under the letters"
    );
    let mut front = PetBrain {
        front_pass_serial: FRONT_PASS_EVERY - 1,
        ..PetBrain::default()
    };
    front.deal_front_pass(PetAction::Walk);
    assert_eq!(
        veiled_over(&mut front, &mut t, PetAction::Walk, 60, 0.6),
        255,
        "a leg in front crosses the letters whole"
    );
    assert_eq!(front.letter_veil.0, 1.0, "and over them");
}

#[test]
fn a_writing_hand_keeps_every_leg_behind_the_letters() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let (t, _) = type_run(&mut pet, start, 29, 18, 4, 0.05);
    pet.note_console_input(t, PetInputKind::Text);
    for _ in 0..2 * FRONT_PASS_EVERY {
        pet.deal_front_pass(PetAction::Walk);
        assert!(!pet.front_pass(), "a leg set out while typing goes under");
        pet.deal_front_pass(PetAction::Stand);
    }
    let (_, _) = idle(&mut pet, t, (29, 22), SIT_AFTER + 0.2);
    pet.front_pass_serial = FRONT_PASS_EVERY - 1;
    pet.deal_front_pass(PetAction::Stand);
    pet.deal_front_pass(PetAction::Walk);
    assert!(
        pet.front_pass(),
        "control: a rested hand lets a leg go in front"
    );
}

#[test]
fn only_a_settled_or_travelling_body_off_the_letters_lifts_the_rest_veil() {
    use crate::pet_world::{PetPane, PetWorldFacts};
    use aterm_core::terminal::Terminal;

    let mut term = Terminal::new(30, 100);
    term.process(format!("\x1b[29;1H{}\x1b[30;3H", "x".repeat(100)).as_bytes());
    let mut pet = PetBrain::default();
    let input = term.cell_frame(30, 100);
    let facts = PetWorldFacts::read(&term, 7);
    pet.observe_console(&input, &facts, PetPane::full(&input));
    pet.set_console_presentable(true);
    let frame = |action: PetAction, lift: f32, scale_y: f32| PetFrame {
        alpha: 255,
        lane_alpha: 255,
        action,
        pose: PetGlyphId::PetStand,
        col: 20.0,
        row: 29.0,
        lift,
        facing_left: false,
        scale_x: 1.0,
        scale_y,
        stretch_from: StretchFrom::Centre,
        purr: 0.0,
        contented: false,
        under_ink: false,
        motes: [None; PET_MOTES_MAX],
        departures: [None; PET_DEPARTURES_MAX],
    };
    let mut t = Instant::now();
    let mut run = |pet: &mut PetBrain, f: PetFrame, secs: f32| {
        let end = t + Duration::from_secs_f32(secs);
        let mut most = 0u8;
        let mut last = 0u8;
        while t < end {
            t += Duration::from_millis(16);
            pet.asked_frames = true;
            let mut f = f;
            pet.veil_over_letters(&mut f, sense(t, Some((29, 2))));
            most = most.max(f.alpha);
            last = f.alpha;
        }
        (most, last)
    };
    let (_, rested) = run(&mut pet, frame(PetAction::Sit, 0.0, 1.0), 1.0);
    assert!(rested < 64, "fixture: the resting cat is veiled ({rested})");
    let low = 0.7;
    for (action, lift, scale_y) in [
        (PetAction::Waking, 0.0, low),
        (PetAction::Droop, 0.0, low),
        (PetAction::Crouch, 0.0, low),
        (PetAction::Leap, 3.0, 1.0),
        (PetAction::Land, 0.0, low),
    ] {
        let (most, _) = run(&mut pet, frame(action, lift, scale_y), 0.6);
        assert!(
            most <= rested,
            "{action:?} off the letters lifted the rest veil from {rested} to {most}"
        );
    }
    let (most, _) = run(&mut pet, frame(PetAction::Walk, 0.0, 1.0), 0.6);
    assert!(
        most <= rested,
        "a walk on the letters lifted the rest veil from {rested} to {most}"
    );
    let (_, lifted) = run(&mut pet, frame(PetAction::Walk, 0.0, low), 0.6);
    assert_eq!(lifted, 255, "a walk off the letters lifts it");
    let (_, rested) = run(&mut pet, frame(PetAction::Sit, 0.0, 1.0), 1.0);
    assert!(
        rested < 64,
        "back on the letters at rest it is veiled again"
    );
    let (_, lifted) = run(&mut pet, frame(PetAction::Sit, 0.0, low), 0.6);
    assert_eq!(lifted, 255, "a settled body off the letters lifts it");
}

#[test]
fn a_cat_walking_under_a_line_of_ink_keeps_its_body() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut spans = vec![(0u16, 0u16); 30];
    spans[19] = (0, 100);
    pet.sense_ink(0, &spans, Some(19));
    let t = awake(&mut pet, start, 20, 10);
    let mut t = t;
    let mut walked = Vec::new();
    for k in 0..30u16 {
        let until = t + Duration::from_millis(140);
        while t < until {
            t += Duration::from_millis(16);
            pet.sense_ink(0, &spans, Some(19));
            let f = pet.tick(sense(t, Some((20, 12 + k))));
            if matches!(f.action, PetAction::Walk | PetAction::Run) {
                walked.push(f.alpha);
            }
        }
    }
    assert!(
        walked.len() > 60,
        "fixture: the cat walks ({})",
        walked.len()
    );
    let faintest = walked.iter().copied().min().unwrap_or(0);
    assert!(
        faintest > 200,
        "the walking cat is veiled: alpha down to {faintest} over {} presents",
        walked.len()
    );
}

#[test]
fn a_first_sighting_materialises_at_the_station_not_at_the_origin() {
    let mut pet = PetBrain::default();
    let f = pet.tick(sense(Instant::now(), Some((5, 40))));
    let w = art_cols(10, 20);
    assert!(
        (f.col - PetBrain::station(40, 100, w)).abs() < 0.01,
        "first frame must appear at its station, got col {}",
        f.col
    );
    assert!((f.row - 5.0).abs() < 0.01, "and on the caret's row");
}

#[test]
fn steady_typing_walks_and_fast_typing_runs() {
    let start = Instant::now();
    let mut slow = PetBrain::default();
    let t = awake(&mut slow, start, 3, 5);
    let (_, slow_f) = type_run(&mut slow, t, 3, 7, 10, 0.30);
    let mut fast = PetBrain::default();
    let t = awake(&mut fast, start, 3, 5);
    let (_, fast_f) = type_run(&mut fast, t, 3, 7, 30, 0.045);
    assert_eq!(
        slow_f.action,
        PetAction::Walk,
        "an unhurried cadence walks the pet"
    );
    assert_eq!(
        fast_f.action,
        PetAction::Run,
        "a fast cadence outruns a walk"
    );
}

#[test]
fn the_gait_cycle_is_driven_by_distance_not_by_the_clock() {
    let start = Instant::now();
    let mut near = PetBrain::default();
    let t = awake(&mut near, start, 3, 5);
    let _ = type_run(&mut near, t, 3, 7, 4, 0.05);
    let mut far = PetBrain::default();
    let t = awake(&mut far, start, 3, 5);
    let _ = type_run(&mut far, t, 3, 7, 30, 0.05);
    assert!(
        far.stride > near.stride * 2.0,
        "more travel = more stride ({} vs {})",
        far.stride,
        near.stride
    );
}

#[test]
fn a_low_cadence_host_still_puts_the_pet_to_sleep_on_time() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let (mut t, _) = type_run(&mut pet, start, 4, 10, 4, 0.05);
    let mut slept_at = None;
    for i in 0..40 {
        t += Duration::from_millis(1000);
        let f = pet.tick(sense(t, Some((4, 14))));
        if f.action == PetAction::Sleep && slept_at.is_none() {
            slept_at = Some(i);
        }
    }
    let i = slept_at.expect("a 1 Hz host must still reach sleep");
    assert!(
        (SLEEP_AFTER as i32 - 2..=SLEEP_AFTER as i32 + 2).contains(&i),
        "slept after {i} one-second frames, expected ~{SLEEP_AFTER}"
    );
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct MoteLook {
    kind: PetMoteKind,
    cx: i32,
    cy: i32,
    scale: i32,
    rot: i32,
    alpha: u8,
}

impl MoteLook {
    fn within(self, trio: [Option<MoteLook>; 3]) -> bool {
        macro_rules! bracketed {
            ($($field:ident),*) => {
                $( trio.iter().flatten().any(|b| b.$field == self.$field) )&&*
            };
        }
        bracketed!(cx, cy, scale, rot, alpha)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Look {
    action: PetAction,
    pose: PetGlyphId,
    box_h: i32,
    box_w: i32,
    row: i32,
    alpha: u8,
    facing_left: bool,
    motes: usize,
    keyed: [Option<MoteLook>; PET_MOTES_MAX],
}

impl Look {
    fn within(&self, trio: [&Look; 3]) -> bool {
        macro_rules! bracketed {
            ($($field:ident),*) => {
                $( trio.iter().any(|b| b.$field == self.$field) )&&*
            };
        }
        bracketed!(action, pose, box_h, box_w, row, alpha, facing_left, motes)
            && self.keyed.iter().enumerate().all(|(i, z)| match z {
                None => trio.iter().any(|b| b.keyed[i].is_none()),
                Some(z) => z.within([trio[0].keyed[i], trio[1].keyed[i], trio[2].keyed[i]]),
            })
    }
}

const fn note_births(f: &PetFrame, births: &mut [Option<f32>; PET_MOTES_MAX]) {
    let mut i = 0;
    while i < PET_MOTES_MAX {
        match f.motes[i] {
            Some(m) if m.kind.rides_offer() => {
                if births[i].is_none() {
                    births[i] = Some(f.col);
                }
            }
            _ => births[i] = None,
        }
        i += 1;
    }
}

const fn zee_shifts(
    pet: &[Option<f32>; PET_MOTES_MAX],
    twin: &[Option<f32>; PET_MOTES_MAX],
) -> [f32; PET_MOTES_MAX] {
    let mut out = [0.0; PET_MOTES_MAX];
    let mut i = 0;
    while i < PET_MOTES_MAX {
        if let (Some(p), Some(t)) = (pet[i], twin[i]) {
            out[i] = p - t;
        }
        i += 1;
    }
    out
}

const NO_SHIFT: [f32; PET_MOTES_MAX] = [0.0; PET_MOTES_MAX];

const fn visible(f: &PetFrame, shift: [f32; PET_MOTES_MAX]) -> Look {
    let h = (ART_ROWS * 20.0).round();
    let w = (h * ART_ASPECT).round();
    let mut keyed = [None; PET_MOTES_MAX];
    let mut motes = 0;
    let mut i = 0;
    while i < PET_MOTES_MAX {
        if let Some(m) = f.motes[i] {
            motes += 1;
            if m.kind.rides_offer() {
                keyed[i] = Some(MoteLook {
                    kind: m.kind,
                    cx: ((m.col + shift[i]) * 10.0).round() as i32,
                    cy: (m.row * 20.0).round() as i32,
                    scale: (m.scale * 1024.0) as i32,
                    rot: (m.rot * 1024.0) as i32,
                    alpha: m.alpha,
                });
            }
        }
        i += 1;
    }
    Look {
        action: f.action,
        pose: f.pose,
        box_h: (f.scale_y * h).round() as i32,
        box_w: (f.scale_x * w).round() as i32,
        row: (f.row * 64.0) as i32,
        alpha: f.alpha,
        facing_left: f.facing_left,
        motes,
        keyed,
    }
}

const fn same_feet(a: f32, b: f32) -> bool {
    (a - b).abs() * 10.0 < 1.0
}

#[derive(Debug, Default)]
struct Walk {
    offers: u32,
    train: u32,
    idle_wakes: u32,
    slop: u32,
    zee_train: u32,
    zee_offers: u32,
    note_train: u32,
    note_offers: u32,
    note_edges: u32,
    note_births: u32,
    dust_train: u32,
    pop_train: u32,
}

fn walk_offers(
    pet: &mut PetBrain,
    twin: &mut PetBrain,
    start: Instant,
    caret: (u16, u16),
    secs: f32,
) -> (Walk, Instant) {
    let grid = Duration::from_millis(16);
    let hair = Duration::from_millis(1);
    let end = start + Duration::from_secs_f32(secs);
    let mut walk = Walk::default();
    let mut t = start;
    let mut tb = start;
    let mut pet_birth = [None; PET_MOTES_MAX];
    let mut twin_birth = [None; PET_MOTES_MAX];
    let first = pet.tick(sense(t, Some(caret)));
    note_births(&first, &mut pet_birth);
    let mut last_full = visible(&first, NO_SHIFT);
    let mut last = last_full;
    let mut last_col = first.col;
    let mut b_last = twin.tick(sense(t, Some(caret)));
    note_births(&b_last, &mut twin_birth);
    let mut was_idle = false;
    while t < end {
        let parked = !pet.needs_frames();
        let own = !parked && !needs_frames_without_motes(pet);
        let next = if parked {
            let Some(d) = pet.next_change_deadline(t) else {
                break;
            };
            assert!(d > t, "a deadline at `now` is a busy re-arm");
            walk.offers += 1;
            d
        } else {
            walk.train += 1;
            t + grid
        };
        if was_idle {
            if next - t <= 2 * hair {
                walk.slop += 1;
            } else {
                walk.idle_wakes += 1;
            }
        }
        let before =
            |twin: &mut PetBrain, twin_birth: &mut [Option<f32>; PET_MOTES_MAX], at: Instant| {
                let f = twin.tick(sense(at, Some(caret)));
                note_births(&f, twin_birth);
                if parked {
                    assert_eq!(
                        visible(&f, zee_shifts(&pet_birth, twin_birth)),
                        last,
                        "the frame changed at +{:?}, before the offer at +{:?} — a late offer",
                        at - start,
                        next - start
                    );
                    assert!(
                        same_feet(f.col, last_col),
                        "the feet moved a pixel at +{:?}, before the offer at +{:?}",
                        at - start,
                        next - start
                    );
                }
                f
            };
        if parked {
            while tb + grid < next {
                tb += grid;
                b_last = before(twin, &mut twin_birth, tb);
            }
            if next - hair > tb {
                tb = next - hair;
                b_last = before(twin, &mut twin_birth, tb);
            }
        }
        t = next;
        let a = pet.tick(sense(t, Some(caret)));
        let drew = |kinds: &[PetMoteKind]| {
            a.motes
                .iter()
                .flatten()
                .any(|m| kinds.contains(&m.kind) && m.alpha > 0)
        };
        if drew(&[PetMoteKind::Zee]) {
            if parked {
                walk.zee_offers += 1;
            } else if own {
                walk.zee_train += 1;
            }
        }
        if drew(&[PetMoteKind::Note, PetMoteKind::Heart]) {
            if parked {
                walk.note_offers += 1;
            } else if own {
                walk.note_train += 1;
            }
        }
        if own && drew(&[PetMoteKind::Dust]) {
            walk.dust_train += 1;
        }
        if own && drew(&[PetMoteKind::ZeePop]) {
            walk.pop_train += 1;
        }
        note_births(&a, &mut pet_birth);
        let b_at = twin.tick(sense(t, Some(caret)));
        note_births(&b_at, &mut twin_birth);
        let b_after = if parked {
            tb = t + hair;
            let f = twin.tick(sense(tb, Some(caret)));
            note_births(&f, &mut twin_birth);
            f
        } else {
            tb = t;
            b_at
        };
        let unkey = |mut look: Look, offered: bool| {
            if !offered {
                look.keyed = [None; PET_MOTES_MAX];
            }
            look
        };
        let full = visible(&a, NO_SHIFT);
        let is_note = |l: &Option<MoteLook>| {
            l.is_some_and(|l| matches!(l.kind, PetMoteKind::Note | PetMoteKind::Heart))
        };
        let mut note_edge = false;
        for (now, prev) in full.keyed.iter().zip(last_full.keyed.iter()) {
            if is_note(now) && !is_note(prev) {
                walk.note_births += 1;
            }
            if (is_note(now) || is_note(prev)) && now != prev {
                note_edge = true;
            }
        }
        if parked && note_edge {
            walk.note_edges += 1;
        }
        let key = unkey(full, parked);
        let shift = zee_shifts(&pet_birth, &twin_birth);
        let trio = [
            unkey(visible(&b_last, shift), parked),
            unkey(visible(&b_at, shift), parked),
            unkey(visible(&b_after, shift), parked),
        ];
        assert!(
            key.within([&trio[0], &trio[1], &trio[2]]),
            "golden: at +{:?} the offered frame {key:?} is not the train's frame \
             ({:?} / {:?} / {:?} a millisecond either side); feet {} vs {}",
            t - start,
            trio[0],
            trio[1],
            trio[2],
            a.col,
            b_at.col
        );
        assert!(
            [&b_last, &b_at, &b_after].iter().any(|b| {
                (a.scale_y - b.scale_y).abs() < 1e-3 && (a.scale_x - b.scale_x).abs() < 1e-3
            }),
            "golden: the scale at +{:?} drifted ({}, {}) vs ({}, {}) / ({}, {}) / ({}, {})",
            t - start,
            a.scale_x,
            a.scale_y,
            b_last.scale_x,
            b_last.scale_y,
            b_at.scale_x,
            b_at.scale_y,
            b_after.scale_x,
            b_after.scale_y
        );
        assert!(
            pet.needs_frames() || same_feet(a.col, b_at.col),
            "golden: the feet at +{:?} drifted a pixel ({} vs {})",
            t - start,
            a.col,
            b_at.col
        );
        was_idle = parked && full == last_full;
        last_full = full;
        last = unkey(full, !pet.needs_frames());
        last_col = a.col;
        b_last = b_after;
    }
    (walk, t)
}

const fn sense_cells(
    now: Instant,
    caret: Option<(u16, u16)>,
    cell_w: u16,
    cell_h: u16,
) -> PetSense {
    PetSense {
        caret_drawn: true,
        cell_w,
        cell_h,
        ..sense(now, caret)
    }
}

#[test]
fn a_move_that_ends_a_pause_lands_on_windows_the_pause_has_aged() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 4, 10);
    let mut col = 12u16;
    for _ in 0..6 {
        t += Duration::from_millis(100);
        col += 1;
        pet.tick(sense(t, Some((4, col))));
    }
    t += Duration::from_secs(4);
    col += 1;
    pet.tick(sense(t, Some((4, col))));
    let fresh = (1.0 / VEL_TAU) * (-0.1f32 / VEL_TAU).exp();
    assert!(
        pet.vhat >= 0.95 * fresh,
        "the key that ended the pause must keep its impulse: vhat {} (fresh impulse {fresh})",
        pet.vhat
    );
}

#[test]
fn the_offer_is_the_renderer_s_own_pixel_step_at_an_odd_cell_height() {
    let start = Instant::now();
    let (cw, ch) = (7u16, 14u16);
    let mut pet = PetBrain::default();
    let mut twin = PetBrain::default();
    let mut t = awake(&mut pet, start, 4, 10);
    let tt = awake(&mut twin, start, 4, 10);
    assert_eq!(t, tt, "the twins share one clock");
    pet.force_seed(0);
    twin.force_seed(0);
    let mut settled = 0.0f32;
    while settled < SLEEP_AFTER + 0.5 {
        t += Duration::from_millis(16);
        pet.tick(sense_cells(t, Some((4, 12)), cw, ch));
        twin.tick(sense_cells(t, Some((4, 12)), cw, ch));
        settled += 0.016;
    }
    assert_eq!(pet.action, PetAction::Sleep, "fixture: a light sleeper");
    let hair = Duration::from_millis(1);
    let mut twin_t = t;
    let mut twin_box = twin
        .tick(sense_cells(twin_t, Some((4, 12)), cw, ch))
        .body_px(cw, ch, 100, 30);
    let mut checked = 0;
    while checked < 10 {
        if pet.needs_frames() {
            t += Duration::from_millis(16);
            pet.tick(sense_cells(t, Some((4, 12)), cw, ch));
            continue;
        }
        let Some(offer) = pet.next_change_deadline(t) else {
            break;
        };
        let mut first_change = None;
        while twin_t < offer + hair {
            twin_t += hair;
            let f = twin.tick(sense_cells(twin_t, Some((4, 12)), cw, ch));
            let b = f.body_px(cw, ch, 100, 30);
            if b != twin_box {
                twin_box = b;
                first_change.get_or_insert(twin_t);
            }
        }
        if let Some(first) = first_change {
            assert!(
                offer <= first + hair,
                "offer #{checked} is late: offered +{:?} after the check began, the drawn box changed at +{:?}",
                offer - t,
                first - t
            );
        }
        checked += 1;
        t = offer;
        pet.tick(sense_cells(t, Some((4, 12)), cw, ch));
    }
    assert!(checked >= 5, "fixture: the sleeper offered {checked} steps");
}

fn settled_pair(start: Instant, quiet: f32) -> (PetBrain, PetBrain, Instant) {
    let mut pet = PetBrain::default();
    let mut twin = PetBrain::default();
    let t = awake(&mut pet, start, 4, 10);
    let tt = awake(&mut twin, start, 4, 10);
    assert_eq!(t, tt, "the twins share one clock");
    pet.force_seed(0);
    twin.force_seed(0);
    let more = quiet - (SIT_AFTER + 0.2);
    let (t, _) = idle(&mut pet, t, (4, 12), more);
    let _ = idle(&mut twin, tt, (4, 12), more);
    (pet, twin, t)
}

#[test]
fn a_moving_pet_still_asks_for_every_frame() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 4, 4);
    let (mut perk, mut leap, mut land) = (false, false, false);
    for _ in 0..600 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 60))));
        perk |= f.action == PetAction::Perk;
        leap |= f.action == PetAction::Leap;
        land |= f.action == PetAction::Land;
        let moving = !pet.action.settled()
            || pet.action == PetAction::Perk
            || pet.flight.is_some()
            || pet.land_t > 0.0;
        if !moving {
            break;
        }
        assert!(
            pet.needs_frames(),
            "moving ({:?}, flight {}, land_t {}) must hold the frame train",
            f.action,
            pet.flight.is_some(),
            pet.land_t
        );
    }
    assert!(
        perk && leap && land,
        "fixture: the crossing must notice, fly and land (perk {perk}, leap {leap}, land {land})"
    );
    let (t, _) = idle(&mut pet, t, (4, 61), 3.0);
    assert!(!pet.needs_frames(), "settled: parked");
    pet.note_petted(t);
    assert!(
        pet.needs_frames(),
        "a latched pet needs the tick that consumes it"
    );
    let f = pet.tick(sense(t + Duration::from_millis(16), Some((4, 61))));
    assert!(
        pet.pet_hold_t > 0.0 && pet.needs_frames(),
        "the petting hold ({:?}) animates every frame",
        f.action
    );
}

#[test]
fn a_lone_key_on_a_quiet_screen_costs_the_pet_a_counted_census() {
    let start = Instant::now();
    let (mut pet, mut twin, t) = settled_pair(start, 5.0);
    let t = t + Duration::from_millis(16);
    let _ = pet.tick(sense(t, Some((4, 13))));
    let _ = twin.tick(sense(t, Some((4, 13))));
    assert!(pet.quiet < 0.1, "fixture: the key reset the quiet clock");
    let (walk, end) = walk_offers(&mut pet, &mut twin, t, (4, 13), 40.0);
    assert_eq!(pet.action, PetAction::Sleep);
    assert!(
        pet.quiet >= SLEEP_AFTER + BREATH_WINDOW,
        "the walk must end in deep sleep, not on a timeout: quiet {}",
        pet.quiet
    );
    let span = (end - t).as_secs_f32();
    eprintln!(
        "pet lane census (one key on a quiet screen → deep sleep): {walk:?} over {span:.1} s; \
         the old train was {} frames",
        (span * 60.0) as u32
    );
    assert_eq!(walk.zee_train, 0, "a z held the frame train ({walk:?})");
    assert!(
        walk.train <= 50 && walk.offers - walk.zee_offers <= 50 && walk.zee_offers <= 300,
        "{walk:?} over {span:.1} s — the old train was {} frames",
        (span * 60.0) as u32
    );
    assert!(
        walk.zee_offers >= 100,
        "the light sleeper's z's ride the offer ({walk:?})"
    );
}

fn contented_pair(start: Instant, quiet: f32) -> (PetBrain, PetBrain, Instant) {
    let mut pet = PetBrain::default();
    let mut twin = PetBrain::default();
    let t = awake(&mut pet, start, 4, 10);
    let tt = awake(&mut twin, start, 4, 10);
    assert_eq!(t, tt, "the twins share one clock");
    pet.force_seed(0);
    twin.force_seed(0);
    let (t, _) = type_run(&mut pet, t, 4, 12, 60, 0.05);
    let (tt, _) = type_run(&mut twin, tt, 4, 12, 60, 0.05);
    assert_eq!(t, tt, "the twins share one clock");
    let (t, _) = idle(&mut pet, t, (4, 72), quiet);
    let _ = idle(&mut twin, tt, (4, 72), quiet);
    assert!(
        pet.content >= PURR_GATE,
        "fixture: the run earned a purr (content {})",
        pet.content
    );
    (pet, twin, t)
}

const fn needs_frames_without_motes(pet: &mut PetBrain) -> bool {
    let lane = pet.motes;
    pet.motes = [None; PET_MOTES_MAX];
    let needs = pet.needs_frames();
    pet.motes = lane;
    needs
}

#[test]
fn a_purring_seat_s_notes_and_hearts_ride_the_offer_not_the_frame_train() {
    let start = Instant::now();
    let (mut pet, mut twin, mut t) = contented_pair(start, SIT_AFTER + 0.3);
    assert!(pet.purring(), "fixture: a purring seat ({:?})", pet.action);
    let mut note = None;
    for _ in 0..800 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 72))));
        let _ = twin.tick(sense(t, Some((4, 72))));
        let floated = f
            .motes
            .iter()
            .flatten()
            .find(|m| matches!(m.kind, PetMoteKind::Note | PetMoteKind::Heart) && m.alpha > 0);
        if let Some(m) = floated
            && !needs_frames_without_motes(&mut pet)
        {
            note = Some(*m);
            break;
        }
    }
    let note = note.expect("fixture: the seat floats a ♪ or ♥ while otherwise parked");
    assert!(pet.purring(), "fixture: still purring ({:?})", pet.action);
    assert!(
        !pet.needs_frames(),
        "a purring seat's ♪/♥ ride the offer, not the frame train \
         (a {:?} at ({}, {}), alpha {})",
        note.kind,
        note.col,
        note.row,
        note.alpha
    );
    let d = pet
        .next_change_deadline(t)
        .expect("a live ♪ has a next edge");
    assert!(
        d <= t + Duration::from_secs_f32(PURR_MOTE_LIFE / PURR_MOTE_STEPS + 0.001),
        "the ♪'s next look is at most one slice away: +{:?}",
        d - t
    );
    let (walk, end) = walk_offers(
        &mut pet,
        &mut twin,
        t,
        (4, 72),
        SLEEP_AFTER + BREATH_WINDOW + 2.0,
    );
    assert!(
        pet.quiet >= SLEEP_AFTER + BREATH_WINDOW && pet.motes.iter().all(Option::is_none),
        "the walk must end in deep sleep with an empty lane: quiet {}",
        pet.quiet
    );
    let span = (end - t).as_secs_f32();
    eprintln!("pet lane census (a purring seat, the ♪/♥ on the offer): {walk:?} over {span:.1} s");
    assert_eq!(walk.note_train, 0, "a ♪/♥ held the frame train ({walk:?})");
    assert_eq!(walk.zee_train, 0, "a z held the frame train ({walk:?})");
    let climb_px = (0.85 * 20.0_f32).ceil() + 1.0;
    let sway_px = ((0.30 + 0.40) * 10.0_f32).ceil() + 1.0 + 2.0;
    let per_note = (climb_px + sway_px + PURR_MOTE_STEPS) as u32;
    assert!(
        walk.note_births >= 3,
        "fixture: the ledger buys a run of ♪/♥ ({walk:?})"
    );
    assert!(
        walk.note_edges >= 30 && walk.note_edges <= (walk.note_births + 1) * per_note,
        "the ♪/♥ are drawn on the offer — a whole pixel, a look, a birth, a death at a \
         time: {} edges for {} of them, the bound {per_note} a ♪ ({walk:?})",
        walk.note_edges,
        walk.note_births
    );
    assert!(
        walk.idle_wakes <= 40,
        "{} wakes changed nothing — the tell's offer is too conservative ({walk:?})",
        walk.idle_wakes
    );
}

#[test]
fn a_zero_dt_tick_does_not_park_a_gliding_body() {
    let start = Instant::now();
    let (mut pet, _, t) = contented_pair(start, 2.0);
    pet.col -= 0.5;
    let t = t + Duration::from_millis(16);
    let _ = pet.tick(sense(t, Some((4, 73))));
    let t = t + Duration::from_millis(16);
    let _ = pet.tick(sense(t, Some((4, 73))));
    let gap_px = (pet.home - pet.col).abs() * 10.0;
    assert!(
        gap_px >= 1.0 && pet.speed.abs() < 0.01,
        "fixture: the body is easing home with a pixel to go ({gap_px} px, speed {})",
        pet.speed
    );
    assert!(pet.needs_frames(), "fixture: the glide holds the lane");
    let _ = pet.tick(sense(t, Some((4, 73))));
    assert!(
        pet.needs_frames(),
        "a zero-dt tick parked a body with {gap_px} px still to glide"
    );
}

/// The rows a landing's dust puff spans, frame by frame: each live puff's centre and the
/// reach of its discs (at most 0.35 of the tile's side from the centre at any turn), against
/// the feet row's top.
fn landing_dust_rows(dust_floats: bool) -> Vec<(f32, f32, f32)> {
    let start = Instant::now();
    let mut pet = PetBrain {
        dust_floats,
        ..PetBrain::default()
    };
    let mut t = awake(&mut pet, start, 4, 4);
    let caret = (4u16, 90u16);
    let s = sense(t, Some(caret));
    let side = f32::from(mote_tile_side(s.cell_h)) / f32::from(s.cell_h);
    let mut rows = Vec::new();
    for _ in 0..300 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some(caret)));
        for m in f.motes.iter().flatten() {
            if m.kind == PetMoteKind::Dust && m.alpha > 0 {
                let reach = 0.35 * side * m.scale;
                rows.push((m.row - f.row, m.row - reach - f.row, m.row + reach - f.row));
            }
        }
    }
    rows
}

#[test]
fn dust_is_kicked_up_at_the_paws_and_stays_in_the_feet_row() {
    let rows = landing_dust_rows(false);
    assert!(!rows.is_empty(), "fixture: the landing threw its dust");
    for &(centre, top, bottom) in &rows {
        assert!(
            centre >= 0.5,
            "the puff sits on the ground at the paws: centre {centre:.2} rows into the feet row"
        );
        assert!(
            top >= 0.0 && bottom <= 1.0,
            "the puff stays in the feet row: rows {top:.2}..{bottom:.2}"
        );
    }
    let floated = landing_dust_rows(true);
    assert!(
        floated.iter().any(|&(centre, ..)| centre < 0.5),
        "control: dust born at the old seat floats at the cat's back"
    );
}

/// The farthest (cells) any live puff lay from the body's box, and how many frames drew one,
/// as a line typed along a row is deleted back: the body sprints back along it, and stumbles.
fn sprint_dust_reach(old: bool) -> (f32, u32) {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    pet.set_old_path(OldPath::DustOutlivesTheHeel, old);
    let (mut t, _) = type_run(&mut pet, start, 4, 2, 60, 0.05);
    let (mut far, mut frames) = (0.0f32, 0u32);
    let s = sense(t, None);
    let width = art_cols(s.cell_w, s.cell_h);
    let mut read = |f: &PetFrame| {
        for m in f.motes.iter().flatten() {
            if m.kind == PetMoteKind::Dust && m.alpha > 0 {
                frames += 1;
                far = far.max(if m.col < f.col {
                    f.col - m.col
                } else {
                    m.col - (f.col + width)
                });
            }
        }
    };
    // Deleted fast, two cells a frame's three: the stand runs off faster than a walk.
    for k in 0..120u16 {
        t += Duration::from_millis(16);
        let caret = (4u16, 62u16.saturating_sub(k).max(2));
        read(&pet.tick(sense(t, Some(caret))));
    }
    (far, frames)
}

/// THE PUFF KEEPS TO THE HEEL IT CAME FROM (`steam.t120`'s grey ball, far behind the running
/// cat): a sprint's stumble throws its dust, and no frame draws it past [`DUST_HEEL_FAR`] of
/// the body. Negative control: [`OldPath::DustOutlivesTheHeel`] reverted alone.
#[test]
fn a_puff_keeps_to_the_heel_it_came_from() {
    let (far, frames) = sprint_dust_reach(false);
    assert!(frames > 0, "fixture: the sprint threw its dust");
    assert!(
        far <= DUST_HEEL_FAR,
        "a puff lay {far:.2} cells behind the running body"
    );
    let (old_far, _) = sprint_dust_reach(true);
    assert!(
        old_far > 2.0 * DUST_HEEL_FAR,
        "control: reverted, the puff stays far behind the body: {old_far:.2} cells"
    );
}

#[test]
fn landing_dust_holds_the_train_for_a_bounded_beat_past_the_landing() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 4, 4);
    let caret = (4u16, 90u16);
    let bound = ((0.45 + 0.07 * 2.0 - LAND_DUR) / 0.016).ceil() as u32;
    let (mut alive, mut own, mut landed) = (0u32, 0u32, false);
    for _ in 0..300 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some(caret)));
        landed |= f.action == PetAction::Land;
        let any = f
            .motes
            .iter()
            .flatten()
            .any(|m| m.kind == PetMoteKind::Dust && m.alpha > 0);
        if any {
            alive += 1;
            assert!(pet.needs_frames(), "live dust is motion");
            if !needs_frames_without_motes(&mut pet) {
                own += 1;
            }
        }
    }
    assert!(landed, "fixture: the jump landed");
    assert!(alive >= 1, "fixture: the landing threw its dust");
    assert!(
        own <= bound,
        "the dust held the frame train {own} frames past the landing, bound {bound}"
    );
    assert!(
        alive <= (0.59 / 0.016) as u32 + 2,
        "the dust outlived its life: {alive} frames"
    );
}

#[test]
fn a_repeating_jump_must_not_pin_the_pet_in_its_double_take() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 4, 4);
    let start_col = pet.tick(sense(t, Some((4, 6)))).col;
    let mut caret = 6u16;
    let mut travelled = 0.0f32;
    let mut last = start_col;
    for _ in 0..60 {
        t += Duration::from_millis(16);
        caret = (caret + 8).min(95);
        let f = pet.tick(sense(t, Some((4, caret))));
        travelled += (f.col - last).abs();
        last = f.col;
    }
    assert!(
        travelled > 5.0,
        "the pet froze through a held jump: {travelled} cells of travel while \
         the caret ran to column {caret}"
    );
}

#[test]
fn a_held_word_jump_to_the_line_end_never_sends_the_cat_a_row_down() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 4, 4);
    let _ = pet.tick(sense(t, Some((4, 6))));
    let mut caret = 6u16;
    let mut lowest = 4.0f32;
    for _ in 0..150 {
        t += Duration::from_millis(16);
        caret = (caret + 8).min(95);
        let f = pet.tick(sense(t, Some((4, caret))));
        lowest = lowest.max(f.row);
    }
    assert!(
        lowest < 4.25,
        "the cat went a row below the caret's (row {lowest:.2}) for a wrap that never came"
    );
    let f = pet.tick(sense(t + Duration::from_millis(16), Some((4, 95))));
    assert!(
        (f.row - 4.0).abs() < 0.01 && f.col > 80.0,
        "fixture: the cat came to the caret on its row, at ({:.2}, {:.2})",
        f.col,
        f.row
    );
}

#[test]
fn a_blink_the_tail_covered_never_shows_its_last_present() {
    let mut pet = PetBrain::default();
    let mut q = SIT_AFTER;
    while pet
        .sit_blink_beat_at(q - SIT_AFTER)
        .is_none_or(|(p, _)| p > 1e-3)
    {
        q += 0.001;
        assert!(q < SIT_AFTER + 60.0, "fixture: the seat blinks");
    }
    let at = |pet: &mut PetBrain, into: f32, last: PetGlyphId| {
        pet.quiet = q + into;
        pet.last_pose = last;
        pet.sit_blink_shows().is_some()
    };
    assert!(
        at(&mut pet, 0.0, PetGlyphId::PetSit),
        "a blink from its start shows"
    );
    assert!(
        !at(&mut pet, BLINK_DUR - 0.008, PetGlyphId::PetSitFlick),
        "the last 8 ms of a blink the tail covered is not drawn"
    );
    assert!(
        at(&mut pet, BLINK_DUR - 0.008, PetGlyphId::PetSitBlink),
        "a blink on glass runs to its end"
    );
    assert!(
        at(
            &mut pet,
            BLINK_DUR - STILL_SECS - 0.002,
            PetGlyphId::PetSitFlick
        ),
        "a covered blink with two frames left still shows"
    );
}

/// A CAT SEATED UNDER THE WRITING STAYS SEATED. Under a composer's last line, a mid-line
/// insert's pushed tail can leave the stand beside the caret within
/// [`INK_EVICT_MAX`] of the station, its head among the letters of the line above. A cat
/// beside the writing keeps to its row there, veiled; a cat already seated on the footer
/// under the box keeps its seat rather than come up onto the words.
///
/// NEGATIVE CONTROL (as run before the fix): the stand on the caret's row, at
/// `(47.7, 11)`.
#[test]
fn a_cat_seated_under_the_writing_stays_seated() {
    use crate::pet_world::{PetPane, PetWorldFacts};
    use aterm_core::terminal::Terminal;

    let mut term = Terminal::new(30, 100);
    term.process(
        format!(
            "\x1b[11;1H{}\x1b[12;1H{}{}\x1b[13;1H{}\x1b[12;41H",
            "x".repeat(100),
            "y".repeat(40),
            "z".repeat(30),
            "\u{2500}".repeat(100)
        )
        .as_bytes(),
    );
    let mut pet = PetBrain::default();
    let input = term.cell_frame(30, 100);
    let facts = PetWorldFacts::read(&term, 7);
    pet.observe_console(&input, &facts, PetPane::full(&input));
    pet.set_console_presentable(true);
    let width = art_cols(10, 20);
    pet.row = 13.0;
    pet.col = 44.0;
    let (want, caret) = (44.0, (11u16, 40u16));
    let seat = pet.clear_of_letters_above((want + 3.7, 11.0), want, caret, (30, 100), width);
    assert!(
        seat.1 >= 12.5,
        "a cat seated under the writing keeps a seat under it, got {seat:?}"
    );
}

/// r29 K9-3, ONLY LETTERS THE CARET HAS PASSED ARE WRITING LAID UNDER THE CAT
/// ([`PetBrain::writing_laid_under`]). Over Claude Code's composer, the stream watch up and
/// the cat on the composer's line on letters: the dictation's letters behind the caret are
/// a program's writing going on there, and send it across the composer's bottom rule to the
/// footer ([`PetBrain::seat_off_the_writing`]); the placeholder its first screens paint
/// ahead of the caret is not. Read as writing, windowed as a session began, it sent the cat
/// to the footer and straight back to the caret.
#[test]
fn only_letters_behind_the_caret_are_writing_laid_under_the_cat() {
    use crate::pet_world::{PetPane, PetWorldFacts};
    use aterm_core::terminal::Terminal;

    let laid_under = |line: &str, caret_col: u16| -> (bool, Option<(f32, f32)>) {
        let rule = "\u{2500}".repeat(100);
        let mut term = Terminal::new(30, 100);
        term.process(
            format!(
                "\x1b[5;1H{rule}\x1b[6;1H> {line}\x1b[7;1H{rule}\x1b[8;1H  auto mode on\x1b[6;{}H",
                caret_col + 1
            )
            .as_bytes(),
        );
        let mut pet = PetBrain::default();
        let input = term.cell_frame(30, 100);
        let facts = PetWorldFacts::read(&term, 7);
        pet.observe_console(&input, &facts, PetPane::full(&input));
        pet.set_console_presentable(true);
        let end = 2 + line.chars().count() as u16;
        pet.sense_ink(5, &[(0, end)], None);
        pet.stream = true;
        let width = art_cols(10, 20);
        pet.row = 5.0;
        pet.col = 7.0;
        let caret = (5u16, caret_col);
        (
            pet.writing_laid_under(caret, width),
            pet.seat_off_the_writing(caret, (30, 100), width),
        )
    };
    let (written, seat) = laid_under("audit and remove the legacy work around ", 42);
    assert!(
        written && seat.is_some_and(|(_, r)| r > 6.5),
        "the dictation's letters behind the caret send the cat to the footer: {written}, \
         {seat:?}"
    );
    let (placeholder, _) = laid_under("Try \"refactor <filepath>\"", 2);
    assert!(
        !placeholder,
        "a placeholder ahead of the caret is no writing laid under the cat"
    );
}

/// A ROW HOP'S GATHER WHOSE ROW CAME TO IT STANDS UP. The caret goes down a row and
/// the cat gathers to hop after it; the host carries the gathering cat down that row
/// with the ground it stands on ([`PetBrain::carry_console_body`]: a composer the
/// session's first turn pushed down). Nothing is left to hop to: it stands up.
///
/// NEGATIVE CONTROL (as run before the fix): it hopped on the spot, a flight from row 6
/// to row 6.
#[test]
fn a_row_hops_gather_whose_row_came_to_it_stands_up() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 5, 20);
    assert!(pet.action.settled(), "fixture: at rest by the caret");
    let caret = (6u16, 22u16);
    let mut spins = 0;
    while !(pet.action == PetAction::Crouch && pet.hop_crouch) && spins < 60 {
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some(caret)));
        spins += 1;
    }
    assert!(
        pet.action == PetAction::Crouch && pet.hop_crouch,
        "fixture: the row hop gathers, got {:?}",
        pet.action
    );
    pet.row += 1.0;
    let mut flights = Vec::new();
    for _ in 0..40 {
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some(caret)));
        if let Some(f) = pet.flight
            && flights.is_empty()
        {
            flights.push((f.from_row, f.to_row));
        }
    }
    assert!(
        flights.is_empty(),
        "the gather's row came to it: no flight, got {flights:?}"
    );
    assert!(
        (pet.row - 6.0).abs() < 1e-3,
        "it stays on row 6, at {}",
        pet.row
    );
}

/// THE WAKE KEEPS ITS SEAT. A wake's stretch ends in the seat it is drawn in;
/// ended with the caret moved under [`SIT_AFTER`] ago, the ladder stood the cat
/// up on the next tick, and the seat was on glass a single frame. The cat keeps
/// the seat until the caret moves.
///
/// NEGATIVE CONTROL (as run before the fix): `[Sit, Stand, ...]`, the seat one
/// 16 ms frame.
#[test]
fn a_wake_keeps_the_seat_it_ends_in() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 5, 20);
    let caret = (5u16, 22u16);
    assert!(pet.action.settled(), "fixture: at rest by the caret");
    pet.set_action(PetAction::Waking);
    pet.action_t = WAKE_DUR - 0.005;
    pet.quiet = 0.5;
    let mut seen = Vec::new();
    for _ in 0..30 {
        t += Duration::from_millis(16);
        seen.push(pet.tick(sense(t, Some(caret))).action);
    }
    let seat = seen
        .iter()
        .skip_while(|a| **a == PetAction::Waking)
        .take_while(|a| **a == PetAction::Sit)
        .count();
    assert!(
        seat as f32 * 0.016 >= STILL_SECS,
        "the seat a wake ends in is held: {seen:?}"
    );
}

/// A LATE SEAT STARTS ITS BEATS FROM THE SEAT. A cat whose trip home outlasts
/// the quiet before a seat ([`SIT_AFTER`]) sits down at once, and the seat's
/// tail, blink and glance beats run from that moment, not from the quiet:
/// counted from the quiet, the cat sat down halfway through a beat, and the
/// tail's flick cut the seat's first pose after a frame (the long insert's
/// footer seat, landed 1.75 s after the caret's last move). Judged at every
/// landing 0 to 3 s past [`SIT_AFTER`]: the seat's first pose stays on glass
/// at least [`STILL_SECS`].
///
/// NEGATIVE CONTROL (as run before the fix): the first pose cut short after a
/// frame at 0.88 s and 1.38 s past [`SIT_AFTER`] (a tail's flick, a loaf).
#[test]
fn a_late_seat_starts_its_beats_from_the_seat() {
    let start = Instant::now();
    let mut cut = Vec::new();
    for k in 0..150u32 {
        let late = k as f32 * 0.02;
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 5, 20);
        let caret = (5u16, 22u16);
        pet.set_action(PetAction::Land);
        pet.land_t = 0.0;
        pet.quiet = SIT_AFTER + late;
        let mut poses = Vec::new();
        for _ in 0..40 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some(caret)));
            poses.push((f.action, f.pose));
        }
        let seated: Vec<_> = poses
            .iter()
            .skip_while(|(a, _)| *a == PetAction::Land)
            .collect();
        let Some(&&(_, first)) = seated.first() else {
            continue;
        };
        let held = seated.iter().take_while(|(_, p)| *p == first).count();
        if held < seated.len() && (held as f32) * 0.016 < STILL_SECS {
            cut.push((late, first, held));
        }
    }
    assert!(
        cut.is_empty(),
        "a late seat's first pose cut short (seconds past SIT_AFTER, pose, frames): {cut:?}"
    );
}

/// A PROMPT THAT MOVES AS THE CAT COMES DOWN DRAWS THE DESCENT ON. The prompt
/// goes two rows further down while the cat is already descending to it: the
/// landing goes on to the prompt's new row ([`ROW_REAIM_MAX_U`]), the rest of
/// the flight drawn out to keep inside the animal's speed ([`PetBrain::retime`]),
/// and its poses never go back from the descent ([`Flight::stage`]): one
/// flight, onto the prompt's row.
#[test]
fn a_prompt_that_moves_as_the_cat_comes_down_draws_the_descent_on() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 4, 20);
    let mut caret = (10u16, 22u16);
    let mut stages = Vec::new();
    let mut moved = false;
    let mut launches = 0;
    let mut last = None;
    let mut landed_row = None;
    for _ in 0..200 {
        t += Duration::from_millis(16);
        if !moved
            && pet
                .flight
                .is_some_and(|f| f.t / f.dur > 0.9 && f.stage == 4)
        {
            caret = (12, 22);
            moved = true;
        }
        let f = pet.tick(sense(t, Some(caret)));
        if f.action == PetAction::Leap {
            stages.push(PetBrain::flight_stage(f.pose));
            if last != Some(PetAction::Leap) {
                launches += 1;
            }
        } else if last == Some(PetAction::Leap) && landed_row.is_none() {
            landed_row = Some(f.row);
        }
        last = Some(f.action);
    }
    assert!(moved, "fixture: the prompt moved while the cat descended");
    assert_eq!(launches, 1, "one flight");
    assert!(
        stages.windows(2).all(|w| w[1] >= w[0]),
        "the poses never go back a stage: {stages:?}"
    );
    assert_eq!(landed_row, Some(12.0), "down on the prompt's new row");
}

/// A seat that comes into a tail beat with less than two frames of its pose left (the quiet
/// clock deals beats in flight too: a landing during a flick) does not begin it, where it
/// would show for one frame; a flick already on glass runs to its end.
#[test]
fn a_tail_flick_a_seat_comes_into_never_shows_its_last_present() {
    let mut pet = PetBrain::default();
    assert!(!pet.sit_front, "fixture: a side-on seat");
    let (period, at) = pet.tail_clock();
    let beat = (0..8i64)
        .find(|&b| pet.beat_deal(b, 4) == 0)
        .expect("fixture: a single-flick hand");
    let start = SIT_AFTER + beat as f32 * period + at;
    let at_q = |pet: &mut PetBrain, into: f32, last: PetGlyphId| {
        pet.quiet = start + into;
        pet.last_pose = last;
        pet.sit_tail()
    };
    assert_eq!(
        at_q(&mut pet, 0.002, PetGlyphId::PetSit),
        Some(PetGlyphId::PetSitFlick),
        "a flick from its start shows"
    );
    assert_eq!(
        at_q(&mut pet, TAIL_FLICK - 0.008, PetGlyphId::PetSit),
        None,
        "the last 8 ms of a flick the seat came into is not begun"
    );
    assert_eq!(
        at_q(&mut pet, TAIL_FLICK - 0.008, PetGlyphId::PetSitFlick),
        Some(PetGlyphId::PetSitFlick),
        "a flick on glass runs to its end"
    );
    assert_eq!(
        at_q(
            &mut pet,
            TAIL_FLICK - STILL_SECS - 0.002,
            PetGlyphId::PetSit
        ),
        Some(PetGlyphId::PetSitFlick),
        "a flick with two frames left still begins"
    );
}

/// THE SKID ENDS ON THE STAND. A bound aims short by its skid and slides
/// the rest of the way; a stand a key moved back toward the cat while it
/// was in the air is slid to, and never past. Slid the whole skid, the cat
/// came down past its stand and walked back to it (the
/// owner's replay: the bound down to Claude Code's composer as a letter
/// was typed into it).
#[test]
fn a_landing_never_slides_past_its_stand() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 5, 60);
    let (mut t, _) = idle(&mut pet, t, (5, 62), 1.0);
    // A jump home along the row: a bound to the left, and one key typed
    // while it is in the air.
    let mut caret = (5u16, 30u16);
    let mut keyed = false;
    let mut touchdown = None;
    let mut ground = Vec::new();
    for _ in 0..240 {
        t += Duration::from_millis(16);
        if !keyed && pet.flight.is_some_and(|f| f.big && f.t > 0.05) {
            keyed = true;
            caret.1 += 1;
        }
        let f = pet.tick(sense(t, Some(caret)));
        if touchdown.is_none() && f.action == PetAction::Land {
            touchdown = Some(f.col);
        }
        if touchdown.is_some() {
            ground.push(f.col);
        }
    }
    assert!(keyed, "fixture: the jump home is a bound");
    let touchdown = touchdown.expect("fixture: the bound lands");
    assert_eq!(pet.land_span, 0.0, "fixture: the landing has run out");
    let rest = *ground.last().expect("frames after the landing");
    let furthest = ground.iter().copied().fold(f32::INFINITY, f32::min);
    assert!(
        furthest >= touchdown.min(rest) - 0.05,
        "the landing slid to col {furthest:.2}, past both its touchdown ({touchdown:.2}) and \
         its stand ({rest:.2})"
    );
}

/// A JUMP IS NO WRITING. The wake's stretch yields to writing that runs on
/// along its row; a caret a program walks along the row by a jump does
/// not cut it. Cut, Claude Code's first screens put the stretch's hind
/// reach on glass for one present before the seat. A typed
/// cell still cuts it.
#[test]
fn a_jump_along_the_row_never_cuts_the_wake_short() {
    let woken = |second: u16| {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 20);
        let (mut t, f) = idle(&mut pet, t, (4, 22), SLEEP_AFTER + 1.0);
        assert_eq!(f.action, PetAction::Sleep, "fixture: asleep");
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((4, 23))));
        assert_eq!(pet.action, PetAction::Waking, "fixture: a cell wakes it");
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((4, 23 + second))));
        pet.action
    };
    assert_eq!(
        woken(POUNCE_JUMP as u16 + 2),
        PetAction::Waking,
        "a jump along the row is no writing: the stretch plays on"
    );
    assert_ne!(
        woken(1),
        PetAction::Waking,
        "control: a cell typed on cuts the stretch"
    );
}

/// A HOP NEVER HANDS BACK A STRETCH WITH NO TIME LEFT. The ink eviction's
/// hop borrows the pose it shoves the cat out of and gives it back on
/// landing with its clock; a wake's stretch with less than [`STILL_SECS`]
/// of it left came back for a single present between the landing and the
/// seat (Claude Code's first screens: the hop off the words
/// its repaint wrote under a waking cat).
#[test]
fn a_hop_never_hands_back_a_stretch_with_no_time_left() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 20);
    let (mut t, f) = idle(&mut pet, t, (4, 22), SLEEP_AFTER + 1.0);
    assert_eq!(f.action, PetAction::Sleep, "fixture: asleep");
    // A cell wakes it, and the stretch runs to its last frames.
    t += Duration::from_millis(16);
    let _ = pet.tick(sense(t, Some((4, 23))));
    while pet.action == PetAction::Waking && pet.action_t < WAKE_DUR - 0.03 {
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((4, 23))));
    }
    assert_eq!(pet.action, PetAction::Waking, "fixture: still stretching");
    // Its row is written over end to end: the eviction hops it off.
    let mut spans = vec![(0u16, 0u16); 8];
    spans[4] = (0, 100);
    pet.sense_ink(0, &spans, Some(4));
    let mut landed = false;
    let mut after = Vec::new();
    for _ in 0..90 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 23))));
        landed |= f.action == PetAction::Land;
        if landed {
            after.push(f.action);
        }
    }
    assert!(
        landed,
        "fixture: the eviction hops the waking cat off the words"
    );
    assert!(
        !after.contains(&PetAction::Waking),
        "the stretch's last frames are not handed back after the landing: {after:?}"
    );
}

/// THE TOUCHDOWN IS A POSE ON THE GROUND. A fright on a landing's first
/// present keeps the touchdown on glass for [`STILL_SECS`], as it keeps
/// any still pose just put on: a backspace's flinch drew the landing for
/// a single frame (the owner's long insert at 120 Hz).
#[test]
fn a_fright_never_takes_a_touchdown_off_after_one_present() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 20);
    let (mut t, _) = idle(&mut pet, t, (4, 22), 1.0);
    // A word jump: the pounce, to its landing.
    let mut f = pet.tick(sense(t, Some((4, 50))));
    for _ in 0..120 {
        if f.action == PetAction::Land {
            break;
        }
        t += Duration::from_millis(16);
        f = pet.tick(sense(t, Some((4, 50))));
    }
    assert_eq!(f.action, PetAction::Land, "fixture: the pounce lands");
    assert_eq!(f.pose, PetGlyphId::PetLand, "fixture: the touchdown");
    // A backspace on the next present: the flinch.
    t += Duration::from_millis(8);
    let f = pet.tick(sense(t, Some((4, 49))));
    assert_eq!(f.action, PetAction::Perk, "fixture: the flinch");
    assert_eq!(
        f.pose,
        PetGlyphId::PetLand,
        "the touchdown stays on glass through the flinch's first present"
    );
    t += Duration::from_secs_f32(STILL_SECS);
    let f = pet.tick(sense(t, Some((4, 49))));
    assert_ne!(
        f.pose,
        PetGlyphId::PetLand,
        "and the flinch is drawn after it"
    );
}

/// A CARET MOVE NEVER TAKES A SEAT'S NEW POSE OFF AT ONCE. The seat's own
/// ladder steps on its own clock; a caret move deals it again (the gaze
/// looks back, the tail's beat restarts), and the pose put on a present
/// before it stays on glass [`STILL_SECS`]. Dealt again at once, the look
/// down between a double tap's two flicks, at a caret rows under the
/// seat, was on glass a single present (8 ms at 120 Hz); so
/// is a face-on seat's blink when the caret moves a cell.
#[test]
fn a_caret_move_never_takes_a_seats_new_pose_off_at_once() {
    // The cat sits on the blank row under a caret whose row is written
    // to the wall, face on to it.
    let seated = |pet: &mut PetBrain| {
        let t = awake(pet, Instant::now(), 6, 20);
        let mut spans = vec![(0u16, 0u16); 10];
        spans[7] = (0, 100);
        pet.sense_ink(0, &spans, Some(7));
        (t, (7u16, 24u16))
    };
    // Where the face-on seat's blink first goes on, found on one cat…
    let mut pet = PetBrain::default();
    let (mut t, caret) = seated(&mut pet);
    let mut last = None;
    let mut blink_at = None;
    for i in 0..4_000u32 {
        t += Duration::from_millis(8);
        let f = pet.tick(sense(t, Some(caret)));
        if f.action == PetAction::Sit
            && f.pose == PetGlyphId::PetSitFrontBlink
            && last == Some(PetGlyphId::PetSitFront)
        {
            blink_at = Some(i);
            break;
        }
        last = Some(f.pose);
    }
    let blink_at = blink_at.expect("fixture: the face-on seat blinks");
    // …and on its twin the caret moves a cell the present after.
    let mut pet = PetBrain::default();
    let (mut t, caret) = seated(&mut pet);
    for _ in 0..=blink_at {
        t += Duration::from_millis(8);
        let _ = pet.tick(sense(t, Some(caret)));
    }
    t += Duration::from_millis(8);
    let f = pet.tick(sense(t, Some((caret.0, caret.1 + 1))));
    assert_eq!(
        f.pose,
        PetGlyphId::PetSitFrontBlink,
        "the blink stays on glass through the caret's move ({:?})",
        f.action
    );
}

/// WRITING THAT RUNS PAST A LANDING TOUCHES DOWN. A bound whose stand the hand's first
/// keys carried on ahead of it while it was in the air comes down on a touch and goes on
/// with the writing: planted for a heavy landing's [`BIG_LAND_DUR`] there, the cat was left
/// on the letters as the caret ran away, and chased them veiled (a prompt typed at the
/// bottom row as the session's first bound came down). Without the keys the bound lands
/// heavy, as it always has.
#[test]
fn writing_that_runs_past_a_landing_touches_down() {
    let landing = |keys: bool| {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 5, 10);
        let (mut t, _) = idle(&mut pet, t, (5, 12), 1.0);
        // The caret jumps far along the row: a bound. Two keys are typed at it while the
        // cat is in the air, too few for the writing's rhythm.
        let mut caret = (5u16, 60u16);
        let mut typed = 0;
        for _ in 0..240 {
            t += Duration::from_millis(16);
            if keys
                && typed < 2
                && pet
                    .flight
                    .is_some_and(|f| f.big && f.t > 0.08 * (typed + 1) as f32)
            {
                typed += 1;
                caret.1 += 1;
                pet.note_console_input(t, PetInputKind::Text);
            }
            let f = pet.tick(sense(t, Some(caret)));
            if f.action == PetAction::Land {
                return (typed, pet.land_span, pet.run_count);
            }
        }
        panic!("fixture: the bound lands");
    };
    let (typed, span, run) = landing(true);
    assert_eq!(typed, 2, "fixture: two keys typed in the air");
    assert!(
        run < RHYTHM_MOVES,
        "fixture: the writing's rhythm is not open yet ({run} moves)"
    );
    assert_eq!(span, 0.0, "the bound touches down: no heavy landing's skid");
    let (_, span, _) = landing(false);
    assert!(span > 0.0, "fixture: the bound with no keys lands heavy");
}

/// A FLIGHT GOES ON TO THE CARET'S NEW ROW. A caret that changes rows while the cat is in
/// the air takes the landing with it, rebased so the drawn arc never jumps: kept on the
/// row it left for, the flight came down where the caret no longer was and the cat
/// leapt again from there.
#[test]
fn a_flight_goes_on_to_the_row_the_caret_moved_to() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 5, 10);
    let (mut t, _) = idle(&mut pet, t, (5, 12), 1.0);
    // The caret jumps four rows down and along: a bound. Early in the air it moves two
    // rows further down.
    let mut caret = (9u16, 40u16);
    let mut moved = false;
    let mut path = Vec::new();
    for _ in 0..240 {
        t += Duration::from_millis(8);
        if !moved && pet.flight.is_some_and(|f| f.t / f.dur > 0.3) {
            moved = true;
            caret.0 = 11;
        }
        let f = pet.tick(sense(t, Some(caret)));
        if pet.flight.is_some() {
            path.push((f.col, f.row));
        }
        if moved && f.action == PetAction::Land {
            assert!(
                (f.row - 11.0).abs() < 0.5,
                "the flight came down on row {} with the caret on row 11",
                f.row
            );
            // Rebased, the drawn path is continuous: no step of it is a jump.
            for w in path.windows(2) {
                let step = span_eq(w[1].0 - w[0].0, w[1].1 - w[0].1);
                assert!(
                    step < 1.5,
                    "the arc jumps {step:.2} cells in a frame: {w:?}"
                );
            }
            return;
        }
    }
    panic!("fixture: the bound lands after the caret moved (moved={moved})");
}

#[test]
fn a_caret_that_hides_and_returns_elsewhere_is_a_new_sighting_not_a_retreat() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 5, 78);
    for _ in 0..60 {
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, None));
    }
    t += Duration::from_millis(16);
    let f = pet.tick(sense(t, Some((5, 3))));
    assert_ne!(
        f.action,
        PetAction::Startle,
        "reappearing elsewhere is not a backspace"
    );
    let w = art_cols(10, 20);
    assert!(
        (f.col - PetBrain::station(3, 100, w)).abs() < 0.01,
        "and it re-materialises at the new station, got col {}",
        f.col
    );
}

#[test]
fn the_first_tick_of_a_fade_in_keeps_the_frame_lane() {
    let mut pet = PetBrain::default();
    let f = pet.tick(sense(Instant::now(), Some((5, 10))));
    assert_eq!(f.alpha, 0, "the ramp has not moved on the seeding tick");
    assert!(
        pet.needs_frames(),
        "but the pet is arriving, so it still needs frames"
    );
}

#[test]
fn the_first_windowless_still_materializes_the_resident_pet() {
    let mut pet = PetBrain::default();
    let f = pet.tick_static_capture(sense(Instant::now(), Some((5, 10))));
    assert_eq!(f.alpha, 255, "a requested still contains the resident pet");
    assert!(pet.is_active(), "status agrees with the captured pixels");
    assert!(
        (f.col - PetBrain::station(10, 100, art_cols(10, 20))).abs() < 0.01,
        "the static capture uses the normal first-sighting station"
    );
}

#[test]
fn the_leap_arc_leaves_the_ground_and_lands_back_on_a_baseline() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 4, 10);
    let mut peak = 0.0f32;
    let mut last = 0.0f32;
    for _ in 0..160 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 20))));
        peak = peak.max(f.lift);
        last = f.lift;
        if f.action == PetAction::Land {
            break;
        }
    }
    assert!(
        peak > 0.3,
        "the arc must actually leave the floor, got {peak}"
    );
    assert!(
        last.abs() < 1e-3,
        "and the landing must be exactly on the baseline, got {last}"
    );
}

#[test]
fn a_single_backspace_flinches_without_the_full_bottle() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let (mut t, _) = type_run(&mut pet, start, 2, 10, 8, 0.06);
    t += Duration::from_millis(16);
    let f = pet.tick(sense(t, Some((2, 17))));
    assert_ne!(
        f.action,
        PetAction::Startle,
        "one backspace must not fire the whole bottle"
    );
    assert_eq!(f.action, PetAction::Perk, "the flinch is a perk-and-duck");
    assert_eq!(f.pose, PetGlyphId::PetPerk);
    assert!(
        f.scale_y < 1.0 && f.scale_x > 1.0,
        "and the duck must be visible in the envelope: y {} x {}",
        f.scale_y,
        f.scale_x
    );
}

#[test]
fn a_big_retreat_still_bottles() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let (t, _) = type_run(&mut pet, start, 2, 10, 8, 0.06);
    let (mut t, _) = idle(&mut pet, t, (2, 18), 0.5);
    t += Duration::from_millis(16);
    let f = pet.tick(sense(t, Some((2, 13))));
    assert_eq!(f.action, PetAction::Startle);
    assert_eq!(f.pose, PetGlyphId::PetStartle);
    assert!(
        f.facing_left,
        "in open grid the caret retreats to the pet's left"
    );
}

#[test]
fn startle_faces_the_caret_even_at_the_wall() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 5, 93);
    assert!(
        pet.col < 90.0,
        "fixture: the pet must be parked left of the caret, col {}",
        pet.col
    );
    t += Duration::from_millis(16);
    let f = pet.tick(sense(t, Some((5, 90))));
    assert_eq!(f.action, PetAction::Startle);
    assert!(
        !f.facing_left,
        "the fright must face the caret, not a compass point"
    );
}

#[test]
fn a_held_backspace_is_one_sustained_fright_not_a_strobe() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let (mut t, _) = type_run(&mut pet, start, 2, 10, 10, 0.05);
    let mut col = 19u16;
    let mut episodes = 0;
    let mut was = PetAction::Walk;
    for _ in 0..8 {
        t += Duration::from_millis(40);
        col -= 1;
        let f = pet.tick(sense(t, Some((2, col))));
        if f.action == PetAction::Startle && was != PetAction::Startle {
            episodes += 1;
        }
        was = f.action;
    }
    assert_eq!(episodes, 1, "one burst, one fright — got {episodes}");
}

#[test]
fn fussing_back_and_forth_makes_it_frolic() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 6, 20);
    let cols = [23u16, 22, 23, 22, 23];
    let mut frolicked = false;
    for c in cols {
        for _ in 0..6 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((6, c))));
            if f.action == PetAction::Frolic {
                frolicked = true;
            }
        }
    }
    assert!(frolicked, "three reversals inside the window is play");
}

#[test]
fn quiet_settles_then_sleeps_and_typing_wakes_it_with_a_stretch() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 18);
    let (t, f) = idle(&mut pet, t, (4, 20), 0.4);
    assert!(
        matches!(f.action, PetAction::Sit | PetAction::Purr),
        "a short quiet sits, got {:?}",
        f.action
    );
    let (mut t, f) = idle(&mut pet, t, (4, 20), SLEEP_AFTER);
    assert_eq!(f.action, PetAction::Sleep, "a long quiet sleeps");
    t += Duration::from_millis(16);
    let f = pet.tick(sense(t, Some((4, 21))));
    assert_eq!(f.action, PetAction::Waking);
    assert_eq!(f.pose, PetGlyphId::PetStretch);
}

#[test]
fn a_good_run_earns_a_purr_and_a_cold_start_does_not() {
    let start = Instant::now();
    let mut worked = PetBrain::default();
    let t = awake(&mut worked, start, 3, 2);
    let (t, _) = type_run(&mut worked, t, 3, 4, 40, 0.05);
    let (_, f) = idle(&mut worked, t, (3, 44), SIT_AFTER + 0.2);
    assert_eq!(f.action, PetAction::Sit, "the seat still comes first");
    assert!(f.purr > 0.0, "a sentence of running earns the purr at once");

    let mut idle_cat = PetBrain::default();
    let t = awake(&mut idle_cat, start, 3, 40);
    let (_, g) = idle(&mut idle_cat, t, (3, 42), 0.6);
    assert_eq!(g.action, PetAction::Sit, "sitting down cold does not");
    assert_eq!(g.purr, 0.0);
}

#[test]
fn the_pet_never_leaves_the_viewport() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = start;
    let w = art_cols(10, 20);
    for col in [0u16, 99, 0, 99, 50] {
        for _ in 0..40 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((0, col))));
            assert!(f.col >= 0.0, "left edge escaped: {}", f.col);
            assert!(f.col + w <= 100.0 + 1e-3, "right edge escaped: {}", f.col);
            assert!(f.row >= 0.0 && f.row <= 29.0, "row escaped: {}", f.row);
        }
    }
}

#[test]
fn a_hidden_caret_fades_the_pet_out_and_the_fingerprint_settles_to_zero() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let (mut t, mut last) = idle(&mut pet, start, (4, 20), 1.0);
    assert!(last.alpha > 0, "fixture: on glass");
    for _ in 0..80 {
        t += Duration::from_millis(16);
        last = pet.tick(sense(t, None));
    }
    assert_eq!(last.alpha, 0, "a hidden caret grounds the pet");
    assert_eq!(last.fp(), 0, "and the repaint key settles");
    assert!(!pet.is_active());
}

#[test]
fn reduced_motion_pins_the_pet_at_its_station_with_no_arc_or_gait() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = start;
    let w = art_cols(10, 20);
    let mut fps = std::collections::BTreeSet::new();
    for col in [10u16, 60, 12] {
        for _ in 0..20 {
            t += Duration::from_millis(16);
            let mut s = sense(t, Some((3, col)));
            s.reduced_motion = true;
            let f = pet.tick(s);
            assert_eq!(f.lift, 0.0, "no arcs under reduced motion");
            assert_eq!(f.scale_x, 1.0);
            assert_eq!(f.scale_y, 1.0);
            assert!(matches!(f.action, PetAction::Sit | PetAction::Sleep));
            assert!((f.col - PetBrain::station(col, 100, w)).abs() < 1e-3);
            assert_eq!(f.alpha, 255, "a static pet appears in one sample");
            assert_eq!(
                pet.home, pet.col,
                "the static placement is its completed home"
            );
            assert_eq!(pet.col_at_tick, pet.col, "no unfinished glide is recorded");
            assert!(
                !pet.needs_frames(),
                "a jump or idle still owes no animation"
            );
            fps.insert(f.fp());
        }
    }
    assert!(
        fps.len() <= 3,
        "a reduced-motion pet must SETTLE (one still frame per caret cell), \
         not animate: {} distinct frames",
        fps.len()
    );

    t += Duration::from_millis(16);
    let _ = pet.tick(sense(t, Some((3, 90))));
    assert!(pet.needs_frames(), "re-enabled motion remains owed");
    t += Duration::from_millis(16);
    let mut reduced = sense(t, Some((3, 90)));
    reduced.reduced_motion = true;
    assert_eq!(pet.tick(reduced).alpha, 255);
    assert!(
        !pet.needs_frames(),
        "reducing a moving brain settles its debt"
    );
}

/// A run's window ages on every tick, a reduced-motion one too ([`PetBrain::advance`]): a
/// run Reduce Motion stops while its lead carries the pet closes, and asks for no frames once
/// it has. Aged only on a moving tick, its window stood still and the carrying run asked for
/// frames for good.
#[test]
fn a_run_reduce_motion_stops_closes_its_window() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 10);
    let (mut t, _) = type_run(&mut pet, t, 4, 12, 8, 0.16);
    assert!(
        pet.carrying_run(),
        "fixture: the run's lead carries the pet"
    );
    let mut asked = 0;
    for _ in 0..120 {
        t += Duration::from_millis(16);
        let mut s = sense(t, Some((4, 20)));
        s.reduced_motion = true;
        let _ = pet.tick(s);
        asked += usize::from(pet.needs_frames());
    }
    assert!(
        pet.run_t > RHYTHM_WINDOW && !pet.carrying_run(),
        "the window closes under reduced motion: run_t {}",
        pet.run_t
    );
    assert!(
        !pet.needs_frames(),
        "and asks for no frames ({asked} of 120 did)"
    );
}

#[test]
fn a_suspended_host_clock_cannot_teleport_the_pet() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let (_, f0) = idle(&mut pet, start, (4, 10), 1.0);
    let f1 = pet.tick(sense(start + Duration::from_secs(3600), Some((4, 90))));
    let jump = (f1.col - f0.col).abs();
    assert!(
        jump < 5.0,
        "a clamped dt must bound the per-frame step, moved {jump} cells"
    );
}

#[test]
fn the_chase_converges_without_oscillating_around_the_station() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = start;
    let w = art_cols(10, 20);
    let target = PetBrain::station(60, 100, w);
    let mut crossings = 0;
    let mut side = 0i8;
    for _ in 0..200 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((3, 60))));
        let s = if f.col > target + 0.02 {
            1
        } else if f.col < target - 0.02 {
            -1
        } else {
            0
        };
        if s != 0 && side != 0 && s != side {
            crossings += 1;
        }
        if s != 0 {
            side = s;
        }
    }
    assert!(
        crossings <= 1,
        "a follower must not ring: {crossings} crossings"
    );
}

#[test]
fn facing_follows_travel_with_hysteresis() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = start;
    let mut f = pet.tick(sense(t, Some((3, 10))));
    for _ in 0..140 {
        t += Duration::from_millis(16);
        f = pet.tick(sense(t, Some((3, 80))));
    }
    assert!(!f.facing_left, "chasing right, facing right");
    for _ in 0..80 {
        t += Duration::from_millis(16);
        f = pet.tick(sense(t, Some((3, 5))));
    }
    assert!(f.facing_left, "chasing left, facing left");
}

#[test]
fn a_mid_appearance_look_sync_parks_until_the_pet_has_faded_out() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 5, 10);
    assert!(pet.is_active(), "the fixture needs a visible pet");
    assert_eq!(
        pet.sync_look((3, 1), PetArrival::Ceremony).worn,
        (3, 1),
        "the first dress applies"
    );
    assert_eq!(
        pet.sync_look((9, 4), PetArrival::Ceremony).worn,
        (3, 1),
        "a repoint mid-appearance keeps the worn coat"
    );
    for _ in 0..120 {
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, None));
    }
    assert!(!pet.is_active(), "the fade-out must complete");
    for _ in 0..4 {
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((5, 10))));
    }
    assert!(pet.is_active(), "the next appearance is under way");
    assert_eq!(
        pet.sync_look((9, 4), PetArrival::Ceremony).worn,
        (9, 4),
        "the parked pair landed at zero alpha, so the new appearance wears it"
    );
}

#[test]
fn resident_palette_is_owned_by_one_pet_appearance() {
    let dark = CatColorKey {
        accent: 1,
        background: 0,
    };
    let light = CatColorKey {
        accent: 7,
        background: 3,
    };
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 5, 10);
    assert_eq!(pet.appearance_colors(), None);
    assert_eq!(pet.colors_for_appearance(dark), dark);
    assert_eq!(
        pet.colors_for_appearance(light),
        dark,
        "walking frames keep one stable atlas palette"
    );

    pet.invalidate_colors();
    assert_eq!(
        pet.colors_for_appearance(light),
        light,
        "a host context edge resamples without resetting the brain"
    );
    for _ in 0..120 {
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, None));
    }
    assert!(!pet.is_active(), "the fixture completes its fade-out");
    assert_eq!(
        pet.appearance_colors(),
        None,
        "zero alpha retires the old appearance palette"
    );

    for _ in 0..4 {
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((5, 10))));
    }
    assert!(pet.is_active());
    assert_eq!(pet.colors_for_appearance(dark), dark);
}

#[test]
fn stand_is_emitted_between_arrival_and_sit() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 10);
    let (t, _) = type_run(&mut pet, t, 4, 12, 3, 0.06);
    let (t, f) = idle(&mut pet, t, (4, 15), 1.0);
    assert_eq!(f.action, PetAction::Stand, "arrived + quiet < SIT_AFTER");
    assert_eq!(f.pose, PetGlyphId::PetStand, "the shipped art, on glass");
    let (_, g) = idle(&mut pet, t, (4, 15), SIT_AFTER);
    assert!(
        matches!(g.action, PetAction::Sit | PetAction::Purr),
        "the stand folds into the sit, got {:?}",
        g.action
    );
}

#[test]
fn horizontal_flight_stretches_along_the_motion_axis() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 4, 10);
    let mut stretched = false;
    for _ in 0..140 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 30))));
        if f.action == PetAction::Leap {
            assert!(
                f.scale_x >= f.scale_y,
                "a horizontal leap must never go tall: x {} y {}",
                f.scale_x,
                f.scale_y
            );
            if f.scale_x > 1.02 {
                stretched = true;
                assert!(f.scale_y < 1.0, "the stretch squashes the height");
            }
        }
    }
    assert!(stretched, "the leap must actually stretch along its travel");
}

#[test]
fn the_gallop_cycle_stays_below_nyquist_and_paces_by_stride_cells() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 3, 2);
    let mut caret = 4u16;
    let mut first: Option<(f32, f32)> = None;
    let mut last = (0.0f32, 0.0f32);
    for i in 0..240 {
        t += Duration::from_millis(16);
        if i % 3 == 2 {
            caret = (caret + 1).min(95);
        }
        let f = pet.tick(sense(t, Some((3, caret))));
        assert_ne!(
            f.action,
            PetAction::Leap,
            "fixture: this cadence must run the pet, not launch it"
        );
        if f.action == PetAction::Run {
            if let Some((_, prev_s)) = first {
                let ds = (pet.stride - last.1).rem_euclid(1024.0);
                assert!(
                    ds <= 0.5,
                    "gallop advanced {ds} cycles in one 16ms frame (> half frame rate)"
                );
                let _ = prev_s;
            } else {
                first = Some((f.col, pet.stride));
            }
            last = (f.col, pet.stride);
        }
    }
    let (c0, s0) = first.expect("a 20 cells/s chase must reach a gallop");
    let cells = (last.0 - c0).abs();
    let cycles = last.1 - s0;
    assert!(cells > 10.0, "need a sustained gallop to measure: {cells}");
    let eff = cells / cycles;
    assert!(
        (eff - RUN_STRIDE_CELLS).abs() < 0.05,
        "the gallop must pace itself by RUN_STRIDE_CELLS ({RUN_STRIDE_CELLS}), measured \
         {eff} cells per cycle"
    );
}

fn gait_run(gap: f32, keys: u16) -> Vec<(PetAction, PetGlyphId, f32)> {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 3, 5);
    let mut out = Vec::new();
    for k in 0..keys {
        let until = t + Duration::from_secs_f32(gap);
        while t < until {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((3, 7 + k))));
            out.push((f.action, f.pose, pet.stride));
        }
        t = until;
        let f = pet.tick(sense(t, Some((3, 8 + k))));
        out.push((f.action, f.pose, pet.stride));
    }
    out
}

const fn cycle_beat(pose: PetGlyphId) -> Option<(bool, usize)> {
    let mut i = 0;
    while i < PetBrain::CYCLE_LWALK.len() {
        if PetBrain::CYCLE_LWALK[i] as u8 == pose as u8 {
            return Some((true, i));
        }
        i += 1;
    }
    let mut j = 0;
    while j < PetBrain::CYCLE_WALK.len() {
        if PetBrain::CYCLE_WALK[j] as u8 == pose as u8 {
            return Some((false, j));
        }
        j += 1;
    }
    None
}

#[test]
fn a_slow_caret_walks_laterally_and_a_brisk_one_trots() {
    let slow = gait_run(0.45, 12);
    let slow_walk: Vec<PetGlyphId> = slow
        .iter()
        .filter(|(a, _, _)| *a == PetAction::Walk)
        .map(|(_, p, _)| *p)
        .collect();
    assert!(
        slow_walk.len() > 40,
        "fixture: a pecking caret must walk the cat ({} frames)",
        slow_walk.len()
    );
    assert!(
        slow_walk.iter().all(|p| PetBrain::CYCLE_LWALK.contains(p)),
        "a pecked caret is ambled after, never trotted after: {:?}",
        slow_walk
            .iter()
            .filter(|p| !PetBrain::CYCLE_LWALK.contains(p))
            .collect::<Vec<_>>()
    );
    assert!(
        slow.iter().all(|(a, _, _)| *a != PetAction::Run),
        "fixture: and never runs"
    );

    let brisk = gait_run(0.30, 12);
    let brisk_walk: Vec<PetGlyphId> = brisk
        .iter()
        .filter(|(a, _, _)| *a == PetAction::Walk)
        .map(|(_, p, _)| *p)
        .collect();
    assert!(
        brisk_walk.iter().any(|p| PetBrain::CYCLE_WALK.contains(p)),
        "a prose cadence earns the trot"
    );

    let mut prev: Option<usize> = None;
    for (a, pose, _) in &brisk {
        if *a != PetAction::Walk {
            prev = None;
            continue;
        }
        let (_, idx) = cycle_beat(*pose).expect("a walk frame is a walk pose");
        if let Some(p) = prev {
            let step = (idx + 4 - p) % 4;
            assert!(
                step <= 1,
                "the gait skipped from beat {p} to {idx} across the flip"
            );
        }
        prev = Some(idx);
    }
}

#[test]
fn a_steady_rhythm_walks_the_cat_on_without_a_stop_at_each_key() {
    for gap in [0.05f32, 0.07, 0.10, 0.14, 0.18, 0.22, 0.28, 0.35, 0.45] {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 3, 5);
        let (mut stands, mut changes, mut slowest) = (0usize, 0usize, f32::MAX);
        let mut last = None;
        for k in 0..24u16 {
            let until = t + Duration::from_secs_f32(gap);
            while t < until {
                t += Duration::from_millis(16);
                let f = pet.tick(sense(t, Some((3, 7 + k))));
                if k >= 6 {
                    stands += usize::from(f.action == PetAction::Stand);
                    changes += usize::from(last.is_some_and(|a| a != f.action));
                    slowest = slowest.min(pet.speed.abs());
                }
                last = Some(f.action);
            }
            t = until;
            let _ = pet.tick(sense(t, Some((3, 8 + k))));
        }
        assert!(
            stands == 0 && changes <= 1 && slowest > 1.0,
            "a {gap} s rhythm walks the cat on: {stands} standing frames, \
             {changes} gait changes, slowest {slowest:.2} cells/s"
        );
    }
}

#[test]
fn a_late_key_lets_the_cat_coast_on_not_stop_dead() {
    for late in [0.15f32, 0.2, 0.3, 0.45] {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 3, 5);
        let (mut stands, mut shed) = (0usize, 0.0f32);
        let mut was = None::<f32>;
        let mut stood: Vec<f32> = Vec::new();
        let mut standing: Option<Instant> = None;
        for k in 0..20u16 {
            let gap = if k == 12 { late } else { 0.1 };
            let until = t + Duration::from_secs_f32(gap);
            while t < until {
                t += Duration::from_millis(16);
                let f = pet.tick(sense(t, Some((3, 7 + k))));
                if k >= 6 {
                    let stand = f.pose == PetGlyphId::PetStand;
                    stands += usize::from(stand);
                    match (stand, standing) {
                        (true, None) => standing = Some(t),
                        (false, Some(from)) => {
                            stood.push((t - from).as_secs_f32());
                            standing = None;
                        }
                        _ => {}
                    }
                    if let Some(v) = was {
                        shed = shed.max(v - pet.speed.abs());
                    }
                }
                was = Some(pet.speed.abs());
            }
            t = until;
            let _ = pet.tick(sense(t, Some((3, 8 + k))));
            was = Some(pet.speed.abs());
        }
        if late <= 0.15 {
            assert_eq!(
                stands, 0,
                "a key {late} s after the last keeps the cat running"
            );
        }
        assert!(
            stood.iter().all(|s| *s >= STAND_MIN),
            "a {late} s gap: a stand under a stride ({STAND_MIN} s): {stood:?}"
        );
        assert!(
            shed < 4.0,
            "a {late} s gap slows the cat, never stops it dead: {shed:.2} cells/s lost in a frame"
        );
    }
}

#[test]
fn a_leg_after_the_caret_was_gone_picks_its_gait_afresh() {
    let hands: [&[f32]; 2] = [
        &[0.12, 0.12, 0.12, 0.3, 0.08, 0.08, 0.08, 0.08, 0.08],
        &[0.15, 0.15, 0.15, 0.15, 0.15, 0.07, 0.07, 0.07, 0.07, 0.07],
    ];
    let family = |pose: PetGlyphId| -> Option<u8> {
        if PetBrain::CYCLE_LWALK.contains(&pose) {
            Some(0)
        } else if PetBrain::CYCLE_WALK.contains(&pose) {
            Some(1)
        } else if PetBrain::CYCLE_RUN.contains(&pose) {
            Some(2)
        } else {
            None
        }
    };
    for (n, gaps) in hands.iter().enumerate() {
        for back in [0.0f32, 0.05] {
            let start = Instant::now();
            let mut pet = PetBrain::default();
            let (t, _) = type_run(&mut pet, start, 3, 5, 4, 0.4);
            let (mut t, _) = idle(&mut pet, t, (3, 9), SIT_AFTER + 0.4);
            for _ in 0..16 {
                t += Duration::from_millis(16);
                let _ = pet.tick(sense(t, None));
            }
            let (mut t, _) = idle(&mut pet, t, (3, 9), back);
            let mut worn: Vec<u8> = Vec::new();
            for (key, gap) in (1u16..).zip(gaps.iter()) {
                let until = t + Duration::from_secs_f32(*gap);
                while t < until {
                    t += Duration::from_millis(16);
                    let f = pet.tick(sense(t, Some((3, 9 + key))));
                    if let Some(gait) = family(f.pose)
                        && worn.last() != Some(&gait)
                    {
                        assert!(
                            !worn.contains(&gait),
                            "hand {n}, {back} s after the caret came back: back to a gait \
                             it left (0 amble, 1 walk, 2 run): {worn:?} then {gait}"
                        );
                        worn.push(gait);
                    }
                }
            }
            assert_eq!(
                worn.first(),
                Some(&0),
                "hand {n}, {back} s after: an amble's pace sets out in the amble, picked \
                 afresh ({worn:?})"
            );
        }
    }
}

fn uneven_gaps(seed: u64, n: usize) -> Vec<f32> {
    let mut state = seed;
    (0..n)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (60 + (state >> 33) % 61) as f32 / 1000.0
        })
        .collect()
}

#[test]
fn a_hands_uneven_keys_never_stop_the_cat_mid_stride() {
    for seed in [3u64, 5, 9] {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 3, 5);
        let (mut stands, mut slow, mut slowest) = (0usize, 0usize, f32::MAX);
        for (k, gap) in (0u16..).zip(uneven_gaps(seed, 40)) {
            let until = t + Duration::from_secs_f32(gap);
            while t + Duration::from_millis(16) < until {
                t += Duration::from_millis(16);
                let f = pet.tick(sense(t, Some((3, 7 + k))));
                if k >= 6 {
                    stands += usize::from(f.action == PetAction::Stand);
                    slow += usize::from(pet.speed.abs() < 1.0);
                    slowest = slowest.min(pet.speed.abs());
                }
            }
            t = until;
            let _ = pet.tick(sense(t, Some((3, 8 + k))));
        }
        assert!(
            stands == 0 && slow == 0,
            "seed {seed}: uneven keys walk the cat on: {stands} standing frames, \
             {slow} frames under a cell a second (slowest {slowest:.2} cells/s)"
        );
    }
}

#[test]
fn where_the_cat_rests_does_not_turn_on_the_hosts_cadence() {
    fn rest(gaps: &[f32], paced: bool) -> f32 {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 3, 5);
        let mut caret = 7u16;
        let mut keys = gaps.iter().scan(t, |at, g| {
            *at += Duration::from_secs_f32(*g);
            Some(*at)
        });
        let mut next_key = keys.next();
        let stop = t + Duration::from_secs_f32(gaps.iter().sum::<f32>() + 3.0);
        loop {
            let frame = Some(t + Duration::from_millis(16));
            let tick = if !paced || pet.needs_frames() {
                frame
            } else {
                pet.next_change_deadline(t)
                    .map(|d| d.max(t + Duration::from_millis(1)))
            };
            t = match (tick, next_key) {
                (Some(a), Some(k)) if a < k => a,
                (_, Some(k)) => {
                    caret += 1;
                    next_key = keys.next();
                    k
                }
                (Some(a), None) if a <= stop => a,
                _ => break,
            };
            let _ = pet.tick(sense(t, Some((3, caret))));
        }
        pet.col
    }
    let mut late = vec![0.12f32; 30];
    late[15] = 0.5;
    for (name, gaps) in [("a 0.2 s rhythm", vec![0.2f32; 30]), ("a late key", late)] {
        let (flat, paced) = (rest(&gaps, false), rest(&gaps, true));
        assert!(
            (flat - paced).abs() < 0.05,
            "{name}: the cat rests at {flat:.3} ticked every frame, {paced:.3} as the GUI paces it"
        );
    }
}

#[test]
fn a_stand_the_ink_ladder_chose_is_walked_with_not_run_into() {
    fn ink(pet: &mut PetBrain, caret: u16) {
        let mut spans = vec![(0u16, 0u16); 5];
        spans[2] = (0, 100);
        spans[3] = (0, caret + 20);
        spans[4] = (0, 100);
        pet.sense_ink(0, &spans, Some(3));
    }
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 3, 5);
    let c0 = 7u16;
    let settled = t + Duration::from_secs(3);
    while t < settled {
        t += Duration::from_millis(16);
        ink(&mut pet, c0);
        let _ = pet.tick(sense(t, Some((3, c0))));
    }
    let (mut stops, mut slowest) = (0usize, f32::MAX);
    for k in 0..24u16 {
        let until = t + Duration::from_secs_f32(0.1);
        while t < until {
            t += Duration::from_millis(16);
            ink(&mut pet, c0 + k);
            let _ = pet.tick(sense(t, Some((3, c0 + k))));
            if k >= 6 {
                stops += usize::from(pet.speed.abs() < 1.0);
                slowest = slowest.min(pet.speed.abs());
            }
        }
        t = until;
        ink(&mut pet, c0 + k + 1);
        let _ = pet.tick(sense(t, Some((3, c0 + k + 1))));
    }
    let last = pet.tick(sense(t + Duration::from_millis(16), Some((3, c0 + 24))));
    assert!(
        last.col >= f32::from(c0 + 24) + 10.0,
        "the cat follows the line's far end, not the caret: at {:.2} for caret {}",
        last.col,
        c0 + 24
    );
    assert_eq!(
        stops, 0,
        "a cat walking with the pushed line never stops on a step (slowest {slowest:.2} cells/s)"
    );
}

#[test]
fn a_typing_rhythm_grows_the_lead_past_the_base_station() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 3, 5);
    let (_, f) = type_run(&mut pet, t, 3, 7, 40, 0.05);
    assert!(
        f.col >= 47.0 + STATION_LEAD + 0.5,
        "a galloping caret must be LED, not chased: pet at {} for caret 47",
        f.col
    );
}

#[test]
fn hunt_and_peck_never_sees_the_lead() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 3, 5);
    let (t, _) = type_run(&mut pet, t, 3, 7, 6, 1.0);
    let (_, f) = idle(&mut pet, t, (3, 13), 0.8);
    assert!(
        (f.col - (13.0 + STATION_LEAD)).abs() <= 0.3,
        "one key a second is not a rhythm: pet at {} for caret 13",
        f.col
    );
}

#[test]
fn alternating_moves_never_lead() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 3, 20);
    let mut max_col = 0.0f32;
    for k in 0..24u16 {
        let c = if k % 2 == 0 { 23 } else { 22 };
        for _ in 0..8 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((3, c))));
            max_col = max_col.max(f.col);
        }
    }
    assert!(
        max_col <= 23.0 + STATION_LEAD + 0.6,
        "an edited caret must not be led: pet reached {max_col}"
    );
}

#[test]
fn a_pause_leaves_the_cat_where_it_stopped() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 3, 5);
    let (mut t, mut f) = type_run(&mut pet, t, 3, 7, 30, 0.05);
    let (mut last, mut back, mut turned) = (f.col, 0.0f32, false);
    for _ in 0..(2.5 / 0.016) as usize {
        t += Duration::from_millis(16);
        f = pet.tick(sense(t, Some((3, 37))));
        back += (last - f.col).max(0.0);
        turned |= f.facing_left;
        last = f.col;
    }
    assert!(back < 0.01, "no step back on a pause: {back} cells back");
    assert!(!turned, "and no turn");
    assert!(
        f.col >= 37.0 + STATION_LEAD + 1.0 && f.col <= 37.0 + STATION_LEAD + LEAD_MAX,
        "it stays out ahead, where it stopped: pet at {} for caret 37",
        f.col
    );
    assert!(
        f.action.settled(),
        "and the pet settles, got {:?}",
        f.action
    );
}

#[test]
fn the_writing_catches_up_with_a_cat_that_ran_ahead() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 3, 5);
    let (t, _) = type_run(&mut pet, t, 3, 7, 30, 0.05);
    let (t, f) = idle(&mut pet, t, (3, 37), 2.0);
    let seat = f.col;
    assert!(
        seat >= 37.0 + STATION_LEAD + 1.0,
        "fixture: out ahead ({seat})"
    );
    let (t, f) = idle(&mut pet, t, (3, 38), 1.0);
    assert!(
        (f.col - seat).abs() < 0.01,
        "a key short of the cat leaves it seated: {} from {seat}",
        f.col
    );
    let caret = (seat - STATION_LEAD).ceil() as u16 + 1;
    let (t, f) = type_run(&mut pet, t, 3, 38, caret - 38, 1.0);
    let (t, f2) = idle(&mut pet, t, (3, caret), 1.0);
    assert!(
        f2.col > seat && (f2.col - (f32::from(caret) + STATION_LEAD)).abs() <= ARRIVED,
        "caught up, it walks on beside the caret: {} ({:?}) for caret {caret}",
        f2.col,
        f.action
    );
    let (_, f) = idle(&mut pet, t, (4, 2), 2.0);
    assert!(
        (f.col - (2.0 + STATION_LEAD)).abs() <= ARRIVED,
        "a new line starts afresh: pet at {}",
        f.col
    );
}

#[test]
fn a_pounce_mid_rhythm_lands_at_the_predicted_station() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 10);
    let (mut t, _) = type_run(&mut pet, t, 4, 12, 20, 0.05);
    let mut caret = 40u16;
    let mut landed: Option<(f32, u16)> = None;
    let mut next_key = t + Duration::from_millis(50);
    for _ in 0..120 {
        t += Duration::from_millis(16);
        if t >= next_key {
            next_key += Duration::from_millis(50);
            caret = (caret + 1).min(95);
        }
        let f = pet.tick(sense(t, Some((4, caret))));
        if f.action == PetAction::Land && landed.is_none() {
            landed = Some((f.col, caret));
        }
    }
    let (col, caret_at_land) = landed.expect("the word jump must pounce");
    assert!(
        col > f32::from(caret_at_land),
        "a predicted aim lands AHEAD of the still-moving caret: landed \
         at {col} while the caret was at {caret_at_land}"
    );
}

#[test]
fn wall_hysteresis_does_not_flip_flop_at_the_crossing() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 5, 92);
    let mut left_frames = 0u32;
    let mut right_frames = 0u32;
    for k in 0..6u16 {
        let c = if k % 2 == 0 { 93 } else { 94 };
        for _ in 0..90 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((5, c))));
            if f.col < f32::from(c) {
                left_frames += 1;
            } else {
                right_frames += 1;
            }
        }
    }
    assert!(left_frames > 400, "the pet must be parked wall-side");
    assert!(
        right_frames == 0,
        "and STAY there through the alternation: {right_frames} frames \
         back on the right of the caret"
    );
}

#[test]
fn the_wall_crossing_transits_with_a_small_arc() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 5, 86);
    let mut airborne = false;
    let mut f = pet.tick(sense(t, Some((5, 91))));
    for _ in 0..90 {
        t += Duration::from_millis(16);
        f = pet.tick(sense(t, Some((5, 94))));
        if f.action == PetAction::Leap && f.lift > 0.0 {
            airborne = true;
        }
    }
    assert!(airborne, "the side flip must arc over the caret");
    assert!(
        f.col < 94.0 - 5.0,
        "and end parked clear on the caret's left, got {}",
        f.col
    );
}

#[test]
fn a_regrid_carries_the_body_with_its_line() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let tall = |t, c| PetSense {
        rows: 40,
        ..sense(t, Some(c))
    };
    let short = |t, c| PetSense {
        rows: 30,
        ..sense(t, Some(c))
    };
    let mut t = start;
    let _ = pet.tick(tall(t, (39, 10)));
    for k in 0..40u16 {
        for _ in 0..6 {
            t += Duration::from_millis(16);
            let _ = pet.tick(tall(t, (39, 10 + k)));
        }
    }
    let f = pet.tick(tall(t, (39, 50)));
    assert!(
        (f.row - 39.0).abs() < 0.01 && f.action == PetAction::Run,
        "fixture: running beside the caret on the last row, got {:?} at row {}",
        f.action,
        f.row
    );
    pet.rebase_geometry();
    let mut actions = Vec::new();
    for i in 0..30u16 {
        t += Duration::from_millis(16);
        let f = pet.tick(short(t, (26, 50 + i / 6)));
        if i == 0 {
            assert!(
                (f.row - 26.0).abs() < 0.01,
                "the body goes with its line, got row {}",
                f.row
            );
        }
        actions.push(f.action);
    }
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, PetAction::Crouch | PetAction::Leap)),
        "no hop back to the line: {actions:?}"
    );
}

#[test]
fn a_regrid_keeps_the_stand_beside_the_caret() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let at = |t, rows, c| PetSense {
        rows,
        ..sense(t, Some(c))
    };
    let mut t = start;
    let _ = pet.tick(at(t, 40, (39, 10)));
    for k in 0..30u16 {
        for _ in 0..6 {
            t += Duration::from_millis(16);
            let _ = pet.tick(at(t, 40, (39, 10 + k)));
        }
    }
    for _ in 0..120 {
        t += Duration::from_millis(16);
        let _ = pet.tick(at(t, 40, (39, 39)));
    }
    let rest = pet.tick(at(t, 40, (39, 39)));
    assert!(
        rest.action.settled() && rest.col > 40.0,
        "fixture: at rest ahead of the caret, got {:?} at col {}",
        rest.action,
        rest.col
    );
    pet.rebase_geometry();
    let mut moved = Vec::new();
    for _ in 0..90 {
        t += Duration::from_millis(16);
        let f = pet.tick(at(t, 30, (26, 39)));
        if (f.col - rest.col).abs() > 0.05 || (f.row - 26.0).abs() > 0.01 {
            moved.push((f.col, f.row, f.action));
        }
    }
    assert!(
        moved.is_empty(),
        "the cat stays where it stood, at col {}: {} frames moved, {:?}",
        rest.col,
        moved.len(),
        &moved[..moved.len().min(4)]
    );
}

/// What a brain is doing as the cells change under it ([`rest_then_regrid`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AtRegrid {
    /// Awake at rest beside the caret.
    Awake,
    /// Asleep there.
    Asleep,
    /// Purring under a stroke ([`PetBrain::note_petted`]): a hold its tick returns from
    /// before the walk.
    Purring,
    /// Crouched in hiding where it rested ([`PetBrain::begin_hide`]): another.
    Hiding,
}

/// A cell's `(width, height)` in px.
type CellPx = (u16, u16);

/// A brain brought to `state` beside the caret on `caret` of a 30-row grid `cols.0` wide on
/// `cells.0` px cells, then (`regrid`) re-gridded to `cells.1` px cells and `cols.1` columns
/// with the caret on `after`, as a host's change of display scale does
/// ([`PetBrain::rebase_geometry`]): the last frame before the re-grid and the frames of the
/// 6.4 s after it, `step` apart, each held to the no-teleport law ([`lawful_tick`]).
fn rest_then_regrid(
    (cells, cols): ((CellPx, CellPx), (u16, u16)),
    (caret, after): ((u16, u16), (u16, u16)),
    (state, step): (AtRegrid, Duration),
    regrid: bool,
) -> (PetFrame, Vec<PetFrame>) {
    let at = |t, c: (u16, u16), px: (u16, u16), cols| PetSense {
        cell_w: px.0,
        cell_h: px.1,
        cols,
        ..sense(t, Some(c))
    };
    let mut pet = PetBrain::default();
    let mut t = Instant::now();
    let _ = pet.tick(at(t, (caret.0, caret.1 - 2), cells.0, cols.0));
    t += Duration::from_millis(50);
    let _ = pet.tick(at(t, (caret.0, caret.1 - 1), cells.0, cols.0));
    let end = t + Duration::from_secs_f32(if state == AtRegrid::Asleep {
        SLEEP_AFTER + 6.0
    } else {
        SIT_AFTER + 3.0
    });
    let mut before = pet.tick(at(t, caret, cells.0, cols.0));
    while t < end {
        t += step;
        before = pet.tick(at(t, caret, cells.0, cols.0));
    }
    let held = match state {
        AtRegrid::Awake | AtRegrid::Asleep => 0,
        AtRegrid::Purring => {
            pet.note_petted(t);
            5
        }
        AtRegrid::Hiding => {
            pet.begin_hide();
            20
        }
    };
    for _ in 0..held {
        t += step;
        before = pet.tick(at(t, caret, cells.0, cols.0));
    }
    if regrid {
        pet.rebase_geometry();
    }
    let mut last = (before.col, before.row, cols.0, 30);
    let n = Duration::from_millis(6400).as_nanos() / step.as_nanos();
    let frames = (0..n)
        .map(|_| {
            t += step;
            lawful_tick(&mut pet, at(t, after, cells.1, cols.1), &mut last)
        })
        .collect();
    (before, frames)
}

/// THE RE-GRID A CHANGE OF DISPLAY SCALE MAKES MOVES NO BODY OFF ITS STATION
/// ([`PetBrain::keep_place_on_restation`]): the window dragged between a 1x and a 2x display,
/// 7x14 px cells to 15x28 and back (and 7x15 to 15x29, a larger change of aspect), the caret
/// on its cell. The art is 5.61 columns wide on 7x14 px cells and 5.23 on 15x28
/// ([`art_cols`]), so the wall side's station (the body's right edge a cell short of the
/// caret) moves by 0.37 columns. A body on either side of the caret, awake, asleep, purring or
/// crouched in hiding there, keeps its pose and alpha frame for frame with the same brain given
/// no re-grid, and its place: the station's anchor stays within a pixel of where it stood on
/// the frame that draws the new cells, and from there the body is as still as that brain,
/// within the pixel it lands its rest on ([`PetBrain::glide_px_left`]). So too where the grid's
/// columns change with the cells: dragged from the 2x display to the 1x one, the window keeps
/// its size in points, and its 100 columns of 15 px become 107 of 7. At 60 and 120 frames a
/// second, each frame within the no-teleport law, whose re-grid give is the art's width
/// change. Before, the resting body glided the 0.37 columns over the next 120 frames, and a
/// body in a hold stood still through it and then jumped them in one frame. A re-grid that
/// moves the caret's cell leaves the body to the locomotion: it is not carried on that frame.
#[test]
fn a_scale_change_keeps_a_resting_body_on_its_station() {
    const ONE: (u16, u16) = (7, 14);
    const TWO: (u16, u16) = (15, 28);
    let states = [
        AtRegrid::Awake,
        AtRegrid::Asleep,
        AtRegrid::Purring,
        AtRegrid::Hiding,
    ];
    for (from, to, cols) in [
        (ONE, TWO, (100u16, 100u16)),
        (TWO, ONE, (100, 100)),
        ((7, 15), (15, 29), (100, 100)),
        (TWO, ONE, (100, 107)),
    ] {
        let (w0, w1) = (art_cols(from.0, from.1), art_cols(to.0, to.1));
        let px = 1.0 / f32::from(to.0);
        for step in [Duration::from_millis(16), Duration::from_millis(8)] {
            for (caret, wall) in [((10u16, 40u16), false), ((10, 96), true)] {
                for state in states {
                    let what = format!(
                        "{from:?} on {} columns to {to:?} on {}, caret {caret:?}, {state:?}, \
                         {step:?} frames",
                        cols.0, cols.1
                    );
                    let (before, scaled) =
                        rest_then_regrid(((from, to), cols), (caret, caret), (state, step), true);
                    let (_, control) = rest_then_regrid(
                        ((from, from), (cols.0, cols.0)),
                        (caret, caret),
                        (state, step),
                        false,
                    );
                    let posed = match state {
                        AtRegrid::Awake => {
                            before.action.settled() && before.action != PetAction::Sleep
                        }
                        AtRegrid::Asleep => before.action == PetAction::Sleep,
                        AtRegrid::Purring => before.action == PetAction::Purr,
                        AtRegrid::Hiding => before.action == PetAction::Crouch,
                    };
                    assert!(
                        posed && before.alpha == 255 && before.lift == 0.0,
                        "fixture: {what}: at rest on glass, got {:?} alpha {}",
                        before.action,
                        before.alpha
                    );
                    let (anchor_was, anchor_now) = if wall {
                        (before.col + w0, scaled[0].col + w1)
                    } else {
                        (before.col, scaled[0].col)
                    };
                    assert!(
                        (anchor_now - anchor_was).abs() < px,
                        "{what}: the station's anchor moved {:.3} columns at the re-grid",
                        anchor_now - anchor_was
                    );
                    let offset = scaled[0].col - control[0].col;
                    for (i, (s, c)) in scaled.iter().zip(&control).enumerate() {
                        assert!(
                            s.action == c.action
                                && s.pose == c.pose
                                && s.alpha == c.alpha
                                && s.row == c.row
                                && s.lift == c.lift
                                && s.facing_left == c.facing_left,
                            "{what}: frame {i} re-posed: {:?} {:?} alpha {} row {} lift {} \
                             against {:?} {:?} alpha {} row {} lift {}",
                            s.action,
                            s.pose,
                            s.alpha,
                            s.row,
                            s.lift,
                            c.action,
                            c.pose,
                            c.alpha,
                            c.row,
                            c.lift
                        );
                        assert!(
                            (s.col - c.col - offset).abs() < px,
                            "{what}: frame {i} moved the body: col {:.4} against {:.4} with \
                             no re-grid, {:.4} apart where the re-grid's frame stood \
                             {offset:.4}",
                            s.col,
                            c.col,
                            s.col - c.col
                        );
                    }
                }
            }
        }
        let (before, frames) = rest_then_regrid(
            ((from, to), cols),
            ((10, 96), (10, 95)),
            (AtRegrid::Asleep, Duration::from_millis(16)),
            true,
        );
        assert!(
            (frames[0].col - before.col).abs() < 0.05,
            "{from:?} to {to:?} with the caret moved to (10, 95): a re-grid that moves the \
             caret is the locomotion's, not carried, got col {} from {}",
            frames[0].col,
            before.col
        );
    }
}

/// A BODY OFF GLASS AS THE CELLS CHANGE IS SEEN AGAIN ON ITS NEW STATION, AND STAYS THERE: the
/// ticks with the caret gone measure no station and keep the last one, the old cells' wall
/// side, but the tick that sees the caret again places the body by the first sighting before
/// its aim, on the station it measures on the new cells, so the body has moved that tick and
/// the re-grid's carry ([`PetBrain::keep_place_on_restation`]) leaves it. Carried as well, the
/// body came back a whole width change past its new station and glided back over 300 frames,
/// a sleeping cat's way turned from the caret.
#[test]
fn a_body_off_glass_through_a_scale_change_is_seen_on_its_new_station() {
    const ONE: (u16, u16) = (7, 14);
    const TWO: (u16, u16) = (15, 28);
    let at = |t, c, px: (u16, u16)| PetSense {
        cell_w: px.0,
        cell_h: px.1,
        ..sense(t, c)
    };
    for (from, to) in [(ONE, TWO), (TWO, ONE)] {
        for caret in [(10u16, 40u16), (10, 96)] {
            let mut pet = PetBrain::default();
            let mut t = Instant::now();
            let _ = pet.tick(at(t, Some((caret.0, caret.1 - 2)), from));
            t += Duration::from_millis(50);
            let end = t + Duration::from_secs_f32(SIT_AFTER + 3.0);
            while t < end {
                t += Duration::from_millis(16);
                let _ = pet.tick(at(t, Some(caret), from));
            }
            for _ in 0..125 {
                t += Duration::from_millis(16);
                let _ = pet.tick(at(t, None, from));
            }
            assert!(pet.alpha == 0.0, "fixture: off glass, alpha {}", pet.alpha);
            pet.rebase_geometry();
            let frames: Vec<PetFrame> = (0..300)
                .map(|_| {
                    t += Duration::from_millis(16);
                    pet.tick(at(t, Some(caret), to))
                })
                .collect();
            let station = PetBrain::station(caret.1, 100, art_cols(to.0, to.1));
            let first = frames
                .iter()
                .position(|f| f.alpha > 0)
                .expect("fixture: seen again");
            for (i, f) in frames.iter().enumerate().skip(first) {
                assert!(
                    (f.col - station).abs() < 1e-4,
                    "{from:?} to {to:?}, caret {caret:?}: frame {i} (alpha {}) stands at col \
                     {:.4}, not on the new station {station:.4}",
                    f.alpha,
                    f.col
                );
            }
        }
    }
}

/// A FLIGHT THROUGH A SCALE CHANGE COMES DOWN ON THE NEW STATION
/// ([`PetBrain::keep_place_on_restation`]): a bound from a caret's old stand to the wall side
/// of one near the grid's right edge, its cells changed 1, 3 or 10 frames after take-off. The
/// landing is re-aimed by the station's move (0.37 columns) and rebased so the drawn body
/// does not move, every frame within the no-teleport law, and once settled the cat is on its
/// new station, within a pixel, and stays. Kept on the old station, it came down 0.37 columns
/// short and glided them over 199 frames. From a stand far enough away the leap splits in two
/// ([`PetBrain::bound2`]), its cells changed in the first bound or on the touch-down between
/// the bounds: the second landing moves with the station, and the cat settles on the new one.
/// Left on the old station, the second bound came down short of it as the single one did
/// (moved only in the air, 1x to 2x on the touch-down).
#[test]
fn a_flight_through_a_scale_change_comes_down_on_its_new_station() {
    const ONE: (u16, u16) = (7, 14);
    const TWO: (u16, u16) = (15, 28);
    let at = |t, c: (u16, u16), px: (u16, u16)| PetSense {
        cell_w: px.0,
        cell_h: px.1,
        ..sense(t, Some(c))
    };
    for (from, to) in [(ONE, TWO), (TWO, ONE)] {
        // `lag` frames after take-off, in the air, or (`None`) on the touch-down between a
        // split leap's bounds.
        for ((stand, split), lag) in [(40u16, false), (12, true)]
            .into_iter()
            .flat_map(|s| [Some(1), Some(3), Some(10)].map(|lag| (s, lag)))
            .chain([((12, true), None)])
        {
            let mut pet = PetBrain::default();
            let mut t = Instant::now();
            let _ = pet.tick(at(t, (10, stand - 2), from));
            t += Duration::from_millis(50);
            let end = t + Duration::from_secs_f32(SIT_AFTER + 3.0);
            while t < end {
                t += Duration::from_millis(16);
                let _ = pet.tick(at(t, (10, stand), from));
            }
            let mut took_off = false;
            for _ in 0..120 {
                t += Duration::from_millis(16);
                let _ = pet.tick(at(t, (10, 96), from));
                if pet.flight.is_some() {
                    took_off = true;
                    break;
                }
            }
            assert!(took_off, "fixture: the cat bounds for the far caret");
            if let Some(lag) = lag {
                for _ in 0..lag {
                    t += Duration::from_millis(16);
                    let _ = pet.tick(at(t, (10, 96), from));
                }
                assert!(pet.flight.is_some(), "fixture: in the air at the re-grid");
            } else {
                let mut touched = false;
                for _ in 0..240 {
                    t += Duration::from_millis(16);
                    let _ = pet.tick(at(t, (10, 96), from));
                    if pet.flight.is_none() && pet.bound2.is_some() {
                        touched = true;
                        break;
                    }
                }
                assert!(
                    touched,
                    "fixture: on the touch-down between the bounds at the re-grid"
                );
            }
            assert_eq!(
                pet.bound2.is_some(),
                split,
                "fixture: from col {stand}, a leap in two bounds: {split}"
            );
            pet.rebase_geometry();
            let mut last = (pet.col, pet.row, 100, 30);
            let frames: Vec<PetFrame> = (0..600)
                .map(|_| {
                    t += Duration::from_millis(16);
                    lawful_tick(&mut pet, at(t, (10, 96), to), &mut last)
                })
                .collect();
            let station = PetBrain::station(96, 100, art_cols(to.0, to.1));
            let px = 1.0 / f32::from(to.0);
            let settled = frames
                .iter()
                .position(|f| f.lift == 0.0 && f.action.settled())
                .expect("fixture: the cat comes down and settles");
            for (i, f) in frames.iter().enumerate().skip(settled) {
                assert!(
                    (f.col - station).abs() < px,
                    "{from:?} to {to:?} from col {stand}, re-gridded {lag:?} frames after \
                     take-off (None: on the touch-down): frame {i} ({:?}) stands at col \
                     {:.4}, {:.3} columns off the new station {station:.4}",
                    f.action,
                    f.col,
                    f.col - station
                );
            }
        }
    }
}

/// A CARET GONE JUST BEFORE A SCALE CHANGE LEAVES A RESTING BODY ON ITS STATION
/// ([`PetBrain::keep_place_on_restation`]): frames with no caret in the pet's space. The ticks
/// with no caret measure no station and keep the last one, so the tick that draws the new
/// cells and sees the caret again carries the body: the station's anchor stays within a
/// pixel, and from there the body is as still as the same brain given no re-grid. Measured
/// against the tick before only, the caretless tick dropped the measure and the body glided
/// the width change (0.374 columns at the wall side) over the next 120 frames. Where a tick
/// with no caret draws the new cells, the body is drawn on them where it stood and the
/// locomotion takes it to its station, every frame within the no-teleport law: carried when
/// the caret came back, it jumped the change on cells that had not changed, 0.59 columns from
/// 7x15 px cells to 15x29, past the law's bound.
#[test]
fn a_caret_gone_through_a_scale_change_keeps_a_resting_body_on_its_station() {
    const ONE: (u16, u16) = (7, 14);
    const TWO: (u16, u16) = (15, 28);
    let at = |t, c, px: (u16, u16)| PetSense {
        cell_w: px.0,
        cell_h: px.1,
        ..sense(t, c)
    };
    for (from, to) in [(ONE, TWO), (TWO, ONE), ((7, 15), (15, 29))] {
        let (w0, w1) = (art_cols(from.0, from.1), art_cols(to.0, to.1));
        let px = 1.0 / f32::from(to.0);
        for (caret, wall) in [((10u16, 40u16), false), ((10, 96), true)] {
            // `hidden` ticks with no caret: on the old cells, the re-grid's tick seeing it
            // again (`before`), or from the re-grid's tick on.
            for (hidden, before) in [(1u32, true), (3, true), (1, false), (3, false)] {
                let what = format!(
                    "{from:?} to {to:?}, caret {caret:?}, {hidden} tick(s) with no caret {}",
                    if before {
                        "before the re-grid"
                    } else {
                        "from the re-grid"
                    }
                );
                let t0 = Instant::now();
                let run = |regrid: bool| {
                    let cells = if regrid { to } else { from };
                    let mut pet = PetBrain::default();
                    let mut t = t0;
                    let _ = pet.tick(at(t, Some((caret.0, caret.1 - 2)), from));
                    t += Duration::from_millis(50);
                    let end = t + Duration::from_secs_f32(SIT_AFTER + 3.0);
                    let mut rest = pet.tick(at(t, Some(caret), from));
                    while t < end {
                        t += Duration::from_millis(16);
                        rest = pet.tick(at(t, Some(caret), from));
                    }
                    if before {
                        for _ in 0..hidden {
                            t += Duration::from_millis(16);
                            let _ = pet.tick(at(t, None, from));
                        }
                    }
                    if regrid {
                        pet.rebase_geometry();
                    }
                    let mut last = (pet.col, pet.row, 100, 30);
                    let frames: Vec<PetFrame> = (0..400u32)
                        .map(|i| {
                            t += Duration::from_millis(16);
                            let seen = (before || i >= hidden).then_some(caret);
                            lawful_tick(&mut pet, at(t, seen, cells), &mut last)
                        })
                        .collect();
                    (rest, frames)
                };
                let (rest, scaled) = run(true);
                let (_, control) = run(false);
                assert!(
                    rest.action.settled() && rest.alpha == 255 && rest.lift == 0.0,
                    "fixture: {what}: at rest on glass, got {:?} alpha {}",
                    rest.action,
                    rest.alpha
                );
                // Where the station's anchor puts the body on the new cells.
                let on_station = if wall { rest.col + w0 - w1 } else { rest.col };
                if before {
                    assert!(
                        (scaled[0].col - on_station).abs() < px,
                        "{what}: the station's anchor moved {:.3} columns at the re-grid",
                        scaled[0].col - on_station
                    );
                    let offset = scaled[0].col - control[0].col;
                    for (i, (s, c)) in scaled.iter().zip(&control).enumerate() {
                        assert!(
                            (s.col - c.col - offset).abs() < px,
                            "{what}: frame {i} moved the body: col {:.4} against {:.4} with \
                             no re-grid, {:.4} apart where the re-grid's frame stood \
                             {offset:.4}",
                            s.col,
                            c.col,
                            s.col - c.col
                        );
                    }
                } else {
                    let end = scaled.last().expect("frames");
                    assert!(
                        (end.col - on_station).abs() < px,
                        "{what}: the body ends at col {:.4}, {:.3} columns off its station",
                        end.col,
                        end.col - on_station
                    );
                }
            }
        }
    }
}

/// WHAT IS KEPT AT A RESTING BODY MOVES WITH IT WHEN A SCALE CHANGE CARRIES IT
/// ([`PetBrain::keep_place_on_restation`], [`PetBrain::carry_resting_body`]): where the last
/// tick left it, its home (left behind, a purr or a hide that returns before the walk keeps
/// a home a width change off the body: [`PetBrain::needs_frames`] reads it), the seat a held
/// look was adopted on (left behind, the look lets go at [`ADOPT_HOLD_REACH`], 0.05 columns,
/// and the body re-poses on the frame that draws the new cells), and a departure drawn still
/// beside it. A departure on its way is its own and is not moved.
#[test]
fn a_scale_change_carries_what_is_kept_at_a_resting_body() {
    const ONE: (u16, u16) = (7, 14);
    const TWO: (u16, u16) = (15, 28);
    let (w0, w1) = (art_cols(ONE.0, ONE.1), art_cols(TWO.0, TWO.1));
    let at = |t, px: (u16, u16)| PetSense {
        cell_w: px.0,
        cell_h: px.1,
        ..sense(t, Some((10, 96)))
    };
    let mut pet = PetBrain::default();
    let mut t = Instant::now();
    let _ = pet.tick(at(t, ONE));
    t += Duration::from_millis(50);
    let end = t + Duration::from_secs_f32(SIT_AFTER + 3.0);
    while t < end {
        t += Duration::from_millis(16);
        let _ = pet.tick(at(t, ONE));
    }
    let seen = pet.station_seen;
    let (col, row, left) = (pet.col, pet.row, pet.col_at_tick);
    // The wall side's station on the new cells, the width's change from the one seen.
    let station1 = PetBrain::station(96, 100, w1);
    let dx = seen.map_or(0.0, |s| station1 - s.at.0);
    assert!(
        pet.action.settled()
            && pet.speed == 0.0
            && seen.is_some_and(|s| s.width == w0 && (col - s.at.0).abs() < 1.0 / 7.0)
            && (dx.abs() - (w1 - w0).abs()).abs() < 1e-4,
        "fixture: at rest on the wall side's station, got {:?} at {col} seen {seen:?}, the \
         station's move {dx}",
        pet.action
    );
    pet.home = col - 0.25;
    pet.adopted_look = Some(AdoptedLook {
        pose: PetGlyphId::PetSit,
        facing_left: false,
        stance: Stance::NEUTRAL,
        eased: 0.5,
        seat: (col, row),
        beneath: None,
    });
    let born = pet.lane_clock;
    let departure = |still| Departure {
        born,
        id: 0,
        coat: 0,
        iris: 0,
        from_col: col - 7.0,
        row,
        edge: col - 1.0,
        speed: if still { 0.0 } else { DEPART_SPEED },
        still,
        facing_left: true,
        stride0: 0.0,
        pose0: PetGlyphId::PetSit,
        stance0: Stance::NEUTRAL,
        resight: false,
        own_glass: None,
        own_clear: false,
        peak: 1.0,
        colors: None,
        surface: None,
    };
    pet.departures[0] = Some(departure(true));
    pet.departures[1] = Some(departure(false));
    let entry = TickEntry {
        at: (col, row),
        landing: None,
        bound2: None,
    };
    pet.keep_place_on_restation((seen, entry, w0), (station1, row), (10, 96), (w1, 100));
    assert!(
        (pet.col - (col + dx)).abs() < 1e-4 && (pet.col_at_tick - (left + dx)).abs() < 1e-4,
        "the body and where the last tick left it move with the station, {dx} columns, got \
         {} and {} from {col}",
        pet.col,
        pet.col_at_tick
    );
    assert!(
        (pet.home - (col - 0.25 + dx)).abs() < 1e-4,
        "home keeps its offset from the body, got {} for {}",
        pet.home,
        col - 0.25 + dx
    );
    assert_eq!(
        pet.adopted_look.map(|l| l.seat),
        Some((col + dx, row)),
        "the adopted look's seat goes with the body"
    );
    let moved = |i: usize| pet.departures[i].map(|d| (d.from_col, d.edge));
    assert_eq!(
        moved(0),
        Some((col - 7.0 + dx, col - 1.0 + dx)),
        "a departure drawn still beside the body goes with it"
    );
    assert_eq!(
        moved(1),
        Some((col - 7.0, col - 1.0)),
        "a departure on its way is not moved"
    );
}

/// WHAT THE RE-GRID'S OWN TICK SETS OFF IS NOT CARRIED ([`PetBrain::keep_place_on_restation`]):
/// a leap set off on the tick that draws the new cells, before its aim (an eviction's), was
/// aimed on those cells, so its landing and its second bound's are left where they are. The
/// same leap in the air as the tick began comes down on the moved station (its landing, or
/// a split leap's second landing, moved by the station's move). Re-aimed on the tick that set
/// it off, the landing came down a width change past the new station.
#[test]
fn a_scale_change_carries_only_a_leap_in_the_air_as_its_tick_began() {
    let (w0, w1) = (art_cols(7, 14), art_cols(15, 28));
    let (s0, s1) = (
        PetBrain::station(96, 100, w0),
        PetBrain::station(96, 100, w1),
    );
    let seen = Some(SeenStation {
        at: (s0, 6.0),
        width: w0,
        caret: (6, 96),
    });
    for (stand, split) in [(40u16, false), (4, true)] {
        for in_air in [true, false] {
            let mut pet = PetBrain::default();
            let _ = awake(&mut pet, Instant::now(), 6, stand);
            pet.launch_big(s0, 6.0, 100, w0, true);
            let landing = pet.flight.map(|f| (f.to_col, f.to_row));
            let second = pet.bound2.map(|(c, r, _)| (c, r));
            assert!(
                landing.is_some() && second.is_some() == split,
                "fixture: from col {stand}, a leap ({landing:?}), in two bounds: {split} \
                 ({second:?})"
            );
            let entry = TickEntry {
                at: (pet.col, pet.row),
                landing: if in_air { landing } else { None },
                bound2: if in_air { second } else { None },
            };
            pet.keep_place_on_restation((seen, entry, w0), (s1, 6.0), (6, 96), (w1, 100));
            let what = format!(
                "from col {stand}, {}",
                if in_air {
                    "in the air as the tick began"
                } else {
                    "set off this tick"
                }
            );
            let now = (
                pet.flight.map(|f| (f.to_col, f.to_row)),
                pet.bound2.map(|(c, r, _)| (c, r)),
            );
            // A split leap's first bound comes down mid-field, beyond ARRIVED of the
            // station: only its second landing is the station's.
            let moved = |at: Option<(f32, f32)>| at.map(|(c, r)| (c + s1 - s0, r));
            let want = match (in_air, split) {
                (false, _) => (landing, second),
                (true, false) => (moved(landing), None),
                (true, true) => (landing, moved(second)),
            };
            let close = |a: Option<(f32, f32)>, b: Option<(f32, f32)>| match (a, b) {
                (Some(a), Some(b)) => (a.0 - b.0).abs() < 1e-4 && (a.1 - b.1).abs() < 1e-4,
                (None, None) => true,
                _ => false,
            };
            assert!(
                close(now.0, want.0) && close(now.1, want.1),
                "{what}: the landing and the second landing are {now:?}, want {want:?} (the \
                 station moved {:.3} columns)",
                s1 - s0
            );
        }
    }
}

#[test]
fn a_regrid_carries_a_body_off_the_carets_line_with_the_content() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let at = |t, rows, c| PetSense {
        rows,
        ..sense(t, Some(c))
    };
    let mut t = start;
    let _ = pet.tick(at(t, 27, (26, 10)));
    for k in 0..30u16 {
        for _ in 0..6 {
            t += Duration::from_millis(16);
            let _ = pet.tick(at(t, 27, (26, 10 + k)));
        }
    }
    for _ in 0..120 {
        t += Duration::from_millis(16);
        let _ = pet.tick(at(t, 27, (26, 39)));
    }
    assert!(
        pet.action.settled() && (pet.row - 26.0).abs() < 0.01,
        "fixture: at rest beside the caret, got {:?} at row {}",
        pet.action,
        pet.row
    );
    pet.row = 25.0;
    pet.rebase_geometry();
    let mut frames = Vec::new();
    for _ in 0..60 {
        t += Duration::from_millis(16);
        frames.push(pet.tick(at(t, 40, (39, 39))));
    }
    assert!(
        (frames[0].row - 38.0).abs() < 0.01,
        "the body goes the rows its line went: row {}",
        frames[0].row
    );
    let far: Vec<(PetAction, f32)> = frames
        .iter()
        .filter(|f| !(37.0..=39.5).contains(&f.row))
        .map(|f| (f.action, f.row))
        .collect();
    assert!(
        far.is_empty(),
        "no leap across the rows after the re-grid: {far:?}"
    );
}

fn drive_and_collect(
    pet: &mut PetBrain,
    mut t: Instant,
    caret: (u16, u16),
    ticks: u32,
) -> (Vec<PetGlyphId>, Vec<PetFrame>) {
    let mut poses = Vec::new();
    let mut frames = Vec::new();
    for _ in 0..ticks {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some(caret)));
        poses.push(f.pose);
        frames.push(f);
    }
    (poses, frames)
}

#[test]
fn a_half_grid_standing_gap_jumps_once_the_hold_resolves() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let _ = pet.tick(sense(start, Some((3, 5))));
    let (poses, frames) =
        drive_and_collect(&mut pet, start + Duration::from_millis(16), (3, 70), 260);
    assert!(
        poses.contains(&PetGlyphId::PetCrouchWiggle),
        "a half-grid gap earns the gather"
    );
    let last = frames.last().expect("frames");
    assert!(
        (last.col - 71.0).abs() <= SKID_CELLS + 1.0,
        "and the bounds arrive at the station, got {}",
        last.col
    );
}

#[test]
fn a_screen_wide_span_splits_into_two_bounds_unless_the_touch_point_is_inked() {
    for (inked, want) in [(false, 2), (true, 1)] {
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, Instant::now(), 4, 2);
        if inked {
            let spans = vec![(0u16, 0u16), (0, 0), (0, 0), (0, 0), (2, 76)];
            pet.sense_ink(0, &spans, Some(4));
        }
        let (_, frames) = drive_and_collect(&mut pet, t, (4, 78), 260);
        let mut episodes = 0;
        let mut airborne_was = false;
        for f in &frames {
            let airborne = f.action == PetAction::Leap;
            if airborne && !airborne_was {
                episodes += 1;
            }
            airborne_was = airborne;
            if want == 2 && episodes == 1 && f.action == PetAction::Land {
                assert_eq!(f.pose, PetGlyphId::PetCrouch, "the touch-land's pose");
            }
        }
        assert_eq!(episodes, want, "inked midfield {inked}: {episodes} bounds");
    }
}

#[test]
fn the_bound_apex_never_clips_the_viewport_top() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 1, 10);
    let (_, frames) = drive_and_collect(&mut pet, t, (1, 60), 220);
    for f in &frames {
        if f.action == PetAction::Leap {
            let top = f.row + 1.0 - ART_ROWS - f.lift;
            assert!(
                top >= -0.05,
                "the sprite top left the viewport: row {} lift {}",
                f.row,
                f.lift
            );
        }
    }
}

fn drop_run(pet: &mut PetBrain, start: Instant, caret: (u16, u16), ticks: u32) -> Vec<PetFrame> {
    let mut t = start;
    let mut out = Vec::new();
    for _ in 0..ticks {
        t += Duration::from_millis(16);
        out.push(pet.tick(sense(t, Some(caret))));
    }
    out
}

fn asleep_at(pet: &mut PetBrain, start: Instant, row: u16, col: u16) -> Instant {
    let t = awake(pet, start, row, col);
    let (t, f) = idle(pet, t, (row, col + 2), SLEEP_AFTER + 1.0);
    assert_eq!(f.action, PetAction::Sleep, "fixture: a sleeping cat");
    t
}

#[test]
fn an_enter_drops_the_sleeper_off_the_line() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = asleep_at(&mut pet, start, 4, 18);
    let col0 = pet.col;
    let frames = drop_run(&mut pet, t, (5, 0), 200);

    let f0 = &frames[0];
    assert_eq!(f0.action, PetAction::Leap, "the floor went: the cat falls");
    assert_eq!(f0.pose, PetGlyphId::PetSleep1, "and it is still asleep");
    assert_eq!(f0.lift, 0.0, "a fall has no arc");
    assert!(
        frames.iter().all(|f| f.action != PetAction::Waking),
        "the stretch is spent in the air, never on the old floor"
    );
    assert!(
        frames.iter().all(|f| f.pose != PetGlyphId::PetCrouchWiggle),
        "and a fall is not a show"
    );

    let landed = frames
        .iter()
        .position(|f| f.action == PetAction::Land)
        .expect("the fall must land");
    assert!(
        (frames[landed].row - 5.0).abs() < 0.01,
        "onto the row the line opened, got {}",
        frames[landed].row
    );
    assert!(
        frames[..=landed]
            .iter()
            .all(|f| (f.col - col0).abs() < 0.01),
        "a fall is straight down"
    );

    let last = frames.last().expect("frames");
    assert!(
        last.action.settled()
            && (last.col - PetBrain::station(0, 100, art_cols(10, 20))).abs() < 1.5,
        "and it ends home at its station, got {} ({:?})",
        last.col,
        last.action
    );
    let (_, f) = idle(&mut pet, t, (5, 0), SLEEP_AFTER + BREATH_WINDOW + 2.0);
    assert_eq!(f.action, PetAction::Sleep);
    assert!(
        !pet.needs_frames(),
        "the drop's latches must all be spent by the time it sleeps"
    );
}

#[test]
fn a_settled_cat_after_a_drop_still_notices() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = asleep_at(&mut pet, start, 4, 18);
    let frames = drop_run(&mut pet, t, (5, 18), 400);
    assert!(
        frames.iter().any(|f| f.action == PetAction::Leap),
        "fixture: the floor went and the cat fell"
    );
    let mut t = t + Duration::from_millis(16 * 400);
    let (t2, f) = idle(&mut pet, t, (5, 18), SIT_AFTER + 0.5);
    t = t2;
    assert!(f.action.settled(), "fixture: settled where it fell");
    assert!(
        !pet.drop_stare,
        "the fall's beat is spent by settling, not carried"
    );
    let mut perked = false;
    let mut launched = false;
    for _ in 0..40 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((5, 80))));
        perked |= f.action == PetAction::Perk;
        launched |= f.action == PetAction::Leap;
    }
    assert!(perked, "a settled cat still looks up before a bound");
    assert!(launched, "…and then goes");
}

#[test]
fn only_a_blank_row_just_below_drops_the_sleeper() {
    let start = Instant::now();
    for (name, caret, ink) in [
        ("a caret going up", (3u16, 0u16), false),
        ("a three-line prompt", (7, 0), false),
        ("ink under its own column", (5, 0), true),
    ] {
        let mut pet = PetBrain::default();
        let t = asleep_at(&mut pet, start, 4, 18);
        if ink {
            let mut spans = [(0u16, 0u16); 12];
            spans[5] = (15, 40);
            pet.sense_ink(0, &spans, Some(5));
            assert!(
                pet.col > 15.0 && pet.col < 40.0,
                "fixture: ink under the cat"
            );
        }
        let frames = drop_run(&mut pet, t, caret, 40);
        assert_eq!(
            frames[0].action,
            PetAction::Waking,
            "{name}: a wake, not a fall"
        );
        assert_eq!(frames[0].pose, PetGlyphId::PetStretch, "{name}");
        assert!(
            !pet.pending_drop,
            "{name}: nothing is owed a fall afterwards"
        );
    }
}

#[test]
fn a_twice_moved_flight_lands_and_stares_before_it_admits_it() {
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, Instant::now(), 4, 10);
    let mut caret = 30u16;
    let (mut two, mut three) = (false, false);
    let mut frames = Vec::new();
    for _ in 0..300 {
        t += Duration::from_millis(16);
        if let Some(f) = pet.flight {
            let u = f.t / f.dur;
            if !two && u >= 0.3 {
                caret = 60;
                two = true;
            } else if two && !three && u >= 0.6 {
                caret = 20;
                three = true;
            }
        }
        frames.push(pet.tick(sense(t, Some((4, caret)))));
    }
    assert!(
        two && three,
        "fixture: the caret must move twice in the air"
    );
    let landed = frames
        .iter()
        .position(|f| f.action == PetAction::Land)
        .expect("the pounce lands");
    let stare = frames[landed..]
        .iter()
        .skip_while(|f| f.action == PetAction::Land)
        .take_while(|f| f.action == PetAction::Stand)
        .count();
    assert!(stare > 0, "the cat stands on the empty cell first");
    assert!(
        frames[landed + stare..]
            .iter()
            .any(|f| f.action == PetAction::Perk),
        "then the double-take"
    );
    let last = frames.last().expect("frames");
    assert!(
        (last.col - PetBrain::station(20, 100, art_cols(10, 20))).abs() < 1.5,
        "home at the station it was left with, got {}",
        last.col
    );
    assert_eq!(pet.commit_t, 0.0, "and the beat is spent, not owed");
}

#[test]
fn the_same_typing_still_produces_the_same_cat() {
    let run = |start: Instant| -> Vec<u64> {
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 10);
        let mut fps = Vec::new();
        for (row, col, ticks) in [(4u16, 30u16, 90u32), (5, 2, 70), (5, 80, 240), (5, 82, 700)] {
            for _ in 0..ticks {
                t += Duration::from_millis(16);
                fps.push(pet.tick(sense(t, Some((row, col)))).fp());
            }
        }
        fps
    };
    let a = run(Instant::now());
    let b = run(Instant::now() + Duration::from_millis(7_919));
    assert_eq!(a, b, "the same typing must produce the same cat");
    assert!(
        a.windows(2).any(|w| w[0] != w[1]),
        "and the script actually animates"
    );
}

fn sleeper_with_a_live_zee(start: Instant) -> (PetBrain, Instant) {
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 10);
    let (mut t, _) = idle(&mut pet, t, (4, 12), SLEEP_AFTER + 0.1);
    for _ in 0..400 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 12))));
        if f.motes
            .iter()
            .flatten()
            .any(|m| m.kind == PetMoteKind::Zee && m.alpha > 0)
        {
            return (pet, t);
        }
    }
    panic!("fixture: the sleeper never dreamed a visible z");
}

#[test]
fn retire_unowned_kills_a_live_mote_where_withholding_the_caret_does_not() {
    let start = Instant::now();

    let (mut control, mut ct) = sleeper_with_a_live_zee(start);
    ct += Duration::from_millis(16);
    let drifting = control.tick(sense(ct, None));
    assert!(
        drifting.motes.iter().flatten().any(|m| m.alpha > 0),
        "fixture: withholding the caret leaves the mote lane drifting — that \
         is the behaviour this switch exists to overrule"
    );
    assert!(
        control.needs_frames(),
        "…and it is still asking for the 60 fps lane it is about to lose"
    );

    let (mut pet, mut t) = sleeper_with_a_live_zee(start);
    assert!(pet.is_active(), "fixture: a visible pet");
    assert!(
        !pet.needs_frames() && pet.next_change_deadline(t).is_some(),
        "fixture: a drifting z names its next wake"
    );
    pet.retire_unowned();
    assert!(!pet.is_active(), "a retired pet paints nothing");
    assert!(!pet.needs_frames(), "…and owes the scheduler no frames");
    assert!(
        pet.next_change_deadline(t).is_none(),
        "…and names no wake for a z it no longer carries"
    );

    t += Duration::from_millis(16);
    let after = pet.tick(sense(t, None));
    assert_eq!(after.alpha, 0, "the body is gone at once, not fading");
    assert!(
        after.motes.iter().all(Option::is_none),
        "the mote lane must be empty, found {:?}",
        after
            .motes
            .iter()
            .flatten()
            .map(|m| m.kind)
            .collect::<Vec<_>>()
    );
    assert!(
        after.departures.iter().all(Option::is_none),
        "and no departing body either"
    );

    pet.retire_unowned();
    assert!(!pet.needs_frames(), "retirement is idempotent");
    t += Duration::from_millis(16);
    let still = pet.tick(sense(t, None));
    assert_eq!(still.alpha, 0);
    assert!(still.motes.iter().all(Option::is_none));
}

#[test]
fn the_first_step_out_of_a_seat_faces_the_gap() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 20);
    let (mut t, f) = idle(&mut pet, t, (4, 20), SIT_AFTER + 0.2);
    assert!(
        f.facing_left && f.action.settled(),
        "fixture: seated facing left, got {:?} left={}",
        f.action,
        f.facing_left
    );
    for _ in 0..30 {
        t += Duration::from_millis(16);
        let g = pet.tick(sense(t, Some((4, 22))));
        if g.col > f.col + 1e-3 {
            assert!(!g.facing_left, "the first moving frame faces the gap");
            return;
        }
    }
    panic!("fixture: the seat never moved");
}

#[test]
fn a_hidden_pet_takes_a_look_sync_immediately() {
    let mut pet = PetBrain::default();
    assert!(!pet.is_active(), "a fresh pet is hidden");
    assert_eq!(pet.sync_look((3, 1), PetArrival::Ceremony).worn, (3, 1));
    assert_eq!(
        pet.sync_look((9, 4), PetArrival::Ceremony).worn,
        (9, 4),
        "hidden: no appearance to protect, the swap is free"
    );
    assert_eq!(
        pet.sync_look((9, 4), PetArrival::Ceremony).worn,
        (9, 4),
        "and the agreeing re-sync is a no-op"
    );
}

#[test]
fn a_bell_wakes_the_sleeper() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let (mut t, f) = idle(&mut pet, start, (4, 10), 1.0);
    assert_eq!(
        f.action,
        PetAction::Sleep,
        "fixture: an untouched window naps"
    );
    pet.note_bell(t);
    t += Duration::from_millis(16);
    let f = pet.tick(sense(t, Some((4, 10))));
    assert_eq!(
        f.action,
        PetAction::Waking,
        "the bell takes the EXISTING wake path"
    );
    assert_eq!(f.pose, PetGlyphId::PetStretch, "stretch and all");
    assert!(
        f.motes
            .iter()
            .flatten()
            .any(|m| m.kind == PetMoteKind::ZeePop),
        "with the startled z popped ahead of it"
    );
}

#[test]
fn a_bell_flinches_the_settled_pet() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 3, 10);
    let (mut t, _) = idle(&mut pet, t, (3, 12), 0.5);
    pet.note_bell(t);
    t += Duration::from_millis(16);
    let f = pet.tick(sense(t, Some((3, 12))));
    assert_eq!(
        f.action,
        PetAction::Perk,
        "a bell is a noise, not a held delete: the FLINCH tier, never the bottle"
    );
    assert!(
        f.scale_y < 1.0 && f.scale_x > 1.0,
        "the duck must be visible: y {} x {}",
        f.scale_y,
        f.scale_x
    );
    assert!(
        f.facing_left,
        "and the fright faces the caret (left of the station)"
    );
}

#[test]
fn a_bell_during_flight_expires_unconsumed() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 4, 10);
    t += Duration::from_millis(16);
    let _ = pet.tick(sense(t, Some((4, 30))));
    pet.note_bell(t);
    let mut landed = false;
    let mut acted_after_ground = false;
    for _ in 0..160 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 30))));
        if f.action == PetAction::Land {
            landed = true;
        }
        if landed && matches!(f.action, PetAction::Perk | PetAction::Waking) {
            acted_after_ground = true;
        }
    }
    assert!(landed, "fixture: the jump must land inside the window");
    assert!(
        pet.pending_bell.is_none(),
        "the latch must expire per its TTL"
    );
    assert!(
        !acted_after_ground,
        "a bell from before the leap must not fire after it"
    );
}

#[test]
fn reduced_motion_turns_every_stimulus_into_bookkeeping() {
    type Note = fn(&mut PetBrain, Instant);
    type Feel = fn(&mut PetSense, u32);
    type Check = fn(&PetBrain, f32);
    let quiet: Feel = |_, _| {};
    let nothing: Check = |_, _| {};
    let stimuli: [(&str, bool, Note, Feel, Check); 6] = [
        ("bell", false, |pet, t| pet.note_bell(t), quiet, nothing),
        (
            "verdicts",
            false,
            |pet, t| {
                let c0 = pet.content();
                pet.note_command_done(t, false, Some(240_000));
                let warmed = pet.content();
                assert!(warmed > c0, "verdicts: the ledger still warms");
                pet.note_command_done(t, true, Some(1000));
                assert!(pet.content() < warmed, "verdicts: and still cools");
            },
            quiet,
            nothing,
        ),
        (
            "pet",
            false,
            |pet, t| pet.note_petted(t),
            quiet,
            |pet, c0| assert!(pet.content() > c0, "pet: the warmth still lands"),
        ),
        (
            "peek",
            true,
            |pet, t| pet.note_peek(t, 15.0, 3.0),
            quiet,
            nothing,
        ),
        (
            "pointer",
            true,
            |_, _| {},
            |s, k| s.pointer = Some(((59.0 - k as f32).max(2.0), 3.0)),
            nothing,
        ),
        (
            "stream",
            true,
            |_, _| {},
            |s, _| s.output_burst = true,
            nothing,
        ),
    ];
    for (name, warm_live, note, feel, check) in stimuli {
        let mut pet = PetBrain::default();
        let mut t = Instant::now();
        for _ in 0..40 {
            t += Duration::from_millis(16);
            let mut s = sense(t, Some((3, 10)));
            s.reduced_motion = !warm_live;
            let _ = pet.tick(s);
        }
        let c0 = pet.content();
        note(&mut pet, t);
        for k in 0..120u32 {
            t += Duration::from_millis(16);
            let mut s = sense(t, Some((3, 10)));
            s.reduced_motion = true;
            feel(&mut s, k);
            let f = pet.tick(s);
            assert!(
                matches!(f.action, PetAction::Sit | PetAction::Sleep),
                "{name}: no action under reduced motion, got {:?}",
                f.action
            );
            assert_eq!((f.scale_x, f.scale_y), (1.0, 1.0), "{name}: no squash");
            assert!(f.motes.iter().all(Option::is_none), "{name}: no motes");
        }
        check(&pet, c0);
        assert!(
            pet.pending_bell.is_none() && pet.pending_cheer.is_none() && !pet.pending_sulk,
            "{name}: the bell and verdict latches clear as bookkeeping"
        );
        assert_eq!(pet.pending_pets(), 0, "{name}: the pet latch is spent");
        assert_eq!(pet.pet_hold_t, 0.0, "{name}: no purr hold");
        assert!(pet.pending_bat.is_none(), "{name}: the bat latch clears");
        assert_eq!(pet.pointer_heat, 0.0, "{name}: no gaze");
        assert!(pet.pending_pointer_pounce.is_none(), "{name}: no pounce");
        assert_eq!(pet.watch_heat, 0.0, "{name}: no watch heat left behind");
    }
}

#[test]
fn a_long_success_cheers_and_warms() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 3, 10);
    let (mut t, _) = idle(&mut pet, t, (3, 12), 0.3);
    let c0 = pet.content();
    pet.note_command_done(t, false, Some(2500));
    assert!(
        pet.content() > c0 + CHEER_CONTENT - 0.01,
        "the ledger warms at note time"
    );
    let (mut saw_perk, mut saw_frolic, mut saw_note) = (false, false, false);
    for _ in 0..120 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((3, 12))));
        match f.action {
            PetAction::Perk => saw_perk = true,
            PetAction::Frolic => {
                saw_frolic = true;
                if f.motes
                    .iter()
                    .flatten()
                    .any(|m| m.kind == PetMoteKind::Note)
                {
                    saw_note = true;
                }
            }
            _ => {}
        }
    }
    assert!(saw_perk, "the notice leads the celebration");
    assert!(saw_frolic, "then the frolic");
    assert!(saw_note, "with the one floating note");
}

#[test]
fn a_failure_droops_and_cools() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 3, 2);
    let (t, _) = type_run(&mut pet, t, 3, 4, 30, 0.05);
    let (mut t, _) = idle(&mut pet, t, (3, 34), 0.6);
    let c0 = pet.content();
    assert!(c0 > 0.3, "fixture: the run must have warmed the cat ({c0})");
    pet.note_command_done(t, true, Some(120_000));
    assert!(
        pet.content() <= c0 - SULK_CONTENT + 1e-5,
        "a failure cools harder than a success warms"
    );
    let mut saw_droop = false;
    for _ in 0..160 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((3, 34))));
        if f.action == PetAction::Droop {
            saw_droop = true;
            assert_eq!(
                f.pose,
                PetGlyphId::PetDroop,
                "the sulk wears its own flat-ears frame — off its feet, \
                 the grief interrupted a stand"
            );
            assert!(f.motes.iter().all(Option::is_none), "grief floats NOTHING");
        }
    }
    assert!(saw_droop, "a failure must droop");
}

#[test]
fn a_fast_command_only_nudges() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 3, 10);
    let (mut t, _) = idle(&mut pet, t, (3, 12), 0.4);
    let c0 = pet.content();
    pet.note_command_done(t, false, Some(300));
    assert!(pet.content() > c0, "the ledger still moves");
    for _ in 0..80 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((3, 12))));
        assert!(
            !matches!(
                f.action,
                PetAction::Perk | PetAction::Frolic | PetAction::Droop
            ),
            "a bare `ls` earns no choreography, got {:?}",
            f.action
        );
    }
}

#[test]
fn a_cheer_survives_a_hold() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let (t, _) = type_run(&mut pet, start, 2, 10, 8, 0.06);
    let (mut t, _) = idle(&mut pet, t, (2, 20), 0.5);
    t += Duration::from_millis(16);
    let f = pet.tick(sense(t, Some((2, 15))));
    assert_eq!(f.action, PetAction::Startle, "fixture: the hold is live");
    pet.note_command_done(t, false, Some(5000));
    let mut saw_frolic = false;
    for _ in 0..200 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((2, 15))));
        if f.action == PetAction::Frolic {
            saw_frolic = true;
        }
    }
    assert!(
        saw_frolic,
        "the cheer latch must outlive the startle hold and the walk home"
    );
}

fn note_cheer_births(pet: &PetBrain, births: &mut Vec<((u64, u8), PetMoteKind)>) {
    for m in pet.motes.iter().flatten() {
        let key = (m.born.to_bits(), m.seed);
        if matches!(m.kind, PetMoteKind::Note | PetMoteKind::Heart)
            && !births.iter().any(|&(k, _)| k == key)
        {
            births.push((key, m.kind));
        }
    }
}

#[test]
fn a_landing_wakes_a_dozing_cat_into_the_big_cheer_and_it_settles_after() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 3, 10);
    let (mut t, f) = idle(&mut pet, t, (3, 12), SLEEP_AFTER + 1.0);
    assert_eq!(
        f.action,
        PetAction::Sleep,
        "fixture: it dozed while the agent worked"
    );
    assert!(!pet.needs_frames(), "fixture: a dozing cat owes no frames");
    let c0 = pet.content();
    pet.note_room_landed();
    assert!(
        pet.content() > c0 + CHEER_CONTENT - 0.01,
        "the ledger warms at note time, as a green build's does"
    );
    assert_eq!(pet.pending_cheer, Some(true), "the BIG cheer is latched");
    let mut beats: Vec<PetAction> = vec![f.action];
    let mut births = Vec::new();
    let end = t + Duration::from_secs(6);
    while t < end {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((3, 12))));
        if beats.last() != Some(&f.action) {
            beats.push(f.action);
        }
        note_cheer_births(&pet, &mut births);
    }
    let at = |a: PetAction| beats.iter().position(|&b| b == a);
    let (Some(waking), Some(perk), Some(frolic)) = (
        at(PetAction::Waking),
        at(PetAction::Perk),
        at(PetAction::Frolic),
    ) else {
        panic!("the stretch, the perk and the frolic all play: {beats:?}");
    };
    assert!(
        waking < perk && perk < frolic,
        "a sleeper stretches awake, then perks, then frolics: {beats:?}"
    );
    assert_eq!(births.len(), 3, "three ♪/♥ motes, a build's: {births:?}");
    assert!(
        births.windows(2).all(|w| w[0].1 != w[1].1),
        "♪ and ♥ alternate: {births:?}"
    );
    assert!(
        pet.motes.iter().flatten().all(|m| m.age(pet.clock) >= 1.0),
        "spent"
    );
    assert!(pet.pending_cheer.is_none(), "the cheer is played, not owed");
    assert!(!pet.needs_frames(), "back off the frame train");
    assert!(
        pet.next_change_deadline(t).is_some_and(|d| d > t),
        "and on the settled ladder: its next step named, not the next frame"
    );
}

fn note_tiles_meeting(
    f: &PetFrame,
    cell: (u16, u16),
    grid: (u16, u16),
) -> (Vec<String>, usize, bool) {
    let tiles: Vec<_> = f
        .motes
        .iter()
        .flatten()
        .filter(|m| matches!(m.kind, PetMoteKind::Note | PetMoteKind::Heart) && m.alpha > 0)
        .filter_map(|m| mote_box_px((m.col, m.row), m.scale, cell, grid).map(|b| (m, b)))
        .collect();
    let mut meeting = Vec::new();
    for (i, (ma, a)) in tiles.iter().enumerate() {
        for (mb, b) in &tiles[i + 1..] {
            let (sa, sb) = (i32::from(a.2), i32::from(b.2));
            if a.0 < b.0 + sb && b.0 < a.0 + sa && a.1 < b.1 + sb && b.1 < a.1 + sa {
                meeting.push(format!(
                    "{:?}@{:.2},{:.2} {a:?} meets {:?}@{:.2},{:.2} {b:?}",
                    ma.kind, ma.col, ma.row, mb.kind, mb.col, mb.row
                ));
            }
        }
    }
    let grid_w = i32::from(grid.0) * i32::from(cell.0);
    let held = tiles.iter().any(|(m, b)| {
        let cx = (m.col * f32::from(cell.0)).round() as i32;
        let d = i32::from(b.2);
        cx - d / 2 < 0 || cx - d / 2 + d > grid_w
    });
    (meeting, tiles.len(), held)
}

#[test]
fn notes_and_hearts_in_the_air_together_never_overlap() {
    let start = Instant::now();
    let at = |t: Instant, caret: (u16, u16), cell: (u16, u16)| PetSense {
        cell_w: cell.0,
        cell_h: cell.1,
        ..sense(t, Some(caret))
    };
    type Case = (&'static str, u16, usize, fn(&mut PetBrain, Instant));
    let cases: [Case; 9] = [
        ("a big green build", 40, 3, |p, t| {
            p.note_command_done(t, false, Some(CHEER_BIG_MS + 10_000));
        }),
        ("a long green build", 40, 2, |p, t| {
            p.note_command_done(t, false, Some(CHEER_MIN_MS + 2_000));
        }),
        ("a landing", 40, 3, |p, _| p.note_room_landed()),
        ("a typed treat", 40, 2, |p, t| {
            p.note_trick(t, Trick::Treat, true)
        }),
        ("a typed sing", 40, usize::from(SONG_NOTES), |p, t| {
            p.note_trick(t, Trick::Sing, true);
        }),
        ("four pets at once", 40, 4, |p, _| {
            let w = art_cols(p.last_cell_w, p.last_cell_h);
            p.spawn_pet_hearts(4, w);
        }),
        ("a mail ♪ into a cheer", 40, 4, |p, t| {
            p.note_command_done(t, false, Some(CHEER_BIG_MS + 10_000));
        }),
        ("a big cheer at the right edge", 96, 3, |p, _| {
            let w = art_cols(p.last_cell_w, p.last_cell_h);
            p.facing_left = false;
            p.spawn_cheer_motes(true, w, false);
        }),
        ("a big cheer at the left edge", 2, 3, |p, _| {
            let w = art_cols(p.last_cell_w, p.last_cell_h);
            p.facing_left = true;
            p.spawn_cheer_motes(true, w, false);
        }),
    ];
    for cell in [(7, 14), (8, 16), (9, 19), (10, 20), (15, 28), (16, 32)] {
        for (why, col, want, play) in cases {
            for face_left in [false, true] {
                let mut pet = PetBrain::default();
                let caret = (6, col);
                let mut t = start;
                for _ in 0..30 {
                    t += Duration::from_millis(16);
                    let _ = pet.tick(at(t, caret, cell));
                }
                for k in 1..=2 {
                    t += Duration::from_millis(50);
                    let _ = pet.tick(at(t, (6, col + k), cell));
                }
                let end = t + Duration::from_secs_f32(SIT_AFTER + 0.5);
                while t < end {
                    t += Duration::from_millis(16);
                    let _ = pet.tick(at(t, (6, col + 2), cell));
                }
                pet.facing_left = face_left;
                play(&mut pet, t);
                let mut most = 0;
                for k in 0..(6.0 / 0.016) as usize {
                    t += Duration::from_millis(16);
                    if why == "a mail ♪ into a cheer" && k == 70 {
                        let w = art_cols(cell.0, cell.1);
                        pet.mail_beat(w);
                    }
                    let f = pet.tick(at(t, (6, col + 2), cell));
                    let (meeting, n, held) = note_tiles_meeting(&f, cell, (100, 30));
                    assert!(
                        meeting.is_empty(),
                        "{why} at {cell:?} facing left {face_left}, frame {k}: {meeting:?}"
                    );
                    assert!(
                        !held,
                        "{why} at {cell:?} facing left {face_left}, frame {k}: a ♪/♥ \
                         held at the pane's side"
                    );
                    most = most.max(n);
                }
                assert!(
                    most >= want,
                    "{why} at {cell:?} facing left {face_left}: {most} ♪/♥ in the air \
                     together, {want} wanted"
                );
            }
        }
    }
}

#[test]
fn under_reduced_motion_a_landing_moves_the_ledger_and_plays_nothing() {
    let reduced = |t: Instant| PetSense {
        reduced_motion: true,
        ..sense(t, Some((3, 12)))
    };
    let mut pet = PetBrain::default();
    let mut t = Instant::now();
    for _ in 0..30 {
        t += Duration::from_millis(16);
        let _ = pet.tick(reduced(t));
    }
    let c0 = pet.content();
    pet.note_room_landed();
    assert!(
        pet.content() > c0 + CHEER_CONTENT - 0.01,
        "the ledger moves"
    );
    for _ in 0..240 {
        t += Duration::from_millis(16);
        let f = pet.tick(reduced(t));
        assert!(
            !matches!(
                f.action,
                PetAction::Waking | PetAction::Perk | PetAction::Frolic
            ),
            "no choreography under reduced motion, got {:?}",
            f.action
        );
        assert!(f.motes.iter().all(Option::is_none), "no motes either");
    }
    assert!(pet.pending_cheer.is_none(), "nothing is owed");
    assert!(!pet.needs_frames(), "and no frames are claimed");
}

#[test]
fn a_pet_on_the_head_makes_a_heart() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 10);
    let (mut t, _) = idle(&mut pet, t, (4, 12), 0.4);
    let c0 = pet.content();
    pet.note_petted(t);
    t += Duration::from_millis(16);
    let f = pet.tick(sense(t, Some((4, 12))));
    assert_eq!(
        f.action,
        PetAction::Purr,
        "affection is answered even by a cold cat: the purr-flavored hold"
    );
    assert!(
        f.motes
            .iter()
            .flatten()
            .any(|m| m.kind == PetMoteKind::Heart),
        "with a heart"
    );
    assert!(pet.content() > c0, "and warmth");
    assert!(pet.pet_hold_t > 0.0, "held, not earned — yet");
}

#[test]
fn a_gentle_body_stroke_earns_a_visible_finite_affection_beat() {
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, Instant::now(), 4, 10);
    let (mut t, initial) = idle(&mut pet, t, (4, 12), 1.0);
    let width = art_cols(10, 20);
    let mut frames = Vec::new();
    let mut held_locations = Vec::new();
    for step in 0..=16 {
        t += Duration::from_millis(50);
        frames.push(ptick(
            &mut pet,
            t,
            (4, 12),
            Some((
                initial.col + width * (0.20 + 0.60 * step as f32 / 16.0),
                4.1,
            )),
        ));
        if pet.pet_hold_t > 0.0 {
            let f = frames.last().unwrap();
            held_locations.push((f.col, f.row, f.lift));
        }
    }
    for _ in 0..70 {
        t += Duration::from_millis(16);
        frames.push(pet.tick(sense(t, Some((4, 12)))));
        if pet.pet_hold_t > 0.0 {
            let f = frames.last().unwrap();
            held_locations.push((f.col, f.row, f.lift));
        }
    }
    assert!(frames.iter().any(|f| f.action == PetAction::Purr));
    assert!(
        frames.iter().any(|f| {
            f.motes
                .iter()
                .flatten()
                .any(|m| m.kind == PetMoteKind::Heart)
        }),
        "a stroke gives the same heart as a click"
    );
    assert!(
        held_locations
            .iter()
            .all(|p| *p == held_locations[0] && p.2 == 0.0),
        "affection changes expression, never the pet's location"
    );
    assert_eq!(
        pet.pet_hold_t, 0.0,
        "one stroke buys only the existing finite hold"
    );
    let (_, f) = idle(&mut pet, t, (4, 12), SLEEP_AFTER + BREATH_WINDOW + 2.0);
    assert_eq!(f.action, PetAction::Sleep);
    assert!(!pet.needs_frames());
    assert!(
        pet.next_change_deadline(t + Duration::from_secs(60))
            .is_none()
    );
}

#[test]
fn a_parked_pointer_over_the_body_never_farms_affection_or_frames() {
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, Instant::now(), 4, 10);
    let pointer = Some((pet.col + art_cols(10, 20) * 0.5, 4.1));
    for _ in 0..360 {
        t += Duration::from_millis(100);
        let f = ptick(&mut pet, t, (4, 12), pointer);
        assert_eq!(pet.pending_pets(), 0);
        assert_eq!(pet.pet_hold_t, 0.0);
        assert!(
            !f.motes
                .iter()
                .flatten()
                .any(|m| m.kind == PetMoteKind::Heart)
        );
    }
    assert!(!pet.needs_frames());
    assert!(
        pet.next_change_deadline(t).is_none(),
        "quiet={} action={:?} offer={:?}",
        pet.quiet,
        pet.action,
        pet.next_change_secs()
    );
}

#[test]
fn stroke_progress_cannot_cross_hidden_reduced_or_work_frames() {
    for interrupt in 0..4 {
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, Instant::now(), 4, 10);
        let (mut t, initial) = idle(&mut pet, t, (4, 12), 1.0);
        let width = art_cols(10, 20);
        for step in 0..=16 {
            t += Duration::from_millis(50);
            let mut s = sense(t, Some((4, 12)));
            s.pointer = Some((
                initial.col + width * (0.20 + 0.60 * step as f32 / 16.0),
                4.1,
            ));
            if step == 8 {
                match interrupt {
                    0 => s.caret = None,
                    1 => s.reduced_motion = true,
                    2 => s.caret = Some((4, 13)),
                    _ => s.output_burst = true,
                }
            }
            pet.tick(s);
            assert_eq!(pet.pending_pets(), 0, "interrupt={interrupt}, step={step}");
            assert_eq!(pet.pet_hold_t, 0.0, "interrupt={interrupt}, step={step}");
        }
    }
}

#[test]
fn gentle_strokes_can_wake_a_sleeper_but_not_a_reduced_motion_pet() {
    for reduced in [false, true] {
        let mut pet = PetBrain::default();
        let start = Instant::now();
        let (mut t, initial) = idle(&mut pet, start, (4, 12), SLEEP_AFTER + BREATH_WINDOW + 2.0);
        assert_eq!(initial.action, PetAction::Sleep);
        let width = art_cols(10, 20);
        let mut woke = false;
        for step in 0..=16 {
            t += Duration::from_millis(50);
            let mut s = sense(t, Some((4, 12)));
            s.reduced_motion = reduced;
            s.pointer = Some((
                initial.col + width * (0.20 + 0.60 * step as f32 / 16.0),
                4.1,
            ));
            let f = pet.tick(s);
            woke |= f.action == PetAction::Waking;
        }
        assert_eq!(woke, !reduced);
        if reduced {
            assert_eq!(pet.content(), 0.0, "hover is not a reduced-motion click");
            assert_eq!(pet.pending_pets(), 0);
            assert!(!pet.needs_frames());
        } else {
            let mut answered = false;
            for _ in 0..150 {
                t += Duration::from_millis(16);
                answered |= pet.tick(sense(t, Some((4, 12)))).action == PetAction::Purr;
            }
            assert!(answered, "the existing wake stretch hands into affection");
        }
    }
}

#[test]
fn a_pet_during_flight_waits_for_the_ground() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 4, 10);
    t += Duration::from_millis(16);
    let _ = pet.tick(sense(t, Some((4, 30))));
    pet.note_petted(t);
    let (mut landed, mut purr_mid_air, mut purr_after_land) = (false, false, false);
    for _ in 0..200 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 30))));
        if f.action == PetAction::Land {
            landed = true;
        }
        if f.action == PetAction::Purr {
            if landed {
                purr_after_land = true;
            } else {
                purr_mid_air = true;
            }
        }
    }
    assert!(landed, "fixture: the pounce must land");
    assert!(!purr_mid_air, "a pet mid-flight waits for the ground");
    assert!(
        purr_after_land,
        "and is honored on landing (inside its TTL)"
    );
}

#[test]
fn a_stale_pet_expires() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 3, 2);
    pet.note_petted(t);
    let (t, _) = type_run(&mut pet, t, 3, 4, 50, 0.045);
    assert_eq!(pet.pending_pets(), 0, "the latch expired mid-chase");
    let (_, _f) = idle(&mut pet, t, (3, 54), 0.6);
    assert_eq!(
        pet.pet_hold_t, 0.0,
        "no purr hold fires for a click from before the sprint"
    );
}

#[test]
fn wave_one_latches_arm_and_release_the_frame_lane() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let (t, _) = type_run(&mut pet, start, 4, 10, 6, 0.05);
    let (mut t, _) = idle(&mut pet, t, (4, 16), SLEEP_AFTER + BREATH_WINDOW + 2.0);
    assert!(!pet.needs_frames(), "fixture: deeply asleep, lane released");
    pet.note_petted(t);
    assert!(pet.needs_frames(), "a latched pet re-arms the lane");
    let (t2, _) = idle(&mut pet, t, (4, 16), 6.0);
    t = t2;
    assert_eq!(pet.pending_pets(), 0, "consumed on the ground");
    let (_, f) = idle(&mut pet, t, (4, 16), SLEEP_AFTER + BREATH_WINDOW + 2.0);
    assert_eq!(f.action, PetAction::Sleep);
    assert!(
        !pet.needs_frames(),
        "every wave-1 envelope is finite: the lane releases again"
    );
}

#[test]
fn a_streaming_pane_perks_the_sitter() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 10);
    let (t, f) = idle(&mut pet, t, (4, 12), SIT_AFTER + 0.4);
    assert!(f.action.settled(), "fixture: a settled cat");
    let (_, f) = stream(&mut pet, t, (4, 12), 0.6);
    assert_eq!(
        f.action,
        PetAction::Perk,
        "the pet turns toward the streaming text and watches"
    );
    assert!(
        !f.facing_left || f32::from(12u16) < f.col,
        "and it faces the output (the caret side)"
    );
}

#[test]
fn typing_echo_never_perks() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 10);
    let (mut t, _) = idle(&mut pet, t, (4, 12), SIT_AFTER + 0.4);
    for _ in 0..120 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 12))));
        assert_ne!(
            f.action,
            PetAction::Perk,
            "no burst, no watch — the sitter just sits"
        );
    }
    assert_eq!(pet.watch_heat, 0.0, "and no heat accumulates");
}

#[test]
fn the_watch_ends_before_sleep_is_due() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 10);
    let (t, _) = idle(&mut pet, t, (4, 12), SIT_AFTER + 0.2);
    let (t, f) = stream(&mut pet, t, (4, 12), WATCH_MAX + 2.0);
    assert_ne!(
        f.action,
        PetAction::Perk,
        "the hard cap ends the stare — a settled still sticker remains"
    );
    assert!(pet.watch_spent, "the cap latched");
    let (t, _) = idle(&mut pet, t, (4, 12), SLEEP_AFTER + BREATH_WINDOW + 2.0);
    assert!(
        !pet.needs_frames(),
        "the watch is finite: idle-to-zero survives an endless stream"
    );
    let f1 = pet.tick(sense(t + Duration::from_secs(1), Some((4, 12))));
    let f2 = pet.tick(sense(t + Duration::from_secs(2), Some((4, 12))));
    assert_eq!(f1.fp(), f2.fp(), "and the frame is byte-stable");
}

#[test]
fn a_caret_jump_interrupts_the_watch() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 10);
    let (t, _) = idle(&mut pet, t, (4, 12), SIT_AFTER + 0.3);
    let (mut t, f) = stream(&mut pet, t, (4, 12), 1.0);
    assert_eq!(f.action, PetAction::Perk, "fixture: mid-watch");
    let mut saw_travel = false;
    for k in 0..120 {
        t += Duration::from_millis(16);
        let caret = (4u16, 30u16);
        let mut s = sense(t, Some(caret));
        s.output_burst = k < 30;
        let f = pet.tick(s);
        if matches!(f.action, PetAction::Crouch | PetAction::Leap) {
            saw_travel = true;
        }
    }
    assert!(
        saw_travel,
        "the latched jump must pull the watcher off its perch"
    );
}

#[test]
fn a_nearby_peek_gets_batted() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 12);
    let (mut t, _) = idle(&mut pet, t, (4, 14), SIT_AFTER + 0.3);
    pet.mote_serial = 0;
    pet.note_peek(t, 22.0, 4.0);
    assert!(pet.pending_bat.is_some(), "in range: the bat latches");
    let (mut crouched, mut leapt, mut swiped, mut dusted) = (false, false, false, false);
    for _ in 0..150 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 14))));
        match f.action {
            PetAction::Crouch => crouched = true,
            PetAction::Leap => leapt = true,
            PetAction::Frolic if f.pose == PetGlyphId::PetBat => {
                swiped = true;
                if f.motes
                    .iter()
                    .flatten()
                    .any(|m| m.kind == PetMoteKind::Dust)
                {
                    dusted = true;
                }
            }
            _ => {}
        }
    }
    assert!(crouched && leapt, "travel to the standoff: crouch, fly");
    assert!(swiped, "the swipe: the bat pose, pinned for the hold");
    assert!(dusted, "with one neutral puff at the peek cell");
    let (_, f) = idle(&mut pet, t, (4, 14), 4.0);
    let w = art_cols(10, 20);
    assert!(
        (f.col - PetBrain::station(14, 100, w)).abs() <= ARRIVED + SIT_FRONT_GAP,
        "home by the caret, got col {}",
        f.col
    );
}

/// r29 K9-5, A BATTED PEEK COMES DOWN ON A ROW ([`OldPath::PlayLandsOffTheRows`]). A
/// word-cat's head lands 0.07 of a row above the cat's row (a peek is a point: its head's
/// centre), and the cat bats it: from its leap on, and home again by the caret on its paws,
/// the cat stands on the grid's row, where the head law reads it ([`PetBrain::head_target`]).
/// Negative control (the law reverted): it came down at the head's fractional row and kept it
/// home, the head law reading no body there, so windowed it rested by a pasted command's
/// prompt drawn whole with its box in the command's line.
#[test]
fn a_batted_peek_comes_down_on_a_row() {
    let rows_after_the_bat = |old: bool| -> Vec<f32> {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        pet.set_old_path(OldPath::PlayLandsOffTheRows, old);
        let t = awake(&mut pet, start, 4, 12);
        let (mut t, _) = idle(&mut pet, t, (4, 14), SIT_AFTER + 0.3);
        pet.mote_serial = 0;
        pet.note_peek(t, 22.0, 3.93);
        assert!(
            pet.pending_bat.is_some(),
            "fixture: in range, the bat latches"
        );
        let mut rows = Vec::new();
        let mut leapt = false;
        for _ in 0..400 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 14))));
            leapt |= f.action == PetAction::Leap;
            if leapt && pet.flight.is_none() {
                rows.push(pet.row);
            }
        }
        assert!(leapt, "fixture: the cat leapt to bat the peek");
        rows
    };
    let off = |rows: &[f32]| {
        rows.iter()
            .filter(|r| (*r - r.round()).abs() > 0.05)
            .count()
    };
    let now = rows_after_the_bat(false);
    assert!(
        !now.is_empty() && off(&now) == 0,
        "after the bat the cat stood off the grid's rows on {} of {} ticks: {:?}",
        off(&now),
        now.len(),
        now.last()
    );
    let old = rows_after_the_bat(true);
    assert!(
        off(&old) > 0,
        "negative control: the bat came down on the grid's row"
    );
}

#[test]
fn a_far_peek_is_ignored() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 12);
    let (mut t, _) = idle(&mut pet, t, (4, 14), SIT_AFTER + 0.3);
    pet.note_peek(t, 55.0, 4.0);
    assert!(
        pet.pending_bat.is_none(),
        "a far peek is ignored at note time"
    );
    for _ in 0..80 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 14))));
        assert!(
            !matches!(f.action, PetAction::Crouch | PetAction::Leap),
            "and nothing travels, got {:?}",
            f.action
        );
    }
}

#[test]
fn a_stale_peek_expires() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 3, 2);
    let (t, _) = type_run(&mut pet, t, 3, 4, 6, 0.045);
    pet.note_peek(t, pet.col + 5.0, 3.0);
    assert!(pet.pending_bat.is_some(), "fixture: latched in range");
    let (t, _) = type_run(&mut pet, t, 3, 10, 44, 0.045);
    assert!(
        pet.pending_bat.is_none(),
        "the head pulled back long ago: the latch expired mid-chase"
    );
    let mut t2 = t;
    let mut swiped = false;
    for _ in 0..120 {
        t2 += Duration::from_millis(16);
        let f = pet.tick(sense(t2, Some((3, 54))));
        swiped |= f.action == PetAction::Frolic;
    }
    assert!(!swiped, "no bat fires off a dead latch");
}

#[test]
fn work_outranks_the_bat() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 12);
    let (mut t, _) = idle(&mut pet, t, (4, 14), SIT_AFTER + 0.3);
    pet.note_peek(t, 22.0, 4.0);
    let mut first_land: Option<f32> = None;
    let mut swiped_after_land = false;
    for _ in 0..250 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 36))));
        if f.action == PetAction::Land && first_land.is_none() {
            first_land = Some(f.col);
        }
        if f.action == PetAction::Frolic && f.pose == PetGlyphId::PetBat && first_land.is_some() {
            swiped_after_land = true;
        }
    }
    let w = art_cols(10, 20);
    let station = PetBrain::station(36, 100, w);
    let land = first_land.expect("the caret jump must fly");
    assert!(
        (land - station).abs() < 6.0,
        "the FIRST flight serves the caret (landed {land}, station {station}), \
         never the toy"
    );
    assert!(
        swiped_after_land,
        "and the bat still fires after the work, inside its TTL"
    );
}

#[test]
fn a_debounced_park_swaps_in_place_and_a_departing_body_runs_off() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 48);
    assert_eq!(
        pet.sync_look((3, 1), PetArrival::Ceremony).worn,
        (3, 1),
        "the first dress applies"
    );
    let (mut t, _) = idle(&mut pet, t, (4, 50), 0.2);
    assert_eq!(
        pet.sync_look((9, 4), PetArrival::Ceremony).worn,
        (3, 1),
        "fixture: the repoint parks mid-appearance"
    );
    let w = art_cols(10, 20);
    let station = PetBrain::station(50, 100, w);
    let mut ghost_frames = 0u32;
    let mut ghost_first: Option<f32> = None;
    let mut ghost_last = 0.0f32;
    let mut swap_frame_moved = false;
    let mut swapped = false;
    let mut prev_col = None::<f32>;
    let mut last_departures_live = false;
    let mut swap_at: Option<Instant> = None;
    let mut prev_alpha = 255u8;
    let mut ramp_frames = 0u32;
    for _ in 0..600 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 50))));
        let worn = pet.sync_look((9, 4), PetArrival::Ceremony).worn; // the host's per-frame re-sync
        if swap_at.is_none() && worn == (9, 4) {
            swap_at = Some(t);
            assert!(
                f.alpha > 0 && f.alpha < 64,
                "the incoming kitty starts faint — but ON glass — at the swap ({})",
                f.alpha
            );
        }
        match swap_at {
            None => assert_eq!(f.alpha, 255, "before the swap: fully there"),
            Some(at) if at == t => {}
            Some(at) if t.duration_since(at).as_secs_f32() <= ARRIVE_IN => {
                assert!(f.alpha >= prev_alpha, "the arrival only ever fades UP");
                ramp_frames += 1;
            }
            Some(_) => assert_eq!(f.alpha, 255, "after the ramp: fully there"),
        }
        prev_alpha = f.alpha;
        assert!(
            (f.col - station).abs() < 4.0,
            "…and never leaves its station ({} vs {station})",
            f.col
        );
        let live: Vec<_> = f.departures.iter().flatten().collect();
        assert!(live.len() <= 1, "one swap spawns exactly one ghost");
        if let Some(d) = live.first() {
            ghost_frames += 1;
            assert_eq!(
                (d.coat, d.iris),
                (3, 1),
                "the ghost wears the OUTGOING look"
            );
            assert_eq!(
                worn,
                (9, 4),
                "…while the live pet already wears the incoming one"
            );
            if ghost_first.is_none() {
                ghost_first = Some(d.col);
                if let Some(p) = prev_col {
                    swap_frame_moved = (f.col - p).abs() > 0.01;
                }
            }
            ghost_last = d.col;
        }
        swapped |= worn == (9, 4);
        prev_col = Some(f.col);
        last_departures_live = f.departures.iter().any(Option::is_some);
    }
    assert!(swapped, "the debounced park lands the new look");
    assert!(!swap_frame_moved, "the swap never moves the live pet");
    assert!(
        ramp_frames >= (ARRIVE_IN / 0.016) as u32 - 2,
        "the arrival ramp ran its whole length ({ramp_frames} frames)"
    );
    let first = ghost_first.expect("the swap spawned a departing body");
    assert!(
        ghost_last > first + 10.0,
        "the ghost genuinely ran toward its edge ({first} → {ghost_last})"
    );
    assert!(
        ghost_frames >= 30 && ghost_frames <= (DEPART_MAX / 0.016) as u32 + 4,
        "the visit is brief and strictly bounded ({ghost_frames} frames)"
    );
    assert!(
        !last_departures_live,
        "every departing body has fully retired by the end"
    );
}

#[test]
fn the_departing_ghost_leaves_at_full_opacity() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 48);
    assert_eq!(
        pet.sync_look((3, 1), PetArrival::Ceremony).worn,
        (3, 1),
        "the first dress applies"
    );
    let (mut t, _) = idle(&mut pet, t, (4, 50), 0.2);
    assert_eq!(
        pet.sync_look((9, 4), PetArrival::Ceremony).worn,
        (3, 1),
        "fixture: the repoint parks"
    );
    for _ in 0..600 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 50))));
        if pet.sync_look((9, 4), PetArrival::Ceremony).worn == (9, 4) {
            let d = f
                .departures
                .iter()
                .flatten()
                .next()
                .expect("the swap spawned its ghost on the same tick");
            assert_eq!(
                f.alpha,
                (ARRIVE_FLOOR * 255.0) as u8,
                "the incoming body lands at the arrival floor"
            );
            assert_eq!(f.lane_alpha, 255, "…while the LANE is at full presence");
            let composed = u16::from(d.alpha) * u16::from(f.lane_alpha) / 255;
            assert!(
                composed >= 250,
                "the ghost leaves at full opacity through the emitter's \
                 own multiply ({composed})"
            );
            return;
        }
    }
    panic!("the debounced park never landed");
}

#[test]
fn a_quiet_redress_never_flips_the_cat() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 48);
    assert_eq!(
        pet.sync_look((3, 1), PetArrival::Ceremony).worn,
        (3, 1),
        "the first dress applies"
    );
    let (mut t, f) = idle(&mut pet, t, (4, 30), 2.0);
    assert!(f.facing_left, "fixture: the pet must be facing left");
    assert!(f.action.settled(), "fixture: …and settled");
    let out = pet.sync_look((9, 4), PetArrival::Quiet);
    assert_eq!(out.worn, (3, 1), "fixture: the homecoming parks");
    assert!(out.parked, "…and reports the genuine park");
    let mut prev_pose = f.pose;
    let mut prev_facing = f.facing_left;
    let mut worn_pose: Option<PetGlyphId> = None;
    let mut swap_col = 0.0f32;
    let mut swapped = false;
    let mut ghost_frames = 0u32;
    for _ in 0..600 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 30))));
        let out = pet.sync_look((9, 4), PetArrival::Quiet);
        if !swapped && out.worn == (9, 4) {
            swapped = true;
            swap_col = f.col;
            assert!(prev_facing, "fixture: still facing left at the swap");
            worn_pose = Some(prev_pose);
        }
        if let Some(d) = f.departures.iter().flatten().next() {
            ghost_frames += 1;
            assert!(
                d.facing_left,
                "the zero-span ghost holds the live pet's facing — \
                 never the run-derived dir"
            );
            assert_eq!(
                d.pose,
                worn_pose.expect("no ghost before the swap"),
                "…and the cached pose, for its whole fade"
            );
            assert!(
                (d.col - swap_col).abs() < 1e-3,
                "…without ever moving ({} vs {swap_col})",
                d.col
            );
        }
        if !swapped {
            prev_pose = f.pose;
            prev_facing = f.facing_left;
        }
    }
    assert!(swapped, "the debounced park landed");
    let want = (EDGE_FADE / 0.016) as u32;
    assert!(
        ghost_frames >= want - 2 && ghost_frames <= want + 2,
        "the still ghost held for the whole 0.25 s ({ghost_frames} frames)"
    );
}

#[test]
fn a_hidden_caret_fades_the_crossfade_with_the_body_instead_of_cutting_it() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 48);
    assert_eq!(
        pet.sync_look((3, 1), PetArrival::Ceremony).worn,
        (3, 1),
        "the first dress applies"
    );
    let (mut t, _) = idle(&mut pet, t, (4, 50), 0.2);
    assert_eq!(
        pet.sync_look((9, 4), PetArrival::Quiet).worn,
        (3, 1),
        "fixture: the homecoming parks"
    );
    let mut born_life = None;
    for _ in 0..600 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 50))));
        if pet.sync_look((9, 4), PetArrival::Quiet).worn == (9, 4) {
            assert_eq!(f.alpha, (ARRIVE_FLOOR * 255.0) as u8);
            born_life = pet.departures[0].map(|d| (d.born, d.life()));
            break;
        }
    }
    let (born, life) = born_life.expect("the debounced park landed with its ghost");
    let mut prev_lane = 255u8;
    let mut ticks = 0u32;
    loop {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, None));
        ticks += 1;
        if f.lane_alpha == 0 {
            break;
        }
        assert!(f.lane_alpha <= prev_lane, "the lane only fades down");
        prev_lane = f.lane_alpha;
        let age = (pet.lane_clock - born) as f32;
        if age < life - 0.02 {
            let g = f.departures[0].unwrap_or_else(|| {
                panic!(
                    "tick {ticks}: the ghost (age {age:.3} of {life:.3} s) was dropped while \
                     the body is still on glass at lane {}",
                    f.lane_alpha
                )
            });
            assert!(g.alpha > 0, "a live ghost has a byte");
        }
        if ticks <= 12 {
            assert!(
                pet.arrive_t > 0.0 && f.alpha < f.lane_alpha,
                "tick {ticks}: the arrival ramp was snapped to full ({} of lane {}) under a \
                 fading body",
                f.alpha,
                f.lane_alpha
            );
        }
        assert!(ticks < 60, "FADE_OUT is 0.45 s");
    }
    assert!(
        pet.departures.iter().all(Option::is_none) && pet.arrive_t == 0.0,
        "the lane is emptied and the ramp retired once the body is at zero"
    );
}

#[test]
fn a_flight_lands_on_wall_time_through_synchronized_output_holds() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 4, 20);

    pet.begin_arc(40.0, 4.0, 0.45, 1.0, FlightKind::PLAIN);
    let dur = pet.flight.expect("fixture: airborne").dur;

    let mut waited = 0.0f32;
    for _ in 0..40 {
        t += Duration::from_millis(150);
        waited += 0.150;
        let _ = pet.tick(sense(t, Some((4, 20))));
        if pet.flight.is_none() {
            break;
        }
    }

    assert!(
        pet.flight.is_none(),
        "the flight never completed: {waited:.2}s of wall time elapsed \
         against an authored {dur:.2}s duration"
    );
    assert!(
        waited <= dur + 0.16,
        "the arc fell behind wall time: took {waited:.2}s to fly {dur:.2}s \
         (a per-bracket deficit that compounds)"
    );
}

#[test]
fn an_agreeing_re_sync_cannot_downgrade_a_parked_ceremony() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 48);
    assert_eq!(pet.sync_look((3, 1), PetArrival::Ceremony).worn, (3, 1));
    let (mut t, _) = idle(&mut pet, t, (4, 50), 0.2);

    let out = pet.sync_look((9, 4), PetArrival::Ceremony);
    assert!(out.parked, "fixture: the stranger parks");

    let mut ghost_first: Option<f32> = None;
    let mut ghost_last = 0.0f32;
    for _ in 0..600 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 50))));
        let _ = pet.sync_look((9, 4), PetArrival::Quiet);
        if let Some(d) = f.departures.iter().flatten().next() {
            ghost_first.get_or_insert(d.col);
            ghost_last = d.col;
        }
    }
    let first = ghost_first.expect("the ceremony spawned a departing body");
    assert!(
        ghost_last > first + 10.0,
        "the ceremony survived the Quiet re-syncs and RAN \
         ({first} -> {ghost_last}); a downgrade leaves a zero-span fade"
    );
}

#[test]
fn a_ghost_reaches_its_door_on_a_wide_pane() {
    let wide = |now: Instant, caret: Option<(u16, u16)>| PetSense {
        caret_drawn: true,
        cols: 200,
        ..sense(now, caret)
    };
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = start;
    let _ = pet.tick(wide(t, Some((4, 8))));
    for k in 1..=2u16 {
        let until = t + Duration::from_secs_f32(0.05);
        while t < until {
            t += Duration::from_millis(16);
            let _ = pet.tick(wide(t, Some((4, 8 + k - 1))));
        }
        t = until;
        let _ = pet.tick(wide(t, Some((4, 8 + k))));
    }
    let settle = t + Duration::from_secs_f32(SIT_AFTER + 0.2);
    while t < settle {
        t += Duration::from_millis(16);
        let _ = pet.tick(wide(t, Some((4, 10))));
    }
    assert_eq!(
        pet.sync_look((3, 1), PetArrival::Ceremony).worn,
        (3, 1),
        "the first dress applies"
    );
    let hold = t + Duration::from_secs_f32(0.2);
    while t < hold {
        t += Duration::from_millis(16);
        let _ = pet.tick(wide(t, Some((4, 10))));
    }
    assert_eq!(
        pet.sync_look((9, 4), PetArrival::Ceremony).worn,
        (3, 1),
        "fixture: the repoint parks"
    );
    let w = art_cols(10, 20);
    let door = 200.0 - w;
    let mut first: Option<f32> = None;
    let mut last: Option<f32> = None;
    for _ in 0..600 {
        t += Duration::from_millis(16);
        let f = pet.tick(wide(t, Some((4, 10))));
        let _ = pet.sync_look((9, 4), PetArrival::Ceremony).worn;
        if let Some(d) = f.departures.iter().flatten().next() {
            first.get_or_insert(d.col);
            last = Some(d.col);
        }
    }
    let first = first.expect("the swap spawned a departing body");
    assert!(
        first < 20.0,
        "the ghost was born at the pet's feet by the caret ({first})"
    );
    let last = last.expect("the ghost lived at least one frame");
    assert!(
        (last - door).abs() < 0.5,
        "the ghost's last live frame is AT its door, not a mid-screen \
         dissolve ({last} vs {door})"
    );
}

#[test]
fn one_long_frame_cannot_move_a_ghost_a_screen() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 48);
    assert_eq!(pet.sync_look((3, 1), PetArrival::Ceremony).worn, (3, 1));
    let (mut t, _) = idle(&mut pet, t, (4, 50), 0.2);
    assert_eq!(
        pet.sync_look((9, 4), PetArrival::Ceremony).worn,
        (3, 1),
        "fixture: the repoint parks"
    );
    let mut before: Option<f32> = None;
    for _ in 0..600 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 50))));
        let _ = pet.sync_look((9, 4), PetArrival::Ceremony).worn;
        if let Some(d) = f.departures.iter().flatten().next() {
            before = Some(d.col);
            break;
        }
    }
    let before = before.expect("the swap spawned a departing body");
    t += Duration::from_secs(5);
    let f = pet.tick(sense(t, Some((4, 50))));
    let d = f.departures.iter().flatten().next().expect(
        "the ghost survives the stall — the lane aged one clamped \
         frame, not five seconds",
    );
    let moved = d.col - before;
    assert!(moved >= 0.0, "the lane never runs backwards ({moved})");
    assert!(
        moved <= DEPART_SPEED * 0.10 + 1e-3,
        "one long frame moves a ghost at most speed × 0.10 cells ({moved})"
    );
}

#[test]
fn a_round_trip_flip_never_walks() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 48);
    assert_eq!(pet.sync_look((3, 1), PetArrival::Ceremony).worn, (3, 1));
    let (mut t, _) = idle(&mut pet, t, (4, 50), 0.2);
    let _ = pet.sync_look((9, 4), PetArrival::Ceremony).worn;
    let (t2, _) = idle(&mut pet, t, (4, 50), 1.0);
    t = t2;
    let _ = pet.sync_look((3, 1), PetArrival::Ceremony).worn;
    let w = art_cols(10, 20);
    let station = PetBrain::station(50, 100, w);
    for _ in 0..400 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 50))));
        assert!(
            (f.col - station).abs() < 4.0,
            "a round-trip flip must never march the pet off ({} vs {station})",
            f.col
        );
        assert_eq!(f.alpha, 255, "and never fades it");
        assert!(
            f.departures.iter().all(Option::is_none),
            "…and never spawns a departing body"
        );
    }
}

#[test]
fn a_caret_jump_is_served_before_the_swap() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 10);
    assert_eq!(pet.sync_look((3, 1), PetArrival::Ceremony).worn, (3, 1));
    let (mut t, _) = idle(&mut pet, t, (4, 12), 0.2);
    assert_eq!(
        pet.sync_look((9, 4), PetArrival::Ceremony).worn,
        (3, 1),
        "fixture: the repoint parks"
    );
    let (t2, _) = idle(&mut pet, t, (4, 12), HANDOFF_DEBOUNCE - 0.3);
    t = t2;
    let mut saw_flight = false;
    let mut ghost_after_flight = false;
    for _ in 0..400 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 90))));
        let worn = pet.sync_look((9, 4), PetArrival::Ceremony).worn;
        if f.action.airborne() {
            saw_flight = true;
            assert_eq!(worn, (3, 1), "no swap mid-air — work first, always");
            assert!(
                f.departures.iter().all(Option::is_none),
                "and no ghost spawns mid-air"
            );
        }
        if let Some(d) = f.departures.iter().flatten().next() {
            ghost_after_flight = true;
            assert!(saw_flight, "the ghost is born only after the trip");
            assert_eq!(worn, (9, 4), "the ghost's birth IS the swap");
            assert_eq!((d.coat, d.iris), (3, 1), "it wears the old look");
            assert!(
                d.col > 50.0,
                "…and spawned at the pet's NEW station, not the old one ({})",
                d.col
            );
        }
    }
    assert!(saw_flight, "fixture: the jump really flew the pet");
    assert!(ghost_after_flight, "the handoff fired at the next arrival");
}

#[test]
fn a_restarted_ramp_never_rewinds_an_adopted_stance() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 48);
    assert_eq!(pet.sync_look((3, 1), PetArrival::Quiet).worn, (3, 1));
    let (mut t, _) = idle(&mut pet, t, (4, 50), 0.2);
    pet.adopted_look = Some(AdoptedLook {
        pose: pet.last_pose,
        facing_left: pet.last_facing,
        stance: Stance {
            scale_x: 1.06,
            scale_y: 0.97,
            lift: 0.0,
            from: StretchFrom::Centre,
        },
        eased: 0.0,
        seat: (pet.col, pet.row),
        beneath: None,
    });
    pet.arrive_t = HOMECOMING_IN;
    pet.arrive_in = HOMECOMING_IN;
    pet.arrive_from = 1.0;
    assert!(pet.sync_look((9, 4), PetArrival::Quiet).parked, "fixture");
    pet.handoff_parked_clock = Some(f64::MAX);
    let px = |f: &PetFrame| f.body_px(10, 20, 100, 30).expect("on glass");
    let mut last: Option<(i32, i32, i32, i32)> = None;
    let mut handed_over_worn = false;
    for i in 0..40 {
        if i == 14 {
            pet.handoff_parked_clock = Some(pet.clock - f64::from(HANDOFF_DEBOUNCE));
        }
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 50))));
        if i == 14 {
            assert_eq!(pet.worn, Some((9, 4)), "fixture: the handoff landed");
            handed_over_worn = pet.adopted_look.is_some();
        }
        let now = px(&f);
        if let Some(was) = last {
            let step = [now.0 - was.0, now.1 - was.1, now.2 - was.2, now.3 - was.3];
            assert!(
                step.iter().all(|d| d.abs() <= 1),
                "frame {i}: the body snapped {was:?} -> {now:?} (pose {:?})",
                f.pose
            );
        }
        last = Some(now);
    }
    assert!(
        handed_over_worn,
        "fixture: the look was still worn on the handoff frame"
    );
}

#[test]
fn reduced_motion_swaps_instantly() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = start;
    for _ in 0..40 {
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((3, 10))));
    }
    assert_eq!(pet.sync_look((3, 1), PetArrival::Ceremony).worn, (3, 1));
    assert_eq!(
        pet.sync_look((9, 4), PetArrival::Ceremony).worn,
        (3, 1),
        "parks while visible"
    );
    t += Duration::from_millis(16);
    let mut s = sense(t, Some((3, 10)));
    s.reduced_motion = true;
    let f = pet.tick(s);
    assert!(
        matches!(f.action, PetAction::Sit | PetAction::Sleep),
        "no walk under reduced motion"
    );
    assert_eq!(
        pet.sync_look((9, 4), PetArrival::Ceremony).worn,
        (9, 4),
        "the parked look applied immediately — theater is motion, \
         the costume is state"
    );
    assert!(
        f.departures.iter().all(Option::is_none),
        "no departing body under reduced motion — departures are theater"
    );
}

#[test]
fn departures_cap_at_the_documented_bound_and_drop_the_oldest() {
    let mut pet = PetBrain::default();
    for k in 0..4u8 {
        pet.lane_clock = f64::from(k) * 0.1;
        pet.spawn_departure(Departure {
            born: pet.lane_clock,
            id: 0,
            coat: k,
            iris: 0,
            from_col: 10.0,
            row: 4.0,
            edge: 90.0,
            speed: DEPART_SPEED,
            still: false,
            facing_left: false,
            stride0: 0.0,
            pose0: PetGlyphId::PetSit,
            stance0: Stance::NEUTRAL,
            resight: false,
            own_glass: None,
            own_clear: false,
            peak: 1.0,
            colors: None,
            surface: None,
        });
    }
    let live: Vec<u8> = pet
        .resolve_departures()
        .iter()
        .flatten()
        .map(|d| d.coat)
        .collect();
    assert_eq!(live.len(), PET_DEPARTURES_MAX, "the cap holds");
    assert!(
        !live.contains(&0) && [1, 2, 3].iter().all(|c| live.contains(c)),
        "the OLDEST ghost was the one dropped ({live:?})"
    );
    pet.lane_clock += f64::from(DEPART_MAX) + 0.1;
    let gone = pet.resolve_departures();
    assert!(gone.iter().all(Option::is_none), "all visits expired");
    assert!(
        pet.departures.iter().all(Option::is_none),
        "…and their slots are freed, not parked"
    );
    assert!(
        !pet.needs_frames(),
        "no lingering animating=true once the ghosts are gone"
    );
}

fn warm_settled(pet: &mut PetBrain, start: Instant) -> Instant {
    let t = awake(pet, start, 4, 2);
    let (t, _) = type_run(pet, t, 4, 4, 40, 0.045);
    let (t, _) = idle(pet, t, (4, 44), SIT_AFTER + 0.3);
    assert!(
        pet.content() >= PLAY_CONTENT,
        "fixture: the run must have warmed the cat ({})",
        pet.content()
    );
    t
}

#[test]
fn the_gaze_follows_a_wandering_pointer() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 40);
    let (mut t, f) = idle(&mut pet, t, (4, 42), 0.1);
    assert_eq!(
        f.pose,
        PetGlyphId::PetSitFront,
        "fixture: eye contact at rest"
    );
    assert!(!f.facing_left, "fixture: the escort faces its travel");
    let mut px = 18.0f32;
    let mut frame = f;
    for k in 0..30 {
        t += Duration::from_millis(16);
        px += if k % 10 < 5 { -0.2 } else { 0.2 };
        frame = ptick(&mut pet, t, (4, 42), Some((px, 4.0)));
    }
    assert!(pet.pointer_heat > 0.0, "the wandering built attention");
    assert_eq!(
        frame.pose,
        PetGlyphId::PetPeekShoulder,
        "a far pointer behind the facing earns the over-the-shoulder peek"
    );
    for _ in 0..80 {
        t += Duration::from_millis(16);
        let _ = ptick(&mut pet, t, (4, 42), Some((px, 4.0)));
    }
    assert_eq!(pet.pointer_heat, 0.0, "a parked pointer is furniture");
    assert!(
        pet.sit_front && !pet.peek,
        "the gaze parks back on the caret (eye contact, no peek)"
    );
}

const SLEEPER_CARET: (u16, u16) = (4, 12);

fn a_sleeping_cat(hop_first: bool) -> (PetBrain, Instant, PetFrame) {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let w = art_cols(10, 20);
    let t = awake(&mut pet, start, SLEEPER_CARET.0, SLEEPER_CARET.1);
    let (mut t, mut f) = idle(&mut pet, t, SLEEPER_CARET, SLEEP_AFTER + 1.0);
    assert_eq!(f.action, PetAction::Sleep, "fixture: a sleeping cat");
    assert!(f.facing_left, "fixture: it dozed off facing LEFT");
    if hop_first {
        let inked_to = (f.col + w + 3.0) as u16;
        let spans = vec![(0u16, inked_to); 8];
        pet.sense_ink(0, &spans, Some(SLEEPER_CARET.0));
        for _ in 0..60 {
            t += Duration::from_millis(16);
            f = pet.tick(sense(t, Some(SLEEPER_CARET)));
        }
        assert_eq!(
            f.action,
            PetAction::Sleep,
            "fixture: the hop handed the sleep back"
        );
        assert!(!f.facing_left, "fixture: the hop left it facing RIGHT");
    }
    (pet, t, f)
}

fn swing_over_a_sleeper(hop_first: bool) -> (bool, u32, u32, u32) {
    let caret = SLEEPER_CARET;
    let w = art_cols(10, 20);
    let (mut pet, mut t, f) = a_sleeping_cat(hop_first);
    let (mid, row, facing0) = (f.col + w * 0.5, f.row, f.facing_left);
    let (mut flips, mut crossed, mut in_range) = (0u32, 0u32, 0u32);
    let (mut prev, mut side) = (facing0, 0.0f32);
    for i in 0..600u16 {
        let swing = 2.2 * (f32::from(i) * 0.016 * TAU * 1.5).sin();
        t += Duration::from_millis(16);
        let f = ptick(&mut pet, t, caret, Some((mid + swing, row)));
        assert_eq!(
            f.action,
            PetAction::Sleep,
            "fixture: a wandering pointer does not wake it"
        );
        if swing.signum() != side.signum() {
            crossed += 1;
            side = swing;
        }
        if pet.sit_front {
            in_range += 1;
        }
        if f.facing_left != prev {
            flips += 1;
            prev = f.facing_left;
        }
    }
    assert!(
        pet.pointer_heat > 0.0,
        "fixture: the swinging is fast enough to hold the pet's gaze"
    );
    (facing0, flips, crossed, in_range)
}

#[test]
fn a_cursor_swinging_over_a_sleeping_cat_never_flips_it() {
    for hop_first in [false, true] {
        let (facing0, flips, crossed, in_range) = swing_over_a_sleeper(hop_first);
        assert!(
            crossed >= 20,
            "fixture is inert — the pointer crossed the cat only {crossed} \
             times (facing_left {facing0})"
        );
        assert!(
            in_range >= 100,
            "fixture never reached eye-contact range ({in_range} frames, \
             facing_left {facing0}) — the turn branch was never entered"
        );
        assert_eq!(
            flips, 0,
            "the sleeping cat (facing_left {facing0}) turned over {flips} \
             times chasing the cursor"
        );
    }
}

/// Hovers a pointer `off` cells beside a sleeper for `secs`, bobbing a row either way twice
/// a second: a hand's wobble the gaze follows throughout (above [`GAZE_FOLLOW_SPEED_MIN`]).
fn hover_beside_a_sleeper(
    pet: &mut PetBrain,
    start: Instant,
    at: &PetFrame,
    off: f32,
    secs: f32,
) -> Option<u16> {
    let w = art_cols(10, 20);
    let px = at.col + w * 0.5 - 0.5 + off;
    let (mut t, mut turned) = (start, None);
    let frames = (secs / 0.016) as u16;
    for i in 0..frames {
        let py = at.row + (f32::from(i) * 0.016 * TAU * 2.0).sin();
        t += Duration::from_millis(16);
        let g = ptick(pet, t, SLEEPER_CARET, Some((px, py)));
        assert_eq!(g.action, PetAction::Sleep, "fixture: it stays asleep");
        if g.facing_left != at.facing_left && turned.is_none() {
            turned = Some(i);
        }
    }
    assert!(pet.pointer_heat > 0.0, "fixture: the pointer stayed live");
    turned
}

#[test]
fn a_cursor_resting_on_a_sleeping_cat_is_not_a_side_to_turn_to() {
    let (mut pet, t, f) = a_sleeping_cat(true);
    assert!(!f.facing_left, "fixture: asleep facing right");
    assert_eq!(
        hover_beside_a_sleeper(&mut pet, t, &f, -0.6, 9.6),
        None,
        "a cursor resting on the cat turned it over"
    );
}

#[test]
fn a_light_sleeper_comes_round_slowly_and_a_deep_one_not_at_all() {
    let (mut pet, t, f) = a_sleeping_cat(true);
    assert!(!f.facing_left, "fixture: asleep facing right");
    let turned = hover_beside_a_sleeper(&mut pet, t, &f, -1.6, 3.0)
        .expect("a light sleeper still notices what stays put");
    assert!(
        f32::from(turned) * 0.016 >= SLEEP_FLIP_DWELL,
        "it came round on frame {turned} — before the dwell was even paid"
    );

    let (mut pet, t, _) = a_sleeping_cat(true);
    let (t, f) = idle(&mut pet, t, SLEEPER_CARET, BREATH_WINDOW + 1.0);
    assert_eq!(f.action, PetAction::Sleep, "fixture: still asleep");
    assert!(
        pet.quiet >= SLEEP_AFTER + BREATH_WINDOW,
        "fixture: past the breath window (quiet {})",
        pet.quiet
    );
    assert!(!f.facing_left, "fixture: still facing right");
    assert_eq!(
        hover_beside_a_sleeper(&mut pet, t, &f, -1.6, 3.0),
        None,
        "a deeply asleep cat is perfectly still — it does not turn over"
    );
}

#[test]
fn a_sleeper_reads_deep_past_its_breath_window_and_again_once_a_focus_flap_faded_it_in() {
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, Instant::now(), SLEEPER_CARET.0, SLEEPER_CARET.1);
    let (t, f) = idle(&mut pet, t, SLEEPER_CARET, SLEEP_AFTER + 1.0);
    assert_eq!(f.action, PetAction::Sleep, "fixture: asleep");
    assert!(
        !pet.deep_asleep(),
        "a sleeper in its breath window read deep (quiet {})",
        pet.quiet
    );
    let (t, _) = idle(&mut pet, t, SLEEPER_CARET, BREATH_WINDOW + 1.0);
    assert!(
        pet.deep_asleep(),
        "a sleeper past its breath window read light (quiet {})",
        pet.quiet
    );
    pet.retire_coordinate_space();
    assert!(
        !pet.deep_asleep(),
        "a pet the lost focus retired read deep before its fade back in"
    );
    let (t, f) = idle(&mut pet, t, SLEEPER_CARET, FADE_IN / 2.0);
    assert_eq!(f.action, PetAction::Sleep, "the focus regained woke it");
    assert!(!pet.deep_asleep(), "a pet fading back in read deep");
    let (_, f) = idle(&mut pet, t, SLEEPER_CARET, FADE_IN);
    assert_eq!(f.action, PetAction::Sleep, "its fade woke it");
    assert!(
        pet.deep_asleep(),
        "a pet faded back in read light (quiet {}, alpha {})",
        pet.quiet,
        pet.alpha
    );
    // CONTROL: the same sleeper warmed by a pointer, whose gaze asks for frames as it cools.
    pet.pointer_heat = 0.5;
    assert!(!pet.deep_asleep(), "a sleeper a pointer warmed read deep");
    pet.pointer_heat = 0.0;
    // CONTROL: an awake cat at the same quiet (one held up, say waiting up with a job).
    pet.action = PetAction::Sit;
    assert!(
        !pet.deep_asleep(),
        "an awake cat past the breath window read deep"
    );
}

#[test]
fn the_gaze_brakes_never_reach_the_waking_chase() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 60);
    let (mut t, f) = idle(&mut pet, t, (4, 60), SLEEP_AFTER + 1.0);
    assert_eq!(f.action, PetAction::Sleep, "fixture: a sleeping cat");
    let asleep_facing = f.facing_left;
    let caret = if asleep_facing { 95 } else { 10 };
    let mut turned = None;
    for i in 0..180u16 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, caret))));
        if f.facing_left != asleep_facing && turned.is_none() {
            turned = Some((i, f.action));
        }
    }
    let (frame, action) = turned.expect("it must turn to chase a caret behind it");
    assert!(
        matches!(action, PetAction::Leap | PetAction::Crouch) || !action.settled(),
        "the turn came from travel, not from the settled gaze (got {action:?})"
    );
    assert!(
        frame < 120,
        "and it turned while getting under way, not {frame} frames later"
    );
}

fn dash(
    pet: &mut PetBrain,
    start: Instant,
    caret: (u16, u16),
    from: f32,
    to: f32,
) -> (Instant, bool, bool) {
    let mut t = start;
    let mut px = from;
    let (mut leapt, mut frolicked) = (false, false);
    while px > to {
        t += Duration::from_millis(16);
        px -= 1.0;
        let f = ptick(pet, t, caret, Some((px, f32::from(caret.0))));
        leapt |= f.action == PetAction::Leap;
        frolicked |= f.action == PetAction::Frolic;
    }
    (t, leapt, frolicked)
}

#[test]
fn a_dashing_pointer_gets_pounced_only_when_content() {
    let start = Instant::now();
    let mut cold = PetBrain::default();
    let t = awake(&mut cold, start, 4, 12);
    let (t, _) = idle(&mut cold, t, (4, 14), SIT_AFTER + 0.4);
    assert!(cold.content() < PLAY_CONTENT, "fixture: a cold cat");
    let (_, leapt, frolicked) = dash(&mut cold, t, (4, 14), 90.0, 5.0);
    assert!(!leapt && !frolicked, "a cold cat does not play");
    let mut warm = PetBrain::default();
    let t = warm_settled(&mut warm, start);
    let from = warm.col + 45.0;
    let (t, leapt, _) = dash(&mut warm, t, (4, 44), from, 5.0);
    assert!(leapt, "a happy cat pounces the dasher");
    let mut frolicked = false;
    let mut t2 = t;
    for _ in 0..200 {
        t2 += Duration::from_millis(16);
        let f = ptick(&mut warm, t2, (4, 44), Some((5.0, 4.0)));
        frolicked |= f.action == PetAction::Frolic;
    }
    assert!(frolicked, "and frolics one beat on the landing");
}

#[test]
fn the_pet_frolics_once_when_the_hand_finds_its_flow() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 20);
    let (mut t, f) = idle(&mut pet, t, (4, 22), SIT_AFTER + 0.2);
    assert!(f.action.settled(), "fixture: a settled, awake cat");
    assert!(
        f.motes.iter().all(Option::is_none),
        "fixture: no light on glass"
    );

    pet.note_flow(t);
    let (mut frolics, mut lit) = (0u32, 0u32);
    for _ in 0..40u16 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 22))));
        if f.action == PetAction::Frolic {
            frolics += 1;
        }
        lit += f.motes.iter().flatten().count() as u32;
    }
    assert!(frolics > 0, "the cat notices the hand finding its flow");
    assert_eq!(lit, 0, "…and it is a POSE, not a light: {lit} motes flew");

    let settled = idle(&mut pet, t, (4, 22), FROLIC_HOLD + 1.0).0;
    let mut again = 0u32;
    let mut t2 = settled;
    for _ in 0..120u16 {
        t2 += Duration::from_millis(16);
        if pet.tick(sense(t2, Some((4, 22)))).action == PetAction::Frolic {
            again += 1;
        }
    }
    assert_eq!(again, 0, "one entry is one frolic, not a standing state");

    assert!(
        pet.pending_flow.is_none(),
        "the entry latch is spent, and no exit ever sets one"
    );

    let (mut pet, t, f) = a_sleeping_cat(false);
    assert_eq!(f.action, PetAction::Sleep, "fixture: asleep");
    let (mut t, _) = idle(&mut pet, t, SLEEPER_CARET, 0.2);
    pet.note_flow(t);
    for _ in 0..60u16 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some(SLEEPER_CARET)));
        assert_eq!(
            f.action,
            PetAction::Sleep,
            "flow never wakes a sleeping cat"
        );
    }

    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 20);
    let (t, _) = idle(&mut pet, t, (4, 22), 0.2);
    pet.note_flow(t);
    assert!(pet.pending_flow.is_some(), "fixture: the latch is live");
    assert!(
        !pet.needs_frames(),
        "a flow latch buys NO frame — it rides the cadence or expires"
    );
    let (t, _) = idle(&mut pet, t, (4, 22), FLOW_LATCH_TTL + 0.2);
    assert!(pet.pending_flow.is_none(), "an unconsumed latch expires");
    let (_, f) = idle(&mut pet, t, (4, 22), SLEEP_AFTER + BREATH_WINDOW + 1.0);
    assert_eq!(f.action, PetAction::Sleep, "and the cat still settles");
    assert!(
        f.motes.iter().all(Option::is_none) && !pet.needs_frames(),
        "…to exactly zero light, asking for no further frame"
    );
}

#[test]
fn the_cat_walks_home_after_playing() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = warm_settled(&mut pet, start);
    let from = pet.col + 45.0;
    let (t, leapt, _) = dash(&mut pet, t, (4, 44), from, 20.0);
    assert!(leapt, "fixture: the pounce fired");
    let (t, f) = idle(&mut pet, t, (4, 44), 3.5);
    let w = art_cols(10, 20);
    let station = PetBrain::station(44, 100, w);
    assert!(
        (f.col - station).abs() <= ARRIVED + SIT_FRONT_GAP,
        "home by the caret again, got col {} (station {station})",
        f.col
    );
    assert!(f.action.settled(), "and settled, got {:?}", f.action);
    let mut t = t;
    let mut acted = false;
    for _ in 0..600 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 44))));
        acted |= matches!(f.action, PetAction::Frolic | PetAction::Loaf) || pet.wriggle_t > 0.0;
    }
    assert!(acted, "a bored content cat demands attention");
    assert!(pet.bored_cool > 0.0, "and pays the cooldown for it");
}

#[test]
fn caret_work_outranks_pointer_play() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = warm_settled(&mut pet, start);
    pet.pending_pointer_pounce = Some((60.0, 4.0));
    t += Duration::from_millis(16);
    let f = ptick(&mut pet, t, (4, 44), Some((60.0, 4.0)));
    assert_eq!(
        f.action,
        PetAction::Crouch,
        "fixture: the play crouch began"
    );
    let mut frolicked = false;
    for _ in 0..250 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 70))));
        frolicked |= f.action == PetAction::Frolic;
    }
    assert!(
        !frolicked,
        "the caret jump cleared the flourish: work first, no play beat"
    );
    let (_, f) = idle(&mut pet, t, (4, 70), 1.0);
    let w = art_cols(10, 20);
    let station = PetBrain::station(70, 100, w);
    assert!(
        (f.col - station).abs() <= ARRIVED + 0.5,
        "and the pet is at the CARET's station, got {} ({station})",
        f.col
    );
}

#[test]
fn one_pounce_per_dash() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = warm_settled(&mut pet, start);
    let mut px = pet.col + 45.0;
    let mut t = t;
    let mut crouches = 0u32;
    let mut last = pet.action();
    for _ in 0..300 {
        t += Duration::from_millis(16);
        px = (px - 1.0).max(2.0);
        let f = ptick(&mut pet, t, (4, 44), Some((px, 4.0)));
        if f.action == PetAction::Crouch && last != PetAction::Crouch {
            crouches += 1;
        }
        last = f.action;
    }
    assert_eq!(
        crouches, 1,
        "one pounce per dash: the trigger re-arms only under POUNCE_REARM"
    );
}

#[test]
fn pointer_none_clears_heat() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 12);
    let (mut t, _) = idle(&mut pet, t, (4, 14), SIT_AFTER + 0.4);
    let mut px = 30.0f32;
    for _ in 0..40 {
        t += Duration::from_millis(16);
        px += 0.2;
        let _ = ptick(&mut pet, t, (4, 14), Some((px, 4.0)));
    }
    assert!(pet.pointer_heat > 0.0, "fixture: attention is live");
    t += Duration::from_millis(16);
    let _ = ptick(&mut pet, t, (4, 14), None);
    assert_eq!(
        pet.pointer_heat, 0.0,
        "outside the pane the pointer does not exist"
    );
    assert!(pet.last_pointer.is_none(), "and the sensor forgets it");
}

const fn on_ink(f: &PetFrame, span: (u16, u16), w: f32) -> bool {
    let (first, end) = (span.0 as f32, span.1 as f32);
    let (a, b) = (f.col + INK_PAD, f.col + w - INK_PAD);
    b > first && a < end
}

#[test]
fn the_sleeper_follows_the_prompt() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let w = art_cols(10, 20);
    let t = awake(&mut pet, start, 2, 8);
    let (mut t, f) = idle(&mut pet, t, (2, 10), SLEEP_AFTER + 1.0);
    assert_eq!(f.action, PetAction::Sleep, "fixture: a sleeping cat");
    let inked_to = (f.col + w + 2.0) as u16;
    let spans = vec![(0u16, inked_to); 6];
    pet.sense_ink(0, &spans, Some(2));
    t += Duration::from_millis(16);
    pet.note_console_input(t, PetInputKind::Text);
    let f1 = pet.tick(sense(t, Some((2, inked_to))));
    assert_eq!(
        pet.resume.map(|(a, _)| a),
        Some(PetAction::Waking),
        "typing wakes it, and the hop owes the stretch back"
    );
    assert_eq!(
        f1.action,
        PetAction::Leap,
        "and it is already off the words"
    );
    let mut stretched_clear = false;
    for _ in 0..90 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((2, inked_to))));
        if f.action == PetAction::Waking {
            assert!(
                !on_ink(&f, (0, inked_to), w),
                "the stretch plays OFF the ink, not atop the fresh words \
                 (col {}, ink to {inked_to})",
                f.col
            );
            stretched_clear = true;
        }
    }
    assert!(stretched_clear, "the borrowed stretch must actually play");
}

#[test]
fn a_deep_sleep_re_anchor_hops_then_gives_the_lane_back() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let w = art_cols(10, 20);
    let t = awake(&mut pet, start, 3, 10);
    let (mut t, f) = idle(&mut pet, t, (3, 12), SLEEP_AFTER + BREATH_WINDOW + 2.0);
    assert_eq!(f.action, PetAction::Sleep);
    assert!(!pet.needs_frames(), "fixture: deep sleep, lane released");
    let inked_to = (f.col + w + 4.0) as u16;
    let spans = vec![(0u16, inked_to); 6];
    pet.sense_ink(0, &spans, Some(3));
    t += Duration::from_millis(16);
    let f2 = pet.tick(sense(t, Some((3, 12))));
    assert_eq!(
        f2.action,
        PetAction::Leap,
        "a shoved cat gets up, got {:?}",
        f2.action
    );
    assert!(pet.needs_frames(), "and the hop is owed frames to draw it");
    let (_, f3) = idle(&mut pet, t, (3, 12), 1.5);
    assert_eq!(f3.action, PetAction::Sleep, "asleep again");
    assert!(
        !on_ink(&f3, (0, inked_to), w),
        "re-anchored off the ink (col {})",
        f3.col
    );
    assert!(
        !pet.needs_frames(),
        "and the lane is released again once the paws are down"
    );
}

#[test]
fn a_growing_line_never_tows_the_cat_along_behind_it() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 4, 12);
    let mut spans = vec![(0u16, 0u16); 8];
    spans[4] = (0, 30);
    pet.sense_ink(0, &spans, Some(4));
    let (t2, settled) = idle(&mut pet, t, (4, 12), 1.2);
    t = t2;
    assert!(
        settled.col < 20.0,
        "the cat stations beside the CARET, not past the end of the line \
         (col {}, line ends at 30)",
        settled.col
    );
    assert!(
        (settled.row - 5.0).abs() < 0.01,
        "and it takes the blank row under the sentence, got row {}",
        settled.row
    );

    let anchor = settled.col;
    for end in [31u16, 32, 33] {
        spans[4] = (0, end);
        pet.sense_ink(0, &spans, Some(4));
        let (t3, f) = idle(&mut pet, t, (4, 12), 0.5);
        t = t3;
        assert!(
            (f.col - anchor).abs() < 0.5,
            "a longer line must not move a cat whose caret stood still \
             (col {} vs {anchor} when the line reached {end})",
            f.col
        );
    }
}

#[test]
fn the_ink_ladder_prefers_a_blank_row_to_a_distant_stand() {
    let mut pet = PetBrain::default();
    let w = art_cols(10, 20);
    let mut spans = vec![(0u16, 0u16); 8];
    spans[4] = (0, 30);
    pet.sense_ink(0, &spans, Some(4));
    assert_eq!(
        pet.ink_stand(13.0, 4.0, w, 100, 30),
        (13.0, 5.0),
        "a blank row below beats a stand 17 cells away"
    );
    spans[4] = (0, 14);
    pet.sense_ink(0, &spans, Some(4));
    assert_eq!(
        pet.ink_stand(13.0, 4.0, w, 100, 30),
        (14.0 + INK_GAP, 4.0),
        "a near stand is cheaper than a row change"
    );
    let spans = vec![(0u16, 30u16); 8];
    pet.sense_ink(0, &spans, Some(4));
    assert_eq!(
        pet.ink_stand(13.0, 4.0, w, 100, 30),
        (30.0 + INK_GAP, 4.0),
        "a full screen still evicts beside the paragraph"
    );
    assert_eq!(
        pet.ink_stand(40.0, 4.0, w, 100, 30),
        (40.0, 4.0),
        "blank ground under the want is answered first and alone"
    );
}

#[test]
fn sparse_ink_refinement_keeps_real_glyphs_and_unknown_coverage_obstructed() {
    use crate::pet_world::{PetPane, PetWorldFacts};
    use aterm_core::terminal::Terminal;

    let mut term = Terminal::new(8, 100);
    term.process(b"\x1b[5;1Hx\x1b[5;100Hx\x1b[5;13H");
    let mut pet = PetBrain::default();
    let width = art_cols(10, 20);
    let spans = vec![(0, 100); 8];
    pet.sense_ink(0, &spans, Some(4));
    assert!(
        pet.ink_overlaps(13.0, 4.0, width),
        "unobserved hull stays conservative"
    );

    let input = term.cell_frame(8, 100);
    let facts = PetWorldFacts::read(&term, 7);
    pet.observe_console(&input, &facts, PetPane::full(&input));
    pet.set_console_presentable(true);
    assert!(
        !pet.ink_overlaps(13.0, 4.0, width),
        "observed spaces are not glyphs"
    );

    term.process(b"\x1b[5;16Hx\x1b[5;13H");
    let input = term.cell_frame(8, 100);
    let facts = PetWorldFacts::read(&term, 7);
    pet.observe_console(&input, &facts, PetPane::full(&input));
    assert!(
        pet.ink_overlaps(13.0, 4.0, width),
        "actual glyph still obstructs the body"
    );
    let beside = pet.ink_safe_col(13.0, 4.0, width, 100);
    assert!((beside - 13.0).abs() <= INK_EVICT_MAX);
    assert!(!pet.ink_overlaps(beside, 4.0, width));

    let mut pane = PetPane::full(&input);
    pane.cols = 8;
    pet.observe_console(&input, &facts, pane);
    assert_eq!(pet.observed_ink_overlaps(13.0, 4.0, width), None);
    assert!(pet.ink_overlaps(13.0, 4.0, width));

    pet.observe_console(&input, &facts, PetPane::full(&input));
    pet.sense_ink(0, &[], None);
    assert!(
        !pet.ink_overlaps(13.0, 4.0, width),
        "unfed ink seam stays inert"
    );
}

#[test]
fn the_exact_ink_map_reads_the_columns_and_feet_row_the_body_covers() {
    use crate::pet_world::{PetPane, PetWorldFacts};
    use aterm_core::terminal::Terminal;

    let width = art_cols(10, 20);
    assert!(
        (13.0 + INK_PAD - 13.3).abs() < 1e-4 && (13.0 + width - INK_PAD - 18.3).abs() < 0.01,
        "the fixture's shaved footprint is 13.3..18.3: width {width}, pad {INK_PAD}"
    );
    let overlaps = |(row, col): (u16, u16), body: f32, feet: f32, presentable: bool| {
        let mut term = Terminal::new(8, 100);
        term.process(format!("\x1b[{};{}Hx\x1b[8;100H", row + 1, col + 1).as_bytes());
        let mut pet = PetBrain::default();
        let input = term.cell_frame(8, 100);
        let facts = PetWorldFacts::read(&term, 7);
        pet.observe_console(&input, &facts, PetPane::full(&input));
        pet.set_console_presentable(presentable);
        pet.observed_ink_overlaps(body, feet, width)
    };
    let cases = [
        ("12 beside 13", (4, 12), 13.0, 4.0, Some(false)),
        ("13 under 13", (4, 13), 13.0, 4.0, Some(true)),
        ("18 under 13", (4, 18), 13.0, 4.0, Some(true)),
        ("19 beside 13", (4, 19), 13.0, 4.0, Some(false)),
        ("12 under 12.8's margin", (4, 12), 12.8, 4.0, Some(false)),
        ("12 under 12.6", (4, 12), 12.6, 4.0, Some(true)),
        ("19 under 13.5's margin", (4, 19), 13.5, 4.0, Some(false)),
        ("19 under 13.8", (4, 19), 13.8, 4.0, Some(true)),
        ("row 4, feet 4.4", (4, 13), 13.0, 4.4, Some(true)),
        ("row 5, feet 4.6", (5, 13), 13.0, 4.6, Some(true)),
        ("row 4, feet 4.6", (4, 13), 13.0, 4.6, Some(false)),
        ("row 0, feet 0", (0, 13), 13.0, 0.0, Some(true)),
    ];
    let got = cases.map(|(what, glyph, body, feet, _)| (what, overlaps(glyph, body, feet, true)));
    assert_eq!(
        got,
        cases.map(|(what, .., answer)| (what, answer)),
        "(the glyph's place, whether the body's ink map holds it)"
    );
    assert_eq!(
        overlaps((4, 13), 13.0, 4.0, false),
        None,
        "the same glyph under the same body, the console in another's custody"
    );
}

#[test]
fn an_unobserved_hull_obstructs_exactly_the_footprints_that_reach_it() {
    let width = art_cols(10, 20);
    assert!(
        (width - 2.0 * INK_PAD - 5.0).abs() < 0.01,
        "the fixture's shaved footprint is five columns: width {width}, pad {INK_PAD}"
    );
    let mut pet = PetBrain::default();
    let mut spans = vec![(0, 0); 8];
    spans[4] = (20, 40);
    pet.sense_ink(0, &spans, Some(4));
    let bodies = [13.0f32, 14.6, 14.8, 39.6, 39.8];
    assert_eq!(
        bodies.map(|col| (col, pet.ink_overlaps(col, 4.0, width))),
        [
            (13.0, false),
            (14.6, false),
            (14.8, true),
            (39.6, true),
            (39.8, false)
        ],
        "(the body's column, whether its footprint reaches the hull [20, 40) on row 4)"
    );
}

#[test]
fn the_caret_delta_folds_only_its_three_seams() {
    type Case = (
        &'static str,
        (u16, u16),
        (u16, u16),
        u16,
        bool,
        bool,
        bool,
        (f32, f32, bool),
    );
    #[rustfmt::skip]
    let cases: [Case; 21] = [
        ("typed past the margin", (5, 99), (6, 0), 100, false, true, false, (0.0, 1.0, true)),
        ("backspaced over the seam", (6, 0), (5, 99), 100, false, false, false, (0.0, -1.0, true)),
        ("a double-width wrap", (5, 98), (6, 0), 100, false, true, false, (0.0, 2.0, true)),
        ("a real row jump", (10, 5), (2, 70), 100, false, false, false, (-8.0, 65.0, false)),
        ("down AND right", (5, 2), (6, 70), 100, false, false, false, (1.0, 68.0, false)),
        ("a 15-cell move in a 20-wide pane", (5, 16), (6, 1), 20, false, true, false, (1.0, -15.0, false)),
        ("a scrolled wrap", (29, 99), (29, 0), 100, true, true, false, (1.0, 1.0, true)),
        ("Home at the last column", (29, 99), (29, 0), 100, false, true, false, (0.0, -99.0, false)),
        ("a mid-line Home in a wrapping frame", (29, 40), (29, 0), 100, true, true, false, (0.0, -40.0, false)),
        ("`ls -la` from the last row", (29, 8), (29, 0), 80, false, true, false, (1.0, -8.0, true)),
        ("…width-blind", (29, 8), (29, 0), 200, false, true, false, (1.0, -8.0, true)),
        ("…home at a double-width margin", (29, 13), (29, 2), 80, false, true, false, (1.0, -11.0, true)),
        ("…at the end of a wide prompt", (23, 20), (23, 12), 80, false, true, false, (1.0, -8.0, true)),
        ("…witnessed by the scroll", (29, 30), (29, 24), 100, false, false, true, (1.0, -6.0, true)),
        ("a backspace under STARTLE_COLS", (29, 27), (29, 24), 100, false, false, true, (0.0, -3.0, false)),
        ("…even onto the margin", (29, 3), (29, 0), 80, false, true, false, (0.0, -3.0, false)),
        ("SNAP_COLS and beyond", (29, 20), (29, 0), 80, false, true, false, (0.0, -20.0, false)),
        ("…the scroll does not widen it", (29, 40), (29, 24), 100, false, false, true, (0.0, -16.0, false)),
        ("a retreat that is not home", (29, 40), (29, 8), 80, false, false, false, (0.0, -32.0, false)),
        ("a mid-line deletion, no witness", (23, 20), (23, 12), 80, false, false, false, (0.0, -8.0, false)),
        ("forward is just typing", (29, 24), (29, 30), 100, false, false, true, (0.0, 6.0, false)),
    ];
    for (name, from, to, cols, wrapped, home, scrolled, want) in cases {
        assert_eq!(
            PetBrain::caret_delta(from, to, cols, wrapped, home, scrolled),
            want,
            "{name}"
        );
    }
}

/// The page's rise ([`page_rise`]): every lettered row up together, by the fewest rows; a
/// blank row may come up written; a map that kept its lettered rows, changed height or had
/// none to move did not rise.
#[test]
fn a_page_rises_by_the_rows_its_letters_went_up() {
    let page = [
        (0, 0),
        (0, 40),
        (2, 7),
        (0, 33),
        (0, 0),
        (1, 12),
        (0, 25),
        (0, 0),
    ];
    let up = |by: usize, foot: &[(u16, u16)]| {
        let mut v = page[by..].to_vec();
        v.extend_from_slice(foot);
        v
    };
    assert_eq!(page_rise(&page, &up(1, &[(0, 2)])), 1, "a line");
    assert_eq!(
        page_rise(&page, &up(2, &[(0, 17), (0, 2)])),
        2,
        "an Enter and its answer"
    );
    let mut answered = up(1, &[(0, 2)]);
    answered[6] = (0, 17);
    assert_eq!(
        page_rise(&page, &answered),
        1,
        "an answer printed on the blank row before it scrolled"
    );
    let mut written = page.to_vec();
    written[7] = (0, 9);
    assert_eq!(page_rise(&page, &written), 0, "writing on a blank row");
    let mut changed = up(1, &[(0, 2)]);
    changed[4] = (3, 9);
    assert_eq!(page_rise(&page, &changed), 0, "a lettered row rewritten");
    assert_eq!(page_rise(&page, &up(1, &[])), 0, "a map of another height");
    assert_eq!(page_rise(&[(0, 0); 8], &[(0, 0); 8]), 0, "nothing to move");
    let mut pet = PetBrain::default();
    pet.sense_ink(0, &page, Some(4));
    pet.sense_ink(0, &up(2, &[(0, 17), (0, 2)]), Some(4));
    assert_eq!(pet.ink_rose, 2);
    pet.sense_ink(0, &up(2, &[(0, 17), (0, 2)]), Some(4));
    assert_eq!(pet.ink_rose, 0, "read once: a still map rose none");
}

#[test]
fn the_ink_map_scroll_fact_is_only_true_for_a_one_row_scroll() {
    let up = |m: &[(u16, u16)]| {
        let mut v = m.to_vec();
        v.remove(0);
        v.push((0, 12));
        v
    };
    let page = [(0, 0), (0, 40), (0, 44), (0, 30), (0, 24)];

    let mut pet = PetBrain::default();
    pet.sense_ink(0, &page, Some(4));
    assert!(
        !pet.ink_scrolled,
        "the FIRST map has nothing to be a scroll of"
    );
    pet.sense_ink(0, &up(&page), Some(4));
    assert!(pet.ink_scrolled, "every inked row moved up exactly one");

    let mut pet = PetBrain::default();
    pet.sense_ink(0, &page, Some(4));
    pet.sense_ink(0, &page, Some(4));
    assert!(!pet.ink_scrolled, "a map that did not move is not a scroll");

    let mut pet = PetBrain::default();
    pet.sense_ink(0, &page, Some(4));
    pet.sense_ink(0, &up(&up(&page)), Some(4));
    assert!(!pet.ink_scrolled, "TWO rows is a jump, not a line");

    let mut pet = PetBrain::default();
    pet.sense_ink(0, &page, Some(4));
    pet.sense_ink(1, &up(&page), Some(5));
    assert!(
        !pet.ink_scrolled,
        "a moved window is scrollback, not a new line"
    );

    let mut pet = PetBrain::default();
    pet.sense_ink(0, &page, Some(4));
    pet.sense_ink(0, &up(&page)[1..], Some(4));
    assert!(
        !pet.ink_scrolled,
        "a resize rewrites the map, it does not shift it"
    );

    let blank = [(0u16, 0u16); 5];
    let mut pet = PetBrain::default();
    pet.sense_ink(0, &blank, Some(4));
    pet.sense_ink(0, &up(&blank), Some(4));
    assert!(
        !pet.ink_scrolled,
        "a blank pane has no content to have advanced"
    );

    let mut pet = PetBrain::default();
    pet.sense_ink(0, &page, Some(4));
    pet.retire_coordinate_space();
    pet.sense_ink(0, &up(&page), Some(4));
    assert!(!pet.ink_scrolled, "retirement clears the comparison buffer");
}

#[test]
fn a_prompt_row_that_inks_past_the_caret_still_gets_a_quiet_new_line() {
    let start = Instant::now();
    let run = |suggest: bool| -> (bool, bool) {
        let mut pet = PetBrain::default();
        let tail = u16::from(suggest) * 3;
        let mut spans: Vec<(u16, u16)> = vec![(0, 0); 30];
        for span in spans.iter_mut().take(29).skip(21) {
            *span = (0, 46);
        }
        spans[29] = (0, 24 + tail);
        pet.sense_ink(0, &spans, Some(29));
        let (t, _) = type_run(&mut pet, start, 29, 24, 6, 0.06);
        spans[29] = (0, 30);
        pet.sense_ink(0, &spans, Some(29));
        let (mut t, f) = idle(&mut pet, t, (29, 30), SIT_AFTER + 0.3);
        assert!(f.action.settled(), "fixture: a settled cat");
        assert!(
            f.col > 30.0,
            "fixture: standing clear of the line's ink at {:.2}",
            f.col
        );
        for r in 0..29 {
            spans[r] = spans[r + 1];
        }
        spans[29] = (0, 24 + tail);
        let (mut startled, mut ran) = (false, false);
        for _ in 0..120 {
            t += Duration::from_millis(16);
            pet.sense_ink(0, &spans, Some(29));
            let f = pet.tick(sense(t, Some((29, 24))));
            startled |= f.action == PetAction::Startle;
            ran |= f.action == PetAction::Run;
        }
        (startled, ran)
    };
    assert_eq!(
        run(true),
        (false, false),
        "an autosuggestion does not make the shell's own new line a fright"
    );
    assert_eq!(
        run(false),
        (false, false),
        "…and the bare prompt it must stay equal to"
    );
}

#[test]
fn a_wide_prompt_still_leaves_the_caret_at_home() {
    let mut pet = PetBrain::default();
    assert!(pet.caret_at_home(23, 0));
    assert!(pet.caret_at_home(23, 2));
    assert!(!pet.caret_at_home(23, 12));
    pet.sense_ink(20, &[(0, 0), (0, 0), (0, 0), (0, 17)], Some(23));
    assert!(
        pet.caret_at_home(23, 18),
        "the caret sits past the prompt's trailing blank and is still home"
    );
    assert!(pet.caret_at_home(23, 17), "and hard against its last glyph");
    pet.sense_ink(20, &[(0, 0), (0, 0), (0, 0), (0, 60)], Some(23));
    assert!(
        !pet.caret_at_home(23, 18),
        "ink to the right of the caret is a line, not a prompt"
    );
    pet.sense_ink(20, &[(0, 0), (0, 0), (0, 0), (30, 42)], Some(23));
    assert!(!pet.caret_at_home(23, 42));
}

#[test]
fn a_wide_prompt_s_own_new_line_does_not_startle() {
    let start = Instant::now();
    let run = |inked: bool| -> bool {
        let mut pet = PetBrain::default();
        let mut spans = vec![(0u16, 0u16); 30];
        spans[29] = (0, 12);
        if inked {
            pet.sense_ink(0, &spans, Some(29));
        }
        let (t, _) = type_run(&mut pet, start, 29, 12, 8, 0.06);
        spans[29] = (0, 20);
        if inked {
            pet.sense_ink(0, &spans, Some(29));
        }
        let (mut t, f) = idle(&mut pet, t, (29, 20), SIT_AFTER + 0.3);
        assert!(
            f.action.settled(),
            "fixture: a settled cat beside the line's end"
        );
        assert!(
            f.col > 20.0,
            "fixture: and clear of the line's ink, col {}",
            f.col
        );
        spans[29] = (0, 12);
        if inked {
            pet.sense_ink(0, &spans, Some(29));
        }
        let mut startled = false;
        for _ in 0..90 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((29, 12))));
            startled |= f.action == PetAction::Startle;
        }
        startled
    };
    assert!(
        !run(true),
        "a twelve-column prompt's own new line is a line change, not a fright"
    );
    assert!(
        run(false),
        "fixture: without the ink seam the margin is all there is, so \
         the same Enter DOES still startle — the assertion above is \
         about the seam, not about the delta"
    );
}

#[test]
fn a_scrolled_wrap_is_a_wrap() {
    let script = |fact: bool| -> Vec<PetAction> {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 29, 90);
        let mut spans = vec![(0u16, 0u16); 30];
        spans[27] = (0, 100);
        spans[28] = (0, 100);
        let mut caret = 92u16;
        while caret < 99 {
            caret += 1;
            for _ in 0..3 {
                t += Duration::from_millis(16);
                spans[29] = (0, caret);
                pet.sense_ink(0, &spans, Some(29));
                let mut s = sense(t, Some((29, caret)));
                s.output_burst = true;
                let _ = pet.tick(s);
            }
        }
        for _ in 0..50 {
            t += Duration::from_millis(16);
            spans[29] = (0, 100);
            pet.sense_ink(0, &spans, Some(29));
            let mut s = sense(t, Some((29, 99)));
            s.output_burst = true;
            let _ = pet.tick(s);
        }
        let mut actions = Vec::new();
        t += Duration::from_millis(16);
        spans[28] = (0, 100);
        spans[29] = (0, 1);
        pet.sense_ink(0, &spans, Some(29));
        let mut s = sense(t, Some((29, 0)));
        s.output_burst = true;
        s.wrapped = fact;
        actions.push(pet.tick(s).action);
        for i in 1..=45u16 {
            t += Duration::from_millis(16);
            let col = (i / 3).clamp(1, 15);
            spans[29] = (0, col);
            pet.sense_ink(0, &spans, Some(29));
            let mut s = sense(t, Some((29, col)));
            s.output_burst = true;
            actions.push(pet.tick(s).action);
        }
        actions
    };

    let with_fact = script(true);
    assert!(
        !with_fact.contains(&PetAction::Startle),
        "the fact folds the seam — no bottle on a scrolled wrap, got \
         {with_fact:?}"
    );
    assert!(
        !with_fact.contains(&PetAction::Crouch) && !with_fact.contains(&PetAction::Leap),
        "and no big-jump show: the watch keeps the ground and the fold \
         keeps the latch, got {with_fact:?}"
    );
    let blind = script(false);
    assert!(
        !blind.contains(&PetAction::Startle),
        "the snap tier catches what the fact does not see: a same-row \
         retreat of a third of the line is a jump, never a fright, got \
         {blind:?}"
    );
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 29, 60);
    let mut bottled = false;
    for _ in 0..30 {
        t += Duration::from_millis(16);
        bottled |= pet.tick(sense(t, Some((29, 54)))).action == PetAction::Startle;
    }
    assert!(
        bottled,
        "control: the harness must still be able to see a bottle, or \
         the assertions above assert nothing"
    );
}

struct Law {
    pet: PetBrain,
    t: Instant,
    last: (f32, f32, u16, u16),
    cols: u16,
    rows: u16,
    spans: Vec<(u16, u16)>,
    live: Option<u16>,
    launches: Vec<Flight>,
    actions: Vec<PetAction>,
    frames: Vec<PetFrame>,
}

impl Law {
    fn new(cols: u16, rows: u16, caret: (u16, u16)) -> Self {
        let t = Instant::now();
        let mut pet = PetBrain::default();
        let mut s = sense(t, Some(caret));
        s.cols = cols;
        s.rows = rows;
        let f = pet.tick(s);
        Self {
            pet,
            t,
            last: (f.col, f.row, cols, rows),
            cols,
            rows,
            spans: vec![(0, 0); usize::from(rows)],
            live: None,
            launches: Vec::new(),
            actions: Vec::new(),
            frames: Vec::new(),
        }
    }

    fn ink(&mut self, row: u16, first: u16, end: u16) {
        self.spans[usize::from(row)] = (first, end);
    }

    fn frame(&mut self, caret: Option<(u16, u16)>) -> PetFrame {
        self.t += Duration::from_millis(16);
        self.pet.sense_ink(0, &self.spans, self.live);
        let mut s = sense(self.t, caret);
        s.cols = self.cols;
        s.rows = self.rows;
        let pre = self.pet.flight;
        let f = lawful_tick(&mut self.pet, s, &mut self.last);
        if pre.is_none()
            && let Some(fl) = self.pet.flight
        {
            self.launches.push(fl);
        }
        self.actions.push(f.action);
        self.frames.push(f);
        f
    }

    fn now(&self) -> usize {
        self.actions.len()
    }

    fn onsets(&self, from: usize, a: PetAction) -> usize {
        let prev = if from == 0 {
            None
        } else {
            self.actions.get(from - 1).copied()
        };
        let mut was = prev == Some(a);
        let mut n = 0;
        for &x in &self.actions[from.min(self.actions.len())..] {
            if x == a && !was {
                n += 1;
            }
            was = x == a;
        }
        n
    }

    fn episodes(&self, from: usize) -> usize {
        self.onsets(from, PetAction::Leap)
    }

    fn touchdown(&self, from: usize) -> Option<usize> {
        if self.pet.flight.is_some() {
            return None;
        }
        let mut last = None;
        for i in from.max(1)..self.actions.len() {
            if self.actions[i - 1] == PetAction::Leap && self.actions[i] != PetAction::Leap {
                last = Some(i);
            }
        }
        last
    }

    fn dust(&self, from: usize) -> bool {
        self.frames[from.min(self.frames.len())..].iter().any(|f| {
            f.motes
                .iter()
                .flatten()
                .any(|m| m.kind == PetMoteKind::Dust)
        })
    }

    fn wore(&self, from: usize, pose: PetGlyphId) -> bool {
        self.frames[from.min(self.frames.len())..]
            .iter()
            .any(|f| f.pose == pose)
    }

    fn hold(&mut self, caret: Option<(u16, u16)>, frames: u32) {
        for _ in 0..frames {
            let _ = self.frame(caret);
        }
    }

    fn type_to(&mut self, row: u16, from: u16, to: u16, frames_per_key: u32) {
        for c in (from + 1)..=to {
            self.ink(row, 0, c);
            self.hold(Some((row, c)), frames_per_key);
        }
    }

    fn delete_to(&mut self, row: u16, from: u16, to: u16, frames_per_key: u32) {
        for c in (to..from).rev() {
            self.ink(row, 0, c);
            self.hold(Some((row, c)), frames_per_key);
        }
    }

    fn warm(cols: u16, rows: u16, row: u16, col: u16) -> Self {
        let start = col.saturating_sub(16);
        let mid = col.saturating_sub(8);
        let mut w = Self::new(cols, rows, (row, start));
        w.live = Some(row);
        w.ink(row, 0, start);
        w.type_to(row, start, mid, 6);
        w.hold(Some((row, mid)), 150);
        w.type_to(row, mid, col, 6);
        w
    }
}

#[test]
fn no_flight_may_outrun_the_animal_that_flies_it() {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    struct Judge {
        in_flight: bool,
        launches: u32,
        fastest: f32,
        aimed: Option<(f32, f32)>,
        fastest_reaim: f32,
    }
    impl Judge {
        fn new() -> Self {
            Judge {
                in_flight: false,
                launches: 0,
                fastest: 0.0,
                aimed: None,
                fastest_reaim: 0.0,
            }
        }
        fn judge(&mut self, pet: &PetBrain) {
            match pet.flight {
                Some(f) => {
                    let rate = span_eq(f.to_col - f.from_col, f.to_row - f.from_row) / f.dur;
                    let bound = if f.big && f.brisk {
                        BRISK_SPEED_MAX
                    } else if f.big {
                        BIG_HOP_SPEED_MAX
                    } else {
                        HOP_SPEED_MAX
                    };
                    assert!(
                        rate <= bound + 1e-3,
                        "a launched arc outruns the animal that flies it: \
                         {rate:.1} c/s against {bound:.0} (big={})",
                        f.big
                    );
                    if let Some((was_to, was_col)) = self.aimed
                        && (f.to_col - was_to).abs() > 1e-3
                    {
                        let pace = (f.to_col - was_col).abs() / (f.dur - f.t).max(1e-3);
                        assert!(
                            pace <= bound + 1e-3,
                            "a mid-air re-aim outruns the flight it \
                             rebased: {pace:.1} c/s against {bound:.0} \
                             (big={})",
                            f.big
                        );
                        self.fastest_reaim = self.fastest_reaim.max(pace);
                    }
                    self.aimed = Some((f.to_col, pet.col));
                    if !self.in_flight {
                        self.launches += 1;
                        self.fastest = self.fastest.max(rate);
                    }
                    self.in_flight = true;
                }
                None => {
                    self.in_flight = false;
                    self.aimed = None;
                }
            }
        }
    }
    let wide = |now: Instant, caret: Option<(u16, u16)>| PetSense {
        caret_drawn: true,
        cols: 200,
        ..sense(now, caret)
    };
    fn awake_wide(pet: &mut PetBrain, start: Instant, row: u16, col: u16) -> Instant {
        let wide = |now: Instant, caret: (u16, u16)| PetSense {
            caret_drawn: true,
            cols: 200,
            ..sense(now, Some(caret))
        };
        let mut t = start;
        let _ = pet.tick(wide(t, (row, col)));
        for k in 1..=2u16 {
            let until = t + Duration::from_secs_f32(0.05);
            while t < until {
                t += Duration::from_millis(16);
                let _ = pet.tick(wide(t, (row, col + k - 1)));
            }
            t = until;
            let _ = pet.tick(wide(t, (row, col + k)));
        }
        let settle = t + Duration::from_secs_f32(SIT_AFTER + 0.4);
        while t < settle {
            t += Duration::from_millis(16);
            let _ = pet.tick(wide(t, (row, col + 2)));
        }
        t
    }
    let start = Instant::now();

    let mut pet = PetBrain::default();
    let mut law = Judge::new();
    let mut t = awake(&mut pet, start, 6, 95);
    t += Duration::from_millis(16);
    let _ = pet.tick(sense(t, Some((6, 55))));
    law.judge(&pet);
    let end = t + Duration::from_secs_f32(3.0);
    let mut last = 0.0f32;
    while t < end {
        t += Duration::from_millis(16);
        last = pet.tick(sense(t, Some((6, 55)))).col;
        law.judge(&pet);
    }
    assert_eq!(
        law.launches, 1,
        "the wall crossing is ONE flight now, not a capped transit plus \
         the remainder"
    );
    assert!(
        law.fastest <= BIG_HOP_SPEED_MAX + 1e-3,
        "and it is brisk, at the animal's pace (fastest {:.1})",
        law.fastest
    );
    let w = art_cols(10, 20);
    let station = PetBrain::station(55, 100, w);
    assert!(
        (last - station).abs() < 3.0,
        "and the cat arrives on the caret's far side (col {last}, \
         station {station})"
    );

    let mut pet = PetBrain::default();
    let mut law = Judge::new();
    let mut t = awake_wide(&mut pet, start, 6, 8);
    t += Duration::from_millis(16);
    let _ = pet.tick(wide(t, Some((6, 50))));
    law.judge(&pet);
    let end = t + Duration::from_secs_f32(4.0);
    let mut final_col = 0.0f32;
    while t < end {
        t += Duration::from_millis(16);
        final_col = pet.tick(wide(t, Some((6, 50)))).col;
        law.judge(&pet);
    }
    assert_eq!(
        law.launches, 1,
        "fixture: the gap closes in ONE flight, not a jog and a pounce"
    );
    assert!(
        law.fastest > HOP_SPEED_MAX && law.fastest <= BIG_HOP_SPEED_MAX + 1e-3,
        "and it is a brisk bound, at the animal's pace (fastest {:.1})",
        law.fastest
    );
    assert!(
        (final_col - 51.0).abs() < 2.5,
        "and the cat still arrives, got col {final_col}"
    );

    let mut pet = PetBrain::default();
    let mut law = Judge::new();
    let mut t = awake_wide(&mut pet, start, 6, 4);
    t += Duration::from_millis(16);
    let _ = pet.tick(wide(t, Some((6, 199))));
    law.judge(&pet);
    let end = t + Duration::from_secs_f32(4.0);
    while t < end {
        t += Duration::from_millis(16);
        let _ = pet.tick(wide(t, Some((6, 199))));
        law.judge(&pet);
    }
    assert!(
        law.launches >= 1 && law.fastest > HOP_SPEED_MAX && law.fastest <= BIG_HOP_SPEED_MAX + 1e-3,
        "fixture: the crossing must fly at a bound's pace — outrunning \
         any ordinary hop, yet obeying the bound's own bar ({} \
         launches, fastest {:.1})",
        law.launches,
        law.fastest
    );

    let scripts: [(&str, fn()); 5] = [
        ("(a) the ink re-anchor at 200 columns", || {
            let mut w = Law::warm(200, 30, 4, 190);
            w.hold(Some((4, 190)), 150);
            for r in 0..30 {
                w.ink(r, 8, 200);
            }
            w.hold(Some((4, 190)), 200);
        }),
        ("(d) a sixty-cell jump during the wake stretch", || {
            let mut w = Law::new(200, 30, (3, 5));
            w.hold(Some((3, 5)), 2);
            w.hold(Some((3, 6)), 12);
            w.hold(Some((3, 65)), 200);
        }),
        ("(e) a held backspace from the wall side", || {
            let mut w = Law::warm(100, 30, 5, 99);
            w.delete_to(5, 99, 60, 2);
            w.hold(Some((5, 60)), 200);
        }),
        ("(g) the wall-side Enter that arrives mid row-hop", || {
            let mut w = Law::warm(80, 24, 6, 78);
            w.ink(7, 0, 2);
            w.live = Some(7);
            w.hold(Some((7, 2)), 200);
        }),
        ("(h) a caret that snaps home mid-SHOW", || {
            let mut w = Law::warm(80, 24, 6, 18);
            w.ink(6, 0, 0);
            w.hold(Some((6, 18)), 150);
            w.hold(Some((6, 74)), 56);
            assert!(
                w.pet.flight.is_some_and(|f| f.show && f.t > 0.25 * f.dur),
                "fixture: the show's bound is in the air"
            );
            w.hold(Some((6, 2)), 200);
        }),
    ];
    let failures: Vec<String> = scripts
        .iter()
        .filter_map(|(name, script)| {
            catch_unwind(AssertUnwindSafe(script)).err().map(|e| {
                let msg = e
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| e.downcast_ref::<&str>().map(ToString::to_string))
                    .unwrap_or_else(|| "(no message)".to_string());
                format!("{name}: {msg}")
            })
        })
        .collect();
    assert!(
        failures.is_empty(),
        "{} of 5 scripts broke the launch-time law:\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}

/// A RE-AIM NEVER TURNS A FLIGHT BACK, AND THE CARET'S NEW ROW IS STILL TAKEN. The
/// caret goes to the next row's start while a pounce flies right along its row: the
/// station lies behind the cat. The landing keeps its column (no turn in the air) and
/// takes the caret's row, and one more trip along that row brings the cat to the caret.
#[test]
fn a_re_aim_keeps_its_column_when_the_caret_goes_behind_and_takes_its_row() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 4, 20);
    let caret = (4, 40u16);
    let mut spins = 0;
    while pet.flight.is_none() && spins < 60 {
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some(caret)));
        spins += 1;
    }
    let f0 = pet.flight.expect("fixture: the word jump pounces");
    assert!(!f0.big, "fixture: a pounce, inside the hop tier: {f0:?}");
    while pet.flight.is_some_and(|f| f.t / f.dur < 0.3) {
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some(caret)));
    }
    assert!(pet.flight.is_some(), "fixture: still airborne");
    t += Duration::from_millis(16);
    let _ = pet.tick(sense(t, Some((5, 2))));
    let f1 = pet.flight.expect("the pounce is still in the air");
    assert!(
        (f1.to_col - f0.to_col).abs() < 1e-6,
        "a re-aim never turns a flight back, got {f1:?}"
    );
    assert!(
        (f1.to_row - 5.0).abs() < 1e-6,
        "the caret's new row is taken, got {f1:?}"
    );
    let mut launches = 0;
    let mut last = pet.flight;
    let mut final_row = 4.0f32;
    for _ in 0..200 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((5, 2))));
        if last.is_none() && pet.flight.is_some() {
            launches += 1;
        }
        last = pet.flight;
        final_row = f.row;
    }
    assert_eq!(launches, 1, "one trip along row 5 after the landing");
    assert!(
        (final_row - 5.0).abs() < 0.01,
        "and it ends on row 5, got {final_row}"
    );
}

#[test]
fn the_wall_side_stand_takes_the_blank_row_below() {
    let w = art_cols(10, 20);
    let mut pet = PetBrain::default();
    let mut spans = vec![(0u16, 0u16); 24];
    spans[6] = (0, 78);
    pet.sense_ink(0, &spans, Some(6));
    let (_, row) = pet.station_safe((6, 75), 80, 24, w);
    assert!(
        (row - 7.0).abs() < 0.01,
        "the walled-shut row hands the stand to the blank row below, got row {row}"
    );
    let mut spans = vec![(0u16, 0u16); 24];
    spans[23] = (0, 78);
    pet.sense_ink(0, &spans, Some(23));
    let (_, row) = pet.station_safe((23, 75), 80, 24, w);
    assert!(
        (row - 22.0).abs() < 0.01,
        "and on the last row, to the blank row above, got row {row}"
    );

    // Typed to two cells short of the margin and stopped there, mid-screen or on the last
    // row, the cat keeps its stand by the caret: no flight for a wrap that never comes. On
    // the parent commit it leapt ahead to the next line's stand mid-screen, 70 cells from
    // the caret, as the typing stopped. (No letter reader here: the wall holds, as the head
    // law's stand does where nothing is certified.)
    for row in [6u16, 23] {
        let mut p = Probe::new(80);
        p.warm(row, 72);
        p.mark();
        for c in 73..=78u16 {
            p.ink(row, 0, c);
            for _ in 0..6 {
                let _ = p.frame(Some((row, c)));
            }
        }
        let mut last = None;
        for _ in 0..120 {
            last = Some(p.frame(Some((row, 78))));
        }
        let tr = p.trigger.expect("marked");
        let flights: Vec<&ProbeFlight> = p.flights.iter().filter(|f| f.at >= tr).collect();
        assert!(
            flights.is_empty(),
            "row {row}: no flight for a wrap that never comes: {flights:?}"
        );
        let last = last.expect("frames");
        assert!(
            (last.row - f32::from(row)).abs() < 0.01 && last.col > 70.0,
            "row {row}: the cat waits by the caret, got col {} row {}",
            last.col,
            last.row
        );
    }
}

#[test]
fn the_wall_transit_never_fires_after_a_flight_that_already_crossed() {
    let mut p = Probe::new(80);
    p.warm(5, 79);
    p.mark();
    assert!(
        !p.pet.wall_side,
        "fixture: typed to the margin, the cat never crossed the caret"
    );
    p.live = Some(6);
    p.ink(6, 0, 0);
    let _ = p.frame(Some((6, 0)));
    assert!(
        !p.pet.wall_side,
        "fixture: the wrap flips the wall side — the transit latched on this frame"
    );
    let mut latched_airborne = false;
    for c in 1..=12u16 {
        p.ink(6, 0, c);
        for _ in 0..8 {
            let _ = p.frame(Some((6, c)));
            latched_airborne |= p.pet.pending_wall_transit && p.pet.flight.is_some();
        }
    }
    p.hold(Some((6, 12)), 2.0);
    assert!(
        !latched_airborne,
        "a flight consumes the transit latch on its way out, never carries it"
    );
    let flights: Vec<&ProbeFlight> = p.flights.iter().collect();
    assert!(!flights.is_empty(), "fixture: the wrap flies");
    assert!(
        flights
            .iter()
            .all(|f| (f.dur - WALL_HOP_DUR).abs() > 1e-4 || (f.arc - WALL_HOP_ARC).abs() > 1e-4),
        "no span-blind wall arc after a flight that already crossed: {flights:?}"
    );
    let episodes = flights
        .iter()
        .filter(|f| !f.site.starts_with("bound 2") && !f.site.starts_with("mid-air re-aim"))
        .count();
    assert_eq!(
        episodes, 1,
        "exactly one airborne episode for the line end and the wrap: {flights:?}"
    );
    let last = p.last.expect("frames were drawn");
    assert!(p.pet.flight.is_none(), "and it is down by the end");
    assert!(
        last.col > 12.0 && (last.row - 6.0).abs() < 0.01,
        "stationed on the caret's right on the new row, got ({:.2}, {})",
        last.col,
        last.row
    );
}

#[test]
fn a_snap_home_is_a_jump_not_a_fright() {
    let judge = |w: &Law, from: usize, door: &str| {
        assert_eq!(
            w.onsets(from, PetAction::Startle),
            0,
            "{door}: no fright, got {:?}",
            &w.actions[from..]
        );
        assert!(
            !w.wore(from, PetGlyphId::PetCrouchWiggle),
            "{door}: never the show's gather"
        );
        assert_eq!(
            w.episodes(from),
            1,
            "{door}: one airborne episode: {:?}",
            &w.actions[from..]
        );
        assert!(w.pet.flight.is_none(), "{door}: down by the end");
        assert!(!w.dust(from), "{door}: no dust");
    };

    let mut w = Law::warm(80, 30, 5, 60);
    w.hold(Some((5, 60)), 150);
    assert!(w.pet.action.settled(), "fixture: settled");
    let at = w.now();
    let _ = w.frame(Some((5, 3)));
    assert!(
        w.pet.pending_pounce && w.pet.quiet_leap && !w.pet.pending_big_jump,
        "the snap latches a quiet jump, never the show"
    );
    w.hold(Some((5, 3)), 120);
    judge(&w, at, "open row");

    let mut w = Law::warm(80, 24, 23, 40);
    w.hold(Some((23, 40)), 150);
    let at = w.now();
    let _ = w.frame(Some((23, 3)));
    w.hold(Some((23, 3)), 120);
    judge(&w, at, "last row");
    let (_, row, _, _) = w.last;
    assert!(
        (row - 22.0).abs() < 0.01,
        "the stand is the blank row above the inked line, got {row}"
    );
    let down = w.touchdown(at).expect("landed");
    assert!(
        w.actions[down..].iter().all(|a| *a != PetAction::Startle),
        "and nothing replays a fright after the landing"
    );
}

#[test]
fn a_startle_shoved_off_its_spot_is_not_replayed() {
    let mut w = Law::warm(80, 30, 5, 30);
    w.hold(Some((5, 30)), 150);
    assert!(w.pet.action.settled(), "fixture: settled");
    let at = w.now();
    let _ = w.frame(Some((5, 24)));
    assert_eq!(w.pet.action, PetAction::Startle, "fixture: the fright");
    w.hold(Some((5, 24)), 3);
    w.ink(5, 0, 40);
    w.hold(Some((5, 24)), 120);
    assert_eq!(
        w.onsets(at, PetAction::Startle),
        1,
        "exactly one fright: {:?}",
        &w.actions[at..]
    );
    let down = w.touchdown(at).expect("the eviction hops");
    assert!(
        w.actions[down..].iter().all(|a| *a != PetAction::Startle),
        "never replayed after the hop: {:?}",
        &w.actions[down..]
    );
    assert!(w.pet.action.settled(), "and settled after it");
}

#[test]
fn a_wrapping_build_log_sits_the_cat_down_to_watch() {
    let last = PROBE_ROWS - 1;
    for (burst, above) in [(false, true), (true, true), (false, false)] {
        let door = format!("burst={burst} row-above-inked={above}");
        let mut p = Probe::judged(80);
        p.warm(last, 40);
        p.hold(Some((last, 40)), 2.5);
        p.mark();
        p.log_rows(240, burst, above);
        assert!(p.pet.stream, "{door}: the watch is armed");
        assert!(
            p.episodes <= 2,
            "{door}: at most two airborne episodes, got {}: {:?}",
            p.episodes,
            p.actions
        );
        assert!(
            p.actions
                .iter()
                .all(|(at, a)| *a != PetAction::Startle || *at < p.trigger.unwrap_or(0.0) + 0.2),
            "{door}: no fright after 0.2 s: {:?}",
            p.actions
        );
        let still: Vec<&(f32, PetFrame)> = p.trace.iter().filter(|(at, _)| *at >= 1.5).collect();
        let (c0, r0) = (still[0].1.col, still[0].1.row);
        assert!(
            still
                .iter()
                .all(|(_, f)| (f.col - c0).abs() < 1e-3 && (f.row - r0).abs() < 1e-3),
            "{door}: still from 1.5 s on"
        );
        assert!(
            still.iter().all(|(_, f)| matches!(
                f.action,
                PetAction::Perk | PetAction::Sit | PetAction::Leap | PetAction::Land
            )),
            "{door}: sat down to watch: {:?}",
            p.actions
        );
        assert!(p.needs_always, "{door}: the lane is held throughout");
        if !above {
            assert!(
                (r0 - f32::from(last - 1)).abs() < 0.01,
                "{door}: seated on the blank row above the log (rule 3), got row {r0}"
            );
        }
        let flights_before = p.flights.len();
        let at_prompt = p.now();
        p.ink(last, 0, 2);
        if above {
            p.ink(last - 1, 0, 80);
        }
        let _ = p.frame(Some((last, 2)));
        p.hold(Some((last, 2)), 3.0);
        let home: Vec<&ProbeFlight> = p.flights[flights_before..].iter().collect();
        assert!(
            (1..=2).contains(&home.len()),
            "{door}: one trip home, at most two bounds: {home:?}"
        );
        assert!(p.pet.flight.is_none(), "{door}: home within 3 s");
        let tr = p.trigger.expect("marked");
        let down = p.touchdown.expect("landed") - (at_prompt - tr);
        assert!(
            down <= 3.0,
            "{door}: paws down within 3 s of the prompt, got {down:.2}"
        );
        let last_f = p.last.expect("frames");
        assert!(
            (last_f.row - f32::from(last)).abs() < 0.01,
            "{door}: home on the last row"
        );
        assert!(!p.pet.stream, "{door}: the watch lapsed");
        p.hold(Some((last, 2)), SLEEP_AFTER + BREATH_WINDOW + 2.0);
        assert!(
            !p.pet.needs_frames(),
            "{door}: the lane is released once settled"
        );
    }
}

#[test]
fn a_top_down_paint_never_hops_every_row() {
    let mut w = Law::warm(80, 30, 0, 10);
    let at = w.now();
    for r in 1..30u16 {
        let _ = w.frame(Some((r, 0)));
    }
    assert!(w.pet.stream, "the paint arms the watch");
    assert!(
        w.episodes(at) <= 2,
        "at most two hops during the paint: {:?}",
        &w.actions[at..]
    );
    let after = w.now();
    w.hold(Some((29, 0)), 200);
    assert!(!w.pet.stream, "the watch lapses");
    assert!(
        (1..=2).contains(&w.episodes(after)),
        "one trip after the lapse: {:?}",
        &w.actions[after..]
    );
    let (_, row, _, _) = w.last;
    assert!(
        w.pet.flight.is_none() && (row - 29.0).abs() < 0.01,
        "home on row 29"
    );
}

#[test]
fn a_held_word_jump_is_not_a_stream() {
    let mut w = Law::new(100, 30, (4, 2));
    w.hold(Some((4, 2)), 2);
    for k in 1..=11u16 {
        let _ = w.frame(Some((4, 2 + 8 * k)));
        assert!(!w.pet.stream, "a held word-jump never arms the watch");
    }
    w.hold(Some((4, 90)), 60);
    assert!(!w.pet.stream);
}

#[test]
fn a_line_change_to_the_right_is_still_a_quiet_crossing() {
    for cols in [80u16, 100] {
        let mut p = Probe::judged(cols);
        p.warm(6, 2);
        p.hold(Some((6, 2)), 2.5);
        p.mark();
        let end = cols - 8;
        p.ink(7, 0, end);
        p.live = Some(7);
        let _ = p.frame(Some((7, end)));
        p.hold(Some((7, end)), 3.0);
        let tr = p.trigger.expect("marked");
        let flights: Vec<&ProbeFlight> = p.flights.iter().filter(|f| f.at >= tr).collect();
        assert!(
            !flights.is_empty(),
            "{cols}: fixture — the line change flies"
        );
        assert!(
            flights.iter().all(|f| !f.show),
            "{cols}: a printed line is not a chosen jump: {flights:?}"
        );
        assert!(!p.wiggle, "{cols}: no gather on a printed line");
        assert!(!p.dust, "{cols}: no dust on a printed line");
        assert!(
            p.onsets(PetAction::Startle) == 0,
            "{cols}: no fright on a printed line: {:?}",
            p.actions
        );
        // A bound after the caret is brisk ([`PetBrain::brisk_shape`], r25 K8-3).
        for f in flights.iter().filter(|f| f.big) {
            assert!(
                f.dur <= BRISK_DUR_MAX + 1e-3,
                "{cols}: a printed line's bound in the air {:.2} s, the brisk bound's \
                 {BRISK_DUR_MAX} s at most: {f:?}",
                f.dur
            );
        }
        let bar = BRISK_SPEED_MAX * PROBE_FRAME.as_secs_f32() + 1e-3;
        assert!(
            p.max_dcol.0 <= bar,
            "{cols}: {:.3} cells in one frame, the brisk pace allows {bar:.3}",
            p.max_dcol.0
        );
    }
}

#[test]
fn a_new_line_costs_one_flight_and_no_gallop() {
    for cols in [80u16, 200] {
        for from in [10u16, 40, 70] {
            let mut p = Probe::judged(cols);
            p.warm(6, from);
            p.mark();
            p.live = Some(7);
            p.ink(6, 0, from);
            p.ink(7, 0, 2);
            let _ = p.frame(Some((7, 2)));
            p.hold(Some((7, 2)), 3.0);
            assert_eq!(
                p.episodes, 1,
                "{cols} cols, Enter from {from}: one new line, one flight — {:?}",
                p.actions
            );
            assert_eq!(
                p.onsets(PetAction::Run),
                0,
                "{cols} cols, Enter from {from}: no gallop home — {:?}",
                p.actions
            );
            assert_eq!(
                p.onsets(PetAction::Startle),
                0,
                "{cols} cols, Enter from {from}: and no fright — {:?}",
                p.actions
            );
        }
    }
}

#[test]
fn a_notice_keeps_its_turned_head_for_the_beat() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 10, 10);
    t += Duration::from_millis(16);
    let f = pet.tick(sense(t, Some((10, 40))));
    let turned = pet.species.skin(PetGlyphId::PetPerkTurn);
    assert_eq!(f.action, PetAction::Perk, "fixture: the notice");
    assert_eq!(f.pose, turned, "fixture: the head turned to the caret");
    pet.pending_pounce = false;
    pet.pending_big_jump = false;
    let mut beat = 1;
    loop {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((10, 40))));
        if f.action != PetAction::Perk {
            break;
        }
        beat += 1;
        assert_eq!(
            f.pose, turned,
            "the head stays turned for the beat (frame {beat} of the notice)"
        );
    }
    assert!(beat >= 4, "fixture: the beat lasts ({beat} frames)");
}

#[test]
fn the_notice_beat_is_only_paid_by_a_cat_at_rest() {
    let mut rested = Probe::judged(80);
    rested.warm(6, 40);
    rested.hold(Some((6, 40)), 1.5);
    rested.mark();
    let _ = rested.frame(None);
    rested.live = Some(7);
    rested.ink(6, 0, 0);
    rested.ink(7, 0, 2);
    let _ = rested.frame(Some((7, 2)));
    rested.hold(Some((7, 2)), 2.5);
    assert!(
        rested.onsets(PetAction::Perk) >= 1,
        "a cat at rest looks up before the bound: {:?}",
        rested.actions
    );
    assert!(
        rested.onsets(PetAction::Leap) >= 1,
        "fixture: and then crosses"
    );
    let mut typing = Probe::judged(80);
    typing.warm(6, 40);
    typing.mark();
    let _ = typing.frame(None);
    typing.live = Some(7);
    typing.ink(6, 0, 0);
    typing.ink(7, 0, 2);
    let _ = typing.frame(Some((7, 2)));
    typing.hold(Some((7, 2)), 2.5);
    assert_eq!(
        typing.onsets(PetAction::Perk),
        0,
        "a cat mid-chase does not pay the beat: {:?}",
        typing.actions
    );
}

#[test]
fn no_bound_crosses_more_than_half_the_animal_in_a_frame() {
    let animal = art_cols(10, 20);
    for cols in [20u16, 40, 80, 120, 200, 400] {
        let per_frame = quiet_speed_max(cols) / 60.0;
        assert!(
            per_frame <= 0.5 * animal,
            "{cols} cols: a bound steps {per_frame:.2} cells a frame, \
             {:.2} of the animal's own {animal:.2}-column width",
            per_frame / animal
        );
    }
}

#[test]
fn the_chosen_show_is_paced_like_every_other_bound() {
    for cols in [80u16, 100, 120] {
        let mut p = Probe::judged(cols);
        p.warm(6, 3);
        p.hold(Some((6, 3)), 2.5);
        p.mark();
        let end = cols - 1;
        for r in 4..=8u16 {
            p.ink(r, 0, cols);
        }
        let _ = p.frame(Some((6, end - 1)));
        p.hold(Some((6, end - 1)), 3.0);
        let tr = p.trigger.expect("marked");
        let flights: Vec<&ProbeFlight> = p.flights.iter().filter(|f| f.at >= tr).collect();
        assert!(
            flights.iter().any(|f| f.show),
            "{cols}: fixture — the chosen show plays: {flights:?}"
        );
        for f in &flights {
            let rate = span_eq(f.to_col - f.from_col, f.to_row - f.from_row) / f.dur;
            assert!(
                rate <= quiet_speed_max(cols) + 1e-3,
                "{cols}: the show flew at {rate:.1} c/s over the {:.1} c/s pace: {f:?}",
                quiet_speed_max(cols)
            );
        }
        let bar = quiet_speed_max(cols) * PROBE_FRAME.as_secs_f32() + 1e-3;
        assert!(
            p.max_dcol.0 <= bar,
            "{cols}: {:.3} cells in one frame, the quiet pace allows {bar:.3}",
            p.max_dcol.0
        );
    }
}

#[test]
fn a_burst_of_new_lines_comes_home_quietly() {
    let mut p = Probe::judged(80);
    p.warm(6, 70);
    p.hold(Some((6, 70)), 2.5);
    p.mark();
    for row in 7..=16u16 {
        p.ink(row, 0, 2);
        p.live = Some(row);
        let _ = p.frame(Some((row, 2)));
        p.hold_frames(Some((row, 2)), 2);
    }
    assert!(p.pet.stream, "fixture: the watch is armed by the burst");
    let held = p.flights.len();
    p.hold(Some((16, 2)), 3.0);
    assert!(!p.pet.stream, "fixture: the watch lapsed");
    let home: Vec<&ProbeFlight> = p.flights[held..].iter().collect();
    assert!(!home.is_empty(), "fixture: the cat comes home");
    assert!(
        (home[0].from_col - home[0].to_col).abs() >= BIG_GAP_FRAC * 80.0,
        "fixture: the gap the burst left is the standing-gap clause's: {home:?}"
    );
    assert!(
        home.iter().all(|f| !f.show),
        "the trip home after a burst is quiet, never the show: {home:?}"
    );
    assert!(!p.wiggle, "no butt-wiggle on a held Enter: {:?}", p.actions);
    assert!(!p.dust, "and no landing dust");
    assert!(p.pet.flight.is_none(), "down by the end");
}

#[test]
fn a_hidden_frame_that_hid_no_move_keeps_the_next_show() {
    fn run(hide: bool) -> (bool, bool, bool) {
        let mut p = Probe::new(100);
        p.warm(6, 30);
        p.hold(Some((6, 30)), 2.5);
        assert!(p.pet.action.settled(), "fixture: settled");
        if hide {
            let _ = p.frame(None);
            let _ = p.frame(Some((6, 30)));
            assert!(
                !p.pet.quiet_leap,
                "a hidden frame that hid no move latched the quiet leap"
            );
        }
        p.mark();
        let _ = p.frame(Some((6, 95)));
        p.hold(Some((6, 95)), 3.0);
        let tr = p.trigger.expect("marked");
        (
            p.wiggle,
            p.dust,
            p.flights.iter().any(|f| f.at >= tr && f.show),
        )
    }
    assert_eq!(
        run(false),
        (true, true, true),
        "fixture: a chosen jump plays the whole show"
    );
    assert_eq!(
        run(true),
        run(false),
        "a hidden frame with no move behind it changes nothing"
    );
}

#[test]
fn a_short_new_line_does_not_stand_down_the_next_show() {
    fn run(newline: bool) -> (bool, bool, bool) {
        let mut p = Probe::new(100);
        p.warm(23, 5);
        p.hold(Some((23, 5)), 2.5);
        assert!(p.pet.action.settled(), "fixture: settled");
        if newline {
            let _ = p.frame(Some((23, 1)));
            p.hold(Some((23, 1)), 2.5);
            assert!(p.pet.action.settled(), "fixture: settled again");
            assert!(
                !p.pet.quiet_leap,
                "a quiet leap no flight consumed outlived the arrival that ended it"
            );
        }
        p.mark();
        let _ = p.frame(Some((23, 90)));
        p.hold(Some((23, 90)), 3.0);
        let tr = p.trigger.expect("marked");
        (
            p.wiggle,
            p.dust,
            p.flights.iter().any(|f| f.at >= tr && f.show),
        )
    }
    assert_eq!(
        run(false),
        (true, true, true),
        "fixture: a chosen jump plays the whole show"
    );
    assert_eq!(
        run(true),
        run(false),
        "a four-column new line the pet never left the ground for cannot \
         spend the next jump's show"
    );
}

#[test]
fn a_seam_crossing_never_bets_past_the_station() {
    let mut p = Probe::new(80);
    p.warm(6, 79);
    assert!(
        p.pet.rhythm_open(),
        "fixture: a real typing run is under way, so the lead is live"
    );
    p.mark();
    p.live = Some(7);
    p.ink(7, 0, 0);
    let _ = p.frame_wrapped(Some((7, 0)));
    p.hold(Some((7, 0)), 3.0);
    let tr = p.trigger.expect("marked");
    let cross = p
        .flights
        .iter()
        .find(|f| f.at >= tr && f.big)
        .expect("the wrap is crossed in one bound");
    let station = PetBrain::station(0, 80, art_cols(10, 20));
    assert!(
        cross.to_col <= station + LEAD_MAX,
        "a seam's bound may not bet past the station's own lead \
         ({station:.2} + {LEAD_MAX}), landed at {:.2}",
        cross.to_col
    );
    assert!(
        !p.actions
            .iter()
            .any(|(t, a)| *t >= tr && *a == PetAction::Run),
        "no gallop home from a wrap: {:?}",
        p.actions
    );
}

#[test]
fn an_enter_during_a_gather_never_latches_the_show() {
    let mut p = Probe::new(80);
    p.warm(6, 41);
    p.ink(14, 0, 24);
    p.live = Some(14);
    let _ = p.frame(Some((14, 24)));
    p.hold_frames(Some((14, 24)), 2);
    assert_eq!(
        p.pet.action,
        PetAction::Crouch,
        "fixture: gathering for the row hop: {:?}",
        p.actions
    );
    assert!(!p.pet.pending_big_jump, "fixture: nothing latched yet");
    p.mark();
    p.ink(15, 0, 4);
    p.live = Some(15);
    let _ = p.frame(Some((15, 60)));
    assert!(
        !p.pet.pending_big_jump,
        "an Enter never latches the screen-crossing jump, whatever its size"
    );
    let mut c = 60u16;
    for _ in 0..10 {
        c += 1;
        let _ = p.frame(Some((15, c)));
        p.hold_frames(Some((15, c)), 5);
    }
    p.hold(Some((15, c)), 4.0);
    let tr = p.trigger.expect("marked");
    assert!(
        p.flights.iter().any(|f| f.at >= tr),
        "fixture: the gather really launches: {:?}",
        p.actions
    );
    assert!(
        !p.wiggle,
        "an Enter is never the chosen show, whoever spent the quiet leap: {:?}",
        p.actions
    );
    assert!(!p.dust, "…and a crossing nobody chose puffs no dust");
    assert!(
        !p.flights.iter().any(|f| f.at >= tr && f.show),
        "…and no flight leaves through the show's door"
    );
}

#[test]
fn a_build_log_under_the_cat_is_not_chased_across_the_pane() {
    let last = PROBE_ROWS - 1;
    let mut p = Probe::judged(80);
    p.warm(last, 40);
    p.hold(Some((last, 40)), 2.5);
    p.mark();
    for k in 0..240u32 {
        for r in (last - 5)..=last {
            p.ink(r, 0, 80);
        }
        p.live = Some(last);
        let caret = if k % 2 == 0 { (last, 79) } else { (last, 0) };
        let _ = p.frame(Some(caret));
    }
    assert!(p.pet.stream, "fixture: the watch is armed");
    assert!(
        p.episodes <= 2,
        "at most two airborne episodes under the watch, got {}: {:?}",
        p.episodes,
        p.actions
    );
    assert_eq!(
        p.onsets(PetAction::Startle),
        0,
        "and no fright: {:?}",
        p.actions
    );
    let cols: Vec<f32> = p
        .trace
        .iter()
        .filter(|(at, _)| *at >= 1.5)
        .map(|(_, f)| f.col)
        .collect();
    let lo = cols.iter().copied().fold(f32::MAX, f32::min);
    let hi = cols.iter().copied().fold(f32::MIN, f32::max);
    assert!(
        hi - lo <= 8.0,
        "the cat is not towed after a caret nobody is chasing ({lo:.1}..{hi:.1})"
    );
}

#[test]
fn every_new_line_reaction_is_lawful_and_brief() {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum State {
        Settled,
        Typing,
        Asleep,
        Hidden,
    }

    fn panic_msg(e: &(dyn std::any::Any + Send)) -> String {
        e.downcast_ref::<String>()
            .cloned()
            .or_else(|| e.downcast_ref::<&str>().map(ToString::to_string))
            .unwrap_or_else(|| "(no message)".to_string())
    }

    // The most a cat may move in one frame of an 80-column pane (cells): a new line's
    // crossing, as every bound after the caret (the margin wrap the hand typed and an
    // Enter's new prompt alike), at the brisk bound's pace ([`PetBrain::brisk_shape`], r25
    // K8-3), the animal's own across a pane this narrow: the cat leaves the margin when the
    // line has wrapped, never ahead of it.
    const BRISK_STEP: f32 = BIG_HOP_SPEED_MAX * PROBE_FRAME.as_secs_f32() + 1e-3;

    fn judge(name: &str, p: &Probe, bar: f32, row: u16, step: f32) -> Vec<String> {
        let mut out = Vec::new();
        let mut fail = |msg: String| out.push(format!("{name}: {msg}"));
        if p.episodes > 2 {
            fail(format!("{} airborne episodes: {:?}", p.episodes, p.actions));
        }
        let tr = p.trigger.unwrap_or(f32::MAX);
        let crossed_before = p
            .flights
            .iter()
            .any(|f| f.at < tr && (f.to_row - f32::from(row)).abs() < 0.01);
        // A flight in the air at the trigger that goes on to the new line and comes down
        // there is the one flight ([`PetBrain::retime`]).
        let carried_on = p.at_trigger.is_some_and(|f| f.action == PetAction::Leap)
            && p.trace
                .iter()
                .find(|(_, f)| f.action != PetAction::Leap)
                .is_some_and(|(_, f)| (f.row - f32::from(row)).abs() < 0.01);
        if p.episodes == 0 && !crossed_before && !carried_on {
            fail(format!("never flew: {:?}", p.actions));
        }
        if p.episodes > 0 && crossed_before {
            fail(format!(
                "crossed before the trigger and flew again: {:?}",
                p.actions
            ));
        }
        let startles = p.onsets(PetAction::Startle);
        if startles > 0 {
            fail(format!("{startles} fright(s): {:?}", p.actions));
        }
        let perks = p.onsets(PetAction::Perk);
        if perks > 1 {
            fail(format!("{perks} notice beats: {:?}", p.actions));
        }
        if p.wiggle {
            fail("the show's gather played".to_string());
        }
        if p.dust {
            fail("dust on a quiet landing".to_string());
        }
        if p.cols == 80 && p.max_dcol.0 >= step {
            fail(format!(
                "{:.3} cells in one frame at {:+.2}s ({:?})",
                p.max_dcol.0, p.max_dcol.1, p.max_dcol.2
            ));
        }
        match p.grounded_at() {
            None if crossed_before && p.pet.flight.is_none() => {}
            None => fail("not on the ground by the end".to_string()),
            Some(t) if t > bar => fail(format!("paws down at {t:.2} s, bar {bar} s")),
            Some(_) => {}
        }
        if let Some(f) = p.last
            && (f.row - f32::from(row)).abs() > 0.01
        {
            fail(format!("ended on row {}, not {row}", f.row));
        }
        out
    }

    let mut failures: Vec<String> = Vec::new();
    let mut check =
        |name: String, bar: f32, row: u16, step: f32, build: &mut dyn FnMut() -> Probe| {
            match catch_unwind(AssertUnwindSafe(build)) {
                Err(e) => failures.push(format!("{name}: {}", panic_msg(&*e))),
                Ok(p) => failures.extend(judge(&name, &p, bar, row, step)),
            }
        };

    for cols in [80u16, 200] {
        for col0 in [10u16, 40, 78] {
            for state in [State::Settled, State::Typing, State::Asleep, State::Hidden] {
                for prompt_rows in [1u16, 3] {
                    let to_row = 6 + prompt_rows;
                    // The brisk crossing, and a present's rounding at each of the
                    // caret's return, the notice, the coil, the launch and the landing.
                    let pane_crossing = BRISK_DUR_MAX + 5.0 * PROBE_FRAME.as_secs_f32();
                    let walled =
                        f32::from(col0) + STATION_LEAD + art_cols(10, 20) > f32::from(cols);
                    let bar = match state {
                        State::Settled => 1.6,
                        State::Typing if walled => 1.4,
                        State::Typing => 1.3,
                        State::Hidden if col0 <= 40 => 1.3,
                        State::Hidden => PERK_HOLD + CROUCH_DUR + pane_crossing,
                        State::Asleep if prompt_rows == 1 => 2.7,
                        State::Asleep => 2.0,
                    };
                    let name = format!(
                        "{cols} cols, Enter from {col0} ({state:?}) onto a {prompt_rows}-row prompt"
                    );
                    check(name, bar, to_row, BRISK_STEP, &mut || {
                        let mut p = Probe::judged(cols);
                        p.warm(6, col0);
                        match state {
                            State::Settled | State::Hidden => p.hold(Some((6, col0)), 2.5),
                            State::Asleep => p.hold(Some((6, col0)), 25.0),
                            State::Typing => {}
                        }
                        p.mark();
                        if state == State::Hidden {
                            let _ = p.frame(None);
                        }
                        for r in 7..=to_row {
                            p.ink(r, 0, 2);
                        }
                        p.live = Some(to_row);
                        let _ = p.frame(Some((to_row, 2)));
                        p.hold(Some((to_row, 2)), 3.0);
                        p
                    });
                }
            }
        }
        let wrap_bar = if cols == 80 {
            1.6
        } else {
            CROUCH_DUR + 2.0 * BIG_FLIGHT_MAX + LAND_DUR
        };
        check(
            format!("{cols} cols, the typed margin wrap"),
            wrap_bar,
            7,
            BRISK_STEP,
            &mut || {
                let mut p = Probe::judged(cols);
                p.warm(6, cols - 1);
                p.mark();
                p.live = Some(7);
                p.ink(7, 0, 0);
                let _ = p.frame(Some((7, 0)));
                p.hold_frames(Some((7, 0)), 5);
                p.type_to(7, 0, 5);
                p.hold(Some((7, 5)), 3.0);
                p
            },
        );
        let last = PROBE_ROWS - 1;
        check(
            format!("{cols} cols, the scrolled Enter after a short command"),
            1.6,
            last,
            BRISK_STEP,
            &mut || {
                let mut p = Probe::judged(cols);
                p.warm(last, 10);
                p.mark();
                p.ink(last - 1, 0, 10);
                p.ink(last, 0, 2);
                p.live = Some(last);
                let _ = p.frame(Some((last, 2)));
                p.hold_frames(Some((last, 2)), 5);
                p.type_to(last, 2, 6);
                p.hold(Some((last, 6)), 3.0);
                p
            },
        );
        check(
            format!("{cols} cols, the scrolled wrap on the last row"),
            wrap_bar,
            last,
            BRISK_STEP,
            &mut || {
                let mut p = Probe::judged(cols);
                p.warm(last, cols - 1);
                p.mark();
                p.ink(last - 1, 0, cols);
                p.ink(last, 0, 0);
                p.live = Some(last);
                let _ = p.frame_wrapped(Some((last, 0)));
                p.hold_frames(Some((last, 0)), 5);
                p.type_to(last, 0, 5);
                p.hold(Some((last, 5)), 3.0);
                p
            },
        );
    }
    assert!(
        failures.is_empty(),
        "{} scenario(s) broke the new-line law:\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}

/// THE WRAP'S CROSSING TAKES AS LONG AT ANY WIDTH ([`PetBrain::brisk_shape`]). A line typed
/// into the margin of an 80-, 120-, 150- and 200-column pane wraps, mid-screen and on the
/// last row (scrolled), and the hand types on: the cat crosses in one bound, in the air no
/// longer than [`BRISK_DUR_MAX`] (and two frames), or, on a pane wider than that pace's
/// limit crosses in it (154 columns), than the pane at [`BRISK_SPEED_MAX`]; and no frame of
/// it moves the body faster than that.
///
/// NEGATIVE CONTROL (as run on main before the fix, dc9a0149ad): held to the animal's 140
/// cells a second at any width, the crossing was 0.8 s in the air at 120 columns, 1.0 s at
/// 150 and 1.4 s at 200, floating over the line as the writing ran on.
#[test]
fn the_wrap_s_crossing_takes_as_long_at_any_width() {
    let mut faults = Vec::new();
    for cols in [80u16, 120, 150, 200] {
        for scrolled in [false, true] {
            let row = if scrolled { PROBE_ROWS - 1 } else { 7 };
            let mut p = Probe::judged(cols);
            p.warm(row - u16::from(!scrolled), cols - 1);
            p.mark();
            if scrolled {
                p.ink(row - 1, 0, cols);
            }
            p.ink(row, 0, 0);
            p.live = Some(row);
            let _ = p.frame_wrapped(Some((row, 0)));
            p.hold_frames(Some((row, 0)), 5);
            p.type_to(row, 0, 5);
            p.hold(Some((row, 5)), 3.0);
            let label = format!("{cols} columns, scrolled {scrolled}");
            let mut aloft = Vec::new();
            let mut up: Option<f32> = None;
            for (at, f) in &p.trace {
                match (up, f.action == PetAction::Leap) {
                    (None, true) => up = Some(*at),
                    (Some(t0), false) => {
                        aloft.push(at - t0);
                        up = None;
                    }
                    _ => {}
                }
            }
            let frame = p.frame_dur.as_secs_f32();
            let bar = BRISK_DUR_MAX.max(f32::from(cols) / BRISK_SPEED_MAX) + 2.0 * frame;
            if aloft.len() != 1 || aloft[0] > bar {
                faults.push(format!("{label}: flights (s aloft) {aloft:?}"));
            }
            if p.max_dcol.0 > BRISK_SPEED_MAX * frame + 1e-3 {
                faults.push(format!(
                    "{label}: {:.2} cells in one frame at {:+.2} s",
                    p.max_dcol.0, p.max_dcol.1
                ));
            }
            if p.last.is_none_or(|f| (f.row - f32::from(row)).abs() > 0.01) {
                faults.push(format!(
                    "{label}: not down on the caret's row: {:?}",
                    p.last
                ));
            }
        }
    }
    assert!(faults.is_empty(), "{}", faults.join("\n"));
}

#[test]
fn a_one_frame_caret_hide_never_teleports_a_visible_cat() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 20, 60);
    let (t2, before) = idle(&mut pet, t, (20, 60), 0.5);
    t = t2;
    assert!(before.alpha > 200, "fixture: a cat plainly on the glass");

    t += Duration::from_millis(16);
    let _ = pet.tick(sense(t, None));
    t += Duration::from_millis(16);
    let after = pet.tick(sense(t, Some((8, 4))));
    assert!(
        after.alpha > 150,
        "one hidden frame must not erase the cat (alpha {})",
        after.alpha
    );
    assert!(
        (after.col - before.col).abs() < 1.0 && (after.row - before.row).abs() < 1.0,
        "a cat the eye can still see has to travel, not blink: \
         ({}, {}) → ({}, {})",
        before.col,
        before.row,
        after.col,
        after.row
    );
}

#[test]
fn a_caret_withheld_mid_flight_holds_its_fade_and_lands_when_dark() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 0, 10);
    let width = art_cols(10, 20);
    let return_caret = (0, 69);
    let target_col = PetBrain::station(return_caret.1, 100, width);
    let target_row = f32::from(return_caret.0);

    pet.begin_arc(
        target_col,
        target_row,
        0.45,
        1.0,
        FlightKind {
            big: true,
            ..FlightKind::PLAIN
        },
    );
    pet.bound2 = Some((target_col + 5.0, target_row, false));
    t += Duration::from_millis(100);
    let mid = pet.tick(sense(t, Some((0, 12))));
    let live = pet.flight.expect("the fixture is genuinely mid-flight");
    assert!(live.t > 0.0 && live.t < live.dur);
    assert!(
        (mid.col - live.to_col).abs() > 10.0,
        "fixture reached the landing instead of the middle: {} -> {}",
        mid.col,
        live.to_col
    );

    let mut hidden_frames = 0;
    loop {
        t += Duration::from_millis(16);
        let hidden = pet.tick(sense(t, None));
        hidden_frames += 1;
        assert!(
            pet.flight.is_none(),
            "a hidden resident retained its old arc"
        );
        assert!(
            pet.bound2.is_none(),
            "a hidden resident retained a second bound"
        );
        if hidden.alpha > 0 {
            assert!((hidden.col - mid.col).abs() < 1e-4);
            assert!(
                (hidden.row - hidden.lift - (mid.row - mid.lift)).abs() < 1e-4,
                "withholding the caret moved the visible feet: row/lift \
                 ({}, {}) -> ({}, {})",
                mid.row,
                mid.lift,
                hidden.row,
                hidden.lift
            );
            assert_eq!(pet.deferred_hidden_landing, Some((target_col, target_row)));
        } else {
            assert!((pet.col - target_col).abs() < 1e-4);
            assert!((pet.row - target_row).abs() < 1e-4);
            assert!(pet.deferred_hidden_landing.is_none());
            break;
        }
        assert!(hidden_frames < 64, "the bounded fade must reach zero");
    }
    assert!(
        hidden_frames > 1,
        "the fixture inspected live hidden frames"
    );

    t += Duration::from_millis(16);
    let returned = pet.tick(sense(t, Some(return_caret)));
    assert!(pet.flight.is_none());
    assert_ne!(returned.action, PetAction::Leap);
    assert!((returned.col - target_col).abs() < 1e-4);
    assert!((returned.row - target_row).abs() < 1e-4);
}

#[test]
fn a_quick_caret_return_cancels_the_deferred_hidden_landing() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 0, 10);
    let width = art_cols(10, 20);
    let return_caret = (0, 69);
    let target_col = PetBrain::station(return_caret.1, 100, width);
    let target_row = f32::from(return_caret.0);

    pet.begin_arc(
        target_col,
        target_row,
        0.45,
        1.0,
        FlightKind {
            big: true,
            ..FlightKind::PLAIN
        },
    );
    t += Duration::from_millis(100);
    let mid = pet.tick(sense(t, Some((0, 12))));
    assert!(pet.flight.is_some());
    assert!((mid.col - target_col).abs() > 10.0);

    let hidden = pet.tick(sense(t, None));
    assert!(hidden.alpha > 0);
    assert!((hidden.col - mid.col).abs() < 1e-4);
    assert!(
        (hidden.row - hidden.lift - (mid.row - mid.lift)).abs() < 1e-4,
        "the first hidden frame must retain the airborne feet baseline"
    );
    assert_eq!(pet.deferred_hidden_landing, Some((target_col, target_row)));
    assert!(
        pet.needs_frames(),
        "a zero-elapsed hide must retain fade/landing scheduler ownership"
    );

    t += Duration::from_millis(16);
    let returned = pet.tick(sense(t, Some(return_caret)));
    assert!(pet.deferred_hidden_landing.is_none());
    assert!((returned.col - hidden.col).abs() < 1.0);
    assert!(
        (returned.row - returned.lift - (hidden.row - hidden.lift)).abs() < 1.0,
        "quick return must resume from the fading feet baseline"
    );
    assert!(
        (returned.col - target_col).abs() > 10.0,
        "a quick return teleported to the deferred landing"
    );
    if let Some(flight) = pet.flight {
        assert!((flight.from_col - hidden.col).abs() < 1.0);
    }

    let mut settled = false;
    for _ in 0..32 {
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some(return_caret)));
        if pet.retired_flight_lift.is_none() {
            settled = true;
            break;
        }
        assert!(
            pet.needs_frames(),
            "retired lift must keep cadence until it reaches zero"
        );
    }
    assert!(settled, "retired lift must settle within FADE_IN");
}

#[test]
fn a_re_anchor_leaves_the_ground_instead_of_cutting() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let w = art_cols(10, 20);
    let mut t = awake(&mut pet, start, 6, 58);
    let (t2, before) = idle(&mut pet, t, (6, 60), SIT_AFTER + 0.4);
    t = t2;
    assert!(before.action.settled(), "fixture: a settled cat");
    let spans = vec![(0u16, 80u16); 12];
    pet.sense_ink(0, &spans, Some(6));
    t += Duration::from_millis(16);
    let f = pet.tick(sense(t, Some((6, 60))));
    assert_eq!(
        f.action,
        PetAction::Leap,
        "the re-anchor leaves the ground, got {:?}",
        f.action
    );
    assert!(
        (f.col - before.col).abs() < 6.0,
        "and it is still near where it was on the first frame of the trip \
         ({} → {})",
        before.col,
        f.col
    );
    let (_, after) = idle(&mut pet, t, (6, 60), 1.2);
    assert!(
        !on_ink(&after, (0, 80), w),
        "the trip ends on blank ground (col {})",
        after.col
    );
    assert!(
        after.action.settled(),
        "and the borrowed pose is handed back, got {:?}",
        after.action
    );
}

fn lawful_tick(pet: &mut PetBrain, s: PetSense, last: &mut (f32, f32, u16, u16)) -> PetFrame {
    const DT: f32 = 0.016;
    let (c0, r0, cols0, rows0) = *last;
    let w = art_cols(s.cell_w, s.cell_h);
    // The art's width on the cells the brain last saw: a re-grid's new cells move every
    // station that reads it ([`PetBrain::keep_place_on_restation`]).
    let w0 = if pet.last_cell_w == 0 || pet.last_cell_h == 0 {
        w
    } else {
        art_cols(pet.last_cell_w, pet.last_cell_h)
    };
    let rate = |fl: Option<Flight>| fl.map_or((0.0, 0.0), |f| PetBrain::flight_pace(&f));
    let pre_flight = pet.flight;
    let (pre_c, pre_r) = rate(pre_flight);
    let pre_top = pet.top_speed();
    let f = pet.tick(s);
    let (post_c, post_r) = rate(pet.flight);
    let top = pre_top.max(pet.top_speed());
    // A re-grid's give: the move of the edge a station clamps at (the columns less the art's
    // width), or the art's width change a station that reads it moves by, the larger.
    let give_col = ((f32::from(cols0) - w0).max(0.0) - (f32::from(s.cols) - w).max(0.0))
        .abs()
        .max((w - w0).abs());
    let give_row = (f32::from(rows0.saturating_sub(1)) - f32::from(s.rows.saturating_sub(1))).abs();
    let col_bound = top.max(pre_c).max(post_c) * DT + give_col + 1e-3;
    let row_bound = pre_r.max(post_r) * DT + give_row + 1e-3;
    assert!(
        (f.col - c0).abs() <= col_bound,
        "the cat teleported {} cells sideways in one frame (bound {col_bound}): \
         col {c0} → {}, action {:?}",
        (f.col - c0).abs(),
        f.col,
        f.action
    );
    assert!(
        (f.row - r0).abs() <= row_bound,
        "the cat teleported {} rows in one frame (bound {row_bound}): \
         row {r0} → {}, action {:?}",
        (f.row - r0).abs(),
        f.row,
        f.action
    );
    // Every flight at its own pace ([`PetBrain::flight_cap`]): a hop at the hop's, a bound
    // at a quiet crossing's, the writing's wrap at the animal's.
    let cap = |fl: &Flight| PetBrain::flight_cap(fl, s.cols);
    match (pre_flight, pet.flight) {
        (None, Some(fl)) => {
            let span = span_eq(fl.to_col - fl.from_col, fl.to_row - fl.from_row);
            let rate = span / fl.dur;
            assert!(
                rate <= cap(&fl) + 1e-3,
                "the flight outruns the animal: launched at {rate:.1} c/s over the {} c/s \
                 cap (span_eq {span:.1} cells in {:.3} s, big {}, ({:.2},r{:.0}) -> \
                 ({:.2},r{:.0}), action {:?})",
                cap(&fl),
                fl.dur,
                fl.big,
                fl.from_col,
                fl.from_row,
                fl.to_col,
                fl.to_row,
                f.action
            );
        }
        (Some(a), Some(b)) if (b.to_col - a.to_col).abs() > 1e-6 => {
            let left = (b.to_col - f.col).abs();
            let t_rem = (b.dur - b.t).max(1e-3);
            let rate = left / t_rem;
            assert!(
                rate <= cap(&b) + 1e-3,
                "the re-aim outruns the flight: {left:.2} cells left to fly in {t_rem:.3} s \
                 is {rate:.1} c/s over the {} c/s cap (to {:.2} -> {:.2}, big {})",
                cap(&b),
                a.to_col,
                b.to_col,
                b.big
            );
        }
        _ => {}
    }
    *last = (f.col, f.row, s.cols, s.rows);
    f
}

#[test]
fn the_cat_never_changes_position_without_animating_it() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = start;
    let f0 = pet.tick(sense(t, Some((6, 20))));
    let mut last = (f0.col, f0.row, 100u16, 30u16);

    for k in 0..280u16 {
        let col = 20 + k % 70;
        let mut spans = vec![(0u16, 0u16); 20];
        spans[6] = (0, col);
        pet.sense_ink(0, &spans, Some(6));
        t += Duration::from_millis(16);
        let _ = lawful_tick(&mut pet, sense(t, Some((6, col))), &mut last);
    }

    for i in 0..120u16 {
        let spans = vec![(0u16, 0u16); 20];
        pet.sense_ink(0, &spans, Some(6));
        t += Duration::from_millis(16);
        let _ = lawful_tick(&mut pet, sense(t, Some((6, 2 + i % 3))), &mut last);
    }

    for i in 0..240u16 {
        let mut spans = vec![(0u16, 0u16); 20];
        let end = if i % 2 == 0 { 70 } else { 4 };
        spans[4..9].fill((0, end));
        pet.sense_ink(0, &spans, Some(6));
        t += Duration::from_millis(16);
        let _ = lawful_tick(&mut pet, sense(t, Some((6, 3))), &mut last);
    }

    pet.sense_ink(0, &[(0u16, 0u16); 20], Some(6));
    t += Duration::from_millis(16);
    let f = lawful_tick(&mut pet, sense(t, None), &mut last);
    assert!(f.alpha > 100, "fixture: still plainly on the glass");
    for _ in 0..90 {
        t += Duration::from_millis(16);
        let _ = lawful_tick(&mut pet, sense(t, Some((22, 80))), &mut last);
    }

    for (cols, rows, frames) in [(40u16, 12u16, 90), (100, 30, 90)] {
        for _ in 0..frames {
            t += Duration::from_millis(16);
            let mut s = sense(t, Some((6, 20)));
            s.cols = cols;
            s.rows = rows;
            let _ = lawful_tick(&mut pet, s, &mut last);
        }
    }

    for _ in 0..900 {
        t += Duration::from_millis(16);
        let _ = lawful_tick(&mut pet, sense(t, Some((6, 20))), &mut last);
    }
}

#[test]
fn a_re_aim_in_the_last_frames_of_an_arc_is_dropped_not_solved() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 6, 20);
    t += Duration::from_millis(16);
    let _ = pet.tick(sense(t, Some((6, 20 + POUNCE_JUMP as u16 + 4))));
    let caret = 20 + POUNCE_JUMP as u16 + 4;
    let mut airborne = 0;
    while pet.flight.is_none() && airborne < 30 {
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((6, caret))));
        airborne += 1;
    }
    let f = pet.flight.expect("fixture: the pounce got off the ground");
    let aimed = f.to_col;
    let mut last = pet.col;
    while pet
        .flight
        .is_some_and(|f| (f.t / f.dur).clamp(0.0, 1.0) <= REAIM_MAX_U)
    {
        t += Duration::from_millis(16);
        last = pet.tick(sense(t, Some((6, caret)))).col;
    }
    assert!(
        pet.flight.is_some(),
        "fixture: still airborne past the bar, with frames left to draw"
    );
    t += Duration::from_millis(16);
    let f = pet.tick(sense(t, Some((6, 2))));
    assert!(
        f.col.is_finite() && f.row.is_finite(),
        "the dropped re-aim left the cat somewhere real, got {} / {}",
        f.col,
        f.row
    );
    assert!(
        (f.col - last).abs() < 2.0,
        "and it did not swerve on the way down ({last} → {})",
        f.col
    );
    assert_eq!(
        pet.flight.map(|f| f.to_col),
        Some(aimed),
        "the flight keeps the target it was already landing on"
    );
    for _ in 0..30 {
        t += Duration::from_millis(16);
        let prev = last;
        let f = pet.tick(sense(t, Some((6, 2))));
        last = f.col;
        assert!(
            f.col.is_finite() && (f.col - prev).abs() < 2.0,
            "the arc finished without a teleport ({prev} → {})",
            f.col
        );
    }
}

#[test]
fn a_failure_never_fires_the_party_ring() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 10);
    let (mut t, _) = idle(&mut pet, t, (4, 12), 0.5);
    pet.note_command_done(t, true, Some(3000));
    assert!(pet.grieving(), "the note opens the grief window");
    let mut saw_droop = false;
    for _ in 0..200 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 12))));
        if f.action == PetAction::Droop {
            saw_droop = true;
            assert!(pet.grieving(), "the droop IS the grief window");
        }
        for m in f.motes.iter().flatten() {
            assert!(
                !matches!(m.kind, PetMoteKind::Note | PetMoteKind::Heart),
                "no party mote may share the glass with grief"
            );
        }
    }
    assert!(saw_droop, "the droop must have played");
    assert!(!pet.grieving(), "and the window closes when it ends");
}

#[test]
fn grief_replays_beside_the_new_prompt() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let w = art_cols(10, 20);
    let t = awake(&mut pet, start, 4, 10);
    let (mut t, _) = idle(&mut pet, t, (4, 12), 0.5);
    pet.note_command_done(t, true, Some(3000));
    let mut f = pet.tick(sense(t + Duration::from_millis(16), Some((4, 12))));
    t += Duration::from_millis(16);
    for _ in 0..40 {
        t += Duration::from_millis(16);
        f = pet.tick(sense(t, Some((4, 12))));
        if f.action == PetAction::Droop {
            break;
        }
    }
    assert_eq!(f.action, PetAction::Droop, "fixture: mid-droop");
    // The prompt prints under it and the caret advances. A caret a program moved may
    // not stay where it went ([`SETTLE`]): the sulk holds that long, then leaves.
    let inked_to = (f.col + w + 3.0) as u16;
    let spans = vec![(0u16, inked_to); 6];
    pet.sense_ink(0, &spans, Some(4));
    let mut waited = 0.0f32;
    let f = loop {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, inked_to))));
        if f.action != PetAction::Droop {
            break f;
        }
        waited += 0.016;
        assert!(
            waited <= SETTLE + 0.02,
            "the droop waits out the caret's settle, and no longer ({waited} s)"
        );
    };
    assert_eq!(
        f.action,
        PetAction::Leap,
        "the droop LEAVES the words instead of blinking off them"
    );
    assert_eq!(
        pet.resume,
        Some((PetAction::Droop, 0.0)),
        "and the hop owes the droop back, from the top of its beat"
    );
    let mut replayed = false;
    for _ in 0..90 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, inked_to))));
        assert!(
            pet.grieving(),
            "the grief window stays shut over the fanfare for the whole \
             trip, got {:?}",
            f.action
        );
        if f.action == PetAction::Droop {
            assert!(
                !on_ink(&f, (0, inked_to), w),
                "replayed on blank ground (col {})",
                f.col
            );
            replayed = true;
            break;
        }
    }
    assert!(replayed, "the borrowed droop must actually come back");
    let (_, f) = idle(&mut pet, t, (4, inked_to), DROOP_HOLD * 0.8);
    assert_eq!(
        f.action,
        PetAction::Droop,
        "the hold restarted so the grief can actually be read"
    );
}

#[test]
fn a_streaming_pane_is_followed_with_or_without_shell_integration() {
    let lag = |burst: bool| -> f32 {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        pet.sense_ink(0, &[(0, 13), (0, 13), (0, 13)], Some(2));
        let t = awake(&mut pet, start, 2, 10);
        let (mut t, f) = idle(&mut pet, t, (2, 12), SIT_AFTER + 0.4);
        assert!(f.action.settled(), "fixture: a settled cat");
        let mut spans = vec![(0u16, 13u16); 3];
        let mut worst = 0.0f32;
        for edge in 3u16..=20 {
            spans.push((0, 14));
            pet.sense_ink(0, &spans, Some(edge));
            for _ in 0..12 {
                t += Duration::from_millis(16);
                let mut s = sense(t, Some((edge, 15)));
                s.output_burst = burst;
                let f = pet.tick(s);
                worst = worst.max(f32::from(edge) - f.row);
            }
        }
        worst
    };
    let (without, with) = (lag(false), lag(true));
    assert!(
        without <= 4.0,
        "a log follows without shell integration too, worst lag {without} rows"
    );
    assert!(
        (without - with).abs() < 0.01,
        "and the SAME log gives the same animal either way: \
         {without} without OSC 133, {with} with it"
    );
}

#[test]
fn the_watcher_hugs_the_live_edge() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 2, 10);
    let (mut t, f) = idle(&mut pet, t, (2, 12), SIT_AFTER + 0.4);
    assert!(f.action.settled(), "fixture: a settled cat");
    let mut spans = vec![(0u16, 13u16); 3];
    let mut f = f;
    let mut worst_lag = 0.0f32;
    for edge in 3u16..=10 {
        spans.push((0, 14));
        pet.sense_ink(0, &spans, Some(edge));
        for _ in 0..12 {
            t += Duration::from_millis(16);
            let mut s = sense(t, Some((2, 12)));
            s.output_burst = true;
            f = pet.tick(s);
            worst_lag = worst_lag.max(f32::from(edge) - f.row);
        }
    }
    assert!(
        worst_lag <= 4.0,
        "the watcher must FOLLOW the stream, worst lag {worst_lag} rows"
    );
    assert!(
        10.0 - f.row <= WATCH_HUG_ROWS,
        "mid-stream the lag is bounded by the hug's own hysteresis: \
         feet at row {}, edge 10",
        f.row
    );
    for _ in 0..40 {
        t += Duration::from_millis(16);
        let mut s = sense(t, Some((2, 12)));
        s.output_burst = true;
        f = pet.tick(s);
    }
    assert!(
        (f.row - 10.0).abs() < WATCH_HUG_ROWS,
        "the watcher must hug the newest inked row: feet at row {}, edge 10",
        f.row
    );
    assert!(
        f.col >= 14.0,
        "and stand clear of the line's ink, col {}",
        f.col
    );
}

#[test]
fn the_zees_die_before_the_ink() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 10);
    let spans = vec![(0u16, 40u16), (0, 40), (0, 40), (0, 0), (0, 0)];
    pet.sense_ink(0, &spans, Some(2));
    let (mut t, f) = idle(&mut pet, t, (4, 12), SLEEP_AFTER + 0.2);
    assert_eq!(f.action, PetAction::Sleep, "fixture: light sleep");
    for _ in 0..((ZEE_EVERY * 3.0 / 0.016) as u32) {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 12))));
        for m in f.motes.iter().flatten() {
            if matches!(m.kind, PetMoteKind::Zee | PetMoteKind::ZeePop) && m.alpha > 0 {
                assert!(
                    m.row >= 3.0,
                    "a visible z must never enter the inked band: row {}",
                    m.row
                );
            }
        }
    }
}

#[test]
fn the_peck_stance_clears_the_caret_cell() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 10);
    let (t, _) = type_run(&mut pet, t, 4, 12, 3, 0.06);
    let (_, f) = idle(&mut pet, t, (4, 15), SIT_AFTER + 0.5);
    assert_eq!(f.pose, PetGlyphId::PetSitFront, "fixture: the stare");
    assert!(
        f.col >= 15.0 + STATION_LEAD + PECK_CLEAR - 0.01,
        "the face-on sit parks clear of the caret cell, col {}",
        f.col
    );
}

#[test]
fn an_unfed_ink_map_changes_nothing() {
    let start = Instant::now();
    let mut fed = PetBrain::default();
    let mut unfed = PetBrain::default();
    fed.sense_ink(0, &[], None);
    let script: &[(u16, u16, f32)] = &[
        (4, 12, 0.5),
        (4, 40, 1.0),
        (5, 2, 0.8),
        (5, 30, SIT_AFTER + 1.0),
    ];
    let mut t = start;
    for &(row, col, dwell) in script {
        let end = t + Duration::from_secs_f32(dwell);
        while t < end {
            t += Duration::from_millis(16);
            let a = fed.tick(sense(t, Some((row, col))));
            let b = unfed.tick(sense(t, Some((row, col))));
            assert_eq!(a.fp(), b.fp(), "empty map ⇒ identical frames");
        }
    }
}

#[test]
fn tease_earns_a_running_chase_then_dignity() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = warm_settled(&mut pet, start);
    let base = pet.col + 12.0;
    let mut chased = false;
    let mut ran = false;
    let mut groomed = false;
    let mut phase = 0.0f32;
    for _ in 0..700 {
        t += Duration::from_millis(16);
        phase += 0.04;
        let px = base + 8.0 * phase.sin();
        let f = ptick(&mut pet, t, (4, 44), Some((px, 4.0)));
        if chased && pet.pursuit_t.is_none() {
            groomed |= f.action == PetAction::Groom;
        }
        if pet.pursuit_t.is_some() {
            chased = true;
            if matches!(f.action, PetAction::Walk | PetAction::Run) {
                ran = true;
            }
            assert!(
                pet.flight.is_none(),
                "a chase runs on paws: the standing-gap doors stay shut"
            );
        }
    }
    assert!(chased, "the tease must bait a chase");
    assert!(ran, "and the chase must actually run");
    assert!(pet.pursuit_t.is_none(), "stamina breaks the chase off");
    assert!(
        pet.pursuit_cool > 0.0,
        "and the cooldown guards the pet's dignity"
    );
    groomed |= pet.action == PetAction::Groom;
    for _ in 0..500 {
        t += Duration::from_millis(16);
        let f = ptick(&mut pet, t, (4, 44), None);
        groomed |= f.action == PetAction::Groom;
    }
    assert!(groomed, "the break-off owes the groom of dignity");
    assert!(!pet.groom_owed, "and the debt is paid, not pinned");
}

#[test]
fn typing_aborts_the_chase_instantly() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = warm_settled(&mut pet, start);
    let base = pet.col + 12.0;
    let mut phase = 0.0f32;
    for _ in 0..120 {
        t += Duration::from_millis(16);
        phase += 0.04;
        let px = base + 8.0 * phase.sin();
        let _ = ptick(&mut pet, t, (4, 44), Some((px, 4.0)));
        if pet.pursuit_t.is_some() {
            break;
        }
    }
    assert!(pet.pursuit_t.is_some(), "fixture: the chase is live");
    t += Duration::from_millis(16);
    let _ = ptick(&mut pet, t, (4, 52), Some((base, 4.0)));
    assert!(pet.pursuit_t.is_none(), "typing ends the game instantly");
    for _ in 0..30 {
        t += Duration::from_millis(16);
        phase += 0.04;
        let px = base + 8.0 * phase.sin();
        let _ = ptick(&mut pet, t, (4, 52), Some((px, 4.0)));
        assert!(
            pet.pursuit_t.is_none(),
            "the chase stays down while typing is recent"
        );
    }
}

#[test]
fn creeping_toy_pins_the_stakeout() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = warm_settled(&mut pet, start);
    let mut px = pet.col + 8.0;
    let mut staked = false;
    for k in 0..300 {
        t += Duration::from_millis(16);
        px += if k % 40 < 20 { 0.05 } else { -0.05 };
        let f = ptick(&mut pet, t, (4, 44), Some((px, 4.0)));
        if pet.stakeout {
            staked = true;
            assert_eq!(
                f.action,
                PetAction::Crouch,
                "the stakeout is the hunting crouch"
            );
        }
    }
    assert!(staked, "a creeping toy must pin the stakeout");
    t += Duration::from_millis(16);
    let f = ptick(&mut pet, t, (4, 44), None);
    assert!(!pet.stakeout, "no toy, no stakeout");
    assert_ne!(f.action, PetAction::Leap, "and never a phantom launch");
}

#[test]
fn far_peek_gets_a_look_not_a_trip() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 12);
    let (mut t, _) = idle(&mut pet, t, (4, 14), SIT_AFTER + 0.4);
    let col0 = pet.col;
    pet.note_peek(t, col0 + 15.0, 4.0);
    assert!(pet.pending_bat.is_none(), "out of paw reach: no visit");
    assert!(pet.pending_look.is_some(), "but well worth a look");
    t += Duration::from_millis(16);
    let f = pet.tick(sense(t, Some((4, 14))));
    assert_eq!(f.action, PetAction::Perk, "the look is the double-take");
    assert!(!pet.facing_left, "faced toward the peek");
    assert!(
        (pet.col - col0).abs() < 0.5,
        "and the paws never left the station"
    );
}

#[test]
fn non_finite_inputs_never_poison_the_pose() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 12);
    let (mut t, _) = idle(&mut pet, t, (4, 14), 0.5);
    pet.note_peek(t, f32::NAN, 4.0);
    assert!(pet.pending_bat.is_none() && pet.pending_look.is_none());
    for _ in 0..20 {
        t += Duration::from_millis(16);
        let f = ptick(&mut pet, t, (4, 14), Some((f32::NAN, f32::INFINITY)));
        assert!(f.col.is_finite(), "the pose stays finite");
    }
    assert_eq!(pet.pointer_heat, 0.0, "a NaN pointer is no pointer");
}

#[test]
fn a_long_rally_earns_the_tennis_watch() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = warm_settled(&mut pet, start);
    let (mut col, mut right) = (44i32, true);
    let mut watched = false;
    let (mut faced_left, mut faced_right) = (false, false);
    for _ in 0..60 {
        t += Duration::from_millis(150);
        right = !right;
        col += if right { 3 } else { -3 };
        let f = pet.tick(sense(t, Some((4, col as u16))));
        if pet.tennis {
            watched = true;
            assert_eq!(f.action, PetAction::Sit, "the watch sits");
            faced_left |= pet.facing_left;
            faced_right |= !pet.facing_left;
        }
    }
    assert!(watched, "a sustained rally must earn the watch");
    assert!(
        faced_left && faced_right,
        "and the facing follows the ball both ways"
    );
    let (_, _) = idle(&mut pet, t, (4, col as u16), TENNIS_LAPSE + 0.5);
    assert!(!pet.tennis, "the rally over, the watch stands down");
}

#[test]
fn a_bored_cat_hides_behind_a_word_and_comes_back() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = warm_settled(&mut pet, start);
    let mut spans = [(0u16, 0u16); 12];
    let word_row = pet.row as usize;
    let seat = pet.col.round() as u16;
    spans[word_row] = (seat - 11, seat - 3);
    let mut hid = false;
    let mut under = false;
    for _ in 0..900 {
        t += Duration::from_millis(16);
        pet.sense_ink(0, &spans, None);
        if !pet.hiding && pet.hide_to.is_none() {
            pet.mote_serial = 3;
            pet.content = pet.content.max(0.9);
        }
        let f = pet.tick(sense(t, Some((4, 44))));
        if pet.hiding {
            hid = true;
            under |= f.under_ink;
        }
    }
    assert!(hid, "the deal must send the cat behind the word");
    assert!(under, "and back there it draws UNDER the glyphs");
    assert!(!pet.hiding, "the dwell ends the lurk");
    let (_, f) = idle(&mut pet, t, (4, 44), 2.0);
    assert!(!f.under_ink, "surfaced on the walk home");
}

fn hide_behind(pet: &mut PetBrain, start: Instant, off: u16) -> (Instant, [(u16, u16); 12]) {
    let mut t = warm_settled(pet, start);
    let mut spans = [(0u16, 0u16); 12];
    let seat = pet.col.round() as u16;
    spans[pet.row as usize] = (seat - 8 - off, seat - off);
    for _ in 0..900 {
        t += Duration::from_millis(16);
        pet.sense_ink(0, &spans, None);
        if !pet.hiding && pet.hide_to.is_none() {
            pet.mote_serial = 3;
            pet.content = pet.content.max(0.9);
        }
        let _ = pet.tick(sense(t, Some((4, 44))));
        if pet.hiding {
            return (t, spans);
        }
    }
    panic!("fixture: the deal must send the cat behind the word");
}

#[test]
fn a_hiding_cat_holds_its_crouch_off_the_frame_train() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let (mut t, spans) = hide_behind(&mut pet, start, 3);
    let hid_at = t;
    while pet.needs_frames() {
        assert!(
            t - hid_at < Duration::from_millis(500),
            "the crouch must leave the train once the veil has eased (veil {:?})",
            pet.letter_veil
        );
        t += Duration::from_millis(16);
        pet.sense_ink(0, &spans, None);
        let _ = pet.tick(sense(t, Some((4, 44))));
    }
    assert!(pet.hiding, "fixture: still behind the word");
    let end = t + Duration::from_secs_f32(pet.hide_t);
    let mut ticks = 0;
    let mut f = None;
    while pet.hiding {
        assert!(!pet.needs_frames(), "the held crouch stays off the train");
        let due = pet
            .next_change_deadline(t)
            .expect("the dwell's end is offered");
        assert!(
            due <= end + Duration::from_millis(50),
            "never past the dwell's end"
        );
        t = due;
        pet.sense_ink(0, &spans, None);
        f = Some(pet.tick(sense(t, Some((4, 44)))));
        ticks += 1;
        assert!(
            ticks <= 20,
            "the dwell is served by its offers, not a train"
        );
    }
    assert!(t >= end, "the dwell is served in full");
    assert_eq!(
        f.map(|f| f.action),
        Some(PetAction::Stand),
        "and the cat stands up"
    );
    let mut owed = false;
    for _ in 0..3 {
        t += Duration::from_millis(16);
        pet.sense_ink(0, &spans, None);
        let _ = pet.tick(sense(t, Some((4, 44))));
        owed |= pet.needs_frames();
    }
    assert!(owed, "the walk home is on the train");
}

#[test]
fn a_cat_surfacing_from_a_hide_walks_home_from_rest() {
    for off in [3u16, 6] {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let (mut t, spans) = hide_behind(&mut pet, start, off);
        while pet.hiding {
            t += Duration::from_millis(16);
            pet.sense_ink(0, &spans, None);
            let _ = pet.tick(sense(t, Some((4, 44))));
        }
        let from = pet.col;
        let mut prev = (pet.col, 0.0f32);
        let mut last = None;
        for _ in 0..150 {
            t += Duration::from_millis(16);
            pet.sense_ink(0, &spans, None);
            let f = pet.tick(sense(t, Some((4, 44))));
            assert_ne!(f.action, PetAction::Leap, "off {off}: walked, not hopped");
            let v = (f.col - prev.0) / 0.016;
            if matches!(f.action, PetAction::Stand | PetAction::Crouch) {
                assert!(
                    (f.col - prev.0).abs() < 1e-4,
                    "off {off}: the {:?} pose slid {}",
                    f.action,
                    f.col - prev.0
                );
            }
            assert!(
                (v - prev.1).abs() <= 6.0,
                "off {off}: the speed jumped {} -> {v} cells/s in one frame",
                prev.1
            );
            prev = (f.col, v);
            last = Some(f);
        }
        let f = last.expect("ticked");
        assert!(
            f.action.settled() && f.col > from + 2.0,
            "off {off}: home again ({:?} at {} from {from})",
            f.action,
            f.col
        );
    }
}

#[test]
fn a_commit_stare_on_the_letters_steps_off_them_at_once() {
    for off in [3u16, 6] {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let (mut t, spans) = hide_behind(&mut pet, start, off);
        while pet.hiding {
            t += Duration::from_millis(16);
            pet.sense_ink(0, &spans, None);
            let _ = pet.tick(sense(t, Some((4, 44))));
        }
        assert_eq!(
            pet.action,
            PetAction::Stand,
            "off {off}: fixture: surfaced standing"
        );
        assert!(
            pet.ink_overlaps(pet.col, pet.row, art_cols(10, 20)),
            "off {off}: fixture: on the letters"
        );
        pet.commit_t = COMMIT_STARE;
        let from = pet.col;
        let mut left = None;
        for k in 0..20usize {
            t += Duration::from_millis(16);
            pet.sense_ink(0, &spans, None);
            let f = pet.tick(sense(t, Some((4, 44))));
            if (f.col - from).abs() > 1e-3 || f.action == PetAction::Leap {
                left = Some(k);
                break;
            }
        }
        assert!(
            left.is_some_and(|k| k <= 1),
            "off {off}: the stare kept the cat on the letters ({left:?} frames)"
        );
    }
}

#[test]
fn a_spent_watch_rearms_once_the_stream_ends() {
    let mut pet = PetBrain {
        watch_heat: 1.0,
        watch_spent: true,
        alpha: 1.0,
        ..PetBrain::default()
    };
    assert!(
        pet.needs_frames(),
        "live heat keeps the lane so the decay can run"
    );
    let start = Instant::now();
    let mut t = start;
    let _ = pet.tick(sense(t, Some((4, 14))));
    for _ in 0..80 {
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((4, 14))));
    }
    assert!(
        pet.watch_heat < WATCH_GATE,
        "the heat decayed under the gate"
    );
    assert!(!pet.watch_spent, "and the next stream earns a fresh stare");
}

#[test]
fn every_idle_deal_goes_byte_stable_by_the_breath_window() {
    let start = Instant::now();
    for seed in 0..12u8 {
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (mut t, _) = idle(&mut pet, t, (5, 0), 1.6);
        assert!(
            pet.action.settled(),
            "hand {seed}: fixture: settled after the Enter"
        );
        pet.force_seed(seed);
        t += Duration::from_millis(16);
        let mut s = sense(t, Some((5, 0)));
        s.output_burst = true;
        let _ = pet.tick(s);
        let (t, f) = idle(&mut pet, t, (5, 0), SLEEP_AFTER + BREATH_WINDOW + 2.0);
        assert_eq!(f.action, PetAction::Sleep, "hand {seed}: deeply asleep");
        assert!(
            !pet.needs_frames(),
            "hand {seed}: every idle envelope died inside the settle window"
        );
        assert!(pet.is_active(), "hand {seed}: still on screen, just still");
        assert!(
            f.motes.iter().all(Option::is_none),
            "hand {seed}: the mote lane is empty"
        );
        let a = pet.tick(sense(t + Duration::from_secs(1), Some((5, 0))));
        let b = pet.tick(sense(t + Duration::from_secs(2), Some((5, 0))));
        assert_eq!(a.fp(), b.fp(), "hand {seed}: deep sleep is byte-stable");
    }
}

#[test]
fn no_idle_beat_moves_the_body() {
    let start = Instant::now();
    for seed in 0..12u8 {
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        pet.force_seed(seed);
        let mut t = t;
        let mut anchor: Option<(f32, f32)> = None;
        let mut prev_col: Option<f32> = None;
        let end = t + Duration::from_secs_f32(SLEEP_AFTER + BREATH_WINDOW + 1.0);
        while t < end {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 12))));
            if !f.action.settled() {
                anchor = None;
                prev_col = None;
                continue;
            }
            assert!(
                f.lift == 0.0,
                "hand {seed}: a settled {:?} frame lifted the body by {}",
                f.action,
                f.lift
            );
            if let Some(c) = prev_col {
                assert!(
                    (f.col - c).abs() <= 0.01,
                    "hand {seed}: a settled {:?} frame STEPPED the feet by {}",
                    f.action,
                    (f.col - c).abs()
                );
            }
            prev_col = Some(f.col);
            match anchor {
                None => anchor = Some((f.col, f.row)),
                Some((c, r)) => {
                    assert!(
                        (f.row - r).abs() < 1e-6,
                        "hand {seed}: a settled {:?} frame changed the row: {r} -> {}",
                        f.action,
                        f.row
                    );
                    assert!(
                        (f.col - c).abs() <= PECK_CLEAR + 1e-3,
                        "hand {seed}: a settled {:?} frame moved the feet {} cells \
                         — past the seat's own clearance",
                        f.action,
                        (f.col - c).abs()
                    );
                }
            }
        }
        assert!(
            anchor.is_some(),
            "hand {seed}: fixture: the run ends settled"
        );
    }
}

fn pose_runs(
    pet: &mut PetBrain,
    start: Instant,
    caret: (u16, u16),
    secs: f32,
) -> (Instant, Vec<(PetGlyphId, f32, u32)>) {
    let mut t = start;
    let end = start + Duration::from_secs_f32(secs);
    let mut runs: Vec<(PetGlyphId, f32, u32)> = Vec::new();
    while t < end {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some(caret)));
        match runs.last_mut() {
            Some(r) if r.0 == f.pose => r.2 += 1,
            _ => runs.push((f.pose, pet.quiet, 1)),
        }
    }
    (t, runs)
}

#[test]
fn reduced_motion_holds_two_still_frames_and_no_motes() {
    let mut pet = PetBrain::default();
    let mut t = Instant::now();
    let mut poses = std::collections::BTreeSet::new();
    for k in 0..((SLEEP_AFTER + 4.0) / 0.016) as u32 {
        t += Duration::from_millis(16);
        let col = 20 + u16::try_from(k.min(60) / 6).unwrap_or(0);
        let mut s = sense(t, Some((4, col)));
        s.reduced_motion = true;
        let f = pet.tick(s);
        poses.insert(f.pose as u8);
        assert_eq!(f.lift, 0.0);
        assert!(
            f.motes.iter().all(Option::is_none),
            "reduced motion must never particle"
        );
    }
    assert_eq!(
        poses,
        [PetGlyphId::PetSit as u8, PetGlyphId::PetSleep0 as u8]
            .into_iter()
            .collect(),
        "reduced motion has exactly two frames: seated and asleep"
    );
}

#[test]
fn every_dealt_idle_frame_holds_at_least_two_ticks() {
    let start = Instant::now();
    let mut seen = std::collections::HashSet::new();
    for seed in 0..12u8 {
        for (label, caret, warm) in [
            ("the plain seat", (4u16, 99u16), (4u16, 97u16)),
            ("eye contact", (4, 14), (4, 12)),
        ] {
            let mut pet = PetBrain::default();
            let t = awake(&mut pet, start, warm.0, warm.1);
            pet.force_seed(seed);
            let (_, runs) = pose_runs(&mut pet, t, caret, SLEEP_AFTER + 1.0);
            seen.extend(runs.iter().map(|(pose, ..)| *pose));
            for (pose, at, ticks) in runs.iter().take(runs.len() - 1) {
                assert!(
                    *ticks >= 2,
                    "hand {seed} ({label}): {pose:?} flashed for {ticks} tick(s) \
                     at quiet {at}"
                );
            }
        }
    }
    for beat in [
        PetGlyphId::PetSitBlink,
        PetGlyphId::PetSitFrontBlink,
        PetGlyphId::PetSitFlick,
        PetGlyphId::PetSitTailBack,
        PetGlyphId::PetSitTailLow,
        PetGlyphId::PetYawn,
        PetGlyphId::PetGroomFlank,
        PetGlyphId::PetLoafOpen,
    ] {
        assert!(seen.contains(&beat), "{beat:?} is never dealt in a settle");
    }
}

const PROBE_FRAME: Duration = Duration::from_micros(16_667);
const PROBE_ROWS: u16 = 24;
const PROBE_STILL: f32 = 0.1;

#[derive(Clone, Copy, Debug)]
struct ProbeFlight {
    at: f32,
    site: &'static str,
    from_col: f32,
    to_col: f32,
    from_row: f32,
    to_row: f32,
    dur: f32,
    arc: f32,
    big: bool,
    show: bool,
}

struct Probe {
    pet: PetBrain,
    t: Instant,
    t0: Instant,
    cols: u16,
    spans: Vec<(u16, u16)>,
    live: Option<u16>,
    last: Option<PetFrame>,
    trigger: Option<f32>,
    at_trigger: Option<PetFrame>,
    log_from: f32,
    flights: Vec<ProbeFlight>,
    actions: Vec<(f32, PetAction)>,
    max_dcol: (f32, f32, PetAction),
    max_drow: (f32, f32, PetAction),
    settled_since: Option<f32>,
    settle_at: Option<f32>,
    settle_where: (f32, f32, PetAction),
    judge: bool,
    frame_dur: Duration,
    law: (f32, f32, u16, u16),
    dust: bool,
    episodes: u32,
    touchdown: Option<f32>,
    wiggle: bool,
    trace: Vec<(f32, PetFrame)>,
    needs_always: bool,
    home_at: Option<f32>,
}

impl Probe {
    fn new(cols: u16) -> Self {
        Self::at(cols, Instant::now())
    }

    fn judged(cols: u16) -> Self {
        let mut p = Self::new(cols);
        p.judge = true;
        p.frame_dur = Duration::from_millis(16);
        p
    }

    fn at(cols: u16, t0: Instant) -> Self {
        Self {
            pet: PetBrain::default(),
            t: t0,
            t0,
            cols,
            spans: vec![(0, 0); usize::from(PROBE_ROWS)],
            live: None,
            last: None,
            trigger: None,
            at_trigger: None,
            log_from: 0.0,
            flights: Vec::new(),
            actions: Vec::new(),
            max_dcol: (0.0, 0.0, PetAction::Sit),
            max_drow: (0.0, 0.0, PetAction::Sit),
            settled_since: None,
            settle_at: None,
            settle_where: (0.0, 0.0, PetAction::Sit),
            judge: false,
            frame_dur: PROBE_FRAME,
            law: (0.0, 0.0, cols, PROBE_ROWS),
            dust: false,
            episodes: 0,
            touchdown: None,
            wiggle: false,
            trace: Vec::new(),
            needs_always: true,
            home_at: None,
        }
    }

    fn onsets(&self, a: PetAction) -> usize {
        let tr = self.trigger.unwrap_or(f32::MAX);
        self.actions
            .iter()
            .filter(|(at, x)| *at >= tr && *x == a)
            .count()
    }

    fn now(&self) -> f32 {
        self.t.saturating_duration_since(self.t0).as_secs_f32()
    }

    fn sense_at(&self, caret: Option<(u16, u16)>) -> PetSense {
        let mut s = sense(self.t, caret);
        s.cols = self.cols;
        s.rows = PROBE_ROWS;
        s
    }

    fn ink(&mut self, row: u16, first: u16, end: u16) {
        self.spans[usize::from(row)] = (first, end);
    }

    fn mark(&mut self) {
        self.trigger = Some(self.now());
        self.at_trigger = self.last;
        self.max_dcol = (0.0, 0.0, PetAction::Sit);
        self.home_at = None;
        self.max_drow = (0.0, 0.0, PetAction::Sit);
        self.settled_since = None;
        self.settle_at = None;
        self.dust = false;
        self.episodes = 0;
        self.touchdown = None;
        self.wiggle = false;
        self.trace.clear();
        self.needs_always = true;
    }

    fn grounded_at(&self) -> Option<f32> {
        if self.pet.flight.is_some() {
            return None;
        }
        self.touchdown
    }

    fn frame(&mut self, caret: Option<(u16, u16)>) -> PetFrame {
        let s = self.sense_at(caret);
        self.frame_with(s, caret)
    }

    fn frame_wrapped(&mut self, caret: Option<(u16, u16)>) -> PetFrame {
        let mut s = self.sense_at(caret);
        s.wrapped = true;
        self.frame_with(s, caret)
    }

    fn frame_with(&mut self, mut s: PetSense, caret: Option<(u16, u16)>) -> PetFrame {
        self.t += self.frame_dur;
        let now = self.now();
        s.now = self.t;
        self.pet.sense_ink(0, &self.spans, self.live);
        let w = art_cols(10, 20);
        let pre_action = self.pet.action;
        let pre_hop = self.pet.hop_crouch;
        let pre_gather = self.pet.big_gather;
        let pre_bound2 = self.pet.bound2.is_some();
        let pre_wall = self.pet.pending_wall_transit;
        let pre_play = self.pet.play_to.is_some();
        let pre_resume = self.pet.resume.is_some();
        let pre_flight = self.pet.flight;
        let pre_on_ink = self.pet.ink_overlaps(self.pet.col, self.pet.row, w);
        if self.last.is_none() {
            let f = self.pet.tick(s);
            self.law = (f.col, f.row, self.cols, PROBE_ROWS);
            self.last = Some(f);
            return f;
        }
        let f = if self.judge {
            lawful_tick(&mut self.pet, s, &mut self.law)
        } else {
            let f = self.pet.tick(s);
            self.law = (f.col, f.row, self.cols, PROBE_ROWS);
            f
        };

        if let Some(fl) = self.pet.flight {
            let launched = pre_flight.is_none();
            let reaimed = pre_flight.is_some_and(|p| (p.to_col - fl.to_col).abs() > 1e-6);
            if (launched || reaimed) && now >= self.log_from {
                let site = if reaimed {
                    "mid-air re-aim (rebase)"
                } else if fl.drop {
                    "sleeper's drop (Waking + pending_drop)"
                } else if fl.big && pre_action == PetAction::Land && pre_bound2 {
                    "bound 2 (bound2 -> begin_big_arc)"
                } else if pre_action == PetAction::Crouch && pre_gather {
                    "big door (big_gather -> launch_big)"
                } else if fl.big {
                    "big bound (launch_big)"
                } else if pre_action == PetAction::Crouch && pre_hop {
                    "row hop door (hop_crouch -> begin_flight vertical)"
                } else if pre_action == PetAction::Crouch && pre_play {
                    "play (Crouch -> launch)"
                } else if pre_action == PetAction::Crouch {
                    "pounce door (Crouch -> launch)"
                } else if self.pet.resume.is_some() && !pre_resume {
                    "ink re-anchor (begin_arc, flight_shape vertical)"
                } else if pre_on_ink
                    && matches!(
                        pre_action,
                        PetAction::Startle | PetAction::Frolic | PetAction::Perk
                    )
                {
                    "ink re-anchor (vertical; reaction consumed)"
                } else if pre_wall {
                    "wall transit (WALL_HOP_DUR arc, span-capped)"
                } else {
                    "unknown"
                };
                self.flights.push(ProbeFlight {
                    at: now,
                    site,
                    from_col: fl.from_col,
                    to_col: fl.to_col,
                    from_row: fl.from_row,
                    to_row: fl.to_row,
                    dur: fl.dur,
                    arc: fl.arc,
                    big: fl.big,
                    show: fl.show,
                });
            }
        }
        if now >= self.log_from && self.last.is_none_or(|l| l.action != f.action) {
            self.actions.push((now, f.action));
        }
        if let (Some(l), Some(tr)) = (self.last, self.trigger) {
            let dc = (f.col - l.col).abs();
            let dr = (f.row - l.row).abs();
            if dc > self.max_dcol.0 {
                self.max_dcol = (dc, now, f.action);
            }
            if dc > PROBE_STILL && self.settled_since.is_none() {
                self.home_at = Some(now - tr);
            }
            if dr > self.max_drow.0 {
                self.max_drow = (dr, now, f.action);
            }
            if f.action == PetAction::Leap && l.action != PetAction::Leap {
                self.episodes += 1;
            }
            if f.action != PetAction::Leap && l.action == PetAction::Leap {
                self.touchdown = Some(now - tr);
            }
            if f.motes
                .iter()
                .flatten()
                .any(|m| m.kind == PetMoteKind::Dust)
            {
                self.dust = true;
            }
            if f.pose == PetGlyphId::PetCrouchWiggle {
                self.wiggle = true;
            }
            self.trace.push((now - tr, f));
            self.needs_always &= self.pet.needs_frames();
        }
        if self.trigger.is_some() && self.settle_at.is_none() {
            let ok = caret.is_some_and(|c| {
                let (tc, tr) = self.pet.station_safe(c, self.cols, PROBE_ROWS, w);
                f.action.settled()
                    && self.pet.flight.is_none()
                    && (f.col - tc).abs() < 1.0
                    && (f.row - tr).abs() < 0.01
            });
            if ok {
                let since = *self.settled_since.get_or_insert(now);
                if now - since >= 0.5 {
                    self.settle_at = Some(since);
                    self.settle_where = (f.col, f.row, f.action);
                }
            } else {
                self.settled_since = None;
            }
        }
        self.last = Some(f);
        f
    }

    fn hold_frames(&mut self, caret: Option<(u16, u16)>, n: u32) {
        for _ in 0..n {
            let _ = self.frame(caret);
        }
    }

    fn hold(&mut self, caret: Option<(u16, u16)>, secs: f32) {
        let n = (secs / self.frame_dur.as_secs_f32()).round();
        assert!((0.0..1e6).contains(&n), "probe: bad hold {secs}");
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        self.hold_frames(caret, n as u32);
    }

    fn log_rows(&mut self, frames: u32, burst: bool, above: bool) {
        let last = PROBE_ROWS - 1;
        for k in 0..frames {
            let at_end = k % 2 == 0;
            if above {
                self.ink(last - 1, 0, self.cols);
            }
            if at_end {
                self.ink(last, 0, self.cols - 1);
            } else {
                self.ink(last, 0, 0);
            }
            self.live = Some(last);
            let caret = if at_end {
                (last, self.cols - 1)
            } else {
                (last, 0)
            };
            let mut s = self.sense_at(Some(caret));
            s.output_burst = burst;
            s.wrapped = !at_end;
            let _ = self.frame_with(s, Some(caret));
        }
    }

    fn type_to(&mut self, row: u16, from: u16, to: u16) {
        for c in (from + 1)..=to {
            self.ink(row, 0, c);
            let _ = self.frame(Some((row, c)));
            self.hold_frames(Some((row, c)), 5);
        }
    }

    fn warm(&mut self, row: u16, col0: u16) {
        let start = col0.saturating_sub(16);
        let mid = col0.saturating_sub(8);
        self.live = Some(row);
        self.ink(row, 0, start);
        let _ = self.frame(Some((row, start)));
        self.type_to(row, start, mid);
        self.hold(Some((row, mid)), 2.5);
        self.log_from = self.now();
        self.type_to(row, mid, col0);
    }
}

const fn offered_star(t: Instant, col: i32) -> StarCatch {
    StarCatch {
        x: col as f32 * 10.0 + 5.0,
        y: 4.0 * 20.0 - 3.0,
        born: t,
        col,
    }
}

#[test]
fn a_run_that_stops_for_a_star_stands_a_stride_first() {
    let stand = [PetGlyphId::PetStand, PetGlyphId::PetStandEar];
    let run = |offer_star: bool| {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (mut t, _) = type_run(&mut pet, t, 4, 12, 14, 0.07);
        let mut seen: Vec<(Instant, PetAction, PetGlyphId)> = Vec::new();
        let mut offer: Option<PetOffer> = None;
        for _ in 0..90 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 26))));
            if offer_star && offer.is_none() && stand.contains(&f.pose) {
                offer = Some(PetOffer {
                    catch: Some(offered_star(t, 34)),
                    ..PetOffer::default()
                });
            }
            seen.push((t, f.action, f.pose));
            if let Some(offer) = offer.as_ref() {
                let _ = pet.note_v2_offer(offer);
            }
        }
        seen
    };
    let bat = PetSpecies::Cat.skin(PetGlyphId::PetBat);
    let with = run(true);
    let swipe = with
        .iter()
        .position(|(_, a, p)| *a == PetAction::Frolic && *p == bat)
        .expect("the star is swiped at");
    let stood_from = with[..swipe]
        .iter()
        .rposition(|(_, _, p)| !stand.contains(p))
        .map_or(0, |i| i + 1);
    assert!(
        stood_from < swipe,
        "fixture: the run stops in its stand before the swipe: {:?}",
        &with[swipe.saturating_sub(4)..=swipe]
    );
    assert!(
        PetBrain::is_leg_pose(with[stood_from - 1].2),
        "fixture: the stand ended the run: {:?}",
        &with[stood_from - 1..=swipe]
    );
    let stood = (with[swipe].0 - with[stood_from].0).as_secs_f32();
    assert!(
        stood >= STAND_MIN - 1e-3,
        "the stand before the swipe lasted {:.0} ms, under a stride ({:.0} ms)",
        stood * 1e3,
        STAND_MIN * 1e3
    );
    let without = run(false);
    assert!(
        without.iter().any(|(_, _, p)| stand.contains(p)),
        "negative control: with no star the run stops in the same stand"
    );
}

#[test]
fn a_play_beat_is_kept_through_the_legs_hold() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 4, 10);
    let bat = PetSpecies::Cat.skin(PetGlyphId::PetBat);
    pet.set_action(PetAction::Frolic);
    pet.play_frolic = true;
    pet.play_hold = FROLIC_HOLD * 0.5 + 0.012;
    let mut seen: Vec<(Instant, PetAction, PetGlyphId)> = Vec::new();
    for i in 0..60u16 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 12 + (i / 6).min(8)))));
        seen.push((t, f.action, f.pose));
    }
    let first = seen
        .iter()
        .position(|(_, _, p)| *p == bat)
        .expect("fixture: the bow swings into its swipe");
    let next = seen[first..]
        .iter()
        .position(|(_, _, p)| *p != bat)
        .map(|i| first + i)
        .expect("fixture: the cat sets out after the caret");
    let on = (seen[next].0 - seen[first].0).as_secs_f32();
    assert!(
        on >= STILL_SECS - 1e-3,
        "the swipe was on glass {:.0} ms before {:?} replaced it",
        on * 1e3,
        seen[next]
    );
}

#[test]
fn the_meteor_s_perk_reaches_the_resident_at_the_landing_s_own_instant() {
    let start = Instant::now();
    let (mut pet, mut twin, mut t) = settled_pair(start, 5.0);
    assert!(pet.action.settled(), "fixture: settled ({:?})", pet.action);
    let perk_at = t + Duration::from_millis(90);
    let offer = PetOffer {
        perk_at: Some(perk_at),
        ..PetOffer::default()
    };
    let mut perk: Option<(Instant, PetFrame, PetFrame)> = None;
    let mut twin_perked = false;
    for _ in 0..40 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 12))));
        let g = twin.tick(sense(t, Some((4, 12))));
        assert!(pet.note_v2_offer(&offer).is_none(), "a perk is not a catch");
        twin_perked |= g.action == PetAction::Perk;
        if f.action == PetAction::Perk && perk.is_none() {
            perk = Some((t, f, g));
        }
    }
    let (at, f, g) = perk.expect("the offered perk edge reaches the pet");
    assert!(
        !twin_perked,
        "fixture: nothing but the offer can perk this cat"
    );
    assert!(
        at >= perk_at,
        "the perk landed {:?} BEFORE the arrival — that is the launch, not the landing",
        perk_at - at
    );
    assert!(
        at - perk_at < Duration::from_millis(17),
        "the perk landed {:?} after the arrival — later than the first frame at t0+T",
        at - perk_at
    );
    assert_eq!(
        (f.col.to_bits(), f.row.to_bits(), f.lift.to_bits()),
        (g.col.to_bits(), g.row.to_bits(), g.lift.to_bits()),
        "NO RELOCATION (D13, owner Q14): the offered cat is at ({}, {}) and the \
         un-offered twin at ({}, {}) on the arrival's own frame",
        f.col,
        f.row,
        g.col,
        g.row
    );
}

#[test]
fn a_gold_star_near_a_contented_cat_is_offered_caught_and_spent_once() {
    let start = Instant::now();
    let (mut pet, mut twin, mut t) = contented_pair(start, SIT_AFTER + 0.3);
    assert!(pet.purring(), "fixture: a contented cat ({:?})", pet.action);
    let star = offered_star(t, pet.col.round() as i32 + 4);
    let offer = PetOffer {
        catch: Some(star),
        ..PetOffer::default()
    };
    let mut caught: Vec<(PetFrame, PetFrame)> = Vec::new();
    let mut motes = 0usize;
    for _ in 0..300 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 72))));
        let g = twin.tick(sense(t, Some((4, 72))));
        if let Some(back) = pet.note_v2_offer(&offer) {
            assert_eq!(back, star, "the star handed back is the star offered");
            motes = f.motes.iter().flatten().count();
            caught.push((f, g));
        }
    }
    assert_eq!(
        caught.len(),
        1,
        "the offer stood for 300 frames and the star's life may shorten ONCE: {} catches",
        caught.len()
    );
    let (f, g) = caught[0];
    assert_eq!(
        f.action,
        PetAction::Frolic,
        "the catch is played as the reach, not taken invisibly"
    );
    assert_eq!(
        f.pose,
        PetSpecies::Cat.skin(PetGlyphId::PetBat),
        "the paw is out on the landing frame"
    );
    assert_eq!(
        motes,
        g.motes.iter().flatten().count(),
        "the reach draws NOTHING: it spawned a mote the un-offered twin did not"
    );
    assert_eq!(f.row.to_bits(), g.row.to_bits(), "the reach changed lines");
    assert!(
        (f.col - g.col).abs() < 0.05 && f.lift.to_bits() == g.lift.to_bits(),
        "a reach is not a trip: the reaching cat is at ({}, {}) and the un-offered \
         twin at ({}, {})",
        f.col,
        f.row,
        g.col,
        g.row
    );
    assert!(
        pet.flight.is_none()
            && pet.play_to.is_none()
            && pet.play_land.is_none()
            && !pet.pending_pounce
            && !pet.pending_big_jump,
        "the catch set travel: flight {:?}, to {:?}, land {:?}",
        pet.flight.is_some(),
        pet.play_to,
        pet.play_land.is_some()
    );
}

#[test]
fn the_purr_notes_wear_the_ribbon_s_colour_under_the_cat() {
    const RIBBON: u32 = 0x0000_7FFF;
    let start = Instant::now();
    let (mut lit, mut dark, mut t) = contented_pair(start, SIT_AFTER + 0.3);
    let under = PetOffer {
        mote_t: Some(0.5),
        mote_rgb: Some(RIBBON),
        ..PetOffer::default()
    };
    let none = PetOffer::default();
    let (mut lit_note, mut dark_note) = (None, None);
    for _ in 0..800 {
        t += Duration::from_millis(16);
        let a = lit.tick(sense(t, Some((4, 72))));
        let b = dark.tick(sense(t, Some((4, 72))));
        assert!(lit.note_v2_offer(&under).is_none());
        assert!(dark.note_v2_offer(&none).is_none());
        let purr = |f: &PetFrame| {
            f.motes
                .iter()
                .flatten()
                .find(|m| matches!(m.kind, PetMoteKind::Note | PetMoteKind::Heart))
                .copied()
        };
        if lit_note.is_none() {
            lit_note = purr(&a);
        }
        if dark_note.is_none() {
            dark_note = purr(&b);
        }
        if lit_note.is_some() && dark_note.is_some() {
            break;
        }
    }
    let lit_note = lit_note.expect("fixture: the lit seat floats a ♪ or ♥");
    let dark_note = dark_note.expect("fixture: the dark seat floats a ♪ or ♥");
    assert_eq!(
        lit_note.rgb,
        Some(RIBBON),
        "a {:?} over a lit ribbon wears the ribbon's stop",
        lit_note.kind
    );
    assert_eq!(
        dark_note.rgb, None,
        "a {:?} with no ribbon under the cat keeps the pet's own colour",
        dark_note.kind
    );
}

#[test]
fn the_offer_buys_no_wake() {
    let start = Instant::now();
    let (mut pet, _twin, mut t) = settled_pair(start, 5.0);
    assert!(!pet.needs_frames(), "fixture: a parked cat");
    let horizon = pet
        .next_change_deadline(t)
        .expect("fixture: a settled cat names its next step");
    let perk_at = t + Duration::from_millis(400);
    let offer = PetOffer {
        perk_at: Some(perk_at),
        catch: Some(offered_star(t, pet.col.round() as i32 + 3)),
        mote_t: Some(0.5),
        mote_rgb: Some(0x00FF_0000),
    };
    assert!(
        pet.note_v2_offer(&offer).is_none(),
        "noting is not catching"
    );
    assert!(
        !pet.needs_frames(),
        "an offer must never pin the frame lane"
    );
    assert_eq!(
        pet.next_change_deadline(t),
        Some(horizon),
        "an offer must never move the idle horizon"
    );
    let mut reached = false;
    for _ in 0..40 {
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((4, 12))));
        reached |= pet.note_v2_offer(&offer).is_some();
    }
    assert!(reached, "a latch that buys no wake is still a latch");
}

fn drive_room(
    pet: &mut PetBrain,
    start: Instant,
    caret: (u16, u16),
    secs: f32,
    executing: bool,
) -> (u32, bool, Option<Instant>, Instant) {
    let end = start + Duration::from_secs_f32(secs);
    let (mut wakes, mut slept, mut crouched) = (0u32, false, None);
    let mut t = start;
    while t < end {
        let next = if pet.needs_frames() {
            t + Duration::from_millis(16)
        } else {
            let Some(d) = pet.next_change_deadline(t) else {
                break;
            };
            assert!(d > t, "a deadline at `now` is a busy re-arm");
            d
        };
        t = next;
        wakes += 1;
        pet.note_executing(t, executing);
        let f = pet.tick(sense(t, Some(caret)));
        slept |= f.action == PetAction::Sleep;
        if pet.vigil && pet.action == PetAction::Crouch && crouched.is_none() {
            crouched = Some(t);
        }
    }
    (wakes, slept, crouched, t)
}

#[test]
fn a_driven_session_s_cat_keeps_its_distance_and_walks_back_at_release() {
    let start = Instant::now();
    let (mut pet, mut twin, t0) = settled_pair(start, 3.0);
    pet.note_room_lease(true);
    let mut t = t0;
    let mut step_max = 0.0f32;
    let mut last = pet.col;
    for k in 1..=6u16 {
        let until = t + Duration::from_millis(120);
        while t < until {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 12 + k - 1))));
            let _ = twin.tick(sense(t, Some((4, 12 + k - 1))));
            step_max = step_max.max((f.col - last).abs());
            last = f.col;
        }
    }
    let (t1, f) = idle(&mut pet, t, (4, 18), LOAF_AFTER - 0.3);
    let (_, g) = idle(&mut twin, t, (4, 18), LOAF_AFTER - 0.3);
    let t = t1;
    assert!(
        (f.col - g.col - LEASE_STANDOFF).abs() < 0.6,
        "the leased cat stands {} cells past the twin (wanted {LEASE_STANDOFF})",
        f.col - g.col
    );
    assert_eq!(
        f.pose,
        PetSpecies::Cat.skin(PetGlyphId::PetPerk),
        "…wearing the perk on the caret, not the seat ({:?} / {:?})",
        f.action,
        f.pose
    );
    assert!(
        g.action.settled() && g.pose != PetGlyphId::PetPerk,
        "the twin sits"
    );
    assert!(
        step_max <= 1.0 && pet.flight.is_none(),
        "the far stand is walked to, never jumped: max step {step_max} cells"
    );
    pet.note_room_lease(false);
    let mut t = t;
    let mut walked = false;
    let mut airborne = false;
    let mut step_max = 0.0f32;
    let mut last = pet.col;
    let end = t + Duration::from_secs(3);
    while t < end {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 18))));
        let _ = twin.tick(sense(t, Some((4, 18))));
        walked |= matches!(f.action, PetAction::Walk | PetAction::Run);
        airborne |= f.action.airborne();
        step_max = step_max.max((f.col - last).abs());
        last = f.col;
    }
    assert!(
        walked && !airborne,
        "the walk home is a walk (walked {walked}, airborne {airborne})"
    );
    assert!(
        (pet.col - twin.col).abs() < 0.3,
        "…and it arrives beside the twin: {} vs {}",
        pet.col,
        twin.col
    );
    assert!(
        step_max <= 1.0,
        "no teleport on the way home: max step {step_max}"
    );
}

#[test]
fn a_running_job_keeps_the_cat_awake_with_you() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let (mut t, _) = type_run(&mut pet, start, 10, 20, 6, 0.12);
    let launched = t;
    for _ in 0..60 {
        t += Duration::from_millis(16);
        pet.note_executing(t, true);
        let mut s = sense(t, Some((10, 20)));
        s.output_burst = true;
        let _ = pet.tick(s);
    }
    assert!(
        pet.watch_heat >= WATCH_GATE,
        "fixture: the stream earned the stare"
    );
    pet.note_room_quiet(true);
    let (wakes, slept, crouched, t) = drive_room(&mut pet, t, (10, 20), 40.0, true);
    assert!(!slept, "the cat slept on a running job");
    let at = crouched.expect("the vigil crouch was never entered");
    let since = at.saturating_duration_since(launched).as_secs_f32();
    assert!(
        since <= VIGIL_AFTER + 1.5,
        "the vigil is taken at the half-minute mark, not {since:.1} s"
    );
    assert!(
        pet.vigil && pet.action == PetAction::Crouch,
        "…and held ({:?})",
        pet.action
    );
    assert!(
        !pet.needs_frames() && pet.next_change_deadline(t).is_none(),
        "the held crouch claims no frame and offers nothing"
    );
    assert!(
        wakes <= 120,
        "a quiet job is a handful of wakes and then the crouch, not a train: {wakes}"
    );
    pet.note_command_done(t, false, Some(40_000));
    pet.note_room_quiet(false);
    let mut t = t;
    let mut left_the_ground = false;
    for _ in 0..200 {
        t += Duration::from_millis(16);
        pet.note_executing(t, false);
        left_the_ground |= pet.tick(sense(t, Some((10, 20)))).action.airborne();
    }
    assert!(left_the_ground, "the verdict still buys the strike");
}

#[test]
fn a_sibling_pane_s_burst_earns_one_look() {
    let start = Instant::now();
    let (mut pet, mut twin, mut t) = settled_pair(start, 5.0);
    let mut looks = 0u32;
    let mut first_facing = None;
    let mut was_perk = false;
    let mut f = pet.tick(sense(t, Some((4, 12))));
    for _ in 0..90 {
        t += Duration::from_millis(16);
        pet.note_room_sibling(t, Some((-20.0, 3.0)));
        f = pet.tick(sense(t, Some((4, 12))));
        let g = twin.tick(sense(t, Some((4, 12))));
        if f.action == PetAction::Perk && !was_perk {
            looks += 1;
            first_facing.get_or_insert(f.facing_left);
        }
        was_perk = f.action == PetAction::Perk;
        assert!(
            (f.col - g.col).abs() < 0.05 && f.row.to_bits() == g.row.to_bits(),
            "a look is not a trip"
        );
    }
    assert_eq!(looks, 1, "one rising edge, one look ({looks})");
    assert_eq!(first_facing, Some(true), "…toward the pane that is talking");
    assert!(
        f.action.settled() && f.action != PetAction::Perk,
        "and then back to its seat"
    );
}

#[test]
fn intermittent_sibling_pane_and_escort_bursts_do_not_loop_idle_ears() {
    let start = Instant::now();
    let (mut pet, _, mut t) = settled_pair(start, 2.0);
    let mut perks = 0u32;
    let mut was_perk = false;
    for step in 0..16 {
        let gap = if step < 8 {
            Duration::from_millis(500)
        } else {
            Duration::from_secs(5)
        };
        t += gap;
        pet.note_room_sibling(t, Some((-20.0, 3.0)));
        let f = pet.tick(sense(t, Some((4, 12))));
        if f.action == PetAction::Perk && !was_perk {
            perks += 1;
        }
        was_perk = f.action == PetAction::Perk;
        for _ in 0..35 {
            t += Duration::from_millis(16);
            pet.note_room_sibling(t, None);
            let g = pet.tick(sense(t, Some((4, 12))));
            if g.action == PetAction::Perk && !was_perk {
                perks += 1;
            }
            was_perk = g.action == PetAction::Perk;
        }
    }
    assert_eq!(
        perks, 1,
        "sibling pane status/stream bursts while focused pane idles earn one look, not a loop"
    );

    let (_, mut escort, mut t2) = settled_pair(start, 2.0);
    let mut twitches = 0u32;
    for (idx, gap_ms) in [16_u64, 400, 400, 1_000, 5_000, 5_000]
        .into_iter()
        .enumerate()
    {
        t2 += Duration::from_millis(gap_ms);
        let jitter_col = 12 + (idx as u16 & 1);
        let mut s = sense(t2, Some((4, jitter_col)));
        s.output_burst = true;
        let _ = escort.tick(s);
        if escort.twitch_t > 0.0 {
            twitches += 1;
        }
        for _ in 0..15 {
            t2 += Duration::from_millis(16);
            let _ = escort.tick(sense(t2, Some((4, jitter_col))));
        }
    }
    assert_eq!(
        twitches, 1,
        "periodic escort output bursts and spinner cursor jitter while idling flick an ear once, not in a loop"
    );

    t2 += Duration::from_secs(5);
    for _ in 0..90 {
        t2 += Duration::from_millis(16);
        let _ = escort.tick(sense(t2, Some((4, 14))));
    }
    t2 += Duration::from_millis(16);
    let mut s = sense(t2, Some((4, 14)));
    s.output_burst = true;
    let _ = escort.tick(s);
    assert!(
        escort.twitch_t > 0.0,
        "a quiet user caret move outside execution rearms a single ear flick"
    );
}

#[test]
fn an_unseen_inbox_row_perks_the_cat_and_a_hold_on_sits_it() {
    let start = Instant::now();
    let (mut pet, mut twin, mut t) = settled_pair(start, 2.0);
    pet.note_room_inbox(t, RoomInbox::default());
    let mail = RoomInbox {
        unseen: 7,
        overdue: false,
        hold: false,
    };
    let (mut perks, mut notes, mut twitched, mut was_perk) = (0u32, 0u32, false, false);
    for _ in 0..60 {
        t += Duration::from_millis(16);
        pet.note_room_inbox(t, mail);
        let f = pet.tick(sense(t, Some((4, 12))));
        let g = twin.tick(sense(t, Some((4, 12))));
        if f.action == PetAction::Perk && !was_perk {
            perks += 1;
        }
        was_perk = f.action == PetAction::Perk;
        let note = |f: &PetFrame| {
            f.motes
                .iter()
                .flatten()
                .filter(|m| m.kind == PetMoteKind::Note)
                .count()
        };
        notes = notes.max((note(&f).saturating_sub(note(&g))) as u32);
        twitched |= pet.twitch_t > 0.0;
    }
    assert_eq!(perks, 1, "one letter, one perk ({perks})");
    assert_eq!(notes, 1, "…and one ♪ of its own ({notes})");
    assert!(twitched, "…with one ear");
    let ledger = pet.content;
    let overdue = RoomInbox {
        overdue: true,
        ..mail
    };
    let mut flat = false;
    for _ in 0..40 {
        t += Duration::from_millis(16);
        pet.note_room_inbox(t, overdue);
        let f = pet.tick(sense(t, Some((4, 12))));
        let _ = twin.tick(sense(t, Some((4, 12))));
        flat |= f.action == PetAction::Droop && pet.droop_brief;
    }
    assert!(flat, "a passed deadline flattens the ears");
    assert!(
        (pet.content - ledger).abs() < 1e-6 && !pet.pending_sulk,
        "…with none of the sulk: ledger {ledger} → {}",
        pet.content
    );
    let hold = RoomInbox {
        hold: true,
        ..overdue
    };
    pet.note_room_inbox(t, hold);
    let (t, f) = idle(&mut pet, t, (4, 12), 10.0);
    let (_, g) = idle(&mut twin, t, (4, 12), 10.0);
    assert_eq!(f.action, PetAction::Sit, "sits tight ({:?})", f.action);
    assert_ne!(
        g.action,
        PetAction::Sit,
        "fixture: the twin has melted ({:?})",
        g.action
    );
    let (t, f) = idle(&mut pet, t, (4, 12), SLEEP_AFTER);
    assert_eq!(f.action, PetAction::Sleep, "a halt does not withhold sleep");
    let (t, _) = idle(&mut pet, t, (4, 12), BREATH_WINDOW + 1.0);
    assert!(
        !pet.needs_frames() && pet.next_change_deadline(t).is_none(),
        "idle → zero, hold or no hold"
    );
}

#[test]
fn the_room_s_facts_buy_no_wake() {
    let start = Instant::now();
    let (mut pet, _twin, t) = settled_pair(start, 5.0);
    assert!(!pet.needs_frames(), "fixture: a parked cat");
    let horizon = pet
        .next_change_deadline(t)
        .expect("fixture: a settled cat names its next step");
    pet.note_room_turn(t, 3, true);
    pet.note_room_inbox(t, RoomInbox::default());
    let check = |pet: &PetBrain, what: &str| {
        assert!(!pet.needs_frames(), "{what} pinned the frame train");
        let d = pet.next_change_deadline(t);
        assert!(
            d.is_none_or(|d| d >= horizon),
            "{what} moved the idle horizon sooner: {:?} vs {horizon:?}",
            d
        );
    };
    pet.note_room_lease(true);
    check(&pet, "a lease");
    pet.note_room_quiet(true);
    check(&pet, "a quiet job");
    pet.note_room_turn(t, 4, true);
    check(&pet, "a settled turn");
    pet.note_room_sibling(t, Some((30.0, 2.0)));
    check(&pet, "a talking sibling");
    pet.note_room_inbox(
        t,
        RoomInbox {
            unseen: 9,
            overdue: false,
            hold: true,
        },
    );
    check(&pet, "unread mail under a hold");
    assert!(pet.pending_room.is_some(), "…and the latch is real");
    let mut t = t;
    let mut answered = false;
    for _ in 0..40 {
        t += Duration::from_millis(16);
        answered |= pet.tick(sense(t, Some((4, 12)))).action == PetAction::Perk;
    }
    assert!(answered, "a latch that buys no wake is still a latch");
}

fn one_key(pet: &mut PetBrain, t: Instant, from: (u16, u16)) -> Instant {
    let t = t + Duration::from_millis(16);
    let _ = pet.tick(sense(t, Some((from.0, from.1 + 1))));
    t
}

fn watch(
    pet: &mut PetBrain,
    start: Instant,
    caret: (u16, u16),
    secs: f32,
) -> (Instant, Vec<PetFrame>) {
    let mut t = start;
    let end = start + Duration::from_secs_f32(secs);
    let mut frames = Vec::new();
    while t < end {
        t += Duration::from_millis(16);
        frames.push(pet.tick(sense(t, Some(caret))));
    }
    (t, frames)
}

#[test]
fn a_trick_latch_arms_and_releases_the_lane_and_survives_a_hold_and_a_flight() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let (t, _) = type_run(&mut pet, start, 4, 10, 6, 0.05);
    let (t, _) = idle(&mut pet, t, (4, 16), SLEEP_AFTER + BREATH_WINDOW + 2.0);
    assert!(!pet.needs_frames(), "fixture: deeply asleep, lane released");
    pet.note_trick(t, Trick::Jump, true);
    assert_eq!(pet.pending_trick(), Some(Trick::Jump));
    assert!(pet.needs_frames(), "a latched trick re-arms the lane");
    let (t, frames) = watch(&mut pet, t, (4, 16), 6.0);
    assert_eq!(pet.pending_trick(), None, "performed on the ground");
    assert!(
        frames.iter().any(|f| f.action == PetAction::Waking),
        "a sleeping cat is addressed: it wakes first, and keeps the latch"
    );
    assert!(
        frames.iter().any(|f| f.action == PetAction::Leap),
        "…then it jumps"
    );
    let (_, f) = idle(&mut pet, t, (4, 16), SLEEP_AFTER + BREATH_WINDOW + 2.0);
    assert_eq!(f.action, PetAction::Sleep);
    assert!(
        !pet.needs_frames(),
        "every envelope the trick rode is finite: the lane releases again"
    );

    let mut pet = PetBrain::default();
    let (t, _) = type_run(&mut pet, start, 2, 10, 8, 0.06);
    let (mut t, _) = idle(&mut pet, t, (2, 20), 0.5);
    t += Duration::from_millis(16);
    let f = pet.tick(sense(t, Some((2, 15))));
    assert_eq!(f.action, PetAction::Startle, "fixture: the hold is live");
    pet.note_trick(t, Trick::Roll, true);
    let (_, frames) = watch(&mut pet, t, (2, 15), 3.0);
    assert!(
        frames.iter().any(|f| f.pose == PetGlyphId::PetRoll1),
        "the latch outlives the startle hold and the walk home"
    );

    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 4, 10);
    t += Duration::from_millis(16);
    let _ = pet.tick(sense(t, Some((4, 40))));
    pet.note_trick(t, Trick::Roll, true);
    let (_, frames) = watch(&mut pet, t, (4, 40), 3.5);
    let landed = frames
        .iter()
        .position(|f| f.action == PetAction::Land)
        .expect("fixture: the jump lands inside the window");
    let rolled = frames
        .iter()
        .position(|f| f.pose == PetGlyphId::PetRoll1)
        .expect("the latch outlives the flight");
    assert!(rolled > landed, "and the verb waits for the ground");
}

#[test]
fn a_tentative_trick_settles_at_one_second_and_a_confirmed_one_at_seven_tenths() {
    let heard_after = |confirmed: bool| -> f32 {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let key = one_key(&mut pet, t, (4, 12));
        pet.note_trick(key, Trick::Play, confirmed);
        let mut t = key;
        for _ in 0..120 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 13))));
            if pet.pending_trick().is_none() {
                assert_eq!(
                    f.action,
                    PetAction::Perk,
                    "the latch is consumed INTO the heard-you beat"
                );
                return (t - key).as_secs_f32();
            }
            assert_ne!(
                f.action,
                PetAction::Frolic,
                "nothing plays while the latch still waits"
            );
        }
        panic!("the trick was never heard (confirmed = {confirmed})");
    };
    let confirmed = heard_after(true);
    let tentative = heard_after(false);
    assert!(
        (TRICK_SETTLE..TRICK_SETTLE + 0.04).contains(&confirmed),
        "confirmed: heard {confirmed} s after the last key, want {TRICK_SETTLE}"
    );
    assert!(
        (TRICK_SETTLE_TENTATIVE..TRICK_SETTLE_TENTATIVE + 0.04).contains(&tentative),
        "tentative: heard {tentative} s after the last key, want {TRICK_SETTLE_TENTATIVE}"
    );
}

#[test]
fn a_revoke_drops_only_a_still_pending_tentative_latch() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 10);
    pet.revoke_trick();
    assert_eq!(pet.pending_trick(), None, "a revoke of nothing is a no-op");

    pet.note_trick(t, Trick::Roll, false);
    pet.revoke_trick();
    assert_eq!(pet.pending_trick(), None, "a tentative latch is taken back");
    assert!(!pet.needs_frames(), "…and the lane goes with it");
    let (t, frames) = watch(&mut pet, t, (4, 12), 2.0);
    assert!(
        frames.iter().all(|f| f.pose != PetGlyphId::PetRoll1),
        "a revoked trick is never performed"
    );

    pet.note_trick(t, Trick::Roll, true);
    pet.revoke_trick();
    assert_eq!(
        pet.pending_trick(),
        Some(Trick::Roll),
        "a confirmed request is not the listener's to take back"
    );

    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 10);
    pet.note_trick(t, Trick::Sit, false);
    pet.note_trick(t, Trick::Sit, true);
    pet.note_trick(t, Trick::Sit, false);
    pet.revoke_trick();
    assert_eq!(pet.pending_trick(), Some(Trick::Sit), "upgraded, and kept");
    pet.note_trick(t, Trick::Jump, false);
    assert_eq!(pet.pending_trick(), Some(Trick::Jump));
}

#[test]
fn a_revoked_trick_leaves_the_frames_byte_identical() {
    let run = |tell: bool| -> Vec<u64> {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 10);
        let mut fps = Vec::new();
        for (i, (row, col)) in [(4u16, 13u16), (4, 14), (4, 15), (5, 2), (5, 3)]
            .into_iter()
            .enumerate()
        {
            if tell && i == 1 {
                pet.note_trick(t, Trick::Roll, false);
            }
            if tell && i == 2 {
                pet.revoke_trick();
            }
            for _ in 0..30 {
                t += Duration::from_millis(16);
                fps.push(pet.tick(sense(t, Some((row, col)))).fp());
            }
        }
        for _ in 0..400 {
            t += Duration::from_millis(16);
            fps.push(pet.tick(sense(t, Some((5, 3)))).fp());
        }
        fps
    };
    assert_eq!(run(false), run(true));
}

#[test]
fn a_trick_expires_four_seconds_after_the_last_key_and_never_from_caret_moves() {
    let drive = |typing: bool| -> (f32, bool) {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t0 = awake(&mut pet, start, 4, 10);
        pet.note_trick(t0, Trick::Roll, true);
        let mut t = t0;
        let mut col = 12u16;
        let mut gone = None;
        let mut rolled = false;
        for step in 0..1500u32 {
            t += Duration::from_millis(16);
            if step % 6 == 0 {
                col = if col == 12 { 13 } else { 12 };
                if typing {
                    pet.note_console_input(t, PetInputKind::Text);
                }
            }
            let f = pet.tick(sense(t, Some((4, col))));
            rolled |= f.pose == PetGlyphId::PetRoll1;
            if gone.is_none() && pet.pending_trick().is_none() {
                gone = Some((t - t0).as_secs_f32());
            }
        }
        (gone.expect("the latch is finite by construction"), rolled)
    };
    let (gone, rolled) = drive(false);
    assert!(
        (TRICK_TTL..TRICK_TTL + 0.1).contains(&gone),
        "streamed caret moves re-stamp nothing: gone at +{gone} s, want {TRICK_TTL}"
    );
    assert!(!rolled, "and an expired trick is never performed");
    let (gone, rolled) = drive(true);
    assert!(
        (TRICK_CAP..TRICK_CAP + 0.1).contains(&gone),
        "the user's own keys re-stamp it, up to the cap: gone at +{gone} s, want {TRICK_CAP}"
    );
    assert!(!rolled, "the hand never stopped, so the gate never opened");
}

#[test]
fn a_typed_word_and_enter_plays_at_the_new_prompt() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let (t, _) = type_run(&mut pet, start, 4, 2, 24, 0.07);
    pet.note_trick(t, Trick::Sit, true);
    let mut t = t + Duration::from_millis(16);
    let _ = pet.tick(sense(t, Some((5, 2))));
    assert_eq!(
        pet.pending_trick(),
        Some(Trick::Sit),
        "the snap home clears play intents, never an owed trick"
    );
    let mut flew = false;
    let mut sat_on = None;
    for _ in 0..240 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((5, 2))));
        flew |= f.action.airborne();
        if f.pose == PetGlyphId::PetSitLookup {
            sat_on = Some((f.row, flew, pet.flight.is_none()));
            break;
        }
        if f.action.airborne() {
            assert_eq!(
                pet.pending_trick(),
                Some(Trick::Sit),
                "work outranks a trick: the latch waits out the hop"
            );
        }
    }
    let (row, flew, grounded) = sat_on.expect("the commanded sit plays");
    assert!(flew && grounded, "after the row hop, on the ground");
    assert!((row - 5.0).abs() < 0.01, "at the NEW prompt: row {row}");
}

#[test]
fn a_trick_typed_at_speed_is_performed_where_the_cat_stands() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 10);
    let (t, _) = type_run(&mut pet, t, 4, 12, 18, 0.07);
    assert!(
        pet.lead() > 0.5,
        "fixture: the run opened the lead ({})",
        pet.lead()
    );
    pet.note_trick(t, Trick::Roll, true);
    let (_, frames) = watch(&mut pet, t, (4, 30), 4.0);
    let heard = frames
        .iter()
        .position(|f| f.action == PetAction::Perk)
        .expect("heard");
    assert!(
        frames[heard..]
            .iter()
            .any(|f| f.pose == PetGlyphId::PetRoll1),
        "a request that is heard is answered"
    );
}

#[test]
fn a_hidden_caret_reduced_motion_and_a_hold_on_each_drop_a_trick() {
    let start = Instant::now();

    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 4, 10);
    pet.note_trick(t, Trick::Roll, true);
    t += Duration::from_millis(16);
    let _ = pet.tick(sense(t, None));
    assert_eq!(pet.pending_trick(), None, "no caret: dropped at once");
    let (_, frames) = watch(&mut pet, t, (4, 12), 3.0);
    assert!(
        frames.iter().all(|f| f.pose != PetGlyphId::PetRoll1),
        "and never replayed when the caret returns"
    );
    let mut pet = PetBrain::default();
    pet.note_trick(start, Trick::Jump, true);
    let mut t = start;
    for _ in 0..4 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, None));
        assert_eq!(f.alpha, 0, "a typed word never summons");
    }
    assert_eq!(pet.pending_trick(), None);
    assert!(
        !pet.needs_frames(),
        "and a hidden cat owes no frames for it"
    );

    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 10);
    pet.note_room_inbox(t, RoomInbox::default());
    pet.note_room_inbox(
        t,
        RoomInbox {
            hold: true,
            ..RoomInbox::default()
        },
    );
    pet.note_trick(t, Trick::Roll, true);
    let (_, frames) = watch(&mut pet, t, (4, 12), 3.0);
    assert_eq!(pet.pending_trick(), None);
    assert!(
        frames.iter().all(|f| f.pose != PetGlyphId::PetRoll1),
        "under a hold the cat sits tight: no games, no tricks"
    );

    let reduced = |t: Instant| {
        let mut s = sense(t, Some((3, 10)));
        s.reduced_motion = true;
        s
    };
    let mut pet = PetBrain::default();
    let mut t = start;
    for _ in 0..40 {
        t += Duration::from_millis(16);
        let _ = pet.tick(reduced(t));
    }
    t += Duration::from_millis(16);
    let mut s = reduced(t);
    s.caret = Some((3, 11));
    let _ = pet.tick(s);
    let still = |t: Instant| {
        let mut s = reduced(t);
        s.caret = Some((3, 11));
        s
    };
    pet.note_trick(t, Trick::Jump, true);
    for _ in 0..90 {
        t += Duration::from_millis(16);
        let f = pet.tick(still(t));
        assert_eq!(
            f.action,
            PetAction::Sit,
            "no choreography under reduced motion"
        );
        assert_eq!((f.scale_x, f.scale_y, f.lift), (1.0, 1.0, 0.0));
        assert!(f.motes.iter().all(Option::is_none));
    }
    assert_eq!(pet.pending_trick(), None, "the latch clears as bookkeeping");
    assert!(!pet.needs_frames());
    pet.note_trick(t, Trick::Sleep, true);
    t += Duration::from_millis(16);
    let f = pet.tick(still(t));
    assert_eq!(
        f.action,
        PetAction::Sleep,
        "a commanded sleep is a pose swap"
    );
    assert_eq!(f.pose, PetGlyphId::PetSleep0);
    assert_eq!(pet.pending_trick(), None);
    assert!(pet.trick_settle.is_none() && !pet.needs_frames());
}

#[test]
fn a_commanded_sleep_ends_byte_stable_and_rides_the_offer() {
    let start = Instant::now();
    let (mut pet, mut twin, t) = settled_pair(start, SIT_AFTER + 0.3);
    pet.note_trick(t, Trick::Sleep, true);
    twin.note_trick(t, Trick::Sleep, true);
    let (_, end) = walk_offers(&mut pet, &mut twin, t, (4, 12), 60.0);
    assert_eq!(pet.action, PetAction::Sleep);
    assert!(
        (end - t).as_secs_f32() < BREATH_WINDOW + 3.0,
        "asleep within the beat, deep within the breath window: {:?}",
        end - t
    );
    assert!(
        pet.quiet >= SLEEP_AFTER + BREATH_WINDOW,
        "the walk ends in deep sleep, not on a timeout: quiet {}",
        pet.quiet
    );
    assert!(pet.next_change_deadline(end).is_none() && !pet.needs_frames());
    let a = pet.tick(sense(end + Duration::from_secs(5), Some((4, 12))));
    let b = pet.tick(sense(end + Duration::from_secs(9), Some((4, 12))));
    assert_eq!(a.fp(), b.fp(), "deep sleep is byte-stable");
    assert_eq!(a.pose, PetGlyphId::PetSleep0);
    let t = end + Duration::from_secs(10);
    let f = pet.tick(sense(t, Some((4, 13))));
    assert_eq!(f.action, PetAction::Waking);
    assert!(pet.trick_settle.is_none());
}

#[test]
fn reduced_motion_mid_roll_releases_the_lane_and_replays_nothing() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 10);
    pet.note_trick(t, Trick::Roll, true);
    let (mut t, _) = idle(&mut pet, t, (4, 12), PERK_HOLD + 0.5);
    assert!(pet.wriggle_t > 1.0 && pet.trick_act, "fixture: mid-roll");
    t += Duration::from_millis(16);
    let mut s = sense(t, Some((4, 12)));
    s.reduced_motion = true;
    let f = pet.tick(s);
    assert_eq!(f.action, PetAction::Sit);
    assert_eq!(pet.wriggle_t, 0.0);
    assert!(!pet.trick_act && !pet.trick_engaged());
    assert!(!pet.needs_frames(), "nothing of the roll holds the lane");
    let (_, frames) = watch(&mut pet, t, (4, 12), 6.0);
    assert!(
        frames.iter().all(|f| !matches!(
            f.pose,
            PetGlyphId::PetRoll0 | PetGlyphId::PetRoll1 | PetGlyphId::PetRoll2
        )),
        "and the shed lifting replays nothing"
    );
}

#[test]
fn a_caret_move_interrupts_a_commanded_seat() {
    for trick in [Trick::Sit, Trick::Down] {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        pet.note_trick(t, trick, true);
        let (t, _) = idle(&mut pet, t, (4, 12), PERK_HOLD + 0.5);
        assert_eq!(pet.trick_settle, Some(trick), "fixture: the seat is held");
        let t = one_key(&mut pet, t, (4, 12));
        assert!(pet.trick_settle.is_none() && pet.hold_end_q == 0.0);
        let (_, f) = idle(&mut pet, t, (4, 13), 0.3);
        assert_ne!(f.pose, PetGlyphId::PetSitLookup);
        assert_ne!(
            f.action,
            PetAction::Loaf,
            "the ladder starts over from the key"
        );
    }
}

#[test]
fn a_roll_interrupted_by_an_enter_jump_releases_the_lane_and_never_replays() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 10);
    pet.note_trick(t, Trick::Roll, true);
    let (mut t, frames) = watch(&mut pet, t, (4, 12), PERK_HOLD + 0.6);
    assert!(
        frames.iter().any(|f| f.pose == PetGlyphId::PetRoll0),
        "fixture: the roll is under way"
    );
    assert!(pet.wriggle_t > 1.0, "…with most of it still to play");
    t += Duration::from_millis(16);
    let _ = pet.tick(sense(t, Some((5, 2))));
    let (t, frames) = watch(&mut pet, t, (5, 2), 1.2);
    assert_eq!(
        pet.wriggle_t, 0.0,
        "the roll's pose was taken away: it is over"
    );
    assert!(
        frames.iter().any(|f| f.action.airborne()),
        "fixture: it hopped"
    );
    let (_, frames) = watch(&mut pet, t, (5, 2), 8.0);
    assert!(
        frames
            .iter()
            .any(|f| matches!(f.action, PetAction::Loaf | PetAction::Purr)),
        "fixture: the long dwell is reached"
    );
    assert!(
        frames.iter().all(|f| !matches!(
            f.pose,
            PetGlyphId::PetRoll0 | PetGlyphId::PetRoll1 | PetGlyphId::PetRoll2
        )),
        "a stale roll never replays"
    );
    assert!(!pet.needs_frames(), "and nothing of it pins the lane");
}

#[test]
fn a_pet_only_submit_forgives_the_fast_failure_that_follows() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 3, 2);
    let (t, _) = type_run(&mut pet, t, 3, 4, 30, 0.05);
    let (t, _) = idle(&mut pet, t, (3, 34), 0.3);
    let c0 = pet.content();
    assert!(c0 > 0.3, "fixture: warmth to lose ({c0})");
    pet.note_trick_submit(t);
    let done = t + Duration::from_millis(40);
    pet.note_command_done(done, true, Some(12));
    assert_eq!(pet.content(), c0, "the ledger does not move");
    assert!(
        !pet.pending_sulk && pet.sulk_at.is_none(),
        "no sulk, and none dated"
    );
    assert!(!pet.grieving(), "and no failure hush either");
    let (t, frames) = watch(&mut pet, done, (3, 34), 2.5);
    assert!(
        frames.iter().all(|f| f.action != PetAction::Droop),
        "the cat does not droop at `command not found: sit`"
    );
    pet.note_command_done(t, false, Some(30));
    assert_eq!(
        pet.pending_cheer, None,
        "nothing was forgiven, so nothing is cheered"
    );
    assert!(pet.trick_submit_at.is_none());
}

#[test]
fn an_unrelated_failure_still_sulks() {
    let warm = || {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 3, 2);
        let (t, _) = type_run(&mut pet, t, 3, 4, 30, 0.05);
        let (t, _) = idle(&mut pet, t, (3, 34), 0.3);
        (pet, t)
    };
    let (mut pet, t) = warm();
    let c0 = pet.content();
    pet.note_trick_submit(t);
    pet.note_command_done(t + Duration::from_secs(3), true, Some(12));
    assert!(pet.pending_sulk && pet.sulk_at.is_some());
    assert!(pet.content() < c0);
    let (mut pet, t) = warm();
    pet.note_trick_submit(t);
    pet.note_command_done(t + Duration::from_secs(1), true, Some(CHEER_MIN_MS));
    assert!(
        pet.pending_sulk,
        "a program that really ran and failed is grieved"
    );
    let (mut pet, t) = warm();
    pet.note_trick_submit(t);
    pet.note_command_done(t + Duration::from_millis(100), false, Some(12));
    pet.note_command_done(t + Duration::from_millis(300), true, Some(12));
    assert!(pet.pending_sulk, "one stamp forgives one completion");
    let (mut pet, t) = warm();
    pet.note_trick_submit(t);
    pet.note_command_done(t + Duration::from_millis(100), true, None);
    assert!(!pet.pending_sulk);
}

#[test]
fn a_confirmation_after_the_cat_answered_replays_nothing() {
    let start = Instant::now();
    let rolls = |frames: &[PetFrame]| {
        frames
            .windows(2)
            .filter(|w| {
                w[0].pose != PetGlyphId::PetRoll0
                    && w[1].pose == PetGlyphId::PetRoll0
                    && w[0].action != PetAction::Loaf
            })
            .count()
    };
    let answered = |pet: &mut PetBrain| -> Instant {
        let t = awake(pet, start, 4, 10);
        pet.note_trick(t, Trick::Roll, false);
        let (t, frames) = watch(pet, t, (4, 12), 4.5);
        assert_eq!(rolls(&frames), 1, "fixture: the tentative roll played");
        t
    };
    let mut pet = PetBrain::default();
    let t = answered(&mut pet);
    pet.note_trick(t, Trick::Roll, true);
    assert_eq!(pet.pending_trick(), None, "one phrase, one trick");
    assert!(!pet.needs_frames());
    let (_, frames) = watch(&mut pet, t, (4, 12), 4.0);
    assert_eq!(rolls(&frames), 0, "and nothing replays");
    pet.note_trick(t, Trick::Roll, true);
    assert_eq!(pet.pending_trick(), Some(Trick::Roll));

    let mut pet = PetBrain::default();
    let t = answered(&mut pet);
    pet.revoke_trick();
    pet.note_trick(t, Trick::Roll, true);
    assert_eq!(pet.pending_trick(), Some(Trick::Roll));

    let mut pet = PetBrain::default();
    let t = answered(&mut pet);
    pet.note_trick(t, Trick::Jump, true);
    assert_eq!(pet.pending_trick(), Some(Trick::Jump));

    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 10);
    pet.note_trick(t, Trick::Roll, false);
    let (t, _) = idle(&mut pet, t, (4, 12), 0.1);
    assert!(
        pet.trick_go == Some(Trick::Roll),
        "fixture: heard, verb owed"
    );
    let t = one_key(&mut pet, t, (4, 12));
    let (t, frames) = watch(&mut pet, t, (4, 13), 0.5);
    assert!(
        pet.trick_go.is_none(),
        "the perk was taken away, and the verb with it"
    );
    assert_eq!(rolls(&frames), 0);
    pet.note_trick(t, Trick::Roll, true);
    assert_eq!(pet.pending_trick(), Some(Trick::Roll), "still owed");
    let (_, frames) = watch(&mut pet, t, (4, 13), 3.0);
    assert_eq!(rolls(&frames), 1);
}

#[test]
fn typed_praise_is_rationed_and_a_scold_moves_nothing() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 4, 10);
    let mut peak = 0.0f32;
    for round in 0..6 {
        let before = pet.content();
        pet.note_trick(
            t,
            if round % 2 == 0 {
                Trick::Purr
            } else {
                Trick::Treat
            },
            true,
        );
        let (t2, _) = idle(&mut pet, t, (4, 12), 2.6);
        t = t2;
        peak = peak.max(pet.content() - before);
    }
    assert!(peak <= TRICK_CONTENT + 1e-4, "one nudge at a time: {peak}");
    assert!(
        pet.content() < PURR_GATE,
        "six praises in sixteen seconds buy at most one nudge: {}",
        pet.content()
    );
    let c0 = pet.content();
    pet.note_trick(t, Trick::Scold, true);
    let (_, frames) = watch(&mut pet, t, (4, 12), 2.0);
    assert!(
        frames.iter().any(|f| f.action == PetAction::Droop),
        "fixture: the ear-flat beat played"
    );
    assert!(
        pet.content() <= c0,
        "a scold takes nothing… ({c0} → {})",
        pet.content()
    );
    assert!(
        c0 - pet.content() < CONTENT_DECAY * 2.1,
        "…beyond the two seconds of ordinary spend"
    );
    assert!(!pet.pending_sulk && pet.sulk_at.is_none() && !pet.grieving());
}

#[test]
fn a_sleeper_is_woken_for_a_trick_but_sleep_is_done_and_stretch_is_the_wake() {
    let start = Instant::now();
    let asleep = || {
        let mut pet = PetBrain::default();
        let (t, _) = type_run(&mut pet, start, 4, 10, 6, 0.05);
        let (t, f) = idle(&mut pet, t, (4, 16), SLEEP_AFTER + 1.0);
        assert_eq!(f.action, PetAction::Sleep, "fixture");
        (pet, t)
    };
    let (mut pet, t) = asleep();
    pet.note_trick(t, Trick::Sleep, true);
    let (_, frames) = watch(&mut pet, t, (4, 16), 1.0);
    assert!(
        frames.iter().all(|f| f.action == PetAction::Sleep),
        "`sleep` to a sleeper is a no-op: never woken to be told to sleep"
    );
    assert_eq!(pet.pending_trick(), None);

    let (mut pet, t) = asleep();
    pet.note_trick(t, Trick::Stretch, true);
    let (_, frames) = watch(&mut pet, t, (4, 16), 3.0);
    let mut wakes = 0;
    let mut prev = PetAction::Sleep;
    for f in &frames {
        wakes += u32::from(f.action == PetAction::Waking && prev != PetAction::Waking);
        prev = f.action;
    }
    assert_eq!(wakes, 1, "`stretch` IS the wake: one stretch, not two");

    let (mut pet, t) = asleep();
    pet.note_trick(t, Trick::Paw, true);
    let (_, frames) = watch(&mut pet, t, (4, 16), 3.0);
    let woke = frames.iter().position(|f| f.action == PetAction::Waking);
    let batted = frames.iter().position(|f| f.pose == PetGlyphId::PetBat);
    assert!(
        woke.is_some() && batted.is_some() && woke < batted,
        "woken first, then the verb: {woke:?} / {batted:?}"
    );
}

#[test]
fn every_trick_is_heard_performs_something_observable_and_releases_the_lane() {
    let start = Instant::now();
    for trick in Trick::ALL {
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let mut control = PetBrain::default();
        let tc = awake(&mut control, start, 4, 10);
        assert_eq!(t, tc);
        pet.note_trick(t, trick, true);
        let (end, frames) = watch(&mut pet, t, (4, 12), 5.0);
        let (_, plain) = watch(&mut control, tc, (4, 12), 5.0);
        let heard = frames
            .iter()
            .position(|f| f.action == PetAction::Perk)
            .unwrap_or_else(|| panic!("{trick:?}: never heard"));
        assert!(
            plain.iter().all(|f| f.action != PetAction::Perk),
            "fixture: an untold cat does not perk"
        );
        let after = &frames[heard..];
        let saw = |pose: PetGlyphId| after.iter().any(|f| f.pose == pose);
        let did = |action: PetAction| after.iter().any(|f| f.action == action);
        let mote = |kind: PetMoteKind| {
            after
                .iter()
                .any(|f| f.motes.iter().flatten().any(|m| m.kind == kind))
        };
        let early = |action: PetAction, frames: &[PetFrame]| {
            frames[..120].iter().any(|f| f.action == action)
        };
        let held_for = |pred: &dyn Fn(&PetFrame) -> bool| {
            let (mut best, mut run) = (0usize, 0usize);
            for f in after {
                run = if pred(f) { run + 1 } else { 0 };
                best = best.max(run);
            }
            best as f32 * 0.016
        };
        let seen = match trick {
            Trick::Sit => saw(PetGlyphId::PetSitLookup),
            Trick::Down => early(PetAction::Loaf, &frames) && !early(PetAction::Loaf, &plain),
            Trick::Sleep => early(PetAction::Sleep, &frames) && !early(PetAction::Sleep, &plain),
            Trick::Stretch => saw(PetGlyphId::PetStretch) && saw(PetGlyphId::PetStretchHind),
            Trick::Jump => did(PetAction::Leap) && after.iter().any(|f| f.lift >= ARC_BASE - 0.05),
            Trick::Play => saw(PetGlyphId::PetPlaybow) && saw(PetGlyphId::PetBat),
            Trick::Roll => saw(PetGlyphId::PetRoll0) && saw(PetGlyphId::PetRoll1),
            Trick::Purr => {
                mote(PetMoteKind::Heart)
                    && held_for(&|f| f.action == PetAction::Purr) >= PET_HOLD - 0.05
            }
            Trick::Groom => held_for(&|f| f.pose == PetGlyphId::PetGroom) >= GROOM_HOLD - 0.05,
            Trick::Speak => mote(PetMoteKind::Note),
            Trick::Sing => {
                let notes = |f: &PetFrame| {
                    f.motes
                        .iter()
                        .flatten()
                        .filter(|m| m.kind == PetMoteKind::Note)
                        .count()
                };
                assert!(
                    plain.iter().all(|f| notes(f) == 0),
                    "fixture: an untold cat sings nothing"
                );
                let first = after.iter().position(|f| notes(f) >= 1);
                let all = after
                    .iter()
                    .position(|f| notes(f) >= usize::from(SONG_NOTES));
                match (first, all) {
                    (Some(a), Some(b)) => {
                        (b - a) as f32 * 0.016 >= (f32::from(SONG_NOTES) - 1.0) * SONG_BEAT - 0.05
                    }
                    _ => false,
                }
            }
            Trick::Look => {
                let held = after
                    .iter()
                    .take_while(|f| f.action == PetAction::Perk)
                    .count();
                held as f32 * 0.016 >= 2.0 * PERK_HOLD - 0.05
            }
            Trick::Paw => {
                mote(PetMoteKind::Dust)
                    && held_for(&|f| f.pose == PetGlyphId::PetBat) >= BAT_HOLD - 0.05
            }
            Trick::Hide => after
                .iter()
                .any(|f| f.action == PetAction::Crouch && f.under_ink),
            Trick::Boo => saw(PetGlyphId::PetStartle),
            Trick::Scold => {
                held_for(&|f| matches!(f.pose, PetGlyphId::PetDroop | PetGlyphId::PetDroopSit))
                    >= EAR_FLAT_HOLD - 0.05
            }
            Trick::Treat => did(PetAction::Frolic) && mote(PetMoteKind::Note),
        };
        assert!(seen, "{trick:?} performed nothing observable");
        assert_eq!(pet.pending_trick(), None, "{trick:?}: consumed");
        let (_, _) = idle(&mut pet, end, (4, 12), 3.0);
        assert!(!pet.needs_frames(), "{trick:?} left the lane pinned");
        assert!(
            !pet.trick_act && pet.trick_go.is_none(),
            "{trick:?} left a flag up"
        );
    }
}

#[test]
fn a_commanded_hide_walks_behind_a_word_in_reach() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 4, 40);
    let mut spans = vec![(0u16, 0u16); 30];
    spans[4] = (30, 38);
    pet.note_trick(t, Trick::Hide, true);
    let mut hid_at = None;
    for _ in 0..300 {
        t += Duration::from_millis(16);
        pet.sense_ink(0, &spans, None);
        let f = pet.tick(sense(t, Some((4, 42))));
        if f.under_ink && f.action == PetAction::Crouch {
            hid_at = Some(f.col);
            break;
        }
    }
    let col = hid_at.expect("the cat hides");
    assert!(
        col < 38.0 && col + art_cols(10, 20) > 30.0,
        "behind the word, not where it stood: col {col}"
    );
    t += Duration::from_millis(16);
    pet.sense_ink(0, &spans, None);
    let _ = pet.tick(sense(t, Some((5, 2))));
    t += Duration::from_millis(16);
    pet.sense_ink(0, &spans, None);
    let f = pet.tick(sense(t, Some((5, 2))));
    assert!(!pet.hiding, "the jump home clears the hide");
    let covered = f
        .body_px(10, 20, 100, 30)
        .is_some_and(|rect| pet.letter_cover(rect, 10, 20) > 0.0);
    assert_eq!(
        f.under_ink, covered,
        "the jump home keeps the hide's place under the glyphs"
    );
}

#[test]
fn a_cat_on_vigil_gets_up_to_answer_and_is_not_dragged_back_mid_seat() {
    let (mut pet, mut t) = at_the_door(Instant::now());
    pet.note_trick(t, Trick::Sit, true);
    let mut looked_up = 0u32;
    for _ in 0..200 {
        t += Duration::from_millis(16);
        pet.note_executing(t, true);
        let f = pet.tick(sense(t, Some((10, 20))));
        looked_up += u32::from(f.pose == PetGlyphId::PetSitLookup);
    }
    assert!(
        looked_up > 100,
        "the commanded seat is held, not cut to a frame: {looked_up} frames"
    );
    for _ in 0..120 {
        t += Duration::from_millis(50);
        pet.note_executing(t, true);
        let _ = pet.tick(sense(t, Some((10, 20))));
    }
    assert!(
        pet.vigil && pet.action == PetAction::Crouch,
        "back at the door"
    );
    assert!(!pet.needs_frames());
}

#[test]
fn a_confirmation_inside_the_heard_you_beat_is_still_owed_and_plays_once() {
    let start = Instant::now();
    let rolls = |frames: &[PetFrame]| {
        frames
            .windows(2)
            .filter(|w| {
                w[0].pose != PetGlyphId::PetRoll0
                    && w[1].pose == PetGlyphId::PetRoll0
                    && w[0].action != PetAction::Loaf
            })
            .count()
    };
    let heard = |pet: &mut PetBrain| -> Instant {
        let t = awake(pet, start, 4, 10);
        pet.note_trick(t, Trick::Roll, false);
        let (t, _) = idle(pet, t, (4, 12), 0.1);
        assert_eq!(pet.trick_go, Some(Trick::Roll), "fixture: heard, verb owed");
        t
    };
    let mut pet = PetBrain::default();
    let t = heard(&mut pet);
    pet.note_trick(t, Trick::Roll, true);
    let t = t + Duration::from_millis(16);
    let _ = pet.tick(sense(t, Some((5, 2))));
    assert!(pet.trick_go.is_none(), "the Enter took the beat's verb");
    assert_eq!(
        pet.pending_trick(),
        Some(Trick::Roll),
        "…and the confirmation still owes it"
    );
    let (_, frames) = watch(&mut pet, t, (5, 2), 4.5);
    assert_eq!(rolls(&frames), 1, "played at the new prompt, once");

    let mut pet = PetBrain::default();
    let t = heard(&mut pet);
    pet.note_trick(t, Trick::Roll, true);
    let (_, frames) = watch(&mut pet, t, (4, 12), 7.0);
    assert_eq!(rolls(&frames), 1, "never twice");
    assert_eq!(pet.pending_trick(), None);
    assert!(!pet.trick_engaged());
}

#[test]
fn a_revoke_inside_the_heard_you_beat_takes_the_verb_back() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 10);
    pet.note_trick(t, Trick::Roll, false);
    let (t, _) = idle(&mut pet, t, (4, 12), 0.1);
    assert_eq!(pet.trick_go, Some(Trick::Roll), "fixture: heard, verb owed");
    pet.revoke_trick();
    assert!(!pet.trick_engaged(), "nothing of it is left");
    let (t, frames) = watch(&mut pet, t, (4, 12), 3.0);
    assert!(
        frames.iter().all(|f| !matches!(
            f.pose,
            PetGlyphId::PetRoll0 | PetGlyphId::PetRoll1 | PetGlyphId::PetRoll2
        )),
        "a tentative word taken back mid-look is never performed"
    );
    let (_, _) = idle(&mut pet, t, (4, 12), SLEEP_AFTER + BREATH_WINDOW);
    assert!(!pet.needs_frames(), "and the lane is released");

    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 10);
    pet.note_trick(t, Trick::Roll, true);
    let (t, _) = idle(&mut pet, t, (4, 12), 0.1);
    pet.revoke_trick();
    let (_, frames) = watch(&mut pet, t, (4, 12), 3.0);
    assert!(frames.iter().any(|f| f.pose == PetGlyphId::PetRoll1));

    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 10);
    pet.note_trick(t, Trick::Roll, false);
    let (t, _) = idle(&mut pet, t, (4, 12), PERK_HOLD + 0.4);
    assert!(pet.wriggle_t > 1.0, "fixture: mid-roll");
    pet.revoke_trick();
    let (_, frames) = watch(&mut pet, t, (4, 12), 1.0);
    assert!(
        frames.iter().all(|f| f.action == PetAction::Loaf),
        "a revoke is not a fright"
    );
}

#[test]
fn a_commanded_hide_behind_a_word_at_the_margin_still_ends() {
    let start = Instant::now();
    for (span, caret_col) in [((0u16, 3u16), 4u16), ((88, 100), 97)] {
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, caret_col - 2);
        let mut spans = vec![(0u16, 0u16); 30];
        spans[4] = span;
        pet.note_trick(t, Trick::Hide, true);
        let mut hid = false;
        for _ in 0..900 {
            t += Duration::from_millis(16);
            pet.sense_ink(0, &spans, None);
            let f = pet.tick(sense(t, Some((4, caret_col))));
            hid |= f.under_ink && f.action == PetAction::Crouch;
        }
        assert!(hid, "{span:?}: the cat hides");
        assert!(
            pet.hide_to.is_none() && !pet.hiding,
            "{span:?}: and the trip ends"
        );
        assert!(!pet.needs_frames(), "{span:?}: the lane is released");
    }
}

#[test]
fn a_surface_switch_drops_a_trick_and_carries_the_praise_cooldown() {
    let start = Instant::now();
    let treat_bump = |pet: &mut PetBrain, t: Instant, caret: (u16, u16)| -> f32 {
        pet.note_trick(t, Trick::Treat, true);
        let mut t = t;
        let mut bump = 0.0f32;
        for _ in 0..150 {
            let before = pet.content();
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, Some(caret)));
            bump = bump.max(pet.content() - before);
        }
        bump
    };
    let mut control = PetBrain::default();
    let t = awake(&mut control, start, 8, 20);
    assert!(
        treat_bump(&mut control, t, (8, 22)) > TRICK_CONTENT * 0.9,
        "control: a first treat is seen landing on the ledger"
    );

    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 10);
    pet.note_trick(t, Trick::Purr, true);
    let (t, _) = idle(&mut pet, t, (4, 12), 2.0);
    pet.note_trick(t, Trick::Sit, false);
    let (t, _) = idle(&mut pet, t, (4, 12), PERK_HOLD + 0.2);
    assert!(
        pet.trick_settle == Some(Trick::Sit) && pet.trick_heard.is_some(),
        "fixture: a seat is held, and a tentative word was answered"
    );
    pet.retire_coordinate_space();
    assert!(!pet.trick_engaged() && pet.trick_heard.is_none());
    assert_eq!(pet.hold_end_q, 0.0);
    pet.note_trick(t, Trick::Jump, true);
    pet.retire_coordinate_space();
    assert_eq!(pet.pending_trick(), None, "the latch is the old pane's");
    let t = awake(&mut pet, t, 8, 20);
    let bump = treat_bump(&mut pet, t, (8, 22));
    assert!(
        bump < TRICK_CONTENT * 0.5,
        "the cooldown crossed the switch: a second nudge of {bump}"
    );
}

#[test]
fn a_trick_noted_at_a_retired_cat_meets_the_slow_arm_and_is_dropped() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = start;
    let mut a = pet.tick(sense(t, None));
    while pet.quiet < SLEEP_AFTER + BREATH_WINDOW {
        t += Duration::from_secs(4);
        a = pet.tick(sense(t, None));
    }
    t += Duration::from_millis(16);
    let b = pet.tick(sense(t, None));
    assert_eq!(a.fp(), b.fp(), "fixture: retired, and byte-stable");
    assert!(
        pet.alpha == 0.0 && pet.quiet >= SLEEP_AFTER + BREATH_WINDOW,
        "fixture: the shortcut's own state"
    );
    pet.note_trick(t, Trick::Roll, true);
    t += Duration::from_millis(16);
    let f = pet.tick(sense(t, None));
    assert_eq!(f.alpha, 0, "a typed word never summons");
    assert_eq!(pet.pending_trick(), None, "dropped by the slow arm");
    assert!(!pet.trick_engaged() && !pet.needs_frames());
    let (_, frames) = watch(&mut pet, t, (4, 12), 3.0);
    assert!(
        frames.iter().all(|f| !matches!(
            f.pose,
            PetGlyphId::PetRoll0 | PetGlyphId::PetRoll1 | PetGlyphId::PetRoll2
        )),
        "and nothing is performed when the caret arrives"
    );
}

#[test]
fn a_flinch_under_a_hiding_cat_ends_the_hide() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 10);
    pet.note_trick(t, Trick::Hide, true);
    let (mut t, f) = idle(&mut pet, t, (4, 12), PERK_HOLD + 0.2);
    assert!(
        pet.hiding && f.under_ink && f.action == PetAction::Crouch,
        "fixture: ducked"
    );
    t += Duration::from_millis(16);
    let f = pet.tick(sense(t, Some((4, 11))));
    assert_eq!(
        f.action,
        PetAction::Perk,
        "fixture: the flinch took the crouch"
    );
    t += Duration::from_millis(16);
    let f = pet.tick(sense(t, Some((4, 11))));
    assert!(
        !pet.hiding && !f.under_ink,
        "the hide is over with its pose"
    );
    let (_, _) = idle(&mut pet, t, (4, 11), SLEEP_AFTER + BREATH_WINDOW + 1.0);
    assert!(!pet.needs_frames(), "and nothing of it pins the lane");
}

#[test]
fn a_cat_waiting_up_with_a_job_sleeps_when_told_to() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let (mut t, _) = type_run(&mut pet, start, 10, 20, 6, 0.12);
    for _ in 0..60 {
        t += Duration::from_millis(16);
        pet.note_executing(t, true);
        let mut s = sense(t, Some((10, 20)));
        s.output_burst = true;
        let _ = pet.tick(s);
    }
    pet.note_room_quiet(true);
    let (_, slept, crouched, mut t) = drive_room(&mut pet, t, (10, 20), 40.0, true);
    assert!(
        !slept && crouched.is_some() && pet.waiting_up(),
        "fixture: up with the job, at the door"
    );
    pet.note_trick(t, Trick::Sleep, true);
    let mut asleep = 0u32;
    for _ in 0..240 {
        t += Duration::from_millis(16);
        pet.note_executing(t, true);
        let f = pet.tick(sense(t, Some((10, 20))));
        asleep += u32::from(f.action == PetAction::Sleep);
    }
    assert!(asleep > 150, "told to sleep, it sleeps: {asleep} frames");
    assert!(
        pet.action == PetAction::Sleep && !pet.vigil && pet.waiting_up(),
        "…and the door does not take a sleeping cat back"
    );
}

#[test]
fn a_bell_inside_a_commanded_sit_ends_the_seat() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 10);
    pet.note_trick(t, Trick::Sit, true);
    let (t, f) = idle(&mut pet, t, (4, 12), PERK_HOLD + 0.5);
    assert_eq!(f.pose, PetGlyphId::PetSitLookup, "fixture: seated");
    pet.note_bell(t);
    let (_, frames) = watch(&mut pet, t, (4, 12), 1.2);
    assert!(
        frames.iter().any(|f| f.action == PetAction::Perk),
        "fixture: the bell's flinch played"
    );
    assert!(pet.trick_settle.is_none() && pet.hold_end_q == 0.0);
    let flinched = frames
        .iter()
        .position(|f| f.action == PetAction::Perk)
        .expect("checked above");
    assert!(
        frames[flinched..]
            .iter()
            .all(|f| f.pose != PetGlyphId::PetSitLookup),
        "the ladder has the cat back, from the bell's own zero"
    );
}

#[test]
fn a_purring_cat_s_commanded_seat_rides_the_offer() {
    let start = Instant::now();
    for trick in [Trick::Sit, Trick::Down] {
        let (mut pet, mut twin, t) = contented_pair(start, SIT_AFTER + 0.3);
        assert!(pet.purring(), "fixture: the tell is on");
        pet.note_trick(t, trick, true);
        twin.note_trick(t, trick, true);
        let (walk, _) = walk_offers(&mut pet, &mut twin, t, (4, 72), 5.0);
        assert!(
            pet.trick_settle.is_none(),
            "{trick:?}: the hold ended inside the walk"
        );
        assert!(walk.offers >= 2, "{trick:?}: held on the offer: {walk:?}");

        let (mut pet, mut twin, t) = contented_pair(start, SIT_AFTER + 2.3);
        assert!(
            pet.purring() && pet.mote_mark >= 1,
            "fixture: past the tell's first beat"
        );
        pet.note_trick(t, trick, true);
        twin.note_trick(t, trick, true);
        let (walk, _) = walk_offers(&mut pet, &mut twin, t, (4, 72), 2.5);
        assert_eq!(pet.trick_settle, Some(trick), "{trick:?}: still held");
        assert!(walk.offers >= 2, "{trick:?}: held on the offer: {walk:?}");
    }
}

#[test]
fn a_seat_commanded_off_its_mark_is_taken_after_the_walk_home() {
    for trick in [Trick::Sit, Trick::Down] {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        pet.note_trick(t, trick, true);
        let (t, _) = idle(&mut pet, t, (4, 12), 0.1);
        assert_eq!(pet.trick_go, Some(trick), "fixture: heard, verb owed");
        pet.col += ARRIVED + 0.6;
        let (_, frames) = watch(&mut pet, t, (4, 12), 2.5);
        let walked = frames
            .iter()
            .position(|f| f.action == PetAction::Walk)
            .unwrap_or_else(|| panic!("{trick:?} fixture: the chase walks it home"));
        assert_eq!(
            pet.trick_settle,
            Some(trick),
            "{trick:?}: the level rode the walk"
        );
        let held = frames[walked..]
            .iter()
            .filter(|f| {
                if trick == Trick::Down {
                    f.action == PetAction::Loaf
                } else {
                    f.pose == PetGlyphId::PetSitLookup
                }
            })
            .count();
        assert!(
            held as f32 * 0.016 > 1.0,
            "{trick:?}: and the seat is taken at home, and held: {held} frames"
        );
    }
}

#[test]
fn ink_arriving_under_the_heard_you_beat_drops_the_owed_verb() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 4, 10);
    pet.note_trick(t, Trick::Roll, true);
    t += Duration::from_millis(16);
    let f = pet.tick(sense(t, Some((4, 12))));
    assert!(
        f.action == PetAction::Perk && pet.trick_go == Some(Trick::Roll),
        "fixture: heard, verb owed"
    );
    let mut spans = vec![(0u16, 0u16); 30];
    spans[4] = (0, 40);
    let mut hopped = false;
    let mut rolled = false;
    for _ in 0..400 {
        t += Duration::from_millis(16);
        pet.sense_ink(0, &spans, None);
        let f = pet.tick(sense(t, Some((4, 12))));
        hopped |= f.action.airborne();
        rolled |= matches!(
            f.pose,
            PetGlyphId::PetRoll0 | PetGlyphId::PetRoll1 | PetGlyphId::PetRoll2
        );
    }
    assert!(hopped, "fixture: the ink re-anchored the cat");
    assert!(pet.trick_go.is_none(), "the verb went with the perk");
    assert!(!rolled, "and is not performed on the words it fled");
    assert!(!pet.trick_engaged());
}

#[test]
fn a_cat_in_the_air_keeps_its_facing_when_the_caret_goes_back_behind_it() {
    for back in [5u16, 9, SNAP_COLS as u16 + 1] {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 5, 10);
        let mut caret = 40u16;
        let (mut retreated, mut down_at, mut turned) = (false, None, false);
        for _ in 0..150 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((5, caret))));
            if f.action == PetAction::Leap {
                assert!(
                    down_at.is_none() && !f.facing_left,
                    "back {back}: flew facing back at col {} (caret {caret})",
                    f.col
                );
                if f.col > f32::from(caret - back) + 1.0 && !retreated {
                    caret -= back;
                    retreated = true;
                }
            } else if retreated {
                down_at.get_or_insert(f.col);
                turned |= f.facing_left;
            }
        }
        let down = down_at.expect("fixture: the caret went back in the air");
        if down > f32::from(caret) + STATION_LEAD + ARRIVED {
            assert!(
                turned,
                "back {back}: down at {down}, the cat turns to its stand"
            );
        }
    }
}

#[test]
fn typed_on_through_the_margin_the_wrap_is_one_flight_never_over_the_caret() {
    let width = art_cols(10, 20);
    for frames_per_key in [2u32, 3, 6, 8, 12] {
        for (row, rows) in [(5u16, 30u16), (29, 30)] {
            let start = Instant::now();
            let mut pet = PetBrain::default();
            let mut spans = vec![(0u16, 0u16); usize::from(rows)];
            if row > 0 {
                spans[usize::from(row - 1)] = (0, 100);
            }
            let mut t = awake(&mut pet, start, row, 70);
            let mut flights = 0;
            let mut over = Vec::new();
            let mut last = pet.tick(sense(t, Some((row, 72))));
            let mut zone = false;
            let mut drive = |pet: &mut PetBrain,
                             t: &mut Instant,
                             last: &mut PetFrame,
                             spans: &[(u16, u16)],
                             caret: (u16, u16),
                             wrapped: bool,
                             counting: bool| {
                *t += Duration::from_millis(16);
                pet.sense_ink(0, spans, Some(caret.0));
                let mut s = sense(*t, Some(caret));
                s.rows = rows;
                s.wrapped = wrapped;
                let f = pet.tick(s);
                if counting {
                    if f.action == PetAction::Leap && last.action != PetAction::Leap {
                        flights += 1;
                    }
                    let landed = last.action == PetAction::Leap && f.action != PetAction::Leap;
                    let on_caret = (f.row - f32::from(caret.0)).abs() < 0.5
                        && f.col < f32::from(caret.1) + 1.0 - INK_PAD
                        && f.col + width > f32::from(caret.1) + INK_PAD;
                    if landed && on_caret {
                        over.push((f.col, caret));
                    }
                }
                assert!(
                    !pet.wall_side,
                    "{frames_per_key} frames a key, row {row}: crossed to the wall side at {caret:?}"
                );
                *last = f;
            };
            for c in 73..=99u16 {
                spans[usize::from(row)] = (0, c);
                zone |= f32::from(c) + STATION_LEAD + width > 100.0;
                for _ in 0..frames_per_key {
                    drive(&mut pet, &mut t, &mut last, &spans, (row, c), false, zone);
                }
            }
            let next = if row + 1 < rows {
                row + 1
            } else {
                spans.remove(0);
                spans.push((0, 0));
                row
            };
            for (i, c) in (0..=12u16).enumerate() {
                spans[usize::from(next)] = (0, c);
                for k in 0..frames_per_key {
                    drive(
                        &mut pet,
                        &mut t,
                        &mut last,
                        &spans,
                        (next, c),
                        i == 0 && k == 0 && next == row,
                        true,
                    );
                }
            }
            for _ in 0..90 {
                drive(&mut pet, &mut t, &mut last, &spans, (next, 12), false, true);
            }
            assert_eq!(
                flights, 1,
                "{frames_per_key} frames a key, row {row}: {flights} flights for one wrap"
            );
            assert!(
                over.is_empty(),
                "{frames_per_key} frames a key, row {row}: landed over the caret {over:?}"
            );
            assert!(
                last.action != PetAction::Leap && (last.row - f32::from(next)).abs() < 0.01,
                "{frames_per_key} frames a key, row {row}: down on the caret's row, got {:?} at row {}",
                last.action,
                last.row
            );
        }
    }
}

/// A cat run ahead of the writing to the wall keeps it while the caret dithers short of the
/// wrap for six seconds, never crossing to the caret's left, and leaves for the next line
/// only once the wrap has come: one flight, onto the caret's new row by its start. On the
/// parent commit the cat leapt ahead to the next line's start as the caret dithered at the
/// margin, for a wrap that never came.
#[test]
fn a_cat_running_ahead_keeps_the_wall_and_crosses_once_for_the_wrap() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 5, 78);
    let limit = 100.0 - art_cols(10, 20);
    let mut track = (pet.col, 0.0f32, 0.0f32);
    let tick =
        |pet: &mut PetBrain, t: &mut Instant, track: &mut (f32, f32, f32), caret: (u16, u16)| {
            *t += Duration::from_millis(16);
            let f = pet.tick(sense(*t, Some(caret)));
            let v = (f.col - track.0) / 0.016;
            if f.action != PetAction::Leap && f.lift == 0.0 && f.col > limit - 3.0 {
                track.2 = track.2.max((v - track.1).abs());
            }
            *track = (f.col, v, track.2);
            f
        };
    for c in 80..=91u16 {
        for _ in 0..8 {
            let f = tick(&mut pet, &mut t, &mut track, (5, c));
            assert_ne!(
                f.action,
                PetAction::Leap,
                "no crossing on the run (caret {c})"
            );
        }
    }
    assert!(!pet.wall_side, "a full lead does not cross the cat at 91");
    let mut f = pet.tick(sense(t, Some((5, 91))));
    for _ in 0..60 {
        f = tick(&mut pet, &mut t, &mut track, (5, 91));
        assert_ne!(f.action, PetAction::Leap, "the cat waits at the wall");
    }
    assert!(
        f.action.settled() && (f.col - limit).abs() < ARRIVED,
        "it stands against the wall ({:?} at {}, wall {limit})",
        f.action,
        f.col
    );
    assert!(
        track.2 <= 2.0,
        "it slows into the wall: the speed changed {} cells/s in one frame",
        track.2
    );
    for c in 92..=93u16 {
        for _ in 0..6 {
            let f = tick(&mut pet, &mut t, &mut track, (5, c));
            assert_ne!(
                f.action,
                PetAction::Leap,
                "the caret is not there yet ({c})"
            );
        }
    }
    assert!(!pet.wall_side, "fixture: still on the right at 93");
    let mut launches: Vec<(f32, f32)> = Vec::new();
    let mut prev = f;
    let mut step = |pet: &mut PetBrain,
                    t: &mut Instant,
                    track: &mut (f32, f32, f32),
                    launches: &mut Vec<(f32, f32)>,
                    caret: (u16, u16)| {
        let f = tick(pet, t, track, caret);
        if f.action == PetAction::Leap && prev.action != PetAction::Leap {
            let fl = pet.flight.expect("airborne");
            launches.push((fl.to_col, fl.to_row));
        }
        prev = f;
        f
    };
    let mut dither = vec![94u16; 60];
    for k in 0..6u16 {
        dither.extend([if k % 2 == 0 { 93 } else { 94 }; 60]);
    }
    let mut waited = f;
    for c in dither {
        waited = step(&mut pet, &mut t, &mut track, &mut launches, (5, c));
        assert!(!pet.wall_side, "never over to the caret's left (caret {c})");
    }
    assert!(
        launches.is_empty(),
        "no flight for a wrap that has not come: {launches:?}"
    );
    assert!(
        (waited.row - 5.0).abs() < 0.01 && (waited.col - limit).abs() < ARRIVED,
        "the cat waits by the caret at the wall (col {} row {})",
        waited.col,
        waited.row
    );
    for c in 94..=99u16 {
        for _ in 0..4 {
            let _ = step(&mut pet, &mut t, &mut track, &mut launches, (5, c));
        }
    }
    for c in 0..=12u16 {
        for _ in 0..4 {
            let _ = step(&mut pet, &mut t, &mut track, &mut launches, (6, c));
        }
    }
    let mut last = waited;
    for _ in 0..90 {
        last = step(&mut pet, &mut t, &mut track, &mut launches, (6, 12));
    }
    assert_eq!(launches.len(), 1, "one flight, for the wrap: {launches:?}");
    assert!(
        (launches[0].1 - 6.0).abs() < 0.01 && launches[0].0 < 30.0,
        "the flight goes to the next line's start: {launches:?}"
    );
    assert!(
        (last.row - 6.0).abs() < 0.01 && last.action != PetAction::Leap,
        "and the cat is down on the caret's row, at row {} ({:?})",
        last.row,
        last.action
    );
}

#[test]
fn reaction_hold_and_step_aside_clear_the_cursor_cell_when_typing_forward() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 5, 10);
    let mut spans = vec![(0u16, 0u16); 30];
    for _ in 0..60 {
        t += Duration::from_millis(16);
        spans[5] = (0, 12);
        pet.sense_ink(0, &spans, Some(5));
        let _ = pet.tick(sense(t, Some((5, 12))));
    }
    for c in [11u16, 10, 9] {
        t += Duration::from_millis(32);
        spans[5] = (0, c);
        pet.sense_ink(0, &spans, Some(5));
        let _ = pet.tick(sense(t, Some((5, c))));
    }
    assert_eq!(pet.action, PetAction::Startle);

    for c in 10..=16u16 {
        for _ in 0..3 {
            t += Duration::from_millis(16);
            spans[5] = (0, c);
            pet.sense_ink(0, &spans, Some(5));
            let _ = pet.tick(sense(t, Some((5, c))));
        }
    }
    assert_ne!(
        pet.action,
        PetAction::Startle,
        "sustained forward typing into the cat must yield Startle"
    );
    for _ in 0..30 {
        t += Duration::from_millis(16);
        spans[5] = (0, 16);
        pet.sense_ink(0, &spans, Some(5));
        let _ = pet.tick(sense(t, Some((5, 16))));
    }
    assert!(
        pet.col >= 16.0 + STATION_LEAD - ARRIVED,
        "cat must clear the cursor cell after forward typing, got col={}",
        pet.col
    );
}

#[test]
fn first_sighting_near_right_margin_and_ink_clears_the_caret() {
    use crate::pet_world::{PetPane, PetWorldFacts};
    use aterm_core::terminal::Terminal;
    let start = Instant::now();
    let width = art_cols(10, 20);
    let mut pet = PetBrain::default();
    let mut term = Terminal::new(8, 100);
    term.process(b"\x1b[6;96H");
    let input = term.cell_frame(8, 100);
    let facts = PetWorldFacts::read(&term, 7);
    pet.observe_console(&input, &facts, PetPane::full(&input));
    pet.set_console_presentable(true);
    let frame = pet.tick(sense(start, Some((5, 95))));
    assert!(
        frame.col + width <= 95.0 - STATION_LEAD + 1e-3,
        "fresh sighting near right margin must seat wall-side left of caret, got col={}",
        frame.col
    );

    let mut pet2 = PetBrain::default();
    let mut spans = vec![(0u16, 0u16); 30];
    spans[4] = (0, 100);
    spans[5] = (12, 16);
    spans[6] = (0, 100);
    pet2.sense_ink(0, &spans, Some(5));
    let frame2 = pet2.tick(sense(start, Some((5, 10))));
    let clears_caret =
        frame2.col >= 10.0 + STATION_LEAD || frame2.col + width <= 10.0 - STATION_LEAD;
    assert!(
        clears_caret,
        "fresh sighting must never materialize on top of the caret (col={})",
        frame2.col
    );
}

#[test]
fn two_forward_keystrokes_after_backspace_flinch_yield_perk_to_walk() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 5, 10);
    for _ in 0..60 {
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((5, 12))));
    }
    t += Duration::from_millis(32);
    let _ = pet.tick(sense(t, Some((5, 11))));
    assert_eq!(pet.action, PetAction::Perk);

    for c in [12u16, 13] {
        t += Duration::from_millis(32);
        let _ = pet.tick(sense(t, Some((5, c))));
    }
    assert!(
        matches!(pet.action, PetAction::Walk | PetAction::Run),
        "second forward keystroke encroaching on the cat must yield Perk to chase (got {:?})",
        pet.action
    );
}

#[test]
fn first_key_after_face_on_sit_and_16ms_ink_push_walk_on_paws_not_slide_in_settled_pose() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 15);
    let (mut t, settled) = idle(&mut pet, t, (4, 15), SIT_AFTER + 0.5);
    assert_eq!(settled.pose, PetGlyphId::PetSitFront);
    assert!(pet.facing_left);

    t += Duration::from_millis(16);
    let f1 = pet.tick(sense(t, Some((4, 16))));
    assert_eq!(
        f1.action,
        PetAction::Walk,
        "first key after PetSitFront must wake into Walk, got {:?}",
        f1.action
    );
    assert!(
        !f1.facing_left,
        "first key after PetSitFront must face the forward writing"
    );
    for _ in 0..8 {
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 16))));
        assert!(
            !(f.action == PetAction::Stand && f.facing_left),
            "must never slide backwards in Stand facing left"
        );
    }

    let mut pet2 = PetBrain::default();
    let mut t2 = awake(&mut pet2, start, 3, 5);
    let mut spans = vec![(0u16, 0u16); 5];
    spans[2] = (0, 100);
    spans[3] = (0, 27);
    spans[4] = (0, 100);
    for _ in 0..180 {
        t2 += Duration::from_millis(16);
        pet2.sense_ink(0, &spans, Some(3));
        let _ = pet2.tick(sense(t2, Some((3, 7))));
    }
    assert!(pet2.action.settled());
    t2 += Duration::from_millis(16);
    spans[3] = (0, 28);
    pet2.sense_ink(0, &spans, Some(3));
    let pushed = pet2.tick(sense(t2, Some((3, 8))));
    assert_eq!(
        pushed.action,
        PetAction::Walk,
        "1-cell ink push on a 16 ms frame must walk in the chase, not slide in {:?}",
        pushed.action
    );
}

#[test]
fn every_stop_brakes_at_any_pace() {
    let limit = 100.0 - art_cols(10, 20);
    for frames_per_key in [8u32, 7, 6, 5, 4] {
        for stop in [84u16, 86, 88, 89, 90, 91, 92, 93] {
            let label = format!(
                "{:.3} s a key, typing stops at {stop}",
                frames_per_key as f32 * 0.016
            );
            let start = Instant::now();
            let mut pet = PetBrain::default();
            let mut t = awake(&mut pet, start, 5, 78);
            let mut prev = (pet.col, 0.0f32, false);
            let mut worst = (0.0f32, 0.0f32, 0.0f32);
            let mut back = None;
            let mut moved_at = 0usize;
            let keys = usize::from(stop - 79);
            let rest_frames = 90usize;
            let carets = (80..=stop).flat_map(|c| std::iter::repeat_n(c, frames_per_key as usize));
            for (k, c) in carets
                .chain(std::iter::repeat_n(stop, rest_frames))
                .enumerate()
            {
                t += Duration::from_millis(16);
                let f = pet.tick(sense(t, Some((5, c))));
                let v = (f.col - prev.0) / 0.016;
                let foot = f.action != PetAction::Leap && f.lift == 0.0;
                if foot && prev.2 && k >= 3 * frames_per_key as usize && v * prev.1 >= 0.0 {
                    let drop = prev.1.abs() - v.abs();
                    if drop > worst.0 {
                        worst = (drop, prev.1, f.col);
                    }
                }
                if k >= 3 * frames_per_key as usize && f.col < prev.0 - 1e-3 {
                    back.get_or_insert((k, prev.0, f.col, f.action));
                }
                if (f.col - prev.0).abs() > 1e-3 {
                    moved_at = k;
                }
                prev = (f.col, v, foot);
            }
            assert!(
                worst.0 <= 2.0,
                "{label}: the speed fell {:.2} cells/s in one frame (from {:.2} at col {:.2})",
                worst.0,
                worst.1,
                worst.2
            );
            assert!(back.is_none(), "{label}: the cat stepped back: {back:?}");
            assert!(
                pet.action.settled() && prev.0 <= limit + 1e-3,
                "{label}: at rest by the margin ({:?} at {:.2}, wall {limit:.2})",
                pet.action,
                prev.0
            );
            let typed = keys * frames_per_key as usize;
            assert!(
                moved_at < typed + 30,
                "{label}: still moving {} frames after the last key",
                moved_at.saturating_sub(typed)
            );
        }
    }
}

/// A skid on glass runs a still pose's floor ([`STILL_SECS`]): a run that crosses its seat
/// at 7 to 13 cells a second brakes, and the skid it draws stays until the floor is out,
/// however soon its crawl arrives. NEGATIVE CONTROL (as run before the floor): crossing at
/// 9.3 cells a second, the skid was on glass a single 16 ms frame.
#[test]
fn a_skid_once_drawn_stays_a_still_poses_floor() {
    let mut drawn = 0;
    for tenth in 70..=130u16 {
        let v = f32::from(tenth) * 0.1;
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 5, 40);
        let seat = pet.col;
        // Mid-run, a long leg's last stride a hair short of the seat.
        pet.col = seat - 0.05;
        pet.set_action(PetAction::Run);
        pet.running = true;
        pet.speed = v;
        pet.leg_dist = BRAKE_DIST;
        pet.content = 1.0;
        pet.facing_left = false;
        pet.stand_from = None;
        pet.still_since = None;
        let mut skid = Vec::new();
        for _ in 0..12 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((5, 42))));
            skid.push(f.alpha > 0 && f.pose == PetGlyphId::PetSkid);
        }
        let frames = skid.iter().filter(|&&on| on).count();
        if frames == 0 {
            continue;
        }
        drawn += 1;
        let first = skid.iter().position(|&on| on).expect("counted");
        assert!(
            skid[first..first + frames].iter().all(|&on| on),
            "crossing at {v:.1} cells/s: the skid broke off and came back: {skid:?}"
        );
        assert!(
            frames as f32 * 0.016 >= STILL_SECS,
            "crossing at {v:.1} cells/s: the skid was on glass {} ms, under the floor \
             ({:.0} ms): {skid:?}",
            frames * 16,
            STILL_SECS * 1000.0
        );
    }
    assert!(drawn > 0, "positive control: no crossing speed drew a skid");
}

/// The stand a stride ends in is held a stride ([`STAND_MIN`]) against a leap as against
/// every other pose: a flight asked for from it waits ([`PetBrain::deferred_arc`]) and
/// leaves once the stride is out; a still pose that ends no stride waits only
/// [`STILL_SECS`]. NEGATIVE CONTROL (as run before the hold): the leap left 50 ms into the
/// stand.
#[test]
fn a_stand_that_ends_a_stride_holds_a_stride_before_a_leap() {
    for ends_leg in [true, false] {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let _ = awake(&mut pet, start, 5, 40);
        let on = pet.clock;
        pet.set_action(PetAction::Stand);
        pet.speed = 0.0;
        pet.still_since = Some((on, PetGlyphId::PetStand));
        pet.still_console = false;
        pet.console_posed = false;
        pet.stand_from = Some(on);
        pet.stand_ends_leg = ends_leg;
        pet.flight = None;
        pet.deferred_arc = None;
        pet.clock = on + 0.05;
        pet.begin_arc(pet.col + 10.0, 8.0, 0.4, 0.8, FlightKind::PLAIN);
        if !ends_leg {
            assert!(
                pet.flight.is_some(),
                "a stand that ends no stride is left once its still floor is out"
            );
            continue;
        }
        assert!(
            pet.flight.is_none() && pet.deferred_arc.is_some(),
            "50 ms into the stand a stride ended in, the leap waits"
        );
        pet.clock = on + f64::from(STAND_MIN) + 1e-3;
        pet.begin_arc(pet.col + 10.0, 8.0, 0.4, 0.8, FlightKind::PLAIN);
        assert!(pet.flight.is_some(), "the stride out, the leap leaves");
    }
}

#[test]
fn a_cat_at_the_wall_stands_while_the_writing_comes_on() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 5, 78);
    let limit = 100.0 - art_cols(10, 20);
    let mut prev = pet.col;
    let (mut at_wall, mut in_place) = (0u32, 0u32);
    for c in 80..=93u16 {
        for _ in 0..8 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((5, c))));
            let still = (f.col - prev).abs() < 1e-3;
            if (f.col - limit).abs() < 0.05 && still {
                at_wall += 1;
                in_place += u32::from(matches!(f.action, PetAction::Walk | PetAction::Run));
            }
            prev = f.col;
        }
    }
    assert!(
        at_wall >= 16,
        "fixture: the cat waits at the wall ({at_wall} frames)"
    );
    assert!(
        in_place <= 2,
        "{in_place} of {at_wall} frames at the wall walk on the spot"
    );
}

#[test]
fn the_walk_into_a_hide_slows_into_the_crouch() {
    for off in [3u16, 6] {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = warm_settled(&mut pet, start);
        let mut spans = [(0u16, 0u16); 12];
        let seat = pet.col.round() as u16;
        spans[pet.row as usize] = (seat - 8 - off, seat - off);
        let mut prev = (pet.col, 0.0f32);
        let mut worst = 0.0f32;
        let mut dest = None;
        for _ in 0..900 {
            t += Duration::from_millis(16);
            pet.sense_ink(0, &spans, None);
            if !pet.hiding && pet.hide_to.is_none() {
                pet.mote_serial = 3;
                pet.content = pet.content.max(0.9);
            }
            let f = pet.tick(sense(t, Some((4, 44))));
            dest = pet.hide_to.or(dest);
            let v = (f.col - prev.0) / 0.016;
            if dest.is_some() && f.action != PetAction::Leap && f.lift == 0.0 && v * prev.1 >= 0.0 {
                worst = worst.max(prev.1.abs() - v.abs());
            }
            prev = (f.col, v);
            if pet.hiding {
                break;
            }
        }
        let dest = dest.expect("fixture: the deal sends the cat behind the word");
        assert!(pet.hiding, "off {off}: the cat hides");
        assert!(
            worst <= 2.0,
            "off {off}: the walk in stopped {worst:.2} cells/s in one frame"
        );
        assert!(
            (pet.col - dest).abs() < 0.1,
            "off {off}: crouched on the spot ({:.2}, the spot {dest:.2})",
            pet.col
        );
    }
}

#[test]
fn a_scrabble_hunts_braced_never_in_the_gallop_s_art() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 5, 20);
    let width = art_cols(10, 20);
    pet.set_action(PetAction::Land);
    pet.scrabble = true;
    let mut wobbled = false;
    let mut k = 0.0f32;
    while k * 0.016 < SCRABBLE_DUR {
        pet.land_t = SCRABBLE_DUR - k * 0.016;
        let f = pet.emit(sense(t, Some((5, 22))), width);
        let id = crate::pet_glyphs_gen::PET_GLYPHS[f.pose as usize].id;
        assert!(
            !id.starts_with("pet_run_"),
            "{:.3} s into the scrabble the still body wears {id}",
            k * 0.016
        );
        wobbled |= (f.scale_x - 1.0).abs() > 1e-3 && k * 0.016 > SCRABBLE_PLANT;
        k += 1.0;
    }
    assert!(wobbled, "negative control: the hunt still wobbles");
}

#[test]
fn face_on_peck_scoot_stops_short_of_adjacent_ink_without_oscillating() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let (cell_w, cell_h) = (9, 20);
    let width = art_cols(cell_w, cell_h);
    let sense_at = |now: Instant, caret: (u16, u16)| PetSense {
        cell_w,
        cell_h,
        ..sense(now, Some(caret))
    };
    let mut t = start;
    let _ = pet.tick(sense_at(t, (4, 14)));
    t += Duration::from_millis(16);
    let _ = pet.tick(sense_at(t, (4, 15)));
    let ink_start = 22u16;
    let mut spans = [(0u16, 0u16); 12];
    spans[4] = (ink_start, ink_start + 4);
    let end = t + Duration::from_secs_f32(SIT_AFTER + 0.6);
    let mut last = None;
    while t < end {
        t += Duration::from_millis(16);
        pet.sense_ink(0, &spans, None);
        last = Some(pet.tick(sense_at(t, (4, 15))));
    }
    let f = last.expect("frames ticked");
    assert_eq!(f.pose, PetGlyphId::PetSitFront);
    assert!(
        !pet.ink_overlaps(f.col, f.row, width),
        "face-on sit must not scoot into adjacent ink: col={}, width={}, ink_start={}",
        f.col,
        width,
        ink_start
    );
    assert!(
        !pet.evicting && !pet.needs_frames(),
        "settled face-on sit beside ink must not get stuck in an eviction/scoot loop"
    );
}

#[test]
fn reduced_motion_and_fade_out_clear_letter_veil_without_pinning_the_frame_train() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 29, 18);
    let faintest = veiled_over(&mut pet, &mut t, PetAction::Run, 60, 0.3);
    assert!(
        faintest < 255 && pet.letter_veil.0 < 1.0,
        "fixture: moving over ink dims the body under the letters"
    );
    let spans = {
        let mut spans = [(0u16, 0u16); 30];
        spans[29] = (0, 60);
        spans
    };

    t += Duration::from_millis(16);
    pet.sense_ink(0, &spans, None);
    let rm = pet.tick(PetSense {
        reduced_motion: true,
        ..sense(t, Some((29, 20)))
    });
    assert_eq!(rm.alpha, 255);
    assert!(!rm.under_ink);
    assert_eq!(pet.letter_veil, (1.0, None, 1.0));
    assert!(!pet.needs_frames());

    let mut pet2 = PetBrain::default();
    let mut t2 = awake(&mut pet2, start, 29, 18);
    let _ = veiled_over(&mut pet2, &mut t2, PetAction::Run, 60, 0.3);
    assert!(pet2.letter_veil.0 < 1.0);
    let mut hidden = None;
    for _ in 0..36 {
        t2 += Duration::from_millis(16);
        let f = pet2.tick(sense(t2, None));
        if f.alpha == 0 {
            hidden = Some(f);
            break;
        }
    }
    assert_eq!(hidden.map(|f| f.alpha), Some(0));
    assert_eq!(pet2.letter_veil, (1.0, None, 1.0));
}

#[test]
fn pet_frame_fp_folds_mote_rgb_and_departure_glass_routing_but_not_departure_id() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 20);
    let mut base = pet.tick(sense(t, Some((4, 20))));
    base.motes[0] = Some(PetMoteSprite {
        kind: PetMoteKind::Note,
        col: 21.0,
        row: 3.5,
        scale: 1.0,
        rot: 0.25,
        alpha: 200,
        rgb: None,
    });
    base.departures[0] = Some(PetDepartureSprite {
        pose: PetGlyphId::PetRun0,
        col: 18.0,
        row: 4.0,
        facing_left: true,
        scale_x: 1.0,
        scale_y: 1.0,
        lift: 0.0,
        stretch_from: StretchFrom::Centre,
        alpha: 180,
        coat: 0,
        iris: 0,
        id: 7,
        under_ink: false,
        own_glass: false,
        glass: 0,
        colors: None,
    });
    let fp0 = base.fp();

    let mut with_rgb = base;
    with_rgb.motes[0].as_mut().unwrap().rgb = Some(0xFF00AA);
    assert_ne!(with_rgb.fp(), fp0, "mote rgb tint must move PetFrame::fp");

    let mut with_under_ink = base;
    with_under_ink.departures[0].as_mut().unwrap().under_ink = true;
    assert_ne!(
        with_under_ink.fp(),
        fp0,
        "departure under_ink must move PetFrame::fp"
    );

    let mut with_own_glass = base;
    with_own_glass.departures[0].as_mut().unwrap().own_glass = true;
    assert_ne!(
        with_own_glass.fp(),
        fp0,
        "departure own_glass must move PetFrame::fp"
    );

    let mut with_glass = base;
    with_glass.departures[0].as_mut().unwrap().glass = 42;
    assert_ne!(
        with_glass.fp(),
        fp0,
        "departure glass key must move PetFrame::fp"
    );

    let mut different_id = base;
    different_id.departures[0].as_mut().unwrap().id = 999;
    assert_eq!(
        different_id.fp(),
        fp0,
        "departure id is identity-only and must not move PetFrame::fp"
    );
}

#[test]
fn scrabble_brake_and_stumble_dust_kick_back_along_motion() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 4, 10);

    pet.skid_dir = -1.0;
    pet.mote_serial = 0;
    pet.stride = 0.0;
    pet.motes = [None; PET_MOTES_MAX];
    pet.begin_arc(30.0, 4.0, 0.45, 0.8, FlightKind::PLAIN);
    if let Some(fl) = pet.flight.as_mut() {
        fl.t = fl.dur;
        fl.reaimed = true;
    }
    t += Duration::from_millis(16);
    let _ = pet.tick(sense(t, Some((4, 10))));
    assert!(pet.scrabble, "scrabble landing armed");
    assert_eq!(pet.skid_dir, 1.0, "rightward scrabble sets +1.0 skid_dir");
    for m in pet
        .motes
        .iter()
        .flatten()
        .filter(|m| m.kind == PetMoteKind::Dust)
    {
        assert_eq!(m.dir, -1.0, "rightward scrabble dust kicks left (-1.0)");
    }

    pet.drop_theater();
    assert_eq!(pet.skid_dir, 0.0, "drop_theater clears skid_dir");
}

#[test]
fn reduced_motion_drops_tennis_stumble_pounce_and_retired_flight_lift() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 4, 10);

    pet.tennis = true;
    pet.stumble_t = STUMBLE_DUR;
    pet.pending_pounce = true;
    pet.retired_flight_lift = Some((0.8, 3.0));
    pet.deferred_hidden_landing = Some((20.0, 4.0));
    pet.droop_brief = true;
    pet.sit_front = true;
    pet.peek = true;
    pet.gaze_dwell = 0.5;
    pet.leg_dist = 8.0;

    let mut s = sense(t + Duration::from_millis(16), Some((4, 10)));
    s.reduced_motion = true;
    let frame = pet.tick(s);
    assert_eq!(frame.lift, 0.0, "reduced motion clears retired_flight_lift");
    assert!(
        (frame.scale_x - 1.0).abs() < 1e-6 && (frame.scale_y - 1.0).abs() < 1e-6,
        "reduced motion clears stumble squash"
    );
    assert!(
        !pet.tennis
            && pet.stumble_t == 0.0
            && !pet.pending_pounce
            && pet.retired_flight_lift.is_none()
            && !pet.droop_brief
            && !pet.sit_front
            && !pet.peek
            && pet.gaze_dwell == 0.0
            && pet.leg_dist == 0.0
    );
    assert!(
        !pet.needs_frames(),
        "reduced motion must not leave tennis, stumble_t or retired_flight_lift pinning needs_frames"
    );
}

#[test]
fn carry_with_regrid_shifts_bound2_adopted_look_and_motes_and_zero_alpha_clears_adopted_look() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 6, 12);

    pet.bound2 = Some((28.0, 6.0, false));
    pet.adopted_look = Some(AdoptedLook {
        pose: PetGlyphId::PetSit,
        facing_left: false,
        stance: Stance::NEUTRAL,
        eased: 0.0,
        seat: (pet.col, 6.0),
        beneath: None,
    });
    pet.motes[0] = Some(Mote {
        kind: PetMoteKind::Zee,
        born: pet.clock,
        life: 1.5,
        col: pet.col,
        row: 6.0,
        dir: 1.0,
        seed: 0,
        rgb: None,
    });

    pet.carry_with_regrid(3.0, 24);
    assert_eq!(pet.row, 9.0, "body row shifted by +3");
    assert_eq!(
        pet.bound2.map(|(_, r2, _)| r2),
        Some(9.0),
        "bound2 target row shifted by +3 with regrid"
    );
    assert_eq!(
        pet.adopted_look.as_ref().map(|l| l.seat.1),
        Some(9.0),
        "adopted_look seat row shifted by +3 so crossfade hold survives regrid"
    );
    assert_eq!(
        pet.motes[0].as_ref().map(|m| m.row),
        Some(9.0),
        "live mote row shifted by +3 with regrid"
    );

    pet.bound2 = Some((28.0, 22.0, false));
    pet.carry_with_regrid(3.0, 24);
    assert_eq!(
        pet.row, 9.0,
        "out-of-bounds bound2 refuses carry_with_regrid"
    );

    // Mid-fade, as a fading pet asks for its frames ([`PetBrain::needs_frames`]).
    pet.alpha = 0.01;
    pet.asked_frames = true;
    let _ = pet.tick(sense(t + Duration::from_secs(1), None));
    assert_eq!(pet.alpha, 0.0, "faded to zero alpha");
    assert!(
        pet.adopted_look.is_none(),
        "zero-alpha fade-out clears adopted_look"
    );
}

#[test]
fn split_crossing_mid_air_typing_moves_bound2_not_touch_and_console_carry_shifts_skid_and_adopted_look()
 {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 6, 4);
    let w = art_cols(10, 20);

    pet.launch_big(82.0, 6.0, 100, w, true);
    let touch0 = pet.flight.expect("first bound launched").to_col;
    let bound2_0 = pet.bound2.expect("split into two bounds").0;
    assert!(
        touch0 < bound2_0 - 15.0,
        "touch ({touch0}) is midfield before bound2 ({bound2_0})"
    );
    pet.last_caret = Some((6, 80));

    for c in [81u16, 82, 83] {
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((6, c))));
    }
    let touch1 = pet.flight.expect("still in first bound").to_col;
    let bound2_1 = pet.bound2.expect("bound2 still pending").0;
    assert!(
        (touch1 - touch0).abs() < 1e-4,
        "first bound keeps its midfield touch point ({touch0} -> {touch1})"
    );
    assert!(
        bound2_1 > bound2_0 && bound2_1 < bound2_0 + 15.0,
        "bound2 advances with the typing rhythm without overshooting ({bound2_0} -> {bound2_1})"
    );

    let mut frame = pet.emit(sense(t, Some((6, 83))), w);
    pet.home = pet.col;
    let home0 = pet.home;
    pet.skid_from = 20.0;
    pet.brake_over = Some(22.0);
    pet.adopted_look = Some(AdoptedLook {
        pose: PetGlyphId::PetSit,
        facing_left: false,
        stance: Stance::NEUTRAL,
        eased: 0.25,
        seat: (20.0, 6.0),
        beneath: None,
    });
    let to = PetFrame {
        col: frame.col + 3.0,
        row: frame.row + 2.0,
        ..frame
    };
    pet.carry_console_body(&mut frame, to, 2.0);
    assert!(
        (pet.home - (home0 + 3.0)).abs() < 1e-4,
        "home shifted by dx so carry_console_body does not open a false home-col gap"
    );
    assert!(
        (pet.skid_from - 23.0).abs() < 1e-4,
        "skid_from shifted by dx"
    );
    assert_eq!(pet.brake_over, Some(25.0), "brake_over shifted by dx");
    assert_eq!(
        pet.adopted_look.map(|l| l.seat),
        Some((23.0, 8.0)),
        "adopted_look seat shifted by (dx, ground_dy)"
    );

    t += Duration::from_millis(16);
    let _ = pet.tick(sense(t, Some((8, 83))));
    assert!(
        pet.bound2.is_none(),
        "mid-flight row change cancels split crossing's second bound"
    );

    pet.flight = None;
    pet.set_action(PetAction::Land);
    pet.land_t = TOUCH_LAND_DUR;
    pet.bound2 = Some((75.0, 8.0, true));
    t += Duration::from_millis(16);
    let _ = pet.tick(sense(t, Some((10, 83))));
    assert!(
        pet.bound2.is_none(),
        "row change during touch-land cancels bound2"
    );

    pet.row = 10.0;
    pet.flight = None;
    pet.bound2 = None;
    pet.play_to = Some((12.0, 10.0));
    pet.play_land = Some(PlayLand::BatSwipe {
        col: 13.0,
        row: 10.0,
    });
    pet.pending_bat = Some((15.0, 10.0));
    pet.room_cheer_stage = Some((14.0, 10.0));
    pet.pending_pointer_pounce = Some((20.0, 10.0));
    pet.rebase_geometry();
    assert!(pet.pending_pointer_pounce.is_none());
    pet.carry_with_regrid(2.0, 24);
    assert_eq!(pet.play_to, Some((12.0, 12.0)));
    assert_eq!(
        pet.play_land,
        Some(PlayLand::BatSwipe {
            col: 13.0,
            row: 12.0,
        })
    );
    assert_eq!(pet.pending_bat, Some((15.0, 12.0)));
    assert_eq!(pet.room_cheer_stage, Some((14.0, 12.0)));
}

#[test]
fn visible_resighting_preserves_wall_side_hysteresis_and_latches_wall_transit() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 5, 96);
    let (_, _) = idle(&mut pet, t, (5, 96), 0.6);
    t += Duration::from_millis(600);
    assert!(pet.wall_side, "fixture: parked wall-side at col 96");

    t += Duration::from_millis(16);
    let _ = pet.tick(sense(t, Some((5, 93))));
    assert!(pet.wall_side, "still wall-side inside hysteresis band");
    t += Duration::from_millis(16);
    let _ = pet.tick(sense(t, None));
    t += Duration::from_millis(16);
    let _ = pet.tick(sense(t, Some((5, 93))));
    assert!(
        pet.wall_side && !pet.pending_wall_transit,
        "1-frame caret hide must not clobber wall_side hysteresis on a visible cat"
    );
    pet.rebase_geometry();
    t += Duration::from_millis(16);
    let _ = pet.tick(sense(t, Some((5, 93))));
    assert!(
        pet.wall_side && !pet.pending_wall_transit,
        "rebase_geometry must not clobber wall_side hysteresis on a visible cat"
    );

    pet.rebase_geometry();
    t += Duration::from_millis(16);
    let mut wide = sense(t, Some((5, 93)));
    wide.cols = 120;
    let f = pet.tick(wide);
    assert!(
        !pet.wall_side && (pet.pending_wall_transit || f.action == PetAction::Leap),
        "widening past WALL_RETURN_MARGIN must hop over the caret"
    );
}

#[test]
fn big_flight_midair_reaim_shortens_for_skid_and_carry_console_body_shifts_in_place_play_to() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 5, 10);
    let (_, _) = idle(&mut pet, t, (5, 10), 0.6);
    t += Duration::from_millis(600);
    let s0 = sense(t, Some((5, 10)));
    let width = art_cols(s0.cell_w, s0.cell_h);

    pet.launch_big(24.0 + STATION_LEAD, 5.0, 100, width, true);
    assert!(pet.flight.is_some_and(|f| f.big) && pet.bound2.is_none());

    pet.last_caret = Some((5, 10));
    pet.vhat = 0.0;
    t += Duration::from_millis(16);
    let mut frame = pet.tick(sense(t, Some((5, 20))));
    let target_station = pet.station_now(20, 100, width);
    let to_col = pet.flight.expect("still airborne after re-aim").to_col;
    assert!(
        (to_col - (target_station - SKID_CELLS)).abs() <= 1e-3,
        "big flight mid-air jump re-aim must shorten f.to_col by SKID_CELLS: got {to_col}, expected {}",
        target_station - SKID_CELLS
    );

    pet.col = 14.0;
    pet.row = 5.0;
    pet.play_to = Some((14.0, 5.0));
    frame.col = 14.0;
    frame.row = 5.0;
    let mut moved = frame;
    moved.col += 3.0;
    moved.row += 2.0;
    pet.carry_console_body(&mut frame, moved, 2.0);
    assert_eq!(
        pet.play_to,
        Some((17.0, 7.0)),
        "in-place play_to must ride along with carry_console_body"
    );
}

#[test]
fn vigil_verdict_with_prompt_jump_or_sleeping_cat_never_strands_frame_lane() {
    let t0 = Instant::now();

    let (mut pet, mut t) = at_the_door(t0);
    pet.note_command_done(t, false, Some(40_000));
    let mut flew = false;
    let mut released = false;
    for _ in 0..2_000 {
        t += Duration::from_millis(16);
        pet.note_executing(t, false);
        let f = pet.tick(sense(t, Some((11, 4))));
        flew |= f.action.airborne();
        if !pet.needs_frames() {
            released = true;
            break;
        }
    }
    assert!(
        flew,
        "vigil verdict with returning prompt jump must still fly"
    );
    assert!(
        released && !pet.pending_vigil_pounce && pet.vigil_cheer.is_none(),
        "vigil verdict with returning prompt jump must release needs_frames()"
    );

    let (mut pet_sleep, mut t2) = at_the_door(t0);
    pet_sleep.note_trick(t2, Trick::Sleep, true);
    for _ in 0..40 {
        t2 += Duration::from_millis(16);
        pet_sleep.note_executing(t2, true);
        let _ = pet_sleep.tick(sense(t2, Some((10, 20))));
    }
    assert!(pet_sleep.action == PetAction::Sleep && !pet_sleep.vigil);
    pet_sleep.note_command_done(t2, false, Some(40_000));
    assert!(
        !pet_sleep.pending_vigil_pounce,
        "sleeping cat not in vigil crouch must not latch pending_vigil_pounce"
    );
}

#[test]
fn play_crouch_faces_target_and_pounce_coil_refreshes_coil_d_every_tick() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 5, 20);
    let (_, _) = idle(&mut pet, t, (5, 20), 0.6);
    t += Duration::from_millis(600);
    let s0 = sense(t, Some((5, 20)));
    let width = art_cols(s0.cell_w, s0.cell_h);

    pet.facing_left = true;
    pet.pending_bat = Some((40.0, 5.0));
    assert!(pet.consume_bat(&s0, width));
    assert!(
        !pet.facing_left && pet.action == PetAction::Crouch,
        "consume_bat to the right must face right on the crouch frame"
    );

    pet.clear_play_intents();
    pet.set_action(PetAction::Sit);
    pet.facing_left = false;
    pet.pending_pointer_pounce = Some((4.0, 5.0));
    assert!(pet.consume_pointer_pounce(&s0, width));
    assert!(
        pet.facing_left && pet.action == PetAction::Crouch,
        "consume_pointer_pounce to the left must face left on the crouch frame"
    );

    pet.clear_play_intents();
    pet.set_action(PetAction::Crouch);
    pet.action_t = 0.01;
    pet.coil_d = CROUCH_DUR;
    pet.quiet_leap = false;
    t += Duration::from_millis(16);
    let _ = pet.tick(sense(t, Some((5, 36))));
    assert!(
        pet.coil_d > CROUCH_DUR,
        "pounce coil must refresh coil_d while action_t < CROUCH_DUR: got {}",
        pet.coil_d
    );
}

#[test]
fn next_change_secs_withholds_bored_vignette_offer_under_room_lease_or_hold() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let t = awake(&mut pet, start, 5, 20);
    let (_t, _) = idle(&mut pet, t, (5, 20), 6.0);
    pet.set_action(PetAction::Sit);
    pet.quiet = BORED_AFTER - 0.01;
    pet.content = PLAY_CONTENT + 0.05;
    pet.bored_cool = 0.0;
    pet.trick_settle = Some(Trick::Sit);
    pet.hold_end_q = pet.quiet + TRICK_HOLD;

    let free_offer = pet
        .next_change_secs()
        .expect("content cat should offer bored vignette at BORED_AFTER - q");
    assert!(
        (free_offer - 0.01).abs() < 1e-4,
        "free cat should offer bored vignette at 0.01s, got {free_offer}"
    );

    pet.room_hold = true;
    let hold_offer = pet
        .next_change_secs()
        .expect("held cat should offer next purr step");
    assert!(
        hold_offer > 0.03,
        "room_hold suppresses bored vignette and must not offer 0.01s wake: got {hold_offer}"
    );

    pet.room_hold = false;
    pet.room_lease = true;
    let lease_offer = pet
        .next_change_secs()
        .expect("leased cat should offer next purr step");
    assert!(
        lease_offer > 0.03,
        "room_lease suppresses bored vignette and must not offer 0.01s wake: got {lease_offer}"
    );
}

#[test]
fn sleeper_drop_is_not_reaimed_sideways_and_play_bound_ignores_caret_drift() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 5, 25);
    let (t2, _) = idle(&mut pet, t, (5, 25), SLEEP_AFTER + 0.5);
    t = t2;
    assert_eq!(pet.action, PetAction::Sleep);
    let sleep_col = pet.col;

    t += Duration::from_millis(16);
    let _ = pet.tick(sense(t, Some((6, 5))));
    assert!(
        pet.flight
            .is_some_and(|f| f.drop && (f.to_col - sleep_col).abs() < 1e-4),
        "sleeper drop must start straight down at sleep_col"
    );
    t += Duration::from_millis(16);
    let _ = pet.tick(sense(t, Some((6, 13))));
    let drop_flight = pet.flight.expect("drop still airborne");
    assert!(
        !drop_flight.reaimed && (drop_flight.to_col - sleep_col).abs() < 1e-4,
        "sleeper drop must not be mid-air re-aimed sideways: got {:?}",
        drop_flight
    );

    let mut pet2 = PetBrain::default();
    let mut t3 = awake(&mut pet2, start, 5, 10);
    for c in 11..=16 {
        t3 += Duration::from_millis(60);
        let _ = pet2.tick(sense(t3, Some((5, c))));
    }
    assert!(
        pet2.station_drift(0.5) > 0.1,
        "fixture: open typing rhythm has positive station_drift"
    );
    let s3 = sense(t3, Some((5, 16)));
    let width = art_cols(s3.cell_w, s3.cell_h);
    pet2.col = 10.0;
    pet2.row = 5.0;
    pet2.play_land = Some(PlayLand::PointerFrolic);
    pet2.leave_ground(40.0, 5.0, false, s3.cols, width, LeapPolicy::Play);
    let f2 = pet2.flight.expect("play bound launched");
    assert!(
        f2.big && (f2.to_col - (40.0 - SKID_CELLS)).abs() < 1e-3,
        "play bound must aim at play target without caret station_drift: got {}",
        f2.to_col
    );
}

#[test]
fn jump_notice_and_launch_doors_face_target_without_liftoff_snap() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 5, 10);
    let (t2, _) = idle(&mut pet, t, (5, 10), 1.5);
    t = t2;
    pet.facing_left = true;

    t += Duration::from_millis(16);
    let f_perk = pet.tick(sense(t, Some((5, 22))));
    assert!(
        f_perk.action == PetAction::Perk && !f_perk.facing_left,
        "rightward jump notice Perk must face right toward the caret"
    );

    let mut pet2 = PetBrain::default();
    let mut t3 = awake(&mut pet2, start, 5, 10);
    let (t4, _) = idle(&mut pet2, t3, (5, 10), 1.5);
    t3 = t4;
    pet2.facing_left = true;
    pet2.set_action(PetAction::Perk);
    pet2.action_t = PERK_HOLD;
    pet2.pending_big_jump = true;
    t3 += Duration::from_millis(16);
    let f_gather = pet2.tick(sense(t3, Some((5, 65))));
    assert!(
        f_gather.action == PetAction::Crouch && pet2.big_gather && !f_gather.facing_left,
        "big_gather crouch must face the jump target from its first frame"
    );
}

#[test]
fn room_hold_stands_down_in_flight_games_and_queued_trick_go() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 5, 10);
    let (t2, _) = idle(&mut pet, t, (5, 10), 1.5);
    t = t2;

    pet.room_hold = true;
    pet.pursuit_t = Some(0.5);
    pet.stakeout = true;
    pet.set_action(PetAction::Crouch);
    pet.pending_bat = Some((18.0, 5.0));
    pet.bat_at = Some(t);
    pet.pending_pointer_pounce = Some((18.0, 5.0));
    pet.trick_go = Some(Trick::Roll);

    t += Duration::from_millis(16);
    let mut s = sense(t, Some((5, 10)));
    s.pointer = Some((16.0, 5.0));
    let f = pet.tick(s);
    assert!(
        pet.pursuit_t.is_none()
            && !pet.stakeout
            && pet.trick_go.is_none()
            && pet.play_to.is_none()
            && f.action == PetAction::Stand,
        "room_hold must stand down active pursuit, stakeout, pointer pounce, and trick_go"
    );

    pet.pending_bat = Some((18.0, 5.0));
    pet.pending_pointer_pounce = Some((18.0, 5.0));
    t += Duration::from_millis(16);
    let _ = pet.tick(sense(t, Some((5, 10))));
    assert!(
        pet.pending_bat.is_none() && pet.pending_pointer_pounce.is_none() && pet.play_to.is_none(),
        "room_hold must drop latched bat and pointer pounce without launching"
    );
}

#[test]
fn play_and_hide_crouches_stand_down_cleanly_on_fresh_input_and_line_change() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 5, 10);
    let (t2, _) = idle(&mut pet, t, (5, 10), 1.5);
    t = t2;

    pet.stakeout = true;
    pet.set_action(PetAction::Crouch);
    pet.action_t = 0.5;
    pet.song_left = 2;
    pet.clear_play();
    pet.drop_tricks();
    t += Duration::from_millis(16);
    let f = pet.tick(sense(t, Some((5, 10))));
    assert!(
        !pet.stakeout
            && pet.song_left == 0
            && pet.flight.is_none()
            && !matches!(f.action, PetAction::Crouch | PetAction::Leap),
        "clearing play during stakeout crouch must stand down without launching a leap"
    );

    pet.stakeout = true;
    pet.hiding = true;
    pet.hide_to = Some(30.0);
    pet.set_action(PetAction::Crouch);
    pet.action_t = 0.5;
    t += Duration::from_millis(16);
    let f2 = pet.tick(sense(t, Some((6, 8))));
    assert!(
        !pet.stakeout
            && !pet.hiding
            && pet.hide_to.is_none()
            && f2.action == PetAction::Crouch
            && pet.hop_crouch,
        "line change during stakeout/hiding must clear play intents and enter hop_crouch"
    );

    let mut pet2 = PetBrain::default();
    let mut t3 = awake(&mut pet2, start, 5, 10);
    pet2.hiding = true;
    pet2.hide_to = Some(28.0);
    pet2.set_action(PetAction::Crouch);
    t3 += Duration::from_millis(16);
    let f3 = pet2.tick(sense(t3, Some((5, 11))));
    assert!(
        !pet2.hiding && pet2.hide_to.is_none() && f3.action != PetAction::Crouch,
        "same-row forward keystroke must end hiding and clear hide_to"
    );
}

#[test]
fn interrupted_crouch_land_and_frolic_substates_clear_and_solved_reaim_spends_jump_latch() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 5, 20);
    let (t2, _) = idle(&mut pet, t, (5, 20), 1.5);
    t = t2;

    pet.stakeout = true;
    pet.play_to = Some((25.0, 5.0));
    pet.play_land = Some(PlayLand::PointerFrolic);
    pet.set_action(PetAction::Crouch);
    t += Duration::from_millis(16);
    let f = pet.tick(sense(t, Some((5, 19))));
    assert_eq!(f.action, PetAction::Perk);
    assert!(
        !pet.stakeout && pet.play_to.is_none() && pet.play_land.is_none(),
        "flinching out of a play/stakeout crouch must clear stakeout, play_to, and play_land"
    );

    pet.set_action(PetAction::Land);
    pet.land_t = LAND_DUR;
    pet.bound2 = Some((40.0, 5.0, false));
    pet.resume = Some((PetAction::Sit, 0.3));
    pet.play_land = Some(PlayLand::PointerFrolic);
    t += Duration::from_millis(16);
    let f_land = pet.tick(sense(t, Some((5, 18))));
    assert_eq!(f_land.action, PetAction::Perk);
    assert!(
        pet.land_t == 0.0
            && pet.bound2.is_none()
            && pet.resume.is_none()
            && pet.play_land.is_none(),
        "flinching out of Land must clear land_t, bound2, resume, and play_land"
    );

    let mut pet2 = PetBrain::default();
    let mut t3 = awake(&mut pet2, start, 5, 10);
    t3 += Duration::from_millis(16);
    let _ = pet2.tick(sense(t3, Some((5, 20))));
    while pet2.flight.is_none() {
        t3 += Duration::from_millis(16);
        let _ = pet2.tick(sense(t3, Some((5, 20))));
    }
    t3 += Duration::from_millis(16);
    let _ = pet2.tick(sense(t3, Some((5, 27))));
    assert!(
        pet2.flight.is_some_and(|fl| fl.reaimed) && !pet2.pending_pounce && !pet2.pending_big_jump,
        "a solved mid-air re-aim must consume pending_pounce and pending_big_jump"
    );
}

#[test]
fn regrid_carry_preserves_quiet_leap_and_repair_target_and_stream_watch_clears_play_intents() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 5, 10);

    pet.row = 4.0;
    pet.action = PetAction::Run;
    pet.console.repair_target = Some((5.0, 10.0));
    pet.console.last_seat = Some((12.0, 4.0));
    pet.rebase_geometry();
    t += Duration::from_millis(16);
    let _ = pet.tick(sense(t, Some((7, 10))));
    assert!(
        (pet.row - 6.0).abs() < 1e-4 && !pet.quiet_leap,
        "regrid carry of a neighbour-row cat must not falsely latch quiet_leap (row={}, quiet_leap={})",
        pet.row,
        pet.quiet_leap
    );
    assert_eq!(pet.console.repair_target, Some((7.0, 10.0)));
    assert_eq!(pet.console.last_seat, Some((12.0, 6.0)));
    pet.console.rest_seat.set(Some((12.0, 4.0)));
    assert!(pet.carry_with_regrid(2.0, 24));
    assert_eq!(pet.console.rest_seat.get(), Some((12.0, 6.0)));

    let mut pet2 = PetBrain::default();
    let mut t2 = awake(&mut pet2, start, 5, 79);
    pet2.pursuit_t = Some(0.5);
    pet2.stakeout = true;
    pet2.play_to = Some((25.0, 5.0));
    pet2.set_action(PetAction::Crouch);
    let jump = |step: usize, t: Instant| {
        let wrapped = step.is_multiple_of(2);
        let mut s = sense(t, Some((5, if wrapped { 0 } else { 79 })));
        s.wrapped = wrapped;
        s
    };
    for step in 0..3 {
        t2 += Duration::from_millis(16);
        let _ = pet2.tick(jump(step, t2));
    }
    assert!(
        pet2.stream && pet2.pursuit_t.is_none() && !pet2.stakeout && pet2.play_to.is_none(),
        "arming stream watch on same-row wraps/jumps must clear play intents"
    );
    // A coil the burst holds ([`SETTLE`]) waits for the caret to settle; a burst that
    // outlasts [`SETTLE_CAP`] is a stream, and the coil perks up to watch it.
    let cap = (SETTLE_CAP / 0.016) as usize + 2;
    for step in 3..3 + cap {
        t2 += Duration::from_millis(16);
        let _ = pet2.tick(jump(step, t2));
    }
    assert_eq!(
        pet2.action,
        PetAction::Perk,
        "a stream of same-row wraps/jumps perks the cat out of its crouch"
    );
}

#[test]
fn crouch_gathers_track_live_facing_and_room_hold_suppresses_yawn_hide_song_and_play_crouch() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let mut t = awake(&mut pet, start, 5, 30);

    pet.facing_left = false;
    pet.set_action(PetAction::Crouch);
    t += Duration::from_millis(16);
    let f_crouch = pet.tick(sense(t, Some((5, 5))));
    assert_eq!(f_crouch.action, PetAction::Crouch);
    assert!(
        f_crouch.facing_left,
        "pounce crouch must update facing_left when target switches sides mid-coil"
    );

    let mut pet_wall = PetBrain::default();
    let t_wall = awake(&mut pet_wall, start, 5, 0);
    pet_wall.col = 0.0;
    pet_wall.facing_left = false;
    pet_wall.pending_pointer_pounce = Some((0.2, 5.0));
    let width = art_cols(8, 16);
    assert!(pet_wall.consume_pointer_pounce(&sense(t_wall, Some((5, 0))), width));
    assert!(
        pet_wall.facing_left,
        "wall-clamped pointer pounce must face the toy via center fallback"
    );

    let mut pet_hold = PetBrain::default();
    let mut t_hold = awake(&mut pet_hold, start, 5, 10);
    let (t_sat, _) = idle(&mut pet_hold, t_hold, (5, 10), SIT_AFTER + 0.5);
    t_hold = t_sat;
    pet_hold.force_seed(0);
    pet_hold.quiet = pet_hold.loaf_after() - YAWN_DUR * 0.5;
    assert!(
        pet_hold.sit_yawn(),
        "fixture: without room_hold, even seed yawns before loaf"
    );
    pet_hold.room_hold = true;
    assert!(
        !pet_hold.sit_yawn(),
        "room_hold withholds the loaf melt and must suppress sit_yawn"
    );

    pet_hold.hide_to = Some(20.0);
    pet_hold.hiding = true;
    pet_hold.play_to = Some((20.0, 5.0));
    pet_hold.play_land = Some(PlayLand::PointerFrolic);
    pet_hold.song_left = 2;
    pet_hold.pending_look = Some(30.0);
    pet_hold.look_at = Some(t_hold);
    pet_hold.groom_owed = true;
    pet_hold.set_action(PetAction::Crouch);
    t_hold += Duration::from_millis(16);
    let _ = pet_hold.tick(sense(t_hold, Some((5, 10))));
    t_hold += Duration::from_millis(16);
    let f_hold = pet_hold.tick(sense(t_hold, Some((5, 10))));
    assert!(
        pet_hold.hide_to.is_none()
            && !pet_hold.hiding
            && pet_hold.play_to.is_none()
            && pet_hold.play_land.is_none()
            && pet_hold.song_left == 0
            && pet_hold.pending_look.is_none()
            && !pet_hold.groom_owed
            && f_hold.action == PetAction::Sit,
        "room_hold must stand down hide_to, hiding, play_to, song_left, pending_look, and groom_owed"
    );
}

#[test]
fn brief_droop_is_not_grief_and_ended_vigil_wakes_one_tick_to_stand_down() {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    let _ = awake(&mut pet, start, 5, 10);

    pet.droop_brief = true;
    pet.set_action(PetAction::Droop);
    assert!(
        !pet.grieving(),
        "brief ear-flat droop must not report grieving()"
    );
    pet.set_action(PetAction::Leap);
    pet.droop_brief = true;
    pet.resume = Some((PetAction::Droop, 0.2));
    assert!(
        !pet.grieving(),
        "resumed brief ear-flat droop must not report grieving()"
    );
    pet.droop_brief = false;
    assert!(
        pet.grieving(),
        "resumed command-failure sulk droop must report grieving()"
    );
    pet.resume = None;
    pet.set_action(PetAction::Droop);
    assert!(
        pet.grieving(),
        "active command-failure sulk droop must report grieving()"
    );

    let (mut pet_vigil, mut t_vigil) = at_the_door(start);
    assert!(
        !pet_vigil.needs_frames() && pet_vigil.next_change_deadline(t_vigil).is_none(),
        "active vigil crouch must park the frame lane while the job is running"
    );
    pet_vigil.note_executing(t_vigil, false);
    assert!(
        pet_vigil.needs_frames() && pet_vigil.next_change_deadline(t_vigil).is_some(),
        "ending the executing phase on a vigil crouch must wake the frame lane to stand down"
    );
    t_vigil += Duration::from_millis(16);
    let f_stood = pet_vigil.tick(sense(t_vigil, Some((10, 20))));
    assert!(
        f_stood.action != PetAction::Crouch && !pet_vigil.vigil,
        "one tick after executing ends must clear the vigil crouch"
    );

    pet_vigil.deferred_hidden_landing = Some((12.0, 5.0));
    assert!(pet_vigil.carry_with_regrid(3.0, 24));
    assert_eq!(pet_vigil.deferred_hidden_landing, Some((12.0, 8.0)));
}

#[test]
fn retire_unowned_retains_console_world_and_ink_spans_capacity() {
    use crate::pet_world::{PetPane, PetWorldFacts};
    use aterm_core::terminal::Terminal;

    let start = Instant::now();
    let mut pet = PetBrain::default();
    let _ = awake(&mut pet, start, 5, 10);
    pet.sense_ink(0, &[(1, 4), (6, 9)], Some(5));
    let mut term = Terminal::new(24, 80);
    let input = term.cell_frame(24, 80);
    let facts = PetWorldFacts::read(&term, 7);
    pet.observe_console(&input, &facts, PetPane::full(&input));
    let world_ptr: *const crate::pet_world::PetWorld =
        pet.console.world.as_deref().expect("world allocated");
    let spans_cap = pet.ink_spans.capacity();
    assert!(spans_cap >= 2);

    pet.retire_unowned();
    assert!(!pet.is_active());
    assert!(pet.ink_spans.is_empty());
    assert_eq!(pet.ink_spans.capacity(), spans_cap);
    let spare = pet
        .console
        .spare_world
        .as_deref()
        .expect("retired PetWorld should be parked in spare_world");
    assert_eq!(core::ptr::from_ref(spare), world_ptr);
}

/// How [`wall_burst`] runs: the laws [`PetBrain::set_old_path`] reverts (its negative
/// controls), whether the console observes the typed line, as a window's does (its letter
/// reader finds the seat by the caret that covers no letter once the hand stops,
/// [`PetBrain::margin_rest_seat`]), and the pane's rows (14 where `0`; the trail gallery's
/// `split` gives the line's pane 6, the line on its last row but one).
#[derive(Clone, Copy, Default)]
struct BurstWay {
    old: &'static [OldPath],
    observed: bool,
    rows: u16,
}

/// The trail gallery's `burst` geometry, replayed on a [`PetBrain`]: a line typed at 20
/// keys a second from the prompt on row 4 of a 72-column pane to three columns short of
/// the margin, the cat at rest beside the prompt when the first key comes and running
/// ahead of the line to the wall, then 4.6 s of quiet.
fn wall_burst(way: BurstWay) -> WallBurst {
    use crate::pet_world::{PetPane, PetWorldFacts};
    use aterm_core::terminal::Terminal;
    const TEXT: &str = "echo the quick brown fox jumps over the lazy dog as the kitty flies";
    let rows = if way.rows == 0 { 14u16 } else { way.rows };
    let (cols, cw, ch) = (72u16, 14u16, 26u16);
    let sense_at = |now: Instant, caret: (u16, u16)| PetSense {
        caret_drawn: true,
        now,
        caret: Some(caret),
        wrapped: false,
        rows,
        cols,
        cell_w: cw,
        cell_h: ch,
        reduced_motion: false,
        output_burst: false,
        pointer: None,
    };
    let text: Vec<char> = TEXT.chars().collect();
    let lettered = |typed: usize, col: u16| -> bool {
        col == 0 || (col >= 2 && usize::from(col - 2) < typed && text[usize::from(col - 2)] != ' ')
    };
    let on_letters = |typed: usize, body: Option<(i32, i32, i32, i32)>| -> usize {
        let Some((x0, x1, y0, y1)) = body else {
            return 0;
        };
        let pad = (INK_PAD * f32::from(cw)) as i32;
        let (cy0, cy1) = (4 * i32::from(ch), 5 * i32::from(ch));
        (0..cols)
            .filter(|&c| {
                let (cx0, cx1) = (
                    i32::from(c) * i32::from(cw),
                    i32::from(c + 1) * i32::from(cw),
                );
                lettered(typed, c) && x0 + pad < cx1 && x1 - pad > cx0 && y0 < cy1 && y1 > cy0
            })
            .count()
    };
    let start = Instant::now();
    let mut pet = PetBrain::default();
    for &law in way.old {
        pet.set_old_path(law, true);
    }
    let mut term = Terminal::new(rows, cols);
    let mut observe = |pet: &mut PetBrain, typed: usize| {
        if !way.observed {
            return;
        }
        let line: String = text.iter().take(typed).collect();
        term.process(format!("\x1b[2J\x1b[5;1H$ {line}").as_bytes());
        let input = term.cell_frame(usize::from(rows), usize::from(cols));
        let facts = PetWorldFacts::read(&term, 7);
        pet.observe_console(&input, &facts, PetPane::full(&input));
        pet.set_console_presentable(true);
    };
    let mut spans = vec![(0u16, 0u16); usize::from(rows)];
    spans[4] = (0, 1);
    let mut t = start;
    for _ in 0..200 {
        t += Duration::from_millis(16);
        observe(&mut pet, 0);
        pet.sense_ink(0, &spans, None);
        let _ = pet.tick(sense_at(t, (4, 2)));
    }
    // The last key, and when the cat that sat at its sit ([`SIT_AFTER`]) has hopped off
    // the letters and come down.
    let last_key = 50 * (text.len() as u64 - 1);
    let rested = last_key + (SIT_AFTER * 1000.0) as u64 + 600;
    let (mut typed, mut ms) = (0usize, 0u64);
    let mut out = WallBurst::default();
    let (mut seen, mut was_leap, mut walled) = (0u8, false, false);
    while ms < last_key + 4600 {
        t += Duration::from_millis(16);
        ms += 16;
        while typed < text.len() && (typed as u64) * 50 <= ms {
            typed += 1;
        }
        let caret = (4u16, 2 + typed as u16);
        spans[4] = (0, caret.1);
        observe(&mut pet, typed);
        pet.sense_ink(0, &spans, None);
        let f = pet.tick(sense_at(t, caret));
        out.dipped |= f.alpha < seen;
        seen = seen.max(f.alpha);
        let leap = f.action == PetAction::Leap;
        if leap && !was_leap {
            match pet.flight {
                Some(fl) if fl.big => out.launches += 1,
                _ => out.other_flights += 1,
            }
        }
        was_leap = leap;
        let on = on_letters(typed, f.body_px(cw, ch, cols, rows));
        walled |= pet.wrap_ahead.is_some();
        if !walled {
            out.start += on;
            if on > 0 {
                out.start_last = ms;
            }
        } else if ms < rested {
            out.wall += on;
        } else {
            out.rest += on;
        }
    }
    out.rest_at = (pet.col, pet.row);
    out
}

/// What [`wall_burst`] saw: cell-frames are lettered cells of the typed line under the
/// cat's body (the drawn art box, shaved by [`INK_PAD`] as every ink law reads it), one
/// per frame each.
#[derive(Debug, Default)]
struct WallBurst {
    /// Bounds begun.
    launches: u32,
    /// Every other flight begun.
    other_flights: u32,
    /// Cell-frames on letters from the first key until the cat takes the margin: the run's
    /// start and the run to the wall.
    start: usize,
    /// The last of those frames, ms after the first key (0 where there is none).
    start_last: u64,
    /// Cell-frames on letters at the margin until the cat has sat (and, with the margin's
    /// slide reverted, hopped off them), on the ground or in the air: the line's last letters
    /// typed over the cat waiting for the wrap.
    wall: usize,
    /// Cell-frames on letters from then to the end: the rest.
    rest: usize,
    /// Whether the cat's presence ever dipped.
    dipped: bool,
    /// Where the cat stands at the end.
    rest_at: (f32, f32),
}

/// THE BURST NEVER LAYS A RESTING CAT ON ITS LETTERS, on the trail gallery's `burst`
/// geometry ([`wall_burst`]), from the first key to 4.6 s after the last. The cat at rest
/// beside the prompt gets going on foot with the first keys: no flight leaves at a typing
/// run's start (looks8's first keys' spring hopped there, after almost every pause in
/// human typing; the owner's "jumping", r24 K7-1), so a fast hand's first letters run over
/// the cat as it gets going (drawn under the text, the letters knocked out of its body),
/// and none once it runs ahead of the line ([`FIRST_KEYS_LAG_MS`]). No bound leaves for a
/// wrap that never comes, and no hop leaves for one that may (looks9's climb onto the line
/// as the writing came to the margin cost every wrapped line a second flight, r24 K7-1). The
/// cat waits for the wrap at the wall off the line, on the row under it, slid there on its
/// paws ([`PetBrain::margin_seat_off_the_line`]), so the line's last letters are typed past
/// it and none under it, and it rests there. Its presence never dips. With the console
/// unobserved (no letter reader certifies a seat) the wall holds. Negative control:
/// [`OldPath::MarginKeepsTheLine`] reverted alone, the letters run under the cat at the wall
/// and it hops off them once the hand has stopped ([`PetBrain::margin_rest_seat`]).
#[test]
fn the_burst_never_lays_a_resting_cat_on_its_letters() {
    let law = wall_burst(BurstWay {
        observed: true,
        ..BurstWay::default()
    });
    assert_eq!(
        (law.launches, law.other_flights),
        (0, 0),
        "no flight at the first key, no bound for a wrap that never comes, and no hop at the \
         margin ({law:?})"
    );
    assert_eq!(
        law.wall, 0,
        "the line's last letters ran under the cat waiting at the wall ({law:?})"
    );
    let old = wall_burst(BurstWay {
        observed: true,
        old: &[OldPath::MarginKeepsTheLine],
        ..BurstWay::default()
    });
    assert!(
        old.wall > 0 && old.other_flights == 1,
        "control: reverted, the last letters run under the cat at the wall and it hops off \
         them ({old:?})"
    );
    assert!(
        law.start_last <= FIRST_KEYS_LAG_MS,
        "the line's letters ran under the cat past its start ({law:?})"
    );
    assert_eq!(
        law.rest, 0,
        "the cat's body lay on the burst's letters at rest ({law:?})"
    );
    assert!(
        (law.rest_at.1 - 5.0).abs() < 0.01 && law.rest_at.0 > 60.0,
        "the cat rests under the line by the caret ({law:?})"
    );
    assert!(!law.dipped, "the cat's presence dipped through the burst");
    let unread = wall_burst(BurstWay::default());
    assert!(
        unread.start_last <= FIRST_KEYS_LAG_MS && unread.launches == 0,
        "unobserved, the run lay on the letters, or a bound left for the wrap ({unread:?})"
    );
}

/// How long (ms after a fast hand's first key) the first letters may run over a cat at rest
/// beside the prompt as it gets going on foot ([`wall_burst`]): the first six keys at the
/// replay's 50 ms a key, before the writing's rhythm gives the cat its lead.
const FIRST_KEYS_LAG_MS: u64 = 6 * 50;

/// THE BURST IN A SHORT PANE RESTS ON THE LINE'S TOP: the trail gallery's `split` types the
/// burst on the last row but one of a 6-row pane, so the row under the line is the pane's
/// last, where the body would be fitted. No flight leaves for the wrap that never comes; the
/// cat slides onto the row over the line, which holds it whole
/// ([`PetBrain::margin_seat_off_the_line`]), and rests on row 3, no frame on the letters.
#[test]
fn a_short_pane_s_burst_rests_on_the_line_s_top() {
    let law = wall_burst(BurstWay {
        observed: true,
        rows: 6,
        ..BurstWay::default()
    });
    assert_eq!(
        (law.launches, law.other_flights),
        (0, 0),
        "no flight at the first key and none at the margin ({law:?})"
    );
    assert_eq!(
        law.wall, 0,
        "the line's last letters ran under the cat waiting at the wall ({law:?})"
    );
    assert!(
        law.start_last <= FIRST_KEYS_LAG_MS,
        "the line's letters ran under the cat past its start ({law:?})"
    );
    assert_eq!(
        law.rest, 0,
        "the cat's body lay on the burst's letters at rest ({law:?})"
    );
    assert!(
        (law.rest_at.1 - 3.0).abs() < 0.01,
        "the cat rests on the line's top ({law:?})"
    );
    assert!(!law.dipped, "the cat's presence dipped through the burst");
}

/// The trail gallery's `cjk` geometry, replayed on a [`PetBrain`]: `echo 你好世界` typed at
/// 60 ms a key from the prompt on row 4 of a 72-column pane (each wide glyph two cells), then
/// half a second of quiet, the cat at rest beside the prompt when the first key comes. What
/// [`wide_burst_keyed`] counts: its drawn box, unshaved, against the caret's cell (what
/// `aterm-lab trail-companion` reads off `pet_body=`).
fn wide_burst(old: &[OldPath]) -> WideMet {
    wide_burst_framed(old, false, 200)
}

/// [`wide_burst`], `sparse`: a present only 30 ms after each key and then the trail
/// gallery's fade (three 40 ms apart, six 60 ms apart), as a headless gallery instance
/// renders, instead of one every 16 ms; `idle` presents 16 ms apart before the first key
/// (one: the cat just born, asleep beside the prompt, as the gallery's is).
fn wide_burst_framed(old: &[OldPath], sparse: bool, idle: usize) -> WideMet {
    let keys: Vec<u64> = (0..9).map(|i| 60 * i).collect();
    let presents = if sparse {
        WidePresents::AfterEachKey {
            delay: 30,
            own: false,
        }
    } else {
        WidePresents::Every16
    };
    wide_burst_keyed(old, &keys, presents, idle, PROMPT_CELL)
}

/// The prompt's cell in [`wide_burst_keyed`]'s pane: where the caret stands before the
/// first key.
const PROMPT_CELL: (u16, u16) = (4, 2);

/// How long (ms) after the last key a cat running down wide writing at its fastest may
/// still lie on the caret's cell ([`WideMet::late`]): two presents. From rest it gets going
/// on foot with the first keys, no flight at a typing run's start (r24 K7-1), and two cells
/// a key carry the caret into its box as it runs ([`wide_burst_keyed`]); it catches up as
/// the writing stops.
const WIDE_CATCH_UP_MS: u64 = 40;

/// How [`wide_burst_keyed`] presents: one every 16 ms, or `delay` ms after each key (and at
/// the key itself too, `own`) and then the trail gallery's fade (three 40 ms apart, six 60
/// ms apart).
#[derive(Clone, Copy)]
enum WidePresents {
    Every16,
    AfterEachKey { delay: u64, own: bool },
}

/// What [`wide_burst_keyed`] counts: the presents after the last key's own whose drawn box,
/// unshaved, meets the caret's cell, those from the first wide key on, those from the first
/// wide key on whose box lies on the caret's cell or the row's lettered cells (what
/// `aterm-lab trail-companion` counts), and those more than [`WIDE_CATCH_UP_MS`] after the
/// last key whose box meets the caret's cell.
#[derive(Debug)]
struct WideMet {
    after: usize,
    wide: usize,
    on_writing: usize,
    late: usize,
}

/// [`wide_burst_framed`] with the keys at `keys` (ms; nine, one for each of `echo 你好世界`,
/// its first wide glyph the sixth), the caret at `from` through the `idle` presents: at the
/// prompt ([`PROMPT_CELL`]), the cat at rest beside it; elsewhere, the cat at rest there
/// comes to the prompt as the first key moves the caret to it, and lands with the writing
/// under way.
fn wide_burst_keyed(
    old: &[OldPath],
    keys: &[u64],
    presents: WidePresents,
    idle: usize,
    from: (u16, u16),
) -> WideMet {
    use crate::pet_world::{PetPane, PetWorldFacts};
    use aterm_core::terminal::Terminal;
    const TEXT: &str = "echo 你好世界";
    let (rows, cols, cw, ch) = (14u16, 72u16, 14u16, 26u16);
    let sense_at = |now: Instant, caret: (u16, u16)| PetSense {
        caret_drawn: true,
        now,
        caret: Some(caret),
        wrapped: false,
        rows,
        cols,
        cell_w: cw,
        cell_h: ch,
        reduced_motion: false,
        output_burst: false,
        pointer: None,
    };
    let text: Vec<char> = TEXT.chars().collect();
    let width = |c: char| if c.is_ascii() { 1u16 } else { 2 };
    let start = Instant::now();
    let mut pet = PetBrain::default();
    for &law in old {
        pet.set_old_path(law, true);
    }
    let mut term = Terminal::new(rows, cols);
    let mut observe = |pet: &mut PetBrain, typed: usize| {
        let line: String = text.iter().take(typed).collect();
        term.process(format!("\x1b[2J\x1b[5;1H$ {line}").as_bytes());
        let input = term.cell_frame(usize::from(rows), usize::from(cols));
        let facts = PetWorldFacts::read(&term, 7);
        pet.observe_console(&input, &facts, PetPane::full(&input));
        pet.set_console_presentable(true);
    };
    let mut spans = vec![(0u16, 0u16); usize::from(rows)];
    let mut t = start;
    if from == PROMPT_CELL {
        spans[4] = (0, 1);
        for _ in 0..idle {
            t += Duration::from_millis(16);
            observe(&mut pet, 0);
            pet.sense_ink(0, &spans, None);
            let _ = pet.tick(sense_at(t, PROMPT_CELL));
        }
    } else {
        // A prompt of its own where the caret waits, and the screen's prompt row bare.
        spans[usize::from(from.0)] = (0, from.1.saturating_sub(1));
        for _ in 0..idle {
            t += Duration::from_millis(16);
            pet.sense_ink(0, &spans, None);
            let _ = pet.tick(sense_at(t, from));
        }
        spans[usize::from(from.0)] = (0, 0);
        spans[4] = (0, 1);
    }
    assert_eq!(keys.len(), text.len(), "fixture: a key for each of {TEXT}");
    let last_key = keys[keys.len() - 1];
    let first_wide = keys[5];
    let presents: Vec<u64> = match presents {
        WidePresents::AfterEachKey { delay, own } => {
            let mut fade = vec![last_key + delay];
            for gap in [40u64, 40, 40, 60, 60, 60, 60, 60, 60] {
                fade.push(fade[fade.len() - 1] + gap);
            }
            let mut at: Vec<u64> = keys.iter().map(|k| k + delay).collect();
            if own {
                at.extend(keys);
            }
            at.extend(fade.into_iter().skip(1));
            at.sort_unstable();
            at.dedup();
            at
        }
        WidePresents::Every16 => (1..=(last_key + 500) / 16).map(|i| 16 * i).collect(),
    };
    let (mut typed, mut ms) = (0usize, 0u64);
    let mut met = WideMet {
        after: 0,
        wide: 0,
        on_writing: 0,
        late: 0,
    };
    for at in presents {
        t += Duration::from_millis(at - ms);
        ms = at;
        while typed < text.len() && keys[typed] <= ms {
            typed += 1;
        }
        let col = 2 + text.iter().take(typed).map(|&c| width(c)).sum::<u16>();
        let caret = (4u16, col);
        spans[4] = (0, col);
        observe(&mut pet, typed);
        pet.sense_ink(0, &spans, None);
        let f = pet.tick(sense_at(t, caret));
        let Some((x0, x1, y0, y1)) = f.body_px(cw, ch, cols, rows) else {
            continue;
        };
        let (cx0, cx1) = (
            i32::from(col) * i32::from(cw),
            i32::from(col + 1) * i32::from(cw),
        );
        let row = y0 < 5 * i32::from(ch) && y1 > 4 * i32::from(ch);
        let meets = row && x0 < cx1 && x1 > cx0;
        // The row's letters: `$ ` and what is typed, left of the caret.
        let on_letters = row && x0 < cx0;
        if meets && ms > last_key {
            met.after += 1;
        }
        if meets && ms > last_key + WIDE_CATCH_UP_MS {
            met.late += 1;
        }
        if meets && ms >= first_wide {
            met.wide += 1;
        }
        if (meets || on_letters) && ms >= first_wide {
            met.on_writing += 1;
        }
    }
    met
}

/// A STANCE'S STRETCH NEVER CARRIES THE BODY ONTO THE CARET'S CELL
/// ([`PetBrain::keep_stretch_off_caret`]), on the trail gallery's geometry (14 x 26 px
/// cells): a cat that typed `yz` at the start of a row and rests beside the caret draws
/// its box off the caret's cell in every frame of three seconds at rest, its stretched
/// poses among them, each frame as wide as the stretch about its centre draws it (the
/// swell is kept whole, reaching away from the caret) and in the same column. Negative
/// control ([`OldPath::StretchOffCaret`] alone): the standing stretch lays the box a pixel
/// into the caret's cell, as the gallery's `fold-rest` `f1400` drew it in every companion
/// style.
#[test]
fn a_resting_cat_s_stretch_keeps_off_the_caret() {
    let (rows, cols, cw, ch) = (14u16, 72u16, 14u16, 26u16);
    let at = |now: Instant, caret: (u16, u16)| PetSense {
        rows,
        cols,
        cell_w: cw,
        cell_h: ch,
        ..sense(now, Some(caret))
    };
    let caret = (5u16, 2u16);
    let (x0, x1) = (
        i32::from(caret.1) * i32::from(cw),
        i32::from(caret.1 + 1) * i32::from(cw),
    );
    let (y0, y1) = (
        i32::from(caret.0) * i32::from(ch),
        i32::from(caret.0 + 1) * i32::from(ch),
    );
    // Each frame at rest: its column, its drawn box, and whether it was drawn stretched.
    type AtRest = (f32, (i32, i32, i32, i32), bool);
    let rest = |old: bool| -> Vec<AtRest> {
        let mut pet = PetBrain::default();
        pet.set_old_path(OldPath::StretchOffCaret, old);
        let mut t = Instant::now();
        for col in 0..=caret.1 {
            for _ in 0..4 {
                t += Duration::from_millis(16);
                let _ = pet.tick(at(t, (caret.0, col)));
            }
        }
        let mut out = Vec::new();
        for k in 0..(4000 / 16) {
            t += Duration::from_millis(16);
            let f = pet.tick(at(t, caret));
            // A second to come to rest beside the caret, then three at rest.
            if k < 1000 / 16 {
                continue;
            }
            let b = f
                .body_px(cw, ch, cols, rows)
                .expect("the resting cat is drawn");
            out.push((f.col, b, f.scale_x > 1.0));
        }
        out
    };
    let meets = |b: &(i32, i32, i32, i32)| b.0 < x1 && b.1 > x0 && b.2 < y1 && b.3 > y0;
    let (now, then) = (rest(false), rest(true));
    assert!(
        now.iter().any(|f| f.2),
        "fixture: the resting cat is never drawn stretched"
    );
    let on = now.iter().filter(|f| meets(&f.1)).count();
    assert_eq!(
        on, 0,
        "{on} frames at rest lay the stretched box on the caret's cell"
    );
    assert_eq!(now.len(), then.len());
    for (i, (a, b)) in now.iter().zip(&then).enumerate() {
        assert!(
            a.0 == b.0 && a.1.1 - a.1.0 == b.1.1 - b.1.0 && (a.1.2, a.1.3) == (b.1.2, b.1.3),
            "frame {i}: the stretch kept off the caret moved the body or changed its size: \
             column {} box {:?}, about its centre column {} box {:?}",
            a.0,
            a.1,
            b.0,
            b.1
        );
    }
    let old = then.iter().filter(|f| meets(&f.1)).count();
    assert!(
        old > 0,
        "negative control: stretched about its centre, the resting cat's box never met the \
         caret's cell"
    );
}

/// One frame of [`hasty_move_up`]: its time (ms), its column, its drawn box, the box its
/// stretch about its centre draws, and whether it was drawn from an edge.
type HastyFrame = (u64, f32, (i32, i32, i32, i32), (i32, i32, i32, i32), bool);

/// The trail gallery's hasty `move-up` ([`OldPath::WatchFromTheLine`]'s scene) on a pane of
/// 14 rows of 72 `cell` px cells, the laws in `old` reverted: lettered rows over a prompt
/// `$ ` on row 7, `./k` typed past the console at 60 ms a key from 1 s, its Enter's echo
/// 10 ms after the last key (the caret's carriage return and line feed onto row 8, before
/// the dog has walked clear of the `k`), and a second later the program's `[vim] NORMAL` on
/// row 8 and the caret moved up onto row 3's `!`, as vim's `k` moves it; a frame every
/// 16 ms to 3.4 s, each from the Enter on kept.
fn hasty_move_up(cell: (u16, u16), old: &[OldPath]) -> Vec<HastyFrame> {
    let (rows, cols) = (14u16, 72u16);
    let (cw, ch) = cell;
    let at = |now: Instant, caret: (u16, u16)| PetSense {
        rows,
        cols,
        cell_w: cw,
        cell_h: ch,
        ..sense(now, Some(caret))
    };
    let mut pet = PetBrain::default();
    pet.set_species(PetSpecies::Dog);
    for &law in old {
        pet.set_old_path(law, true);
    }
    let mut spans = vec![(0u16, 0u16); usize::from(rows)];
    for (r, end) in [54u16, 11, 37, 46, 62, 1].into_iter().enumerate() {
        spans[r] = (0, end);
    }
    spans[7] = (0, 2);
    let (keys, enter, mark) = ([1_000u64, 1_060, 1_120], 1_130u64, 2_130u64);
    let start = Instant::now();
    let mut caret = (7u16, 2u16);
    let mut out = Vec::new();
    for ms in (0..=3_400u64).step_by(16) {
        // What came since the last frame, up to this one.
        let since = |at: u64| at + 16 > ms && at <= ms;
        for (k, &key) in keys.iter().enumerate() {
            if since(key) {
                caret = (7, 3 + k as u16);
                spans[7] = (0, caret.1);
            }
        }
        if since(enter) {
            caret = (8, 0);
        }
        if since(mark) {
            spans[8] = (0, 12);
            caret = (3, 11);
        }
        pet.sense_ink(0, &spans, Some(caret.0));
        let f = pet.tick(at(start + Duration::from_millis(ms), caret));
        if ms < enter {
            continue;
        }
        let Some(drawn) = f.body_px(cw, ch, cols, rows) else {
            continue;
        };
        let mut centre = f;
        centre.stretch_from = StretchFrom::Centre;
        let centred = centre
            .body_px(cw, ch, cols, rows)
            .expect("drawn about its centre as from its edge");
        let edge = !matches!(f.stretch_from, StretchFrom::Centre);
        out.push((ms, f.col, drawn, centred, edge));
    }
    out
}

/// A STANCE'S STRETCH NEVER CARRIES THE BODY ONTO LETTERS IT KEEPS OFF
/// ([`PetBrain::keep_stretch_off_letters`]), in the trail gallery's hasty `move-up`
/// ([`hasty_move_up`]) under Linux's bundled JetBrains Mono's 13 x 29 px cells (a body 6.25
/// cells across) and on the gallery's 14 x 26 (5.2 across): the dog over `./k`'s `k` when
/// the Enter comes steps clear of it along the line ([`PetBrain::watching_from_the_line`])
/// to a stand whose footprint lies 0.15 of a cell past the `k`'s cell, and coils there
/// to leap when the caret moves up. From 120 ms after the Enter (the `k` typed under its
/// tail is left behind by then), no frame whose box covers the line's row by a quarter of a
/// row or more lays its drawn footprint ([`INK_PAD`] in from each edge) on the line's cells
/// by a pixel or more. Each frame the law draws from an edge is the frame the dog draws
/// without it, its stretch about its centre, kept whole and moved off the letters; every
/// other frame is the same frame. Negative control ([`OldPath::StretchOffLetters`] alone):
/// the coil stretched about its centre lays the footprint back onto the `k`'s cell, as the
/// render replay's dog laid its body on the `k`'s ink (`companion_off_letters`).
#[test]
fn a_coil_beside_the_letters_it_stepped_clear_of_keeps_off_them() {
    for cell in [(13u16, 29u16), (14, 26)] {
        let (cw, ch) = (i32::from(cell.0), i32::from(cell.1));
        let pad = INK_PAD * cw as f32;
        // The line's row (7), covered by a quarter of a row, and the line's cells (`$ ./k`).
        let (top, bottom, line) = (7 * ch, 8 * ch, 5 * cw);
        let onto = |b: &(i32, i32, i32, i32)| {
            (b.3.min(bottom) - b.2.max(top)) * 4 >= ch && b.0 as f32 + pad <= (line - 1) as f32
        };
        let onto_letters = |frames: &[HastyFrame]| -> Vec<u64> {
            frames
                .iter()
                .filter(|f| f.0 >= 1_130 + 120 && onto(&f.2))
                .map(|f| f.0)
                .collect()
        };
        let now = hasty_move_up(cell, &[]);
        let old = hasty_move_up(cell, &[OldPath::StretchOffLetters]);
        assert_eq!(
            onto_letters(&now),
            Vec::<u64>::new(),
            "{cell:?}: frames (ms) laying the drawn footprint on the line's letters"
        );
        assert_eq!(
            now.len(),
            old.len(),
            "fixture ({cell:?}): the same frames drawn"
        );
        for (a, b) in now.iter().zip(&old) {
            assert!(
                a.0 == b.0 && a.1 == b.1 && a.3 == b.2 && (a.4 || a.2 == b.2),
                "{cell:?} at {} ms: the law moved the body or changed its size, or drew a \
                 frame it left: column {} box {:?} (about its centre {:?}), without the law \
                 column {} box {:?}",
                a.0,
                a.1,
                a.2,
                a.3,
                b.1,
                b.2
            );
        }
        assert!(
            !onto_letters(&old).is_empty(),
            "negative control ({cell:?}): stretched about its centre, the coil never laid \
             its footprint on the line's letters"
        );
    }
}

/// THE CAT KEEPS A WIDE GLYPH'S STEP AHEAD ([`PetBrain::wide_step`]), on the trail gallery's
/// `cjk` geometry ([`wide_burst`]), the cat at rest beside the prompt when the first key
/// comes: with a present 30 ms after each key, as the gallery's instance renders, the
/// body's drawn box, unshaved, never meets the caret's cell from the first wide key on, nor
/// after the last key's own present (the gallery's `t0`); with one every 16 ms, it meets it
/// no later than [`WIDE_CATCH_UP_MS`] after the last key. The cat gets going on foot with
/// the first keys (no flight at a typing run's start, r24 K7-1: looks8's first keys' spring
/// put it ahead of them in a hop, and its lead kept every present off the caret), so two
/// cells a key run into its box as it runs ([`WIDE_CATCH_UP_MS`]). Negative control
/// ([`OldPath::NarrowStation`] alone, looks9 round 1's station): at the caret's heel, the
/// caret's wide steps land on its box at the gallery's presents, and past the catch-up.
#[test]
fn a_cat_running_beside_wide_glyphs_keeps_off_the_caret() {
    let met = wide_burst(&[]);
    assert_eq!(
        met.late, 0,
        "{} presents more than {WIDE_CATCH_UP_MS} ms after the last key lay the body's box \
         on the caret's cell ({met:?})",
        met.late
    );
    let sparse = wide_burst_framed(&[], true, 200);
    assert_eq!(
        (sparse.after, sparse.wide),
        (0, 0),
        "presents after the last key, and from the first wide key, a present a key, lay the \
         body's box on the caret's cell ({sparse:?})"
    );
    let old = wide_burst_framed(&[OldPath::NarrowStation], true, 200);
    let old_late = wide_burst(&[OldPath::NarrowStation]);
    assert!(
        old.wide > 0 && old_late.late > 0,
        "negative control: the narrow station's box never met the caret's cell ({old:?}, \
         {old_late:?})"
    );
}

/// THE CAT RUNS DOWN WIDE WRITING AT ITS FASTEST ([`PetBrain::wide_chase`]), on the trail
/// gallery's `cjk` geometry ([`wide_burst_keyed`]), the cat just born asleep beside the
/// prompt, as the gallery's is: from the first wide glyph on, with a present a key (30 ms
/// after it, or at it and 10 ms after it), the body's drawn box, unshaved, never meets the
/// caret's cell; and with one every 16 ms, with `echo` typed at the gallery's pace or a
/// person's (130 ms a key) before the wide glyphs, it meets it no later than
/// [`WIDE_CATCH_UP_MS`] after the last key: the cat that gets going on foot with the first
/// keys (r24 K7-1) runs the wide writing down. Negative control ([`OldPath::WideChase`] with
/// [`OldPath::StillUnderWriting`], which ends the same landings for the run; its own scenes
/// are [`a_still_pet_runs_off_the_writing_that_reaches_it`]'s): looks9 round 1's run, at its
/// old pace, lays the cat's tail on the caret's cell after the wide keys. The station's own
/// control is [`a_cat_running_beside_wide_glyphs_keeps_off_the_caret`]'s.
#[test]
fn the_cat_runs_down_wide_writing_at_its_fastest() {
    let gallery: Vec<u64> = (0..9).map(|k| 60 * k).collect();
    let by_hand: Vec<u64> = vec![0, 130, 260, 390, 520, 580, 640, 700, 760];
    let keyed = WidePresents::AfterEachKey {
        delay: 10,
        own: true,
    };
    let run = |old: &[OldPath]| {
        (
            wide_burst_framed(old, true, 1),
            wide_burst_keyed(old, &gallery, keyed, 1, PROMPT_CELL),
            wide_burst_framed(old, false, 1),
            wide_burst_keyed(old, &by_hand, WidePresents::Every16, 1, PROMPT_CELL),
        )
    };
    let (sparse, at_keys, every, hand) = run(&[]);
    assert_eq!(
        (sparse.wide, at_keys.wide),
        (0, 0),
        "presents from the first wide key, a present a key, lay the body's box on the \
         caret's cell ({sparse:?}, {at_keys:?})"
    );
    assert_eq!(
        (every.late, hand.late),
        (0, 0),
        "presents more than {WIDE_CATCH_UP_MS} ms after the last key lay the body's box on \
         the caret's cell ({every:?}, {hand:?})"
    );
    let (_, old_keys, _, old_hand) = run(&[OldPath::WideChase, OldPath::StillUnderWriting]);
    assert!(
        old_keys.wide > 0 && old_hand.late > 0,
        "negative control: at its old pace, the cat's box never met the caret's cell \
         ({old_keys:?}, {old_hand:?})"
    );
}

/// Where the cat rests before [`a_run_off_a_held_landing_sets_out_with_the_writing`]'s
/// writing: beside a prompt on the row under the screen's, so the first key's caret move
/// brings it up a row, and it comes down beside the prompt with the writing under way.
const UNDER_THE_PROMPT: (u16, u16) = (5, 2);

/// THE WRITING NEVER RUNS UNDER A STILL CAT ([`PetBrain::writing_reaches`],
/// [`PetBrain::landing_fresh`]) on a landing late for the writing, on the trail gallery's
/// `cjk` geometry with its keys as a headless gallery instance admitted them (45 to 65 ms
/// apart): the cat at rest under the prompt ([`UNDER_THE_PROMPT`]) hops up to it as the
/// first key comes and lands with the writing under way, holds its landing until it is no
/// longer fresh and runs on with the writing: with a present at each key and 10 or 20 ms
/// after it, from the first wide glyph on no present lays its box on the caret's cell or
/// the cells written left of it; with one every 16 ms, none past [`WIDE_CATCH_UP_MS`] after
/// the last key. (The cat at rest beside the prompt gets going on foot, no landing there:
/// looks8's first keys' spring, whose landing the gallery's scene held, goes in r24 K7-1.)
/// Negative control ([`OldPath::StillUnderWriting`] alone): the run off the landing plants
/// a stand, the stand's latch holds the body a stride, and the caret steps into it (the
/// gallery's `cjk` `t0` in `flat-rainbow` and `rainbow-kitty-pet` on Solarized Dark, review
/// 9 round 3; the dog's case is `a_still_pet_runs_off_the_writing_that_reaches_it`).
#[test]
fn a_run_off_a_held_landing_sets_out_with_the_writing() {
    let keys: Vec<u64> = [0u64, 45, 45, 47, 47, 50, 65, 62, 53]
        .iter()
        .scan(0u64, |at, gap| {
            *at += gap;
            Some(*at)
        })
        .collect();
    let at = |delay| WidePresents::AfterEachKey { delay, own: true };
    for (idle, delay) in [(60, 10), (200, 10), (60, 20), (200, 20)] {
        let met = wide_burst_keyed(&[], &keys, at(delay), idle, UNDER_THE_PROMPT);
        assert_eq!(
            met.on_writing, 0,
            "{} presents from the first wide key ({idle} idle presents, a present {delay} ms \
             after each key) lay the body's box on the caret's cell or the writing ({met:?})",
            met.on_writing
        );
    }
    let every = wide_burst_keyed(&[], &keys, WidePresents::Every16, 200, UNDER_THE_PROMPT);
    assert_eq!(
        every.late, 0,
        "presents more than {WIDE_CATCH_UP_MS} ms after the last key lay the body's box on \
         the caret's cell ({every:?})"
    );
    let old = wide_burst_keyed(
        &[OldPath::StillUnderWriting],
        &keys,
        at(10),
        200,
        UNDER_THE_PROMPT,
    );
    let old_every = wide_burst_keyed(
        &[OldPath::StillUnderWriting],
        &keys,
        WidePresents::Every16,
        200,
        UNDER_THE_PROMPT,
    );
    assert!(
        old.on_writing > 0 && old.wide > 0 && old_every.late > 0,
        "negative control: held still under the writing, the cat's box never met the \
         caret's cell ({old:?}, {old_every:?})"
    );
}

/// The `cjk` rig of [`wide_burst_framed`] typing `widths` (each key's step: 1 for an ASCII
/// letter, 2 for a wide glyph) from the prompt on row 4 at 60 ms a key, a present every
/// 16 ms, the cat bounding to the writing from where it was born; `paired`: the key that
/// echoes in one present with the one after it, as two keys a program writes together do.
/// Returns the cat's mean lead over the caret (its column less the caret's, cells) over
/// the presents from the key `from` on to the last key, and the largest wide lead it stood
/// by there ([`PetBrain::wide_step`]).
fn wide_lead(widths: &[u16], paired: Option<usize>, from: usize, old: &[OldPath]) -> (f32, f32) {
    use crate::pet_world::{PetPane, PetWorldFacts};
    use aterm_core::terminal::Terminal;
    let (rows, cols, cw, ch) = (14u16, 72u16, 14u16, 26u16);
    let sense_at = |now: Instant, caret: (u16, u16)| PetSense {
        caret_drawn: true,
        now,
        caret: Some(caret),
        wrapped: false,
        rows,
        cols,
        cell_w: cw,
        cell_h: ch,
        reduced_motion: false,
        output_burst: false,
        pointer: None,
    };
    let line: Vec<char> = widths
        .iter()
        .enumerate()
        .map(|(i, &w)| match (w, i % 2) {
            (2, 0) => '你',
            (2, _) => '好',
            _ => char::from(b'a' + (i % 26) as u8),
        })
        .collect();
    // Key `i` lands at this ms: 60 ms apart, a paired key with the one before it.
    let lands: Vec<u64> = (0..widths.len())
        .scan(0u64, |at, i| {
            if i > 0 && paired != Some(i - 1) {
                *at += 60;
            }
            Some(*at)
        })
        .collect();
    let start = Instant::now();
    let mut pet = PetBrain::default();
    for &law in old {
        pet.set_old_path(law, true);
    }
    let mut term = Terminal::new(rows, cols);
    let mut observe = |pet: &mut PetBrain, typed: usize| {
        let text: String = line.iter().take(typed).collect();
        term.process(format!("\x1b[2J\x1b[5;1H$ {text}").as_bytes());
        let input = term.cell_frame(usize::from(rows), usize::from(cols));
        let facts = PetWorldFacts::read(&term, 7);
        pet.observe_console(&input, &facts, PetPane::full(&input));
        pet.set_console_presentable(true);
    };
    let mut spans = vec![(0u16, 0u16); usize::from(rows)];
    spans[4] = (0, 1);
    let mut t = start + Duration::from_millis(16);
    observe(&mut pet, 0);
    pet.sense_ink(0, &spans, None);
    let _ = pet.tick(sense_at(t, (4, 2)));
    let last = *lands.last().expect("keys");
    let (mut typed, mut ms) = (0usize, 0u64);
    let (mut sum, mut n, mut most) = (0.0f32, 0usize, 0.0f32);
    while ms <= last {
        t += Duration::from_millis(16);
        ms += 16;
        while typed < widths.len() && lands[typed] <= ms {
            typed += 1;
        }
        let col = 2 + widths.iter().take(typed).sum::<u16>();
        spans[4] = (0, col);
        observe(&mut pet, typed);
        pet.sense_ink(0, &spans, None);
        let _ = pet.tick(sense_at(t, (4, col)));
        if typed > from {
            sum += pet.col - f32::from(col);
            n += 1;
            most = most.max(pet.wide_step());
        }
    }
    (sum / n.max(1) as f32, most)
}

/// ONE WIDE STEP HOLDS NO WIDE STATION ([`PetBrain::wide_run`]): on the `cjk` rig a line of
/// ASCII keys with one wide glyph in it (`echo ab你cdef…`), or with two keys a program
/// echoes in one present (`echo ab~Ycdef…`), keeps the station the same line typed in ASCII
/// keeps from six keys past that step on: the wide lead is not taken, and the cat's mean
/// lead over the caret is the ASCII line's within half a cell, while a line of wide glyphs
/// takes the lead. Negative control ([`OldPath::WideByLargestStep`], looks9 round 2's read
/// off the run's largest step): the one step holds the wide station for the rest of the run,
/// the cat a cell and more further ahead (looks9's review read five cells against three).
#[test]
fn one_wide_step_holds_no_wide_station() {
    let ascii = [1u16; 22];
    let mut lone = ascii;
    lone[7] = 2;
    let from = 7 + 6;
    let (plain, plain_lead) = wide_lead(&ascii, None, from, &[]);
    assert_eq!(plain_lead, 0.0, "fixture: an ASCII line takes no wide lead");
    let wide = [2u16; 12];
    let (_, wide_lead_taken) = wide_lead(&wide, None, 4, &[]);
    assert!(
        wide_lead_taken > 0.0,
        "a line of wide glyphs takes no wide lead"
    );
    for (label, paired) in [
        ("a lone wide glyph", None),
        ("two keys in one present", Some(7)),
    ] {
        let widths = if paired.is_some() { ascii } else { lone };
        let (mean, lead) = wide_lead(&widths, paired, from, &[]);
        assert_eq!(lead, 0.0, "{label}: the wide lead is taken past it");
        assert!(
            (mean - plain).abs() <= 0.5,
            "{label}: the cat leads the caret by {mean:.2} cells, the ASCII line's {plain:.2}"
        );
        let (old_mean, _) = wide_lead(&widths, paired, from, &[OldPath::WideByLargestStep]);
        assert!(
            old_mean - plain >= 1.0,
            "negative control: {label} read off the run's largest step leads by \
             {old_mean:.2} cells, the ASCII line's {plain:.2}"
        );
    }
}

/// A pet held still just ahead of a typing caret: how it is held.
#[derive(Clone, Copy, Debug)]
enum Held {
    /// A landing's recovery ([`LAND_DUR`]).
    Landing,
    /// A big landing's recovery ([`BIG_LAND_DUR`]) with a commit's stare after it.
    StaringLanding,
    /// A commit's stare on a stand ([`COMMIT_STARE`]) that has stood out its stride.
    Stare,
    /// A plain stand, nothing holding it: the floor the held poses are judged by.
    Stand,
}

/// Six keys typed `step` cells a key 60 ms apart set the writing's rhythm; the pet is then
/// put down `lead` cells past the cell after the caret, at rest and held as `held` says,
/// and five more keys come on the same rhythm, each noted as text as a host notes it.
/// Ticked every `every` ms for 900 ms: the presents whose drawn body, unshaved, meets the
/// caret's cell.
fn held_ahead_of_writing(old: &[OldPath], held: Held, lead: f32, step: u16, every: u64) -> usize {
    let start = Instant::now();
    let mut pet = PetBrain::default();
    for &law in old {
        pet.set_old_path(law, true);
    }
    let row = 4u16;
    let mut t = awake(&mut pet, start, row, 10);
    let mut cc = 12u16;
    for _ in 0..6 {
        let until = t + Duration::from_millis(60);
        while t < until {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, Some((row, cc))));
        }
        cc += step;
        pet.note_console_input(t, PetInputKind::Text);
        let _ = pet.tick(sense(t, Some((row, cc))));
    }
    pet.flight = None;
    pet.col = f32::from(cc) + 1.0 + lead;
    pet.row = f32::from(row);
    pet.speed = 0.0;
    pet.vhat = 0.0;
    match held {
        Held::Landing => {
            pet.set_action(PetAction::Land);
            pet.land_t = LAND_DUR;
        }
        Held::StaringLanding => {
            pet.set_action(PetAction::Land);
            pet.land_t = BIG_LAND_DUR;
            pet.commit_t = COMMIT_STARE;
        }
        Held::Stare => {
            pet.set_action(PetAction::Stand);
            pet.commit_t = COMMIT_STARE;
            // On glass a stride already: a stand younger than that stands it out before
            // any run, as every stand beside a stride does ([`STAND_MIN`]).
            pet.stand_from = Some(pet.clock - f64::from(STAND_MIN));
        }
        Held::Stand => pet.set_action(PetAction::Stand),
    }
    let (mut meets, mut ms, mut next_key, mut keys) = (0usize, 0u64, 60u64, 0);
    while ms < 900 {
        ms += every;
        t += Duration::from_millis(every);
        while keys < 5 && next_key <= ms {
            cc += step;
            keys += 1;
            next_key += 60;
            pet.note_console_input(t, PetInputKind::Text);
        }
        let f = pet.tick(sense(t, Some((row, cc))));
        meets += usize::from(f.body_px(10, 20, 100, 30).is_some_and(|(x0, x1, y0, y1)| {
            let (cx0, cx1) = (i32::from(cc) * 10, i32::from(cc + 1) * 10);
            let (cy0, cy1) = (i32::from(row) * 20, i32::from(row + 1) * 20);
            x0 < cx1 && x1 > cx0 && y0 < cy1 && y1 > cy0
        }));
    }
    meets
}

/// THE WRITING NEVER RUNS UNDER A STILL CAT ([`PetBrain::writing_reaches`]): a pet held
/// still just ahead of a typing caret (a landing's recovery, a commit's stare after a big
/// landing or on a stand that has stood out its stride; a younger stand stands it out
/// first, as every stand beside a stride does) runs off as the writing comes, so that, typed into one cell a
/// key or a wide glyph's two, at every lead and every present (16, 30 and 60 ms apart), its
/// drawn body never meets the caret's cell at one cell a key, nor at two from a lead of a
/// cell and a half; closer, where the wide step's next cell lies under a body put down
/// there, no more often than a plain stand's from the same spot. The trail gallery's `cjk`
/// scene, the cat hopping up to the prompt from the row under it as the first key comes
/// ([`UNDER_THE_PROMPT`], every 16 ms), keeps the body off the caret's cell from
/// [`WIDE_CATCH_UP_MS`] after the last key. (The gallery's own late landing, looks10 round
/// 4's capture, came down off looks8's first keys' spring, which no longer leaves: r24
/// K7-1.) Negative control ([`OldPath::StillUnderWriting`], looks10 rounds 3 and 4's dog):
/// each held pose stands out its hold with the writing running under it, and the gallery
/// scene lays the box on the caret's cell, as the dog's lay at `cjk` `t0`.
#[test]
fn a_still_pet_runs_off_the_writing_that_reaches_it() {
    const LEADS: [f32; 7] = [0.0, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0];
    const HELD: [Held; 3] = [Held::Landing, Held::StaringLanding, Held::Stare];
    for step in [1u16, 2] {
        for every in [16u64, 30, 60] {
            for lead in LEADS {
                let floor = if step == 1 || lead >= 1.5 {
                    0
                } else {
                    held_ahead_of_writing(&[], Held::Stand, lead, step, every)
                };
                for held in HELD {
                    let on = held_ahead_of_writing(&[], held, lead, step, every);
                    assert!(
                        on <= floor,
                        "{held:?} {lead} cells ahead, {step} cells a key, every {every} ms: \
                         {on} presents lay the body on the caret's cell, a plain stand's \
                         {floor}"
                    );
                }
            }
        }
    }
    let sixty: Vec<u64> = (0..9).map(|k| 60 * k).collect();
    let hop_in = |old: &[OldPath]| {
        wide_burst_keyed(old, &sixty, WidePresents::Every16, 200, UNDER_THE_PROMPT)
    };
    let gallery = hop_in(&[]);
    assert_eq!(
        gallery.late, 0,
        "presents more than {WIDE_CATCH_UP_MS} ms after the last key lay the body's box on \
         the caret's cell ({gallery:?})"
    );
    for held in HELD {
        let old = held_ahead_of_writing(&[OldPath::StillUnderWriting], held, 2.0, 1, 16);
        assert!(
            old > 0,
            "negative control: {held:?} stood out its hold clear of the writing"
        );
    }
    let old = held_ahead_of_writing(&[OldPath::StillUnderWriting], Held::Stare, 2.0, 2, 16);
    assert!(
        old > 0,
        "negative control: a stare stood out clear of the wide writing"
    );
    let old = hop_in(&[OldPath::StillUnderWriting]);
    assert!(
        old.late > 0,
        "negative control: the gallery's scene kept the body's box off the caret's cell \
         ({old:?})"
    );
}
