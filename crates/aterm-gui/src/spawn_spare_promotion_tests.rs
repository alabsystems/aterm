// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::{Arc, mpsc};
use std::time::Instant;

use super::{attach_reader_inner, park_reader};
use crate::temporal::Exported;
use crate::{App, Wake, WindowId};

const PATIENCE: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;

fn write_shown(session: &crate::Session, slave: i32, bytes: &[u8], shown: &str) {
    // SAFETY: a bounded write of a live slice to the test-owned slave.
    let wrote = unsafe { libc::write(slave, bytes.as_ptr().cast(), bytes.len()) };
    assert_eq!(wrote, bytes.len() as isize);
    let seen = aterm_spec::wait::within(PATIENCE, std::time::Duration::ZERO, || {
        crate::control::screen_text(&crate::term_lock(&session.term))
            .contains(shown)
            .then_some(())
    });
    assert!(seen.is_some(), "timed out waiting for {shown:?} on screen");
}

/// A spare records nothing; promoted, it records from one keyframe of the screen it had, then every slice after (Rewind R4).
#[test]
fn a_promoted_spare_records_from_one_keyframe_of_its_screen() {
    let mut app = App::headless_for_test();
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    // SAFETY: A zeroed termios is a valid out-slot for tcgetattr, then cfmakeraw/tcsetattr on the test-owned slave fd.
    unsafe {
        let mut tio: libc::termios = std::mem::zeroed();
        assert_eq!(libc::tcgetattr(slave, &mut tio), 0, "tcgetattr");
        libc::cfmakeraw(&mut tio);
        assert_eq!(libc::tcsetattr(slave, libc::TCSANOW, &tio), 0, "tcsetattr");
    }
    let mut session = crate::stub_session(93);
    session.master = master;
    let (ready_tx, ready_rx) = mpsc::channel::<()>();
    let post = Arc::new(move |wake: Wake| {
        if matches!(wake, Wake::Ready { .. }) {
            let _ = ready_tx.send(());
        }
        true
    });
    let mut spare = app.session_factory.clone();
    spare.temporal_recording = false;
    attach_reader_inner(&mut session, WindowId(0), post, &spare, None)
        .expect("attach the real reader");
    ready_rx
        .recv_timeout(PATIENCE)
        .expect("the reader's gather started");
    write_shown(&session, slave, b"rc output\r\n", "rc output");
    assert_eq!(session.ctx.temporal.lock().unwrap().total_events(), 0);

    app.session_factory.temporal_recording = true;
    app.prepare_adopted_spare(&mut session, WindowId(0));
    write_shown(&session, slave, b"after adoption\r\n", "after adoption");
    session
        .temporal_drain
        .as_ref()
        .expect("its queue's drain")
        .wait();
    {
        let live = crate::control::screen_text(&crate::term_lock(&session.term));
        let rec = session.ctx.temporal.lock().unwrap();
        assert_eq!(rec.keyframe_count(), 1, "keyframes=1");
        let replayed = rec.replay_at(None).expect("a replay from the promotion");
        assert_eq!(
            crate::control::screen_text(&replayed),
            live,
            "the replay is the live screen"
        );
        let export = rec.export().expect("an export");
        let slices: Vec<&[u8]> = export
            .events
            .iter()
            .filter_map(|(_, ev)| match ev {
                Exported::Output(slice) => Some(slice.bytes()),
                _ => None,
            })
            .collect();
        assert_eq!(
            slices,
            [&b"after adoption\r\n"[..]],
            "only what came after it"
        );
    }
    assert!(park_reader(&mut session, Instant::now() + PATIENCE));
    let master = std::mem::replace(&mut session.master, -1);
    aterm_pty::close_fd(slave);
    aterm_pty::close_fd(master);
}
