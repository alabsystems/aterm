// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! MOTION POLICY (W11) — the SINGLE accessibility gate for every decorative
//! animation aterm paints. One pure, total resolution folds the three motion
//! facts — config `motion = auto|full|reduced`, the live OS "Reduce Motion"
//! accessibility flag (macOS: `NSWorkspace.accessibilityDisplayShouldReduceMotion`,
//! observed live via the apprt seam), and the window's focus — into one
//! [`MotionPolicy`] value, and every animated effect ([`MotionEffect`]) obtains
//! its animation amplitude from that value with ONE call
//! ([`MotionPolicy::amplitude`] / [`MotionPolicy::animate`]).
//!
//! Later motion features (smooth scroll, ink-fade, …) join by adding a
//! [`MotionEffect`] variant: the exhaustive match in `amplitude` then FAILS TO
//! COMPILE until the new effect's Reduced amplitude is decided, and the
//! reduced-motion totality proof below fails unless that amplitude is exactly 0.
//!
//! # Invariant (proven)
//!
//! `MotionPolicy::resolve` is a PURE TOTAL function of `(config, system_flag,
//! focus)`, and under a `Reduced` policy every governed animation amplitude is
//! EXACTLY 0. Two-tier proof:
//!
//! * Tier-0 (abstract): `aterm_spec::derive::motion_policy_model()` — the ty
//!   model checked by the real Trust `ty` in aterm-spec's `derived_ring_ty`
//!   (proves the invariant at `Buggy=0`, and REQUIRES a counterexample at
//!   `Buggy=1`, which reproduces the pre-W11 defect: the OS flag was never
//!   queried, so auto mode kept animating under system Reduce Motion).
//! * Tier-1 (this code): the `reduced_motion_totality` test below enumerates
//!   the COMPLETE 3×2×2 input domain × the full [`MotionEffect::ALL`] set —
//!   with finite inputs the exhaustive test is a complete proof over the
//!   shipping resolver itself.

/// The `motion` config value: how aterm decides whether decorative animations
/// run. `Auto` (the default and the value for any unknown string) consumes the
/// platform sample: live Reduce Motion on macOS, an attach-time animations
/// sample on Windows, and no OS-driven reduction where no query exists.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum MotionMode {
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
    pub(crate) fn parse(s: &str) -> Self {
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

/// Every decorative animation the policy governs — the ENUMERATED effect set
/// the reduced-motion totality proof quantifies over. Each variant names the
/// real seam that consumes its amplitude.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MotionEffect {
    /// The LUMEN cursor aurora (comet / bloom crown / landing ring / particles):
    /// the amplitude scales `GlowConfig::intensity` in `App::redraw_window` /
    /// `App::redraw_compose` (0 ⇒ the animator clears and emits nothing).
    CursorGlow,
    /// Sparkle-word twinkle/jitter animation (`WordDecorations::tick`): 0 ⇒ the
    /// static path (steady residual spark / cat-paw still draw — they are not
    /// motion).
    WordSparkles,
    /// The retained Settings test-scaffold cursor-effect demo lane
    /// (`next_demo_tick` arming in `about_to_wait`): 0 ⇒ the demo phase freezes.
    SettingsDemo,
    /// The M1 smooth-scroll wheel GLIDE (`App::scroll_wheel_animated`): 0 ⇒ the
    /// viewport snaps instantly (no eased motion, no glide deadlines armed).
    SmoothScroll,
    /// The M1 scroll-pill FADE ramp (`scroll_motion::pill_alpha`): 0 ⇒ the pill
    /// still shows (it is information, not decoration) but hides BINARY at the
    /// hold boundary instead of animating a fade.
    ScrollPill,
    /// The M2 "ink that dries" stream fade (`crate::stream_fade`): 0 ⇒ the
    /// bypass gate (`fade_permitted`) forces the instant path — every frame is
    /// byte-identical to the no-feature output and all ink dries immediately.
    StreamFade,
    /// The PHOSPHOR matrix rain (`MatrixRain::set_reduced_motion`, fed at the
    /// render tick): 0 ⇒ the engine emits NOTHING (empty channels, fp 0,
    /// inactive — timers disarm) — bypass-to-final-state (the drained-empty
    /// frame), not a freeze.
    MatrixRain,
    /// The transient update NOTICE card's slide (`notice::layout` scales
    /// `TransientNotice::rise` by this amplitude): 0 ⇒ the card holds its rest
    /// position for its whole life and only the alpha ramp remains. The pill
    /// still SHOWS under reduced motion — it is information, like the scroll
    /// pill — it simply stops travelling.
    NoticePill,
    /// ROBI the helper robot's show (`aterm_effects::robi`, gated in the
    /// redraw's Robi block): 0 ⇒ no show STARTS and a live one is stopped —
    /// bypass-to-final-state (no robot on glass), the matrix-rain rule. He is
    /// pure decoration; his tips re-appear on the next show once motion
    /// returns.
    Robi,
}

impl MotionEffect {
    /// The COMPLETE governed-effect set, in `seq` order. The totality proof
    /// iterates this array; [`Self::seq`]'s exhaustive match plus the
    /// `effect_set_is_complete` test pin the two together, so a new effect
    /// cannot silently skip the reduced-motion invariant. Test-only, like
    /// `seq`: production consumers gate per-effect via [`MotionPolicy`].
    #[cfg(test)]
    pub(crate) const ALL: [Self; 9] = [
        Self::CursorGlow,
        Self::WordSparkles,
        Self::SettingsDemo,
        Self::SmoothScroll,
        Self::ScrollPill,
        Self::StreamFade,
        Self::MatrixRain,
        Self::NoticePill,
        Self::Robi,
    ];

    /// Stable index of each variant (0..ALL.len()). EXHAUSTIVE match on purpose:
    /// adding a variant fails compilation here until it is given an index, and
    /// `effect_set_is_complete` fails until [`Self::ALL`] carries it at that
    /// index — so the proof's quantification domain grows in lockstep.
    #[cfg(test)]
    fn seq(self) -> usize {
        match self {
            Self::CursorGlow => 0,
            Self::WordSparkles => 1,
            Self::SettingsDemo => 2,
            Self::SmoothScroll => 3,
            Self::ScrollPill => 4,
            Self::StreamFade => 5,
            Self::MatrixRain => 6,
            Self::NoticePill => 7,
            Self::Robi => 8,
        }
    }
}

/// The resolved motion policy for one window, one frame: either animations run
/// at full amplitude, or every governed amplitude is exactly 0 (static effects
/// only). Obtained via [`crate::App::motion_policy`] — the ONE call consumers
/// (and future motion features) make.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MotionPolicy {
    /// Animations run at their configured amplitude.
    Full,
    /// Static effects only: every governed animation amplitude is exactly 0.
    Reduced,
}

/// Every nonessential effect suppressed by "serious mode".  This set is kept
/// separate from [`MotionEffect`]: serious mode removes decorative output, but
/// must not demote functional motion such as smooth scrolling or the scroll
/// position pill, and it must not affect cursor blink, visual bell, or window
/// attention.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SeriousEffect {
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
}

impl SeriousEffect {
    #[cfg(test)]
    const ALL: [Self; 12] = [
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
        }
    }
}

/// Pure, total projection from the single effective serious-mode bit to each
/// decorative output gate.  Keeping this law independent of mutable App state
/// makes the transition and render paths share one decision and gives the
/// finite truth table below a complete executable proof.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct SeriousModePolicy {
    serious: bool,
}

impl SeriousModePolicy {
    #[must_use]
    pub(crate) const fn resolve(serious: bool) -> Self {
        Self { serious }
    }

    #[must_use]
    pub(crate) const fn allows(self, effect: SeriousEffect) -> bool {
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
            | SeriousEffect::Robi => !self.serious,
        }
    }
}

impl MotionPolicy {
    /// Resolve the policy — a PURE, TOTAL function of the three motion facts:
    ///
    /// * `mode` — config `motion` (`Auto` follows `system_reduce`; `Full` /
    ///   `Reduced` override it),
    /// * `system_reduce` — the live OS "Reduce Motion" accessibility flag,
    /// * `focused` — this window's focus: an UNFOCUSED window always demotes to
    ///   static effects (W11b), regardless of config — background motion is
    ///   pure distraction and its animation timers are already focus-gated.
    #[must_use]
    pub(crate) fn resolve(mode: MotionMode, system_reduce: bool, focused: bool) -> Self {
        let animate = focused
            && match mode {
                MotionMode::Full => true,
                MotionMode::Reduced => false,
                MotionMode::Auto => !system_reduce,
            };
        if animate { Self::Full } else { Self::Reduced }
    }

    /// The ANIMATION amplitude for `effect` under this policy: `1.0` under
    /// [`Self::Full`], EXACTLY `0.0` under [`Self::Reduced`] — the invariant the
    /// two-tier proof pins (see the module doc). The `Reduced` arm matches every
    /// effect EXPLICITLY (no `_`), so a new [`MotionEffect`] variant forces an
    /// explicit decision here and the totality test rejects any nonzero choice.
    #[must_use]
    pub(crate) fn amplitude(self, effect: MotionEffect) -> f32 {
        match self {
            Self::Full => 1.0,
            Self::Reduced => match effect {
                MotionEffect::CursorGlow
                | MotionEffect::WordSparkles
                | MotionEffect::SettingsDemo
                | MotionEffect::SmoothScroll
                | MotionEffect::ScrollPill
                | MotionEffect::StreamFade
                | MotionEffect::MatrixRain
                | MotionEffect::NoticePill
                | MotionEffect::Robi => 0.0,
            },
        }
    }

    /// Whether `effect` may animate at all (`amplitude > 0`) — the boolean
    /// convenience for consumers that gate rather than scale.
    #[must_use]
    pub(crate) fn animate(self, effect: MotionEffect) -> bool {
        self.amplitude(effect) > 0.0
    }
}

#[cfg(test)]
mod tests {
    //! Two-tier proof, Tier-1 (real code): these tests enumerate the SAME
    //! invariant the derived ty model `motion_policy_model()` carries
    //! (aterm-spec/src/derive.rs; Tier-0 checked by the real Trust `ty` in
    //! aterm-spec's derived_ring_ty), over the SHIPPING `MotionPolicy` itself.
    //! The whole input domain is finite (3 modes × 2 system flags × 2 focus
    //! states × 8 effects), so the exhaustive enumeration is a COMPLETE proof
    //! under plain `cargo test`.

    use super::{MotionEffect, MotionMode, MotionPolicy, SeriousEffect, SeriousModePolicy};

    /// REDUCED-MOTION TOTALITY (the W11 PROVE bullet): `resolve` is total over
    /// the full `(config, system_flag, focus)` domain, its truth table is
    /// exactly "animate ⟺ focused ∧ (full ∨ (auto ∧ ¬system))", and under a
    /// `Reduced` policy every governed effect's amplitude is EXACTLY 0 (with
    /// `animate()` false). Under `Full` every amplitude is exactly 1 — the
    /// NON-VACUITY control: both policy branches are reachable and the
    /// invariant is not satisfied by a constant-zero amplitude.
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

    /// NEGATIVE CONTROL (the pre-W11 defect, the ty model's `Buggy=1` twin):
    /// a resolver that ignores the OS flag under `Auto` disagrees with the
    /// proven one exactly on the (Auto, sys=true, focused=true) point — so the
    /// truth-table assertion above genuinely catches that regression.
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

    /// UNFOCUSED DEMOTION (W11b): an unfocused window is Reduced under EVERY
    /// mode/flag combination — background windows get static effects only.
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

    /// The effect set is COMPLETE and duplicate-free: `ALL[i].seq() == i` for
    /// every entry. `seq`'s match is exhaustive, so a new variant fails to
    /// compile until indexed — and this test fails until `ALL` lists it, which
    /// pulls it into the totality proof's quantification domain.
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

    /// Complete truth table for serious mode: every enumerated fun effect
    /// is allowed in normal mode and denied in serious mode.  Both rows are
    /// reachable, so the proof cannot pass via a constant result.
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

    /// Negative control: a partial policy that forgets the level-up flourish is
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
    /// project each of its requested/effective bits through the shipping
    /// `SeriousModePolicy`. This includes changing a preference underneath the
    /// overlay and proves disable restores the latest request, not a stale
    /// startup snapshot.
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
