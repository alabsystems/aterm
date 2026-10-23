// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The kitty-command trick flash allocates nothing on the input path or the frame path.

use aterm_core::terminal::Terminal;
use aterm_effects::word_decorations::{DecoConfig, EffectGeom, TrickFlashPhase, WordDecorations};
use aterm_lexicon::{Lexicon, TrickLexicon};
use aterm_time::{Duration, Instant};

fn allocations_during(f: impl FnOnce()) -> usize {
    crate::alloc_count::allocations_during(f).1
}

#[test]
fn a_flash_costs_the_input_path_and_the_frame_path_no_allocation() {
    let (lex, cfg) = (Lexicon::with_languages(&["en"]), DecoConfig::default());
    let geom = EffectGeom::default();
    let mut term = Terminal::new(4, 40);
    let mut wd = WordDecorations::default();
    let (mut out, mut ink, mut free, mut nova) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut now = Instant::now();

    term.process(b"$ sit");
    wd.rescan(&term, 4, 40, &lex, &cfg, 1, now);

    let window = "x".repeat(TrickLexicon::MAX_SURFACE_CHARS);
    let too_long = "x".repeat(TrickLexicon::MAX_SURFACE_CHARS + 1);
    let shapes: [&str; 6] = ["", "\u{200d}", "นั่ง", &too_long, &window, "sit"];

    let mut pending = 0usize;
    let allocations = allocations_during(|| {
        for word in shapes {
            wd.revoke_trick_flash(now);
            wd.note_trick_typed(now, None, word, 0);
            pending += usize::from(wd.trick_flash_phase(now) == TrickFlashPhase::Pending);
        }
    });
    assert_eq!(
        pending, 3,
        "the Thai word, the full window and `sit` are stored"
    );
    assert_eq!(allocations, 0, "a note must not allocate");

    term.process(b" ");
    wd.rescan(&term, 4, 40, &lex, &cfg, 2, now);
    assert_eq!(wd.trick_flash_phase(now), TrickFlashPhase::Live);
    now += Duration::from_millis(16);
    wd.tick(
        now, &cfg, geom, None, None, true, &mut out, &mut ink, &mut free, &mut nova,
    );
    assert_eq!(ink.len(), 3, "one cell per glyph of `sit`");

    let (mut painted, mut changed, mut last_fp) = (0usize, 0usize, 0u64);
    let allocations = allocations_during(|| {
        for _ in 0..40 {
            now += Duration::from_millis(16);
            let fp = wd.tick(
                now, &cfg, geom, None, None, true, &mut out, &mut ink, &mut free, &mut nova,
            );
            painted += ink.len();
            changed += usize::from(fp != last_fp);
            last_fp = fp;
        }
    });
    assert_eq!(
        painted,
        40 * 3,
        "the counted frames really painted the flash"
    );
    assert_eq!(changed, 40, "and every one of them was a new frame");
    assert_eq!(allocations, 0, "a flashing frame must not allocate");

    let allocations = allocations_during(|| {
        wd.revoke_trick_flash(now);
        for _ in 0..4 {
            now += Duration::from_millis(16);
            wd.tick(
                now, &cfg, geom, None, None, true, &mut out, &mut ink, &mut free, &mut nova,
            );
        }
    });
    assert_eq!(wd.trick_flash_phase(now), TrickFlashPhase::Revoked);
    assert_eq!(allocations, 0, "a revoke and its fade must not allocate");
}
