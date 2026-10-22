// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Style resolution: the selectable looks ([`GlowStyle`]), the spelling
//! predicates that read the `spellings` table, and the resolved per-frame
//! [`GlowConfig`]. The spellings and aliases themselves live in
//! `cursor_glow::spellings`.

use super::*;

/// The selectable look of the aurora.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum GlowStyle {
    /// Directional tracer in the cursor colour fading to the accent + soft bloom.
    #[default]
    Lumen,
    /// Phaser — a full-spectrum hue sweep along the comet and over time. Reads as
    /// a laser/phaser sweep, not the flowing rainbow the kitty delivers.
    Phaser,
    /// Rainbow kitty: a momentum-driven continuous rainbow ribbon streaming the
    /// swept path, brighter and longer with typing heat, with a sparse
    /// twinkling starfield. The default presentation is the tall body: one
    /// smooth seven-anchor spectrum filling every swept glyph cell with the
    /// letters inside it (`… tall` spellings are aliases of the default);
    /// `… underline` spellings select the quieter highlighter plus
    /// under-baseline shoulder.
    RainbowKitty,
    /// A wide ribbon in one rolling hue (the whole streak steps round the wheel
    /// a little each key; a multi-cell move sweeps half the wheel tail to
    /// head) on its own tight bloom stack, plus a shower of night-sky grains:
    /// round stardust and glitter, the odd crescent moon and mini-comet, and a
    /// dealt four-point star. On a light ground the grains are ink and keep
    /// off the letters.
    Sparkle,
    /// Black-body warm comet + rising, drifting embers.
    Fire,
    /// Lightning: a monochrome beam in storm violet ([`LASER_DEFAULT_COLOR`];
    /// an explicit `cursor_trail_color` wins), its dim tail brightening to the
    /// pure hue at the head around a white-hot filament core, a jagged strike
    /// (white-hot main channel, same-hue forks) on a jump and short same-hue
    /// crackle arcs off a hot typing run. Every bolt flashes once, and at most
    /// three are born in any second. On a light ground the strike and crackles
    /// are painted beneath the letters in a darkened ink of the hue, with no
    /// white. Settings labels it "Lightning".
    Laser,
    /// Beam: a clean, steady tube of cool light with near-constant power along
    /// its length, no particles, scintillation or lightning. Instead of fading
    /// in place it powers down, thinning toward a hairline like an emitter
    /// switching off.
    Beam,
    /// Fluid ocean wake — a deep-blue undertow under a thin travelling cyan
    /// crest — with droplets that spray up and arc back down under gravity.
    /// On a light ground the wake is painted beneath the letters in the
    /// ramp's darkened ink, and a landing lays the shared veil ripple.
    Water,
    /// Comet: an icy dust tail (white-hot at the head, freezing back through the
    /// base hue to a dusty tail) plus twinkling debris glitter shed along the
    /// swept path. (The raw style `lumen` keeps the plain [`Self::Lumen`]
    /// tracer.)
    Comet,
    /// The classic wake: a thin four-layer comet under a soft square box-stack
    /// bloom, with a landing ring on a jump, and a comet across screen-crossing
    /// jumps. Its art and animation state live in [`crate::classic_wake`],
    /// reached by one branch in [`CursorGlow::tick`], so no other style's output
    /// depends on it.
    Classic,
}

impl GlowStyle {
    /// Whether this style string asks for the full-body pet companion
    /// ([`crate::kitty_pet`]) rather than the flying kitty.
    ///
    /// A predicate rather than a [`GlowStyle`] variant: every spelling here
    /// resolves to [`GlowStyle::RainbowKitty`], so every style-keyed table
    /// (ribbon geometry, starfield, sound palette, momentum) is shared and the
    /// two companions differ only where one is drawn.
    ///
    /// Read through [`spellings::canonical`]: true for the three canonical
    /// rows that draw the cat (`rainbow kitty pet`, `rainbow kitty underline`,
    /// `rainbow kitty flat`) and every alias of them, which includes
    /// `rainbow kitty`, bare `kitty` and the `… tall` spellings. A style named
    /// kitty draws the cat, and the underline and flat bodies are geometries,
    /// not animals. The flying head keeps `rainbow kitty flying` and the
    /// historical `nyan` / `rainbow` / `nyan rainbow` aliases.
    ///
    /// Both animals are never on glass at once: `app_render::trail_is_kitty_pet`
    /// ([`Self::style_names_any_pet`]) feeds `pet_mode`, the flying head is
    /// admitted only outside pet mode, and `retire_kitty_cursor_without_owner`
    /// grounds a head already on glass once `pet_mode && sing <= 0.0`, including
    /// on a hot config reload. (The song tenure, `sing > 0.0`, is a bounded
    /// promise the pet does not own.)
    #[inline]
    #[must_use]
    pub fn style_names_kitty_pet(s: &str) -> bool {
        matches!(
            spellings::canonical(s),
            Some("rainbow kitty pet" | "rainbow kitty underline" | "rainbow kitty flat")
        )
    }

    /// Whether this style string explicitly asks for the flying kitty head
    /// ([`crate::kitty_cursor`]), the rare earned flypast, on the same rainbow
    /// ribbon.
    ///
    /// Not consulted by the draw path (which asks
    /// [`Self::style_names_any_pet`]; the head is what a non-pet
    /// [`GlowStyle::RainbowKitty`] draws). Tests use it to prove the flying
    /// and pet spellings never overlap (an overlap would let the pet silently
    /// win).
    #[must_use]
    #[cfg(test)]
    pub fn style_names_flying_kitty(s: &str) -> bool {
        spellings::canonical(s) == Some("rainbow kitty flying")
    }

    /// Whether this style string asks for the pet drawn as a dog
    /// ([`crate::kitty_pet::PetSpecies::Dog`]). The dog is a species of the pet,
    /// not another trail: it resolves to [`GlowStyle::RainbowKitty`], so ribbon,
    /// starfield, chip melody and sound palette are unchanged; only the animal
    /// in front of the caret differs.
    #[inline]
    #[must_use]
    pub fn style_names_dog_pet(s: &str) -> bool {
        spellings::canonical(s) == Some("rainbow dog pet")
    }

    /// Whether this style string asks for the full-body pet in any species: the
    /// predicate the render path asks; use [`Self::style_names_dog_pet`] after
    /// only to pick the skin.
    #[inline]
    #[must_use]
    pub fn style_names_any_pet(s: &str) -> bool {
        matches!(
            spellings::canonical(s),
            Some(
                "rainbow kitty pet"
                    | "rainbow kitty underline"
                    | "rainbow kitty flat"
                    | "rainbow dog pet"
            )
        )
    }

    /// Whether this style string selects the highlighter-plus-under-baseline
    /// ribbon: `rainbow kitty underline` and its aliases.
    ///
    /// Every other rainbow spelling draws the tall body, the default (owner,
    /// 2026-08-29), so the resolved geometry is
    /// `!style_names_underline_ribbon(raw)` ([`GlowConfig::ribbon_tall`]).
    ///
    /// Not another [`GlowStyle`]: the classic field, scheduling, admission,
    /// companion, sound, sparkle, retract and delete machinery are one
    /// rainbow-kitty family; only the dark body geometry differs.
    #[inline]
    #[must_use]
    pub fn style_names_underline_ribbon(s: &str) -> bool {
        spellings::canonical(s) == Some("rainbow kitty underline")
    }

    /// Whether this style string selects the flat body
    /// (`docs/design/RAINBOW-KITTY-V2.md` §30): the same shape at every cell, the
    /// bed alone below the row bottom, a uniform 18 ms edge-in.
    ///
    /// The A/B twin of the default: every other rainbow-kitty spelling draws the
    /// comet body (fattest at the hand, thinner behind) with the vivid rail under
    /// the baseline and the from-the-hand attack; this spelling draws the flat
    /// body. Like [`Self::style_names_underline_ribbon`] it resolves to
    /// [`GlowStyle::RainbowKitty`], draws the resident pet, and forks at
    /// [`GlowConfig::ribbon_flat`] alone. `rainbow kitty flat` is the tall flat
    /// body (owner, 2026-09-13).
    #[inline]
    #[must_use]
    pub fn style_names_flat_ribbon(s: &str) -> bool {
        spellings::canonical(s) == Some("rainbow kitty flat")
    }

    /// The resolved style's diagnostic name, printed by `trail status` beside the
    /// raw config string so a spelling that fell back to the default reads as a
    /// fallback. Stable wire tokens (hyphenated, lower case), not config
    /// spellings.
    #[inline]
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Lumen => "lumen",
            Self::Phaser => "phaser",
            Self::RainbowKitty => "rainbow-kitty",
            Self::Sparkle => "sparkle",
            Self::Fire => "fire",
            Self::Laser => "laser",
            Self::Beam => "beam",
            Self::Water => "water",
            Self::Comet => "comet",
            Self::Classic => "classic",
        }
    }

    /// Whether a resolved `classic` spelling names the mono face (the two-tone
    /// `accent → color` tracer) rather than the default spectrum: the old
    /// `classic mono` spellings. They are aliases of `classic` in the
    /// vocabulary ([`spellings::canonical`] answers `classic`), so this reads
    /// the AUTHORED spelling, never the canonical one: a saved `classic mono`
    /// still draws the mono face. Both faces resolve to [`Self::Classic`]; the
    /// fork lands where the colour is picked (Classic with a trail colour
    /// draws it too, `cursor_fx::resolve_glow_config`).
    #[inline]
    #[must_use]
    pub fn style_names_classic_mono(style_raw: &str) -> bool {
        let s = style_raw.trim();
        ["classic mono", "classic lumen", "mono classic"]
            .iter()
            .any(|o| s.eq_ignore_ascii_case(o))
    }

    /// Parse a config string (trimmed, case-insensitive) through
    /// [`spellings::canonical`]. `off` and an unknown spelling are `Lumen`,
    /// the documented fallback of a page's `set_cursor_glow` (the native host
    /// resolves unknown spellings to its own default before it gets here).
    #[inline]
    pub fn parse(s: &str) -> Self {
        let s = s.trim();
        spellings::row(s)
            .and_then(|row| row.style)
            .unwrap_or(Self::Lumen)
    }
    /// Whether this style spawns particles (sparks / embers / droplets /
    /// laser-ablation sparks / beam stardust / comet debris glitter).
    #[inline]
    pub(super) const fn has_particles(self) -> bool {
        matches!(
            self,
            Self::Sparkle | Self::Fire | Self::Water | Self::Laser | Self::Beam | Self::Comet
        )
    }
}

/// Resolved tunables (`Copy`, so the host reads it out before borrowing window
/// state). `PartialEq` backs the `last_cfg` compare-on-write in
/// [`CursorGlow::tick`]: equal configs produce the same frame (configs
/// differing only in the audio-only [`Self::audible`] also do).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlowConfig {
    pub enabled: bool,
    pub style: GlowStyle,
    /// Base hue `0x00RRGGBB` (comet head + bloom), default theme cursor.
    pub color: u32,
    /// Secondary hue `0x00RRGGBB` (comet tail + ring), default cursor brightened.
    pub accent: u32,
    /// Comet fade duration.
    pub duration: Duration,
    /// Max comet length in cells.
    pub length: usize,
    /// Additive brightness scale 0.0..=1.0 (0 ⇒ effectively off): the visual
    /// half, folding the user's brightness knob, the Reduce Motion policy, and
    /// the host's load-shed envelope. See [`Self::audible`] for the audio half.
    pub intensity: f32,
    /// The audio half: whether a key pressed on this tick's window should be
    /// heard. A key-time click costs no GPU, so a shed frame is still a heard
    /// key, and Reduce Motion dims the light without closing the key seam. The
    /// master switch arrives as [`Self::enabled`] and the sound knobs gate
    /// host-side (`keystroke_click_audible`); the host puts here the one term
    /// the engine cannot see, whether the key landed in this window.
    pub audible: bool,
    /// Bloom-crown radius in cells (0 ⇒ no crown, comet only).
    pub radius: f32,
    /// Landing-ring "ping" on a jump.
    pub ring: bool,
    /// Whether the theme background is dark. Additive light needs a dark
    /// ground; on light themes the vapor (smoke/steam) switches to
    /// [`HaloMode::Over`] source-over veils. Default true.
    pub dark_theme: bool,
    /// The theme's resolved default foreground and background, `0x00RRGGBB`, with
    /// OSC 10/11 and DECSCNM folded in by the host. Only the fresh-typed glyph
    /// tint reads them: it anchors on `theme_fg` and bounds itself against
    /// `theme_bg` by WCAG contrast. [`aterm_core::render::COLOR_UNSET`] or
    /// `fg == bg` (a conceal-shaped theme) suppresses the tint.
    pub theme_fg: u32,
    pub theme_bg: u32,
    /// Whether this style draws its own additive comet beam (the anti-aliased
    /// streak along the swept path). False for `water` (its fluid wake) and
    /// `rainbow kitty` (its ribbon is the streak). The `comet` style layers
    /// this beam under its faint cell-body ember bed, which makes the streak
    /// continuous rather than grid-quantized. Derive it with [`style_has_beam`],
    /// not from `style` alone (`lumen` and unknown strings parse to
    /// [`GlowStyle::Lumen`] yet may need different beams).
    pub beam: bool,
    /// Horizontal attach point of the live-cursor bridge as a fraction of cell
    /// width (`0.5` = centre). The host sets ~`0.08` for a thin bar cursor
    /// (DECSCUSR bar / `cursor_style beam`), so the streak meets the bar rather
    /// than overshooting it by half a cell.
    pub head_dx: f32,
    // How much recent typing the rainbow kitty shows is not a config field: the
    // v2 mark is the set of cells the hand laid, each priced by
    // `Ribbon::cell_life` from `cfg.duration` (`cursor_trail_ms`) and floored at
    // the typist's phrase rest. `WAKE_LIFE_S` / `WAKE_MAX_CELLS` in
    // `rainbow_kitty::ribbon` belong to the jump corridor, a different effect.
    /// The rainbow ribbon's dark-theme transverse presentation: tall body or
    /// explicit underline/highlighter.
    ///
    /// `false`, selected only by [`GlowStyle::style_names_underline_ribbon`], is
    /// the underline presentation: an under-baseline strip plus a quieter
    /// glyph-band highlighter and momentum bloom toward the tapered typed
    /// streak. `true` is the default: the tall upward reach and full-strength
    /// glyph-band shoulder, so the gradient fills the swept glyph cell. Every
    /// other rainbow spelling, the `… tall` aliases included, selects it
    /// (owner, 2026-08-29). Resolve it with
    /// `!GlowStyle::style_names_underline_ribbon(raw)`.
    ///
    /// There is one emitter; this flag selects only the upward reach and
    /// shoulder. Cohort admission, segment survival, retract, momentum,
    /// starfield and the light-theme rail are shared.
    pub ribbon_tall: bool,
    /// The flat body, selected only by the explicit `… flat` spelling
    /// ([`GlowStyle::style_names_flat_ribbon`]). `false` draws the comet body
    /// with its vivid rail (`docs/design/RAINBOW-KITTY-V2.md` §30): fattest and
    /// brightest at the hand, the full-value spectrum below the row bottom, and
    /// a new cell's light entering from the caret side. `true` draws the same
    /// shape at every cell as an A/B control. Read by `rainbow_kitty::ribbon`
    /// alone.
    pub ribbon_flat: bool,
    /// The classic wake's colour face: `false` (plain `classic`) is a rolling
    /// spectrum sweep independent of the theme; `true` (`classic mono`) is the
    /// two-tone tracer fading `accent → color`, which follows the theme's cursor
    /// colour, `cursor_trail_color` and live OSC 12.
    pub classic_mono: bool,
}

/// `Glow size`'s shipped default, in cells: the size every crown is authored
/// at, from which `cursor_trail_radius` scales it.
pub const GLOW_RADIUS_DEFAULT: f32 = 0.6;

/// The shipped `Length` (cells): the knob's default, against which a style
/// whose tail is drawn by distance scales it (sparkle's gold rail).
pub const GLOW_LENGTH_DEFAULT: usize = 24;

/// One tuning knob of the cursor family that a style may or may not read:
/// the `cursor_trail_*` keys the Settings Adjust card lists per style.
/// (Every style reads its duration and brightness.)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrailKnob {
    /// `cursor_trail_length`: how many cells one move's trail may span.
    Length,
    /// `cursor_trail_radius` (Glow size): the size of the crown at the caret.
    Radius,
    /// `cursor_trail_ring`: the landing ring on a jump.
    Ring,
    /// `cursor_trail_color`.
    Color,
    /// `cursor_trail_accent`.
    Accent,
    /// `cursor_fire_shimmer`: the GPU heat shimmer, which distorts the fire
    /// field only Fire emits.
    FireShimmer,
}

/// The facts besides the style that decide whether a knob reaches the
/// frame: the ground, the live cursor shape, whether the landing ring is on,
/// and the classic wake's colour face.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct KnobContext {
    /// The ground is dark. On a light one only Lumen (its glow behind the
    /// caret) and Classic (its crown in ink, `classic_wake`) draw a crown,
    /// so `Glow size` reaches no other style there, and no tracer fades from
    /// the accent.
    pub dark: bool,
    /// The cursor is a bar: the light rod is its body in every style, and it
    /// wears an explicit trail colour.
    pub bar: bool,
    /// `cursor_trail_ring` is on.
    pub ring: bool,
    /// The classic wake draws its one-colour face (a trail colour is set, or
    /// the old `classic mono` spelling is saved).
    pub classic_tinted: bool,
}

impl KnobContext {
    /// The context in which a knob shows at its widest for a style: the ring
    /// on and the classic face tinted. Settings lists a style's rows from it,
    /// so a row stays put while another knob toggles whether it shows.
    #[inline]
    #[must_use]
    pub const fn widest(bar: bool) -> Self {
        Self {
            dark: true,
            bar,
            ring: true,
            classic_tinted: true,
        }
    }
}

impl GlowStyle {
    /// THE KNOB LAW: whether a trail drawn in this style reads `knob` in
    /// `ctx`. One law for the engine and Settings: the engine applies it
    /// (`cursor_fx::resolve_glow_config` folds a knob the style does not read
    /// to the value that leaves its frame unchanged, the light rod wears the
    /// trail colour only where it says, and the spawn bounds a path by
    /// `Length` only where it says) and Settings lists and words a style's
    /// rows from it, so the two cannot disagree. Pinned by
    /// `the_knob_law_is_what_the_frame_reads`, which changes each knob and
    /// diffs the frames.
    ///
    /// * `Length`: not the rainbow kitty's ribbon (its own walk), nor Laser,
    ///   Beam and Water, whose streaks span the whole move; Lumen's tracer
    ///   and Sparkle's ribbon on either ground (on a light one they are ink
    ///   under the letters, as long as the path).
    /// * `Radius`, `Ring`: not the rainbow kitty (the ribbon owns its head)
    ///   or Beam (the tube is its whole look); on a light ground `Radius` is
    ///   Lumen's alone, the one style that draws a crown there.
    /// * `Color`: every style's light rod on a bar cursor; on a block, not
    ///   the styles that colour from their own ramps (the rainbow kitty,
    ///   Fire, Water, Phaser's sweep).
    /// * `Accent`: the landing ring of the styles whose ring wears it
    ///   (Lumen, Comet, Sparkle, Phaser, Classic) while the ring is on, plus
    ///   the tails that fade from it (Lumen's tracer and Comet's dust and
    ///   coma fringe on a dark ground, the tinted classic tracer on either).
    /// * `FireShimmer`: Fire alone.
    #[inline]
    #[must_use]
    pub const fn reads_knob(self, knob: TrailKnob, ctx: KnobContext) -> bool {
        use GlowStyle::{
            Beam, Classic, Comet, Fire, Laser, Lumen, Phaser, RainbowKitty, Sparkle, Water,
        };
        match knob {
            TrailKnob::Length => match self {
                RainbowKitty | Laser | Beam | Water => false,
                Lumen | Sparkle | Phaser | Fire | Comet | Classic => true,
            },
            // On a light ground only Lumen (its glow behind the caret) and
            // Classic (its crown as one ink mark on the caret's row) draw a
            // crown; every other crown is light, which a light page cannot
            // show.
            TrailKnob::Radius if !ctx.dark => matches!(self, Lumen | Classic),
            TrailKnob::Radius | TrailKnob::Ring => !matches!(self, RainbowKitty | Beam),
            TrailKnob::Color => ctx.bar || !matches!(self, RainbowKitty | Fire | Water | Phaser),
            TrailKnob::Accent => match self {
                Lumen | Comet => ctx.dark || ctx.ring,
                Classic => ctx.ring || ctx.classic_tinted,
                Sparkle | Phaser => ctx.ring,
                RainbowKitty | Fire | Laser | Beam | Water => false,
            },
            TrailKnob::FireShimmer => matches!(self, Fire),
        }
    }

    /// Whether the style draws the trail colour on a block cursor: the auto
    /// accent then brightens it, and an explicit colour is pinned against the
    /// live cursor's. A style that does not keeps both on the cursor colour.
    #[inline]
    #[must_use]
    pub const fn draws_trail_color(self) -> bool {
        self.reads_knob(
            TrailKnob::Color,
            KnobContext {
                dark: true,
                bar: false,
                ring: false,
                classic_tinted: false,
            },
        )
    }
}

/// Whether a raw `cursor_trail_style` string draws its own additive comet beam.
///
/// False for exactly two style families: `water` (its dedicated curved wake)
/// and `rainbow kitty` (its continuous ribbon is the streak). Every other style,
/// `comet` among them, shows the anti-aliased pixel-space beam: the
/// comet's [`crate::cursor_trail::CursorTrail`] cell body is grid-quantized, so
/// it layers over the beam (the beam supplies continuity, the cells supply
/// body; see `READABLE_ALPHA_CAP` in `cursor_trail.rs`).
///
/// Keyed off the raw string, not [`GlowStyle`]: in the GUI resolver the raw
/// `comet` spelling also routes the opaque cadence-comet body.
#[inline]
#[must_use]
pub fn style_has_beam(style_raw: &str) -> bool {
    style_has_beam_of(GlowStyle::parse(style_raw.trim()), style_raw)
}

/// [`style_has_beam`] for a caller that already parsed the style (the per-frame
/// `glow_config` path), avoiding a re-parse while sharing one source of truth.
/// `style_raw` is taken (currently unused) so a future raw-string distinction
/// between spellings of one [`GlowStyle`] needs no call-site changes.
#[inline]
#[must_use]
pub const fn style_has_beam_of(style: GlowStyle, _style_raw: &str) -> bool {
    !matches!(style, GlowStyle::Water | GlowStyle::RainbowKitty)
}

/// The default laser/lightning hue: storm violet (`0x00RRGGBB`), applied when
/// no `cursor_trail_color` is pinned (a night strike is a white-violet flash).
/// Blaze whitening still runs, so a hot bolt flashes white and cools to violet.
/// An explicit trail colour wins, and every quad stays in that hue; a pinned
/// cursor colour (config `cursor_color`, a live OSC 12) does NOT recolour it
/// (`cursor_fx::glow_color_follows_live_cursor`).
pub const LASER_DEFAULT_COLOR: u32 = 0x00B4_8CFF;

/// The default beam hue: photon ice-blue (`0x00RRGGBB`), applied when no
/// `cursor_trail_color` is set and no cursor colour is pinned (config
/// `cursor_color`, a live OSC 12 — either one recolours the tube), so the tube
/// reads as cool light (a searchlight, not a weapon). An explicit trail colour
/// wins.
pub const BEAM_DEFAULT_COLOR: u32 = 0x008C_DCFF;

/// The default sparkle hue: starlight gold (`0x00RRGGBB`), applied when no
/// `cursor_trail_color` is set and no cursor colour is pinned (config
/// `cursor_color`, a live OSC 12). Only the emitter block and monochrome
/// accents use it; the ribbon stays rainbow.
pub const SPARKLE_DEFAULT_COLOR: u32 = 0x00FF_D9A0;

/// The default comet hue: pale glacial blue (`0x00RRGGBB`), applied when no
/// `cursor_trail_color` is set and no cursor colour is pinned (config
/// `cursor_color`, a live OSC 12) — a comet is ice lit white-hot at the
/// nucleus. An explicit or pinned colour wins, and the tail/coma/debris
/// palette and the cell bed re-derive from it.
pub const COMET_DEFAULT_COLOR: u32 = 0x009E_D6FF;
