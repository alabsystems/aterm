// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::Duration;

use aterm_phase::Program;

use super::*;
use crate::task_record::{TaskMark, TaskState, mark_session_task};

/// A shell's prompt (OSC 133 `A`, `B`): the id of the block a line typed at it runs in.
fn prompt(term: &mut Terminal) -> u64 {
    term.process(b"\x1b]133;A\x07% \x1b]133;B\x07");
    term.all_blocks().last().expect("its prompt's block").id
}

/// `line` typed at the prompt standing and run (`C`).
fn run(term: &mut Terminal, line: &str) {
    term.process(line.as_bytes());
    term.process(b"\r\n\x1b]133;C\x07");
}

/// A shell's first prompt, the line typed there, and its program started: what a program worker's
/// tab holds once its line ran. The id of the block it runs in.
fn ran(term: &mut Terminal, line: &str) -> u64 {
    let block = prompt(term);
    run(term, line);
    block
}

/// `line` typed at the prompt [`exited`] drew, and run: the id of its block.
fn then(term: &mut Terminal, line: &str) -> u64 {
    term.process(b"\x1b]133;B\x07");
    let block = term
        .all_blocks()
        .last()
        .expect("the next prompt's block")
        .id;
    run(term, line);
    block
}

/// The program printed `lines`.
fn printed(term: &mut Terminal, lines: &[&str]) {
    for line in lines {
        term.process(line.as_bytes());
        term.process(b"\r\n");
    }
}

/// The program exited `status` (`D`) and the shell drew its next prompt (`A`), sealing its block.
fn exited(term: &mut Terminal, status: i32) {
    term.process(format!("\x1b]133;D;{status}\x07\x1b]133;A\x07% ").as_bytes());
}

/// A session whose task is a program worker's, `working`, and its record, no line typed yet.
fn program_worker(local: u64) -> (crate::Session, TaskRecord) {
    worker_of(local, Program::Generic)
}

fn worker_of(local: u64, agent: Program) -> (crate::Session, TaskRecord) {
    let session = crate::stub_session(local);
    mark_session_task(&session.ctx, local, &TaskMark::new(TaskState::Working)).expect("unfenced");
    let mut timeline = session.ctx.timeline.lock().unwrap();
    timeline.set_task_worker(agent, None);
    let record = timeline.task().cloned().expect("a task");
    drop(timeline);
    (session, record)
}

fn record_of(session: &crate::Session) -> TaskRecord {
    session
        .ctx
        .timeline
        .lock()
        .unwrap()
        .task()
        .cloned()
        .expect("a task")
}

/// `session`'s record once its launcher noted `block` as the one it typed its program into.
fn noted(session: &crate::Session, block: u64) -> TaskRecord {
    assert!(
        crate::task_record::note_program_block(&session.ctx, block, Duration::ZERO),
        "the typed block is noted"
    );
    record_of(session)
}

const QUIET: Unwatched = Unwatched {
    restored: false,
    job: false,
};

/// EXIT 0 FINISHES THE TASK, ITS OUTPUT TAIL ITS RESULT: the shell's status for the program's block
/// marks it `done why=exit-status exit=0`, and `result` answers `source=output` with what the
/// program printed, none of the prompt or the line typed. The concluded record keeps no block.
/// NEGATIVE CONTROL: before its line was typed, and while it runs, nothing ends.
#[test]
fn a_program_that_exits_0_finishes_its_task_with_its_output_tail_as_its_result() {
    let (session, unnoted) = program_worker(7101);
    let mut term = session.term.lock().unwrap();
    assert!(
        !awaits_its_end(&unnoted) && !started(&term, &unnoted),
        "CONTROL: nothing typed"
    );
    let typed = ran(&mut term, "'/bin/sh' < \"$ATERM_TASK_FILE\"");
    let read = noted(&session, typed);
    assert!(awaits_its_end(&read));
    printed(&mut term, &["building", "all 12 passed"]);
    assert!(started(&term, &read), "its line ran");
    assert_eq!(
        ended(&term, typed, || false),
        None,
        "CONTROL: it still runs"
    );
    exited(&mut term, 0);
    let end = ended(&term, typed, || false).expect("it ended");
    drop(term);
    assert_eq!(
        end,
        ProgramEnd {
            exit: Some(0),
            output: "building\nall 12 passed".to_string(),
        }
    );
    let sid = session.ctx.self_id.as_str().to_string();
    let concluded = conclude(&session.ctx, 7101, &sid, &read, &end, Duration::ZERO)
        .expect("unfenced")
        .expect("marked");
    assert_eq!(concluded.state, TaskState::Done);
    assert_eq!(concluded.why, Some(crate::task_record::WHY_EXIT_STATUS));
    assert_eq!(concluded.exit, Some(0));
    assert_eq!(concluded.program_block, None, "its end concluded it once");
    assert_eq!(
        concluded.conclusion(),
        Some(crate::task_record::Conclusion::Finished)
    );
    assert_eq!(
        crate::task_record::cmd_result(&session.ctx, 7101, ""),
        "OK 2 source=output bytes=22\nbuilding\nall 12 passed\n"
    );
    let row = crate::task_record::await_row(&sid, Some(&concluded), "generic", "-", "-", false);
    assert!(
        row.contains(" conclusion=finished why=exit-status exit=0"),
        "{row}"
    );
}

/// A NON-ZERO EXIT FAILS THE TASK, ITS WHY NAMING THE STATUS: `failed why=exit-status exit=3` on
/// the record and on an `await task` row, its output still the result. A signal reads as its shell
/// reports it (128 plus its number). NEGATIVE CONTROL: the same program exiting 0 finishes.
#[test]
fn a_program_that_exits_3_fails_its_task_with_a_why_naming_3() {
    for (status, state) in [
        (3, TaskState::Failed),
        (137, TaskState::Failed),
        (0, TaskState::Done),
    ] {
        let local = 7200 + u64::try_from(status).unwrap();
        let (session, _) = program_worker(local);
        let mut term = session.term.lock().unwrap();
        let typed = ran(&mut term, "'/bin/sh' < \"$ATERM_TASK_FILE\"");
        let read = noted(&session, typed);
        printed(&mut term, &["boom"]);
        exited(&mut term, status);
        let end = ended(&term, typed, || false).expect("it ended");
        drop(term);
        assert_eq!(end.exit, Some(status));
        let sid = session.ctx.self_id.as_str().to_string();
        let concluded = conclude(&session.ctx, local, &sid, &read, &end, Duration::ZERO)
            .expect("unfenced")
            .expect("marked");
        assert_eq!(concluded.state, state, "exit {status}");
        assert_eq!(concluded.why, Some(crate::task_record::WHY_EXIT_STATUS));
        assert_eq!(concluded.exit, Some(status));
        let row = crate::task_record::await_row(&sid, Some(&concluded), "generic", "-", "-", false);
        let conclusion = if state == TaskState::Done {
            "finished"
        } else {
            "failed"
        };
        assert!(
            row.contains(&format!(
                " conclusion={conclusion} why=exit-status exit={status}"
            )),
            "{row}"
        );
        assert_eq!(
            crate::task_record::cmd_result(&session.ctx, local, ""),
            "OK 1 source=output bytes=4\nboom\n"
        );
    }
    assert_eq!(
        crate::task_record::failed_words(TaskState::Failed, Some("exit-status")),
        "its program exited with a failure status"
    );
}

/// A PROGRAM THAT PRINTED NOTHING HAS NO RESULT, and one whose block the next prompt sealed with no
/// status read still ends, failed `why=no-exit-status`, whose words claim no status: nothing says
/// it succeeded, or how it failed. CONTROL: a status read is `why=exit-status`, with its words.
#[test]
fn a_silent_program_has_no_result_and_one_with_no_status_fails() {
    let (session, _) = program_worker(7301);
    let mut term = session.term.lock().unwrap();
    let typed = ran(&mut term, "'true' < \"$ATERM_TASK_FILE\"");
    let read = noted(&session, typed);
    term.process(b"\x1b]133;A\x07% ");
    let end = ended(&term, typed, || false).expect("its block was sealed");
    drop(term);
    assert_eq!(end.exit, None);
    assert_eq!(end.state(), TaskState::Failed);
    let sid = session.ctx.self_id.as_str().to_string();
    let concluded = conclude(&session.ctx, 7301, &sid, &read, &end, Duration::ZERO)
        .expect("unfenced")
        .expect("marked");
    assert_eq!(
        (concluded.state, concluded.why, concluded.exit),
        (TaskState::Failed, Some("no-exit-status"), None)
    );
    assert_eq!(
        crate::task_record::failed_words(concluded.state, concluded.why),
        "no exit status of its program was read"
    );
    assert_ne!(
        crate::task_record::failed_words(TaskState::Failed, Some("exit-status")),
        crate::task_record::failed_words(concluded.state, concluded.why),
        "CONTROL: a status read says so"
    );
    assert_eq!(
        crate::task_record::cmd_result(&session.ctx, 7301, ""),
        "ERR result unavailable why=empty\n"
    );
}

/// THE RESULT IS THE OUTPUT'S TAIL, BOUNDED AND REDACTED: the last [`TAIL_ROWS`] rows, under a line
/// that says earlier output was cut, with a fleet join code redacted as the survey redacts a
/// screen. NEGATIVE CONTROL: a short output is whole, with no such line.
#[test]
fn a_programs_result_is_its_last_rows_redacted() {
    let mut term = Terminal::new(24, 80);
    let typed = ran(&mut term, "'/bin/sh' < \"$ATERM_TASK_FILE\"");
    let lines: Vec<String> = (0..TAIL_ROWS + 50).map(|n| format!("line {n}")).collect();
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    printed(&mut term, &refs);
    printed(&mut term, &["join with code:atf1-abcd"]);
    exited(&mut term, 0);
    let output = ended(&term, typed, || false).expect("ended").output;
    let mut rows = output.lines();
    assert_eq!(rows.next(), Some(CUT), "{output}");
    assert_eq!(
        output.lines().last(),
        Some("join with code:atf1-[redacted]"),
        "redacted"
    );
    assert!(
        !output.lines().any(|row| row == "line 0"),
        "the head was cut"
    );
    assert!(
        output.contains(&format!("line {}\n", TAIL_ROWS + 49)),
        "the tail stands"
    );
    assert!(output.lines().count() <= usize::try_from(TAIL_ROWS).unwrap() + 1);

    let mut short = Terminal::new(24, 80);
    let typed = ran(&mut short, "'/bin/sh' < \"$ATERM_TASK_FILE\"");
    printed(&mut short, &["one", "two"]);
    exited(&mut short, 0);
    assert_eq!(
        ended(&short, typed, || false).expect("ended").output,
        "one\ntwo"
    );
}

/// THE BYTE CAP KEEPS WHOLE LINES FROM THE END. NEGATIVE CONTROL: text under the cap is kept as it is.
#[test]
fn the_byte_cap_keeps_the_newest_whole_lines() {
    let text = "aaaa\nbbbb\ncccc";
    assert_eq!(cut_to(text, 9, false), format!("{CUT}\nbbbb\ncccc"));
    assert_eq!(
        cut_to(text, 8, false),
        format!("{CUT}\ncccc"),
        "no part of a line"
    );
    assert_eq!(cut_to(text, 64, false), text);
    assert_eq!(cut_to(text, 64, true), format!("{CUT}\n{text}"));
    assert_eq!(
        cut_to("ééé", 3, false),
        format!("{CUT}\né"),
        "a character's start"
    );
}

/// AN AGENT WORKER'S CONCLUSION IS UNCHANGED: its record is no program worker's, so no exit status
/// in its tab concludes it, and its record stands as it was. CONTROL: a program worker's record of
/// the same tab content is concluded.
#[test]
fn an_agent_workers_task_is_never_concluded_by_its_tabs_exit_status() {
    let (session, read) = worker_of(7401, Program::Claude);
    let mut term = session.term.lock().unwrap();
    let typed = ran(
        &mut term,
        "'claude' -- \"$(command cat -- \"$ATERM_TASK_FILE\")\"",
    );
    assert!(
        !crate::task_record::note_program_block(&session.ctx, typed, Duration::ZERO),
        "an agent worker's record notes no program block"
    );
    assert!(
        !awaits_its_end(&record_of(&session)),
        "an agent worker's record"
    );
    exited(&mut term, 0);
    let end = ended(&term, typed, || false).expect("a block ended");
    drop(term);
    let sid = session.ctx.self_id.as_str().to_string();
    assert_eq!(
        conclude(&session.ctx, 7401, &sid, &read, &end, Duration::ZERO),
        Ok(None),
        "nothing marked"
    );
    assert_eq!(record_of(&session), read, "the record stands as it was");
    assert_eq!(
        crate::task_record::failed_words(TaskState::Exited, Some("exit")),
        "its agent exited before landing",
        "an agent's exit keeps its words"
    );

    let (control, _) = program_worker(7402);
    let read = noted(&control, typed);
    assert!(awaits_its_end(&read));
    let sid = control.ctx.self_id.as_str().to_string();
    assert!(
        conclude(&control.ctx, 7402, &sid, &read, &end, Duration::ZERO)
            .expect("unfenced")
            .is_some(),
        "CONTROL: a program worker's is"
    );
}

/// A RECORD THAT MOVED SINCE THE READ IS NOT MARKED AGAIN: a second conclusion on the same read (the
/// reaper's beside its launcher's) finds the rev moved and leaves the first one's mark.
#[test]
fn a_program_workers_end_is_marked_once() {
    let (session, _) = program_worker(7501);
    let read = noted(&session, 0);
    let end = ProgramEnd {
        exit: Some(0),
        output: "ok".to_string(),
    };
    let sid = session.ctx.self_id.as_str().to_string();
    let first = conclude(&session.ctx, 7501, &sid, &read, &end, Duration::ZERO)
        .expect("unfenced")
        .expect("marked");
    let failed = ProgramEnd {
        exit: Some(1),
        output: "no".to_string(),
    };
    assert_eq!(
        conclude(&session.ctx, 7501, &sid, &read, &failed, Duration::ZERO),
        Ok(None)
    );
    assert_eq!(record_of(&session), first, "the first mark stands");
}

/// A LOOK THAT IS NOT THE LAUNCHER'S concludes what the blocks hold from the one its launcher typed
/// into, and a restored worker with no job in front as ended with no status read, whatever its
/// tab's new shell ran. NEGATIVE CONTROLS: a running program, a restored worker with a job in
/// front, and one never restored whose blocks show no end, are left.
#[test]
fn an_unwatched_program_worker_concludes_on_what_its_tab_holds() {
    let restored = Unwatched {
        restored: true,
        job: false,
    };
    let (session, _) = program_worker(7551);
    let record = noted(&session, 0);
    let fresh = Terminal::new(24, 80);
    assert_eq!(
        unwatched_end(&fresh, &record, QUIET, || false),
        None,
        "CONTROL: not started"
    );
    let mut persons = Terminal::new(24, 80);
    ran(&mut persons, "ls");
    exited(&mut persons, 0);
    for tab in [&fresh, &persons] {
        assert_eq!(
            unwatched_end(tab, &record, restored, || false),
            Some(ProgramEnd {
                exit: None,
                output: String::new(),
            }),
            "its program ran in a shell that is gone"
        );
    }
    assert_eq!(
        unwatched_end(
            &fresh,
            &record,
            Unwatched {
                restored: true,
                job: true,
            },
            || false
        ),
        None,
        "CONTROL: something still runs in front"
    );
    let mut running = Terminal::new(24, 80);
    let typed = ran(&mut running, "'/bin/sh' < \"$ATERM_TASK_FILE\"");
    let (session, _) = program_worker(7552);
    let record = noted(&session, typed);
    assert_eq!(
        unwatched_end(&running, &record, QUIET, || false),
        None,
        "CONTROL: it runs"
    );
    exited(&mut running, 2);
    assert_eq!(
        unwatched_end(&running, &record, QUIET, || false).map(|end| end.exit),
        Some(Some(2))
    );
}

/// A LAUNCHER THAT NEVER TYPED LEAVES NOTHING TO CONCLUDE (the review's major finding): a worker
/// whose launcher stalled before its first prompt (`stalled why=startup`, or `why=shell` once a
/// person used its tab) names no block, so a command a person then runs in its tab neither starts
/// it nor ends it, and the stall stands. A command run after the program's, or before the block its
/// line was typed into, is never its end either. NEGATIVE CONTROLS: the same block, noted as typed
/// into, is its program's end; and read from an earlier block, the person's command would be taken.
#[test]
fn a_command_its_launcher_never_typed_concludes_nothing() {
    for why in ["startup", "shell"] {
        let (session, _) = program_worker(7601);
        let mut stall = TaskMark::new(TaskState::Stalled);
        stall.why = Some(why);
        mark_session_task(&session.ctx, 7601, &stall).expect("unfenced");
        let stalled = record_of(&session);
        let mut term = session.term.lock().unwrap();
        let ls = ran(&mut term, "ls");
        printed(&mut term, &["Cargo.toml"]);
        exited(&mut term, 0);
        assert!(
            !awaits_its_end(&stalled),
            "{why}: no line typed, nothing is its program's"
        );
        assert!(!started(&term, &stalled), "{why}: nothing started it");
        assert_eq!(
            unwatched_end(&term, &stalled, QUIET, || false),
            None,
            "{why}"
        );
        let typed = noted(&session, ls);
        assert!(
            awaits_its_end(&typed),
            "CONTROL ({why}): a typed block is its program's"
        );
        assert_eq!(
            unwatched_end(&term, &typed, QUIET, || false).map(|end| (end.exit, end.output)),
            Some((Some(0), "Cargo.toml".to_string())),
            "CONTROL ({why})"
        );
    }

    let mut term = Terminal::new(24, 80);
    let before = ran(&mut term, "ls");
    printed(&mut term, &["before"]);
    exited(&mut term, 0);
    let typed = then(&mut term, "'/bin/sh' < \"$ATERM_TASK_FILE\"");
    printed(&mut term, &["work"]);
    exited(&mut term, 3);
    then(&mut term, "make");
    printed(&mut term, &["after"]);
    exited(&mut term, 0);
    assert!(before < typed, "a shell numbers its blocks upward");
    assert_eq!(
        ended(&term, typed, || false).map(|end| (end.exit, end.output)),
        Some((Some(3), "work".to_string())),
        "its own block, neither the command before it nor the one after"
    );
    assert_eq!(
        ended(&term, before, || false).map(|end| end.exit),
        Some(Some(0)),
        "CONTROL: read from the earlier block, the person's command is taken"
    );
}

/// A PROGRAM'S END CONCLUDES ITS TASK ONCE (the review's turn finding): a `turn` into a concluded
/// program worker types into its tab but never reopens its task, and a hand that reopens it finds
/// no block left, so no look concludes it again from the end that concluded it. NEGATIVE CONTROL:
/// an agent worker's done task reopens at a turn.
#[test]
fn a_turn_never_reopens_a_program_workers_concluded_task() {
    use crate::turn_ledger::TurnBy;
    let (session, _) = program_worker(7701);
    let read = noted(&session, 0);
    let end = ProgramEnd {
        exit: Some(3),
        output: "boom".to_string(),
    };
    let sid = session.ctx.self_id.as_str().to_string();
    let failed = conclude(&session.ctx, 7701, &sid, &read, &end, Duration::ZERO)
        .expect("unfenced")
        .expect("marked");
    crate::task_record::reopen_at_turn(&session.ctx, 7701, &TurnBy::Ctl, Some(failed.entered));
    assert_eq!(record_of(&session), failed, "a turn reopens nothing");
    let mut again = TaskMark::new(TaskState::Working);
    again.why = Some("turn");
    mark_session_task(&session.ctx, 7701, &again).expect("unfenced");
    let reopened = record_of(&session);
    assert_eq!(reopened.state, TaskState::Working);
    assert!(
        !awaits_its_end(&reopened),
        "a hand's reopening finds no block to conclude it from again"
    );

    let (agent, _) = worker_of(7702, Program::Claude);
    let mut done = TaskMark::new(TaskState::Done);
    done.why = Some("landed");
    mark_session_task(&agent.ctx, 7702, &done).expect("unfenced");
    let entered = record_of(&agent).entered;
    crate::task_record::reopen_at_turn(&agent.ctx, 7702, &TurnBy::Ctl, Some(entered));
    assert_eq!(
        (record_of(&agent).state, record_of(&agent).why),
        (TaskState::Working, Some("turn")),
        "CONTROL: an agent worker's task reopens"
    );
}

/// NO AUTOPILOT WORD MOVES A PROGRAM WORKER'S TASK (the review's Off finding): an Off a manager or a
/// person sets on its tab fails nothing, so its program's exit status still concludes it, and
/// Cruise reopens no failed one. NEGATIVE CONTROL: an agent worker's same record fails at the Off,
/// and reopens at Cruise.
#[test]
fn no_autopilot_word_moves_a_program_workers_task() {
    use aterm_agent::supervise::Autopilot;
    let t0 = std::time::Instant::now();
    let by = crate::session_timeline::AutopilotSetBy {
        actor: aterm_slog::Actor::Ctl,
        surface: None,
        since_ms: 2_000,
    };
    const LATER: u64 = 60_000;
    let of = |state: TaskState, agent: Program| {
        let mut record = TaskMark::new(state)
            .apply(None, &Default::default(), t0, 1_000)
            .0;
        record.agent = Some(agent);
        record
    };
    for (word, state) in [
        (Autopilot::Off, TaskState::Working),
        (Autopilot::Cruise, TaskState::Failed),
    ] {
        let program = of(state, Program::Generic);
        assert_eq!(
            crate::task_record::autopilot_mark(&program, Some(word), Some(&by), LATER),
            None,
            "{word:?} over a {state:?} program worker"
        );
        assert_eq!(
            crate::task_record::off_settles_in(&program, Some(word), Some(&by), 2_500),
            None
        );
        let agent = of(state, Program::Claude);
        assert!(
            crate::task_record::autopilot_mark(&agent, Some(word), Some(&by), LATER).is_some(),
            "CONTROL: {word:?} moves an agent worker's"
        );
    }
}

/// A HARNESS MARKS NOTHING OF A PROGRAM WORKER'S TASK (the review's wrapper finding): a CLI the
/// program started (`program=env arg=claude`) is read by its harness as any tab's, but its harness's
/// `task mark exited` is refused and marks nothing, so only the program's exit status concludes it.
/// NEGATIVE CONTROLS: the same mark from the harness on an agent worker's task is taken, and a
/// manager's own mark on the program worker's is too.
#[test]
fn a_harness_marks_nothing_of_a_program_workers_task() {
    use crate::control::{Principal, Scope, ServingPrincipal};
    let (session, before) = program_worker(7801);
    let (agent, _) = worker_of(7802, Program::Claude);
    {
        let _harness = ServingPrincipal::enter(Principal::Harness);
        assert_eq!(
            crate::task_record::cmd_task(
                &session.ctx,
                7801,
                Scope::Owner,
                "mark exited why=exit exit=0"
            ),
            crate::task_record::PROGRAM_MARK_BY_HARNESS
        );
        assert_eq!(record_of(&session), before, "nothing marked");
        let taken =
            crate::task_record::cmd_task(&agent.ctx, 7802, Scope::Owner, "mark exited why=exit");
        assert!(taken.contains(" state=exited "), "CONTROL: {taken}");
    }
    let _manager = ServingPrincipal::enter(Principal::Instance);
    let hand = crate::task_record::cmd_task(&session.ctx, 7801, Scope::Owner, "mark done");
    assert!(hand.contains(" state=done "), "CONTROL: {hand}");
}

/// JOB CONTROL'S STOP IS NO END (the review's Ctrl-Z finding): a block a stop's status sealed
/// (128 plus `SIGTSTP`, as zsh and bash report one) is no end while the shell holds a stopped job,
/// nor while the `fg` that resumed it runs; once neither holds, how it ended was hidden, so it ends
/// with no status read. NEGATIVE CONTROL: the same status with no job stopped and nothing run after
/// it is the program's own exit.
#[cfg(unix)]
#[test]
fn a_stopped_program_has_not_ended() {
    let stop = 128 + libc::SIGTSTP;
    let mut term = Terminal::new(24, 80);
    let typed = ran(&mut term, "'/bin/sh' < \"$ATERM_TASK_FILE\"");
    printed(&mut term, &["half"]);
    exited(&mut term, stop);
    assert_eq!(
        ended(&term, typed, || true),
        None,
        "its shell holds it stopped"
    );
    assert_eq!(
        ended(&term, typed, || false).map(|end| end.exit),
        Some(Some(stop)),
        "CONTROL: no job stopped, nothing after: its own status"
    );
    then(&mut term, "fg");
    printed(&mut term, &["rest"]);
    assert_eq!(ended(&term, typed, || false), None, "resumed, it runs");
    exited(&mut term, 0);
    assert_eq!(
        ended(&term, typed, || false),
        Some(ProgramEnd {
            exit: None,
            output: "half".to_string(),
        }),
        "job control hid how it ended"
    );
    let mut plain = Terminal::new(24, 80);
    let typed = ran(&mut plain, "'/bin/sh' < \"$ATERM_TASK_FILE\"");
    exited(&mut plain, 3);
    assert_eq!(
        ended(&plain, typed, || panic!(
            "no stop's status, no look at the shell"
        ))
        .map(|end| end.exit),
        Some(Some(3))
    );
}

/// THE SHELL'S STOPPED JOB IS READ LIVE: a child of this process the kernel reports stopped is a
/// stopped job of its. NEGATIVE CONTROL: the same child running is not stopped.
#[cfg(unix)]
#[test]
fn a_stopped_child_is_a_stopped_job() {
    let copy = aterm_uds::deadline::stand_in(std::path::Path::new("/bin/sleep"));
    let mut child = aterm_uds::spawnfd::command(&copy)
        .arg(aterm_spec::wait::PARKED_SECS.to_string())
        .spawn()
        .expect("the stand-in runs");
    let pid = i32::try_from(child.id()).expect("a pid");
    let me = i32::try_from(std::process::id()).expect("a pid");
    assert!(
        children_of(me).is_some_and(|children| children.contains(&pid)),
        "a child of this process"
    );
    assert!(
        !aterm_uds::process::is_stopped(child.id()),
        "CONTROL: it runs"
    );
    // SAFETY: SIGSTOP to this test's own unreaped child.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGSTOP) }, 0);
    if let Err(report) = aterm_uds::deadline::await_stopped(pid, aterm_spec::wait::MUST_HAPPEN) {
        panic!("the child stops: {report:?}");
    }
    let held = holds_stopped_job(me);
    let _ = child.kill();
    let _ = child.wait();
    assert!(held, "its stopped child is a stopped job");
}

/// THE TYPED BLOCK CROSSES A SEAMLESS UPDATE: a carry row holds `block=`, read back as it was, and
/// a malformed one refuses the row. NEGATIVE CONTROL: a record with none carries none.
#[test]
fn a_program_workers_block_is_carried() {
    let (session, unnoted) = program_worker(7901);
    let now = std::time::Instant::now();
    assert!(!unnoted.carry_row(now).contains(" block="), "CONTROL");
    let typed = noted(&session, 42);
    let row = typed.carry_row(now);
    assert!(row.contains(" block=42"), "{row}");
    let back = TaskRecord::from_carry_row(&row, None, now, typed.since_unix_ms).expect("reads");
    assert_eq!(back.program_block, Some(42));
    assert!(awaits_its_end(&back));
    assert!(
        TaskRecord::from_carry_row(
            &row.replace(" block=42", " block=x"),
            None,
            now,
            typed.since_unix_ms
        )
        .is_none(),
        "a malformed block refuses the row"
    );
}

/// A PROGRAM IS NAMED BY ITS FILE NAME: a path's last part, a `PATH` word as given, never its
/// folder or a word beside it. NEGATIVE CONTROL: a program with no file name (`/`, `..`), or one
/// no file can have, has no name.
#[test]
fn a_program_is_named_by_its_file_name() {
    let named = |program: &str| {
        ProgramRun {
            program: program.to_string(),
            args: vec!["--quick".to_string()],
        }
        .name()
        .map(str::to_owned)
    };
    assert_eq!(named("/opt/lab/bench").as_deref(), Some("bench"));
    assert_eq!(named("bench").as_deref(), Some("bench"));
    assert_eq!(named("./tools/run.sh").as_deref(), Some("run.sh"));
    assert_eq!(named("/opt/my tools/run it").as_deref(), Some("run it"));
    assert_eq!(named("/"), None);
    assert_eq!(named("/opt/.."), None);
    assert_eq!(named(&"x".repeat(256)), None, "longer than any file name");
    assert_eq!(named(&"x".repeat(255)).map(|name| name.len()), Some(255));
}

/// THE PROGRAM'S NAME CROSSES A SEAMLESS UPDATE AND A RESTORE: a carry row holds `program=`,
/// percent-encoded, read back as it was. NEGATIVE CONTROLS: an agent's worker carries none, and a
/// `program=` that is no program's name (a path, empty) or sits on an agent's record is dropped
/// while the row reads; a program worker's row with none (an older build's) names it `the
/// program`.
#[test]
fn a_program_workers_name_is_carried() {
    let now = std::time::Instant::now();
    let (_, agents) = worker_of(7902, Program::Claude);
    assert!(!agents.carry_row(now).contains(" program="), "CONTROL");
    let (session, _) = program_worker(7903);
    session
        .ctx
        .timeline
        .lock()
        .unwrap()
        .set_task_program(Some("run it"));
    let named = record_of(&session);
    let row = named.carry_row(now);
    assert!(row.contains(" program=run%20it"), "{row}");
    let back = TaskRecord::from_carry_row(&row, None, now, named.since_unix_ms).expect("reads");
    assert_eq!(back.program.as_deref(), Some("run it"));
    assert_eq!(back.worker_name(), Some("run it"));
    for (malformed, why) in [
        (" program=%2Fopt%2Frun", "a path"),
        (" program=", "empty"),
        (" program=..", "no file name"),
    ] {
        let row = row.replace(" program=run%20it", malformed);
        let back = TaskRecord::from_carry_row(&row, None, now, named.since_unix_ms)
            .unwrap_or_else(|| panic!("{why}: the row reads"));
        assert_eq!(back.program, None, "{why}");
        assert_eq!(
            back.worker_name(),
            Some(Program::Generic.product()),
            "{why}"
        );
    }
    let older = row.replace(" program=run%20it", "");
    let back = TaskRecord::from_carry_row(&older, None, now, named.since_unix_ms).expect("reads");
    assert_eq!(
        (back.program.as_deref(), back.worker_name()),
        (None, Some(Program::Generic.product())),
        "a program worker's row an older build carried"
    );
    let agents = row.replace(" agent=generic", " agent=claude");
    assert!(agents.contains(" agent=claude"), "PRECONDITION: {agents}");
    let back = TaskRecord::from_carry_row(&agents, None, now, named.since_unix_ms).expect("reads");
    assert_eq!(back.program, None, "an agent's record names no program");
    assert_eq!(back.worker_name(), Some("Claude Code"));
}
