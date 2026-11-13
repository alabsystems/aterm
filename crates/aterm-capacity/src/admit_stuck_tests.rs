// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! STUCK waiters (CAP3, ATERM-KI-1111-25): a waiter of memory that no claim's end can start
//! reserves none of it against the claims behind it, while one that some holder's end would start
//! keeps its place. Each claim is another process's record, as the ledger reads them, so no
//! waiter is exempt from another's turn as its own process's would be.

use super::*;
use crate::Rank;

/// The look of a claim that asked after them all, reading memory the way the measured Mac did
/// (KI-1111-25, m16, 2026-11-11): 128 GiB, of which another program held so much that 35 GiB
/// stayed available.
fn measured() -> Reading {
    Reading::of_gib(128, 35, 18)
}

/// Another process's claim: `pid`, asked at `asks` (ms), held or waiting, needing `need`.
fn claim(pid: u32, asks: u64, need: Need, held: bool) -> Record {
    Record {
        timing: crate::TimingEvidence::default(),
        accounting: crate::Accounting::Holder,
        pid,
        start_ms: asks,
        seq: 0,
        checkout: "/w".to_owned(),
        need,
        held: held.then_some(Held {
            admitted_ms: asks,
            cores: 0,
            alone: false,
        }),
        credit: Vec::new(),
        wrote: None,
        next_ms: None,
    }
}

/// The release qualify's slice budget as `aterm-release` asks it: Release rank, and a margin of
/// a byte so that memory no claim holds never turns its shortage into ALONE admission.
fn slice_budget(gib: u64) -> Need {
    Need::new("release slice memory budget")
        .rank(Rank::Release)
        .mem_gib(gib)
        .margin(1)
}

/// The process sweep of a look that read every claimant's tree, each using what `uses` names
/// for its pid and nothing else.
fn sweep(records: &[Record], uses: &[(u32, u64)]) -> HashMap<u32, TreeUse> {
    records
        .iter()
        .map(|record| {
            let bytes = uses
                .iter()
                .find(|(pid, _)| *pid == record.pid)
                .map_or(0, |(_, bytes)| *bytes);
            let used = TreeUse {
                charged: bytes,
                footprint: bytes,
                written: 0,
            };
            (record.pid, used)
        })
        .collect()
}

/// `records` as a look with `trees` takes them ([`others`], as `Waiter::poll` builds them).
fn as_seen<'r>(records: &'r [Record], trees: &HashMap<u32, TreeUse>) -> Vec<Other<'r>> {
    let records: Vec<&Record> = records.iter().collect();
    others(&records, records.iter().map(|r| r.pid).collect(), trees)
}

/// The look of `asker` behind `records` on `reading` with `trees`.
fn look_with(
    asker: &Record,
    records: &[Record],
    trees: &HashMap<u32, TreeUse>,
    reading: &Reading,
) -> Result<Grant, Shortfall> {
    let ahead: Vec<Record> = records
        .iter()
        .filter(|r| r.order() != asker.order())
        .cloned()
        .collect();
    decide(
        &asker.need,
        asker.order(),
        reading,
        &as_seen(&ahead, trees),
        10,
    )
}

/// The look of `asker` behind `records` on `reading`, every claimant's tree read and none using
/// any memory but `uses`.
fn look(
    asker: &Record,
    records: &[Record],
    uses: &[(u32, u64)],
    reading: &Reading,
) -> Result<Grant, Shortfall> {
    look_with(asker, records, &sweep(records, uses), reading)
}

/// `record` kept by `accounting` instead of its holder's tree.
fn kept_by(mut record: Record, accounting: crate::Accounting) -> Record {
    record.accounting = accounting;
    record
}

/// THE MEASURED STALL, AS A LEDGER: a release qualify's 42 GiB slice budget waits on 35 GiB
/// available, with no claim held, so what it lacks is memory no claim holds and no claim's end
/// can start it (STUCK). A 3 GiB claim that asked after it is admitted at its first look, and
/// the budget still waits, never admitted ALONE through its margin; its own line says no claim's
/// end it can wait for brings what it lacks, and `spared` (what `aterm pkg capacity` lists)
/// names it stuck. Once the 3 GiB claim is held beside it, its look still names it stuck, with
/// that claim among its holders: what a release's slice budget escapes a wait on, whatever
/// claims went first. NEGATIVE CONTROL (mutant M-NEVER, the rule before): an unfunded waiter
/// spared what first in, first out left beside it, none here, and held the 3 GiB claim, and
/// every build on the Mac, for two hours.
#[test]
fn a_waiter_short_of_memory_no_claim_holds_admits_a_small_claim_behind_it() {
    let budget = claim(1, 1, slice_budget(42), false);
    let small = claim(2, 2, Need::new("test build").mem_gib(3), false);
    let ledger = [budget.clone(), small.clone()];
    assert!(
        look(&small, &ledger, &[], &measured()).is_ok(),
        "the 3 GiB claim goes first past the stuck budget"
    );

    let short = look(&budget, &ledger, &[], &measured()).expect_err("the budget still waits");
    assert_eq!(
        short.shorts,
        [Short::Memory {
            need: 42 * GIB,
            margin: 1,
            available: 35 * GIB,
            reserved: 0,
            passes: None,
            stuck_ahead: 0,
            stuck: true,
        }]
    );
    assert!(
        short.holders.is_empty(),
        "no claim holds what it lacks: {short}"
    );
    assert!(
        short.shorts[0]
            .to_string()
            .ends_with("; no end of a claim it can wait for brings what it lacks (it waits for memory no claim holds)"),
        "{short}"
    );

    let standing = spared(&ledger, None, 35 * GIB, measured().reachable());
    assert_eq!(standing, [Some(Spared::Stuck), None]);

    let went_first = claim(2, 2, Need::new("test build").mem_gib(3), true);
    let short = look(
        &budget,
        &[went_first, budget.clone()],
        &[(2, 3 * GIB)],
        &Reading::of_gib(128, 32, 18),
    )
    .expect_err("the budget still waits beside the claim that went first");
    assert!(
        matches!(
            short.shorts.as_slice(),
            [Short::Memory {
                reserved: 0,
                stuck: true,
                ..
            }]
        ) && short.holders.len() == 1,
        "{short:?}"
    );
}

/// FAIL CLOSED: a held claim whose processes' use the look does not see may fund a waiter, so it
/// keeps its place. The CONTROL's ledger below (a 42 GiB budget on 35 GiB behind a held 20 GiB
/// claim whose processes use 20 GiB), the held claim kept by its cohort, detached, as a whole
/// group, or in a look with no process sweep: the look reserves the whole 20 GiB and cannot see
/// what its processes use, so their end may bring what the budget lacks. The budget spares
/// nothing: a 3 GiB claim waits behind it, and its own look does not name it stuck. Once the
/// held claim ends it is stuck again and the 3 GiB claim goes first. And where memory no claim
/// holds has fallen while a detached 10 GiB claim that went first is held, so that its end may
/// start the budget, a 20 GiB claim behind it waits. NEGATIVE CONTROL: the stuck test reckoned
/// on what the look sees alone (before this rule) admits the 3 GiB claim and the 20 GiB one in
/// each of these looks.
#[test]
fn a_held_claim_the_look_does_not_see_keeps_a_waiter_it_may_fund_in_its_place() {
    let budget = claim(1, 1, slice_budget(42), false);
    let small = claim(2, 2, Need::new("test build").mem_gib(3), false);
    let held = claim(3, 0, Need::new("held compile").mem_gib(20), true);
    let used = sweep(&[held.clone(), budget.clone()], &[(3, 20 * GIB)]);
    let looks = [
        (
            kept_by(held.clone(), crate::Accounting::CohortOwned),
            used.clone(),
        ),
        (
            kept_by(held.clone(), crate::Accounting::Detached),
            used.clone(),
        ),
        (kept_by(held.clone(), crate::Accounting::Whole), used),
        (held, HashMap::new()),
    ];
    for (held, trees) in looks {
        let ledger = [held.clone(), budget.clone(), small.clone()];
        let short = look_with(&small, &ledger, &trees, &measured())
            .expect_err("the 3 GiB claim waits behind the budget");
        assert!(
            matches!(
                short.shorts.as_slice(),
                [Short::Memory {
                    passes: Some(0),
                    stuck_ahead: 0,
                    ..
                }]
            ),
            "{:?}: {short:?}",
            held.accounting
        );
        let own = look_with(&budget, &ledger, &trees, &measured()).expect_err("it waits");
        assert!(
            matches!(own.shorts.as_slice(), [Short::Memory { stuck: false, .. }]),
            "{:?}: {own:?}",
            held.accounting
        );
        assert_eq!(
            spared(&ledger[..2], Some(&trees), 35 * GIB, measured().reachable()),
            [None, Some(Spared::Within(0))],
            "{:?}",
            held.accounting
        );
    }
    assert!(look(&small, &[budget.clone(), small.clone()], &[], &measured()).is_ok());

    let detached = kept_by(
        claim(2, 2, Need::new("went first").mem_gib(10), true),
        crate::Accounting::Detached,
    );
    let wide = claim(4, 4, Need::new("wide build").mem_gib(20), false);
    let ledger = [detached.clone(), budget.clone()];
    let short = look_with(
        &wide,
        &ledger,
        &sweep(&ledger, &[(2, 10 * GIB)]),
        &measured(),
    )
    .expect_err("20 GiB waits");
    assert!(
        matches!(
            short.shorts.as_slice(),
            [Short::Memory {
                passes: Some(0),
                ..
            }]
        ),
        "{short:?}"
    );
}

/// A waiter's end gives back the tree its process holds already besides what it reserves: a
/// 10 GiB build waits ahead of a VM's 42 GiB boot (a margin of a byte), its process using 5 GiB
/// of it already, on 38 GiB available. It starts now, reserving the other 5 GiB, and its end
/// brings 10 GiB, which with the 33 GiB beside it starts the boot: the boot is not stuck and a
/// 3 GiB claim behind it waits. On 37 GiB the build's end leaves the boot short, so it is stuck
/// and the 3 GiB claim goes first. MUTANT (a waiter's end reckoned as its reservation alone)
/// names the boot stuck on 38 GiB and admits the 3 GiB claim: this test fails under it.
#[test]
fn a_waiters_end_gives_back_what_its_process_holds_already() {
    let build = claim(2, 0, Need::new("build").mem_gib(10), false);
    let budget = claim(1, 1, Need::new("vm boot").mem_gib(42).margin(1), false);
    let small = claim(3, 3, Need::new("test build").mem_gib(3), false);
    let ledger = [build, budget, small.clone()];
    let uses = [(2, 5 * GIB)];
    let short = look(&small, &ledger, &uses, &Reading::of_gib(128, 38, 18))
        .expect_err("the 3 GiB claim waits behind the boot the build's end starts");
    assert!(
        matches!(
            short.shorts.as_slice(),
            [Short::Memory {
                passes: Some(0),
                stuck_ahead: 0,
                ..
            }]
        ),
        "{short:?}"
    );
    assert!(look(&small, &ledger, &uses, &Reading::of_gib(128, 37, 18)).is_ok());
}

/// A waiter funded by what a claim ahead holds spares what that funding leaves, though a stuck
/// waiter waits ahead of it: a held 10 GiB claim whose tree holds it, the stuck budget of 50 GiB
/// on 35 GiB, then a 40 GiB build of no margin, which that claim's end starts and which spares
/// the 5 GiB its end brings past what the build lacks. A 3 GiB claim behind them goes first.
/// MUTANT (the stuck budget's need counted in what a later waiter's turn waits for) leaves the
/// build lacking more than any end brings, so it spares nothing and the 3 GiB claim waits: this
/// test fails under it.
#[test]
fn a_funded_waiter_behind_a_stuck_one_spares_what_its_own_funding_leaves() {
    let held = claim(3, 0, Need::new("held").mem_gib(10), true);
    let budget = claim(1, 1, slice_budget(50), false);
    let build = claim(2, 2, Need::new("whole build").mem_gib(40), false);
    let small = claim(4, 4, Need::new("test build").mem_gib(3), false);
    let ledger = [held, budget, build, small.clone()];
    let uses = [(3, 10 * GIB)];
    assert!(look(&small, &ledger, &uses, &measured()).is_ok());
    let trees = sweep(&ledger, &uses);
    assert_eq!(
        spared(&ledger[..3], Some(&trees), 35 * GIB, measured().reachable()),
        [None, Some(Spared::Stuck), Some(Spared::Within(5 * GIB))]
    );
}

/// A waiter of no margin at or above what can ever be available is STUCK: memory wired since it
/// asked leaves 28 GiB reachable of 128, and a 30 GiB build waits on 20 GiB available. It is
/// never admitted ALONE, so no claim's end can start it, and a 3 GiB claim behind it goes first.
/// MUTANTS: the reach left out of its standing (it reads as one that runs alone once the claims
/// ahead end) makes the 3 GiB claim wait, and `spared` asking with no reach names it one that
/// keeps its place: this test fails under either.
#[test]
fn a_waiter_beyond_what_can_ever_be_available_holds_back_no_claim_behind_it() {
    let reading = Reading {
        wired: Some(100 * GIB),
        ..Reading::of_gib(128, 20, 18)
    };
    assert_eq!(reading.reachable(), Some(28 * GIB));
    let build = claim(1, 1, Need::new("whole build").mem_gib(30), false);
    let small = claim(2, 2, Need::new("test build").mem_gib(3), false);
    let ledger = [build.clone(), small.clone()];
    assert!(look(&small, &ledger, &[], &reading).is_ok());
    let own = look(&build, &ledger, &[], &reading).expect_err("it waits");
    assert!(
        matches!(own.shorts.as_slice(), [Short::Memory { stuck: true, .. }]),
        "{own:?}"
    );
    assert_eq!(
        spared(&ledger, None, 20 * GIB, reading.reachable()),
        [Some(Spared::Stuck), None]
    );
}

/// A waiter's own look names it stuck only where no claim's end brings what it asks: a VM's boot
/// of 20 GiB and 14 GiB beside it, on 40 GiB available behind a held 10 GiB claim whose tree
/// uses nothing, lacks 4 GiB, which that claim's end brings; it keeps its place and its line
/// does not call it stuck. MUTANT (its own standing reckoned with its margin twice over, as its
/// reservation and beside it) names it stuck: this test fails under it.
#[test]
fn a_funded_waiters_own_look_does_not_name_it_stuck() {
    let held = claim(3, 0, Need::new("held").mem_gib(10), true);
    let vm = claim(
        1,
        1,
        Need::new("vm boot").mem_gib(20).margin(14 * GIB),
        false,
    );
    let ledger = [held, vm.clone()];
    let reading = Reading::of_gib(64, 40, 14);
    let own = look(&vm, &ledger, &[], &reading).expect_err("it waits for the held claim");
    assert!(
        matches!(
            own.shorts.as_slice(),
            [Short::Memory {
                reserved,
                stuck: false,
                ..
            }] if *reserved == 10 * GIB
        ),
        "{own:?}"
    );
    assert!(
        !own.shorts[0].to_string().contains("no end of a claim"),
        "{own}"
    );
}

/// While a claim holds memory alone, no waiter lets a claim go first, a stuck one included: a
/// held 2 GiB claim admitted alone, whose tree holds it, beside the stuck 42 GiB budget on
/// 35 GiB. `spared` names the budget one that spares nothing, as `decide` holds every memory
/// claim behind the alone one. MUTANT (the held-alone override skipping a stuck waiter) names it
/// stuck: this test fails under it.
#[test]
fn a_claim_held_alone_lets_no_claim_go_first_past_a_stuck_waiter() {
    let mut alone = claim(3, 0, Need::new("alone").mem_gib(2), true);
    if let Some(held) = alone.held.as_mut() {
        held.alone = true;
    }
    let budget = claim(1, 1, slice_budget(42), false);
    let small = claim(2, 2, Need::new("test build").mem_gib(3), false);
    let ledger = [alone, budget];
    let trees = sweep(&ledger, &[(3, 2 * GIB)]);
    assert_eq!(
        spared(&ledger, Some(&trees), 35 * GIB, measured().reachable()),
        [None, Some(Spared::Within(0))]
    );
    let mut with_small = ledger.to_vec();
    with_small.push(small.clone());
    let short = look_with(&small, &with_small, &trees, &measured()).expect_err("it waits");
    assert!(
        short
            .shorts
            .iter()
            .any(|s| matches!(s, Short::Alone { pid: 3, .. })),
        "{short:?}"
    );
}

/// BACKGROUND WORK STILL YIELDS TO A RELEASE: a background-rank 3 GiB claim behind the stuck
/// release budget fits the room, yet waits, named behind the release ([`Short::Release`]):
/// background work is preemptible by design, whatever the memory turn says. An ordinary 3 GiB
/// claim there goes first (the measured ledger above).
#[test]
fn background_work_still_yields_to_a_stuck_release_waiter() {
    let budget = claim(1, 1, slice_budget(42), false);
    let background = claim(
        2,
        2,
        Need::new("background test run")
            .rank(Rank::Background)
            .mem_gib(3),
        false,
    );
    let short = look(&background, &[budget, background.clone()], &[], &measured())
        .expect_err("it waits for the release");
    assert_eq!(short.shorts, [Short::Release], "{short:?}");
}

/// CONTROL: a waiter one holder's end would start keeps its place ahead of the claims behind it.
/// The same 42 GiB budget on 35 GiB, behind a held 20 GiB claim whose tree holds what it claims:
/// that end brings what the budget lacks (7 GiB and a byte), so it spares 13 GiB less a byte. A
/// 3 GiB claim behind it goes first within that; a 13 GiB one, which fits the room, waits.
/// MUTANT M-ALWAYS (every waiter that cannot start lets the claims behind it pass, whatever
/// funds it) admits the 13 GiB claim and delays the budget's turn past the holder's end: this
/// test fails under it.
#[test]
fn a_waiter_one_holders_end_would_start_keeps_its_place() {
    let holder = claim(3, 0, Need::new("held compile").mem_gib(20), true);
    let budget = claim(1, 1, slice_budget(42), false);
    let small = claim(2, 2, Need::new("test build").mem_gib(3), false);
    let wide = claim(4, 3, Need::new("wide build").mem_gib(13), false);
    let ledger = [holder, budget, small.clone()];
    let uses = [(3, 20 * GIB)];
    assert!(look(&small, &ledger, &uses, &measured()).is_ok());

    let short = look(&wide, &ledger[..2], &uses, &measured()).expect_err("13 GiB waits");
    assert_eq!(
        short.shorts,
        [Short::Memory {
            need: 13 * GIB,
            margin: 0,
            available: 35 * GIB,
            reserved: 42 * GIB,
            passes: Some(13 * GIB - 1),
            stuck_ahead: 0,
            stuck: false,
        }]
    );

    let trees: HashMap<u32, TreeUse> = [(
        3,
        TreeUse {
            charged: 20 * GIB,
            footprint: 20 * GIB,
            written: 0,
        },
    )]
    .into_iter()
    .collect();
    let standing = spared(&ledger[..2], Some(&trees), 35 * GIB, measured().reachable());
    assert_eq!(standing, [None, Some(Spared::Within(13 * GIB - 1))]);
}

/// A waiter its own look admits ALONE once the claims ahead end keeps its place, though no set of
/// those ends brings what it lacks: a 40 GiB build of no margin on 35 GiB, behind a held 10 GiB
/// claim that has used nothing yet. A 3 GiB claim behind it waits, so no stream of small claims
/// keeps it from ever being alone, and once the held claim ends it is admitted alone. MUTANT
/// M-NOALONE (such a waiter treated as stuck) admits the 3 GiB claim: this test fails under it.
#[test]
fn a_waiter_that_runs_alone_once_the_claims_ahead_end_keeps_its_place() {
    let holder = claim(3, 0, Need::new("held").mem_gib(10), true);
    let whole = claim(1, 1, Need::new("whole build").mem_gib(40), false);
    let small = claim(2, 2, Need::new("test build").mem_gib(3), false);
    let short = look(&small, &[holder, whole.clone()], &[], &measured())
        .expect_err("the 3 GiB claim waits behind the build");
    assert!(
        matches!(
            short.shorts.as_slice(),
            [Short::Memory {
                passes: Some(0),
                stuck_ahead: 0,
                stuck: false,
                ..
            }]
        ),
        "{short:?}"
    );

    let short = look(&small, std::slice::from_ref(&whole), &[], &measured())
        .expect_err("still behind the build once the holder ends");
    assert!(
        matches!(
            short.shorts.as_slice(),
            [Short::Memory {
                passes: Some(0),
                ..
            }]
        ),
        "{short:?}"
    );
    let grant = look(&whole, &[whole.clone(), small], &[], &measured()).expect("alone");
    assert!(grant.alone.is_some(), "admitted alone: {grant:?}");
}

/// A waiter behind a stuck one stands as though it were not there: a 40 GiB build of no margin,
/// and one that asked to run alone, each queued behind the stuck 42 GiB budget on 35 GiB, is
/// admitted ALONE in its turn, and a 3 GiB claim behind the build waits for it as it would with
/// no budget queued. MUTANT M-COUNTSTUCK (the stuck budget counted among the claims that wait
/// ahead for memory) holds the build back from ALONE for as long as the budget stays stuck, and
/// M-NEVER does too: this test fails under either.
#[test]
fn a_waiter_behind_a_stuck_one_runs_alone_in_its_turn() {
    let budget = claim(1, 1, slice_budget(42), false);
    for need in [
        Need::new("whole build").mem_gib(40),
        Need::new("isolated build").mem_gib(20).alone(),
    ] {
        let build = claim(2, 2, need, false);
        let grant = look(&build, &[budget.clone(), build.clone()], &[], &measured())
            .expect("admitted alone past the stuck budget");
        assert!(grant.alone.is_some(), "admitted alone: {grant:?}");
    }

    let build = claim(2, 2, Need::new("whole build").mem_gib(40), false);
    let small = claim(3, 3, Need::new("test build").mem_gib(3), false);
    let short = look(&small, &[budget, build], &[], &measured())
        .expect_err("the 3 GiB claim waits for the build's turn");
    assert!(
        matches!(
            short.shorts.as_slice(),
            [Short::Memory {
                reserved,
                passes: Some(0),
                stuck_ahead,
                stuck: false,
                ..
            }] if *reserved == 40 * GIB && *stuck_ahead == 42 * GIB
        ),
        "{short:?}"
    );
    let line = short.shorts[0].to_string();
    assert!(
        line.contains(&format!(
            "; no claim's end can start the claims ahead that ask {} more (they wait for memory \
             no claim holds), so they reserve none of it",
            gib(42 * GIB)
        )),
        "{line}"
    );
}

/// A stuck waiter keeps its place again once the claims that went first can bring what it lacks:
/// memory no claim holds fell after a 3 GiB claim went first past the 42 GiB budget, so 40 GiB
/// is available beside that claim's tree. The budget would start were that claim not held, and
/// its end brings the 2 GiB and a byte the budget lacks: it spares 1 GiB less a byte, so a 1 GiB
/// claim behind it waits and a 512 MiB one goes first; once the 3 GiB claim ends the budget is
/// admitted. MUTANT M-ALWAYS admits the 1 GiB claim: this test fails under it.
#[test]
fn a_stuck_waiter_keeps_its_place_once_the_claims_that_went_first_fund_it() {
    let went_first = claim(2, 2, Need::new("test build").mem_gib(3), true);
    let budget = claim(1, 1, slice_budget(42), false);
    let risen = Reading::of_gib(128, 40, 18);
    let uses = [(2, 3 * GIB)];
    let ledger = [went_first, budget.clone()];
    let one = claim(3, 3, Need::new("one").mem_gib(1), false);
    let short = look(&one, &ledger, &uses, &risen).expect_err("1 GiB waits");
    assert!(
        matches!(
            short.shorts.as_slice(),
            [Short::Memory {
                passes: Some(most),
                stuck_ahead: 0,
                ..
            }] if *most == GIB - 1
        ),
        "{short:?}"
    );
    let half = claim(3, 3, Need::new("half").mem(GIB / 2), false);
    assert!(look(&half, &ledger, &uses, &risen).is_ok());

    let ended = Reading::of_gib(128, 43, 18);
    assert!(look(&budget, std::slice::from_ref(&budget), &[], &ended).is_ok());
}

/// THE LEDGER REFINES THE PASSING MODEL (`aterm_models_capacity::memory_passing_model`). In each
/// reachable state where the head waits, the ledger the state projects to (a GiB a unit: the head
/// another process's 8 GiB claim, 1 GiB beside it where it asks a margin; the held claim and each
/// small claim that went first another process whose tree holds what it claims; what is
/// available `Memory - out - tree - small`) admits a small claim behind them exactly where
/// `SmallStarts` is enabled, refuses one that fits what is available exactly where
/// `SmallRefused` is, and the head's own look admits it exactly where `HeadStarts` or
/// `HeadRunsAlone` is, alone where the latter is. NEGATIVE CONTROL: each mutant dial of the
/// model (Buggy 1, 2, 3) changes the rule in some state the walk judges, and there `decide`
/// keeps to the committed rule; with the product mutant of the same name planted in
/// `memory_turn` or `decide`, this test fails.
#[test]
fn the_ledger_refines_the_memory_passing_model() {
    use aterm_spec::derive::Model;
    use aterm_spec::interp::{State, reachable_states, with_buggy};

    let model = aterm_models_capacity::memory_passing_model();
    let enabled = |m: &Model, action: &str, state: &State| !m.successors(action, state).is_empty();
    let constant = |name: &str| {
        model
            .consts
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| u64::try_from(*v).expect("a non-negative constant"))
            .expect("the model declares it")
    };
    let units = |v: i64| u64::try_from(v).expect("a non-negative unit count") * GIB;
    let (memory, need, smalls) = (constant("Memory"), constant("Need"), constant("Smalls"));
    let mutants = [1, 2, 3].map(|buggy| with_buggy(&model, buggy));
    let mut looked = 0;
    let mut admitted = 0;
    let mut caught = [0usize; 3];
    for state in reachable_states(&model)
        .iter()
        .filter(|s| s["head"] == 1 && u64::try_from(s["small"]).is_ok_and(|n| n < smalls))
    {
        let available =
            memory * GIB - units(state["out"]) - units(state["tree"]) - units(state["small"]);
        let reading = Reading {
            available: Some(available),
            ..Reading::of_gib(2 * memory, 0, 8)
        };
        let head = claim(
            1,
            1,
            Need::new("head")
                .mem(need * GIB)
                .margin(units(state["margin"])),
            false,
        );
        let mut ledger = vec![head.clone()];
        let mut uses = Vec::new();
        if state["tree"] > 0 {
            ledger.push(claim(
                3,
                0,
                Need::new("held").mem(units(state["tree"])),
                true,
            ));
            uses.push((3, units(state["tree"])));
        }
        for i in 0..u32::try_from(state["small"]).expect("a small count") {
            ledger.push(claim(
                10 + i,
                10 + u64::from(i),
                Need::new("went first").mem_gib(1),
                true,
            ));
            uses.push((10 + i, GIB));
        }
        let asker = claim(20, 20, Need::new("small").mem_gib(1), false);
        let starts = look(&asker, &ledger, &uses, &reading).is_ok();
        assert_eq!(
            starts,
            enabled(&model, "SmallStarts", state),
            "a small claim behind the head in {state:?}"
        );
        assert_eq!(
            enabled(&model, "SmallRefused", state),
            !starts && available >= GIB,
            "a refused small claim that fits what is available in {state:?}"
        );
        let own = look(&head, &ledger, &uses, &reading);
        assert_eq!(
            own.as_ref().ok().map(|grant| grant.alone.is_some()),
            if enabled(&model, "HeadRunsAlone", state) {
                Some(true)
            } else if enabled(&model, "HeadStarts", state) {
                Some(false)
            } else {
                None
            },
            "the head's own look in {state:?}: {own:?}"
        );
        for (i, mutant) in mutants.iter().enumerate() {
            if enabled(mutant, "SmallStarts", state) != starts {
                assert_eq!(
                    enabled(mutant, "SmallRefused", state),
                    starts,
                    "Buggy {} changes the rule in both looks, in {state:?}",
                    i + 1
                );
                caught[i] += 1;
            }
        }
        looked += 1;
        admitted += usize::from(starts);
    }
    assert!(
        looked > 0 && admitted > 0 && admitted < looked,
        "the walk judged {looked} waiting states, {admitted} of them admitting"
    );
    assert!(
        caught.iter().all(|n| *n > 0),
        "each mutant dial's change to the rule met in some state: {caught:?}"
    );
}
