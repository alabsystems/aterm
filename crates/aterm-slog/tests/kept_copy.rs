// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Closed-session copy tests (`Log::kept_within`).

use aterm_slog::{Actor, Close, Conf, Cursor, GapWhy, Log, Op, Out, SessionOut, Src, Tier, encode};

use crate::support::{at, realistic_log};

fn closed_session(turns: u64) -> Log {
    let mut log = realistic_log(turns);
    log.close_session(
        Close::new(Out::Session(SessionOut::UiClose), Src::Aterm, Conf::Exact).by(Actor::Human),
        at(10_000_000),
    )
    .expect("the session closes");
    log
}

#[test]
fn a_kept_copy_reads_like_the_log_it_was_from_any_cursor() {
    let log = closed_session(300);
    let kept = log.kept_within(64 * 1024);
    assert!(
        kept.len() < log.len(),
        "PRECONDITION: the copy left some out"
    );
    assert_eq!(kept.ep(), log.ep());
    assert_eq!(kept.last_seq(), log.last_seq());
    assert_eq!(
        kept.lost() - log.lost(),
        (log.len() - kept.len()) as u64,
        "every record left out is counted"
    );
    let seqs: Vec<u64> = log.window().map(|r| r.seq).collect();
    for cursor_seq in [
        0,
        1,
        seqs[seqs.len() / 3],
        seqs[seqs.len() - 2],
        log.last_seq(),
    ] {
        let cursor = Cursor::new(log.ep(), cursor_seq);
        let live = log.read(cursor, usize::MAX);
        let copy = kept.read(cursor, usize::MAX);
        let live_seqs: Vec<u64> = live.recs.iter().map(|r| r.seq).collect();
        for r in &copy.recs {
            assert!(
                live_seqs.contains(&r.seq),
                "seq {} is one the log showed",
                r.seq
            );
            let shown = live.recs.iter().find(|l| l.seq == r.seq).expect("shown");
            assert_eq!(
                encode(r, Tier::Content).expect("fits"),
                encode(shown, Tier::Content).expect("fits"),
                "the same record"
            );
        }
        let live_lost = live.gap.map_or(0, |g| g.lost);
        let copy_lost = copy.gap.map_or(0, |g| g.lost);
        assert_eq!(
            copy_lost,
            live_lost + (live.recs.len() - copy.recs.len()) as u64,
            "from seq {cursor_seq}: the gap counts exactly what the copy left out"
        );
        assert_eq!(copy.next, live.next, "the same place to resume");
        if copy_lost > 0 {
            assert_eq!(copy.gap.map(|g| g.why), Some(GapWhy::Evicted));
        }
    }
    for r in kept.window() {
        let mut n = r.node;
        for _ in 0..8 {
            assert_eq!(kept.parent_of(n), log.parent_of(n), "{n}'s parent");
            match kept.parent_of(n) {
                Some(p) => n = p,
                None => break,
            }
        }
    }
    assert_eq!(kept.window().next().map(|r| r.op), Some(Op::Open));
    assert_eq!(kept.window().last().map(|r| r.op), Some(Op::Close));
}

#[test]
fn a_kept_copy_keeps_to_its_bytes_and_the_summary_to_its_session() {
    let log = closed_session(700);
    let skeleton = log.kept_within(64 * 1024);
    assert!(skeleton.bytes() <= 64 * 1024, "{} B", skeleton.bytes());
    assert_eq!(
        skeleton.window().nth_back(1).map(|r| r.seq),
        log.window().nth_back(1).map(|r| r.seq),
        "the newest record before the close is kept"
    );
    let summary = skeleton.kept_within(0);
    assert_eq!(summary.len(), 2, "the session's open and close");
    assert_eq!(summary.lost(), log.lost() + log.len() as u64 - 2);
    assert_eq!(
        summary.fold().state(),
        log.fold().state(),
        "the fold is whole"
    );
    assert_eq!(summary.read(Cursor::default(), usize::MAX).recs.len(), 2);
    let whole = log.kept_within(usize::MAX);
    assert_eq!(
        whole.len(),
        log.len(),
        "NEGATIVE CONTROL: it fits, nothing left out"
    );
    assert_eq!(whole.lost(), log.lost());
    println!(
        "MEASURE kept copy: log {} records {} B (footprint {} B); skeleton {} records {} B \
         (footprint {} B); summary footprint {} B",
        log.len(),
        log.bytes(),
        log.footprint(),
        skeleton.len(),
        skeleton.bytes(),
        skeleton.footprint(),
        summary.footprint()
    );
}

#[test]
fn a_fenced_logs_copy_reads_to_its_head() {
    let mut log = realistic_log(5);
    log.fence_reads();
    let fence = log.read_fence().expect("fenced");
    log.close_session(
        Close::new(Out::Session(SessionOut::UiClose), Src::Aterm, Conf::Exact).by(Actor::Human),
        at(10_000_000),
    )
    .expect("the session closes");
    assert_eq!(log.reader_last_seq(), fence, "CONTROL: the fence holds");
    let kept = log.kept_within(64 * 1024);
    assert_eq!(kept.read_fence(), None);
    assert_eq!(kept.reader_last_seq(), log.last_seq());
}

#[test]
fn a_closed_summary_keeps_to_a_kilobyte_and_reads_as_the_log_did() {
    use aterm_slog::CLOSED_SUMMARY_BYTES;
    let carried = |turns: u64, updates: u64| -> Log {
        let mut log = realistic_log(turns);
        for u in 0..updates {
            log.skip_to(log.last_seq() + 1_000 + u);
            log.mark(
                aterm_slog::Mark::new(aterm_slog::Kind::Bell, Src::Engine, Conf::Exact),
                at(20_000_000 + u),
            )
            .expect("a mark");
        }
        log.close_session(
            Close::new(Out::Session(SessionOut::UiClose), Src::Aterm, Conf::Exact).by(Actor::Human),
            at(30_000_000),
        )
        .expect("closes");
        log
    };
    for (turns, updates) in [(0, 0), (5, 0), (700, 0), (300, 3), (300, 64)] {
        let log = carried(turns, updates);
        let text = log.closed_summary();
        assert!(
            text.len() <= CLOSED_SUMMARY_BYTES,
            "{turns}/{updates}: {} B",
            text.len()
        );
        let back = Log::from_closed_summary(&text)
            .unwrap_or_else(|e| panic!("{turns}/{updates}: {}", e.why()));
        assert_eq!((back.ep(), back.last_seq()), (log.ep(), log.last_seq()));
        assert_eq!(back.window().last().map(|r| r.op), Some(Op::Close));
        assert_eq!(back.window().next().map(|r| r.op), Some(Op::Open));
        assert_eq!(
            back.fold().state().totals,
            log.fold().state().totals,
            "{turns}/{updates}: the totals as they stood"
        );
        assert_eq!(back.fold().state().closed, log.fold().state().closed);
        if updates < 3 {
            assert_eq!(back.first_seq(), log.first_seq(), "every overlap kept");
        }
        assert!(back.complete_from() >= log.complete_from());
        assert!(back.complete_from() <= log.last_seq());
        let seqs: Vec<u64> = log.window().map(|r| r.seq).collect();
        for cursor_seq in [
            0,
            1,
            seqs[seqs.len() / 2],
            log.last_seq() - 1,
            log.last_seq(),
        ] {
            let cursor = Cursor::new(log.ep(), cursor_seq);
            let live = log.read(cursor, usize::MAX);
            let copy = back.read(cursor, usize::MAX);
            assert_eq!(copy.next, live.next, "{turns}/{updates} from {cursor_seq}");
            if back.first_seq() == log.first_seq() {
                let lost = |g: Option<aterm_slog::Gap>| g.map_or(0, |g| g.lost);
                assert_eq!(
                    lost(copy.gap),
                    lost(live.gap) + (live.recs.len() - copy.recs.len()) as u64,
                    "{turns}/{updates} from {cursor_seq}: exactly what it left out"
                );
            }
        }
    }
    let old = closed_session(700).kept_within(0);
    assert!(
        old.footprint() > 4 * CLOSED_SUMMARY_BYTES,
        "{} B",
        old.footprint()
    );
}

/// C7's review: the journal writer's hold on a take ([`aterm_slog::Journaled`]) marks the take
/// spent first at once and gives it back to the log it was taken from, and to no other: a copy of
/// that log keeps a mark of its own, and a kept copy follows no writer.
#[test]
fn a_takes_hold_gives_back_to_its_own_log_only() {
    let mut log = realistic_log(3);
    log.note_journaled(2);
    let held = log.take_journaled(2, 9);
    assert_eq!(log.journaled(), Some(9), "the take is spent first at once");
    let copy = log.clone();
    let kept = log.kept_within(64 * 1024);
    held.unwritten();
    assert_eq!(log.journaled(), Some(2), "the hold gave the take back");
    assert_eq!(copy.journaled(), Some(9), "a copy's mark is its own");
    assert_eq!(kept.journaled(), None, "a kept copy follows no writer");
}
