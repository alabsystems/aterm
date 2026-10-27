// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A line that moved carries its band, at every width, timing and host.

use crate::trail_host::*;
use aterm_effects::cursor_glow::InsertWidth;
use aterm_effects::kitty_pet::PetBrain;
use aterm_effects::rainbow_kitty::TypedClass;
use std::time::{Duration, Instant};

mod b2m {
    use super::*;

    const CW: usize = 15;
    const CH: usize = 28;
    const FRAME_US: u64 = 16_667;
    const SYNC_BEGIN: &str = "\x1b[?2026h";
    const SYNC_END: &str = "\x1b[?2026l";
    const RULE_PEN: &str = "\x1b[38;2;136;136;136m";
    const TEXT_PEN: &str = "\x1b[39m";
    const PROMPT: &str = "\u{276f}\u{a0}";

    pub struct Host {
        c: Core,
        /// `Pace::Flat(_)`: the 16.667 ms train; otherwise the GUI's pacing.
        pace: Pace,
        next_frame: Instant,
        /// When the last frame was drawn.
        last_frame: Instant,
        /// Output landed that no frame has presented yet.
        dirty: bool,
        last_key: Instant,
        rows: usize,
        cols: usize,
        lines: Vec<String>,
        cur: String,
        key_ms: u64,
        transcript: Vec<String>,
    }
    core_deref!(Host);

    impl Host {
        fn width(&self) -> usize {
            self.cols - 4
        }
        fn caret_row1(&self) -> usize {
            self.rows - 2
        }
        fn caret_row0(&self) -> u16 {
            (self.rows - 3) as u16
        }
        fn top_rule1(&self) -> usize {
            self.rows - 3 - self.lines.len()
        }

        fn new(rows: usize, cols: usize, key_ms: u64) -> Self {
            Self::paced(rows, cols, key_ms, Pace::Flat(16))
        }

        fn paced(rows: usize, cols: usize, key_ms: u64, pace: Pace) -> Self {
            let mut c = Core::new(rows, cols, CW, CH, Theme::Default, LOCK_A);
            if matches!(pace, Pace::Pet) {
                c.pet = Some(PetBrain::default());
            }
            c.term.process(b"\x1b[?1049h\x1b[2J\x1b[H");
            let rule = "\u{2500}".repeat(cols);
            c.term.process(
                format!(
                    "\x1b[{};1H{RULE_PEN}{rule}\x1b[{};1H{TEXT_PEN}{PROMPT}\x1b[{};1H{RULE_PEN}{rule}{TEXT_PEN}\x1b[{};1H  \u{23f5}\u{23f5} auto mode on (shift+tab to cycle)\x1b[{};3H",
                    rows - 3,
                    rows - 2,
                    rows - 1,
                    rows,
                    rows - 2
                )
                .as_bytes(),
            );
            let now = c.now;
            let mut h = Self {
                c,
                pace,
                next_frame: now,
                last_frame: now,
                dirty: false,
                last_key: now,
                rows,
                cols,
                lines: Vec::new(),
                cur: String::new(),
                key_ms,
                transcript: Vec::new(),
            };
            h.frame_t();
            h
        }

        fn frame_t(&mut self) {
            self.c.frame();
            self.dirty = false;
            self.last_frame = self.now;
            self.next_frame = self.now + Duration::from_micros(FRAME_US);
            if !matches!(self.pace, Pace::Flat(_)) {
                self.next_frame = self.paced_next();
            }
        }

        fn paced_next(&self) -> Instant {
            let fd = Duration::from_micros(FRAME_US);
            let glow = if self.dirty || self.glow.needs_frame_cadence() {
                Some(self.now + fd)
            } else {
                self.glow.next_change_deadline(self.now, fd)
            };
            let pet = self.pet.as_ref().and_then(|p| {
                if p.needs_frames() {
                    Some(self.now + fd)
                } else {
                    p.next_change_deadline(self.now)
                }
            });
            match (glow, pet) {
                (Some(a), Some(b)) => a.min(b),
                (a, b) => a.or(b).unwrap_or(self.now + Duration::from_secs(3600)),
            }
        }

        fn advance_to(&mut self, t: Instant) {
            while self.next_frame <= t {
                self.now = self.next_frame.max(self.now);
                self.frame_t();
            }
            self.now = self.now.max(t);
        }

        fn next_frame(&mut self) {
            let t = self.next_frame;
            self.advance_to(t);
        }

        fn landed(&mut self, bytes: &[u8]) {
            self.term.process(bytes);
            self.dirty = true;
            if !matches!(self.pace, Pace::Flat(_)) {
                let due = (self.last_frame + Duration::from_micros(FRAME_US)).max(self.now);
                self.next_frame = self.next_frame.min(due);
            }
        }

        fn press(&mut self, ch: char, bytes: &[u8], gap_ms: u64) {
            let t = self.last_key + Duration::from_millis(gap_ms);
            self.advance_to(t);
            self.now = t;
            self.last_key = t;
            let class = if ch == ' ' {
                TypedClass::Space
            } else {
                TypedClass::Glyph
            };
            self.glow.note_typed_glyph(t, 1, false, class);
            self.landed(bytes);
        }

        fn repaint_bytes(&self) -> Vec<u8> {
            let mut s = format!("{SYNC_BEGIN}\x1b[?25l");
            let top = self.top_rule1();
            for (i, t) in self.transcript.iter().enumerate() {
                let r = top.saturating_sub(self.transcript.len()) + i;
                if r >= 1 && r < top {
                    s.push_str(&format!("\x1b[{r};1H{TEXT_PEN}{t}\x1b[K"));
                }
            }
            s.push_str(&format!(
                "\x1b[{top};1H{RULE_PEN}{}",
                "\u{2500}".repeat(self.cols)
            ));
            for (i, l) in self.lines.iter().enumerate() {
                let pre = if i == 0 { PROMPT } else { "  " };
                s.push_str(&format!("\x1b[{};1H{TEXT_PEN}{pre}{l}\x1b[K", top + 1 + i));
            }
            let pre = if self.lines.is_empty() { PROMPT } else { "  " };
            s.push_str(&format!(
                "\x1b[{};1H{TEXT_PEN}{pre}{}\x1b[K",
                self.caret_row1(),
                self.cur
            ));
            s.push_str(&format!(
                "\x1b[{};1H\x1b[{};{}H\x1b[?25h{SYNC_END}",
                self.rows,
                self.caret_row1(),
                3 + self.cur.chars().count()
            ));
            s.into_bytes()
        }

        fn key_gap(&mut self, ch: char, gap_ms: u64) -> bool {
            let w = self.width();
            let len = self.cur.chars().count();
            if len < w {
                let c = 3 + len;
                let bytes = if ch == ' ' {
                    format!(
                        "{SYNC_BEGIN}\x1b[?25l\x1b[{};{}H\x1b[?25h{SYNC_END}",
                        self.caret_row1(),
                        c + 1
                    )
                } else {
                    format!(
                        "{SYNC_BEGIN}\x1b[?25l\x1b[H\r\x1b[{}C\x1b[{}B{ch}\x1b[{};1H\x1b[{};{}H\x1b[?25h{SYNC_END}",
                        c - 1,
                        self.caret_row1() - 1,
                        self.rows,
                        self.caret_row1(),
                        c + 1
                    )
                };
                self.cur.push(ch);
                self.press(ch, bytes.as_bytes(), gap_ms);
                return false;
            }
            let (line, moved) = if ch == ' ' {
                (self.cur.trim_end().to_string(), String::new())
            } else if self.cur.ends_with(' ') {
                (self.cur.trim_end().to_string(), ch.to_string())
            } else if let Some(i) = self.cur.rfind(' ') {
                let mut m = self.cur[i + 1..].to_string();
                m.push(ch);
                (self.cur[..i].trim_end().to_string(), m)
            } else {
                (self.cur.clone(), ch.to_string())
            };
            self.lines.push(line);
            self.cur = moved;
            let bytes = self.repaint_bytes();
            self.press(ch, &bytes, gap_ms);
            true
        }

        fn newline_gap(&mut self, gap_ms: u64) {
            self.lines.push(self.cur.clone());
            self.cur.clear();
            let bytes = self.repaint_bytes();
            let t = self.last_key + Duration::from_millis(gap_ms);
            self.advance_to(t);
            self.now = t;
            self.last_key = t;
            self.glow.note_newline_break(t);
            self.glow.note_typed_glyph(t, 1, false, TypedClass::Glyph);
            self.landed(&bytes);
        }

        fn key(&mut self, ch: char) -> bool {
            let g = self.key_ms;
            self.key_gap(ch, g)
        }

        fn line_row0(&self, i: usize) -> u16 {
            (self.caret_row0() as usize - self.lines.len() + i) as u16
        }

        fn census(&self) -> Vec<(usize, usize)> {
            (0..self.lines.len())
                .map(|i| {
                    let r = self.line_row0(i);
                    let n = self.lines[i].chars().count();
                    let lo = 2u16;
                    let hi = 2 + n as u16;
                    let lit = self.live(r).iter().filter(|&&c| c >= lo && c < hi).count();
                    (lit, n)
                })
                .collect()
        }

        fn lit_before(&self) -> Vec<usize> {
            let before_lines = self.lines.len();
            (0..=before_lines)
                .map(|i| {
                    let r = if i == before_lines {
                        self.caret_row0()
                    } else {
                        self.line_row0(i)
                    };
                    self.live(r).len()
                })
                .collect()
        }
    }

    fn dark_of(census: &[(usize, usize)], before: &[usize]) -> Vec<usize> {
        census
            .iter()
            .enumerate()
            .filter(|(i, (lit, n))| {
                *lit * 10 < *n * 9 && before.get(*i).is_some_and(|b| *b * 10 >= *n * 9)
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn run(h: &mut Host, text: &str, pause_before_line_ms: u64) -> Outcome {
        let mut first_of_line = false;
        let (mut df, mut dr, mut dark, mut wraps) = (0, 0, 0, 0);
        let mut notes = Vec::new();
        for ch in text.chars() {
            let before = h.lit_before();
            let (f0, r0) = h.counts();
            let gap = if first_of_line && pause_before_line_ms > 0 {
                pause_before_line_ms
            } else {
                h.key_ms
            };
            first_of_line = false;
            if h.key_gap(ch, gap) {
                wraps += 1;
                h.next_frame();
                h.next_frame();
                let (f1, r1) = h.counts();
                df += f1 - f0;
                dr += r1 - r0;
                let d = dark_of(&h.census(), &before);
                if !d.is_empty() {
                    notes.push(format!("wrap{wraps}:dark{d:?}"));
                }
                dark += d.len() as u64;
                first_of_line = true;
            }
        }
        Outcome::mf(df, dr, dark).with_note(format!("wraps={wraps} {}", notes.join(" ")))
    }

    const PROSE: &str = "the quick brown fox jumps over the lazy dog while the rainbow kitty trail follows each key we type into this composer so we can watch exactly what happens when the line wraps and then again when it wraps a second time and a third time and a fourth time because a long prompt is what people really type into claude code every single day of the week and the band should follow every line of it up the screen as the box grows taller and taller with each new row of text that we add to it here";

    const LIST: [&str; 5] = [
        "- fix the wrap",
        "- keep the band",
        "- test it all",
        "- then ship it",
        "- and tell me",
    ];

    fn newline_prompt(
        cols: usize,
        key_ms: u64,
        nl_gap: u64,
        take: usize,
        stale_at: usize,
    ) -> Outcome {
        let mut h = Host::new(53, cols, key_ms);
        let lines = &LIST[..take];
        let (mut df, mut dr, mut dark) = (0, 0, 0);
        let mut notes = Vec::new();
        for (k, l) in lines.iter().enumerate() {
            for ch in l.chars() {
                if h.key(ch) {
                    return Outcome::mf(0, 0, 1).with_note("FIXTURE: a list line wrapped");
                }
            }
            if k + 1 == lines.len() {
                break;
            }
            let before = h.lit_before();
            let (f0, r0) = h.counts();
            h.newline_gap(nl_gap);
            if k + 1 == stale_at {
                h.drop_far_frames = 1;
            }
            h.next_frame();
            h.next_frame();
            let (f1, r1) = h.counts();
            df += f1 - f0;
            dr += r1 - r0;
            let lit_lines: u64 = before.iter().map(|&n| n as u64).sum();
            let all_followed = r1 == r0 && f1 - f0 >= lit_lines;
            let d: Vec<usize> = if all_followed {
                Vec::new()
            } else {
                dark_of(&h.census(), &before)
            };
            if !d.is_empty() {
                notes.push(format!(
                    "nl{}:dark{d:?}(+f{} +r{})",
                    k + 1,
                    f1 - f0,
                    r1 - r0
                ));
            }
            dark += d.len() as u64;
        }
        Outcome::mf(df, dr, dark).with_note(notes.join(" "))
    }

    fn paces() -> Vec<Pace> {
        sweep(
            &[Pace::Flat(16), Pace::Paced, Pace::Pet],
            &[Pace::Flat(16), Pace::Paced],
        )
    }

    pub fn cases(v: &mut Vec<Case>) {
        for cols in [80usize, 91, 120, 200] {
            for key_ms in sweep(&[80u64, 40], &[80]) {
                for pace in paces() {
                    case(
                        v,
                        format!(
                            "b2.census.composer_wraps/cols{cols}/key{key_ms}/{}",
                            pace.tag()
                        ),
                        CO,
                        move || {
                            let mut h = Host::paced(53, cols, key_ms, pace);
                            run(&mut h, PROSE, 0)
                        },
                    );
                }
            }
        }
        for cols in sweep(&[91usize, 120], &[91]) {
            for pause in sweep(&[600u64, 1500, 3000], &[600]) {
                for pace in paces() {
                    case(
                        v,
                        format!(
                            "b2.census.pause_before_each_line/cols{cols}/pause{pause}/{}",
                            pace.tag()
                        ),
                        CO,
                        move || {
                            let mut h = Host::paced(53, cols, 80, pace);
                            run(&mut h, PROSE, pause)
                        },
                    );
                }
            }
        }
        for cols in sweep(&[80usize, 91, 120], &[91]) {
            for key_ms in sweep(&[80u64, 50], &[80]) {
                case(
                    v,
                    format!("b2.census.newline_list/cols{cols}/key{key_ms}"),
                    CO,
                    move || newline_prompt(cols, key_ms, key_ms, 5, 0),
                );
            }
        }
        for stale_at in [1usize, 2] {
            case(
                v,
                format!("b2.census.newline_list_stale_far_read/cols91/stale@{stale_at}"),
                CO,
                move || newline_prompt(91, 80, 80, 3, stale_at),
            );
        }
    }
}

mod b2s {
    use super::*;

    pub struct Host {
        c: Core,
    }
    core_deref!(Host);

    impl Host {
        fn new(rows: usize, cols: usize) -> Self {
            let mut c = Core::new(
                rows,
                cols,
                8,
                16,
                Theme::Tokyo,
                Opts {
                    scroll_seam: true,
                    alt_rebaseline: false,
                    skip_caret: false,
                    honour_sync: false,
                    pane_columns: false,
                },
            );
            c.frame();
            Self { c }
        }

        fn idle(&mut self, ms: u64) {
            let end = self.now + Duration::from_millis(ms);
            while self.now < end {
                self.now += Duration::from_millis(16);
                self.frame();
            }
        }

        fn key(&mut self, ch: char, bytes: &[u8]) {
            self.now += Duration::from_millis(90);
            let class = if ch == ' ' {
                TypedClass::Space
            } else {
                TypedClass::Glyph
            };
            let now = self.now;
            self.glow.note_typed_glyph(now, 1, false, class);
            self.term.process(bytes);
            self.frame();
        }

        fn type_str(&mut self, s: &str) {
            for ch in s.chars() {
                let mut b = [0u8; 4];
                self.key(ch, ch.encode_utf8(&mut b).as_bytes());
            }
        }

        fn program(&mut self, bytes: &[u8]) {
            self.now += Duration::from_millis(16);
            self.term.process(bytes);
            self.frame();
        }
    }

    fn relocated_twice(start: u16, to: [u16; 2]) -> Outcome {
        let mut h = Host::new(24, 80);
        h.term.process(format!("\x1b[{};1H", start + 1).as_bytes());
        h.frame();
        h.type_str("hello world");
        h.idle(200);
        let want: Vec<u16> = (0..11).collect();
        let mut law = Law::default();
        law.eq(
            h.live(start),
            want.clone(),
            "fixture: the band is under `hello world`",
        );
        for (k, row) in to.into_iter().enumerate() {
            let (f0, r0) = h.counts();
            h.program(format!("\x1b[2J\x1b[{};1Hhello world", row + 1).as_bytes());
            h.frame();
            let (f1, r1) = h.counts();
            law.add(f1 - f0, r1 - r0);
            law.eq(
                (f1 - f0, r1 - r0, h.live(row)),
                (11, 0, want.clone()),
                &format!("move {} onto row {row}", k + 1),
            );
            h.idle(150);
        }
        law.done()
    }

    fn keys_between(keys: &str) -> Outcome {
        let mut h = Host::new(24, 80);
        h.term.process(b"\x1b[6;1H");
        h.frame();
        h.type_str("hello world");
        h.idle(200);
        h.program(b"\x1b[2J\x1b[7;1Hhello world");
        h.idle(150);
        h.type_str(keys);
        h.idle(100);
        let (f0, r0) = h.counts();
        h.program(format!("\x1b[2J\x1b[8;1Hhello world{keys}").as_bytes());
        h.frame();
        let (f1, r1) = h.counts();
        let n = 11 + keys.len();
        let mut law = Law::default();
        law.add(f1 - f0, r1 - r0);
        law.eq(
            (r1 - r0, h.live(7).len()),
            (0, n),
            "second move: nothing retired, the whole line lit on row 7",
        );
        law.done()
    }

    fn control_move_back() -> Outcome {
        let mut h = Host::new(24, 80);
        h.term.process(b"\x1b[6;1H");
        h.frame();
        h.type_str("hello world");
        h.idle(200);
        h.program(b"\x1b[2J\x1b[7;1Hhello world");
        h.idle(150);
        let (f0, r0) = h.counts();
        h.program(b"\x1b[2J\x1b[6;1Hhello world");
        h.frame();
        let (f1, r1) = h.counts();
        let mut law = Law::default();
        law.add(f1 - f0, r1 - r0);
        law.eq((f1 - f0, r1 - r0), (11, 0), "a move back follows");
        law.done()
    }

    pub fn cases(v: &mut Vec<Case>) {
        case(v, "b2.second_move.relocated_down_twice/5-6-7", MF, || {
            relocated_twice(5, [6, 7])
        });
        case(v, "b2.second_move.relocated_up_twice/20-19-18", MF, || {
            relocated_twice(20, [19, 18])
        });
        for keys in ["s", "st", "stu"] {
            case(
                v,
                format!("b2.second_move.keys_between_moves/{keys}"),
                MF,
                move || keys_between(keys),
            );
        }
        case(v, "b2.second_move.control_move_back", MF, control_move_back);
    }
}

mod b3 {
    use super::*;

    pub struct Host {
        c: Core,
        frame_ms: u64,
    }
    core_deref!(Host);

    impl Host {
        fn new(rows: usize, cols: usize, frame_ms: u64) -> Self {
            let mut c = Core::new(
                rows,
                cols,
                8,
                16,
                Theme::Tokyo,
                Opts {
                    scroll_seam: true,
                    alt_rebaseline: false,
                    skip_caret: true,
                    honour_sync: true,
                    pane_columns: false,
                },
            );
            c.frame();
            Self { c, frame_ms }
        }

        fn idle(&mut self, ms: u64) {
            let end = self.now + Duration::from_millis(ms);
            while self.now < end {
                let fm = self.frame_ms;
                self.now += Duration::from_millis(fm);
                self.frame();
            }
        }

        fn step(&mut self) {
            let fm = self.frame_ms;
            self.now += Duration::from_millis(fm);
            self.frame();
        }

        fn key(&mut self, ch: char, bytes: &[u8], gap: u64) {
            self.now += Duration::from_millis(gap);
            let class = if ch == ' ' {
                TypedClass::Space
            } else {
                TypedClass::Glyph
            };
            let now = self.now;
            self.glow.note_typed_glyph(now, 1, false, class);
            self.term.process(bytes);
            self.frame();
        }

        fn program(&mut self, bytes: &[u8]) {
            let fm = self.frame_ms;
            self.now += Duration::from_millis(fm);
            self.term.process(bytes);
            self.frame();
        }
    }

    fn draw_box(cols: usize, top: usize, lines: &[String], transcript: usize) -> Vec<u8> {
        let rule = "\u{2500}".repeat(cols);
        let mut s = String::from("\x1b[?2026h\x1b[?25l\x1b[H\x1b[2J");
        for i in 0..transcript {
            s.push_str(&format!(
                "\x1b[{};1H\u{25cf} streamed answer row {i} with some words in it",
                i + 1
            ));
        }
        s.push_str(&format!("\x1b[{};1H{rule}", top + 1));
        for (i, l) in lines.iter().enumerate() {
            let pre = if i == 0 { "\u{276f} " } else { "  " };
            s.push_str(&format!("\x1b[{};1H{pre}{l}", top + 2 + i));
        }
        let n = lines.len().max(1);
        s.push_str(&format!(
            "\x1b[{};1H{rule}\x1b[{};1H  ? for shortcuts\x1b[{};{}H\x1b[?25h\x1b[?2026l",
            top + 2 + n,
            top + 3 + n,
            top + 1 + n,
            3 + lines.last().map_or(0, |l| l.chars().count())
        ));
        s.into_bytes()
    }

    fn type_one_line(h: &mut Host, top: usize, text: &str, gap: u64) {
        for (i, ch) in text.chars().enumerate() {
            let bytes = format!(
                "\x1b[?2026h\x1b[?25l\x1b[{};{}H{ch}\x1b[{};{}H\x1b[?25h\x1b[?2026l",
                top + 2,
                3 + i,
                top + 2,
                4 + i
            );
            h.key(ch, bytes.as_bytes(), gap);
        }
    }

    fn h1_case(moves: &[i32], gap_frames: u32, first_frames: u32, frame_ms: u64) -> Outcome {
        const COLS: usize = 100;
        let text = "fix the flaky test in the witness";
        let n = text.chars().count();
        let mut h = Host::new(40, COLS, frame_ms);
        let mut top = 10usize;
        h.program(&draw_box(COLS, top, &[String::new()], top));
        h.idle(100);
        type_one_line(&mut h, top, text, 90);
        for _ in 0..first_frames {
            h.step();
        }
        let mut bad = Vec::new();
        let (mut df, mut dr) = (0, 0);
        for (k, &d) in moves.iter().enumerate() {
            top = (top as i32 + d) as usize;
            let (f0, r0) = h.counts();
            h.program(&draw_box(COLS, top, &[text.to_string()], top));
            let (f1, r1) = h.counts();
            df += f1 - f0;
            dr += r1 - r0;
            let got = (f1 - f0, r1 - r0, h.live((top + 1) as u16).len());
            if got != (n as u64, 0, n) {
                bad.push((k + 1, got));
            }
            for _ in 0..gap_frames {
                h.step();
            }
        }
        Outcome::mf(df, dr, bad.len() as u64).with_note(if bad.is_empty() {
            String::new()
        } else {
            format!("MISSED (move,(followed,retired,lit))={bad:?}")
        })
    }

    fn wrap(text: &str, w: usize) -> Vec<String> {
        let mut lines = Vec::new();
        let mut cur = String::new();
        for word in text.split(' ') {
            if cur.is_empty() {
                cur = word.to_string();
            } else if cur.chars().count() + 1 + word.chars().count() <= w {
                cur.push(' ');
                cur.push_str(word);
            } else {
                lines.push(std::mem::take(&mut cur));
                cur = word.to_string();
            }
        }
        lines.push(cur);
        lines
    }

    const PROSE: &str = "please fix the flaky test in the witness module and then run every law again so we know the band follows its text when the box moves";

    fn h2_case(cols: usize, moves: &[i32], gap: u64, frame_ms: u64) -> Outcome {
        let text = PROSE;
        let w = cols - 4;
        let mut h = Host::new(40, cols, frame_ms);
        let mut top = 12usize;
        h.program(&draw_box(cols, top, &[String::new()], top));
        h.idle(100);
        let mut typed = String::new();
        for ch in text.chars() {
            typed.push(ch);
            let lines = wrap(&typed, w);
            h.key(ch, &draw_box(cols, top, &lines, top), 90);
        }
        h.idle(150);
        let lines = wrap(text, w);
        let lit_of = |h: &Host, top: usize, i: usize| {
            let row = (top + 1 + i) as u16;
            let n = lines[i].chars().count() as u16;
            h.live(row).iter().filter(|&&c| c >= 2 && c < 2 + n).count()
        };
        let mut report = Vec::new();
        let (mut df, mut dr) = (0, 0);
        for (k, &d) in moves.iter().enumerate() {
            let before: Vec<usize> = (0..lines.len()).map(|i| lit_of(&h, top, i)).collect();
            top = (top as i32 + d) as usize;
            let (f0, r0) = h.counts();
            h.program(&draw_box(cols, top, &lines, top));
            h.step();
            let (f1, r1) = h.counts();
            df += f1 - f0;
            dr += r1 - r0;
            let after: Vec<usize> = (0..lines.len()).map(|i| lit_of(&h, top, i)).collect();
            for i in 0..lines.len() {
                let n = lines[i].chars().count();
                if before[i] * 10 >= n * 9 && after[i] * 10 < n * 9 {
                    report.push(format!("move{}:line{i}:{}->{}", k + 1, before[i], after[i]));
                }
            }
            h.idle(gap);
        }
        Outcome::mf(df, dr, report.len() as u64).with_note(format!(
            "lines={} {}",
            lines.len(),
            report.join(" ")
        ))
    }

    fn h3_case(frame_ms: u64, every: u32, rows_per: usize) -> Outcome {
        const ROWS: usize = 40;
        const COLS: usize = 100;
        let rule = "\u{2500}".repeat(COLS);
        let text = "fix the flaky test in the witness";
        let n = text.chars().count();
        let mut h = Host::new(ROWS, COLS, frame_ms);
        let mut s = String::new();
        for i in 0..(ROWS - 4) {
            s.push_str(&format!("\u{25cf} transcript row {i}\r\n"));
        }
        s.push_str(&format!(
            "{rule}\r\n\u{276f} \r\n{rule}\r\n  ? for shortcuts\x1b[{};3H",
            ROWS - 2
        ));
        h.program(s.as_bytes());
        h.idle(100);
        let text_row = (ROWS - 3) as u16;
        for (i, ch) in text.chars().enumerate() {
            let bytes = format!("\x1b[{};{}H{ch}", ROWS - 2, 3 + i);
            h.key(ch, bytes.as_bytes(), 90);
        }
        h.idle(100);
        let lit0 = h.live(text_row).len();
        let mut trace = Vec::new();
        let (mut df, mut dr) = (0, 0);
        for k in 1..=8usize {
            let (f0, r0) = h.counts();
            let mut streamed = String::new();
            for j in 0..rows_per {
                streamed.push_str(&format!("\u{25cf} streamed row {k}.{j}\x1b[K\r\n"));
            }
            h.program(
                format!(
                    "\x1b[?2026h\x1b[?25l\x1b[{};1H{streamed}{rule}\r\n\u{276f} {text}\x1b[K\r\n{rule}\r\n  ? for shortcuts\x1b[K\x1b[{};{}H\x1b[?25h\x1b[?2026l",
                    ROWS - 3,
                    ROWS - 2,
                    3 + n
                )
                .as_bytes(),
            );
            for _ in 1..every {
                h.step();
            }
            let (f1, r1) = h.counts();
            df += f1 - f0;
            dr += r1 - r0;
            trace.push((f1 - f0, r1 - r0, h.live(text_row).len()));
        }
        h.idle(100);
        let end = h.live(text_row).len();
        let bad = end * 10 < lit0 * 9;
        Outcome::mf(df, dr, u64::from(bad))
            .with_note(format!("lit {lit0}->end {end} trace(f,r,lit)={trace:?}"))
    }

    fn h4_case(frame_ms: u64, every: u32, d: usize) -> Outcome {
        const COLS: usize = 100;
        let text = "fix the flaky test in the witness";
        let n = text.chars().count();
        let mut h = Host::new(60, COLS, frame_ms);
        let mut top = 6usize;
        h.program(&draw_box(COLS, top, &[String::new()], top));
        h.idle(100);
        type_one_line(&mut h, top, text, 90);
        h.idle(100);
        let lit0 = h.live((top + 1) as u16).len();
        let mut trace = Vec::new();
        let (mut df, mut dr) = (0, 0);
        for _ in 0..8 {
            top += d;
            let (f0, r0) = h.counts();
            h.program(&draw_box(COLS, top, &[text.to_string()], top));
            for _ in 1..every {
                h.step();
            }
            let (f1, r1) = h.counts();
            df += f1 - f0;
            dr += r1 - r0;
            trace.push((f1 - f0, r1 - r0, h.live((top + 1) as u16).len()));
        }
        h.idle(100);
        let end = h.live((top + 1) as u16).len();
        let bad = end * 10 < lit0 * 9 || trace.iter().any(|t| t.2 * 10 < n * 9);
        Outcome::mf(df, dr, u64::from(bad))
            .with_note(format!("lit {lit0}->end {end} trace(f,r,lit)={trace:?}"))
    }

    fn h5_case(frame_ms: u64, torn: u32, from: u16, to: u16) -> Outcome {
        let mut h = Host::new(24, 80, frame_ms);
        h.term.process(format!("\x1b[{};1H", from + 1).as_bytes());
        h.frame();
        for ch in "hello world".chars() {
            let mut b = [0u8; 4];
            h.key(ch, ch.encode_utf8(&mut b).as_bytes(), 90);
        }
        h.idle(200);
        let (f0, r0) = h.counts();
        h.program(format!("\x1b[{};1Hhello world", to + 1).as_bytes());
        for _ in 1..torn {
            h.step();
        }
        h.program(format!("\x1b[{};1H\x1b[2K\x1b[{};12H", from + 1, to + 1).as_bytes());
        h.step();
        let (f1, r1) = h.counts();
        let lit = h.live(to).len();
        Outcome::mf(f1 - f0, r1 - r0, u64::from(lit < 10)).with_note(format!("lit={lit}"))
    }

    struct Composer {
        h: Host,
        rows: usize,
        cols: usize,
        lines: Vec<String>,
        cur: String,
        sync: bool,
    }

    impl Composer {
        const PROMPT: &'static str = "\u{276f}\u{a0}";

        fn new(rows: usize, cols: usize, frame_ms: u64, sync: bool) -> Self {
            let mut h = Host::new(rows, cols, frame_ms);
            let rule = "\u{2500}".repeat(cols);
            h.term.process(
                format!(
                    "\x1b[?1049h\x1b[2J\x1b[{};1H{rule}\x1b[{};1H{}\x1b[{};1H{rule}\x1b[{};1H  auto mode on\x1b[{};3H",
                    rows - 3,
                    rows - 2,
                    Self::PROMPT,
                    rows - 1,
                    rows,
                    rows - 2
                )
                .as_bytes(),
            );
            h.frame();
            Self {
                h,
                rows,
                cols,
                lines: Vec::new(),
                cur: String::new(),
                sync,
            }
        }

        fn caret_row1(&self) -> usize {
            self.rows - 2
        }

        fn line_row(&self, i: usize) -> u16 {
            (self.caret_row1() - 1 - self.lines.len() + i) as u16
        }

        fn width(&self) -> usize {
            self.cols - 4
        }

        fn sb(&self) -> &'static str {
            if self.sync { "\x1b[?2026h" } else { "" }
        }

        fn se(&self) -> &'static str {
            if self.sync { "\x1b[?2026l" } else { "" }
        }

        fn repaint_halves(&self) -> (String, String) {
            let top = self.caret_row1() - 1 - self.lines.len();
            let rule = "\u{2500}".repeat(self.cols);
            let mut a = format!("{}\x1b[?25l\x1b[{top};1H{rule}", self.sb());
            for (i, l) in self.lines.iter().enumerate() {
                let pre = if i == 0 { Self::PROMPT } else { "  " };
                a.push_str(&format!("\x1b[{};1H{pre}{l}\x1b[K", top + 1 + i));
            }
            let pre = if self.lines.is_empty() {
                Self::PROMPT
            } else {
                "  "
            };
            let b = format!(
                "\x1b[{};1H{pre}{}\x1b[K\x1b[{};{}H\x1b[?25h{}",
                self.caret_row1(),
                self.cur,
                self.caret_row1(),
                3 + self.cur.chars().count(),
                self.se()
            );
            (a, b)
        }

        fn key(&mut self, ch: char, gap: u64, torn_frames: u32) -> bool {
            let w = self.width();
            let len = self.cur.chars().count();
            self.h.now += Duration::from_millis(gap);
            let class = if ch == ' ' {
                TypedClass::Space
            } else {
                TypedClass::Glyph
            };
            if len < w {
                let c = 3 + len;
                let row = self.caret_row1();
                let bytes = format!(
                    "{}\x1b[?25l\x1b[{row};{c}H{ch}\x1b[{};1H\x1b[{row};{}H\x1b[?25h{}",
                    self.sb(),
                    self.rows,
                    c + 1,
                    self.se()
                );
                self.cur.push(ch);
                let now = self.h.now;
                self.h.glow.note_typed_glyph(now, 1, false, class);
                self.h.term.process(bytes.as_bytes());
                self.h.frame();
                return false;
            }
            let (line, moved) = if ch == ' ' {
                (self.cur.trim_end().to_string(), String::new())
            } else if self.cur.ends_with(' ') {
                (self.cur.trim_end().to_string(), ch.to_string())
            } else if let Some(i) = self.cur.rfind(' ') {
                let mut m = self.cur[i + 1..].to_string();
                m.push(ch);
                (self.cur[..i].trim_end().to_string(), m)
            } else {
                (self.cur.clone(), ch.to_string())
            };
            self.lines.push(line);
            self.cur = moved;
            let (a, b) = self.repaint_halves();
            let now = self.h.now;
            self.h.glow.note_typed_glyph(now, 1, false, class);
            if torn_frames == 0 {
                self.h.term.process(format!("{a}{b}").as_bytes());
                self.h.frame();
            } else {
                self.h.term.process(a.as_bytes());
                self.h.frame();
                for _ in 1..torn_frames {
                    self.h.step();
                }
                let fm = self.h.frame_ms;
                self.h.now += Duration::from_millis(fm);
                self.h.term.process(b.as_bytes());
                self.h.frame();
            }
            true
        }

        fn paste(&mut self, s: &str, gap: u64) {
            self.h.now += Duration::from_millis(gap);
            let c = 3 + self.cur.chars().count();
            let row = self.caret_row1();
            let n = s.chars().count();
            let now = self.h.now;
            self.h
                .glow
                .note_insert_delivered_from(now, now, InsertWidth::Cells(n as u16));
            let bytes = format!(
                "{}\x1b[?25l\x1b[{row};{c}H{s}\x1b[{};1H\x1b[{row};{}H\x1b[?25h{}",
                self.sb(),
                self.rows,
                c + n,
                self.se()
            );
            self.cur.push_str(s);
            self.h.term.process(bytes.as_bytes());
            self.h.frame();
        }

        fn census(&self) -> Vec<(usize, usize)> {
            (0..self.lines.len())
                .map(|i| {
                    let r = self.line_row(i);
                    let n = self.lines[i].chars().count() as u16;
                    let lit = self
                        .h
                        .live(r)
                        .iter()
                        .filter(|&&c| c >= 2 && c < 2 + n)
                        .count();
                    (lit, n as usize)
                })
                .collect()
        }

        fn lit_now(&self) -> Vec<usize> {
            let mut v: Vec<usize> = (0..self.lines.len())
                .map(|i| self.h.live(self.line_row(i)).len())
                .collect();
            v.push(self.h.live((self.caret_row1() - 1) as u16).len());
            v
        }
    }

    fn dark_lines(census: &[(usize, usize)], before: &[usize]) -> Vec<usize> {
        census
            .iter()
            .enumerate()
            .filter(|(i, (lit, n))| {
                *lit * 10 < *n * 9 && before.get(*i).is_some_and(|b| *b * 10 >= *n * 9)
            })
            .map(|(i, _)| i)
            .collect()
    }

    const LONG: &str = "the quick brown fox jumps over the lazy dog while the rainbow kitty trail follows each key we type into this composer so we can watch exactly what happens when the line wraps and then again when it wraps a second time and a third time and a fourth time because a long prompt is what people really type into claude code every single day of the week and the band should follow every line of it up the screen as the box grows taller and taller with each new row of text that we add to it here and then some more words so the box grows past the witness budget of eight rows and keeps growing";

    fn composer_run(c: &mut Composer, text: &str, key_ms: u64, torn: u32) -> Outcome {
        let (mut dark, mut wraps, mut df, mut dr) = (0u64, 0, 0, 0);
        let mut notes = Vec::new();
        for ch in text.chars() {
            let before = c.lit_now();
            let (f0, r0) = c.h.counts();
            if c.key(ch, key_ms, torn) {
                wraps += 1;
                c.h.step();
                c.h.step();
                let (f1, r1) = c.h.counts();
                df += f1 - f0;
                dr += r1 - r0;
                let d = dark_lines(&c.census(), &before);
                if !d.is_empty() {
                    notes.push(format!(
                        "wrap{wraps}:dark{d:?}(+f{} +r{})",
                        f1 - f0,
                        r1 - r0
                    ));
                }
                dark += d.len() as u64;
            }
        }
        Outcome::mf(df, dr, dark).with_note(format!(
            "wraps={wraps} lines={} {}",
            c.lines.len(),
            notes.join(" ")
        ))
    }

    fn h8_case(frame_ms: u64, gap_after_paste: u64, typed_first: usize) -> Outcome {
        let mut c = Composer::new(53, 91, frame_ms, true);
        let w = c.width();
        let head: String = "abcde fghij klmno pqrst uvwxy"
            .chars()
            .cycle()
            .take(typed_first)
            .collect();
        for ch in head.chars() {
            c.key(ch, 90, 0);
        }
        let fill: String = "pasted words from the clipboard go here and here and here and more"
            .chars()
            .cycle()
            .take(w - typed_first)
            .collect();
        c.paste(&fill, 90);
        let before = c.lit_now();
        let (f0, r0) = c.h.counts();
        let wrapped = c.key('x', gap_after_paste, 0);
        c.h.step();
        c.h.step();
        let (f1, r1) = c.h.counts();
        let census = c.census();
        let bad = !dark_lines(&census, &before).is_empty();
        Outcome::mf(f1 - f0, r1 - r0, u64::from(bad)).with_note(format!(
            "wrapped={wrapped} before={before:?} census={census:?}"
        ))
    }

    fn h8b_case(frame_ms: u64, between: u32) -> Outcome {
        let mut c = Composer::new(53, 91, frame_ms, true);
        let w = c.width();
        let fill: String = "pasted words from the clipboard go here and here and here and more"
            .chars()
            .cycle()
            .take(w)
            .collect();
        c.paste(&fill, 90);
        for _ in 0..between {
            c.h.step();
        }
        let before = c.lit_now();
        let (f0, r0) = c.h.counts();
        let wrapped = c.key('x', frame_ms, 0);
        c.h.step();
        let (f1, r1) = c.h.counts();
        let census = c.census();
        let bad = !dark_lines(&census, &before).is_empty();
        Outcome::mf(f1 - f0, r1 - r0, u64::from(bad)).with_note(format!(
            "wrapped={wrapped} before={before:?} census={census:?}"
        ))
    }

    fn diag_case(typed_first: usize) -> Outcome {
        let frame_ms = 16u64;
        let mut c = Composer::new(53, 91, frame_ms, true);
        let w = c.width();
        let head: String = "abcde".chars().take(typed_first).collect();
        for ch in head.chars() {
            c.key(ch, 90, 0);
        }
        let fill: String = "pasted words from the clipboard go here and here and here and more"
            .chars()
            .cycle()
            .take(w - typed_first)
            .collect();
        c.paste(&fill, 90);
        for _ in 0..4 {
            c.h.step();
        }
        let before = c.lit_now();
        let (f0, r0) = c.h.counts();
        let wrapped = c.key('x', 30, 0);
        c.h.step();
        let (f1, r1) = c.h.counts();
        let census = c.census();
        let bad = !dark_lines(&census, &before).is_empty();
        Outcome::mf(f1 - f0, r1 - r0, u64::from(bad)).with_note(format!(
            "wrapped={wrapped} before={before:?} census={census:?}"
        ))
    }

    fn h9_case(frame_ms: u64, history_every: usize) -> Outcome {
        const ROWS: usize = 40;
        const COLS: usize = 90;
        let w = COLS - 6;
        let mut h = Host::new(ROWS, COLS, frame_ms);
        let mut lines: Vec<String> = vec![String::new()];
        let mut vtop = ROWS - 4;
        let draw = |lines: &[String], vtop: usize| -> String {
            let mut s = format!("\x1b[?2026h\x1b[?25l\x1b[{};1H\x1b[J", vtop + 1);
            s.push_str(&format!("\x1b[{};1H\u{258c}", vtop + 1));
            for (i, l) in lines.iter().enumerate() {
                s.push_str(&format!("\x1b[{};1H\u{258c} {l}", vtop + 2 + i));
            }
            s.push_str(&format!(
                "\x1b[{};1H\u{258c}\x1b[{};1H  \u{23ce} send   \u{21e7}\u{23ce} newline\x1b[{};{}H\x1b[?25h\x1b[?2026l",
                vtop + 2 + lines.len(),
                vtop + 3 + lines.len(),
                vtop + 1 + lines.len(),
                3 + lines.last().unwrap().chars().count()
            ));
            s
        };
        let mut hist = String::new();
        for i in 0..vtop {
            hist.push_str(&format!("history line {i}\r\n"));
        }
        h.program(format!("{hist}{}", draw(&lines, vtop)).as_bytes());
        h.idle(100);
        let text: String = LONG.chars().take(260).collect();
        let mut wraps = 0;
        let mut keys = 0usize;
        let mut bad = 0u64;
        let (mut df, mut dr) = (0, 0);
        let mut notes = Vec::new();
        for ch in text.chars() {
            keys += 1;
            if history_every > 0 && keys.is_multiple_of(history_every) {
                h.now += Duration::from_millis(frame_ms);
                h.term.process(
                    format!(
                        "\x1b[?2026h\x1b[1;{}r\x1b[{};1H\n\x1b[{};1Hhistory streamed {keys}\x1b[r\x1b[{};{}H\x1b[?2026l",
                        vtop,
                        vtop,
                        vtop,
                        vtop + 1 + lines.len(),
                        3 + lines.last().unwrap().chars().count()
                    )
                    .as_bytes(),
                );
                h.frame();
            }
            let cur = lines.last_mut().unwrap();
            let wrap_now = cur.chars().count() >= w && ch != ' ';
            let before: Vec<usize> = (0..lines.len())
                .map(|i| h.live((vtop + 1 + i) as u16).len())
                .collect();
            let (f0, r0) = h.counts();
            h.now += Duration::from_millis(80);
            let now = h.now;
            h.glow.note_typed_glyph(
                now,
                1,
                false,
                if ch == ' ' {
                    TypedClass::Space
                } else {
                    TypedClass::Glyph
                },
            );
            if !wrap_now {
                let cur = lines.last_mut().unwrap();
                let col = 3 + cur.chars().count();
                cur.push(ch);
                let row = vtop + 1 + lines.len();
                h.term.process(
                    format!(
                        "\x1b[?2026h\x1b[?25l\x1b[{row};{col}H{ch}\x1b[{row};{}H\x1b[?25h\x1b[?2026l",
                        col + 1
                    )
                    .as_bytes(),
                );
                h.frame();
                continue;
            }
            let cur = lines.last_mut().unwrap();
            let (keep, moved) = match cur.rfind(' ') {
                Some(i) => {
                    let mut m = cur[i + 1..].to_string();
                    m.push(ch);
                    (cur[..i].to_string(), m)
                }
                None => (cur.clone(), ch.to_string()),
            };
            *cur = keep;
            lines.push(moved);
            wraps += 1;
            let s = format!(
                "\x1b[?2026h\x1b[1;{}r\x1b[{};1H\n\x1b[r{}",
                vtop,
                vtop,
                draw(&lines, vtop - 1).trim_start_matches("\x1b[?2026h")
            );
            vtop -= 1;
            h.term.process(s.as_bytes());
            h.frame();
            h.step();
            h.step();
            let (f1, r1) = h.counts();
            df += f1 - f0;
            dr += r1 - r0;
            for (i, b) in before.iter().enumerate() {
                let n = lines[i].chars().count();
                let lit = h
                    .live((vtop + 1 + i) as u16)
                    .iter()
                    .filter(|&&c| c >= 2 && c < 2 + n as u16)
                    .count();
                if *b * 10 >= n * 9 && lit * 10 < n * 9 {
                    bad += 1;
                    notes.push(format!("wrap{wraps}:line{i}:{b}->{lit}"));
                }
            }
        }
        Outcome::mf(df, dr, bad).with_note(format!("wraps={wraps} {}", notes.join(" ")))
    }

    const TALL_COLS: usize = 80;
    const TALL_TOP: usize = 14;

    /// A prompt of `nlines` short lines in a box at [`TALL_TOP`], each typed `key_ms` a key after a
    /// Shift+Enter `line_ms` after the last, 100 ms idle after the last key.
    fn tall_prompt(nlines: usize, frame_ms: u64, key_ms: u64, line_ms: u64) -> (Host, Vec<String>) {
        let words = [
            "fix it", "test it", "run it", "ship it", "log it", "tag it", "see it", "say it",
        ];
        let mut h = Host::new(50, TALL_COLS, frame_ms);
        let top = TALL_TOP;
        let mut lines: Vec<String> = vec![String::new()];
        h.program(&draw_box(TALL_COLS, top, &lines, top));
        h.idle(100);
        for (k, word) in words.iter().enumerate().take(nlines) {
            if k > 0 {
                lines.push(String::new());
                h.now += Duration::from_millis(line_ms);
                let now = h.now;
                h.glow.note_newline_break(now);
                h.glow.note_typed_glyph(now, 1, false, TypedClass::Glyph);
                h.term.process(&draw_box(TALL_COLS, top, &lines, top));
                h.frame();
            }
            for ch in word.chars() {
                lines.last_mut().unwrap().push(ch);
                h.key(ch, &draw_box(TALL_COLS, top, &lines, top), key_ms);
            }
        }
        h.idle(100);
        (h, lines)
    }

    /// Live cells over line `i`'s glyphs, the box at `top`.
    fn tall_lit(h: &Host, top: usize, i: usize, lines: &[String]) -> usize {
        let row = (top + 1 + i) as u16;
        let n = lines[i].chars().count() as u16;
        h.live(row).iter().filter(|&&c| c >= 2 && c < 2 + n).count()
    }

    fn h10_case(nlines: usize, moves: &[i32], gap: u64, frame_ms: u64) -> Outcome {
        const COLS: usize = TALL_COLS;
        let (mut h, lines) = tall_prompt(nlines, frame_ms, 70, 80);
        let mut top = TALL_TOP;
        let lit0: Vec<usize> = (0..lines.len())
            .map(|i| tall_lit(&h, top, i, &lines))
            .collect();
        let mut rep = Vec::new();
        let (mut df, mut dr) = (0, 0);
        for (k, &d) in moves.iter().enumerate() {
            let before: Vec<usize> = (0..lines.len())
                .map(|i| tall_lit(&h, top, i, &lines))
                .collect();
            top = (top as i32 + d) as usize;
            let (f0, r0) = h.counts();
            h.program(&draw_box(COLS, top, &lines, top));
            h.step();
            let (f1, r1) = h.counts();
            df += f1 - f0;
            dr += r1 - r0;
            let after: Vec<usize> = (0..lines.len())
                .map(|i| tall_lit(&h, top, i, &lines))
                .collect();
            for i in 0..lines.len() {
                let n = lines[i].chars().count();
                if before[i] * 10 >= n * 9 && after[i] * 10 < n * 9 {
                    rep.push(format!("move{}:line{i}:{}->{}", k + 1, before[i], after[i]));
                }
            }
            h.idle(gap);
        }
        Outcome::mf(df, dr, rep.len() as u64)
            .with_note(format!("lit_at_start={lit0:?} {}", rep.join(" ")))
    }

    /// The tall cohort: a prompt typed fast enough (10 ms a key, 20 ms a Shift+Enter) that every
    /// line is lit, moved once `d` rows, past the follow offsets. A line counts as dark when the
    /// move leaves it unlit while a twin redrawn in place keeps it lit.
    fn h11_case(nlines: usize, d: i32) -> Outcome {
        let (mut h, lines) = tall_prompt(nlines, 16, 10, 20);
        let (mut twin, _) = tall_prompt(nlines, 16, 10, 20);
        let top = TALL_TOP;
        let to = (top as i32 + d) as usize;
        let lit = |h: &Host, top: usize| -> Vec<usize> {
            (0..lines.len())
                .map(|i| tall_lit(h, top, i, &lines))
                .collect()
        };
        let before = lit(&h, top);
        let (f0, r0) = h.counts();
        h.program(&draw_box(TALL_COLS, to, &lines, to));
        h.step();
        twin.program(&draw_box(TALL_COLS, top, &lines, top));
        twin.step();
        let (f1, r1) = h.counts();
        let (moved, stayed) = (lit(&h, to), lit(&twin, top));
        let whole = |k: usize, i: usize| k * 10 >= lines[i].chars().count() * 9;
        let mut rep: Vec<String> = (0..lines.len())
            .filter(|&i| !whole(stayed[i], i) || !whole(moved[i], i))
            .map(|i| format!("line{i}:{}->{}", before[i], moved[i]))
            .collect();
        let tall = (0..lines.len()).filter(|&i| whole(stayed[i], i)).count();
        if tall < lines.len() {
            rep.push(format!(
                "premise: the twin lights {tall} of {} lines",
                lines.len()
            ));
        }
        Outcome::mf(f1 - f0, r1 - r0, rep.len() as u64).with_note(rep.join(" "))
    }

    pub fn cases(v: &mut Vec<Case>) {
        let seqs: [&'static [i32]; 12] = [
            &[1, 1],
            &[1, 2],
            &[2, 1],
            &[2, 2],
            &[-1, -1],
            &[-1, -2],
            &[-2, -1],
            &[-2, -2],
            &[1, -1],
            &[-1, 1],
            &[1, 1, 1, 1, 1],
            &[2, 2, 2],
        ];
        for frame_ms in sweep(&[16u64, 8], &[16]) {
            for gap_frames in [0u32, 1, 2] {
                for seq in seqs {
                    case(
                        v,
                        format!(
                            "b3.h1.moves_few_frames_apart/fr{frame_ms}/gapf{gap_frames}/m{}",
                            istr(seq)
                        ),
                        MF,
                        move || h1_case(seq, gap_frames, 1, frame_ms),
                    );
                }
            }
        }
        for frame_ms in sweep(&[16u64, 8], &[8]) {
            for first in [0u32, 1, 2, 3] {
                for d in [1i32, -1, 2, -2] {
                    case(
                        v,
                        format!(
                            "b3.h1b.first_move_after_last_key/fr{frame_ms}/after{first}/dr{d:+}"
                        ),
                        MF,
                        move || h1_case(&[d], 0, first, frame_ms),
                    );
                }
            }
        }
        let m2: [&'static [i32]; 11] = [
            &[1],
            &[2],
            &[-1],
            &[-2],
            &[1, 1],
            &[1, 2],
            &[2, 1],
            &[2, 2],
            &[-1, -2],
            &[-2, -1],
            &[1, 1, 1],
        ];
        for cols in [48usize, 72] {
            for moves in m2 {
                for gap in sweep(&[60u64, 150, 300], &[60, 300]) {
                    case(
                        v,
                        format!(
                            "b3.h2.multiline_inline_box_pushed/cols{cols}/m{}/gap{gap}",
                            istr(moves)
                        ),
                        MF,
                        move || h2_case(cols, moves, gap, 16),
                    );
                }
            }
        }
        for frame_ms in [16u64, 8] {
            for every in [1u32, 2, 3] {
                for rows_per in [1usize, 2] {
                    case(
                        v,
                        format!("b3.h3.bottom_streaming/fr{frame_ms}/every{every}/rows{rows_per}"),
                        MF,
                        move || h3_case(frame_ms, every, rows_per),
                    );
                }
            }
        }
        for frame_ms in [16u64, 8] {
            for every in [1u32, 2, 3] {
                for d in [1usize, 2, 3, 4] {
                    case(
                        v,
                        format!("b3.h4.midscreen_streaming/fr{frame_ms}/every{every}/dr{d}"),
                        MF,
                        move || h4_case(frame_ms, every, d),
                    );
                }
            }
        }
        for frame_ms in [16u64, 8] {
            for torn in [1u32, 2, 3] {
                for (from, to) in [(5u16, 6u16), (20, 19), (5, 7), (20, 18)] {
                    case(
                        v,
                        format!("b3.h5.torn_repaint/fr{frame_ms}/torn{torn}/{from}-{to}"),
                        MF,
                        move || h5_case(frame_ms, torn, from, to),
                    );
                }
            }
        }
        for cols in sweep(&[60usize, 80], &[60]) {
            for frame_ms in [16u64, 8] {
                for key_ms in sweep(&[80u64, 40], &[80]) {
                    case(
                        v,
                        format!("b3.h6.tall_composer/cols{cols}/fr{frame_ms}/key{key_ms}"),
                        CO,
                        move || {
                            let mut c = Composer::new(53, cols, frame_ms, true);
                            composer_run(&mut c, LONG, key_ms, 0)
                        },
                    );
                }
            }
        }
        for frame_ms in sweep(&[16u64, 8], &[16]) {
            for torn in sweep(&[0u32, 1, 2, 3], &[0, 3]) {
                case(
                    v,
                    format!("b3.h7.unsynced_composer_torn_wrap/fr{frame_ms}/torn{torn}"),
                    CO,
                    move || {
                        let mut c = Composer::new(53, 91, frame_ms, false);
                        c.h.o.honour_sync = false;
                        let text: String = LONG.chars().take(300).collect();
                        composer_run(&mut c, &text, 80, torn)
                    },
                );
            }
        }
        for frame_ms in [16u64, 8] {
            for gap_after_paste in [frame_ms, 2 * frame_ms, 3 * frame_ms, 90] {
                for typed_first in [0usize, 5, 20] {
                    case(
                        v,
                        format!(
                            "b3.h8.paste_then_wrap/fr{frame_ms}/gap{gap_after_paste}/typed{typed_first}"
                        ),
                        CO,
                        move || h8_case(frame_ms, gap_after_paste, typed_first),
                    );
                }
            }
        }
        for frame_ms in [16u64, 8] {
            for between in [0u32, 1, 2] {
                case(
                    v,
                    format!("b3.h8b.paste_into_empty_then_wrap/fr{frame_ms}/between{between}"),
                    CO,
                    move || h8b_case(frame_ms, between),
                );
            }
        }
        for typed_first in [0usize, 5] {
            case(
                v,
                format!("b3.diag.paste_4frames_then_wrap/fr16/typed{typed_first}"),
                CO,
                move || diag_case(typed_first),
            );
        }
        for frame_ms in sweep(&[16u64, 8], &[8]) {
            for history_every in sweep(&[0usize, 7, 3], &[0, 3]) {
                case(
                    v,
                    format!("b3.h9.codex_viewport_growth/fr{frame_ms}/hist{history_every}"),
                    CO,
                    move || h9_case(frame_ms, history_every),
                );
            }
        }
        let m10: [&'static [i32]; 12] = [
            &[1, 2],
            &[2, 2],
            &[2, 1],
            &[1, 1],
            &[-1, -2],
            &[-2, -2],
            &[2],
            &[-2],
            &[3],
            &[-3],
            &[4],
            &[-4],
        ];
        for nlines in [3usize, 5, 6, 7] {
            for moves in m10 {
                for gap in [60u64, 150] {
                    case(
                        v,
                        format!(
                            "b3.h10.tall_lit_prompt_moved/lines{nlines}/m{}/gap{gap}",
                            istr(moves)
                        ),
                        MF,
                        move || h10_case(nlines, moves, gap, 16),
                    );
                }
            }
        }
        for nlines in [2usize, 3, 4, 5, 6] {
            for d in [3i32, -3, 4, -4, 5, -5] {
                case(
                    v,
                    format!("b3.h11.tall_cohort_moved_far/lines{nlines}/dr{d:+}"),
                    MF,
                    move || h11_case(nlines, d),
                );
            }
        }
    }
}

mod d4 {
    use super::*;

    fn t1(pace: Pace, text: &'static str, from: u16, to: u16, torn: u64) -> Outcome {
        let mut h = PacedHost::new(24, 80, pace, false);
        h.land(format!("\x1b[{};1H", from + 1).as_bytes());
        h.type_plain(text, 90);
        h.advance(200);
        let n = text.chars().count();
        let (f0, r0) = h.counts();
        h.land(format!("\x1b[{};1H{text}\x1b[{};{}H", to + 1, from + 1, n + 1).as_bytes());
        h.advance(torn);
        h.land(format!("\x1b[{};1H\x1b[2K\x1b[{};{}H", from + 1, to + 1, n + 1).as_bytes());
        h.advance(40);
        let (f1, r1) = h.counts();
        let lit = h.live(to).len();
        Outcome::mf(f1 - f0, r1 - r0, u64::from(lit * 10 < n * 9))
            .with_note(format!("lit={lit}/{n}"))
    }

    fn t1p(pace: Pace, from: u16, to: u16, torn: u64, phase: u64, caret_first: bool) -> Outcome {
        let text = "hello world";
        let mut h = PacedHost::new(24, 80, pace, false);
        h.land(format!("\x1b[{};1H", from + 1).as_bytes());
        h.type_plain(text, 90);
        h.advance(1500 + phase);
        let n = text.chars().count();
        let (f0, r0) = h.counts();
        let caret_row = if caret_first { to } else { from };
        h.land(format!("\x1b[{};1H{text}\x1b[{};{}H", to + 1, caret_row + 1, n + 1).as_bytes());
        h.advance(torn);
        h.land(format!("\x1b[{};1H\x1b[2K\x1b[{};{}H", from + 1, to + 1, n + 1).as_bytes());
        h.advance(40);
        let (f1, r1) = h.counts();
        let lit = h.live(to).len();
        Outcome::mf(f1 - f0, r1 - r0, u64::from(lit * 10 < n * 9))
            .with_note(format!("lit={lit}/{n}"))
    }

    struct Composer {
        h: PacedHost,
        rows: usize,
        cols: usize,
        lines: Vec<String>,
        cur: String,
    }

    impl Composer {
        const PROMPT: &'static str = "\u{276f}\u{a0}";

        fn new(rows: usize, cols: usize, pace: Pace) -> Self {
            let mut h = PacedHost::new(rows, cols, pace, false);
            let rule = "\u{2500}".repeat(cols);
            h.land(
                format!(
                    "\x1b[?1049h\x1b[2J\x1b[{};1H{rule}\x1b[{};1H{}\x1b[{};1H{rule}\x1b[{};1H  auto mode on\x1b[{};3H",
                    rows - 3,
                    rows - 2,
                    Self::PROMPT,
                    rows - 1,
                    rows,
                    rows - 2
                )
                .as_bytes(),
            );
            Self {
                h,
                rows,
                cols,
                lines: Vec::new(),
                cur: String::new(),
            }
        }

        fn caret_row1(&self) -> usize {
            self.rows - 2
        }

        fn line_row(&self, i: usize) -> u16 {
            (self.caret_row1() - 1 - self.lines.len() + i) as u16
        }

        fn repaint_halves(&self) -> (String, String) {
            let top = self.caret_row1() - 1 - self.lines.len();
            let rule = "\u{2500}".repeat(self.cols);
            let mut a = format!("\x1b[?25l\x1b[{top};1H{rule}");
            for (i, l) in self.lines.iter().enumerate() {
                let pre = if i == 0 { Self::PROMPT } else { "  " };
                a.push_str(&format!("\x1b[{};1H{pre}{l}\x1b[K", top + 1 + i));
            }
            let pre = if self.lines.is_empty() {
                Self::PROMPT
            } else {
                "  "
            };
            let b = format!(
                "\x1b[{};1H{pre}{}\x1b[K\x1b[{};{}H\x1b[?25h",
                self.caret_row1(),
                self.cur,
                self.caret_row1(),
                3 + self.cur.chars().count(),
            );
            (a, b)
        }

        fn key(&mut self, ch: char, gap: u64, torn_ms: u64) -> bool {
            let w = self.cols - 4;
            let len = self.cur.chars().count();
            self.h.advance(gap.saturating_sub(2));
            let class = if ch == ' ' {
                TypedClass::Space
            } else {
                TypedClass::Glyph
            };
            let now = self.h.now;
            self.h.glow.note_typed_glyph(now, 1, false, class);
            self.h.advance(2);
            if len < w {
                let c = 3 + len;
                let row = self.caret_row1();
                let bytes = format!(
                    "\x1b[?25l\x1b[{row};{c}H{ch}\x1b[{};1H\x1b[{row};{}H\x1b[?25h",
                    self.rows,
                    c + 1
                );
                self.cur.push(ch);
                self.h.land(bytes.as_bytes());
                return false;
            }
            let (line, moved) = if ch == ' ' {
                (self.cur.trim_end().to_string(), String::new())
            } else if self.cur.ends_with(' ') {
                (self.cur.trim_end().to_string(), ch.to_string())
            } else if let Some(i) = self.cur.rfind(' ') {
                let mut m = self.cur[i + 1..].to_string();
                m.push(ch);
                (self.cur[..i].trim_end().to_string(), m)
            } else {
                (self.cur.clone(), ch.to_string())
            };
            self.lines.push(line);
            self.cur = moved;
            let (a, b) = self.repaint_halves();
            if torn_ms == 0 {
                self.h.land(format!("{a}{b}").as_bytes());
            } else {
                self.h.land(a.as_bytes());
                self.h.advance(torn_ms);
                self.h.land(b.as_bytes());
            }
            true
        }

        fn census(&self) -> Vec<(usize, usize)> {
            (0..self.lines.len())
                .map(|i| {
                    let n = self.lines[i].chars().count() as u16;
                    let lit = self
                        .h
                        .live(self.line_row(i))
                        .iter()
                        .filter(|&&c| c >= 2 && c < 2 + n)
                        .count();
                    (lit, n as usize)
                })
                .collect()
        }

        fn lit_now(&self) -> Vec<usize> {
            let mut v: Vec<usize> = (0..self.lines.len())
                .map(|i| self.h.live(self.line_row(i)).len())
                .collect();
            v.push(self.h.live((self.caret_row1() - 1) as u16).len());
            v
        }
    }

    const LONG: &str = "the quick brown fox jumps over the lazy dog while the rainbow kitty trail follows each key we type into this composer so we can watch exactly what happens when the line wraps and then again when it wraps a second time and a third time";

    fn t2(pace: Pace, key_ms: u64, torn_ms: u64) -> Outcome {
        let mut c = Composer::new(53, 91, pace);
        let (mut dark, mut wraps, mut df, mut dr) = (0u64, 0, 0, 0);
        let mut notes = Vec::new();
        for ch in LONG.chars() {
            let before = c.lit_now();
            let (f0, r0) = c.h.counts();
            if c.key(ch, key_ms, torn_ms) {
                wraps += 1;
                c.h.advance(40);
                let (f1, r1) = c.h.counts();
                df += f1 - f0;
                dr += r1 - r0;
                let d: Vec<usize> = c
                    .census()
                    .iter()
                    .enumerate()
                    .filter(|(i, (lit, n))| {
                        *lit * 10 < *n * 9 && before.get(*i).is_some_and(|b| *b * 10 >= *n * 9)
                    })
                    .map(|(i, _)| i)
                    .collect();
                if !d.is_empty() {
                    notes.push(format!(
                        "wrap{wraps}:dark{d:?}(+f{} +r{})",
                        f1 - f0,
                        r1 - r0
                    ));
                }
                dark += d.len() as u64;
            }
        }
        Outcome::mf(df, dr, dark).with_note(format!(
            "wraps={wraps} lines={} {}",
            c.lines.len(),
            notes.join(" ")
        ))
    }

    fn t3(pace: Pace, torn: u64) -> Outcome {
        let rows = 24usize;
        let text = "hello world";
        let n = text.chars().count();
        let mut h = PacedHost::new(rows, 80, pace, false);
        h.land(format!("\x1b[{rows};1H").as_bytes());
        h.type_plain(text, 90);
        h.advance(200);
        let (f0, r0) = h.counts();
        let erase = format!("\x1b[{};1H\x1b[2K\x1b[{rows};{}H", rows - 1, n + 1);
        if torn == 0 {
            h.land(format!("\r\n{text}{erase}").as_bytes());
        } else {
            h.land(format!("\r\n{text}").as_bytes());
            h.advance(torn);
            h.land(erase.as_bytes());
        }
        h.advance(40);
        let (f1, r1) = h.counts();
        let lit = h.live(rows as u16 - 1).len();
        Outcome::mf(f1 - f0, r1 - r0, u64::from(lit * 10 < n * 9))
            .with_note(format!("lit={lit}/{n}"))
    }

    fn t6(
        pace: Pace,
        text: &'static str,
        output_between: bool,
        rows_down: u16,
        gap: u64,
    ) -> Outcome {
        let mut h = PacedHost::new(24, 80, pace, false);
        if output_between {
            h.land(format!("\x1b[9;1H$ {text}\r\n{text}-output\r\n$ ").as_bytes());
        } else {
            h.land(format!("\x1b[10;1H$ {text}\r\n$ ").as_bytes());
        }
        h.advance(100);
        h.type_plain(text, 110);
        h.advance(gap);
        let n = text.chars().count();
        let (f0, r0) = h.counts();
        let mut notice = String::new();
        for i in 0..rows_down {
            notice += &format!("[{}]  + done       sleep {}\r\n", i + 1, i + 1);
        }
        h.land(format!("\r\x1b[J{notice}$ {text}").as_bytes());
        h.advance(40);
        let (f1, r1) = h.counts();
        let lit = h
            .live(10 + rows_down)
            .iter()
            .filter(|&&c| c >= 2 && c < 2 + n as u16)
            .count();
        Outcome::mf(f1 - f0, r1 - r0, u64::from(lit * 10 < n * 9))
            .with_note(format!("lit={lit}/{n}"))
    }

    fn t8(pace: Pace, stand: u64) -> Outcome {
        let text = "hello world";
        let n = text.chars().count();
        let mut h = PacedHost::new(24, 80, pace, false);
        h.land(b"\x1b[11;1H");
        h.type_plain(text, 90);
        h.advance(200);
        h.land(format!("\x1b7\x1b[12;1H{text}\x1b8").as_bytes());
        h.advance(stand);
        h.land(b"\x1b7\x1b[12;1H\x1b[2K\x1b8");
        h.advance(200);
        let (f0, r0) = h.counts();
        h.land(format!("\x1b[11;1H\x1b[2K\x1b[12;1H{text}\x1b[12;{}H", n + 1).as_bytes());
        h.advance(40);
        let (f1, r1) = h.counts();
        let lit = h.live(11).len();
        Outcome::mf(f1 - f0, r1 - r0, u64::from(lit * 10 < n * 9))
            .with_note(format!("lit={lit}/{n}"))
    }

    pub fn cases(v: &mut Vec<Case>) {
        for pace in PACES {
            for stand in sweep(
                &[16u64, 24, 32, 40, 48, 64, 100, 130, 160, 200, 250, 300],
                &[32, 48, 100, 300],
            ) {
                case(
                    v,
                    format!("d4.t8.moved_where_a_copy_stood/{}/stand{stand}", pace.tag()),
                    MF,
                    move || t8(pace, stand),
                );
            }
        }
        for pace in PACES {
            for (from, to) in [(10u16, 9u16), (10, 11), (10, 8), (10, 12)] {
                for torn in sweep(
                    &[
                        8u64, 16, 24, 30, 34, 38, 42, 46, 50, 56, 64, 80, 100, 120, 150, 200, 300,
                    ],
                    &[8, 38, 42, 46, 50, 100, 120],
                ) {
                    case(
                        v,
                        format!("d4.t1.torn_move/{}/{from}-{to}/torn{torn}", pace.tag()),
                        MF,
                        move || t1(pace, "hello world", from, to, torn),
                    );
                }
            }
        }
        for pace in PACES {
            for torn in sweep(
                &[8u64, 16, 24, 32, 40, 48, 56, 64, 80, 100, 130, 200],
                &[32, 40, 48],
            ) {
                for key_ms in sweep(&[80u64, 40], &[80]) {
                    case(
                        v,
                        format!(
                            "d4.t2.unsynced_composer_torn_wrap/{}/key{key_ms}/torn{torn}",
                            pace.tag()
                        ),
                        CO,
                        move || t2(pace, key_ms, torn),
                    );
                }
            }
        }
        for (from, to) in sweep(&[(10u16, 9u16), (10, 11)], &[(10, 9)]) {
            for torn in sweep(&[30u64, 42, 50, 60, 70, 80, 90, 100, 110], &[42, 50]) {
                for phase in sweep(
                    &[0u64, 10, 20, 30, 40, 50, 60, 70, 80, 90, 100],
                    &[0, 50, 100],
                ) {
                    case(
                        v,
                        format!("d4.t1p.torn_move_phase/paced/{from}-{to}/torn{torn}/ph{phase}"),
                        MF,
                        move || t1p(Pace::Paced, from, to, torn, phase, false),
                    );
                }
            }
        }
        for pace in PACES {
            for (from, to) in sweep(&[(10u16, 8u16), (10, 12)], &[(10, 8)]) {
                for torn in sweep(&[16u64, 32, 42, 50, 64, 100], &[32, 42, 50]) {
                    case(
                        v,
                        format!(
                            "d4.t1c.torn_move_caret_first/{}/{from}-{to}/torn{torn}",
                            pace.tag()
                        ),
                        MF,
                        move || t1p(pace, from, to, torn, 0, true),
                    );
                }
            }
        }
        for pace in PACES {
            for torn in [0u64, 8, 16, 24, 32, 48, 100] {
                case(
                    v,
                    format!("d4.t3.scroll_relocation/{}/torn{torn}", pace.tag()),
                    MF,
                    move || t3(pace, torn),
                );
            }
        }
        for pace in PACES {
            for (name, text) in sweep(
                &[("cd", "cd .."), ("gs", "git status"), ("make", "make test")],
                &[("cd", "cd .."), ("gs", "git status")],
            ) {
                for rows in [1u16, 2, 3] {
                    for gap in sweep(&[100u64, 400], &[100]) {
                        case(
                            v,
                            format!(
                                "d4.t6b.identical_above_pushed_down/{}/{name}/rows{rows}/gap{gap}",
                                pace.tag()
                            ),
                            MF,
                            move || t6(pace, text, false, rows, gap),
                        );
                        case(
                            v,
                            format!(
                                "d4.t6.identical_two_above_pushed_down/{}/{name}/rows{rows}/gap{gap}",
                                pace.tag()
                            ),
                            MF,
                            move || t6(pace, text, true, rows, gap),
                        );
                    }
                }
            }
        }
    }
}

fn all_cases() -> Vec<Case> {
    let mut v = Vec::new();
    b2m::cases(&mut v);
    b2s::cases(&mut v);
    b3::cases(&mut v);
    d4::cases(&mut v);
    v
}

#[test]
fn claude_code_s_composer_follows_every_wrap_at_every_width() {
    holds(all_cases(), "b2.census.composer_wraps/");
}

#[test]
fn claude_code_s_composer_follows_after_a_pause_before_each_line() {
    holds(all_cases(), "b2.census.pause_before_each_line/");
}

#[test]
fn claude_code_s_composer_follows_every_shift_enter() {
    holds(all_cases(), "b2.census.newline_list");
}

#[test]
fn a_line_relocated_again_and_again_carries_its_band() {
    holds(all_cases(), "b2.second_move.");
}

#[test]
fn a_box_moved_on_consecutive_frames_follows_every_move() {
    holds_where(all_cases(), |id| {
        id.starts_with("b3.h1.") || id.starts_with("b3.h1b.")
    });
}

fn moves_of(id: &str) -> &str {
    let at = id.find("/m").expect("a move sequence") + "/m".len();
    id[at..].split('/').next().expect("a move sequence")
}

fn num_of(id: &str, key: &str) -> u64 {
    let tag = format!("/{key}");
    let at = id.find(&tag).expect("the segment") + tag.len();
    id[at..]
        .split('/')
        .next()
        .and_then(|n| n.parse().ok())
        .expect("a number")
}

#[test]
fn a_multi_line_inline_box_pushed_by_streamed_rows_follows() {
    holds_but(
        all_cases(),
        |id| id.starts_with("b3.h2.multiline_inline_box_pushed/"),
        |id| id.contains("/cols48/") && moves_of(id).contains("-2"),
    );
}

#[test]
fn a_box_under_streaming_output_keeps_its_band() {
    holds_where(all_cases(), |id| {
        id.starts_with("b3.h3.") || id.starts_with("b3.h4.")
    });
}

#[test]
fn a_torn_repaint_across_up_to_three_frames_follows() {
    holds(all_cases(), "b3.h5.");
}

#[test]
fn tall_and_unsynced_composers_follow_every_wrap() {
    holds_where(all_cases(), |id| {
        ["b3.h6.", "b3.h7.", "b3.h9."]
            .iter()
            .any(|p| id.starts_with(p))
    });
}

#[test]
fn a_paste_then_a_wrap_on_the_next_frame_follows() {
    holds_where(all_cases(), |id| {
        ["b3.h8.", "b3.h8b.", "b3.diag."]
            .iter()
            .any(|p| id.starts_with(p))
    });
}

#[test]
fn a_tall_lit_prompt_moved_carries_every_line_it_can() {
    holds_but(
        all_cases(),
        |id| id.starts_with("b3.h10."),
        |id| {
            let moves = moves_of(id);
            let lines = num_of(id, "lines");
            // A move past the two-row offsets rides the caret's carry, which asks no twin
            // evidence, so the copies of a line's glyphs on the rows beside it hold nothing back.
            let far = moves.len() == 2 && moves[1..].parse::<u32>().is_ok_and(|d| d > 2);
            let two_up_first = moves.starts_with("-2");
            let near_copy = !far && (lines == 6 || (lines == 7 && moves.starts_with("+1")));
            let retract_ends = num_of(id, "gap") == 60 && moves.len() > 2;
            two_up_first || near_copy || retract_ends
        },
    );
}

fn dr_of(id: &str) -> i32 {
    let at = id.find("/dr").expect("a row offset") + "/dr".len();
    id[at..]
        .split('/')
        .next()
        .and_then(|n| n.parse().ok())
        .expect("a row offset")
}

/// The limit: a cohort of four or more lit rows moved past the follow offsets loses the rows
/// whose targets the witness never samples. On the arrival frame `Engine::band_rows` names the
/// focus row, its two neighbours and its lead row (focus + d), then every cohort row, then each
/// cohort row's lead row, and stops at `WITNESS_ROWS` (8); a row whose target is not named is
/// not followed and retracts where it was. Two and three lit rows ride at every distance from ±3
/// to ±5; four ride at +3, -3 and +4, five at +3 only, six at none.
#[test]
fn a_tall_lit_cohort_moved_past_the_follow_offsets_carries_every_row() {
    holds_but(
        all_cases(),
        |id| id.starts_with("b3.h11."),
        |id| {
            let lines = num_of(id, "lines");
            let d = dr_of(id);
            lines >= 4 && !matches!((lines, d), (4, 3 | -3 | 4) | (5, 3))
        },
    );
}

#[test]
fn a_line_under_an_identical_command_pushed_down_by_a_notice_follows() {
    holds_where(all_cases(), |id| {
        id.starts_with("d4.t6b.") || id.starts_with("d4.t6.")
    });
}

fn torn_of(id: &str) -> u64 {
    num_of(id, "torn")
}

fn scroll_tear_read_as_copy(id: &str) -> u64 {
    if id.contains("/fr8/") { 48 } else { 32 }
}

#[test]
fn a_line_relocated_by_a_scroll_follows_until_its_new_row_has_stood_twin_min() {
    holds_but(
        all_cases(),
        |id| id.starts_with("d4.t3."),
        |id| !id.contains("/paced/") && torn_of(id) >= scroll_tear_read_as_copy(id),
    );
}

fn tear_read_as_copy(id: &str) -> u64 {
    if id.contains("/fr8/") { 40 } else { 48 }
}

#[test]
fn the_limit_a_torn_relocation_whose_new_row_stood_twin_min_is_a_copy() {
    holds_but(
        all_cases(),
        |id| {
            ["d4.t1.", "d4.t1p.", "d4.t1c."]
                .iter()
                .any(|p| id.starts_with(p))
        },
        |id| {
            let seen_between =
                id.contains("/10-9/") || id.contains("/10-11/") || id.starts_with("d4.t1c.");
            let threshold = if id.starts_with("d4.t1.torn_move/paced/") {
                120
            } else {
                tear_read_as_copy(id)
            };
            seen_between && torn_of(id) >= threshold
        },
    );
}

#[test]
fn the_limit_an_unsynced_composer_s_wrap_torn_past_twin_min_melts() {
    holds_but(
        all_cases(),
        |id| id.starts_with("d4.t2."),
        |id| torn_of(id) >= tear_read_as_copy(id),
    );
}

fn stand_of(id: &str) -> u64 {
    num_of(id, "stand")
}

#[test]
fn the_limit_a_move_onto_a_row_a_copy_stood_on_for_twin_min_melts() {
    holds_but(
        all_cases(),
        |id| id.starts_with("d4.t8."),
        |id| {
            let threshold = if id.contains("/paced/") {
                STICKY_PACED
            } else {
                tear_read_as_copy(id)
            };
            stand_of(id) >= threshold
        },
    );
}

const STICKY_PACED: u64 = 130;
