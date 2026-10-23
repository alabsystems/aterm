// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claude Code's composer.

use crate::trail_host::{Core, LOCK_A, Theme};
use aterm_effects::rainbow_kitty::TypedClass;
use aterm_effects::rainbow_kitty::ribbon::{RETIRE_MELT_S, RETRACT_DUR_S, RETRACT_FADE_S};
use std::time::{Duration, Instant};

const ROWS: usize = 53;
const COLS: usize = 91;
const CW: usize = 15;
const CH: usize = 28;

const FRAME_US: u64 = 16_667;
const KEY_MS: u64 = 80;

const TOP_RULE_ROW: usize = 50;
const TEXT_ROW: usize = 51;
const BOTTOM_RULE_ROW: usize = 52;
const STATUS_ROW: usize = 53;
const CARET_ROW: u16 = (TEXT_ROW - 1) as u16;
const UP_ONE: u16 = CARET_ROW - 1;

const SYNC_BEGIN: &str = "\x1b[?2026h";
const SYNC_END: &str = "\x1b[?2026l";
const RULE_PEN: &str = "\x1b[38;2;136;136;136m";
const TEXT_PEN: &str = "\x1b[39m";
const PROMPT: &str = "\u{276f}\u{a0}";

const LINE1: &str =
    "the quick brown fox jumps over the lazy dog while the rainbow kitty trail follows each";
const LINE2: &str =
    "key we type into this composer so we can watch exactly what happens when the line wraps";
const AFTER: &str = "and then";

fn captured_first_wrap() -> String {
    format!(
        "\x1b[?2026h\x1b[?25l\x1b[H\r\x1b[48B\x1b[38;2;136;136;136m{}\r\x1b[1B\x1b[39m\u{276f}\u{a0}the quick brown fox jumps over the lazy dog while the rainbow kitty trail follows each\x1b[K\r\x1b[1B  k\x1b[K\x1b[53;1H\x1b[51;4H\x1b[?25h\x1b[?2026l",
        "\u{2500}".repeat(91)
    )
}

fn captured_second_wrap() -> String {
    format!(
        "\x1b[?2026h\x1b[?25l\x1b[H\r\x1b[47B\x1b[38;2;136;136;136m{}\r\x1b[1B\x1b[39m\u{276f}\u{a0}the quick brown fox jumps over the lazy dog while the rainbow kitty trail follows each\x1b[K\r\x1b[1B  key\x1b[7Gwe type into this comp\x1b[30Gs\x1b[34Gso we can watc\x1b[49G exactly what happens\x1b[71Gwhen the \x1b[81Gine\x1b[85Gwraps\r\x1b[2C\x1b[1B\x1b[K\x1b[53;1H\x1b[51;3H\x1b[?25h\x1b[?2026l",
        "\u{2500}".repeat(91)
    )
}

fn rule() -> String {
    "\u{2500}".repeat(COLS)
}

fn labelled_rule(label: &str) -> String {
    format!(
        "{}\x1b[80G\x1b[38;2;175;135;255m{label}\x1b[{COLS}G{RULE_PEN}\u{2500}",
        "\u{2500}".repeat(78)
    )
}

fn glyph_bytes(ch: char, c: usize) -> Vec<u8> {
    format!(
        "{SYNC_BEGIN}\x1b[?25l\x1b[H\r\x1b[{}C\x1b[{}B{ch}\x1b[{STATUS_ROW};1H\x1b[{TEXT_ROW};{}H\x1b[?25h{SYNC_END}",
        c - 1,
        TEXT_ROW - 1,
        c + 1
    )
    .into_bytes()
}

fn space_bytes(c: usize) -> Vec<u8> {
    format!(
        "{SYNC_BEGIN}\x1b[?25l\x1b[{TEXT_ROW};{}H\x1b[?25h{SYNC_END}",
        c + 1
    )
    .into_bytes()
}

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

fn first_wrap_bytes(rule: &str, line1: &str, typed: &str, moved: &str) -> Vec<u8> {
    format!(
        "{SYNC_BEGIN}\x1b[?25l\x1b[H\r\x1b[{}B{RULE_PEN}{rule}\r\x1b[1B{TEXT_PEN}{PROMPT}{line1}\x1b[K{}\x1b[{STATUS_ROW};1H\x1b[{TEXT_ROW};{}H\x1b[?25h{SYNC_END}",
        TOP_RULE_ROW - 2,
        ink_row(&format!("{PROMPT}{typed}"), &format!("  {moved}")),
        3 + moved.chars().count()
    )
    .into_bytes()
}

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

fn shared_letters(a: &str, b: &str) -> Vec<u16> {
    a.chars()
        .zip(b.chars())
        .enumerate()
        .filter(|&(_, (x, y))| x == y && x != ' ')
        .map(|(i, _)| i as u16 + 2)
        .collect()
}

struct Host {
    c: Core,
    next_frame: Instant,
    last_key: Instant,
    /// The 1-based column the next glyph lands on.
    col: usize,
}
core_deref!(Host);

impl Host {
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

    fn frame(&mut self) {
        self.c.frame();
        self.next_frame = self.now + Duration::from_micros(FRAME_US);
    }

    fn advance_to(&mut self, t: Instant) {
        while self.next_frame <= t {
            self.now = self.next_frame;
            self.frame();
        }
        self.now = self.now.max(t);
    }

    fn idle(&mut self, ms: u64) {
        let t = self.now + Duration::from_millis(ms);
        self.advance_to(t);
    }

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

    fn wrap(&mut self, ch: char, bytes: &[u8], col: usize) {
        self.press(ch, bytes);
        self.col = col;
    }

    fn next_frame(&mut self) {
        let t = self.next_frame;
        self.advance_to(t);
    }

    fn t_at(&self, row: u16, col: u16) -> Option<f32> {
        self.ribbon()
            .cells()
            .iter()
            .find(|c| c.row == row && c.col == col && !c.leaving())
            .map(|c| c.t)
    }
}

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

#[test]
fn agy_three_col_prompt_wraps_to_two_col_continuation_without_a_gap() {
    let agy_prompt = " > ";
    let line1_kept = "design a superior solution so that I read what is happening. finally:";
    let wrapped_word = "LOOK!";
    let mut c = Core::new(ROWS, COLS, CW, CH, Theme::Default, LOCK_A);
    c.term.process(b"\x1b[?1049h\x1b[2J\x1b[H");
    c.term.process(
        format!(
            "\x1b[{TOP_RULE_ROW};1H{RULE_PEN}{}\x1b[{TEXT_ROW};1H{TEXT_PEN}{agy_prompt}\x1b[{BOTTOM_RULE_ROW};1H{RULE_PEN}{}{TEXT_PEN}\x1b[{TEXT_ROW};4H",
            rule(),
            rule()
        )
        .as_bytes(),
    );
    let now = c.now;
    let mut h = Host {
        c,
        next_frame: now,
        last_key: now,
        col: 4,
    };
    h.frame();

    let pre_wrap = format!("{line1_kept} {wrapped_word}");
    h.type_str(&pre_wrap);
    h.next_frame();
    assert_eq!(
        h.live(CARET_ROW).first(),
        Some(&3),
        "line 1's 3-column ` > ` prompt starts live cells at column 3"
    );

    let moved = format!("{wrapped_word} ");
    let wrap_bytes = format!(
        "{SYNC_BEGIN}\x1b[?25l\x1b[H\r\x1b[{}B{RULE_PEN}{}\r\x1b[1B{TEXT_PEN}{agy_prompt}{line1_kept} \x1b[K{}\x1b[{TEXT_ROW};{}H\x1b[?25h{SYNC_END}",
        TOP_RULE_ROW - 2,
        rule(),
        ink_row(&format!("{agy_prompt}{pre_wrap}"), &format!("  {moved}")),
        3 + moved.chars().count()
    )
    .into_bytes();
    h.wrap(' ', &wrap_bytes, 3 + moved.chars().count());
    h.next_frame();

    h.type_str("a gap");
    h.next_frame();

    let live = h.live(CARET_ROW);
    let expected: Vec<u16> = (2..13).collect();
    assert_eq!(
        live, expected,
        "every cell of `LOOK! a gap` on the 2-column continuation row is lit without a gap: {live:?}"
    );
}
