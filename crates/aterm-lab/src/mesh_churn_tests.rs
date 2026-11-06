// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The churn lab's pure parts: its arguments, its plan, the batch prefix a kill leaves, and the
//! judgement, each with the evidence that must fail it.

use super::*;

fn words(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_owned).collect()
}

fn recipient() -> Recipient {
    aterm_secrets::age::Identity::generate()
        .expect("an identity")
        .recipient()
}

#[test]
fn parse_takes_the_design_defaults() {
    let args = parse(&[], 7).expect("no arguments parse");
    assert_eq!(args.n, 30);
    assert_eq!(args.minutes, 5);
    assert_eq!(args.seed, 7);
    assert_eq!(args.control, None);
    assert!(!args.record && !args.keep && args.label.is_none());
}

#[test]
fn parse_reads_every_flag() {
    let args = parse(
        &words("--n 14 --seed 42 --minutes 2 --aterm /x/aterm --keep --record --label first"),
        0,
    )
    .expect("they parse");
    assert_eq!(args.n, 14);
    assert_eq!(args.seed, 42);
    assert_eq!(args.minutes, 2);
    assert_eq!(args.aterm.as_deref(), Some("/x/aterm"));
    assert!(args.keep && args.record);
    assert_eq!(args.label.as_deref(), Some("first"));
}

#[test]
fn parse_refuses_what_the_lab_cannot_run() {
    for line in [
        "--n 13",
        "--n 65",
        "--n many",
        "--minutes 0",
        "--minutes 31",
        "--seed -1",
        "--label Upper",
        "--label -edge",
        "--negative-control no-inventory-on-gap",
        "--negative-control reused-incarnation",
        "--negative-control nothing",
        "--frobnicate",
        "--n",
    ] {
        let refused = parse(&words(line), 0).expect_err(line);
        assert_eq!(refused.code, 2, "{line} is a usage error");
    }
}

/// Each control the lab runs parses to itself, and its world's environment names it.
/// NEGATIVE CONTROL: putting back the refusal that waited for A3 fails the parse.
#[test]
fn parse_takes_each_control_the_lab_runs_into_every_macs_world() {
    for control in CONTROLS {
        let args =
            parse(&words(&format!("--negative-control {}", control.name)), 0).expect(control.name);
        assert_eq!(args.control, Some(*control));
        assert_eq!(
            control_env(args.control),
            Some((NEGATIVE_CONTROL.to_owned(), control.name.to_owned()))
        );
    }
    assert_eq!(control_env(None), None);
}

#[test]
fn every_control_breaks_a_check_the_judgement_makes() {
    let names: BTreeSet<&str> = judge(&Evidence::default()).iter().map(|c| c.name).collect();
    for control in CONTROLS {
        assert!(names.contains(control.breaks), "{}", control.name);
    }
}

#[test]
fn a_plan_gives_every_role_its_own_mac() {
    for n in [MIN_N, 20, 30, MAX_N] {
        for seed in 0..40 {
            let p = plan(n, seed);
            let roles = [
                p.removals[0].0,
                p.removals[0].1,
                p.removals[1].0,
                p.removals[1].1,
                p.sleepers[0],
                p.sleepers[1],
                p.restarts[0],
                p.restarts[1],
                p.split_remover,
                p.split_removed,
                p.writer,
                p.admitter,
                p.rebuilt,
            ];
            let distinct: BTreeSet<usize> = roles.iter().copied().collect();
            assert_eq!(distinct.len(), roles.len(), "n {n} seed {seed}");
            assert!(!distinct.contains(&0), "m00 founds and has no role");
            assert!(roles.iter().all(|&r| r < n));
            assert_eq!(p.newcomer, n);
            assert_eq!(p.islands.len(), n + 1);
            assert_eq!(p.islands[p.split_remover], Island::A);
            assert_eq!(p.islands[p.split_removed], Island::A);
            for b in [p.writer, p.admitter, p.rebuilt, p.newcomer] {
                assert_eq!(p.islands[b], Island::B, "n {n} seed {seed}");
            }
            let a = p.on(Island::A).len();
            let b = p.on(Island::B).len() - 1;
            assert!(a.abs_diff(b) <= 1, "n {n} seed {seed}: {a} against {b}");
        }
    }
}

#[test]
fn a_seed_repeats_its_plan() {
    assert_eq!(plan(30, 99), plan(30, 99));
    assert_ne!(plan(30, 99), plan(30, 100));
}

/// The cut is at the `[everyone]` epoch wherever the batch rotates it, and stops short of the
/// admin's `Wrap` of it.
/// NEGATIVE CONTROLS: a cut at the batch's first `Epoch` (the lab's before) leaves the second
/// case's ring epoch and half its `Wrap`s, and no `[everyone]` epoch at all (3, not 7); a cut
/// that ignores the admin delivers its `Wrap` (4, not 3).
#[test]
fn a_kill_mid_batch_leaves_the_everyone_epoch_and_half_its_wraps() {
    use Kind::{Epoch, Put, Remove, Wrap};
    let (e, ring) = ((Epoch, true), (Epoch, false));
    let (r, w, p) = ((Remove, false), (Wrap, false), (Put, false));
    assert_eq!(
        prefix_len(&[r, e, w, w, w, w, p, ring, w], None),
        4,
        "Remove, Epoch, two of four Wraps"
    );
    assert_eq!(
        prefix_len(&[r, ring, w, w, e, w, w, w, w], None),
        7,
        "the ring epoch whole, then the [everyone] epoch and two of its four Wraps"
    );
    assert_eq!(
        prefix_len(&[r, e, w, w, w, w], Some(3)),
        3,
        "short of the admin's Wrap, the second"
    );
    assert_eq!(
        prefix_len(&[r, e, w, w, w, w], Some(5)),
        4,
        "an admin past the half changes nothing"
    );
    assert_eq!(
        prefix_len(&[r, e, w, w], Some(0)),
        3,
        "a record before the epoch is no admin Wrap of it"
    );
    assert_eq!(prefix_len(&[r, e, w], None), 2);
    assert_eq!(prefix_len(&[r, e], None), 2);
    assert_eq!(
        prefix_len(&[r, ring, w, w], None),
        2,
        "no [everyone] epoch: half the batch"
    );
    assert_eq!(prefix_len(&[r, p, p, p], None), 2);
    assert_eq!(prefix_len(&[], None), 0);
}

#[test]
fn record_arms_name_what_differs_from_the_default() {
    let mut args = parse(&[], 3).expect("parses");
    assert_eq!(record_arm(&args).0, "");
    args.n = 14;
    args.control = CONTROLS.first().copied();
    args.label = Some("first".to_owned());
    let (arm, flags) = record_arm(&args);
    assert_eq!(arm, "-n14-nc-old-gap-rule-first");
    assert!(flags.contains("--n 14") && flags.contains("--negative-control old-gap-rule"));
    assert!(flags.contains("--seed 3"));
}

/// A record names the tree its run began on, and says where the checkout was when it was written
/// once that moved: a commit landed mid-run is not what the run's image ran.
/// NEGATIVE CONTROL: a `moved_note` that names nothing, a record that names only the tree its run
/// began on, fails the moved case.
#[test]
fn a_record_names_the_tree_its_run_began_on() {
    let began = Tree {
        commit: "6230b8acdb".to_owned(),
        dirty: false,
    };
    assert_eq!(moved_note(&began, &began.clone()), "");
    let moved = Tree {
        commit: "4edb66c12f".to_owned(),
        dirty: false,
    };
    assert_eq!(
        moved_note(&began, &moved),
        " (the checkout was at `4edb66c12f` when this was written)"
    );
    let edited = Tree {
        dirty: true,
        ..began.clone()
    };
    assert_eq!(
        moved_note(&began, &edited),
        " (the checkout was at `6230b8acdb` with uncommitted changes when this was written)"
    );
}

/// A run in which every check holds: two current members, three removed.
fn holding() -> Evidence {
    let (r1, r2, r3) = (recipient(), recipient(), recipient());
    let head = [7u8; 32];
    let epoch = [9u8; 32];
    let late = digest(b"late");
    let honest = digest(b"honest");
    let node = [5u8; 32];
    let mac = |name: &str| Seen {
        name: name.to_owned(),
        has_log: true,
        heads: vec![head],
        records: 40,
        held: [head].into_iter().collect(),
        genesis: Some([1u8; 32]),
        vault: Some((VAULT.to_owned(), false)),
        recipient: Some(recipient()),
        current: true,
        seated: BTreeSet::new(),
        newest: vec![epoch],
        newest_wrap: BTreeSet::new(),
        opens_newest: true,
        unsettled: Vec::new(),
        seats: Some(["m1", "m2"].into_iter().map(str::to_owned).collect()),
        node_seats: [(node, 1)].into_iter().collect(),
        entries: [(SPLIT_ENTRY.to_owned(), honest)].into_iter().collect(),
        up: true,
        vault_state: "in".to_owned(),
    };
    let mut gone = mac("m09");
    gone.current = false;
    let end = vec![mac("m01"), mac("m02"), gone];
    // The prefix wrapped m01; m02 was in its gap, which a heal Wrap closed.
    let (to_m01, to_m02) = (
        end[0].recipient.expect("m01's recipient"),
        end[1].recipient.expect("m02's recipient"),
    );
    let wrote = |entry: &str, digest: [u8; 32], record: u8| Wrote {
        entry: entry.to_owned(),
        digest,
        record: Some([record; 32]),
        unheld: Vec::new(),
    };
    Evidence {
        steps: vec![Step {
            name: "bring-up".to_owned(),
            ok: true,
            settled: true,
            ms: 10,
            note: String::new(),
        }],
        current: [("m01", "m1"), ("m02", "m2")]
            .into_iter()
            .map(|(a, b)| (a.to_owned(), b.to_owned()))
            .collect(),
        removed: [("m09", r1), ("m10", r2), ("m11", r3)]
            .into_iter()
            .map(|(a, r)| (a.to_owned(), r))
            .collect(),
        healed: end.clone(),
        end,
        most_wraps: (1, String::new()),
        wrap_histogram: [(1, 80)].into_iter().collect(),
        late: Some(wrote(LATE_ENTRY, late, 3)),
        honest: Some(wrote(SPLIT_ENTRY, honest, 4)),
        read_ups: vec![ReadUp {
            mac: "m01".to_owned(),
            how: "slept",
            ms: Some(900),
        }],
        rebuilt: Some(Rebuilt {
            mac: "m02".to_owned(),
            path: "proof".to_owned(),
            refused: Vec::new(),
            node: Some(node),
        }),
        newcomer: Some("m01".to_owned()),
        split: Split {
            read: true,
            crossed: Vec::new(),
        },
        partial: Partial {
            epoch: Some(epoch),
            delivered: [to_m01].into_iter().collect(),
            reached_ms: 1_000,
            heal_wraps: vec![(
                "m01".to_owned(),
                to_m02,
                1_000 + client::SETTLE_GRACE_MS + 61_000,
            )],
            newer: Vec::new(),
            read: true,
        },
        control: None,
    }
}

/// `ev` with one more heal `Wrap` to m02, signed `after` milliseconds past its prefix's posting.
fn healed_after(ev: &mut Evidence, after: u64) {
    let at = ev.partial.reached_ms + after;
    let to = ev.healed[1].recipient.expect("m02's recipient");
    ev.partial.heal_wraps.push(("m01".to_owned(), to, at));
}

fn failing(ev: &Evidence) -> Vec<&'static str> {
    judge(ev)
        .into_iter()
        .filter(|c| !c.pass)
        .map(|c| c.name)
        .collect()
}

#[test]
fn a_holding_run_holds_every_check() {
    let ev = holding();
    assert_eq!(failing(&ev), Vec::<&str>::new());
    assert!(verdict(&judge(&ev), None));
}

#[test]
fn each_check_fails_on_the_evidence_it_names() {
    type Break = fn(&mut Evidence);
    let cases: Vec<(&str, Break)> = vec![
        (ISOLATED, |ev| {
            ev.end[0].vault = Some(("alab".to_owned(), false))
        }),
        (ISOLATED, |ev| {
            ev.end[1].vault = Some((VAULT.to_owned(), true));
        }),
        (STEPS, |ev| ev.steps[0].ok = false),
        (SETTLED, |ev| ev.steps[0].settled = false),
        (FRONTIER, |ev| ev.end[1].heads = vec![[8u8; 32]]),
        (FRONTIER, |ev| {
            ev.end.remove(1);
        }),
        (NEWEST_KEY, |ev| ev.healed[0].opens_newest = false),
        (GAP_HEALED, |ev| ev.partial.heal_wraps.clear()),
        (GAP_HEALED, |ev| ev.partial.delivered.clear()),
        (GAP_HEALED, |ev| ev.partial.epoch = None),
        (GAP_HEALED, |ev| {
            ev.partial.newer = vec![("08080808 by m00".to_owned(), 0)];
        }),
        (GAP_HEALED, |ev| {
            let to = ev.healed[0].recipient.expect("m01's recipient");
            ev.partial.delivered = [to, recipient()].into_iter().collect();
            ev.partial.heal_wraps[0].1 = to;
        }),
        (VAULT_IN, |ev| ev.end[1].vault_state = "revoked".to_owned()),
        (WRAP_BOUND, |ev| {
            ev.most_wraps = (3, "epoch 09 to x".to_owned())
        }),
        (PREFIX_HELD, |ev| healed_after(ev, 12_300)),
        (PREFIX_HELD, |ev| {
            healed_after(ev, client::SETTLE_GRACE_MS - 1)
        }),
        (REMOVED_NO_KEY, |ev| {
            let r = ev.removed["m10"];
            ev.end[0].newest_wrap.insert(r);
        }),
        (REMOVED_NO_KEY, |ev| {
            let code = client::join_code(&ev.removed["m11"]);
            ev.end[1].seated.insert(code);
        }),
        (REMOVED_NO_KEY, |ev| ev.end[2].current = true),
        (POST_CUTOFF_VOID, |ev| {
            let late = ev.late.clone().expect("a late write");
            ev.end[1].entries.insert(late.entry, late.digest);
        }),
        (POST_CUTOFF_VOID, |ev| {
            ev.end[0].entries.clear();
        }),
        (POST_CUTOFF_VOID, |ev| {
            if let Some(late) = ev.late.as_mut() {
                late.unheld.push("m02".to_owned());
            }
        }),
        (POST_CUTOFF_VOID, |ev| {
            if let Some(honest) = ev.honest.as_mut() {
                honest.record = None;
            }
        }),
        (SEAT_VIEW, |ev| {
            if let Some(seats) = ev.end[0].seats.as_mut() {
                seats.insert("m9".to_owned());
            }
        }),
        (SEAT_VIEW, |ev| ev.end[1].seats = None),
        (READ_UP, |ev| ev.read_ups[0].ms = None),
        (SPLIT_HELD, |ev| {
            ev.split.crossed.push("a removal on B".to_owned())
        }),
    ];
    for (check, breaks) in cases {
        let mut ev = holding();
        breaks(&mut ev);
        let failed = failing(&ev);
        assert!(failed.contains(&check), "{check} held: {failed:?}");
    }
}

#[test]
fn the_key_assertion_reads_the_heal_not_the_end() {
    let mut ev = holding();
    ev.end[0].opens_newest = false;
    assert!(
        !failing(&ev).contains(&NEWEST_KEY),
        "the end read, after the remover returned, is no evidence of the heal"
    );
    let mut ev = holding();
    ev.healed[0].opens_newest = false;
    ev.healed[0].up = false;
    assert!(
        !failing(&ev).contains(&NEWEST_KEY),
        "a Mac down at the heal (the remover) is not judged by it"
    );
    let mut ev = holding();
    ev.healed.clear();
    let key = judge(&ev).into_iter().find(|c| c.name == NEWEST_KEY);
    assert!(
        key.is_some_and(|c| !c.ran && !c.pass),
        "no heal read, no judgement"
    );
}

#[test]
fn admission_fails_on_a_second_seat_or_an_absent_mac() {
    let mut ev = holding();
    ev.end[0].node_seats.insert([5u8; 32], 2);
    assert!(failing(&ev).contains(&ADMITTED));
    let mut ev = holding();
    ev.end[1].current = false;
    assert!(failing(&ev).contains(&ADMITTED));
    let mut ev = holding();
    ev.newcomer = None;
    assert!(failing(&ev).contains(&ADMITTED));
}

#[test]
fn nothing_read_holds_nothing_and_judges_nothing() {
    let checks = judge(&Evidence::default());
    for check in [
        ISOLATED,
        STEPS,
        SETTLED,
        FRONTIER,
        NEWEST_KEY,
        GAP_HEALED,
        VAULT_IN,
        WRAP_BOUND,
        PREFIX_HELD,
        REMOVED_NO_KEY,
        POST_CUTOFF_VOID,
        SEAT_VIEW,
        READ_UP,
        ADMITTED,
        SPLIT_HELD,
    ] {
        let c = checks.iter().find(|c| c.name == check);
        assert!(
            c.is_some_and(|c| !c.pass && !c.ran),
            "{check} held, or judged, on no evidence"
        );
    }
}

/// The heal is judged by the `Wrap`s that closed its gap, not by which epoch is newest once its
/// wait ended: an epoch a writer mints after the heal's first `Wrap` (the admin's envelope
/// mirror, once its own heal `Wrap` lets it open the partial epoch) supersedes the rest of the
/// gap, and the remover, down, is no member the gap must reach.
/// NEGATIVE CONTROLS: the same epoch minted before the heal's first `Wrap` covered the gap, and
/// left-out members no heal `Wrap` reached with no newer epoch were skipped.
#[test]
fn a_heal_is_judged_by_its_wraps_not_by_the_newest_epoch() {
    let mut ev = holding();
    let first = ev.partial.heal_wraps[0].2;
    ev.partial.delivered.clear();
    ev.partial.newer = vec![("08080808 by m00".to_owned(), first + 5_000)];
    assert!(
        !failing(&ev).contains(&GAP_HEALED),
        "an epoch after the heal's first Wrap supersedes the rest of its gap"
    );
    ev.partial.newer[0].1 = first - 1;
    assert!(
        failing(&ev).contains(&GAP_HEALED),
        "CONTROL: an epoch before the heal's first Wrap covered the gap"
    );
    ev.partial.newer.clear();
    assert!(
        failing(&ev).contains(&GAP_HEALED),
        "CONTROL: m01, left out, unhealed and superseded by nothing"
    );
    let mut down = holding();
    down.partial.heal_wraps.clear();
    down.healed[1].up = false;
    let c = judge(&down).into_iter().find(|c| c.name == GAP_HEALED);
    assert!(
        c.is_some_and(|c| !c.ran),
        "a member down at the heal is not in its gap, which leaves none here to judge"
    );
}

/// A heal `Wrap` at the settle grace's end holds the prefix; the heal is judged by what it read
/// once its wait ended, and the Wrap bound alone cannot tell a heal from the prefix apart.
#[test]
fn the_prefix_hold_reads_when_the_heal_signed() {
    let mut ev = holding();
    healed_after(&mut ev, client::SETTLE_GRACE_MS);
    assert!(!failing(&ev).contains(&PREFIX_HELD), "at the grace's end");
    let mut ev = holding();
    healed_after(&mut ev, 2_000);
    ev.most_wraps = (2, String::new());
    let failed = failing(&ev);
    assert!(
        failed.contains(&PREFIX_HELD) && !failed.contains(&WRAP_BOUND),
        "a heal from the prefix leaves two Wraps a gap, as one after the grace does: {failed:?}"
    );
    let mut ev = holding();
    ev.partial.read = false;
    for check in [GAP_HEALED, PREFIX_HELD] {
        let c = judge(&ev).into_iter().find(|c| c.name == check);
        assert!(
            c.is_some_and(|c| !c.ran && !c.pass),
            "{check} judged unread"
        );
    }
}

#[test]
fn a_control_bites_only_on_its_evidence_in_a_sound_run() {
    let control = CONTROLS[1];
    assert_eq!(control.breaks, PREFIX_HELD);
    let bitten = || {
        let mut ev = holding();
        healed_after(&mut ev, 5_000);
        ev
    };
    assert!(
        !verdict(&judge(&holding()), Some(control)),
        "a control that did not bite"
    );
    assert!(verdict(&judge(&bitten()), Some(control)));
    assert!(!verdict(&judge(&bitten()), None));
    type Spoil = fn(&mut Evidence);
    let spoiled: [(&str, Spoil); 4] = [
        ("its check failed for want of evidence", |ev| {
            ev.partial.heal_wraps.clear();
        }),
        ("an act did not run", |ev| ev.steps[0].ok = false),
        ("the split did not hold", |ev| {
            ev.split.crossed.push("x".to_owned())
        }),
        ("a Mac was on ALab's vault", |ev| {
            ev.end[0].vault = Some(("alab".to_owned(), false))
        }),
    ];
    for (why, spoil) in spoiled {
        let mut ev = bitten();
        spoil(&mut ev);
        assert!(!verdict(&judge(&ev), Some(control)), "bit though {why}");
    }
    let mut ev = bitten();
    ev.steps[0].settled = false;
    assert!(
        verdict(&judge(&ev), Some(control)),
        "a wait the control stalls does not spoil its bite"
    );
    let gap = CONTROLS[0];
    let mut ev = holding();
    ev.healed[1].opens_newest = false;
    assert!(
        verdict(&judge(&ev), Some(gap)),
        "{} bites the heal",
        gap.name
    );
}

#[test]
fn a_step_records_acts_and_waits_apart() {
    let t = Timed::now();
    let done = t.went("x", Ok(None), "did");
    assert!(done.ok && done.settled);
    let stalled = t.went("x", Ok(Some("no frontier".to_owned())), "did");
    assert!(stalled.ok && !stalled.settled);
    let broke = t.went("x", Err("no act".to_owned()), "did");
    assert!(!broke.ok && broke.settled);
}

#[test]
fn a_down_process_or_an_interrupt_breaks_a_step_and_only_a_hang_leaves_it_unsettled() {
    let t = Timed::now();
    let step = |wait| t.went("x", waited(wait), "did");
    let settled = step(Ok(7));
    assert!(settled.ok && settled.settled);
    let hung = step(Err(Unsettled::Hung("no frontier".to_owned())));
    assert!(
        hung.ok && !hung.settled,
        "every act ran; the wait never settled"
    );
    let down = step(Err(Unsettled::Down(
        "m03's subscriber is not running".to_owned(),
    )));
    assert!(!down.ok, "a run whose process died did not run its step");
    let stopped = step(Err(Unsettled::Stopped));
    assert!(!stopped.ok, "an interrupt is no unsettled wait");
}

#[test]
fn settled_wants_one_frontier_and_the_newest_key() {
    let ev = holding();
    let none = BTreeMap::new();
    let refs: Vec<&Seen> = ev.end.iter().take(2).collect();
    assert_eq!(settled(&refs, Need::Key(&none)), None);
    assert!(
        settled(&refs, Need::Key(&ev.removed)).is_none(),
        "no newest epoch wraps a removed Mac"
    );
    let mut wrapped = ev.end[0].clone();
    wrapped.newest_wrap.insert(ev.removed["m09"]);
    assert!(settled(&[&wrapped, &ev.end[1]], Need::Key(&ev.removed)).is_some());
    let nothing = BTreeSet::new();
    assert_eq!(
        settled(&[&wrapped, &ev.end[1]], Need::Holds(&nothing)),
        None,
        "a frontier wait asks nothing of the key"
    );
    let mut other = ev.end[1].clone();
    other.heads = vec![[3u8; 32]];
    assert!(settled(&[&ev.end[0], &other], Need::Holds(&nothing)).is_some());
    let mut keyless = ev.end[1].clone();
    keyless.opens_newest = false;
    assert!(settled(&[&ev.end[0], &keyless], Need::Key(&none)).is_some());
    assert_eq!(
        settled(&[&ev.end[0], &keyless], Need::Holds(&nothing)),
        None
    );
    let mut logless = ev.end[1].clone();
    logless.has_log = false;
    assert!(settled(&[&ev.end[0], &logless], Need::Holds(&nothing)).is_some());
}

#[test]
fn an_island_wait_needs_what_reached_the_island_not_only_one_frontier() {
    let ev = holding();
    let reached: BTreeSet<RecordId> = [[9u8; 32]].into_iter().collect();
    let (a, b) = (ev.end[0].clone(), ev.end[1].clone());
    assert!(
        settled(&[&a, &b], Need::Holds(&reached)).is_some(),
        "one frontier no Mac has moved from yet is no spread"
    );
    let (mut a, mut b) = (a, b);
    a.held.insert([9u8; 32]);
    assert!(settled(&[&a, &b], Need::Holds(&reached)).is_some());
    b.held.insert([9u8; 32]);
    assert_eq!(settled(&[&a, &b], Need::Holds(&reached)), None);
}

#[test]
fn rng_is_splitmix64() {
    // SplitMix64's first output from seed 0 (Vigna's reference).
    assert_eq!(Rng::new(0).next(), 0xe220_a839_7b1d_cdaf);
}

#[test]
fn a_wait_ends_when_nothing_moved_past_the_schedule_or_as_a_livelock_while_moving() {
    let start = Instant::now();
    let schedule = Duration::from_secs(30);
    let at = |s: u64| start + Duration::from_secs(s);
    let past = 30 + HANG.as_secs();
    assert_eq!(
        ended(at(past - 1), start, start, schedule),
        None,
        "the schedule and the detector run first"
    );
    assert_eq!(ended(at(past), start, start, schedule), Some(Ended::Still));
    assert_eq!(
        ended(at(past), start, at(past - 1), schedule),
        None,
        "a move a second ago resets the detector"
    );
    let livelock = past + LIVELOCK.as_secs();
    assert_eq!(
        ended(at(livelock - 1), start, at(livelock - 2), schedule),
        None
    );
    assert_eq!(
        ended(at(livelock), start, at(livelock - 1), schedule),
        Some(Ended::Livelock),
        "moving on every read past the bound is a livelock"
    );
}

/// A settled wait's per-Mac line names the Macs whose reads changed last, latest first, each
/// with its time from what the wait counts from, and how many read a newest epoch as a prefix:
/// the line that named the Macs holding the ten-minute heal (one-mesh §11 H1).
#[test]
fn last_settled_names_the_macs_that_held_the_wait() {
    let from = std::time::Instant::now();
    let at = |s: u64| from + std::time::Duration::from_secs(s);
    let seen = |name: &str, prefix: bool| Seen {
        name: name.to_owned(),
        unsettled: if prefix { vec![[3u8; 32]] } else { Vec::new() },
        ..Seen::default()
    };
    let (a, b, c) = (seen("m01", false), seen("m02", true), seen("m03", false));
    let line = last_settled(&[&a, &b, &c], &[at(14), at(652), at(14)], from);
    assert_eq!(
        line,
        "last settled m02 652.0s, m01 14.0s, m03 14.0s; 1 of 3 read a newest [everyone] epoch as a prefix"
    );
}
