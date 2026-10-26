// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A word carried down by a soft-wrapped caret keeps its band, and only its band.

use crate::common::{
    LIT_COV, Rec, caret_sample, coverage, is_arrow, is_key, note_blink, observe_neighbor_rows,
    observe_ribbon_rows_where, rainbow_cfg, recording, sync_scroll,
};
use aterm_core::render::GlowQuad;
use aterm_core::terminal::{ContentScrollState, Terminal};
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, ProbeTrust};
use aterm_effects::rainbow_kitty::TypedClass;
use aterm_effects::rainbow_kitty::ribbon::{FLOW_TOTAL_S, RETRACT_FADE_S};
use aterm_effects::rainbow_kitty::witness::WITNESS_ROWS;
use std::time::{Duration, Instant};

/// The take's grid and cell: a headless instance at 40×120, 7×14 px.
const ROWS: usize = 40;
const COLS: usize = 120;
const CW: usize = 7;
const CH: usize = 14;
/// Composer text rows once the line has wrapped (0-based).
const ROW1: u16 = 36;
const ROW2: u16 = 37;
/// Continuation-row indent (`❯ ` on row 1, two blanks on later rows).
const INDENT: u16 = 2;
/// The frame train.
const FRAME_MS: u64 = 8;
/// `app_render.rs`'s `BLINK_RECENT_MAX`.
const BLINK_RECENT: Duration = Duration::from_secs(1);
/// Pause duration for [`Present::PauseBeforeWrap`], past `PHASER_CHAIN_GAP_MAX`.
const WRAP_PAUSE_MS: u64 = 1_000;
/// Row 1 must hold no band this long after the last key.
const STRANDED_AFTER_MS: u64 = 2_500;
const _: () = assert!(FLOW_TOTAL_S * 1000.0 < STRANDED_AFTER_MS as f32);

const FIXTURES: [(&str, &str); 7] = [
    (
        "row1-end",
        include_str!("fixtures/claude-wrap-2026-09-23-row1-end.ptylog"),
    ),
    (
        "row1-end-six",
        include_str!("fixtures/claude-wrap-2026-09-23-row1-end-six.ptylog"),
    ),
    (
        "hops",
        include_str!("fixtures/claude-wrap-2026-09-23-hops.ptylog"),
    ),
    (
        "hops-fast",
        include_str!("fixtures/claude-wrap-2026-09-23-hops-fast.ptylog"),
    ),
    (
        "eol-caret",
        include_str!("fixtures/claude-wrap-2026-09-23-eol-caret.ptylog"),
    ),
    (
        "row2-start",
        include_str!("fixtures/claude-wrap-2026-09-23-row2-start.ptylog"),
    ),
    (
        "end",
        include_str!("fixtures/claude-wrap-2026-09-23-end.ptylog"),
    ),
];

/// How the host presents a key relative to its echo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Present {
    /// Echo lands before the next frame.
    EchoInFrame,
    /// One frame renders between each key press and its echo.
    FrameBeforeEcho,
    /// [`Present::EchoInFrame`] with a one-second pause before the wrap key.
    PauseBeforeWrap,
    /// [`Present::EchoInFrame`] with typeahead across the wrap.
    TypeaheadAcrossWrap,
    /// [`Present::PauseBeforeWrap`] with typeahead across the wrap.
    TypeaheadAfterPause,
}

impl Present {
    const ALL: [Self; 5] = [
        Self::EchoInFrame,
        Self::FrameBeforeEcho,
        Self::PauseBeforeWrap,
        Self::TypeaheadAcrossWrap,
        Self::TypeaheadAfterPause,
    ];

    fn pauses(self) -> bool {
        matches!(self, Self::PauseBeforeWrap | Self::TypeaheadAfterPause)
    }

    fn typeahead(self) -> bool {
        matches!(self, Self::TypeaheadAcrossWrap | Self::TypeaheadAfterPause)
    }
}

fn fixture(name: &str) -> &'static str {
    FIXTURES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, s)| *s)
        .expect("a fixture")
}

/// Rainbow kitty, tall body, dark, full intensity.
fn cfg() -> GlowConfig {
    GlowConfig {
        duration: Duration::from_millis(260),
        intensity: 1.0,
        ..rainbow_cfg()
    }
}

fn geom() -> Geom {
    crate::common::geom(ROWS, COLS, CW, CH)
}

/// The host's `typed_class_for` and its shiftedness.
fn class_of(ch: char) -> (bool, TypedClass) {
    match ch {
        ' ' => (false, TypedClass::Space),
        '!' => (true, TypedClass::Bang),
        c if c.is_uppercase() => (true, TypedClass::Capital),
        c => ("~!@#$%^&*()_+{}|:\"<>?".contains(c), TypedClass::Glyph),
    }
}

/// Claude Code's kitty-keyboard Ctrl+C: the take's exit.
const CTRL_C: &[u8] = b"\x1b[99;5u";

/// One frame's census of the two text rows.
#[derive(Clone, Debug)]
struct Frame {
    ms: u64,
    caret: (u16, u16, bool),
    /// Planned coverage per column of [`ROW1`] and [`ROW2`].
    cov: [Vec<u8>; 2],
    /// Columns owned by a live (not leaving) ribbon cell, per row.
    live: [Vec<u16>; 2],
    /// Columns owned by any ribbon cell (leaving included), per row.
    any: [Vec<u16>; 2],
    /// The rows' text.
    text: [String; 2],
}

impl Frame {
    fn lit(&self, r: usize) -> Vec<usize> {
        (0..COLS).filter(|&c| self.cov[r][c] >= LIT_COV).collect()
    }

    fn map(&self, r: usize) -> String {
        self.cov[r]
            .iter()
            .map(|&v| if v >= LIT_COV { '#' } else { '.' })
            .collect()
    }
}

struct Host {
    term: Terminal,
    glow: CursorGlow,
    cfg: GlowConfig,
    t0: Instant,
    now: Instant,
    out: Vec<GlowQuad>,
    row_buf: Vec<char>,
    /// The caret row's neighbours, `[above, below]`.
    nbrs: [Vec<char>; 2],
    blink: u64,
    last_blink: Option<Instant>,
    scroll: Option<ContentScrollState>,
    frames: Vec<Frame>,
    /// Optional `(from_ms, to_ms)` window for admission and ribbon debug logging.
    ring: Option<(u64, u64)>,
    ring_seen: u64,
}

impl Host {
    fn new() -> Self {
        let now = Instant::now();
        let mut glow = crate::common::quads_glow();
        glow.note_pane_columns(0, COLS);
        glow.note_pane_rows(0, ROWS);
        Self {
            term: Terminal::new(ROWS as u16, COLS as u16),
            glow,
            cfg: cfg(),
            t0: now,
            now,
            out: Vec::new(),
            row_buf: Vec::new(),
            nbrs: [Vec::new(), Vec::new()],
            blink: 0,
            last_blink: None,
            scroll: None,
            frames: Vec::new(),
            ring: std::env::var("CLAUDE_WRAP_RING").ok().and_then(|w| {
                let (a, b) = w.split_once(':')?;
                Some((a.parse().ok()?, b.parse().ok()?))
            }),
            ring_seen: 0,
        }
    }

    fn ms(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_millis() as u64
    }

    fn debug(&mut self) {
        let Some((a, b)) = self.ring else { return };
        let ms = self.ms();
        if ms < a || ms > b {
            return;
        }
        for rec in self.glow.admission_log() {
            if rec.seq > self.ring_seen {
                eprintln!("  t={ms} ring: {}", rec.line(self.now));
                self.ring_seen = rec.seq;
            }
        }
        let tally = self.glow.in_flight_tally();
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        let mut cells: Vec<(u16, u16, bool)> = rib
            .cells()
            .iter()
            .map(|c| (c.row, c.col, c.leaving()))
            .collect();
        cells.sort_unstable();
        eprintln!(
            "  t={ms} credits={} cells(row,col,leaving)={cells:?}",
            tally.credits
        );
    }

    /// Run one host frame in production order.
    fn frame(&mut self) {
        sync_scroll(&self.term, &mut self.glow, &mut self.scroll, self.now);
        let (c, cur) = caret_sample(&self.term);
        if note_blink(&self.term, &mut self.glow, &mut self.blink, self.now) {
            self.last_blink = Some(self.now);
        }
        let alt = self.term.is_alternate_screen();
        self.glow.note_context(alt);
        let blink_recent = self
            .last_blink
            .is_some_and(|t| self.now.saturating_duration_since(t) <= BLINK_RECENT);
        let trust = if !alt || blink_recent {
            ProbeTrust::Full
        } else {
            ProbeTrust::ContentOnly
        };
        self.term
            .row_cols_into(usize::from(c.row), &mut self.row_buf);
        self.glow
            .observe_row_with_trust(c.row, c.col, &self.row_buf, self.now, trust);
        observe_neighbor_rows(&self.term, &mut self.glow, &mut self.nbrs, c.row, ROWS);
        self.glow
            .observe_print_anchor(aterm_effects::host::motion_print_anchor(&self.term).0);
        self.glow.observe_ribbon_row(c.row, &self.row_buf);
        observe_ribbon_rows_where::<WITNESS_ROWS>(
            &self.term,
            &mut self.glow,
            &mut self.row_buf,
            |r| r != c.row,
        );
        self.glow
            .tick(cur, self.now, &self.cfg, geom(), &mut self.out);
        self.record((c.row, c.col, cur.is_some()));
        self.debug();
    }

    fn record(&mut self, caret: (u16, u16, bool)) {
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        let cov = [ROW1, ROW2].map(|row| coverage(&self.glow, geom(), row));
        let cols_of = |row: u16, live_only: bool| {
            let mut v: Vec<u16> = rib
                .cells()
                .iter()
                .filter(|c| c.row == row && (!live_only || !c.leaving()))
                .map(|c| c.col)
                .collect();
            v.sort_unstable();
            v.dedup();
            v
        };
        let live = [cols_of(ROW1, true), cols_of(ROW2, true)];
        let any = [cols_of(ROW1, false), cols_of(ROW2, false)];
        let mut text = [String::new(), String::new()];
        for (i, row) in [ROW1, ROW2].into_iter().enumerate() {
            self.term.row_cols_into(usize::from(row), &mut self.row_buf);
            text[i] = self
                .row_buf
                .iter()
                .map(|&c| if c == '\0' { '_' } else { c })
                .collect::<String>()
                .trim_end()
                .to_string();
        }
        self.frames.push(Frame {
            ms: self.ms(),
            caret,
            cov,
            live,
            any,
            text,
        });
    }

    /// Advance the frame train up to `ms`, withholding presents while `?2026` is open.
    fn run_to(&mut self, ms: u64) {
        let t = self.t0 + Duration::from_millis(ms);
        while self.now + Duration::from_millis(FRAME_MS) <= t {
            self.now += Duration::from_millis(FRAME_MS);
            self.present();
        }
        self.now = self.now.max(t);
    }

    fn present(&mut self) {
        if !self.term.sync_open_dirty() {
            self.frame();
        }
    }

    /// Forward keyboard input hints to the cursor glow engine.
    fn hint(&mut self, bytes: &[u8]) {
        match bytes {
            [b] if (0x20..0x7f).contains(b) => {
                let ch = char::from(*b);
                let (shifted, class) = class_of(ch);
                self.glow
                    .note_typed_expected(self.now, 1, shifted && ch != ' ', class, ch);
            }
            [0x7f] => self.glow.note_backspace_erasing(self.now, Some(1)),
            b if is_arrow(b) => {
                self.glow.clear_typed_keeping_hand(self.now);
                self.glow.note_motion(self.now);
            }
            _ => {}
        }
    }
}

/// Replayed take with its captured frames and inserted key sequence.
struct Take {
    name: &'static str,
    present: Present,
    frames: Vec<Frame>,
    first_key: u64,
    last_key: u64,
    keys: String,
}

impl Take {
    fn window(&self, from: u64, to: u64) -> impl Iterator<Item = &Frame> {
        self.frames
            .iter()
            .filter(move |f| f.ms >= from && f.ms <= to)
    }

    fn at(&self, ms: u64) -> &Frame {
        self.frames
            .iter()
            .find(|f| f.ms >= ms)
            .unwrap_or_else(|| self.frames.last().expect("a frame"))
    }

    /// Format a frame-by-frame summary of the two rows when the plan changes.
    fn story(&self) -> String {
        let mut s = format!(
            "{} {:?}: insert {:?} at {}..{} ms\n",
            self.name, self.present, self.keys, self.first_key, self.last_key
        );
        let mut last = (String::new(), String::new());
        for f in self.window(self.first_key.saturating_sub(300), self.last_key + 2_500) {
            let m = (f.map(0), f.map(1));
            if m == last {
                continue;
            }
            s.push_str(&format!(
                "t={:6} caret={:?}\n  r36 {}\n      {}\n  r37 {}\n      {}\n  live36={:?}\n  live37={:?}\n",
                f.ms, f.caret, m.0, f.text[0], m.1, f.text[1], f.live[0], f.live[1],
            ));
            last = m;
        }
        s
    }
}

/// Timestamp of the key whose echo first wraps the caret from row 1 to row 2.
fn wrap_key_ms(rec: &[(u64, Rec)], from_ms: u64) -> Option<u64> {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    let mut key = None;
    let mut row = None;
    for (ms, ev) in rec {
        match ev {
            Rec::In(b) if *ms >= from_ms && is_key(b) => key = Some(*ms),
            Rec::In(_) => {}
            Rec::Out(bytes) => {
                term.process(bytes);
                if term.sync_open_dirty() {
                    continue;
                }
                let now = term.cursor().row;
                if key.is_some() && row == Some(ROW1) && now == ROW2 {
                    return key;
                }
                row = Some(now);
            }
        }
    }
    None
}

fn replay(name: &'static str, present: Present) -> Take {
    let mut rec = recording(fixture(name), 1_000_000);
    let exit = rec
        .iter()
        .find(|(_, e)| matches!(e, Rec::In(b) if b.as_slice() == CTRL_C))
        .map_or(u64::MAX, |(ms, _)| *ms);
    rec.retain(|(ms, _)| *ms < exit);
    let last_arrow = rec
        .iter()
        .filter(|(_, e)| matches!(e, Rec::In(b) if is_arrow(b)))
        .map(|(ms, _)| *ms)
        .next_back()
        .unwrap_or(0);
    if present.pauses() || present.typeahead() {
        let wrap = wrap_key_ms(&rec, last_arrow + 1)
            .unwrap_or_else(|| panic!("{name}: no key soft-wraps the caret to row 2"));
        if present.typeahead() {
            let wk = rec
                .iter()
                .position(|(ms, e)| *ms == wrap && matches!(e, Rec::In(b) if is_key(b)))
                .expect("the wrap key");
            let next = rec
                .iter()
                .skip(wk + 1)
                .position(|(_, e)| matches!(e, Rec::In(b) if is_key(b)))
                .map(|i| wk + 1 + i)
                .unwrap_or_else(|| panic!("{name}: a key follows the wrap key"));
            for (ms, _) in &mut rec[wk + 1..] {
                *ms += 2;
            }
            rec[next].0 = wrap + 1;
            rec.sort_by_key(|(ms, _)| *ms);
        }
        if present.pauses() {
            for (ms, _) in &mut rec {
                if *ms >= wrap {
                    *ms += WRAP_PAUSE_MS;
                }
            }
        }
    }
    let keys: Vec<(u64, char)> = rec
        .iter()
        .filter_map(|(ms, e)| match e {
            Rec::In(b) if *ms > last_arrow && is_key(b) => Some((*ms, char::from(b[0]))),
            _ => None,
        })
        .collect();
    let first_key = keys.first().expect("a typed key").0;
    let last_key = keys.last().expect("a typed key").0;
    let mut h = Host::new();
    for (ms, ev) in rec {
        h.run_to(ms);
        match ev {
            Rec::In(b) => {
                h.hint(&b);
                if present == Present::FrameBeforeEcho {
                    h.present();
                }
            }
            Rec::Out(bytes) => {
                h.term.process(&bytes);
                h.present();
            }
        }
    }
    h.run_to(last_key + STRANDED_AFTER_MS + 500);
    let take = Take {
        name,
        present,
        frames: h.frames,
        first_key,
        last_key,
        keys: keys.iter().map(|k| k.1).collect(),
    };
    if std::env::var_os("CLAUDE_WRAP_DUMP").is_some() {
        eprintln!("{}", take.story());
    }
    take
}

/// One take's census against the laws.
#[derive(Debug, Default)]
struct Verdict {
    /// When the wrapped row first held inserted text under a caret past it.
    landed_ms: Option<u64>,
    /// Frames with a ribbon cell or lit coverage in the wrapped row's indent.
    stub: Vec<(u64, Vec<u16>)>,
    /// Frames missing a live cell under typed text left of the caret on the wrapped row.
    gaps: Vec<(u64, u16, Vec<u16>)>,
    /// Frames with a live cell at or right of the caret on the wrapped row.
    ahead: Vec<(u64, u16, Vec<u16>)>,
    /// Row-1 cells and lit columns remaining after `STRANDED_AFTER_MS`.
    stranded: Vec<(u64, Vec<u16>, Vec<usize>)>,
    /// Frames with a live row-1 cell past row 1's text after retract fade.
    tail: Vec<(u64, Vec<u16>)>,
}

impl Verdict {
    fn of(t: &Take) -> Self {
        let mut v = Self::default();
        for f in t.window(t.first_key, u64::MAX) {
            let mut stub: Vec<u16> = f.any[1]
                .iter()
                .copied()
                .filter(|&c| c < INDENT)
                .chain((0..INDENT).filter(|&c| f.cov[1][usize::from(c)] >= LIT_COV))
                .collect();
            stub.sort_unstable();
            stub.dedup();
            if !stub.is_empty() {
                v.stub.push((f.ms, stub));
            }
            if v.landed_ms.is_none()
                && f.text[0].starts_with('❯')
                && f.caret.0 == ROW2
                && f.caret.1 > INDENT + 1
            {
                v.landed_ms = Some(f.ms);
            }
            if let Some(landed) = v.landed_ms
                && f.caret.0 == ROW2
                && f.ms <= t.last_key + 100
            {
                let missing: Vec<u16> = (INDENT..f.caret.1)
                    .filter(|c| !f.live[1].contains(c))
                    .collect();
                if !missing.is_empty() {
                    v.gaps.push((f.ms, f.caret.1, missing));
                }
                let end = u16::try_from(f.text[0].chars().count()).unwrap_or(u16::MAX);
                let past: Vec<u16> = f.live[0].iter().copied().filter(|&c| c > end).collect();
                let fade_ms = (RETRACT_FADE_S * 1000.0) as u64 + FRAME_MS;
                if !past.is_empty() && f.ms >= landed + fade_ms {
                    v.tail.push((f.ms, past));
                }
                let ahead: Vec<u16> = f.live[1]
                    .iter()
                    .copied()
                    .filter(|&c| c >= f.caret.1)
                    .collect();
                if !ahead.is_empty() {
                    v.ahead.push((f.ms, f.caret.1, ahead));
                }
            }
            if f.ms >= t.last_key + STRANDED_AFTER_MS {
                let lit = f.lit(0);
                if !f.any[0].is_empty() || !lit.is_empty() {
                    v.stranded.push((f.ms, f.any[0].clone(), lit));
                }
            }
        }
        v
    }

    fn report(&self, t: &Take) -> String {
        let mut s = format!(
            "{} {:?}: insert {}..{} ms, landed on row 2 at {:?} ms\n  stub frames {} {:?}\n  gap frames {} {:?}\n  ahead-of-caret frames {} {:?}\n  stranded frames {} {:?}\n  row-1 tail frames {} {:?}\n",
            t.name,
            t.present,
            t.first_key,
            t.last_key,
            self.landed_ms,
            self.stub.len(),
            self.stub.first(),
            self.gaps.len(),
            self.gaps.first(),
            self.ahead.len(),
            self.ahead.first(),
            self.stranded.len(),
            self.stranded.first(),
            self.tail.len(),
            self.tail.first(),
        );
        if let Some(&(ms, _, _)) = self.gaps.first() {
            let f = t.at(ms);
            s.push_str(&format!(
                "  at {ms} ms: caret {:?}\n    row2 text  {}\n    row2 plan  {}\n    row2 live  {:?}\n",
                f.caret,
                f.text[1],
                f.map(1),
                f.live[1],
            ));
        }
        s
    }

    /// The laws, as the first one broken.
    fn broken(&self) -> Option<&'static str> {
        if self.landed_ms.is_none() {
            Some("the insert never reached the wrapped row")
        } else if !self.stub.is_empty() {
            Some("a band cell stands in the wrapped row's indent")
        } else if !self.gaps.is_empty() {
            Some("typed text on the wrapped row has no band under it")
        } else if !self.ahead.is_empty() {
            Some("a band cell stands at or right of the caret")
        } else if !self.stranded.is_empty() {
            Some("band left stranded on row 1")
        } else if !self.tail.is_empty() {
            Some("the moved word's old light stayed on row 1's blank tail")
        } else {
            None
        }
    }
}

/// Assert the laws on one take under each of `modes`, naming every mode that broke one.
fn assert_laws(name: &'static str, modes: &[Present]) {
    assert_laws_of(&[name], modes);
}

/// Run [`assert_laws`] across `names` and `modes` in parallel.
fn assert_laws_of(names: &[&'static str], modes: &[Present]) {
    let takes: Vec<(&'static str, Present)> = names
        .iter()
        .flat_map(|&name| modes.iter().map(move |&present| (name, present)))
        .collect();
    let mut verdicts = crate::common::par_map(&takes, |&(name, present)| {
        std::panic::catch_unwind(|| {
            let t = replay(name, present);
            let v = Verdict::of(&t);
            v.broken()
                .map(|law| format!("{name} {present:?}: {law}\n{}", v.report(&t)))
        })
    })
    .into_iter();
    for _ in names {
        let failures: Vec<String> = verdicts
            .by_ref()
            .take(modes.len())
            .filter_map(|verdict| verdict.unwrap_or_else(|panic| std::panic::resume_unwind(panic)))
            .collect();
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}

/// Partial word carried down by the next key keeps its first letter without an indent stub.
#[test]
fn a_partial_word_carried_down_by_the_next_key_keeps_its_first_letter_and_no_indent_stub() {
    assert_laws("row1-end", &Present::ALL);
}

/// Six-letter word carried down keeps every letter without an indent stub.
#[test]
fn a_six_letter_word_carried_down_keeps_every_letter_and_no_indent_stub() {
    assert_laws("row1-end-six", &Present::ALL);
}

/// Word hops across row 1 followed by a wrapped insert keep the first letter.
#[test]
fn the_reflowed_word_after_word_hops_keeps_its_first_letter() {
    assert_laws("hops", &Present::ALL);
}

/// Fast word hops followed immediately by a wrapped insert keep the first letter.
#[test]
fn the_reflowed_word_after_fast_word_hops_keeps_its_first_letter() {
    assert_laws("hops-fast", &Present::ALL);
}

/// Control cases: insert at the continuation indent and wrap at the end of text.
#[test]
fn controls_an_insert_on_the_wrapped_row_and_an_end_of_text_wrap() {
    assert_laws_of(
        &["row2-start", "end"],
        &[Present::EchoInFrame, Present::FrameBeforeEcho],
    );
}

/// Insert whose first key carries the caret and trailing text down to row 2 stays lit.
#[test]
fn an_insert_whose_first_key_carries_the_caret_down_is_lit() {
    assert_laws("eol-caret", &Present::ALL);
}
