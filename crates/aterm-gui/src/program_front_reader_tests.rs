// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The OSC 7501 run sign's front through `attach_reader_inner` on a live PTY: the parse stage
//! says each slice's group to the engine (`Terminal::program_front_is`).

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;

use super::{attach_reader_inner, park_reader};
use crate::{App, Wake, WindowId};

const SHELL: i32 = 4100;
const CLAUDE: i32 = 4200;
const NEXT_CLAUDE: i32 = 4300;

const WORKING: &str = "state=working:app=claude-code";
const IDLE: &str = "state=idle:app=claude-code";

const PATIENCE: Duration = aterm_spec::wait::MUST_HAPPEN;

static FG: AtomicI32 = AtomicI32::new(SHELL);

fn scripted_fg(_master: i32) -> i32 {
    FG.load(Ordering::SeqCst)
}

fn report(body: &str) -> Vec<u8> {
    format!("\x1b]7501;{body}\x07").into_bytes()
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
        let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
        // SAFETY: a zeroed termios is a valid out-slot for tcgetattr, then cfmakeraw/tcsetattr on
        // the test-owned slave fd.
        unsafe {
            let mut tio: libc::termios = std::mem::zeroed();
            assert_eq!(libc::tcgetattr(slave, &mut tio), 0, "tcgetattr");
            libc::cfmakeraw(&mut tio);
            assert_eq!(libc::tcsetattr(slave, libc::TCSANOW, &tio), 0, "tcsetattr");
        }
        let mut session = crate::stub_session(7501);
        session.master = master;
        session.pid = SHELL;
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

    /// Write `bytes`, then `marker`, as group `fg`'s (the group in front when the reader reads
    /// them), and wait until the marker is drawn.
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

    /// The report standing (what a checkpoint carries) and the run sign the readers take.
    fn signs(&self) -> (Option<String>, Option<String>) {
        let report = self
            .session
            .term
            .lock()
            .expect("terminal lock")
            .program_state();
        (report.carried(), report.run_sign())
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

/// A CLAUDE CODE KILLED MID-TURN LEAVES NO RUN SIGN: its `working` report outlives it in the
/// engine, and nothing the shell that takes the terminal back writes overwrites it. The reader
/// says each slice's group as it parses it, so while the shell's bytes are parsed the report
/// stands (a checkpoint still carries it) and is no run sign, what `text --json` and every reader
/// take. Stopped mid-turn (`^Z`) and resumed (`fg`), Claude Code writes no report again, and its
/// own `working` is its run sign again once its bytes are parsed. A later program's bytes with no
/// report of its own leave none; its own report is one. CONTROL: more of the same group's bytes
/// keep the report the sign.
#[test]
fn a_report_is_a_run_sign_only_while_its_writers_bytes_are_parsed() {
    let rig = Rig::attach();
    rig.write(SHELL, b"% claude\r\n", "[shell]");
    rig.write(CLAUDE, &report(WORKING), "[reply]");
    let working = (Some(WORKING.to_string()), Some(WORKING.to_string()));
    let left = (Some(WORKING.to_string()), None);
    assert_eq!(rig.signs(), working, "its writer's bytes");
    rig.write(CLAUDE, b"\r\n", "[reply more]");
    assert_eq!(rig.signs(), working, "CONTROL: the same group's bytes");
    rig.write(SHELL, b"\r\nzsh: suspended  claude\r\n% ", "[stopped]");
    assert_eq!(rig.signs(), left, "stopped: the shell in front");
    rig.write(CLAUDE, b"\r\n", "[resumed]");
    assert_eq!(rig.signs(), working, "resumed: its writer's bytes again");
    rig.write(SHELL, b"\r\nzsh: killed     claude\r\n% ", "[reclaimed]");
    assert_eq!(rig.signs(), left, "killed mid-turn: its writer gone");
    rig.write(NEXT_CLAUDE, b"\r\n", "[next drawn]");
    assert_eq!(rig.signs(), left, "a later program that wrote no report");
    rig.write(NEXT_CLAUDE, &report(IDLE), "[next]");
    let idle = Some(IDLE.to_string());
    assert_eq!(rig.signs(), (idle.clone(), idle), "the next program's own");
}
