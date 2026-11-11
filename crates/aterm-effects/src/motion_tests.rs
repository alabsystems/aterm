// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! These tests enumerate the invariant over the shipping `MotionPolicy`.
use super::{MotionEffect, MotionMode, MotionPolicy, PageMotion, SeriousEffect, SeriousModePolicy};

/// Reduce Motion dims the light without closing the key seam: audibility
/// depends on focus and the brightness knob, never on the motion policy.
#[test]
fn reduce_motion_never_mutes_typing() {
    use crate::cursor_glow::{CursorGlow, Geom, GlowConfig};
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
        accent: 0x007A_A2F7,
        duration: Duration::from_millis(240),
        length: 18,
        intensity: 0.7,
        audible: true,
        radius: 0.6,
        ring: true,
        ..Default::default()
    };
    let t0 = Instant::now();
    let mut seen_dark_and_heard = false;

    for mode in [MotionMode::Auto, MotionMode::Full, MotionMode::Reduced] {
        for system_reduce in [false, true] {
            for focused in [false, true] {
                let p = MotionPolicy::resolve(mode, system_reduce, focused);
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
/// (`animate()` false).
#[test]
fn reduced_motion_totality() {
    let mut saw_full = false;
    let mut saw_reduced = false;
    for mode in [MotionMode::Auto, MotionMode::Full, MotionMode::Reduced] {
        for system_reduce in [false, true] {
            for focused in [false, true] {
                let p = MotionPolicy::resolve(mode, system_reduce, focused);
                let expect_full = focused
                    && (mode == MotionMode::Full || (mode == MotionMode::Auto && !system_reduce));
                assert_eq!(
                    p == MotionPolicy::Full,
                    expect_full,
                    "resolve({mode:?}, sys={system_reduce}, focused={focused}) = {p:?}"
                );
                saw_full |= p == MotionPolicy::Full;
                saw_reduced |= p == MotionPolicy::Reduced;
                for e in MotionEffect::ALL {
                    let a = p.amplitude(e);
                    match p {
                        MotionPolicy::Reduced => {
                            assert_eq!(a, 0.0, "under Reduced, {e:?} amplitude must be EXACTLY 0");
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
    assert!(saw_full, "the Full policy must be reachable");
    assert!(saw_reduced, "the Reduced policy must be reachable");
}

/// A native page's motion is total over every fact that moves it, and its truth table is
/// §5.5's: an unfocused page is still; Serious Mode, and the shed or Low Power unless the
/// person asked for full motion, keep only crossfades (a static working glyph); Reduce Motion
/// keeps the spinner stepping in place and the crossfades but nothing that travels; anything
/// else moves in full. Every resolution is reached, and a non-native effect animates only
/// where the page is in full motion.
#[test]
fn page_motion_is_total_and_follows_the_reduced_motion_rows() {
    let mut seen = std::collections::HashSet::new();
    let bools = [false, true];
    for mode in [MotionMode::Auto, MotionMode::Full, MotionMode::Reduced] {
        for system_reduce in bools {
            for focused in bools {
                for serious in bools {
                    for shed in bools {
                        for low_power in bools {
                            let page = MotionPolicy::resolve_page(
                                mode,
                                system_reduce,
                                focused,
                                serious,
                                shed,
                                low_power,
                            );
                            let quiet =
                                serious || ((shed || low_power) && mode != MotionMode::Full);
                            let reduced = mode == MotionMode::Reduced
                                || (mode == MotionMode::Auto && system_reduce);
                            let want = if !focused {
                                PageMotion::Still
                            } else if quiet {
                                PageMotion::Fades
                            } else if reduced {
                                PageMotion::InPlace
                            } else {
                                PageMotion::Full
                            };
                            assert_eq!(
                                page, want,
                                "{mode:?} sys={system_reduce} focused={focused} \
                                 serious={serious} shed={shed} low_power={low_power}"
                            );
                            seen.insert(page);
                        }
                    }
                }
            }
        }
    }
    assert_eq!(
        seen.len(),
        PageMotion::ALL.len(),
        "every resolution is reached"
    );
    for page in PageMotion::ALL {
        let spins = page.animate(MotionEffect::NativeInPlace);
        let travels = page.animate(MotionEffect::NativeTravel);
        let fades = page.animate(MotionEffect::NativeFade);
        match page {
            PageMotion::Full => assert!(spins && travels && fades),
            PageMotion::InPlace => assert!(spins && !travels && fades, "§5.5 Reduce Motion"),
            PageMotion::Fades => assert!(!spins && !travels && fades, "a static glyph"),
            PageMotion::Still => assert!(!spins && !travels && !fades),
        }
        for effect in MotionEffect::ALL {
            if !matches!(
                effect,
                MotionEffect::NativeInPlace | MotionEffect::NativeTravel | MotionEffect::NativeFade
            ) {
                assert_eq!(page.animate(effect), page == PageMotion::Full, "{effect:?}");
            }
        }
    }
}

/// The load-shed exemption is exactly the functional pair (wheel glide and
/// scroll pill): a decorative effect in it would paint under overload.
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
/// (case-insensitively), `none` and `off` ask for no motion (`Reduced`);
/// anything else — including empty — is `Auto`.
#[test]
fn mode_parses_documented_spellings() {
    const REDUCED: MotionMode = MotionMode::parse("\u{00a0}REDUCE\u{2003}");
    const FULL: MotionMode = MotionMode::parse("\u{2003}FULL\u{00a0}");
    assert_eq!(REDUCED, MotionMode::Reduced);
    assert_eq!(FULL, MotionMode::Full);
    assert_eq!(MotionMode::parse("auto"), MotionMode::Auto);
    assert_eq!(MotionMode::parse("Full"), MotionMode::Full);
    assert_eq!(MotionMode::parse(" REDUCED "), MotionMode::Reduced);
    assert_eq!(MotionMode::parse("reduce"), MotionMode::Reduced);
    assert_eq!(MotionMode::parse("none"), MotionMode::Reduced);
    assert_eq!(MotionMode::parse(" Off "), MotionMode::Reduced);
    for (raw, want) in [
        ("\u{00a0}Full\u{2003}", MotionMode::Full),
        ("\u{2003}REDUCED\u{00a0}", MotionMode::Reduced),
        ("\u{00a0}reduce\u{2003}", MotionMode::Reduced),
    ] {
        assert_eq!(MotionMode::parse(raw), want);
    }
    for unknown in [
        "",
        "bogus",
        "\u{200b}full",
        "reduced\u{feff}",
        "fu\u{2003}ll",
        "re\u{00a0}duced",
    ] {
        assert_eq!(MotionMode::parse(unknown), MotionMode::Auto);
    }
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
