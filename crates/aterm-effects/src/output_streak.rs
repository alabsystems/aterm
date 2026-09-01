// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PRISM WAKE — the terminal answers PROGRAM OUTPUT with a comet, softly, in the
//! theme's own colours (owner: *"add some kind of small sound effect and rainbow
//! streak when in terminal there is output"*, then the ruling that outranks it:
//! *"it should't be overwhelming but I want some per-theme effect and soft
//! sound"*).
//!
//! Design record: `docs/DESIGN-output-streak-2026-08-30.md`.
//!
//! ## The rule
//!
//! > Output earns light the way typing earns momentum: metered,
//! > echo-discounted, and always decaying to zero. A burst gets one comet and
//! > one soft note. A flood gets one silent ribbon. **No configuration animates
//! > forever.**
//!
//! The corollary, and it is the whole ceiling ruling: every governor below is an
//! ENGINE-SIDE CLAMP, not host policy. An embedder, a config typo, or a future
//! theme cannot make this loud, fast, or flashy, because the limits live in this
//! state machine the same way the WCAG posture already lives in the crate
//! contract (see the [crate] docs).
//!
//! ## The licence — what counts as "output"
//!
//! The host feeds [`OutputStreak::note_output`] a content-generation counter and
//! the freshly damaged row spans it read at the deco-rescan seam. Four gates,
//! all here rather than at the call site, decide whether that mints light:
//!
//! * **First observation BASELINES.** A cold engine (or one whose counter went
//!   backwards — a session or tab switch) records the counter and mints nothing,
//!   so attaching to a busy session never fires a burst of comets for history.
//! * **ECHO DISCOUNT.** A delta within [`ECHO_DISCOUNT_MS`] of a keystroke, or
//!   one arriving while the host reports `input_hot`, is the user's own typing
//!   coming back and mints NOTHING. The `input_hot` term is load-bearing, not
//!   belt-and-braces: paste does not stamp a keystroke, and without it a large
//!   paste into a quiet prompt reads as program output.
//! * **EMPTY DAMAGE MINTS NOTHING.** The host passes only spans that still carry
//!   ink. `\x1b[2K` over an already-blank line advances the content counter, and
//!   without this clause an idle TUI redrawing its own chrome would shimmer
//!   forever.
//! * **Output never feeds the TYPING metric.** Output momentum is a separate
//!   instance of the one [`TypingMomentum`] law; output must not read as
//!   typing-earned drama anywhere else in the family.
//!
//! ## Why it cannot overwhelm
//!
//! Six stacked structural governors — the ceiling ruling, made arithmetic:
//!
//! 1. **[`TypingMomentum`], reused verbatim.** Rate-normalized accrual means a
//!    firehose cannot accumulate more drama than a trickle; its `low_crossing`
//!    gives the episode edges analytically, at any frame rate.
//! 2. **[`SPAWN_MIN_GAP_MS`] — the ignition floor.** At 700 ms this is ≤1.43
//!    spawns/second, inside the ≤2/s WCAG rolling-ignition budget the word-nova
//!    limiter keeps for the same window; because a sound cue is recorded only on
//!    a spawn edge, ONE clamp bounds eye and ear together.
//! 3. **[`MAX_COMETS`] + per-row dedup.** A row already carrying a comet cannot
//!    respawn until it retires.
//! 4. **Flood coalescing.** Past [`SATURATED`] momentum, per-row spawning STOPS
//!    and the engine degrades to a single constant-cost ribbon: `cat bigfile` is
//!    one ribbon, one pip, then quiet.
//! 5. **Amplitude clamps.** [`ALPHA_DEFAULT`] with [`ALPHA_CLAMP`] as the hard
//!    ceiling, halved again on light grounds, so text is never harder to read
//!    than with the effect off.
//! 6. **The episode sound law.** One [`StreakSound::Shimmer`] on the quiet→active
//!    crossing, then MUTE until momentum drains through [`EPISODE_LOW`], which
//!    re-arms the voice and fires one [`StreakSound::Settle`]. A build log yields
//!    one pip and one exhale, total.
//!
//! Decay is guaranteed from three directions: the momentum τ, the ribbon's
//! [`RIBBON_LINGER_MS`], and the mandatory idle drain
//! ([`StreakConfig::idle_secs`], clamped by the host to a finite band).
//!
//! ## Contract
//!
//! Clockless (every entry point takes an injected `now`), overlay-only (output is
//! premultiplied [`GlowQuad`]s destined for the grid-anchored `nova_add` channel
//! — the grid is never touched), and empty-is-off: disabled, reduced-motion, or
//! settled ticks reset the state machine, emit no quads, record no cue, and
//! return fingerprint `0`.

use aterm_render::{GlowQuad, premul_rgb};
use aterm_time::Instant;
use std::time::Duration;

use crate::cursor_glow::Geom;
use crate::effect_util::{lerp_rgb, push_grid_quad};
use crate::genome;
use crate::spectrum::{clear_light_of_cyan, spectrum, spectrum_snap_index};
use crate::trail_sweep::row_sweep_cells;
use crate::typing_momentum::TypingMomentum;

/// A content-counter delta this close behind a keystroke is the user's own echo
/// and mints nothing. Matches the rain weather machine's discount so the two
/// output-activity consumers cannot disagree about what "your own typing" is.
pub const ECHO_DISCOUNT_MS: u64 = 250;

/// The ignition floor between two comet spawns. 700 ms is ≤1.43 spawns/second —
/// inside the ≤2/s rolling WCAG ignition budget — and because a sound cue is
/// recorded only on a spawn edge, this one clamp limits the eye and the ear
/// together (the one-event law).
pub const SPAWN_MIN_GAP_MS: u64 = 700;

/// The voice's own floor, on top of the episode law: even across episode
/// boundaries two pips can never fall closer than this. A denied fire does NOT
/// re-arm — it is simply not heard.
pub const SOUND_MIN_GAP_MS: u64 = 1_500;

/// Comet sweep duration band. The floor is above the 350 ms WCAG twinkle floor
/// with room to spare, and the whole envelope is MONOTONE — a comet fades once,
/// never oscillates, so no comet has a flash rate at all.
pub const COMET_MS_MIN: u32 = 420;
/// Upper end of the sweep band (see [`COMET_MS_MIN`]).
pub const COMET_MS_MAX: u32 = 900;

/// Tail length band in cells, before the host's own `tail` clamp.
pub const TAIL_MIN: u16 = 4;
/// Upper end of the tail band (see [`TAIL_MIN`]).
pub const TAIL_MAX: u16 = 14;

/// Peak coverage on a dark ground at full intensity — the SHIPPING amplitude.
/// Deliberately a third under [`ALPHA_CLAMP`]: the clamp is the ceiling a config
/// may reach for, this is what the effect actually looks like.
pub const ALPHA_DEFAULT: f32 = 0.10;

/// The hard amplitude ceiling. No configuration, theme derivation, or embedder
/// setter may put more light than this on one cell.
pub const ALPHA_CLAMP: f32 = 0.18;

/// Momentum at or above which per-row spawning STOPS and the single ribbon takes
/// over — the flood's constant-cost degradation.
pub const SATURATED: f32 = 0.72;

/// Momentum below which an episode is over: the voice re-arms and one
/// [`StreakSound::Settle`] is heard. Read through
/// [`TypingMomentum::low_crossing`] so the deadline is exact at any frame rate.
pub const EPISODE_LOW: f32 = 0.10;

/// How long the ribbon lingers after the last licensed output before retiring.
pub const RIBBON_LINGER_MS: u64 = 600;

/// Hue span across one comet, in spectrum turns. Compressed on purpose: a fifth
/// of the arc reads as an IRIDESCENT SHEEN, where a full turn reads as a rainbow
/// bar laid over the user's text.
pub const HUE_SPAN: f32 = 0.22;

/// Ribbon hue drift, turns/second — three orders under the 3.2 Hz
/// photosensitivity invariant, and a drift rather than a cycle.
pub const RIBBON_DRIFT: f32 = 0.25;

/// The most comets that may be resident at once (the host clamps its own
/// `max_streaks` into `1..=MAX_COMETS`).
pub const MAX_COMETS: usize = 4;

/// Largest frame step the sweep clock will honour, in seconds: a window that
/// slept must not fling every resident comet to its end in one tick.
const MAX_DT_S: f32 = 0.25;

/// What the host must play, recorded ONLY on an edge that also minted light.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreakSound {
    /// The episode's one soft pip, on the quiet→active crossing.
    Shimmer,
    /// The episode's closing exhale, quieter still, on the drain to rest.
    Settle,
}

/// One sound cue: what to play and where it happened.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StreakCue {
    /// Which voice (see [`StreakSound`]).
    pub sound: StreakSound,
    /// Equal-power pan position `-1..=1`, from the span centre column.
    pub pan: f32,
    /// The comet's spectrum entry phase `0..1` — the host maps it to a lattice
    /// degree so the theme audibly shades the pip.
    pub hue: f32,
}

/// Resolved per-frame inputs. `Copy` so the host reads it out before borrowing
/// engine state, exactly like the cursor emitters' configs.
#[derive(Clone, Copy, Debug)]
pub struct StreakConfig {
    /// Master on/off. Default ON at the config layer (owner ruling 2026-08-31);
    /// `false` here is fully inert, byte-identical to a build without the
    /// effect.
    pub enabled: bool,
    /// Amplitude `0..=1` — the reduced-motion / load-shed scale the host folds
    /// in. `0` ⇒ fully inert, reset not dimmed.
    pub intensity: f32,
    /// Tail length in cells, clamped here into [`TAIL_MIN`]`..=`[`TAIL_MAX`].
    pub tail: u16,
    /// Resident comet cap, clamped here into `1..=`[`MAX_COMETS`].
    pub max_streaks: u8,
    /// Mandatory idle drain: with no licensed output for this long, everything
    /// retires. The host clamps the band; a non-finite or non-positive value is
    /// treated as "drain immediately" rather than "animate forever".
    pub idle_secs: f32,
    /// Whether the resolved theme background is dark. Selects the POLARITY:
    /// additive spectrum light on dark grounds, a tinted shadow-shimmer on
    /// light ones (additive light cannot darken a pale ground, so on a light
    /// theme it would simply not exist).
    pub dark_theme: bool,
    /// The resolved theme background `0x00RRGGBB` — the real ground the cyan law
    /// is judged against, and the pole the light-theme shimmer is mixed toward.
    pub theme_bg: u32,
    /// The resolved theme foreground `0x00RRGGBB` — the legibility grounding mix
    /// on light themes.
    pub theme_fg: u32,
    /// The theme's cursor colour (foreground is the host's documented fallback):
    /// its nearest spectrum stop seeds where on the arc a comet's head ENTERS,
    /// which is the whole per-theme character axis. Pure position selection on
    /// THE ONE SPECTRUM — never a second ramp.
    pub theme_cursor: u32,
    /// Whether the pip may be recorded at all (the host's own sound ladder —
    /// focus, master switch, volume — still applies on top).
    pub sound: bool,
}

impl Default for StreakConfig {
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

/// The resident flood ribbon (see governor 4 in the module docs).
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

/// PRISM WAKE's state machine. ONE PER PANE — see the module docs for the law.
///
/// Not one per window, and the difference is the design rather than a detail:
/// `crates/aterm-gui/src/lib.rs` holds `output_streak_panes: BTreeMap<u64,
/// OutputStreak>` keyed by session id, and says why at the field — "a split
/// shows several live terminals at once and each carries its own arrival clock,
/// so one engine per window would blend their streams into nonsense (and let a
/// chatty pane spend the quiet pane's spawn budget)". The sibling
/// `output_streak: Option<Box<OutputStreak>>` beside it is the SINGLE-PANE
/// present, not a second scope.
///
/// The one budget that IS window-wide is the ≤2/s WCAG rolling ignition
/// allowance this engine spends from, and it does not live here: it belongs to
/// the word-nova's `FlashLimiterWindow`, which is a registered, machine-checked
/// window-scope claim (`aterm_census::scope_census`, id `flash-limiter`).
/// Saying "one per window" here claimed that budget's scope for a per-pane
/// state machine, which is the exact confusion OB-17 exists to catch.
#[derive(Default)]
pub struct OutputStreak {
    /// The output metric — [`TypingMomentum`]'s law, a separate instance, fed
    /// only by licensed output.
    momentum: TypingMomentum,
    /// Last ARRIVAL TOKEN seen; `None` ⇒ not yet baselined. Any CHANGE is an
    /// arrival — deliberately not "any increase". A monotone content counter
    /// and a content hash both satisfy that, which is what lets the composed
    /// hosts (whose snapshots carry only a synthetic, always-advancing frame
    /// seq) share this law with the hosts that have a real content clock.
    last_token: Option<u64>,
    /// Last keystroke stamp, for the echo discount.
    last_key: Option<Instant>,
    /// Newest licensed span awaiting a spawn decision.
    pending: Option<Pending>,
    /// Stamp of the last licensed output (drives the ribbon and the idle drain).
    last_output: Option<Instant>,
    /// Last spawn stamp — the [`SPAWN_MIN_GAP_MS`] floor.
    last_spawn: Option<Instant>,
    /// Last cue stamp — the [`SOUND_MIN_GAP_MS`] floor.
    last_sound: Option<Instant>,
    /// Whether the voice has already spoken this episode (the mute half of the
    /// episode law).
    in_episode: bool,
    /// Resident comets, oldest slots first; `None` is a free slot.
    comets: [Option<Comet>; MAX_COMETS],
    /// The flood ribbon, when saturated.
    ribbon: Option<Ribbon>,
    /// Frame clock for the sweep integrator.
    last_tick: Option<Instant>,
    /// Per-window variation seed, mixed into every comet's genome key.
    seed: u64,
    /// Latched: something was drawable at the last tick (so [`Self::is_active`]
    /// answers without a clock, the [`crate::cursor_rainbow`] idiom).
    drawing: bool,
}

impl OutputStreak {
    /// A fresh engine with a caller-chosen variation `seed` (the host passes a
    /// per-window value; equal seeds replay equal art, which is what the
    /// determinism tests lean on).
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            seed,
            ..Self::default()
        }
    }

    /// Whether the host must keep arming frames. Answers from latched scalars —
    /// no clock, no allocation — so a settled engine costs one bool read.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.drawing
    }

    /// The next instant at which something visibly changes, or `None` when the
    /// engine is at rest. The host arms its effect lane on this; `None` is what
    /// lets the window park to a pure wait.
    #[must_use]
    pub fn next_change_deadline(&self, now: Instant) -> Option<Instant> {
        if !self.drawing {
            // Even with nothing on glass, a live episode still owes its Settle
            // exhale at the analytic drain crossing.
            return self.pending_settle_deadline();
        }
        Some(now)
    }

    /// The instant the current episode drains, if one is open and owed a
    /// [`StreakSound::Settle`].
    fn pending_settle_deadline(&self) -> Option<Instant> {
        if !self.in_episode {
            return None;
        }
        self.momentum.low_crossing(EPISODE_LOW)
    }

    /// Back to rest — everything retired, the voice closed, the counter
    /// un-baselined so the next observation baselines instead of minting.
    pub fn reset(&mut self) {
        let seed = self.seed;
        *self = Self {
            seed,
            ..Self::default()
        };
    }

    /// FORGET THE ARRIVAL TOKEN, keeping everything else. The next observation
    /// then BASELINES instead of minting — the session/tab-switch seam.
    ///
    /// Under the token law any change is an arrival, so a host that re-points
    /// one engine at a different terminal would otherwise answer the swap
    /// itself with a comet. Hosts that key an engine per session get this
    /// structurally (a different session is a different engine) and never need
    /// to call it; the single-engine window path does.
    pub fn rebase(&mut self) {
        self.last_token = None;
    }

    /// One keystroke at `now`, for the echo discount. The host forwards its own
    /// key stamps; nothing else about typing reaches this engine.
    pub fn note_keystroke(&mut self, now: Instant) {
        self.last_key = Some(now);
    }

    /// One frame-time output observation: the terminal's content-generation
    /// counter, the freshly damaged INK-BEARING row spans (the host filters
    /// blank damage — see "empty damage mints nothing" in the module docs), and
    /// whether the host currently has input in flight.
    ///
    /// Returns `true` when the observation was LICENSED (it credited momentum
    /// and armed a spawn) — the tests' non-vacuity handle, and a cheap way for a
    /// host to assert its own licence facts.
    pub fn note_output(
        &mut self,
        token: u64,
        spans: &[(u16, u16, u16)],
        now: Instant,
        input_hot: bool,
    ) -> bool {
        // FIRST OBSERVATION BASELINES — a fresh engine (or one that just
        // rebased across a session or tab switch) records what it sees and
        // mints nothing, so attaching to a busy session never answers history
        // with a burst. After that, any CHANGE is an arrival and an unchanged
        // token is no arrival at all: a screen being repainted byte-identically
        // has produced no output, however busy the byte stream looks.
        let Some(prev) = self.last_token else {
            self.last_token = Some(token);
            return false;
        };
        self.last_token = Some(token);
        if token == prev {
            return false;
        }
        // EMPTY DAMAGE MINTS NOTHING.
        let Some(&(row, left, right)) = spans.last() else {
            return false;
        };
        // ECHO DISCOUNT — the user's own typing coming back.
        if input_hot || self.within_echo_window(now) {
            return false;
        }
        self.momentum.advance(now);
        self.last_output = Some(now);
        self.pending = Some(Pending { row, left, right });
        true
    }

    /// [`Self::note_output`] with THE ANCHOR derived from a frame's cells — the
    /// one authority every host shares, because four different render paths
    /// (the single-pane present, the composed present, and both capture
    /// splices) need the identical answer and a fifth copy of this reasoning is
    /// a fifth chance to get it subtly different.
    ///
    /// The freshly written row is the cursor's OWN while it is mid-line, and
    /// the row above it otherwise: program output ends its lines with a
    /// newline, which parks the cursor at column 0 of a row with no ink in it
    /// yet, so anchoring naively on the live cursor would licence nothing for
    /// the overwhelming majority of real output. The span is that row's true
    /// ink extent, which is also how EMPTY DAMAGE MINTS NOTHING is enforced
    /// here: a row carrying no ink yields no span at all.
    ///
    /// Returns whether the observation was LICENSED (see [`Self::note_output`]).
    pub fn note_output_cells(
        &mut self,
        cells: &[Vec<aterm_core::terminal::RenderCell>],
        cursor_row: usize,
        cursor_col: usize,
        rows: usize,
        now: Instant,
        input_hot: bool,
    ) -> bool {
        let anchor_row = if cursor_col > 0 {
            cursor_row
        } else {
            cursor_row.saturating_sub(1)
        };
        let span = cells
            .get(anchor_row)
            .and_then(|row| row.iter().rposition(|c| c.ch != ' '))
            .filter(|_| anchor_row < rows)
            .map(|end| {
                [(
                    anchor_row as u16,
                    0u16,
                    u16::try_from(end).unwrap_or(u16::MAX),
                )]
            });
        // THE ARRIVAL TOKEN, derived from the very row the comet would light:
        // its index folded with its glyphs. This is a CONTENT clock by
        // construction — it moves when what is written moves and stands still
        // when the screen is merely repainted — which is what the composed
        // hosts need, since their snapshots carry only a synthetic frame seq
        // that advances on every single frame and would otherwise licence a
        // comet forever (a split that never settles is a split that never
        // returns to 0% idle).
        const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut token = FNV_OFFSET;
        let mut fold = |v: u64| {
            token ^= v;
            token = token.wrapping_mul(FNV_PRIME);
        };
        fold(anchor_row as u64);
        if let Some(row) = cells.get(anchor_row) {
            for c in row {
                fold(u64::from(u32::from(c.ch)));
            }
        }
        self.note_output(
            token,
            span.as_ref().map_or(&[][..], |s| &s[..]),
            now,
            input_hot,
        )
    }

    /// Whether `now` sits inside the echo shadow of the last keystroke.
    fn within_echo_window(&self, now: Instant) -> bool {
        self.last_key.is_some_and(|k| {
            now.saturating_duration_since(k) <= Duration::from_millis(ECHO_DISCOUNT_MS)
        })
    }

    /// Advance one frame at `now` and append this frame's light to `out`.
    ///
    /// Pure in the crate's sense: no wall clock, no allocation beyond `out`'s
    /// own growth, and byte-identical replays for identical `(now, cfg)`
    /// streams.
    pub fn tick(
        &mut self,
        now: Instant,
        geom: Geom,
        cfg: &StreakConfig,
        out: &mut Vec<GlowQuad>,
    ) -> StreakFrame {
        // FULLY INERT: off, reduced to zero amplitude, or degenerate geometry.
        // A reset, not a dimming — the pin-not-ease rule for reduced motion.
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

        // MANDATORY IDLE DRAIN — no configuration animates forever.
        if self.idle_drained(now, cfg) {
            let cue = self.close_episode_cue(now, cfg);
            self.comets = Default::default();
            self.ribbon = None;
            self.pending = None;
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
    fn idle_drained(&self, now: Instant, cfg: &StreakConfig) -> bool {
        let Some(last) = self.last_output else {
            // Nothing has ever been licensed: only a live comet keeps us awake.
            return self.comets.iter().all(Option::is_none) && self.ribbon.is_none();
        };
        if !cfg.idle_secs.is_finite() || cfg.idle_secs <= 0.0 {
            return true;
        }
        now.saturating_duration_since(last).as_secs_f32() > cfg.idle_secs
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

        // GOVERNOR 4 — FLOOD COALESCING. Past saturation nothing new spawns; the
        // single ribbon carries the activity at constant cost, mutely.
        if m >= SATURATED {
            self.refresh_ribbon(pending, cfg);
            return None;
        }

        // GOVERNOR 2 — the ignition floor (and, because a cue rides only a spawn
        // edge, the ear's upstream limit too).
        if let Some(last) = self.last_spawn
            && now.saturating_duration_since(last) < Duration::from_millis(SPAWN_MIN_GAP_MS)
        {
            return None;
        }
        // GOVERNOR 3 — per-row dedup: a row already carrying a comet waits.
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

        self.comets[slot] = Some(Comet {
            row: pending.row,
            from_col: i32::from(pending.left),
            to_col: i32::from(pending.right).min(geom.cols as i32 - 1),
            progress: 0.0,
            dur_s: dur_ms as f32 / 1000.0,
            tail: tail.min(cfg.tail.clamp(TAIL_MIN, TAIL_MAX)),
            hue,
        });
        self.last_spawn = Some(now);

        self.open_episode_cue(now, cfg, pending, geom, hue)
    }

    /// The first free comet slot under the host's resident cap.
    fn free_slot(&self, cfg: &StreakConfig) -> Option<usize> {
        let cap = (cfg.max_streaks as usize).clamp(1, MAX_COMETS);
        self.comets.iter().take(cap).position(Option::is_none)
    }

    /// GOVERNOR 6, opening half: one pip on the quiet→active crossing, then
    /// mute. Honours the voice's own [`SOUND_MIN_GAP_MS`] floor; a denied fire
    /// is simply not heard and does not re-arm.
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

    /// GOVERNOR 6, closing half: when momentum has drained through
    /// [`EPISODE_LOW`] the voice re-arms and one exhale is heard.
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

    /// PER-THEME CHARACTER, axis 2: the theme's cursor colour snapped to its
    /// nearest named spectrum stop decides where on the arc this comet's head
    /// ENTERS — Nord enters through the blue window, Gruvbox through amber,
    /// Dracula through violet. Pure position selection on THE ONE SPECTRUM; the
    /// per-comet genome only jitters within a stop's neighbourhood.
    fn hue_for(&self, pending: Pending, cfg: &StreakConfig) -> f32 {
        let stops = crate::spectrum::SPECTRUM_STOPS.max(1) as f32;
        let anchor = spectrum_snap_index(hue_position_of(cfg.theme_cursor)) as f32 / stops;
        let key = genome::mix(self.seed ^ u64::from(pending.row));
        let jitter = (genome::field(key, 0, 8) as f32 / 255.0 - 0.5) * (1.0 / stops);
        (anchor + jitter).rem_euclid(1.0)
    }

    /// Refresh (or open) the flood ribbon on the newest output row.
    fn refresh_ribbon(&mut self, pending: Pending, cfg: &StreakConfig) {
        let hue = match self.ribbon {
            Some(r) => r.hue,
            None => self.hue_for(pending, cfg),
        };
        self.ribbon = Some(Ribbon {
            row: pending.row,
            from_col: i32::from(pending.left),
            to_col: i32::from(pending.right),
            hue,
            since_output_s: 0.0,
        });
    }

    /// Advance every resident comet's sweep and retire the finished ones.
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
        let before = out.len();
        let mut fp: u64 = 0;
        let mut cells: Vec<(i32, i32)> = Vec::new();

        for c in self.comets.iter().flatten() {
            // MONOTONE fade — one decay across the life, never an oscillation,
            // so a comet has no flash rate to bound.
            let life = 1.0 - c.progress;
            let head = c.from_col as f32 + (c.to_col - c.from_col) as f32 * c.progress;
            let tail_start = head - f32::from(c.tail);
            cells.clear();
            row_sweep_cells(
                &mut cells,
                i32::from(c.row),
                tail_start.floor() as i32 - 1,
                head.round() as i32,
            );
            for &(row, col) in &cells {
                let u = ((head - col as f32) / f32::from(c.tail).max(1.0)).clamp(0.0, 1.0);
                // Bright head, fading tail; squared so the tail thins fast and
                // the comet reads as motion rather than a painted bar.
                let shape = (1.0 - u) * (1.0 - u);
                self.push_cell(out, geom, cfg, row, col, c.hue + u * HUE_SPAN, shape * life);
            }
            fp = fp
                .wrapping_mul(1_000_003)
                .wrapping_add((c.progress * 4096.0) as u64)
                .wrapping_add(u64::from(c.row) << 20);
        }

        if let Some(r) = self.ribbon {
            cells.clear();
            row_sweep_cells(&mut cells, i32::from(r.row), r.from_col - 1, r.to_col);
            // The ribbon is a FLAT, constant-cost texture: no head, no sweep,
            // one steady low-amplitude wash whose hue drifts far under the
            // photosensitivity invariant.
            let fade =
                1.0 - (r.since_output_s / (RIBBON_LINGER_MS as f32 / 1000.0)).clamp(0.0, 1.0) * 0.5;
            for &(row, col) in &cells {
                let t = r.hue + (col as f32 / geom.cols.max(1) as f32) * HUE_SPAN;
                self.push_cell(out, geom, cfg, row, col, t, 0.55 * fade);
            }
            fp = fp
                .wrapping_mul(31)
                .wrapping_add((r.hue * 4096.0) as u64)
                .wrapping_add(u64::from(r.row) << 8);
        }

        if out.len() == before {
            return 0;
        }
        // Never 0 while light is on glass — a settled engine returns 0 above.
        fp | 1
    }

    /// One cell of streak light, in the polarity the theme calls for.
    #[allow(
        clippy::too_many_arguments,
        reason = "cell coordinate + hue + shape + the frame's geometry/config; one internal \
                  call site per emitter arm (the `cursor_comet::emit_coma` precedent)"
    )]
    fn push_cell(
        &self,
        out: &mut Vec<GlowQuad>,
        geom: Geom,
        cfg: &StreakConfig,
        row: i32,
        col: i32,
        hue_t: f32,
        shape: f32,
    ) {
        if row < 0 || col < 0 || shape <= 0.0 {
            return;
        }
        let peak = (ALPHA_DEFAULT * cfg.intensity.clamp(0.0, 1.0)).min(ALPHA_CLAMP);
        // GOVERNOR 5 / axis 1: on a light ground the same light is halved AND
        // flipped to a tinted shadow — additive light can only brighten, so on a
        // pale theme the additive form would simply not exist.
        let amp = if cfg.dark_theme { peak } else { peak * 0.5 };
        let a = (amp * shape.clamp(0.0, 1.0) * 255.0).round() as u8;
        if a == 0 {
            return;
        }
        let base = spectrum(hue_t.rem_euclid(1.0));
        let x = col * geom.cw as i32;
        let y = row * geom.ch as i32;
        let (w, h) = (geom.cw as i32, geom.ch as i32);

        if cfg.dark_theme {
            let premul = clear_light_of_cyan(premul_rgb(base, a), 0, cfg.theme_bg);
            push_grid_quad(out, geom, x, y, w, h, premul, 0);
        } else {
            // SHADOW-SHIMMER: the spectrum hue pulled toward the near-black pole
            // and grounded toward the theme's own ink for legibility, composited
            // source-over so it can actually darken.
            let shade = lerp_rgb(lerp_rgb(base, 0x0000_0000, 0.55), cfg.theme_fg, 0.25);
            push_grid_quad(out, geom, x, y, w, h, premul_rgb(shade, a), a);
        }
    }
}

/// Equal-power pan position `-1..=1` from a span's centre column.
fn pan_of(pending: Pending, geom: Geom) -> f32 {
    let cols = geom.cols.max(1) as f32;
    let centre = (f32::from(pending.left) + f32::from(pending.right)) * 0.5;
    ((centre / cols) * 2.0 - 1.0).clamp(-1.0, 1.0)
}

/// Where a theme colour sits on the spectrum arc `0..1`, by hue angle. Used only
/// to pick an ENTRY STOP — the colour itself never enters the emission path, so
/// this is position selection, not a second ramp.
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

    /// FIRST OBSERVATION BASELINES: attaching to a session that already has a
    /// counter mints nothing, so a fresh window never bursts for history.
    #[test]
    fn first_observation_baselines_and_mints_nothing() {
        let t0 = Instant::now();
        let mut e = OutputStreak::new(7);
        assert!(
            !e.note_output(9_000, &span(3), t0, false),
            "the first observation only baselines"
        );
        assert!(
            e.note_output(9_001, &span(3), t0 + Duration::from_millis(50), false),
            "the second is licensed"
        );
    }

    /// THE TOKEN LAW: an UNCHANGED token is no arrival — a screen repainted
    /// byte-identically has produced no output however busy its byte stream is
    /// — and an explicit [`OutputStreak::rebase`] (the session/tab-switch seam)
    /// makes the next observation baseline instead of answering the swap.
    #[test]
    fn an_unchanged_token_is_no_arrival_and_rebase_baselines() {
        let t0 = Instant::now();
        let mut e = OutputStreak::new(7);
        e.note_output(500, &span(1), t0, false); // baseline
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

    /// THE ECHO DISCOUNT: your own typing — by keystroke stamp OR by the host's
    /// `input_hot` flag, which is the arm that covers PASTE — mints nothing.
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
        // PASTE: no keystroke stamp at all, but the host reports input in flight.
        let mut e2 = OutputStreak::new(1);
        e2.note_output(1, &span(2), t0, false);
        assert!(
            !e2.note_output(2, &span(2), t0 + Duration::from_secs(9), true),
            "input_hot alone must discount the echo — paste stamps no key"
        );
    }

    /// EMPTY DAMAGE MINTS NOTHING: a content-counter step carrying no
    /// ink-bearing span (the `\x1b[2K`-over-blank case) licences nothing, so an
    /// idle TUI redrawing its chrome never shimmers.
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

    /// REDUCED MOTION IS EXACT ZERO, NOT EASED: amplitude 0 resets the machine,
    /// emits no quads, records no cue, and reports fingerprint 0.
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

    /// OFF IS BYTE-IDENTICAL OFF: a disabled engine is inert no matter what it
    /// is fed.
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

    /// THE FLOOD: 10 k content steps a second yield ONE ribbon at constant cost,
    /// at most one pip, and never more than the resident cap of comets.
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
        // One ribbon row of ≤ cols cells, plus at most the few comets that were
        // minted before momentum saturated.
        assert!(
            peak <= 80 * (MAX_COMETS + 1),
            "the flood's quad cost is bounded: {peak}"
        );
    }

    /// THE SPAWN FLOOR bounds ignitions to the WCAG budget, and because a cue
    /// rides only a spawn edge it bounds the ear with the same clamp.
    #[test]
    fn the_spawn_gap_clamps_eye_and_ear_together() {
        let t0 = Instant::now();
        let mut e = OutputStreak::new(2);
        let cfg = cfg_on();
        // Output every 100 ms on DIFFERENT rows for 2 s: the per-row dedup can
        // not be what limits this, so the floor is doing the work.
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

    /// THE EPISODE LAW: sustained-but-sparse output (the test-runner case) does
    /// not drip one pip per spawn — it speaks once, then stays mute until the
    /// metric drains, and closes with a single exhale.
    #[test]
    fn an_episode_is_one_pip_and_one_exhale() {
        let t0 = Instant::now();
        let mut e = OutputStreak::new(4);
        let cfg = cfg_on();
        // 6 s of output every 800 ms — never enough to saturate, exactly the
        // cadence that would drip under a per-spawn cue.
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

    /// IDLE-TO-ZERO: after the drain the engine reports inactive, owes no
    /// deadline, and draws nothing — the host parks to a pure wait.
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
        assert_eq!(e.next_change_deadline(late), None);
    }

    /// CLOCKLESS DETERMINISM: the same seed and the same injected `(now, input)`
    /// script replay byte-identical quads and fingerprints.
    #[test]
    fn identical_scripts_replay_identical_light() {
        let t0 = Instant::now();
        let cfg = cfg_on();
        let play = || {
            let mut e = OutputStreak::new(42);
            let mut all = Vec::new();
            let mut fps = Vec::new();
            for i in 0..60u64 {
                let now = t0 + Duration::from_millis(i * 40);
                e.note_output(i + 1, &span((i % 7) as u16), now, false);
                let mut out = Vec::new();
                let f = e.tick(now, geom(), &cfg, &mut out);
                fps.push(f.fp);
                all.extend(out);
            }
            (all, fps)
        };
        let (a_quads, a_fps) = play();
        let (b_quads, b_fps) = play();
        assert_eq!(a_quads, b_quads);
        assert_eq!(a_fps, b_fps);
        assert!(!a_quads.is_empty(), "NON-VACUITY: the script drew light");
    }

    /// A DIFFERENT SEED IS A DIFFERENT COMET: the per-window variation actually
    /// varies (the twin that keeps the determinism test above from passing
    /// vacuously).
    #[test]
    fn a_different_seed_draws_differently() {
        let t0 = Instant::now();
        let cfg = cfg_on();
        let play = |seed: u64| {
            let mut e = OutputStreak::new(seed);
            let mut all = Vec::new();
            for i in 0..40u64 {
                let now = t0 + Duration::from_millis(i * 40);
                e.note_output(i + 1, &span((i % 5) as u16), now, false);
                let mut out = Vec::new();
                e.tick(now, geom(), &cfg, &mut out);
                all.extend(out);
            }
            all
        };
        assert_ne!(play(1), play(2));
    }

    /// AMPLITUDE IS CLAMPED: no configuration — including one that asks for far
    /// more than the ceiling — can put more than [`ALPHA_CLAMP`] of light on a
    /// cell, and a light theme is halved again.
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

    /// THE POLARITY FLIP: a light theme emits SOURCE-OVER shadow, a dark theme
    /// emits ADDITIVE light — the fix for "additive light cannot darken a pale
    /// ground", which would otherwise make the effect invisible on Solarized
    /// Light.
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

    /// PER-THEME CHARACTER IS REAL: two themes whose cursor colours sit in
    /// different spectrum windows enter the arc at different places.
    #[test]
    fn different_themes_enter_the_spectrum_at_different_places() {
        let nord = StreakConfig {
            theme_cursor: 0x0088_C0D0, // frost blue
            ..cfg_on()
        };
        let gruvbox = StreakConfig {
            theme_cursor: 0x00FA_BD2F, // amber
            ..cfg_on()
        };
        let e = OutputStreak::new(13);
        let p = Pending {
            row: 3,
            left: 0,
            right: 20,
        };
        let a = e.hue_for(p, &nord);
        let b = e.hue_for(p, &gruvbox);
        assert!(
            (a - b).abs() > 0.05,
            "the theme must move the entry phase: {a} vs {b}"
        );
    }

    /// THE SWEEP IS GAPLESS: every comet frame's cells form one contiguous run
    /// on one row — the continuity contract inherited from `trail_sweep`.
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
