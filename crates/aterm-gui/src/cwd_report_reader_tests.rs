// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Which cwd report sets a tab's folder, through the REAL reader
//! (docs/DESIGN-cwd-reports-2026-10-19.md): `attach_reader_inner`, the gather and parse pipeline
//! every session runs, on a real pseudo-terminal whose slave the test writes as the shell and its
//! programs.

use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use aterm_core::terminal::{ShellIntegrationNonce, Terminal};

use super::{attach_reader_inner, park_reader};
use crate::{App, Wake, WindowId};

/// Fake pids, never handed to a real `kill`: the shell (its group's leader)
/// and a program a key binding started in a group of its own.
const SHELL: i32 = 4100;
const JOB: i32 = 4200;

const KEY: [u8; 32] = [0x5D; 32];

/// A hang detector, never a latency budget.
const PATIENCE: Duration = aterm_spec::wait::MUST_HAPPEN;

/// The scripted foreground group (`SessionFactory::fg_probe`).
static FG: AtomicI32 = AtomicI32::new(SHELL);
/// When the shell's one child in its own group started, unix epoch ms (0: no
/// child runs), and which shell the probe was asked about.
static CHILD_STARTED: AtomicU64 = AtomicU64::new(0);
static ASKED_FOR: AtomicI32 = AtomicI32::new(0);

fn scripted_fg(_master: i32) -> i32 {
    FG.load(Ordering::SeqCst)
}

fn scripted_program(shell: i32, since_ms: u64) -> bool {
    ASKED_FOR.store(shell, Ordering::SeqCst);
    let started = CHILD_STARTED.load(Ordering::SeqCst);
    started != 0 && started >= since_ms
}

fn signed(seq: &str) -> Vec<u8> {
    format!("\x1b]{seq};id={}\x07", ShellIntegrationNonce(KEY).to_hex()).into_bytes()
}

/// The shell's own report, as every integration sends it before its prompt.
fn own_report(path: &str) -> Vec<u8> {
    [
        signed(&format!("633;P;AtermCwd=file://host{path}")),
        format!("\x1b]7;file://host{path}\x07").into_bytes(),
    ]
    .concat()
}

/// A program's report: an OSC 7 no one signed.
fn osc7(path: &str) -> Vec<u8> {
    format!("\x1b]7;file://host{path}\x07").into_bytes()
}

fn screen_has(t: &Terminal, needle: &str) -> bool {
    (0..usize::from(t.rows())).any(|r| t.row_text(r).is_some_and(|l| l.contains(needle)))
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
        app.session_factory.fg_probe = scripted_fg;
        app.session_factory.fg_gone = |_, _, _| false;
        app.session_factory.program_probe = scripted_program;
        let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
        // SAFETY: a zeroed termios is a valid out-slot for tcgetattr, then cfmakeraw/tcsetattr on
        // the test-owned slave fd.
        unsafe {
            let mut tio: libc::termios = std::mem::zeroed();
            assert_eq!(libc::tcgetattr(slave, &mut tio), 0, "tcgetattr");
            libc::cfmakeraw(&mut tio);
            assert_eq!(libc::tcsetattr(slave, libc::TCSANOW, &tio), 0, "tcsetattr");
        }
        let mut session = crate::stub_session(77);
        session.master = master;
        session.pid = SHELL;
        {
            let mut t = session.term.lock().expect("terminal lock");
            t.authorize_shell_integration(KEY);
            t.set_require_shell_integration_nonce(true);
        }
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

    /// Write `bytes` and then `marker` as the holder `fg`, and wait until the
    /// engine has parsed the marker.
    fn write(&self, fg: i32, bytes: &[u8], marker: &str) {
        FG.store(fg, Ordering::SeqCst);
        let all = [bytes, marker.as_bytes()].concat();
        // SAFETY: a bounded write of a live slice to the test-owned slave.
        let wrote = unsafe { libc::write(self.slave, all.as_ptr().cast(), all.len()) };
        assert_eq!(wrote, all.len() as isize, "write {marker}");
        let seen = aterm_spec::wait::within(PATIENCE, Duration::from_millis(1), || {
            screen_has(&self.session.term.lock().expect("terminal lock"), marker).then_some(())
        });
        assert!(seen.is_some(), "timed out waiting for {marker}");
    }

    fn cwd(&self) -> Option<String> {
        self.session
            .term
            .lock()
            .expect("terminal lock")
            .current_working_directory()
            .map(str::to_owned)
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

/// THE READER SAYS WHO HOLDS THE TERMINAL. At a signing shell's prompt, a key-bound program's
/// report sets the folder while it runs, whether it holds the foreground in a group of its own
/// or runs in the shell's group, where the reader's program probe sees it; a report the shell's
/// group writes while no such program runs is a stray, dropped; and the shell's first output
/// after the program is gone brings the prompt's folder back.
#[test]
fn the_reader_lets_a_key_bound_programs_report_set_the_folder_while_it_runs() {
    let rig = Rig::attach();
    let prompt = |cwd: &str| {
        [
            own_report(cwd),
            signed("133;A"),
            b"$ ".to_vec(),
            signed("133;B"),
        ]
        .concat()
    };
    rig.write(SHELL, &prompt("/home/me"), "<p1>");
    rig.write(
        SHELL,
        &[
            &b"\r\ntrue\r\n"[..],
            &signed("133;C"),
            &signed("133;D;0"),
            &prompt("/home/me"),
        ]
        .concat(),
        "<p2>",
    );
    assert_eq!(rig.cwd().as_deref(), Some("/home/me"));

    rig.write(JOB, &osc7("/srv/app"), "<job>");
    assert_eq!(
        rig.cwd().as_deref(),
        Some("/srv/app"),
        "the program's report"
    );
    rig.write(SHELL, b"\r$ ", "<redraw>");
    assert_eq!(
        rig.cwd().as_deref(),
        Some("/home/me"),
        "the prompt's folder"
    );

    rig.write(SHELL, &osc7("/elsewhere"), "<stray>");
    assert_eq!(rig.cwd().as_deref(), Some("/home/me"), "a stray is dropped");

    CHILD_STARTED.store(u64::MAX, Ordering::SeqCst);
    rig.write(SHELL, &osc7("/srv/logs"), "<own-group>");
    assert_eq!(
        ASKED_FOR.load(Ordering::SeqCst),
        SHELL,
        "asked about the shell"
    );
    assert_eq!(
        rig.cwd().as_deref(),
        Some("/srv/logs"),
        "the probe's program"
    );
    rig.write(SHELL, b"\r\n", "<still>");
    assert_eq!(rig.cwd().as_deref(), Some("/srv/logs"), "it still runs");
    CHILD_STARTED.store(0, Ordering::SeqCst);
    rig.write(SHELL, b"\r$ ", "<gone>");
    assert_eq!(
        rig.cwd().as_deref(),
        Some("/home/me"),
        "the prompt's folder"
    );
}
