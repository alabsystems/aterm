// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! GUI-faithful frame harness for cursor-trail case families and laws.
#![allow(dead_code)]

use crate::common::{Ground, ground_or, on_ground, rainbow_cfg_on};
use aterm_core::render::GlowQuad;
use aterm_core::terminal::{ContentScrollDelta, ContentScrollState, Terminal};
use aterm_effects::cursor_glow::{CURSOR_WITNESS_ROWS, CursorGlow, Geom, GlowConfig};
use aterm_effects::kitty_pet::{PetBrain, PetSense};
use aterm_effects::rainbow_kitty::TypedClass;
use std::time::{Duration, Instant};

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
    /// A false-follow reading: `(followed, frames the wrong row was lit)`
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

    /// A missed-follow / composer reading: `dark` lines or moves that went
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

#[derive(Default)]
pub struct Law {
    pub followed: u64,
    pub retired: u64,
    pub wrong_lit: u64,
    pub fails: Vec<String>,
}

impl Law {
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

#[derive(Clone, Copy, Debug)]
pub struct Opts {
    /// `sync_cursor_effect_scroll` first: a scroll / band move is applied to
    /// the engine and that frame takes no row probe (as `app_render.rs`); a
    /// whole-grid translation still feeds the content witness the caret's row
    /// and the rows it names after the sync
    /// (`ScrollChange::translated_only`), a band move nothing.
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

/// The frame-hold-only host (no scroll seam; every named row read).
pub const LOCK_A: Opts = Opts {
    scroll_seam: false,
    alt_rebaseline: false,
    skip_caret: false,
    honour_sync: false,
    pane_columns: false,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    Tokyo,
    Default,
    /// `#2b2d33` on `#f6f7f9`.
    DefaultLight,
    GithubLight,
    SolarizedLight,
    Latte,
}

impl Theme {
    /// The light built-ins every law also reads its cases on ([`light_grounds`]).
    pub const LIGHT: [Theme; 4] = [
        Theme::DefaultLight,
        Theme::GithubLight,
        Theme::SolarizedLight,
        Theme::Latte,
    ];

    pub fn ground(self) -> Ground {
        match self {
            Theme::Tokyo => Ground::TOKYO,
            Theme::Default => Ground::scheme("Default"),
            Theme::DefaultLight => Ground::scheme("Default Light"),
            Theme::GithubLight => Ground::scheme("GitHub Light"),
            Theme::SolarizedLight => Ground::scheme("Solarized Light"),
            Theme::Latte => Ground::scheme("Catppuccin Latte"),
        }
    }
}

/// The rainbow kitty config on `theme`'s ground, `dark_theme` taken from
/// `theme_is_dark` as the host takes it.
pub fn cfg(theme: Theme) -> GlowConfig {
    rainbow_cfg_on(theme.ground())
}

/// The light grounds a law reads every case on besides its family's own
/// theme: all of [`Theme::LIGHT`] under the full grid, Default Light on a
/// debug build's spine ([`sweep`]).
pub fn light_grounds() -> Vec<Ground> {
    sweep(&Theme::LIGHT, &[Theme::DefaultLight])
        .into_iter()
        .map(Theme::ground)
        .collect()
}

/// A plain law's body, run on its family's own theme and then on each light
/// ground ([`light_grounds`]); a light ground it breaks on is named.
pub fn on_every_ground(f: impl Fn()) {
    f();
    for g in light_grounds() {
        let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| on_ground(g, &f)));
        if let Err(e) = read {
            let why = e
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| e.downcast_ref::<&str>().copied())
                .unwrap_or("a panic");
            panic!("on {}: {why}", g.name);
        }
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
    /// `false`: every frame's far-row read (rows beyond the caret's `±1`) is
    /// dropped, as when the composed host's generation-checked read loses its
    /// race while output streams.
    pub far_ok: bool,
    /// The next `n` frames' far-row reads are dropped (one-shot staleness).
    pub drop_far_frames: u32,
    /// ms since `t0` of every frame run.
    pub frames: Vec<u64>,
    /// `rainbow kitty pet`: the resident pet's own frame demand.
    pub pet: Option<PetBrain>,
    /// The caret row's flanks (the rows above and below it, `None` off the
    /// grid) go with its probe, as `app_render.rs`'s frame hold sends them
    /// (`observe_neighbor_rows`).
    pub flanks: bool,
}

impl Core {
    /// A host on its family's `theme`, or on the ground an [`on_ground`] names.
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
        let mut glow = crate::common::quads_glow();
        if o.pane_columns {
            glow.note_pane_columns(0, cols);
        }
        let now = Instant::now();
        Self {
            term: Terminal::new(rows as u16, cols as u16),
            glow,
            cfg: rainbow_cfg_on(ground_or(theme.ground())),
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
            flanks: false,
        }
    }

    /// One frame: (SYNC-1), the content-scroll seam, the frame hold's samples
    /// (the caret row's probe and the rows the witness names), the tick.
    pub fn frame(&mut self) {
        if self.o.honour_sync && self.term.sync_open_dirty() {
            return;
        }
        self.frames
            .push(self.now.duration_since(self.t0).as_millis() as u64);
        let alt = self.term.is_alternate_screen();
        let mut changed = false;
        let mut translated = false;
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
                    translated = true;
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
        if !changed || translated {
            self.term
                .row_cols_into(usize::from(c.row), &mut self.row_buf);
            if !changed {
                self.glow.observe_row(c.row, c.col, &self.row_buf, self.now);
                if self.flanks {
                    let rows = self.g.rows as u16;
                    let read = |term: &Terminal, row: Option<u16>| {
                        row.filter(|&r| r < rows).map(|r| {
                            let mut cols = Vec::new();
                            term.row_cols_into(usize::from(r), &mut cols);
                            cols
                        })
                    };
                    let above = read(&self.term, c.row.checked_sub(1));
                    let below = read(&self.term, c.row.checked_add(1));
                    self.glow
                        .observe_neighbor_rows(above.as_deref(), below.as_deref());
                }
            }
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

    /// The bed's quads covering the centre of `row`'s pixel band.
    pub fn row_quads(&self, row: u16) -> impl Iterator<Item = &GlowQuad> {
        let centre = f32::from(self.g.origin_y) + (f32::from(row) + 0.5) * self.g.ch as f32;
        self.glow.under_quads().iter().filter(move |q| {
            let y0 = f32::from(q.y);
            let y1 = y0 + f32::from(q.h);
            q.w > 0 && q.alpha > 0 && y0 <= centre && centre < y1
        })
    }

    /// Whether any bed quad covers the centre of `row`'s pixel band.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pace {
    Flat(u64),
    Paced,
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
/// re-baseline and the frame hold (the caret row not re-read), framed by a
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

    /// Output lands now and the host presents a frame for it.
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

/// Whether the laws run every case of each family's grid: in a release
/// build, or with `TRAIL_LAWS_FULL` set (to anything but `0`). A debug build
/// runs each grid's spine ([`sweep`]).
pub fn full_grid() -> bool {
    !cfg!(debug_assertions) || std::env::var_os("TRAIL_LAWS_FULL").is_some_and(|v| v != "0")
}

/// One axis of a family's grid: every value of `all` under the full grid
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

/// Every picked case on its family's own theme and on each light ground
/// ([`light_grounds`]), one verdict for all: a stated limit reads bad on
/// every ground.
fn run_law(cases: Vec<Case>, pick: impl Fn(&str) -> bool, limit: impl Fn(&str) -> bool) {
    let picked: Vec<Case> = cases.into_iter().filter(|c| pick(&c.id)).collect();
    assert!(!picked.is_empty(), "the law picks no case");
    let grounds: Vec<Option<Ground>> = std::iter::once(None)
        .chain(light_grounds().into_iter().map(Some))
        .collect();
    let runs: Vec<(&Case, Option<Ground>)> = picked
        .iter()
        .flat_map(|c| grounds.iter().map(move |&g| (c, g)))
        .collect();
    let outcomes = crate::common::par_map(&runs, |&(c, g)| match g {
        None => (c.run)(),
        Some(g) => on_ground(g, || (c.run)()),
    });
    let mut broken = Vec::new();
    for (&(c, g), o) in runs.iter().zip(outcomes) {
        let is_limit = limit(&c.id);
        if o.ok == is_limit {
            broken.push(format!(
                "{}{} [{}] {}: followed={} retired={} wrong_lit={} dark={} {}",
                c.id,
                g.map_or_else(String::new, |g| format!(" on {}", g.name)),
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
        "{} of {} readings ({} cases on {} grounds):\n{}",
        broken.len(),
        runs.len(),
        picked.len(),
        grounds.len(),
        broken.join("\n")
    );
}

/// A family holds: every case whose id satisfies `pick` holds, on every
/// ground ([`run_law`]).
pub fn holds_where(cases: Vec<Case>, pick: impl Fn(&str) -> bool) {
    run_law(cases, pick, |_| false);
}

/// [`holds_where`] for the cases whose id starts with `prefix`.
pub fn holds(cases: Vec<Case>, prefix: &str) {
    holds_where(cases, |id| id.starts_with(prefix));
}

/// A family holds but for its stated limits: every picked case `limit` names
/// must read bad and every other must hold, on every ground. Neither side may
/// be empty, so a grid's spine ([`sweep`]) must keep a case on each.
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
