// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The escort is the pet's default life.
//!
//! The console layer may direct the pet's body only while genuinely resident
//! (reading a selection, watching a command block, visiting a content anchor,
//! holding a hidden cursor's home); otherwise it keeps only presence and layering
//! (`console_pet_visibility_hold`). The oracle is a twin brain with
//! `set_console_presentable(false)`, which returns every console entry point
//! early: the plain escort on the same terminal. Ordinary typing must not tell the
//! two apart. Absolute numbers derive from `PetBrain::station` (`STATION_LEAD = 1.0`).

use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;
use aterm_effects::kitty_pet::{PetBrain, PetFrame, PetInputKind, PetSense, art_cols};
use aterm_effects::pet_world::{PetPane, PetRect, PetWorld, PetWorldFacts};

const ROWS: u16 = 24;
const COLS: u16 = 80;
const CELL_W: u16 = 8;
const CELL_H: u16 = 16;
const CARET_COL: u16 = 39;

struct Scene {
    term: Terminal,
    brain: PetBrain,
    presentable: bool,
}

impl Scene {
    /// `presentable == false` is the plain escort, with no console layer.
    fn new(presentable: bool, home: &[u8]) -> Self {
        let mut term = Terminal::new(ROWS, COLS);
        term.process(b"\x1b]11;#000000\x07");
        term.process(home);
        Self {
            term,
            brain: PetBrain::default(),
            presentable,
        }
    }

    fn tick(&mut self, now: Instant) -> PetFrame {
        let input = self.term.cell_frame(usize::from(ROWS), usize::from(COLS));
        let facts = PetWorldFacts::read(&self.term, 7);
        self.brain
            .observe_console(&input, &facts, PetPane::full(&input));
        self.brain.set_console_presentable(self.presentable);
        self.brain.tick(PetSense {
            caret_drawn: true,
            now,
            caret: (input.cursor_visible && input.display_offset == 0)
                .then_some((input.cursor_row as u16, input.cursor_col as u16)),
            wrapped: false,
            rows: ROWS,
            cols: COLS,
            cell_w: CELL_W,
            cell_h: CELL_H,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
        })
    }

    fn world(&mut self) -> PetWorld {
        let input = self.term.cell_frame(usize::from(ROWS), usize::from(COLS));
        let facts = PetWorldFacts::read(&self.term, 7);
        let mut world = PetWorld::default();
        assert!(world.observe(&input, &facts, PetPane::full(&input)));
        world
    }
}

fn body(frame: PetFrame) -> Option<PetRect> {
    frame
        .body_px(CELL_W, CELL_H, COLS, ROWS)
        .map(|(x0, x1, y0, y1)| {
            PetRect::new(
                y0 as f32 / f32::from(CELL_H),
                x0 as f32 / f32::from(CELL_W),
                (y1 - y0) as f32 / f32::from(CELL_H),
                (x1 - x0) as f32 / f32::from(CELL_W),
            )
        })
}

/// The escort's station, computed from the escort law.
fn escort_station(caret_col: u16) -> f32 {
    PetBrain::station(caret_col, COLS, art_cols(CELL_W, CELL_H))
}

/// A caret parked mid-pane with nothing printed anywhere.
fn parked() -> Scene {
    Scene::new(true, b"\x1b[12;40H")
}

fn settle(scene: &mut Scene, now: &mut Instant) -> PetFrame {
    *now += Duration::from_millis(16);
    let mut frame = scene.tick(*now);
    for _ in 0..400 {
        *now += Duration::from_millis(16);
        frame = scene.tick(*now);
        if !scene.brain.needs_frames() {
            break;
        }
    }
    frame
}

#[test]
fn the_caret_is_not_an_obstruction_to_its_own_escort() {
    let mut now = Instant::now();
    let mut scene = parked();
    let settled = settle(&mut scene, &mut now);
    let want = escort_station(CARET_COL);
    // A cat seated one cell past the caret stands inside the caret's 3x3 keep-off
    // ring, which keeps the pet off the cursor but never evicts the escort.
    let seated = body(settled).expect("a settled pet has a body");
    assert_eq!(
        scene.world().under_text_clearance(seated, 0.10),
        Some(false),
        "precondition: this scene is only meaningful while the escort's \
         body is inside the caret ring (body {seated:?})",
    );
    for i in 0..240 {
        now += Duration::from_millis(16);
        let frame = scene.tick(now);
        assert!(
            (frame.col - want).abs() <= 0.5,
            "frame {i}: the escort was pushed to {:.2}, off the ladder's {want:.2}",
            frame.col,
        );
    }
}

#[test]
fn plain_typing_is_the_pet_with_no_console_layer() {
    let mut now = Instant::now();
    let mut live = Scene::new(true, b"\x1b[12;1H");
    let mut bare = Scene::new(false, b"\x1b[12;1H");
    for _ in 0..300 {
        now += Duration::from_millis(16);
        live.tick(now);
        bare.tick(now);
    }
    let mut worst = 0.0f32;
    let mut worst_at = 0;
    let mut contact = 0usize;
    let mut frames = 0usize;
    for k in 0..24u32 {
        let ch = [b'a' + (k % 26) as u8];
        live.term.process(&ch);
        bare.term.process(&ch);
        live.brain.note_console_input(now, PetInputKind::Text);
        for _ in 0..13 {
            now += Duration::from_millis(16);
            let a = live.tick(now);
            let b = bare.tick(now);
            // On an ordinary typing frame the layer may say `quiet` or
            // `committed-input` (the escort untouched) or `advancing-ink` (the
            // contact brace when new ink runs into the body); `cursor-home`,
            // `protected-*` and the perch reasons claim the body and must not.
            let reason = live.brain.console_reason();
            assert!(
                matches!(reason, "quiet" | "committed-input" | "advancing-ink"),
                "key {k}: the console layer claimed the escort's body ({reason})",
            );
            contact += usize::from(reason == "advancing-ink");
            frames += 1;
            let dx = (a.col - b.col).abs();
            if dx > worst {
                worst = dx;
                worst_at = k;
            }
        }
    }
    // Both brains run the same deterministic chase, so any real standoff shows
    // far above a quarter cell.
    assert!(
        worst <= 0.25,
        "the console layer displaced the escort by {worst:.2} cells (worst at key {worst_at})",
    );
    // `CONTACT_HOLD` (0.40 s) re-arms only once the body is roomy again, so a
    // typing run buys about one brace.
    assert!(
        contact * 5 <= frames,
        "the contact brace held {contact} of {frames} typing frames",
    );
}

#[test]
fn a_settled_cat_typed_at_keeps_its_own_animation() {
    let mut now = Instant::now();
    let mut live = Scene::new(true, b"\x1b[12;1H");
    for _ in 0..300 {
        now += Duration::from_millis(16);
        live.tick(now);
    }
    // A burst to wake the cat, then a pause long enough to sit but not sleep.
    for k in 0..24u32 {
        let ch = [b'a' + (k % 26) as u8];
        live.term.process(&ch);
        live.brain.note_console_input(now, PetInputKind::Text);
        for _ in 0..13 {
            now += Duration::from_millis(16);
            live.tick(now);
        }
    }
    for _ in 0..120 {
        now += Duration::from_millis(16);
        live.tick(now);
    }
    assert!(
        live.brain.action().settled(),
        "precondition: the cat must have settled, got {:?}",
        live.brain.action(),
    );
    // One key that changes nothing on screen (a password prompt); 36 frames stay
    // inside `INPUT_HOLD` (0.65 s).
    live.brain.note_console_input(now, PetInputKind::Text);
    let mut poses = std::collections::BTreeSet::new();
    for i in 0..36 {
        now += Duration::from_millis(16);
        poses.insert(format!("{:?}", live.tick(now).pose));
        // Precondition: the input hold owns the frame (`committed-input` imposes
        // `PetStandEar`) and the cat stays settled.
        assert_eq!(
            live.brain.console_reason(),
            "committed-input",
            "frame {i}: precondition — the input hold must own this frame",
        );
        assert!(
            live.brain.action().settled(),
            "frame {i}: precondition — the cat must stay settled, got {:?}",
            live.brain.action(),
        );
    }
    // The silhouette is the action machine's: a cat whose action says Stand wears
    // `PetStand`, not the console layer's `PetStandEar`.
    assert!(
        !poses.contains("PetStandEar"),
        "the console layer replaced a settled cat's silhouette with its own \
         stand-ear pin for the whole input hold: {poses:?}",
    );
}
