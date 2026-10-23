// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 bind for `ForegroundHandback` (`aterm_models_gui::foreground_handback_model`) — the 2026-09-25 "crashed" tab, where a Claude Code killed while it held the terminal left the alt screen, kitty flags, modifyOtherKeys, mouse tracking, focus reports, 2026 and a hidden cursor armed under the zsh that reclaimed it.

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use aterm_core::terminal::{Terminal, program_evidence};
use aterm_models_gui::{foreground_handback_model, foreground_handback_ownership_model};
use aterm_spec::derive::Model;
use aterm_spec::interp;

use super::{attach_reader_inner, park_reader};
use crate::foreground_handback::{FgGone, FgRole};
use crate::{App, Session, Wake, WindowId};

type State = BTreeMap<&'static str, i64>;

/// Fake process-group ids: the shell and the job (and, for the `gdb -tui`
/// shape, the job's own child).
const SHELL: i32 = 4100;
const JOB: i32 = 4200;
const CHILD: i32 = 4300;

/// The model variables the real pipeline projects onto.
const PROJECTED: [&str; 6] = ["fg", "life", "leak", "paste", "prompt", "tail"];

/// A hang detector, never a latency budget: a loaded machine only slows the
/// reader down.
const PATIENCE: Duration = aterm_spec::wait::MUST_HAPPEN;

/// What zle writes before it hands a command the terminal.
const ZLE_PROMPT: &[u8] = b"\x1b[?2004h% ";
const ZLE_EXEC: &[u8] = b"\x1b[?2004l\r\n";
/// What a killed Claude Code had armed.
const INCIDENT: &[u8] = b"\x1b[?1049h\x1b[>5u\x1b[?1003h\x1b[?1006h\x1b[>4;2m\x1b[?2004h\
\x1b[?1004h\x1b[?25l\x1b[?2026hFRAME";
/// What zsh writes after it reaped the job and took the terminal back.
const RECLAIM: &[u8] = b"zsh: killed     claude\r\n% \x1b[?2004h";
/// What zsh writes after Ctrl-Z stopped the job and it took the terminal back.
const SUSPENDED: &[u8] = b"\r\nzsh: suspended  claude\r\n% \x1b[?2004h";
/// What the job draws when `fg` resumes it (a full repaint, as on SIGCONT).
const REPAINT: &[u8] = b"\x1b[?2004h\x1b[2J\x1b[HFRAME2";

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let seen =
        aterm_spec::wait::within(PATIENCE, Duration::from_millis(1), || done().then_some(()));
    assert!(seen.is_some(), "timed out waiting for {what}");
}

/// The model's `leak`: the evidence gate without bracketed paste, which `paste` projects
/// (zle's own `2004h` is evidence too: PG1, docs/AUDIT-side-effects-2026-10-07.md).
fn leak(t: &Terminal) -> bool {
    t.program_evidence() & !program_evidence::BRACKETED_PASTE != 0
}

fn screen_has(t: &Terminal, needle: &str) -> bool {
    (0..usize::from(t.rows())).any(|r| t.row_text(r).is_some_and(|l| l.contains(needle)))
}

/// The scripted liveness probe's backing set: the fake groups "killed" so far.
type Dead = Mutex<Vec<i32>>;

fn is_dead(dead: &Dead, pgid: i32) -> bool {
    dead.lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains(&pgid)
}

/// A PTY pair with both ends close-on-exec from birth (`aterm_pty::open_pty_pair`) and the
/// slave in raw mode, so byte counts are exact.
fn open_raw_pty() -> (i32, i32) {
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    // SAFETY: a zeroed termios is a valid out-slot for tcgetattr, then cfmakeraw/tcsetattr on the test-owned slave fd.
    unsafe {
        let mut tio: libc::termios = std::mem::zeroed();
        assert_eq!(libc::tcgetattr(slave, &mut tio), 0, "tcgetattr");
        libc::cfmakeraw(&mut tio);
        assert_eq!(libc::tcsetattr(slave, libc::TCSANOW, &tio), 0, "tcsetattr");
    }
    (master, slave)
}

/// A real session on a real pseudo-terminal (raw slave, so byte counts are
/// exact), with the real reader attached through a scripted probe.
struct Rig {
    session: Session,
    slave: i32,
    fg: &'static AtomicI32,
    dead: &'static Dead,
    life: i64,
    /// A schedule's own post-reclaim prompt text, read as `prompt` alongside
    /// zsh's `killed`/`suspended` notices (a one-shot's clean exit prints none).
    prompt_marker: Option<&'static str>,
    /// Kept for [`Self::reattach`] (the session factory the reader reads).
    app: App,
}

impl Rig {
    /// Attach with `probe` and `gone`; `pre` is fed to the engine first (an
    /// adopted session's screen); `fg` and `dead` back the two probes.
    fn attach(
        id: u64,
        probe: (fn(i32) -> i32, &'static AtomicI32),
        gone: (FgGone, &'static Dead),
        pre: &[u8],
    ) -> Self {
        Self::attach_with(id, probe, gone, pre, false)
    }

    /// [`Self::attach`] with the temporal spine recording
    /// (`temporal_recording`), seeded from the engine as a spawn seeds it.
    fn attach_recording(
        id: u64,
        probe: (fn(i32) -> i32, &'static AtomicI32),
        gone: (FgGone, &'static Dead),
    ) -> Self {
        Self::attach_with(id, probe, gone, b"", true)
    }

    fn attach_with(
        id: u64,
        (probe, fg): (fn(i32) -> i32, &'static AtomicI32),
        (gone, dead): (FgGone, &'static Dead),
        pre: &[u8],
        record: bool,
    ) -> Self {
        let mut app = App::headless_for_test();
        app.session_factory.fg_probe = probe;
        app.session_factory.fg_gone = gone;
        app.session_factory.temporal_recording = record;
        dead.lock().unwrap_or_else(|p| p.into_inner()).clear();
        let (master, slave) = open_raw_pty();
        let mut session = crate::stub_session(id);
        session.master = master;
        if !pre.is_empty() {
            session.term.lock().expect("terminal lock").process(pre);
        }
        if record {
            assert!(
                super::seed_temporal_keyframe(&session.term, &session.ctx.temporal, id),
                "the recording is seeded"
            );
        }
        let mut rig = Self {
            session,
            slave,
            fg,
            dead,
            life: 0,
            prompt_marker: None,
            app,
        };
        rig.start_reader();
        rig
    }

    /// Attach the real reader and wait for its gather to start.
    fn start_reader(&mut self) {
        let (ready_tx, ready_rx) = mpsc::channel::<()>();
        let post = Arc::new(move |wake: Wake| {
            if matches!(wake, Wake::Ready { .. }) {
                let _ = ready_tx.send(());
            }
            true
        });
        attach_reader_inner(
            &mut self.session,
            WindowId(0),
            post,
            &self.app.session_factory,
            None,
        )
        .expect("attach the real reader");
        ready_rx
            .recv_timeout(PATIENCE)
            .expect("the reader's gather started");
    }

    /// Park the reader exactly as the overlap handoff does (`park_reader`).
    fn park(&mut self) {
        assert!(
            park_reader(&mut self.session, Instant::now() + PATIENCE),
            "the reader parks"
        );
    }

    /// Attach a new reader to the parked session (the handoff's resume).
    fn reattach(&mut self) {
        self.start_reader();
    }

    /// The group's leader is gone (the scripted `ESRCH`).
    fn kill(&self, pgid: i32) {
        self.dead
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(pgid);
    }

    fn term(&self) -> Arc<Mutex<Terminal>> {
        self.session.term.clone()
    }

    /// The program (or shell) writes `bytes`; `done` says when the engine has
    /// parsed them.
    fn write(&self, bytes: &[u8], what: &str, done: impl Fn(&Terminal) -> bool) {
        // SAFETY: a bounded write of a live slice to the test-owned slave.
        let wrote = unsafe { libc::write(self.slave, bytes.as_ptr().cast(), bytes.len()) };
        assert_eq!(wrote, bytes.len() as isize, "write {what}");
        let term = self.term();
        wait_for(what, || done(&term.lock().expect("terminal lock")));
    }

    /// `tcsetpgrp`: move the scripted foreground.
    fn set_fg(&self, pgid: i32) {
        self.fg.store(pgid, Ordering::SeqCst);
    }

    fn project(&self) -> State {
        let term = self.term();
        let t = term.lock().expect("terminal lock");
        [
            ("fg", i64::from(self.fg.load(Ordering::SeqCst) == JOB)),
            ("life", self.life),
            ("leak", i64::from(leak(&t))),
            ("paste", i64::from(t.modes().bracketed_paste)),
            (
                "prompt",
                i64::from(
                    screen_has(&t, "zsh: killed")
                        || screen_has(&t, "zsh: suspended")
                        || self.prompt_marker.is_some_and(|m| screen_has(&t, m)),
                ),
            ),
            ("tail", 0),
        ]
        .into_iter()
        .collect()
    }

    /// Every `modes-restored` payload once at least `n` are recorded. The reader records the event
    /// just after it releases the term lock, so a screen a `write` saw finished can precede its
    /// event.
    fn restored_n(&self, n: usize) -> Vec<String> {
        wait_for("the reader's modes-restored event", || {
            self.restored().len() >= n
        });
        self.restored()
    }

    /// The session log's `modes` marks (phase 3B), written in the same
    /// timeline hold as each `modes-restored` event.
    fn modes_marks(&self) -> Vec<String> {
        let g = self
            .session
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        g.slog()
            .log()
            .window()
            .filter(|r| r.k == aterm_slog::Kind::Modes)
            .filter_map(|r| aterm_slog::encode(r, aterm_slog::Tier::Content).ok())
            .collect()
    }

    /// Every `modes-restored` payload the reader recorded.
    fn restored(&self) -> Vec<String> {
        self.session
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .since(None)
            .filter(|e| e.kind == "modes-restored")
            .map(|e| e.payload.to_string())
            .collect()
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

/// Fire `actions` on `model` from `state`, each one enabled.
fn fire(model: &Model, state: &mut State, actions: &[&str]) {
    for action in actions {
        assert!(model.fire(action, state), "{action} from {state:?}");
    }
}

fn projected(state: &State) -> State {
    PROJECTED.iter().map(|k| (*k, state[k])).collect()
}

/// The real projection must be the model's state after the same prefix.
fn check(rig: &Rig, model_state: &State, at: &str) {
    assert_eq!(rig.project(), projected(model_state), "{at}");
}

/// Exactly one handback, from `from` to `to`.
fn assert_one_handback(rig: &Rig, from: i32, to: i32) {
    let events = rig.restored_n(1);
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(
        events[0].starts_with(&format!("from={from} to={to} ")),
        "{events:?}"
    );
}

/// From the model's initial state: the prompt, zle's exec, and the job takes
/// the terminal and arms the incident's modes — `Launch` and the job's write,
/// parsed and checked.
fn launch_the_job(rig: &mut Rig, model: &Model) -> State {
    let mut s = model.init_state();
    rig.write(ZLE_PROMPT, "the prompt", |t| t.modes().bracketed_paste);
    rig.write(ZLE_EXEC, "zle's 2004l", |t| !t.modes().bracketed_paste);
    rig.set_fg(JOB);
    rig.life = 1;
    fire(model, &mut s, &["Launch"]);
    rig.write(INCIDENT, "the job's modes", |t| {
        t.program_owns_terminal() && screen_has(t, "FRAME")
    });
    fire(
        model,
        &mut s,
        &["JobWrite", "ReadJob", "Park", "Deliver", "Parse"],
    );
    check(rig, &s, "the job's output parsed");
    s
}

/// Ctrl-Z: the job stops alive and zsh takes the terminal and says so (`Stop`, `Reclaim`, the
/// shell's write).
fn stop_the_job(rig: &mut Rig, model: &Model, s: &mut State) {
    rig.life = 4;
    fire(model, s, &["Stop"]);
    rig.set_fg(SHELL);
    rig.life = 5;
    fire(model, s, &["Reclaim"]);
    check(rig, s, "Stop + Reclaim");
    rig.write(SUSPENDED, "zsh: suspended", |t| {
        screen_has(t, "zsh: suspended") && t.modes().bracketed_paste
    });
    fire(
        model,
        s,
        &["ShellWrite", "ReadShell", "Park", "Deliver", "Parse"],
    );
}

/// `fg`: zle turns 2004 off, the job takes the terminal back and repaints —
/// `Resume` and the job's write, checked at `at` — and runs with its modes:
/// every mode it armed (`evidence`, where the caller read it), its alt screen
/// and mouse, and the committed `JobKeepsItsModesWhileAlive`.
fn resume_with_its_modes(
    rig: &mut Rig,
    model: &Model,
    s: &mut State,
    at: &str,
    evidence: Option<u16>,
) {
    rig.write(ZLE_EXEC, "zle's 2004l", |t| !t.modes().bracketed_paste);
    rig.set_fg(JOB);
    rig.life = 1;
    fire(model, s, &["Resume"]);
    rig.write(REPAINT, "the job's repaint", |t| screen_has(t, "FRAME2"));
    fire(
        model,
        s,
        &["JobWrite", "ReadJob", "Park", "Deliver", "Parse"],
    );
    check(rig, s, at);
    {
        let term = rig.term();
        let t = term.lock().expect("terminal lock");
        if let Some(evidence) = evidence {
            assert_eq!(t.program_evidence(), evidence, "every mode it armed");
        }
        assert!(t.is_alternate_screen());
        assert!(
            t.encode_mouse_motion(0, 10, 5, 0).is_some(),
            "its mouse still reports"
        );
    }
    assert!(model.check_invariant("JobKeepsItsModesWhileAlive", s));
}

/// The job dies holding the terminal and zsh takes it back and writes —
/// `Die`, `Reclaim` and the shell's write, checked — and the job's modes are
/// handed back once, from the job to the shell.
fn die_and_hand_back(rig: &mut Rig, model: &Model, s: &mut State) {
    rig.life = 2;
    fire(model, s, &["Die"]);
    rig.set_fg(SHELL);
    rig.life = 3;
    fire(model, s, &["Reclaim"]);
    rig.write(RECLAIM, "zsh's reclaim output", |t| {
        screen_has(t, "zsh: killed") && t.modes().bracketed_paste
    });
    fire(
        model,
        s,
        &["ShellWrite", "ReadShell", "Park", "Deliver", "Parse"],
    );
    check(rig, s, "the death edge parsed");
    assert_eq!((s["prompt"], s["leak"], s["paste"]), (1, 0, 1));
    assert_one_handback(rig, JOB, SHELL);
}

/// The incident, step by step, on the real reader. Returns the rig at its
/// end state and the model's state after the whole schedule.
fn run_incident(rig: &mut Rig, model: &Model) -> State {
    let mut s = model.init_state();
    rig.write(ZLE_PROMPT, "the prompt", |t| t.modes().bracketed_paste);
    check(rig, &s, "Init: the prompt with zle's 2004h");

    rig.write(ZLE_EXEC, "zle's 2004l", |t| !t.modes().bracketed_paste);
    rig.set_fg(JOB);
    rig.life = 1;
    fire(model, &mut s, &["Launch"]);
    check(rig, &s, "Launch");

    rig.write(INCIDENT, "the job's modes", |t| {
        t.program_owns_terminal() && screen_has(t, "FRAME")
    });
    fire(
        model,
        &mut s,
        &["JobWrite", "ReadJob", "Park", "Deliver", "Parse"],
    );
    check(rig, &s, "the job's output parsed");

    rig.kill(JOB);
    rig.life = 2;
    fire(model, &mut s, &["Die"]);
    check(rig, &s, "Die");
    rig.set_fg(SHELL);
    rig.life = 3;
    fire(model, &mut s, &["Reclaim"]);
    check(rig, &s, "Reclaim");

    rig.write(RECLAIM, "zsh's reclaim output", |t| {
        screen_has(t, "zsh: killed") && t.modes().bracketed_paste
    });
    fire(
        model,
        &mut s,
        &["ShellWrite", "ReadShell", "Park", "Deliver", "Parse"],
    );
    check(rig, &s, "the shell's output parsed");
    s
}

static FG_INCIDENT: AtomicI32 = AtomicI32::new(SHELL);
fn probe_incident(_master: i32) -> i32 {
    FG_INCIDENT.load(Ordering::SeqCst)
}
static DEAD_INCIDENT: Dead = Mutex::new(Vec::new());
fn gone_incident(_master: i32, pgid: i32, _role: FgRole) -> bool {
    is_dead(&DEAD_INCIDENT, pgid)
}

#[test]
fn foreground_handback_the_incident_is_handed_back_at_the_cut() {
    let model = foreground_handback_model();
    FG_INCIDENT.store(SHELL, Ordering::SeqCst);
    let mut rig = Rig::attach(
        71,
        (probe_incident, &FG_INCIDENT),
        (gone_incident, &DEAD_INCIDENT),
        b"",
    );
    let end = run_incident(&mut rig, &model);
    assert_eq!(
        projected(&end),
        [
            ("fg", 0),
            ("leak", 0),
            ("life", 3),
            ("paste", 1),
            ("prompt", 1),
            ("tail", 0)
        ]
        .into_iter()
        .collect::<State>()
    );
    assert!(model.check_invariant("PromptNotHijacked", &end));
    assert!(model.check_invariant("ShellKeepsItsOwnModes", &end));

    let term = rig.term();
    let t = term.lock().expect("terminal lock");
    assert!(!t.is_alternate_screen(), "back on the main screen");
    assert!(t.modes().cursor_visible);
    assert!(
        t.encode_mouse_motion(0, 10, 5, 0).is_none(),
        "no motion reports"
    );
    drop(t);

    let events = rig.restored_n(1);
    assert_eq!(events.len(), 1, "exactly one handback: {events:?}");
    let e = &events[0];
    assert!(e.starts_with(&format!("from={JOB} to={SHELL} ")), "{e}");
    let reverted = e
        .split_whitespace()
        .find_map(|f| f.strip_prefix("reverted="))
        .expect("reverted=");
    let reverted: Vec<&str> = reverted.split(',').collect();
    for want in ["alt", "mouse", "mok", "focus", "cursor", "sync"] {
        assert!(reverted.contains(&want), "{want} in {reverted:?}");
    }
    assert!(
        reverted.iter().any(|r| r.starts_with("kitty")),
        "{reverted:?}"
    );
    let marks = rig.modes_marks();
    assert_eq!(marks.len(), 1, "{marks:?}");
    assert!(
        marks[0].contains(&format!(" mark - modes n={} ", reverted.len()))
            && marks[0].contains(" src=engine conf=exact "),
        "{marks:?}"
    );
}

static FG_ADOPTED: AtomicI32 = AtomicI32::new(JOB);
fn probe_adopted(_master: i32) -> i32 {
    FG_ADOPTED.load(Ordering::SeqCst)
}
static DEAD_ADOPTED: Dead = Mutex::new(Vec::new());
fn gone_adopted(_master: i32, pgid: i32, _role: FgRole) -> bool {
    is_dead(&DEAD_ADOPTED, pgid)
}

/// An adopted session: the reader attached while the job held the terminal with its modes
/// in force.
#[test]
fn foreground_handback_an_adopted_session_is_handed_back_when_its_job_dies() {
    let model = foreground_handback_model();
    FG_ADOPTED.store(JOB, Ordering::SeqCst);
    let mut rig = Rig::attach(
        72,
        (probe_adopted, &FG_ADOPTED),
        (gone_adopted, &DEAD_ADOPTED),
        INCIDENT,
    );
    let mut s = model.init_state();
    rig.life = 1;
    fire(&model, &mut s, &["Adopt"]);
    check(&rig, &s, "Adopt");

    rig.kill(JOB);
    rig.life = 2;
    fire(&model, &mut s, &["Die"]);
    rig.set_fg(SHELL);
    rig.life = 3;
    fire(&model, &mut s, &["Reclaim"]);
    check(&rig, &s, "Reclaim");

    rig.write(RECLAIM, "zsh's reclaim output", |t| {
        screen_has(t, "zsh: killed") && t.modes().bracketed_paste
    });
    fire(
        &model,
        &mut s,
        &["ShellWrite", "ReadShell", "Park", "Deliver", "Parse"],
    );
    check(&rig, &s, "the shell's output parsed");
    assert_eq!((s["prompt"], s["leak"], s["paste"]), (1, 0, 1));
    assert_eq!(rig.restored_n(1).len(), 1, "{:?}", rig.restored());
}

static FG_MIXED: AtomicI32 = AtomicI32::new(JOB);
static DEAD_MIXED: Dead = Mutex::new(Vec::new());
fn gone_mixed(_master: i32, pgid: i32, _role: FgRole) -> bool {
    is_dead(&DEAD_MIXED, pgid)
}
static SLAVE_MIXED: AtomicI32 = AtomicI32::new(-1);
static ARMED_MIXED: AtomicBool = AtomicBool::new(false);
/// The first sample after arming stands in for K2 inside one gather: it still sees the job,
/// and the shell (having reaped it and taken the terminal) writes its prompt before the
/// drain's next read.
fn probe_mixed(_master: i32) -> i32 {
    if ARMED_MIXED.swap(false, Ordering::SeqCst) {
        let slave = SLAVE_MIXED.load(Ordering::SeqCst);
        // SAFETY: a bounded write of a live slice to the test-owned slave.
        let wrote = unsafe { libc::write(slave, RECLAIM.as_ptr().cast(), RECLAIM.len()) };
        assert_eq!(wrote, RECLAIM.len() as isize);
        FG_MIXED.store(SHELL, Ordering::SeqCst);
        return JOB;
    }
    FG_MIXED.load(Ordering::SeqCst)
}

/// A job frame and the shell's prompt in one gathered batch: the park sample cuts it at the
/// frame's end, and the handback lands there, after the job's last byte and before the
/// shell's first; the byte tap carries it at the same offset.
#[test]
fn foreground_handback_a_mixed_batch_is_cut_at_the_park() {
    const FRAME_LEN: usize = 1024;
    let mut frame = b"\x1b[H".to_vec();
    frame.resize(FRAME_LEN, b'x');
    let expected_handback = {
        let mut t = Terminal::new(24, 80);
        t.process(INCIDENT);
        t.process(&frame);
        t.foreground_handback().expect("the gate is open").bytes
    };
    let mut one_batch = false;
    for attempt in 0..5u64 {
        FG_MIXED.store(JOB, Ordering::SeqCst);
        ARMED_MIXED.store(false, Ordering::SeqCst);
        let rig = Rig::attach(
            73 + attempt,
            (probe_mixed, &FG_MIXED),
            (gone_mixed, &DEAD_MIXED),
            INCIDENT,
        );
        rig.kill(JOB);
        SLAVE_MIXED.store(rig.slave, Ordering::SeqCst);
        let taps = rig.session.ctx.byte_fanout.subscribe();
        ARMED_MIXED.store(true, Ordering::SeqCst);
        rig.write(&frame, "the frame and the shell's output", |t| {
            screen_has(t, "zsh: killed") && t.modes().bracketed_paste
        });
        {
            let term = rig.term();
            let t = term.lock().expect("terminal lock");
            assert!(!leak(&t), "attempt {attempt}: clean");
            assert!(!t.is_alternate_screen(), "attempt {attempt}");
            assert!(screen_has(&t, "zsh: killed"), "the shell's text is on main");
        }
        assert_eq!(
            rig.restored_n(1).len(),
            1,
            "attempt {attempt}: {:?}",
            rig.restored()
        );
        let mut stream = Vec::new();
        let mut bursts = Vec::new();
        wait_for("every burst on the tap", || {
            let (more, dropped) = taps.drain();
            assert_eq!(dropped, 0);
            for b in more {
                stream.extend_from_slice(&b.bytes);
                bursts.push(b.bytes);
            }
            stream.len() >= FRAME_LEN + expected_handback.len() + RECLAIM.len()
        });
        let want = [&frame[..], &expected_handback[..], RECLAIM].concat();
        assert_eq!(
            stream, want,
            "attempt {attempt}: the handback rides the tap at the cut"
        );
        if bursts.len() == 1 {
            one_batch = true;
            break;
        }
    }
    assert!(
        one_batch,
        "the park sample never cut a mixed batch at the frame's end in 5 attempts"
    );
}

/// A probe that never answers: no cut, no handback.
fn probe_none(_master: i32) -> i32 {
    -1
}
static FG_NONE: AtomicI32 = AtomicI32::new(SHELL);
static DEAD_NONE: Dead = Mutex::new(Vec::new());
fn gone_none(_master: i32, pgid: i32, _role: FgRole) -> bool {
    is_dead(&DEAD_NONE, pgid)
}

#[test]
fn foreground_handback_negative_control_no_probe_is_the_incident() {
    let model = foreground_handback_model();
    let buggy = interp::with_buggy(&model, 1);
    FG_NONE.store(SHELL, Ordering::SeqCst);
    let mut rig = Rig::attach(81, (probe_none, &FG_NONE), (gone_none, &DEAD_NONE), b"");

    let mut s = buggy.init_state();
    rig.write(ZLE_PROMPT, "the prompt", |t| t.modes().bracketed_paste);
    rig.write(ZLE_EXEC, "zle's 2004l", |t| !t.modes().bracketed_paste);
    rig.set_fg(JOB);
    rig.life = 1;
    rig.write(INCIDENT, "the job's modes", |t| {
        t.program_owns_terminal() && screen_has(t, "FRAME")
    });
    rig.kill(JOB);
    rig.life = 2;
    rig.set_fg(SHELL);
    rig.life = 3;
    rig.write(RECLAIM, "zsh's reclaim output", |t| {
        screen_has(t, "zsh: killed")
    });
    fire(
        &buggy,
        &mut s,
        &[
            "Launch",
            "JobWrite",
            "ReadJob",
            "Park",
            "Deliver",
            "Parse",
            "Die",
            "Reclaim",
            "ShellWrite",
            "ReadShell",
            "Park",
            "Deliver",
            "Parse",
        ],
    );
    let real = rig.project();
    assert_eq!(
        (real["prompt"], real["leak"], real["tail"]),
        (1, 1, 0),
        "the 2026-09-25 terminal: the prompt under the dead program's modes"
    );
    assert_eq!(
        real,
        projected(&s),
        "exactly where the Buggy = 1 replay lands"
    );
    assert!(
        !model.check_invariant("PromptNotHijacked", &s),
        "the committed model rejects it"
    );
    assert!(rig.restored().is_empty(), "{:?}", rig.restored());
    let term = rig.term();
    assert!(term.lock().expect("terminal lock").is_alternate_screen());
}

static FG_STOP: AtomicI32 = AtomicI32::new(SHELL);
fn probe_stop(_master: i32) -> i32 {
    FG_STOP.load(Ordering::SeqCst)
}
static DEAD_STOP: Dead = Mutex::new(Vec::new());
fn gone_stop(_master: i32, pgid: i32, _role: FgRole) -> bool {
    is_dead(&DEAD_STOP, pgid)
}

/// Ctrl-Z then `fg`: the stop edge hands nothing back (the leader lives), `fg` finds the
/// job's modes in force, and only its death hands back.
#[test]
fn foreground_handback_a_stopped_job_keeps_its_modes_until_it_dies() {
    let model = foreground_handback_model();
    FG_STOP.store(SHELL, Ordering::SeqCst);
    let mut rig = Rig::attach(91, (probe_stop, &FG_STOP), (gone_stop, &DEAD_STOP), b"");
    let mut s = launch_the_job(&mut rig, &model);

    stop_the_job(&mut rig, &model, &mut s);
    check(&rig, &s, "the stop edge parsed");
    assert!(
        rig.restored().is_empty(),
        "a stopped job is not handed back: {:?}",
        rig.restored()
    );
    let evidence = rig.term().lock().expect("terminal lock").program_evidence();

    resume_with_its_modes(
        &mut rig,
        &model,
        &mut s,
        "fg: the job runs with its modes",
        Some(evidence),
    );

    rig.kill(JOB);
    die_and_hand_back(&mut rig, &model, &mut s);

    let every = interp::with_consts(&model, &[("Alive", 0)]);
    let mut b = every.init_state();
    fire(
        &every,
        &mut b,
        &[
            "Launch",
            "JobWrite",
            "ReadJob",
            "Park",
            "Deliver",
            "Parse",
            "Stop",
            "Reclaim",
            "ShellWrite",
            "ReadShell",
            "Park",
            "Deliver",
            "Parse",
            "Resume",
        ],
    );
    assert!(!model.check_invariant("JobKeepsItsModesWhileAlive", &b));
}

/// The scripted process table for the pipeline shape (`sleep 0 | sh -c 'printf
/// "\e[?1049h\e[?1003h"; sleep 30'`, whose leader exits at once): `leaders` answer `ESRCH`,
/// `stopped` groups have a stopped member.
struct Table {
    leaders: &'static Dead,
    stopped: &'static Dead,
}

impl Table {
    fn set(set: &Dead, pgid: i32, on: bool) {
        let mut v = set.lock().unwrap_or_else(|p| p.into_inner());
        v.retain(|p| *p != pgid);
        if on {
            v.push(pgid);
        }
    }
}

static FG_PIPE: AtomicI32 = AtomicI32::new(SHELL);
fn probe_pipe(_master: i32) -> i32 {
    FG_PIPE.load(Ordering::SeqCst)
}
static LEADERS_PIPE: Dead = Mutex::new(Vec::new());
static STOPPED_PIPE: Dead = Mutex::new(Vec::new());
static EMPTY_PIPE: Dead = Mutex::new(Vec::new());
/// The shipping rule (`foreground_handback::owner_gone`) over the scripted table.
fn gone_pipe(_master: i32, pgid: i32, role: FgRole) -> bool {
    crate::foreground_handback::owner_gone(
        role,
        || is_dead(&LEADERS_PIPE, pgid),
        || is_dead(&STOPPED_PIPE, pgid),
        || is_dead(&EMPTY_PIPE, pgid),
    )
}

static FG_PIPE_LEADER: AtomicI32 = AtomicI32::new(SHELL);
fn probe_pipe_leader(_master: i32) -> i32 {
    FG_PIPE_LEADER.load(Ordering::SeqCst)
}
static LEADERS_PIPE_LEADER: Dead = Mutex::new(Vec::new());
static STOPPED_PIPE_LEADER: Dead = Mutex::new(Vec::new());
/// The negative control's probe: the leader alone (`leader_gone`).
fn gone_pipe_leader_only(_master: i32, pgid: i32, _role: FgRole) -> bool {
    is_dead(&LEADERS_PIPE_LEADER, pgid)
}

/// The pipeline schedule up to its stop edge: the job arms its modes, its first stage (the
/// leader) exits while the TUI runs on (no model action), then Ctrl-Z stops it and zsh takes
/// the terminal.
fn run_pipeline_to_the_stop(rig: &mut Rig, model: &Model, table: &Table) -> State {
    Table::set(table.stopped, JOB, false);
    let mut s = launch_the_job(rig, model);

    rig.kill(JOB);
    check(rig, &s, "the first stage exited, the job runs on");

    Table::set(table.stopped, JOB, true);
    stop_the_job(rig, model, &mut s);
    s
}

/// A stopped pipeline whose first stage already exited keeps its modes, and `fg` finds them
/// in force.
#[test]
fn foreground_handback_a_stopped_pipeline_whose_leader_exited_keeps_its_modes() {
    let model = foreground_handback_model();
    FG_PIPE.store(SHELL, Ordering::SeqCst);
    let mut rig = Rig::attach(95, (probe_pipe, &FG_PIPE), (gone_pipe, &LEADERS_PIPE), b"");
    let table = Table {
        leaders: &LEADERS_PIPE,
        stopped: &STOPPED_PIPE,
    };
    let mut s = run_pipeline_to_the_stop(&mut rig, &model, &table);
    check(&rig, &s, "the stop edge parsed");
    assert!(
        rig.restored().is_empty(),
        "a stopped pipeline is not handed back: {:?}",
        rig.restored()
    );
    let evidence = rig.term().lock().expect("terminal lock").program_evidence();

    Table::set(table.stopped, JOB, false);
    resume_with_its_modes(
        &mut rig,
        &model,
        &mut s,
        "fg: the pipeline runs with its modes",
        Some(evidence),
    );

    assert!(is_dead(table.leaders, JOB), "the leader stayed gone");
    die_and_hand_back(&mut rig, &model, &mut s);
}

/// Negative control: under the leader-only probe the same pipeline is handed back at the stop
/// edge (the `Alive = 0` replay), and `fg` resumes it without its alt screen and mouse.
#[test]
fn foreground_handback_negative_control_the_leader_alone_strips_a_stopped_pipeline() {
    let committed = foreground_handback_model();
    let every = interp::with_consts(&committed, &[("Alive", 0)]);
    FG_PIPE_LEADER.store(SHELL, Ordering::SeqCst);
    let mut rig = Rig::attach(
        96,
        (probe_pipe_leader, &FG_PIPE_LEADER),
        (gone_pipe_leader_only, &LEADERS_PIPE_LEADER),
        b"",
    );
    let table = Table {
        leaders: &LEADERS_PIPE_LEADER,
        stopped: &STOPPED_PIPE_LEADER,
    };
    let s = run_pipeline_to_the_stop(&mut rig, &every, &table);
    let real = rig.project();
    assert_eq!(
        (real["life"], real["leak"], real["prompt"]),
        (5, 0, 1),
        "the stopped job's modes were stripped at the stop edge"
    );
    assert_eq!(
        real,
        projected(&s),
        "exactly where the Alive = 0 replay lands"
    );
    assert_one_handback(&rig, JOB, SHELL);

    Table::set(table.stopped, JOB, false);
    rig.write(ZLE_EXEC, "zle's 2004l", |t| !t.modes().bracketed_paste);
    rig.set_fg(JOB);
    rig.write(REPAINT, "the job's repaint", |t| screen_has(t, "FRAME2"));
    {
        let term = rig.term();
        let t = term.lock().expect("terminal lock");
        assert!(!t.is_alternate_screen(), "the probe's alt_screen=false");
        assert!(
            t.encode_mouse_motion(0, 10, 5, 0).is_none(),
            "the probe's mouse_mode=none"
        );
    }
    let mut resumed = s;
    fire(&every, &mut resumed, &["Resume"]);
    assert!(
        !committed.check_invariant("JobKeepsItsModesWhileAlive", &resumed),
        "the committed model rejects the resumed job without its modes"
    );
}

/// A command the shell runs while the job sits in the background (`ls`).
const LS: i32 = 4400;

static FG_BG: AtomicI32 = AtomicI32::new(SHELL);
fn probe_bg(_master: i32) -> i32 {
    FG_BG.load(Ordering::SeqCst)
}
static LEADERS_BG: Dead = Mutex::new(Vec::new());
static STOPPED_BG: Dead = Mutex::new(Vec::new());
static EMPTY_BG: Dead = Mutex::new(Vec::new());
/// The shipping rule (`foreground_handback::owner_gone`) over the scripted table.
fn gone_bg(_master: i32, pgid: i32, role: FgRole) -> bool {
    crate::foreground_handback::owner_gone(
        role,
        || is_dead(&LEADERS_BG, pgid),
        || is_dead(&STOPPED_BG, pgid),
        || is_dead(&EMPTY_BG, pgid),
    )
}

static FG_BG_HOLDER: AtomicI32 = AtomicI32::new(SHELL);
fn probe_bg_holder(_master: i32) -> i32 {
    FG_BG_HOLDER.load(Ordering::SeqCst)
}
static LEADERS_BG_HOLDER: Dead = Mutex::new(Vec::new());
static STOPPED_BG_HOLDER: Dead = Mutex::new(Vec::new());
/// The negative control's probe: the holder rule (`group_orphaned`) for every group.
fn gone_bg_holder_rule(_master: i32, pgid: i32, _role: FgRole) -> bool {
    crate::foreground_handback::group_orphaned(is_dead(&LEADERS_BG_HOLDER, pgid), || {
        is_dead(&STOPPED_BG_HOLDER, pgid)
    })
}

/// The stopped pipeline of [`run_pipeline_to_the_stop`] is `bg`'d (it runs on leaderless, no
/// member stopped; no model action), and the shell runs `ls`: two edges at which the job is
/// neither `from` nor `to` but still owns its input bits.
fn run_ls_behind_a_background_pipeline(rig: &mut Rig, table: &Table) {
    Table::set(table.stopped, JOB, false);
    rig.write(b"[1]  + continued  claude\r\n", "bg", |t| {
        screen_has(t, "continued")
    });
    rig.write(ZLE_EXEC, "zle's 2004l", |t| !t.modes().bracketed_paste);
    rig.set_fg(LS);
    rig.write(b"ls-out\r\n", "ls's output", |t| screen_has(t, "ls-out"));
    rig.kill(LS);
    Table::set(table.stopped, LS, false);
    rig.set_fg(SHELL);
    rig.write(b"% \x1b[?2004h", "the next prompt", |t| {
        t.modes().bracketed_paste
    });
}

/// A leaderless pipeline stopped, `bg`'d, and running while the shell runs `ls`: at both `ls`
/// edges the job is a bystander, gone only when its whole group is, so its modes stay and
/// `fg` finds them.
#[test]
fn foreground_handback_a_backgrounded_leaderless_pipeline_keeps_its_modes() {
    let model = foreground_handback_model();
    FG_BG.store(SHELL, Ordering::SeqCst);
    Table::set(&EMPTY_BG, JOB, false);
    let mut rig = Rig::attach(89, (probe_bg, &FG_BG), (gone_bg, &LEADERS_BG), b"");
    let table = Table {
        leaders: &LEADERS_BG,
        stopped: &STOPPED_BG,
    };
    let mut s = run_pipeline_to_the_stop(&mut rig, &model, &table);
    check(&rig, &s, "the stop edge parsed");
    let evidence = rig.term().lock().expect("terminal lock").program_evidence();

    run_ls_behind_a_background_pipeline(&mut rig, &table);
    check(
        &rig,
        &s,
        "`ls` ran and exited while the job ran in the background",
    );
    assert!(
        rig.restored().is_empty(),
        "a backgrounded pipeline is not handed back: {:?}",
        rig.restored()
    );
    assert_eq!(
        rig.term().lock().expect("terminal lock").program_evidence(),
        evidence,
        "every mode it armed"
    );

    resume_with_its_modes(
        &mut rig,
        &model,
        &mut s,
        "fg: the pipeline runs with its modes",
        None,
    );

    die_and_hand_back(&mut rig, &model, &mut s);
}

/// Negative control: the holder rule for every suspect hands the running background job's
/// modes back at the shell → `ls` edge, which `JobKeepsItsModesWhileAlive` rejects once `fg`
/// resumes it.
#[test]
fn foreground_handback_negative_control_the_holder_rule_strips_a_backgrounded_pipeline() {
    let model = foreground_handback_model();
    FG_BG_HOLDER.store(SHELL, Ordering::SeqCst);
    let mut rig = Rig::attach(
        88,
        (probe_bg_holder, &FG_BG_HOLDER),
        (gone_bg_holder_rule, &LEADERS_BG_HOLDER),
        b"",
    );
    let table = Table {
        leaders: &LEADERS_BG_HOLDER,
        stopped: &STOPPED_BG_HOLDER,
    };
    let s = run_pipeline_to_the_stop(&mut rig, &model, &table);
    check(
        &rig,
        &s,
        "the stop edge parsed: kept, as in the positive row",
    );
    run_ls_behind_a_background_pipeline(&mut rig, &table);
    let real = rig.project();
    assert_eq!(
        (real["life"], real["leak"]),
        (5, 0),
        "the running background job's modes were stripped"
    );
    assert_one_handback(&rig, SHELL, LS);
    let mut resumed = s;
    resumed.insert("leak", 0);
    fire(&model, &mut resumed, &["Resume"]);
    assert!(
        !model.check_invariant("JobKeepsItsModesWhileAlive", &resumed),
        "the committed model rejects the resumed job without its modes"
    );
}

static FG_PARKED: AtomicI32 = AtomicI32::new(SHELL);
fn probe_parked(_master: i32) -> i32 {
    FG_PARKED.load(Ordering::SeqCst)
}
static DEAD_PARKED: Dead = Mutex::new(Vec::new());
fn gone_parked(_master: i32, pgid: i32, _role: FgRole) -> bool {
    is_dead(&DEAD_PARKED, pgid)
}
static FG_PARKED_FRESH: AtomicI32 = AtomicI32::new(SHELL);
fn probe_parked_fresh(_master: i32) -> i32 {
    FG_PARKED_FRESH.load(Ordering::SeqCst)
}
static DEAD_PARKED_FRESH: Dead = Mutex::new(Vec::new());
fn gone_parked_fresh(_master: i32, pgid: i32, _role: FgRole) -> bool {
    is_dead(&DEAD_PARKED_FRESH, pgid)
}

/// The incident across a reader restart: the reader is parked (`park_reader`) while the job
/// holds the terminal; the job dies and zsh writes its prompt into the kernel queue; a new
/// reader attaches.
fn run_parked(carry: bool) -> (Rig, State) {
    let committed = foreground_handback_model();
    let model = interp::with_consts(&committed, &[("Carry", i64::from(carry))]);
    let mut rig = if carry {
        FG_PARKED.store(SHELL, Ordering::SeqCst);
        Rig::attach(
            93,
            (probe_parked, &FG_PARKED),
            (gone_parked, &DEAD_PARKED),
            b"",
        )
    } else {
        FG_PARKED_FRESH.store(SHELL, Ordering::SeqCst);
        Rig::attach(
            92,
            (probe_parked_fresh, &FG_PARKED_FRESH),
            (gone_parked_fresh, &DEAD_PARKED_FRESH),
            b"",
        )
    };
    let mut s = launch_the_job(&mut rig, &model);
    assert_eq!(
        rig.session.fg_holder.load(Ordering::Acquire),
        JOB,
        "the gather left the job as the session's holder"
    );

    rig.park();
    rig.kill(JOB);
    rig.life = 2;
    fire(&model, &mut s, &["Die"]);
    rig.set_fg(SHELL);
    rig.life = 3;
    fire(&model, &mut s, &["Reclaim"]);
    // SAFETY: a bounded write of a live slice to the test-owned slave.
    let wrote = unsafe { libc::write(rig.slave, RECLAIM.as_ptr().cast(), RECLAIM.len()) };
    assert_eq!(wrote, RECLAIM.len() as isize);
    fire(&model, &mut s, &["ShellWrite"]);
    check(&rig, &s, "the prompt queued while parked");

    if !carry {
        rig.session.fg_holder.store(0, Ordering::Release);
    }
    rig.reattach();
    fire(&model, &mut s, &["Restart"]);
    let term = rig.term();
    wait_for("the new reader parsed zsh's output", || {
        screen_has(&term.lock().expect("terminal lock"), "zsh: killed")
    });
    fire(&model, &mut s, &["ReadShell", "Park", "Deliver", "Parse"]);
    check(&rig, &s, "the new reader's first batch parsed");
    (rig, s)
}

#[test]
fn foreground_handback_a_job_that_dies_while_the_reader_is_parked_is_still_an_edge() {
    let (rig, s) = run_parked(true);
    assert_eq!(
        (s["prompt"], s["leak"], s["paste"], s["tail"]),
        (1, 0, 1, 0)
    );
    assert_one_handback(&rig, JOB, SHELL);
    assert!(
        !rig.term()
            .lock()
            .expect("terminal lock")
            .is_alternate_screen()
    );
}

#[test]
fn foreground_handback_negative_control_a_fresh_probe_after_the_park_is_the_incident() {
    let (rig, s) = run_parked(false);
    assert_eq!(
        (s["prompt"], s["leak"], s["tail"]),
        (1, 1, 0),
        "the 2026-09-25 terminal: no edge, the dead job's modes under the prompt"
    );
    assert!(!foreground_handback_model().check_invariant("PromptNotHijacked", &s));
    assert!(rig.restored().is_empty(), "{:?}", rig.restored());
    assert!(
        rig.term()
            .lock()
            .expect("terminal lock")
            .is_alternate_screen()
    );
}

static FG_CHILD: AtomicI32 = AtomicI32::new(SHELL);
fn probe_child(_master: i32) -> i32 {
    FG_CHILD.load(Ordering::SeqCst)
}
static DEAD_CHILD: Dead = Mutex::new(Vec::new());
fn gone_child(_master: i32, pgid: i32, _role: FgRole) -> bool {
    is_dead(&DEAD_CHILD, pgid)
}

/// The `gdb -tui` shape: the holder arms the alt screen, mouse and a hidden cursor and hands
/// the terminal to its child.
#[test]
fn foreground_handback_a_live_holder_handing_the_terminal_to_its_child_keeps_its_modes() {
    FG_CHILD.store(SHELL, Ordering::SeqCst);
    let rig = Rig::attach(94, (probe_child, &FG_CHILD), (gone_child, &DEAD_CHILD), b"");
    rig.write(ZLE_PROMPT, "the prompt", |t| t.modes().bracketed_paste);
    rig.write(ZLE_EXEC, "zle's 2004l", |t| !t.modes().bracketed_paste);
    rig.set_fg(JOB);
    rig.write(
        b"\x1b[?1049h\x1b[?1003h\x1b[?1006h\x1b[?25l(gdb) run",
        "the holder's TUI",
        |t| screen_has(t, "(gdb) run"),
    );
    let armed = rig.term().lock().expect("terminal lock").program_evidence();
    assert_ne!(armed, 0);

    rig.set_fg(CHILD);
    rig.write(b"\r\ninferior output\x1b[12;", "the child's output", |t| {
        screen_has(t, "inferior output")
    });
    rig.kill(CHILD);
    rig.set_fg(JOB);
    rig.write(b"\r\n(gdb) ", "the holder's prompt", |t| {
        t.row_text(usize::from(t.grid().cursor_row()))
            .is_some_and(|l| l.starts_with("(gdb)"))
    });
    {
        let term = rig.term();
        let t = term.lock().expect("terminal lock");
        assert_eq!(t.program_evidence(), armed, "the live holder's modes stay");
        assert!(t.is_alternate_screen() && !t.modes().cursor_visible);
        assert!(t.encode_mouse_motion(0, 10, 5, 0).is_some());
        assert!(t.parser_is_ground());
    }
    let events = rig.restored_n(1);
    assert_eq!(events.len(), 1, "only the torn sequence: {events:?}");
    assert!(
        events[0].starts_with(&format!("from={CHILD} to={JOB} "))
            && events[0].contains(" reverted=parser "),
        "{events:?}"
    );

    rig.kill(JOB);
    rig.set_fg(SHELL);
    rig.write(RECLAIM, "zsh's reclaim output", |t| {
        screen_has(t, "zsh: killed") && t.modes().bracketed_paste
    });
    {
        let term = rig.term();
        let t = term.lock().expect("terminal lock");
        assert!(!leak(&t) && !t.is_alternate_screen());
        assert!(t.modes().cursor_visible && t.modes().bracketed_paste);
    }
    let events = rig.restored_n(2);
    assert_eq!(events.len(), 2, "{events:?}");
    assert!(
        events[1].starts_with(&format!("from={JOB} to={SHELL} ")),
        "{events:?}"
    );
}

static FG_UNLOCKED: AtomicI32 = AtomicI32::new(SHELL);
fn probe_unlocked(_master: i32) -> i32 {
    FG_UNLOCKED.load(Ordering::SeqCst)
}
static DEAD_UNLOCKED: Dead = Mutex::new(Vec::new());
/// Every liveness question the reader asked, with whether the asking thread
/// held the term lock at that moment.
static ASKED_UNLOCKED: Mutex<Vec<(i32, bool)>> = Mutex::new(Vec::new());
fn gone_unlocked(_master: i32, pgid: i32, _role: FgRole) -> bool {
    ASKED_UNLOCKED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push((pgid, crate::term_lock_held_on_this_thread()));
    is_dead(&DEAD_UNLOCKED, pgid)
}

/// The incident with a probe that records whether the asking thread holds the term lock:
/// every liveness question is asked with the lock free, each group once per edge.
#[test]
fn foreground_handback_liveness_is_probed_outside_the_term_lock() {
    {
        let term = Mutex::new(Terminal::new(4, 10));
        assert!(!crate::term_lock_held_on_this_thread());
        let guard = crate::term_lock(&term);
        assert!(
            crate::term_lock_held_on_this_thread(),
            "the instrument sees a hold"
        );
        drop(guard);
        assert!(!crate::term_lock_held_on_this_thread());
    }

    let model = foreground_handback_model();
    FG_UNLOCKED.store(SHELL, Ordering::SeqCst);
    ASKED_UNLOCKED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clear();
    let mut rig = Rig::attach(
        97,
        (probe_unlocked, &FG_UNLOCKED),
        (gone_unlocked, &DEAD_UNLOCKED),
        b"",
    );
    let end = run_incident(&mut rig, &model);
    assert_eq!((end["prompt"], end["leak"], end["paste"]), (1, 0, 1));
    assert_eq!(rig.restored_n(1).len(), 1, "{:?}", rig.restored());
    let asked = ASKED_UNLOCKED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    assert!(
        asked.iter().all(|(_, held)| !held),
        "a liveness probe ran under the term lock: {asked:?}"
    );
    assert_eq!(
        asked.iter().map(|(pgid, _)| *pgid).collect::<Vec<_>>(),
        vec![SHELL, JOB],
        "each edge asks about each group once: the launch edge its old holder, \
         the death edge the job"
    );
}

/// The shell's prompt after a one-shot's clean exit: no `zsh:` notice.
const ONE_SHOT_PROMPT: &[u8] = b"\r\n~/src % \x1b[?2004h";

static FG_ONE_SHOT: AtomicI32 = AtomicI32::new(SHELL);
fn probe_one_shot(_master: i32) -> i32 {
    FG_ONE_SHOT.load(Ordering::SeqCst)
}
static DEAD_ONE_SHOT: Dead = Mutex::new(Vec::new());
fn gone_one_shot(_master: i32, pgid: i32, _role: FgRole) -> bool {
    is_dead(&DEAD_ONE_SHOT, pgid)
}

/// The one-shot schedule: zle hands the terminal to a one-shot (`JOB`) that writes `arm` and
/// exits; the terminal goes to the wrapper's next command (`CHILD`), then back to the shell.
fn run_one_shot(
    rig: &mut Rig,
    model: &Model,
    (launch, arm): (&str, &[u8]),
    mid: impl FnOnce(&Rig),
) -> State {
    rig.prompt_marker = Some("~/src %");
    let mut s = model.init_state();
    rig.write(ZLE_PROMPT, "the prompt", |t| t.modes().bracketed_paste);
    check(rig, &s, "Init");
    rig.write(ZLE_EXEC, "zle's 2004l", |t| !t.modes().bracketed_paste);
    rig.set_fg(JOB);
    rig.life = 1;
    fire(model, &mut s, &[launch]);
    check(rig, &s, launch);

    rig.write(arm, "the one-shot's mode", Terminal::program_owns_terminal);
    fire(
        model,
        &mut s,
        &["JobWrite", "ReadJob", "Park", "Deliver", "Parse"],
    );
    check(rig, &s, "the one-shot's output parsed");

    rig.kill(JOB);
    rig.life = 2;
    fire(model, &mut s, &["Die"]);
    check(rig, &s, "Die");
    rig.set_fg(CHILD);
    rig.life = 3;
    fire(model, &mut s, &["Reclaim"]);
    rig.write(b"inside\r\n", "the wrapped command's output", |t| {
        screen_has(t, "inside")
    });
    mid(rig);

    rig.kill(CHILD);
    rig.set_fg(SHELL);
    rig.write(ONE_SHOT_PROMPT, "the shell's prompt", |t| {
        screen_has(t, "~/src %") && t.modes().bracketed_paste
    });
    fire(
        model,
        &mut s,
        &["ShellWrite", "ReadShell", "Park", "Deliver", "Parse"],
    );
    check(rig, &s, "the shell's prompt parsed");
    s
}

/// `tput smcup; sh -c 'echo inside; sleep 3'` and `tput civis`: a gone group that armed only
/// display modes orphans nothing, so the alt screen holds for the wrapped command and after
/// it, the cursor stays hidden, and nothing is restored.
#[test]
fn foreground_handback_a_one_shot_display_mode_outlives_the_one_shot() {
    let model = foreground_handback_model();
    FG_ONE_SHOT.store(SHELL, Ordering::SeqCst);
    let mut rig = Rig::attach(
        98,
        (probe_one_shot, &FG_ONE_SHOT),
        (gone_one_shot, &DEAD_ONE_SHOT),
        b"",
    );
    let end = run_one_shot(
        &mut rig,
        &model,
        ("OneShot", b"\x1b[?1049h\x1b[?25l"),
        |rig| {
            let term = rig.term();
            let t = term.lock().expect("terminal lock");
            assert!(
                t.is_alternate_screen() && screen_has(&t, "inside"),
                "the wrapped command draws on the alt screen"
            );
            assert!(!t.modes().cursor_visible);
            drop(t);
            assert!(rig.restored().is_empty(), "{:?}", rig.restored());
        },
    );
    assert_eq!(
        (end["hij"], end["prompt"], end["leak"], end["paste"]),
        (0, 1, 1, 1)
    );
    for invariant in [
        "PromptNotHijacked",
        "ShellKeepsItsOwnModes",
        "OneShotKeepsItsModes",
    ] {
        assert!(model.check_invariant(invariant, &end), "{invariant}");
    }
    {
        let term = rig.term();
        let t = term.lock().expect("terminal lock");
        assert!(t.is_alternate_screen(), "the wrapper's alt screen holds");
        assert!(!t.modes().cursor_visible, "tput civis holds");
        assert!(t.modes().bracketed_paste, "zle's own 2004h");
    }
    assert!(rig.restored().is_empty(), "{:?}", rig.restored());

    let reviewed = interp::with_consts(&model, &[("Display", 1)]);
    let mut s = reviewed.init_state();
    for action in [
        "OneShot",
        "JobWrite",
        "ReadJob",
        "Park",
        "Deliver",
        "Parse",
        "Die",
        "Reclaim",
        "ShellWrite",
        "ReadShell",
        "Park",
        "Deliver",
        "Parse",
    ] {
        assert!(reviewed.fire(action, &mut s), "{action}: {s:?}");
    }
    assert_ne!(
        projected(&s),
        rig.project(),
        "the real reader is not where the reviewed rule lands"
    );
    assert!(!model.check_invariant("OneShotKeepsItsModes", &s));
}

static FG_ONE_SHOT_INPUT: AtomicI32 = AtomicI32::new(SHELL);
fn probe_one_shot_input(_master: i32) -> i32 {
    FG_ONE_SHOT_INPUT.load(Ordering::SeqCst)
}
static DEAD_ONE_SHOT_INPUT: Dead = Mutex::new(Vec::new());
fn gone_one_shot_input(_master: i32, pgid: i32, _role: FgRole) -> bool {
    is_dead(&DEAD_ONE_SHOT_INPUT, pgid)
}

/// The display-only row's non-vacuity control (and a residual): the same schedule with the
/// one-shot arming an input mode (`/usr/bin/printf '\e[?1000h'`) is handed back at the
/// one-shot's own exit, the edge where that row records nothing.
#[test]
fn foreground_handback_a_one_shot_input_mode_is_handed_back_at_its_exit() {
    let model = foreground_handback_model();
    FG_ONE_SHOT_INPUT.store(SHELL, Ordering::SeqCst);
    let mut rig = Rig::attach(
        99,
        (probe_one_shot_input, &FG_ONE_SHOT_INPUT),
        (gone_one_shot_input, &DEAD_ONE_SHOT_INPUT),
        b"",
    );
    let arm = b"\x1b[?1000h\x1b[?2004h";
    let end = run_one_shot(&mut rig, &model, ("Launch", arm), |rig| {
        let term = rig.term();
        let t = term.lock().expect("terminal lock");
        assert_eq!(
            t.modes().mouse_mode,
            aterm_types::mouse::MouseMode::None,
            "handed back before the wrapped command's first byte"
        );
        drop(t);
        assert_eq!(rig.restored_n(1).len(), 1, "{:?}", rig.restored());
    });
    assert_eq!(
        (end["hij"], end["prompt"], end["leak"], end["paste"]),
        (1, 1, 0, 1)
    );
    {
        let term = rig.term();
        let t = term.lock().expect("terminal lock");
        assert_eq!(t.modes().mouse_mode, aterm_types::mouse::MouseMode::None);
        assert!(t.modes().bracketed_paste, "zle's own 2004h survives");
    }
    let events = rig.restored_n(1);
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(
        events[0].starts_with(&format!("from={JOB} to={CHILD} "))
            && events[0]
                .split_whitespace()
                .find_map(|f| f.strip_prefix("reverted="))
                .is_some_and(|r| r.split(',').any(|x| x == "mouse")),
        "handed back at the one-shot's exit: {events:?}"
    );
}

/// The incident's modes in force under a zsh prompt, armed by the one holder the reader has
/// seen: the shell, which never goes away.
const STUCK_UNDER_THE_SHELL: &[u8] =
    b"\x1b[?1049h\x1b[>5u\x1b[?1003h\x1b[?1006h\x1b[>4;2m\x1b[?25lpublication % ";

static FG_RESET: AtomicI32 = AtomicI32::new(SHELL);
fn probe_reset(_master: i32) -> i32 {
    FG_RESET.load(Ordering::SeqCst)
}
static DEAD_RESET: Dead = Mutex::new(Vec::new());
fn gone_reset(_master: i32, pgid: i32, _role: FgRole) -> bool {
    is_dead(&DEAD_RESET, pgid)
}

/// `reset` hands back what the automatic handback never will, on the reader's parse stage:
/// the reply names what moved, the timeline records `reason=manual source=ctl`, the `bytes`
/// tap carries the synthesized bytes, the main screen is shown again rather than cleared, and
/// the reader keeps reading.
#[test]
fn manual_reset_hands_back_what_the_shell_armed_and_the_handback_never_will() {
    let mut rig = Rig::attach(
        9_261,
        (probe_reset, &FG_RESET),
        (gone_reset, &DEAD_RESET),
        b"",
    );
    rig.write(b"history line\r\n", "history", |t| {
        screen_has(t, "history line")
    });
    rig.write(STUCK_UNDER_THE_SHELL, "the stuck prompt", |t| {
        t.program_owns_terminal() && screen_has(t, "publication %")
    });
    assert!(
        rig.restored().is_empty(),
        "precondition: nothing hands the shell's own modes back: {:?}",
        rig.restored()
    );

    let tap = rig.session.ctx.byte_fanout.subscribe();
    let term = rig.term();
    let reply = crate::manual_reset::cmd_reset(rig.session.id, &term, &rig.session.ctx, "");
    assert!(reply.starts_with("OK reset reverted="), "{reply:?}");
    let reverted = reply
        .split_whitespace()
        .find_map(|f| f.strip_prefix("reverted="))
        .expect("reverted=");
    for name in [
        "kitty-alt",
        "alt",
        "mouse",
        "mouse-encoding",
        "mok",
        "cursor",
    ] {
        assert!(reverted.split(',').any(|r| r == name), "{name}: {reply:?}");
    }
    {
        let t = term.lock().expect("terminal lock");
        assert!(!t.program_owns_terminal(), "no program mode is left");
        assert!(!t.is_alternate_screen());
        assert!(
            screen_has(&t, "history line"),
            "the main screen is shown again, not cleared"
        );
    }
    let events = rig.restored_n(1);
    assert!(
        events[0].starts_with(&format!(
            "reason=manual source=ctl reverted={reverted} bytes="
        )),
        "{events:?}"
    );
    let marks = rig.modes_marks();
    assert_eq!(marks.len(), 1, "{marks:?}");
    assert!(
        marks[0].contains(&format!(" mark - modes n={} ", reverted.split(',').count()))
            && !marks[0].contains(" by="),
        "{marks:?}"
    );
    let mut tapped = Vec::new();
    wait_for("the reset's bytes on the tap", || {
        let (bursts, _) = tap.drain();
        for b in bursts {
            tapped.extend_from_slice(&b.bytes);
        }
        tapped.windows(8).any(|w| w == b"\x1b[?1049l")
    });

    rig.write(b"after\r\n", "output after the reset", |t| {
        screen_has(t, "after")
    });
    assert_eq!(
        crate::manual_reset::cmd_reset(rig.session.id, &term, &rig.session.ctx, ""),
        "OK reset reverted=- bytes=0\n",
        "nothing is stuck any more"
    );
    assert_eq!(
        crate::manual_reset::cmd_reset(rig.session.id, &term, &rig.session.ctx, "now"),
        "ERR usage: reset [flush]\n"
    );

    rig.write(b"\x1b[?1000h", "mouse armed again", |t| {
        t.program_owns_terminal()
    });
    rig.park();
    let reply = crate::manual_reset::cmd_reset(rig.session.id, &term, &rig.session.ctx, "");
    assert!(reply.contains("reverted=mouse"), "{reply:?}");
    assert!(!term.lock().expect("terminal lock").program_owns_terminal());
    let events = rig.restored_n(3);
    assert!(
        events[2].starts_with("reason=manual source=ctl reverted=mouse"),
        "{events:?}"
    );
}

/// What a replay must reproduce of `t`: every visible row, the screen in
/// front, and the modes the handback and the reset move.
fn seen(t: &Terminal) -> (Vec<String>, bool, bool, bool, bool) {
    let rows = (0..usize::from(t.rows()))
        .map(|r| t.row_text(r).unwrap_or_default())
        .collect();
    (
        rows,
        t.is_alternate_screen(),
        t.modes().cursor_visible,
        t.modes().bracketed_paste,
        t.encode_mouse_motion(0, 10, 5, 0).is_some(),
    )
}

/// `chunks` fed in order through a fresh engine of `like`'s geometry.
fn folded(like: &Terminal, chunks: &[&[u8]]) -> Terminal {
    let mut t = Terminal::new(like.rows(), like.cols());
    for chunk in chunks {
        t.process(chunk);
    }
    t
}

/// The rig's spine replayed to its end, read with the reader parked (which
/// drains its writer), nothing dropped on the way.
fn spine_replay(rig: &Rig) -> Terminal {
    let rec = rig
        .session
        .ctx
        .temporal
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    assert_eq!(rec.dropped_input(), 0, "the reader queued every slice");
    rec.replay_at(None).expect("the spine's end replays")
}

/// The ranges of the spine's retained `RawIn` slices whose batch is `burst`'s
/// allocation (`Arc::ptr_eq`), oldest first.
fn sharing(retained: &[(Arc<[u8]>, Range<usize>)], burst: &Arc<[u8]>) -> Vec<Range<usize>> {
    retained
        .iter()
        .filter(|(batch, _)| Arc::ptr_eq(batch, burst))
        .map(|(_, range)| range.clone())
        .collect()
}

static FG_SPINE: AtomicI32 = AtomicI32::new(SHELL);
fn probe_spine(_master: i32) -> i32 {
    FG_SPINE.load(Ordering::SeqCst)
}
static DEAD_SPINE: Dead = Mutex::new(Vec::new());
fn gone_spine(_master: i32, pgid: i32, _role: FgRole) -> bool {
    is_dead(&DEAD_SPINE, pgid)
}

/// A recorded session's spine and byte taps hold the bytes a foreground
/// handback and a manual reset synthesize where the engine processed them,
/// between slices of the batches the reader copied once: a replay of the spine
/// and a fold of the `bytes` tap each reach the live terminal, after the
/// incident's handback and again after a reset on a reattached reader.
#[test]
fn a_recording_holds_the_handback_and_reset_bytes_at_their_offsets() {
    let model = foreground_handback_model();
    FG_SPINE.store(SHELL, Ordering::SeqCst);
    let mut rig = Rig::attach_recording(72, (probe_spine, &FG_SPINE), (gone_spine, &DEAD_SPINE));
    let tap = rig.session.ctx.byte_fanout.subscribe();
    run_incident(&mut rig, &model);
    assert_one_handback(&rig, JOB, SHELL);
    rig.park();
    let term = rig.term();
    let handback = {
        let live = term.lock().expect("terminal lock");
        assert_eq!(
            seen(&spine_replay(&rig)),
            seen(&live),
            "the spine after the handback"
        );
        let mut twin = folded(&live, &[ZLE_PROMPT, ZLE_EXEC, INCIDENT]);
        let handback = twin
            .foreground_handback_scoped(true)
            .expect("the job's modes are handed back");
        let late = folded(
            &live,
            &[ZLE_PROMPT, ZLE_EXEC, INCIDENT, RECLAIM, &handback.bytes],
        );
        assert_ne!(
            seen(&late),
            seen(&live),
            "the handback's bytes past their cut"
        );
        handback.bytes
    };

    rig.reattach();
    rig.write(STUCK_UNDER_THE_SHELL, "the stuck prompt", |t| {
        t.program_owns_terminal() && screen_has(t, "publication %")
    });
    let reply = crate::manual_reset::cmd_reset(rig.session.id, &term, &rig.session.ctx, "");
    assert!(reply.starts_with("OK reset reverted="), "{reply:?}");
    let reset_len: usize = reply
        .trim_end()
        .rsplit_once(" bytes=")
        .and_then(|(_, n)| n.parse().ok())
        .expect("the reply counts the reset's bytes");
    rig.write(b"after\r\n", "output after the reset", |t| {
        screen_has(t, "after")
    });
    rig.park();
    let cast = &rig.session.ctx.cast;
    let watermark = cast.lock().unwrap_or_else(|p| p.into_inner()).watermark();
    let (cut, drawn) = {
        let live = term.lock().expect("terminal lock");
        let rows: Vec<String> = (0..usize::from(live.rows()))
            .map(|r| crate::control::visible_row(&live, r))
            .collect();
        (crate::cast::CastCut::take(&live, &watermark), rows)
    };
    let (recording, exact) = crate::cast::CastRecorder::snapshot_at(cast, &cut, Duration::ZERO);
    assert!(exact, "the recording holds the screen the reader drew");
    assert_eq!(
        recording
            .fold_frames(1, aterm_core::grid::ResizePolicy::Native)
            .pop()
            .map(|(_, rows)| rows),
        Some(drawn),
        "the recording folds to the screen"
    );
    let live = term.lock().expect("terminal lock");
    assert!(
        !live.program_owns_terminal(),
        "the reset handed the modes back"
    );
    assert_eq!(
        seen(&spine_replay(&rig)),
        seen(&live),
        "the spine after the reset"
    );
    let (taps, dropped) = tap.drain();
    assert_eq!(dropped, 0, "the tap kept every burst");
    let taps: Vec<std::sync::Arc<[u8]>> = taps.into_iter().map(|b| b.bytes).collect();
    let bursts: Vec<&[u8]> = taps.iter().map(|b| &b[..]).collect();
    assert_eq!(
        seen(&folded(&live, &bursts)),
        seen(&live),
        "the taps' bytes"
    );
    let pty: [&[u8]; 6] = [
        ZLE_PROMPT,
        ZLE_EXEC,
        INCIDENT,
        RECLAIM,
        STUCK_UNDER_THE_SHELL,
        b"after\r\n",
    ];
    let raw = folded(&live, &pty);
    assert_ne!(seen(&raw), seen(&live), "the PTY's bytes alone");

    let retained = rig
        .session
        .ctx
        .temporal
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .retained_inputs();
    let stream = pty.concat();
    let (plain, spliced): (Vec<_>, Vec<_>) = taps
        .iter()
        .partition(|b| stream.windows(b.len()).any(|w| w == &b[..]));
    for burst in &plain {
        let ranges = sharing(&retained, burst);
        assert_eq!(
            ranges
                .iter()
                .try_fold(0, |at, r| (r.start == at).then_some(r.end)),
            Some(burst.len()),
            "the spine's slices {ranges:?} tile the tap's burst {:?}",
            String::from_utf8_lossy(burst)
        );
    }
    let [handed_back, reset] = spliced[..] else {
        panic!("two bursts carry synthesized bytes, not {}", spliced.len());
    };
    assert!(
        handed_back
            .windows(handback.len())
            .any(|w| w == &handback[..]),
        "the first carries the handback at its cut"
    );
    assert_eq!(
        reset.len(),
        reset_len,
        "the second is the reset's bytes alone (an empty batch)"
    );
    for (burst, what) in [(handed_back, "the handback's"), (reset, "the reset's")] {
        assert!(
            sharing(&retained, burst).is_empty(),
            "{what} burst is a copy of its own, a batch no slice shares"
        );
    }
}

const OWNERSHIP_PROJECTED: [&str; 4] = ["life", "n", "bit", "backs"];

/// The real reader's ownership projection once `backs` events are recorded (an event follows
/// the term lock's release), or after the patience runs out, so the comparison says what
/// differs.
fn project_ownership(rig: &Rig, n: i64, backs: i64) -> State {
    let deadline = Instant::now() + PATIENCE;
    while (rig.restored().len() as i64) < backs && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    std::thread::sleep(Duration::from_millis(30));
    let term = rig.term();
    let t = term.lock().expect("terminal lock");
    [
        ("life", rig.life),
        ("n", n),
        (
            "bit",
            i64::from(t.modes().mouse_mode != aterm_types::mouse::MouseMode::None),
        ),
        ("backs", rig.restored().len() as i64),
    ]
    .into_iter()
    .collect()
}

fn ownership_projected(state: &State) -> State {
    OWNERSHIP_PROJECTED.iter().map(|k| (*k, state[k])).collect()
}

fn check_ownership(rig: &Rig, s: &State, at: &str) {
    assert_eq!(
        project_ownership(rig, s["n"], s["backs"]),
        ownership_projected(s),
        "{at}: {:?}",
        rig.restored()
    );
}

/// After the prompt (and whatever `before` did at it), a job that arms mouse
/// tracking in its own bytes, is killed and is reclaimed by the shell.
fn run_ownership_job(rig: &mut Rig, model: &Model, s: &mut State, job: i32) {
    rig.write(ZLE_EXEC, "zle's 2004l", |t| !t.modes().bracketed_paste);
    rig.set_fg(job);
    rig.life = 1;
    fire(model, s, &["Launch", "Sample"]);
    check_ownership(rig, s, "Launch: the job holds the terminal");
    rig.write(b"\x1b[?1003harmed\r\n", "the job's re-arm", |t| {
        t.modes().mouse_mode == aterm_types::mouse::MouseMode::AnyEvent && screen_has(t, "armed")
    });
    fire(model, s, &["Arm"]);
    check_ownership(rig, s, "Arm: the job's ?1003h over the stale ?1000h");
    rig.kill(job);
    rig.life = 2;
    fire(model, s, &["Die"]);
    rig.set_fg(SHELL);
    rig.write(RECLAIM, "zsh's reclaim output", |t| {
        screen_has(t, "zsh: killed") && t.modes().bracketed_paste
    });
    rig.life = 0;
    fire(model, s, &["Reclaim"]);
    check_ownership(rig, s, "Reclaim: the job's death");
}

/// The schedule's end state must not be where the replaced rule (`Buggy = 1`) lands.
fn assert_not_the_replaced_rule(rig: &Rig, model: &Model, schedule: &[&str]) {
    let buggy = interp::with_buggy(model, 1);
    let mut b = buggy.init_state();
    fire(&buggy, &mut b, schedule);
    assert_eq!(
        (b["bit"], b["backs"]),
        (1, 0),
        "the replaced rule strands the mode: {b:?}"
    );
    assert!(!model.check_invariant("ObservedArmIsHandedBack", &b));
    assert_ne!(
        project_ownership(rig, b["n"], 0),
        ownership_projected(&b),
        "the real reader is not where the replaced rule lands"
    );
}

static FG_OWN_MISSED: AtomicI32 = AtomicI32::new(SHELL);
fn probe_own_missed(_master: i32) -> i32 {
    FG_OWN_MISSED.load(Ordering::SeqCst)
}
static DEAD_OWN_MISSED: Dead = Mutex::new(Vec::new());
fn gone_own_missed(_master: i32, pgid: i32, _role: FgRole) -> bool {
    is_dead(&DEAD_OWN_MISSED, pgid)
}

/// A one-shot `/usr/bin/printf '\e[?1000h'` the reader never saw holding the terminal, then a
/// job that re-arms mouse tracking and is killed: the one-shot's own handback is lost (a
/// residual), the job's is not.
#[test]
fn foreground_handback_ownership_a_missed_one_shot_loses_only_its_own_handback() {
    const ONE_SHOT: i32 = 4500;
    let model = foreground_handback_ownership_model();
    FG_OWN_MISSED.store(SHELL, Ordering::SeqCst);
    let mut rig = Rig::attach(
        101,
        (probe_own_missed, &FG_OWN_MISSED),
        (gone_own_missed, &DEAD_OWN_MISSED),
        b"",
    );
    let mut s = model.init_state();
    rig.write(ZLE_PROMPT, "the prompt", |t| t.modes().bracketed_paste);
    check_ownership(&rig, &s, "Init: the prompt");

    rig.write(ZLE_EXEC, "zle's 2004l", |t| !t.modes().bracketed_paste);
    rig.life = 1;
    fire(&model, &mut s, &["Launch"]);
    check_ownership(&rig, &s, "Launch: the one-shot, never sampled");
    rig.write(b"\x1b[?1000h", "the one-shot's mouse", |t| {
        t.modes().mouse_mode == aterm_types::mouse::MouseMode::Normal
    });
    fire(&model, &mut s, &["Arm"]);
    check_ownership(&rig, &s, "Arm: parsed as the shell's bytes");
    rig.kill(ONE_SHOT);
    rig.life = 2;
    fire(&model, &mut s, &["Die"]);
    rig.write(b"one-shot-done\r\n% \x1b[?2004h", "the prompt again", |t| {
        screen_has(t, "one-shot-done") && t.modes().bracketed_paste
    });
    rig.life = 0;
    fire(&model, &mut s, &["Reclaim"]);
    check_ownership(&rig, &s, "Reclaim: no edge, the residual");
    assert_eq!(
        (s["bit"], s["backs"]),
        (1, 0),
        "the one-shot's mode is lost"
    );

    run_ownership_job(&mut rig, &model, &mut s, JOB);
    assert_eq!((s["done"], s["bit"], s["backs"]), (1, 0, 1), "{s:?}");
    let events = rig.restored();
    assert!(
        events[0].starts_with(&format!("from={JOB} to={SHELL} ")),
        "the job's own death: {events:?}"
    );
    assert_not_the_replaced_rule(
        &rig,
        &model,
        &[
            "Launch", "Arm", "Die", "Reclaim", "Launch", "Sample", "Arm", "Die", "Reclaim",
        ],
    );
}

static FG_OWN_SHELL: AtomicI32 = AtomicI32::new(SHELL);
fn probe_own_shell(_master: i32) -> i32 {
    FG_OWN_SHELL.load(Ordering::SeqCst)
}
static DEAD_OWN_SHELL: Dead = Mutex::new(Vec::new());
fn gone_own_shell(_master: i32, pgid: i32, _role: FgRole) -> bool {
    is_dead(&DEAD_OWN_SHELL, pgid)
}

/// zsh's builtin `printf '\e[?1000h'` arms mouse tracking in the shell's own bytes; the next
/// job that re-arms it and is killed is still handed back.
#[test]
fn foreground_handback_ownership_the_shells_own_mouse_does_not_hide_a_jobs_death() {
    let model = foreground_handback_ownership_model();
    FG_OWN_SHELL.store(SHELL, Ordering::SeqCst);
    let mut rig = Rig::attach(
        102,
        (probe_own_shell, &FG_OWN_SHELL),
        (gone_own_shell, &DEAD_OWN_SHELL),
        b"",
    );
    let mut s = model.init_state();
    rig.write(ZLE_PROMPT, "the prompt", |t| t.modes().bracketed_paste);
    check_ownership(&rig, &s, "Init: the prompt");
    rig.write(b"\x1b[?1000h\r\n% ", "the shell's own mouse", |t| {
        t.modes().mouse_mode == aterm_types::mouse::MouseMode::Normal
    });
    fire(&model, &mut s, &["ShellArm"]);
    check_ownership(&rig, &s, "ShellArm");

    run_ownership_job(&mut rig, &model, &mut s, JOB);
    assert_eq!((s["done"], s["bit"], s["backs"]), (1, 0, 1), "{s:?}");
    assert_not_the_replaced_rule(
        &rig,
        &model,
        &["ShellArm", "Launch", "Sample", "Arm", "Die", "Reclaim"],
    );
}
