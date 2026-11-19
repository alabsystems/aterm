// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn target(thread: &str) -> TapTarget {
    crate::self_claim::RolloutTarget {
        revision: 1,
        pid: 4242,
        start: "Mon Oct 20 16:37:58 2026".to_string(),
        thread: thread.to_string(),
        home: std::path::PathBuf::from("/private/tmp/x/codex-home"),
    }
    .into()
}

/// THE CADENCE: a proven claimant in front is asked for at once, then,
/// while it writes, at most once per [`RECHECK`] however often the sweep
/// looks; once no target stands (the program in front moved, the claim
/// gone) its tap is stopped once, and a target again is asked for at once.
#[test]
fn a_claimant_is_asked_once_a_recheck_and_stopped_once_gone() {
    const THREAD: &str = "019a0000-0000-7000-8000-00000000000a";
    let mut asked = None;
    let mut other = None;
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    assert_eq!(due(&mut asked, None, None, (at(0), None)), Ask::Nothing);
    assert_eq!(
        due(&mut asked, Some(target(THREAD)), None, (at(0), None)),
        Ask::Read(target(THREAD))
    );
    assert_eq!(asked.as_deref().map(|a| a.owed), Some(false));
    for ms in [1, 250, 999] {
        assert_eq!(
            due(
                &mut asked,
                Some(target(THREAD)),
                None,
                (at(ms), Some(at(ms)))
            ),
            Ask::Nothing,
            "{ms}"
        );
        assert_eq!(asked.as_deref().map(|a| a.owed), Some(true), "{ms}");
    }
    assert_eq!(
        due(&mut other, Some(target(THREAD)), None, (at(500), None)),
        Ask::Read(target(THREAD))
    );
    assert_eq!(
        due(&mut asked, Some(target(THREAD)), None, (at(1_000), None)),
        Ask::Read(target(THREAD))
    );
    assert_eq!(asked.as_deref().map(|a| a.owed), Some(false));
    assert_eq!(
        due(&mut asked, Some(target(THREAD)), None, (at(1_400), None)),
        Ask::Nothing
    );
    assert_eq!(due(&mut asked, None, None, (at(1_500), None)), Ask::Stop);
    assert_eq!(due(&mut asked, None, None, (at(1_600), None)), Ask::Nothing);
    assert_eq!(
        due(&mut asked, Some(target(THREAD)), None, (at(1_700), None)),
        Ask::Read(target(THREAD)),
        "back in front: asked at once"
    );
    assert_eq!(due(&mut other, None, None, (at(1_800), None)), Ask::Stop);
}

/// AT REST, NO TRAILING READ: a claimant whose last output was older than [`RECHECK`] at its
/// read, and wrote nothing since, swept again inside [`RECHECK`] by wakes not its own, owes no
/// read, so the sweep's `next_wake` arms no timer for it (`lab codex-alab-proof
/// --window-only`'s END: `timer_wakes=0`). CONTROL: output since the read owes one.
#[test]
fn a_claimant_at_rest_owes_no_trailing_read() {
    const THREAD: &str = "019a0000-0000-7000-8000-00000000000b";
    let mut asked = None;
    let t0 = Instant::now() + Duration::from_secs(10);
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let wrote = Some(t0 - Duration::from_secs(5));
    assert_eq!(
        due(&mut asked, Some(target(THREAD)), None, (at(0), wrote)),
        Ask::Read(target(THREAD))
    );
    for ms in [300, 600, 900] {
        assert_eq!(
            due(&mut asked, Some(target(THREAD)), None, (at(ms), wrote)),
            Ask::Nothing
        );
        assert_eq!(
            asked.as_deref().map(|a| a.owed),
            Some(false),
            "{ms}: nothing moved"
        );
    }
    assert_eq!(
        due(
            &mut asked,
            Some(target(THREAD)),
            None,
            (at(950), Some(at(940)))
        ),
        Ask::Nothing
    );
    assert_eq!(
        asked.as_deref().map(|a| a.owed),
        Some(true),
        "CONTROL: output since the read"
    );
}

/// JUST AFTER A BURST, ONE TRAILING READ: codex-alab's rollout writer can put a record in the
/// file after the frame that ended its burst, so a read sent within [`RECHECK`] of the last
/// output owes one trailing read though nothing was written since; that trailing read, past
/// [`RECHECK`] of the output, owes none. CONTROL: the same sweeps after a read sent at rest owe
/// nothing ([`a_claimant_at_rest_owes_no_trailing_read`]).
#[test]
fn a_read_just_after_a_burst_owes_one_trailing_read() {
    const THREAD: &str = "019a0000-0000-7000-8000-00000000000c";
    let mut asked = None;
    let t0 = Instant::now() + Duration::from_secs(10);
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let wrote = Some(at(0));
    assert_eq!(
        due(&mut asked, Some(target(THREAD)), None, (at(250), wrote)),
        Ask::Read(target(THREAD))
    );
    assert_eq!(
        due(&mut asked, Some(target(THREAD)), None, (at(500), wrote)),
        Ask::Nothing
    );
    assert_eq!(
        asked.as_deref().map(|a| a.owed),
        Some(true),
        "a read 250 ms after the burst owes a trailing read"
    );
    assert_eq!(
        due(&mut asked, Some(target(THREAD)), None, (at(1_250), wrote)),
        Ask::Read(target(THREAD)),
        "the trailing read"
    );
    assert_eq!(
        due(&mut asked, Some(target(THREAD)), None, (at(1_500), wrote)),
        Ask::Nothing
    );
    assert_eq!(
        asked.as_deref().map(|a| a.owed),
        Some(false),
        "the trailing read, 1.25 s after the burst, owes none"
    );
    let mut rest = None;
    let old = Some(t0 - Duration::from_secs(5));
    let _ = due(&mut rest, Some(target(THREAD)), None, (at(250), old));
    let _ = due(&mut rest, Some(target(THREAD)), None, (at(500), old));
    assert_eq!(
        rest.as_deref().map(|a| a.owed),
        Some(false),
        "CONTROL: a read sent at rest"
    );
}

/// THE SETTLE: while the record names a count, each sweep that sees the screen move owes one
/// more read [`SETTLE`] after it, re-armed by each later move and kept across a read
/// dispatched before it (that read may precede codex-alab's coalesced write); the first read at
/// or after it discharges it, and nothing more is owed while the screen rests. An uncounted
/// look, or none, arms nothing. The wake never falls inside the [`RECHECK`] after a read,
/// where `due` would refuse it.
#[test]
fn a_settle_read_follows_the_screens_last_move_while_the_record_counts() {
    const THREAD: &str = "019a0000-0000-7000-8000-00000000000b";
    let g = |seq: u64| crate::control::ScreenGen { epoch: 1, seq };
    let look = |seq: u64, counted: bool| {
        Some(Look {
            screen: g(seq),
            counted,
        })
    };
    let mut asked = None;
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let ask = |asked: &Option<Box<RolloutAsk>>| *asked.as_deref().expect("asked");

    assert_eq!(
        due(
            &mut asked,
            Some(target(THREAD)),
            look(1, true),
            (t0, Some(t0))
        ),
        Ask::Read(target(THREAD))
    );
    assert_eq!(
        ask(&asked).settle,
        Some(t0 + SETTLE),
        "the first look arms it"
    );
    assert_eq!(
        due(
            &mut asked,
            Some(target(THREAD)),
            look(1, true),
            (at(500), Some(at(500)))
        ),
        Ask::Nothing
    );
    assert!(ask(&asked).owed);
    assert_eq!(
        ask(&asked).settle,
        Some(t0 + SETTLE),
        "a still screen keeps it"
    );
    assert_eq!(
        due(
            &mut asked,
            Some(target(THREAD)),
            look(2, true),
            (at(800), Some(at(800)))
        ),
        Ask::Nothing
    );
    assert_eq!(
        ask(&asked).settle,
        Some(at(800) + SETTLE),
        "a move re-arms it"
    );
    assert_eq!(
        ask(&asked).wake(),
        Some(t0 + RECHECK),
        "the trailing read first"
    );
    assert_eq!(
        due(
            &mut asked,
            Some(target(THREAD)),
            look(2, true),
            (t0 + RECHECK, Some(t0 + RECHECK))
        ),
        Ask::Read(target(THREAD))
    );
    assert_eq!(
        ask(&asked).settle,
        Some(at(800) + SETTLE),
        "a read before the settle keeps it"
    );
    assert_eq!(ask(&asked).wake(), Some(at(800) + SETTLE));
    assert_eq!(
        due(
            &mut asked,
            Some(target(THREAD)),
            look(2, true),
            (at(800) + SETTLE, Some(at(800) + SETTLE))
        ),
        Ask::Read(target(THREAD))
    );
    assert_eq!(ask(&asked).settle, None, "the settle read discharges it");
    assert_eq!(
        ask(&asked).wake(),
        None,
        "nothing more while the screen rests"
    );
    assert_eq!(
        due(
            &mut asked,
            Some(target(THREAD)),
            look(3, false),
            (at(6_000), Some(at(6_000)))
        ),
        Ask::Read(target(THREAD))
    );
    assert_eq!(
        ask(&asked).settle,
        None,
        "a record naming no count arms nothing"
    );
    assert_eq!(
        due(
            &mut asked,
            Some(target(THREAD)),
            None,
            (at(8_000), Some(at(8_000)))
        ),
        Ask::Read(target(THREAD))
    );
    assert_eq!(ask(&asked).settle, None, "no look arms nothing");
    assert_eq!(ask(&asked).screen, None);

    let base = RolloutAsk {
        at: t0,
        owed: false,
        output: None,
        screen: None,
        read: None,
        settle: None,
        wait: None,
        pair: None,
    };
    assert_eq!(base.wake(), None, "neither");
    assert_eq!(
        RolloutAsk { owed: true, ..base }.wake(),
        Some(t0 + RECHECK),
        "owed only"
    );
    assert_eq!(
        RolloutAsk {
            settle: Some(at(1_500)),
            ..base
        }
        .wake(),
        Some(at(1_500)),
        "settle only"
    );
    assert_eq!(
        RolloutAsk {
            settle: Some(at(300)),
            ..base
        }
        .wake(),
        Some(t0 + RECHECK),
        "settle only, inside the recheck: due reads no sooner"
    );
    assert_eq!(
        RolloutAsk {
            owed: true,
            settle: Some(at(300)),
            ..base
        }
        .wake(),
        Some(t0 + RECHECK),
        "both, the settle inside the recheck"
    );
    assert_eq!(
        RolloutAsk {
            owed: true,
            settle: Some(at(3_000)),
            ..base
        }
        .wake(),
        Some(t0 + RECHECK),
        "both: the trailing read is earlier"
    );

    // A late trailing read keeps a settle that lies within RECHECK of it: the wake names the
    // first instant due reads, not the settle, which due would refuse.
    let mut late = None;
    assert_eq!(
        due(
            &mut late,
            Some(target(THREAD)),
            look(1, true),
            (t0, Some(t0))
        ),
        Ask::Read(target(THREAD))
    );
    assert_eq!(
        due(
            &mut late,
            Some(target(THREAD)),
            look(2, true),
            (at(100), Some(at(100)))
        ),
        Ask::Nothing
    );
    assert_eq!(
        due(
            &mut late,
            Some(target(THREAD)),
            look(2, true),
            (at(1_500), Some(at(1_500)))
        ),
        Ask::Read(target(THREAD)),
        "the trailing read, late"
    );
    let kept = *late.as_deref().expect("asked");
    assert_eq!(kept.settle, Some(at(2_100)), "the settle stands past it");
    let mut refused = late.clone();
    assert_eq!(
        due(
            &mut refused,
            Some(target(THREAD)),
            look(2, true),
            (at(2_100), Some(at(2_100)))
        ),
        Ask::Nothing,
        "premise: due refuses a read at the settle itself"
    );
    assert_eq!(
        kept.wake(),
        Some(at(1_500) + RECHECK),
        "the wake waits out the recheck"
    );
    assert_eq!(
        due(
            &mut late,
            Some(target(THREAD)),
            look(2, true),
            (at(1_500) + RECHECK, Some(at(1_500) + RECHECK))
        ),
        Ask::Read(target(THREAD))
    );
    assert_eq!(
        late.as_deref().expect("asked").settle,
        None,
        "that read discharges the settle"
    );
}

/// THE PAIR READ (gap 25, [`pair_wait`]): a harness read wait the log names owes one read at
/// or after the instant it gives, carried by [`RolloutAsk::wake`] and kept across a read
/// dispatched before it; the first read at or after it discharges it. The same wait handed
/// again (the read failed or stopped short of the file's end) owes nothing more, so the ask
/// arms no wake for it, however long the turn waits; a wait a read to the end moved (its
/// watermark rose, the read begun before the wait's time) owes one more read, no sooner than
/// the recheck. No wait clears it, and a session with no ask takes none.
#[test]
fn a_harness_read_wait_owes_one_read_at_or_after_it() {
    const THREAD: &str = "019a0000-0000-7000-8000-00000000000e";
    let mut asked = None;
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let ask = |asked: &Option<Box<RolloutAsk>>| *asked.as_deref().expect("asked");
    let wait = |read_ms: u64| ReadWait {
        since: 9_000,
        read_ms,
    };
    // A sweep at `ms` of a tab whose last output was at `wrote`: the tab at rest is read on the
    // slow clock alone, so the reads this law needs before and after the pair read are sweeps that
    // find the output moved, each more than [`RECHECK`] old so no trailing read is owed.
    let sweep = |asked: &mut Option<Box<RolloutAsk>>, ms: u64, wrote: Option<u64>| {
        due(asked, Some(target(THREAD)), None, (at(ms), wrote.map(at)))
    };
    let read = |asked: &mut Option<Box<RolloutAsk>>, ms: u64| sweep(asked, ms, None);

    pair_wait(&mut asked, Some((wait(0), at(3_000))));
    assert!(asked.is_none(), "no ask, nothing taken");
    assert_eq!(read(&mut asked, 0), Ask::Read(target(THREAD)));
    assert_eq!(
        ask(&asked).wake(),
        None,
        "premise: a read at rest owes nothing"
    );
    pair_wait(&mut asked, Some((wait(0), at(3_000))));
    assert_eq!(
        ask(&asked).wake(),
        Some(at(3_000)),
        "a new wait owes its read"
    );
    assert_eq!(
        sweep(&mut asked, 1_500, Some(400)),
        Ask::Read(target(THREAD)),
        "the output moved"
    );
    assert_eq!(
        ask(&asked).wake(),
        Some(at(3_000)),
        "a read before it keeps it"
    );
    pair_wait(&mut asked, Some((wait(0), at(3_000))));
    assert_eq!(
        ask(&asked).wake(),
        Some(at(3_000)),
        "the same wait again keeps it"
    );
    assert_eq!(
        sweep(&mut asked, 2_000, Some(400)),
        Ask::Nothing,
        "inside the recheck"
    );
    assert_eq!(
        ask(&asked).wake(),
        Some(at(3_000)),
        "a refused sweep keeps it"
    );
    assert_eq!(
        sweep(&mut asked, 2_600, Some(400)),
        Ask::Nothing,
        "past the recheck the tab rests until the pair read"
    );
    assert_eq!(ask(&asked).wake(), Some(at(3_000)), "and the rest keeps it");
    assert_eq!(
        sweep(&mut asked, 3_000, Some(400)),
        Ask::Read(target(THREAD))
    );
    assert_eq!(ask(&asked).pair, None, "the read at it discharges it");
    assert_eq!(ask(&asked).wake(), None);
    pair_wait(&mut asked, Some((wait(0), at(3_000))));
    assert_eq!(
        ask(&asked).wake(),
        None,
        "a failed read leaves the wait as it was: nothing more is owed"
    );
    assert_eq!(
        sweep(&mut asked, 4_000, Some(2_900)),
        Ask::Read(target(THREAD)),
        "the output moved again"
    );
    pair_wait(&mut asked, Some((wait(0), at(4_000))));
    assert_eq!(ask(&asked).wake(), None, "nor at the next read's");
    pair_wait(&mut asked, Some((wait(1_900), at(4_000))));
    assert_eq!(
        ask(&asked).wake(),
        Some(at(4_000) + RECHECK),
        "a read to the end that left it waiting owes one more, after the recheck"
    );
    pair_wait(&mut asked, None);
    assert_eq!((ask(&asked).wait, ask(&asked).pair), (None, None));
    assert_eq!(ask(&asked).wake(), None, "no wait clears it");
}

/// AT REST, THE SLOW CLOCK (S1): a tab whose output and screen stand since its last read, with no
/// trailing, settle or pair read owed, is read once per [`REST_NEAR`] while it wrote within
/// [`NEAR_FOR`], and once per [`REST_FAR`] after: swept each second for a minute, the tab that
/// wrote 5 s before the first sweep is read at 0, 15, 30 and 45 s, the one that wrote 11 minutes
/// before, and the one that never wrote, once. No rest read arms a wake. NEGATIVE CONTROL: with
/// the rest branch of [`due`] deleted every sweep reads, 60 a tab.
#[test]
fn a_tab_at_rest_is_read_on_the_slow_clock() {
    const THREAD: &str = "019a0000-0000-7000-8000-00000000000d";
    let g = crate::control::ScreenGen { epoch: 1, seq: 9 };
    let t0 = Instant::now() + Duration::from_secs(3_600);
    let reads = |wrote: Option<Instant>, look: Option<Look>| {
        let mut asked = None;
        let mut reads = Vec::new();
        for k in 0..60u64 {
            let now = t0 + Duration::from_secs(k);
            match due(&mut asked, Some(target(THREAD)), look, (now, wrote)) {
                Ask::Read(to) => {
                    assert_eq!(to, target(THREAD));
                    reads.push(k);
                }
                Ask::Nothing => {}
                Ask::Stop => panic!("{k}: a standing target is never stopped"),
            }
            assert_eq!(
                asked.as_deref().and_then(RolloutAsk::wake),
                None,
                "{k}: a tab at rest arms no wake"
            );
        }
        reads
    };
    let uncounted = Some(Look {
        screen: g,
        counted: false,
    });
    let near = Some(t0 - Duration::from_secs(5));
    assert_eq!(reads(near, None), [0, 15, 30, 45]);
    assert_eq!(reads(near, uncounted), [0, 15, 30, 45]);
    let far = Some(t0 - Duration::from_secs(11 * 60));
    assert_eq!(reads(far, None), [0]);
    assert_eq!(reads(far, uncounted), [0]);
    assert_eq!(reads(None, uncounted), [0], "a tab that never wrote");
    // The ladder's edge: output 9 min 30 s old at the first sweep is near at the 15 s sweep and
    // ten minutes old at the 30 s one, which then waits [`REST_FAR`] from the read at 15 s.
    let edge = Some(t0 - Duration::from_secs(9 * 60 + 30));
    assert_eq!(reads(edge, None), [0, 15]);
    assert_eq!(
        rest_recheck(t0, Some(t0 - NEAR_FOR + Duration::from_millis(1))),
        REST_NEAR
    );
    assert_eq!(rest_recheck(t0, Some(t0 - NEAR_FOR)), REST_FAR);
    assert_eq!(rest_recheck(t0, None), REST_FAR);
}

/// THE TRAILING READ STANDS THOUGH NO SWEEP FELL INSIDE ITS SECOND (S1): a read sent 250 ms after
/// the tab's output owes one more, [`RECHECK`] later, and the first sweep past that reads though
/// none came between to note it owed; the sweeps after that trailing read, the tab at rest, read
/// nothing. NEGATIVE CONTROL: with the rest branch's "sent within `RECHECK` of the output" clause
/// deleted the 1300 ms sweep answers `Nothing`.
#[test]
fn a_read_just_after_output_is_followed_by_its_trailing_read_then_rests() {
    const THREAD: &str = "019a0000-0000-7000-8000-00000000000e";
    let mut asked = None;
    let t0 = Instant::now() + Duration::from_secs(3_600);
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let wrote = Some(t0);
    let mut said = Vec::new();
    for ms in [250, 1_300, 2_300, 3_300] {
        let ask = due(&mut asked, Some(target(THREAD)), None, (at(ms), wrote));
        said.push((ms, ask == Ask::Read(target(THREAD))));
        assert_ne!(ask, Ask::Stop, "{ms}");
    }
    assert_eq!(
        said,
        [(250, true), (1_300, true), (2_300, false), (3_300, false)]
    );
    assert_eq!(
        asked.as_deref().map(|a| a.at),
        Some(at(1_300)),
        "the rest is timed from the trailing read"
    );
}

/// MOVEMENT ENDS THE REST (S1): a resting tab whose output moved, or whose screen moved, is read
/// at the next sweep, and a settle or a pair read that came due is read at the sweep that finds
/// it due; each such read starts the rest again. A screen that a sweep inside [`RECHECK`] was
/// first to see moved is read at the first sweep past the recheck: the rest is judged against
/// the screen the last read saw. CONTROL: the same sweeps with nothing moved read nothing.
/// NEGATIVE CONTROLS, one for each test the rest branch of [`due`] makes, each deleted alone:
/// without `output == prior.output` the sweep after the output moved answers `Nothing`; without
/// `screen == prior.read`, the sweep after the screen moved; without the settle test, the sweep
/// at the settle; without the pair test, the sweep at the pair read's instant. With the screen
/// compared to the last call's (`prior.screen`), the sweep past the recheck after a move seen
/// inside it answers `Nothing`.
#[test]
fn a_resting_tab_is_read_at_the_first_sweep_after_it_moved() {
    const THREAD: &str = "019a0000-0000-7000-8000-00000000000f";
    let g = |seq: u64| crate::control::ScreenGen { epoch: 1, seq };
    let look = |seq: u64, counted: bool| {
        Some(Look {
            screen: g(seq),
            counted,
        })
    };
    let t0 = Instant::now() + Duration::from_secs(3_600);
    let at = |s: u64| t0 + Duration::from_secs(s);
    let old = Some(t0 - Duration::from_secs(5));
    let rested = || {
        let mut asked = None;
        assert_eq!(
            due(&mut asked, Some(target(THREAD)), look(1, false), (t0, old)),
            Ask::Read(target(THREAD))
        );
        for s in 1..=19 {
            let read = due(
                &mut asked,
                Some(target(THREAD)),
                look(1, false),
                (at(s), old),
            ) == Ask::Read(target(THREAD));
            assert_eq!(read, s == 15, "{s}: the rest's own read alone");
        }
        asked
    };

    let mut still = rested();
    assert_eq!(
        due(
            &mut still,
            Some(target(THREAD)),
            look(1, false),
            (at(20), old)
        ),
        Ask::Nothing,
        "CONTROL: nothing moved"
    );

    let mut wrote = rested();
    assert_eq!(
        due(
            &mut wrote,
            Some(target(THREAD)),
            look(1, false),
            (at(20), Some(at(20)))
        ),
        Ask::Read(target(THREAD)),
        "the output moved"
    );

    let mut drew = rested();
    assert_eq!(
        due(
            &mut drew,
            Some(target(THREAD)),
            look(2, false),
            (at(20), old)
        ),
        Ask::Read(target(THREAD)),
        "the screen moved"
    );
    assert_eq!(
        due(
            &mut drew,
            Some(target(THREAD)),
            look(2, false),
            (at(21), old)
        ),
        Ask::Nothing,
        "and the rest starts again from that read"
    );

    // A screen that moves while the record names a count is read at the move and owes its settle
    // read, which the rest does not hold back.
    let mut counted = rested();
    assert_eq!(
        due(
            &mut counted,
            Some(target(THREAD)),
            look(2, true),
            (at(20), old)
        ),
        Ask::Read(target(THREAD))
    );
    assert_eq!(
        counted.as_deref().and_then(RolloutAsk::wake),
        Some(at(20) + SETTLE)
    );
    assert_eq!(
        due(
            &mut counted,
            Some(target(THREAD)),
            look(2, true),
            (at(21), old)
        ),
        Ask::Nothing,
        "a settle still to come is not read early"
    );
    assert_eq!(
        counted.as_deref().and_then(RolloutAsk::wake),
        Some(at(20) + SETTLE),
        "and the rest keeps its wake"
    );
    assert_eq!(
        due(
            &mut counted,
            Some(target(THREAD)),
            look(2, true),
            (at(20) + SETTLE, old)
        ),
        Ask::Read(target(THREAD)),
        "the settle read"
    );
    assert_eq!(counted.as_deref().and_then(RolloutAsk::wake), None);

    // A pair read the log names is read at the sweep that finds it due.
    let mut paired = rested();
    let wait = ReadWait {
        since: 9_000,
        read_ms: 0,
    };
    pair_wait(&mut paired, Some((wait, at(25))));
    assert_eq!(
        due(
            &mut paired,
            Some(target(THREAD)),
            look(1, false),
            (at(24), old)
        ),
        Ask::Nothing,
        "a pair read still to come"
    );
    assert_eq!(paired.as_deref().and_then(RolloutAsk::wake), Some(at(25)));
    assert_eq!(
        due(
            &mut paired,
            Some(target(THREAD)),
            look(1, false),
            (at(25), old)
        ),
        Ask::Read(target(THREAD)),
        "the pair read"
    );
    assert_eq!(paired.as_deref().and_then(RolloutAsk::wake), None);

    // A screen a sweep inside the recheck was first to see moved, on a tab whose record names no
    // count: no wake is armed for it, and the first sweep past the recheck reads.
    let mut early = None;
    let ms = |ms: u64| t0 + Duration::from_millis(ms);
    let mut sweep = |seq: u64, at: Instant| {
        let ask = due(
            &mut early,
            Some(target(THREAD)),
            look(seq, false),
            (at, old),
        );
        (
            ask == Ask::Read(target(THREAD)),
            early.as_deref().and_then(RolloutAsk::wake),
        )
    };
    assert_eq!(sweep(1, t0), (true, None));
    assert_eq!(sweep(2, ms(500)), (false, None), "inside the recheck");
    assert_eq!(
        sweep(2, ms(1_500)),
        (true, None),
        "the screen moved since the read"
    );
    assert_eq!(
        sweep(2, ms(2_500)),
        (false, None),
        "and the rest starts again from that read"
    );
}

/// The updater may replace the launch symlink between rollout reads. Both product and
/// client versions continue to describe the mapped file, even when both builds report
/// the same upstream client version.
#[cfg(unix)]
#[test]
fn codex_rollout_versions_follow_the_loaded_image_after_the_managed_alias_moves() {
    use aterm_sysprobe::kproc::ImageFile;
    use std::fs;
    use std::os::unix::fs::{MetadataExt as _, symlink};

    fn file_id(path: &Path) -> Option<ImageFile> {
        let meta = fs::metadata(path).ok()?;
        Some(ImageFile {
            device: meta.dev(),
            inode: meta.ino(),
        })
    }

    let scratch = aterm_tempfile::tempdir().unwrap();
    let layout = atpkg::Layout {
        prefix: scratch.path().to_owned(),
    };
    let install = |build, version| {
        let root = layout.build_dir("codex", build);
        fs::create_dir_all(root.join("bin")).unwrap();
        let path = root.join("bin/codex");
        fs::write(&path, version).unwrap();
        fs::write(
            root.join("codex-package.json"),
            format!(r#"{{"layoutVersion":1,"entrypoint":"bin/codex","version":"{version}"}}"#),
        )
        .unwrap();
        fs::write(
            root.join("alab-build.json"),
            format!(r#"{{"version":"{version}","client_version":"0.170.0"}}"#),
        )
        .unwrap();
        atpkg::store::mark_build_ready(&root).unwrap();
        path
    };
    let old = install(2026111601, "0.10.0");
    let new = install(2026111701, "0.11.0");
    let alias_dir = atpkg::activate::exec_dir(&layout, "codex");
    fs::create_dir_all(&alias_dir).unwrap();
    let launch = alias_dir.join("codex");
    symlink(&old, &launch).unwrap();
    let before = file_id(&launch).unwrap();
    let now = Instant::now();
    let mut lookup = PackageLookup::default();
    let observe =
        |lookup: &mut PackageLookup, id| lookup.read(id, &launch, |p| file_id(p) == Some(id), now);
    assert_eq!(observe(&mut lookup, before).unwrap().product, "0.10.0");

    let next = alias_dir.join("next");
    symlink(&new, &next).unwrap();
    fs::rename(next, &launch).unwrap();
    let held = observe(&mut lookup, before).unwrap();
    assert_eq!(
        (held.product.as_str(), held.client.as_str()),
        ("0.10.0", "0.170.0")
    );

    fs::rename(&old, scratch.path().join("retired")).unwrap();
    assert!(observe(&mut lookup, before).is_none());
    let fresh = observe(&mut lookup, file_id(&launch).unwrap()).unwrap();
    assert_eq!(
        (fresh.product.as_str(), fresh.client.as_str()),
        ("0.11.0", "0.170.0")
    );
}
