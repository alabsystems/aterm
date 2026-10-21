// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A LANDED TASK KEEPS THE TIME IT LANDED ACROSS A REAL SUPERVISION RESTART
//! (`docs/DESIGN-harness-autopilot-2026-10-15.md` §10.2.7; the post-landing
//! check's owner-eye-4 and behaviour-5 found it of a Cruise done, which
//! FOREVER CRUISE / FOREVER LAND retired: a landing is now the one end a loop
//! seeds): the host's REAL worker runs aterm-agent's REAL hosted loop under
//! Land (`Session::run_hosted`, the seed it reads from the journal beside the
//! ledger, its start face and its first point) over a Claude Code screen
//! drawn into the App's own session terminal and read through the window's
//! own `text --json` (the control lane is a script only for the claim,
//! `meta`, `status` and the waits). The journal holds the landing's point and
//! its `DONE … rule=land@v1` note, nine minutes back, as a loop that landed
//! wrote them. What the worker tells goes into a real headless App's window,
//! as `App::apply_harness_fact` takes it from the host's wake.

use std::sync::mpsc;

use aterm_agent::harness::acts::TaskFace;
use aterm_agent::supervise::run::{Ctl, CtlReply, Session};

use super::*;
use crate::App;

/// The worker's last words: the landing's clean defense.
const LANDING_REPLY: &str = "Committed and pushed the parser and its tests.\nNothing left: all \
                             of it is merged into the remote main branch.";
/// Another reply, which no loop watched: a turn someone else ran while
/// nothing supervised the session.
const THEIR_REPLY: &str = "Rebased the branch onto main as you asked.";
/// The landing's note, as the loop that landed journals it.
const LANDED_NOTE: &str = "DONE seq=104 rule=land@v1 the worker said it is done after the land \
                           instruction (autopilot land): nothing more is typed until a person or \
                           an orchestrator types";

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
/// the claim and every other write is taken, `meta` says Land, `status`
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
                 autopilot=land agent_cwd=-\n",
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
    /// The journal beside the session's ledger, which the loop writes.
    journal: PathBuf,
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

    /// The `DONE … rule=land@v1` notes in the journal, oldest first: the
    /// landing written before the first loop (index 0), then each loop's
    /// own note of it (none while the journal is not written yet).
    fn landed_notes(&self) -> Vec<aterm_agent::supervise::journal::JournalRecord> {
        aterm_agent::supervise::journal::read_journal(&self.journal)
            .map(|(records, _)| {
                records
                    .into_iter()
                    .filter(|r| r.line.starts_with("DONE ") && r.line.contains(" rule=land@v1 "))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// THE LOOP'S NOTE OF THE LANDING, the next after the journal's first
    /// `before`: a must-happen wait for the record itself (a hang
    /// detector), which is how the test knows the loop that runs now has
    /// judged its first point and watches. Nothing the loop tells or asks
    /// says so: a restarted loop tells its start face before its first
    /// read, and journals the note only once that point's read is back,
    /// judged and its line journaled, right after that line (`say_point`)
    /// — a count of its screen reads, the last still in its hands, ended
    /// before the note was written (a full parallel run's "noted again").
    fn note_past(
        &self,
        before: usize,
        what: &str,
    ) -> aterm_agent::supervise::journal::JournalRecord {
        let mut notes = Vec::new();
        let found = aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            Duration::from_millis(5),
            || {
                notes = self.landed_notes();
                notes.get(before).cloned()
            },
        );
        found.unwrap_or_else(|| panic!("not within 60 s: {what}: the notes {notes:?}"))
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

/// The App's session 0 under Land, its screen the landing's defense, its
/// journal holding that landing (its point, marked with the defense's words,
/// and its note, nine minutes back), supervised by a host whose worker runs
/// the real hosted loop over [`Lane`], its ledger and journal under a scratch
/// directory of the test's own. The note's wall time is handed back with the
/// rest.
fn watched(app: &mut App, tag: &str) -> (Watched, String, i64) {
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
        .set("autopilot", Some("land".into()));
    let cols = usize::from(term.lock().unwrap().cols());
    term.lock()
        .unwrap()
        .process(&ended_screen(LANDING_REPLY, cols));
    let dir = std::env::temp_dir().join(format!(
        "aterm-done-restart-{tag}-{}-{}",
        std::process::id(),
        aterm_agent::supervise::journal::unix_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let ledger = dir.join(format!("{sid}.jsonl"));
    // The landing a loop before this one landed: its point, then its note.
    let noted = aterm_agent::supervise::journal::unix_ms() - 9 * 60_000;
    let mut point = aterm_agent::supervise::journal::JournalRecord::of_line(
        noted,
        Some(&sid),
        "EVENT idle seq=104 Nothing left: all of it is merged into the remote main branch.",
        None,
    );
    point.mark = aterm_agent::supervise::policy::turn_end::said_mark(Some(LANDING_REPLY));
    assert!(point.mark.is_some());
    let note = aterm_agent::supervise::journal::JournalRecord::of_line(
        noted,
        Some(&sid),
        LANDED_NOTE,
        None,
    );
    let journal = aterm_agent::supervise::approvals::journal_beside(&ledger);
    std::fs::write(
        &journal,
        format!("{}\n{}\n", point.to_json(), note.to_json()),
    )
    .unwrap();
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
    world.word(&sid, Some(Autopilot::Land));
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
            journal,
        },
        sid,
        noted,
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

/// Whether `told` holds a landing after an unknown: the restarted loop's
/// face.
fn landed_after_unknown(told: &[crate::HarnessFact]) -> bool {
    let f = faces(told);
    f.iter()
        .position(Option::is_none)
        .is_some_and(|at| f[at..].contains(&Some(TaskFace::Landed)))
}

/// THE REAL RESTART KEEPS THE LANDING'S TIME: a Land task whose journal holds
/// its landing is told the landing restated at the real loop's start (never
/// `open`) and nothing is typed; then its supervision restarted twice — the
/// agent's switch in Settings ▸ Harness off and on, and a SUPERVISION setting
/// saved (`approve`) — is told "unknown" and then `landed` at the restarted
/// loop's start, never `open`; the window gives the held face back with the
/// time it first moved there (the chip's, its title's and Settings ▸
/// Harness's clock), and nothing is typed. NEGATIVE CONTROL, in-test: a reply
/// someone else's turn left while nothing supervised the session (other
/// words) drops the seed at the first point: `landed` restated, then `open`.
#[test]
fn a_real_supervision_restart_keeps_a_landings_first_time() {
    let mut app = App::headless_for_test();
    let (w, sid, _) = watched(&mut app, "keep");
    let first = w.told_until("the first loop's landing", |t| {
        faces(t).contains(&Some(TaskFace::Landed))
    });
    // Its first point judged, and the loop watching: nothing more told.
    w.note_past(1, "the first loop's note of the landing");
    let mut first = first;
    first.extend(w.facts.try_iter());
    assert_eq!(
        faces(&first),
        [Some(TaskFace::Landed)],
        "the landing restated at the loop's start, kept at its first point"
    );
    apply(&mut app, &sid, &first);
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    let face_at = || ctx.timeline.lock().unwrap().task_face();
    assert_eq!(face_at().map(|(f, _)| f), Some(TaskFace::Landed));
    // The landing three hours back, so a restart's clock would show.
    let old = aterm_agent::supervise::journal::unix_ms() - 3 * 3_600_000;
    {
        let mut tl = ctx.timeline.lock().unwrap();
        tl.set_task_face(Some(TaskFace::Open), old);
        tl.set_task_face(Some(TaskFace::Landed), old);
    }
    let held = crate::harness_glass::Stamp::at(old.div_euclid(1000));

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
    for (notes, (how, steps)) in (2..).zip(restarts) {
        for cfg in steps {
            w.host.set_config(cfg);
        }
        let mut told = w.told_until(how, landed_after_unknown);
        // Its first point judged, and the loop watching: nothing more told.
        w.note_past(notes, how);
        told.extend(w.facts.try_iter());
        assert_eq!(
            faces(&told),
            [None, Some(TaskFace::Landed)],
            "{how}: unknown, then the landing restated — never open"
        );
        apply(&mut app, &sid, &told);
        assert_eq!(
            face_at(),
            Some((TaskFace::Landed, old)),
            "{how}: the held time"
        );
        let glass = app.chip_glass(0);
        assert_eq!(glass.task, Some(TaskFace::Landed), "{how}");
        assert_eq!(
            glass.task_at, held,
            "{how}: the chip's, the title's and the page's clock"
        );
    }
    assert_eq!(w.typed(), Vec::<String>::new(), "nothing typed");

    // NEGATIVE CONTROL: someone else's reply while nothing supervised.
    w.host.set_config(claude_off);
    let gone = w.told_until("unknown as the worker stops", |t| faces(t).contains(&None));
    let term = app.pool.get(0).unwrap().term.clone();
    let cols = usize::from(term.lock().unwrap().cols());
    term.lock()
        .unwrap()
        .process(&ended_screen(THEIR_REPLY, cols));
    w.host.set_config(on());
    let mut told = w.told_until("the seed dropped", |t| {
        faces(t).ends_with(&[Some(TaskFace::Landed), Some(TaskFace::Open)])
    });
    told.splice(0..0, gone);
    assert_eq!(
        faces(&told),
        [None, Some(TaskFace::Landed), Some(TaskFace::Open)],
        "the seed restated, then dropped at the first point over other words"
    );
    apply(&mut app, &sid, &told);
    assert_eq!(face_at().map(|(f, _)| f), Some(TaskFace::Open));
}

/// A RESTARTED LANDING IS TOLD WITH ITS FIRST NOTE'S TIME, AND A WINDOW THAT
/// HELD NO FACE SHOWS IT (§10.2.7: a seamless update's new window — the
/// session keeps its id, so its journal, but no face of it crossed into the
/// new window — stamped the restated end with the update's clock): the real
/// loop restates the journal's landing with the `t` of its note
/// (`HarnessFact::TaskNoted`); its supervision restarted twice (the agent's
/// switch off and on, a SUPERVISION setting saved) tells "unknown", then the
/// landing restated with that same FIRST note's time — each restarted loop
/// notes the landing again, carrying that time forward
/// (`JournalRecord::ended`) — and nothing is typed. The facts of the last
/// restart, applied to a second window that never held the session's face,
/// show that time on its timeline and its chip. NEGATIVE CONTROLS: the note's
/// time taken out of aterm-agent's loop (`Session`'s `seed_noted_at` call,
/// `supervise/run.rs`) fails this test — the landing is told plain, and the
/// new window stamps its own now; and the restated note's first time not
/// carried (`task_edge_with`'s `ended`) fails it at the second restart.
#[test]
fn a_restarted_landing_tells_its_first_notes_time_and_a_new_window_shows_it() {
    let mut app = App::headless_for_test();
    let (w, sid, noted) = watched(&mut app, "noted");
    let first = w.told_until("the first loop's landing", |t| {
        faces(t).contains(&Some(TaskFace::Landed))
    });
    assert!(
        first.contains(&crate::HarnessFact::TaskNoted(TaskFace::Landed, noted)),
        "the start restates the landing with its note's time {noted}: {first:?}"
    );
    apply(&mut app, &sid, &first);
    w.note_past(1, "the first loop's note of the landing");

    let mut claude_off = on();
    claude_off.set("claude.enabled", "false").unwrap();
    let mut approve_none = on();
    approve_none.set("approve", "none").unwrap();
    let restarts: [(&str, Vec<SupervisorConfig>); 2] = [
        ("the agent's switch off and on", vec![claude_off, on()]),
        ("a supervision setting saved", vec![approve_none]),
    ];
    let mut told = Vec::new();
    for (notes, (how, steps)) in (2..).zip(restarts) {
        std::thread::sleep(Duration::from_millis(5));
        for cfg in steps {
            w.host.set_config(cfg);
        }
        told = w.told_until(how, landed_after_unknown);
        // The restarted loop's own note: its first point judged.
        let again = w.note_past(notes, how);
        told.extend(w.facts.try_iter());
        assert_eq!(
            faces(&told),
            [None, Some(TaskFace::Landed)],
            "{how}: {told:?}"
        );
        assert!(
            told.contains(&crate::HarnessFact::TaskNoted(TaskFace::Landed, noted)),
            "{how}: the landing restated with its first note's time {noted}: {told:?}"
        );
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
        .set("autopilot", Some("land".into()));
    let fresh_sid = fresh_ctx.self_id.as_str().to_string();
    apply(&mut fresh, &fresh_sid, &told);
    assert_eq!(
        fresh_ctx.timeline.lock().unwrap().task_face(),
        Some((TaskFace::Landed, noted)),
        "the first note's time, not the new window's now"
    );
    assert_eq!(
        fresh.chip_glass(0).task_at,
        crate::harness_glass::Stamp::at(noted.div_euclid(1000))
    );
}

#[path = "harness_host_end_gate_tests.rs"]
mod end_gate_tests;
