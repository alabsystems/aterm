// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A key whose echo rides a relayout is still the hand's key.
//!
//! Claude Code echoes each key in a per-key shape (glyph addressed from home, caret after it,
//! inside `?2026h` / `?25l` … `?25h` / `?2026l`), but a key can instead ride a relayout: a
//! synchronized frame re-laying the whole screen with the bottom-anchored box one row higher (the
//! agent list under it grew a row) and the composer text rewritten there.
//!
//! When the frame train stalls across the relayout, one tick sees the keys before, on and after
//! it as one caret move, `(r, 21) → (r − 1, 25)`. Read against the old row, the move judge
//! (`CursorGlow::judge_observed_caret`) would take the typed re-anchor arm and lay only the
//! landing. The hand riding its line (`CursorGlow::lift_hand_row`, `rk::Engine::lift_hand`) lays
//! the middle key: with a key in flight, a caret shown a row away first asks whether the hand's
//! own line was repainted there with it, and the move is judged as the same-row advance
//! `21 → 25` it is in the text's frame, while the follow pass carries the band up with the hand.
//! With the seam's lift switched off the stalled takes go red; the per-key and keyless takes stay
//! green.
//!
//! Each take feeds an `aterm_core` [`Terminal`], samples rows as `app_render.rs`'s frame hold
//! does and ticks [`CursorGlow`] every 16.7 ms with the app's per-key stamp
//! (`note_typed_expected`). A hole is a dark column strictly between two lit ones in the text
//! row's planned coverage.

use aterm_core::render::GlowQuad;
use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CURSOR_WITNESS_ROWS, CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::rainbow_kitty::TypedClass;
use std::time::{Duration, Instant};

/// A 15×28 device-px cell; a 90×30 pane keeps the take small.
const ROWS: usize = 30;
const COLS: usize = 90;
const CW: usize = 15;
const CH: usize = 28;
const FRAME_US: u64 = 16_667;
/// Within the recorded cadence around the hole, 60–120 ms between keys.
const KEY_MS: u64 = 90;
/// One macOS PTY read.
const PTY_READ: usize = 1024;
/// A column is lit at this planned coverage (the glass census's 20/255).
const LIT_COV: u8 = 20;
const SYNC_BEGIN: &str = "\x1b[?2026h";
const SYNC_END: &str = "\x1b[?2026l";

/// The composer's text row, 1-based, before the relayout; the box sits on
/// the rows around it and the agent list below it.
const TEXT_ROW: usize = 22;
/// The grid column (0-based) of the first text glyph: `❯` + NBSP before it.
const COL0: usize = 2;

/// The recorded line up to the relayout, the key that rode it, and the rest.
const BEFORE: &str = "also in parallel, ch";
const RIDER: char = 'e';
const AFTER: &str = "ck why the ";

/// The transcript above the box — enough rows that the relayout is a
/// multi-KiB frame, as Claude Code's is.
const TRANSCRIPT: [&str; 6] = [
    "⏺ Agent \"Lint pass: the parser crate\" failed: Agent terminated early",
    "  ⎿  Backgrounded agent (↓ to manage · ctrl+o to expand)",
    "⏺ Goal paused · usage limit reached · continues automatically when it resets",
    "✻ Cogitated for 1d 11h 24m · done 4:01 PM · 8 shells still running",
    "❯ keep going, and check the other tab when you can.",
    "✽ Whirlpooling… (37s · ↓ 2.5k tokens · thinking with xhigh effort)",
];

/// The agent list under the box; the relayout adds its last row, which is
/// what lifts the bottom-anchored box one row.
const AGENTS: [&str; 6] = [
    "  ⏺ main",
    "  ◯ general-purpose  Checking fix lines in the parser",
    "  ◯ general-purpose  Merge the two lanes into main",
    "  ◯ general-purpose  Measure the render path, then decide",
    "  ◯ general-purpose  Close the last open review item",
    "  ◯ general-purpose  Read the metadata the importer trusts",
];

fn geom() -> Geom {
    Geom {
        cw: CW,
        ch: CH,
        rows: ROWS,
        cols: COLS,
        origin_x: 0,
        origin_y: 0,
        win_w: (COLS * CW) as u16,
        win_h: (ROWS * CH) as u16,
        head: 0,
    }
}

/// Rainbow kitty at intensity 0.70 on the dark theme.
fn cfg() -> GlowConfig {
    GlowConfig {
        classic_mono: false,
        ribbon_tall: true,
        ribbon_flat: false,
        enabled: true,
        dark_theme: true,
        theme_fg: 0x00D0_D0D0,
        theme_bg: 0x0011_1318,
        style: GlowStyle::RainbowKitty,
        color: 0x0050_FA7B,
        accent: 0x007A_A2F7,
        duration: Duration::from_millis(240),
        length: 18,
        intensity: 0.7,
        audible: true,
        radius: 0.6,
        ring: true,
        beam: false,
        head_dx: 0.5,
    }
}

fn rule() -> String {
    "\u{2500}".repeat(COLS)
}

/// Plan coverage per column of `row` (as `tests/common/mod.rs` `coverage`).
fn coverage(glow: &CursorGlow, row: u16) -> Vec<u8> {
    let mut cov = vec![0u8; COLS];
    let rib = glow.v2_ribbon().expect("rainbow kitty owns the frame");
    for sg in rib.plan_segments() {
        let r = ((sg.spine / CH as f32) - 0.5).floor();
        let c = (sg.x / CW as f32).floor();
        if r >= 0.0 && c >= 0.0 && r as usize == usize::from(row) && (c as usize) < COLS {
            let i = c as usize;
            cov[i] = cov[i].max(sg.cov);
        }
    }
    cov
}

/// Interior dark runs of a coverage lane as `(first col, len)`.
fn holes_of(cov: &[u8]) -> Vec<(usize, usize)> {
    let lit: Vec<usize> = (0..cov.len()).filter(|&i| cov[i] >= LIT_COV).collect();
    let mut holes = Vec::new();
    if lit.len() < 2 {
        return holes;
    }
    let mut start = None;
    for (i, &v) in cov
        .iter()
        .enumerate()
        .take(lit[lit.len() - 1] + 1)
        .skip(lit[0])
    {
        if v >= LIT_COV {
            if let Some(s) = start.take() {
                holes.push((s, i - s));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    holes
}

/// How the relayout frame reaches the terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Delivery {
    /// In one read.
    Whole,
    /// In 1024-byte reads, a frame of the train between each — the capped
    /// `?2026` hold letting the host present mid-frame.
    Reads,
}

/// One frame's census of the text row.
#[derive(Clone, Debug)]
struct Census {
    ms: u64,
    label: String,
    row: u16,
    text: String,
    lit: String,
    holes: Vec<(usize, usize)>,
}

struct Host {
    term: Terminal,
    glow: CursorGlow,
    cfg: GlowConfig,
    t0: Instant,
    now: Instant,
    next_frame: Instant,
    last_key: Instant,
    out: Vec<GlowQuad>,
    row_buf: Vec<char>,
    blink_seen: u64,
    /// The composer's text so far.
    text: String,
    /// The 1-based text row; the relayout moves it up one.
    row: usize,
    /// How many agent rows stand under the box.
    agents: usize,
    label: String,
    census: Vec<Census>,
    /// The frame train is stalled: time passes and keys echo, and no tick
    /// observes any of it until the stall ends.
    stalled: bool,
}

impl Host {
    fn new() -> Self {
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        term.process(b"\x1b[?1049h\x1b[2J\x1b[H");
        let now = Instant::now();
        let mut glow = CursorGlow::default();
        glow.note_pane_columns(0, COLS);
        let mut h = Self {
            term,
            glow,
            cfg: cfg(),
            t0: now,
            now,
            next_frame: now,
            last_key: now,
            out: Vec::new(),
            row_buf: Vec::new(),
            blink_seen: 0,
            text: String::new(),
            row: TEXT_ROW,
            agents: AGENTS.len() - 1,
            label: "start".into(),
            census: Vec::new(),
            stalled: false,
        };
        let screen = h.screen();
        h.term.process(&screen);
        h.frame();
        h
    }

    fn ms(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_millis() as u64
    }

    /// Claude Code's whole screen, bottom-anchored: the transcript, a blank,
    /// the rule, the text row, the rule, the mode row, a blank, the agent
    /// list — each row rewritten in full inside the DEC 2026 bracket, then
    /// the caret after the text.
    fn screen(&self) -> Vec<u8> {
        let mut rows: Vec<String> = TRANSCRIPT.iter().map(|s| (*s).to_string()).collect();
        rows.push(String::new());
        rows.push(rule());
        let text_i = rows.len();
        rows.push(format!("\u{276f}\u{a0}{}", self.text));
        rows.push(rule());
        rows.push("  ⏵⏵ bypass permissions on · 8 shells".to_string());
        rows.push(String::new());
        rows.extend(AGENTS[..self.agents].iter().map(|s| (*s).to_string()));
        let top = self.row - text_i;
        assert!(top >= 1 && top + rows.len() - 1 <= ROWS, "the screen fits");
        let mut s = format!("{SYNC_BEGIN}\x1b[?25l\x1b[H");
        for r in 1..top {
            s.push_str(&format!("\x1b[{r};1H\x1b[K"));
        }
        for (i, r) in rows.iter().enumerate() {
            s.push_str(&format!("\x1b[{};1H{r}\x1b[K", top + i));
        }
        for r in top + rows.len()..=ROWS {
            s.push_str(&format!("\x1b[{r};1H\x1b[K"));
        }
        s.push_str(&format!(
            "\x1b[{ROWS};1H\x1b[{};{}H\x1b[?25h{SYNC_END}",
            self.row,
            COL0 + self.text.chars().count() + 1
        ));
        s.into_bytes()
    }

    /// The frame hold's samples, then the tick, then the text row's census.
    fn frame(&mut self) {
        let c = self.term.cursor();
        let cur = self.term.cursor_visible().then_some((c.row, c.col));
        let epoch = self.term.repaint_blink_epoch();
        if epoch != self.blink_seen {
            self.blink_seen = epoch;
            self.glow.note_repaint_blink(self.now);
        }
        self.glow.note_context(self.term.is_alternate_screen());
        self.term
            .row_cols_into(usize::from(c.row), &mut self.row_buf);
        self.glow.observe_row(c.row, c.col, &self.row_buf, self.now);
        self.glow.observe_ribbon_row(c.row, &self.row_buf);
        let mut rows = [0u16; CURSOR_WITNESS_ROWS];
        let n = self.glow.ribbon_rows(&mut rows);
        for &r in &rows[..n] {
            self.term.row_cols_into(usize::from(r), &mut self.row_buf);
            self.glow.observe_ribbon_row(r, &self.row_buf);
        }
        self.glow
            .tick(cur, self.now, &self.cfg, geom(), &mut self.out);
        self.next_frame = self.now + Duration::from_micros(FRAME_US);
        self.record();
    }

    fn record(&mut self) {
        let row = (self.row - 1) as u16;
        let cov = coverage(&self.glow, row);
        self.term.row_cols_into(usize::from(row), &mut self.row_buf);
        let text: String = self
            .row_buf
            .iter()
            .map(|&c| if c == '\0' { '_' } else { c })
            .collect();
        self.census.push(Census {
            ms: self.ms(),
            label: self.label.clone(),
            row,
            text: text.trim_end().to_string(),
            lit: cov
                .iter()
                .map(|&v| if v >= LIT_COV { '#' } else { '.' })
                .collect(),
            holes: holes_of(&cov),
        });
    }

    fn advance_to(&mut self, t: Instant) {
        if self.stalled {
            self.now = self.now.max(t);
            return;
        }
        while self.next_frame <= t {
            self.now = self.next_frame;
            self.frame();
        }
        self.now = self.now.max(t);
    }

    /// Starve the frame train until [`Host::resume`].
    fn stall(&mut self) {
        self.stalled = true;
    }

    /// The train resumes: the first frame is due at once and observes every
    /// byte the stall held back.
    fn resume(&mut self) {
        self.stalled = false;
        self.next_frame = self.now;
    }

    fn idle(&mut self, ms: u64) {
        let t = self.now + Duration::from_millis(ms);
        self.advance_to(t);
    }

    /// The app_input seam stamps the typed hint at the key, `KEY_MS` after the
    /// last one; the frames until then run first.
    fn press(&mut self, ch: char) {
        let t = self.last_key + Duration::from_millis(KEY_MS);
        self.advance_to(t);
        self.now = t;
        self.last_key = t;
        self.label = format!("key {ch:?}");
        let class = if ch == ' ' {
            TypedClass::Space
        } else {
            TypedClass::Glyph
        };
        // The window's own stamp for a one-cell printable key: the glyph
        // itself rides the press (`app_input`'s `note_typed_expected` arm).
        self.glow.note_typed_expected(t, 1, false, class, ch);
    }

    /// One ordinary key in Claude Code's captured per-key shape: the glyph
    /// addressed from home, the status row touched, the caret put after it. A
    /// Space is a caret move only.
    fn key(&mut self, ch: char) {
        self.press(ch);
        let c = COL0 + self.text.chars().count() + 1;
        let bytes = if ch == ' ' {
            format!(
                "{SYNC_BEGIN}\x1b[?25l\x1b[{};{}H\x1b[?25h{SYNC_END}",
                self.row,
                c + 1
            )
        } else {
            format!(
                "{SYNC_BEGIN}\x1b[?25l\x1b[H\r\x1b[{}C\x1b[{}B{ch}\x1b[{ROWS};1H\x1b[{};{}H\x1b[?25h{SYNC_END}",
                c - 1,
                self.row - 1,
                self.row,
                c + 1
            )
        };
        self.text.push(ch);
        self.term.process(bytes.as_bytes());
    }

    fn type_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.key(ch);
        }
    }

    /// The relayout the key rode: the program answers the key with its whole
    /// screen re-laid (the agent list a row longer, the box one row higher),
    /// the key's glyph in the rewritten text and the caret after it.
    fn relay(&mut self, ch: char, delivery: Delivery) {
        self.press(ch);
        self.text.push(ch);
        self.agents += 1;
        self.row -= 1;
        let bytes = self.screen();
        match delivery {
            Delivery::Whole => self.term.process(&bytes),
            Delivery::Reads => {
                let mut reads = bytes.chunks(PTY_READ).peekable();
                while let Some(read) = reads.next() {
                    self.term.process(read);
                    if reads.peek().is_some() {
                        self.frame();
                        self.now += Duration::from_millis(1);
                    }
                }
            }
        }
        self.label = "after relayout".into();
    }

    /// The first census frame labelled `label`.
    fn first_frame_labelled(&self, label: &str) -> usize {
        self.census
            .iter()
            .position(|c| c.label == label)
            .expect("the take reached the label")
    }

    fn dump(&self, i: usize) -> String {
        let c = &self.census[i];
        let w = (COL0 + self.text.chars().count() + 3).min(COLS);
        format!(
            "  f{i:<4} t={:>5} ms row {:>2} {:<16} holes={:?}\n    text |{}\n    lit  |{}",
            c.ms,
            c.row,
            c.label,
            c.holes,
            c.text.chars().take(w).collect::<String>(),
            c.lit.chars().take(w).collect::<String>()
        )
    }

    fn story(&self, from: usize) -> String {
        let first = self.census[from..]
            .iter()
            .position(|c| !c.holes.is_empty())
            .map(|k| k + from);
        let holed = self.census[from..]
            .iter()
            .filter(|c| !c.holes.is_empty())
            .count();
        let mut s = format!(
            "{} frames since the relayout, {holed} with a hole\n",
            self.census.len() - from
        );
        if let Some(i) = first {
            for k in i.saturating_sub(2)..(i + 3).min(self.census.len()) {
                s.push_str(&self.dump(k));
                s.push('\n');
            }
        }
        s.push_str("  …the last frame:\n");
        s.push_str(&self.dump(self.census.len() - 1));
        s
    }

    /// `trail status`'s `inflight_forgotten=`.
    fn inflight_forgotten(&self) -> u64 {
        self.glow.in_flight_tally().forgotten
    }
}

/// The first census frame at or after `label`, and every frame from it on
/// that reads the relaid text row, must hold the band whole; the rider's own
/// column must be lit on the last frame; and no press the relayout carried
/// may be forgotten as an unexplained hop.
fn assert_whole_from(h: &Host, label: &str, what: &str, forgotten_before: u64) {
    let from = h.first_frame_labelled(label);
    let holed = h.census[from..]
        .iter()
        .any(|c| c.row == (TEXT_ROW - 2) as u16 && !c.holes.is_empty());
    assert!(
        !holed,
        "{what}: the key that rode the relayout left a hole in the band\n{}",
        h.story(from)
    );
    let rider_col = COL0 + BEFORE.chars().count();
    let last = h.census.last().expect("frames ran");
    assert_eq!(
        last.row,
        (TEXT_ROW - 2) as u16,
        "the census reads the relaid text row"
    );
    assert_eq!(
        last.lit.as_bytes()[rider_col],
        b'#',
        "{what}: the column of the key that rode the relayout is dark\n{}",
        h.story(from)
    );
    assert_eq!(
        h.inflight_forgotten(),
        forgotten_before,
        "{what}: a press the relayout carried was forgotten as an unexplained hop"
    );
}

/// Every frame ticks: the key before, the relayout and the keys after are each
/// observed on their own (a one-cell diagonal hop, licensed as the rider's
/// typed advance). A guard: whole with and without the seam's lift.
fn take(delivery: Delivery) {
    let mut h = Host::new();
    h.type_str(BEFORE);
    let forgotten_before = h.inflight_forgotten();
    h.relay(RIDER, delivery);
    h.type_str(AFTER);
    h.idle(200);
    assert_whole_from(
        &h,
        "after relayout",
        &format!("{delivery:?}"),
        forgotten_before,
    );
}

/// The relayout delivered whole, and read in 1024-byte pieces.
#[test]
fn a_key_whose_echo_rides_a_relayout_a_row_up_is_laid_under_its_glyph() {
    take(Delivery::Whole);
    take(Delivery::Reads);
}

/// The frame train stalls from the key before the rider until `after` more
/// keys have echoed on the relaid row, so one tick sees it all as one move
/// from the old row to the new. `before` is how many keys of [`BEFORE`] echo
/// inside the stall.
fn stalled_take(before: usize, after: usize) {
    let mut h = Host::new();
    let n = BEFORE.chars().count();
    h.type_str(&BEFORE.chars().take(n - before).collect::<String>());
    h.idle(100);
    let forgotten_before = h.inflight_forgotten();
    h.stall();
    h.type_str(&BEFORE.chars().skip(n - before).collect::<String>());
    h.relay(RIDER, Delivery::Whole);
    h.type_str(&AFTER.chars().take(after).collect::<String>());
    h.label = "resumed".into();
    h.resume();
    h.type_str(&AFTER.chars().skip(after).collect::<String>());
    h.idle(200);
    assert_whole_from(
        &h,
        "resumed",
        &format!("a stall over {before} key(s), the relayout and {after} key(s) after it"),
        forgotten_before,
    );
}

/// Every stall width around the relayout, from the rider alone to three keys
/// either side: each must lay every glyph typed. `(1, 2)` is the recorded map:
/// the `h` before, the relayout with the `e`, and `ck` after, seen by one tick
/// as `(r, 21) → (r − 1, 25)`; red without the seam's lift.
#[test]
fn every_stall_width_around_the_relayout_lays_every_key() {
    for before in 0..=3 {
        for after in 0..=3 {
            stalled_take(before, after);
        }
    }
}

/// A relayout with no key behind it (the box moves up a row alone) and the
/// hand types on: the band follows its text, nothing is laid for the move,
/// and the next keys join the band with no hole.
#[test]
fn a_keyless_relayout_moves_the_band_and_the_next_keys_join_it() {
    let mut h = Host::new();
    h.type_str(BEFORE);
    h.idle(150);
    let forgotten_before = h.inflight_forgotten();
    // The program re-lays the box a row up on its own; the text is unchanged.
    h.agents += 1;
    h.row -= 1;
    let bytes = h.screen();
    h.term.process(&bytes);
    h.label = "after relayout".into();
    h.idle(150);
    h.type_str(&format!("{RIDER}{AFTER}"));
    h.idle(200);
    assert_whole_from(&h, "after relayout", "a keyless relayout", forgotten_before);
}

/// The band laid before the relayout moved up a row with its text and the caret, so it stays
/// lit and whole, under the stall too.
#[test]
fn the_band_laid_before_the_relayout_stays_lit_under_the_hand() {
    for stalled in [false, true] {
        let mut h = Host::new();
        h.type_str(&BEFORE[..BEFORE.len() - 1]);
        if stalled {
            h.stall();
        }
        h.type_str(&BEFORE[BEFORE.len() - 1..]);
        h.relay(RIDER, Delivery::Whole);
        h.type_str(&AFTER[..2]);
        h.resume();
        h.type_str(&AFTER[2..]);
        // Past `FLOW_TOTAL_S` from the relayout, the hand still typing.
        h.type_str("aterm claude harness ");
        let last = h.census.last().expect("frames ran");
        let lit_before = last.lit.as_bytes()[COL0..COL0 + BEFORE.chars().count()]
            .iter()
            .filter(|&&b| b == b'#')
            .count();
        assert_eq!(
            lit_before,
            BEFORE.chars().count(),
            "stalled={stalled}: the band laid before the relayout went dark under a typing hand\n{}",
            h.dump(h.census.len() - 1)
        );
        assert!(
            last.holes.is_empty(),
            "stalled={stalled}: the relaid line carries a hole\n{}",
            h.dump(h.census.len() - 1)
        );
    }
}
