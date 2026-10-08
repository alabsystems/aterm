// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1: the shipping `MessageCenter` follows the derived
//! `ExceptionsDigestEnds` model in lock step (AGENTS.md, derived models, step
//! 3), and so the liveness `AnUnnamedEntryEnds` proven on the model is a
//! property of the code. After every real step the projection `(open, missed,
//! age)` of the center equals the model's, where `missed` is "the center names
//! a deadline" (`digest_deadline`) and `age` counts the clock in halves of
//! `RESOLVE_AFTER`. The negative controls replay the second edition's two-look
//! rule and the early end against the mutant models and show the real center
//! is not what they describe.

use aterm_messages::need::{Fingerprint, LaterAsk, RESOLVE_AFTER, Reason, Resurface, Then, Why};
use aterm_messages::{
    Duration, Instant, Message, MessageCenter, MessageLog, Severity, WallStamp, tags,
};
use aterm_spec::derive::exceptions_digest_ends_model;
use aterm_spec::interp::{self, State};
use aterm_spec::verify;

const PREFIX: &str = "harness.upgrade.";
const TICKS: i64 = 2;

fn tick() -> Duration {
    Duration::from_millis(u64::try_from(RESOLVE_AFTER.as_millis()).expect("fits") / 2)
}

fn entry() -> Message {
    Message::new(
        tags::HARNESS,
        Severity::Warn,
        "Claude in tab 1 is still on 2.1.283",
    )
    .later(LaterAsk::person(
        Why::OnlyYou(Reason::new("its own background work holds the move")),
        Then::step("End that job in the tab"),
        Fingerprint::new("harness.upgrade", "", "own-work"),
        Resurface::WAIT,
    ))
}

struct Rng(u64);
impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

/// The real center, one entry open and named, and what the harness knows of the
/// run (the ghost `cause`, the tick count of the clock).
struct Real {
    c: MessageCenter,
    now: Instant,
    wall: u64,
    t_miss: Option<Instant>,
    age: i64,
    cause: i64,
    ended_age: i64,
}

impl Real {
    fn new() -> Self {
        let now = Instant::now();
        let mut c = MessageCenter::new(MessageLog::empty(), now);
        let wall = 1_790_000_000_000;
        let s = c.sync_digest(PREFIX, vec![entry()], WallStamp { unix_ms: wall }, now);
        assert_eq!((s.posted, s.pending_end), (1, 0));
        Self {
            c,
            now,
            wall,
            t_miss: None,
            age: 0,
            cause: 1,
            ended_age: 0,
        }
    }

    fn stamp(&self) -> WallStamp {
        WallStamp { unix_ms: self.wall }
    }

    /// The model's variables, read off the real center.
    fn project(&self) -> State {
        let mut s = State::new();
        s.insert("open", i64::try_from(self.c.digest().len()).expect("small"));
        s.insert("cause", self.cause);
        s.insert("missed", i64::from(self.c.digest_deadline().is_some()));
        s.insert("age", self.age);
        s.insert("ended_age", self.ended_age);
        s.insert("frozen", 0);
        // A clock that runs with no instant named for the loop to wake at.
        s.insert(
            "unwired",
            i64::from(self.t_miss.is_some() && self.c.digest_deadline().is_none()),
        );
        s
    }

    fn step(&mut self, action: &str) {
        match action {
            "CauseEnds" => self.cause = 0,
            // A look that does not name the entry: the reporter's whole answer is empty.
            "Miss" => {
                let s = self.c.sync_digest(PREFIX, vec![], self.stamp(), self.now);
                assert_eq!((s.pending_end, s.resolved), (1, 0), "{s:?}");
                self.t_miss = Some(self.now);
                self.age = 0;
                assert_eq!(
                    self.c.digest_deadline(),
                    Some(self.now + RESOLVE_AFTER),
                    "the clock names the instant the loop wakes at"
                );
            }
            // A look that names it again: a flicker neither ends nor re-posts a fact.
            "Name" => {
                let s = self
                    .c
                    .sync_digest(PREFIX, vec![entry()], self.stamp(), self.now);
                assert_eq!((s.posted, s.restated, s.pending_end), (0, 0, 0), "{s:?}");
                self.t_miss = None;
                self.age = 0;
                assert!(self.c.digest_deadline().is_none());
            }
            // Time passes; nothing else happens. Before the clock has run, neither the
            // loop's settle nor a second empty look ends the entry or restarts the clock.
            "Tick" => {
                self.now += tick();
                self.wall += u64::try_from(tick().as_millis()).expect("fits");
                self.age += 1;
                if self.age < TICKS {
                    assert!(!self.c.settle_digest(self.stamp(), self.now), "early end");
                    let again = self.c.sync_digest(PREFIX, vec![], self.stamp(), self.now);
                    assert_eq!((again.pending_end, again.resolved), (1, 0), "{again:?}");
                    let t0 = self.t_miss.expect("a clock is running");
                    assert_eq!(
                        self.c.digest_deadline(),
                        Some(t0 + RESOLVE_AFTER),
                        "a repeated empty look does not restart the clock"
                    );
                }
            }
            // The loop's wake at the deadline: no look is needed.
            "Settle" => {
                assert!(
                    self.c.settle_digest(self.stamp(), self.now),
                    "the wake ends it"
                );
                self.ended_age = self.age;
                self.t_miss = None;
                assert_eq!(self.c.digest().len(), 0);
                assert!(
                    self.c.digest_deadline().is_none(),
                    "the clock dies with the entry"
                );
            }
            other => panic!("not a healthy action: {other}"),
        }
    }
}

const HEALTHY: [&str; 5] = ["CauseEnds", "Miss", "Name", "Tick", "Settle"];

#[test]
fn the_real_center_follows_the_model_in_lock_step() {
    let model = exceptions_digest_ends_model();
    let mut fired = std::collections::BTreeMap::<&str, u32>::new();
    let mut ended_without_a_second_look = 0u32;
    for seed in 1..=30u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        for episode in 0..20 {
            let mut real = Real::new();
            let mut st = model.init_state();
            assert_eq!(real.project(), st, "seed {seed} episode {episode}: init");
            let mut looks = 0u32;
            for step in 0..60 {
                let enabled: Vec<&str> = HEALTHY
                    .iter()
                    .copied()
                    .filter(|a| model.action_enabled(a, &st))
                    .collect();
                if enabled.is_empty() {
                    break; // the entry has ended: nothing more is enabled
                }
                // Lean on time and the wake so runs end; looks and flickers stay possible.
                let leaning: Vec<&str> = enabled
                    .iter()
                    .copied()
                    .filter(|a| matches!(*a, "Tick" | "Settle"))
                    .collect();
                let action = if !leaning.is_empty() && rng.below(3) != 0 {
                    leaning[rng.below(leaning.len())]
                } else {
                    enabled[rng.below(enabled.len())]
                };
                let prev = st.clone();
                looks += u32::from(action == "Miss" || action == "Name");
                real.step(action);
                assert!(
                    model.fire(action, &mut st),
                    "seed {seed} step {step}: {action}"
                );
                let got = real.project();
                assert_eq!(
                    got, st,
                    "seed {seed} episode {episode} step {step}: {action}"
                );
                assert_eq!(
                    interp::admits(&model, &prev, &got),
                    Some(action),
                    "the model admits the real step, by name"
                );
                *fired.entry(action).or_default() += 1;
                if action == "Settle" && looks == 1 {
                    ended_without_a_second_look += 1;
                }
            }
        }
    }
    for a in HEALTHY {
        assert!(
            fired.get(a).copied().unwrap_or(0) > 0,
            "{a} never fired: {fired:?}"
        );
    }
    assert!(
        ended_without_a_second_look > 0,
        "the run the second edition got stuck on happened, and the real center ended it"
    );
}

/// The second edition ended an entry at a SECOND missed look. The mutant model
/// (`TwoLooksToEnd`) is that rule; the real center, given one empty look and
/// the passing of the clock, ends the entry, which the mutant does not admit.
#[test]
fn the_second_editions_two_look_rule_is_not_this_code() {
    let model = exceptions_digest_ends_model();
    let two = verify::buggy_baseline_with(&model, "TwoLooksToEnd");
    let mut real = Real::new();
    let mut healthy = model.init_state();
    let mut mutant = two.init_state();
    real.step("CauseEnds");
    assert!(model.fire("CauseEnds", &mut healthy));
    assert!(two.fire("CauseEnds", &mut mutant));
    // ONE look, and no second look ever comes.
    real.step("Miss");
    assert!(model.fire("Miss", &mut healthy));
    assert!(two.fire("TwoLooksToEnd", &mut mutant));
    assert!(
        !two.action_enabled("Tick", &mutant),
        "the mutant's clock never runs"
    );
    assert!(
        !two.action_enabled("Settle", &mutant),
        "and nothing ends the entry"
    );
    for a in ["Tick", "Tick", "Settle"] {
        real.step(a);
        assert!(model.fire(a, &mut healthy), "{a}");
    }
    let got = real.project();
    assert_eq!(
        got["open"], 0,
        "the real center ended the entry with no second look"
    );
    assert_eq!(got, healthy);
    // The mutant model, holding the same look, cannot take the step the code took.
    let mut before = mutant.clone();
    before.insert("age", 2);
    assert_eq!(
        interp::admits(&two, &before, &got),
        None,
        "not the mutant's machine"
    );
    assert_eq!(
        interp::admits(
            &model,
            &{
                let mut prev = got.clone();
                prev.insert("open", 1);
                prev.insert("missed", 1);
                prev.insert("ended_age", 0);
                prev
            },
            &got
        ),
        Some("Settle")
    );
}

/// The early end: two looks a few milliseconds apart (or the loop's settle) must
/// not end a fact the reporter merely flickered, and a repeated empty look must
/// not push the deadline out either. `EndsBeforeTheClockRan` is the mutant.
#[test]
fn a_flicker_is_not_an_end_and_a_repeated_look_does_not_move_the_clock() {
    let mut real = Real::new();
    real.step("Miss");
    let deadline = real.c.digest_deadline().expect("armed");
    // a second empty look at once, and one after a tick, in the same pass shape
    for _ in 0..3 {
        let s = real.c.sync_digest(PREFIX, vec![], real.stamp(), real.now);
        assert_eq!((s.pending_end, s.resolved), (1, 0));
        assert_eq!(real.c.digest_deadline(), Some(deadline));
    }
    real.step("Tick");
    assert_eq!(
        real.c.digest().len(),
        1,
        "half the clock has run: still open"
    );
    // named again: the clock is gone and the entry is unchanged
    real.step("Name");
    assert_eq!(real.c.digest().len(), 1);
    assert!(real.c.digest_deadline().is_none());
    // a NEW miss starts a NEW clock, from its own instant (time passed unarmed)
    real.now += tick();
    real.wall += u64::try_from(tick().as_millis()).expect("fits");
    real.step("Miss");
    assert_eq!(real.c.digest_deadline(), Some(real.now + RESOLVE_AFTER));
    assert!(matches!(real.project()["open"], 1));
}
