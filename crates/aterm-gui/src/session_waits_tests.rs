// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::io::Read;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_messages::waits::{WAIT_SAMPLE, WAIT_SAMPLE_IDLE};
use aterm_messages::{Intent, PROGRESS_GRACE};
use aterm_session::sink::SinkWriter;

use crate::app_search::FindAction;
use crate::input::{InputEvent, PasteFraming, ScrollIntent, Source};
use crate::message_reporters::{Attention, attention};
use crate::{App, WindowId};

const OPEN: &[u8] = b"\x1b[200~";
const CLOSE: &[u8] = b"\x1b[201~";

fn app_with_reader() -> (App, Arc<SinkWriter>, UnixStream) {
    let (reader, writer) = UnixStream::pair().expect("socketpair");
    reader
        .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .expect("timeout");
    let sink = Arc::new(SinkWriter::new_owned(writer.into()));
    let mut app = App::headless_for_test_with_sink(sink.clone());
    app.windows
        .get_mut(&WindowId(0))
        .expect("window 0")
        .window_on_screen_for_test = true;
    (app, sink, reader)
}

fn paste(app: &mut App, bytes: usize) {
    let text = "p".repeat(bytes);
    app.input(
        WindowId(0),
        InputEvent::Paste(text, PasteFraming::Gesture { bracketed: true }),
        Source::Human,
    );
}

fn drained(sink: &SinkWriter) {
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while sink.ordered_egress_count() != 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(sink.ordered_egress_count(), 0, "the ordered queue drains");
}

#[test]
fn a_small_paste_never_shows_a_row() {
    let (mut app, sink, mut reader) = app_with_reader();
    let small = usize::try_from(aterm_messages::waits::LARGE_PASTE_BYTES).unwrap() - 1;
    paste(&mut app, small);
    assert!(app.session_waits.pastes.is_empty(), "not watched");
    assert_eq!(app.session_waits_deadline(), None, "nothing armed");
    let mut got = vec![0u8; small + OPEN.len() + CLOSE.len()];
    reader.read_exact(&mut got).expect("delivered");
    drained(&sink);
    app.sample_session_waits(Instant::now() + PROGRESS_GRACE * 5);
    assert_eq!(app.messages.live_rows().count(), 0);
}

#[test]
fn a_rewrap_job_ends_on_every_exit() {
    use aterm_messages::WaitEnd;

    use super::RewrapGauge;

    let gauge = Arc::new(RewrapGauge::default());
    let job = gauge.begin_job(1_000);
    assert!(gauge.reading().running);
    job.gauge().pass_done(1_000, None);
    job.finish(WaitEnd::Done);
    let done = gauge.reading();
    assert!(!done.running);
    assert_eq!(
        (done.done, done.total, done.end),
        (1_000, 1_000, Some(WaitEnd::Done))
    );

    let job = gauge.begin_job(500);
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        job.gauge().step(250);
        panic!("finish_resize_offload");
    }));
    assert!(unwound.is_err());
    let lost = gauge.reading();
    assert!(!lost.running, "the unwind ended the hold");
    assert_eq!(lost.end, Some(WaitEnd::Failed));

    let slot = std::sync::Mutex::new(Some(gauge.begin_job(500)));
    assert!(gauge.reading().running);
    drop(slot);
    assert_eq!(gauge.reading().end, Some(WaitEnd::Failed));
    assert_eq!(
        gauge.reading().jobs,
        3,
        "every job counted, however it ended"
    );
}

#[test]
fn a_large_paste_the_sanitizer_empties_ends_with_no_row() {
    let (mut app, sink, _reader) = app_with_reader();
    let big = usize::try_from(aterm_messages::waits::LARGE_PASTE_BYTES).unwrap();
    app.input(
        WindowId(0),
        InputEvent::Paste(
            "\u{1}".repeat(big),
            PasteFraming::Gesture { bracketed: false },
        ),
        Source::Human,
    );
    assert_eq!(app.session_waits.pastes.len(), 1, "watched");
    drained(&sink);
    app.sample_session_waits(Instant::now() + PROGRESS_GRACE * 5);
    assert!(app.session_waits.pastes.is_empty(), "the watch ended");
    assert_eq!(app.session_waits_deadline(), None, "nothing armed");
    assert_eq!(app.messages.live_rows().count(), 0, "no row");
}

#[test]
fn a_large_paste_takes_its_row_and_stop_drops_only_its_rest() {
    let (mut app, sink, mut reader) = app_with_reader();
    let body = 1 << 20;
    paste(&mut app, body);
    app.input(WindowId(0), InputEvent::Text("K".into()), Source::Human);
    assert_eq!(app.session_waits.pastes.len(), 1, "watched");
    let started = app.session_waits.pastes[&0].started;
    let t0 = started + PROGRESS_GRACE / 4;
    assert!(
        app.session_waits_deadline()
            .is_some_and(|d| d <= t0 + WAIT_SAMPLE_IDLE),
        "a sample is armed"
    );
    app.sample_session_waits(t0);
    let live = app.messages.live_rows().next().expect("the paste's row");
    let id = live.id;
    assert_eq!(live.msg.actions, [Intent::StopPaste { session: 0 }]);
    assert_eq!(attention(&live.msg), Ok(Attention::Progress));
    assert_eq!(
        live.msg.reveal_after,
        Some(PROGRESS_GRACE - PROGRESS_GRACE / 4),
        "the grace counts from the paste"
    );
    assert!(
        app.session_waits_deadline()
            .is_some_and(|d| d <= t0 + WAIT_SAMPLE),
        "4 Hz while the row is up"
    );
    let mut got = vec![0u8; 256 * 1024];
    reader.read_exact(&mut got).expect("a prefix");
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    loop {
        app.sample_session_waits(Instant::now());
        let meter = app
            .messages
            .live(id)
            .and_then(|l| l.msg.meter.clone())
            .expect("the meter");
        if meter.fill_permille.is_some_and(|f| f >= 200) {
            break;
        }
        assert!(Instant::now() < deadline, "the meter follows the writer");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(app.perform_intent(WindowId(0), id, Intent::StopPaste { session: 0 }));
    assert!(app.session_waits.pastes.is_empty(), "no longer watched");
    assert_eq!(app.messages.live_rows().count(), 0, "the row fades");
    let mut chunk = [0u8; 8192];
    while !got.ends_with(b"K") {
        let n = reader.read(&mut chunk).expect("the rest");
        assert!(n > 0);
        got.extend_from_slice(&chunk[..n]);
    }
    assert!(got.starts_with(OPEN));
    assert!(
        got.ends_with(&[CLOSE, b"K"].concat()),
        "the paste is closed and the key follows it"
    );
    assert!(
        got.len() < body + OPEN.len() + CLOSE.len(),
        "the rest was dropped"
    );
    drained(&sink);
    let state = app.messages_state();
    assert_eq!(state.entries.len(), 1, "{:?}", state.entries);
    let entry = &state.entries[0];
    assert_eq!(entry.title, super::PASTE_STOPPED);
    assert_eq!(entry.id, id.raw(), "the row's own record");
    assert!(entry.actions.is_empty(), "{:?}", entry.actions);
    assert_eq!(entry.severity, "info");
    assert!(!app.stop_paste(0));
}

#[test]
fn a_delivered_paste_completes_in_its_words() {
    let (mut app, sink, mut reader) = app_with_reader();
    paste(&mut app, 1 << 20);
    let late = Instant::now() + PROGRESS_GRACE * 2;
    app.sample_session_waits(late);
    app.settle_messages(late);
    let drain = std::thread::spawn(move || {
        let mut sink_bytes = Vec::new();
        let mut chunk = [0u8; 65536];
        while !sink_bytes.ends_with(CLOSE) {
            let n = reader.read(&mut chunk).expect("read");
            assert!(n > 0);
            sink_bytes.extend_from_slice(&chunk[..n]);
        }
        sink_bytes.len()
    });
    assert_eq!(
        drain.join().expect("drain"),
        (1 << 20) + OPEN.len() + CLOSE.len()
    );
    drained(&sink);
    app.sample_session_waits(late + WAIT_SAMPLE);
    assert!(app.session_waits.pastes.is_empty());
    assert_eq!(app.session_waits_deadline(), None, "idle again");
    let state = app.messages_state();
    let entry = &state.entries[0];
    assert_eq!(entry.severity, "success");
    assert!(entry.actions.is_empty(), "{:?}", entry.actions);
}

#[test]
fn a_rewrap_takes_a_row_only_when_the_person_reaches_for_history() {
    let (mut app, _sink, _reader) = app_with_reader();
    let gauge = app.pool.get(0).expect("session 0").ctx.rewrap_gauge.clone();
    gauge.begin(3_400_000);
    gauge.step(1_200_000);
    app.sample_session_waits(Instant::now());
    assert!(app.session_waits.rewraps.is_empty());
    assert_eq!(app.session_waits_deadline(), None, "nothing armed");
    app.input(
        WindowId(0),
        InputEvent::ScrollView(ScrollIntent::Down),
        Source::Human,
    );
    assert!(app.session_waits.rewraps.is_empty());
    gauge.pass_done(3_400_000, None);
    gauge.end(aterm_messages::WaitEnd::Done);
    assert_eq!(app.messages.live_rows().count(), 0);

    gauge.begin(3_400_000);
    gauge.step(1_200_000);
    app.input(
        WindowId(0),
        InputEvent::ScrollView(ScrollIntent::Up),
        Source::Human,
    );
    let live = app.messages.live_rows().next().expect("the rewrap's row");
    let id = live.id;
    assert_eq!(attention(&live.msg), Ok(Attention::Progress));
    assert!(live.msg.actions.is_empty(), "nothing to decide");
    assert!(
        live.msg
            .reveal_after
            .is_some_and(|d| d > PROGRESS_GRACE / 2),
        "the grace counts from the ask"
    );
    assert!(app.session_waits_deadline().is_some());
    gauge.step(2_000_000);
    let later = Instant::now() + PROGRESS_GRACE * 2;
    app.sample_session_waits(later);
    app.settle_messages(later);
    assert!(app.messages.live(id).is_some(), "the row is still up");
    gauge.pass_done(3_400_000, None);
    gauge.end(aterm_messages::WaitEnd::Done);
    app.sample_session_waits(later + WAIT_SAMPLE);
    assert!(app.session_waits.rewraps.is_empty());
    assert_eq!(app.session_waits_deadline(), None);
}

#[test]
fn a_search_during_a_rewrap_says_partial_history_and_runs_again() {
    let (mut app, _sink, _reader) = app_with_reader();
    app.frontmost_window = Some(WindowId(0));
    let gauge = app.pool.get(0).expect("session 0").ctx.rewrap_gauge.clone();
    gauge.begin(500_000);
    app.find_cmd(FindAction::Open);
    let status = app
        .find_cmd(FindAction::Type("needle".into()))
        .expect("status");
    assert_eq!(status.matches, 0);
    assert!(status.truncated, "a floor, not a verdict");
    assert_eq!(
        app.messages.live_rows().count(),
        1,
        "the person searching waits on the rewrap"
    );
    gauge.pass_done(500_000, None);
    gauge.end(aterm_messages::WaitEnd::Done);
    app.sample_session_waits(Instant::now() + WAIT_SAMPLE);
    let status = app.find_cmd(FindAction::Status).expect("status");
    assert!(!status.truncated, "the whole history was searched again");
    assert!(
        !app.windows[&WindowId(0)]
            .search
            .as_ref()
            .expect("search")
            .history_away,
        "the history is back"
    );
    let (mut plain, _s, _r) = app_with_reader();
    plain.frontmost_window = Some(WindowId(0));
    plain.find_cmd(FindAction::Open);
    let status = plain
        .find_cmd(FindAction::Type("needle".into()))
        .expect("status");
    assert!(!status.truncated);
    assert_eq!(plain.messages.live_rows().count(), 0);
}

#[test]
fn a_torn_rerun_after_the_rewrap_does_not_keep_saying_rewrapping() {
    let (mut app, _sink, _reader) = app_with_reader();
    app.frontmost_window = Some(WindowId(0));
    let gauge = app.pool.get(0).expect("session 0").ctx.rewrap_gauge.clone();
    gauge.begin(500_000);
    app.find_cmd(FindAction::Open);
    app.find_cmd(FindAction::Type("needle".into()))
        .expect("status");
    assert!(
        app.windows[&WindowId(0)]
            .search
            .as_ref()
            .expect("search")
            .history_away,
        "fixture: the search ran while the history was away"
    );
    crate::app_search::FORCE_TORN_SEARCH.with(|torn| torn.set(true));
    gauge.pass_done(500_000, None);
    gauge.end(aterm_messages::WaitEnd::Done);
    app.sample_session_waits(Instant::now() + WAIT_SAMPLE);
    crate::app_search::FORCE_TORN_SEARCH.with(|torn| torn.set(false));
    let search = app.windows[&WindowId(0)].search.as_ref().expect("search");
    assert!(search.results_dirty, "fixture: the rerun came back torn");
    assert!(!search.history_away, "the history is home");
    assert!(search.truncated, "the kept result is still a floor");
}

/// A torn rerun keeps no match to highlight, but the find bar's words moved (stale, and no
/// longer the history's absence): the rerun asks its own window for a frame, and the end of
/// the rewrap's row asks no other window for one.
#[test]
fn a_torn_rerun_after_the_rewrap_asks_its_window_for_a_frame() {
    let (mut app, _sink, _reader) = app_with_reader();
    let wid = WindowId(0);
    app.frontmost_window = Some(wid);
    let gauge = app.pool.get(0).expect("session 0").ctx.rewrap_gauge.clone();
    gauge.begin(500_000);
    app.find_cmd(FindAction::Open);
    app.find_cmd(FindAction::Type("needle".into()))
        .expect("status");
    assert_eq!(
        app.messages.live_rows().count(),
        1,
        "fixture: the person searching waits on the rewrap"
    );
    let own = app.windows[&wid].redraw_requests_for_test.get();
    let every = crate::redraw_all_requests_on_this_thread();
    crate::app_search::FORCE_TORN_SEARCH.with(|torn| torn.set(true));
    gauge.pass_done(500_000, None);
    gauge.end(aterm_messages::WaitEnd::Done);
    app.sample_session_waits(Instant::now() + WAIT_SAMPLE);
    crate::app_search::FORCE_TORN_SEARCH.with(|torn| torn.set(false));
    assert!(
        app.windows[&wid]
            .search
            .as_ref()
            .expect("search")
            .results_dirty,
        "fixture: the rerun came back torn"
    );
    assert_eq!(
        app.messages.live_rows().count(),
        0,
        "fixture: the rewrap's row ended"
    );
    assert!(
        app.windows[&wid].redraw_requests_for_test.get() > own,
        "the torn rerun asks its window for a frame"
    );
    assert_eq!(
        crate::redraw_all_requests_on_this_thread(),
        every,
        "the row's end asks every window for no frame"
    );
}

/// A paste wait restates its row every [`WAIT_SAMPLE`] while the paste moves, and ends it when
/// the paste is delivered. No window draws the row, so neither asks a window for a frame: not
/// the paste's own window, and not every window at once. No Settings view is open, so no
/// publish repaints one.
#[test]
fn a_paste_waits_restates_and_end_ask_no_window_for_a_frame() {
    let (mut app, sink, mut reader) = app_with_reader();
    let body = 1 << 20;
    paste(&mut app, body);
    let started = app.session_waits.pastes[&0].started;
    let mut at = started + PROGRESS_GRACE / 4;
    app.sample_session_waits(at);
    let id = app.messages.live_rows().next().expect("the paste's row").id;
    let asks = crate::frame_asks_for_test(&app);
    let (mut samples, mut restates, mut read) = (0_u32, 0_u32, 0_usize);
    let mut chunk = vec![0u8; 16 * 1024];
    let hang = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while restates < 16 {
        assert!(
            Instant::now() < hang,
            "the paste restated its row 16 times in a minute ({restates} so far)"
        );
        reader
            .read_exact(&mut chunk)
            .expect("the paste's next bytes");
        read += chunk.len();
        let revision = app.messages.revision();
        at += WAIT_SAMPLE;
        app.sample_session_waits(at);
        samples += 1;
        restates += u32::from(app.messages.revision() != revision);
    }
    assert!(
        app.messages.live(id).is_some(),
        "PRECONDITION: the paste is still under way"
    );
    let asked = crate::frame_asks_for_test(&app) - asks;
    let waited = WAIT_SAMPLE * samples;
    assert_eq!(
        asked, 0,
        "{restates} restatements over {samples} samples ({waited:?} of the wait at one \
         sample per {WAIT_SAMPLE:?}) asked the windows for {asked} frames"
    );
    let rest = body + OPEN.len() + CLOSE.len() - read;
    let drain = std::thread::spawn(move || {
        let mut left = vec![0u8; rest];
        reader.read_exact(&mut left).expect("the paste's rest");
        left.ends_with(CLOSE)
    });
    assert!(drain.join().expect("drain"), "the paste is closed");
    drained(&sink);
    let hang = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while !app.session_waits.pastes.is_empty() {
        assert!(Instant::now() < hang, "the delivered paste ends its wait");
        std::thread::sleep(Duration::from_millis(1));
        at += WAIT_SAMPLE;
        app.sample_session_waits(at);
    }
    assert!(app.messages.live(id).is_none(), "the row ended");
    assert_eq!(
        crate::frame_asks_for_test(&app),
        asks,
        "the end asks no window for a frame"
    );
}

#[test]
fn a_glide_across_a_rewrap_lands_on_its_target_not_the_sum_of_its_ticks() {
    let (mut app, _sink, _reader) = app_with_reader();
    let wid = WindowId(0);
    let (term, pending) = rewrapping_history(&mut app, 500);
    reattach(&mut app, &term, pending);
    let cols = crate::term_lock(&term).cols();
    crate::term_lock(&term).scroll_display(3);
    app.scroll_wheel_animated(wid, &term, 6);
    let end = app.windows[&wid]
        .scroll_glide
        .as_ref()
        .expect("a glide")
        .glide
        .end();
    app.tick_scroll_glide(wid, end - Duration::from_millis(150));
    let mid = crate::term_lock(&term).grid().display_offset();
    assert!((4..9).contains(&mid), "fixture: mid-ease ({mid})");
    let ws = app.windows.get_mut(&wid).expect("window");
    ws.cols = cols - 10;
    app.resize_panes(wid);
    assert!(
        app.windows[&wid].scroll_glide.is_none(),
        "the glide landed before the detach"
    );
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while crate::term_lock(&term).grid().reflow_offload_in_flight() {
        assert!(Instant::now() < deadline, "the rewrap re-attaches");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        crate::term_lock(&term).grid().display_offset(),
        9,
        "the re-attach restores the row the glide was going to"
    );

    let (mut app, _sink, _reader) = app_with_reader();
    let (term, pending) = rewrapping_history(&mut app, 500);
    reattach(&mut app, &term, pending);
    crate::term_lock(&term).scroll_display(3);
    app.scroll_wheel_animated(wid, &term, 6);
    let end = app.windows[&wid]
        .scroll_glide
        .as_ref()
        .expect("a glide")
        .glide
        .end();
    app.tick_scroll_glide(wid, end - Duration::from_millis(150));
    let restored = crate::term_lock(&term).grid().display_offset();
    let pending = {
        let mut t = crate::term_lock(&term);
        let (rows, cols) = (t.rows(), t.cols());
        t.resize_offloading_scrollback(rows, cols - 10)
            .expect("a width change detaches the tiered history")
    };
    for ms in [140, 120, 100, 80, 60, 40, 20, 0] {
        app.tick_scroll_glide(wid, end - Duration::from_millis(ms));
    }
    app.tick_scroll_glide(wid, end + Duration::from_millis(1));
    assert!(app.windows[&wid].scroll_glide.is_none(), "the glide ended");
    let next = crate::term_lock(&term).finish_resize_offload(pending.reflow());
    assert!(next.is_none(), "one pass");
    assert_eq!(
        crate::term_lock(&term).grid().display_offset(),
        restored,
        "the restore stands: the glide's wakes compounded no aim"
    );
}

#[test]
fn a_trackpad_swipe_during_a_rewrap_moves_the_view_by_the_finger_once() {
    use aterm_window::dpi::PhysicalPosition;
    use aterm_window::event::{MouseScrollDelta, TouchPhase};

    let (mut app, _sink, _reader) = app_with_reader();
    let wid = WindowId(0);
    let (term, pending) = rewrapping_history(&mut app, 500);
    {
        let mut t = crate::term_lock(&term);
        for i in 0..60 {
            t.process(format!("W{i}\r\n").as_bytes());
        }
        assert!(
            t.grid().scrollback_lines() > 8,
            "fixture: output attached rows during the window"
        );
        assert_eq!(t.grid().display_offset(), 0);
    }
    let cell_h = app.win_cell_size(wid).1.max(1) as f64;
    let swipe = |dy: f64| MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, dy));
    let before = crate::term_lock_acquisitions_on_this_thread();
    app.on_mouse_wheel_phased(wid, swipe(cell_h * 0.3), TouchPhase::Moved);
    assert_eq!(
        crate::term_lock_acquisitions_on_this_thread() - before,
        0,
        "a sub-row delta during the rewrap takes no engine lock"
    );
    for _ in 1..11 {
        app.on_mouse_wheel_phased(wid, swipe(cell_h * 0.3), TouchPhase::Moved);
        assert!(
            app.windows[&wid].scroll_glide.is_none(),
            "no band over a history that is away"
        );
    }
    app.on_mouse_wheel_phased(wid, swipe(0.0), TouchPhase::Ended);
    let now = Instant::now();
    for ms in [20, 60, 120, 240, 480] {
        app.service_due_scroll_motion(now + Duration::from_millis(ms));
    }
    assert_eq!(
        crate::term_lock(&term).grid().display_offset(),
        3,
        "the banked rows, applied once"
    );
    assert!(!crate::term_lock(&term).grid().reader_aim_held());
    reattach(&mut app, &term, pending);
}

#[test]
fn completed_paste_meters_release_during_a_continuous_burst_without_losing_totals() {
    use aterm_session::sink::BulkMeter;

    let (mut app, sink, _reader) = app_with_reader();
    let now = Instant::now();
    let outstanding = Arc::new(BulkMeter::new());
    app.watch_paste(0, &sink, outstanding.clone(), 100, now);
    let mut expected_sent = 0;
    let mut expected_total = 100;
    for index in 0usize..256 {
        let meter = Arc::new(BulkMeter::new());
        app.watch_paste(0, &sink, meter.clone(), 100, now);
        if index.is_multiple_of(2) {
            meter.settle_unmetered(true);
            expected_sent += 100;
        } else {
            meter.abandon();
        }
        expected_total += 100;
        let released = Arc::downgrade(&meter);
        drop(meter);
        app.sample_session_waits(now);
        assert!(
            released.upgrade().is_none(),
            "completed meter {index} was retained"
        );
        let watch = &app.session_waits.pastes[&0];
        assert_eq!(watch.meters.len(), 1, "only unfinished work stays resident");
        assert!(Arc::ptr_eq(&watch.meters[0].0, &outstanding));
        assert_eq!(watch.amounts(), (expected_sent, expected_total));
        assert!(!watch.completed_failed);
    }

    let failed = Arc::new(BulkMeter::new());
    app.watch_paste(0, &sink, failed.clone(), 200, now);
    failed.settle_unmetered(false);
    let released = Arc::downgrade(&failed);
    drop(failed);
    let later = Arc::new(BulkMeter::new());
    app.watch_paste(0, &sink, later.clone(), 300, now);
    let watch = &app.session_waits.pastes[&0];
    assert!(released.upgrade().is_none());
    assert_eq!(watch.meters.len(), 2);
    assert!(
        watch.completed_failed,
        "compaction must retain the failed verdict"
    );
    assert_eq!(watch.amounts(), (expected_sent, expected_total + 500));
    app.sample_session_waits(now);
    assert!(!app.session_waits.pastes.contains_key(&0));
    assert_eq!(Arc::strong_count(&outstanding), 1);
    assert_eq!(Arc::strong_count(&later), 1);
}

#[test]
fn a_paste_whose_rest_a_discard_dropped_claims_no_delivery() {
    use aterm_session::sink::BulkMeter;

    let (mut app, sink, _reader) = app_with_reader();
    let t0 = Instant::now();
    let meter = Arc::new(BulkMeter::new());
    app.watch_paste(0, &sink, meter.clone(), 1 << 20, t0);
    let late = t0 + PROGRESS_GRACE * 2;
    app.sample_session_waits(late);
    app.settle_messages(late);
    meter.abandon();
    app.sample_session_waits(late + WAIT_SAMPLE);
    assert!(app.session_waits.pastes.is_empty(), "the watch ended");
    let state = app.messages_state();
    let entry = &state.entries[0];
    assert_eq!(entry.title, super::PASTE_STOPPED, "{entry:?}");
    assert_ne!(entry.severity, "success", "{entry:?}");
}

fn rewrapping_history(
    app: &mut App,
    n: usize,
) -> (
    Arc<std::sync::Mutex<aterm_core::terminal::Terminal>>,
    aterm_grid::PendingScrollbackReflow,
) {
    let term = app.pool.get(0).expect("session 0").term.clone();
    let pending = {
        let mut t = crate::term_lock(&term);
        let (rows, cols) = (t.rows(), t.cols());
        *t.grid_mut() = aterm_grid::Grid::with_tiered_scrollback(
            rows,
            cols,
            8,
            aterm_scrollback::ScrollbackStorage::from(aterm_scrollback::Scrollback::new(
                64, 512, 8_000_000,
            )),
        );
        let mut buf = Vec::new();
        for i in 0..n {
            buf.extend_from_slice(format!("H{i}\r\n").as_bytes());
        }
        buf.resize(
            buf.len() + usize::from(cols) * (5 * usize::from(rows) + 8 + 6),
            b'y',
        );
        buf.extend_from_slice(b"\r\n");
        t.process(&buf);
        t.resize_offloading_scrollback(rows, cols - 10)
            .expect("a width change detaches the tiered history")
    };
    let gauge = app.pool.get(0).expect("session 0").ctx.rewrap_gauge.clone();
    gauge.begin(pending.lines_total());
    (term, pending)
}

fn reattach(
    app: &mut App,
    term: &std::sync::Mutex<aterm_core::terminal::Terminal>,
    pending: aterm_grid::PendingScrollbackReflow,
) {
    let next = crate::term_lock(term).finish_resize_offload(pending.reflow());
    assert!(next.is_none(), "one pass");
    let gauge = app.pool.get(0).expect("session 0").ctx.rewrap_gauge.clone();
    gauge.end(aterm_messages::WaitEnd::Done);
    app.sample_session_waits(Instant::now() + WAIT_SAMPLE_IDLE);
}

#[test]
fn a_scroll_up_during_a_rewrap_lands_where_it_asked_at_the_reattach() {
    use aterm_types::mouse::WheelDir;

    use crate::input::PixelOffset;

    let wheel_up = || InputEvent::Wheel {
        dir: WheelDir::Up,
        lines: 3,
        detent: true,
        row: 0,
        col: 0,
        mods: 0,
        px_off: PixelOffset::CELL_ORIGIN,
    };
    let (mut app, _sink, _reader) = app_with_reader();
    let (term, pending) = rewrapping_history(&mut app, 500);
    app.input(WindowId(0), wheel_up(), Source::Human);
    {
        let t = crate::term_lock(&term);
        assert_eq!(t.grid().display_offset(), 0, "nothing to show yet");
        assert!(t.grid().reader_aim_held(), "the ask is kept");
    }
    assert!(
        app.windows[&WindowId(0)].scroll_glide.is_none(),
        "no glide over a history that is not there"
    );
    assert_eq!(
        app.messages.live_rows().count(),
        1,
        "the person reaching for history waits on the rewrap"
    );
    app.input(WindowId(0), wheel_up(), Source::Human);
    reattach(&mut app, &term, pending);
    let landed = crate::term_lock(&term).grid().display_offset();
    assert!(landed > 0, "the view lands up in the history it asked for");

    let (mut plain, _s, _r) = app_with_reader();
    let (pterm, ppending) = rewrapping_history(&mut plain, 500);
    reattach(&mut plain, &pterm, ppending);
    let before = crate::term_lock(&pterm).grid().display_offset();
    assert_eq!(before, 0);
    for _ in 0..2 {
        plain.input(WindowId(0), wheel_up(), Source::Human);
        plain.settle_scroll_motion_at_target(WindowId(0), Instant::now());
    }
    assert_eq!(
        crate::term_lock(&pterm).grid().display_offset(),
        landed,
        "exactly where the same notches go once the history is home"
    );

    let (mut app, _sink, _reader) = app_with_reader();
    let (term, pending) = rewrapping_history(&mut app, 500);
    app.input(WindowId(0), wheel_up(), Source::Human);
    assert!(crate::term_lock(&term).grid().reader_aim_held());
    app.input(WindowId(0), InputEvent::Text("k".into()), Source::Human);
    reattach(&mut app, &term, pending);
    assert_eq!(
        crate::term_lock(&term).grid().display_offset(),
        0,
        "typing brought the aim home; the re-attach does not jump the view"
    );
}
