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

/// The look of `asker` behind `records` on `reading` with `trees`.
fn look_with(
    asker: &Record,
    records: &[Record],
    trees: &HashMap<u32, TreeUse>,
    reading: &Reading,
) -> Result<Grant, Shortfall> {
    let read: Vec<&Record> = records.iter().collect();
    let wide = ledgerwide(&read, trees, None, reading);
    let ahead: Vec<&Record> = read
        .iter()
        .copied()
        .filter(|r| r.order() != asker.order())
        .collect();
    let seen = others(
        &ahead,
        read.iter().map(|r| r.pid).collect(),
        trees,
        &HashMap::new(),
        &wide,
    );
    decide(&asker.need, asker.order(), reading, &seen, 10)
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
            .ends_with("; no end of a claim it can wait for brings what it lacks (it waits for memory no claim holds but the work it runs under)"),
        "{short}"
    );

    let standing = spared(&ledger, None, None, &measured());
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
            spared(&ledger[..2], Some(&trees), None, &measured(),),
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
        spared(&ledger[..3], Some(&trees), None, &measured(),),
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
        spared(&ledger, None, None, &reading,),
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
        spared(&ledger, Some(&trees), None, &measured(),),
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
    let standing = spared(&ledger[..2], Some(&trees), None, &measured());
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
            "; no end of a claim they can wait for can start the claims ahead that ask {} more \
             (they wait for memory no claim holds but the work they run under), so they reserve \
             none of it",
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
/// small claim that went first another process whose tree holds what it claims, the held claim's
/// process the head's process's parent in the sweep where `kin` is set, the outer waiter another
/// process's cohort-owned claim of `Outer` GiB and one beside it, asked first, where `outer` is;
/// what is available `Memory - out - tree - small`), each look taken as `Waiter::poll` takes it
/// ([`swept_look`]),
/// admits a small claim behind them exactly where `SmallStarts` is enabled, refuses one that
/// fits what is available exactly where `SmallRefused` is, and the head's own look admits it
/// exactly where `HeadStarts` or `HeadRunsAlone` is, alone where the latter is. NEGATIVE
/// CONTROL: each mutant dial of the model (Buggy 1 to 5) changes the rule in some state the walk
/// judges, and there `decide` keeps to the committed rule; with the product mutant of the same
/// name planted in `memory_turn` or `decide` (Buggy 4's is M-KIN-END, a waiter's kin's ends
/// counted; Buggy 5's M-INSIDE-ALL, an inside waiter stood beside every waiter ahead that is
/// none of its kin's nor its process's),
/// this test fails.
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
    let mutants = [1, 2, 3, 4, 5].map(|buggy| with_buggy(&model, buggy));
    let mut looked = 0;
    let mut admitted = 0;
    let mut caught = [0usize; 5];
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
        if state["outer"] == 1 {
            ledger.push(kept_by(
                claim(
                    7,
                    0,
                    Need::new("outer").mem(constant("Outer") * GIB).margin(GIB),
                    false,
                ),
                crate::Accounting::CohortOwned,
            ));
        }
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
        // The head's process is the held claim's process's child where `kin` is set, and
        // another's child where it is not; each process's tree holds what its claim uses.
        let mut line = vec![
            (1, if state["kin"] == 1 { 3 } else { 2 }, 0),
            (3, 2, 0),
            (7, 2, 0),
            (20, 2, 0),
            (2, 0, 0),
        ];
        for (pid, used) in &uses {
            if let Some(p) = line.iter_mut().find(|(p, _, _)| p == pid) {
                p.2 = *used;
            } else {
                line.push((*pid, 2, *used));
            }
        }
        let procs = processes(&line);
        let asker = claim(20, 20, Need::new("small").mem_gib(1), false);
        let starts = swept_look(&asker, &ledger, &procs, &reading).is_ok();
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
        let own = swept_look(&head, &ledger, &procs, &reading);
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

/// The processes `(pid, parent, memory charged to it)`, as a sweep reads them.
fn processes(line: &[(u32, u32, u64)]) -> Procs {
    Procs::of(
        line.iter()
            .map(|&(pid, ppid, charged)| crate::reading::Proc {
                pid,
                ppid,
                start: 0,
                charged,
                footprint: charged,
                written: 0,
                cpu_ns: 0,
                runnable_ns: 0,
            })
            .collect(),
    )
}

/// The look of `asker` behind `records` as `Waiter::poll` takes it with the sweep `procs`, through
/// the one read every look makes ([`look_read`]): its kin's claims left out ([`counted`]) unless
/// it is cohort-owned, each tree rooted at every claim's holder and each claim reckoned among
/// all of them ([`ledgerwide`]).
fn swept_look(
    asker: &Record,
    records: &[Record],
    procs: &Procs,
    reading: &Reading,
) -> Result<Grant, Shortfall> {
    let look = look_read(
        &records.iter().collect::<Vec<_>>(),
        asker,
        Some(procs),
        reading,
    );
    let seen = others(
        &look.counted,
        look.used.claimants,
        &look.used.trees,
        &look.used.lines,
        &look.wide,
    );
    decide(&asker.need, asker.order(), reading, &seen, 10)
}

/// Whether `look` is a wait for memory whose own line names it stuck or not (`stuck`).
fn waits_stuck(look: &Result<Grant, Shortfall>, stuck: bool) -> bool {
    matches!(look, Err(short) if matches!(
        short.shorts.as_slice(),
        [Short::Memory { stuck: s, .. }] if *s == stuck
    ))
}

/// OWN PROCESS (ATERM-KI-1113-21's rejected half): a 42 GiB slice budget (a margin, so never
/// ALONE) waits on 35 GiB available beside a 10 GiB claim its own process holds, whose tree uses
/// all 10 GiB. One process may hold a claim and wait for another side by side (verify's test
/// binaries, each admitted from its one process), and the held claim ends when its work does,
/// whatever the wait does: its end may bring the 8 GiB the budget lacks. So the budget keeps its
/// place in the look of a 3 GiB claim behind it, which waits, sparing it less than 3 GiB; its own
/// look does not name it stuck, and `spared` (what `aterm pkg capacity` lists) agrees, with the
/// sweep's ancestors or without. The CONTROL: the same budget with nothing held stuck by every
/// look, and the 3 GiB claim admitted at once, so it is the own claim's end that keeps the place.
#[test]
fn a_waiter_its_own_processs_held_claim_funds_keeps_its_place() {
    let budget = claim(11, 1, slice_budget(42), false);
    let own = claim(
        11,
        0,
        Need::new("verify's other test binary").mem_gib(10),
        true,
    );
    let small = claim(12, 2, Need::new("test build").mem_gib(3), false);
    let procs = processes(&[(11, 9, 10 * GIB), (12, 9, 0), (9, 1, 0)]);
    let ledger = [own, budget.clone(), small.clone()];
    let short = swept_look(&small, &ledger, &procs, &measured());
    assert!(
        matches!(&short, Err(s) if matches!(
            s.shorts.as_slice(),
            [Short::Memory { passes: Some(most), .. }] if *most < 3 * GIB
        )),
        "the own process's held claim may end first and fund the budget: {short:?}"
    );
    let mine = swept_look(&budget, &ledger, &procs, &measured());
    assert!(waits_stuck(&mine, false), "{mine:?}");
    let trees = sweep(&ledger, &[(11, 10 * GIB)]);
    let got = spared(&ledger, Some(&trees), Some(&procs), &measured());
    assert!(
        matches!(got.as_slice(), [None, Some(Spared::Within(_)), _]),
        "{got:?}"
    );

    let ledger = [budget.clone(), small.clone()];
    let procs = processes(&[(11, 9, 0), (12, 9, 0), (9, 1, 0)]);
    assert!(
        swept_look(&small, &ledger, &procs, &measured()).is_ok(),
        "with nothing held, no end can fund the budget and the 3 GiB claim goes first"
    );
    let mine = swept_look(&budget, &ledger, &procs, &measured());
    assert!(waits_stuck(&mine, true), "{mine:?}");
}

/// KIN, AN ANCESTOR (ATERM-KI-1113-21): the same budget, its process (5) started under a process
/// (4) that holds a 10 GiB claim its tree uses whole. The ancestor's claim ends only once the
/// budget's work has run, so in the look of a 3 GiB claim behind it, which reads the budget's
/// ancestors from the sweep, the budget is STUCK and the 3 GiB claim is admitted; the budget's
/// own look, which leaves its ancestor's claim out ([`counted`]), names it stuck too, and so does
/// `spared` given the same sweep's ancestors (what `aterm pkg capacity` lists). The CONTROL: the
/// 10 GiB holder is no ancestor of the budget's (both under 9), so its end may come first, the
/// 3 GiB claim waits and every look keeps the budget's place. NEGATIVE CONTROL: `spared` with
/// no sweep (a listing that read no process's ancestors) keeps the budget's place, the
/// disagreement the KI named.
#[test]
fn a_waiter_only_an_ancestors_held_claim_funds_admits_a_small_claim_behind_it() {
    let budget = claim(5, 1, slice_budget(42), false);
    let parent = claim(4, 0, Need::new("verify's held stage").mem_gib(10), true);
    let small = claim(2, 2, Need::new("test build").mem_gib(3), false);
    let ledger = [parent, budget.clone(), small.clone()];
    let trees = sweep(&ledger, &[(4, 10 * GIB)]);
    let listed = |procs: &Procs| spared(&ledger, Some(&trees), Some(procs), &measured());
    let under = processes(&[(4, 9, 10 * GIB), (5, 4, 0), (2, 9, 0), (9, 1, 0)]);
    assert!(
        swept_look(&small, &ledger, &under, &measured()).is_ok(),
        "the 3 GiB claim goes first past the budget its ancestor's claim cannot fund"
    );
    let mine = swept_look(&budget, &ledger, &under, &measured());
    assert!(waits_stuck(&mine, true), "{mine:?}");
    assert_eq!(listed(&under), [None, Some(Spared::Stuck), None]);
    let unswept = spared(&ledger, Some(&trees), None, &measured());
    assert!(
        matches!(unswept.as_slice(), [None, Some(Spared::Within(_)), _]),
        "{unswept:?}"
    );

    let beside = processes(&[(4, 9, 10 * GIB), (5, 9, 0), (2, 9, 0), (9, 1, 0)]);
    let short = swept_look(&small, &ledger, &beside, &measured());
    assert!(
        matches!(&short, Err(s) if matches!(
            s.shorts.as_slice(),
            [Short::Memory { passes: Some(most), .. }] if *most < 3 * GIB
        )),
        "a held claim that is no ancestor's funds the budget: {short:?}"
    );
    let mine = swept_look(&budget, &ledger, &beside, &measured());
    assert!(waits_stuck(&mine, false), "{mine:?}");
    assert!(matches!(
        listed(&beside).as_slice(),
        [None, Some(Spared::Within(_)), _]
    ));
}

/// KIN'S RESERVATIONS: a waiter's turn is reckoned beside the claims its own look counts, none of
/// its kin's, neither their reservations nor their ends, since that look leaves them out
/// ([`counted`]). The budget's ancestor claims 20 GiB of which its tree has used 5, so it reserves
/// 15 against the claims behind; another process holds 10 GiB, all of it in use. On 35 GiB the
/// budget's own look lacks 7 GiB and a byte, which the other claim's end brings with 3 GiB less a
/// byte to spare: it keeps its place, and so does every look behind it, which holds a 13 GiB claim
/// back and lets a 2 GiB one go first. NEGATIVE CONTROL (mutant M-KIN-IN, the kin's claims counted
/// where another look stands a waiter): that look takes the kin's reservation off its room and
/// counts the kin's end among those it waits for, and finds the budget sparing 8 GiB less a byte,
/// which its own look never reckons.
#[test]
fn a_waiters_turn_is_reckoned_in_the_room_its_own_look_sees() {
    let parent = claim(3, 0, Need::new("held release").mem_gib(20), true);
    let other = claim(5, 0, Need::new("held compile").mem_gib(10), true);
    let budget = claim(1, 1, slice_budget(42), false);
    let wide = claim(4, 3, Need::new("wide build").mem_gib(13), false);
    let small = claim(6, 4, Need::new("test build").mem_gib(2), false);
    let procs = processes(&[
        (3, 2, 5 * GIB),
        (5, 2, 10 * GIB),
        (1, 3, 0),
        (4, 2, 0),
        (6, 2, 0),
        (2, 0, 0),
    ]);
    let ledger = [parent, other, budget.clone(), wide.clone(), small.clone()];
    let short = swept_look(&wide, &ledger[..4], &procs, &measured());
    assert!(
        matches!(&short, Err(s) if matches!(
            s.shorts.as_slice(),
            [Short::Memory { passes: Some(most), stuck_ahead: 0, stuck: false, .. }]
                if *most == 3 * GIB - 1
        )),
        "{short:?}"
    );
    assert!(swept_look(&small, &ledger, &procs, &measured()).is_ok());
    let mine = swept_look(&budget, &ledger[..3], &procs, &measured());
    assert!(waits_stuck(&mine, false), "{mine:?}");
    let trees = sweep(&ledger, &[(3, 5 * GIB), (5, 10 * GIB)]);
    assert_eq!(
        spared(&ledger[..3], Some(&trees), Some(&procs), &measured(),),
        [None, None, Some(Spared::Within(3 * GIB - 1))]
    );
}

/// A waiter that only its kin's reservations make room for starts now in every look, as in its
/// own, and takes its reservation from the room the claims behind it are held to, all of that room
/// here, where its reservation is more than the room holds: its kin still hold their
/// reservations against them. A 30 GiB build of no margin under an ancestor that claims 20 GiB
/// and has used 5, on 35 GiB: its own look admits it, the listing says it starts now, and a
/// 13 GiB claim behind it waits on no room. MUTANT M-ROOM-UNDERFLOW (the room taken unsaturated)
/// overflows here.
#[test]
fn a_waiter_its_kins_reservation_makes_room_for_starts_now_in_every_look() {
    let parent = claim(3, 0, Need::new("held release").mem_gib(20), true);
    let build = claim(1, 1, Need::new("release compile").mem_gib(30), false);
    let wide = claim(4, 3, Need::new("wide build").mem_gib(13), false);
    let procs = processes(&[(3, 2, 5 * GIB), (1, 3, 0), (4, 2, 0), (2, 0, 0)]);
    let ledger = [parent, build.clone(), wide.clone()];
    assert!(swept_look(&build, &ledger[..2], &procs, &measured()).is_ok());
    let trees = sweep(&ledger, &[(3, 5 * GIB)]);
    assert_eq!(
        spared(&ledger[..2], Some(&trees), Some(&procs), &measured(),),
        [None, None]
    );
    let short = swept_look(&wide, &ledger, &procs, &measured());
    assert!(
        matches!(&short, Err(s) if matches!(
            s.shorts.as_slice(),
            [Short::Memory { passes: None, stuck_ahead: 0, stuck: false, .. }]
        )),
        "{short:?}"
    );
}

/// A waiter's turn, its kin's claims left out, has what is available, not a room that the held
/// claims took below nothing: an ancestor that claims 20 GiB and has used none, on 15 GiB
/// available, leaves its descendant's 16 GiB budget 15 GiB, as the budget's own look finds, so
/// the budget is stuck in every look, and a 30 GiB budget behind it, which only the ancestor's end
/// could fund in part, is stuck too: its own line names it so, which is what a release's slice
/// budget escapes a wait on. So it is where the held claims past what is available are the
/// ancestor's and a build it started: the build's sibling stands funded by the build's end in
/// every look, and a budget behind it that every end together cannot fund is stuck. NEGATIVE
/// CONTROL (mutant M-KIN-IN, the kin's claims counted where another look stands a waiter): another
/// look counts the ancestor's end among those the descendant waits for and names it funded,
/// where its own look names it stuck.
#[test]
fn a_waiters_kin_give_back_what_was_available_not_a_room_below_nothing() {
    let parent = claim(3, 0, Need::new("held release").mem_gib(20), true);
    let child = claim(1, 1, slice_budget(16), false);
    let later = claim(4, 3, slice_budget(30), false);
    let procs = processes(&[(3, 2, 0), (1, 3, 0), (4, 2, 0), (2, 0, 0)]);
    let ledger = [parent, child.clone(), later.clone()];
    let reading = Reading::of_gib(128, 15, 18);
    for asker in [&child, &later] {
        let own = swept_look(asker, &ledger, &procs, &reading);
        assert!(waits_stuck(&own, true), "{own:?}");
    }
    let trees = sweep(&ledger, &[]);
    assert_eq!(
        spared(&ledger, Some(&trees), Some(&procs), &reading,),
        [None, Some(Spared::Stuck), Some(Spared::Stuck)]
    );

    // The held claims past what is available, the waiter's ancestor's among them: on 35 GiB, an
    // ancestor that has used 5 of its 20 GiB and a 30 GiB build it started, held, reserve 45. The
    // build's sibling's 10 GiB budget has 5 GiB by its own look, which the build's end funds with
    // 25 GiB less a byte to spare, and so it stands in every look; a 55 GiB budget behind it,
    // which every end together funds only to 60 of the 65 GiB and a byte it would need, is stuck.
    let parent = claim(3, 0, Need::new("held release").mem_gib(20), true);
    let build = claim(1, 1, Need::new("release compile").mem_gib(30), true);
    let sibling = claim(7, 2, slice_budget(10), false);
    let later = claim(6, 3, slice_budget(55), false);
    let procs = processes(&[(3, 2, 5 * GIB), (1, 3, 0), (7, 3, 0), (6, 2, 0), (2, 0, 0)]);
    let ledger = [parent, build, sibling.clone(), later.clone()];
    for (asker, stuck) in [(&sibling, false), (&later, true)] {
        let own = swept_look(asker, &ledger, &procs, &measured());
        assert!(waits_stuck(&own, stuck), "{own:?}");
    }
    let trees = sweep(&ledger, &[(3, 5 * GIB)]);
    assert_eq!(
        spared(&ledger, Some(&trees), Some(&procs), &measured(),),
        [
            None,
            None,
            Some(Spared::Within(25 * GIB - 1)),
            Some(Spared::Stuck)
        ]
    );
}

/// A COHORT-OWNED waiter has no kin: its own look counts every claim ([`counted`] is not
/// applied to it), its holder's ancestor's and one crediting it too, so its turn in every other
/// look counts them as well ([`Other::kin`]). A 30 GiB budget of a byte's margin, kept by its
/// cohort, under a process that holds 20 GiB and has used 5 of them, or beside such a claim that
/// credits its process, on 35 GiB: its own look lacks 10 GiB and a byte, which that claim's end
/// brings with 10 GiB less a byte to spare, so a 2 GiB claim behind it goes first and `spared`
/// (what `aterm pkg capacity` lists) names it funded by as much. NEGATIVE CONTROL (mutant
/// M-KIN-COHORT, a cohort-owned waiter given kin and an own count as one its holder's tree keeps
/// is): another look leaves that claim out of the budget's turn, has it start now in 35 GiB and
/// the 2 GiB claim wait on no room, which the budget's own look never reckons.
#[test]
fn a_cohort_owned_waiter_is_reckoned_beside_every_claim_as_its_own_look_counts_them() {
    let ancestor = claim(3, 0, Need::new("held release").mem_gib(20), true);
    let mut crediting = claim(5, 0, Need::new("held stage").mem_gib(20), true);
    crediting.credit = vec![7];
    let small = claim(4, 2, Need::new("test build").mem_gib(2), false);
    let procs = processes(&[
        (3, 2, 5 * GIB),
        (5, 2, 5 * GIB),
        (1, 3, 0),
        (7, 2, 0),
        (4, 2, 0),
        (2, 0, 0),
    ]);
    for (held, pid) in [(ancestor, 1), (crediting, 7)] {
        let cohort = kept_by(
            claim(pid, 1, slice_budget(30), false),
            crate::Accounting::CohortOwned,
        );
        let ledger = [held, cohort.clone(), small.clone()];
        let mine = swept_look(&cohort, &ledger[..2], &procs, &measured());
        assert!(waits_stuck(&mine, false), "{pid}: {mine:?}");
        let behind = swept_look(&small, &ledger, &procs, &measured());
        assert!(behind.is_ok(), "{pid}: {behind:?}");
        let trees = sweep(&ledger, &[(3, 5 * GIB), (5, 5 * GIB)]);
        assert_eq!(
            spared(&ledger[..2], Some(&trees), Some(&procs), &measured(),),
            [None, Some(Spared::Within(10 * GIB - 1))],
            "{pid}"
        );
    }
}

/// KIN UNSEEN: a held claim that credits the waiter's own process is its kin's even in a look
/// with no sweep, and its end is none the waiter's turn can wait for, though the look sees none
/// of its tree and so reckons it may give back as much again as it claims ([`End::unseen`]). A
/// 42 GiB slice budget on 35 GiB, its own process credited by a held 5 GiB claim: every look
/// leaves that claim out of its turn, finds it 7 GiB and a byte short with no end it can wait
/// for, and names it STUCK; a 3 GiB claim behind it goes first. NEGATIVE CONTROL (mutant
/// M-KIN-UNSEEN, what the kin's ends may give back counted toward the budget): the budget keeps
/// its place sparing nothing, and the 3 GiB claim waits for an end that comes only after the
/// budget has run.
#[test]
fn a_waiters_unseen_kin_brings_none_of_what_it_lacks() {
    let mut kin = claim(3, 0, Need::new("held stage").mem_gib(5), true);
    kin.credit = vec![1];
    let budget = claim(1, 1, slice_budget(42), false);
    let small = claim(2, 2, Need::new("test build").mem_gib(3), false);
    let ledger = [kin, budget.clone(), small.clone()];
    let behind = look_with(&small, &ledger, &HashMap::new(), &measured());
    assert!(behind.is_ok(), "{behind:?}");
    assert_eq!(
        spared(&ledger, None, None, &measured(),),
        [None, Some(Spared::Stuck), None]
    );
}

/// ON ITS WHOLE NEED: a waiter's turn in every look is reckoned on what its own look asks, its
/// whole need, though what its holder's tree holds already lowers what it reserves against the
/// claims behind it ([`reservations`]). A 38 GiB budget (a byte's margin) whose process uses
/// 4 GiB, under an ancestor that holds 20 GiB and whose tree beside it uses 5, on 35 GiB: its own
/// look lacks 3 GiB and a byte, which no end it can wait for brings, and names it STUCK; so does
/// every look behind it, which admits a 3 GiB claim at once, and `spared`. So too where its look
/// counts every claim ahead of it, and every look walks it so: a 36 GiB budget with nothing ahead
/// of it, whose process uses 4 GiB, lacks 1 GiB and a byte on 35 GiB and is STUCK, and a 4 GiB
/// claim behind it starts at once. NEGATIVE CONTROL (mutant M-FIT-RESERVE, a waiter's fit in the
/// walk reckoned on its reservation): the 4 GiB claim's look has the 36 GiB budget start now on
/// its 32 GiB reservation and holds the claim to the 3 GiB left.
#[test]
fn a_waiters_turn_is_reckoned_on_the_whole_need_its_own_look_asks() {
    let parent = claim(9, 0, Need::new("held release").mem_gib(20), true);
    let budget = claim(11, 1, slice_budget(38), false);
    let small = claim(12, 2, Need::new("test build").mem_gib(3), false);
    let procs = processes(&[(9, 2, 5 * GIB), (11, 9, 4 * GIB), (12, 2, 0), (2, 0, 0)]);
    let ledger = [parent, budget.clone(), small.clone()];
    let mine = swept_look(&budget, &ledger[..2], &procs, &measured());
    assert!(waits_stuck(&mine, true), "{mine:?}");
    let behind = swept_look(&small, &ledger, &procs, &measured());
    assert!(behind.is_ok(), "{behind:?}");
    let trees = sweep(&ledger, &[(9, 5 * GIB), (11, 4 * GIB)]);
    assert_eq!(
        spared(&ledger[..2], Some(&trees), Some(&procs), &measured(),),
        [None, Some(Spared::Stuck)]
    );

    let budget = claim(11, 1, slice_budget(36), false);
    let small = claim(12, 2, Need::new("test build").mem_gib(4), false);
    let procs = processes(&[(11, 2, 4 * GIB), (12, 2, 0), (2, 0, 0)]);
    let ledger = [budget.clone(), small.clone()];
    let mine = swept_look(&budget, &ledger[..1], &procs, &measured());
    assert!(waits_stuck(&mine, true), "{mine:?}");
    let behind = swept_look(&small, &ledger, &procs, &measured());
    assert!(behind.is_ok(), "{behind:?}");
}

/// SHARED HOLDER (ATERM-KI-1113-341): a held claim's end gives back its holder's whole tree only
/// where that holder holds no other claim the look read ([`Other::sole`]), as every look of the
/// ledger finds. On 10 GiB, pid 5 holds N, 10 GiB, and its tree uses 25; beside N it holds K, a
/// 10 GiB claim crediting the 30 GiB budget's process (the budget's kin, which its own look
/// leaves out), or it waits for a 1 GiB claim behind the budget, which the budget's own look does
/// not walk. Either way pid 5 lives on past N's end, which gives back N's 10 GiB alone: the
/// budget (a byte's margin) lacks 20 GiB and a byte, which no end it can wait for brings, and is
/// STUCK in its own look, in the look of an 8 GiB claim behind it, which is admitted, and in
/// `spared`. NEGATIVE CONTROL (mutant M-SOLE-AHEAD, a holder's claims counted among the claims
/// ahead alone, the rule before): the budget's own look gives N's end all 25 GiB of pid 5's tree
/// and keeps the budget in its place sparing 5 GiB less a byte.
#[test]
fn a_held_claims_end_gives_back_its_holders_tree_only_where_it_holds_nothing_else() {
    let n = claim(5, 0, Need::new("held build").mem_gib(10), true);
    let mut k = claim(5, 1, Need::new("held stage").mem_gib(10), true);
    k.credit = vec![1];
    let budget = claim(1, 2, slice_budget(30), false);
    let next = claim(5, 3, Need::new("its next build").mem_gib(1), false);
    let small = claim(4, 4, Need::new("test build").mem_gib(8), false);
    let procs = processes(&[(5, 2, 25 * GIB), (1, 2, 0), (4, 2, 0), (2, 0, 0)]);
    let reading = Reading::of_gib(128, 10, 18);
    for ledger in [
        vec![n.clone(), k, budget.clone(), small.clone()],
        vec![n, budget.clone(), next, small.clone()],
    ] {
        let mine = swept_look(&budget, &ledger, &procs, &reading);
        assert!(waits_stuck(&mine, true), "{mine:?}");
        let behind = swept_look(&small, &ledger, &procs, &reading);
        assert!(behind.is_ok(), "{behind:?}");
        let trees = sweep(&ledger, &[(5, 25 * GIB)]);
        let listed = spared(&ledger, Some(&trees), Some(&procs), &reading);
        let at = ledger
            .iter()
            .position(|r| r.order() == budget.order())
            .expect("the budget is in the ledger");
        assert_eq!(listed[at], Some(Spared::Stuck), "{listed:?}");
    }
}

/// OWN LOOK: every look stands each waiter as its own look stands it, beside the claims that look
/// counts, each waiter there at its own standing ([`memory_turn`]'s OWN LOOK), each claim reckoned
/// among every claim the look read before it leaves any out ([`look_read`]). On 35 GiB a process
/// (3) holds 20 GiB its tree uses whole, and a 30 GiB unit under 3 waits behind one claim its own
/// look leaves out: another process's 10 GiB build, a 20 GiB lead of the unit's own process, or a
/// 10 GiB lead of 3's. Each claim starts in its own look and so in the look of a 2 GiB claim behind
/// them, which waits on no room and spares 3 GiB: once the unit holds and the claim ahead has not
/// started, that claim's own look counts the unit and waits, and the 2 GiB claim would start beside
/// them, leaving 3 GiB ([`once_started`]). A WAITING ANCESTOR: a 16 GiB build under a
/// waiting 17 GiB claim stands beside a held 20 GiB alone, sparing 19 GiB, and a 9 GiB claim behind
/// them starts within the 18 GiB the ancestor spares. THE ASKER'S OWN LEAD: a 10 GiB boot of a 2
/// GiB margin behind a 15 GiB lead and a held 25 GiB spares 8 GiB in its own look, and so in the
/// look of a 10 GiB claim of the lead's process, which leaves the lead out and waits. TREES ROOTED
/// AT EVERY CLAIM: an inside 30 GiB boot of a 3 GiB margin finds a held 25 GiB claim reserving all
/// of it, though a waiting child of that claim's process uses 6 GiB, its own. ALONE: `spared` names
/// a budget under a claim held alone stuck, as its own look does, and a claim beside it sparing
/// nothing. ITS OWN CLAIM: a 50 GiB budget beside its process's held 10 GiB, whose tree uses 25
/// GiB, is stuck in its own look, since that end gives back only what it claims. NEGATIVE CONTROLS:
/// M-INSIDE-ALL, M-OWN-SIBLING and M-OWN-KIN (the unit counting the build, its process's lead or
/// 3's) fund the unit by that claim's end and admit the 2 GiB claim; M-OWN-KIN also stands the 16
/// GiB build beside its ancestor's claim, sparing 2 GiB, and holds the 9 GiB claim back;
/// M-RECKONED-OFF (each look standing a waiter over its own claims alone) stands the boot without
/// the lead in the 10 GiB claim's look and admits it; M-ROOTS-COUNTED (trees rooted at the claims a
/// look counts) gives the 30 GiB boot the child's 6 GiB as room; M-ALONE-ALL (every waiter sparing
/// nothing beside a claim held alone) lists the stuck budget sparing nothing; M-OWN-UNCOUNTED (the
/// look's own claim left out of what it reads alike) gives the 50 GiB budget the held claim's whole
/// tree and keeps it in its place.
#[test]
fn every_look_stands_a_waiter_as_its_own_look_stands_it() {
    let reading = measured();
    let wants =
        |need: u64, margin: u64, reserved: u64, passes: Option<u64>, stuck: bool| Short::Memory {
            need,
            margin,
            available: 35 * GIB,
            reserved,
            passes,
            stuck_ahead: 0,
            stuck,
        };
    let lead = |pid: u32, asks: u64, gib: u64| {
        claim(
            pid,
            asks,
            Need::new("slice compiler")
                .rank(Rank::Release)
                .mem_gib(gib)
                .leads(),
            false,
        )
    };

    let parent = claim(3, 0, Need::new("held release").mem_gib(20), true);
    let unit = claim(1, 2, Need::new("compile unit").mem_gib(30), false);
    let small = claim(4, 3, Need::new("test build").mem_gib(2), false);
    for (ahead, reserves) in [
        (claim(5, 1, Need::new("plain build").mem_gib(10), false), 10),
        (lead(1, 1, 20), 20),
        (lead(3, 1, 10), 10),
    ] {
        let ledger = [parent.clone(), ahead.clone(), unit.clone(), small.clone()];
        let procs = processes(&[(3, 2, 20 * GIB), (5, 2, 0), (1, 3, 0), (4, 2, 0), (2, 0, 0)]);
        let what = format!("behind {} of {}", ahead.need.what, ahead.pid);
        let theirs = swept_look(&ahead, &ledger, &procs, &reading);
        assert!(theirs.is_ok(), "{what}: {theirs:?}");
        let mine = swept_look(&unit, &ledger, &procs, &reading);
        assert!(mine.is_ok(), "{what}: {mine:?}");
        let behind = swept_look(&small, &ledger, &procs, &reading).expect_err("no room is left");
        assert_eq!(
            behind.shorts,
            [wants(2 * GIB, 0, (reserves + 30) * GIB, None, false)],
            "{what}"
        );
        let trees = sweep(&ledger, &[(3, 20 * GIB)]);
        assert_eq!(
            spared(&ledger, Some(&trees), Some(&procs), &reading),
            [None, None, None, Some(Spared::Within(3 * GIB))],
            "{what}"
        );
    }

    let held = claim(50, 0, Need::new("held build").mem_gib(20), true);
    let ancestor = claim(10, 1, Need::new("release").mem_gib(17), false);
    let child = claim(11, 2, Need::new("child build").mem_gib(16), false);
    let later = claim(30, 3, Need::new("test build").mem_gib(9), false);
    let ledger = [held, ancestor, child.clone(), later.clone()];
    let procs = processes(&[(50, 2, 0), (10, 2, 0), (11, 10, 0), (30, 2, 0), (2, 0, 0)]);
    let mine = swept_look(&child, &ledger, &procs, &reading).expect_err("it waits");
    assert_eq!(mine.shorts, [wants(16 * GIB, 0, 20 * GIB, None, false)]);
    let behind = swept_look(&later, &ledger, &procs, &reading);
    assert!(behind.is_ok(), "{behind:?}");
    let trees = sweep(&ledger, &[]);
    assert_eq!(
        spared(&ledger, Some(&trees), Some(&procs), &reading),
        [
            None,
            Some(Spared::Within(18 * GIB)),
            Some(Spared::Within(19 * GIB)),
            None
        ]
    );

    let held = claim(50, 0, Need::new("held build").mem_gib(25), true);
    let boot = claim(
        30,
        2,
        Need::new("vm boot").mem_gib(10).margin(2 * GIB),
        false,
    );
    let asker = claim(20, 3, Need::new("test build").mem_gib(10), false);
    let ledger = [held, lead(20, 1, 15), boot.clone(), asker.clone()];
    let procs = processes(&[(50, 2, 0), (20, 2, 0), (30, 2, 0), (2, 0, 0)]);
    let theirs = swept_look(&boot, &ledger, &procs, &reading).expect_err("the boot waits");
    assert_eq!(
        theirs.shorts,
        [wants(10 * GIB, 2 * GIB, 40 * GIB, Some(10 * GIB), false)]
    );
    let mine = swept_look(&asker, &ledger, &procs, &reading).expect_err("it waits");
    assert_eq!(
        mine.shorts,
        [wants(10 * GIB, 0, 35 * GIB, Some(8 * GIB), false)]
    );
    let trees = sweep(&ledger, &[]);
    assert_eq!(
        spared(&ledger, Some(&trees), Some(&procs), &reading),
        [
            None,
            Some(Spared::Within(20 * GIB)),
            Some(Spared::Within(8 * GIB)),
            Some(Spared::Within(0))
        ]
    );

    let held = claim(400, 0, Need::new("held build").mem_gib(25), true);
    let release = claim(100, 0, Need::new("held release").mem_gib(4), true);
    let boot = claim(
        101,
        1,
        Need::new("vm boot").mem_gib(30).margin(3 * GIB),
        false,
    );
    let asker = claim(300, 2, Need::new("test build").mem_gib(10), false);
    let child = claim(401, 3, Need::new("child build").mem_gib(8), false);
    let ledger = [held, release, boot.clone(), asker.clone(), child];
    let procs = processes(&[
        (400, 2, 0),
        (401, 400, 6 * GIB),
        (100, 2, 4 * GIB),
        (101, 100, 0),
        (300, 2, 0),
        (2, 0, 0),
    ]);
    let mine = swept_look(&boot, &ledger, &procs, &reading).expect_err("the boot waits");
    assert_eq!(
        mine.shorts,
        [wants(30 * GIB, 3 * GIB, 25 * GIB, None, false)]
    );
    let behind = swept_look(&asker, &ledger, &procs, &reading).expect_err("it waits");
    assert_eq!(
        behind.shorts,
        [wants(10 * GIB, 0, 55 * GIB, Some(2 * GIB), false)]
    );
    let trees = sweep(&ledger, &[(401, 6 * GIB), (100, 4 * GIB)]);
    assert_eq!(
        spared(&ledger, Some(&trees), Some(&procs), &reading),
        [
            None,
            None,
            Some(Spared::Within(2 * GIB)),
            Some(Spared::Within(0)),
            None
        ]
    );

    let mut alone = claim(100, 0, Need::new("cold release slice").mem_gib(20), true);
    if let Some(held) = alone.held.as_mut() {
        held.alone = true;
    }
    let under = claim(101, 1, slice_budget(40), false);
    let beside = claim(200, 2, Need::new("test build").mem_gib(2), false);
    let ledger = [alone, under.clone(), beside.clone()];
    let procs = processes(&[(100, 2, 20 * GIB), (101, 100, 0), (200, 2, 0), (2, 0, 0)]);
    let mine = swept_look(&under, &ledger, &procs, &reading).expect_err("it waits");
    assert_eq!(mine.shorts, [wants(40 * GIB, 1, 0, None, true)]);
    let theirs = swept_look(&beside, &ledger, &procs, &reading).expect_err("it waits");
    assert!(
        matches!(theirs.shorts.as_slice(), [Short::Alone { pid: 100, .. }]),
        "{theirs:?}"
    );
    let trees = sweep(&ledger, &[(100, 20 * GIB)]);
    assert_eq!(
        spared(&ledger, Some(&trees), Some(&procs), &reading),
        [None, Some(Spared::Stuck), Some(Spared::Within(0))]
    );

    let own = claim(11, 0, Need::new("held build").mem_gib(10), true);
    let budget = claim(11, 1, slice_budget(50), false);
    let ledger = [own, budget.clone()];
    let procs = processes(&[(11, 2, 25 * GIB), (2, 0, 0)]);
    let mine = swept_look(&budget, &ledger, &procs, &reading);
    assert!(waits_stuck(&mine, true), "{mine:?}");
}

/// OWN LOOK: what a waiter its own look keeps in its place spares is no more than it spares once
/// each set of the waiters ahead that start now has started ([`once_started`]): each that is none
/// of its kin's is a held claim its look counts, and each waiter's INSIDE is read again. A SIBLING:
/// on 35 GiB beside a held 15 GiB, a process waits for 8 GiB, which starts now, and for 25 GiB,
/// which its own look stands without its sibling and which lacks 5 GiB there. Once the 8 GiB has
/// started the 25 GiB lacks 13, which only the held claim's end brings: it spares 2 GiB, and a 5
/// GiB claim behind it waits. INSIDE: on 45 GiB, under its parent's held 10 GiB and beside a held
/// 15 GiB, a 35 GiB build's own look counts the held 15 GiB alone and drops another process's 8 GiB
/// build, which starts now; it spares the same 2 GiB, and the 5 GiB claim waits again. TREE: on 35
/// GiB beside held 9 GiB and 20 GiB claims, a process whose tree uses 2 GiB waits for 3 GiB, which
/// starts now reserving 1 GiB, and for 16 GiB, which lacks 10 GiB by its own look and spares 10 GiB
/// there. Once the 3 GiB holds, the 16 GiB lacks 11 GiB, which the 9 GiB claim's end and the 3
/// GiB's bring with 1 GiB over: it spares 1 GiB, and a 2 GiB claim behind it waits. CREDIT: on 38
/// GiB beside held 6 GiB and 20 GiB claims, a process whose tree uses 4 GiB waits for a STUCK 38
/// GiB, which takes that tree's credit, then for 4 GiB, which starts now reserving all 4 GiB, and
/// for 20 GiB, which spares 12 GiB by its own look. Once the 4 GiB holds it takes the credit and
/// reserves nothing: the 20 GiB lacks 8 GiB, which the 6 GiB claim's end and the 4 GiB's bring with
/// 2 GiB over, so it spares 2 GiB and an 8 GiB claim behind it waits. COUNTED: on 35 GiB beside a
/// held 3 GiB, another process's 33 GiB of a 1 GiB margin counts a starting 4 GiB, whose tree uses
/// 2 GiB, as a waiter whose end gives back its 2 GiB reservation: it lacks 4 GiB and spares 1 GiB.
/// Once the 4 GiB holds, its end gives back all 4 GiB, which is what the 33 GiB lacks: it spares
/// nothing, and a 1 GiB claim behind it waits. KIN: a waiting ancestor's 10 GiB claim that starts
/// now lowers nothing a 16 GiB child build under it spares beside a held 20 GiB, 19 GiB, since the
/// child's own look leaves its kin's claims out whether they wait or hold. SUBSET: on 35 GiB beside
/// a held 3 GiB, two processes whose trees use 2 GiB each wait for 4 GiB, both starting now, and a
/// 33 GiB of a 1 GiB margin behind them spares 1 GiB by its own look and beside both held; beside
/// either alone held, whose end gives back all 4 GiB while the other's gives back its 2 GiB
/// reservation, it lacks 6 GiB and those ends bring it exactly: it spares nothing, and a 1 GiB
/// claim waits. PAIR: the SIBLING ledger with two starting 4 GiB in place of the 8 GiB: beside
/// either alone held the 25 GiB spares 6 GiB, beside both 2 GiB, and a 3 GiB claim behind waits.
/// LEAVES: on 35 GiB beside a held 10 GiB, a process whose tree uses 4 GiB waits for a STUCK 40 GiB
/// of a 1 GiB margin, which takes the tree's credit, and for 6 GiB, which starts now; another's 21
/// GiB spares 4 GiB by its own look. Once the 6 GiB holds it takes the credit and reserves 2 GiB:
/// the 21 GiB would start and leave 2 GiB, so a 3 GiB claim behind it waits. FOUR: beside a held 15
/// GiB, a process waits for four starting 1 GiB claims and a 25 GiB, which spares 10 GiB less each
/// that holds: 6 GiB, and a 5 GiB claim behind goes first. FIVE: with five, past [`STARTERS_MAX`],
/// it spares nothing, and a 2 GiB claim waits. AHEAD: on 43 GiB beside held 20 GiB and 3 GiB
/// claims, a 30 GiB lead waits sparing 10 GiB, a critical 4 GiB link of a 3 GiB margin starts past
/// it, and an 8 GiB build behind both spares 2 GiB by its own look. Once the link holds, the 30
/// GiB, stood again beside it, spares 6 GiB, and the 8 GiB, which passes it only within that,
/// spares 1 GiB: a 2 GiB claim behind waits. HELD LEAD: on 35 GiB, under its parent's held 1 GiB
/// and beside a held 10 GiB, a 26 GiB build waits behind two leads that start now: a 2 GiB one
/// asking 1 GiB of disk, which is not sure for it and so ends its sure leads, and a critical 4 GiB
/// link of a 3 GiB margin. Its own look counts the held 10 GiB alone and spares 9 GiB. Once the 2
/// GiB holds, the link is sure for it: beside the 10 GiB and the 2 GiB, the link starting, it lacks
/// 10 GiB, which the 10 GiB's end brings exactly, so it spares nothing and a 3 GiB claim behind
/// waits. HELD KIN: on 35 GiB beside a held 10 GiB, a critical 5 GiB link of a 3 GiB margin asking
/// 1 GiB of disk and another process's 4 GiB start now, and a 33 GiB of a 1 GiB margin under that
/// process, whose own tree uses 30 GiB, is STUCK. Once the 4 GiB holds, the 33 GiB is inside it,
/// its look counting the held 10 GiB alone, and spares 1 GiB: a 14 GiB build behind them, past the
/// link starting, lacks 4 GiB and spares nothing, and a 2 GiB claim behind waits. HELD KIN LEAD:
/// the HELD LEAD ledger with the 2 GiB lead its parent's: once that holds, the link is sure for the
/// build, which lacks 8 GiB and spares 2 GiB, so the 3 GiB claim waits. GRANT: on 30 GiB and 18
/// cores, under its parent's held 1 GiB on 6 cores and beside a held 20 GiB on 4, a 12 GiB build
/// counts a release lead of 12 GiB and a core, which spares 17 GiB, and spares 6 GiB by its own
/// look; another process's 3 GiB of 12 cores, whose process waits for more, starts now on the 4
/// cores it is granted, 8 at most, what the held claims leave it. Once it holds it is a parent of
/// the lead's too, and on any of those grants the lead is sure for the build, which spares 3 GiB: a
/// 5 GiB claim behind waits. THRESHOLD: with the parent on 16 cores, the 20 GiB on none, and the
/// lead of the 3 GiB's rank asking after it, so that the 3 GiB's share counts no other party, the 3
/// GiB is granted the 2 cores left on an idle machine, where the parents hold every core and the
/// lead is not sure for the build, which then spares 6 GiB, and 1 under load, where it is and the
/// build spares 3 GiB: no look reckons the load at its admission, so the build spares nothing and a
/// 4 GiB claim behind waits. LOOKS: there, another process's 12 GiB waiting after the 3 GiB and
/// before the build, which none of its looks reads inside, spares 2 GiB whatever the build's
/// INSIDE, and a 2 GiB claim behind it goes first. CORE: with the parent on 17 cores and the 3
/// GiB's process under it, whose own look leaves the parent's cores out, the 3 GiB is granted up to
/// all 12 it asks, and on even one the lead is not sure for the build, which spares 6 GiB. NEGATIVE
/// CONTROLS: M-STARTED-OFF (what a waiter spares by its own look alone) has the 25 GiB and the 35
/// GiB spare 10 GiB, and admits the 5 GiB claim, which delays either past the held claim's end;
/// M-STARTED-NOW (each set's turn holding its starters at what they reserve now) has the CREDIT 20
/// GiB spare 8 GiB and admits the 8 GiB claim, which delays it past the ends of the 6 GiB and the 4
/// GiB; M-STARTED-COUNTED (no starter its own look counts held) has the 33 GiB spare 1 GiB and
/// admits the 1 GiB claim; M-STARTED-KIN (its kin's starting claims lowering it too) has the child
/// spare 9 GiB; M-STARTED-ALL (the set of every starter alone) has SUBSET's 33 GiB spare 1 GiB and
/// admits the 1 GiB claim; M-STARTED-ONE (each starter alone) has PAIR's 25 GiB spare 6 GiB and
/// admits the 3 GiB claim; M-STARTED-LEAVES (a set in which it would start bounding nothing) has
/// the 21 GiB spare 4 GiB and admits the 3 GiB claim, which holds it past the next look;
/// M-STARTERS-SPARE (its own spare past [`STARTERS_MAX`]) admits FIVE's 2 GiB claim; and
/// M-STARTED-STALE (each waiter ahead taken at how it stood before the set started) has AHEAD's 8
/// GiB spare 2 GiB and admits the 2 GiB claim, which delays it once the held claims end; and
/// M-INSIDE-STALE (each waiter's INSIDE as it was before the set started) has HELD LEAD's 26 GiB
/// spare 3 GiB and HELD KIN's 14 GiB 2 GiB, and admits the claim behind each; M-STARTED-OWNKIN (its
/// kin's starters left out of its sets) has HELD KIN LEAD's 26 GiB spare 5 GiB and admits the 3 GiB
/// claim; M-GRANT-ASKED (a starter granted at most all it asks) has GRANT's build spare nothing;
/// M-GRANT-SAME (each starter held on its most cores alone) and M-GRANT-TWO (on two at the fewest)
/// have THRESHOLD's build spare 6 GiB and admit the 4 GiB claim; M-GRANT-NONE (on none at the
/// fewest) has CORE's build spare nothing; and M-GRANT-ALL (the INSIDE of every waiter compared)
/// has LOOKS' 12 GiB spare nothing and keeps the 2 GiB claim waiting.
#[test]
fn a_waiter_stands_beside_each_waiter_that_starts_ahead_of_it() {
    let held = claim(9, 0, Need::new("held build").mem_gib(15), true);
    let first = claim(20, 1, Need::new("test binary").mem_gib(8), false);
    let second = claim(20, 2, Need::new("test binary").mem_gib(25), false);
    let small = claim(30, 3, Need::new("test build").mem_gib(5), false);
    let ledger = [held, first.clone(), second.clone(), small.clone()];
    let procs = processes(&[(9, 2, 0), (20, 2, 0), (30, 2, 0), (2, 0, 0)]);
    let reading = measured();
    assert!(swept_look(&first, &ledger, &procs, &reading).is_ok());
    let theirs = swept_look(&second, &ledger, &procs, &reading);
    assert!(waits_stuck(&theirs, false), "{theirs:?}");
    let behind = swept_look(&small, &ledger, &procs, &reading).expect_err("it waits");
    assert_eq!(
        behind.shorts,
        [Short::Memory {
            need: 5 * GIB,
            margin: 0,
            available: 35 * GIB,
            reserved: 48 * GIB,
            passes: Some(2 * GIB),
            stuck_ahead: 0,
            stuck: false,
        }]
    );
    let trees = sweep(&ledger, &[]);
    assert_eq!(
        spared(&ledger, Some(&trees), Some(&procs), &reading),
        [
            None,
            None,
            Some(Spared::Within(2 * GIB)),
            Some(Spared::Within(5 * GIB))
        ]
    );

    let parent = claim(3, 0, Need::new("held release").mem_gib(10), true);
    let held = claim(9, 0, Need::new("held build").mem_gib(15), true);
    let other = claim(40, 1, Need::new("plain build").mem_gib(8), false);
    let inside = claim(1, 2, Need::new("compile unit").mem_gib(35), false);
    let small = claim(30, 3, Need::new("test build").mem_gib(5), false);
    let ledger = [parent, held, other.clone(), inside.clone(), small.clone()];
    let procs = processes(&[
        (3, 2, 0),
        (1, 3, 0),
        (9, 2, 0),
        (40, 2, 0),
        (30, 2, 0),
        (2, 0, 0),
    ]);
    let reading = Reading::of_gib(128, 45, 18);
    assert!(swept_look(&other, &ledger, &procs, &reading).is_ok());
    let theirs = swept_look(&inside, &ledger, &procs, &reading);
    assert!(waits_stuck(&theirs, false), "{theirs:?}");
    let behind = swept_look(&small, &ledger, &procs, &reading).expect_err("it waits");
    assert_eq!(
        behind.shorts,
        [Short::Memory {
            need: 5 * GIB,
            margin: 0,
            available: 45 * GIB,
            reserved: 68 * GIB,
            passes: Some(2 * GIB),
            stuck_ahead: 0,
            stuck: false,
        }]
    );
    let trees = sweep(&ledger, &[]);
    assert_eq!(
        spared(&ledger, Some(&trees), Some(&procs), &reading),
        [
            None,
            None,
            None,
            Some(Spared::Within(2 * GIB)),
            Some(Spared::Within(5 * GIB))
        ]
    );

    let first_held = claim(9, 0, Need::new("held build").mem_gib(9), true);
    let second_held = claim(8, 0, Need::new("held build").mem_gib(20), true);
    let first = claim(20, 1, Need::new("test binary").mem_gib(3), false);
    let second = claim(20, 2, Need::new("test binary").mem_gib(16), false);
    let small = claim(30, 3, Need::new("test build").mem_gib(2), false);
    let ledger = [
        first_held,
        second_held,
        first.clone(),
        second.clone(),
        small.clone(),
    ];
    let procs = processes(&[
        (9, 2, 0),
        (8, 2, 0),
        (20, 2, 2 * GIB),
        (30, 2, 0),
        (2, 0, 0),
    ]);
    let reading = measured();
    assert!(swept_look(&first, &ledger, &procs, &reading).is_ok());
    let behind = swept_look(&small, &ledger, &procs, &reading);
    assert!(behind.is_err(), "{behind:?}");
    let trees = sweep(&ledger, &[(20, 2 * GIB)]);
    assert_eq!(
        spared(&ledger, Some(&trees), Some(&procs), &reading)[3],
        Some(Spared::Within(GIB))
    );

    let first_held = claim(9, 0, Need::new("held build").mem_gib(6), true);
    let second_held = claim(8, 0, Need::new("held build").mem_gib(20), true);
    let budget = claim(20, 1, Need::new("vm boot").mem_gib(38).margin(GIB), false);
    let first = claim(20, 2, Need::new("test binary").mem_gib(4), false);
    let second = claim(20, 3, Need::new("test binary").mem_gib(20), false);
    let small = claim(30, 4, Need::new("test build").mem_gib(8), false);
    let ledger = [
        first_held,
        second_held,
        budget,
        first.clone(),
        second,
        small.clone(),
    ];
    let procs = processes(&[
        (9, 2, 0),
        (8, 2, 0),
        (20, 2, 4 * GIB),
        (30, 2, 0),
        (2, 0, 0),
    ]);
    let reading = Reading::of_gib(128, 38, 18);
    assert!(swept_look(&first, &ledger, &procs, &reading).is_ok());
    let behind = swept_look(&small, &ledger, &procs, &reading);
    assert!(behind.is_err(), "{behind:?}");
    let trees = sweep(&ledger, &[(20, 4 * GIB)]);
    assert_eq!(
        spared(&ledger, Some(&trees), Some(&procs), &reading)[2..5],
        [Some(Spared::Stuck), None, Some(Spared::Within(2 * GIB))]
    );

    let held = claim(9, 0, Need::new("held build").mem_gib(3), true);
    let first = claim(20, 1, Need::new("test binary").mem_gib(4), false);
    let boot = claim(30, 2, Need::new("vm boot").mem_gib(33).margin(GIB), false);
    let small = claim(40, 3, Need::new("test build").mem_gib(1), false);
    let ledger = [held, first.clone(), boot, small.clone()];
    let procs = processes(&[
        (9, 2, 0),
        (20, 2, 2 * GIB),
        (30, 2, 0),
        (40, 2, 0),
        (2, 0, 0),
    ]);
    let reading = measured();
    assert!(swept_look(&first, &ledger, &procs, &reading).is_ok());
    let behind = swept_look(&small, &ledger, &procs, &reading);
    assert!(behind.is_err(), "{behind:?}");
    let trees = sweep(&ledger, &[(20, 2 * GIB)]);
    assert_eq!(
        spared(&ledger, Some(&trees), Some(&procs), &reading)[2],
        Some(Spared::Within(0))
    );

    let held = claim(50, 0, Need::new("held build").mem_gib(20), true);
    let ancestor = claim(10, 1, Need::new("release").mem_gib(10), false);
    let child = claim(11, 2, Need::new("child build").mem_gib(16), false);
    let ledger = [held, ancestor.clone(), child];
    let procs = processes(&[(50, 2, 0), (10, 2, 0), (11, 10, 0), (2, 0, 0)]);
    assert!(swept_look(&ancestor, &ledger, &procs, &measured()).is_ok());
    let trees = sweep(&ledger, &[]);
    assert_eq!(
        spared(&ledger, Some(&trees), Some(&procs), &measured()),
        [None, None, Some(Spared::Within(19 * GIB))]
    );

    let held = claim(9, 0, Need::new("held build").mem_gib(3), true);
    let one = claim(20, 1, Need::new("test binary").mem_gib(4), false);
    let two = claim(21, 2, Need::new("test binary").mem_gib(4), false);
    let boot = claim(30, 3, Need::new("vm boot").mem_gib(33).margin(GIB), false);
    let small = claim(40, 4, Need::new("test build").mem_gib(1), false);
    let ledger = [held, one, two, boot, small.clone()];
    let procs = processes(&[
        (9, 2, 0),
        (20, 2, 2 * GIB),
        (21, 2, 2 * GIB),
        (30, 2, 0),
        (40, 2, 0),
        (2, 0, 0),
    ]);
    let behind = swept_look(&small, &ledger, &procs, &measured());
    assert!(behind.is_err(), "{behind:?}");
    let trees = sweep(&ledger, &[(20, 2 * GIB), (21, 2 * GIB)]);
    assert_eq!(
        spared(&ledger, Some(&trees), Some(&procs), &measured())[3],
        Some(Spared::Within(0))
    );

    let held = claim(9, 0, Need::new("held build").mem_gib(15), true);
    let first = claim(20, 1, Need::new("test binary").mem_gib(4), false);
    let second = claim(20, 2, Need::new("test binary").mem_gib(4), false);
    let wide = claim(20, 3, Need::new("test binary").mem_gib(25), false);
    let small = claim(30, 4, Need::new("test build").mem_gib(3), false);
    let ledger = [held, first, second, wide, small.clone()];
    let procs = processes(&[(9, 2, 0), (20, 2, 0), (30, 2, 0), (2, 0, 0)]);
    let behind = swept_look(&small, &ledger, &procs, &measured());
    assert!(behind.is_err(), "{behind:?}");
    let trees = sweep(&ledger, &[]);
    assert_eq!(
        spared(&ledger, Some(&trees), Some(&procs), &measured())[3],
        Some(Spared::Within(2 * GIB))
    );

    let held = claim(9, 0, Need::new("held build").mem_gib(10), true);
    let budget = claim(20, 1, Need::new("vm boot").mem_gib(40).margin(GIB), false);
    let first = claim(20, 2, Need::new("test binary").mem_gib(6), false);
    let wide = claim(30, 3, Need::new("wide build").mem_gib(21), false);
    let small = claim(40, 4, Need::new("test build").mem_gib(3), false);
    let ledger = [held, budget, first.clone(), wide, small.clone()];
    let procs = processes(&[
        (9, 2, 0),
        (20, 2, 4 * GIB),
        (30, 2, 0),
        (40, 2, 0),
        (2, 0, 0),
    ]);
    assert!(swept_look(&first, &ledger, &procs, &measured()).is_ok());
    let behind = swept_look(&small, &ledger, &procs, &measured()).expect_err("it waits");
    let passes = match behind.shorts.as_slice() {
        [Short::Memory { passes, .. }] => *passes,
        _ => None,
    };
    assert_eq!(passes, Some(2 * GIB), "{behind:?}");
    let trees = sweep(&ledger, &[(20, 4 * GIB)]);
    assert_eq!(
        spared(&ledger, Some(&trees), Some(&procs), &measured())[1..4],
        [Some(Spared::Stuck), None, Some(Spared::Within(2 * GIB))]
    );

    for (starters, spares, small) in [(4, 6, 5), (5, 0, 2)] {
        let mut ledger = vec![claim(9, 0, Need::new("held build").mem_gib(15), true)];
        for k in 1..=starters {
            ledger.push(claim(20, k, Need::new("test binary").mem_gib(1), false));
        }
        ledger.push(claim(
            20,
            starters + 1,
            Need::new("test binary").mem_gib(25),
            false,
        ));
        let build = claim(
            30,
            starters + 2,
            Need::new("test build").mem_gib(small),
            false,
        );
        ledger.push(build.clone());
        let procs = processes(&[(9, 2, 0), (20, 2, 0), (30, 2, 0), (2, 0, 0)]);
        let behind = swept_look(&build, &ledger, &procs, &measured());
        assert_eq!(behind.is_ok(), spares >= small, "{starters}: {behind:?}");
        let trees = sweep(&ledger, &[]);
        let at = usize::try_from(starters + 1).expect("an index");
        assert_eq!(
            spared(&ledger, Some(&trees), Some(&procs), &measured())[at],
            Some(Spared::Within(spares * GIB)),
            "{starters}"
        );
    }

    let reading = Reading::of_gib(128, 43, 18);
    let first = claim(9, 0, Need::new("held build").mem_gib(20), true);
    let second = claim(8, 0, Need::new("held build").mem_gib(3), true);
    let lead = claim(
        6,
        1,
        Need::new("slice compiler")
            .rank(Rank::Release)
            .mem_gib(30)
            .leads(),
        false,
    );
    let link = claim(
        7,
        2,
        Need::new("final link")
            .rank(Rank::Release)
            .mem_gib(4)
            .margin(3 * GIB)
            .critical_path(),
        false,
    );
    let build = claim(40, 3, Need::new("test build").mem_gib(8), false);
    let small = claim(30, 4, Need::new("test build").mem_gib(2), false);
    let ledger = [first, second, lead, link, build, small.clone()];
    let procs = processes(&[
        (9, 2, 0),
        (8, 2, 0),
        (6, 2, 0),
        (7, 2, 0),
        (40, 2, 0),
        (30, 2, 0),
        (2, 0, 0),
    ]);
    let behind = swept_look(&small, &ledger, &procs, &reading);
    assert!(behind.is_err(), "{behind:?}");
    let trees = sweep(&ledger, &[]);
    assert_eq!(
        spared(&ledger, Some(&trees), Some(&procs), &reading)[2..5],
        [
            Some(Spared::Within(10 * GIB)),
            None,
            Some(Spared::Within(GIB))
        ]
    );

    let reading = measured();
    let unsure = claim(
        20,
        1,
        Need::new("slice compiler")
            .rank(Rank::Release)
            .mem_gib(2)
            .disk(GIB, "/w")
            .leads(),
        false,
    );
    let link = claim(
        21,
        2,
        Need::new("final link")
            .rank(Rank::Release)
            .mem_gib(4)
            .margin(3 * GIB)
            .critical_path(),
        false,
    );
    let small = claim(500, 4, Need::new("test build").mem_gib(3), false);
    let ledger = [
        claim(10, 0, Need::new("held run").mem_gib(1), true),
        claim(100, 0, Need::new("held build").mem_gib(10), true),
        unsure,
        link,
        claim(11, 3, Need::new("test build").mem_gib(26), false),
        small.clone(),
    ];
    let procs = processes(&[
        (10, 2, 0),
        (11, 10, 0),
        (100, 2, 0),
        (20, 2, 0),
        (21, 2, 0),
        (500, 2, 0),
        (2, 0, 0),
    ]);
    let behind = swept_look(&small, &ledger, &procs, &reading);
    assert!(behind.is_err(), "{behind:?}");
    let trees = sweep(&ledger, &[]);
    assert_eq!(
        spared(&ledger, Some(&trees), Some(&procs), &reading)[2..5],
        [None, None, Some(Spared::Within(0))]
    );

    let link = claim(
        300,
        0,
        Need::new("final link")
            .rank(Rank::Release)
            .mem_gib(5)
            .margin(3 * GIB)
            .disk(GIB, "/w")
            .critical_path(),
        false,
    );
    let small = claim(500, 4, Need::new("test build").mem_gib(2), false);
    let ledger = [
        claim(100, 0, Need::new("held build").mem_gib(10), true),
        link,
        claim(200, 1, Need::new("test build").mem_gib(4), false),
        claim(
            201,
            2,
            Need::new("test binary").mem_gib(33).margin(GIB),
            false,
        ),
        claim(400, 3, Need::new("test build").mem_gib(14), false),
        small.clone(),
    ];
    let procs = processes(&[
        (100, 2, 0),
        (300, 2, 0),
        (200, 2, 0),
        (201, 200, 30 * GIB),
        (400, 2, 0),
        (500, 2, 0),
        (2, 0, 0),
    ]);
    let behind = swept_look(&small, &ledger, &procs, &reading);
    assert!(behind.is_err(), "{behind:?}");
    let trees = sweep(&ledger, &[(201, 30 * GIB)]);
    assert_eq!(
        spared(&ledger, Some(&trees), Some(&procs), &reading)[1..5],
        [None, None, Some(Spared::Stuck), Some(Spared::Within(0))]
    );

    let small = claim(500, 4, Need::new("test build").mem_gib(3), false);
    let ledger = [
        claim(10, 0, Need::new("held run").mem_gib(1), true),
        claim(100, 0, Need::new("held build").mem_gib(10), true),
        claim(
            10,
            1,
            Need::new("slice compiler")
                .rank(Rank::Release)
                .mem_gib(2)
                .disk(GIB, "/w")
                .leads(),
            false,
        ),
        claim(
            21,
            2,
            Need::new("final link")
                .rank(Rank::Release)
                .mem_gib(4)
                .margin(3 * GIB)
                .critical_path(),
            false,
        ),
        claim(11, 3, Need::new("test build").mem_gib(26), false),
        small.clone(),
    ];
    let procs = processes(&[
        (10, 2, 0),
        (11, 10, 0),
        (100, 2, 0),
        (21, 2, 0),
        (500, 2, 0),
        (2, 0, 0),
    ]);
    let behind = swept_look(&small, &ledger, &procs, &reading);
    assert!(behind.is_err(), "{behind:?}");
    let trees = sweep(&ledger, &[]);
    assert_eq!(
        spared(&ledger, Some(&trees), Some(&procs), &reading)[2..5],
        [None, None, Some(Spared::Within(2 * GIB))]
    );

    let on = |cores: u32, mut record: Record| {
        record.held = Some(Held {
            admitted_ms: 0,
            cores,
            alone: false,
        });
        record
    };
    let reading = Reading::of_gib(128, 30, 18);
    let small = claim(500, 4, Need::new("test build").mem_gib(5), false);
    let ledger = [
        on(
            6,
            claim(10, 0, Need::new("held run").mem_gib(1).cores(6), true),
        ),
        on(
            4,
            claim(100, 0, Need::new("held build").mem_gib(20).cores(4), true),
        ),
        claim(
            20,
            1,
            Need::new("slice compiler")
                .rank(Rank::Release)
                .mem_gib(12)
                .cores(1)
                .leads(),
            false,
        ),
        claim(30, 2, Need::new("test binary").mem_gib(3).cores(12), false),
        claim(11, 3, Need::new("test build").mem_gib(12), false),
        small.clone(),
        claim(30, 5, Need::new("test binary").mem_gib(40), false),
    ];
    let procs = processes(&[
        (10, 2, 0),
        (11, 10, 0),
        (100, 2, 0),
        (20, 2, 0),
        (30, 2, 0),
        (500, 2, 0),
        (2, 0, 0),
    ]);
    let behind = swept_look(&small, &ledger, &procs, &reading);
    assert!(behind.is_err(), "{behind:?}");
    let trees = sweep(&ledger, &[]);
    assert_eq!(
        spared(&ledger, Some(&trees), Some(&procs), &reading)[2..5],
        [
            Some(Spared::Within(17 * GIB)),
            None,
            Some(Spared::Within(3 * GIB))
        ]
    );
    // The share each 3 GiB is granted at the machine's load: none of these looks reckons it.
    let granted = |ledger: &[Record], procs: &Procs, load5: f64| {
        let reading = Reading {
            load5: Some(load5),
            ..reading.clone()
        };
        swept_look(&ledger[3], ledger, procs, &reading).map(|grant| grant.cores)
    };
    assert_eq!(granted(&ledger, &procs, 0.0).ok(), Some(4));

    // THRESHOLD, LOOKS and CORE: the parent on more cores, the held 20 GiB on none, and the lead
    // of the 3 GiB's rank asking after it, so that no share of the 3 GiB counts the lead.
    let threshold = |parent: u32| {
        [
            on(
                parent,
                claim(10, 0, Need::new("held run").mem_gib(1).cores(parent), true),
            ),
            on(0, claim(100, 0, Need::new("held build").mem_gib(20), true)),
            claim(
                20,
                2,
                Need::new("lab compiler").mem_gib(12).cores(1).leads(),
                false,
            ),
            claim(30, 1, Need::new("test binary").mem_gib(3).cores(12), false),
        ]
    };
    let procs = processes(&[
        (10, 2, 0),
        (11, 10, 0),
        (100, 2, 0),
        (20, 2, 0),
        (30, 2, 0),
        (500, 2, 0),
        (600, 2, 0),
        (700, 2, 0),
        (2, 0, 0),
    ]);
    let small = claim(500, 4, Need::new("test build").mem_gib(4), false);
    let mut ledger = threshold(16).to_vec();
    ledger.extend([
        claim(11, 3, Need::new("test build").mem_gib(12), false),
        small.clone(),
        claim(30, 5, Need::new("test binary").mem_gib(40), false),
    ]);
    let behind = swept_look(&small, &ledger, &procs, &reading);
    assert!(behind.is_err(), "{behind:?}");
    let trees = sweep(&ledger, &[]);
    assert_eq!(
        spared(&ledger, Some(&trees), Some(&procs), &reading)[2..5],
        [
            Some(Spared::Within(17 * GIB)),
            None,
            Some(Spared::Within(0))
        ]
    );
    assert_eq!(granted(&ledger, &procs, 0.0).ok(), Some(2));
    assert_eq!(granted(&ledger, &procs, 60.0).ok(), Some(1));

    let small = claim(700, 4, Need::new("test build").mem_gib(2), false);
    let mut ledger = threshold(16).to_vec();
    ledger.extend([
        claim(600, 3, Need::new("test build").mem_gib(12), false),
        small.clone(),
        claim(11, 5, Need::new("test build").mem_gib(12), false),
        claim(30, 6, Need::new("test binary").mem_gib(40), false),
    ]);
    let behind = swept_look(&small, &ledger, &procs, &reading);
    assert!(behind.is_ok(), "{behind:?}");
    let trees = sweep(&ledger, &[]);
    assert_eq!(
        spared(&ledger, Some(&trees), Some(&procs), &reading)[2..5],
        [
            Some(Spared::Within(17 * GIB)),
            None,
            Some(Spared::Within(2 * GIB))
        ]
    );

    let mut ledger = threshold(17).to_vec();
    ledger.extend([
        claim(11, 3, Need::new("test build").mem_gib(12), false),
        claim(30, 4, Need::new("test binary").mem_gib(40), false),
    ]);
    let procs = processes(&[
        (10, 2, 0),
        (11, 10, 0),
        (100, 2, 0),
        (20, 2, 0),
        (30, 10, 0),
        (2, 0, 0),
    ]);
    let trees = sweep(&ledger, &[]);
    assert_eq!(
        spared(&ledger, Some(&trees), Some(&procs), &reading)[2..5],
        [
            Some(Spared::Within(17 * GIB)),
            None,
            Some(Spared::Within(6 * GIB))
        ]
    );
    assert_eq!(granted(&ledger, &procs, 0.0).ok(), Some(12));
}

/// LEADS-FIRST: a lead is sure for a waiter inside held work only where nothing a parent holds can
/// keep it waiting ([`sure_leads`]): its memory fits beside the parents, it asks no disk, no quiet
/// machine and not for the machine alone, and where it asks cores the parents leave it one. CORES:
/// on 35 GiB and 18 cores, a parent holds 4 GiB and every core; five 2 GiB leads of a core each and
/// a 22 GiB lead wait, and a 1 GiB build under the parent asks after them. The five fit beside the
/// parent but wait for its cores, so none is sure and the build starts. Held to them, it waited for
/// the 22 GiB, which spared nothing beside the five starting ([`STARTERS_MAX`]), which waited for
/// the parent's cores, which waited for the build. MARGIN: under a parent holding 10 GiB and every
/// core, a critical 2 GiB link of a 5 GiB margin and a core leads a 22 GiB lead. Held to them, a
/// 2 GiB build waited for the 22 GiB, which leaves 1 GiB once the link holds, a look that never
/// comes; it starts. COHORT: a cohort-owned lead's own look counts its kin's claims too: under a
/// parent holding 4 GiB and every core, a cohort's 6 GiB lead of a core under the parent waits for
/// its cores, so it is not sure, and a 30 GiB build under the parent starts. WAITS: of leads beside
/// a parent holding no core, a 2 GiB one and one asking a core are counted, and one asking disk,
/// the quiet machine or the machine alone is not. BOUNDARY: a parent on 17 of 18 cores leaves a
/// lead of 2 cores one, and it is counted; so is a lead of a core beside another process's held
/// claim on every core, which is no parent, and a lead of no core beside a parent on every core.
/// NEGATIVE CONTROLS: M-LEAD-CORES (a lead's cores unread) holds the build in CORES and MARGIN;
/// M-LEAD-COHORT (a lead's kin left out of its fit, cohort-owned or not) holds it in COHORT;
/// M-LEAD-WAITS (a lead's disk, quiet and ALONE unread) counts each lead WAITS names;
/// M-LEAD-ALLCORES (every held claim's cores counted as a parent's) leaves out the lead beside
/// another process's claim; M-LEAD-SPARE (a lead's own cores counted beside the parents') leaves
/// out the lead of 2 cores; and M-LEAD-NOCORE (a lead of no core judged by the parents' cores)
/// leaves out the lead of no core.
#[test]
fn a_lead_is_sure_only_where_no_parent_holds_what_it_waits_for() {
    let held_on = |cores: u32, mut record: Record| {
        record.held = Some(Held {
            admitted_ms: 0,
            cores,
            alone: false,
        });
        record
    };
    let every_core = |record: Record| held_on(18, record);
    let reading = measured();
    let mut ledger = vec![every_core(claim(
        3,
        0,
        Need::new("held release").mem_gib(4),
        true,
    ))];
    for k in 0..5 {
        ledger.push(claim(
            41 + k,
            u64::from(1 + k),
            Need::new("slice compiler")
                .rank(Rank::Release)
                .mem_gib(2)
                .cores(1)
                .leads(),
            false,
        ));
    }
    ledger.push(claim(
        50,
        6,
        Need::new("slice compiler")
            .rank(Rank::Release)
            .mem_gib(22)
            .leads(),
        false,
    ));
    let build = claim(1, 7, Need::new("test build").mem_gib(1), false);
    ledger.push(build.clone());
    let procs = processes(&[
        (3, 2, 0),
        (1, 3, 0),
        (41, 2, 0),
        (42, 2, 0),
        (43, 2, 0),
        (44, 2, 0),
        (45, 2, 0),
        (50, 2, 0),
        (2, 0, 0),
    ]);
    let mine = swept_look(&build, &ledger, &procs, &reading);
    assert!(mine.is_ok(), "{mine:?}");

    let parent = every_core(claim(3, 0, Need::new("held release").mem_gib(10), true));
    let link = claim(
        41,
        1,
        Need::new("final link")
            .rank(Rank::Release)
            .mem_gib(2)
            .margin(5 * GIB)
            .cores(1)
            .critical_path(),
        false,
    );
    let lead = claim(
        50,
        2,
        Need::new("slice compiler")
            .rank(Rank::Release)
            .mem_gib(22)
            .leads(),
        false,
    );
    let build = claim(1, 3, Need::new("test build").mem_gib(2), false);
    let ledger = [parent, link, lead, build.clone()];
    let procs = processes(&[(3, 2, 0), (1, 3, 0), (41, 2, 0), (50, 2, 0), (2, 0, 0)]);
    let mine = swept_look(&build, &ledger, &procs, &reading);
    assert!(mine.is_ok(), "{mine:?}");

    let lead = kept_by(
        claim(
            41,
            1,
            Need::new("slice compiler")
                .rank(Rank::Release)
                .mem_gib(6)
                .cores(1)
                .leads(),
            false,
        ),
        crate::Accounting::CohortOwned,
    );
    let build = claim(1, 2, Need::new("test build").mem_gib(30), false);
    let ledger = [
        every_core(claim(3, 0, Need::new("held release").mem_gib(4), true)),
        lead,
        build.clone(),
    ];
    let procs = processes(&[(3, 2, 0), (1, 3, 0), (41, 3, 0), (2, 0, 0)]);
    let mine = swept_look(&build, &ledger, &procs, &reading);
    assert!(mine.is_ok(), "{mine:?}");

    let parent = claim(3, 0, Need::new("held release").mem_gib(4), true);
    let procs = processes(&[(3, 2, 0), (1, 3, 0), (41, 2, 0), (2, 0, 0)]);
    let lead = || {
        Need::new("slice compiler")
            .rank(Rank::Release)
            .mem_gib(2)
            .leads()
    };
    for (need, sure) in [
        (lead(), true),
        (lead().cores(1), true),
        (lead().disk(GIB, "/w"), false),
        (lead().quiet(1.0), false),
        (lead().alone(), false),
    ] {
        let theirs = claim(41, 1, need, false);
        let counted =
            crate::tests::counted_by(vec![&parent, &theirs], 1, Some(&procs), Some(35 * GIB));
        assert_eq!(
            counted.iter().map(|r| r.order()).collect::<Vec<_>>(),
            if sure {
                vec![theirs.order()]
            } else {
                Vec::new()
            },
            "{:?}",
            theirs.need
        );
    }

    let other = every_core(claim(60, 0, Need::new("held build").mem_gib(1), true));
    let procs = processes(&[(3, 2, 0), (1, 3, 0), (41, 2, 0), (60, 2, 0), (2, 0, 0)]);
    for (parent, others, need) in [
        (held_on(17, parent.clone()), Vec::new(), lead().cores(2)),
        (parent.clone(), vec![&other], lead().cores(1)),
        (every_core(parent.clone()), Vec::new(), lead()),
    ] {
        let theirs = claim(41, 1, need, false);
        let mut records = vec![&parent, &theirs];
        records.extend(others);
        let counted = crate::tests::counted_by(records, 1, Some(&procs), Some(35 * GIB));
        assert!(
            counted.iter().any(|r| r.order() == theirs.order()),
            "{:?} beside {:?}",
            theirs.need,
            parent.held
        );
    }
}

/// LEADS-FIRST: a lead is sure for an inside waiter only where its fit owes each parent what that
/// parent reserves in the lead's own look ([`sure_leads`], [`Reckoned::reserve`]). On 35 GiB a
/// process (3) holds 20 GiB, and a 4 GiB unit under it, whose own process uses 10 GiB, waits
/// behind two release leads: its grandparent's (6) of 25 GiB, which waits for that held claim's
/// end, and another process's (4) of 12 GiB, which waits behind it. Each tree rooted at every
/// claimant, the held claim reserves all 20 GiB: the first lead is not sure, so neither is the
/// second, and the unit, which counts neither, starts now. CREDIT: a held 20 GiB claim of 3's
/// credited with a VM (77) that uses 15 GiB reserves 5 GiB, so another process's 28 GiB lead
/// starts now in its own look, and a 10 GiB unit under 3 counts it as sure and waits behind it.
/// NEGATIVE CONTROLS: M-LEAD-ROOTS (the trees of a lead's fit rooted at the held claims alone)
/// gives the held claim the unit's 10 GiB, makes both leads sure, and holds the unit to the
/// second lead's 3 GiB, while that lead waits for the first and the first for the held claim,
/// which ends only once the unit has run; M-FIT-TREES (a parent owing what its holder's own tree
/// leaves of its claims) owes the credited claim all 20 GiB, makes the 28 GiB lead unsure and
/// starts the unit ahead of it, so that the lead waits for the unit's end.
#[test]
fn a_lead_is_sure_only_where_each_parent_owes_what_it_reserves_in_the_leads_own_look() {
    let lead = |pid: u32, asks: u64, gib: u64| {
        claim(
            pid,
            asks,
            Need::new("slice compiler")
                .rank(Rank::Release)
                .mem_gib(gib)
                .leads(),
            false,
        )
    };
    let parent = claim(3, 0, Need::new("held release").mem_gib(20), true);
    let unit = claim(1, 3, Need::new("compile unit").mem_gib(4), false);
    let ledger = [parent, lead(6, 1, 25), lead(4, 2, 12), unit.clone()];
    let procs = processes(&[(6, 2, 0), (3, 6, 0), (1, 3, 10 * GIB), (4, 2, 0), (2, 0, 0)]);
    let reading = measured();
    let mine = swept_look(&unit, &ledger, &procs, &reading);
    assert!(mine.is_ok(), "{mine:?}");
    let trees = sweep(&ledger, &[(1, 10 * GIB)]);
    assert_eq!(
        spared(&ledger, Some(&trees), Some(&procs), &reading),
        [
            None,
            Some(Spared::Within(10 * GIB)),
            Some(Spared::Within(3 * GIB)),
            None
        ]
    );

    let mut credited = claim(3, 0, Need::new("held vm boot").mem_gib(20), true);
    credited.credit = vec![77];
    let unit = claim(1, 2, Need::new("compile unit").mem_gib(10), false);
    let ledger = [credited, lead(6, 1, 28), unit.clone()];
    let procs = processes(&[
        (3, 2, 0),
        (77, 2, 15 * GIB),
        (6, 2, 0),
        (1, 3, 0),
        (2, 0, 0),
    ]);
    let theirs = swept_look(&ledger[1], &ledger, &procs, &reading);
    assert!(theirs.is_ok(), "{theirs:?}");
    let mine = swept_look(&unit, &ledger, &procs, &reading);
    assert!(waits_stuck(&mine, false), "{mine:?}");
}

/// A credited process is counted toward one claim in every look ([`Reckoned::credited`]): the
/// first that names it among every claim the look read. A VM (77) using 30 GiB is credited by
/// two held claims, a 30 GiB one of a process (3) and a 10 GiB one of another's (9), and on
/// 35 GiB a 40 GiB budget of a 1 GiB margin waits under 3. Every look credits the VM to 3's
/// claim, whose end the budget cannot wait for (its kin's): the other claim's end gives back its
/// 10 GiB, short of the 16 GiB the budget lacks, so the budget is STUCK in its own look as in
/// `spared`. HOLDER: its look names each claim at what it reserves in every look ([`Holder::mem`]):
/// a process (9) holds a 10 GiB claim that credits a 40 GiB unit's own process, its kin's, and a
/// 5 GiB one, its tree using 6 GiB; the first takes that tree's credit, so the second reserves
/// 5 GiB, as the look that leaves the first out names it. NEGATIVE CONTROLS: M-CREDIT-LOOK (a look
/// crediting a process toward the first claim it counts) credits the VM to 9's claim in the
/// budget's own look, whose end then brings it, and names it waiting where `spared` names it
/// stuck; M-HOLDER-MEM (a holder named at what it reserves among the claims its look counts)
/// names the 5 GiB claim reserving none.
#[test]
fn every_look_credits_a_process_to_one_claim_and_names_a_holder_at_its_reservation() {
    let crediting = |mut r: Record| {
        r.credit = vec![77];
        r
    };
    let ancestor = crediting(claim(3, 0, Need::new("held release").mem_gib(30), true));
    let other = crediting(claim(9, 1, Need::new("lab vm").mem_gib(10), true));
    let budget = claim(1, 2, Need::new("vm boot").mem_gib(40).margin(GIB), false);
    let ledger = [ancestor, other, budget.clone()];
    let procs = processes(&[
        (3, 2, 0),
        (1, 3, 0),
        (9, 2, 0),
        (77, 2, 30 * GIB),
        (2, 0, 0),
    ]);
    let reading = measured();
    let mine = swept_look(&budget, &ledger, &procs, &reading);
    assert!(waits_stuck(&mine, true), "{mine:?}");
    let read: Vec<&Record> = ledger.iter().collect();
    let trees = process_use(&read, budget.pid, Some(&procs)).trees;
    assert_eq!(
        spared(&ledger, Some(&trees), Some(&procs), &reading),
        [None, None, Some(Spared::Stuck)]
    );

    let mut kin = claim(9, 0, Need::new("held vm").mem_gib(10), true);
    kin.credit = vec![1];
    let plain = claim(9, 1, Need::new("held build").mem_gib(5), true);
    let asker = claim(1, 2, Need::new("compile unit").mem_gib(40), false);
    let ledger = [kin, plain.clone(), asker.clone()];
    let procs = processes(&[(9, 2, 6 * GIB), (1, 2, 0), (2, 0, 0)]);
    let short = swept_look(&asker, &ledger, &procs, &reading).expect_err("it waits");
    assert_eq!(
        short
            .holders
            .iter()
            .map(|h| (h.order, h.mem, h.claimed))
            .collect::<Vec<_>>(),
        [(plain.order(), 5 * GIB, 5 * GIB)]
    );
}

/// INSIDE (ATERM-KI-1113-342): a waiter whose kin hold a claim is held by its own look to the held
/// claims and its sure leads alone ([`counted`]), so every look stands it beside those alone
/// ([`own_counts`], [`memory_turn`]'s OWN LOOK). On 35 GiB a process (3) holds 20 GiB its tree
/// uses whole; a cohort-owned 40 GiB slice budget (7, a byte's margin), funded by that claim's end
/// with 15 GiB less a byte to spare, asked first; then a 42 GiB slice budget (1). Under 3 the
/// budget is STUCK in its own look, and so in every look: a 2 GiB and a 14 GiB claim behind it go
/// first and `spared` names it stuck. CONTROL: under 2 it counts the cohort budget, keeps its
/// place sparing 13 GiB less a byte and the 14 GiB claim waits. SURE LEAD: a 20 GiB lead its own
/// look counts starts in every look, and a 22 GiB budget under 3 stands funded by its end. ITS
/// LEADS AS THEIR OWN LOOKS STAND THEM (ATERM-KI-1113-343): a 34 GiB lead, whose own look takes
/// 3's 10 GiB reservation off its room and waits for another claim's end, spares 11 GiB in every
/// look, the own look of a 5 GiB unit under 3 among them, which leaves that reservation out: the
/// unit starts within it, and a 6 GiB claim behind them within what it leaves. NEGATIVE
/// CONTROLS: M-INSIDE-ALL (an inside waiter stood beside every waiter ahead none of its kin's nor
/// its process's) holds the 2 GiB claim back with the 42 GiB budget sparing nothing; M-KIN-IN
/// (its kin's claims counted where it stands) funds it, sparing 13 GiB less a byte, and holds
/// the 14 GiB claim; M-INSIDE-NONE (no waiter counted where it stands) starts the 22 GiB budget
/// in 35 GiB and holds the 2 GiB claim to what is left; M-RECKONED-OFF (each look standing a
/// waiter over its own claims alone) stands the lead in the unit's look without 3's
/// reservation, starting it, and holds the unit back.
#[test]
fn an_inside_waiter_is_reckoned_beside_the_claims_its_own_look_counts() {
    let reading = measured();
    let wants =
        |need: u64, margin: u64, reserved: u64, passes: Option<u64>, stuck: bool| Short::Memory {
            need,
            margin,
            available: 35 * GIB,
            reserved,
            passes,
            stuck_ahead: 0,
            stuck,
        };
    let parent = claim(3, 0, Need::new("held release").mem_gib(20), true);
    let cohort = kept_by(
        claim(7, 1, slice_budget(40), false),
        crate::Accounting::CohortOwned,
    );
    let budget = claim(1, 2, slice_budget(42), false);
    let small = claim(4, 3, Need::new("test build").mem_gib(2), false);
    let wide = claim(5, 3, Need::new("wide build").mem_gib(14), false);
    let ledger = [
        parent.clone(),
        cohort.clone(),
        budget.clone(),
        small.clone(),
    ];
    let widened = [parent.clone(), cohort.clone(), budget.clone(), wide.clone()];
    let trees = sweep(&ledger, &[(3, 20 * GIB)]);
    for (under, inside) in [(3, true), (2, false)] {
        let procs = processes(&[
            (3, 2, 20 * GIB),
            (1, under, 0),
            (7, 2, 0),
            (4, 2, 0),
            (5, 2, 0),
            (2, 0, 0),
        ]);
        let behind = swept_look(&small, &ledger, &procs, &reading);
        assert!(behind.is_ok(), "{inside}: {behind:?}");
        let theirs =
            swept_look(&cohort, &ledger, &procs, &reading).expect_err("the cohort budget waits");
        assert_eq!(
            theirs.shorts,
            [wants(40 * GIB, 1, 0, None, false)],
            "{inside}"
        );
        let mine =
            swept_look(&budget, &ledger, &procs, &reading).expect_err("the 42 GiB budget waits");
        let wider = swept_look(&wide, &widened, &procs, &reading);
        let listed = spared(&ledger, Some(&trees), Some(&procs), &reading);
        if inside {
            assert_eq!(mine.shorts, [wants(42 * GIB, 1, 0, None, true)]);
            assert!(wider.is_ok(), "{wider:?}");
            assert_eq!(
                listed,
                [
                    None,
                    Some(Spared::Within(15 * GIB - 1)),
                    Some(Spared::Stuck),
                    None
                ]
            );
        } else {
            assert_eq!(
                mine.shorts,
                [wants(42 * GIB, 1, 40 * GIB, Some(15 * GIB - 1), false)]
            );
            assert_eq!(
                wider.expect_err("the 14 GiB claim waits").shorts,
                [wants(14 * GIB, 0, 82 * GIB, Some(13 * GIB - 1), false)]
            );
            assert_eq!(
                listed,
                [
                    None,
                    Some(Spared::Within(15 * GIB - 1)),
                    Some(Spared::Within(13 * GIB - 1)),
                    None
                ]
            );
        }
    }

    let lead = claim(
        8,
        1,
        Need::new("slice compiler")
            .rank(Rank::Release)
            .mem_gib(20)
            .leads(),
        false,
    );
    let narrow = claim(1, 2, slice_budget(22), false);
    let ledger = [parent, lead.clone(), narrow.clone(), small.clone()];
    let procs = processes(&[(3, 2, 20 * GIB), (1, 3, 0), (8, 2, 0), (4, 2, 0), (2, 0, 0)]);
    assert!(swept_look(&lead, &ledger, &procs, &reading).is_ok());
    let mine = swept_look(&narrow, &ledger, &procs, &reading).expect_err("it waits");
    assert_eq!(mine.shorts, [wants(22 * GIB, 1, 20 * GIB, None, false)]);
    assert!(swept_look(&small, &ledger, &procs, &reading).is_ok());
    let trees = sweep(&ledger, &[(3, 20 * GIB)]);
    assert_eq!(
        spared(&ledger, Some(&trees), Some(&procs), &reading),
        [None, None, Some(Spared::Within(13 * GIB - 1)), None]
    );

    let kin = claim(3, 0, Need::new("held release").mem_gib(20), true);
    let held = claim(8, 0, Need::new("held compile").mem_gib(20), true);
    let last = claim(9, 0, Need::new("held test").mem_gib(4), true);
    let lead = claim(
        6,
        1,
        Need::new("slice compiler")
            .rank(Rank::Release)
            .mem_gib(34)
            .leads(),
        false,
    );
    let unit = claim(1, 2, Need::new("compile unit").mem_gib(5), false);
    let after = claim(4, 3, Need::new("test build").mem_gib(6), false);
    let ledger = [kin, held, last, lead.clone(), unit.clone(), after.clone()];
    let procs = processes(&[
        (3, 2, 10 * GIB),
        (8, 2, 20 * GIB),
        (9, 2, 4 * GIB),
        (6, 2, 0),
        (1, 3, 0),
        (4, 2, 0),
        (2, 0, 0),
    ]);
    let theirs = swept_look(&lead, &ledger, &procs, &reading).expect_err("the lead waits");
    assert_eq!(theirs.shorts, [wants(34 * GIB, 0, 10 * GIB, None, false)]);
    let mine = swept_look(&unit, &ledger, &procs, &reading);
    assert!(mine.is_ok(), "{mine:?}");
    let behind = swept_look(&after, &ledger, &procs, &reading);
    assert!(behind.is_ok(), "{behind:?}");
    let trees = sweep(&ledger, &[(3, 10 * GIB), (8, 20 * GIB), (9, 4 * GIB)]);
    assert_eq!(
        spared(&ledger, Some(&trees), Some(&procs), &reading),
        [None, None, None, Some(Spared::Within(11 * GIB)), None, None]
    );
}
