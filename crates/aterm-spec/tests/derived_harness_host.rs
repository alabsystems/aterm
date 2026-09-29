// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{
    derive::{
        harness_handoff_claim_model, harness_leave_model, harness_upgrade_look_model,
        harness_worker_lifecycle_model,
    },
    interp, verify,
};

#[test]
fn one_supervisor_per_session_across_restarts_and_reloads() {
    let model = harness_worker_lifecycle_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the harness worker lifecycle must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "harness worker lifecycle");

    // A reload while a worker parks in its wait: the shipped host starts the
    // next worker only once the old one has ended.
    let schedule = ["Arrive", "Start", "Reload"];
    let mut state = model.init_state();
    for action in schedule {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    assert!(!model.action_enabled("Start", &state), "{state:?}");
    assert!(model.fire("Exit", &mut state));
    assert!(model.fire("Start", &mut state));

    // A host that did not wait: two supervisors on one session.
    let eager = interp::with_buggy(&model, 1);
    let mut two = eager.init_state();
    for action in ["Arrive", "Start", "Reload", "Start"] {
        assert!(eager.fire(action, &mut two), "{action}: {two:?}");
    }
    assert!(!eager.check_invariant("OneSupervisor", &two));

    // The budget: Budget failures restart in place unbadged; the next one
    // is badged — and STILL restarted (the philosophy review of 2026-09-25:
    // it was turned off until the policy changed), the badge going as the
    // restart runs.
    let mut s = model.init_state();
    assert!(model.fire("Arrive", &mut s) && model.fire("Start", &mut s));
    for _ in 0..model.consts.iter().find(|c| c.0 == "Budget").unwrap().1 {
        assert!(model.fire("Fail", &mut s));
        assert_eq!((s["cur"], s["faulted"]), (1, 0), "{s:?}");
    }
    assert!(model.fire("Fail", &mut s));
    assert_eq!((s["cur"], s["faulted"]), (1, 1), "{s:?}");
    let mut restarted = s.clone();
    assert!(model.fire("Restart", &mut restarted));
    assert_eq!((restarted["cur"], restarted["faulted"]), (1, 0));

    // The budget is the SESSION's: the program leaving and coming back
    // clears the badge but not the count, so the next failure is badged at
    // once (a flap gives a crash-looping engine no fresh budget)…
    let mut flap = s.clone();
    for action in ["Leave", "Exit", "Arrive", "Start", "Fail"] {
        assert!(model.fire(action, &mut flap), "{action}: {flap:?}");
    }
    assert_eq!((flap["cur"], flap["faulted"]), (1, 1), "{flap:?}");
    // …the window ageing a failure out gives one unbadged run back…
    let mut aged = s.clone();
    for action in ["Leave", "Exit", "Age", "Age", "Arrive", "Start", "Fail"] {
        assert!(model.fire(action, &mut aged), "{action}: {aged:?}");
    }
    assert_eq!((aged["cur"], aged["faulted"]), (1, 0), "{aged:?}");
    // …and a changed policy forgives it all.
    for action in ["Reload", "Exit", "Start"] {
        assert!(model.fire(action, &mut s), "{action}: {s:?}");
    }
    assert_eq!((s["faults"], s["faulted"]), (0, 0), "{s:?}");

    // A host that gave up past the budget: badged with nobody supervising.
    let quits = interp::with_buggy(&model, 1);
    let mut q = quits.init_state();
    assert!(quits.fire("Arrive", &mut q) && quits.fire("Start", &mut q));
    for _ in 0..=quits.consts.iter().find(|c| c.0 == "Budget").unwrap().1 {
        assert!(quits.fire("Fail", &mut q), "{q:?}");
    }
    assert_eq!((q["cur"], q["faulted"]), (0, 1), "{q:?}");
    assert!(!quits.check_invariant("NeverGivesUp", &q));
}

/// THE UPGRADE'S LOOKS ARE NEVER LET GO FOR WHAT A LOOK COULD NOT READ, AND
/// A FRESH LAUNCH IS BEHIND FROM ITS ATTACH (the live re-test of 155c72a28,
/// 2026-09-26): a look refused by the instance's socket — or before Claude
/// Code's record is written — owes a later look, so an upgrade due, a READY
/// answer's restart included, always has one coming; only a look that reads
/// nothing to take lets it go. The note behind owed from the attach is asked
/// until the record reads, and the state it mints is behind from the attach.
/// NEGATIVE CONTROLS: the host of 155c72a28 strands a READY'd session after
/// a refused look, and — its note given up at the attach — mints the state
/// at the first step, its age from then.
#[test]
fn an_upgrade_is_never_let_go_for_a_look_that_could_not_read() {
    let model = harness_upgrade_look_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the upgrade's look machine must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "harness upgrade look");

    // A fresh launch: the note unread at the attach, asked again once the
    // record is written, the state behind from the attach — then the first
    // look steps it.
    let mut f = model.init_state();
    for action in ["NoteUnread", "Regain", "Note", "Look"] {
        assert!(model.fire(action, &mut f), "{action}: {f:?}");
    }
    assert_eq!((f["noted"], f["aged"], f["ready"]), (1, 1, 1), "{f:?}");
    // READY, then a refused look: looked at again, and restarted once read.
    for action in ["Lose", "Unread", "Unread", "Regain", "Look"] {
        assert!(model.fire(action, &mut f), "{action}: {f:?}");
    }
    assert_eq!((f["done"], f["looks"]), (1, 0), "{f:?}");
    // Past the host's asks, the first idle point asks the note before its step.
    let mut i = model.init_state();
    for action in ["Regain", "Look"] {
        assert!(model.fire(action, &mut i), "{action}: {i:?}");
    }
    assert_eq!((i["owed"], i["noted"], i["aged"]), (0, 1, 1), "{i:?}");
    // A look that reads nothing to take lets it go.
    let mut n = model.init_state();
    for action in ["Regain", "Settle", "NotDue"] {
        assert!(model.fire(action, &mut n), "{action}: {n:?}");
    }
    assert_eq!((n["looks"], n["owed"], n["noted"]), (0, 0, 0), "{n:?}");

    // The host of 155c72a28: the READY'd session let go by a refused look…
    let buggy = interp::with_buggy(&model, 1);
    let mut b = buggy.init_state();
    for action in ["Regain", "Look", "Lose", "Unread"] {
        assert!(buggy.fire(action, &mut b), "{action}: {b:?}");
    }
    assert_eq!((b["ready"], b["looks"]), (1, 0), "{b:?}");
    assert!(!buggy.check_invariant("NeverDropped", &b));
    // …and the note given up at the attach: behind from the first step.
    let mut g = buggy.init_state();
    for action in ["NoteUnread", "Regain", "Look"] {
        assert!(buggy.fire(action, &mut g), "{action}: {g:?}");
    }
    assert_eq!((g["noted"], g["aged"]), (1, 0), "{g:?}");
    assert!(!buggy.check_invariant("BehindFromTheAttach", &g));
}

/// ONE SESSION'S SUPERVISOR CLAIM ACROSS A SEAMLESS UPDATE (the round-four
/// plan, item 9): the incoming instance's own host never takes a session a
/// live external supervisor held in the outgoing one — a lease carried and a
/// grace for what the record cannot vouch for — never parks behind its
/// predecessor's own lease, and never holds off a session the record vouched
/// for. NEGATIVE CONTROLS: the three dead Commits, each caught by its claim.
#[test]
fn an_external_supervisor_keeps_its_session_across_the_handoff() {
    let model = harness_handoff_claim_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the handoff claim machine must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "harness handoff claim");
    let konst = |name: &str| model.consts.iter().find(|c| c.0 == name).unwrap().1;
    assert!(
        konst("Grace") > konst("Renew"),
        "the grace outlasts the renewal it waits for"
    );

    // An external lease, carried: the host is refused, the holder renews.
    let mut lease = model.init_state();
    for action in ["ExternalLease", "Park", "Commit"] {
        assert!(model.fire(action, &mut lease), "{action}: {lease:?}");
    }
    assert_eq!((lease["server"], lease["grace"]), (1, konst("Grace")));
    assert!(!model.action_enabled("HostClaims", &lease), "{lease:?}");
    // Even if the carried lease lapses before the renewal, the grace holds.
    assert!(model.fire("Lapse", &mut lease));
    assert!(!model.action_enabled("HostClaims", &lease), "{lease:?}");
    assert!(model.fire("ExternalRenews", &mut lease));
    assert_eq!(lease["server"], 1, "the holder has it again");

    // An older producer says nothing: nothing seeded, the grace waited, and
    // time cannot outrun the renewal it owes.
    let mut older = model.init_state();
    for action in [
        "ExternalConn",
        "OlderParent",
        "Park",
        "Commit",
        "Tick",
        "Tick",
    ] {
        assert!(model.fire(action, &mut older), "{action}: {older:?}");
    }
    assert_eq!((older["server"], older["wait"]), (0, konst("Renew")));
    assert!(
        !model.action_enabled("Tick", &older),
        "the renewal is owed now"
    );
    assert!(!model.action_enabled("HostClaims", &older), "{older:?}");
    assert!(model.fire("ExternalRenews", &mut older));

    // The outgoing host's own lease is left behind: claimed at the Commit.
    let mut own = model.init_state();
    for action in ["HostHeld", "Park", "Commit", "HostClaims"] {
        assert!(model.fire(action, &mut own), "{action}: {own:?}");
    }
    assert_eq!((own["server"], own["took"]), (2, 0));

    // What shipped: nothing carried, no grace — the host claims first.
    let buggy = interp::with_buggy(&model, 1);
    let mut shipped = buggy.init_state();
    for action in ["ExternalLease", "Park", "CommitUncarried", "HostClaims"] {
        assert!(buggy.fire(action, &mut shipped), "{action}: {shipped:?}");
    }
    assert!(!buggy.check_invariant("NoTakeover", &shipped));
    // Carrying our own lease: a claim nobody external made.
    let mut phantom = buggy.init_state();
    for action in ["HostHeld", "Park", "CommitOwnLease"] {
        assert!(buggy.fire(action, &mut phantom), "{action}: {phantom:?}");
    }
    assert!(!buggy.check_invariant("NoPhantomClaim", &phantom));
    // A grace on everything: a vouched session held off.
    let mut needless = buggy.init_state();
    for action in ["Park", "CommitGraceAll"] {
        assert!(buggy.fire(action, &mut needless), "{action}: {needless:?}");
    }
    assert!(!buggy.check_invariant("NoNeedlessGrace", &needless));
    // None of the three exists in the shipped machine.
    let mut parked = model.init_state();
    assert!(model.fire("Park", &mut parked));
    for mutant in ["CommitUncarried", "CommitOwnLease", "CommitGraceAll"] {
        assert!(!model.action_enabled(mutant, &parked), "{mutant}");
    }
}

/// THE HOST HANDS A WORKER ITS AGENT'S EXIT ONLY FOR AN EXIT (2026-09-28): a
/// roster pass that cannot name an agent which still holds its tab keeps its
/// worker, and looks again — an exit during the misread that nothing rings
/// for (the model's conservative case) is handed by that look; the upgrade's
/// own restart, its relaunch not landed at
/// the step's end, is carried on under `[harness] relaunch = false` — and
/// nothing else is: an exit in a tab with another process's restart in
/// flight is the agent's own, and said. NEGATIVE CONTROLS: the host of
/// 32a51a716 hands a misread as an exit, a keep without a look never sees the
/// exit that follows, the worker of 32a51a716 drops the upgrade's own restart
/// for `relaunch = false` (the gate failure of 32a51a716), and a worker that
/// reads the tab's record, not the agent's, carries on a stranger's exit.
#[test]
fn an_exit_is_handed_only_for_an_exit_and_the_upgrades_own_is_carried_on() {
    let model = harness_leave_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the harness leave machine must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "harness leave");

    // A misread: kept, looked at again while it lasts, named again, and
    // nothing handed.
    let mut s = model.init_state();
    for action in ["Misread", "Visit", "Visit", "Reread", "Visit"] {
        assert!(model.fire(action, &mut s), "{action}: {s:?}");
    }
    assert_eq!((s["handed"], s["due"], s["decided"]), (0, 0, 0), "{s:?}");
    // An exit during a misread that nothing rings for: the look the pass set
    // hands it, and the agent's own exit is left and said.
    let mut q = model.init_state();
    for action in ["Misread", "Visit", "Exit"] {
        assert!(model.fire(action, &mut q), "{action}: {q:?}");
    }
    assert_eq!(q["due"], 1, "the look, not a bell: {q:?}");
    for action in ["Visit", "Decide"] {
        assert!(model.fire(action, &mut q), "{action}: {q:?}");
    }
    assert_eq!((q["handed"], q["decided"]), (1, 2), "{q:?}");
    // The upgrade's own restart, its relaunch waiting: carried on.
    let mut u = model.init_state();
    for action in ["Terminate", "Visit", "Decide"] {
        assert!(model.fire(action, &mut u), "{action}: {u:?}");
    }
    assert_eq!((u["handed"], u["decided"]), (1, 1), "{u:?}");
    // A stray record in the tab, then the agent's own exit: said.
    let mut t = model.init_state();
    for action in ["Stray", "Exit", "Visit", "Decide"] {
        assert!(model.fire(action, &mut t), "{action}: {t:?}");
    }
    assert_eq!((t["handed"], t["decided"]), (1, 2), "{t:?}");

    let buggy = interp::with_buggy(&model, 1);
    // The host of 32a51a716: a pass that cannot name the agent hands its exit.
    let mut b = buggy.init_state();
    for action in ["Misread", "VisitUnconfirmed"] {
        assert!(buggy.fire(action, &mut b), "{action}: {b:?}");
    }
    assert!(!buggy.check_invariant("NoExitWhileAlive", &b), "{b:?}");
    // A keep with no look: the exit that follows is never seen.
    let mut n = buggy.init_state();
    for action in ["Misread", "VisitNoLook", "Exit"] {
        assert!(buggy.fire(action, &mut n), "{action}: {n:?}");
    }
    assert!(!buggy.action_enabled("Visit", &n), "{n:?}");
    assert!(!buggy.check_invariant("NeverMissed", &n), "{n:?}");
    // The worker of 32a51a716: the upgrade's restart dropped for the limit.
    let mut d = buggy.init_state();
    for action in ["Terminate", "Visit", "DecideLimited"] {
        assert!(buggy.fire(action, &mut d), "{action}: {d:?}");
    }
    assert!(
        !buggy.check_invariant("TheUpgradesOwnIsCarried", &d),
        "{d:?}"
    );
    // The tab's record read for the agent's: a stranger's exit carried on.
    let mut r = buggy.init_state();
    for action in ["Stray", "Exit", "Visit", "DecideByTab"] {
        assert!(buggy.fire(action, &mut r), "{action}: {r:?}");
    }
    assert!(
        !buggy.check_invariant("OnlyTheUpgradesOwnIsCarried", &r),
        "{r:?}"
    );
}
