// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 of the repaint nudge and of the render debt's handoff carry (R07,
//! R08): the shipping `App` tick, the real resize ledger, the real engine and a
//! REAL pty pair, in process - no window, no WindowServer (a headless `App`
//! and a raw pty; the stand-in program lives on the slave end).
//!
//! The stand-in is a SIZE-COMPARING full-screen program, the class Claude Code
//! belongs to: it reads the kernel's `TIOCGWINSZ` of its tty and repaints (a
//! full clear and its whole UI) only when the size it reads CHANGED from the
//! size it last read. So a flap it never saw twice leaves it drawing onto a
//! screen the engine moved, and only a nudge that makes it read each size
//! repairs it.
//!
//! Each schedule runs the real event-loop steps in order - the epoch look
//! (`observe_render_epochs`), then the nudge tick (`tick_nudges`) - against
//! real time (the risk wait is one second, the never-touched person gate one
//! and a half), so a schedule takes about four seconds, and asserts the
//! outcome, not a latency: waits are hang detectors.
//!
//! WHAT IS ASSERTED. A session ADOPTED with a seeded debt (R08: the successor
//! judges the tab natively) is repaired once: the stand-in read the short size
//! and then the true one, the engine's rows never moved, its debt is gone, the
//! kernel's size ends at the engine's, and the ledger closed the epoch
//! `by=nudge`. NEGATIVE CONTROLS: the same stand-in adopted with NO debt (a
//! debt-free predecessor, an older one, an emacs-shaped program whose pulse
//! answer is zero bytes) is never held short; mode `log` holds nothing and
//! says what it would have done; a program that is not Claude Code is never
//! held; and a stand-in that never answers is let go at the engine's size.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_core::terminal::repaint_nudge::Mode;
use aterm_session::sink::SinkWriter;

use crate::input_stall::tests::raw_pty_pair;
use crate::{App, WindowId};

const ROWS: u16 = 12;
const COLS: u16 = 40;

/// How the stand-in program answers a size it reads as changed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// A full clear and its whole UI (Claude Code).
    Repaint,
    /// Nothing at all (a program that will not, or cannot).
    Silent,
}

struct Rig {
    app: App,
    master: i32,
    slave: i32,
    /// The window's grid: the engine's size, which only a real resize moves.
    window: (u16, u16),
    /// What the stand-in last read, and every change it read.
    read: (u16, u16),
    sizes: Vec<(u16, u16)>,
    answer: Answer,
}

fn frame(rows: u16, why: &str) -> Vec<u8> {
    let mut out = b"\x1b[H\x1b[2J".to_vec();
    for r in 0..rows.saturating_sub(1) {
        out.extend_from_slice(format!("\x1b[{};1H{why} row {r}", r + 1).as_bytes());
    }
    out
}

fn tty_size(fd: i32) -> (u16, u16) {
    // SAFETY: `winsize` is plain integers; zeroed is valid and the ioctl fills it.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is an open terminal descriptor and `ws` a valid out-param.
    let rc = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) };
    assert_eq!(rc, 0, "TIOCGWINSZ");
    (ws.ws_row, ws.ws_col)
}

impl Rig {
    /// An `App` whose session 0 owns a real pty master, the engine on the
    /// alternate screen with every row painted (the incident's shape), a flap's
    /// residue as the render debt when `debt`, and the presence classifier
    /// reading Claude Code when `agent`.
    fn nudge_rig(mode: Mode, debt: bool, agent: bool, answer: Answer) -> Self {
        let (master, slave) = raw_pty_pair();
        aterm_pty::set_nonblocking(master, true).expect("nonblocking master");
        aterm_pty::resize(master, ROWS, COLS);
        let sink = Arc::new(SinkWriter::new(master));
        sink.note_master_nonblocking(true);
        let mut app = App::headless_for_test_with_sink(sink);
        app.nudges = crate::app_nudge::NudgeHost::new(mode);
        let wid = WindowId(0);
        app.windows.get_mut(&wid).expect("the window").focused = true;
        app.frontmost_window = Some(wid);
        {
            let pooled = app.pool.sessions.get_mut(&0).expect("session 0");
            pooled.session.master = master;
        }
        let session = app.pool.get(0).expect("session 0");
        {
            let mut t = crate::term_lock(&session.term);
            *t = aterm_core::terminal::Terminal::new(ROWS, COLS);
            t.process(b"\x1b[?1049h");
            t.process(&frame(ROWS, "painted"));
        }
        session
            .ctx
            .timeline
            .lock()
            .unwrap()
            .set_agent_reader_for_test(agent);
        if debt {
            // The predecessor's engine owed a row: what `spawn_session` seeds
            // from the handoff record (R08), and books the ledger's first look.
            crate::spawn::seed_adopted_render_debt(&session.term, &session.ctx.timeline, (-1, 0));
            assert_eq!(crate::term_lock(&session.term).render_debt(), (-1, 0));
        }
        Self {
            app,
            master,
            slave,
            window: (ROWS, COLS),
            read: (ROWS, COLS),
            sizes: Vec::new(),
            answer,
        }
    }

    fn engine(&self) -> (u16, u16) {
        let t = crate::term_lock(&self.app.pool.get(0).unwrap().term);
        (t.rows(), t.cols())
    }

    /// One pass of the "machine": the stand-in reads its size and answers,
    /// the reader carries the program's bytes into the engine and stamps the
    /// session's output clock, then the event loop's two steps run.
    fn pump(&mut self) {
        let now = tty_size(self.slave);
        if now != self.read {
            self.read = now;
            self.sizes.push(now);
            if self.answer == Answer::Repaint {
                let bytes = frame(now.0, "repainted");
                // SAFETY: `slave` is open and the slice is valid for its length.
                let n = unsafe { libc::write(self.slave, bytes.as_ptr().cast(), bytes.len()) };
                assert!(n > 0, "the program writes its frame");
            }
        }
        let mut got = false;
        let mut buf = [0u8; 4096];
        loop {
            // SAFETY: `master` is open and `buf` writable for its length.
            let n = unsafe { libc::read(self.master, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                break;
            }
            let bytes = &buf[..usize::try_from(n).unwrap_or(0)];
            crate::term_lock(&self.app.pool.get(0).unwrap().term).process(bytes);
            got = true;
        }
        if got {
            let ns = u64::try_from(
                Instant::now()
                    .saturating_duration_since(self.app.lat_epoch)
                    .as_nanos(),
            )
            .unwrap_or(u64::MAX);
            self.app
                .pool
                .get(0)
                .unwrap()
                .latest_output_activity_ns
                .store(ns.max(1), std::sync::atomic::Ordering::Release);
        }
        // The epoch look is a process-wide minimum that any other `App`'s walk
        // resets (one `App` per process in production; the test harness runs
        // several in parallel): re-register what this ledger still owes.
        if let Some(at) = self
            .app
            .pool
            .get(0)
            .unwrap()
            .ctx
            .timeline
            .lock()
            .unwrap()
            .resizes()
            .look_at_ms()
        {
            crate::resize_epoch::note_look(at);
        }
        self.app.observe_render_epochs();
        self.app.tick_nudges();
        // The engine is the window's: the nudge never resizes it.
        assert_eq!(self.engine(), self.window, "the engine was never resized");
    }

    /// A real resize of the window to `rows` (the control `resize` funnel: the
    /// engine, the PTY, and the ledger's booking).
    fn resize(&mut self, rows: u16) {
        self.app.apply_grid_resize(rows, COLS);
        self.window = (rows, COLS);
        assert_eq!(self.engine(), self.window, "the resize moved the engine");
    }

    /// The digest's `render.<tab>.unrepaired` line stands.
    fn render_line(&self) -> bool {
        self.app
            .messages
            .digest()
            .entries
            .iter()
            .any(|entry| entry.key.starts_with("render."))
    }

    /// Pump until `done`, for at most `limit` (a hang detector, not a budget).
    fn until(&mut self, limit: Duration, mut done: impl FnMut(&mut Rig) -> bool) -> bool {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            self.pump();
            if done(self) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    fn header(&self) -> String {
        let session = self.app.pool.get(0).unwrap();
        let tl = session.ctx.timeline.lock().unwrap();
        tl.resizes().reply(0, None)
    }

    fn debt(&self) -> (i32, i32) {
        crate::term_lock(&self.app.pool.get(0).unwrap().term).render_debt()
    }

    fn pty(&self) -> Option<(u16, u16)> {
        aterm_pty::size(self.master)
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        aterm_pty::close_fd(self.master);
        aterm_pty::close_fd(self.slave);
    }
}

/// THE REPAIR, end to end: adopted with a seeded debt, the size-comparing
/// stand-in that the adoption pulse could not make repaint is repainted by the
/// closed loop, once, with the engine untouched.
#[test]
fn an_adopted_tab_that_owes_rows_is_repainted_by_the_closed_loop_once() {
    let mut rig = Rig::nudge_rig(Mode::On, true, true, Answer::Repaint);
    let done = rig.until(Duration::from_secs(60), |r| {
        r.debt() == (0, 0) && r.header().contains(" nudge=idle nudged=1")
    });
    assert!(done, "never repaired:\n{}", rig.header());
    assert_eq!(
        rig.sizes,
        [(ROWS - 1, COLS), (ROWS, COLS)],
        "the program READ the short size and then the true one"
    );
    assert_eq!(
        rig.pty(),
        Some((ROWS, COLS)),
        "the kernel ends at the engine's"
    );
    assert_eq!(rig.engine(), (ROWS, COLS));
    let reply = rig.header();
    assert!(reply.contains("verdict=healed by=nudge tried=1"), "{reply}");
    assert!(
        reply.contains(" total=0 "),
        "the nudge booked no resize: {reply}"
    );
    // Once: more time, no second hold.
    let again = rig.until(Duration::from_secs(4), |r| r.sizes.len() > 2);
    assert!(!again, "a second attempt: {:?}", rig.sizes);
}

/// NEGATIVE CONTROL: a debt-free predecessor (or one from before the field, or
/// an emacs-shaped program whose pulse answer is zero bytes) hands over no
/// debt, and the identical stand-in is never held short.
#[test]
fn a_tab_adopted_with_no_debt_is_never_held_short() {
    let mut rig = Rig::nudge_rig(Mode::On, false, true, Answer::Repaint);
    let held = rig.until(Duration::from_secs(5), |r| !r.sizes.is_empty());
    assert!(!held, "held short: {:?}", rig.sizes);
    assert_eq!(rig.pty(), Some((ROWS, COLS)));
    assert!(
        rig.header().contains(" nudge=idle nudged=0"),
        "{}",
        rig.header()
    );
}

/// Mode `log` runs the machine and holds nothing.
#[test]
fn mode_log_holds_no_pty_and_the_epoch_stays_at_risk() {
    let mut rig = Rig::nudge_rig(Mode::Log, true, true, Answer::Repaint);
    let held = rig.until(Duration::from_secs(6), |r| !r.sizes.is_empty());
    assert!(!held, "held short in log mode: {:?}", rig.sizes);
    assert_eq!(rig.debt(), (-1, 0), "nothing repaired it");
    assert_eq!(rig.pty(), Some((ROWS, COLS)));
    assert!(
        rig.header().contains("verdict=desync-risk"),
        "{}",
        rig.header()
    );
}

/// NEGATIVE CONTROL: not Claude Code, never held, whatever the mode.
#[test]
fn a_program_that_is_not_claude_code_is_never_held() {
    let mut rig = Rig::nudge_rig(Mode::On, true, false, Answer::Repaint);
    let held = rig.until(Duration::from_secs(6), |r| !r.sizes.is_empty());
    assert!(!held, "held short: {:?}", rig.sizes);
    assert!(
        rig.header().contains("verdict=unjudged"),
        "{}",
        rig.header()
    );
}

/// A REAL RESIZE THAT SUPERSEDED THE HOLD AND DID NOT LAST. While it stands the
/// program has a size of its own, so the attempt ends superseded and nothing is
/// said; once the window is back at the size the hold found, after the
/// attempt's wait, the epoch is at risk again with its one try spent: the
/// give-up the same attempt is when the resize comes back inside its wait, one
/// digest line. It used to end there in silence, the tab still displaced.
#[test]
fn a_superseded_repair_whose_resize_comes_back_is_a_give_up() {
    let mut rig = Rig::nudge_rig(Mode::On, true, true, Answer::Silent);
    let held = rig.until(Duration::from_secs(60), |r| {
        r.header().contains(" nudge=short ")
    });
    assert!(held, "never held:\n{}", rig.header());
    rig.resize(ROWS - 2);
    let ended = rig.until(Duration::from_secs(60), |r| {
        r.header().contains(" nudge=idle nudged=1")
    });
    assert!(ended, "never ended:\n{}", rig.header());
    assert!(!rig.render_line(), "the resize stands: nothing to say yet");
    rig.resize(ROWS);
    let told = rig.until(Duration::from_secs(60), |r| r.render_line());
    assert!(
        told,
        "back where the hold found it, still owing, and nothing said:\n{}",
        rig.header()
    );
    assert_ne!(rig.debt(), (0, 0), "still displaced");
}

/// NEGATIVE CONTROL: a program that never answers is let go at the engine's
/// size, once, and the epoch stays at risk (the owner's one action is the
/// digest's).
#[test]
fn a_program_that_never_answers_is_let_go_at_the_engines_size() {
    let mut rig = Rig::nudge_rig(Mode::On, true, true, Answer::Silent);
    let done = rig.until(Duration::from_secs(60), |r| {
        r.header().contains(" nudge=idle nudged=1")
    });
    assert!(done, "never ended:\n{}", rig.header());
    assert_eq!(
        rig.sizes,
        [(ROWS - 1, COLS), (ROWS, COLS)],
        "it read each size and drew nothing"
    );
    assert_eq!(rig.pty(), Some((ROWS, COLS)));
    assert_eq!(rig.debt(), (-1, 0), "no answer, no change");
    assert!(
        rig.header().contains("verdict=desync-risk"),
        "{}",
        rig.header()
    );
}
