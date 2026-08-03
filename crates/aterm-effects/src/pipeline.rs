// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The **web-embedder effects pipeline**: one struct that owns every effect
//! state machine (cursor aurora + comet trail + sparkle words + PHOSPHOR
//! matrix rain) plus their
//! resolved configs and per-frame scratch buffers, and applies them onto a
//! [`RenderInput`] — the exact wiring `aterm-gui`'s `redraw_window` performs,
//! packaged for hosts without an event loop (`aterm-wasm`, `aterm-gpu-web`).
//!
//! ## Animation-drive contract (host rAF, engine idle-to-zero)
//!
//! The pipeline is **clockless**: it never reads a wall clock. The host owns
//! time and advances it explicitly:
//!
//! 1. `advance(dt_ms)` — accumulate the host's frame delta (rAF timestamps).
//! 2. render — snapshot the grid, then [`EffectsPipeline::apply`] fills the
//!    overlay channels for the accumulated instant.
//! 3. [`EffectsPipeline::is_active`] plus
//!    [`EffectsPipeline::next_deadline_ms`] select the drive: display-rAF for
//!    frame-rate motion, an exact timer for rain's 12/30 Hz engine cadence,
//!    then 0% idle once every effect settles.
//!
//! Same `dt` stream + same PTY bytes ⇒ identical frames (the state machines
//! are deterministic; the only clock is the one the host advances).
//!
//! ## Defaults
//!
//! Everything is **OFF** at construction: `apply` clears the overlay channels
//! and returns fingerprint `0`, so output is byte-identical to a build without
//! effects (the native default is ON per the 2026-06-30 flips, but an embedder
//! opts in explicitly — its config surface owns the default).

use std::time::Duration;
use web_time::Instant;

use aterm_core::render::RenderInput;
use aterm_core::terminal::Terminal;
use aterm_render::{
    GlowQuad, InkCell, RainHalo, SpriteQuad, TrailCell, WordDecoration, theme_is_dark,
};

use crate::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle, RAINBOW_WAKE_PERSIST};
use crate::cursor_trail::{CursorTrail, TrailConfig, TypingCadence};
use crate::matrix_rain::{
    MatrixRain, RAIN_ALPHA_CAP, RAIN_ALPHA_FLOOR, RainConfig, RainHue, RainTickInput,
    RainVisibility,
};
use crate::word_decorations::{DecoConfig, EffectGeom, Resolved, SelView, WordDecorations};

/// Brighten a packed `0x00RRGGBB` by `f` (the native accent derivation).
fn brighten(c: u32, f: f32) -> u32 {
    let m = |sh: u32| ((((c >> sh) & 0xff) as f32) * f).min(255.0) as u32;
    (m(16) << 16) | (m(8) << 8) | m(0)
}

/// Whether a lexicon build-time conflict still applies under the resolved
/// scan options. The lexicon cannot see scan options, so its single-char-CJK
/// warning ("… requires `cjk_single_char = true` to scan" — the word
/// "requires" is load-bearing, see aterm-lexicon) fires unconditionally; once
/// the resolved config enables that opt-in the surface WILL scan and the
/// warning is satisfied, not a problem. Shared by BOTH resolvers (the native
/// `recompute_sparkle` log and [`EffectsPipeline::sparkle_lexicon_warnings`])
/// so they filter identically.
#[must_use]
pub fn lexicon_warning_applies(warning: &str, cjk_single_char: bool) -> bool {
    !(cjk_single_char && warning.contains("requires cjk_single_char = true"))
}

/// Owns the effect engines + scratch and applies them to a frame snapshot.
pub struct EffectsPipeline {
    /// Host-advanced monotonic offset from `t0` (the injected clock).
    clock: Duration,
    /// Epoch captured at construction; only differences ever matter.
    t0: Instant,
    focused: bool,

    glow: CursorGlow,
    glow_cfg: GlowConfig,
    /// `true` when the host left glow color unset, so the wake follows the
    /// frame's live OSC 12 / OSC 112 cursor instead of a pinned color.
    glow_color_from_cursor: bool,
    /// The accent follows the live cursor only when both color and accent were
    /// left automatic; an explicit accent always stays fixed.
    glow_accent_from_cursor: bool,
    trail: CursorTrail,
    trail_cfg: TrailConfig,
    /// Automatic opaque-comet color provenance, retained across frames so
    /// OSC 12 / OSC 112 and live theme changes can recolor an existing trail.
    trail_color_from_cursor: bool,
    /// Typing-cadence heat → comet ignition intensity. The embedder feeds it via
    /// [`Self::note_keystroke`]; `apply` reads it each frame to ignite the comet.
    typing_cadence: TypingCadence,
    /// Keystrokes noted since the last [`Self::advance`], replayed by `advance`
    /// evenly spaced across its `dt` — web hosts batch key events between rAF
    /// callbacks, and feeding them all at one injected instant collapses the
    /// inter-key gap to zero, over-igniting the cadence (a paste read as
    /// infinite-speed typing). Spreading across the elapsed frame delta is the
    /// honest reconstruction: the keys really did arrive within that window.
    /// Capped well past the cadence's saturation point (heat clamps at
    /// `knee_hi` ≈ 7 unit gains) so a paste flood bounds the replay work.
    pending_keys: u32,

    decos: WordDecorations,
    /// Compiled lexicon + resolved config; `None` while sparkle words are off.
    sparkle: Option<Resolved>,
    /// The knobs, kept across off/on toggles (native `DecoConfig` defaults).
    deco_cfg: DecoConfig,
    /// Raw `[sparkle_words.emphasis] enabled`: the v3 §6 resolve gate
    /// `enabled && (ink_enabled || has_custom_specs)` folds into
    /// `deco_cfg.emphasis` (a graphic-only custom word must scan with ink off).
    emphasis_enabled: bool,
    /// Languages whose ambiguous homograph entries are un-gated (native default
    /// `["en"]`); keys the lexicon build.
    languages: Vec<String>,
    /// User lexicon-override TOML (the native `lexicon` + `extra_words` channel).
    override_toml: Option<String>,
    /// v3 §6: the synthesized emphasis-class lexicon fragment for the custom
    /// spec words (appended to `override_toml` at lexicon build).
    custom_lexicon: String,

    /// Embedder window-chrome in px for window-absolute effect emissions —
    /// interior padding on every edge (`pad`) plus the top-only rise band
    /// (`head`), the same `[head][pad][grid][pad]` layout aterm-render composes.
    /// `0/0` (the default) keeps the identity Geom: origin 0, win == grid
    /// extents, byte-identical to the historical grid-relative contract.
    chrome_pad: u16,
    chrome_head: u16,
    /// Raw `Lexicon::conflicts()` captured at the last `rebuild_sparkle`
    /// (empty while sparkle is off). Surfaced — filtered by the resolved scan
    /// options — through [`Self::sparkle_lexicon_warnings`].
    lexicon_conflicts: Vec<String>,
    /// The parse diagnostic from the last [`Self::set_sparkle_custom_specs`]
    /// call whose TOML fragment was malformed (`None` after a clean parse or a
    /// clear). Rides [`Self::sparkle_lexicon_warnings`] — the same channel the
    /// lexicon-build diagnostics use — so an embedder that already surfaces
    /// those sees a bad custom-spec fragment too instead of a silent no-op.
    custom_spec_warning: Option<String>,

    trail_scratch: Vec<TrailCell>,
    glow_scratch: Vec<GlowQuad>,
    deco_scratch: Vec<WordDecoration>,
    ink_scratch: Vec<InkCell>,
    /// Free-overlay channel (overlay Phase 4: peeking cats + gaze dots ride
    /// here as FreeSprites; the legacy `cat_quads` split is retired).
    free_scratch: Vec<aterm_core::render::FreeSprite>,
    nova_scratch: Vec<GlowQuad>,

    /// PHOSPHOR rain engine; `None` while disabled so an off pipeline carries
    /// zero rain state (the zero-cost-off posture).
    rain: Option<Box<MatrixRain>>,
    /// The rain knobs, kept across off/on toggles (`enabled` mirrors
    /// `rain.is_some()` — the `deco_cfg` posture).
    rain_cfg: RainConfig,
    /// Last host-reported visibility, replayed onto a lazily-built engine.
    rain_visibility: RainVisibility,
    /// Last reduce-motion state, replayed onto a lazily-built engine.
    rain_reduced_motion: bool,
    /// Composer provenance survives while the lazily-owned rain engine is
    /// absent, then seeds a recreated engine on the next enable.
    rain_material_editing: bool,
    /// Whole milliseconds already fed to the engine's clockless accumulator;
    /// fractional rAF deltas keep accumulating in `clock` instead of
    /// truncating away per `advance` call.
    rain_clock_ms: u64,
    rain_scratch: Vec<SpriteQuad>,
    rain_add_scratch: Vec<RainHalo>,
}

impl Default for EffectsPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectsPipeline {
    /// Everything off; no lexicon is compiled until sparkle words are enabled.
    #[must_use]
    pub fn new() -> Self {
        Self {
            clock: Duration::ZERO,
            t0: Instant::now(),
            focused: true,
            glow: CursorGlow::default(),
            glow_cfg: GlowConfig {
                dark_theme: true,
                enabled: false,
                style: GlowStyle::Lumen,
                color: 0x0050_FA7B,
                accent: brighten(0x0050_FA7B, 1.5),
                duration: Duration::from_millis(260),
                length: 24,
                intensity: 0.7,
                radius: 0.6,
                ring: true,
                beam: true,
                head_dx: 0.5,
                pack: None,
                wake_persist_s: RAINBOW_WAKE_PERSIST,
            },
            glow_color_from_cursor: true,
            glow_accent_from_cursor: true,
            trail: CursorTrail::default(),
            trail_cfg: TrailConfig {
                enabled: false,
                duration: Duration::from_millis(260),
                max_len: 24,
                color: 0x0050_FA7B,
                intensity: 0.0,
                warmth: 0.0,
            },
            trail_color_from_cursor: true,
            typing_cadence: TypingCadence::default(),
            pending_keys: 0,
            decos: WordDecorations::default(),
            sparkle: None,
            // v3 §4: the web resolver honors the orca suspension from
            // construction (the native resolver ANDs the same const).
            deco_cfg: DecoConfig {
                orca: !crate::ORCA_SUSPENDED,
                ..DecoConfig::default()
            },
            emphasis_enabled: true,
            languages: vec!["en".to_string()],
            override_toml: None,
            custom_lexicon: String::new(),
            lexicon_conflicts: Vec::new(),
            custom_spec_warning: None,
            trail_scratch: Vec::new(),
            glow_scratch: Vec::new(),
            deco_scratch: Vec::new(),
            ink_scratch: Vec::new(),
            free_scratch: Vec::new(),
            nova_scratch: Vec::new(),
            rain: None,
            rain_cfg: RainConfig::default(),
            rain_visibility: RainVisibility::Focused,
            rain_reduced_motion: false,
            rain_material_editing: false,
            rain_clock_ms: 0,
            rain_scratch: Vec::new(),
            rain_add_scratch: Vec::new(),
            chrome_pad: 0,
            chrome_head: 0,
        }
    }

    /// Set the embedder's window-chrome (interior `pad` per edge + top-only
    /// `head` rise band, px) so effect emissions become window-absolute within
    /// the padded frame — the wasm twin of the native app's
    /// `effects_origin_win` derivation (no tab strip in an embedder). `0/0`
    /// restores the identity layout.
    pub fn set_chrome(&mut self, pad: u16, head: u16) {
        self.chrome_pad = pad;
        self.chrome_head = head;
    }

    /// The window-space geometry for a `rows`×`cols` grid of `cell_w`×`cell_h`
    /// cells under the configured chrome: grid-interior origin `(pad, pad +
    /// head)`, full frame `(grid + 2·pad, grid + 2·pad + head)`. With `0/0`
    /// chrome this is the identity law's Geom (origin 0, win == grid extents).
    fn chrome_geom(&self, rows: usize, cols: usize, cell_w: usize, cell_h: usize) -> Geom {
        let pad = self.chrome_pad;
        let head = self.chrome_head;
        let grid_w = u16::try_from(cols * cell_w).unwrap_or(u16::MAX);
        let grid_h = u16::try_from(rows * cell_h).unwrap_or(u16::MAX);
        Geom {
            cw: cell_w,
            ch: cell_h,
            rows,
            cols,
            origin_x: pad,
            origin_y: pad.saturating_add(head),
            win_w: grid_w.saturating_add(pad.saturating_mul(2)),
            win_h: grid_h
                .saturating_add(pad.saturating_mul(2))
                .saturating_add(head),
            head,
        }
    }

    /// Advance the injected clock by `dt_ms` (host rAF delta). Negative/NaN
    /// deltas are ignored; the clock is monotonic by construction.
    pub fn advance(&mut self, dt_ms: f64) {
        if dt_ms.is_finite() && dt_ms > 0.0 {
            let prev = self.clock;
            self.clock += Duration::from_secs_f64(dt_ms / 1000.0);
            // Replay the keystrokes noted since the last advance, spaced EVENLY
            // across the elapsed delta (the last one landing exactly at the new
            // now, so the freshest key ignites at full weight). Web hosts batch
            // key events between rAF callbacks; without the spread every batched
            // key would land at ONE instant — a zero inter-key gap the cadence
            // reads as infinite typing speed. The spread instants stay monotonic
            // (all within `(prev, clock]`), preserving the clockless contract:
            // same dt stream + same note_keystroke sequence ⇒ identical frames.
            let n = self.pending_keys;
            if n > 0 {
                self.pending_keys = 0;
                let dt = self.clock - prev;
                for i in 1..=n {
                    let at = self.t0 + prev + dt.mul_f64(f64::from(i) / f64::from(n));
                    self.typing_cadence.on_keystroke(at);
                }
            }
            // The rain engine accumulates whole host milliseconds; feed it the
            // delta of the pipeline clock's ms floor so sub-ms rAF fractions
            // carry over instead of truncating per call.
            if let Some(rain) = self.rain.as_deref_mut() {
                let total = self.clock.as_millis() as u64;
                rain.advance_ms(total.saturating_sub(self.rain_clock_ms));
                self.rain_clock_ms = total;
            }
        }
    }

    /// The current injected instant (`t0 + clock`).
    #[must_use]
    pub fn now(&self) -> Instant {
        self.t0 + self.clock
    }

    /// `true` while any effect is animating. Hosts must also consult
    /// [`Self::next_deadline_ms`]: rain is active at its own 12/30 Hz cadence,
    /// not at the display's 60/120 Hz rAF rate.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.trail.is_active()
            || self.glow.is_active()
            || self.decos.is_active(self.now())
            || self.rain.as_ref().is_some_and(|r| r.is_active())
    }

    /// Milliseconds until the next scheduled engine wake, or `None` when an
    /// active effect needs display-rAF cadence. Rain-only animation returns the
    /// exact remaining time to its next engine tick, and a glow whose only live
    /// term is the slowly-cooling FORGE EMBER (or a pending kill hint) returns
    /// its coarse poll instead of pinning rAF for the whole multi-second
    /// cooling tail — the native `next_change_deadline` collapse, mirrored.
    /// This lets web hosts stay input-immediate without repainting
    /// byte-identical terminal frames at monitor refresh. Settled one-shots
    /// arm no idle deadline.
    #[must_use]
    pub fn next_deadline_ms(&self) -> Option<f64> {
        let now = self.now();
        if self.trail.is_active() || self.decos.is_active(now) {
            return None;
        }
        // Glow: MOVING light needs display-rAF; the ember/kill-hint-only tail
        // paces at the engine's coarse poll. `frame_interval` ZERO makes the
        // live-motion arm collapse to `now`, so "deadline not in the future"
        // is exactly "needs rAF cadence".
        let glow_ms = match self.glow.next_change_deadline(now, Duration::ZERO) {
            Some(d) if d > now => Some(d.duration_since(now).as_secs_f64() * 1000.0),
            Some(_) => return None, // live motion → display-rAF
            None => None,           // glow settled
        };
        let rain_ms = self
            .rain
            .as_ref()
            .filter(|rain| rain.is_active())
            .map(|rain| rain.next_tick_in_ms() as f64);
        match (glow_ms, rain_ms) {
            (Some(g), Some(r)) => Some(g.min(r)),
            (g, r) => g.or(r),
        }
    }

    /// Focus gate for the sparse cat idle one-shots (`§5.6`): an unfocused
    /// embedder fires no blink events and freezes their fingerprints. Rain
    /// maps the bool onto its drain ladder (focused = full profile, unfocused
    /// = CALM cap + drain); the bool cannot express hidden — that arrives via
    /// [`Self::set_effects_visibility`].
    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        self.set_rain_visibility(if focused {
            RainVisibility::Focused
        } else {
            RainVisibility::VisibleUnfocused
        });
    }

    /// Tri-state pane visibility (design §11): `"focused"` (full profile),
    /// `"visible_unfocused"` (CALM cap + drain), `"hidden"` (hard drain even
    /// while a buggy host keeps calling `advance`). Unknown states parse as
    /// focused (the lenient-string precedent); `set_focused(bool)` survives
    /// for compat.
    pub fn set_effects_visibility(&mut self, state: &str) {
        let v = if state.eq_ignore_ascii_case("hidden") {
            RainVisibility::Hidden
        } else if state.eq_ignore_ascii_case("visible_unfocused")
            || state.eq_ignore_ascii_case("unfocused")
        {
            RainVisibility::VisibleUnfocused
        } else {
            RainVisibility::Focused
        };
        self.focused = v == RainVisibility::Focused;
        self.set_rain_visibility(v);
    }

    /// Record the visibility and replay it onto a live engine (a lazily-built
    /// one receives it at construction).
    fn set_rain_visibility(&mut self, v: RainVisibility) {
        self.rain_visibility = v;
        if let Some(rain) = self.rain.as_deref_mut() {
            rain.set_visibility(v);
        }
    }

    /// Register one keystroke for the comet ignition: fast, sustained calls heat
    /// the typing cadence so `apply` ignites the trail; a few (or slow) calls keep
    /// it gentle. Keystrokes are QUEUED and fed to the cadence by the next
    /// [`Self::advance`], spread evenly across its `dt` — so a host that batches
    /// several key events between rAF callbacks no longer collapses them onto one
    /// injected instant (a zero inter-key gap the cadence over-ignites on). A
    /// host that renders without advancing still ignites: `apply` flushes any
    /// stragglers at the current instant (the pre-queue behavior). This also
    /// freezes literal rain material sampling until the host reports `TurnStart`
    /// (signal code 10) from its submit handler; occupancy continues to track
    /// the live grid.
    pub fn note_keystroke(&mut self) {
        // Cap far past cadence saturation (heat clamps at knee_hi ≈ 7 gains):
        // bounds the advance-side replay under a paste flood.
        self.pending_keys = self.pending_keys.saturating_add(1).min(64);
        self.rain_material_editing = true;
        // Rain weather: keystrokes keep CALM alive (never reach WORKING).
        if let Some(rain) = self.rain.as_deref_mut() {
            rain.note_keystroke();
        }
    }

    /// Visual bell → the rain engine's 2 s constant-luminance amber ALERT
    /// hue-ramp (gated by the `bell_alert` knob engine-side).
    pub fn note_bell(&mut self) {
        if let Some(rain) = self.rain.as_deref_mut() {
            rain.note_bell();
        }
    }

    /// Embedded host observed wheel/PgUp while an alternate-screen TUI is
    /// active: hold the rain still during transcript reading.
    pub fn note_matrix_rain_alt_scroll(&mut self) {
        if let Some(rain) = self.rain.as_deref_mut() {
            rain.note_alt_scroll();
        }
    }

    /// Payload-free observable agent/tool choreography. The host sends only a
    /// stable enum code + bounded weight; literal glyphs still come exclusively
    /// from the protected terminal snapshot.
    pub fn note_matrix_rain_signal(&mut self, code: u32, weight: u32) {
        if code == crate::matrix_rain::RainSignal::TurnStart as u32 {
            self.rain_material_editing = false;
        }
        if let Some(rain) = self.rain.as_deref_mut() {
            rain.note_signal(code, weight);
        }
    }

    /// Whether any effect surface is enabled (sparkle and rain count even
    /// while quiescent — the master switches are the honest gates).
    #[must_use]
    pub fn enabled_any(&self) -> bool {
        self.glow_cfg.enabled
            || self.trail_cfg.enabled
            || self.sparkle.is_some()
            || self.rain.is_some()
    }

    // --- cursor wake ------------------------------------------------------

    /// Configure the LUMEN cursor aurora (the additive `cursor_glow_add`
    /// channel). Mirrors the native knobs + clamps (`aterm-gui` accessors):
    /// `style` ∈ lumen|phaser|rainbow kitty|sparkle|fire|laser|beam|water|comet
    /// (unknown → lumen; `rainbow` is a back-compat alias for the rainbow kitty banded
    /// ribbon — the old laser-like sweep it used to name is the explicit
    /// `phaser`); `color = None` uses the native style default (Laser/Beam/
    /// Sparkle/Comet have canonical hues; other styles derive from
    /// `theme_cursor`) and `accent = None` brightens that color 1.5×;
    /// `duration_ms` clamps 30..=2000, `length` 1..=512, `intensity` 0..=1,
    /// `radius` 0..=2.
    #[allow(
        clippy::too_many_arguments,
        reason = "one call mirrors the native GlowConfig resolution; a builder would relocate the list"
    )]
    pub fn set_cursor_glow(
        &mut self,
        enabled: bool,
        style: &str,
        color: Option<u32>,
        accent: Option<u32>,
        duration_ms: u64,
        length: u32,
        intensity: f32,
        radius: f32,
        ring: bool,
        theme_cursor: u32,
    ) {
        use crate::cursor_glow::{
            BEAM_DEFAULT_COLOR, COMET_DEFAULT_COLOR, LASER_DEFAULT_COLOR, SPARKLE_DEFAULT_COLOR,
        };

        let color_from_cursor = color.is_none();
        let accent_from_cursor = color_from_cursor && accent.is_none();
        let resolved = GlowStyle::parse(style);
        let default_color = match resolved {
            GlowStyle::Laser => LASER_DEFAULT_COLOR,
            GlowStyle::Beam => BEAM_DEFAULT_COLOR,
            GlowStyle::Comet => COMET_DEFAULT_COLOR,
            GlowStyle::Sparkle => SPARKLE_DEFAULT_COLOR,
            _ => theme_cursor,
        };
        let color = color.unwrap_or(default_color) & 0x00FF_FFFF;
        let accent = accent.map_or_else(|| brighten(color, 1.5), |a| a & 0x00FF_FFFF);
        self.glow_color_from_cursor = color_from_cursor;
        self.glow_accent_from_cursor = accent_from_cursor;
        self.glow_cfg = GlowConfig {
            dark_theme: true,
            enabled,
            style: resolved,
            color,
            accent,
            duration: Duration::from_millis(duration_ms.clamp(30, 2000)),
            length: (length as usize).clamp(1, 512),
            intensity: intensity.clamp(0.0, 1.0),
            radius: radius.clamp(0.0, 2.0),
            ring,
            // Water (its fluid wave wake is the streak) and rainbow kitty (its banded
            // ribbon IS the streak) suppress the additive beam — see
            // `style_has_beam`; derived from the raw style string so the beam
            // flag can't cross the FFI boundary.
            beam: crate::cursor_glow::style_has_beam(style),
            // Embedders don't report the live cursor SHAPE; keep the classic
            // cell-centre attach.
            head_dx: 0.5,
            // `set_cursor_glow` never RESOLVES a Trail Pack (a `pack:<id>` style is
            // armed separately via `set_cursor_trail_pack`). A built-in style
            // selection means "not a pack": selecting one after a pack was armed
            // must CLEAR the pack, or `tick` (which branches on `pack.is_some()`)
            // keeps rendering the stale pack under the new built-in style. Only a
            // re-select of the SAME `pack:<id>` style (which parses back to
            // `Custom`) preserves the already-resolved pack, so a plain reconfigure
            // (colour/intensity) of a live custom trail does not clobber it.
            pack: if matches!(resolved, GlowStyle::Custom) {
                self.glow_cfg.pack
            } else {
                None
            },
            // The host's typing-wake preference survives a reconfigure exactly
            // like its intensity/colour choices do.
            wake_persist_s: self.glow_cfg.wake_persist_s,
        };
        if !enabled {
            self.glow.reset();
        }
    }

    /// Arm (or clear) a **Trail Pack** — user-generated cursor trails as data
    /// (`trail_pack::compile_trail_pack_toml`). `toml == None` clears any live
    /// pack (reverting to the plain `style`); `Some(source)` compiles the pack
    /// and, on success, stores the resolved [`TrailParams`] into `glow_cfg.pack`
    /// with `style = GlowStyle::Custom`. On a compile ERROR the prior pack is
    /// LEFT INTACT and the joined diagnostics are returned (closing the
    /// silent-drop gap the sparkle `set_sparkle_custom_specs` posture warns
    /// about); `Ok` returns `None`.
    pub fn set_cursor_trail_pack(&mut self, toml: Option<String>) -> Option<String> {
        match toml {
            None => {
                self.glow_cfg.pack = None;
                // Revert the enum to the plain default so the built-in emit path
                // resumes cleanly (the resolver re-picks a real style next call).
                if matches!(self.glow_cfg.style, GlowStyle::Custom) {
                    self.glow_cfg.style = GlowStyle::Lumen;
                }
                None
            }
            Some(source) => match crate::trail_pack::compile_trail_pack_toml(&source) {
                Ok(pack) => {
                    self.glow_cfg.pack = Some(*pack.params());
                    self.glow_cfg.style = GlowStyle::Custom;
                    self.glow_cfg.beam = true;
                    None
                }
                Err(e) => Some(e.diagnostics().join("\n")),
            },
        }
    }

    /// Configure the legacy opaque comet trail (the `cursor_trail` channel).
    /// `color` `None` = the theme cursor. Clamps mirror the native accessors.
    pub fn set_cursor_trail(
        &mut self,
        enabled: bool,
        duration_ms: u64,
        length: u32,
        color: Option<u32>,
        theme_cursor: u32,
    ) {
        self.trail_color_from_cursor = color.is_none();
        self.trail_cfg = TrailConfig {
            enabled,
            duration: Duration::from_millis(duration_ms.clamp(30, 2000)),
            max_len: (length as usize).clamp(1, 512),
            color: color.unwrap_or(theme_cursor) & 0x00FF_FFFF,
            // Gentle baseline; `apply` heats a per-frame copy from the typing cadence.
            intensity: 0.0,
            warmth: 0.0,
        };
        if !enabled {
            self.trail.reset();
        }
    }

    // --- sparkle words ----------------------------------------------------

    /// Master sparkle-words switch. Enabling compiles the lexicon (languages +
    /// any override TOML) once; disabling drops every occurrence/episode and
    /// clears the channels next frame (the `toggle_sparkle_words` panic-off).
    pub fn set_sparkle_enabled(&mut self, on: bool) {
        if on {
            self.rebuild_sparkle(true);
        } else {
            self.sparkle = None;
            // Master off is user intent — the §1.1 reset table's hard_reset
            // arm (done marks cleared; a re-enable starts fresh). The lexicon
            // diagnostics drop with the lexicon they describe.
            self.lexicon_conflicts.clear();
            self.decos.hard_reset();
        }
    }

    /// `true` while the sparkle master is on.
    #[must_use]
    pub fn sparkle_enabled(&self) -> bool {
        self.sparkle.is_some()
    }

    /// Per-class gates (native `[sparkle_words.<class>] enabled`). `orca` is
    /// additionally ANDed with `!ORCA_SUSPENDED` (v3 §4 — the suspension's
    /// single source, mirrored by the native resolver). `emphasis` resolves
    /// as `enabled && (ink_enabled || has_custom_specs)` (v3 §6) exactly like
    /// the native resolver.
    pub fn set_sparkle_classes(
        &mut self,
        profanity: bool,
        feline: bool,
        orca: bool,
        emphasis: bool,
    ) {
        self.deco_cfg.profanity = profanity;
        self.deco_cfg.feline = feline;
        self.deco_cfg.orca = orca && !crate::ORCA_SUSPENDED;
        self.emphasis_enabled = emphasis;
        self.recompute_emphasis();
        self.refresh_sparkle_cfg();
    }

    /// Animated-ink knobs (native `[sparkle_words.ink]`): `strength` clamps
    /// 0..=1; `sweep_ms` clamps 350..=6000 with a 600 floor while `loop_` (the
    /// §6.4 flash-safety margin — structural, not bypassable).
    pub fn set_sparkle_ink(&mut self, enabled: bool, strength: f32, sweep_ms: u32, loop_: bool) {
        let floor = if loop_ { 600 } else { 350 };
        self.deco_cfg.ink_enabled = enabled;
        self.deco_cfg.ink_strength = strength.clamp(0.0, 1.0);
        self.deco_cfg.ink_sweep_ms = sweep_ms.clamp(floor, 6000);
        self.deco_cfg.ink_loop = loop_;
        // v3 §6: emphasis re-resolves — custom specs keep it scanning with
        // ink off (a graphic-only custom word must still fire).
        self.recompute_emphasis();
        self.refresh_sparkle_cfg();
    }

    /// Force the static, non-animating path (no twinkle/jitter/pulse; novas
    /// collapse to the static glint) — the accessibility override the native
    /// `reduced_motion` config flag drives.
    pub fn set_sparkle_reduced_motion(&mut self, on: bool) {
        self.deco_cfg.reduced_motion = on;
        self.refresh_sparkle_cfg();
    }

    /// Alt-screen suppression (native `[sparkle_words] suppress_in_alt_screen`,
    /// default off): when on, full-screen apps (vim/less/htop) render with no
    /// decorations at all — the v1 launch behavior.
    pub fn set_sparkle_alt_screen_suppression(&mut self, on: bool) {
        self.deco_cfg.suppress_in_alt_screen = on;
        self.refresh_sparkle_cfg();
    }

    /// Feline knobs (native `[sparkle_words.feline]`): `style = "cat"` emits
    /// the authored cat; legacy `"paw"` is ink-only and emits no paw graphic.
    pub fn set_sparkle_feline(
        &mut self,
        style: &str,
        magic: bool,
        allow_bare_cat: bool,
        cjk_single_char: bool,
    ) {
        self.deco_cfg.feline_style = if style.eq_ignore_ascii_case("paw") {
            crate::word_decorations::FelineStyle::Paw
        } else {
            crate::word_decorations::FelineStyle::Cat
        };
        self.deco_cfg.feline_magic = magic;
        self.deco_cfg.allow_bare_cat = allow_bare_cat;
        self.deco_cfg.cjk_single_char = cjk_single_char;
        self.refresh_sparkle_cfg();
    }

    /// Profanity knobs (native `[sparkle_words.profanity]`): `style` =
    /// "rainbow" (the v3 §3.1 animated rainbow ink, the default) | "nova"
    /// (the v2 classic nova) | "sparkle" (exact v1); clamps mirror the
    /// native resolver — `density` 1..=12, `anim_ms` 350..=10000 (the WCAG
    /// 2.3.1 twinkle floor), `jitter` 0..=6, `intensity` 0..=1.
    /// `supernova_chance` (0..=100, 0 disables) is the §3.2 FUCK SUPER NOVA
    /// escalation chance — consulted only under `style = "rainbow"`.
    #[allow(
        clippy::too_many_arguments,
        reason = "one call mirrors the native profanity table; a builder would relocate the list"
    )]
    pub fn set_sparkle_profanity(
        &mut self,
        style: &str,
        density: u32,
        anim_ms: u32,
        jitter: i8,
        intensity: f32,
        magic: bool,
        supernova_chance: u32,
    ) {
        self.deco_cfg.profanity_style = if style.eq_ignore_ascii_case("sparkle") {
            crate::word_decorations::ProfanityStyle::Sparkle
        } else if style.eq_ignore_ascii_case("nova") {
            crate::word_decorations::ProfanityStyle::Nova
        } else {
            crate::word_decorations::ProfanityStyle::Rainbow
        };
        self.deco_cfg.density = density.clamp(1, 12);
        self.deco_cfg.anim_ms = u64::from(anim_ms).clamp(350, 10_000);
        self.deco_cfg.jitter = jitter.clamp(0, 6);
        self.deco_cfg.intensity = intensity.clamp(0.0, 1.0);
        self.deco_cfg.profanity_magic = magic;
        self.deco_cfg.supernova_chance = supernova_chance.min(100) as u8;
        self.refresh_sparkle_cfg();
    }

    /// v3 §6: the `[[sparkle_words.custom]]` TOML fragment (the SAME fragment
    /// the native config carries) — per-word effect specs over the three
    /// axes. Words are auto-appended to the emphasis class (CJK surfaces as
    /// `cjk = true` entries) and their specs override class defaults +
    /// bypass class gates. Malformed TOML KEEPS the previous specs and
    /// surfaces the parse error through
    /// [`Self::sparkle_lexicon_warnings`] (the Toy-Pack posture: a bad
    /// fragment must not silently wipe a working config with zero
    /// diagnostics). Pass `None` to clear.
    pub fn set_sparkle_custom_specs(&mut self, toml: Option<String>) {
        let entries = match toml.as_deref().map(crate::spec::parse_custom_toml) {
            None => {
                self.custom_spec_warning = None;
                Vec::new() // explicit clear
            }
            Some(Ok(entries)) => {
                self.custom_spec_warning = None;
                entries
            }
            Some(Err(e)) => {
                // Keep the previous table/lexicon untouched — the caller sees
                // the diagnostic instead of a silently-cleared working config.
                self.custom_spec_warning = Some(format!("sparkle_words.custom: {e}"));
                return;
            }
        };
        let (table, lexicon) = crate::spec::build_custom(&entries);
        self.deco_cfg.spec_table = table;
        self.custom_lexicon = lexicon;
        self.recompute_emphasis();
        // Custom words extend the LEXICON too — full rebuild (hard_reset per
        // the §1.1 table rides along).
        self.rebuild_sparkle(false);
    }

    /// v3 §6 emphasis resolve: `enabled && (ink_enabled || has_custom_specs)`
    /// (both resolvers, normative).
    fn recompute_emphasis(&mut self) {
        self.deco_cfg.emphasis = self.emphasis_enabled
            && (self.deco_cfg.ink_enabled || self.deco_cfg.spec_table.has_custom());
    }

    /// Languages whose AMBIGUOUS homograph entries un-gate (native
    /// `languages`; non-ambiguous forms always load). Rebuilds the lexicon if
    /// sparkle is enabled.
    pub fn set_sparkle_languages(&mut self, languages: &[&str]) {
        self.languages = if languages.is_empty() {
            vec!["en".to_string()]
        } else {
            languages.iter().map(|s| (*s).to_string()).collect()
        };
        self.rebuild_sparkle(false);
    }

    /// User lexicon-override TOML merged over the builtin (the native
    /// `lexicon` file + per-class `extra_words` channel — same `[[entry]]`
    /// schema). A malformed override falls back to the builtin-only lexicon
    /// (the native fail-open posture).
    pub fn set_sparkle_lexicon_override(&mut self, toml: Option<String>) {
        self.override_toml = toml;
        self.rebuild_sparkle(false);
    }

    /// Folded surfaces to never decorate (native global `deny` +
    /// `ignore_words`), replacing the current set.
    pub fn set_sparkle_deny(&mut self, words: &[&str]) {
        self.deco_cfg.ignore = words.iter().map(|w| aterm_lexicon::fold(w)).collect();
        self.refresh_sparkle_cfg();
    }

    /// Lexicon build diagnostics (v3 §6): user/custom surfaces that can never
    /// scan as written (single-char CJK without the opt-in, mixed-script /
    /// multi-word) and cross-class collisions, captured at the last lexicon
    /// rebuild — the web mirror of the native resolver's warning log. Empty
    /// while sparkle is off or the lexicon is clean. Filtered by the CURRENT
    /// resolved config: a "requires `cjk_single_char = true`" warning drops
    /// out once `set_sparkle_feline` enables that opt-in (the lexicon cannot
    /// see scan options, so the filter lives here). Also carries the parse
    /// diagnostic from a malformed [`Self::set_sparkle_custom_specs`]
    /// fragment (whose previous specs were KEPT).
    #[must_use]
    pub fn sparkle_lexicon_warnings(&self) -> Vec<String> {
        self.custom_spec_warning
            .iter()
            .cloned()
            .chain(
                self.lexicon_conflicts
                    .iter()
                    .filter(|w| lexicon_warning_applies(w, self.deco_cfg.cjk_single_char))
                    .cloned(),
            )
            .collect()
    }

    /// Re-resolve the active config (knob change; lexicon reuse) and flush the
    /// per-occurrence state so the next frame reflects it — the native
    /// hot-reload semantics. v3 §1.1 reset table: web `set_sparkle_*` knob
    /// setters are `hard_reset()` (parity with the native config reload),
    /// so `done_marks` clears and every one-shot replays after a knob change.
    fn refresh_sparkle_cfg(&mut self) {
        if let Some(rs) = self.sparkle.as_mut() {
            rs.cfg = self.deco_cfg.clone();
            self.decos.hard_reset();
        }
    }

    /// (Re)compile the lexicon + resolved config. `force` builds even when
    /// currently off (the master-enable path).
    fn rebuild_sparkle(&mut self, force: bool) {
        if self.sparkle.is_none() && !force {
            return;
        }
        let refs: Vec<&str> = self.languages.iter().map(String::as_str).collect();
        // v3 §6: the synthesized custom-word entries append to the user's
        // override so custom words actually scan; dropped/conflicting
        // surfaces are captured off the lexicon `conflicts` channel here and
        // surfaced through `sparkle_lexicon_warnings` (the web mirror of the
        // native resolver's warning log).
        let mut over = self.override_toml.clone().unwrap_or_default();
        over.push_str(&self.custom_lexicon);
        let over = (!over.is_empty()).then_some(over);
        let lexicon = aterm_lexicon::Lexicon::with_languages_and_override(&refs, over.as_deref())
            .unwrap_or_else(|_| aterm_lexicon::Lexicon::with_languages(&refs));
        self.lexicon_conflicts = lexicon.conflicts().to_vec();
        self.sparkle = Some(Resolved {
            cfg: self.deco_cfg.clone(),
            lexicon: std::sync::Arc::new(lexicon),
        });
        // Lexicon rebuild ≈ config reload: hard_reset per the §1.1 table.
        self.decos.hard_reset();
    }

    // --- matrix rain --------------------------------------------------------

    /// Master PHOSPHOR rain switch. Enabling lazily builds the engine (ROM
    /// raster + first atlas bake are one-time warmup, not per-frame cost);
    /// disabling drops it entirely — the channels scrub on the next `apply`
    /// and the pipeline carries zero rain state (the sparkle master-off
    /// posture).
    pub fn set_matrix_rain_enabled(&mut self, on: bool) {
        self.rain_cfg.enabled = on;
        if !on {
            self.rain = None;
        } else if self.rain.is_none() {
            let mut rain = Box::new(MatrixRain::new(self.rain_cfg));
            rain.set_visibility(self.rain_visibility);
            rain.set_reduced_motion(self.rain_reduced_motion);
            rain.set_material_editing(self.rain_material_editing);
            // The engine's clock starts at zero: anchor the ms feed here so
            // the first `advance` delivers only the post-enable delta.
            self.rain_clock_ms = self.clock.as_millis() as u64;
            self.rain = Some(rain);
        }
    }

    /// `true` while the rain master is on.
    #[must_use]
    pub fn matrix_rain_enabled(&self) -> bool {
        self.rain.is_some()
    }

    /// Configure the rain knobs (native `[matrix_rain]`), kept across off/on
    /// toggles. Clamps mirror the native resolver (the engine re-clamps
    /// defensively): `fps` 12..=60, `density` 1..=12, `speed`/`trail` 1..=10,
    /// alphas 16..=135 (`None` derives from the §6 luminance constraint;
    /// `head_alpha` is floored at the resolved body alpha engine-side),
    /// `mutation_ms` 80..=2000, `idle_secs` 2..=120. `hue` ∈
    /// matrix|theme|custom — custom reads `hue_color`, unknown → matrix (the
    /// lenient-string precedent). `output_material` is explicit; its material
    /// sampler freezes from `note_keystroke` through an explicit TurnStart so
    /// unsent multiline drafts retain the previous real-output tape. A live
    /// engine re-resolves immediately (ramp + atlas rebake — the hot-reload
    /// dirty rebuild).
    #[allow(
        clippy::too_many_arguments,
        reason = "one call mirrors the native matrix_rain table; a builder would relocate the list"
    )]
    pub fn set_matrix_rain(
        &mut self,
        fps: u32,
        density: u32,
        speed: u32,
        trail: u32,
        alpha: Option<u32>,
        head_alpha: Option<u32>,
        hue: &str,
        hue_color: Option<u32>,
        mutation_ms: u32,
        idle_secs: u32,
        suppress_in_alt_screen: bool,
        turn_wave: bool,
        bell_alert: bool,
        output_material: bool,
        seed: u64,
        default_bg: u32,
        theme_fg: u32,
    ) {
        let clamp_alpha =
            |a: u32| a.clamp(u32::from(RAIN_ALPHA_FLOOR), u32::from(RAIN_ALPHA_CAP)) as u8;
        self.rain_cfg = RainConfig {
            enabled: self.rain_cfg.enabled,
            fps: fps.clamp(12, 60) as u8,
            density: density.clamp(1, 12) as u8,
            speed: speed.clamp(1, 10) as u8,
            trail: trail.clamp(1, 10) as u8,
            alpha_override: alpha.map(clamp_alpha),
            head_alpha_override: head_alpha.map(clamp_alpha),
            hue: if hue.eq_ignore_ascii_case("theme") {
                RainHue::Theme
            } else if hue.eq_ignore_ascii_case("custom") {
                hue_color.map_or(RainHue::Matrix, |c| RainHue::Custom(c & 0x00FF_FFFF))
            } else {
                RainHue::Matrix
            },
            mutation_ms: mutation_ms.clamp(80, 2000) as u16,
            idle_secs: idle_secs.clamp(2, 120) as u16,
            suppress_in_alt_screen,
            turn_wave,
            bell_alert,
            // Web hosts have no OSC-133 host feed yet; the knob is inert there.
            exit_tint: true,
            // When enabled, the coherent RenderCell snapshot supplies literal
            // output codepoints; the Editing gate above withholds unsent drafts.
            output_material,
            seed,
            default_bg: default_bg & 0x00FF_FFFF,
            theme_fg: theme_fg & 0x00FF_FFFF,
        };
        if let Some(rain) = self.rain.as_deref_mut() {
            rain.set_config(self.rain_cfg);
        }
    }

    /// Update theme-derived rain colors without requiring the host to replay
    /// every matrix-rain knob after a live theme change.
    pub fn set_matrix_rain_theme(&mut self, default_bg: u32, theme_fg: u32) {
        let default_bg = default_bg & 0x00FF_FFFF;
        let theme_fg = theme_fg & 0x00FF_FFFF;
        if self.rain_cfg.default_bg == default_bg && self.rain_cfg.theme_fg == theme_fg {
            return;
        }
        self.rain_cfg.default_bg = default_bg;
        self.rain_cfg.theme_fg = theme_fg;
        if let Some(rain) = self.rain.as_deref_mut() {
            rain.set_config(self.rain_cfg);
        }
    }

    /// OS/config reduce-motion for rain: the engine emits nothing, fp 0,
    /// inactive (the sparkle `reduced_motion` twin).
    pub fn set_matrix_rain_reduced_motion(&mut self, on: bool) {
        self.rain_reduced_motion = on;
        if let Some(rain) = self.rain.as_deref_mut() {
            rain.set_reduced_motion(on);
        }
    }

    // --- per-frame application ---------------------------------------------

    /// Run every enabled effect for the current injected instant and fill the
    /// overlay channels of `input` (a fresh `cell_frame` snapshot of `term`).
    /// `cell_w`/`cell_h` are the renderer's device-pixel cell metrics.
    ///
    /// With everything disabled the channels are cleared (a reused snapshot
    /// scratch may carry the previous frame's overlays after a live toggle)
    /// and `0` returns — byte-identical to the pre-effects render.
    ///
    /// Returns a fingerprint that changes on every visible effect change
    /// (stable when settled), for host-side repaint keys.
    pub fn apply(
        &mut self,
        term: &mut Terminal,
        input: &mut RenderInput,
        cell_w: usize,
        cell_h: usize,
    ) -> u64 {
        if !self.enabled_any() {
            input.cursor_trail.clear();
            input.cursor_glow_add.clear();
            input.glow_halo.clear();
            input.fire_patch.clear();
            input.glow_under.clear();
            input.char_fg.clear();
            input.fire_halo.clear();
            input.cursor_fill_override = None;
            input.word_decorations.clear();
            input.ink.clear();
            input.cat_quads.clear();
            input.cat_atlas = None;
            input.free_sprites.clear();
            input.free_atlas = None;
            input.nova_add.clear();
            input.rain_quads.clear();
            input.rain_atlas = None;
            input.rain_add.clear();
            return 0;
        }
        let now = self.now();
        let (rows, cols) = (input.rows, input.cols);
        let cur = input
            .cursor_visible
            .then_some((input.cursor_row as u16, input.cursor_col as u16));
        // Fire/Water/Vapor choose additive-vs-contrast treatment from the
        // background actually presented by this frame, not the construction
        // default. Web hosts stamp live OSC 11 + DECSCNM here before `apply`.
        // An older/custom host may leave the field unset; preserve the legacy
        // dark-ground behavior in that case rather than interpreting the
        // sentinel's high byte as a real color.
        self.glow_cfg.dark_theme =
            input.default_bg == aterm_core::render::COLOR_UNSET || theme_is_dark(input.default_bg);
        // Match the native cursor-wake rule: automatic colors follow the live
        // cursor sampled into this coherent frame. Explicit colors/accents stay
        // fixed, and Laser remains electric in its configured/default hue
        // rather than inheriting arbitrary shell OSC 12 colors.
        if input.cursor_color != aterm_core::render::COLOR_UNSET {
            let live = input.cursor_color & 0x00FF_FFFF;
            if self.glow_color_from_cursor && !matches!(self.glow_cfg.style, GlowStyle::Laser) {
                self.glow_cfg.color = live;
                if self.glow_accent_from_cursor {
                    self.glow_cfg.accent = brighten(live, 1.5);
                }
            }
            if self.trail_color_from_cursor {
                self.trail_cfg.color = live;
            }
        }

        // Straggler keystrokes (noted after the last `advance`): flush them at
        // the current instant so a host that renders without advancing first
        // still ignites this frame — the pre-queue behavior, zero-gap and all;
        // the honest spread needs an `advance` delta to spread across.
        for _ in 0..self.pending_keys {
            self.typing_cadence.on_keystroke(now);
        }
        self.pending_keys = 0;

        // Why: native suppresses unfocused animation with a motion-policy
        // AMPLITUDE fold (app_render: `intensity *=` / `enabled &=`) that the web
        // path never ported, so visible-unfocused split panes animated at full
        // strength. Composition forms so a load-shed envelope can stack here.
        let motion_amp: f32 = if self.focused { 1.0 } else { 0.0 };

        // Cursor wake: comet trail (opaque) + aurora (additive), exactly one
        // of which is enabled by style in the native app — both are honored
        // here so the embedder's config surface decides. Ignite a COPY of the
        // persistent config with this frame's typing-cadence intensity (mutating
        // the stored cfg would compound the colour heat every frame).
        let mut trail_cfg = self.trail_cfg;
        crate::cursor_trail::ignite(
            &mut trail_cfg,
            self.typing_cadence.intensity(now),
            self.typing_cadence.warmth(now),
        );
        trail_cfg.enabled &= self.focused;
        let trail_fp = self
            .trail
            .tick(cur, now, &trail_cfg, &mut self.trail_scratch);
        // SWAP (not clone_from) the double-buffered channels into the input:
        // every engine clears its scratch at tick/emit start, so the returned
        // buffer's stale content is never read — the swap hands the host the
        // fresh frame without an O(len) copy per channel per frame.
        std::mem::swap(&mut input.cursor_trail, &mut self.trail_scratch);
        // Keyed on the STORED flag, not the focus-gated local: the colour is a
        // config value, so unfocusing must not freeze it at a stale publish.
        if self.trail_cfg.enabled {
            input.cursor_trail_color = trail_cfg.color;
        }
        // Window-space layout under the embedder's chrome (set_chrome). The
        // default 0/0 chrome degenerates to the identity layout (origin 0,
        // win == grid extents), so window-absolute emissions coincide with the
        // historical grid-relative ones byte-for-byte.
        let glow_geom = self.chrome_geom(rows, cols, cell_w, cell_h);
        // Why: a COPY — zeroing the stored intensity would destroy the user's
        // configured value, which only `set_cursor_glow` ever republishes.
        // `intensity <= 0` is the engine's documented unfocus channel (it cools
        // the thermal integrators instead of wiping them, as `enabled=false` would).
        let mut glow_cfg = self.glow_cfg;
        glow_cfg.intensity *= motion_amp;
        let glow_fp = self
            .glow
            .tick(cur, now, &glow_cfg, glow_geom, &mut self.glow_scratch);
        std::mem::swap(&mut input.cursor_glow_add, &mut self.glow_scratch);
        // The glow engine's OTHER output streams, spliced exactly like the
        // native app's block (app_render): the RADIAL halos (fire embers /
        // crown / impact flash), the PER-PIXEL fire field, the UNDER-INK flame
        // body, the CHARRED glyph ink and the fire CONTRAST-HALO strengths.
        // The pixel streams are window-absolute in the SAME chrome_geom the
        // tick just used — under chrome, flames genuinely rise into the head
        // band instead of clipping at the cell edge. Every stream is empty for
        // the styles that don't emit it (and `tick` clears its scratches each
        // frame), so a non-fire/halo style stays byte-identical to the
        // pre-splice frames.
        //
        // SWAPPED, not copied — for the same reason the channels above are
        // (`tick` clears every one of these scratches at entry, so the buffer
        // handed back holds the previous frame and is never read). `glow_under`
        // is the one that pays: a hot rainbow ribbon pins 6k-14k 16-byte GlowQuad
        // (tests/cursor_bench.rs), i.e. ~100-230 KB of memcpy per present that
        // duplicates bytes the emitter just wrote. `apply` is the sole consumer
        // of the pipeline's glow and reads each stream exactly once, which is
        // the precondition the swap needs.
        self.glow.swap_halos(&mut input.glow_halo);
        self.glow.swap_patches(&mut input.fire_patch);
        self.glow.swap_under_quads(&mut input.glow_under);
        self.glow.swap_charred(&mut input.char_fg);
        self.glow.swap_halo_cells(&mut input.fire_halo);
        // Fire's FORGE cursor is visible state just like its flame streams:
        // carry the warm-metal fill through the shared RenderInput seam on the
        // wasm/gpu-web hosts too. Stamp this field on EVERY enabled frame so a
        // Fire -> non-Fire toggle cannot retain the prior fill in the reused
        // scratch. The renderer applies the resolved body colour to the live
        // DECSCUSR shape, matching the native GUI's shared override seam.
        input.cursor_fill_override = (self.glow_cfg.enabled
            && self.glow_cfg.intensity > 0.0
            && matches!(self.glow_cfg.style, GlowStyle::Fire))
        .then(|| self.glow.forge_fill())
        .flatten();

        // Sparkle words: rescan only when the grid changed (damage epoch),
        // animate every applied frame. Alt-screen handling mirrors native:
        // decorated by default, suppressed only when the knob opts back in.
        let mut damage_consumed = false;
        let deco_fp = if let Some(rs) = self.sparkle.as_ref() {
            if term.is_alternate_screen() && rs.cfg.suppress_in_alt_screen {
                // v3 §1.1 reset table: suppressed-alt-screen entry is
                // freeze/thaw, not a reset — a vim round-trip resumes every
                // animation exactly where it paused (no mass replay).
                self.decos.freeze(now);
                self.deco_scratch.clear();
                self.ink_scratch.clear();
                self.free_scratch.clear();
                self.nova_scratch.clear();
                0
            } else {
                // Resume from a suppressed-alt round-trip (no-op when the
                // engine is not frozen): shift every stored timestamp by the
                // freeze duration before the rescan/tick read them.
                self.decos.thaw(now);
                let epoch = term.damage_epoch();
                if self.decos.needs_rescan(epoch) {
                    // Scan the frame the renderer ALREADY resolved into `input`
                    // (cell_frame_into ran before apply on every host) instead of
                    // re-walking the grid a second time — one full-grid resolve per
                    // damaged frame, not two. This is the same path the native GUI
                    // scans (aterm-gui app_render rescan_from_cells), so the embedder
                    // now matches it. Byte-identical to the cold term rescan in the
                    // shipped bidi-off config (proven by rescan_from_cells_matches_term_rescan);
                    // under the `bidi` feature it scans the renderer's VISUAL-order
                    // cells, which is the intended on-screen decoration placement.
                    // Fall back to the term walk only if a host called apply without a
                    // populated cell snapshot.
                    if input.cells.len() >= rows && input.line_sizes.len() >= rows {
                        self.decos.rescan_from_cells(
                            &input.cells,
                            &input.line_sizes,
                            rows,
                            cols,
                            &rs.lexicon,
                            &rs.cfg,
                            epoch,
                            now,
                        );
                    } else {
                        self.decos
                            .rescan(term, rows, cols, &rs.lexicon, &rs.cfg, epoch, now);
                    }
                }
                // Consume the damage session: `damage_epoch` counts once per
                // session and is re-armed only by `take_damage`, which nothing
                // else calls on the web path (the headless-capture lesson —
                // without this the epoch freezes and stale occurrences stick).
                term.take_damage();
                damage_consumed = true;
                let geom = EffectGeom {
                    cell_w: cell_w as u16,
                    cell_h: cell_h as u16,
                    rows: rows as u16,
                    cols: cols as u16,
                };
                let sel_view = SelView {
                    sel: term.text_selection(),
                    display_offset: term.grid().display_offset() as i32,
                };
                self.decos.tick(
                    now,
                    &rs.cfg,
                    geom,
                    // The web pipeline draws no cursor companion, so no word is
                    // ever answered by one — every feline occurrence keeps its
                    // ambient peek.
                    None,
                    Some(sel_view),
                    self.focused,
                    &mut self.deco_scratch,
                    &mut self.ink_scratch,
                    &mut self.free_scratch,
                    &mut self.nova_scratch,
                )
            }
        } else {
            self.deco_scratch.clear();
            self.ink_scratch.clear();
            self.free_scratch.clear();
            self.nova_scratch.clear();
            0
        };
        std::mem::swap(&mut input.word_decorations, &mut self.deco_scratch);
        std::mem::swap(&mut input.ink, &mut self.ink_scratch);
        // Overlay Phase 4: the engine no longer produces legacy cat quads —
        // clear the channel (a reused host snapshot may carry stale ones).
        input.cat_quads.clear();
        input.cat_atlas = None;
        // The atlas Arc rides only when free sprites do, so cat-free frames
        // stay byte-identical. (Emptiness checked on the input side — the
        // swap just moved this frame's sprites there.)
        std::mem::swap(&mut input.free_sprites, &mut self.free_scratch);
        input.free_atlas = if input.free_sprites.is_empty() {
            None
        } else {
            self.decos.free_atlas()
        };
        std::mem::swap(&mut input.nova_add, &mut self.nova_scratch);

        // PHOSPHOR rain: Tier-A occupancy rescan on grid change (damage
        // epoch); Tier-B live gates run inside `emit`. A snapshot whose
        // `snapshot_seq` trails the live epoch is TORN (the host mutated the
        // grid after `cell_frame_into`) — rain scanned from those cells would
        // land under text, so the torn frame emits nothing and the next fresh
        // snapshot resumes.
        let rain_fp = if let Some(rain) = self.rain.as_deref_mut() {
            rain.note_activity(term.content_seq());
            let epoch = term.damage_epoch();
            let torn = input.snapshot_seq != epoch;
            let display_offset = term.grid().display_offset() as i32;
            if display_offset != 0 {
                // A scrollback viewport is display-translated while the cursor
                // remains grid-relative. Do no O(rows*cols) work for a frame
                // that emit must suppress; returning live forces a fresh scan.
                rain.defer_reading();
            } else if !torn
                && rain.can_emit()
                && input.cells.len() >= rows
                && input.line_sizes.len() >= rows
            {
                let needs_rescan = rain.needs_rescan(epoch);
                let needs_resample = needs_rescan || rain.needs_material_sample();
                if needs_rescan {
                    rain.rescan_from_cells(
                        &input.cells,
                        &input.line_sizes,
                        &input.images,
                        rows,
                        cols,
                        input.default_bg,
                        epoch,
                    );
                }
                if needs_resample {
                    // Sample the exact SAME coherent cells used for occupancy.
                    // MatrixRain itself freezes this refresh during an unsent
                    // edit, sharing one native/embedded provenance contract.
                    rain.sample_material(&input.cells, rows, cur, &[]);
                }
            }
            // The web path's single damage consumer must still run when rain
            // is on while sparkle is off or suppressed (else the epoch
            // freezes); never consume twice per apply.
            if !damage_consumed {
                term.take_damage();
            }
            if torn {
                self.rain_scratch.clear();
                self.rain_add_scratch.clear();
                0
            } else {
                let geom = EffectGeom {
                    cell_w: cell_w as u16,
                    cell_h: cell_h as u16,
                    rows: rows as u16,
                    cols: cols as u16,
                };
                let tick_input = RainTickInput {
                    cursor: cur,
                    // No damage-recency tracking on the web path (v1): a
                    // hidden cursor protects no extra band here.
                    hidden_band: &[],
                    sel: Some(SelView {
                        sel: term.text_selection(),
                        display_offset,
                    }),
                    display_offset,
                    is_alt_screen: term.is_alternate_screen(),
                };
                rain.emit(
                    geom,
                    &tick_input,
                    &mut self.rain_scratch,
                    &mut self.rain_add_scratch,
                )
            }
        } else {
            self.rain_scratch.clear();
            self.rain_add_scratch.clear();
            0
        };
        std::mem::swap(&mut input.rain_quads, &mut self.rain_scratch);
        std::mem::swap(&mut input.rain_add, &mut self.rain_add_scratch);
        // The atlas Arc rides only when rain output does, so rain-free frames
        // stay byte-identical to the pre-feature path (the free_atlas
        // precedent; emptiness checked input-side post-swap).
        input.rain_atlas = if input.rain_quads.is_empty() && input.rain_add.is_empty() {
            None
        } else {
            self.rain.as_mut().and_then(|r| r.rain_atlas())
        };

        // One folded fingerprint (rotations keep equal per-engine fps from
        // cancelling); host repaint keys only need "changed vs stable".
        trail_fp ^ glow_fp.rotate_left(21) ^ deco_fp.rotate_left(42) ^ rain_fp.rotate_left(11)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SYNTHWAVE: &str = include_str!("../assets/trail-packs/synthwave.toml");

    fn stamp_effective_terminal_background(term: &Terminal, input: &mut RenderInput) {
        let color = if term.modes().reverse_video() {
            term.default_foreground()
        } else {
            term.default_background()
        };
        input.default_bg = aterm_render::rgb_to_u32([color.r, color.g, color.b]);
    }

    #[test]
    fn glow_polarity_tracks_live_osc11_and_decscnm_background() {
        let mut pipeline = EffectsPipeline::new();
        pipeline.set_cursor_glow(
            true,
            "fire",
            None,
            None,
            400,
            24,
            1.0,
            0.9,
            true,
            0x0050_FA7B,
        );
        let mut term = Terminal::new(2, 8);

        term.process(b"\x1b]11;rgb:ff/ff/ff\x07");
        let mut input = term.cell_frame(2, 8);
        stamp_effective_terminal_background(&term, &mut input);
        pipeline.apply(&mut term, &mut input, 10, 19);
        assert!(
            !pipeline.glow_cfg.dark_theme,
            "a live light OSC 11 ground selects the light-theme treatment"
        );

        term.process(b"\x1b]10;rgb:ee/ee/ee\x07\x1b]11;rgb:01/02/03\x07\x1b[?5h");
        term.cell_frame_into(&mut input, 2, 8);
        stamp_effective_terminal_background(&term, &mut input);
        pipeline.apply(&mut term, &mut input, 10, 19);
        assert!(
            !pipeline.glow_cfg.dark_theme,
            "DECSCNM makes the live foreground the effective light ground"
        );

        term.process(b"\x1b[?5l");
        term.cell_frame_into(&mut input, 2, 8);
        stamp_effective_terminal_background(&term, &mut input);
        pipeline.apply(&mut term, &mut input, 10, 19);
        assert!(
            pipeline.glow_cfg.dark_theme,
            "leaving DECSCNM restores the dark OSC 11 ground"
        );

        input.default_bg = aterm_core::render::COLOR_UNSET;
        pipeline.apply(&mut term, &mut input, 10, 19);
        assert!(
            pipeline.glow_cfg.dark_theme,
            "an unstamped legacy host retains the dark-ground fallback"
        );
    }

    fn stamp_cursor_color(term: &Terminal, input: &mut RenderInput, fallback: u32) {
        input.cursor_color = term.cursor_color().map_or(fallback, |color| {
            aterm_render::rgb_to_u32([color.r, color.g, color.b])
        });
    }

    #[test]
    fn automatic_wake_colors_follow_osc12_resets_and_theme_baselines() {
        let theme_cursor = 0x0011_2233;
        let mut pipeline = EffectsPipeline::new();
        pipeline.set_cursor_glow(
            true,
            "lumen",
            None,
            None,
            400,
            24,
            1.0,
            0.9,
            true,
            theme_cursor,
        );
        pipeline.set_cursor_trail(true, 400, 24, None, theme_cursor);
        let mut term = Terminal::new(2, 8);
        term.set_default_cursor_color(Some(aterm_core::terminal::Rgb::new(0x11, 0x22, 0x33)));

        term.process(b"\x1b]12;rgb:aa/00/11\x07");
        let mut input = term.cell_frame(2, 8);
        stamp_cursor_color(&term, &mut input, theme_cursor);
        pipeline.apply(&mut term, &mut input, 10, 19);
        assert_eq!(pipeline.glow_cfg.color, 0x00AA_0011);
        assert_eq!(pipeline.glow_cfg.accent, brighten(0x00AA_0011, 1.5));
        assert_eq!(input.cursor_trail_color, 0x00AA_0011);

        term.process(b"\x1b]112\x07");
        term.cell_frame_into(&mut input, 2, 8);
        stamp_cursor_color(&term, &mut input, theme_cursor);
        pipeline.apply(&mut term, &mut input, 10, 19);
        assert_eq!(pipeline.glow_cfg.color, theme_cursor);
        assert_eq!(input.cursor_trail_color, theme_cursor);

        let new_theme_cursor = 0x0044_5566;
        term.set_default_cursor_color(Some(aterm_core::terminal::Rgb::new(0x44, 0x55, 0x66)));
        term.cell_frame_into(&mut input, 2, 8);
        stamp_cursor_color(&term, &mut input, new_theme_cursor);
        pipeline.apply(&mut term, &mut input, 10, 19);
        assert_eq!(
            pipeline.glow_cfg.color, new_theme_cursor,
            "an automatic wake follows a live theme change without reconfiguration"
        );
        assert_eq!(input.cursor_trail_color, new_theme_cursor);
    }

    #[test]
    fn explicit_wake_colors_and_laser_ignore_live_cursor_recolors() {
        let mut pipeline = EffectsPipeline::new();
        pipeline.set_cursor_glow(
            true,
            "lumen",
            Some(0x0001_0203),
            None,
            400,
            24,
            1.0,
            0.9,
            true,
            0x0011_2233,
        );
        pipeline.set_cursor_trail(true, 400, 24, Some(0x0004_0506), 0x0011_2233);
        let mut term = Terminal::new(2, 8);
        term.process(b"\x1b]12;rgb:aa/bb/cc\x07");
        let mut input = term.cell_frame(2, 8);
        stamp_cursor_color(&term, &mut input, 0x0011_2233);
        pipeline.apply(&mut term, &mut input, 10, 19);
        assert_eq!(pipeline.glow_cfg.color, 0x0001_0203);
        assert_eq!(pipeline.glow_cfg.accent, brighten(0x0001_0203, 1.5));
        assert_eq!(input.cursor_trail_color, 0x0004_0506);

        pipeline.set_cursor_glow(
            true,
            "laser",
            None,
            None,
            400,
            24,
            1.0,
            0.9,
            true,
            0x0011_2233,
        );
        pipeline.set_cursor_trail(true, 400, 24, None, 0x0011_2233);
        term.cell_frame_into(&mut input, 2, 8);
        stamp_cursor_color(&term, &mut input, 0x0011_2233);
        pipeline.apply(&mut term, &mut input, 10, 19);
        assert_eq!(
            pipeline.glow_cfg.color,
            crate::cursor_glow::LASER_DEFAULT_COLOR,
            "Laser keeps its canonical storm-violet default hue"
        );
        assert_eq!(
            input.cursor_trail_color, 0x00AA_BBCC,
            "an independently enabled automatic opaque trail still follows OSC 12"
        );
    }

    /// The web FFI `set_cursor_trail_pack`: a valid pack arms `GlowStyle::Custom`
    /// with resolved params; a malformed pack RETURNS diagnostics and LEAVES the
    /// prior pack intact (the silent-drop gap this closes); `None` clears it.
    #[test]
    fn set_cursor_trail_pack_arms_clears_and_reports_diagnostics() {
        let mut p = EffectsPipeline::new();
        // Valid pack → armed, no diagnostics.
        assert!(
            p.set_cursor_trail_pack(Some(SYNTHWAVE.to_string()))
                .is_none()
        );
        assert!(p.glow_cfg.pack.is_some(), "valid pack is armed");
        assert!(matches!(p.glow_cfg.style, GlowStyle::Custom));
        let armed_fp = p.glow_cfg.pack.unwrap().pack_fp;

        // Malformed pack → diagnostics returned, PRIOR pack left intact.
        let diags = p
            .set_cursor_trail_pack(Some(
                "pack = 1\nid = \"x\"\n[ramp]\nkind = \"bogus\"\n".to_string(),
            ))
            .expect("malformed pack returns diagnostics");
        assert!(diags.contains("bogus"), "diagnostics surfaced: {diags}");
        assert_eq!(
            p.glow_cfg.pack.map(|q| q.pack_fp),
            Some(armed_fp),
            "the prior pack survives a failed reload"
        );

        // None clears the pack and reverts the enum off Custom.
        assert!(p.set_cursor_trail_pack(None).is_none());
        assert!(p.glow_cfg.pack.is_none(), "None clears the pack");
        assert!(!matches!(p.glow_cfg.style, GlowStyle::Custom));
    }

    /// Coherence: selecting a BUILT-IN style after a pack was armed CLEARS the
    /// pack, so `tick` (which branches on `pack.is_some()`) renders the built-in —
    /// never a stale pack under the new style's name. Re-selecting the SAME
    /// `pack:<id>` style preserves the resolved pack (a plain reconfigure).
    #[test]
    fn set_cursor_glow_builtin_clears_a_previously_armed_pack() {
        let mut p = EffectsPipeline::new();
        // Arm a pack (style → Custom, pack resolved).
        assert!(
            p.set_cursor_trail_pack(Some(SYNTHWAVE.to_string()))
                .is_none()
        );
        assert!(p.glow_cfg.pack.is_some() && matches!(p.glow_cfg.style, GlowStyle::Custom));

        // Select a built-in ("phaser"): the pack must be dropped and the style
        // must resolve to PHASER — the tick will take the built-in branch.
        p.set_cursor_glow(
            true,
            "phaser",
            None,
            None,
            240,
            18,
            0.7,
            0.6,
            true,
            0x0050_FA7B,
        );
        assert!(
            p.glow_cfg.pack.is_none(),
            "a built-in selection clears the pack"
        );
        assert!(
            matches!(p.glow_cfg.style, GlowStyle::Phaser),
            "style is PHASER"
        );

        // Drive a real frame: with the pack cleared and style Phaser, the glow
        // renders through the built-in path (non-empty on a hot typing run).
        let mut term = Terminal::new(6, 40);
        for _ in 0..6 {
            term.process(b"a");
            p.advance(60.0);
            let mut input = term.cell_frame(6, 40);
            p.apply(&mut term, &mut input, 8, 16);
        }
        // The pack stays cleared across ticks (no resurrection of the custom path).
        assert!(
            p.glow_cfg.pack.is_none(),
            "pack stays cleared while rendering PHASER"
        );

        // Re-arming the same pack then re-selecting the SAME pack: style parses
        // back to Custom, so the resolved pack is PRESERVED (a plain reconfigure).
        assert!(
            p.set_cursor_trail_pack(Some(SYNTHWAVE.to_string()))
                .is_none()
        );
        let fp = p.glow_cfg.pack.expect("re-armed").pack_fp;
        p.set_cursor_glow(
            true,
            "pack:synthwave",
            None,
            None,
            240,
            18,
            0.7,
            0.6,
            true,
            0x0050_FA7B,
        );
        assert_eq!(
            p.glow_cfg.pack.map(|q| q.pack_fp),
            Some(fp),
            "re-selecting the same pack style keeps the resolved pack"
        );
        assert!(matches!(p.glow_cfg.style, GlowStyle::Custom));
    }

    /// Keystrokes batched between `advance` calls must NOT collapse to a zero
    /// inter-key gap: `advance` replays them spread evenly across its `dt`, so
    /// a web host that batches key events between rAF callbacks ignites the
    /// cadence exactly like a native host that saw the real arrival times —
    /// and a paste no longer reads as infinite-speed typing.
    #[test]
    fn batched_keystrokes_spread_across_the_advance_delta() {
        // 6 keys physically spread over 360ms (60ms human cadence), delivered
        // as one batch: the spread reconstruction must match a hand-paced
        // cadence fed the true arrival times.
        let mut p = EffectsPipeline::new();
        for _ in 0..6 {
            p.note_keystroke();
        }
        p.advance(360.0);
        let spread = p.typing_cadence.intensity(p.now());

        let mut hand = TypingCadence::default();
        let t0 = p.t0;
        for i in 1..=6u64 {
            hand.on_keystroke(t0 + Duration::from_millis(60 * i));
        }
        let paced = hand.intensity(t0 + Duration::from_millis(360));
        assert!(
            (spread - paced).abs() < 1e-4,
            "spread replay {spread} must match the true-cadence heat {paced}"
        );

        // The OLD zero-gap collapse (all 6 at one instant) over-ignites; pin
        // that the spread stays strictly cooler than it.
        let mut collapsed = TypingCadence::default();
        let end = t0 + Duration::from_millis(360);
        for _ in 0..6 {
            collapsed.on_keystroke(end);
        }
        assert!(
            spread < collapsed.intensity(end),
            "the spread must ignite less than a zero-gap collapse"
        );
    }

    /// The identity law: 0/0 chrome must produce the exact historical Geom
    /// (origin 0, win == grid extents) so every embedder that never calls
    /// set_chrome stays byte-identical; padded chrome offsets the grid interior
    /// and grows the frame per the [head][pad][grid][pad] layout.
    #[test]
    fn chrome_geom_identity_and_padded_derivation() {
        let mut p = EffectsPipeline::new();
        let g = p.chrome_geom(24, 80, 9, 18);
        assert_eq!((g.origin_x, g.origin_y), (0, 0));
        assert_eq!((g.win_w, g.win_h), (80 * 9, 24 * 18));
        assert_eq!(g.head, 0);

        p.set_chrome(12, 30);
        let g = p.chrome_geom(24, 80, 9, 18);
        assert_eq!((g.origin_x, g.origin_y), (12, 42));
        assert_eq!((g.win_w, g.win_h), (80 * 9 + 24, 24 * 18 + 24 + 30));
        assert_eq!(g.head, 30);

        // Back to identity: 0/0 restores the historical layout exactly.
        p.set_chrome(0, 0);
        let g = p.chrome_geom(24, 80, 9, 18);
        assert_eq!((g.origin_x, g.origin_y, g.head), (0, 0, 0));
        assert_eq!((g.win_w, g.win_h), (80 * 9, 24 * 18));
    }

    /// The fire-stream splice: a FIRE-style glow burning on the top row under
    /// chrome must land per-pixel fire patches in the RenderInput — with the
    /// flame field rising ABOVE the grid into the head band (window-absolute
    /// coords from the same chrome_geom the tick used). This is what makes
    /// fire genuinely escape the grid in an embedder instead of being an
    /// app-only stream.
    #[test]
    fn fire_glow_splices_field_streams_into_the_head_band() {
        let (cell_w, cell_h) = (10usize, 19usize);
        let (pad, head) = (12u16, 30u16);
        let origin_y = pad + head; // grid top; the head band is above it
        let mut p = EffectsPipeline::new();
        p.set_chrome(pad, head);
        p.set_cursor_glow(
            true,
            "ember",
            None,
            None,
            400,
            64,
            1.0,
            2.0,
            true,
            0x00FF_8833,
        );
        let mut term = Terminal::new(12, 40);
        let (mut any_patch, mut band_patch, mut any_companion) = (false, false, false);
        // Type on row 0 (the burn ignites on observed cursor motion), then let
        // the field churn a few frames — flames rise off the row toward the
        // chrome-relaxed clamp (fx_top = pad).
        for _ in 0..3 {
            for ch in [b"a", b"b", b"c", b"d"] {
                term.process(ch);
                p.advance(30.0);
                let mut input = term.cell_frame(12, 40);
                p.apply(&mut term, &mut input, cell_w, cell_h);
                any_patch |= !input.fire_patch.is_empty();
                band_patch |= input.fire_patch.iter().any(|q| q.y < origin_y);
                // The splice carries the burn's companion streams too (frames
                // hot enough to engulf ink carry under-glow/charred/halo/rim).
                any_companion |= !input.glow_under.is_empty()
                    || !input.char_fg.is_empty()
                    || !input.glow_halo.is_empty()
                    || !input.fire_halo.is_empty();
            }
        }
        assert!(any_patch, "a burning fire must splice fire_patch quads");
        assert!(
            band_patch,
            "under chrome the flame field must rise above the grid (y < pad+head)"
        );
        assert!(
            any_companion,
            "a burn must splice at least one companion stream (halo/under/char/rim)"
        );
    }

    /// The identity law through the splice: an all-off apply() clears every
    /// fire/halo stream a reused scratch may carry, and an enabled NON-fire
    /// style never emits them — non-fire embedders stay byte-identical to the
    /// pre-splice frames.
    #[test]
    fn effects_off_and_non_fire_styles_leave_fire_streams_empty() {
        let mut p = EffectsPipeline::new();
        let mut term = Terminal::new(6, 20);
        let mut input = term.cell_frame(6, 20);
        // Simulate a reused scratch carrying stale fire-frame state.
        input.fire_patch.push(aterm_core::render::FirePatch {
            row: 0,
            x: 1,
            y: 1,
            w: 8,
            h: 8,
            base_y: 20,
            peak_h: 16,
            phase: 7,
            temp: 200,
            strength: 200,
            lean: 0,
            cov_cap: 200,
            cell_h: 19,
            mode: aterm_core::render::FireMode::Add,
        });
        input.glow_halo.push(RainHalo::default());
        input.glow_under.push(GlowQuad::default());
        input.char_fg.push(aterm_core::render::CharFg {
            row: 0,
            col: 0,
            fg: 0x00FF_0000,
        });
        input.fire_halo.push(aterm_core::render::FireHaloCell {
            row: 0,
            col: 0,
            strength: 128,
        });
        input.cursor_fill_override = Some(0x00FE_DCBA);
        let fp = p.apply(&mut term, &mut input, 10, 19);
        assert_eq!(fp, 0, "all-off apply is the identity fingerprint");
        assert!(input.fire_patch.is_empty(), "stale fire_patch cleared");
        assert!(input.glow_halo.is_empty(), "stale glow_halo cleared");
        assert!(input.glow_under.is_empty(), "stale glow_under cleared");
        assert!(input.char_fg.is_empty(), "stale char_fg cleared");
        assert!(input.fire_halo.is_empty(), "stale fire_halo cleared");
        assert_eq!(
            input.cursor_fill_override, None,
            "all-off apply clears a stale forge cursor fill"
        );

        // An enabled NON-fire style ticks the glow but emits no fire streams.
        p.set_cursor_glow(
            true,
            "lumen",
            None,
            None,
            400,
            24,
            0.9,
            0.9,
            true,
            0x0050_FA7B,
        );
        for ch in [b"a", b"b", b"c"] {
            term.process(ch);
            p.advance(30.0);
            input = term.cell_frame(6, 20);
            p.apply(&mut term, &mut input, 10, 19);
            assert!(input.fire_patch.is_empty(), "lumen never emits fire_patch");
            assert!(input.glow_under.is_empty(), "lumen never emits glow_under");
            assert!(input.char_fg.is_empty(), "lumen never emits char_fg");
            assert!(input.fire_halo.is_empty(), "lumen never emits fire_halo");
            assert_eq!(
                input.cursor_fill_override, None,
                "a non-fire style never inherits the forge cursor fill"
            );
        }
    }

    /// THE focus gate. The engine was built expecting the host to deliver
    /// unfocus as amplitude 0 (`cursor_glow`'s documented contract); native does
    /// so via its motion policy, the web path never did — so a split pane that
    /// is unfocused but still VISIBLE animated at full strength. The focused
    /// half runs first so the unfocused assertion can never pass vacuously.
    #[test]
    fn unfocused_pane_emits_no_glow_but_focused_one_does() {
        let mut p = EffectsPipeline::new();
        p.set_cursor_glow(
            true,
            "lumen",
            None,
            None,
            400,
            24,
            0.9,
            0.9,
            true,
            0x0050_FA7B,
        );
        let mut term = Terminal::new(6, 20);
        let (mut lit, mut fp_focused) = (false, 0u64);
        for ch in [b"a", b"b", b"c"] {
            term.process(ch);
            p.advance(30.0);
            let mut input = term.cell_frame(6, 20);
            fp_focused = p.apply(&mut term, &mut input, 10, 19);
            lit |= !input.cursor_glow_add.is_empty();
        }
        assert!(lit, "a focused pane emits cursor light");
        assert_ne!(fp_focused, 0, "emitted light folds a nonzero fingerprint");

        // Unfocused, still ticking, still MOVING: the gate must zero all of it.
        p.set_focused(false);
        for ch in [b"d", b"e", b"f"] {
            term.process(ch);
            p.advance(30.0);
            let mut input = term.cell_frame(6, 20);
            let fp = p.apply(&mut term, &mut input, 10, 19);
            assert!(
                input.cursor_glow_add.is_empty(),
                "an unfocused pane emits no aurora"
            );
            assert!(input.glow_halo.is_empty(), "no radial halo while unfocused");
            assert!(input.fire_patch.is_empty(), "no fire field while unfocused");
            assert!(
                input.glow_under.is_empty(),
                "no under-ink flame while unfocused"
            );
            assert!(input.char_fg.is_empty(), "no charred ink while unfocused");
            assert!(
                input.fire_halo.is_empty(),
                "no contrast halo while unfocused"
            );
            assert_eq!(fp, 0, "an unfocused pane folds the identity fingerprint");
        }
    }

    /// The perf half: a dark pane must genuinely SETTLE, or the host's shared
    /// rAF keeps re-booking it and the gate buys nothing. Non-Fire style — Fire
    /// deliberately keeps a bounded ember tail. Two gated ticks: the first arms
    /// the lazy-cooling clock, the second cools through it.
    #[test]
    fn unfocused_glow_settles_so_the_host_stops_rebooking_the_pane() {
        let mut p = EffectsPipeline::new();
        p.set_cursor_glow(
            true,
            "lumen",
            None,
            None,
            400,
            24,
            0.9,
            0.9,
            true,
            0x0050_FA7B,
        );
        let mut term = Terminal::new(6, 20);
        for ch in [b"a", b"b", b"c"] {
            term.process(ch);
            p.advance(30.0);
            let mut input = term.cell_frame(6, 20);
            p.apply(&mut term, &mut input, 10, 19);
        }
        assert!(p.is_active(), "a live focused glow holds the engine active");

        p.set_focused(false);
        for _ in 0..2 {
            p.advance(16.0);
            let mut input = term.cell_frame(6, 20);
            p.apply(&mut term, &mut input, 10, 19);
        }
        assert!(
            !p.is_active(),
            "an idle unfocused pane settles so the shared rAF can drop it"
        );
    }

    /// Refocus must not fire a comet across the ground the cursor covered while
    /// dark. The gated branches keep tracking the cursor precisely so that
    /// `last` is current on the way back — which is why a `reset()` here would
    /// be actively harmful (it nulls `last` and CREATES the phantom).
    #[test]
    fn refocus_resumes_without_a_phantom_comet() {
        let mut p = EffectsPipeline::new();
        p.set_cursor_trail(true, 260, 24, None, 0x0050_FA7B);
        let mut term = Terminal::new(6, 20);
        term.process(b"ab");
        p.advance(30.0);
        let mut input = term.cell_frame(6, 20);
        p.apply(&mut term, &mut input, 10, 19);

        // Travel a long way while dark.
        p.set_focused(false);
        for ch in [b"c", b"d", b"e", b"f", b"g", b"h"] {
            term.process(ch);
            p.advance(30.0);
            let mut input = term.cell_frame(6, 20);
            p.apply(&mut term, &mut input, 10, 19);
        }

        // Refocus WITHOUT moving: a tracked `last` means nothing to sweep.
        p.set_focused(true);
        p.advance(30.0);
        let mut input = term.cell_frame(6, 20);
        p.apply(&mut term, &mut input, 10, 19);
        assert!(
            input.cursor_trail.is_empty(),
            "refocus at a stationary cursor sweeps no cells"
        );
    }

    /// The trail's gated branch is a hard clear (native does the identical
    /// `enabled &=`), but the trail COLOUR is a config value, not animation —
    /// it must keep being published so an unfocused frame carries a defined
    /// colour rather than a stale one. Cadence is cold here, and `ignite` leaves
    /// the colour untouched at intensity 0, so the published value is exact.
    #[test]
    fn unfocused_trail_clears_its_comet_but_still_publishes_the_colour() {
        let mut p = EffectsPipeline::new();
        p.set_cursor_trail(true, 260, 24, None, 0x0050_FA7B);
        let mut term = Terminal::new(6, 20);
        let mut swept = false;
        for ch in [b"a", b"b", b"c"] {
            term.process(ch);
            p.advance(30.0);
            let mut input = term.cell_frame(6, 20);
            p.apply(&mut term, &mut input, 10, 19);
            swept |= !input.cursor_trail.is_empty();
        }
        assert!(swept, "a focused pane sweeps a comet");
        assert!(p.trail.is_active(), "sparks are live before the gate");

        // One SHORT dark frame: well inside the 260 ms spark life, so an ungated
        // trail would still be carrying a live comet here. Fresh input each
        // frame, so a skipped colour publish leaves the default rather than a
        // stale value from the focused run.
        p.set_focused(false);
        p.advance(30.0);
        let mut input = term.cell_frame(6, 20);
        p.apply(&mut term, &mut input, 10, 19);
        assert!(
            input.cursor_trail.is_empty(),
            "the gate clears the comet while its sparks would still be alive"
        );
        assert!(
            !p.trail.is_active(),
            "the trail settles on its first dark tick"
        );
        assert_ne!(
            input.cursor_trail_color, 0,
            "the colour is still published while unfocused"
        );

        // Once the cadence goes cold `ignite` leaves the colour untouched, so
        // the published value is exactly the configured one.
        p.advance(1000.0);
        let mut input = term.cell_frame(6, 20);
        p.apply(&mut term, &mut input, 10, 19);
        assert_eq!(
            input.cursor_trail_color, 0x0050_FA7B,
            "the colour is keyed on the stored flag, not the focus-gated local"
        );
    }

    /// SCOPE GUARD. Word decorations are deliberately NOT focus-gated: the focus
    /// deferral was removed 2026-07-17 because entrances replayed en masse on
    /// refocus, and `latch-at-birth` replaced it. Two pipelines driven in
    /// lockstep — one focused, one not — must emit identical decoration frames.
    /// If this goes red, someone has re-gated decorations and reopened that bug.
    #[test]
    fn focus_does_not_alter_word_decoration_emission() {
        let drive = |focused: bool| {
            let mut p = EffectsPipeline::new();
            p.set_sparkle_enabled(true);
            p.set_focused(focused);
            let mut term = Terminal::new(4, 24);
            term.process(b"\r\n\r\na nice kitty");
            let mut input = RenderInput::default();
            p.advance(10.0);
            term.cell_frame_into(&mut input, 4, 24);
            p.apply(&mut term, &mut input, 10, 20);
            let mut frames = Vec::new();
            for _ in 0..60 {
                p.advance(100.0);
                term.cell_frame_into(&mut input, 4, 24);
                p.apply(&mut term, &mut input, 10, 20);
                frames.push((
                    input.free_sprites.len(),
                    input.word_decorations.clone(),
                    input.ink.clone(),
                ));
            }
            frames
        };
        let focused = drive(true);
        let unfocused = drive(false);
        assert!(
            focused.iter().any(|(sprites, _, _)| *sprites > 0),
            "the focused cat actually played (else this proves nothing)"
        );
        assert_eq!(
            focused, unfocused,
            "decorations are out of scope for the focus gate (latch-at-birth, 2026-07-17)"
        );
    }

    /// The gate scales a COPY. Zeroing the stored intensity would silently
    /// destroy the user's configured value — only `set_cursor_glow` ever
    /// republishes it, and the host posts that on a settings change alone.
    #[test]
    fn unfocusing_never_consumes_the_configured_glow_intensity() {
        let mut p = EffectsPipeline::new();
        p.set_cursor_glow(
            true,
            "lumen",
            None,
            None,
            400,
            24,
            0.9,
            0.9,
            true,
            0x0050_FA7B,
        );
        let configured = p.glow_cfg.intensity;
        let mut term = Terminal::new(6, 20);
        p.set_focused(false);
        for ch in [b"a", b"b", b"c"] {
            term.process(ch);
            p.advance(30.0);
            let mut input = term.cell_frame(6, 20);
            p.apply(&mut term, &mut input, 10, 19);
        }
        assert_eq!(
            p.glow_cfg.intensity, configured,
            "the stored intensity survives an unfocused stretch byte-unchanged"
        );

        // And it comes back at full strength.
        p.set_focused(true);
        let mut relit = false;
        for ch in [b"d", b"e", b"f"] {
            term.process(ch);
            p.advance(30.0);
            let mut input = term.cell_frame(6, 20);
            p.apply(&mut term, &mut input, 10, 19);
            relit |= !input.cursor_glow_add.is_empty();
        }
        assert!(relit, "refocus restores the light");
    }

    /// The embedded hosts use this pipeline as their sole overlay producer, so
    /// Fire's warm-metal cursor body must ride the same RenderInput field as it
    /// does in the native GUI. A live style toggle must scrub it immediately.
    #[test]
    fn fire_forge_cursor_fill_is_spliced_and_cleared_on_toggle() {
        let mut p = EffectsPipeline::new();
        p.set_cursor_glow(
            true,
            "fire",
            None,
            None,
            400,
            24,
            1.0,
            0.9,
            true,
            0x0050_FA7B,
        );
        let mut term = Terminal::new(6, 20);
        let mut input = term.cell_frame(6, 20);
        p.apply(&mut term, &mut input, 10, 19);
        assert_eq!(
            input.cursor_fill_override,
            p.glow.forge_fill(),
            "Fire publishes its warm-metal cursor fill"
        );
        assert!(
            input.cursor_fill_override.is_some(),
            "Fire's cold forge fill is deliberately still visible"
        );

        p.set_cursor_glow(
            true,
            "lumen",
            None,
            None,
            400,
            24,
            1.0,
            0.9,
            true,
            0x0050_FA7B,
        );
        term.cell_frame_into(&mut input, 6, 20);
        p.apply(&mut term, &mut input, 10, 19);
        assert_eq!(
            input.cursor_fill_override, None,
            "switching away from Fire clears the reused frame's fill"
        );
    }

    fn composer_toggle_model() -> aterm_spec::derive::Model {
        use aterm_spec::ty_model;
        ty_model! {
            ComposerToggleGate {
                const Buggy = 0;
                var host = 0;
                var present = 0;
                var engine = 0;
                action Key {
                    host = 1;
                    engine = if present == 1 { 1 } else { 0 };
                }
                action TurnStart {
                    host = 0;
                    engine = 0;
                }
                action Enable {
                    present = 1;
                    engine = if Buggy == 1 { 0 } else { host };
                }
                action Disable {
                    present = 0;
                    engine = 0;
                }
                invariant BooleanBounds: host <= 1 && present <= 1 && engine <= 1;
                invariant EngineMatchesHost: engine == if present == 1 { host } else { 0 };
            }
        }
    }

    #[test]
    fn composer_toggle_model_proves_and_real_pipeline_conforms() {
        let model = composer_toggle_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
        let mut state = model.init_state();
        let mut pipeline = EffectsPipeline::new();
        for action in ["Key", "Enable", "Disable", "Enable", "TurnStart", "Key"] {
            match action {
                "Key" => pipeline.note_keystroke(),
                "TurnStart" => pipeline
                    .note_matrix_rain_signal(crate::matrix_rain::RainSignal::TurnStart as u32, 4),
                "Enable" => pipeline.set_matrix_rain_enabled(true),
                "Disable" => pipeline.set_matrix_rain_enabled(false),
                _ => unreachable!(),
            }
            assert!(model.fire(action, &mut state));
            assert_eq!(i64::from(pipeline.rain_material_editing), state["host"]);
            assert_eq!(i64::from(pipeline.rain.is_some()), state["present"]);
            let engine_editing = pipeline
                .rain
                .as_deref()
                .is_some_and(MatrixRain::material_editing_for_test);
            assert_eq!(i64::from(engine_editing), state["engine"]);
        }
    }

    /// v3 §4 web-resolver pin: `set_sparkle_classes` ANDs the orca gate with
    /// `!ORCA_SUSPENDED` (tied to `aterm_effects::ORCA_SUSPENDED` — flip the
    /// const to re-enable and this assertion inverts). Plus the §6 web
    /// surface: `set_sparkle_custom_specs` registers overrides, keeps the
    /// emphasis class scanning with ink off, and the custom rainbow word
    /// actually inks end-to-end through `apply`.
    #[test]
    fn v3_web_resolver_pins_orca_suspension_and_custom_specs() {
        let mut p = EffectsPipeline::new();
        p.set_sparkle_enabled(true);
        p.set_sparkle_classes(true, true, true, true);
        let cfg = &p.sparkle.as_ref().expect("on").cfg;
        assert!(
            !cfg.orca,
            "web resolver: orca gate ANDs !ORCA_SUSPENDED (v3 §4)"
        );
        assert!(cfg.profanity && cfg.feline && cfg.emphasis);
        // §6 gate: ink off kills emphasis only while NO custom specs exist.
        p.set_sparkle_ink(false, 0.75, 2200, false);
        assert!(!p.sparkle.as_ref().unwrap().cfg.emphasis);
        p.set_sparkle_custom_specs(Some(
            "[[sparkle_words.custom]]\nwords = [\"ultrathink\"]\nink = { colorway = \"rainbow\" }\n"
                .to_string(),
        ));
        let cfg = &p.sparkle.as_ref().unwrap().cfg;
        assert!(
            cfg.emphasis && cfg.spec_table.has_custom(),
            "custom specs keep emphasis scanning with ink off (v3 §6)"
        );
        // End-to-end: ink back on, the custom rainbow word inks via apply.
        p.set_sparkle_ink(true, 0.75, 2200, false);
        let mut term = Terminal::new(4, 32);
        term.process(b"go ultrathink now");
        let mut input = RenderInput::default();
        p.advance(10.0);
        term.cell_frame_into(&mut input, 4, 32);
        p.apply(&mut term, &mut input, 10, 20);
        assert_eq!(
            input.ink.len(),
            10,
            "the custom rainbow word inks its 10 lead cells"
        );
        // Malformed fragment: the previous specs are KEPT (a bad edit must not
        // silently wipe a working config) and the parse error surfaces on the
        // lexicon-warnings channel — the Toy-Pack diagnostics posture.
        p.set_sparkle_custom_specs(Some("[[sparkle_words.custom".to_string()));
        assert!(
            p.sparkle.as_ref().unwrap().cfg.spec_table.has_custom(),
            "malformed fragment keeps the previous specs"
        );
        assert!(
            p.sparkle_lexicon_warnings()
                .iter()
                .any(|w| w.starts_with("sparkle_words.custom:")),
            "the parse diagnostic surfaces instead of vanishing"
        );
        // An explicit `None` clears the specs AND the stale diagnostic.
        p.set_sparkle_custom_specs(None);
        assert!(!p.sparkle.as_ref().unwrap().cfg.spec_table.has_custom());
        assert!(
            p.sparkle_lexicon_warnings().is_empty(),
            "a clear drops the malformed-fragment warning"
        );
    }

    /// FIX 7 + FIX 8 (web path) regression: `rebuild_sparkle` no longer drops
    /// `Lexicon::conflicts()` — the pipeline captures them and surfaces them
    /// via `sparkle_lexicon_warnings()`; and the single-char-CJK warning is
    /// filtered out once the resolved config enables `cjk_single_char` (the
    /// surface then actually scans, so the "requires the opt-in" diagnostic
    /// no longer applies), while unrelated warnings survive the filter.
    #[test]
    fn lexicon_warnings_surface_and_respect_cjk_single_char_opt_in() {
        let mut p = EffectsPipeline::new();
        assert!(
            p.sparkle_lexicon_warnings().is_empty(),
            "off: no lexicon, no warnings"
        );
        p.set_sparkle_enabled(true);
        assert!(
            p.sparkle_lexicon_warnings().is_empty(),
            "the builtin lexicon is conflict-free"
        );
        // A single-char CJK custom word (scans only under the opt-in) plus a
        // mixed-script one (dropped at insert, can never scan).
        p.set_sparkle_custom_specs(Some(
            "[[sparkle_words.custom]]\nwords = [\"犬\", \"abc猫\"]\nink = { colorway = \"rainbow\" }\n"
                .to_string(),
        ));
        let warns = p.sparkle_lexicon_warnings();
        assert!(
            warns
                .iter()
                .any(|w| w.contains("\"犬\"") && w.contains("requires cjk_single_char = true")),
            "FIX 7: the web path surfaces the single-char-CJK conflict, got {warns:?}"
        );
        assert!(
            warns
                .iter()
                .any(|w| w.contains("\"abc猫\"") && w.contains("dropped")),
            "FIX 7: the web path surfaces the mixed-script drop, got {warns:?}"
        );
        // FIX 8: enable the opt-in (a cfg refresh, not a lexicon rebuild) —
        // the requires-warning is satisfied and disappears; the genuinely
        // unscannable mixed-script warning stays.
        p.set_sparkle_feline("cat", true, true, true);
        let warns = p.sparkle_lexicon_warnings();
        assert!(
            !warns
                .iter()
                .any(|w| w.contains("requires cjk_single_char = true")),
            "FIX 8: cjk_single_char = true satisfies the warning, got {warns:?}"
        );
        assert!(
            warns
                .iter()
                .any(|w| w.contains("\"abc猫\"") && w.contains("dropped")),
            "unrelated warnings survive the opt-in filter, got {warns:?}"
        );
        // Flipping the opt-in back off re-applies it (filtered at read, so a
        // knob change never needs a rebuild to stay honest).
        p.set_sparkle_feline("cat", true, true, false);
        assert!(
            p.sparkle_lexicon_warnings()
                .iter()
                .any(|w| w.contains("requires cjk_single_char = true"))
        );
        // Master off drops the diagnostics with the lexicon they describe.
        p.set_sparkle_enabled(false);
        assert!(p.sparkle_lexicon_warnings().is_empty());
    }

    /// v3 §1.1 reset-table parity (native vs pipeline): the web knob setters
    /// are `hard_reset()` — parity with the native config-reload arm — so a
    /// finished one-shot REPLAYS after a knob change; a suppressed-alt-screen
    /// round-trip is freeze/thaw (episodes resume, done marks untouched).
    /// The native side of the table lives in aterm-gui (app_render:
    /// perf_reduced + suppressed-alt = freeze/thaw, master off = hard_reset;
    /// app_config: toggle + reload = hard_reset) — this pins the pipeline
    /// mirror of the same rows through observable engine state.
    #[test]
    fn v3_reset_table_knob_setters_hard_reset_and_alt_suppression_freezes() {
        let mut p = EffectsPipeline::new();
        p.set_sparkle_enabled(true);
        // Configured up front: a knob setter is itself a hard_reset, so the
        // freeze/thaw stanza below must not flip knobs mid-episode.
        p.set_sparkle_alt_screen_suppression(true);
        p.set_focused(true);
        let mut term = Terminal::new(4, 24);
        term.process(b"\r\n\r\na nice kitty");
        let mut input = RenderInput::default();

        // Drive the one-shot to Done: the peek worst case is < 5 s.
        p.advance(10.0);
        term.cell_frame_into(&mut input, 4, 24);
        p.apply(&mut term, &mut input, 10, 20);
        let mut saw_cat = false;
        for _ in 0..60 {
            p.advance(100.0);
            term.cell_frame_into(&mut input, 4, 24);
            p.apply(&mut term, &mut input, 10, 20);
            saw_cat |= !input.free_sprites.is_empty();
        }
        assert!(saw_cat, "the peek actually played");
        assert!(
            input.free_sprites.is_empty(),
            "the one-shot is spent (zero sprites at rest)"
        );

        // Knob setter = hard_reset: the SAME word plays again.
        p.set_sparkle_feline("cat", true, true, false);
        p.advance(10.0);
        term.cell_frame_into(&mut input, 4, 24);
        p.apply(&mut term, &mut input, 10, 20); // release present (reveal 0)
        let mut replayed = false;
        for _ in 0..10 {
            p.advance(100.0);
            term.cell_frame_into(&mut input, 4, 24);
            p.apply(&mut term, &mut input, 10, 20);
            replayed |= !input.free_sprites.is_empty();
        }
        assert!(
            replayed,
            "web knob setters are hard_reset(): the one-shot replays (§1.1 table)"
        );

        // Suppressed alt screen = freeze/thaw: the mid-peek cat RESUMES (it
        // neither restarts nor silently completes while suppressed).
        let sprites_before = input.free_sprites.clone();
        assert!(
            !sprites_before.is_empty(),
            "mid-peek before the alt round-trip"
        );
        term.process(b"\x1b[?1049h"); // enter the alternate screen
        p.advance(100.0);
        term.cell_frame_into(&mut input, 4, 24);
        p.apply(&mut term, &mut input, 10, 20);
        assert!(input.free_sprites.is_empty(), "suppressed: no decorations");
        term.process(b"\x1b[?1049l"); // long suspension, then back
        p.advance(5000.0);
        term.cell_frame_into(&mut input, 4, 24);
        p.apply(&mut term, &mut input, 10, 20);
        assert_eq!(
            input.free_sprites, sprites_before,
            "thaw resumes the peek exactly where it froze (no mass replay)"
        );
    }

    /// A 24×80 snapshot of `term` whose cells are all MATERIALIZED default-bg
    /// spaces (every cell Tier-A rain-eligible). A fresh terminal's rows are
    /// unmaterialized — `cell_frame_into` yields empty row Vecs, which scan as
    /// ineligible — so the real stamp runs first (geometry, cursor, selection,
    /// `snapshot_seq`) and only the cells are synthesized.
    fn synthetic_empty_frame(term: &mut Terminal, input: &mut RenderInput) {
        use aterm_core::terminal::{RenderCell, UnderlineStyle};
        term.cell_frame_into(input, 24, 80);
        let bg = input.default_bg;
        let space = RenderCell {
            ch: ' ',
            fg: [0xD0, 0xD0, 0xD0],
            bg: [(bg >> 16) as u8, (bg >> 8) as u8, bg as u8],
            wide: false,
            emoji_presentation: false,
            bold: false,
            italic: false,
            underline: UnderlineStyle::None,
            strikethrough: false,
            overline: false,
            underline_color: None,
        };
        for row in &mut input.cells {
            row.clear();
            row.resize(80, space);
        }
    }

    fn enable_classic_rain(pipeline: &mut EffectsPipeline) {
        // These lifecycle tests intentionally exercise the opt-in decorative
        // ROM on an empty grid; literal mode correctly needs sampled output.
        pipeline.rain_cfg.output_material = false;
        pipeline.set_matrix_rain_enabled(true);
    }

    /// Drive an enabled-rain pipeline on the all-eligible synthetic grid until
    /// quads land in `input` (CALM ticks at 12 Hz — a few seconds suffice;
    /// per-frame keystrokes hold CALM against the idle→SLEEP mandate).
    fn rain_until_visible(p: &mut EffectsPipeline, term: &mut Terminal, input: &mut RenderInput) {
        for _ in 0..120 {
            p.note_keystroke();
            p.advance(50.0);
            synthetic_empty_frame(term, input);
            p.apply(term, input, 10, 20);
            if !input.rain_quads.is_empty() {
                return;
            }
        }
        panic!("rain never appeared on an empty 24x80 grid");
    }

    /// PHOSPHOR zero-cost-off: with rain disabled (the default) `apply` leaves
    /// the rain channels exactly as `clear_overlays` does (empty/None) on both
    /// the all-off early return and the enabled path, and the rain field
    /// contributes nothing to `is_active`.
    #[test]
    fn rain_disabled_leaves_channels_clear_and_is_active_unaffected() {
        let mut p = EffectsPipeline::new();
        assert!(!p.is_active(), "everything off: inactive");
        assert!(!p.matrix_rain_enabled());
        let mut term = Terminal::new(24, 80);
        let mut input = RenderInput::default();
        term.cell_frame_into(&mut input, 24, 80);
        let fp = p.apply(&mut term, &mut input, 10, 20);
        assert_eq!(fp, 0, "all-off apply is fingerprint 0");
        assert!(input.rain_quads.is_empty());
        assert!(input.rain_add.is_empty());
        assert!(input.rain_atlas.is_none());
        assert!(!p.is_active(), "rain-off never animates the pipeline");
        // Enabled path (another effect on), rain still absent: same posture.
        p.set_cursor_trail(true, 260, 24, None, 0x0050_FA7B);
        p.advance(50.0);
        term.cell_frame_into(&mut input, 24, 80);
        p.apply(&mut term, &mut input, 10, 20);
        assert!(input.rain_quads.is_empty());
        assert!(input.rain_add.is_empty());
        assert!(input.rain_atlas.is_none());
    }

    /// PHOSPHOR: enable + advance + apply rains on an empty grid (empty cells
    /// are Tier-A eligible), the atlas rides only alongside output, and a
    /// repeat `apply` with NO `advance` is byte-stable — same quads, same
    /// fold — the fp-driven repaint-key contract.
    #[test]
    fn rain_enabled_emits_and_repeat_apply_is_byte_stable() {
        let mut p = EffectsPipeline::new();
        enable_classic_rain(&mut p);
        assert!(p.matrix_rain_enabled());
        assert!(p.is_active(), "CALM weather holds the engine active");
        let mut term = Terminal::new(24, 80);
        let mut input = RenderInput::default();
        rain_until_visible(&mut p, &mut term, &mut input);
        assert!(input.rain_atlas.is_some(), "the atlas rides with the quads");
        let quads = input.rain_quads.clone();
        let adds = input.rain_add.clone();
        let atlas_version = input.rain_atlas.as_ref().map(|a| a.version);
        let fp1 = p.apply(&mut term, &mut input, 10, 20);
        let fp2 = p.apply(&mut term, &mut input, 10, 20);
        assert_ne!(fp1, 0, "nonempty emission folds a nonzero fp");
        assert_eq!(fp1, fp2, "no advance ⇒ stable fold");
        assert_eq!(input.rain_quads, quads, "byte-stable quads");
        assert_eq!(input.rain_add, adds, "byte-stable halos");
        assert_eq!(
            input.rain_atlas.as_ref().map(|a| a.version),
            atlas_version,
            "no tick ⇒ no rebake"
        );
    }

    #[test]
    fn rain_only_uses_engine_deadline_while_frame_effects_keep_raf() {
        let mut p = EffectsPipeline::new();
        enable_classic_rain(&mut p);
        assert_eq!(p.next_deadline_ms(), Some(83.0), "CALM rain is 12 Hz");

        p.advance(30.0);
        assert_eq!(
            p.next_deadline_ms(),
            Some(53.0),
            "unrelated redraws retain the partial rain period"
        );

        p.set_cursor_trail(true, 260, 24, None, 0x0050_FA7B);
        let now = p.now();
        p.trail
            .tick(Some((4, 4)), now, &p.trail_cfg, &mut p.trail_scratch);
        p.advance(1.0);
        let now = p.now();
        p.trail
            .tick(Some((4, 8)), now, &p.trail_cfg, &mut p.trail_scratch);
        assert!(p.is_active());
        assert_eq!(
            p.next_deadline_ms(),
            None,
            "a live motion effect still requests display-rAF cadence"
        );
    }

    /// The shared CPU/wasm/GPU-web pipeline retains its prior REAL output tape
    /// throughout an arbitrarily tall unsent edit. This is stronger than a
    /// cursor-radius heuristic: seven visible draft rows cannot enter material,
    /// including the rows more than five lines from the visible cursor. Submit
    /// releases the gate and a subsequent real response refreshes the tape.
    #[test]
    fn embedded_pipeline_freezes_literal_tape_across_multiline_draft() {
        let mut p = EffectsPipeline::new();
        p.set_matrix_rain_enabled(true);
        let mut term = Terminal::new(24, 80);
        let mut input = RenderInput::default();
        term.process(b"\x1b[HREAL OUTPUT 2468\x1b[24;1H");
        term.cell_frame_into(&mut input, 24, 80);
        p.apply(&mut term, &mut input, 10, 20);

        let initial = p
            .rain
            .as_deref()
            .expect("enabled rain engine")
            .literal_material_chars_for_test()
            .to_vec();
        for required in ['R', 'E', 'A', 'L', 'O', 'U', 'T', '2', '4', '6', '8'] {
            assert!(
                initial.contains(&required),
                "initial tape contains real output glyph {required:?}"
            );
        }

        for row in 10..17 {
            p.note_keystroke();
            term.process(format!("\x1b[{};1H> private zqjx draft 13579", row + 1).as_bytes());
            term.cell_frame_into(&mut input, 24, 80);
            p.apply(&mut term, &mut input, 10, 20);
        }
        let during_edit = p
            .rain
            .as_deref()
            .expect("enabled rain engine")
            .literal_material_chars_for_test();
        assert_eq!(
            during_edit,
            initial.as_slice(),
            "unsent edits freeze the tape"
        );
        for draft_only in ['p', 'r', 'i', 'v', 'a', 't', 'e', 'z', 'q', 'j', 'x'] {
            assert!(
                !during_edit.contains(&draft_only),
                "draft-only glyph {draft_only:?} never enters literal material"
            );
        }

        p.note_matrix_rain_signal(crate::matrix_rain::RainSignal::TurnStart as u32, 4);
        term.process(b"\x1b[2J\x1b[HREAL response z\x1b[24;1H");
        term.cell_frame_into(&mut input, 24, 80);
        p.apply(&mut term, &mut input, 10, 20);
        let after_submit = p
            .rain
            .as_deref()
            .expect("enabled rain engine")
            .literal_material_chars_for_test();
        assert!(
            after_submit.contains(&'z'),
            "TurnStart re-enables sampling for subsequent real output"
        );
    }

    #[test]
    fn live_classic_to_literal_switch_resamples_without_grid_damage() {
        let mut p = EffectsPipeline::new();
        p.set_matrix_rain(
            30,
            6,
            4,
            6,
            None,
            None,
            "matrix",
            None,
            320,
            12,
            false,
            true,
            true,
            false,
            7,
            0x0011_1318,
            0x00D0_D0D0,
        );
        p.set_matrix_rain_enabled(true);
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[HREAL output\x1b[24;1H> draft\x1b[24;8H");
        let mut input = RenderInput::default();
        term.cell_frame_into(&mut input, 24, 80);
        p.apply(&mut term, &mut input, 10, 20);
        let epoch = term.damage_epoch();

        p.set_matrix_rain(
            30,
            6,
            4,
            6,
            None,
            None,
            "matrix",
            None,
            320,
            12,
            false,
            true,
            true,
            true,
            7,
            0x0011_1318,
            0x00D0_D0D0,
        );
        term.cell_frame_into(&mut input, 24, 80);
        assert_eq!(input.snapshot_seq, epoch, "fixture has no new grid damage");
        p.apply(&mut term, &mut input, 10, 20);
        let chars = p
            .rain
            .as_deref()
            .expect("enabled rain")
            .literal_material_chars_for_test();
        assert!(chars.contains(&'R') && chars.contains(&'o'));
    }

    #[test]
    fn matrix_rain_theme_updates_cached_and_live_config() {
        let mut p = EffectsPipeline::new();
        p.set_matrix_rain_enabled(true);
        p.set_matrix_rain_theme(0x0012_3456, 0x00AB_CDEF);
        assert_eq!(p.rain_cfg.default_bg, 0x0012_3456);
        assert_eq!(p.rain_cfg.theme_fg, 0x00AB_CDEF);
        let live = p.rain.as_deref().expect("enabled rain").config_for_test();
        assert_eq!(live.default_bg, 0x0012_3456);
        assert_eq!(live.theme_fg, 0x00AB_CDEF);
    }

    #[test]
    fn scrollback_reading_skips_scan_and_live_return_rescans() {
        let mut p = EffectsPipeline::new();
        enable_classic_rain(&mut p);
        let mut term = Terminal::new(6, 20);
        for line in 0..20 {
            term.process(format!("line {line}\r\n").as_bytes());
        }
        let mut input = RenderInput::default();
        term.cell_frame_into(&mut input, 6, 20);
        p.apply(&mut term, &mut input, 8, 16);
        let live_epoch = term.damage_epoch();
        assert!(!p.rain.as_deref().unwrap().needs_rescan(live_epoch));

        term.scroll_display(3);
        term.cell_frame_into(&mut input, 6, 20);
        let scrolled_epoch = term.damage_epoch();
        p.apply(&mut term, &mut input, 8, 16);
        assert!(
            p.rain.as_deref().unwrap().needs_rescan(scrolled_epoch),
            "translated scrollback frame is never copied or scanned"
        );

        term.scroll_display(-1000);
        term.cell_frame_into(&mut input, 6, 20);
        let returned_epoch = term.damage_epoch();
        p.apply(&mut term, &mut input, 8, 16);
        assert!(
            !p.rain.as_deref().unwrap().needs_rescan(returned_epoch),
            "returning live rebuilds from the fresh coherent frame"
        );
    }

    /// PHOSPHOR live-toggle: every off path scrubs stale rain from a reused
    /// snapshot — the `!enabled_any` early return, and the rain-absent arm
    /// while another effect keeps the enabled path alive.
    #[test]
    fn rain_toggle_off_clears_stale_channels_on_every_off_path() {
        let mut p = EffectsPipeline::new();
        enable_classic_rain(&mut p);
        let mut term = Terminal::new(24, 80);
        let mut input = RenderInput::default();
        rain_until_visible(&mut p, &mut term, &mut input);

        // Master off with nothing else enabled: the early return scrubs.
        p.set_matrix_rain_enabled(false);
        assert!(!p.is_active(), "dropping the engine disarms the host loop");
        let fp = p.apply(&mut term, &mut input, 10, 20);
        assert_eq!(fp, 0);
        assert!(input.rain_quads.is_empty());
        assert!(input.rain_add.is_empty());
        assert!(input.rain_atlas.is_none());

        // Rain off while the glow keeps the enabled path alive: the
        // rain-absent arm scrubs the reused snapshot the same way.
        p.set_matrix_rain_enabled(true);
        rain_until_visible(&mut p, &mut term, &mut input);
        p.set_matrix_rain_enabled(false);
        p.set_cursor_glow(
            true,
            "lumen",
            None,
            None,
            260,
            24,
            0.7,
            0.6,
            true,
            0x0050_FA7B,
        );
        p.advance(16.0);
        term.cell_frame_into(&mut input, 24, 80);
        p.apply(&mut term, &mut input, 10, 20);
        assert!(input.rain_quads.is_empty());
        assert!(input.rain_add.is_empty());
        assert!(input.rain_atlas.is_none());
    }

    /// PHOSPHOR torn frame: a snapshot the grid outran (`snapshot_seq` trails
    /// the live damage epoch) emits no rain and scrubs the channels; the next
    /// fresh snapshot re-anchors and emission resumes.
    #[test]
    fn rain_torn_snapshot_clears_channels_and_fresh_frame_resumes() {
        let mut p = EffectsPipeline::new();
        enable_classic_rain(&mut p);
        let mut term = Terminal::new(24, 80);
        let mut input = RenderInput::default();
        rain_until_visible(&mut p, &mut term, &mut input);

        term.process(b"tear"); // grid damage AFTER the snapshot: `input` is stale
        p.apply(&mut term, &mut input, 10, 20);
        assert!(input.rain_quads.is_empty(), "torn frame: no rain quads");
        assert!(input.rain_add.is_empty(), "torn frame: no rain halos");
        assert!(input.rain_atlas.is_none(), "torn frame: no atlas rides");

        // A fresh snapshot re-anchors (rescan re-fires on the new epoch).
        rain_until_visible(&mut p, &mut term, &mut input);
    }
}
