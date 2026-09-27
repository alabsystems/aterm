// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 bind for `NovaPhase` (`aterm_spec::derive::nova_phase_model`) on the
//! HOST: the real `WordDecorations` engine, over a real terminal holding a
//! profanity word in the classic nova style, ticked at 1 ms through three whole
//! arms and everything between them.
//!
//! The model's defect lives in `Rearm`, and a re-arm is something only the host
//! does — its ignition grant (`nova_start`, handed out by the flash limiter),
//! its spent mark (`nova_done`), `hard_reset`, the suspend time-shift in
//! `thaw`, and the grace/rebirth rules that decide when a word may NOT ignite
//! again. So every one of those runs here, and both model variables come from
//! the host:
//!
//! * `phase` is read off the episode the host keeps for the word: `Armed` until
//!   the host grants an ignition, then the `(now − nova_start, D)` window the
//!   emitter draws from (`D` from the word's own genome), and `Settled` for an
//!   episode the host has marked spent without a grant (born done). The host's
//!   own spent mark is checked against it on every frame: `nova_done` exactly
//!   when the window has run out.
//! * `flashes` counts the frames on which the host's nova stream goes from
//!   dark to lit, per phase window. The model counts a flash exactly when the
//!   walk enters `Flash`, so light that blooms in any other window, twice, or
//!   not at all is a step the model rejects.
//!
//! Every window boundary is validated as the model's `Step`, every
//! `hard_reset` + rescan as its `Rearm` (the new episode must be `Armed` with no
//! crown yet), and every host transition that is neither — a grace re-hit, an
//! identity death and rebirth — must not move the projection at all. Three arms
//! spend the model's whole `MaxSteps` budget on real steps; the second is
//! frozen mid-crown and thawed two seconds later, and must resume where it
//! stopped.
//!
//! NEGATIVE CONTROL: from the real settled state, a re-arm that re-enters
//! `Flash` and counts a second crown (the model's `Buggy = 1` `Rearm`) is
//! rejected by the committed model and admitted by `Buggy = 1`.

use std::collections::BTreeMap;

use aterm_spec::derive::{Model, nova_phase_model};
use aterm_spec::{interp, verify};

use super::*;
use crate::genome::nova_duration_ms;
use crate::nova::NovaPhase;

type State = BTreeMap<&'static str, i64>;

fn state(phase: i64, steps: i64, flashes: i64, arms: i64) -> State {
    [
        ("phase", phase),
        ("steps", steps),
        ("flashes", flashes),
        ("arms", arms),
    ]
    .into_iter()
    .collect()
}

fn phase_index(p: NovaPhase) -> i64 {
    match p {
        NovaPhase::Armed => 0,
        NovaPhase::Dip => 1,
        NovaPhase::Flash => 2,
        NovaPhase::Ring => 3,
        NovaPhase::Debris => 4,
        NovaPhase::Ember => 5,
        NovaPhase::Settled => 6,
    }
}

const SETTLED: i64 = 6;
const ROWS: usize = 2;
const COLS: usize = 20;

/// The real engine over a real terminal, driven like a pane.
struct Host {
    wd: WordDecorations,
    screen: Terminal,
    blank: Terminal,
    lex: Lexicon,
    cfg: DecoConfig,
    geom: EffectGeom,
    epoch: u64,
}

impl Host {
    fn new(now: Instant) -> Self {
        let mut screen = Terminal::new(ROWS as u16, COLS as u16);
        screen.process(b"oh fuck");
        let mut host = Self {
            wd: WordDecorations::default(),
            screen,
            blank: Terminal::new(ROWS as u16, COLS as u16),
            lex: Lexicon::with_languages(&["en"]),
            cfg: DecoConfig {
                profanity_style: ProfanityStyle::Nova,
                ..DecoConfig::default()
            },
            geom: EffectGeom {
                cell_w: 10,
                cell_h: 20,
                rows: 6,
                cols: COLS as u16,
            },
            epoch: 0,
        };
        host.rescan(true, now);
        host
    }

    fn rescan(&mut self, word_on_screen: bool, now: Instant) {
        self.epoch += 1;
        let term = if word_on_screen {
            &self.screen
        } else {
            &self.blank
        };
        self.wd
            .rescan(term, ROWS, COLS, &self.lex, &self.cfg, self.epoch, now);
    }

    /// One presented frame; whether the host's nova stream carried any light.
    fn tick(&mut self, now: Instant) -> bool {
        let (mut out, mut ink, mut free, mut nova) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        self.wd.tick(
            now, &self.cfg, self.geom, None, None, true, &mut out, &mut ink, &mut free, &mut nova,
        );
        !nova.is_empty()
    }

    /// The word's occurrence and the episode the host keeps for it.
    fn episode(&self) -> Option<(&Occurrence, &Episode)> {
        let occ = self.wd.occ.iter().find(|o| o.class == Class::Profanity)?;
        Some((occ, self.wd.persist.get(&occ.ident)?))
    }

    /// The phase the host is in at `now` (see the module docs), after checking
    /// the host's own spent mark against it.
    fn phase(&self, now: Instant) -> i64 {
        let (occ, ep) = self.episode().expect("the word has an episode");
        let Some(start) = ep.nova_start else {
            return if ep.nova_done { SETTLED } else { 0 };
        };
        let d = nova_duration_ms(occ.genome.gkey);
        let t = if now >= start {
            i64::try_from(now.saturating_duration_since(start).as_millis()).expect("small")
        } else {
            -1 // a delayed grant: not yet ignited
        };
        assert_eq!(
            ep.nova_done,
            t >= i64::from(d),
            "t={t} D={d}: the host's spent mark disagrees with its window"
        );
        phase_index(nova::phase(t, d))
    }
}

/// One contiguous phase window of a real walk and the blooms inside it.
struct Window {
    phase: i64,
    blooms: i64,
    at: Duration,
}

/// Tick the host at 1 ms from `start` until its episode has settled, freezing
/// it for `freeze.1` once the walk reaches `freeze.0` into the arm, and return
/// the phase windows it went through (the first is the one it was already in).
fn walk(host: &mut Host, start: Instant, freeze: Option<(Duration, Duration)>) -> Vec<Window> {
    let mut windows = vec![Window {
        phase: host.phase(start),
        blooms: 0,
        at: Duration::ZERO,
    }];
    let (mut now, mut elapsed, mut lit_before) = (start, Duration::ZERO, false);
    let mut frozen = freeze;
    loop {
        now += Duration::from_millis(1);
        elapsed += Duration::from_millis(1);
        if let Some((at, pause)) = frozen
            && elapsed >= at
        {
            // The pane is suspended: nothing ticks, the clock runs on.
            host.wd.freeze(now);
            now += pause;
            host.wd.thaw(now);
            frozen = None;
        }
        let lit = host.tick(now);
        let phase = host.phase(now);
        let bloom = i64::from(lit && !lit_before);
        lit_before = lit;
        let last = windows.last_mut().expect("a window");
        if last.phase == phase {
            last.blooms += bloom;
        } else {
            windows.push(Window {
                phase,
                blooms: bloom,
                at: elapsed,
            });
        }
        if phase == SETTLED && elapsed > Duration::from_secs(3) {
            return windows;
        }
        assert!(elapsed < Duration::from_secs(10), "the nova never settled");
    }
}

/// Validate a walk's window boundaries as the model's `Step`s from `cur`.
fn steps(model: &Model, mut cur: State, windows: &[Window], arm: i64) -> State {
    assert_eq!(
        windows[0].phase, cur["phase"],
        "arm {arm} opens where the model stands"
    );
    assert_eq!(
        windows[0].blooms, 0,
        "arm {arm}: no light before the first step"
    );
    for w in &windows[1..] {
        let next = state(
            w.phase,
            cur["steps"] + 1,
            cur["flashes"] + w.blooms,
            cur["arms"],
        );
        let (ok, why) = verify::validate_transition_tiered(
            model,
            &[],
            &cur,
            &next,
            Some("Step"),
            "nova host conformance",
        );
        assert!(
            ok,
            "arm {arm} at {:?}: real step {cur:?} -> {next:?}\n{why}",
            w.at
        );
        for inv in ["Monotone", "OneFlashPerArm", "CanSettle"] {
            assert!(
                model.check_invariant(inv, &next),
                "arm {arm}: {inv} broken by {next:?}"
            );
        }
        cur = next;
    }
    assert_eq!(cur["phase"], SETTLED, "arm {arm} settles");
    assert_eq!(cur["flashes"], 1, "arm {arm}: exactly one crown");
    cur
}

/// A host transition that is not a model step must leave the projection alone.
fn no_step(host: &mut Host, cur: &State, now: Instant, what: &str) {
    host.tick(now);
    let lit = host.tick(now + Duration::from_millis(300));
    assert!(!lit, "{what}: the spent word emitted nova light");
    assert_eq!(
        host.phase(now + Duration::from_millis(300)),
        cur["phase"],
        "{what}: the host re-ignited a spent word without a re-arm"
    );
}

/// The host's re-arm (`hard_reset` + rescan), validated as the model's `Rearm`:
/// a fresh episode, not yet granted, with no crown counted for the new arm.
fn rearm(model: &Model, host: &mut Host, cur: &State, now: Instant) -> State {
    host.wd.hard_reset();
    host.rescan(true, now);
    let next = state(host.phase(now), cur["steps"], 0, cur["arms"] + 1);
    let (ok, why) = verify::validate_transition_tiered(
        model,
        &[],
        cur,
        &next,
        Some("Rearm"),
        "nova host rearm conformance",
    );
    assert!(ok, "re-arm {cur:?} -> {next:?}\n{why}");
    next
}

#[test]
fn the_real_host_conforms_to_nova_phase_model() {
    let model = nova_phase_model();
    let t0 = Instant::now();
    let mut host = Host::new(t0);
    let mut cur = model.init_state();
    assert_eq!(
        host.phase(t0),
        cur["phase"],
        "a fresh word is the model's Init"
    );

    // Arm 0.
    let windows = walk(&mut host, t0, None);
    cur = steps(&model, cur, &windows, 0);
    let mut now = t0 + Duration::from_secs(4);

    // Occluded for one rescan and back within the grace window: the same
    // episode, still spent — no re-ignition.
    host.rescan(false, now);
    host.rescan(true, now + Duration::from_secs(1));
    now += Duration::from_secs(1);
    no_step(&mut host, &cur, now, "a grace re-hit");
    // Gone past the grace window and back: a new episode, born spent.
    now += Duration::from_secs(12);
    host.rescan(false, now);
    now += Duration::from_secs(1);
    host.rescan(true, now);
    no_step(&mut host, &cur, now, "an identity rebirth");

    // Arm 1, frozen mid-crown for two seconds.
    now += Duration::from_secs(1);
    cur = rearm(&model, &mut host, &cur, now);
    let crown_at = Duration::from_millis(nova::DIP_MS + 30);
    let windows = walk(&mut host, now, Some((crown_at, Duration::from_secs(2))));
    cur = steps(&model, cur, &windows, 1);

    // Arm 2.
    now += Duration::from_secs(8);
    cur = rearm(&model, &mut host, &cur, now);
    let windows = walk(&mut host, now, None);
    cur = steps(&model, cur, &windows, 2);

    let max_steps = model
        .consts
        .iter()
        .find(|(name, _)| *name == "MaxSteps")
        .map(|(_, v)| *v)
        .expect("MaxSteps");
    assert_eq!(
        cur["steps"], max_steps,
        "three real arms spend the whole step budget"
    );
}

/// The modelled defect on the REAL settled host state: a re-arm that lands in
/// `Flash` and counts a second crown.
#[test]
fn a_rearm_into_flash_is_the_buggy_step_the_host_bind_rejects() {
    let model = nova_phase_model();
    let t0 = Instant::now();
    let mut host = Host::new(t0);
    let windows = walk(&mut host, t0, None);
    let settled = steps(&model, model.init_state(), &windows, 0);
    let reflash = state(
        phase_index(NovaPhase::Flash),
        settled["steps"],
        settled["flashes"] + 1,
        settled["arms"] + 1,
    );
    let (committed_ok, _) = verify::validate_transition_tiered(
        &model,
        &[],
        &settled,
        &reflash,
        Some("Rearm"),
        "nova host rearm negative control",
    );
    assert!(!committed_ok, "the committed model must reject a re-flash");
    assert!(
        interp::with_buggy(&model, 1)
            .successors("Rearm", &settled)
            .contains(&reflash),
        "Buggy = 1 must admit exactly this re-flash"
    );
    assert!(!model.check_invariant("OneFlashPerArm", &reflash));
}
