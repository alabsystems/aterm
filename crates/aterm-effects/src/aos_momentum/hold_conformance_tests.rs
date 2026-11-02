// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance of the host's key hold to `AosKeyHold`
//! (`aterm_models_aos::aos_key_hold_model`, ATERM-KI-1025-1): driven as the GUI drives it, the
//! real [`MomentumHost`] takes every step the model takes, in lock step, over seeded walks and
//! every transition of the bounded space.
//!
//! The projection reads `att` and `glow` (the host's taint) off the host; `held` and `echoed` are
//! the harness's, and the host's own reading of a hold must agree with `held`. The ghosts are the
//! real code's: `leak` is the log growing across a held step that logs nothing, `gap` an
//! export of the recording (its checkpoint and log) replayed to a state other than the live one,
//! and `dirty` an attach checkpoint that carries a typing-rate stamp while the harness knows the
//! glow took a key no recording logged. `unproven` is not the host's to judge (it never sees a
//! character): aterm-gui's binding drives that claim through the echo proof.
//!
//! The steps are the GUI's calls: `Private` and `Refute` are `private_key`'s stop (the retired
//! segments taken, the recording dropped, the glow tainted), the held keys then handed to the glow
//! unrecorded, and for `Private` the key itself; `Detach` is `aos detach`, which hands the held
//! keys over the same way. `Echo` is the program's and calls nothing.

use std::collections::BTreeSet;
use std::time::Duration;

use aterm_aos::task::state_digest;
use aterm_models_aos::aos_key_hold_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};
use aterm_time::Instant;

use super::test_fixtures::{BUILD, LANE, TAU, cfg, draw, geom, hosted, replay_log};
use super::{HELD_TICK_DELIVERS, MomentumCode, MomentumHost, MomentumSchema, MomentumState};

/// Seeds of the lock-step walk, each run for [`EPISODES`] episodes of at most [`STEPS`] moves.
const SEEDS: u64 = 32;
const EPISODES: u64 = 8;
const STEPS: usize = 40;
/// The GUI's bound on a held key's echo.
const BOUND: Duration = Duration::from_secs(1);
/// The time between two calls.
const STEP: Duration = Duration::from_millis(7);

/// A deterministic xorshift stream: the walk draws nothing from the host.
struct Rng(u64);

impl Rng {
    fn new(seed: u64, episode: u64) -> Rng {
        Rng((seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ episode.wrapping_add(1)) | 1)
    }

    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        usize::try_from(self.0 % u64::try_from(n).expect("small")).expect("small")
    }
}

/// The real host and the harness's half of the state.
struct Real {
    host: MomentumHost,
    now: Instant,
    held: i64,
    echoed: i64,
    attaches: u64,
    /// Whether the harness handed the glow a key no recording logged since the last attach.
    unlogged: bool,
    leak: i64,
    gap: i64,
    dirty: i64,
}

impl Real {
    fn new() -> Real {
        Real {
            host: MomentumHost::default(),
            now: Instant::now(),
            held: 0,
            echoed: 0,
            attaches: 0,
            unlogged: false,
            leak: 0,
            gap: 0,
            dirty: 0,
        }
    }

    /// The events the recording has logged since its ancestor checkpoint.
    fn log_len(&self) -> Option<usize> {
        self.host
            .aos_attached()
            .then(|| hosted(&self.host).boundary.log().map_or(0, |l| l.len()))
    }

    /// The model's view of the host and the harness.
    fn project(&self) -> State {
        assert_eq!(
            self.host.aos_holding(),
            self.held > 0,
            "the host holds exactly while keys wait"
        );
        let mut s = State::new();
        for (name, v) in [
            ("att", i64::from(self.host.aos_attached())),
            ("held", self.held),
            ("echoed", self.echoed),
            ("glow", i64::from(self.host.aos_tainted())),
            ("leak", self.leak),
            ("unproven", 0),
            ("gap", self.gap),
            ("dirty", self.dirty),
        ] {
            s.insert(name, v);
        }
        s
    }

    /// `private_key`'s stop and the held keys handed to the glow, as the GUI makes them.
    fn stop_for_privacy(&mut self) {
        let _ = self.host.aos_take_retired();
        self.host.aos_detach();
        self.host.taint();
        self.hand_over_held();
    }

    /// The held keys to the glow, unrecorded.
    fn hand_over_held(&mut self) {
        if self.held > 0 {
            self.unlogged = true;
        }
        for _ in 0..self.held {
            self.host.on_key(self.now, TAU);
        }
        self.held = 0;
        self.echoed = 0;
    }

    /// Makes the GUI's calls for `action`.
    fn act(&mut self, action: &str) {
        self.now += STEP;
        let now = self.now;
        let before = (self.held > 0).then(|| self.log_len()).flatten();
        match action {
            "Attach" => {
                self.attaches += 1;
                self.host
                    .aos_attach(now, MomentumCode::Shipped, BUILD, self.attaches);
                let b = &hosted(&self.host).boundary;
                let start: MomentumState = b
                    .checkpoint()
                    .expect("attached")
                    .restore(&MomentumSchema::new(now), b.identity())
                    .expect("restores");
                let stamped = start.glow().snapshot(now).expect("a snapshot").rate_at;
                if self.unlogged && stamped.is_some() {
                    self.dirty = 1;
                }
                self.unlogged = false;
            }
            "Detach" => {
                self.host.aos_detach();
                self.hand_over_held();
            }
            "Key" => self.host.on_key(now, TAU),
            "Private" => {
                self.stop_for_privacy();
                self.unlogged = true;
                self.host.on_key(now, TAU);
            }
            "Hold" => {
                if self.held == 0 {
                    self.host.aos_hold(now + BOUND);
                }
                self.held += 1;
            }
            "Echo" => self.echoed += 1,
            "Tick" => {
                let _ = draw(&mut self.host, Some((4, 10)), now, geom(), &cfg(true), 1);
                let _ = self.host.next_change_deadline(now, LANE);
                let _ = (self.host.is_active(), self.host.settling());
            }
            "Release" => {
                let rest = (self.held > 1).then_some(now + BOUND);
                self.host.aos_release(now, 1, TAU, rest);
                self.held -= 1;
                self.echoed -= 1;
            }
            "Refute" => self.stop_for_privacy(),
            _ => panic!("no real call for the model action `{action}`"),
        }
        if let Some(n) = before
            && matches!(action, "Hold" | "Echo" | "Tick")
            && self.log_len() != Some(n)
        {
            self.leak = 1;
        }
        if self.host.aos_attached() {
            let run = replay_log(&self.host, MomentumCode::Shipped);
            let h = hosted(&self.host);
            if run.refused.is_some()
                || run.state_digest != state_digest(h.boundary.schema(), h.state())
            {
                self.gap = 1;
            }
        }
    }
}

/// One move: the action, the model's state before it and the real projection after it.
struct Step {
    action: &'static str,
    prev: State,
    got: State,
}

/// A real host walked in lock step with `model`.
struct Walker<'m> {
    model: &'m Model,
    real: Real,
    st: State,
    at: String,
}

impl<'m> Walker<'m> {
    fn new(model: &'m Model, at: String) -> Self {
        let real = Real::new();
        let st = model.init_state();
        assert_eq!(
            real.project(),
            st,
            "{at}: a fresh host is the initial state"
        );
        Walker {
            model,
            real,
            st,
            at,
        }
    }

    /// Takes `action` on the real host and on the model, then checks that the projection is the
    /// model's state and that the model admits the real step by that name.
    fn take(&mut self, action: &'static str) -> Step {
        self.real.act(action);
        let prev = self.st.clone();
        assert!(
            self.model.fire(action, &mut self.st),
            "{}: `{action}` is enabled",
            self.at
        );
        let got = self.real.project();
        assert_eq!(got, self.st, "{}: `{action}` from {prev:?}", self.at);
        assert!(
            self.model.successors(action, &prev).contains(&got),
            "{}: the model admits the real step by its name, `{action}`",
            self.at
        );
        Step { action, prev, got }
    }
}

/// The actions the model allows in `st`, in model order.
fn allowed(model: &Model, st: &State) -> Vec<&'static str> {
    model
        .actions
        .iter()
        .map(|a| a.name)
        .filter(|a| model.action_enabled(a, st))
        .collect()
}

/// Each reachable state of `model`, once, with the shortest action path to it (breadth first).
fn shortest_paths(model: &Model) -> Vec<(State, Vec<&'static str>)> {
    let init = model.init_state();
    let mut seen = BTreeSet::from([init.clone()]);
    let mut tree = vec![(init, Vec::new())];
    let mut next = 0;
    while let Some((state, path)) = tree.get(next).cloned() {
        for a in &model.actions {
            for succ in model.successors(a.name, &state) {
                if seen.insert(succ.clone()) {
                    let mut longer = path.clone();
                    longer.push(a.name);
                    tree.push((succ, longer));
                }
            }
        }
        next += 1;
    }
    tree
}

/// Every transition of the model's bounded space, each on a fresh host: the shortest path to its
/// state replayed, then `each` given the walker and the action to take. Returns how many.
fn every_transition(mut each: impl FnMut(&mut Walker<'_>, &'static str)) -> usize {
    let model = aos_key_hold_model();
    let tree = shortest_paths(&model);
    let reachable: BTreeSet<State> = interp::reachable_states(&model).into_iter().collect();
    assert_eq!(
        tree.iter().map(|(s, _)| s.clone()).collect::<BTreeSet<_>>(),
        reachable
    );
    let mut taken = 0usize;
    for (i, (state, path)) in tree.iter().enumerate() {
        for action in allowed(&model, state) {
            let mut w = Walker::new(&model, format!("state {i} by {path:?}"));
            for a in path {
                w.take(a);
            }
            assert_eq!(&w.st, state, "{}", w.at);
            w.at = format!("{}, then {action}", w.at);
            each(&mut w, action);
            taken += 1;
        }
    }
    let transitions: usize = reachable.iter().map(|s| allowed(&model, s).len()).sum();
    assert_eq!(taken, transitions);
    taken
}

#[test]
fn the_real_host_follows_the_hold_model_in_lock_step() {
    let model = aos_key_hold_model();
    let mut fired = BTreeSet::new();
    for seed in 1..=SEEDS {
        for episode in 0..EPISODES {
            let mut rng = Rng::new(seed, episode);
            let mut w = Walker::new(&model, format!("seed {seed} episode {episode}"));
            for _ in 0..STEPS {
                let moves = allowed(&model, &w.st);
                let action = moves[rng.below(moves.len())];
                fired.insert(w.take(action).action);
            }
        }
    }
    assert_eq!(
        fired,
        interp::fired_actions(&model),
        "the real host took every action"
    );
}

#[test]
fn every_transition_of_the_hold_model_is_a_real_step() {
    let mut fired = BTreeSet::new();
    let taken = every_transition(|w, action| {
        fired.insert(w.take(action).action);
    });
    assert!(taken > 20, "the space is not trivial: {taken}");
    assert_eq!(fired, interp::fired_actions(&aos_key_hold_model()));
}

/// The negative control of a dial that changes an existing action's update: at every transition
/// `applies` picks, the healthy model admits the real step (checked by `take`), the mutant does
/// not by that name, and the mutant's own step from the same state breaks `law`. Returns how many it caught.
fn changed_update_control(dial: i64, law: &str, applies: impl Fn(&Step) -> bool) -> u32 {
    let mutant = interp::with_buggy(&aos_key_hold_model(), dial);
    let mut caught = 0u32;
    every_transition(|w, action| {
        let s = w.take(action);
        if !applies(&s) {
            return;
        }
        assert!(
            !mutant.successors(s.action, &s.prev).contains(&s.got),
            "Buggy {dial}: `{}` from {:?}",
            s.action,
            s.prev
        );
        let wrong = mutant
            .successors(s.action, &s.prev)
            .into_iter()
            .next()
            .expect("the mutant takes the step");
        assert!(
            !mutant.check_invariant(law, &wrong),
            "Buggy {dial}: {wrong:?}"
        );
        caught += 1;
    });
    caught
}

#[test]
fn negative_control_a_held_tick_is_caught_by_the_model_and_by_the_projection() {
    let caught = changed_update_control(1, "NothingLoggedWhileHeld", |s| {
        s.action == "Tick" && s.prev["held"] > 0
    });
    assert!(caught > 0, "the space has a held tick");

    let model = aos_key_hold_model();
    let mutant = interp::with_buggy(&model, 1);
    HELD_TICK_DELIVERS.with(|on| on.set(true));
    let mut w = Walker::new(&mutant, "the held-tick seam".into());
    w.real.act("Attach");
    w.real.act("Hold");
    let prev = w.real.project();
    w.real.act("Tick");
    let got = w.real.project();
    HELD_TICK_DELIVERS.with(|on| on.set(false));
    assert_eq!(got["leak"], 1, "the projection saw the log grow while held");
    assert_eq!(
        interp::admits(&model, &prev, &got),
        None,
        "the healthy model refuses it"
    );
    assert!(
        mutant.successors("Tick", &prev).contains(&got),
        "the mutant's step"
    );
}

#[test]
fn negative_control_a_refute_that_keeps_recording() {
    let caught = changed_update_control(3, "HostedGlowMatchesItsLog", |s| s.action == "Refute");
    assert!(caught > 0, "the space has a refute");
}

#[test]
fn negative_control_an_attach_that_keeps_an_unlogged_key() {
    let caught = changed_update_control(4, "AttachStartsFromRest", |s| {
        s.action == "Attach" && s.prev["glow"] == 1
    });
    assert!(caught > 0, "the space has an attach over a tainted glow");
}

#[test]
fn a_release_logs_exactly_the_keys_it_names() {
    let model = aos_key_hold_model();
    let mut w = Walker::new(&model, "a release".into());
    for a in ["Attach", "Hold", "Hold", "Echo", "Echo"] {
        w.take(a);
    }
    let n = w.real.log_len().expect("attached");
    w.take("Release");
    assert_eq!(w.real.log_len(), Some(n + 1), "one key, and nothing else");
    assert!(w.real.host.aos_holding(), "the second still waits");
    w.take("Release");
    assert_eq!(w.real.log_len(), Some(n + 2));
    assert_eq!(w.real.held, 0);
}
