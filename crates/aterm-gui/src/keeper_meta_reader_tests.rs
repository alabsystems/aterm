// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The PTY reader's rings of the keeper's META pump, through `attach_reader_inner` on a live PTY:
//! the factory's [`MetaRing`] is this test's own, which records, on the reader's thread, each ring
//! and what a META's fingerprint read as it was made.

use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError, mpsc};
use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;

use super::{attach_reader_inner, park_reader};
use crate::keeper_link::{Fingerprint, MetaRing};
use crate::{App, Wake, WindowId};

const PATIENCE: Duration = aterm_spec::wait::MUST_HAPPEN;

/// The rig's shell, always in front.
const SHELL: i32 = 4_300;

/// The rig's terminal, which [`heard`] reads.
static TERM: OnceLock<Arc<Mutex<Terminal>>> = OnceLock::new();

/// Each ring the reader made, in order: what a META's fingerprint read as it rang, or `None`
/// for a ring made while the reader still held the terminal.
static RUNG: (Mutex<Vec<Option<Fingerprint>>>, Condvar) = (Mutex::new(Vec::new()), Condvar::new());

/// The rig's ring, on the reader's thread.
fn heard() {
    let fingerprint = TERM
        .get()
        .filter(|_| !crate::term_lock_held_on_this_thread())
        .map(|term| Fingerprint::of(&crate::term_lock(term)));
    let (rung, noted) = &RUNG;
    rung.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(fingerprint);
    noted.notify_all();
}

/// The rings so far, once there are `n`, under the one-minute hang detector.
fn rung_until(n: usize) -> Vec<Option<Fingerprint>> {
    let (rung, noted) = &RUNG;
    let (rung, waited) = noted
        .wait_timeout_while(
            rung.lock().unwrap_or_else(PoisonError::into_inner),
            PATIENCE,
            |rung| rung.len() < n,
        )
        .unwrap_or_else(PoisonError::into_inner);
    assert!(
        !waited.timed_out(),
        "the reader never rang {n} times: {:?}",
        *rung
    );
    rung.clone()
}

struct Rig {
    session: crate::Session,
    slave: i32,
    /// Kept alive for the reader's factory.
    _app: App,
}

impl Rig {
    fn attach() -> Self {
        let mut app = App::headless_for_test();
        app.session_factory.fg_probe = |_| SHELL;
        app.session_factory.fg_gone = |_, _, _| false;
        app.session_factory.program_probe = |_, _| false;
        app.session_factory.meta_ring = MetaRing {
            on: || true,
            ring: heard,
        };
        let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
        // SAFETY: a zeroed termios is a valid out-slot for tcgetattr, then cfmakeraw/tcsetattr on
        // the test-owned slave fd.
        unsafe {
            let mut tio: libc::termios = std::mem::zeroed();
            assert_eq!(libc::tcgetattr(slave, &mut tio), 0, "tcgetattr");
            libc::cfmakeraw(&mut tio);
            assert_eq!(libc::tcsetattr(slave, libc::TCSANOW, &tio), 0, "tcsetattr");
        }
        let mut session = crate::stub_session(7_301);
        session.master = master;
        session.pid = SHELL;
        assert!(
            TERM.set(Arc::clone(&session.term)).is_ok(),
            "one rig per test binary"
        );
        let (ready_tx, ready_rx) = mpsc::channel::<()>();
        let post = Arc::new(move |wake: Wake| {
            if matches!(wake, Wake::Ready { .. }) {
                let _ = ready_tx.send(());
            }
            true
        });
        attach_reader_inner(&mut session, WindowId(0), post, &app.session_factory, None)
            .expect("attach the real reader");
        ready_rx
            .recv_timeout(PATIENCE)
            .expect("the reader's gather started");
        Self {
            session,
            slave,
            _app: app,
        }
    }

    /// The program writes `bytes`.
    fn write(&self, bytes: &[u8]) {
        // SAFETY: a bounded write of a live slice to the test-owned slave.
        let wrote = unsafe { libc::write(self.slave, bytes.as_ptr().cast(), bytes.len()) };
        assert_eq!(wrote, bytes.len() as isize, "write {bytes:?}");
    }

    /// Once the reader has parsed `marker` onto the screen.
    fn shown(&self, marker: &str) {
        let seen = aterm_spec::wait::within(PATIENCE, Duration::from_millis(1), || {
            let t = crate::term_lock(&self.session.term);
            (0..usize::from(t.rows()))
                .any(|r| t.row_text(r).is_some_and(|l| l.contains(marker)))
                .then_some(())
        });
        assert!(seen.is_some(), "the reader never showed {marker}");
    }

    /// What a META's fingerprint reads now.
    fn fingerprint(&self) -> Fingerprint {
        Fingerprint::of(&crate::term_lock(&self.session.term))
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = park_reader(&mut self.session, Instant::now() + PATIENCE);
        let master = std::mem::replace(&mut self.session.master, -1);
        aterm_pty::close_fd(self.slave);
        aterm_pty::close_fd(master);
    }
}

/// The real reader rings the META pump for its first sight and then once for each slice that
/// moved what a META's fingerprint reads, never for plain output, always with its hold dropped;
/// and once for a slice that puts back what a host-side change (a reset) took away, though that
/// slice ends where the reader's last one did. Each absence is shown by the next ring: rings are
/// made in order on the reader's one thread, and the plain output is on the screen (its slice
/// parsed) before the next write, so a ring for it would be the one awaited after it.
#[test]
fn the_reader_rings_the_meta_pump_for_each_slice_that_moved_the_fingerprint() {
    let rig = Rig::attach();
    rig.write(b"ready\r\n");
    rung_until(1);
    let blank = rig.fingerprint();
    assert_eq!(
        rung_until(1),
        [Some(blank)],
        "the first sight, with the hold dropped"
    );

    rig.write(b"\x1b[?2004hpaste\r\n");
    rung_until(2);
    let paste = rig.fingerprint();
    assert_ne!(paste, blank, "premise: bracketed paste is carried");
    assert_eq!(rung_until(2), [Some(blank), Some(paste)]);

    rig.write(b"plain output\r\n");
    rig.shown("plain output");
    rig.write(b"\x1b[?1hkeys\r\n");
    rung_until(3);
    let keys = rig.fingerprint();
    assert_ne!(keys, paste, "premise: cursor keys are carried");
    assert_eq!(
        rung_until(3),
        [Some(blank), Some(paste), Some(keys)],
        "plain output rang nothing: the third ring is the cursor keys'"
    );

    // Edit > Reset Terminal, outside any reader slice (its own ring is `manual_reset::record`'s).
    crate::term_lock(&rig.session.term).process(b"\x1b[?1l");
    assert_eq!(
        rig.fingerprint(),
        paste,
        "premise: the reset undid the cursor keys"
    );
    rig.write(b"\x1b[?1hagain\r\n");
    rig.shown("again");
    rig.write(b"\x1b[?2004loff\r\n");
    assert_eq!(
        rung_until(4)[3],
        Some(keys),
        "the program's write after the reset rang, so the keeper hears its cursor keys again"
    );
    rung_until(5);
    let off = rig.fingerprint();
    assert_ne!(off, keys, "premise: bracketed paste off is carried");
    assert_eq!(
        rung_until(5),
        [Some(blank), Some(paste), Some(keys), Some(keys), Some(off)]
    );
}
