// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier 0 of `WakeBook` and `WakeRoom`: each holds at `Buggy = 0` in every configuration it
//! declares, each law breaks under its own mutant alone, each liveness mutant has an invariant
//! twin, and walks follow the paths the design names. Tier 1 is
//! `aterm-agent/tests/conformance_wakes.rs` (the book) and `aterm-gui`'s
//! `occupancy_host::conformance` (the room). The non-vacuity sweep and the liveness audits are
//! `aterm-models`' `non_vacuity`.

use aterm_models_harness::{
    wake_book_liveness, wake_book_model, wake_room_liveness, wake_room_model,
};
use aterm_spec::derive::Model;
use aterm_spec::interp;
use aterm_spec::walk::walk;

/// A configuration: the constants a scenario overrides.
type Config = &'static [(&'static str, i64)];

/// The book's configurations: committed (`prompt`, unmanaged, hosted), then each scenario.
const BOOK_CONFIGS: [Config; 4] = [&[], &[("Prompt", 0)], &[("Managed", 1)], &[("Hosted", 0)]];

/// Each mutant of the book, its dial, the law it was written to break, and the configuration it
/// breaks it under.
const BOOK_MUTANTS: [(&str, i64, &str, Config); 15] = [
    ("ParkDeaf", 1, "NoDeafWaiter", &[]),
    ("DeclareFromOutside", 2, "OnlyItsOwnTree", &[]),
    ("AwaitFromAnyone", 3, "OnlyItsOwnTree", &[]),
    ("TypeTwice", 4, "OneTypedLine", &[]),
    ("AnswerThenType", 5, "NoTellAfterHeard", &[]),
    ("ParkSecond", 6, "OneParkedWait", &[]),
    ("TellOnStaleNeed", 7, "NeedAtTheTell", &[]),
    ("TellExpired", 8, "OnlyReadyIsTold", &[]),
    (
        "TypeIntoManaged",
        9,
        "NoLineIntoAManagedWorker",
        &[("Managed", 1)],
    ),
    (
        "TypeUnderObserve",
        10,
        "NothingTypedUnderObserve",
        &[("Prompt", 0)],
    ),
    ("LevelOnly", 11, "NoMissedEdge", &[]),
    ("ToldBeforeWrite", 12, "NoLostAnswer", &[]),
    ("ReleaseAfterTyped", 13, "OneTypedLine", &[]),
    ("RetellAfterHeard", 14, "NoTellAfterHeard", &[]),
    ("AnswerTwice", 15, "OneAnswer", &[]),
];

/// Each mutant of the room, its dial and the law it was written to break.
const ROOM_MUTANTS: [(&str, i64, &str, Config); 5] = [
    ("RenewWhileWaited", 1, "NoReservationOutlivesItsLapse", &[]),
    ("HerdTell", 2, "OneUnitOneTellWhileReserved", &[]),
    ("YoungestFirst", 3, "TheOlderHoldFirst", &[]),
    ("RenewOnAsk", 4, "AReservationIsTakenOnce", &[]),
    ("CountOwn", 5, "AHolderIsNeverRefused", &[]),
];

/// A bound on the states any configuration of either model reaches, at any dial.
const STATE_BOUND: usize = 50_000;

fn book(config: Config) -> Model {
    interp::with_consts(&wake_book_model(), config)
}

/// `m` with every mutant of `mutants` removed but `keep`.
fn alone(m: &Model, mutants: &[(&str, i64, &str, Config)], keep: &str) -> Model {
    let mut one = m.clone();
    one.actions
        .retain(|a| a.name == keep || mutants.iter().all(|(mutant, ..)| *mutant != a.name));
    one
}

/// No law breaks at `Buggy = 0` in any of `configs`, and the actions that fire across them are
/// exactly those that are no mutant.
fn fires_exactly_its_committed_moves(
    base: &Model,
    configs: &[Config],
    mutants: &[(&str, i64, &str, Config)],
) {
    let mut fired = std::collections::BTreeSet::new();
    for config in configs {
        let m = interp::with_consts(base, config);
        assert!(
            interp::violated_invariants(&m).is_empty(),
            "{}: every law holds under {config:?}",
            base.name
        );
        fired.extend(interp::fired_actions(&m));
    }
    for action in base.actions.iter().map(|a| a.name) {
        let mutant = mutants.iter().any(|(name, ..)| *name == action);
        assert_eq!(
            fired.contains(action),
            !mutant,
            "{}: {action} fires with no mutant on exactly when it is no mutant: {fired:?}",
            base.name
        );
    }
}

/// Each mutant breaks the law it was written to break, alone, and the committed model keeps it;
/// every law has a mutant.
fn each_breaks_its_own_law_alone(base: &Model, mutants: &[(&str, i64, &str, Config)]) {
    for (mutant, dial, law, config) in mutants {
        let m = alone(&interp::with_consts(base, config), mutants, mutant);
        let wrong = interp::with_buggy(&aterm_spec::walk::only(&m, law), *dial);
        assert!(
            interp::violated_invariants(&wrong).contains(law),
            "{mutant} (Buggy = {dial}, {config:?}) breaks {law}"
        );
        let right = interp::with_buggy(&aterm_spec::walk::only(&m, law), 0);
        assert!(
            interp::violated_invariants(&right).is_empty(),
            "the committed {} keeps {law} under {config:?}",
            base.name
        );
    }
    let laws: Vec<&str> = mutants.iter().map(|(_, _, law, _)| *law).collect();
    for inv in &base.invariants {
        assert!(laws.contains(&inv.name), "{} has its own mutant", inv.name);
    }
}

#[test]
fn wake_book_fires_exactly_its_committed_moves() {
    fires_exactly_its_committed_moves(&wake_book_model(), &BOOK_CONFIGS, &BOOK_MUTANTS);
}

#[test]
fn wake_room_fires_exactly_its_committed_moves() {
    fires_exactly_its_committed_moves(&wake_room_model(), &[&[]], &ROOM_MUTANTS);
}

#[test]
fn each_mutant_breaks_its_own_law_alone() {
    each_breaks_its_own_law_alone(&wake_book_model(), &BOOK_MUTANTS);
    each_breaks_its_own_law_alone(&wake_room_model(), &ROOM_MUTANTS);
}

/// Each liveness mutant (dial 1) is additive, dead with no mutant on and firing at its dial, and
/// breaks an invariant twin, so a safety walk catches it too.
#[test]
fn each_liveness_mutant_has_an_invariant_twin() {
    for (model, live, twin) in [
        (wake_book_model(), wake_book_liveness(), "NoDeafWaiter"),
        (
            wake_room_model(),
            wake_room_liveness(),
            "NoReservationOutlivesItsLapse",
        ),
    ] {
        let committed = interp::fired_actions(&interp::with_buggy(&model, 0));
        let dialled = interp::fired_actions(&interp::with_buggy(&model, 1));
        for mutant in &live.mutants {
            assert!(!committed.contains(mutant), "{mutant} is dead at Buggy = 0");
            assert!(dialled.contains(mutant), "{mutant} fires at Buggy = 1");
        }
        assert!(
            interp::violated_invariants(&interp::with_buggy(&model, 1)).contains(twin),
            "{}: dial 1 breaks {twin}",
            model.name
        );
    }
}

#[test]
fn each_model_stays_under_50k_states() {
    let mut counts = Vec::new();
    for (base, configs, mutants) in [
        (wake_book_model(), &BOOK_CONFIGS[..], &BOOK_MUTANTS[..]),
        (wake_room_model(), &[&[][..]][..], &ROOM_MUTANTS[..]),
    ] {
        for config in configs {
            let m = interp::with_consts(&base, config);
            for dial in std::iter::once(0).chain(mutants.iter().map(|(_, dial, ..)| *dial)) {
                let n = interp::reachable_states(&interp::with_buggy(&m, dial)).len();
                assert!(
                    n < STATE_BOUND,
                    "{} under {config:?} at Buggy = {dial}: {n} states",
                    base.name
                );
                counts.push(format!("{} {config:?} Buggy={dial}: {n}", base.name));
            }
        }
    }
    eprintln!("{}", counts.join("\n"));
}

const READY: [&str; 4] = ["DeclareOwn", "Holds", "Edge", "NeedHolds"];

fn then(more: &[&'static str]) -> Vec<&'static str> {
    [&READY[..], more].concat()
}

/// A file made and gone between two looks makes the wake ready though its level reads false;
/// read only by level, the edge is missed.
#[test]
fn an_edge_latches_through_a_false_level() {
    let m = book(&[]);
    let st = walk(&m, &["DeclareOwn", "Blip", "Edge"]);
    assert_eq!((st["st"], st["cond"], st["blip"]), (2, 0, 0));
    let pending = walk(&m, &["DeclareOwn"]);
    assert!(!m.action_enabled("Edge", &pending), "no edge, no level");
    let level = interp::with_buggy(&m, 11);
    let missed = walk(&level, &["DeclareOwn", "LevelOnly"]);
    assert!(!level.action_enabled("Edge", &missed), "{missed:?}");
    assert!(!level.check_invariant("NoMissedEdge", &missed));
}

/// A parked wait on a ready wake whose need holds is answered once, and told once the loop sees
/// a turn run after it; no door claims a told wake. Unhosted, the answer is told at once.
#[test]
fn an_await_is_answered_once_and_heard() {
    let m = book(&[]);
    let answered = walk(&m, &then(&["Park", "Answer", "WriteOk"]));
    assert_eq!(
        (answered["st"], answered["answers"], answered["parked"]),
        (4, 1, 0)
    );
    let heard = walk(
        &m,
        &then(&["Break", "Park", "Answer", "WriteOk", "TurnRuns", "Heard"]),
    );
    assert_eq!((heard["st"], heard["door"]), (6, 1));
    assert!(!m.action_enabled("Claim", &heard), "{heard:?}");
    let twice = interp::with_buggy(&m, 15);
    let wrong = walk(
        &twice,
        &then(&["Park", "Answer", "WriteOk", "AnswerTwice", "WriteOk"]),
    );
    assert!(!twice.check_invariant("OneAnswer", &wrong));
    let stale = walk(&m, &["DeclareOwn", "Holds", "Edge", "Park"]);
    assert!(!m.action_enabled("Answer", &stale), "the need holds first");
    let unhosted = book(&[("Hosted", 0)]);
    let told = walk(&unhosted, &then(&["Park", "Answer", "WriteOk"]));
    assert_eq!(told["st"], 6);
}

/// An unheard answer is retold once by a door under `prompt`; a heard one is not.
#[test]
fn an_unheard_answer_is_retold_once_by_a_door() {
    let m = book(&[]);
    let retold = walk(
        &m,
        &then(&["Park", "Answer", "WriteOk", "Break", "Claim", "Write"]),
    );
    assert_eq!(
        (retold["st"], retold["answers"], retold["typed"]),
        (6, 1, 1)
    );
    let heard = walk(
        &m,
        &then(&["Park", "Answer", "WriteOk", "TurnRuns", "Break"]),
    );
    assert!(!m.action_enabled("Claim", &heard), "{heard:?}");
    let late = interp::with_buggy(&m, 14);
    let mut wrong = heard.clone();
    assert!(late.fire("RetellAfterHeard", &mut wrong));
    assert!(!late.check_invariant("NoTellAfterHeard", &wrong));
    let released = walk(
        &m,
        &then(&["Park", "Answer", "WriteOk", "Break", "Claim", "Release"]),
    );
    assert_eq!(released["st"], 4, "back to answered, not to ready");
}

/// A failed reply leaves the wake ready for the next answer; recorded answered before its write,
/// the answer is lost.
#[test]
fn a_failed_reply_leaves_the_wake_ready() {
    let m = book(&[]);
    let failed = walk(&m, &then(&["Park", "Answer", "WriteFails"]));
    assert_eq!((failed["st"], failed["parked"]), (2, 1));
    assert!(m.action_enabled("Answer", &failed));
    let early = interp::with_buggy(&m, 12);
    let lost = walk(&early, &then(&["Park", "Answer", "ToldBeforeWrite"]));
    assert!(!early.check_invariant("NoLostAnswer", &lost));
}

/// A crash mid-write (an intent row with no outcome) releases the claim for its one retell; a
/// typed row makes the wake told.
#[test]
fn a_crash_mid_write_retells_once() {
    let m = book(&[]);
    let retold = walk(
        &m,
        &then(&["Break", "Claim", "CrashMidWrite", "Claim", "Write"]),
    );
    assert_eq!(
        (retold["st"], retold["typed"], retold["midwrite"]),
        (6, 2, 1)
    );
    let crashed = walk(&m, &then(&["Break", "Claim", "CrashMidWrite", "Claim"]));
    assert!(
        !m.action_enabled("CrashMidWrite", &crashed),
        "one crash mid-write is the bound's"
    );
    let typed = walk(&m, &then(&["Break", "Claim", "Reconcile"]));
    assert_eq!((typed["st"], typed["typed"]), (6, 1));
    let wrong = interp::with_buggy(&m, 13);
    let twice = walk(
        &wrong,
        &then(&["Break", "Claim", "ReleaseAfterTyped", "Claim", "Write"]),
    );
    assert_eq!((twice["typed"], twice["midwrite"]), (2, 0));
    assert!(!wrong.check_invariant("OneTypedLine", &twice));
}

/// Under `observe` an await is still answered and no line is typed; a managed worker and an
/// unhosted session type none either.
#[test]
fn observe_answers_and_types_nothing() {
    for (config, law) in [
        (&[("Prompt", 0)][..], "NothingTypedUnderObserve"),
        (&[("Managed", 1)][..], "NoLineIntoAManagedWorker"),
        (&[("Hosted", 0)][..], "OneTypedLine"),
    ] {
        let m = book(config);
        let answered = walk(&m, &then(&["Park", "Answer", "WriteOk"]));
        assert_eq!(answered["answers"], 1, "{config:?}");
        let door = walk(&m, &then(&["Break"]));
        assert!(!m.action_enabled("Claim", &door), "{config:?}: {door:?}");
        assert!(m.check_invariant(law, &door));
    }
    let observe = book(&[("Prompt", 0)]);
    let typed = interp::with_buggy(&observe, 10);
    let wrong = walk(&typed, &then(&["Break", "TypeUnderObserve", "Write"]));
    assert!(!typed.check_invariant("NothingTypedUnderObserve", &wrong));
}

/// An expiry ends a held wake: answering or claimed, it is gone; answered, it is told by its
/// answer. Nothing tells a gone wake.
#[test]
fn an_expiry_ends_a_held_wake() {
    let m = book(&[]);
    for (path, after) in [
        (then(&["Park", "Answer", "Expire"]), 7),
        (then(&["Break", "Claim", "Expire"]), 7),
        (then(&["Park", "Answer", "WriteOk", "Expire"]), 6),
        (then(&["UnlessHolds", "Unless"]), 7),
    ] {
        let st = walk(&m, &path);
        assert_eq!(st["st"], after, "{path:?}");
        for teller in ["Answer", "WriteOk", "Claim", "Write", "Reconcile", "Edge"] {
            assert!(!m.action_enabled(teller, &st), "{teller} after {path:?}");
        }
    }
    let wrong = interp::with_buggy(&m, 8);
    let told = walk(&wrong, &then(&["Expire", "TellExpired", "Write"]));
    assert!(!wrong.check_invariant("OnlyReadyIsTold", &told));
}

/// With `a` waiting, a freed slot is `a`'s to take, never the younger `c`'s.
#[test]
fn one_slot_reserves_for_the_oldest_hold() {
    let m = wake_room_model();
    let held = walk(&m, &["ReadyB", "ReserveB", "ReadyA", "RefuseA", "ReadyC"]);
    assert_eq!((held["b_res"], held["a_wait"]), (1, 1));
    assert!(!m.action_enabled("ReserveC", &held), "{held:?}");
    let freed = walk(
        &m,
        &[
            "ReadyB", "ReserveB", "ReadyA", "RefuseA", "ReadyC", "LapseB",
        ],
    );
    assert!(
        !m.action_enabled("ReserveC", &freed),
        "the older hold waits"
    );
    assert!(m.action_enabled("RefuseC", &freed));
    assert!(m.action_enabled("ReserveA", &freed));
    let young = interp::with_buggy(&m, 3);
    let mut wrong = freed.clone();
    assert!(young.fire("YoungestFirst", &mut wrong));
    assert!(!young.check_invariant("TheOlderHoldFirst", &wrong));
    let herd = interp::with_buggy(&m, 2);
    let two = walk(&herd, &["ReadyA", "ReserveA", "ReadyB", "HerdTell"]);
    assert!(!herd.check_invariant("OneUnitOneTellWhileReserved", &two));
}

/// A holder's re-ask, its write skipped, is granted against its own reservation even with the
/// slot gone to its build, and renews nothing.
#[test]
fn a_holders_reask_is_granted_and_renews_nothing() {
    let m = wake_room_model();
    let skipped = walk(&m, &["ReadyA", "ReserveA", "SkipA", "BuildTakes"]);
    assert_eq!((skipped["a_res"], skipped["usable"]), (1, 0));
    assert!(m.action_enabled("ReserveA", &skipped), "{skipped:?}");
    assert!(!m.action_enabled("RefuseA", &skipped));
    let own = interp::with_buggy(&m, 5);
    let mut wrong = skipped.clone();
    assert!(own.fire("CountOwn", &mut wrong));
    assert!(!own.check_invariant("AHolderIsNeverRefused", &wrong));
    let renew = interp::with_buggy(&m, 4);
    let mut wrong = skipped.clone();
    assert!(renew.fire("RenewOnAsk", &mut wrong));
    assert!(!renew.check_invariant("AReservationIsTakenOnce", &wrong));
}

/// A reservation ends at its lapse and the next hold may take the slot; kept while another
/// waits, it outlives its lapse.
#[test]
fn a_reservation_ends_at_its_lapse() {
    let m = wake_room_model();
    let st = walk(
        &m,
        &[
            "ReadyA", "ReserveA", "WriteA", "ReadyB", "RefuseB", "LapseA", "ReserveB",
        ],
    );
    assert_eq!((st["a_res"], st["b_res"], st["b_wait"]), (0, 1, 0));
    let kept = interp::with_buggy(&m, 1);
    let wrong = walk(
        &kept,
        &[
            "ReadyA",
            "ReserveA",
            "WriteA",
            "ReadyB",
            "RefuseB",
            "RenewWhileWaited",
        ],
    );
    assert!(!kept.action_enabled("LapseA", &wrong), "{wrong:?}");
    assert!(!kept.check_invariant("NoReservationOutlivesItsLapse", &wrong));
}
