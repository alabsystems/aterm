// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Control reads of an unfinished DEC 2026 frame ([`crate::term_lock_whole`]).

use super::*;
#[cfg(unix)]
use crate::ReadOn;
#[cfg(unix)]
use crate::input::Delivery;
use crate::session_store::SessionHandle;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const WHOLE: &str = "\x1b[?2026h\x1b[H\x1b[2J⏺ Done\r\n❯ ready\x1b[?2026l";
const HALF: &str = "\x1b[?2026h\x1b[H\x1b[2J⏺ On it.";
const REST: &str = "\r\n❯ ready\x1b[?2026l";

fn session(h: SessionHandle, bound: Duration) -> SessionHandle {
    h.ctx.watchers.bind_frame(&h.term);
    term_lock(&h.term).apply_config(&aterm_core::config::TerminalConfig {
        sync_timeout_ms: u64::try_from(bound.as_millis()).expect("a bound in u64 ms"),
        ..Default::default()
    });
    h
}

fn burst(h: &SessionHandle, bytes: &str) {
    term_lock(&h.term).process(bytes.as_bytes());
    h.ctx.watchers.ring_watchers();
}

fn abs_row_1(h: &SessionHandle) -> String {
    let t = term_lock(&h.term);
    (t.grid().oldest_absolute_row() + t.grid().scrollback_lines() as u64 + 1).to_string()
}

type Reader<'a> = (&'static str, Box<dyn Fn() -> String + Send + Sync + 'a>);

fn readers(h: &SessionHandle) -> Vec<Reader<'_>> {
    let term = &h.term;
    let line = abs_row_1(h);
    let abs_cell = format!("abs={line} 0");
    vec![
        ("text", Box::new(move || cmd_text(term))),
        (
            "text --json",
            Box::new(move || cmd_text_json_opt(term, TextArgs::default())),
        ),
        (
            "screen",
            Box::new(move || {
                cmd_screen_styled_json(term, &crate::frame_memo::FrameSlot::default(), "")
            }),
        ),
        ("cursor", Box::new(move || control_query::cmd_cursor(term))),
        ("cursor --json", Box::new(move || cmd_cursor_json(term))),
        ("cell", Box::new(move || cmd_cell(term, "1 0"))),
        ("cell abs=", Box::new(move || cmd_cell(term, &abs_cell))),
        ("line", Box::new(move || cmd_line(term, &line))),
        (
            "offscreen screen=1",
            Box::new(move || {
                control_query::cmd_offscreen(
                    term,
                    &crate::frame_memo::FrameSlot::default(),
                    "screen=1",
                )
            }),
        ),
        (
            "search",
            Box::new(move || cmd_search(term, "ready", &RewrapGauge::default())),
        ),
        (
            "select, selection",
            Box::new(move || {
                let host = super::super::control_host::GuiHost::new(0, term, None, &h.ctx.watchers);
                let selected = control_selection::cmd_select(&host, 0, "line 1");
                selected + &control_selection::cmd_selection(&host, 0)
            }),
        ),
    ]
}

#[test]
fn every_control_read_waits_for_the_frame_and_reads_it_whole() {
    let h = session(registered_session(0, -1, b""), EVENTUALLY);
    burst(&h, WHOLE);
    burst(&h, HALF);
    assert!(
        term_lock(&h.term).frame_unfinished_at(Instant::now()),
        "precondition: the frame is unfinished"
    );
    let readers = readers(&h);
    let replies: Vec<(&str, String)> = std::thread::scope(|s| {
        let running: Vec<_> = readers
            .iter()
            .map(|(name, read)| (*name, s.spawn(read)))
            .collect();
        aterm_spec::wait::until_within(EVENTUALLY, "every reader waiting, or done", || {
            (h.ctx.watchers.count() == running.len()
                || running.iter().all(|(_, r)| r.is_finished()))
            .then_some(())
        });
        burst(&h, REST);
        running
            .into_iter()
            .map(|(name, r)| (name, r.join().expect("a reader")))
            .collect()
    });
    let host = super::super::control_host::GuiHost::new(0, &h.term, None, &h.ctx.watchers);
    let _ = control_selection::cmd_select(&host, 0, "clear");
    let unselected = cmd_screen_styled_json(&h.term, &crate::frame_memo::FrameSlot::default(), "");
    let _ = control_selection::cmd_select(&host, 0, "line 1");
    for ((name, reply), (_, read)) in replies.iter().zip(&readers) {
        if *name != "screen" || reply != &unselected {
            assert_eq!(reply, &read(), "{name} read the whole frame");
        }
        assert!(
            !reply.contains("frame=open") && !reply.contains("\"frame\":\"open\""),
            "{name}: {reply}"
        );
    }
    assert!(
        replies[0].1.contains("\n⏺ On it.\n❯ ready\n"),
        "text: {}",
        replies[0].1
    );
}

#[test]
fn a_reader_is_never_starved_by_frame_after_frame() {
    use aterm_core::terminal::ClockReading;
    const BOUND: Duration = Duration::from_secs(2);
    let h = session(registered_session(0, -1, b""), BOUND);
    burst(&h, WHOLE);
    let later = Instant::now()
        .checked_add(Duration::from_secs(24 * 3600))
        .expect("an instant a day on");
    term_lock(&h.term).process_at(
        HALF.as_bytes(),
        ClockReading {
            monotonic: later,
            wall_ms: None,
        },
    );
    h.ctx.watchers.ring_watchers();
    let done = AtomicBool::new(false);
    let (answered, reply, took) = std::thread::scope(|s| {
        let reader = s.spawn(|| {
            let call = Instant::now();
            let reply = cmd_text(&h.term);
            (reply, call.elapsed())
        });
        aterm_spec::wait::until_within(EVENTUALLY, "the reader waiting, or done", || {
            (h.ctx.watchers.count() == 1 || reader.is_finished()).then_some(())
        });
        let writer = s.spawn(|| {
            let mut n = 0u64;
            while !done.load(Ordering::Acquire) {
                n += 1;
                burst(&h, &format!("\x1b[?2026l\x1b[?2026h\x1b[H\x1b[2Jframe {n}"));
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        let answered = aterm_spec::wait::within(EVENTUALLY, Duration::from_millis(10), || {
            reader.is_finished().then_some(())
        });
        done.store(true, Ordering::Release);
        writer.join().expect("the writer");
        burst(&h, "\x1b[?2026l");
        let (reply, took) = reader.join().expect("the reader");
        (answered, reply, took)
    });
    assert!(answered.is_some(), "the read never answered: {took:?}");
    let header = reply.lines().next().unwrap_or_default();
    assert_eq!(header, "OK 24 frame=open", "{reply}");
    assert!(
        took >= BOUND,
        "it answered at its bound, not before: {took:?} from its call"
    );
}

#[test]
fn a_read_facing_an_unfinished_frame_at_its_bound_says_frame_open() {
    let h = session(registered_session(0, -1, b""), Duration::from_millis(1));
    burst(&h, WHOLE);
    burst(&h, HALF);
    let term = &h.term;
    let slot = crate::frame_memo::FrameSlot::default();
    let gauge = RewrapGauge::default();
    let header = |reply: String| reply.lines().next().unwrap_or_default().to_string();
    let reads = || -> Vec<(&str, String, &str)> {
        let trim = control_query::text_args("trim").expect("args");
        vec![
            ("text", header(cmd_text(term)), "OK 24 frame=open"),
            (
                "text trim",
                header(control_query::cmd_text_opt(term, &slot, trim)),
                "OK 1 frame=open trimmed=23",
            ),
            (
                "cursor",
                header(control_query::cmd_cursor(term)),
                "OK 0 8 1 blinking_block frame=open",
            ),
            (
                "cursor --json",
                cmd_cursor_json(term),
                ",\"frame\":\"open\"}\n",
            ),
            (
                "cell",
                header(cmd_cell(term, "0 0")),
                "OK %E2%8F%BA e5e5e5 000000 none frame=open",
            ),
            (
                "text --json",
                cmd_text_json_opt(term, TextArgs::default()),
                ",\"frame\":\"open\"}\n",
            ),
            (
                "screen",
                cmd_screen_styled_json(term, &slot, ""),
                ",\"frame\":\"open\"}\n",
            ),
            (
                "offscreen screen=1",
                header(control_query::cmd_offscreen(term, &slot, "screen=1")),
                " frame=open screen_rows=24",
            ),
            (
                "search",
                header(cmd_search(term, "On it", &gauge)),
                " frame=open screen_first=",
            ),
            (
                "search, no pattern",
                header(cmd_search(term, "", &gauge)),
                "OK 0 frame=open screen_first=",
            ),
        ]
    };
    let lacking: Vec<String> = reads()
        .into_iter()
        .filter(|(_, reply, word)| {
            if word.ends_with('\n') {
                !reply.ends_with(word)
            } else {
                !reply.contains(word)
            }
        })
        .map(|(name, reply, _)| format!("{name}: {reply}"))
        .collect();
    assert!(lacking.is_empty(), "no frame word: {lacking:#?}");
    burst(&h, REST);
    for (name, reply, _) in reads() {
        assert!(
            !reply.contains("frame=open") && !reply.contains("\"frame\""),
            "{name} once the frame is whole: {reply}"
        );
    }
}

#[test]
#[cfg(unix)]
fn a_row_fence_declines_an_unfinished_frame_and_the_event_loop_never_waits() {
    use std::os::fd::FromRawFd as _;
    let (h, rx) = guarded_pipe_session(1);
    // SAFETY: guarded_pipe_session returned this new write descriptor; the session and sink
    // borrow it, and no other owner closes it.
    let _write_end = unsafe { std::os::fd::OwnedFd::from_raw_fd(h.master) };
    let h = session(h, EVENTUALLY);
    burst(&h, WHOLE);
    burst(&h, HALF);
    let guard = control_input::compile_guard(Some("On.it"))
        .unwrap()
        .unwrap();
    let rows_fenced = InputFence {
        guard: Some(&*guard),
        ..InputFence::default()
    };
    let press = |fence: &InputFence<'_>, on: ReadOn| {
        input_if_fenced_observed(
            &h.term,
            &h.ctx,
            InputEvent::KeySequence(b"1".to_vec()),
            fence,
            on,
            None,
        )
    };
    let (on_loop, registered) = std::thread::scope(|s| {
        let pressing = s.spawn(|| press(&rows_fenced, ReadOn::EventLoop));
        let mut registered = 0;
        aterm_spec::wait::until_within(EVENTUALLY, "the event loop's press", || {
            registered = registered.max(h.ctx.watchers.count());
            pressing.is_finished().then_some(())
        });
        (pressing.join().expect("the press"), registered)
    });
    assert_eq!(on_loop, GuardedInput::Changed);
    assert_eq!(registered, 0, "the event loop never waited");
    assert!(drain_pipe(&rx).is_empty(), "nothing written");

    let generation = crate::control::screen_gen(&term_lock(&h.term));
    let as_stands = InputFence {
        generation: Some(generation),
        ..InputFence::default()
    };
    assert_eq!(
        press(&as_stands, ReadOn::EventLoop),
        GuardedInput::Pressed(Delivery::Full)
    );
    assert_eq!(drain_pipe(&rx), b"1", "a fence that reads no row presses");

    let on_control = std::thread::scope(|s| {
        let pressing = s.spawn(|| press(&rows_fenced, ReadOn::Control));
        aterm_spec::wait::until_within(EVENTUALLY, "the control press waiting", || {
            (h.ctx.watchers.count() == 1 || pressing.is_finished()).then_some(())
        });
        burst(&h, REST);
        pressing.join().expect("the press")
    });
    assert_eq!(on_control, GuardedInput::Pressed(Delivery::Full));
    assert_eq!(drain_pipe(&rx), b"1", "pressed on the whole frame");
}

#[test]
fn a_turns_row_fence_and_settled_screen_take_the_whole_frame_rule() {
    use aterm_types::control_verbs::RowsFence;
    use std::cell::RefCell;
    let store = session_store::new_store();
    let h = session(registered_session(0, -1, b""), Duration::from_millis(1));
    store.write().unwrap().register(h.clone());
    let term = &h.term;
    let pasted: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let paste = |text: &str| {
        pasted.borrow_mut().push(text.to_string());
        term_lock(term).process(text.as_bytes());
        true
    };
    let press = |_: &str| {
        term_lock(term).process(b"\r\nresponse\r\n");
        true
    };
    let io = TurnIo {
        paste: &paste,
        press: &press,
        ..TurnIo::paste_only()
    };
    burst(
        &h,
        "\x1b[?2026h\x1b[H\x1b[2Jtick 1\r\n❯ \x1b[?2026l\x1b[2;3H",
    );
    let hash = crate::control::rows_hash(&term_lock(term), 1, 1).expect("in the grid");
    let fence = RowsFence {
        first: 1,
        last: 1,
        hash,
    };
    burst(&h, "\x1b[?2026h\x1b[1;1Htick 2\x1b[2;3H");
    let turn = |args: &str| cmd_turn(term, &store, 0, args, &h.ctx, &io);
    let out = turn(&format!("if-rows={fence} idle=20 timeout=60000 hello"));
    assert!(
        out.starts_with("OK 0 turn skipped reason=changed submitted=0 seq="),
        "{out}"
    );
    assert!(
        pasted.borrow().is_empty(),
        "nothing typed into half a frame"
    );
    let keys = std::cell::Cell::new(0usize);
    let key = |_: crate::input::InputEvent| {
        keys.set(keys.get() + 1);
        true
    };
    let momentum = || Ok((0.0, Instant::now()));
    let typed_io = TurnIo {
        paste: &paste,
        press: &press,
        key: &key,
        momentum: &momentum,
        ..TurnIo::paste_only()
    };
    let out = cmd_turn(
        term,
        &store,
        0,
        &format!("if-rows={fence} typed=1 cadence=1 idle=20 timeout=60000 hello"),
        &h.ctx,
        &typed_io,
    );
    assert!(
        out.starts_with("OK 0 turn skipped reason=changed "),
        "{out}"
    );
    assert_eq!(keys.get(), 0, "no key into half a frame");
    let judged = crate::control::screen_gen(&term_lock(term));
    let out = turn(&format!("if-gen={judged} idle=20 timeout=60000 hello"));
    let verdict = out.lines().next().unwrap_or_default();
    assert!(verdict.contains(" submitted=1 "), "{out}");
    assert!(verdict.ends_with(" frame=open"), "{verdict}");
    assert_eq!(pasted.borrow().as_slice(), ["hello"]);
    burst(&h, "\x1b[?2026l");
    let hash = crate::control::rows_hash(&term_lock(term), 1, 1).expect("in the grid");
    let fence = RowsFence {
        first: 1,
        last: 1,
        hash,
    };
    let out = turn(&format!("if-rows={fence} idle=20 timeout=60000 again"));
    let verdict = out.lines().next().unwrap_or_default();
    assert!(verdict.contains(" submitted=1 "), "{out}");
    assert!(!verdict.contains("frame=open"), "{verdict}");
}

fn waited_for(h: &SessionHandle, read: impl FnOnce() -> String + Send) -> (bool, String) {
    std::thread::scope(|s| {
        let reading = s.spawn(read);
        let waited =
            aterm_spec::wait::until_within(EVENTUALLY, "the read answers, or waits", || {
                if h.ctx.watchers.count() > 0 {
                    Some(true)
                } else {
                    reading.is_finished().then_some(false)
                }
            });
        if waited {
            burst(h, REST);
        }
        (waited, reading.join().expect("the read"))
    })
}

#[test]
fn a_frame_the_glass_gave_up_is_read_at_once() {
    use aterm_core::terminal::ClockReading;
    let h = session(registered_session(0, -1, b""), Duration::from_secs(5));
    burst(&h, WHOLE);
    let begun = Instant::now()
        .checked_sub(Duration::from_secs(10))
        .expect("an instant 10 s back");
    term_lock(&h.term).process_at(
        HALF.as_bytes(),
        ClockReading {
            monotonic: begun,
            wall_ms: None,
        },
    );
    assert!(
        term_lock(&h.term).frame_unfinished_at(Instant::now()),
        "precondition: the frame is unfinished"
    );
    let (waited, reply) = waited_for(&h, || cmd_text(&h.term));
    assert!(!waited, "the read waited for a frame given up: {reply}");
    assert_eq!(reply.lines().next(), Some("OK 24 frame=open"), "{reply}");
}

#[test]
fn a_history_line_is_read_without_waiting_for_the_frame() {
    let h = session(registered_session(0, -1, b""), EVENTUALLY);
    let lines: String = (0..30).map(|n| format!("old {n}\r\n")).collect();
    burst(&h, &lines);
    burst(&h, HALF);
    let oldest = {
        let t = term_lock(&h.term);
        assert!(t.grid().scrollback_lines() > 0, "precondition: history");
        assert!(
            t.frame_unfinished_at(Instant::now()),
            "precondition: the frame is unfinished"
        );
        t.grid().oldest_absolute_row().to_string()
    };
    let (waited, reply) = waited_for(&h, || cmd_line(&h.term, &oldest));
    assert!(!waited, "a history row waited for the frame: {reply}");
    assert_eq!(reply, "OK old 0\n");
}

#[test]
fn a_history_cell_is_read_without_waiting_for_the_frame() {
    let h = session(registered_session(0, -1, b""), EVENTUALLY);
    let lines: String = (0..30).map(|n| format!("old {n}\r\n")).collect();
    burst(&h, &lines);
    burst(&h, HALF);
    let oldest = {
        let t = term_lock(&h.term);
        assert!(t.grid().scrollback_lines() > 0, "precondition: history");
        assert!(
            t.frame_unfinished_at(Instant::now()),
            "precondition: the frame is unfinished"
        );
        t.grid().oldest_absolute_row()
    };
    let (waited, reply) = waited_for(&h, || cmd_cell(&h.term, &format!("abs={oldest} 0")));
    assert!(!waited, "a history cell waited for the frame: {reply}");
    assert_eq!(reply, "OK o e5e5e5 000000 none\n");
}

#[test]
fn text_json_stamps_the_person_after_its_wait() {
    let h = session(registered_session(0, -1, b""), EVENTUALLY);
    burst(&h, WHOLE);
    burst(&h, HALF);
    let reply = std::thread::scope(|s| {
        let reading = s.spawn(|| cmd_text_json_looked(&h.term, &h.ctx, TextArgs::default()));
        aterm_spec::wait::until_within(EVENTUALLY, "the read waiting", || {
            (h.ctx.watchers.count() == 1).then_some(())
        });
        let typed = h
            .ctx
            .human_input
            .note(crate::metrics::now_us(), crate::human_input::Gesture::Other);
        assert!(typed, "precondition: the person's first gesture");
        drop(
            h.ctx
                .output_echo
                .begin_event(&InputEvent::KeySequence(b"x".to_vec())),
        );
        burst(&h, REST);
        reading.join().expect("the read")
    });
    assert!(reply.contains(",\"human_seq\":1,"), "{reply}");
    assert!(!reply.contains("\"human_ms\":null"), "{reply}");
    assert!(reply.contains(",\"inputs\":[1,1],"), "{reply}");
}

#[test]
fn the_host_verbs_say_frame_open_on_a_frame_unfinished_at_their_bound() {
    let h = session(registered_session(0, -1, b""), Duration::from_millis(1));
    let (term, watchers) = (&h.term, &h.ctx.watchers);
    let host = || super::super::control_host::GuiHost::new(0, term, None, watchers);
    burst(
        &h,
        "\x1b]133;A\x07$ \x1b]133;B\x07true\r\n\x1b]133;C\x07done\r\n\x1b]133;D;0\x07",
    );
    let id = term_lock(term)
        .newest_completed_block()
        .map(|b| b.id)
        .expect("a completed block");
    assert_eq!(control_selection::cmd_select(&host(), 0, "line 0"), "OK\n");
    burst(&h, "\x1b[?2026h\x1b[6;1Hdrawing");
    let headers = || {
        [
            control_selection::blocktext(&host(), 0, id, false),
            control_selection::blocktext(&host(), 0, id, true),
            control_selection::cmd_selection(&host(), 0),
            control_selection::cmd_copy(&host(), 0),
            control_selection::cmd_select(&host(), 0, "line 0"),
        ]
        .map(|reply| reply.lines().next().unwrap_or_default().to_string())
    };
    assert_eq!(
        headers(),
        [
            "OK 1 frame=open",
            "OK 1 frame=open trimmed=0",
            "OK 1 frame=open",
            "OK 6 frame=open",
            "OK frame=open",
        ]
    );
    burst(&h, "\x1b[?2026l");
    assert_eq!(headers(), ["OK 1", "OK 1 trimmed=0", "OK 1", "OK 6", "OK"]);
}

#[test]
fn a_typed_turn_off_an_input_frame_waits_for_the_whole_frame() {
    use aterm_types::control_verbs::RowsFence;
    let store = session_store::new_store();
    let h = session(registered_session(0, -1, b""), EVENTUALLY);
    store.write().unwrap().register(h.clone());
    let term = &h.term;
    burst(
        &h,
        "\x1b[?2026h\x1b[H\x1b[2Jtick 1\r\n❯ \x1b[?2026l\x1b[2;3H",
    );
    let hash = crate::control::rows_hash(&term_lock(term), 1, 1).expect("in the grid");
    let fence = RowsFence {
        first: 1,
        last: 1,
        hash,
    };
    burst(&h, "\x1b[?2026h\x1b[1;1Htick 2\x1b[2;3H");
    let keys = std::cell::Cell::new(0usize);
    let key = |_: crate::input::InputEvent| {
        keys.set(keys.get() + 1);
        true
    };
    let press = |_: &str| {
        term_lock(term).process(b"\r\nresponse\r\n");
        true
    };
    let momentum = || Ok((0.0, Instant::now()));
    let io = TurnIo {
        press: &press,
        key: &key,
        momentum: &momentum,
        ..TurnIo::paste_only()
    };
    let turned = AtomicBool::new(false);
    let (out, waited) = std::thread::scope(|s| {
        let program = s.spawn(|| {
            let waited =
                aterm_spec::wait::until_within(EVENTUALLY, "the turn waits, or answers", || {
                    if h.ctx.watchers.count() == 2 {
                        Some(true)
                    } else {
                        turned.load(Ordering::Acquire).then_some(false)
                    }
                });
            burst(&h, "\x1b[?2026l");
            waited
        });
        let out = cmd_turn(
            term,
            &store,
            0,
            &format!("if-rows={fence} typed=1 cadence=1 idle=20 timeout=60000 hello"),
            &h.ctx,
            &io,
        );
        turned.store(true, Ordering::Release);
        (out, program.join().expect("the program"))
    });
    assert!(waited, "the turn waited for the frame: {out}");
    let verdict = out.lines().next().unwrap_or_default();
    assert!(verdict.contains(" submitted=1 "), "{out}");
    assert!(!verdict.contains("frame=open"), "{verdict}");
    assert_eq!(keys.get(), 5, "every key typed into the whole frame");
}

#[test]
fn a_typed_turns_fence_is_judged_on_the_whole_frame_its_first_key_waits_for() {
    use aterm_types::control_verbs::RowsFence;
    use control_session::set_hold_wait_for_test;
    struct Clock;
    impl Drop for Clock {
        fn drop(&mut self) {
            set_hold_wait_for_test(None);
        }
    }
    let _clock = Clock;
    let store = session_store::new_store();
    let h = session(registered_session(0, -1, b""), EVENTUALLY);
    store.write().unwrap().register(h.clone());
    let term = &h.term;
    let keys = std::cell::Cell::new(0usize);
    let key = |_: crate::input::InputEvent| {
        keys.set(keys.get() + 1);
        true
    };
    let press = |_: &str| {
        term_lock(term).process(b"\r\nresponse\r\n");
        true
    };
    let momentum = || Ok((0.0, Instant::now()));
    let io = TurnIo {
        press: &press,
        key: &key,
        momentum: &momentum,
        ..TurnIo::paste_only()
    };
    let rule = "─".repeat(20);
    for (composer, typed) in [("❯ someone else's draft", 0), ("❯ ", 5)] {
        burst(
            &h,
            &format!(
                "\x1b[?2026h\x1b[H\x1b[2Jtick 1\r\n{rule}\r\n❯ \r\n{rule}\x1b[?2026l\x1b[3;3H"
            ),
        );
        let hash = crate::control::rows_hash(&term_lock(term), 1, 3).expect("in the grid");
        let fence = RowsFence {
            first: 1,
            last: 3,
            hash,
        };
        burst(&h, "\x1b[?2026h\x1b[1;1Htick 2\x1b[3;3H");
        let (program, close) = (h.clone(), format!("\x1b[3;1H\x1b[2K{composer}\x1b[?2026l"));
        set_hold_wait_for_test(Some(Box::new(move || {
            if term_lock(&program.term).sync_frame_unfinished() {
                burst(&program, &close);
            }
            Instant::now()
        })));
        keys.set(0);
        let out = cmd_turn(
            term,
            &store,
            0,
            &format!("if-rows={fence} typed=1 cadence=1 idle=20 timeout=60000 hello"),
            &h.ctx,
            &io,
        );
        set_hold_wait_for_test(None);
        let verdict = out.lines().next().unwrap_or_default();
        assert_eq!(keys.get(), typed, "{composer:?}: {verdict}");
        if typed == 0 {
            assert!(
                verdict.starts_with("OK 0 turn skipped reason=changed typed=0 "),
                "{verdict}"
            );
        } else {
            assert!(verdict.contains(" submitted=1 "), "{verdict}");
        }
    }
}

#[test]
fn a_typed_turns_fence_is_deferred_only_to_an_input_frame_its_first_look_found() {
    use aterm_core::terminal::ClockReading;
    use aterm_types::control_verbs::RowsFence;
    use control_session::set_hold_wait_for_test;
    use std::sync::Arc;
    struct Clock;
    impl Drop for Clock {
        fn drop(&mut self) {
            set_hold_wait_for_test(None);
        }
    }
    let _clock = Clock;
    let held = Arc::new(AtomicBool::new(false));
    let hand = Arc::clone(&held);
    set_hold_wait_for_test(Some(Box::new(move || {
        hand.store(true, Ordering::Release);
        Instant::now() + Duration::from_secs(3600)
    })));
    let store = session_store::new_store();
    let h = session(registered_session(0, -1, b""), Duration::from_secs(2));
    store.write().unwrap().register(h.clone());
    let term = &h.term;
    let keys = std::cell::Cell::new(0usize);
    let key = |_: crate::input::InputEvent| {
        keys.set(keys.get() + 1);
        true
    };
    let press = |_: &str| true;
    let momentum = || Ok((0.0, Instant::now()));
    let io = TurnIo {
        press: &press,
        key: &key,
        momentum: &momentum,
        ..TurnIo::paste_only()
    };
    let rule = "─".repeat(20);
    let input = format!("\x1b[2;1H{rule}\r\n❯ \r\n{rule}\x1b[3;3H");
    let fence = || {
        burst(&h, "\x1b[?2026l");
        burst(
            &h,
            &format!("\x1b[?2026h\x1b[H\x1b[2Jtick 1{input}\x1b[?2026l"),
        );
        let hash = crate::control::rows_hash(&term_lock(term), 1, 3).expect("in the grid");
        RowsFence {
            first: 1,
            last: 3,
            hash,
        }
    };
    let turn = |fence: RowsFence| {
        cmd_turn(
            term,
            &store,
            0,
            &format!("if-rows={fence} typed=1 cadence=1 idle=20 timeout=60000 hello"),
            &h.ctx,
            &io,
        )
    };
    let given_up = fence();
    term_lock(term).process_at(
        b"\x1b[?2026h\x1b[1;1Htick 2\x1b[3;3H",
        ClockReading {
            monotonic: Instant::now()
                .checked_sub(Duration::from_secs(10))
                .expect("an instant 10 s back"),
            wall_ms: None,
        },
    );
    let at_once = turn(given_up);
    let drawn = fence();
    burst(&h, "\x1b[?2026h\x1b[H\x1b[2Jtick 2");
    let at_bound = std::thread::scope(|s| {
        s.spawn(|| {
            aterm_spec::wait::until_within(EVENTUALLY, "the fence's look waiting", || {
                (h.ctx.watchers.count() == 2).then_some(())
            });
            burst(&h, &input);
        });
        turn(drawn)
    });
    for out in [at_once, at_bound] {
        assert!(
            out.starts_with("OK 0 turn skipped reason=changed submitted=0 "),
            "{out}"
        );
    }
    assert_eq!(keys.get(), 0, "no key into half a frame");
    assert!(!held.load(Ordering::Acquire), "a hold waited on the frame");
}

fn claude_echoing(store: &session_store::Store) -> (SessionHandle, &'static str, String) {
    let h = session(
        registered_session(0, -1, &claude_composer_screen()),
        EVENTUALLY,
    );
    store.write().unwrap().register(h.clone());
    h.ctx.timeline.lock().unwrap().publish_agent(
        "idle",
        None,
        None,
        Some(aterm_phase::Program::Claude),
        crate::session_timeline::AgentStamp {
            generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
            fp: 1,
        },
    );
    let echo_head = "\x1b[?2026h\x1b[H\x1b[2J\u{2590}\u{259b}\u{2588}\u{2588}\u{2588}\u{259c}\
                     \u{258c}   Claude Code v2.1.301";
    let rule = "\u{2500}".repeat(80);
    let echo_rest = format!(
        "\x1b[20;1H{rule}\x1b[21;1H\u{276f} carry on\x1b[22;1H{rule}\
         \x1b[23;1H  \u{23f5}\u{23f5} bypass permissions on (shift+tab to cycle)\
         \x1b[21;11H\x1b[?2026l"
    );
    (h, echo_head, echo_rest)
}

#[test]
fn a_turns_composer_verify_reads_the_composer_in_a_whole_frame() {
    use std::cell::Cell;
    let store = session_store::new_store();
    let (h, echo_head, echo_rest) = claude_echoing(&store);
    let term = &h.term;
    let paste = |_: &str| {
        burst(&h, echo_head);
        true
    };
    let presses = Cell::new(0u32);
    let press = |_: &str| {
        presses.set(presses.get() + 1);
        if term_lock(term).sync_frame_unfinished() {
            burst(&h, &echo_rest);
        }
        true
    };
    let io = TurnIo {
        paste: &paste,
        press: &press,
        ..TurnIo::paste_only()
    };
    let turned = AtomicBool::new(false);
    let (out, waited) = std::thread::scope(|s| {
        let program = s.spawn(|| {
            let waited = aterm_spec::wait::until_within(
                EVENTUALLY,
                "the look waits, or the turn ends",
                || {
                    if h.ctx.watchers.count() == 2 {
                        Some(true)
                    } else {
                        turned.load(Ordering::Acquire).then_some(false)
                    }
                },
            );
            if waited {
                burst(&h, &echo_rest);
            }
            waited
        });
        let out = cmd_turn(
            term,
            &store,
            0,
            "settle=submitted presses=2 submit_window=200 timeout=60000 carry on",
            &h.ctx,
            &io,
        );
        turned.store(true, Ordering::Release);
        (out, program.join().expect("the program"))
    });
    let verdict = out.lines().next().unwrap_or_default();
    assert!(
        verdict.contains(" submitted=0 status=timeout "),
        "{verdict}"
    );
    assert_eq!(
        presses.get(),
        2,
        "the line still in the composer: pressed again"
    );
    assert!(waited, "the look waited for the frame: {verdict}");
}

#[test]
fn a_turns_settled_screen_is_read_in_a_whole_frame() {
    let store = session_store::new_store();
    let h = session(registered_session(0, -1, b""), EVENTUALLY);
    store.write().unwrap().register(h.clone());
    let term = &h.term;
    let paste = |text: &str| {
        burst(&h, text);
        true
    };
    let press = |_: &str| {
        burst(&h, HALF);
        true
    };
    let io = TurnIo {
        paste: &paste,
        press: &press,
        ..TurnIo::paste_only()
    };
    let turned = AtomicBool::new(false);
    let (out, waited) = std::thread::scope(|s| {
        let program = s.spawn(|| {
            let waited = aterm_spec::wait::until_within(
                EVENTUALLY,
                "the read waits, or the turn ends",
                || {
                    if h.ctx.watchers.count() == 2 {
                        Some(true)
                    } else {
                        turned.load(Ordering::Acquire).then_some(false)
                    }
                },
            );
            burst(&h, REST);
            waited
        });
        let out = cmd_turn(term, &store, 0, "idle=20 timeout=60000 hello", &h.ctx, &io);
        turned.store(true, Ordering::Release);
        (out, program.join().expect("the program"))
    });
    let verdict = out.lines().next().unwrap_or_default();
    assert!(verdict.contains(" submitted=1 status=settled "), "{out}");
    assert!(
        out.contains("\n⏺ On it.\n❯ ready\n"),
        "the closed frame's rows: {out}"
    );
    assert!(!verdict.contains("frame=open"), "{verdict}");
    assert!(waited, "the read waited for the frame: {verdict}");
}

#[test]
fn a_guihost_select_is_made_on_a_whole_frame() {
    let h = session(registered_session(0, -1, b""), EVENTUALLY);
    burst(&h, WHOLE);
    burst(&h, HALF);
    let (term, watchers) = (&h.term, &h.ctx.watchers);
    let host = || super::super::control_host::GuiHost::new(0, term, None, watchers);
    let (waited, reply) = waited_for(&h, || control_selection::cmd_select(&host(), 0, "word 1 2"));
    if !waited {
        burst(&h, REST);
    }
    assert_eq!(reply, "OK\n");
    assert_eq!(
        control_selection::cmd_selection(&host(), 0),
        "OK 1\nready\n",
        "the word of the whole frame"
    );
    assert!(waited, "the select waited for the frame");
}

#[test]
#[cfg(unix)]
fn a_guihost_verb_whose_caller_hung_up_does_nothing_on_half_a_frame() {
    use aterm_control::SessionHost as _;
    let h = session(registered_session(0, -1, b""), Duration::from_secs(600));
    burst(&h, WHOLE);
    burst(&h, HALF);
    let (term, watchers) = (&h.term, &h.ctx.watchers);
    let host = || super::super::control_host::GuiHost::new(0, term, None, watchers);
    let (client, server) = aterm_uds::CtlStream::pair().expect("socket pair");
    let (selected, read) = std::thread::scope(|s| {
        let verb = s.spawn(move || {
            let _serving = ServingFd::enter(&server);
            let selected = control_selection::cmd_select(&host(), 0, "word 0 2");
            (selected, host().with_terminal(0, |_| ()))
        });
        aterm_spec::wait::until_within(EVENTUALLY, "the select waiting", || {
            (h.ctx.watchers.count() == 1).then_some(())
        });
        drop(client);
        aterm_spec::wait::until_within(EVENTUALLY, "the verb giving its lane back", || {
            verb.is_finished().then_some(())
        });
        verb.join().expect("the verb")
    });
    assert!(
        crate::frame_unfinished_now(&term_lock(term)),
        "precondition: given up at the hangup"
    );
    let made = term_lock(term).selection_to_string_bounded().0;
    assert!(
        made.as_deref().is_none_or(str::is_empty),
        "selected {made:?} off half a frame: {selected}"
    );
    assert!(read.is_none(), "a read ran for a caller that hung up");
}

#[test]
fn a_reads_bound_never_passes_the_hang_detector() {
    use aterm_core::terminal::{ClockReading, FRAME_HANG};
    let h = session(registered_session(0, -1, b""), Duration::from_secs(600));
    burst(&h, WHOLE);
    let now = Instant::now();
    let begun = now
        .checked_sub(FRAME_HANG - Duration::from_secs(1))
        .expect("an instant a minute back");
    term_lock(&h.term).process_at(
        HALF.as_bytes(),
        ClockReading {
            monotonic: begun,
            wall_ms: None,
        },
    );
    let t = term_lock(&h.term);
    let hang = t.frame_hang_deadline().expect("an unfinished frame");
    assert_eq!(hang, begun + FRAME_HANG, "precondition: the run began then");
    assert!(t.frame_unfinished_at(now), "precondition: still unfinished");
    assert_eq!(crate::whole_frame_bound(&t, now), Some(hang));
}

#[test]
#[cfg(unix)]
fn a_read_waiting_for_a_frame_gives_its_lane_back_when_its_caller_hangs_up() {
    let h = session(registered_session(0, -1, b""), Duration::from_secs(600));
    burst(&h, WHOLE);
    burst(&h, HALF);
    let term = &h.term;
    let (client, server) = aterm_uds::CtlStream::pair().expect("socket pair");
    let out = std::thread::scope(|s| {
        let reading = s.spawn(move || {
            let _serving = ServingFd::enter(&server);
            cmd_text(term)
        });
        aterm_spec::wait::until_within(EVENTUALLY, "the read waiting", || {
            (h.ctx.watchers.count() == 1).then_some(())
        });
        drop(client);
        aterm_spec::wait::until_within(EVENTUALLY, "the read giving its lane back", || {
            reading.is_finished().then_some(())
        });
        reading.join().expect("the read")
    });
    let header = out.lines().next().unwrap_or_default();
    assert!(header.ends_with(" frame=open"), "{header}");
    assert!(
        crate::frame_unfinished_now(&term_lock(term)),
        "answered at the hangup, not at the frame's hang detector"
    );
}

/// Whether a read parked as the session's `waiting`th watcher outlasted a few hang-up slices before `close` closed its frame.
#[cfg(unix)]
fn outlasted_the_hangup(h: &SessionHandle, waiting: usize, close: &str) -> bool {
    aterm_spec::wait::until_within(EVENTUALLY, "the read waiting", || {
        (h.ctx.watchers.count() == waiting).then_some(())
    });
    let let_go = aterm_spec::wait::within(HANGUP_POLL * 3, Duration::from_millis(10), || {
        (h.ctx.watchers.count() < waiting).then_some(())
    });
    burst(h, close);
    let_go.is_none()
}

/// Drop the caller and wait until this lane sees it gone.
#[cfg(unix)]
fn hang_up(client: &std::cell::Cell<Option<aterm_uds::CtlStream>>) {
    drop(client.take());
    aterm_spec::wait::until_within(EVENTUALLY, "the lane seeing the hangup", || {
        caller_hung_up().then_some(())
    });
}

#[test]
#[cfg(unix)]
fn a_turn_whose_caller_hung_up_books_a_whole_frame() {
    let store = session_store::new_store();
    let h = session(registered_session(0, -1, b""), EVENTUALLY);
    store.write().unwrap().register(h.clone());
    let term = &h.term;
    let (client, server) = aterm_uds::CtlStream::pair().expect("socket pair");
    let client = std::cell::Cell::new(Some(client));
    let paste = |text: &str| {
        burst(&h, text);
        true
    };
    let press = |_: &str| {
        burst(&h, HALF);
        hang_up(&client);
        true
    };
    let io = TurnIo {
        paste: &paste,
        press: &press,
        ..TurnIo::paste_only()
    };
    let (out, outlasted) = std::thread::scope(|s| {
        let program = s.spawn(|| outlasted_the_hangup(&h, 2, REST));
        let _serving = ServingFd::enter(&server);
        let out = cmd_turn(term, &store, 0, "idle=20 timeout=60000 hello", &h.ctx, &io);
        (out, program.join().expect("the program"))
    });
    let verdict = out.lines().next().unwrap_or_default();
    assert!(verdict.contains(" submitted=1 status=hangup "), "{out}");
    assert!(
        out.contains("\n⏺ On it.\n❯ ready\n"),
        "the closed frame's rows: {out}"
    );
    assert!(!verdict.contains("frame=open"), "{verdict}");
    assert!(
        outlasted,
        "the read gave the frame up at the hangup: {verdict}"
    );
}

#[test]
#[cfg(unix)]
fn a_turn_whose_caller_hung_up_verifies_its_submit_off_a_whole_frame() {
    use std::cell::Cell;
    let store = session_store::new_store();
    let (h, echo_head, echo_rest) = claude_echoing(&store);
    let term = &h.term;
    let (client, server) = aterm_uds::CtlStream::pair().expect("socket pair");
    let client = Cell::new(Some(client));
    let paste = |_: &str| {
        burst(&h, echo_head);
        hang_up(&client);
        true
    };
    let presses = Cell::new(0u32);
    let press = |_: &str| {
        presses.set(presses.get() + 1);
        if term_lock(term).sync_frame_unfinished() {
            burst(&h, &echo_rest);
        }
        true
    };
    let io = TurnIo {
        paste: &paste,
        press: &press,
        ..TurnIo::paste_only()
    };
    let (out, outlasted) = std::thread::scope(|s| {
        let program = s.spawn(|| outlasted_the_hangup(&h, 2, &echo_rest));
        let _serving = ServingFd::enter(&server);
        let out = cmd_turn(
            term,
            &store,
            0,
            "settle=submitted presses=2 submit_window=200 timeout=60000 carry on",
            &h.ctx,
            &io,
        );
        (out, program.join().expect("the program"))
    });
    let verdict = out.lines().next().unwrap_or_default();
    assert!(
        verdict.contains(" submitted=0 status=timeout "),
        "{verdict}"
    );
    assert_eq!(
        presses.get(),
        2,
        "the line still in the composer: pressed again"
    );
    assert!(
        outlasted,
        "the look gave the frame up at the hangup: {verdict}"
    );
}

#[test]
#[cfg(unix)]
fn a_turn_whose_caller_hung_up_presses_its_guarded_enter_off_a_whole_frame() {
    use std::cell::Cell;
    let store = session_store::new_store();
    let (h, rx) = guarded_pipe_session(0);
    let h = session(h, EVENTUALLY);
    store.write().unwrap().register(h.clone());
    burst(&h, WHOLE);
    let term = &h.term;
    let (client, server) = aterm_uds::CtlStream::pair().expect("socket pair");
    let client = Cell::new(Some(client));
    let paste = |_: &str| {
        burst(&h, HALF);
        hang_up(&client);
        true
    };
    let press = |_: &str| panic!("a guarded submit presses through the fence, never io.press");
    let io = TurnIo {
        paste: &paste,
        press: &press,
        ..TurnIo::paste_only()
    };
    let (out, outlasted) = std::thread::scope(|s| {
        let program = s.spawn(|| outlasted_the_hangup(&h, 2, REST));
        let _serving = ServingFd::enter(&server);
        let out = cmd_turn(
            term,
            &store,
            0,
            "idle=20 timeout=60000 submit_window=60000 presses=1 submit=guarded:^\u{276f} hello",
            &h.ctx,
            &io,
        );
        (out, program.join().expect("the program"))
    });
    assert!(!out.contains("reason=guard"), "{out}");
    assert_eq!(drain_pipe(&rx), b"\r", "the Enter went: {out}");
    assert!(
        outlasted,
        "the fence gave the frame up at the hangup: {out}"
    );
}

#[test]
fn a_verb_that_reads_no_row_never_waits_for_a_frame() {
    let h = session(registered_session(0, -1, b""), EVENTUALLY);
    let (term, watchers) = (&h.term, &h.ctx.watchers);
    let host = || super::super::control_host::GuiHost::new(0, term, None, watchers);
    burst(
        &h,
        "\x1b]133;A\x07$ \x1b]133;B\x07true\r\n\x1b]133;C\x07done\r\n\x1b]133;D;0\x07",
    );
    let id = term_lock(term)
        .newest_completed_block()
        .map(|b| b.id)
        .expect("a completed block");
    burst(&h, HALF);
    let reads: Vec<Reader<'_>> = vec![
        (
            "blocks",
            Box::new(|| control_selection::cmd_blocks(&host(), 0, "")),
        ),
        (
            "blocks --json",
            Box::new(|| control_selection::cmd_blocks_json(&host(), 0, "")),
        ),
        (
            "wait",
            Box::new(|| control_selection::cmd_wait(&host(), 0, "")),
        ),
    ];
    for (name, read) in reads {
        assert!(
            term_lock(term).frame_unfinished_at(Instant::now()),
            "precondition: the frame is unfinished"
        );
        let (waited, reply) = waited_for(&h, read);
        assert!(
            !waited,
            "{name} waited for a frame it does not read: {reply}"
        );
        assert!(reply.starts_with("OK"), "{name}: {reply}");
        if name == "wait" {
            assert_eq!(reply, format!("OK complete {id} exit=0\n"));
        }
    }
    let (waited, _) = waited_for(&h, || {
        control_selection::cmd_blocktext(&host(), 0, &id.to_string())
    });
    assert!(waited, "CONTROL: blocktext waits for the frame");
}

#[test]
fn a_parked_wait_never_waits_for_a_frame() {
    let h = session(registered_session(0, -1, b""), EVENTUALLY);
    let (term, watchers) = (&h.term, &h.ctx.watchers);
    let host = || super::super::control_host::GuiHost::new(0, term, None, watchers);
    burst(&h, "\x1b]133;A\x07$ \x1b]133;B\x07sleep\r\n\x1b]133;C\x07");
    let (twice, reply) = std::thread::scope(|s| {
        let parked = s.spawn(|| control_selection::cmd_wait(&host(), 0, "60000"));
        aterm_spec::wait::until_within(EVENTUALLY, "the wait parked", || {
            (watchers.count() == 1).then_some(())
        });
        burst(&h, HALF);
        assert!(
            term_lock(term).frame_unfinished_at(Instant::now()),
            "precondition: the frame is unfinished"
        );
        burst(&h, "\x1b]133;D;0\x07");
        let twice = aterm_spec::wait::until_within(EVENTUALLY, "the wait answers", || {
            if watchers.count() > 1 {
                Some(true)
            } else {
                parked.is_finished().then_some(false)
            }
        });
        if twice {
            burst(&h, REST);
        }
        (twice, parked.join().expect("the wait"))
    });
    assert!(!twice, "the parked wait waited for the frame: {reply}");
    assert!(reply.starts_with("OK complete "), "{reply}");
}

#[test]
fn a_harness_enter_is_booked_on_a_whole_frame() {
    use aterm_agent::harness::upgrade_drive::hand_take_here_request;
    let h = session(registered_session(0, -1, b""), EVENTUALLY);
    let on = |conn: Option<u64>| SERVING_CONNECTION.with(|s| s.set(conn));
    let input = |verb: &str, rest: &str| {
        control_session::ledger_typed_input(&h.term, &h.ctx, Scope::Owner, verb, rest, "OK\n");
    };
    on(Some(7));
    let request = hand_take_here_request();
    let reply = cmd_lease(&h.ctx, request.strip_prefix("lease ").unwrap_or(&request));
    assert!(reply.starts_with("OK lease acquired"), "{reply}");
    burst(&h, WHOLE);
    input("send", "keep going");
    burst(&h, HALF);
    let half = crate::frame_memo::ScreenText::of(&term_lock(&h.term)).fnv();
    let (waited, _) = waited_for(&h, || {
        on(Some(7));
        input("key", "enter");
        on(None);
        String::new()
    });
    if !waited {
        burst(&h, REST);
    }
    let whole = crate::frame_memo::ScreenText::of(&term_lock(&h.term)).fnv();
    assert_ne!(
        half, whole,
        "precondition: the frame's close moved the screen"
    );
    let history = control_session::cmd_history(&h.ctx, "");
    assert_eq!(
        history.lines().count(),
        2,
        "the Enter booked one turn: {history}"
    );
    assert!(
        history.contains(&format!(" hash={whole:016x} ")),
        "the closed frame's hash {whole:016x}, not the half one's {half:016x}: {history}"
    );
    assert!(waited, "the record waited for the frame");
    on(None);
    CONNECTION_CLAIMED.with(|c| c.set(false));
}

#[test]
fn a_harness_enter_with_nothing_to_book_never_waits_for_a_frame() {
    use aterm_agent::harness::upgrade_drive::hand_take_here_request;
    let h = session(registered_session(0, -1, b""), EVENTUALLY);
    let on = |conn: Option<u64>| SERVING_CONNECTION.with(|s| s.set(conn));
    on(Some(7));
    let request = hand_take_here_request();
    let reply = cmd_lease(&h.ctx, request.strip_prefix("lease ").unwrap_or(&request));
    assert!(reply.starts_with("OK lease acquired"), "{reply}");
    burst(&h, WHOLE);
    burst(&h, HALF);
    let (waited, _) = waited_for(&h, || {
        on(Some(7));
        control_session::ledger_typed_input(&h.term, &h.ctx, Scope::Owner, "key", "enter", "OK\n");
        on(None);
        String::new()
    });
    assert!(
        !waited,
        "the Enter's reply waited for a frame it books nothing off"
    );
    assert_eq!(
        control_session::cmd_history(&h.ctx, "").lines().count(),
        1,
        "nothing booked"
    );
    on(None);
    CONNECTION_CLAIMED.with(|c| c.set(false));
}

#[test]
#[cfg(unix)]
fn a_harness_enter_whose_caller_hung_up_is_booked_on_a_whole_frame() {
    use aterm_agent::harness::upgrade_drive::hand_take_here_request;
    let h = session(registered_session(0, -1, b""), EVENTUALLY);
    let on = |conn: Option<u64>| SERVING_CONNECTION.with(|s| s.set(conn));
    on(Some(7));
    let request = hand_take_here_request();
    let reply = cmd_lease(&h.ctx, request.strip_prefix("lease ").unwrap_or(&request));
    assert!(reply.starts_with("OK lease acquired"), "{reply}");
    burst(&h, WHOLE);
    control_session::ledger_typed_input(
        &h.term,
        &h.ctx,
        Scope::Owner,
        "send",
        "keep going",
        "OK\n",
    );
    burst(&h, HALF);
    let (client, server) = aterm_uds::CtlStream::pair().expect("socket pair");
    let client = std::cell::Cell::new(Some(client));
    let outlasted = std::thread::scope(|s| {
        let program = s.spawn(|| outlasted_the_hangup(&h, 1, REST));
        let _serving = ServingFd::enter(&server);
        hang_up(&client);
        on(Some(7));
        control_session::ledger_typed_input(&h.term, &h.ctx, Scope::Owner, "key", "enter", "OK\n");
        on(None);
        program.join().expect("the program")
    });
    let whole = crate::frame_memo::ScreenText::of(&term_lock(&h.term)).fnv();
    let history = control_session::cmd_history(&h.ctx, "");
    assert!(
        history.contains(&format!(" hash={whole:016x} ")),
        "the closed frame's hash {whole:016x}: {history}"
    );
    assert!(outlasted, "the booking gave the frame up at the hangup");
    CONNECTION_CLAIMED.with(|c| c.set(false));
}
