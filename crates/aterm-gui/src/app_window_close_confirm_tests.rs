// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::close_confirm::CloseReplay;
use crate::quit_safety::ConfirmPrompt;

fn busy_quit_prompt() -> ConfirmPrompt {
    crate::quit_safety::confirm_prompt(true, true, true).expect("a busy quit always prompts")
}

fn busy_close_prompt() -> ConfirmPrompt {
    crate::quit_safety::confirm_prompt(false, true, true).expect("a busy close prompts")
}

#[test]
fn presenting_refuses_now_and_the_answer_replays_or_drops() {
    let mut app = App::headless_for_test();
    app.headless = false;
    let wid = WindowId(0);
    assert!(!app.present_close_banner(wid, busy_quit_prompt(), CloseReplay::Window(wid)));
    assert!(app.close_banner.as_ref().is_some_and(|p| p.wid == wid));
    assert!(
        app.windows[&wid].pending_close_attribution.is_some(),
        "the gesture's exit-ledger attribution rides to the replay"
    );
    let refusal = app
        .pending_action_refusal
        .take()
        .expect("the parked question is reported to the wire");
    assert_eq!(refusal, "a quit is waiting for the person at the window");
    assert!(!app.present_close_banner(wid, busy_close_prompt(), CloseReplay::Quit));
    assert_eq!(
        app.close_banner.as_ref().map(|p| p.replay),
        Some(CloseReplay::Window(wid))
    );
    assert!(
        app.pending_action_refusal
            .take()
            .is_some_and(|why| why.starts_with("a quit ")),
        "the refusal names the question that stands, not the one dropped"
    );
    assert_eq!(app.answer_close_banner(false), None);
    assert!(app.close_banner.is_none());
    assert!(app.windows.contains_key(&wid), "cancel closes nothing");
    assert!(app.windows[&wid].pending_close_attribution.is_none());
    assert!(!app.present_close_banner(wid, busy_quit_prompt(), CloseReplay::Quit));
    assert_eq!(app.answer_close_banner(true), Some(CloseReplay::Quit));
    assert!(app.close_banner.is_none());
    assert_eq!(app.answer_close_banner(true), None);
}

#[test]
fn controls_front_reports_the_pending_confirm() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let lines = app.read_aux_controls(crate::app_introspect::AuxTarget::Front);
    assert_eq!(
        app.close_confirm_controls_line().as_deref(),
        Some("confirm open=false")
    );
    assert_eq!(lines, ["overlay open=false", "confirm open=false"]);
    {
        let _closing = crate::session_store::CloseAttribution::enter(
            crate::session_store::ExitReason::CtlClose,
            crate::session_store::ExitActor::Ctl,
        );
        app.present_close_banner(wid, busy_quit_prompt(), CloseReplay::Window(wid));
    }
    let line = app
        .close_confirm_controls_line()
        .expect("a pending confirm is reported on every host");
    assert!(
        line.starts_with("confirm open=true kind=quit window=0 by=ctl "),
        "{line}"
    );
    assert!(line.contains("answer=\"confirm yes\""), "{line}");
    let lines = app.read_aux_controls(crate::app_introspect::AuxTarget::Front);
    assert_eq!(lines.last(), Some(&line), "{lines:?}");
    assert_eq!(app.answer_close_banner(false), None);
    app.present_close_banner(wid, busy_quit_prompt(), CloseReplay::Window(wid));
    let line = app
        .close_confirm_controls_line()
        .expect("a pending confirm is reported on every host");
    assert!(line.contains(" by=human "), "{line}");
    assert!(line.contains("answer=\"-\""), "{line}");
    assert_eq!(app.answer_close_banner(false), None);
    assert_eq!(
        app.close_confirm_controls_line().as_deref(),
        Some("confirm open=false")
    );
}

fn raise_from_the_wire(app: &mut App, wid: WindowId, prompt: ConfirmPrompt) {
    let proceeded = app.invoke_attributed(crate::session_store::ExitActor::Ctl, |app| {
        Ok(app
            .present_close_banner(wid, prompt, CloseReplay::Window(wid))
            .to_string())
    });
    assert_eq!(proceeded.as_deref(), Ok("false"), "the gesture waits");
    assert!(
        app.close_banner
            .as_ref()
            .is_some_and(crate::close_confirm::PendingClose::wire_may_answer),
        "a question raised under `invoke` is the wire's"
    );
}

#[test]
fn the_wire_answers_its_own_question_whatever_window_is_in_front() {
    let mut app = App::headless_for_test();
    app.headless = false;
    let wid = WindowId(0);
    assert_eq!(
        app.answer_close_confirm_from_wire(true),
        Err("nothing is waiting for an answer".to_string())
    );
    raise_from_the_wire(&mut app, wid, busy_quit_prompt());
    let sid = app.next_session_id;
    let other = app.insert_logical_window(crate::stub_session(sid), 24, 80);
    app.frontmost_window = Some(other);
    assert_eq!(
        app.answer_close_confirm_from_wire(false),
        Ok("answered=no kind=quit".to_string())
    );
    assert!(app.close_banner.is_none());
    assert!(app.windows.contains_key(&wid), "`no` closes nothing");
    assert!(app.windows[&wid].pending_close_attribution.is_none());

    raise_from_the_wire(&mut app, wid, busy_quit_prompt());
    assert_eq!(
        app.answer_close_confirm_from_wire(true),
        Ok("answered=yes kind=quit".to_string())
    );
    assert!(app.close_banner.is_none());
    assert!(
        app.windows[&wid].pending_close_attribution.is_some(),
        "`yes` leaves the close's attribution for the replay to journal"
    );
    assert_eq!(
        app.answer_close_confirm_from_wire(false),
        Err("nothing is waiting for an answer".to_string()),
        "answered once"
    );
}

#[test]
fn the_wire_cannot_answer_a_question_the_person_asked() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(!app.present_close_banner(wid, busy_close_prompt(), CloseReplay::Window(wid)));
    for proceed in [true, false] {
        assert_eq!(
            app.answer_close_confirm_from_wire(proceed),
            Err("a window close is waiting for the person at the window".to_string())
        );
        assert!(app.close_banner.is_some(), "the person's question stands");
    }
}

fn named_key(key: aterm_window::keyboard::NamedKey) -> aterm_window::event::KeyEvent {
    aterm_window::event::KeyEvent::synthetic_for_test(
        aterm_window::keyboard::PhysicalKey::Code(aterm_window::keyboard::KeyCode::Enter),
        aterm_window::keyboard::Key::Named(key),
        key.to_text().map(aterm_window::keyboard::SmolStr::new),
        aterm_window::keyboard::KeyLocation::Standard,
        aterm_window::event::ElementState::Pressed,
        false,
    )
}

fn char_key(c: &str) -> aterm_window::event::KeyEvent {
    aterm_window::event::KeyEvent::synthetic_for_test(
        aterm_window::keyboard::PhysicalKey::Code(aterm_window::keyboard::KeyCode::KeyA),
        aterm_window::keyboard::Key::Character(aterm_window::keyboard::SmolStr::new(c)),
        Some(aterm_window::keyboard::SmolStr::new(c)),
        aterm_window::keyboard::KeyLocation::Standard,
        aterm_window::event::ElementState::Pressed,
        false,
    )
}

#[test]
fn the_keyboard_answers_a_standing_question_and_nothing_leaks_under_it() {
    use aterm_window::keyboard::NamedKey;
    let sink = std::sync::Arc::new(aterm_session::sink::SinkWriter::new(-1));
    let mut app = App::headless_for_test_with_sink(std::sync::Arc::clone(&sink));
    app.headless = false;
    let wid = WindowId(0);
    let settled = |sink: &aterm_session::sink::SinkWriter| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while sink.ordered_egress_count() > 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    };
    let before = sink.input_epoch();
    app.on_key(wid, char_key("a"));
    settled(&sink);
    assert_ne!(sink.input_epoch(), before, "an unguarded key writes");

    app.present_close_banner(wid, busy_quit_prompt(), CloseReplay::Window(wid));
    let before = sink.input_epoch();
    app.on_key(wid, char_key("a"));
    app.on_key(wid, named_key(NamedKey::Tab));
    settled(&sink);
    assert_eq!(
        sink.input_epoch(),
        before,
        "nothing reaches the shell under it"
    );
    assert!(
        app.close_banner.is_some(),
        "and nothing but an answer answers it"
    );

    app.on_key(wid, named_key(NamedKey::Escape));
    settled(&sink);
    assert!(app.close_banner.is_none(), "Escape cancels");
    assert!(app.windows.contains_key(&wid), "…and closes nothing");
    assert!(app.windows[&wid].pending_close_attribution.is_none());
    assert_eq!(sink.input_epoch(), before, "the answer is not typed either");

    app.present_close_banner(wid, busy_quit_prompt(), CloseReplay::Window(wid));
    app.on_key(wid, named_key(NamedKey::Enter));
    settled(&sink);
    assert!(app.close_banner.is_none(), "Enter answers");
    assert!(
        app.windows[&wid].pending_close_attribution.is_some(),
        "…yes: the attribution waits for the replay"
    );
    assert_eq!(sink.input_epoch(), before, "the Enter is not typed");

    let sid = app.next_session_id;
    let other = app.insert_logical_window(crate::stub_session(sid), 24, 80);
    app.present_close_banner(other, busy_quit_prompt(), CloseReplay::Window(other));
    app.on_key(wid, char_key("b"));
    settled(&sink);
    assert_ne!(sink.input_epoch(), before, "a key in another window passes");
    assert!(app.close_banner.is_some(), "…and answers nothing");
}

#[test]
fn a_closing_window_drops_its_question() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.present_close_banner(wid, busy_quit_prompt(), CloseReplay::Window(wid));
    assert_eq!(app.close_window_logical(wid), CloseOutcome::Exit);
    assert!(app.close_banner.is_none());
}

#[test]
fn an_interactive_busy_close_parks_the_banner_instead_of_a_dialog() {
    let mut app = App::headless_for_test();
    app.headless = false;
    let wid = WindowId(0);
    app.close_confirm = CloseConfirm::Interactive;
    assert!(!app.confirm_destructive_close(wid, true, true));
    let pending = app.close_banner.as_ref().expect("the question is parked");
    assert_eq!(pending.wid, wid);
    assert_eq!(pending.replay, CloseReplay::Window(wid));
    assert_eq!(
        crate::close_confirm::kind(&pending.prompt),
        "quit",
        "a last-window close quits the app, and its prompt says so"
    );
    app.close_banner = None;
    assert!(!app.confirm_destructive_gesture(wid, true, true, CloseReplay::Quit));
    assert_eq!(
        app.close_banner.as_ref().map(|p| p.replay),
        Some(CloseReplay::Quit)
    );
    #[cfg(target_os = "macos")]
    {
        app.close_banner = None;
        assert!(!app.confirm_destructive_gesture(wid, true, false, CloseReplay::Quit));
        assert!(
            app.close_banner.is_some(),
            "an idle Mac quit also asks in the window"
        );
    }
    app.close_banner = None;
    app.close_confirm = CloseConfirm::WireRefuseBusy;
    assert!(!app.confirm_destructive_close(wid, true, true));
    assert!(app.close_banner.is_none());
    app.close_confirm = CloseConfirm::Programmatic;
    assert!(app.confirm_destructive_close(wid, true, true));
    assert!(app.close_banner.is_none());
    app.close_confirm = CloseConfirm::Interactive;
    assert!(app.confirm_destructive_close(wid, false, false));
    assert!(app.close_banner.is_none());
}

/// A quit the system or System Settings' Quit & Reopen asks for is the person's answer
/// already: the window asks nothing (no `Quit aterm?` sheet, idle or busy, the Mac Setup
/// tab's installer running among the busy), the quit completes at once through its barriers,
/// and a replay cannot terminate twice. A stale Quit & Reopen
/// cannot complete a later one. Sent onto the person's own quit whose question stands, Quit &
/// Reopen takes that quit up: its question goes, and it completes as a reopen.
///
/// NEGATIVE CONTROL: the person's own quit, on an idle Mac and on a busy one, still asks in
/// the window, and its generation stays pending until answered.
#[cfg(target_os = "macos")]
#[test]
fn a_quit_the_system_or_quit_and_reopen_asks_for_asks_nothing() {
    use crate::menu::{NativeTerminateDecision, admit_native_terminate};
    use crate::system_quit::Asker;
    /// A Mac window's App, its one tab running a command (a shell's block open, as
    /// `quit_safety::foreground_busy` reads it) and standing as the Mac Setup tab's
    /// installer when `busy`.
    fn interactive(busy: bool) -> App {
        let mut app = App::headless_for_test();
        app.headless = false;
        app.close_confirm = CloseConfirm::Interactive;
        if busy {
            crate::feed_session_for_test(
                &app,
                0,
                b"\x1b]133;A\x07$ \x1b]133;B\x07softwareupdate -i\n\x1b]133;C\x07",
            );
            app.land_install_session_for_test(0);
            assert!(app.any_foreground_job() && app.developer_tools_install_active());
        }
        app
    }
    for (asker, busy) in [
        (Asker::Reopen, false),
        (Asker::System, false),
        (Asker::Reopen, true),
        (Asker::System, true),
    ] {
        let _serial = crate::menu::terminate_test_serial();
        let mut app = interactive(busy);
        let NativeTerminateDecision::Dispatch(generation) = admit_native_terminate(asker) else {
            panic!("a fresh termination request");
        };
        assert!(app.native_termination_ready(generation), "{asker:?} {busy}");
        assert!(app.close_banner.is_none(), "{asker:?} {busy}: no question");
        assert!(
            !crate::menu::native_termination_is_current(generation),
            "{asker:?} {busy}: completed"
        );
        assert_eq!(crate::system_quit::reopens(), asker == Asker::Reopen);
        assert!(
            !app.native_termination_ready(generation),
            "{asker:?} {busy}: a replay cannot terminate twice"
        );
    }
    let _serial = crate::menu::terminate_test_serial();
    let mut busy = interactive(true);
    let NativeTerminateDecision::Dispatch(person) = admit_native_terminate(Asker::Person) else {
        panic!("a fresh termination request");
    };
    assert!(
        !busy.native_termination_ready(person),
        "a busy person's quit asks"
    );
    assert_eq!(
        busy.close_banner.as_ref().map(|pending| pending.replay),
        Some(CloseReplay::NativeTerminate { generation: person })
    );
    drop(_serial);

    let _serial = crate::menu::terminate_test_serial();
    let mut app = interactive(false);
    let NativeTerminateDecision::Dispatch(old) = admit_native_terminate(Asker::Reopen) else {
        panic!("a fresh termination request");
    };
    assert!(crate::menu::cancel_native_termination(old));
    let NativeTerminateDecision::Dispatch(new) = admit_native_terminate(Asker::Reopen) else {
        panic!("the arbiter is free again");
    };
    assert!(!app.native_termination_ready(old), "a stale Quit & Reopen");
    assert!(crate::menu::native_termination_is_current(new));
    assert!(app.native_termination_ready(new));
    drop(_serial);

    let _serial = crate::menu::terminate_test_serial();
    let mut app = interactive(false);
    let NativeTerminateDecision::Dispatch(person) = admit_native_terminate(Asker::Person) else {
        panic!("a fresh termination request");
    };
    assert!(
        !app.native_termination_ready(person),
        "the person's quit asks"
    );
    assert_eq!(
        app.close_banner.as_ref().map(|pending| pending.replay),
        Some(CloseReplay::NativeTerminate { generation: person })
    );
    assert!(crate::menu::native_termination_is_current(person));
    assert!(!crate::system_quit::reopens());
    let crate::menu::TerminateReply::Later(taken) =
        crate::menu::ask_native_terminate(Asker::Reopen, true)
    else {
        panic!("Quit & Reopen onto the person's quit takes it up");
    };
    assert_eq!(taken, person);
    assert!(
        app.native_termination_ready(taken),
        "taken up, not asked again"
    );
    assert!(app.close_banner.is_none(), "the person's question went");
    assert!(!crate::menu::native_termination_is_current(person));
    assert!(crate::system_quit::reopens());
}

/// A person's own quit (the Dock's, AppleScript's) has AppKit wait on nothing while its
/// question stands: their Cancel owes macOS no NO, and their Quit ends the run on aterm's own
/// path. A logout or Quit & Reopen sent onto that standing question is put to aterm, and AppKit
/// waits on its reply: the question is withdrawn, the barriers run, the quit completes as the
/// system's or the reopen's (so the run's end keeps the hosted agents, or leaves the reopen
/// helper), and the loop's exit answers YES.
///
/// NEGATIVE CONTROL: answer the person's quit `NSTerminateLater` (the code at 0007e00dcf) and
/// AppKit waits on the person's answer, putting no logout or Quit & Reopen to aterm meanwhile:
/// it terminates at once, with no barriers, no mark, no reopen helper and no agents kept.
#[cfg(target_os = "macos")]
#[test]
fn a_logout_onto_the_persons_standing_question_takes_their_quit_up() {
    use crate::menu::{TerminateReply, ask_native_terminate, offer_terminate};
    use crate::system_quit::Asker;
    let person_quit = |app: &mut App| {
        app.headless = false;
        app.close_confirm = CloseConfirm::Interactive;
        let TerminateReply::TakeUp(generation) =
            offer_terminate(ask_native_terminate(Asker::Person, true), |_| true)
        else {
            panic!("the window takes the person's quit up");
        };
        assert!(
            !app.native_termination_ready(generation),
            "the person's quit asks"
        );
        assert!(
            !crate::menu::appkit_awaits_quit_reply(),
            "AppKit waits on no person's answer"
        );
        generation
    };
    for proceed in [false, true] {
        let _serial = crate::menu::terminate_test_serial();
        let mut app = App::headless_for_test();
        let generation = person_quit(&mut app);
        let replay = app.answer_close_banner(proceed);
        if proceed {
            assert_eq!(replay, Some(CloseReplay::NativeTerminate { generation }));
            app.close_confirm = CloseConfirm::Programmatic;
            assert!(app.native_termination_ready(generation));
        } else {
            assert_eq!(replay, None);
            assert!(!crate::menu::native_termination_is_current(generation));
        }
        assert!(!crate::menu::appkit_awaits_quit_reply(), "{proceed}");
        assert!(
            crate::menu::take_refusals_for_test().is_empty(),
            "{proceed}: macOS is owed no reply"
        );
    }
    for asker in [Asker::System, Asker::Reopen] {
        let _serial = crate::menu::terminate_test_serial();
        let mut app = App::headless_for_test();
        let person = person_quit(&mut app);
        let mut posted = None;
        let reply = offer_terminate(ask_native_terminate(asker, true), |generation| {
            posted = Some(generation);
            true
        });
        assert_eq!(reply, TerminateReply::Later(person), "{asker:?}");
        assert_eq!(posted, Some(person), "{asker:?}: the wake goes again");
        assert!(crate::menu::appkit_awaits_quit_reply(), "{asker:?}");
        assert!(
            app.native_termination_ready(person),
            "{asker:?}: taken up, not asked again"
        );
        assert!(app.close_banner.is_none(), "{asker:?}: the question went");
        assert!(!crate::menu::native_termination_is_current(person));
        assert!(
            crate::system_quit::asked(),
            "{asker:?}: the run's end keeps the agents"
        );
        assert_eq!(
            crate::system_quit::reopens(),
            asker == Asker::Reopen,
            "{asker:?}: the reopen helper"
        );
        assert!(
            crate::menu::appkit_awaits_quit_reply()
                && crate::menu::take_refusals_for_test().is_empty(),
            "{asker:?}: the loop's exit answers YES"
        );
    }
}

#[test]
fn a_second_window_close_waits_its_turn_and_is_asked_next() {
    let mut app = App::headless_for_test();
    app.headless = false;
    let a = WindowId(0);
    let sid = app.next_session_id;
    let b = app.insert_logical_window(crate::stub_session(sid), 24, 80);
    let sid = app.next_session_id;
    app.push_stub_tab(b, crate::stub_session(sid));
    assert!(!app.present_close_banner(a, busy_close_prompt(), CloseReplay::Window(a)));
    assert!(!app.present_close_banner(b, busy_close_prompt(), CloseReplay::Window(b)));
    assert_eq!(
        app.close_banner.as_ref().map(|p| p.wid),
        Some(a),
        "one question shows at a time"
    );
    assert_eq!(app.close_banner_queue.len(), 1, "the second waits its turn");
    assert!(
        app.windows[&b].pending_close_attribution.is_some(),
        "the waiting gesture's attribution rides to its replay"
    );
    let line = app
        .close_confirm_controls_line()
        .expect("a question stands");
    assert!(line.ends_with(" waiting=1"), "{line}");
    app.present_close_banner(b, busy_close_prompt(), CloseReplay::Window(b));
    app.present_close_banner(a, busy_close_prompt(), CloseReplay::Window(a));
    assert_eq!(app.close_banner_queue.len(), 1);
    {
        let _closing = crate::session_store::CloseAttribution::enter(
            crate::session_store::ExitReason::CtlClose,
            crate::session_store::ExitActor::Ctl,
        );
        let sid = app.next_session_id;
        let c = app.insert_logical_window(crate::stub_session(sid), 24, 80);
        assert!(!app.present_close_banner(c, busy_close_prompt(), CloseReplay::Window(c)));
        assert!(
            app.pending_action_refusal
                .take()
                .is_some_and(|why| why.starts_with("a window close ")),
            "the wire is told what stands"
        );
        assert_eq!(app.close_banner_queue.len(), 1);
        assert_eq!(app.close_window_logical(c), CloseOutcome::Stay);
    }
    assert_eq!(app.answer_close_banner(true), Some(CloseReplay::Window(a)));
    assert!(
        app.close_banner.is_none(),
        "asked after the replay, not before"
    );
    assert_eq!(app.close_window_logical(a), CloseOutcome::Stay);
    if cfg!(any(windows, target_os = "macos")) {
        let pending = app
            .close_banner
            .as_ref()
            .expect("B's question is asked next");
        assert_eq!(pending.wid, b);
        assert_eq!(
            crate::close_confirm::kind(&pending.prompt),
            "quit",
            "B is the last window now, so its close quits and its question says so"
        );
        assert!(app.close_banner_queue.is_empty());
        assert!(
            !app.close_confirm_controls_line()
                .unwrap()
                .contains("waiting=")
        );
    } else {
        assert!(app.close_banner.is_none());
        assert!(app.close_banner_queue.is_empty());
    }

    let mut app = App::headless_for_test();
    app.headless = false;
    let sid = app.next_session_id;
    let b = app.insert_logical_window(crate::stub_session(sid), 24, 80);
    let sid = app.next_session_id;
    app.push_stub_tab(b, crate::stub_session(sid));
    app.present_close_banner(a, busy_quit_prompt(), CloseReplay::Quit);
    app.present_close_banner(b, busy_close_prompt(), CloseReplay::Window(b));
    assert_eq!(app.answer_close_banner(false), None);
    if cfg!(not(windows)) {
        return;
    }
    let pending = app
        .close_banner
        .as_ref()
        .expect("the waiting question asks");
    assert_eq!(pending.wid, b);
    assert!(app.windows.contains_key(&a), "the cancel closed nothing");
    assert_eq!(app.close_window_logical(b), CloseOutcome::Stay);
    assert!(app.close_banner.is_none());
}
