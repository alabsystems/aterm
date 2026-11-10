// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A LINE HELD IN CUSTODY THROUGH THE LOOP (ATERM-KI-1101-3, [`turn_end_loop::Custody`]): a line
//! the loop wrote and gave up on undrawn holds every writer until evidence settles it; one the
//! worker read and drew none of a minute on, at rest, is restarted in place once and written once
//! to the process after; where no restart can be made, and after a second loss, it is a doubling
//! wait that writes nothing. A held line raises nothing: journal records alone.

use super::super::super::turn_end_loop::{
    CUSTODY_CAP, CUSTODY_STEP, ECHO_HANG, NO_HOST, RELAUNCH_OFF, Settles, custody_step,
};
use super::*;
use crate::supervise::LineHeld;
use crate::supervise::journal::{
    LINE_LOST_AGAIN, LINE_READ_UNDRAWN, LINE_RESTART_STANDS, LINE_RESTART_UNMADE, LINE_SETTLED,
};

/// What the worker said at the turn end the line is written at.
const SAID: &str = "Stage 1 is in.";

/// What the process a restart in place resumed says at its turn end.
const RESUMED: &str = "Resumed: stage 2 is next.";

/// The continuation the owner's policy writes ([`owners_policy`]).
const LINE: &str = "keep going";

/// How long the worker's screen stands unmoved after the first write: past the give-up at the
/// echo's minute and the read line's minute after its first clear read.
const STANDS: Duration = Duration::from_secs(100);

/// What a [`lost_run`] left: `host` is the run's host, or one never asked where it ran with none.
struct Lost {
    t: Typed,
    noted: Vec<String>,
    host: Arc<AccountHost>,
}

/// A Claude Code at a turn end that reads the loop's continuation (`status` reads its input
/// clear) and draws none of it: its screen stands [`STANDS`] after the write, then moves through
/// `after` (each screen's composer as it reads). The restart the host is asked for answers
/// `answer`; `shape` shapes the mock and the policy last.
fn lost_run(
    tag: &str,
    after: Vec<Vec<String>>,
    answer: &'static str,
    shape: impl FnOnce(&mut Mock, &mut SupervisorConfig),
) -> Lost {
    let mut screens = vec![busy_screen(), ended(SAID)];
    screens.extend(after);
    lost_run_on(
        tag,
        screens,
        Some(AccountHost::scripted(&[], &[answer])),
        shape,
    )
}

/// [`lost_run`] over the whole script `screens` (the first point the line is written at), under
/// `host` (`None`: the loop runs with no host), whose restarts are stamped on the run's clock
/// ([`restart_after_clear`]).
fn lost_run_on(
    tag: &str,
    screens: Vec<Vec<String>>,
    host: Option<Arc<AccountHost>>,
    shape: impl FnOnce(&mut Mock, &mut SupervisorConfig),
) -> Lost {
    lost_run_at(tag, screens, host, shape, |_| {})
}

/// [`lost_run_on`], `at` setting the session up as well (its wall clock, say).
fn lost_run_at(
    tag: &str,
    screens: Vec<Vec<String>>,
    host: Option<Arc<AccountHost>>,
    shape: impl FnOnce(&mut Mock, &mut SupervisorConfig),
    at: impl FnOnce(&mut Session<'_, TapedCtl<'_, Mock>>),
) -> Lost {
    let mut m = Mock::new(screens);
    m.gen_fence = true;
    m.sends_gen = true;
    m.human_seq = Some(0);
    m.help = FENCED_HELP.to_string();
    m.unmoved_after_send = Some(STANDS);
    m.vanish_after = Some(600);
    m.exit_when = Some(|m| {
        m.requests
            .iter()
            .any(|r| r.starts_with("key ") && r.ends_with(" enter"))
    });
    let mut policy = SupervisorConfig {
        human_grace_s: 0,
        answer_questions: false,
        ..owners_policy()
    };
    shape(&mut m, &mut policy);
    let (dir, journal) = journal_file(&format!("custody-{tag}"));
    let opts = SuperviseOpts {
        idle_host: host.clone().map(|h| h as Arc<dyn IdleHost>),
        journal: Some(journal.clone()),
        policy,
        ..auto(900)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(quick_turn_end(&[Duration::from_millis(10)]));
        if let Some(host) = &host {
            host.stamps_on(s);
        }
        at(s);
    });
    let noted: Vec<String> = journal_records(&journal)
        .0
        .into_iter()
        .map(|r| r.line)
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    Lost {
        t: Typed { m, lines },
        noted,
        host: host.unwrap_or_default(),
    }
}

/// The screens a restart in place resumes on: its turn runs, ends, and the resumed point's line
/// draws whole.
fn resumed() -> Vec<Vec<String>> {
    vec![busy_screen(), ended(RESUMED), typed_in(RESUMED, LINE)]
}

/// Whether the journal `noted` says the line held settled as `how` says.
fn settled(noted: &[String], how: Settles) -> bool {
    let said = format!("{LINE_SETTLED} {}: ", how.words());
    noted
        .iter()
        .any(|l| l.starts_with("NOTE ") && l.contains(&said))
}

/// The `until=` of each `WAITING` record in `noted` that says `why`, as unix seconds, in order.
fn waits_until(noted: &[String], why: &str) -> Vec<i64> {
    noted
        .iter()
        .filter(|l| l.starts_with("WAITING ") && l.contains(why))
        .filter_map(|l| {
            l.split_whitespace()
                .find_map(|w| w.strip_prefix("until="))
                .and_then(aterm_types::rfc3339::parse_utc)
        })
        .collect()
}

/// How long after its first clear read each `restarting it in place` record in `noted` says the
/// line stood undrawn, in seconds, in order.
fn restarting(noted: &[String]) -> Vec<u64> {
    noted
        .iter()
        .filter(|l| l.starts_with("NOTE ") && l.ends_with("restarting it in place"))
        .filter_map(|l| {
            let (_, on) = l.split_once(LINE_READ_UNDRAWN)?;
            on.split_whitespace().next()?.parse().ok()
        })
        .collect()
}

/// How long after the first `status` read that found the input clear since the write, on the
/// run's own clock, each restart the host was asked for came: the mock's record against the
/// host's, never the loop's own word for how long the line stood.
fn restart_after_clear(run: &Lost) -> Vec<Duration> {
    let Some(sent) = run.t.m.first_send_at else {
        return Vec::new();
    };
    let Some(clear) = run.t.m.clear_reads.iter().find(|at| **at >= sent) else {
        return Vec::new();
    };
    run.host
        .restarted_at()
        .iter()
        .map(|at| at.saturating_duration_since(*clear))
        .collect()
}

/// The sends of the continuation.
fn lines_sent(t: &Typed) -> usize {
    t.sends()
        .iter()
        .filter(|r| r.ends_with(&format!(" -- {LINE}")))
        .count()
}

/// A LINE THE WORKER READ AND NEVER DREW IS RESTARTED ONCE AND WRITTEN ONCE TO THE PROCESS AFTER
/// (ATERM-KI-1101-3, the rv1 run): `status` reads the continuation clear 10 s after the write, so
/// the worker read it; its screen stands, so it drew none of it; at the echo's minute it is held,
/// raising nothing; a minute after the first clear read, at rest, the host is asked for one
/// restart in place (`restart:input-lost`), [`ECHO_HANG`] or more after that read on the run's
/// clock ([`restart_after_clear`]), its one record saying so, the line settles with its program,
/// and the resumed point writes it once, entered when it draws. Negative controls by mutation:
/// the restart dropped from [`Session::custody_lost`] (a doubling wait in its place): no restart
/// asked; the custody gate dropped from [`Session::turn_end_write`]: the line written again on
/// the unmoved screen.
#[test]
fn a_line_the_worker_read_and_never_drew_restarts_it_once_and_is_written_once() {
    let run = lost_run("lost", resumed(), "relaunched", |m, _| {
        m.screen_cols.insert(4, 2 + LINE.len());
        m.cell_attrs_on.insert(4, "none");
    });
    let (t, noted) = (&run.t, &run.noted);
    assert_eq!(
        run.host.said("restart:"),
        ["restart:input-lost"],
        "one restart in place: {noted:#?}\n{}",
        t.evidence()
    );
    let read_s = restarting(noted);
    assert_eq!(read_s.len(), 1, "its reason journaled once: {noted:#?}");
    let after = restart_after_clear(&run);
    assert!(
        after.len() == 1 && after[0] >= ECHO_HANG && read_s[0] >= ECHO_HANG.as_secs(),
        "asked a minute after the first clear read: {after:?} {read_s:?}\n{noted:#?}"
    );
    assert!(
        settled(noted, Settles::Ended),
        "the line settled with its program: {noted:#?}"
    );
    assert_eq!(
        lines_sent(t),
        2,
        "once to the process that lost it, once to the one after: {}",
        t.evidence()
    );
    assert_eq!(t.enters().len(), 1, "entered once: {}", t.evidence());
    assert!(
        t.enters()[0].contains(&format!(
            "if={}",
            super::super::super::super::policy::row_guard(&format!("❯ {LINE}"))
        )),
        "fenced on the read that showed it whole: {}",
        t.evidence()
    );
    assert!(
        t.m.attention.is_none() && !noted.iter().any(|l| l.starts_with("ESCALATED ")),
        "no badge, no ask: {noted:#?}"
    );
    let formed = held_formed(noted).expect("held at the echo's minute");
    assert!(formed.ends_with("(input=clear)"), "read, undrawn: {formed}");
    let sent = t.m.first_send_at.expect("the write");
    let enter = t.enter_at().expect("its Enter");
    assert!(
        enter.saturating_duration_since(sent) >= STANDS,
        "entered on the resumed point, once the screen moved: {}",
        t.evidence()
    );
}

/// What the worker asks at the turn end a question point's line is written at.
const ASKED: &str = "Stage 1 is in. Shall I start stage 2?";

/// A LINE LOST AT A QUESTION POINT IS RESTARTED AS AT AN IDLE ONE (ATERM-KI-1101-3,
/// [`Session::custody_at_rest`]): the worker ends its turn on a question, the loop answers it with
/// the continuation, and `status` reads it clear while the screen shows none of it. A question is a
/// turn's end, no turn running and no box, as an idle point is: a minute after the first clear read
/// the host is asked for one restart in place, the line settles with its program, and the resumed
/// point writes it once, entered when it draws. Before, the rest asked for `Phase::Idle` alone: at
/// every due the gate held the line `not at rest`, no restart was ever asked and the host was told
/// for good that the line stands. Negative control by mutation: rest read from `Phase::Idle` alone
/// again in [`Session::custody_at_rest`]: no restart asked.
#[test]
fn a_line_lost_at_a_question_point_is_restarted_as_at_an_idle_one() {
    let mut screens = vec![busy_screen(), ended(ASKED)];
    screens.extend(resumed());
    let run = lost_run_on(
        "question",
        screens,
        Some(AccountHost::scripted(&[], &["relaunched"])),
        |m, policy| {
            policy.answer_questions = true;
            m.screen_cols.insert(4, 2 + LINE.len());
            m.cell_attrs_on.insert(4, "none");
        },
    );
    let (t, noted) = (&run.t, &run.noted);
    assert_eq!(
        run.host.said("restart:"),
        ["restart:input-lost"],
        "one restart in place: {noted:#?}\n{}",
        t.evidence()
    );
    let after = restart_after_clear(&run);
    assert!(
        after.len() == 1 && after[0] >= ECHO_HANG && restarting(noted).len() == 1,
        "asked once, a minute after the first clear read: {after:?}\n{noted:#?}"
    );
    assert!(
        !noted.iter().any(|l| l.contains("not at rest")),
        "a question is a rest: {noted:#?}"
    );
    assert!(
        settled(noted, Settles::Ended),
        "the line settled with its program: {noted:#?}"
    );
    assert_eq!(
        (lines_sent(t), t.enters().len()),
        (2, 1),
        "once to each process, entered once: {}",
        t.evidence()
    );
    assert!(t.m.attention.is_none(), "no badge: {}", t.evidence());
}

/// A LINE LOST AT A LIMIT'S POINT AFTER ITS RESET IS RESTARTED AS AT AN IDLE ONE (ATERM-KI-1101-3,
/// [`Session::custody_at_rest`]): the worker's turn ended on a weekly limit whose reset has passed,
/// the loop writes the limit's resume (`usage-resume@v1`), and `status` reads it clear while the
/// screen, still the limit's notice, shows none of it. A limit's notice is a turn's end, no turn
/// running and no box: a minute after the first clear read the host is asked for one restart in
/// place and the line settles with its program. Before, the rest asked for `Phase::Idle` alone: the
/// notice held the line `not at rest` at every due, no restart was ever asked. Negative control by
/// mutation: rest read from `Phase::Idle` alone again in [`Session::custody_at_rest`]: no restart
/// asked.
#[test]
fn a_line_lost_at_a_limits_point_after_its_reset_is_restarted_as_at_an_idle_one() {
    let mut screens = vec![busy_screen(), weekly_limited()];
    screens.extend(resumed());
    let run = lost_run_at(
        "limit",
        screens,
        Some(AccountHost::scripted(&[], &["relaunched"])),
        |_, policy| policy.resume_limits = true,
        |s| s.set_clock(AFTER_RESET, PDT),
    );
    let (t, noted) = (&run.t, &run.noted);
    assert!(
        t.sends()
            .first()
            .is_some_and(|r| r.ends_with(&format!(" -- {CLEARED}"))),
        "the limit's resume, written first: {}",
        t.evidence()
    );
    assert_eq!(
        run.host.said("restart:"),
        ["restart:input-lost"],
        "one restart in place: {noted:#?}\n{}",
        t.evidence()
    );
    let after = restart_after_clear(&run);
    assert!(
        after.len() == 1 && after[0] >= ECHO_HANG && restarting(noted).len() == 1,
        "asked once, a minute after the first clear read: {after:?}\n{noted:#?}"
    );
    assert!(
        !noted.iter().any(|l| l.contains("not at rest")),
        "a limit's notice is a rest: {noted:#?}"
    );
    assert!(
        settled(noted, Settles::Ended),
        "the line settled with its program: {noted:#?}"
    );
    assert!(t.m.attention.is_none(), "no badge: {}", t.evidence());
}

/// `screen` with its done row saying `work` still runs (` · 1 local agent still running`).
fn under_work(mut screen: Vec<String>, work: &str) -> Vec<String> {
    let done = screen
        .iter()
        .position(|row| row.starts_with("✻ ") && row.contains(" · done "))
        .expect("a done row");
    screen[done] = format!("{} · {work} still running", screen[done]);
    screen
}

/// A LINE LOST OVER THE AGENT'S OWN BACKGROUND WORK IS NEVER RESTARTED WHILE THAT WORK RUNS
/// (ATERM-KI-1101-3, [`Session::custody_at_rest`]): the worker's turn ends on a question with a
/// monitor it left running, or on a weekly limit whose reset has passed with a background agent
/// still running in its own process; the loop writes the point's line, `status` reads it clear and
/// the screen shows none of it. The phase reads a question or a limit's notice there (the notice
/// and the question are read before the soft busy of the work), but a restart in place would end
/// the work, which no process row names for an agent of its own process: the line is held `not at
/// rest` at every due while the work runs (each hold the journal records after the one that formed
/// the custody says so; a record is written at each change of what holds the line), the host is
/// asked for no restart, and the next turn settles the line (`went with a turn`, and by nothing
/// else). A monitor the done row names at that limit, under the progress row of a workflow that
/// ended (which Claude Code leaves drawn at its full bar, ahead of the monitor in the screen
/// reader's first rule), is held the same. Before, the rest took any question or limit's phase as
/// rest: the host was asked `restart:input-lost` over the running work; and the monitor under that
/// progress row read as no work, the restart asked over it. Negative controls by mutation: the
/// background work dropped from [`Session::custody_at_rest`]: a restart asked in each case; the
/// turn's settle journaled as the program's end in [`Session::custody_turned`]: the settle wrong
/// in each case.
#[test]
fn a_line_lost_over_the_agents_own_background_work_is_never_restarted_while_it_runs() {
    let mut wrong = Vec::new();
    for (tag, point, limit) in [
        (
            "question-monitor",
            under_work(ended(ASKED), "1 monitor"),
            false,
        ),
        (
            "limit-monitor-outlived",
            under_outlived_progress(under_work(weekly_limited(), "1 monitor")),
            true,
        ),
        (
            "limit-agent",
            under_work(weekly_limited(), "1 local agent"),
            true,
        ),
    ] {
        let mut screens = vec![busy_screen(), point];
        screens.extend(resumed());
        let run = lost_run_at(
            &format!("work-{tag}"),
            screens,
            Some(AccountHost::scripted(&[], &["relaunched"])),
            |m, policy| {
                policy.answer_questions = !limit;
                policy.resume_limits = limit;
                m.screen_cols.insert(4, 2 + LINE.len());
                m.cell_attrs_on.insert(4, "none");
            },
            |s| {
                if limit {
                    s.set_clock(AFTER_RESET, PDT);
                }
            },
        );
        let noted = &run.noted;
        let held = held_formed(noted);
        let restarts = run.host.said("restart:");
        let holds: Vec<&String> = held_lines(noted)
            .into_iter()
            .filter(|l| !l.contains(" typed, not submitted ("))
            .collect();
        let ways = [
            Settles::Drawn,
            Settles::Person,
            Settles::Turn,
            Settles::Ended,
        ]
        .into_iter()
        .filter(|how| settled(noted, *how))
        .collect::<Vec<_>>();
        if !held.is_some_and(|f| f.ends_with("(input=clear)"))
            || !restarts.is_empty()
            || holds.is_empty()
            || !holds.iter().all(|l| l.contains("not at rest"))
            || ways != [Settles::Turn]
        {
            wrong.push(format!(
                "{tag}: held={held:?} restarts={restarts:?} holds={holds:#?} settled={ways:?}\n\
                 {noted:#?}\n{}",
                run.t.evidence()
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "no restart in place over the agent's own work: {wrong:#?}"
    );
}

/// `screen` with the progress row a dynamic workflow leaves drawn under the footer at its full
/// bar once it ended (Claude Code 2.1.304, `claude-2.1.304-agents-panel-workflow-done-3m20s-120col.txt`).
fn under_outlived_progress(mut screen: Vec<String>) -> Vec<String> {
    screen.push(String::new());
    screen.push("  ◯ stall-probe  ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰  2/2 · 3m20s · ↓ 12.9k tokens".to_string());
    screen
}

/// A LINE LOST UNDER A PROGRESS ROW ITS WORKFLOW OUTLIVED IS RESTARTED AS AT ANY REST
/// (ATERM-KI-1101-3, [`Session::custody_at_rest`]): the worker's turn ends on a weekly limit whose
/// reset has passed, its done row naming no work left running, and under the footer the progress
/// row of a dynamic workflow that ended, which Claude Code leaves drawn at its full bar
/// (`aterm_phase::phase::background_wait` still reads it as `a workflow running`); the loop
/// writes the point's line, `status` reads it clear and the screen shows none of it. No work runs
/// that a restart would end (a workflow still running is named by its status row's `Waiting for
/// …`, `aterm_phase::phase::in_process_work`), so the line is restarted in place once, as at an
/// idle point. Before, the rest read the progress row as background work: the line was held `not
/// at rest` at every due, for as long as the row stood drawn, and no restart was asked.
#[test]
fn a_line_lost_under_a_progress_row_its_workflow_outlived_is_restarted_in_place() {
    let mut screens = vec![busy_screen(), under_outlived_progress(weekly_limited())];
    screens.extend(resumed());
    let run = lost_run_at(
        "outlived-progress",
        screens,
        Some(AccountHost::scripted(&[], &["relaunched"])),
        |m, policy| {
            policy.answer_questions = false;
            policy.resume_limits = true;
            m.screen_cols.insert(4, 2 + LINE.len());
            m.cell_attrs_on.insert(4, "none");
        },
        |s| s.set_clock(AFTER_RESET, PDT),
    );
    let noted = &run.noted;
    assert!(
        held_formed(noted).is_some_and(|f| f.ends_with("(input=clear)")),
        "the line held, read: {noted:#?}\n{}",
        run.t.evidence()
    );
    assert_eq!(
        run.host.said("restart:"),
        ["restart:input-lost"],
        "restarted in place once: {noted:#?}\n{}",
        run.t.evidence()
    );
    assert!(
        !noted.iter().any(|l| l.contains("not at rest")),
        "never held not at rest: {noted:#?}"
    );
}

/// `screen` with its footer's pill counting `work` (`· 1 MCP task ·`) and nothing else.
fn counted_in_footer(mut screen: Vec<String>, work: &str) -> Vec<String> {
    let footer = screen.last_mut().expect("the footer");
    *footer = format!("{footer} · {work} · ↓ to manage");
    screen
}

/// A LINE LOST OVER WORK ONLY THE FOOTER'S PILL COUNTS IS NEVER RESTARTED WHILE IT RUNS
/// (ATERM-KI-1101-3, [`Session::custody_at_rest`], `aterm_phase::phase::work_running`): the
/// worker's turn ends on a question or on a weekly limit whose reset has passed, its done row
/// naming nothing left running, and the pill counts work of the agent's own under one of Claude
/// Code 2.1.286's labels (read from its source; no capture draws them: `1 MCP task`, `2
/// background tasks`, `◇ 1 cloud session`); the loop writes the point's line, `status` reads it
/// clear and the screen shows none of it. The
/// phase reads the question or the limit ahead of the pill's soft busy, but a restart in place
/// would end that work, so the line is held `not at rest` and the host is asked for no restart.
/// Before, the rest read the pill's count as work only for a shell or a monitor alone: the host
/// was asked `restart:input-lost` over each of these. The controls are the question's and the
/// limit's restarts with no work counted
/// (`a_line_lost_at_a_question_point_is_restarted_as_at_an_idle_one`,
/// `a_line_lost_at_a_limits_point_after_its_reset_is_restarted_as_at_an_idle_one`).
#[test]
fn a_line_lost_over_work_only_the_footers_pill_counts_is_never_restarted_while_it_runs() {
    let mut wrong = Vec::new();
    for (tag, point, limit) in [
        (
            "question-mcp",
            counted_in_footer(ended(ASKED), "1 MCP task"),
            false,
        ),
        (
            "question-tasks",
            counted_in_footer(ended(ASKED), "2 background tasks"),
            false,
        ),
        (
            "limit-cloud",
            counted_in_footer(weekly_limited(), "◇ 1 cloud session"),
            true,
        ),
    ] {
        let mut screens = vec![busy_screen(), point];
        screens.extend(resumed());
        let run = lost_run_at(
            &format!("pill-{tag}"),
            screens,
            Some(AccountHost::scripted(&[], &["relaunched"])),
            |m, policy| {
                policy.answer_questions = !limit;
                policy.resume_limits = limit;
                m.screen_cols.insert(4, 2 + LINE.len());
                m.cell_attrs_on.insert(4, "none");
            },
            |s| {
                if limit {
                    s.set_clock(AFTER_RESET, PDT);
                }
            },
        );
        let noted = &run.noted;
        let held = held_formed(noted);
        let restarts = run.host.said("restart:");
        if !held.is_some_and(|f| f.ends_with("(input=clear)"))
            || !restarts.is_empty()
            || !noted.iter().any(|l| l.contains("not at rest"))
        {
            wrong.push(format!(
                "{tag}: held={held:?} restarts={restarts:?}\n{noted:#?}\n{}",
                run.t.evidence()
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "no restart in place over the work the pill counts: {wrong:#?}"
    );
}

/// A host whose restarts answer `answers` while it names the agent's process 41, then each
/// process of `moves` after each restart asked (none: 41 for ever).
fn naming(answers: &[&'static str], moves: &[u32]) -> Arc<AccountHost> {
    let host = AccountHost::scripted(&[], answers);
    *host.pid.lock().unwrap() = Some(41);
    *host.pids_on_restart.lock().unwrap() = moves.iter().copied().collect();
    host
}

/// `host`, each restart's own report naming each process of `lefts` after it, in order
/// ([`IdleHost::restart_left`]; none: the report names none).
fn reporting(host: Arc<AccountHost>, lefts: &[u32]) -> Arc<AccountHost> {
    *host.left_on_restart.lock().unwrap() = lefts.iter().copied().collect();
    host
}

/// A RESTART ANSWERED MADE WHILE THE PROCESS THAT READ THE LINE STILL RUNS IS NO RESTART
/// (ATERM-KI-1101-3): the worker read the line and drew none of it a minute on, at rest, and the
/// host answers the restart in place with a success word (`adopted`, as a window's host answers
/// a relaunch in flight it only adopted, or `relaunched:…`) while the process the line was written
/// to is named after it, by the restart's own report or, where that names none, by the host. The
/// custody settles only on evidence, and nothing shows its program ended: the line stands held, is
/// never written again to the process that may still hold it, and the restart is no restart made
/// but stands asked, asked again on its back-off, the line's `restarting it in place` record
/// written once, and no `RESTARTED` record books it made. Once the screen shows the line it is
/// entered, as drawn. Negative control by mutation: the settle on the word restored in
/// [`Session::restart_agent`]: the line settles `ended with its program` on the first answer and
/// the restart is asked once; the deferral alone dropped there: the restart is booked made.
#[test]
fn a_restart_answered_made_while_the_lines_process_runs_is_no_restart() {
    let cases: [(&'static str, &[u32]); 3] = [
        ("adopted", &[]),
        ("relaunched:same", &[]),
        ("adopted", &[41]),
    ];
    for (word, lefts) in cases {
        let answer = format!("{word} reported {lefts:?}");
        let run = lost_run_on(
            &format!("stands-{}-{}", word.len(), lefts.len()),
            vec![busy_screen(), ended(SAID), typed_in(SAID, LINE)],
            Some(reporting(naming(&[word], &[]), lefts)),
            |m, _| {
                m.unmoved_after_send = Some(Duration::from_secs(200));
                m.screen_cols.insert(2, 2 + LINE.len());
                m.cell_attrs_on.insert(2, "none");
            },
        );
        let (t, noted) = (&run.t, &run.noted);
        assert!(
            run.host.said("restart:").len() >= 2,
            "{answer}: asked again on its back-off: {noted:#?}\n{}",
            t.evidence()
        );
        assert!(
            !settled(noted, Settles::Ended),
            "{answer}: no program ended: {noted:#?}"
        );
        assert!(
            !noted.iter().any(|l| l.starts_with("RESTARTED ")),
            "{answer}: no restart booked made: {noted:#?}"
        );
        assert_eq!(
            lines_sent(t),
            1,
            "{answer}: never written again to the process that read it: {}",
            t.evidence()
        );
        assert_eq!(
            restarting(noted).len(),
            1,
            "{answer}: its record once: {noted:#?}"
        );
        assert!(
            settled(noted, Settles::Drawn) && t.enters().len() == 1,
            "{answer}: drawn, entered once: {noted:#?}\n{}",
            t.evidence()
        );
        assert!(
            t.m.attention.is_none() && !noted.iter().any(|l| l.starts_with("ESCALATED ")),
            "{answer}: no badge, no ask: {noted:#?}"
        );
    }
}

/// A RESTART THAT ENDED NOTHING IS ASKED AGAIN, AND MADE ONCE THE HOST NAMES ANOTHER PROCESS: the
/// first answer `adopted` leaves the process the line was written to standing, so it is no
/// restart; the second moves the agent to another process, which ends the line's program: the
/// line settles `ended with its program` then, once, and the point the new process resumes on
/// writes it once to it, entered when it draws. Negative control by mutation: the settle on the
/// word restored in [`Session::restart_agent`]: settled on the first answer, the restart asked
/// once, and the line written again to the process that read it.
#[test]
fn a_restart_that_ended_nothing_is_asked_again_and_made_once_the_process_moves() {
    let mut screens = vec![busy_screen(), ended(SAID)];
    screens.extend(resumed());
    let run = lost_run_on(
        "moves",
        screens,
        Some(naming(&["adopted"], &[41, 42])),
        |m, _| {
            m.screen_cols.insert(4, 2 + LINE.len());
            m.cell_attrs_on.insert(4, "none");
        },
    );
    let (t, noted) = (&run.t, &run.noted);
    assert_eq!(
        run.host.said("restart:"),
        ["restart:input-lost"; 2],
        "asked again, then made: {noted:#?}\n{}",
        t.evidence()
    );
    assert_eq!(
        noted
            .iter()
            .filter(|l| l.starts_with("NOTE ") && l.contains(&format!("{LINE_SETTLED} ")))
            .count(),
        1,
        "settled once: {noted:#?}"
    );
    assert!(
        settled(noted, Settles::Ended),
        "with its program: {noted:#?}"
    );
    assert_eq!(
        noted.iter().filter(|l| l.starts_with("RESTARTED ")).count(),
        1,
        "booked made once, by the ask that moved the process: {noted:#?}"
    );
    assert_eq!(
        (lines_sent(t), t.enters().len()),
        (2, 1),
        "once to each process, entered once: {}",
        t.evidence()
    );
}

/// A RESTART ITS OWN REPORT NAMES MADE IS MADE, THOUGH THE HOST'S KEPT SNAPSHOT LAGS IT: the host
/// answers the restart in place `adopted`, its report naming the process it started (42), while
/// the host still names the one the line was written to (41), as a window's host does whose
/// snapshot read after the act outran its bound. The report is the restart's own evidence: the
/// line settles `ended with its program` once, the restart is booked made once (`RESTARTED`, no
/// `WAITING` that says nothing ended), it is never asked again, and the point after writes the
/// line once to the new process, entered when it draws. Negative control by mutation: the host's
/// naming taken before the report in `custody::Line::restarted` (the planted
/// `CustodyRestartBySnapshot`): nothing reads ended, the restart is asked again and never booked.
#[test]
fn a_restart_its_report_names_made_is_made_though_the_hosts_snapshot_lags() {
    let mut screens = vec![busy_screen(), ended(SAID)];
    screens.extend(resumed());
    let run = lost_run_on(
        "lags",
        screens,
        Some(reporting(naming(&["adopted"], &[]), &[42])),
        |m, _| {
            m.screen_cols.insert(4, 2 + LINE.len());
            m.cell_attrs_on.insert(4, "none");
        },
    );
    let (t, noted) = (&run.t, &run.noted);
    assert_eq!(
        run.host.said("restart:"),
        ["restart:input-lost"],
        "made at the first ask, never asked again: {noted:#?}\n{}",
        t.evidence()
    );
    assert_eq!(
        noted.iter().filter(|l| l.starts_with("RESTARTED ")).count(),
        1,
        "booked made once: {noted:#?}"
    );
    assert!(
        !noted.iter().any(|l| l.contains(LINE_RESTART_STANDS)),
        "never journaled as nothing ended: {noted:#?}"
    );
    assert!(
        settled(noted, Settles::Ended),
        "with its program: {noted:#?}"
    );
    assert_eq!(
        (lines_sent(t), t.enters().len()),
        (2, 1),
        "once to each process, entered once: {}",
        t.evidence()
    );
}

/// A RESTART MADE UNDER A DEFERRAL'S WORD SPENDS THE LINE'S RESTART ONCE THE LINE SETTLES: the
/// host answers the restart in place `wait:resume`, as a window's host does whose relaunch sent
/// its signal and typed its line but whose new process had not registered within the held wait,
/// and names another process (42) from then on. The restart stood asked; the line settles `ended
/// with its program` at the gate, which spends it, as the host may have made it; the line written
/// to the new process, read and drawn none of either, then waits ([`LINE_LOST_AGAIN`]) and writes
/// nothing more: one restart for the line short of an act submitted. Negative control by
/// mutation: the spend dropped from `custody::Restarts::settled` (the planted
/// `CustodySettleKeepsAsked`): the second loss asks a second restart in place.
#[test]
fn a_restart_made_under_a_deferrals_word_spends_the_lines_restart() {
    let run = lost_run_on(
        "made-deferred",
        vec![busy_screen(), ended(SAID), busy_screen(), ended(RESUMED)],
        Some(naming(&["wait:resume"], &[42])),
        |m, _| {
            // The process the restart started reads the line written to it and draws none of it.
            m.stands.insert(3, Duration::from_secs(600));
            m.exit_when =
                Some(|m| m.requests.iter().filter(|r| r.starts_with("send ")).count() > 2);
        },
    );
    let (t, noted) = (&run.t, &run.noted);
    assert_eq!(
        run.host.said("restart:"),
        ["restart:input-lost"],
        "one restart, never a second: {noted:#?}\n{}",
        t.evidence()
    );
    assert!(
        settled(noted, Settles::Ended),
        "the line went with its program: {noted:#?}"
    );
    assert_eq!(
        lines_sent(t),
        2,
        "once to each process, no more: {}",
        t.evidence()
    );
    assert!(
        noted
            .iter()
            .any(|l| l.starts_with("WAITING ") && l.contains(LINE_LOST_AGAIN)),
        "the second loss waits: {noted:#?}"
    );
    assert!(t.enters().is_empty(), "{}", t.evidence());
    assert!(t.m.attention.is_none(), "no badge: {}", t.evidence());
}

/// A RESTART FOR A LINE WRITTEN WHERE THE HOST NAMED NO PROCESS, ASKED AGAIN AFTER A DEFERRAL, IS
/// BOOKED ONCE ON THE WORD ITS RE-ASK CARRIES (ATERM-KI-1101-3): the host names no agent process
/// at the write, so the line's restart names none (`Restart::InputLost { of: (None, None), .. }`)
/// and the line cannot settle by its program's end. Its first ask answers `wait:resume`, as a
/// window's host does whose relaunch sent its signal and typed its line but whose new process (42,
/// named from then on) had not registered within the held wait; the loop asks again on the
/// back-off with the same line's ask (`Restart::InputLost`'s `line`, the restart standing
/// deferred), and there the host's relaunch carries the record that ask claimed
/// (`relaunch::its_own`, the record's `ask`) and answers `adopted`, ending nothing; a record
/// of its cause another line's ask claimed is another's, which it supersedes. Every ask of one
/// line's restart carries that line's ask, and none another's: the loop's next line asks with its
/// own. With no process named at the write, the word is all the evidence there is
/// (`custody::Line::restarted`): the line settles with its program once, the restart is booked
/// made once (one `RESTARTED` for the one relaunch), and the point after writes the line once to
/// the new process. The relaunch's half: `a_restart_asked_again_over_its_own_record_carries_it`.
#[test]
fn a_nameless_lines_restart_asked_again_after_a_deferral_is_booked_once_on_its_carried_word() {
    let host = AccountHost::scripted(&[], &["wait:resume", "adopted"]);
    *host.pids_on_restart.lock().unwrap() = [42].into_iter().collect();
    let mut screens = vec![busy_screen(), ended(SAID)];
    screens.extend(resumed());
    let run = lost_run_on("nameless", screens, Some(host), |m, _| {
        m.screen_cols.insert(4, 2 + LINE.len());
        m.cell_attrs_on.insert(4, "none");
    });
    let (t, noted) = (&run.t, &run.noted);
    assert_eq!(
        run.host.said("restart:"),
        ["restart:input-lost"; 2],
        "asked again after its deferral: {noted:#?}\n{}",
        t.evidence()
    );
    let asked = run.host.asked.lock().unwrap().clone();
    assert!(
        matches!(
            asked.as_slice(),
            [
                Restart::InputLost {
                    of: (None, None),
                    line: first,
                },
                Restart::InputLost {
                    of: (None, None),
                    line: again,
                },
            ] if first == again
        ),
        "first asked, then asked again with the same line's ask: {asked:?} {noted:#?}"
    );
    let asks = run.host.asks.lock().unwrap().clone();
    assert!(
        matches!(asks.as_slice(), [first, again] if first == again && first.word().starts_with("line:")),
        "the host is asked the line's restart as one ask, its line's: {asks:?}"
    );
    assert_eq!(
        noted.iter().filter(|l| l.starts_with("RESTARTED ")).count(),
        1,
        "booked made once, on the carried word: {noted:#?}"
    );
    assert_eq!(
        noted
            .iter()
            .filter(|l| l.starts_with("NOTE ") && l.contains(&format!("{LINE_SETTLED} ")))
            .count(),
        1,
        "settled once: {noted:#?}"
    );
    assert!(
        settled(noted, Settles::Ended),
        "with its program: {noted:#?}"
    );
    assert_eq!(restarting(noted).len(), 1, "its record once: {noted:#?}");
    assert_eq!(
        (lines_sent(t), t.enters().len()),
        (2, 1),
        "once to each process, entered once: {}",
        t.evidence()
    );
}

/// A LINE FIRST READ CLEAR AT A LOOK IS RESTARTED A MINUTE AFTER THAT READ, NOT AT IT: the worker
/// holds the continuation unread (`input=pending`) 90 s after the write, so it is held unread at
/// the echo's minute; the doubling wait's look that first reads it clear dates its read, and the
/// restart in place is asked a minute on ([`ECHO_HANG`]) on the run's clock
/// ([`restart_after_clear`]), never at that look. Negative controls by mutation: the gate's minute
/// dropped from `custody::Line::gate_read`: the restart asked at the read itself; the write's
/// held-text note's `input=pending` taken for a read in [`Session::custody_formed`] (the line
/// dated read 10 s after the write): the restart asked at the look that first reads it clear,
/// though its own record says it stood 80 s.
#[test]
fn a_line_first_read_clear_at_a_look_is_restarted_a_minute_after_that_read() {
    let run = lost_run("read-late", resumed(), "relaunched", |m, _| {
        m.unread_after_write = Some(Duration::from_secs(90));
        m.unread_first_send_only = true;
        m.unmoved_after_send = Some(Duration::from_secs(240));
        m.screen_cols.insert(4, 2 + LINE.len());
        m.cell_attrs_on.insert(4, "none");
    });
    let (t, noted) = (&run.t, &run.noted);
    let formed = held_formed(noted).expect("held at the echo's minute");
    assert!(formed.ends_with("(input=pending)"), "unread: {formed}");
    assert_eq!(
        run.host.said("restart:"),
        ["restart:input-lost"],
        "one restart in place: {noted:#?}\n{}",
        t.evidence()
    );
    let read_s = restarting(noted);
    let after = restart_after_clear(&run);
    assert!(
        read_s.len() == 1 && after.len() == 1 && after[0] >= ECHO_HANG,
        "asked a minute after its first clear read: {after:?} {read_s:?}\n{noted:#?}"
    );
    assert_eq!(
        (lines_sent(t), t.enters().len()),
        (2, 1),
        "once to each process, entered once: {}",
        t.evidence()
    );
}

/// WHERE NO RESTART CAN BE MADE, A LINE READ AND UNDRAWN IS A DOUBLING WAIT THAT WRITES NOTHING:
/// under `[harness] relaunch = false`, and where the loop runs with no host, the read line's minute
/// passes at rest and no restart is asked; the line is never written again on the unmoved screen
/// and raises nothing, its `WAITING` records name why (never the program, as though it were the
/// cause), each wait longer than the last (the gaps between their `until=` stamps grow, whether
/// the run's wall clock stands or runs). Once the screen moves to show the line, it is entered.
/// Negative controls by mutation: the relaunch-off arm dropped from `custody::Line::lose`:
/// `restart:input-lost` asked; its no-host arm dropped: the no-host wait blames a host's restart
/// lane (`the host makes none for claude`) and none says [`NO_HOST`]; its wait count unkept:
/// every wait a flat [`CUSTODY_STEP`].
#[test]
fn a_restart_that_cannot_be_made_is_a_doubling_wait_that_writes_nothing() {
    for (tag, host, relaunch, why) in [
        (
            "no-relaunch",
            Some(AccountHost::scripted(&[], &["relaunched"])),
            false,
            RELAUNCH_OFF,
        ),
        ("no-host", None, true, NO_HOST),
    ] {
        let screens = vec![busy_screen(), ended(SAID), typed_in(SAID, LINE)];
        let run = lost_run_on(tag, screens, host, |m, policy| {
            policy.relaunch = relaunch;
            m.unmoved_after_send = Some(Duration::from_secs(400));
            m.screen_cols.insert(2, 2 + LINE.len());
            m.cell_attrs_on.insert(2, "none");
        });
        let (t, noted) = (&run.t, &run.noted);
        assert!(
            run.host.said("restart:").is_empty(),
            "{tag}: no restart asked: {noted:#?}"
        );
        assert_eq!(lines_sent(t), 1, "{tag}: written once: {}", t.evidence());
        assert!(
            t.m.attention.is_none() && !noted.iter().any(|l| l.starts_with("ESCALATED ")),
            "{tag}: no badge, no ask: {noted:#?}"
        );
        assert!(
            !noted.iter().any(|l| l.starts_with("WAITING ")
                && l.contains(LINE_READ_UNDRAWN)
                && (l.contains("the host makes none for") || l.contains("claude"))),
            "{tag}: no wait blames the program or a host's restart lane: {noted:#?}"
        );
        let until = waits_until(noted, why);
        assert!(until.len() >= 3, "{tag}: a wait, and two more: {noted:#?}");
        let gaps: Vec<i64> = until.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(
            gaps.windows(2).all(|g| g[1] > g[0]),
            "{tag}: each wait longer than the last: {gaps:?}\n{noted:#?}"
        );
        assert_eq!(
            t.enters().len(),
            1,
            "{tag}: drawn, entered: {noted:#?}\n{}",
            t.evidence()
        );
    }
}

/// A HELD LINE'S WAIT DOUBLES FROM [`CUSTODY_STEP`] AND STOPS AT [`CUSTODY_CAP`]: a held line wakes
/// the loop a few times an hour, never three times a minute.
#[test]
fn a_held_lines_wait_doubles_to_its_cap() {
    assert_eq!(custody_step(0), CUSTODY_STEP);
    assert_eq!(custody_step(1), CUSTODY_STEP * 2);
    let steps: Vec<Duration> = (0..40).map(custody_step).collect();
    assert!(steps.windows(2).all(|w| w[1] >= w[0]), "{steps:?}");
    assert_eq!(
        steps.last().copied(),
        Some(CUSTODY_CAP),
        "capped: {steps:?}"
    );
    assert_eq!(custody_step(u32::MAX), CUSTODY_CAP, "capped");
}

/// A LOST LINE'S `restarting it in place` RECORD IS WRITTEN ONCE, WHATEVER HOLDS COME BETWEEN ITS
/// ASKS (ATERM-KI-1101-3, [`crate::supervise::custody::Line::lose`]'s `Loss::Restart { first }`,
/// on which [`Session::custody_lost`] writes it): the line read clear and drawn none of past its
/// minute, at rest, its restart asked and deferred, then held at a due by a read not at rest (a
/// person's key within the grace, a background call making no progress:
/// [`crate::supervise::custody::NOT_AT_REST`]'s record) or by `status` reading typeahead (the read
/// hold's record), and lost again at rest: the restart is asked again, its first record standing.
/// Before, `first` was read off the field the hold records keep their last words in, which each of
/// those records overwrote, so the next ask wrote the line's `restarting it in place` record again.
#[test]
fn a_lost_lines_restart_record_is_written_once_whatever_holds_come_between_its_asks() {
    use crate::supervise::custody::{GateRead, Lane, Line, Loss, NOT_AT_REST, Restarts, hold_said};
    let t0 = Instant::now();
    let mut line = Line::formed(LINE, 0, Some("clear"), None, Some((Some(41), None)), t0);
    let lane = Lane {
        relaunch: true,
        host: true,
        can_restart: true,
    };
    let mut restarts = Restarts::default();
    let at = t0 + ECHO_HANG;
    let first = |line: &mut Line, restarts: &Restarts| {
        assert_eq!(line.gate_read(Some("clear"), true, at), GateRead::Again);
        match line.lose(restarts, &lane, at) {
            Loss::Restart { first, .. } => first,
            other => panic!("a restart asked: {other:?}"),
        }
    };
    assert!(first(&mut line, &restarts), "the first ask's record");
    restarts.deferred();
    assert!(
        !first(&mut line, &restarts),
        "asked again: no second record"
    );
    for said in [NOT_AT_REST.to_string(), hold_said(line.held(), "typeahead")] {
        assert!(line.says(said.clone()), "{said}: the hold's own record");
        assert!(
            !first(&mut line, &restarts),
            "{said} between asks: no second record of the restart"
        );
    }
}

/// A LOST LINE'S FIRST WAIT IS ITS LOSS'S FIRST, HOWEVER MANY HOLDS CAME BEFORE IT
/// (ATERM-KI-1101-3, [`crate::supervise::custody::Line::lose`]'s `Loss::Wait { waits }`, which
/// [`Session::custody_lost`] journals as the first four, then every sixth): the line read clear and
/// drawn none of past its minute, held not at rest at four dues in a row (each re-armed, a wait of
/// the line's in a row), then at rest where no restart can be made (the host makes none for the
/// program). The loss's waits count from its first, whose `WAITING` record is the one that names
/// why no restart is made, while its step stays the next of the line's waits in a row, so the
/// holds before it start it higher up the doubling. Before, the loss's waits were the line's waits
/// in a row: after four holds its first wait was counted 4, and no record of the loss was written
/// until its third (counted 6), the host told the line lost meanwhile.
#[test]
fn a_lost_lines_first_wait_is_its_losss_first_however_many_holds_came_before_it() {
    use crate::supervise::custody::{
        CustodyGate, GateRead, Lane, Line, Loss, NOT_AT_REST, NoRestart, Rest, Restarts,
    };
    let t0 = Instant::now();
    let mut line = Line::formed(LINE, 0, Some("clear"), None, None, t0);
    let lane = Lane {
        relaunch: true,
        host: true,
        can_restart: false,
    };
    let mut now = t0 + ECHO_HANG;
    for _ in 0..4 {
        assert_eq!(line.gate_read(Some("clear"), true, now), GateRead::Again);
        assert_eq!(Rest::default().gate(), CustodyGate::Hold, "not at rest");
        line.says(NOT_AT_REST.to_string());
        now = line.rearm(now);
    }
    let mut waits = Vec::new();
    for _ in 0..2 {
        assert_eq!(line.gate_read(Some("clear"), true, now), GateRead::Again);
        match line.lose(&Restarts::default(), &lane, now) {
            Loss::Wait {
                why: NoRestart::NoLane,
                until,
                waits: nth,
            } => waits.push((nth, until.saturating_duration_since(now))),
            other => panic!("a wait for no restart lane: {other:?}"),
        }
    }
    assert_eq!(
        waits,
        [(0, custody_step(4)), (1, custody_step(5))],
        "the loss's own waits count from its first; its step goes on from the holds"
    );
}

/// A RESTART THE HOST REFUSES IS ASKED FOR ONCE, NOT AT EVERY DUE: the worker read the line and drew
/// none of it a minute on, at rest, and the host answers the restart in place `refused:…`, or a
/// `busy:` step other than `busy:another-sweep` (its state directory cannot hold the sweep's lock,
/// a refusal: `relaunch::outcome`'s `Outcome::Cannot`), never a deferral. A repeated failure
/// calls for a different approach: from there the line is a doubling wait that writes nothing
/// ([`LINE_RESTART_UNMADE`], the host's step named), and the host is never asked again. Once the
/// screen moves to show the line, it is entered. Negative control by mutation: the refusal unkept
/// in [`Session::restart_agent`]: `restart:input-lost` asked at each due.
#[test]
fn a_refused_restart_is_asked_once_and_the_line_waits() {
    for (tag, answer) in [
        ("refused", "refused:not-a-shell-job"),
        ("refused-busy", "busy:state-unwritable"),
    ] {
        let run = lost_run(tag, vec![typed_in(SAID, LINE)], answer, |m, _| {
            m.unmoved_after_send = Some(Duration::from_secs(400));
            m.screen_cols.insert(2, 2 + LINE.len());
            m.cell_attrs_on.insert(2, "none");
        });
        let (t, noted) = (&run.t, &run.noted);
        assert_eq!(
            run.host.said("restart:"),
            ["restart:input-lost"],
            "{answer}: asked once, never again: {noted:#?}\n{}",
            t.evidence()
        );
        assert_eq!(lines_sent(t), 1, "{answer}: written once: {}", t.evidence());
        assert!(
            t.m.attention.is_none() && !noted.iter().any(|l| l.starts_with("ESCALATED ")),
            "{answer}: no badge, no ask: {noted:#?}"
        );
        let unmade = format!("{LINE_RESTART_UNMADE} ({answer})");
        assert!(
            !waits_until(noted, &unmade).is_empty(),
            "{answer}: the refusal's wait names its step: {noted:#?}"
        );
        assert_eq!(
            t.enters().len(),
            1,
            "{answer}: drawn, entered: {noted:#?}\n{}",
            t.evidence()
        );
    }
}

/// A RESTART THE HOST DEFERS IS ASKED AGAIN ON ITS BACK-OFF, ITS LINE'S RECORD WRITTEN ONCE: the
/// worker read the line and drew none of it a minute on, at rest, and the host answers the
/// restart in place `wait:no-session-record` three times, then refuses it. A deferral is no
/// refusal, so each back-off due asks again; the line's `restarting it in place` record is
/// written once however often it is asked, and the asks count none of the line's own waits, so
/// once the refusal ends the asking, its doubling wait starts at [`CUSTODY_STEP`]. The line is
/// written once and entered once the screen moves to show it. Negative controls by mutation: the
/// record unkept in `custody::Line::lose`: one at every ask; the line's wait counted at every
/// ask: its waits after the refusal start three steps up the doubling.
#[test]
fn a_deferred_restart_is_asked_again_and_its_line_noted_once() {
    let deferred = "wait:no-session-record";
    let run = lost_run_on(
        "deferred",
        vec![busy_screen(), ended(SAID), typed_in(SAID, LINE)],
        Some(AccountHost::scripted(
            &[],
            &[deferred, deferred, deferred, "refused:not-a-shell-job"],
        )),
        |m, _| {
            m.unmoved_after_send = Some(Duration::from_secs(700));
            m.screen_cols.insert(2, 2 + LINE.len());
            m.cell_attrs_on.insert(2, "none");
        },
    );
    let (t, noted) = (&run.t, &run.noted);
    assert_eq!(
        run.host.said("restart:"),
        ["restart:input-lost"; 4],
        "asked at each deferral's due, then refused: {noted:#?}\n{}",
        t.evidence()
    );
    let asked = run.host.asked.lock().unwrap().clone();
    assert!(
        asked.windows(2).all(|w| w[0] == w[1]),
        "every ask of the one line's restart carries its ask: {asked:?}"
    );
    assert_eq!(
        restarting(noted).len(),
        1,
        "the line's record written once: {noted:#?}"
    );
    assert!(
        noted
            .iter()
            .any(|l| l.starts_with("WAITING ") && l.contains(deferred)),
        "the deferral's own wait: {noted:#?}"
    );
    let unmade = format!("{LINE_RESTART_UNMADE} (refused:not-a-shell-job)");
    let until = waits_until(noted, &unmade);
    assert!(
        until.len() >= 2 && until[1] - until[0] < i64::try_from(custody_step(2).as_secs()).unwrap(),
        "the refusal's wait starts at the first step: {until:?}\n{noted:#?}"
    );
    assert_eq!(lines_sent(t), 1, "written once: {}", t.evidence());
    assert!(
        t.m.attention.is_none() && !noted.iter().any(|l| l.starts_with("ESCALATED ")),
        "no badge, no ask: {noted:#?}"
    );
    assert_eq!(
        t.enters().len(),
        1,
        "drawn, entered: {noted:#?}\n{}",
        t.evidence()
    );
}

/// A SECOND LOSS OF THE SAME LINE WAITS RATHER THAN RESTARTING AGAIN: the process a restart in
/// place resumed reads the line written to it and draws none of it either; one restart was made,
/// so the second loss is a doubling wait ([`LINE_LOST_AGAIN`]) that writes nothing more. Negative
/// control by mutation: the loss count unkept ([`Session::restart_agent`]): a second
/// `restart:input-lost`.
#[test]
fn a_second_loss_of_the_same_line_waits_rather_than_restarting_again() {
    let run = lost_run(
        "lost-again",
        vec![busy_screen(), ended(RESUMED)],
        "relaunched",
        |m, _| {
            // The resumed process's screen stands too: it draws none of the line written to it.
            m.stands.insert(3, Duration::from_secs(600));
            m.exit_when =
                Some(|m| m.requests.iter().filter(|r| r.starts_with("send ")).count() > 2);
        },
    );
    let (t, noted) = (&run.t, &run.noted);
    assert_eq!(
        run.host.said("restart:"),
        ["restart:input-lost"],
        "one restart, never a second: {noted:#?}\n{}",
        t.evidence()
    );
    assert_eq!(
        lines_sent(t),
        2,
        "once to each process, no more: {}",
        t.evidence()
    );
    assert!(
        noted
            .iter()
            .any(|l| l.starts_with("WAITING ") && l.contains(LINE_LOST_AGAIN)),
        "the second loss waits: {noted:#?}"
    );
    assert!(t.enters().is_empty(), "{}", t.evidence());
    assert!(t.m.attention.is_none(), "no badge: {}", t.evidence());
}

/// A PERSON'S KEYS DRAWN AFTER A HELD LINE END ITS HOLD, AND THEIR DRAFT IS THEIRS: in order their
/// keys came after the line, so a composer that shows their text and none of the line under a key
/// count past the write's says the line went. The hold settles (`is under a person's keys`), no
/// restart is asked, the loop's line is never written again, and their draft is a person's: their
/// grace is waited on before the policy's draft step sends it. Negative control by mutation: the
/// person's arm dropped from `custody::Line::look` (their text read as the line's draw):
/// `is drawn`, and their draft entered at once as the loop's own line.
#[test]
fn a_persons_keys_drawn_after_a_held_line_end_its_hold() {
    const THEIRS: &str = "fix the flaky test first";
    let run = lost_run(
        "person",
        vec![typed_in(SAID, THEIRS)],
        "relaunched",
        |m, policy| {
            policy.human_grace_s = 120;
            m.unmoved_after_send = Some(Duration::from_secs(65));
            m.screen_cols.insert(2, 2 + THEIRS.len());
            m.cell_attrs_on.insert(2, "none");
            m.human_seq_on.insert(2, Some(1));
            m.stands.insert(2, Duration::from_secs(300));
        },
    );
    let (t, noted) = (&run.t, &run.noted);
    assert!(held_formed(noted).is_some(), "held: {noted:#?}");
    assert!(
        settled(noted, Settles::Person),
        "settled by the person's keys: {noted:#?}"
    );
    assert!(run.host.said("restart:").is_empty(), "{noted:#?}");
    assert_eq!(
        (lines_sent(t), t.sends().len()),
        (1, 1),
        "nothing written over their draft: {noted:#?}\n{}",
        t.evidence()
    );
    assert!(
        noted.iter().any(|l| l.starts_with("WAITING ")
            && l.contains(crate::supervise::policy::turn_end::PERSON_TYPING)),
        "their grace waited on: {noted:#?}"
    );
}

/// A TURN ENDS A HOLD ON A LINE READ, BUT NOT ON ONE STILL UNREAD: a turn runs 65 s after the
/// write. Read clear before it, the worker took the line in order, so the turn settles the hold
/// (`went with a turn`) and the point it ends on writes afresh, entered once. Still unread
/// (`input=pending` for 90 s), the line may yet draw: the hold outlasts the turn, the next point
/// writes nothing, and when the line draws it is entered once, never written twice. Negative
/// control by mutation: `custody::Line::turned` settling on any turn: the unread line settled
/// as `went with a turn`.
#[test]
fn a_turn_ends_a_read_hold_but_not_an_unread_one() {
    const OTHER: &str = "Ran the suite again; one failure left.";
    let read = lost_run(
        "turn-read",
        vec![busy_screen(), ended(OTHER), typed_in(OTHER, LINE)],
        "relaunched",
        |m, _| {
            m.unmoved_after_send = Some(Duration::from_secs(65));
            m.screen_cols.insert(4, 2 + LINE.len());
            m.cell_attrs_on.insert(4, "none");
        },
    );
    let (t, noted) = (&read.t, &read.noted);
    assert!(held_formed(noted).is_some(), "held: {noted:#?}");
    assert!(
        settled(noted, Settles::Turn),
        "read: the turn settled it: {noted:#?}"
    );
    assert_eq!(
        (lines_sent(t), t.enters().len()),
        (2, 1),
        "read: written afresh at the turn's end, entered once: {}",
        t.evidence()
    );
    assert!(read.host.said("restart:").is_empty(), "{noted:#?}");

    let unread = lost_run(
        "turn-unread",
        vec![busy_screen(), ended(OTHER), typed_in(OTHER, LINE)],
        "relaunched",
        |m, _| {
            m.unmoved_after_send = Some(Duration::from_secs(65));
            m.unread_after_write = Some(Duration::from_secs(90));
            m.stands.insert(3, Duration::from_secs(40));
            m.screen_cols.insert(4, 2 + LINE.len());
            m.cell_attrs_on.insert(4, "none");
        },
    );
    let (t, noted) = (&unread.t, &unread.noted);
    assert!(
        held_formed(noted).is_some_and(|l| l.ends_with("(input=pending)")),
        "held unread: {noted:#?}"
    );
    assert!(
        !settled(noted, Settles::Turn),
        "unread: the turn settles nothing: {noted:#?}"
    );
    assert!(
        settled(noted, Settles::Drawn),
        "unread: settled by its draw: {noted:#?}"
    );
    assert_eq!(
        (lines_sent(t), t.enters().len()),
        (1, 1),
        "unread: written once, entered once as it drew: {noted:#?}\n{}",
        t.evidence()
    );
    assert!(unread.host.said("restart:").is_empty(), "{noted:#?}");
}

/// A HELD LINE HOLDS THE OCCUPANCY DOORS' WRITES AS A DRAFT DOES ([`Session::write_guards`]): a
/// line at a background break would land behind the line the worker may still draw. Controls:
/// before the write, on the same screen, no draft guards the door; by mutation, the custody arm
/// dropped from the guards: no draft after the write either.
#[test]
fn a_held_line_guards_the_doors_as_a_draft() {
    let mut m = Mock::new(vec![
        busy_screen(),
        ended(SAID),
        typed_in(SAID, LINE),
        busy_screen(),
    ]);
    m.gen_fence = true;
    m.sends_gen = true;
    m.human_seq = Some(0);
    m.help = FENCED_HELP.to_string();
    m.unmoved_after_send = Some(Duration::from_secs(120));
    let host = AccountHost::scripted(&[], &[]);
    let cfg = SupervisorConfig {
        human_grace_s: 0,
        ..owners_policy()
    };
    let mut s = virtual_session(&mut m, None);
    let point = s.await_turn(Duration::from_secs(60)).expect("the turn end");
    let now = s.fx.mono();
    let before = s
        .write_guards(&point.screen, now, &cfg, host.as_ref())
        .expect("the guards read");
    let mut heard = Heard::default();
    let written = s
        .type_text_written(&point, LINE, RULE_CONTINUE, &mut heard)
        .expect("the act");
    assert_ne!(
        written,
        crate::supervise::run::turn_end_loop::Written::Submitted
    );
    assert!(s.custody.is_some(), "held: {:#?}", heard.noted);
    let now = s.fx.mono();
    let after = s
        .write_guards(&point.screen, now, &cfg, host.as_ref())
        .expect("the guards read");
    drop(s);
    assert!(
        before.as_ref().is_ok_and(|g| !g.draft),
        "no draft before the write: {before:?}"
    );
    assert!(
        after.as_ref().is_ok_and(|g| g.draft),
        "the held line guards the doors: {after:?}"
    );
}

/// A Claude Code break of its own background work (one shell still running), `typed` in its
/// composer (empty: none).
fn break_with(typed: &str) -> Vec<String> {
    let mut r = rows(&[
        "⏺ Waiting for the build.",
        "",
        "✻ Crunched for 9m 27s · done 11:07 AM · 1 shell still running",
        "",
    ]);
    r.extend(composer("  ⏵⏵ auto mode on · 1 shell · ← for agents"));
    if !typed.is_empty() {
        let caret = r.iter().position(|row| row == "❯").expect("caret row");
        r[caret] = format!("❯ {typed}");
    }
    r
}

/// A spell of Claude Code's own background work under `host`: at its first settled break
/// (`1 shell still running`) Cruise carries it, and the worker reads the carry and draws none of
/// it; the breaks stand past the echo's minute, the spell ends idle with no turn read running
/// since, and a restart in place resumes on a point whose line draws whole.
fn break_spell(tag: &str, host: Arc<AccountHost>) -> Lost {
    lost_run_on(
        tag,
        vec![
            break_with(""),
            break_with(""),
            ended(SAID),
            busy_screen(),
            ended(RESUMED),
            typed_in(RESUMED, LINE),
        ],
        Some(host),
        |m, _| {
            // The person's last key an hour ago: no person holds the break's carry.
            m.human_ms = Some(3_600_000);
            m.unmoved_after_send = Some(Duration::from_secs(65));
            m.stands.insert(0, Duration::from_secs(40));
            m.stands.insert(1, Duration::from_secs(40));
            m.stands.insert(2, Duration::from_secs(30));
            m.screen_cols.insert(5, 2 + LINE.len());
            m.cell_attrs_on.insert(5, "none");
        },
    )
}

/// A SPELL OF BREAK READS ALONE IS NO TURN AFTER A LINE HELD (ATERM-KI-1101-3): at a settled
/// break of Claude Code's own background work (`1 shell still running`) Cruise carries it; the
/// worker reads the carry (`status` clear) and draws none of it, so at the echo's minute it is
/// held. The breaks go on, the shell exits, and the worker goes idle with no turn read running
/// since the write (`custody::Line::ran` unset). That point settles nothing (never
/// `went with a turn`): its gate finds the line read and undrawn a minute on, at rest, and asks
/// the one restart in place (`restart:input-lost`), so nothing is written again to the process
/// that read it. Negative control by mutation: [`Session::custody_after_work`] settling without
/// `custody::Line::ran`: `went with a turn`, no restart asked, and the point's line written to the
/// process holding the carry.
#[test]
fn a_spell_of_break_reads_alone_settles_no_held_line() {
    let run = break_spell("break-spell", AccountHost::scripted(&[], &["relaunched"]));
    let (t, noted) = (&run.t, &run.noted);
    let formed = held_formed(noted).expect("the carry held at the echo's minute");
    assert!(
        formed.contains("Autopilot is Cruise: your turn ended while")
            && formed.ends_with("(input=clear)"),
        "the break's carry held, read: {formed}\n{noted:#?}"
    );
    assert!(
        !settled(noted, Settles::Turn),
        "no turn ran after it: {noted:#?}\n{}",
        t.evidence()
    );
    assert_eq!(
        run.host.said("restart:"),
        ["restart:input-lost"],
        "the point's gate restarts it in place: {noted:#?}\n{}",
        t.evidence()
    );
    assert!(
        settled(noted, Settles::Ended),
        "the line settled with its program: {noted:#?}"
    );
    assert_eq!(
        lines_sent(t),
        1,
        "the point's line written only to the resumed process: {}",
        t.evidence()
    );
}

/// A HELD LINE HOLDS WHAT THE HOST WOULD TYPE INTO THE AGENT ([`IdlePoint::line_held`]): the
/// window host's own lines (a carry-on, the ask to free disk space, the upgrade's step) would
/// queue behind the line the worker may still hold and join it, so every break and idle point
/// the host is offered while a line stands held says so, and none other does; the host is still
/// offered each, for its steps that type nothing. Through [`break_spell`] the breaks before the
/// carry is held are free and those after it held, and the restart in place goes as without the
/// host. At an idle point the host is offered free before the write, held once the line is, and
/// free again once another process stands in the tab: the line settles with its program before
/// the host is offered the point (`Session::custody_holds_host`), so the carry-on owed a new
/// process is never held by the old one's line. Negative controls by mutation: `line_held`
/// unset at a break ([`Session::break_point`]) or at an idle point (`Session::host_steps_here`):
/// no point of that kind says it; the settle dropped from `Session::custody_holds_host`: the new
/// process's point held.
#[test]
fn a_held_line_holds_what_the_host_would_type() {
    /// The points `host` was offered at `at` (`break` or `idle`), each held or not, in order.
    fn offered(host: &AccountHost, at: &str) -> Vec<bool> {
        host.points
            .lock()
            .unwrap()
            .iter()
            .filter(|(kind, _)| *kind == at)
            .map(|(_, p)| p.line_held.is_some())
            .collect()
    }
    let host = AccountHost::scripted(&[], &["relaunched"]);
    host.asks_points.store(true, Ordering::SeqCst);
    let run = break_spell("host-held-break", Arc::clone(&host));
    let breaks = offered(&host, "break");
    let flips = breaks.windows(2).filter(|w| w[0] != w[1]).count();
    assert!(
        breaks.first() == Some(&false) && breaks.last() == Some(&true) && flips == 1,
        "breaks free before the hold, held after it: {breaks:?}\n{:#?}",
        run.noted
    );
    assert_eq!(
        run.host.said("restart:"),
        ["restart:input-lost"],
        "{:#?}",
        run.noted
    );

    let mut m = Mock::new(vec![busy_screen(), ended(SAID), ended(SAID)]);
    m.gen_fence = true;
    m.sends_gen = true;
    m.human_seq = Some(0);
    m.help = FENCED_HELP.to_string();
    m.unmoved_after_send = Some(Duration::from_secs(120));
    // The cursor on the composer's caret: an idle point the host is offered.
    m.cursor_on_caret = true;
    let host = AccountHost::scripted(&[], &[]);
    host.asks_points.store(true, Ordering::SeqCst);
    *host.pid.lock().unwrap() = Some(41);
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        policy: SupervisorConfig {
            human_grace_s: 0,
            ..owners_policy()
        },
        ..auto(900)
    };
    let mut s = virtual_session(&mut m, None);
    s.stall_host = Some(Arc::clone(&host) as Arc<dyn IdleHost>);
    let point = s.await_turn(Duration::from_secs(60)).expect("the turn end");
    let mut heard = Heard::default();
    let offer = |s: &mut Session<'_, Mock>, heard: &mut Heard| {
        s.host_passed = None;
        s.host_steps_here(&point, &opts, &mut false, heard)
            .expect("the host's step");
    };
    offer(&mut s, &mut heard);
    s.type_text_written(&point, LINE, RULE_CONTINUE, &mut heard)
        .expect("the act");
    assert!(s.custody.is_some(), "held: {:#?}", heard.noted);
    offer(&mut s, &mut heard);
    *host.pid.lock().unwrap() = Some(42);
    offer(&mut s, &mut heard);
    drop(s);
    assert_eq!(
        offered(&host, "idle"),
        [false, true, false],
        "free before the write, held after it, free once another process stands: {:#?}",
        heard.noted
    );
    assert!(
        settled(&heard.noted, Settles::Ended),
        "settled with its program: {:#?}",
        heard.noted
    );
}

/// A HELD LINE DRAWN IN A BUSY SPELL IS HANDED BACK TO ITS WRITER AT THE POINT THE SPELL ENDS ON
/// (ATERM-KI-1101-3): the worker read the continuation and drew none of it, so it is held; a turn
/// of its own then runs, and the line draws, at the point that turn ends on or at a break of the
/// spell before it. The point looks before anything settles the line: `is drawn`, never `went with
/// a turn`, and the line is the loop's own again, entered as the act that wrote it, never waited
/// on as a person's draft or written twice. Negative controls by mutation: the turn settled before
/// the look ([`Session::custody_after_work`]): `went with a turn`, and the drawn line a person's
/// draft; the break's draw handed back at the break ([`Session::custody_at_break`]): the point
/// drops it, the same.
#[test]
fn a_held_line_drawn_in_a_busy_spell_is_handed_back_to_its_writer() {
    const OTHER: &str = "Ran the suite again; one failure left.";
    for (tag, spell) in [
        ("drawn-at-point", vec![busy_screen(), typed_in(OTHER, LINE)]),
        (
            "drawn-at-break",
            vec![busy_screen(), break_with(LINE), typed_in(OTHER, LINE)],
        ),
    ] {
        // The screens after the turn's busy read show the line: the break's, if any, and the
        // point's.
        let shown = 3..spell.len() + 2;
        let run = lost_run(tag, spell, "relaunched", |m, policy| {
            policy.human_grace_s = 120;
            m.unmoved_after_send = Some(Duration::from_secs(65));
            for at in shown {
                m.screen_cols.insert(at, 2 + LINE.len());
                m.cell_attrs_on.insert(at, "none");
            }
        });
        let (t, noted) = (&run.t, &run.noted);
        assert!(held_formed(noted).is_some(), "{tag}: held: {noted:#?}");
        assert!(
            settled(noted, Settles::Drawn) && !settled(noted, Settles::Turn),
            "{tag}: drawn, not gone with the turn: {noted:#?}\n{}",
            t.evidence()
        );
        assert!(
            !noted.iter().any(|l| l.starts_with("WAITING ")
                && l.contains(crate::supervise::policy::turn_end::PERSON_TYPING)),
            "{tag}: the loop's own line, no person's draft: {noted:#?}"
        );
        assert_eq!(
            (lines_sent(t), t.enters().len()),
            (1, 1),
            "{tag}: written once, entered once: {noted:#?}\n{}",
            t.evidence()
        );
        assert!(run.host.said("restart:").is_empty(), "{tag}: {noted:#?}");
    }
}

/// A Claude Code permission box after `said`, drawn where the composer was, as Claude Code draws
/// one: no composer shows under it, so no read can say what the composer holds while it stands.
fn boxed(said: &str) -> Vec<String> {
    rows(&[
        &format!("⏺ {said}"),
        "",
        "⏺ Bash(git log --oneline -5)",
        "",
        &"─".repeat(120),
        " Bash command",
        "",
        "   git log --oneline -5",
        "   Show the five most recent commits",
        "",
        " Do you want to proceed?",
        " ❯ 1. Yes",
        "   2. Yes, and don’t ask again for: git log *",
        "   3. No",
        "",
        " Esc to cancel · Tab to amend",
    ])
}

/// A HELD LINE DRAWN UNDER A READ THAT CANNOT SAY IS HANDED BACK TO ITS WRITER ONCE A READ CAN
/// (ATERM-KI-1101-3): the worker read the continuation and drew none of it, so it is held; a turn
/// of its own then runs and the line draws in its composer, but the read the busy spell ends on
/// cannot say what the composer holds: a permission box stands over it at the point, or, at a
/// break of the spell, the composer's glyph cell is unread. Such a read is no evidence for the
/// turn's settle either: the line stands held, the turn's evidence kept, and the box is answered
/// as any box is, the custody holding none of it (handed to the person, or, with the person's
/// quiet read stamped, pressed by the loop: its digit goes while the line stands held). Once a
/// read can say, the composer shows the line, which is handed back: `is drawn`, never `went with
/// a turn`, entered once as the act that wrote it, never waited on as a person's draft or written
/// twice. Negative control by mutation: `custody::Line::turned` taking a read that cannot say for
/// one that shows none of the line (the planted `CustodyTurnUnreadable`, armed always): `went
/// with a turn` at the box or the break, and the drawn line waited on as a person's draft, never
/// entered.
#[test]
fn a_held_line_drawn_under_a_read_that_cannot_say_is_handed_back_once_one_can() {
    const OTHER: &str = "Ran the suite again; one failure left.";
    for (tag, spell, unread, quiet) in [
        (
            "boxed-at-point",
            vec![busy_screen(), boxed(OTHER), typed_in(OTHER, LINE)],
            None,
            false,
        ),
        (
            "boxed-answered-at-point",
            vec![busy_screen(), boxed(OTHER), typed_in(OTHER, LINE)],
            None,
            true,
        ),
        (
            "unread-at-break",
            vec![busy_screen(), break_with(LINE), typed_in(OTHER, LINE)],
            Some(3),
            false,
        ),
    ] {
        let run = lost_run(tag, spell, "relaunched", |m, policy| {
            policy.human_grace_s = 120;
            m.unmoved_after_send = Some(Duration::from_secs(65));
            for at in [3, 4] {
                m.screen_cols.insert(at, 2 + LINE.len());
                m.cell_attrs_on.insert(at, "none");
            }
            m.cell_unread_on.extend(unread);
            if quiet {
                // Every read stamped quiet: the box's digit is the loop's to press.
                m.human = vec!["null"];
            }
        });
        let (t, noted) = (&run.t, &run.noted);
        let pressed =
            t.m.requests
                .iter()
                .any(|r| r.starts_with("key ") && r.contains("git\\x20log") && r.ends_with(" 1"));
        assert_eq!(
            pressed,
            quiet,
            "{tag}: the box answered by the loop where the read is stamped quiet: {}",
            t.evidence()
        );
        assert!(held_formed(noted).is_some(), "{tag}: held: {noted:#?}");
        assert!(
            settled(noted, Settles::Drawn) && !settled(noted, Settles::Turn),
            "{tag}: drawn, not gone with the turn: {noted:#?}\n{}",
            t.evidence()
        );
        assert!(
            !noted.iter().any(|l| l.starts_with("WAITING ")
                && l.contains(crate::supervise::policy::turn_end::PERSON_TYPING)),
            "{tag}: the loop's own line, no person's draft: {noted:#?}"
        );
        assert_eq!(
            (lines_sent(t), t.enters().len()),
            (1, 1),
            "{tag}: written once, entered once: {noted:#?}\n{}",
            t.evidence()
        );
        assert!(run.host.said("restart:").is_empty(), "{tag}: {noted:#?}");
    }
}

/// A TURN'S EVIDENCE KEPT UNDER A BOX SETTLES A DROPPED LINE AT THE NEXT READ THAT CAN SAY
/// (ATERM-KI-1101-3): the worker read the continuation and drew none of it, so it is held; a turn
/// of its own runs and ends on a permission box over the composer, which keeps the turn's evidence
/// as it stands; the box goes with no turn read running after it, and the point it leaves shows the
/// composer empty. That point's look weighs the kept turn on its read and one `status` read:
/// `went with a turn`, so the point writes afresh, entered once, and no restart in place is asked
/// for a line a turn ran after. Negative control by mutation: the kept turn dropped from
/// `Session::custody_look_on`: the line read and undrawn a minute on is restarted for
/// (`restart:input-lost`).
#[test]
fn a_turn_kept_under_a_box_settles_a_dropped_line_once_a_read_can_say() {
    const OTHER: &str = "Ran the suite again; one failure left.";
    let run = lost_run(
        "boxed-then-dropped",
        vec![
            busy_screen(),
            boxed(OTHER),
            ended(OTHER),
            typed_in(OTHER, LINE),
        ],
        "relaunched",
        |m, _| {
            m.unmoved_after_send = Some(Duration::from_secs(65));
            // The empty point stands past the read line's minute, so a gate that lost the turn
            // reads it again at rest.
            m.stands.insert(4, Duration::from_secs(30));
            m.screen_cols.insert(5, 2 + LINE.len());
            m.cell_attrs_on.insert(5, "none");
        },
    );
    let (t, noted) = (&run.t, &run.noted);
    assert!(held_formed(noted).is_some(), "held: {noted:#?}");
    assert!(
        settled(noted, Settles::Turn),
        "the kept turn settled it: {noted:#?}\n{}",
        t.evidence()
    );
    assert!(
        run.host.said("restart:").is_empty(),
        "no restart for a line a turn ran after: {noted:#?}"
    );
    assert_eq!(
        (lines_sent(t), t.enters().len()),
        (2, 1),
        "written afresh at the point, entered once: {noted:#?}\n{}",
        t.evidence()
    );
}

/// What a line held after a turn read running comes to on one read, `rows` with the cursor past
/// `typed`, taken at the point a busy spell ends on (`Session::custody_after_work`) or at a break
/// of its own background work (`Session::host_steps_in_background`), every `cell` read failing
/// where `unread`: whether it stands held, and the journal.
fn turn_read_on(
    rows: Vec<String>,
    typed: &str,
    unread: bool,
    at_break: bool,
) -> (bool, Vec<String>) {
    let mut m = Mock::new(vec![
        busy_screen(),
        ended(SAID),
        typed_in(SAID, LINE),
        busy_screen(),
    ]);
    m.gen_fence = true;
    m.sends_gen = true;
    m.human_seq = Some(0);
    m.help = FENCED_HELP.to_string();
    m.unmoved_after_send = Some(Duration::from_secs(120));
    m.cell_attrs = "none";
    m.cell_unreadable = unread;
    let mut s = virtual_session(&mut m, None);
    s.setup.background_settle = Duration::ZERO;
    let point = s.await_turn(Duration::from_secs(60)).expect("the turn end");
    let mut heard = Heard::default();
    s.type_text_written(&point, LINE, RULE_CONTINUE, &mut heard)
        .expect("the act");
    assert!(s.custody.is_some(), "held: {:#?}", heard.noted);
    let seq = point.screen.seq + 10;
    s.took(seq, None, &mut heard);
    let screen = Screen {
        seq,
        cursor_col: 2 + typed.chars().count(),
        human_seq: Some(0),
        ..read_screen(rows)
    };
    if at_break {
        let mut since = None;
        s.host_steps_in_background(&screen, &mut since, None, &mut heard)
            .expect("the break's step");
    } else {
        let turn = s.turn_of(screen);
        s.custody_after_work(&turn, &mut heard)
            .expect("the point's look");
    }
    let held = s.custody.is_some();
    drop(s);
    (held, heard.noted)
}

/// EVERY READ THAT CANNOT SAY WHAT THE COMPOSER HOLDS LEAVES A LINE HELD AFTER A TURN AS IT
/// STANDS (ATERM-KI-1101-3, [`Session::custody_turned`]): at the point a busy spell ends on, a box
/// over the composer, no composer drawn, and the line drawn under a glyph cell unread; at a break,
/// the line drawn under a glyph cell unread. None settles it (`went with a turn`), as a read that
/// shows none of the line does once `status` reads the input clear. Controls: that read settles
/// it at the point and at the break, and a read that shows the line settles it drawn.
#[test]
fn a_read_that_cannot_say_leaves_a_line_held_after_a_turn_as_it_stands() {
    const OTHER: &str = "Ran the suite again; one failure left.";
    let mut bare = ended(OTHER);
    bare.truncate(4);
    for (tag, rows, typed, unread, at_break) in [
        ("boxed-at-point", boxed(OTHER), "", false, false),
        ("no-composer-at-point", bare, "", false, false),
        ("unread-at-point", typed_in(OTHER, LINE), LINE, true, false),
        ("unread-at-break", break_with(LINE), LINE, true, true),
    ] {
        let (held, noted) = turn_read_on(rows, typed, unread, at_break);
        assert!(
            held && !noted.iter().any(|l| l.contains(LINE_SETTLED)),
            "{tag}: held as it stands: {noted:#?}"
        );
    }
    for (tag, at_break) in [("none-at-point", false), ("none-at-break", true)] {
        let rows = if at_break {
            break_with("")
        } else {
            ended(OTHER)
        };
        let (held, noted) = turn_read_on(rows, "", false, at_break);
        assert!(
            !held && settled(&noted, Settles::Turn),
            "{tag}: went with the turn: {noted:#?}"
        );
    }
    let (held, noted) = turn_read_on(typed_in(OTHER, LINE), LINE, false, false);
    assert!(
        !held && settled(&noted, Settles::Drawn),
        "drawn: {noted:#?}"
    );
}

/// A TURN KEPT UNDER A BOX IS WEIGHED AT THE NEXT POINT'S LOOK, WHATEVER THAT POINT DECIDES
/// (ATERM-KI-1101-3, [`Session::custody_look`]): the worker read the continuation and drew none of
/// it, so it is held; a turn of its own runs and the busy spell ends on a permission box over the
/// composer, which keeps the turn's evidence; the box goes with no turn read running after it, and
/// the new point's read shows the composer empty, `status` reading the input clear. That point's
/// look weighs the kept turn (`went with a turn`) before the policy decides anything, so the host
/// offered the point is told no line is held whether or not the point writes a line (only a line's
/// write reaches the gate). Controls: the box still over the composer at that point keeps the line
/// held; with no turn read running since the write, the empty composer settles nothing. Negative
/// controls: the point's look a look alone, as before (the kept turn weighed only at a spell's end,
/// a break and the gate), and by mutation the kept turn dropped from `Session::custody_look_on`:
/// the line stands held at the empty point (`LineHeld::Stands`), as the host is told.
#[test]
fn a_turn_kept_under_a_box_is_weighed_at_the_next_points_look() {
    const OTHER: &str = "Ran the suite again; one failure left.";
    for (tag, then, ran, settles) in [
        ("box-gone", ended(OTHER), true, true),
        ("box-stands", boxed(OTHER), true, false),
        ("no-turn", ended(OTHER), false, false),
    ] {
        let mut m = Mock::new(vec![
            busy_screen(),
            ended(SAID),
            typed_in(SAID, LINE),
            busy_screen(),
        ]);
        m.gen_fence = true;
        m.sends_gen = true;
        m.human_seq = Some(0);
        m.help = FENCED_HELP.to_string();
        m.unmoved_after_send = Some(Duration::from_secs(120));
        m.cell_attrs = "none";
        let opts = SuperviseOpts {
            policy: SupervisorConfig {
                human_grace_s: 0,
                ..owners_policy()
            },
            ..auto(900)
        };
        let mut s = virtual_session(&mut m, None);
        s.setup.background_settle = Duration::ZERO;
        let point = s.await_turn(Duration::from_secs(60)).expect("the turn end");
        let mut heard = Heard::default();
        s.type_text_written(&point, LINE, RULE_CONTINUE, &mut heard)
            .expect("the act");
        assert!(s.custody.is_some(), "{tag}: held: {:#?}", heard.noted);
        let seq = point.screen.seq + 10;
        if ran {
            s.took(seq, None, &mut heard);
        }
        let at_box = Screen {
            seq,
            cursor_col: 2,
            human_seq: Some(0),
            ..read_screen(boxed(OTHER))
        };
        let spell_end = s.turn_of(at_box);
        s.custody_after_work(&spell_end, &mut heard)
            .expect("the spell's end");
        assert_eq!(
            s.custody_line_held(),
            Some(LineHeld::Stands),
            "{tag}: held under the box: {:#?}",
            heard.noted
        );
        let next = Screen {
            seq: seq + 1,
            cursor_col: 2,
            human_seq: Some(0),
            ..read_screen(then)
        };
        let next = s.turn_of(next);
        s.turn_end_looked(&next, None, &opts, &mut heard)
            .expect("the point's decision");
        let told = s.custody_line_held();
        drop(s);
        assert_eq!(
            (told, settled(&heard.noted, Settles::Turn)),
            if settles {
                (None, true)
            } else {
                (Some(LineHeld::Stands), false)
            },
            "{tag}: the point's look: {:#?}",
            heard.noted
        );
    }
}

/// A HELD LINE HOLDS THE SURVEY'S 0 UNTIL IT SETTLES (ATERM-KI-1101-3): Claude Code's rating card
/// is open over an empty composer while the continuation is held. Its `0` is a loop writer's key
/// like any other: queued behind a line the worker has yet to read, or pressed as a line it read
/// draws, it may land in the composer beside the line. So it waits while the line is held, and goes
/// once the custody settles. Negative control by mutation: the custody's hold dropped from
/// `Session::survey`: the `0` pressed while the line is held.
#[test]
fn a_held_line_holds_the_surveys_0_until_it_settles() {
    let card = with_survey(ended(SAID));
    let mut m = Mock::new(vec![busy_screen(), ended(SAID), card.clone(), card]);
    m.gen_fence = true;
    m.sends_gen = true;
    m.human_seq = Some(0);
    m.help = FENCED_HELP.to_string();
    m.unmoved_after_send = Some(Duration::from_secs(120));
    m.cell_attrs = "none";
    let opts = surveys(
        SuperviseOpts {
            policy: SupervisorConfig {
                human_grace_s: 0,
                ..owners_policy()
            },
            ..auto(900)
        },
        true,
    );
    let mut s = virtual_session(&mut m, None);
    s.setup.background_settle = Duration::ZERO;
    s.set_survey_settle(Duration::ZERO);
    let point = s.await_turn(Duration::from_secs(60)).expect("the turn end");
    let mut heard = Heard::default();
    s.type_text_written(&point, LINE, RULE_CONTINUE, &mut heard)
        .expect("the act");
    assert!(s.custody.is_some(), "held: {:#?}", heard.noted);
    // The screen moves on from the write's stand to the card.
    s.fx.sleep(Duration::from_secs(130));
    let read = s.screen().expect("the card's read");
    assert!(
        read.rows
            .iter()
            .any(|r| r.starts_with("● How is Claude doing")),
        "the card: {:#?}",
        read.rows
    );
    let at = s.turn_of(read);
    let mut survey = Survey::Closed;
    s.survey(&at, &opts, &mut survey, false, &mut heard)
        .expect("the card's look");
    assert_eq!(
        survey,
        Survey::Closed,
        "the 0 waits on the line held: {:#?}",
        heard.noted
    );
    s.custody_ended(at.screen.seq, Settles::Person, &mut heard);
    s.survey(&at, &opts, &mut survey, false, &mut heard)
        .expect("the card's look once the line settled");
    drop(s);
    assert!(
        matches!(
            survey,
            Survey::Tried {
                pressed: Some(_),
                ..
            }
        ),
        "the 0 goes once the line settled: {survey:?} {:#?}",
        heard.noted
    );
    let zeros: Vec<&String> = m
        .presses()
        .into_iter()
        .filter(|r| r.ends_with(" 0"))
        .collect();
    assert_eq!(zeros.len(), 1, "one 0: {:?}", m.requests);
}

/// A HELD LINE DRAWN AT A BREAK AND GONE BY THE POINT IS DRAWN, NEVER LOST (ATERM-KI-1101-3): the
/// worker read the continuation and drew none of it, so it is held; at a break of its own
/// background work the line draws in its composer, and by the point the spell ends on the
/// composer stands empty (someone cleared it), with no turn read running since the write. The
/// break gives no Enter and hands nothing back, but what it saw is the evidence of the draw
/// (`custody::Line::drawn`): the point settles the line `is drawn`, never restarts the agent for
/// a line it saw, and never journals it as read and drawn none of; the point then writes afresh,
/// entered once as it draws. Negative control by mutation: the break's draw unkept in
/// `custody::Line::look`: `restart:input-lost` asked, and the line journaled as drawn none of.
#[test]
fn a_held_line_drawn_at_a_break_and_gone_by_the_point_is_drawn_never_lost() {
    const OTHER: &str = "Ran the suite again; one failure left.";
    let run = lost_run(
        "drawn-then-gone",
        vec![break_with(LINE), ended(OTHER), typed_in(OTHER, LINE)],
        "relaunched",
        |m, _| {
            // The break comes past the echo's minute, inside the read line's; the empty point
            // stands, so its look and its gate's fresh read both find the composer empty.
            m.unmoved_after_send = Some(Duration::from_secs(65));
            m.stands.insert(3, Duration::from_secs(30));
            for at in [2, 4] {
                m.screen_cols.insert(at, 2 + LINE.len());
                m.cell_attrs_on.insert(at, "none");
            }
        },
    );
    let (t, noted) = (&run.t, &run.noted);
    assert!(held_formed(noted).is_some(), "held: {noted:#?}");
    assert!(
        settled(noted, Settles::Drawn),
        "drawn at the break: {noted:#?}\n{}",
        t.evidence()
    );
    assert!(
        run.host.said("restart:").is_empty(),
        "no restart for a line it saw drawn: {noted:#?}\n{}",
        t.evidence()
    );
    assert!(
        restarting(noted).is_empty() && !noted.iter().any(|l| l.contains(LINE_READ_UNDRAWN)),
        "never journaled as drawn none of: {noted:#?}"
    );
    assert!(
        t.m.attention.is_none() && !noted.iter().any(|l| l.starts_with("ESCALATED ")),
        "no badge, no ask: {noted:#?}"
    );
    assert_eq!(
        (lines_sent(t), t.enters().len()),
        (2, 1),
        "written afresh at the point, entered once: {noted:#?}\n{}",
        t.evidence()
    );
}

/// A LINE LOST WITH NO RESTART OF THE LOOP'S OWN IS TOLD THE HOST AS LOST ([`LineHeld::Lost`]):
/// under `[harness] relaunch = false` the worker reads the continuation and draws none of it.
/// While the line may still come the host is offered the idle point as it stands
/// ([`LineHeld::Stands`]: the host's upgrade waits whole); once the point's decision finds it read
/// and undrawn a minute on, at rest, and the loop makes no restart, the line waits, writing
/// nothing, and the host is offered the point as lost, so its upgrade, whose move ends the process
/// that read the line, may go where the loop makes no restart itself. Negative control by
/// mutation: the lost mark unset in `custody::Line::lose`: the point is offered as standing
/// after the loss, and an upgrade would wait for good.
#[test]
fn a_line_lost_with_no_restart_of_the_loops_own_is_told_the_host_as_lost() {
    let mut m = Mock::new(vec![busy_screen(), ended(SAID), ended(SAID)]);
    m.gen_fence = true;
    m.sends_gen = true;
    m.human_seq = Some(0);
    m.help = FENCED_HELP.to_string();
    m.unmoved_after_send = Some(Duration::from_secs(400));
    // The cursor on the composer's caret: an idle point the host is offered.
    m.cursor_on_caret = true;
    let host = AccountHost::scripted(&[], &[]);
    host.asks_points.store(true, Ordering::SeqCst);
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        policy: SupervisorConfig {
            human_grace_s: 0,
            relaunch: false,
            ..owners_policy()
        },
        ..auto(900)
    };
    let mut s = virtual_session(&mut m, None);
    let point = s.await_turn(Duration::from_secs(60)).expect("the turn end");
    let mut heard = Heard::default();
    let offer = |s: &mut Session<'_, Mock>, heard: &mut Heard| {
        s.host_passed = None;
        s.host_steps_here(&point, &opts, &mut false, heard)
            .expect("the host's step");
    };
    offer(&mut s, &mut heard);
    s.type_text_written(&point, LINE, RULE_CONTINUE, &mut heard)
        .expect("the act");
    assert!(s.custody.is_some(), "held: {:#?}", heard.noted);
    offer(&mut s, &mut heard);
    // The point decided past the read line's minute stands on the short turn's back-off; the
    // decision after it writes, as the gate lets it.
    for wait in [ECHO_HANG + CUSTODY_STEP, Duration::from_secs(150)] {
        s.fx.sleep(wait);
        s.turn_end_now(&point, &opts, &[], &mut heard)
            .expect("the point decided");
    }
    offer(&mut s, &mut heard);
    let lost_waits = waits_until(&heard.noted, RELAUNCH_OFF).len();
    drop(s);
    assert!(host.said("restart:").is_empty(), "{:#?}", heard.noted);
    assert_eq!(lost_waits, 1, "lost, waiting: {:#?}", heard.noted);
    let told: Vec<Option<LineHeld>> = host
        .points
        .lock()
        .unwrap()
        .iter()
        .filter(|(kind, _)| *kind == "idle")
        .map(|(_, p)| p.line_held)
        .collect();
    assert_eq!(
        told,
        [None, Some(LineHeld::Stands), Some(LineHeld::Lost)],
        "free before the write, standing while it may come, lost once judged so: {:#?}",
        heard.noted
    );
}

/// What a break met a line held in custody with ([`at_break`]).
struct AtBreak {
    /// Whether the line stands held after the break's step.
    held: bool,
    /// Whether the occupancy doors' and Cruise's lines at the break are held as behind a draft
    /// ([`Session::write_guards`]) after it.
    draft: bool,
    noted: Vec<String>,
}

/// The continuation written at a turn end and given up on undrawn (taken into custody), then a
/// break of the agent's own background work read with `typed` in its composer under the person's
/// key count `human` (the write's was 0), a turn read running before it where `ran`.
fn at_break(typed: &str, human: u64, ran: bool) -> AtBreak {
    let mut m = Mock::new(vec![
        busy_screen(),
        ended(SAID),
        typed_in(SAID, LINE),
        busy_screen(),
    ]);
    m.gen_fence = true;
    m.sends_gen = true;
    m.human_seq = Some(0);
    m.help = FENCED_HELP.to_string();
    m.unmoved_after_send = Some(Duration::from_secs(120));
    m.cell_attrs = "none";
    let host = AccountHost::scripted(&[], &[]);
    let cfg = SupervisorConfig {
        human_grace_s: 0,
        ..owners_policy()
    };
    let mut s = virtual_session(&mut m, None);
    s.setup.background_settle = Duration::ZERO;
    let point = s.await_turn(Duration::from_secs(60)).expect("the turn end");
    let mut heard = Heard::default();
    s.type_text_written(&point, LINE, RULE_CONTINUE, &mut heard)
        .expect("the act");
    assert!(s.custody.is_some(), "held: {:#?}", heard.noted);
    let seq = point.screen.seq + 10;
    if ran {
        s.took(seq, None, &mut heard);
    }
    let screen = Screen {
        seq,
        cursor_col: 2 + typed.chars().count(),
        human_seq: Some(human),
        ..read_screen(break_with(typed))
    };
    let mut since = None;
    s.host_steps_in_background(&screen, &mut since, None, &mut heard)
        .expect("the break's step");
    let held = s.custody.is_some();
    let now = s.fx.mono();
    let guards = s
        .write_guards(&screen, now, &cfg, host.as_ref())
        .expect("the guards read");
    drop(s);
    AtBreak {
        held,
        draft: guards.as_ref().is_ok_and(|g| g.draft),
        noted: heard.noted,
    }
}

/// A LINE HELD IN CUSTODY IS SETTLED AT A BREAK OF THE AGENT'S OWN BACKGROUND WORK BY WHAT SETTLES
/// IT AT A POINT ([`Session::custody_at_break`]), so a session that reads busy for hours on its
/// background work is not held for the spell by a line it dropped: a turn read running after the
/// write, with `status` reading the input clear, settles it (`went with a turn`), and the break's
/// lines go again (no draft guards them); a person's keys drawn after it do (`is under a person's
/// keys`), their draft then the break's guard as any draft is. Its draw settles nothing there: no
/// Enter is given at a break, so the line stands held, a draft the break's lines wait behind,
/// until the point's look hands it back to its writer
/// ([`a_held_line_drawn_in_a_busy_spell_is_handed_back_to_its_writer`]). Negative control: with
/// no turn and nothing drawn the line stands held at the break, its lines held behind it. By
/// mutation: the look dropped from the break's step: the person's case stands held.
#[test]
fn a_held_line_is_settled_at_a_break_by_what_settles_it_at_a_point() {
    let turn = at_break("", 0, true);
    assert!(
        !turn.held && settled(&turn.noted, Settles::Turn),
        "a turn ran after it: {:#?}",
        turn.noted
    );
    assert!(!turn.draft, "the break's lines go again: {:#?}", turn.noted);

    let drawn = at_break(LINE, 0, false);
    assert!(
        drawn.held && drawn.draft && !drawn.noted.iter().any(|l| l.contains(LINE_SETTLED)),
        "drawn at the break: held for the point's look, a draft: {:#?}",
        drawn.noted
    );

    let person = at_break("fix the flaky test first", 1, false);
    assert!(
        !person.held && settled(&person.noted, Settles::Person),
        "a person's keys after it: {:#?}",
        person.noted
    );
    assert!(
        person.draft,
        "their draft guards the break: {:#?}",
        person.noted
    );

    let none = at_break("", 0, false);
    assert!(
        none.held && none.draft,
        "nothing settles it: held, its lines held behind it: {:#?}",
        none.noted
    );
    assert!(
        !none.noted.iter().any(|l| l.contains(LINE_SETTLED)),
        "{:#?}",
        none.noted
    );
}

/// A DIALOG'S ANSWER GOES PAST A LINE HELD UNDER IT ONLY AFTER THE LINE IS KNOWN READ
/// (ATERM-KI-1101-3, [`custody::answer_goes`]): with the continuation held in custody, and a turn
/// read running after it or not, Claude Code's `/login` dialog over the composer gets the loop's
/// fenced key only where `status` proved the line read. With no input word, the key is held: it
/// may still sit behind the queued line. The line stays held for the read after a key that goes.
#[test]
fn a_dialogs_answer_only_after_the_held_line_is_known_read() {
    use crate::supervise::policy::login::{RULE_LOGIN_DONE, RULE_LOGIN_OVER_LIMIT};
    use aterm_phase::prompt::fixtures::{LOGIN_PICKER, LOGIN_SUCCESS, screen};
    let success = screen(LOGIN_SUCCESS);
    let row = success
        .iter()
        .find(|row| row.contains("Login successful"))
        .expect("the success row")
        .clone();
    let confirm = TurnEndAction::Confirm {
        rule_id: RULE_LOGIN_DONE,
        row,
    };
    let cancel = TurnEndAction::Cancel {
        rule_id: RULE_LOGIN_OVER_LIMIT,
    };
    for (tag, dialog, action, key) in [
        ("login-done", success, confirm, "login-done@v1 enter at "),
        (
            "over-limit",
            screen(LOGIN_PICKER),
            cancel,
            "login-over-limit@v1 esc at ",
        ),
    ] {
        for (input, first_goes, input_by_write) in [
            (INPUT_CLEAR, true, vec![]),
            (
                "input=- input_bytes=- input_wait_ms=- fg_footprint_mb=-",
                false,
                vec![
                    (
                        ECHO_HANG + Duration::from_secs(100),
                        "input=- input_bytes=- input_wait_ms=- fg_footprint_mb=-",
                    ),
                    (Duration::MAX, INPUT_CLEAR),
                ],
            ),
            (
                INPUT_PENDING,
                false,
                vec![
                    (ECHO_HANG + Duration::from_secs(100), INPUT_PENDING),
                    (Duration::MAX, INPUT_CLEAR),
                ],
            ),
            (
                "read-then-pending",
                false,
                vec![
                    (Duration::from_secs(30), INPUT_CLEAR),
                    (ECHO_HANG + Duration::from_secs(100), INPUT_PENDING),
                    (Duration::MAX, INPUT_CLEAR),
                ],
            ),
            (
                "read-then-unknown",
                true,
                vec![
                    (Duration::from_secs(30), INPUT_CLEAR),
                    (
                        ECHO_HANG + Duration::from_secs(100),
                        "input=- input_bytes=- input_wait_ms=- fg_footprint_mb=-",
                    ),
                    (Duration::MAX, INPUT_CLEAR),
                ],
            ),
        ] {
            for ran in [true, false] {
                let mut m = Mock::new(vec![
                    busy_screen(),
                    ended(SAID),
                    dialog.clone(),
                    dialog.clone(),
                ]);
                m.gen_fence = true;
                m.sends_gen = true;
                m.human_seq = Some(0);
                m.help = FENCED_HELP.to_string();
                m.unmoved_after_send = Some(Duration::from_secs(120));
                m.cell_attrs = "none";
                m.input = INPUT_CLEAR;
                // The first dialog gate is about 150 seconds after the line was sent. Its retry
                // is one custody step later, after each non-clear status script turns clear. The
                // The last two cases first prove the line read. Later pending input is still held
                // by the ordinary unread-input gate; a later unknown word cannot revoke proof
                // that the line, which precedes it in the one input queue, was already consumed.
                m.input_by_write = input_by_write.clone();
                let opts = SuperviseOpts {
                    policy: SupervisorConfig {
                        human_grace_s: 0,
                        ..owners_policy()
                    },
                    ..auto(900)
                };
                let mut s = virtual_session(&mut m, None);
                s.setup.background_settle = Duration::ZERO;
                let point = s.await_turn(Duration::from_secs(60)).expect("the turn end");
                let mut heard = Heard::default();
                s.type_text_written(&point, LINE, RULE_CONTINUE, &mut heard)
                    .expect("the act");
                assert!(s.custody.is_some(), "{tag}: held: {:#?}", heard.noted);
                if ran {
                    s.took(point.screen.seq + 1, None, &mut heard);
                }
                s.fx.sleep(Duration::from_secs(90));
                let read = s.screen().expect("the dialog's read");
                let at = s.turn_of(read);
                s.turn_end_execute(&at, action.clone(), &opts, &[], &mut heard)
                    .expect("carried out");
                let typed = |heard: &Heard| {
                    heard
                        .said
                        .iter()
                        .chain(&heard.noted)
                        .filter(|l| l.starts_with("TYPED ") && l.contains(&format!(" rule={key}")))
                        .count()
                };
                assert_eq!(
                    typed(&heard),
                    usize::from(first_goes),
                    "{tag} (input {input}, ran {ran}): first dialog gate: {:#?}",
                    [&heard.said, &heard.noted]
                );
                assert!(
                    s.custody.is_some(),
                    "{tag} (input {input}, ran {ran}): custody after first gate: {:#?}",
                    [&heard.said, &heard.noted]
                );
                if !first_goes {
                    s.fx.sleep(CUSTODY_STEP);
                    let read = s.screen().expect("the dialog's clear-input retry");
                    let at = s.turn_of(read);
                    s.turn_end_execute(&at, action.clone(), &opts, &[], &mut heard)
                        .expect("carried out after the line was read");
                }
                assert_eq!(
                    typed(&heard),
                    1,
                    "{tag} (input {input}, ran {ran}): exactly one dialog key: {:#?}",
                    [&heard.said, &heard.noted]
                );
                assert!(
                    s.custody.is_some(),
                    "{tag} (input {input}, ran {ran}): the line stands after the key: {:#?}",
                    [&heard.said, &heard.noted]
                );
            }
        }
    }
}
