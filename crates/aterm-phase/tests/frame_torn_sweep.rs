// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WHOLE-FRAME SWEEP: over every measured screen, a reader judges a frame torn
//! ([`ScreenReader::frame_torn`]) only where one was caught mid-redraw, and every measured
//! half-drawn frame is one a turn-end reader takes again rather than judges
//! (`docs/measured/half-drawn-frame-2026-10-22.md`).
//!
//! The half frames are the lab's (`aterm-lab halfdraw scan` of three tabs' casts):
//!
//! * Claude Code 2.1.308 (`claude-2.1.308-lab-split-half.txt`): a 1024-byte read ended inside
//!   its DEC 2026 frame, the composer's bottom rule cut at 109 of 144 cells on the last row
//!   and its busy footer, drawn UNDER the composer, not reached: idle to the phase, with a
//!   composer. Torn by its geometry ([`aterm_phase::phase::composer_frame_torn`]).
//! * agy 1.4.0 (`agy-1.4.0-lab-split-half.txt`) and Gemini CLI 0.46.0
//!   (`gemini-0.46.0-lab-erase-half.txt`): each erases below before it redraws and draws its
//!   busy row ABOVE its composer, so a read cut before the busy row holds no composer either
//!   (the supervisor's `writing`: output still arriving), and a read that reached the composer
//!   passed the busy row. Their composer finders demand both edges as wide as the other, and
//!   Muse Code's a footer as the last row drawn: a cut edge is no composer.
//!
//! NEGATIVE CONTROL: no other fixture of the four programs reads torn, and each half frame's
//! whole successor (`*-split-whole`, `*-erase-whole`) is busy with a composer.

use aterm_phase::{Phase, Program};

use super::fixture_files::fixture_screens;

fn program_of(name: &str) -> Option<Program> {
    let file = name.rsplit('/').next()?;
    [
        ("claude-", Program::Claude),
        ("agy-", Program::Agy),
        ("muse-", Program::Muse),
        ("gemini-", Program::Gemini),
    ]
    .into_iter()
    .find_map(|(p, program)| file.starts_with(p).then_some(program))
}

#[test]
fn only_a_frame_caught_mid_redraw_reads_torn() {
    let screens = fixture_screens("frame_torn_sweep");
    let mut torn = Vec::new();
    let mut swept = 0usize;
    for (name, rows) in &screens {
        let Some(program) = program_of(name) else {
            continue;
        };
        swept += 1;
        if program.reader().frame_torn(rows) {
            torn.push(name.clone());
        }
    }
    assert!(swept > 500, "only {swept} screens swept");
    assert_eq!(
        torn,
        ["src/fixtures/claude-2.1.308-lab-split-half.txt"],
        "torn readings"
    );
}

#[test]
fn every_measured_half_frame_is_read_again() {
    let screens = fixture_screens("frame_torn_sweep");
    let rows = |file: &str| {
        screens
            .iter()
            .find(|(n, _)| n.ends_with(file))
            .map(|(_, r)| r.clone())
            .unwrap_or_else(|| panic!("{file}: no such fixture"))
    };
    // Claude Code: idle, a composer, torn.
    let half = rows("claude-2.1.308-lab-split-half.txt");
    let reader = Program::Claude.reader();
    assert_eq!(reader.phase(&half), Phase::Idle);
    assert!(reader.composer(&half).is_some());
    assert!(reader.frame_torn(&half));
    // agy, Gemini CLI: idle, no composer (`writing`).
    for (program, file) in [
        (Program::Agy, "agy-1.4.0-lab-split-half.txt"),
        (Program::Gemini, "gemini-0.46.0-lab-erase-half.txt"),
    ] {
        let half = rows(file);
        let reader = program.reader();
        assert_ne!(
            reader.phase(&half),
            Phase::Busy,
            "{file}: the busy row is not drawn"
        );
        assert!(reader.composer(&half).is_none(), "{file}: nor the composer");
    }
    // Each whole successor: busy, with its composer.
    for (program, file) in [
        (Program::Claude, "claude-2.1.308-lab-split-whole.txt"),
        (Program::Agy, "agy-1.4.0-lab-split-whole.txt"),
        (Program::Gemini, "gemini-0.46.0-lab-erase-whole.txt"),
    ] {
        let whole = rows(file);
        let reader = program.reader();
        assert_eq!(reader.phase(&whole), Phase::Busy, "{file}");
        assert!(reader.composer(&whole).is_some(), "{file}");
        assert!(!reader.frame_torn(&whole), "{file}");
    }
}

/// THE GEOMETRIC CHECK IS EVERY READER'S. Muse Code brackets nothing, so the kernel has no
/// frame to hold, and its composer is framed by two rules as Claude Code's is (the top one
/// labelled `── Voice input … ──`). Its end-of-turn screen with the composer's bottom rule cut
/// to 109 cells and the footer row of the frame before still under it, the shape a read that
/// split a redraw there would show (DERIVED from the measured fixture: no such Muse split is
/// measured), is idle with a composer, and torn. NEGATIVE CONTROL: the whole screen is not.
/// MUTANT (a reader with no geometry, `frame_torn` false by default, as the first cut of this
/// fix had it): the cut screen is judged idle.
#[test]
fn a_muse_composer_cut_above_its_footer_reads_torn() {
    let screens = fixture_screens("frame_torn_sweep");
    let file = "muse-1.4.10-R6427.1-end-of-turn.txt";
    let Some((_, whole)) = screens.iter().find(|(n, _)| n.ends_with(file)) else {
        panic!("{file}: no such fixture");
    };
    let reader = Program::Muse.reader();
    assert!(reader.composer(whole).is_some(), "a composer");
    assert!(!reader.frame_torn(whole), "the whole screen");
    let bottom = whole
        .iter()
        .rposition(|r| {
            let t = r.trim_end();
            t.chars().count() > 109 && t.chars().all(|c| c == '─')
        })
        .unwrap_or_else(|| panic!("{file}: no bottom rule"));
    assert!(bottom + 1 < whole.len(), "{file}: a footer under the rule");
    let mut cut = whole.clone();
    cut[bottom] = whole[bottom].chars().take(109).collect();
    assert_eq!(reader.phase(&cut), Phase::Idle, "idle to the phase");
    assert!(reader.composer(&cut).is_some(), "with its composer");
    assert!(reader.frame_torn(&cut), "torn");
}

/// THE GEOMETRIC CHECK, per reader: on each program's whole frame, the composer's bottom edge
/// cut to 109 cells (where Claude Code's measured split cut its own) and nothing drawn under it
/// is no composer to agy's, Muse Code's and Gemini CLI's readers, and a torn frame to Claude
/// Code's. A finder that took the cut frame whole would judge a redraw caught midway.
#[test]
fn a_composer_edge_cut_short_is_no_whole_frame() {
    let screens = fixture_screens("frame_torn_sweep");
    for (program, file) in [
        (Program::Claude, "claude-2.1.308-lab-split-whole.txt"),
        (Program::Agy, "agy-1.4.0-lab-split-whole.txt"),
        (Program::Gemini, "gemini-0.46.0-lab-erase-whole.txt"),
        (Program::Muse, "muse-1.4.10-R6427.1-idle.txt"),
    ] {
        let Some((_, whole)) = screens.iter().find(|(n, _)| n.ends_with(file)) else {
            panic!("{file}: no such fixture");
        };
        let reader = program.reader();
        assert!(reader.composer(whole).is_some(), "{file}: a composer");
        // The bottom edge: the last row of the composer's edge glyph, cut to 109 cells and
        // everything under it erased, as a read that split the redraw there shows it.
        let edge = whole
            .iter()
            .rposition(|r| {
                let t = r.trim_end();
                t.chars().count() > 109 && t.chars().all(|c| c == '─' || c == '▀')
            })
            .unwrap_or_else(|| panic!("{file}: no bottom edge"));
        let mut cut: Vec<String> = whole[..edge].to_vec();
        cut.push(whole[edge].chars().take(109).collect());
        cut.resize(whole.len(), String::new());
        assert!(
            reader.composer(&cut).is_none() || reader.frame_torn(&cut),
            "{file}: a cut edge read as a whole frame"
        );
    }
}
