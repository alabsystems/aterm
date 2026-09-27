// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **THE TRAIL HOST** (2026-09-25) — the one GUI-faithful frame seam that
//! `tests/copies_are_not_moves.rs` and `tests/moves_are_followed.rs` drive
//! their case families through, and `tests/erased_under_its_twin.rs`,
//! `tests/copy_beside_the_line.rs`, `tests/moved_again_without_a_key.rs` and
//! `tests/composer_multiline_wrap.rs` their laws (each declares
//! `#[macro_use] mod trail_host;`).
//!
//! A real `aterm_core::terminal::Terminal` driven byte for byte; each frame
//! (`Core::frame`) is the GUI's: SYNC-1 withholding when asked, the
//! content-scroll seam first (a scroll or band move is applied to the engine
//! and the frame samples NOTHING, as `app_render.rs` does), then LOCK A — the
//! caret row's probe, which is also its witness sample, and every row
//! `CursorGlow::ribbon_rows` names into the GUI's `CURSOR_WITNESS_ROWS`
//! slots, read AFTER the last `process` — then the tick. What differs between the families is an explicit [`Opts`]; the
//! host options a family sweeps (`far_ok`: the composed host's far-row read
//! dropped; `drop_far_frames`: one such drop; a pet's frame demand) are
//! fields of [`Core`]. [`PacedHost`] frames time on a flat 16 ms or 8 ms
//! train or at the GUI's real pacing ([`Pace`]).
//!
//! Every case is registered by id ([`case`]) and judged by the reading its
//! family states ([`Outcome::ff`]: `(followed, frames the row no key wrote
//! was lit)` against `(0, 0)`; [`Outcome::mf`]: lines or moves that went dark
//! under text that stayed, against `0`). A law runs the cases under one id
//! prefix ([`holds`], [`holds_where`]) and each must hold — or, where the law
//! states LIMITS, a rule over the ids names them with its reason
//! ([`holds_but`]): each case the rule names must still read bad and every
//! other must hold, so a change to a limit is seen and the law is restated.
//!
//! **THE GRID AND ITS SPINE.** A family's grid is the product of what it
//! sweeps (widths, key rates, pauses, tear lengths, pacings). A release
//! build, or `TRAIL_LAWS_FULL=1`, runs every case of it — and the merge
//! gate's test run sets it (`aterm_verify::stages::TRAIL_LAWS_FULL`), so the
//! gate enforces every rule on the full grid; a plain debug run runs its
//! SPINE ([`sweep`]): the values that cross each stated threshold on both
//! sides, and the ones 0.93.0 was RED on, so every law still holds a case
//! that must hold and every limit rule still names a case — which
//! [`holds_but`] asserts. The counts the laws quote (`RED on 0.93.0 8/8`)
//! are of the full grid.
#![allow(dead_code)]

use aterm_core::render::GlowQuad;
use aterm_core::terminal::{ContentScrollDelta, ContentScrollState, Terminal};
use aterm_effects::cursor_glow::{CURSOR_WITNESS_ROWS, CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::kitty_pet::{PetBrain, PetSense};
use aterm_effects::rainbow_kitty::TypedClass;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// OUTCOMES AND CASES
// ---------------------------------------------------------------------------

/// One case's reading and verdict.
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    pub followed: u64,
    pub retired: u64,
    pub wrong_lit: u64,
    pub dark: u64,
    pub ok: bool,
    pub note: String,
}

impl Outcome {
    /// A FALSE-FOLLOW reading: `(followed, frames the wrong row was lit)`
    /// against `(0, 0)`.
    pub fn ff(followed: u64, retired: u64, lit: usize) -> Self {
        Self {
            followed,
            retired,
            wrong_lit: lit as u64,
            dark: 0,
            ok: followed == 0 && lit == 0,
            note: String::new(),
        }
    }

    /// A MISSED-FOLLOW / composer reading: `dark` lines or moves that went
    /// out under text that stayed, against `0`.
    pub fn mf(followed: u64, retired: u64, dark: u64) -> Self {
        Self {
            followed,
            retired,
            wrong_lit: 0,
            dark,
            ok: dark == 0,
            note: String::new(),
        }
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = note.into();
        self
    }
}

pub struct Case {
    pub id: String,
    pub class: &'static str,
    pub run: Box<dyn Fn() -> Outcome + Send + Sync>,
}

pub const FF: &str = "false-follow";
pub const MF: &str = "missed-follow";
pub const CO: &str = "composer";

pub fn case(
    v: &mut Vec<Case>,
    id: impl Into<String>,
    class: &'static str,
    run: impl Fn() -> Outcome + Send + Sync + 'static,
) {
    v.push(Case {
        id: id.into(),
        class,
        run: Box::new(run),
    });
}

/// A law's running tally: the metrics it read, and every assertion that
/// failed (a failed assertion does not stop the law — each is one `dark`).
#[derive(Default)]
pub struct Law {
    pub followed: u64,
    pub retired: u64,
    pub wrong_lit: u64,
    pub fails: Vec<String>,
}

impl Law {
    pub fn check(&mut self, cond: bool, what: impl FnOnce() -> String) {
        if !cond {
            self.fails.push(what());
        }
    }

    pub fn eq<T: PartialEq + std::fmt::Debug>(&mut self, got: T, want: T, what: &str) {
        if got != want {
            self.fails
                .push(format!("{what}: got {got:?} want {want:?}"));
        }
    }

    pub fn add(&mut self, followed: u64, retired: u64) {
        self.followed += followed;
        self.retired += retired;
    }

    pub fn done(self) -> Outcome {
        let dark = self.fails.len() as u64;
        Outcome {
            followed: self.followed,
            retired: self.retired,
            wrong_lit: self.wrong_lit,
            dark,
            ok: dark == 0,
            note: self.fails.join(" | "),
        }
    }
}

/// `"git status"` → `git_status`: a case-id component.
pub fn slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .collect()
}

/// `[2, 2, 2]` → `2x3`, `[1, 2]` → `1-2`, `[4]` → `4`.
pub fn gstr(g: &[usize]) -> String {
    if g.len() > 1 && g.iter().all(|&x| x == g[0]) {
        format!("{}x{}", g[0], g.len())
    } else {
        g.iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
            .join("-")
    }
}

pub fn istr(g: &[i32]) -> String {
    g.iter()
        .map(|x| format!("{x:+}"))
        .collect::<Vec<_>>()
        .join("")
}

// ---------------------------------------------------------------------------
// THE SHARED HOST CORE
// ---------------------------------------------------------------------------

/// The frame seam's variants, as the case families use them.
#[derive(Clone, Copy, Debug)]
pub struct Opts {
    /// `sync_cursor_effect_scroll` first: a scroll / band move is applied to
    /// the engine and that frame samples NOTHING (as `app_render.rs`).
    pub scroll_seam: bool,
    /// `sync_cursor_effect_coordinate_space`: an alt-screen switch
    /// re-baselines the scroll clock.
    pub alt_rebaseline: bool,
    /// The caret row, already fed as the row probe, is not read again among
    /// the named rows.
    pub skip_caret: bool,
    /// SYNC-1: no frame while a `?2026` bracket is open and dirty.
    pub honour_sync: bool,
    /// `note_pane_columns(0, cols)` at creation.
    pub pane_columns: bool,
}

/// The implementer's LOCK-A-only host (no scroll seam; every named row read).
pub const LOCK_A: Opts = Opts {
    scroll_seam: false,
    alt_rebaseline: false,
    skip_caret: false,
    honour_sync: false,
    pane_columns: false,
};

#[derive(Clone, Copy)]
pub enum Theme {
    /// fg `0xC8D3F5`, bg `0x1A1B26`.
    Tokyo,
    /// fg `0xD0D0D0`, bg `0x111318` (the composer captures').
    Default,
}

pub fn cfg(theme: Theme) -> GlowConfig {
    let (theme_fg, theme_bg) = match theme {
        Theme::Tokyo => (0x00C8_D3F5, 0x001A_1B26),
        Theme::Default => (0x00D0_D0D0, 0x0011_1318),
    };
    GlowConfig {
        classic_mono: false,
        ribbon_tall: true,
        ribbon_flat: false,
        enabled: true,
        dark_theme: true,
        theme_fg,
        theme_bg,
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
        pack: None,
    }
}

/// One terminal, one glow engine, one clock, one frame seam.
pub struct Core {
    pub term: Terminal,
    pub glow: CursorGlow,
    pub cfg: GlowConfig,
    pub g: Geom,
    pub now: Instant,
    pub t0: Instant,
    pub out: Vec<GlowQuad>,
    pub row_buf: Vec<char>,
    pub blink_seen: u64,
    pub scroll: Option<ContentScrollState>,
    pub alt_seen: Option<bool>,
    pub o: Opts,
    /// `false`: EVERY frame's far-row read (rows beyond the caret's `±1`) is
    /// dropped — the composed host's second, generation-checked read losing
    /// its race while output streams.
    pub far_ok: bool,
    /// The next `n` frames' far-row reads are dropped (one-shot staleness).
    pub drop_far_frames: u32,
    /// ms since `t0` of every frame run.
    pub frames: Vec<u64>,
    /// `rainbow kitty pet`: the resident pet's own frame demand.
    pub pet: Option<PetBrain>,
}

impl Core {
    pub fn new(rows: usize, cols: usize, cw: usize, ch: usize, theme: Theme, o: Opts) -> Self {
        let g = Geom {
            cw,
            ch,
            rows,
            cols,
            origin_x: 0,
            origin_y: 0,
            win_w: (cols * cw) as u16,
            win_h: (rows * ch) as u16,
            head: 0,
        };
        let mut glow = CursorGlow::default();
        if o.pane_columns {
            glow.note_pane_columns(0, cols);
        }
        let now = Instant::now();
        Self {
            term: Terminal::new(rows as u16, cols as u16),
            glow,
            cfg: cfg(theme),
            g,
            now,
            t0: now,
            out: Vec::new(),
            row_buf: Vec::new(),
            blink_seen: 0,
            scroll: None,
            alt_seen: None,
            o,
            far_ok: true,
            drop_far_frames: 0,
            frames: Vec::new(),
            pet: None,
        }
    }

    /// ONE FRAME: (SYNC-1), the content-scroll seam, LOCK A's samples (the
    /// caret row's probe and the rows the witness names), the tick.
    pub fn frame(&mut self) {
        if self.o.honour_sync && self.term.sync_open_dirty() {
            return;
        }
        self.frames
            .push(self.now.duration_since(self.t0).as_millis() as u64);
        let alt = self.term.is_alternate_screen();
        let mut changed = false;
        if self.o.scroll_seam {
            if self.o.alt_rebaseline {
                if self.alt_seen.is_some_and(|a| a != alt) {
                    self.scroll = None;
                }
                self.alt_seen = Some(alt);
            }
            let scroll = self.term.content_scroll_state();
            match ContentScrollState::delta_since(self.scroll, scroll) {
                ContentScrollDelta::Baseline | ContentScrollDelta::Unchanged => {}
                ContentScrollDelta::Translate(rows) => {
                    self.glow.note_scroll(rows);
                    self.glow.drop_row_probe();
                    changed = true;
                }
                ContentScrollDelta::Bands { first_seq, count } => {
                    for i in 0..u64::from(count) {
                        let m = scroll.band(first_seq + i);
                        self.glow.note_band_move(m.top, m.bottom, m.delta);
                    }
                    self.glow.drop_row_probe();
                    changed = true;
                }
                ContentScrollDelta::Invalidate => {
                    self.glow.curtain(self.now);
                    changed = true;
                }
            }
            self.scroll = Some(scroll);
        }
        let c = self.term.cursor();
        let cur = self.term.cursor_visible().then_some((c.row, c.col));
        let epoch = self.term.repaint_blink_epoch();
        if epoch != self.blink_seen {
            self.blink_seen = epoch;
            self.glow.note_repaint_blink(self.now);
        }
        self.glow.note_context(alt);
        let one_shot = if self.drop_far_frames > 0 {
            self.drop_far_frames -= 1;
            true
        } else {
            false
        };
        let drop_far = one_shot || !self.far_ok;
        if !changed {
            self.term
                .row_cols_into(usize::from(c.row), &mut self.row_buf);
            self.glow.observe_row(c.row, c.col, &self.row_buf, self.now);
            self.glow.observe_ribbon_row(c.row, &self.row_buf);
            let mut rows = [0u16; CURSOR_WITNESS_ROWS];
            let n = self.glow.ribbon_rows(&mut rows);
            for &r in &rows[..n] {
                if self.o.skip_caret && r == c.row {
                    continue;
                }
                if drop_far && r.abs_diff(c.row) > 1 {
                    continue;
                }
                self.term.row_cols_into(usize::from(r), &mut self.row_buf);
                self.glow.observe_ribbon_row(r, &self.row_buf);
            }
        }
        self.glow
            .tick(cur, self.now, &self.cfg, self.g, &mut self.out);
        if let Some(pet) = self.pet.as_mut() {
            let _ = pet.tick(PetSense {
                caret_drawn: cur.is_some(),
                now: self.now,
                caret: Some((c.row, c.col)),
                wrapped: false,
                rows: self.g.rows as u16,
                cols: self.g.cols as u16,
                cell_w: self.g.cw as u16,
                cell_h: self.g.ch as u16,
                reduced_motion: false,
                output_burst: false,
                pointer: None,
            });
        }
    }

    pub fn ribbon(&self) -> &aterm_effects::rainbow_kitty::ribbon::Ribbon {
        self.glow.v2_ribbon().expect("rainbow kitty owns the frame")
    }

    /// Resident ribbon cells on `row`: `(col, leaving)`, sorted.
    pub fn cells(&self, row: u16) -> Vec<(u16, bool)> {
        let mut v: Vec<(u16, bool)> = self
            .glow
            .v2_ribbon()
            .map(|r| {
                r.cells()
                    .iter()
                    .filter(|c| c.row == row)
                    .map(|c| (c.col, c.leaving()))
                    .collect()
            })
            .unwrap_or_default();
        v.sort_unstable();
        v
    }

    /// The columns of `row` with a live (not leaving) ribbon cell.
    pub fn live(&self, row: u16) -> Vec<u16> {
        self.cells(row)
            .into_iter()
            .filter(|&(_, l)| !l)
            .map(|(c, _)| c)
            .collect()
    }

    /// The bed's quads covering the CENTRE of `row`'s pixel band.
    pub fn row_quads(&self, row: u16) -> impl Iterator<Item = &GlowQuad> {
        let centre = f32::from(self.g.origin_y) + (f32::from(row) + 0.5) * self.g.ch as f32;
        self.glow.under_quads().iter().filter(move |q| {
            let y0 = f32::from(q.y);
            let y1 = y0 + f32::from(q.h);
            q.w > 0 && q.alpha > 0 && y0 <= centre && centre < y1
        })
    }

    /// Whether any BODY quad covers the CENTRE of `row`'s pixel band.
    pub fn lit(&self, row: u16) -> bool {
        self.row_quads(row).next().is_some()
    }

    /// The share of `row`'s pixel width the bed covers, opacity-weighted.
    pub fn coverage(&self, row: u16) -> f32 {
        let sum: f32 = self
            .row_quads(row)
            .map(|q| f32::from(q.w) * f32::from(q.alpha) / 255.0)
            .sum();
        sum / (self.g.cols * self.g.cw) as f32
    }

    pub fn followed(&self) -> u64 {
        self.glow.v2_status().map_or(0, |s| s.followed)
    }

    /// `(ribbon_followed=, ribbon_retired=)`.
    pub fn counts(&self) -> (u64, u64) {
        self.glow
            .v2_status()
            .map_or((0, 0), |s| (s.followed, s.retired))
    }

    pub fn t(&self) -> u64 {
        self.now.duration_since(self.t0).as_millis() as u64
    }

    pub fn frames_since(&self, t: u64) -> usize {
        self.frames.iter().filter(|&&f| f > t).count()
    }

    /// The rows the host would sample this frame (the witness's own list).
    pub fn named(&self) -> Vec<u16> {
        let mut rows = [0u16; CURSOR_WITNESS_ROWS];
        let n = self.glow.ribbon_rows(&mut rows);
        rows[..n].to_vec()
    }
}

macro_rules! core_deref {
    ($t:ty) => {
        impl ::std::ops::Deref for $t {
            type Target = Core;
            fn deref(&self) -> &Core {
                &self.c
            }
        }
        impl ::std::ops::DerefMut for $t {
            fn deref_mut(&mut self) -> &mut Core {
                &mut self.c
            }
        }
    };
}

// ---------------------------------------------------------------------------
// THE PACED HOST
// ---------------------------------------------------------------------------

/// How a [`PacedHost`] frames time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pace {
    /// A frame every `n` ms, whatever the engine asks for.
    Flat(u64),
    /// The GUI's real pacing for the style "rainbow kitty" (no pet): 16 ms
    /// while `needs_frame_cadence`, else at `next_change_deadline` —
    /// 110–130 ms apart once a key's light has settled.
    Paced,
    /// The same, with the default style's resident pet asking for frames.
    Pet,
}

impl Pace {
    /// The id segment: `fr16`, `fr8`, `paced`, `pet`.
    pub fn tag(self) -> String {
        match self {
            Pace::Flat(n) => format!("fr{n}"),
            Pace::Paced => "paced".into(),
            Pace::Pet => "pet".into(),
        }
    }
}

/// Every pacing a family sweeps.
pub const PACES: [Pace; 4] = [Pace::Flat(16), Pace::Flat(8), Pace::Paced, Pace::Pet];

/// A 24×80 (or any) [`Core`] with the scroll seam, the alt-screen
/// re-baseline and LOCK A (the caret row not re-read), framed by a
/// [`Pace`]; SYNC-1 when `honour_sync`.
pub struct PacedHost {
    pub c: Core,
    pub pace: Pace,
}
core_deref!(PacedHost);

impl PacedHost {
    pub fn new(rows: usize, cols: usize, pace: Pace, honour_sync: bool) -> Self {
        let mut c = Core::new(
            rows,
            cols,
            8,
            16,
            Theme::Tokyo,
            Opts {
                scroll_seam: true,
                alt_rebaseline: true,
                skip_caret: true,
                honour_sync,
                pane_columns: false,
            },
        );
        if matches!(pace, Pace::Pet) {
            c.pet = Some(PetBrain::default());
        }
        c.frame();
        Self { c, pace }
    }

    /// Let `ms` pass with the frames the host would draw in that time (no
    /// output lands).
    pub fn advance(&mut self, ms: u64) {
        let end = self.now + Duration::from_millis(ms);
        match self.pace {
            Pace::Flat(f) => {
                let fd = Duration::from_millis(f);
                while self.now + fd <= end {
                    self.now += fd;
                    self.frame();
                }
                self.now = end;
            }
            Pace::Paced | Pace::Pet => {
                let fd = Duration::from_millis(16);
                loop {
                    let glow = if self.glow.needs_frame_cadence() {
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
                    let next = match (glow, pet) {
                        (Some(a), Some(b)) => Some(a.min(b)),
                        (a, b) => a.or(b),
                    };
                    match next {
                        Some(t) if t <= end => {
                            self.now = t.max(self.now + Duration::from_millis(1));
                            self.frame();
                        }
                        _ => break,
                    }
                }
                self.now = end;
            }
        }
    }

    /// Output lands NOW and the host presents a frame for it.
    pub fn land(&mut self, bytes: &[u8]) {
        self.term.process(bytes);
        self.frame();
    }

    /// A key `gap` ms after the last event, echoed with `bytes` 2 ms after
    /// the press.
    pub fn key(&mut self, ch: char, bytes: &[u8], gap: u64) {
        self.advance(gap.saturating_sub(2));
        let class = if ch == ' ' {
            TypedClass::Space
        } else {
            TypedClass::Glyph
        };
        let now = self.now;
        self.glow.note_typed_glyph(now, 1, false, class);
        self.advance(2);
        self.land(bytes);
    }

    /// `s` typed a key at a time, `gap` ms apart, each echoed alone.
    pub fn type_plain(&mut self, s: &str, gap: u64) {
        for ch in s.chars() {
            let mut b = [0u8; 4];
            self.key(ch, ch.encode_utf8(&mut b).as_bytes(), gap);
        }
    }
}

// ---------------------------------------------------------------------------
// THE GRID, AND THE LAW RUNNER
// ---------------------------------------------------------------------------

/// Whether the laws run every case of each family's grid: in a release
/// build, or with `TRAIL_LAWS_FULL` set (to anything but `0`). A debug build
/// runs each grid's spine ([`sweep`]).
pub fn full_grid() -> bool {
    !cfg!(debug_assertions) || std::env::var_os("TRAIL_LAWS_FULL").is_some_and(|v| v != "0")
}

/// **ONE AXIS OF A FAMILY'S GRID**: every value of `all` under the full grid
/// ([`full_grid`]), only `spine` otherwise. The spine is drawn from `all`.
pub fn sweep<T: Clone + PartialEq + std::fmt::Debug>(all: &[T], spine: &[T]) -> Vec<T> {
    assert!(
        spine.iter().all(|x| all.contains(x)),
        "the spine {spine:?} is not drawn from {all:?}"
    );
    if full_grid() {
        all.to_vec()
    } else {
        spine.to_vec()
    }
}

/// Runs every case of `cases` whose id satisfies `pick`, in order, and fails
/// listing every one that does not read as it must: bad where `limit` names
/// it (a limit that holds now is a finding too — restate the law), held
/// everywhere else. Panics inside a case propagate: an engine assertion is a
/// failure of the law.
fn run_law(cases: Vec<Case>, pick: impl Fn(&str) -> bool, limit: impl Fn(&str) -> bool) {
    let picked: Vec<Case> = cases.into_iter().filter(|c| pick(&c.id)).collect();
    assert!(!picked.is_empty(), "the law picks no case");
    let mut broken = Vec::new();
    for c in &picked {
        let o = (c.run)();
        let is_limit = limit(&c.id);
        if o.ok == is_limit {
            broken.push(format!(
                "{} [{}] {}: followed={} retired={} wrong_lit={} dark={} {}",
                c.id,
                c.class,
                if is_limit {
                    "a stated LIMIT now holds — restate the law"
                } else {
                    "BROKEN"
                },
                o.followed,
                o.retired,
                o.wrong_lit,
                o.dark,
                o.note
            ));
        }
    }
    assert!(
        broken.is_empty(),
        "{} of {} cases:\n{}",
        broken.len(),
        picked.len(),
        broken.join("\n")
    );
}

/// **A FAMILY HOLDS**: every case whose id satisfies `pick` holds.
pub fn holds_where(cases: Vec<Case>, pick: impl Fn(&str) -> bool) {
    run_law(cases, pick, |_| false);
}

/// [`holds_where`] for the cases whose id starts with `prefix`.
pub fn holds(cases: Vec<Case>, prefix: &str) {
    holds_where(cases, |id| id.starts_with(prefix));
}

/// **A FAMILY HOLDS, BUT FOR ITS STATED LIMITS** — a rule over the picked
/// ids that says WHY each is one (a tear past a measured threshold, a move
/// onto a row no band names): every picked case the rule names must read
/// bad, and every other must hold. Neither side may be empty — a rule that
/// names no case states nothing, and one that names every case leaves
/// nothing to hold — so a grid's spine ([`sweep`]) must keep a case on each.
pub fn holds_but(cases: Vec<Case>, pick: impl Fn(&str) -> bool, limit: impl Fn(&str) -> bool) {
    let named = cases.iter().filter(|c| pick(&c.id) && limit(&c.id)).count();
    let picked = cases.iter().filter(|c| pick(&c.id)).count();
    assert!(named > 0, "the limit rule names none of the {picked} cases");
    assert!(
        named < picked,
        "the limit rule names every one of the {picked} cases"
    );
    run_law(cases, pick, limit);
}
