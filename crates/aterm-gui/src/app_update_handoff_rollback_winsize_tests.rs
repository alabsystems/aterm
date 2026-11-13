// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::{App, term_lock};

fn winsize_of(fd: i32) -> (u16, u16) {
    // SAFETY: `winsize` is a valid zeroed out-parameter filled by `TIOCGWINSZ` on an open PTY fd.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: as above; `fd` is open for the whole test.
    assert_eq!(unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) }, 0);
    (ws.ws_row, ws.ws_col)
}

#[test]
fn a_rolled_back_commit_puts_each_parked_ptys_own_size_back() {
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    let mut app = App::headless_for_test();
    let session = &mut app.pool.sessions.get_mut(&0).expect("session 0").session;
    session.master = master;
    let parked = {
        let t = term_lock(&session.term);
        (t.rows(), t.cols())
    };
    aterm_pty::resize(master, parked.0, parked.1);
    aterm_pty::resize(master, parked.0 + 6, parked.1 + 20);
    assert_ne!(winsize_of(slave), parked, "the successor really moved it");
    app.rollback_overlap(None, &[(0, master, -1)]);
    assert_eq!(
        winsize_of(slave),
        parked,
        "the program resumes at the size its engine was parked at"
    );
    // SAFETY: both fds were opened above and remain exclusively owned here.
    unsafe {
        libc::close(master);
        libc::close(slave);
    }
}

#[test]
fn a_rollback_never_waits_on_a_held_engine_and_puts_its_size_back_once_free() {
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    let mut app = App::headless_for_test();
    let session = &mut app.pool.sessions.get_mut(&0).expect("session 0").session;
    session.master = master;
    let term = std::sync::Arc::clone(&session.term);
    let parked = {
        let t = term_lock(&term);
        (t.rows(), t.cols())
    };
    aterm_pty::resize(master, parked.0, parked.1);
    let moved = (parked.0 + 6, parked.1 + 20);
    aterm_pty::resize(master, moved.0, moved.1);
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (returned_tx, returned_rx) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let guard = term_lock(&term);
        held_tx.send(()).expect("the test waits");
        let waited_out = returned_rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .is_err();
        drop(guard);
        waited_out
    });
    held_rx.recv().expect("the engine is held");
    app.rollback_overlap(None, &[(0, master, -1)]);
    let size_while_held = winsize_of(slave);
    let _ = returned_tx.send(());
    let waited_out = holder.join().expect("the holder finished");
    assert!(!waited_out, "the rollback waited for the engine's holder");
    assert_eq!(
        size_while_held, moved,
        "PRECONDITION — the held engine's size is owed, not taken"
    );
    assert!(
        app.deferred_reader_resume_at.is_some(),
        "the owed size is the event loop's to retry"
    );
    app.resume_deferred_readers_nonblocking();
    assert_eq!(
        winsize_of(slave),
        parked,
        "the program resumes at the size its engine was parked at"
    );
    assert!(app.owed_parked_winsizes.is_empty(), "nothing is owed");
    // SAFETY: both fds were opened above and remain exclusively owned here.
    unsafe {
        libc::close(master);
        libc::close(slave);
    }
}
