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

/// THE CADENCE: a proven claimant in front is asked for at once, then
/// once per [`RECHECK`] however often the sweep looks; once no target
/// stands (the program in front moved, the claim gone) its tap is
/// stopped once, and a target again is asked for at once.
#[test]
fn a_claimant_is_asked_once_a_recheck_and_stopped_once_gone() {
    const THREAD: &str = "019a0000-0000-7000-8000-00000000000a";
    let mut asked = HashMap::new();
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    assert_eq!(due(&mut asked, 7, None, None, (at(0), None)), Ask::Nothing);
    assert_eq!(
        due(&mut asked, 7, Some(target(THREAD)), None, (at(0), None)),
        Ask::Read(target(THREAD))
    );
    assert_eq!(asked.get(&7).map(|a| a.owed), Some(false));
    for ms in [1, 250, 999] {
        assert_eq!(
            due(
                &mut asked,
                7,
                Some(target(THREAD)),
                None,
                (at(ms), Some(at(ms)))
            ),
            Ask::Nothing,
            "{ms}"
        );
        assert_eq!(asked.get(&7).map(|a| a.owed), Some(true), "{ms}");
    }
    assert_eq!(
        due(&mut asked, 8, Some(target(THREAD)), None, (at(500), None)),
        Ask::Read(target(THREAD))
    );
    assert_eq!(
        due(&mut asked, 7, Some(target(THREAD)), None, (at(1_000), None)),
        Ask::Read(target(THREAD))
    );
    assert_eq!(asked.get(&7).map(|a| a.owed), Some(false));
    assert_eq!(
        due(&mut asked, 7, Some(target(THREAD)), None, (at(1_400), None)),
        Ask::Nothing
    );
    assert_eq!(due(&mut asked, 7, None, None, (at(1_500), None)), Ask::Stop);
    assert_eq!(
        due(&mut asked, 7, None, None, (at(1_600), None)),
        Ask::Nothing
    );
    assert_eq!(
        due(&mut asked, 7, Some(target(THREAD)), None, (at(1_700), None)),
        Ask::Read(target(THREAD)),
        "back in front: asked at once"
    );
    assert_eq!(due(&mut asked, 8, None, None, (at(1_800), None)), Ask::Stop);
}

/// AT REST, NO TRAILING READ: a claimant whose last output was older than [`RECHECK`] at its
/// read, and wrote nothing since, swept again inside [`RECHECK`] by wakes not its own, owes no
/// read, so the sweep's `next_wake` arms no timer for it (`lab codex-alab-proof
/// --window-only`'s END: `timer_wakes=0`). CONTROL: output since the read owes one.
#[test]
fn a_claimant_at_rest_owes_no_trailing_read() {
    const THREAD: &str = "019a0000-0000-7000-8000-00000000000b";
    let mut asked = HashMap::new();
    let t0 = Instant::now() + Duration::from_secs(10);
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let wrote = Some(t0 - Duration::from_secs(5));
    assert_eq!(
        due(&mut asked, 7, Some(target(THREAD)), None, (at(0), wrote)),
        Ask::Read(target(THREAD))
    );
    for ms in [300, 600, 900] {
        assert_eq!(
            due(&mut asked, 7, Some(target(THREAD)), None, (at(ms), wrote)),
            Ask::Nothing
        );
        assert_eq!(
            asked.get(&7).map(|a| a.owed),
            Some(false),
            "{ms}: nothing moved"
        );
    }
    assert_eq!(
        due(
            &mut asked,
            7,
            Some(target(THREAD)),
            None,
            (at(950), Some(at(940)))
        ),
        Ask::Nothing
    );
    assert_eq!(
        asked.get(&7).map(|a| a.owed),
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
    let mut asked = HashMap::new();
    let t0 = Instant::now() + Duration::from_secs(10);
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let wrote = Some(at(0));
    assert_eq!(
        due(&mut asked, 7, Some(target(THREAD)), None, (at(250), wrote)),
        Ask::Read(target(THREAD))
    );
    assert_eq!(
        due(&mut asked, 7, Some(target(THREAD)), None, (at(500), wrote)),
        Ask::Nothing
    );
    assert_eq!(
        asked.get(&7).map(|a| a.owed),
        Some(true),
        "a read 250 ms after the burst owes a trailing read"
    );
    assert_eq!(
        due(
            &mut asked,
            7,
            Some(target(THREAD)),
            None,
            (at(1_250), wrote)
        ),
        Ask::Read(target(THREAD)),
        "the trailing read"
    );
    assert_eq!(
        due(
            &mut asked,
            7,
            Some(target(THREAD)),
            None,
            (at(1_500), wrote)
        ),
        Ask::Nothing
    );
    assert_eq!(
        asked.get(&7).map(|a| a.owed),
        Some(false),
        "the trailing read, 1.25 s after the burst, owes none"
    );
    let mut rest = HashMap::new();
    let old = Some(t0 - Duration::from_secs(5));
    let _ = due(&mut rest, 7, Some(target(THREAD)), None, (at(250), old));
    let _ = due(&mut rest, 7, Some(target(THREAD)), None, (at(500), old));
    assert_eq!(
        rest.get(&7).map(|a| a.owed),
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
    let mut asked = HashMap::new();
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let ask = |asked: &HashMap<u64, RolloutAsk>| *asked.get(&7).expect("asked");

    assert_eq!(
        due(
            &mut asked,
            7,
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
            7,
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
            7,
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
            7,
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
            7,
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
            7,
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
            7,
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
    let mut late = HashMap::new();
    assert_eq!(
        due(
            &mut late,
            9,
            Some(target(THREAD)),
            look(1, true),
            (t0, Some(t0))
        ),
        Ask::Read(target(THREAD))
    );
    assert_eq!(
        due(
            &mut late,
            9,
            Some(target(THREAD)),
            look(2, true),
            (at(100), Some(at(100)))
        ),
        Ask::Nothing
    );
    assert_eq!(
        due(
            &mut late,
            9,
            Some(target(THREAD)),
            look(2, true),
            (at(1_500), Some(at(1_500)))
        ),
        Ask::Read(target(THREAD)),
        "the trailing read, late"
    );
    let kept = *late.get(&9).expect("asked");
    assert_eq!(kept.settle, Some(at(2_100)), "the settle stands past it");
    let mut refused = late.clone();
    assert_eq!(
        due(
            &mut refused,
            9,
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
            9,
            Some(target(THREAD)),
            look(2, true),
            (at(1_500) + RECHECK, Some(at(1_500) + RECHECK))
        ),
        Ask::Read(target(THREAD))
    );
    assert_eq!(
        late.get(&9).expect("asked").settle,
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
    let mut asked = HashMap::new();
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let ask = |asked: &HashMap<u64, RolloutAsk>| *asked.get(&7).expect("asked");
    let wait = |read_ms: u64| ReadWait {
        since: 9_000,
        read_ms,
    };
    let read = |asked: &mut HashMap<u64, RolloutAsk>, ms: u64| {
        due(asked, 7, Some(target(THREAD)), None, (at(ms), None))
    };

    pair_wait(&mut asked, 7, Some((wait(0), at(3_000))));
    assert!(asked.is_empty(), "no ask, nothing taken");
    assert_eq!(read(&mut asked, 0), Ask::Read(target(THREAD)));
    assert_eq!(
        ask(&asked).wake(),
        None,
        "premise: a read at rest owes nothing"
    );
    pair_wait(&mut asked, 7, Some((wait(0), at(3_000))));
    assert_eq!(
        ask(&asked).wake(),
        Some(at(3_000)),
        "a new wait owes its read"
    );
    assert_eq!(read(&mut asked, 1_500), Ask::Read(target(THREAD)));
    assert_eq!(
        ask(&asked).wake(),
        Some(at(3_000)),
        "a read before it keeps it"
    );
    pair_wait(&mut asked, 7, Some((wait(0), at(3_000))));
    assert_eq!(
        ask(&asked).wake(),
        Some(at(3_000)),
        "the same wait again keeps it"
    );
    assert_eq!(read(&mut asked, 2_000), Ask::Nothing, "inside the recheck");
    assert_eq!(
        ask(&asked).wake(),
        Some(at(3_000)),
        "a refused sweep keeps it"
    );
    assert_eq!(read(&mut asked, 3_000), Ask::Read(target(THREAD)));
    assert_eq!(ask(&asked).pair, None, "the read at it discharges it");
    assert_eq!(ask(&asked).wake(), None);
    pair_wait(&mut asked, 7, Some((wait(0), at(3_000))));
    assert_eq!(
        ask(&asked).wake(),
        None,
        "a failed read leaves the wait as it was: nothing more is owed"
    );
    assert_eq!(read(&mut asked, 4_000), Ask::Read(target(THREAD)));
    pair_wait(&mut asked, 7, Some((wait(0), at(4_000))));
    assert_eq!(ask(&asked).wake(), None, "nor at the next read's");
    pair_wait(&mut asked, 7, Some((wait(1_900), at(4_000))));
    assert_eq!(
        ask(&asked).wake(),
        Some(at(4_000) + RECHECK),
        "a read to the end that left it waiting owes one more, after the recheck"
    );
    pair_wait(&mut asked, 7, None);
    assert_eq!((ask(&asked).wait, ask(&asked).pair), (None, None));
    assert_eq!(ask(&asked).wake(), None, "no wait clears it");
}
