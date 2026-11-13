// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for `ReleaseQualify` (`aterm_models_release::release_qualify_model`).
//!
//! Drives the release's steps 4 to 8 against the fake world ([`super::World`]). AppKit's
//! synchronous gate must pass before the qualify graph starts; then the graph's branches run at
//! once on their own threads. The replay maps each `qualify-branch` event and each step's
//! `release-stage` outcome to the model's actions: every one must be admitted, in the order the
//! run said it, and the model's records and verdicts must match the world's. The negative
//! controls replay the defects the model's laws exist for and must be refused.

use super::{BTreeSet, Branch, Half, World, bump, mac};

use aterm_models_release::release_qualify_model;
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
        let model = release_qualify_model();
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
            &format!("ReleaseQualify step {} {action}", self.taken.len() + 1),
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

fn camel(branch: &str) -> &'static str {
    match branch {
        "tree" => "Tree",
        "build" => "Build",
        "export" => "Export",
        other => panic!("no branch {other}"),
    }
}

/// Whether a run that needed AppKit put that synchronous gate before the full disk admission,
/// every modeled branch and the bundle invocation. `None` means AppKit's standing record made an
/// invocation unnecessary.
fn appkit_precedes_graph(calls: &[String]) -> Option<bool> {
    let appkit = calls.iter().position(|call| call == "verify appkit")?;
    Some(calls.iter().enumerate().all(|(at, call)| {
        let after_gate = call.starts_with("disk-claim")
            || matches!(
                call.as_str(),
                "reserve"
                    | "build"
                    | "verify --tree"
                    | "stage --dry-run"
                    | "stage"
                    | "verify --bundle"
            );
        !after_gate || appkit < at
    }))
}

/// Replay `w`'s last run from its step 4 on.
fn replay(w: &World) -> Replay {
    assert_ne!(
        appkit_precedes_graph(&w.calls),
        Some(false),
        "AppKit's synchronous gate must precede the modeled graph: {:?}",
        w.calls
    );
    let events: Vec<aterm_json::Value> = w
        .out
        .iter()
        .filter_map(|l| l.strip_prefix("release-event: "))
        .map(|json| aterm_json::from_str(json).unwrap())
        .collect();
    let branch_of = |e: &aterm_json::Value| {
        (e["event"].as_str() == Some("qualify-branch"))
            .then(|| e["branch"].as_str().unwrap().to_owned())
    };
    let planned: BTreeSet<String> = events.iter().filter_map(branch_of).collect();
    let mut r = Replay::new();
    for branch in ["tree", "build", "export"] {
        if !planned.contains(branch) {
            r.fire(&format!("Skip{}", camel(branch)));
        }
    }
    r.fire("Plan");
    let mut started = BTreeSet::new();
    for e in &events {
        if let Some(branch) = branch_of(e) {
            let x = camel(&branch);
            match e["phase"].as_str().unwrap() {
                "started" => {
                    started.insert(branch.clone());
                    r.fire(&format!("Start{x}"));
                }
                "push-began" => r.fire("ExportPush"),
                "passed" => {
                    // A local export, and a fake's, says no push: it is the pass's last part.
                    if x == "Export" && r.var("export") == 1 {
                        r.fire("ExportPush");
                    }
                    r.fire(&format!("{x}Pass"));
                }
                "red" if x == "Tree" && w.keep_build_on_test_red => r.fire("TreeTestRed"),
                "red" => r.fire(&format!("{x}Red")),
                "could-not-run" => r.fire(&format!("{x}CouldNotRun")),
                // A branch the red stopped before it started never ran: the model keeps it idle.
                "stopped" if started.contains(&branch) => r.fire(&format!("Stop{x}")),
                "stopped" => {}
                phase => panic!("no phase {phase}"),
            }
            continue;
        }
        if e["event"].as_str() != Some("release-stage") {
            continue;
        }
        let action: &[&str] = match (e["stage"].as_str().unwrap(), e["outcome"].as_str().unwrap()) {
            ("qualify", "ok") => &["QualifyPass"],
            ("qualify", "FAIL" | "NOT RUN") => &["QualifyRefuse"],
            ("bundle", "skip") => &["SkipBundle"],
            ("bundle", "ok") => &["StartBundle", "BundlePass"],
            ("bundle", "FAIL") => &["StartBundle", "BundleRed"],
            ("bundle", "NOT RUN") => &["StartBundle", "BundleCouldNotRun"],
            ("ready", "ok") => &["Ready"],
            ("ready", "FAIL" | "NOT RUN") => &["NotReady"],
            ("publish", "ok") => &["Publish"],
            ("cut", "ok") => &["Cut"],
            _ => &[],
        };
        for a in action {
            r.fire(a);
        }
    }
    r
}

/// The model's records, verdict and publication match the world's.
fn agrees(r: &Replay, w: &World, case: &str) {
    let commit = w.journal().unwrap().commit;
    let checks = w.checks_of(&commit, w.macho().as_deref());
    let tree_recorded = aterm_verify::checks::RELEASE_TREE
        .iter()
        .all(|c| !checks.missing.iter().any(|m| m == c.name()))
        && checks.failed.is_empty();
    assert_eq!(
        r.var("tree_rec") == 2,
        tree_recorded,
        "{case}: {}",
        checks.line()
    );
    assert_eq!(r.var("public") == 1, w.count("publish") > 0, "{case}");
    assert_eq!(r.var("cut") == 1, w.cut.is_some(), "{case}");
    // The graph models export validation. Remote staging additionally requires that
    // qualify accepted the whole graph; a local pass beside a refusal cannot attest it.
    if !case.contains("--qualify") {
        assert_eq!(
            matches!(r.var("export"), 2 | 6)
                && r.taken.iter().any(|action| action == "QualifyPass"),
            w.attested.as_deref() == Some(commit.as_str()),
            "{case}: R is staged only after the graph passes"
        );
    }
}

/// Every run's step 4 to 8 is a path of the model: a release that passes, a red that stops the
/// rest before they start, a red after private export validation, a COULD NOT RUN that stops
/// nothing and its resume, a private qualification whose export is red while the tree and the
/// build run, and a bundle half that could not run and its resume.
#[test]
fn every_qualify_run_the_release_makes_is_a_path_of_the_model() {
    type Setup = fn(&mut World);
    let cases: [(&str, Setup, Option<Setup>); 6] = [
        ("passes", |_| {}, None),
        (
            "build red before the tree starts",
            |w| {
                w.build_claims_late = true;
            },
            None,
        ),
        (
            "tree red after private export validation",
            |w| {
                w.verify_half = Some(Half::Tree);
                w.verify_outcomes.insert("test", "FAIL");
                w.verify_code = 1;
                w.export_pushes = true;
            },
            None,
        ),
        (
            "tree could not run",
            |w| {
                w.verify_half = Some(Half::Tree);
                w.verify_passes = false;
                w.verify_code = 3;
                w.after_tree = true;
            },
            Some(|w| {
                w.verify_half = None;
                w.verify_passes = true;
                w.verify_code = 0;
                w.after_tree = false;
            }),
        ),
        (
            "bundle could not run",
            |w| {
                w.verify_half = Some(Half::Bundle);
                // This case is the late, bundle-dependent measure run. AppKit has the same
                // canonical half, but its explicit pass lets the synchronous gate finish first.
                w.verify_outcomes.insert("appkit", "ok");
                w.verify_passes = false;
                w.verify_code = 3;
            },
            Some(|w| {
                w.verify_half = None;
                w.verify_passes = true;
                w.verify_code = 0;
            }),
        ),
        (
            "export red under --qualify",
            |w| {
                w.versions.insert(super::TIP.into(), "0.6.0".into());
                w.stage_stages = false;
                w.holds = BTreeSet::from([Branch::Tree, Branch::Build]);
            },
            None,
        ),
    ];
    for (case, setup, mend) in cases {
        let mut w = World::new();
        setup(&mut w);
        let opts = if case.contains("--qualify") {
            crate::release::ReleaseOptions {
                qualify: true,
                commit: Some(super::TIP.into()),
                ..mac()
            }
        } else {
            bump()
        };
        let (code, last) = w.run(opts);
        let r = replay(&w);
        let refused = mend.is_some() || case != "passes";
        assert_eq!(code == 0, !refused, "{case}: {last}");
        assert_eq!(
            r.var("phase"),
            if refused { 6 } else { 5 },
            "{case}: {:?}",
            r.taken
        );
        agrees(&r, &w, case);
        if let Some(mend) = mend {
            mend(&mut w);
            let (code, last) = w.run(mac());
            assert_eq!(code, 0, "{case}, resumed: {last}");
            let r = replay(&w);
            assert_eq!(r.var("phase"), 5, "{case}, resumed: {:?}", r.taken);
            agrees(&r, &w, case);
        }
    }
}

/// NEGATIVE CONTROLS: the defects the model's laws are for, replayed, are each refused — the
/// bundle half (measure) before the tree ended, a branch started after a red, a staging push
/// begun after a red, a push in flight stopped, and step 4 passed on a COULD NOT RUN.
#[test]
fn the_model_refuses_the_defects_the_graph_must_not_have() {
    let start = || {
        let mut r = Replay::new();
        r.fire("Plan");
        r.fire("StartBuild");
        r.fire("StartTree");
        r.fire("StartExport");
        r
    };
    let mut r = start();
    r.fire("TreeTestRed");
    r.refuses("StopBuild");
    r.fire("BuildPass");

    let mut r = start();
    r.fire("BuildPass");
    r.fire("ExportPush");
    r.fire("ExportPass");
    r.refuses("QualifyPass");
    r.refuses("StartBundle");

    let mut r = Replay::new();
    r.fire("Plan");
    r.refuses("StartTree");
    r.fire("StartBuild");
    r.fire("BuildRed");
    r.refuses("StartTree");

    let mut r = start();
    r.fire("TreeRed");
    r.refuses("ExportPush");

    let mut r = start();
    r.fire("ExportPush");
    r.fire("TreeRed");
    r.refuses("StopExport");

    let mut r = start();
    r.fire("TreeCouldNotRun");
    r.fire("BuildPass");
    r.fire("ExportPush");
    r.fire("ExportPass");
    r.refuses("QualifyPass");
}

/// The conformance boundary begins only after AppKit passes. The old bundle-step ordering is the
/// negative control: it puts slice work before the windowed gate and must not satisfy the order.
#[test]
fn appkit_is_a_synchronous_gate_before_the_modelled_graph() {
    let calls = |calls: &[&str]| {
        calls
            .iter()
            .map(|call| (*call).to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        appkit_precedes_graph(&calls(&[
            "verify appkit",
            "disk-claim 30",
            "reserve",
            "build",
            "verify --tree",
            "stage --dry-run",
            "verify --bundle",
        ])),
        Some(true)
    );
    assert_eq!(
        appkit_precedes_graph(&calls(&[
            "disk-claim 30",
            "reserve",
            "build",
            "verify appkit",
        ])),
        Some(false),
        "negative control: the former step-5 order reached AppKit after slice work"
    );
    assert_eq!(
        appkit_precedes_graph(&calls(&["build", "verify --tree"])),
        None,
        "a standing AppKit record needs no invocation"
    );
}

/// The product survives an executing-test red only when it had already started. The control
/// replays the previous unconditional stop and must lose that product.
#[test]
fn a_test_red_keeps_a_started_build_and_the_model_agrees() {
    for keep in [false, true] {
        let mut w = World::new();
        w.verify_outcomes.insert("test", "FAIL");
        w.verify_half = Some(Half::Tree);
        w.verify_code = 1;
        w.after_tree = true;
        w.keep_build_on_test_red = keep;
        let (code, last) = w.run(bump());
        assert_eq!(code, 1, "{last}");
        let r = replay(&w);
        assert_eq!(r.var("build"), if keep { 2 } else { 5 });
        assert_eq!(r.var("kept_build"), i64::from(keep));
        assert_eq!(r.var("phase"), 6);
        assert_eq!(w.journal().unwrap().artifact.is_some(), keep);
    }
}
