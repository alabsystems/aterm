// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **THE PET'S VIEWPORT WALK** — the one per-frame cost in the effects crate
//! that no bench measured.
//!
//! `PetWorld::observe` reclassifies every cell of the pane the resident pet
//! lives in, on every PRESENTED frame, so the pet knows which cells are ink,
//! which are protected and which are clear. It is default-on (the shipped
//! `cursor_trail_style` is the rainbow kitty pet), and it is O(WINDOW), not
//! O(ink): the `blank` arms below are the control that says so — a screen with
//! nothing on it costs very nearly what a full one does, because the walk
//! visits every cell either way.
//!
//! Arms are the owner's reporting shape (57 x 151) and a small terminal
//! (24 x 80), each full of text and each blank. The timed unit is `observe`
//! alone: the frame and the facts are built once, outside the loop, and the
//! fixture ASSERTS that `observe` admitted the frame — an early-out returns
//! `false` in microseconds and would otherwise be timed as a win.

use aterm_core::render::RenderInput;
use aterm_core::terminal::Terminal;
use aterm_effects::pet_world::{PetPane, PetWorld, PetWorldFacts};
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

/// One arm: a terminal, the frame it produced, and the facts read beside it.
struct Fixture {
    world: PetWorld,
    input: RenderInput,
    facts: PetWorldFacts,
    pane: PetPane,
}

impl Fixture {
    fn new(rows: usize, cols: usize, fill: bool) -> Self {
        let mut term = Terminal::new(rows as u16, cols as u16);
        if fill {
            // Ordinary prose, one wrapped paragraph per row, so the walk sees
            // real glyphs rather than a uniform run it could shortcut.
            let line: Vec<u8> = std::iter::repeat_n(b"the quick brown fox jumps over ", cols)
                .flatten()
                .copied()
                .take(cols.saturating_sub(1))
                .chain(std::iter::once(b'\n'))
                .collect();
            for _ in 0..rows.saturating_sub(1) {
                term.process(&line);
            }
        }
        let input = term.cell_frame(rows, cols);
        let facts = PetWorldFacts::read(&term, 1);
        let pane = PetPane::full(&input);
        let mut world = PetWorld::default();
        assert!(
            world.observe(&input, &facts, pane),
            "the fixture must be ADMITTED — a refused frame early-outs and would time as a win"
        );
        Fixture {
            world,
            input,
            facts,
            pane,
        }
    }

    /// THE TIMED UNIT. Nothing here but the walk.
    fn observe(&mut self) -> bool {
        self.world
            .observe(black_box(&self.input), black_box(&self.facts), self.pane)
    }
}

fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("pet_world_observe");
    for (name, rows, cols, fill) in [
        ("owner_57x151_text", 57, 151, true),
        ("owner_57x151_blank", 57, 151, false),
        ("small_24x80_text", 24, 80, true),
        ("small_24x80_blank", 24, 80, false),
    ] {
        let mut fx = Fixture::new(rows, cols, fill);
        group.bench_function(name, |b| b.iter(|| black_box(fx.observe())));
    }
    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
