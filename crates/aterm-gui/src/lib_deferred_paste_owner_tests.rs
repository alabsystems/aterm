// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;

use aterm_session::sink::SinkWriter;

use super::{App, Source, WindowId};
use crate::input::PasteFraming;

fn observer_pipe() -> (i32, Arc<SinkWriter>) {
    use std::os::fd::IntoRawFd;
    let pipe = aterm_uds::pipe()
        .map(|(r, w)| [r.into_raw_fd(), w.into_raw_fd()])
        .expect("observer pipe");
    let flags = unsafe { libc::fcntl(pipe[0], libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(pipe[0], libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    (pipe[0], Arc::new(SinkWriter::new(pipe[1])))
}

fn drain_within(fd: i32, ms: u64) -> Vec<u8> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    let mut out = Vec::new();
    let mut buf = [0u8; 256];
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 {
            out.extend_from_slice(&buf[..n as usize]);
            continue;
        }
        if !out.is_empty() || std::time::Instant::now() >= deadline {
            return out;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn a_confirmed_paste_reaches_its_owner_after_a_tab_switch_not_the_new_front() {
    let (owner_rx, owner_sink) = observer_pipe();
    let (front_rx, front_sink) = observer_pipe();
    let mut app = App::headless_for_test_with_sink(owner_sink);
    let wid = WindowId(0);
    let owner = app
        .focused_session_id(wid)
        .expect("front session at gesture");

    let new_id = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session_with_sink(new_id, front_sink));
    assert_ne!(
        app.focused_session_id(wid),
        Some(owner),
        "the switch really moved the front off the owner"
    );

    app.deliver_paste_confirmed(
        wid,
        Some(owner),
        "ls\nrm -rf ~".to_string(),
        Source::Human,
        PasteFraming::Gesture { bracketed: false },
    );

    assert_eq!(
        drain_within(owner_rx, 60_000),
        b"ls\rrm -rf ~",
        "the confirmed paste reaches the session that owned the gesture"
    );
    assert_eq!(
        drain_within(front_rx, 0),
        b"",
        "the tab that came to the front receives nothing"
    );

    unsafe {
        libc::close(owner_rx);
        libc::close(front_rx);
    }
}
