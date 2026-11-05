// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `settle`, `choose` and `check` held to the machine-number models
//! (`aterm_models_swarm::machine_number_*_model`, `docs/DESIGN-machine-names-2026-11-04.md`
//! §6.6): every reachable state at `Buggy = 0` is built into the records it stands for, and
//! the real functions' answers are the model's successors. Wherever a model determines it,
//! settle's whole standings map is compared, so each standing (`Holds`, `Second`, `Waiting`,
//! `Displaced`, `Unnumbered`) is bound somewhere. Each model's `project_*` is the projection its
//! anchors name, and each test ends in the model's execution-evidence audit and a negative
//! control: a defect the model's mutants stand for, applied to the real input or written out
//! beside it, disagrees with the model in some reachable state.

use std::collections::BTreeMap;

use aterm_spec::derive::Model;
use aterm_spec::interp::{State, reachable_states};
use aterm_spec::xref::StepEvidence;

use super::{
    Anchor, Choice, Class, Entry, Fleet, Holder, Kept, Labels, NodeId, Number, Record, Settled,
    Standing, Verdict, Was, check, choose, renamed, settle, taken, top,
};

/// The anchor of the models' two-node machines.
const SHARED: Anchor = Anchor([7; 8]);

type View = BTreeMap<NodeId, Entry>;

type Standings = BTreeMap<NodeId, Standing>;

fn node(id: i64) -> NodeId {
    NodeId::new(&[u8::try_from(id).expect("a model node id"); 16]).expect("16 bytes")
}

fn id_of(node: NodeId) -> i64 {
    i64::from(node.as_bytes().first().copied().unwrap_or(0))
}

fn num(n: i64) -> Number {
    Number::new(u32::try_from(n).expect("a model number")).expect("a number")
}

fn wide(v: i64) -> u64 {
    u64::try_from(v).expect("a model stamp")
}

fn get(state: &State, key: &str) -> i64 {
    *state
        .get(key)
        .unwrap_or_else(|| panic!("no `{key}` in {state:?}"))
}

/// Node `i`'s variable `field`: `n2`, `s3`.
fn of(state: &State, field: &str, i: i64) -> i64 {
    get(state, &format!("{field}{i}"))
}

fn konst(model: &Model, name: &str) -> i64 {
    model
        .consts
        .iter()
        .find(|(k, _)| *k == name)
        .map_or_else(|| panic!("no constant {name}"), |(_, v)| *v)
}

/// A class rank and since as the record class they stand for: an evidence rank (1–6) not taken
/// is `seeded`.
fn class_of(rank: i64, since: i64, tgen: i64) -> Class {
    match rank {
        1 => Class::Auto,
        _ if since <= 6 && tgen == 0 => Class::Seeded,
        _ => Class::Chosen,
    }
}

fn record(num: Option<Number>, class: Class, since: i64) -> Record {
    Record {
        num,
        class,
        since: wide(since),
        base: None,
        tgen: 0,
        sgen: 0,
        act: wide(since),
        anchor: None,
        prev: None,
        waits: false,
        ephemeral: false,
        was: Vec::new(),
        top: 0,
        cap: None,
    }
}

/// Node `i`'s claim fields the models keep: NAME, class, since, base and take generation.
fn claim(record: &Record, i: i64) -> (Option<Number>, Class, u64, i64, u32) {
    (
        record.num,
        record.class,
        record.since,
        id_of(record.base_or(node(i))),
        record.tgen,
    )
}

fn holder_id(settled: &Settled, n: Number) -> i64 {
    settled.holder(n).map_or(0, id_of)
}

/// Node `i`'s standing on its claim to `n`: holding it when `holder` is `i`, else displaced.
fn on(i: i64, n: Number, holder: i64) -> Standing {
    if holder == i {
        Standing::Holds(n)
    } else {
        Standing::Displaced {
            lost: n,
            to: node(holder),
        }
    }
}

/// A machine's standings, P (1) and Q (2): the primary `pr` holds `n` (unnumbered on 0), and
/// the other node is second.
fn machine_standings(pr: i64, n: i64) -> Standings {
    (1..=2)
        .map(|i| {
            let standing = match (i == pr, n) {
                (false, _) => Standing::Second { of: node(pr) },
                (true, 0) => Standing::Unnumbered,
                (true, n) => Standing::Holds(num(n)),
            };
            (node(i), standing)
        })
        .collect()
}

/// No node has labels in the models but `MachineNumberChoose`.
static LABELS: BTreeMap<NodeId, Labels> = BTreeMap::new();

fn fleet<'a>(view: &'a View, settled: &'a Settled, kept: &'a [Kept]) -> Fleet<'a> {
    Fleet {
        view,
        settled,
        top: top(view, kept, [], 0),
        kept,
        labels: &LABELS,
    }
}

/// The stamp `check` writes a press at: one past every `since` and `act` in view.
fn press_stamp(view: &View) -> u64 {
    view.values()
        .filter_map(|entry| match entry {
            Entry::Record(record) => Some(record.since.max(record.act) + 1),
            Entry::Implied(_) | Entry::Unknown(_) => None,
        })
        .max()
        .unwrap_or(0)
}

/// Every reachable state of `model` at `Buggy = 0`, its count reported.
fn walk(model: &Model) -> Vec<State> {
    let states = reachable_states(model);
    eprintln!(
        "{}: {} reachable states (Buggy=0)",
        model.name,
        states.len()
    );
    states
}

fn complete(ev: &StepEvidence) {
    let audit = ev.audit();
    assert!(audit.is_complete(), "{}", audit.summary());
}

/// Node `i` of `MachineNumberHolder`: unnumbered (`n` 0), on m1 (1), or moved to m(10 + i) with
/// m1 lost (2).
fn holder_record(state: &State, i: i64) -> Record {
    let n = of(state, "n", i);
    let (since, tgen, base) = (of(state, "s", i), of(state, "t", i), of(state, "b", i));
    let mut record = record(
        match n {
            0 => None,
            1 => Some(num(1)),
            _ => Some(num(10 + i)),
        },
        class_of(of(state, "c", i), since, tgen),
        since,
    );
    record.base = (base != i).then(|| node(base));
    record.tgen = u32::try_from(tgen).expect("a take generation");
    if n == 2 {
        record.was = vec![Was {
            num: num(1),
            retired: false,
            lost: true,
        }];
    }
    record
}

/// `MachineNumberHolder`'s three machines. `anchored` makes each a machine of one node under an
/// anchor that sorts in the reverse of its id: settle reads unanchored nodes in id order and
/// anchored machines in anchor order, so this view hands step 3 its claims in descending id
/// order, and a tie broken by input order rather than by node id answers differently on it.
fn holder_view(state: &State, anchored: bool) -> View {
    (1..=3)
        .map(|i| {
            let mut record = holder_record(state, i);
            if anchored {
                record.anchor = Some(Anchor([u8::try_from(10 - i).expect("an anchor byte"); 8]));
            }
            (node(i), Entry::Record(record))
        })
        .collect()
}

/// The view `MachineNumberHolder`'s state stands for.
pub(super) fn project_holder(state: &State) -> View {
    holder_view(state, false)
}

/// The standings `MachineNumberHolder`'s state determines, `ha` holding m1.
fn holder_standings(state: &State, ha: i64) -> Standings {
    (1..=3)
        .map(|i| {
            let standing = match of(state, "n", i) {
                0 => Standing::Unnumbered,
                1 => on(i, num(1), ha),
                _ => Standing::Holds(num(10 + i)),
            };
            (node(i), standing)
        })
        .collect()
}

/// M-a written out: step 3 for m1 with the key lacking the node id, so the first of equal
/// claims in settle's push order over the anchored view (descending id) holds.
fn holder_by_push_order(state: &State) -> i64 {
    let claims: Vec<(i64, (u8, u64, i64), u32)> = (1..=3)
        .rev()
        .filter(|i| of(state, "n", *i) == 1)
        .map(|i| {
            let record = holder_record(state, i);
            let base = id_of(record.base_or(node(i)));
            (i, (record.class.rank(), record.since, base), record.tgen)
        })
        .collect();
    claims
        .iter()
        .filter(|(_, (_, _, base), tgen)| {
            !claims.iter().any(|(_, (_, _, b), t)| b == base && t > tgen)
        })
        .min_by_key(|(_, key, _)| *key)
        .map_or(0, |(i, _, _)| *i)
}

#[test]
fn holder_model_is_check_and_settle() {
    let model = aterm_models_swarm::machine_number_holder_model();
    let (clock_max, t_max) = (konst(&model, "ClockMax"), konst(&model, "TMax"));
    let mut ev = StepEvidence::new("MachineNumberHolder");
    let mut tie_differs = 0;
    for state in &walk(&model) {
        let views = [project_holder(state), holder_view(state, true)];
        if get(state, "phase") == 1 {
            for view in &views {
                let settled = settle(view);
                let ha = holder_id(&settled, num(1));
                for next in model.successors("Settle", state) {
                    assert_eq!((get(&next, "ha"), get(&next, "hb")), (ha, ha), "{state:?}");
                    assert_eq!(settled.standings, holder_standings(&next, ha), "{state:?}");
                }
            }
            continue;
        }
        let ha = get(state, "ha");
        // The negative control: M-a's tie-break, which hands a tie to the claim pushed first.
        tie_differs += usize::from(holder_by_push_order(state) != ha);
        let clock = get(state, "clock");
        let auto = ha != 0 && of(state, "c", ha) == 1;
        let chosen = ha != 0 && of(state, "c", ha) == 0;
        for (anchored, view) in [false, true].into_iter().zip(&views) {
            let settled = settle(view);
            assert_eq!(holder_id(&settled, num(1)), ha, "{anchored} {state:?}");
            assert_eq!(settled.standings, holder_standings(state, ha), "{state:?}");
            let fleet = fleet(view, &settled, &[]);
            for i in 1..=3 {
                let verdict = check("m1", &fleet, node(i));
                if i == ha {
                    assert_eq!(verdict, Verdict::Same, "{state:?}");
                    continue;
                }
                assert_eq!(
                    verdict.renames(),
                    ha == 0 || auto,
                    "{i}: {verdict:?} {state:?}"
                );
                assert_eq!(verdict.takes(), chosen, "{i}: {verdict:?} {state:?}");
            }
        }
        let view = &views[0];
        let settled = settle(view);
        let fleet = fleet(view, &settled, &[]);
        let who = get(state, "who");
        let pressed = |verdict: &Verdict, i: i64| {
            renamed(
                &holder_record(state, i),
                num(1),
                taken(&fleet, verdict),
                wide(clock),
            )
        };
        let room = clock <= clock_max;
        let below = ha != 0 && of(state, "t", ha) < t_max;
        assert_eq!(
            model.action_enabled("Rename", state),
            who != ha && (ha == 0 || auto) && room,
            "{state:?}"
        );
        assert_eq!(
            model.action_enabled("Take", state),
            who != ha && chosen && below,
            "{state:?}"
        );
        assert_eq!(
            model.action_enabled("TakeTwice", state),
            chosen && below,
            "{state:?}"
        );
        for action in ["Rename", "Take"] {
            if !model.action_enabled(action, state) {
                continue;
            }
            let written = ev.step(action, || {
                let verdict = check("m1", &fleet, node(who));
                claim(&pressed(&verdict, who), who)
            });
            for next in model.successors(action, state) {
                assert_eq!(claim(&holder_record(&next, who), who), written, "{action}");
            }
        }
        if model.action_enabled("TakeTwice", state) {
            let takers: Vec<i64> = (1..=3).filter(|i| *i != ha).collect();
            let written: Vec<_> = ev.step("TakeTwice", || {
                takers
                    .iter()
                    .map(|i| {
                        let verdict = check("m1", &fleet, node(*i));
                        assert!(verdict.takes(), "{i}: {verdict:?}");
                        claim(&pressed(&verdict, *i), *i)
                    })
                    .collect()
            });
            for (i, written) in takers.iter().zip(&written) {
                for next in model.successors("TakeTwice", state) {
                    assert_eq!(&claim(&holder_record(&next, *i), *i), written);
                }
            }
        }
    }
    complete(&ev);
    assert!(
        tie_differs > 0,
        "step 3 with ties to the first claim pushed agrees with the model everywhere"
    );
}

/// Node `i` of `MachineNumberEvidence`: an older build's implied record while `o` is 1, else
/// its record.
fn evidence_entry(state: &State, i: i64, implied_since: Option<i64>) -> Entry {
    if i < 3 && of(state, "o", i) == 1 {
        return match implied_since {
            None => Entry::Implied(num(1)),
            Some(since) => Entry::Record(record(Some(num(1)), Class::Seeded, since)),
        };
    }
    Entry::Record(evidence_record(state, i))
}

/// Node `i`'s record in `MachineNumberEvidence`, once it runs a new build.
fn evidence_record(state: &State, i: i64) -> Record {
    let n = of(state, "n", i);
    let (since, tgen, base) = (of(state, "s", i), of(state, "t", i), of(state, "b", i));
    let mut record = record(
        match n {
            0 => None,
            1 => Some(num(1)),
            _ => Some(num(10 + i)),
        },
        class_of(of(state, "c", i), since, tgen),
        since,
    );
    record.base = (base != i).then(|| node(base));
    record.tgen = u32::try_from(tgen).expect("a take generation");
    record
}

/// The view `MachineNumberEvidence`'s state stands for.
pub(super) fn project_evidence(state: &State) -> View {
    (1..=3)
        .map(|i| (node(i), evidence_entry(state, i, None)))
        .collect()
}

/// The standings `MachineNumberEvidence`'s state determines, `h` holding m1.
fn evidence_standings(state: &State, h: i64) -> Standings {
    (1..=3)
        .map(|i| {
            let standing = match of(state, "n", i) {
                0 => Standing::Unnumbered,
                1 => on(i, num(1), h),
                _ => Standing::Holds(num(10 + i)),
            };
            (node(i), standing)
        })
        .collect()
}

/// What `check` says of node 3's press to m1 when `h` holds it: by the holder's build and class.
fn evidence_verdict(state: &State, h: i64) -> Verdict {
    let by = node(h);
    match h {
        0 => Verdict::Free,
        3 => Verdict::Same,
        _ if of(state, "o", h) == 1 => Verdict::HeldByOlderBuild { by },
        _ => match class_of(of(state, "c", h), of(state, "s", h), of(state, "t", h)) {
            Class::Auto => Verdict::TakesAuto { from: by },
            Class::Chosen => Verdict::HeldByPerson { by },
            Class::Seeded => Verdict::HeldSeeded { by },
        },
    }
}

/// Node 3's press is `check`'s, which carries `MachineNumberHolder`'s presses and runs `settle`,
/// the anchor of this model's Settle: it is held outside the windows, its verdict in every
/// settled state (`HeldSeeded` and `HeldByOlderBuild` included) and the record its take writes.
#[test]
fn evidence_model_is_settle() {
    let model = aterm_models_swarm::machine_number_evidence_model();
    let clock_max = konst(&model, "ClockMax");
    let mut ev = StepEvidence::new("MachineNumberEvidence");
    let mut early_differs = 0;
    for state in &walk(&model) {
        let view = project_evidence(state);
        // The negative control: an implied record ranked at since 0.
        let early: View = (1..=3)
            .map(|i| (node(i), evidence_entry(state, i, Some(0))))
            .collect();
        if get(state, "phase") == 0 {
            let settled = settle(&view);
            let h = get(state, "h");
            assert_eq!(holder_id(&settled, num(1)), h, "{state:?}");
            assert_eq!(settled.standings, evidence_standings(state, h), "{state:?}");
            early_differs += usize::from(holder_id(&settle(&early), num(1)) != h);
            let fleet = fleet(&view, &settled, &[]);
            let verdict = check("m1", &fleet, node(3));
            assert_eq!(verdict, evidence_verdict(state, h), "{state:?}");
            let (clock, tk) = (get(state, "clock"), get(state, "tk"));
            assert_eq!(
                model.action_enabled("Take", state),
                verdict.takes() && clock <= clock_max && tk == 0,
                "{verdict:?} {state:?}"
            );
            if model.action_enabled("Take", state) {
                let written = renamed(
                    &evidence_record(state, 3),
                    num(1),
                    taken(&fleet, &verdict),
                    wide(clock),
                );
                for next in model.successors("Take", state) {
                    assert_eq!(claim(&evidence_record(&next, 3), 3), claim(&written, 3));
                }
            }
            continue;
        }
        let settled = ev.step("Settle", || settle(&view));
        let h = holder_id(&settled, num(1));
        let lost = |i: i64| {
            let now = matches!(
                settled.standings.get(&node(i)),
                Some(Standing::Displaced { lost, .. }) if *lost == num(1)
            );
            i64::from(of(state, "l", i) == 1 || now)
        };
        for next in model.successors("Settle", state) {
            assert_eq!(get(&next, "h"), h, "{state:?}");
            assert_eq!((get(&next, "l1"), get(&next, "l2")), (lost(1), lost(2)));
            assert_eq!(settled.standings, evidence_standings(&next, h), "{state:?}");
        }
    }
    complete(&ev);
    assert!(
        early_differs > 0,
        "settle with implied records at since 0 agrees with the model everywhere"
    );
}

/// Node `i` of a two-node machine: its NAME, class, since, base, take and succession
/// generations and `act`, where the model keeps them.
fn group_record(state: &State, i: i64, act: impl Fn(i64) -> i64) -> Record {
    let n = of(state, "n", i);
    let field = |name: &str, or: i64| {
        state
            .get(format!("{name}{i}").as_str())
            .copied()
            .unwrap_or(or)
    };
    let (since, tgen, base) = (of(state, "s", i), field("t", 0), field("b", i));
    let mut record = record(
        (n != 0).then(|| num(n)),
        class_of(of(state, "c", i), since, tgen),
        since,
    );
    record.base = (base != i).then(|| node(base));
    record.tgen = u32::try_from(tgen).expect("a take generation");
    record.sgen = u32::try_from(field("g", 0)).expect("a succession generation");
    record.act = wide(act(of(state, "a", i)));
    record.anchor = Some(SHARED);
    record
}

/// The view `MachineNumberGroup`'s state stands for: P and Q, one machine.
pub(super) fn project_group(state: &State) -> View {
    (1..=2)
        .map(|i| (node(i), Entry::Record(group_record(state, i, |a| a))))
        .collect()
}

/// The NAME node `i`'s machine shows: its primary's.
fn title(view: &View, settled: &Settled, i: i64) -> i64 {
    match settled.primary_of(node(i)).and_then(|p| view.get(&p)) {
        Some(Entry::Record(Record { num: Some(n), .. })) => i64::from(n.get()),
        _ => 0,
    }
}

/// `MachineNumberGroup`'s `rev` is not a field of `Record`, so its re-signings change nothing
/// settle reads here (mutant 2, `BY_REV`, is the model's alone); the re-sign side of
/// `PrimaryStable` is stage 2's, with the signed envelope.
#[test]
fn group_model_is_settle_and_choose() {
    let model = aterm_models_swarm::machine_number_group_model();
    let mut ev = StepEvidence::new("MachineNumberGroup");
    let mut named_tie_differs = 0;
    for state in &walk(&model) {
        let view = project_group(state);
        let phase = get(state, "phase");
        let derived = |settled: &Settled| {
            let unnumbered =
                |i| i64::from(settled.standings.get(&node(i)) == Some(&Standing::Unnumbered));
            [
                settled.primary_of(node(1)).map_or(0, id_of),
                unnumbered(1),
                unnumbered(2),
                title(&view, settled, 1),
                title(&view, settled, 2),
            ]
        };
        let names = ["pr", "u1", "u2", "ti1", "ti2"];
        let standings = |state: &State| {
            let pr = get(state, "pr");
            machine_standings(pr, of(state, "n", pr))
        };
        if phase != 0 {
            let settled = ev.step("Settle", || settle(&view));
            let want = derived(&settled);
            for next in model.successors("Settle", state) {
                assert_eq!(names.map(|k| get(&next, k)), want, "{state:?}");
                assert_eq!(settled.standings, standings(&next), "{state:?}");
            }
            // The negative control: primaries by the latest `act`, where both nodes carry a NAME
            // that ties on class and since, so only the named branch's `act` level decides.
            let tie = (1..=2).all(|i| of(state, "n", i) != 0)
                && of(state, "c", 1) == of(state, "c", 2)
                && of(state, "s", 1) == of(state, "s", 2);
            if tie {
                let latest: View = (1..=2)
                    .map(|i| (node(i), Entry::Record(group_record(state, i, |a| 100 - a))))
                    .collect();
                let latest = settle(&latest).primary_of(node(1)).map_or(0, id_of);
                named_tie_differs += usize::from(latest != want[0]);
            }
        }
        assert!(
            phase != 2 || !model.action_enabled("Pick", state),
            "a second pick on one view: {state:?}"
        );
        if phase != 0 {
            continue;
        }
        let settled = settle(&view);
        assert_eq!(names.map(|k| get(state, k)), derived(&settled), "{state:?}");
        assert_eq!(settled.standings, standings(state), "{state:?}");
        let fleet = fleet(&view, &settled, &[]);
        let picker = if get(state, "u1") == 1 { 1 } else { 2 };
        for i in 1..=2 {
            if model.action_enabled("Pick", state) && i == picker {
                let choice = ev.step("Pick", || choose(&fleet, node(i), None, &[]));
                let Choice::Pick(n) = choice else {
                    panic!("{i} picks {choice:?} in {state:?}");
                };
                for next in model.successors("Pick", state) {
                    assert_eq!(of(&next, "n", i), i64::from(n.get()), "{state:?}");
                }
            } else {
                assert_eq!(
                    choose(&fleet, node(i), None, &[]),
                    Choice::Keep,
                    "{state:?}"
                );
            }
        }
    }
    complete(&ev);
    assert!(
        named_tie_differs > 0,
        "settle naming a primary by the latest act agrees with the model on every tie"
    );
}

/// X, the machine holding m12 in `MachineNumberSuccession`.
fn other_machine() -> Entry {
    Entry::Record(record(Some(num(12)), Class::Chosen, 9))
}

/// The view `MachineNumberSuccession`'s state stands for.
pub(super) fn project_succession(state: &State) -> View {
    let mut view: View = (1..=2)
        .map(|i| (node(i), Entry::Record(group_record(state, i, |a| a))))
        .collect();
    view.insert(node(3), other_machine());
    view
}

/// The standings `MachineNumberSuccession`'s state determines: the primary on its NAME, held
/// by the holder the model stores for it, and X on m12.
fn succession_standings(state: &State) -> Standings {
    let holder = |n: i64| get(state, &format!("h{n}"));
    let pr = get(state, "pr");
    let mut standings = machine_standings(pr, 0);
    let n = of(state, "n", pr);
    if n != 0 {
        standings.insert(node(pr), on(pr, num(n), holder(n)));
    }
    standings.insert(node(3), on(3, num(12), holder(12)));
    standings
}

#[test]
fn succession_model_is_settle() {
    let model = aterm_models_swarm::machine_number_succession_model();
    let mut ev = StepEvidence::new("MachineNumberSuccession");
    let mut unranked_differs = 0;
    let derived = |settled: &Settled| {
        [
            settled.primary_of(node(1)).map_or(0, id_of),
            holder_id(settled, num(8)),
            holder_id(settled, num(12)),
            holder_id(settled, num(20)),
        ]
    };
    let names = ["pr", "h8", "h12", "h20"];
    for state in &walk(&model) {
        let view = project_succession(state);
        if get(state, "phase") == 0 {
            let settled = settle(&view);
            assert_eq!(names.map(|k| get(state, k)), derived(&settled));
            assert_eq!(settled.standings, succession_standings(state), "{state:?}");
            continue;
        }
        let settled = ev.step("Settle", || settle(&view));
        let want = derived(&settled);
        for next in model.successors("Settle", state) {
            assert_eq!(names.map(|k| get(&next, k)), want, "{state:?}");
            assert_eq!(settled.standings, succession_standings(&next), "{state:?}");
        }
        // The negative control: `sgen` left out of every record.
        let unranked: View = view
            .iter()
            .map(|(node, entry)| {
                let mut entry = entry.clone();
                if let Entry::Record(record) = &mut entry {
                    record.sgen = 0;
                }
                (*node, entry)
            })
            .collect();
        unranked_differs += usize::from(derived(&settle(&unranked)) != want);
    }
    complete(&ev);
    assert!(
        unranked_differs > 0,
        "settle without sgen agrees with the model everywhere"
    );
}

/// The view `MachineNumberStripped`'s state stands for, as reader J (3) holds it. `implied`
/// reads a stripped copy that kept its `name=` as an implied record. Currency is this
/// projection's: S's entry is its newest, stripped or not, and the older record J holds behind
/// it (`held`) is never handed to settle, which reads the entries it is given; the layer that
/// keeps only the newest entry per node is stage 2's.
fn stripped_view(state: &State, implied: bool) -> View {
    let strip = get(state, "strip");
    let s = match strip {
        0 => Entry::Record(record(Some(num(1)), Class::Auto, get(state, "ss"))),
        1 if implied => Entry::Implied(num(1)),
        1 => Entry::Unknown(Some(num(1))),
        _ => Entry::Unknown(None),
    };
    let jn = get(state, "jn");
    let mut view = View::new();
    view.insert(node(1), s);
    view.insert(node(2), Entry::Record(record(Some(num(1)), Class::Auto, 9)));
    view.insert(
        node(3),
        Entry::Record(record((jn != 0).then(|| num(jn)), Class::Auto, 12)),
    );
    view
}

/// The view `MachineNumberStripped`'s state stands for.
pub(super) fn project_stripped(state: &State) -> View {
    stripped_view(state, false)
}

/// The entries opaque to J, as the model's variables say: S's stripped copy, and the older node
/// on J's Mac (4) while it is live, its listener stopped or not. This list is the test's, read
/// off the model; the probe and the liveness that build it are stage 2's.
fn opaque_to_reader(state: &State) -> Vec<NodeId> {
    let mut opaque = Vec::new();
    if get(state, "strip") != 0 {
        opaque.push(node(1));
    }
    if get(state, "live") == 1 {
        opaque.push(node(4));
    }
    opaque
}

/// The standings `MachineNumberStripped`'s state determines: S on m1 while its record is in
/// view, V waiting while m1 is unconfirmed and else on m1, and J on what it picked.
fn stripped_standings(state: &State, h: i64) -> Standings {
    let strip = get(state, "strip");
    let mut standings = Standings::new();
    if strip == 0 {
        standings.insert(node(1), on(1, num(1), h));
    }
    let v = if strip == 1 {
        Standing::Waiting(num(1))
    } else {
        on(2, num(1), h)
    };
    standings.insert(node(2), v);
    let jn = get(state, "jn");
    let j = if jn == 0 {
        Standing::Unnumbered
    } else {
        Standing::Holds(num(jn))
    };
    standings.insert(node(3), j);
    standings
}

#[test]
fn stripped_model_is_settle_and_choose() {
    let model = aterm_models_swarm::machine_number_stripped_model();
    let mut ev = StepEvidence::new("MachineNumberStripped");
    let mut implied_differs = 0;
    let derived = |view: &View, settled: &Settled| {
        let h = match settled.holders.get(&num(1)) {
            Some(Holder::Node(holder)) => id_of(*holder),
            _ => 0,
        };
        let displaced = [1, 2]
            .into_iter()
            .filter(|i| {
                matches!(view.get(&node(*i)), Some(Entry::Record(_)))
                    && matches!(
                        settled.standings.get(&node(*i)),
                        Some(Standing::Displaced { .. })
                    )
            })
            .count();
        [h, i64::try_from(displaced).expect("two nodes")]
    };
    for state in &walk(&model) {
        let (view, opaque) = (project_stripped(state), opaque_to_reader(state));
        if get(state, "phase") == 1 {
            let settled = ev.step("Settle", || settle(&view));
            let want = derived(&view, &settled);
            for next in model.successors("Settle", state) {
                assert_eq!([get(&next, "h"), get(&next, "d")], want, "{state:?}");
                assert_eq!(settled.standings, stripped_standings(&next, want[0]));
            }
            // The negative control: a stripped copy read as an implied record.
            let implied = stripped_view(state, true);
            implied_differs += usize::from(derived(&implied, &settle(&implied))[0] != want[0]);
            continue;
        }
        let settled = settle(&view);
        assert_eq!([get(state, "h"), get(state, "d")], derived(&view, &settled));
        let h = get(state, "h");
        assert_eq!(settled.standings, stripped_standings(state, h), "{state:?}");
        if get(state, "jn") != 0 {
            continue;
        }
        let fleet = fleet(&view, &settled, &[]);
        if model.action_enabled("Pick", state) {
            let choice = ev.step("Pick", || choose(&fleet, node(3), None, &opaque));
            let Choice::Pick(n) = choice else {
                panic!("J picks {choice:?} in {state:?}");
            };
            for next in model.successors("Pick", state) {
                assert_eq!(get(&next, "jn"), i64::from(n.get()), "{state:?}");
            }
        } else {
            let choice = choose(&fleet, node(3), None, &opaque);
            assert!(
                matches!(choice, Choice::Defer(_)),
                "{choice:?} in {state:?}"
            );
        }
    }
    complete(&ev);
    assert!(
        implied_differs > 0,
        "settle reading a stripped copy as implied agrees with the model everywhere"
    );
}

/// E (3), on m5 in `MachineNumberChoose`.
fn expiring() -> Record {
    record(Some(num(5)), Class::Auto, 9)
}

/// The view `MachineNumberChoose`'s state stands for: A (1), unnumbered or on m1 from since 10
/// once it contests it; B (2) on m1 from since 8 once joined; E (3) on m5 while live; W (4), a
/// migration that waits keeping m7 or m4 as `prev`; and D (5), an unknown claim naming m4.
pub(super) fn project_choose(state: &State) -> View {
    let hist = get(state, "hist");
    let mut a = if get(state, "disp") == 1 {
        record(Some(num(1)), Class::Auto, 10)
    } else {
        record(None, Class::Auto, 0)
    };
    a.prev = (hist >= 1).then(|| num(4));
    if hist == 1 {
        a.was = vec![Was {
            num: num(4),
            retired: false,
            lost: true,
        }];
    }
    let mut view = View::new();
    view.insert(node(1), Entry::Record(a));
    if get(state, "joined") == 1 {
        view.insert(node(2), Entry::Record(record(Some(num(1)), Class::Auto, 8)));
    }
    if get(state, "e") == 1 {
        view.insert(node(3), Entry::Record(expiring()));
    }
    let wait = get(state, "wait");
    if wait >= 1 {
        let mut w = record(None, Class::Seeded, 2);
        w.waits = true;
        w.prev = Some(num(if wait == 1 { 7 } else { 4 }));
        view.insert(node(4), Entry::Record(w));
    }
    if get(state, "rival") == 2 {
        view.insert(node(5), Entry::Unknown(Some(num(4))));
    }
    view
}

/// The numbers `MachineNumberChoose`'s state keeps out of automatic use: E's tombstone once it
/// expired, and the planned row for m6.
fn choose_kept(state: &State) -> Vec<Kept> {
    let mut kept = Vec::new();
    if get(state, "e") == 2 {
        kept.push(Kept::Tombstone {
            node: node(3),
            last: expiring(),
        });
    }
    if get(state, "row") == 1 {
        kept.push(Kept::Row {
            num: num(6),
            node: None,
            planned: true,
        });
    }
    kept
}

/// D's admission label, m4, while `rival` is 1.
fn choose_labels(state: &State) -> BTreeMap<NodeId, Labels> {
    let mut labels = BTreeMap::new();
    if get(state, "rival") == 1 {
        labels.insert(
            node(5),
            Labels {
                admission: Some(num(4)),
                roster: None,
            },
        );
    }
    labels
}

/// The standings `MachineNumberChoose`'s state determines.
fn choose_standings(state: &State) -> Standings {
    let joined = get(state, "joined") == 1;
    let mut standings = Standings::new();
    let a = match (get(state, "disp"), joined) {
        (0, _) => Standing::Unnumbered,
        (_, true) => on(1, num(1), 2),
        (_, false) => Standing::Holds(num(1)),
    };
    standings.insert(node(1), a);
    if joined {
        standings.insert(node(2), Standing::Holds(num(1)));
    }
    if get(state, "e") == 1 {
        standings.insert(node(3), Standing::Holds(num(5)));
    }
    if get(state, "wait") >= 1 {
        standings.insert(node(4), Standing::Unnumbered);
    }
    standings
}

#[test]
fn choose_model_is_choose() {
    let model = aterm_models_swarm::machine_number_choose_model();
    let mut ev = StepEvidence::new("MachineNumberChoose");
    let mut unkept_differs = 0;
    let answer = |choice: &Choice| match choice {
        Choice::Want(n) => [i64::from(n.get()), 1],
        Choice::Pick(n) => [i64::from(n.get()), 2],
        other => panic!("A needs a number: {other:?}"),
    };
    for state in &walk(&model) {
        if get(state, "pk") == 1 {
            continue;
        }
        let (view, kept, labels) = (
            project_choose(state),
            choose_kept(state),
            choose_labels(state),
        );
        let settled = settle(&view);
        assert_eq!(settled.standings, choose_standings(state), "{state:?}");
        let signed = labels.values().filter_map(|labels| labels.admission);
        let fleet = Fleet {
            view: &view,
            settled: &settled,
            top: top(&view, &kept, signed, 0),
            kept: &kept,
            labels: &labels,
        };
        if get(state, "wait") >= 1 {
            assert_eq!(choose(&fleet, node(4), None, &[]), Choice::Keep);
        }
        if !model.action_enabled("Pick", state) {
            assert_eq!(
                choose(&fleet, node(1), None, &[]),
                Choice::Keep,
                "{state:?}"
            );
            continue;
        }
        let choice = ev.step("Pick", || choose(&fleet, node(1), None, &[]));
        let want = answer(&choice);
        for next in model.successors("Pick", state) {
            assert_eq!([get(&next, "res"), get(&next, "kind")], want, "{state:?}");
        }
        // The negative control: tombstones and rows ignored.
        let unkept = Fleet {
            top: top(&view, &[], [], 0),
            kept: &[],
            ..fleet
        };
        unkept_differs += usize::from(answer(&choose(&unkept, node(1), None, &[])) != want);
    }
    complete(&ev);
    assert!(
        unkept_differs > 0,
        "choose without kept numbers agrees with the model everywhere"
    );
}

/// Node `i` of `MachineNumberPress`: P (1) on m5 or m7, Q (2) unnumbered, on m6, or a
/// migration that waits keeping m6 as `prev`. `unseen` leaves Q's claim out (no NAME, no wait).
fn press_record(state: &State, i: i64, unseen: bool) -> Record {
    let n = of(state, "n", i);
    let since = of(state, "s", i);
    let mut record = record(
        (n != 0).then(|| num(n)),
        class_of(of(state, "c", i), since, 0),
        since,
    );
    record.act = wide(of(state, "a", i));
    record.anchor = Some(SHARED);
    if i == 2 && get(state, "w2") == 1 {
        record.waits = true;
        record.prev = Some(num(6));
    }
    if i == 2 && unseen {
        record.num = None;
        record.waits = false;
        record.prev = None;
    }
    record
}

fn press_view(state: &State, unseen: bool) -> View {
    (1..=2)
        .map(|i| (node(i), Entry::Record(press_record(state, i, unseen))))
        .collect()
}

/// The view `MachineNumberPress`'s state stands for.
pub(super) fn project_press(state: &State) -> View {
    press_view(state, false)
}

/// What `check` says of the primary's press to m7: `Same` once it holds m7, the other node when
/// that node carries a `chosen` or `seeded` claim the press would rank behind, else `Free`.
fn press_verdict(state: &State, pr: i64) -> Verdict {
    let other = 3 - pr;
    let carries = of(state, "n", other) != 0 || (other == 2 && get(state, "w2") == 1);
    if of(state, "n", pr) == 7 {
        Verdict::Same
    } else if carries && of(state, "c", other) == 0 {
        Verdict::OtherAccountNamed { node: node(other) }
    } else {
        Verdict::Free
    }
}

/// `check` carries this model's Rename and runs `settle` inside it, so the model's Settle is
/// compared outside the windows.
#[test]
fn press_model_is_check() {
    let model = aterm_models_swarm::machine_number_press_model();
    let mut ev = StepEvidence::new("MachineNumberPress");
    let mut unseen_differs = 0;
    for state in &walk(&model) {
        let view = project_press(state);
        let settled = settle(&view);
        let pr = settled.primary_of(node(1)).map_or(0, id_of);
        if get(state, "phase") == 1 {
            let holds = settled.standings.get(&node(pr)) == Some(&Standing::Holds(num(7)));
            let lost = get(state, "lost") == 1 || (get(state, "pressed") == 1 && !holds);
            for next in model.successors("Settle", state) {
                assert_eq!(
                    [get(&next, "pr"), get(&next, "lost")],
                    [pr, i64::from(lost)],
                    "{state:?}"
                );
                assert_eq!(settled.standings, machine_standings(pr, of(&next, "n", pr)));
            }
            continue;
        }
        assert_eq!(get(state, "pr"), pr, "{state:?}");
        assert_eq!(settled.standings, machine_standings(pr, of(state, "n", pr)));
        let fleet = fleet(&view, &settled, &[]);
        let expected = press_verdict(state, pr);
        assert_eq!(
            check("m7", &fleet, node(3 - pr)),
            Verdict::SecondNode { of: node(pr) },
            "{state:?}"
        );
        assert_eq!(
            model.action_enabled("Rename", state),
            expected.renames(),
            "{expected:?} {state:?}"
        );
        // The negative control: P's press checked blind to Q's claim, which only the simulated
        // settle ranks before the press.
        if pr == 1 {
            let unseen = press_view(state, true);
            let unseen_settled = settle(&unseen);
            let blind = Fleet {
                view: &unseen,
                settled: &unseen_settled,
                top: top(&unseen, &[], [], 0),
                ..fleet
            };
            let blind = check("m7", &blind, node(1));
            unseen_differs += usize::from(blind != expected);
        }
        if !model.action_enabled("Rename", state) {
            assert_eq!(check("m7", &fleet, node(pr)), expected, "{state:?}");
            continue;
        }
        let clock = get(state, "clock");
        assert_eq!(press_stamp(&view), wide(clock), "{state:?}");
        let (verdict, written) = ev.step("Rename", || {
            let verdict = check("m7", &fleet, node(pr));
            let written = renamed(
                &press_record(state, pr, false),
                num(7),
                taken(&fleet, &verdict),
                wide(clock),
            );
            (verdict, written)
        });
        assert_eq!(verdict, expected, "{state:?}");
        for next in model.successors("Rename", state) {
            let after = press_record(&next, pr, false);
            assert_eq!(
                (claim(&after, pr), after.act),
                (claim(&written, pr), written.act)
            );
        }
    }
    complete(&ev);
    assert!(
        unseen_differs > 0,
        "check blind to the other node's claim agrees with the model everywhere"
    );
}
