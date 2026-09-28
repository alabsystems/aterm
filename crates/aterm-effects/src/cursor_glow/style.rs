// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! STYLE RESOLUTION — the selectable looks ([`GlowStyle`]), their spellings
//! and compatibility aliases, and the resolved per-frame [`GlowConfig`].

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
    /// Rainbow kitty — a momentum-driven continuous rainbow ribbon streaming the
    /// swept path, brighter/longer with typing heat, with a sparse twinkling
    /// starfield. The DEFAULT presentation is
    /// the v0.43 tall body: one smooth continuous seven-anchor spectrum filling
    /// every swept glyph cell with the letters inside it. Explicit `… tall`
    /// spellings name that geometry directly; explicit `… underline`
    /// spellings select the quieter highlighter-plus-under-baseline shoulder.
    RainbowKitty,
    /// Phaser comet + a shower of additive spark particles.
    Sparkle,
    /// Black-body warm comet + rising, drifting embers.
    Fire,
    /// A monochrome beam in the cursor colour — dim tail brightening to the pure
    /// hue at the head, with a tight same-hue bloom (no white flash).
    Laser,
    /// BEAM — a clean, steady TUBE of cool light: near-constant power along its
    /// whole length (a coherent rod, not a dying comet), no particles, no
    /// scintillation, no lightning — and instead of fading in place it POWERS
    /// DOWN: the tube thins toward a hairline as its light dies, like an
    /// emitter switching off.
    Beam,
    /// Fluid ocean wake — a deep-blue undertow under a thin travelling cyan
    /// crest — with droplets that spray up and arc back down under gravity.
    Water,
    /// The COMET, grown from the plain tracer into a real comet: an icy dust
    /// tail — white-hot at the head, freezing back through the base hue to a
    /// dusty tail — plus twinkling debris glitter shed along the swept path.
    /// (The raw styles `beam` and `lumen` keep the plain [`Self::Lumen`] tracer.)
    Comet,
    /// THE CLASSIC WAKE — the v0.28 trail, salvaged: a thin four-layer comet
    /// under a soft square box-stack bloom, with a landing ring on a jump. The
    /// look aterm shipped as its DEFAULT at tag `v0.28`, kept as an option
    /// because the modern engine no longer draws it — today's `phaser` is a fat
    /// saturated band, and today's shape gates mint no geometry at all from a
    /// screen-crossing jump. Its art and its whole animation state live in
    /// [`crate::classic_wake`], reached by ONE branch in [`CursorGlow::tick`],
    /// so no built-in style's output can move because this one exists.
    Classic,
    /// A user-generated **Trail Pack** — the look is DATA
    /// ([`GlowConfig::pack`]'s [`TrailParams`]), not code. Every `pack:<id>`
    /// style resolves to this one variant; the resolved params are carried
    /// inline on the config and driven by [`CursorGlow::emit_custom`].
    /// Shared tick/spawn paths read `cfg.pack` (heat τ/gain, crown window,
    /// ring, particle spawn), but every such read is provably inert when
    /// `cfg.pack` is `None` (its `else`/default reproduces the prior
    /// behaviour), so no built-in style's OUTPUT changes — the ten built-in
    /// styles stay byte-identical (pinned by the whole-frame golden proof).
    Custom,
}

impl GlowStyle {
    /// Whether this style string asks for the full-body **pet** companion
    /// ([`crate::kitty_pet`]) rather than the flying kitty.
    ///
    /// A separate predicate rather than a [`GlowStyle`] variant on purpose: every
    /// spelling here resolves to [`GlowStyle::RainbowKitty`], so every style-keyed
    /// table in the engine — the ribbon geometry, the starfield, the sound
    /// palette, the momentum law — is untouched, and the two companions are a
    /// pure swap at the one place that draws one.
    ///
    /// `rainbow kitty` AND BARE `kitty` ARE IN THIS LIST (owner, twice, against
    /// v0.60.0: "I STILL don't see the cursor kitty pet, I see the old style
    /// kitty head"). A style whose name is *kitty* that draws no kitty — only a
    /// head flying past a few times a minute, when a sustained high-band typing
    /// run earns it — is a naming trap, and it was the last one standing: the
    /// shipped default already reads `rainbow kitty pet`, so the ONLY people who
    /// still got the flying head were the ones who had written the obvious
    /// spelling into their config by hand.
    ///
    /// WHAT HAPPENS TO SOMEONE WHO WANTED THE FLYING HEAD FROM `rainbow kitty`.
    /// They lose it on upgrade, and that is a deliberate, bounded trade:
    ///
    ///  * The flying head is NOT deleted and NOT unreachable. It has its own
    ///    explicit spellings now (`Self::style_names_flying_kitty` —
    ///    `rainbow kitty flying` / `flying kitty` / `kitty flying`), it is a
    ///    first-class entry in the Settings picker (`prefs::CURSOR_TRAIL_STYLES`),
    ///    and the historical aliases `nyan rainbow` / `nyan` / `rainbow` still
    ///    select it untouched, so a config written against any past release that
    ///    used one of those keeps exactly the animal it had.
    ///  * The two words this predicate takes over are the two that NAME A KITTY.
    ///    Someone typing `kitty` is asking for the cat, and the resident pet is
    ///    the one that is actually there — visible from frame zero, walking the
    ///    line — where the head is a rare flypast. The trail itself, which is
    ///    what the other spellings buy, is bit-for-bit the same either way.
    ///  * It is one documented line to get back, it is in the picker, and the
    ///    CHANGELOG entry says so.
    ///
    /// BOTH ANIMALS CAN NEVER BE ON GLASS AT ONCE, and the head can never be
    /// stranded mid-flight by this widening. `app_render::trail_is_kitty_pet`
    /// ([`Self::style_names_any_pet`]) feeds `pet_mode`, and `pet_mode` is the
    /// flying head's own suppressor: `flying_kitty_admitted(pet_mode, sing)` is
    /// `!pet_mode || sing > 0.0`, and `retire_kitty_cursor_without_owner`
    /// GROUNDS an episode already on glass the moment `pet_mode && sing <= 0.0`.
    /// So the widened list moves these configs from "flying head, no pet" to
    /// "pet, no flying head" in one step, including on a hot config reload with
    /// a head mid-arc — it is retired, not abandoned. (The song tenure, the one
    /// `sing > 0.0` case, is a bounded explicit promise the pet does not own.)
    ///
    /// **AND THE UNDERLINE RIBBON IS A GEOMETRY, NOT AN ANIMAL.** Every
    /// spelling [`Self::style_names_underline_ribbon`] recognizes takes the
    /// family's companion too, because this list is a WHOLE-STRING equality and
    /// `"rainbow kitty underline"` was in neither companion list: it fell
    /// through to the flying head purely by not matching, so appending one
    /// geometry word to the shipped `rainbow kitty` silently swapped the animal
    /// (measured: `pet_active=true cat_active=false` tall,
    /// `pet_active=false cat_active=true` underline, on otherwise identical
    /// `trail status` rows).
    ///
    /// That was never the rule anyone wrote down — it is the rule the matcher
    /// happened to have. [`Self::style_names_underline_ribbon`]'s own docstring
    /// says the opposite in as many words: *"the spectrum field, scheduling,
    /// admission, **companion**, sound, sparkle, retract and delete machinery
    /// remain one rainbow-kitty family; only its dark body geometry differs"*,
    /// and `docs/design/RAINBOW-TRAIL-ONE-STORY.md` §"every *kitty-named*
    /// spelling actually draws the cat" reads false for a spelling that is
    /// literally `rainbow kitty underline`.
    ///
    /// It is the whole underline LIST and not just the kitty-named member,
    /// because `prefs::CURSOR_TRAIL_STYLE_ALIASES` canonicalises all four onto
    /// `"rainbow kitty underline"` and `cursor_trail_style_aliases_agree_with_
    /// engine_parse` requires an alias and its canonical to draw the same
    /// animal. The flying head keeps every spelling it was promised —
    /// `Self::style_names_flying_kitty` plus the bare `nyan` / `rainbow` /
    /// `nyan rainbow` aliases, none of which name a geometry.
    #[must_use]
    pub fn style_names_kitty_pet(s: &str) -> bool {
        let s = s.trim();
        [
            "rainbow kitty pet",
            "kitty pet",
            "pet kitty",
            // The owner's spelling, and the bare word. See the note above: the
            // flying head keeps `style_names_flying_kitty` and the `nyan`/
            // `rainbow` aliases, and these two now mean what they say.
            "rainbow kitty",
            "kitty",
        ]
        .iter()
        .any(|o| s.eq_ignore_ascii_case(o))
            || Self::style_names_underline_ribbon(s)
            // …and the FLAT spelling (2026-09-13), for the same reason: it is
            // the A/B twin of the default body, so it must differ from the
            // default in the BODY and in nothing else — the same resident.
            || Self::style_names_flat_ribbon(s)
            // …and the TALL spellings, third time, same reason — and this one
            // was the sharpest, because `… tall` names the geometry the
            // DEFAULT ALREADY DRAWS. `ribbon_tall` is
            // `!style_names_underline_ribbon`, so `rainbow kitty tall` and
            // `rainbow kitty pet` resolve to the SAME geometry; the word
            // changed nothing it names and swapped the ANIMAL instead,
            // because this list is a whole-string equality that never learned
            // the spelling. Both places a user reads call it "an explicit
            // spelling of the DEFAULT" (the config doc and the starter
            // aterm.toml), and the default is the resident — so an explicit
            // spelling of the default that drew a different animal was the
            // naming trap this list exists to close, for the third time.
            || Self::style_names_tall_ribbon(s)
    }

    /// Whether this style string explicitly asks for the OLD FLYING KITTY HEAD
    /// ([`crate::kitty_cursor`]) — the rare, earned flypast — on the same
    /// rainbow ribbon.
    ///
    /// The escape hatch for [`Self::style_names_kitty_pet`] taking over
    /// `rainbow kitty`. It is not consulted by the draw path at all (the draw
    /// path asks [`Self::style_names_any_pet`], and the head is simply what a
    /// non-pet [`GlowStyle::RainbowKitty`] draws); it exists so the spellings
    /// are declared in ONE place, so the picker can offer the head as a real
    /// choice, and so
    /// `every_kitty_spelling_draws_the_resident_and_flying_stays_reachable` can
    /// prove the two lists never overlap — an overlap would make one string mean both animals
    /// and the pet would silently win.
    #[must_use]
    #[cfg(test)]
    pub fn style_names_flying_kitty(s: &str) -> bool {
        let s = s.trim();
        ["rainbow kitty flying", "flying kitty", "kitty flying"]
            .iter()
            .any(|o| s.eq_ignore_ascii_case(o))
    }

    /// Whether this style string asks for the pet companion drawn as a **dog**
    /// ([`crate::kitty_pet::PetSpecies::Dog`]).
    ///
    /// The kitty-pet predicate's twin, and the same reasoning: the dog is a
    /// SPECIES of the pet, not an eleventh trail. It resolves to
    /// [`GlowStyle::RainbowKitty`] like the cat pet does, so the ribbon, the
    /// starfield, the chip melody and the sound palette are untouched — the
    /// only thing that changes is which animal walks in front of the caret.
    #[must_use]
    pub fn style_names_dog_pet(s: &str) -> bool {
        let s = s.trim();
        ["rainbow dog pet", "dog pet", "pet dog", "rainbow puppy pet"]
            .iter()
            .any(|o| s.eq_ignore_ascii_case(o))
    }

    /// Whether this style string asks for the full-body pet at all, in any
    /// species. The one predicate the render path should ask; use
    /// [`Self::style_names_dog_pet`] afterwards only to pick the skin.
    #[must_use]
    pub fn style_names_any_pet(s: &str) -> bool {
        Self::style_names_kitty_pet(s) || Self::style_names_dog_pet(s)
    }

    /// Whether this style string selects the **classic full-height ribbon** —
    /// the v0.43 presentation geometry where a continuous seven-anchor rainbow
    /// fills the glyph cell and the letters sit inside it.
    ///
    /// TRUE for exactly these four explicit spellings. Every other recognized
    /// rainbow-kitty spelling — the bare name, the cat/dog pet aliases, the
    /// flying head, `nyan`/`rainbow` — draws the TALL body, which is the
    /// DEFAULT again (owner, 2026-08-29: "WHERE IS MY TALL RIBBON"). It was
    /// the default from `d0d0b863` through v0.63.0; `317f765a` flipped it to
    /// the highlighter on a claimed ruling the owner did not give, and this
    /// restores it. Only the `… underline` aliases select the
    /// highlighter-plus-under-baseline hybrid. Both resolve to
    /// [`GlowStyle::RainbowKitty`], so every style-keyed table — momentum law,
    /// starfield, sound palette, retract machinery — is shared; only the body
    /// geometry forks at [`GlowConfig::ribbon_tall`].
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

    /// Whether this style string spells the post-v0.43
    /// highlighter-plus-under-baseline ribbon EXPLICITLY.
    ///
    /// This is a raw-spelling predicate, not a general geometry query. A bare
    /// `rainbow kitty` — and every pet/flying/`nyan` spelling — draws the
    /// default tall body while returning `false` here and from
    /// [`Self::style_names_tall_ribbon`]. The resolved geometry is
    /// `!style_names_underline_ribbon(raw)`; use this predicate only when what
    /// the string explicitly says matters (for example, the Settings label).
    ///
    /// Kept as a raw-string predicate rather than another [`GlowStyle`] for the
    /// same reason as the pet and classic-body predicates: the cached classic field,
    /// scheduling, admission, companion, sound, sparkle, retract and delete
    /// machinery remain one rainbow-kitty family; only its dark body geometry
    /// differs.
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

    /// Whether this style string spells the FLAT body EXPLICITLY — the
    /// 2026-09-13 tall body as it was before the comet and its vivid rail
    /// (`docs/design/RAINBOW-KITTY-V2.md` §30): the same shape at every
    /// cell, the bed alone below the row bottom, a uniform 18 ms edge-in.
    ///
    /// THE A/B TWIN of the default (the owner, 2026-09-13: *"a wider rainbow
    /// that seems to be painted from the cursor instead of seems to be
    /// painting to the screen"*, *"I don't see much yellow?"*). Every other
    /// rainbow-kitty spelling draws the COMET body — fattest at the hand,
    /// thinner behind it — with the VIVID RAIL under the baseline and the
    /// FROM-THE-HAND attack; this spelling restores the flat body byte for
    /// byte, pinned by `the_flat_spelling_collapses_every_comet_branch_byte_for_byte`.
    /// A raw-spelling predicate like [`Self::style_names_underline_ribbon`]:
    /// it resolves to [`GlowStyle::RainbowKitty`] like every other rainbow
    /// spelling, draws the resident pet like the bare name, and forks at
    /// [`GlowConfig::ribbon_flat`] alone. It composes with the geometry
    /// spellings' default only: `rainbow kitty flat` is the TALL flat body.
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

    /// The RESOLVED style's diagnostic name — what `trail status` prints
    /// beside the raw config string, so a spelling that fell back to the
    /// default is legible as a fallback rather than as an endorsement. Stable
    /// wire tokens (hyphenated, lower case); not a config spelling.
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

    /// Whether a resolved `classic` spelling names the MONO face — the
    /// two-tone `accent → color` tracer — rather than the default spectrum.
    ///
    /// A raw-token predicate, like the ribbon-geometry and pet-species forks:
    /// both spellings resolve to [`Self::Classic`], so every style-keyed table
    /// in the engine is untouched and the fork lands at the one place that
    /// picks a colour.
    #[must_use]
    pub fn style_names_classic_mono(style_raw: &str) -> bool {
        let s = style_raw.trim();
        ["classic mono", "classic lumen", "mono classic"]
            .iter()
            .any(|o| s.eq_ignore_ascii_case(o))
    }

    /// Parse a config string (case-insensitive); unknown → `Lumen`.
    pub fn parse(s: &str) -> Self {
        // Case-insensitive WITHOUT allocating. The GUI resolves this once per
        // immutable config-asset generation, not on the redraw hot path.
        // `eq_ignore_ascii_case` instead of a `to_ascii_lowercase()` heap alloc per frame.
        let s = s.trim();
        // A `pack:<id>` spelling names a Trail Pack: it resolves to the DATA-driven
        // custom variant. The resolved params are attached by the app-layer resolver
        // (`glow_config`), not here — `parse` only classifies the string.
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
            // The BARE word, admitted with the pet (`style_names_kitty_pet`):
            // the companion predicate is only ever consulted for a style that
            // already parsed to `RainbowKitty`, so a spelling that names the pet
            // has to land here or it would resolve to `Lumen` and the pet would
            // be gated off by `resident_pet_owner_present`'s style term.
            "kitty",
            "nyan rainbow",
            "nyan",
            "rainbow",
            // …and the explicit FLYING-HEAD spellings
            // (`style_names_flying_kitty`): the same ribbon, the same style, the
            // head instead of the resident — a companion choice, never an
            // eleventh GlowStyle.
            "rainbow kitty flying",
            "flying kitty",
            "kitty flying",
            // The PET spellings resolve to the SAME style: the pet swaps the
            // companion, not the trail. The ribbon, the starfield, the chip
            // melody and the whole sound palette are the rainbow kitty's and
            // stay exactly as they are — only the animal riding in front of the
            // caret changes, which is why this is a companion predicate
            // (`style_names_kitty_pet`) and not an eleventh GlowStyle.
            "rainbow kitty pet",
            "kitty pet",
            "pet kitty",
            // …and the DOG spellings for the same reason again: a different
            // species of the same companion, riding the same trail.
            "rainbow dog pet",
            "dog pet",
            "pet dog",
            "rainbow puppy pet",
            // …and the presentation aliases. The v0.43 full-height body is the
            // default; `… underline` explicitly selects the quieter
            // highlighter-plus-strip shoulder, while `… tall` spells the
            // default geometry out loud. Both are PRESENTATIONS of this same
            // style, not extra GlowStyle variants.
            "rainbow kitty tall",
            "rainbow tall",
            "tall rainbow",
            "nyan tall",
            "rainbow kitty underline",
            "rainbow underline",
            "underline rainbow",
            "nyan underline",
            // …and the FLAT spellings (`style_names_flat_ribbon`, 2026-09-13):
            // the A/B twin of the default comet body and its vivid rail, a
            // PRESENTATION of this same style like the two above.
            "rainbow kitty flat",
            "rainbow flat",
            "flat rainbow",
            "nyan flat",
            // THE ENGINE WORD (§17.3, phases 6-7): v2 IS the rainbow kitty,
            // so the phase-3 `v2` spellings are kept as no-op aliases of the
            // bare spelling — an A/B config written during the seam era
            // still loads. The `v1` escape spellings died with v1 (phase 7,
            // 2026-09-06): they parse to `Lumen` like any unknown word.
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
            // THE SALVAGE. `classic` is the name offered in the picker; the
            // version spellings are here because the way an owner asks for this
            // trail is by the release they remember it from.
            "classic",
            "classic wake",
            // The MONO face is a PRESENTATION of this same style — the colour
            // closure forks on the spelling (`style_names_classic_mono`), the
            // style does not.
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

/// Resolved tunables (Copy so the host reads it out before borrowing window state).
/// `PartialEq` (structural; never `Eq` — f32 fields) backs the `last_cfg`
/// compare-on-write in [`CursorGlow::tick`]: two configs that compare equal
/// hold the same field values, so they must — and do — produce the identical
/// frame. The converse does NOT hold, and has not since [`Self::audible`]
/// joined the struct: that field is AUDIO-only, so two configs differing in
/// it alone are unequal and still draw a byte-identical frame. Nothing needs
/// a fade or a reset on a focus flip; the cost is one short-circuiting
/// re-store of `last_cfg`.
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
    /// Additive brightness scale 0.0..=1.0 (0 ⇒ effectively off).
    ///
    /// THE VISUAL HALF of what used to be one scalar with three folded
    /// meanings: the user's brightness knob, the accessibility policy
    /// (`Reduce Motion`), and the host's performance headroom (the load-shed
    /// envelope). All three still land here, and all three still dim the
    /// light exactly as before — see [`Self::audible`] for the half that
    /// stopped riding along.
    pub intensity: f32,
    /// THE AUDIO HALF, carrying ONE fact: should a key pressed on this tick's
    /// window be HEARD, stated independently of how bright the trail is.
    ///
    /// Two rulings live in this split. A key-time click costs no GPU, so a
    /// frame the host sheds for performance is still a heard key; and
    /// `Reduce Motion` is a MOTION setting, not an audio setting, so it dims
    /// the light without closing the key seam. Both used to mute every
    /// keystroke because the engine read `intensity <= 0` as "silent".
    ///
    /// What still decides audibility elsewhere, and is deliberately NOT here:
    /// the master switch and serious mode arrive as [`Self::enabled`], and
    /// the sound knobs (`trail_sounds`, `trail_sound_volume`, the resize
    /// quiet window, a dead audio worker) gate host-side in
    /// `keystroke_click_audible` before a cue is ever minted. What the host
    /// must put here is the one term the engine cannot see: whether the key
    /// landed in THIS window.
    pub audible: bool,
    /// Bloom-crown radius in cells (0 ⇒ no crown, comet only).
    pub radius: f32,
    /// Landing-ring "ping" on a jump.
    pub ring: bool,
    /// Whether the theme background is DARK. Additive light needs a dark
    /// ground; on LIGHT themes the vapor (smoke/steam) switches to
    /// [`HaloMode::Over`] source-over veils so it reads on white too.
    /// Default true.
    pub dark_theme: bool,
    /// The theme's resolved DEFAULT FOREGROUND and BACKGROUND, `0x00RRGGBB`,
    /// with OSC 10/11 and DECSCNM already folded in by the host (the canonical
    /// expression wherever a `Terminal` is in scope is
    /// `rgb_to_u32(terminal_blank_cell(term).fg / .bg)`).
    ///
    /// Only the fresh-typed GLYPH TINT reads them, and it reads them for two
    /// things it cannot do without: it ANCHORS on `theme_fg` so the tint hands
    /// the foreground back bit-exactly instead of stepping to a guessed
    /// constant, and it BOUNDS itself against `theme_bg` by real WCAG contrast
    /// instead of against a fixed luminance chosen for the light themes that
    /// happened to be considered.
    ///
    /// A host that cannot resolve one may pass [`aterm_core::render::COLOR_UNSET`];
    /// the tint suppresses itself rather than masking the sentinel's high byte
    /// into a colour (which would read as pure black and flip the ink side).
    /// `fg == bg` — a conceal-shaped theme — suppresses for the same reason
    /// `floor_min_contrast_fg` refuses to reveal what a theme concealed.
    pub theme_fg: u32,
    pub theme_bg: u32,
    /// Whether this style draws its OWN additive comet BEAM (the anti-aliased
    /// streak of light along the swept path). FALSE for the two styles whose
    /// streak is drawn elsewhere: `water` (its fluid wave wake — no laser
    /// beam; WATER-1) and `rainbow kitty` (its flowing ribbon IS the streak). The
    /// default `comet` layers this beam UNDER its faint cell-body ember bed —
    /// the beam is what makes the streak spatially continuous instead of
    /// grid-quantized blocks. Derive it with [`style_has_beam`], never from
    /// `style` alone — `lumen` (and any unknown string) parses to [`GlowStyle::Lumen`]
    /// yet may need different beams.
    pub beam: bool,
    /// Horizontal ATTACH POINT of the live-cursor bridge, as a fraction of the
    /// cell width (`0.5` = the cell centre, the classic look). The host sets
    /// ~`0.08` while the cursor is a thin BAR (DECSCUSR bar / `cursor_style
    /// beam`), so the streak noses precisely into the bar instead of
    /// overshooting it by half a cell — the light visibly leaves the cursor.
    pub head_dx: f32,
    /// The resolved **Trail Pack** params when `style == GlowStyle::Custom`
    /// (`None` for every built-in style). This is the ONE field the custom
    /// interpreter reads; the built-in emit/spawn paths never touch it (they
    /// dispatch on `style`), so the ten built-ins stay byte-identical. Carried
    /// INLINE (it is `Copy`, so `GlowConfig` stays `Copy`) exactly like the
    /// already-resolved `color`/`duration`/`length` — the app-layer resolver
    /// looks a `pack:<id>` up in its registry once, off the frame path, and
    /// fills this in.
    pub pack: Option<TrailParams>,
    // THE TYPING-WAKE PERSISTENCE DIAL IS DELETED (`wake_persist_s`, retired
    // 2026-09-16). It promised "the plume is literally the last
    // `wake_persist_s` seconds of typing, to scale" — the v1 rainbow kitty's
    // model, where a continuous plume replayed a fixed window of recent
    // travel. `RAINBOW-KITTY-V2.md` §17.3 phase 7 deleted the walk that read
    // it, and this field survived that deletion as a value every construction
    // site wrote and no frame ever read: a compiler proof (delete the field,
    // build the crate) returned nine struct-literal WRITES and one read that
    // was the field copying itself forward across a reconfigure.
    //
    // There is no duration to re-wire it to. The v2 mark's extent is the set
    // of cells the hand actually laid (one per key, uncapped) and its clock is
    // the cohort's — `Ribbon::cell_life` prices a cell from `cfg.duration`
    // (`cursor_trail_ms`, a shipped dial) and floors it at the phrase rest the
    // typist's own inter-key interval sets, so "how much recent typing you
    // see" is now the trail-duration dial plus the melody, not a window.
    // `WAKE_LIFE_S` / `WAKE_MAX_CELLS` in `rainbow_kitty::ribbon` are the only
    // live things still called a wake, and they are the JUMP corridor
    // (`RAINBOW-PATH-V3.md` §2.7) — a different effect with design-law
    // constants, not this dial's mechanism under a new name.
    /// The rainbow ribbon's DARK-THEME TRANSVERSE PRESENTATION — tall body or
    /// explicit underline/highlighter.
    ///
    /// `false`, selected only by
    /// [`GlowStyle::style_names_underline_ribbon`], is the explicit underline
    /// presentation: an under-baseline strip plus its quieter glyph-band
    /// highlighter and momentum bloom toward the tapered typed streak
    /// ([`RAINBOW_RIBBON_TOP`]). `true` is the DEFAULT (owner, 2026-08-29:
    /// "WHERE IS MY TALL RIBBON"): it restores both the v0.43 TALL upward
    /// reach and its full-strength glyph-band shoulder, so the gradient fills
    /// the swept glyph cell. Both ordinary rainbow spellings and the four
    /// explicit `… tall` aliases select it.
    ///
    /// The raw-name predicates are NOT complements: a bare `rainbow kitty`
    /// answers `false` to both [`GlowStyle::style_names_tall_ribbon`] and
    /// [`GlowStyle::style_names_underline_ribbon`] even though its resolved
    /// geometry is tall. Resolve geometry with
    /// `!GlowStyle::style_names_underline_ribbon(raw)`.
    ///
    /// THERE IS NO SECOND EMITTER BEHIND THIS FLAG. Step 10 collapsed the two
    /// dark emitters into one polyline ([`CursorGlow::emit_rainbow_ribbon`]);
    /// both spellings run the same sweep and spine, while this bool selects the
    /// polyline's upward reach and shoulder. Everything else about the ribbon
    /// (cohort admission, segment survival, retract, momentum, starfield,
    /// light-theme rail) was already shared and still is.
    pub ribbon_tall: bool,
    /// THE FLAT BODY — the explicit `… flat` spelling
    /// ([`GlowStyle::style_names_flat_ribbon`]), `false` by default.
    ///
    /// `false` draws the rainbow ribbon as the COMET body with its VIVID
    /// RAIL (`docs/design/RAINBOW-KITTY-V2.md` §30, 2026-09-13): the body is
    /// fattest and brightest at the hand and thinner behind it, the reach
    /// below the row bottom carries the full-value spectrum where no letter
    /// can be, and a new cell's light enters from the caret side. `true`
    /// restores the 2026-09-13 flat body byte for byte — the same shape at
    /// every cell, the bed alone below the baseline, the uniform edge-in —
    /// as the owner's A/B control. A presentation of one style, exactly like
    /// [`Self::ribbon_tall`]: the momentum law, the starfield, the sound
    /// palette and the companion are untouched by it, and it composes with
    /// either geometry (the comet is spelled on the tall AND the underline
    /// body). Read by `rainbow_kitty::ribbon` alone.
    pub ribbon_flat: bool,
    /// THE CLASSIC WAKE'S COLOUR FACE, chosen by the resolved spelling exactly
    /// as [`Self::ribbon_tall`] chooses the ribbon's geometry — a presentation
    /// of one style, never a style of its own.
    ///
    /// `false` (plain `classic`) is v0.28's shipped default: a rolling
    /// SPECTRUM sweep that owes nothing to the theme. `true` (`classic mono`)
    /// is v0.28's other face, the two-tone tracer that fades
    /// `accent → color` — which means it follows the theme's cursor colour,
    /// `cursor_trail_color`, and live OSC 12, none of which the spectrum can.
    /// Both were shipped looks of the same v0.28 engine; the salvage keeps
    /// both rather than picking one and calling the other lost.
    pub classic_mono: bool,
}

/// Whether a raw `cursor_trail_style` string draws its own additive comet beam.
///
/// FALSE for exactly two style families: `water` (WATER-1 — water has a
/// dedicated curved wake, not a recolored laser beam) and `rainbow kitty` (its
/// continuous ribbon body IS the streak; a thin beam under it would muddy it);
/// every other style — INCLUDING the default `comet` — shows the anti-aliased
/// pixel-space beam. The comet's [`crate::cursor_trail::CursorTrail`] cell body
/// is GRID-QUANTIZED and reads as gappy blocks on its own, so it layers over the
/// continuous beam: the beam supplies spatial continuity, the cells supply body.
/// See the READABLE_ALPHA_CAP note in `cursor_trail.rs`.
///
/// This MUST key off the raw string, not [`GlowStyle`]: in the GUI resolver the
/// raw `comet` spelling ADDITIONALLY routes the opaque cadence-comet body, so
/// the raw string stays the seam's currency.
#[must_use]
pub fn style_has_beam(style_raw: &str) -> bool {
    style_has_beam_of(GlowStyle::parse(style_raw.trim()), style_raw)
}

/// [`style_has_beam`] for a caller that has ALREADY parsed the style (the
/// per-frame `glow_config` path parses `cursor_trail_style` once for the style
/// enum, then needs the beam flag) — avoids the redundant re-parse while keeping
/// `style_has_beam` and this the SAME source of truth. `style_raw` is still taken
/// (unused today) so that if the beam predicate ever gains a raw-string
/// distinction among `comet`/`beam`/`lumen` — all of which parse to
/// [`GlowStyle::Lumen`] yet may want different beams (see the type docs) — it can
/// be threaded here without touching call sites.
#[must_use]
pub fn style_has_beam_of(style: GlowStyle, _style_raw: &str) -> bool {
    !matches!(style, GlowStyle::Water | GlowStyle::RainbowKitty)
}

/// The default LASER/lightning hue: STORM VIOLET (`0x00RRGGBB`). Applied by
/// the host when the user hasn't pinned an explicit `cursor_trail_color` — the
/// style reads as a lightning strike, and a night strike is a white-violet
/// flash. The blaze whitening still runs, so a hot bolt flashes white and
/// cools back to violet. An explicit trail colour still wins (the monochrome law keeps
/// every emitted quad in whatever hue is chosen).
pub const LASER_DEFAULT_COLOR: u32 = 0x00B4_8CFF;

/// The default BEAM hue: PHOTON ICE-BLUE (`0x00RRGGBB`). Applied by the host
/// when the user hasn't pinned an explicit `cursor_trail_color` — the steady
/// tube reads as a beam of cool light (a searchlight, not a laser weapon), and
/// the theme cursor colour rarely says "light". An explicit trail colour still
/// wins, exactly like [`LASER_DEFAULT_COLOR`].
pub const BEAM_DEFAULT_COLOR: u32 = 0x008C_DCFF;

/// The default SPARKLE hue: STARLIGHT GOLD (`0x00RRGGBB`). Applied by the host
/// when the user hasn't pinned an explicit `cursor_trail_color` — the glitter
/// wand's emitter block glows warm champagne-gold (starlight, not the theme
/// cursor). The ribbon itself stays rainbow (its ramp rolls the live hue); only the
/// cursor emitter and any monochrome accents key off this.
pub const SPARKLE_DEFAULT_COLOR: u32 = 0x00FF_D9A0;

/// The default COMET hue: pale GLACIAL BLUE (`0x00RRGGBB`). Applied by the host
/// when the user hasn't pinned an explicit `cursor_trail_color` — a comet is
/// dirty ice lit white-hot at the nucleus, and that reads icy blue-white, not
/// theme-cursor-coloured. An explicit trail colour still wins, and the whole
/// tail/coma/debris palette re-derives from it (the ramps lerp off the config
/// hues, never this constant).
pub const COMET_DEFAULT_COLOR: u32 = 0x009E_D6FF;
