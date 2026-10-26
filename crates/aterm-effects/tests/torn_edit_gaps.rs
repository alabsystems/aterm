// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests that torn edit and erase repaints leave no interior holes in cursor-glow coverage.

use crate::common::{
    LIT_COV, caret_sample, class_of, coverage, holes_of, is_arrow, is_key, note_blink,
    observe_caret_row, observe_ribbon_rows, rainbow_cfg, sync_scroll, unhex,
};
use aterm_core::render::GlowQuad;
use aterm_core::terminal::{ContentScrollState, Terminal};
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig};
use aterm_effects::rainbow_kitty::ribbon::SWOOSH_TOTAL_S;
use std::time::{Duration, Instant};

const CW: usize = 8;
const CH: usize = 16;
const ROWS: usize = 60;
const COL0: usize = 4;
const KEY_MS: u64 = 83;
const FRAME_MS: u64 = 16;
const SPINNER_MS: u64 = 100;
const PTY_READ: usize = 1024;

const LINE: &str = "zoom out. first of all, STOP LANDING BRANCHES AND PATCHES! YOU NEED TO FUCKING MERGE ALL BEST WORK INTO MAIN!!!!";

const FILLER: [&str; 8] = [
    "⏺ I'll start by reading the harness I'm told to copy, then the ribbon module doc.",
    "⏺ Bash(ls -la crates/aterm-effects/tests/ && wc -l crates/aterm-effects/tests/scrub_gaps.rs)",
    "  ⎿  total 1560 — 48 entries, 789 lines in scrub_gaps.rs, 486 in wrapped_composer_band.rs",
    "⏺ Read(crates/aterm-effects/src/rainbow_kitty/ribbon.rs)",
    "  ⎿  Read 420 lines: the module ledger of every prior gap defect and the law that closed it",
    "⏺ The event handler shows the insert path routes through Event::Rewrite and the witness.",
    "⏺ Bash(TRUST_NO_MIGRATE_WARN=1 targo --unverified test -p aterm-effects --test scrub_gaps)",
    "  ⎿  test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; finished in 0.21s",
];
const SPIN: [char; 4] = ['✻', '✶', '✳', '✢'];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Split {
    Whole,
    TextCol(usize),
    Chunks,
}

fn sgr_pad(n: usize) -> String {
    assert!(n == 0 || n >= 3);
    if n == 0 {
        return String::new();
    }
    let mut s = "\x1b[m".repeat(n / 3 - 1);
    s.push_str(["\x1b[m", "\x1b[0m", "\x1b[00m"][n % 3]);
    s
}

type HoleAt = (usize, u64, String, u16, Vec<(usize, usize)>);

#[derive(Clone, Debug)]
struct RowC {
    row: u16,
    lit: String,
    holes: Vec<(usize, usize)>,
}

#[derive(Clone, Debug)]
struct Frame {
    ms: u64,
    label: String,
    rows: Vec<RowC>,
    retiring: Vec<(u16, u16)>,
    released: Vec<(u16, u16)>,
    live: Vec<(u16, u16)>,
    fp: u64,
    text: Vec<(u16, String)>,
}

struct Host {
    term: Terminal,
    glow: CursorGlow,
    cfg: GlowConfig,
    cols: usize,
    w: usize,
    t0: Instant,
    now: Instant,
    last_event: Instant,
    out: Vec<GlowQuad>,
    row_buf: Vec<char>,
    blink: u64,
    scroll: Option<ContentScrollState>,
    text: String,
    caret: usize,
    transcript: Vec<String>,
    working: bool,
    spinner: Option<Instant>,
    spin_i: usize,
    key_split: Split,
    spin_split: Split,
    pad: usize,
    min_rows: usize,
    height: usize,
    top: usize,
    label: String,
    frames: Vec<Frame>,
    torn: Vec<usize>,
    watch: Vec<u16>,
}

impl Host {
    fn new(cols: usize, min_rows: usize, spinner: bool, pad: usize) -> Self {
        let mut term = Terminal::new(ROWS as u16, cols as u16);
        let now = Instant::now();
        let mut glow = crate::common::quads_glow();
        glow.note_pane_columns(0, cols);
        term.process(b"\x1b[?1049h\x1b[2J");
        let mut h = Self {
            term,
            glow,
            cfg: rainbow_cfg(),
            cols,
            w: cols - 6,
            t0: now,
            now,
            last_event: now,
            out: Vec::new(),
            row_buf: Vec::new(),
            blink: 0,
            scroll: None,
            text: String::new(),
            caret: 0,
            transcript: FILLER.iter().map(|s| (*s).to_string()).collect(),
            working: spinner,
            spinner: spinner.then_some(now + Duration::from_millis(SPINNER_MS)),
            spin_i: 0,
            key_split: Split::Whole,
            spin_split: Split::Whole,
            pad,
            min_rows,
            height: 0,
            top: 0,
            label: "start".into(),
            frames: Vec::new(),
            torn: Vec::new(),
            watch: Vec::new(),
        };
        h.repaint(Split::Whole);
        h
    }

    fn geom(&self) -> Geom {
        crate::common::geom(ROWS, self.cols, CW, CH)
    }

    fn ms(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_millis() as u64
    }

    fn frame(&mut self) {
        sync_scroll(&self.term, &mut self.glow, &mut self.scroll, self.now);
        let (c, cur) = caret_sample(&self.term);
        note_blink(&self.term, &mut self.glow, &mut self.blink, self.now);
        self.glow.note_context(self.term.is_alternate_screen());
        observe_caret_row(&self.term, &mut self.glow, &mut self.row_buf, c, self.now);
        observe_ribbon_rows(&self.term, &mut self.glow, &mut self.row_buf);
        let g = self.geom();
        let fp = self.glow.tick(cur, self.now, &self.cfg, g, &mut self.out);
        self.record(fp);
    }

    fn record(&mut self, fp: u64) {
        let cols = self.cols;
        let mut cov = vec![0u8; ROWS * cols];
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        for sg in rib.plan_segments() {
            let r = ((sg.spine / CH as f32) - 0.5).floor();
            let c = (sg.x / CW as f32).floor();
            if r >= 0.0 && c >= 0.0 && (r as usize) < ROWS && (c as usize) < cols {
                let i = r as usize * cols + c as usize;
                cov[i] = cov[i].max(sg.cov);
            }
        }
        let mut rows = Vec::new();
        for r in 0..ROWS {
            let lane = &cov[r * cols..(r + 1) * cols];
            if lane.iter().all(|&v| v < LIT_COV) {
                continue;
            }
            rows.push(RowC {
                row: r as u16,
                lit: lane
                    .iter()
                    .map(|&v| if v >= LIT_COV { '#' } else { '.' })
                    .collect(),
                holes: holes_of(lane),
            });
        }
        let mut retiring: Vec<(u16, u16)> = rib
            .cells()
            .iter()
            .filter(|c| c.retire_at.is_some())
            .map(|c| (c.row, c.col))
            .collect();
        let mut released: Vec<(u16, u16)> = rib
            .cells()
            .iter()
            .filter(|c| c.released_at.is_some())
            .map(|c| (c.row, c.col))
            .collect();
        let mut live: Vec<(u16, u16)> = rib
            .cells()
            .iter()
            .filter(|c| !c.leaving())
            .map(|c| (c.row, c.col))
            .collect();
        for v in [&mut retiring, &mut released, &mut live] {
            v.sort_unstable();
            v.dedup();
        }
        let mut text = Vec::new();
        for &r in &self.watch {
            self.term.row_cols_into(usize::from(r), &mut self.row_buf);
            let s: String = self
                .row_buf
                .iter()
                .map(|&c| if c == '\0' { ' ' } else { c })
                .collect();
            text.push((r, s.trim_end().to_string()));
        }
        self.frames.push(Frame {
            ms: self.ms(),
            label: self.label.clone(),
            rows,
            retiring,
            released,
            live,
            fp,
            text,
        });
    }

    fn step(&mut self) {
        self.now += Duration::from_millis(FRAME_MS);
        self.frame();
    }

    fn idle_to(&mut self, t: Instant) {
        loop {
            let next = self.now + Duration::from_millis(FRAME_MS);
            if let Some(due) = self.spinner
                && due <= t
                && due <= next
            {
                self.now = self.now.max(due);
                self.spinner = Some(due + Duration::from_millis(SPINNER_MS));
                self.spin_i += 1;
                let keep = std::mem::replace(&mut self.label, "spinner".into());
                self.repaint(self.spin_split);
                self.label = keep;
                continue;
            }
            if next > t {
                break;
            }
            self.step();
        }
    }

    fn idle(&mut self, ms: u64) {
        let t = self.now + Duration::from_millis(ms);
        self.idle_to(t);
    }

    fn schedule(&mut self, ms: u64) {
        let t = self.last_event + Duration::from_millis(ms);
        self.idle_to(t);
        self.now = self.now.max(t);
        self.last_event = self.now;
    }

    fn text_rows(&self) -> usize {
        let by_len = self.text.len().div_ceil(self.w);
        let by_caret = self.caret / self.w + 1;
        by_len.max(by_caret).max(self.min_rows).max(1)
    }

    fn text_row0(&self) -> u16 {
        (self.top - 1 + self.transcript_shown() + 3) as u16
    }

    fn transcript_shown(&self) -> usize {
        FILLER.len()
    }

    fn frame_bytes(&self) -> (Vec<u8>, Vec<usize>, usize) {
        let cols = self.cols;
        let clip = |s: &str| -> String { s.chars().take(cols - 1).collect() };
        let shown = self.transcript_shown();
        let mut rows: Vec<String> = self.transcript[self.transcript.len() - shown..]
            .iter()
            .map(|s| clip(s))
            .collect();
        rows.push(if self.working {
            format!(
                "{} Clauding… ({}s · ↓ 2.3k tokens)",
                SPIN[self.spin_i % SPIN.len()],
                40 + (self.spin_i / 10) % 50
            )
        } else {
            String::new()
        });
        rows.push(String::new());
        rows.push(format!("╭{}╮", "─".repeat(cols - 2)));
        let first_text = rows.len();
        let n = self.text_rows();
        let b = self.text.as_bytes();
        for k in 0..n {
            let lo = (k * self.w).min(b.len());
            let hi = ((k + 1) * self.w).min(b.len());
            let chunk = &self.text[lo..hi];
            let pad = self.w - chunk.len();
            rows.push(format!(
                "│ {} {}{} │",
                if k == 0 { '>' } else { ' ' },
                chunk,
                " ".repeat(pad)
            ));
        }
        rows.push(format!("╰{}╯", "─".repeat(cols - 2)));
        rows.push("  ? for shortcuts".to_string());
        let height = rows.len();
        let top = ROWS - height + 1;
        let mut s = String::from("\x1b[?2026h\x1b[?25l");
        s.push_str(&sgr_pad(self.pad));
        if self.height > 0 && height > self.height {
            s.push_str(&format!("\x1b[{ROWS};1H"));
            s.push_str(&"\n".repeat(height - self.height));
        } else if self.height > 0 && height < self.height {
            let old_top = ROWS - self.height + 1;
            for r in old_top..top {
                s.push_str(&format!("\x1b[{r};1H\x1b[2K"));
            }
        }
        s.push_str(&format!("\x1b[{top};1H"));
        let mut offs = Vec::new();
        for (i, r) in rows.iter().enumerate() {
            s.push_str("\x1b[2K");
            if i >= first_text && i < first_text + n {
                offs.push(s.len() + "│ > ".len());
            }
            s.push_str(r);
            if i + 1 < rows.len() {
                s.push_str("\r\n");
            }
        }
        let crow = top + first_text + self.caret / self.w;
        let ccol = COL0 + self.caret % self.w + 1;
        s.push_str(&format!("\x1b[{crow};{ccol}H\x1b[?25h\x1b[?2026l"));
        (s.into_bytes(), offs, top)
    }

    fn repaint(&mut self, split: Split) {
        let (bytes, offs, top) = self.frame_bytes();
        let n_rows = offs.len();
        let mut cuts: Vec<usize> = Vec::new();
        let len = self.text.len();
        let w = self.w;
        let text_cut = |j: usize| -> usize {
            if j >= len {
                let last = (len.saturating_sub(1) / w).min(n_rows - 1);
                offs[last] + (len - (last * w).min(len))
            } else {
                offs[j / w] + j % w
            }
        };
        let inside = |c: usize| -> bool {
            (0..n_rows).any(|k| {
                let lo = (k * w).min(len);
                let hi = ((k + 1) * w).min(len);
                hi > lo && c >= offs[k] && c < offs[k] + (hi - lo)
            }) || (n_rows > 1 && len > w && c > offs[0] && c < offs[n_rows - 1])
        };
        match split {
            Split::Whole => {}
            Split::TextCol(j) => cuts.push(text_cut(j)),
            Split::Chunks => {
                cuts.extend((1..).map(|k| k * PTY_READ).take_while(|&p| p < bytes.len()))
            }
        }
        let mut start = 0;
        for &c in &cuts {
            self.term.process(&bytes[start..c]);
            if inside(c) {
                self.torn.push(self.frames.len());
            }
            self.frame();
            self.now += Duration::from_millis(1);
            start = c;
        }
        self.term.process(&bytes[start..]);
        self.height = ROWS - top + 1;
        self.top = top;
        self.frame();
    }

    fn key_after(&mut self, ch: char, ms: u64) {
        self.schedule(ms);
        self.label = format!("key {ch:?}");
        let (shifted, class) = class_of(ch);
        self.glow.note_typed_glyph(self.now, 1, shifted, class);
        self.text.insert(self.caret, ch);
        self.caret += 1;
        self.repaint(self.key_split);
    }

    fn type_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.key_after(ch, KEY_MS);
        }
    }

    fn hop_left(&mut self, ms: u64) {
        self.schedule(ms);
        self.label = "opt-left".into();
        self.glow.note_motion(self.now);
        let b = self.text.as_bytes();
        let mut i = self.caret;
        while i > 0 && b[i - 1] == b' ' {
            i -= 1;
        }
        while i > 0 && b[i - 1] != b' ' {
            i -= 1;
        }
        self.caret = i;
        self.repaint(Split::Whole);
    }

    fn kill_to_end(&mut self, ms: u64, split: Split) {
        self.schedule(ms);
        self.label = "ctrl-k".into();
        self.glow.note_kill(self.now, false);
        self.text.truncate(self.caret);
        self.repaint(split);
    }

    fn holes_on(&self, rows: &[u16]) -> Vec<HoleAt> {
        let mut v = Vec::new();
        for (i, f) in self.frames.iter().enumerate() {
            for rc in &f.rows {
                if rows.contains(&rc.row) && !rc.holes.is_empty() {
                    v.push((i, f.ms, f.label.clone(), rc.row, rc.holes.clone()));
                }
            }
        }
        v
    }

    fn lit_of(&self, i: usize, row: u16) -> String {
        self.frames[i]
            .rows
            .iter()
            .find(|r| r.row == row)
            .map_or_else(|| ".".repeat(self.cols), |r| r.lit.clone())
    }

    fn dump(&self, i: usize, rows: &[u16]) -> String {
        let f = &self.frames[i];
        let mut s = format!(
            "  f{i:<5} t={:>6} {:<12} fp={}\n",
            f.ms,
            f.label,
            u64::from(f.fp != 0)
        );
        for &r in rows {
            let text = f
                .text
                .iter()
                .find(|(rr, _)| *rr == r)
                .map_or(String::new(), |(_, t)| t.clone());
            let rel: Vec<u16> = f
                .released
                .iter()
                .filter(|c| c.0 == r)
                .map(|c| c.1)
                .collect();
            let ret: Vec<u16> = f
                .retiring
                .iter()
                .filter(|c| c.0 == r)
                .map(|c| c.1)
                .collect();
            s.push_str(&format!(
                "    r{r:<3} text |{}\n    r{r:<3} lit  |{}  rel={rel:?} ret={ret:?}\n",
                text,
                self.lit_of(i, r)
                    .chars()
                    .take(text.chars().count() + 4)
                    .collect::<String>()
            ));
        }
        s
    }

    fn story(&self, rows: &[u16]) -> String {
        let holes = self.holes_on(rows);
        let mut s = format!(
            "{} frames, {} with a hole on {rows:?}; torn presents {:?}\n",
            self.frames.len(),
            holes.len(),
            &self.torn[..self.torn.len().min(20)]
        );
        if let Some(&(i, ..)) = holes.first() {
            for k in i.saturating_sub(3)..(i + 3).min(self.frames.len()) {
                s.push_str(&self.dump(k, rows));
            }
        }
        s
    }

    fn assert_idle_to_zero(&mut self, what: &str) {
        self.idle((SWOOSH_TOTAL_S * 1000.0) as u64 + 1500);
        for _ in 0..10 {
            self.step();
            let f = self.frames.last().expect("a frame");
            let cells = self
                .glow
                .v2_ribbon()
                .expect("rainbow kitty owns the frame")
                .cells()
                .len();
            assert_eq!(
                f.fp, 0,
                "{what}: T6 — fingerprint 0 at idle (cells {cells})"
            );
            assert!(
                !self.glow.needs_frame_cadence(),
                "{what}: T6 — no frame cadence at idle"
            );
            assert_eq!(
                self.glow
                    .next_change_deadline(self.now, Duration::from_millis(FRAME_MS)),
                None,
                "{what}: T6 — no deadline at idle"
            );
            assert!(
                f.rows.is_empty(),
                "{what}: nothing lit at idle: {:?}",
                f.rows.iter().map(|r| r.row).collect::<Vec<_>>()
            );
        }
    }
}

fn pad_for(cols: usize, min_rows: usize, spinner: bool, j: usize, text_len: usize) -> usize {
    let mut h = Host::new(cols, min_rows, spinner, 0);
    h.text = "x".repeat(text_len);
    h.caret = text_len;
    h.height = 0;
    let (_, offs, _) = h.frame_bytes();
    let off = offs[j / h.w] + j % h.w;
    let n = (PTY_READ - off % PTY_READ) % PTY_READ;
    if n < 3 { n + PTY_READ } else { n }
}

fn assert_clean(h: &Host, rows: &[u16], what: &str) {
    let holes = h.holes_on(rows);
    assert!(
        holes.is_empty(),
        "{what}: {} frames with an interior hole, first {:?}\n{}",
        holes.len(),
        holes.first().map(|x| (x.1, &x.2, x.3, &x.4)),
        h.story(rows)
    );
}

fn assert_no_melt(h: &Host, rows: &[u16], what: &str) {
    if let Some(i) = h
        .frames
        .iter()
        .position(|f| f.retiring.iter().any(|c| rows.contains(&c.0)))
    {
        let mut s = String::new();
        for k in i.saturating_sub(2)..(i + 2).min(h.frames.len()) {
            s.push_str(&h.dump(k, rows));
        }
        panic!("{what}: text that came straight back took the melt\n{s}");
    }
}

fn run_takes<T: Copy + Sync>(takes: Vec<(String, T)>, f: impl Fn(&str, T) + Sync) {
    let n = takes.len();
    let failed: Vec<String> = crate::common::par_map(&takes, |(what, t)| {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(what, *t)));
        r.err().map(|e| {
            let msg = e
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| (*s).to_string()))
                .unwrap_or_default();
            format!("{what}: {}", msg.lines().next().unwrap_or(""))
        })
    })
    .into_iter()
    .flatten()
    .collect();
    assert!(
        failed.is_empty(),
        "{} of {n} takes failed:\n  {}",
        failed.len(),
        failed.join("\n  ")
    );
}

mod codex {
    use super::*;

    const C_ROWS: usize = 32;
    const C_COLS: usize = 100;
    const C_CW: usize = 7;
    const C_CH: usize = 14;
    const C_FRAME_MS: u64 = 8;
    const END_MS: u64 = 9_500;
    const FIXTURE: &str = include_str!("fixtures/codex-particles-2026-09-16.ptylog");

    fn g() -> Geom {
        crate::common::geom(C_ROWS, C_COLS, C_CW, C_CH)
    }

    struct C {
        term: Terminal,
        glow: CursorGlow,
        cfg: GlowConfig,
        t0: Instant,
        now: Instant,
        out: Vec<GlowQuad>,
        row_buf: Vec<char>,
        blink: u64,
        scroll: Option<ContentScrollState>,
        composer_row: Option<u16>,
        broken: Vec<(u64, String)>,
        lit_max: usize,
        honor_sync: bool,
        sync_since: Option<Instant>,
    }

    impl C {
        fn frame(&mut self) {
            sync_scroll(&self.term, &mut self.glow, &mut self.scroll, self.now);
            let (c, cur) = caret_sample(&self.term);
            note_blink(&self.term, &mut self.glow, &mut self.blink, self.now);
            self.glow.note_context(self.term.is_alternate_screen());
            observe_caret_row(&self.term, &mut self.glow, &mut self.row_buf, c, self.now);
            observe_ribbon_rows(&self.term, &mut self.glow, &mut self.row_buf);
            self.glow.tick(cur, self.now, &self.cfg, g(), &mut self.out);
            let Some(row) = self.composer_row else { return };
            let cov = coverage(&self.glow, g(), row);
            self.lit_max = self
                .lit_max
                .max(cov.iter().filter(|&&v| v >= LIT_COV).count());
            if !holes_of(&cov).is_empty() {
                let ms = self.now.saturating_duration_since(self.t0).as_millis() as u64;
                self.broken.push((
                    ms,
                    cov.iter()
                        .map(|&v| if v >= LIT_COV { '#' } else { '.' })
                        .collect(),
                ));
            }
        }

        fn present_ok(&mut self) -> bool {
            if !self.term.sync_open_dirty() {
                self.sync_since = None;
                return true;
            }
            let since = *self.sync_since.get_or_insert(self.now);
            !self.honor_sync
                || self.now.saturating_duration_since(since) >= Duration::from_millis(150)
        }

        fn run_to(&mut self, ms: u64) {
            let t = self.t0 + Duration::from_millis(ms);
            while self.now + Duration::from_millis(C_FRAME_MS) <= t {
                self.now += Duration::from_millis(C_FRAME_MS);
                if self.present_ok() {
                    self.frame();
                }
            }
            self.now = t;
        }
    }

    /// Replay; `torn` splits every read in half with a present between the
    /// halves, and `honor_sync = false` presents through an open `?2026`
    /// bracket (the hold's cap).
    pub(super) fn replay(torn: bool, honor_sync: bool) -> (Vec<(u64, String)>, usize) {
        let now = Instant::now();
        let mut glow = crate::common::quads_glow();
        glow.note_pane_columns(0, C_COLS);
        let mut h = C {
            term: Terminal::new(C_ROWS as u16, C_COLS as u16),
            glow,
            cfg: GlowConfig {
                intensity: 1.0,
                audible: true,
                ..rainbow_cfg()
            },
            t0: now,
            now,
            out: Vec::new(),
            row_buf: Vec::new(),
            blink: 0,
            scroll: None,
            composer_row: None,
            broken: Vec::new(),
            lit_max: 0,
            honor_sync,
            sync_since: None,
        };
        for l in FIXTURE.lines().filter(|l| !l.is_empty()) {
            let mut it = l.splitn(3, ' ');
            let ms: u64 = it.next().unwrap().parse().unwrap();
            let kind = it.next().unwrap();
            let bytes = unhex(it.next().unwrap());
            if ms > END_MS {
                break;
            }
            h.run_to(ms);
            match kind {
                "O" => {
                    let cut = if torn { bytes.len() / 2 } else { bytes.len() };
                    h.term.process(&bytes[..cut]);
                    if h.present_ok() {
                        h.frame();
                    }
                    if cut < bytes.len() {
                        h.now += Duration::from_millis(1);
                        h.term.process(&bytes[cut..]);
                        if h.present_ok() {
                            h.frame();
                        }
                    }
                }
                "I" => match bytes.as_slice() {
                    b if is_key(b) => {
                        if h.composer_row.is_none() {
                            h.composer_row = Some(h.term.cursor().row);
                        }
                        h.glow.note_typed_cells(h.now, 1);
                    }
                    [0x7f] => h.glow.note_backspace_erasing(h.now, Some(1)),
                    b if is_arrow(b) => h.glow.note_motion(h.now),
                    _ => {}
                },
                k => panic!("bad kind {k}"),
            }
        }
        h.run_to(END_MS);
        (h.broken, h.lit_max)
    }
}

#[test]
fn codex_particle_replay_stays_one_run_whole_and_torn_in_half_under_the_capped_sync_hold() {
    let pairs = [(false, true), (true, true)];
    let replays = crate::common::par_map(&pairs, |&(torn, honor)| {
        std::panic::catch_unwind(|| codex::replay(torn, honor))
    });
    for ((torn, honor), replay) in pairs.into_iter().zip(replays) {
        let (broken, lit_max) = replay.unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        assert!(
            lit_max >= 9,
            "torn={torn} sync={honor}: band laid ({lit_max})"
        );
        assert!(
            broken.is_empty(),
            "torn={torn} honor_sync={honor}: {} frames with a hole, first t={} {}",
            broken.len(),
            broken[0].0,
            broken[0].1
        );
    }
}

#[test]
fn claude_code_1024_byte_reads_with_the_boundary_at_the_wrap_keep_both_rows_whole() {
    let cols = 70;
    let w = cols - 6;
    let mut takes = Vec::new();
    for j in [w - 1, w, w + 1] {
        for spinner in [false, true] {
            takes.push((
                format!("1024-byte reads, boundary at {j}, spinner {spinner}"),
                (j, spinner),
            ));
        }
    }
    run_takes(takes, |what, (j, spinner)| {
        let pad = pad_for(cols, 2, spinner, j, LINE.len());
        let mut h = Host::new(cols, 2, spinner, pad);
        let r0 = h.text_row0();
        h.watch = vec![r0, r0 + 1];
        h.key_split = Split::Chunks;
        h.spin_split = Split::Chunks;
        h.type_str(LINE);
        h.idle(600);
        assert!(h.torn.len() > 3, "{what}: torn {:?}", h.torn);
        assert_clean(&h, &[r0, r0 + 1], what);
        assert_no_melt(&h, &[r0, r0 + 1], what);
        h.assert_idle_to_zero(what);
    });
}

#[test]
fn a_growing_composer_torn_at_its_wrap_keeps_the_band_whole() {
    let cols = 70;
    let w = cols - 6;
    let all: Vec<u16> = (0..ROWS as u16).collect();
    let mut takes = vec![("growing composer, untorn control".to_string(), None)];
    for j in [w - 2, w, w + 2, w + 10] {
        takes.push((format!("growing composer torn at {j}"), Some(j)));
    }
    run_takes(takes, |what, j| {
        let mut h = Host::new(cols, 1, false, 0);
        if let Some(j) = j {
            h.key_split = Split::TextCol(j);
        }
        h.type_str(LINE);
        h.idle(600);
        assert_clean(&h, &all, what);
        h.assert_idle_to_zero(what);
    });
}

fn lit_count(h: &Host, i: usize, row: u16) -> usize {
    h.lit_of(i, row).chars().filter(|&c| c == '#').count()
}

#[test]
fn every_key_in_1024_byte_reads_for_three_seconds_at_one_fixed_boundary_keeps_the_band_whole() {
    let typed = &LINE[..36];
    let mut takes = Vec::new();
    for j in [0usize, 1, 2, 5, 10, 20, 30] {
        for spinner in [false, true] {
            takes.push((
                format!("1024-byte reads, boundary at {j}, spinner {spinner}"),
                (j, spinner),
            ));
        }
    }
    run_takes(takes, |what, (j, spinner)| {
        let pad = pad_for(140, 1, spinner, j, typed.len());
        let mut h = Host::new(140, 1, spinner, pad);
        let row = h.text_row0();
        h.watch = vec![row];
        h.key_split = Split::Chunks;
        h.spin_split = Split::Chunks;
        h.type_str(typed);
        let end = h.frames.len() - 1;
        let lit = lit_count(&h, end, row);
        h.idle(300);
        assert_clean(&h, &[row], what);
        assert!(
            lit >= typed.len() - 2,
            "{what}: {lit} lit at the last key\n{}",
            h.dump(end, &[row])
        );
        assert_no_melt(&h, &[row], what);
        h.assert_idle_to_zero(what);
    });
}

#[test]
fn a_backspace_run_under_torn_reads_keeps_the_band_whole() {
    let typed = &LINE[..60];
    let mut takes = vec![(
        "backspace run, untorn control".to_string(),
        (Split::Whole, 0usize, false),
    )];
    for j in [0usize, 20, 45, 52, 58] {
        takes.push((
            format!("backspace run, every frame torn at {j}"),
            (Split::TextCol(j), 0usize, false),
        ));
    }
    for j in [20usize, 45, 52] {
        takes.push((
            format!("backspace run, 1024-byte reads at {j}, spinner"),
            (Split::Chunks, pad_for(140, 1, true, j, 50), true),
        ));
    }
    run_takes(takes, |what, (split, pad, spinner)| {
        let mut h = Host::new(140, 1, spinner, pad);
        let row = h.text_row0();
        h.watch = vec![row];
        h.type_str(typed);
        h.key_split = split;
        h.spin_split = split;
        for k in 0..10 {
            h.schedule(if k == 0 { 250 } else { 110 });
            h.label = "backspace".into();
            h.glow.note_backspace_erasing(h.now, Some(1));
            h.caret -= 1;
            h.text.remove(h.caret);
            h.repaint(split);
        }
        h.type_str(" and on");
        let end = h.frames.len() - 1;
        h.idle(300);
        assert_clean(&h, &[row], what);
        let dark: Vec<usize> = (COL0..COL0 + h.text.len())
            .filter(|&c| !h.frames[end].live.contains(&(row, c as u16)))
            .collect();
        assert!(
            dark.is_empty(),
            "{what}: at the last key the standing line lost {} cells {dark:?}\n{}",
            dark.len(),
            h.dump(end, &[row])
        );
        h.assert_idle_to_zero(what);
    });
}

#[test]
fn a_torn_backspace_frame_keeps_the_standing_text_lit() {
    let typed = &LINE[..60];
    let mut takes = vec![(
        "one backspace, untorn control".to_string(),
        (Split::Whole, 0usize, false),
    )];
    for j in [10usize, 20, 40, 55] {
        takes.push((
            format!("one backspace torn at {j}"),
            (Split::TextCol(j), 0, false),
        ));
    }
    for j in [20usize, 40] {
        takes.push((
            format!("one backspace, 1024-byte reads at {j}, spinner"),
            (
                Split::Chunks,
                pad_for(140, 1, true, j, typed.len() - 1),
                true,
            ),
        ));
    }
    run_takes(takes, |what, (split, pad, spinner)| {
        let mut h = Host::new(140, 1, spinner, pad);
        let row = h.text_row0();
        h.watch = vec![row];
        h.type_str(typed);
        h.spin_split = split;
        h.schedule(250);
        h.label = "backspace".into();
        h.glow.note_backspace_erasing(h.now, Some(1));
        h.caret -= 1;
        h.text.remove(h.caret);
        h.repaint(split);
        let at = h.frames.len() - 1;
        h.idle(400);
        let standing = COL0..COL0 + h.text.len();
        let t_at = h.frames[at].ms;
        for (i, f) in h.frames.iter().enumerate().skip(at) {
            if f.ms < t_at + 150 {
                continue;
            }
            let lit = h.lit_of(i, row);
            let dark: Vec<usize> = standing
                .clone()
                .filter(|&c| lit.as_bytes()[c] != b'#')
                .collect();
            assert!(
                dark.is_empty(),
                "{what}: +{} ms after the Backspace the standing text is dark on glass at {} cells {}..={}\n{}",
                f.ms - h.frames[at].ms,
                dark.len(),
                dark.first().copied().unwrap_or(0),
                dark.last().copied().unwrap_or(0),
                h.dump(i, &[row])
            );
        }
        h.key_split = Split::Whole;
        h.type_str(" and on");
        h.idle(300);
        h.assert_idle_to_zero(what);
        assert_clean(&h, &[row], what);
    });
}

#[test]
fn a_torn_word_kill_frame_keeps_the_standing_text_lit() {
    let typed = &LINE[..60];
    let mut takes = vec![("word kill, untorn control".to_string(), Split::Whole)];
    for j in [10usize, 20, 40, 50] {
        takes.push((format!("word kill torn at {j}"), Split::TextCol(j)));
    }
    run_takes(takes, |what, split| {
        let mut h = Host::new(140, 1, false, 0);
        let row = h.text_row0();
        h.watch = vec![row];
        h.type_str(typed);
        h.schedule(250);
        h.label = "ctrl-w".into();
        h.glow.note_word_kill(h.now, true);
        let b = h.text.as_bytes();
        let mut i = h.caret;
        while i > 0 && b[i - 1] == b' ' {
            i -= 1;
        }
        while i > 0 && b[i - 1] != b' ' {
            i -= 1;
        }
        h.text.truncate(i);
        h.caret = i;
        h.repaint(split);
        let at = h.frames.len() - 1;
        h.idle(400);
        let standing = COL0..COL0 + h.text.len();
        let t_at = h.frames[at].ms;
        for (k, f) in h.frames.iter().enumerate().skip(at + 1) {
            if f.ms < t_at + 150 {
                continue;
            }
            let lit = h.lit_of(k, row);
            let dark: Vec<usize> = standing
                .clone()
                .filter(|&c| lit.as_bytes()[c] != b'#')
                .collect();
            assert!(
                dark.is_empty(),
                "{what}: +{} ms after the word kill the standing text is dark on glass at {} cells {}..={}\n{}",
                f.ms - h.frames[at].ms,
                dark.len(),
                dark.first().copied().unwrap_or(0),
                dark.last().copied().unwrap_or(0),
                h.dump(k, &[row])
            );
        }
        h.type_str(" and on");
        h.idle(300);
        assert_clean(&h, &[row], what);
    });
}

fn kill_take(split: Split) -> (Host, u16, usize) {
    let mut h = Host::new(140, 1, false, 0);
    let row = h.text_row0();
    h.watch = vec![row];
    h.type_str(LINE);
    h.idle(80);
    for _ in 0..3 {
        h.hop_left(110);
    }
    let caret = h.caret;
    h.kill_to_end(150, split);
    (h, row, caret)
}

#[test]
fn ctrl_k_then_typing_on_keeps_one_run() {
    let mut takes: Vec<(String, (Split, Split))> = [
        Split::Whole,
        Split::TextCol(0),
        Split::TextCol(10),
        Split::TextCol(60),
        Split::TextCol(93),
    ]
    .into_iter()
    .map(|s| (format!("ctrl-k then typing, kill {s:?}"), (s, Split::Whole)))
    .collect();
    for j in [0usize, 10, 60, 93] {
        takes.push((
            format!("ctrl-k then typing, every frame torn at {j}"),
            (Split::TextCol(j), Split::TextCol(j)),
        ));
    }
    run_takes(takes, |what, (kill, keys)| {
        let (mut h, row, caret) = kill_take(kill);
        h.key_split = keys;
        h.key_after('x', 150);
        h.type_str(" and more words here");
        let end = h.frames.len() - 1;
        h.idle(300);
        assert_clean(&h, &[row], what);
        let dark: Vec<usize> = (COL0..COL0 + caret + 21)
            .filter(|&c| !h.frames[end].live.contains(&(row, c as u16)))
            .collect();
        assert!(
            dark.is_empty(),
            "{what}: at the last key the line lost {dark:?}\n{}",
            h.dump(end, &[row])
        );
        h.assert_idle_to_zero(what);
    });
}
