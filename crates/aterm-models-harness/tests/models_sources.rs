// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier 0 of `SourceEpisode`, `OwnerTell` and `WorkflowStep`: each holds at `Buggy = 0` in every
//! configuration it declares, each law breaks under its own mutant alone, the liveness mutant has
//! an invariant twin, and walks follow the paths the design names. Tier 1 of the first two is
//! `aterm-agent/tests/conformance_sources.rs`; `WorkflowStep`'s lands with its reader. The
//! non-vacuity sweep and the liveness audit are `aterm-models`' `non_vacuity`.

use aterm_models_harness::{
    owner_tell_model, source_episode_liveness, source_episode_model, workflow_step_model,
};
use aterm_spec::derive::Model;
use aterm_spec::interp;
use aterm_spec::walk::walk;

/// A configuration: the constants a scenario overrides.
type Config = &'static [(&'static str, i64)];

/// A mutant, its dial, the law it was written to break, and the configuration it breaks it under.
type Mutant = (&'static str, i64, &'static str, Config);

/// The board's configurations: committed (a restart within the day), then past a day.
const SOURCE_CONFIGS: [Config; 2] = [&[], &[("Stale", 1)]];

const SOURCE_MUTANTS: [Mutant; 5] = [
    ("QuietRestarts", 1, "TheDwellCountsQuietReads", &[]),
    (
        "CloseOnFirstQuiet",
        2,
        "AFlapInsideTheDwellIsOneEpisode",
        &[],
    ),
    ("RebaseOnRestart", 3, "AMoveAcrossARestartIsSeen", &[]),
    ("ReadUnconfirmed", 4, "NoReadUnconfirmed", &[]),
    ("UndoAtFirstBase", 5, "AnUndoNeedsTheDwell", &[]),
];

/// The tell's configurations: committed (`prompt`, live, not urgent, every episode fresh), then
/// each scenario.
const TELL_CONFIGS: [Config; 5] = [
    &[],
    &[("Prompt", 0)],
    &[("Dry", 1)],
    &[("Urgent", 1)],
    &[("Older", 1)],
];

const TELL_MUTANTS: [Mutant; 9] = [
    ("TellKeepsDue", 1, "OncePerEpisode", &[]),
    ("ConfirmByName", 2, "NothingTypedUnconfirmed", &[]),
    (
        "ConfirmStaleSnapshot",
        3,
        "AStaleSnapshotConfirmsNothing",
        &[],
    ),
    ("TypeUnderDryRun", 4, "DryRunTypesNothing", &[("Dry", 1)]),
    (
        "TypeUnderObserve",
        5,
        "NothingTypedUnderObserve",
        &[("Prompt", 0)],
    ),
    ("UrgentUncapped", 6, "UrgentCapHolds", &[("Urgent", 1)]),
    (
        "UrgentPassesGuard",
        7,
        "UrgentIsNeverAGuard",
        &[("Urgent", 1)],
    ),
    ("TellUndone", 8, "NeverToldStale", &[]),
    ("TellOlder", 9, "NeverToldAnOlderEpisode", &[("Older", 1)]),
];

/// The step's configurations: committed (a captured format, `prompt`), then each scenario.
const STEP_CONFIGS: [Config; 3] = [&[], &[("Captured", 0)], &[("Prompt", 0)]];

const STEP_MUTANTS: [Mutant; 4] = [
    ("TellKeepsDue", 1, "OnceAStep", &[]),
    ("TellAfterEnd", 2, "NeverToldAfterTheRunEnds", &[]),
    ("ReadUncaptured", 3, "UnreadIsQuiet", &[("Captured", 0)]),
    (
        "TypeUnderObserve",
        4,
        "NothingTypedUnderObserve",
        &[("Prompt", 0)],
    ),
];

/// A bound on the states any configuration of any of the three reaches, at any dial.
const STATE_BOUND: usize = 50_000;

/// `m` with every mutant of `mutants` removed but `keep`.
fn alone(m: &Model, mutants: &[Mutant], keep: &str) -> Model {
    let mut one = m.clone();
    one.actions
        .retain(|a| a.name == keep || mutants.iter().all(|(mutant, ..)| *mutant != a.name));
    one
}

/// No law breaks at `Buggy = 0` in any of `configs`, and the actions that fire across them are
/// exactly those that are no mutant.
fn fires_exactly_its_committed_moves(base: &Model, configs: &[Config], mutants: &[Mutant]) {
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
fn each_breaks_its_own_law_alone(base: &Model, mutants: &[Mutant]) {
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
fn source_episode_fires_exactly_its_committed_moves() {
    fires_exactly_its_committed_moves(&source_episode_model(), &SOURCE_CONFIGS, &SOURCE_MUTANTS);
}

#[test]
fn owner_tell_fires_exactly_its_committed_moves() {
    fires_exactly_its_committed_moves(&owner_tell_model(), &TELL_CONFIGS, &TELL_MUTANTS);
}

#[test]
fn workflow_step_fires_exactly_its_committed_moves() {
    fires_exactly_its_committed_moves(&workflow_step_model(), &STEP_CONFIGS, &STEP_MUTANTS);
}

#[test]
fn each_mutant_breaks_its_own_law_alone() {
    each_breaks_its_own_law_alone(&source_episode_model(), &SOURCE_MUTANTS);
    each_breaks_its_own_law_alone(&owner_tell_model(), &TELL_MUTANTS);
    each_breaks_its_own_law_alone(&workflow_step_model(), &STEP_MUTANTS);
}

/// The liveness mutant (dial 1) is additive, dead with no mutant on and firing at its dial, and
/// breaks an invariant twin, so a safety walk catches it too.
#[test]
fn each_liveness_mutant_has_an_invariant_twin() {
    let model = source_episode_model();
    let live = source_episode_liveness();
    let committed = interp::fired_actions(&interp::with_buggy(&model, 0));
    let dialled = interp::fired_actions(&interp::with_buggy(&model, 1));
    for mutant in &live.mutants {
        assert!(!committed.contains(mutant), "{mutant} is dead at Buggy = 0");
        assert!(dialled.contains(mutant), "{mutant} fires at Buggy = 1");
    }
    assert!(
        interp::violated_invariants(&interp::with_buggy(&model, 1))
            .contains(&"TheDwellCountsQuietReads"),
        "dial 1 breaks TheDwellCountsQuietReads"
    );
}

#[test]
fn each_model_stays_under_50k_states() {
    let mut counts = Vec::new();
    for (base, configs, mutants) in [
        (
            source_episode_model(),
            &SOURCE_CONFIGS[..],
            &SOURCE_MUTANTS[..],
        ),
        (owner_tell_model(), &TELL_CONFIGS[..], &TELL_MUTANTS[..]),
        (workflow_step_model(), &STEP_CONFIGS[..], &STEP_MUTANTS[..]),
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

fn board(config: Config) -> Model {
    interp::with_consts(&source_episode_model(), config)
}

/// The base read, then the move to 1 read: an episode open from 0.
const OPENED: [&str; 3] = ["Look", "Move", "Look"];

fn opened(more: &[&'static str]) -> Vec<&'static str> {
    [&OPENED[..], more].concat()
}

/// A flap inside the dwell (0 -> 1 -> 0 -> 1, a quiet read between two of its moves) joins one
/// episode, its base the first read's; superseded at the first quiet read, it is two.
#[test]
fn a_flap_inside_the_dwell_is_one_episode() {
    let m = board(&[]);
    let st = walk(&m, &opened(&["MoveBack", "Look", "Look", "Move", "Look"]));
    assert_eq!(
        (st["open"], st["base"], st["latest"], st["quiet"]),
        (1, 0, 1, 0),
        "{st:?}"
    );
    let early = interp::with_buggy(&m, 2);
    let split = walk(&early, &opened(&["Look", "Move", "CloseOnFirstQuiet"]));
    assert_eq!((split["base"], split["latest"]), (1, 2), "{split:?}");
    assert!(!early.check_invariant("AFlapInsideTheDwellIsOneEpisode", &split));
}

/// Two moves a dwell apart are two episodes: the second supersedes the first, its base the
/// first's latest.
#[test]
fn two_moves_a_dwell_apart_are_two_episodes() {
    let m = board(&[]);
    let dwelt = walk(&m, &opened(&["Look", "Look"]));
    assert_eq!((dwelt["open"], dwelt["quiet"]), (1, 2), "{dwelt:?}");
    let second = walk(&m, &opened(&["Look", "Look", "Move", "Look"]));
    assert_eq!(
        (
            second["open"],
            second["base"],
            second["latest"],
            second["quiet"]
        ),
        (1, 1, 2, 0),
        "{second:?}"
    );
}

/// A move undone and held at its base for the dwell closes the episode at the read that ends
/// the dwell, never before it.
#[test]
fn a_move_undone_closes_only_after_the_dwell() {
    let m = board(&[]);
    let joined = walk(&m, &opened(&["MoveBack", "Look"]));
    assert_eq!(
        (joined["open"], joined["latest"], joined["quiet"]),
        (1, 0, 0),
        "{joined:?}"
    );
    let undone = walk(&m, &opened(&["MoveBack", "Look", "Look"]));
    assert_eq!((undone["open"], undone["quiet"]), (1, 1), "{undone:?}");
    let closed = walk(&m, &opened(&["MoveBack", "Look", "Look", "Look"]));
    assert_eq!(
        (closed["open"], closed["base"], closed["latest"]),
        (0, 0, 0),
        "{closed:?}"
    );
    let early = interp::with_buggy(&m, 5);
    let mut wrong = joined.clone();
    assert!(early.fire("UndoAtFirstBase", &mut wrong));
    assert_eq!(wrong["open"], 0);
    assert!(!early.check_invariant("AnUndoNeedsTheDwell", &wrong));
}

/// Nothing is read while aterm is down; a move made then is an edge at the first read after the
/// restart; past a day, the restart re-bases and the first read is the base.
#[test]
fn a_move_across_a_restart_is_an_edge() {
    let m = board(&[]);
    let gap = walk(&m, &["Look", "Down", "Move"]);
    assert!(!m.action_enabled("Look", &gap), "{gap:?}");
    let st = walk(&m, &["Look", "Down", "Move", "Up", "Look"]);
    assert_eq!(
        (st["open"], st["base"], st["latest"], st["seen"]),
        (1, 0, 1, 1),
        "{st:?}"
    );
    let stale = board(&[("Stale", 1)]);
    let rebased = walk(&stale, &["Look", "Down", "Move", "Up"]);
    assert_eq!(rebased["seen"], 3, "{rebased:?}");
    let based = walk(&stale, &["Look", "Down", "Move", "Up", "Look"]);
    assert_eq!((based["open"], based["seen"]), (0, 1), "{based:?}");
    let lost = interp::with_buggy(&m, 3);
    let wrong = walk(&lost, &["Look", "Down", "Move", "RebaseOnRestart", "Look"]);
    assert_eq!(wrong["open"], 0, "{wrong:?}");
    assert!(!lost.check_invariant("AMoveAcrossARestartIsSeen", &wrong));
}

/// A source no confirmed rule names is not read.
#[test]
fn an_unarmed_source_is_not_read() {
    let m = board(&[]);
    let disarmed = walk(&m, &["Disarm", "Move"]);
    assert!(!m.action_enabled("Look", &disarmed), "{disarmed:?}");
    let rearmed = walk(&m, &["Disarm", "Move", "Arm", "Look"]);
    assert_eq!(rearmed["seen"], 1, "{rearmed:?}");
    let rogue = interp::with_buggy(&m, 4);
    let mut wrong = disarmed.clone();
    assert!(rogue.fire("ReadUnconfirmed", &mut wrong));
    assert!(!rogue.check_invariant("NoReadUnconfirmed", &wrong));
}

fn tell(config: Config) -> Model {
    interp::with_consts(&owner_tell_model(), config)
}

/// The owner's yes to the table in force, then an episode opened.
const CONFIRMED: [&str; 3] = ["Press", "ConfirmWrite", "Open"];

fn confirmed(more: &[&'static str]) -> Vec<&'static str> {
    [&CONFIRMED[..], more].concat()
}

/// A yes writes the snapshot's rev only while the table in force still hashes to it; written
/// whatever the table, a yes to a stale snapshot confirms the edit.
#[test]
fn a_yes_confirms_only_the_snapshot_still_in_force() {
    let m = tell(&[]);
    let current = walk(&m, &["Press", "ConfirmWrite"]);
    assert_eq!((current["crev"], current["snap"]), (0, 2), "{current:?}");
    let stale = walk(&m, &["Press", "EditTable", "ConfirmWrite"]);
    assert_eq!((stale["crev"], stale["snap"]), (2, 2), "{stale:?}");
    let wrong = interp::with_buggy(&m, 3);
    let st = walk(&wrong, &["Press", "EditTable", "ConfirmStaleSnapshot"]);
    assert_eq!(st["crev"], 0, "{st:?}");
    assert!(!wrong.check_invariant("AStaleSnapshotConfirmsNothing", &st));
}

/// A confirmed live rule's episode is told once; the next episode is told again. Until it is told,
/// held by a guard or the pace or not, its line is neither lapsed nor booked a would row.
#[test]
fn a_confirmed_rule_is_told_once_an_episode() {
    let m = tell(&[]);
    for held in [&[][..], &["GuardOn"], &["PaceOn"]] {
        let due = walk(&m, &confirmed(held));
        for dropper in ["Would", "Lapse"] {
            assert!(
                !m.action_enabled(dropper, &due),
                "{dropper} after {held:?}: {due:?}"
            );
        }
    }
    let told = walk(&m, &confirmed(&["Tell"]));
    assert_eq!((told["told"], told["due"]), (1, 0), "{told:?}");
    assert!(!m.action_enabled("Tell", &told), "{told:?}");
    let next = walk(&m, &confirmed(&["Tell", "Close", "Open"]));
    assert!(m.action_enabled("Tell", &next), "{next:?}");
    let keeps = interp::with_buggy(&m, 1);
    let wrong = walk(&keeps, &confirmed(&["TellKeepsDue", "Tell"]));
    assert!(!keeps.check_invariant("OncePerEpisode", &wrong));
}

/// An edit after the yes is a new rev: the episode lapses untold; counted by any yes on record,
/// the edited table is typed.
#[test]
fn an_edited_table_types_nothing_under_the_old_yes() {
    let m = tell(&[]);
    let edited = walk(&m, &confirmed(&["EditTable"]));
    for teller in ["Tell", "Would"] {
        assert!(!m.action_enabled(teller, &edited), "{teller}: {edited:?}");
    }
    assert!(m.action_enabled("Lapse", &edited), "{edited:?}");
    let unconfirmed = walk(&m, &["Open"]);
    assert!(!m.action_enabled("Tell", &unconfirmed), "{unconfirmed:?}");
    let by_name = interp::with_buggy(&m, 2);
    let mut wrong = edited.clone();
    assert!(by_name.fire("ConfirmByName", &mut wrong));
    assert!(!by_name.check_invariant("NothingTypedUnconfirmed", &wrong));
}

/// Under `observe`, and for a `dry-run` rule under `prompt`, a confirmed rule's episode is a
/// would row, never a tell.
#[test]
fn observe_and_dry_run_book_would_rows() {
    for (config, dial, mutant, law) in [
        (
            &[("Prompt", 0)][..],
            5,
            "TypeUnderObserve",
            "NothingTypedUnderObserve",
        ),
        (
            &[("Dry", 1)][..],
            4,
            "TypeUnderDryRun",
            "DryRunTypesNothing",
        ),
    ] {
        let m = tell(config);
        let due = walk(&m, &confirmed(&[]));
        assert!(!m.action_enabled("Tell", &due), "{config:?}: {due:?}");
        let would = walk(&m, &confirmed(&["Would"]));
        assert_eq!((would["due"], would["told"]), (0, 0), "{config:?}");
        let typed = interp::with_buggy(&m, dial);
        let mut wrong = due.clone();
        assert!(typed.fire(mutant, &mut wrong), "{mutant}");
        assert!(!typed.check_invariant(law, &wrong), "{mutant}");
    }
}

/// An urgent rule passes the session's pace up to its cap an hour, and never a guard.
#[test]
fn urgent_passes_the_pace_up_to_its_cap_and_never_a_guard() {
    let paced = walk(&tell(&[]), &confirmed(&["PaceOn"]));
    assert!(!tell(&[]).action_enabled("Tell", &paced), "{paced:?}");
    let m = tell(&[("Urgent", 1)]);
    let first = walk(&m, &confirmed(&["PaceOn", "Tell"]));
    assert_eq!((first["told"], first["urg"]), (1, 1), "{first:?}");
    let capped = walk(&m, &confirmed(&["PaceOn", "Tell", "Close", "Open"]));
    assert!(!m.action_enabled("Tell", &capped), "{capped:?}");
    let hour = walk(
        &m,
        &confirmed(&["PaceOn", "Tell", "Close", "Open", "HourPasses"]),
    );
    assert!(m.action_enabled("Tell", &hour), "{hour:?}");
    let guarded = walk(&m, &confirmed(&["GuardOn"]));
    assert!(!m.action_enabled("Tell", &guarded), "{guarded:?}");
    let uncapped = interp::with_buggy(&m, 6);
    let mut wrong = capped.clone();
    assert!(uncapped.fire("UrgentUncapped", &mut wrong));
    assert!(!uncapped.check_invariant("UrgentCapHolds", &wrong));
    let passes = interp::with_buggy(&m, 7);
    let mut wrong = guarded.clone();
    assert!(passes.fire("UrgentPassesGuard", &mut wrong));
    assert!(!passes.check_invariant("UrgentIsNeverAGuard", &wrong));
}

/// A closed episode keeps its due line until a look lapses it, and is never told; nor is one
/// that opened before the session's agent did.
#[test]
fn a_closed_or_older_episode_is_never_told() {
    let m = tell(&[]);
    let closed = walk(&m, &confirmed(&["Close"]));
    assert_eq!(closed["due"], 1, "{closed:?}");
    assert!(!m.action_enabled("Tell", &closed), "{closed:?}");
    assert!(m.action_enabled("Lapse", &closed), "{closed:?}");
    let undone = interp::with_buggy(&m, 8);
    let mut wrong = closed.clone();
    assert!(undone.fire("TellUndone", &mut wrong));
    assert!(!undone.check_invariant("NeverToldStale", &wrong));
    let old = tell(&[("Older", 1)]);
    let older = walk(&old, &confirmed(&[]));
    assert_eq!(older["fresh"], 0, "{older:?}");
    assert!(!old.action_enabled("Tell", &older), "{older:?}");
    assert!(old.action_enabled("Lapse", &older), "{older:?}");
    let told = interp::with_buggy(&old, 9);
    let mut wrong = older.clone();
    assert!(told.fire("TellOlder", &mut wrong));
    assert!(!told.check_invariant("NeverToldAnOlderEpisode", &wrong));
}

fn step(config: Config) -> Model {
    interp::with_consts(&workflow_step_model(), config)
}

/// One track's steps and variables, and the other track's return.
struct Track {
    ret: &'static str,
    open: &'static str,
    tell: &'static str,
    would: &'static str,
    lapse: &'static str,
    told: &'static str,
    due: &'static str,
    other: &'static str,
}

/// The two tracks: each walk runs on both, since the mutants drop a guard term on track A only.
const TRACKS: [Track; 2] = [
    Track {
        ret: "ReturnA",
        open: "OpenA",
        tell: "TellA",
        would: "WouldA",
        lapse: "LapseA",
        told: "told_a",
        due: "due_a",
        other: "ReturnB",
    },
    Track {
        ret: "ReturnB",
        open: "OpenB",
        tell: "TellB",
        would: "WouldB",
        lapse: "LapseB",
        told: "told_b",
        due: "due_b",
        other: "ReturnA",
    },
];

/// `m` with `action`'s guard gone: it fires in every state.
fn unguarded(m: &Model, action: &str) -> Model {
    let mut any = m.clone();
    any.actions
        .iter_mut()
        .find(|a| a.name == action)
        .unwrap_or_else(|| panic!("{action} is an action"))
        .guard = None;
    any
}

/// Each committed open and tell writes its laws' ghosts from the state it fires in, so on either
/// track one with its guard gone breaks each law its guard stands on.
#[test]
fn each_committed_step_is_judged_by_its_laws() {
    for t in &TRACKS {
        for (action, law, config) in [
            (t.open, "UnreadIsQuiet", &[("Captured", 0)][..]),
            (t.tell, "OnceAStep", &[]),
            (t.tell, "NeverToldAfterTheRunEnds", &[]),
            (t.tell, "NothingTypedUnderObserve", &[("Prompt", 0)]),
        ] {
            let m = aterm_spec::walk::only(&step(config), law);
            assert!(
                interp::violated_invariants(&m).is_empty(),
                "the committed WorkflowStep keeps {law} under {config:?}"
            );
            assert!(
                interp::violated_invariants(&unguarded(&m, action)).contains(law),
                "{action} with no guard breaks {law} under {config:?}"
            );
        }
    }
}

/// A track's step opens only once it returned, and is told once while the other track runs;
/// until then its line is neither lapsed nor booked a would row.
#[test]
fn a_returned_step_is_told_once_while_the_run_goes_on() {
    let m = step(&[]);
    for t in &TRACKS {
        assert!(!m.action_enabled(t.open, &m.init_state()), "{}", t.open);
        let due = walk(&m, &[t.ret, t.open]);
        for dropper in [t.would, t.lapse] {
            assert!(!m.action_enabled(dropper, &due), "{dropper}: {due:?}");
        }
        let told = walk(&m, &[t.ret, t.open, t.tell]);
        assert_eq!((told[t.told], told[t.due]), (1, 2), "{told:?}");
        for again in [t.open, t.tell, t.would, t.lapse] {
            assert!(!m.action_enabled(again, &told), "{again}: {told:?}");
        }
    }
    let keeps = interp::with_buggy(&m, 1);
    let wrong = walk(&keeps, &["ReturnA", "OpenA", "TellKeepsDue", "TellA"]);
    assert!(!keeps.check_invariant("OnceAStep", &wrong));
}

/// A step whose run ended before its line went lapses untold; no step opens once both returned.
#[test]
fn a_step_whose_run_ended_lapses() {
    let m = step(&[]);
    for t in &TRACKS {
        let ended = walk(&m, &[t.ret, t.open, t.other]);
        assert!(!m.action_enabled(t.tell, &ended), "{}: {ended:?}", t.tell);
        assert!(m.action_enabled(t.lapse, &ended), "{}: {ended:?}", t.lapse);
        let both = walk(&m, &[t.ret, t.other]);
        assert!(!m.action_enabled(t.open, &both), "{}: {both:?}", t.open);
    }
    let late = interp::with_buggy(&m, 2);
    let mut wrong = walk(&m, &["ReturnA", "OpenA", "ReturnB"]);
    assert!(late.fire("TellAfterEnd", &mut wrong));
    assert!(!late.check_invariant("NeverToldAfterTheRunEnds", &wrong));
}

/// With no capture for the running build's journal, no step opens; under `observe` a step is a
/// would row.
#[test]
fn an_unread_format_is_quiet_and_observe_types_nothing() {
    let unread = step(&[("Captured", 0)]);
    let observe = step(&[("Prompt", 0)]);
    for t in &TRACKS {
        let returned = walk(&unread, &[t.ret]);
        assert!(!unread.action_enabled(t.open, &returned), "{returned:?}");
        let due = walk(&observe, &[t.ret, t.open]);
        assert!(!observe.action_enabled(t.tell, &due), "{due:?}");
        let would = walk(&observe, &[t.ret, t.open, t.would]);
        assert_eq!((would[t.due], would[t.told]), (2, 0), "{would:?}");
    }
    let read = interp::with_buggy(&unread, 3);
    let mut wrong = walk(&unread, &["ReturnA"]);
    assert!(read.fire("ReadUncaptured", &mut wrong));
    assert!(!read.check_invariant("UnreadIsQuiet", &wrong));
    let typed = interp::with_buggy(&observe, 4);
    let mut wrong = walk(&observe, &["ReturnA", "OpenA"]);
    assert!(typed.fire("TypeUnderObserve", &mut wrong));
    assert!(!typed.check_invariant("NothingTypedUnderObserve", &wrong));
}
