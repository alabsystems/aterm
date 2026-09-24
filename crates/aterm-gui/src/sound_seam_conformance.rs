// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TIER-1 CONFORMANCE of the derived `TrailSoundSeam` machine
//! (`aterm_spec::derive::trail_sound_seam_model`) to the shipping code.
//!
//! The model is hand-written Rust describing the seam; this file is what
//! makes a proof about it a proof about the program. Every step below runs
//! REAL code and nothing is re-derived:
//!
//! * the host fold — `MotionPolicy::resolve` for the accessibility stage and
//!   [`super::fold_window_audibility`], the function `tick_cursor_fx` calls;
//! * the writer — `CursorGlow::tick`, then `CursorGlow::sound_seam_open`;
//! * reader 1, the key path — `keystroke_click_audible` then
//!   `CursorGlow::cue_keystroke_shifted`, exactly as the key handler chains
//!   them;
//! * reader 2, the verb — [`super::verb_seam_inputs`], the ONE constructor
//!   `App::tone_status` builds its inputs with, then [`super::sound_seam`].
//!   The config both readers see comes from the real resolver
//!   (`resolve_trail_presentation` then `resolve_cursor_glow`), not a
//!   literal. The verb's CALL SITE — that `tone_status` hands the
//!   constructor the resolved config and the window's engine — is bound by
//!   `app_input`'s `tone_status_tests`, which drive the real verb after a
//!   real frame.
//!
//! What is projected from where, stated so the binding is not overclaimed:
//! `seam`, `key` and `tone` are OBSERVED (the engine's seam, the key
//! chain's answer, the verb's verdict). `master`, `amplitude` and
//! `dark_cause` are the INPUT CLASS the model action names — read off the
//! resolved and folded config the engine was handed, never off the engine,
//! because the engine's answer is what is under test.
//!
//! Each observed step is projected onto the model's variables and must be a
//! successor of the model action its HOST INPUT class names, and every
//! invariant must hold on it. The run must also VISIT every state the model
//! can reach, so the binding is two-way: nothing the code does is outside
//! the model, and nothing the model allows is unexercised.
//!
//! Every style is driven — `Classic` (whose tick used to return above
//! every writer of the seam) and `Custom` (with the default pack) included
//! — on a real grid and on the three degenerate ones (no columns, no rows,
//! no cell), which used to close the seam like the master switch.
//!
//! NEGATIVE CONTROLS, so a pass is never vacuous: the same harness is run
//! with the PRE-FIX host fold (`audible` read off the folded, dark
//! intensity — exactly what made the engine's zero-amplitude return close
//! the seam under load shed and Reduce Motion, D1/D2) and with the PRE-FIX
//! verb (`trail_on` read as `GlowConfig::enabled` alone, substituted into
//! the shipping constructor's output), and each must be REJECTED at the
//! named step while the shipping code passes the same case.
//!
//! BEFORE A WINDOW'S FIRST FRAME the engine has resolved no seam: a key is
//! refused (the model's `Key`-before-any-tick witness) and the verb, with
//! every named gate open, says `closed:engine-silent`. The model's
//! `EngineSilentIsUnreachable` is a claim about a window that has run a
//! frame, and `the_verb_before_the_first_frame_is_read_not_invented` pins
//! the pre-frame reading instead of substituting one.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle, TrailParams};
use aterm_effects::trail_sound::SoundKind;
use aterm_spec::derive::{Model, trail_sound_seam_model};
use aterm_spec::interp::State;

use super::{
    HostSoundTerms, SeamReason, fold_window_audibility, sound_seam, trail_on, verb_seam_inputs,
};
use crate::app_config::{
    CursorGlowInputs, ResolvedTrailStyle, TrailPackCatalog, resolve_cursor_glow,
    resolve_trail_presentation, resolve_trail_presentation_from_style,
};
use crate::motion::{MotionEffect, MotionMode, MotionPolicy};
use crate::trail_audio::HostState;

/// EVERY style, each with the spelling the resolver is handed.
const STYLES: [GlowStyle; 11] = [
    GlowStyle::Lumen,
    GlowStyle::Phaser,
    GlowStyle::RainbowKitty,
    GlowStyle::Sparkle,
    GlowStyle::Fire,
    GlowStyle::Laser,
    GlowStyle::Beam,
    GlowStyle::Water,
    GlowStyle::Comet,
    GlowStyle::Classic,
    GlowStyle::Custom,
];

/// The config spelling of a style (`Custom` is a `pack:` spelling).
fn spelling(style: GlowStyle) -> &'static str {
    match style {
        GlowStyle::Lumen => "lumen",
        GlowStyle::Phaser => "phaser",
        GlowStyle::RainbowKitty => "rainbow kitty",
        GlowStyle::Sparkle => "sparkle",
        GlowStyle::Fire => "fire",
        GlowStyle::Laser => "laser",
        GlowStyle::Beam => "beam",
        GlowStyle::Water => "water",
        GlowStyle::Comet => "comet",
        GlowStyle::Classic => "classic",
        GlowStyle::Custom => "pack:conformance",
    }
}

/// One window's host facts for one tick.
#[derive(Clone, Copy, Debug)]
struct HostInputs {
    /// `cursor_trail`.
    trail_knob: bool,
    /// Serious mode is on (it closes both `CursorGlow` and `TerminalSound`).
    serious: bool,
    /// The RESOLVED brightness knob, before any policy fold.
    user_intensity: f32,
    mode: MotionMode,
    system_reduce: bool,
    /// The folded `win_focused`.
    focused: bool,
    /// The soft load-shed envelope.
    shed_env: f32,
}

impl HostInputs {
    fn motion_amplitude(self) -> f32 {
        MotionPolicy::resolve(self.mode, self.system_reduce, self.focused)
            .amplitude(MotionEffect::CursorGlow)
    }

    /// The model action this input CLASS names. Classified from the host's
    /// facts, never from what the engine did — the engine's answer is what
    /// is under test.
    fn action(self) -> &'static str {
        if !self.trail_knob || self.serious {
            "TickMasterOff"
        } else if self.user_intensity <= 0.0 {
            "TickDarkUserOff"
        } else if !self.focused {
            "TickDarkUnfocused"
        } else if self.motion_amplitude() * self.shed_env <= 0.0 {
            "TickDarkMotion"
        } else {
            "TickLit"
        }
    }

    fn dark_cause(self) -> i64 {
        match self.action() {
            "TickDarkMotion" => 1,
            "TickDarkUnfocused" => 2,
            "TickDarkUserOff" => 3,
            _ => 0,
        }
    }
}

/// The whole finite host domain: 2 × 2 × 2 × 3 × 2 × 2 × 3 = 288 inputs.
fn domain() -> Vec<HostInputs> {
    let mut out = Vec::new();
    for trail_knob in [false, true] {
        for serious in [false, true] {
            for user_intensity in [0.0_f32, 0.7] {
                for mode in [MotionMode::Auto, MotionMode::Full, MotionMode::Reduced] {
                    for system_reduce in [false, true] {
                        for focused in [false, true] {
                            for shed_env in [0.0_f32, 0.35, 1.0] {
                                out.push(HostInputs {
                                    trail_knob,
                                    serious,
                                    user_intensity,
                                    mode,
                                    system_reduce,
                                    focused,
                                    shed_env,
                                });
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

/// Which code the harness runs: what ships, or a replay of a fixed defect.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Fold {
    /// [`fold_window_audibility`] and the engine's own seam — what ships.
    Shipping,
    /// NEGATIVE CONTROL: the pre-2026-09-22 host law, `audible` read off the
    /// FOLDED intensity, so any dark frame closes the seam.
    PreFixDarkIsSilent,
    /// NEGATIVE CONTROL: the pre-fix classic wake, which returned above
    /// every writer of the seam — a classic tick past the master switch
    /// left whatever seam the tick before it left.
    PreFixClassicKeepsSeam,
    /// NEGATIVE CONTROL: the pre-fix degenerate grid, which closed the seam
    /// like the master switch.
    PreFixEmptyGridCloses,
}

/// How the harness builds the verb's `trail_on`.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Verb {
    /// [`trail_on`] — what ships.
    Shipping,
    /// NEGATIVE CONTROL: the pre-fix `GlowConfig::enabled` alone.
    PreFixEnabledOnly,
}

/// The grid the engine is handed: a real one, and the three degenerate
/// shapes the engine's master-off return also takes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Grid {
    Real,
    NoColumns,
    NoRows,
    NoCell,
}

const GRIDS: [Grid; 4] = [Grid::Real, Grid::NoColumns, Grid::NoRows, Grid::NoCell];

fn geom(grid: Grid) -> Geom {
    let real = Geom {
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
    match grid {
        Grid::Real => real,
        Grid::NoColumns => Geom { cols: 0, ..real },
        Grid::NoRows => Geom { rows: 0, ..real },
        Grid::NoCell => Geom {
            cw: 0,
            ch: 0,
            ..real
        },
    }
}

/// The RESOLVED config for one input and style, through the SHIPPING
/// resolver `App::glow_config_for` calls (`resolve_cursor_glow`, after
/// `resolve_trail_presentation`). `enabled` is formed as the host forms it:
/// the master knob AND serious mode's `CursorGlow` permission.
fn resolved(i: HostInputs, style: GlowStyle) -> GlowConfig {
    let raw = spelling(style);
    let presentation = if style == GlowStyle::Custom {
        resolve_trail_presentation_from_style(
            raw,
            ResolvedTrailStyle {
                canonical: None,
                style: Some(GlowStyle::Custom),
                pack: Some(TrailParams::defaults()),
                issue: None,
            },
        )
    } else {
        resolve_trail_presentation(raw, &TrailPackCatalog::default())
    };
    assert_eq!(
        presentation.style.style,
        Some(style),
        "`{raw}` must resolve to {style:?}"
    );
    resolve_cursor_glow(
        CursorGlowInputs {
            enabled: i.trail_knob && !i.serious,
            color: None,
            accent: None,
            duration_ms: 240,
            length: 18,
            intensity: i.user_intensity,
            radius: 0.6,
            ring: true,
        },
        presentation,
        0x00C8_D3F5,
        true,
        0x00C8_D3F5,
        0x001A_1B26,
        0.5,
    )
}

fn folded(i: HostInputs, style: GlowStyle, fold: Fold) -> GlowConfig {
    let mut cfg = resolved(i, style);
    match fold {
        Fold::Shipping | Fold::PreFixClassicKeepsSeam | Fold::PreFixEmptyGridCloses => {
            fold_window_audibility(&mut cfg, i.motion_amplitude(), i.shed_env, i.focused);
        }
        Fold::PreFixDarkIsSilent => {
            cfg.intensity *= i.motion_amplitude() * i.shed_env;
            cfg.audible = i.focused && cfg.intensity > 0.0;
        }
    }
    cfg
}

/// The harness: one real engine, the model state it is claimed to refine,
/// and every projected state it visited.
struct Harness<'m> {
    model: &'m Model,
    glow: CursorGlow,
    state: State,
    now: Instant,
    col: u16,
    /// The last tick's inputs and style, which the readers consult.
    last: Option<(HostInputs, GlowStyle)>,
    grid: Grid,
    fold: Fold,
    verb: Verb,
    visited: &'m mut BTreeSet<Vec<(&'static str, i64)>>,
}

impl Harness<'_> {
    fn check(&mut self, action: &str, observed: State) -> Result<(), String> {
        if !self
            .model
            .successors(action, &self.state)
            .contains(&observed)
        {
            return Err(format!(
                "`{action}` from {:?} cannot reach the observed {observed:?} (last input {:?})",
                self.state, self.last
            ));
        }
        for inv in &self.model.invariants {
            if !self.model.check_invariant(inv.name, &observed) {
                return Err(format!("{} violated at {observed:?}", inv.name));
            }
        }
        self.visited
            .insert(observed.iter().map(|(k, v)| (*k, *v)).collect());
        self.state = observed;
        Ok(())
    }

    fn tick(&mut self, i: HostInputs, style: GlowStyle) -> Result<(), String> {
        self.now += Duration::from_millis(16);
        self.col = (self.col + 1) % 70;
        let cfg = folded(i, style, self.fold);
        let mut out = Vec::new();
        let prior = self.glow.sound_seam_open();
        self.glow.tick(
            Some((2, self.col)),
            self.now,
            &cfg,
            geom(self.grid),
            &mut out,
        );
        self.last = Some((i, style));
        let seam = match self.fold {
            Fold::PreFixClassicKeepsSeam
                if style == GlowStyle::Classic && cfg.enabled && self.grid == Grid::Real =>
            {
                prior
            }
            Fold::PreFixEmptyGridCloses if self.grid != Grid::Real => false,
            _ => self.glow.sound_seam_open(),
        };
        // The input class (see the module note) and the OBSERVED seam.
        let mut observed = self.state.clone();
        observed.insert("master", i64::from(cfg.enabled));
        observed.insert("amplitude", i64::from(cfg.enabled && cfg.intensity > 0.0));
        observed.insert("dark_cause", i.dark_cause());
        observed.insert("seam", i64::from(seam));
        observed.insert("key", 0);
        observed.insert("tone", 0);
        self.check(i.action(), observed)
    }

    /// Reader 1: the key handler's own chain, with the host's sound knobs
    /// open (the model's `Key` is the engine seam, not the knobs).
    fn key(&mut self) -> Result<(), String> {
        self.now += Duration::from_millis(1);
        let heard =
            crate::app_input::keystroke_click_audible(HostState::Running, true, 0.4, true, false)
                && self
                    .glow
                    .cue_keystroke_shifted(self.now, SoundKind::Typed, false);
        if heard {
            assert!(
                self.glow.take_key_cue().is_some(),
                "a heard key banks its cue"
            );
        }
        let mut observed = self.state.clone();
        observed.insert("key", if heard { 1 } else { 2 });
        self.check("Key", observed)
    }

    /// Reader 2: the verb, through the constructor `tone_status` uses and
    /// the real table. Only called after a tick: before a window's first
    /// frame there is no seam to read (see the module note).
    fn tone(&mut self) -> Result<(), String> {
        let (i, style) = self.last.expect("the verb is read after a tick");
        let verdict = sound_seam(&verb_inputs(i, style, &self.glow, self.verb));
        let mut observed = self.state.clone();
        observed.insert(
            "tone",
            match verdict {
                None => 1,
                Some(SeamReason::EngineSilent) => 3,
                Some(_) => 2,
            },
        );
        self.check("Tone", observed)
    }
}

/// The verb's inputs for one input, style and engine: the SHIPPING
/// constructor over the shipping resolver, with the host's own sound knobs
/// open (the model's `Tone` is about the engine and the aurora config).
fn verb_inputs(
    i: HostInputs,
    style: GlowStyle,
    engine: &CursorGlow,
    verb: Verb,
) -> super::SeamInputs {
    let glow = resolved(i, style);
    let mut inputs = verb_seam_inputs(
        &glow,
        engine,
        HostSoundTerms {
            sounds_on: true,
            volume: 0.4,
            serious_allows: !i.serious,
            focused: i.focused,
            resize_quiet: false,
            host: HostState::Running,
        },
    );
    if verb == Verb::PreFixEnabledOnly {
        inputs.trail_on = glow.enabled;
    }
    inputs
}

/// Drive one script — `(input, style, grid)` ticks, each followed by both
/// readers in the given order — through a fresh engine.
fn drive(
    model: &Model,
    script: &[(HostInputs, GlowStyle, Grid)],
    tone_first: bool,
    fold: Fold,
    verb: Verb,
    visited: &mut BTreeSet<Vec<(&'static str, i64)>>,
) -> Result<(), String> {
    let mut h = Harness {
        model,
        glow: CursorGlow::default(),
        state: model.init_state(),
        now: Instant::now(),
        col: 0,
        last: None,
        grid: Grid::Real,
        fold,
        verb,
        visited,
    };
    // A host that has never ticked records nothing.
    h.key()?;
    for &(i, style, grid) in script {
        h.grid = grid;
        h.tick(i, style)?;
        if tone_first {
            h.tone()?;
            h.key()?;
        } else {
            h.key()?;
            h.tone()?;
        }
    }
    Ok(())
}

/// One representative input per model tick class.
fn representatives() -> Vec<HostInputs> {
    let mut seen = BTreeSet::new();
    domain()
        .into_iter()
        .filter(|i| seen.insert(i.action()))
        .collect()
}

fn reachable(model: &Model) -> BTreeSet<Vec<(&'static str, i64)>> {
    let key = |s: &State| s.iter().map(|(k, v)| (*k, *v)).collect::<Vec<_>>();
    let mut seen = BTreeSet::new();
    let mut queue = vec![model.init_state()];
    seen.insert(key(&queue[0]));
    while let Some(s) = queue.pop() {
        for a in &model.actions {
            for n in model.successors(a.name, &s) {
                if seen.insert(key(&n)) {
                    queue.push(n);
                }
            }
        }
    }
    seen
}

/// A shipping run of one script: both reader orders.
fn conforms(
    model: &Model,
    script: &[(HostInputs, GlowStyle, Grid)],
    visited: &mut BTreeSet<Vec<(&'static str, i64)>>,
) {
    for tone_first in [false, true] {
        drive(
            model,
            script,
            tone_first,
            Fold::Shipping,
            Verb::Shipping,
            visited,
        )
        .unwrap_or_else(|e| panic!("{script:?} (tone first: {tone_first}): {e}"));
    }
}

/// THE CONFORMANCE: every host input on every style and every grid, every
/// ordered pair of host inputs on the default style, class pairs across a
/// style switch and across a grid change — and the run covers the model's
/// whole reachable space.
#[test]
fn the_shipping_seam_conforms_to_the_trail_sound_seam_model() {
    let model = trail_sound_seam_model();
    let mut visited = BTreeSet::new();
    let domain = domain();
    let reps = representatives();
    assert_eq!(reps.len(), 5, "every model tick class has a host input");

    // Every input on every style and every grid, from a fresh engine.
    for &style in &STYLES {
        for &grid in &GRIDS {
            for &i in &domain {
                conforms(&model, &[(i, style, grid)], &mut visited);
            }
        }
    }
    // Every ordered pair of inputs on the default style: a class change
    // between two ticks (shed latch flapping, focus leaving, the master
    // switch) is where a latched seam would show.
    for &a in &domain {
        for &b in &domain {
            drive(
                &model,
                &[
                    (a, GlowStyle::RainbowKitty, Grid::Real),
                    (b, GlowStyle::RainbowKitty, Grid::Real),
                ],
                false,
                Fold::Shipping,
                Verb::Shipping,
                &mut visited,
            )
            .unwrap_or_else(|e| panic!("{a:?} -> {b:?}: {e}"));
        }
    }
    // Class pairs across a STYLE switch (the crossfade and ramp-in path) —
    // including INTO and OUT OF the classic wake, whose seam used to be
    // whatever the style before it left.
    for &sa in &STYLES {
        for &sb in &STYLES {
            for &a in &reps {
                for &b in &reps {
                    conforms(
                        &model,
                        &[(a, sa, Grid::Real), (b, sb, Grid::Real)],
                        &mut visited,
                    );
                }
            }
        }
    }
    // Class pairs across a GRID change, both ways, on every style.
    for &style in &STYLES {
        for &ga in &GRIDS {
            for &gb in &GRIDS {
                for &a in &reps {
                    for &b in &reps {
                        conforms(&model, &[(a, style, ga), (b, style, gb)], &mut visited);
                    }
                }
            }
        }
    }

    // TWO-WAY: the code reached every state the model can reach.
    let space = reachable(&model);
    let missing: Vec<_> = space.difference(&visited).collect();
    assert!(
        missing.is_empty(),
        "model states the code never produced: {missing:?}"
    );
    assert_eq!(visited, space);

    // THE FIX, witnessed on both of its causes: a dark frame from Reduce
    // Motion and one from the load-shed envelope both occur in the domain
    // and both are heard (the conformance above already required it).
    let reduce = domain.iter().any(|i| {
        i.action() == "TickDarkMotion"
            && MotionPolicy::resolve(i.mode, i.system_reduce, i.focused) == MotionPolicy::Reduced
    });
    let shed = domain.iter().any(|i| {
        i.action() == "TickDarkMotion"
            && MotionPolicy::resolve(i.mode, i.system_reduce, i.focused) == MotionPolicy::Full
            && i.shed_env == 0.0
    });
    assert!(reduce && shed, "both dark-but-heard causes are exercised");
}

/// The lit, focused input every negative control starts from.
fn lit() -> HostInputs {
    HostInputs {
        trail_knob: true,
        serious: false,
        user_intensity: 0.7,
        mode: MotionMode::Auto,
        system_reduce: false,
        focused: true,
        shed_env: 1.0,
    }
}

/// NEGATIVE CONTROL 1 — D1/D2. The pre-fix fold is REJECTED, at a
/// `TickDarkMotion` step, for BOTH causes, while the shipping fold passes
/// the identical script.
#[test]
fn the_pre_fix_fold_is_rejected_by_the_conformance() {
    let model = trail_sound_seam_model();
    let mut scratch = BTreeSet::new();
    let lit = lit();
    let reduce_motion = HostInputs {
        system_reduce: true,
        ..lit
    };
    let shed = HostInputs {
        shed_env: 0.0,
        ..lit
    };
    for dark in [reduce_motion, shed] {
        assert_eq!(dark.action(), "TickDarkMotion");
        let script = [
            (lit, GlowStyle::RainbowKitty, Grid::Real),
            (dark, GlowStyle::RainbowKitty, Grid::Real),
        ];
        drive(
            &model,
            &script,
            false,
            Fold::Shipping,
            Verb::Shipping,
            &mut scratch,
        )
        .expect("the shipping fold conforms on this script");
        let err = drive(
            &model,
            &script,
            false,
            Fold::PreFixDarkIsSilent,
            Verb::Shipping,
            &mut scratch,
        )
        .expect_err("the pre-fix fold must be rejected");
        assert!(
            err.contains("TickDarkMotion"),
            "rejected at the dark tick: {err}"
        );
    }
    // …and over the whole domain it is rejected EXACTLY on the motion class:
    // everywhere else the old and new folds agree, so the control is sharp.
    for i in domain() {
        let got = drive(
            &model,
            &[(i, GlowStyle::RainbowKitty, Grid::Real)],
            false,
            Fold::PreFixDarkIsSilent,
            Verb::Shipping,
            &mut scratch,
        );
        assert_eq!(
            got.is_err(),
            i.action() == "TickDarkMotion",
            "{i:?}: {got:?}"
        );
    }
}

/// NEGATIVE CONTROL 2 — the verb's pre-fix `trail_on`, substituted into the
/// shipping constructor's output. A user-dimmed aurora read
/// `closed:engine-silent`; the conformance rejects it at the `Tone` step,
/// and only there. (That `tone_status` itself calls the constructor, with
/// the resolved config, is `app_input`'s
/// `a_dimmed_aurora_reads_trail_off_never_engine_silent`.)
#[test]
fn the_pre_fix_verb_is_rejected_by_the_conformance() {
    let model = trail_sound_seam_model();
    let mut scratch = BTreeSet::new();
    for i in domain() {
        let got = drive(
            &model,
            &[(i, GlowStyle::RainbowKitty, Grid::Real)],
            false,
            Fold::Shipping,
            Verb::PreFixEnabledOnly,
            &mut scratch,
        );
        assert_eq!(
            got.is_err(),
            i.action() == "TickDarkUserOff" && i.focused,
            "{i:?}: {got:?}"
        );
        if let Err(e) = got {
            assert!(e.contains("`Tone`"), "{e}");
        }
    }
}

/// NEGATIVE CONTROL 3 — the two engine defects this round fixed, replayed:
/// the classic wake keeping the previous tick's seam, and a degenerate grid
/// closing it like the master switch. Each is REJECTED at the tick it
/// happens on, on exactly the classes it got wrong, while the shipping
/// engine passes the identical scripts.
#[test]
fn the_pre_fix_classic_and_empty_grid_seams_are_rejected() {
    let model = trail_sound_seam_model();
    let mut scratch = BTreeSet::new();
    let run = |script: &[(HostInputs, GlowStyle, Grid)], fold: Fold, scratch: &mut BTreeSet<_>| {
        drive(&model, script, false, fold, Verb::Shipping, scratch)
    };
    for i in domain() {
        let heard = matches!(i.action(), "TickLit" | "TickDarkMotion");
        // A FRESH classic window: pre-fix, nothing ever opened its seam.
        let fresh = [(i, GlowStyle::Classic, Grid::Real)];
        run(&fresh, Fold::Shipping, &mut scratch).expect("the shipping classic tick conforms");
        let got = run(&fresh, Fold::PreFixClassicKeepsSeam, &mut scratch);
        assert_eq!(got.is_err(), heard, "fresh classic {i:?}: {got:?}");
        // AN EMPTY GRID: pre-fix, a heard window read a closed seam.
        for grid in [Grid::NoColumns, Grid::NoRows, Grid::NoCell] {
            let empty = [(i, GlowStyle::RainbowKitty, grid)];
            run(&empty, Fold::Shipping, &mut scratch).expect("the shipping empty grid conforms");
            let got = run(&empty, Fold::PreFixEmptyGridCloses, &mut scratch);
            assert_eq!(got.is_err(), heard, "{grid:?} {i:?}: {got:?}");
        }
    }
    // A classic tick after another style left the seam OPEN: pre-fix, an
    // unfocused classic window kept clicking.
    let unfocused = HostInputs {
        focused: false,
        ..lit()
    };
    let switched = [
        (lit(), GlowStyle::RainbowKitty, Grid::Real),
        (unfocused, GlowStyle::Classic, Grid::Real),
    ];
    run(&switched, Fold::Shipping, &mut scratch).expect("the shipping switch conforms");
    let err = run(&switched, Fold::PreFixClassicKeepsSeam, &mut scratch)
        .expect_err("the stale open seam must be rejected");
    assert!(err.contains("`TickDarkUnfocused`"), "{err}");
}

/// BEFORE THE FIRST FRAME, read — never substituted. The engine has
/// resolved no seam, so a key is refused (the model's `Key` before any tick
/// answers the same) and the verb, with every named gate open, says
/// `closed:engine-silent`: no CONFIG gate explains the silence, the window
/// has simply not drawn. The model's `EngineSilentIsUnreachable` is a claim
/// about a window that has run a frame, and this pins what the verb says
/// before one so the claim is not overread.
#[test]
fn the_verb_before_the_first_frame_is_read_not_invented() {
    let fresh = CursorGlow::default();
    for i in domain() {
        let got = sound_seam(&verb_inputs(
            i,
            GlowStyle::RainbowKitty,
            &fresh,
            Verb::Shipping,
        ));
        let named = !trail_on(&resolved(i, GlowStyle::RainbowKitty)) || i.serious || !i.focused;
        if named {
            assert_ne!(got, Some(SeamReason::EngineSilent), "{i:?}");
            assert!(got.is_some(), "{i:?}");
        } else {
            assert_eq!(got, Some(SeamReason::EngineSilent), "{i:?}");
        }
    }
}
