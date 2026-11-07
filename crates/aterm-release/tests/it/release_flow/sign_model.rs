// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for `ReleaseSignBesidePublish`
//! (`aterm_models_release::release_sign_beside_publish_model`).
//!
//! Drives the release against the fake world ([`super::World`]), step 7's source and sign sides
//! running at once on their own threads, and replays what the run said from step 7 on — each
//! `publish-branch` event, and the publish and cut steps' `release-stage` outcomes — as the
//! model's actions: every one must be admitted, in the order the run said it, and the model's
//! publication, claim and private copy must match the world's. The negative controls replay the
//! defects the model's laws exist for and must be refused.

use super::{SideOrder, World, bump, mac};

use aterm_models_release::release_sign_beside_publish_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::State;

struct Replay {
    model: Model,
    state: State,
    /// The actions taken, for the failure message.
    taken: Vec<String>,
}

impl Replay {
    fn new() -> Self {
        let model = release_sign_beside_publish_model();
        let state = model.init_state();
        Replay {
            model,
            state,
            taken: Vec::new(),
        }
    }

    fn next(&self, action: &str) -> Option<State> {
        let mut next = self.model.successors(action, &self.state).into_iter();
        let one = next.next();
        assert!(
            next.next().is_none(),
            "{action} is deterministic in the model"
        );
        one
    }

    /// Take `action`, which the model must admit from where the replay stands.
    fn fire(&mut self, action: &str) {
        let next = self.next(action).unwrap_or_else(|| {
            panic!(
                "the model refuses the run's {action} after {:?}\nat {:?}",
                self.taken, self.state
            )
        });
        let (ok, why) = aterm_spec::verify::validate_transition_tiered(
            &self.model,
            &[],
            &self.state,
            &next,
            Some(action),
            &format!(
                "ReleaseSignBesidePublish step {} {action}",
                self.taken.len() + 1
            ),
        );
        assert!(ok, "{action} after {:?}: {why}", self.taken);
        self.taken.push(action.to_owned());
        self.state = next;
    }

    /// The model refuses `action` from here: the defect it stands for is caught.
    fn refuses(&self, action: &str) {
        assert!(
            self.next(action).is_none(),
            "NEGATIVE CONTROL passed the model — {action} after {:?} was admitted; the \
             conformance would not catch the defect it replays",
            self.taken
        );
    }

    fn var(&self, name: &str) -> i64 {
        *self
            .state
            .get(name)
            .unwrap_or_else(|| panic!("no model variable {name}"))
    }
}

/// Replay `w`'s last run from step 7 on; `last` is its final line.
fn replay(w: &World, last: &str) -> Replay {
    let events: Vec<aterm_json::Value> = w
        .out
        .iter()
        .filter_map(|l| l.strip_prefix("release-event: "))
        .map(|json| aterm_json::from_str(json).unwrap())
        .collect();
    let side_of = |e: &aterm_json::Value| {
        (e["event"].as_str() == Some("publish-branch"))
            .then(|| e["branch"].as_str().unwrap().to_owned())
    };
    let mut r = Replay::new();
    if !events
        .iter()
        .any(|e| side_of(e).as_deref() == Some("source"))
    {
        r.fire("SkipSource");
    }
    r.fire("Begin");
    for e in &events {
        if let Some(side) = side_of(e) {
            let x = if side == "source" { "Source" } else { "Sign" };
            match e["phase"].as_str().unwrap() {
                "started" => r.fire(&format!("Start{x}")),
                "push-began" => r.fire("SourcePush"),
                "passed" => r.fire(&format!("{x}Pass")),
                "red" => r.fire(&format!("{x}Red")),
                "could-not-run" => r.fire(&format!("{x}CouldNotRun")),
                "stopped" => r.fire(&format!("Stop{x}")),
                phase => panic!("no phase {phase}"),
            }
            continue;
        }
        if e["event"].as_str() != Some("release-stage") {
            continue;
        }
        let claimed = w.cut.as_ref().map(|cut| cut.finished);
        let action: &[&str] = match (e["stage"].as_str().unwrap(), e["outcome"].as_str().unwrap()) {
            ("publish", "ok") => &["PublishPass"],
            ("publish", "FAIL" | "NOT RUN") => &["PublishRefuse"],
            ("cut", "ok") => &["Claim", "CutPublish"],
            ("cut", "FAIL" | "NOT RUN") => match claimed {
                Some(_) => &["Claim", "CutRefuse"],
                None if last.contains("lost the ledger's race") => &["LoseRace"],
                None => &["ClaimRefused"],
            },
            _ => &[],
        };
        for a in action {
            r.fire(a);
        }
    }
    r
}

/// The model's publication, claim and private copy match the world's.
fn agrees(r: &Replay, w: &World, case: &str) {
    let commit = w.journal().unwrap().commit;
    assert_eq!(
        r.var("src_public") == 1,
        w.published.contains_key(&commit),
        "{case}: {:?}",
        r.taken
    );
    assert_eq!(
        r.var("bin_public") == 1,
        w.cut.as_ref().is_some_and(|cut| cut.finished),
        "{case}"
    );
    assert_eq!(r.var("claimed") == 1, w.cut.is_some(), "{case}");
    assert_eq!(r.var("record") == 1, w.signed.is_some(), "{case}");
}

/// Every run's step 7 and 8 is a path of the model: both sides passing in either order and
/// together, a source red that stops the sign, a sign red before the source's push (nothing
/// public) and after it (the source finishes), a sign that could not run and its resume (the
/// sign alone), and a claim that lost the ledger's race.
#[test]
fn every_sign_beside_publish_run_the_release_makes_is_a_path_of_the_model() {
    type Setup = fn(&mut World);
    let cases: [(&str, Setup, Option<Setup>); 8] = [
        ("source first", |_| {}, None),
        ("sign first", |w| w.sides = SideOrder::SignFirst, None),
        ("both at once", |w| w.sides = SideOrder::Overlap, None),
        ("source red", |w| w.publish_code = 1, None),
        (
            "sign red before the push",
            |w| {
                w.sides = SideOrder::SignFirst;
                w.sign_code = 1;
            },
            None,
        ),
        (
            "sign red after the push",
            |w| {
                w.sides = SideOrder::PushThenSign;
                w.sign_code = 1;
            },
            Some(|w| w.sign_code = 0),
        ),
        (
            "sign could not run",
            |w| w.sign_code = 130,
            Some(|w| w.sign_code = 0),
        ),
        ("lost race", |w| w.overtaken_at_cut = true, None),
    ];
    for (case, setup, mend) in cases {
        let mut w = World::new();
        setup(&mut w);
        let (code, last) = w.run(bump());
        let r = replay(&w, &last);
        let passes = matches!(case, "source first" | "sign first" | "both at once");
        assert_eq!(code == 0, passes, "{case}: {last}");
        assert_eq!(
            r.var("phase"),
            match case {
                _ if passes => 4,
                "lost race" => 6,
                _ => 5,
            },
            "{case}: {:?}",
            r.taken
        );
        agrees(&r, &w, case);
        if let Some(mend) = mend {
            mend(&mut w);
            let (code, last) = w.run(mac());
            assert_eq!(code, 0, "{case}, resumed: {last}");
            let r = replay(&w, &last);
            assert_eq!(r.var("phase"), 4, "{case}, resumed: {:?}", r.taken);
            assert_eq!(
                r.var("source"),
                6,
                "{case}, resumed: the source was published"
            );
            agrees(&r, &w, case);
        }
    }
}

/// NEGATIVE CONTROLS: the defects the model's laws are for, replayed, are each refused — a claim
/// before the sign passed, a push after the sign's red took the gate, a push in flight stopped,
/// a side stopped on a COULD NOT RUN, step 7 ended while a side runs, and a binary made public
/// before the claim.
#[test]
fn the_model_refuses_the_defects_step_seven_must_not_have() {
    let start = || {
        let mut r = Replay::new();
        r.fire("Begin");
        r.fire("StartSource");
        r.fire("StartSign");
        r
    };
    let mut r = start();
    r.fire("SourcePush");
    r.fire("SourcePass");
    r.refuses("PublishPass");
    r.refuses("Claim");

    let mut r = start();
    r.fire("SignRed");
    r.refuses("SourcePush");
    r.fire("StopSource");
    r.refuses("PublishPass");

    let mut r = start();
    r.fire("SourcePush");
    r.fire("SignRed");
    r.refuses("StopSource");
    r.fire("SourcePass");
    r.fire("PublishRefuse");
    r.refuses("Claim");

    let mut r = start();
    r.fire("SourceCouldNotRun");
    r.refuses("StopSign");

    let mut r = start();
    r.fire("SourceRed");
    r.refuses("PublishRefuse");
    r.fire("StopSign");
    r.fire("PublishRefuse");

    let mut r = start();
    r.fire("SourcePush");
    r.fire("SourcePass");
    r.fire("SignPass");
    r.fire("PublishPass");
    r.refuses("CutPublish");
    r.fire("LoseRace");
    r.refuses("Claim");
    assert_eq!(r.var("record"), 0, "a lost race discards the copy");
}
