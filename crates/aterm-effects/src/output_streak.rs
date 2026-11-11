// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Prism wake output comets and ribbon
//! ([`OutputStreak::note_output`](crate::output_streak::OutputStreak::note_output),
//! [`OutputStreak::note_output_cells`](crate::output_streak::OutputStreak::note_output_cells),
//! [`OutputStreak::rebase`](crate::output_streak::OutputStreak::rebase),
//! [`ECHO_DISCOUNT_MS`](crate::output_streak::ECHO_DISCOUNT_MS),
//! [`UNWATCHED_MS`](crate::output_streak::UNWATCHED_MS),
//! [`TypingMomentum`](crate::typing_momentum::TypingMomentum),
//! [`SPAWN_MIN_GAP_MS`](crate::output_streak::SPAWN_MIN_GAP_MS),
//! [`MAX_COMETS`](crate::output_streak::MAX_COMETS),
//! [`SATURATED`](crate::output_streak::SATURATED),
//! [`ALPHA_DEFAULT`](crate::output_streak::ALPHA_DEFAULT),
//! [`ALPHA_CLAMP`](crate::output_streak::ALPHA_CLAMP),
//! [`StreakSound::Shimmer`](crate::output_streak::StreakSound::Shimmer),
//! [`EPISODE_LOW`](crate::output_streak::EPISODE_LOW),
//! [`StreakSound::Settle`](crate::output_streak::StreakSound::Settle),
//! [`RIBBON_LINGER_MS`](crate::output_streak::RIBBON_LINGER_MS),
//! [`StreakConfig::idle_secs`](crate::output_streak::StreakConfig::idle_secs),
//! [`GlowQuad`](aterm_render::GlowQuad), [`crate::nova::MAX_NOVA_QUADS`]).

use aterm_render::{GlowQuad, premul_rgb};
use aterm_time::Instant;
use std::time::Duration;

use crate::cursor_glow::Geom;
use crate::effect_util::{lerp_rgb, push_grid_quad};
use crate::genome;
use crate::spectrum::{spectrum, spectrum_stop, spectrum_stop_position};
use crate::trail_sound::OutputGesture;
use crate::typing_momentum::TypingMomentum;

/// A token change this close behind accepted editor input is its echo and mints nothing.
pub const ECHO_DISCOUNT_MS: u64 = 250;

/// The engine's one ignition floor.
pub const SPAWN_MIN_GAP_MS: u64 = 700;

/// The voice's own floor on top of the episode rule.
pub const SOUND_MIN_GAP_MS: u64 = 1_500;

/// Comet sweep duration band.
pub const COMET_MS_MIN: u32 = 420;
/// Upper end of the sweep band (see [`COMET_MS_MIN`]).
pub const COMET_MS_MAX: u32 = 900;

/// Output a frame first sees longer than this after the screen drew it mints nothing: a watched
/// comet minted at its arrival would already have finished its longest sweep
/// ([`COMET_MS_MAX`]), so the light would date a landing nobody watched to the frame that
/// shows it (an occluded window's first frame back, a headless capture).
pub const UNWATCHED_MS: u64 = COMET_MS_MAX as u64;

/// Tail length band in cells.
pub const TAIL_MIN: u16 = 4;
/// Upper end of the tail band (see [`TAIL_MIN`]).
pub const TAIL_MAX: u16 = 14;

/// Peak coverage on a dark ground at full intensity.
pub const ALPHA_DEFAULT: f32 = 0.10;

/// The hard amplitude ceiling.
pub const ALPHA_CLAMP: f32 = 0.18;

/// Momentum at or above which per-row spawning stops and the single ribbon takes over.
pub const SATURATED: f32 = 0.72;

/// Momentum below which an episode re-arms.
pub const EPISODE_LOW: f32 = 0.10;

/// How long the ribbon lingers after the last licensed output before retiring.
pub const RIBBON_LINGER_MS: u64 = 600;

/// Hue span across one comet, in spectrum turns.
pub const HUE_SPAN: f32 = 0.22;

/// Ribbon hue drift, turns per second.
pub const RIBBON_DRIFT: f32 = 0.25;

/// The most comets resident at once (`max_streaks` is clamped into `1..=MAX_COMETS`).
pub const MAX_COMETS: usize = 4;

const MAX_DT_S: f32 = 0.25;

/// What the host must play, recorded only on an edge that also minted light.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreakSound {
    /// The episode's one soft pip, on the quiet→active crossing.
    Shimmer,
    /// The episode's closing exhale, quieter still, on the drain to rest.
    Settle,
}

impl StreakSound {
    /// The synth gesture for this edge, defined here so every host assigns the same voice.
    #[inline]
    #[must_use]
    pub const fn output_gesture(self) -> OutputGesture {
        match self {
            Self::Shimmer => OutputGesture::Shimmer,
            Self::Settle => OutputGesture::Settle,
        }
    }
}

/// One sound cue: what to play and where it happened.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StreakCue {
    /// Which voice (see [`StreakSound`]).
    pub sound: StreakSound,
    /// Equal-power pan position `-1..=1`, from the span centre column.
    pub pan: f32,
    /// The comet's spectrum entry phase `0..1`.
    pub hue: f32,
}

/// Resolved per-frame inputs.
#[derive(Clone, Copy, Debug)]
pub struct StreakConfig {
    /// Master on/off (on by default at the config layer).
    pub enabled: bool,
    /// Amplitude `0..=1`, the reduced-motion / load-shed scale.
    pub intensity: f32,
    /// Tail length in cells, clamped here into [`TAIL_MIN`]`..=`[`TAIL_MAX`].
    pub tail: u16,
    /// Resident comet cap, clamped here into `1..=`[`MAX_COMETS`].
    pub max_streaks: u8,
    /// Mandatory idle drain: with no licensed output for this long, everything retires.
    pub idle_secs: f32,
    /// Whether the theme background is dark.
    pub dark_theme: bool,
    /// The resolved theme background `0x00RRGGBB`.
    pub theme_bg: u32,
    /// The resolved theme foreground `0x00RRGGBB`.
    pub theme_fg: u32,
    /// The theme's cursor colour (the host falls back to the foreground).
    pub theme_cursor: u32,
    /// Whether a cue may be recorded at all (the host's own sound gates still apply).
    pub sound: bool,
}

impl StreakConfig {
    /// Default inert configuration (`enabled: false`, `tail: 9`, `max_streaks: 3`).
    pub const DEFAULT: Self = Self {
        enabled: false,
        intensity: 1.0,
        tail: 9,
        max_streaks: 3,
        idle_secs: 10.0,
        dark_theme: true,
        theme_bg: 0x0000_0000,
        theme_fg: 0x00FF_FFFF,
        theme_cursor: 0x00FF_FFFF,
        sound: false,
    };
}

impl Default for StreakConfig {
    #[inline]
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// What a tick produced.
#[derive(Clone, Copy, Debug)]
pub struct StreakFrame {
    /// Fingerprint of everything emitted this frame.
    pub fp: u64,
    /// The one cue to play, if this frame crossed a sounding edge.
    pub cue: Option<StreakCue>,
}

impl StreakFrame {
    /// Dormant frame output with fingerprint `0` and no sound cue.
    pub const DORMANT: Self = Self { fp: 0, cue: None };
}

impl Default for StreakFrame {
    #[inline]
    fn default() -> Self {
        Self::DORMANT
    }
}

/// One resident comet.
#[derive(Clone, Copy, Debug)]
struct Comet {
    row: u16,
    from_col: i32,
    to_col: i32,
    /// Sweep progress `0..=1`.
    progress: f32,
    /// Reciprocal of sweep duration (`1.0 / dur_s`).
    inv_dur_s: f32,
    tail: u16,
    /// Spectrum entry phase `0..1`.
    hue: f32,
}

/// The resident flood ribbon (governor 4).
#[derive(Clone, Copy, Debug)]
struct Ribbon {
    row: u16,
    from_col: i32,
    to_col: i32,
    /// Drifting spectrum phase `0..1`.
    hue: f32,
    /// Seconds since the last licensed output refreshed it.
    since_output_s: f32,
}

/// A licensed output observation waiting for the next tick to spend it.
#[derive(Clone, Copy, Debug)]
struct Pending {
    row: u16,
    left: u16,
    right: u16,
}

/// The glyph identity a `RenderCell` carries.
#[derive(Clone, Copy, PartialEq, Eq)]
struct ObservedGlyph {
    ch: char,
    presentation: u8,
}

impl ObservedGlyph {
    const BLANK: Self = Self {
        ch: ' ',
        presentation: 0,
    };

    #[inline]
    const fn of(cell: &aterm_core::terminal::RenderCell) -> Self {
        Self {
            ch: cell.ch,
            presentation: (cell.wide as u8)
                | ((cell.emoji_presentation as u8) << 1)
                | ((cell.text_presentation as u8) << 2),
        }
    }

    #[inline]
    const fn has_ink(self) -> bool {
        self.ch != ' ' && self.ch != '\0'
    }

    #[inline]
    const fn eq_glyph(self, other: Self) -> bool {
        self.ch == other.ch && self.presentation == other.presentation
    }

    #[inline]
    const fn same_row(old: &[Self], new: &[aterm_core::terminal::RenderCell]) -> bool {
        let common = if old.len() < new.len() {
            old.len()
        } else {
            new.len()
        };
        let mut col = 0;
        while col < common {
            if !old[col].eq_glyph(Self::of(&new[col])) {
                return false;
            }
            col += 1;
        }
        let mut col = common;
        while col < old.len() {
            if !old[col].eq_glyph(Self::BLANK) {
                return false;
            }
            col += 1;
        }
        let mut col = common;
        while col < new.len() {
            if !Self::of(&new[col]).eq_glyph(Self::BLANK) {
                return false;
            }
            col += 1;
        }
        true
    }
}

/// The prism wake state machine, one per pane.
pub struct OutputStreak {
    /// The output metric: its own [`TypingMomentum`], fed only by licensed output.
    momentum: TypingMomentum,
    /// Last arrival token.
    last_token: Option<u64>,
    /// Prior visible glyphs for the cell API (a changed token says some row changed, not which).
    observed_rows: Vec<Vec<ObservedGlyph>>,
    /// The host's viewport dimensions, independent of sparse row lengths.
    observed_viewport: Option<(usize, usize)>,
    /// Last accepted editor-input stamp, for the echo discount.
    last_key: Option<Instant>,
    /// When the screen last changed, as the host's terminal clocked it ([`Self::note_drawn_at`]).
    drawn_at: Option<Instant>,
    /// Newest licensed span awaiting a spawn decision.
    pending: Option<Pending>,
    /// Stamp of the last licensed output (drives the ribbon and the idle drain).
    last_output: Option<Instant>,
    /// Last spawn stamp.
    last_spawn: Option<Instant>,
    /// Last cue stamp — the [`SOUND_MIN_GAP_MS`] floor.
    last_sound: Option<Instant>,
    /// Whether the voice has already spoken this episode.
    in_episode: bool,
    /// Resident comets, oldest slots first.
    comets: [Option<Comet>; MAX_COMETS],
    /// The flood ribbon, when saturated.
    ribbon: Option<Ribbon>,
    /// Frame clock for the sweep integrator.
    last_tick: Option<Instant>,
    /// Variation seed, mixed into every comet's genome key.
    seed: u64,
    /// Latched: something was drawable at the last tick, so [`Self::is_active`] needs no clock.
    drawing: bool,
}

impl Default for OutputStreak {
    #[inline]
    fn default() -> Self {
        Self::new(0)
    }
}

impl OutputStreak {
    /// A fresh engine with variation `seed`.
    #[inline]
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self {
            momentum: TypingMomentum::new(),
            last_token: None,
            observed_rows: Vec::new(),
            observed_viewport: None,
            last_key: None,
            drawn_at: None,
            pending: None,
            last_output: None,
            last_spawn: None,
            last_sound: None,
            in_episode: false,
            comets: [None; MAX_COMETS],
            ribbon: None,
            last_tick: None,
            seed,
            drawing: false,
        }
    }

    /// Whether the host must keep arming frames.
    #[inline]
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.drawing
    }

    /// The next instant at which something visibly changes, or `None` at rest.
    #[inline]
    #[must_use]
    pub fn next_change_deadline(&self, now: Instant, idle_secs: f32) -> Option<Instant> {
        if !self.drawing {
            let deadline = match (
                self.pending_idle_deadline(idle_secs),
                self.pending_settle_deadline(),
            ) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            };
            return deadline.map(|d| d.max(now));
        }
        Some(now)
    }

    /// The mandatory idle cutoff for the current episode.
    #[inline]
    fn pending_idle_deadline(&self, idle_secs: f32) -> Option<Instant> {
        if !self.in_episode {
            return None;
        }
        let last = self.last_output?;
        let idle = idle_duration(idle_secs).unwrap_or(Duration::ZERO);
        Some(last.checked_add(idle).unwrap_or(last))
    }

    /// The instant the current episode drains, if one is open and owed a [`StreakSound::Settle`].
    #[inline]
    fn pending_settle_deadline(&self) -> Option<Instant> {
        if !self.in_episode {
            return None;
        }
        self.momentum.low_crossing(EPISODE_LOW)
    }

    /// Back to rest: everything retired, the voice closed, the token un-baselined.
    #[inline]
    pub fn reset(&mut self) {
        let seed = self.seed;
        let mut observed_rows = std::mem::take(&mut self.observed_rows);
        let mut i = 0;
        while i < observed_rows.len() {
            observed_rows[i].clear();
            i += 1;
        }
        *self = Self {
            observed_rows,
            ..Self::new(seed)
        };
    }

    /// Forget the arrival token so the next observation baselines.
    #[inline]
    pub fn rebase(&mut self) {
        self.last_token = None;
        let mut i = 0;
        while i < self.observed_rows.len() {
            self.observed_rows[i].clear();
            i += 1;
        }
        self.observed_viewport = None;
    }

    /// One accepted editor input at `now` (a key, mapped sequence, IME text or paste receipt).
    #[inline]
    pub const fn note_keystroke(&mut self, now: Instant) {
        self.last_key = Some(now);
    }

    /// When the screen the next observation reads last changed (the host terminal's
    /// `drawn_at`), or `None` when the host cannot say: output older than [`UNWATCHED_MS`] at
    /// that observation mints nothing. A host stamps it only on the clock its frames' `now`
    /// reads (the native window's; the page pipeline's injected clock is not the parse's, so it
    /// stamps nothing).
    #[inline]
    pub const fn note_drawn_at(&mut self, drawn_at: Option<Instant>) {
        self.drawn_at = drawn_at;
    }

    /// A submitted turn boundary at `now`.
    #[inline]
    pub fn note_turn_boundary(&mut self, now: Instant) {
        if !matches!(self.last_key, Some(accepted) if accepted > now) {
            self.last_key = None;
        }
    }

    /// One frame's output observation.
    #[inline]
    pub fn note_output(
        &mut self,
        token: u64,
        spans: &[(u16, u16, u16)],
        now: Instant,
        input_hot: bool,
    ) -> bool {
        let Some(prev) = self.last_token else {
            self.last_token = Some(token);
            return false;
        };
        self.last_token = Some(token);
        if token == prev {
            return false;
        }
        let Some(&(row, left, right)) = spans.last() else {
            return false;
        };
        if input_hot || self.within_echo_window(now) || self.unwatched(now) {
            return false;
        }
        self.momentum.advance(now);
        self.last_output = Some(now);
        self.pending = Some(Pending { row, left, right });
        true
    }

    /// [`Self::note_output`] with the anchor derived from a frame's cells.
    #[inline]
    pub fn note_output_cells(
        &mut self,
        cells: &[Vec<aterm_core::terminal::RenderCell>],
        cursor: (usize, usize),
        viewport: (usize, usize),
        token: u64,
        now: Instant,
        input_hot: bool,
    ) -> bool {
        // Geometry can change before the terminal publishes a new token, so the walk is skipped
        // only when both are unchanged.
        if self.last_token == Some(token) && self.observed_viewport == Some(viewport) {
            return self.note_output(token, &[], now, input_hot);
        }
        let (rows, _) = viewport;
        let visible = &cells[..rows.min(cells.len())];
        let baseline = self.last_token.is_none()
            || self.observed_viewport != Some(viewport)
            || self.observed_rows.len() != visible.len();
        let (cursor_row, cursor_col) = cursor;
        let anchor_row = if cursor_col > 0 {
            cursor_row
        } else {
            cursor_row.saturating_sub(1)
        };
        let span = if !baseline
            && !input_hot
            && !self.within_echo_window(now)
            && !self.unwatched(now)
            && let Some(row) = visible.get(anchor_row)
        {
            let old_anchor = &self.observed_rows[anchor_row];
            let mut extent: Option<(usize, usize)> = None;
            let mut col = 0;
            while col < row.len() {
                let cell = &row[col];
                let old = if col < old_anchor.len() {
                    old_anchor[col]
                } else {
                    ObservedGlyph::BLANK
                };
                let new = ObservedGlyph::of(cell);
                let continuation = cell.wide && matches!(extent, Some((_, end)) if end + 1 == col);
                if (new.has_ink() && old != new) || continuation {
                    extent = Some(match extent {
                        Some((left, end)) if end + 1 == col => (left, col),
                        _ => (col, col),
                    });
                }
                col += 1;
            }
            if let Some((start, end)) = extent {
                let mut moved_row = false;
                let mut index = 0;
                while index < self.observed_rows.len() {
                    let old = &self.observed_rows[index];
                    if index != anchor_row
                        && ObservedGlyph::same_row(old, row)
                        && !ObservedGlyph::same_row(old, &visible[index])
                    {
                        moved_row = true;
                        break;
                    }
                    index += 1;
                }
                if moved_row {
                    None
                } else {
                    Some([(
                        anchor_row as u16,
                        start.min(u16::MAX as usize) as u16,
                        end.min(u16::MAX as usize) as u16,
                    )])
                }
            } else {
                None
            }
        } else {
            None
        };
        self.observed_rows.resize_with(visible.len(), Vec::new);
        self.observed_viewport = Some(viewport);
        let mut r = 0;
        while r < visible.len() {
            let old = &mut self.observed_rows[r];
            let row = &visible[r];
            old.clear();
            old.extend(row.iter().map(ObservedGlyph::of));
            r += 1;
        }
        if baseline {
            self.comets = Default::default();
            self.ribbon = None;
            self.pending = None;
            self.drawing = false;
        } else {
            self.retire_light_over_erased_ink();
        }
        self.note_output(
            token,
            match &span {
                Some(s) => s.as_slice(),
                None => &[],
            },
            now,
            input_hot,
        )
    }

    #[inline]
    const fn no_comets(&self) -> bool {
        let mut i = 0;
        while i < MAX_COMETS {
            if self.comets[i].is_some() {
                return false;
            }
            i += 1;
        }
        true
    }

    #[inline]
    const fn has_comet_on_row(&self, row: u16) -> bool {
        let mut i = 0;
        while i < MAX_COMETS {
            if let Some(c) = self.comets[i]
                && c.row == row
            {
                return true;
            }
            i += 1;
        }
        false
    }

    #[inline]
    const fn row_span_inked(observed: &[Vec<ObservedGlyph>], row: u16, from: i32, to: i32) -> bool {
        if to < 0 || from > to {
            return false;
        }
        let r = row as usize;
        if r >= observed.len() {
            return false;
        }
        let cells = observed[r].as_slice();
        let start = if from > 0 { from as usize } else { 0 };
        let end_cand = (to as usize).saturating_add(1);
        let end = if end_cand < cells.len() {
            end_cand
        } else {
            cells.len()
        };
        if start >= end {
            return false;
        }
        let mut i = start;
        while i < end {
            if cells[i].has_ink() {
                return true;
            }
            i += 1;
        }
        false
    }

    #[inline]
    const fn retire_light_over_erased_ink(&mut self) {
        if self.no_comets() && self.ribbon.is_none() {
            return;
        }
        let observed = self.observed_rows.as_slice();
        let mut i = 0;
        while i < MAX_COMETS {
            if let Some(c) = self.comets[i]
                && !Self::row_span_inked(observed, c.row, c.from_col, c.to_col)
            {
                self.comets[i] = None;
            }
            i += 1;
        }
        if let Some(r) = self.ribbon
            && !Self::row_span_inked(observed, r.row, r.from_col, r.to_col)
        {
            self.ribbon = None;
        }
    }

    /// Whether `now` sits inside the echo shadow of the last accepted input.
    #[inline]
    fn within_echo_window(&self, now: Instant) -> bool {
        matches!(
            self.last_key,
            Some(k) if now.saturating_duration_since(k) <= Duration::from_millis(ECHO_DISCOUNT_MS)
        )
    }

    /// Whether the screen this observation reads was drawn longer than [`UNWATCHED_MS`] before
    /// `now`: a landing no frame watched.
    #[inline]
    fn unwatched(&self, now: Instant) -> bool {
        matches!(
            self.drawn_at,
            Some(at) if now.saturating_duration_since(at) > Duration::from_millis(UNWATCHED_MS)
        )
    }

    /// Advance one frame at `now` and append this frame's light to `out`.
    #[inline]
    pub fn tick(
        &mut self,
        now: Instant,
        geom: Geom,
        cfg: &StreakConfig,
        out: &mut Vec<GlowQuad>,
    ) -> StreakFrame {
        // Fully inert: off, zero amplitude, or degenerate geometry. A reset, not a dimming.
        if !cfg.enabled
            || cfg.intensity <= 0.0
            || geom.cw == 0
            || geom.ch == 0
            || geom.rows == 0
            || geom.cols == 0
        {
            self.reset();
            return StreakFrame::default();
        }

        let dt = match self.last_tick {
            Some(t) => now
                .saturating_duration_since(t)
                .as_secs_f32()
                .clamp(0.0, MAX_DT_S),
            None => 0.0,
        };
        self.last_tick = Some(now);

        if self.idle_drained(now, cfg) {
            let cue = self.close_episode_cue(now, cfg);
            self.comets = Default::default();
            self.ribbon = None;
            self.pending = None;
            self.last_output = None;
            self.drawing = false;
            return StreakFrame { fp: 0, cue };
        }

        if self.pending.is_none() && self.no_comets() && self.ribbon.is_none() && !self.in_episode {
            self.drawing = false;
            return StreakFrame::default();
        }

        let m = self.momentum.value(now);
        let cue = match self.spend_pending(now, m, cfg, geom) {
            Some(c) => Some(c),
            None => self.close_episode_cue(now, cfg),
        };

        self.advance_comets(dt);
        self.advance_ribbon(dt, now, m);

        let fp = self.emit(geom, cfg, out);
        self.drawing = fp != 0;
        StreakFrame { fp, cue }
    }

    /// Whether the idle drain has expired.
    #[inline]
    fn idle_drained(&self, now: Instant, cfg: &StreakConfig) -> bool {
        let Some(last) = self.last_output else {
            return self.no_comets() && self.ribbon.is_none();
        };
        match idle_duration(cfg.idle_secs) {
            Some(idle) => now.saturating_duration_since(last) >= idle,
            None => true,
        }
    }

    #[inline]
    fn spend_pending(
        &mut self,
        now: Instant,
        m: f32,
        cfg: &StreakConfig,
        geom: Geom,
    ) -> Option<StreakCue> {
        let pending = self.pending.take()?;
        if (pending.row as usize) >= geom.rows {
            return None;
        }

        if m >= SATURATED {
            self.refresh_ribbon(now, pending, cfg, geom);
            return None;
        }

        if let Some(last) = self.last_spawn
            && now.saturating_duration_since(last) < Duration::from_millis(SPAWN_MIN_GAP_MS)
        {
            return None;
        }
        if self.has_comet_on_row(pending.row) {
            return None;
        }
        let slot = self.free_slot(cfg)?;

        let hue = self.hue_for(pending, cfg);
        let key = genome::mix(
            self.seed ^ genome::mix(u64::from(pending.row) << 32 | u64::from(pending.left)),
        );
        let dur_ms =
            COMET_MS_MIN + (genome::field(key, 8, 8) as u32 * (COMET_MS_MAX - COMET_MS_MIN)) / 255;
        let tail_span = u32::from(TAIL_MAX - TAIL_MIN);
        let tail = TAIL_MIN + ((genome::field(key, 16, 8) as u32 * tail_span) / 255) as u16;

        let (from_col, to_col) = clipped_span(pending, geom.cols)?;
        self.comets[slot] = Some(Comet {
            row: pending.row,
            from_col,
            to_col,
            progress: 0.0,
            inv_dur_s: 1000.0 / (dur_ms.max(1) as f32),
            tail: tail.min(cfg.tail.clamp(TAIL_MIN, TAIL_MAX)),
            hue,
        });
        self.last_spawn = Some(now);

        self.open_episode_cue(now, cfg, pending, geom, hue)
    }

    /// The first free comet slot under the host's resident cap.
    #[inline]
    const fn free_slot(&self, cfg: &StreakConfig) -> Option<usize> {
        let cap = if (cfg.max_streaks as usize) < 1 {
            1
        } else if (cfg.max_streaks as usize) > MAX_COMETS {
            MAX_COMETS
        } else {
            cfg.max_streaks as usize
        };
        let mut i = 0;
        while i < cap {
            if self.comets[i].is_none() {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    /// Governor 6, opening half: one pip on the quiet→active crossing, then mute.
    #[inline]
    fn open_episode_cue(
        &mut self,
        now: Instant,
        cfg: &StreakConfig,
        pending: Pending,
        geom: Geom,
        hue: f32,
    ) -> Option<StreakCue> {
        if self.in_episode {
            return None;
        }
        self.in_episode = true;
        if !cfg.sound {
            return None;
        }
        if let Some(last) = self.last_sound
            && now.saturating_duration_since(last) < Duration::from_millis(SOUND_MIN_GAP_MS)
        {
            return None;
        }
        self.last_sound = Some(now);
        Some(StreakCue {
            sound: StreakSound::Shimmer,
            pan: pan_of(pending, geom),
            hue,
        })
    }

    /// Governor 6, closing half: once momentum drains through [`EPISODE_LOW`] the voice re-arms
    /// and one exhale is heard.
    #[inline]
    fn close_episode_cue(&mut self, now: Instant, cfg: &StreakConfig) -> Option<StreakCue> {
        if !self.in_episode || self.momentum.value(now) > EPISODE_LOW {
            return None;
        }
        self.in_episode = false;
        if !cfg.sound {
            return None;
        }
        self.last_sound = Some(now);
        Some(StreakCue {
            sound: StreakSound::Settle,
            pan: 0.0,
            hue: 0.0,
        })
    }

    /// Per-theme character: the theme's cursor colour, snapped to its nearest named spectrum stop,
    /// decides where the comet's head enters the arc; the genome jitters within that stop.
    #[inline]
    const fn hue_for(&self, pending: Pending, cfg: &StreakConfig) -> f32 {
        const INV_255_X2: f32 = 2.0 / 255.0;
        let named = nearest_spectrum_stop_by_hue(cfg.theme_cursor);
        let anchor = spectrum_stop_position(named);
        let key = genome::mix(self.seed ^ (pending.row as u64));
        let jitter = (genome::field(key, 0, 8) as f32 * INV_255_X2 - 1.0)
            * spectrum_stop_jitter_radius(named);
        (anchor + jitter).clamp(0.0, 1.0)
    }

    #[inline]
    fn refresh_ribbon(&mut self, now: Instant, pending: Pending, cfg: &StreakConfig, geom: Geom) {
        let Some((from_col, to_col)) = clipped_span(pending, geom.cols) else {
            return;
        };
        let want = (pending.row, from_col, to_col);
        if let Some(r) = self.ribbon.as_mut() {
            r.since_output_s = 0.0;
            if (r.row, r.from_col, r.to_col) == want {
                return;
            }
        }
        if let Some(last) = self.last_spawn
            && now.saturating_duration_since(last) < Duration::from_millis(SPAWN_MIN_GAP_MS)
        {
            return;
        }
        let hue = match self.ribbon {
            Some(r) => r.hue,
            None => self.hue_for(pending, cfg),
        };
        self.ribbon = Some(Ribbon {
            row: want.0,
            from_col: want.1,
            to_col: want.2,
            hue,
            since_output_s: 0.0,
        });
        self.last_spawn = Some(now);
    }

    /// Advance every resident comet's sweep and retire the finished ones.
    #[inline]
    const fn advance_comets(&mut self, dt: f32) {
        let mut i = 0;
        while i < MAX_COMETS {
            if let Some(c) = &mut self.comets[i] {
                c.progress += dt * c.inv_dur_s;
                if c.progress >= 1.0 {
                    self.comets[i] = None;
                }
            }
            i += 1;
        }
    }

    /// Drift the ribbon's hue and retire it once the flood stops.
    #[inline]
    fn advance_ribbon(&mut self, dt: f32, now: Instant, m: f32) {
        let Some(r) = self.ribbon.as_mut() else {
            return;
        };
        let next_hue = r.hue + dt * RIBBON_DRIFT;
        r.hue = next_hue - next_hue.floor();
        r.since_output_s += dt;
        let stale = match self.last_output {
            Some(t) => now.saturating_duration_since(t) > Duration::from_millis(RIBBON_LINGER_MS),
            None => false,
        };
        if stale || m < EPISODE_LOW {
            self.ribbon = None;
        }
    }

    #[inline]
    fn emit(&self, geom: Geom, cfg: &StreakConfig, out: &mut Vec<GlowQuad>) -> u64 {
        if self.no_comets() && self.ribbon.is_none() {
            return 0;
        }
        let peak = (ALPHA_DEFAULT * cfg.intensity.clamp(0.0, 1.0)).min(ALPHA_CLAMP);
        let amp = if cfg.dark_theme { peak } else { peak * 0.5 };
        if amp <= 0.0 {
            return 0;
        }
        let before = out.len();
        let mut fp: u64 = 0;
        let ch_i = geom.ch as i32;
        let max_col = (geom.cols as i32).saturating_sub(1);
        let amp_255 = amp * 255.0;

        let mut ci = 0;
        while ci < MAX_COMETS {
            let Some(c) = self.comets[ci] else {
                ci += 1;
                continue;
            };
            ci += 1;
            let life = 1.0 - c.progress;
            if (c.row as usize) < geom.rows && life > 0.0 {
                let y = i32::from(c.row) * ch_i;
                let head = c.from_col as f32 + (c.to_col - c.from_col) as f32 * c.progress;
                let col_end = (head.round() as i32).min(max_col);
                let tail_f = f32::from(c.tail);
                let tail_start = (head - tail_f).max(c.from_col as f32);
                let mut col = (tail_start.floor() as i32).max(0);
                if col <= col_end {
                    let inv_tail_denom = 1.0 / tail_f.max(1.0);
                    while col <= col_end {
                        let u = ((head - col as f32) * inv_tail_denom).clamp(0.0, 1.0);
                        let shape = (1.0 - u) * (1.0 - u) * life;
                        let a = (amp_255 * shape.clamp(0.0, 1.0)).round() as u8;
                        if a > 0 {
                            Self::push_cell_alpha(
                                out,
                                geom,
                                cfg,
                                (y, col),
                                c.hue + u * HUE_SPAN,
                                a,
                            );
                        }
                        col += 1;
                    }
                }
            }
            fp = fp
                .wrapping_mul(1_000_003)
                .wrapping_add((c.progress * 4096.0) as u64)
                .wrapping_add(u64::from(c.row) << 20);
        }

        if let Some(r) = self.ribbon {
            const INV_RIBBON_LINGER_S: f32 = 1000.0 / (RIBBON_LINGER_MS as f32);
            let fade = 1.0 - (r.since_output_s * INV_RIBBON_LINGER_S).clamp(0.0, 1.0) * 0.5;
            let shape = 0.55 * fade;
            let a = (amp_255 * shape.clamp(0.0, 1.0)).round() as u8;
            if (r.row as usize) < geom.rows && shape > 0.0 && a > 0 {
                let hue_step = HUE_SPAN / geom.cols.max(1) as f32;
                let y = i32::from(r.row) * ch_i;
                let mut col = r.from_col.max(0);
                let col_end = r.to_col.min(max_col);
                let mut t = r.hue + col as f32 * hue_step;
                while col <= col_end {
                    Self::push_cell_alpha(out, geom, cfg, (y, col), t, a);
                    col += 1;
                    t += hue_step;
                }
            }
            fp = fp
                .wrapping_mul(31)
                .wrapping_add((r.hue * 4096.0) as u64)
                .wrapping_add(u64::from(r.row) << 8);
        }

        if out.len() == before {
            return 0;
        }
        fp | 1
    }

    /// One cell of streak light.
    #[inline]
    fn push_cell_alpha(
        out: &mut Vec<GlowQuad>,
        geom: Geom,
        cfg: &StreakConfig,
        (y, col): (i32, i32),
        hue_t: f32,
        a: u8,
    ) {
        let (w, h) = (geom.cw as i32, geom.ch as i32);
        let base = spectrum(hue_t - hue_t.floor());
        let x = col * w;

        if cfg.dark_theme {
            push_grid_quad(out, geom, x, y, w, h, premul_rgb(base, a), 0);
        } else {
            let shade = lerp_rgb(lerp_rgb(base, 0x0000_0000, 0.55), cfg.theme_fg, 0.25);
            push_grid_quad(out, geom, x, y, w, h, premul_rgb(shade, a), a);
        }
    }
}

/// Clip one licensed span to the visible columns without relocating a wholly off-screen or
/// malformed span onto the viewport edge.
#[inline]
const fn clipped_span(pending: Pending, cols: usize) -> Option<(i32, i32)> {
    if pending.right < pending.left || (pending.left as usize) >= cols {
        return None;
    }
    let Some(last_col_usize) = cols.checked_sub(1) else {
        return None;
    };
    let last_col = if last_col_usize > i32::MAX as usize {
        i32::MAX
    } else {
        last_col_usize as i32
    };
    let right = pending.right as i32;
    Some((
        pending.left as i32,
        if right < last_col { right } else { last_col },
    ))
}

/// The mandatory idle duration.
#[inline]
fn idle_duration(idle_secs: f32) -> Option<Duration> {
    if idle_secs.is_finite() && idle_secs > 0.0 {
        Duration::try_from_secs_f32(idle_secs).ok()
    } else {
        None
    }
}

/// Equal-power pan position `-1..=1` from a span's centre column.
#[inline]
const fn pan_of(pending: Pending, geom: Geom) -> f32 {
    let cols = if geom.cols < 1 { 1 } else { geom.cols } as f32;
    let centre = (pending.left as f32 + pending.right as f32) * 0.5;
    ((centre / cols) * 2.0 - 1.0).clamp(-1.0, 1.0)
}

/// Nearest named spectrum colour by circular HSV hue.
#[inline]
const fn nearest_spectrum_stop_by_hue(rgb: u32) -> usize {
    const STOP_HUES: [f32; crate::spectrum::SPECTRUM_STOPS] = {
        let mut hues = [0.0; crate::spectrum::SPECTRUM_STOPS];
        let mut i = 0;
        while i < crate::spectrum::SPECTRUM_STOPS {
            hues[i] = hue_position_of(spectrum_stop(i));
            i += 1;
        }
        hues
    };
    let hue = hue_position_of(rgb);
    let mut nearest = 0;
    let mut nearest_distance = f32::INFINITY;
    let mut index = 0;
    while index < STOP_HUES.len() {
        let stop_hue = STOP_HUES[index];
        let direct = (hue - stop_hue).abs();
        let distance = direct.min(1.0 - direct);
        if distance < nearest_distance {
            nearest = index;
            nearest_distance = distance;
        }
        index += 1;
    }
    nearest
}

/// Half the narrower adjacent arc gap of one named stop.
#[inline]
const fn spectrum_stop_jitter_radius(index: usize) -> f32 {
    const STOP_JITTER_RADII: [f32; crate::spectrum::SPECTRUM_STOPS] = {
        let mut radii = [0.0; crate::spectrum::SPECTRUM_STOPS];
        let last = crate::spectrum::SPECTRUM_STOPS.saturating_sub(1);
        if last > 0 {
            let mut i = 0;
            while i <= last {
                let at = spectrum_stop_position(i);
                let gap = if i == 0 {
                    spectrum_stop_position(1) - at
                } else if i == last {
                    at - spectrum_stop_position(last - 1)
                } else {
                    (at - spectrum_stop_position(i - 1)).min(spectrum_stop_position(i + 1) - at)
                };
                radii[i] = gap.max(0.0) * 0.5;
                i += 1;
            }
        }
        radii
    };
    let last = STOP_JITTER_RADII.len().saturating_sub(1);
    STOP_JITTER_RADII[if index > last { last } else { index }]
}

/// A colour's circular HSV hue turn (`0..1`).
#[inline]
const fn hue_position_of(rgb: u32) -> f32 {
    let r_b = (rgb >> 16) & 0xff;
    let g_b = (rgb >> 8) & 0xff;
    let b_b = rgb & 0xff;
    if r_b == g_b && g_b == b_b {
        return 0.0;
    }
    let r = r_b as f32 * (1.0 / 255.0);
    let g = g_b as f32 * (1.0 / 255.0);
    let b = b_b as f32 * (1.0 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    if d <= f32::EPSILON {
        return 0.0;
    }
    let inv_d = 1.0 / d;
    let h = if max == r {
        let raw = (g - b) * inv_d;
        if raw < 0.0 { raw + 6.0 } else { raw }
    } else if max == g {
        (b - r) * inv_d + 2.0
    } else {
        (r - g) * inv_d + 4.0
    };
    let turn = h * (1.0 / 6.0);
    if turn >= 1.0 { turn - 1.0 } else { turn }
}

#[cfg(test)]
#[path = "output_streak_tests.rs"]
mod tests;
