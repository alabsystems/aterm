// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Style resolution: the selectable looks ([`GlowStyle`]), their spellings and
//! compatibility aliases, and the resolved per-frame [`GlowConfig`].

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
    /// letters inside it. `… tall` spellings name that geometry directly;
    /// `… underline` spellings select the quieter highlighter plus
    /// under-baseline shoulder.
    RainbowKitty,
    /// Phaser comet + a shower of additive spark particles.
    Sparkle,
    /// Black-body warm comet + rising, drifting embers.
    Fire,
    /// A lightning bolt in storm violet ([`LASER_DEFAULT_COLOR`]) or the pinned
    /// trail colour — dim tail brightening to the pure hue at the head, with a
    /// tight same-hue bloom.
    Laser,
    /// Beam: a clean, steady tube of cool light with near-constant power along
    /// its length, no particles, scintillation or lightning. Instead of fading
    /// in place it powers down, thinning toward a hairline like an emitter
    /// switching off.
    Beam,
    /// Fluid ocean wake — a deep-blue undertow under a thin travelling cyan
    /// crest — with droplets that spray up and arc back down under gravity.
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
    /// A user-generated trail pack: the look is data ([`GlowConfig::pack`]'s
    /// [`TrailParams`]), driven by [`CursorGlow::emit_custom`]. Every
    /// `pack:<id>` style resolves to this variant. Shared tick/spawn paths read
    /// `cfg.pack` (heat τ/gain, crown window, ring, particle spawn), but every
    /// such read is inert when `cfg.pack` is `None`, so built-in styles'
    /// output is unaffected (pinned by the whole-frame golden test).
    Custom,
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
    /// `rainbow kitty` and bare `kitty` are in this list: a style named kitty
    /// should draw the cat, and the resident pet is visible from frame zero
    /// where the flying head is a rare earned flypast. The flying head has its
    /// own explicit spellings (`Self::style_names_flying_kitty`: `rainbow kitty
    /// flying`, `flying kitty`, `kitty flying`), is offered in the Settings
    /// picker (`prefs::CURSOR_TRAIL_STYLES`), and the historical aliases
    /// `nyan rainbow` / `nyan` / `rainbow` still select it. The trail itself is
    /// identical either way.
    ///
    /// Both animals are never on glass at once: `app_render::trail_is_kitty_pet`
    /// ([`Self::style_names_any_pet`]) feeds `pet_mode`, the flying head is
    /// admitted only outside pet mode, and `retire_kitty_cursor_without_owner`
    /// grounds a head already on glass once `pet_mode && sing <= 0.0`, including
    /// on a hot config reload. (The song tenure, `sing > 0.0`, is a bounded
    /// promise the pet does not own.)
    ///
    /// Every spelling [`Self::style_names_underline_ribbon`] recognizes takes the
    /// pet too: the underline ribbon is a geometry, not an animal, and
    /// `prefs::CURSOR_TRAIL_STYLE_ALIASES` canonicalises all four underline
    /// spellings onto `"rainbow kitty underline"`
    /// (`cursor_trail_style_aliases_agree_with_engine_parse` requires an alias
    /// and its canonical to draw the same animal). The flying head keeps
    /// `Self::style_names_flying_kitty` and the bare `nyan` / `rainbow` /
    /// `nyan rainbow` aliases, none of which names a geometry.
    #[must_use]
    pub fn style_names_kitty_pet(s: &str) -> bool {
        let s = s.trim();
        [
            "rainbow kitty pet",
            "kitty pet",
            "pet kitty",
            // The bare spelling and word; the flying head keeps
            // `style_names_flying_kitty` and the `nyan`/`rainbow` aliases.
            "rainbow kitty",
            "kitty",
        ]
        .iter()
        .any(|o| s.eq_ignore_ascii_case(o))
            || Self::style_names_underline_ribbon(s)
            // The flat spelling: the A/B twin of the default body, differing in
            // the body only, so the same resident.
            || Self::style_names_flat_ribbon(s)
            // The tall spellings: `… tall` names the geometry the default
            // already draws (`ribbon_tall` is `!style_names_underline_ribbon`),
            // so it must draw the default's animal too.
            || Self::style_names_tall_ribbon(s)
    }

    /// Whether this style string explicitly asks for the flying kitty head
    /// ([`crate::kitty_cursor`]), the rare earned flypast, on the same rainbow
    /// ribbon.
    ///
    /// Not consulted by the draw path (which asks
    /// [`Self::style_names_any_pet`]; the head is what a non-pet
    /// [`GlowStyle::RainbowKitty`] draws). It declares the spellings in one
    /// place for the picker and for
    /// `every_kitty_spelling_draws_the_resident_and_flying_stays_reachable`,
    /// which proves the two lists never overlap (an overlap would let the pet
    /// silently win).
    #[must_use]
    #[cfg(test)]
    pub fn style_names_flying_kitty(s: &str) -> bool {
        let s = s.trim();
        ["rainbow kitty flying", "flying kitty", "kitty flying"]
            .iter()
            .any(|o| s.eq_ignore_ascii_case(o))
    }

    /// Whether this style string asks for the pet drawn as a dog
    /// ([`crate::kitty_pet::PetSpecies::Dog`]). The dog is a species of the pet,
    /// not another trail: it resolves to [`GlowStyle::RainbowKitty`], so ribbon,
    /// starfield, chip melody and sound palette are unchanged; only the animal
    /// in front of the caret differs.
    #[must_use]
    pub fn style_names_dog_pet(s: &str) -> bool {
        let s = s.trim();
        ["rainbow dog pet", "dog pet", "pet dog", "rainbow puppy pet"]
            .iter()
            .any(|o| s.eq_ignore_ascii_case(o))
    }

    /// Whether this style string asks for the full-body pet in any species: the
    /// predicate the render path asks; use [`Self::style_names_dog_pet`] after
    /// only to pick the skin.
    #[must_use]
    pub fn style_names_any_pet(s: &str) -> bool {
        Self::style_names_kitty_pet(s) || Self::style_names_dog_pet(s)
    }

    /// Whether this style string explicitly spells the tall ribbon: a continuous
    /// seven-anchor rainbow filling the glyph cell with the letters inside it.
    ///
    /// True for exactly the four explicit `… tall` spellings. Every other
    /// rainbow-kitty spelling (the bare name, cat/dog pet aliases, the flying
    /// head, `nyan`/`rainbow`) also draws the tall body, the default; only the
    /// `… underline` aliases select the highlighter-plus-under-baseline hybrid
    /// (owner, 2026-08-29).
    /// Both resolve to [`GlowStyle::RainbowKitty`], so every style-keyed table
    /// is shared; only the body geometry forks at [`GlowConfig::ribbon_tall`].
    #[must_use]
    pub fn style_names_tall_ribbon(s: &str) -> bool {
        let s = s.trim();
        [
            "rainbow kitty tall",
            "rainbow tall",
            "tall rainbow",
            "nyan tall",
        ]
        .iter()
        .any(|o| s.eq_ignore_ascii_case(o))
    }

    /// Whether this style string spells the highlighter-plus-under-baseline
    /// ribbon explicitly.
    ///
    /// A raw-spelling predicate, not a geometry query: a bare `rainbow kitty`
    /// (and every pet/flying/`nyan` spelling) draws the tall body while
    /// returning `false` here and from [`Self::style_names_tall_ribbon`]. The
    /// resolved geometry is `!style_names_underline_ribbon(raw)`; use this only
    /// where what the string says matters (for example, the Settings label).
    ///
    /// Not another [`GlowStyle`]: the classic field, scheduling, admission,
    /// companion, sound, sparkle, retract and delete machinery are one
    /// rainbow-kitty family; only the dark body geometry differs.
    #[must_use]
    pub fn style_names_underline_ribbon(s: &str) -> bool {
        let s = s.trim();
        [
            "rainbow kitty underline",
            "rainbow underline",
            "underline rainbow",
            "nyan underline",
        ]
        .iter()
        .any(|o| s.eq_ignore_ascii_case(o))
    }

    /// Whether this style string spells the flat body explicitly
    /// (`docs/design/RAINBOW-KITTY-V2.md` §30): the same shape at every cell, the
    /// bed alone below the row bottom, a uniform 18 ms edge-in.
    ///
    /// The A/B twin of the default: every other rainbow-kitty spelling draws the
    /// comet body (fattest at the hand, thinner behind) with the vivid rail under
    /// the baseline and the from-the-hand attack; this spelling draws the flat
    /// body, pinned by
    /// `the_flat_spelling_collapses_every_comet_branch_byte_for_byte`. Like
    /// [`Self::style_names_underline_ribbon`] it resolves to
    /// [`GlowStyle::RainbowKitty`], draws the resident pet, and forks at
    /// [`GlowConfig::ribbon_flat`] alone. `rainbow kitty flat` is the tall flat
    /// body (owner, 2026-09-13).
    #[must_use]
    pub fn style_names_flat_ribbon(s: &str) -> bool {
        let s = s.trim();
        [
            "rainbow kitty flat",
            "rainbow flat",
            "flat rainbow",
            "nyan flat",
        ]
        .iter()
        .any(|o| s.eq_ignore_ascii_case(o))
    }

    /// The resolved style's diagnostic name, printed by `trail status` beside the
    /// raw config string so a spelling that fell back to the default reads as a
    /// fallback. Stable wire tokens (hyphenated, lower case), not config
    /// spellings.
    #[must_use]
    pub fn label(self) -> &'static str {
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
            Self::Custom => "pack",
        }
    }

    /// Whether a resolved `classic` spelling names the mono face (the two-tone
    /// `accent → color` tracer) rather than the default spectrum. Both spellings
    /// resolve to [`Self::Classic`]; the fork lands where the colour is picked.
    #[must_use]
    pub fn style_names_classic_mono(style_raw: &str) -> bool {
        let s = style_raw.trim();
        ["classic mono", "classic lumen", "mono classic"]
            .iter()
            .any(|o| s.eq_ignore_ascii_case(o))
    }

    /// Parse a config string (case-insensitive); unknown → `Lumen`.
    pub fn parse(s: &str) -> Self {
        // Case-insensitive without allocating (`eq_ignore_ascii_case`). The GUI
        // resolves this once per config-asset generation, not per frame.
        let s = s.trim();
        // A `pack:<id>` spelling names a trail pack and resolves to the custom
        // variant; the params are attached by the app-layer resolver
        // (`glow_config`), not here.
        if s.strip_prefix("pack:")
            .is_some_and(|id| !id.trim().is_empty())
        {
            return Self::Custom;
        }
        let any = |opts: &[&str]| opts.iter().any(|o| s.eq_ignore_ascii_case(o));
        if any(&["phaser"]) {
            Self::Phaser
        } else if any(&[
            "rainbow kitty",
            // The bare word, admitted with the pet (`style_names_kitty_pet`): the
            // companion predicate is consulted only for styles that parsed to
            // `RainbowKitty`, so a pet spelling must land here or it would resolve
            // to `Lumen` and fail `resident_pet_owner_present`'s style term.
            "kitty",
            "nyan rainbow",
            "nyan",
            "rainbow",
            // The explicit flying-head spellings (`style_names_flying_kitty`):
            // same ribbon and style, the head instead of the resident.
            "rainbow kitty flying",
            "flying kitty",
            "kitty flying",
            // The pet spellings resolve to the same style: the pet swaps the
            // companion, not the trail (`style_names_kitty_pet`).
            "rainbow kitty pet",
            "kitty pet",
            "pet kitty",
            // …and the DOG spellings for the same reason again: a different
            // species of the same companion, riding the same trail.
            "rainbow dog pet",
            "dog pet",
            "pet dog",
            "rainbow puppy pet",
            // Presentation aliases: the tall body is the default; `… underline`
            // selects the highlighter-plus-strip shoulder, and `… tall` spells the
            // default out. Presentations of this style, not extra variants.
            "rainbow kitty tall",
            "rainbow tall",
            "tall rainbow",
            "nyan tall",
            "rainbow kitty underline",
            "rainbow underline",
            "underline rainbow",
            "nyan underline",
            // The flat spellings (`style_names_flat_ribbon`): the A/B twin of the
            // default comet body, a presentation of this same style.
            "rainbow kitty flat",
            "rainbow flat",
            "flat rainbow",
            "nyan flat",
            // `v2` spellings are no-op aliases of the bare spelling, so older A/B
            // configs still load; `v1` spellings are unknown words and parse to
            // `Lumen`.
            "rainbow kitty v2",
            "rainbow v2",
            "nyan v2",
            "kitty v2",
        ]) {
            Self::RainbowKitty
        } else if any(&["sparkle", "sparkles", "phaser-sparkle", "rainbow-sparkle"]) {
            Self::Sparkle
        } else if any(&["fire", "ember", "embers"]) {
            Self::Fire
        } else if any(&["laser"]) {
            Self::Laser
        } else if any(&["beam", "lightbeam", "light-beam"]) {
            Self::Beam
        } else if any(&["water", "ocean", "wave"]) {
            Self::Water
        } else if any(&["comet"]) {
            Self::Comet
        } else if any(&[
            // `classic` is the picker's name; the version spellings are how a
            // user asks for it by the release they remember.
            "classic",
            "classic wake",
            // The mono face is a presentation of this style: the colour closure
            // forks on the spelling (`style_names_classic_mono`).
            "classic mono",
            "classic lumen",
            "mono classic",
            "v0.28",
            "v028",
            "0.28",
            "retro",
        ]) {
            Self::Classic
        } else {
            Self::Lumen
        }
    }
    /// Whether this style spawns particles (sparks / embers / droplets /
    /// laser-ablation sparks / beam stardust / comet debris glitter).
    pub(super) fn has_particles(self) -> bool {
        matches!(
            self,
            Self::Sparkle | Self::Fire | Self::Water | Self::Laser | Self::Beam | Self::Comet
        )
    }
}

/// Resolved tunables (`Copy`, so the host reads it out before borrowing window
/// state). `PartialEq` (never `Eq`; f32 fields) backs the `last_cfg`
/// compare-on-write in [`CursorGlow::tick`]: equal configs produce the same
/// frame. The converse does not hold: [`Self::audible`] is audio-only, so
/// configs differing in it alone draw identical frames.
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
    /// heard, independent of how bright the trail is.
    ///
    /// A key-time click costs no GPU, so a frame shed for performance is still a
    /// heard key, and Reduce Motion dims the light without closing the key seam.
    ///
    /// Not decided here: the master switch and serious mode arrive as
    /// [`Self::enabled`], and the sound knobs (`trail_sounds`,
    /// `trail_sound_volume`, the resize-quiet window, a dead audio worker) gate
    /// host-side in `keystroke_click_audible` before a cue is minted. The host
    /// puts here the one term the engine cannot see: whether the key landed in
    /// this window.
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
    /// OSC 10/11 and DECSCNM folded in by the host (canonically
    /// `rgb_to_u32(terminal_blank_cell(term).fg / .bg)`).
    ///
    /// Only the fresh-typed glyph tint reads them: it anchors on `theme_fg` so
    /// the tint hands the foreground back exactly, and bounds itself against
    /// `theme_bg` by real WCAG contrast.
    ///
    /// A host that cannot resolve one may pass [`aterm_core::render::COLOR_UNSET`];
    /// the tint then suppresses itself rather than reading the sentinel as black.
    /// `fg == bg` (a conceal-shaped theme) also suppresses it, as
    /// `floor_min_contrast_fg` refuses to reveal concealed text.
    pub theme_fg: u32,
    pub theme_bg: u32,
    /// Whether this style draws its own additive comet beam (the anti-aliased
    /// streak along the swept path). False for `water` (its fluid wake) and
    /// `rainbow kitty` (its ribbon is the streak). The default `comet` layers
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
    /// The resolved trail-pack params when `style == GlowStyle::Custom` (`None`
    /// for built-in styles). Only the custom interpreter reads it; built-in
    /// paths dispatch on `style`. Carried inline (`Copy`) like the resolved
    /// `color`/`duration`/`length`; the app-layer resolver looks a `pack:<id>`
    /// up in its registry once, off the frame path.
    pub pack: Option<TrailParams>,
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
    /// glyph-band shoulder, so the gradient fills the swept glyph cell. Ordinary
    /// rainbow spellings and the four `… tall` aliases select it
    /// (owner, 2026-08-29).
    ///
    /// The raw-name predicates are not complements: a bare `rainbow kitty`
    /// answers `false` to both [`GlowStyle::style_names_tall_ribbon`] and
    /// [`GlowStyle::style_names_underline_ribbon`] though its geometry is tall.
    /// Resolve geometry with `!GlowStyle::style_names_underline_ribbon(raw)`.
    ///
    /// There is one emitter; this flag selects only the upward reach and
    /// shoulder. Cohort admission, segment survival, retract, momentum,
    /// starfield and the light-theme rail are shared.
    pub ribbon_tall: bool,
    /// The flat body: the explicit `… flat` spelling
    /// ([`GlowStyle::style_names_flat_ribbon`]), `false` by default.
    ///
    /// `false` draws the comet body with its vivid rail
    /// (`docs/design/RAINBOW-KITTY-V2.md` §30): fattest and brightest at the
    /// hand, the full-value spectrum below the row bottom where no letter can
    /// be, and a new cell's light entering from the caret side. `true` draws the
    /// flat body (same shape at every cell, the bed alone below the baseline,
    /// uniform edge-in) as an A/B control. A presentation like
    /// [`Self::ribbon_tall`], composing with either geometry; read by
    /// `rainbow_kitty::ribbon` alone.
    pub ribbon_flat: bool,
    /// The classic wake's colour face, chosen by the resolved spelling as
    /// [`Self::ribbon_tall`] chooses geometry.
    ///
    /// `false` (plain `classic`) is a rolling spectrum sweep independent of the
    /// theme. `true` (`classic mono`) is the two-tone tracer fading
    /// `accent → color`, which follows the theme's cursor colour,
    /// `cursor_trail_color` and live OSC 12.
    pub classic_mono: bool,
}

/// Whether a raw `cursor_trail_style` string draws its own additive comet beam.
///
/// False for exactly two style families: `water` (its dedicated curved wake)
/// and `rainbow kitty` (its continuous ribbon is the streak). Every other style,
/// including the default `comet`, shows the anti-aliased pixel-space beam: the
/// comet's [`crate::cursor_trail::CursorTrail`] cell body is grid-quantized, so
/// it layers over the beam (the beam supplies continuity, the cells supply
/// body; see `READABLE_ALPHA_CAP` in `cursor_trail.rs`).
///
/// Keyed off the raw string, not [`GlowStyle`]: in the GUI resolver the raw
/// `comet` spelling also routes the opaque cadence-comet body.
#[must_use]
pub fn style_has_beam(style_raw: &str) -> bool {
    style_has_beam_of(GlowStyle::parse(style_raw.trim()), style_raw)
}

/// [`style_has_beam`] for a caller that already parsed the style (the per-frame
/// `glow_config` path), avoiding a re-parse while sharing one source of truth.
/// `style_raw` is taken (currently unused) so a future raw-string distinction
/// between spellings of one style needs no call-site changes.
#[must_use]
pub fn style_has_beam_of(style: GlowStyle, _style_raw: &str) -> bool {
    !matches!(style, GlowStyle::Water | GlowStyle::RainbowKitty)
}

/// The default laser/lightning hue: storm violet (`0x00RRGGBB`), applied when
/// no `cursor_trail_color` is pinned (a night strike is a white-violet flash).
/// Blaze whitening still runs, so a hot bolt flashes white and cools to violet.
/// An explicit trail colour wins, and every quad stays in that hue.
pub const LASER_DEFAULT_COLOR: u32 = 0x00B4_8CFF;

/// The default beam hue: photon ice-blue (`0x00RRGGBB`), applied when no
/// `cursor_trail_color` is pinned, so the tube reads as cool light (a
/// searchlight, not a weapon). An explicit trail colour wins.
pub const BEAM_DEFAULT_COLOR: u32 = 0x008C_DCFF;

/// The default sparkle hue: starlight gold (`0x00RRGGBB`), applied when no
/// `cursor_trail_color` is pinned. Only the emitter block and monochrome accents
/// use it; the ribbon stays rainbow.
pub const SPARKLE_DEFAULT_COLOR: u32 = 0x00FF_D9A0;

/// The default comet hue: pale glacial blue (`0x00RRGGBB`), applied when no
/// `cursor_trail_color` is pinned (a comet is ice lit white-hot at the
/// nucleus). An explicit trail colour wins, and the tail/coma/debris palette
/// re-derives from it.
pub const COMET_DEFAULT_COLOR: u32 = 0x009E_D6FF;
