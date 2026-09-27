// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **CLAUDE CODE'S COMPOSER, THE SECOND WRAP AND AFTER** (2026-09-24) — the
//! owner, on aterm 0.93.0 with the rainbow kitty trail, typing a long prompt
//! into Claude Code: the band *"smashes the spectrum on the next line"*, and
//! a Space typed at the end of a full line *"skips a space"*.
//!
//! `tests/composer_box_growth_wrap.rs` pins the FIRST wrap of a composer
//! (Claude Code 2.1.278 at 90×30). What it cannot see is the second: the
//! first text row wraps under the rule, whose cells hold no letter, but the
//! second wraps under the first line of prose, and Ink's diff writes that
//! repaint SPARSELY — every cell where the second line's glyph equals the
//! first line's at the same column is skipped, because the glyph standing
//! there is already right. The follow pass read those coincidences as holes
//! (`Witness::follow_runs`, the twin law of 2026-09-22 — "an erase is not a
//! move"), refused the whole run, and the witness walk then melted the band
//! on the caret's row — the new, empty continuation row — in `RETIRE_MELT_S`
//! while the text it lit went dark a row up; the keys typed next joined that
//! stale cohort and the colours restarted.
//!
//! The bytes are Claude Code 2.1.282's at 91×53, captured on glass in an
//! isolated aterm 0.93.0 instance and under a Python pty
//! (`docs/measured/claude-code-composer-multiline-wrap-bytes-2026-09-24.md`,
//! the on-glass `run1`): the two wrap chunks of that run are quoted below
//! verbatim, and the first law proves this file's writers reproduce them byte
//! for byte before any other law uses them. The DEC 2026 bracket is the
//! window's own — the on-glass capture carries it.
//!
//! Every law runs at the HOST seam (`trail_host::Core`, LOCK A), as
//! `tests/composer_box_growth_wrap.rs` does: a real
//! `aterm_core::terminal::Terminal` driven byte for byte, its
//! rows sampled exactly as `app_render.rs`'s LOCK A samples them, fed through
//! `CursorGlow::observe_row` / `observe_ribbon_row` / `ribbon_rows` and ticked
//! through `CursorGlow::tick` at 16.7 ms, keys at the capture's 80 ms. Each
//! law says whether it was RED on `aa71f9319` (the tree the report was
//! reproduced against has no diff in these files from `73a9b4240`, the 0.93.0
//! the owner runs).

#[macro_use]
mod trail_host;

use aterm_effects::rainbow_kitty::TypedClass;
use aterm_effects::rainbow_kitty::ribbon::{RETIRE_MELT_S, RETRACT_DUR_S, RETRACT_FADE_S};
use std::time::{Duration, Instant};
use trail_host::{Core, LOCK_A, Theme};

/// The capture's window: 91 columns, 53 rows, the retina cell 15×28 device
/// px.
const ROWS: usize = 53;
const COLS: usize = 91;
const CW: usize = 15;
const CH: usize = 28;

/// The frame train and the capture's cadence (`KEY_S = 0.08`).
const FRAME_US: u64 = 16_667;
const KEY_MS: u64 = 80;

/// Claude Code 2.1.282's box before it grows, 1-based as the escapes carry
/// them: the top rule on 50, the text row on 51, the bottom rule on 52 and
/// the status on 53. The box grows UPWARD: a wrap repaints the top rule and
/// every text row one row higher and leaves the bottom rule where it is.
const TOP_RULE_ROW: usize = 50;
const TEXT_ROW: usize = 51;
const BOTTOM_RULE_ROW: usize = 52;
const STATUS_ROW: usize = 53;
/// …and 0-based, as the engine reads them: the caret's row, and the row the
/// text on it moves to at a wrap.
const CARET_ROW: u16 = (TEXT_ROW - 1) as u16;
const UP_ONE: u16 = CARET_ROW - 1;

const SYNC_BEGIN: &str = "\x1b[?2026h";
const SYNC_END: &str = "\x1b[?2026l";
/// The rule's pen and the text's, as the capture sets them.
const RULE_PEN: &str = "\x1b[38;2;136;136;136m";
const TEXT_PEN: &str = "\x1b[39m";
/// The prompt marker and the NO-BREAK SPACE after it, columns 1–2 of the
/// first text row; a continuation row holds two blanks there instead.
const PROMPT: &str = "\u{276f}\u{a0}";

/// The on-glass `run1`'s first text row: 86 glyphs on 1-based columns
/// 3..=88. The Space after it FITS (the caret goes to 90) and the `k` of the
/// next word wraps.
const LINE1: &str =
    "the quick brown fox jumps over the lazy dog while the rainbow kitty trail follows each";
/// `run1`'s second text row: 87 glyphs on 1-based columns 3..=89, its first
/// letter the `k` that wrapped. It shares five letters with `LINE1` at the
/// same column (`o` `e` `r` `h` `l`, 0-based columns 28, 30, 31, 47, 79) —
/// the cells the capture's sparse repaint skips. The Space after it wraps.
const LINE2: &str =
    "key we type into this composer so we can watch exactly what happens when the line wraps";
/// The keys typed on the third row.
const AFTER: &str = "and then";

/// `run1` key 87, the `k` that wraps the first line, VERBATIM from the
/// on-glass byte log (458 bytes; the rule is 91 × U+2500).
fn captured_first_wrap() -> String {
    format!(
        "\x1b[?2026h\x1b[?25l\x1b[H\r\x1b[48B\x1b[38;2;136;136;136m{}\r\x1b[1B\x1b[39m\u{276f}\u{a0}the quick brown fox jumps over the lazy dog while the rainbow kitty trail follows each\x1b[K\r\x1b[1B  k\x1b[K\x1b[53;1H\x1b[51;4H\x1b[?25h\x1b[?2026l",
        "\u{2500}".repeat(91)
    )
}

/// `run1` key 174, the Space that wraps the second line, VERBATIM from the
/// on-glass byte log (578 bytes). Row 50 is written SPARSELY: `ESC[7G`,
/// `ESC[30G`, `ESC[34G`, `ESC[49G`, `ESC[71G`, `ESC[81G`, `ESC[85G` step
/// over the cells where `LINE2` already matches the `LINE1` standing there.
fn captured_second_wrap() -> String {
    format!(
        "\x1b[?2026h\x1b[?25l\x1b[H\r\x1b[47B\x1b[38;2;136;136;136m{}\r\x1b[1B\x1b[39m\u{276f}\u{a0}the quick brown fox jumps over the lazy dog while the rainbow kitty trail follows each\x1b[K\r\x1b[1B  key\x1b[7Gwe type into this comp\x1b[30Gs\x1b[34Gso we can watc\x1b[49G exactly what happens\x1b[71Gwhen the \x1b[81Gine\x1b[85Gwraps\r\x1b[2C\x1b[1B\x1b[K\x1b[53;1H\x1b[51;3H\x1b[?25h\x1b[?2026l",
        "\u{2500}".repeat(91)
    )
}

/// 2.1.282's rule: 91 cells of U+2500, no label.
fn rule() -> String {
    "\u{2500}".repeat(COLS)
}

/// A LABELLED rule in 2.1.278's shape (`docs/measured/claude-code-composer-
/// wrap-bytes-2026-09-21.md`: 78 cells of rule, `ESC[80G`, the label in its
/// own pen, the rule's last cell at the margin; the cells between are the
/// blanks Ink's diff steps over). Not captured at 91 columns: the label sits
/// on 1-based columns 80.. as it does at 90, the last cell on 91.
fn labelled_rule(label: &str) -> String {
    format!(
        "{}\x1b[80G\x1b[38;2;175;135;255m{label}\x1b[{COLS}G{RULE_PEN}\u{2500}",
        "\u{2500}".repeat(78)
    )
}

/// One ordinary key's echo, as captured: the glyph `ch` on 1-based column
/// `c` of the text row, addressed from home, the caret put after it.
fn glyph_bytes(ch: char, c: usize) -> Vec<u8> {
    format!(
        "{SYNC_BEGIN}\x1b[?25l\x1b[H\r\x1b[{}C\x1b[{}B{ch}\x1b[{STATUS_ROW};1H\x1b[{TEXT_ROW};{}H\x1b[?25h{SYNC_END}",
        c - 1,
        TEXT_ROW - 1,
        c + 1
    )
    .into_bytes()
}

/// A Space's echo when it fits: the caret moves and nothing is written.
fn space_bytes(c: usize) -> Vec<u8> {
    format!(
        "{SYNC_BEGIN}\x1b[?25l\x1b[{TEXT_ROW};{}H\x1b[?25h{SYNC_END}",
        c + 1
    )
    .into_bytes()
}

/// **INK'S DIFF OF ONE ROW**, as the capture writes it: `CR`, a `CUF` over
/// the leading cells that already match, the step down to the row, then the
/// row's differing cells — a run of matching cells stepped over with `CHA`,
/// a blank tail (where the old row still had ink) cut with `EL`.
fn ink_row(old: &str, new: &str) -> String {
    let old: Vec<char> = old.chars().collect();
    let new: Vec<char> = new.chars().collect();
    let n = old.len().max(new.len());
    let at = |v: &[char], i: usize| v.get(i).copied().unwrap_or(' ');
    let lead = (0..n).take_while(|&i| at(&old, i) == at(&new, i)).count();
    let mut s = String::from("\r");
    if lead > 0 {
        s.push_str(&format!("\x1b[{lead}C"));
    }
    s.push_str("\x1b[1B");
    let mut cur = lead;
    for col in lead..n {
        if (col..n).all(|i| at(&new, i) == ' ') {
            if (col..n).any(|i| at(&old, i) != ' ') {
                if cur != col {
                    s.push_str(&format!("\x1b[{}G", col + 1));
                }
                s.push_str("\x1b[K");
            }
            break;
        }
        if at(&old, col) == at(&new, col) {
            continue;
        }
        if cur != col {
            s.push_str(&format!("\x1b[{}G", col + 1));
        }
        s.push(at(&new, col));
        cur = col + 1;
    }
    s
}

/// **THE FIRST WRAP**: the box one row higher under `rule`, the first text
/// row `line1` on row 50 where the rule stood — written whole, as a rule of
/// U+2500 shares no cell with text — and the continuation row diffed from
/// what the text row held (`typed`) to the moved word `moved`; the caret
/// after the moved word.
fn first_wrap_bytes(rule: &str, line1: &str, typed: &str, moved: &str) -> Vec<u8> {
    format!(
        "{SYNC_BEGIN}\x1b[?25l\x1b[H\r\x1b[{}B{RULE_PEN}{rule}\r\x1b[1B{TEXT_PEN}{PROMPT}{line1}\x1b[K{}\x1b[{STATUS_ROW};1H\x1b[{TEXT_ROW};{}H\x1b[?25h{SYNC_END}",
        TOP_RULE_ROW - 2,
        ink_row(&format!("{PROMPT}{typed}"), &format!("  {moved}")),
        3 + moved.chars().count()
    )
    .into_bytes()
}

/// **THE SECOND WRAP**: the box one more row higher, `line1` on row 49,
/// `line2` diffed onto row 50 over the `line1` standing there — the sparse
/// write — and the continuation row diffed from `line2` to `moved`.
fn second_wrap_bytes(line1: &str, line2: &str, moved: &str) -> Vec<u8> {
    format!(
        "{SYNC_BEGIN}\x1b[?25l\x1b[H\r\x1b[{}B{RULE_PEN}{}\r\x1b[1B{TEXT_PEN}{PROMPT}{line1}\x1b[K{}{}\x1b[{STATUS_ROW};1H\x1b[{TEXT_ROW};{}H\x1b[?25h{SYNC_END}",
        TOP_RULE_ROW - 3,
        rule(),
        ink_row(&format!("{PROMPT}{line1}"), &format!("  {line2}")),
        ink_row(&format!("  {line2}"), &format!("  {moved}")),
        3 + moved.chars().count()
    )
    .into_bytes()
}

/// The 0-based columns where `a` and `b`, both typed from 1-based column 3,
/// hold the same letter.
fn shared_letters(a: &str, b: &str) -> Vec<u16> {
    a.chars()
        .zip(b.chars())
        .enumerate()
        .filter(|&(_, (x, y))| x == y && x != ' ')
        .map(|(i, _)| i as u16 + 2)
        .collect()
}

/// The host: `trail_host`'s LOCK-A frame seam (every named row read, the
/// caret row's among them) on the owner's 53×91 grid of retina cells, the
/// default dark theme (fg `0xD0D0D0`, bg `0x111318`) at intensity 0.70 —
/// `tests/composer_box_growth_wrap.rs`'s setup — and a 16.667 ms frame
/// train.
struct Host {
    c: Core,
    next_frame: Instant,
    last_key: Instant,
    /// The 1-based column the next glyph lands on.
    col: usize,
}
core_deref!(Host);

impl Host {
    /// Claude Code's screen before the first key, as on glass: the alt
    /// screen, the top rule (`rule`) on row 50, the prompt on 51, the bottom
    /// rule on 52, the status on 53, the caret on (51, 3); one frame
    /// presented.
    fn new(rule: &str) -> Self {
        let mut c = Core::new(ROWS, COLS, CW, CH, Theme::Default, LOCK_A);
        c.term.process(b"\x1b[?1049h\x1b[2J\x1b[H");
        c.term.process(
            format!(
                "\x1b[{TOP_RULE_ROW};1H{RULE_PEN}{rule}\x1b[{TEXT_ROW};1H{TEXT_PEN}{PROMPT}\x1b[{BOTTOM_RULE_ROW};1H{RULE_PEN}{}{TEXT_PEN}\x1b[{STATUS_ROW};1H  \u{23f5}\u{23f5} auto mode on (shift+tab to cycle)\x1b[{TEXT_ROW};3H",
                "\u{2500}".repeat(COLS)
            )
            .as_bytes(),
        );
        let now = c.now;
        let mut h = Self {
            c,
            next_frame: now,
            last_key: now,
            col: 3,
        };
        h.frame();
        h
    }

    /// One frame of the seam, and the train's next one due a frame on.
    fn frame(&mut self) {
        self.c.frame();
        self.next_frame = self.now + Duration::from_micros(FRAME_US);
    }

    /// Run the frame train up to and including the last frame at or before
    /// `t`.
    fn advance_to(&mut self, t: Instant) {
        while self.next_frame <= t {
            self.now = self.next_frame;
            self.frame();
        }
        self.now = self.now.max(t);
    }

    /// Frames for `ms` more.
    fn idle(&mut self, ms: u64) {
        let t = self.now + Duration::from_millis(ms);
        self.advance_to(t);
    }

    /// One key, `KEY_MS` after the last: the frames until then run, the
    /// app_input seam arms the typed hint at the key, the PTY answers
    /// `bytes`, and the next frame of the train samples the echo.
    fn press(&mut self, ch: char, bytes: &[u8]) {
        let t = self.last_key + Duration::from_millis(KEY_MS);
        self.advance_to(t);
        self.now = t;
        self.last_key = t;
        let class = if ch == ' ' {
            TypedClass::Space
        } else {
            TypedClass::Glyph
        };
        self.glow.note_typed_glyph(t, 1, false, class);
        self.term.process(bytes);
    }

    /// An ordinary key on the text row: a glyph's echo or a Space's move.
    fn key(&mut self, ch: char) {
        let bytes = if ch == ' ' {
            space_bytes(self.col)
        } else {
            glyph_bytes(ch, self.col)
        };
        self.press(ch, &bytes);
        self.col += 1;
    }

    fn type_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.key(ch);
        }
    }

    /// A wrap key `ch` answered by `bytes`; the next glyph lands on
    /// 1-based column `col`.
    fn wrap(&mut self, ch: char, bytes: &[u8], col: usize) {
        self.press(ch, bytes);
        self.col = col;
    }

    /// One more frame of the train, whenever it is due.
    fn next_frame(&mut self) {
        let t = self.next_frame;
        self.advance_to(t);
    }

    /// The `t` of the live cell at `(row, col)`.
    fn t_at(&self, row: u16, col: u16) -> Option<f32> {
        self.ribbon()
            .cells()
            .iter()
            .find(|c| c.row == row && c.col == col && !c.leaving())
            .map(|c| c.t)
    }
}

/// `run1`'s shape up to the first wrap: `line1` (86 glyphs) typed, the Space
/// that fits after it, the band laid under both.
fn typed_to_the_first_wrap(rule: &str, line1: &str) -> Host {
    let mut h = Host::new(rule);
    h.type_str(line1);
    h.key(' ');
    h.next_frame();
    let c = h.term.cursor();
    let end = 2 + line1.chars().count() as u16;
    assert_eq!(
        (c.row, c.col),
        (CARET_ROW, end + 1),
        "the caret stands after the Space"
    );
    h
}

/// `run1` up to the second wrap: the first wrap on its `k`, then the rest of
/// `line2` typed on the continuation row. `line2` must begin with `k`.
fn typed_to_the_second_wrap(line2: &str) -> Host {
    let mut h = typed_to_the_first_wrap(&rule(), LINE1);
    h.wrap('k', &first_wrap_bytes(&rule(), LINE1, LINE1, "k"), 4);
    h.next_frame();
    assert!(line2.starts_with('k'));
    h.type_str(&line2[1..]);
    h.next_frame();
    let c = h.term.cursor();
    let end = 2 + line2.chars().count() as u16;
    assert_eq!((c.row, c.col), (CARET_ROW, end), "the caret after the line");
    let live = h.live(CARET_ROW);
    assert!(
        live.first() == Some(&2) && live.last() == Some(&(end - 1)),
        "the second line's band is laid under it: {live:?}"
    );
    h
}

/// **THE WRITERS ARE THE CAPTURE.** `first_wrap_bytes` and
/// `second_wrap_bytes` rebuild `run1`'s two wrap chunks byte for byte from
/// the two lines alone — the sparse write of row 50 included — so a law
/// that feeds them other text feeds the shape Claude Code writes.
#[test]
fn the_writers_reproduce_the_captured_wrap_chunks_byte_for_byte() {
    assert_eq!(
        String::from_utf8(first_wrap_bytes(&rule(), LINE1, LINE1, "k")).expect("utf-8"),
        captured_first_wrap()
    );
    assert_eq!(captured_first_wrap().len(), 458, "run1 key 87 is 458 bytes");
    assert_eq!(
        String::from_utf8(second_wrap_bytes(LINE1, LINE2, "")).expect("utf-8"),
        captured_second_wrap()
    );
    assert_eq!(
        captured_second_wrap().len(),
        578,
        "run1 key 174 is 578 bytes"
    );
    // The Space that wraps a FULL line (the pty capture's KEY 87, 87 glyphs
    // typed, no bracket there — put back as the window sees it): the
    // continuation row is cut whole from its first column.
    let full =
        "the quick brown fox jumps over lazy dogs the quick brown fox jumps over lazy dogs abcde";
    assert_eq!(
        String::from_utf8(first_wrap_bytes(&rule(), full, full, "")).expect("utf-8"),
        format!(
            "\x1b[?2026h\x1b[?25l\x1b[H\r\x1b[48B\x1b[38;2;136;136;136m{}\r\x1b[1B\x1b[39m\u{276f}\u{a0}{full}\x1b[K\r\x1b[1B\x1b[K\x1b[53;1H\x1b[51;3H\x1b[?25h\x1b[?2026l",
            "\u{2500}".repeat(91)
        )
    );
    assert_eq!(
        shared_letters(LINE2, LINE1),
        vec![28, 30, 31, 47, 79],
        "the premise: five letters of the second line match the first at their column"
    );
}

/// **THE SECOND WRAPPED LINE FOLLOWS ITS TEXT A ROW UP.** `run1` on glass:
/// after the Space that wraps the second line, its band stands under its
/// own glyphs on row 49 — every cell of it, none retired — and it is still
/// lit past `RETIRE_MELT_S`; the continuation row the caret is on holds no
/// stale light.
///
/// RED on `aa71f9319`: the five letters the second line shares with the
/// first were each scored a hole in the block that moved, the follow pass
/// named nothing (`followed +0`), and the walk retired the whole run on the
/// caret's row in `RETIRE_MELT_S` — the on-glass `trail status` read
/// `ribbon_retired=87` right after this wrap's echo, `ribbon_followed=`
/// unmoved from the first wrap's 86 — while row 49 went dark.
#[test]
fn the_second_wrapped_line_follows_its_text_a_row_up_despite_the_letters_it_shares() {
    let mut h = typed_to_the_second_wrap(LINE2);
    let n = h.live(CARET_ROW).len();
    let pre_cov = h.coverage(CARET_ROW);
    let (followed0, retired0) = h.counts();
    h.wrap(' ', &captured_second_wrap().into_bytes(), 3);
    h.next_frame();
    let (followed, retired) = h.counts();
    assert_eq!(
        (followed - followed0, retired - retired0),
        (n as u64, 0),
        "`ribbon_followed=` counts the second line's {n} cells and `ribbon_retired=` none"
    );
    let live = h.live(UP_ONE);
    assert!(
        live.len() == n && live.first() == Some(&2) && live.last() == Some(&88),
        "row 49 carries the second line's band, 2..=88: {live:?}"
    );
    assert!(
        h.cells(CARET_ROW).is_empty(),
        "the continuation row holds no stale light: {:?}",
        h.cells(CARET_ROW)
    );
    h.idle(((RETIRE_MELT_S + 0.03) * 1000.0) as u64);
    let cov = h.coverage(UP_ONE);
    assert!(
        cov >= 0.5 * pre_cov,
        "at +{:.2} s the followed band is not melted: {cov} against {pre_cov} before",
        RETIRE_MELT_S + 0.03
    );
}

/// **THE THIRD ROW CONTINUES THE SPECTRUM.** The keys typed after the second
/// wrap walk on from where the second line's band ended: every cell of the
/// new row lies past the second line's last `t`, rising, and none of them
/// is in a cohort that is leaving.
///
/// RED on `aa71f9319`: the second line's band melted on the caret's row and
/// the new keys walked from the START of its walk — the third row's first
/// cell took exactly the `t` of the second line's first cell (2.97, against
/// the 5.92 the line ended on): the colours started over, the owner's
/// "smashes the spectrum on the next line".
#[test]
fn the_row_after_the_second_wrap_continues_the_walk_the_second_line_left_off() {
    let mut h = typed_to_the_second_wrap(LINE2);
    let t_end = h.t_at(CARET_ROW, 88).expect("the second line's last cell");
    h.wrap(' ', &second_wrap_bytes(LINE1, LINE2, ""), 3);
    h.next_frame();
    h.type_str(AFTER);
    h.next_frame();
    let row: Vec<(u16, f32, bool)> = {
        let mut v: Vec<(u16, f32, bool)> = h
            .ribbon()
            .cells()
            .iter()
            .filter(|c| c.row == CARET_ROW)
            .map(|c| (c.col, c.t, c.leaving()))
            .collect();
        v.sort_by_key(|c| c.0);
        v
    };
    assert!(
        !row.is_empty() && row.iter().all(|c| !c.2),
        "the third row's cells are the hand's, none leaving: {row:?}"
    );
    assert!(
        row.iter().all(|c| c.1 > t_end),
        "every cell of the third row lies past the second line's end t {t_end}: {row:?}"
    );
    assert!(
        row.windows(2).all(|w| w[1].1 >= w[0].1 - 1e-5),
        "the third row's walk rises: {row:?}"
    );
}

/// **A SPACE TYPED AT THE END OF THE LINE GOES UP WITH IT.** `run1` keys 86
/// and 87: the Space after `each` fits on 1-based column 89, then the `k`
/// wraps. The Space's cell — never armed, since a Space writes no glyph —
/// rides up to row 49 with the line it ended, and nothing is left behind
/// on the continuation row right of the moved `k`.
///
/// RED on `aa71f9319`: the follow pass carried only the extent of the
/// ARMED glyphs, 0-based 2..=87, and the Space's cell stayed on the caret's
/// row at column 88 — the owner's "skips a space".
#[test]
fn a_space_typed_at_the_end_of_a_full_line_goes_up_with_it_when_the_next_glyph_wraps() {
    let mut h = typed_to_the_first_wrap(&rule(), LINE1);
    assert!(
        h.live(CARET_ROW).contains(&88),
        "the Space laid its cell on column 88: {:?}",
        h.live(CARET_ROW)
    );
    h.wrap('k', &captured_first_wrap().into_bytes(), 4);
    h.next_frame();
    let up = h.live(UP_ONE);
    assert!(
        up.first() == Some(&2) && up.last() == Some(&88),
        "the line's band and its Space's cell rode up together, 2..=88: {up:?}"
    );
    assert!(
        h.cells(CARET_ROW).iter().all(|&(c, _)| c < 4),
        "nothing stranded on the continuation row past the moved `k`: {:?}",
        h.cells(CARET_ROW)
    );
}

/// **THE FIRST WRAP UNDER A LABELLED RULE FOLLOWS TOO.** The first line
/// wraps under the rule, which shares no letter with it — why the first wrap
/// usually followed — but a LABELLED rule (2.1.278 writes one; the label on
/// 1-based column 80) can: `…and then at` puts the `t` of `then` under the
/// `t` of `extension` (a stand-in for the vendor's 9-letter label, sharing
/// the same one letter). The band follows the whole line. (The chunk writes
/// the line whole where Ink's diff would step over that `t`; the screen it
/// leaves is the same.)
///
/// RED on `aa71f9319`, as the second wrap is: the shared `t` split the
/// block, nothing followed, and the line's band melted on the caret's row.
#[test]
fn the_first_wrap_under_a_labelled_rule_follows_despite_a_letter_the_label_shares() {
    let label = "extension";
    let line1 =
        "can you check the cursor trail when a long prompt wraps onto a second line and then at";
    let rule = labelled_rule(label);
    let under: String = " ".repeat(79 - 2) + label;
    assert_eq!(
        shared_letters(line1, &under),
        vec![81],
        "the premise: the line and the label share one letter, the `t` on column 81"
    );
    let mut h = typed_to_the_first_wrap(&rule, line1);
    let n = h.live(CARET_ROW).iter().filter(|&&c| c <= 88).count();
    let (followed0, retired0) = h.counts();
    h.wrap('l', &first_wrap_bytes(&rule, line1, line1, "l"), 4);
    h.next_frame();
    let (followed, retired) = h.counts();
    assert_eq!(
        (followed - followed0, retired - retired0),
        (n as u64, 0),
        "the line's {n} cells followed, none retired"
    );
    let up = h.live(UP_ONE);
    assert!(
        up.first() == Some(&2) && up.last() == Some(&88),
        "row 49 carries the line's band: {up:?}"
    );
}

/// **A LINE TYPED UNDER A NEAR-COPY OF ITSELF FOLLOWS TOO.** A prompt that
/// repeats an instruction with one word changed, each sentence one full
/// row (87 columns, the Space after it wrapping): the second line's glyphs
/// stand under their twins on the first at every column but the three of
/// `new` under `old`. Those three are the only records that can testify —
/// the rest are twins standing where the moved text puts them — and they
/// arrived: the band follows its text a row up, whole.
///
/// RED with the twins still counted in the half (the neutral law alone): 3
/// found of 70 armed is under half, nothing followed, and the band melted
/// on the caret's row — `followed +0`, `retired +87` — the defect this file
/// is about, for a line of repeated words. RED on `aa71f9319` the same way.
#[test]
fn a_line_typed_under_a_near_copy_of_itself_follows_on_the_letters_that_differ() {
    let s1 =
        "run the effects suite on the old tree and paste each red test name and its output here.";
    let s2 =
        "run the effects suite on the new tree and paste each red test name and its output here.";
    let glyphs = s2.chars().filter(|&c| c != ' ').count();
    assert_eq!(
        (s1.chars().count(), shared_letters(s2, s1).len(), glyphs),
        (87, glyphs - 3, 70),
        "the premise: two full rows alike but for `new` under `old`"
    );
    let mut h = Host::new(&rule());
    h.type_str(s1);
    h.next_frame();
    h.wrap(' ', &first_wrap_bytes(&rule(), s1, s1, ""), 3);
    h.next_frame();
    h.type_str(s2);
    h.next_frame();
    let n = h.live(CARET_ROW).len();
    assert_eq!(n, 87, "the second line's band is laid under it");
    let (followed0, retired0) = h.counts();
    h.wrap(' ', &second_wrap_bytes(s1, s2, ""), 3);
    h.next_frame();
    let (followed, retired) = h.counts();
    assert_eq!(
        (followed - followed0, retired - retired0),
        (n as u64, 0),
        "the second line's {n} cells followed, none retired"
    );
    assert_eq!(h.live(UP_ONE).len(), n, "row 49 carries the band");
    assert!(
        h.cells(CARET_ROW).is_empty(),
        "the continuation row holds no stale light: {:?}",
        h.cells(CARET_ROW)
    );
}

/// **NEGATIVE CONTROL: A SECOND LINE THAT SHARES NO LETTER FOLLOWS, BEFORE
/// AND AFTER.** The same two wraps with a second line that matches the
/// first at no column: the sparse write skips nothing but shared blanks,
/// no record is a twin, and the band follows — GREEN on `aa71f9319` too,
/// which is what says the RED laws above are the shared letters and nothing
/// else about the second wrap.
#[test]
fn a_second_line_that_shares_no_letter_with_the_first_follows_on_both_trees() {
    let line2 =
        "key we type into this prompt should keep its band glowing right under it as it grows up";
    assert!(
        shared_letters(line2, LINE1).is_empty(),
        "the premise: no letter shared at its column"
    );
    let mut h = typed_to_the_second_wrap(line2);
    let n = h.live(CARET_ROW).len();
    let (followed0, retired0) = h.counts();
    h.wrap(' ', &second_wrap_bytes(LINE1, line2, ""), 3);
    h.next_frame();
    let (followed, retired) = h.counts();
    assert_eq!(
        (followed - followed0, retired - retired0),
        (n as u64, 0),
        "the second line's {n} cells followed, none retired"
    );
    assert_eq!(h.live(UP_ONE).len(), n);
    assert!(h.cells(CARET_ROW).is_empty());
}

/// **NEGATIVE CONTROL: OTHER TEXT A ROW UP IS NOT FOLLOWED.** The same
/// second wrap, but the row the second line would move to is written with
/// DIFFERENT text (its letters rotated by 13): the band's glyphs are found
/// nowhere, nothing follows, and the second line's band goes out where it
/// stood — its text went and nothing replaced it, so the walk releases it
/// (`ribbon_retired=` counts it) and its cohort drains into the retract on
/// the caret's row — while the row the other text went to never lights. The
/// neutral twin loosened nothing for text that did not move. GREEN on
/// `aa71f9319` and here.
#[test]
fn a_second_wrap_that_writes_other_text_a_row_up_follows_nothing() {
    let mut h = typed_to_the_second_wrap(LINE2);
    let n = h.live(CARET_ROW).len();
    let other: String = LINE2
        .chars()
        .map(|c| match c {
            'a'..='z' => (((c as u8 - b'a' + 13) % 26) + b'a') as char,
            c => c,
        })
        .collect();
    let (followed0, retired0) = h.counts();
    h.wrap(' ', &second_wrap_bytes(LINE1, &other, ""), 3);
    h.next_frame();
    let (followed, retired) = h.counts();
    assert_eq!(
        followed - followed0,
        0,
        "nothing followed: the text is not the run's"
    );
    assert!(
        retired - retired0 >= n as u64,
        "the second line's band went out with its text: {} of {n}",
        retired - retired0
    );
    assert!(
        h.ribbon()
            .cohorts()
            .iter()
            .filter(|c| c.row == CARET_ROW)
            .all(|c| c.abandoned),
        "its cohort drains into the retract where it stood"
    );
    let span = ((RETRACT_DUR_S + RETRACT_FADE_S) * 1000.0) as u64 + 100;
    let end = h.now + Duration::from_millis(span);
    while h.now < end {
        assert!(
            h.cells(UP_ONE).is_empty() && h.coverage(UP_ONE) == 0.0,
            "the row the other text went to never lights: {:?}",
            h.cells(UP_ONE)
        );
        h.next_frame();
    }
    assert!(
        h.cells(CARET_ROW).is_empty(),
        "…and the band is gone from the caret's row by the retract's end: {:?}",
        h.cells(CARET_ROW)
    );
}

/// **THE LIMIT, STATED AS A LAW: A LINE IDENTICAL TO THE ONE ABOVE IT — OR
/// ONE COLUMN OFF — MELTS INSTEAD OF FOLLOWING** (2026-09-24). The second
/// line is the first again, or the first with
/// one digit changed (`step 1:` then `step 2:`). At the second wrap it
/// moves a row up onto a row that ALREADY held it, glyph for glyph but for
/// at most one: nothing arrived there, or one glyph did, and one is too
/// little evidence for a move. That screen is exactly the one a line erased
/// under an identical line leaves (`tests/erased_under_its_twin.rs`), and
/// the follow pass cannot tell the two apart from the glyphs — so it
/// refuses both, and this band goes out on the caret's row in
/// `RETIRE_MELT_S` while its text stands a row up. What the owner sees is
/// the defect this file fixed, for a line that repeats the one above it
/// (a sentence typed twice; a row of one held letter has the same shape).
/// If a future witness can tell the two apart, flip this law; do not loosen
/// the erase laws to pass it.
///
/// It also pins what the limit must NOT become. The first line, re-laid a
/// row higher by the same wrap, is the identical text ARRIVING two rows up,
/// and a twin one row away does not stop the search two rows away
/// (`a_twin_testifies_only_about_its_own_row`): the band stays off the
/// FIRST line because nothing samples its row at that wrap — no row two
/// away is named for its own sake (`no_row_two_away_is_named`), and the
/// first line's own band, which would name it, has left by then at this
/// law's pace. Were that row named, the band would go onto the first line
/// here — a follow pass that stopped its search at a whole text one row
/// away would keep it off, and melt the line under an identical command
/// pushed down two rows (`tests/moves_are_followed.rs`). GREEN on 0.93.0,
/// which named no row two away either.
#[test]
fn the_limit_a_line_identical_to_the_one_above_it_or_one_column_off_melts_instead_of_following() {
    let step1 =
        "step 1: run the effects suite on the old tree and paste each red test name and outputs.";
    let step2 =
        "step 2: run the effects suite on the old tree and paste each red test name and outputs.";
    for (s1, s2) in [(step1, step1), (step1, step2)] {
        assert_eq!(s2.chars().count(), 87, "the premise: two full rows");
        let mut h = Host::new(&rule());
        h.type_str(s1);
        h.next_frame();
        h.wrap(' ', &first_wrap_bytes(&rule(), s1, s1, ""), 3);
        h.next_frame();
        h.type_str(s2);
        h.next_frame();
        let n = h.live(CARET_ROW).len();
        assert_eq!(n, 87, "the second line's band is laid under it");
        let (followed0, retired0) = h.counts();
        h.wrap(' ', &second_wrap_bytes(s1, s2, ""), 3);
        h.next_frame();
        let (followed, retired) = h.counts();
        assert_eq!(
            followed - followed0,
            0,
            "`{}` under `{}`: nothing arrived that was not standing there",
            &s2[..6],
            &s1[..6]
        );
        assert!(
            retired - retired0 >= n as u64,
            "`{}` under `{}`: the band goes out on the caret's row: {} of {n}",
            &s2[..6],
            &s1[..6],
            retired - retired0
        );
    }
}
