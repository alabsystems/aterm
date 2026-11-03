// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The storage release: `DbStorageRelease` conformance over [`pass`], and the release itself on
//! scratch labs (design §3.8, §3.10, step 13).

use super::*;
use crate::table::tests::Lab;
use aterm_secrets::commons::roster::Role;
use aterm_spec::derive::Model;
use std::collections::BTreeMap;

type State = BTreeMap<&'static str, i64>;

/// One grace, in seconds, for the walk's clock.
const GRACE: u64 = 1000;
/// When this Mac first saw the seal, by its own clock, in the walk.
const SEEN_AT: u64 = 50_000;

/// The inputs [`pass`] gets in the model's `state`: a lapsing or restated table, sealed by a
/// claim `claim` graces old, first seen here `age` graces ago, covered where the covers carry
/// what this Mac holds.
fn inputs(state: &State) -> (Option<u64>, Carry, Option<u64>, u64, bool) {
    let now = SEEN_AT + state["age"] as u64 * GRACE;
    let sealed_ms = (state["sealed"] == 1).then(|| (now - state["claim"] as u64 * GRACE) * 1000);
    let carry = if state["carry"] == 1 {
        Carry::Lapses { grace_s: GRACE }
    } else {
        Carry::Restated
    };
    let seen = (state["seen"] == 1).then_some(SEEN_AT);
    (
        sealed_ms,
        carry,
        seen,
        now,
        state["held"] <= state["covered"],
    )
}

/// The model state after the code's step `action` from `state`: [`pass`] for a release pass;
/// the environment's own step for the rest, which the walk sets.
pub(crate) fn release_transition(state: &State, action: &str) -> State {
    let mut next = state.clone();
    let mut set = |name: &'static str, value: i64| {
        next.insert(name, value);
    };
    match action {
        "Lapses" => set("carry", 1),
        "Restated" => set("carry", 2),
        "Restate" | "RestateLate" => set("covered", state["held"]),
        "Seal" => {
            set("sealed", 1);
            set("claim", 0);
        }
        "SealSkewed" => {
            set("sealed", 1);
            set("claim", 2);
        }
        "Late" => set("held", state["held"] + 1),
        "Elapse" => {
            if state["seen"] == 1 && state["age"] < 2 {
                set("age", state["age"] + 1);
            }
            set("claim", (state["claim"] + 1).min(2));
        }
        "Pass" => {
            let (sealed_ms, carry, seen, now, covered) = inputs(state);
            let (stamp, decision) = pass(sealed_ms, carry, seen, now, covered);
            let needed = (state["carry"] == 1 && state["age"] < 1)
                || (state["carry"] == 2 && state["held"] > state["covered"]);
            if decision.is_ok() {
                set("stored", 0);
                if needed {
                    set("lost", 1);
                }
                if state["sealed"] == 0 {
                    set("early", 1);
                }
            } else {
                set("stored", 1);
            }
            set("seen", i64::from(stamp.is_some()));
        }
        other => panic!("no step for {other}"),
    }
    next
}

/// Every state the model reaches.
fn walk(model: &Model) -> BTreeMap<State, Vec<&'static str>> {
    let init = model.init_state();
    let mut reached = BTreeMap::from([(init.clone(), Vec::new())]);
    let mut queue = vec![init];
    while let Some(state) = queue.pop() {
        for action in &model.actions {
            if !model.action_enabled(action.name, &state) {
                continue;
            }
            let next = release_transition(&state, action.name);
            assert!(
                model.successors(action.name, &state).contains(&next),
                "{} from {state:?} steps to {next:?}",
                action.name
            );
            if !reached.contains_key(&next) {
                let mut path = reached[&state].clone();
                path.push(action.name);
                reached.insert(next.clone(), path);
                queue.push(next);
            }
        }
    }
    reached
}

/// Every release pass is a `DbStorageRelease` successor from every state the model reaches, and
/// the walk reaches every state. NEGATIVE CONTROLS: a pass that trusts any cover row releases a
/// restated table while this Mac holds a record no cover carried, which breaks
/// `NothingNeededReleased` and is no successor; and one that counts the grace by the sealer's
/// claim alone releases a lapsing table the moment a skewed sealer's seal is seen, also no
/// successor.
#[test]
fn storage_release_conforms() {
    let model = aterm_models_db::storage_release_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let reached = walk(&model);
    assert_eq!(
        reached.len(),
        aterm_spec::interp::reachable_states(&model).len()
    );

    // Any cover row taken as carrying everything.
    let late = reached
        .keys()
        .find(|s| {
            s["carry"] == 2
                && s["sealed"] == 1
                && s["held"] > s["covered"]
                && s["covered"] > 0
                && s["stored"] == 1
        })
        .expect("a sealed restated table with a late record");
    let mut blind = late.clone();
    blind.insert("stored", 0);
    blind.insert("lost", 1);
    blind.insert("seen", 1);
    assert!(!model.check_invariant("NothingNeededReleased", &blind));
    assert!(!model.successors("Pass", late).contains(&blind));
    let (sealed_ms, carry, seen, now, _) = inputs(late);
    assert_eq!(
        pass(sealed_ms, carry, seen, now, false).1,
        Err(Kept::NotCarried)
    );

    // The sealer's claim alone.
    let skewed = reached
        .keys()
        .find(|s| {
            s["carry"] == 1
                && s["sealed"] == 1
                && s["claim"] == 2
                && s["seen"] == 0
                && s["stored"] == 1
        })
        .expect("a skewed seal not yet seen here");
    let (sealed_ms, carry, seen, now, covered) = inputs(skewed);
    assert_eq!(
        pass(sealed_ms, carry, seen, now, covered).1,
        Err(Kept::Lapsing)
    );
    let mut early = skewed.clone();
    early.insert("stored", 0);
    early.insert("lost", 1);
    early.insert("seen", 1);
    assert!(!model.successors("Pass", skewed).contains(&early));
}

/// The descriptor a test seals: the first pre-ring claims table.
fn legacy(trust: Trust) -> Descriptor {
    crate::accounts::claims::legacy_descriptors(trust).remove(0)
}

/// Declare and enroll `descriptor` on the lab's first Mac, so it holds storage of it.
fn declare(lab: &Lab, descriptor: &Descriptor) {
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    directory.publish(&session, descriptor).unwrap();
}

/// One release pass on the lab's first Mac at `now` seconds; nothing is restated here, so a
/// restated table counts as not carried.
fn release(lab: &Lab, now: u64) -> Released {
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let snapshot = directory.snapshot(&session).unwrap();
    directory
        .release_sealed(&session, &snapshot, now, &|_, _, _| Ok(false))
        .unwrap()
}

/// A pre-ring claims table's storage stays while it is subscribed, stays for [`LAPSE_S`] after
/// this Mac first sees it sealed, and then goes with its claim, leaving the catalog's
/// declarations; a sealed table this build does not retire stays stored. NEGATIVE CONTROLS:
/// with the first-seen stamp ignored (`pass` given `None` for it), the pass at the seal's first
/// sight plus a grace still reads `Lapsing` forever, and the `seen + LAPSE_S` assertion fails;
/// with `retired` listing the application table too, it goes and `stored(&other)` fails.
#[test]
fn a_lapsed_claims_table_is_released_only_after_its_grace() {
    let lab = Lab::new(&[Role::Writer]);
    let trust = lab.bootstrap(0).trust();
    let directory = Directory::new(&lab.paths[0], trust);
    let claims = legacy(trust);
    let other = Descriptor::application(trust, "aterm-lab-release", "kept", 1).unwrap();
    declare(&lab, &claims);
    declare(&lab, &other);
    let stored = |descriptor: &Descriptor| directory.path(descriptor.scope).unwrap().exists();
    assert!(stored(&claims) && stored(&other));

    // Subscribed: kept.
    let first = release(&lab, 1_000_000);
    assert_eq!(first.kept, vec![(claims.scope.digest(), Kept::NotSealed)]);
    assert!(first.released.is_empty() && stored(&claims));

    // Sealed: the grace starts at this Mac's first sight.
    {
        let bootstrap = lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        assert!(directory.seal(&session, &claims).unwrap());
        assert!(directory.seal(&session, &other).unwrap());
    }
    let now_s = || aterm_time::unix_secs().unwrap();
    let seen = now_s() + LAPSE_S;
    assert_eq!(
        release(&lab, seen).kept,
        vec![(claims.scope.digest(), Kept::Lapsing)]
    );
    assert_eq!(
        release(&lab, seen + LAPSE_S - 1).kept,
        vec![(claims.scope.digest(), Kept::Lapsing)]
    );
    assert!(stored(&claims));
    let done = release(&lab, seen + LAPSE_S);
    assert_eq!(done.released, vec![claims.scope.digest()]);
    assert!(!stored(&claims) && stored(&other));
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    assert!(
        !claim_dir(&session)
            .unwrap()
            .join(claim_name(claims.scope))
            .exists()
    );
    let snapshot = directory.snapshot(&session).unwrap();
    assert!(sealed_at(snapshot.entries.get(&claims.scope.digest()), &claims).is_some());
    drop(session);

    // Nothing left to release.
    assert_eq!(release(&lab, seen + 2 * LAPSE_S), Released::default());
}

/// [`pass`] alone: an unsealed table is never released however old; a lapsing one needs both
/// clocks past the grace; a restated one needs its cover. NEGATIVE CONTROL: dropping the
/// sealer's-claim check releases the third case.
#[test]
fn a_pass_needs_the_seal_both_clocks_and_the_cover() {
    let lapse = Carry::Lapses { grace_s: 10 };
    assert_eq!(
        pass(None, lapse, Some(0), 1_000, true).1,
        Err(Kept::NotSealed)
    );
    assert_eq!(
        pass(None, Carry::Restated, None, 1_000, true),
        (None, Err(Kept::NotSealed))
    );
    // Seen long ago here, but the sealer claims a seal only five seconds old.
    assert_eq!(
        pass(Some(995_000), lapse, Some(0), 1_000, false).1,
        Err(Kept::Lapsing)
    );
    assert_eq!(pass(Some(0), lapse, Some(0), 1_000, false).1, Ok(()));
    // First sight stamps now.
    assert_eq!(
        pass(Some(0), lapse, None, 1_000, false),
        (Some(1_000), Err(Kept::Lapsing))
    );
    assert_eq!(
        pass(Some(0), Carry::Restated, Some(5), 6, false).1,
        Err(Kept::NotCarried)
    );
    assert_eq!(pass(Some(0), Carry::Restated, Some(5), 6, true).1, Ok(()));
}

/// A claims write's pass runs at most once a day by this Mac's clock, and again at once when the
/// clock went back past its stamp; a pass not due reads no table at all. NEGATIVE CONTROL: with
/// `due` always true, the second pass inside the day runs `covered` and the counter assertion
/// fails.
#[test]
fn a_due_pass_runs_at_most_daily() {
    assert!(due(None, 5));
    assert!(!due(Some(100), 100 + RELEASE_EVERY_S - 1));
    assert!(due(Some(100), 100 + RELEASE_EVERY_S));
    assert!(due(Some(100), 99));

    let lab = Lab::new(&[Role::Writer]);
    let trust = lab.bootstrap(0).trust();
    let directory = Directory::new(&lab.paths[0], trust);
    let restated = crate::att::REGISTRY.descriptor(trust).unwrap();
    declare(&lab, &restated);
    {
        let bootstrap = lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        assert!(directory.seal(&session, &restated).unwrap());
    }
    let asked = std::cell::Cell::new(0);
    let pass_at = |now: u64| {
        let bootstrap = lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        let snapshot = directory.snapshot(&session).unwrap();
        directory
            .release_due(&session, &snapshot, now, &|_, _, _| {
                asked.set(asked.get() + 1);
                Ok(false)
            })
            .unwrap()
    };
    let now = 1_000_000;
    let first = pass_at(now).expect("a first pass is due");
    assert_eq!(
        first.kept,
        vec![(restated.scope.digest(), Kept::NotCarried)]
    );
    assert_eq!(asked.get(), 1);
    assert_eq!(pass_at(now + RELEASE_EVERY_S - 1), None);
    assert_eq!(asked.get(), 1);
    assert!(pass_at(now + RELEASE_EVERY_S).is_some());
    assert_eq!(asked.get(), 2);
}
