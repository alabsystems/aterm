// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A LINE HELD IN CUSTODY THROUGH THE LOOP (ATERM-KI-1101-3, [`turn_end_loop::Custody`]): a line
//! the loop wrote and gave up on undrawn holds every writer until evidence settles it; one the
//! worker read and drew none of a minute on, at rest, is restarted in place once and written once
//! to the process after; where no restart can be made, and after a second loss, it is a doubling
//! wait that writes nothing. A held line raises nothing: journal records alone.

use super::super::super::turn_end_loop::{
    CUSTODY_CAP, CUSTODY_STEP, RELAUNCH_OFF, Settles, custody_step,
};
use super::*;
use crate::supervise::journal::{
    LINE_LOST_AGAIN, LINE_READ_UNDRAWN, LINE_RESTART_UNMADE, LINE_SETTLED,
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

/// What a [`lost_run`] left.
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
    let host = AccountHost::scripted(&[], &[answer]);
    let (dir, journal) = journal_file(&format!("custody-{tag}"));
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        journal: Some(journal.clone()),
        policy,
        ..auto(900)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(quick_turn_end(&[Duration::from_millis(10)]));
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
        host,
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
/// restart in place (`restart:input-lost`), the line settles with its program, and the resumed
/// point writes it once, entered when it draws. Negative controls by mutation: the restart
/// dropped from [`Session::custody_lost`] (a doubling wait in its place): no restart asked; the
/// custody gate dropped from [`Session::turn_end_write`]: the line written again on the unmoved
/// screen.
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
    assert!(
        noted
            .iter()
            .any(|l| l.starts_with("NOTE ") && l.contains(LINE_READ_UNDRAWN)),
        "its reason journaled: {noted:#?}"
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

/// WHERE NO RESTART CAN BE MADE, A LINE READ AND UNDRAWN IS A DOUBLING WAIT THAT WRITES NOTHING:
/// under `[harness] relaunch = false` the read line's minute passes at rest and no restart is
/// asked; the line is never written again on the unmoved screen and raises nothing, its `WAITING`
/// records name why, each wait longer than the last (the gaps between their `until=` stamps grow,
/// whether the run's wall clock stands or runs). Once the screen moves to show the line, it is
/// entered. Negative controls by mutation: the relaunch-off arm dropped from
/// [`Session::custody_lost`]: `restart:input-lost` asked; its wait count unkept: every wait a flat
/// [`CUSTODY_STEP`].
#[test]
fn a_restart_that_cannot_be_made_is_a_doubling_wait_that_writes_nothing() {
    let run = lost_run(
        "no-relaunch",
        vec![typed_in(SAID, LINE)],
        "relaunched",
        |m, policy| {
            policy.relaunch = false;
            m.unmoved_after_send = Some(Duration::from_secs(400));
            m.screen_cols.insert(2, 2 + LINE.len());
            m.cell_attrs_on.insert(2, "none");
        },
    );
    let (t, noted) = (&run.t, &run.noted);
    assert!(
        run.host.said("restart:").is_empty(),
        "no restart asked: {noted:#?}"
    );
    assert_eq!(lines_sent(t), 1, "written once: {}", t.evidence());
    assert!(
        t.m.attention.is_none() && !noted.iter().any(|l| l.starts_with("ESCALATED ")),
        "no badge, no ask: {noted:#?}"
    );
    let until = waits_until(noted, RELAUNCH_OFF);
    assert!(until.len() >= 3, "a wait, and two more: {noted:#?}");
    let gaps: Vec<i64> = until.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(
        gaps.windows(2).all(|g| g[1] > g[0]),
        "each wait longer than the last: {gaps:?}\n{noted:#?}"
    );
    assert_eq!(
        t.enters().len(),
        1,
        "drawn, entered: {noted:#?}\n{}",
        t.evidence()
    );
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

/// A RESTART THE HOST REFUSES IS ASKED FOR ONCE, NOT AT EVERY DUE: the worker read the line and drew
/// none of it a minute on, at rest, and the host answers the restart in place `refused:…`. A
/// repeated failure calls for a different approach: from there the line is a doubling wait that
/// writes nothing ([`LINE_RESTART_UNMADE`], the host's step named), and the host is never asked
/// again. Once the screen moves to show the line, it is entered. Negative control by mutation: the
/// refusal unkept in [`Session::restart_agent`]: `restart:input-lost` asked at each due.
#[test]
fn a_refused_restart_is_asked_once_and_the_line_waits() {
    let run = lost_run(
        "refused",
        vec![typed_in(SAID, LINE)],
        "refused:not-a-shell-job",
        |m, _| {
            m.unmoved_after_send = Some(Duration::from_secs(400));
            m.screen_cols.insert(2, 2 + LINE.len());
            m.cell_attrs_on.insert(2, "none");
        },
    );
    let (t, noted) = (&run.t, &run.noted);
    assert_eq!(
        run.host.said("restart:"),
        ["restart:input-lost"],
        "asked once, never again: {noted:#?}\n{}",
        t.evidence()
    );
    assert_eq!(lines_sent(t), 1, "written once: {}", t.evidence());
    assert!(
        t.m.attention.is_none() && !noted.iter().any(|l| l.starts_with("ESCALATED ")),
        "no badge, no ask: {noted:#?}"
    );
    let unmade = format!("{LINE_RESTART_UNMADE} (refused:not-a-shell-job)");
    assert!(
        !waits_until(noted, &unmade).is_empty(),
        "the refusal's wait names its step: {noted:#?}"
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
/// person's arm dropped from [`Session::custody_look`] (their text read as the line's draw):
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
/// control by mutation: [`Session::custody_turned`] settling on any turn: the unread line settled
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
/// lines go again (no draft guards them); its draw settles it (`is drawn`) and a person's keys
/// drawn after it do (`is under a person's keys`), each draft then the break's guard as any
/// draft is. Negative control: with no turn and nothing drawn the line stands held at the break,
/// its lines held behind it. By mutation: the look dropped from the break's step: every case
/// stands held.
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
        !drawn.held && settled(&drawn.noted, Settles::Drawn),
        "drawn at the break: {:#?}",
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
