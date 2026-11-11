// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn t0() -> Instant {
    Instant::now()
}

/// No row text: the scalar predicates do not read rows.
const NO_ROWS: &[Option<String>] = &[];

/// Matches a row containing the literal.
#[derive(Debug)]
struct Contains(&'static str);
impl RowMatch for Contains {
    fn verdict(&self, row: &str) -> RowVerdict {
        row.contains(self.0).into()
    }
}

/// Exactly the level-triggered predicates are evaluated at arm (`Terminal::watch`).
#[test]
fn needs_arm_eval_is_exactly_the_level_triggered_predicates() {
    assert!(WatcherSpec::SeqAdvanced { after: 0 }.needs_arm_eval());
    #[derive(Debug)]
    struct Never;
    impl RowMatch for Never {
        fn verdict(&self, _: &str) -> RowVerdict {
            RowVerdict::NoMatch
        }
    }
    assert!(
        WatcherSpec::RowMatches {
            matcher: Arc::new(Never),
            rows: RowRange::All,
        }
        .needs_arm_eval()
    );
    assert!(
        WatcherSpec::RowsClear {
            matcher: Arc::new(Never),
            rows: RowRange::All,
        }
        .needs_arm_eval(),
        "RowsClear is level-triggered: an already-clear surface latches at arm"
    );
    assert!(
        WatcherSpec::SeqAdvanced { after: 7 }.needs_arm_eval(),
        "SeqAdvanced is level-triggered; arming must not wait for the next batch"
    );
    assert!(
        WatcherSpec::FrameWhole.needs_arm_eval(),
        "FrameWhole is level-triggered: a whole frame at arm latches at arm"
    );
    assert!(
        !WatcherSpec::IdleFor {
            dur: Duration::from_millis(1)
        }
        .needs_arm_eval()
    );
    assert!(
        WatcherSpec::BlockComplete { after: 0 }.needs_arm_eval(),
        "BlockComplete is level-triggered: a count already past `after` latches at arm"
    );
}

/// `MomentumBelow` latches at its armed crossing whenever the host wakes, and content
/// activity does not move it; an `IdleFor` beside it is reset by the same activity.
#[test]
fn a_momentum_watcher_latches_at_its_crossing_and_content_activity_does_not_move_it() {
    let base = t0();
    let mut set = WatcherSet::default();
    let crossing = base + Duration::from_millis(500);
    let id = set
        .arm(
            WatcherSpec::MomentumBelow {
                floor: 0.25,
                crossing,
            },
            base,
        )
        .expect("budget");
    let idle = set
        .arm(
            WatcherSpec::IdleFor {
                dur: Duration::from_millis(500),
            },
            base,
        )
        .expect("budget");
    assert_eq!(set.next_deadline(), Some(crossing));

    let typed = base + Duration::from_millis(300);
    assert!(!set.observe(7, 0, typed, NO_ROWS));
    assert_eq!(
        set.next_deadline(),
        Some(crossing),
        "a keystroke must not move the analytic crossing"
    );
    assert!(!set.expire(typed), "nothing has crossed yet");
    assert!(set.poll(id).is_none());

    let late = base + Duration::from_millis(900);
    assert!(set.expire(late));
    let sat = set.poll(id).expect("crossed");
    assert_eq!(sat.at, crossing, "latched at the deadline, not at `now`");
    assert_eq!(sat.seq, 7, "the seq the clock last saw");
    assert_eq!(
        set.poll(idle).map(|s| s.at),
        Some(typed + Duration::from_millis(500))
    );
}

#[test]
fn watch_latches_an_already_advanced_seq_before_it_returns() {
    let mut term = crate::terminal::Terminal::new(6, 20);
    term.process(b"hello");
    let seq = term.content_seq();
    assert!(seq > 0, "feeding bytes must advance content_seq");

    let id = term
        .watch(WatcherSpec::SeqAdvanced { after: 0 }, t0())
        .expect("watcher budget");
    let latched = term
        .watch_poll(id)
        .expect("an ALREADY-advanced seq must latch at arm, with no new batch");
    assert_eq!(latched.seq, seq);

    let id2 = term
        .watch(WatcherSpec::SeqAdvanced { after: seq }, t0())
        .expect("watcher budget");
    assert!(
        term.watch_poll(id2).is_none(),
        "arming at the current seq must NOT latch — nothing has changed yet"
    );
}

#[test]
fn seq_advanced_latches_at_the_batch() {
    let base = t0();
    let mut w = WatcherSet::default();
    let id = w.arm(WatcherSpec::SeqAdvanced { after: 5 }, base).unwrap();
    assert!(w.poll(id).is_none(), "pending before any advance");
    w.observe(5, 0, base + Duration::from_millis(1), NO_ROWS);
    assert!(w.poll(id).is_none());
    let at = base + Duration::from_millis(2);
    w.observe(7, 0, at, NO_ROWS);
    assert_eq!(w.poll(id), Some(Satisfaction { seq: 7, at }));
}

/// A restarted idle watcher waits its whole window again, and a latched one
/// keeps its latch.
#[test]
fn a_restarted_idle_waits_its_whole_window_again() {
    let base = t0();
    let d = Duration::from_millis(250);
    let mut w = WatcherSet::default();
    let id = w.arm(WatcherSpec::IdleFor { dur: d }, base).unwrap();
    let later = base + Duration::from_millis(1000);
    let mut control = w.clone();
    control.expire(later);
    assert!(control.poll(id).is_some(), "the window passed unseen");
    w.restart_idle(later);
    w.expire(later + d - Duration::from_millis(1));
    assert!(
        w.poll(id).is_none(),
        "not before a whole window from the restart"
    );
    w.expire(later + d);
    assert_eq!(w.poll(id).map(|s| s.at), Some(later + d));
    w.restart_idle(later + Duration::from_secs(10));
    assert_eq!(w.poll(id).map(|s| s.at), Some(later + d), "a latch stays");
}

#[test]
fn live_and_replay_latch_identically() {
    let base = t0();
    let d = Duration::from_millis(250);
    let schedule = [
        (1u64, Duration::from_millis(10)),
        (2, Duration::from_millis(20)),
        (3, Duration::from_millis(35)),
    ];
    let run = |expire_at: Duration| {
        let mut w = WatcherSet::default();
        let id = w.arm(WatcherSpec::IdleFor { dur: d }, base).unwrap();
        for (seq, off) in schedule {
            w.observe(seq, 0, base + off, NO_ROWS);
        }
        w.expire(base + expire_at);
        w.poll(id)
    };
    let live = run(Duration::from_millis(35) + d + Duration::from_millis(2));
    let replay = run(Duration::from_millis(5000));
    assert_eq!(
        live, replay,
        "live and replay must latch the identical instant"
    );
    assert_eq!(live.unwrap().at, base + Duration::from_millis(35) + d);
}

#[test]
fn idle_armed_on_a_stale_quiet_surface_waits_the_full_window() {
    let base = t0();
    let d = Duration::from_millis(500);
    let mut w = WatcherSet::default();
    w.observe(1, 0, base, NO_ROWS);
    let arm_at = base + Duration::from_millis(5000);
    let id = w.arm(WatcherSpec::IdleFor { dur: d }, arm_at).unwrap();
    assert!(!w.expire(arm_at), "must not latch at arm on stale idleness");
    assert!(w.poll(id).is_none());
    assert!(w.expire(arm_at + d));
    assert_eq!(w.poll(id).unwrap().at, arm_at + d);
}

#[test]
fn next_deadline_is_the_minimum() {
    let base = t0();
    let mut w = WatcherSet::default();
    let near = w
        .arm(
            WatcherSpec::IdleFor {
                dur: Duration::from_millis(100),
            },
            base,
        )
        .unwrap();
    let _far = w
        .arm(
            WatcherSpec::IdleFor {
                dur: Duration::from_millis(300),
            },
            base,
        )
        .unwrap();
    assert_eq!(w.next_deadline(), Some(base + Duration::from_millis(100)));
    w.expire(base + Duration::from_millis(150));
    assert!(w.poll(near).is_some());
    assert_eq!(w.next_deadline(), Some(base + Duration::from_millis(300)));
}

#[test]
fn block_complete_latches_once_the_completed_count_passes_after() {
    let base = t0();
    let mut w = WatcherSet::default();
    let id = w
        .arm(WatcherSpec::BlockComplete { after: 3 }, base)
        .unwrap();
    w.observe(1, 3, base + Duration::from_millis(1), NO_ROWS);
    assert!(
        w.poll(id).is_none(),
        "no command finished since the count was read"
    );
    w.observe(2, 4, base + Duration::from_millis(2), NO_ROWS);
    assert_eq!(w.poll(id).unwrap().seq, 2);
}

/// PowerShell's prompt function sends `D`, `A`, its prompt and `B` as one string, so the
/// batch that finishes a command ends on the next prompt's input; a count already past
/// `after` latches at arm, and a keystroke echoed at a prompt, with or without its `B`,
/// finishes nothing.
#[test]
fn block_complete_latches_on_a_completion_the_next_prompt_follows_in_the_same_batch() {
    let mut term = crate::terminal::Terminal::new(6, 40);
    term.process(b"\x1b]133;A\x07PS> \x1b]133;B\x07make\r\n\x1b]133;C\x07");
    let block = |term: &mut crate::terminal::Terminal| {
        let after = term.completed_command_seq();
        term.watch(WatcherSpec::BlockComplete { after }, t0())
            .expect("watcher budget")
    };
    let id = block(&mut term);
    assert!(
        term.watch_poll(id).is_none(),
        "armed at the count, nothing latches at arm"
    );
    term.process(b"built\r\n");
    assert!(term.watch_poll(id).is_none(), "the command still runs");
    term.process(b"\x1b]133;D;0\x07\x1b]133;A\x07PS> \x1b]133;B\x07");
    let latched = term
        .watch_poll(id)
        .expect("the D latches although the batch ends on the next prompt's B");
    assert_eq!(latched.seq, term.content_seq());
    let after = term.completed_command_seq() - 1;
    let behind = term
        .watch(WatcherSpec::BlockComplete { after }, t0())
        .expect("watcher budget");
    assert!(
        term.watch_poll(behind).is_some(),
        "a count already past `after` latches before `watch` returns"
    );

    let typed = block(&mut term);
    term.process(b"l");
    assert!(term.watch_poll(typed).is_none(), "an echo after B");
    term.process(b"\x1b]133;A\x07$ ");
    let bare = block(&mut term);
    term.process(b"l");
    assert!(
        term.watch_poll(bare).is_none(),
        "an echo at a prompt with no B"
    );
}

/// Equal-seq re-entry: watchers armed at `(seq 15, alt, epoch 1)` see
/// `(seq 15, alt, epoch 2)` as a change.
#[test]
fn a_new_screen_generation_at_an_equal_seq_is_an_advance() {
    let base = t0();
    let box_b = [Some("rm -rf /tmp/work".to_string())];
    for (epoch, moves) in [(1, false), (2, true)] {
        let mut w = WatcherSet::default();
        w.seed_screen(15, true, 1);
        let rows = w
            .arm(
                WatcherSpec::RowMatches {
                    matcher: Arc::new(Contains("rm -rf")),
                    rows: RowRange::All,
                },
                base,
            )
            .unwrap();
        let seq = w.arm(WatcherSpec::SeqAdvanced { after: 15 }, base).unwrap();
        w.observe_screen(
            15,
            true,
            1,
            0,
            base,
            &[Some("ls".to_string())],
            Frame::CLOSED,
        );
        assert!(w.poll(rows).is_none() && w.poll(seq).is_none());
        assert_eq!(w.is_advance(15, true, epoch), moves, "epoch {epoch}");
        w.observe_screen(15, true, epoch, 0, base, &box_b, Frame::CLOSED);
        assert_eq!(w.poll(rows).is_some(), moves, "row watcher, epoch {epoch}");
        assert_eq!(w.poll(seq).is_some(), moves, "seq watcher, epoch {epoch}");
        assert_eq!(w.seen_epoch(), epoch);
    }
}

/// The shared row matcher for the `RowsClear` tests below.
#[derive(Debug)]
struct Busy;
impl RowMatch for Busy {
    fn verdict(&self, row: &str) -> RowVerdict {
        row.contains("esc to interrupt").into()
    }
}

fn rows_clear(range: RowRange) -> WatcherSpec {
    WatcherSpec::RowsClear {
        matcher: Arc::new(Busy),
        rows: range,
    }
}

#[test]
fn rows_clear_latches_at_arm_when_nothing_matches() {
    let base = t0();
    let mut w = WatcherSet::default();
    w.seed_seq(7);
    let id = w.arm(rows_clear(RowRange::All), base).unwrap();
    let rows = [Some("done".to_string()), Some("❯ ".to_string())];
    w.observe(7, 0, base, &rows);
    assert_eq!(
        w.poll(id).unwrap().seq,
        7,
        "no matching row at arm => latched on the fresh scan, same seq"
    );
}

/// A quiet frame in between, with no rows collected, is not read as clear.
#[test]
fn rows_clear_holds_while_a_row_matches_then_latches_when_it_leaves() {
    let base = t0();
    let mut w = WatcherSet::default();
    let id = w.arm(rows_clear(RowRange::All), base).unwrap();
    let busy = [
        Some("thinking…".to_string()),
        Some("esc to interrupt".to_string()),
    ];
    w.observe(1, 0, base, &busy);
    assert!(w.poll(id).is_none(), "a matching row keeps `gone` pending");
    let still_busy = [
        Some("still thinking…".to_string()),
        Some("(3s · esc to interrupt)".to_string()),
    ];
    w.observe(2, 0, base, &still_busy);
    assert!(w.poll(id).is_none(), "still matching => still pending");
    w.observe(2, 0, base, NO_ROWS);
    assert!(
        w.poll(id).is_none(),
        "an un-scanned frame is not a clear surface"
    );
    let clear = [Some("Done.".to_string()), Some("❯ ".to_string())];
    w.observe(3, 0, base, &clear);
    assert_eq!(w.poll(id).unwrap().seq, 3);
}

#[test]
fn rows_clear_range_ignores_a_match_outside_the_range() {
    let base = t0();
    let mut w = WatcherSet::default();
    let rows = [
        Some("esc to interrupt".to_string()),
        Some("body".to_string()),
        Some("footer".to_string()),
    ];
    let outside = w
        .arm(rows_clear(RowRange::Span { start: 1, end: 2 }), base)
        .unwrap();
    let covering = w
        .arm(rows_clear(RowRange::Span { start: 0, end: 1 }), base)
        .unwrap();
    w.observe(1, 0, base, &rows);
    assert_eq!(
        w.poll(outside).unwrap().seq,
        1,
        "the match on row 0 is outside rows 1..=2 => clear => latched"
    );
    assert!(
        w.poll(covering).is_none(),
        "the same match inside rows 0..=1 holds that watcher pending"
    );
}

/// `RowsClear` on an unbracketed screen requires `GONE_SETTLE` of clear scans before latching.
#[test]
fn rows_clear_settles_on_a_screen_no_closed_frame_vouches_for() {
    let base = t0();
    let ms = |n| base + Duration::from_millis(n);
    let busy = [Some("esc to interrupt".to_string())];
    let clear = [Some("Done.".to_string())];
    let unb = |n| Frame::unbracketed(Some(ms(n)));
    let mut w = WatcherSet::default();
    let id = w.arm(rows_clear(RowRange::All), base).unwrap();
    w.observe_screen(1, false, 0, 0, ms(0), &busy, unb(0));
    w.observe_screen(2, false, 0, 0, ms(10), &clear, unb(10));
    assert!(w.poll(id).is_none());
    assert_eq!(w.next_deadline(), Some(ms(10) + GONE_SETTLE));
    w.observe_screen(3, false, 0, 0, ms(50), &clear, unb(50));
    assert_eq!(w.next_deadline(), Some(ms(10) + GONE_SETTLE));
    w.observe_screen(4, false, 0, 0, ms(60), &busy, unb(60));
    assert_eq!(w.next_deadline(), None);
    assert!(!w.expire(ms(10) + GONE_SETTLE));
    w.observe_screen(5, false, 0, 0, ms(200), &clear, unb(200));
    w.restart_idle(ms(250));
    assert_eq!(w.next_deadline(), Some(ms(250) + GONE_SETTLE));
    assert!(!w.expire(ms(200) + GONE_SETTLE));
    assert!(w.expire(ms(250) + GONE_SETTLE));
    assert_eq!(
        w.poll(id),
        Some(Satisfaction {
            seq: 5,
            at: ms(250) + GONE_SETTLE
        })
    );
    let closed = w.arm(rows_clear(RowRange::All), ms(300)).unwrap();
    w.observe_screen(6, false, 0, 0, ms(300), &clear, Frame::CLOSED);
    assert_eq!(
        w.poll(closed),
        Some(Satisfaction {
            seq: 6,
            at: ms(300)
        })
    );
}

/// An empty range keeps `RowsClear` pending: no reading is not a clear reading.
#[test]
fn rows_clear_never_latches_on_a_range_that_covers_no_visible_row() {
    let base = t0();
    let mut w = WatcherSet::default();
    let past_the_grid = w
        .arm(rows_clear(RowRange::Span { start: 40, end: 50 }), base)
        .unwrap();
    let inverted = w
        .arm(rows_clear(RowRange::Span { start: 2, end: 1 }), base)
        .unwrap();
    let clear = [
        Some("Done.".to_string()),
        Some("❯ ".to_string()),
        Some(String::new()),
    ];
    w.observe(1, 0, base, &clear);
    w.observe(2, 0, base, &clear);
    assert!(
        w.poll(past_the_grid).is_none(),
        "rows 40..=50 on a 3-row grid meets no row: no vacuous latch"
    );
    assert!(
        w.poll(inverted).is_none(),
        "rows 2..=1 is inverted, so empty: no vacuous latch"
    );
    let covering = w
        .arm(rows_clear(RowRange::Span { start: 2, end: 50 }), base)
        .unwrap();
    w.observe(2, 0, base, &clear);
    assert_eq!(
        w.poll(covering).unwrap().seq,
        2,
        "a span reaching past the grid is intersected with it, not emptied"
    );
    assert!(!RowRange::Span { start: 40, end: 50 }.covers_any(24));
    assert!(!RowRange::Span { start: 2, end: 1 }.covers_any(24));
    assert!(RowRange::Span { start: 23, end: 99 }.covers_any(24));
    assert!(RowRange::All.covers_any(1));
    assert!(!RowRange::All.covers_any(0));
}

/// An undecided row (a bounded engine out of budget) neither fires `RowMatches` nor lets
/// `RowsClear` latch; both answer once a later scan decides it.
#[test]
fn an_undecided_row_holds_both_row_predicates_pending() {
    /// Decides a row only when it is short: a stand-in for a scan budget.
    #[derive(Debug)]
    struct Budgeted;
    impl RowMatch for Budgeted {
        fn verdict(&self, row: &str) -> RowVerdict {
            if row.len() > 16 {
                RowVerdict::Undecided
            } else {
                row.contains("busy").into()
            }
        }
    }
    let base = t0();
    let mut w = WatcherSet::default();
    let busy = || Arc::new(Budgeted) as Arc<dyn RowMatch>;
    let shown = w
        .arm(
            WatcherSpec::RowMatches {
                matcher: busy(),
                rows: RowRange::All,
            },
            base,
        )
        .unwrap();
    let gone = w
        .arm(
            WatcherSpec::RowsClear {
                matcher: busy(),
                rows: RowRange::All,
            },
            base,
        )
        .unwrap();
    let heavy = [Some("done".to_string()), Some(format!("{:>40}", "busy"))];
    w.observe(1, 0, base, &heavy);
    assert!(w.poll(shown).is_none(), "undecided is not a match");
    assert!(w.poll(gone).is_none(), "undecided is not clear");
    let light = [Some("done".to_string()), Some("busy".to_string())];
    w.observe(2, 0, base, &light);
    assert_eq!(w.poll(shown).unwrap().seq, 2);
    assert!(w.poll(gone).is_none(), "a decided match holds `gone`");
    let clear = [Some("done".to_string()), Some("❯ ".to_string())];
    w.observe(3, 0, base, &clear);
    assert_eq!(w.poll(gone).unwrap().seq, 3);
}

#[test]
fn arm_is_bounded_and_fails_closed() {
    let base = t0();
    let mut w = WatcherSet::with_capacity(2);
    assert!(w.arm(WatcherSpec::SeqAdvanced { after: 0 }, base).is_some());
    assert!(w.arm(WatcherSpec::SeqAdvanced { after: 0 }, base).is_some());
    assert!(
        w.arm(WatcherSpec::SeqAdvanced { after: 0 }, base).is_none(),
        "third arm past capacity fails closed"
    );
}

#[test]
fn disarm_frees_a_slot() {
    let base = t0();
    let mut w = WatcherSet::with_capacity(1);
    let id = w.arm(WatcherSpec::SeqAdvanced { after: 0 }, base).unwrap();
    assert!(w.arm(WatcherSpec::SeqAdvanced { after: 0 }, base).is_none());
    w.disarm(id);
    assert!(w.arm(WatcherSpec::SeqAdvanced { after: 0 }, base).is_some());
    assert!(w.poll(id).is_none(), "disarmed id no longer known");
}

/// The reused `row_text_scratch` sees the same text a fresh collection would.
#[test]
fn observe_at_reuses_scratch_across_batches_and_still_latches() {
    use crate::terminal::Terminal;

    let now = t0();
    let mut term = Terminal::new(4, 40);
    let id = term
        .watch_rows(Arc::new(Contains("BUILD SUCCESSFUL")), RowRange::All, now)
        .expect("arm the row watcher");

    term.process(b"Compiling...\r\n");
    assert!(term.watch_poll(id).is_none(), "pending while building (1)");
    term.process(b"Linking...\r\n");
    assert!(term.watch_poll(id).is_none(), "pending while building (2)");
    term.process(b"Running tests...\r\n");
    assert!(term.watch_poll(id).is_none(), "pending while building (3)");

    term.process(b"BUILD SUCCESSFUL");
    assert!(
        term.watch_poll(id).is_some(),
        "RowMatches must latch once a visible row matches, even with scratch reuse",
    );
}

/// The hull is taken over exactly the watchers `observe_screen` scans, each range
/// intersected with the grid.
#[test]
fn row_scan_hull_covers_the_scanning_watchers_on_the_grid() {
    let base = t0();
    let span = |start, end| RowRange::Span { start, end };
    let shown = |range| WatcherSpec::RowMatches {
        matcher: Arc::new(Contains("never shown")),
        rows: range,
    };
    let mut w = WatcherSet::default();
    assert_eq!(w.row_scan_hull(true, 24), None, "no watcher, no scan");
    let scalar = w.arm(WatcherSpec::SeqAdvanced { after: 0 }, base).unwrap();
    assert_eq!(
        w.row_scan_hull(true, 24),
        None,
        "a scalar watcher reads no row"
    );
    w.disarm(scalar);
    w.arm(shown(span(40, 50)), base).unwrap();
    w.arm(rows_clear(span(2, 1)), base).unwrap();
    assert_eq!(w.row_scan_hull(true, 24), None);
    w.arm(shown(span(20, 20)), base).unwrap();
    w.arm(rows_clear(span(5, 99)), base).unwrap();
    assert_eq!(
        w.row_scan_hull(true, 24),
        Some((5, 23)),
        "rows 5..=99 is cut at the last grid row"
    );
    w.arm(shown(span(0, 0)), base).unwrap();
    assert_eq!(w.row_scan_hull(true, 24), Some((0, 23)));
    assert_eq!(
        w.row_scan_hull(true, 0),
        None,
        "a grid with no rows has none to read"
    );
    let busy = vec![Some("esc to interrupt".to_string()); 24];
    w.observe(0, 0, base, &busy);
    assert_eq!(w.row_scan_hull(false, 24), None);
    assert_eq!(w.row_scan_hull(true, 24), Some((0, 23)));
    w.arm(rows_clear(RowRange::All), base).unwrap();
    assert_eq!(w.row_scan_hull(false, 3), Some((0, 2)));
    let mut once = WatcherSet::default();
    let id = once.arm(shown(RowRange::All), base).unwrap();
    once.observe(1, 0, base, &[Some("never shown".to_string())]);
    assert!(once.poll(id).is_some());
    assert_eq!(once.row_scan_hull(true, 24), None);
}

/// `observe_at` renders only the armed rows' hull, and the scratch keeps one entry per
/// grid row.
#[test]
fn observe_at_renders_only_the_armed_span() {
    use crate::terminal::Terminal;

    let rendered = |t: &Terminal| -> Vec<usize> {
        t.row_text_scratch
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.is_some().then_some(i))
            .collect()
    };
    let now = t0();
    let mut term = Terminal::new(24, 40);
    let top = term
        .watch_rows(
            Arc::new(Contains("never shown")),
            RowRange::Span { start: 0, end: 0 },
            now,
        )
        .expect("arm rows 0..=0");
    term.process(b"one\r\n");
    assert_eq!(term.row_text_scratch.len(), 24, "one entry per grid row");
    assert_eq!(
        rendered(&term),
        [0],
        "a rows 0..=0 watcher reads row 0 alone"
    );
    let all = term
        .watch_rows(Arc::new(Contains("never shown")), RowRange::All, now)
        .expect("arm all rows");
    term.process(b"two\r\n");
    assert_eq!(rendered(&term), (0..24).collect::<Vec<_>>());
    term.watch_disarm(all);
    term.process(b"three\r\n");
    assert_eq!(rendered(&term), [0]);
    assert!(term.watch_poll(top).is_none());
}

/// Watchers on disjoint spans latch at the same seqs whether the scan reads only their
/// hull or, with an extra `All` watcher armed, every row.
#[test]
fn disjoint_spans_latch_exactly_as_a_full_scan() {
    use crate::terminal::{ClockReading, Terminal};

    const SETUP: &[u8] = b"\x1b[6;1Hbusy\x1b[24;1HREADY";
    const CORPUS: [&[u8]; 6] = [
        b"\x1b[22;1HREADY",
        b"\x1b[21;1HREADY",
        b"\x1b[7;1Hstill busy",
        b"\x1b[6;1H\x1b[2K",
        b"\x1b[7;1H\x1b[2K",
        b"\x1b[1;1HREADY",
    ];
    let run = |full_scan: bool| {
        let now = t0();
        let at = |i: u64| ClockReading {
            monotonic: now + Duration::from_secs(i),
            wall_ms: None,
        };
        let mut term = Terminal::new(24, 40);
        term.process_at(SETUP, at(0));
        if full_scan {
            term.watch_rows(Arc::new(Contains("never shown")), RowRange::All, now)
                .expect("arm the full-scan watcher");
        }
        let ids = [
            term.watch_rows(
                Arc::new(Contains("READY")),
                RowRange::Span { start: 0, end: 0 },
                now,
            ),
            term.watch_rows(
                Arc::new(Contains("READY")),
                RowRange::Span { start: 20, end: 20 },
                now,
            ),
            term.watch_rows_gone(
                Arc::new(Contains("busy")),
                RowRange::Span { start: 5, end: 7 },
                now,
            ),
        ]
        .map(|id| id.expect("arm"));
        let mut latches = Vec::new();
        let mut read_last_row = false;
        for (i, batch) in (1..).zip(CORPUS) {
            term.process_at(batch, at(i));
            term.watch_expire(at(i).monotonic + GONE_SETTLE);
            read_last_row |= term.row_text_scratch.get(23).is_some_and(Option::is_some);
            latches.push(ids.map(|id| term.watch_poll(id).map(|s| s.seq)));
        }
        (latches, read_last_row)
    };
    let (hull, hull_read_last) = run(false);
    let (full, full_read_last) = run(true);
    assert!(full_read_last, "the full scan reads row 23");
    assert!(
        !hull_read_last,
        "the hull of 0..=0, 20..=20 and 5..=7 never reads row 23"
    );
    assert_eq!(hull, full, "the hull latches exactly as the full scan");
    let first_latch: Vec<Option<usize>> = (0..3)
        .map(|w| hull.iter().position(|step| step[w].is_some()))
        .collect();
    assert_eq!(first_latch, [Some(5), Some(1), Some(4)]);
}
