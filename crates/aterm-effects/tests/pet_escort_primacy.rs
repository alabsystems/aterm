// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ESCORT IS THE PET'S DEFAULT LIFE.
//!
//! The console observation layer may direct the pet's BODY and its
//! PERFORMANCE only while it is genuinely RESIDENT — reading a selection,
//! watching a command block, visiting a content anchor, holding a hidden
//! cursor's home. At every other moment the pet is the caret's escort,
//! exactly as it was in v0.76.0, and what the console layer keeps is
//! PRESENCE and LAYERING: whether the sprite may be painted where it stands
//! (a selection, an image, an incoherent surface) and whether it goes behind
//! the ink. Those are the visibility hold and they are not this file's
//! subject — `console_pet_visibility_hold` owns them.
//!
//! THE ORACLE IS THE PET ITSELF. `set_console_presentable(false)` returns
//! every console entry point early, so a twin brain driven with it off IS
//! the v0.76.0 caret escort, running the same shipped chase law against the
//! same terminal. Ordinary typing must not be able to tell the two apart.
//! Every absolute number here is derived from the shipped escort law
//! (`PetBrain::station`, `STATION_LEAD = 1.0`), never from an observation of
//! the current build.

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
    /// `presentable == false` is the v0.76.0 escort with no console layer.
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

/// The v0.76.0 station, computed from the shipped law rather than recalled.
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
fn the_escort_stands_at_the_caret_ladder_not_a_console_perch() {
    let mut now = Instant::now();
    let mut scene = parked();
    let frame = settle(&mut scene, &mut now);
    assert!(frame.alpha > 0, "the pet must be on glass to be measured");
    let want = escort_station(CARET_COL);
    // STATION_LEAD is ONE cell. Half a cell of slack absorbs the chase's
    // easing and `body_px`'s pixel rounding; a whole extra cell of standoff
    // is the thing this test refuses.
    assert!(
        (frame.col - want).abs() <= 0.5,
        "escort stood at {:.2}; the v0.76 ladder says {want:.2} (caret col {CARET_COL})",
        frame.col,
    );
}

#[test]
fn the_caret_is_not_an_obstruction_to_its_own_escort() {
    let mut now = Instant::now();
    let mut scene = parked();
    let settled = settle(&mut scene, &mut now);
    let want = escort_station(CARET_COL);
    // A cat seated one cell past the caret stands INSIDE the caret's 3x3
    // keep-off ring. That ring exists so the pet never comes to rest ON the
    // cursor and hides the cell the user is watching; it is not a licence to
    // push the escort clear of the ring, and nothing may evict it for being
    // there. (`under_text_clearance` is the strict reading, caret included.)
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
            // WHAT THE LAYER MAY SAY ON AN ORDINARY TYPING FRAME. `quiet`
            // and `committed-input` are the escort running untouched;
            // `advancing-ink` is the located contact brace — a SILHOUETTE
            // for 0.40 s when new ink runs into the body, which is one of
            // the features this restoration keeps. `cursor-home`,
            // `protected-*` and the perch reasons are the layer claiming
            // the escort's BODY, and none of them may appear here.
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
    // A quarter of a cell: the chase is deterministic and both brains run
    // the same one, so any real standoff shows up far above this.
    assert!(
        worst <= 0.25,
        "the console layer displaced the escort by {worst:.2} cells (worst at key {worst_at})",
    );
    // CONTACT_HOLD is 0.40 s and re-arms only once the body is roomy again,
    // so a whole typing run buys about one brace. A cat seated at the
    // caret's shoulder that is PERMANENTLY braced is a different pet, and
    // this is where that would show.
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
    // A short burst so the cat is AWAKE, then a pause long enough to sit but
    // not to sleep — a key on a sleeping cat is a wake, which is its own
    // animation and not this file's subject.
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
    // ONE KEY that moves nothing on screen — a password prompt, a key the
    // shell swallows. INPUT_HOLD is 0.65 s; 36 frames at 16 ms stays inside
    // it, so the hold itself is what is being watched and not its expiry.
    live.brain.note_console_input(now, PetInputKind::Text);
    let mut poses = std::collections::BTreeSet::new();
    for i in 0..36 {
        now += Duration::from_millis(16);
        poses.insert(format!("{:?}", live.tick(now).pose));
        // The precondition that makes this scene the override's own: the
        // layer HAS a pose to impose (`committed-input` is where
        // `console.pose = Some(PetStandEar)` is set), it is NOT resident,
        // and the pet's action is one of the settled ones the override used
        // to reach through — Sleep|Sit|Loaf|Purr|Groom|Perk|Stand.
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
    // THE SILHOUETTE IS THE ACTION MACHINE'S. `PetStandEar` is the console
    // layer's own authored stand-ear glyph; a cat whose action says Stand
    // wears `PetStand`. Seeing the ear here is the override reaching past
    // `resident` into a settled cat's own animation.
    assert!(
        !poses.contains("PetStandEar"),
        "the console layer replaced a settled cat's silhouette with its own \
         stand-ear pin for the whole input hold: {poses:?}",
    );
}
