// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Digest, Uuid, eligible_owned, settle};
use aterm_spec::interp::State;

const SETTLE_ISLAND: &str = include_str!("file_merge_settle_island.rs");
const OWNED_ISLAND: &str = include_str!("file_merge_owned_island.rs");

/// The literal `island` proves the sheet `name` equals.
fn proved(island: &'static str, name: &str) -> &'static str {
    let key = format!("{name} = \"");
    let start = island
        .find(&key)
        .unwrap_or_else(|| panic!("the island proves no {name}"))
        + key.len();
    let len = island[start..]
        .find('"')
        .expect("the sheet's closing quote");
    &island[start..start + len]
}

fn bit(answer: bool) -> char {
    if answer { '1' } else { '0' }
}

/// The settle sheet: the register's candidates, then the intent's expected ones, each ranging
/// over every list of at most two of the digests 0, 1 and 2, as the island's `lists` orders them.
fn settle_sheet(settle: impl Fn(&[Digest], &[Digest]) -> bool) -> String {
    let mut lists: Vec<Vec<Digest>> = vec![Vec::new()];
    lists.extend((0..3).map(|a| vec![[a; 32]]));
    for a in 0..3 {
        lists.extend((0..3).map(|b| vec![[a; 32], [b; 32]]));
    }
    let mut sheet = String::new();
    for now in &lists {
        for expected in &lists {
            sheet.push(bit(settle(now, expected)));
        }
    }
    sheet
}

/// The eligibility sheet: the key, then the signer, each naming no machine, machine 0, then
/// machine 1, as the island's `names` orders them.
fn eligible_sheet(eligible: impl Fn(Option<Uuid>, Option<Uuid>) -> bool) -> String {
    let names = [None, Some([0; 16]), Some([1; 16])];
    let mut sheet = String::new();
    for key in names {
        for signer in names {
            sheet.push(bit(eligible(key, signer)));
        }
    }
    sheet
}

#[test]
fn the_kernels_answer_the_islands_sheets() {
    assert_eq!(settle_sheet(settle), proved(SETTLE_ISLAND, "sheetSettle"));
    assert_eq!(
        eligible_sheet(eligible_owned),
        proved(OWNED_ISLAND, "sheetEligible")
    );
}

/// NEGATIVE CONTROL: a publisher that asks only whether what it observed is still there (the
/// island's `settleStillThere`) answers another sheet, and writes over a register that gained a
/// candidate nobody observed.
#[test]
fn a_settle_that_asks_only_for_what_it_observed_answers_another_sheet() {
    let still_there =
        |now: &[Digest], expected: &[Digest]| expected.iter().all(|digest| now.contains(digest));
    assert_ne!(
        settle_sheet(still_there),
        proved(SETTLE_ISLAND, "sheetSettle")
    );
    let (observed, unobserved) = ([1; 32], [2; 32]);
    assert!(still_there(&[observed, unobserved], &[observed]));
    assert!(!settle(&[observed, unobserved], &[observed]));
}

/// NEGATIVE CONTROL: eligibility as a bare comparison of the two names (the island's
/// `eligibleBare`) answers another sheet, and projects a row whose key names no machine.
#[test]
fn an_eligibility_that_compares_two_absent_names_answers_another_sheet() {
    let bare = |key: Option<Uuid>, signer: Option<Uuid>| key == signer;
    assert_ne!(eligible_sheet(bare), proved(OWNED_ISLAND, "sheetEligible"));
    assert!(bare(None, None));
    assert!(!eligible_owned(None, None));
}

/// NEGATIVE CONTROL: a publisher that asks only whether the register holds nothing it never
/// observed answers another sheet, and writes over a register that lost a candidate.
#[test]
fn a_settle_that_asks_only_for_nothing_new_answers_another_sheet() {
    let nothing_new =
        |now: &[Digest], expected: &[Digest]| now.iter().all(|digest| expected.contains(digest));
    assert_ne!(
        settle_sheet(nothing_new),
        proved(SETTLE_ISLAND, "sheetSettle")
    );
    let (kept, gone) = ([1; 32], [2; 32]);
    assert!(nothing_new(&[kept], &[kept, gone]));
    assert!(!settle(&[kept], &[kept, gone]));
}

#[test]
fn settle_writes_the_observed_set_in_any_order_and_holds_a_moved_register() {
    let (a, b, c) = ([1; 32], [2; 32], [3; 32]);
    assert!(settle(&[], &[]));
    assert!(settle(&[a, b], &[b, a]));
    assert!(settle(&[a, b, a], &[b, a]));
    // A born cell expects no candidate, and a cell a peer made meanwhile has one.
    assert!(!settle(&[a], &[]));
    // The register gained a candidate, lost one, or changed one for another.
    assert!(!settle(&[a, b], &[a]));
    assert!(!settle(&[a], &[a, b]));
    assert!(!settle(&[a, c], &[a, b]));
}

#[test]
fn only_a_row_whose_key_names_its_signer_is_eligible() {
    let (here, peer) = ([7; 16], [9; 16]);
    assert!(eligible_owned(Some(here), Some(here)));
    assert!(!eligible_owned(Some(here), Some(peer)));
    assert!(!eligible_owned(None, Some(here)));
    assert!(!eligible_owned(Some(here), None));
    assert!(!eligible_owned(None, None));
}

/// The candidate the head intent observed, this Mac's own write, and the peer's.
const OBSERVED: Digest = [0; 32];
const OURS: Digest = [1; 32];
const PEERS: Digest = [2; 32];

fn set(state: &State, changes: &[(&'static str, i64)]) -> State {
    let mut next = state.clone();
    for (var, value) in changes {
        assert!(
            next.insert(var, *value).is_some(),
            "the model has no variable {var}"
        );
    }
    next
}

/// The registers the head intent of the model's writing walk may meet at `state`. Either
/// write replaced the one candidate it observed, or stands beside it, written without having
/// seen it; a peer's write that arrived after this Mac's stands beside that, or replaced it.
fn registers(state: &State) -> &'static [&'static [Digest]] {
    match (state["cb"], state["mine"], state["recs"]) {
        (0, 0, _) => &[&[OBSERVED]],
        (1, 0, 0) => &[&[PEERS], &[OBSERVED, PEERS]],
        (1, 0, _) => &[&[PEERS]],
        (0, 1, _) => &[&[OURS]],
        _ => &[&[OURS, PEERS]],
    }
}

/// One publisher pass over the head intent, which observed [`OBSERVED`] alone, at the register
/// `now`: the action it is and the state it leaves. An intent whose record is in the live
/// home's history is done, whatever the register holds; any other is signed where `settle`
/// says the register is as observed, and held where it moved.
fn publish(
    state: &State,
    now: &[Digest],
    settle: impl Fn(&[Digest], &[Digest]) -> bool,
) -> (&'static str, State) {
    if state["recs"] > 0 {
        let done = [
            ("q", state["q"] - 1),
            ("p", state["p"] + 1),
            ("mine", 0),
            ("cb", 0),
            ("recs", 0),
        ];
        ("Done", set(state, &done))
    } else if settle(now, &[OBSERVED]) {
        let lost = i64::from(now.contains(&PEERS)).max(state["lost"]);
        let wrote = [("mine", 1), ("cb", 0), ("recs", 1), ("lost", lost)];
        ("Sign", set(state, &wrote))
    } else {
        let held = [("q", state["q"] - 1), ("h", state["h"] + 1), ("cb", 0)];
        ("Hold", set(state, &held))
    }
}

#[test]
fn settle_conforms_at_every_reachable_publisher_pass_and_catches_a_write_over_a_moved_register() {
    let model = aterm_models_db::shared_file_model();
    let mut passes = 0;
    let (mut beside, mut replaced) = (None, None);
    for state in aterm_spec::interp::reachable_states(&model) {
        if state["area"] != 2 || state["q"] == 0 || state["custody"] != 0 {
            continue;
        }
        for now in registers(&state) {
            passes += 1;
            let (action, next) = publish(&state, now, settle);
            for other in ["Sign", "Done", "Hold"] {
                assert_eq!(
                    model.action_enabled(other, &state),
                    other == action,
                    "{other} at {state:?} with the register {now:?}"
                );
            }
            assert!(
                model.successors(action, &state).contains(&next),
                "{action}: {state:?} -> {next:?}"
            );
            if action == "Hold" && now.contains(&OBSERVED) {
                beside = Some((state.clone(), *now));
            }
            if action == "Done" && !now.contains(&OURS) {
                replaced = Some((state.clone(), *now));
            }
        }
    }
    assert!(
        passes > 6,
        "the writing walk reached {passes} publisher passes"
    );
    // NEGATIVE CONTROLS: a publisher that ignores `expected`, and one that asks only whether
    // what it observed is still there, each write over a register that gained a candidate.
    let (moved, now) = beside.expect("the walk holds an intent beside a candidate it never saw");
    let still_there =
        |now: &[Digest], expected: &[Digest]| expected.iter().all(|digest| now.contains(digest));
    for (action, bad) in [
        publish(&moved, now, |_, _| true),
        publish(&moved, now, still_there),
    ] {
        assert_eq!(action, "Sign");
        assert!(!model.action_enabled("Sign", &moved));
        assert!(!model.check_invariant("PublishObservedOnly", &bad));
    }
    // The other half of `settle`, that the register holds nothing the intent never observed,
    // has no control here: a register loses a candidate only to a newer one, so no pass meets
    // a register that only shrank. The islands' sheet holds that half
    // (`a_settle_that_asks_only_for_nothing_new_answers_another_sheet`).

    // NEGATIVE CONTROL: a publisher that reads "done" off the candidates holds an intent whose
    // record a peer has since replaced, which is published and no step of the model holds.
    let (state, now) = replaced.expect("the walk has a peer replace this Mac's record");
    assert!(!settle(now, &[OBSERVED]) && !now.contains(&OURS));
    assert!(!model.action_enabled("Hold", &state));
    assert_eq!(publish(&state, now, settle).0, "Done");
}

/// A row or a log entry at this Mac's key, signed by this Mac (1) or by the peer (2), read
/// with `eligible`: the signer that projects, or what projected before.
fn projected(
    eligible: impl Fn(Option<Uuid>, Option<Uuid>) -> bool,
    signer: i64,
    before: i64,
) -> i64 {
    let uuid = |machine: i64| Some([u8::try_from(machine).expect("a machine number"); 16]);
    if eligible(uuid(1), uuid(signer)) {
        signer
    } else {
        before
    }
}

#[test]
fn eligibility_conforms_at_every_reachable_owned_read_and_catches_a_foreign_row() {
    let model = aterm_models_db::shared_file_model();
    let keep = model
        .consts
        .iter()
        .find_map(|(name, value)| (*name == "Keep").then_some(*value))
        .expect("the model's log keep");
    let mut reads = 0;
    let mut authority = None;
    for state in aterm_spec::interp::reachable_states(&model) {
        if state["area"] != 3 {
            continue;
        }
        let row = |signer| ("rowby", projected(eligible_owned, signer, state["rowby"]));
        let entry = |signer| ("logby", projected(eligible_owned, signer, state["logby"]));
        let steps: [(&str, Vec<(&'static str, i64)>); 4] = [
            ("OwnRow", vec![row(1)]),
            ("ForeignRow", vec![row(2)]),
            (
                "OwnEntry",
                vec![
                    entry(1),
                    ("logged", state["logged"] + 1),
                    ("logrows", (state["logrows"] + 1).min(keep)),
                ],
            ),
            ("ForeignEntry", vec![entry(2)]),
        ];
        for (action, changes) in steps {
            if !model.action_enabled(action, &state) {
                continue;
            }
            reads += 1;
            let next = set(&state, &changes);
            assert!(
                model.successors(action, &state).contains(&next),
                "{action}: {state:?} -> {next:?}"
            );
        }
        authority.get_or_insert(state);
    }
    assert!(reads > 4, "the authority walk reached {reads} owned reads");
    // NEGATIVE CONTROL: a reader that takes any key naming a machine projects the peer's row.
    let state = authority.expect("the walk reaches the authority area");
    let any_key = |key: Option<Uuid>, _: Option<Uuid>| key.is_some();
    let bad = set(&state, &[("rowby", projected(any_key, 2, state["rowby"]))]);
    assert!(!model.successors("ForeignRow", &state).contains(&bad));
    assert!(!model.check_invariant("OwnKeyOnly", &bad));
}
