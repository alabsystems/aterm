// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance of the derived `TrailSoundSeam` machine
//! (`aterm_models_misc::trail_sound_seam_model`) to the shipping code.
//!
//! Every step runs real code:
//!
//! * the host fold: `MotionPolicy::resolve` for the accessibility stage and
//!   [`super::fold_window_audibility`], which `cursor_frame` calls;
//! * the writer: `CursorGlow::tick`, then `CursorGlow::sound_seam_open`;
//! * reader 1, the key path: `keystroke_click_audible` then
//!   `CursorGlow::cue_keystroke_shifted`, chained as the key handler does;
//! * reader 2, the verb: [`super::verb_seam_inputs`] (the constructor
//!   `App::tone_status` uses), then [`super::sound_seam`]. The config both
//!   readers see comes from the real resolver (`resolve_trail_presentation`
//!   then `resolve_cursor_glow`). The verb's call site is covered by
//!   `app_input`'s `tone_status_tests`.
//!
//! `seam`, `key` and `tone` are observed (the engine's seam, the key chain's
//! answer, the verb's verdict). `master`, `amplitude` and `dark_cause` are the
//! input class the model action names, read off the config the engine was
//! handed, never off the engine under test.
//!
//! Each observed step is projected onto the model and must be a successor of
//! the action its host input class names, with every invariant holding. The
//! run must also visit every reachable model state, so the binding is two-way.
//! Every style is driven (`Classic` included), on a real grid and on the
//! three degenerate ones (no columns, no rows, no cell).
//!
//! Negative controls replay the defects this binding catches: a host fold that
//! reads `audible` off the folded (dark) intensity, a verb that reads
//! `trail_on` as `GlowConfig::enabled` alone, a classic tick that keeps the
//! previous seam, and a degenerate grid that closes the seam. Each must be
//! rejected at the named step while the shipping code passes the same case.
//!
//! Before a window's first frame the engine has resolved no seam: a key is
//! refused (the model's `Key` before any tick) and the verb, with every gate
//! open, says `closed:engine-silent`. `EngineSilentIsUnreachable` is a claim
//! about a window that has run a frame;
//! `the_verb_before_the_first_frame_is_read_not_invented` pins the pre-frame
//! reading.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::trail_sound::SoundKind;
use aterm_models_misc::trail_sound_seam_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::State;

use super::{
    HostSoundTerms, SeamReason, fold_window_audibility, sound_seam, trail_on, verb_seam_inputs,
};
use crate::app_config::{CursorGlowInputs, resolve_cursor_glow, resolve_trail_presentation};
use crate::motion::{MotionEffect, MotionMode, MotionPolicy};
use crate::trail_audio::HostState;

/// Every style, each with the spelling the resolver is handed.
const STYLES: [GlowStyle; 10] = [
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
];

/// The config spelling of a style.
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
    }
}

/// One window's host facts for one tick.
#[derive(Clone, Copy, Debug)]
struct HostInputs {
    /// `cursor_trail`.
    trail_knob: bool,
    /// Serious mode is on (it closes both `CursorGlow` and `TerminalSound`).
    serious: bool,
    /// The resolved brightness knob, before any policy fold.
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

    /// The model action this input class names, classified from the host's
    /// facts, never from what the engine did.
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
    /// [`fold_window_audibility`] and the engine's own seam: what ships.
    Shipping,
    /// Negative control: `audible` read off the folded intensity, so any dark
    /// frame closes the seam.
    PreFixDarkIsSilent,
    /// Negative control: a classic wake that returns above every writer of the
    /// seam, so a classic tick keeps the previous tick's seam.
    PreFixClassicKeepsSeam,
    /// Negative control: a degenerate grid closes the seam like the master
    /// switch.
    PreFixEmptyGridCloses,
}

/// How the harness builds the verb's `trail_on`.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Verb {
    /// [`trail_on`]: what ships.
    Shipping,
    /// Negative control: `GlowConfig::enabled` alone.
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

/// The resolved config for one input and style, through the shipping
/// resolver `App::glow_config_for` calls (`resolve_cursor_glow`, after
/// `resolve_trail_presentation`). `enabled` is formed as the host forms it:
/// the master knob and serious mode's `CursorGlow` permission.
fn resolved(i: HostInputs, style: GlowStyle) -> GlowConfig {
    let raw = spelling(style);
    let presentation = resolve_trail_presentation(raw);
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

/// The states a run of ONE model visited, and what that model already
/// answered about them.
///
/// WHY THE ANSWERS (test-run speed): every script starts from the
/// model's initial state, so the same few states are asked about again by
/// script after script, and asking was 17% of
/// [`the_shipping_seam_conforms_to_the_trail_sound_seam_model`]'s samples
/// (`successors` 518 and `check_invariant` 1,824 of 13,998): 7.6 CPU-s run
/// alone, 6.0 with them kept. Both are pure functions of the model, the
/// action and the state: `successors` keeps exactly what the model returned,
/// and a state enters `visited` only after every invariant held at it, so
/// neither answer is ever one asking again could change, and every real step
/// is still judged by it.
struct Seen<'m> {
    model: &'m Model,
    /// Every projected state the run produced; every invariant holds at each.
    visited: BTreeSet<Vec<(&'static str, i64)>>,
    /// The model's successors of each `(state, action)` asked so far.
    successors: BTreeMap<State, BTreeMap<&'static str, Vec<State>>>,
}

impl<'m> Seen<'m> {
    fn new(model: &'m Model) -> Self {
        Self {
            model,
            visited: BTreeSet::new(),
            successors: BTreeMap::new(),
        }
    }
}

/// The harness: one real engine, the model state it is claimed to refine,
/// and every projected state it visited.
struct Harness<'s, 'm> {
    seen: &'s mut Seen<'m>,
    glow: CursorGlow,
    state: State,
    now: Instant,
    col: u16,
    /// The last tick's inputs and style, which the readers consult.
    last: Option<(HostInputs, GlowStyle)>,
    grid: Grid,
    fold: Fold,
    verb: Verb,
}

impl Harness<'_, '_> {
    fn check(&mut self, action: &'static str, observed: State) -> Result<(), String> {
        let seen = &mut *self.seen;
        let model = seen.model;
        let admitted = if let Some(next) = seen
            .successors
            .get(&self.state)
            .and_then(|by_action| by_action.get(action))
        {
            next.contains(&observed)
        } else {
            let next = model.successors(action, &self.state);
            let admitted = next.contains(&observed);
            seen.successors
                .entry(self.state.clone())
                .or_default()
                .insert(action, next);
            admitted
        };
        if !admitted {
            return Err(format!(
                "`{action}` from {:?} cannot reach the observed {observed:?} (last input {:?})",
                self.state, self.last
            ));
        }
        let key: Vec<(&'static str, i64)> = observed.iter().map(|(k, v)| (*k, *v)).collect();
        if !seen.visited.contains(&key) {
            for inv in &model.invariants {
                if !model.check_invariant(inv.name, &observed) {
                    return Err(format!("{} violated at {observed:?}", inv.name));
                }
            }
            seen.visited.insert(key);
        }
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
        // The input class (see the module note) and the observed seam.
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

/// The verb's inputs for one input, style and engine: the shipping
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
    script: &[(HostInputs, GlowStyle, Grid)],
    tone_first: bool,
    fold: Fold,
    verb: Verb,
    seen: &mut Seen<'_>,
) -> Result<(), String> {
    let state = seen.model.init_state();
    let mut h = Harness {
        seen,
        glow: CursorGlow::default(),
        state,
        now: Instant::now(),
        col: 0,
        last: None,
        grid: Grid::Real,
        fold,
        verb,
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
fn conforms(script: &[(HostInputs, GlowStyle, Grid)], seen: &mut Seen<'_>) {
    for tone_first in [false, true] {
        drive(script, tone_first, Fold::Shipping, Verb::Shipping, seen)
            .unwrap_or_else(|e| panic!("{script:?} (tone first: {tone_first}): {e}"));
    }
}

/// Every host input on every style and grid, every ordered pair of inputs on
/// the default style, and class pairs across a style switch and a grid change
/// conform, and the run covers the model's whole reachable space.
#[test]
fn the_shipping_seam_conforms_to_the_trail_sound_seam_model() {
    let model = trail_sound_seam_model();
    let mut visited = BTreeSet::new();
    let domain = domain();
    let reps = representatives();
    assert_eq!(reps.len(), 5, "every model tick class has a host input");

    // Each script runs on a fresh engine, so the scripts are independent: they
    // are listed first and run side by side (22 s run in turn on a 6-core
    // host), each exactly once, and the states each thread visited are united
    // before the two-way check below.
    // A script, and whether it runs in both reader orders (`conforms`) or
    // tone-last alone.
    type Script = (Vec<(HostInputs, GlowStyle, Grid)>, bool);
    let mut scripts: Vec<Script> = Vec::new();
    // Every input on every style and every grid, from a fresh engine.
    for &style in &STYLES {
        for &grid in &GRIDS {
            for &i in &domain {
                scripts.push((vec![(i, style, grid)], true));
            }
        }
    }
    // Every ordered pair of inputs on the default style: a class change
    // between two ticks (shed latch flapping, focus leaving, the master
    // switch) is where a latched seam would show.
    for &a in &domain {
        for &b in &domain {
            scripts.push((
                vec![
                    (a, GlowStyle::RainbowKitty, Grid::Real),
                    (b, GlowStyle::RainbowKitty, Grid::Real),
                ],
                false,
            ));
        }
    }
    // Class pairs across a style switch (the crossfade and ramp-in path),
    // including into and out of the classic wake.
    for &sa in &STYLES {
        for &sb in &STYLES {
            for &a in &reps {
                for &b in &reps {
                    scripts.push((vec![(a, sa, Grid::Real), (b, sb, Grid::Real)], true));
                }
            }
        }
    }
    // Class pairs across a grid change, both ways, on every style.
    for &style in &STYLES {
        for &ga in &GRIDS {
            for &gb in &GRIDS {
                for &a in &reps {
                    for &b in &reps {
                        scripts.push((vec![(a, style, ga), (b, style, gb)], true));
                    }
                }
            }
        }
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(1, usize::from);
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                let (model, scripts, next) = (&model, &scripts, &next);
                scope.spawn(move || {
                    let mut seen = Seen::new(model);
                    while let Some((script, both)) =
                        scripts.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
                    {
                        if *both {
                            conforms(script, &mut seen);
                        } else {
                            drive(script, false, Fold::Shipping, Verb::Shipping, &mut seen)
                                .unwrap_or_else(|e| panic!("{script:?}: {e}"));
                        }
                    }
                    seen.visited
                })
            })
            .collect();
        for worker in workers {
            visited.extend(
                worker
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            );
        }
    });

    // Two-way: the code reached every state the model can reach.
    let space = reachable(&model);
    let missing: Vec<_> = space.difference(&visited).collect();
    assert!(
        missing.is_empty(),
        "model states the code never produced: {missing:?}"
    );
    assert_eq!(visited, space);

    // A dark frame from Reduce Motion and one from the load-shed envelope both
    // occur in the domain and both are heard.
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

/// Negative control 1: the dark-is-silent fold is rejected at a
/// `TickDarkMotion` step for both causes, while the shipping fold passes the
/// identical script.
#[test]
fn the_pre_fix_fold_is_rejected_by_the_conformance() {
    let model = trail_sound_seam_model();
    let mut scratch = Seen::new(&model);
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
        drive(&script, false, Fold::Shipping, Verb::Shipping, &mut scratch)
            .expect("the shipping fold conforms on this script");
        let err = drive(
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
    // …and over the whole domain it is rejected exactly on the motion class;
    // everywhere else the two folds agree.
    for i in domain() {
        let got = drive(
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

/// Negative control 2: the enabled-only `trail_on`, substituted into the
/// shipping constructor's output, reads a user-dimmed aurora as
/// `closed:engine-silent`; the conformance rejects it at the `Tone` step only.
/// (`app_input`'s `a_dimmed_aurora_reads_trail_off_never_engine_silent` covers
/// `tone_status` calling the constructor.)
#[test]
fn the_pre_fix_verb_is_rejected_by_the_conformance() {
    let model = trail_sound_seam_model();
    let mut scratch = Seen::new(&model);
    for i in domain() {
        let got = drive(
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

/// Negative control 3: the classic wake keeping the previous tick's seam, and a
/// degenerate grid closing it like the master switch. Each is rejected at the
/// tick it happens on, on exactly the classes it gets wrong, while the shipping
/// engine passes the identical scripts.
#[test]
fn the_pre_fix_classic_and_empty_grid_seams_are_rejected() {
    let model = trail_sound_seam_model();
    let mut scratch = Seen::new(&model);
    let run = |script: &[(HostInputs, GlowStyle, Grid)], fold: Fold, scratch: &mut Seen<'_>| {
        drive(script, false, fold, Verb::Shipping, scratch)
    };
    for i in domain() {
        let heard = matches!(i.action(), "TickLit" | "TickDarkMotion");
        // A fresh classic window: the defect never opens its seam.
        let fresh = [(i, GlowStyle::Classic, Grid::Real)];
        run(&fresh, Fold::Shipping, &mut scratch).expect("the shipping classic tick conforms");
        let got = run(&fresh, Fold::PreFixClassicKeepsSeam, &mut scratch);
        assert_eq!(got.is_err(), heard, "fresh classic {i:?}: {got:?}");
        // An empty grid: the defect reads a heard window's seam as closed.
        for grid in [Grid::NoColumns, Grid::NoRows, Grid::NoCell] {
            let empty = [(i, GlowStyle::RainbowKitty, grid)];
            run(&empty, Fold::Shipping, &mut scratch).expect("the shipping empty grid conforms");
            let got = run(&empty, Fold::PreFixEmptyGridCloses, &mut scratch);
            assert_eq!(got.is_err(), heard, "{grid:?} {i:?}: {got:?}");
        }
    }
    // A classic tick after another style left the seam open: with the defect,
    // an unfocused classic window keeps clicking.
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

/// Before the first frame the verb's answer is read, never substituted: the
/// engine has resolved no seam, so a key is refused (as the model's `Key`
/// before any tick) and the verb, with every gate open, says
/// `closed:engine-silent`. Pins the pre-frame reading so the model's
/// `EngineSilentIsUnreachable` is not overread.
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
