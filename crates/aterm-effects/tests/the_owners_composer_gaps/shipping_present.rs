// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Composer laws and committed `ptyrec` replay through `EffectsPipeline::cursor_step`.

use super::*;
use crate::common::{
    FrameHold, LIT_COV, Rec, class_of, coverage, is_arrow, is_key, note_blink,
    observe_ribbon_rows_where, par_map, recording,
};
use aterm_core::terminal::{ClockReading, ContentScrollDelta, ContentScrollState};
use aterm_effects::cursor_fx::CursorFxInput;
use aterm_effects::cursor_glow::ProbeTrust;
use aterm_effects::cursor_trail::TrailConfig;
use aterm_effects::host::{hidden_caret_withheld, motion_caret, motion_print_anchor};
use aterm_effects::pipeline::{CursorStep, RowProbe};
use std::ops::{Deref, DerefMut};

/// The composer's label in the probes' top rule.
const LABEL: &str = "workspace";
/// The text after the hand's insert in the mid-line probes.
const TAIL: &str = " carefully and ultrathink design this.";

/// The engine side of one present: what the frame reads into and ticks.
struct Glass<'a> {
    glow: &'a mut PresentedGlow,
    cfg: &'a GlowConfig,
    g: Geom,
    /// The caret row's probe, then each witness row's.
    buf: &'a mut Vec<char>,
    witness: &'a mut Vec<char>,
    /// The caret row's flanks, `[above, below]`.
    nbrs: &'a mut [Vec<char>; 2],
    /// The repaint blink epoch last seen.
    blink: &'a mut u64,
    last_blink: &'a mut Option<Instant>,
}

/// One frame the hold let through, as the shipping single-pane present runs
/// it over `term` at `now`: the caret through the motion law, the blink edge,
/// the context, the caret row's probe and its flanks, the echo anchor and its
/// glyph, the caret-drawn bit, the witness rows, the tick.
fn present(term: &Terminal, now: Instant, gl: Glass<'_>) {
    let c = term.cursor();
    let visible = term.cursor_visible();
    let cur = motion_caret(Some((c.row, c.col)), visible, hidden_caret_withheld(term));
    let scroll = gl.glow.0.sync_scroll(term.content_scroll_state(), now);
    if note_blink(term, gl.glow, gl.blink, now) {
        *gl.last_blink = Some(now);
        gl.glow.0.cursor_mut().trail.note_repaint_blink(now);
    }
    let alt = term.is_alternate_screen();
    gl.glow.note_context(alt);
    gl.glow.note_pane_columns(0, gl.g.cols);
    gl.glow.note_pane_rows(0, gl.g.rows);
    gl.glow.0.cursor_mut().trail.note_context(alt);
    gl.glow.0.cursor_mut().trail.note_pane_columns(0, gl.g.cols);
    let probe = if term.grid().display_offset() == 0 && !scroll.changed() {
        term.row_cols_into(usize::from(c.row), gl.buf);
        let [above, below] = gl.nbrs;
        if c.row > 0 {
            term.row_cols_into(usize::from(c.row) - 1, above);
        }
        if usize::from(c.row) + 1 < gl.g.rows {
            term.row_cols_into(usize::from(c.row) + 1, below);
        }
        gl.glow.observe_ribbon_row(c.row, gl.buf);
        observe_ribbon_rows_where::<WROWS>(term, gl.glow, gl.witness, |r| r != c.row);
        let blink_recent = gl
            .last_blink
            .is_some_and(|at| now.saturating_duration_since(at) <= Duration::from_secs(1));
        Some(RowProbe {
            row: c.row,
            caret: c.col,
            cols: gl.buf,
            trust: if !alt || blink_recent {
                ProbeTrust::Full
            } else {
                ProbeTrust::ContentOnly
            },
            flanks: Some((
                (c.row > 0).then_some(above.as_slice()),
                (usize::from(c.row) + 1 < gl.g.rows).then_some(below.as_slice()),
            )),
        })
    } else {
        None
    };
    let (anchor, glyph) = motion_print_anchor(term);
    gl.glow.0.cursor_step(
        &CursorStep {
            input: CursorFxInput {
                now,
                cur,
                live_viewport: term.grid().display_offset() == 0,
                cursor_style: term.cursor_style(),
                blink_phase: true,
                live_cursor: gl.cfg.color,
                cursor_color_pinned: false,
                default_bg: gl.cfg.theme_bg,
                geom: gl.g,
                focused: true,
                cursor_cell_bg: None,
                trail_brightness: 1.0,
                shed_envelope: 1.0,
                body_allowed: false,
                momentum_glow: false,
                user_tinted: false,
                accent_pinned: false,
            },
            glow: gl.cfg,
            trail: TrailConfig {
                enabled: false,
                duration: gl.cfg.duration,
                max_len: gl.cfg.length,
                color: gl.cfg.color,
                intensity: gl.cfg.intensity,
                warmth: 0.0,
            },
            animate: true,
            row_probe: probe,
            pending_keys: 0,
            print_anchor: anchor,
            print_anchor_glyph: glyph,
            caret_drawn: visible,
            frame_cells: None,
            frame_cells_at: (0, 0),
        },
        std::iter::empty(),
    );
}

/// Single-pane shipping present harness wrapping [`Sim`].
pub(crate) struct Hosted {
    s: Sim,
    hold: FrameHold,
    nbrs: [Vec<char>; 2],
    witness: Vec<char>,
    /// `(from, to)` ms windows in which aterm presents no frame (its own
    /// frames starved).
    starved: Vec<(u64, u64)>,
}

impl Deref for Hosted {
    type Target = Sim;
    fn deref(&self) -> &Sim {
        &self.s
    }
}

impl DerefMut for Hosted {
    fn deref_mut(&mut self) -> &mut Sim {
        &mut self.s
    }
}

/// Cut point for splitting a synthetic render across frames.
#[derive(Clone, Copy, Debug)]
pub(crate) enum TearAt {
    /// Before the park (the parent's own tear).
    Park,
    /// Before the CUP of the first row at or below this one the render writes.
    Row(u16),
}

impl Hosted {
    /// Create a composer harness under a top rule labelled `label`.
    pub(crate) fn new(label: &'static str) -> Self {
        Self {
            s: Sim::new(label),
            hold: FrameHold::default(),
            nbrs: [Vec::new(), Vec::new()],
            witness: Vec::new(),
            starved: Vec::new(),
        }
    }

    fn frame(&mut self) {
        let s = &mut self.s;
        if self.hold.withholds(&s.term, s.now) {
            return;
        }
        present(
            &s.term,
            s.now,
            Glass {
                glow: &mut s.glow,
                cfg: &s.cfg,
                g: s.g,
                buf: &mut s.buf,
                nbrs: &mut self.nbrs,
                blink: &mut s.blink_seen,
                last_blink: &mut s.last_blink,
                witness: &mut self.witness,
            },
        );
        s.scan_holes();
        s.drain_declines();
    }

    fn process(&mut self, bytes: &[u8]) {
        let at = ClockReading {
            monotonic: self.s.now,
            wall_ms: None,
        };
        self.s.term.process_at(bytes, at);
    }

    /// Advance the frame train to `t` ms, invoking `obs` after each presented frame.
    pub(crate) fn advance(&mut self, t: u64, obs: &mut dyn FnMut(&Sim)) {
        let target = self.s.t0 + Duration::from_millis(t);
        while self.s.next_frame <= target {
            self.s.now = self.s.next_frame;
            let ms = self.s.ms();
            if !self.starved.iter().any(|&(a, b)| ms >= a && ms < b) {
                self.frame();
                obs(&self.s);
            }
            self.s.next_frame = self.s.now + Duration::from_micros(FRAME_US);
        }
        self.s.now = self.s.now.max(target);
    }

    /// Run a scripted key/echo sequence with an optional torn render.
    pub(crate) fn run(
        &mut self,
        keys: &[(u64, char)],
        renders: &[u64],
        tear: Option<(usize, TearAt, u64)>,
        obs: &mut dyn FnMut(&Sim),
    ) {
        const KEY: u8 = 0;
        const REST: u8 = 1;
        const RENDER: u8 = 2;
        let mut ev: BinaryHeap<Reverse<(u64, u8, usize)>> = BinaryHeap::new();
        for (i, k) in keys.iter().enumerate() {
            ev.push(Reverse((k.0, KEY, i)));
        }
        for (i, &r) in renders.iter().enumerate() {
            ev.push(Reverse((r, RENDER, i)));
        }
        let mut rest: Option<(u64, Vec<u8>)> = None;
        while let Some(Reverse((t, kind, i))) = ev.pop() {
            let t_now = t.max(self.s.ms());
            self.advance(t_now, obs);
            match kind {
                KEY => self.s.press(keys[i].1),
                REST => {
                    if let Some((_, bytes)) = rest.take() {
                        self.process(&bytes);
                    }
                }
                _ => {
                    if let Some(&(due, _)) = rest.as_ref() {
                        ev.push(Reverse((due, RENDER, i)));
                        continue;
                    }
                    let Some(b) = self.render_pending() else {
                        continue;
                    };
                    match tear.filter(|&(r, _, _)| r == i) {
                        None => self.process(&b),
                        Some((_, at, ms)) => {
                            let cut = cut_at(&b, at);
                            self.process(&b[..cut]);
                            let due = t + ms;
                            ev.push(Reverse((due, REST, i)));
                            rest = Some((due, b[cut..].to_vec()));
                        }
                    }
                }
            }
        }
    }

    /// Every pending key echoed: the composer's repaint, rows a shrinking box
    /// left blanked inside the same chunk; `None` with nothing pending.
    fn render_pending(&mut self) -> Option<Vec<u8>> {
        let s = &mut self.s;
        let pending: Vec<char> = std::mem::take(&mut s.pending);
        if pending.is_empty() {
            return None;
        }
        let before_top = top_rule(s);
        for ch in pending.iter().copied() {
            if ch == BACKSPACE {
                if s.comp.caret > 0 {
                    s.comp.caret -= 1;
                    s.comp.text.remove(s.comp.caret);
                }
                continue;
            }
            s.comp.text.insert(s.comp.caret, ch);
            s.comp.caret += 1;
        }
        let mut b = s.comp.render();
        let after_top = s.comp.rows_for().0[0].0;
        if after_top > before_top {
            let mut erase = String::new();
            for r in before_top..after_top {
                erase.push_str(&format!("\x1b[{};1H\x1b[2K", r + 1));
                s.comp.screen[r] = vec![' '; COLS];
            }
            let at = SYNC_BEGIN.len() + "\x1b[?25l".len();
            b.splice(at..at, erase.into_bytes());
        }
        if s.verbose {
            s.log.push(format!(
                "{}ms RENDER {:?} caret->{:?}",
                s.ms(),
                pending.iter().collect::<String>(),
                s.comp.rows_for().1
            ));
        }
        Some(b)
    }
}

/// The byte offset a tear `at` cuts the render `b` at.
fn cut_at(b: &[u8], at: TearAt) -> usize {
    match at {
        TearAt::Park => {
            let park = format!("\x1b[{ROWS};1H");
            b.windows(park.len())
                .rposition(|w| w == park.as_bytes())
                .expect("the park")
        }
        TearAt::Row(row) => (row..ROWS as u16)
            .find_map(|r| {
                let cup = format!("\x1b[{};1H", r + 1);
                b.windows(cup.len()).position(|w| w == cup.as_bytes())
            })
            .expect("a row at or below the tear"),
    }
}

/// The row of the box's top rule as the composer last drew it.
fn top_rule(s: &Sim) -> usize {
    s.comp.rows_for().0[0].0
}

/// Words padded with `q`s to exactly `n` chars, ending in a Space.
fn words_to(n: usize) -> String {
    let words = [
        "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta", "iota", "kapa",
        "lambda", "mu", "nu", "xi", "omicron", "pi", "rho", "sigma", "tau", "upsilon",
    ];
    let mut s = String::new();
    let mut i = 0;
    while s.chars().count() + words[i % words.len()].len() < n {
        s.push_str(words[i % words.len()]);
        s.push(' ');
        i += 1;
    }
    while s.chars().count() < n {
        s.insert(0, 'q');
    }
    s
}

/// The light on `(row, col)` this frame: the brightest channel of the
/// brightest bed quad on `row` overlapping the cell's columns.
fn light(s: &Sim, row: u16, col: u16) -> u8 {
    let x0 = usize::from(col) * CW;
    let x1 = x0 + CW;
    s.glow
        .under_quads()
        .iter()
        .filter(|q| {
            let qx = usize::from(q.x);
            q.row == row && qx < x1 && qx + usize::from(q.w) > x0
        })
        .map(|q| {
            let c = q.color;
            ((c >> 16) & 0xff).max((c >> 8) & 0xff).max(c & 0xff) as u8
        })
        .max()
        .unwrap_or(0)
}

/// The last non-blank column on `row`, if any.
fn last_glyph(s: &Sim, row: u16) -> Option<u16> {
    let t: Vec<char> = s.row_text(row).chars().collect();
    t.iter()
        .rposition(|&c| c != ' ' && c != '\u{a0}')
        .map(|c| c as u16)
}

/// The declines the take logged.
fn declines(s: &Sim) -> Vec<String> {
    s.log
        .iter()
        .filter(|l| l.contains("Declined"))
        .cloned()
        .collect()
}

/// The glued word F1 and F3 type.
const GLUED: &str = "versussystemwidediskspaces";

/// A box-growth take: `lines_before` full lines, then a last line of `pad`
/// chars ending in a Space, then `word` typed at its end, `gap` ms apart.
struct BoxGrow {
    lines_before: usize,
    pad: usize,
    word: &'static str,
    gap: u64,
}

/// What a box-growth take's frames showed from the wrap key on.
#[derive(Default)]
struct Census {
    /// Frames with a cell of the hand's word dark at the band's left end
    /// while a later one is lit.
    relaid_dark: u32,
    relaid_dark_example: String,
    /// Frames with light on cells past the hand's row's last glyph.
    lit_blank: u32,
    lit_blank_max_run_ms: u64,
    lit_blank_example: String,
    holes: u32,
    grew: bool,
    dark_at_settle: String,
}

/// Runs `b` with each key echoed `delay(i)` after it and render `tear` torn;
/// returns the take, its census and the hand's screen row.
fn boxgrow_take(
    b: &BoxGrow,
    delay: &dyn Fn(usize) -> u64,
    tear: Option<(usize, TearAt, u64)>,
) -> (Hosted, Census, u16) {
    let mut s = Hosted::new(LABEL);
    s.verbose = true;
    let mut text = String::new();
    for _ in 0..b.lines_before {
        text.push_str(&words_to(TW));
    }
    text.push_str(&words_to(b.pad));
    s.preload(&text, text.chars().count());
    let (row0, col0) = s.caret();
    let top0 = top_rule(&s);
    assert_eq!(
        usize::from(col0),
        2 + b.pad,
        "the hand must stand after the pad"
    );
    let keys = keys_of(b.word, s.ms() + 100, b.gap);
    let renders = renders_for(&keys, delay);
    let mut c = Census::default();
    let wrap_press = keys[TW - b.pad].0;
    let last_press = keys.last().expect("a typed word").0;
    let mut echoed_at = None;
    let mut lit_blank_since: Option<u64> = None;
    let mut obs = |s: &Sim| {
        let t = s.ms();
        if t >= last_press && s.pending.is_empty() && !s.term.sync_frame_unfinished() {
            echoed_at.get_or_insert(t);
        }
        if t < wrap_press {
            return;
        }
        let (cr, cc) = s.caret();
        if cr != row0 {
            return;
        }
        let live = s.live(row0);
        if echoed_at.is_some_and(|at| t >= at + 150) && c.dark_at_settle.is_empty() {
            let dark: Vec<u16> = (2..cc).filter(|c| !live.contains(c)).collect();
            c.dark_at_settle = format!("settle dark[{}]={dark:?}", dark.len());
        }
        if let Some(fl) = (2..cc).find(|c| live.contains(c))
            && fl > 2
            && cc < 40
        {
            c.relaid_dark += 1;
            if c.relaid_dark_example.is_empty() {
                c.relaid_dark_example =
                    format!("t={t}ms caret ({cr},{cc}) dark 2..{fl} lit from {fl}");
            }
        }
        let last = last_glyph(s, row0).unwrap_or(0).max(cc);
        let past: Vec<u16> = ((last + 1)..=(TW as u16 + 1))
            .filter(|&c| light(s, row0, c) > 40)
            .collect();
        if past.is_empty() {
            if let Some(since) = lit_blank_since.take() {
                c.lit_blank_max_run_ms = c.lit_blank_max_run_ms.max(t - since);
            }
        } else {
            c.lit_blank += 1;
            let since = *lit_blank_since.get_or_insert(t);
            c.lit_blank_max_run_ms = c.lit_blank_max_run_ms.max(t - since);
            if c.lit_blank_example.is_empty() {
                let lv: Vec<u8> = past.iter().map(|&col| light(s, row0, col)).collect();
                c.lit_blank_example =
                    format!("t={t}ms row {row0} lit over blanks {past:?} lv {lv:?}");
            }
        }
    };
    s.run(&keys, &renders, tear, &mut obs);
    let end = s.ms() + 1500;
    s.advance(end, &mut obs);
    assert!(
        !c.dark_at_settle.is_empty(),
        "the completed echo must reach its settle observation"
    );
    c.holes = s.holes_seen.len() as u32;
    c.grew = top_rule(&s) + 1 == top0;
    s.cov = Some((row0, 2));
    (s, c, row0)
}

/// F1: light follows its text through a box growth's same-row re-anchor. A
/// glued word typed into the last columns of the composer's last line wraps
/// whole to the next row's indent and the box grows a row (the caret keeps
/// its screen row): the relaid word is lit there, and no light stays on the
/// blanks it left.
#[test]
fn a_glued_word_a_box_growth_relays_is_lit_where_it_lands() {
    let mut fails = Vec::new();
    for lines_before in [0usize, 2] {
        for pad in [TW - 4, TW - 11] {
            for gap in [110u64, 60] {
                for echo in [2u64, 20] {
                    let b = BoxGrow {
                        lines_before,
                        pad,
                        word: GLUED,
                        gap,
                    };
                    let (s, c, row0) = boxgrow_take(&b, &|_| echo, None);
                    let grew = c.grew && s.caret().0 == row0;
                    let st = s.glow.v2_status();
                    let line = format!(
                        "lines {lines_before} pad {pad} gap {gap} echo {echo}: grew {grew} {} {} relaid_dark {} ({}) lit_blank {} frames max {} ms ({}) holes {} | retired {:?} followed {:?} | {}",
                        s.dark(),
                        c.dark_at_settle,
                        c.relaid_dark,
                        c.relaid_dark_example,
                        c.lit_blank,
                        c.lit_blank_max_run_ms,
                        c.lit_blank_example,
                        c.holes,
                        st.map(|s| s.retired),
                        st.map(|s| s.followed),
                        declines(&s).join(" ; ")
                    );
                    eprintln!("{line}");
                    if !grew
                        || s.dark() != "dark[0]="
                        || c.relaid_dark > 0
                        || c.lit_blank_max_run_ms > 200
                    {
                        fails.push(line);
                    }
                }
            }
        }
    }
    assert!(
        fails.is_empty(),
        "the relaid glued word dark or light left on blanks in {} takes:\n{}",
        fails.len(),
        fails.join("\n")
    );
}

/// F1's control: the same relay of a word typed after a Space, which measures
/// the word (`Ribbon::moved_word_len`): lit whole once it settles.
#[test]
fn a_spaced_word_a_box_growth_relays_is_lit_whole() {
    let mut fails = Vec::new();
    for pad in [TW - 5, TW - 12] {
        for echo in [2u64, 20] {
            let b = BoxGrow {
                lines_before: 2,
                pad,
                word: " versussystemwidediskspaces",
                gap: 110,
            };
            let (s, c, _) = boxgrow_take(&b, &|_| echo, None);
            let st = s.glow.v2_status();
            let line = format!(
                "space+glued pad {pad} echo {echo}: grew {} {} {} relaid_dark {} retired {:?} followed {:?}",
                c.grew,
                s.dark(),
                c.dark_at_settle,
                c.relaid_dark,
                st.map(|s| s.retired),
                st.map(|s| s.followed)
            );
            eprintln!("{line}");
            if !c.grew || c.dark_at_settle != "settle dark[0]=[]" {
                fails.push(line);
            }
        }
    }
    assert!(
        fails.is_empty(),
        "the relaid spaced word dark at settle in {} takes:\n{}",
        fails.len(),
        fails.join("\n")
    );
}

/// The echo schedules F3 and its control sweep: uniform lags, stalls at or
/// before the wrap key `wrap`, alternating lags.
type Lag = (String, Box<dyn Fn(usize) -> u64>);

/// F3: presses in flight license their echo however late it lands within
/// their patience, whatever shape the composer gives it.
#[test]
fn probe_f3_lagged_catchup_across_boxgrow() {
    let mut fails = Vec::new();
    let b = BoxGrow {
        lines_before: 2,
        pad: TW - 4,
        word: GLUED,
        gap: 110,
    };
    let wrap = TW - b.pad;
    let cases: Vec<Lag> = vec![
        ("lag 280".into(), Box::new(|_| 280)),
        ("lag 600".into(), Box::new(|_| 600)),
        ("lag 1200".into(), Box::new(|_| 1200)),
        ("lag 2300".into(), Box::new(|_| 2300)),
        (
            "stall 600 at the wrap key".into(),
            Box::new(move |i| if i == wrap { 600 } else { 2 }),
        ),
        (
            "stall 1500 at the wrap key".into(),
            Box::new(move |i| if i == wrap { 1500 } else { 2 }),
        ),
        (
            "stall 2000 two keys before the wrap".into(),
            Box::new(move |i| if i == wrap - 2 { 2000 } else { 2 }),
        ),
        (
            "alternating 150/650".into(),
            Box::new(|i| if i % 3 == 0 { 650 } else { 150 }),
        ),
    ];
    for (name, delay) in &cases {
        let (s, c, _) = boxgrow_take(&b, delay.as_ref(), None);
        let line = format!(
            "{name}: {} relaid_dark {} ({}) lit_blank {} holes {} first {:?} | {}",
            c.dark_at_settle,
            c.relaid_dark,
            c.relaid_dark_example,
            c.lit_blank,
            c.holes,
            s.holes_seen.first(),
            declines(&s).join(" ; ")
        );
        eprintln!("{line}");
        if c.dark_at_settle != "settle dark[0]=[]" || c.holes > 0 {
            fails.push(line);
        }
    }
    assert!(
        fails.is_empty(),
        "F3 reproduced in {} takes:\n{}",
        fails.len(),
        fails.join("\n")
    );
}

/// F3's control: the lagged relay of a word typed after a Space, so the
/// word's measure is not in question: lit whole once it settles.
#[test]
fn probe_f3_control_typed_space() {
    let mut fails = Vec::new();
    let b = BoxGrow {
        lines_before: 2,
        pad: TW - 5,
        word: " versussystemwidediskspaces",
        gap: 110,
    };
    let wrap = TW - b.pad;
    let cases: Vec<Lag> = vec![
        ("lag 150".into(), Box::new(|_| 150)),
        ("lag 280".into(), Box::new(|_| 280)),
        ("lag 600".into(), Box::new(|_| 600)),
        ("lag 2300".into(), Box::new(|_| 2300)),
        (
            "stall 600 at the wrap key".into(),
            Box::new(move |i| if i == wrap { 600 } else { 2 }),
        ),
        (
            "stall 1500 at the wrap key".into(),
            Box::new(move |i| if i == wrap { 1500 } else { 2 }),
        ),
    ];
    for (name, delay) in &cases {
        let (s, c, _) = boxgrow_take(&b, delay.as_ref(), None);
        let tally = s.glow.in_flight_tally();
        let line = format!(
            "space+glued {name}: {} relaid_dark {} parks f{} | {}",
            c.dark_at_settle,
            c.relaid_dark,
            tally.park_flushed,
            declines(&s).join(" ; ")
        );
        eprintln!("{line}");
        if c.dark_at_settle != "settle dark[0]=[]" {
            fails.push(line);
        }
    }
    assert!(
        fails.is_empty(),
        "lagged relay dark in {} takes:\n{}",
        fails.len(),
        fails.join("\n")
    );
}

/// F2: light stands only over its own text, also while it leaves. A mid-line
/// insert whose word crosses the right edge relays to the next row's indent
/// with no box growth (the tail already stands there): the cells it vacated
/// at the old row's end are blank and keep no light past 200 ms, and the
/// relaid word is lit.
#[test]
fn probe_f2_midline_wrap_vacated_light() {
    let mut fails = Vec::new();
    for head_n in [102usize, 60, 20] {
        let lead = if head_n == 102 {
            String::new()
        } else {
            words_to(102 - head_n)
        };
        for gap in [110u64, 60] {
            for echo in [2u64, 20] {
                let mut s = Hosted::new(LABEL);
                s.verbose = true;
                let head = words_to(head_n);
                s.preload(&format!("{head}{TAIL}"), head.chars().count());
                let typed = format!("{lead}processtree of the");
                assert_eq!(
                    head_n + lead.len() + 7,
                    TW,
                    "process must fill the row exactly"
                );
                let keys = keys_of(&typed, s.ms() + 100, gap);
                let renders = renders_for(&keys, |_| echo);
                let relay_press = keys[lead.len() + 7].0;
                let p_press = keys[lead.len()].0;
                let mut hand_row: Option<u16> = None;
                let mut vac: Vec<(u64, Vec<u16>, Vec<u8>)> = Vec::new();
                let mut relaid_lit_at: Option<u64> = None;
                let mut obs = |s: &Sim| {
                    let t = s.ms();
                    let (cr, cc) = s.caret();
                    if t < relay_press {
                        if t >= p_press + 20 && hand_row.is_none() {
                            hand_row = Some(cr);
                        }
                        return;
                    }
                    let Some(old) = hand_row else { return };
                    let last = last_glyph(s, old).unwrap_or(0);
                    let past: Vec<u16> = ((last + 2)..=(TW as u16 + 1))
                        .filter(|&c| light(s, old, c) > 40)
                        .collect();
                    if !past.is_empty() && t > relay_press + 300 {
                        let lv = past.iter().map(|&c| light(s, old, c)).collect();
                        vac.push((t, past, lv));
                    }
                    if relaid_lit_at.is_none() && cr == old + 1 && cc > 9 {
                        let live = s.live(cr);
                        if (2..cc).all(|c| live.contains(&c)) {
                            relaid_lit_at = Some(t - relay_press);
                        }
                    }
                };
                s.run(&keys, &renders, None, &mut obs);
                let end = s.ms() + 2000;
                s.advance(end, &mut obs);
                let span = vac.first().zip(vac.last()).map_or(0, |(a, b)| b.0 - a.0);
                let line = format!(
                    "head {head_n} gap {gap} echo {echo}: hand_row {hand_row:?} vacated light {} frames over {span} ms, first {:?}, last {:?}, relaid word lit {:?} ms after the relay key; holes {} first {:?} | {}",
                    vac.len(),
                    vac.first(),
                    vac.last(),
                    relaid_lit_at,
                    s.holes_seen.len(),
                    s.holes_seen.first(),
                    declines(&s).join(" ; ")
                );
                eprintln!("{line}");
                if span > 200 || relaid_lit_at.is_none() {
                    fails.push(line);
                }
            }
        }
    }
    assert!(
        fails.is_empty(),
        "F2 reproduced in {} takes:\n{}",
        fails.len(),
        fails.join("\n")
    );
}

/// F4a: the band rides with its text through a hidden repaint. The owner's
/// long mid-line insert with the app silent for seconds, its catch-up
/// relaying the tail and growing the box, drawn whole, torn at the hand's row
/// or torn before the park: the band is not retired, no reappearance is
/// declined as a hidden relocation, and the hand's text is lit.
#[test]
fn probe_f4a_synthetic_torn_catchup() {
    let insert = "(versus system wide) is running and what disk space is being used and especially all the subtrees and such. so that every processtree of the system stays visible at a glance";
    let mut takes = Vec::new();
    for (k0, stall, lag) in [
        (45usize, 4000u64, 2u64),
        (45, 4000, 550),
        (30, 2500, 300),
        (45, 3000, 550),
    ] {
        for tear in [None, Some(TearAt::Row(0)), Some(TearAt::Park)] {
            for tear_ms in [400u64, 1900] {
                if tear.is_none() && tear_ms != 400 {
                    continue;
                }
                takes.push((k0, stall, lag, tear, tear_ms));
            }
        }
    }
    let fails: Vec<String> = par_map(&takes, |&(k0, stall, lag, tear, tear_ms)| {
        let mut s = Hosted::new(LABEL);
        s.verbose = true;
        let head = format!("{}{}{}", words_to(TW), words_to(TW), words_to(12));
        s.preload(&format!("{head}{TAIL}"), head.chars().count());
        let row0 = s.caret().0;
        let top0 = top_rule(&s);
        let keys = keys_of(insert, s.ms() + 100, 110);
        let stall_end = keys[k0].0 + stall;
        let renders = renders_for(&keys, |i| {
            if i < k0 {
                2
            } else if keys[i].0 < stall_end {
                stall_end - keys[i].0
            } else {
                lag
            }
        });
        let catch = renders
            .iter()
            .position(|&r| r >= stall_end)
            .expect("the catch-up");
        let tear_spec = tear.map(|t| {
            let t = match t {
                TearAt::Row(_) => TearAt::Row(row0 - 1),
                p => p,
            };
            (catch, t, tear_ms)
        });
        let tear_used = tear_spec.map(|(_, t, _)| t);
        let mut grew_at_catch: Option<u64> = None;
        let mut max_dark_after: (u64, String) = (0, String::new());
        let mut obs = |s: &Sim| {
            let t = s.ms();
            if t > stall_end + tear_ms + 1500 {
                let (cr, cc) = s.caret();
                let live = s.live(cr);
                let lo = live.first().copied().unwrap_or(cc);
                let dark = (lo..cc.saturating_sub(1))
                    .filter(|c| !live.contains(c))
                    .count();
                if dark as u64 > max_dark_after.0 {
                    max_dark_after = (
                        dark as u64,
                        format!("t={t} row {cr} live {live:?} caret {cc}"),
                    );
                }
            }
            if grew_at_catch.is_none() && top_rule(s) < top0 {
                grew_at_catch = Some(t);
            }
        };
        s.run(&keys, &renders, tear_spec, &mut obs);
        let end = s.ms() + 600;
        s.advance(end, &mut obs);
        s.cov = Some((s.caret().0, 2));
        let st = s.glow.v2_status();
        let judged_after = s
            .glow
            .admission_log()
            .filter(|r| {
                r.at.duration_since(s.t0).as_millis() as u64 > stall_end + tear_ms + 200
            })
            .count();
        let line = format!(
            "k0 {k0} stall {stall} lag {lag} tear {tear_used:?}/{tear_ms}: stall_end {stall_end} grew_at {grew_at_catch:?} caret {:?} {} retired {:?} followed {:?} judged_after_catchup {judged_after} max_dark_after {:?} holes {} | {}",
            s.caret(),
            s.dark(),
            st.map(|s| s.retired),
            st.map(|s| s.followed),
            max_dark_after,
            s.holes_seen.len(),
            declines(&s).join(" ; ")
        );
        eprintln!("{line}");
        let hidden = declines(&s).iter().any(|l| l.contains("hidden-relocation"));
        (st.is_some_and(|st| st.retired > 20) || hidden || s.dark() != "dark[0]=")
            .then_some(line)
    })
    .into_iter()
    .flatten()
    .collect();
    assert!(
        fails.is_empty(),
        "F4 reproduced in {} takes:\n{}",
        fails.len(),
        fails.join("\n")
    );
}

/// F4b: a held park never swallows later moves. F2's word crossing the right
/// edge with the app silent from the key that fills the row while the hand
/// types on; the catch-up relays the word to the next row (a cross-row move,
/// every stamp stale, the presses unpaid) and later keys echo at once: every
/// later move is judged, and the keys typed after the catch-up are lit.
#[test]
fn probe_f4b_park_custody() {
    let mut fails = Vec::new();
    for stall in [600u64, 1500, 2500] {
        let mut s = Hosted::new(LABEL);
        s.verbose = true;
        let head = words_to(60);
        s.preload(&format!("{head}{TAIL}"), head.chars().count());
        let lead = words_to(42);
        let typed = format!("{lead}processtree of the system stays visible at a glance");
        let keys = keys_of(&typed, s.ms() + 100, 110);
        let t_stall = keys[lead.len() + 6].0;
        let release = t_stall + stall;
        let renders = renders_for(&keys, |i| {
            if keys[i].0 >= t_stall && keys[i].0 < release {
                release - keys[i].0
            } else {
                2
            }
        });
        s.run(&keys, &renders, None, &mut |_| {});
        let end = s.ms() + 300;
        s.advance(end, &mut |_| {});
        let after: Vec<String> = s
            .glow
            .admission_log()
            .filter(|r| r.at.duration_since(s.t0).as_millis() as u64 > release + 50)
            .map(|r| format!("{:?} {} {:?}->{:?}", r.phase, r.reason, r.origin, r.target))
            .collect();
        let (cr, cc) = s.caret();
        let live = s.live(cr);
        let dark = (2..cc).filter(|c| !live.contains(c)).count();
        let tally = s.glow.in_flight_tally();
        let line = format!(
            "stall {stall}: caret ({cr},{cc}) dark {dark} of {} on the hand's row; verdicts after the catch-up {} (last {:?}); parks returned {} flushed {} | {}",
            cc.saturating_sub(2),
            after.len(),
            after.last(),
            tally.park_returns,
            tally.park_flushed,
            declines(&s).join(" ; ")
        );
        eprintln!("{line}");
        if after.len() < 5 || dark > 2 {
            fails.push(line);
        }
    }
    assert!(
        fails.is_empty(),
        "F4b reproduced in {} takes:\n{}",
        fails.len(),
        fails.join("\n")
    );
}

/// F5's control: a Backspace held (33 ms repeat) mid-line right after a box
/// flap, the last typed key wrapping the tail and growing the box and the
/// second Backspace shrinking it, with no twin glyph sliding into an erased
/// column: the cells the hand retreats from keep their light no more than a
/// few frames.
#[test]
fn probe_f5_backspace_hold_retract_linger() {
    let mut fails = Vec::new();
    for echo in [2u64, 12] {
        for (held, over) in [(6usize, 1usize), (6, 2), (12, 2)] {
            let mut s = Hosted::new(LABEL);
            s.verbose = true;
            let typed = "lorem ipsum dolor sit amet";
            let head = words_to(TW - TAIL.len() - typed.len() + over);
            s.preload(&format!("{head}{TAIL}"), head.chars().count());
            let row0 = s.caret().0;
            let mut keys = keys_of(typed, s.ms() + 100, 110);
            let last = keys.last().expect("a key").0;
            for k in 0..held {
                keys.push((last + 150 + 33 * k as u64, BACKSPACE));
            }
            let renders = renders_for(&keys, |_| echo);
            let bs0 = keys[typed.len()].0;
            let mut linger: Vec<String> = Vec::new();
            let mut obs = |s: &Sim| {
                let t = s.ms();
                if t < bs0 + 34 {
                    return;
                }
                let (cr, cc) = s.caret();
                let lit: Vec<(u16, u8)> = (cc..cc + held as u16)
                    .filter(|&c| s.live(cr).contains(&c))
                    .map(|c| (c, light(s, cr, c)))
                    .collect();
                if !lit.is_empty() {
                    linger.push(format!("t={t} caret ({cr},{cc}) lit right {lit:?}"));
                }
            };
            s.run(&keys, &renders, None, &mut obs);
            let end = s.ms() + 800;
            s.advance(end, &mut obs);
            let line = format!(
                "echo {echo} held {held} over {over}: row0 {row0} caret {:?} lingering frames {} first {:?} last {:?} holes {} | {}",
                s.caret(),
                linger.len(),
                linger.first(),
                linger.last(),
                s.holes_seen.len(),
                declines(&s).join(" ; ")
            );
            eprintln!("{line}");
            if linger.len() > 6 {
                fails.push(line);
            }
        }
    }
    assert!(
        fails.is_empty(),
        "F5 reproduced in {} takes:\n{}",
        fails.len(),
        fails.join("\n")
    );
}

/// F5: an erased cell's light goes with its text, not its glyph. `…all the
/// subtr` typed mid-line before the tail, the box growing at `t` and
/// shrinking at the second Backspace, then Backspace held: a twin glyph of
/// the tail that slides into an erased column neither keeps nor restores that
/// cell's light, so no live cell stands right of the caret beside retracting
/// ones.
#[test]
fn probe_f5_twin_slides_into_an_erased_cell() {
    let mut fails = Vec::new();
    let typed = "especially all the subtr";
    for echo in [2u64, 12, 30] {
        for repeat in [33u64, 50] {
            for starved in [false, true] {
                let mut s = Hosted::new(LABEL);
                s.verbose = true;
                let head = words_to(TW - TAIL.len() - typed.len() + 2);
                s.preload(&format!("{head}{TAIL}"), head.chars().count());
                let mut keys = keys_of(typed, s.ms() + 100, 110);
                let last = keys.last().expect("a key").0;
                for k in 0..10 {
                    keys.push((last + 230 + repeat * k as u64, BACKSPACE));
                }
                let bs0 = keys[typed.len()].0;
                if starved {
                    s.starved = (0..16)
                        .map(|k| (bs0 + 50 * k + 17, bs0 + 50 * k + 50))
                        .collect();
                }
                let renders = renders_for(&keys, |_| echo);
                let mut seen: Vec<String> = Vec::new();
                let mut obs = |s: &Sim| {
                    if s.ms() < bs0 {
                        return;
                    }
                    let (cr, cc) = s.caret();
                    let Some(rib) = s.glow.v2_ribbon() else {
                        return;
                    };
                    let text: Vec<char> = s.row_text(cr).chars().collect();
                    let live_right: Vec<(u16, char)> = rib
                        .cells()
                        .iter()
                        .filter(|x| x.row == cr && x.col >= cc && !x.leaving())
                        .map(|x| (x.col, text.get(usize::from(x.col)).copied().unwrap_or('?')))
                        .collect();
                    let leaving_right = rib
                        .cells()
                        .iter()
                        .any(|x| x.row == cr && x.col >= cc && x.leaving());
                    if !live_right.is_empty() && leaving_right {
                        seen.push(format!(
                            "t={} caret ({cr},{cc}) live right {live_right:?}",
                            s.ms()
                        ));
                    }
                };
                s.run(&keys, &renders, None, &mut obs);
                let end = s.ms() + 600;
                s.advance(end, &mut obs);
                let flapped = s
                    .log
                    .iter()
                    .filter(|l| l.contains("RENDER"))
                    .any(|l| l.contains("caret->(49"));
                let line = format!(
                    "echo {echo} repeat {repeat} starved {starved}: flapped {flapped} right-live frames {} first {:?}",
                    seen.len(),
                    seen.first()
                );
                eprintln!("{line}");
                if !seen.is_empty() {
                    fails.push(line);
                }
            }
        }
    }
    assert!(
        fails.is_empty(),
        "F5 reproduced in {} takes:\n{}",
        fails.len(),
        fails.join("\n")
    );
}

/// Grid row count for recorded `ptyrec` takes.
pub(crate) const LIVE_ROWS: usize = 60;
/// Grid column count for recorded `ptyrec` takes.
pub(crate) const LIVE_COLS: usize = 146;

/// Replay state for a recorded `ptyrec` take through the shipping single-pane present.
pub(crate) struct Live {
    pub(crate) term: Terminal,
    pub(crate) glow: PresentedGlow,
    cfg: GlowConfig,
    pub(crate) g: Geom,
    t0: Instant,
    pub(crate) now: Instant,
    buf: Vec<char>,
    nbrs: [Vec<char>; 2],
    witness: Vec<char>,
    last_blink: Option<Instant>,
    blink: u64,
    scroll: Option<ContentScrollState>,
    hold: FrameHold,
    /// Frames presented.
    pub(crate) frames: u64,
    /// Frames presented while the program's frame was unfinished.
    pub(crate) torn: u64,
    last_seq: u64,
    /// Every admission verdict (`"{ms}ms ring {phase} {reason} {licence}
    /// {origin}->{target}"`) and content scroll (`"{ms}ms SCROLL {delta}"`),
    /// in order.
    pub(crate) ring: Vec<String>,
}

impl Live {
    fn new() -> Self {
        let now = Instant::now();
        let mut glow = PresentedGlow::default();
        glow.note_pane_columns(0, LIVE_COLS);
        Self {
            term: Terminal::new(LIVE_ROWS as u16, LIVE_COLS as u16),
            glow,
            cfg: cfg(),
            g: crate::common::geom(LIVE_ROWS, LIVE_COLS, CW, CH),
            t0: now,
            now,
            buf: Vec::new(),
            nbrs: [Vec::new(), Vec::new()],
            witness: Vec::new(),
            last_blink: None,
            blink: 0,
            scroll: None,
            hold: FrameHold::default(),
            frames: 0,
            torn: 0,
            last_seq: 0,
            ring: Vec::new(),
        }
    }

    /// Milliseconds elapsed since the take's first record.
    pub(crate) fn ms(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_millis() as u64
    }

    fn process(&mut self, bytes: &[u8]) {
        let at = ClockReading {
            monotonic: self.now,
            wall_ms: None,
        };
        self.term.process_at(bytes, at);
    }

    /// One present; false when the hold withheld it.
    fn frame(&mut self) -> bool {
        if self.hold.withholds(&self.term, self.now) {
            return false;
        }
        if self.term.sync_frame_unfinished() {
            self.torn += 1;
        }
        let d = ContentScrollState::delta_since(self.scroll, self.term.content_scroll_state());
        if !matches!(
            d,
            ContentScrollDelta::Baseline | ContentScrollDelta::Unchanged
        ) {
            self.ring.push(format!("{:>6}ms SCROLL {d:?}", self.ms()));
        }
        self.scroll = Some(self.term.content_scroll_state());
        present(
            &self.term,
            self.now,
            Glass {
                glow: &mut self.glow,
                cfg: &self.cfg,
                g: self.g,
                buf: &mut self.buf,
                nbrs: &mut self.nbrs,
                blink: &mut self.blink,
                last_blink: &mut self.last_blink,
                witness: &mut self.witness,
            },
        );
        self.frames += 1;
        let t0 = self.t0;
        let last_seq = self.last_seq;
        for r in self.glow.admission_log().filter(|r| r.seq > last_seq) {
            self.last_seq = self.last_seq.max(r.seq);
            self.ring.push(format!(
                "{:>6}ms ring {:?} {} {} {:?}->{:?}",
                r.at.saturating_duration_since(t0).as_millis(),
                r.phase,
                r.reason,
                r.licence,
                r.origin,
                r.target
            ));
        }
        true
    }

    /// The press `b` stamped at `now`: the key it is (`\u{8}` a Backspace,
    /// `→` navigation), or `None` for input that stamps nothing.
    fn key(&mut self, b: &[u8]) -> Option<char> {
        let home_end = b.starts_with(b"\x1b[") && matches!(b.last(), Some(b'H' | b'F'));
        match b {
            [c] if is_key(b) => {
                let ch = char::from(*c);
                let (shifted, class) = class_of(ch);
                self.glow
                    .note_typed_expected(self.now, 1, shifted && ch != ' ', class, ch);
                Some(ch)
            }
            [0x7f] | [0x08] => {
                self.glow.clear_typed_keeping_hand(self.now);
                self.glow.note_backspace_erasing(self.now, Some(1));
                Some('\u{8}')
            }
            _ if is_arrow(b) || home_end => {
                self.glow.clear_typed_keeping_hand(self.now);
                self.glow.note_motion(self.now);
                Some('\u{2192}')
            }
            _ => None,
        }
    }
}

/// Frame train phase and period (in microseconds) for a replay.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Train {
    pub(crate) phase_us: u64,
    pub(crate) period_us: u64,
}

impl Default for Train {
    fn default() -> Self {
        Self {
            phase_us: 0,
            period_us: FRAME_US,
        }
    }
}

/// Replay event passed to the observer callback.
pub(crate) enum Step {
    /// A frame presented.
    Frame,
    /// A key stamped at its press ([`Live`]'s doc says which).
    Key,
    /// The program's bytes, processed.
    Out,
}

/// How long a replay runs past its last record, in ms: the in-flight patience
/// (the engine's `IN_FLIGHT_PATIENCE_S`, 10 s), past which no press licenses
/// anything, and half a second.
const REPLAY_TAIL_MS: u64 = 10_500;

/// Replay a `lab ptyrec` recording `src` on `train` through [`REPLAY_TAIL_MS`] past its last event.
pub(crate) fn replay(src: &str, train: Train, obs: &mut dyn FnMut(&Live, u64, Step)) -> Live {
    assert!(
        train.period_us > 0,
        "a replay frame period must be positive"
    );
    let rec = recording(src, 1_000_000);
    let mut h = Live::new();
    h.now += Duration::from_micros(train.phase_us);
    let period = Duration::from_micros(train.period_us);
    let end = rec.last().map_or(0, |(ms, _)| *ms) + REPLAY_TAIL_MS;
    for (ms, ev) in rec {
        let t = h.t0 + Duration::from_millis(ms);
        while h.now + period <= t {
            h.now += period;
            if h.frame() {
                obs(&h, h.ms(), Step::Frame);
            }
        }
        h.now = h.now.max(t);
        match ev {
            Rec::In(b) => {
                if h.key(&b).is_some() {
                    obs(&h, ms, Step::Key);
                }
            }
            Rec::Out(b) => {
                h.process(&b);
                obs(&h, ms, Step::Out);
            }
        }
    }
    while h.ms() < end {
        h.now += period;
        if h.frame() {
            obs(&h, h.ms(), Step::Frame);
        }
    }
    h
}

/// Summary metrics collected over a [`replay`] run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ReplaySummary {
    /// `ribbon_retired=` at the end.
    pub(crate) retired: u64,
    /// `ribbon_followed=` at the end.
    pub(crate) followed: u64,
    /// `park_flushed=` at the end.
    pub(crate) park_flushed: u64,
    /// The longest stretch, in ms, the hand typed through with no verdict on
    /// a caret move.
    pub(crate) blind_ms: u64,
    /// The longest stretch, in ms, from 300 ms to 1.5 s after the caret
    /// relays to the next row's indent, that the row it left stayed planned
    /// lit on two or more columns past the one after its last glyph (the
    /// break Space).
    pub(crate) vacated_lit_ms: u64,
}

/// [`ReplaySummary`]'s census, one step at a time.
#[derive(Default)]
struct ReplayCensus {
    ring_seen: usize,
    last_key: u64,
    prev_caret: (u16, u16),
    unjudged_since: Option<u64>,
    left_row: Option<(u16, u64)>,
    vac_since: Option<u64>,
    out: ReplaySummary,
}

impl ReplayCensus {
    fn see(&mut self, h: &Live, ms: u64, step: &Step) {
        match step {
            Step::Key => self.last_key = ms,
            Step::Out => {}
            Step::Frame => self.frame(h, ms),
        }
    }

    fn frame(&mut self, h: &Live, ms: u64) {
        let verdict = h.ring[self.ring_seen..]
            .iter()
            .any(|l| !l.contains("(3, ") && !l.contains("SCROLL"));
        self.ring_seen = h.ring.len();
        let c = h.term.cursor();
        let caret = (c.row, c.col);
        let moved = h.term.cursor_visible() && caret != self.prev_caret && caret.0 > 4;
        if verdict {
            self.unjudged_since = None;
        } else if moved && self.unjudged_since.is_none() {
            self.unjudged_since = Some(ms);
        }
        if let Some(since) = self.unjudged_since
            && self.last_key > since
        {
            self.out.blind_ms = self.out.blind_ms.max(self.last_key - since);
        }
        if moved {
            if caret.0 == self.prev_caret.0 + 1 && caret.1 < 20 {
                self.left_row = Some((self.prev_caret.0, ms));
                self.vac_since = None;
            }
            self.prev_caret = caret;
        }
        let Some((r, since)) = self.left_row else {
            return;
        };
        if ms > since + 1500 {
            self.left_row = None;
            self.vac_since = None;
        } else if h.glow.v2_ribbon().is_some() {
            let cov = coverage(&h.glow, h.g, r);
            let mut cols = Vec::new();
            h.term.row_cols_into(usize::from(r), &mut cols);
            let last = cols
                .iter()
                .rposition(|&ch| ch != ' ' && ch != '\0')
                .unwrap_or(0);
            let past = (last + 2..cov.len()).filter(|&i| cov[i] >= LIT_COV).count();
            if past >= 2 && ms > since + 300 {
                let s0 = *self.vac_since.get_or_insert(ms);
                self.out.vacated_lit_ms = self.out.vacated_lit_ms.max(ms - s0);
            } else {
                self.vac_since = None;
            }
        }
    }

    fn summary(self, h: &Live) -> ReplaySummary {
        let v2 = h.glow.v2_status();
        ReplaySummary {
            retired: v2.map_or(0, |s| s.retired),
            followed: v2.map_or(0, |s| s.followed),
            park_flushed: h.glow.in_flight_tally().park_flushed,
            ..self.out
        }
    }
}

/// Replay `src` on the default 120 Hz train and return its summary and final [`Live`] state.
pub(crate) fn replay_take(src: &str) -> (ReplaySummary, Live) {
    let mut census = ReplayCensus::default();
    let h = replay(src, Train::default(), &mut |h, ms, step| {
        census.see(h, ms, &step);
    });
    (census.summary(&h), h)
}

/// Committed `ptyrec` take metadata and payload.
pub(crate) struct Take {
    pub(crate) id: &'static str,
    pub(crate) src: &'static str,
}

/// Committed fullscreen composer `ptyrec` takes.
pub(crate) const TAKES: &[Take] = &[
    Take {
        id: "g-boxgrow",
        src: include_str!("../fixtures/claude-boxgrow-2026-10-17.ptylog"),
    },
    Take {
        id: "r-boxgrow2",
        src: include_str!("../fixtures/claude-boxgrow-2026-10-17-again.ptylog"),
    },
    Take {
        id: "g-boxgrow11",
        src: include_str!("../fixtures/claude-boxgrow-2026-10-17-eleven.ptylog"),
    },
    Take {
        id: "s2-boxgrow-h",
        src: include_str!("../fixtures/claude-boxgrow-2026-10-17-starved-whole.ptylog"),
    },
    Take {
        id: "s3-boxgrow-h2",
        src: include_str!("../fixtures/claude-boxgrow-2026-10-17-starved-whole2.ptylog"),
    },
    Take {
        id: "s3-boxgrow-m",
        src: include_str!("../fixtures/claude-boxgrow-2026-10-17-starved-torn.ptylog"),
    },
    Take {
        id: "r-long2d",
        src: include_str!("../fixtures/claude-long-insert-2026-10-17.ptylog"),
    },
    Take {
        id: "g-long2",
        src: include_str!("../fixtures/claude-long-insert-2026-10-17-again.ptylog"),
    },
    Take {
        id: "s3-long2-m",
        src: include_str!("../fixtures/claude-long-insert-2026-10-17-starved-torn.ptylog"),
    },
    Take {
        id: "s4-long2-m2",
        src: include_str!("../fixtures/claude-long-insert-2026-10-17-starved-wrap.ptylog"),
    },
];

/// Look up a committed take's payload by `id`.
pub(crate) fn take(id: &str) -> &'static str {
    TAKES
        .iter()
        .find(|t| t.id == id)
        .map(|t| t.src)
        .unwrap_or_else(|| panic!("no committed take {id}"))
}

/// The live families on their own takes' bytes.
#[test]
fn probe_live_families() {
    type Law = fn(&ReplaySummary) -> bool;
    let rows: [(&str, &str, &str, Law); 10] = [
        ("F1", "g-boxgrow", "retired 0", |s| s.retired == 0),
        ("F1", "r-boxgrow2", "retired 0", |s| s.retired == 0),
        ("F1", "g-boxgrow11", "retired 0", |s| s.retired == 0),
        ("F2", "r-long2d", "vacated lit < 200 ms", |s| {
            s.vacated_lit_ms < 200
        }),
        ("F2", "g-long2", "vacated lit < 200 ms", |s| {
            s.vacated_lit_ms < 200
        }),
        ("F3", "s2-boxgrow-h", "no park flushed", |s| {
            s.park_flushed == 0
        }),
        ("F3", "s3-boxgrow-h2", "no park flushed", |s| {
            s.park_flushed == 0
        }),
        ("F4a", "s3-long2-m", "retired <= 10", |s| s.retired <= 10),
        ("F4b", "s4-long2-m2", "blind < 1.5 s", |s| {
            s.blind_ms < 1_500
        }),
        ("F4b", "s3-long2-m", "blind < 1.5 s", |s| s.blind_ms < 1_500),
    ];
    let mut ids: Vec<&str> = Vec::new();
    for r in &rows {
        if !ids.contains(&r.1) {
            ids.push(r.1);
        }
    }
    let sums = par_map(&ids, |id| {
        let (summary, h) = replay_take(take(id));
        (summary, h.ring)
    });
    let mut fails = Vec::new();
    for (fam, id, law, holds) in rows {
        let (s, ring) = &sums[ids.iter().position(|&i| i == id).expect("replayed")];
        let s = *s;
        let line = format!("{fam} {id} ({law}): {s:?}");
        eprintln!("{line}");
        if !holds(&s) {
            eprintln!("{id} full failing replay ring:\n{}", ring.join("\n"));
            fails.push(line);
        }
    }
    assert!(
        fails.is_empty(),
        "{} live rows break their law:\n{}",
        fails.len(),
        fails.join("\n")
    );
}
