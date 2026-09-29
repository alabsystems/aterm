// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 bind for `DriverGeometry` (`aterm_spec::derive::driver_geometry_model`)
//! — the live upgrade's notice, refused `skipped reason=changed` on 121
//! visits of 121 on 2026-09-27.
//!
//! WHAT WAS MEASURED. A private windowed aterm, a program in its tab that
//! enters the alternate screen, turns on focus reports (`?1004h`), bracketed
//! paste and any-motion mouse, logs every byte it reads and every `SIGWINCH`,
//! and repaints its whole screen on either. `lease acquire` → one `SIGWINCH`
//! (rows 32 → 31: the presence band's row was born), `lease release` →
//! another (31 → 32: it folded); no byte at all, focus or other. A `turn
//! if-gen=<fresh> yield=0.2` → `skipped reason=changed`, every time; the same
//! turn with no `yield=` typed, and its `SIGWINCH` landed between the paste
//! and the Enter. A headless instance moved nothing: it never re-grids for a
//! chrome row.
//!
//! THE RIG. Everything shipping is REAL: the `turn` and `lease` verbs
//! (`control_session`), the App's presence projection (`on_presence_wake` →
//! `refresh_presence_session` → `sync_presence_rows`), the session's sink
//! into a real pseudo-terminal, the engine. The test plays the three parts a
//! unit test has no instance of, each the way the windowed App does it:
//!
//! * the EVENT LOOP — a change of the session's lease is delivered as
//!   `Wake::LeaseChanged` (`on_presence_wake`), and the `turn yield=`
//!   momentum read, a main-thread round trip, is answered only after it: the
//!   wake the turn posted when it took the lease is ahead of it in the queue;
//! * the WINDOW — a change of the window's chrome rows is the re-grid
//!   `regrid_window_for_chrome_rows` makes from the OS window's size: the
//!   engine and the PTY lose (or gain) the row;
//! * the PROGRAM, on the slave: the probe above, repainting on every size
//!   change, echoing a paste, answering an Enter — its output read back from
//!   the master into the engine, as the reader thread does.
//!
//! Projection onto the model at the end of each schedule: `rows` (the band
//! row the window committed), `refused` (the verb skipped `reason=changed`),
//! `refused_own` (refused with no output of the program's own since the
//! read), `grids_under_hand` (a re-grid landed while a driver could still
//! type: a turn before its Enter, or a live drive lease) — compared with the
//! model's state after the same actions.
//!
//! NEGATIVE CONTROL: the owner's visit replayed with the lease as it was
//! before the fix — no input phase (the rig clears `typing` the moment the
//! turn takes the lease) — ends refused exactly as the owner's session did,
//! and lands where the model's `Buggy = 1` replay does.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;
use aterm_session::sink::SinkWriter;
use aterm_spec::derive::driver_geometry_model;
use aterm_spec::interp;

use crate::input_stall::tests::raw_pty_pair;
use crate::presence::Level;
use crate::{App, Lease, SessionCtx, WindowId};

const ROWS: u16 = 24;
const COLS: u16 = 80;
/// How long the rig waits for something that MUST happen: the pumped main
/// thread answering the turn's momentum read. A hang detector, never a latency
/// budget: the rig pumps without pause, so the answer is milliseconds away
/// alone, and one that never comes still fails.
const MUST_HAPPEN: Duration = Duration::from_secs(60);
const FOCUS_IN: &[u8] = b"\x1b[I";
const FOCUS_OUT: &[u8] = b"\x1b[O";

/// What the program saw, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Seen {
    /// Bytes it read from its terminal.
    In(Vec<u8>),
    /// Its terminal's size changed to this many rows (its `SIGWINCH`).
    Winch(u16),
}

/// A momentum read the turn's `yield=` sent to the "main thread".
type MomentumAsk = mpsc::Sender<Result<(f32, Instant), String>>;

struct Rig {
    app: App,
    wid: WindowId,
    master: i32,
    slave: i32,
    term: Arc<Mutex<Terminal>>,
    ctx: Arc<SessionCtx>,
    subscribers: crate::subscribe::Subscribers,
    /// The lease the event loop last delivered a wake for.
    delivered: Option<Lease>,
    /// The window's chrome rows at the last re-grid.
    chrome: u16,
    base_chrome: u16,
    /// The rows the program's terminal had when it last looked.
    program_rows: u16,
    program_wrote: usize,
    reader_read: usize,
    seen: Vec<Seen>,
    /// The program has read an Enter.
    entered: bool,
    /// A re-grid landed while a driver could still type.
    grids_under_hand: bool,
    /// Replay the lease as it was before the fix: no input phase.
    incident: bool,
    asks: mpsc::Receiver<MomentumAsk>,
    ask_tx: mpsc::Sender<MomentumAsk>,
    pending: Option<MomentumAsk>,
}

fn read_some(fd: i32) -> Option<Vec<u8>> {
    let mut buf = [0u8; 4096];
    // SAFETY: `fd` is an open descriptor and `buf` a writable buffer of its length.
    let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
    (n > 0).then(|| buf[..usize::try_from(n).unwrap_or(0)].to_vec())
}

fn rows_of(fd: i32) -> u16 {
    // SAFETY: `winsize` is plain integers; zeroed is valid and the ioctl fills it.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is an open terminal descriptor and `ws` a valid out-param.
    let rc = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) };
    assert_eq!(rc, 0, "TIOCGWINSZ");
    ws.ws_row
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

impl Rig {
    fn driver_geometry_rig(incident: bool) -> Rig {
        let (master, slave) = raw_pty_pair();
        aterm_pty::set_nonblocking(slave, true).expect("nonblocking slave");
        aterm_pty::resize(master, ROWS, COLS);
        let sink = Arc::new(SinkWriter::new(master));
        sink.note_master_nonblocking(true);
        let mut app = App::headless_for_test_with_sink(sink);
        let wid = WindowId(0);
        app.windows.get_mut(&wid).expect("the window").focused = true;
        app.frontmost_window = Some(wid);
        let session = app.pool.get(0).expect("session 0");
        let (term, ctx) = (session.term.clone(), session.ctx.clone());
        term.lock().unwrap().resize(ROWS, COLS);
        let chrome = app.chrome_rows(wid);
        let (ask_tx, asks) = mpsc::channel();
        let mut rig = Rig {
            app,
            wid,
            master,
            slave,
            term,
            ctx,
            subscribers: crate::subscribe::new_registry(),
            delivered: None,
            chrome,
            base_chrome: chrome,
            program_rows: ROWS,
            program_wrote: 0,
            reader_read: 0,
            seen: Vec::new(),
            entered: false,
            grids_under_hand: false,
            incident,
            asks,
            ask_tx,
            pending: None,
        };
        // The program starts: the alternate screen, focus reports, bracketed
        // paste, any-motion SGR mouse — Claude Code's modes on the owner's tab
        // — and its first full paint.
        rig.program_write(b"\x1b[?1049h\x1b[?1004h\x1b[?2004h\x1b[?1003h\x1b[?1006h");
        rig.program_write(&Rig::paint(ROWS, "start"));
        rig.settle();
        let modes = *rig.term.lock().unwrap().modes();
        assert!(modes.alternate_screen && modes.focus_reporting && modes.bracketed_paste);
        rig
    }

    fn paint(rows: u16, why: &str) -> Vec<u8> {
        let mut out = b"\x1b[H\x1b[2J".to_vec();
        for r in 0..rows.saturating_sub(1) {
            out.extend_from_slice(
                format!("row {r} painted on {why} at {rows} rows\r\n").as_bytes(),
            );
        }
        out.extend_from_slice(b"> ");
        out
    }

    fn program_write(&mut self, bytes: &[u8]) {
        let mut at = 0;
        while at < bytes.len() {
            // SAFETY: `slave` is open and the slice is valid for its length.
            let n =
                unsafe { libc::write(self.slave, bytes[at..].as_ptr().cast(), bytes.len() - at) };
            if n > 0 {
                at += usize::try_from(n).unwrap_or(0);
            } else {
                self.read_output();
            }
        }
        self.program_wrote += bytes.len();
    }

    /// The reader thread's half: the program's output into the engine.
    fn read_output(&mut self) {
        let mut any = false;
        while let Some(bytes) = read_some(self.master) {
            self.term.lock().unwrap().process(&bytes);
            self.reader_read += bytes.len();
            any = true;
        }
        if any {
            self.subscribers.lock().unwrap().notify(0);
        }
    }

    fn driver_may_type(&self) -> bool {
        let now = crate::metrics::now_us();
        self.ctx
            .turn_lease
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|lease| match lease {
                // Judged from the turn's own side, so the incident replay
                // (which cleared `typing`) is judged the same way: a turn is
                // typing until its Enter was read.
                Lease::Turn { .. } => !self.entered,
                drive => drive.driver_may_type(now),
            })
    }

    /// One turn of the "main thread": the lease's wake, the window's re-grid,
    /// the program, the reader — then any momentum read queued behind them.
    fn pump(&mut self) {
        // The event loop: `Wake::LeaseChanged`.
        let mut lease = self.ctx.turn_lease.lock().unwrap().clone();
        if lease != self.delivered {
            if self.incident
                && let Some(Lease::Turn { typing, .. }) = lease.as_mut()
                && *typing
            {
                // The lease as it was before the fix: no input phase.
                *typing = false;
                *self.ctx.turn_lease.lock().unwrap() = lease.clone();
            }
            self.delivered = lease;
            let sid = self.ctx.self_id.clone();
            self.app.on_presence_wake(&sid, false);
        }
        // The window: a chrome-row change is one re-grid from its OS size.
        let chrome = self.app.chrome_rows(self.wid);
        if chrome != self.chrome {
            self.grids_under_hand |= self.driver_may_type();
            self.chrome = chrome;
            let rows = ROWS - (chrome - self.base_chrome);
            self.term.lock().unwrap().resize(rows, COLS);
            aterm_pty::resize(self.master, rows, COLS);
        }
        // The program.
        while let Some(bytes) = read_some(self.slave) {
            self.seen.push(Seen::In(bytes.clone()));
            if let (Some(a), Some(b)) = (
                bytes.windows(6).position(|w| w == b"\x1b[200~"),
                bytes.windows(6).position(|w| w == b"\x1b[201~"),
            ) {
                let text = bytes[a + 6..b].to_vec();
                self.program_write(&text);
            }
            if bytes.contains(&b'\r') {
                self.entered = true;
                self.program_write(b"\r\n* answered\r\n> ");
            }
        }
        let rows = rows_of(self.slave);
        if rows != self.program_rows {
            self.program_rows = rows;
            self.seen.push(Seen::Winch(rows));
            let paint = Rig::paint(rows, "winch");
            self.program_write(&paint);
        }
        self.read_output();
        // The momentum read, behind everything the wakes above caused.
        if self.pending.is_none() {
            self.pending = self.asks.try_recv().ok();
        }
        if self.reader_read == self.program_wrote
            && let Some(ask) = self.pending.take()
        {
            let _ = ask.send(Ok((0.0, Instant::now())));
        }
    }

    fn settle(&mut self) {
        let until = Instant::now() + Duration::from_millis(60);
        while Instant::now() < until {
            self.pump();
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// A real `turn` on its own (control) thread, the rig pumping the main
    /// thread until it answers.
    fn turn(&mut self, args: &str) -> String {
        let term = self.term.clone();
        let ctx = self.ctx.clone();
        let store = self.app.store.clone();
        let subscribers = self.subscribers.clone();
        let ask_tx = self.ask_tx.clone();
        let args = args.to_string();
        let runner = std::thread::spawn(move || {
            let sink = ctx.sink.clone();
            let paste = |text: &str| {
                let framed = [b"\x1b[200~", text.as_bytes(), b"\x1b[201~"].concat();
                sink.write_frame_nonparking(&framed).is_ok()
            };
            let press = |name: &str| name == "enter" && sink.write_frame_nonparking(b"\r").is_ok();
            let momentum = || {
                let (tx, rx) = mpsc::channel();
                ask_tx.send(tx).map_err(|e| e.to_string())?;
                rx.recv_timeout(MUST_HAPPEN).map_err(|e| e.to_string())?
            };
            crate::control::cmd_turn(
                &term,
                &store,
                0,
                &args,
                &subscribers,
                &ctx,
                &crate::control::TurnIo {
                    paste: &paste,
                    press: &press,
                    momentum: &momentum,
                    ..crate::control::TurnIo::paste_only()
                },
            )
        });
        // Past the momentum read's own minute, so a read that never came is
        // reported by the turn rather than cut short here.
        let deadline = Instant::now() + 2 * MUST_HAPPEN;
        while !runner.is_finished() {
            assert!(Instant::now() < deadline, "the turn never answered");
            self.pump();
            std::thread::sleep(Duration::from_millis(1));
        }
        let reply = runner.join().expect("the turn thread");
        // The lease the turn let go reaches the window too.
        self.settle();
        reply
    }

    fn generation(&self) -> String {
        crate::control::screen_gen(&self.term.lock().unwrap()).to_string()
    }

    /// A driver's READ, through the verb's own function (`text --json`): the
    /// generation it hands out, noted as a look at the session's screen.
    fn look(&self) -> String {
        let args = crate::control::text_args("").expect("bare text args");
        let reply = crate::control::cmd_text_json_looked(&self.term, &self.ctx, args);
        let at = reply.find("\"gen\":\"").expect("gen") + "\"gen\":\"".len();
        let len = reply[at..].find('"').expect("gen's close");
        reply[at..at + len].to_string()
    }

    fn rows(&self) -> u16 {
        self.app.presence_view(self.wid).expect("the window").rows
    }

    fn winches(&self) -> Vec<u16> {
        self.seen
            .iter()
            .filter_map(|s| match s {
                Seen::Winch(rows) => Some(*rows),
                Seen::In(_) => None,
            })
            .collect()
    }

    fn input(&self) -> Vec<u8> {
        self.seen
            .iter()
            .filter_map(|s| match s {
                Seen::In(bytes) => Some(bytes.clone()),
                Seen::Winch(_) => None,
            })
            .flatten()
            .collect()
    }

    fn no_focus_byte(&self) {
        let input = self.input();
        assert!(
            !contains(&input, FOCUS_IN) && !contains(&input, FOCUS_OUT),
            "a focus report reached the program: {:?}",
            String::from_utf8_lossy(&input)
        );
    }

    /// This schedule's end, projected onto the model's variables.
    fn project(&self, reply: &str) -> [(&'static str, i64); 4] {
        let refused = reply.contains("skipped reason=changed");
        [
            ("rows", i64::from(self.rows())),
            ("refused", i64::from(refused)),
            ("refused_own", i64::from(refused && self.grids_under_hand)),
            ("grids_under_hand", i64::from(self.grids_under_hand)),
        ]
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        aterm_pty::close_fd(self.master);
        aterm_pty::close_fd(self.slave);
    }
}

/// The model's state after `schedule`, at `buggy`, on the projected variables.
fn model_after(buggy: i64, schedule: &[&str]) -> [(&'static str, i64); 4] {
    let model = interp::with_buggy(&driver_geometry_model(), buggy);
    let mut state = model.init_state();
    for action in schedule {
        assert!(model.fire(action, &mut state), "model {action}: {state:?}");
    }
    ["rows", "refused", "refused_own", "grids_under_hand"].map(|v| (v, state[v]))
}

/// THE OWNER'S VISIT: the caller reads the idle program's screen, judges it,
/// and types a turn fenced on that read with the live upgrade's own
/// arguments. It types — the paste and the Enter reach the program before any
/// size change — and the band row the lease asked for is born after them,
/// inside the turn. No focus byte reaches the program at any point.
#[test]
fn a_fresh_fenced_turn_types_into_a_repainting_alt_screen_program() {
    crate::fabric::with_link_reset(|| {
        let mut rig = Rig::driver_geometry_rig(false);
        assert_eq!(rig.rows(), 0, "a quiet window has no band row");
        let judged = rig.generation();
        let reply = rig.turn(&format!(
            "if-gen={judged} yield=0.2 idle=150 timeout=5000 hello peer"
        ));
        let head = reply.lines().next().unwrap_or_default();
        assert!(
            head.contains("submitted=1") && !head.contains("skipped"),
            "the fresh fence did not type: {head} (program saw {:?})",
            rig.seen
        );
        let enter = rig
            .seen
            .iter()
            .position(|s| matches!(s, Seen::In(b) if b.contains(&b'\r')))
            .expect("the Enter reached the program");
        let first_winch = rig.seen.iter().position(|s| matches!(s, Seen::Winch(_)));
        assert!(
            first_winch.is_none_or(|w| w > enter),
            "the program was resized before the turn's Enter: {:?}",
            rig.seen
        );
        assert!(contains(&rig.input(), b"hello peer"));
        rig.no_focus_byte();
        assert_eq!(
            rig.rows(),
            1,
            "the row the lease asked for is born after the input"
        );
        assert_eq!(rig.winches(), [ROWS - 1], "one re-grid, after the Enter");
        // …INSIDE the turn: its settle waited out the program's repaint, so
        // the screen it answers with is the one the next read will see.
        assert!(
            reply.contains(&format!("painted on winch at {} rows", ROWS - 1)),
            "the turn answered before its own re-grid's repaint: {reply}"
        );
        // The settled turn is the story the row now stands for (`Note`).
        assert_eq!(
            rig.project(&reply),
            model_after(
                0,
                &[
                    "Read", "Take", "Fence", "EndInput", "Regrid", "Note", "Release"
                ]
            ),
        );
    });
}

/// THE FENCE KEEPS ITS GUARANTEE: the program's OWN output between the
/// caller's read and the turn (a box drawn, a tick) refuses it, and nothing
/// reaches the program — no text, no Enter, no size change.
#[test]
fn a_fenced_turn_over_the_programs_own_output_types_nothing() {
    crate::fabric::with_link_reset(|| {
        let mut rig = Rig::driver_geometry_rig(false);
        let judged = rig.generation();
        rig.program_write(b"\x1b[3;1H\x1b[2Ka box the program drew");
        rig.settle();
        assert_ne!(rig.generation(), judged, "the program moved its screen");
        let reply = rig.turn(&format!(
            "if-gen={judged} yield=0.2 idle=150 timeout=5000 hello peer"
        ));
        assert!(
            reply.starts_with("OK 0 turn skipped reason=changed submitted=0"),
            "{reply}"
        );
        assert!(
            rig.input().is_empty(),
            "typed over a moved screen: {:?}",
            rig.seen
        );
        assert!(rig.winches().is_empty(), "{:?}", rig.seen);
        assert_eq!(rig.rows(), 0, "a refused turn leaves the geometry alone");
        assert_eq!(
            rig.project(&reply),
            model_after(0, &["Read", "Output", "Take", "Fence", "Drop"]),
        );
    });
}

/// A `lease acquire` and its `release`: the rim says a hand is on the
/// session, and the program is sent nothing — no byte, no focus report, no
/// size change (the owner measured one `SIGWINCH` each way).
#[test]
fn a_drive_lease_pokes_the_program_with_nothing() {
    crate::fabric::with_link_reset(|| {
        let mut rig = Rig::driver_geometry_rig(false);
        let acquired = crate::control::cmd_lease(&rig.ctx, "acquire holder=rig ttl=5000");
        assert!(
            acquired.starts_with("OK lease acquired holder=rig"),
            "{acquired}"
        );
        rig.settle();
        assert_eq!(rig.app.presence_level(rig.wid), Level::Driven);
        assert_eq!(
            rig.app.presence_report(rig.wid).expect("the window").1,
            "drive"
        );
        assert_eq!(rig.rows(), 0, "no band row is born under a drive lease");
        let released = crate::control::cmd_lease(&rig.ctx, "release holder=rig");
        assert_eq!(released, "OK lease released\n");
        rig.settle();
        assert!(
            rig.seen.is_empty(),
            "the lease reached the program: {:?}",
            rig.seen
        );
        rig.no_focus_byte();
        assert_eq!(rig.project("OK"), model_after(0, &["Take", "Drop"]),);
    });
}

/// NEGATIVE CONTROL — the owner's session: the same visit with the lease as
/// it was before the fix (no input phase). The band row is born on the
/// lease, the program repaints on its `SIGWINCH`, and the fence the caller
/// judged a moment earlier is refused `reason=changed` with nothing typed —
/// the 121 refused visits — exactly where the model's `Buggy = 1` lands.
#[test]
fn the_incident_lease_refuses_its_own_fresh_fence() {
    crate::fabric::with_link_reset(|| {
        let mut rig = Rig::driver_geometry_rig(true);
        let judged = rig.generation();
        let reply = rig.turn(&format!(
            "if-gen={judged} yield=0.2 idle=150 timeout=5000 hello peer"
        ));
        assert!(
            reply.starts_with("OK 0 turn skipped reason=changed submitted=0"),
            "the incident replay typed: {reply}"
        );
        assert!(rig.input().is_empty(), "{:?}", rig.seen);
        // The lease's end folds the row only after the fold quiet (ruling 394,
        // proposed): the incident's net-zero pair inside a millisecond is no
        // longer paid. The presence timer the held fold arms commits it.
        assert_eq!(
            rig.winches(),
            [ROWS - 1],
            "the lease re-gridded the program; its fold waits out the quiet: {:?}",
            rig.seen
        );
        let now = Instant::now();
        assert!(
            rig.app.presence_deadline(now).is_some(),
            "the held fold arms the presence timer"
        );
        let _ = rig
            .app
            .presence_tick(now + aterm_messages::presence::FOLD_QUIET);
        rig.settle();
        assert_eq!(
            rig.winches(),
            [ROWS - 1, ROWS],
            "the lease re-gridded the program, and its end re-gridded it back: {:?}",
            rig.seen
        );
        rig.no_focus_byte();
        assert_eq!(
            rig.project(&reply),
            model_after(
                1,
                &[
                    "Read",
                    "Take",
                    "RegridUnderTheHand",
                    "Fence",
                    "Drop",
                    "Regrid"
                ]
            ),
        );
    });
}

/// THE LEASE RACE (measured 2026-09-28 on a live window: `lease acquire` then
/// `lease release` under 0.1 ms apart raised the band row and folded it
/// again, 24 → 23 → 24 inside 0.35 ms, in 3 runs of 10). The acquire's wake
/// senses the lease — the words say "driven", the row is held — and the
/// release lands before the next commit of the row: here a refresh of the
/// window (the presence timer's tick, a focus change) that reaches the App
/// ahead of the release's own wake, reading the words the acquire formed and
/// the LIVE lease that is already gone. The same interleaving as a release
/// landing inside the acquire's own refresh, between `presence_facts` and the
/// row's commit. The row must not be born for a hand already gone: the
/// window never re-grids, and the program is sent nothing — the model's
/// `Take, Sense, Drop` with `Commit` disabled, and the window's refresh a
/// `Sense` of the moved lease (`drive::refresh_window` re-senses a front
/// session whose lease moved; `drive::commit_rows` holds a row whose words
/// were formed under another lease). With neither, this test's window
/// re-grids twice.
#[test]
fn a_lease_let_go_before_its_wake_raises_no_row() {
    crate::fabric::with_link_reset(|| {
        let mut rig = Rig::driver_geometry_rig(false);
        let regrids = crate::app_presence::presence_regrids();
        let acquired = crate::control::cmd_lease(&rig.ctx, "acquire holder=rig ttl=5000");
        assert!(
            acquired.starts_with("OK lease acquired holder=rig"),
            "{acquired}"
        );
        rig.settle();
        assert_eq!(rig.app.presence_level(rig.wid), Level::Driven);
        assert_eq!(rig.rows(), 0, "no band row is born under a drive lease");
        let released = crate::control::cmd_lease(&rig.ctx, "release holder=rig");
        assert_eq!(released, "OK lease released\n");
        // A refresh ahead of the release's wake: the words still say driven.
        rig.app.refresh_presence_window(rig.wid);
        assert_eq!(
            (
                rig.rows(),
                crate::app_presence::presence_regrids() - regrids
            ),
            (0, 0),
            "a row was born for a hand already gone"
        );
        rig.pump();
        // The release's own wake re-derives the want: nothing to fold.
        rig.settle();
        assert_eq!(rig.app.presence_level(rig.wid), Level::Quiet);
        assert_eq!(crate::app_presence::presence_regrids() - regrids, 0);
        assert!(
            rig.seen.is_empty(),
            "the lease reached the program: {:?}",
            rig.seen
        );
        rig.no_focus_byte();
        let model = interp::with_buggy(&driver_geometry_model(), 0);
        let mut state = model.init_state();
        for action in ["Take", "Sense", "Drop"] {
            assert!(model.fire(action, &mut state), "model {action}: {state:?}");
        }
        assert!(!model.fire("Commit", &mut state.clone()), "{state:?}");
        assert_eq!(
            rig.project("OK"),
            ["rows", "refused", "refused_own", "grids_under_hand"].map(|v| (v, state[v]))
        );
    });
}

/// A PENDING FOLD AND A DRIVER'S READ (review of ruling 394, C1). The fold
/// quiet defers a fold; deferred, its re-grid could land between a driver's
/// read and its fenced act — the act refused `reason=changed`, where the
/// immediate fold had landed ahead of the read. A settling turn's lease bore
/// the row and left (the fold pends); the driver reads through `text --json`
/// and the presence timer fires a quiet after the want's drop: the READ holds
/// the fold, and the turn fenced on it types. NEGATIVE CONTROL: the same
/// schedule with the generation read off the engine (no look noted) — the
/// timer folds the row, the program repaints on its `SIGWINCH`, and the fence
/// is refused.
#[test]
fn a_pending_fold_never_lands_between_a_drivers_read_and_its_fenced_turn() {
    use aterm_messages::presence::FOLD_QUIET;
    for looked in [true, false] {
        crate::fabric::with_link_reset(|| {
            let mut rig = Rig::driver_geometry_rig(false);
            *rig.ctx.turn_lease.lock().unwrap() = Some(Lease::Turn {
                id: 90,
                driver: None,
                typing: false,
            });
            rig.settle();
            assert_eq!(rig.rows(), 1, "the settling lease bore the row");
            *rig.ctx.turn_lease.lock().unwrap() = None;
            rig.settle();
            assert_eq!(rig.rows(), 1, "the fold waits out its quiet");
            // The fold pends from before `read_at`; the look lands after it.
            let read_at = Instant::now();
            std::thread::sleep(Duration::from_millis(5));
            let judged = if looked { rig.look() } else { rig.generation() };
            let _ = rig.app.presence_tick(read_at + FOLD_QUIET);
            rig.settle();
            let reply = rig.turn(&format!(
                "if-gen={judged} yield=0.2 idle=150 timeout=5000 hello peer"
            ));
            if looked {
                assert_eq!(rig.rows(), 1, "the read held the fold: {:?}", rig.seen);
                assert!(
                    reply
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .contains("submitted=1"),
                    "the read's fence did not type: {reply} (program saw {:?})",
                    rig.seen
                );
                assert_eq!(rig.winches(), [ROWS - 1], "the birth alone");
            } else {
                assert!(
                    reply.starts_with("OK 0 turn skipped reason=changed submitted=0"),
                    "the unlooked fold did not land under the read: {reply}"
                );
                assert_eq!(rig.winches(), [ROWS - 1, ROWS], "{:?}", rig.seen);
            }
            rig.no_focus_byte();
        });
    }
}
