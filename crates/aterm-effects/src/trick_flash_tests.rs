// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::word_decorations::{DecoConfig, EffectGeom, WordDecorations};
use aterm_core::render::RenderInput;
use aterm_core::terminal::Terminal;
use aterm_lexicon::Lexicon;

/// Which rescan twin a scenario drives: the cold term walk the batteries
/// use, or the snapshot path the shipping renderer uses.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Path {
    Cold,
    Snapshot,
}
const BOTH: [Path; 2] = [Path::Cold, Path::Snapshot];

const fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

struct Glass {
    term: Terminal,
    wd: WordDecorations,
    lex: Lexicon,
    cfg: DecoConfig,
    rows: usize,
    cols: usize,
    path: Path,
    /// The host declares the snapshot's `base_y` before each rescan.
    wired: bool,
    epoch: u64,
    snap: RenderInput,
}

impl Glass {
    fn new(path: Path, rows: usize, cols: usize) -> Self {
        Glass {
            term: Terminal::new(rows as u16, cols as u16),
            wd: WordDecorations::default(),
            lex: Lexicon::with_languages(&["en"]),
            cfg: DecoConfig::default(),
            rows,
            cols,
            path,
            wired: true,
            epoch: 0,
            snap: RenderInput::default(),
        }
    }

    const fn unwired(mut self) -> Self {
        self.wired = false;
        self
    }

    fn feed(&mut self, text: &str) {
        self.term.process(text.as_bytes());
    }

    fn caret(&self) -> (u16, u16) {
        let cursor = self.term.cursor();
        (cursor.row, cursor.col)
    }

    fn scan(&mut self, now: Instant) {
        self.scan_with_cursor(now, true);
    }

    fn scan_with_cursor(&mut self, now: Instant, with_cursor: bool) {
        self.epoch += 1;
        match self.path {
            Path::Cold => {
                if self.wired {
                    let base_y = self.term.grid().base_y() as i64;
                    self.wd.set_scan_base_y(Some(base_y));
                }
                self.wd.rescan(
                    &self.term, self.rows, self.cols, &self.lex, &self.cfg, self.epoch, now,
                );
            }
            Path::Snapshot => {
                self.term
                    .cell_frame_into(&mut self.snap, self.rows, self.cols);
                if self.wired {
                    self.wd.set_scan_base_y(Some(self.snap.base_y));
                }
                let cursor = with_cursor.then(|| self.caret());
                self.wd.rescan_from_cells_with_geom_at_cursor(
                    &self.snap.cells,
                    &self.snap.line_sizes,
                    self.rows,
                    self.cols,
                    &self.lex,
                    &self.cfg,
                    self.epoch,
                    now,
                    EffectGeom::default(),
                    self.snap.default_bg,
                    cursor,
                );
            }
        }
    }

    fn echo(&mut self, text: &str, now: Instant) {
        self.feed(text);
        self.scan(now);
    }

    fn fire(&mut self, word: &str, back_chars: u16, boundary: &str, now: Instant) {
        self.wd.note_trick_typed(now, None, word, back_chars);
        self.echo(boundary, now);
    }

    fn frame(&mut self, now: Instant) -> (Vec<InkCell>, u64) {
        let (mut out, mut ink, mut free, mut nova) = (vec![], vec![], vec![], vec![]);
        let fp = self.wd.tick(
            now,
            &self.cfg,
            EffectGeom::default(),
            None,
            None,
            true,
            &mut out,
            &mut ink,
            &mut free,
            &mut nova,
        );
        (ink, fp)
    }

    fn row(&self, row: usize) -> Vec<RenderCell> {
        let mut cells = Vec::new();
        self.term.render_row_into(row, &mut cells);
        cells
    }
}

fn cells_at(ink: &[InkCell]) -> Vec<(u16, u16)> {
    ink.iter().map(|cell| (cell.row, cell.col)).collect()
}

fn sorted_unique(ink: &[InkCell]) -> bool {
    ink.windows(2)
        .all(|w| (w[0].row, w[0].col) < (w[1].row, w[1].col))
}

fn law(
    caret: (u16, u16),
    word: &str,
    i: usize,
    t: u64,
    base: [u8; 3],
    first_fg: [u8; 3],
    bg: [u8; 3],
) -> [u8; 3] {
    let chars: Vec<char> = word.chars().collect();
    let (sat, val) = resolve_sat_val(rgb3_to_u32(bg));
    let strength = legible_strength(rgb3_to_u32(first_fg), rgb3_to_u32(bg), sat, val);
    let hue0 = rainbow_base_hue(hue_seed(caret, &chars));
    let target = hsv2rgb(flash_hue(hue0, i, chars.len(), t), sat, val);
    u32_to_rgb3(mix_rgb(
        rgb3_to_u32(base),
        target,
        envelope(t, None) * strength,
    ))
}

#[test]
fn a_typed_word_flashes_then_emits_nothing_with_a_stable_fp_and_a_disarmed_scheduler() {
    for path in BOTH {
        let t0 = Instant::now();
        let mut g = Glass::new(path, 4, 40);
        g.echo("$ sit", t0);
        assert_eq!(g.wd.trick_flash_phase(t0), TrickFlashPhase::Idle);
        g.wd.note_trick_typed(t0, None, "sit", 0);
        assert_eq!(
            g.wd.trick_flash_phase(t0),
            TrickFlashPhase::Pending,
            "{path:?}: noted, not yet found"
        );
        let (ink, fp) = g.frame(t0);
        assert!(
            ink.is_empty() && fp == 0,
            "{path:?}: a pending flash paints nothing"
        );
        g.echo(" ", t0);
        assert_eq!(g.wd.trick_flash_phase(t0), TrickFlashPhase::Live);

        let (ink, fp_a) = g.frame(t0 + ms(300));
        assert_eq!(
            cells_at(&ink),
            [(0, 2), (0, 3), (0, 4)],
            "{path:?}: one cell per glyph of `sit`"
        );
        assert_ne!(fp_a, 0);
        assert!(
            g.wd.is_active(t0 + ms(300)),
            "{path:?}: a live flash on a wordless screen must arm the scheduler"
        );
        let (_, fp_b) = g.frame(t0 + ms(316));
        assert_ne!(
            fp_a, fp_b,
            "{path:?}: an animating flash never repeats an fp"
        );

        let done = t0 + ms(u64::from(FLASH_MS));
        let (ink, fp) = g.frame(done);
        assert!(ink.is_empty(), "{path:?}: past the window, zero cells");
        assert_eq!(fp, 0, "{path:?}: …and the pre-feature fingerprint");
        assert_eq!(g.frame(done + ms(16)).1, 0, "{path:?}: …which is stable");
        assert!(!g.wd.is_active(done), "{path:?}: …and nothing is armed");
        assert_eq!(g.wd.trick_flash_phase(done), TrickFlashPhase::Idle);
        g.scan(done + ms(32));
        assert!(g.frame(done + ms(48)).0.is_empty());
    }
}

#[test]
fn the_cold_path_and_the_snapshot_path_paint_the_same_bytes() {
    let t0 = Instant::now();
    let mut runs = BOTH.map(|path| {
        let mut g = Glass::new(path, 4, 40);
        g.echo("$ good kitty", t0);
        g.fire("good", 6, " ", t0);
        [0u64, 90, 400, 1000, 1200].map(|t| g.frame(t0 + ms(t)))
    });
    let snapshot = runs[1].clone();
    assert_eq!(runs[0], snapshot);
    assert!(runs[0].iter_mut().any(|(ink, _)| !ink.is_empty()));
}

#[test]
fn the_envelope_ramps_in_holds_and_fades_to_zero_at_both_ends() {
    let (ramp, fade_from, whole) = (
        u64::from(RAMP_MS),
        u64::from(FLASH_MS - FADE_MS),
        u64::from(FLASH_MS),
    );
    assert_eq!(envelope(0, None), 0.0);
    assert_eq!(envelope(whole, None), 0.0);
    for t in 1..=ramp {
        assert!(
            envelope(t, None) > envelope(t - 1, None),
            "ramp must rise at {t} ms"
        );
    }
    for t in ramp..=fade_from {
        assert_eq!(envelope(t, None), 1.0, "hold at {t} ms");
    }
    for t in fade_from + 1..=whole {
        assert!(
            envelope(t, None) < envelope(t - 1, None),
            "fade must fall at {t} ms"
        );
    }
    let biggest = (1..=whole)
        .map(|t| (envelope(t, None) - envelope(t - 1, None)).abs())
        .fold(0.0f32, f32::max);
    assert!(biggest < 0.02, "a {biggest} jump in one millisecond");
}

#[test]
fn a_word_recoloured_mid_flash_fades_to_the_colour_it_has_now() {
    for path in BOTH {
        let t0 = Instant::now();
        let mut g = Glass::new(path, 4, 40);
        g.echo("$ sit", t0);
        let caret = g.caret();
        g.fire("sit", 0, " ", t0);
        let was = g.row(0)[2].fg;
        let red = [255u8, 85, 85];
        assert_ne!(was, red);
        g.echo("\r$ \x1b[38;2;255;85;85msit\x1b[m ", t0 + ms(100));
        let bg = g.row(0)[2].bg;
        let (ink, _) = g.frame(t0 + ms(500));
        assert_eq!(ink.len(), 3, "{path:?}: the same word, still flashing");
        for (i, cell) in ink.iter().enumerate() {
            assert_eq!(
                cell.color,
                law(caret, "sit", i, 500, red, red, bg),
                "{path:?}: cell {i} mixes from the recaptured colour"
            );
        }
        let (ink, _) = g.frame(t0 + ms(u64::from(FLASH_MS) - 1));
        for cell in &ink {
            for (got, want) in cell.color.iter().zip(red) {
                assert!(
                    got.abs_diff(want) <= 1,
                    "{path:?}: the last frame is {:?}, the word is now {red:?}",
                    cell.color
                );
            }
        }
    }
}

#[test]
fn echo_sit_output_never_inks_without_a_note() {
    for path in BOTH {
        let t0 = Instant::now();
        let mut g = Glass::new(path, 6, 40);
        g.echo("$ echo sit\r\nsit\r\n$ sit ", t0);
        g.wd.note_typed_edit(t0, None);
        g.scan(t0);
        for t in [0u64, 16, 300, 1100, 2000] {
            let (ink, fp) = g.frame(t0 + ms(t));
            assert!(ink.is_empty() && fp == 0, "{path:?}: t = {t}");
            assert_eq!(g.wd.trick_flash_phase(t0 + ms(t)), TrickFlashPhase::Idle);
        }
        assert!(!g.wd.is_active(t0));
    }
}

#[test]
fn enter_on_the_bottom_row_is_found_on_the_scrolled_row_never_on_the_shells_error_line() {
    let prompt = format!("{}$ ", "~".repeat(22));
    let script = |g: &mut Glass, t0: Instant| {
        g.echo(&format!("one\r\ntwo\r\nthree\r\n{prompt}sit"), t0);
        assert_eq!(g.caret(), (3, 27), "typed on the bottom row");
        g.fire(
            "sit",
            0,
            &format!("\r\nzsh: command not found: sit\r\n{prompt}"),
            t0,
        );
        let typed: String = g.row(1).iter().map(|cell| cell.ch).collect();
        let error: String = g.row(2).iter().map(|cell| cell.ch).collect();
        assert!(typed.starts_with(&format!("{prompt}sit")), "{typed:?}");
        assert_eq!(&error[24..27], "sit", "the adversarial column: {error:?}");
        g.frame(t0 + ms(300)).0
    };
    for path in BOTH {
        let t0 = Instant::now();
        let mut g = Glass::new(path, 4, 40);
        assert_eq!(
            cells_at(&script(&mut g, t0)),
            [(1, 24), (1, 25), (1, 26)],
            "{path:?}: the TYPED line, two rows up"
        );
        let mut blind = Glass::new(path, 4, 40).unwired();
        assert_eq!(
            cells_at(&script(&mut blind, t0)),
            [(2, 24), (2, 25), (2, 26)],
            "{path:?}: non-vacuous — without base_y the error line wins"
        );
    }
}

#[test]
fn a_scroll_region_scroll_is_followed_upward_by_exact_anchor_only() {
    for path in BOTH {
        let t0 = Instant::now();
        let mut g = Glass::new(path, 6, 40);
        g.echo("\x1b[2;5r\x1b[5;1H$ sit", t0);
        assert_eq!(g.caret(), (4, 5));
        let base_y = g.term.grid().base_y();
        g.fire("sit", 0, "\r\nzsh: command not found: sit\r\n$ ", t0);
        assert_eq!(g.term.grid().base_y(), base_y, "a region scroll");
        assert_eq!(
            cells_at(&g.frame(t0 + ms(300)).0),
            [(2, 2), (2, 3), (2, 4)],
            "{path:?}: past the error line (not exactly anchored) to the typed one"
        );

        let mut g = Glass::new(path, 6, 40);
        g.echo("\x1b[2;5r\x1b[5;1H$ s", t0);
        g.wd.note_trick_typed(t0, None, "sit", 0);
        let mut still = Glass::new(path, 6, 40);
        still.echo("\x1b[2;5r\x1b[5;1H$ s", t0);
        still.wd.note_trick_typed(t0, None, "sit", 0);
        still.echo("it ", t0);
        assert_eq!(
            cells_at(&still.frame(t0 + ms(300)).0),
            [(4, 2), (4, 3), (4, 4)],
            "{path:?}: a lagging caret still finds the word on ITS row"
        );
        g.echo("it\r\nzsh: command not found: sit\r\n$ ", t0);
        assert!(
            g.frame(t0 + ms(300)).0.is_empty(),
            "{path:?}: …but never by searching upward"
        );

        let mut g = Glass::new(path, 6, 40);
        g.echo("\x1b[2;5r\x1b[5;1H$ good kitty", t0);
        g.fire("good", 6, "\r\nzsh: command not found: good\r\n$ ", t0);
        let flashed: Vec<(u16, u16)> = cells_at(&g.frame(t0 + ms(300)).0)
            .into_iter()
            .filter(|(_, col)| *col < 6)
            .collect();
        assert_eq!(flashed, [(2, 2), (2, 3), (2, 4), (2, 5)], "{path:?}");

        let mut g = Glass::new(path, 6, 40);
        g.echo("\x1b[2;5r\x1b[4;1H$ sit", t0);
        assert_eq!(g.caret(), (3, 5));
        g.fire("sit", 0, "\r\nzsh: command not found: sit\r\n[127] $ ", t0);
        assert_eq!(g.caret(), (4, 8));
        assert_eq!(
            cells_at(&g.frame(t0 + ms(300)).0),
            [(2, 2), (2, 3), (2, 4)],
            "{path:?}: the cursor left the caret's row"
        );
        let mut g = Glass::new(path, 6, 40);
        g.echo("\x1b[2;5r\x1b[5;1H$ sit", t0);
        g.fire("sit", 0, "\r\nzsh: command not found: sit\r\n[127] $ ", t0);
        assert_eq!(g.caret(), (4, 8));
        assert!(g.frame(t0 + ms(300)).0.is_empty(), "{path:?}");
    }
}

#[test]
fn a_reported_scroll_is_never_second_guessed_by_searching() {
    for path in BOTH {
        let t0 = Instant::now();
        let mut g = Glass::new(path, 6, 40);
        g.echo(
            "a\r\nb\r\n$ sit\r\nzsh: command not found: sit\r\nc\r\n$ sit",
            t0,
        );
        assert_eq!(g.caret(), (5, 5));
        g.fire("sit", 0, "\r\x1b[K\r\n$ ", t0);
        let history: String = g.row(1).iter().take(5).map(|cell| cell.ch).collect();
        assert_eq!(history, "$ sit", "exactly anchored, three rows above");
        assert!(g.frame(t0 + ms(300)).0.is_empty(), "{path:?}");
        let mut blind = Glass::new(path, 6, 40).unwired();
        blind.echo(
            "a\r\nb\r\n$ sit\r\nzsh: command not found: sit\r\nc\r\n$ sit",
            t0,
        );
        blind.fire("sit", 0, "\r\x1b[K\r\n$ ", t0);
        assert_eq!(
            cells_at(&blind.frame(t0 + ms(300)).0)[0],
            (1, 2),
            "{path:?}"
        );
    }
}

#[test]
fn a_late_echo_never_sends_the_search_to_last_times_line() {
    for path in BOTH {
        for wired in [true, false] {
            let t0 = Instant::now();
            let mut g = Glass::new(path, 8, 40);
            g.wired = wired;
            g.echo("$ sit\r\nzsh: command not found: sit\r\n$ ", t0);
            assert_eq!(g.caret(), (2, 2), "the prompt's end, nothing typed yet");
            g.wd.note_trick_typed(t0, None, "sit", 0);
            g.echo("s", t0 + ms(40));
            assert_eq!(
                g.wd.trick_flash_phase(t0 + ms(40)),
                TrickFlashPhase::Pending,
                "{path:?}, wired = {wired}: `$ s` is a word still arriving"
            );
            assert!(
                g.frame(t0 + ms(50)).0.is_empty(),
                "{path:?}, wired = {wired}: history never flashes"
            );
            g.echo("\x1b[1;1H", t0 + ms(60));
            assert_eq!(g.caret(), (0, 0));
            assert_eq!(
                g.wd.trick_flash_phase(t0 + ms(60)),
                TrickFlashPhase::Pending,
                "{path:?}, wired = {wired}: `$ sit` two rows up STARTS at the caret, and is history"
            );
            g.echo("\x1b[3;4Hit ", t0 + ms(80));
            assert_eq!(
                cells_at(&g.frame(t0 + ms(300)).0),
                [(2, 2), (2, 3), (2, 4)],
                "{path:?}, wired = {wired}: the word itself, once it lands"
            );

            let mut g = Glass::new(path, 8, 40);
            g.wired = wired;
            g.echo("$ echo sit\r\nsit\r\n$ s", t0);
            assert_eq!(g.caret(), (2, 3));
            g.wd.note_trick_typed(t0, None, "sit", 0);
            g.echo("\x1b7\x1b[1;30H12:00\x1b8", t0 + ms(20));
            assert_eq!(g.caret(), (2, 3));
            assert_eq!(
                g.wd.trick_flash_phase(t0 + ms(20)),
                TrickFlashPhase::Pending,
                "{path:?}, wired = {wired}: nothing has moved"
            );
            g.echo("i", t0 + ms(40));
            assert_eq!(
                g.wd.trick_flash_phase(t0 + ms(40)),
                TrickFlashPhase::Pending,
                "{path:?}, wired = {wired}: `$ si` is a word still arriving"
            );
            assert!(
                g.frame(t0 + ms(50)).0.is_empty(),
                "{path:?}, wired = {wired}: program output never flashes"
            );
            g.echo("t ", t0 + ms(80));
            assert_eq!(
                cells_at(&g.frame(t0 + ms(300)).0),
                [(2, 2), (2, 3), (2, 4)],
                "{path:?}, wired = {wired}"
            );
        }
    }
}

#[test]
fn a_declaration_is_spent_by_the_rescan_it_was_made_for() {
    for path in BOTH {
        let t0 = Instant::now();
        let mut g = Glass::new(path, 6, 40);
        g.echo("a\r\nb\r\nc\r\nd\r\ne\r\n$ sit", t0);
        g.wired = false;
        g.echo(" \r\nzsh: command not found: sit\r\n$ sit", t0);
        assert_eq!(g.caret(), (5, 5));
        let history: String = g.row(3).iter().take(5).map(|cell| cell.ch).collect();
        assert_eq!(history, "$ sit", "exactly anchored, two rows above");
        g.wired = true;
        g.fire("sit", 0, " ", t0 + ms(2000));
        assert_eq!(
            cells_at(&g.frame(t0 + ms(2300)).0),
            [(5, 2), (5, 3), (5, 4)],
            "{path:?}: the typed line, not the one a stale origin points at"
        );
    }
}

#[test]
fn an_exact_anchor_beats_an_inside_one_beats_slack_and_among_equals_the_rightmost_wins() {
    use Anchor::{Exact, Inside, Slack};
    let mut g = Glass::new(Path::Cold, 2, 40);
    g.feed("$ sit sit");
    let row = g.row(0);
    let find = |caret_col: u16, cursor_col: Option<u16>| {
        find_in_row(&row, &['s', 'i', 't'], caret_col, 0, cursor_col, false)
            .map(|hit| (hit.cols[0], hit.anchor))
    };
    assert_eq!(
        find(5, None),
        Some((2, Exact)),
        "exact on the left, though the right one is within reach"
    );
    assert_eq!(find(9, None), Some((6, Exact)));
    assert_eq!(
        find(6, None),
        Some((6, Exact)),
        "the caret at the word's START"
    );
    assert_eq!(find(3, Some(10)), Some((2, Inside)), "inside beats slack");
    assert_eq!(find(8, None), Some((6, Inside)), "and the other way round");
    assert_eq!(find(20, Some(10)), None, "out of reach");

    let mut g = Glass::new(Path::Cold, 2, 40);
    g.feed("$ x sit");
    let row = g.row(0);
    let find = |cursor_col: Option<u16>| {
        find_in_row(&row, &['s', 'i', 't'], 2, 0, cursor_col, false)
            .map(|hit| (hit.cols[0], hit.anchor))
    };
    assert_eq!(find(Some(7)), Some((4, Slack)), "the burst's own echo");
    assert_eq!(find(Some(9)), Some((4, Slack)), "and a wide boundary key");
    assert_eq!(find(Some(10)), None, "the cursor ran on past the tail");
    assert_eq!(find(Some(6)), None, "the cursor has not passed the word");
    assert_eq!(find(None), None, "the cursor is on another row");
}

#[test]
fn a_burst_flashes_the_copy_that_fired_not_the_rightmost() {
    for path in BOTH {
        let t0 = Instant::now();
        let mut g = Glass::new(path, 4, 40);
        g.echo("$ s", t0);
        g.wd.note_trick_typed(t0, None, "sit", 0);
        g.echo("it sit ", t0);
        assert_eq!(
            cells_at(&g.frame(t0 + ms(300)).0),
            [(0, 2), (0, 3), (0, 4)],
            "{path:?}"
        );
    }
}

#[test]
fn output_the_caret_only_reaches_never_flashes_under_an_unreported_scroll() {
    for path in BOTH {
        let t0 = Instant::now();
        let mut g = Glass::new(path, 6, 40).unwired();
        g.echo("\x1b[2;5r\x1b[5;1H$ sit", t0);
        g.fire("sit", 0, "\r\nbash: sit: command not found", t0);
        assert_eq!(g.caret().0, 4, "the error is on the caret's row");
        assert!(g.frame(t0 + ms(300)).0.is_empty(), "{path:?}: output");
        g.echo("\r\n$ ", t0 + ms(320));
        assert_eq!(
            cells_at(&g.frame(t0 + ms(600)).0),
            [(2, 2), (2, 3), (2, 4)],
            "{path:?}: the typed line"
        );
    }
}

#[test]
fn a_held_address_word_is_found_behind_the_pets_name_by_back_chars() {
    for path in BOTH {
        let t0 = Instant::now();
        let mut g = Glass::new(path, 4, 40);
        g.echo("$ good kitty", t0);
        g.fire("good", 6, " ", t0);
        let (ink, _) = g.frame(t0 + ms(500));
        let flashed: Vec<(u16, u16)> = cells_at(&ink)
            .into_iter()
            .filter(|(_, col)| *col < 6)
            .collect();
        assert_eq!(flashed, [(0, 2), (0, 3), (0, 4), (0, 5)], "{path:?}");
        assert!(sorted_unique(&ink));

        let mut adjacent_only = Glass::new(path, 4, 40);
        adjacent_only.echo("$ good kitty", t0);
        adjacent_only.fire("good", 0, " ", t0);
        let (ink, _) = adjacent_only.frame(t0 + ms(500));
        assert!(
            cells_at(&ink).iter().all(|(_, col)| *col >= 7),
            "{path:?}: non-vacuous — back_chars is what reaches `good`"
        );
    }
}

#[test]
fn pane_b_never_shows_pane_as_flash() {
    let t0 = Instant::now();
    let (lex, cfg) = (Lexicon::with_languages(&["en"]), DecoConfig::default());
    let mut a = Terminal::new(4, 40);
    let mut b = Terminal::new(4, 40);
    a.process(b"$ sit");
    b.process(b"$ sit");
    let mut c = Terminal::new(4, 40);
    c.process(b"$$$ sit ");
    let mut wd = WordDecorations::default();
    let tick = |wd: &mut WordDecorations, now: Instant| {
        let (mut out, mut ink, mut free, mut nova) = (vec![], vec![], vec![], vec![]);
        let geom = EffectGeom::default();
        let fp = wd.tick(
            now, &cfg, geom, None, None, true, &mut out, &mut ink, &mut free, &mut nova,
        );
        (ink, fp)
    };
    wd.bind_pane(1, (0, 0));
    wd.rescan(&a, 4, 40, &lex, &cfg, 1, t0);
    wd.bind_pane(2, (400, 0));
    wd.rescan(&b, 4, 40, &lex, &cfg, 1, t0);
    wd.note_trick_typed(t0, Some(1), "sit", 0);
    a.process(b" ");
    b.process(b" ");
    wd.bind_pane(3, (800, 0));
    wd.rescan(&c, 4, 40, &lex, &cfg, 1, t0);
    assert_eq!(wd.trick_flash_phase(t0), TrickFlashPhase::Pending);
    assert!(tick(&mut wd, t0).0.is_empty());
    wd.bind_pane(1, (0, 0));
    wd.rescan(&a, 4, 40, &lex, &cfg, 2, t0);
    let (ink_a, _) = tick(&mut wd, t0 + ms(300));
    assert_eq!(cells_at(&ink_a), [(0, 2), (0, 3), (0, 4)]);
    wd.bind_pane(2, (400, 0));
    wd.rescan(&b, 4, 40, &lex, &cfg, 2, t0);
    let (ink_b, fp_b) = tick(&mut wd, t0 + ms(300));
    assert!(
        ink_b.is_empty() && fp_b == 0,
        "pane B's tick is the pre-feature path"
    );
    assert_eq!(
        wd.trick_flash_phase(t0 + ms(300)),
        TrickFlashPhase::Live,
        "pane B's rescan must not collect pane A's flash either"
    );
    wd.bind_pane(1, (0, 0));
    assert_eq!(tick(&mut wd, t0 + ms(316)).0.len(), 3);
    wd.bind_pane(2, (400, 0));
    assert!(wd.is_active(t0 + ms(316)));
    assert!(!wd.is_active(t0 + ms(u64::from(FLASH_MS))));
}

#[test]
fn closing_the_pane_takes_its_flash_with_it() {
    let t0 = Instant::now();
    let (lex, cfg) = (Lexicon::with_languages(&["en"]), DecoConfig::default());
    for (closed, left) in [(1u64, TrickFlashPhase::Idle), (2, TrickFlashPhase::Live)] {
        let mut a = Terminal::new(4, 40);
        a.process(b"$ sit");
        let mut wd = WordDecorations::default();
        wd.bind_pane(1, (0, 0));
        wd.rescan(&a, 4, 40, &lex, &cfg, 1, t0);
        wd.note_trick_typed(t0, Some(1), "sit", 0);
        a.process(b" ");
        wd.rescan(&a, 4, 40, &lex, &cfg, 2, t0);
        wd.bind_pane(2, (400, 0));
        assert_eq!(wd.trick_flash_phase(t0), TrickFlashPhase::Live);
        wd.retain_panes(|pane| pane != closed);
        assert_eq!(wd.trick_flash_phase(t0), left, "pane {closed} closed");
        wd.bind_pane(1, (0, 0));
        assert_eq!(wd.trick_flash_phase(t0), left, "pane {closed}: re-bound");
        if closed == 2 {
            wd.rescan(&a, 4, 40, &lex, &cfg, 3, t0);
            let (mut out, mut ink, mut free, mut nova) = (vec![], vec![], vec![], vec![]);
            let geom = EffectGeom::default();
            let t = t0 + ms(300);
            wd.tick(
                t, &cfg, geom, None, None, true, &mut out, &mut ink, &mut free, &mut nova,
            );
            assert_eq!(cells_at(&ink), [(0, 2), (0, 3), (0, 4)], "and it paints");
        }
    }
}

#[test]
fn a_flash_that_names_no_pane_dies_with_the_window_wide_grid() {
    let t0 = Instant::now();
    let (lex, cfg) = (Lexicon::with_languages(&["en"]), DecoConfig::default());
    let mut a = Terminal::new(4, 40);
    a.process(b"$ sit");
    let mut wd = WordDecorations::default();
    wd.rescan(&a, 4, 40, &lex, &cfg, 1, t0);
    wd.note_trick_typed(t0, None, "sit", 0);
    a.process(b" ");
    wd.rescan(&a, 4, 40, &lex, &cfg, 2, t0);
    assert_eq!(wd.trick_flash_phase(t0), TrickFlashPhase::Live);
    wd.bind_pane(1, (0, 0));
    assert_eq!(wd.trick_flash_phase(t0), TrickFlashPhase::Idle);

    let mut a = Terminal::new(4, 40);
    a.process(b"$ sit");
    let mut wd = WordDecorations::default();
    wd.set_scan_session(Some(1));
    wd.rescan(&a, 4, 40, &lex, &cfg, 1, t0);
    wd.note_trick_typed(t0, Some(1), "sit", 0);
    assert_eq!(wd.trick_flash_phase(t0), TrickFlashPhase::Pending);
    wd.bind_pane(1, (0, 0));
    assert_eq!(wd.trick_flash_phase(t0), TrickFlashPhase::Idle, "unfound");
}

#[test]
fn a_note_that_names_no_pane_reads_the_scan_sessions_row_origin() {
    let prompt = format!("{}$ ", "~".repeat(22));
    for path in BOTH {
        let t0 = Instant::now();
        let mut g = Glass::new(path, 4, 40);
        g.wd.set_scan_session(Some(10));
        g.echo(&format!("one\r\ntwo\r\nthree\r\n{prompt}sit"), t0);
        g.fire(
            "sit",
            0,
            &format!("\r\nzsh: command not found: sit\r\n{prompt}"),
            t0,
        );
        assert_eq!(
            cells_at(&g.frame(t0 + ms(300)).0),
            [(1, 24), (1, 25), (1, 26)],
            "{path:?}: the typed line, not the shell's"
        );
    }
}

#[test]
fn a_rescan_without_a_cursor_ends_the_flash() {
    let t0 = Instant::now();
    for pending in [false, true] {
        let mut g = Glass::new(Path::Snapshot, 4, 40);
        g.echo("$ sit", t0);
        g.wd.note_trick_typed(t0, None, "sit", 0);
        if !pending {
            g.echo(" ", t0);
            assert_eq!(g.frame(t0 + ms(100)).0.len(), 3);
        }
        g.scan_with_cursor(t0 + ms(110), false);
        assert_eq!(g.wd.trick_flash_phase(t0 + ms(110)), TrickFlashPhase::Idle);
        let (ink, fp) = g.frame(t0 + ms(116));
        assert!(ink.is_empty() && fp == 0, "pending = {pending}");
        g.scan(t0 + ms(120));
        assert!(g.frame(t0 + ms(130)).0.is_empty(), "…and it stays ended");
    }
    let mut g = Glass::new(Path::Cold, 4, 40);
    g.echo("a\r\nb\r\nc\r\nd\r\n$ sit", t0);
    g.fire("sit", 0, " ", t0);
    assert_eq!(g.frame(t0 + ms(100)).0.len(), 3);
    g.term.scroll_display(1);
    assert_ne!(g.term.grid().display_offset(), 0, "scrolled back");
    g.scan(t0 + ms(110));
    assert!(g.frame(t0 + ms(116)).0.is_empty());
    assert_eq!(g.wd.trick_flash_phase(t0 + ms(116)), TrickFlashPhase::Idle);
}

#[test]
fn a_word_not_found_within_the_echo_window_is_given_up_unpainted() {
    for path in BOTH {
        for (late, found) in [(GIVE_UP_MS, true), (GIVE_UP_MS + 1, false)] {
            let t0 = Instant::now();
            let mut g = Glass::new(path, 4, 40);
            g.echo("$ sit", t0);
            g.wd.note_trick_typed(t0, None, "sit", 0);
            let then = t0 + ms(late);
            assert_eq!(
                g.wd.trick_flash_phase(then) == TrickFlashPhase::Pending,
                found,
                "{path:?}: the phase is a pure function of the clock"
            );
            g.echo(" ", then);
            assert_eq!(
                !g.frame(then + ms(200)).0.is_empty(),
                found,
                "{path:?}: echo after {late} ms"
            );
        }
    }
}

#[test]
fn a_late_echo_still_ramps_in_from_the_base() {
    let t0 = Instant::now();
    let mut g = Glass::new(Path::Cold, 4, 40);
    g.echo("$ sit", t0);
    g.wd.note_trick_typed(t0, None, "sit", 0);
    let echoed = t0 + ms(400);
    g.echo(" ", echoed);
    let own = g.row(0)[2].fg;
    let (ink, _) = g.frame(echoed);
    assert!(ink.iter().all(|cell| cell.color == own) && ink.len() == 3);
    let end = echoed + ms(u64::from(FLASH_MS));
    assert!(!g.frame(end - ms(1)).0.is_empty());
    assert!(g.frame(end).0.is_empty());
    let mut g = Glass::new(Path::Cold, 4, 40);
    g.echo("$ sit", t0);
    g.wd.note_trick_typed(t0 + ms(50), None, "sit", 0);
    g.echo(" ", t0);
    assert_eq!(g.frame(t0 + ms(50)).0[0].color, own, "since = the key time");
}

#[test]
fn a_revoke_fast_fades_a_live_flash_and_drops_an_unfound_one() {
    let t0 = Instant::now();
    let mut g = Glass::new(Path::Cold, 4, 40);
    g.wd.revoke_trick_flash(t0); // nothing pending: a no-op
    g.echo("$ sit", t0);
    g.fire("sit", 0, " ", t0);
    let own = g.row(0)[2].fg;
    let before = g.frame(t0 + ms(400)).0;
    g.wd.revoke_trick_flash(t0 + ms(400));
    assert_eq!(
        g.wd.trick_flash_phase(t0 + ms(400)),
        TrickFlashPhase::Revoked
    );
    assert_eq!(
        g.frame(t0 + ms(400)).0,
        before,
        "continuous at the revoke: no pop"
    );
    let dist = |ink: &[InkCell]| -> u32 {
        ink.iter()
            .flat_map(|cell| cell.color.into_iter().zip(own))
            .map(|(a, b)| u32::from(a.abs_diff(b)))
            .sum()
    };
    let mid = g.frame(t0 + ms(460)).0;
    assert!(
        dist(&mid) < dist(&before) && dist(&mid) > 0,
        "half-way through the fast fade the tint is going, not gone"
    );
    g.wd.revoke_trick_flash(t0 + ms(500));
    let gone = t0 + ms(400 + u64::from(REVOKE_FADE_MS));
    assert!(!g.frame(gone - ms(1)).0.is_empty());
    let (ink, fp) = g.frame(gone);
    assert!(ink.is_empty() && fp == 0);
    assert!(
        !g.wd.is_active(gone),
        "the deadline came in with the revoke"
    );
    assert_eq!(g.wd.trick_flash_phase(gone), TrickFlashPhase::Idle);

    let mut g = Glass::new(Path::Cold, 4, 40);
    g.echo("$ sit", t0);
    g.wd.note_trick_typed(t0, None, "sit", 0);
    g.wd.revoke_trick_flash(t0 + ms(5));
    assert_eq!(g.wd.trick_flash_phase(t0 + ms(5)), TrickFlashPhase::Idle);
    g.echo(" ", t0 + ms(10));
    assert!(g.frame(t0 + ms(300)).0.is_empty());
}

#[test]
fn reduced_motion_and_ink_off_emit_nothing_and_never_arm() {
    let gates: [fn(&mut DecoConfig); 2] = [
        |cfg| cfg.reduced_motion = true,
        |cfg| cfg.ink_enabled = false,
    ];
    for path in BOTH {
        for gate in gates {
            let t0 = Instant::now();
            let mut g = Glass::new(path, 4, 40);
            gate(&mut g.cfg);
            g.echo("$ sit", t0);
            g.fire("sit", 0, " ", t0);
            assert_eq!(g.wd.trick_flash_phase(t0), TrickFlashPhase::Idle);
            let (ink, fp) = g.frame(t0 + ms(300));
            assert!(ink.is_empty() && fp == 0, "{path:?}");
            assert!(!g.wd.is_active(t0), "{path:?}: never armed");

            let mut g = Glass::new(path, 4, 40);
            g.echo("$ sit", t0);
            g.fire("sit", 0, " ", t0);
            assert_eq!(g.frame(t0 + ms(300)).0.len(), 3);
            gate(&mut g.cfg);
            let (ink, fp) = g.frame(t0 + ms(316));
            assert!(ink.is_empty() && fp == 0, "{path:?}");
            assert!(!g.wd.is_active(t0 + ms(316)), "{path:?}: disarmed at once");
            assert_eq!(g.wd.trick_flash_phase(t0 + ms(316)), TrickFlashPhase::Idle);
        }
    }
}

#[test]
fn thai_and_hindi_words_are_found_under_their_combining_marks() {
    for path in BOTH {
        for word in ["นั่ง", "बैठ"] {
            let t0 = Instant::now();
            let mut g = Glass::new(path, 4, 40);
            g.echo(&format!("$ {word}"), t0);
            assert_eq!(g.caret(), (0, 4), "{word}: two cells on the glass");
            g.fire(word, 0, " ", t0);
            assert_eq!(
                cells_at(&g.frame(t0 + ms(300)).0),
                [(0, 2), (0, 3)],
                "{path:?}: {word}"
            );
        }
    }
}

#[test]
fn a_hebrew_word_is_found_in_visual_order_and_in_logical_order() {
    for path in BOTH {
        for on_glass in ["שב", "בש"] {
            let t0 = Instant::now();
            let mut g = Glass::new(path, 4, 40);
            g.echo(&format!("$ {on_glass}"), t0);
            g.fire("שב", 0, " ", t0);
            assert_eq!(
                cells_at(&g.frame(t0 + ms(300)).0),
                [(0, 2), (0, 3)],
                "{path:?}: typed שב, the glass shows {on_glass}"
            );
            g.scan(t0 + ms(310));
            assert_eq!(g.frame(t0 + ms(320)).0.len(), 2, "{path:?}");
        }
        let t0 = Instant::now();
        let mut g = Glass::new(path, 4, 40);
        g.echo("$ tis", t0);
        g.fire("sit", 0, " ", t0);
        assert!(
            g.frame(t0 + ms(300)).0.is_empty(),
            "{path:?}: `tis` is not `sit`"
        );
    }
}

#[test]
fn a_russian_word_is_found_under_ambiguous_double_width() {
    for path in BOTH {
        let t0 = Instant::now();
        let mut g = Glass::new(path, 4, 40);
        g.term.apply_config(&aterm_core::config::TerminalConfig {
            ambiguous_width_double: true,
            ..Default::default()
        });
        g.echo("$ сидеть", t0);
        assert_eq!(g.caret(), (0, 14), "six letters, twelve cells");
        g.fire("сидеть", 0, " ", t0);
        assert_eq!(
            cells_at(&g.frame(t0 + ms(300)).0),
            [(0, 2), (0, 4), (0, 6), (0, 8), (0, 10), (0, 12)],
            "{path:?}: LEAD cells only"
        );
    }
}

#[test]
fn a_japanese_word_is_found_on_its_lead_cells_inside_one_ime_commit() {
    for path in BOTH {
        let t0 = Instant::now();
        let mut g = Glass::new(path, 4, 40);
        g.echo("$ ", t0);
        assert_eq!(g.caret(), (0, 2));
        g.fire("おすわり", 3, "ねこ、おすわり！", t0);
        let flashed: Vec<(u16, u16)> = cells_at(&g.frame(t0 + ms(300)).0)
            .into_iter()
            .filter(|(_, col)| *col >= 6)
            .collect();
        assert_eq!(
            flashed,
            [(0, 8), (0, 10), (0, 12), (0, 14)],
            "{path:?}: lead cells of the four wide glyphs"
        );
    }
}

#[test]
fn a_piece_of_a_longer_word_is_never_the_typed_word() {
    for path in BOTH {
        for (typed, rest) in [("$ sit", "e "), ("$ visit", " ")] {
            let t0 = Instant::now();
            let mut g = Glass::new(path, 4, 40);
            g.echo(typed, t0);
            g.fire("sit", 0, rest, t0);
            assert!(
                g.frame(t0 + ms(300)).0.is_empty(),
                "{path:?}: {typed}{rest}"
            );
            assert_eq!(g.wd.trick_flash_phase(t0), TrickFlashPhase::Pending);
        }
    }
}

#[test]
fn the_merge_keeps_ink_sorted_unique_over_an_overlapping_self_glow() {
    let t0 = Instant::now();
    let script = |fire: bool| {
        let mut g = Glass::new(Path::Cold, 4, 40);
        g.echo("a kitten naps\r\n$ \x1b[38;2;60;120;200mmeow", t0);
        if fire {
            g.wd.note_trick_typed(t0, None, "meow", 0);
        }
        g.echo(" ", t0);
        g
    };
    let (mut plain, mut flashed) = (script(false), script(true));
    let caret = (1u16, 6u16);
    for t in [350u64, 500, 1000, 1099] {
        let (glow, _) = plain.frame(t0 + ms(t));
        let (ink, _) = flashed.frame(t0 + ms(t));
        assert!(sorted_unique(&ink), "t = {t}");
        assert_eq!(
            cells_at(&ink),
            cells_at(&glow),
            "t = {t}: the flash added no second cell to an inked glyph"
        );
        assert_eq!(glow.len(), 6 + 4, "kitten + meow, both glowing");
        for (i, (got, base)) in ink.iter().zip(&glow).skip(6).enumerate() {
            assert_ne!(base.color, [60, 120, 200], "the glow really moved it");
            assert_eq!(
                got.color,
                law(
                    caret,
                    "meow",
                    i,
                    t,
                    base.color,
                    [60, 120, 200],
                    plain.row(1)[2].bg
                ),
                "t = {t}, cell {i}: the base is the GLOW's colour"
            );
        }
        assert_eq!(ink[..6], glow[..6], "t = {t}");
    }
}

#[test]
fn the_flash_inserts_between_other_rows_ink_in_order() {
    let t0 = Instant::now();
    let mut g = Glass::new(Path::Cold, 4, 40);
    g.echo("kitten\r\n\r\nkitty\x1b[2;1H$ sit", t0);
    assert_eq!(g.caret(), (1, 5));
    g.fire("sit", 0, " ", t0);
    let (ink, _) = g.frame(t0 + ms(350));
    assert!(sorted_unique(&ink));
    let rows: Vec<u16> = ink.iter().map(|cell| cell.row).collect();
    assert_eq!(rows, [0, 0, 0, 0, 0, 0, 1, 1, 1, 2, 2, 2, 2, 2]);
}

#[test]
fn a_second_flash_inside_the_opening_window_is_skipped_and_a_later_one_replaces() {
    let t0 = Instant::now();
    let mut g = Glass::new(Path::Cold, 4, 40);
    g.echo("$ sit", t0);
    g.fire("sit", 0, " ", t0);
    let early = t0 + ms(u64::from(MIN_ANIMATION_MS) - 1);
    g.echo("jump", early);
    g.fire("jump", 0, " ", early);
    assert_eq!(
        cells_at(&g.frame(early + ms(10)).0),
        [(0, 2), (0, 3), (0, 4)],
        "still `sit`: the second note was skipped"
    );
    let later = t0 + ms(u64::from(MIN_ANIMATION_MS));
    g.echo("play", later);
    g.fire("play", 0, " ", later);
    assert_eq!(
        cells_at(&g.frame(later + ms(200)).0),
        [(0, 11), (0, 12), (0, 13), (0, 14)],
        "`play` replaced it"
    );
    g.wd.revoke_trick_flash(later + ms(10));
    let gone = later + ms(10 + u64::from(REVOKE_FADE_MS));
    assert!(g.frame(gone).0.is_empty());
    g.echo("roll", gone);
    g.fire("roll", 0, " ", gone);
    assert!(
        g.frame(gone + ms(50)).0.is_empty(),
        "inside `play`'s opening window"
    );
}

#[test]
fn a_flash_follows_its_line_as_output_scrolls_it() {
    let prompt = format!("{}$ ", "~".repeat(22));
    for path in BOTH {
        for wired in [true, false] {
            let t0 = Instant::now();
            let mut g = Glass::new(path, 4, 40);
            g.wired = wired;
            g.echo(&format!("one\r\ntwo\r\nthree\r\n{prompt}sit"), t0);
            g.fire("sit", 0, " ", t0);
            assert_eq!(cells_at(&g.frame(t0 + ms(100)).0)[0], (3, 24));
            g.echo(
                &format!("\r\nzsh: command not found: sit\r\n{prompt}"),
                t0 + ms(200),
            );
            let (ink, _) = g.frame(t0 + ms(300));
            if wired {
                assert_eq!(
                    cells_at(&ink),
                    [(1, 24), (1, 25), (1, 26)],
                    "{path:?}: followed two rows up"
                );
            } else {
                assert!(ink.is_empty(), "{path:?}: ended, never re-searched");
            }
        }
    }
}

#[test]
fn a_flash_ends_when_its_text_is_erased_or_the_grid_changes_width() {
    for path in BOTH {
        let t0 = Instant::now();
        let mut g = Glass::new(path, 4, 40);
        g.echo("$ sit", t0);
        g.fire("sit", 0, " ", t0);
        assert_eq!(g.frame(t0 + ms(100)).0.len(), 3);
        g.echo("\r\x1b[K$ ", t0 + ms(110));
        let (ink, fp) = g.frame(t0 + ms(116));
        assert!(ink.is_empty() && fp == 0, "{path:?}: text gone");
        assert!(!g.wd.is_active(t0 + ms(116)));

        let mut g = Glass::new(path, 4, 40);
        g.echo("$ sit", t0);
        g.fire("sit", 0, " ", t0);
        assert_eq!(g.frame(t0 + ms(100)).0.len(), 3);
        g.term.resize(4, 50);
        g.cols = 50;
        g.scan(t0 + ms(110));
        assert!(
            g.frame(t0 + ms(116)).0.is_empty(),
            "{path:?}: width changed"
        );
    }
}

#[test]
fn a_pane_with_no_caret_observation_stores_no_flash() {
    let t0 = Instant::now();
    let mut wd = WordDecorations::default();
    wd.note_trick_typed(t0, None, "sit", 0);
    assert_eq!(wd.trick_flash_phase(t0), TrickFlashPhase::Idle);
    let mut g = Glass::new(Path::Cold, 4, 40);
    g.echo("$ sit", t0);
    g.wd.note_trick_typed(t0, Some(77), "sit", 0);
    assert_eq!(g.wd.trick_flash_phase(t0), TrickFlashPhase::Pending);
    g.wd.bind_pane(5, (0, 0));
    g.wd.revoke_trick_flash(t0);
    g.wd.note_trick_typed(t0, Some(99), "sit", 0);
    assert_eq!(
        g.wd.trick_flash_phase(t0),
        TrickFlashPhase::Idle,
        "pane 99 was never scanned"
    );
    let mut g = Glass::new(Path::Cold, 4, 40);
    g.echo("$ ", t0);
    for word in ["", "\u{200d}\u{fe0f}", &"x".repeat(MAX_CHARS + 1)] {
        g.wd.note_trick_typed(t0, None, word, 0);
        assert_eq!(
            g.wd.trick_flash_phase(t0),
            TrickFlashPhase::Idle,
            "{word:?}"
        );
    }
    g.wd.note_trick_typed(t0, None, &"x".repeat(MAX_CHARS), 0);
    assert_eq!(g.wd.trick_flash_phase(t0), TrickFlashPhase::Pending);
}

#[test]
fn the_base_y_memo_is_bounded_and_pruned_with_the_panes() {
    let t0 = Instant::now();
    let mut flash = TrickFlash::default();
    let mut buf = Vec::new();
    let mut scan = |flash: &mut TrickFlash, pane: u64, base_y: Option<i64>, cursor| {
        flash.declare_base_y(Some(pane), base_y);
        flash.on_rescan(Some(pane), t0, 4, 40, cursor, true, &mut buf, |_, row| {
            row.clear();
        });
    };
    for pane in 0..40u64 {
        scan(&mut flash, pane, Some(pane as i64), Some((0, 0)));
    }
    assert_eq!(flash.base_y_slice().len(), BASE_Y_MEMO_CAP);
    assert_eq!(
        flash.base_y_slice()[0],
        (Some(24), 24),
        "oldest-first eviction"
    );
    scan(&mut flash, 39, Some(1000), None);
    assert!(flash.base_y_slice().contains(&(Some(39), 39)));
    scan(&mut flash, 39, Some(41), Some((0, 0)));
    assert!(flash.base_y_slice().contains(&(Some(39), 41)));
    scan(&mut flash, 39, None, Some((0, 0)));
    assert!(
        flash
            .base_y_slice()
            .iter()
            .all(|(pane, _)| *pane != Some(39))
    );
    flash.declare_base_y(Some(30), Some(7));
    flash.on_rescan(
        Some(31),
        t0,
        4,
        40,
        Some((0, 0)),
        true,
        &mut buf,
        |_, row| {
            row.clear();
        },
    );
    assert!(
        flash
            .base_y_slice()
            .iter()
            .all(|(pane, _)| *pane != Some(31))
    );

    flash.note(t0, Some(38), Some(38), Some((0, 0)), "sit", 0);
    assert_eq!(flash.slot.map(|slot| slot.base_y), Some(Some(38)));
    flash.retain_panes(|pane| pane < 30);
    assert!(
        flash
            .base_y_slice()
            .iter()
            .all(|(pane, _)| pane.is_some_and(|p| p < 30))
    );
    assert!(!flash.base_y_slice().is_empty());
    assert!(
        flash.slot.is_none(),
        "pane 38 is gone, and its flash with it"
    );
}

#[test]
fn the_legibility_guard_holds_the_contrast_bound_at_every_hue() {
    let contrast = |a: u32, b: u32| {
        let (la, lb) = (relative_luminance(a), relative_luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    };
    let themes: [(u32, u32); 7] = [
        (0x00E6_E6E6, 0x0000_0000), // light on black
        (0x00FF_FFFF, 0x001E_1E2E), // white on a dark blue
        (0x001E_1E1E, 0x00FA_FAFA), // dark on near-white
        (0x0000_0000, 0x00FF_FFFF), // black on white
        (0x0060_6060, 0x0000_0000), // dim comment grey: below the bound
        (0x00FF_5555, 0x0028_2A36), // an "unknown command" red
        (0x0080_8080, 0x0080_8080), // concealed: fg == bg
    ];
    for (fg, bg) in themes {
        let (sat, val) = resolve_sat_val(bg);
        let strength = legible_strength(fg, bg, sat, val);
        assert!((0.0..=FLASH_STRENGTH).contains(&strength));
        let floor = MIN_INK_CONTRAST.min(contrast(fg, bg));
        for hue in 0..360 {
            let ink = mix_rgb(fg, hsv2rgb(hue as f32, sat, val), strength);
            assert!(
                contrast(ink, bg) >= floor - 0.02,
                "fg {fg:06x} on {bg:06x} at {hue}°: {} < {floor}",
                contrast(ink, bg)
            );
        }
    }
    let (dark, light) = (themes[0], themes[2]);
    for (fg, bg) in [dark, light] {
        let (sat, val) = resolve_sat_val(bg);
        let strength = legible_strength(fg, bg, sat, val);
        assert!(
            strength > 0.0 && strength < FLASH_STRENGTH,
            "{fg:06x} on {bg:06x}: the guard must bind without erasing ({strength})"
        );
    }
    for (fg, bg) in [themes[4], themes[6]] {
        let (sat, val) = resolve_sat_val(bg);
        assert_eq!(
            legible_strength(fg, bg, sat, val),
            0.0,
            "{fg:06x} on {bg:06x}: below the bound already, so never tinted"
        );
    }
    for sgr in ["\x1b[38;2;96;96;96m\x1b[48;2;0;0;0m", "\x1b[8m"] {
        for path in BOTH {
            let t0 = Instant::now();
            let mut g = Glass::new(path, 4, 40);
            g.echo(&format!("$ {sgr}sit"), t0);
            g.fire("sit", 0, " ", t0);
            assert_eq!(g.wd.trick_flash_phase(t0), TrickFlashPhase::Idle);
            let (ink, fp) = g.frame(t0 + ms(300));
            assert!(ink.is_empty() && fp == 0, "{path:?}: {sgr:?}");
            assert!(!g.wd.is_active(t0), "{path:?}: never armed");
        }
    }
}

#[test]
fn the_flash_stays_under_the_photosensitivity_bound() {
    fn reversals(lums: &[f32]) -> u32 {
        let (mut dir, mut extreme, mut count) = (0i8, lums[0], 0u32);
        for &lum in lums {
            let moved = lum - extreme;
            if dir == 0 {
                if moved.abs() >= 0.1 {
                    dir = if moved > 0.0 { 1 } else { -1 };
                    extreme = lum;
                }
            } else if moved * f32::from(dir) > 0.0 {
                extreme = lum;
            } else if moved.abs() >= 0.1 {
                dir = -dir;
                extreme = lum;
                count += 1;
            }
        }
        count
    }
    let themes: [(u32, u32); 5] = [
        (0x00FF_FFFF, 0x0000_0000),
        (0x00E6_E6E6, 0x001E_1E2E),
        (0x0080_8080, 0x0000_0000),
        (0x0000_0000, 0x00FF_FFFF),
        (0x001E_1E1E, 0x00FA_FAFA),
    ];
    let mut worst = 0u32;
    for (fg, bg) in themes {
        let (sat, val) = resolve_sat_val(bg);
        for strength in [legible_strength(fg, bg, sat, val), FLASH_STRENGTH, 1.0] {
            for hue0 in (0..360).step_by(5) {
                for (i, n) in [(0usize, 1usize), (0, 4), (3, 4), (7, 8)] {
                    let lums: Vec<f32> = (0..=u64::from(FLASH_MS))
                        .map(|t| {
                            let target = hsv2rgb(flash_hue(hue0 as f32, i, n, t), sat, val);
                            relative_luminance(mix_rgb(fg, target, envelope(t, None) * strength))
                        })
                        .collect();
                    worst = worst.max(reversals(&lums));
                }
            }
        }
    }
    assert!(
        worst as f32 <= FLASH_REVERSALS,
        "measured {worst} large reversals in one flash against a declared {FLASH_REVERSALS}"
    );
    let hz = worst as f32 * 1000.0 / FLASH_MS as f32;
    assert!(
        hz <= TWINKLE_FLASH_BOUND_HZ,
        "the flash reverses luminance {hz} times a second — over the {TWINKLE_FLASH_BOUND_HZ} Hz bound"
    );
    assert!(
        worst >= 2,
        "non-vacuous: a hue revolution DOES cross the cliffs"
    );
    let (fg, bg) = themes[0];
    let (sat, val) = resolve_sat_val(bg);
    for revoked in [0u64, 60, 130, 400, 900] {
        let lums: Vec<f32> = (0..=revoked + u64::from(REVOKE_FADE_MS))
            .map(|t| {
                let target = hsv2rgb(flash_hue(240.0, 0, 1, t), sat, val);
                relative_luminance(mix_rgb(fg, target, envelope(t, Some(revoked))))
            })
            .collect();
        assert!(
            reversals(&lums) as f32 <= FLASH_REVERSALS,
            "revoked at {revoked}"
        );
        assert_eq!(
            envelope(revoked + u64::from(REVOKE_FADE_MS), Some(revoked)),
            0.0
        );
    }
}
