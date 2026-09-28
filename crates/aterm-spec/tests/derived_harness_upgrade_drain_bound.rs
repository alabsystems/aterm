// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_spec::{derive::harness_upgrade_drain_bound_model, interp, verify};

/// The drain never ends the agent on an answer a person held past the bound,
/// never waits in silence on the agent's own work past it, never ends running
/// work, and never supersedes a READY answer before its own bound. Each bug
/// it was written for is caught:
/// - with no void, the answer outlives the hold, and the end comes the moment
///   the person lets go;
/// - with no work bound, a tab behind loops that can never end is told once
///   and then waited on for good;
/// - the wrong fixes for that silence (ending the agent at the bound, and
///   superseding a READY answer on the notice's clock) are caught too.
#[test]
fn the_drain_never_ends_the_agent_on_an_answer_a_person_held_past_its_bound() {
    let model = harness_upgrade_drain_bound_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name)
    );
    verify::prove_and_catch_scalar(&model, "harness upgrade drain bound");

    // Held to the bound, the answer is voided and can never end the agent.
    let mut held = model.init_state();
    assert!(model.fire("PersonHolds", &mut held));
    assert!(model.fire("Wait", &mut held));
    assert!(model.fire("Wait", &mut held));
    assert!(!model.action_enabled("Wait", &held), "the bound is a void");
    assert!(model.fire("Void", &mut held));
    assert!(model.fire("PersonLets", &mut held));
    assert!(!model.action_enabled("Terminate", &held));

    // The same at a break: the void is typed nowhere and ends nothing, and the
    // stale answer can authorize no end at the next idle point.
    let mut held_at_break = model.init_state();
    assert!(model.fire("AtBreak", &mut held_at_break));
    assert!(model.fire("PersonHolds", &mut held_at_break));
    assert!(model.fire("Wait", &mut held_at_break));
    assert!(model.fire("Wait", &mut held_at_break));
    assert!(!model.action_enabled("Wait", &held_at_break));
    assert!(model.fire("Void", &mut held_at_break));

    // The agent's own work is waited for up to the bound and is never voided
    // or ended. Past the bound the upgrade asks again: the new notice
    // supersedes the old answer. Past `MaxAsks` it gives up. It never waits
    // on the work in silence.
    let mut working = model.init_state();
    assert!(model.fire("AgentWorks", &mut working));
    assert!(model.fire("Wait", &mut working));
    assert!(model.fire("Wait", &mut working));
    for step in ["Wait", "Void", "Terminate", "GiveUp"] {
        assert!(!model.action_enabled(step, &working), "{step} at the bound");
    }
    assert!(model.fire("ReAsk", &mut working));
    assert!(model.fire("Wait", &mut working));
    assert!(model.fire("Wait", &mut working));
    // A READY answer given late, after the re-asked notice's own bound: it
    // gets a whole bound of its own before the work it outlives gives up on it.
    assert!(model.fire("Answers", &mut working));
    assert!(!model.action_enabled("GiveUp", &working), "a fresh answer");
    assert!(model.fire("Wait", &mut working));
    assert!(model.fire("Wait", &mut working));
    assert!(!model.action_enabled("ReAsk", &working), "MaxAsks notices");
    assert!(model.fire("GiveUp", &mut working));
    assert!(
        !model.action_enabled("Terminate", &working),
        "a give-up ends nothing"
    );
    for invariant in ["NoSilentWait", "NoHastySupersede", "NeverEndsRunningWork"] {
        assert!(model.check_invariant(invariant, &working), "{invariant}");
    }

    // Work that ends in time: the agent is ended on its answer.
    let mut rests = model.init_state();
    assert!(model.fire("AgentWorks", &mut rests));
    assert!(model.fire("Wait", &mut rests));
    assert!(model.fire("AgentRests", &mut rests));
    assert!(model.fire("Terminate", &mut rests));
    assert!(model.check_invariant("NoEndOnAHeldAnswer", &rests));

    // At a break only a notice goes: never an end.
    let mut at_break = model.init_state();
    assert!(model.fire("AtBreak", &mut at_break));
    assert!(!model.action_enabled("Terminate", &at_break));
    assert!(model.fire("Wait", &mut at_break));
    assert!(model.fire("Wait", &mut at_break));
    assert!(
        !model.action_enabled("Wait", &at_break),
        "the break's bound is a notice"
    );
    assert!(model.fire("ReAsk", &mut at_break));

    // CLAUDE'S OWN STATUS LAGS AN IDLE SCREEN (the review of 2026-09-27):
    // `busy` or `shell` over a screen read idle, nothing under the agent the
    // kernel can see. Work in the agent's own process may still run, and the
    // status is the one word that says so: the agent is never ended on it.
    // Its READY is asked again past the bound, then given up on — never
    // waited on in silence. Once Claude writes `idle`, the restart goes.
    let mut lag = model.init_state();
    assert!(model.fire("StatusLags", &mut lag));
    assert!(
        !model.action_enabled("Terminate", &lag),
        "never on a status that is not idle"
    );
    let mut idles = lag.clone();
    assert!(model.fire("StatusIdle", &mut idles));
    assert!(model.fire("Terminate", &mut idles));
    assert!(model.check_invariant("NeverEndsRunningWork", &idles));
    assert!(model.fire("Wait", &mut lag));
    assert!(model.fire("Wait", &mut lag));
    for step in ["Wait", "Void", "Terminate", "GiveUp"] {
        assert!(!model.action_enabled(step, &lag), "{step} at the bound");
    }
    assert!(model.fire("ReAsk", &mut lag));
    assert!(model.fire("Answers", &mut lag));
    assert!(model.fire("Wait", &mut lag));
    assert!(model.fire("Wait", &mut lag));
    assert!(model.fire("GiveUp", &mut lag));
    for invariant in ["NoSilentWait", "NeverEndsRunningWork"] {
        assert!(model.check_invariant(invariant, &lag), "{invariant}");
    }
    // The defect the review found: the restart took the lagging status for
    // idle (`LagEnds`), and ended the agent over whatever ran in it.
    let lag_ends = interp::with_consts(&model, &[("LagEnds", 1)]);
    let mut cut = lag_ends.init_state();
    assert!(lag_ends.fire("StatusLags", &mut cut));
    assert!(lag_ends.fire("Terminate", &mut cut));
    assert!(!lag_ends.check_invariant("NeverEndsRunningWork", &cut));
    // The tempting wrong fix (`LagHolds`): the restart kept to Claude's
    // `idle`, and the READY behind the lag waited on for good, in silence.
    let lag_holds = interp::with_consts(&model, &[("LagHolds", 1)]);
    let mut silent = lag_holds.init_state();
    assert!(lag_holds.fire("StatusLags", &mut silent));
    assert!(!lag_holds.action_enabled("Terminate", &silent));
    for _ in 0..3 {
        assert!(lag_holds.fire("Wait", &mut silent));
    }
    assert!(!lag_holds.action_enabled("ReAsk", &silent));
    assert!(!lag_holds.check_invariant("NoSilentWait", &silent));

    let buggy = interp::with_buggy(&model, 1);
    let mut stale = buggy.init_state();
    assert!(buggy.fire("PersonHolds", &mut stale));
    for _ in 0..3 {
        assert!(buggy.fire("Wait", &mut stale));
    }
    assert!(!buggy.action_enabled("Void", &stale));
    assert!(buggy.fire("PersonLets", &mut stale));
    assert!(buggy.fire("Terminate", &mut stale));
    assert!(!buggy.check_invariant("NoEndOnAHeldAnswer", &stale));

    // THE FOUR-DAY TAB (2026-09-26), replayed on the drain before the work
    // bound: two poll loops that can never end, a break every minute, one
    // notice and then silence.
    let mut silent = buggy.init_state();
    assert!(buggy.fire("AgentWorks", &mut silent));
    assert!(buggy.fire("AtBreak", &mut silent));
    for _ in 0..3 {
        assert!(buggy.fire("Wait", &mut silent));
    }
    assert!(!buggy.check_invariant("NoSilentWait", &silent));

    // The first tempting wrong fix for that silence: ending the agent at the
    // bound, its work still running. aterm never ends running work.
    let mut cut = buggy.init_state();
    assert!(buggy.fire("AgentWorks", &mut cut));
    assert!(buggy.fire("Wait", &mut cut));
    assert!(buggy.fire("Wait", &mut cut));
    assert!(buggy.fire("Terminate", &mut cut));
    assert!(!buggy.check_invariant("NeverEndsRunningWork", &cut));
    assert!(!model.action_enabled("Terminate", &{
        let mut s = model.init_state();
        assert!(model.fire("AgentWorks", &mut s));
        assert!(model.fire("Wait", &mut s));
        assert!(model.fire("Wait", &mut s));
        s
    }));

    // The second: the notice's clock alone. On the last notice, a READY answer
    // given after the notice's bound is given up on at once.
    let mut hasty = buggy.init_state();
    assert!(buggy.fire("AgentWorks", &mut hasty));
    assert!(buggy.fire("Wait", &mut hasty));
    assert!(buggy.fire("Wait", &mut hasty));
    assert!(buggy.fire("ReAsk", &mut hasty));
    assert!(buggy.fire("Wait", &mut hasty));
    assert!(buggy.fire("Wait", &mut hasty));
    assert!(buggy.fire("Answers", &mut hasty));
    assert!(buggy.fire("GiveUp", &mut hasty));
    assert!(!buggy.check_invariant("NoHastySupersede", &hasty));
}
