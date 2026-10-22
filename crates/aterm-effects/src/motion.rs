// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Motion policy: the single accessibility gate for decorative animation.
//! [`MotionPolicy::resolve`] folds config `motion = auto|full|reduced`, the live
//! OS Reduce Motion flag (macOS `NSWorkspace.accessibilityDisplayShouldReduceMotion`,
//! via the apprt seam) and window focus into one policy, and every
//! [`MotionEffect`] takes its amplitude from it ([`MotionPolicy::amplitude`] /
//! [`MotionPolicy::animate`]).
//!
//! Under `Reduced` every governed amplitude is exactly 0. A new [`MotionEffect`]
//! variant does not compile until `amplitude` decides its reduced value, and
//! `reduced_motion_totality` fails unless that value is 0.
//!
//! The host's `App::motion_policy` adds a fourth input, the load-shed latch
//! (`perf_reduced`), which forces `Reduced` while presents run over budget.
//! Functional motion that answers the user's hand (the wheel glide and scroll
//! pill, [`MotionEffect::load_shed_exempt`]) resolves through
//! `App::effect_policy` from the accessibility facts alone, so it never sheds;
//! under a `Reduced` policy its amplitude is still 0.

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
    /// Whether the load-adaptive shed may reduce motion in this mode: an
    /// explicit Always animate (`Full`) is the person's own intent and
    /// overrides it, so under `Full` the shed (and the switch that enables
    /// it) does nothing. The host's latch and envelope and Settings' note on
    /// that switch read this one rule.
    #[must_use]
    pub const fn admits_load_shed(self) -> bool {
        !matches!(self, Self::Full)
    }

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
    /// The smooth-scroll wheel glide (`App::scroll_wheel_animated`): 0 ⇒ the
    /// viewport snaps instantly (no eased motion, no glide deadlines armed).
    SmoothScroll,
    /// The scroll-pill fade ramp (`scroll_motion::pill_alpha`): at 0 the pill
    /// still shows (it is information) but hides at once at the hold boundary.
    ScrollPill,
    /// The matrix rain (`MatrixRain::set_reduced_motion`, fed at the render
    /// tick): 0 means the engine emits nothing (empty channels, fp 0, timers
    /// disarmed), the final state rather than a freeze.
    MatrixRain,
    /// Prism wake's output streak (`OutputStreak::tick` reads it as
    /// `StreakConfig::intensity`): at 0 the engine resets and emits nothing (no
    /// quads, no cue, fp 0). A pin rather than an ease: an effect answering
    /// program output is one a motion-sensitive user needs gone.
    OutputStreak,
    /// The upgrade rim (the host's `level_up`): pulse, climb, hue travel and
    /// thickening scale with it. At 0 the rim is still at fixed alpha and
    /// thickness: the information stays, the movement goes.
    UpgradeSurge,
    /// The message band's live indicator: a busy row's braille spinner and
    /// whole-row comet (`aterm_messages::Meter::busy`), the determinate bar's
    /// glide and glint, the completion echo's glow and fade. At 0 it is
    /// `Pace::Still`: a bar moving only with its data, a busy row's glyph over
    /// its unlit track, a still ✓ on completion.
    BandProgress,
    /// The harness card's motion (`HarnessCard::tick`'s `animate`): the border's
    /// drift and specular head, the trail's step wave and shimmer, the kitty's
    /// blink and tail flick, the stars' twinkle and burst. At 0 the card draws
    /// its settled frame from the first tick: border, trail, kitty and steady
    /// stars stay (they show the session is wrapped); nothing moves.
    HarnessCard,
}

impl MotionEffect {
    /// The complete governed-effect set, which the totality proof iterates.
    /// Test-only; production gates via [`MotionPolicy`].
    #[cfg(test)]
    pub const ALL: [Self; 9] = [
        Self::CursorGlow,
        Self::WordSparkles,
        Self::SmoothScroll,
        Self::ScrollPill,
        Self::MatrixRain,
        Self::OutputStreak,
        Self::UpgradeSurge,
        Self::BandProgress,
        Self::HarnessCard,
    ];

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
            | Self::MatrixRain
            | Self::OutputStreak
            | Self::UpgradeSurge
            | Self::BandProgress
            | Self::HarnessCard => false,
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
    LevelUp,
    SettingsPreview,
    GpuPostFx,
    /// The message band's live indicator motion, the busy spinner and comet
    /// included. The band and its words stay; the indicator takes its still
    /// form.
    BandProgress,
    /// The harness card's kitty, rainbow trail, stars and border motion:
    /// serious mode keeps a static border, so the session still shows as
    /// wrapped.
    HarnessCard,
    /// The act ticker's label on the Autopilot chip (`✓ approved` for 3 s,
    /// design `docs/DESIGN-harness-autopilot-2026-10-15.md` §9.2.1): serious
    /// mode withholds it. The chip, its title's trail of the last acts and
    /// Settings ▸ Harness's counts stay.
    HarnessTicker,
    /// Prism wake's comet over new program output: decoration, so serious mode
    /// takes it with the rest (its pip goes with [`Self::TerminalSound`]).
    OutputStreak,
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
        Self::LevelUp,
        Self::SettingsPreview,
        Self::GpuPostFx,
        Self::BandProgress,
        Self::HarnessCard,
        Self::HarnessTicker,
        Self::OutputStreak,
    ];
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
            | SeriousEffect::LevelUp
            | SeriousEffect::SettingsPreview
            | SeriousEffect::GpuPostFx
            | SeriousEffect::BandProgress
            | SeriousEffect::HarnessCard
            | SeriousEffect::HarnessTicker
            | SeriousEffect::OutputStreak => !self.serious,
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
    #[inline]
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
    #[inline]
    #[must_use]
    pub fn amplitude(self, effect: MotionEffect) -> f32 {
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
                | MotionEffect::BandProgress
                | MotionEffect::HarnessCard => 0.0,
            },
        }
    }

    /// Whether `effect` may animate at all (`amplitude > 0`) — the boolean
    /// convenience for consumers that gate rather than scale.
    #[inline]
    #[must_use]
    pub fn animate(self, effect: MotionEffect) -> bool {
        self.amplitude(effect) > 0.0
    }
}

#[cfg(test)]
mod tests {
    //! These tests enumerate the invariant over the shipping `MotionPolicy`.
    //! The domain is finite (3 modes × 2 system flags × 2 focus states × every
    //! effect), so the enumeration is a
    //! complete proof.

    use super::{MotionEffect, MotionMode, MotionPolicy, SeriousEffect, SeriousModePolicy};

    /// Reduce Motion dims the light without closing the key seam: audibility
    /// depends on focus and the brightness knob, never on the motion policy.
    /// Drives the real `CursorGlow` through the host's fold
    /// (`sound_policy::fold_window_audibility`) and reads `sound_seam_open()`.
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
        };
        let t0 = Instant::now();
        let mut seen_dark_and_heard = false;

        for mode in [MotionMode::Auto, MotionMode::Full, MotionMode::Reduced] {
            for system_reduce in [false, true] {
                for focused in [false, true] {
                    let p = MotionPolicy::resolve(mode, system_reduce, focused);
                    // The host's fold: the amplitude scales the visual scalar,
                    // with no load shed (envelope 1.0).
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
                    // Non-vacuity: some point has the light dark and the keys
                    // heard.
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
                    // The amplitude law, quantified over the enumerated effect
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

    /// Conformance: drive the derived serious-mode state machine and
    /// project its requested/effective bits through `SeriousModePolicy`,
    /// including a preference change under the overlay; disabling restores the
    /// latest request, not a stale startup snapshot.
    #[test]
    fn serious_mode_policy_conforms_to_derived_transition_model() {
        let model = aterm_models_render::serious_mode_model();
        let mut state = model.init_state();

        let assert_projection = |state: &aterm_spec::interp::State| {
            let serious = state["serious"] == 1;
            let policy = SeriousModePolicy::resolve(serious);
            let projected = [
                ("trail", "want_trail", SeriousEffect::CursorTrail),
                ("bell", "want_bell", SeriousEffect::TerminalSound),
                ("sparkle", "want_sparkle", SeriousEffect::WordDecorations),
                ("rain", "want_rain", SeriousEffect::MatrixRain),
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
