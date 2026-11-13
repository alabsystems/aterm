// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::host::FrameGeom;
use aterm_time::Duration;
use std::collections::BTreeMap;

const BLACK_FALLBACK: ContrastFallback = ContrastFallback {
    bg: 0,
    cursor: 0,
    accent: 0,
};

const GRID: FrameGeom = FrameGeom {
    rows: 24,
    cols: 80,
    cell_w: 10,
    cell_h: 20,
    origin_px: (0, 0),
};

const fn host_at(now: Instant) -> HostFrameInput {
    HostFrameInput::new(now, GRID)
}

const fn facts_with(caret: Option<(u16, u16)>) -> TerminalFacts {
    TerminalFacts {
        session: 1,
        caret,
        cursor_visible: caret.is_some(),
        frame_unfinished: false,
        sync_end_seq: 0,
        sync_end_caret: None,
        display_offset: 0,
        live_viewport: true,
        content_seq: 0,
        wrap_serial: 0,
        scrolled: false,
        shell_executing: false,
        cmd_done: None,
        alt_screen: false,
    }
}

const fn pet_glow() -> PetOwnership {
    PetOwnership { on: true }
}

const fn pet_facts<'a>(facts: &'a TerminalFacts, host: &'a HostFrameInput) -> PetFacts<'a> {
    PetFacts::new(facts, host, pet_glow())
}

/// `new` is the literal a test host wrote, and each setter moves only its own field: the
/// constructor every host and harness builds a pet frame's facts through (§15.1 P0.1).
#[test]
fn pet_facts_built_by_new_are_the_literal() {
    let facts = facts_with(Some((3, 4)));
    let host = host_at(Instant::now());
    let plain = PetFacts {
        facts: &facts,
        host: &host,
        pet: pet_glow(),
        sing: SingFacts::EMPTY,
        focused: true,
        obscured: false,
        pane: None,
        room: None,
        caret_path: [None; CARET_PATH_LEN],
    };
    let built = PetFacts::new(&facts, &host, pet_glow());
    assert!(std::ptr::eq(built.facts, plain.facts) && std::ptr::eq(built.host, plain.host));
    assert_eq!(format!("{built:?}"), format!("{plain:?}"));
    let sing = SingFacts {
        drive: 0.5,
        flying_alpha: 9,
    };
    let every = PetFacts {
        facts: &facts,
        host: &host,
        pet: PetOwnership::NONE,
        sing,
        focused: false,
        obscured: true,
        pane: Some((7, (8, 9))),
        room: Some(RoomFacts::EMPTY),
        caret_path: [None; CARET_PATH_LEN],
    };
    let built = PetFacts::new(&facts, &host, PetOwnership::NONE)
        .with_sing(sing)
        .with_focused(false)
        .with_obscured(true)
        .with_pane(Some((7, (8, 9))))
        .with_room(Some(RoomFacts::EMPTY));
    assert_eq!(format!("{built:?}"), format!("{every:?}"));
    assert_ne!(
        format!("{plain:?}"),
        format!("{every:?}"),
        "control: the setters moved the facts off their defaults"
    );
}

const fn an_enabled_owner() -> CompanionOwner {
    let mut owner = CompanionOwner::new();
    let look = KittyLook::for_launch(0x5EED);
    owner.pair = (look.coat, look.iris);
    owner.arrival = PetArrival::Quiet;
    owner.rung = CompanionRung::Launch;
    owner.enabled = true;
    owner
}

fn frame(
    owner: &mut CompanionOwner,
    decos: &mut WordDecorations,
    now: Instant,
    caret: Option<(u16, u16)>,
) -> CompanionFrame {
    let facts = facts_with(caret);
    let host = host_at(now);
    owner.sense(pet_facts(&facts, &host), decos)
}

fn materialize(owner: &mut CompanionOwner, decos: &mut WordDecorations) -> Instant {
    let mut now = Instant::now();
    let mut on_glass = false;
    for step in 0u16..40 {
        now += Duration::from_millis(16);
        let t = frame(owner, decos, now, Some((4, 10 + step % 8)));
        on_glass |= t.on_glass;
    }
    assert!(on_glass, "fixture: the resident reached the glass");
    assert!(owner.alpha() > 0 && owner.hit_rect().is_some());
    assert!(owner.needs_frames(), "fixture: it is claiming the lane");
    now
}

fn settle(owner: &mut CompanionOwner, decos: &mut WordDecorations, mut now: Instant) -> Instant {
    for _ in 0..2400 {
        now += Duration::from_millis(50);
        let _ = frame(owner, decos, now, Some((4, 12)));
        if !owner.needs_frames() {
            return now;
        }
    }
    panic!("fixture: the resident never settled");
}

#[test]
fn the_brain_ticks_even_when_undrawable_so_the_lane_is_released() {
    let mut decos = WordDecorations::default();
    let mut owner = an_enabled_owner();
    let mut now = materialize(&mut owner, &mut decos);

    let mut released = false;
    for _ in 0..400 {
        now += Duration::from_millis(50);
        let facts = facts_with(Some((4, 12)));
        let host = host_at(now);
        let t = owner.sense(
            PetFacts {
                focused: false,
                ..pet_facts(&facts, &host)
            },
            &mut decos,
        );
        assert!(!t.on_glass, "an unfocused surface draws no pet");
        assert_eq!(owner.alpha(), 0);
        assert_eq!(owner.hit_rect(), None);
        if !owner.needs_frames() {
            released = true;
            break;
        }
    }
    assert!(
        released,
        "hidden-caret ticks let the resident release the lane"
    );

    let mut frozen = an_enabled_owner();
    let _ = materialize(&mut frozen, &mut decos);
    assert!(
        frozen.needs_frames(),
        "a brain that is not ticked keeps claiming the lane — the latch the law prevents"
    );
}

#[test]
fn a_press_inside_the_body_plus_slop_pets_and_outside_passes() {
    let mut decos = WordDecorations::default();
    let mut owner = an_enabled_owner();
    let now = materialize(&mut owner, &mut decos);
    let (x0, x1, y0, y1) = owner.hit_rect().expect("a drawn body");

    let inside = ((x0 + x1) as f32 * 0.5, (y0 + y1) as f32 * 0.5);
    assert_eq!(owner.press(now, inside), PressOutcome::Pet);
    assert_eq!(owner.brain().pending_pets(), 1, "the latch moved");
    let grace = (x0 as f32 - PET_HIT_SLOP_PX as f32 + 0.5, y0 as f32);
    assert_eq!(
        owner.press(now, grace),
        PressOutcome::Pet,
        "the slop band pets too"
    );
    assert_eq!(owner.brain().pending_pets(), 2);

    let outside = (x1 as f32 + PET_HIT_SLOP_PX as f32 + 1.0, inside.1);
    assert_eq!(owner.press(now, outside), PressOutcome::Pass);
    assert_eq!(owner.brain().pending_pets(), 2, "a miss latches nothing");

    owner.withdraw_hit_rect();
    assert_eq!(owner.hit_rect(), None);
    assert!(owner.brain().is_active(), "the brain keeps its frame");
    assert_eq!(
        owner.press(now, inside),
        PressOutcome::Pass,
        "no body, no pet"
    );
    let mut host = host_at(now + Duration::from_millis(16));
    host.pointer_px = Some(inside);
    let facts = facts_with(Some((4, 12)));
    let t = prepared(&mut owner, &mut decos, &facts, &host, false);
    assert_eq!(t.sense.pointer, None, "a withdrawn body is untouchable");
}

#[test]
fn only_a_presented_resident_can_receive_pointer_strokes() {
    for (sing_drive, shed, focused, visible) in [
        (0.0, 1.0, true, true),
        (0.2, 1.0, true, true),
        (0.0, 1.0, false, false),
        (0.2, 0.0, false, false),
    ] {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        let now = materialize(&mut owner, &mut decos);
        let mut now = settle(&mut owner, &mut decos, now);
        for _ in 0..20 {
            now += Duration::from_millis(50);
            frame(&mut owner, &mut decos, now, Some((4, 12)));
        }
        let (x0, x1, y0, y1) = owner.hit_rect().expect("fixture: a drawn resident");
        let width = (x1 - x0) as f32;
        let before = owner.brain().content();
        let facts = facts_with(Some((4, 12)));
        let mut answered = false;
        for step in 0..=12 {
            now += Duration::from_millis(25);
            let mut host = host_at(now);
            host.shed_envelope = shed;
            host.pointer_px = Some((
                x0 as f32 + width * (0.20 + 0.60 * step as f32 / 12.0),
                (y0 + y1) as f32 * 0.5,
            ));
            let result = owner.sense(
                PetFacts {
                    sing: SingFacts {
                        drive: sing_drive,
                        ..SingFacts::default()
                    },
                    focused,
                    ..pet_facts(&facts, &host)
                },
                &mut decos,
            );
            assert_eq!(result.on_glass, visible);
            answered |= owner.brain().pending_pets() > 0
                || owner.brain().action() == crate::kitty_pet::PetAction::Purr;
        }
        if visible {
            assert!(answered, "control: the same sweep strokes a drawn pet");
            assert!(owner.brain().content() > before + 0.05);
        } else {
            assert!(
                !answered,
                "a hidden pet cannot be touched: sing={sing_drive}, shed={shed}"
            );
            assert!(
                owner.brain().content() <= before,
                "a hidden pet earned affection: sing={sing_drive}, shed={shed}"
            );
        }
    }
}

#[test]
fn the_pointer_is_value_shadowed() {
    let mut owner = CompanionOwner::default();
    assert!(!owner.set_pointer(None), "None over None is no change");
    assert!(owner.set_pointer(Some((12.0, 34.0))));
    assert!(
        !owner.set_pointer(Some((12.0, 34.0))),
        "a still pointer costs no render"
    );
    assert!(owner.set_pointer(Some((12.5, 34.0))));
    assert!(
        !owner.set_pointer(Some((f32::NAN, 1.0))),
        "poison is dropped"
    );
    assert!(
        !owner.set_pointer(Some((12.5, 34.0))),
        "…and left the value alone"
    );
    assert!(owner.set_pointer(None), "leaving the surface is a change");
    assert!(!owner.set_pointer(None));
}

#[test]
fn a_pointer_over_the_grid_is_sensed_and_one_off_the_grid_is_not() {
    let mut decos = WordDecorations::default();
    let mut owner = an_enabled_owner();
    let now = materialize(&mut owner, &mut decos);
    let mut now = settle(&mut owner, &mut decos, now);

    for k in 0..30 {
        now += Duration::from_millis(16);
        let dx = if k % 10 < 5 { 2.0 } else { -2.0 };
        let _ = owner.set_pointer(Some((-500.0 + dx * k as f32, -300.0)));
        let _ = frame(&mut owner, &mut decos, now, Some((4, 12)));
    }
    assert!(
        !owner.needs_frames(),
        "off the grid the pointer does not exist for the pet"
    );

    let mut px = 400.0f32;
    for k in 0..30 {
        now += Duration::from_millis(16);
        px += if k % 10 < 5 { 2.0 } else { -2.0 };
        let _ = owner.set_pointer(Some((px, 4.0 * 20.0 + 10.0)));
        let _ = frame(&mut owner, &mut decos, now, Some((4, 12)));
    }
    assert!(
        owner.needs_frames(),
        "a wandering pointer over the grid is sensed"
    );
}

#[test]
fn a_static_capture_materialises_the_first_still() {
    let mut decos = WordDecorations::default();
    let now = Instant::now();
    let facts = facts_with(Some((2, 2)));

    let mut capture = an_enabled_owner();
    let mut host = host_at(now);
    host.capture = CaptureMode::StaticCapture;
    let still = capture.sense(pet_facts(&facts, &host), &mut decos);
    assert_eq!(still.pet.alpha, 255, "the still shows the resident");
    assert!(still.on_glass && still.body_px.is_some());

    let mut present = an_enabled_owner();
    let host = host_at(now);
    let first = present.sense(pet_facts(&facts, &host), &mut decos);
    assert_eq!(first.pet.alpha, 0, "negative control: a present fades in");
    assert!(present.needs_frames(), "…and claims the lane to do so");
}

#[test]
fn emit_draws_one_body_wearing_the_latched_pair_and_syncs_the_verdict() {
    let mut decos = WordDecorations::default();
    let mut owner = an_enabled_owner();
    let now = materialize(&mut owner, &mut decos);
    let t = frame(
        &mut owner,
        &mut decos,
        now + Duration::from_millis(16),
        Some((4, 12)),
    );
    assert!(t.on_glass);
    assert_eq!(t.duty, CompanionDuty::Pet);
    assert!(
        t.companion
            .is_some_and(|c| c.body_px == t.body_px && !c.guards_caret),
        "the yield box is the pet's own body"
    );
    assert_ne!(t.fp, 0);

    let mut free = Vec::new();
    let theme = ContrastFallback {
        bg: 0x0010_1010,
        cursor: 0x00F0_F0F0,
        accent: 0x0040_80FF,
    };
    let (fp, outcome) = owner.emit(&t, &[], theme, &mut decos, &mut free);
    assert_ne!(fp, 0, "a drawn body folds into the fingerprint");
    assert!(!free.is_empty(), "the body landed as free sprites");
    let launch = KittyLook::for_launch(0x5EED);
    assert_eq!(outcome.worn, (launch.coat, launch.iris));
    assert!(!outcome.parked);
    assert!(
        !owner.commit_hello_due(true, outcome),
        "a launch rung spends no hello"
    );

    let fav = KittyLook {
        coat: (launch.coat + 5) % 16,
        iris: (launch.iris + 3) % 8,
        ..KittyLook::default()
    };
    owner.set_look(
        (fav.coat, fav.iris),
        PetArrival::Quiet,
        CompanionRung::Favourite,
    );
    let t = frame(
        &mut owner,
        &mut decos,
        now + Duration::from_millis(32),
        Some((4, 12)),
    );
    let mut free = Vec::new();
    let (_, parked) = owner.emit(&t, &[], theme, &mut decos, &mut free);
    assert!(parked.parked, "the favourite is parked for the handoff");
    assert_eq!(
        parked.worn, outcome.worn,
        "…while the old coat is still worn"
    );
    assert!(
        !owner.commit_hello_due(true, parked),
        "a favourite is always quiet"
    );

    owner.retire();
    let idle = frame(
        &mut owner,
        &mut decos,
        now + Duration::from_millis(48),
        None,
    );
    assert!(!idle.on_glass);
    let mut free = Vec::new();
    let (fp, _) = owner.emit(&idle, &[], theme, &mut decos, &mut free);
    assert_eq!(fp, 0);
    assert!(free.is_empty());
}

#[test]
fn the_hello_seam_commits_only_a_performed_ceremony_on_a_present() {
    use crate::kitty_registry::{SUFFICIENT_DIFFERENCE, coat_distance};
    let mut decos = WordDecorations::default();
    let mut owner = an_enabled_owner();
    let now = materialize(&mut owner, &mut decos);
    let t = frame(
        &mut owner,
        &mut decos,
        now + Duration::from_millis(16),
        Some((4, 12)),
    );
    let theme = ContrastFallback {
        bg: 0,
        cursor: 0x00FF_FFFF,
        accent: 0,
    };
    let (_, first) = owner.emit(&t, &[], theme, &mut decos, &mut Vec::new());
    let worn = first.worn;
    let far = (0u8..16)
        .find(|&c| coat_distance(worn.0, c) >= SUFFICIENT_DIFFERENCE)
        .expect("a sufficiently different coat exists");

    owner.set_look((far, worn.1), PetArrival::Ceremony, CompanionRung::Program);
    let t = frame(
        &mut owner,
        &mut decos,
        now + Duration::from_millis(32),
        Some((4, 12)),
    );
    let (_, outcome) = owner.emit(&t, &[], theme, &mut decos, &mut Vec::new());
    assert_eq!(outcome.worn, worn, "the ceremony is parked, not yet worn");
    assert!(
        owner.commit_hello_due(true, outcome),
        "a drawn present spends the hello"
    );
    assert!(
        !owner.commit_hello_due(false, outcome),
        "a capture never does"
    );
    let off = CompanionFrame {
        on_glass: false,
        duty: CompanionDuty::Idle,
        body_px: None,
        companion: None,
        ..t
    };
    let (fp, off_outcome) = owner.emit(&off, &[], theme, &mut decos, &mut Vec::new());
    assert_eq!(fp, 0, "nothing drawn");
    assert!(
        !owner.commit_hello_due(true, off_outcome),
        "an off-glass frame performed nothing, so it commits nothing"
    );

    owner.set_look((far, worn.1), PetArrival::Quiet, CompanionRung::Program);
    let t = frame(
        &mut owner,
        &mut decos,
        now + Duration::from_millis(48),
        Some((4, 12)),
    );
    let (_, again) = owner.emit(&t, &[], theme, &mut decos, &mut Vec::new());
    assert!(!owner.commit_hello_due(true, again));
}

fn prepared(
    owner: &mut CompanionOwner,
    decos: &mut WordDecorations,
    facts: &TerminalFacts,
    host: &HostFrameInput,
    obscured: bool,
) -> PetTick {
    owner.prepare(
        PetFacts {
            obscured,
            ..pet_facts(facts, host)
        },
        decos,
    )
}

#[test]
fn a_torn_frames_hidden_cursor_reads_as_the_last_finished_caret() {
    let mut decos = WordDecorations::default();
    let mut owner = an_enabled_owner();
    let now = materialize(&mut owner, &mut decos);
    let at = |ms: u64| host_at(now + Duration::from_millis(ms));
    let finished = facts_with(Some((20, 7)));
    let t = prepared(&mut owner, &mut decos, &finished, &at(16), false);
    assert_eq!((t.sense.caret, t.sense.caret_drawn), (Some((20, 7)), true));
    let _ = owner.tick(t);

    let mut torn = facts_with(Some((2, 0)));
    torn.cursor_visible = false;
    torn.frame_unfinished = true;
    torn.wrap_serial = 1;
    let t = prepared(&mut owner, &mut decos, &torn, &at(32), false);
    assert_eq!(t.sense.caret, Some((20, 7)), "the write cursor is no caret");
    assert!(t.sense.caret_drawn, "…and the finished frame's paint rides");
    assert!(!t.sense.wrapped, "a torn frame leaves the wrap fact unread");
    assert_eq!(t.painted_caret, None, "nothing is painted on this frame");
    let _ = owner.tick(t);

    let mut closed = torn;
    closed.caret = Some((20, 8));
    closed.cursor_visible = true;
    closed.frame_unfinished = false;
    let t = prepared(&mut owner, &mut decos, &closed, &at(48), false);
    assert_eq!(t.sense.caret, Some((20, 8)));
    assert!(t.sense.wrapped, "the wrap is read with the finished frame");
    let _ = owner.tick(t);

    let mut hidden = closed;
    hidden.cursor_visible = false;
    let t = prepared(&mut owner, &mut decos, &hidden, &at(64), false);
    assert_eq!(
        (t.sense.caret, t.sense.caret_drawn),
        (Some((20, 8)), false),
        "a finished frame's hidden caret still travels, unpainted"
    );
    assert_eq!(
        t.painted_caret, None,
        "no stale word claim at a hidden caret"
    );
    let frame = owner.tick(t);
    assert!(
        frame.on_glass,
        "the resident keeps its body through the hide"
    );
    assert!(
        frame.companion.is_some_and(|c| c.cell.is_none()),
        "the yield box is the body alone"
    );

    let mut other = torn;
    other.session = 2;
    let t = prepared(&mut owner, &mut decos, &other, &at(80), false);
    assert_eq!(t.sense.caret, None, "no finished caret in this session");
}

#[test]
fn the_escort_caret_is_the_finished_caret_for_every_companion() {
    for pet_mode in [true, false] {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        let now = Instant::now();
        let glow = PetOwnership { on: pet_mode };
        let mut prep = |facts: &TerminalFacts, ms: u64| {
            let host = host_at(now + Duration::from_millis(ms));
            let t = owner.prepare(
                PetFacts {
                    pet: glow,
                    ..pet_facts(facts, &host)
                },
                &mut decos,
            );
            let escort = t.escort_caret();
            let sensed = t.sense.caret;
            let _ = owner.tick(t);
            (escort, sensed)
        };
        let finished = facts_with(Some((20, 7)));
        assert_eq!(prep(&finished, 16).0, Some((20, 7)), "pet {pet_mode}");
        let mut torn = facts_with(Some((2, 0)));
        torn.cursor_visible = false;
        torn.frame_unfinished = true;
        let (escort, sensed) = prep(&torn, 32);
        assert_eq!(escort, Some((20, 7)), "pet {pet_mode}: no write cursor");
        assert_eq!(sensed.is_some(), pet_mode, "only an owned pet senses it");
        let mut hidden = facts_with(Some((5, 9)));
        hidden.cursor_visible = false;
        hidden.sync_end_seq = 1;
        hidden.sync_end_caret = hidden.caret;
        assert_eq!(prep(&hidden, 48).0, Some((5, 9)), "pet {pet_mode}");
        let mut other = torn;
        other.session = 2;
        assert_eq!(prep(&other, 64).0, None, "pet {pet_mode}");
    }
}

#[test]
fn an_unbracketed_repaint_s_write_position_is_no_caret() {
    let mut decos = WordDecorations::default();
    let mut owner = an_enabled_owner();
    let now = Instant::now();
    let mut prep = |facts: &TerminalFacts, ms: u64| {
        let t = prepared(
            &mut owner,
            &mut decos,
            facts,
            &host_at(now + Duration::from_millis(ms)),
            false,
        );
        let reading = (t.escort_caret(), t.sense.caret);
        let _ = owner.tick(t);
        reading
    };
    let hidden = |caret| {
        let mut f = facts_with(Some(caret));
        f.cursor_visible = false;
        f
    };
    assert_eq!(prep(&hidden((20, 10)), 16).0, Some((20, 10)), "first sight");
    assert_eq!(prep(&hidden((3, 60)), 32), (Some((20, 10)), Some((20, 10))));
    assert_eq!(prep(&hidden((20, 10)), 48).0, Some((20, 10)));
    assert_eq!(prep(&hidden((3, 60)), 64).0, Some((20, 10)));
    assert_eq!(
        prep(&hidden((3, 60)), 80).0,
        Some((20, 10)),
        "16 ms is no rest"
    );
    assert_eq!(prep(&hidden((20, 10)), 96).0, Some((20, 10)));
    assert_eq!(prep(&hidden((20, 11)), 112).0, Some((20, 10)));
    assert_eq!(prep(&hidden((20, 11)), 128).0, Some((20, 10)));
    assert_eq!(
        prep(&hidden((20, 11)), 144).0,
        Some((20, 11)),
        "rested 32 ms"
    );
    let mut shown = facts_with(Some((3, 60)));
    shown.cursor_visible = true;
    assert_eq!(prep(&shown, 160).0, Some((3, 60)), "negative control");
    let mut torn = hidden((3, 0));
    torn.frame_unfinished = true;
    assert_eq!(
        prep(&torn, 176).0,
        Some((3, 60)),
        "a torn frame reads the last finished"
    );
    let mut closed = hidden((9, 9));
    closed.sync_end_seq = 1;
    closed.sync_end_caret = closed.caret;
    assert_eq!(
        prep(&closed, 192).0,
        Some((9, 9)),
        "a finished frame's caret"
    );
    let mut again = hidden((9, 10));
    again.sync_end_seq = 2;
    again.sync_end_caret = again.caret;
    assert_eq!(prep(&again, 208).0, Some((9, 10)), "and the next");
    let mut unclosed = hidden((3, 60));
    unclosed.sync_end_seq = 2;
    assert_eq!(
        prep(&unclosed, 224).0,
        Some((9, 10)),
        "an unbracketed write position after bracketed frames"
    );

    let mut terminal = aterm_core::terminal::Terminal::new(24, 80);
    let mut owner = an_enabled_owner();
    let mut decos = WordDecorations::default();
    let clock = |ms| aterm_core::terminal::ClockReading {
        monotonic: now + Duration::from_millis(ms),
        wall_ms: None,
    };
    let mut take = |term: &aterm_core::terminal::Terminal, ms| {
        let facts = TerminalFacts::read(term, 1, false);
        let tick = prepared(
            &mut owner,
            &mut decos,
            &facts,
            &host_at(now + Duration::from_millis(ms)),
            false,
        );
        let caret = tick.escort_caret();
        let _ = owner.tick(tick);
        caret
    };
    terminal.process_at(b"\x1b[?25l\x1b[20;11H", clock(0));
    assert_eq!(take(&terminal, 0), Some((19, 10)), "first sight");
    terminal.process_at(b"\x1b[?2026h\x1b[20;12H\x1b[?2026l", clock(16));
    assert_eq!(terminal.sync_end_caret(), Some((19, 11)));
    assert_eq!(
        take(&terminal, 16),
        Some((19, 11)),
        "closed move: immediate"
    );

    terminal.process_at(
        b"\x1b[?2026h\x1b[20;13H\x1b[?2026l\x1b[3;1Hwrite",
        clock(32),
    );
    assert_eq!(terminal.sync_end_seq(), 2, "a new close really occurred");
    assert!(
        !terminal.sync_frame_unfinished(),
        "later write is unbracketed"
    );
    assert_eq!(terminal.sync_end_caret(), Some((19, 12)));
    assert_eq!((terminal.cursor().row, terminal.cursor().col), (2, 5));
    assert_eq!(
        take(&terminal, 32),
        Some((19, 11)),
        "the close did not end here"
    );
    assert_eq!(take(&terminal, 48), Some((19, 11)), "16 ms is no rest");
    assert_eq!(
        take(&terminal, 64),
        Some((2, 5)),
        "a genuine rest still arrives"
    );

    terminal.process_at(b"\x1b[?2026h\x1b[20;14H\x1b[?2026l\x1b[?2026h", clock(80));
    assert!(!terminal.sync_frame_unfinished(), "empty reopened bracket");
    assert_eq!(
        take(&terminal, 80),
        Some((19, 13)),
        "closed move still immediate"
    );
    terminal.process_at(b"\x1b[3;1Hpartial", clock(96));
    assert_eq!(take(&terminal, 96), Some((19, 13)), "torn frame stays held");
    terminal.process_at(b"", clock(2100));
    assert!(!terminal.modes().synchronized_output(), "timeout control");
    assert!(
        terminal.sync_frame_unfinished(),
        "timeout is no program close"
    );
    assert_eq!(terminal.sync_end_caret(), None, "latest count is a timeout");
    assert_eq!(take(&terminal, 2100), Some((19, 13)));
    terminal.process_at(b"\x1b[?2026l", clock(2116));
    assert_eq!(
        take(&terminal, 2116),
        Some((2, 7)),
        "real close after timeout"
    );

    terminal.process_at(b"\x1b[!p", clock(2132));
    assert_eq!(terminal.sync_end_caret(), None, "soft reset invalidates");
    terminal.process_at(b"\x1b[?2026h\x1b[?2026l\x1bc", clock(2148));
    assert_eq!(terminal.sync_end_caret(), None, "full reset invalidates");
    terminal.process_at(b"\x1b[?2026h\x1b[?2026l\x1b[?1049h", clock(2164));
    assert_eq!(terminal.sync_end_caret(), None, "screen switch invalidates");
    terminal.process_at(b"\x1b[?1049l\x1b[?2026h\x1b[?2026l", clock(2180));
    assert!(
        terminal.sync_end_caret().is_some(),
        "fresh main-screen close"
    );
    terminal.resize(24, 81);
    terminal.resize(24, 80);
    assert_eq!(
        terminal.sync_end_caret(),
        None,
        "regrid back cannot revive it"
    );
    terminal.process_at(b"\x1b[?2026h\x1b[?2026l", clock(2196));
    assert!(terminal.sync_end_caret().is_some());
    let checkpoint = terminal.checkpoint();
    terminal.restore_checkpoint(&checkpoint);
    assert_eq!(
        terminal.sync_end_caret(),
        None,
        "in-place restore invalidates"
    );
    let restored = aterm_core::terminal::Terminal::from_checkpoint(&checkpoint);
    assert_eq!(
        restored.sync_end_caret(),
        None,
        "fresh restore starts unknown"
    );
    terminal.process_at(b"\x1b[?2026h\x1b[?2026l", clock(2212));
    assert!(terminal.sync_end_caret().is_some());
    let _ = terminal.cell_frame(24, 80);
    assert!(
        terminal.sync_end_caret().is_some(),
        "cell_frame snapshot preserves close cell for TerminalFacts::read"
    );
    let _ = terminal.grid_mut();
    assert_eq!(terminal.sync_end_caret(), None, "mutable grid boundary");
}

#[test]
fn a_torn_frame_after_a_resize_reads_no_old_caret() {
    let mut decos = WordDecorations::default();
    let mut owner = an_enabled_owner();
    let now = materialize(&mut owner, &mut decos);
    let finished = facts_with(Some((20, 7)));
    let t = prepared(&mut owner, &mut decos, &finished, &host_at(now), false);
    let _ = owner.tick(t);
    let mut torn = facts_with(Some((2, 0)));
    torn.cursor_visible = false;
    torn.frame_unfinished = true;
    let mut host = host_at(now + Duration::from_millis(16));
    let t = prepared(&mut owner, &mut decos, &torn, &host, false);
    assert_eq!(
        t.escort_caret(),
        Some((20, 7)),
        "fixture: same grid, the latch"
    );
    let _ = owner.tick(t);
    host.now += Duration::from_millis(16);
    host.geometry.rows += 4;
    let t = prepared(&mut owner, &mut decos, &torn, &host, false);
    assert_eq!(
        t.escort_caret(),
        None,
        "a caret read before the resize is a cell of another grid"
    );
}

#[test]
fn a_capture_spends_no_present_latch() {
    for capture in [CaptureMode::LiveCapture, CaptureMode::StaticCapture] {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        let now = materialize(&mut owner, &mut decos);
        let mut facts = facts_with(Some((4, 12)));
        facts.shell_executing = true;
        let t = prepared(&mut owner, &mut decos, &facts, &host_at(now), false);
        let _ = owner.tick(t);
        facts.cmd_done = Some((1, 1, Some(40)));
        facts.content_seq = 9;
        facts.wrap_serial = 3;
        facts.frame_unfinished = true;
        facts.cursor_visible = false;
        facts.caret = Some((2, 0));
        let mut host = host_at(now + Duration::from_millis(16));
        host.capture = capture;
        host.pointer_px = Some((55.0, 85.0));
        let t = prepared(&mut owner, &mut decos, &facts, &host, false);
        assert_eq!(
            t.completion, None,
            "{capture:?}: a capture feels no completion"
        );
        assert!(!t.sense.output_burst && !t.sense.wrapped, "{capture:?}");
        assert_eq!(t.sense.pointer, None, "{capture:?}: a capture has no mouse");
        let _ = owner.tick(t);
        assert!(!owner.grieving(), "{capture:?}: nothing was noted");

        host.capture = CaptureMode::Present;
        host.now += Duration::from_millis(16);
        facts.frame_unfinished = false;
        facts.caret = Some((4, 13));
        let t = prepared(&mut owner, &mut decos, &facts, &host, false);
        assert_eq!(
            t.escort_caret(),
            Some((4, 12)),
            "{capture:?}: the new hidden caret still waits for a live rest"
        );
        assert_eq!(t.completion, Some((1, Some(40))), "{capture:?}");
        assert!(t.sense.output_burst && t.sense.wrapped, "{capture:?}");
        assert!(owner.grieving(), "{capture:?}: the failure is felt now");
        let _ = owner.tick(t);
    }
}

#[test]
fn the_completion_edge_is_handed_back_once_and_never_on_a_session_switch() {
    let mut decos = WordDecorations::default();
    let mut owner = an_enabled_owner();
    let mut now = materialize(&mut owner, &mut decos);
    let mut step = |owner: &mut CompanionOwner, session, cmd_done| {
        now += Duration::from_millis(16);
        let mut facts = facts_with(Some((4, 12)));
        facts.session = session;
        facts.cmd_done = cmd_done;
        let t = prepared(owner, &mut decos, &facts, &host_at(now), false);
        let edge = t.completion;
        let _ = owner.tick(t);
        edge
    };
    assert_eq!(
        step(&mut owner, 7, Some((3, 0, None))),
        None,
        "first sighting"
    );
    assert_eq!(step(&mut owner, 7, Some((3, 0, None))), None, "no change");
    assert_eq!(
        step(&mut owner, 7, Some((4, 2, Some(900)))),
        Some((2, Some(900))),
        "a new completion in the same session"
    );
    assert_eq!(step(&mut owner, 7, Some((4, 2, Some(900)))), None, "once");
    assert_eq!(
        step(&mut owner, 8, Some((11, 1, None))),
        None,
        "a session switch re-baselines silently"
    );
    assert_eq!(step(&mut owner, 9, None), None, "a session with no history");
    assert_eq!(
        step(&mut owner, 9, Some((1, 1, None))),
        Some((1, None)),
        "its None -> Some edge is a real first completion"
    );
}

fn landed_frame(
    owner: &mut CompanionOwner,
    decos: &mut WordDecorations,
    now: Instant,
    landed: Option<u32>,
    shape: impl FnOnce(PetFacts<'_>) -> PetFacts<'_>,
) -> (Option<(u64, u32)>, CompanionFrame) {
    let facts = facts_with(Some((4, 12)));
    let host = host_at(now);
    let room = RoomFacts {
        session: Some(SessionRoom {
            landed,
            ..SessionRoom::default()
        }),
        ..RoomFacts::default()
    };
    let t = owner.prepare(
        shape(PetFacts {
            room: Some(room),
            ..pet_facts(&facts, &host)
        }),
        decos,
    );
    let seen = t.landed_seen;
    (seen, owner.tick(t))
}

#[test]
fn a_landing_is_cheered_once_by_the_first_frame_that_shows_it() {
    let mut decos = WordDecorations::default();
    let mut owner = an_enabled_owner();
    let now = materialize(&mut owner, &mut decos);
    let mut now = settle(&mut owner, &mut decos, now);
    let c0 = owner.brain().content();
    for (why, focused, obscured) in [("unfocused", false, false), ("covered", true, true)] {
        for _ in 0..30 {
            now += Duration::from_millis(16);
            let (seen, _) = landed_frame(&mut owner, &mut decos, now, Some(1), |f| PetFacts {
                focused,
                obscured,
                ..f
            });
            assert_eq!(seen, None, "{why}: nobody can see the pane, the end waits");
            assert!(!owner.brain().cheer_owed(), "{why}: no cheer latched");
        }
    }
    assert!(
        owner.brain().content() < c0 + 0.1,
        "no ledger moved while it waited"
    );
    now += Duration::from_millis(16);
    let (seen, _) = landed_frame(&mut owner, &mut decos, now, Some(1), |f| f);
    assert_eq!(seen, Some((1, 1)), "the first frame that shows it takes it");
    assert!(owner.brain().cheer_owed(), "and the pet latched the cheer");
    let mut most = 0;
    for _ in 0..400 {
        now += Duration::from_millis(16);
        let (seen, frame) = landed_frame(&mut owner, &mut decos, now, Some(1), |f| f);
        assert_eq!(seen, Some((1, 1)), "handed back while the room names it");
        let cheer = frame
            .pet
            .motes
            .iter()
            .flatten()
            .filter(|m| {
                matches!(
                    m.kind,
                    crate::kitty_pet::PetMoteKind::Note | crate::kitty_pet::PetMoteKind::Heart
                )
            })
            .count();
        most = most.max(cheer);
    }
    assert!(
        (1..=3).contains(&most),
        "one cheer's ♪/♥ on glass, not a cheer per restatement: {most}"
    );
    assert!(
        !owner.brain().cheer_owed(),
        "nothing owed: the serial again is nothing"
    );
    assert!(!owner.needs_frames(), "and the pet settled");
    now += Duration::from_millis(16);
    let (seen, _) = landed_frame(&mut owner, &mut decos, now, Some(2), |f| f);
    assert_eq!(seen, Some((1, 2)));
    assert!(owner.brain().cheer_owed(), "a second end is news again");
}

#[test]
fn without_the_pet_a_landing_is_spent_unplayed() {
    for (why, glow, serious) in [
        ("the pet setting off", PetOwnership::NONE, false),
        ("serious mode", pet_glow(), true),
    ] {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        let now = materialize(&mut owner, &mut decos);
        let now = settle(&mut owner, &mut decos, now) + Duration::from_millis(16);
        let facts = facts_with(Some((4, 12)));
        let mut host = host_at(now);
        host.serious = serious;
        let room = RoomFacts {
            session: Some(SessionRoom {
                landed: Some(1),
                ..SessionRoom::default()
            }),
            ..RoomFacts::default()
        };
        let t = owner.prepare(
            PetFacts {
                pet: glow,
                room: Some(room),
                ..pet_facts(&facts, &host)
            },
            &mut decos,
        );
        assert_eq!(t.landed_seen, Some((1, 1)), "{why}: seen, so spent");
        assert!(!owner.brain().cheer_owed(), "{why}: no cheer");
        let _ = owner.tick(t);
    }
}

#[test]
fn the_owner_inks_a_cheers_motes_for_the_page_it_draws_on() {
    use crate::kitty_pet::{PetMoteKind, PetMoteSprite};
    let mut decos = WordDecorations::default();
    let mut owner = an_enabled_owner();
    let now = materialize(&mut owner, &mut decos);
    let mut t = frame(
        &mut owner,
        &mut decos,
        now + Duration::from_millis(16),
        Some((4, 12)),
    );
    assert!(t.on_glass, "fixture: the pet is on glass");
    let air = [
        (PetMoteKind::Note, 0x00FF_C24Du32),
        (PetMoteKind::Heart, 0x00FF_6B9A),
        (PetMoteKind::Note, 0x00FF_C24D),
    ];
    for (k, (kind, rgb)) in air.into_iter().enumerate() {
        t.pet.motes[k] = Some(PetMoteSprite {
            kind,
            col: t.pet.col + 2.0 + k as f32,
            row: (t.pet.row - 2.0).max(0.5),
            scale: 1.0,
            rot: 0.4,
            alpha: 255,
            rgb: Some(rgb),
        });
    }
    let mut tints = |bg: u32| {
        let theme = ContrastFallback {
            bg,
            cursor: 0x0080_8080,
            accent: 0x0040_80FF,
        };
        let mut free = Vec::new();
        for _ in 0..3 {
            free.clear();
            decos.begin_host_frame();
            let _ = owner.emit(&t, &[], theme, &mut decos, &mut free);
        }
        free.iter()
            .map(|s| s.tint)
            .filter(|&tint| {
                air.iter().any(|&(_, rgb)| {
                    tint == rgb || tint == crate::rainbow_kitty::ribbon::paper_mark_ink(rgb, bg)
                })
            })
            .collect::<Vec<_>>()
    };
    let contrast = |a: u32, b: u32| {
        let (ya, yb) = (
            crate::color_math::relative_luminance(a),
            crate::color_math::relative_luminance(b),
        );
        (ya.max(yb) + 0.05) / (ya.min(yb) + 0.05)
    };
    let dark = tints(0x0010_1010);
    assert_eq!(
        dark,
        air.map(|(_, rgb)| rgb).to_vec(),
        "a dark ground keeps each ♪/♥ its own colour"
    );
    for (name, bg) in [("white", 0x00F8_FAFDu32), ("cream", 0x00F9_F3E6)] {
        let paper = tints(bg);
        assert_eq!(
            paper,
            air.map(|(_, rgb)| crate::rainbow_kitty::ribbon::paper_mark_ink(rgb, bg))
                .to_vec(),
            "{name}: each ♪/♥ takes the mark's ink for the page"
        );
        for &ink in &paper {
            assert!(
                contrast(ink, bg) >= 4.5 - 1e-3,
                "{name}: {ink:06x} on {bg:06x} reads at {:.2}:1",
                contrast(ink, bg)
            );
        }
    }
}

#[test]
fn an_obscured_surface_keeps_the_pet_owned_but_off_glass() {
    let mut decos = WordDecorations::default();
    let mut owner = an_enabled_owner();
    let now = materialize(&mut owner, &mut decos);
    let facts = facts_with(Some((4, 12)));
    let t = prepared(
        &mut owner,
        &mut decos,
        &facts,
        &host_at(now + Duration::from_millis(16)),
        true,
    );
    assert!(!t.visible && t.sense.caret.is_none());
    let frame = owner.tick(t);
    assert!(!frame.on_glass && owner.hit_rect().is_none());
    assert!(
        owner.brain().is_active(),
        "still the resident: only unpresented"
    );
    let t = prepared(
        &mut owner,
        &mut decos,
        &facts,
        &host_at(now + Duration::from_millis(32)),
        false,
    );
    assert!(
        t.visible,
        "negative control: the uncovered grid presents it"
    );
}

#[test]
fn the_shed_latch_takes_the_train_never_the_motion() {
    let mut decos = WordDecorations::default();
    let mut owner = an_enabled_owner();
    let now = materialize(&mut owner, &mut decos);
    let facts = facts_with(Some((4, 12)));
    let mut host = host_at(now + Duration::from_millis(16));
    host.shed_active = true;
    host.shed_envelope = 0.0;
    let t = prepared(&mut owner, &mut decos, &facts, &host, false);
    assert!(!t.sense.reduced_motion, "the latch is not a posture");
    let _ = owner.tick(t);
    assert!(owner.brain().needs_frames(), "the body is still moving");
    assert!(!owner.needs_frames(), "…but it owes no frame train");
    assert_eq!(
        owner.next_change_deadline(host.now),
        Some(host.now + RESIDENT_SHED_CADENCE),
        "…it is offered at the coarse cadence"
    );
    let late = host.now + RESIDENT_SHED_CADENCE * 2;
    assert_eq!(
        owner.next_change_deadline(late),
        Some(late + RESIDENT_SHED_CADENCE)
    );
    host.reduced_motion = true;
    host.now += Duration::from_millis(16);
    let t = prepared(&mut owner, &mut decos, &facts, &host, false);
    assert!(t.sense.reduced_motion, "negative control: the policy pins");
    let _ = owner.tick(t);
    assert_eq!(owner.next_change_deadline(host.now), None);
}

#[test]
fn the_drain_is_a_fresh_brain_that_keeps_the_session_latches() {
    let mut decos = WordDecorations::default();
    let mut owner = an_enabled_owner();
    owner.set_species(PetSpecies::Dog);
    assert_eq!(
        owner.brain().species(),
        PetSpecies::Dog,
        "a cold owner's brain already wears the chosen animal"
    );
    let mut now = materialize(&mut owner, &mut decos);
    let mut facts = facts_with(Some((4, 12)));
    facts.cmd_done = Some((5, 0, None));
    let t = prepared(&mut owner, &mut decos, &facts, &host_at(now), false);
    let _ = owner.tick(t);
    owner.note_trick(now, aterm_lexicon::Trick::Sit, true);
    assert!(owner.brain().pending_trick().is_some());

    owner.drain();
    assert!(!owner.brain().is_active() && owner.brain().pending_trick().is_none());
    assert_eq!(owner.hit_rect(), None);
    assert_ne!(PetSpecies::default(), PetSpecies::Dog, "fixture");
    assert_eq!(
        owner.brain().species(),
        PetSpecies::default(),
        "a drain is a fresh brain: the identity is gone until the next prepare"
    );
    now += Duration::from_millis(16);
    let t = prepared(&mut owner, &mut decos, &facts, &host_at(now), false);
    assert_eq!(owner.brain().species(), PetSpecies::Dog);
    assert_eq!(t.completion, None, "seq 5 was already felt");
    let _ = owner.tick(t);
    facts.cmd_done = Some((6, 0, None));
    now += Duration::from_millis(16);
    let t = prepared(&mut owner, &mut decos, &facts, &host_at(now), false);
    assert_eq!(t.completion, Some((0, None)));
}

#[test]
fn the_level_rules_retire_hide_and_preserve_the_resident_under_load() {
    let mut decos = WordDecorations::default();

    let mut owner = an_enabled_owner();
    let now = materialize(&mut owner, &mut decos);
    let facts = facts_with(Some((4, 12)));
    let host = host_at(now + Duration::from_millis(16));
    let t = owner.sense(
        PetFacts {
            pet: PetOwnership { on: false },
            ..pet_facts(&facts, &host)
        },
        &mut decos,
    );
    assert!(
        !t.on_glass && !owner.needs_frames(),
        "retired at once, lane released"
    );

    let mut owner = an_enabled_owner();
    let now = materialize(&mut owner, &mut decos);
    let mut host = host_at(now + Duration::from_millis(16));
    host.serious = true;
    let t = owner.sense(pet_facts(&facts, &host), &mut decos);
    assert!(!t.on_glass && !owner.needs_frames());

    let mut owner = an_enabled_owner();
    let now = materialize(&mut owner, &mut decos);
    let t = frame(
        &mut owner,
        &mut decos,
        now + Duration::from_millis(16),
        Some((4, 12)),
    );
    let (_, worn) = owner.emit(&t, &[], BLACK_FALLBACK, &mut decos, &mut Vec::new());
    let mut host = host_at(now + Duration::from_millis(32));
    host.visibility = Visibility::Hidden;
    let t = owner.sense(
        PetFacts {
            focused: false,
            ..pet_facts(&facts, &host)
        },
        &mut decos,
    );
    assert!(!t.on_glass);
    assert_eq!(owner.hit_rect(), None);
    assert_eq!(
        owner.brain().worn_pair(),
        Some(worn.worn),
        "the same coat returns"
    );

    let mut owner = an_enabled_owner();
    let now = materialize(&mut owner, &mut decos);
    let mut history = facts_with(None);
    history.display_offset = 3;
    history.live_viewport = false;
    let host = host_at(now + Duration::from_millis(16));
    let t = owner.sense(pet_facts(&history, &host), &mut decos);
    assert!(!t.on_glass);
    assert_eq!(
        owner.hit_rect(),
        None,
        "history clears the clickable body at once"
    );
    assert!(
        owner.brain().is_active(),
        "…while the brain is still fading"
    );

    let mut owner = an_enabled_owner();
    let now = materialize(&mut owner, &mut decos);
    for _ in 0..40 {
        let _ = frame(
            &mut owner,
            &mut decos,
            now + Duration::from_millis(16),
            Some((4, 12)),
        );
    }
    let mut host = host_at(now + Duration::from_millis(32));
    host.shed_envelope = 0.5;
    host.shed_active = true;
    let t = owner.sense(pet_facts(&facts, &host), &mut decos);
    assert!(t.on_glass);
    assert_eq!(t.pet.alpha, 255, "the full resident remains visible");
    assert_eq!(owner.alpha(), 255);
    assert!(
        owner.brain().is_active(),
        "the resident remains active under the shed"
    );
}

fn resident_shed_model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        ResidentShed {
            const Buggy = 0;
            var phase = 0;
            var smooth = 1;
            var gait = 1;
            var train = 1;
            var paced = 1;
            var coarse = 1;
            action Shed when (phase == 0) {
                phase = 1;
                smooth = if Buggy == 0 { 1 } else { 0 };
                gait = if Buggy == 0 { 1 } else { 0 };
                train = 0;
                paced = if Buggy == 0 { 1 } else { 0 };
            }
            action Move when (phase == 1) {
                phase = 2;
                train = if Buggy == 0 { 0 } else { 1 };
            }
            action Settle when (phase == 2) {
                phase = 3;
                train = 0;
                coarse = if Buggy == 0 { 1 } else { 0 };
            }
            action Recover when (phase == 3) {
                phase = 4;
                train = if Buggy == 0 { 1 } else { 0 };
            }
            invariant Continuous: smooth == 1;
            invariant NoForcedSeat: gait == 1;
            invariant MovingStaysPaced: paced == 1;
            invariant ShedTakesTheTrain: phase == 0 || phase == 4 || train == 0;
            invariant RecoveryRestoresTheTrain: phase <= 3 || train == 1;
            invariant ShedWakesAreCoarse: coarse == 1;
            invariant Bounds: phase <= 4 && smooth <= 1 && gait <= 1 && train <= 1
                && paced <= 1 && coarse <= 1;
        }
    }
}

#[test]
fn resident_shed_model_proves_and_catches_the_snap() {
    let model = resident_shed_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    assert_eq!(
        aterm_spec::verify::uncaught_invariants(&model),
        ["Bounds"],
        "every design invariant has a Buggy branch that breaks it"
    );
}

struct ShedRig {
    term: aterm_core::terminal::Terminal,
    owner: CompanionOwner,
    decos: WordDecorations,
    now: Instant,
    prev: Option<PetFrame>,
    /// The largest step so far, in cells per 16 ms of clock.
    max_step: f32,
    /// Replay the snap rule: the latch folded into the reduced-motion pin.
    old_rule: bool,
    /// Every offer after a shed tick lands no sooner than
    /// [`RESIDENT_SHED_CADENCE`]: `[the owner's, the unfloored rule's]`.
    coarse: [bool; 2],
}

const STEP_PER_FRAME: f32 = 1.2;

impl ShedRig {
    fn new(old_rule: bool) -> Self {
        let mut term = aterm_core::terminal::Terminal::new(GRID.rows, GRID.cols);
        term.process(b"\x1b]11;#000000\x07$ ");
        Self {
            term,
            owner: an_enabled_owner(),
            decos: WordDecorations::default(),
            now: Instant::now(),
            prev: None,
            max_step: 0.0,
            old_rule,
            coarse: [true; 2],
        }
    }

    fn present(
        &mut self,
        dt: Duration,
        key: Option<u8>,
        shed: bool,
        envelope: f32,
    ) -> (PetFrame, bool) {
        use crate::pet_world::{PetPane, PetWorldFacts};
        self.now += dt;
        if let Some(byte) = key {
            self.owner.note_console_input(self.now, PetInputKind::Text);
            self.term.process(&[byte]);
        }
        let input = self
            .term
            .cell_frame(usize::from(GRID.rows), usize::from(GRID.cols));
        let world = PetWorldFacts::read(&self.term, 1);
        self.owner
            .observe_console(&input, &world, PetPane::full(&input), &[]);
        let facts = TerminalFacts::read(&self.term, 1, false);
        let mut host = host_at(self.now);
        host.shed_active = shed;
        host.shed_envelope = envelope;
        host.reduced_motion = self.old_rule && (shed || envelope < 1.0);
        let t = prepared(&mut self.owner, &mut self.decos, &facts, &host, false);
        let pet = self.owner.tick(t).pet;
        if shed {
            let brain = self.owner.brain();
            let unfloored = if brain.needs_frames() {
                self.owner.next_change_deadline(self.now)
            } else {
                brain.next_change_deadline(self.now)
            };
            let late = |offer: Option<Instant>| {
                offer.is_none_or(|d| d >= self.now + RESIDENT_SHED_CADENCE)
            };
            self.coarse[0] &= late(self.owner.next_change_deadline(self.now));
            self.coarse[1] &= late(unfloored);
        }
        let step = self.prev.map_or(0.0, |p| {
            ((pet.col - p.col).powi(2) + (pet.row - p.row).powi(2)).sqrt()
                / (dt.as_secs_f32() / 0.016).max(1.0)
        });
        self.max_step = self.max_step.max(step);
        self.prev = Some(pet);
        (pet, step <= STEP_PER_FRAME)
    }

    fn paced(&self) -> bool {
        !self.owner.brain().needs_frames()
            || self.owner.needs_frames()
            || self
                .owner
                .next_change_deadline(self.now)
                .is_some_and(|d| d <= self.now + RESIDENT_SHED_CADENCE)
    }
}

fn shed_state(
    phase: i64,
    [smooth, gait, train, paced, coarse]: [bool; 5],
) -> BTreeMap<&'static str, i64> {
    BTreeMap::from([
        ("phase", phase),
        ("smooth", i64::from(smooth)),
        ("gait", i64::from(gait)),
        ("train", i64::from(train)),
        ("paced", i64::from(paced)),
        ("coarse", i64::from(coarse)),
    ])
}

#[test]
fn a_shed_edge_never_snaps_or_seats_a_moving_resident() {
    use crate::kitty_pet::PetAction;
    const FRAME: Duration = Duration::from_millis(16);
    const KEY: Duration = Duration::from_millis(80);
    let model = resident_shed_model();
    let step = |state: &mut BTreeMap<&'static str, i64>,
                action: &str,
                observed: BTreeMap<&'static str, i64>|
     -> Result<(), String> {
        let (ok, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            state,
            &observed,
            Some(action),
            "real resident across a load-shed edge",
        );
        if !ok {
            return Err(format!("{action}: {observed:?}: {why}"));
        }
        *state = observed;
        Ok(())
    };
    for old_rule in [false, true] {
        let mut rig = ShedRig::new(old_rule);
        for _ in 0..40 {
            let _ = rig.present(Duration::from_millis(100), None, false, 1.0);
        }
        let mut text = b"the quick brown fox jumps over the lazy dog and on and on".iter();
        let mut state = model.init_state();

        let (mut smooth, mut paced) = (true, true);
        let mut prev = rig.present(FRAME, None, false, 1.0).0;
        let mut frames = 0u32;
        while frames < 60 || prev.action != PetAction::Run {
            let key = frames
                .is_multiple_of(5)
                .then(|| *text.next().expect("fixture: text"));
            let (pet, ok) = rig.present(FRAME, key, false, 1.0);
            smooth &= ok;
            prev = pet;
            frames += 1;
        }
        assert!(
            smooth && rig.owner.needs_frames(),
            "fixture: a smooth run on the train"
        );

        let (edge, ok) = rig.present(FRAME, None, true, 1.0);
        let gait = !matches!(edge.action, PetAction::Sit | PetAction::Sleep);
        let edge_state = shed_state(
            1,
            [
                ok,
                gait,
                rig.owner.needs_frames(),
                rig.paced(),
                rig.coarse[0],
            ],
        );
        if let Err(why) = step(&mut state, "Shed", edge_state) {
            assert!(old_rule, "the edge {prev:?} -> {edge:?}: {why}");
            println!(
                "shipped rule: max step {:.2} cells per 16 ms through the edge, {:?} -> {:?}; rejected: {why}",
                rig.max_step, prev.action, edge.action
            );
            continue;
        }
        assert!(
            !old_rule,
            "negative control: the shipped snap passed the edge"
        );

        let shed_at = rig.now;
        let mut next_key = rig.now + KEY;
        let mut train = false;
        for _ in 0..20 {
            loop {
                let offer = rig.owner.next_change_deadline(rig.now);
                let at = offer.map_or(next_key, |o| o.min(next_key));
                let key = (at >= next_key).then(|| *text.next().expect("fixture: text"));
                let dt = at
                    .saturating_duration_since(rig.now)
                    .max(Duration::from_millis(1));
                let fall = 1.0 - (rig.now + dt - shed_at).as_secs_f32() / 0.45;
                let (_, ok) = rig.present(dt, key, true, fall.max(0.0));
                smooth &= ok;
                paced &= rig.paced();
                train |= rig.owner.needs_frames();
                if key.is_some() {
                    next_key = rig.now + KEY;
                    break;
                }
            }
        }
        let observed = shed_state(2, [smooth, gait, train, paced, rig.coarse[0]]);
        step(&mut state, "Move", observed).expect("moving under the shed");

        let mut settled = false;
        for _ in 0..400 {
            if !rig.owner.brain().needs_frames() {
                settled = true;
                break;
            }
            let at = rig.owner.next_change_deadline(rig.now);
            let dt = at.map_or(Duration::from_millis(100), |d| {
                d.saturating_duration_since(rig.now)
            });
            let (_, ok) = rig.present(dt.max(Duration::from_millis(1)), None, true, 0.0);
            smooth &= ok;
            paced &= rig.paced();
            train |= rig.owner.needs_frames();
        }
        assert!(settled, "fixture: the resident settles under the shed");
        let (settled_at, mut wakes) = (rig.now, 0u32);
        while rig.now < settled_at + Duration::from_secs(3) {
            let Some(at) = rig.owner.next_change_deadline(rig.now) else {
                break;
            };
            let dt = at
                .saturating_duration_since(rig.now)
                .max(Duration::from_millis(1));
            let (_, ok) = rig.present(dt, None, true, 0.0);
            smooth &= ok;
            paced &= rig.paced();
            train |= rig.owner.needs_frames();
            wakes += 1;
        }
        assert!(
            wakes > 3,
            "fixture: a breathing seat is woken on its offers"
        );
        let unfloored = shed_state(3, [smooth, gait, train, paced, rig.coarse[1]]);
        assert!(
            step(&mut state.clone(), "Settle", unfloored).is_err(),
            "negative control: the unfloored settled offers passed"
        );
        let observed = shed_state(3, [smooth, gait, train, paced, rig.coarse[0]]);
        step(&mut state, "Settle", observed).expect("settled under the shed");

        let clear_at = rig.now;
        let mut train = false;
        for _ in 0..8 {
            let rise = (rig.now + FRAME - clear_at).as_secs_f32() / 0.25;
            let (_, ok) = rig.present(FRAME, Some(b'!'), false, rise.min(1.0));
            smooth &= ok;
            train |= rig.owner.needs_frames();
        }
        let observed = shed_state(4, [smooth, gait, train, paced, rig.coarse[0]]);
        step(&mut state, "Recover", observed).expect("full motion resumes");
        println!("new rule: max step {:.2} cells per 16 ms", rig.max_step);
    }
}

#[test]
fn capture_and_reenable_do_not_leave_stale_static_scheduler_custody() {
    let mut decos = WordDecorations::default();
    let mut owner = an_enabled_owner();
    let mut now = materialize(&mut owner, &mut decos);
    let facts = facts_with(Some((4, 12)));
    let mut host = host_at(now);
    host.capture = CaptureMode::StaticCapture;
    owner.sense(pet_facts(&facts, &host), &mut decos);
    assert!(
        owner.brain().needs_frames(),
        "the captured owner still has live motion"
    );
    assert!(
        owner.needs_frames(),
        "capture alone must not cancel that motion"
    );
    assert!(!owner.last_static);
    owner.last_static = true;
    assert!(
        !owner.needs_frames(),
        "control: the wrong cached fact loses real debt"
    );
    owner.last_static = false;

    now += Duration::from_millis(16);
    host.now = now;
    host.capture = CaptureMode::Present;
    host.shed_envelope = 0.0;
    host.shed_active = true;
    owner.sense(pet_facts(&facts, &host), &mut decos);
    assert!(!owner.last_static && owner.last_shed_tick == Some(now));
    assert!(!owner.needs_frames());
    owner.set_enabled_seed(false, 0x5EED);
    assert!(!owner.last_static && owner.last_shed_tick.is_none() && !owner.needs_frames());
    assert_eq!(owner.hit_rect(), None);
    assert_eq!(owner.alpha(), 0, "disable retires the body at once");
    assert!(!owner.brain().is_active());
    owner.set_enabled_seed(true, 0x5EED);
    now += Duration::from_millis(16);
    host.now = now;
    host.shed_envelope = 1.0;
    host.shed_active = false;
    owner.sense(pet_facts(&facts, &host), &mut decos);
    assert!(!owner.last_static);
    assert!(
        owner.brain().needs_frames(),
        "the new appearance owes live frames"
    );
    assert!(
        owner.needs_frames(),
        "re-enable must expose the new motion debt"
    );
}

#[test]
fn a_song_and_its_tail_keep_full_resident_custody() {
    let model = aterm_models_effects::reduced_motion_companion_handoff_model();
    let held = (1.0, "StartReducedSong");
    let runs: [&[(f32, &str)]; 4] = [
        &[
            held,
            (0.5, "SampleAtHalfCutoff"),
            (0.4, "SampleCadencedBelowHalf"),
            (0.329, "SampleBelowFaceSwap"),
            (0.1, ""),
            (0.0, "DrainSongTail"),
        ],
        &[held, (0.49, "SampleLateBelowHalf")],
        &[held, (0.30, "SampleLateBelowFaceSwap")],
        &[held, (0.0, "SampleLateDrained")],
    ];
    for reduced_motion in [false, true] {
        for run in runs {
            let mut decos = WordDecorations::default();
            let mut owner = an_enabled_owner();
            let mut now = materialize(&mut owner, &mut decos);
            let facts = facts_with(Some((4, 12)));
            let mut state = model.init_state();
            for &(drive, action) in run {
                now += Duration::from_millis(16);
                let mut host = host_at(now);
                host.reduced_motion = reduced_motion;
                let result = owner.sense(
                    PetFacts {
                        sing: SingFacts {
                            drive,
                            flying_alpha: 200,
                        },
                        ..pet_facts(&facts, &host)
                    },
                    &mut decos,
                );
                assert_eq!(result.duty, CompanionDuty::Pet, "drive={drive}");
                assert!(result.on_glass);
                assert_eq!(result.pet.alpha, 255);
                assert!(result.body_px.is_some(), "the full body owns geometry");
                assert!(owner.hit_rect().is_some(), "the resident stays pettable");
                assert!(
                    result.companion.is_some_and(|c| !c.guards_caret),
                    "word companions yield to the resident body, not a head's caret band"
                );
                if action.is_empty() {
                    continue;
                }
                let mut observed = model.successors(action, &state)[0].clone();
                observed.insert(
                    "singer_visible",
                    i64::from(result.duty != CompanionDuty::Pet),
                );
                observed.insert("pet_ready", i64::from(result.pet.alpha == 255));
                observed.insert("pet_visible", i64::from(result.on_glass));
                let label = format!("resident custody {action} (reduced={reduced_motion})");
                let (ok, why) = aterm_spec::verify::validate_transition_tiered(
                    &model,
                    &[],
                    &state,
                    &observed,
                    Some(action),
                    &label,
                );
                assert!(ok, "{label}: the real owner's sample is rejected: {why}");
                let mut blackout = observed.clone();
                blackout.insert("singer_visible", 0);
                blackout.insert("pet_visible", 0);
                let (forged_ok, _) = aterm_spec::verify::validate_transition_tiered(
                    &model,
                    &[],
                    &state,
                    &blackout,
                    Some(action),
                    "forged blackout",
                );
                assert!(!forged_ok, "{label}: an all-transparent projection passed");
                state = observed;
            }
        }
    }
}

#[test]
fn reduced_motion_pins_the_pet_at_its_station() {
    let mut decos = WordDecorations::default();
    let mut owner = an_enabled_owner();
    let now = Instant::now();
    let mut host = host_at(now);
    host.reduced_motion = true;
    let facts = facts_with(Some((4, 12)));
    let first = owner.sense(pet_facts(&facts, &host), &mut decos);
    assert_eq!(
        first.pet.alpha, 255,
        "the first reduced frame has real pixels"
    );
    let far = facts_with(Some((20, 70)));
    let mut host = host_at(now + Duration::from_millis(16));
    host.reduced_motion = true;
    let jumped = owner.sense(pet_facts(&far, &host), &mut decos);
    assert_eq!(jumped.pet.alpha, 255);
    assert_eq!(jumped.pet.lift, 0.0, "no arc");
    assert_eq!(
        jumped.pet.row, 20.0,
        "the station follows the caret row at once"
    );
}

#[test]
fn peek_cues_reach_the_brain_as_fractional_cells() {
    let mut decos = WordDecorations::default();
    let mut owner = an_enabled_owner();
    let now = materialize(&mut owner, &mut decos);
    let now = settle(&mut owner, &mut decos, now);
    let settled = frame(&mut owner, &mut decos, now, Some((4, 12)));
    assert!(!owner.needs_frames(), "fixture: settled");
    let cx = ((settled.pet.col + 4.0) * 10.0) as i32;
    let cy = (settled.pet.row * 20.0) as i32;
    let mut cues = vec![PeekCue {
        row: 4,
        col: 16,
        head_px: (cx - 10, cx + 10, cy - 10, cy + 10),
    }];
    owner.note_peeks(now, cues.drain(..), (10, 20));
    assert!(cues.is_empty(), "the drain is consumed");
    assert!(owner.needs_frames(), "a latched near peek re-arms the lane");
}

fn blank_glass(rows: u16, cols: u16, bg: [u8; 3]) -> Vec<Vec<RenderCell>> {
    let cell = RenderCell {
        ch: ' ',
        bg,
        ..RenderCell::default()
    };
    vec![vec![cell; usize::from(cols)]; usize::from(rows)]
}

#[test]
fn blank_glass_keys_the_pets_contrast_off_the_cursor_colour() {
    let bg = 0x0010_1010;
    let cells = blank_glass(GRID.rows, GRID.cols, [0x10, 0x10, 0x10]);
    let dress = |cursor: u32, accent: u32| {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        let now = materialize(&mut owner, &mut decos);
        let t = frame(
            &mut owner,
            &mut decos,
            now + Duration::from_millis(16),
            Some((4, 12)),
        );
        assert!(t.on_glass, "fixture: the body is drawn");
        let _ = owner.emit(
            &t,
            &cells,
            ContrastFallback { bg, cursor, accent },
            &mut decos,
            &mut Vec::new(),
        );
        owner
            .brain()
            .appearance_colors()
            .expect("a drawn appearance freezes its palette")
    };

    let red = dress(0x00FF_0000, 0x00FF_2020);
    assert_eq!(
        red,
        CatColorKey::from_rgb(bg, 0x00FF_0000, 0x00FF_2020),
        "no ink under the body: the key is the cursor colour's"
    );
    let neutral = dress(0x0080_8080, 0x0080_8080);
    assert_eq!(neutral, CatColorKey::from_rgb(bg, 0x0080_8080, 0x0080_8080));
    assert_ne!(
        red.accent, neutral.accent,
        "the cursor slot decides the accent family on blank glass"
    );
}

#[test]
fn retire_coordinate_space_keeps_the_command_latch_and_retire_owner_rebaselines_it() {
    let sense_cmd = |owner: &mut CompanionOwner,
                     decos: &mut WordDecorations,
                     now: Instant,
                     cmd_done: Option<(u64, i32, Option<u64>)>| {
        let mut facts = facts_with(Some((4, 12)));
        facts.cmd_done = cmd_done;
        let host = host_at(now);
        let _ = owner.sense(pet_facts(&facts, &host), decos);
    };

    for owner_edge in [false, true] {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        owner.set_species(PetSpecies::Dog);
        let now = materialize(&mut owner, &mut decos);
        sense_cmd(&mut owner, &mut decos, now, Some((9, 0, None)));
        assert!(owner.hit_rect().is_some(), "fixture: a body to retire");
        let t = frame(&mut owner, &mut decos, now, Some((4, 12)));
        let (_, before) = owner.emit(&t, &[], BLACK_FALLBACK, &mut decos, &mut Vec::new());

        if owner_edge {
            owner.retire_owner();
        } else {
            owner.retire_coordinate_space();
        }
        assert_eq!(
            owner.hit_rect(),
            None,
            "either retire drops the frame's coordinate artifact"
        );
        assert_eq!(
            owner.alpha(),
            0,
            "either retire clears the cached frame alpha ({owner_edge})"
        );
        assert!(
            !owner.brain().is_active() && !owner.needs_frames(),
            "either retire drops the body and its lane claim ({owner_edge})"
        );
        assert_eq!(
            (owner.brain().species(), owner.brain().worn_pair()),
            (PetSpecies::Dog, Some(before.worn)),
            "durable identity, species and coat, survives either retire ({owner_edge})"
        );

        sense_cmd(
            &mut owner,
            &mut decos,
            now + Duration::from_millis(16),
            Some((10, 1, None)),
        );
        if owner_edge {
            assert!(
                !owner.grieving(),
                "the replacement owner's first completion is a silent baseline"
            );
        } else {
            assert!(
                owner.grieving(),
                "a surface retire keeps feeling this session's completions"
            );
        }
    }
}

#[test]
fn alt_screen_switch_and_owner_retire_do_not_leak_finished_caret_or_hidden_rest() {
    let mut decos = WordDecorations::default();
    let mut owner = an_enabled_owner();
    let t0 = Instant::now();

    let mut main_facts = facts_with(Some((3, 15)));
    main_facts.cursor_visible = false;
    let host0 = host_at(t0);
    let tick0 = owner.prepare(pet_facts(&main_facts, &host0), &mut decos);
    assert_eq!(tick0.escort_caret, Some((3, 15)));

    let t1 = t0 + Duration::from_millis(5);
    let mut alt_torn = facts_with(Some((20, 5)));
    alt_torn.alt_screen = true;
    alt_torn.cursor_visible = false;
    alt_torn.frame_unfinished = true;
    let host1 = host_at(t1);
    let tick1 = owner.prepare(pet_facts(&alt_torn, &host1), &mut decos);
    assert_eq!(
        tick1.escort_caret, None,
        "torn alt-screen frame must not inherit main-screen finished_caret"
    );

    let mut alt_finished = alt_torn;
    alt_finished.frame_unfinished = false;
    let tick2 = owner.prepare(pet_facts(&alt_finished, &host1), &mut decos);
    assert_eq!(
        tick2.escort_caret,
        Some((20, 5)),
        "first hidden-cursor reading on alt screen is taken immediately"
    );

    owner.rebase_geometry();
    let t2 = t1 + Duration::from_millis(5);
    let host2 = host_at(t2);
    let tick3 = owner.prepare(pet_facts(&alt_torn, &host2), &mut decos);
    assert_eq!(
        tick3.escort_caret, None,
        "rebase_geometry clears finished_caret"
    );

    for use_resight in [false, true] {
        let mut m = main_facts;
        m.wrap_serial = 7;
        let _ = owner.prepare(pet_facts(&m, &host2), &mut decos);
        owner.last_alpha = 200;
        owner.last_visible = true;
        if use_resight {
            owner.resight_owner();
        } else {
            owner.retire_owner();
        }
        assert_eq!(
            owner.alpha(),
            0,
            "use_resight={use_resight}: retired/resighted owner immediately reports alpha 0 before next tick"
        );
        assert!(
            !owner.last_visible,
            "use_resight={use_resight}: retired/resighted owner immediately clears last_visible"
        );
        let mut next = m;
        next.caret = Some((9, 2));
        next.wrap_serial = 99;
        let t3 = t2 + Duration::from_millis(5);
        let host3 = host_at(t3);
        let tick4 = owner.prepare(pet_facts(&next, &host3), &mut decos);
        assert_eq!(
            tick4.escort_caret,
            Some((9, 2)),
            "use_resight={use_resight}: replacement owner takes its first hidden caret immediately"
        );
        assert!(
            !tick4.sense.wrapped,
            "use_resight={use_resight}: replacement owner baselines wrap_serial silently"
        );
    }
}
