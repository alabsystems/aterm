// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Prism wake: the terminal answers program output with a soft comet in the theme's own
//! colours, plus a soft sound. Design: `docs/DESIGN-output-streak-2026-08-30.md`.
//!
//! Output earns light the way typing earns momentum: metered, echo-discounted, and always
//! decaying to zero. A burst gets one comet and one soft note; a flood gets one silent ribbon.
//! Every governor is an engine-side clamp, not host policy, so no embedder, config or theme can
//! make this loud, fast or flashy.
//!
//! ## What counts as output
//!
//! The host feeds [`OutputStreak::note_output`] a content token and the fresh ink-bearing row
//! spans, or [`OutputStreak::note_output_cells`] a frame's cells, from which fresh damage is
//! derived (sparse changed runs never join across unchanged ink or blank gaps). Nothing mints:
//!
//! * on the first observation, or the first after [`OutputStreak::rebase`], so attaching to a
//!   busy session never fires comets for history;
//! * within [`ECHO_DISCOUNT_MS`] of accepted editor input or while the host reports
//!   `input_hot`; a submitted turn boundary clears the echo shadow so a fast command response
//!   still counts;
//! * for empty damage, so an idle TUI redrawing blank chrome never shimmers.
//!
//! Output momentum is its own [`TypingMomentum`] instance and never feeds the typing metric.
//!
//! ## Governors
//!
//! 1. [`TypingMomentum`]: rate-normalized accrual, so a firehose accumulates no more than a
//!    trickle, and `low_crossing` gives episode edges exactly at any frame rate.
//! 2. [`SPAWN_MIN_GAP_MS`]: one ignition clock per engine, spent by every edge that puts light
//!    on a cell or takes it off (a comet spawn or a ribbon re-home). Cues ride spawn edges, so
//!    it bounds eye and ear together.
//! 3. [`MAX_COMETS`] and per-row dedup: a row carrying a comet cannot respawn until it retires.
//! 4. Flood coalescing: at [`SATURATED`] momentum per-row spawning stops and one constant-cost
//!    ribbon remains.
//! 5. Amplitude: [`ALPHA_DEFAULT`] under the [`ALPHA_CLAMP`] ceiling, halved on light grounds.
//! 6. Episodes: one [`StreakSound::Shimmer`] on the quiet→active crossing, then mute until
//!    momentum drains through [`EPISODE_LOW`], which re-arms with one [`StreakSound::Settle`].
//!
//! Decay is guaranteed by the momentum τ, [`RIBBON_LINGER_MS`], and the mandatory idle drain
//! ([`StreakConfig::idle_secs`]).
//!
//! ## Contract
//!
//! Clockless (every entry point takes `now`), overlay-only (premultiplied [`GlowQuad`]s for the
//! grid-anchored `nova_add` channel), and empty-is-off: disabled, reduced-motion or settled
//! ticks reset the machine, emit nothing, record no cue and return fingerprint `0`.
//!
//! The host appends these quads to `nova_add` after the word-decoration pass, outside its
//! [`crate::nova::MAX_NOVA_QUADS`] budget. One engine emits at most one ribbon row of ≤ `cols`
//! cells plus `MAX_COMETS` heads of ≤ `TAIL_MAX + 2` cells, i.e. `cols + 64`
//! (`tests/nova_channel_budget.rs`).

use aterm_render::{GlowQuad, premul_rgb};
use aterm_time::Instant;
use std::time::Duration;

use crate::cursor_glow::Geom;
use crate::effect_util::{lerp_rgb, push_grid_quad};
use crate::genome;
use crate::spectrum::{spectrum, spectrum_stop, spectrum_stop_position};
use crate::trail_sound::OutputGesture;
use crate::typing_momentum::TypingMomentum;

/// A token change this close behind accepted editor input is its echo and mints nothing. The
/// same as the rain weather machine's discount, so the two agree on what "your own typing" is.
pub const ECHO_DISCOUNT_MS: u64 = 250;

/// The engine's one ignition floor: 700 ms between any two edges that put light
/// on a cell or take it off (a comet spawn, or a flood-ribbon re-home, which
/// shares `last_spawn`), so ≤1.43 ignitions/second per engine; cues ride spawn
/// edges, so this also limits the sound.
///
/// This engine's own floor, not a draw on another budget: nothing here reserves
/// against the word-nova's `FlashLimiterWindow`, and the host does not reserve
/// on its behalf. Per pane is safe because WCAG 2.3.1's rate arm is measured per
/// cell and a cell belongs to one pane (see [`OutputStreak`]).
pub const SPAWN_MIN_GAP_MS: u64 = 700;

/// The voice's own floor on top of the episode rule: two cues never fall closer than this. A
/// denied fire is not heard and does not re-arm.
pub const SOUND_MIN_GAP_MS: u64 = 1_500;

/// Comet sweep duration band. The envelope is monotone (`shape = (1 − u)²` times a decaying
/// `life`), so one comet over a cell is one rise and one fall, and the per-cell flash rate
/// depends on [`SPAWN_MIN_GAP_MS`] alone.
pub const COMET_MS_MIN: u32 = 420;
/// Upper end of the sweep band (see [`COMET_MS_MIN`]).
pub const COMET_MS_MAX: u32 = 900;

/// Tail length band in cells: the genome picks within it, capped by [`StreakConfig::tail`].
pub const TAIL_MIN: u16 = 4;
/// Upper end of the tail band (see [`TAIL_MIN`]).
pub const TAIL_MAX: u16 = 14;

/// Peak coverage on a dark ground at full intensity.
pub const ALPHA_DEFAULT: f32 = 0.10;

/// The hard amplitude ceiling: nothing puts more light than this on one cell.
pub const ALPHA_CLAMP: f32 = 0.18;

/// Momentum at or above which per-row spawning stops and the single ribbon takes over.
pub const SATURATED: f32 = 0.72;

/// Momentum below which an episode is over: the voice re-arms and one
/// [`StreakSound::Settle`] is heard. Read through [`TypingMomentum::low_crossing`], exact at
/// any frame rate.
pub const EPISODE_LOW: f32 = 0.10;

/// How long the ribbon lingers after the last licensed output before retiring.
pub const RIBBON_LINGER_MS: u64 = 600;

/// Hue span across one comet, in spectrum turns: about a fifth of the arc reads as a sheen; a
/// full turn would be a rainbow bar over the text.
pub const HUE_SPAN: f32 = 0.22;

/// Ribbon hue drift, turns per second: a drift, not a cycle. Spectrum luminance is not flat,
/// so a cell under the ribbon sees a sub-hertz luminance rhythm; the drift only recolours
/// light already there, and the ribbon's edges (re-homes) spend from [`SPAWN_MIN_GAP_MS`].
pub const RIBBON_DRIFT: f32 = 0.25;

/// The most comets resident at once (`max_streaks` is clamped into `1..=MAX_COMETS`).
pub const MAX_COMETS: usize = 4;

/// Largest frame step the sweep clock honours, in seconds, so a window that slept does not
/// fling every comet to its end in one tick.
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
    /// The comet's spectrum entry phase `0..1`; the host maps it to a lattice degree so the
    /// theme shades the pip.
    pub hue: f32,
}

/// Resolved per-frame inputs; `Copy` so the host reads it out before borrowing engine state.
#[derive(Clone, Copy, Debug)]
pub struct StreakConfig {
    /// Master on/off (on by default at the config layer). `false` is fully inert,
    /// byte-identical to a build without the effect.
    pub enabled: bool,
    /// Amplitude `0..=1`, the reduced-motion / load-shed scale. `0` is fully inert: a reset,
    /// not a dimming.
    pub intensity: f32,
    /// Tail length in cells, clamped here into [`TAIL_MIN`]`..=`[`TAIL_MAX`].
    pub tail: u16,
    /// Resident comet cap, clamped here into `1..=`[`MAX_COMETS`].
    pub max_streaks: u8,
    /// Mandatory idle drain: with no licensed output for this long, everything retires. A
    /// non-finite or non-positive value drains immediately.
    pub idle_secs: f32,
    /// Whether the theme background is dark: additive spectrum light on dark grounds, a tinted
    /// shadow on light ones (additive light cannot darken a pale ground).
    pub dark_theme: bool,
    /// The resolved theme background `0x00RRGGBB`. Not read by this engine; polarity comes
    /// from `dark_theme`.
    pub theme_bg: u32,
    /// The resolved theme foreground `0x00RRGGBB`, mixed into the light-theme shadow for
    /// legibility.
    pub theme_fg: u32,
    /// The theme's cursor colour (the host falls back to the foreground): its nearest spectrum
    /// stop sets where on the arc a comet's head enters.
    pub theme_cursor: u32,
    /// Whether a cue may be recorded at all (the host's own sound gates still apply).
    pub sound: bool,
}

impl Default for StreakConfig {
    #[inline]
    fn default() -> Self {
        Self {
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
        }
    }
}

/// What a tick produced.
#[derive(Clone, Copy, Debug, Default)]
pub struct StreakFrame {
    /// Fingerprint of everything emitted this frame; exactly `0` when nothing
    /// was, so a settled engine contributes nothing to the host's repaint key.
    pub fp: u64,
    /// The one cue to play, if this frame crossed a sounding edge.
    pub cue: Option<StreakCue>,
}

/// One resident comet.
#[derive(Clone, Copy, Debug)]
struct Comet {
    row: u16,
    from_col: i32,
    to_col: i32,
    /// Sweep progress `0..=1`; retires at 1.
    progress: f32,
    /// Seconds the whole sweep takes.
    dur_s: f32,
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

/// The glyph identity a `RenderCell` carries, without colours. Grapheme tails live in
/// `RenderInput` channels this API does not receive, so a tail-only change claims nothing.
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
    fn same_row(old: &[Self], new: &[aterm_core::terminal::RenderCell]) -> bool {
        (0..old.len().max(new.len())).all(|col| {
            old.get(col).copied().unwrap_or(Self::BLANK)
                == new.get(col).map_or(Self::BLANK, Self::of)
        })
    }
}

/// The prism wake state machine, one per pane.
///
/// The pipeline keys engines by `(session id, visible pane slot)` (`streak_panes`, plus
/// `streak` for the single-pane present): each view has its own arrival clock, so a chatty
/// pane cannot spend a quiet pane's spawn budget. The window-wide WCAG ignition allowance is
/// the word-nova's `FlashLimiterWindow` (the census `flash-limiter` scope claim); this engine
/// never reserves against it, which is safe because WCAG 2.3.1's rate arm is per cell and a
/// cell belongs to one pane. N engines do multiply the flashing area; no margin is claimed.
#[derive(Default)]
pub struct OutputStreak {
    /// The output metric: its own [`TypingMomentum`], fed only by licensed output.
    momentum: TypingMomentum,
    /// Last arrival token; `None` until baselined. Any change (not only an increase) is an
    /// arrival, so a counter, a content hash or a synthetic frame seq all work.
    last_token: Option<u64>,
    /// Prior visible glyphs for the cell API (a changed token says some row changed, not which).
    /// Refreshed even for discounted echoes; bounded by and reused with the visible geometry.
    observed_rows: Vec<Vec<ObservedGlyph>>,
    /// The host's viewport dimensions, independent of sparse row lengths: width reflow moves
    /// old ink without printing new text.
    observed_viewport: Option<(usize, usize)>,
    /// Last accepted editor-input stamp, for the echo discount.
    last_key: Option<Instant>,
    /// Newest licensed span awaiting a spawn decision.
    pending: Option<Pending>,
    /// Stamp of the last licensed output (drives the ribbon and the idle drain).
    last_output: Option<Instant>,
    /// Last spawn stamp — the [`SPAWN_MIN_GAP_MS`] floor.
    last_spawn: Option<Instant>,
    /// Last cue stamp — the [`SOUND_MIN_GAP_MS`] floor.
    last_sound: Option<Instant>,
    /// Whether the voice has already spoken this episode.
    in_episode: bool,
    /// Resident comets, oldest slots first; `None` is a free slot.
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

impl OutputStreak {
    /// A fresh engine with variation `seed`; equal seeds replay equal art.
    #[inline]
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            seed,
            ..Self::default()
        }
    }

    /// Whether the host must keep arming frames; one bool read, no clock.
    #[inline]
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.drawing
    }

    /// The next instant at which something visibly changes, or `None` at rest, which lets the
    /// window park.
    #[inline]
    #[must_use]
    pub fn next_change_deadline(&self, now: Instant, idle_secs: f32) -> Option<Instant> {
        if !self.drawing {
            // Nothing on glass: a live episode still owes its idle cutoff and
            // its analytic Settle; wake at the earlier.
            return [
                self.pending_idle_deadline(idle_secs),
                self.pending_settle_deadline(),
            ]
            .into_iter()
            .flatten()
            .min()
            .map(|deadline| deadline.max(now));
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
        // An invalid or unrepresentable duration cuts the episode off at
        // once, as in `idle_drained`.
        Some(last.checked_add(idle).unwrap_or(last))
    }

    /// The instant the current episode drains, if one is open and owed a
    /// [`StreakSound::Settle`].
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
        *self = Self {
            seed,
            ..Self::default()
        };
    }

    /// Forget the arrival token so the next observation baselines: the session/tab-switch seam
    /// for a single-engine host, which would otherwise answer the swap with a comet.
    #[inline]
    pub fn rebase(&mut self) {
        self.last_token = None;
        self.observed_rows.clear();
        self.observed_viewport = None;
    }

    /// One accepted editor input at `now` (a key, mapped sequence, IME text or paste receipt),
    /// for the echo discount.
    #[inline]
    pub const fn note_keystroke(&mut self, now: Instant) {
        self.last_key = Some(now);
    }

    /// A submitted turn boundary at `now`. Clears only an older or equal editor shadow: hosts
    /// may replay their latest receipt every frame, and an old boundary must not erase newer
    /// input.
    #[inline]
    pub fn note_turn_boundary(&mut self, now: Instant) {
        if self.last_key.is_none_or(|accepted| accepted <= now) {
            self.last_key = None;
        }
    }

    /// One frame's output observation: the content token, the fresh ink-bearing row spans (the
    /// host filters blank damage), and whether the host has input in flight. `true` when it
    /// was licensed (credited momentum and armed a spawn).
    #[inline]
    pub fn note_output(
        &mut self,
        token: u64,
        spans: &[(u16, u16, u16)],
        now: Instant,
        input_hot: bool,
    ) -> bool {
        // First observation baselines; after that any token change is an
        // arrival, and an unchanged token (a repaint) is none.
        let Some(prev) = self.last_token else {
            self.last_token = Some(token);
            return false;
        };
        self.last_token = Some(token);
        if token == prev {
            return false;
        }
        // Empty damage mints nothing.
        let Some(&(row, left, right)) = spans.last() else {
            return false;
        };
        // Echo discount: the user's own typing coming back.
        if input_hot || self.within_echo_window(now) {
            return false;
        }
        self.momentum.advance(now);
        self.last_output = Some(now);
        self.pending = Some(Pending { row, left, right });
        true
    }

    /// [`Self::note_output`] with the anchor derived from a frame's cells, shared by every
    /// render path.
    ///
    /// The fresh row is the cursor's while it is mid-line and the row above otherwise (output
    /// lines end with a newline). The span is the rightmost contiguous run of gained or
    /// replaced ink (wide continuations included) since the previous observation, so a timer
    /// changing elsewhere cannot decorate an unchanged composer. Erasure alone, and a row moved
    /// intact, are not output.
    ///
    /// `viewport` is the host's actual `(rows, cols)`, not the sparse row lengths: a dimension
    /// change baselines afresh even under an unchanged token.
    pub fn note_output_cells(
        &mut self,
        cells: &[Vec<aterm_core::terminal::RenderCell>],
        cursor: (usize, usize),
        viewport: (usize, usize),
        token: u64,
        now: Instant,
        input_hot: bool,
    ) -> bool {
        // Geometry can change before the terminal publishes a new token, so
        // the walk is skipped only when both are unchanged.
        if self.last_token == Some(token) && self.observed_viewport == Some(viewport) {
            return self.note_output(token, &[], now, input_hot);
        }
        let (rows, _) = viewport;
        let visible = &cells[..rows.min(cells.len())];
        // Render rows are sparse prefixes: a row growing is output, not a
        // resize. Only the supplied `viewport` identifies reflow.
        let baseline = self.last_token.is_none()
            || self.observed_viewport != Some(viewport)
            || self.observed_rows.len() != visible.len();
        let (cursor_row, cursor_col) = cursor;
        let anchor_row = if cursor_col > 0 {
            cursor_row
        } else {
            cursor_row.saturating_sub(1)
        };
        let span = visible
            .get(anchor_row)
            .and_then(|row| {
                if baseline || input_hot || self.within_echo_window(now) {
                    return None;
                }
                // One pending span: keep the rightmost run rather than joining
                // remote changes across a static composer.
                let old_anchor = &self.observed_rows[anchor_row];
                let mut extent: Option<(usize, usize)> = None;
                for (col, cell) in row.iter().enumerate() {
                    let old = old_anchor.get(col).copied().unwrap_or(ObservedGlyph::BLANK);
                    let new = ObservedGlyph::of(cell);
                    let continuation = cell.wide && extent.is_some_and(|(_, end)| end + 1 == col);
                    if (new.has_ink() && old != new) || continuation {
                        extent = Some(match extent {
                            Some((left, end)) if end + 1 == col => (left, col),
                            _ => (col, col),
                        });
                    }
                }
                let extent = extent?;
                // An exact old row now at the anchor is a move (a TUI moving
                // its input box, or scrolling), unless it is also still at its
                // old position: repeated output lines may duplicate a row.
                let moved_row = self.observed_rows.iter().enumerate().any(|(index, old)| {
                    index != anchor_row
                        && ObservedGlyph::same_row(old, row)
                        && !ObservedGlyph::same_row(old, &visible[index])
                });
                (!moved_row).then_some(extent)
            })
            .map(|(start, end)| {
                [(
                    anchor_row as u16,
                    u16::try_from(start).unwrap_or(u16::MAX),
                    u16::try_from(end).unwrap_or(u16::MAX),
                )]
            });
        // Retain every row, echo-discounted ones included, so revisiting one
        // later does not replay old output as a fresh arrival.
        self.observed_rows.resize_with(visible.len(), Vec::new);
        self.observed_viewport = Some(viewport);
        for (old, row) in self.observed_rows.iter_mut().zip(visible) {
            old.clear();
            old.extend(row.iter().map(ObservedGlyph::of));
        }
        if baseline {
            // History was lost: no resident coordinate survives onto the new
            // grid and no old pending span fires.
            self.comets = Default::default();
            self.ribbon = None;
            self.pending = None;
            self.drawing = false;
        } else {
            self.retire_light_over_erased_ink();
        }
        self.note_output(
            token,
            span.as_ref().map_or(&[][..], |s| &s[..]),
            now,
            input_hot,
        )
    }

    /// Retire every comet (and the flood ribbon) whose licensed ink is gone
    /// from the rows just observed: a cleared screen (`Ctrl+L`, `clear`), an
    /// erased line. Its light marks output that is no longer there, and left
    /// to its sweep it would linger over the blank cells it was laid on, a
    /// dim ghost of the old text for the rest of its life (critic #15: the
    /// old prompt's `$` cell after `Ctrl+L`). A comet whose span still holds
    /// any ink sweeps on. Removing a tint of at most [`ALPHA_CLAMP`] is no
    /// flash.
    fn retire_light_over_erased_ink(&mut self) {
        let observed = &self.observed_rows;
        let inked = |row: u16, from: i32, to: i32| {
            to >= 0
                && from <= to
                && observed.get(usize::from(row)).is_some_and(|cells| {
                    let start = usize::try_from(from.max(0)).unwrap_or(usize::MAX);
                    let end = usize::try_from(to)
                        .ok()
                        .map_or(0, |e| e.saturating_add(1).min(cells.len()));
                    start < end && cells[start..end].iter().any(|glyph| glyph.has_ink())
                })
        };
        for slot in &mut self.comets {
            if slot.is_some_and(|c| !inked(c.row, c.from_col, c.to_col)) {
                *slot = None;
            }
        }
        if self
            .ribbon
            .is_some_and(|r| !inked(r.row, r.from_col, r.to_col))
        {
            self.ribbon = None;
        }
    }

    /// Whether `now` sits inside the echo shadow of the last accepted input.
    #[inline]
    fn within_echo_window(&self, now: Instant) -> bool {
        self.last_key.is_some_and(|k| {
            now.saturating_duration_since(k) <= Duration::from_millis(ECHO_DISCOUNT_MS)
        })
    }

    /// Advance one frame at `now` and append this frame's light to `out`. No wall clock, no
    /// allocation beyond `out`'s growth, and byte-identical replays for identical inputs.
    pub fn tick(
        &mut self,
        now: Instant,
        geom: Geom,
        cfg: &StreakConfig,
        out: &mut Vec<GlowQuad>,
    ) -> StreakFrame {
        // Fully inert: off, zero amplitude, or degenerate geometry. A reset,
        // not a dimming.
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

        let dt = self
            .last_tick
            .map(|t| now.saturating_duration_since(t).as_secs_f32())
            .unwrap_or(0.0)
            .clamp(0.0, MAX_DT_S);
        self.last_tick = Some(now);

        // Mandatory idle drain: no configuration animates forever.
        if self.idle_drained(now, cfg) {
            let cue = self.close_episode_cue(now, cfg);
            self.comets = Default::default();
            self.ribbon = None;
            self.pending = None;
            // The idle transition is consumed; a later Settle, if owed, is now
            // the sole wake (keeping this stamp would return an overdue one).
            self.last_output = None;
            self.drawing = false;
            return StreakFrame { fp: 0, cue };
        }

        let m = self.momentum.value(now);
        let cue = self.spend_pending(now, m, cfg, geom).or_else(|| {
            // No spawn this frame; the episode may still have drained.
            self.close_episode_cue(now, cfg)
        });

        self.advance_comets(dt);
        self.advance_ribbon(dt, now, m);

        let fp = self.emit(geom, cfg, out);
        self.drawing = fp != 0;
        StreakFrame { fp, cue }
    }

    /// Whether the idle drain has expired. A non-finite or non-positive
    /// `idle_secs` drains immediately rather than animating forever.
    #[inline]
    fn idle_drained(&self, now: Instant, cfg: &StreakConfig) -> bool {
        let Some(last) = self.last_output else {
            // Nothing licensed yet: only resident light keeps the engine awake.
            return self.comets.iter().all(Option::is_none) && self.ribbon.is_none();
        };
        idle_duration(cfg.idle_secs).is_none_or(|idle| now.saturating_duration_since(last) >= idle)
    }

    /// Spend a pending licensed observation: spawn a comet, refresh the flood
    /// ribbon, or decline. Returns the one cue this edge earned, if any.
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

        // Governor 4, flood coalescing: past saturation the single ribbon
        // carries the activity, mutely.
        if m >= SATURATED {
            self.refresh_ribbon(now, pending, cfg, geom);
            return None;
        }

        // Governor 2, the ignition floor (cues ride only spawn edges, so it
        // bounds the sound too).
        if let Some(last) = self.last_spawn
            && now.saturating_duration_since(last) < Duration::from_millis(SPAWN_MIN_GAP_MS)
        {
            return None;
        }
        // Governor 3, per-row dedup: a row already carrying a comet waits.
        if self.comets.iter().flatten().any(|c| c.row == pending.row) {
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
            dur_s: dur_ms as f32 / 1000.0,
            tail: tail.min(cfg.tail.clamp(TAIL_MIN, TAIL_MAX)),
            hue,
        });
        self.last_spawn = Some(now);

        self.open_episode_cue(now, cfg, pending, geom, hue)
    }

    /// The first free comet slot under the host's resident cap.
    #[inline]
    fn free_slot(&self, cfg: &StreakConfig) -> Option<usize> {
        let cap = (cfg.max_streaks as usize).clamp(1, MAX_COMETS);
        self.comets.iter().take(cap).position(Option::is_none)
    }

    /// Governor 6, opening half: one pip on the quiet→active crossing, then mute. A fire denied
    /// by [`SOUND_MIN_GAP_MS`] is not heard and does not re-arm.
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

    /// Per-theme character: the theme's cursor colour, snapped to its nearest named spectrum
    /// stop, decides where the comet's head enters the arc; the genome jitters within that stop.
    #[inline]
    fn hue_for(&self, pending: Pending, cfg: &StreakConfig) -> f32 {
        let named = nearest_spectrum_stop_by_hue(cfg.theme_cursor);
        let anchor = spectrum_stop_position(named);
        let key = genome::mix(self.seed ^ u64::from(pending.row));
        let jitter = (genome::field(key, 0, 8) as f32 / 255.0 * 2.0 - 1.0)
            * spectrum_stop_jitter_radius(named);
        // The spectrum is an arc: jitter clamps at red/violet instead of
        // wrapping to the opposite named colour.
        (anchor + jitter).clamp(0.0, 1.0)
    }

    /// Refresh, open or move the flood ribbon on the newest output row. A move takes light off
    /// one row and puts it on another, so it spends from the shared ignition floor
    /// (`last_spawn`); otherwise a cursor alternating between two rows (a two-line progress
    /// display) would re-home it at the frame rate. The linger clock and hue drift refresh on
    /// every observation and only recolour. Cost: the ribbon can lag the newest row by up to
    /// [`SPAWN_MIN_GAP_MS`].
    fn refresh_ribbon(&mut self, now: Instant, pending: Pending, cfg: &StreakConfig, geom: Geom) {
        // Clip first: comparing an unclipped span with the stored one would
        // see a change every frame for a flood past the last column.
        let Some((from_col, to_col)) = clipped_span(pending, geom.cols) else {
            return;
        };
        let want = (pending.row, from_col, to_col);
        if let Some(r) = self.ribbon.as_mut() {
            // The flood is still running: the linger clock refreshes regardless.
            r.since_output_s = 0.0;
            if (r.row, r.from_col, r.to_col) == want {
                // Same geometry. No light moved, so there is nothing to charge.
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
    fn advance_comets(&mut self, dt: f32) {
        for slot in &mut self.comets {
            let Some(c) = slot else { continue };
            c.progress += dt / c.dur_s.max(0.001);
            if c.progress >= 1.0 {
                *slot = None;
            }
        }
    }

    /// Drift the ribbon's hue and retire it once the flood stops.
    #[inline]
    fn advance_ribbon(&mut self, dt: f32, now: Instant, m: f32) {
        let Some(r) = self.ribbon.as_mut() else {
            return;
        };
        r.hue = (r.hue + dt * RIBBON_DRIFT).rem_euclid(1.0);
        r.since_output_s += dt;
        let stale = self.last_output.is_some_and(|t| {
            now.saturating_duration_since(t) > Duration::from_millis(RIBBON_LINGER_MS)
        });
        if stale || m < EPISODE_LOW {
            self.ribbon = None;
        }
    }

    /// Emit this frame's light and return its fingerprint (`0` ⇒ nothing drawn).
    fn emit(&self, geom: Geom, cfg: &StreakConfig, out: &mut Vec<GlowQuad>) -> u64 {
        let peak = (ALPHA_DEFAULT * cfg.intensity.clamp(0.0, 1.0)).min(ALPHA_CLAMP);
        // Governor 5: halved on a light ground, where the light flips to a
        // tinted shadow (additive light cannot darken a pale ground).
        let amp = if cfg.dark_theme { peak } else { peak * 0.5 };
        if amp <= 0.0 {
            return 0;
        }
        let before = out.len();
        let mut fp: u64 = 0;

        for c in self.comets.iter().flatten() {
            // Monotone fade: one decay across the life, so a comet has no
            // flash rate to bound.
            let life = 1.0 - c.progress;
            let head = c.from_col as f32 + (c.to_col - c.from_col) as f32 * c.progress;
            // The tail stays on licensed ink: a one-cell rewrite must not sweep
            // the unchanged cells to its left.
            let tail_f = f32::from(c.tail);
            let tail_denom = tail_f.max(1.0);
            let tail_start = (head - tail_f).max(c.from_col as f32);
            let row = i32::from(c.row);
            // The inclusive range `row_sweep_cells` would yield, emitted
            // directly so the tick stays allocation-free.
            for col in tail_start.floor() as i32..=head.round() as i32 {
                let u = ((head - col as f32) / tail_denom).clamp(0.0, 1.0);
                // Bright head, fading tail; squared so the tail thins fast and
                // the comet reads as motion rather than a painted bar.
                let shape = (1.0 - u) * (1.0 - u);
                self.push_cell(
                    out,
                    geom,
                    cfg,
                    amp,
                    row,
                    col,
                    c.hue + u * HUE_SPAN,
                    shape * life,
                );
            }
            fp = fp
                .wrapping_mul(1_000_003)
                .wrapping_add((c.progress * 4096.0) as u64)
                .wrapping_add(u64::from(c.row) << 20);
        }

        if let Some(r) = self.ribbon {
            // A flat, constant-cost wash: no head or sweep, low amplitude; the
            // re-home, not the hue drift, is the edge the floor charges.
            let fade =
                1.0 - (r.since_output_s / (RIBBON_LINGER_MS as f32 / 1000.0)).clamp(0.0, 1.0) * 0.5;
            let cols_f = geom.cols.max(1) as f32;
            let row = i32::from(r.row);
            let shape = 0.55 * fade;
            for col in r.from_col..=r.to_col {
                let t = r.hue + (col as f32 / cols_f) * HUE_SPAN;
                self.push_cell(out, geom, cfg, amp, row, col, t, shape);
            }
            fp = fp
                .wrapping_mul(31)
                .wrapping_add((r.hue * 4096.0) as u64)
                .wrapping_add(u64::from(r.row) << 8);
        }

        if out.len() == before {
            return 0;
        }
        // Never 0 while light is on glass.
        fp | 1
    }

    /// One cell of streak light, in the polarity the theme calls for.
    #[allow(
        clippy::too_many_arguments,
        reason = "cell coordinate + hue + shape + the frame's geometry/config; one internal \
                  call site per emitter arm (the `cursor_comet::emit_coma` precedent)"
    )]
    #[inline]
    fn push_cell(
        &self,
        out: &mut Vec<GlowQuad>,
        geom: Geom,
        cfg: &StreakConfig,
        amp: f32,
        row: i32,
        col: i32,
        hue_t: f32,
        shape: f32,
    ) {
        if row < 0 || col < 0 || shape <= 0.0 {
            return;
        }
        let a = (amp * shape.clamp(0.0, 1.0) * 255.0).round() as u8;
        if a == 0 {
            return;
        }
        let base = spectrum(hue_t.rem_euclid(1.0));
        let x = col * geom.cw as i32;
        let y = row * geom.ch as i32;
        let (w, h) = (geom.cw as i32, geom.ch as i32);

        if cfg.dark_theme {
            push_grid_quad(out, geom, x, y, w, h, premul_rgb(base, a), 0);
        } else {
            // Shadow shimmer: the hue pulled toward black and the theme's ink,
            // composited source-over so it darkens.
            let shade = lerp_rgb(lerp_rgb(base, 0x0000_0000, 0.55), cfg.theme_fg, 0.25);
            push_grid_quad(out, geom, x, y, w, h, premul_rgb(shade, a), a);
        }
    }
}

/// Clip one licensed span to the visible columns without relocating a wholly
/// off-screen or malformed span onto the viewport edge.
#[inline]
fn clipped_span(pending: Pending, cols: usize) -> Option<(i32, i32)> {
    if pending.right < pending.left || usize::from(pending.left) >= cols {
        return None;
    }
    let last_col = i32::try_from(cols.checked_sub(1)?).unwrap_or(i32::MAX);
    Some((
        i32::from(pending.left),
        i32::from(pending.right).min(last_col),
    ))
}

/// The mandatory idle duration. Scheduling and consumption share this one rounded `Duration`,
/// so a wake at the promised boundary is consumed on that tick.
#[inline]
fn idle_duration(idle_secs: f32) -> Option<Duration> {
    (idle_secs.is_finite() && idle_secs > 0.0)
        .then(|| Duration::try_from_secs_f32(idle_secs).ok())
        .flatten()
}

/// Equal-power pan position `-1..=1` from a span's centre column.
#[inline]
const fn pan_of(pending: Pending, geom: Geom) -> f32 {
    let cols = if geom.cols < 1 { 1 } else { geom.cols } as f32;
    let centre = (pending.left as f32 + pending.right as f32) * 0.5;
    ((centre / cols) * 2.0 - 1.0).clamp(-1.0, 1.0)
}

/// Nearest named spectrum colour by circular HSV hue. Comparing hues, not
/// `spectrum_stop_position`'s arc parameter, keeps the two domains apart when the arc is
/// perceptually re-paced.
#[inline]
fn nearest_spectrum_stop_by_hue(rgb: u32) -> usize {
    let hue = hue_position_of(rgb);
    let mut nearest = 0;
    let mut nearest_distance = f32::INFINITY;
    for index in 0..crate::spectrum::SPECTRUM_STOPS {
        let stop_hue = hue_position_of(spectrum_stop(index));
        let direct = (hue - stop_hue).abs();
        let distance = direct.min(1.0 - direct);
        if distance < nearest_distance {
            nearest = index;
            nearest_distance = distance;
        }
    }
    nearest
}

/// Half the narrower adjacent arc gap of one named stop: its Voronoi radius, so genome jitter
/// cannot cross into a neighbour however the arc is paced. Endpoints use their one gap and
/// clamp to the arc in `hue_for`.
#[inline]
const fn spectrum_stop_jitter_radius(index: usize) -> f32 {
    let last = crate::spectrum::SPECTRUM_STOPS.saturating_sub(1);
    if last == 0 {
        return 0.0;
    }
    let index = if index > last { last } else { index };
    let at = spectrum_stop_position(index);
    let gap = if index == 0 {
        spectrum_stop_position(1) - at
    } else if index == last {
        at - spectrum_stop_position(last - 1)
    } else {
        (at - spectrum_stop_position(index - 1)).min(spectrum_stop_position(index + 1) - at)
    };
    gap.max(0.0) * 0.5
}

/// A colour's circular HSV hue turn (`0..1`), used only to pick the theme's named stop; it is
/// not a spectrum-arc parameter.
#[inline]
fn hue_position_of(rgb: u32) -> f32 {
    let r = ((rgb >> 16) & 0xff) as f32 / 255.0;
    let g = ((rgb >> 8) & 0xff) as f32 / 255.0;
    let b = (rgb & 0xff) as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    if d <= f32::EPSILON {
        return 0.0;
    }
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h / 6.0).rem_euclid(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geom() -> Geom {
        Geom {
            cw: 8,
            ch: 16,
            rows: 24,
            cols: 80,
            origin_x: 0,
            origin_y: 0,
            win_w: 640,
            win_h: 384,
            head: 0,
        }
    }

    fn cfg_on() -> StreakConfig {
        StreakConfig {
            enabled: true,
            sound: true,
            ..StreakConfig::default()
        }
    }

    /// The host's spans for one ordinary line of output.
    fn span(row: u16) -> [(u16, u16, u16); 1] {
        [(row, 0, 40)]
    }

    /// Drive `steps` frames of `step` from `t0`, returning every cue heard and
    /// the peak quad count any single frame put on glass.
    fn run(
        e: &mut OutputStreak,
        t0: Instant,
        steps: u32,
        step: Duration,
        cfg: &StreakConfig,
        mut before_tick: impl FnMut(&mut OutputStreak, Instant, u32),
    ) -> (Vec<StreakCue>, usize) {
        let mut cues = Vec::new();
        let mut peak = 0;
        for i in 0..steps {
            let now = t0 + step * i;
            before_tick(e, now, i);
            let mut out = Vec::new();
            let f = e.tick(now, geom(), cfg, &mut out);
            peak = peak.max(out.len());
            if let Some(c) = f.cue {
                cues.push(c);
            }
        }
        (cues, peak)
    }

    /// An unchanged token is a repaint, not an arrival, and [`OutputStreak::rebase`] makes the
    /// next observation baseline.
    #[test]
    fn an_unchanged_token_is_no_arrival_and_rebase_baselines() {
        let t0 = Instant::now();
        let mut e = OutputStreak::new(7);
        assert!(
            !e.note_output(500, &span(1), t0, false),
            "the first observation only baselines"
        );
        assert!(
            !e.note_output(500, &span(1), t0 + Duration::from_millis(10), false),
            "the same token twice is a repaint, not an arrival"
        );
        assert!(
            e.note_output(501, &span(1), t0 + Duration::from_millis(20), false),
            "a changed token IS an arrival"
        );
        // A host re-pointing this engine at another terminal rebases, so the
        // swap itself mints nothing…
        e.rebase();
        assert!(!e.note_output(9_000, &span(1), t0 + Duration::from_millis(30), false));
        // …and the very next real change is licensed again.
        assert!(e.note_output(9_001, &span(1), t0 + Duration::from_millis(40), false));
    }

    /// The cell path checks the host's token before deriving geometry: glyph churn under an
    /// unchanged token is no observation, and an advanced token licenses only the changed run.
    #[test]
    fn cell_observation_is_gated_by_the_host_token_before_geometry() {
        let t0 = Instant::now();
        let mut e = OutputStreak::new(9);
        let mut cells = vec![vec![aterm_core::terminal::RenderCell::default(); 8]; 3];
        cells[1][2].ch = 'a';
        assert!(!e.note_output_cells(&cells, (2, 0), (3, 8), 10, t0, false));

        cells[1][6].ch = 'z';
        assert!(
            !e.note_output_cells(
                &cells,
                (2, 0),
                (3, 8),
                10,
                t0 + Duration::from_millis(20),
                false,
            ),
            "glyph churn without a content-clock edge is only a repaint"
        );
        assert!(e.pending.is_none());

        assert!(e.note_output_cells(
            &cells,
            (2, 0),
            (3, 8),
            11,
            t0 + Duration::from_millis(40),
            false,
        ));
        let pending = e.pending.expect("changed token licenses the ink row");
        assert_eq!(
            (pending.row, pending.left, pending.right),
            (1, 6, 6),
            "unchanged ink and gaps stay outside the newly changed run"
        );
    }

    /// A comet over output that a clear then erases retires with it (critic
    /// #15's ghost: the old prompt's `$` cell kept a dim tint for the rest of
    /// the comet's sweep after `Ctrl+L`). One row of output earns a comet;
    /// the next observation has that row blank (the screen cleared, a new
    /// prompt on row 0) and the next frame paints nothing on the old row.
    /// Negative control: the same clear arriving with the old row's ink still
    /// there (only row 0 changed) leaves the comet sweeping on that row.
    #[test]
    fn a_comet_retires_when_its_ink_is_cleared() {
        let row_lit = |clear: bool| {
            let t0 = Instant::now();
            let mut e = OutputStreak::new(11);
            let cfg = cfg_on();
            let blank = vec![vec![aterm_core::terminal::RenderCell::default(); 8]; 3];
            let mut cells = blank.clone();
            assert!(!e.note_output_cells(&cells, (2, 0), (3, 8), 10, t0, false));
            // `$ ` printed on row 1, the caret parked after it.
            cells[1][0].ch = '$';
            let t1 = t0 + Duration::from_millis(40);
            assert!(e.note_output_cells(&cells, (1, 2), (3, 8), 11, t1, false));
            let mut out = Vec::new();
            e.tick(t1, geom(), &cfg, &mut out);
            assert!(
                out.iter().any(|q| q.row == 1),
                "the output earns a comet on its row"
            );
            // `Ctrl+L`: the new prompt on row 0, row 1 cleared (or, for the
            // control, left as it was).
            let mut after = if clear { blank.clone() } else { cells.clone() };
            after[0][0].ch = '$';
            let t2 = t1 + Duration::from_millis(50);
            e.note_output_cells(&after, (0, 2), (3, 8), 12, t2, false);
            out.clear();
            e.tick(t2, geom(), &cfg, &mut out);
            out.iter().any(|q| q.row == 1)
        };
        assert!(!row_lit(true), "a cleared row keeps no ghost of its comet");
        assert!(
            row_lit(false),
            "control: a comet over ink still there sweeps on"
        );
    }

    #[test]
    fn cell_observation_all_blank_row_mints_nothing() {
        let t0 = Instant::now();
        let mut e = OutputStreak::new(9);
        let cells = vec![vec![aterm_core::terminal::RenderCell::default(); 8]; 3];
        assert!(!e.note_output_cells(&cells, (2, 0), (3, 8), 10, t0, false));
        assert!(!e.note_output_cells(
            &cells,
            (2, 0),
            (3, 8),
            11,
            t0 + Duration::from_millis(20),
            false,
        ));
        assert!(e.pending.is_none(), "a blank anchor row has no ink extent");
    }

    #[test]
    fn theme_hue_uses_named_stop_positions_with_bounded_non_wrapping_jitter() {
        let pending = Pending {
            row: 9,
            left: 3,
            right: 12,
        };
        let last = crate::spectrum::SPECTRUM_STOPS - 1;
        for named in 0..crate::spectrum::SPECTRUM_STOPS {
            let cursor = spectrum_stop(named);
            assert_eq!(
                nearest_spectrum_stop_by_hue(cursor),
                named,
                "named stop {named} must select itself"
            );
            let cfg = StreakConfig {
                theme_cursor: cursor,
                ..cfg_on()
            };
            let anchor = spectrum_stop_position(named);
            let lower_midpoint = if named == 0 {
                0.0
            } else {
                (spectrum_stop_position(named - 1) + anchor) * 0.5
            };
            let upper_midpoint = if named == last {
                1.0
            } else {
                (anchor + spectrum_stop_position(named + 1)) * 0.5
            };
            for seed in 0..32 {
                let actual = OutputStreak::new(seed).hue_for(pending, &cfg);
                assert!(
                    (lower_midpoint..=upper_midpoint).contains(&actual),
                    "jitter escaped stop {named}'s Voronoi interval: {actual} not in \
                     {lower_midpoint}..={upper_midpoint}"
                );
            }
        }

        // The spectrum is an arc: positive jitter on exact violet pins at the
        // end instead of wrapping to red.
        let violet_edge = StreakConfig {
            theme_cursor: spectrum_stop(crate::spectrum::SPECTRUM_STOPS - 1),
            ..cfg_on()
        };
        assert_eq!(
            nearest_spectrum_stop_by_hue(violet_edge.theme_cursor),
            crate::spectrum::SPECTRUM_STOPS - 1
        );
        let violet_lower = (spectrum_stop_position(last - 1) + spectrum_stop_position(last)) * 0.5;
        for seed in 0..256 {
            let actual = OutputStreak::new(seed).hue_for(pending, &violet_edge);
            assert!(actual >= violet_lower, "violet jitter wrapped: {actual}");
        }

        // #55FF00 is HSV hue 100°, nearer green (120°) than yellow (60°):
        // catches an accidental arc-parameter comparison.
        assert_eq!(nearest_spectrum_stop_by_hue(0x0055_FF00), 3);
    }

    #[test]
    fn clipping_rejects_malformed_and_wholly_off_right_spans() {
        assert_eq!(
            clipped_span(
                Pending {
                    row: 0,
                    left: 6,
                    right: 12,
                },
                8,
            ),
            Some((6, 7)),
            "a partly visible span clips only its right edge"
        );
        assert_eq!(
            clipped_span(
                Pending {
                    row: 0,
                    left: 8,
                    right: 12,
                },
                8,
            ),
            None,
            "a wholly off-right span must not relocate onto the last cell"
        );
        assert_eq!(
            clipped_span(
                Pending {
                    row: 0,
                    left: 5,
                    right: 4,
                },
                8,
            ),
            None,
            "a reversed span is malformed rather than a one-cell flash"
        );
    }

    /// Accepted editor input (its receipt, or the host's `input_hot`) mints nothing.
    #[test]
    fn echo_discount_mints_nothing_including_paste() {
        let t0 = Instant::now();
        let mut e = OutputStreak::new(1);
        e.note_output(1, &span(2), t0, false); // baseline
        e.note_keystroke(t0 + Duration::from_millis(100));
        assert!(
            !e.note_output(2, &span(2), t0 + Duration::from_millis(200), false),
            "a delta inside the echo window is the user's own typing"
        );
        // Active/spilled input is discounted before its completion receipt.
        let mut e2 = OutputStreak::new(1);
        e2.note_output(1, &span(2), t0, false);
        assert!(
            !e2.note_output(2, &span(2), t0 + Duration::from_secs(9), true),
            "input_hot alone must discount output while input is active/spilled"
        );
    }

    #[test]
    fn submitted_turn_boundary_clears_only_older_echo_shadow() {
        let t0 = Instant::now();
        let mut e = OutputStreak::new(1);
        e.note_output(1, &span(2), t0, false);
        e.note_keystroke(t0 + Duration::from_millis(10));
        e.note_turn_boundary(t0 + Duration::from_millis(20));
        assert!(e.note_output(2, &span(2), t0 + Duration::from_millis(21), false));

        let mut newer = OutputStreak::new(1);
        newer.note_output(1, &span(2), t0, false);
        newer.note_keystroke(t0 + Duration::from_millis(30));
        newer.note_turn_boundary(t0 + Duration::from_millis(20));
        assert!(
            !newer.note_output(2, &span(2), t0 + Duration::from_millis(31), false),
            "a replayed older boundary must not erase newer accepted input"
        );
    }

    /// A token step with no ink-bearing span (`\x1b[2K` over a blank line) licenses nothing.
    #[test]
    fn empty_damage_mints_nothing() {
        let t0 = Instant::now();
        let mut e = OutputStreak::new(1);
        e.note_output(1, &span(0), t0, false);
        assert!(!e.note_output(2, &[], t0 + Duration::from_secs(1), false));
        let mut out = Vec::new();
        let f = e.tick(t0 + Duration::from_secs(1), geom(), &cfg_on(), &mut out);
        assert_eq!(f.fp, 0, "nothing was licensed, so nothing is drawn");
        assert!(out.is_empty());
    }

    /// Reduced motion is exact zero, not eased: amplitude 0 resets the machine, emits no quads,
    /// records no cue, and reports fingerprint 0.
    #[test]
    fn reduced_motion_emits_nothing() {
        let t0 = Instant::now();
        let mut e = OutputStreak::new(3);
        let cfg = cfg_on();
        e.note_output(1, &span(4), t0, false);
        e.note_output(2, &span(4), t0 + Duration::from_millis(400), false);
        let mut warm = Vec::new();
        e.tick(t0 + Duration::from_millis(400), geom(), &cfg, &mut warm);
        assert!(!warm.is_empty(), "premise: the effect is live");

        let reduced = StreakConfig {
            intensity: 0.0,
            ..cfg
        };
        let mut out = Vec::new();
        let f = e.tick(t0 + Duration::from_millis(450), geom(), &reduced, &mut out);
        assert_eq!(f.fp, 0);
        assert!(f.cue.is_none());
        assert!(out.is_empty());
        assert!(!e.is_active(), "and the host may disarm the lane");
    }

    /// Off is byte-identical off: a disabled engine is inert whatever it is fed.
    #[test]
    fn disabled_is_inert() {
        let t0 = Instant::now();
        let mut e = OutputStreak::new(5);
        let cfg = StreakConfig {
            enabled: false,
            ..cfg_on()
        };
        for i in 0..20u64 {
            e.note_output(i, &span(2), t0 + Duration::from_millis(i * 50), false);
            let mut out = Vec::new();
            let f = e.tick(t0 + Duration::from_millis(i * 50), geom(), &cfg, &mut out);
            assert_eq!(f.fp, 0);
            assert!(out.is_empty());
        }
    }

    /// A ribbon re-home is an ignition: a cursor alternating between two rows every frame
    /// re-homes it no faster than [`SPAWN_MIN_GAP_MS`] allows.
    #[test]
    fn a_two_row_alternation_cannot_strobe_the_ribbon() {
        let t0 = Instant::now();
        let mut e = OutputStreak::new(17);
        let cfg = cfg_on();
        let g = geom();

        // 120 Hz for 8 s, alternating rows every frame, past SATURATED so the
        // ribbon is the only thing on glass.
        let steps = 960u64;
        let hz = 120u64;
        let mut homes = 0u32;
        let mut prev: Option<(u16, i32, i32)> = None;
        for i in 0..steps {
            let at = t0 + Duration::from_millis(i * 1000 / hz);
            let row = if i % 2 == 0 { 5 } else { 6 };
            // A big content step per frame keeps momentum saturated.
            e.note_output(i * 4096, &span(row), at, false);
            let mut out = Vec::new();
            e.tick(at, g, &cfg, &mut out);
            let now = e.ribbon.map(|r| (r.row, r.from_col, r.to_col));
            if now.is_some() && now != prev {
                homes += 1;
            }
            prev = now;
        }

        let secs = steps as f32 / hz as f32;
        let per_s = homes as f32 / secs;
        let floor_per_s = 1000.0 / SPAWN_MIN_GAP_MS as f32;
        assert!(
            per_s <= floor_per_s + 0.05,
            "the ribbon re-homed {homes} times in {secs:.1} s = {per_s:.2}/s, above the \
             ignition floor of {floor_per_s:.2}/s. A re-home moves light off one whole row \
             and onto another; unbounded, this is the strobe that measured 60 re-homes per \
             second before the floor covered it."
        );
        assert!(
            homes >= 2,
            "the drive must actually MOVE the ribbon, or this test cannot fail: {homes} \
             re-home(s) means the fixture stopped exercising the alternation"
        );
    }

    /// A flood (1000 content steps a second for 3 s) yields one ribbon at constant cost and at
    /// most one pip.
    #[test]
    fn a_flood_coalesces_to_one_quiet_ribbon() {
        let t0 = Instant::now();
        let mut e = OutputStreak::new(11);
        let cfg = cfg_on();
        let step = Duration::from_millis(1);
        let mut seq = 0u64;
        let (cues, peak) = run(&mut e, t0, 3_000, step, &cfg, |e, now, i| {
            seq = u64::from(i) + 1;
            e.note_output(seq, &span((i % 20) as u16), now, false);
        });
        let pips = cues
            .iter()
            .filter(|c| c.sound == StreakSound::Shimmer)
            .count();
        assert!(
            pips <= 1,
            "a three-second flood is ONE pip, not a drip feed: {pips}"
        );
        // One ribbon row of ≤ cols cells, plus the few comets minted before
        // momentum saturated.
        assert!(
            peak <= 80 * (MAX_COMETS + 1),
            "the flood's quad cost is bounded: {peak}"
        );
    }

    /// The spawn floor bounds ignitions, and since a cue rides only a spawn edge, the sound too.
    #[test]
    fn the_spawn_gap_clamps_eye_and_ear_together() {
        let t0 = Instant::now();
        let mut e = OutputStreak::new(2);
        let cfg = cfg_on();
        // Output every 100 ms on different rows for 2 s, so the floor, not
        // per-row dedup, is what limits spawns.
        let mut spawns = 0;
        for i in 0..20u64 {
            let now = t0 + Duration::from_millis(i * 100);
            e.note_output(i + 1, &span((i % 20) as u16), now, false);
            let before = e.last_spawn;
            let mut out = Vec::new();
            e.tick(now, geom(), &cfg, &mut out);
            if e.last_spawn != before {
                spawns += 1;
            }
        }
        assert!(
            spawns <= 4,
            "≤2 ignitions/second across 2 s of steady output: {spawns}"
        );
    }

    /// The episode rule: sustained sparse output (a test runner) speaks once,
    /// stays mute until the metric drains, and closes with one exhale.
    #[test]
    fn an_episode_is_one_pip_and_one_exhale() {
        let t0 = Instant::now();
        let mut e = OutputStreak::new(4);
        let cfg = cfg_on();
        // 6 s of output every 800 ms: never saturating, the cadence that
        // would drip under a per-spawn cue.
        let (mut cues, _) = run(
            &mut e,
            t0,
            120,
            Duration::from_millis(50),
            &cfg,
            |e, now, i| {
                if i % 16 == 0 {
                    e.note_output(u64::from(i) + 1, &span((i % 12) as u16), now, false);
                }
            },
        );
        // …then silence, long enough to drain through EPISODE_LOW.
        let (tail, _) = run(
            &mut e,
            t0 + Duration::from_secs(6),
            200,
            Duration::from_millis(50),
            &cfg,
            |_, _, _| {},
        );
        cues.extend(tail);
        let pips = cues
            .iter()
            .filter(|c| c.sound == StreakSound::Shimmer)
            .count();
        let exhales = cues
            .iter()
            .filter(|c| c.sound == StreakSound::Settle)
            .count();
        assert_eq!(pips, 1, "one pip opens the episode: {pips}");
        assert_eq!(exhales, 1, "one exhale closes it: {exhales}");
    }

    /// Idle to zero: after the drain the engine is inactive, owes no deadline,
    /// and draws nothing.
    #[test]
    fn idle_settles_to_fingerprint_zero_and_no_deadline() {
        let t0 = Instant::now();
        let mut e = OutputStreak::new(6);
        let cfg = StreakConfig {
            idle_secs: 2.0,
            ..cfg_on()
        };
        e.note_output(1, &span(5), t0, false);
        e.note_output(2, &span(5), t0 + Duration::from_millis(300), false);
        run(
            &mut e,
            t0,
            40,
            Duration::from_millis(50),
            &cfg,
            |_, _, _| {},
        );
        let late = t0 + Duration::from_secs(30);
        let mut out = Vec::new();
        let f = e.tick(late, geom(), &cfg, &mut out);
        assert_eq!(f.fp, 0);
        assert!(out.is_empty());
        assert!(!e.is_active());
        assert_eq!(e.next_change_deadline(late, cfg.idle_secs), None);
    }

    /// An idle cutoff earlier than momentum's Settle: the parked engine wakes at it, consumes it
    /// on the boundary, then arms only the later Settle.
    #[test]
    fn parked_episode_wakes_at_idle_cutoff_before_later_settle() {
        let t0 = Instant::now();
        let cfg = StreakConfig {
            idle_secs: 2.0,
            ..cfg_on()
        };
        let mut e = OutputStreak::new(71);
        assert!(!e.note_output(1, &span(4), t0, false));
        let spawned = t0 + Duration::from_millis(300);
        assert!(e.note_output(2, &span(4), spawned, false));
        let mut out = Vec::new();
        assert_eq!(
            e.tick(spawned, geom(), &cfg, &mut out)
                .cue
                .map(|cue| cue.sound),
            Some(StreakSound::Shimmer)
        );
        e.momentum.set_value(spawned, 1.0);
        for step in 1..=4 {
            out.clear();
            e.tick(
                spawned + Duration::from_millis(250 * step),
                geom(),
                &cfg,
                &mut out,
            );
        }
        let parked = spawned + Duration::from_secs(1);
        assert!(!e.is_active());
        let idle = spawned + Duration::from_secs(2);
        assert_eq!(e.next_change_deadline(parked, cfg.idle_secs), Some(idle));

        out.clear();
        let drained = e.tick(idle, geom(), &cfg, &mut out);
        assert_eq!(drained.fp, 0);
        assert!(
            drained.cue.is_none(),
            "momentum still owes its later Settle"
        );
        let settle = e
            .next_change_deadline(idle, cfg.idle_secs)
            .expect("the episode still owes its analytic Settle");
        assert!(
            settle > idle,
            "the consumed idle cutoff cannot spin overdue"
        );
    }

    #[test]
    fn fractional_idle_deadline_is_consumed_at_the_exact_promised_boundary() {
        let t0 = Instant::now();
        let cfg = StreakConfig {
            idle_secs: 0.1,
            ..cfg_on()
        };
        let mut e = OutputStreak::new(73);
        assert!(!e.note_output(1, &span(4), t0, false));
        let spawned = t0 + Duration::from_millis(300);
        assert!(e.note_output(2, &span(4), spawned, false));
        let mut out = Vec::new();
        e.tick(spawned, geom(), &cfg, &mut out);
        // Model the natural parked interval without depending on this seed's
        // randomized comet duration; the episode and momentum stay live.
        e.comets = Default::default();
        e.drawing = false;

        let exact = spawned + Duration::from_secs_f32(cfg.idle_secs);
        assert_eq!(e.next_change_deadline(spawned, cfg.idle_secs), Some(exact));
        out.clear();
        e.tick(exact, geom(), &cfg, &mut out);
        assert!(e.last_output.is_none(), "the exact cutoff was consumed");
        assert_ne!(
            e.next_change_deadline(exact, cfg.idle_secs),
            Some(exact),
            "float rounding cannot manufacture an overdue wake loop"
        );
    }

    #[test]
    fn huge_finite_idle_value_drains_immediately_without_panicking() {
        let last = Instant::now();
        let cfg = StreakConfig {
            idle_secs: f32::MAX,
            ..cfg_on()
        };
        let mut e = OutputStreak::new(79);
        e.in_episode = true;
        e.last_output = Some(last);

        assert_eq!(idle_duration(f32::MAX), None);
        assert_eq!(e.pending_idle_deadline(f32::MAX), Some(last));
        assert!(e.idle_drained(last, &cfg));
    }

    /// Amplitude is clamped: no configuration can put more than [`ALPHA_CLAMP`]
    /// of light on a cell, and a light theme is halved again.
    #[test]
    fn no_configuration_can_exceed_the_amplitude_ceiling() {
        let t0 = Instant::now();
        let ceiling = (ALPHA_CLAMP * 255.0).round() as u32;
        for dark in [true, false] {
            let cfg = StreakConfig {
                intensity: 9.9,
                dark_theme: dark,
                theme_bg: if dark { 0x0000_0000 } else { 0x00FF_FFFF },
                ..cfg_on()
            };
            let mut e = OutputStreak::new(8);
            let mut peak = 0;
            for i in 0..40u64 {
                let now = t0 + Duration::from_millis(i * 40);
                e.note_output(i + 1, &span((i % 6) as u16), now, false);
                let mut out = Vec::new();
                e.tick(now, geom(), &cfg, &mut out);
                for q in &out {
                    for sh in [16, 8, 0] {
                        peak = peak.max((q.color >> sh) & 0xff);
                    }
                }
            }
            assert!(
                peak <= ceiling,
                "dark={dark}: peak channel {peak} exceeds the {ceiling} ceiling"
            );
        }
    }

    /// Polarity: a light theme emits source-over shadow, a dark theme additive
    /// light (additive light would be invisible on Solarized Light).
    #[test]
    fn light_themes_get_shadow_not_invisible_additive_light() {
        let t0 = Instant::now();
        let sample = |dark: bool| {
            let cfg = StreakConfig {
                dark_theme: dark,
                theme_bg: if dark { 0x0000_0000 } else { 0x00FD_F6E3 },
                ..cfg_on()
            };
            let mut e = OutputStreak::new(9);
            let mut modes = Vec::new();
            for i in 0..30u64 {
                let now = t0 + Duration::from_millis(i * 40);
                e.note_output(i + 1, &span((i % 6) as u16), now, false);
                let mut out = Vec::new();
                e.tick(now, geom(), &cfg, &mut out);
                modes.extend(out.iter().map(|q| q.alpha));
            }
            modes
        };
        let dark = sample(true);
        let light = sample(false);
        assert!(!dark.is_empty() && !light.is_empty(), "both themes drew");
        assert!(
            dark.iter().all(|&a| a == 0),
            "a dark theme's streak is additive light"
        );
        assert!(
            light.iter().any(|&a| a > 0),
            "a light theme's streak composites source-over so it can darken"
        );
    }

    /// The sweep is gapless: every comet frame's cells form one contiguous run on
    /// one row.
    #[test]
    fn a_comet_paints_one_contiguous_run() {
        let t0 = Instant::now();
        let cfg = cfg_on();
        let mut e = OutputStreak::new(21);
        e.note_output(1, &span(4), t0, false);
        e.note_output(2, &span(4), t0 + Duration::from_millis(50), false);
        for i in 0..8u64 {
            let now = t0 + Duration::from_millis(50 + i * 30);
            let mut out = Vec::new();
            e.tick(now, geom(), &cfg, &mut out);
            if out.is_empty() {
                continue;
            }
            let mut xs: Vec<u16> = out.iter().map(|q| q.x / 8).collect();
            xs.sort_unstable();
            xs.dedup();
            for pair in xs.windows(2) {
                assert_eq!(pair[1] - pair[0], 1, "the sweep must be gapless: {xs:?}");
            }
        }
    }
}
