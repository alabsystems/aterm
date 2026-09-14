// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Program output must change the row PRISM WAKE decorates. A TUI can keep
//! its caret on an unchanged composer while repainting a timer elsewhere.

use std::time::{Duration, Instant};

use aterm_core::{render::RenderInput, terminal::Terminal};
use aterm_effects::{
    cursor_glow::Geom,
    output_streak::{OutputStreak, StreakConfig},
};
use aterm_render::GlowQuad;
use aterm_spec::derive::output_streak_attribution_model;

const ROWS: usize = 8;
const COLS: usize = 48;

struct Host {
    term: Terminal,
    input: RenderInput,
    streak: OutputStreak,
    now: Instant,
    out: Vec<GlowQuad>,
}

impl Host {
    fn new(initial: &str) -> Self {
        let mut host = Self {
            term: Terminal::new(ROWS as u16, COLS as u16),
            input: RenderInput::default(),
            streak: OutputStreak::new(17),
            now: Instant::now(),
            out: Vec::new(),
        };
        host.term.process(initial.as_bytes());
        assert!(
            !host.observe(false),
            "the first frame only establishes history"
        );
        host
    }

    fn observe(&mut self, input_hot: bool) -> bool {
        self.term.cell_frame_into(&mut self.input, ROWS, COLS);
        self.streak.note_output_cells(
            &self.input.cells,
            (self.input.cursor_row, self.input.cursor_col),
            (ROWS, COLS),
            self.term.content_seq(),
            self.now,
            input_hot,
        )
    }

    fn output(&mut self, text: &str) -> bool {
        self.now += Duration::from_millis(100);
        self.term.process(text.as_bytes());
        self.observe(false)
    }

    fn paint(&mut self) -> u64 {
        self.paint_in_viewport(ROWS, COLS)
    }

    fn paint_in_viewport(&mut self, rows: usize, cols: usize) -> u64 {
        self.out.clear();
        let frame = self.streak.tick(
            self.now,
            Geom {
                cw: 8,
                ch: 16,
                rows,
                cols,
                origin_x: 0,
                origin_y: 0,
                win_w: (cols * 8) as u16,
                win_h: (rows * 16) as u16,
                head: 0,
            },
            &StreakConfig {
                enabled: true,
                sound: true,
                ..StreakConfig::default()
            },
            &mut self.out,
        );
        if frame.fp == 0 {
            assert!(frame.cue.is_none(), "a never-licensed frame owes no sound");
        }
        frame.fp
    }
}

#[test]
fn another_rows_timer_cannot_paint_the_stationary_composer() {
    let mut h = Host::new("Working 0\x1b[6;3HAsk Codex to do anything\x1b[6;3H");
    let composer = h.input.cells[5].clone();
    for n in 1..=60 {
        let before = h.term.content_seq();
        let licensed = h.output(&format!("\x1b[1;1HWorking {n}\x1b[K\x1b[6;3H"));
        assert_ne!(h.term.content_seq(), before, "real program output arrived");
        assert_eq!((h.input.cursor_row, h.input.cursor_col), (5, 2));
        assert_eq!(h.input.cells[5], composer, "the composer stayed unchanged");
        assert!(!licensed, "a different row's timer is not composer output");
        assert_eq!(h.paint(), 0);
        assert!(h.out.is_empty());
    }
    assert!(!h.streak.is_active());
}

#[test]
fn an_identical_prompt_redraw_does_not_restart_output_light() {
    let mut h = Host::new("\x1b[6;3HAsk Codex to do anything\x1b[6;3H");
    assert!(!h.output("\x1b[6;1H\x1b[2K  Ask Codex to do anything\x1b[6;3H"));
    assert_eq!(h.paint(), 0);
}

#[test]
fn genuine_same_row_append_and_rewrite_are_output() {
    let mut append = Host::new("result");
    assert!(append.output(" ready"));
    assert_ne!(append.paint(), 0);
    assert!(append.out.iter().all(|q| q.row == 0));

    let mut rewrite = Host::new("\x1b[3;1Hprogress 1");
    assert!(rewrite.output("\x1b[3;1Hprogress 2"));
    assert_ne!(rewrite.paint(), 0);
    assert!(rewrite.out.iter().all(|q| q.row == 2));
}

#[test]
fn newline_and_soft_wrap_keep_the_newly_written_row() {
    let mut newline = Host::new("");
    assert!(newline.output("new output\r\n"));
    assert_eq!((newline.input.cursor_row, newline.input.cursor_col), (1, 0));
    assert_ne!(newline.paint(), 0);
    assert!(newline.out.iter().all(|q| q.row == 0));

    let mut wrap = Host::new(&"x".repeat(COLS));
    assert!(wrap.output("fresh"));
    assert_eq!((wrap.input.cursor_row, wrap.input.cursor_col), (1, 5));
    assert_ne!(wrap.paint(), 0);
    assert!(wrap.out.iter().all(|q| q.row == 1));
}

#[test]
fn clearing_a_suffix_cannot_license_the_surviving_prefix() {
    let mut h = Host::new("kept removed");
    assert!(!h.output("\x1b[1;5H\x1b[K"));
    assert_eq!(h.paint(), 0);
    assert!(
        h.output(" fresh"),
        "new ink after the clear still earns light"
    );
}

#[test]
fn moving_an_existing_input_row_is_not_new_output() {
    let mut h = Host::new("\x1b[6;3HAsk Codex to do anything\x1b[6;3H");
    assert!(!h.output("\x1b[2J\x1b[5;3HAsk Codex to do anything\x1b[5;3H"));
    assert_eq!(h.paint(), 0);
    assert!(
        h.output("x"),
        "a genuine replacement at the new location is output"
    );
}

#[test]
fn an_echo_discounted_change_is_consumed_without_later_replay() {
    let mut h = Host::new("prompt");
    h.now += Duration::from_millis(50);
    h.streak.note_keystroke(h.now);
    h.term.process(b" typed");
    assert!(!h.observe(true));
    h.now += Duration::from_secs(1);
    assert!(!h.output("\x1b[3;1Htimer changed\x1b[1;13H"));
    assert_eq!(h.paint(), 0);
    assert!(h.output("!"));
}

#[test]
fn wide_glyph_rewrites_and_presentation_changes_are_output() {
    let mut h = Host::new("\u{4f60}");
    assert!(h.output("\r\u{597d}"));
    assert_ne!(h.paint(), 0);
    let mut continuation_lit = false;
    for _ in 0..50 {
        h.paint();
        assert!(h.out.iter().all(|q| q.x + q.w <= 16));
        continuation_lit |= h.out.iter().any(|q| q.x == 8);
        h.now += Duration::from_millis(20);
    }
    assert!(continuation_lit, "the changed wide glyph owns both columns");

    let mut presentation = Host::new("\u{2764}");
    assert!(presentation.output("\u{fe0f}"));
    assert_ne!(presentation.paint(), 0);
}

#[test]
fn colour_and_bold_repaint_of_the_same_text_is_not_new_output() {
    let mut h = Host::new("prompt");
    assert!(!h.output("\r\x1b[31;1mprompt\x1b[0m"));
    assert_eq!(h.paint(), 0);
}

#[test]
fn repeated_new_output_is_not_mistaken_for_a_moved_row() {
    let mut h = Host::new("same\r\n");
    assert!(h.output("same\r\n"));
    assert_ne!(h.paint(), 0);
    assert!(h.out.iter().all(|q| q.row == 1));
}

#[test]
fn sparse_particle_rewrites_never_light_the_unchanged_composer_or_gaps() {
    // The captured TUI uses braille particles. ASCII obeys the same rule:
    // this is attribution by changed cells, not a Unicode-block exception.
    for particles in [["⠁", "⠂"], ["x", "y"]] {
        let mut h = Host::new(&format!(
            "\x1b[6;3HAsk Codex\x1b[6;25H{}\x1b[6;41H{}\x1b[6;3H",
            particles[0], particles[0]
        ));
        let placeholder = h.input.cells[5][2..11].to_vec();
        let mut painted = 0;
        // Enough sustained arrivals to exercise both comets and the flood
        // ribbon: neither may bridge from column 24 to 40 over blank cells.
        for n in 0..60 {
            let ch = particles[(n + 1) % 2];
            assert!(h.output(&format!("\x1b[6;25H{ch}\x1b[6;41H{ch}\x1b[6;3H")));
            assert_eq!((h.input.cursor_row, h.input.cursor_col), (5, 2));
            assert_eq!(h.input.cells[5][2..11], placeholder);
            if h.paint() != 0 {
                painted += 1;
            }
            assert!(
                h.out
                    .iter()
                    .all(|q| q.row == 5 && q.x >= 40 * 8 && q.x + q.w <= 41 * 8),
                "every emitted pixel must stay in the actual changed run: {:?}",
                h.out
            );
        }
        assert!(painted > 40, "the live output effect remains enabled");
    }
}

#[test]
fn ordinary_contiguous_output_can_still_sweep_its_full_changed_run() {
    let mut h = Host::new("prefix ");
    assert!(h.output("continuous"));
    let mut rightmost = 0;
    for _ in 0..50 {
        h.paint();
        for q in &h.out {
            assert!(q.x >= 7 * 8 && q.x + q.w <= 17 * 8);
            rightmost = rightmost.max(q.x);
        }
        h.now += Duration::from_millis(20);
    }
    assert!(rightmost >= 15 * 8, "the comet traverses real new text");
}

#[test]
fn real_cell_observations_refine_the_attribution_snapshot_model() {
    let model = output_streak_attribution_model();
    let mut state = model.init_state();
    let mut visited = std::collections::BTreeSet::new();
    // Only the public licence decision comes from OutputStreak. Token changes
    // and glyph identity are read from the real terminal; event facts below
    // describe the VT stimuli and are pinned against its actual visible cells.
    let mut step =
        |action, licensed: bool, token: i64, snapshot: i64, fresh, echo, moved, based| {
            let mut next = state.clone();
            for (name, value) in [
                ("based", based),
                ("token", token),
                ("snapshot", snapshot),
                ("fresh", fresh),
                ("echo", echo),
                ("moved", moved),
                ("licensed", i64::from(licensed)),
                (
                    "geometry_changed",
                    i64::from(matches!(
                        action,
                        "GeometryBaseline" | "GeometryWithoutToken"
                    )),
                ),
            ] {
                next.insert(name, value);
            }
            assert!(
                model.successors(action, &state).contains(&next),
                "real decision does not refine {action}: {state:?} -> {next:?}"
            );
            assert!(model.check_invariant("LicensedRequiresFreshUndiscountedInk", &next));
            assert!(model.check_invariant("GeometryBaselineIsSilent", &next));
            state = next;
            visited.insert(action);
        };
    let mut h = Host::new("a"); // Host::new pins the real baseline licence false.
    let glyph = |h: &Host| i64::from(h.input.cells[h.input.cursor_row][0].ch == 'a');
    let mut token = 0;
    step("Baseline", false, token, glyph(&h), 0, 0, 0, 1);
    step(
        "StableToken",
        h.observe(false),
        token,
        glyph(&h),
        0,
        0,
        0,
        1,
    );
    let before = h.term.content_seq();
    let licensed = h.output("\x1b[3;1Htimer\x1b[1;2H");
    token ^= i64::from(h.term.content_seq() != before);
    assert_eq!(h.input.cells[0][0].ch, 'a');
    step("OtherRow", licensed, token, glyph(&h), 0, 0, 0, 1);

    let before = h.term.content_seq();
    let licensed = h.output("\rb");
    token ^= i64::from(h.term.content_seq() != before);
    assert_eq!(h.input.cells[0][0].ch, 'b');
    step("ChangedRun", licensed, token, glyph(&h), 1, 0, 0, 1);

    let before = h.term.content_seq();
    h.now += Duration::from_millis(100);
    h.streak.note_keystroke(h.now);
    h.term.process(b"\ra");
    let licensed = h.observe(true);
    token ^= i64::from(h.term.content_seq() != before);
    assert_eq!(h.input.cells[0][0].ch, 'a');
    step("Echo", licensed, token, glyph(&h), 1, 1, 0, 1);

    // Consuming the echo snapshot is observable at the next non-echo token:
    // leaving the old 'b' behind would mis-license unchanged 'a' here.
    h.now += Duration::from_secs(1);
    let before = h.term.content_seq();
    let licensed = h.output("\x1b[3;1Htimer 2\x1b[1;2H");
    token ^= i64::from(h.term.content_seq() != before);
    assert_eq!(h.input.cells[0][0].ch, 'a');
    step("OtherRow", licensed, token, glyph(&h), 0, 0, 0, 1);

    let before = h.term.content_seq();
    let licensed = h.output("\x1b[2J\x1b[2;1Ha");
    token ^= i64::from(h.term.content_seq() != before);
    assert!(h.input.cells[0].is_empty() || h.input.cells[0][0].ch == ' ');
    assert_eq!(h.input.cells[1][0].ch, 'a');
    step("Relocated", licensed, token, glyph(&h), 1, 0, 1, 1);

    // A geometry discontinuity establishes history before accepting another
    // character. Exercise the real helper with one fewer supplied visible row.
    let before = h.term.content_seq();
    h.term.process(b"\rb");
    h.term.cell_frame_into(&mut h.input, ROWS, COLS);
    let licensed = h.streak.note_output_cells(
        &h.input.cells,
        (h.input.cursor_row, h.input.cursor_col),
        (ROWS - 1, COLS),
        h.term.content_seq(),
        h.now,
        false,
    );
    token ^= i64::from(h.term.content_seq() != before);
    step("GeometryBaseline", licensed, token, glyph(&h), 0, 0, 0, 1);

    // The host's width changes again while the snapshot/token remains the
    // same. This must pass through the geometry baseline before the idle gate.
    let before = h.term.content_seq();
    let licensed = h.streak.note_output_cells(
        &h.input.cells,
        (h.input.cursor_row, h.input.cursor_col),
        (ROWS - 1, COLS - 1),
        h.term.content_seq(),
        h.now,
        false,
    );
    assert_eq!(h.term.content_seq(), before);
    step(
        "GeometryWithoutToken",
        licensed,
        token,
        glyph(&h),
        0,
        0,
        0,
        1,
    );

    h.streak.rebase();
    step("Rebase", false, 0, 0, 0, 0, 0, 0);
    h.term.process(b"\ra");
    let licensed = h.observe(false);
    step("Baseline", licensed, 0, glyph(&h), 0, 0, 0, 1);

    assert_eq!(visited, model.actions.iter().map(|a| a.name).collect());

    // Reproduce the historical convenience-path shape through the shipping
    // low-level API: pass existing ink as if globally changed token meant row
    // damage. The projection MUST reject its real (wrong) licence decision.
    let mut wrong = OutputStreak::new(17);
    assert!(!wrong.note_output(1, &[(0, 0, 0)], h.now, false));
    let wrong_licence = wrong.note_output(2, &[(0, 0, 0)], h.now, false);
    assert!(wrong_licence);
    let mut bad = model.init_state();
    assert!(model.fire("Baseline", &mut bad));
    assert!(model.fire("OtherRow", &mut bad));
    bad.insert("licensed", i64::from(wrong_licence));
    assert!(!model.check_invariant("LicensedRequiresFreshUndiscountedInk", &bad));

    // Reflowed old text does differ at the newly selected coordinates; that
    // alone passes the fresh-ink law. The geometry law must reject it.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut reflow = model.init_state();
    assert!(model.fire("Baseline", &mut reflow));
    assert!(buggy.fire("GeometryBaseline", &mut reflow));
    assert!(model.check_invariant("LicensedRequiresFreshUndiscountedInk", &reflow));
    assert!(!model.check_invariant("GeometryBaselineIsSilent", &reflow));
}

#[test]
fn rebase_does_not_compare_a_new_sessions_rows_with_the_old_one() {
    let mut h = Host::new("old session");
    h.streak.rebase();
    assert!(!h.output("\r\x1b[2Knew session"));
    assert_eq!(h.paint(), 0);
    assert!(h.output(" output"));
}

#[test]
fn width_only_reflow_does_not_licence_existing_text_as_new_output() {
    let mut h = Host::new("abcdefghijklmnopqrstuvwxyz0123456789");
    let original_token = h.term.content_seq();
    assert_eq!((h.input.cursor_row, h.input.cursor_col), (0, 36));

    // Real geometry change, no PTY write. Height stays fixed, while existing
    // text wraps onto a formerly blank row. Sparse row lengths cannot tell
    // this apart from output; the host's actual column count can.
    h.now += Duration::from_secs(1);
    h.term.resize(ROWS as u16, 24);
    h.term.cell_frame_into(&mut h.input, ROWS, 24);
    assert_ne!(h.term.content_seq(), original_token);
    assert_eq!((h.input.cursor_row, h.input.cursor_col), (1, 12));
    assert_eq!(
        h.input.cells[1].iter().map(|c| c.ch).collect::<String>(),
        "yz0123456789",
        "the selected row contains reflowed history, not new bytes"
    );
    assert!(
        !h.streak.note_output_cells(
            &h.input.cells,
            (h.input.cursor_row, h.input.cursor_col),
            (ROWS, 24),
            h.term.content_seq(),
            h.now,
            false,
        ),
        "a width-only reflow must establish geometry without licensing output"
    );
    assert_eq!(h.paint_in_viewport(ROWS, 24), 0);

    h.now += Duration::from_millis(100);
    h.term.process(b"!");
    h.term.cell_frame_into(&mut h.input, ROWS, 24);
    assert!(
        h.streak.note_output_cells(
            &h.input.cells,
            (h.input.cursor_row, h.input.cursor_col),
            (ROWS, 24),
            h.term.content_seq(),
            h.now,
            false,
        ),
        "real new text after the resize still earns light"
    );
    assert_ne!(h.paint_in_viewport(ROWS, 24), 0);
    assert!(h.out.iter().all(|q| q.row == 1 && q.x == 12 * 8));
}

#[test]
fn viewport_change_retires_pending_and_live_output_even_before_a_new_token() {
    for already_painted in [false, true] {
        for terminal_resized in [false, true] {
            let mut h = Host::new("");
            assert!(h.output("machine output"));
            if already_painted {
                assert_ne!(h.paint(), 0);
                assert!(h.streak.is_active());
            }
            let before = h.term.content_seq();
            h.now += Duration::from_millis(40);
            if terminal_resized {
                h.term.resize(ROWS as u16, 24);
            }
            // A host can receive its new viewport before a resized terminal
            // frame. The independently supplied geometry must retire the old
            // effect even when the content token still names the same cells.
            h.term.cell_frame_into(&mut h.input, ROWS, 24);
            assert_eq!(h.term.content_seq() != before, terminal_resized);
            assert!(!h.streak.note_output_cells(
                &h.input.cells,
                (h.input.cursor_row, h.input.cursor_col),
                (ROWS, 24),
                h.term.content_seq(),
                h.now,
                false,
            ));
            assert!(!h.streak.is_active());
            assert_eq!(h.paint_in_viewport(ROWS, 24), 0);
            assert!(
                h.out.is_empty(),
                "old pending and resident geometry is gone"
            );
        }
    }
}

#[test]
fn compact_row_growth_at_fixed_dimensions_preserves_live_output() {
    let mut h = Host::new("");
    assert!(h.output("first"));
    assert_ne!(h.paint(), 0);
    let previous_prefix_len = h.input.cells[0].len();
    assert!(h.output(" longer"));
    assert!(h.input.cells[0].len() > previous_prefix_len);
    assert!(
        h.streak.is_active(),
        "prefix growth is not a geometry reset"
    );
    assert_ne!(h.paint(), 0);
    // The already-admitted comet continues on its original text. A reset on
    // sparse prefix growth would instead erase it or jump to the later word.
    assert!(h.out.iter().all(|q| q.x + q.w <= 5 * 8));
}
