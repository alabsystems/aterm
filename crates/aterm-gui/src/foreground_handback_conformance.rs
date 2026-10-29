// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance tests for `ForegroundHandback` (`aterm_models_gui::foreground_handback_model`).

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

const SHELL: i32 = 4100;
const JOB: i32 = 4200;
const CHILD: i32 = 4300;

const PROJECTED: [&str; 6] = ["fg", "life", "leak", "paste", "prompt", "tail"];

const PATIENCE: Duration = aterm_spec::wait::MUST_HAPPEN;

const ZLE_PROMPT: &[u8] = b"\x1b[?2004h% ";
const ZLE_EXEC: &[u8] = b"\x1b[?2004l\r\n";
const INCIDENT: &[u8] = b"\x1b[?1049h\x1b[>5u\x1b[?1003h\x1b[?1006h\x1b[>4;2m\x1b[?2004h\
\x1b[?1004h\x1b[?25l\x1b[?2026hFRAME";
const RECLAIM: &[u8] = b"zsh: killed     claude\r\n% \x1b[?2004h";
const SUSPENDED: &[u8] = b"\r\nzsh: suspended  claude\r\n% \x1b[?2004h";
const REPAINT: &[u8] = b"\x1b[?2004h\x1b[2J\x1b[HFRAME2";

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let seen =
        aterm_spec::wait::within(PATIENCE, Duration::from_millis(1), || done().then_some(()));
    assert!(seen.is_some(), "timed out waiting for {what}");
}

fn leak(t: &Terminal) -> bool {
    t.program_evidence() & !program_evidence::BRACKETED_PASTE != 0
}

fn screen_has(t: &Terminal, needle: &str) -> bool {
    (0..usize::from(t.rows())).any(|r| t.row_text(r).is_some_and(|l| l.contains(needle)))
}

type Dead = Mutex<Vec<i32>>;

fn is_dead(dead: &Dead, pgid: i32) -> bool {
    dead.lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains(&pgid)
}

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
    fn attach(
        id: u64,
        probe: (fn(i32) -> i32, &'static AtomicI32),
        gone: (FgGone, &'static Dead),
        pre: &[u8],
    ) -> Self {
        Self::attach_with(id, probe, gone, pre, false, 0)
    }

    /// A rig whose session's own process is `SHELL`, as a spawned session's is: the reader
    /// knows which group is the shell.
    fn attach_shell(
        id: u64,
        probe: (fn(i32) -> i32, &'static AtomicI32),
        gone: (FgGone, &'static Dead),
    ) -> Self {
        Self::attach_with(id, probe, gone, b"", false, SHELL)
    }

    fn attach_recording(
        id: u64,
        probe: (fn(i32) -> i32, &'static AtomicI32),
        gone: (FgGone, &'static Dead),
    ) -> Self {
        Self::attach_with(id, probe, gone, b"", true, 0)
    }

    fn attach_with(
        id: u64,
        (probe, fg): (fn(i32) -> i32, &'static AtomicI32),
        (gone, dead): (FgGone, &'static Dead),
        pre: &[u8],
        record: bool,
        pid: i32,
    ) -> Self {
        let mut app = App::headless_for_test();
        app.session_factory.fg_probe = probe;
        app.session_factory.fg_gone = gone;
        app.session_factory.temporal_recording = record;
        dead.lock().unwrap_or_else(|p| p.into_inner()).clear();
        let (master, slave) = open_raw_pty();
        let mut session = crate::stub_session(id);
        session.master = master;
        session.pid = pid;
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

    fn park(&mut self) {
        assert!(
            park_reader(&mut self.session, Instant::now() + PATIENCE),
            "the reader parks"
        );
    }

    fn reattach(&mut self) {
        self.start_reader();
    }

    fn kill(&self, pgid: i32) {
        self.dead
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(pgid);
    }

    fn term(&self) -> Arc<Mutex<Terminal>> {
        self.session.term.clone()
    }

    fn write(&self, bytes: &[u8], what: &str, done: impl Fn(&Terminal) -> bool) {
        // SAFETY: a bounded write of a live slice to the test-owned slave.
        let wrote = unsafe { libc::write(self.slave, bytes.as_ptr().cast(), bytes.len()) };
        assert_eq!(wrote, bytes.len() as isize, "write {what}");
        let term = self.term();
        wait_for(what, || done(&term.lock().expect("terminal lock")));
    }

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

    fn restored_n(&self, n: usize) -> Vec<String> {
        wait_for("the reader's modes-restored event", || {
            self.restored().len() >= n
        });
        self.restored()
    }

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

fn fire(model: &Model, state: &mut State, actions: &[&str]) {
    for action in actions {
        assert!(model.fire(action, state), "{action} from {state:?}");
    }
}

fn projected(state: &State) -> State {
    PROJECTED.iter().map(|k| (*k, state[k])).collect()
}

fn check(rig: &Rig, model_state: &State, at: &str) {
    assert_eq!(rig.project(), projected(model_state), "{at}");
}

fn assert_one_handback(rig: &Rig, from: i32, to: i32) {
    let events = rig.restored_n(1);
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(
        events[0].starts_with(&format!("from={from} to={to} ")),
        "{events:?}"
    );
}

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
    FG_MIXED.store(JOB, Ordering::SeqCst);
    ARMED_MIXED.store(false, Ordering::SeqCst);
    let rig = Rig::attach(
        73,
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
        assert!(!leak(&t), "clean");
        assert!(!t.is_alternate_screen());
        assert!(screen_has(&t, "zsh: killed"), "the shell's text is on main");
    }
    assert_eq!(rig.restored_n(1).len(), 1, "{:?}", rig.restored());
    let mut stream = Vec::new();
    wait_for("every burst on the tap", || {
        let (more, dropped) = taps.drain();
        assert_eq!(dropped, 0);
        for b in more {
            stream.extend_from_slice(&b.bytes);
        }
        stream.len() >= FRAME_LEN + expected_handback.len() + RECLAIM.len()
    });
    let want = [&frame[..], &expected_handback[..], RECLAIM].concat();
    assert_eq!(stream, want, "the handback rides the tap at the cut");
}

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
fn gone_pipe_leader_only(_master: i32, pgid: i32, _role: FgRole) -> bool {
    is_dead(&LEADERS_PIPE_LEADER, pgid)
}

fn run_pipeline_to_the_stop(rig: &mut Rig, model: &Model, table: &Table) -> State {
    Table::set(table.stopped, JOB, false);
    let mut s = launch_the_job(rig, model);

    rig.kill(JOB);
    check(rig, &s, "the first stage exited, the job runs on");

    Table::set(table.stopped, JOB, true);
    stop_the_job(rig, model, &mut s);
    s
}

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

const LS: i32 = 4400;

static FG_BG: AtomicI32 = AtomicI32::new(SHELL);
fn probe_bg(_master: i32) -> i32 {
    FG_BG.load(Ordering::SeqCst)
}
static LEADERS_BG: Dead = Mutex::new(Vec::new());
static STOPPED_BG: Dead = Mutex::new(Vec::new());
static EMPTY_BG: Dead = Mutex::new(Vec::new());
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
fn gone_bg_holder_rule(_master: i32, pgid: i32, _role: FgRole) -> bool {
    crate::foreground_handback::group_orphaned(is_dead(&LEADERS_BG_HOLDER, pgid), || {
        is_dead(&STOPPED_BG_HOLDER, pgid)
    })
}

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
static ASKED_UNLOCKED: Mutex<Vec<(i32, bool)>> = Mutex::new(Vec::new());
fn gone_unlocked(_master: i32, pgid: i32, _role: FgRole) -> bool {
    ASKED_UNLOCKED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push((pgid, crate::term_lock_held_on_this_thread()));
    is_dead(&DEAD_UNLOCKED, pgid)
}

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

const ONE_SHOT_PROMPT: &[u8] = b"\r\n~/src % \x1b[?2004h";

static FG_ONE_SHOT: AtomicI32 = AtomicI32::new(SHELL);
fn probe_one_shot(_master: i32) -> i32 {
    FG_ONE_SHOT.load(Ordering::SeqCst)
}
static DEAD_ONE_SHOT: Dead = Mutex::new(Vec::new());
fn gone_one_shot(_master: i32, pgid: i32, _role: FgRole) -> bool {
    is_dead(&DEAD_ONE_SHOT, pgid)
}

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

fn folded(like: &Terminal, chunks: &[&[u8]]) -> Terminal {
    let mut t = Terminal::new(like.rows(), like.cols());
    for chunk in chunks {
        t.process(chunk);
    }
    t
}

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

static FG_OWN_RESUMED: AtomicI32 = AtomicI32::new(SHELL);
fn probe_own_resumed(_master: i32) -> i32 {
    FG_OWN_RESUMED.load(Ordering::SeqCst)
}
static DEAD_OWN_RESUMED: Dead = Mutex::new(Vec::new());
fn gone_own_resumed(_master: i32, pgid: i32, _role: FgRole) -> bool {
    is_dead(&DEAD_OWN_RESUMED, pgid)
}

/// zsh's `fg` of a stopped job that writes nothing while it holds (a bg'd leaderless
/// pipeline, `foreground_handback_live_headless`): the reader's sample after zsh's
/// `continued` notice lands before zsh's `tcsetpgrp`, and the job dies before anything else
/// is read, so every sample reads the shell and no edge is cut. The reader saw the job arm its
/// mouse, so its death is handed back all the same (`ObservedArmIsHandedBack`), once.
#[test]
fn foreground_handback_ownership_a_resumed_job_whose_hold_was_never_sampled_is_handed_back() {
    let model = foreground_handback_ownership_model();
    FG_OWN_RESUMED.store(SHELL, Ordering::SeqCst);
    let mut rig = Rig::attach_shell(
        103,
        (probe_own_resumed, &FG_OWN_RESUMED),
        (gone_own_resumed, &DEAD_OWN_RESUMED),
    );
    let mut s = model.init_state();
    rig.write(ZLE_PROMPT, "the prompt", |t| t.modes().bracketed_paste);
    check_ownership(&rig, &s, "Init: the prompt");

    rig.write(ZLE_EXEC, "zle's 2004l", |t| !t.modes().bracketed_paste);
    rig.set_fg(JOB);
    rig.life = 1;
    fire(&model, &mut s, &["Launch", "Sample"]);
    rig.write(b"\x1b[?1003harmed\r\n", "the job's mouse", |t| {
        t.modes().mouse_mode == aterm_types::mouse::MouseMode::AnyEvent && screen_has(t, "armed")
    });
    fire(&model, &mut s, &["Arm"]);
    check_ownership(&rig, &s, "Arm: sampled as the job's");

    rig.set_fg(SHELL);
    rig.write(SUSPENDED, "zsh: suspended", |t| {
        screen_has(t, "zsh: suspended") && t.modes().bracketed_paste
    });
    check_ownership(&rig, &s, "the stop edge: a stopped job keeps its mouse");
    rig.write(
        b"fg\r\n[1]  + continued  armed\r\n",
        "zsh's continued notice, read while the shell still holds",
        |t| screen_has(t, "continued"),
    );
    check_ownership(&rig, &s, "fg: the shell's own notice");
    assert!(rig.restored().is_empty(), "{:?}", rig.restored());

    rig.set_fg(JOB);
    rig.kill(JOB);
    rig.life = 2;
    fire(&model, &mut s, &["Die"]);
    rig.set_fg(SHELL);
    rig.write(RECLAIM, "zsh's reclaim output", |t| {
        screen_has(t, "zsh: killed") && t.modes().bracketed_paste
    });
    rig.life = 0;
    fire(&model, &mut s, &["Reclaim"]);
    check_ownership(&rig, &s, "Reclaim: the death no edge was sampled for");
    assert_eq!((s["done"], s["bit"], s["backs"]), (1, 0, 1), "{s:?}");
    assert_one_handback(&rig, JOB, SHELL);
}

static FG_OWN_REUSED: AtomicI32 = AtomicI32::new(SHELL);
fn probe_own_reused(_master: i32) -> i32 {
    FG_OWN_REUSED.load(Ordering::SeqCst)
}
static DEAD_OWN_REUSED: Dead = Mutex::new(Vec::new());
/// The dead job's leader pid went to a process outside its group: the holder rule's leader probe
/// (`kill(pid, 0)`) answers, while the bystander rule (`kill(-pgid, 0)`) finds the group empty.
fn gone_own_reused(_master: i32, pgid: i32, role: FgRole) -> bool {
    role == FgRole::Bystander && is_dead(&DEAD_OWN_REUSED, pgid)
}

/// The missed death of [`foreground_handback_ownership_a_resumed_job_whose_hold_was_never_sampled_is_handed_back`]
/// after the system gave the job's leader pid to another process: the edge was found by the
/// bystander rule (no member of the group is left), and its handback is judged by the same rule,
/// so the job's mouse is handed back once rather than the edge being cut again on every batch.
#[test]
fn foreground_handback_ownership_a_missed_death_whose_leader_pid_was_reused_is_handed_back() {
    let model = foreground_handback_ownership_model();
    FG_OWN_REUSED.store(SHELL, Ordering::SeqCst);
    let mut rig = Rig::attach_shell(
        104,
        (probe_own_reused, &FG_OWN_REUSED),
        (gone_own_reused, &DEAD_OWN_REUSED),
    );
    let mut s = model.init_state();
    rig.write(ZLE_PROMPT, "the prompt", |t| t.modes().bracketed_paste);
    rig.write(ZLE_EXEC, "zle's 2004l", |t| !t.modes().bracketed_paste);
    rig.set_fg(JOB);
    rig.life = 1;
    fire(&model, &mut s, &["Launch", "Sample"]);
    rig.write(b"\x1b[?1003harmed\r\n", "the job's mouse", |t| {
        t.modes().mouse_mode == aterm_types::mouse::MouseMode::AnyEvent && screen_has(t, "armed")
    });
    fire(&model, &mut s, &["Arm"]);
    check_ownership(&rig, &s, "Arm: sampled as the job's");

    rig.set_fg(SHELL);
    rig.write(SUSPENDED, "zsh: suspended", |t| {
        screen_has(t, "zsh: suspended") && t.modes().bracketed_paste
    });
    rig.write(
        b"fg\r\n[1]  + continued  armed\r\n",
        "zsh's continued notice, read while the shell still holds",
        |t| screen_has(t, "continued"),
    );
    check_ownership(&rig, &s, "fg: the shell's own notice");
    assert!(rig.restored().is_empty(), "{:?}", rig.restored());

    rig.set_fg(JOB);
    rig.kill(JOB);
    rig.life = 2;
    fire(&model, &mut s, &["Die"]);
    rig.set_fg(SHELL);
    rig.write(RECLAIM, "zsh's reclaim output", |t| {
        screen_has(t, "zsh: killed") && t.modes().bracketed_paste
    });
    rig.life = 0;
    fire(&model, &mut s, &["Reclaim"]);
    check_ownership(&rig, &s, "Reclaim: the death no edge was sampled for");
    assert_eq!((s["done"], s["bit"], s["backs"]), (1, 0, 1), "{s:?}");
    assert_one_handback(&rig, JOB, SHELL);
}
