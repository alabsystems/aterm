// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A DONE TASK KEEPS THE TIME IT FINISHED ACROSS A REAL SUPERVISION RESTART
//! (`docs/DESIGN-harness-autopilot-2026-10-15.md` §10.2.7; the post-landing
//! check's owner-eye-4 and behaviour-5, and the review of its fix, which found
//! the window's own test could not see the fix taken out): the host's REAL
//! worker runs aterm-agent's REAL hosted loop (`Session::run_hosted`, the seed
//! it reads from the journal beside the ledger, its start face and its first
//! point) over a Claude Code screen drawn into the App's own session terminal
//! and read through the window's own `text --json` (the control lane is a
//! script only for the claim, `meta`, `status` and the waits). What the
//! worker tells goes into a real headless App's window, as
//! `App::apply_harness_fact` takes it from the host's wake.

use std::sync::mpsc;

use aterm_agent::harness::acts::TaskFace;
use aterm_agent::supervise::run::{Ctl, CtlReply, Session};

use super::*;
use crate::App;

/// The worker's last words with its own done token, as Cruise reads a done.
const TOKEN_REPLY: &str = "Committed and pushed the parser and its tests.\nATERM-TASK-DONE";
/// Another reply's done, which no loop watched: a turn someone else ran while
/// nothing supervised the session.
const THEIR_REPLY: &str = "Rebased the branch onto main as you asked.\nATERM-TASK-DONE";

/// Claude Code 2.1.x idle after a turn of 3 minutes whose last words are
/// `said`, the composer empty, drawn at the terminal's own width `cols`: the
/// bytes that put it on a cleared screen, the cursor left after the caret.
fn ended_screen(said: &str, cols: usize) -> Vec<u8> {
    let mut rows: Vec<String> = Vec::new();
    for (n, line) in said.lines().enumerate() {
        rows.push(if n == 0 {
            format!("⏺ {line}")
        } else {
            format!("  {line}")
        });
    }
    rows.extend([
        String::new(),
        "✻ Worked for 3m 2s · done 4:24 PM".to_string(),
        String::new(),
    ]);
    let rule = "─".repeat(cols);
    rows.push(rule.clone());
    let caret = rows.len() + 1;
    rows.push("❯ ".to_string());
    rows.push(rule);
    rows.push("  ⏵⏵ bypass permissions on (shift+tab to cycle)".to_string());
    format!("\x1b[2J\x1b[H{}\x1b[{caret};3H", rows.join("\r\n")).into_bytes()
}

/// The control lane the real loop runs over: `text --json` is the window's
/// own read of the session's terminal ([`crate::control::cmd_text_json_looked`]);
/// the claim and every other write is taken, `meta` says Cruise, `status`
/// carries the terminal's sequence, and a wait for a change that does not come
/// times out at once (the screen stands). Every request is kept, `@sid` left
/// out.
struct Lane {
    term: Arc<Mutex<aterm_core::terminal::Terminal>>,
    ctx: Arc<crate::SessionCtx>,
    asked: Arc<Mutex<Vec<String>>>,
}

fn reply(code: i32, stdout: &str) -> CtlReply {
    CtlReply {
        code,
        stdout: stdout.to_string(),
        stderr: String::new(),
    }
}

impl Ctl for Lane {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        let words: Vec<&str> = args
            .iter()
            .copied()
            .filter(|a| !a.starts_with('@'))
            .collect();
        self.asked.lock().unwrap().push(words.join(" "));
        Ok(match words.as_slice() {
            ["meta"] => reply(
                0,
                "OK title=- user_title=- description=- icon=- role=- attention=- cwd=- \
                 state=alive attention_owner=- attention_owners=0 supervisor=- \
                 autopilot=cruise agent_cwd=-\n",
            ),
            ["text", "--json", rest @ ..] => {
                let args = crate::control::text_args(&rest.join(" "))?;
                reply(
                    0,
                    &crate::control::cmd_text_json_looked(&self.term, &self.ctx, args),
                )
            }
            ["status"] => {
                let seq = self.term.lock().unwrap().content_seq();
                reply(
                    0,
                    &format!(
                        "OK schema=1 sid=- phase=running hold=0 fabric=connected \
                         program=claude agent=- seq={seq} hand=-\n"
                    ),
                )
            }
            ["await", "seq", ..] => {
                std::thread::sleep(Duration::from_millis(5));
                reply(124, "OK timeout\n")
            }
            _ => reply(0, "OK\n"),
        })
    }
}

/// What the test reads back from one supervised session.
struct Watched {
    host: HostHandle,
    facts: mpsc::Receiver<crate::HarnessFact>,
    asked: Arc<Mutex<Vec<String>>>,
    dir: PathBuf,
}

impl Watched {
    /// The facts told since the last call, in order (a hang detector: at
    /// least one is waited for, until `done` says the run has told enough).
    fn told_until(
        &self,
        what: &str,
        done: impl Fn(&[crate::HarnessFact]) -> bool,
    ) -> Vec<crate::HarnessFact> {
        let mut told = Vec::new();
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while !done(&told) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "not within 60 s: {what}: {told:?}");
            if let Ok(fact) = self.facts.recv_timeout(left.min(Duration::from_millis(50))) {
                told.push(fact);
            }
        }
        told
    }

    /// How often the loop has read the screen so far.
    fn reads(&self) -> usize {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.starts_with("text --json"))
            .count()
    }

    /// Wait until the loop that runs now has read the screen `n` more
    /// times: its start face told, its first point judged, and it watching.
    fn read_more(&self, n: usize) {
        let from = self.reads();
        until("the loop read the screen again", || {
            self.reads() >= from + n
        });
    }

    /// Every write the loop asked for that types into the session.
    fn typed(&self) -> Vec<String> {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .filter(|r| {
                let verb = r.split(' ').next().unwrap_or_default();
                matches!(verb, "send" | "key" | "turn" | "paste" | "restart")
            })
            .cloned()
            .collect()
    }
}

impl Drop for Watched {
    fn drop(&mut self) {
        self.host.shutdown_and_join();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The App's session 0 under Cruise, its screen `said`'s done, supervised by a
/// host whose worker runs the real hosted loop over [`Lane`], its ledger and
/// journal under a scratch directory of the test's own.
fn watched(app: &mut App, tag: &str) -> (Watched, String) {
    let entry = app.pool.get(0).unwrap();
    let (term, ctx) = (entry.term.clone(), entry.ctx.clone());
    let sid = ctx.self_id.as_str().to_string();
    {
        let mut tl = ctx.timeline.lock().unwrap();
        tl.note_foreground_group(7);
        tl.set_program(7, Some("claude".into()));
    }
    ctx.meta
        .lock()
        .unwrap()
        .set("autopilot", Some("cruise".into()));
    let cols = usize::from(term.lock().unwrap().cols());
    term.lock()
        .unwrap()
        .process(&ended_screen(TOKEN_REPLY, cols));
    let dir = std::env::temp_dir().join(format!(
        "aterm-done-restart-{tag}-{}-{}",
        std::process::id(),
        aterm_agent::supervise::journal::unix_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let ledger = dir.join(format!("{sid}.jsonl"));
    let asked: Arc<Mutex<Vec<String>>> = Arc::default();
    let (lane_term, lane_ctx, lane_asked) = (term, ctx, Arc::clone(&asked));
    let body: Arc<BodyFn> = Arc::new(move |job: &WorkerJob| {
        let mut lane = Lane {
            term: Arc::clone(&lane_term),
            ctx: Arc::clone(&lane_ctx),
            asked: Arc::clone(&lane_asked),
        };
        // The job's own options, the ledger and its journal the test's.
        let opts = SuperviseOpts {
            ledger: Some(ledger.clone()),
            journal: Some(aterm_agent::supervise::approvals::journal_beside(&ledger)),
            ..job.opts.clone()
        };
        let mut session = Session::new(&mut lane, Some(format!("@{}", job.sid)));
        session.set_supervisor_name(Some(holder()));
        session.set_handover(Arc::clone(&job.handover));
        let mut out: Vec<u8> = Vec::new();
        match session.run_hosted(&opts, Arc::clone(&job.stop), &mut out) {
            Ok(()) => BodyEnd::Stopped,
            Err(e) => BodyEnd::Failed(e),
        }
    });
    let world = Arc::new(World::default());
    world.set(&[(sid.as_str(), Program::Claude)]);
    world.word(&sid, Some(Autopilot::Cruise));
    let (tx, facts) = mpsc::channel();
    let tx = Mutex::new(tx);
    let mut h = hooks(&world, body);
    let mine = sid.clone();
    h.fact = Arc::new(move |sid, fact| {
        if sid == mine {
            let _ = tx.lock().unwrap().send(fact);
        }
    });
    let host = HostHandle::start(on(), false, false, h);
    (
        Watched {
            host,
            facts,
            asked,
            dir,
        },
        sid,
    )
}

/// `told`, every fact applied to `app`'s window in order.
fn apply(app: &mut App, sid: &str, told: &[crate::HarnessFact]) {
    for fact in told {
        app.apply_harness_fact(sid, *fact);
    }
}

/// The faces in `told`, "unknown" said once however often it was told in a
/// row (the host tells it as a run ends and as its worker ends).
fn faces(told: &[crate::HarnessFact]) -> Vec<Option<TaskFace>> {
    let mut out: Vec<Option<TaskFace>> = Vec::new();
    for fact in told {
        let face = match fact {
            crate::HarnessFact::Task(face) => *face,
            crate::HarnessFact::TaskNoted(face, _) => Some(*face),
            _ => continue,
        };
        if !(face.is_none() && out.last() == Some(&None)) {
            out.push(face);
        }
    }
    out
}

/// Whether `told` holds a done after an unknown: the restarted loop's face.
fn done_after_unknown(told: &[crate::HarnessFact]) -> bool {
    let f = faces(told);
    f.iter()
        .position(Option::is_none)
        .is_some_and(|at| f[at..].contains(&Some(TaskFace::Done)))
}

/// THE REAL RESTART KEEPS THE DONE'S TIME: a Cruise task the real loop read
/// done (open at its start, done at its first point, its note journaled),
/// then its supervision restarted twice — the agent's switch in Settings ▸
/// Harness off and on, and a SUPERVISION setting saved (`approve`) — is told
/// "unknown" and then `done` at the restarted loop's start, never `open`;
/// the window gives the held face back with the time it first moved there
/// (the chip's, its title's and Settings ▸ Harness's clock), and nothing is
/// typed. NEGATIVE CONTROLS: the done seed taken out of aterm-agent's loop
/// (`Session`'s `seed_done` call, `supervise/run.rs`) fails this test — the
/// restart tells `open` before the done, and the window stamps it anew;
/// in-test, a reply someone else's turn left while nothing supervised the
/// session (another reply's done) is a new done: the seed withdrawn, `open`
/// then `done`, stamped at its own time.
#[test]
fn a_real_supervision_restart_keeps_a_dones_first_time() {
    let mut app = App::headless_for_test();
    let (w, sid) = watched(&mut app, "keep");
    let first = w.told_until("the first loop's done", |t| {
        faces(t).contains(&Some(TaskFace::Done))
    });
    assert_eq!(
        faces(&first),
        [Some(TaskFace::Open), Some(TaskFace::Done)],
        "a fresh loop: open at its start, done at its first point"
    );
    apply(&mut app, &sid, &first);
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    let face_at = || ctx.timeline.lock().unwrap().task_face();
    assert_eq!(face_at().map(|(f, _)| f), Some(TaskFace::Done));
    // The done three hours back, so a restart's clock would show.
    let old = aterm_agent::supervise::journal::unix_ms() - 3 * 3_600_000;
    {
        let mut tl = ctx.timeline.lock().unwrap();
        tl.set_task_face(Some(TaskFace::Open), old);
        tl.set_task_face(Some(TaskFace::Done), old);
    }
    let held = crate::harness_glass::Stamp::at(old.div_euclid(1000));
    w.read_more(3);

    let mut claude_off = on();
    claude_off.set("claude.enabled", "false").unwrap();
    let mut approve_none = on();
    approve_none.set("approve", "none").unwrap();
    let restarts: [(&str, Vec<SupervisorConfig>); 2] = [
        (
            "the agent's switch off and on",
            vec![claude_off.clone(), on()],
        ),
        ("a supervision setting saved", vec![approve_none]),
    ];
    for (how, steps) in restarts {
        for cfg in steps {
            w.host.set_config(cfg);
        }
        let mut told = w.told_until(how, done_after_unknown);
        // Its first point judged, and the loop watching: nothing more told.
        w.read_more(3);
        told.extend(w.facts.try_iter());
        assert_eq!(
            faces(&told),
            [None, Some(TaskFace::Done)],
            "{how}: unknown, then the done restated — never open"
        );
        apply(&mut app, &sid, &told);
        assert_eq!(
            face_at(),
            Some((TaskFace::Done, old)),
            "{how}: the held time"
        );
        let glass = app.chip_glass(0);
        assert_eq!(glass.task, Some(TaskFace::Done), "{how}");
        assert_eq!(
            glass.task_at, held,
            "{how}: the chip's, the title's and the page's clock"
        );
    }
    assert_eq!(w.typed(), Vec::<String>::new(), "nothing typed");

    // NEGATIVE CONTROL: someone else's done while nothing supervised.
    w.host.set_config(claude_off);
    let gone = w.told_until("unknown as the worker stops", |t| faces(t).contains(&None));
    let term = app.pool.get(0).unwrap().term.clone();
    let cols = usize::from(term.lock().unwrap().cols());
    term.lock()
        .unwrap()
        .process(&ended_screen(THEIR_REPLY, cols));
    w.host.set_config(on());
    let mut told = w.told_until("the new done", |t| {
        let f = faces(t);
        f.iter()
            .filter(|face| **face == Some(TaskFace::Done))
            .count()
            >= 2
            || f.ends_with(&[Some(TaskFace::Open), Some(TaskFace::Done)])
    });
    told.splice(0..0, gone);
    assert_eq!(
        faces(&told),
        [
            None,
            Some(TaskFace::Done),
            Some(TaskFace::Open),
            Some(TaskFace::Done)
        ],
        "the seed restated, withdrawn at the first point, then the new done"
    );
    apply(&mut app, &sid, &told);
    let (face, at) = face_at().unwrap();
    assert_eq!(face, TaskFace::Done);
    assert!(
        at > old + 3_600_000,
        "a new done, at its own time: {at} against {old}"
    );
    assert_ne!(app.chip_glass(0).task_at, held);
    assert_eq!(w.typed(), Vec::<String>::new(), "nothing typed");
}

/// A RESTARTED DONE IS TOLD WITH ITS FIRST NOTE'S TIME, AND A WINDOW THAT
/// HELD NO FACE SHOWS IT (§10.2.7; the review of the done's seed: a seamless
/// update's new window — the session keeps its id, so its journal, but no
/// face of it crossed into the new window — stamped the restated done with
/// the update's clock): the real loop read the task done and journaled its
/// note; its supervision restarted twice (the agent's switch off and on, a
/// SUPERVISION setting saved) tells "unknown", then the done restated with
/// the `t` of the FIRST `DONE … rule=finished@v1` note
/// (`HarnessFact::TaskNoted`) — each restarted loop notes the done again,
/// carrying that time forward (`JournalRecord::ended`) — and nothing is
/// typed. The facts of the last restart, applied to a second window that
/// never held the session's face, show that time on its timeline and its
/// chip. NEGATIVE CONTROLS: the note's time taken out of aterm-agent's loop
/// (`Session`'s `seed_noted_at` call, `supervise/run.rs`) fails this test —
/// the done is told plain, and the new window stamps its own now; and the
/// restated note's first time not carried (`task_edge_with`'s `ended`) fails
/// it: the first restart's note carries none (aterm-agent's
/// `a_restated_ends_note_carries_its_first_time_forward` shows the restart
/// after it then telling that restart's time).
#[test]
fn a_restarted_done_tells_its_first_notes_time_and_a_new_window_shows_it() {
    let mut app = App::headless_for_test();
    let (w, sid) = watched(&mut app, "noted");
    let first = w.told_until("the first loop's done", |t| {
        faces(t).contains(&Some(TaskFace::Done))
    });
    apply(&mut app, &sid, &first);
    w.read_more(3);
    let ledger = w.dir.join(format!("{sid}.jsonl"));
    let last_done = || {
        let (records, _) = aterm_agent::supervise::journal::read_journal(
            &aterm_agent::supervise::approvals::journal_beside(&ledger),
        )
        .unwrap();
        records
            .into_iter()
            .rev()
            .find(|r| r.line.starts_with("DONE ") && r.line.contains(" rule=finished@v1 "))
            .expect("the done's note")
    };
    let noted = last_done().t;

    let mut claude_off = on();
    claude_off.set("claude.enabled", "false").unwrap();
    let mut approve_none = on();
    approve_none.set("approve", "none").unwrap();
    let restarts: [(&str, Vec<SupervisorConfig>); 2] = [
        ("the agent's switch off and on", vec![claude_off, on()]),
        ("a supervision setting saved", vec![approve_none]),
    ];
    let mut told = Vec::new();
    for (how, steps) in restarts {
        std::thread::sleep(Duration::from_millis(5));
        for cfg in steps {
            w.host.set_config(cfg);
        }
        told = w.told_until(how, done_after_unknown);
        w.read_more(3);
        told.extend(w.facts.try_iter());
        assert_eq!(
            faces(&told),
            [None, Some(TaskFace::Done)],
            "{how}: {told:?}"
        );
        assert!(
            told.contains(&crate::HarnessFact::TaskNoted(TaskFace::Done, noted)),
            "{how}: the done restated with its first note's time {noted}: {told:?}"
        );
        let again = last_done();
        assert!(again.t > noted, "{how}: noted again: {again:?}");
        assert_eq!(again.ended, Some(noted), "{how}: its first time carried");
    }
    assert_eq!(w.typed(), Vec::<String>::new(), "nothing typed");

    // A window that never held the session's face: a successor's.
    let mut fresh = App::headless_for_test();
    let fresh_ctx = fresh.pool.get(0).unwrap().ctx.clone();
    fresh_ctx
        .meta
        .lock()
        .unwrap()
        .set("autopilot", Some("cruise".into()));
    let fresh_sid = fresh_ctx.self_id.as_str().to_string();
    apply(&mut fresh, &fresh_sid, &told);
    assert_eq!(
        fresh_ctx.timeline.lock().unwrap().task_face(),
        Some((TaskFace::Done, noted)),
        "the first note's time, not the new window's now"
    );
    assert_eq!(
        fresh.chip_glass(0).task_at,
        crate::harness_glass::Stamp::at(noted.div_euclid(1000))
    );
}
