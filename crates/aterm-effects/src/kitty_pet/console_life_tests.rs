// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::pet_world::PetCell;
use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::Terminal;

/// `row_signature` spells FNV-1a's word step out for the Trust lane, over each cell's scalar:
/// the same fold as `aterm_hash`'s, with 0 kept for a blank row and 1 for a fold that lands on
/// 0 or 1.
#[test]
fn a_rows_signature_folds_its_scalars_with_aterm_hashs_step() {
    let cell = |ch| aterm_core::terminal::RenderCell {
        ch,
        ..aterm_core::terminal::RenderCell::default()
    };
    let row: Vec<_> = "ab \u{1F600}\0z".chars().map(cell).collect();
    let want = row.iter().fold(aterm_hash::FNV64_OFFSET, |h, c| {
        aterm_hash::fnv_mix_u64(h, u64::from(u32::from(c.ch)))
    });
    assert!(want > 1);
    assert_eq!(row_signature(&row), want);
    assert_eq!(row_signature(&[cell(' '), cell('\0')]), 0);
    assert_eq!(row_signature(&[]), 0);
}

struct Scene {
    term: Terminal,
    pet: PetBrain,
    now: Instant,
}

impl Scene {
    fn new() -> Self {
        let mut term = Terminal::new(20, 80);
        term.process(b"\x1b]11;#000000\x07\x1b[6;17H");
        let now = Instant::now();
        let pet = PetBrain {
            col: 20.0,
            row: 5.0,
            alpha: 1.0,
            action: PetAction::Sit,
            quiet: 2.0,
            last_caret: Some((5, 16)),
            last_now: Some(now),
            ..PetBrain::default()
        };
        let mut scene = Self { term, pet, now };
        scene.frame(0.0);
        scene
    }

    fn frame(&mut self, seconds: f32) -> PetFrame {
        self.now += Duration::from_secs_f32(seconds);
        let input = self.term.cell_frame(20, 80);
        let facts = PetWorldFacts::read(&self.term, 17);
        self.pet
            .observe_console(&input, &facts, PetPane::full(&input));
        self.pet.set_console_presentable(true);
        let c = self.term.cursor();
        self.pet.tick(PetSense {
            caret_drawn: true,
            now: self.now,
            caret: (self.term.cursor_visible() && self.term.grid().display_offset() == 0)
                .then_some((c.row, c.col)),
            wrapped: false,
            rows: 20,
            cols: 80,
            cell_w: 10,
            cell_h: 20,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
        })
    }

    fn input(&mut self, kind: PetInputKind) {
        self.pet.note_console_input(self.now, kind);
    }
}

#[test]
fn contact_requires_new_local_ink_not_a_redraw_or_body_motion() {
    let mut s = Scene::new();
    s.pet.col = 20.0;
    s.pet.held_lead = 3.0;
    s.pet.console.last_body = Some(PetRect::new(4.3, 20.0, 1.7, 5.6));
    s.input(PetInputKind::Text);
    s.term.process(b"\x1b[15;3Hunrelated\x1b[6;17H");
    s.frame(0.016);
    assert_ne!(s.pet.console_attention(), PetAttention::Contact);
    s.pet.col = 20.0;
    s.pet.console.last_body = Some(PetRect::new(4.3, 20.0, 1.7, 5.6));
    s.input(PetInputKind::Text);
    s.term.process(b"\x1b[6;20HX\x1b[6;17H");
    let f = s.frame(0.016);
    assert_eq!(s.pet.console_attention(), PetAttention::Contact);
    let f = if matches!(
        f.pose,
        PetGlyphId::PetContactBrace | PetGlyphId::PetTailTuck
    ) {
        f
    } else {
        s.frame(0.016)
    };
    assert!(matches!(
        f.pose,
        PetGlyphId::PetContactBrace | PetGlyphId::PetTailTuck
    ));
    let at = s.pet.console.contact_at;
    assert_eq!(
        s.pet.console_deadline(s.now),
        at.map(|time| time + Duration::from_millis(140)),
        "the recovery pose has its own finite wake edge"
    );
    s.frame(0.016);
    assert_eq!(
        s.pet.console.contact_at, at,
        "same ink never restarts contact"
    );
    s.frame(0.20);
    assert_eq!(s.pet.last_pose, PetGlyphId::PetContactRecover);
    s.frame(1.0);
    assert_ne!(s.pet.console_attention(), PetAttention::Contact);

    let mut term = Terminal::new(4, 12);
    term.set_default_background(aterm_types::Rgb { r: 0, g: 0, b: 0 });
    term.set_palette_color_components(1, 255, 0, 0);
    let clock = aterm_core::terminal::ClockReading {
        monotonic: s.now,
        wall_ms: Some(1_000),
    };
    term.process_at(
        "abc \u{05D0}\u{05D1}\u{05D2}\x1b[3;1H│> \x1b[41m \x1b[0mabc   │\x1b[4;1H\x1b[?25l"
            .as_bytes(),
        clock,
    );
    term.process_at(b"\x1b[2 k", clock);
    let rtl = term.cell_frame(4, 12);
    let before_facts = PetWorldFacts::read(&term, 17);
    let mut pet = PetBrain::default();
    pet.observe_console(&rtl, &before_facts, PetPane::full(&rtl));
    let before = pet.console.row_sig.clone();
    assert_eq!(pet.console.faux_caret, Some((2, 3)));

    term.process_at(b"\x1b[1 k", clock);
    let ltr = term.cell_frame(4, 12);
    let after_facts = PetWorldFacts::read(&term, 17);
    assert_eq!(before_facts.stamp, after_facts.stamp);
    assert_ne!(after_facts.stamp.content_seq, 0);
    let chars = |input: &aterm_core::render::RenderInput| {
        input.cells[0]
            .iter()
            .map(|cell| cell.ch)
            .collect::<Vec<_>>()
    };
    assert_ne!(chars(&rtl), chars(&ltr), "SCP reprojects the same row");
    pet.observe_console(&ltr, &after_facts, PetPane::full(&ltr));
    assert_ne!(pet.console.row_sig, before, "changed glass refreshes rows");
    assert_eq!(pet.console.row_sig_prev, before);
    let after = pet.console.row_sig.clone();
    pet.observe_console(&ltr, &after_facts, PetPane::full(&ltr));
    assert_eq!(pet.console.row_sig, after);
    assert_eq!(
        pet.console.row_sig_prev, before,
        "a duplicate read retains the previous different rows"
    );

    assert_ne!(ltr.cells[2][3].bg, ltr.cells[2][1].bg);
    term.set_palette_color(1, term.default_background());
    let recoloured = term.cell_frame(4, 12);
    let recoloured_facts = PetWorldFacts::read(&term, 17);
    assert_eq!(after_facts.stamp, recoloured_facts.stamp);
    assert_eq!(recoloured.cells[2][3].bg, recoloured.cells[2][1].bg);
    pet.observe_console(&recoloured, &recoloured_facts, PetPane::full(&recoloured));
    assert_eq!(pet.console.row_sig, after);
    assert_eq!(
        pet.console.faux_caret,
        Some((2, 7)),
        "palette changes revalidate the painted caret"
    );

    let pane = PetPane {
        row: 2,
        col: 0,
        rows: 2,
        cols: 12,
    };
    let mut pane_facts = PetWorldFacts::read(&term, 17);
    pane_facts.stamp.surface.rows = pane.rows;
    for (available, signatures) in [(4, 2), (3, 1), (1, 0), (2, 0), (0, 0), (4, 2)] {
        let mut input = term.cell_frame(4, 12);
        input.cells.truncate(available);
        for _ in 0..2 {
            pet.observe_console(&input, &pane_facts, pane);
            assert_eq!(pet.console.row_sig.len(), signatures);
            let world = pet.console_world().unwrap();
            assert_eq!(world.stamp(), Some(pane_facts.stamp));
            if available < 4 {
                assert_eq!(world.cell(1, 0), PetCell::Unknown);
            }
            if available < 3 {
                assert_eq!(world.cell(0, 0), PetCell::Unknown);
                assert!(pet.console.row_sig_prev.is_empty());
            }
        }
    }
}

/// A CAT AHEAD OF THE WRITING WAITS AT REST. Typed toward a cat standing a few
/// cells ahead of its stand, the escort waits for the writing to reach it
/// instead of turning back to meet it; kept in its gait while it waited, it
/// stood frozen in one frame of its stride, legs apart, until the writing came.
/// It waits at rest now: no frame draws a gait on a body that did not move.
///
/// NEGATIVE CONTROL (as run before the fix): every frame of the wait a walk
/// frame at a standstill.
#[test]
fn a_cat_ahead_of_the_writing_waits_at_rest() {
    let mut s = Scene::new();
    s.pet.col = 20.0;
    s.pet.console.last_body = Some(PetRect::new(4.3, 20.0, 1.7, 5.6));
    s.input(PetInputKind::Text);
    s.term.process(b"\x1b[6;20HX\x1b[6;17H");
    let gait = |p: PetGlyphId| {
        let id = PET_GLYPHS[p as usize].id;
        ["pet_walk_", "pet_lwalk_", "pet_run_"]
            .iter()
            .any(|g| id.starts_with(g))
    };
    let mut col = s.pet.col;
    let mut frozen = Vec::new();
    for k in 0..20 {
        let f = s.frame(0.016);
        if gait(f.pose) && (f.col - col).abs() < 1e-3 {
            frozen.push((k, f.action, f.pose));
        }
        col = f.col;
    }
    assert!(
        frozen.is_empty(),
        "a gait drawn on a body that did not move (frame, action, pose): {frozen:?}"
    );
}

/// THE CAT RIDES ITS BOX. A transcript that grows over an agent's composer
/// pushes the composer down the screen, no key behind it; a cat resting beside
/// its prompt is carried down with it, in the frame the rows move, as a host's
/// regrid carries it. Hopped after the prompt instead, it followed Claude
/// Code's composer down with a flight every time a reply's line pushed it.
///
/// NEGATIVE CONTROL (as run before the fix): the cat stayed on its row as the
/// box went down two, and hopped after it.
#[test]
fn the_cat_rides_its_box_down() {
    let mut s = Scene::new();
    let rule = "\u{2500}".repeat(80);
    let screen = |lines: usize| {
        let mut out = String::from("\x1b[2J\x1b[H");
        for r in 0..lines {
            out.push_str(&format!(
                "\x1b[{};1H\u{23fa} reply line {r} of the answer",
                r + 1
            ));
        }
        let top = lines + 2;
        out.push_str(&format!(
            "\x1b[{top};1H{rule}\x1b[{};1H\u{276f} \x1b[{};1H{rule}\x1b[{};1H  footer\x1b[{};3H",
            top + 1,
            top + 2,
            top + 3,
            top + 1
        ));
        out
    };
    s.term.process(screen(2).as_bytes());
    for _ in 0..(3.0 / 0.016) as usize {
        s.frame(0.016);
    }
    let before = s.frame(0.016);
    assert!(
        before.action.settled() && (before.row - 4.0).abs() < 0.01,
        "fixture: at rest beside the prompt on row 4: {before:?}"
    );
    s.term.process(screen(4).as_bytes());
    let carried = s.frame(0.016);
    assert!(
        (carried.row - 6.0).abs() < 0.01 && (carried.col - before.col).abs() < 1e-3,
        "carried down two rows with the box, in its frame: {carried:?}"
    );
    let leapt = (0..(1.0 / 0.016) as usize)
        .map(|_| s.frame(0.016))
        .filter(|f| f.action == PetAction::Leap)
        .count();
    assert_eq!(leapt, 0, "no flight after the box");
}

/// THE RE-GRID A CHANGE OF DISPLAY SCALE MAKES KEEPS THE CAT ON ITS CONSOLE SEAT
/// ([`PetBrain::keep_place_on_restation`]): asleep on the seat the console keeps for it in
/// Claude Code's pane (under the composer's footer, row 16.85, its centre on column 6), the
/// pane's cells changed from 7x14 px to 15x28 and back, its grid and caret unchanged. The art
/// is 5.61 columns wide on the one and 5.23 on the other ([`art_cols`]), and the seat stays
/// centred, so its left edge moves by half the change (0.19 columns). The cat moves with it on
/// the frame that draws the new cells, its centre within a pixel of where it was, and from
/// there is as still as the same pane given no re-grid, frame for frame in pose, alpha and
/// row. Left where it was, it glided the 0.19 columns over the next 120 frames.
#[test]
fn a_scale_change_keeps_the_cat_on_its_console_seat() {
    for (from, to) in [((7u16, 14u16), (15u16, 28u16)), ((15, 28), (7, 14))] {
        let pane = || {
            let mut p = AgentPane::new_warm(0);
            p.2.cell = from;
            p.1 = Some(PetRect::new(12.0, 0.0, 1.0, 100.0));
            for _ in 0..(30.0 / 0.016) as usize {
                p.frame();
            }
            p
        };
        let (mut scaled, mut control) = (pane(), pane());
        let before = scaled.frame();
        let _ = control.frame();
        let (w0, w1) = (art_cols(from.0, from.1), art_cols(to.0, to.1));
        let caret_row = f32::from(scaled.0.term.cursor().row);
        assert!(
            before.action == PetAction::Sleep
                && before.alpha == 255
                && (before.row - caret_row).abs() >= 0.5
                && scaled.0.pet.aim == Some((before.col, before.row)),
            "fixture: {from:?}: asleep on its seat off the caret's row, got {:?} at ({}, {}) \
             alpha {}, aim {:?}",
            before.action,
            before.col,
            before.row,
            before.alpha,
            scaled.0.pet.aim
        );
        scaled.2.cell = to;
        scaled.0.pet.rebase_geometry();
        let px = 1.0 / f32::from(to.0);
        let frames: Vec<(PetFrame, PetFrame)> = (0..400)
            .map(|_| (scaled.frame(), control.frame()))
            .collect();
        let (centre_was, centre_now) = (before.col + w0 / 2.0, frames[0].0.col + w1 / 2.0);
        assert!(
            (centre_now - centre_was).abs() < px,
            "{from:?} to {to:?}: the seat's centre moved {:.3} columns at the re-grid",
            centre_now - centre_was
        );
        let offset = frames[0].0.col - frames[0].1.col;
        for (i, (s, c)) in frames.iter().enumerate() {
            assert!(
                s.action == c.action && s.pose == c.pose && s.alpha == c.alpha && s.row == c.row,
                "{from:?} to {to:?}: frame {i} re-posed: {:?} {:?} alpha {} row {} against \
                 {:?} {:?} alpha {} row {}",
                s.action,
                s.pose,
                s.alpha,
                s.row,
                c.action,
                c.pose,
                c.alpha,
                c.row
            );
            assert!(
                (s.col - c.col - offset).abs() < px,
                "{from:?} to {to:?}: frame {i} moved the cat: col {:.4} against {:.4} with no \
                 re-grid, where the re-grid's frame stood {offset:.4} apart",
                s.col,
                c.col
            );
        }
    }
}

#[test]
fn a_walking_body_wears_its_gait_through_a_contact() {
    let mut s = Scene::new();
    // Behind its stand by a walk: a body ahead of the caret waits for the writing to
    // come to it, at rest.
    s.pet.col = 8.0;
    s.pet.console.last_body = Some(PetRect::new(4.3, 8.0, 1.7, 5.6));
    s.input(PetInputKind::Text);
    s.term.process(b"\x1b[6;10HX\x1b[6;17H");
    let f = s.frame(0.016);
    assert_eq!(s.pet.console_attention(), PetAttention::Contact);
    let gait = |p: PetGlyphId| {
        matches!(
            p,
            PetGlyphId::PetWalk0
                | PetGlyphId::PetWalk1
                | PetGlyphId::PetWalk2
                | PetGlyphId::PetWalk3
                | PetGlyphId::PetLwalk0
                | PetGlyphId::PetLwalk1
                | PetGlyphId::PetLwalk2
                | PetGlyphId::PetLwalk3
                | PetGlyphId::PetRun0
                | PetGlyphId::PetRun1
                | PetGlyphId::PetRun2
                | PetGlyphId::PetRun3
        )
    };
    assert_eq!(f.action, PetAction::Walk, "fixture: the chase walks it");
    assert!(
        gait(f.pose),
        "a walking body wears its gait, not {:?}",
        f.pose
    );
    for _ in 0..20 {
        let f = s.frame(0.016);
        if matches!(f.action, PetAction::Walk | PetAction::Run) {
            assert!(gait(f.pose), "through the hold too, not {:?}", f.pose);
        }
    }
}

#[test]
fn repair_holds_its_body_in_the_inspect_pose() {
    let mut s = Scene::new();
    s.input(PetInputKind::Delete);
    s.term.process(b"\x1b[6;15H");
    let inspect = s.frame(0.016);
    assert_eq!(s.pet.console_attention(), PetAttention::Editing);
    assert_eq!(inspect.pose, PetGlyphId::PetInspectDown);
    let before = (inspect.col, inspect.row);
    s.frame(0.016);
    assert_eq!((s.pet.col, s.pet.row), before);
}

#[test]
fn held_selection_has_real_presence_without_a_caret_and_stops_waking() {
    let mut s = Scene::new();
    s.term.process(b"\x1b[3;3Hread this\x1b[?25l");
    s.term
        .text_selection_mut()
        .start_selection(2, 2, SelectionSide::Left, SelectionType::Simple);
    s.term
        .text_selection_mut()
        .update_selection(2, 10, SelectionSide::Right);
    let first = s.frame(0.016);
    assert_eq!(s.pet.console_attention(), PetAttention::Reading);
    assert!(first.alpha > 0);
    assert!(
        s.pet.last_caret.is_none(),
        "selection never fabricates a cursor"
    );
    let next = s.frame(2.0);
    assert_eq!(
        first.fp(),
        next.fp(),
        "stable reading has no background performance"
    );
    assert!(!s.pet.needs_frames());
    assert!(s.pet.next_change_deadline(s.now).is_none());
}

#[test]
fn an_incoherent_observation_never_falls_back_to_uncertified_legacy_placement() {
    let incoherent = |s: &mut Scene, ms: u64| {
        let input = s.term.cell_frame(20, 80);
        s.term.process(b"changed");
        let facts = PetWorldFacts::read(&s.term, 17);
        s.pet.observe_console(&input, &facts, PetPane::full(&input));
        s.now += Duration::from_millis(ms);
        s.pet.tick(PetSense {
            caret_drawn: true,
            now: s.now,
            caret: Some((5, 23)),
            wrapped: false,
            rows: 20,
            cols: 80,
            cell_w: 10,
            cell_h: 20,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
        })
    };
    let mut s = Scene::new();
    let before = s.frame(0.016);
    assert_eq!(before.alpha, 255, "fixture: the resident is on glass");
    let f = incoherent(&mut s, 16);
    assert_eq!(s.pet.console_reason(), "incoherent-surface");
    assert_eq!(
        (f.alpha, f.lane_alpha),
        (before.alpha, before.lane_alpha),
        "the body on glass is held, not cut"
    );
    assert_eq!(
        (f.col, f.row),
        (before.col, before.row),
        "…exactly where the last certified frame judged it"
    );
    let mut last = f;
    for _ in 0..80 {
        last = incoherent(&mut s, 16);
    }
    assert_eq!(last.alpha, 0, "an unbroken blind run fades the body out");
    assert_eq!((last.col, last.row), (before.col, before.row));

    let mut cold = Scene::new();
    cold.pet.console.on_glass = false;
    let f = incoherent(&mut cold, 16);
    assert_eq!((f.alpha, f.lane_alpha), (0, 0));
    assert_eq!(cold.pet.console_reason(), "incoherent-surface");
}

#[test]
fn progress_baselines_each_command_and_duplicate_reports_are_not_events() {
    let mut s = Scene::new();
    s.term.process(
        b"\x1b]9;4;1;100\x07\x1b[8;1H\x1b]133;A\x07> \x1b]133;B\x07build\r\n\x1b]133;C\x07working",
    );
    s.frame(0.1);
    assert_eq!(
        s.pet.console_attention(),
        PetAttention::Output,
        "old100 is a baseline, not completion"
    );
    s.term.process(b"\x1b]9;4;1;20\x07");
    s.frame(0.1);
    assert_eq!(s.pet.console_attention(), PetAttention::Progress);
    let at = s.pet.console.progress_at;
    s.term.process(b"\x1b]9;4;1;20\x07");
    s.frame(0.1);
    assert_eq!(s.pet.console.progress_at, at);
    s.frame(0.4);
    assert_eq!(s.pet.console_attention(), PetAttention::Output);
    assert_ne!(s.pet.console_attention(), PetAttention::Result);
}

#[test]
fn suppressed_failure_still_hushes_celebration_for_a_finite_window() {
    let mut s = Scene::new();
    s.input(PetInputKind::Text);
    s.pet.note_command_done(s.now, true, Some(100));
    s.frame(0.016);
    assert_eq!(s.pet.console_attention(), PetAttention::Typing);
    assert!(s.pet.grieving(), "no celebration over a fresh failed exit");
    s.frame(1.3);
    assert!(
        !s.pet.grieving(),
        "suppressed reactions leave no unbounded hush"
    );
    assert_ne!(s.pet.console_attention(), PetAttention::Result);
}

#[test]
fn a_spent_content_perch_hands_the_pet_back_its_own_life() {
    let mut s = Scene::new();
    s.term
        .process(b"\x1b[8;1H\x1b]133;A\x07> \x1b]133;B\x07build\r\n\x1b]133;C\x07working");
    s.frame(0.1);
    assert_eq!(s.pet.console_attention(), PetAttention::Output);
    s.term.process(b"\x1b]133;D;0\x07");
    s.frame(0.1);
    assert_eq!(
        s.pet.console_attention(),
        PetAttention::Exploring,
        "fixture: the surviving anchor is the content perch"
    );
    for _ in 0..240 {
        s.frame(0.016);
        if !s.pet.console.moving && !s.pet.needs_frames() {
            break;
        }
    }
    assert!(
        s.pet.needs_frames() || s.pet.next_change_deadline(s.now).is_some(),
        "a resident with nothing left to react to must still own a future"
    );
    for _ in 0..((PERCH_DWELL / 0.016).ceil() as usize + 2) {
        s.frame(0.016);
    }
    assert_eq!(
        s.pet.console_attention(),
        PetAttention::Rest,
        "a spent perch is released, not held forever"
    );
    assert!(
        !s.pet.console.resident,
        "a released perch is no longer a console resident"
    );
}

#[test]
fn no_perch_in_reach_is_not_an_opinion_about_where_the_pet_belongs() {
    let mut s = Scene::new();
    for row in 1..=20 {
        let line = format!("\x1b[{row};1H\x1b#6{}", "X".repeat(40));
        s.term.process(line.as_bytes());
    }
    s.term.process(b"\x1b[6;17H");
    s.frame(0.016);
    let width = art_cols(10, 20);
    assert_eq!(
        s.pet.console_station((70.0, 5.0), (20, 80), width),
        None,
        "a dense screen offers no perch, and that is not a station"
    );
    let here = (s.pet.col, s.pet.row);
    assert_ne!(
        s.pet.station_safe((5, 70), 80, 20, width),
        here,
        "the baseline escort must still answer when the console cannot"
    );

    for row in [15, 16] {
        let line = format!("\x1b[{row};1H\x1b#5\x1b[2K\x1b[{row};9H{}", "X".repeat(72));
        s.term.process(line.as_bytes());
    }
    s.term.process(b"\x1b[6;17H");
    s.frame(0.016);
    s.pet.alpha = 0.0;
    s.pet.flight = None;
    s.pet.console.rest_seat.set(None);
    let desired = (70.0, 5.0);
    let body = PetBrain::console_stand_rect(desired, 20, width);
    let world = s.pet.console.world.as_ref().expect("observed pane");
    assert_eq!(
        world.nearest_clear_perch(body, CLEARANCE, REST_REACH, None),
        None,
        "the nearby search cannot reach the clear strip"
    );
    let seat = s
        .pet
        .console_station(desired, (20, 80), width)
        .expect("clear ground elsewhere in the pane");
    let seated = PetBrain::console_stand_rect(seat, 20, width);
    assert!(
        (2.0 * (seated.row - body.row)).hypot(seated.col - body.col) > REST_REACH,
        "the fallback reaches beyond the nearby search: {seated:?}"
    );
    assert!(
        world.ground_clear(seated, CLEARANCE),
        "the distant seat must still be certified clear ground"
    );
}

#[test]
fn a_stream_the_cat_is_staring_at_is_not_taken_by_the_output_hold() {
    let mut s = Scene::new();
    s.executing();
    s.pet.watch_heat = 1.0;
    let frames = s.watch(0.3);
    assert!(
        s.pet.watch_heat >= WATCH_GATE,
        "the heat only cools, it is not taken: {:.2}",
        s.pet.watch_heat
    );
    assert!(
        frames.iter().all(|f| f.action == PetAction::Perk),
        "perked at the stream throughout: {:?}",
        frames.iter().map(|f| f.action).collect::<Vec<_>>()
    );
    assert!(!s.pet.console.resident, "the hold stood aside");
}

#[test]
fn a_late_wake_read_still_offers_the_unconsumed_expiry() {
    let mut s = Scene::new();
    s.input(PetInputKind::Delete);
    s.frame(0.016);
    assert_eq!(s.pet.console_attention(), PetAttention::Editing);
    let late = s.now + Duration::from_secs(5);
    let deadline = s
        .pet
        .next_change_deadline(late)
        .expect("expired pose still owes one tick");
    assert!(deadline >= late && deadline <= late + Duration::from_millis(20));
    s.frame(5.0);
    assert_ne!(s.pet.console_attention(), PetAttention::Editing);
}

fn hidden_home_continuity_model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        HiddenCursorHomeContinuity {
            const Buggy = 0;
            var home = 1;
            var hidden = 0;
            var event = 0;
            action KnownMove when (home == 1) {
                hidden = 1;
                home = if Buggy == 1 { 0 } else { 1 };
                event = 1;
            }
            action UnknownMove when (home <= 1) {
                hidden = 1; home = 0; event = 2;
            }
            action Observe when (home <= 1) {
                hidden = 0; home = 1; event = 0;
            }
            invariant Bounds: home <= 1 && hidden <= 1 && event <= 2;
            invariant KnownKeepsHome: event == 0 || event == 2 || home == 1;
            invariant UnknownClearsHome: event <= 1 || home == 0;
            invariant VisibleHasHome: hidden == 1 || home == 1;
        }
    }
}

#[test]
fn hidden_cursor_home_continuity_conforms_to_real_terminal_motion() {
    use aterm_core::terminal::{ContentScrollDelta, ContentScrollState};
    use std::collections::BTreeMap;
    let model = hidden_home_continuity_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    let known: &[&[u8]] = &[
        b"",
        b"\x1b[1;18r\x1b[18;1H\n",
        b"\x1b[3;18r\x1b[8;1H\x1b[2L",
        b"\x1b[3;18r\x1b[8;1H\x1b[2M",
        b"\x1b[3;18r\x1b[3;1H\x1bM",
        b"\x1b[20;1H\n",
    ];
    for motion in known {
        let mut s = Scene::new();
        let home = s
            .pet
            .console
            .cursor_home
            .expect("the visible fixture has a home");
        let input_seq = s.pet.console_input_seq();
        let before = s.term.content_scroll_state();
        let mut bytes = b"\x1b[?25l\x1b7".to_vec();
        bytes.extend_from_slice(motion);
        bytes.extend_from_slice(b"\x1b[r\x1b8");
        s.term.process(&bytes);
        assert!(!matches!(
            ContentScrollState::delta_since(Some(before), s.term.content_scroll_state()),
            ContentScrollDelta::Invalidate | ContentScrollDelta::Baseline
        ));
        s.frame(0.016);
        let actual = BTreeMap::from([
            ("home", i64::from(s.pet.console.cursor_home.is_some())),
            ("hidden", i64::from(!s.term.cursor_visible())),
            ("event", 1),
        ]);
        assert!(
            model
                .successors("KnownMove", &model.init_state())
                .contains(&actual)
        );
        assert_eq!(
            s.pet.console.cursor_home,
            Some(home),
            "home must not follow moved text"
        );
        assert_eq!(s.pet.console_input_seq(), input_seq);
        let mut historical = actual.clone();
        historical.insert("home", 0);
        assert!(!model.check_invariant("KnownKeepsHome", &historical));
        assert!(
            !model
                .successors("KnownMove", &model.init_state())
                .contains(&historical)
        );
    }
    for fault in 0..7 {
        let mut s = Scene::new();
        let before = s.term.content_scroll_state();
        s.term.process(b"\x1b[?25l");
        match fault {
            0 => s.term.process(b"\x1bc\x1b[?25l"),
            1 => s.term.process(b"\x1b[?1049h\x1b[?25l"),
            2 => s
                .term
                .process(b"\x1b[?69h\x1b[2;6s\x1b[2;18r\x1b[18;1H\x1b[S"),
            3 => {
                for _ in 0..17 {
                    s.term.process(b"\x1b[1;18r\x1b[18;1H\n");
                }
            }
            4 => s.term.resize(21, 81),
            5 => {
                s.term = Terminal::new(20, 80);
                s.term.process(b"\x1b[?25l");
            }
            _ => {}
        }
        if fault <= 3 {
            assert!(
                matches!(
                    ContentScrollState::delta_since(Some(before), s.term.content_scroll_state()),
                    ContentScrollDelta::Invalidate
                ),
                "fault {fault} must actually invalidate the terminal motion reader"
            );
        }
        let rows = s.term.grid().rows();
        let cols = s.term.grid().cols();
        let input = s.term.cell_frame(usize::from(rows), usize::from(cols));
        let facts = PetWorldFacts::read(&s.term, if fault == 6 { 18 } else { 17 });
        s.pet.observe_console(&input, &facts, PetPane::full(&input));
        assert!(s.pet.console.world.as_ref().unwrap().stamp().is_some());
        s.pet.set_console_presentable(true);
        s.now += Duration::from_millis(16);
        let input_seq = s.pet.console_input_seq();
        s.pet.tick(PetSense {
            caret_drawn: true,
            now: s.now,
            caret: None,
            wrapped: false,
            rows,
            cols,
            cell_w: 10,
            cell_h: 20,
            reduced_motion: true,
            output_burst: false,
            pointer: None,
        });
        let actual = BTreeMap::from([
            ("home", i64::from(s.pet.console.cursor_home.is_some())),
            ("hidden", i64::from(!s.term.cursor_visible())),
            ("event", 2),
        ]);
        assert!(
            model
                .successors("UnknownMove", &model.init_state())
                .contains(&actual),
            "fault {fault}: {actual:?}"
        );
        assert_eq!(s.pet.console_input_seq(), input_seq);
        let mut leaked = actual;
        leaked.insert("home", 1);
        assert!(!model.check_invariant("UnknownClearsHome", &leaked));
    }

    let mut s = Scene::new();
    let state_of = |s: &Scene, event: i64| {
        BTreeMap::from([
            ("home", i64::from(s.pet.console.cursor_home.is_some())),
            ("hidden", i64::from(!s.term.cursor_visible())),
            ("event", event),
        ])
    };
    let mut model_state = model.init_state();
    assert_eq!(state_of(&s, 0), model_state);

    s.term
        .process(b"\x1b[?25l\x1b7\x1b[1;18r\x1b[18;1H\n\x1b[r\x1b8");
    s.frame(0.016);
    let next = state_of(&s, 1);
    assert!(model.successors("KnownMove", &model_state).contains(&next));
    model_state = next;

    s.term.process(b"\x1b[?1049h\x1b[?25l");
    s.frame(0.016);
    let next = state_of(&s, 2);
    assert!(
        model
            .successors("UnknownMove", &model_state)
            .contains(&next)
    );
    assert!(
        model.successors("KnownMove", &next).is_empty(),
        "KnownMove is disabled once home is cleared"
    );
    model_state = next;

    s.term.process(b"\x1b[6;9H\x1b[?25h");
    s.frame(0.016);
    let next = state_of(&s, 0);
    assert!(model.successors("Observe", &model_state).contains(&next));
    assert_eq!(s.pet.console.cursor_home, Some((5, 8)));
    model_state = next;

    s.term.process(b"\x1b[?25l\x1b7\x1b[20;1H\n\x1b8");
    s.frame(0.016);
    let next = state_of(&s, 1);
    assert!(model.successors("KnownMove", &model_state).contains(&next));
    assert_eq!(s.pet.console.cursor_home, Some((5, 8)));
}

#[test]
fn no_caret_retirement_settles_pending_visibility_runs_and_releases_frames() {
    let mut s = Scene::new();
    s.pet.col = 17.0;
    for _ in 0..10 {
        s.frame(5.0);
    }
    assert_eq!(s.pet.action(), PetAction::Sleep);
    assert!(s.pet.console.on_glass);
    assert!(s.pet.console.hold.settled());

    let _ = s.pet.console.hold.update(s.now, None);
    assert!(!s.pet.console.hold.settled());
    assert!(s.pet.needs_frames());

    s.term.process(b"\x1b[?1049h\x1b[?25l");
    for _ in 0..35 {
        s.frame(0.016);
    }
    s.frame(5.0);
    assert_eq!(s.pet.alpha, 0.0);
    assert!(
        s.pet.console.hold.settled(),
        "idle emission must clear pending visibility runs once the body fades off glass"
    );
    assert!(
        !s.pet.needs_frames(),
        "a faded-out sleeper must not stay on the 60 fps train"
    );
}
#[test]
fn a_look_swaps_arrival_ramp_runs_while_the_console_resident_owns_the_tick() {
    let mut s = Scene::new();
    s.term.process(b"\x1b[3;3Hread this\x1b[?25l");
    s.term
        .text_selection_mut()
        .start_selection(2, 2, SelectionSide::Left, SelectionType::Simple);
    s.term
        .text_selection_mut()
        .update_selection(2, 10, SelectionSide::Right);
    let settled = s.frame(0.016);
    assert_eq!(s.pet.console_attention(), PetAttention::Reading);
    assert_eq!(settled.alpha, 255, "the resident is opaque before the swap");
    s.pet.arrive_t = HOMECOMING_IN;
    s.pet.arrive_in = HOMECOMING_IN;
    assert!(
        s.pet.needs_frames(),
        "a ramp in flight owes frames even when the resident has nothing else to draw"
    );
    let first = s.frame(0.016);
    assert_eq!(s.pet.console_attention(), PetAttention::Reading);
    assert!(
        first.alpha < 64,
        "the incoming body starts faint ({}), the ghost carries the crossfade",
        first.alpha
    );
    let mut ticks = 0;
    let mut alpha = first.alpha;
    while alpha < 255 && ticks < 40 {
        alpha = s.frame(0.016).alpha;
        ticks += 1;
        assert_eq!(s.pet.console_attention(), PetAttention::Reading);
    }
    let budget = (HOMECOMING_IN / 0.016).ceil() as u32 + 2;
    assert!(
        alpha == 255 && ticks <= budget,
        "the ramp must finish in HOMECOMING_IN under a resident-owned tick: alpha {alpha} after {ticks} ticks (budget {budget})"
    );
}

#[test]
fn a_departing_ghost_over_ordinary_ink_survives_and_over_a_selection_does_not() {
    let mut s = Scene::new();
    s.term
        .process(b"\x1b[5;21Hxxxxxxxx\x1b[6;21Hxxxxxxxx\x1b[6;17H");
    let ghost = |s: &Scene| Departure {
        born: s.pet.lane_clock,
        id: 0,
        coat: 1,
        iris: 1,
        from_col: s.pet.col,
        row: s.pet.row,
        edge: s.pet.col,
        speed: DEPART_SPEED,
        still: true,
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
    };
    s.pet.departures[0] = Some(ghost(&s));
    let f = s.frame(0.016);
    assert!(f.alpha > 0, "the live body stands under the ink");
    assert!(
        f.departures[0].is_some(),
        "ordinary ink under the ghost is a z-order fact, not a cull"
    );
    s.term
        .text_selection_mut()
        .start_selection(4, 20, SelectionSide::Left, SelectionType::Simple);
    s.term
        .text_selection_mut()
        .update_selection(5, 30, SelectionSide::Right);
    s.pet.departures[0] = Some(ghost(&s));
    let f = s.frame(0.016);
    assert!(f.alpha > 0, "the body stepped aside rather than hiding");
    let d = f.departures[0]
        .expect("the still ghost stepped aside with the body instead of being culled on its own");
    assert!(
        (d.col - f.col).abs() < 1e-3 && (d.row - f.row).abs() < 1e-3,
        "…to the very same stand ({}, {}) vs ({}, {})",
        d.col,
        d.row,
        f.col,
        f.row
    );
    s.pet.departures[0] = Some(Departure {
        still: false,
        edge: 0.0,
        from_col: 20.0,
        row: 5.0,
        ..ghost(&s)
    });
    let f = s.frame(0.016);
    assert!(f.alpha > 0, "the body is still drawn");
    assert!(
        f.departures[0].is_none(),
        "a selection under a running ghost culls it at once, as it culls the body"
    );
}

#[test]
fn a_ghost_on_its_own_glass_needs_its_own_worlds_admission() {
    let switch = |select: bool| {
        let mut s = Scene::new();
        s.pet.worn = Some((1, 1));
        let before = s.frame(0.016);
        assert_eq!(before.alpha, 255, "fixture: on glass");
        if select {
            s.term.text_selection_mut().start_selection(
                4,
                20,
                SelectionSide::Left,
                SelectionType::Simple,
            );
            s.term
                .text_selection_mut()
                .update_selection(5, 30, SelectionSide::Right);
            let input = s.term.cell_frame(20, 80);
            let facts = PetWorldFacts::read(&s.term, 17);
            s.pet.observe_console(&input, &facts, PetPane::full(&input));
        }
        s.pet.resight_coordinate_space();
        s.pet.displace_resight(1);
        let kept = s.pet.has_own_glass_ghost();
        s.term.process(b"\x1b[15;40H");
        (kept, before, s.frame(0.016))
    };
    let (kept, before, f) = switch(false);
    assert!(kept, "clear ground admits the ghost");
    let d = f.departures[0].expect("the ghost is spent on its own glass");
    assert!(d.own_glass && d.under_ink, "{d:?}");
    assert_eq!(
        (d.col, d.row),
        (before.col, before.row),
        "it stands at the old body's cell, whatever the new world holds there"
    );
    assert!(f.alpha > 0, "beside the arriver");

    let (_, _, f) = switch(true);
    assert!(
        f.departures.iter().all(Option::is_none),
        "control: a selection under the body refuses it"
    );
}

#[test]
fn a_carried_ghost_keeps_what_its_own_slot_drew() {
    let carry = |drawn: f32| {
        let mut s = Scene::new();
        s.pet.departures[1] = Some(Departure {
            born: s.pet.lane_clock,
            id: 0,
            coat: 1,
            iris: 1,
            from_col: s.pet.col,
            row: s.pet.row,
            edge: s.pet.col,
            speed: DEPART_SPEED,
            still: true,
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
        s.pet.last_drawn = [0.0; PET_DEPARTURES_MAX];
        s.pet.last_drawn[1] = drawn;
        s.pet.resight_coordinate_space();
        s.pet
            .carried
            .iter()
            .flatten()
            .map(|c| c.presence)
            .collect::<Vec<_>>()
    };
    assert_eq!(carry(0.4), [0.4], "carried at what its slot drew");
    assert!(carry(0.0).is_empty(), "control: a ghost that drew nothing");
}

#[test]
fn a_look_swap_landing_under_a_hidden_caret_resident_keeps_both_halves_of_the_crossfade() {
    let mut s = Scene::new();
    assert_eq!(
        s.pet.sync_look((3, 1), PetArrival::Ceremony).worn,
        (3, 1),
        "the first dress applies"
    );
    s.frame(0.016);
    assert_eq!(
        s.pet.sync_look((9, 4), PetArrival::Quiet).worn,
        (3, 1),
        "fixture: the homecoming parks"
    );
    let mut landing = None;
    for _ in 0..400 {
        let f = s.frame(0.016);
        if s.pet.sync_look((9, 4), PetArrival::Quiet).worn == (9, 4) {
            landing = Some(f);
            break;
        }
    }
    let landing = landing.expect("the debounced homecoming landed");
    assert_eq!(
        landing.lane_alpha, 255,
        "the lane is at full presence at the swap"
    );
    assert_eq!(
        landing.departures.iter().flatten().count(),
        1,
        "the swap spawned its ghost"
    );
    let pre = u32::from(landing.lane_alpha);
    s.term.process(b"\x1b[?25l");
    let mut ticks = 0u32;
    let mut resident_ticks = 0u32;
    loop {
        let f = s.frame(0.016);
        ticks += 1;
        if s.pet.console.resident {
            resident_ticks += 1;
        }
        let cover = u32::from(f.lane_alpha);
        let ghost = f
            .departures
            .iter()
            .flatten()
            .map(|d| u32::from(d.alpha) * cover / 255)
            .max()
            .unwrap_or(0);
        let arriver = u32::from(f.alpha);
        assert!(
            ghost.max(arriver) * 2 + 2 >= pre * cover / 255,
            "tick {ticks}: max(ghost {ghost}, arriver {arriver}) fell under half the \
             pre-swap alpha × cover ({pre} × {cover}/255)"
        );
        let composite = 255 - (255 - ghost) * (255 - arriver) / 255;
        assert!(
            composite * 100 >= pre * cover / 255 * 70,
            "tick {ticks}: composite {composite} under 70 % of the pre-swap cover {cover}"
        );
        if s.pet.arrive_t <= 0.0 && s.pet.departures.iter().all(Option::is_none) {
            break;
        }
        assert!(ticks < 60, "the crossfade is a quarter second, not a stall");
    }
    assert!(
        resident_ticks + 1 >= ticks,
        "fixture: the hidden caret handed the resident every tick of the crossfade \
         ({resident_ticks} of {ticks})"
    );
}

#[test]
fn a_departing_ghost_over_a_protected_line_fades_with_the_body_instead_of_vanishing() {
    let mut s = Scene::new();
    s.term.process(b"\x1b[9;1H\x1b#6\x1b[10;1H\x1b#6\x1b[6;17H");
    s.frame(0.016);
    let ghost = Departure {
        born: s.pet.lane_clock,
        id: 0,
        coat: 1,
        iris: 1,
        from_col: s.pet.col,
        row: s.pet.row + 3.0,
        edge: s.pet.col,
        speed: DEPART_SPEED,
        still: true,
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
    };
    s.pet.departures[0] = Some(ghost);
    let f = s.frame(0.016);
    assert!(f.alpha > 0, "the live body stands clear");
    let world = s.pet.console.world.as_ref().expect("observed");
    let d = f.departures[0].expect(
        "a protected line under the ghost is the body's dwell-and-fade, never the ghost's cull",
    );
    let (x0, x1, y0, y1) = PetFrame {
        alpha: d.alpha,
        col: d.col,
        row: d.row,
        lift: 0.0,
        ..f
    }
    .body_px(10, 20, 80, 20)
    .expect("the ghost is on the grid");
    let rect = PetRect::new(
        y0 as f32 / 20.0,
        x0 as f32 / 10.0,
        (y1 - y0) as f32 / 20.0,
        (x1 - x0) as f32 / 10.0,
    );
    assert_eq!(
        world.under_text_clearance_past_caret(rect, CLEARANCE),
        Some(false),
        "fixture: the ghost's ground is protected"
    );
    let mut frames = 1u32;
    let mut prev = d.alpha;
    while s.pet.departures[0].is_some() {
        let f = s.frame(0.016);
        frames += 1;
        if let Some(d) = s.pet.departures[0] {
            let drawn = f.departures[0].unwrap_or_else(|| {
                panic!(
                    "frame {frames}: the ghost (born {}) was culled while the body is drawn at {}",
                    d.born, f.alpha
                )
            });
            assert!(drawn.alpha <= prev, "the old coat only ever fades DOWN");
            prev = drawn.alpha;
        }
        assert!(frames < 40, "a zero-span ghost's life is EDGE_FADE");
    }
    let want = (EDGE_FADE / 0.016) as u32;
    assert!(
        frames + 2 >= want,
        "the ghost ran its whole ramp ({frames} frames, want ≥ {want})"
    );
}

impl Scene {
    fn type_text(&mut self, text: &str) {
        for ch in text.bytes() {
            self.input(PetInputKind::Text);
            self.term.process(&[ch]);
            self.frame(0.05);
        }
    }

    fn executing(&mut self) {
        self.term
            .process(b"\x1b[8;1H\x1b]133;A\x07> \x1b]133;B\x07build\r\n\x1b]133;C\x07working");
        for _ in 0..90 {
            self.frame(0.016);
            if self.pet.console.resident && !self.pet.console.moving && !self.pet.needs_frames() {
                break;
            }
        }
        assert_eq!(self.pet.console_attention(), PetAttention::Output);
        assert!(
            self.pet.console.resident,
            "fixture: the running command owns the cat"
        );
    }

    fn watch(&mut self, secs: f32) -> Vec<PetFrame> {
        (0..(secs / 0.016).ceil() as usize)
            .map(|_| self.frame(0.016))
            .collect()
    }
}

#[test]
fn a_typed_treat_frolics_with_the_console_layer_watching() {
    let mut s = Scene::new();
    s.type_text("treat ");
    assert!(
        matches!(
            s.pet.console_attention(),
            PetAttention::Typing | PetAttention::Contact
        ),
        "fixture: the last key is live — the window the cheer latch is wiped in"
    );
    s.pet.note_trick(s.now, Trick::Treat, true);
    let frames = s.watch(2.0);
    let frolic = frames
        .iter()
        .position(|f| f.action == PetAction::Frolic)
        .expect("the treat frolics");
    assert!(
        frames[..frolic].iter().any(|f| f.action == PetAction::Perk),
        "heard first"
    );
    assert!(
        matches!(
            frames[frolic].pose,
            PetGlyphId::PetPlaybow | PetGlyphId::PetBat
        ),
        "in the frolic's own frames, not a console pose: {:?}",
        frames[frolic].pose
    );
    assert!(
        frames[frolic..].iter().any(|f| f
            .motes
            .iter()
            .flatten()
            .any(|m| m.kind == PetMoteKind::Note)),
        "with its motes on glass"
    );
    assert!(
        s.pet.pending_cheer.is_none(),
        "and never through the cheer latch"
    );
}

#[test]
fn a_commanded_sleep_survives_an_executing_block() {
    let mut s = Scene::new();
    s.executing();
    s.pet.note_trick(s.now, Trick::Sleep, true);
    assert!(
        !s.pet.needs_frames(),
        "a parked resident still answers first"
    );
    s.frame(0.016);
    assert!(!s.pet.console.resident && s.pet.needs_frames());
    let frames = s.watch(3.0);
    assert!(frames.iter().any(|f| f.action == PetAction::Perk), "heard");
    let last = frames.last().expect("frames");
    assert_eq!(
        last.action,
        PetAction::Sleep,
        "asleep a second later, and still"
    );
    assert!(
        matches!(last.pose, PetGlyphId::PetSleep0 | PetGlyphId::PetSleep1),
        "in the sleeper's own frame, not the resident's: {:?}",
        last.pose
    );
    assert!(
        !s.pet.console.resident,
        "the running command does not reseat it"
    );
    assert!(!s.pet.needs_frames(), "and a sleeping cat rides the offer");
    s.term.process(b"\r\nmore output");
    s.frame(0.016);
    assert!(s.pet.trick_settle.is_none());
    let _ = s.watch(2.5);
    assert!(s.pet.console.resident, "the resident gets the cat back");
    assert_eq!(s.pet.console_attention(), PetAttention::Output);
}

#[test]
fn a_roll_plays_through_an_executing_block() {
    let mut s = Scene::new();
    s.executing();
    s.pet.note_trick(s.now, Trick::Roll, true);
    let frames = s.watch(4.5);
    let rolled = frames
        .iter()
        .filter(|f| {
            matches!(
                f.pose,
                PetGlyphId::PetRoll0 | PetGlyphId::PetRoll1 | PetGlyphId::PetRoll2
            )
        })
        .count();
    assert!(
        rolled as f32 * 0.016 > WRIGGLE_DUR - 0.2,
        "the whole roll, not a frame of it: {rolled} frames"
    );
    assert!(
        !s.pet.trick_act,
        "and the flag comes down with the performance"
    );
    assert!(
        s.pet.console.resident,
        "after which the command has its watcher back"
    );
}

#[test]
fn a_landing_cheers_over_a_running_command_and_hands_the_cat_back() {
    let mut s = Scene::new();
    s.executing();
    assert!(
        !s.pet.needs_frames(),
        "fixture: the running command's resident is parked"
    );
    s.pet.note_room_landed();
    let mut frames = Vec::new();
    let mut aloft = 0;
    for _ in 0..(5.0 / 0.016) as usize {
        frames.push(s.frame(0.016));
        aloft = aloft.max(s.pet.cheer_motes_aloft());
    }
    let frolic = frames
        .iter()
        .position(|f| f.action == PetAction::Frolic)
        .expect("the landing frolics over the running command");
    assert!(
        frames[..frolic].iter().any(|f| f.action == PetAction::Perk),
        "perked first"
    );
    assert_eq!(aloft, 3, "its three ♪/♥ in the air together");
    assert!(s.pet.pending_cheer.is_none() && !s.pet.room_cheer, "spent");
    assert!(
        s.pet.console.resident,
        "the running command has its watcher back"
    );
    assert_eq!(s.pet.console_attention(), PetAttention::Output);
    for _ in 0..240 {
        s.frame(0.016);
        if !s.pet.needs_frames() {
            break;
        }
    }
    assert!(!s.pet.needs_frames(), "and the frames stop");
}

struct AgentPane(Scene, Option<PetRect>, PaneGeom);

#[derive(Clone, Copy, Debug)]
struct PaneGeom {
    cols: u16,
    rows: u16,
    cell: (u16, u16),
}

impl PaneGeom {
    const WIDE: Self = Self {
        cols: 100,
        rows: 30,
        cell: (15, 28),
    };

    const fn chrome(self) -> [PetRect; 2] {
        let min_cols = if self.cols < 38 { self.cols } else { 38 };
        let card = min_cols as f32;
        [
            PetRect::new(12.0, self.cols as f32 - card, 1.0, card),
            PetRect::new(14.0, 0.0, 1.0, 16.0),
        ]
    }
}

impl AgentPane {
    fn new() -> Self {
        Self::new_warm(200)
    }

    fn new_warm(warm: usize) -> Self {
        Self::with_transcript(
            "\x1b[2J\x1b[1;1H \u{2590}\u{259b}\u{2588}\u{2588}\u{2588}\u{259b}\u{2588}   \
             Claude Code v2.1.304\r\n\u{259d}\u{259c}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\
             \u{2588}\u{2580}  Opus 5.5 \u{b7} API Usage Billing\r\n \u{259d}\u{259d}   \
             \u{259d}\u{259d}   /private/tmp/proj\r\n\r\n\r\n\x1b[48;2;55;55;55m\u{276f} finish the parser\
             \x1b[K\x1b[0m\r\n\r\n\
             \u{23fa} Committed and pushed the parser and its tests.\r\n  ATERM-TASK-DONE\r\n\r\n\
             \u{273b} Churned for 40s \u{b7} done 3:01 AM\r\n\r\n",
            warm,
        )
    }

    /// [`Self::new`] with the composer's upper rule held from the first frame, as aterm's
    /// footer holds the top rule it repaints with its lockup and switch (the host's line).
    /// Before the pet's window fix the caret's line here (row 13) was refused by rounding
    /// alone, so the bare pane parked the cat under the footer as this one does.
    fn hosted() -> Self {
        let mut p = Self::new_warm(0);
        p.1 = Some(PetRect::new(12.0, 0.0, 1.0, 100.0));
        for _ in 0..200 {
            p.frame();
        }
        p
    }

    fn with_transcript(transcript: &str, warm: usize) -> Self {
        Self::with_geom(transcript, warm, PaneGeom::WIDE)
    }

    fn narrow(cols: u16, cell: (u16, u16)) -> Self {
        Self::narrow_at(cols, 30, cell)
    }

    fn narrow_at(cols: u16, rows: u16, cell: (u16, u16)) -> Self {
        Self::with_geom(
            "\x1b[2J\x1b[1;1H  pushed to origin/main.\r\n\r\n\
             \u{273b} Baked for 40s \u{b7} done 1:39 PM\r\n\r\n\
             \x1b[48;2;55;55;55m\u{276f} [aterm harness] Autopilot is Land: land\x1b[K\r\n\
             \x20 remote main, defend that in a few lines\x1b[K\x1b[0m\r\n\r\n\
             \u{23fa} Nothing left: the work is committed and\r\n\
             \x20 pushed to origin/main.\r\n\r\n\
             \u{273b} Cooked for 40s \u{b7} done 1:41 PM\r\n\r\n",
            200,
            PaneGeom { cols, rows, cell },
        )
    }

    fn with_geom(transcript: &str, warm: usize, geom: PaneGeom) -> Self {
        let mut term = Terminal::new(geom.rows, geom.cols);
        term.process(b"\x1b]11;#000000\x07");
        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07claude\r\n\x1b]133;C\x07");
        term.process(transcript.as_bytes());
        let rule = "\u{2500}".repeat(usize::from(geom.cols));
        term.process(
            format!(
                "{rule}\u{276f} \r\n{rule}  \u{23f5}\u{23f5} auto mode on (shift+tab to cycle)\
                 \x1b[?25l\x1b[14;3H"
            )
            .as_bytes(),
        );
        let now = Instant::now();
        let pet = PetBrain {
            col: 6.0,
            row: 10.0,
            alpha: 1.0,
            action: PetAction::Sit,
            quiet: 2.0,
            last_caret: Some((13, 2)),
            last_now: Some(now),
            ..PetBrain::default()
        };
        let mut pane = Self(Scene { term, pet, now }, None, geom);
        for _ in 0..warm {
            pane.frame();
        }
        pane
    }

    fn parks_within(&mut self, secs: f32) -> bool {
        for _ in 0..(secs / 0.016) as usize {
            self.frame();
            if !self.0.pet.needs_frames() {
                return true;
            }
        }
        false
    }

    fn observe(&mut self) {
        let (extra, geom) = (self.1, self.2);
        let s = &mut self.0;
        let input = s
            .term
            .cell_frame(usize::from(geom.rows), usize::from(geom.cols));
        let facts = PetWorldFacts::read(&s.term, 17);
        let [card, chips] = geom.chrome();
        let chrome = [card, chips, extra.unwrap_or_default()];
        let n = if extra.is_some() { 3 } else { 2 };
        s.pet
            .observe_console_with_exclusions(&input, &facts, PetPane::full(&input), &chrome[..n]);
        s.pet.set_console_presentable(true);
    }

    fn frame(&mut self) -> PetFrame {
        self.0.now += Duration::from_millis(16);
        self.observe();
        let geom = self.2;
        let s = &mut self.0;
        let c = s.term.cursor();
        s.pet.tick(PetSense {
            caret_drawn: s.term.cursor_visible(),
            now: s.now,
            caret: Some((c.row, c.col)),
            wrapped: false,
            rows: geom.rows,
            cols: geom.cols,
            cell_w: geom.cell.0,
            cell_h: geom.cell.1,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
        })
    }
}

#[test]
fn a_landing_in_an_agents_pane_cheers_on_open_ground_with_its_notes_on_glass() {
    let mut p = AgentPane::hosted();
    assert!(p.parks_within(10.0), "fixture: the cat parks");
    p.0.pet.note_room_landed();
    let mut frames: Vec<PetFrame> = (0..(4.0 / 0.016) as usize).map(|_| p.frame()).collect();
    let frolic = frames
        .iter()
        .position(|f| f.action == PetAction::Frolic)
        .expect("the landing frolics in the agent's pane");
    assert!(
        frames[..frolic].iter().any(|f| f.action == PetAction::Perk),
        "perked first"
    );
    assert_eq!(
        frames[frolic..]
            .iter()
            .map(|f| f.motes.iter().flatten().count())
            .max(),
        Some(3),
        "its three ♪/♥ on glass together"
    );
    let stage = (frames[frolic].col, frames[frolic].row);
    assert_ne!(
        stage,
        (frames[0].col, frames[0].row),
        "it went to open ground"
    );
    assert!(
        frames[frolic..]
            .iter()
            .take_while(|f| f.motes.iter().flatten().count() > 0)
            .all(|f| f.action != PetAction::Leap),
        "the cat stays on its stage while they rise"
    );
    for _ in 0..(10.0 / 0.016) as usize {
        frames.push(p.frame());
        if !p.0.pet.needs_frames() {
            break;
        }
    }
    assert!(!p.0.pet.needs_frames(), "then it parks");
    assert!(
        !p.0.pet.room_cheer && p.0.pet.pending_cheer.is_none(),
        "spent"
    );
    assert_ne!(
        (p.0.pet.col, p.0.pet.row),
        stage,
        "and the cat has gone home"
    );
    let faded = frames[frolic..]
        .iter()
        .filter(|f| f.alpha < 255)
        .map(|f| (f.action, f.col, f.row, f.alpha))
        .collect::<Vec<_>>();
    assert!(
        faded.is_empty() && p.0.pet.console.on_glass,
        "on glass all the way home: a home it may stand on is no reason to \
         leave: {faded:?}"
    );
}

#[test]
fn a_cat_asleep_on_the_transcript_wakes_into_the_cheer_not_an_eviction() {
    let mut p = AgentPane::new();
    p.1 = Some(PetRect::new(12.0, 0.0, 1.0, 100.0));
    p.0.pet.col = 0.371;
    p.0.pet.row = 10.995;
    p.0.pet.set_action(PetAction::Sleep);
    p.0.pet.note_room_landed();
    let frames: Vec<PetFrame> = (0..(4.0 / 0.016) as usize).map(|_| p.frame()).collect();
    let woke = frames
        .iter()
        .position(|f| f.action != PetAction::Sleep)
        .expect("it wakes");
    let frolic = frames
        .iter()
        .position(|f| f.action == PetAction::Frolic)
        .expect("and frolics");
    assert!(woke < frolic);
    assert_eq!(
        frames[frolic..]
            .iter()
            .map(|f| f.motes.iter().flatten().count())
            .max(),
        Some(3),
        "its three ♪/♥ on glass together"
    );
    p.assert_home_on_clean_ground("the sleeper's cheer", None);
}

#[test]
fn a_success_stretches_only_on_blank_ground() {
    for under_a_word in [true, false] {
        let why = if under_a_word {
            "under a word"
        } else {
            "on blank ground"
        };
        let mut s = Scene::new();
        for _ in 0..30 {
            s.frame(0.016);
        }
        let (col, row) = (s.pet.col, s.pet.row);
        if under_a_word {
            s.term.process(
                format!(
                    "\x1b[{};{}Hall 42 tests passed in 31.9s\x1b[6;17H",
                    row as usize + 1,
                    col.floor() as usize
                )
                .as_bytes(),
            );
        }
        s.pet
            .note_command_done(s.now, false, Some(CHEER_BIG_MS + 1_000));
        let first = s.frame(0.016);
        assert_eq!(
            s.pet.console_reason(),
            "command-exit-success",
            "{why}: the result is answered"
        );
        let mut frames = vec![first];
        frames.extend(s.watch(RESULT_HOLD + 0.5));
        let world = s.pet.console_world().expect("observed");
        let stretches: Vec<_> = frames
            .iter()
            .filter(|f| {
                f.alpha > 0 && matches!(f.pose, PetGlyphId::PetStretch | PetGlyphId::PetStretchHind)
            })
            .map(|f| {
                let (x0, x1, y0, y1) = f.body_px(10, 20, 80, 20).expect("on glass");
                let rect = PetRect::new(
                    y0 as f32 / 20.0,
                    x0 as f32 / 10.0,
                    (y1 - y0) as f32 / 20.0,
                    (x1 - x0) as f32 / 10.0,
                );
                world.ink_clearance(rect, 0.0)
            })
            .collect();
        if under_a_word {
            assert!(
                stretches.is_empty(),
                "{why}: no stretch drawn under the words: {stretches:?}"
            );
            let held: Vec<_> = frames
                .iter()
                .take_while(|f| f.action != PetAction::Leap)
                .take((RESULT_HOLD / 0.016) as usize)
                .map(|f| {
                    let (x0, x1, y0, y1) = f.body_px(10, 20, 80, 20).expect("on glass");
                    let rect = PetRect::new(
                        y0 as f32 / 20.0,
                        x0 as f32 / 10.0,
                        (y1 - y0) as f32 / 20.0,
                        (x1 - x0) as f32 / 10.0,
                    );
                    (f.alpha, f.under_ink, world.ink_clearance(rect, 0.0), f.pose)
                })
                .collect();
            assert!(
                held.len() >= (RESULT_HOLD / 0.016) as usize - 1
                    && held.iter().all(|&(alpha, under, ink, _)| {
                        alpha > 0 && under && ink == Some(false)
                    }),
                "{why}: through the hold the cat stays where it lies, on glass, drawn \
                 behind the words: {held:?}"
            );
        } else {
            assert!(
                !stretches.is_empty() && stretches.iter().all(|&c| c == Some(true)),
                "{why}: the stretch plays, in front of blank ground: {stretches:?}"
            );
        }
    }
}

#[test]
fn a_key_wakes_a_sleeper_under_a_word_without_the_stretch() {
    for under_a_word in [true, false] {
        let why = if under_a_word {
            "under a word"
        } else {
            "on blank ground"
        };
        let mut s = Scene::new();
        for _ in 0..30 {
            s.frame(0.016);
        }
        let (col, row) = (s.pet.col, s.pet.row);
        if under_a_word {
            s.term.process(
                format!(
                    "\x1b[{};{}Hall 42 tests passed in 31.9s\x1b[6;17H",
                    row as usize + 1,
                    col.floor() as usize
                )
                .as_bytes(),
            );
        }
        s.watch(3.0);
        s.pet.set_action(PetAction::Sleep);
        s.pet.quiet = 30.0;
        let asleep = s.watch(0.064).pop().expect("a frame");
        assert_eq!(asleep.action, PetAction::Sleep, "{why}: fixture: asleep");
        let (x0, x1, y0, y1) = asleep.body_px(10, 20, 80, 20).expect("on glass");
        let sleeper = PetRect::new(
            y0 as f32 / 20.0,
            x0 as f32 / 10.0,
            (y1 - y0) as f32 / 20.0,
            (x1 - x0) as f32 / 10.0,
        );
        assert_eq!(
            s.pet
                .console_world()
                .expect("observed")
                .ink_clearance(sleeper, 0.0),
            Some(!under_a_word),
            "{why}: fixture: the sleeper's body and the words"
        );
        s.input(PetInputKind::Text);
        let frames = s.watch(1.5);
        assert!(
            frames.iter().any(|f| f.action == PetAction::Waking),
            "{why}: the key wakes it"
        );
        let world = s.pet.console_world().expect("observed");
        let stretches: Vec<_> = frames
            .iter()
            .filter(|f| {
                f.alpha > 0 && matches!(f.pose, PetGlyphId::PetStretch | PetGlyphId::PetStretchHind)
            })
            .map(|f| {
                let (x0, x1, y0, y1) = f.body_px(10, 20, 80, 20).expect("on glass");
                let rect = PetRect::new(
                    y0 as f32 / 20.0,
                    x0 as f32 / 10.0,
                    (y1 - y0) as f32 / 20.0,
                    (x1 - x0) as f32 / 10.0,
                );
                world.ink_clearance(rect, 0.0)
            })
            .collect();
        if under_a_word {
            assert!(
                stretches.is_empty(),
                "{why}: no stretch drawn under the words: {stretches:?}"
            );
        } else {
            assert!(
                !stretches.is_empty() && stretches.iter().all(|&c| c == Some(true)),
                "{why}: the stretch plays, in front of blank ground: {stretches:?}"
            );
        }
    }
}

#[test]
fn a_sleepers_fall_stretches_only_on_blank_ground() {
    let words = |n: usize| -> String {
        "echo 'all 42 tests passed in 31.9s; the parser and its tests are pushed' "
            .chars()
            .cycle()
            .take(n)
            .collect()
    };
    let floors: [(&str, &str, &str, bool, [bool; 2]); 4] = [
        ("over the typed words", "", "", true, [false, false]),
        ("over a blank line", "", "", false, [true, true]),
        (
            "from under a line of output onto a blank line",
            "test result: ok. 271 passed; 0 failed; 0 ignored; 0 measured; 1776 filtered out",
            "",
            false,
            [false, true],
        ),
        (
            "from blank ground onto a painted band",
            "",
            "\x1b[48;2;55;55;55m\x1b[K\x1b[0m",
            false,
            [true, false],
        ),
    ];
    for (why, above, below, typed_words, blank) in floors {
        let mut s = Scene::new();
        for _ in 0..30 {
            s.frame(0.016);
        }
        s.term
            .process(format!("\x1b[5;1H{above}\x1b[7;1H{below}\x1b[6;17H").as_bytes());
        s.watch(3.0);
        s.pet.set_action(PetAction::Sleep);
        s.pet.quiet = 30.0;
        let asleep = s.watch(0.064).pop().expect("a frame");
        assert_eq!(asleep.action, PetAction::Sleep, "{why}: fixture: asleep");
        assert_eq!(s.pet.row, 5.0, "{why}: fixture: asleep on the caret's row");
        let wrap = 80 - usize::from(s.term.cursor().col) + 3;
        let line = if typed_words {
            words(wrap)
        } else {
            " ".repeat(wrap)
        };
        s.term.process(line.as_bytes());
        let mut fell = None;
        let mut frames = Vec::new();
        for _ in 0..(1.0 / 0.016) as usize {
            frames.push(s.frame(0.016));
            fell = fell.or(s.pet.flight.filter(|f| f.drop));
        }
        let fell = fell.unwrap_or_else(|| panic!("{why}: fixture: the sleeper falls one row"));
        let ends = [
            s.pet
                .console_stretch_on_blank_at((fell.from_col, fell.from_row)),
            s.pet
                .console_stretch_on_blank_at((fell.to_col, fell.to_row)),
        ];
        assert_eq!(ends, blank, "{why}: fixture: blank ground at (from, to)");
        let world = s.pet.console_world().expect("observed");
        let stretches: Vec<_> = frames
            .iter()
            .filter(|f| {
                f.alpha > 0 && matches!(f.pose, PetGlyphId::PetStretch | PetGlyphId::PetStretchHind)
            })
            .map(|f| {
                let (x0, x1, y0, y1) = f.body_px(10, 20, 80, 20).expect("on glass");
                let rect = PetRect::new(
                    y0 as f32 / 20.0,
                    x0 as f32 / 10.0,
                    (y1 - y0) as f32 / 20.0,
                    (x1 - x0) as f32 / 10.0,
                );
                world.ink_clearance(rect, 0.0)
            })
            .collect();
        if blank == [true, true] {
            assert!(
                !stretches.is_empty() && stretches.iter().all(|&c| c == Some(true)),
                "{why}: the fall stretches, in front of blank ground: {stretches:?}"
            );
        } else {
            assert!(
                stretches.is_empty(),
                "{why}: no stretch drawn in the fall: {stretches:?}"
            );
        }
    }
}

impl AgentPane {
    fn stretches_over_ink(&self, frames: &[PetFrame]) -> Vec<(usize, Vec<(i32, i32)>)> {
        let world = self.0.pet.console_world().expect("observed");
        let PaneGeom {
            cols,
            cell: (cw, ch),
            ..
        } = self.2;
        let (cw, ch) = (i32::from(cw), i32::from(ch));
        frames
            .iter()
            .enumerate()
            .filter(|(_, f)| {
                f.alpha > 0 && matches!(f.pose, PetGlyphId::PetStretch | PetGlyphId::PetStretchHind)
            })
            .filter_map(|(k, f)| {
                let (x0, x1, y0, y1) =
                    f.body_px(self.2.cell.0, self.2.cell.1, cols, self.2.rows)?;
                let under: Vec<_> = (y0.div_euclid(ch)..(y1 + ch - 1).div_euclid(ch))
                    .flat_map(|r| {
                        (x0.div_euclid(cw)..(x1 + cw - 1).div_euclid(cw)).map(move |c| (r, c))
                    })
                    .filter(|&(r, c)| {
                        world.cell(r as usize, c as usize) != crate::pet_world::PetCell::Clear
                    })
                    .collect();
                (!under.is_empty()).then_some((k, under))
            })
            .collect()
    }
}

#[test]
fn a_sleeper_stretches_only_on_blank_ground() {
    let mut p = AgentPane::new();
    p.1 = Some(PetRect::new(12.0, 0.0, 1.0, 100.0));
    p.0.pet.col = 0.371;
    p.0.pet.row = 10.995;
    p.0.pet.set_action(PetAction::Sleep);
    p.0.pet.note_room_landed();
    let frames: Vec<PetFrame> = (0..(4.0 / 0.016) as usize).map(|_| p.frame()).collect();
    let over = p.stretches_over_ink(&frames);
    assert!(
        over.is_empty(),
        "no stretch drawn over a word: {over:?} (frames, cells under the body)"
    );
    let leap = frames
        .iter()
        .position(|f| f.action == PetAction::Leap)
        .expect("it hops to its stage");
    assert!(
        frames[..leap].iter().all(|f| f.action == PetAction::Sleep),
        "no wake beat before the hop: {:?}",
        frames[..leap].iter().map(|f| f.action).collect::<Vec<_>>()
    );
    let frolic = frames
        .iter()
        .position(|f| f.action == PetAction::Frolic)
        .expect("it wakes and frolics on its stage");
    assert_eq!(
        frames[frolic..]
            .iter()
            .map(|f| f.motes.iter().flatten().count())
            .max(),
        Some(3),
        "its three ♪/♥ on glass together"
    );

    let mut p = AgentPane::hosted();
    assert!(p.parks_within(10.0), "fixture: the cat parks at home");
    p.0.pet.set_action(PetAction::Sleep);
    p.0.pet.note_room_landed();
    let frames: Vec<PetFrame> = (0..(4.0 / 0.016) as usize).map(|_| p.frame()).collect();
    let stretched = frames
        .iter()
        .filter(|f| matches!(f.pose, PetGlyphId::PetStretch | PetGlyphId::PetStretchHind))
        .count();
    assert!(stretched > 0, "a sleeper on blank ground stretches awake");
    let over = p.stretches_over_ink(&frames);
    assert!(over.is_empty(), "on blank ground: {over:?}");
    assert!(
        frames.iter().any(|f| f.action == PetAction::Frolic),
        "and frolics"
    );

    let mut p = AgentPane::hosted();
    assert!(p.parks_within(10.0), "fixture: the cat parks at home");
    p.1 = None;
    p.0.pet.set_action(PetAction::Sleep);
    p.frame();
    assert!(
        p.0.pet.console_stretch_on_blank(),
        "fixture: asleep on blank ground"
    );
    let body = p.0.pet.console_body(PetSense {
        caret_drawn: false,
        now: p.0.now,
        caret: None,
        wrapped: false,
        rows: 30,
        cols: 100,
        cell_w: 15,
        cell_h: 28,
        reduced_motion: false,
        output_burst: false,
        pointer: None,
    });
    p.1 = Some(body);
    p.observe();
    let world = p.0.pet.console_world().expect("observed");
    assert_eq!(
        world.under_text_clearance_past_caret(body, 0.0),
        Some(false),
        "fixture: the host's chrome over the sleeper"
    );
    assert!(
        !p.0.pet.console_stretch_on_blank(),
        "under the host's chrome: no blank ground for the stretch"
    );
}

impl AgentPane {
    fn at_first_look(asleep: bool) -> (Self, (f32, f32)) {
        let mut p = Self::new();
        p.1 = Some(PetRect::new(12.0, 0.0, 1.0, 100.0));
        assert!(p.parks_within(10.0), "fixture: the cat parks at home");
        assert!(
            p.0.pet.console.on_glass,
            "fixture: its home is clean ground it may stand on: {}",
            p.0.pet.console.reason
        );
        let home = (p.0.pet.col, p.0.pet.row);
        p.0.pet.alpha = 0.0;
        p.0.pet.console.on_glass = false;
        p.0.pet.set_action(if asleep {
            PetAction::Sleep
        } else {
            PetAction::Sit
        });
        (p, home)
    }

    fn assert_home_on_clean_ground(&mut self, why: &str, home: Option<(f32, f32)>) {
        assert!(
            self.parks_within(10.0),
            "{why}: the cat parks after the cheer, not a hop in place: \
             {:?} at {:.2},{:.2}",
            self.0.pet.action,
            self.0.pet.col,
            self.0.pet.row
        );
        let f = self.frame();
        assert!(
            self.0.pet.console.on_glass && f.alpha > 0,
            "{why}: home on glass ({})",
            self.0.pet.console.reason
        );
        let PaneGeom {
            cols,
            rows,
            cell: (cw, ch),
        } = self.2;
        let (x0, x1, y0, y1) = f.body_px(cw, ch, cols, rows).expect("on glass");
        let world = self.0.pet.console_world().expect("observed");
        let (cw, ch) = (i32::from(cw), i32::from(ch));
        let under: Vec<_> = (y0.div_euclid(ch)..(y1 + ch - 1).div_euclid(ch))
            .flat_map(|r| (x0.div_euclid(cw)..(x1 + cw - 1).div_euclid(cw)).map(move |c| (r, c)))
            .filter(|&(r, c)| {
                world.cell(r as usize, c as usize) != crate::pet_world::PetCell::Clear
            })
            .collect();
        assert!(
            under.is_empty(),
            "{why}: nothing under the cat at home ({:.2}, {:.2}): {under:?}",
            self.0.pet.col,
            self.0.pet.row
        );
        if let Some((col, row)) = home {
            assert!(
                (self.0.pet.col - col).abs() < 0.5 && (self.0.pet.row - row).abs() < 0.5,
                "{why}: back at its home ({col:.2}, {row:.2}): {:.2},{:.2}",
                self.0.pet.col,
                self.0.pet.row
            );
        }
        assert!(
            !self.0.pet.room_cheer && self.0.pet.pending_cheer.is_none(),
            "{why}: spent"
        );
    }
}

#[test]
fn a_first_look_appears_on_open_ground_cheers_and_goes_home() {
    for asleep in [false, true] {
        let why = if asleep { "asleep" } else { "awake" };
        let (mut p, home) = AgentPane::at_first_look(asleep);
        p.0.pet.note_room_landed();
        let frames: Vec<PetFrame> = (0..(4.0 / 0.016) as usize).map(|_| p.frame()).collect();
        let frolic = frames
            .iter()
            .position(|f| f.action == PetAction::Frolic)
            .unwrap_or_else(|| panic!("{why}: the landing frolics"));
        assert!(
            frames[..frolic].iter().any(|f| f.action == PetAction::Perk),
            "{why}: perked first"
        );
        assert_eq!(
            frames[frolic..]
                .iter()
                .map(|f| f.motes.iter().flatten().count())
                .max(),
            Some(3),
            "{why}: its three ♪/♥ on glass together"
        );
        let stage = (frames[frolic].col, frames[frolic].row);
        let seen_elsewhere = frames[..frolic]
            .iter()
            .filter(|f| f.alpha > 0 && (f.col, f.row) != stage)
            .map(|f| (f.action, f.col, f.row))
            .collect::<Vec<_>>();
        assert!(
            seen_elsewhere.is_empty(),
            "{why}: the cat is seen only on its stage {stage:?}, never flying \
             in: {seen_elsewhere:?}"
        );
        p.assert_home_on_clean_ground(why, Some(home));
    }
}

impl AgentPane {
    fn landing(&mut self) -> (Vec<PetFrame>, Option<CheerAir>) {
        self.0.pet.note_room_landed();
        let mut air = None;
        let frames = (0..(5.0 / 0.016) as usize)
            .map(|_| {
                let f = self.frame();
                air = self.0.pet.room_cheer_air.or(air);
                f
            })
            .collect();
        (frames, air)
    }

    fn notes_on_glass(&self, frames: &[PetFrame]) -> (usize, usize) {
        let PaneGeom { cols, rows, cell } = self.2;
        let mut most = 0;
        let mut meets = 0;
        for f in frames.iter().filter(|f| f.alpha > 0) {
            let boxes: Vec<_> = f
                .motes
                .iter()
                .flatten()
                .filter(|m| matches!(m.kind, PetMoteKind::Note | PetMoteKind::Heart) && m.alpha > 0)
                .filter_map(|m| {
                    crate::kitty_pet::mote_box_px((m.col, m.row), m.scale, cell, (cols, rows))
                })
                .collect();
            most = most.max(boxes.len());
            for (i, a) in boxes.iter().enumerate() {
                meets += boxes[i + 1..]
                    .iter()
                    .filter(|b| {
                        a.0 < b.0 + i32::from(b.2)
                            && b.0 < a.0 + i32::from(a.2)
                            && a.1 < b.1 + i32::from(b.2)
                            && b.1 < a.1 + i32::from(a.2)
                    })
                    .count();
            }
        }
        (most, meets)
    }
}

#[test]
fn a_landing_in_a_narrow_pane_shows_its_notes() {
    for (cols, rows, cell, compact) in [
        (44, 16, (15, 28), false),
        (44, 16, (7, 14), false),
        (41, 16, (7, 14), true),
        (41, 30, (7, 14), false),
    ] {
        let why = format!("{cols} x {rows} at {} x {} px", cell.0, cell.1);
        let mut p = AgentPane::narrow_at(cols, rows, cell);
        assert!(p.parks_within(10.0), "{why}: fixture: the cat parks");
        let (frames, air) = p.landing();
        let frolic = frames
            .iter()
            .position(|f| f.action == PetAction::Frolic)
            .unwrap_or_else(|| panic!("{why}: the landing frolics"));
        let flights = frames[..frolic]
            .windows(2)
            .filter(|w| w[1].action == PetAction::Leap && w[0].action != PetAction::Leap)
            .count();
        assert!(
            flights <= 1,
            "{why}: {flights} flights, not one to its stage"
        );
        let (most, meets) = p.notes_on_glass(&frames[frolic..]);
        assert_eq!(most, 3, "{why}: its three ♪/♥ on glass together");
        assert_eq!(
            air.map(|a| a == CheerAir::Compact),
            Some(compact),
            "{why}: the stage's air ({air:?})"
        );
        if !compact {
            assert_eq!(meets, 0, "{why}: no two ♪/♥ tiles meet");
        }
    }
}

#[test]
fn a_stage_is_reached_in_one_flight() {
    let mut p = AgentPane::narrow(60, (7, 14));
    assert!(p.parks_within(10.0), "fixture: the cat parks");
    p.1 = Some(PetRect::new(12.0, 0.0, 1.0, 60.0));
    (p.0.pet.col, p.0.pet.row) = (20.0, 8.495);
    let (frames, air) = p.landing();
    let frolic = frames
        .iter()
        .position(|f| f.action == PetAction::Frolic)
        .expect("the landing frolics");
    let flights = frames[..frolic]
        .windows(2)
        .filter(|w| w[1].action == PetAction::Leap && w[0].action != PetAction::Leap)
        .count()
        + usize::from(frames[0].action == PetAction::Leap);
    assert_eq!(flights, 1, "one flight to its stage ({air:?})");
    let (most, meets) = p.notes_on_glass(&frames[frolic..]);
    assert_eq!(most, 3, "its three ♪/♥ on glass together");
    assert_eq!(meets, 0, "no two ♪/♥ tiles meet");
}

#[test]
fn a_cat_at_the_panes_edge_cheers_facing_into_the_pane() {
    let w = art_cols(15, 28);
    let edge = 60.0 - w - 0.2;
    // As it ships, and with a fit by the wall off: there a frame's `col` is where it stands,
    // and not where the fit draws it in its box, so the frames read its stand exactly.
    for (centred, reach) in [(false, w * 0.5), (true, 0.0)] {
        let mut p = AgentPane::narrow(60, (15, 28));
        assert!(p.parks_within(10.0), "fixture: the cat parks");
        (p.0.pet.col, p.0.pet.row) = (edge, 2.85);
        p.0.pet.facing_left = false;
        p.0.pet.set_old_path(OldPath::FitCentredAtTheWall, centred);
        let (frames, air) = p.landing();
        assert_eq!(air, Some(CheerAir::Lanes(true)), "the air is to its left");
        let frolic = frames
            .iter()
            .position(|f| f.action == PetAction::Frolic)
            .expect("the landing frolics");
        assert!(
            frames[..=frolic].iter().all(|f| {
                f.action != PetAction::Leap && f.col - edge > -0.5 && f.col - edge < reach + 0.5
            }),
            "it cheers where it stands, against the edge (fit centred: {centred})"
        );
        assert!(
            frames[frolic..]
                .iter()
                .take_while(|f| f.action == PetAction::Frolic)
                .all(|f| f.facing_left),
            "it frolics facing into the pane (fit centred: {centred})"
        );
        let (most, meets) = p.notes_on_glass(&frames[frolic..]);
        assert_eq!(most, 3, "its three ♪/♥ on glass together");
        assert_eq!(meets, 0, "no two ♪/♥ tiles meet");
    }
}

#[test]
fn a_flight_over_a_reserved_rule_reaches_its_stage_in_one() {
    let mut p = AgentPane::new();
    assert!(p.parks_within(10.0), "fixture: the cat parks at home");
    assert!(p.0.pet.console.on_glass, "fixture: on glass");
    p.1 = Some(PetRect::new(12.0, 0.0, 1.0, 100.0));
    (p.0.pet.col, p.0.pet.row) = (3.0, 13.0);
    p.0.pet.note_room_landed();
    let mut bound_for = None;
    let mut frames = Vec::new();
    for _ in 0..(5.0 / 0.016) as usize {
        frames.push(p.frame());
        bound_for = bound_for.or(p.0.pet.room_cheer_stage);
    }
    let to_frolic = frames
        .iter()
        .position(|f| f.action == PetAction::Frolic)
        .expect("the landing frolics");
    let flights = frames[..to_frolic]
        .windows(2)
        .filter(|w| w[1].action == PetAction::Leap && w[0].action != PetAction::Leap)
        .count()
        + usize::from(frames[0].action == PetAction::Leap);
    assert_eq!(flights, 1, "one flight, over the rule, to its stage");
    let bound_for = bound_for.expect("it set out for a stage");
    let frolic = frames
        .iter()
        .position(|f| f.action == PetAction::Frolic)
        .expect("the landing frolics");
    let at = (frames[frolic].col, frames[frolic].row);
    assert!(
        (at.0 - bound_for.0).abs() * 15.0 < 0.5 && at.1 == bound_for.1,
        "it cheers on the stage it set out for: at {at:?}, bound for {bound_for:?}"
    );
    assert_eq!(
        frames[frolic..]
            .iter()
            .map(|f| f.motes.iter().flatten().count())
            .max(),
        Some(3),
        "its three ♪/♥ on glass together"
    );
    p.assert_home_on_clean_ground("the cheer over the rule", None);
}

#[test]
fn a_home_under_the_footer_cheers_on_the_ground_below_it() {
    let mut p = AgentPane::new_warm(0);
    // The host's line held from the first frame ([`AgentPane::hosted`]).
    p.1 = Some(PetRect::new(12.0, 0.0, 1.0, 100.0));
    p.0.pet.col = 4.0;
    p.0.pet.row = 17.0;
    // The composer's line under the host's chrome end to end: no rest on it beside
    // the chrome ([`PetBrain::console_station`]), so the cat's home is under the footer.
    p.1 = Some(PetRect::new(13.0, 0.0, 1.0, 100.0));
    for _ in 0..200 {
        p.frame();
    }
    assert!(p.parks_within(10.0), "fixture: the cat parks at home");
    let home = (p.0.pet.col, p.0.pet.row);
    assert!(
        p.0.pet.console.on_glass && home.1 > 15.0,
        "fixture: its home is under the footer, on glass: {home:?} ({})",
        p.0.pet.console.reason
    );
    p.1 = Some(PetRect::new(12.0, 0.0, 1.0, 100.0));
    p.0.pet.note_room_landed();
    let frames: Vec<PetFrame> = (0..(4.0 / 0.016) as usize).map(|_| p.frame()).collect();
    let frolic = frames
        .iter()
        .position(|f| f.action == PetAction::Frolic)
        .expect("the landing frolics");
    assert!(
        frames[..frolic].iter().any(|f| f.action == PetAction::Perk),
        "perked first"
    );
    assert_eq!(
        frames[frolic..]
            .iter()
            .map(|f| f.motes.iter().flatten().count())
            .max(),
        Some(3),
        "its three ♪/♥ on glass together"
    );
    let stage = (frames[frolic].col, frames[frolic].row);
    assert!(
        stage.1 > home.1,
        "its stage is the open ground below its home {home:?}: {stage:?}"
    );
    p.assert_home_on_clean_ground("the home under the footer", Some(home));
}

#[test]
fn a_key_before_a_landings_cheer_spends_it_unplayed() {
    let mut s = Scene::new();
    s.executing();
    s.pet.note_room_landed();
    s.input(PetInputKind::Text);
    s.term.process(b"x");
    let mut frames = Vec::new();
    let mut aloft = 0;
    for _ in 0..(4.0 / 0.016) as usize {
        frames.push(s.frame(0.016));
        aloft = aloft.max(s.pet.cheer_motes_aloft());
    }
    assert!(
        frames.iter().all(|f| f.action != PetAction::Frolic) && aloft == 0,
        "nothing plays"
    );
    assert!(
        s.pet.pending_cheer.is_none() && !s.pet.room_cheer,
        "spent, not owed"
    );
    assert!(
        s.pet.console.resident,
        "the running command has its watcher back"
    );
}

#[test]
fn a_held_selection_drops_a_trick_and_never_replays_it() {
    let mut s = Scene::new();
    s.term.process(b"\x1b[3;3Hread this");
    s.term
        .text_selection_mut()
        .start_selection(2, 2, SelectionSide::Left, SelectionType::Simple);
    s.term
        .text_selection_mut()
        .update_selection(2, 10, SelectionSide::Right);
    for _ in 0..240 {
        s.frame(0.016);
        if !s.pet.needs_frames() {
            break;
        }
    }
    assert_eq!(s.pet.console_attention(), PetAttention::Reading);
    assert!(
        !s.pet.needs_frames(),
        "fixture: the reading resident is parked"
    );
    s.pet.note_trick(s.now, Trick::Roll, true);
    s.frame(0.016);
    assert_eq!(
        s.pet.pending_trick(),
        None,
        "dropped by the resident that owns the tick"
    );
    assert!(!s.pet.needs_frames(), "and a held selection still parks");
    s.term.text_selection_mut().clear();
    let frames = s.watch(3.0);
    assert!(
        frames.iter().all(|f| f.pose != PetGlyphId::PetRoll1),
        "released, it replays nothing"
    );
}

#[test]
fn streamed_output_lets_a_trick_expire_and_the_resident_takes_its_perch_back() {
    let mut s = Scene::new();
    s.executing();
    s.pet.note_trick(s.now, Trick::Roll, true);
    let t0 = s.now;
    let mut gone = None;
    let mut rolled = false;
    for step in 0..625u32 {
        if step % 6 == 0 {
            s.term.process(if step % 12 == 0 {
                b"\r\ncompiling another crate".as_slice()
            } else {
                b"\r\ncompiling".as_slice()
            });
        }
        let f = s.frame(0.016);
        rolled |= f.pose == PetGlyphId::PetRoll1;
        if gone.is_none() && s.pet.pending_trick().is_none() {
            gone = Some((s.now - t0).as_secs_f32());
        }
    }
    let gone = gone.expect("the latch is finite");
    assert!(
        (TRICK_TTL..TRICK_TTL + 0.1).contains(&gone),
        "gone at +{gone} s: output re-stamps nothing, want {TRICK_TTL}"
    );
    assert!(!rolled, "and an expired trick is never performed");
    assert!(
        s.pet.console.resident,
        "the running command has its watcher back"
    );
    assert_eq!(s.pet.console_attention(), PetAttention::Output);
}

#[test]
fn a_trick_typed_through_a_repair_is_still_performed() {
    let mut s = Scene::new();
    s.type_text("rot");
    for _ in 0..2 {
        s.input(PetInputKind::Delete);
        s.term.process(b"\x08 \x08");
        s.frame(0.05);
    }
    s.type_text("oll ");
    s.pet.note_trick(s.now, Trick::Roll, true);
    s.frame(0.016);
    assert_eq!(
        s.pet.pending_trick(),
        Some(Trick::Roll),
        "the repair's resident does not eat the word it was repairing"
    );
    let frames = s.watch(3.5);
    assert!(
        frames.iter().any(|f| f.pose == PetGlyphId::PetRoll1),
        "and the roll plays once the hand has stopped"
    );
}

#[test]
fn a_seat_typed_through_a_repair_is_taken_and_held() {
    let mut s = Scene::new();
    s.type_text("sot");
    for _ in 0..2 {
        s.input(PetInputKind::Delete);
        s.term.process(b"\x08 \x08");
        s.frame(0.05);
    }
    s.type_text("it ");
    s.pet.note_trick(s.now, Trick::Sit, true);
    let frames = s.watch(3.0);
    let heard = frames
        .iter()
        .position(|f| f.action == PetAction::Perk)
        .expect("heard");
    let seated = frames[heard..]
        .iter()
        .filter(|f| f.pose == PetGlyphId::PetSitLookup)
        .count();
    assert!(
        seated as f32 * 0.016 > 1.0,
        "the commanded seat is on glass and held: {seated} frames"
    );
    assert_eq!(s.pet.trick_settle, Some(Trick::Sit), "and still is");
}

#[test]
fn fresh_input_stands_a_commanded_seat_down_but_keeps_an_owed_latch() {
    let mut s = Scene::new();
    s.pet.note_trick(s.now, Trick::Sit, true);
    let frames = s.watch(1.0);
    assert!(
        frames.iter().any(|f| f.pose == PetGlyphId::PetSitLookup),
        "fixture: the commanded seat is on glass"
    );
    assert_eq!(s.pet.trick_settle, Some(Trick::Sit));
    s.input(PetInputKind::Text);
    let f = s.frame(0.016);
    assert!(
        s.pet.trick_settle.is_none(),
        "the seat stands down with the key"
    );
    assert_ne!(f.pose, PetGlyphId::PetSitLookup);

    s.pet.note_trick(s.now, Trick::Roll, true);
    s.pet.pending_cheer = Some(false);
    s.input(PetInputKind::Submit);
    s.term.process(b"\r\n> ");
    s.frame(0.016);
    assert_eq!(s.pet.pending_cheer, None, "fixture: the clear ran");
    assert_eq!(
        s.pet.pending_trick(),
        Some(Trick::Roll),
        "the latch is owed to the prompt after the Enter"
    );
    let frames = s.watch(3.5);
    assert!(
        frames.iter().any(|f| f.pose == PetGlyphId::PetRoll1),
        "and it plays there"
    );
}

#[test]
fn a_forgiven_pet_only_submit_leaves_no_failed_result_on_the_console() {
    let failed_result = |pet_talk: bool| -> (bool, bool) {
        let mut s = Scene::new();
        s.type_text("sit");
        s.input(PetInputKind::Submit);
        if pet_talk {
            s.pet.note_trick_submit(s.now);
        }
        s.term.process(b"\r\nzsh: command not found: sit\r\n> ");
        s.frame(0.03);
        s.pet.note_command_done(s.now, true, Some(3));
        let hushed = s.pet.grieving();
        let mut inspected = false;
        for _ in 0..150 {
            s.frame(0.016);
            inspected |= s.pet.console_attention() == PetAttention::Result;
        }
        (hushed, inspected)
    };
    assert_eq!(
        failed_result(true),
        (false, false),
        "pet talk: no hush, no failed-result inspection"
    );
    let (hushed, _) = failed_result(false);
    assert!(hushed, "control: an ordinary fast failure is hushed");
}

#[test]
fn an_enter_inside_the_heard_you_beat_still_gets_its_trick() {
    let rolling = |f: &PetFrame| {
        matches!(
            f.pose,
            PetGlyphId::PetRoll0 | PetGlyphId::PetRoll1 | PetGlyphId::PetRoll2
        )
    };
    let mut s = Scene::new();
    s.type_text("roll ");
    s.pet.note_trick(s.now, Trick::Roll, false);
    for _ in 0..120 {
        s.frame(0.016);
        if s.pet.trick_go.is_some() {
            break;
        }
    }
    assert_eq!(
        s.pet.trick_go,
        Some(Trick::Roll),
        "fixture: heard, verb owed"
    );
    let _ = s.watch(0.1);
    assert!(s.pet.trick_go.is_some(), "fixture: still inside the beat");
    s.input(PetInputKind::Submit);
    s.pet.note_trick(s.now, Trick::Roll, true);
    s.term.process(b"\r\n> ");
    let f = s.frame(0.016);
    assert!(
        s.pet.trick_go.is_none() && !rolling(&f),
        "the Enter took the beat"
    );
    assert_eq!(
        s.pet.pending_trick(),
        Some(Trick::Roll),
        "…and the confirmation still owes the roll"
    );
    let frames = s.watch(4.5);
    let rolls = frames
        .windows(2)
        .filter(|w| !rolling(&w[0]) && rolling(&w[1]))
        .count();
    assert_eq!(rolls, 1, "performed at the new prompt, once");
}

#[test]
fn typing_after_repair_or_near_box_wall_never_sticks_on_caret_or_jiggles() {
    let mut s = Scene::new();
    s.type_text("hello");
    let _ = s.watch(0.5);
    s.input(PetInputKind::Delete);
    s.term.process(b"\x08 \x08");
    s.frame(0.016);
    for ch in "world!".chars() {
        s.type_text(&ch.to_string());
        let _ = s.watch(0.12);
        let f = s.frame(0.016);
        let cc = f32::from(s.term.cursor().col);
        assert!(
            f.col >= cc + 0.5 || f.col + art_cols(10, 20) <= cc + 0.1,
            "cat at col {} must not stick on advancing caret at col {cc}",
            f.col,
        );
    }

    s.term.process(b"\x1b[s\x1b[6;32H|\x1b[u");
    for ch in "abcd".chars() {
        s.type_text(&ch.to_string());
        let mut was = s.pet.col;
        for k in 0..10 {
            let f = s.frame(0.02);
            let cr = f32::from(s.term.cursor().row);
            let cc = f32::from(s.term.cursor().col);
            let on_caret_row = (f.row - cr).abs() < 0.5;
            // A cat sitting a cell ahead sets off at the key ([`PetBrain::tick`]): the caret
            // stands on its heel for the frames the run takes to clear it, never longer. The
            // console's seat ended the run it was carried on (the run's window ages through
            // the seat, as on a host that slept through it), so the run that clears it starts
            // from rest: six frames of 20 ms here.
            let setting_off = k < 7 && f.col > was;
            assert!(
                !on_caret_row
                    || setting_off
                    || f.col >= cc + 0.5
                    || f.col + art_cols(10, 20) <= cc + 0.1,
                "cat at ({}, {}) must never park on caret ({cr}, {cc}) near box border",
                f.col,
                f.row
            );
            was = f.col;
        }
    }
}

#[test]
fn faux_composer_caret_and_corner_park_prevent_bottom_left_stuck_kitty() {
    let mut s = Scene::new();
    s.term.process(
        b"\x1b[16;1H\xe2\x95\xad\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x95\xae\
          \x1b[17;1H\xe2\x94\x82 > hello world             \xe2\x94\x82\
          \x1b[18;1H\xe2\x95\xb0\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x95\xaf\
          \x1b[20;1H  26 files changed\x1b[20;1H\x1b[?25l",
    );
    let mut last = s.frame(0.016);
    for _ in 0..120 {
        s.now += Duration::from_secs_f32(0.02);
        let input = s.term.cell_frame(20, 80);
        let facts = PetWorldFacts::read(&s.term, 17);
        s.pet.observe_console(&input, &facts, PetPane::full(&input));
        s.pet.set_console_presentable(true);
        let c = s.term.cursor();
        last = s.pet.tick(PetSense {
            caret_drawn: s.term.cursor_visible(),
            now: s.now,
            caret: Some((c.row, c.col)),
            wrapped: false,
            rows: 20,
            cols: 80,
            cell_w: 10,
            cell_h: 20,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
        });
    }
    assert!(
        !(last.row >= 18.5 && last.col <= 2.5),
        "kitty must not get stuck in bottom-left corner over status bar: got ({}, {})",
        last.col,
        last.row
    );
    assert!(
        (last.row - 16.0).abs() <= 1.5,
        "kitty should station at the boxed composer row (row 16), got ({}, {})",
        last.col,
        last.row
    );

    s.term.process(
        b"\x1b[16;1H\xe2\x95\xad\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x95\xae\
          \x1b[17;1H\xe2\x94\x82 > first line of prompt     \xe2\x94\x82\
          \x1b[18;1H\xe2\x94\x82   second line\xe2\x96\x88            \xe2\x94\x82\
          \x1b[19;1H\xe2\x95\xb0\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x95\xaf\
          \x1b[20;1H  26 files changed\x1b[?25l",
    );
    let input = s.term.cell_frame(20, 80);
    assert_eq!(
        detect_faux_composer_caret(&input, PetPane::full(&input)),
        Some((17, 15)),
        "multiline composer should locate the block cursor on the second composer line"
    );

    s.term.process(
        b"\x1b[17;1H\xe2\x94\x82 > \x1b[48;5;24m[Image #1]\x1b[0m hello \x1b[7m \x1b[0m       \xe2\x94\x82\
          \x1b[18;1H\xe2\x95\xb0\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x95\xaf\
          \x1b[19;1H                              \
          \x1b[20;1H  26 files changed\x1b[?25l",
    );
    let input = s.term.cell_frame(20, 80);
    assert_eq!(
        detect_faux_composer_caret(&input, PetPane::full(&input)),
        Some((16, 21)),
        "multi-cell background pill must not be mistaken for the 1-cell inverse caret at col 21"
    );
}

#[test]
fn bordered_tui_box_places_station_in_interior_blank_gap_instead_of_on_words() {
    let mut s = Scene::new();
    let border = format!("+{}+", "-".repeat(78));
    let composer = format!("| > hello world{}|", " ".repeat(63));
    s.term.process(
        format!("\x1b[10;1H{border}\x1b[11;1H{composer}\x1b[12;1H{border}\x1b[11;7H").as_bytes(),
    );
    s.pet.sense_ink(9, &[(0, 80), (0, 80), (0, 80)], None);
    let frames = s.watch(1.5);
    let last = frames.last().copied().expect("frame");
    assert!(
        last.col >= 15.0 && last.col <= 74.0,
        "kitty inside walled TUI box should step past the words into the interior blank space (col >= 15), got col {} row {}",
        last.col,
        last.row
    );
}

struct Console {
    term: Terminal,
    pet: PetBrain,
    now: Instant,
    rows: u16,
    cols: u16,
    chrome: Vec<PetRect>,
    overlays: Vec<PetRect>,
    ink: bool,
    burst: bool,
}

impl Console {
    fn new(rows: u16, cols: u16, screen: &str, chrome: Vec<PetRect>) -> Self {
        let mut term = Terminal::new(rows, cols);
        term.process(b"\x1b]11;#000000\x07");
        term.process(screen.as_bytes());
        Self {
            term,
            pet: PetBrain::default(),
            now: Instant::now(),
            rows,
            cols,
            chrome,
            overlays: Vec::new(),
            ink: false,
            burst: false,
        }
    }

    fn agent(rows: u16, cols: u16, screen: &str, chrome: Vec<PetRect>) -> Self {
        Self::new(
            rows,
            cols,
            &format!("\x1b]133;A\x07$ \x1b]133;B\x07claude\r\n\x1b]133;C\x07\x1b[2J{screen}"),
            chrome,
        )
    }

    fn repaint(&mut self, screen: &str, chrome: Vec<PetRect>) {
        self.term.process(b"\x1b[2J");
        self.term.process(screen.as_bytes());
        self.chrome = chrome;
    }

    fn watch(&mut self, secs: f32) -> (usize, Option<f32>, PetFrame) {
        let (mut leaps, mut parked) = (0, None);
        let mut last = self.frame();
        for i in 1..(secs * 60.0) as usize {
            last = self.frame();
            if last.action == PetAction::Leap {
                leaps += 1;
            }
            if parked.is_none() && !self.pet.needs_frames() {
                parked = Some(i as f32 / 60.0);
            }
        }
        (leaps, parked, last)
    }

    fn frame(&mut self) -> PetFrame {
        self.now += Duration::from_secs_f32(1.0 / 60.0);
        let input = self
            .term
            .cell_frame(usize::from(self.rows), usize::from(self.cols));
        let facts = PetWorldFacts::read(&self.term, 17);
        self.pet.observe_console_with_overlays(
            &input,
            &facts,
            PetPane::full(&input),
            &self.chrome,
            &self.overlays,
        );
        self.pet.set_console_presentable(true);
        if self.ink {
            let spans: Vec<(u16, u16)> = (0..usize::from(self.rows))
                .map(|r| {
                    let text: Vec<char> =
                        self.term.row_text(r).unwrap_or_default().chars().collect();
                    let first = text.iter().position(|c| !c.is_whitespace());
                    let end = text.iter().rposition(|c| !c.is_whitespace());
                    first
                        .zip(end)
                        .map_or((0, 0), |(f, e)| (f as u16, e as u16 + 1))
                })
                .collect();
            let live = spans.iter().rposition(|&(f, e)| e > f).map(|r| r as u16);
            self.pet.sense_ink(0, &spans, live);
        }
        let c = self.term.cursor();
        self.pet.tick(PetSense {
            caret_drawn: self.term.cursor_visible(),
            now: self.now,
            caret: Some((c.row, c.col)),
            wrapped: false,
            rows: self.rows,
            cols: self.cols,
            cell_w: 10,
            cell_h: 20,
            reduced_motion: false,
            output_burst: self.burst,
            pointer: None,
        })
    }

    fn rest(&mut self, secs: f32) -> PetFrame {
        let mut last = self.frame();
        for _ in 1..(secs * 60.0) as usize {
            last = self.frame();
        }
        last
    }

    fn covered(&self, frame: &PetFrame) -> Vec<(usize, usize, PetCell)> {
        let (x0, x1, y0, y1) = frame
            .body_px(10, 20, self.cols, self.rows)
            .expect("the pet is on glass");
        let world = self.pet.console_world().expect("observed");
        let mut out = Vec::new();
        for r in y0.div_euclid(20)..(y1 + 19).div_euclid(20) {
            for c in x0.div_euclid(10)..(x1 + 9).div_euclid(10) {
                let cell = world.cell(r as usize, c as usize);
                if cell != PetCell::Clear {
                    out.push((r as usize, c as usize, cell));
                }
            }
        }
        out
    }
}

fn claude_screen(cols: usize, rule: usize, history: &[(usize, &str)]) -> String {
    let line = "\u{2500}".repeat(cols);
    let mut out = String::from(
        "\x1b[1;1H \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   Claude Code v2.1.304\
         \x1b[2;1H\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}  Opus 5.5 \u{b7} API Usage Billing\
         \x1b[3;1H \u{259D}\u{259D}   \u{259D}\u{259D}   /private/tmp/lab/proj",
    );
    for (row, text) in history {
        out.push_str(&format!("\x1b[{};1H{text}", row + 1));
    }
    out.push_str(&format!(
        "\x1b[{};1H{line}\x1b[{};1H\u{276F} \x1b[7m \x1b[27m\x1b[{};1H{line}\
         \x1b[{};1H  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle)\x1b[{};3H\x1b[?25l",
        rule + 1,
        rule + 2,
        rule + 3,
        rule + 4,
        rule + 2,
    ));
    out
}

fn claude_chrome(cols: usize, rule: usize, card: bool) -> Vec<PetRect> {
    let cols = cols as f32;
    let mut chrome = vec![
        PetRect::new(rule as f32, 0.0, 1.0, cols),
        PetRect::new(rule as f32 + 2.0, 3.0, 1.0, 5.0),
        PetRect::new(rule as f32 + 2.0, cols - 36.0, 1.0, 35.0),
    ];
    if card {
        chrome.push(PetRect::new(0.0, 0.0, 5.0, 87.0));
    }
    chrome
}

#[test]
fn a_hidden_composer_caret_at_the_top_of_a_tall_pane_rests_clear_of_every_word() {
    let mut s = Console::new(
        40,
        120,
        &claude_screen(120, 5, &[]),
        claude_chrome(120, 5, true),
    );
    let last = s.rest(6.0);
    assert!(
        matches!(
            last.action,
            PetAction::Sit | PetAction::Loaf | PetAction::Sleep
        ),
        "fixture: the pet has come to rest, got {:?}",
        last.action
    );
    assert_eq!(
        s.pet.last_caret,
        Some((6, 2)),
        "the home is the real composer caret, not one synthesized for it"
    );
    let covered = s.covered(&last);
    assert!(
        covered.is_empty(),
        "the resting pet covers no word and no chrome; at ({}, {}) it covers {covered:?}",
        last.col,
        last.row
    );
    assert!(
        last.row > 8.5 && last.row < 11.0 && last.col < 8.0,
        "the nearest clean ground: under the footer, at the caret's side, got ({}, {})",
        last.col,
        last.row
    );
}

#[test]
fn near_the_bottom_the_resting_pet_takes_the_footers_blank_tail_not_its_words() {
    let words = "Claude wrote this line all the way to the margin of the pane and on ".repeat(2);
    let history: Vec<(usize, &str)> = (3..=10).map(|row| (row, &words[..120])).collect();
    let mut s = Console::new(
        16,
        120,
        &claude_screen(120, 11, &history),
        claude_chrome(120, 11, false),
    );
    let last = s.rest(6.0);
    assert_eq!(
        s.pet.last_caret,
        Some((12, 2)),
        "fixture: the composer caret"
    );
    let covered = s.covered(&last);
    assert!(
        covered.is_empty(),
        "the resting pet covers no word and no chrome; at ({}, {}) it covers {covered:?}",
        last.col,
        last.row
    );
    assert!(
        last.row > 14.0 && last.col >= 38.0,
        "the footer's blank tail, right of `(shift+tab to cycle)`, got ({}, {})",
        last.col,
        last.row
    );
}

#[test]
fn a_real_caret_keeps_its_stand_beside_it() {
    let words = "x".repeat(80);
    let mut screen = String::new();
    for row in 1..=10 {
        screen.push_str(&format!("\x1b[{row};1H{words}"));
    }
    screen.push_str("\x1b[11;1H$ ");
    let mut s = Console::new(24, 80, &screen, Vec::new());
    let last = s.rest(6.0);
    assert!(s.term.cursor_visible(), "fixture: a real, visible caret");
    assert_eq!(s.pet.last_caret, Some((10, 2)));
    let width = art_cols(10, 20);
    let (col, row) = s.pet.station_safe((10, 2), 80, 24, width);
    assert!(
        (row - 10.0).abs() < 0.25,
        "the escort's stand stays on the caret's row, got {row}"
    );
    assert!(
        (last.col - col).abs() <= ARRIVED && (last.row - 10.0).abs() < 0.25,
        "the resting pet stays at its caret's stand ({col}, {row}), got ({}, {})",
        last.col,
        last.row
    );
}

const TURN: [(usize, &str); 3] = [
    (5, "\x1b[48;2;55;55;55m\u{276F} hi there\x1b[K\x1b[0m"),
    (
        7,
        "\u{23FA} Hi! I am the local lab stub, not a model. (reply 1)",
    ),
    (9, "\u{273B} Cooked for 0s \u{b7} done 6:54 AM"),
];

#[test]
fn a_turn_under_the_resting_cat_bounds_once_to_the_clean_ground_by_the_caret() {
    let mut s = Console::agent(
        40,
        120,
        &claude_screen(120, 5, &[]),
        claude_chrome(120, 5, true),
    );
    let rested = s.rest(6.0);
    assert!(
        s.covered(&rested).is_empty() && rested.row > 8.5,
        "fixture: at rest under the fresh footer, at ({}, {})",
        rested.col,
        rested.row
    );
    s.repaint(&claude_screen(120, 11, &TURN), claude_chrome(120, 11, true));
    let mut hops = Hops::default();
    let mut parked = None;
    for i in 0..(4 * 60) {
        hops.frame(&mut s);
        if parked.is_none() && !s.pet.needs_frames() {
            parked = Some(i);
        }
    }
    let last = s.frame();
    assert!(
        parked.is_some() && hops.leaps == 1 && hops.asides == 0 && hops.missed.is_empty(),
        "the cat parks after one bound: parked at frame {parked:?}, {} flights, {} frames \
         stepped aside, landings off their aim {:?}, at ({}, {}) {:?}",
        hops.leaps,
        hops.asides,
        hops.missed,
        last.col,
        last.row,
        last.action
    );
    let seat = (last.col, last.row);
    let (leaps, _, last) = s.watch(16.0);
    assert!(
        leaps == 0 && (last.col - seat.0).abs() < 0.05 && (last.row - seat.1).abs() < 0.05,
        "and stays parked on its seat {seat:?}: {leaps} leap frames, at ({}, {})",
        last.col,
        last.row
    );
    let covered = s.covered(&last);
    assert!(
        covered.is_empty(),
        "at rest it covers no word and no chrome; at ({}, {}) it covers {covered:?}",
        last.col,
        last.row
    );
    assert!(
        last.row > 14.0 && last.col < 12.0,
        "on the clean ground under the new footer, by the caret, got ({}, {})",
        last.col,
        last.row
    );
}

fn composer_with(draft: &str) -> (String, Vec<PetRect>) {
    let rule = "\u{2500}".repeat(120);
    let chars: Vec<char> = draft.chars().collect();
    let lines: Vec<String> = if chars.is_empty() {
        vec![String::new()]
    } else {
        chars.chunks(118).map(|c| c.iter().collect()).collect()
    };
    let mut out = String::new();
    for row in 11..20 {
        out.push_str(&format!("\x1b[{};1H\x1b[2K", row + 1));
    }
    out.push_str(&format!("\x1b[12;1H{rule}"));
    for (i, line) in lines.iter().enumerate() {
        let lead = if i == 0 { "\u{276F} " } else { "  " };
        out.push_str(&format!("\x1b[{};1H{lead}{line}", 13 + i));
    }
    let last = lines.len() - 1;
    let caret_col = 2 + lines[last].chars().count();
    out.push_str("\x1b[7m \x1b[27m");
    let lower = 12 + lines.len();
    out.push_str(&format!(
        "\x1b[{};1H{rule}\x1b[{};1H  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle)\x1b[{};{}H\x1b[?25l",
        lower + 1,
        lower + 2,
        13 + last,
        caret_col + 1
    ));
    let lower = lower as f32;
    let chrome = vec![
        PetRect::new(0.0, 0.0, 5.0, 87.0),
        PetRect::new(11.0, 0.0, 1.0, 120.0),
        PetRect::new(lower, 3.0, 1.0, 5.0),
        PetRect::new(lower, 84.0, 1.0, 35.0),
    ];
    (out, chrome)
}

#[test]
fn a_cat_stopped_in_the_escorts_grace_never_sets_out_under_the_chrome_to_turn_back() {
    let rule = 20usize;
    let mark = vec![PetRect::new(rule as f32, 2.0, 1.0, 16.0)];
    let mut s = Console::agent(40, 120, &claude_screen(120, rule, &[]), mark);
    s.ink = true;
    s.rest(6.0);
    let line = rule as f32 + 1.0;
    s.pet.col = 9.0;
    s.pet.row = line;
    s.pet.speed = 0.0;
    s.pet.set_action(PetAction::Sit);
    s.pet
        .note_console_input(s.now - Duration::from_secs_f32(1.0), PetInputKind::Delete);
    let mut cols = Vec::new();
    let mut last = None;
    for _ in 0..(3 * 60) {
        let f = s.frame();
        if f.alpha > 0 && f.action != PetAction::Leap {
            cols.push(f.col);
            last = Some(f);
        }
    }
    let (mut dir, mut anchor) = (0.0f32, cols[0]);
    let mut turns = Vec::new();
    for &c in &cols {
        let d = c - anchor;
        if d.abs() >= 0.5 {
            if dir != 0.0 && d.signum() != dir {
                turns.push((anchor, c));
            }
            dir = d.signum();
            anchor = c;
        }
    }
    let end = *cols.last().expect("the cat is drawn");
    assert!(
        turns.is_empty(),
        "the cat turned back on the composer's line: {turns:?} (from 9.00 to {end:.2})"
    );
    // THE HEAD LAW AT THE HOST'S LINE (2026-10-24): the caret's stand under the mark is
    // taken on top of the line instead, so the cat ends there, whole, clear of the mark;
    // until then it ended on the line past the mark's end (col 18), its head under the rule.
    let last = last.expect("the cat is drawn");
    let covered = s.covered(&last);
    assert!(
        (last.row - (rule as f32 - 1.0 - LINE_PERCH_GAP)).abs() < 0.01 && covered.is_empty(),
        "it ends on top of the composer's upper rule clear of the mark, at ({:.2}, {:.2}) \
         covering {covered:?}",
        last.col,
        last.row
    );
}

fn resting_above_the_composer() -> Console {
    let mut s = Console::agent(
        16,
        120,
        &claude_screen(120, 5, &[]),
        claude_chrome(120, 5, true),
    );
    s.ink = true;
    s.rest(6.0);
    let (bottom, chrome) = composer_with("");
    s.repaint(
        &format!("{}{bottom}", claude_screen(120, 11, &TURN)),
        chrome,
    );
    let rested = s.rest(8.0);
    assert!(
        rested.row < 11.0 && s.covered(&rested).is_empty() && !s.pet.needs_frames(),
        "fixture: at rest above the composer's upper rule, on clean ground, at ({}, {})",
        rested.col,
        rested.row
    );
    s
}

type Landing = ((f32, f32), Vec<(f32, f32)>);

#[derive(Default)]
struct Hops {
    leaps: usize,
    asides: usize,
    missed: Vec<Landing>,
    seats: Vec<(f32, f32)>,
    aim: Option<(f32, f32)>,
    aloft: bool,
    at: Option<(f32, f32)>,
}

impl Hops {
    fn frame(&mut self, s: &mut Console) {
        let f = s.frame();
        let aloft = f.action == PetAction::Leap;
        if aloft && !self.aloft {
            self.leaps += 1;
            self.seats.clear();
        }
        if let Some(seat) = s.pet.console.rest_seat.get() {
            self.seats.push(seat);
        }
        if let Some(flight) = s.pet.flight.as_ref() {
            self.aim = Some((flight.to_col, flight.to_row));
        }
        if self.aloft && !aloft {
            let on = |&(c, r): &(f32, f32)| (f.col - c).abs() <= 1.0 && (f.row - r).abs() < 0.25;
            if !self.seats.iter().any(on) && !self.aim.as_ref().is_some_and(on) {
                self.seats.dedup();
                self.missed
                    .push(((f.col, f.row), std::mem::take(&mut self.seats)));
            }
        }
        self.aloft = aloft;
        if s.pet.console.reason == "cursor-home" {
            self.asides += 1;
        }
        self.at = Some((f.col, f.row));
    }
}

/// THE HEAD LAW AT THE HOST'S LINE (2026-10-24). With the harness off the host still
/// repaints the composer's upper rule with its switch's label, and holds that line whole:
/// the cat escorting the caret walks on top of it, key after key with no flight, and comes
/// down to the draft's second line at the wrap in one bound. Until then it walked the
/// composer's line under the rule's strokes and waited short of the label (b3ad799ce1),
/// drawn under the line where the person could not see it.
#[test]
fn typed_up_to_a_label_on_the_rule_the_cat_walks_on_top_of_the_line() {
    let mut s = resting_above_the_composer();
    let draft = "the quick brown fox jumps over the lazy dog ".repeat(4);
    let chrome_off = |chrome: Vec<PetRect>| {
        let lower = chrome[2].row;
        vec![
            PetRect::new(11.0, 0.0, 1.0, 120.0),
            PetRect::new(11.0, 88.0, 1.0, 32.0),
            PetRect::new(lower, 3.0, 1.0, 5.0),
            PetRect::new(lower, 84.0, 1.0, 35.0),
        ]
    };
    let mut hops = Hops::default();
    let mut on_line = None;
    let mut off_line = Vec::new();
    let mut under = Vec::new();
    let mut wrap_leaps = None;
    for n in 1..=125usize {
        s.pet.note_console_input(s.now, PetInputKind::Text);
        let (bottom, chrome) = composer_with(&draft[..n]);
        s.term.process(bottom.as_bytes());
        s.chrome = chrome_off(chrome);
        if n == 119 {
            wrap_leaps = Some(hops.leaps);
        }
        for _ in 0..6 {
            hops.frame(&mut s);
            let (col, row) = hops.at.expect("a frame");
            // On top of the rule: feet on row 10, or a seat held a hair above it
            // ([`PetBrain::at_console_stand`]); never lower, where the body reaches into
            // the rule's row.
            if !hops.aloft && row > 10.01 && row < 12.5 {
                under.push((n, col, row));
            }
            if n < 119 && !hops.aloft {
                if row > 9.5 && row <= 10.01 {
                    on_line = on_line.or(Some((n, hops.leaps)));
                } else if on_line.is_some() {
                    off_line.push((n, col, row, s.pet.action));
                }
            }
        }
    }
    let (from_key, leaps_on) =
        on_line.expect("fixture: the cat comes on top of the composer's upper rule");
    let before_wrap = wrap_leaps.expect("the draft wraps") - leaps_on;
    let at_wrap = hops.leaps - wrap_leaps.unwrap_or(0);
    for _ in 0..120 {
        if !hops.aloft {
            break;
        }
        hops.frame(&mut s);
    }
    let (col, row) = hops.at.expect("a frame");
    assert!(
        off_line.is_empty() && before_wrap == 0 && at_wrap == 1,
        "on top of the line from key {from_key}: {before_wrap} flights before the wrap, \
         {at_wrap} at it, frames off it {:?}",
        &off_line[..off_line.len().min(4)]
    );
    assert!(
        under.is_empty(),
        "{} ground frames on the composer's first line, under the rule (key, col, row): {:?}",
        under.len(),
        &under[..under.len().min(4)]
    );
    assert!(
        (row - 13.0).abs() < 0.01 && col >= 2.0,
        "the bound comes down to the draft's second line, at ({col:.2}, {row:.2})"
    );
}

#[test]
fn typed_to_the_right_margin_the_cat_stays_on_glass() {
    let mut s = resting_above_the_composer();
    let draft = "the quick brown fox jumps over the lazy dog ".repeat(3);
    let mut hidden = Vec::new();
    for n in 1..=118usize {
        s.pet.note_console_input(s.now, PetInputKind::Text);
        let (bottom, chrome) = composer_with(&draft[..n]);
        s.term.process(bottom.as_bytes());
        s.chrome = chrome;
        for _ in 0..6 {
            let f = s.frame();
            if f.alpha == 0 || s.pet.console.clipped {
                hidden.push((n, f.col, f.row, s.pet.console.reason));
            }
        }
    }
    assert!(
        hidden.is_empty(),
        "{} frames hid the cat (key, col, row, why): {:?}",
        hidden.len(),
        &hidden[..hidden.len().min(4)]
    );
}

#[test]
fn typing_into_an_agents_composer_never_sends_the_cat_hopping() {
    let mut s = resting_above_the_composer();
    let draft = "the quick brown fox jumps over the lazy dog ".repeat(4);
    let mut hops = Hops::default();
    let keys = (1..=150)
        .map(|n| (n, PetInputKind::Text))
        .chain((0..150).rev().map(|n| (n, PetInputKind::Delete)));
    let mut typed_leaps = 0;
    for (i, (n, kind)) in keys.enumerate() {
        if i == 150 {
            typed_leaps = hops.leaps;
        }
        s.pet.note_console_input(s.now, kind);
        let (bottom, chrome) = composer_with(&draft[..n]);
        s.term.process(bottom.as_bytes());
        s.chrome = chrome;
        for _ in 0..(2 + i % 2) {
            hops.frame(&mut s);
        }
    }
    for _ in 0..(3 * 60) {
        hops.frame(&mut s);
    }
    let deleted_leaps = hops.leaps - typed_leaps;
    assert!(
        hops.asides == 0 && typed_leaps <= 2 && deleted_leaps <= 3 && hops.missed.is_empty(),
        "the cat goes where the caret goes and lands where it aims: {typed_leaps} leaps \
         typing and {deleted_leaps} deleting, {} frames stepped aside, landings off their \
         aim {:?}",
        hops.asides,
        hops.missed
    );
    let (_, parked, last) = s.watch(6.0);
    assert!(
        parked.is_some() && s.covered(&last).is_empty(),
        "and parks on clean ground, at ({}, {})",
        last.col,
        last.row
    );
}

#[test]
fn the_live_edge_hug_never_chases_across_the_hosts_chrome() {
    let mut s = resting_above_the_composer();
    let seat = (s.pet.col, s.pet.row);
    s.burst = true;
    let mut hops = Hops::default();
    let mut armed = 0;
    for _ in 0..(4 * 60) {
        hops.frame(&mut s);
        armed += usize::from(s.pet.hug_armed);
    }
    assert!(armed > 0, "fixture: the burst armed the live-edge hug");
    assert!(
        hops.asides == 0 && hops.leaps == 0,
        "the cat watches from its seat {seat:?}: {} leaps, {} frames stepped back off \
         the rule, at ({:.2}, {:.2})",
        hops.leaps,
        hops.asides,
        s.pet.col,
        s.pet.row
    );
}

fn shell_after_output() -> String {
    let mut screen = String::new();
    for i in 12..=40 {
        screen.push_str(&format!("line {i} of the output, words words words\r\n"));
    }
    screen.push_str("$ ");
    screen
}

#[test]
fn a_growing_selection_moves_the_shell_pet_no_farther_than_home_reach() {
    let mut s = Console::new(30, 100, &shell_after_output(), Vec::new());
    let rested = s.rest(5.0);
    let home = (rested.col, rested.row);
    assert!(
        (home.1 - 29.0).abs() < 0.25,
        "fixture: at rest beside the prompt, at {home:?}"
    );
    let mut farthest = 0.0f32;
    for k in 1..=14u16 {
        let sel = s.term.text_selection_mut();
        sel.start_selection(
            29 - i32::from(k),
            2,
            SelectionSide::Left,
            SelectionType::Block,
        );
        sel.update_selection(29, 14 + 4 * k, SelectionSide::Right);
        for _ in 0..18 {
            let f = s.frame();
            if f.alpha > 0 {
                farthest = farthest.max((f.col - home.0).abs().max((f.row - home.1).abs() * 2.0));
            }
        }
    }
    assert!(
        farthest <= HOME_REACH + ARRIVED,
        "seen no farther than HOME_REACH from its stand, got {farthest}"
    );
    s.term.text_selection_mut().clear();
    let back = s.rest(4.0);
    assert!(
        (back.col - home.0).abs() <= ARRIVED && (back.row - home.1).abs() < 0.25,
        "back at its stand {home:?}, got ({}, {})",
        back.col,
        back.row
    );
}

#[test]
fn a_selection_over_the_rest_seat_hides_the_cat_in_place() {
    let mut s = Console::agent(
        16,
        120,
        &claude_screen(120, 5, &[]),
        claude_chrome(120, 5, true),
    );
    s.rest(6.0);
    s.repaint(&claude_screen(120, 11, &TURN), claude_chrome(120, 11, true));
    let (_, parked, rested) = s.watch(6.0);
    assert!(parked.is_some(), "fixture: parked");
    let seat = (rested.col, rested.row);
    assert!(
        seat.1 < 11.0 && s.covered(&rested).is_empty(),
        "fixture: at rest beside the turn's last line, at {seat:?}"
    );
    let mut moved = 0.0f32;
    for k in 1..=8u16 {
        let sel = s.term.text_selection_mut();
        sel.start_selection(10, 0, SelectionSide::Left, SelectionType::Block);
        sel.update_selection(10 - i32::from(k), 20 + 9 * k, SelectionSide::Right);
        for _ in 0..20 {
            s.frame();
            moved = moved.max(
                (s.pet.col - seat.0)
                    .abs()
                    .max((s.pet.row - seat.1).abs() * 2.0),
            );
        }
    }
    assert!(
        moved <= ARRIVED,
        "the cat keeps its seat under the selection, moved {moved}"
    );
    s.term.text_selection_mut().clear();
    let (_, parked, back) = s.watch(3.0);
    assert!(
        parked.is_some()
            && back.alpha > 0
            && (back.col - seat.0).abs() <= ARRIVED
            && (back.row - seat.1).abs() < 0.25,
        "back on its seat {seat:?}, seen: ({}, {}) alpha {}",
        back.col,
        back.row,
        back.alpha
    );
}

#[test]
fn the_autopilot_menu_over_the_cat_hides_it_in_place() {
    let mut s = Console::agent(
        40,
        120,
        &claude_screen(120, 5, &[]),
        claude_chrome(120, 5, true),
    );
    let rested = s.rest(6.0);
    let seat = (rested.col, rested.row);
    assert!(
        s.covered(&rested).is_empty() && seat.1 > 8.5,
        "fixture: at rest under the footer, at {seat:?}"
    );
    s.overlays = vec![PetRect::new(8.0, 2.0, 11.0, 40.0)];
    let mut moved = 0.0f32;
    let mut seen = 0;
    for _ in 0..(3.0 * 60.0) as usize {
        let f = s.frame();
        moved = moved.max(
            (s.pet.col - seat.0)
                .abs()
                .max((s.pet.row - seat.1).abs() * 2.0),
        );
        if f.alpha > 0 {
            seen += 1;
        }
    }
    assert!(
        moved <= ARRIVED,
        "the cat keeps its seat under the menu, moved {moved}"
    );
    assert!(seen < 30, "and yields there, seen {seen} frames");
    s.overlays.clear();
    let (_, parked, back) = s.watch(3.0);
    assert!(
        parked.is_some()
            && back.alpha > 0
            && (back.col - seat.0).abs() <= ARRIVED
            && (back.row - seat.1).abs() < 0.25,
        "the menu closed, it is on its seat {seat:?}: ({}, {}) alpha {}",
        back.col,
        back.row,
        back.alpha
    );
}

/// A SEAT GOES WITH ITS CARET. Claude Code's first frame of a composer can carry a
/// placeholder on the prompt line (`Try "refactor upgrade_drive.rs"`): the stand beside the
/// caret lies past its ink, under the harness's rule, and the cat is seated on top of the
/// rule there, 30 cells along the composer. The placeholder cleared, the stand is back by
/// the prompt; the seat goes with it, on top of the rule over the prompt, where the first
/// key will find the cat. Kept anywhere in reach while still clean, it held the cat 34
/// cells from the prompt, and the first key cost a flight back.
///
/// NEGATIVE CONTROL (as run before the fix): the cat stayed at column 35.2.
#[test]
fn a_seat_taken_past_a_placeholder_goes_over_the_prompt_when_it_clears() {
    let screen = |placeholder: bool| {
        let mut screen = claude_screen(120, 11, &[]);
        if placeholder {
            screen.push_str("\x1b[13;5H\x1b[2mTry \"refactor upgrade_drive.rs\"\x1b[22m\x1b[13;3H");
        }
        screen
    };
    let mut s = Console::agent(40, 120, &screen(true), claude_chrome(120, 11, true));
    s.ink = true;
    let rested = s.rest(6.0);
    assert!(
        rested.row < 11.0 && rested.col > 25.0 && s.covered(&rested).is_empty(),
        "fixture: at rest on top of the composer past the placeholder, at ({}, {})",
        rested.col,
        rested.row
    );
    s.repaint(&screen(false), claude_chrome(120, 11, true));
    let (_, parked, last) = s.watch(4.0);
    assert!(
        parked.is_some() && last.row < 11.0 && last.col < 10.0 && s.covered(&last).is_empty(),
        "the seat goes over the prompt: parked {parked:?} at ({}, {})",
        last.col,
        last.row
    );
}

#[test]
fn a_command_list_redrawn_at_every_key_moves_the_resting_cat_once() {
    let list = |rows: usize| {
        let mut screen = claude_screen(120, 5, &[]);
        for row in 9..9 + rows {
            screen.push_str(&format!(
                "\x1b[{};1H    /command-{row:<16}A long description of what the command does, \
                 and when to use it",
                row + 1
            ));
        }
        screen.push_str("\x1b[7;3H");
        screen
    };
    let mut s = Console::agent(
        40,
        120,
        &claude_screen(120, 5, &[]),
        claude_chrome(120, 5, true),
    );
    let rested = s.rest(6.0);
    assert!(
        s.covered(&rested).is_empty() && rested.row < 12.0,
        "fixture: at rest under the fresh footer, at ({}, {})",
        rested.col,
        rested.row
    );
    s.repaint(&list(16), claude_chrome(120, 5, true));
    let (_, parked, moved) = s.watch(4.0);
    assert!(
        parked.is_some() && moved.row > 25.0 && s.covered(&moved).is_empty(),
        "the long list took its seat: it went past the list's end, at ({}, {})",
        moved.col,
        moved.row
    );
    let seat = (moved.col, moved.row);
    for rows in [12, 8, 4] {
        s.repaint(&list(rows), claude_chrome(120, 5, true));
        let (leaps, _, last) = s.watch(3.0);
        assert!(
            leaps == 0 && (last.col - seat.0).abs() <= ARRIVED && (last.row - seat.1).abs() < 0.25,
            "a {rows}-row list leaves its seat {seat:?} clean: it stays, {leaps} leap \
             frames, at ({}, {})",
            last.col,
            last.row
        );
    }
    s.repaint(&list(20), claude_chrome(120, 5, true));
    let (_, parked, last) = s.watch(4.0);
    assert!(
        parked.is_some() && last.row > 29.0 && s.covered(&last).is_empty(),
        "a longer list over its seat moves it again, past the end, at ({}, {})",
        last.col,
        last.row
    );
}

#[test]
fn the_rest_seat_never_takes_the_gutter_of_a_command_list() {
    let list = [
        (
            9,
            "  /cd                         Change the working directory",
        ),
        (10, "  /clear                      Clear the conversation"),
        (11, "  /compact                    Compact the conversation"),
        (12, "  /config                     Open the settings"),
        (13, "  /copy                       Copy the last reply"),
    ];
    let mut screen = claude_screen(120, 5, &[]);
    for (row, text) in list {
        screen.push_str(&format!("\x1b[{};1H{text}", row + 1));
    }
    screen.push_str("\x1b[7;3H");
    let mut s = Console::agent(40, 120, &screen, claude_chrome(120, 5, true));
    let last = s.rest(6.0);
    let covered = s.covered(&last);
    assert!(covered.is_empty(), "no glyph under the cat: {covered:?}");
    let (x0, x1, y0, y1) = last.body_px(10, 20, 120, 40).expect("on glass");
    let cols = x0.div_euclid(10)..(x1 + 9).div_euclid(10);
    for row in y0.div_euclid(20)..(y1 + 19).div_euclid(20) {
        let Some((_, text)) = list.iter().find(|(r, _)| *r == row) else {
            continue;
        };
        let first = text.find(|c: char| c != ' ').unwrap_or(0) as i32;
        let end = text.trim_end().chars().count() as i32;
        assert!(
            cols.end <= first || cols.start >= end,
            "off the list's lines, not in their gutter: columns {cols:?} on row {row}, \
             whose words run {first}..{end}, the cat at ({}, {})",
            last.col,
            last.row
        );
    }
}

#[test]
fn full_transcript_above_the_composer_keeps_an_under_text_seat_without_hopping_or_bobbing() {
    let mut s = resting_above_the_composer();
    let full = "Claude wrote this line all the way to the margin of the pane and on ".repeat(2);
    let dense: Vec<(usize, &str)> = (5..=10).map(|row| (row, &full[..120])).collect();
    let (bottom, chrome) = composer_with("");
    s.repaint(
        &format!("{}{bottom}", claude_screen(120, 11, &dense)),
        chrome,
    );
    let rested = s.rest(4.0);
    assert!(
        matches!(rested.action, PetAction::Sit | PetAction::Loaf),
        "fixture: seated on the nearest under-text ground, got ({}, {}) {:?}",
        rested.col,
        rested.row,
        rested.action
    );
    let draft = "the quick brown fox jumps over the lazy dog ".repeat(2);
    let mut hops = Hops::default();
    let mut stands = 0usize;
    let mut contacts = 0usize;
    for (i, n) in (1..=60).chain((0..30).rev()).enumerate() {
        let kind = if i < 60 {
            PetInputKind::Text
        } else {
            PetInputKind::Delete
        };
        s.pet.note_console_input(s.now, kind);
        let (bottom, chrome) = composer_with(&draft[..n]);
        s.term.process(bottom.as_bytes());
        s.chrome = chrome;
        for _ in 0..3 {
            hops.frame(&mut s);
            if !hops.aloft && s.pet.action == PetAction::Stand {
                stands += 1;
            }
            if s.pet.console_attention() == PetAttention::Contact {
                contacts += 1;
            }
        }
    }
    assert!(
        hops.asides == 0 && hops.leaps <= 2 && hops.missed.is_empty(),
        "reachable under-text seat above the rule is kept: {} leaps, {} cursor-home asides, \
         missed {:?}",
        hops.leaps,
        hops.asides,
        hops.missed
    );
    assert_eq!(
        contacts, 0,
        "typing in a hidden-cursor composer never braces the cat as foreign contact"
    );
    assert!(
        stands <= 30,
        "seated cat on its rest seat does not pop into Stand on every key (got {stands} Stand frames)"
    );
}

#[test]
fn interactive_typing_in_executing_block_holds_escort_lead_and_keeps_caret_gaze_past_loaf_after() {
    let mut s = Scene::new();
    s.executing();
    s.type_text("hello world");
    assert!(
        s.pet.held_lead > ARRIVED,
        "fixture: forward typing run earned a held lead ({})",
        s.pet.held_lead
    );
    let frames = s.watch(3.5);
    assert!(
        s.pet.console.resident && !s.pet.needs_frames(),
        "resident holds the escort's led stand and parks without oscillating (resident={}, needs_frames={})",
        s.pet.console.resident,
        s.pet.needs_frames()
    );
    let last = frames.last().expect("frames");
    assert_eq!(
        last.pose,
        PetGlyphId::PetSit,
        "interactive session in an executing block keeps PetSit past LOAF_AFTER without snapping to PetEdgeLean"
    );
}

#[test]
fn gentle_pointer_stroke_pets_settled_console_resident_and_returns_to_watch() {
    let mut s = Scene::new();
    s.executing();
    let _ = s.watch(1.1);
    let col = s.pet.col;
    let row = s.pet.row;
    let mut frames = Vec::new();
    for step in 0..=30 {
        let u = step as f32 / 30.0;
        let px = col + 1.0 + u * 3.2;
        let py = row - 0.1;
        s.now += Duration::from_secs_f32(0.016);
        let input = s.term.cell_frame(20, 80);
        let facts = PetWorldFacts::read(&s.term, 17);
        s.pet.observe_console(&input, &facts, PetPane::full(&input));
        s.pet.set_console_presentable(true);
        let c = s.term.cursor();
        frames.push(s.pet.tick(PetSense {
            caret_drawn: true,
            now: s.now,
            caret: Some((c.row, c.col)),
            wrapped: false,
            rows: 20,
            cols: 80,
            cell_w: 10,
            cell_h: 20,
            reduced_motion: false,
            output_burst: false,
            pointer: Some((px, py)),
        }));
    }
    frames.extend(s.watch(2.5));
    assert!(
        frames.iter().any(|f| f.action == PetAction::Purr),
        "gentle pointer stroke over a settled console resident earns Purr"
    );
    assert!(
        s.pet.console.resident && s.pet.console_attention() == PetAttention::Output,
        "resident watch resumes once the affection hold finishes"
    );
}

#[test]
fn terminal_bell_flinches_settled_console_resident_and_returns_to_watch() {
    let mut s = Scene::new();
    s.executing();
    assert!(!s.pet.needs_frames());
    s.pet.note_bell(s.now);
    assert!(
        s.pet.needs_frames(),
        "pending bell wakes the frame train even while a console resident is parked"
    );
    let first = s.frame(0.016);
    assert!(
        !s.pet.console.resident && (first.action == PetAction::Perk || s.pet.flinch_t > 0.0),
        "bell flinches the resident instead of being silently swallowed (resident={}, action={:?}, flinch_t={})",
        s.pet.console.resident,
        first.action,
        s.pet.flinch_t
    );
    let _ = s.watch(1.5);
    assert!(
        s.pet.console.resident && s.pet.console_attention() == PetAttention::Output,
        "resident watch resumes after the bell flinch completes"
    );
}

#[test]
fn exploring_content_perch_orients_vertical_gaze_toward_anchor_row() {
    let mut s = Scene::new();
    s.executing();
    s.term.process(b"\x1b]133;D;0\x07");
    let _ = s.watch(RESULT_HOLD + 0.2);
    s.term.process(b"\x1b[13;17H");
    for _ in 0..90 {
        s.frame(0.016);
        if s.pet.console.resident
            && s.pet.console_attention() == PetAttention::Exploring
            && !s.pet.needs_frames()
        {
            break;
        }
    }
    assert_eq!(s.pet.console_attention(), PetAttention::Exploring);
    let settled = s.frame(0.016);
    assert_eq!(
        settled.pose,
        PetGlyphId::PetEdgeLookUp,
        "content-perch below the observed anchor looks up toward the anchor row"
    );
}

#[test]
fn perch_trip_hop_cycles_rise_tuck_and_descend_poses() {
    let mut s = Scene::new();
    s.executing();
    s.term.process(b"\x1b[?25l");
    s.pet.console.cursor_home = None;
    s.pet.row -= 2.0;
    let mut poses = Vec::new();
    for _ in 0..30 {
        let f = s.frame(0.016);
        if s.pet.console.moving {
            poses.push(f.pose);
        } else if !poses.is_empty() {
            break;
        }
    }
    assert!(
        poses.contains(&PetGlyphId::PetLeapRise)
            && poses.contains(&PetGlyphId::PetHop)
            && poses.contains(&PetGlyphId::PetLeapDescend),
        "vertical PerchTrip hop should progress through rise, tuck, and descend poses: {poses:?}"
    );
}

/// A trip the console's resident sets off on, on a tick that ends a sleep, starts where the
/// body stood: the tick moves it by none of the sleep ([`PetBrain::tick`]) and asks for the
/// trip's frames, which carry it from there. Timed by the tick's span, the trip began up to a
/// tenth of a second in, on whichever tick the host next ran.
#[test]
fn a_resident_trip_a_woken_tick_begins_starts_where_the_body_stood() {
    let mut s = Scene::new();
    s.executing();
    for _ in 0..240 {
        if !s.pet.console.moving && !s.pet.needs_frames() {
            break;
        }
        s.frame(0.016);
    }
    assert!(!s.pet.needs_frames(), "fixture: the resident settles");
    s.term.process(b"\x1b[?25l");
    s.pet.console.cursor_home = None;
    s.pet.row -= 2.0;
    let at = (s.pet.col, s.pet.row);
    s.frame(2.0);
    assert!(
        s.pet.console.trip.is_some(),
        "fixture: the tick sets off on a trip"
    );
    assert_eq!(
        (s.pet.col, s.pet.row),
        at,
        "the woken tick moves the body by none of the sleep"
    );
    assert!(s.pet.needs_frames(), "and asks for the trip's frames");
    s.frame(0.016);
    assert_ne!(
        (s.pet.col, s.pet.row),
        at,
        "which carry the body from there"
    );
}

#[test]
fn room_mail_and_sibling_cues_play_over_executing_resident_and_return_to_watch() {
    let mut s = Scene::new();
    s.executing();
    s.pet.note_room_inbox(s.now, RoomInbox::default());
    s.pet.note_room_inbox(
        s.now,
        RoomInbox {
            unseen: 1,
            overdue: false,
            hold: false,
        },
    );
    assert!(
        !s.pet.needs_frames(),
        "room facts must never add a wake on their own"
    );
    let mut frames = Vec::new();
    let mut aloft = 0;
    for _ in 0..(2.0 / 0.016) as usize {
        frames.push(s.frame(0.016));
        aloft = aloft.max(s.pet.cheer_motes_aloft());
    }
    assert!(
        frames.iter().any(|f| f.action == PetAction::Perk),
        "unread mail perks the resident in place"
    );
    assert!(
        aloft >= 1,
        "unread mail spawns a musical note mote in the air"
    );
    assert!(
        s.pet.console.resident && s.pet.console_attention() == PetAttention::Output,
        "resident watch resumes once the mail beat finishes"
    );

    s.pet.note_room_sibling(s.now, Some((60.0, s.pet.row)));
    assert!(!s.pet.needs_frames());
    let first = s.frame(0.016);
    assert_eq!(first.action, PetAction::Perk);
    assert!(
        !first.facing_left,
        "resident turns toward the talking sibling pane on the right"
    );
    let _ = s.watch(1.0);
    assert!(
        s.pet.console.resident && s.pet.console_attention() == PetAttention::Output,
        "resident watch resumes once the sibling look finishes"
    );
}

#[test]
fn word_cat_peek_bats_from_executing_resident_and_returns_to_watch() {
    let mut s = Scene::new();
    s.executing();
    assert!(!s.pet.needs_frames());
    s.pet.note_peek(s.now, s.pet.col + 6.0, s.pet.row);
    assert!(
        s.pet.needs_frames(),
        "nearby word-cat peek wakes the parked resident"
    );
    let frames = s.watch(3.0);
    assert!(
        frames.iter().any(|f| f.action == PetAction::Frolic),
        "resident hops to bat/greet the nearby word-cat peek"
    );
    assert!(
        s.pet.console.resident && s.pet.console_attention() == PetAttention::Output,
        "resident returns to its watch stand after batting the word-cat"
    );
}

#[test]
fn v2_meteor_perk_and_star_catch_play_in_place_over_executing_resident() {
    let mut s = Scene::new();
    s.executing();
    let stand = (s.pet.col.to_bits(), s.pet.row.to_bits());
    let perk_at = s.now + Duration::from_millis(80);
    let perk_offer = PetOffer {
        perk_at: Some(perk_at),
        ..PetOffer::default()
    };
    assert!(s.pet.note_v2_offer(&perk_offer).is_none());
    assert!(
        !s.pet.needs_frames(),
        "v2 offer latches must never wake the frame train on their own"
    );

    let mut perked_at = None;
    for _ in 0..20 {
        let f = s.frame(0.016);
        assert!(s.pet.note_v2_offer(&perk_offer).is_none());
        if f.action == PetAction::Perk && perked_at.is_none() {
            perked_at = Some((s.now, f));
        }
    }
    let (at, perk_frame) = perked_at.expect("settled resident perks at the meteor arrival");
    assert!(at >= perk_at, "perk must not fire before perk_at");
    assert_eq!(
        (perk_frame.col.to_bits(), perk_frame.row.to_bits()),
        stand,
        "v2 perk in place must never relocate the resident"
    );
    let _ = s.watch(0.6);
    assert!(
        s.pet.console.resident && s.pet.console_attention() == PetAttention::Output,
        "resident resumes watching output after the v2 perk"
    );

    let star = StarCatch {
        x: (s.pet.col + 4.0) * 10.0,
        y: s.pet.row * 20.0,
        born: s.now,
        col: s.pet.col.round() as i32 + 4,
    };
    let catch_offer = PetOffer {
        catch: Some(star),
        ..PetOffer::default()
    };
    assert!(s.pet.note_v2_offer(&catch_offer).is_none());
    let bat_frame = s.frame(0.016);
    let caught = s.pet.note_v2_offer(&catch_offer);
    assert_eq!(
        caught,
        Some(star),
        "settled resident bats and returns the offered gold star"
    );
    assert_eq!(bat_frame.action, PetAction::Frolic);
    assert_eq!(bat_frame.pose, PetGlyphId::PetBat);
    assert_eq!(
        (bat_frame.col.to_bits(), bat_frame.row.to_bits()),
        stand,
        "v2 star catch bats in place without relocating"
    );
    let _ = s.watch(BAT_HOLD + 0.1);
    assert!(
        s.pet.console.resident && s.pet.console_attention() == PetAttention::Output,
        "resident resumes watching output after batting the star"
    );
}

#[test]
fn pointer_gaze_tracks_moving_mouse_on_settled_resident_and_idles_when_gone() {
    let mut s = Scene::new();
    s.executing();
    assert!(!s.pet.needs_frames());

    let mut looked_up_right = false;
    for step in 0..10 {
        s.now += Duration::from_secs_f32(0.016);
        let input = s.term.cell_frame(20, 80);
        let facts = PetWorldFacts::read(&s.term, 17);
        s.pet.observe_console(&input, &facts, PetPane::full(&input));
        s.pet.set_console_presentable(true);
        let c = s.term.cursor();
        let px = s.pet.col + 10.0 + step as f32 * 1.2;
        let py = (s.pet.row - 3.0).max(0.0);
        let f = s.pet.tick(PetSense {
            caret_drawn: true,
            now: s.now,
            caret: Some((c.row, c.col)),
            wrapped: false,
            rows: 20,
            cols: 80,
            cell_w: 10,
            cell_h: 20,
            reduced_motion: false,
            output_burst: false,
            pointer: Some((px, py)),
        });
        if f.pose == PetGlyphId::PetEdgeLookUp && !f.facing_left {
            looked_up_right = true;
        }
    }
    assert!(
        looked_up_right && s.pet.needs_frames(),
        "settled resident turns its gaze toward a moving pointer above and to the right"
    );

    let settled = s.frame(0.016);
    assert!(
        !s.pet.needs_frames(),
        "pointer leaving the pane immediately releases the resident's frame train"
    );
    assert_eq!(
        settled.pose,
        PetGlyphId::PetEdgeLean,
        "output resident returns to its output gaze once the pointer leaves"
    );
}

#[test]
fn interactive_caret_output_resident_flicks_ear_on_output_burst_edge_and_settles() {
    let mut s = Scene::new();
    s.executing();
    s.term.process(b"\x1b[4;17H");
    for _ in 0..120 {
        s.frame(0.016);
        if s.pet.console.resident && !s.pet.console.moving && !s.pet.needs_frames() {
            break;
        }
    }
    let base = s.frame(0.016);
    assert_eq!(s.pet.console_attention(), PetAttention::Output);
    assert_eq!(base.pose, PetGlyphId::PetSit);
    assert!(!s.pet.needs_frames());

    s.now += Duration::from_secs_f32(0.016);
    let input = s.term.cell_frame(20, 80);
    let facts = PetWorldFacts::read(&s.term, 17);
    s.pet.observe_console(&input, &facts, PetPane::full(&input));
    s.pet.set_console_presentable(true);
    let c = s.term.cursor();
    let twitch = s.pet.tick(PetSense {
        caret_drawn: true,
        now: s.now,
        caret: Some((c.row, c.col)),
        wrapped: false,
        rows: 20,
        cols: 80,
        cell_w: 10,
        cell_h: 20,
        reduced_motion: false,
        output_burst: true,
        pointer: None,
    });
    assert!(
        matches!(
            twitch.pose,
            PetGlyphId::PetSitEar | PetGlyphId::PetSitEarFar
        ),
        "interactive-caret resident flicks an ear on output burst edge, got {:?}",
        twitch.pose
    );
    assert!(s.pet.needs_frames(), "ear flick keeps the frame train live");

    let settled = *s.watch(TWITCH_DUR + 0.1).last().unwrap();
    assert_eq!(settled.pose, PetGlyphId::PetSit);
    assert!(!s.pet.needs_frames());
}

#[test]
fn interactive_caret_idle_status_bursts_do_not_loop_ear_flapping() {
    let mut s = Scene::new();
    s.executing();
    s.term.process(b"\x1b[4;17H");
    for _ in 0..120 {
        s.frame(0.016);
        if s.pet.console.resident && !s.pet.console.moving && !s.pet.needs_frames() {
            break;
        }
    }
    assert_eq!(s.frame(0.016).pose, PetGlyphId::PetSit);

    let pulse = |s: &mut Scene, dt: f32, burst: bool| -> PetFrame {
        s.now += Duration::from_secs_f32(dt);
        let input = s.term.cell_frame(20, 80);
        let facts = PetWorldFacts::read(&s.term, 17);
        s.pet.observe_console(&input, &facts, PetPane::full(&input));
        s.pet.set_console_presentable(true);
        let c = s.term.cursor();
        s.pet.tick(PetSense {
            caret_drawn: true,
            now: s.now,
            caret: Some((c.row, c.col)),
            wrapped: false,
            rows: 20,
            cols: 80,
            cell_w: 10,
            cell_h: 20,
            reduced_motion: false,
            output_burst: burst,
            pointer: None,
        })
    };

    let first = pulse(&mut s, 0.016, true);
    assert!(matches!(
        first.pose,
        PetGlyphId::PetSitEar | PetGlyphId::PetSitEarFar
    ));
    let _ = s.watch(TWITCH_DUR + 0.1);
    assert_eq!(s.frame(0.016).pose, PetGlyphId::PetSit);
    assert!(!s.pet.needs_frames());

    for gap in [0.4_f32, 0.4, 1.0, 1.0, 5.0, 5.0] {
        let f = pulse(&mut s, gap, true);
        assert_eq!(
            f.pose,
            PetGlyphId::PetSit,
            "idle spinner/clock output burst after {gap}s must not flap ears"
        );
        assert!(
            !s.pet.needs_frames(),
            "ignored idle status burst after {gap}s must not wake the frame train"
        );
        assert_eq!(pulse(&mut s, 0.016, false).pose, PetGlyphId::PetSit);
    }

    s.input(PetInputKind::Text);
    let _ = s.watch(2.0);
    assert_eq!(s.frame(0.016).pose, PetGlyphId::PetSit);
    let rearmed = pulse(&mut s, 0.016, true);
    assert!(
        matches!(
            rearmed.pose,
            PetGlyphId::PetSitEar | PetGlyphId::PetSitEarFar
        ),
        "genuine console input rearms a single ear flick once cooldown has elapsed, got {:?}",
        rearmed.pose
    );
    let _ = s.watch(TWITCH_DUR + 0.1);
    assert_eq!(pulse(&mut s, 1.0, true).pose, PetGlyphId::PetSit);

    for (step, gap) in [0.5_f32, 1.0, 5.0, 5.0].into_iter().enumerate() {
        let col1 = 17 + (step & 1);
        s.term.process(format!("\x1b[4;{col1}H").as_bytes());
        let moved = pulse(&mut s, 0.016, true);
        assert!(
            !matches!(moved.pose, PetGlyphId::PetSitEar | PetGlyphId::PetSitEarFar),
            "program-driven spinner cursor jitter must not flap ears on move"
        );
        let _ = s.watch(0.4);
        let f = pulse(&mut s, gap, true);
        assert!(
            !matches!(f.pose, PetGlyphId::PetSitEar | PetGlyphId::PetSitEarFar),
            "program-driven spinner cursor jitter after {gap}s must not rearm ear flapping, got {:?}",
            f.pose
        );
        assert_eq!(s.pet.twitch_t, 0.0);
    }
}

#[test]
fn blinking_unboxed_faux_composer_caret_does_not_oscillate_idle_pose() {
    let mut s = Scene::new();
    s.executing();
    s.term.process(
        "\x1b[6;1Hstreaming output above composer\
         \x1b[19;1H> hello\u{2588}            \
         \x1b[20;1H  26 files changed\x1b[?25l"
            .as_bytes(),
    );
    let step = |s: &mut Scene, dt: f32| -> PetFrame {
        s.now += Duration::from_secs_f32(dt);
        let input = s.term.cell_frame(20, 80);
        let facts = PetWorldFacts::read(&s.term, 17);
        s.pet.observe_console(&input, &facts, PetPane::full(&input));
        s.pet.set_console_presentable(true);
        let c = s.term.cursor();
        s.pet.tick(PetSense {
            caret_drawn: s.term.cursor_visible(),
            now: s.now,
            caret: Some((c.row, c.col)),
            wrapped: false,
            rows: 20,
            cols: 80,
            cell_w: 10,
            cell_h: 20,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
        })
    };
    for _ in 0..160 {
        let _ = step(&mut s, 0.02);
        if s.pet.console.resident && !s.pet.console.moving && !s.pet.needs_frames() {
            break;
        }
    }
    assert_eq!(s.pet.console.faux_caret, Some((18, 7)));
    assert_eq!(step(&mut s, 0.016).pose, PetGlyphId::PetSit);

    for phase in 0..8 {
        if phase % 2 == 0 {
            s.term.process(b"\x1b[19;8H \x1b[20;19H\x1b[?25l");
        } else {
            s.term
                .process("\x1b[19;8H\u{2588}\x1b[20;19H\x1b[?25l".as_bytes());
        }
        for _ in 0..25 {
            let f = step(&mut s, 0.02);
            assert_eq!(
                s.pet.console.faux_caret,
                Some((18, 7)),
                "blinking unboxed composer caret must hold across blink phase {phase}"
            );
            assert_eq!(
                f.pose,
                PetGlyphId::PetSit,
                "blinking unboxed composer caret must not flip pose on phase {phase}"
            );
        }
    }

    s.term
        .process(b"\x1b[19;1H  running checks... \x1b[20;19H\x1b[?25l");
    let _ = step(&mut s, 0.02);
    assert_eq!(
        s.pet.console.faux_caret, None,
        "replacing the composer row must drop the held faux caret immediately"
    );
}

#[test]
fn protected_surface_resident_retires_pending_bell_and_look_and_regrid_shifts_console_trip() {
    let mut s = Scene::new();
    s.term.process(b"\x1b[3;3Hread this");
    s.term
        .text_selection_mut()
        .start_selection(0, 0, SelectionSide::Left, SelectionType::Block);
    s.term
        .text_selection_mut()
        .update_selection(19, 79, SelectionSide::Right);
    for _ in 0..240 {
        s.frame(0.016);
        if !s.pet.needs_frames() {
            break;
        }
    }
    assert_eq!(s.pet.console.reason, "protected-surface");
    assert!(!s.pet.needs_frames());

    s.pet.note_bell(s.now);
    s.pet.note_peek(s.now, s.pet.col + 20.0, s.pet.row);
    assert!(s.pet.pending_look.is_some());
    assert!(s.pet.needs_frames());
    s.frame(0.016);
    assert!(
        s.pet.pending_bell.is_none() && s.pet.pending_look.is_none() && !s.pet.needs_frames(),
        "protected-surface yield must retire pending_bell and pending_look on its tick"
    );

    s.pet.row = 5.0;
    s.pet.console.target = Some(PetRect::new(4.0, 10.0, 1.7, 3.4));
    s.pet.console.trip = Some(PerchTrip {
        from: (10.0, 5.0),
        to: (14.0, 6.0),
        elapsed: 0.1,
        duration: 0.4,
        hop: false,
    });
    assert!(s.pet.carry_with_regrid(3.0, 20));
    assert_eq!(s.pet.console.target.unwrap().row, 7.0);
    let trip = s.pet.console.trip.unwrap();
    assert_eq!((trip.from.1, trip.to.1), (8.0, 9.0));
}

/// The composer of [`composer_with`] with its upper rule on `rule`: the rule, the draft's
/// lines under it, the lower rule, the mode line, the caret hidden at the draft's end.
fn composer_at(rule: usize, cols: usize, draft: &str) -> String {
    let stroke = "\u{2500}".repeat(cols);
    let chars: Vec<char> = draft.chars().collect();
    let lines: Vec<String> = if chars.is_empty() {
        vec![String::new()]
    } else {
        chars.chunks(cols - 2).map(|c| c.iter().collect()).collect()
    };
    let mut out = String::new();
    for row in rule..rule + 9 {
        out.push_str(&format!("\x1b[{};1H\x1b[2K", row + 1));
    }
    out.push_str(&format!("\x1b[{};1H{stroke}", rule + 1));
    for (i, line) in lines.iter().enumerate() {
        let lead = if i == 0 { "\u{276F} " } else { "  " };
        out.push_str(&format!("\x1b[{};1H{lead}{line}", rule + 2 + i));
    }
    let last = lines.len() - 1;
    let caret_col = 2 + lines[last].chars().count();
    let lower = rule + 1 + lines.len();
    out.push_str(&format!(
        "\x1b[{};1H{stroke}\x1b[{};1H  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle)\x1b[{};{}H\x1b[?25l",
        lower + 1,
        lower + 2,
        rule + 2 + last,
        caret_col + 1
    ));
    out
}

/// The host's chrome over a composer whose upper rule is on `rule`, as `PetChrome` reserves
/// it with the harness on: the tool's lockup and the lit switch on the upper rule and the
/// rule's whole run they are repainted with, the chips on the lower rule `lower`, and the
/// card when `card`.
fn harness_chrome(cols: usize, rule: usize, lower: usize, card: bool) -> Vec<PetRect> {
    let cols = cols as f32;
    let mut chrome = vec![
        PetRect::new(rule as f32, 2.0, 1.0, 11.0),
        PetRect::new(rule as f32, cols - 21.0, 1.0, 19.0),
        PetRect::new(rule as f32, 0.0, 1.0, cols),
        PetRect::new(lower as f32, 3.0, 1.0, 5.0),
        PetRect::new(lower as f32, cols - 36.0, 1.0, 35.0),
    ];
    if card {
        chrome.push(PetRect::new(0.0, 0.0, 5.0, 87.0));
    }
    chrome
}

/// What typing `keys` letters into the composer on `rule` showed, six frames a key.
#[derive(Debug, Default)]
struct Typed {
    /// Frames on glass with the body on the ground (no flight).
    drawn: usize,
    /// Frames the cat was off glass.
    hidden: usize,
    /// Ground frames whose body reached into the upper rule's row or covered the host's
    /// chrome: (key, col, row, under_ink, body y0..y1 px).
    under: Vec<(usize, f32, f32, bool, i32, i32)>,
    /// Ground frames farther than [`TYPED_REACH`] columns from the caret: (key, caret col, col, row).
    far: Vec<(usize, u16, f32, f32)>,
}

const TYPED_REACH: f32 = 12.0;

fn type_into(
    s: &mut Console,
    rule: usize,
    keys: usize,
    chrome: &dyn Fn(usize) -> Vec<PetRect>,
) -> Typed {
    let draft = "the quick brown fox jumps over the lazy dog ".repeat(3);
    let cols = usize::from(s.cols);
    let band = (rule as i32 * 20, rule as i32 * 20 + 20);
    let mut t = Typed::default();
    for n in 1..=keys {
        s.pet.note_console_input(s.now, PetInputKind::Text);
        let typed = &draft[..n];
        let lines = typed.chars().count().div_ceil(cols - 2).max(1);
        s.term.process(composer_at(rule, cols, typed).as_bytes());
        s.chrome = chrome(rule + 1 + lines);
        for _ in 0..6 {
            let f = s.frame();
            if f.alpha == 0 {
                t.hidden += 1;
                continue;
            }
            if f.action == PetAction::Leap {
                continue;
            }
            t.drawn += 1;
            let (x0, x1, y0, y1) = f.body_px(10, 20, s.cols, s.rows).expect("on glass");
            let covers = |r: &PetRect| {
                let (rx0, rx1) = ((r.col * 10.0) as i32, ((r.col + r.cols) * 10.0) as i32);
                let (ry0, ry1) = ((r.row * 20.0) as i32, ((r.row + r.rows) * 20.0) as i32);
                x0 < rx1 && x1 > rx0 && y0 < ry1 && y1 > ry0
            };
            if (y0 < band.1 && y1 > band.0) || s.chrome.iter().any(covers) {
                t.under.push((n, f.col, f.row, f.under_ink, y0, y1));
            }
            let caret = s.term.cursor().col;
            if (f.col - f32::from(caret)).abs() > TYPED_REACH {
                t.far.push((n, caret, f.col, f.row));
            }
        }
    }
    t
}

/// THE HEAD LAW AT THE HOST'S LINE, TYPED THROUGH (2026-10-24, the owner: "the cursor kitty
/// pet is drawn BELOW the top line and I can't see it"). The resident pet escorting the caret
/// along a Claude Code composer whose upper rule the host holds whole (its lockup, its switch
/// and the rule's run it repaints them with, as `PetChrome` holds them) is never drawn with
/// its body in the rule's row or over the host's chrome, and never off glass: over a
/// transcript it walks on top of the rule; with the card right over the composer, where the
/// top of the rule has no room, it stays clear of the line where it is seen. Before the fix
/// the escort passed under the chrome over its line: 314 of its 334 ground frames over the
/// transcript, and 336 of 343 under the card, had its head in the rule's row.
#[test]
fn typing_into_a_hosted_composer_never_draws_the_cat_under_its_upper_rule() {
    let pane = |rule: usize| {
        let mut s = Console::agent(
            16,
            120,
            &claude_screen(120, 5, &[]),
            claude_chrome(120, 5, true),
        );
        s.ink = true;
        s.rest(6.0);
        if rule != 5 {
            s.repaint(
                &format!(
                    "{}{}",
                    claude_screen(120, rule, &TURN),
                    composer_at(rule, 120, "")
                ),
                harness_chrome(120, rule, rule + 2, true),
            );
            s.rest(8.0);
        }
        s
    };
    for (name, rule) in [("over a transcript", 11usize), ("under the card", 5)] {
        let mut s = pane(rule);
        let chrome = move |lower: usize| harness_chrome(120, rule, lower, true);
        let t = type_into(&mut s, rule, 60, &chrome);
        assert!(
            t.drawn > 300 && t.hidden == 0,
            "{name}: on glass for every key: {} frames drawn, {} hidden",
            t.drawn,
            t.hidden
        );
        assert!(
            t.under.is_empty(),
            "{name}: {} of {} ground frames under the composer's upper rule or the host's \
             chrome (key, col, row, under ink, body y0..y1 px): {:?}",
            t.under.len(),
            t.drawn,
            &t.under[..t.under.len().min(6)]
        );
    }
}

/// THE STAND OVER THE HOST'S LINE ([`PetBrain::over_the_hosts_line`]) is one a hold keeps:
/// over a composer whose upper rule the host holds whole, the stand lifted off the caret's
/// line is a perch ([`LINE_PERCH_GAP`]), its body clear by the clearance all round
/// ([`PetBrain::console_may_stand`]), so a repair's watch or a resident's seat holds the
/// cat there; at the caret's column, over no letters. And no stand is taken in a rule
/// ([`PetWorld::rule_line`]): over a reserved line that is the composer's first, the upper
/// rule on the row over it, the stand is the ladder's. NEGATIVE CONTROLS (as run before
/// the fix): with the soles on the line (the stand on row 10, not 9.85) the hold judged
/// the stand blocked; with no rule check the stand went up into the rule's row.
#[test]
fn the_stand_over_the_hosts_line_is_a_perch_a_hold_keeps_and_never_in_a_rule() {
    let width = art_cols(10, 20);
    let mut s = Console::agent(
        16,
        120,
        &format!(
            "{}{}",
            claude_screen(120, 11, &TURN),
            composer_at(11, 120, "")
        ),
        harness_chrome(120, 11, 13, true),
    );
    for _ in 0..3 {
        s.frame();
    }
    let world = s.pet.console.world.clone().expect("observed");
    assert_eq!(
        world.host_line(11),
        Some(true),
        "fixture: the upper rule is held"
    );
    let (col, row) = s
        .pet
        .over_the_hosts_line(&world, (40.0, 12.0), (16, 120), width)
        .expect("over a transcript the stand goes on top of the line");
    assert!(
        (row - (10.0 - LINE_PERCH_GAP)).abs() < 1e-4 && (col - 40.0).abs() < 1e-4,
        "a perch over the line at the caret's column, got ({col}, {row})"
    );
    let body = PetBrain::console_stand_rect((col, row), 16, width);
    assert!(
        PetBrain::console_may_stand(&world, body, CLEARANCE, false),
        "a hold keeps the stand over the line ({col}, {row})"
    );

    // A draft two lines long whose first line the host holds whole, under the upper rule.
    let draft = "the quick brown fox jumps over the lazy dog ".repeat(4);
    s.repaint(
        &format!(
            "{}{}",
            claude_screen(120, 11, &TURN),
            composer_at(11, 120, &draft)
        ),
        vec![PetRect::new(12.0, 0.0, 1.0, 120.0)],
    );
    for _ in 0..3 {
        s.frame();
    }
    let world = s.pet.console.world.clone().expect("observed");
    assert_eq!(
        (world.host_line(12), world.rule_line(11)),
        (Some(true), Some(true)),
        "fixture: the draft's first line is held, the upper rule over it"
    );
    assert_eq!(
        s.pet
            .over_the_hosts_line(&world, (60.0, 13.0), (16, 120), width),
        None,
        "no stand is taken in the composer's upper rule"
    );
}

/// How a host fills the scratch it hands the pet each frame.
#[derive(Clone, Copy, Debug)]
enum Host {
    /// The single-pane window redraw without chrome rows
    /// (`App::redraw_window_with_layout`): the snapshot reused while it is
    /// the terminal's current frame, else a damage-consuming refill.
    Reuse,
    /// A split's focused pane: a damage-consuming refill every frame.
    Refill,
    /// A window with a chrome row (the tab strip): the
    /// last frame's row prepend undone, a damage-consuming refill, and the
    /// row prepended again once the pet has observed the engine rows
    /// ([`Twin::observe`]).
    Banded,
}

/// The host's fill of `scratch`, and what it tells the pet of it.
fn redraw_fill(
    term: &mut Terminal,
    scratch: &mut aterm_core::render::RenderInput,
    host: Host,
) -> PetFill {
    let rows = usize::from(term.grid().rows());
    let cols = usize::from(term.grid().cols());
    let epoch = term.damage_epoch();
    let current = !term.has_damage()
        && scratch.terminal_id == term.render_identity()
        && scratch.snapshot_seq == epoch
        && scratch.engine_fill_seq == scratch.snapshot_seq
        && scratch.process_sequence == term.pipeline_timestamps().process_sequence
        && scratch.rows == rows
        && scratch.cols == cols
        && scratch.cells.len() == rows
        && i64::from(scratch.display_offset)
            == i64::try_from(term.grid().display_offset()).unwrap_or(i64::MAX)
        && scratch.base_y == i64::try_from(term.grid().base_y()).unwrap_or(i64::MAX)
        && scratch.absolute_row_revision == term.absolute_row_revision()
        && scratch.selection == *term.text_selection();
    if matches!(host, Host::Reuse) && current {
        return PetFill::Consumed;
    }
    if scratch.row_shift > 0 && !scratch.undo_host_row_prepend(rows, &mut Vec::new(), 1) {
        scratch.cells.truncate(rows);
    }
    let prior = scratch.extract_gen;
    let refill = term.cell_frame_damage_scoped_into(scratch, rows, cols);
    PetFill::after_refill(prior, refill)
}

/// One screen handed each frame to a brain that keeps its reading and to
/// one that forgets it before every observation.
struct Twin {
    host: Host,
    term: Terminal,
    scratch: aterm_core::render::RenderInput,
    kept: PetBrain,
    fresh: PetBrain,
    now: Instant,
    kept_frames: usize,
    frames: usize,
}

impl Twin {
    fn new(host: Host) -> Self {
        let mut term = Terminal::new(20, 80);
        term.process(
            b"\x1b]11;#000000\x07$ make\r\n\x1b[41m  \x1b[0m built 3 crates\r\n$ \x1b[3;3H",
        );
        let now = Instant::now();
        let brain = || PetBrain {
            col: 20.0,
            row: 5.0,
            alpha: 1.0,
            action: PetAction::Sit,
            quiet: 2.0,
            last_caret: Some((2, 2)),
            last_now: Some(now),
            ..PetBrain::default()
        };
        Self {
            host,
            term,
            scratch: aterm_core::render::RenderInput::default(),
            kept: brain(),
            fresh: brain(),
            now,
            kept_frames: 0,
            frames: 0,
        }
    }

    /// A pane past the map's budget, `rows` by `cols` ([`crate::pet_world::MAX_WORLD_ROWS`],
    /// [`crate::pet_world::MAX_WORLD_COLS`]): its rows 2 and 4 (counted from 1) lettered to
    /// the margin, a body seated between them at the 21st column, the caret in row 3's third
    /// column.
    fn past_the_budget(host: Host, (rows, cols): (u16, u16)) -> Self {
        let mut t = Self::new(host);
        let row: String = "the quick brown fox jumps over the lazy dog "
            .repeat(usize::from(cols))
            .chars()
            .take(usize::from(cols))
            .collect();
        t.term = Terminal::new(rows, cols);
        t.term.process(
            format!("\x1b]11;#000000\x07\x1b[2;1H{row}\x1b[4;1H{row}\x1b[3;3H").as_bytes(),
        );
        for brain in [&mut t.kept, &mut t.fresh] {
            brain.row = 2.0;
        }
        t
    }

    /// The host fills, both brains observe, and what each holds is equal.
    fn observe(&mut self, what: &str, reserve: PetReserve<'_>) {
        let host = self.host;
        let fill = redraw_fill(&mut self.term, &mut self.scratch, host);
        let facts = PetWorldFacts::read(&self.term, 17);
        let pane = PetPane::full(&self.scratch);
        self.kept
            .observe_console_with_reserve(&self.scratch, &facts, pane, reserve, fill);
        self.fresh.console.reading.forget();
        self.fresh
            .observe_console_with_reserve(&self.scratch, &facts, pane, reserve, fill);
        assert!(!self.fresh.console.reading.kept());
        if matches!(host, Host::Banded) {
            let band = vec![
                aterm_core::terminal::RenderCell {
                    ch: '!',
                    ..aterm_core::terminal::RenderCell::default()
                };
                self.scratch.cols
            ];
            self.scratch.prepend_host_rows(
                1,
                std::iter::once(band.as_slice()),
                20,
                0,
                &mut Vec::new(),
                aterm_core::render::HostRowPixels::Placed,
            );
        }
        self.frames += 1;
        self.kept_frames += usize::from(self.kept.console.reading.kept());
        let (k, f) = (&self.kept.console, &self.fresh.console);
        assert_eq!(
            format!("{:?}", k.world),
            format!("{:?}", f.world),
            "{host:?} {what}: the world"
        );
        assert_eq!(k.row_sig, f.row_sig, "{host:?} {what}: the row signatures");
        assert_eq!(
            k.row_sig_prev, f.row_sig_prev,
            "{host:?} {what}: the rows before"
        );
        assert_eq!(
            k.faux_caret, f.faux_caret,
            "{host:?} {what}: the faux caret"
        );
        assert_eq!(
            k.observed_seq, f.observed_seq,
            "{host:?} {what}: the observation count"
        );
        assert_eq!(
            k.contact_changed, f.contact_changed,
            "{host:?} {what}: the contact"
        );
    }

    /// Both brains tick and draw the same frame.
    fn tick(&mut self, what: &str) {
        self.now += Duration::from_millis(16);
        let c = self.term.cursor();
        let sense = PetSense {
            caret_drawn: true,
            now: self.now,
            caret: (self.term.cursor_visible() && self.term.grid().display_offset() == 0)
                .then_some((c.row, c.col)),
            wrapped: false,
            rows: self.term.grid().rows(),
            cols: self.term.grid().cols(),
            cell_w: 10,
            cell_h: 20,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
        };
        self.kept.set_console_presentable(true);
        self.fresh.set_console_presentable(true);
        let (a, b) = (self.kept.tick(sense), self.fresh.tick(sense));
        assert_eq!(
            format!("{a:?}"),
            format!("{b:?}"),
            "{:?} {what}: the frame drawn",
            self.host
        );
    }

    fn frame(&mut self, what: &str, reserve: PetReserve<'_>) {
        self.observe(what, reserve);
        self.tick(what);
    }

    fn frames(&mut self, what: &str, n: usize) {
        for _ in 0..n {
            self.frame(what, PetReserve::default());
        }
    }

    /// `n` frames, of which at least `n - 1` keep the reading.
    fn kept_frames_of(&mut self, what: &str, n: usize) {
        let before = self.kept_frames;
        self.frames(what, n);
        assert!(
            self.kept_frames - before + 1 >= n,
            "{:?} {what}: an unchanged screen keeps its reading ({} of {n})",
            self.host,
            self.kept_frames - before
        );
    }
}

#[test]
fn a_kept_console_reading_draws_what_a_fresh_one_does() {
    for host in [Host::Reuse, Host::Refill, Host::Banded] {
        let mut t = Twin::new(host);
        t.frames("idle", 8);
        for key in b"ls -la" {
            t.term.process(&[*key]);
            t.frames("typing", 3);
        }
        t.term.process(b"\r\nCargo.toml  src\r\n$ ");
        t.frames("output", 4);
        t.term
            .set_palette_color(1, aterm_types::Rgb { r: 0, g: 0, b: 0 });
        t.frames("a palette entry", 3);
        t.term.set_default_background(aterm_types::Rgb {
            r: 12,
            g: 14,
            b: 16,
        });
        t.frames("the theme's background", 3);
        // Another consumer takes the damage of a palette change (the red
        // cells now wear the default background) without reading the
        // epoch: no stamp but the extraction generation moves.
        t.term.set_palette_color(
            1,
            aterm_types::Rgb {
                r: 12,
                g: 14,
                b: 16,
            },
        );
        t.term.take_damage();
        t.frames("another consumer took the damage", 3);
        // The same, and the host's next fill is one the pet does not
        // observe, so the fill after it re-extracts no row of cells the
        // reading held was not taken from.
        t.term
            .set_palette_color(1, aterm_types::Rgb { r: 200, g: 0, b: 0 });
        t.term.take_damage();
        let _ = redraw_fill(&mut t.term, &mut t.scratch, host);
        t.frames("a taken damage the pet did not see", 3);
        t.term.text_selection_mut().start_selection(
            1,
            4,
            SelectionSide::Left,
            SelectionType::Simple,
        );
        t.term
            .text_selection_mut()
            .update_selection(1, 12, SelectionSide::Right);
        t.frames("a selection", 3);
        t.term.text_selection_mut().clear();
        t.frames("no selection", 3);
        t.term.process(b"\x1b[2;5H");
        t.frames("a caret move", 3);
        t.term.process(b"\x1b[?25l");
        t.frames("a hidden caret", 4);
        // Claude Code's composer: the caret hidden on the last row below
        // a prompt row that draws its own block caret.
        t.term
            .process("\x1b[2J\x1b[19;1H> abc\u{2588}\x1b[20;1H".as_bytes());
        t.kept_frames_of("a composer's faux caret", 5);
        assert_eq!(
            t.kept.console.faux_caret,
            Some((18, 5)),
            "{host:?}: the composer's drawn caret is read"
        );
        t.term.process(b"\x1b[?25h\x1b[3;3H");
        t.frames("the caret shown", 3);
        // New ink by the body is contact; a second observation of the
        // same screen before the tick keeps it.
        for brain in [&mut t.kept, &mut t.fresh] {
            brain.console.last_body = Some(PetRect::new(9.3, 40.0, 1.7, 5.6));
        }
        t.term.process(b"\x1b[11;42HX\x1b[3;3H");
        t.observe("ink by the body", PetReserve::default());
        assert!(
            t.kept.console.contact_changed,
            "{host:?}: new ink by the body is contact"
        );
        let before = t.kept_frames;
        t.observe("ink by the body, seen again", PetReserve::default());
        assert_eq!(
            t.kept_frames,
            before + 1,
            "{host:?}: the second observation keeps the reading"
        );
        assert!(t.kept.console.contact_changed);
        t.tick("ink by the body");
        t.frames("after contact", 3);
        // A fill the pet does not observe (a frame it was not console
        // owned) names cells the reading it holds was not taken from.
        t.term.process(b"\x1b[12;1Hunseen");
        let _ = redraw_fill(&mut t.term, &mut t.scratch, host);
        t.frames("a frame the pet did not see", 3);
        t.term.process(b"\x1b[?25h\x1b[20;1H");
        for line in 0..30 {
            t.term.process(format!("line {line}\r\n").as_bytes());
        }
        t.frames("a scrolled page", 3);
        t.term.scroll_display(3);
        t.frames("history", 3);
        t.term.scroll_to_bottom();
        t.frames("the live bottom", 3);
        let rect = [PetRect::new(4.0, 30.0, 2.0, 12.0)];
        for (what, reserve) in [
            ("chrome", PetReserve::protected(&rect)),
            (
                "an overlay",
                PetReserve {
                    overlays: &rect,
                    ..PetReserve::default()
                },
            ),
            (
                "typed ink",
                PetReserve {
                    ink: &rect,
                    ..PetReserve::default()
                },
            ),
            ("no reserve", PetReserve::default()),
        ] {
            for _ in 0..3 {
                t.frame(what, reserve);
            }
        }
        t.term.resize(24, 90);
        t.frames("a resize", 3);
        t.term.process(b"\x1b[?1049hfull screen\x1b[5;9H");
        t.frames("the other screen", 3);
        t.term.process(b"\x1b[?1049l");
        t.frames("back", 3);
        assert!(
            t.kept_frames * 2 > t.frames,
            "{host:?}: the frames repeat a screen more often than not ({} of {})",
            t.kept_frames,
            t.frames
        );
    }
    // Past the map's budget the console reads the rows about the body in a second window
    // ([`crate::pet_world::PetWorld::body_window`]), which moves with the body while the
    // screen stands still: on a pane 100 rows tall and one 300 columns wide, a caret sent
    // far from the body and back, and far again, 120 frames each, the body following it.
    for host in [Host::Reuse, Host::Refill, Host::Banded] {
        let far: [((u16, u16), &[u8]); 2] =
            [((100, 80), b"\x1b[80;3H"), ((20, 300), b"\x1b[12;290H")];
        for (grid, far) in far {
            let mut t = Twin::past_the_budget(host, grid);
            t.frames("settled", 240);
            let mut read = 0;
            for (what, at) in [("far", far), ("home", b"\x1b[3;3H"), ("far again", far)] {
                t.term.process(at);
                for _ in 0..120 {
                    t.frames(what, 1);
                    read += usize::from(matches!(
                        t.kept.console.world.as_deref(),
                        Some(w) if w.body_window().rows > 0
                    ));
                }
            }
            assert!(
                read >= 40 && t.kept_frames >= 400,
                "fixture, {host:?} on {grid:?}: the window about the body read in {read} \
                 frames, the reading kept in {} of {}",
                t.kept_frames,
                t.frames
            );
        }
    }
}

#[test]
fn an_unchanged_screen_is_read_once() {
    for host in [Host::Reuse, Host::Refill, Host::Banded] {
        let mut t = Twin::new(host);
        t.frames("first", 1);
        let world = format!("{:?}", t.kept.console.world);
        assert_eq!(
            t.kept.observed_cells(),
            20 * 80,
            "{host:?}: the first frame reads the pane"
        );
        for _ in 0..12 {
            t.frames("unchanged", 1);
            assert_eq!(
                t.kept.observed_cells(),
                0,
                "{host:?}: an unchanged frame reads no cell"
            );
            assert!(t.kept.console.reading.kept());
            assert_eq!(format!("{:?}", t.kept.console.world), world);
        }
        assert_eq!(t.kept_frames, 12);
        let facts = PetWorldFacts::read(&t.term, 17);
        let input = t.term.cell_frame(20, 80);
        t.kept
            .observe_console(&input, &facts, PetPane::full(&input));
        assert_eq!(
            t.kept.observed_cells(),
            20 * 80,
            "{host:?}: an unconsumed fill of the same screen is read"
        );
    }
}

#[test]
fn a_pet_faded_out_in_history_reads_no_cell_until_the_screen_moves_and_reads_afresh_back_live() {
    for host in [Host::Reuse, Host::Refill, Host::Banded] {
        let mut t = Twin::new(host);
        t.term.process(b"\x1b[20;1H");
        for line in 0..40 {
            t.term.process(format!("line {line}\r\n").as_bytes());
        }
        t.frames("a scrolled page", 3);
        t.term.scroll_display(3);
        t.frames("parked: the pet fades out", 60);
        assert!(
            !t.kept.is_active(),
            "{host:?}: precondition: no caret in history, so the pet faded out"
        );
        for i in 0..12 {
            t.frames("parked, unchanged", 1);
            assert_eq!(
                t.kept.observed_cells(),
                0,
                "{host:?}: an unchanged parked frame reads no cell (frame {i})"
            );
            assert!(!t.kept.is_active(), "{host:?}: still faded out");
        }
        // What the pet reads while it is not drawn: a selection in history
        // wakes it to read along, so an inactive pet still classifies a
        // screen that moved.
        t.term.text_selection_mut().start_selection(
            -2,
            0,
            SelectionSide::Left,
            SelectionType::Simple,
        );
        t.term
            .text_selection_mut()
            .update_selection(-2, 6, SelectionSide::Right);
        t.frames("a selection in history", 1);
        assert_eq!(
            t.kept.observed_cells(),
            20 * 80,
            "{host:?}: a selection made while the pet is not drawn is read"
        );
        t.frames("reading the selection", 30);
        assert!(
            t.kept.is_active(),
            "{host:?}: the selection read while faded out brought the pet back"
        );
        t.term.text_selection_mut().clear();
        t.frames("parked, the selection gone", 60);
        assert!(!t.kept.is_active(), "{host:?}: faded out again");
        t.frames("parked, unchanged again", 1);
        assert_eq!(t.kept.observed_cells(), 0, "{host:?}: no cell read");
        t.term.scroll_to_bottom();
        t.frames("back to the live bottom", 1);
        assert_eq!(
            t.kept.observed_cells(),
            20 * 80,
            "{host:?}: the frame the pet comes back on reads the screen afresh"
        );
        // The tick that sees the caret back ends the pet's sleep and fades it in from there:
        // drawn from the frame after ([`PetBrain::tick`]).
        t.frames("fading back in", 1);
        assert!(
            t.kept.is_active(),
            "{host:?}: the caret is back, so is the pet"
        );
    }
}

/// The cells and grid [`Dozing`] draws in.
const DOZE_CELL: (u16, u16) = (15, 28);
const DOZE_GRID: (u16, u16) = (108, 35);
/// How many entries of a [`drawn`] key the body takes.
const DOZE_BODY_KEY: usize = 8;
/// How many entries of a [`drawn`] key each mote takes, its slot first.
const DOZE_MOTE_KEY: usize = 8;

/// A shell's scrollback above a prompt near the top: each z rises into the ink, and the
/// console hides it for its whole life.
const DOZE_INK: &[u8] =
    b"\x1b]11;#000000\x07$ echo hi\r\nhi\r\n$ echo hi\r\nhi\r\n$ echo hi\r\nhi\r\n$ ";

/// What the host draws of `f` (`WordDecorations::pet_cursor`) in `cell`-sized cells on a
/// `grid`: the body's pose, look and pixel rect, then each mote it draws, in draw order, by
/// slot, kind, pixel box, rotation bucket, alpha and tint. Empty while the lane is off.
fn drawn(f: &PetFrame, (cell_w, cell_h): (u16, u16), (cols, rows): (u16, u16)) -> Vec<i64> {
    let mut key = Vec::new();
    if f.lane_alpha == 0 {
        return key;
    }
    if let Some((x0, x1, y0, y1)) = f.body_px(cell_w, cell_h, cols, rows) {
        key.extend([
            f.pose as i64,
            i64::from(f.alpha),
            i64::from(f.facing_left),
            i64::from(f.under_ink),
        ]);
        key.extend([x0, x1, y0, y1].map(i64::from));
    }
    if f.alpha > 0 {
        for (slot, m) in f.motes.iter().enumerate() {
            let Some(m) = m else {
                continue;
            };
            let alpha = (f32::from(m.alpha) * (f32::from(f.alpha) / 255.0)) as u8;
            let Some((x, y, side)) =
                mote_box_px((m.col, m.row), m.scale, (cell_w, cell_h), (cols, rows))
            else {
                continue;
            };
            if alpha == 0 {
                continue;
            }
            let turn = m.rot / core::f32::consts::TAU;
            let bucket = ((turn - turn.floor()) * 12.0) as i64 % 12;
            key.extend([
                slot as i64,
                m.kind as i64,
                i64::from(x),
                i64::from(y),
                i64::from(side),
                bucket,
                i64::from(alpha),
                m.rgb.map_or(-1, i64::from),
            ]);
        }
    }
    key
}

/// A pet beside a shell prompt in a console, ticked by hand.
struct Dozing {
    term: Terminal,
    pet: PetBrain,
    now: Instant,
    /// The console's cells, in pixels.
    cell: (u16, u16),
    /// The console's columns and rows.
    grid: (u16, u16),
    /// The console's cells, filled as the shipped redraw fills them ([`redraw_fill`]): reused
    /// while current, else a damage-consuming refill. A tick over the same cells keeps the
    /// pet's reading ([`PetFill`]).
    scratch: aterm_core::render::RenderInput,
    /// The console's facts as the shell last left them, read once a write ([`Self::write`]).
    seen: Option<PetWorldFacts>,
    /// What the last tick drew ([`drawn`]).
    last_key: Vec<i64>,
}

/// One tick of [`Dozing::run`].
struct Doze {
    /// When it ran.
    at: Instant,
    /// What the host drew ([`drawn`]).
    key: Vec<i64>,
    /// Whether it ran at the brain's offer, not on a frame train.
    offered: bool,
    /// Whether a mote was born or dropped.
    motes_moved: bool,
    /// Whether it ran at the brain's offer and dropped a mote the brain would have dropped
    /// more than [`ARM_MIN`] before.
    dropped_late: bool,
    /// Whether the console drew a mote it hid the tick before.
    revealed: bool,
    /// Whether the console hid a mote it drew the tick before.
    covered: bool,
    /// How many motes the console hid.
    hidden: usize,
    /// Whether a rhythm run's lead carried the pet ahead ([`PetBrain::carrying_run`]).
    carrying: bool,
    /// Whether the shell wrote one of [`Dozing::run`]'s edits on it.
    edit: bool,
    /// What the rest has done to the pet by the tick's end: its content, how far its gait
    /// has slowed, and whether the gait has rested a full dwell ([`GAIT_DWELL`]).
    rested: (f32, f32, bool),
}

/// When [`Dozing::run`] ticks the pet.
#[derive(Clone, Copy)]
enum Schedule {
    /// At the brain's offers, and this often while it asks for frames.
    Offered(Duration),
    /// Every [`DENSE`], whatever the brain asks, and at its offers: the ticks every schedule
    /// runs, with a tick at most [`DENSE`] after any moment between them.
    Dense,
}

impl Dozing {
    /// [`Self::in_console`] in [`DOZE_CELL`] cells on a [`DOZE_GRID`].
    fn new(screen: &[u8], start: Instant, old_schedule: bool) -> Self {
        Self::in_console(screen, start, old_schedule, DOZE_CELL, DOZE_GRID)
    }

    /// The pet beside the caret `screen` leaves in a console of `cell`-sized cells on a `grid`,
    /// before its first tick, at `start`.
    fn unticked(screen: &[u8], start: Instant, cell: (u16, u16), grid: (u16, u16)) -> Self {
        let (cols, rows) = grid;
        let mut term = Terminal::new(rows, cols);
        term.process(screen);
        Self {
            term,
            pet: PetBrain::default(),
            now: start,
            cell,
            grid,
            scratch: aterm_core::render::RenderInput::default(),
            seen: None,
            last_key: Vec::new(),
        }
    }

    /// The pet beside the caret `screen` leaves in a console of `cell`-sized cells on a `grid`,
    /// ticked at 60 Hz from `start` until it has slept [`SLEEP_AFTER`]. `old_schedule` keeps
    /// the hidden motes' own edges in its offers.
    fn in_console(
        screen: &[u8],
        start: Instant,
        old_schedule: bool,
        cell: (u16, u16),
        grid: (u16, u16),
    ) -> Self {
        let mut doze = Self::unticked(screen, start, cell, grid);
        doze.pet.console.offer_hidden_motes = old_schedule;
        doze.tick();
        while !(doze.pet.action == PetAction::Sleep && doze.pet.quiet >= SLEEP_AFTER) {
            doze.now += Duration::from_millis(16);
            doze.tick();
            assert!(
                doze.now - start < Duration::from_secs(90),
                "the pet never slept: {:?}",
                doze.pet.action
            );
        }
        doze
    }

    /// [`Self::unticked`], awake: ticked at 60 Hz from `start` for `warm`.
    fn awake(
        screen: &[u8],
        start: Instant,
        cell: (u16, u16),
        grid: (u16, u16),
        warm: Duration,
    ) -> Self {
        let mut doze = Self::unticked(screen, start, cell, grid);
        doze.tick();
        while doze.now - start < warm {
            doze.now += Duration::from_millis(16);
            doze.tick();
        }
        doze
    }

    /// The shell writes `bytes`.
    fn write(&mut self, bytes: &[u8]) {
        self.term.process(bytes);
        self.seen = None;
    }

    fn tick(&mut self) -> PetFrame {
        let ((cell_w, cell_h), (cols, rows)) = (self.cell, self.grid);
        let fill = redraw_fill(&mut self.term, &mut self.scratch, Host::Reuse);
        let term = &self.term;
        let facts = self
            .seen
            .get_or_insert_with(|| PetWorldFacts::read(term, 7));
        self.pet.observe_console_with_reserve(
            &self.scratch,
            facts,
            PetPane::full(&self.scratch),
            PetReserve::protected(&[]),
            fill,
        );
        self.pet.set_console_presentable(true);
        let c = self.term.cursor();
        let frame = self.pet.tick(PetSense {
            caret_drawn: true,
            now: self.now,
            caret: Some((c.row, c.col)),
            wrapped: false,
            rows,
            cols,
            cell_w,
            cell_h,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
        });
        self.last_key = drawn(&frame, self.cell, self.grid);
        frame
    }

    /// Ticks the pet at its own offers, or at 60 Hz while it asks for frames, until its breath
    /// window has closed and deep sleep offers nothing.
    fn breathe(&mut self) -> Vec<Doze> {
        self.run(&[], Schedule::Offered(Duration::from_millis(16)), None)
    }

    /// Ticks the pet on `schedule` from [`Self::now`], while the shell writes each of `edits`
    /// (bytes, with how long after the start), each on a tick of its own; for `horizon`, or
    /// without one, until deep sleep offers nothing.
    fn run(
        &mut self,
        edits: &[(Duration, Vec<u8>)],
        schedule: Schedule,
        horizon: Option<Duration>,
    ) -> Vec<Doze> {
        assert!(
            horizon.is_some() || matches!(schedule, Schedule::Offered(_)),
            "a dense host runs to a horizon"
        );
        let t0 = self.now;
        let mut edits = edits.iter().peekable();
        let mut out = Vec::new();
        loop {
            let offered = matches!(schedule, Schedule::Offered(_)) && !self.pet.needs_frames();
            let next = match schedule {
                Schedule::Dense if self.pet.needs_frames() => Some(self.now + DENSE),
                Schedule::Dense => Some(
                    self.pet
                        .next_change_deadline(self.now)
                        .map_or(self.now + DENSE, |at| at.min(self.now + DENSE)),
                ),
                Schedule::Offered(_) if offered => self.pet.next_change_deadline(self.now),
                Schedule::Offered(train) => Some(self.now + train),
            };
            let past = |at: Instant| horizon.is_some_and(|h| at >= t0 + h);
            if let Some((after, bytes)) = edits
                .next_if(|(after, _)| !past(t0 + *after) && next.is_none_or(|n| n >= t0 + *after))
            {
                self.now = (t0 + *after).max(self.now);
                self.write(bytes);
                self.step(false, &mut out);
                if let Some(d) = out.last_mut() {
                    d.edit = true;
                }
                continue;
            }
            let Some(at) = next else {
                assert!(
                    self.pet.quiet >= SLEEP_AFTER + BREATH_WINDOW,
                    "only deep sleep offers no change"
                );
                return out;
            };
            if past(at) {
                return out;
            }
            self.now = at;
            self.step(offered, &mut out);
            assert!(
                self.now - t0 < Duration::from_secs(60),
                "the breath window closes"
            );
        }
    }

    /// Ticks the pet at [`Self::now`] and notes the tick in `out`.
    fn step(&mut self, offered: bool, out: &mut Vec<Doze>) {
        let ids = |pet: &PetBrain| pet.motes.map(|m| m.map(|m| (m.seed, m.born.to_bits())));
        let (motes, before) = (self.pet.motes, ids(&self.pet));
        let hid = self.pet.console.hidden_motes;
        self.tick();
        let after = ids(&self.pet);
        let hides = self.pet.console.hidden_motes;
        let kept = |i: usize| before[i].is_some() && before[i] == after[i];
        let earlier = self.pet.clock - ARM_MIN.as_secs_f64();
        out.push(Doze {
            at: self.now,
            key: self.last_key.clone(),
            offered,
            motes_moved: before != after,
            dropped_late: offered
                && (0..PET_MOTES_MAX).any(|i| {
                    before[i] != after[i]
                        && motes[i]
                            .is_some_and(|m| self.pet.mote_sprite(&m, m.age(earlier)).is_none())
                }),
            revealed: (0..PET_MOTES_MAX).any(|i| kept(i) && hid[i] && !hides[i]),
            covered: (0..PET_MOTES_MAX).any(|i| kept(i) && !hid[i] && hides[i]),
            hidden: hides.iter().filter(|h| **h).count(),
            carrying: self.pet.carrying_run(),
            edit: false,
            rested: (
                self.pet.content,
                self.pet.gait_pace,
                self.pet.gait_rest >= GAIT_DWELL,
            ),
        });
    }

    /// Where the pet rests: on the pixel its home draws at, or on the front seat it keeps on
    /// purpose beside the caret (`seat_kept`, [`PECK_CLEAR`] past its station); else how far
    /// from home's pixel, in pixels.
    fn rest(&self) -> Result<Rest, f32> {
        let p = &self.pet;
        let cw = f32::from(self.cell.0);
        if (p.col * cw).round() == (p.home * cw).round() {
            return Ok(Rest::Home);
        }
        let width = art_cols(self.cell.0, self.cell.1);
        let caret = f32::from(self.term.cursor().col);
        let seat =
            (caret + STATION_LEAD + PECK_CLEAR).min((f32::from(self.grid.0) - width).max(0.0));
        if p.sit_front && p.facing_left && p.col == seat {
            return Ok(Rest::FrontSeat);
        }
        Err((p.col - p.home) * cw)
    }
}

/// Where [`Dozing::rest`] found the pet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rest {
    Home,
    FrontSeat,
}

/// The offered ticks of `run` that drew the frame before them and kept every mote.
fn idle_wakes(run: &[Doze]) -> usize {
    run.windows(2)
        .filter(|w| w[1].offered && w[1].key == w[0].key && !w[1].motes_moved)
        .count()
}

#[test]
fn a_dozing_pet_does_not_wake_for_the_zs_the_ink_hides() {
    let start = Instant::now();
    for old_schedule in [false, true] {
        let run = Dozing::new(DOZE_INK, start, old_schedule).breathe();
        assert!(
            run.iter().filter(|d| d.hidden > 0).count() > 20,
            "the brain keeps z's the console hides through the breath window"
        );
        assert!(
            run.iter().all(|d| d.key.len() == DOZE_BODY_KEY),
            "the console draws none of them"
        );
        let idle = idle_wakes(&run);
        if old_schedule {
            assert!(
                idle > 100,
                "the every-step schedule wakes for each hidden z's steps: {idle} idle wakes"
            );
        } else {
            assert_eq!(
                idle, 0,
                "each tick the pet asks for draws a new frame, or births or drops a z"
            );
        }
    }
}

/// A seeded linear congruential generator, for [`doze_case`].
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    /// A draw from `lo..=hi`.
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo + 1)
    }
}

/// The cells the dense sweep draws in, in pixels.
const DOZE_CELLS: [(u16, u16); 9] = [
    (15, 28),
    (8, 16),
    (7, 15),
    (9, 19),
    (10, 22),
    (12, 25),
    (16, 33),
    (6, 13),
    (11, 23),
];

/// The dense sweep's console, in columns and rows.
const SWEEP_GRID: (u16, u16) = (100, 30);

/// A word's row, column and length near the prompt at `(prow, pcol)`, for [`doze_page`].
fn doze_near(rng: &mut Lcg, (prow, pcol): (u64, u64)) -> (u64, u64, usize) {
    (
        rng.range(prow.saturating_sub(5).max(1), prow),
        rng.range(pcol.saturating_sub(4).max(1), pcol + 14),
        rng.range(1, 6) as usize,
    )
}

/// [`doze_case`]'s page: up to six words of up to six letters around a prompt, and the
/// prompt's row and column.
fn doze_page(rng: &mut Lcg) -> (String, (u64, u64)) {
    let prow = rng.range(6, 20);
    let pcol = rng.range(5, 80);
    let mut screen = String::from("\x1b]11;#000000\x07");
    for _ in 0..rng.range(1, 6) {
        let (r, c, len) = doze_near(rng, (prow, pcol));
        screen.push_str(&format!("\x1b[{r};{c}H{}", "x".repeat(len)));
    }
    screen.push_str(&format!("\x1b[{prow};{pcol}H$ "));
    (screen, (prow, pcol))
}

/// A dense sweep case: up to six words of up to six letters around a prompt, which a z rises
/// past, into or clear of; and, if `edit`, up to four edits mid-doze that write over, blank
/// out or scroll the page near it.
fn doze_case(rng: &mut Lcg, edit: bool) -> (String, Vec<(Duration, Vec<u8>)>) {
    let (screen, (prow, pcol)) = doze_page(rng);
    let near = |rng: &mut Lcg| doze_near(rng, (prow, pcol));
    let mut edits = Vec::new();
    if edit {
        for _ in 0..rng.range(1, 4) {
            let at = Duration::from_millis(rng.range(50, 9500));
            let (r, c, len) = near(rng);
            let bytes = match rng.range(0, 3) {
                0 => format!("\x1b7\x1b[{r};{c}H{}\x1b8", "y".repeat(len)),
                1 => format!("\x1b7\x1b[{r};{c}H{}\x1b8", " ".repeat(len)),
                2 => "\x1b7\x1b[1;1H\x1b[1J\x1b8".to_owned(),
                _ => "\x1b7\x1b[1;1H\x1b[1L\x1b8".to_owned(),
            };
            edits.push((at, bytes.into_bytes()));
        }
        edits.sort_by_key(|e| e.0);
    }
    (screen, edits)
}

/// What a key the shell echoes beside the prompt at `(prow, pcol)` writes: about one in nine
/// a backspace, one in nine a jump of the caret to a cell near the prompt, the rest a letter.
fn typed_key(rng: &mut Lcg, (prow, pcol): (u64, u64)) -> Vec<u8> {
    match rng.range(0, 8) {
        0 => b"\x08".to_vec(),
        1 => {
            let row = rng.range(prow.saturating_sub(5).max(1), prow + 3);
            let col = rng.range(pcol.saturating_sub(15).max(1), pcol + 15);
            format!("\x1b[{row};{col}H").into_bytes()
        }
        _ => b"a".to_vec(),
    }
}

/// A typing sweep case: [`doze_case`]'s page, and one to three bursts of 1 to 14 keys
/// ([`typed_key`]) echoed 40 to 260 ms apart, the first from 0.1 to 0.4 s in and each after a
/// pause of 0.3 to 7 s. A third of the time a new prompt line follows the last key, up to
/// 0.9 s after it. Returns the page, the edits, and when the last lands.
fn typing_case(rng: &mut Lcg) -> (String, Vec<(Duration, Vec<u8>)>, Duration) {
    let (screen, prompt) = doze_page(rng);
    let mut edits = Vec::new();
    let mut t = rng.range(100, 400);
    for burst in 0..rng.range(1, 3) {
        if burst > 0 {
            t += rng.range(300, 7000);
        }
        for _ in 0..rng.range(1, 14) {
            edits.push((Duration::from_millis(t), typed_key(rng, prompt)));
            t += rng.range(40, 260);
        }
    }
    if rng.range(0, 2) == 0 {
        edits.push((
            Duration::from_millis(t + rng.range(100, 900)),
            b"\r\n$ ".to_vec(),
        ));
    }
    let last = edits.last().map_or(Duration::ZERO, |e| e.0);
    (screen, edits, last)
}

/// A wake sweep case: [`doze_case`]'s page, and one to three keys ([`typed_key`]) 40 ms to
/// 1.5 s apart, the first from 0.2 to 14 s into the pet's sleep: in its breath window or past
/// it. Returns the page, the keys, and when the last lands.
fn wake_case(rng: &mut Lcg) -> (String, Vec<(Duration, Vec<u8>)>, Duration) {
    let (screen, prompt) = doze_page(rng);
    let mut keys = Vec::new();
    let mut t = rng.range(200, 14_000);
    for _ in 0..rng.range(1, 3) {
        keys.push((Duration::from_millis(t), typed_key(rng, prompt)));
        t += rng.range(40, 1500);
    }
    let last = keys.last().map_or(Duration::ZERO, |e| e.0);
    (screen, keys, last)
}

/// 2⁻⁸ s, exact in `f32` and in [`Duration`]: a host ticking this often sums the brain's
/// `f32` clocks exactly.
const DENSE: Duration = Duration::from_nanos(3_906_250);

/// How far apart the dense sweep lets its two hosts draw the same change: two [`DENSE`]
/// ticks. Both tick at every offer, arming exactly ([`ConsoleLife::exact_arms`]), and on a
/// [`DENSE`] train while the brain asks for frames; the dense host also ticks between, at
/// most a [`DENSE`] after any moment. A change one host meets on such a tick and the other
/// at the offer after it, or a train one starts on such a tick, parts them by under a
/// [`DENSE`]; a change timed from one of those moments, by under two. A change neither the
/// train nor an offer names parts them until the offered host next ticks: tens of
/// milliseconds, or for good.
const DENSE_SLACK: Duration = Duration::from_nanos(2 * 3_906_250);

/// How far apart the dense sweep lets its two hosts' rest clocks run ([`Doze::rested`]) at an
/// edit, which both tick at its moment. They spend the same rest in different pieces, which
/// `f32` rounds apart by a few millionths over a run. A span of rest one of them skipped parts
/// them by what it rests: the gait pace it slows, and the content it tires, by more than this
/// for any span over 15 ms ([`CONTENT_DECAY`]).
const REST_SLACK: f32 = 1e-3;

/// What [`drawn`] key `key` holds of `view`: the body's entries, or mote slot `slot`'s.
fn view_of(key: &[i64], view: Option<usize>) -> &[i64] {
    match view {
        None => &key[..key.len().min(DOZE_BODY_KEY)],
        Some(slot) => key
            .get(DOZE_BODY_KEY..)
            .unwrap_or_default()
            .chunks(DOZE_MOTE_KEY)
            .find(|m| m[0] == slot as i64)
            .unwrap_or_default(),
    }
}

/// Where `run`'s drawing of `view` changes, from `first`, the frame up before its first tick:
/// each change's time, and what it drew from then.
fn changes<'a>(
    first: (Instant, &'a [i64]),
    run: &'a [Doze],
    view: Option<usize>,
) -> Vec<(Instant, &'a [i64])> {
    let mut out = vec![(first.0, view_of(first.1, view))];
    for d in run {
        let k = view_of(&d.key, view);
        if out.last().is_none_or(|(_, last)| *last != k) {
            out.push((d.at, k));
        }
    }
    out
}

/// What a run's [`changes`] draw at `t`.
fn drawn_at<'a>(c: &[(Instant, &'a [i64])], t: Instant) -> Option<&'a [i64]> {
    let i = c.partition_point(|x| x.0 <= t);
    i.checked_sub(1).map(|i| c[i].1)
}

/// The first of `a`'s [`changes`] whose frame `b` draws at no moment within `slack` of it.
/// Only a change to a frame `a` holds for `slack` or longer counts: a shorter one can fall
/// between two of `b`'s ticks.
fn unmatched<'a>(
    a: &[(Instant, &'a [i64])],
    b: &[(Instant, &[i64])],
    slack: Duration,
) -> Option<(Instant, &'a [i64])> {
    let held = |i: usize| a.get(i + 1).is_none_or(|next| next.0 - a[i].0 >= slack);
    let drawn_near = |(at, key): (Instant, &[i64])| {
        let from = b.partition_point(|c| c.0 + slack <= at).saturating_sub(1);
        b[from..]
            .iter()
            .enumerate()
            .take_while(|(_, c)| c.0 <= at + slack)
            .any(|(j, c)| c.1 == key && b.get(from + j + 1).is_none_or(|end| end.0 + slack >= at))
    };
    (1..a.len())
        .find(|&i| held(i) && !drawn_near(a[i]))
        .map(|i| a[i])
}

/// What a dense sweep saw across its cases.
#[derive(Default)]
struct Sweep {
    /// Ticks the console drew a mote it hid the tick before.
    revealed: usize,
    /// Ticks the console hid a mote it drew the tick before.
    covered: usize,
    /// Cases whose edits moved the pet's home.
    rehomed: usize,
    /// Cases in which a rhythm run's lead carried the pet ahead.
    carried: usize,
    /// Runs that ended on the pixel home draws at.
    at_home: usize,
    /// Runs that ended on the front seat ([`Rest::FrontSeat`]).
    front_seat: usize,
}

/// A dense sweep case: the pet `make` builds, ticked for `horizon` by a host at its offers
/// (its frame train every [`DENSE`]) and by one that ticks every [`DENSE`] between them too
/// ([`Schedule::Dense`]), while the shell writes `edits`. The two draw the body, and each
/// mote, alike to within [`DENSE_SLACK`], and meet each edit as rested ([`REST_SLACK`]); the
/// offered host drops each mote on time; and each pet ends on the pixel its home draws at,
/// or on its front seat.
fn dense_case(
    label: &str,
    make: &dyn Fn() -> Dozing,
    edits: &[(Duration, Vec<u8>)],
    horizon: Duration,
    sweep: &mut Sweep,
) {
    let mut hosts = [Schedule::Offered(DENSE), Schedule::Dense].map(|host| {
        let mut doze = make();
        doze.pet.console.exact_arms = true;
        (host, doze)
    });
    let (t0, first, home) = (
        hosts[0].1.now,
        hosts[0].1.last_key.clone(),
        hosts[0].1.pet.home,
    );
    let [offered, dense] = hosts
        .each_mut()
        .map(|(host, doze)| doze.run(edits, *host, Some(horizon)));
    assert!(
        !offered.iter().any(|d| d.dropped_late),
        "{label}: the offered host drops each mote as it ends"
    );
    for view in [None].into_iter().chain((0..PET_MOTES_MAX).map(Some)) {
        let what = view.map_or_else(|| "body".to_owned(), |slot| format!("mote {slot}"));
        let runs = [
            ("offered", changes((t0, &first), &offered, view)),
            ("dense", changes((t0, &first), &dense, view)),
        ];
        for (one, other) in [(&runs[0], &runs[1]), (&runs[1], &runs[0])] {
            if let Some((at, key)) = unmatched(&one.1, &other.1, DENSE_SLACK) {
                panic!(
                    "{label}: {:?} in, the {} host draws {what} as {key:?}, which the {} host \
                     does not within {DENSE_SLACK:?}: it draws {:?}",
                    at - t0,
                    one.0,
                    other.0,
                    drawn_at(&other.1, at),
                );
            }
        }
    }
    let rests = |run: &[Doze]| {
        run.iter()
            .filter(|d| d.edit)
            .map(|d| (d.at, d.rested))
            .collect::<Vec<_>>()
    };
    for ((at, a), (_, b)) in rests(&offered).into_iter().zip(rests(&dense)) {
        assert!(
            (a.0 - b.0).abs() <= REST_SLACK && (a.1 - b.1).abs() <= REST_SLACK && a.2 == b.2,
            "{label}: {:?} in, at an edit, the offered host's pet has rested to {a:?} (content, \
             gait pace, gait dwelt), the dense host's to {b:?}",
            at - t0,
        );
    }
    for (host, (_, doze)) in ["offered", "dense"].into_iter().zip(&hosts) {
        match doze.rest() {
            Ok(Rest::Home) => sweep.at_home += 1,
            Ok(Rest::FrontSeat) => sweep.front_seat += 1,
            Err(px) => panic!(
                "{label}: the {host} host's pet rests {px} px from home's pixel, {:?}",
                doze.pet.action
            ),
        }
    }
    sweep.revealed += dense.iter().filter(|d| d.revealed).count();
    sweep.covered += dense.iter().filter(|d| d.covered).count();
    sweep.rehomed += usize::from(hosts[1].1.pet.home != home);
    sweep.carried += usize::from(dense.iter().any(|d| d.carrying));
}

/// `case` on each of `cases` on the tests' replay pool ([`crate::test_pool::par_map`]), each
/// into a [`Sweep`] of its own, and the sums: every case runs and reports whatever another
/// does, and the test fails naming each case that failed by its place in `cases` (the case
/// number its label carries). A case
/// builds its pets on its worker (a pet holds `Cell`s), from inputs drawn in turn on the test
/// thread, so the seeded sequence is the serial sweep's.
fn sweep_each<T: Sync>(cases: &[T], case: impl Fn(&T, &mut Sweep) + Sync) -> Sweep {
    let runs = crate::test_pool::par_map(cases, |one| {
        aterm_spec::check::attempt(|| {
            let mut sweep = Sweep::default();
            case(one, &mut sweep);
            sweep
        })
    });
    let failed: Vec<String> = runs
        .iter()
        .enumerate()
        .filter_map(|(at, run)| run.as_ref().err().map(|why| format!("case {at}: {why}")))
        .collect();
    assert!(
        failed.is_empty(),
        "{} of {} cases failed:\n{}",
        failed.len(),
        cases.len(),
        failed.join("\n")
    );
    runs.into_iter()
        .flatten()
        .fold(Sweep::default(), |mut all, one| {
            // Named whole, so a count added to `Sweep` cannot be left out of the sums.
            let Sweep {
                revealed,
                covered,
                rehomed,
                carried,
                at_home,
                front_seat,
            } = one;
            all.revealed += revealed;
            all.covered += covered;
            all.rehomed += rehomed;
            all.carried += carried;
            all.at_home += at_home;
            all.front_seat += front_seat;
            all
        })
}

/// Over a seeded sweep of prompts with words around them, in nine cell sizes, half with edits
/// mid-doze, a dozing pet ticked at its offers draws, at every moment of its breath window,
/// what a host that ticks every [`DENSE`] between them draws ([`dense_case`]): no edge the
/// offers miss changes the body or a mote, the console's verdict on a hidden z among them.
/// Each host reads the console as the shipped redraw does ([`Dozing::tick`]), keeping its
/// reading between writes; [`a_kept_console_reading_draws_what_a_fresh_one_does`] holds that
/// reading to a fresh one, for a pet already ticked when it first observes. These pets first
/// observe before their first tick, and there a kept reading leaves `row_changed` all `None`
/// where a fresh read stamps it, so [`PetBrain::rows_settled`] stays false until the next read
/// not kept. That is shipped behaviour: these hosts match the shipped keep path, and that test
/// does not see this difference.
#[test]
fn a_dozing_pet_draws_what_a_dense_host_draws() {
    let start = Instant::now();
    let mut rng = Lcg(0x5eed);
    let cases: Vec<_> = (0..140)
        .map(|case| {
            let cell = DOZE_CELLS[case % DOZE_CELLS.len()];
            let (screen, edits) = doze_case(&mut rng, case % 2 == 1);
            (case, cell, screen, edits)
        })
        .collect();
    let sweep = sweep_each(&cases, |(case, cell, screen, edits), sweep| {
        let make = || Dozing::in_console(screen.as_bytes(), start, false, *cell, SWEEP_GRID);
        let label = format!("doze case {case}, {cell:?} cells: {screen:?}, edits {edits:?}");
        dense_case(&label, &make, edits, Duration::from_secs(12), sweep);
    });
    assert!(
        sweep.revealed > 50 && sweep.covered > 100,
        "the sweep reveals and covers z's: {} and {}",
        sweep.revealed,
        sweep.covered
    );
    assert!(sweep.rehomed > 0, "edits move the pet's home");
    assert_eq!(sweep.at_home, 280, "every dozing pet rests home");
}

/// Over a seeded sweep of prompts, cell sizes and bursts of typing ([`typing_case`]:
/// letters, backspaces and jumps of the caret, paused between), a pet that walks beside the
/// keys, rides a rhythm run's lead ([`PetBrain::carrying_run`]) and is left to settle, sit,
/// loaf and fall asleep draws, ticked at its offers, what a host that ticks every [`DENSE`]
/// between them draws ([`dense_case`]), and rests on the pixel its home draws at, or on its
/// front seat. Each host reads the console as the shipped redraw does ([`Dozing::tick`]), each
/// edit refilling what it damaged.
///
/// The claim is over these seeds. Two hosts whose ticks differ round the body's column apart
/// by a last f32 bit now and then, and a choice made on a threshold can fall either side of
/// so small a difference: of 2,700 cases a wider sweep ran, one (seed 0x9999, case 92) came
/// down on a brake's [`FLIP_SPEED`] 4e-4 cells a second apart, one host's run arriving a tick
/// before the other's, its gait pace 0.09 apart at the next key.
#[test]
fn a_typing_pet_draws_what_a_dense_host_draws() {
    let start = Instant::now();
    let mut rng = Lcg(0xbeef);
    let cases: Vec<_> = (0..48)
        .map(|case| {
            let cell = DOZE_CELLS[case % DOZE_CELLS.len()];
            let (screen, edits, last) = typing_case(&mut rng);
            (case, cell, screen, edits, last)
        })
        .collect();
    let sweep = sweep_each(&cases, |(case, cell, screen, edits, last), sweep| {
        let make = || {
            Dozing::awake(
                screen.as_bytes(),
                start,
                *cell,
                SWEEP_GRID,
                Duration::from_secs(2),
            )
        };
        let label = format!("typing case {case}, {cell:?} cells: {screen:?}, edits {edits:?}");
        let horizon = *last + Duration::from_secs_f32(SLEEP_AFTER + 2.0);
        dense_case(&label, &make, edits, horizon, sweep);
    });
    assert!(
        sweep.carried > 30 && sweep.rehomed > 40,
        "runs carry the pet ahead, and edits move its home: {} and {}",
        sweep.carried,
        sweep.rehomed
    );
    assert!(
        sweep.front_seat > 25 && sweep.at_home > 40,
        "pets rest on the front seat, and home: {} and {}",
        sweep.front_seat,
        sweep.at_home
    );
}

/// Cases a wider sweep of [`typing_case`] seeds found the two hosts parting on, each from a
/// cause since fixed, run as [`a_typing_pet_draws_what_a_dense_host_draws`] runs its own: a
/// stand met by a jump after a sleep its clock had not yet aged ([`PetBrain::at_rest`]), a
/// run setting off from a rest whose stride a host that slept had left unplanted
/// ([`PetBrain::keep_rest`]), a drop's sit running out with its caret moved on
/// ([`PetBrain::owe_set_off`]), a glide home after a backspace that a front seat held
/// within a hair of it stopped on the seat, on the schedule whose step fell there (the seat
/// holds only a body its step out landed on), a cat at rest waiting for the writing to reach
/// it whose wait ran out at an edge no offer named ([`PetBrain::writing_wait`]), and a
/// drop's sit that ran out on a tick that returned on the seat, the stand after it left to
/// a tick nothing asked for (the sit is spent before the arms are read), a purring loaf
/// whose rest veil judged a box that eased with its fold, crossing a quarter of a letter's
/// row at a step no offer named ([`PetBrain::rest_stance`]), and a seated cat stepped out to
/// a front seat a hair short of home, which the seat does not hold, so the landing took it
/// home on the next tick, at a moment no offer named (it steps out only to a seat at or past
/// home).
#[test]
fn the_partings_a_wider_sweep_found_draw_alike() {
    let start = Instant::now();
    // A cell size, a screen and its edits, each at a millisecond after the screen.
    type Parting = ((u16, u16), &'static str, &'static [(u64, &'static str)]);
    let typed: [Parting; 8] = [
        (
            (7, 15),
            "\x1b]11;#000000\x07\x1b[6;24Hx\x1b[6;25Hxxxxx\x1b[2;15Hx\x1b[6;15Hxxx\x1b[4;28Hxxx\
             \x1b[6;25Hxxxxx\x1b[6;16H$ ",
            &[
                (382, "a"),
                (457, "a"),
                (497, "\x08"),
                (573, "\x1b[4;19H"),
                (772, "\x08"),
                (870, "\x08"),
                (1018, "a"),
                (1122, "a"),
                (1287, "a"),
                (1393, "a"),
                (3020, "a"),
                (3695, "\r\n$ "),
            ],
        ),
        (
            (7, 15),
            "\x1b]11;#000000\x07\x1b[2;50Hxxxxxx\x1b[3;50Hxxxxxx\x1b[6;52Hx\x1b[3;48Hx\x1b[7;41H$ ",
            &[
                (368, "\x08"),
                (626, "a"),
                (781, "a"),
                (937, "\x08"),
                (992, "a"),
                (1051, "a"),
                (1194, "a"),
                (1290, "a"),
                (1548, "a"),
                (1760, "a"),
            ],
        ),
        (
            (7, 15),
            "\x1b]11;#000000\x07\x1b[14;65Hxxx\x1b[14;61Hxxxx\x1b[14;60H$ ",
            &[
                (356, "\x1b[15;48H"),
                (1197, "a"),
                (1359, "a"),
                (1409, "\x1b[14;49H"),
                (1485, "a"),
                (1666, "a"),
            ],
        ),
        (
            (8, 16),
            "\x1b]11;#000000\x07\x1b[12;19Hxxxx\x1b[14;5H$ ",
            &[(4234, "a"), (5240, "\x08")],
        ),
        (
            (15, 28),
            "\x1b]11;#000000\x07\x1b[11;15Hx\x1b[12;20Hx\x1b[16;13Hxxx\x1b[11;13Hxx\x1b[16;7Hxxxx\
             \x1b[16;7H$ ",
            &[
                (381, "a"),
                (501, "a"),
                (677, "a"),
                (793, "\x1b[13;22H"),
                (845, "a"),
                (1045, "a"),
                (1189, "\x1b[18;1H"),
                (1378, "a"),
                (1436, "a"),
                (1620, "a"),
                (1769, "a"),
                (1956, "a"),
                (2013, "\x1b[16;6H"),
            ],
        ),
        (
            (8, 16),
            "\x1b]11;#000000\x07\x1b[6;29Hxxx\x1b[3;41Hxxxxx\x1b[6;38Hx\x1b[6;33Hxxxxx\x1b[8;33H$ ",
            &[(2021, "\x1b[9;34H"), (2916, "a")],
        ),
        (
            (7, 15),
            "\x1b]11;#000000\x07\x1b[8;23Hx\x1b[11;22Hxxxx\x1b[9;10Hx\x1b[11;10H$ ",
            &[
                (325, "\x1b[14;18H"),
                (455, "a"),
                (612, "\x1b[9;4H"),
                (1184, "\r\n$ "),
            ],
        ),
        (
            (8, 16),
            "\x1b]11;#000000\x07\x1b[12;42Hxxxx\x1b[15;40H$ ",
            &[(162, "a"), (228, "a"), (477, "a"), (551, "\x08")],
        ),
    ];
    let mut sweep = Sweep::default();
    let at = |edits: &[(u64, &str)]| {
        edits
            .iter()
            .map(|&(ms, bytes)| (Duration::from_millis(ms), bytes.as_bytes().to_vec()))
            .collect::<Vec<_>>()
    };
    for (i, (cell, screen, edits)) in typed.into_iter().enumerate() {
        let edits = at(edits);
        let make = || {
            Dozing::awake(
                screen.as_bytes(),
                start,
                cell,
                SWEEP_GRID,
                Duration::from_secs(2),
            )
        };
        let label = format!("parting {i}, {cell:?} cells: {screen:?}, edits {edits:?}");
        let last = edits.last().map_or(Duration::ZERO, |e| e.0);
        dense_case(
            &label,
            &make,
            &edits,
            last + Duration::from_secs(4),
            &mut sweep,
        );
    }
}

/// Over a seeded sweep of sleepers a key or a few wake ([`wake_case`]), in nine cell sizes, a
/// pet ticked at its offers draws what a host that ticks every [`DENSE`] between them draws
/// ([`dense_case`]) through the wake's stretch, the seat it ends in and what follows, and
/// rests on the pixel its home draws at, or on its front seat.
#[test]
fn a_woken_pet_draws_what_a_dense_host_draws() {
    let start = Instant::now();
    let mut rng = Lcg(0x3a4e);
    let mut sweep = Sweep::default();
    for case in 0..24 {
        let cell = DOZE_CELLS[case % DOZE_CELLS.len()];
        let (screen, keys, last) = wake_case(&mut rng);
        let make = || Dozing::in_console(screen.as_bytes(), start, false, cell, SWEEP_GRID);
        let label = format!("wake case {case}, {cell:?} cells: {screen:?}, keys {keys:?}");
        dense_case(
            &label,
            &make,
            &keys,
            last + Duration::from_secs(6),
            &mut sweep,
        );
    }
    assert!(
        sweep.rehomed > 16,
        "keys move the pet's home: {}",
        sweep.rehomed
    );
}

/// The rows [`DOZE_INK`] inks, as the host's ink probe reads them ([`PetBrain::sense_ink`]):
/// each row's first inked column and the column after its last.
const DOZE_INK_SPANS: [(u16, u16); 7] = [(0, 9), (0, 2), (0, 9), (0, 2), (0, 9), (0, 2), (0, 1)];

/// A key that wakes a sleeper below scrollback costs its 60 Hz host no frame train while the
/// wake's stretch holds still. The stretch's reach eases out over [`STRETCH_LEAD`] on the
/// train, and its pose then holds to the hind leg's swap and the hand-off
/// ([`PetBrain::waking_moves`]); the z the wake pops rises into the ink, which hides it and
/// then drops it, and a hidden pop waits at the offer for the moment it could show again or
/// drop ([`PetMoteKind::offers_hidden`]). So the host sleeps from one offered edge to the
/// next, and a host that also ticks every [`DENSE`] draws the same ([`dense_case`]).
#[test]
fn a_wake_below_scrollback_trains_no_frames_while_its_stretch_holds() {
    const FRAME: Duration = Duration::from_nanos(16_666_667);
    let start = Instant::now();
    let key = Duration::from_secs(1);
    let keys = [(key, b"a".to_vec())];
    let make = || {
        let mut doze = Dozing::new(DOZE_INK, start, false);
        doze.pet.sense_ink(0, &DOZE_INK_SPANS, None);
        doze
    };
    dense_case(
        "a key below scrollback",
        &make,
        &keys,
        key + Duration::from_secs(6),
        &mut Sweep::default(),
    );
    let mut doze = make();
    let _ = doze.run(&keys, Schedule::Offered(FRAME), Some(key + 3 * FRAME));
    assert!(
        (0..PET_MOTES_MAX).any(|i| doze.pet.console_hid_mote(i)
            && matches!(doze.pet.motes[i], Some(m) if m.kind == PetMoteKind::ZeePop)),
        "fixture: the ink hides the pop the wake startles"
    );
    let mut doze = make();
    let t0 = doze.now;
    let run = doze.run(
        &keys,
        Schedule::Offered(FRAME),
        Some(key + Duration::from_secs(2)),
    );
    let woke = t0 + key;
    let held = (
        woke + Duration::from_secs_f32(STRETCH_LEAD) + 2 * FRAME,
        woke + Duration::from_secs_f32(WAKE_DUR - WAKE_HANDOFF),
    );
    let train: Vec<_> = run
        .iter()
        .filter(|d| d.at > held.0 && d.at < held.1 && !d.offered)
        .map(|d| d.at - woke)
        .collect();
    assert!(
        train.is_empty(),
        "the held stretch rides the frame train at {train:?} after the key"
    );
}

/// Over a seeded sweep of [`typing_case`]s, a pet left after the keys costs its host no
/// frames once it has settled: ticked at its offers by a 60 Hz host, its frame trains end
/// within 20 s of the last key, it sleeps, and its breath window closes on a tick that asks
/// for none ([`PetBrain::needs_frames`]). The last case is one a 600-case sweep found: the
/// pet sleeps with its top a hair from a quarter of a letter's row, where each breath carried
/// its drawn box across that line and back, and a veil judged by the drawn box eased down
/// and up again with each breath ([`PetBrain::rest_stance`]).
#[test]
fn a_pet_left_after_typing_asks_no_frames_once_settled() {
    const FRAME: Duration = Duration::from_nanos(16_666_667);
    let start = Instant::now();
    let mut rng = Lcg(0x1d1e);
    let mut cases: Vec<_> = (0..100)
        .map(|case| {
            let (screen, edits, last) = typing_case(&mut rng);
            (DOZE_CELLS[case % DOZE_CELLS.len()], screen, edits, last)
        })
        .collect();
    cases.push((
        (7, 15),
        "\x1b]11;#000000\x07\x1b[8;23Hx\x1b[11;22Hxxxx\x1b[9;10Hx\x1b[11;10H$ ".to_owned(),
        vec![
            (Duration::from_millis(325), b"\x1b[14;18H".to_vec()),
            (Duration::from_millis(455), b"a".to_vec()),
            (Duration::from_millis(612), b"\x1b[9;4H".to_vec()),
            (Duration::from_millis(1184), b"\r\n$ ".to_vec()),
        ],
        Duration::from_millis(1184),
    ));
    for (case, (cell, screen, edits, last)) in cases.into_iter().enumerate() {
        let mut doze = Dozing::awake(
            screen.as_bytes(),
            start,
            cell,
            SWEEP_GRID,
            Duration::from_secs(2),
        );
        let t0 = doze.now;
        let run = doze.run(&edits, Schedule::Offered(FRAME), None);
        let late = run
            .iter()
            .find(|d| !d.offered && !d.edit && d.at >= t0 + last + Duration::from_secs(20));
        assert!(
            late.is_none(),
            "typing case {case}, {cell:?} cells: {screen:?}, edits {edits:?}: a frame train \
             still runs {:?} after the last key",
            late.map(|d| d.at - (t0 + last)),
        );
        assert!(
            !doze.pet.needs_frames() && doze.pet.quiet >= SLEEP_AFTER + BREATH_WINDOW,
            "typing case {case}: the breath window closes asking for no frames"
        );
    }
}

#[test]
fn a_tick_on_a_z_beat_puffs_its_z() {
    let start = Instant::now();
    let mut doze = Dozing::new(b"\x1b]11;#000000\x07\x1b[20;10H$ ", start, false);
    while doze.pet.mote_mark < 3 {
        doze.now = if doze.pet.needs_frames() {
            doze.now + Duration::from_millis(16)
        } else {
            doze.pet
                .next_change_deadline(doze.now)
                .expect("light sleep offers its next z")
        };
        let _ = doze.tick();
    }
    let on_beat = beat_at(SLEEP_AFTER, ZEE_EVERY, 4);
    assert!(
        doze.pet.quiet < on_beat && ((on_beat - SLEEP_AFTER) * (1.0 / ZEE_EVERY)).floor() < 4.0,
        "fixture: beat 4 is to come, and dividing by the beat counts it short ({on_beat})"
    );
    doze.pet.quiet = on_beat;
    let _ = doze.tick();
    assert_eq!(doze.pet.mote_mark, 4, "a tick on beat 4 puffs its z");
}

/// THE CAT HIDES ONLY WHERE IT MAY WALK ([`PetBrain::hide_spot`]), AND A WALK THE CONSOLE
/// TURNS BACK NEVER WALKS ON ([`PetBrain::walk_turned_back`]). A cat resting past the words
/// of a footer row: of ordinary ink, the hide goes behind their end; of words the host
/// reserves (aterm's rewrite of Claude Code's mode row holds the whole extent it wrote), no
/// hide is dealt there. A hide walked onto them anyway (the chrome came after the deal) is
/// given up at the first step the frame pass puts back past them, the cat stopped where it
/// was put, and so is a chase of the pointer, cooled; a walk to the cat's own station is put
/// back and owes the hop over the chrome to it ([`PetBrain::chrome_hop`]). NEGATIVE CONTROLS
/// (as run before): the spot behind the host's words was dealt; the step put back left the
/// hide owed and the cat walking at 8 cells a second, to be put back again every frame.
#[test]
fn a_hide_behind_the_hosts_words_is_never_dealt_and_a_turned_back_walk_never_walks_on() {
    const WORDS: &[u8] = b"  YOLO on \xc2\xb7 1 shell \xc2\xb7 and some more footer words";
    let width = art_cols(10, 20);
    let mut term = Terminal::new(20, 80);
    term.process(b"\x1b]11;#000000\x07\x1b[15;1H");
    term.process(WORDS);
    term.process(b"\x1b[8;3H");
    let words = core::str::from_utf8(WORDS)
        .expect("fixture text")
        .chars()
        .count();
    let end = words as f32;
    let reserved = [PetRect::new(14.0, 0.0, 1.0, end)];
    let mut spans = [(0u16, 0u16); 20];
    spans[14] = (2, words as u16);
    let start = Instant::now();
    let observe = |pet: &mut PetBrain, term: &mut Terminal, chrome: bool| {
        let input = term.cell_frame(20, 80);
        let facts = PetWorldFacts::read(term, 17);
        let exclusions: &[PetRect] = if chrome { &reserved } else { &[] };
        pet.observe_console_with_exclusions(&input, &facts, PetPane::full(&input), exclusions);
        pet.set_console_presentable(true);
        pet.sense_ink(0, &spans, None);
    };
    let sense = |now: Instant| PetSense {
        caret_drawn: true,
        now,
        caret: Some((7, 2)),
        wrapped: false,
        rows: 20,
        cols: 80,
        cell_w: 10,
        cell_h: 20,
        reduced_motion: false,
        output_burst: false,
        pointer: None,
    };
    let mut resting = |chrome: bool| {
        let mut pet = PetBrain {
            col: end + 1.0,
            row: 14.0,
            alpha: 1.0,
            action: PetAction::Sit,
            quiet: 2.0,
            last_caret: Some((7, 2)),
            last_now: Some(start),
            ..PetBrain::default()
        };
        observe(&mut pet, &mut term, chrome);
        let _ = pet.tick(sense(start));
        pet
    };
    let ink = resting(false);
    let spot = ink.hide_spot(width, (10, 20), (80, 20));
    assert!(
        matches!(spot, Some(dest) if dest < end && dest + width > end),
        "control: behind ordinary words the hide goes behind their end ({spot:?}, the words end at {end})"
    );
    let mut pet = resting(true);
    assert_eq!(
        pet.hide_spot(width, (10, 20), (80, 20)),
        None,
        "no hide behind the host's words"
    );
    // The chrome came after the deal: the hide walks onto the host's words, and the frame
    // pass puts the step back on the perch past them.
    let f = pet.tick(sense(start + Duration::from_millis(16)));
    let walk = |pet: &mut PetBrain, hide: Option<f32>| {
        pet.set_action(PetAction::Walk);
        pet.speed = -8.0;
        pet.hide_to = hide;
        pet.col = end - 0.4;
        pet.row = 14.0;
        pet.console.clipped = false;
        pet.deferred_arc = None;
        pet.touched_on_chrome = false;
        pet.chrome_hop = false;
        // A whole copy of `f`, not `..f` on the capture: a closure that captures `f`'s array
        // fields one by one (their lengths are named consts) is broken MIR to stock rustc
        // 1.95, an ICE that failed `verify cells`' win-arm cell.
        let base = f;
        let mut frame = PetFrame {
            col: pet.col,
            row: pet.row,
            action: PetAction::Walk,
            lift: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
            alpha: 255,
            ..base
        };
        pet.place_console_body_at_home(&mut frame, sense(start + Duration::from_millis(32)));
        frame
    };
    let station = walk(&mut pet, None);
    assert!(
        pet.console_reason() == "cursor-home" && station.col >= end,
        "fixture: the frame pass puts the station walk's step back past the host's words \
         ({} at {:.2})",
        pet.console_reason(),
        station.col
    );
    assert!(
        pet.chrome_hop && pet.hide_to.is_none() && pet.pursuit_t.is_none(),
        "a walk to the cat's own station the console turned back owes the hop over the chrome \
         ({}, {:.1} cells/s)",
        pet.chrome_hop,
        pet.speed
    );
    pet.pursuit_t = Some(0.5);
    pet.pursuit_cool = 0.0;
    let chased = walk(&mut pet, None);
    assert!(
        chased.col >= end
            && pet.pursuit_t.is_none()
            && pet.pursuit_cool >= PURSUIT_COOL
            && pet.speed == 0.0
            && !pet.chrome_hop,
        "a chase the console turned back is given up and cooled, the cat stopped where it was \
         put ({:?}, cool {:.1}, {:.1} cells/s, hop {})",
        pet.pursuit_t,
        pet.pursuit_cool,
        pet.speed,
        pet.chrome_hop
    );
    let put = walk(&mut pet, spot);
    assert!(
        pet.console_reason() == "cursor-home" && put.col >= end,
        "fixture: the frame pass puts the hide's step back past the host's words ({} at {:.2})",
        pet.console_reason(),
        put.col
    );
    assert!(
        pet.hide_to.is_none() && pet.speed == 0.0 && !pet.chrome_hop,
        "the hide it turned back is given up, the cat stopped where it was put \
         ({:?}, {:.1} cells/s, hop {})",
        pet.hide_to,
        pet.speed,
        pet.chrome_hop
    );
}

/// A WAY ALONG THE BODY'S ROW IS READ ON THE ROWS OVER AND UNDER IT ALIKE
/// ([`PetBrain::head_span`], [`PetBrain::read_about`], ATERM-KI-1107-3): on a pane 100 rows
/// by 300 columns, the caret at row 39's third column (the caret's window rows 7..71 and
/// columns 0..256), and a body at rest on row 70 at column 250, last drawn there (the window
/// about it rows 66..74 and columns 44..300), about to set off back along its row to column
/// 5, between rows 69 and 71 lettered to the margin. Its way, from 5 less a stretch's reach
/// to past column 256, is read whole by neither window. Over row 69 the caret's window reads
/// the body's columns and the most of the way, and reads it to that window's last column;
/// under, row 71 lies past that window's rows, and the way read so far reads nothing there.
/// The head law reads the way as far as both rows are read, from the first column of the
/// window about the body to the caret's window's last (44..256), each lettered: the body is
/// fitted to its row.
#[test]
fn a_way_along_the_row_is_read_on_the_rows_over_and_under_the_body_alike() {
    let row: String = "the quick brown fox jumps over the lazy dog "
        .repeat(7)
        .chars()
        .take(300)
        .collect();
    let mut term = Terminal::new(100, 300);
    term.process(format!("\x1b[70;1H{row}\x1b[72;1H{row}\x1b[40;3H").as_bytes());
    let input = term.cell_frame(100, 300);
    let facts = PetWorldFacts::read(&term, 17);
    let width = 4.0;
    let mut pet = PetBrain {
        col: 250.0,
        row: 70.0,
        alpha: 1.0,
        ..PetBrain::default()
    };
    pet.console.last_body = Some(PetRect::new(69.25, 250.0, 1.75, width));
    pet.observe_console(&input, &facts, PetPane::full(&input));
    pet.set_console_presentable(true);
    let window = |row, rows, col| PetPane {
        row,
        col,
        rows,
        cols: crate::pet_world::MAX_WORLD_COLS,
    };
    let world = pet.console.world.as_ref().expect("the pane is read");
    assert_eq!(
        (world.coverage(), world.body_window()),
        (window(7, 64, 0), window(66, 8, 44))
    );
    pet.setting_off = Some(5.0);
    let pad = HEAD_STRETCH_PAD * width;
    let way = (5.0 - pad, 254.0 + pad + HEAD_REST_REACH);
    assert!(
        way.1 > 256.0 && way.1 < 300.0,
        "fixture: the way runs past the caret's window's last column ({way:?})"
    );
    let over = pet.read_about(69.0, way, (250.0, 254.0));
    assert_eq!(over, (way.0, 256.0));
    assert_eq!(
        pet.observed_ink_in_span(71.0, over.0, over.1),
        None,
        "fixture: the way as read over the body reads nothing under it"
    );
    let (seat, span) = pet.head_span(300, width);
    assert_eq!((seat, span), ((250.0, 70.0), (44.0, 256.0)));
    let read = pet
        .head_target(70.0, (100, 300), span)
        .expect("the rows over and under the body are read");
    assert_eq!(read.law, head_law(true, true));
}
