// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Motion policy: the single accessibility gate for decorative animation.

/// The `motion` config value.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MotionMode {
    /// Follow the available platform reduce-motion sample (default).
    #[default]
    Auto,
    /// Always animate (an explicit user override of the OS setting).
    Full,
    /// Never animate (an explicit user override, especially useful where the OS flag is unavailable or cannot be observed live).
    Reduced,
}

impl MotionMode {
    /// Whether the load-adaptive shed may reduce motion in this mode: an explicit Always animate (`Full`) is the person's own intent and overrides it, so under `Full` the shed (and the switch that enables it) does nothing.
    #[must_use]
    #[inline]
    pub const fn admits_load_shed(self) -> bool {
        !matches!(self, Self::Full)
    }

    /// Parse a config string (case-insensitive, trimmed); unknown → [`Self::Auto`] (use the platform accessibility sample when available). `none` and `off` ask for no motion, so they read as [`Self::Reduced`]: read as unknown, a config written to stop motion played it in full wherever the system allowed.
    #[must_use]
    #[inline]
    pub const fn parse(s: &str) -> Self {
        use crate::cursor_glow::spellings::{eq_ascii_ci, trim_whitespace_bytes};
        let s = trim_whitespace_bytes(s);
        if eq_ascii_ci(s, "full") {
            Self::Full
        } else if eq_ascii_ci(s, "reduced")
            || eq_ascii_ci(s, "reduce")
            || eq_ascii_ci(s, "none")
            || eq_ascii_ci(s, "off")
        {
            Self::Reduced
        } else {
            Self::Auto
        }
    }
}

/// Every decorative animation the policy governs: the effect set the totality proof quantifies over.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MotionEffect {
    /// The Lumen cursor aurora (comet, bloom crown, landing ring, particles): the amplitude scales `GlowConfig::intensity` in `App::redraw_window` / `App::redraw_compose` (0: the animator clears and emits nothing).
    CursorGlow,
    /// Sparkle-word twinkle and jitter (`WordDecorations::tick`): 0 selects the static path (the steady spark and cat still draw; they are not motion).
    WordSparkles,
    /// The smooth-scroll wheel glide (`App::scroll_wheel_animated`): 0 ⇒ the viewport snaps instantly (no eased motion, no glide deadlines armed).
    SmoothScroll,
    /// The scroll-pill fade ramp (`scroll_motion::pill_alpha`): at 0 the pill still shows (it is information) but hides at once at the hold boundary.
    ScrollPill,
    /// The matrix rain (`MatrixRain::set_reduced_motion`, fed at the render tick): 0 means the engine emits nothing (empty channels, fp 0, timers disarmed), the final state rather than a freeze.
    MatrixRain,
    /// Prism wake's output streak (`OutputStreak::tick` reads it as `StreakConfig::intensity`): at 0 the engine resets and emits nothing (no quads, no cue, fp 0).
    OutputStreak,
    /// The upgrade rim (the host's `level_up`): pulse, climb, hue travel and thickening scale with it.
    UpgradeSurge,
    /// The harness card's motion (`HarnessCard::tick`'s `animate`): its intro — the border's drift and specular head, the trail's step wave and shimmer, the kitty's blink and tail flick, the stars' twinkle and burst — and the FLOW after it, for as long as this verdict allows: the border's hue travelling on and Claude's mascot ripple running out from its wand across the card (`crate::ripple`).
    HarnessCard,
    /// The stats deck's live edge (`aterm-gui`'s `stats_live`): its strips' glide to the left
    /// between readings and the beat of its now marker at each reading. At 0 the columns stand in
    /// their slots and the marker stands still; it still turns grey when readings stop, since that
    /// is information, not motion.
    DeckLive,
    /// A native page's motion in place (`docs/DESIGN-init-on-by-default-2026-10-25.md` §5.5): a
    /// working spinner's steps. Rotation in place is not travel, so Reduce Motion keeps it
    /// ([`PageMotion::InPlace`]); Serious Mode, load shedding and Low Power still it to a static
    /// working glyph ([`PageMotion::Fades`]).
    NativeInPlace,
    /// A native page's travel (§5.3–5.5): a glide, a walk, a run, a leap, a pulse, a sheen. Reduce
    /// Motion stills it; its changes become dissolves ([`Self::NativeFade`]).
    NativeTravel,
    /// A native page's crossfade of at most [`NATIVE_FADE_MAX_MS`] (§5.5): what Reduce Motion,
    /// Serious Mode, load shedding and Low Power keep, since a dissolve tells a change apart
    /// without moving anything.
    NativeFade,
    /// The fade of the mode's name a click on the Autopilot chip brings up beside its glyph
    /// (`⤓ land` back to `⤓`, `aterm-gui`'s `claude_lights::ChipWord`): at 0 the name still shows
    /// (it says what the click did) but goes at once when its hold ends.
    ChipWord,
}

/// The longest crossfade [`MotionEffect::NativeFade`] admits, in milliseconds (§5.5).
pub const NATIVE_FADE_MAX_MS: u32 = 150;

impl MotionEffect {
    /// The complete governed-effect set, which the totality proof iterates.
    #[cfg(test)]
    pub const ALL: [Self; 13] = [
        Self::CursorGlow,
        Self::WordSparkles,
        Self::SmoothScroll,
        Self::ScrollPill,
        Self::MatrixRain,
        Self::OutputStreak,
        Self::UpgradeSurge,
        Self::HarnessCard,
        Self::DeckLive,
        Self::NativeInPlace,
        Self::NativeTravel,
        Self::NativeFade,
        Self::ChipWord,
    ];

    /// Whether this effect is functional motion the host's load-shed latch (`perf_reduced`) must
    /// never demote: the terminal answering the user's hand.
    #[must_use]
    #[inline]
    pub const fn load_shed_exempt(self) -> bool {
        match self {
            Self::SmoothScroll | Self::ScrollPill => true,
            Self::CursorGlow
            | Self::WordSparkles
            | Self::MatrixRain
            | Self::OutputStreak
            | Self::UpgradeSurge
            | Self::HarnessCard
            | Self::DeckLive
            | Self::NativeInPlace
            | Self::NativeTravel
            | Self::NativeFade
            | Self::ChipWord => false,
        }
    }
}

/// The resolved motion policy for one window and frame: animations at full amplitude, or every governed amplitude exactly 0.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MotionPolicy {
    /// Animations run at their configured amplitude.
    Full,
    /// Static effects only: every governed animation amplitude is exactly 0.
    Reduced,
}

/// Every nonessential effect suppressed by serious mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SeriousEffect {
    TerminalSound,
    CursorTrail,
    CursorGlow,
    CursorBody,
    CursorCat,
    WordDecorations,
    MatrixRain,
    LevelUp,
    SettingsPreview,
    GpuPostFx,
    /// The harness card's kitty, rainbow trail, stars and border motion: serious mode keeps a static border, so the session still shows as wrapped.
    HarnessCard,
    /// The act ticker's label on the Autopilot chip (`✓ approved` for 3 s, design `docs/DESIGN-harness-autopilot-2026-10-15.md` §9.2.1): serious mode withholds it.
    HarnessTicker,
    /// Prism wake's comet over new program output: decoration, so serious mode takes it with the rest (its pip goes with [`Self::TerminalSound`]).
    OutputStreak,
    /// The beat of the stats deck's now marker at each reading: decoration, so serious mode takes it; the marker, its grey when readings stop and the strips' glide stay.
    DeckBeat,
    /// The steam rising off the stats deck's hot dial: decoration, so serious mode takes it; the dial, its glow and the needle stay.
    DeckSteam,
}

impl SeriousEffect {
    #[cfg(test)]
    const ALL: [Self; 15] = [
        Self::TerminalSound,
        Self::CursorTrail,
        Self::CursorGlow,
        Self::CursorBody,
        Self::CursorCat,
        Self::WordDecorations,
        Self::MatrixRain,
        Self::LevelUp,
        Self::SettingsPreview,
        Self::GpuPostFx,
        Self::HarnessCard,
        Self::HarnessTicker,
        Self::OutputStreak,
        Self::DeckBeat,
        Self::DeckSteam,
    ];
}

/// Pure, total projection from the effective serious-mode bit to each decorative output gate, independent of App state, so transition and render paths share one decision and the truth table below is a complete proof.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SeriousModePolicy {
    serious: bool,
}

impl SeriousModePolicy {
    #[must_use]
    #[inline]
    pub const fn resolve(serious: bool) -> Self {
        Self { serious }
    }

    #[must_use]
    #[inline]
    pub const fn allows(self, effect: SeriousEffect) -> bool {
        match effect {
            SeriousEffect::TerminalSound
            | SeriousEffect::CursorTrail
            | SeriousEffect::CursorGlow
            | SeriousEffect::CursorBody
            | SeriousEffect::CursorCat
            | SeriousEffect::WordDecorations
            | SeriousEffect::MatrixRain
            | SeriousEffect::LevelUp
            | SeriousEffect::SettingsPreview
            | SeriousEffect::GpuPostFx
            | SeriousEffect::HarnessCard
            | SeriousEffect::HarnessTicker
            | SeriousEffect::OutputStreak
            | SeriousEffect::DeckBeat
            | SeriousEffect::DeckSteam => !self.serious,
        }
    }
}

impl MotionPolicy {
    /// Resolve the policy, a pure total function of:
    ///
    /// * `mode`: config `motion` (`Auto` follows `system_reduce`; `Full` /
    ///   `Reduced` override it),
    /// * `system_reduce`: the platform's Reduce Motion sample (live on macOS),
    /// * `focused`: an unfocused window always demotes to static effects,
    ///   whatever the config.
    #[inline]
    #[must_use]
    pub const fn resolve(mode: MotionMode, system_reduce: bool, focused: bool) -> Self {
        let animate = focused
            && match mode {
                MotionMode::Full => true,
                MotionMode::Reduced => false,
                MotionMode::Auto => !system_reduce,
            };
        if animate { Self::Full } else { Self::Reduced }
    }

    /// The animation amplitude for `effect`: `1.0` under [`Self::Full`], exactly `0.0` under [`Self::Reduced`].
    #[inline]
    #[must_use]
    pub const fn amplitude(self, effect: MotionEffect) -> f32 {
        match self {
            Self::Full => 1.0,
            Self::Reduced => match effect {
                MotionEffect::CursorGlow
                | MotionEffect::WordSparkles
                | MotionEffect::SmoothScroll
                | MotionEffect::ScrollPill
                | MotionEffect::MatrixRain
                | MotionEffect::OutputStreak
                | MotionEffect::UpgradeSurge
                | MotionEffect::HarnessCard
                | MotionEffect::DeckLive
                | MotionEffect::NativeInPlace
                | MotionEffect::NativeTravel
                | MotionEffect::NativeFade
                | MotionEffect::ChipWord => 0.0,
            },
        }
    }

    /// Whether `effect` may animate at all (`amplitude > 0`) — the boolean convenience for consumers that gate rather than scale.
    #[inline]
    #[must_use]
    pub const fn animate(self, effect: MotionEffect) -> bool {
        self.amplitude(effect) > 0.0
    }

    /// Resolve how a native page may move (`docs/DESIGN-init-on-by-default-2026-10-25.md`
    /// §5.5–5.6), a pure total function of the same facts [`Self::resolve`] reads and the ones
    /// that quiet a page:
    ///
    /// * an unfocused window, or a page nobody sees, is [`PageMotion::Still`];
    /// * Serious Mode is [`PageMotion::Fades`];
    /// * load shedding (`shed`, the latch) and Low Power (`low_power`) are
    ///   [`PageMotion::Fades`] unless the person asked for full motion (`motion = full`), which
    ///   overrides them as it overrides the shed everywhere ([`MotionMode::admits_load_shed`]);
    /// * otherwise [`Self::resolve`] decides: [`Self::Full`] is [`PageMotion::Full`], and Reduce
    ///   Motion ([`Self::Reduced`] in a focused window) is [`PageMotion::InPlace`].
    #[inline]
    #[must_use]
    pub const fn resolve_page(
        mode: MotionMode,
        system_reduce: bool,
        focused: bool,
        serious: bool,
        shed: bool,
        low_power: bool,
    ) -> PageMotion {
        if !focused {
            return PageMotion::Still;
        }
        if serious || ((shed || low_power) && mode.admits_load_shed()) {
            return PageMotion::Fades;
        }
        match Self::resolve(mode, system_reduce, true) {
            Self::Full => PageMotion::Full,
            Self::Reduced => PageMotion::InPlace,
        }
    }
}

/// How a native page may move, as [`MotionPolicy::resolve_page`] decides it: one home for the
/// person's `motion` setting, Reduce Motion, Serious Mode, load shedding, Low Power and focus.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Default)]
pub enum PageMotion {
    /// Everything moves: travel, in place, crossfades.
    #[default]
    Full,
    /// Reduce Motion: a spinner still steps in place and changes dissolve; nothing travels.
    InPlace,
    /// Serious Mode, load shedding or Low Power: a static working glyph, and crossfades of at
    /// most [`NATIVE_FADE_MAX_MS`].
    Fades,
    /// An unfocused window or a page nobody sees: nothing moves, and on its return the page
    /// jumps to where it stands, with no catch-up replay.
    Still,
}

impl PageMotion {
    /// Every resolution, for the totality proof.
    #[cfg(test)]
    pub const ALL: [Self; 4] = [Self::Full, Self::InPlace, Self::Fades, Self::Still];

    /// Whether `effect` may animate under this resolution. A native effect follows the page's
    /// rows of §5.5; any other effect animates only where the page is [`Self::Full`], as
    /// [`MotionPolicy::Full`] would let it.
    #[inline]
    #[must_use]
    pub const fn animate(self, effect: MotionEffect) -> bool {
        match (self, effect) {
            (Self::Full, _)
            | (Self::InPlace, MotionEffect::NativeInPlace | MotionEffect::NativeFade)
            | (Self::Fades, MotionEffect::NativeFade) => true,
            (Self::InPlace | Self::Fades | Self::Still, _) => false,
        }
    }
}

#[cfg(test)]
#[path = "motion_tests.rs"]
mod tests;
