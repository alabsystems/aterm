// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tier-0 of `HarnessUpgradeLimitQueue`
//! (`aterm_spec::derive::harness_upgrade_limit_queue_model`): proved at
//! `Buggy = 0` and caught at `Buggy = 1` on EACH invariant, with no dead action
//! at the committed configuration; ONE KNOB PER DEFECT, each caught on its own
//! by the invariant it breaks; the quiet word (`Look`) checked, state by
//! state, to be exactly the upgrade's own guards negated; the owner's report
//! of 2026-09-27 walked as it ran (the caught negative control) and as it goes
//! now; a limit that lasts for DAYS walked as it goes now — a handful of
//! copies after rests shorter than a day, then one a day — and as d5cc7f01c
//! had it (a flat rest, caught); and NO STRAND as a property of the whole
//! graph — from every reachable state the upgrade asks again BY ITSELF once
//! the limit ends, a full queue after its rest, with nobody's turn needed. Tier-1 is aterm-agent's
//! `harness::upgrade_drive` tests (`upgrade_queued_tests.rs`), over the real
//! readers, `queue_facts`, clock and reducer on every reachable state.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use aterm_spec::derive::{Model, harness_upgrade_limit_queue_model};
use aterm_spec::{interp, verify};

type S = BTreeMap<&'static str, i64>;

fn only(m: &Model, invariant: &str) -> Model {
    let mut m = m.clone();
    m.invariants.retain(|i| i.name == invariant);
    m
}

fn run(m: &Model, actions: &[&str]) -> S {
    let mut s = m.init_state();
    for action in actions {
        assert!(m.fire(action, &mut s), "{action} at {s:?}");
    }
    s
}

/// Every state `m` reaches, invariants unchecked.
fn reachable(m: &Model) -> Vec<S> {
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([m.init_state()]);
    let mut out = Vec::new();
    while let Some(s) = queue.pop_front() {
        if !seen.insert(s.clone()) {
            continue;
        }
        for a in &m.actions {
            let mut next = s.clone();
            if m.fire(a.name, &mut next) {
                queue.push_back(next);
            }
        }
        out.push(s);
    }
    out
}

const INVARIANTS: [&str; 5] = [
    "NeverTwoAsksUntaken",
    "CopiesBounded",
    "CopiesPerDayBounded",
    "NoGiveUpUntaken",
    "NeverStranded",
];

/// The four the owner's report broke as it ran: its build had no queue to
/// rest (it gave up after four notices), so the fifth, the rest's growth, is
/// caught by the flat rest that followed it
/// ([`a_limit_that_lasts_for_days_adds_a_handful_of_copies_then_one_a_day`]).
const REPORT: [&str; 4] = [
    "NeverTwoAsksUntaken",
    "CopiesBounded",
    "NoGiveUpUntaken",
    "NeverStranded",
];

/// The defects the fix and this bound repair, one knob each, and the
/// invariant that catches each alone.
const KNOBS: [(&str, &str); 6] = [
    ("CountRetype", "NoGiveUpUntaken"),
    ("CountRetype", "NeverStranded"),
    ("Unbounded", "CopiesBounded"),
    ("NoBound", "NeverStranded"),
    ("ForGood", "NeverStranded"),
    ("Flat", "CopiesPerDayBounded"),
];

/// The model's `untaken` at its top: a full queue whose rest has grown to a
/// day (`RetypeMax + 1 + Daily`).
const TOP: i64 = 2 + 1 + 4;

/// The upgrade's own steps and clocks: what `Look` is the negation of.
const STEPS: [&str; 5] = ["Announce", "Retype", "GiveUp", "Elapse", "Rests"];

#[test]
fn the_limit_queue_model_proves_and_catches() {
    let model = harness_upgrade_limit_queue_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the model must stay enrolled in the spec-link registry"
    );
    let fired = interp::fired_actions(&model);
    assert_eq!(
        fired.len(),
        model.actions.len(),
        "every action fires at the committed configuration: {fired:?}"
    );
    verify::prove_and_catch_scalar(
        &model,
        "harness upgrade limit queue: no ask behind an untaken notice, its copies bounded, at \
         most Daily of them before a day's rest, no give-up before MaxAsks taken, never quiet \
         while a readable ask is owed",
    );
    // Each invariant is load-bearing on its own: the owner's build breaks
    // every one.
    for invariant in INVARIANTS {
        let alone = only(&model, invariant);
        assert!(
            interp::bmc(&interp::with_buggy(&alone, 0)).is_ok(),
            "{invariant}"
        );
        assert!(
            interp::bmc(&interp::with_buggy(&alone, 1)).is_err(),
            "{invariant} catches the owner's build"
        );
    }
}

/// ONE KNOB PER DEFECT, EACH CAUGHT ON ITS OWN: each knob alone breaks the
/// invariant it names, and the committed configuration breaks none. No knob
/// stacks a second ask behind a queued one: that is the owner's build's alone.
#[test]
fn each_defect_is_caught_on_its_own() {
    let model = harness_upgrade_limit_queue_model();
    for (knob, invariant) in KNOBS {
        let alone = only(&model, invariant);
        assert!(
            interp::bmc(&interp::with_consts(&alone, &[(knob, 0)])).is_ok(),
            "{knob} off"
        );
        let caught = interp::bmc(&interp::with_consts(&alone, &[(knob, 1)]));
        assert!(caught.is_err(), "{knob} is caught by {invariant}");
    }
    let stacked = only(&model, "NeverTwoAsksUntaken");
    for (knob, _) in KNOBS {
        assert!(
            interp::bmc(&interp::with_consts(&stacked, &[(knob, 1)])).is_ok(),
            "{knob} never stacks an ask"
        );
    }
}

/// THE QUIET WORD IS THE UPGRADE'S OWN GUARDS NEGATED: on every reachable
/// state where the limit is over by every word (the account's, the screen's,
/// the transcript's) and the upgrade has asked, `Look` is enabled exactly
/// where none of `Announce`, `Retype`, `GiveUp`, `Elapse` and `Rests` is — at
/// the committed configuration, the owner's build and under every knob;
/// nowhere else is it enabled at all. The owner's `NowAsks` is no step of the
/// upgrade's own, and is not in it.
#[test]
fn the_quiet_word_is_derived_from_the_upgrades_guards() {
    let model = harness_upgrade_limit_queue_model();
    let configs: Vec<(&str, Model)> = std::iter::once(("committed", model.clone()))
        .chain(std::iter::once(("Buggy", interp::with_buggy(&model, 1))))
        .chain(
            KNOBS
                .iter()
                .map(|(knob, _)| (*knob, interp::with_consts(&model, &[(knob, 1)]))),
        )
        .collect();
    for (name, m) in &configs {
        let mut looked = 0;
        for s in reachable(m) {
            let point = s["phase"] > 0
                && s["limited"] == 0
                && s["shown"] == 0
                && s["held"] == 0
                && s["stuck"] == 0;
            let quiet = !STEPS.iter().any(|a| m.action_enabled(a, &s));
            assert_eq!(
                m.action_enabled("Look", &s),
                point && quiet,
                "{name}: {s:?}"
            );
            looked += usize::from(point && quiet);
        }
        assert!(looked > 0, "{name}: the quiet word is said somewhere");
    }
}

/// THE OWNER'S REPORT AS IT RAN (`Buggy = 1`, 0.93.0 and the screen-only gate
/// 0.94.0 shipped): the weekly limit hit during the agent's own work, its
/// banner gone from the screen; four notices half an hour apart, each
/// answered by the limit and each read as asked, its window run from the
/// typing; the give-up; the limit ends and the session goes on, all four
/// reaching the agent at once — one ask it had. Every step is `Buggy`'s, the
/// committed model refuses the very first notice (the limit's row named its
/// reset, and the transcript's word holds until then), and the run breaks
/// the four invariants the report is about (not the fifth: four notices in
/// two hours, then a give-up, is no pile a day). Nor, had nothing said the limit stood, would it
/// type the second: behind a queued notice no step of the upgrade's is
/// enabled until the transcript's word runs out.
#[test]
fn the_owners_report_as_it_ran_is_the_caught_negative_control() {
    let m = harness_upgrade_limit_queue_model();
    let buggy = interp::with_buggy(&m, 1);
    let mut ran = vec!["LimitHits", "Clear", "Announce"];
    for _ in 1..4 {
        ran.extend(["Clear", "Elapse", "Announce"]);
    }
    ran.extend([
        "Clear",
        "Elapse",
        "GiveUp",
        "LimitEnds",
        "BoundPasses",
        "GoesOn",
        "Look",
    ]);
    let mut s = buggy.init_state();
    let mut refused = None;
    let mut broken = BTreeSet::new();
    for (at, action) in ran.iter().enumerate() {
        let prev = s.clone();
        assert!(buggy.fire(action, &mut s), "{action} at {prev:?}");
        if refused.is_none() && interp::admits(&m, &prev, &s).is_none() {
            refused = Some((at, *action));
        }
        broken.extend(
            INVARIANTS
                .into_iter()
                .filter(|invariant| !buggy.check_invariant(invariant, &s)),
        );
    }
    assert_eq!(
        refused,
        Some((2, "Announce")),
        "the limit's row named its reset (Oct 3): not even the first notice goes"
    );
    assert_eq!(
        (s["phase"], s["asks"], s["took"]),
        (2, 4, 1),
        "gave up on four notices, taken as one"
    );
    assert_eq!(
        broken,
        BTreeSet::from(REPORT),
        "four notices waited untaken at once, the fourth stacked behind three"
    );
    assert_eq!(
        (s["stacked"], s["spent"], s["stuck"]),
        (1, 1, 1),
        "and it ends given up before the agent had one readable ask: {s:?}"
    );
    // The first stacked notice: the committed upgrade waits behind the queue
    // while the limit's word holds.
    let queued = run(
        &m,
        &["LimitHits", "Clear", "BoundPasses", "Announce", "Clear"],
    );
    assert_eq!((queued["untaken"], queued["held"]), (1, 1), "{queued:?}");
    for quiet in STEPS {
        assert!(!m.action_enabled(quiet, &queued), "{quiet}: {queued:?}");
    }
}

/// THE REPORT NOW: the first notice typed into the limit (the transcript's
/// word ran out while it still stood) waits in the conversation; nothing is
/// typed behind it and no window runs while its limit holds; once the
/// transcript says that limit is over, it is typed again as the same ask —
/// straight away `RetypeMax` times, every copy the limit answers again — and
/// then the queue is FULL: the upgrade rests (never quiet, never waiting on
/// anyone), and types one copy more; the owner's `Upgrade now` types one
/// more at once. The session goes on: every copy taken at once, one ask, its
/// window opened then; the next asks read as they come; the give-up only
/// after `MaxAsks` the model took. And a limit that ended by the time a copy
/// is typed again: that copy reaches the model, and the round goes on from
/// there.
#[test]
fn the_report_now_waits_types_again_rests_and_types_once_more() {
    let m = harness_upgrade_limit_queue_model();
    let mut s = run(&m, &["LimitHits", "BoundPasses", "Clear", "Announce"]);
    assert_eq!((s["asks"], s["untaken"], s["took"]), (1, 1, 0));
    for quiet in STEPS.iter().chain(&["NowAsks"]) {
        assert!(
            !m.action_enabled(quiet, &s),
            "{quiet} behind the queue: {s:?}"
        );
    }
    for _ in 0..2 {
        for action in ["BoundPasses", "Clear", "Retype"] {
            assert!(m.fire(action, &mut s), "{action}: {s:?}");
        }
    }
    assert_eq!(
        (s["asks"], s["untaken"]),
        (1, 3),
        "the same ask, three copies"
    );
    for action in ["BoundPasses", "Clear"] {
        assert!(m.fire(action, &mut s), "{action}: {s:?}");
    }
    // FULL: only its rest runs, and the owner's word would type one more.
    for step in STEPS {
        assert_eq!(m.action_enabled(step, &s), step == "Rests", "{step}: {s:?}");
    }
    assert!(m.action_enabled("NowAsks", &s), "{s:?}");
    assert!(!m.action_enabled("Look", &s), "resting is no quiet word");
    // Still at the limit at the rest: one copy more, four unread, and no
    // burst — the rest came first.
    let mut still = s.clone();
    for action in ["Rests", "Retype"] {
        assert!(m.fire(action, &mut still), "{action}: {still:?}");
    }
    assert_eq!(
        (
            still["asks"],
            still["untaken"],
            still["held"],
            still["burst"]
        ),
        (1, 4, 1, 0),
        "{still:?}"
    );
    // The owner's word, the limit still standing: one copy more, the same.
    let mut asked = s.clone();
    assert!(m.fire("NowAsks", &mut asked));
    assert_eq!(
        (asked["untaken"], asked["held"], asked["burst"]),
        (4, 1, 0),
        "{asked:?}"
    );
    // The limit ends. The session goes on: every copy taken at once.
    assert!(m.fire("LimitEnds", &mut s));
    let mut on = s.clone();
    assert!(m.fire("GoesOn", &mut on));
    assert_eq!(
        (on["untaken"], on["took"], on["window"], on["rested"]),
        (0, 1, 0, 0)
    );
    // Nobody types: it rests, and the copy it types then is read.
    for action in ["Rests", "Retype"] {
        assert!(m.fire(action, &mut s), "{action}: {s:?}");
    }
    assert_eq!(
        (s["asks"], s["untaken"], s["took"], s["burst"]),
        (1, 0, 1, 0)
    );
    for _ in 1..4 {
        for action in ["Elapse", "Announce"] {
            assert!(m.fire(action, &mut s), "{action}: {s:?}");
        }
    }
    for action in ["Elapse", "GiveUp"] {
        assert!(m.fire(action, &mut s), "{action}: {s:?}");
    }
    assert_eq!((s["phase"], s["asks"], s["took"], s["spent"]), (2, 4, 4, 0));

    // The limit over by the time the copy goes: it is read, one ask taken.
    let mut s = run(&m, &["LimitHits", "BoundPasses", "Clear", "Announce"]);
    for action in ["LimitEnds", "BoundPasses", "Clear", "Retype"] {
        assert!(m.fire(action, &mut s), "{action}: {s:?}");
    }
    assert_eq!((s["asks"], s["untaken"], s["took"]), (1, 0, 1));
    assert!(m.action_enabled("Elapse", &s));

    // At every reachable state a give-up follows `MaxAsks` asks taken, and
    // the copies waiting are one ask's.
    let mut gave_up = 0;
    for s in reachable(&m) {
        if s["phase"] == 2 {
            assert_eq!(s["took"], 4, "{s:?}");
            gave_up += 1;
        }
        assert!(s["untaken"] <= TOP, "{s:?}");
    }
    assert!(gave_up > 0, "the give-up is reached");
}

/// Walk `m` from a notice queued behind a limit that NEVER ENDS and names no
/// reset: the notice typed into it, its copies straight away while the queue
/// has room, then — the queue full — each rest and the copy after it, `n`
/// rests in all, the transcript's word running out after every one. The
/// rest each took (1: shorter than a day, 2: a day), and the state after.
fn days_at_the_limit(m: &Model, n: usize) -> (Vec<i64>, S) {
    let mut s = run(m, &["LimitHits", "BoundPasses", "Clear", "Announce"]);
    for _ in 0..2 {
        for action in ["BoundPasses", "Clear", "Retype"] {
            assert!(m.fire(action, &mut s), "{action}: {s:?}");
        }
    }
    let mut rests = Vec::new();
    for _ in 0..n {
        for action in ["BoundPasses", "Clear", "Rests"] {
            assert!(m.fire(action, &mut s), "{action}: {s:?}");
        }
        rests.push(s["rested"]);
        assert!(m.fire("Retype", &mut s), "Retype: {s:?}");
    }
    (rests, s)
}

/// A LIMIT THAT LASTS FOR DAYS ADDS A HANDFUL OF COPIES, THEN ONE A DAY (the
/// owner, 2026-09-27: a usage limit whose row names no reset still added one
/// copy of the notice per rest, every two and a half hours for as long as it
/// stood). Walked: the notice, its two copies straight away, and then ten
/// rests of a full queue, the limit never ending. The first `Daily` rests are
/// shorter than a day (the real two, four, eight and sixteen hours), every
/// later one a day; the copies after a short rest stop at `Daily`, and none
/// waits unread in a burst. The session then goes on: every copy is taken at
/// once, and the next limit's queue starts from a short rest again — the
/// rest grows with copies UNREAD, not for good. NEGATIVE CONTROL: with the
/// flat rest (`Flat`, d5cc7f01c) every rest is short, and the fifth copy
/// after one breaks `CopiesPerDayBounded`; so does `Buggy`, which takes that
/// rest.
#[test]
fn a_limit_that_lasts_for_days_adds_a_handful_of_copies_then_one_a_day() {
    let m = harness_upgrade_limit_queue_model();
    let (rests, s) = days_at_the_limit(&m, 10);
    assert_eq!(rests, [1, 1, 1, 1, 2, 2, 2, 2, 2, 2], "{s:?}");
    assert_eq!(
        (s["untaken"], s["hurried"], s["burst"], s["asks"], s["took"]),
        (TOP, 4, 0, 1, 0),
        "{s:?}"
    );
    for invariant in INVARIANTS {
        assert!(m.check_invariant(invariant, &s), "{invariant}: {s:?}");
    }
    // The session goes on: all taken at once, and the next queue starts
    // over — its rests short again.
    let mut s = s;
    for action in ["LimitEnds", "GoesOn"] {
        assert!(m.fire(action, &mut s), "{action}: {s:?}");
    }
    assert_eq!((s["untaken"], s["hurried"], s["took"]), (0, 0, 1));
    // Over the whole graph: every rest is a day once `Daily` copies went
    // past the queue's room, and shorter before.
    for s in reachable(&m) {
        if s["rested"] > 0 {
            assert_eq!(s["rested"] == 2, s["untaken"] == TOP, "{s:?}");
        }
        assert!(s["hurried"] <= 4, "{s:?}");
    }
    // NEGATIVE CONTROLS: the flat rest, and the owner's build that takes it.
    let flat = interp::with_consts(&m, &[("Flat", 1)]);
    let (rests, s) = days_at_the_limit(&flat, 5);
    assert_eq!(rests, [1, 1, 1, 1, 1], "every rest the first one's");
    assert!(!flat.check_invariant("CopiesPerDayBounded", &s), "{s:?}");
    for (name, m) in [("Flat", flat), ("Buggy", interp::with_buggy(&m, 1))] {
        let alone = only(&m, "CopiesPerDayBounded");
        assert!(interp::bmc(&alone).is_err(), "{name} is caught");
    }
}

/// The upgrade's own actions and the passing of time — no new limit,
/// nobody else's turn, and no word of the owner's.
const BY_ITSELF: [&str; 8] = [
    "Announce",
    "Retype",
    "GiveUp",
    "Elapse",
    "Rests",
    "BoundPasses",
    "Clear",
    "Look",
];

/// Whether, from `s`, moving only by `allowed`, a state is reached where the
/// upgrade types a notice or gives up.
fn asks_again(m: &Model, s: &S, allowed: &[&str]) -> bool {
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([s.clone()]);
    while let Some(s) = queue.pop_front() {
        if !seen.insert(s.clone()) {
            continue;
        }
        if ["Announce", "Retype", "GiveUp"]
            .iter()
            .any(|a| m.action_enabled(a, &s))
        {
            return true;
        }
        for a in allowed {
            let mut next = s.clone();
            if m.fire(a, &mut next) {
                queue.push_back(next);
            }
        }
    }
    false
}

/// NO STRAND, OVER THE WHOLE GRAPH (the upgrade asks again once the limit
/// ends — a property of paths, which `NeverStranded` states at each quiet
/// word and this checks over them): from every reachable state of an announced
/// upgrade whose limit has ended, the upgrade types a notice or gives up BY
/// ITSELF — its own steps, its rest, the transcript's word running out, the
/// banner leaving the screen — full queue or not: no path waits on someone
/// else's turn (`GoesOn`) or the owner's word (`NowAsks`). A round that gave
/// up asks no more in this model (its rest and new round are
/// `HarnessUpgradeNeverStrands`'), and gives up only after `MaxAsks` asks
/// taken. `NoBound` fails it (a queued notice held until a person types), and
/// so does `ForGood` (a full queue held until the session goes on — the review
/// of 2026-09-27).
#[test]
fn once_the_limit_ends_the_upgrade_asks_again_by_itself() {
    let model = harness_upgrade_limit_queue_model();
    let strands = |m: &Model| {
        let mut stranded = 0;
        let mut checked = 0;
        let mut full = 0;
        for s in reachable(m) {
            if s["phase"] != 1 || s["limited"] != 0 {
                continue;
            }
            checked += 1;
            full += usize::from(s["untaken"] > 2);
            if !asks_again(m, &s, &BY_ITSELF) {
                stranded += 1;
            }
        }
        assert!(checked > 0, "{checked} states");
        (stranded, full)
    };
    let (stranded, full) = strands(&model);
    assert_eq!(stranded, 0, "the committed upgrade never strands");
    assert!(full > 0, "full queues are among the states checked");
    for knob in ["NoBound", "ForGood"] {
        assert!(
            strands(&interp::with_consts(&model, &[(knob, 1)])).0 > 0,
            "{knob}: a queue held for good strands an idle session"
        );
    }
}
