// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Motion policy: the single accessibility gate for every decorative animation.
//! One pure, total resolution folds config `motion = auto|full|reduced`, the
//! live OS Reduce Motion flag (macOS
//! `NSWorkspace.accessibilityDisplayShouldReduceMotion`, via the apprt seam) and
//! the window's focus into one [`MotionPolicy`], and every animated effect
//! ([`MotionEffect`]) takes its amplitude from it with one call
//! ([`MotionPolicy::amplitude`] / [`MotionPolicy::animate`]).
//!
//! A new motion feature adds a [`MotionEffect`] variant: the exhaustive match in
//! `amplitude` does not compile until its reduced amplitude is decided, and the
//! totality test fails unless that amplitude is exactly 0.
//!
//! # Invariant
//!
//! `MotionPolicy::resolve` is a pure total function of `(config, system_flag,
//! focus)`, and under `Reduced` every governed amplitude is exactly 0:
//!
//! * Tier 0: `aterm_spec::derive::motion_policy_model()`, checked by Trust `ty`
//!   in aterm-spec's `spec_suite/derived_ring_ty.rs` (proved at `Buggy=0`; a
//!   counterexample required at `Buggy=1`, where auto mode ignores the OS flag).
//! * Tier 1: `reduced_motion_totality` enumerates the complete 3×2×2 input
//!   domain × [`MotionEffect::ALL`] over the shipping resolver.
//!
//! # Functional motion and the load-shed latch
//!
//! The host's `App::motion_policy` has a fourth input: the load-adaptive
//! shedding latch (`perf_reduced`), a render-cost heuristic that forces
//! `Reduced` for a 1.5–30 s dwell once presents run over budget, to shed
//! decoration. The wheel/trackpad glide ([`MotionEffect::SmoothScroll`]) and the
//! scroll pill's fade ([`MotionEffect::ScrollPill`]) respond to the user's hand,
//! and shedding them would snap scrolling to whole rows, so
//! [`MotionEffect::load_shed_exempt`] names them and `App::effect_policy`
//! resolves them from the three accessibility facts alone. Under a `Reduced`
//! policy (OS Reduce Motion, `motion = "reduced"`, unfocused) their amplitude is
//! still exactly 0 (owner, 2026-09-22).

/// The `motion` config value. `Auto` (the default, and any unknown string) uses
/// the platform sample: live Reduce Motion on macOS, an attach-time animations
/// sample on Windows, and no OS-driven reduction where no query exists.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MotionMode {
    /// Follow the available platform reduce-motion sample (default).
    #[default]
    Auto,
    /// Always animate (an explicit user override of the OS setting).
    Full,
    /// Never animate (an explicit user override, especially useful where the
    /// OS flag is unavailable or cannot be observed live).
    Reduced,
}

impl MotionMode {
    /// Parse a config string (case-insensitive, trimmed); unknown → [`Self::Auto`]
    /// (use the platform accessibility sample when available).
    pub fn parse(s: &str) -> Self {
        let s = s.trim();
        if s.eq_ignore_ascii_case("full") {
            Self::Full
        } else if s.eq_ignore_ascii_case("reduced") || s.eq_ignore_ascii_case("reduce") {
            Self::Reduced
        } else {
            Self::Auto
        }
    }
}

/// Every decorative animation the policy governs: the effect set the totality
/// proof quantifies over. Each variant names the seam that consumes its
/// amplitude.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MotionEffect {
    /// The Lumen cursor aurora (comet, bloom crown, landing ring, particles): the
    /// amplitude scales `GlowConfig::intensity` in `App::redraw_window` /
    /// `App::redraw_compose` (0: the animator clears and emits nothing).
    CursorGlow,
    /// Sparkle-word twinkle and jitter (`WordDecorations::tick`): 0 selects the
    /// static path (the steady spark and cat still draw; they are not motion).
    WordSparkles,
    /// The M1 smooth-scroll wheel GLIDE (`App::scroll_wheel_animated`): 0 ⇒ the
    /// viewport snaps instantly (no eased motion, no glide deadlines armed).
    SmoothScroll,
    /// The scroll-pill fade ramp (`scroll_motion::pill_alpha`): at 0 the pill
    /// still shows (it is information) but hides at once at the hold boundary.
    ScrollPill,
    /// The "ink that dries" stream fade (the host's `stream_fade`): 0 makes
    /// `fade_permitted` force the instant path, byte-identical to no fade.
    StreamFade,
    /// The matrix rain (`MatrixRain::set_reduced_motion`, fed at the render
    /// tick): 0 means the engine emits nothing (empty channels, fp 0, timers
    /// disarmed), the final state rather than a freeze.
    MatrixRain,
    /// Robi's tip bubble slide (`robi_bubble::layout` scales `RobiBubble::rise`
    /// by it): at 0 the bubble holds its rest position and only the alpha ramp
    /// remains; it still shows.
    NoticePill,
    /// Robi's show (`aterm_effects::robi`, gated in the redraw's Robi block): at 0
    /// no show starts and a live one stops (no robot drawn); his tips return
    /// with the next show once motion returns.
    Robi,
    /// Prism wake's output streak (`OutputStreak::tick` reads it as
    /// `StreakConfig::intensity`): at 0 the engine resets and emits nothing (no
    /// quads, no cue, fp 0). A pin rather than an ease: an effect answering
    /// program output is one a motion-sensitive user needs gone.
    OutputStreak,
    /// The upgrade rim (the host's `level_up`): pulse, climb, hue travel and
    /// thickening scale with it. At 0 the rim is still at fixed alpha and
    /// thickness: the information stays, the movement goes.
    UpgradeSurge,
    /// The presence rim's edge ripple (the host's `presence`): a 300 ms flash
    /// along the driven window's rim when a peer's turn submit landed. At 0 the
    /// ripple never starts; the steady rim carries the information.
    PresenceRipple,
    /// The message band's live indicator: a busy row's braille spinner and
    /// whole-row comet (`aterm_messages::Meter::busy`), the determinate bar's
    /// glide and glint, the completion echo's glow and fade. At 0 it is
    /// `Pace::Still`: a bar moving only with its data, a busy row's glyph over
    /// its unlit track, a still ✓ on completion.
    BandProgress,
}

impl MotionEffect {
    /// The complete governed-effect set in `seq` order, which the totality proof
    /// iterates. [`Self::seq`]'s exhaustive match and `effect_set_is_complete`
    /// keep them together. Test-only; production gates via [`MotionPolicy`].
    #[cfg(test)]
    pub const ALL: [Self; 12] = [
        Self::CursorGlow,
        Self::WordSparkles,
        Self::SmoothScroll,
        Self::ScrollPill,
        Self::StreamFade,
        Self::MatrixRain,
        Self::NoticePill,
        Self::Robi,
        Self::OutputStreak,
        Self::UpgradeSurge,
        Self::PresenceRipple,
        Self::BandProgress,
    ];

    /// Stable index of each variant (0..ALL.len()). Exhaustive, so a new variant
    /// must be indexed here and `effect_set_is_complete` fails until
    /// [`Self::ALL`] carries it.
    #[cfg(test)]
    fn seq(self) -> usize {
        match self {
            Self::CursorGlow => 0,
            Self::WordSparkles => 1,
            Self::SmoothScroll => 2,
            Self::ScrollPill => 3,
            Self::StreamFade => 4,
            Self::MatrixRain => 5,
            Self::NoticePill => 6,
            Self::Robi => 7,
            Self::OutputStreak => 8,
            Self::UpgradeSurge => 9,
            Self::PresenceRipple => 10,
            Self::BandProgress => 11,
        }
    }

    /// Whether this effect is functional motion the load-shed latch must never
    /// demote (see the module doc): the terminal answering the user's hand. The
    /// same line [`SeriousModePolicy`] draws. Exhaustive, so a new
    /// [`MotionEffect`] must be classified.
    ///
    /// Accessibility is not exemptable: OS Reduce Motion, `motion = "reduced"`
    /// and unfocused demotion still zero an exempt effect through `resolve`.
    #[must_use]
    pub const fn load_shed_exempt(self) -> bool {
        match self {
            Self::SmoothScroll | Self::ScrollPill => true,
            Self::CursorGlow
            | Self::WordSparkles
            | Self::StreamFade
            | Self::MatrixRain
            | Self::NoticePill
            | Self::Robi
            | Self::OutputStreak
            | Self::UpgradeSurge
            | Self::PresenceRipple
            | Self::BandProgress => false,
        }
    }
}

/// The resolved motion policy for one window and frame: animations at full
/// amplitude, or every governed amplitude exactly 0. Obtained via the host's
/// `App::motion_policy`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MotionPolicy {
    /// Animations run at their configured amplitude.
    Full,
    /// Static effects only: every governed animation amplitude is exactly 0.
    Reduced,
}

/// Every nonessential effect suppressed by serious mode. Kept apart from
/// [`MotionEffect`]: serious mode removes decorative output but must not demote
/// functional motion (smooth scrolling, the scroll pill), cursor blink, the
/// visual bell or window attention.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SeriousEffect {
    TerminalSound,
    CursorTrail,
    CursorGlow,
    CursorBody,
    CursorCat,
    WordDecorations,
    MatrixRain,
    StreamFade,
    LevelUp,
    SettingsPreview,
    GpuPostFx,
    Robi,
    /// The presence rim's turn-submit ripple: decoration on a rim that
    /// otherwise moves only with a fact.
    PresenceRipple,
    /// The message band's live indicator motion, the busy spinner and comet
    /// included. The band and its words stay; the indicator takes its still
    /// form.
    BandProgress,
}

impl SeriousEffect {
    #[cfg(test)]
    const ALL: [Self; 14] = [
        Self::TerminalSound,
        Self::CursorTrail,
        Self::CursorGlow,
        Self::CursorBody,
        Self::CursorCat,
        Self::WordDecorations,
        Self::MatrixRain,
        Self::StreamFade,
        Self::LevelUp,
        Self::SettingsPreview,
        Self::GpuPostFx,
        Self::Robi,
        Self::PresenceRipple,
        Self::BandProgress,
    ];

    #[cfg(test)]
    fn seq(self) -> usize {
        match self {
            Self::TerminalSound => 0,
            Self::CursorTrail => 1,
            Self::CursorGlow => 2,
            Self::CursorBody => 3,
            Self::CursorCat => 4,
            Self::WordDecorations => 5,
            Self::MatrixRain => 6,
            Self::StreamFade => 7,
            Self::LevelUp => 8,
            Self::SettingsPreview => 9,
            Self::GpuPostFx => 10,
            Self::Robi => 11,
            Self::PresenceRipple => 12,
            Self::BandProgress => 13,
        }
    }
}

/// Pure, total projection from the effective serious-mode bit to each
/// decorative output gate, independent of App state, so transition and render
/// paths share one decision and the truth table below is a complete proof.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SeriousModePolicy {
    serious: bool,
}

impl SeriousModePolicy {
    #[must_use]
    pub const fn resolve(serious: bool) -> Self {
        Self { serious }
    }

    #[must_use]
    pub const fn allows(self, effect: SeriousEffect) -> bool {
        match effect {
            SeriousEffect::TerminalSound
            | SeriousEffect::CursorTrail
            | SeriousEffect::CursorGlow
            | SeriousEffect::CursorBody
            | SeriousEffect::CursorCat
            | SeriousEffect::WordDecorations
            | SeriousEffect::MatrixRain
            | SeriousEffect::StreamFade
            | SeriousEffect::LevelUp
            | SeriousEffect::SettingsPreview
            | SeriousEffect::GpuPostFx
            | SeriousEffect::Robi
            | SeriousEffect::PresenceRipple
            | SeriousEffect::BandProgress => !self.serious,
        }
    }
}

impl MotionPolicy {
    /// Resolve the policy, a pure total function of:
    ///
    /// * `mode`: config `motion` (`Auto` follows `system_reduce`; `Full` /
    ///   `Reduced` override it),
    /// * `system_reduce`: the live OS Reduce Motion flag,
    /// * `focused`: an unfocused window always demotes to static effects,
    ///   whatever the config.
    #[must_use]
    pub fn resolve(mode: MotionMode, system_reduce: bool, focused: bool) -> Self {
        let animate = focused
            && match mode {
                MotionMode::Full => true,
                MotionMode::Reduced => false,
                MotionMode::Auto => !system_reduce,
            };
        if animate { Self::Full } else { Self::Reduced }
    }

    /// The animation amplitude for `effect`: `1.0` under [`Self::Full`], exactly
    /// `0.0` under [`Self::Reduced`]. The `Reduced` arm matches every effect
    /// explicitly, so a new variant needs a decision and the totality test
    /// rejects anything nonzero.
    #[must_use]
    pub fn amplitude(self, effect: MotionEffect) -> f32 {
        match self {
            Self::Full => 1.0,
            Self::Reduced => match effect {
                MotionEffect::CursorGlow
                | MotionEffect::WordSparkles
                | MotionEffect::SmoothScroll
                | MotionEffect::ScrollPill
                | MotionEffect::StreamFade
                | MotionEffect::MatrixRain
                | MotionEffect::NoticePill
                | MotionEffect::Robi
                | MotionEffect::OutputStreak
                | MotionEffect::UpgradeSurge
                | MotionEffect::PresenceRipple
                | MotionEffect::BandProgress => 0.0,
            },
        }
    }

    /// Whether `effect` may animate at all (`amplitude > 0`) — the boolean
    /// convenience for consumers that gate rather than scale.
    #[must_use]
    pub fn animate(self, effect: MotionEffect) -> bool {
        self.amplitude(effect) > 0.0
    }
}

#[cfg(test)]
mod tests {
    //! Tier 1: these tests enumerate the invariant the ty model
    //! `motion_policy_model()` carries (aterm-spec, checked by Trust `ty`) over
    //! the shipping `MotionPolicy`. The input domain is finite (3 modes × 2
    //! system flags × 2 focus states × every effect), so the enumeration is a
    //! complete proof.

    use super::{MotionEffect, MotionMode, MotionPolicy, SeriousEffect, SeriousModePolicy};

    /// Reduce Motion is a motion setting, not an audio setting. The audio
    /// verdict depends on focus and the user's brightness knob, never on the
    /// motion policy, so Reduce Motion (and the load-shed envelope on the same
    /// scalar) dims the light without closing the key seam.
    ///
    /// Drives the real engine: the policy's amplitude folds
    /// `GlowConfig::intensity` as `App::cursor_frame` does, `CursorGlow::tick`
    /// runs, and the assertion reads `sound_seam_open()`. If the seam reads the
    /// folded scalar, every reduced-and-focused row fails.
    #[test]
    fn reduce_motion_never_mutes_typing() {
        use crate::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
        use std::time::{Duration, Instant};

        let geom = Geom {
            cw: 8,
            ch: 16,
            rows: 24,
            cols: 80,
            origin_x: 0,
            origin_y: 0,
            win_w: 640,
            win_h: 384,
            head: 0,
        };
        let base = GlowConfig {
            classic_mono: false,
            ribbon_tall: true,
            ribbon_flat: false,
            enabled: true,
            dark_theme: true,
            theme_fg: 0x00C8_D3F5,
            theme_bg: 0x001A_1B26,
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
        };
        let t0 = Instant::now();
        let mut seen_dark_and_heard = false;

        for mode in [MotionMode::Auto, MotionMode::Full, MotionMode::Reduced] {
            for system_reduce in [false, true] {
                for focused in [false, true] {
                    let p = MotionPolicy::resolve(mode, system_reduce, focused);
                    // The host's fold, called rather than transcribed: the
                    // motion amplitude scales the visual scalar; `audible` is
                    // focus and the brightness knob (nonzero here), not `p`. No
                    // load shed in this domain (envelope 1.0).
                    let mut cfg = base;
                    crate::sound_policy::fold_window_audibility(
                        &mut cfg,
                        p.amplitude(MotionEffect::CursorGlow),
                        1.0,
                        focused,
                    );
                    let mut glow = CursorGlow::default();
                    let mut out = Vec::new();
                    glow.tick(Some((2, 0)), t0, &base, geom, &mut out);
                    out.clear();
                    let t1 = t0 + Duration::from_millis(16);
                    glow.tick(Some((2, 1)), t1, &cfg, geom, &mut out);

                    let case = format!("{mode:?}, sys={system_reduce}, focused={focused}");
                    assert_eq!(
                        glow.sound_seam_open(),
                        focused,
                        "audibility must depend on focus alone ({case})"
                    );
                    if !focused {
                        assert!(
                            !glow.cue_keystroke_kind(t1, crate::trail_sound::SoundKind::Typed),
                            "an unfocused window must stay silent ({case})"
                        );
                    }
                    // NON-VACUITY: there really is a point where the light is
                    // dark and the keys are heard, which is the whole fix.
                    if focused && p == MotionPolicy::Reduced {
                        assert_eq!(cfg.intensity, 0.0, "the light is folded dark ({case})");
                        assert!(
                            glow.cue_keystroke_kind(t1, crate::trail_sound::SoundKind::Typed),
                            "a reduced-motion focused window is still heard ({case})"
                        );
                        seen_dark_and_heard = true;
                    }
                }
            }
        }
        assert!(
            seen_dark_and_heard,
            "the dark-and-heard case must be reachable, or this proves nothing"
        );
    }

    /// Reduced-motion totality: `resolve` is total over `(config, system_flag,
    /// focus)`, its truth table is "animate ⟺ focused ∧ (full ∨ (auto ∧
    /// ¬system))", and under `Reduced` every governed amplitude is exactly 0
    /// (`animate()` false). Under `Full` every amplitude is exactly 1, so both
    /// branches are reachable.
    #[test]
    fn reduced_motion_totality() {
        let mut saw_full = false;
        let mut saw_reduced = false;
        for mode in [MotionMode::Auto, MotionMode::Full, MotionMode::Reduced] {
            for system_reduce in [false, true] {
                for focused in [false, true] {
                    // Totality: every point of the domain resolves (no panic,
                    // by construction of the exhaustive loops).
                    let p = MotionPolicy::resolve(mode, system_reduce, focused);
                    // The exact truth table.
                    let expect_full = focused
                        && (mode == MotionMode::Full
                            || (mode == MotionMode::Auto && !system_reduce));
                    assert_eq!(
                        p == MotionPolicy::Full,
                        expect_full,
                        "resolve({mode:?}, sys={system_reduce}, focused={focused}) = {p:?}"
                    );
                    saw_full |= p == MotionPolicy::Full;
                    saw_reduced |= p == MotionPolicy::Reduced;
                    // The amplitude law, quantified over the ENUMERATED effect
                    // set — an effect missing from ALL cannot be claimed proven.
                    for e in MotionEffect::ALL {
                        let a = p.amplitude(e);
                        match p {
                            MotionPolicy::Reduced => {
                                assert_eq!(
                                    a, 0.0,
                                    "under Reduced, {e:?} amplitude must be EXACTLY 0"
                                );
                                assert!(!p.animate(e), "under Reduced, {e:?} must not animate");
                            }
                            MotionPolicy::Full => {
                                assert_eq!(a, 1.0, "under Full, {e:?} amplitude must be 1");
                                assert!(p.animate(e), "under Full, {e:?} must animate");
                            }
                        }
                    }
                }
            }
        }
        // Non-vacuity: both branches genuinely occur in the domain.
        assert!(saw_full, "the Full policy must be reachable");
        assert!(saw_reduced, "the Reduced policy must be reachable");
    }

    /// The load-shed exemption is exactly the functional pair (wheel glide and
    /// scroll pill): a decorative effect in it would paint under overload. Every
    /// exempt effect is still exactly 0 under `Reduced`.
    #[test]
    fn load_shed_exemption_is_exactly_functional_motion() {
        for e in MotionEffect::ALL {
            let functional = matches!(e, MotionEffect::SmoothScroll | MotionEffect::ScrollPill);
            assert_eq!(
                e.load_shed_exempt(),
                functional,
                "{e:?}: only functional motion may be exempt from load shedding"
            );
            if functional {
                assert_eq!(
                    MotionPolicy::Reduced.amplitude(e),
                    0.0,
                    "{e:?}: the exemption must not weaken the Reduced-motion proof"
                );
            }
        }
        assert_eq!(
            MotionEffect::ALL
                .iter()
                .filter(|e| e.load_shed_exempt())
                .count(),
            2,
            "non-vacuity: the exempt set is populated, and by exactly the pair"
        );
    }

    /// Negative control (the ty model's `Buggy=1` twin): a resolver ignoring the
    /// OS flag under `Auto` differs from the proven one exactly at (Auto,
    /// sys=true, focused=true), so the truth-table assertion catches it.
    #[test]
    fn ignoring_the_system_flag_is_caught() {
        let buggy = |mode: MotionMode, _sys: bool, focused: bool| -> MotionPolicy {
            // The old behavior: "OS reduced-motion query is a future refinement".
            let animate = focused && mode != MotionMode::Reduced;
            if animate {
                MotionPolicy::Full
            } else {
                MotionPolicy::Reduced
            }
        };
        assert_eq!(
            MotionPolicy::resolve(MotionMode::Auto, true, true),
            MotionPolicy::Reduced,
            "auto mode must honour the OS Reduce Motion flag"
        );
        assert_eq!(
            buggy(MotionMode::Auto, true, true),
            MotionPolicy::Full,
            "control: the pre-fix resolver kept animating"
        );
    }

    /// An unfocused window is Reduced under every mode/flag combination.
    #[test]
    fn unfocused_windows_demote_to_static() {
        for mode in [MotionMode::Auto, MotionMode::Full, MotionMode::Reduced] {
            for sys in [false, true] {
                assert_eq!(
                    MotionPolicy::resolve(mode, sys, false),
                    MotionPolicy::Reduced,
                    "unfocused must demote under ({mode:?}, sys={sys})"
                );
            }
        }
    }

    /// The effect set is complete and duplicate-free: `ALL[i].seq() == i` for
    /// every entry, so a new variant joins the totality proof's domain.
    #[test]
    fn effect_set_is_complete() {
        for (i, e) in MotionEffect::ALL.iter().enumerate() {
            assert_eq!(e.seq(), i, "{e:?} out of place in MotionEffect::ALL");
        }
    }

    /// Config parsing: the three documented spellings map to their modes
    /// (case-insensitively); anything else — including empty — is `Auto`.
    #[test]
    fn mode_parses_documented_spellings() {
        assert_eq!(MotionMode::parse("auto"), MotionMode::Auto);
        assert_eq!(MotionMode::parse("Full"), MotionMode::Full);
        assert_eq!(MotionMode::parse(" REDUCED "), MotionMode::Reduced);
        assert_eq!(MotionMode::parse("reduce"), MotionMode::Reduced);
        assert_eq!(MotionMode::parse(""), MotionMode::Auto);
        assert_eq!(MotionMode::parse("bogus"), MotionMode::Auto);
    }

    /// Serious mode's truth table: every fun effect is allowed in normal mode
    /// and denied in serious mode; both rows are reachable.
    #[test]
    fn serious_mode_policy_is_total_and_non_vacuous() {
        let mut saw_allowed = false;
        let mut saw_denied = false;
        for serious in [false, true] {
            let policy = SeriousModePolicy::resolve(serious);
            for effect in SeriousEffect::ALL {
                let allowed = policy.allows(effect);
                assert_eq!(allowed, !serious, "serious={serious}, effect={effect:?}");
                saw_allowed |= allowed;
                saw_denied |= !allowed;
            }
        }
        assert!(saw_allowed);
        assert!(saw_denied);
    }

    /// Negative control: a partial policy that forgets the level-up gate is
    /// observably different at the exact point the exhaustive table checks.
    #[test]
    fn serious_mode_policy_catches_an_ungated_effect() {
        let buggy = |effect| effect == SeriousEffect::LevelUp;
        assert!(!SeriousModePolicy::resolve(true).allows(SeriousEffect::LevelUp));
        assert!(buggy(SeriousEffect::LevelUp));
    }

    #[test]
    fn serious_effect_set_is_complete_and_ordered() {
        for (index, effect) in SeriousEffect::ALL.iter().enumerate() {
            assert_eq!(effect.seq(), index, "{effect:?} out of place");
        }
    }

    /// Tier-1 conformance: drive the derived serious-mode state machine and
    /// project its requested/effective bits through `SeriousModePolicy`,
    /// including a preference change under the overlay; disabling restores the
    /// latest request, not a stale startup snapshot.
    #[test]
    fn serious_mode_policy_conforms_to_derived_transition_model() {
        let model = aterm_spec::derive::serious_mode_model();
        let mut state = model.init_state();

        let assert_projection = |state: &aterm_spec::interp::State| {
            let serious = state["serious"] == 1;
            let policy = SeriousModePolicy::resolve(serious);
            let projected = [
                ("trail", "want_trail", SeriousEffect::CursorTrail),
                ("bell", "want_bell", SeriousEffect::TerminalSound),
                ("sparkle", "want_sparkle", SeriousEffect::WordDecorations),
                ("rain", "want_rain", SeriousEffect::MatrixRain),
                ("fade", "want_fade", SeriousEffect::StreamFade),
                ("celebration", "want_celebration", SeriousEffect::LevelUp),
                ("notice", "want_celebration", SeriousEffect::LevelUp),
                ("preview", "want_preview", SeriousEffect::SettingsPreview),
                ("gpu", "want_gpu", SeriousEffect::GpuPostFx),
            ];
            for (effective, requested, effect) in projected {
                let want = state[requested] == 1;
                assert_eq!(
                    state[effective] == 1,
                    want && policy.allows(effect),
                    "{effective}: {state:?}",
                );
            }
            // The four cursor surfaces share the modeled trail gate.
            for effect in [
                SeriousEffect::CursorGlow,
                SeriousEffect::CursorBody,
                SeriousEffect::CursorCat,
            ] {
                assert_eq!(
                    policy.allows(effect),
                    policy.allows(SeriousEffect::CursorTrail),
                    "cursor effect split from trail policy: {effect:?}",
                );
            }
        };
        let step = |state: &mut aterm_spec::interp::State, action| {
            assert!(model.fire(action, state), "{action}: {state:?}");
            for invariant in &model.invariants {
                assert!(
                    model.check_invariant(invariant.name, state),
                    "{} after {action}: {state:?}",
                    invariant.name,
                );
            }
        };

        assert_projection(&state);
        step(&mut state, "Enable");
        assert_projection(&state);
        step(&mut state, "ChangeTrail");
        assert_projection(&state);
        step(&mut state, "ChangeCelebration");
        assert_projection(&state);
        step(&mut state, "ChangePreview");
        assert_projection(&state);
        step(&mut state, "ChangeGpu");
        assert_projection(&state);
        step(&mut state, "Disable");
        assert_projection(&state);
        assert_eq!(state["want_trail"], 0);
        assert_eq!(state["trail"], 0, "latest underlying request restored");

        // Negative control: the modeled mutant that forgets the trail gate is
        // caught at the exact shipping projection point.
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let mut mutant = buggy.init_state();
        assert!(buggy.fire("Enable", &mut mutant));
        assert!(!buggy.check_invariant("SeriousSilencesEverything", &mutant));
        assert_ne!(
            mutant["trail"] == 1,
            mutant["want_trail"] == 1
                && SeriousModePolicy::resolve(true).allows(SeriousEffect::CursorTrail),
        );
    }
}
