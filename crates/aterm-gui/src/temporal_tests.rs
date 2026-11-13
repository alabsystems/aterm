// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn records_raw_in_reply_resize_on_one_spine() {
    let mut r = TemporalRecorder::new();
    r.record_raw_in(b"ls -la\n");
    r.record_reply();
    r.record_resize(30, 100);

    assert_eq!(r.total_events(), 3, "raw_in + reply + resize");
    assert_eq!(r.live_events(), 3);
    assert_eq!(r.dropped_events(), 0, "nothing evicted under capacity");
    let ops: Vec<Op> = r.log.live().map(|e| e.op.clone()).collect();
    assert_eq!(
        ops,
        [
            Op::RawIn(BlobId(0)),
            Op::Reply(BlobId(1)),
            Op::Resize {
                rows: 30,
                cols: 100
            }
        ]
    );
    assert_eq!(r.blob_bytes(BlobId(0)), Some(&b"ls -la\n"[..]));
    assert_eq!(
        r.blob_bytes(BlobId(1)),
        None,
        "a reply's bytes are not kept"
    );
    assert_eq!(r.used, b"ls -la\n".len(), "only the input is charged");
}

#[test]
fn keyframe_is_recorded_and_counted() {
    let mut t = aterm_core::terminal::Terminal::new(6, 20);
    t.process(b"seed content\r\n");
    assert!(t.parser_is_ground());
    let cp = t.checkpoint();

    let mut r = TemporalRecorder::new();
    r.record_keyframe(cp);
    r.record_raw_in(b"more");

    assert_eq!(r.keyframe_count(), 1);
    assert_eq!(r.total_events(), 2, "Keyframe event + RawIn event");
}

#[test]
fn replay_at_seek_matches_the_full_spine_scan() {
    let mut t = aterm_core::terminal::Terminal::new(6, 20);
    t.process(b"seed\r\n");
    let mut r = TemporalRecorder::new();
    r.record_keyframe(t.checkpoint());
    for i in 0..40u32 {
        r.record_raw_in(format!("line{i}\r\n").as_bytes());
        if i % 9 == 0 {
            r.record_resize(6, 20 + (i % 3) as u16);
            let mut t2 = aterm_core::terminal::Terminal::new(6, 20);
            t2.process(format!("kf{i}\r\n").as_bytes());
            r.record_keyframe(t2.checkpoint());
        }
    }
    assert!(
        r.keyframe_count() > 1,
        "REACH: the fixture must record several keyframes"
    );

    let ticks: Vec<u64> = r.log.live().map(|e| e.ts.0).collect();
    assert!(
        ticks.windows(2).all(|w| w[1] >= w[0]),
        "spine ticks must be nondecreasing"
    );
    assert_eq!(
        r.latest_tick(),
        r.log.live().map(|e| e.ts).max().unwrap_or_default(),
        "latest_tick must equal the max fold it replaced"
    );

    for kf in &r.keyframes {
        let ev = r
            .log
            .live()
            .find(|e| e.seq == kf.seq)
            .expect("a retained keyframe is live on the spine");
        assert_eq!(
            ev.op,
            Op::Keyframe(kf.id),
            "seq {:?} is not this keyframe",
            kf.seq
        );
        assert_eq!(ev.ts, kf.ts, "retained ts disagrees with the spine");
    }

    let reference = |at: Ticks| -> Option<Vec<String>> {
        let mut base: Option<(Seq, KeyframeId)> = None;
        for ev in r.log.live() {
            if ev.ts > at {
                continue;
            }
            if let Op::Keyframe(kid) = ev.op {
                base = Some((ev.seq, kid));
            }
        }
        let (base_seq, base_kid) = base?;
        let cp = r
            .keyframes
            .iter()
            .find(|kf| kf.id == base_kid)
            .map(|kf| &kf.checkpoint)?;
        let mut term = Terminal::from_checkpoint(cp);
        for ev in r.log.live() {
            if ev.seq <= base_seq || ev.ts > at {
                continue;
            }
            match ev.op {
                Op::RawIn(id) => term.process(r.blob_bytes(id)?),
                Op::Resize { rows, cols } => {
                    term.resize_with_policy(rows, cols, r.resize_policy);
                }
                _ => {}
            }
        }
        Some(screen_of(&term))
    };
    let observed =
        |at: Ticks| -> Option<Vec<String>> { r.replay_at(Some(at)).map(|t| screen_of(&t)) };

    let latest = r.latest_tick();
    let mut probes: Vec<Ticks> = vec![Ticks(0), Ticks(latest.0 / 2), latest, Ticks(latest.0 + 1)];
    probes.extend(r.log.live().map(|e| e.ts));
    probes.extend(r.log.live().map(|e| Ticks(e.ts.0.saturating_sub(1))));
    let mut reached = 0usize;
    for at in probes {
        let (a, b) = (observed(at), reference(at));
        assert_eq!(a, b, "replay_at({at:?}) diverged from the full-spine scan");
        if a.is_some() {
            reached += 1;
        }
    }
    assert!(
        reached > 0,
        "REACH: every probe was unreachable — the differential proved nothing"
    );
    assert_eq!(
        r.replay_at(None).map(|t| screen_of(&t)),
        reference(latest),
        "the default replay target must still be the latest recorded instant"
    );
}

fn screen_of(t: &Terminal) -> Vec<String> {
    (0..t.rows() as usize)
        .map(|r| crate::control::visible_row(t, r))
        .collect()
}

#[test]
fn shared_burst_is_retained_not_copied_and_refunded_on_eviction() {
    let burst: Arc<[u8]> = Arc::from(&[b'y'; 64][..]);
    let mut r = TemporalRecorder::with_budget(1024);
    r.record_raw_in_shared(burst.clone());
    assert_eq!(Arc::strong_count(&burst), 2, "blob shares the caller's Arc");
    assert_eq!(r.used, 64);
    for _ in 0..64 {
        r.record_raw_in(&[b'x'; 64]);
    }
    assert_eq!(
        Arc::strong_count(&burst),
        1,
        "evicted blob released its ref"
    );
    assert!(
        r.used <= r.budget,
        "eviction refunded blob bytes: used={}",
        r.used
    );
}

#[test]
fn budget_bounds_blobs_without_silent_event_loss_on_spine() {
    let mut r = TemporalRecorder::with_budget(256);
    for _ in 0..1000 {
        r.record_raw_in(&[b'x'; 64]);
        assert!(r.used <= r.budget, "used={} past the budget", r.used);
    }
    assert_eq!(r.total_events(), 1000);
    assert_eq!(r.live_events(), 1000, "the ring holds every event");
    assert_eq!(r.dropped_events(), 0);
    assert_eq!(r.used, 256);
    assert_eq!((r.blob_base(), r.blobs.len()), (996, 4));
}

#[test]
fn replay_applies_resize_in_spine_order_after_cursor_addressed_write() {
    let mut r = TemporalRecorder::new();
    let blank = aterm_core::terminal::Terminal::new(4, 10);
    r.record_keyframe(blank.checkpoint());
    r.record_raw_in(b"\x1b[16GX");
    r.record_resize(4, 20);

    let term = r.replay_at(None).expect("replay");
    assert_eq!(term.cols(), 20, "the resize is applied");
    let row0 = term.get_line_text(0, None).unwrap_or_default();
    assert_eq!(
        row0.find('X'),
        Some(9),
        "X at the pre-resize column: row0={row0:?}"
    );
}

#[test]
fn replay_resizes_under_the_sessions_seam_policy() {
    let mut live = Terminal::new(6, 20);
    let mut r = TemporalRecorder::new();
    r.set_resize_policy(ResizePolicy::ConPty);
    r.record_keyframe(live.checkpoint());
    let lines: Vec<u8> = (0..12)
        .flat_map(|i| format!("L{i:02}\r\n").into_bytes())
        .collect();
    live.process(&lines);
    r.record_raw_in(&lines);

    let screen = screen_of(&live);
    live.resize_with_policy(10, 20, ResizePolicy::ConPty);
    r.record_resize(10, 20);
    let mut repaint = b"\x1b[H".to_vec();
    for (i, row) in screen.iter().enumerate() {
        if i > 0 {
            repaint.extend_from_slice(b"\r\n");
        }
        repaint.extend_from_slice(row.trim_end().as_bytes());
        repaint.extend_from_slice(b"\x1b[K");
    }
    repaint.extend_from_slice(&b"\r\n\x1b[K".repeat(4));
    repaint.extend_from_slice(b"\x1b[6;1H");
    live.process(&repaint);
    r.record_raw_in(&repaint);

    let history = |t: &Terminal| -> Vec<String> {
        (0..t.grid().scrollback_lines())
            .map(|i| {
                t.grid()
                    .get_history_line(i)
                    .map(|l| l.to_string())
                    .unwrap_or_default()
            })
            .collect()
    };
    assert_eq!(live.grid().scrollback_lines(), 7, "precondition");
    let replayed = r.replay_at(None).expect("replay");
    assert_eq!(
        history(&replayed),
        history(&live),
        "the history the live engine kept"
    );
    assert_eq!(screen_of(&replayed), screen_of(&live));

    r.set_resize_policy(ResizePolicy::Native);
    let native = r.replay_at(None).expect("replay");
    assert_eq!(native.grid().scrollback_lines(), 3);
}

#[test]
fn a_replay_takes_the_sessions_posture_over_its_keyframes() {
    let mut native = Terminal::new(6, 20);
    native.set_console_host_posture(ResizePolicy::Native);
    let keyframe = native.checkpoint();
    let mut r = TemporalRecorder::new();
    r.set_resize_policy(ResizePolicy::ConPty);
    r.record_keyframe(keyframe.clone());
    r.record_raw_in(b"\x1b[?9001h");
    let replayed = r.replay_at(None).expect("replay");
    let archiving = |t: &Terminal| {
        t.alt_archive_read(aterm_core::terminal::AltArchiveQuery::oldest(0, 0))
            .enabled
    };
    assert!(replayed.modes().win32_input_mode, "the ConPTY posture");
    assert!(!archiving(&replayed));

    let mut bare = Terminal::from_checkpoint(&keyframe);
    bare.process(b"\x1b[?9001h");
    assert!(!bare.modes().win32_input_mode);
    assert!(archiving(&bare));
}

#[test]
fn replay_leaves_a_dead_alt_screen_under_the_sessions_host_recovery() {
    let mut live = Terminal::new(6, 20);
    live.set_host_resyncs_alt_screen(true);
    let mut r = TemporalRecorder::new();
    r.set_host_resyncs_alt_screen(true);
    r.record_keyframe(live.checkpoint());
    let mut feed = |live: &mut Terminal, bytes: &[u8]| {
        live.process(bytes);
        r.record_raw_in(bytes);
    };
    feed(
        &mut live,
        b"C:\\> less\r\n\x1b[?1049h\x1b[H\x1b[2Jpager 1\r\n:",
    );
    feed(
        &mut live,
        b"\r\n\x1b]133;A\x07C:\\Users\\me> \x1b]133;B\x07",
    );
    assert!(live.take_alt_screen_resync_request(), "precondition");
    assert!(live.is_alternate_screen(), "precondition");
    feed(&mut live, b"\x1b[?1049l");
    feed(&mut live, b"dir\r\nx\r\n\x1b]133;A\x07C:\\Users\\me> ");
    assert!(!live.is_alternate_screen(), "precondition");

    let replayed = r.replay_at(None).expect("replay");
    assert_eq!(screen_of(&replayed), screen_of(&live));
    assert_eq!(
        (replayed.cursor().row, replayed.cursor().col),
        (live.cursor().row, live.cursor().col)
    );

    r.set_host_resyncs_alt_screen(false);
    let local = r.replay_at(None).expect("replay");
    assert_ne!(screen_of(&local), screen_of(&live));
}

#[test]
fn blob_handle_resolves_after_eviction_shifts_the_deque() {
    let mut r = TemporalRecorder::with_budget(150);
    r.record_raw_in(&[b'a'; 64]);
    r.record_raw_in(&[b'b'; 64]);
    r.record_raw_in(&[b'c'; 64]);

    assert!(r.blob_bytes(BlobId(0)).is_none(), "evicted handle => None");
    assert_eq!(r.blob_bytes(BlobId(1)), Some(&[b'b'; 64][..]));
    assert_eq!(r.blob_bytes(BlobId(2)), Some(&[b'c'; 64][..]));
    assert!(r.blob_bytes(BlobId(3)).is_none(), "unissued handle => None");
}

/// The readers' rekeys keep the latest instant reachable through four budgets of output: each a follows keyframe of the live screen, every kept one replays, and the latest replay is the live screen.
#[test]
fn rekeyframing_keeps_the_latest_reachable_past_the_budget() {
    let mut rig = ReaderRig::new(Terminal::new(6, 24), 1024 * 1024);
    for i in 0..4096u32 {
        rig.slice(format!("row {i:06}\r\n").as_bytes(), 0);
        rig.slice(&[b'.'; 1000], 0);
        rig.slice(b"\r\n", 0);
    }
    rig.slice(b"FINAL-SENTINEL-LINE\r\n", 0);

    let r = &rig.r;
    let term = r
        .replay_at(None)
        .expect("re-keyframing keeps the latest instant reachable past the budget");
    assert_eq!(
        term.checkpoint_visible(),
        rig.live.checkpoint_visible(),
        "the latest replay is the live screen"
    );
    assert!(
        r.keyframes.iter().all(|kf| kf.follows),
        "every kept keyframe is a reader's rekey: the seed went with the budget"
    );
    let mut found = false;
    for row in 0..i32::from(term.rows()) {
        if term
            .get_line_text(row, None)
            .unwrap_or_default()
            .contains("FINAL-SENTINEL-LINE")
        {
            found = true;
        }
    }
    assert!(
        found,
        "the recent sentinel is reconstructable after a 4x-budget flood"
    );
    // Every chain but the newest ran at least the byte interval, so the budget holds at most
    // one more keyframe than it holds intervals.
    let interval = (r.budget / 4).max(MIN_REKEY_BYTES);
    assert!(
        (2..=r.budget / interval + 1).contains(&r.keyframe_count()),
        "the readers keyed fresh chains, at most {} kept: {}",
        r.budget / interval + 1,
        r.keyframe_count()
    );
    assert_eq!(
        r.oldest_replayable().map(|kf| kf.seq),
        r.keyframes.front().map(|kf| kf.seq),
        "every kept keyframe replays"
    );
    assert!(r.used <= r.budget, "used={} past the budget", r.used);
}

fn assert_retention_holds(r: &TemporalRecorder) {
    assert!(
        r.used <= r.budget,
        "used={} past the budget {}",
        r.used,
        r.budget
    );
    assert_eq!(
        r.dropped_events(),
        r.total_events() - r.live_events() as u64
    );
    assert!(
        r.blobs.len() <= r.live_events(),
        "a payload outlived its event"
    );
    let floor = r.log.oldest_live().map_or(Seq(0), |e| e.seq);
    assert!(
        r.keyframes.iter().all(|kf| kf.seq >= floor),
        "a retained keyframe outlived its event"
    );
}

#[test]
fn replay_survives_more_than_twice_the_ring_of_small_events() {
    const RING: usize = aterm_buffer::MAX_LOG_EVENTS;
    const PROBE: usize = 16_384;
    const {
        assert!(
            3 * RING * 8 < DEFAULT_BUDGET_BYTES / 4,
            "REACH: the byte trigger never fires"
        );
    }
    for events_trigger in [true, false] {
        let mut rig = ReaderRig::new(Terminal::new(6, 20), DEFAULT_BUDGET_BYTES);
        if !events_trigger {
            rig.r
                .spine_watermark()
                .disable_trigger(RekeyTrigger::Events);
        }
        let mut lost_at = None;
        for i in 0..3 * RING {
            rig.slice(format!("{i:06}\r\n").as_bytes(), 0);
            if i % PROBE == PROBE / 2 {
                let Some(replayed) = rig.r.replay_at(None) else {
                    lost_at = Some(i);
                    break;
                };
                assert_eq!(screen_of(&replayed), screen_of(&rig.live), "after {i}");
            }
        }
        let r = &rig.r;
        assert_retention_holds(r);
        assert!(r.dropped_events() > 0, "REACH: the ring wrapped");
        if events_trigger {
            assert_eq!(lost_at, None, "replay lost");
            assert!(r.reachable_from().is_some());
            assert!(
                rig.rekeys > 0,
                "REACH: the writer asked the reader for rekeys"
            );
        } else {
            assert!(
                lost_at.is_some_and(|i| (RING - 1..RING - 1 + PROBE).contains(&i)),
                "lost at the first probe past the seed's eviction: {lost_at:?}"
            );
            assert_eq!(r.keyframe_count(), 0, "the seed went with its event");
            assert_eq!(r.reachable_from(), None);
        }
    }
}

#[test]
fn replay_survives_more_than_twice_the_ring_under_a_small_budget() {
    const RING: usize = aterm_buffer::MAX_LOG_EVENTS;
    let mut rig = ReaderRig::new(Terminal::new(6, 40), 1024 * 1024);
    let pad = "x".repeat(31);
    for i in 0..2 * RING + 8192 {
        let line = format!("{i:06} {pad}\r\n");
        assert_eq!(line.len(), 40);
        rig.slice(line.as_bytes(), 0);
        if i % 16_384 == 8191 {
            let replayed = rig
                .r
                .replay_at(None)
                .unwrap_or_else(|| panic!("replay lost after event {i}"));
            assert_eq!(screen_of(&replayed), screen_of(&rig.live), "after {i}");
            assert_retention_holds(&rig.r);
        }
    }
    assert!(rig.rekeys > 0, "REACH: the readers keyed the recording");
    let r = &rig.r;
    assert_retention_holds(r);
    assert!(r.dropped_events() > 0, "REACH: the ring wrapped");
    assert!(
        r.blobs.len() < r.live_events(),
        "REACH: the budget evicted blobs of live events"
    );
    assert!(r.reachable_from().is_some());
}

#[test]
fn budget_pressure_rekeys_before_the_newest_chain_is_evicted() {
    const COLS: u16 = 200;
    let line = |i: usize| format!("{i:06} {}\r\n", "y".repeat(usize::from(COLS) - 8));
    let seeded = || {
        let mut t = Terminal::new(24, COLS);
        t.set_scrollback_line_limit(Some(REKEY_SCROLLBACK_LINES));
        for i in 0..REKEY_SCROLLBACK_LINES + 24 {
            t.process(line(i).as_bytes());
        }
        t
    };
    let keyframe = ReplayKeyframe::from(seeded().checkpoint());
    let charge = keyframe.charge();
    let budget = charge * 5 / 4;
    let interval = (budget / 4).max(MIN_REKEY_BYTES);
    assert!(
        budget - charge < interval,
        "REACH: the chain has less room than the byte interval (charge {charge})"
    );
    assert!(
        charge + MIN_REKEY_BYTES + 2 * line(0).len() < budget,
        "REACH: under pressure the chain still fits (charge {charge})"
    );
    for pressure in [true, false] {
        let mut r = TemporalRecorder::with_budget(budget);
        r.record_keyframe(keyframe.clone());
        let mut rig = ReaderRig::with(seeded(), r);
        assert_eq!(
            rig.r.spine.base_charge.load(Ordering::Acquire),
            charge,
            "the pressure trigger counts from the seed's charge"
        );
        if !pressure {
            rig.r
                .spine_watermark()
                .disable_trigger(RekeyTrigger::Pressure);
        }
        let mut lost_at = None;
        for i in 0..3 * budget / line(0).len() {
            rig.slice(line(REKEY_SCROLLBACK_LINES + 24 + i).as_bytes(), 0);
            if rig.r.reachable_from().is_none() {
                lost_at = Some(i);
                break;
            }
        }
        let r = &rig.r;
        assert!(r.used <= r.budget);
        if pressure {
            assert_eq!(lost_at, None, "the newest chain was evicted");
            assert!(rig.rekeys > 3, "REACH: pressure re-keyframed");
            let replayed = r.replay_at(None).expect("replay");
            assert_eq!(screen_of(&replayed), screen_of(&rig.live));
        } else {
            assert!(lost_at.is_some(), "the chain survived without pressure");
            assert!(r.replay_at(None).is_none());
        }
    }
}

#[test]
fn the_budget_loses_the_oldest_window_first_and_says_where_replay_starts() {
    // Each slice is charged what a keyframe is, so the budget holds five of either.
    let blank = Terminal::new(4, 10).checkpoint();
    let c = ReplayKeyframe::from(blank.clone()).charge();
    assert!(6 * c < MIN_REKEY_BYTES, "REACH: no re-keyframe is due");
    let oldest = |r: &TemporalRecorder| r.oldest_replayable().map(|kf| kf.id);
    let mut r = TemporalRecorder::with_budget(5 * c + c / 2);
    r.record_keyframe(blank.clone());
    r.record_raw_in(&vec![b'a'; c]);
    r.record_keyframe(blank.clone());
    r.record_raw_in(&vec![b'b'; c]);
    r.record_keyframe(blank.clone());
    assert_eq!(oldest(&r), Some(KeyframeId(0)), "everything fits");
    assert_eq!(r.reachable_from(), Some(r.keyframes[0].ts));

    r.record_raw_in(&vec![b'c'; c]);
    assert_eq!(r.keyframe_count(), 2, "the oldest keyframe went");
    assert_eq!(oldest(&r), Some(KeyframeId(1)), "and no chain lost a blob");
    assert_eq!(r.reachable_from(), Some(r.keyframes[0].ts));
    assert!(r.replay_at(None).is_some());
    assert!(r.used <= r.budget);

    r.record_raw_in(&vec![b'd'; 5 * c]);
    assert_eq!(
        r.keyframe_count(),
        0,
        "the last resort took the newest window"
    );
    assert_eq!(r.reachable_from(), None);
    assert!(r.replay_at(None).is_none());
    assert!(r.used <= r.budget);
}

/// Review fix #50 (R1): once the oldest keyframe is evicted, the track is Lost from the oldest tick the log still names up to the keyframe a view now replays from.
#[test]
fn the_track_is_lost_up_to_the_oldest_replayable_keyframe_once_the_first_is_evicted() {
    let mut rig = ReaderRig::new(Terminal::new(6, 20), 4 * MIN_REKEY_BYTES);
    let line = vec![b'x'; MIN_REKEY_BYTES / 8];
    let lost = (0..10_000).find_map(|_| {
        rig.slice(&line, 0);
        let track = rig.r.rewind_track();
        track
            .spans
            .first()
            .copied()
            .filter(|(.., kind)| *kind == Span::Lost)
            .map(|span| (track, span))
    });
    let (track, (from, to, _)) = lost.expect("the first keyframe is evicted");
    assert!(rig.rekeys > 0, "REACH: the readers keyed the recording");
    let r = &rig.r;
    assert_eq!(from, track.from);
    assert_eq!(Some(to), r.oldest_replayable().map(|kf| kf.ts));
    assert!(to > from, "a stretch, not a point");
    assert!(
        r.rewind_instants().first().is_some_and(|i| i.at >= to),
        "no instant before the stretch's end replays"
    );
}

/// A due rekey waits for the reader's slice that ends at parser ground: a slice that leaves a CSI or a tmux passthrough wrapper open sends no witness and no keyframe, nor does any slice or resize inside it, and the slice that closes it keys the chain, which then replays faithfully. The rekey is due by bytes, or because the writer asked for it.
#[test]
fn a_due_rekey_waits_for_the_reader_at_parser_ground() {
    /// What opens the sequence, the slices inside it, and what closes it.
    type Case<'a> = (&'a [u8], &'a [&'a [u8]], &'a [u8]);
    let cases: [Case<'_>; 2] = [
        (b"\x1b[1;3", &[b"2", b";"], b"1m"),
        (b"\x1bPtmux;", &[b"abc", b"def"], b"\x1b\\"),
    ];
    for ((open, inside, close), by_bytes) in cases
        .into_iter()
        .flat_map(|case| [(case, true), (case, false)])
    {
        let label = format!("{} by_bytes={by_bytes}", String::from_utf8_lossy(open));
        let mut rig = ReaderRig::new(Terminal::new(6, 40), 4 * MIN_REKEY_BYTES);
        let mut first = if by_bytes {
            format!("{}\r\n", "x".repeat(30))
                .repeat(MIN_REKEY_BYTES / 32 + 1)
                .into_bytes()
        } else {
            rig.want_rekey();
            b"prompt$ ".to_vec()
        };
        first.extend_from_slice(open);
        assert_eq!(rig.slice(&first, 0), ["in"], "{label}: deferred inside it");
        assert!(!rig.live.parser_is_ground());
        assert!(rig.r.spine.rekey_due(), "{label}: the rekey stays due");
        if !by_bytes {
            rig.live.resize(6, 41);
        }
        for bytes in inside {
            let names = rig.slice(bytes, 0);
            assert_eq!(
                names.last().map(String::as_str),
                Some("in"),
                "{label}: {names:?}"
            );
            assert!(!rig.live.parser_is_ground(), "{label}: still inside it");
        }
        assert_eq!(
            rig.slice(close, 0),
            ["in", "witness", "rekeyframe"],
            "{label}: keyed once it ended"
        );
        assert!(rig.live.parser_is_ground());
        assert_eq!(rig.follows(), 1, "{label}");
        rig.assert_faithful(&label);
    }
}

/// A split UTF-8 character reads as parser ground, so the reader keys the chain at it: the keyframe's parser carry holds the partial character, and the replay that completes it from there is the live screen.
#[test]
fn nonground_rekey_partial_utf8_still_keys_and_preserves_its_carry() {
    let mut rig = ReaderRig::new(Terminal::new(6, 40), 4 * MIN_REKEY_BYTES);
    let mut first = vec![b'x'; MIN_REKEY_BYTES];
    first.push(0xf0);
    assert_eq!(
        rig.slice(&first, 0),
        ["in", "witness", "rekeyframe"],
        "keyed at the split character"
    );
    assert!(rig.live.parser_is_ground());
    let keyed = rig.r.keyframes.back().expect("the rekey");
    assert!(keyed.follows);
    assert_eq!(
        keyed
            .checkpoint
            .parser
            .as_ref()
            .map(|p| p.utf8_tail.clone()),
        Some(vec![0xf0]),
        "the keyframe's parser carry holds the partial character"
    );
    for bytes in [b"\x9f".as_slice(), b"\x98", b"\x80"] {
        assert_eq!(rig.slice(bytes, 0), ["in"]);
        assert!(rig.live.parser_is_ground());
    }
    assert_eq!(rig.follows(), 1);
    rig.assert_faithful("the completed character");
    for chain in rig.r.pane_chains().expect("chains") {
        assert_eq!(
            chain
                .check(aterm_aos_pane::PaneCode::ThisBuild, "test")
                .verdict,
            aterm_aos::contract::Verdict::Pass
        );
    }
}

fn boundary_rekey_bulk(start: usize, count: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    for row in start..start + count {
        bytes.extend_from_slice(format!(
            "\x1b[3{}mrow-{row:05} é界 abcdefghijklmnopqrstuvwxyz abcdefghijklmnopqrstuvwxyz\x1b[m\r\n",
            row % 7
        ).as_bytes());
    }
    bytes
}

/// History the seed kept whole, a resize and alternate-screen switches survive the readers' rekeys: after each the replay to the spine's end is the live screen and every witness judges faithful, and so on through unscrolls, screen switches and resizes past the last.
#[test]
fn boundary_rekey_preserves_initial_history_and_observed_resize_alt_transitions() {
    let mut live = Terminal::new(20, 80);
    live.set_scrollback_line_limit(None);
    live.process_at(&boundary_rekey_bulk(0, 4000), clock(0));
    let mut rig = ReaderRig::new(live, 64 * 1024 * 1024);
    rig.slice_keyed(&b"\x1b[20+T".repeat(150), 1000);
    rig.assert_faithful("after unscrolling the seed's history");
    for chunk in 0..40 {
        let bulk = boundary_rekey_bulk(4000 + chunk * 100, 100);
        let ms = 2000 + chunk as u64 * 100;
        if chunk == 39 {
            rig.slice_keyed(&bulk, ms);
        } else {
            rig.slice(&bulk, ms);
        }
    }
    rig.assert_faithful("after more history");
    rig.live.resize(20, 53);
    rig.slice(b"resized\r\n", 7000);
    rig.slice_keyed(b"\x1b[?1049halt-screen", 7500);
    rig.assert_faithful("on the alternate screen after a resize");
    rig.slice_keyed(b"\x1b[?1049lmain-again\r\n", 8000);
    rig.assert_faithful("back on the main screen");
    for (i, bytes) in [
        b"\x1b[20+".as_slice(),
        b"T",
        b"\x1b[?1049halt\r\n",
        b"\x1b[?1049lresume\r\n",
    ]
    .iter()
    .enumerate()
    {
        rig.slice(bytes, 9000 + i as u64 * 1000);
    }
    for (rows, cols) in [(30, 53), (10, 80), (20, 80)] {
        rig.live.resize(rows, cols);
        rig.slice(b"~", 14_000);
    }
    rig.assert_faithful("past the last rekey");
}

/// A CSI split across slices defers the reader's rekey to the slice that ends it, the keyed chain replays under the source's scrollback limit, and a witness of another screen judges diverged and asks for a live keyframe.
#[test]
fn boundary_rekey_keeps_split_parser_state_and_source_limits_and_rejects_bad_witness() {
    for limit in [None, Some(80)] {
        let config = Arc::new(TerminalConfig {
            scrollback_limit: limit,
            ..TerminalConfig::default()
        });
        let mut live = Terminal::new(20, 80);
        let _ = live.apply_config(&config);
        let mut r = TemporalRecorder::with_budget(64 * 1024 * 1024);
        r.set_replay_config(Some(config));
        r.record_keyframe(keyframe_of(&live, usize::MAX).unwrap());
        let mut rig = ReaderRig::with(live, r);
        let mut first = boundary_rekey_bulk(0, 4000);
        first.extend_from_slice(b"\x1b[20+");
        rig.want_rekey();
        assert_eq!(rig.slice(&first, 1000), ["in"], "{limit:?}: inside the CSI");
        assert!(!rig.live.parser_is_ground());
        assert_eq!(
            rig.slice(b"T", 2000),
            ["in", "witness", "rekeyframe"],
            "{limit:?}: keyed once the CSI ended"
        );
        let replayed = rig.r.replay_at(None).expect("replay");
        assert_eq!(replayed.scrollback_line_limit(), limit);
        rig.assert_faithful(&format!("{limit:?}"));
        let stranger = Terminal::new(20, 80).checkpoint_visible().unwrap();
        rig.r.apply(TemporalMsg::Witness(Arc::new(stranger)));
        let counts = rig.judge_all();
        assert_eq!(
            (counts.judged, counts.diverged, counts.pending),
            (1, 1, 0),
            "{limit:?}: a witness of another screen diverges"
        );
        assert!(
            rig.r.spine.keyframe_wanted(),
            "{limit:?}: and asks the reader for a live keyframe"
        );
    }
}

/// A sixel on either grid survives a rekey of a chain whose seed kept more history than a rekey keeps: the keyed chain replays the live screen, before and after a screen switch.
#[test]
fn boundary_rekey_keeps_image_bearing_bases_on_either_grid() {
    for alternate in [false, true] {
        let mut live = Terminal::new(20, 80);
        live.set_scrollback_line_limit(None);
        live.set_cell_pixel_size(10, 20);
        live.process_at(&boundary_rekey_bulk(0, 2750), clock(100));
        live.process_at(b"\x1bP0;0;0q\"1;1;10;20#1;2;100;0;0~\x1b\\", clock(200));
        if alternate {
            live.process_at(b"\x1b[?1049h", clock(300));
        }
        assert!(live.has_image_state());
        let mut rig = ReaderRig::new(live, 64 * 1024 * 1024);
        rig.slice_keyed(&boundary_rekey_bulk(2750, 4000), 1000);
        assert!(rig.live.main_grid().scrollback_lines() > REKEY_SCROLLBACK_LINES);
        assert!(rig.live.has_image_state());
        rig.assert_faithful(&format!("alternate={alternate}"));
        let switch: &[u8] = if alternate {
            b"\x1b[?1049lmain\r\n"
        } else {
            b"\x1b[?1049halt\r\n"
        };
        rig.slice(switch, 2000);
        rig.assert_faithful(&format!("alternate={alternate}, switched"));
    }
}

/// A kitty image a reader's rekey carries in its history stays referenced in the keyed chain's replay once more output scrolls it past the history a rekey keeps: deleted from the screen and placed again by its id, the replay shows it where the live screen does.
#[test]
fn boundary_rekey_keeps_kitty_history_references_for_later_delete_and_place() {
    let mut live = aterm_core::terminal::TerminalBuilder::new()
        .size(20, 80)
        .tiered_scrollback_defaults()
        .build();
    live.set_scrollback_line_limit(None);
    live.set_cell_pixel_size(10, 20);
    let mut rig = ReaderRig::new(live, 64 * 1024 * 1024);
    let rows = |range: std::ops::Range<usize>| {
        let mut bytes = Vec::new();
        for row in range {
            if row == 500 {
                bytes.extend_from_slice(
                    b"\x1b_Ga=T,f=32,s=1,v=1,i=7,c=1,r=1,q=2;/wAA/w==\x1b\\\r\n",
                );
            }
            bytes.extend_from_slice(format!("row-{row:04}\r\n").as_bytes());
        }
        bytes
    };
    rig.slice_keyed(&rows(0..1000), 1000);
    rig.slice(&rows(1000..2750), 1500);
    let placed = |term: &Terminal| {
        term.grid()
            .history_any_image(&|span| span.kitty.is_some_and(|tag| tag.image_id == 7))
    };
    assert!(placed(&rig.live));
    assert!(
        rig.live
            .grid()
            .capture_image_carry(usize::MAX)
            .history_rev
            .iter()
            .any(|(rev, spans)| {
                usize::try_from(*rev).is_ok_and(|rev| rev >= REKEY_SCROLLBACK_LINES)
                    && spans
                        .iter()
                        .any(|span| span.kitty.is_some_and(|tag| tag.image_id == 7))
            }),
        "image 7 is placed older than the history a rekey keeps"
    );
    rig.slice(b"\x1b_Ga=d,d=A,q=2\x1b\\", 2000);
    rig.slice(b"\x1b_Ga=p,i=7,c=1,r=1,q=2\x1b\\", 3000);
    assert!(!rig.live.grid().capture_image_carry(0).visible.is_empty());
    let replayed = rig.r.replay_at(None).expect("replay");
    assert_eq!(
        replayed.grid().capture_image_carry(0).visible,
        rig.live.grid().capture_image_carry(0).visible,
        "the image is placed again in the replay"
    );
    rig.assert_faithful("placed again after the rekey");
}

fn alt_frame(rows: u16, cols: u16) -> Vec<u8> {
    let mut f = String::from("\x1b[?1049h\x1b[2J");
    for r in 0..rows {
        f.push_str(&format!(
            "\x1b[{row};1HL{r}\x1b[{row};{cols}HE",
            row = r + 1
        ));
    }
    f.push_str(&format!("\x1b[{};3H", rows - 2));
    f.into_bytes()
}

fn drain_named(rx: &TemporalRx, r: &mut TemporalRecorder) -> Vec<String> {
    let mut out = Vec::new();
    while let Some(msg) = rx.try_recv() {
        out.push(match &msg {
            TemporalMsg::RawIn(_) => "in".to_string(),
            other => shape(other),
        });
        r.apply(msg);
    }
    out
}

fn shape(msg: &TemporalMsg) -> String {
    match msg {
        TemporalMsg::RawIn(input) => format!("in:{}", String::from_utf8_lossy(input.bytes())),
        TemporalMsg::Reply => "reply".to_string(),
        TemporalMsg::Resize { rows, cols } => format!("r:{rows}x{cols}"),
        TemporalMsg::CellPx { width, height } => format!("px:{width}x{height}"),
        TemporalMsg::Host(op, _) => format!("host:{op:?}"),
        TemporalMsg::ResizesLost(n) => format!("lost:{n}"),
        TemporalMsg::Keyframe(_) => "keyframe".to_string(),
        TemporalMsg::LiveKeyframe(..) => "live-keyframe".to_string(),
        TemporalMsg::Rekeyframe(_) => "rekeyframe".to_string(),
        TemporalMsg::Witness(_) => "witness".to_string(),
    }
}

/// A clock `ms` past one epoch every test shares, so a slice's time decides no witness unless a test means it to.
fn clock(ms: u64) -> ClockReading {
    static EPOCH: std::sync::OnceLock<ClockReading> = std::sync::OnceLock::new();
    let epoch = *EPOCH.get_or_init(ClockReading::now);
    ClockReading {
        monotonic: epoch.monotonic + std::time::Duration::from_millis(ms),
        wall_ms: Some(1_800_000_000_000 + ms),
    }
}

fn slice(live: &mut Terminal, mark: &mut SpineMark, tx: &TemporalTx, bytes: &[u8]) {
    slice_at(live, mark, tx, bytes, clock(0));
}

fn slice_at(
    live: &mut Terminal,
    mark: &mut SpineMark,
    tx: &TemporalTx,
    bytes: &[u8],
    at: ClockReading,
) {
    let pre = SpinePre::take(live);
    live.process_at(bytes, at);
    mark.record(tx, pre, live, InputSlice::at(bytes, at));
}

/// A bounded view fold past a narrowing that rewraps every line reads an honest witness as the live screen, though the history it dropped renumbered the shell's marks after it, and still differs from a screen that was never shown; the whole fold judges the same.
#[test]
fn a_bounded_fold_reads_faithful_over_renumbered_shell_marks() {
    const ROWS: u16 = 24;
    for poisoned in [false, true] {
        let mut live = aterm_core::terminal::TerminalBuilder::new()
            .size(ROWS, 80)
            .tiered_scrollback_defaults()
            .build();
        live.set_scrollback_line_limit(None);
        let mut r = seeded(&live);
        let (tx, rx) = queue(64);
        let mut mark = SpineMark::attach(r.spine_watermark());
        for chunk in 0..80 {
            let mut out = String::from("\x1b]133;A\x07$ \x1b]133;B\x07make\r\n\x1b]133;C\x07");
            for n in 0..60 {
                out.push_str(&format!(
                    "chunk {chunk:02} line {n:02} {}\r\n",
                    "x".repeat(60)
                ));
            }
            out.push_str("\x1b]133;D;0\x07");
            slice(&mut live, &mut mark, &tx, out.as_bytes());
            drain(&rx, &mut r);
        }
        live.resize(ROWS, 60);
        slice(&mut live, &mut mark, &tx, b"\x1b]133;A\x07$ \x1b]133;B\x07");
        drain(&rx, &mut r);
        let seen = if poisoned {
            let mut stranger = Terminal::new(ROWS, 60);
            stranger.process(b"not what was on screen");
            stranger.checkpoint_visible()
        } else {
            live.checkpoint_visible()
        };
        r.apply(TemporalMsg::Witness(Arc::new(seen.expect("visible"))));
        let plan = r.replay_plan(None).expect("plan");
        let mut bounded = plan.restore_bounded(crate::rewind_worker::STORE_LINES);
        let (differs, _) = plan.fold_onto(&mut bounded, plan.base_seq(), |_, _| true);
        assert_eq!(differs, poisoned, "the bounded fold, poisoned={poisoned}");
        let matched: Vec<bool> = plan.fold().judged.0.iter().map(|&(_, m)| m).collect();
        assert_eq!(
            matched.split_last(),
            Some((&!poisoned, &vec![true; matched.len().saturating_sub(1)][..])),
            "the whole fold's verdicts, the reader's own witnesses first"
        );
    }
}

fn seeded(live: &Terminal) -> TemporalRecorder {
    let mut r = TemporalRecorder::new();
    r.record_keyframe(keyframe_of(live, usize::MAX).expect("seedable"));
    r.seed_spine(SpinePre::take(live));
    r
}

fn drain(rx: &TemporalRx, r: &mut TemporalRecorder) -> usize {
    let mut n = 0;
    while let Some(msg) = rx.try_recv() {
        r.apply(msg);
        n += 1;
    }
    n
}

/// One pane's reader and the writer of its recording: the live engine, the reader's mark and queue, and the recorder the queue's messages are applied to in the writer's way ([`TemporalRecorder::apply_in`]).
struct ReaderRig {
    live: Terminal,
    r: TemporalRecorder,
    mark: SpineMark,
    tx: TemporalTx,
    rx: TemporalRx,
    /// The readers' rekeys the writer took off the queue ([`TemporalMsg::Rekeyframe`]), a stale one included.
    rekeys: usize,
}

impl ReaderRig {
    /// A rig on `live` whose recorder of `budget` is seeded with a keyframe of it.
    fn new(live: Terminal, budget: usize) -> Self {
        let mut r = TemporalRecorder::with_budget(budget);
        r.record_keyframe(keyframe_of(&live, usize::MAX).expect("seedable"));
        Self::with(live, r)
    }

    /// A rig on `live` and `r`, whose newest keyframe the caller recorded of `live`.
    fn with(live: Terminal, r: TemporalRecorder) -> Self {
        r.seed_spine(SpinePre::take(&live));
        let (tx, rx) = queue_on(r.spine_watermark(), 256);
        let mark = SpineMark::attach(r.spine_watermark());
        Self {
            live,
            r,
            mark,
            tx,
            rx,
            rekeys: 0,
        }
    }

    /// Parse `bytes` live `ms` past the tests' epoch ([`clock`]) and queue the slice and its reply as the reader records them ([`SpineMark::record`]), applying nothing.
    fn queue(&mut self, bytes: &[u8], ms: u64) {
        let at = clock(ms);
        let pre = SpinePre::take(&self.live);
        self.live.process_at(bytes, at);
        let reply = self.live.take_response().map(Arc::<[u8]>::from);
        self.mark.record(
            &self.tx,
            pre,
            &self.live,
            InputSlice::at(bytes, at).replying(reply),
        );
    }

    /// [`Self::queue`], then [`Self::drain`].
    fn slice(&mut self, bytes: &[u8], ms: u64) -> Vec<String> {
        self.queue(bytes, ms);
        self.drain()
    }

    /// Apply the next queued message, named as [`shape`] names it (`in` for a slice).
    fn apply_next(&mut self) -> Option<String> {
        let (generation, msg) = self.rx.try_recv_stamped()?;
        let name = match &msg {
            TemporalMsg::RawIn(_) => "in".to_string(),
            TemporalMsg::Rekeyframe(_) => {
                self.rekeys += 1;
                shape(&msg)
            }
            other => shape(other),
        };
        self.r.apply_in(generation, msg);
        Some(name)
    }

    /// Apply every queued message, in order, naming each ([`Self::apply_next`]).
    fn drain(&mut self) -> Vec<String> {
        std::iter::from_fn(|| self.apply_next()).collect()
    }

    /// [`judge_all`] on the recorder.
    fn judge_all(&mut self) -> JudgeCounts {
        let shared = Mutex::new(std::mem::take(&mut self.r));
        let counts = judge_all(&shared);
        self.r = shared.into_inner().unwrap_or_else(PoisonError::into_inner);
        counts
    }

    /// Ask the readers for a rekey as the writer does ([`TemporalRecorder::after_op`]).
    fn want_rekey(&self) {
        self.r.spine.rekey_wanted.store(true, Ordering::SeqCst);
    }

    /// [`Self::slice`] with a rekey asked for first, which the reader sends right after the slice.
    fn slice_keyed(&mut self, bytes: &[u8], ms: u64) {
        self.want_rekey();
        let names = self.slice(bytes, ms);
        assert_eq!(
            names.last().map(String::as_str),
            Some("rekeyframe"),
            "the reader keys the chain after the slice: {names:?}"
        );
    }

    /// The kept keyframes a reader's rekey recorded ([`RetainedKeyframe::follows`]).
    fn follows(&self) -> usize {
        self.r.keyframes.iter().filter(|kf| kf.follows).count()
    }

    /// The replay to the spine's end is the live screen, and every kept witness judges faithful.
    fn assert_faithful(&mut self, label: &str) {
        let replayed = self.r.replay_at(None).expect("the spine's end replays");
        assert_eq!(
            replayed.checkpoint_visible(),
            self.live.checkpoint_visible(),
            "{label}: the replay is the live screen"
        );
        let counts = self.judge_all();
        assert_eq!(
            (counts.diverged, counts.pending),
            (0, 0),
            "{label}: every witness judged faithful"
        );
    }
}

/// The reader's rekey follows the slice it keyed after, in the same hold: the slice, the witness that closes the chain, then the keyframe, which stands in for no slice. It adds no Rewind instant and no exported screen, cuts no chain, and the replay to the spine's end is the live screen.
#[test]
fn a_reader_rekey_follows_the_slice_and_cuts_nothing() {
    let mut rig = ReaderRig::new(Terminal::new(6, 30), 256 * 1024);
    assert_eq!(rig.slice(b"before\r\n", 0), ["in"]);
    let instants = rig.r.rewind_instants().len();
    let exported = rig.r.export().expect("export").events.len();
    rig.queue(&vec![b'.'; MIN_REKEY_BYTES], 0);
    let names = rig.drain();
    let keyed = rig.r.keyframes.back().expect("the rekey");
    let (at, follows, cut) = (keyed.ts, keyed.follows, keyed.cut);
    let now = rig.r.rewind_instants();
    assert_eq!(
        now.len(),
        instants + 1,
        "the slice is the one new instant: {now:?}"
    );
    assert!(now.iter().all(|i| i.at != at), "no instant at the rekey");
    let export = rig.r.export().expect("export");
    assert_eq!(
        export.events.len(),
        exported + 1,
        "the export writes the slice and no screen for the rekey"
    );
    assert!(
        export
            .events
            .iter()
            .all(|(_, e)| !matches!(e, Exported::Screen(_)))
    );
    assert_eq!(
        names,
        ["in", "witness", "rekeyframe"],
        "one hold queued the slice, the closing witness and the rekey"
    );
    assert!(follows, "the rekey follows its slice");
    assert_eq!(cut, None);
    let chains = rig.r.pane_chains().expect("chains");
    assert_eq!(chains.len(), 2);
    for chain in &chains {
        assert_eq!(
            chain
                .check(aterm_aos_pane::PaneCode::ThisBuild, "test")
                .verdict,
            aterm_aos::contract::Verdict::Pass
        );
    }
    rig.assert_faithful("across the rekey");
}

/// A due rekey the queue has no room for is refused without a trace: no witness and no keyframe are sent, no break opens, no keyframe stays counted in flight, and the rekey stays due, so the reader sends it once the writer has caught up: on its next slice, or on the next host flush when the queue took the closing witness but not the keyframe.
#[test]
fn a_refused_rekey_stays_due_and_opens_no_break() {
    let fresh = |cap: usize, byte_cap: usize| {
        let mut rig = ReaderRig::new(Terminal::new(6, 30), 256 * 1024);
        let (tx, rx) = queue_in(
            rig.r.spine_watermark(),
            cap,
            byte_cap,
            RewindPool::new(usize::MAX),
        );
        (rig.tx, rig.rx) = (tx, rx);
        rig
    };
    let refused = |rig: &ReaderRig, label: &str| {
        let spine = &rig.r.spine;
        assert!(!spine.break_open(), "{label}: no break opened");
        assert_eq!(spine.dropped_input(), 0, "{label}");
        assert_eq!(
            spine.keyframes_queued.load(Ordering::SeqCst),
            0,
            "{label}: no keyframe counted in flight"
        );
        assert!(spine.rekey_due(), "{label}: the rekey stays due");
    };

    // The writer holds two 40 KiB slices of a 128 KiB queue: past half its bytes, no room to key.
    // Carriage returns leave the screen as it is, so once the writer catches up the keyframe and
    // a whole read fit the queue ([`TemporalTx::fits`]).
    let mut rig = fresh(64, 128 * 1024);
    let slice = vec![b'\r'; 40 * 1024];
    rig.queue(&slice, 0);
    rig.queue(&slice, 0);
    refused(&rig, "no room");
    assert_eq!(rig.drain(), ["in", "in"], "no witness and no keyframe");
    assert_eq!(rig.slice(b"x", 0), ["in", "witness", "rekeyframe"]);
    assert!(rig.r.reachable_from().is_some());
    rig.assert_faithful("no room");

    // Two places: the slice and the closing witness take them, and the keyframe is refused.
    let mut rig = fresh(2, usize::MAX);
    rig.queue(&vec![b'.'; MIN_REKEY_BYTES], 0);
    refused(&rig, "no place");
    assert_eq!(rig.drain(), ["in", "witness"]);
    rig.mark.flush_host(&rig.tx, &rig.live);
    assert_eq!(
        rig.drain(),
        ["rekeyframe"],
        "the flush keys the chain its witness closed"
    );
    assert!(rig.r.reachable_from().is_some());
    rig.assert_faithful("no place");
}

/// The pressure trigger keys a chain at half the room its keyframe leaves it under the budget, at least [`MIN_REKEY_BYTES`], so it keys sooner the heavier the keyframe; and never for a keyframe that leaves less than [`MIN_REKEY_BYTES`], which a fresh keyframe of the same charge could not keep a chain behind either.
#[test]
fn pressure_keys_halfway_to_eviction_and_never_for_a_keyframe_no_chain_fits_behind() {
    const MIB: usize = 1024 * 1024;
    let spine = SpineWatermark::keyed(8 * MIB);
    let due = |base: usize, chain: usize| {
        spine.base_charge.store(base, Ordering::Release);
        spine.rekey_bytes.store(chain, Ordering::Release);
        spine.rekey_due()
    };
    assert!(
        !due(MIB, MIB) && due(MIB, 2 * MIB),
        "the interval keys a light chain"
    );
    assert!(
        !due(5 * MIB, 3 * MIB / 2 - 1) && due(5 * MIB, 3 * MIB / 2),
        "3 MiB of room: at half of it"
    );
    assert!(
        !due(7 * MIB, MIB / 2 - 1) && due(7 * MIB, MIB / 2),
        "1 MiB of room: at half of it, not at MIN_REKEY_BYTES"
    );
    assert!(
        !due(8 * MIB - 3 * MIN_REKEY_BYTES / 2, MIN_REKEY_BYTES - 1)
            && due(8 * MIB - 3 * MIN_REKEY_BYTES / 2, MIN_REKEY_BYTES),
        "less room than two minimums: at the minimum"
    );
    assert!(
        !due(8 * MIB - MIN_REKEY_BYTES / 2, 2 * MIB - 1),
        "a keyframe no chain fits behind keys only at the interval"
    );
    assert!(due(8 * MIB - MIN_REKEY_BYTES / 2, 2 * MIB));
}

/// A keyframe the budget cannot keep is evicted as it lands. The writer asks for a rekey once for that loss, and a keyframe evicted as it lands asks for no other, so the readers key at their own cadence rather than after every slice.
#[test]
fn a_keyframe_the_budget_cannot_keep_is_asked_for_once() {
    let mut rig = ReaderRig::new(Terminal::new(24, 80), 2048);
    assert_eq!(
        rig.r.keyframe_count(),
        0,
        "fixture: the seed keyframe alone charges past the budget"
    );
    for n in 0..32 {
        let _ = rig.slice(format!("line {n}\r\n").as_bytes(), 0);
    }
    assert_eq!(
        rig.rekeys, 1,
        "one rekey for the loss, none for each keyframe evicted as it lands"
    );
    assert!(
        !rig.r.spine.rekey_wanted.load(Ordering::SeqCst),
        "no rekey is asked for"
    );
    assert_eq!(rig.r.spine.dropped_input(), 0);
}

/// A rekey's keyframe passes the queue's byte caps, so a rekey sends only while the queue keeps room for the keyframe it took and for the reader's next whole read ([`TemporalTx::fits`]). With the writer behind by half the queue, the keyframe is refused, the rekey stays due and the next whole read is queued; once the writer catches up, the next slice keys the chain.
#[test]
fn a_rekey_leaves_the_queue_room_for_the_next_slice() {
    let mut rig = ReaderRig::new(Terminal::new(6, 30), 8 * 1024 * 1024);
    let weight = capture_keyframe(&rig.live, REKEY_SCROLLBACK_LINES)
        .expect("keyable")
        .queue_weight();
    let seen = charge_of(&rig.live.checkpoint_visible().expect("visible"));
    assert!(
        seen < weight,
        "fixture: a witness weighs less than a keyframe"
    );
    let half = weight + SLICE_ROOM - 1;
    let (tx, rx) = queue_in(
        rig.r.spine_watermark(),
        1024,
        2 * half,
        RewindPool::new(usize::MAX),
    );
    (rig.tx, rig.rx) = (tx, rx);
    // Carriage returns leave the screen, and so the keyframe a rekey takes, as it is.
    let mut left = half;
    while left > 0 {
        let n = left.min(16 * 1024);
        if left == n {
            rig.r.spine.rekey_wanted.store(true, Ordering::SeqCst);
        }
        rig.queue(&vec![b'\r'; n], 0);
        left -= n;
    }
    assert!(rig.r.spine.rekey_due(), "the rekey stays due");
    rig.queue(&vec![b'\r'; SLICE_ROOM], 0);
    assert_eq!(
        rig.r.spine.dropped_input(),
        0,
        "the whole read after the refused rekey is queued"
    );
    assert!(!rig.r.spine.break_open(), "no break opened");
    let applied = rig.drain();
    assert!(
        !applied.iter().any(|m| m == "rekeyframe"),
        "no keyframe went ahead of the read: {applied:?}"
    );
    assert_eq!(rig.slice(b"x", 0), ["in", "witness", "rekeyframe"]);
    rig.assert_faithful("caught up");
}

/// A keyframe the queue has no room for beside the next read, with the writer behind, is refused, and the readers capture no other while the queue still has no room for one as heavy ([`SpineMark::room_expected`]); once the writer catches up, the next slice keys the screen.
#[test]
fn a_refused_rekey_captures_again_only_once_the_queue_has_room() {
    let mut live = Terminal::new(6, 30);
    for n in 0..3000 {
        live.process(format!("{n:029}\r\n").as_bytes());
    }
    let weight = capture_keyframe(&live, REKEY_SCROLLBACK_LINES)
        .expect("keyable")
        .queue_weight();
    let mut rig = ReaderRig::new(live, 8 * 1024 * 1024);
    // A queue that fits the keyframe beside a whole read, with 16 KiB to spare.
    let (tx, rx) = queue_in(
        rig.r.spine_watermark(),
        1024,
        weight + 2 * SLICE_ROOM + 16 * 1024,
        RewindPool::new(usize::MAX),
    );
    (rig.tx, rig.rx) = (tx, rx);
    // Past the read's room beside the keyframe, within has_room's half.
    for _ in 0..(SLICE_ROOM / (8 * 1024) + 3) {
        rig.queue(&[b'\r'; 8 * 1024], 0);
    }
    let before = capture_probe::count();
    rig.r.spine.rekey_wanted.store(true, Ordering::SeqCst);
    rig.queue(b"y", 0);
    assert_eq!(capture_probe::count() - before, 1, "one capture");
    assert_eq!(
        rig.r.spine.keyframes_queued.load(Ordering::SeqCst),
        0,
        "the keyframe was refused"
    );
    for _ in 0..4 {
        rig.queue(&[b'\r'; 1024], 0);
    }
    assert_eq!(
        capture_probe::count() - before,
        1,
        "no capture while the writer is still behind"
    );
    let _ = rig.drain();
    assert_eq!(rig.slice(b"z", 0), ["in", "witness", "rekeyframe"]);
    assert_eq!(capture_probe::count() - before, 2);
    assert_eq!(rig.r.spine.dropped_input(), 0);
    rig.assert_faithful("caught up");
}

/// A queue that fits a refused keyframe beside a read with only 8 KiB to spare: the weight the next rekey expects is the keyframe's own, so while the writer stays behind it captures nothing, and once the writer catches up it keys. An expected weight an eighth heavier would read as one no queue could fit and capture every slice.
#[test]
fn a_refused_rekey_near_the_queue_cap_neither_stalls_nor_captures_each_slice() {
    let mut live = Terminal::new(6, 30);
    for n in 0..3000 {
        live.process(format!("{n:029}\r\n").as_bytes());
    }
    let weight = capture_keyframe(&live, REKEY_SCROLLBACK_LINES)
        .expect("keyable")
        .queue_weight();
    assert!(
        weight / 8 > 8 * 1024,
        "fixture: an eighth is more than the spare room"
    );
    let mut rig = ReaderRig::new(live, 8 * 1024 * 1024);
    let (tx, rx) = queue_in(
        rig.r.spine_watermark(),
        1024,
        weight + SLICE_ROOM + 8 * 1024,
        RewindPool::new(usize::MAX),
    );
    (rig.tx, rig.rx) = (tx, rx);
    rig.queue(&[b'\r'; 16 * 1024], 0);
    let before = capture_probe::count();
    rig.r.spine.rekey_wanted.store(true, Ordering::SeqCst);
    rig.queue(b"y", 0);
    assert_eq!(capture_probe::count() - before, 1, "one capture");
    assert_eq!(
        rig.r.spine.keyframes_queued.load(Ordering::SeqCst),
        0,
        "the keyframe was refused"
    );
    for _ in 0..4 {
        rig.queue(&[b'\r'; 1024], 0);
    }
    assert_eq!(
        capture_probe::count() - before,
        1,
        "no capture while the writer is still behind"
    );
    let _ = rig.drain();
    assert_eq!(rig.slice(b"z", 0), ["in", "witness", "rekeyframe"]);
    assert_eq!(rig.r.spine.dropped_input(), 0);
    rig.assert_faithful("caught up");
}

/// The weight a rekey expects is only a prediction: one too heavy for any queue, left by a screen since cleared, does not stop a rekey, which captures the screen as it is, keys it, and expects that weight next.
#[test]
fn a_stale_heavy_prediction_does_not_stop_a_rekey() {
    let mut rig = ReaderRig::new(Terminal::new(6, 30), 8 * 1024 * 1024);
    let (tx, rx) = queue_in(
        rig.r.spine_watermark(),
        1024,
        MIN_QUEUE_BYTES,
        RewindPool::new(usize::MAX),
    );
    (rig.tx, rig.rx) = (tx, rx);
    rig.r
        .spine
        .keyframe_weight
        .store(1 << 40, Ordering::Release);
    rig.r.spine.rekey_wanted.store(true, Ordering::SeqCst);
    assert_eq!(rig.slice(b"y", 0), ["in", "witness", "rekeyframe"]);
    let light = capture_keyframe(&rig.live, REKEY_SCROLLBACK_LINES)
        .expect("keyable")
        .queue_weight();
    assert!(
        rig.r.spine.keyframe_weight.load(Ordering::Acquire) <= light,
        "the next rekey expects the light screen's weight"
    );
    rig.assert_faithful("keyed");
}

/// A keyframe that could not leave the next read room even were the writer caught up to the reader's own hold is sent whenever the queue has room: waiting would never make room for both, and keying the chain comes before the read.
#[test]
fn a_keyframe_no_queue_fits_beside_a_read_goes_when_the_queue_has_room() {
    let mut live = Terminal::new(6, 30);
    for n in 0..3000 {
        live.process(format!("{n:029}\r\n").as_bytes());
    }
    let weight = capture_keyframe(&live, REKEY_SCROLLBACK_LINES)
        .expect("keyable")
        .queue_weight();
    let mut rig = ReaderRig::new(live, 8 * 1024 * 1024);
    // No room for a read beside the keyframe even with nothing else queued.
    let (tx, rx) = queue_in(
        rig.r.spine_watermark(),
        1024,
        weight + SLICE_ROOM,
        RewindPool::new(usize::MAX),
    );
    (rig.tx, rig.rx) = (tx, rx);
    // The writer behind, within has_room's half.
    for _ in 0..4 {
        rig.queue(&[b'\r'; 8 * 1024], 0);
    }
    rig.r.spine.rekey_wanted.store(true, Ordering::SeqCst);
    rig.queue(b"y", 0);
    assert_eq!(
        rig.r.spine.keyframes_queued.load(Ordering::SeqCst),
        1,
        "the rekey's keyframe is queued"
    );
    assert!(rig.drain().iter().any(|m| m == "rekeyframe"));
    assert_eq!(rig.r.spine.dropped_input(), 0);
    rig.assert_faithful("keyed");
}

/// A keyframe's image payloads are the live engine's, shared rather than copied, so they weigh nothing on the queue ([`ReplayKeyframe::queue_weight`]): with the writer behind, a rekey of a screen whose image would not fit beside the next read is still queued.
#[test]
fn a_rekey_weighs_its_shared_images_at_nothing_on_the_queue() {
    let mut live = Terminal::new(6, 30);
    // A 256x128 RGBA image of zeros, 128 KiB, in 4096-character chunks.
    let payload = format!("{}AAA=", "AAAA".repeat(43_690));
    let chunks: Vec<&[u8]> = payload.as_bytes().chunks(4096).collect();
    for (i, chunk) in chunks.iter().enumerate() {
        let head = if i == 0 {
            "a=T,f=32,s=256,v=128,i=9,q=2,"
        } else {
            ""
        };
        let more = u8::from(i + 1 < chunks.len());
        live.process(format!("\x1b_G{head}m={more};").as_bytes());
        live.process(chunk);
        live.process(b"\x1b\\");
    }
    let keyframe = capture_keyframe(&live, REKEY_SCROLLBACK_LINES).expect("keyable");
    let (charge, weight) = (keyframe.charge(), keyframe.queue_weight());
    assert!(
        charge >= weight + 128 * 1024,
        "fixture: the image is in the keyframe's charge: {charge} {weight}"
    );
    let mut rig = ReaderRig::new(live, 8 * 1024 * 1024);
    let (tx, rx) = queue_in(
        rig.r.spine_watermark(),
        1024,
        charge + 2 * SLICE_ROOM + 16 * 1024,
        RewindPool::new(usize::MAX),
    );
    (rig.tx, rig.rx) = (tx, rx);
    for _ in 0..(SLICE_ROOM / (8 * 1024) + 3) {
        rig.queue(&[b'\r'; 8 * 1024], 0);
    }
    rig.r.spine.rekey_wanted.store(true, Ordering::SeqCst);
    rig.queue(b"y", 0);
    assert_eq!(
        rig.r.spine.keyframes_queued.load(Ordering::SeqCst),
        1,
        "the rekey's keyframe is queued"
    );
    assert!(rig.drain().iter().any(|m| m == "rekeyframe"));
    assert_eq!(rig.r.spine.dropped_input(), 0);
    rig.assert_faithful("imaged");
}

/// The writer takes back a rekey it asked for while a reader's keyframe is in flight. Should that keyframe not be sent after all, the writer asks for the loss again after its next op: it marks a loss asked for only once the ask stands.
#[test]
fn a_loss_is_asked_for_again_when_the_keyframe_in_flight_is_not_sent() {
    let mut rig = ReaderRig::new(Terminal::new(24, 80), 2048);
    assert_eq!(
        rig.r.keyframe_count(),
        0,
        "fixture: the seed keyframe alone charges past the budget"
    );
    let slice = || TemporalMsg::RawIn(InputSlice::at(b"x", ClockReading::now()));
    ask_probe::race_next();
    rig.r.apply(slice());
    assert_eq!(
        rig.r.spine.keyframes_queued.load(Ordering::SeqCst),
        1,
        "a reader counted a keyframe as the ask was stored"
    );
    assert!(
        !rig.r.spine.rekey_wanted.load(Ordering::SeqCst),
        "taken back while a keyframe is in flight"
    );
    rig.r.spine.unqueue_keyframe();
    rig.r.apply(slice());
    assert!(
        rig.r.spine.rekey_wanted.load(Ordering::SeqCst),
        "asked for again once none is"
    );
}

/// The replay a cell search reads the shown instant's chain on folds on to the witness that covers that instant, so the search passes that witness while it is still pending and finds the replay it differs from untrusted; with no covering witness it folds to the instant.
#[test]
fn a_searchs_last_chain_folds_on_to_its_covering_witness() {
    let mut live = Terminal::new(6, 30);
    let mut r = TemporalRecorder::new();
    r.record_keyframe(keyframe_of(&live, usize::MAX).expect("keyable"));
    let put = |r: &mut TemporalRecorder, live: &mut Terminal, bytes: &[u8]| {
        let at = ClockReading::now();
        live.process_at(bytes, at);
        r.apply(TemporalMsg::RawIn(InputSlice::at(bytes, at)));
        r.latest_tick()
    };
    let shown = put(&mut r, &mut live, b"\x1b[3;1Hone");
    live.process(b"\x1b[5;1Hunrecorded");
    while r.now() <= shown {}
    r.apply(TemporalMsg::Witness(Arc::new(
        live.checkpoint_visible().expect("visible"),
    )));
    let _ = put(&mut r, &mut live, b"\x1b[6;1Htwo");
    let differs = |plans: Vec<ReplayPlan>| {
        let last = plans.last().expect("the shown chain");
        let mut replay = last.restore_bounded(crate::rewind_worker::STORE_LINES);
        let mut seen = Vec::new();
        assert!(last.fold_before(&mut replay, |_, _| true, |_, d| seen.push(d)));
        seen
    };
    assert_eq!(
        differs(r.chain_plans(shown)),
        [true],
        "the fold passes the pending witness that covers the shown instant, which the replay differs from"
    );
    assert_eq!(r.pending(), 1, "and posts no verdict");
    assert_eq!(
        differs(r.chain_plans(r.latest_tick())),
        [true],
        "past the witness, the fold to the instant passes it too"
    );
}

/// The writer asks the readers for a rekey ([`SpineWatermark::rekey_wanted`]) once the newest chain ran [`REKEY_EVENTS`] events, lost its first slice, or no keyframe is kept, and never while a reader's keyframe is in flight: a slice the reader takes between a rekeyed slice's apply and its keyframe's sends no second keyframe.
#[test]
fn the_writer_asks_for_a_rekey_only_with_no_keyframe_in_flight() {
    let wanted = |r: &TemporalRecorder| r.spine.rekey_wanted.load(Ordering::SeqCst);

    // REKEY_EVENTS events past the newest keyframe, none in flight.
    let mut r = TemporalRecorder::new();
    r.record_keyframe(Terminal::new(4, 10).checkpoint());
    for n in 1..=REKEY_EVENTS {
        r.apply(TemporalMsg::Reply);
        assert_eq!(wanted(&r), n >= REKEY_EVENTS, "after {n} events");
    }

    // A rekey in flight as the chain reaches them.
    let mut rig = ReaderRig::new(Terminal::new(6, 30), 256 * 1024);
    for _ in 1..REKEY_EVENTS {
        rig.r.apply(TemporalMsg::Reply);
    }
    assert!(!wanted(&rig.r), "precondition: one event short");
    rig.queue(&vec![b'.'; MIN_REKEY_BYTES], 0);
    assert_eq!(rig.apply_next().as_deref(), Some("in"));
    assert_eq!(
        rig.r.spine.keyframes_queued.load(Ordering::SeqCst),
        1,
        "the rekey is in flight"
    );
    rig.queue(b"x", 0);
    let names = rig.drain();
    assert_eq!(
        rig.follows(),
        1,
        "the slice taken with the rekey in flight sent no second one"
    );
    assert_eq!(names, ["witness", "rekeyframe", "in"]);
    assert!(!wanted(&rig.r), "the applied rekey leaves nothing wanted");

    // The newest chain's first slice evicted.
    let mut r = TemporalRecorder::with_budget(4 * MIN_REKEY_BYTES);
    r.record_keyframe(Terminal::new(4, 10).checkpoint());
    let third = vec![b'.'; 4 * MIN_REKEY_BYTES / 3];
    for _ in 0..3 {
        r.apply(TemporalMsg::RawIn(InputSlice::at(&third, clock(0))));
    }
    assert_eq!(r.keyframe_count(), 1, "precondition: the keyframe is kept");
    assert!(
        r.keyframes[0].first_blob < r.blob_base(),
        "precondition: its first slice went"
    );
    assert!(
        wanted(&r),
        "a chain that lost its first slice is keyed afresh"
    );

    // No keyframe kept.
    let mut r = TemporalRecorder::new();
    r.record_keyframe(Terminal::new(4, 10).checkpoint());
    r.forget();
    assert!(!wanted(&r));
    r.apply(TemporalMsg::Reply);
    assert!(wanted(&r), "events with no keyframe kept");
}

/// The readers key a chain after the slice its rule names, counted from what the writer applies: the bytes of the slices since the chain's keyframe and the charges of the witnesses sent before deciding, against a quarter of the budget, or, under pressure, against half the room the keyframe's own charge leaves under the budget, at least [`MIN_REKEY_BYTES`]. Host steps count nothing. A 2,000,000-byte chain of 80-column lines at the default budget, its witnesses included, is never keyed: the Rewind step row's one chain.
#[test]
fn reader_rekey_accounting_keys_after_the_slice_its_rule_names() {
    const COLS: usize = 120;
    let text = |len: usize, n: usize| {
        let line = format!("{n:06} {}\r\n", "y".repeat(COLS - 9));
        line.repeat(len / line.len() + 1).into_bytes()[..len].to_vec()
    };
    let sizes = [3_000, 17_000, 40_000, 9_000, 1_000, 25_000, 60_000];
    let (mut by_bytes, mut by_pressure) = (0, 0);
    // Budgets from half again the seed's charge, which leaves a chain room for MIN_REKEY_BYTES and the largest slice, to ten times it.
    for scale in [6, 8, 16, 40] {
        let mut live = Terminal::new(24, COLS as u16);
        live.set_scrollback_line_limit(Some(REKEY_SCROLLBACK_LINES));
        live.process(&text(COLS * (REKEY_SCROLLBACK_LINES + 24), 0));
        let charge = keyframe_of(&live, usize::MAX).expect("keyable").charge();
        let budget = charge * scale / 4;
        let interval = (budget / 4).max(MIN_REKEY_BYTES);
        let mut rig = ReaderRig::new(live, budget);
        let mut base = rig.r.keyframes.back().expect("seed").charge();
        let mut chain = 0;
        let mut first_of_chain = true;
        for step in 0..140 {
            let ms = 1_100 * (step as u64 + 1);
            if step % 9 == 8 {
                rig.live.clear_scrollback();
                rig.mark.flush_host(&rig.tx, &rig.live);
                let names = rig.drain();
                assert!(
                    !names.iter().any(|n| n.contains("keyframe")),
                    "scale {scale}, step {step}: a host step counts nothing: {names:?}"
                );
                continue;
            }
            let len = sizes[step % sizes.len()];
            rig.queue(&text(len, step), ms);
            let (mut names, mut witness_charges) = (Vec::new(), Vec::new());
            while let Some(name) = rig.apply_next() {
                if name == "witness" {
                    let w = rig.r.witnesses.back().expect("the witness just applied");
                    witness_charges.push(w.snapshot_charge);
                }
                names.push(name);
            }
            chain += len;
            // Every slice but a chain's first comes a second past the last witness, so the reader witnessed it before deciding.
            let witnessed = !first_of_chain;
            if witnessed {
                assert_eq!(
                    names.get(1).map(String::as_str),
                    Some("witness"),
                    "scale {scale}, step {step}: {names:?}"
                );
                // With the chain's tail covered, the reader sends no witness of its own before a rekey: the first is the slice's.
                chain += witness_charges[0];
            }
            let bytes_due = chain >= interval;
            let room = budget.saturating_sub(base);
            let pressure_due = room >= MIN_REKEY_BYTES && chain >= (room / 2).max(MIN_REKEY_BYTES);
            let keyed = names.last().map(String::as_str) == Some("rekeyframe");
            assert_eq!(
                keyed,
                bytes_due || pressure_due,
                "scale {scale}, step {step}: chain {chain} base {base} interval {interval} budget {budget}: {names:?}"
            );
            if keyed {
                if bytes_due {
                    by_bytes += 1;
                } else {
                    by_pressure += 1;
                }
                chain = 0;
                base = rig.r.keyframes.back().expect("the rekey").charge();
            }
            first_of_chain = keyed;
        }
        assert!(rig.r.used <= rig.r.budget);
    }
    assert!(by_bytes > 0, "REACH: the byte interval keyed a chain");
    assert!(by_pressure > 0, "REACH: the pressure trigger keyed a chain");

    // The Rewind step row's chain: 80-column lines at the default budget, in reads of up to 64 KiB.
    let mut rig = ReaderRig::new(Terminal::new(24, 80), DEFAULT_BUDGET_BYTES);
    // The step row's chain file (`chain_text`): whole numbered lines just under 2,000,000 bytes, each newline reaching the reader as CRLF through the PTY.
    let (mut chain, mut file_len) = (String::with_capacity(2_100_000), 0);
    for n in 0.. {
        let digits = format!("{n:07} ");
        let line = format!(
            "\x1b[{}m{digits}\x1b[m{}\n",
            31 + n % 6,
            char::from(b'a' + (n % 26) as u8)
                .to_string()
                .repeat(80 - 1 - digits.len())
        );
        if file_len + line.len() > 2_000_000 {
            break;
        }
        file_len += line.len();
        chain.push_str(&line.replace('\n', "\r\n"));
    }
    let mut names = Vec::new();
    for (i, read) in chain.as_bytes().chunks(64 * 1024).enumerate() {
        names.extend(rig.slice(read, 20 * i as u64));
    }
    assert!(
        names.iter().filter(|n| *n == "witness").count() >= 7,
        "REACH: the chain was witnessed every 256 KiB: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n.contains("keyframe")),
        "the step row's chain stays one chain"
    );
    assert_eq!(rig.r.keyframe_count(), 1);
}

/// The reader closes every chain it keys with a witness of the screen right before the keyframe, so an instant late in the chain, past its last cadence witness, is judged: faithful once judged.
#[test]
fn every_closed_chain_end_is_covered_by_a_witness() {
    let mut rig = ReaderRig::new(Terminal::new(6, 30), 256 * 1024);
    assert_eq!(rig.slice(b"a\r\n", 0), ["in"]);
    assert_eq!(rig.slice(b"b\r\n", 1_100), ["in", "witness"]);
    let names = rig.slice(&vec![b'c'; MIN_REKEY_BYTES], 1_200);
    assert_eq!(
        names.last().map(String::as_str),
        Some("rekeyframe"),
        "precondition: due by bytes, a tenth of a second and less than WITNESS_BYTES past the last witness: {names:?}"
    );
    let at_c = rig
        .r
        .log
        .live()
        .filter(|e| matches!(e.op, Op::RawIn(_)))
        .last()
        .map(|e| e.ts)
        .expect("the last slice");
    let counts = rig.judge_all();
    assert_eq!((counts.diverged, counts.pending), (0, 0));
    assert_eq!(
        rig.r.fidelity_at(at_c),
        Fidelity::Faithful,
        "the chain's last slice is judged"
    );
    let keyed = rig.r.keyframes.back().expect("the rekey").seq;
    let closing = rig.r.witnesses.back().expect("the closing witness");
    assert_eq!(
        Seq(closing.seq.0 + 1),
        keyed,
        "the closed chain's last witness is right before its rekey"
    );
    assert!(closing.ts > at_c);
}

#[test]
fn a_net_zero_flap_between_two_reads_reaches_the_spine_and_the_replay() {
    const ROWS: u16 = 6;
    const COLS: u16 = 20;
    let mut live = Terminal::new(ROWS, COLS);
    let mut r = seeded(&live);
    let (tx, rx) = queue(64);
    let mut mark = SpineMark::attach(r.spine_watermark());
    let frame = alt_frame(ROWS, COLS);
    let diff = b"\x1b[1;1HT1\x1b[K\x1b[4;3H";
    slice(&mut live, &mut mark, &tx, &frame);
    live.resize(ROWS, COLS - 1);
    live.resize(ROWS, COLS);
    slice(&mut live, &mut mark, &tx, diff);
    drain(&rx, &mut r);
    assert_eq!((live.rows(), live.cols()), (ROWS, COLS), "net zero");
    assert_eq!(
        live.row_text(1).unwrap_or_default().trim_end(),
        "L1",
        "the live screen lost its last column"
    );
    let replayed = r.replay_at(None).expect("replay");
    assert_eq!(screen_of(&replayed), screen_of(&live));
    assert_eq!(r.lost_resizes(), 0);

    let mut old = TemporalRecorder::new();
    old.record_keyframe(Terminal::new(ROWS, COLS).checkpoint());
    old.record_raw_in(&frame);
    old.record_raw_in(diff);
    let uncut = old.replay_at(None).expect("replay");
    assert_ne!(screen_of(&uncut), screen_of(&live), "the flap it missed");

    let mut live = Terminal::new(ROWS, COLS);
    let mut r = seeded(&live);
    let (tx, rx) = queue(64);
    let mut mark = SpineMark::attach(r.spine_watermark());
    slice(&mut live, &mut mark, &tx, &frame);
    let before = screen_of(&live);
    live.resize(ROWS - 1, COLS);
    live.resize(ROWS, COLS);
    assert_eq!(screen_of(&live), before, "the engine undid the quiet flap");
    slice(&mut live, &mut mark, &tx, diff);
    assert_eq!(
        drain_named(&rx, &mut r),
        ["in", "r:5x20", "r:6x20", "in"],
        "both halves reach the spine, before the output after them"
    );
    let replayed = r.replay_at(None).expect("replay");
    assert_eq!(screen_of(&replayed), screen_of(&live));
}

#[test]
fn the_slices_of_one_batch_share_its_copy_in_engine_order() {
    const ROWS: u16 = 6;
    const COLS: u16 = 20;
    let mut live = Terminal::new(ROWS, COLS);
    let mut r = seeded(&live);
    let keyframe_charge = r.used;
    let (tx, rx) = queue(64);
    let mut mark = SpineMark::attach(r.spine_watermark());
    let mut bytes = alt_frame(ROWS, COLS);
    let a = bytes.len();
    bytes.extend_from_slice(b"\x1b[1;1HT1\x1b[K\x1b[4;3H");
    let b = bytes.len();
    bytes.extend_from_slice(b"\x1b[2;20HZ");
    let batch: Arc<[u8]> = Arc::from(bytes);
    let holds = [0..a, a..b, b..batch.len()];
    for (i, range) in holds.iter().enumerate() {
        match i {
            1 => live.resize(ROWS, COLS - 1),
            2 => live.resize(ROWS, COLS),
            _ => {}
        }
        let pre = SpinePre::take(&live);
        live.process_at(&batch[range.clone()], clock(0));
        mark.record(
            &tx,
            pre,
            &live,
            InputSlice::new(batch.clone(), range.clone(), clock(0)),
        );
    }
    assert_eq!(
        Arc::strong_count(&batch),
        1 + holds.len(),
        "each queued slice is a refcount on the batch"
    );
    let mut order = Vec::new();
    while let Some(msg) = rx.try_recv() {
        order.push(match &msg {
            TemporalMsg::RawIn(input) => {
                assert!(Arc::ptr_eq(&input.batch, &batch), "a range of the batch");
                format!("in:{:?}", input.range)
            }
            other => shape(other),
        });
        r.apply(msg);
    }
    assert_eq!(
        order,
        [
            format!("in:{:?}", holds[0]),
            format!("r:{ROWS}x{}", COLS - 1),
            format!("in:{:?}", holds[1]),
            format!("r:{ROWS}x{COLS}"),
            format!("in:{:?}", holds[2]),
        ],
        "each slice after the resizes before it"
    );
    assert_eq!(
        Arc::strong_count(&batch),
        1 + holds.len(),
        "the recorder retains the slices, not copies"
    );
    assert_eq!(r.used - keyframe_charge, batch.len(), "charged once");
    let replayed = r.replay_at(None).expect("replay");
    assert_eq!(screen_of(&replayed), screen_of(&live));

    let mut late = TemporalRecorder::new();
    late.record_keyframe(Terminal::new(ROWS, COLS).checkpoint());
    late.record_raw_in_shared(batch.clone());
    late.record_resize(ROWS, COLS - 1);
    late.record_resize(ROWS, COLS);
    let uncut = late.replay_at(None).expect("replay");
    assert_ne!(
        screen_of(&uncut),
        screen_of(&live),
        "the resizes out of order"
    );
}

#[test]
fn a_journal_overrun_is_counted_once_and_the_replay_ends_at_the_live_geometry() {
    let mut live = Terminal::new(6, 20);
    let mut r = seeded(&live);
    let (tx, rx) = queue(256);
    let mut mark = SpineMark::attach(r.spine_watermark());
    slice(&mut live, &mut mark, &tx, b"a\r\n");
    let n = aterm_core::terminal::RESIZE_JOURNAL_CAP as u64 + 5;
    for i in 0..n {
        live.resize(6, 20 + u16::try_from(i % 7).unwrap_or(0) + 1);
    }
    slice(&mut live, &mut mark, &tx, b"b\r\n");
    slice(&mut live, &mut mark, &tx, b"c\r\n");
    drain(&rx, &mut r);
    assert_eq!(r.lost_resizes(), 5, "counted once, not per slice");
    let replayed = r.replay_at(None).expect("replay");
    assert_eq!(
        (replayed.rows(), replayed.cols()),
        (live.rows(), live.cols())
    );
}

#[test]
fn a_full_queue_never_records_input_ahead_of_its_resizes() {
    let mut live = Terminal::new(6, 20);
    let mut r = TemporalRecorder::new();
    let (tx, rx) = queue(3);
    let mut mark = SpineMark::attach(r.spine_watermark());
    let mut seen = Vec::new();
    slice(&mut live, &mut mark, &tx, b"1");
    live.resize(5, 20);
    live.resize(6, 20);
    slice(&mut live, &mut mark, &tx, b"2");
    while let Some(msg) = rx.try_recv() {
        seen.push(shape(&msg));
        r.apply(msg);
    }
    assert_eq!(r.dropped_input(), 1, "slice 2's input");
    slice(&mut live, &mut mark, &tx, b"3");
    while let Some(msg) = rx.try_recv() {
        seen.push(shape(&msg));
        r.apply(msg);
    }
    assert_eq!(
        seen,
        ["r:6x20", "in:1", "r:5x20", "keyframe"],
        "slice 2's input was dropped, never queued before the grow"
    );
    let replayed = r.replay_at(None).expect("replay from the keyframe");
    assert_eq!(screen_of(&replayed), screen_of(&live));
    assert!(!r.spine_watermark().break_open());
}

#[test]
fn a_flap_while_no_reader_was_attached_reaches_the_next_reader() {
    const ROWS: u16 = 6;
    const COLS: u16 = 20;
    let diff = b"\x1b[1;1HT1\x1b[K\x1b[4;3H";
    for starts_over in [false, true] {
        let mut live = Terminal::new(ROWS, COLS);
        let mut r = seeded(&live);
        let (tx, rx) = queue(64);
        let mut first = SpineMark::attach(r.spine_watermark());
        slice(&mut live, &mut first, &tx, &alt_frame(ROWS, COLS));
        drop(first);
        live.resize(ROWS, COLS - 1);
        live.resize(ROWS, COLS);
        let mut next = if starts_over {
            SpineMark::default()
        } else {
            SpineMark::attach(r.spine_watermark())
        };
        slice(&mut live, &mut next, &tx, diff);
        drain(&rx, &mut r);
        let replayed = r.replay_at(None).expect("replay");
        assert_eq!(
            screen_of(&replayed) == screen_of(&live),
            !starts_over,
            "a reader that starts over misses the flap (starts over: {starts_over})"
        );
        assert_eq!(r.lost_resizes(), 0);
    }
}

#[test]
fn an_overrun_while_no_reader_was_attached_is_counted() {
    let mut live = Terminal::new(6, 20);
    let mut r = seeded(&live);
    let (tx, rx) = queue(256);
    let mut first = SpineMark::attach(r.spine_watermark());
    slice(&mut live, &mut first, &tx, b"a\r\n");
    drop(first);
    let n = aterm_core::terminal::RESIZE_JOURNAL_CAP as u64 + 3;
    for i in 0..n {
        live.resize(6, 21 + u16::try_from(i % 5).unwrap_or(0));
    }
    let mut next = SpineMark::attach(r.spine_watermark());
    slice(&mut live, &mut next, &tx, b"b\r\n");
    drain(&rx, &mut r);
    assert_eq!(r.lost_resizes(), 3);
    let replayed = r.replay_at(None).expect("replay");
    assert_eq!(
        (replayed.rows(), replayed.cols()),
        (live.rows(), live.cols())
    );
}

#[test]
fn pending_counts_the_resizes_the_spine_has_not_been_handed() {
    let mut live = Terminal::new(6, 20);
    let r = seeded(&live);
    let watermark = r.spine_watermark();
    let (tx, _rx) = queue(64);
    let mut mark = SpineMark::attach(watermark.clone());
    slice(&mut live, &mut mark, &tx, b"a");
    assert_eq!(watermark.pending(&live), 0);
    live.resize(5, 20);
    live.resize(6, 20);
    assert_eq!(
        watermark.pending(&live),
        2,
        "a net-zero flap, no output since"
    );
    slice(&mut live, &mut mark, &tx, b"b");
    assert_eq!(
        watermark.pending(&live),
        0,
        "the next hold handed both over"
    );
    assert_eq!(SpineWatermark::default().pending(&live), 2);
}

#[test]
fn replay_degrades_to_none_when_needed_blobs_were_evicted() {
    let mut r = TemporalRecorder::with_budget(128);
    let blank = aterm_core::terminal::Terminal::new(6, 20);
    r.record_keyframe(blank.checkpoint());
    for _ in 0..64 {
        r.record_raw_in(&[b'x'; 64]);
    }
    assert!(r.replay_at(None).is_none());
    assert_eq!(r.reachable_from(), None, "status says nothing replays");
}

fn tick_past(r: &TemporalRecorder, tick: Ticks) {
    let deadline = Instant::now() + std::time::Duration::from_secs(60);
    while r.now().0 <= tick.0 + 1 {
        assert!(Instant::now() < deadline, "the recorder clock stopped");
        std::hint::spin_loop();
    }
}

#[test]
fn a_dropped_rawin_is_counted_and_unreachable_after_it() {
    for cap in [2, 64] {
        let dropped = cap == 2;
        let mut live = Terminal::new(6, 20);
        let mut r = seeded(&live);
        let (tx, rx) = queue(cap);
        let mut mark = SpineMark::attach(r.spine_watermark());
        slice(&mut live, &mut mark, &tx, b"one\r\n");
        slice(&mut live, &mut mark, &tx, b"two\r\n");
        let before = screen_of(&live);
        slice(&mut live, &mut mark, &tx, b"three\r\n");
        slice(&mut live, &mut mark, &tx, b"four\r\n");
        assert_eq!(drain(&rx, &mut r), if dropped { 2 } else { 4 });
        let last = r.latest_tick();
        assert_eq!(r.dropped_input(), u64::from(dropped));
        assert_eq!(r.spine_watermark().break_open(), dropped);
        assert_eq!(r.replay_at(None).is_none(), dropped, "the spine's end");
        assert_eq!(r.reachable_from().is_none(), dropped);
        let at_last = r.replay_at(Some(last)).expect("the last event replays");
        let expected = if dropped { before } else { screen_of(&live) };
        assert_eq!(screen_of(&at_last), expected);

        drop(mark);
        let mut mark = SpineMark::attach(r.spine_watermark());
        tick_past(&r, last);
        slice(&mut live, &mut mark, &tx, b"five\r\n");
        let queued = drain_named(&rx, &mut r);
        assert_eq!(queued, [if dropped { "keyframe" } else { "in" }]);
        assert!(!r.spine_watermark().break_open());
        let end = r.replay_at(None).expect("the spine's end replays again");
        assert_eq!(screen_of(&end), screen_of(&live));
        assert_eq!(
            r.replay_at(Some(Ticks(last.0 + 1))).is_none(),
            dropped,
            "a tick past the drop"
        );
        assert_eq!(
            r.reachable_from().is_some_and(|tick| tick > last),
            dropped,
            "replay starts past the break"
        );
    }
}

#[test]
fn a_dropped_reply_is_counted_and_breaks_nothing() {
    let mut live = Terminal::new(6, 20);
    let mut r = seeded(&live);
    let (tx, rx) = queue(1);
    let mut mark = SpineMark::attach(r.spine_watermark());
    slice(&mut live, &mut mark, &tx, b"one\r\n");
    mark.reply(&tx);
    assert_eq!((r.dropped_replies(), r.dropped_input()), (1, 0));
    drain(&rx, &mut r);
    slice(&mut live, &mut mark, &tx, b"two\r\n");
    assert_eq!(drain_named(&rx, &mut r), ["in"]);
    let replayed = r.replay_at(None).expect("no break");
    assert_eq!(screen_of(&replayed), screen_of(&live));

    slice(&mut live, &mut mark, &tx, b"three\r\n");
    slice(&mut live, &mut mark, &tx, b"four\r\n");
    drain(&rx, &mut r);
    mark.reply(&tx);
    assert!(
        rx.try_recv().is_none(),
        "the break queues only its keyframe"
    );
    assert_eq!((r.dropped_replies(), r.dropped_input()), (2, 1));
}

#[test]
fn a_break_waits_for_a_parser_a_keyframe_can_carry() {
    let mut live = Terminal::new(6, 20);
    let mut r = seeded(&live);
    let (tx, rx) = queue(1);
    let mut mark = SpineMark::attach(r.spine_watermark());
    slice(&mut live, &mut mark, &tx, b"one\r\n");
    slice(&mut live, &mut mark, &tx, b"two\x1bP$qm");
    drain(&rx, &mut r);
    slice(&mut live, &mut mark, &tx, b"x");
    assert_eq!(live.partial_sequence_uncarried(), Some("DcsPassthrough"));
    assert!(rx.try_recv().is_none(), "no keyframe carries the query");
    assert!(r.spine_watermark().break_open());
    slice(&mut live, &mut mark, &tx, b"\x1b\\three");
    assert_eq!(drain_named(&rx, &mut r), ["keyframe"]);
    let replayed = r.replay_at(None).expect("replay");
    assert_eq!(screen_of(&replayed), screen_of(&live));
}

#[test]
fn a_break_waits_out_a_sequence_a_keyframe_would_swallow() {
    let tail = b"/w==\x1b\\after";
    let mut live = Terminal::new(6, 20);
    let mut r = seeded(&live);
    let (tx, rx) = queue(1);
    let mut mark = SpineMark::attach(r.spine_watermark());
    slice(&mut live, &mut mark, &tx, b"one\r\n");
    slice(
        &mut live,
        &mut mark,
        &tx,
        b"two\x1b_Ga=T,f=32,s=1,v=1,c=4,r=3,q=2;",
    );
    drain(&rx, &mut r);
    slice(&mut live, &mut mark, &tx, b"/wAA");
    assert_eq!(live.partial_sequence_swallowed(), Some("SosPmApcString"));
    assert!(rx.try_recv().is_none(), "no keyframe swallows the image");
    assert!(r.spine_watermark().break_open());
    let swallowing = ReplayPlan {
        base: Arc::new(
            live.checkpoint_continuing_bounded(REKEY_SCROLLBACK_LINES)
                .expect("carried, as a string to swallow"),
        ),
        carry: Arc::new(ReplayCarry::default()),
        base_seq: Seq(0),
        base_ts: Ticks(0),
        steps: vec![Placed {
            seq: Seq(1),
            ts: Ticks(0),
            step: Step::RawIn(InputSlice::at(&tail[..], clock(0))),
        }],
        policy: ResizePolicy::Native,
        host_resyncs: false,
        stands_in: true,
        config: None,
    };

    slice(&mut live, &mut mark, &tx, tail);
    assert_eq!(drain_named(&rx, &mut r), ["keyframe"]);
    let replayed = r.replay_at(None).expect("replay");
    let view = |t: &Terminal| (screen_of(t), t.cursor());
    assert_eq!(view(&replayed), view(&live));
    assert_ne!(view(&swallowing.run()), view(&live), "the image moved it");
}

#[test]
fn a_break_projects_no_keyframe_for_a_writer_that_is_gone() {
    for gone in [false, true] {
        let mut live = Terminal::new(6, 20);
        let r = seeded(&live);
        let (tx, rx) = queue(1);
        let mut mark = SpineMark::attach(r.spine_watermark());
        slice(&mut live, &mut mark, &tx, b"one\r\n");
        slice(&mut live, &mut mark, &tx, b"two\r\n");
        assert!(r.spine_watermark().break_open());
        assert!(rx.try_recv().is_some(), "the writer takes the first slice");
        let rx = (!gone).then_some(rx);
        let before = resync_probe::count();
        for _ in 0..4 {
            slice(&mut live, &mut mark, &tx, b"x");
        }
        let projected = resync_probe::count() - before;
        assert_eq!(projected, usize::from(!gone), "gone: {gone}");
        if let Some(rx) = rx {
            assert_eq!(
                rx.try_recv().as_ref().map(shape).as_deref(),
                Some("keyframe")
            );
        }
    }
}

fn mark_times(t: &Terminal) -> Vec<[Option<u64>; 4]> {
    t.command_marks()
        .iter()
        .map(|m| {
            [
                m.prompt_time_ms,
                m.command_input_start_time_ms,
                m.command_exec_start_time_ms,
                m.command_end_time_ms,
            ]
        })
        .collect()
}

/// A mode-2026 frame left open past its timeout, bells 50 ms apart and OSC 133 marks replay at the recorded clock exactly as the live engine parsed them; on the live clock the frame is still open.
#[test]
fn a_replay_at_the_recorded_clock_is_the_live_engine() {
    const TIMEOUT_MS: u64 = 1000;
    let fixture: [(u64, &[u8]); 8] = [
        (0, b"\x1b]133;A\x07$ \x1b]133;B\x07"),
        (10, b"ls\r\n\x1b]133;C\x07"),
        (20, b"\x1b[?2026hframe"),
        (50, b"\x07"),
        (100, b"\x07"),
        (150, b"\x07"),
        (200, b"\x07"),
        (20 + TIMEOUT_MS + 500, b"after\r\n\x1b]133;D;0\x07"),
    ];
    let mut live = Terminal::new(6, 30);
    let mut r = TemporalRecorder::new();
    r.record_keyframe(live.checkpoint());
    for (ms, bytes) in fixture {
        live.process_at(bytes, clock(ms));
        r.apply(TemporalMsg::RawIn(InputSlice::at(bytes, clock(ms))));
    }
    assert!(
        !live.modes().synchronized_output,
        "precondition: the timeout ended the frame live"
    );
    assert_eq!(
        live.bell_total(),
        2,
        "precondition: the 100 ms bell floor held two"
    );
    let marks = mark_times(&live);
    assert_eq!(
        marks.last().and_then(|m| m[3]),
        clock(20 + TIMEOUT_MS + 500).wall_ms,
        "precondition: the mark took the slice's wall clock"
    );

    let replayed = r.replay_at(None).expect("replay");
    assert_eq!(replayed.checkpoint_visible(), live.checkpoint_visible());
    assert_eq!(replayed.bell_total(), live.bell_total());
    assert_eq!(mark_times(&replayed), marks);

    let _live = replay_clock::on_live_clock();
    let plain = r.replay_at(None).expect("replay");
    assert!(
        plain.modes().synchronized_output,
        "on the live clock the frame never timed out"
    );
    assert_ne!(plain.checkpoint_visible(), live.checkpoint_visible());
}

/// Two live witnesses of a recorded session match its replay once judged ([`judge_all`]); replayed on the live clock both diverge, which asks the reader for a live keyframe it queues on the next slice. A burst due a rekey ends its chain at the witness the reader sends before its keyframe, which covers the burst once judged, and the replay is the live screen.
#[test]
fn live_witnesses_judge_the_replay_and_a_mismatch_takes_a_live_keyframe() {
    for live_clock in [false, true] {
        let mut rig = ReaderRig::new(Terminal::new(6, 30), 4 * MIN_REKEY_BYTES);
        let _clock = live_clock.then(replay_clock::on_live_clock);
        let feed = |rig: &mut ReaderRig, bytes: &[u8], ms| {
            tick_past(&rig.r, rig.r.latest_tick());
            rig.slice(bytes, ms)
        };
        assert_eq!(feed(&mut rig, b"\x1b[?2026hframe", 0), ["in"]);
        assert_eq!(feed(&mut rig, b"\r\none", 1500), ["in", "witness"]);
        assert!(!rig.live.modes().synchronized_output, "precondition");
        assert_eq!(feed(&mut rig, b"\r\ntwo", 3000), ["in", "witness"]);
        assert_eq!(rig.r.witnessed(), 0, "no fold has judged them yet");
        let counts = rig.judge_all();
        assert_eq!(
            (counts.judged, counts.diverged, counts.pending),
            (2, if live_clock { 2 } else { 0 }, 0)
        );
        assert_eq!(rig.r.spine_watermark().keyframe_wanted(), live_clock);
        let at_witness = rig.r.witnesses[0].ts;
        assert_eq!(
            rig.r.fidelity_at(at_witness),
            if live_clock {
                Fidelity::Diverged
            } else {
                Fidelity::Faithful
            }
        );
        assert_eq!(
            feed(&mut rig, b"\r\nthree", 3200),
            [if live_clock { "live-keyframe" } else { "in" }]
        );
        assert!(!rig.r.spine_watermark().keyframe_wanted());
        assert_eq!(rig.r.keyframe_count(), if live_clock { 2 } else { 1 });
        assert_eq!(
            feed(&mut rig, &[b'.'; MIN_REKEY_BYTES], 3300),
            ["in", "witness", "rekeyframe"],
            "the burst is due a rekey"
        );
        let at_burst = rig
            .r
            .log
            .live()
            .filter(|e| matches!(e.op, Op::RawIn(_)))
            .last()
            .map(|e| e.ts)
            .expect("the burst");
        assert_eq!(
            rig.r.fidelity_at(at_burst),
            Fidelity::Unwitnessed,
            "the witness that closed the chain is pending"
        );
        rig.assert_faithful(&format!("live_clock={live_clock}"));
        assert_eq!(
            rig.r.fidelity_at(at_burst),
            Fidelity::Faithful,
            "the witness that closed the chain covers the burst"
        );
    }
}

/// An instant is faithful once the first witness after the last slice its replay folds, and every one before it in the chain, matched; diverged from a mismatch on; unwitnessed while that witness is pending, and with no keyframe.
#[test]
fn the_fidelity_of_an_instant_is_the_verdict_of_the_witnesses_that_cover_it() {
    assert_eq!(
        TemporalRecorder::new().fidelity_at(Ticks(0)),
        Fidelity::Unwitnessed,
        "no keyframe"
    );
    let mut live = Terminal::new(6, 30);
    let mut r = seeded(&live);
    let (tx, rx) = queue(64);
    let mut mark = SpineMark::attach(r.spine_watermark());
    let mut feed = |live: &mut Terminal, r: &mut TemporalRecorder, bytes: &[u8], ms| {
        tick_past(r, r.latest_tick());
        slice_at(live, &mut mark, &tx, bytes, clock(ms));
        drain(&rx, r)
    };
    feed(&mut live, &mut r, b"a", 0);
    let before = r.latest_tick();
    feed(&mut live, &mut r, b"b", 1000);
    feed(&mut live, &mut r, b"c", 2000);
    assert_eq!(r.witnesses.len(), 2, "REACH: one witness a second");
    let [first, second] = [r.witnesses[0].ts, r.witnesses[1].ts];
    let at_c = r
        .log
        .live()
        .filter(|e| matches!(e.op, Op::RawIn(_)))
        .last()
        .map(|e| e.ts)
        .expect("slice c");
    assert!(first < at_c, "precondition: c follows the first witness");
    assert_eq!(
        r.fidelity_at(before),
        Fidelity::Unwitnessed,
        "nothing judged"
    );

    let fold = r.replay_plan(Some(first)).expect("replay").fold();
    r.judge(fold.judged);
    assert_eq!(
        (r.witnessed(), r.diverged()),
        (1, 0),
        "a fold judges what it passes"
    );
    assert_eq!(r.fidelity_at(before), Fidelity::Faithful);
    assert_eq!(r.fidelity_at(first), Fidelity::Faithful);
    assert_eq!(
        r.fidelity_at(Ticks(first.0 + 1)),
        Fidelity::Faithful,
        "a replay past the witness that folds no more slices is the screen it saw"
    );
    assert_eq!(
        r.fidelity_at(at_c),
        Fidelity::Unwitnessed,
        "its next witness is pending"
    );

    let poisoned = r.witnesses.back_mut().expect("the second witness");
    let WitnessState::Pending(live_cp) = &poisoned.state else {
        panic!("pending");
    };
    let mut wrong = (**live_cp).clone();
    wrong.cols += 1;
    poisoned.state = WitnessState::Pending(Arc::new(wrong));
    let fold = r.replay_plan(None).expect("replay").fold();
    r.judge(fold.judged);
    assert_eq!((r.witnessed(), r.diverged()), (2, 1));
    assert_eq!(
        r.fidelity_at(first),
        Fidelity::Faithful,
        "before the mismatch"
    );
    assert_eq!(r.fidelity_at(at_c), Fidelity::Diverged);
    assert_eq!(r.fidelity_at(second), Fidelity::Diverged);
    assert_eq!(r.fidelity_at(Ticks(second.0 + 1)), Fidelity::Diverged);
    assert!(r.spine_watermark().keyframe_wanted());
}

/// A witness no fold has judged reads unchecked at every instant it covers, never faithful, in every chain the recording keyed past it; [`judge_all`] then judges each by full equality, so a witness that differs from the replay only where a bounded history renumbers (its row counter) reads diverged, and the track draws a differing stretch only once it is judged.
#[test]
fn an_unjudged_witness_reads_unchecked_never_faithful() {
    use crate::rewind::Faith;
    let mut live = Terminal::new(6, 30);
    let recorder = Mutex::new(seeded(&live));
    let put = |live: &mut Terminal, r: &mut TemporalRecorder, bytes: &[u8]| {
        tick_past(r, r.latest_tick());
        live.process_at(bytes, clock(0));
        r.apply(TemporalMsg::RawIn(InputSlice::at(bytes, clock(0))));
        r.latest_tick()
    };
    let see = |live: &Terminal, r: &mut TemporalRecorder, renumbered: bool| {
        tick_past(r, r.latest_tick());
        let mut seen = live.checkpoint_visible().expect("visible");
        if renumbered {
            seen.absolute_row_counter += 7;
        }
        r.apply(TemporalMsg::Witness(Arc::new(seen)));
    };
    let (covered, tampered) = {
        let mut r = recorder.lock().unwrap();
        let mut covered = Vec::new();
        for chain in 0..3 {
            if chain > 0 {
                tick_past(&r, r.latest_tick());
                r.record_keyframe(keyframe_of(&live, usize::MAX).expect("keyable"));
            }
            for n in 0..2 {
                let line = format!("chain {chain} line {n}\r\n");
                covered.push(put(&mut live, &mut r, line.as_bytes()));
                see(&live, &mut r, false);
            }
        }
        let tampered = put(&mut live, &mut r, b"renumbered\r\n");
        see(&live, &mut r, true);
        assert_eq!(
            (r.keyframe_count(), r.pending(), r.witnessed()),
            (3, 7, 0),
            "fixture: three chains, every witness pending"
        );
        for &at in covered.iter().chain([&tampered]) {
            assert_eq!(r.fidelity_at(at), Fidelity::Unwitnessed, "at {at:?}");
            assert_eq!(Faith::of(r.fidelity_at(at)).band(), "unchecked");
        }
        assert!(
            r.rewind_track()
                .spans
                .iter()
                .all(|&(.., span)| span != Span::Differs),
            "nothing differs before a judge"
        );
        (covered, tampered)
    };
    assert_eq!(
        judge_all(&recorder),
        JudgeCounts {
            judged: 7,
            diverged: 1,
            pending: 0,
            unjudged_evicted: 0,
        },
        "full equality catches the renumbered witness"
    );
    let r = recorder.lock().unwrap();
    for &at in &covered {
        assert_eq!(r.fidelity_at(at), Fidelity::Faithful, "at {at:?}");
        assert_eq!(Faith::of(r.fidelity_at(at)).band(), "faithful");
    }
    assert_eq!(r.fidelity_at(tampered), Fidelity::Diverged);
    assert!(
        r.rewind_track()
            .spans
            .iter()
            .any(|&(.., span)| span == Span::Differs),
        "the judged mismatch differs"
    );
}

/// Every witness no fold judged that an eviction drops is counted, and only those: the budget's eviction of the oldest chains and the pool's forget add each, a judged witness evicted adds none, and a forget the person asked for adds none and leaves nothing pending.
#[test]
fn an_evicted_unjudged_witness_is_counted() {
    let mut live = Terminal::new(4, 20);
    let chain = keyframe_of(&live, usize::MAX).expect("keyable").charge
        + 3 * (charge_of(&live.checkpoint_visible().expect("visible")) + 256);
    let mut r = TemporalRecorder::with_budget(3 * chain);
    r.record_keyframe(keyframe_of(&live, usize::MAX).expect("keyable"));
    let mut judged = Vec::new();
    for n in 0..8 {
        if n > 0 {
            r.record_keyframe(keyframe_of(&live, usize::MAX).expect("keyable"));
        }
        for line in 0..3 {
            let bytes = format!("chain {n} line {line}\r\n");
            live.process_at(bytes.as_bytes(), clock(0));
            r.apply(TemporalMsg::RawIn(InputSlice::at(
                bytes.as_bytes(),
                clock(0),
            )));
            let seen = live.checkpoint_visible().expect("visible");
            r.apply(TemporalMsg::Witness(Arc::new(seen)));
        }
        if n == 0 {
            judged.extend(r.witnesses.iter().map(|w| w.id));
            let fold = r.replay_plan(None).expect("replay").fold();
            r.judge(fold.judged);
        }
    }
    let kept: Vec<WitnessId> = r.witnesses.iter().map(|w| w.id).collect();
    let evicted = (0..r.next_witness)
        .map(WitnessId)
        .filter(|id| !kept.contains(id) && !judged.contains(id))
        .count();
    assert_eq!(r.next_witness, 24, "fixture: every witness was taken");
    assert_eq!(r.witnessed(), 3, "fixture: only the first chain was judged");
    assert!(
        r.keyframe_count() < 8 && !kept.is_empty() && judged.iter().all(|id| !kept.contains(id)),
        "fixture: the budget evicted the oldest chains, the judged one among them, and kept the newest"
    );
    assert_eq!(r.unjudged_evicted(), evicted as u64);
    assert_eq!(r.pending(), kept.len() as u64);

    let before = r.unjudged_evicted();
    let pending = r.pending();
    r.forget_for_pool();
    assert_eq!(
        (r.unjudged_evicted(), r.pending()),
        (before + pending, 0),
        "the pool's forget counts what it drops"
    );

    r.record_keyframe(keyframe_of(&live, usize::MAX).expect("keyable"));
    let seen = live.checkpoint_visible().expect("visible");
    r.apply(TemporalMsg::Witness(Arc::new(seen)));
    assert_eq!(r.pending(), 1, "fixture: a pending witness");
    let before = r.unjudged_evicted();
    r.forget();
    assert_eq!(
        (r.unjudged_evicted(), r.pending()),
        (before, 0),
        "a forget the person asked for counts nothing"
    );
}

/// Slices recorded while the clock reads no later than the newest event, as a writer recording
/// a batch within one microsecond sees it, each take the tick past the one before and stay
/// their own Rewind state.
#[test]
fn slices_recorded_within_one_microsecond_keep_their_own_ticks_and_states() {
    let mut live = Terminal::new(6, 30);
    let mut r = seeded(&live);
    let put = |live: &mut Terminal, r: &mut TemporalRecorder, bytes: &[u8]| {
        live.process_at(bytes, clock(0));
        r.apply(TemporalMsg::RawIn(InputSlice::at(bytes, clock(0))));
    };
    let ahead = Ticks(r.now().0 + 60_000_000);
    r.freeze_now_for_test(Some(ahead));
    put(&mut live, &mut r, b"one");
    r.freeze_now_for_test(None);
    let states = r.rewind_instants().len();
    put(&mut live, &mut r, b"\r\ntwo");
    put(&mut live, &mut r, b"\r\nthree");
    let ticks: Vec<Ticks> = r.log.live().map(|e| e.ts).collect();
    assert!(
        ticks.windows(2).all(|pair| pair[0] < pair[1]),
        "every event has its own tick: {ticks:?}"
    );
    assert_eq!(
        r.latest_tick(),
        Ticks(ahead.0 + 2),
        "the clock read behind the newest event, so each slice took the tick past it"
    );
    assert_eq!(
        r.rewind_instants().len(),
        states + 2,
        "each slice is its own state"
    );
}

/// A witness and the slice after it on one tick, as a frozen clock stamps them (the writer's own stamps never share one, [`TemporalRecorder::stamp`]): a replay to that tick folds the slice, so only the next witness covers it.
#[test]
fn a_slice_on_its_witness_tick_is_covered_by_the_next_witness() {
    for poisoned in [false, true] {
        let mut live = Terminal::new(6, 30);
        let mut r = seeded(&live);
        let put = |live: &mut Terminal, r: &mut TemporalRecorder, bytes: &[u8], ms| {
            live.process_at(bytes, clock(ms));
            r.apply(TemporalMsg::RawIn(InputSlice::at(bytes, clock(ms))));
        };
        tick_past(&r, r.latest_tick());
        put(&mut live, &mut r, b"one", 0);
        let one = r.latest_tick();
        tick_past(&r, one);
        let tie = r.now();
        r.frozen_now = Some(tie);
        let seen = live.checkpoint_visible().expect("at ground");
        r.apply(TemporalMsg::Witness(Arc::new(seen)));
        put(&mut live, &mut r, b"\r\ntwo", 10);
        r.frozen_now = None;
        assert_eq!(
            (r.witnesses[0].ts, r.latest_tick()),
            (tie, tie),
            "precondition: the witness and the slice after it share a tick"
        );
        let fold = r.replay_plan(Some(tie)).expect("replay").fold();
        r.judge(fold.judged);
        assert_eq!((r.witnessed(), r.diverged()), (1, 0));
        assert_eq!(r.fidelity_at(one), Fidelity::Faithful);
        assert_eq!(r.covering_witness(one), Some(tie));
        assert_eq!(
            r.fidelity_at(tie),
            Fidelity::Unwitnessed,
            "the witness on the tick saw the screen before its slice"
        );
        assert_eq!(
            r.covering_witness(tie),
            None,
            "no witness covers the tick yet"
        );

        tick_past(&r, tie);
        let mut seen = live.checkpoint_visible().expect("at ground");
        if poisoned {
            seen.cols += 1;
        }
        r.apply(TemporalMsg::Witness(Arc::new(seen)));
        assert_eq!(
            r.fidelity_at(tie),
            Fidelity::Unwitnessed,
            "the covering witness is pending"
        );
        let to = r.covering_witness(tie);
        assert_eq!(
            to,
            Some(r.witnesses[1].ts),
            "a view at the tick folds to the next witness for its verdict"
        );
        let fold = r.replay_plan(to).expect("replay").fold();
        r.judge(fold.judged);
        assert_eq!((r.witnessed(), r.diverged()), (2, u64::from(poisoned)));
        assert_eq!(
            r.fidelity_at(tie),
            if poisoned {
                Fidelity::Diverged
            } else {
                Fidelity::Faithful
            },
            "poisoned: {poisoned}"
        );
    }
}

/// A witness covering a view's instant on the tick of the next keyframe is judged in the instant's own chain, not the keyframe's, so the view reaches a verdict instead of staying unchecked.
#[test]
fn a_covering_witness_on_the_next_keyframes_tick_is_judged_in_its_own_chain() {
    for poisoned in [false, true] {
        let mut live = Terminal::new(6, 30);
        let mut r = seeded(&live);
        tick_past(&r, r.latest_tick());
        live.process_at(b"one", clock(0));
        r.apply(TemporalMsg::RawIn(InputSlice::at(b"one", clock(0))));
        let at = r.latest_tick();
        tick_past(&r, at);
        let tie = r.now();
        r.frozen_now = Some(tie);
        let mut seen = live.checkpoint_visible().expect("at ground");
        if poisoned {
            seen.cols += 1;
        }
        r.apply(TemporalMsg::Witness(Arc::new(seen)));
        let next = keyframe_of(&live, usize::MAX).expect("at ground");
        r.apply(TemporalMsg::LiveKeyframe(next, SliceMarks::default()));
        r.frozen_now = None;
        assert_eq!(
            (r.witnesses[0].ts, r.keyframes[1].ts),
            (tie, tie),
            "precondition: the covering witness shares the next keyframe's tick"
        );
        assert_eq!(r.fidelity_at(at), Fidelity::Unwitnessed);
        assert!(
            r.replay_plan(Some(tie))
                .expect("replay")
                .fold()
                .judged
                .0
                .is_empty(),
            "negative control: a fold to the tick replays the next keyframe's chain"
        );
        let judge = r
            .covering_plan(at)
            .expect("the instant's chain folds to its witness");
        r.judge(judge.fold().judged);
        assert_eq!((r.witnessed(), r.diverged()), (1, u64::from(poisoned)));
        assert_eq!(
            r.fidelity_at(at),
            if poisoned {
                Fidelity::Diverged
            } else {
                Fidelity::Faithful
            },
            "poisoned: {poisoned}"
        );
        assert!(
            r.covering_plan(tie).is_none(),
            "no witness after the tie covers it yet"
        );
    }
}

/// A chain recorded under the session's config replays a soft reset (DECSTR) to that config's cursor, so its witness matches in a fold and in the pane task; with no config recorded the same chain diverges there.
#[test]
fn a_chain_replays_under_the_config_its_keyframe_was_recorded_with() {
    let config = Arc::new(TerminalConfig {
        cursor_style: aterm_core::terminal::CursorStyle::SteadyBar,
        ..TerminalConfig::default()
    });
    for recorded in [false, true] {
        let mut live = Terminal::new(6, 30);
        let _ = live.apply_config(&config);
        let mut r = TemporalRecorder::new();
        if recorded {
            r.set_replay_config(Some(config.clone()));
        }
        r.record_keyframe(live.checkpoint());
        r.seed_spine(SpinePre::take(&live));
        let (tx, rx) = queue(64);
        let mut mark = SpineMark::attach(r.spine_watermark());
        slice_at(&mut live, &mut mark, &tx, b"\x1b[2 qa", clock(0));
        slice_at(&mut live, &mut mark, &tx, b"\x1b[!pb", clock(1000));
        drain(&rx, &mut r);
        assert_eq!(r.witnesses.len(), 1, "REACH: the soft reset is witnessed");
        let chains = r.pane_chains().expect("a chain");
        let check = chains[0].check(aterm_aos_pane::PaneCode::ThisBuild, "test");
        let fold = r.replay_plan(None).expect("replay").fold();
        r.judge(fold.judged);
        assert_eq!(r.diverged(), u64::from(!recorded), "recorded: {recorded}");
        assert_eq!(
            check.verdict == aterm_aos::contract::Verdict::Pass,
            recorded,
            "recorded: {recorded}: {:?}",
            check.verdict
        );
    }
}

/// Every chain the recorder keeps replays as the pane task, each slice's reply and each witness matched; a reply the reader did not record fails `RepliesMatch` at its slice, in each chain.
#[test]
fn every_kept_chain_replays_as_the_pane_task_with_its_replies() {
    use aterm_aos::contract::Verdict;
    use aterm_aos_pane::{PaneCode, PaneCounts, REPLIES_MATCH};

    for replies_recorded in [true, false] {
        let mut live = Terminal::new(6, 30);
        let mut r = seeded(&live);
        let (tx, rx) = queue(64);
        let mut mark = SpineMark::attach(r.spine_watermark());
        let mut feed = |live: &mut Terminal, r: &mut TemporalRecorder, bytes: &[u8], ms| {
            let pre = SpinePre::take(live);
            live.process_at(bytes, clock(ms));
            let (reports, response) = (live.take_reports(), live.take_response());
            let reply =
                reply_of(response.as_deref(), reports.as_deref()).filter(|_| replies_recorded);
            mark.record(
                &tx,
                pre,
                live,
                InputSlice::at(bytes, clock(ms)).replying(reply),
            );
            drain(&rx, r);
        };
        feed(&mut live, &mut r, b"$ ", 0);
        feed(&mut live, &mut r, b"\x1b[c", 10);
        feed(&mut live, &mut r, b"ls\r\n", 1000);
        r.spine.keyframe_wanted.store(true, Ordering::Release);
        feed(&mut live, &mut r, b"a b c\r\n", 1100);
        feed(&mut live, &mut r, b"\x1b[6n$ ", 2200);
        feed(&mut live, &mut r, b"x", 3300);
        assert_eq!(
            (r.keyframe_count(), r.witnesses.len()),
            (2, 2),
            "REACH: a live keyframe starts a second chain, each witnessed once"
        );
        let checks: Vec<_> = r
            .pane_chains()
            .expect("chains")
            .iter()
            .map(|chain| chain.check(PaneCode::ThisBuild, "test"))
            .collect();
        let counts: Vec<_> = checks.iter().map(|c| c.counts).collect();
        assert_eq!(
            counts,
            [
                PaneCounts {
                    bytes: 3,
                    resizes: 0,
                    witnesses: 1,
                    matched: 1
                },
                PaneCounts {
                    bytes: 2,
                    resizes: 0,
                    witnesses: 1,
                    matched: 1
                }
            ]
        );
        let verdicts: Vec<_> = checks
            .iter()
            .map(|c| match &c.verdict {
                Verdict::Pass => None,
                Verdict::Fail { law, cursor, .. } => Some((*law, *cursor)),
            })
            .collect();
        if replies_recorded {
            assert_eq!(verdicts, [None, None]);
            assert_eq!(
                checks.iter().map(|c| c.replies_matched).collect::<Vec<_>>(),
                [3, 2]
            );
        } else {
            assert_eq!(
                verdicts,
                [Some((REPLIES_MATCH, 1)), Some((REPLIES_MATCH, 0))],
                "the DA and CPR slices"
            );
        }
    }
}

/// The first slice at parser ground that brings the output since the last witness to 256 KiB takes one, inside the second; a slice off ground leaves it due.
#[test]
fn a_witness_is_due_after_256_kib_of_output() {
    let mut live = Terminal::new(6, 30);
    let mut r = seeded(&live);
    let (tx, rx) = queue(64);
    let mut mark = SpineMark::attach(r.spine_watermark());
    let mut feed = |live: &mut Terminal, r: &mut TemporalRecorder, bytes: &[u8], ms| {
        slice_at(live, &mut mark, &tx, bytes, clock(ms));
        drain_named(&rx, r)
    };
    assert_eq!(feed(&mut live, &mut r, b"a", 0), ["in"]);
    let short = vec![b'.'; WITNESS_BYTES - 2];
    assert_eq!(
        feed(&mut live, &mut r, &short, 10),
        ["in"],
        "one byte short"
    );
    assert_eq!(feed(&mut live, &mut r, b".", 20), ["in", "witness"]);

    let open = [&b"\x1b]2;"[..], &vec![b'.'; WITNESS_BYTES]].concat();
    assert_eq!(feed(&mut live, &mut r, &open, 30), ["in"], "off ground");
    assert_eq!(feed(&mut live, &mut r, b"\x07", 40), ["in", "witness"]);
}

/// The reader's rekey keys the chain with the session's shell nonce, so the signed marks after it replay and the witnesses judge faithful.
#[test]
fn a_reader_rekey_carries_the_shell_nonce() {
    let nonce = [0x5Au8; 32];
    let mut live = Terminal::new(6, 40);
    live.authorize_shell_integration(nonce);
    live.set_require_shell_integration_nonce(true);
    let mut rig = ReaderRig::new(live, 4 * MIN_REKEY_BYTES);
    assert_eq!(
        rig.slice(&vec![b'.'; MIN_REKEY_BYTES], 0),
        ["in", "witness", "rekeyframe"],
        "precondition: the burst keyed the chain"
    );
    assert!(
        rig.r
            .keyframes
            .back()
            .is_some_and(|kf| kf.follows && kf.checkpoint.shell_integration_nonce.is_some())
    );
    assert_eq!(rig.slice(signed_marks(nonce).as_bytes(), 10), ["in"]);
    assert_eq!(rig.live.command_marks().len(), 1, "precondition");
    assert_eq!(rig.slice(b"more", 1200), ["in", "witness"]);
    let counts = rig.judge_all();
    assert_eq!((counts.judged, counts.diverged), (2, 0));
}

/// The live keyframe a divergence asks for carries the shell nonce, so the signed marks after it replay and the next witness matches.
#[test]
fn a_live_keyframe_carries_the_shell_nonce() {
    let nonce = [0x5Au8; 32];
    let mut live = Terminal::new(6, 40);
    live.authorize_shell_integration(nonce);
    live.set_require_shell_integration_nonce(true);
    let mut r = TemporalRecorder::new();
    r.record_keyframe(keyframe_of(&live, usize::MAX).expect("at ground"));
    r.seed_spine(SpinePre::take(&live));
    let (tx, rx) = queue(64);
    let mut mark = SpineMark::attach(r.spine_watermark());
    r.spine.keyframe_wanted.store(true, Ordering::Release);
    slice_at(&mut live, &mut mark, &tx, b"x", clock(0));
    assert_eq!(drain_named(&rx, &mut r), ["live-keyframe"]);
    let signed = signed_marks(nonce);
    slice_at(&mut live, &mut mark, &tx, signed.as_bytes(), clock(10));
    slice_at(&mut live, &mut mark, &tx, b"more", clock(1100));
    assert_eq!(drain_named(&rx, &mut r), ["in", "in", "witness"]);
    let fold = r.replay_plan(None).expect("replay").fold();
    r.judge(fold.judged);
    assert_eq!((r.witnessed(), r.diverged()), (1, 0));
}

fn signed_marks(nonce: [u8; 32]) -> String {
    let hex: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "\x1b]133;A;id={hex}\x07$ \x1b]133;B;id={hex}\x07ls\r\n\x1b]133;C;id={hex}\x07\
         out\r\n\x1b]133;D;0;id={hex}\x07"
    )
}

/// A replay keyframe carries the session's shell-integration nonce, so its replay accepts the shell's signed marks as the live engine did; a keyframe without it drops them.
#[test]
fn a_replay_keyframe_carries_the_nonce_its_shell_signs_marks_with() {
    let nonce = [0x5Au8; 32];
    let hex: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
    let mut live = Terminal::new(6, 40);
    live.authorize_shell_integration(nonce);
    live.set_require_shell_integration_nonce(true);
    let signed = format!(
        "\x1b]133;A;id={hex}\x07$ \x1b]133;B;id={hex}\x07ls\r\n\x1b]133;C;id={hex}\x07\
         out\r\n\x1b]133;D;0;id={hex}\x07"
    );
    let carried = keyframe_of(&live, usize::MAX).expect("at ground");
    assert!(carried.shell_integration_nonce.is_some());
    let bare = live.checkpoint();
    live.process_at(signed.as_bytes(), clock(0));
    assert_eq!(
        live.command_marks().len(),
        1,
        "precondition: the mark was accepted"
    );
    for (keyframe, accepted) in [(carried, true), (bare.into(), false)] {
        let mut r = TemporalRecorder::new();
        r.record_keyframe(keyframe);
        r.apply(TemporalMsg::RawIn(InputSlice::at(
            signed.as_bytes(),
            clock(0),
        )));
        let replayed = r.replay_at(None).expect("replay");
        assert_eq!(
            replayed.command_marks().len() == 1,
            accepted,
            "nonce carried: {accepted}"
        );
        assert_eq!(
            replayed.checkpoint_visible() == live.checkpoint_visible(),
            accepted
        );
    }
}

/// A chain is judged with its recorder unlocked, by the idle writer ([`IdleJudge::run`]) and by [`judge_all`] alike: each [`ChainJudge`] restores its base with the lock free, which the replay hook finds, and the idle judge finds it free before every pass; a fold run under the lock finds it held.
#[test]
fn temporal_replay_runs_with_the_recorder_unlocked() {
    use std::cell::RefCell;
    use std::rc::Rc;
    let pool = RewindPool::new(usize::MAX);
    let mut live = Terminal::new(6, 20);
    let recorder = Arc::new(Mutex::new(TemporalRecorder::new()));
    pool.join(1, &recorder);
    {
        let mut rec = recorder.lock().unwrap();
        for chain in 0..3 {
            rec.record_keyframe(keyframe_of(&live, usize::MAX).expect("keyable"));
            for n in 0..2 {
                let line = format!("chain {chain} line {n}\r\n");
                live.process_at(line.as_bytes(), clock(0));
                rec.apply(TemporalMsg::RawIn(InputSlice::at(
                    line.as_bytes(),
                    clock(0),
                )));
                let seen = live.checkpoint_visible().expect("visible");
                rec.apply(TemporalMsg::Witness(Arc::new(seen)));
            }
        }
        assert_eq!(
            (rec.keyframe_count(), rec.pending()),
            (3, 6),
            "fixture: two closed chains and the open one, every witness pending"
        );
    }
    let held = Rc::new(RefCell::new(Vec::new()));
    let _hook = replay_hook::install({
        let (recorder, held) = (recorder.clone(), held.clone());
        move || held.borrow_mut().push(recorder.try_lock().is_err())
    });
    let mut before_pass = Vec::new();
    IdleJudge::default().run(&pool, || {
        before_pass.push(recorder.try_lock().is_err());
        false
    });
    assert_eq!(
        held.take(),
        [false, false],
        "the idle writer restores each closed chain unlocked"
    );
    assert!(
        before_pass.len() >= 3 && before_pass.iter().all(|&locked| !locked),
        "and finds the recorder unlocked before every pass: {before_pass:?}"
    );
    assert_eq!(
        recorder.lock().unwrap().pending(),
        2,
        "the open chain's witnesses wait for a judge on demand"
    );
    assert_eq!(
        judge_all(&recorder),
        JudgeCounts {
            judged: 2,
            diverged: 0,
            pending: 0,
            unjudged_evicted: 0,
        }
    );
    assert_eq!(
        held.take(),
        [false],
        "judge_all restores the open chain unlocked"
    );
    let rec = recorder.lock().unwrap();
    let _ = rec.replay_plan(None).map(ReplayPlan::run);
    assert_eq!(held.take(), [true], "the probe sees a held recorder");
}

/// A replay keyframe carries the live engine's open mode-2026 frame start, drained response rate-limit buckets, alternate-screen leave notes, Kitty ids, host alternate-scroll default and pending restore lineage across initial keyframes, live resyncs and rekeys; a bare checkpoint without the carry diverges on each.
#[test]
fn a_replay_keyframe_carries_the_live_transient_state_across_chains_and_rekeys() {
    let judge_after = |keyframe: ReplayKeyframe,
                       live: &mut Terminal,
                       steps: &[(&[u8], u64)],
                       rekey_before: bool|
     -> (u64, u64) {
        let mut r = TemporalRecorder::with_budget(4 * MIN_REKEY_BYTES);
        r.record_keyframe(keyframe);
        if rekey_before {
            r.seed_spine(SpinePre::take(live));
            let (tx, rx) = queue_on(r.spine_watermark(), 64);
            let mut mark = SpineMark::attach(r.spine_watermark());
            let burst = vec![b'.'; MIN_REKEY_BYTES];
            slice_at(live, &mut mark, &tx, &burst, clock(0));
            assert_eq!(
                drain_named(&rx, &mut r),
                ["in", "witness", "rekeyframe"],
                "precondition: the reader keyed the burst"
            );
        }
        for &(bytes, ms) in steps {
            live.process_at(bytes, clock(ms));
            r.apply(TemporalMsg::RawIn(InputSlice::at(bytes, clock(ms))));
        }
        let seen = live.checkpoint_visible().expect("at ground");
        r.apply(TemporalMsg::Witness(Arc::new(seen)));
        let fold = r.replay_plan(None).expect("replay").fold();
        r.judge(fold.judged);
        (r.witnessed(), r.diverged())
    };

    // 1. Open mode-2026 synchronized-output frame whose 1 s window stays open at +500 ms.
    for carried in [true, false] {
        let mut live = Terminal::new(6, 30);
        live.process_at(b"\x1b[?2026hframe", clock(3_600_000));
        let kf = if carried {
            keyframe_of(&live, usize::MAX).expect("at ground")
        } else {
            live.checkpoint().into()
        };
        assert_eq!(
            judge_after(kf, &mut live, &[(b" still-open", 3_600_500)], false),
            (1, u64::from(!carried)),
            "open sync frame, carried: {carried}"
        );
    }

    // Also across a SpineMark live-keyframe resync while mode 2026 is open (both open and expired).
    {
        let mut live = Terminal::new(6, 30);
        let mut r = TemporalRecorder::new();
        r.record_keyframe(keyframe_of(&live, usize::MAX).expect("at ground"));
        r.seed_spine(SpinePre::take(&live));
        let (tx, rx) = queue(64);
        let mut mark = SpineMark::attach(r.spine_watermark());
        live.process_at(b"\x1b[?2026hopen", clock(3_600_000));
        r.spine.keyframe_wanted.store(true, Ordering::Release);
        slice_at(&mut live, &mut mark, &tx, b"!", clock(3_600_100));
        assert_eq!(drain_named(&rx, &mut r), ["live-keyframe"]);
        slice_at(&mut live, &mut mark, &tx, b"still", clock(3_600_500));
        assert_eq!(drain_named(&rx, &mut r), ["in"]);
        assert!(live.modes().synchronized_output(), "still open at +500 ms");
        r.apply(TemporalMsg::Witness(Arc::new(
            live.checkpoint_visible().expect("at ground"),
        )));
        slice_at(&mut live, &mut mark, &tx, b"expired", clock(3_601_050));
        assert_eq!(drain_named(&rx, &mut r), ["in"]);
        assert!(
            !live.modes().synchronized_output(),
            "expired 1050 ms past open (950 ms past live-keyframe)"
        );
        r.apply(TemporalMsg::Witness(Arc::new(
            live.checkpoint_visible().expect("at ground"),
        )));
        let fold = r.replay_plan(None).expect("replay").fold();
        r.judge(fold.judged);
        assert_eq!(
            (r.witnessed(), r.diverged()),
            (2, 0),
            "live keyframe inside mode 2026 preserves sync_start"
        );
    }

    // 2. Drained response rate-limit bucket (both PolicyEngine standard profile and fallback limiter), including across a reader's rekey.
    for (with_policy, rekey, carried) in [
        (true, false, true),
        (true, false, false),
        (true, true, true),
        (false, false, true),
        (false, false, false),
    ] {
        let mut live = Terminal::new(6, 30);
        if with_policy {
            live.apply_policy_engine(aterm_policy::engine::PolicyEngine::new(
                aterm_policy::profiles::standard(),
            ));
        }
        let mut drain_seq = String::from("\x1b]11;#000000\x07");
        drain_seq.push_str(&"\x1b[5n".repeat(24_592));
        live.process_at(drain_seq.as_bytes(), clock(0));
        let _ = live.take_response();
        let mut probe = "\x1b[5n".repeat(30);
        probe.push_str("\x1b[?2031h\x1b]11;#ffffff\x07");
        let kf = if carried {
            keyframe_of(&live, usize::MAX).expect("at ground")
        } else {
            live.checkpoint().into()
        };
        assert_eq!(
            judge_after(kf, &mut live, &[(probe.as_bytes(), 1)], rekey),
            (1, u64::from(!carried)),
            "drained response bucket (policy={with_policy}, rekey={rekey}, carried={carried})"
        );
    }

    // 3. Alternate screen entered without cursor save (?1047h).
    for carried in [true, false] {
        let mut live = Terminal::new(6, 30);
        live.process_at(b"main\x1b7\x1b[4;10H\x1b[?1047halt", clock(0));
        let kf = if carried {
            keyframe_of(&live, usize::MAX).expect("at ground")
        } else {
            live.checkpoint().into()
        };
        assert_eq!(
            judge_after(kf, &mut live, &[(b"\x1b]133;A\x07$ ls\r\n", 10)], false),
            (1, u64::from(!carried)),
            "1047 enter + orphaned leave without cursor restore, carried: {carried}"
        );
    }

    // 4. Host alternate-scroll default across DECSTR, and pending restore lineage before output.
    for carried in [true, false] {
        let mut live = Terminal::new(6, 30);
        live.set_host_alternate_scroll(true);
        live.process_at(b"\x1b[?1007h", clock(0));
        let kf = if carried {
            keyframe_of(&live, usize::MAX).expect("at ground")
        } else {
            live.checkpoint().into()
        };
        assert_eq!(
            judge_after(kf, &mut live, &[(b"\x1b[!pafter", 10)], false),
            (1, u64::from(!carried)),
            "host_alternate_scroll across DECSTR, carried: {carried}"
        );
    }

    for carried in [true, false] {
        let mut prior = Terminal::new(6, 30);
        prior.process_at(b"prior\r\n", clock(0));
        let seed = prior.checkpoint_continuing().expect("at ground");
        let mut live = Terminal::new(6, 30);
        live.restore_checkpoint(&seed);
        let kf = if carried {
            keyframe_of(&live, usize::MAX).expect("at ground")
        } else {
            live.checkpoint().into()
        };
        assert_eq!(
            judge_after(kf, &mut live, &[(b"more\r\n", 10)], false),
            (1, u64::from(!carried)),
            "pending restore lineage before output, carried: {carried}"
        );
    }
}

fn drain_stamped(rx: &TemporalRx, r: &mut TemporalRecorder) {
    while let Some((generation, msg)) = rx.try_recv_stamped() {
        r.apply_in(generation, msg);
    }
}

fn holds(r: &TemporalRecorder, needle: &[u8]) -> bool {
    r.blobs
        .iter()
        .flatten()
        .any(|s| s.bytes().windows(needle.len()).any(|w| w == needle))
}

#[test]
fn a_forget_keeps_the_screen_and_none_of_what_was_queued_before_it() {
    let mut live = Terminal::new(6, 20);
    let mut r = seeded(&live);
    let (tx, rx) = queue_on(r.spine_watermark(), 64);
    let mut mark = SpineMark::attach(r.spine_watermark());
    slice(&mut live, &mut mark, &tx, b"before\r\n");
    assert!(
        r.forget_onto(&live),
        "a ground screen keys the forgotten recording"
    );
    slice(&mut live, &mut mark, &tx, b"after\r\n");
    drain_stamped(&rx, &mut r);
    assert_eq!(r.keyframe_count(), 1);
    assert!(
        !holds(&r, b"before"),
        "the slice queued before the forget is dropped"
    );
    assert!(holds(&r, b"after"));
    let replayed = r
        .replay_at(None)
        .expect("replay from the forget's keyframe");
    assert_eq!(screen_of(&replayed), screen_of(&live));
    assert_eq!(r.spine_watermark().generation(), 1);
}

#[test]
fn a_forgotten_recording_keeps_nothing_and_counts_on() {
    let mut live = Terminal::new(6, 20);
    let mut r = seeded(&live);
    let (tx, rx) = queue_on(r.spine_watermark(), 64);
    let mut mark = SpineMark::attach(r.spine_watermark());
    slice(&mut live, &mut mark, &tx, b"kept\r\n");
    drain_stamped(&rx, &mut r);
    let total = r.total_events();
    r.forget();
    assert_eq!(
        (r.keyframe_count(), r.live_events(), r.used, r.blobs.len()),
        (0, 0, 0, 0)
    );
    assert!(
        r.replay_at(None).is_none(),
        "nothing before the forget replays"
    );
    assert!(matches!(r.export(), Err(NoExport::NothingReplayable)));
    assert_eq!(r.total_events(), total, "the spine's count goes on");
    slice(&mut live, &mut mark, &tx, b"next\r\n");
    drain_stamped(&rx, &mut r);
    assert_eq!(r.total_events(), total + 1);
}

/// A rekey a reader queued before a forget is dropped when the writer reaches it: it closes no break, leaves no keyframe counted in flight and records no keyframe that follows. A recording forgotten onto the live screen keeps only the forget's keyframe, starts its readers' rekey count afresh from that keyframe, and replays the live screen.
#[test]
fn a_rekeyframe_queued_before_a_forget_is_discarded() {
    let burst = vec![b'x'; MIN_REKEY_BYTES];
    for onto in [false, true] {
        let mut rig = ReaderRig::new(Terminal::new(6, 20), 4 * MIN_REKEY_BYTES);
        rig.queue(&burst, 0);
        let spine = rig.r.spine_watermark();
        assert_eq!(
            spine.keyframes_queued.load(Ordering::SeqCst),
            1,
            "onto={onto}: the rekey is in flight"
        );
        let closed = spine.closed.load(Ordering::Acquire);
        if onto {
            assert!(rig.r.forget_onto(&rig.live));
        } else {
            rig.r.forget();
        }
        assert_eq!(rig.drain(), ["in", "witness", "rekeyframe"]);
        assert_eq!(
            spine.closed.load(Ordering::Acquire),
            closed,
            "onto={onto}: a rekey closes no break"
        );
        assert_eq!(
            spine.keyframes_queued.load(Ordering::SeqCst),
            0,
            "onto={onto}"
        );
        assert_eq!(rig.follows(), 0, "onto={onto}: the stale rekey was dropped");
        if !onto {
            assert_eq!(
                (rig.r.keyframe_count(), rig.r.live_events()),
                (0, 0),
                "the pool's forget appends nothing, so only the generation refuses the rekey"
            );
            assert!(rig.r.replay_at(None).is_none());
            continue;
        }
        assert_eq!(rig.r.keyframe_count(), 1, "only the forget's keyframe");
        assert_eq!(
            (
                spine.rekey_bytes.load(Ordering::Acquire),
                spine.rekey_wanted.load(Ordering::Acquire),
                spine.tail_unwitnessed.load(Ordering::Acquire),
                spine.base_charge.load(Ordering::Acquire),
            ),
            (0, false, false, rig.r.keyframes[0].charge()),
            "the forget's seed starts the readers' rekey count at its keyframe"
        );
        assert_eq!(rig.slice(b"after\r\n", 0), ["in"]);
        rig.assert_faithful("after the forget");
    }
}

/// A recorder of `chains` chains, each a keyframe and `bytes` of output.
fn chained(chains: usize, bytes: usize) -> Arc<Mutex<TemporalRecorder>> {
    let mut r = TemporalRecorder::with_budget(usize::MAX / 4);
    let live = Terminal::new(6, 20);
    for _ in 0..chains {
        r.record_keyframe(live.checkpoint());
        r.record_raw_in(&vec![b'x'; bytes]);
    }
    Arc::new(Mutex::new(r))
}

fn kept(r: &Arc<Mutex<TemporalRecorder>>) -> usize {
    r.lock().unwrap().keyframe_count()
}

#[test]
fn the_pool_sheds_the_pane_focused_longest_ago_first() {
    const CHAIN: usize = 64 * 1024;
    let pool = RewindPool::new(usize::MAX);
    let (a, b) = (chained(3, CHAIN), chained(3, CHAIN));
    pool.join(1, &a);
    pool.join(2, &b);
    let charge = |r: &Arc<Mutex<TemporalRecorder>>| r.lock().unwrap().charge();
    assert_eq!(
        pool.used(),
        charge(&a) + charge(&b),
        "each member's charge counts"
    );
    pool.focus(1);
    pool.set_limit(pool.used() - CHAIN);
    pool.relieve();
    assert_eq!(
        (kept(&a), kept(&b)),
        (3, 1),
        "b, focused longest ago, sheds first"
    );
    assert!(pool.used() <= pool.limit());
    pool.set_limit(CHAIN * 3 / 2);
    pool.relieve();
    assert!(pool.used() <= pool.limit());
    assert_eq!(
        kept(&b),
        0,
        "past every older chain, b's whole history goes"
    );
    assert!(b.lock().unwrap().spine_watermark().keyframe_wanted());
    assert_eq!(kept(&a), 1, "the front pane keeps its newest chain");
    drop(b);
    assert_eq!(
        pool.used(),
        charge(&a),
        "a dropped recorder refunds its charge"
    );
}

#[test]
fn focusing_the_front_pane_again_moves_nothing_until_a_pane_joins() {
    let pool = RewindPool::new(usize::MAX);
    let (a, b) = (chained(1, 16), chained(1, 16));
    pool.join(1, &a);
    pool.focus(1);
    let stamp = |pool: &RewindPool, s: u64| {
        pool.members
            .lock()
            .unwrap()
            .iter()
            .find(|m| m.session == s)
            .map(|m| m.focused)
    };
    let first = stamp(&pool, 1);
    pool.focus(1);
    assert_eq!(
        stamp(&pool, 1),
        first,
        "the same front pane is stamped once"
    );
    pool.join(2, &b);
    pool.focus(1);
    assert!(
        stamp(&pool, 1) > stamp(&pool, 2),
        "a pane that joined behind it is older"
    );
}

#[test]
fn the_shared_writer_writes_every_queue_and_its_drain_says_when() {
    let recorders: Vec<Arc<Mutex<TemporalRecorder>>> = (0..3)
        .map(|_| {
            let r = Arc::new(Mutex::new(TemporalRecorder::new()));
            r.lock()
                .unwrap()
                .record_keyframe(Terminal::new(6, 20).checkpoint());
            r
        })
        .collect();
    let queues: Vec<TemporalTx> = recorders
        .iter()
        .map(|r| open(r, QUEUE_CAP).expect("the shared writer starts"))
        .collect();
    for (n, tx) in queues.iter().enumerate() {
        for _ in 0..=n {
            assert!(tx.send(TemporalMsg::RawIn(InputSlice::at(b"x", clock(0)))));
        }
    }
    for (n, (tx, r)) in queues.iter().zip(&recorders).enumerate() {
        tx.drain().wait();
        assert!(tx.drain().is_finished());
        assert_eq!(
            r.lock().unwrap().live_events(),
            n + 2,
            "keyframe and {} slices",
            n + 1
        );
    }
}

#[test]
fn a_full_queue_refuses_until_the_writer_catches_up() {
    let (tx, rx) = queue(2);
    let msg = || TemporalMsg::RawIn(InputSlice::at(b"x", clock(0)));
    assert!(tx.send(msg()) && tx.send(msg()));
    assert!(!tx.send(msg()), "the third message is past the cap");
    assert!(!tx.drain().is_finished());
    let _ = rx.try_recv();
    assert!(tx.send(msg()), "one written frees one place");
    drop(rx);
    assert!(!tx.send(msg()), "a closed queue takes nothing");
}

#[test]
fn queued_messages_keep_the_recorder_alive_but_a_completed_drain_does_not() {
    let recorder = Arc::new(Mutex::new(TemporalRecorder::new()));
    let weak = Arc::downgrade(&recorder);
    let (mut tx, rx) = queue(2);
    tx.route = Arc::new(QueueRoute {
        to: Some(recorder.clone()),
        state: tx.state.clone(),
        #[cfg(any(debug_assertions, feature = "dev-seams"))]
        costs: tx.spine.costs.handle(),
    });
    let drain = tx.drain();
    assert!(tx.send(TemporalMsg::RawIn(InputSlice::at(b"x", clock(0)))));
    drop(tx);
    drop(recorder);
    assert!(weak.upgrade().is_some(), "queued work owns its recorder");
    let mut queued = rx.rx.try_recv().expect("queued work");
    drop(rx);
    assert!(!drain.is_finished());
    queued.completion.written();
    drop(queued);
    assert!(drain.is_finished());
    assert!(
        weak.upgrade().is_none(),
        "a retained drain must not keep the completed recorder alive"
    );
}

#[test]
fn an_armed_recording_is_picked_up_once_at_its_spine() {
    let live = Terminal::new(6, 20);
    let r = seeded(&live);
    let arm = Arm::default();
    let (tx, _rx) = queue_on(r.spine_watermark(), 8);
    let mut held = None;
    let mut mark = SpineMark::default();
    pick_up(&arm, &mut held, &mut mark);
    assert!(held.is_none(), "nothing armed, nothing picked up");
    arm.set(tx);
    pick_up(&arm, &mut held, &mut mark);
    assert!(held.is_some());
    assert_eq!(
        mark.at,
        r.spine_watermark().load(),
        "the mark starts at the seed"
    );
    assert!(arm.take().is_none(), "an arm is taken once");
}

#[test]
fn a_queue_holds_at_most_its_bytes_and_its_pools_share() {
    let slice = |n: usize| TemporalMsg::RawIn(InputSlice::at(&vec![b'x'; n][..], clock(0)));
    let (tx, rx) = queue_in(Arc::default(), 64, 100, RewindPool::new(usize::MAX));
    assert!(tx.send(slice(60)));
    assert!(!tx.send(slice(60)), "past the queue's bytes");
    assert!(tx.send(slice(40)), "up to them");
    assert!(
        !tx.has_room(),
        "a break keys again only once half its bytes are written"
    );
    let broken = SpineMark {
        broken: true,
        ..SpineMark::default()
    };
    assert!(
        !broken.needs_input(&tx),
        "a full broken queue discards input"
    );
    assert!(
        SpineMark::default().needs_input(&tx),
        "the first overflow still records its gap"
    );
    let key = Arc::new(Terminal::new(6, 20).checkpoint()).into();
    assert!(
        tx.send(TemporalMsg::Keyframe(key)),
        "a keyframe passes the bytes"
    );
    while rx.try_recv().is_some() {}
    assert!(tx.has_room());
    assert!(
        broken.needs_input(&tx),
        "a drained queue can recover from live input"
    );
    let pool = RewindPool::new(4 * MIN_QUEUE_BYTES);
    let (a, _a) = queue_in(Arc::default(), 64, usize::MAX, pool.clone());
    let (b, _b) = queue_in(Arc::default(), 64, usize::MAX, pool.clone());
    assert!(a.send(slice(MIN_QUEUE_BYTES - 10)));
    assert!(
        !b.send(slice(20)),
        "past the pool's share across its queues"
    );
    assert_eq!(
        pool.used(),
        MIN_QUEUE_BYTES - 10,
        "what is queued counts against the pool"
    );
}

#[test]
fn output_queued_onto_a_full_pool_sheds_history_to_make_room() {
    const CHAIN: usize = 64 * 1024;
    let a = chained(3, CHAIN);
    let pool = RewindPool::new(usize::MAX);
    pool.join(1, &a);
    pool.set_limit(pool.used() + CHAIN / 2);
    let (tx, rx) = queue_in(Arc::default(), 64, usize::MAX, pool.clone());
    assert!(tx.send(TemporalMsg::RawIn(InputSlice::at(
        &vec![b'x'; CHAIN][..],
        clock(0)
    ))));
    pool.relieve();
    assert!(kept(&a) < 3, "the queued output made room for itself");
    assert!(pool.used() <= pool.limit());
    let _ = rx.try_recv();
    assert_eq!(
        pool.used(),
        a.lock().unwrap().charge(),
        "a written message is uncounted"
    );
}

#[test]
fn pane_chains_carry_transient_engine_state_and_cell_px_across_keyframes_and_slices() {
    use aterm_aos::contract::Verdict;
    use aterm_aos_pane::PaneCode;
    use aterm_aos_pane::trace::PaneTrace;

    let mut policy = aterm_core::policy::profiles::permissive();
    for rl in &mut policy.rate_limits {
        if rl.id == "response" {
            rl.capacity_bytes = 24;
            rl.refill_per_second = 100;
            rl.per_sequence_max = 0;
        }
    }
    let mut live = Terminal::new(6, 30);
    live.apply_policy_engine(aterm_core::policy::engine::PolicyEngine::new(policy));
    let _ = live.authorize_hyperlink_scheme("aterm-test");
    live.set_host_alternate_scroll_default(false);
    live.set_cell_pixel_size(9, 19);

    // Drain the 24-byte response rate-limit bucket (6 DSRs = 24 B) and open a mode-2026 frame
    // before the seed keyframe.
    live.process_at(
        b"\x1b[5n\x1b[5n\x1b[5n\x1b[5n\x1b[5n\x1b[5n\x1b[?2026h",
        clock(100),
    );
    let _ = (live.take_response(), live.take_reports());

    let mut r = seeded(&live);
    let (tx, rx) = queue(64);
    let mut mark = SpineMark::attach(r.spine_watermark());
    let mut feed = |live: &mut Terminal, r: &mut TemporalRecorder, bytes: &[u8], ms: u64| {
        let pre = SpinePre::take(live);
        live.process_at(bytes, clock(ms));
        let (reports, response) = (live.take_reports(), live.take_response());
        let reply = reply_of(response.as_deref(), reports.as_deref());
        mark.record(
            &tx,
            pre,
            live,
            InputSlice::at(bytes, clock(ms)).replying(reply),
        );
        drain(&rx, r);
    };

    // At 110ms (+1 B refilled < 4 B needed), the 7th DSR is dropped by the carried response
    // bucket and the custom hyperlink scheme is accepted.
    feed(
        &mut live,
        &mut r,
        b"\x1b[5n\x1b]8;;aterm-test://doc\x07link\x1b]8;;\x07",
        110,
    );
    // At 400ms (bucket refilled), CSI 16 t reports 19;9 and mode 2048 enables in-band reports;
    // then the host updates cell pixel size and drains its immediate report as the UI does.
    feed(&mut live, &mut r, b"\x1b[16t\x1b[?2048h", 400);
    live.set_cell_pixel_size(12, 24);
    let _ = live.take_reports();
    // Past the 1s mode-2026 window (100ms + 1150ms > 1s): synchronized output auto-expires,
    // CSI 16 t reports 24;12, and a witness is taken past WITNESS_INTERVAL.
    feed(&mut live, &mut r, b"\x1b[16t!", 1250);

    // Project a LiveKeyframe mid-session and verify the second chain also carries everything.
    r.want_keyframe();
    feed(&mut live, &mut r, b"resync", 1300);
    feed(&mut live, &mut r, b"\x1b[!p\x1b[16t.", 2400);
    feed(&mut live, &mut r, b"done", 3500);

    assert_eq!(r.keyframe_count(), 2);
    for plan in r.chain_plans(r.latest_tick()) {
        r.judge(plan.fold().judged);
    }
    assert_eq!(
        r.diverged(),
        0,
        "in-memory ReplayPlan folds match every witness"
    );
    assert!(r.witnessed() >= 2, "REACH: both chains witnessed");

    let chains = r.pane_chains().expect("chains");
    assert_eq!(chains.len(), 2);
    for (idx, chain) in chains.iter().enumerate() {
        let check = chain.check(PaneCode::ThisBuild, "test");
        assert_eq!(
            check.verdict,
            Verdict::Pass,
            "chain {idx}: {:?}",
            check.verdict
        );
        let bytes = chain
            .trace(PaneCode::ThisBuild, "test")
            .expect("trace builds")
            .encode();
        let decoded = PaneTrace::decode(&bytes).expect("trace decodes");
        let recheck = decoded
            .check(ClockReading::now(), "test")
            .expect("trace checks");
        assert_eq!(
            recheck.verdict,
            Verdict::Pass,
            "decoded chain {idx}: {:?}",
            recheck.verdict
        );
    }
}

/// The main thread's install of `config` on `live`, as `session_theme::install_session_config` does it: the engine journals it, the recorder takes it for later keyframes, and it goes out on `tx` at once.
fn install(
    live: &mut Terminal,
    r: &mut TemporalRecorder,
    tx: &TemporalTx,
    config: &Arc<TerminalConfig>,
) {
    let _ = live.apply_config_shared(config.clone());
    r.set_replay_config(Some(config.clone()));
    SpineMark::attach(r.spine_watermark()).send_host_ops(tx, live);
}

/// A live keyframe in flight when the config moves keeps the config it was projected under; the move follows it on the spine as a host op that asks for no keyframe, so a soft reset after it takes the new config's cursor in a fold and in the pane task.
#[test]
fn an_in_flight_live_keyframe_keeps_its_config_and_the_move_follows_it_as_a_host_op() {
    use aterm_aos::contract::Verdict;
    use aterm_aos_pane::PaneCode;

    let c1 = Arc::new(TerminalConfig {
        cursor_style: aterm_core::terminal::CursorStyle::SteadyBar,
        ..TerminalConfig::default()
    });
    let c2 = Arc::new(TerminalConfig {
        cursor_style: aterm_core::terminal::CursorStyle::SteadyUnderline,
        ..TerminalConfig::default()
    });

    let mut live = Terminal::new(6, 30);
    let _ = live.apply_config(&c1);
    let mut r = TemporalRecorder::new();
    r.set_replay_config(Some(c1.clone()));
    // A pool-forgotten recording: no keyframe kept, one wanted, so the first slice projects one.
    r.forget_for_pool();

    let (tx, rx) = queue(64);
    let mut mark = SpineMark::attach(r.spine_watermark());

    // The reader projects a live keyframe under c1 onto `tx`, not yet drained by the writer.
    slice_at(&mut live, &mut mark, &tx, b"under-c1", clock(0));
    assert!(!r.spine_watermark().keyframe_wanted());

    // The main thread installs c2 before the writer drains `rx`.
    install(&mut live, &mut r, &tx, &c2);
    assert!(
        !r.spine_watermark().keyframe_wanted(),
        "a config move asks the reader for no keyframe"
    );

    slice_at(&mut live, &mut mark, &tx, b"after-c2", clock(100));
    slice_at(&mut live, &mut mark, &tx, b"\x1b[2 qa", clock(200));
    slice_at(&mut live, &mut mark, &tx, b"\x1b[!pb", clock(1300));

    drain(&rx, &mut r);
    assert_eq!(r.keyframe_count(), 1, "the move cost no keyframe");
    assert_eq!(r.keyframes[0].config.as_deref(), Some(&*c1));
    assert!(
        r.host_ops
            .iter()
            .any(|(op, _)| matches!(op, HostOp::Config { config, .. } if *config.0 == *c2)),
        "the move is on the spine"
    );

    let fold = r.replay_plan(None).expect("replay").fold();
    assert_eq!(fold.term.cursor_style(), live.cursor_style());
    r.judge(fold.judged);
    assert_eq!((r.witnessed(), r.diverged()), (1, 0));

    for (idx, chain) in r.pane_chains().expect("chains").iter().enumerate() {
        let check = chain.check(PaneCode::ThisBuild, "test");
        assert_eq!(
            check.verdict,
            Verdict::Pass,
            "chain {idx}: {:?}",
            check.verdict
        );
    }

    // A reader's rekey projected under c1 and still in flight when the config moves keeps c1; the move follows it on the spine as a host op and asks for no keyframe, so a soft reset after it replays to c2's cursor.
    let mut live = Terminal::new(6, 30);
    let _ = live.apply_config(&c1);
    let mut r = TemporalRecorder::with_budget(4 * MIN_REKEY_BYTES);
    r.set_replay_config(Some(c1.clone()));
    r.record_keyframe(keyframe_of(&live, usize::MAX).expect("keyable"));
    let mut rig = ReaderRig::with(live, r);
    rig.queue(&vec![b'.'; MIN_REKEY_BYTES], 0);
    assert_eq!(
        rig.r.spine.keyframes_queued.load(Ordering::SeqCst),
        1,
        "the rekey is in flight"
    );
    install(&mut rig.live, &mut rig.r, &rig.tx, &c2);
    assert!(
        !rig.r.spine.keyframe_wanted(),
        "a config move while a rekey is in flight asks for no keyframe"
    );
    rig.queue(b"\x1b[2 qa", 100);
    rig.queue(b"\x1b[!pb", 200);
    let names: Vec<String> = rig
        .drain()
        .into_iter()
        .map(|name| {
            if name.starts_with("host:") {
                "host".to_string()
            } else {
                name
            }
        })
        .collect();
    assert_eq!(names, ["in", "witness", "rekeyframe", "host", "in", "in"]);
    let configs: Vec<_> = rig
        .r
        .keyframes
        .iter()
        .map(|kf| (kf.follows, kf.config.as_deref() == Some(&*c1)))
        .collect();
    assert_eq!(
        configs,
        [(false, true), (true, true)],
        "the rekey keeps c1, and the move costs no keyframe"
    );
    assert!(
        !rig.r.spine.keyframe_wanted(),
        "the rekey landing under c1 asks for none either: the move follows it"
    );
    assert_eq!(
        rig.live.cursor_style(),
        aterm_core::terminal::CursorStyle::SteadyUnderline,
        "fixture: the soft reset took c2's cursor"
    );
    assert_eq!(
        rig.r.replay_at(None).expect("replay").cursor_style(),
        rig.live.cursor_style(),
        "the replay takes c2's cursor too"
    );
    rig.assert_faithful("across the config move");
}

/// Six theme changes with a slice between each start no keyframe chain and evict none: one keyframe, the oldest instant still reachable, and each change a state of its own. Restoring the keyframe a config move asked for (the negative control, by hand) gives a chain per change.
#[test]
fn a_theme_change_costs_no_keyframe() {
    let theme = |b: u8| {
        Arc::new(TerminalConfig {
            default_background: aterm_core::terminal::Rgb { r: b, g: b, b },
            ..TerminalConfig::default()
        })
    };
    for wanted in [false, true] {
        let mut live = Terminal::new(6, 30);
        let _ = live.apply_config(&theme(0));
        let mut r = seeded(&live);
        r.set_replay_config(Some(theme(0)));
        let oldest = r.reachable_from();
        let (tx, rx) = queue(64);
        let mut mark = SpineMark::attach(r.spine_watermark());
        for i in 1..=6u8 {
            install(&mut live, &mut r, &tx, &theme(i * 30));
            if wanted {
                r.want_keyframe();
            }
            slice_at(&mut live, &mut mark, &tx, b"x", clock(u64::from(i) * 10));
            drain(&rx, &mut r);
        }
        if wanted {
            assert_eq!(
                r.keyframe_count(),
                7,
                "negative control: a chain per change"
            );
            continue;
        }
        assert_eq!(r.keyframe_count(), 1);
        assert_eq!(
            r.reachable_from(),
            oldest,
            "the oldest instant still replays"
        );
        assert_eq!(
            r.used,
            used_from_scratch(&r).0,
            "each kept install charges its config"
        );
        assert!(r.used >= 6 * std::mem::size_of::<TerminalConfig>());
        assert_eq!(
            r.rewind_instants().len(),
            1 + 6 * 2,
            "a state per change and per slice"
        );
        let fold = r.replay_plan(None).expect("replay").fold();
        assert_eq!(fold.term.default_background(), live.default_background());
    }
}

/// A reader's rekey past a theme change records the config in force, so its chain replays a soft reset to the new config's cursor in the pane task; a rekey recorded under the config before the change (the negative control, by hand) fails `WitnessMatches` there, though the GUI fold, whose keyframe carries the cursor default, matches either way.
#[test]
fn a_rekey_past_a_theme_change_replays_under_the_new_config() {
    use aterm_aos::contract::Verdict;
    use aterm_aos_pane::{PaneCode, WITNESS_MATCHES};

    let c1 = Arc::new(TerminalConfig {
        cursor_style: aterm_core::terminal::CursorStyle::SteadyBar,
        ..TerminalConfig::default()
    });
    let c2 = Arc::new(TerminalConfig {
        cursor_style: aterm_core::terminal::CursorStyle::SteadyUnderline,
        ..TerminalConfig::default()
    });
    for keyed_under_old in [false, true] {
        let mut live = Terminal::new(6, 30);
        let _ = live.apply_config(&c1);
        let mut r = TemporalRecorder::new();
        r.set_replay_config(Some(c1.clone()));
        r.record_keyframe(keyframe_of(&live, usize::MAX).expect("keyable"));
        let mut rig = ReaderRig::with(live, r);
        let _ = rig.slice(b"\x1b[2 qa", 0);
        install(&mut rig.live, &mut rig.r, &rig.tx, &c2);
        rig.slice_keyed(b"b", 100);
        let keyed = rig.r.keyframes.back_mut().expect("the rekey");
        assert!(keyed.follows, "REACH: the slice keyed a rekey");
        assert_eq!(keyed.config.as_deref(), Some(&*c2), "the config in force");
        if keyed_under_old {
            keyed.config = Some(c1.clone());
        }
        let _ = rig.slice(b"\x1b[2 qc", 1200);
        let _ = rig.slice(b"\x1b[!pd", 2400);
        let chains = rig.r.pane_chains().expect("chains");
        let check = chains
            .last()
            .expect("the rekey's chain")
            .check(PaneCode::ThisBuild, "test");
        if keyed_under_old {
            assert!(
                matches!(check.verdict, Verdict::Fail { law, .. } if law == WITNESS_MATCHES),
                "negative control: {:?}",
                check.verdict
            );
        } else {
            assert_eq!(check.verdict, Verdict::Pass, "{:?}", check.verdict);
        }
        let fold = rig.r.replay_plan(None).expect("replay").fold();
        assert!(
            !fold.judged.is_empty(),
            "REACH: the soft reset is witnessed"
        );
        rig.r.judge(fold.judged);
        assert_eq!(rig.r.diverged(), 0, "keyed under old: {keyed_under_old}");
    }
}

/// A theme change against a full recording queue opens no break, from the install or from a host flush (the Kitty animation's and the manual reset's path); the reader's next slice carries the change.
#[test]
fn a_theme_change_against_a_full_queue_opens_no_break() {
    let c2 = Arc::new(TerminalConfig {
        default_background: aterm_core::terminal::Rgb {
            r: 240,
            g: 240,
            b: 230,
        },
        ..TerminalConfig::default()
    });
    for flushed in [false, true] {
        let mut live = Terminal::new(6, 30);
        let mut r = seeded(&live);
        let (tx, rx) = queue(2);
        let mut mark = SpineMark::attach(r.spine_watermark());
        assert!(tx.send(TemporalMsg::Reply) && tx.send(TemporalMsg::Reply));
        assert!(!tx.has_room(), "fixture: the queue is full");
        let _ = live.apply_config_shared(c2.clone());
        r.set_replay_config(Some(c2.clone()));
        let mut main = SpineMark::attach(r.spine_watermark());
        if flushed {
            main.flush_host(&tx, &live);
        } else {
            main.send_host_ops(&tx, &live);
        }
        assert!(!r.spine_watermark().break_open(), "flushed: {flushed}");
        assert_eq!(r.dropped_input(), 0);
        drain(&rx, &mut r);
        slice_at(&mut live, &mut mark, &tx, b"next", clock(0));
        drain(&rx, &mut r);
        assert!(
            r.host_ops
                .iter()
                .any(|(op, _)| matches!(op, HostOp::Config { .. })),
            "the next slice carried the change"
        );
        let folded = r.replay_plan(None).expect("replay").run();
        assert_eq!(folded.default_background(), c2.default_background);
    }
}

#[test]
fn foreground_drawer_holder_cuts_and_retires_replay_without_divergence() {
    use aterm_aos::contract::Verdict;
    use aterm_aos_pane::PaneCode;
    use aterm_aos_pane::trace::PaneTrace;
    use aterm_core::terminal::{CutKind, HolderCut};

    for record_cuts in [true, false] {
        let mut live = Terminal::new(8, 40);
        live.process_at(b"shell prompt\r\n", clock(0));
        let mut r = seeded(&live);
        let (tx, rx) = queue(64);
        let mut mark = SpineMark::attach(r.spine_watermark());

        // Drawer token 42 takes foreground, draws on main screen, suspends (`from_alive: true`),
        // resumes, suspends again, and then its group exits (`retire_holder_mark` + `flush_host`).
        live.note_holder_cut(HolderCut {
            to: Some(42),
            kind: CutKind::Group,
            from_alive: true,
        });
        live.set_foreground_job(true);
        if !record_cuts {
            // Negative control: advance the spine mark past the cut so the replay never hears it.
            mark.advance_host((live.resize_lifetime(), live.host_ordinal()));
        }
        slice_at(
            &mut live,
            &mut mark,
            &tx,
            b"drawer line 1\r\ndrawer line 2\r\n",
            clock(100),
        );

        live.note_holder_cut(HolderCut {
            to: None,
            kind: CutKind::Group,
            from_alive: true,
        });
        live.set_foreground_job(false);
        if !record_cuts {
            mark.advance_host((live.resize_lifetime(), live.host_ordinal()));
        }
        slice_at(&mut live, &mut mark, &tx, b"$ fg\r\n", clock(1200));

        live.note_holder_cut(HolderCut {
            to: Some(42),
            kind: CutKind::Group,
            from_alive: true,
        });
        live.set_foreground_job(true);
        if !record_cuts {
            mark.advance_host((live.resize_lifetime(), live.host_ordinal()));
        }
        slice_at(&mut live, &mut mark, &tx, b"drawer line 3\r\n", clock(2400));

        live.note_holder_cut(HolderCut {
            to: None,
            kind: CutKind::Group,
            from_alive: true,
        });
        live.set_foreground_job(false);
        if !record_cuts {
            mark.advance_host((live.resize_lifetime(), live.host_ordinal()));
        }
        slice_at(&mut live, &mut mark, &tx, b"$ kill %1\r\n", clock(3600));

        live.retire_holder_mark(42);
        if record_cuts {
            mark.flush_host(&tx, &live);
        } else {
            mark.advance_host((live.resize_lifetime(), live.host_ordinal()));
        }
        slice_at(&mut live, &mut mark, &tx, b"[1] + killed\r\n", clock(4800));
        drain(&rx, &mut r);

        let fold = r.replay_plan(None).expect("replay").fold();
        r.judge(fold.judged);
        if record_cuts {
            assert_eq!((r.witnessed(), r.diverged()), (4, 0));
            assert_eq!(
                fold.term.checkpoint_visible(),
                live.checkpoint_visible(),
                "holder fence anchors, seals and retirements match live"
            );
            for (idx, chain) in r.pane_chains().expect("chains").iter().enumerate() {
                let check = chain.check(PaneCode::ThisBuild, "test");
                assert_eq!(check.verdict, Verdict::Pass, "chain {idx}: {check:?}");
                let encoded = chain
                    .trace(PaneCode::ThisBuild, "test")
                    .expect("trace")
                    .encode();
                let rechecked = PaneTrace::decode(&encoded)
                    .expect("decode")
                    .check(ClockReading::now(), "test")
                    .expect("check");
                assert_eq!(rechecked.verdict, Verdict::Pass);
            }
        } else {
            assert!(
                r.diverged() >= 1,
                "negative control: omitting HolderCut diverges on holder state"
            );
        }
    }
}

#[test]
fn foreground_job_and_program_probe_cwd_restoration_replay_with_zero_extra_probes() {
    use aterm_aos::contract::Verdict;
    use aterm_aos_pane::PaneCode;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    let nonce = [0x44u8; 32];
    let hex: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
    let probe_calls = Arc::new(AtomicUsize::new(0));
    let child_running = Arc::new(AtomicBool::new(false));

    let mut live = Terminal::new(6, 40);
    live.authorize_shell_integration(nonce);
    live.set_require_shell_integration_nonce(true);
    {
        let calls = probe_calls.clone();
        let running = child_running.clone();
        live.set_program_probe(Some(Box::new(move |_since_ms| {
            calls.fetch_add(1, Ordering::Relaxed);
            running.load(Ordering::Relaxed)
        })));
    }
    let boot = format!(
        "\x1b]133;A;id={hex}\x07$ \x1b]133;B;id={hex}\x07:\r\n\x1b]133;C;id={hex}\x07\x1b]133;D;0;id={hex}\x07"
    );
    let prompt = format!(
        "\x1b]133;A;id={hex}\x07\x1b]633;P;Cwd=/work;id={hex}\x07$ \x1b]133;B;aterm_input=start;id={hex}\x07"
    );
    live.process_at(boot.as_bytes(), clock(0));
    live.process_at(prompt.as_bytes(), clock(10));
    assert_eq!(live.current_working_directory(), Some("/work"));

    let mut r = seeded(&live);
    let (tx, rx) = queue(64);
    let mut mark = SpineMark::attach(r.spine_watermark());

    // 1. Explicit unix foreground job (`set_foreground_job(true)` -> unsigned OSC 7 -> `false`).
    live.set_foreground_job(true);
    slice_at(
        &mut live,
        &mut mark,
        &tx,
        b"\x1b]7;file:///work/sub1\x07",
        clock(100),
    );
    assert_eq!(live.current_working_directory(), Some("/work/sub1"));
    assert_eq!(
        probe_calls.load(Ordering::Relaxed),
        0,
        "explicit foreground_job never calls program_probe"
    );
    live.set_foreground_job(false);
    slice_at(&mut live, &mut mark, &tx, b"back\r\n", clock(1200));
    assert_eq!(live.current_working_directory(), Some("/work"));
    // `set_foreground_job(false)` with `cwd_before_job.is_some()` checks `program_probe` once.
    assert_eq!(probe_calls.load(Ordering::Relaxed), 1);

    // Re-enter fresh signed prompt for the in-group child (`program_probe`) path.
    slice_at(&mut live, &mut mark, &tx, prompt.as_bytes(), clock(1300));
    child_running.store(true, Ordering::Relaxed);
    slice_at(
        &mut live,
        &mut mark,
        &tx,
        b"\x1b]7;file:///work/sub2\x07",
        clock(2400),
    );
    assert_eq!(live.current_working_directory(), Some("/work/sub2"));
    assert!(
        live.input_region_start().is_some(),
        "in-group probe does not set foreground_job on input_region_start"
    );
    // Only the single unsigned OSC 7 called `program_probe` — zero extra probe calls for spine recording.
    assert_eq!(probe_calls.load(Ordering::Relaxed), 2);

    // Child exits; next reader burst calls `set_foreground_job(false)` which probes once and restores `/work`.
    child_running.store(false, Ordering::Relaxed);
    live.set_foreground_job(false);
    assert_eq!(live.current_working_directory(), Some("/work"));
    assert_eq!(probe_calls.load(Ordering::Relaxed), 3);
    slice_at(&mut live, &mut mark, &tx, b"done\r\n", clock(3600));
    drain(&rx, &mut r);

    let fold = r.replay_plan(None).expect("replay").fold();
    r.judge(fold.judged);
    assert_eq!((r.witnessed(), r.diverged()), (3, 0));
    assert_eq!(fold.term.current_working_directory(), Some("/work"));

    for (idx, chain) in r.pane_chains().expect("chains").iter().enumerate() {
        let check = chain.check(PaneCode::ThisBuild, "test");
        assert_eq!(check.verdict, Verdict::Pass, "chain {idx}: {check:?}");
    }
}

#[test]
fn off_reader_screen_cuts_alt_leave_and_shell_rekey_replay_and_step_rewind_instants() {
    use aterm_aos::contract::Verdict;
    use aterm_aos_pane::PaneCode;

    let n1 = [0x11u8; 32];
    let n2 = [0x22u8; 32];
    let n3 = [0x33u8; 32];
    let hex3: String = n3.iter().map(|b| format!("{b:02x}")).collect();

    let mut live = Terminal::new(5, 30);
    live.authorize_shell_integration(n1);
    live.set_require_shell_integration_nonce(true);
    for i in 0..8 {
        live.process_at(format!("hist {i}\r\n").as_bytes(), clock(i * 10));
    }
    let mut r = seeded(&live);
    let (tx, rx) = queue(64);
    let mut mark = SpineMark::attach(r.spine_watermark());

    // Enter alt screen, then force-leave off-reader at t = 200ms.
    r.freeze_now_for_test(Some(Ticks(1_000)));
    slice_at(
        &mut live,
        &mut mark,
        &tx,
        b"\x1b[?1049h\x1b[1;1HALT1",
        clock(100),
    );
    drain(&rx, &mut r);

    r.freeze_now_for_test(Some(Ticks(2_000)));
    assert!(live.leave_alternate_screen_at(clock(200)));
    mark.flush_host(&tx, &live);
    drain(&rx, &mut r);

    // Enter alt screen again, arm `expect_host_alt_screen_leave`, and forward `?1049l`.
    r.freeze_now_for_test(Some(Ticks(3_000)));
    slice_at(
        &mut live,
        &mut mark,
        &tx,
        b"\x1b[?1049h\x1b[1;1HALT2",
        clock(300),
    );
    drain(&rx, &mut r);
    assert!(live.expect_host_alt_screen_leave());
    r.freeze_now_for_test(Some(Ticks(4_000)));
    slice_at(&mut live, &mut mark, &tx, b"\x1b[?1049lMAIN", clock(1500));
    drain(&rx, &mut r);

    // Off-reader `clear_scrollback` at tick 5_000, then typed shell rekey withdraw + settle across a live keyframe.
    r.freeze_now_for_test(Some(Ticks(5_000)));
    live.clear_scrollback();
    mark.flush_host(&tx, &live);
    drain(&rx, &mut r);

    live.authorize_shell_integration_rekey(n2);
    assert!(live.withdraw_shell_integration_rekey(&n2));
    live.authorize_shell_integration_rekey(n3);
    r.want_keyframe();
    r.freeze_now_for_test(Some(Ticks(6_000)));
    slice_at(&mut live, &mut mark, &tx, b"rekey-kf", clock(1600));
    drain(&rx, &mut r);

    assert!(live.settle_shell_integration_rekey(&n3));
    r.freeze_now_for_test(Some(Ticks(7_000)));
    let signed_c =
        format!("\x1b]133;A;id={hex3}\x07$ \x1b]133;B;id={hex3}\x07ls\r\n\x1b]133;C;id={hex3}\x07");
    slice_at(&mut live, &mut mark, &tx, signed_c.as_bytes(), clock(2800));
    drain(&rx, &mut r);

    // Off-reader `reset` at tick 8_000 creates a distinct steppable RewindInstant and clears screen.
    r.freeze_now_for_test(Some(Ticks(8_000)));
    live.reset();
    mark.flush_host(&tx, &live);
    drain(&rx, &mut r);

    r.freeze_now_for_test(Some(Ticks(9_000)));
    slice_at(&mut live, &mut mark, &tx, b"after-reset", clock(4000));
    drain(&rx, &mut r);
    r.freeze_now_for_test(None);

    let instants: Vec<u64> = r.rewind_instants().into_iter().map(|i| i.at.0).collect();
    for expected_cut_tick in [2_000, 5_000, 8_000] {
        assert!(
            instants.contains(&expected_cut_tick),
            "screen-cut HostOp at tick {expected_cut_tick} starts a RewindInstant in {instants:?}"
        );
    }
    let at_reset = r.replay_at(Some(Ticks(8_000))).expect("replay at reset");
    assert!(at_reset.render_row(0).is_empty());

    for plan in r.chain_plans(r.latest_tick()) {
        r.judge(plan.fold().judged);
    }
    assert_eq!(r.diverged(), 0);
    assert!(r.witnessed() >= 2);

    for (idx, chain) in r.pane_chains().expect("chains").iter().enumerate() {
        let check = chain.check(PaneCode::ThisBuild, "test");
        assert_eq!(check.verdict, Verdict::Pass, "chain {idx}: {check:?}");
    }
}

#[test]
fn split_decrqss_and_xtgettcap_across_slices_and_deferred_rekeys_pass_pane_chains() {
    use aterm_aos::contract::Verdict;
    use aterm_aos_pane::PaneCode;

    let mut live = Terminal::new(6, 40);
    let mut r = TemporalRecorder::with_budget(4 * MIN_REKEY_BYTES);
    r.record_keyframe(keyframe_of(&live, usize::MAX).expect("at ground"));
    r.seed_spine(SpinePre::take(&live));
    let (tx, rx) = queue(64);
    let mut mark = SpineMark::attach(r.spine_watermark());
    let step = |live: &mut Terminal, mark: &mut SpineMark, bytes: &[u8], ms: u64| {
        let at = clock(ms);
        let pre = SpinePre::take(live);
        live.process_at(bytes, at);
        let reply = live.take_response().map(Arc::<[u8]>::from);
        mark.record(&tx, pre, live, InputSlice::at(bytes, at).replying(reply));
    };

    // Push past the rekey byte interval and leave the slice inside a DECRQSS body, which a keyframe would swallow (`partial_sequence_swallowed`), so the reader's gate defers the rekey.
    let mut near_rekey = vec![b'a'; MIN_REKEY_BYTES];
    near_rekey.extend_from_slice(b"\x1b[32m\x1bP$q");
    step(&mut live, &mut mark, &near_rekey, 100);
    drain(&rx, &mut r);
    assert_eq!(
        r.keyframe_count(),
        1,
        "a slice inside the DECRQSS body defers the rekey"
    );

    // Advance to ST-pending (`ESC`), still mid-query.
    step(&mut live, &mut mark, b"m\x1b", 200);
    drain(&rx, &mut r);
    assert_eq!(r.keyframe_count(), 1);

    // Complete DECRQSS (`\`), landing at Ground so the reader sends the deferred rekey, then split XTGETTCAP.
    step(&mut live, &mut mark, b"\\ok\r\n", 1300);
    drain(&rx, &mut r);
    assert_eq!(
        r.keyframe_count(),
        2,
        "the reader keyed once Ground returned"
    );

    step(&mut live, &mut mark, b"\x1bP+q54", 1400);
    step(&mut live, &mut mark, b"4E\x1b", 1500);
    step(&mut live, &mut mark, b"\\done\r\n", 2600);
    drain(&rx, &mut r);

    for plan in r.chain_plans(r.latest_tick()) {
        r.judge(plan.fold().judged);
    }
    assert_eq!(r.diverged(), 0);
    assert!(r.witnessed() >= 2);

    let chains = r.pane_chains().expect("chains");
    assert_eq!(chains.len(), 2);
    for (idx, chain) in chains.iter().enumerate() {
        let check = chain.check(PaneCode::ThisBuild, "test");
        assert_eq!(check.verdict, Verdict::Pass, "chain {idx}: {check:?}");
    }
}

#[test]
fn completed_unshared_chains_pack_into_slabs_while_shared_bursts_stay_shared() {
    let mut live = Terminal::new(8, 32);
    let mut r = TemporalRecorder::with_budget(8 * 1024 * 1024);
    r.record_keyframe(keyframe_of(&live, usize::MAX).expect("seed"));

    let shared: Arc<[u8]> = Arc::from(&b"shared-burst\r\n"[..]);
    live.process(&shared);
    r.record_raw_in_shared(shared.clone());

    // Push 100 small unshared slices (4 KiB each = 400 KiB > BLOB_SLAB_BYTES) in chain 0.
    for i in 0..100u32 {
        let mut chunk = format!("line-{i:04}: ").into_bytes();
        chunk.resize(4096, b'x');
        chunk[4094] = b'\r';
        chunk[4095] = b'\n';
        live.process(&chunk);
        r.record_raw_in(&chunk);
    }
    let chain0_end = r.latest_tick();
    let expected_chain0 = screen_of(&live);

    // Record a second keyframe so chain 0 is completed and packed.
    r.record_keyframe(keyframe_of(&live, REKEY_SCROLLBACK_LINES).expect("rekey"));
    assert_eq!(
        Arc::strong_count(&shared),
        2,
        "externally held burst was not copied"
    );
    let retained = r.retained_inputs();
    assert_eq!(retained.len(), 101);
    assert!(
        Arc::ptr_eq(&retained[0].0, &shared),
        "shared slice keeps caller allocation"
    );
    let mut unique_slabs: Vec<*const u8> = retained[1..]
        .iter()
        .map(|(arc, _)| Arc::as_ptr(arc).cast::<u8>())
        .collect();
    unique_slabs.sort_unstable();
    unique_slabs.dedup();
    assert_eq!(
        unique_slabs.len(),
        2,
        "400 KiB of 4 KiB slices packed into two <=256 KiB slabs"
    );
    assert!(
        !r.keyframes[0].packed,
        "chain stays eligible until externally shared burst is dropped"
    );
    let replayed = r.replay_at(Some(chain0_end)).expect("replay packed chain");
    assert_eq!(screen_of(&replayed), expected_chain0);

    drop(retained);
    drop(shared);
    r.record_keyframe(keyframe_of(&live, REKEY_SCROLLBACK_LINES).expect("rekey 2"));
    assert!(
        r.keyframes[0].packed,
        "once shared holder drops, remaining small slice packs and marks chain packed"
    );
    let replayed_again = r
        .replay_at(Some(chain0_end))
        .expect("replay fully packed chain");
    assert_eq!(screen_of(&replayed_again), expected_chain0);
}

#[test]
fn kitty_store_numbers_virtuals_and_chunked_transfers_pass_pane_chains_and_charge_budget() {
    use aterm_aos::contract::Verdict;
    use aterm_aos_pane::PaneCode;

    let mut live = Terminal::new(6, 40);
    live.set_cell_pixel_size(10, 20);
    let mut r = TemporalRecorder::with_budget(4 * MIN_REKEY_BYTES);
    r.record_keyframe(keyframe_of(&live, usize::MAX).expect("at ground"));
    r.seed_spine(SpinePre::take(&live));
    let (tx, rx) = queue(64);
    let mut mark = SpineMark::attach(r.spine_watermark());
    let step = |live: &mut Terminal, mark: &mut SpineMark, bytes: &[u8], ms: u64| {
        let at = clock(ms);
        let pre = SpinePre::take(live);
        live.process_at(bytes, at);
        let reply = live.take_response().map(Arc::<[u8]>::from);
        mark.record(&tx, pre, live, InputSlice::at(bytes, at).replying(reply));
    };

    // Chain 0: store RGBA image 1 (via number 42) + frame 2, virtual placement U=1 + placeholder,
    // open m=1 chunked transfer for image 8, and cross MIN_REKEY_BYTES so the reader keys the chain.
    let mut pre_rekey = b"\x1b_Ga=t,f=32,s=3,v=1,I=42;////AAAA////AAAA\x1b\\\
                          \x1b_Ga=f,f=32,s=3,v=1,i=1,z=50;AAAA////AAAA////\x1b\\\
                          \x1b_Ga=p,U=1,i=1,p=3,c=2,r=1\x1b\\"
        .to_vec();
    pre_rekey
        .extend_from_slice("\x1b[2;1H\x1b[38;5;1m\u{10EEEE}\u{0305}\u{0305}\x1b[39m".as_bytes());
    pre_rekey.extend_from_slice(b"\x1b_Ga=t,f=24,s=1,v=1,i=8,m=1;/w\x1b\\");
    pre_rekey.extend(std::iter::repeat_n(b'x', MIN_REKEY_BYTES));
    pre_rekey.extend_from_slice(b"\r\n");
    step(&mut live, &mut mark, &pre_rekey, 100);
    drain(&rx, &mut r);
    assert_eq!(
        r.keyframe_count(),
        2,
        "the reader's rekey carries the Kitty store"
    );
    assert!(
        r.keyframes[1].carry.retained_image_bytes() >= 24,
        "the rekey retains the Kitty image and chunk bytes"
    );
    // Chain 1 (after the rekey): complete m=0 chunk, place by I=42 and i=8, animate, delete.
    step(
        &mut live,
        &mut mark,
        b"\x1b_Gm=0;AA\x1b\\\
          \x1b[3;1H\x1b_Ga=p,I=42,c=2,r=1\x1b\\\
          \x1b[4;1H\x1b_Ga=p,i=8,c=1,r=1\x1b\\\
          \x1b_Ga=a,i=1,c=2\x1b\\",
        1200,
    );
    step(&mut live, &mut mark, b"\x1b_Ga=d,d=A\x1b\\", 2400);
    drain(&rx, &mut r);

    for plan in r.chain_plans(r.latest_tick()) {
        r.judge(plan.fold().judged);
    }
    assert_eq!(r.diverged(), 0);

    let chains = r.pane_chains().expect("chains");
    assert_eq!(chains.len(), 2);
    for (idx, chain) in chains.iter().enumerate() {
        let check = chain.check(PaneCode::ThisBuild, "test");
        assert_eq!(check.verdict, Verdict::Pass, "chain {idx}: {check:?}");
    }

    // Budget shedding: a keyframe holding Kitty payload bytes counts against the recorder
    // budget and is evicted when a newer keyframe pushes `used` past `budget`.
    let mut img_term = Terminal::new(4, 20);
    let raw = vec![0xABu8; 80 * 30 * 4];
    let b64 = aterm_codec::base64::encode(&raw).expect("base64");
    let cmd = format!("\x1b_Ga=t,f=32,s=80,v=30,i=1;{b64}\x1b\\");
    img_term.process(cmd.as_bytes());
    let kf_heavy = keyframe_of(&img_term, usize::MAX).expect("heavy keyframe");
    let heavy_img_bytes = kf_heavy.carry.retained_image_bytes();
    assert_eq!(heavy_img_bytes, raw.len());

    // The charge holds the payload once: a keyframe of the same engine with the image deleted
    // charges its bytes less, give or take the store's bookkeeping, not twice them or none.
    img_term.process(b"\x1b_Ga=d,d=A\x1b\\");
    let bare = keyframe_of(&img_term, usize::MAX).expect("bare keyframe");
    assert_eq!(
        bare.carry.retained_image_bytes(),
        0,
        "REACH: the image is gone"
    );
    let lighter = kf_heavy.charge().abs_diff(bare.charge());
    assert!(
        kf_heavy.charge() > bare.charge() && lighter.abs_diff(raw.len()) <= 1024,
        "deleting a {} B image takes {lighter} B off the keyframe's charge ({} B to {} B)",
        raw.len(),
        kf_heavy.charge(),
        bare.charge()
    );

    // Store a second 9,600-byte Kitty image on a new keyframe, under a budget each keyframe
    // fits alone and both together exceed.
    let raw2 = vec![0xCDu8; 80 * 30 * 4];
    let b64_2 = aterm_codec::base64::encode(&raw2).expect("base64 2");
    img_term.process(format!("\x1b_Ga=t,f=32,s=80,v=30,i=2;{b64_2}\x1b\\").as_bytes());
    let kf_heavy_2 = keyframe_of(&img_term, usize::MAX).expect("second keyframe");
    assert_eq!(kf_heavy_2.carry.retained_image_bytes(), raw2.len());
    let (one, two) = (kf_heavy.charge(), kf_heavy_2.charge());
    let mut small = TemporalRecorder::with_budget(one.max(two) + one.min(two) / 2);
    small.record_keyframe(kf_heavy);
    assert!(small.used > raw.len());
    assert_eq!(small.keyframe_count(), 1);
    small.record_keyframe(kf_heavy_2);
    assert!(
        small.used <= small.budget,
        "budget shed stale image-bearing keyframe ({} <= {})",
        small.used,
        small.budget
    );
    assert_eq!(small.keyframe_count(), 1);
}

#[test]
fn kitty_file_queries_and_missing_file_transmits_pass_witnesses_and_pane_chains_across_rekeys() {
    use aterm_aos::contract::Verdict;
    use aterm_aos_pane::PaneCode;

    let mut live = Terminal::new(6, 40);
    live.set_kitty_file_resolver(|_, _| None);
    let mut r = TemporalRecorder::with_budget(4 * MIN_REKEY_BYTES);
    r.record_keyframe(keyframe_of(&live, usize::MAX).expect("at ground"));
    r.seed_spine(SpinePre::take(&live));
    let (tx, rx) = queue(64);
    let mut mark = SpineMark::attach(r.spine_watermark());
    let step = |live: &mut Terminal, mark: &mut SpineMark, bytes: &[u8], ms: u64| {
        let at = clock(ms);
        let pre = SpinePre::take(live);
        live.process_at(bytes, at);
        let reply = live.take_response().map(Arc::<[u8]>::from);
        mark.record(&tx, pre, live, InputSlice::at(bytes, at).replying(reply));
    };

    // Chain 0: query `t=f` (`OK`) and cross `MIN_REKEY_BYTES`, so the reader keys a second chain.
    let mut pre_rekey = b"\x1b_Gi=31,a=q,t=f\x1b\\".to_vec();
    pre_rekey.extend(std::iter::repeat_n(b'x', MIN_REKEY_BYTES));
    pre_rekey.extend_from_slice(b"\r\n");
    step(&mut live, &mut mark, &pre_rekey, 100);
    drain(&rx, &mut r);
    assert_eq!(r.keyframe_count(), 2);
    assert!(
        r.keyframes[1].carry.kitty_media_admitted(),
        "the reader's rekey preserves kitty_media_admitted"
    );

    // Chain 1 (after the rekey): query `t=t`/`t=s` (`OK`) and refuse a missing `t=f` (`EINVAL`).
    step(
        &mut live,
        &mut mark,
        b"\x1b_Gi=32,a=q,t=t\x1b\\\
          \x1b_Gi=33,a=q,t=s\x1b\\\
          \x1b_Gi=34,a=T,t=f,f=32,s=1,v=1;L21pc3Npbmc=\x1b\\",
        1200,
    );
    drain(&rx, &mut r);

    let r = Mutex::new(r);
    let counts = judge_all(&r);
    assert_eq!((counts.diverged, counts.pending), (0, 0));
    let r = r.into_inner().unwrap();
    assert_eq!(r.diverged(), 0);

    let chains = r.pane_chains().expect("chains");
    assert_eq!(chains.len(), 2);
    for (idx, chain) in chains.iter().enumerate() {
        let check = chain.check(PaneCode::ThisBuild, "test");
        assert_eq!(check.verdict, Verdict::Pass, "chain {idx}: {check:?}");
    }
}

#[test]
fn successful_kitty_file_loads_and_osc52_queries_project_live_keyframes_with_zero_divergence_and_pass_pane_chains()
 {
    use aterm_aos::contract::Verdict;
    use aterm_aos_pane::{PaneCode, trace::PaneTrace};
    use aterm_core::terminal::{ClipboardAccess, ClipboardOperation};

    let secret_path = "/secret/host/photo.raw";
    let secret_path_b64 = aterm_codec::base64::encode(secret_path.as_bytes()).expect("b64");
    let secret_clip = "secret-clipboard-token";
    let secret_clip_b64 = aterm_codec::base64::encode(secret_clip.as_bytes()).expect("b64");

    let mut live = Terminal::new(6, 40);
    live.set_kitty_file_resolver(|_, path| {
        (path == "/secret/host/photo.raw").then(|| vec![0x11, 0x22, 0x33, 0xff])
    });
    live.authorize_clipboard_access(ClipboardAccess::Query);
    live.set_clipboard_callback(|op| match op {
        ClipboardOperation::Query { .. } => Some("secret-clipboard-token".to_string()),
        _ => None,
    });

    let mut r = TemporalRecorder::with_budget(8 * MIN_REKEY_BYTES);
    r.record_keyframe(keyframe_of(&live, usize::MAX).expect("seed"));
    r.seed_spine(SpinePre::take(&live));
    let (tx, rx) = queue(64);
    let mut mark = SpineMark::attach(r.spine_watermark());
    let step = |live: &mut Terminal, mark: &mut SpineMark, bytes: &[u8], ms: u64| {
        let at = clock(ms);
        let pre = SpinePre::take(live);
        live.process_at(bytes, at);
        let reply = live.take_response().map(Arc::<[u8]>::from);
        mark.record(&tx, pre, live, InputSlice::at(bytes, at).replying(reply));
    };

    step(&mut live, &mut mark, b"before\r\n", 100);
    let load = format!("\x1b_Gi=7,a=T,t=f,f=32,s=1,v=1;{secret_path_b64}\x1b\\");
    step(&mut live, &mut mark, load.as_bytes(), 200);
    step(&mut live, &mut mark, b"\x1b[3;1H\x1b_Gi=7,a=p\x1b\\", 300);
    step(&mut live, &mut mark, b"placed\r\n", 1_400);
    step(&mut live, &mut mark, b"\x1b]52;c;?\x07", 2_000);
    step(&mut live, &mut mark, b"after\r\n", 3_500);
    step(&mut live, &mut mark, b"done\r\n", 4_600);
    drain(&rx, &mut r);

    for plan in r.chain_plans(r.latest_tick()) {
        r.judge(plan.fold().judged);
    }
    assert_eq!((r.witnessed(), r.diverged()), (2, 0));
    let replayed = r.plan(r.latest_tick()).expect("plan").fold().term;
    assert_eq!(replayed.checkpoint_visible(), live.checkpoint_visible());

    let chains = r.pane_chains().expect("chains");
    assert_eq!(chains.len(), 3);
    let lacks = |hay: &[u8], needle: &[u8]| !hay.windows(needle.len()).any(|w| w == needle);
    for (idx, chain) in chains.iter().enumerate() {
        assert_eq!(
            chain.check(PaneCode::ThisBuild, "test").verdict,
            Verdict::Pass,
            "{idx}"
        );
        let bytes = chain
            .trace(PaneCode::ThisBuild, "test")
            .expect("trace")
            .encode();
        for s in [
            secret_path.as_bytes(),
            secret_path_b64.as_bytes(),
            secret_clip.as_bytes(),
            secret_clip_b64.as_bytes(),
        ] {
            assert!(lacks(&bytes, s));
        }
        let round = PaneTrace::decode(&bytes)
            .expect("decode")
            .check(clock(0), "test")
            .expect("check");
        assert_eq!(round.verdict, Verdict::Pass);
    }

    // Negative control: recording `load` as a bare `RawIn` without the LiveKeyframe cut fails `REPLIES_MATCH`.
    let mut unresynced = TemporalRecorder::with_budget(4 * MIN_REKEY_BYTES);
    let mut bare_seed = Terminal::new(6, 40);
    bare_seed.set_kitty_file_resolver(|_, _| None);
    unresynced.record_keyframe(keyframe_of(&bare_seed, usize::MAX).expect("seed"));
    unresynced.apply(TemporalMsg::RawIn(
        InputSlice::at(load.as_bytes(), clock(200))
            .replying(Some(Arc::from(&b"\x1b_Gi=7;OK\x1b\\"[..]))),
    ));
    assert_ne!(
        unresynced.pane_chains().expect("chains")[0]
            .check(PaneCode::ThisBuild, "test")
            .verdict,
        Verdict::Pass
    );
}

#[test]
fn consecutive_kitty_file_loads_coalesce_live_keyframes_and_preserve_earlier_chains() {
    use aterm_aos::contract::Verdict;
    use aterm_aos_pane::{PaneCode, trace::PaneTrace};

    let mut live = Terminal::new(8, 40);
    live.set_kitty_file_resolver(|_, path| {
        path.strip_prefix("/secret/gallery/thumb-")
            .and_then(|n| n.strip_suffix(".raw"))
            .and_then(|n| n.parse::<u8>().ok())
            .map(|k| vec![k, k.wrapping_add(1), k.wrapping_add(2), 0xff])
    });
    let mut r = TemporalRecorder::with_budget(16 * MIN_REKEY_BYTES);
    r.freeze_now_for_test(Some(Ticks(10)));
    r.record_keyframe(keyframe_of(&live, usize::MAX).expect("seed"));
    r.seed_spine(SpinePre::take(&live));
    let (tx, rx) = queue(128);
    let mut mark = SpineMark::attach(r.spine_watermark());
    let mut step = |bytes: &[u8], ms: u64| {
        let pre = SpinePre::take(&live);
        live.process_at(bytes, clock(ms));
        let reply = live.take_response().map(Arc::<[u8]>::from);
        let replied = reply.is_some();
        mark.record(
            &tx,
            pre,
            &live,
            InputSlice::at(bytes, clock(ms)).replying(reply),
        );
        if replied {
            mark.reply(&tx);
        }
        r.freeze_now_for_test(Some(Ticks(ms)));
        drain(&rx, &mut r);
    };

    step(b"pre-gallery shell output\r\n", 100);
    step(b"ready\r\n", 1_200);
    let mut secrets = Vec::new();
    for idx in 1u8..=10 {
        let path = format!("/secret/gallery/thumb-{idx}.raw");
        let path_b64 = aterm_codec::base64::encode(path.as_bytes()).expect("b64");
        let prefix = if idx == 1 {
            "\x1b]133;A\x07$ \x1b]133;B\x07gallery\r\n\x1b]133;C\x07"
        } else {
            ""
        };
        let cmd = format!("{prefix}\x1b_Gi={idx},a=T,t=f,f=32,s=1,v=1;{path_b64}\x1b\\");
        secrets.extend([path.into_bytes(), path_b64.into_bytes()]);
        step(cmd.as_bytes(), 1_200 + u64::from(idx) * 50);
    }
    step(b"gallery complete\r\n", 2_000);
    step(b"done\r\n", 3_100);

    assert_eq!(
        (r.keyframe_count(), r.reachable_from()),
        (2, Some(Ticks(10)))
    );
    let (scratch, per_keyframe, once) = used_from_scratch(&r);
    assert_eq!(
        r.used, scratch,
        "each coalesced keyframe handed its images on"
    );
    assert!(
        once == per_keyframe && once >= 10 * 4,
        "REACH: the one live keyframe holds the ten thumbnails ({once} B)"
    );
    for plan in r.chain_plans(r.latest_tick()) {
        r.judge(plan.fold().judged);
    }
    assert_eq!((r.witnessed(), r.diverged()), (2, 0));
    let replayed = r.plan(r.latest_tick()).expect("plan").fold().term;
    assert_eq!(replayed.checkpoint_visible(), live.checkpoint_visible());
    let pre_gallery = r.plan(Ticks(1_200)).expect("pre-gallery").fold().term;
    assert_eq!(
        pre_gallery.row_text(0).as_deref().map(str::trim_end),
        Some("pre-gallery shell output")
    );
    let inst = |at, command| RewindInstant {
        at: Ticks(at),
        command,
    };
    assert_eq!(
        r.rewind_instants(),
        vec![
            inst(10, false),
            inst(100, false),
            inst(1_200, false),
            inst(1_700, true),
            inst(2_000, false),
            inst(3_100, false),
        ]
    );
    assert!(r.export().is_ok());

    let chains = r.pane_chains().expect("chains");
    assert_eq!(chains.len(), 2);
    for (idx, chain) in chains.iter().enumerate() {
        assert_eq!(
            chain.check(PaneCode::ThisBuild, "test").verdict,
            Verdict::Pass,
            "{idx}"
        );
        let bytes = chain
            .trace(PaneCode::ThisBuild, "test")
            .expect("trace")
            .encode();
        assert!(
            secrets
                .iter()
                .all(|s| !bytes.windows(s.len()).any(|w| w == s.as_slice()))
        );
        let round = PaneTrace::decode(&bytes)
            .expect("decode")
            .check(clock(0), "test")
            .expect("check");
        assert_eq!(round.verdict, Verdict::Pass);
    }
}

/// Back-to-back Kitty file loads that each store a new image 1 coalesce into one live keyframe holding only the newest payload: each keyframe the next one replaced lets go of a payload no kept keyframe holds, so the recording refunds it and charges only the image still kept.
#[test]
fn coalesced_kitty_loads_that_replace_one_image_refund_each_replaced_payload() {
    const FRAME_BYTES: usize = 8 * 8 * 4;
    let mut live = Terminal::new(8, 40);
    live.set_kitty_file_resolver(|_, path| {
        path.strip_prefix("/host/frame-")
            .and_then(|n| n.strip_suffix(".raw"))
            .and_then(|n| n.parse::<u8>().ok())
            .map(|k| vec![k; FRAME_BYTES])
    });
    let mut r = TemporalRecorder::with_budget(16 * MIN_REKEY_BYTES);
    r.freeze_now_for_test(Some(Ticks(10)));
    r.record_keyframe(keyframe_of(&live, usize::MAX).expect("seed"));
    r.seed_spine(SpinePre::take(&live));
    let (tx, rx) = queue(128);
    let mut mark = SpineMark::attach(r.spine_watermark());
    let mut step = |bytes: &[u8], ms: u64| {
        let pre = SpinePre::take(&live);
        live.process_at(bytes, clock(ms));
        let reply = live.take_response().map(Arc::<[u8]>::from);
        let replied = reply.is_some();
        mark.record(
            &tx,
            pre,
            &live,
            InputSlice::at(bytes, clock(ms)).replying(reply),
        );
        if replied {
            mark.reply(&tx);
        }
        r.freeze_now_for_test(Some(Ticks(ms)));
        drain(&rx, &mut r);
    };

    step(b"ready\r\n", 100);
    for idx in 1u8..=5 {
        let path =
            aterm_codec::base64::encode(format!("/host/frame-{idx}.raw").as_bytes()).expect("b64");
        let load = format!("\x1b_Gi=1,a=t,t=f,f=32,s=8,v=8;{path}\x1b\\");
        step(load.as_bytes(), 1_000 + u64::from(idx) * 50);
    }
    step(b"done\r\n", 2_000);

    assert_eq!(
        r.keyframe_count(),
        2,
        "REACH: the seed and the one live keyframe the five loads coalesced into"
    );
    let kept = r.keyframes.back().map(images_of).unwrap_or_default();
    assert!(
        kept.len() == 1 && kept[0].1 == FRAME_BYTES,
        "REACH: the live keyframe holds image 1's newest payload alone: {kept:?}"
    );
    let (scratch, per_keyframe, once) = used_from_scratch(&r);
    assert_eq!(r.used, scratch, "each replaced payload was refunded");
    assert_eq!((per_keyframe, once), (FRAME_BYTES, FRAME_BYTES));
}

/// A 256x512 RGBA raster's bytes (512 KiB): what the host file `/host/screen.raw` holds in [`five_live_read_stretches`].
const SCREEN_IMAGE_BYTES: usize = 256 * 512 * 4;

/// A Kitty file load of `/host/screen.raw` as image 1 under `action`: `T` places it in a 4x2-cell box, `t` only stores it (replacing any image 1).
fn screen_image_load(action: char) -> String {
    let path = aterm_codec::base64::encode(b"/host/screen.raw").expect("b64");
    let place = if action == 'T' { ",c=4,r=2" } else { "" };
    format!("\x1b_Gi=1,a={action},t=f,f=32,s=256,v=512{place};{path}\x1b\\")
}

/// The recording [`five_non_adjacent_live_read_stretches_keep_every_chain_the_budget_holds`] makes under `budget`: `prelude` on the live engine before the seed, a seed and its chain, then each of `reads` (live host reads) followed by output and a witness, so no two live keyframes coalesce and every chain takes a witness, then `coda` as one more slice when it is not empty. Returns the recorder, the live engine, and the recorder's `used` before any fold judged a witness.
fn five_live_read_stretches(
    budget: usize,
    prelude: &[u8],
    reads: &[String],
    coda: &[u8],
) -> (TemporalRecorder, Terminal, usize) {
    use aterm_core::terminal::{ClipboardAccess, ClipboardOperation};

    let mut live = Terminal::new(8, 40);
    live.set_kitty_file_resolver(|_, path| match path {
        "/secret/host/photo.raw" => Some(vec![0x11, 0x22, 0x33, 0xff]),
        "/host/screen.raw" => Some(vec![0x5a; SCREEN_IMAGE_BYTES]),
        _ => None,
    });
    live.authorize_clipboard_access(ClipboardAccess::Query);
    live.set_clipboard_callback(|op| match op {
        ClipboardOperation::Query { .. } => Some("secret-clipboard-token".to_string()),
        _ => None,
    });
    live.process(prelude);
    let mut r = TemporalRecorder::with_budget(budget);
    r.freeze_now_for_test(Some(Ticks(10)));
    r.record_keyframe(keyframe_of(&live, usize::MAX).expect("seed"));
    r.seed_spine(SpinePre::take(&live));
    let (tx, rx) = queue(128);
    let mut mark = SpineMark::attach(r.spine_watermark());
    let mut step = |live: &mut Terminal, r: &mut TemporalRecorder, bytes: &[u8], ms: u64| {
        let pre = SpinePre::take(live);
        live.process_at(bytes, clock(ms));
        let reply = live.take_response().map(Arc::<[u8]>::from);
        let replied = reply.is_some();
        mark.record(
            &tx,
            pre,
            live,
            InputSlice::at(bytes, clock(ms)).replying(reply),
        );
        if replied {
            mark.reply(&tx);
        }
        r.freeze_now_for_test(Some(Ticks(ms)));
        drain(&rx, r);
    };
    step(&mut live, &mut r, b"before every stretch\r\n", 100);
    step(&mut live, &mut r, b"seed chain\r\n", 1_200);
    for (k, read) in (1u64..).zip(reads) {
        let at = 2_000 * k;
        step(&mut live, &mut r, read.as_bytes(), at);
        step(
            &mut live,
            &mut r,
            format!("after stretch {k}\r\n").as_bytes(),
            at + 100,
        );
        step(
            &mut live,
            &mut r,
            format!("witness {k}\r\n").as_bytes(),
            at + 1_200,
        );
    }
    if !coda.is_empty() {
        let at = 2_000 * (reads.len() as u64 + 1);
        step(&mut live, &mut r, coda, at);
    }
    let used = r.used;
    (r, live, used)
}

/// Five runs of live host reads (two Kitty file transmits, two OSC 52 queries, a Kitty file frame load), each with output and a witness between them, project five live keyframes; the recording keeps all six chains while its byte budget holds them, so Rewind reaches the seed. Under half the bytes the oldest chains go first, and the newest still replays the live screen.
#[test]
fn five_non_adjacent_live_read_stretches_keep_every_chain_the_budget_holds() {
    use aterm_aos::contract::Verdict;
    use aterm_aos_pane::{PaneCode, trace::PaneTrace};

    let secret_path = "/secret/host/photo.raw";
    let secret_path_b64 = aterm_codec::base64::encode(secret_path.as_bytes()).expect("b64");
    let secret_clip = "secret-clipboard-token";
    let secret_clip_b64 = aterm_codec::base64::encode(secret_clip.as_bytes()).expect("b64");
    let reads = [
        format!("\x1b_Gi=1,a=T,t=f,f=32,s=1,v=1;{secret_path_b64}\x1b\\"),
        "\x1b]52;c;?\x07".to_string(),
        format!("\x1b_Gi=1,a=f,t=f,f=32,s=1,v=1,z=40;{secret_path_b64}\x1b\\"),
        format!("\x1b_Gi=2,a=T,t=f,f=32,s=1,v=1;{secret_path_b64}\x1b\\"),
        "\x1b]52;c;?\x07".to_string(),
    ];

    let (mut r, live, used) = five_live_read_stretches(DEFAULT_BUDGET_BYTES, b"", &reads, b"");
    assert_eq!(
        live.host_io_ordinal(),
        5,
        "REACH: every read went to the host"
    );
    assert_eq!(r.keyframe_count(), 6, "the seed and five live keyframes");
    assert_eq!(
        r.reachable_from(),
        Some(Ticks(10)),
        "Rewind reaches the seed"
    );
    assert!(used <= r.budget, "used={used} past the budget");
    let before = r.plan(Ticks(100)).expect("the seed's chain").fold().term;
    assert_eq!(
        before.row_text(0).as_deref().map(str::trim_end),
        Some("before every stretch")
    );
    assert_eq!(r.rewind_instants().first().map(|i| i.at), Some(Ticks(10)));
    let ticks: Vec<Ticks> = r.keyframes.iter().map(|kf| kf.ts).collect();
    for plan in r.chain_plans(r.latest_tick()) {
        r.judge(plan.fold().judged);
    }
    assert_eq!(
        (r.witnessed(), r.diverged()),
        (6, 0),
        "every chain's witness matched its replay"
    );
    let latest = r.plan(r.latest_tick()).expect("latest").fold().term;
    assert_eq!(latest.checkpoint_visible(), live.checkpoint_visible());
    let chains = r.pane_chains().expect("chains");
    assert_eq!(chains.len(), 6);
    let lacks = |hay: &[u8], needle: &[u8]| !hay.windows(needle.len()).any(|w| w == needle);
    for (idx, chain) in chains.iter().enumerate() {
        assert_eq!(
            chain.check(PaneCode::ThisBuild, "test").verdict,
            Verdict::Pass,
            "chain {idx}"
        );
        let bytes = chain
            .trace(PaneCode::ThisBuild, "test")
            .expect("trace")
            .encode();
        for secret in [
            secret_path.as_bytes(),
            secret_path_b64.as_bytes(),
            secret_clip.as_bytes(),
            secret_clip_b64.as_bytes(),
        ] {
            assert!(lacks(&bytes, secret), "chain {idx} carries a secret");
        }
        let round = PaneTrace::decode(&bytes)
            .expect("decode")
            .check(clock(0), "test")
            .expect("check");
        assert_eq!(round.verdict, Verdict::Pass, "chain {idx} round trip");
    }
    assert!(r.used <= r.budget);

    // Negative control: half the bytes keep only the newest chains, oldest gone first.
    let (r, live, _) = five_live_read_stretches(used / 2, b"", &reads, b"");
    let kept: Vec<Ticks> = r.keyframes.iter().map(|kf| kf.ts).collect();
    assert!(
        (1..ticks.len()).contains(&kept.len()),
        "half the bytes kept {} of {} keyframes",
        kept.len(),
        ticks.len()
    );
    assert!(ticks.ends_with(&kept), "kept {kept:?} of {ticks:?}");
    assert_eq!(r.reachable_from(), kept.first().copied());
    assert!(kept[0] > Ticks(10), "the seed went first");
    assert!(r.plan(Ticks(100)).is_none(), "the seed's chain is gone");
    let latest = r.plan(r.latest_tick()).expect("latest").fold().term;
    assert_eq!(latest.checkpoint_visible(), live.checkpoint_visible());
    assert!(r.used <= r.budget, "used={} past {}", r.used, r.budget);
}

/// The distinct images `kf` keeps alive, each by the address of its `Arc` and its payload bytes, in address order.
fn images_of(kf: &RetainedKeyframe) -> Vec<(usize, usize)> {
    let mut images = Vec::new();
    kf.carry
        .for_each_retained_image(|img| images.push((Arc::as_ptr(img).addr(), img.payload_bytes())));
    images.sort_unstable();
    images
}

/// `used` recomputed from what `r` keeps: each slice and its reply, each pending witness, each kept host op's config, each keyframe's charge less its images, and each distinct image any kept keyframe holds, once. Also returns the image bytes the keyframes hold summed over them (what charging each keyframe its images whole would count) and the distinct images' bytes. Asserts the ledger counts, for each image, exactly the kept keyframes that hold it, so a keyframe that went without letting go of its images fails here even while another kept keyframe still holds each of them.
fn used_from_scratch(r: &TemporalRecorder) -> (usize, usize, usize) {
    let mut distinct: Vec<(usize, usize)> = Vec::new();
    let mut holders: HashMap<usize, usize> = HashMap::new();
    let (mut own, mut per_keyframe) = (0usize, 0usize);
    for kf in &r.keyframes {
        let images = images_of(kf);
        let held: usize = images.iter().map(|&(_, bytes)| bytes).sum();
        own += kf.charge() - held;
        per_keyframe += held;
        for &(addr, _) in &images {
            *holders.entry(addr).or_insert(0) += 1;
        }
        distinct.extend(images);
    }
    assert_eq!(
        r.images.0, holders,
        "the ledger counts each image's kept holders"
    );
    distinct.sort_unstable();
    distinct.dedup();
    let once: usize = distinct.iter().map(|&(_, bytes)| bytes).sum();
    let blobs: usize = r.blobs.iter().flatten().map(InputSlice::charge).sum();
    let witnesses: usize = r.witnesses.iter().map(Witnessed::charge).sum();
    let host: usize = r.host_ops.iter().map(|(op, _)| host_op_charge(op)).sum();
    (blobs + witnesses + host + own + once, per_keyframe, once)
}

/// One 512 KiB Kitty image on screen while five non-adjacent live clipboard reads and a rekey make seven keyframes that all hold it: the recording charges it once, so a 2 MiB budget keeps every chain (charged to each keyframe, it kept the newest two). A distinct image per read is still charged each, and the budget drops the oldest chains for them.
#[test]
fn a_kitty_image_five_keyframes_share_is_charged_once() {
    const BUDGET: usize = 2 * 1024 * 1024;
    let clipboard_reads = vec!["\x1b]52;c;?\x07".to_string(); 5];
    let mut flood = vec![b'z'; BUDGET / 4];
    flood.extend_from_slice(b"\r\n");
    let (mut r, live, used) = five_live_read_stretches(
        BUDGET,
        screen_image_load('T').as_bytes(),
        &clipboard_reads,
        &flood,
    );
    assert_eq!(
        live.host_io_ordinal(),
        6,
        "REACH: the image load and five clipboard reads went to the host"
    );
    let screen = r.keyframes.front().map(images_of).unwrap_or_default();
    assert!(
        screen.iter().any(|&(_, bytes)| bytes >= SCREEN_IMAGE_BYTES),
        "REACH: the oldest keyframe holds the image: {screen:?}"
    );
    assert_eq!(
        r.keyframe_count(),
        7,
        "the seed, five live keyframes and a rekey"
    );
    assert!(
        r.keyframes.back().is_some_and(|kf| kf.follows),
        "REACH: the flood rekeyed"
    );
    assert_eq!(
        r.reachable_from(),
        Some(Ticks(10)),
        "Rewind reaches the seed"
    );
    assert!(used <= r.budget, "used={used} past {}", r.budget);
    let image = screen
        .iter()
        .find(|&&(_, bytes)| bytes >= SCREEN_IMAGE_BYTES)
        .copied()
        .expect("the image");
    assert!(
        r.keyframes.iter().all(|kf| images_of(kf).contains(&image)),
        "every keyframe, the rekey's included, holds the one image"
    );
    let (scratch, per_keyframe, once) = used_from_scratch(&r);
    eprintln!(
        "shared: {} keyframes, used {} B of {} B, images held {per_keyframe} B, charged {once} B",
        r.keyframe_count(),
        r.used,
        r.budget
    );
    assert_eq!(r.used, scratch, "used is the ledger's sum");
    assert!(
        per_keyframe - once >= 6 * SCREEN_IMAGE_BYTES,
        "the image is charged once, not once per keyframe ({per_keyframe} B held, {once} B charged)"
    );
    let before = r.plan(Ticks(100)).expect("the seed's chain").fold().term;
    assert!(
        (0..8)
            .any(|row| before.row_text(row).as_deref().map(str::trim)
                == Some("before every stretch")),
        "the seed's chain replays its first slice beside the image: {:?}",
        (0..8).map(|row| before.row_text(row)).collect::<Vec<_>>()
    );
    let latest = r.plan(r.latest_tick()).expect("latest").fold().term;
    assert_eq!(latest.checkpoint_visible(), live.checkpoint_visible());
    for plan in r.chain_plans(r.latest_tick()) {
        r.judge(plan.fold().judged);
    }
    assert_eq!(r.diverged(), 0, "every chain's witness matched its replay");
    assert_eq!(
        r.used,
        used_from_scratch(&r).0,
        "judging refunds the witnesses"
    );

    // Negative control: each read stores a new image 1, so every keyframe holds an image of its own, charged each.
    let loads = vec![screen_image_load('t'); 5];
    let (r, live, _) = five_live_read_stretches(BUDGET, b"", &loads, b"");
    assert_eq!(live.host_io_ordinal(), 5, "REACH: five image loads");
    let kept: Vec<Ticks> = r.keyframes.iter().map(|kf| kf.ts).collect();
    let all = [10, 2_000, 4_000, 6_000, 8_000, 10_000].map(Ticks);
    assert!(
        (1..all.len()).contains(&kept.len()),
        "distinct images kept {} of {} keyframes",
        kept.len(),
        all.len()
    );
    assert!(all.ends_with(&kept), "kept {kept:?}, oldest first gone");
    let (scratch, per_keyframe, once) = used_from_scratch(&r);
    eprintln!(
        "distinct: kept {kept:?}, used {} B of {} B, images charged {once} B",
        r.used, r.budget
    );
    assert_eq!(r.used, scratch, "used is the ledger's sum");
    assert_eq!(per_keyframe, once, "no keyframe shares an image");
    assert_eq!(
        once,
        kept.len() * SCREEN_IMAGE_BYTES,
        "REACH: each kept live keyframe holds one image"
    );
    assert!(r.used <= r.budget, "used={} past {}", r.used, r.budget);
    let latest = r.plan(r.latest_tick()).expect("latest").fold().term;
    assert_eq!(latest.checkpoint_visible(), live.checkpoint_visible());
}

/// A keyframe is charged what dropping it frees, within a quarter and a kilobyte over, for a fresh shell, a full history, one dense with shell marks, a styled alternate-screen TUI, a production terminal's policy, a stored Kitty image (whose shared payload the live store keeps) and an in-flight transfer; a witness's snapshot the same. The rule before (a checkpoint's grid bytes) charged less than either frees.
#[test]
fn a_keyframe_charges_the_heap_it_holds() {
    use crate::work_counts::bytes_freed_in;

    let history = |marked: bool| {
        let mut t = Terminal::new(24, 80);
        t.set_scrollback_line_limit(Some(4096));
        for i in 0..2100 {
            if marked && i % 4 == 0 {
                t.process(
                    format!(
                        "\x1b]7;file://localhost/home/user/dir{i}\x07\x1b]133;A\x07$ \
                         \x1b]133;B\x07make target-{i}\r\n\x1b]133;C\x07"
                    )
                    .as_bytes(),
                );
                continue;
            }
            t.process(format!("line {i:05} {}\r\n", "z".repeat(60)).as_bytes());
            if marked && i % 4 == 3 {
                t.process(b"\x1b]133;D;0\x07");
            }
        }
        t
    };
    let tui = || {
        let mut t = Terminal::new(50, 200);
        t.process(b"\x1b[?1049h");
        for row in 1..=50u16 {
            let mut line = format!("\x1b[{row};1H");
            for run in 0..20u16 {
                let colour = (row + run) % 8;
                line.push_str(&format!("\x1b[1;3{colour};4{}m{:<10}", 7 - colour, run));
            }
            line.push_str("\x1b[0m");
            t.process(line.as_bytes());
        }
        t
    };
    let image = |pending: bool| {
        let mut t = Terminal::new(24, 80);
        t.set_cell_pixel_size(10, 20);
        let raw = vec![0x5Au8; 128 * 128 * 4];
        let b64 = aterm_codec::base64::encode(&raw).expect("b64");
        if pending {
            let chunk = &b64[..4096];
            t.process(format!("\x1b_Ga=t,f=32,s=128,v=128,i=2,m=1;{chunk}\x1b\\").as_bytes());
        } else {
            t.process(format!("\x1b_Ga=T,f=32,s=128,v=128,i=1;{b64}\x1b\\").as_bytes());
        }
        t
    };
    let policed = || {
        let mut t = Terminal::new(24, 80);
        t.apply_policy_engine(aterm_policy::engine::PolicyEngine::new(
            aterm_policy::profiles::standard(),
        ));
        t.process(b"$ ls\r\n");
        t
    };
    let fixtures: Vec<(&str, Terminal)> = vec![
        ("fresh 24x80", Terminal::new(24, 80)),
        ("2100 plain lines", history(false)),
        ("2100 lines with shell marks", history(true)),
        ("200x50 styled TUI", tui()),
        ("standard policy", policed()),
        ("64 KiB Kitty image", image(false)),
        ("in-flight m=1 transfer", image(true)),
    ];
    let mut grids_short = 0;
    for (name, live) in &fixtures {
        let kf = keyframe_of(live, REKEY_SCROLLBACK_LINES).expect(name);
        if *name == "2100 lines with shell marks" {
            assert!(
                kf.checkpoint.shell.command_marks.len() >= 500,
                "REACH: {} marks",
                kf.checkpoint.shell.command_marks.len()
            );
        }
        if name.contains("Kitty") {
            assert!(
                kf.carry.retained_image_bytes() >= 128 * 128 * 4,
                "REACH: the store holds the image"
            );
        }
        if name.contains("in-flight") {
            assert!(
                kf.carry.pending_transfer_bytes() > 0,
                "REACH: the transfer is open"
            );
        }
        let shared = kf.carry.retained_image_bytes() - kf.carry.pending_transfer_bytes();
        let charged = kf.charge() - shared;
        let grids = kf.checkpoint.grid.len()
            + kf.checkpoint.alt_grid.as_ref().map_or(0, Vec::len)
            + kf.carry.pending_transfer_bytes();
        let ((), freed) = bytes_freed_in(|| drop(kf));
        eprintln!(
            "keyframe {name}: freed {freed} B, grids-only rule {grids} B, charged {charged} B"
        );
        assert!(
            freed <= charged,
            "{name}: charged {charged} B, freed {freed} B"
        );
        assert!(
            charged <= freed * 5 / 4 + 1024,
            "{name}: charged {charged} B, past freed {freed} B"
        );
        grids_short += usize::from(grids < freed);

        let seen = Arc::new(live.checkpoint_visible().expect(name));
        let charged = charge_of(&seen);
        let grids = seen.grid.len() + seen.alt_grid.as_ref().map_or(0, Vec::len);
        let ((), freed) = bytes_freed_in(|| drop(seen));
        eprintln!(
            "witness {name}: freed {freed} B, grids-only rule {grids} B, charged {charged} B"
        );
        assert!(
            freed <= charged,
            "{name} witness: charged {charged} B, freed {freed} B"
        );
        assert!(
            charged <= freed * 5 / 4 + 1024,
            "{name} witness: charged {charged} B, past freed {freed} B"
        );
    }
    assert_eq!(
        grids_short,
        fixtures.len(),
        "negative control: the grids-only rule charged less than every keyframe frees"
    );
}

/// The table a std `HashMap` allocates is at most what [`aterm_alloc::heap::table_bytes`] names for its capacity, and within two control groups of it, at every size a keyframe's Kitty tables reach.
#[test]
fn table_bytes_names_what_a_hash_map_allocates() {
    use crate::work_counts::bytes_freed_in;
    use std::collections::HashMap;
    fn check<K: std::hash::Hash + Eq, V>(entry: impl Fn(u32) -> (K, V)) {
        for len in 0..600u32 {
            let map: HashMap<K, V> = (0..len).map(&entry).collect();
            let named = aterm_alloc::heap::table_bytes::<(K, V)>(map.capacity());
            let ((), freed) = bytes_freed_in(|| drop(map));
            assert!(
                freed <= named,
                "{len} entries: freed {freed} B, named {named} B"
            );
            assert!(
                named <= freed + 32,
                "{len} entries: named {named} B, freed {freed} B"
            );
        }
    }
    check(|i| (i, i));
    check(|i| (i, u64::from(i)));
    check(|i| (i, [0u8; 3]));
}

/// The sorted keyframe and witness lookups answer what the linear scans they replaced did, at every instant of two thousand chains (ties on a tick, judged, pending and diverged witnesses), for the base keyframe, the fidelity and covering witness, the plan, the instants, the track, the contradictions, the export and the chain plans; a contradiction is where the fidelity reads diverged.
#[test]
fn keyframe_lookups_match_the_linear_scan_across_many_chains() {
    const CHAINS: u64 = 2_000;
    let mut r = TemporalRecorder::with_budget(usize::MAX / 4);
    let mut live = Terminal::new(2, 8);
    live.set_scrollback_line_limit(Some(4));
    r.freeze_now_for_test(Some(Ticks(0)));
    r.record_keyframe(live.checkpoint());
    let mut ms = 0;
    let put = |r: &mut TemporalRecorder, live: &mut Terminal, ms: u64, bytes: &[u8]| {
        r.freeze_now_for_test(Some(Ticks(ms)));
        live.process_at(bytes, clock(ms));
        r.apply(TemporalMsg::RawIn(InputSlice::at(bytes, clock(ms))));
    };
    for chain in 0..CHAINS {
        ms += 10;
        put(&mut r, &mut live, ms, format!("{chain}\r\n").as_bytes());
        let seen = if chain == CHAINS / 2 {
            let mut stranger = Terminal::new(2, 8);
            stranger.process(b"never shown");
            stranger.checkpoint_visible()
        } else {
            live.checkpoint_visible()
        };
        r.apply(TemporalMsg::Witness(Arc::new(seen.expect("visible"))));
        if chain % 3 != 0 {
            ms += 10;
        }
        put(&mut r, &mut live, ms, b"x");
        r.record_keyframe(live.checkpoint());
    }
    ms += 10;
    put(&mut r, &mut live, ms, b"tail");
    assert_eq!(
        r.keyframe_count(),
        usize::try_from(CHAINS).expect("fits") + 1,
        "REACH: every chain is kept"
    );
    let plans = r.chain_plans(r.latest_tick());
    for (idx, plan) in plans.into_iter().enumerate() {
        if idx % 4 != 1 {
            r.judge(plan.fold().judged);
        }
    }
    assert_eq!(r.diverged(), 1, "REACH: one chain's witness diverged");
    assert!(
        r.witnesses
            .iter()
            .any(|w| matches!(w.state, WitnessState::Pending(_))),
        "REACH: some witnesses wait for a fold"
    );

    let ticks: Vec<Ticks> = (0..=ms + 10).step_by(5).map(Ticks).collect();
    let instant_answers = |r: &TemporalRecorder| {
        ticks
            .iter()
            .map(|&at| {
                (
                    r.base_keyframe(at).map(|kf| kf.seq),
                    r.fidelity_at(at),
                    r.covering_witness(at),
                    r.plan(at).map(|p| {
                        (
                            p.base_seq,
                            p.steps.iter().map(|s| s.seq).collect::<Vec<_>>(),
                        )
                    }),
                    r.chain_before(at).map(|p| p.base_seq),
                    r.warm_plans(at)
                        .iter()
                        .map(|p| p.base_seq)
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>()
    };
    let whole_answers = |r: &TemporalRecorder| {
        let instants = r.rewind_instants();
        let contradicted = r.contradicted(&instants);
        let export: Vec<(u64, Option<usize>)> = r
            .export()
            .expect("export")
            .events
            .iter()
            .map(|(t, e)| match e {
                Exported::Screen(cp) => (*t, Some(Arc::as_ptr(cp) as usize)),
                _ => (*t, None),
            })
            .collect();
        let plans: Vec<(Seq, Vec<Seq>)> = r
            .chain_plans(r.latest_tick())
            .iter()
            .map(|p| (p.base_seq, p.steps.iter().map(|s| s.seq).collect()))
            .collect();
        (instants, contradicted, r.rewind_track(), export, plans)
    };
    let (linear_instant, linear_whole) = {
        let _linear = lookup_scan::scan_linearly();
        (instant_answers(&r), whole_answers(&r))
    };
    let sorted_instant = instant_answers(&r);
    let sorted_whole = whole_answers(&r);
    for ((at, linear), sorted) in ticks.iter().zip(&linear_instant).zip(&sorted_instant) {
        assert_eq!(linear, sorted, "at {at:?}");
    }
    assert_eq!(linear_whole, sorted_whole);

    let (instants, contradicted, track, ..) = sorted_whole;
    let read: Vec<bool> = instants
        .iter()
        .map(|i| r.fidelity_at(i.at) == Fidelity::Diverged)
        .collect();
    assert_eq!(contradicted, read, "a contradiction is a diverged fidelity");
    assert!(
        contradicted.iter().any(|&c| c) && contradicted.iter().any(|&c| !c),
        "REACH: the diverged chain is contradicted and the rest are not"
    );
    assert!(
        track.spans.iter().any(|&(.., span)| span == Span::Differs),
        "REACH: the track hatches the diverged chain"
    );
    let fidelities: Vec<Fidelity> = linear_instant.iter().map(|a| a.1).collect();
    for want in [
        Fidelity::Faithful,
        Fidelity::Diverged,
        Fidelity::Unwitnessed,
    ] {
        assert!(
            fidelities.contains(&want),
            "REACH: an instant reads {want:?}"
        );
    }
    assert!(
        r.keyframes
            .iter()
            .zip(r.keyframes.iter().skip(1))
            .any(|(a, b)| r
                .log
                .live_after(a.seq)
                .take_while(|ev| ev.seq < b.seq)
                .any(|ev| ev.ts == b.ts && matches!(ev.op, Op::RawIn(_)))),
        "REACH: a keyframe shares its tick with the slice before it"
    );

    // What a rewound frame pays per `fidelity_at`, sorted against linear, for the report.
    let mid = ticks[ticks.len() / 2];
    let per_call = |r: &TemporalRecorder| {
        let start = Instant::now();
        for _ in 0..200 {
            std::hint::black_box(r.fidelity_at(std::hint::black_box(mid)));
        }
        start.elapsed() / 200
    };
    let linear = {
        let _linear = lookup_scan::scan_linearly();
        per_call(&r)
    };
    let sorted = per_call(&r);
    eprintln!(
        "fidelity_at over {} keyframes and {} witnesses: linear {linear:?}, sorted {sorted:?} per call",
        r.keyframes.len(),
        r.witnesses.len()
    );
}

#[test]
fn deep_unscroll_past_carried_scrollback_projects_live_keyframe_and_hot_tier_images_survive() {
    use aterm_aos::contract::Verdict;
    use aterm_aos_pane::{PaneCode, trace::PaneTrace};

    let mut live = Terminal::new(20, 40);
    live.set_cell_pixel_size(10, 20);
    let mut r = TemporalRecorder::with_budget(80 * MIN_REKEY_BYTES);
    r.record_keyframe(keyframe_of(&live, REKEY_SCROLLBACK_LINES).expect("seed"));
    r.seed_spine(SpinePre::take(&live));
    let (tx, rx) = queue(256);
    let mut mark = SpineMark::attach(r.spine_watermark());
    let step = |live: &mut Terminal, mark: &mut SpineMark, bytes: &[u8], ms: u64| {
        let at = clock(ms);
        let pre = SpinePre::take(live);
        live.process_at(bytes, at);
        let reply = live.take_response().map(Arc::<[u8]>::from);
        mark.record(&tx, pre, live, InputSlice::at(bytes, at).replying(reply));
    };

    let mut bulk = Vec::with_capacity(21 * MIN_REKEY_BYTES);
    for idx in 0..2_219 {
        if idx == 700 {
            bulk.extend_from_slice(b"\x1bP0;0;0q\"1;1;10;20#1;2;100;0;0~\x1b\\");
        }
        bulk.extend_from_slice(format!("line-{idx:04}\r\n").as_bytes());
    }
    bulk.extend_from_slice(b"line-2219");
    bulk.extend(std::iter::repeat_n(0u8, 20 * MIN_REKEY_BYTES));
    step(&mut live, &mut mark, &bulk, 100);
    drain(&rx, &mut r);
    assert_eq!(r.keyframe_count(), 2);
    assert!(
        r.keyframes[1].follows,
        "the bulk crossed the byte interval, so the reader keyed the chain with history bounded to REKEY_SCROLLBACK_LINES"
    );

    // 102 slices * 20 lines = 2,040 lines (<= 2,048 carried): zero extra keyframes, and the
    // hot-tier Sixel image at line-0700 (rev ~ 1,500) unscrolls cleanly across the rekey.
    for s in 1..=102u64 {
        step(&mut live, &mut mark, b"\x1b[20+T", 100 + s * 20);
        drain(&rx, &mut r);
    }
    assert_eq!(r.keyframe_count(), 2);

    // Slice 103 reaches 2,060 > 2,048 lines, projecting one LiveKeyframe and re-seeding the
    // remaining 141 lines (2,200 text lines + 1 Sixel row) so slices 104..=111 replay without
    // further keyframes.
    for s in 103..=111u64 {
        step(&mut live, &mut mark, b"\x1b[20+T", 100 + s * 20);
        drain(&rx, &mut r);
    }
    assert_eq!(r.keyframe_count(), 3);
    for plan in r.chain_plans(r.latest_tick()) {
        r.judge(plan.fold().judged);
    }
    assert_eq!(r.diverged(), 0);
    let replayed = r.plan(r.latest_tick()).expect("plan").fold().term;
    assert_eq!(replayed.checkpoint_visible(), live.checkpoint_visible());
    assert_eq!(
        replayed.row_text(0).as_deref().map(str::trim_end),
        Some("line-0000")
    );

    let chains = r.pane_chains().expect("chains");
    assert_eq!(chains.len(), 3);
    for (idx, chain) in chains.iter().enumerate() {
        assert_eq!(
            chain.check(PaneCode::ThisBuild, "test").verdict,
            Verdict::Pass,
            "{idx}"
        );
        let bytes = chain
            .trace(PaneCode::ThisBuild, "test")
            .expect("trace")
            .encode();
        let round = PaneTrace::decode(&bytes)
            .expect("decode")
            .check(clock(0), "test")
            .expect("check");
        assert_eq!(round.verdict, Verdict::Pass, "{idx}");
    }
}

#[test]
fn deep_unscroll_without_live_keyframe_cut_diverges_negative_control() {
    use aterm_aos::contract::Verdict;
    use aterm_aos_pane::PaneCode;

    let mut bare_live = Terminal::new(20, 40);
    let mut bulk = Vec::with_capacity(64 * 1024);
    for idx in 0..2_219 {
        bulk.extend_from_slice(format!("line-{idx:04}\r\n").as_bytes());
    }
    bulk.extend_from_slice(b"line-2219");
    bare_live.process(&bulk);
    let mut unresynced = TemporalRecorder::with_budget(8 * MIN_REKEY_BYTES);
    unresynced.record_keyframe(keyframe_of(&bare_live, REKEY_SCROLLBACK_LINES).expect("seed"));
    let deep = b"\x1b[20+T".repeat(103);
    bare_live.process_at(&deep, clock(5_000));
    unresynced.apply(TemporalMsg::RawIn(InputSlice::at(&deep, clock(5_000))));
    unresynced.apply(TemporalMsg::Witness(Arc::new(
        bare_live.checkpoint_visible().expect("witness"),
    )));
    let folded = unresynced
        .plan(unresynced.latest_tick())
        .expect("plan")
        .fold();
    unresynced.judge(folded.judged);
    assert_eq!(unresynced.diverged(), 1);
    assert_ne!(
        unresynced.pane_chains().expect("chains")[0]
            .check(PaneCode::ThisBuild, "test")
            .verdict,
        Verdict::Pass
    );
}
