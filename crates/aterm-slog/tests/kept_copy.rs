// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CLOSED SESSION'S COPY (`Log::kept_within`, design §6): what a host's
//! closed-session index keeps of a log reads as the log did, its accounting
//! exact, within the bytes it was given.

use aterm_slog::{Actor, Close, Conf, Cursor, GapWhy, Log, Op, Out, SessionOut, Src, Tier, encode};

use crate::measure::realistic_log;
use crate::support::at;

/// A realistic session, closed: `turns` agent turns with their steps and
/// commands, and the session's close.
fn closed_session(turns: u64) -> Log {
    let mut log = realistic_log(turns);
    log.close_session(
        Close::new(Out::Session(SessionOut::UiClose), Src::Aterm, Conf::Exact).by(Actor::Human),
        at(10_000_000),
    )
    .expect("the session closes");
    log
}

/// A KEPT COPY READS AS THE LOG DID: from any cursor, the copy shows a
/// subset of what the log showed past it, in the same seqs, and its gap
/// counts exactly what it left out on top of what the log had already lost.
/// The epoch, head and session records are the log's.
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
    // Every kept record's chain walks as it did (a `log <node>` subtree).
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
    // The session's own open and close are kept.
    assert_eq!(kept.window().next().map(|r| r.op), Some(Op::Open));
    assert_eq!(kept.window().last().map(|r| r.op), Some(Op::Close));
}

/// ITS BOUNDS: the skeleton keeps to its bytes, the newest records first
/// (the policy a live ring runs), and the summary (0 bytes) keeps only the
/// session's open and close, with the whole of its accounting and fold.
/// NEGATIVE CONTROL: a budget the log already fits leaves it whole.
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

/// A FENCED LOG'S COPY SHOWS EVERYTHING: an update's read fence holds a
/// live log's readers back; the copy of a session that closed under it has
/// no reader the fence protects, so it reads to the head. CONTROL: the
/// fenced log itself reads only to its fence.
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

/// THE CLOSED SUMMARY IS AT MOST A KILOBYTE AND READS AS THE LOG DID: for
/// closed sessions small and full, carried across updates (overlaps in its
/// seqs) or not, the text keeps to `CLOSED_SUMMARY_BYTES`, and the log it
/// reads back as answers every cursor as the closed log did, the gap
/// counting exactly the records the summary left out: the same epoch,
/// head, `oldest=`, session open and close, and the fold's totals and the
/// session's close as they stood. NEGATIVE CONTROL: the summary the index
/// kept before (the log evicted to its session's open and close, a `Log`
/// with its fold) is several times that on the heap.
#[test]
fn a_closed_summary_keeps_to_a_kilobyte_and_reads_as_the_log_did() {
    use aterm_slog::CLOSED_SUMMARY_BYTES;
    let carried = |turns: u64, updates: u64| -> Log {
        let mut log = realistic_log(turns);
        for u in 0..updates {
            // An overlap a carried update leaves (`Log::skip_to`), then more.
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
        // Whole only past what it left out (the close, at the head, is kept).
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
    // NEGATIVE CONTROL: the copy the index kept before.
    let old = closed_session(700).kept_within(0);
    assert!(
        old.footprint() > 4 * CLOSED_SUMMARY_BYTES,
        "{} B",
        old.footprint()
    );
}
