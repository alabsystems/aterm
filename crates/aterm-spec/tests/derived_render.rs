// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for `ResizeRender`: the engine's resize undo and the resize
//! ledger's render verdict against the ground truth, proved at `Buggy = 0` and
//! caught at `Buggy = 1` (the historical append-only grow and the
//! size-difference judge), with the incident's schedule replayed on both; a
//! tick on a still-changed size replayed on `Buggy = 2` (the assumed repaint
//! the ledger first landed with, never in a release); a quiet flap only time
//! split replayed on `Buggy = 3` (time closing every run: neither the late
//! run nor the credit); a split flap's risk downgraded to `unverified` on
//! `Buggy = 4`; and the flap a spinner tick splits replayed on `Buggy = 5` (a
//! draw closing every run, main's grouping before the integration). Each
//! mutant breaks exactly its claims over the whole bounded space.

use aterm_spec::{derive::Model, derive::resize_render_model, interp, verify};

/// `model` with only the invariant `name` kept: the interpreter's whole-space
/// check of that one claim.
fn only(model: &Model, name: &str) -> Model {
    let mut one = model.clone();
    one.invariants.retain(|inv| inv.name == name);
    assert_eq!(one.invariants.len(), 1, "{name} is an invariant");
    one
}

/// The mutant `buggy` breaks every one of `claims` somewhere in the bounded
/// space and every other invariant nowhere.
fn breaks_exactly(buggy: i64, claims: &[&str]) {
    let model = resize_render_model();
    let mutant = interp::with_buggy(&model, buggy);
    for inv in &model.invariants {
        let verdict = interp::bmc(&only(&mutant, inv.name));
        if claims.contains(&inv.name) {
            assert!(verdict.is_err(), "Buggy = {buggy} must break {}", inv.name);
        } else {
            assert!(
                verdict.is_ok(),
                "Buggy = {buggy} must break {claims:?} ONLY, but breaks {}: {verdict:?}",
                inv.name
            );
        }
    }
}

/// The 2026-09-28 schedule: a 64→63→64 flap lands before the app's SIGWINCH
/// handler runs, the handler reads an unchanged size, and its diff frame lands
/// on whatever the flap left.
const FLAP: [&str; 4] = ["Demote", "Grow", "Step", "Evaluate"];

#[test]
fn a_quiet_flap_is_an_identity_and_the_historical_engine_is_caught() {
    let model = resize_render_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the resize-render model must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "resize render");

    // The fixed engine: the grow hands the demoted row back, the app's diff
    // frame lands on exactly what it painted, and there is nothing to flag.
    let mut state = model.init_state();
    for action in FLAP {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
        assert!(
            model.check_invariant("QuietFlapIsIdentity", &state),
            "{state:?}"
        );
    }
    assert_eq!(state["geom"], state["app_geom"], "the sizes agree again");
    assert_eq!(state["disp"], 0, "the content is where the app painted it");
    assert_eq!((state["drew"], state["verdict"]), (0, 0));

    // The halves split by TIME alone, nothing drawn: the undo still holds,
    // the grow still hands the row back, and the shrink's run, displaced and
    // undrawn, waited for it (`late`) — one net-zero run that moved nothing,
    // so the reader answers ok, not displaced.
    let mut split = model.init_state();
    for action in ["Demote", "Evaluate"] {
        assert!(model.fire(action, &mut split), "{action}: {split:?}");
    }
    assert_eq!(
        (split["ro"], split["late"], split["st"], split["shown"]),
        (1, 1, 1, 1),
        "displaced meanwhile, and still open"
    );
    for action in ["Grow", "Evaluate"] {
        assert!(model.fire(action, &mut split), "{action}: {split:?}");
    }
    assert_eq!(
        (
            split["disp"],
            split["blind"],
            split["verdict"],
            split["shown"]
        ),
        (0, 0, 0, 0),
        "restored: render=ok"
    );
    for action in ["Step", "Evaluate"] {
        assert!(model.fire(action, &mut split), "{action}: {split:?}");
    }
    assert_eq!((split["disp"], split["drew"], split["verdict"]), (0, 0, 0));

    // The historical engine (append-only grow) and judge on the same
    // schedule: the flap leaves the content one row up, the app draws on it,
    // and the size-difference judge sees agreeing sizes and answers ok.
    let historical = interp::with_buggy(&model, 1);
    let mut missed = historical.init_state();
    for action in ["Demote", "Grow"] {
        assert!(historical.fire(action, &mut missed), "{action}: {missed:?}");
    }
    assert!(
        !historical.check_invariant("QuietFlapIsIdentity", &missed),
        "the append-only grow shifts a quiet flap: {missed:?}"
    );
    for action in ["Step", "Evaluate"] {
        assert!(historical.fire(action, &mut missed), "{action}: {missed:?}");
    }
    assert_eq!((missed["disp"], missed["drew"]), (1, 1));
    assert_eq!(missed["verdict"], 0);
    assert!(!historical.check_invariant("RiskIsFlagged", &missed));
    assert!(!historical.check_invariant("NoSilentPass", &missed));
}

/// A net-zero run that DID displace rows is still judged: output that draws
/// nothing (the first bytes of a SIGWINCH handler: a mode set, a query) lands
/// between the halves, drops the engine's undo but leaves the run open, and
/// the grow can only append. The ledger calls a draw on it a desync risk, and
/// a changed-size repaint heals it.
#[test]
fn a_displaced_net_zero_run_is_still_a_risk_until_repainted() {
    let model = resize_render_model();
    let mut risk = model.init_state();
    for action in ["Demote", "Output", "Grow"] {
        assert!(model.fire(action, &mut risk), "{action}: {risk:?}");
        assert!(model.check_invariant("QuietFlapIsIdentity", &risk));
    }
    assert_eq!((risk["disp"], risk["out"], risk["bot"]), (1, 1, 1));
    assert!(
        !model.action_enabled("Demote", &risk),
        "the appended blank is trimmed, not demoted"
    );
    for action in ["Step", "Evaluate"] {
        assert!(model.fire(action, &mut risk), "{action}: {risk:?}");
    }
    assert_eq!(
        (risk["disp"], risk["drew"], risk["verdict"]),
        (1, 1, 1),
        "drawn on after a displaced net-zero run"
    );
    for action in ["Trim", "Step", "Evaluate"] {
        assert!(model.fire(action, &mut risk), "{action}: {risk:?}");
    }
    assert_eq!(
        (risk["disp"], risk["verdict"]),
        (0, 0),
        "the changed-size repaint healed it"
    );
}

#[test]
fn a_trim_is_no_risk_and_a_changed_size_repaint_heals() {
    let model = resize_render_model();

    // A blank tail absorbs the shrink: nothing moved, nothing to flag. The
    // size-difference judge flags it while the app has not yet read the size.
    let mut trimmed = model.init_state();
    for action in ["Trim", "Evaluate"] {
        assert!(model.fire(action, &mut trimmed), "{action}: {trimmed:?}");
    }
    assert_eq!(trimmed["verdict"], 0);
    let judge = interp::with_buggy(&model, 1);
    let mut alarmed = judge.init_state();
    for action in ["Trim", "Evaluate"] {
        assert!(judge.fire(action, &mut alarmed), "{action}: {alarmed:?}");
    }
    assert_eq!(alarmed["verdict"], 1);
    assert!(!judge.check_invariant("NoFalseVerdict", &alarmed));

    // The healthy flap: the handler runs between the halves, reads the changed
    // size and repaints from scratch, which heals the displacement; the grow
    // that follows appends a blank row, the handler repaints again.
    let mut healthy = model.init_state();
    for action in ["Demote", "Step", "Grow", "Step", "Draw", "Evaluate"] {
        assert!(model.fire(action, &mut healthy), "{action}: {healthy:?}");
    }
    assert_eq!(healthy["disp"], 0);
    assert_eq!(healthy["led"], 0);
    assert_eq!(healthy["verdict"], 0);

    // The quiet flap and a spinner tick after it: the grow handed the row
    // back, so the tick lands on what the app painted.
    let mut quiet = model.init_state();
    for action in ["Demote", "Grow", "Draw", "Evaluate"] {
        assert!(model.fire(action, &mut quiet), "{action}: {quiet:?}");
    }
    assert_eq!((quiet["disp"], quiet["drew"], quiet["verdict"]), (0, 0, 0));
}

/// A shrink whose geometry STAYS changed is the app's to repaint (its handler
/// reads the change), so the ledger never calls it a risk — but a draw on the
/// displaced rows before a heal is `unverified` (2), never ok: a spinner tick
/// before the handler ran looks like the handler's non-clearing repaint to
/// every counter the ledger has. The reader that came by while it was short
/// is the ground-truth blind spot that excuses it from `RiskIsFlagged`.
#[test]
fn a_net_changed_run_is_never_a_risk_and_never_ok_once_drawn_on() {
    let model = resize_render_model();
    let mut tick = model.init_state();
    for action in ["Demote", "Draw", "Evaluate"] {
        assert!(model.fire(action, &mut tick), "{action}: {tick:?}");
    }
    assert_eq!(
        (
            tick["disp"],
            tick["drew"],
            tick["blind"],
            tick["br"],
            tick["verdict"]
        ),
        (1, 1, 1, 1, 2),
        "drawn on, not a risk: unverified"
    );
    assert!(model.check_invariant("RiskIsFlagged", &tick));
    assert!(model.check_invariant("NoSilentPass", &tick));

    // The handler then reads the change and repaints: nothing was missed.
    for action in ["Step", "Evaluate"] {
        assert!(model.fire(action, &mut tick), "{action}: {tick:?}");
    }
    assert_eq!(
        (tick["disp"], tick["blind"], tick["br"], tick["verdict"]),
        (0, 0, 0, 0)
    );

    // A shrink nobody has drawn on yet is not a verdict: nothing to vouch for.
    // It stays open past the gap (`late`), displaced, whether or not output
    // that draws nothing dropped the undo meanwhile.
    let mut quiet = model.init_state();
    for action in ["Demote", "Evaluate", "Output", "Evaluate"] {
        assert!(model.fire(action, &mut quiet), "{action}: {quiet:?}");
    }
    assert_eq!(
        (
            quiet["ro"],
            quiet["late"],
            quiet["st"],
            quiet["drew"],
            quiet["verdict"],
            quiet["shown"]
        ),
        (1, 1, 0, 0, 0, 1)
    );
}

/// THE ASSUMED REPAINT (`Buggy = 2`, the ledger as it first landed, never in a
/// release): the same tick on a still-changed size. The ledger says
/// `unverified`; the judge that took the draw as a repaint says ok over a
/// screen one row up and drawn on, and breaks `NoSilentPass` — alone.
#[test]
fn a_tick_on_a_changed_size_is_unverified_where_the_assumed_repaint_said_ok() {
    let model = resize_render_model();
    let schedule = ["Demote", "Draw", "Evaluate"];
    let assumed = interp::with_buggy(&model, 2);
    let mut ok = assumed.init_state();
    for action in schedule {
        assert!(assumed.fire(action, &mut ok), "{action}: {ok:?}");
    }
    assert_eq!((ok["disp"], ok["drew"], ok["verdict"]), (1, 1, 0));
    assert!(!assumed.check_invariant("NoSilentPass", &ok));
    assert!(
        assumed.check_invariant("RiskIsFlagged", &ok),
        "the reader saw it short: only the new claim catches it"
    );
}

/// THE SPLIT FLAP (2026-09-28, live on main with the stand-in ticking): the
/// presence row's attention set and unset 2-15 ms apart, a spinner tick
/// between the halves. The tick is output, so the engine's undo is gone and
/// the grow appends (`appended=1 restored=0`): the top row is lost, the app's
/// handler reads an unchanged size, and its frames land one row off. The
/// ledger keeps the tick inside the run, judges the net-zero run it now is,
/// and flags it; a judge that closes a run on any draw (`Buggy = 5`: main's
/// grouping before the integration) reads two net-changed runs and answers
/// `unverified` at best.
#[test]
fn a_flap_a_spinner_tick_splits_is_judged_and_the_draw_closing_judge_is_caught() {
    const SPLIT: [&str; 5] = ["Demote", "Draw", "Grow", "Step", "Evaluate"];
    let model = resize_render_model();
    let mut split = model.init_state();
    for action in SPLIT {
        assert!(model.fire(action, &mut split), "{action}: {split:?}");
    }
    assert_eq!(
        (
            split["geom"],
            split["app_geom"],
            split["disp"],
            split["drew"]
        ),
        (0, 0, 1, 1),
        "a permanent shift the app drew on"
    );
    assert_eq!(
        (split["blind"], split["verdict"], split["led"]),
        (0, 1, 1),
        "one net-zero run, flagged"
    );

    let old = interp::with_buggy(&model, 5);
    let mut missed = old.init_state();
    for action in SPLIT {
        assert!(old.fire(action, &mut missed), "{action}: {missed:?}");
    }
    assert_eq!(
        (
            missed["disp"],
            missed["drew"],
            missed["blind"],
            missed["verdict"]
        ),
        (1, 1, 0, 2)
    );
    assert!(
        !old.check_invariant("RiskIsFlagged", &missed),
        "a shifted screen the app drew on, not called a risk: {missed:?}"
    );
    assert!(old.check_invariant("NoSilentPass", &missed));

    // A full clear BETWEEN the halves (the handler read the smaller size and
    // repainted from scratch) closes the run: the grow appends a blank under
    // a frame laid out for the smaller size, which the next handler repaints.
    let mut healthy = model.init_state();
    for action in ["Demote", "Step", "Grow", "Draw", "Evaluate"] {
        assert!(model.fire(action, &mut healthy), "{action}: {healthy:?}");
    }
    assert_eq!(
        (healthy["disp"], healthy["verdict"], healthy["shown"]),
        (0, 0, 0)
    );
}

/// The blind spot excuses only the displacement a reader saw: a restored
/// flap a reader came by while it was short, then a tick-split flap, is the
/// tick-split flap alone. With the excuse kept past the grow that brought the
/// displacement back to zero (the first cut of the split-flap model), a judge
/// that missed the second flap passed `RiskIsFlagged` on this schedule.
#[test]
fn an_earlier_restored_flap_excuses_no_later_one() {
    const TWICE: [&str; 8] = [
        "Demote", "Evaluate", "Grow", "Demote", "Draw", "Grow", "Step", "Evaluate",
    ];
    let model = resize_render_model();
    let mut fixed = model.init_state();
    for action in TWICE {
        assert!(model.fire(action, &mut fixed), "{action}: {fixed:?}");
    }
    assert_eq!(
        (
            fixed["disp"],
            fixed["drew"],
            fixed["blind"],
            fixed["verdict"]
        ),
        (1, 1, 0, 1)
    );

    let old = interp::with_buggy(&model, 5);
    let mut missed = old.init_state();
    for (i, action) in TWICE.iter().enumerate() {
        assert!(old.fire(action, &mut missed), "{action}: {missed:?}");
        if i == 1 {
            assert_eq!(missed["blind"], 1, "a reader saw the first flap short");
        }
        if i == 2 {
            assert_eq!(missed["blind"], 0, "the undo put it back: episode over");
        }
    }
    assert_eq!((missed["disp"], missed["verdict"]), (1, 2));
    assert!(!old.check_invariant("RiskIsFlagged", &missed), "{missed:?}");
}

/// A flap the engine's undo RESTORED, its halves further apart than the run
/// gap with nothing output between (the attention held ~600 ms, S6b quiet):
/// an identity, so `render=ok`, and a spinner tick after it reads ok too. A
/// judge that closes a run on time alone (`Buggy = 3`: neither the late run
/// nor the credit) leaves the shrink's half `silent` — `render=displaced` over
/// an intact screen — and, once the app draws, reads `unverified` over a
/// screen that is exactly the app's. And a held run the undo can no longer
/// hand back to (a mode set dropped it) is a flap output split: the grow
/// appends, and the tick is a risk.
#[test]
fn a_restored_flap_split_by_time_is_ok_and_the_time_closing_judge_is_caught() {
    const SLOW: [&str; 4] = ["Demote", "Evaluate", "Grow", "Evaluate"];
    const TICKED: [&str; 5] = ["Demote", "Evaluate", "Grow", "Draw", "Evaluate"];
    let model = resize_render_model();
    let mut slow = model.init_state();
    for action in SLOW {
        assert!(model.fire(action, &mut slow), "{action}: {slow:?}");
    }
    assert_eq!(
        (slow["disp"], slow["shown"], slow["verdict"], slow["ro"]),
        (0, 0, 0, 0)
    );
    let mut ticked = model.init_state();
    for action in TICKED {
        assert!(model.fire(action, &mut ticked), "{action}: {ticked:?}");
    }
    assert_eq!(
        (
            ticked["disp"],
            ticked["drew"],
            ticked["led"],
            ticked["br"],
            ticked["verdict"]
        ),
        (0, 0, 0, 0, 0),
        "the screen is the app's and nothing is owed"
    );
    let mut dropped = model.init_state();
    for action in ["Demote", "Evaluate", "Output", "Grow", "Draw", "Evaluate"] {
        assert!(model.fire(action, &mut dropped), "{action}: {dropped:?}");
    }
    assert_eq!(
        (dropped["disp"], dropped["drew"], dropped["verdict"]),
        (1, 1, 1)
    );

    let old = interp::with_buggy(&model, 3);
    let mut stuck = old.init_state();
    for action in SLOW {
        assert!(old.fire(action, &mut stuck), "{action}: {stuck:?}");
    }
    assert_eq!((stuck["disp"], stuck["shown"]), (0, 1));
    assert!(
        !old.check_invariant("NoFalseDisplaced", &stuck),
        "displaced over an intact screen: {stuck:?}"
    );
    let mut noisy = old.init_state();
    for action in TICKED {
        assert!(old.fire(action, &mut noisy), "{action}: {noisy:?}");
    }
    assert_eq!((noisy["disp"], noisy["verdict"]), (0, 2));
    assert!(!old.check_invariant("NoFalseVerdict", &noisy), "{noisy:?}");
}

/// THE RISK, NOT ONLY A VERDICT (`Buggy = 4`: a net-zero run's risk
/// downgraded to `unverified`). A flap output split (the mode set dropped the
/// undo, the grow appended), the app's handler reads an unchanged size and
/// draws its diff: the screen is one row up and drawn on. `NoSilentPass`
/// takes any verdict; `RiskIsFlagged` demands the risk, and it is the one
/// claim that breaks.
#[test]
fn a_net_zero_risk_is_never_downgraded_to_unverified() {
    let model = resize_render_model();
    let schedule = ["Demote", "Output", "Grow", "Step", "Evaluate"];
    let mut risk = model.init_state();
    for action in schedule {
        assert!(model.fire(action, &mut risk), "{action}: {risk:?}");
    }
    assert_eq!(
        (risk["disp"], risk["drew"], risk["verdict"]),
        (1, 1, 1),
        "desync-risk"
    );

    let downgraded = interp::with_buggy(&model, 4);
    let mut soft = downgraded.init_state();
    for action in schedule {
        assert!(downgraded.fire(action, &mut soft), "{action}: {soft:?}");
    }
    assert_eq!(soft["verdict"], 2, "the downgrade says unverified");
    assert!(!downgraded.check_invariant("RiskIsFlagged", &soft));
    for invariant in ["NoSilentPass", "NoFalseVerdict", "QuietFlapIsIdentity"] {
        assert!(
            downgraded.check_invariant(invariant, &soft),
            "only RiskIsFlagged catches it: {invariant}"
        );
    }
}

/// Every named mutant breaks exactly its claims anywhere in the bounded
/// space: 2 `NoSilentPass`; 3 the verdict both ways and the displaced render
/// both ways (a time-closed run is left `silent`, or booked, whatever the
/// undo later hands back); 4 and 5 `RiskIsFlagged`.
#[test]
fn each_named_mutant_breaks_exactly_its_claims() {
    breaks_exactly(2, &["NoSilentPass"]);
    breaks_exactly(
        3,
        &[
            "RiskIsFlagged",
            "NoFalseVerdict",
            "DisplacedIsShown",
            "NoFalseDisplaced",
        ],
    );
    breaks_exactly(4, &["RiskIsFlagged"]);
    breaks_exactly(5, &["RiskIsFlagged"]);
}

/// Each invariant, alone, is falsified by the `Buggy = 1` family (the
/// workspace ratchet's claim, restated here for the displaced render).
#[test]
fn the_render_invariants_are_each_caught() {
    let model = resize_render_model();
    assert_eq!(
        aterm_spec::verify::uncaught_invariants(&model),
        vec!["Bounded"],
        "only the space guard is uncatchable"
    );
    let judge = interp::with_buggy(&model, 1);
    let mut trimmed = judge.init_state();
    for action in ["Trim", "Evaluate"] {
        assert!(judge.fire(action, &mut trimmed), "{action}");
    }
    assert!(!judge.check_invariant("NoFalseDisplaced", &trimmed));
    let mut quiet = judge.init_state();
    for action in ["Demote", "Grow", "Evaluate"] {
        assert!(judge.fire(action, &mut quiet), "{action}");
    }
    assert!(!judge.check_invariant("DisplacedIsShown", &quiet));
}
