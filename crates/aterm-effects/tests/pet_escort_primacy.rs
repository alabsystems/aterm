// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The escort is the pet's default life: ordinary typing matches a twin brain with
//! `set_console_presentable(false)`.

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

fn escort_station(caret_col: u16) -> f32 {
    PetBrain::station(caret_col, COLS, art_cols(CELL_W, CELL_H))
}

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
    assert!(
        worst <= 0.25,
        "the console layer displaced the escort by {worst:.2} cells (worst at key {worst_at})",
    );
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
    live.brain.note_console_input(now, PetInputKind::Text);
    let mut poses = std::collections::BTreeSet::new();
    for i in 0..36 {
        now += Duration::from_millis(16);
        poses.insert(format!("{:?}", live.tick(now).pose));
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
    assert!(
        !poses.contains("PetStandEar"),
        "the console layer replaced a settled cat's silhouette with its own \
         stand-ear pin for the whole input hold: {poses:?}",
    );
}
