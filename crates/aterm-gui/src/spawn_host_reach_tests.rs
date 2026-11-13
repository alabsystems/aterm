// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::mpsc::{Receiver, RecvTimeoutError, channel, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aterm_containment::ContainmentMode as Cm;
use aterm_core::config::TerminalConfig;
use aterm_core::terminal::Terminal;

use super::{
    ClipWrite, configure_notifications, host_reach, install_clipboard, new_live_terminal, term_lock,
};

const HANG: Duration = Duration::from_secs(60);

const SET: &[u8] = b"\x1b]52;c;cGxhbnRlZA==\x07";

fn query_allowed() -> TerminalConfig {
    TerminalConfig {
        allow_osc52_query: true,
        ..TerminalConfig::default()
    }
}

fn session(mode: Cm) -> (Arc<Mutex<Terminal>>, Receiver<ClipWrite>) {
    let term = Arc::new(Mutex::new(new_live_terminal(
        24,
        80,
        Some(&query_allowed()),
        None,
    )));
    let (tx, rx) = channel();
    install_clipboard(&term, host_reach(mode), move |write| {
        let _ = tx.send(write);
    });
    term_lock(&term).apply_policy_engine(aterm_policy::engine::PolicyEngine::new(
        aterm_policy::profiles::standard(),
    ));
    (term, rx)
}

fn query_reply(term: &Arc<Mutex<Terminal>>) -> Option<Vec<u8>> {
    crate::control::PBPASTE_STUB.with(|b| *b.borrow_mut() = Some("owner-secret".into()));
    crate::host_io_prefetch::feed_as_reader(term, b"\x1b]52;c;?\x07");
    let reply = term_lock(term).take_response();
    crate::control::PBPASTE_STUB.with(|b| *b.borrow_mut() = None);
    reply
}

#[test]
#[cfg(not(target_os = "linux"))]
fn an_osc52_query_reads_the_pasteboard_before_the_terminal_lock() {
    let (term, _writes) = session(Cm::User);
    crate::control::PBPASTE_STUB.with(|b| *b.borrow_mut() = Some("owner-secret".into()));
    term_lock(&term).process(b"\x1b]52;c;?\x07");
    let in_the_hold = term_lock(&term).take_response();
    crate::control::PBPASTE_STUB.with(|b| *b.borrow_mut() = None);
    assert_eq!(
        in_the_hold, None,
        "the pasteboard was read under the terminal lock"
    );
    crate::control::PBPASTE_STUB.with(|b| *b.borrow_mut() = Some("owner-secret".into()));
    crate::host_io_prefetch::feed_as_reader(&term, b"\x1b]52;c;?\x07\x1b[c");
    let reply = term_lock(&term).take_response().unwrap_or_default();
    crate::control::PBPASTE_STUB.with(|b| *b.borrow_mut() = None);
    assert!(
        reply.starts_with(b"\x1b]52;c;b3duZXItc2VjcmV0\x07\x1b[?"),
        "{:?}",
        String::from_utf8_lossy(&reply)
    );
}

#[test]
fn the_clipboard_worker_starts_with_the_first_write() {
    crate::work_counts::take_clipboard_workers_started();
    let (term, writes) = session(Cm::User);
    assert_eq!(
        crate::work_counts::take_clipboard_workers_started(),
        0,
        "no worker for a session that wrote nothing"
    );
    term_lock(&term).process(SET);
    let first = writes.recv_timeout(HANG).expect("the first write arrives");
    assert_eq!(first.clipboard.as_deref(), Some("planted"));
    term_lock(&term).process(SET);
    writes.recv_timeout(HANG).expect("the second write arrives");
    assert_eq!(
        crate::work_counts::take_clipboard_workers_started(),
        1,
        "one worker, started by the first write"
    );
}

#[test]
fn a_contained_session_reaches_no_clipboard() {
    let answer = b"\x1b]52;c;b3duZXItc2VjcmV0\x07".to_vec();
    for mode in [Cm::User, Cm::Safety] {
        let (term, writes) = session(mode);
        term_lock(&term).process(SET);
        let write = writes
            .recv_timeout(HANG)
            .unwrap_or_else(|e| panic!("{mode}: the OSC 52 write never arrived ({e:?})"));
        assert_eq!(write.clipboard.as_deref(), Some("planted"), "{mode}");
        assert_eq!(query_reply(&term), Some(answer.clone()), "{mode}");
    }

    let (term, writes) = session(Cm::Containment);
    term_lock(&term).process(SET);
    match writes.recv_timeout(HANG) {
        Err(RecvTimeoutError::Disconnected) => {}
        Ok(write) => panic!(
            "a contained OSC 52 write reached the clipboard: {:?}",
            write.clipboard
        ),
        Err(RecvTimeoutError::Timeout) => {
            panic!("the contained session kept a clipboard writer")
        }
    }
    assert!(
        !term_lock(&term).modes().allow_osc52_set,
        "a contained session holds no clipboard write authority"
    );
    assert_eq!(query_reply(&term), None, "a contained query is answered");
    term_lock(&term).apply_config(&query_allowed());
    assert_eq!(
        query_reply(&term),
        None,
        "a contained query is answered after a reload"
    );
}

fn notifications_allowed() -> TerminalConfig {
    TerminalConfig {
        allow_notifications: true,
        ..TerminalConfig::default()
    }
}

const NOTICES: [&[u8]; 3] = [
    b"\x1b]9;from the tab\x07",
    b"\x1b]99;;from the tab\x1b\\",
    b"\x1b]777;notify;title;from the tab\x07",
];

fn posted(term: &Arc<Mutex<Terminal>>, notices: &Receiver<super::notify::NotifyMsg>) -> usize {
    for notice in NOTICES {
        term_lock(term).process(notice);
    }
    notices.try_iter().count()
}

#[test]
fn a_contained_session_posts_no_notification() {
    for mode in [Cm::User, Cm::Safety, Cm::Containment] {
        let term = Arc::new(Mutex::new(new_live_terminal(
            24,
            80,
            Some(&notifications_allowed()),
            None,
        )));
        let (tx, notices) = sync_channel(8);
        configure_notifications(&term, host_reach(mode), &tx, 1, &Arc::default());
        term_lock(&term).apply_policy_engine(aterm_policy::engine::PolicyEngine::new(
            aterm_policy::profiles::standard(),
        ));
        if mode == Cm::Containment {
            assert_eq!(
                posted(&term, &notices),
                0,
                "a contained program posted a desktop notification"
            );
            term_lock(&term).apply_config(&notifications_allowed());
            assert_eq!(
                posted(&term, &notices),
                0,
                "a contained program posted a desktop notification after a reload"
            );
        } else {
            assert_eq!(posted(&term, &notices), NOTICES.len(), "{mode}");
        }
    }
}

/// A PROGRAM'S NOTIFICATION PASSES WHILE ITS BOX STANDS: a codex `blocked` report standing in
/// the session holds none of its program's notifications off. OSC 9, OSC 99 and OSC 777 are
/// each queued once, with their words, while the box stands, and again once it is answered.
#[test]
fn a_program_notification_passes_while_its_box_stands() {
    const BLOCKED: &[u8] = b"\x1b]7501;state=blocked:app=codex:kind=permission\x1b\\";
    const WORKING: &[u8] = b"\x1b]7501;state=working:app=codex\x1b\\";
    let term = Arc::new(Mutex::new(new_live_terminal(
        24,
        80,
        Some(&notifications_allowed()),
        None,
    )));
    let (tx, notices) = sync_channel(8);
    configure_notifications(&term, host_reach(Cm::User), &tx, 1, &Arc::default());
    term_lock(&term).process(&[WORKING, BLOCKED].concat());
    assert!(
        term_lock(&term)
            .program_state()
            .state
            .is_some_and(|report| report.state
                == aterm_core::terminal::program_state::ProgramStateWord::Blocked),
        "PRECONDITION: the box stands"
    );
    for notice in NOTICES {
        term_lock(&term).process(notice);
        let passed: Vec<(u64, String)> = notices
            .try_iter()
            .map(|msg| {
                (
                    msg.session,
                    [msg.title.unwrap_or_default(), msg.body].join(" "),
                )
            })
            .collect();
        let escaped = notice.escape_ascii().to_string();
        assert_eq!(passed.len(), 1, "{escaped}: queued while the box stands");
        assert_eq!(passed[0].0, 1, "{escaped}: the session's own");
        assert!(
            passed[0].1.contains("from the tab"),
            "{escaped}: {passed:?}"
        );
    }
    term_lock(&term).process(WORKING);
    assert_eq!(
        posted(&term, &notices),
        NOTICES.len(),
        "and once the box is answered"
    );
}
