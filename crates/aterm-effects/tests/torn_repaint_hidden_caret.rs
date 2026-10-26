// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests that torn repaints with a hidden write cursor keep the composer band intact.

use aterm_core::render::GlowQuad;
use aterm_core::terminal::{ClockReading, Terminal};
use aterm_effects::cursor_glow::{AdmissionPhase, CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::host::{hidden_caret_withheld, motion_caret, motion_print_anchor};
use aterm_effects::rainbow_kitty::TypedClass;
use aterm_effects::rainbow_kitty::witness::WITNESS_ROWS;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

const ROWS: usize = 45;
const COLS: usize = 130;
const CW: usize = 8;
const CH: usize = 16;
const ROW: u16 = 40;
const COL0: u16 = 2;
const FRAME_MS: u64 = 16;
const SYNC_HOLD_CAP: Duration = Duration::from_millis(150);

const SYNC_ON: &str = "\x1b[?2026h";
const SYNC_OFF: &str = "\x1b[?2026l";
const HIDE: &str = "\x1b[?25l";
const SHOW: &str = "\x1b[?25h";

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

fn cfg() -> GlowConfig {
    GlowConfig {
        classic_mono: false,
        ribbon_tall: true,
        ribbon_flat: false,
        enabled: true,
        dark_theme: true,
        theme_fg: 0x00C8_D3F5,
        theme_bg: 0x001A_1B26,
        style: GlowStyle::RainbowKitty,
        color: 0x0050_FA7B,
        accent: 0x007A_A2F7,
        duration: Duration::from_millis(240),
        length: 18,
        intensity: 1.0,
        audible: true,
        radius: 0.6,
        ring: true,
        beam: false,
        head_dx: 0.5,
    }
}

fn sync_frame_hold(
    active: bool,
    open_dirty: bool,
    end_seq: u64,
    was_active: bool,
    armed: (Option<Instant>, u64),
    now: Instant,
) -> (Option<Instant>, bool, u64) {
    if !active {
        return (None, false, end_seq);
    }
    if was_active && end_seq != armed.1 {
        return (Some(now + SYNC_HOLD_CAP), open_dirty, end_seq);
    }
    let mut deadline = if was_active {
        armed.0
    } else {
        Some(now + SYNC_HOLD_CAP)
    };
    let hold = deadline.is_some_and(|d| now < d);
    if !hold {
        deadline = None;
    }
    (deadline, hold, end_seq)
}

/// How the host hands the caret to the glow tick.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Contract {
    /// The composed, focus and headless paths and `EffectsPipeline`: a
    /// hidden caret is `None`, and the echo anchor goes through the anchor
    /// law (`motion_print_anchor`).
    Withheld,
    /// A hidden caret is always handed over; `observe_caret_drawn(false)`
    /// says so.
    HandedOver,
    /// The shipping single-pane present: the hidden caret handed over
    /// through `motion_caret` with the program's account of its frame, so
    /// an unfinished frame withholds it, and its echo anchor through
    /// `motion_print_anchor`, which withholds that frame's anchor too.
    SinglePane,
    /// `motion_caret` fed the mode level, which the terminal's timeout
    /// force-clears mid-frame.
    ModeLevel,
    /// [`Contract::SinglePane`]'s caret with the echo anchor handed over
    /// raw, without the anchor law.
    CaretOnly,
}

struct Host {
    term: Terminal,
    glow: CursorGlow,
    cfg: GlowConfig,
    t0: Instant,
    now: Instant,
    out: Vec<GlowQuad>,
    row_buf: Vec<char>,
    blink_seen: u64,
    contract: Contract,
    sync_was_active: bool,
    sync_hold_until: Option<Instant>,
    sync_armed_seq: u64,
    torn_presents: usize,
}

impl Host {
    fn new(contract: Contract) -> Self {
        Self::with_composer(contract, "")
    }

    fn with_composer(contract: Contract, after: &str) -> Self {
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        let now = Instant::now();
        let mut glow = crate::common::quads_glow();
        glow.note_pane_columns(0, COLS);
        term.process(
            format!(
                "\x1b[?1049h\x1b[2J\x1b[{};1H❯\u{a0}{after}\x1b[{};{}H",
                ROW + 1,
                ROW + 1,
                COL0 + 1
            )
            .as_bytes(),
        );
        let sync_armed_seq = term.sync_end_seq();
        let mut h = Self {
            term,
            glow,
            cfg: cfg(),
            t0: now,
            now,
            out: Vec::new(),
            row_buf: Vec::new(),
            blink_seen: 0,
            contract,
            sync_was_active: false,
            sync_hold_until: None,
            sync_armed_seq,
            torn_presents: 0,
        };
        h.frame();
        h
    }

    fn process(&mut self, bytes: &str) {
        self.term.process_at(
            bytes.as_bytes(),
            ClockReading {
                monotonic: self.now,
                wall_ms: None,
            },
        );
    }

    fn frame(&mut self) {
        let c = self.term.cursor();
        let visible = self.term.cursor_visible();
        let caret = Some((c.row, c.col));
        let cur = match self.contract {
            Contract::Withheld => caret.filter(|_| visible),
            Contract::HandedOver => caret,
            Contract::SinglePane | Contract::CaretOnly => {
                motion_caret(caret, visible, hidden_caret_withheld(&self.term))
            }
            Contract::ModeLevel => {
                motion_caret(caret, visible, self.term.modes().synchronized_output())
            }
        };
        let epoch = self.term.repaint_blink_epoch();
        if epoch != self.blink_seen {
            self.blink_seen = epoch;
            self.glow.note_repaint_blink(self.now);
        }
        self.glow.note_context(self.term.is_alternate_screen());
        self.term
            .row_cols_into(usize::from(c.row), &mut self.row_buf);
        self.glow.observe_row(c.row, c.col, &self.row_buf, self.now);
        let (anchor, glyph) = match self.contract {
            Contract::Withheld | Contract::SinglePane => motion_print_anchor(&self.term),
            Contract::HandedOver | Contract::ModeLevel | Contract::CaretOnly => {
                (self.term.print_anchor(), self.term.print_anchor_glyph())
            }
        };
        self.glow.observe_print_anchor_glyph(anchor, glyph);
        if self.contract != Contract::Withheld {
            self.glow.observe_caret_drawn(visible);
        }
        self.glow.observe_ribbon_row(c.row, &self.row_buf);
        let mut rows = [0u16; WITNESS_ROWS];
        let n = self.glow.ribbon_rows(&mut rows);
        for &r in &rows[..n] {
            self.term.row_cols_into(usize::from(r), &mut self.row_buf);
            self.glow.observe_ribbon_row(r, &self.row_buf);
        }
        self.glow
            .tick(cur, self.now, &self.cfg, geom(), &mut self.out);
    }

    fn present(&mut self) {
        let active = self.term.modes().synchronized_output();
        let (deadline, hold, seq) = sync_frame_hold(
            active,
            self.term.sync_open_dirty(),
            self.term.sync_end_seq(),
            self.sync_was_active,
            (self.sync_hold_until, self.sync_armed_seq),
            self.now,
        );
        self.sync_was_active = active;
        self.sync_hold_until = deadline;
        self.sync_armed_seq = seq;
        if hold {
            return;
        }
        if self.term.sync_frame_unfinished() {
            self.torn_presents += 1;
        }
        self.frame();
    }

    fn to(&mut self, ms: u64) {
        let t = self.t0 + Duration::from_millis(ms);
        while self.now + Duration::from_millis(FRAME_MS) <= t {
            self.now += Duration::from_millis(FRAME_MS);
            self.present();
        }
        self.now = self.now.max(t);
    }

    fn key(&mut self, ms: u64, ch: char) {
        self.to(ms);
        let class = if ch == ' ' {
            TypedClass::Space
        } else {
            TypedClass::Glyph
        };
        self.glow.note_typed_glyph(self.now, 1, false, class);
    }

    fn typed(&mut self, ms: u64, ch: char) {
        self.to(ms);
        let class = if ch == ' ' {
            TypedClass::Space
        } else {
            TypedClass::Glyph
        };
        self.glow.note_typed_expected(self.now, 1, false, class, ch);
    }

    fn write(&mut self, ms: u64, bytes: &str) {
        self.to(ms);
        self.process(bytes);
    }

    fn lit(&self, col: u16) -> bool {
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        let owned = rib
            .cells()
            .iter()
            .any(|c| c.row == ROW && c.col == col && !c.leaving());
        let cov = rib.plan_segments().iter().any(|s| {
            ((s.spine / CH as f32) - 0.5).floor() as i64 == i64::from(ROW)
                && (s.x / CW as f32).floor() as i64 == i64::from(col)
                && s.cov > 0
        });
        owned && cov
    }

    fn ring(&self) -> String {
        let mut s = String::new();
        for r in self.glow.admission_log() {
            let at = r.at.saturating_duration_since(self.t0).as_millis();
            let _ = write!(
                s,
                "\n  @{at:>5} {} {} ({},{})->({},{}) {}",
                if r.phase == AdmissionPhase::Declined {
                    "declined"
                } else {
                    "licensed"
                },
                r.reason,
                r.origin.0,
                r.origin.1,
                r.target.0,
                r.target.1,
                r.licence
            );
        }
        s
    }
}

fn letter_frame(col: u16, glyphs: &str, caret: u16) -> String {
    format!(
        "{SYNC_ON}{HIDE}\x1b[H\r\x1b[{col}C\x1b[{ROW}B{glyphs}\x1b[{ROWS};1H\x1b[{};{}H{SHOW}{SYNC_OFF}",
        ROW + 1,
        caret + 1
    )
}

fn space_frame(caret: u16) -> String {
    format!(
        "{SYNC_ON}{HIDE}\x1b[{};{}H{SHOW}{SYNC_OFF}",
        ROW + 1,
        caret + 1
    )
}

fn spinner_frame(glyph: char, caret: u16) -> String {
    format!(
        "{SYNC_ON}{HIDE}\x1b[H\r\x1b[{}B{glyph}\x1b[{ROWS};1H\x1b[{};{}H{SHOW}{SYNC_OFF}",
        ROW - 3,
        ROW + 1,
        caret + 1
    )
}

fn transcript_row(row: u16, text: &str) -> String {
    format!("\x1b[H\r\x1b[{row}B{text}\x1b[K")
}

fn take(contract: Contract, open_ms: u64) -> Host {
    let gap = ((open_ms - 20) / 3).min(100);
    take_shaped(
        contract,
        Shape {
            open_ms,
            keys: [30, 30 + gap, 30 + 2 * gap],
            middle: 40 + open_ms / 2,
        },
    )
}

struct Shape {
    open_ms: u64,
    /// ` `, `b`, `u`.
    keys: [u64; 3],
    /// The middle transcript read.
    middle: u64,
}

fn take_shaped(contract: Contract, shape: Shape) -> Host {
    let Shape {
        open_ms,
        keys: key_ms,
        middle: middle_ms,
    } = shape;
    let mut h = Host::new(contract);
    let typed = "was a";
    let mut t = 100;
    for (i, ch) in typed.chars().enumerate() {
        let col = COL0 + i as u16;
        h.key(t, ch);
        let f = if ch == ' ' {
            space_frame(col + 1)
        } else {
            letter_frame(col, &ch.to_string(), col + 1)
        };
        h.write(t + 20, &f);
        t += 100;
    }
    let caret = COL0 + typed.len() as u16;
    let open = t;
    let keys = [
        (open + key_ms[0], ' '),
        (open + key_ms[1], 'b'),
        (open + key_ms[2], 'u'),
    ];
    let head = format!(
        "{SYNC_ON}{HIDE}{}{}",
        transcript_row(1, "It resumed successfully. The crash report is deleted."),
        transcript_row(18, "The cause is a live bug on the other branch's executor")
    );
    let middle = transcript_row(29, "- Row D: 48 of 155 sampled queries now prove unsat.");
    let close = format!(
        "{}\x1b[{ROWS};1H\x1b[{};{}H{SHOW}{SYNC_OFF}{}{SYNC_ON}{HIDE}\x1b[H\r\x1b[{}B·\r\x1b[{}C\x1b[3B{}\x1b[{ROWS};1H\x1b[{};{}H{SHOW}{SYNC_OFF}",
        transcript_row(33, "  ⎿  Running in background"),
        ROW + 1,
        caret + 1,
        spinner_frame('✢', caret + 1),
        ROW - 3,
        caret + 1,
        "bu",
        ROW + 1,
        caret + 4
    );
    let mut events: Vec<(u64, Option<char>, String)> = keys
        .iter()
        .map(|&(at, ch)| (at, Some(ch), String::new()))
        .collect();
    events.push((open + 40, None, head));
    events.push((open + middle_ms, None, middle));
    events.push((open + 40 + open_ms, None, close));
    events.sort_by_key(|e| e.0);
    for (at, key, bytes) in events {
        match key {
            Some(ch) => h.key(at, ch),
            None => h.write(at, &bytes),
        }
    }
    let after = open + 40 + open_ms;
    h.key(after + 60, 'g');
    h.write(after + 80, &letter_frame(caret + 3, "g", caret + 4));
    h.to(after + 280);
    h
}

fn report(h: &Host) -> (Vec<u16>, String) {
    let line = "was a bug";
    let dark: Vec<u16> = (COL0..COL0 + line.len() as u16)
        .filter(|&c| !h.lit(c))
        .collect();
    let glyphs: Vec<String> = dark
        .iter()
        .map(|&c| {
            format!(
                "{c}:{:?}",
                line.chars().nth(usize::from(c - COL0)).unwrap_or('?')
            )
        })
        .collect();
    (
        dark,
        format!(
            "torn presents={} dark={}{}",
            h.torn_presents,
            glyphs.join(","),
            h.ring()
        ),
    )
}

fn cross_row_licences(h: &Host) -> Vec<String> {
    h.glow
        .admission_log()
        .filter(|r| r.phase != AdmissionPhase::Declined && (r.origin.0 != ROW || r.target.0 != ROW))
        .map(|r| {
            format!(
                "{} ({},{})->({},{})",
                r.reason, r.origin.0, r.origin.1, r.target.0, r.target.1
            )
        })
        .collect()
}

#[test]
fn a_torn_full_screen_repaint_keeps_the_boundary_space_lit() {
    let pre_fix = take(Contract::HandedOver, 280);
    let (dark, rep) = report(&pre_fix);
    assert!(
        pre_fix.torn_presents > 0,
        "the take must present torn: {rep}"
    );
    assert!(!dark.is_empty(), "the gap must reproduce: {rep}");
    assert!(
        !cross_row_licences(&pre_fix).is_empty(),
        "the write cursor must be licensed as the caret: {rep}"
    );

    let h = take(Contract::SinglePane, 280);
    let (dark, rep) = report(&h);
    assert!(h.torn_presents > 0, "the take must present torn: {rep}");
    assert!(dark.is_empty(), "owner's shape: {rep}");
    let off_row = cross_row_licences(&h);
    assert!(
        off_row.is_empty(),
        "licensed off the composer row: {off_row:?}{rep}"
    );
    assert_eq!(
        h.glow.in_flight_tally().park_returns,
        0,
        "the shipping contract never shows the glow the write cursor{rep}"
    );
}

fn outliving_the_timeout() -> Shape {
    Shape {
        open_ms: 1_260,
        keys: [900, 960, 1_020],
        middle: 1_060,
    }
}

#[test]
fn a_torn_repaint_outliving_the_terminal_timeout_keeps_the_line_whole() {
    let withheld = take_shaped(Contract::Withheld, outliving_the_timeout());
    let (dark, rep) = report(&withheld);
    assert!(
        withheld.torn_presents > 0,
        "withheld: must present torn: {rep}"
    );
    assert!(dark.is_empty(), "withheld control: {rep}");

    let mode_level = take_shaped(Contract::ModeLevel, outliving_the_timeout());
    let (dark, rep) = report(&mode_level);
    assert!(
        !dark.is_empty(),
        "negative control: the mode-level reading must gap past the timeout: {rep}"
    );

    let h = take_shaped(Contract::SinglePane, outliving_the_timeout());
    let (dark, rep) = report(&h);
    assert!(h.torn_presents > 0, "the take must present torn: {rep}");
    assert!(dark.is_empty(), "past the timeout: {rep}");
    let off_row = cross_row_licences(&h);
    assert!(
        off_row.is_empty(),
        "licensed off the composer row: {off_row:?}{rep}"
    );
}

#[test]
fn every_bracket_length_keeps_the_line_whole_under_the_shipping_contract() {
    for open_ms in [200_u64, 500, 900, 1_100, 1_500, 2_500] {
        for late in [false, true] {
            let keys = if late {
                let k = open_ms.saturating_sub(360).max(30);
                [k, k + 60, k + 120]
            } else {
                [30, 90, 150]
            };
            let shape = || Shape {
                open_ms,
                keys,
                middle: (keys[2] + 40).min(open_ms),
            };
            let (withheld_dark, _) = report(&take_shaped(Contract::Withheld, shape()));
            let h = take_shaped(Contract::SinglePane, shape());
            let (dark, rep) = report(&h);
            assert_eq!(
                dark, withheld_dark,
                "open {open_ms} ms, late keys {late}: {rep}"
            );
            assert!(
                dark.is_empty(),
                "open {open_ms} ms, late keys {late}: {rep}"
            );
        }
    }
}

const BS: char = '\u{8}';

/// Claude Code's mid-line insert: keys typed in front of [`TAIL`], one key
/// every 110 ms, a Backspace among them as [`BS`].
#[derive(Clone, Copy)]
struct MidLine<'a> {
    keys: &'a str,
    /// Each key's echo latency; echoes queue behind a stalled write.
    lag_ms: u64,
    /// The key whose echo is torn before its parking CUP and finished this
    /// many ms later (Node stalled mid-write).
    tear: Option<(usize, u64)>,
    /// Keys whose echo is not rendered alone: the next key's repaint carries
    /// it (Ink rendering a lagging state).
    merged: &'a [usize],
    /// Claude Code's keyless `ultrathink` shimmer recolouring one tail letter
    /// on the caret row at this period, the caret returned.
    shimmer_ms: Option<u64>,
}

impl MidLine<'_> {
    const PLAIN: MidLine<'static> = MidLine {
        keys: TYPED,
        lag_ms: 12,
        tear: None,
        merged: &[],
        shimmer_ms: None,
    };
}

const TYPED: &str = "all the subtrees";
const TAIL: &str = " carefully and ultrathink design this.";

fn mid_line(contract: Contract, take: MidLine) -> (Host, String) {
    let mut h = Host::with_composer(contract, TAIL);
    let mut line: Vec<char> = TAIL.chars().collect();
    let mut shown = line.clone();
    let mut caret = 0usize;
    let paint = |line: &[char]| line.iter().collect::<String>();
    let echo = |from: usize, line: &[char], caret: usize| {
        format!(
            "{SYNC_ON}{HIDE}\x1b[H\r\x1b[{}C\x1b[{ROW}B{}\x1b[K",
            usize::from(COL0) + from,
            paint(&line[from.min(line.len())..])
        ) + &format!(
            "\x1b[{ROWS};1H\x1b[{};{}H{SHOW}{SYNC_OFF}",
            ROW + 1,
            usize::from(COL0) + caret + 1
        )
    };
    let mut outs: Vec<(u64, String, usize)> = Vec::new();
    let mut keys: Vec<(u64, char)> = Vec::new();
    let mut busy_until = 0u64;
    let mut torn: Option<(u64, u64)> = None;
    for (i, ch) in take.keys.chars().enumerate() {
        let press = 200 + 110 * i as u64;
        keys.push((press, ch));
        if ch == BS {
            caret -= 1;
            line.remove(caret);
        } else {
            line.insert(caret, ch);
            caret += 1;
        }
        if take.merged.contains(&i) {
            continue;
        }
        let from = (0..line.len().max(shown.len()))
            .find(|&c| shown.get(c) != line.get(c))
            .unwrap_or(caret);
        shown.clone_from(&line);
        let full = echo(from, &line, caret);
        let at = (press + take.lag_ms).max(busy_until);
        if let Some((_, stall)) = take.tear.filter(|&(key, _)| key == i) {
            let cut = full.find(&format!("\x1b[{ROWS};1H")).expect("the park");
            outs.push((at, full[..cut].to_string(), caret));
            outs.push((at + stall, full[cut..].to_string(), caret));
            torn = Some((at, at + stall));
            busy_until = at + stall + 1;
        } else {
            outs.push((at, full, caret));
            busy_until = at + 1;
        }
    }
    let last_out = outs.iter().map(|o| o.0).max().unwrap_or(0);
    let end = (keys.last().expect("a key").0 + take.lag_ms).max(last_out) + 400;
    if let Some(period) = take.shimmer_ms {
        let echoes = outs.clone();
        let mut t = 230;
        while t < end {
            let inside = torn.is_some_and(|(a, b)| t >= a && t <= b);
            if !inside {
                let parked = echoes
                    .iter()
                    .filter(|o| o.0 <= t)
                    .map(|o| o.2)
                    .next_back()
                    .unwrap_or(0);
                let col = usize::from(COL0) + parked + 15;
                let bracket = format!(
                    "{SYNC_ON}{HIDE}\x1b[H\r\x1b[{col}C\x1b[{ROW}B\x1b[38;2;250;195;95mu\x1b[39m\x1b[{ROWS};1H\x1b[{};{}H{SHOW}{SYNC_OFF}",
                    ROW + 1,
                    usize::from(COL0) + parked + 1
                );
                outs.push((t, bracket, parked));
            }
            t += period;
        }
    }
    let mut events: Vec<(u64, Option<char>, String)> = keys
        .iter()
        .map(|&(at, ch)| (at, Some(ch), String::new()))
        .chain(outs.into_iter().map(|(at, b, _)| (at, None, b)))
        .collect();
    events.sort_by_key(|e| (e.0, e.1.is_none()));
    for (at, key, bytes) in events {
        match key {
            Some(BS) => {
                h.to(at);
                h.glow.note_backspace_erasing(h.now, Some(1));
            }
            Some(ch) => h.typed(at, ch),
            None => h.write(at, &bytes),
        }
    }
    h.to(end);
    (h, line.iter().collect())
}

fn mid_line_take(contract: Contract, stall_ms: u64, shimmer_ms: Option<u64>) -> (Host, u16) {
    let space = TYPED.find("the ").expect("the Space") + 3;
    let take = MidLine {
        tear: Some((space, stall_ms)),
        shimmer_ms,
        ..MidLine::PLAIN
    };
    (mid_line(contract, take).0, COL0 + space as u16)
}

fn mid_line_report(h: &Host) -> (Vec<u16>, String) {
    mid_line_report_of(h, TYPED)
}

fn mid_line_report_of(h: &Host, typed: &str) -> (Vec<u16>, String) {
    let dark: Vec<u16> = (COL0..COL0 + typed.chars().count() as u16)
        .filter(|&c| !h.lit(c))
        .collect();
    let glyphs: Vec<String> = dark
        .iter()
        .map(|&c| {
            format!(
                "{c}:{:?}",
                typed.chars().nth(usize::from(c - COL0)).unwrap_or('?')
            )
        })
        .collect();
    (
        dark,
        format!(
            "torn presents={} dark={}{}",
            h.torn_presents,
            glyphs.join(","),
            h.ring()
        ),
    )
}

fn line_end_licences(h: &Host) -> Vec<String> {
    let typed_end = COL0 + TYPED.len() as u16;
    h.glow
        .admission_log()
        .filter(|r| {
            r.phase != AdmissionPhase::Declined && r.target.0 == ROW && r.target.1 > typed_end + 1
        })
        .map(|r| {
            format!(
                "{} ({},{})->({},{})",
                r.reason, r.origin.0, r.origin.1, r.target.0, r.target.1
            )
        })
        .collect()
}

#[test]
fn a_raw_echo_anchor_spends_a_torn_space_at_the_line_end() {
    let (h, space) = mid_line_take(Contract::CaretOnly, 200, None);
    let (dark, rep) = mid_line_report(&h);
    assert!(h.torn_presents > 0, "must present torn: {rep}");
    assert_eq!(dark, vec![space], "the owner's hole: {rep}");
    assert!(
        !line_end_licences(&h).is_empty(),
        "spent at the line end: {rep}"
    );

    let (h, space) = mid_line_take(Contract::CaretOnly, 200, Some(130));
    let (dark, rep) = mid_line_report(&h);
    assert_eq!(
        dark.first(),
        Some(&space),
        "shimmer: the Space goes dark: {rep}"
    );
}

#[test]
fn a_torn_mid_line_echo_keeps_the_space_lit() {
    for shimmer in [None, Some(130), Some(50)] {
        for (contract, stall) in [
            (Contract::SinglePane, 200),
            (Contract::SinglePane, 1_200),
            (Contract::SinglePane, 1_500),
            (Contract::SinglePane, 2_000),
            (Contract::SinglePane, 3_000),
            (Contract::Withheld, 200),
            (Contract::Withheld, 2_000),
            (Contract::SinglePane, 60),
        ] {
            let (h, _) = mid_line_take(contract, stall, shimmer);
            let (dark, rep) = mid_line_report(&h);
            if stall > 150 {
                assert!(
                    h.torn_presents > 0,
                    "{contract:?} {stall} ms {shimmer:?}: must present torn: {rep}"
                );
            }
            assert!(
                dark.is_empty(),
                "{contract:?} {stall} ms {shimmer:?}: {rep}"
            );
            let spent = line_end_licences(&h);
            assert!(
                spent.is_empty(),
                "{contract:?} {stall} ms {shimmer:?}: {spent:?}{rep}"
            );
        }
    }
}

fn erase_order_lights(path: &[&str]) -> bool {
    let model = aterm_models_effects::erased_key_echo_order_model();
    let mut state = model.init_state();
    for action in path {
        assert!(model.fire(action, &mut state), "{path:?}: {action}");
    }
    state["lit"] == 1
}

#[test]
fn a_key_erased_before_its_echo_keeps_the_next_key_lit() {
    let erase = "all thes".len();
    let erase_at = Duration::from_millis(200 + 110 * erase as u64);
    let typo = COL0 + erase as u16 - 1;
    for (keys, typed) in [
        ("all thes\u{8} subtrees", TYPED),
        ("all thes\u{8} ", "all the "),
    ] {
        for lag in [12, 100, 200, 300, 900] {
            for merged in [&[][..], &[erase][..]] {
                let take = MidLine {
                    keys,
                    lag_ms: lag,
                    merged,
                    ..MidLine::PLAIN
                };
                let (h, text) = mid_line(Contract::SinglePane, take);
                let (dark, rep) = mid_line_report_of(&h, typed);
                assert!(text.starts_with(typed), "lag {lag} {merged:?}: {text:?}");
                let late = h
                    .glow
                    .admission_log()
                    .find(|r| r.origin == (ROW, typo) && r.target == (ROW, typo + 1))
                    .is_some_and(|r| r.at >= h.t0 + erase_at);
                assert_eq!(
                    late,
                    lag >= 100,
                    "lag {lag} {merged:?}: the typo's echo must land after the Backspace exactly when lagging: {rep}"
                );
                assert!(dark.is_empty(), "{keys:?} lag {lag} {merged:?}: {rep}");
            }
        }
    }
    assert!(erase_order_lights(&[
        "EraseUnechoed",
        "NextKey",
        "LateEcho",
        "Retreat",
        "NextEcho"
    ]));
    assert!(erase_order_lights(&[
        "EraseUnechoed",
        "NextKey",
        "LateEcho",
        "MergedCorrection"
    ]));
}

#[test]
fn every_correction_echoed_late_or_merged_keeps_the_line_lit() {
    for keys in [
        "all thes\u{8}\u{8}e subtrees",
        "all teh\u{8}\u{8}he subtrees",
        "all thexs\u{8}\u{8} subtrees",
        "all thee\u{8}\u{8}e subtrees",
    ] {
        let first = keys.chars().position(|c| c == BS).expect("a Backspace");
        for lag in [12, 200, 500] {
            for merged in [&[][..], &[first, first + 1][..]] {
                let take = MidLine {
                    keys,
                    lag_ms: lag,
                    merged,
                    ..MidLine::PLAIN
                };
                let (h, text) = mid_line(Contract::SinglePane, take);
                let (dark, rep) = mid_line_report(&h);
                assert!(text.starts_with(TYPED), "{keys:?} lag {lag}: {text:?}");
                assert!(dark.is_empty(), "{keys:?} lag {lag} {merged:?}: {rep}");
            }
        }
    }
}

#[test]
fn a_correction_repainted_torn_keeps_its_keys_lit() {
    for keys in ["all thes\u{8} subtrees", "all teh\u{8}\u{8}he subtrees"] {
        let first = keys.chars().position(|c| c == BS).expect("a Backspace");
        let erases = keys.chars().filter(|&c| c == BS).count();
        let merged: Vec<usize> = (first..first + erases).collect();
        for lag in [12, 300] {
            for stall in [200, 600, 1_500] {
                for merged in [&[][..], &merged[..]] {
                    let take = MidLine {
                        keys,
                        lag_ms: lag,
                        merged,
                        tear: Some((first + erases, stall)),
                        ..MidLine::PLAIN
                    };
                    let (h, text) = mid_line(Contract::SinglePane, take);
                    let (dark, rep) = mid_line_report(&h);
                    assert!(text.starts_with(TYPED), "{keys:?}: {text:?}");
                    assert!(
                        h.torn_presents > 0,
                        "{keys:?} {stall} ms: must present torn"
                    );
                    assert!(
                        dark.is_empty(),
                        "{keys:?} lag {lag} stall {stall} {merged:?}: {rep}"
                    );
                }
            }
        }
    }
}

#[test]
fn a_key_erased_and_never_drawn_leaves_the_next_key_its_echo() {
    let keys = "all thes\u{8} subtrees";
    let erase = keys.chars().position(|c| c == BS).expect("the Backspace");
    for lag in [12, 200] {
        let take = MidLine {
            keys,
            lag_ms: lag,
            merged: &[erase - 1, erase],
            ..MidLine::PLAIN
        };
        let (h, text) = mid_line(Contract::SinglePane, take);
        let (dark, rep) = mid_line_report(&h);
        assert!(text.starts_with(TYPED), "lag {lag}: {text:?}");
        assert!(dark.is_empty(), "lag {lag}: {rep}");
    }
}

#[test]
fn a_correction_the_app_has_not_drawn_spends_no_key() {
    let keys = "all thee\u{8}e subtrees";
    for lag in [200, 500] {
        let take = MidLine {
            keys,
            lag_ms: lag,
            ..MidLine::PLAIN
        };
        let (h, text) = mid_line(Contract::SinglePane, take);
        let (dark, rep) = mid_line_report_of(&h, "all thee subtrees");
        assert!(text.starts_with("all thee subtrees"), "lag {lag}: {text:?}");
        assert!(dark.is_empty(), "lag {lag}: {rep}");
    }
}
