// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 bind for `ForegroundHandback`
//! (`aterm_spec::derive::foreground_handback_model`) — the 2026-09-25
//! "crashed" tab, where a Claude Code killed while it held the terminal left
//! the alt screen, kitty flags, modifyOtherKeys, mouse tracking, focus
//! reports, 2026 and a hidden cursor armed under the zsh that reclaimed it.
//!
//! The reader is the REAL one — `attach_reader_inner`, the gather + parse
//! pipeline every session runs — attached to a real pseudo-terminal whose
//! slave the test writes as the "shell" and the "job". Only three things are
//! scripted: the wakes go to a channel (a unit test has no event loop), the
//! foreground probe (`SessionFactory::fg_probe`, `tcgetpgrp` in shipping)
//! reads a per-test atomic the test moves exactly where a shell would call
//! `tcsetpgrp` — which is the one fact (K2) the design rests on — and the
//! liveness probe (`SessionFactory::fg_gone`, `kill(pgid, 0)` → `ESRCH` in
//! shipping) reads a per-test set of the fake groups the script has killed.
//!
//! Projection, at every checkpoint of a schedule:
//!
//! * `fg`     — the scripted foreground (0 the shell, 1 the job);
//! * `life`   — the script's position (0 prompt, 1 job holds, 2 dead, 3 back,
//!   4 stopped, 5 the shell holds while the job is stopped);
//! * `leak`   — `Terminal::program_owns_terminal()`, the evidence gate;
//! * `paste`  — `modes().bracketed_paste`;
//! * `prompt` — the shell's post-reclaim text (`zsh: killed`, or
//!   `zsh: suspended`) is on screen;
//! * `tail`   — 0: the rig lets the reader drain the job before the reclaim.
//!
//! and compared with the model's state after the same action prefix.
//!
//! NEGATIVE CONTROLS: the incident with a probe that never answers (the reader
//! as it was before the fix) ends at `prompt=1 leak=1 tail=0` — the state the
//! committed model rejects and exactly where its `Buggy = 1` replay lands —
//! and records no `modes-restored` event. The parked-reader schedule with the
//! carried holder cleared (a reader seeded from a fresh probe, as the reviewed
//! design did) lands exactly where the `Carry = 0` replay does.
//!
//! The 2026-09-27 schedules (`ForegroundHandbackOwnership`, the lane at load
//! 59-65): a one-shot the reader never saw, whose `?1000h` is parsed as the
//! shell's, and zsh's builtin `printf '\e[?1000h'`, each followed by a job
//! that arms mouse tracking again and is killed — handed back, with the
//! `Buggy = 1` replay (a bit that stayed on kept its owner) as the negative
//! control.
//!
//! The 2026-09-25 review's schedules: Ctrl-Z of a job that armed its modes
//! and then `fg` (`STOP_FG`: nothing is handed back until the job dies), the
//! same for a pipeline whose first stage (its group leader) already exited,
//! bound through the shipping orphan rule (`group_orphaned`: the leader gone
//! AND no member stopped) with the leader-only probe as its negative control
//! (it lands on the `Alive = 0` replay), a job
//! that dies while the reader is parked (`PARKED`), and a live `gdb -tui`
//! shape — a holder that hands the terminal to its own child and takes it back
//! — where the child's exit must not strip the holder's modes.
//!
//! The second 2026-09-25 review's schedules: the parse stage's liveness probe
//! never runs under the term lock (the scripted probe records the reader
//! thread's lock depth), and a one-shot that arms only a DISPLAY mode (`tput
//! smcup; cmd`) keeps it after it exits — bound to the model's `OneShot`
//! schedule, with the same one-shot arming an INPUT mode (`/usr/bin/printf
//! '\e[?1000h'`) handed back at the very same edge as its non-vacuity
//! control.
//!
//! The third 2026-09-25 review's schedule: the stopped leaderless pipeline,
//! `bg`'d, while the shell runs `ls` — two edges at which the job is a
//! BYSTANDER (neither `from` nor `to`), bound through the shipping
//! `owner_gone` (a bystander is gone only when its whole group is), with the
//! holder rule for every suspect as its negative control.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;
use aterm_spec::derive::{Model, foreground_handback_model, foreground_handback_ownership_model};
use aterm_spec::interp;

use super::{attach_reader_inner, park_reader};
use crate::foreground_handback::{FgGone, FgRole};
use crate::{App, Session, Wake, WindowId};

type State = BTreeMap<&'static str, i64>;

/// Fake process-group ids: the shell and the job (and, for the `gdb -tui`
/// shape, the job's own child). Never passed to a real `kill`: the liveness
/// probe is scripted.
const SHELL: i32 = 4100;
const JOB: i32 = 4200;
const CHILD: i32 = 4300;

/// The model variables the real pipeline projects onto.
const PROJECTED: [&str; 6] = ["fg", "life", "leak", "paste", "prompt", "tail"];

/// Generous: a loaded machine only slows the reader down.
const PATIENCE: Duration = Duration::from_secs(10);

/// What zle writes before it hands a command the terminal.
const ZLE_PROMPT: &[u8] = b"\x1b[?2004h% ";
const ZLE_EXEC: &[u8] = b"\x1b[?2004l\r\n";
/// What Claude Code had armed when it was killed (the owner's reproduction).
const INCIDENT: &[u8] = b"\x1b[?1049h\x1b[>5u\x1b[?1003h\x1b[?1006h\x1b[>4;2m\x1b[?2004h\
\x1b[?1004h\x1b[?25l\x1b[?2026hFRAME";
/// What zsh writes after it reaped the job and took the terminal back.
const RECLAIM: &[u8] = b"zsh: killed     claude\r\n% \x1b[?2004h";
/// What zsh writes after Ctrl-Z stopped the job and it took the terminal back.
const SUSPENDED: &[u8] = b"\r\nzsh: suspended  claude\r\n% \x1b[?2004h";
/// What the job draws when `fg` resumes it (a full repaint, as on SIGCONT).
const REPAINT: &[u8] = b"\x1b[?2004h\x1b[2J\x1b[HFRAME2";

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(1));
    }
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

/// `openpty(3)`, serialized across this module's rigs and retried. macOS's
/// libutil `openpty` is not safe to call from several threads at once: with
/// thirteen rigs attaching in parallel (2026-09-25, the one-shot and
/// lock-depth rows) it failed now and then with a meaningless errno (`-6`),
/// always in whichever rig lost the race. Other test modules open ptys too, so
/// the lock alone cannot exclude them; a failed open is retried.
fn open_pty_pair() -> (i32, i32) {
    static OPENPTY: Mutex<()> = Mutex::new(());
    let _serial = OPENPTY.lock().unwrap_or_else(|p| p.into_inner());
    let mut last = String::new();
    for _ in 0..20 {
        let (mut master, mut slave) = (-1i32, -1i32);
        // SAFETY: openpty(3) into two valid out-slots; no name/termios/winsize.
        let opened = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if opened == 0 {
            return (master, slave);
        }
        last = std::io::Error::last_os_error().to_string();
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("openpty failed 20 times: {last}");
}

/// [`open_pty_pair`] with the slave in raw mode, so byte counts are exact.
fn open_raw_pty() -> (i32, i32) {
    let (master, slave) = open_pty_pair();
    // SAFETY: a zeroed termios is a valid out-slot for tcgetattr, then
    // cfmakeraw/tcsetattr on the test-owned slave fd.
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
        (probe, fg): (fn(i32) -> i32, &'static AtomicI32),
        (gone, dead): (FgGone, &'static Dead),
        pre: &[u8],
    ) -> Self {
        let mut app = App::headless_for_test();
        app.session_factory.fg_probe = probe;
        app.session_factory.fg_gone = gone;
        dead.lock().unwrap_or_else(|p| p.into_inner()).clear();
        let (master, slave) = open_raw_pty();
        let mut session = crate::stub_session(id);
        session.master = master;
        if !pre.is_empty() {
            session.term.lock().expect("terminal lock").process(pre);
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
    /// Output written until [`Self::reattach`] waits in the kernel.
    fn park(&mut self) {
        assert!(
            park_reader(&mut self.session, Instant::now() + PATIENCE),
            "the reader parks"
        );
    }

    /// Attach a NEW reader to the parked session (the handoff's resume).
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
            ("leak", i64::from(t.program_owns_terminal())),
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

    /// Every `modes-restored` payload once at least `n` are recorded. The
    /// reader records the event just AFTER it releases the term lock (the
    /// timeline is a strict leaf, taken with no other lock held), so a screen
    /// a `write` already saw finished can precede its event by one descheduled
    /// reader slice: under load an immediate read came back empty and a
    /// schedule failed with `only the torn sequence: []` (merge review,
    /// 2026-09-25). Positive assertions wait here; an assertion that NOTHING
    /// was restored reads [`Self::restored`] directly.
    fn restored_n(&self, n: usize) -> Vec<String> {
        wait_for("the reader's modes-restored event", || {
            self.restored().len() >= n
        });
        self.restored()
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
            .map(|e| e.payload.clone())
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

    // SIGKILL: the job is gone and restored nothing; the shell reaps it and
    // takes the terminal back BEFORE writing (K2).
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
}

static FG_ADOPTED: AtomicI32 = AtomicI32::new(JOB);
fn probe_adopted(_master: i32) -> i32 {
    FG_ADOPTED.load(Ordering::SeqCst)
}
static DEAD_ADOPTED: Dead = Mutex::new(Vec::new());
fn gone_adopted(_master: i32, pgid: i32, _role: FgRole) -> bool {
    is_dead(&DEAD_ADOPTED, pgid)
}

/// The incident tab was an ADOPTED session: the reader attached while the job
/// held the terminal with its modes already in force.
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
/// The first sample after arming stands in for K2 inside one gather: it still
/// sees the job, and the shell (having reaped it and taken the terminal) writes
/// its prompt before the drain's next read. Every later sample sees the shell.
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

/// A job frame and the shell's prompt in ONE gathered batch: the park sample
/// cuts it at the frame's end, and the handback lands exactly there — after the
/// job's last byte, before the shell's first. The byte tap carries the
/// synthesized bytes at the same offset.
///
/// The frame is one tty queue (1 KiB, written with one `write`), so the drain
/// reads it whole and parks on the first dry gap; a loaded machine can instead
/// end the batch on its budget before parking, which splits the frame and the
/// prompt into two batches (the handback then runs at the second batch's
/// offset 0 — the same engine result). So the strict end-state checks hold on
/// every attempt, and the one-batch shape must be seen within a few.
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
        // The job writes its last frame and is killed; the probe below moves
        // the foreground to the shell that reaped it.
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
            assert!(!t.program_owns_terminal(), "attempt {attempt}: clean");
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
                stream.extend_from_slice(&b);
                bursts.push(b);
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

/// The reader as it was before the fix: no probe answer, no cut, no handback.
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

    // The same schedule; the projection is compared with the Buggy = 1 replay.
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

/// The 2026-09-25 review's live probe (`sh -c 'printf …; sleep 3'`, Ctrl-Z,
/// `fg`): the stop edge stripped the job's modes and nothing re-armed them, so
/// the resumed job ran without its alt screen and mouse. Now the stop edge
/// hands nothing back (the job's leader lives), `fg` finds the job's modes in
/// force, and only the job's death — the leader gone — hands back. Bound to the
/// model's `STOP_FG` schedule at every checkpoint.
#[test]
fn foreground_handback_a_stopped_job_keeps_its_modes_until_it_dies() {
    let model = foreground_handback_model();
    FG_STOP.store(SHELL, Ordering::SeqCst);
    let mut rig = Rig::attach(91, (probe_stop, &FG_STOP), (gone_stop, &DEAD_STOP), b"");
    let mut s = model.init_state();
    rig.write(ZLE_PROMPT, "the prompt", |t| t.modes().bracketed_paste);
    rig.write(ZLE_EXEC, "zle's 2004l", |t| !t.modes().bracketed_paste);
    rig.set_fg(JOB);
    rig.life = 1;
    fire(&model, &mut s, &["Launch"]);
    rig.write(INCIDENT, "the job's modes", |t| {
        t.program_owns_terminal() && screen_has(t, "FRAME")
    });
    fire(
        &model,
        &mut s,
        &["JobWrite", "ReadJob", "Park", "Deliver", "Parse"],
    );
    check(&rig, &s, "the job's output parsed");

    // Ctrl-Z: the job stops ALIVE; zsh takes the terminal and says so.
    rig.life = 4;
    fire(&model, &mut s, &["Stop"]);
    rig.set_fg(SHELL);
    rig.life = 5;
    fire(&model, &mut s, &["Reclaim"]);
    check(&rig, &s, "Stop + Reclaim");
    rig.write(SUSPENDED, "zsh: suspended", |t| {
        screen_has(t, "zsh: suspended") && t.modes().bracketed_paste
    });
    fire(
        &model,
        &mut s,
        &["ShellWrite", "ReadShell", "Park", "Deliver", "Parse"],
    );
    check(&rig, &s, "the stop edge parsed");
    assert!(
        rig.restored().is_empty(),
        "a stopped job is not handed back: {:?}",
        rig.restored()
    );
    let evidence = rig.term().lock().expect("terminal lock").program_evidence();

    // `fg`: zle turns 2004 off, the job gets the terminal back and repaints.
    rig.write(ZLE_EXEC, "zle's 2004l", |t| !t.modes().bracketed_paste);
    rig.set_fg(JOB);
    rig.life = 1;
    fire(&model, &mut s, &["Resume"]);
    rig.write(REPAINT, "the job's repaint", |t| screen_has(t, "FRAME2"));
    fire(
        &model,
        &mut s,
        &["JobWrite", "ReadJob", "Park", "Deliver", "Parse"],
    );
    check(&rig, &s, "fg: the job runs with its modes");
    {
        let term = rig.term();
        let t = term.lock().expect("terminal lock");
        assert_eq!(t.program_evidence(), evidence, "every mode it armed");
        assert!(t.is_alternate_screen());
        assert!(
            t.encode_mouse_motion(0, 10, 5, 0).is_some(),
            "its mouse still reports"
        );
    }
    assert!(model.check_invariant("JobKeepsItsModesWhileAlive", &s));

    // Only now does it die: handed back once, at the death edge.
    rig.kill(JOB);
    rig.life = 2;
    fire(&model, &mut s, &["Die"]);
    rig.set_fg(SHELL);
    rig.life = 3;
    fire(&model, &mut s, &["Reclaim"]);
    rig.write(RECLAIM, "zsh's reclaim output", |t| {
        screen_has(t, "zsh: killed") && t.modes().bracketed_paste
    });
    fire(
        &model,
        &mut s,
        &["ShellWrite", "ReadShell", "Park", "Deliver", "Parse"],
    );
    check(&rig, &s, "the death edge parsed");
    assert_eq!((s["prompt"], s["leak"], s["paste"]), (1, 0, 1));
    let events = rig.restored_n(1);
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(
        events[0].starts_with(&format!("from={JOB} to={SHELL} ")),
        "{events:?}"
    );

    // The reviewed design (a handback at every change) lands where the
    // `Alive = 0` replay does: the job resumed without its modes.
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

/// The pipeline shape (the 2026-09-25 review's second live probe, `sleep 0 |
/// sh -c 'printf "\e[?1049h\e[?1003h"; sleep 30'`): zsh made the FIRST stage
/// the job's group leader, and it has exited. `kill(pgid, 0)` answers `ESRCH`
/// for a job whose TUI still runs. The scripted table has two halves, as the
/// real one does: `leaders` (the leader pids that answer `ESRCH`) and
/// `stopped` (the groups with a stopped member).
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
/// The SHIPPING rule (`foreground_handback::owner_gone`, the body of
/// `group_gone`) over the scripted table.
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
/// The reviewed probe: the leader alone (`leader_gone`), the stopped member
/// never asked about.
fn gone_pipe_leader_only(_master: i32, pgid: i32, _role: FgRole) -> bool {
    is_dead(&LEADERS_PIPE_LEADER, pgid)
}

/// The pipeline schedule up to its stop edge, on the real reader: the job
/// arms its modes, its FIRST STAGE (the group leader) exits while the TUI
/// runs on — no model action, the job lives — then Ctrl-Z stops the TUI and
/// zsh takes the terminal and says so. Returns the model's state (after
/// `model`'s own `Launch … Stop, Reclaim, … Parse`), with every earlier
/// checkpoint compared against `model`.
fn run_pipeline_to_the_stop(rig: &mut Rig, model: &Model, table: &Table) -> State {
    Table::set(table.stopped, JOB, false);
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

    // `sleep 0` exits and is reaped: the group's LEADER answers ESRCH, while
    // the TUI after it in the pipeline runs on. In the model the job lives.
    rig.kill(JOB);
    check(rig, &s, "the first stage exited, the job runs on");

    // Ctrl-Z: the TUI stops ALIVE; zsh takes the terminal and says so.
    Table::set(table.stopped, JOB, true);
    rig.life = 4;
    fire(model, &mut s, &["Stop"]);
    rig.set_fg(SHELL);
    rig.life = 5;
    fire(model, &mut s, &["Reclaim"]);
    check(rig, &s, "Stop + Reclaim");
    rig.write(SUSPENDED, "zsh: suspended", |t| {
        screen_has(t, "zsh: suspended") && t.modes().bracketed_paste
    });
    fire(
        model,
        &mut s,
        &["ShellWrite", "ReadShell", "Park", "Deliver", "Parse"],
    );
    s
}

/// A stopped pipeline whose first stage already exited keeps its modes, and
/// `fg` finds them in force (the 2026-09-25 review's live probe: on the
/// reviewed build the stop edge reverted `alt,mouse` and `fg` resumed the
/// job without them — a regression on origin/main, where Ctrl-Z never touched
/// modes). Bound to the model's `STOP_FG` schedule at every checkpoint, the
/// leader's exit being no model action: the job lives. Its last member's death
/// hands back.
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

    // `fg`: SIGCONT, the TUI runs again and repaints.
    Table::set(table.stopped, JOB, false);
    rig.write(ZLE_EXEC, "zle's 2004l", |t| !t.modes().bracketed_paste);
    rig.set_fg(JOB);
    rig.life = 1;
    fire(&model, &mut s, &["Resume"]);
    rig.write(REPAINT, "the job's repaint", |t| screen_has(t, "FRAME2"));
    fire(
        &model,
        &mut s,
        &["JobWrite", "ReadJob", "Park", "Deliver", "Parse"],
    );
    check(&rig, &s, "fg: the pipeline runs with its modes");
    {
        let term = rig.term();
        let t = term.lock().expect("terminal lock");
        assert_eq!(t.program_evidence(), evidence, "every mode it armed");
        assert!(t.is_alternate_screen());
        assert!(
            t.encode_mouse_motion(0, 10, 5, 0).is_some(),
            "its mouse still reports"
        );
    }
    assert!(model.check_invariant("JobKeepsItsModesWhileAlive", &s));

    // The TUI is killed: nothing of the group is stopped and its leader is
    // gone. Handed back once, at this death edge.
    assert!(is_dead(table.leaders, JOB), "the leader stayed gone");
    rig.life = 2;
    fire(&model, &mut s, &["Die"]);
    rig.set_fg(SHELL);
    rig.life = 3;
    fire(&model, &mut s, &["Reclaim"]);
    rig.write(RECLAIM, "zsh's reclaim output", |t| {
        screen_has(t, "zsh: killed") && t.modes().bracketed_paste
    });
    fire(
        &model,
        &mut s,
        &["ShellWrite", "ReadShell", "Park", "Deliver", "Parse"],
    );
    check(&rig, &s, "the death edge parsed");
    assert_eq!((s["prompt"], s["leak"], s["paste"]), (1, 0, 1));
    let events = rig.restored_n(1);
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(
        events[0].starts_with(&format!("from={JOB} to={SHELL} ")),
        "{events:?}"
    );
}

/// NEGATIVE CONTROL: the same pipeline under the reviewed probe (the leader
/// alone) is handed back at the STOP edge — exactly where the `Alive = 0`
/// replay lands, a state the committed model says loses a live job's modes —
/// and `fg` then resumes the job without its alt screen and mouse, which is
/// what the review's live probe measured.
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
    let events = rig.restored_n(1);
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(
        events[0].starts_with(&format!("from={JOB} to={SHELL} ")),
        "{events:?}"
    );

    // `fg`: the job runs again, without the modes it armed.
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
/// The SHIPPING rule (`foreground_handback::owner_gone`) over the scripted
/// table: the edge's holder by its leader, a bystander by its whole group.
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
/// The reviewed probe: the holder rule (`group_orphaned`) for every group,
/// whatever side of the edge it is on.
fn gone_bg_holder_rule(_master: i32, pgid: i32, _role: FgRole) -> bool {
    crate::foreground_handback::group_orphaned(is_dead(&LEADERS_BG_HOLDER, pgid), || {
        is_dead(&STOPPED_BG_HOLDER, pgid)
    })
}

/// The stopped pipeline of [`run_pipeline_to_the_stop`] is `bg`'d — it runs
/// on in the background, leaderless, no member stopped; no model action, the
/// job lives and the shell holds — and the shell then runs `ls` and takes the
/// terminal back: two foreground edges at which the job is neither `from`
/// nor `to`, a BYSTANDER that still owns its input bits.
fn run_ls_behind_a_background_pipeline(rig: &mut Rig, table: &Table) {
    Table::set(table.stopped, JOB, false);
    rig.write(b"[1]  + continued  claude\r\n", "bg", |t| {
        screen_has(t, "continued")
    });
    rig.write(ZLE_EXEC, "zle's 2004l", |t| !t.modes().bracketed_paste);
    rig.set_fg(LS);
    rig.write(b"ls-out\r\n", "ls's output", |t| screen_has(t, "ls-out"));
    // `ls` exits and is reaped (its leader AND its group are gone); zsh takes
    // the terminal back BEFORE writing (K2).
    rig.kill(LS);
    Table::set(table.stopped, LS, false);
    rig.set_fg(SHELL);
    rig.write(b"% \x1b[?2004h", "the next prompt", |t| {
        t.modes().bracketed_paste
    });
}

/// The third 2026-09-25 review's L2, on the real reader: a pipeline whose
/// first stage (its leader) exited is stopped by Ctrl-Z (kept), `bg`'d, and
/// the shell runs `ls` while it runs in the background. At the shell → `ls`
/// and `ls` → shell edges the job is a bystander, and a bystander is gone
/// only when its whole group is: its modes stay, and `fg` finds them in
/// force. Bound to the model's `STOP_FG` schedule at every checkpoint (`bg`
/// and the `ls` are no model action: the job lives, the shell holds). Its
/// death as the edge's holder is still handed back, once.
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

    // `fg`: it takes the terminal and repaints, with its modes.
    rig.write(ZLE_EXEC, "zle's 2004l", |t| !t.modes().bracketed_paste);
    rig.set_fg(JOB);
    rig.life = 1;
    fire(&model, &mut s, &["Resume"]);
    rig.write(REPAINT, "the job's repaint", |t| screen_has(t, "FRAME2"));
    fire(
        &model,
        &mut s,
        &["JobWrite", "ReadJob", "Park", "Deliver", "Parse"],
    );
    check(&rig, &s, "fg: the pipeline runs with its modes");
    {
        let term = rig.term();
        let t = term.lock().expect("terminal lock");
        assert!(t.is_alternate_screen());
        assert!(
            t.encode_mouse_motion(0, 10, 5, 0).is_some(),
            "its mouse still reports"
        );
    }
    assert!(model.check_invariant("JobKeepsItsModesWhileAlive", &s));

    // Killed while it holds the terminal: the edge's HOLDER, by the leader
    // rule, handed back once at this death edge.
    rig.life = 2;
    fire(&model, &mut s, &["Die"]);
    rig.set_fg(SHELL);
    rig.life = 3;
    fire(&model, &mut s, &["Reclaim"]);
    rig.write(RECLAIM, "zsh's reclaim output", |t| {
        screen_has(t, "zsh: killed") && t.modes().bracketed_paste
    });
    fire(
        &model,
        &mut s,
        &["ShellWrite", "ReadShell", "Park", "Deliver", "Parse"],
    );
    check(&rig, &s, "the death edge parsed");
    assert_eq!((s["prompt"], s["leak"], s["paste"]), (1, 0, 1));
    let events = rig.restored_n(1);
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(
        events[0].starts_with(&format!("from={JOB} to={SHELL} ")),
        "{events:?}"
    );
}

/// NEGATIVE CONTROL: the same schedule under the reviewed probe (the holder
/// rule for every suspect) hands the running background job's modes back at
/// the shell → `ls` edge — the state the committed model's
/// `JobKeepsItsModesWhileAlive` rejects once `fg` resumes it.
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
    let events = rig.restored_n(1);
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(
        events[0].starts_with(&format!("from={SHELL} to={LS} ")),
        "{events:?}"
    );
    let mut resumed = s;
    resumed.insert("leak", 0);
    fire(&model, &mut resumed, &["Resume"]);
    assert!(
        !model.check_invariant("JobKeepsItsModesWhileAlive", &resumed),
        "the committed model rejects the resumed job without its modes"
    );
}

// One probe pair PER `run_parked` variant: the carried and the fresh-probe
// rows run in parallel, and sharing one scripted foreground let one row's
// `set_fg` land in the other's reader (seen 2026-09-25 as a flaky
// "the job's output parsed" mismatch once more rigs ran at once).
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

/// The incident across a reader restart: the reader is parked (the overlap
/// handoff's `park_reader`) while the job holds the terminal; the job dies and
/// zsh reaps it, reclaims the terminal and writes its prompt into the kernel
/// queue; a NEW reader is attached. `carry = true` is the shipping reader: it
/// starts from `Session::fg_holder` (the job), so its first sample is an edge
/// and the terminal is handed back. `carry = false` clears the carried holder
/// first — the reviewed design, a fresh probe — and is the negative control:
/// no edge, no handback, exactly the `Carry = 0` replay.
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
    let mut s = model.init_state();
    rig.write(ZLE_PROMPT, "the prompt", |t| t.modes().bracketed_paste);
    rig.write(ZLE_EXEC, "zle's 2004l", |t| !t.modes().bracketed_paste);
    rig.set_fg(JOB);
    rig.life = 1;
    fire(&model, &mut s, &["Launch"]);
    rig.write(INCIDENT, "the job's modes", |t| {
        t.program_owns_terminal() && screen_has(t, "FRAME")
    });
    fire(
        &model,
        &mut s,
        &["JobWrite", "ReadJob", "Park", "Deliver", "Parse"],
    );
    check(&rig, &s, "the job's output parsed");
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
    // zsh's output waits in the kernel: no reader runs.
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
    let events = rig.restored_n(1);
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(
        events[0].starts_with(&format!("from={JOB} to={SHELL} ")),
        "{events:?}"
    );
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

/// The `gdb -tui` shape (the review's second live probe: a python TUI that
/// forks a child into its own group and `tcsetpgrp`s it). The holder arms the
/// alt screen, mouse and a hidden cursor and hands the terminal to its child:
/// nothing is handed back while it lives. The child tears an escape sequence
/// and exits: that exit is an edge whose old holder IS gone, but every mode in
/// force is the live holder's, so only the torn sequence is cancelled. Only
/// the holder's own death hands the modes back. In the model's terms this is
/// the `life = 4 / 5` path with the child in the shell's place.
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

    // `run`: the holder hands the terminal to its child and stays alive.
    rig.set_fg(CHILD);
    rig.write(b"\r\ninferior output\x1b[12;", "the child's output", |t| {
        screen_has(t, "inferior output")
    });
    // The child exits (torn mid-sequence); the holder takes the terminal back.
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

    // The holder is killed: now its modes are orphaned and handed back.
    rig.kill(JOB);
    rig.set_fg(SHELL);
    rig.write(RECLAIM, "zsh's reclaim output", |t| {
        screen_has(t, "zsh: killed") && t.modes().bracketed_paste
    });
    {
        let term = rig.term();
        let t = term.lock().expect("terminal lock");
        assert!(!t.program_owns_terminal() && !t.is_alternate_screen());
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

/// The second 2026-09-25 review: the liveness probe ran UNDER the term lock,
/// at nearly every foreground edge — after any finished command the old
/// leader is reaped, so a member listing followed (on Linux, then, a walk of
/// all of `/proc`) while the render and input paths waited. The incident on
/// the real reader, with a probe that records the reader thread's lock depth:
/// every question is asked with the lock free, and each group once per edge.
/// RED on the reviewed reader, which asked both of its questions under the
/// lock. The instrument's own control: it reads a held guard as held.
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

/// The one-shot schedule on the real reader: zle hands the terminal to a
/// one-shot (`JOB`) that writes `arm` and exits (gone, exactly as a SIGKILLed
/// program is); the terminal goes to the command the wrapper runs next
/// (`CHILD`, which writes a line and exits), then back to the shell, which
/// draws its prompt. Every checkpoint but one is compared with `model`'s
/// `launch` schedule; the wrapped command is invisible to the model (to it,
/// like the shell, it is "not the job", and its output is not the shell's), so
/// `mid` asserts what the real terminal holds while it runs.
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

    // It exits cleanly and is reaped; the wrapper's next command runs.
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

    // It exits too; the shell takes the terminal and draws its prompt.
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

/// The second 2026-09-25 review's live probe (`tput smcup; sh -c 'echo
/// inside; sleep 3'`, and `tput civis`): the one-shot's alt screen and hidden
/// cursor were reverted the moment `tput` exited, and the wrapped command
/// drew on the main screen. A gone group that armed only DISPLAY modes
/// orphans nothing: the alt screen holds for the wrapped command and after
/// it, the cursor stays hidden, and no `modes-restored` is recorded. Bound to
/// the model's `OneShot` schedule; the reviewed rule is the `Display = 1`
/// replay, which lands elsewhere.
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
    // `tput smcup` then `tput civis`, as one one-shot's bytes.
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

    // The reviewed rule, replayed: the one-shot's modes reverted at the cut.
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

/// The one-shot row's non-vacuity control, and the documented residual: the
/// SAME schedule, rig and edges with the one-shot arming an INPUT mode
/// (`/usr/bin/printf '\e[?1000h'`, mouse tracking) is handed back — at the
/// one-shot's own exit, the edge where the display-only row records nothing.
/// So that row's silence is the input rule, not a missed edge. (Nothing tells
/// a clean one-shot exit from a SIGKILL; a mouse left reporting under the
/// prompt is the incident.)
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
    // The model's `Launch` job arms bracketed paste with its input modes.
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

// ---------------------------------------------------------------------------
// THE MANUAL RESET (2026-09-26, `crate::manual_reset`): the `reset` verb's
// function against the REAL reader.
// ---------------------------------------------------------------------------

/// The live session the 2026-09-26 audit found (window pid 6874, sid 0): the
/// incident's modes in force under a zsh prompt, armed by the holder the
/// reader has always seen — the shell, which never goes away.
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

/// `reset` hands back what the automatic handback never will, on the reader's
/// parse stage: the reply names what moved, the timeline records `reason=manual
/// source=ctl`, the `bytes` tap carries the synthesized bytes (the stream-order
/// claim), the main screen is shown again rather than cleared, and the reader
/// keeps reading. A second reset has nothing to send. With the reader PARKED
/// (the update handoff) the lane is closed and the reset runs directly.
///
/// NEGATIVE CONTROL, in-line: before the reset, the same real reader with the
/// same scripted probes has handed nothing back (`restored()` is empty and the
/// evidence gate still reads the modes) — the state the live window sat in for
/// 97 000 s.
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
    // The `bytes` tap got the synthesized bytes: they ride the stream.
    let mut tapped = Vec::new();
    wait_for("the reset's bytes on the tap", || {
        let (bursts, _) = tap.drain();
        for b in bursts {
            tapped.extend_from_slice(&b);
        }
        tapped.windows(8).any(|w| w == b"\x1b[?1049l")
    });

    // The reader is alive and reading after it.
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

    // PARKED: no parse stage takes requests; the reset runs directly.
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

// ---- ForegroundHandbackOwnership (2026-09-27, the lane at load 59-65) ------
//
// Tier-1 for `aterm_spec::derive::foreground_handback_ownership_model`: who
// owns an input mode across jobs when the reader MISSES one. The scripted
// probe never shows the missed one-shot (its whole life fell between two
// samples, or its bytes were read after the shell's reclaim — the starved
// gather under load), so its `?1000h` is parsed as the SHELL's. Projection:
// `life` and `n` (the script's position), `bit` (mouse tracking in force) and
// `backs` (the timeline's `modes-restored` events), at every checkpoint.
//
// NEGATIVE CONTROL: the same schedule replayed on the model at `Buggy = 1` —
// the rule this replaced, where a bit that stayed on kept its owner — lands at
// `bit = 1, backs = 0` (the lane's stuck session), and the real reader must
// not.

const OWNERSHIP_PROJECTED: [&str; 4] = ["life", "n", "bit", "backs"];

// The machine's other four actions carry `#[refines]` anchors on the shipping
// code: `Arm` and `ShellArm` on `FgOwners::observe` (whose slice holder decides
// which), `Reclaim` on `FgOwners::orphaned_by`, `Sample` on `FgCutter::sample`.
#[aterm_spec::spec_unmodeled(
    machine = "foreground_handback_ownership",
    action = "Launch",
    reason = "The shell's `tcsetpgrp` giving a job the terminal: an environment step. The \
              Tier-1 rows move the scripted foreground probe exactly there."
)]
#[aterm_spec::spec_unmodeled(
    machine = "foreground_handback_ownership",
    action = "Die",
    reason = "A process's death: an environment step. The Tier-1 rows add the job to the \
              scripted liveness probe's dead set exactly there."
)]
#[expect(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it"
)]
fn ownership_scope_waivers() {}

/// The real reader's ownership projection once `backs` events are recorded
/// (the reader records an event just after it releases the term lock), or
/// after the patience runs out — the comparison then says what differs.
fn project_ownership(rig: &Rig, n: i64, backs: i64) -> State {
    let deadline = Instant::now() + PATIENCE;
    while (rig.restored().len() as i64) < backs && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    // An expectation of NO new event: give a late one the time to show.
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

/// The schedule's end state must not be the replaced rule's.
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

/// THE LANE'S STUCK SESSION (2026-09-27, load 59-65, three runs): a one-shot
/// `/usr/bin/printf '\e[?1000h'` the reader never saw holding the terminal,
/// then a job that re-arms mouse tracking and is killed. The one-shot's own
/// handback is lost (the documented residual); the job's is not. Before the
/// asserted-evidence rule the job's death recorded nothing and its mouse
/// stayed on, as did every later death in the session.
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

    // The one-shot: the scripted foreground never leaves the shell.
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

/// The same stuck state with no load at all: zsh's BUILTIN `printf
/// '\e[?1000h'` arms mouse tracking in the shell's own bytes. The next job
/// that re-arms it and is killed is handed back.
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
