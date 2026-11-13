// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{ResetReport, ResetSource, manual_payload};
use crate::menu::MenuAction;
use crate::{App, WindowId};

#[test]
fn manual_reset_payload_names_its_reason_and_source() {
    let r = ResetReport {
        reverted: vec!["alt", "mouse"],
        bytes: 17,
    };
    assert_eq!(
        manual_payload(ResetSource::Verb, &r),
        "reason=manual source=ctl reverted=alt,mouse bytes=17"
    );
    let empty = ResetReport {
        reverted: Vec::new(),
        bytes: 0,
    };
    assert_eq!(
        manual_payload(ResetSource::Menu, &empty),
        "reason=manual source=menu reverted=- bytes=0"
    );
}

/// Edit ▸ Reset Terminal: enabled over a terminal tab, reachable by its `invoke` name, writes
/// no PTY input, and on a reader-less stub resets the focused session and records
/// `source=menu`.
#[test]
fn reset_terminal_menu_row_resets_the_focused_session() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let sid = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(sid));
    app.frontmost_window = Some(wid);
    let (term, ctx) = {
        let s = app.pool.get(sid).expect("stub pooled");
        (s.term.clone(), s.ctx.clone())
    };
    term.lock()
        .expect("terminal lock")
        .process(b"\x1b[?1049h\x1b[?1003h\x1b[?1006h\x1b(0\x0e% ");
    assert!(term.lock().expect("terminal lock").program_owns_terminal());

    let row = app
        .palette_snapshot(wid)
        .rows()
        .iter()
        .find(|r| r.action == MenuAction::ResetTerminal)
        .cloned()
        .expect("Reset Terminal has a palette row");
    assert!(row.enabled, "enabled over a terminal tab");
    assert_eq!(
        MenuAction::from_invoke_name("ResetTerminal"),
        Some(MenuAction::ResetTerminal)
    );
    assert!(!MenuAction::ResetTerminal.writes_pty_input());

    app.menu_reset_terminal(wid);
    let t = term.lock().expect("terminal lock");
    assert!(!t.program_owns_terminal(), "the modes are handed back");
    assert!(!t.is_alternate_screen());
    drop(t);
    let events: Vec<String> = ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .since(None)
        .filter(|e| e.kind == "modes-restored")
        .map(|e| e.payload.to_string())
        .collect();
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(
        events[0].starts_with("reason=manual source=menu reverted=")
            && events[0].contains("alt")
            && events[0].contains("mouse")
            && events[0].contains("g0")
            && events[0].contains("gl"),
        "{events:?}"
    );
}

/// `reset flush` where nothing can be flushed answers `discarded=-`, never `discarded=0`. The
/// stub's sink has no tty (`master = -1`), the same `None` every Linux and Windows session
/// gets.
#[test]
fn reset_flush_that_flushes_nothing_says_so() {
    let s = crate::stub_session(11);
    s.term
        .lock()
        .expect("terminal lock")
        .process(b"\x1b[?1003h\x1b[?1006h");
    assert_eq!(
        super::cmd_reset(11, &s.term, &s.ctx, "flush"),
        "OK reset reverted=mouse,mouse-encoding bytes=16 discarded=-\n"
    );
    assert!(
        !s.term
            .lock()
            .expect("terminal lock")
            .program_owns_terminal(),
        "the reset itself still ran"
    );
    assert_eq!(super::discarded_field(None), " discarded=-");
    assert_eq!(super::discarded_field(Some(0)), " discarded=0");
    assert_eq!(super::discarded_field(Some(9)), " discarded=9");
}

/// On a real raw pty with an unread SGR mouse report queued, `reset flush` drops and counts it.
#[test]
#[cfg(target_os = "macos")]
fn reset_flush_on_a_real_tty_discards_and_counts_the_queue() {
    use crate::input_stall::tests::raw_pty_pair;
    let (master, slave) = raw_pty_pair();
    let sink = std::sync::Arc::new(aterm_session::sink::SinkWriter::new(master));
    let s = crate::stub_session_with_sink(12, sink.clone());
    let report = b"\x1b[<35;10;5M";
    sink.write_frame_nonparking(report).expect("queued");
    assert_eq!(aterm_pty::input_queue_len(master), Some(report.len()));
    assert_eq!(
        super::cmd_reset(12, &s.term, &s.ctx, "flush"),
        format!("OK reset reverted=- bytes=0 discarded={}\n", report.len())
    );
    assert_eq!(aterm_pty::input_queue_len(master), Some(0));
    drop(s);
    drop(sink);
    // SAFETY: both fds came from `raw_pty_pair` and are closed once, here.
    unsafe {
        libc::close(slave);
        libc::close(master);
    }
}

/// A reader that ends with a reset queued and unanswered disconnects the verb's wait, and the
/// reset runs directly rather than timing out as "stays queued".
#[test]
#[cfg(unix)]
fn a_reader_that_ends_without_answering_runs_the_reset_directly() {
    let s = crate::stub_session(7);
    let (tx, rx) = std::sync::mpsc::sync_channel::<crate::spawn::GatherMsg>(4);
    let generation = s.ctx.reset_lane.open(tx, None);
    s.term
        .lock()
        .expect("terminal lock")
        .process(b"\x1b[?1000h");
    let (term, ctx) = (s.term.clone(), s.ctx.clone());
    let asker = std::thread::spawn(move || {
        super::request(
            7,
            &term,
            &ctx,
            ResetSource::Verb,
            Some(aterm_spec::wait::MUST_HAPPEN),
        )
    });
    let msg = rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the reset was queued on the lane");
    assert!(matches!(msg, crate::spawn::GatherMsg::Reset(_)));
    s.ctx.reset_lane.close_if(generation);
    drop(msg);
    drop(rx);
    let report = asker
        .join()
        .expect("the asker thread")
        .expect("a report, not a timeout")
        .expect("waited for");
    assert_eq!(report.reverted, vec!["mouse"]);
    assert!(
        !s.term
            .lock()
            .expect("terminal lock")
            .program_owns_terminal()
    );
}
