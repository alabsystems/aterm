// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Budget, level-shedding, and refinement conformance tests for `brief`.

use aterm_slog::{
    Actor, Amend, BoxWord, BriefInput, Close, Conf, Cursor, Fields, Key, Kind, Limits, Log, Mark,
    Node, Open, Out, RunOut, ShortId, Src, StepOut, TurnTracker, Verdict, WallKind, tokens,
};

use crate::support::{WALL0, agent_session, at, brief_mail_links_hold};

fn session(turns: u64) -> (Log, u64) {
    let mut log = Log::new(2);
    let run = agent_session(&mut log, 0);
    log.mark(
        Mark::new(Kind::Title, Src::Engine, Conf::Exact).fields(
            Fields::new()
                .text(Key::Title, "SECRET TITLE")
                .expect("title"),
        ),
        at(1),
    )
    .expect("title");
    let mut tr = TurnTracker::new(run);
    let mut ms = 10;
    for i in 0..turns {
        for v in [
            Verdict::Busy,
            Verdict::Prompt(BoxWord::from_detail("bash:not-read-only")),
            Verdict::Busy,
        ] {
            ms += 50;
            tr.observe(&mut log, v, None, at(ms));
        }
        if i % 3 == 0 {
            log.mark(
                Mark::new(Kind::Human, Src::Input, Conf::Strong).by(Actor::Human),
                at(ms),
            )
            .expect("human");
        }
        if i + 1 < turns {
            ms += 50;
            tr.observe(&mut log, Verdict::Idle, None, at(ms));
            ms += 1000;
            tr.tick(&mut log, at(ms));
        }
    }
    ms += 50;
    tr.observe(
        &mut log,
        Verdict::Prompt(BoxWord::from_detail("edit")),
        None,
        at(ms),
    );
    (log, WALL0 + ms + 40_000)
}

fn body_lines(text: &str) -> Vec<&str> {
    text.lines().skip(1).collect()
}

#[test]
fn brief_has_its_shape() {
    let (log, now) = session(5);
    let b = log.brief("s-7bd6", 2000, now);
    assert_eq!(b.level, 0, "everything fits:\n{}", b.text);
    let lines: Vec<&str> = b.text.lines().collect();
    let header = lines[0];
    assert!(
        header.starts_with(&format!(
            "OK {} brief sid=s-7bd6 v=2 as_of={} ep=2 budget=2000 used=",
            lines.len() - 1,
            log.last_seq()
        )),
        "{header}"
    );
    assert!(header.ends_with(&format!("used={}", b.used)));
    assert_eq!(b.used, tokens(b.text.len()));
    let session = lines[1];
    assert!(
        session.starts_with("- claude r1 ") && session.contains("need=approval:edit"),
        "{session}"
    );
    assert!(session.ends_with("[screen+]"), "{session}");
    assert_eq!(lines.last().map(|l| l.starts_with("more=")), Some(true));
    for line in &lines[1..lines.len() - 1] {
        let lead = line.split(' ').next().unwrap_or("");
        let node_led = lead == "-"
            || lead.parse::<u64>().is_ok()
            || (lead.len() > 1
                && "rtp".contains(&lead[..1])
                && lead[1..]
                    .split('-')
                    .next()
                    .is_some_and(|n| n.parse::<u32>().is_ok()));
        let trailer = ["need", "mail", "claim", "gap"].contains(&lead);
        assert!(node_led || trailer, "a line led by no node id: {line:?}");
    }
    assert!(
        lines.iter().any(|l| l.starts_with("t5 open ")),
        "{}",
        b.text
    );
    assert!(
        lines.iter().any(|l| l.starts_with("t1 done ")),
        "{}",
        b.text
    );
    assert!(
        lines.iter().any(|l| l.contains("approval bash passed")),
        "{}",
        b.text
    );
    assert!(
        !b.text.contains("SECRET"),
        "the title never reaches a brief"
    );
}

#[test]
fn brief_stays_within_its_budget() {
    let (log, now) = session(120);
    let floor = log.brief("s-1", 0, now);
    assert_eq!(floor.level, 4, "a zero budget gets the one-line form");
    for budget in [floor.used, 40, 60, 100, 150, 250, 400, 600, 1000, 4000] {
        let b = log.brief("s-1", budget, now);
        assert_eq!(b.used, tokens(b.text.len()));
        if budget >= floor.used {
            assert!(
                b.used <= budget,
                "budget {budget}: used {}\n{}",
                b.used,
                b.text
            );
        }
    }
}

#[test]
fn briefing_floor_is_its_fit_at_zero() {
    for turns in [1, 5, 30, 120] {
        let (log, now) = session(turns);
        for cursor in [
            Cursor::new(log.ep(), 0),
            Cursor::new(log.ep(), log.last_seq() - 3),
            Cursor::new(log.ep() + 7, 3),
        ] {
            let read = log.read(cursor, usize::MAX);
            let input = BriefInput::new(read.recs, read.gap, log.state(), &[]);
            let briefing = input.scanned();
            let floor = briefing.floor("s-1", now);
            assert_eq!(floor, briefing.fit("s-1", 0, now), "{turns} turns");
            assert_eq!(floor.level, 4, "{}", floor.text);

            let one = briefing.fit("s-1", 1, now);
            assert_eq!(one.level, 4, "{}", one.text);
            assert_ne!(one.text, floor.text);
            assert_eq!(one.text, floor.text.replacen(" budget=0 ", " budget=1 ", 1));
        }
    }
}

#[test]
fn brief_sheds_in_order() {
    let (log, now) = session(30);
    let full = log.brief("s-1", 100_000, now);
    assert_eq!(full.level, 0);
    let full_steps = body_lines(&full.text)
        .iter()
        .filter(|l| l.starts_with('p'))
        .count();
    let mut seen = Vec::new();
    for budget in (0..=full.used).rev() {
        let b = log.brief("s-1", budget, now);
        if seen.last() != Some(&b.level) {
            seen.push(b.level);
        }
        let lines = body_lines(&b.text);
        let steps = lines.iter().filter(|l| l.starts_with('p')).count();
        let ranges = lines
            .iter()
            .filter(|l| l.starts_with('t') && l.contains('-') && l.contains(" turns: "))
            .count();
        let marks = lines
            .iter()
            .filter(|l| {
                l.split(' ')
                    .next()
                    .is_some_and(|w| w.parse::<u64>().is_ok())
            })
            .count();
        match b.level {
            0 => assert!(steps > 1 && ranges == 0 && marks > 0),
            1 => assert!(steps < full_steps && ranges == 0 && marks > 0, "{}", b.text),
            2 => assert!(ranges > 0 && marks > 0, "{}", b.text),
            3 => assert!(
                marks == 0 && lines.iter().any(|l| l.starts_with("t30 ")),
                "{}",
                b.text
            ),
            _ => {
                assert_eq!(lines.len(), 2, "the session in one line:\n{}", b.text);
                assert!(lines[1].starts_with("more=t1-t30"), "{}", b.text);
            }
        }
    }
    assert_eq!(seen, vec![0, 1, 2, 3, 4], "each level in turn");
    let collapsed = log.brief("s-1", 0, now);
    let more = collapsed.text.lines().last().unwrap_or("");
    assert!(more.contains("marks:"), "{more}");
}

#[test]
fn brief_names_every_need_and_claim() {
    let mut log = Log::new(0);
    let run = agent_session(&mut log, 0);
    let mut tr = TurnTracker::new(run);
    tr.observe(&mut log, Verdict::Busy, None, at(100));
    log.mark(
        Mark::new(Kind::Need, Src::Mark, Conf::Claim)
            .by(Actor::Sid(ShortId::new("s-ops").expect("sid")))
            .fields(Fields::new().word(Key::Word, "help").expect("help")),
        at(200),
    )
    .expect("need");
    tr.observe(
        &mut log,
        Verdict::Prompt(BoxWord::from_detail("bash:not-read-only")),
        None,
        at(300),
    );
    log.mark(
        Mark::new(Kind::Need, Src::Meta, Conf::Strong).fields(
            Fields::new()
                .word(Key::Word, "escalated")
                .expect("escalated"),
        ),
        at(400),
    )
    .expect("escalated");
    let now = WALL0 + 40_000;
    let b = log.brief("s-1", 600, now);
    let lines: Vec<&str> = b.text.lines().collect();
    assert!(lines[1].contains("need=approval:bash"), "{}", b.text);
    assert!(
        lines.iter().any(|l| l.starts_with("need escalated - ")),
        "{}",
        b.text
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("claim need help by=s-ops ") && l.ends_with("[?]")),
        "{}",
        b.text
    );
    let one = log.brief("s-1", 0, now);
    assert_eq!(one.level, 4);
    let more = one.text.lines().last().unwrap_or("");
    assert!(
        more.contains("needs:1") && more.contains("claims:1"),
        "{more}"
    );
}

#[test]
fn brief_keeps_the_open_turn_its_window_lacks() {
    let mut log = Log::with_limits(0, Limits::new(64, 1 << 20).expect("small"));
    let run = agent_session(&mut log, 0);
    let mut tr = TurnTracker::new(run);
    tr.ctl_turn(&mut log, Actor::Harness, 7, at(10));
    tr.observe(&mut log, Verdict::Busy, None, at(20));
    tr.observe(
        &mut log,
        Verdict::Prompt(BoxWord::from_detail("edit")),
        None,
        at(30),
    );
    for i in 0..100 {
        log.mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), at(40 + i))
            .expect("bell");
    }
    assert!(log.lost() > 0, "the bells evicted");
    assert!(
        log.window().any(|r| r.k == Kind::Turn),
        "the open turn's open record is pinned"
    );
    let without: Vec<&aterm_slog::Rec> = log.window().filter(|r| r.k != Kind::Turn).collect();
    let b = aterm_slog::brief(
        "s-1",
        without,
        None,
        log.state(),
        &[],
        100_000,
        WALL0 + 60_000,
    );
    let turn = b
        .text
        .lines()
        .find(|l| l.starts_with("t1 open "))
        .unwrap_or_else(|| panic!("no open turn line:\n{}", b.text));
    assert!(turn.contains(" by=harness ledger=7"), "{turn}");
    assert!(turn.contains("· p1 approval edit open"), "{turn}");
}

#[test]
fn a_harness_turn_whose_open_was_evicted_still_briefs_its_author_and_ledger() {
    let mut log = Log::with_limits(1, Limits::new(20, 1 << 20).expect("a 20-record ring"));
    let run = agent_session(&mut log, 0);
    let mut tr = TurnTracker::new(run);
    tr.ctl_turn(&mut log, Actor::Harness, 7, at(10));
    tr.observe(&mut log, Verdict::Busy, None, at(20));
    tr.observe(&mut log, Verdict::Idle, None, at(30));
    tr.tick(&mut log, at(60_000));
    let t1 = log
        .window()
        .find(|r| r.op == aterm_slog::Op::Close && r.k == Kind::Turn)
        .map(|r| r.node)
        .expect("the harness turn closed");
    let has = |log: &Log, op: aterm_slog::Op| log.window().any(|r| r.node == t1 && r.op == op);
    let mut ms = 61_000;
    while has(&log, aterm_slog::Op::Open) {
        let t = log
            .open(
                Open::new(Kind::Turn, Src::Screen, Conf::Strong)
                    .under(run)
                    .by(Actor::Ctl),
                at(ms),
            )
            .expect("a later turn");
        log.close(
            t,
            Close::new(
                Out::Turn(aterm_slog::TurnOut::Done),
                Src::Screen,
                Conf::Strong,
            ),
            at(ms + 5),
        )
        .expect("it closes");
        ms += 10;
        assert!(ms < 70_000, "the ring never evicted t1's open");
    }
    assert!(
        has(&log, aterm_slog::Op::Close),
        "PRECONDITION: t1's close is live"
    );
    let b = log.brief("s-1", 100_000, WALL0 + ms);
    let line = b
        .text
        .lines()
        .find(|l| l.starts_with(&format!("{t1} ")))
        .unwrap_or_else(|| panic!("no line for {t1}:\n{}", b.text));
    assert!(line.contains(" by=harness ledger=7"), "{line}");
}

fn mail_then_turn(mails: &[(&str, u64)], by: Actor, cause: Option<&str>) -> (Log, Node, u64) {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let mut ms = 10;
    for (kind, off) in mails {
        let f = Fields::new()
            .word(Key::Dir, "in")
            .and_then(|f| f.word(Key::MailKind, kind))
            .and_then(|f| f.u(Key::Off, *off))
            .expect("mail fields");
        log.mark(
            Mark::new(Kind::Mail, Src::Fabric, Conf::Exact)
                .at(Node::SESSION)
                .by(Actor::Bridge)
                .fields(f),
            at(ms),
        )
        .expect("the mail mark");
        ms += 10;
    }
    let f = cause.map_or_else(Fields::new, |c| Fields::new().cause(c).expect("a cause"));
    let src = if by == Actor::Human {
        Src::Input
    } else {
        Src::Ctl
    };
    let t = log
        .open(
            Open::new(Kind::Turn, src, Conf::Strong)
                .under(run)
                .by(by)
                .fields(f),
            at(ms),
        )
        .expect("the turn opens");
    (log, t, ms)
}

fn turn_line_of(log: &Log, t: Node, ms: u64) -> String {
    let b = log.brief("s-1", 100_000, WALL0 + ms);
    b.text
        .lines()
        .find(|l| l.starts_with(&format!("{t} ")))
        .unwrap_or_else(|| panic!("no line for {t}:\n{}", b.text))
        .to_string()
}

#[test]
fn a_turn_after_one_task_mail_briefs_it_as_a_guess() {
    let (mut log, t, ms) = mail_then_turn(&[("task", 9)], Actor::Ctl, None);
    let line = turn_line_of(&log, t, ms + 1000);
    assert!(
        line.starts_with(&format!("{t} open 1s by=ctl mail=@9~ ")),
        "{line}"
    );
    assert_eq!(brief_mail_links_hold(&log, WALL0 + ms + 1000), Ok(1));
    log.close(
        t,
        Close::new(
            Out::Turn(aterm_slog::TurnOut::Done),
            Src::Screen,
            Conf::Strong,
        ),
        at(ms + 2000),
    )
    .expect("it closes");
    let line = turn_line_of(&log, t, ms + 3000);
    assert!(line.contains(" by=ctl mail=@9~ "), "{line}");
    assert_eq!(brief_mail_links_hold(&log, WALL0 + ms + 3000), Ok(1));

    for (mails, by, cause, what) in [
        (&[("note", 9)][..], Actor::Ctl, None, "a note"),
        (
            &[("task", 9), ("task", 10)][..],
            Actor::Ctl,
            None,
            "two task mails",
        ),
        (&[("task", 9)][..], Actor::Human, None, "a person's turn"),
        (&[("task", 9)][..], Actor::Ctl, Some("@9"), "a named cause"),
    ] {
        let (log, t, ms) = mail_then_turn(mails, by, cause);
        let line = turn_line_of(&log, t, ms + 1000);
        assert!(!line.contains("mail="), "{what}: {line}");
        assert_eq!(
            brief_mail_links_hold(&log, WALL0 + ms + 1000),
            Ok(0),
            "{what}"
        );
        if cause.is_some() {
            assert!(line.contains(" by=ctl cause=@9 "), "{what}: {line}");
        }
        if mails.len() == 2 {
            assert_eq!(log.state().totals.mail_amb, 1, "{what}");
        }
    }
}

#[test]
fn brief_sheds_old_turn_lines_before_the_session() {
    let mut log = Log::new(0);
    let run = agent_session(&mut log, 0);
    let mut tr = TurnTracker::new(run);
    let mut ms = 0;
    for _ in 0..60 {
        ms += 100;
        tr.observe(&mut log, Verdict::Busy, None, at(ms));
        ms += 100;
        tr.observe(
            &mut log,
            Verdict::Wall(WallKind::UsageSession),
            Some("3pm"),
            at(ms),
        );
        ms += 1000;
        tr.tick(&mut log, at(ms));
    }
    assert_eq!(log.state().totals.walled, 60);
    let b = log.brief("s-1", 600, WALL0 + ms + 1000);
    assert!(b.level < 4, "fell to one line:\n{}", b.text);
    assert!(b.used <= 600, "{}", b.text);
    assert!(
        b.text.lines().any(|l| l.starts_with("t60 ")),
        "the newest turn:\n{}",
        b.text
    );
    let more = b.text.lines().last().unwrap_or("");
    assert!(more.starts_with("more=") && more.contains("t1"), "{more}");
}

#[test]
fn a_narrowed_window_reports_only_the_gap_its_read_crossed() {
    let (log, now) = session(3);
    let past = log.read(
        aterm_slog::Cursor::new(log.ep(), log.last_seq() - 3),
        usize::MAX,
    );
    assert!(past.gap.is_none());
    let b = aterm_slog::brief("s-1", past.recs, past.gap, log.state(), &[], 2000, now);
    assert!(!b.text.contains("gap "), "{}", b.text);
    let stale = log.read(aterm_slog::Cursor::new(log.ep() + 7, 3), usize::MAX);
    let g = stale.gap.expect("another epoch's cursor is reset");
    let b = aterm_slog::brief("s-1", stale.recs, stale.gap, log.state(), &[], 2000, now);
    assert!(
        b.text.contains(&format!(
            "\ngap why=reset lost={} resync_to={}\nmore=",
            g.lost, g.resync_to
        )),
        "{}",
        b.text
    );
}

#[test]
fn an_unobserved_phase_claims_nothing() {
    let mut log = Log::new(1);
    log.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        Actor::Aterm,
        Fields::new(),
        at(0),
    )
    .expect("session");
    let b = log.brief("s-1", 600, WALL0 + 10);
    let session_line = b.text.lines().nth(1).expect("a session line");
    assert_eq!(session_line, "- session · -", "{}", b.text);
    let (log, now) = session(1);
    let b = log.brief("s-1", 2000, now);
    assert!(
        b.text.lines().nth(1).is_some_and(|l| l.ends_with(']')),
        "{}",
        b.text
    );
}

#[test]
fn a_source_or_landing_mark_gives_no_brief_line() {
    let (mut log, now) = session(3);
    let before = log.brief("s-7bd6", 2000, now).text;
    let run = log.state().run().expect("the run").node;
    for (k, f) in [
        (
            Kind::Source,
            Fields::new()
                .tok(Key::Field, "transcript")
                .and_then(|f| f.word(Key::State, "partial"))
                .and_then(|f| f.u(Key::Skipped, 9)),
        ),
        (
            Kind::MailLanded,
            Fields::new()
                .u(Key::Inbox, 4)
                .and_then(|f| f.u(Key::Off, 88)),
        ),
    ] {
        log.mark(
            Mark::new(k, Src::Aterm, Conf::Strong)
                .at(run)
                .fields(f.expect("fields")),
            at(9_000),
        )
        .expect("a mark");
    }
    let after = log.brief("s-7bd6", 2000, now).text;
    let body = |t: &str| t.lines().skip(1).map(str::to_string).collect::<Vec<_>>();
    assert_eq!(body(&after), body(&before), "{after}");
    log.mark(
        Mark::new(Kind::Human, Src::Input, Conf::Strong).by(Actor::Human),
        at(9_001),
    )
    .expect("human");
    let rung = log.brief("s-7bd6", 2000, now).text;
    assert!(body(&rung).len() > body(&before).len(), "{rung}");
}

#[test]
fn brief_reads_an_exact_refinement_and_never_a_strong_one() {
    use aterm_slog::{Amend, Close, Open, Out, TurnOut};
    let mut log = Log::new(2);
    let run = agent_session(&mut log, 0);
    let turn = log
        .open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
            at(1),
        )
        .expect("turn");
    let tool = log
        .open(
            Open::new(Kind::Tool, Src::Transcript, Conf::Strong)
                .under(turn)
                .fields(Fields::new().word(Key::Cls, "shell").expect("cls")),
            at(2),
        )
        .expect("tool");
    log.close(
        turn,
        Close::new(Out::Turn(TurnOut::Done), Src::Screen, Conf::Strong),
        at(5),
    )
    .expect("closes");
    let amend = |log: &mut Log, node, kind, conf, f: Fields, ms| {
        log.amend(node, Amend::new(kind, Src::Transcript, conf, f), at(ms))
            .expect("amends");
    };
    let line = |log: &Log, lead: &str| {
        let b = log.brief("s-7bd6", 4000, WALL0 + 10_000);
        b.text
            .lines()
            .find(|l| l.starts_with(lead))
            .map(str::to_string)
            .unwrap_or_else(|| panic!("no {lead} line:\n{}", b.text))
    };
    let t = format!("t{} ", turn.n);
    let p = format!("p{} ", tool.n);
    assert!(line(&log, &t).starts_with(&format!("t{} done ", turn.n)));
    assert!(line(&log, &p).contains(" gone "), "{}", line(&log, &p));
    amend(
        &mut log,
        turn,
        Kind::Turn,
        Conf::Strong,
        Fields::new().word(Key::Out, "wall:context").expect("o"),
        6,
    );
    amend(
        &mut log,
        tool,
        Kind::Tool,
        Conf::Strong,
        Fields::new().word(Key::Out, "ok").expect("o"),
        6,
    );
    assert!(
        line(&log, &t).starts_with(&format!("t{} done ", turn.n)),
        "{}",
        line(&log, &t)
    );
    assert!(line(&log, &p).contains(" gone "), "{}", line(&log, &p));
    amend(
        &mut log,
        turn,
        Kind::Turn,
        Conf::Exact,
        Fields::new().word(Key::Out, "wall:context").expect("o"),
        7,
    );
    amend(
        &mut log,
        tool,
        Kind::Tool,
        Conf::Exact,
        Fields::new().word(Key::Out, "error").expect("o"),
        7,
    );
    assert!(
        line(&log, &t).starts_with(&format!("t{} wall:context ", turn.n)),
        "{}",
        line(&log, &t)
    );
    assert!(line(&log, &p).contains(" error "), "{}", line(&log, &p));
    amend(
        &mut log,
        turn,
        Kind::Turn,
        Conf::Strong,
        Fields::new()
            .u(Key::Steps, 3)
            .and_then(|f| f.tok(Key::Tools, "shell:2,edit:1"))
            .expect("rollup"),
        8,
    );
    let tl = line(&log, &t);
    assert!(tl.contains(" · 3 steps shell 2 edit 1"), "{tl}");
}

#[test]
fn the_3b_marks_leave_the_brief_as_it_was() {
    let (mut base, now) = session(12);
    let (mut with, _) = session(12);
    let run = with.state().run().expect("the run").node;
    let words = |k: Kind| -> Fields {
        let f = Fields::new();
        match k {
            Kind::Integration | Kind::Render | Kind::Progress | Kind::Goal | Kind::Hold => {
                f.word(Key::State, "on")
            }
            Kind::Ctx => f.u(Key::Pct, 40),
            Kind::Notify | Kind::Bell => f.u(Key::N, 2),
            Kind::InDoubt => f.word(Key::Verb, "send"),
            Kind::Usage => f.u(Key::H5, 30).and_then(|f| f.u(Key::D7, 9)),
            Kind::Status => f.word(Key::Phase, "running"),
            Kind::Meta => f
                .tok(Key::Field, "attention")
                .and_then(|f| f.text(Key::Value, "supervisor")),
            Kind::Mail => f
                .word(Key::Dir, "in")
                .and_then(|f| f.word(Key::MailKind, "task"))
                .and_then(|f| f.u(Key::Inbox, 3)),
            Kind::MailSeen => f
                .u(Key::Inbox, 3)
                .and_then(|f| f.word(Key::Word, "handled")),
            Kind::MailLanded => f.u(Key::Inbox, 4).and_then(|f| f.u(Key::Off, 88)),
            Kind::Task => f
                .u(Key::Rev, 2)
                .and_then(|f| f.word(Key::State, "done"))
                .and_then(|f| f.u(Key::Attention, 0))
                .and_then(|f| f.word(Key::Why, "marker")),
            Kind::Act => f.word(Key::Word, "approve"),
            Kind::Story => f.word(Key::Word, "approved"),
            Kind::Cwd => f.text(Key::Cwd, "/w/repo"),
            Kind::State => f.word(Key::State, "alive"),
            Kind::Policy => f.id(Key::Gen, "7"),
            Kind::Topic => f.id(Key::Fp, "0a1b2c3d4e5f6071"),
            Kind::Advisory => f
                .word(Key::Origin, "fleet")
                .and_then(|f| f.id(Key::Gen, "3")),
            Kind::Program => f
                .word(Key::State, "blocked")
                .and_then(|f| f.word(Key::Ask, "permission")),
            Kind::MailFetch => f.u(Key::Off, 91),
            Kind::Shown => f
                .word(Key::Need, "approval")
                .and_then(|f| f.word(Key::Sev, "decide")),
            Kind::Row => f
                .tok(Key::RowKind, "job-change")
                .and_then(|f| f.text(Key::RowText, "exited job=j41 class=exited rev=1")),
            Kind::MailGap => f
                .u(Key::Off, 7)
                .and_then(|f| f.u(Key::Floor, 42))
                .and_then(|f| f.u(Key::Lost, 3)),
            _ => Ok(f),
        }
        .expect("fields")
    };
    let kinds = [
        Kind::Integration,
        Kind::Status,
        Kind::Ctx,
        Kind::Goal,
        Kind::Notify,
        Kind::Progress,
        Kind::Bell,
        Kind::Render,
        Kind::Modes,
        Kind::InDoubt,
        Kind::Usage,
        Kind::Cwd,
        Kind::Meta,
        Kind::Mail,
        Kind::MailSeen,
        Kind::MailLanded,
        Kind::Task,
        Kind::Hold,
        Kind::Act,
        Kind::Story,
        Kind::State,
        Kind::Policy,
        Kind::Topic,
        Kind::Advisory,
        Kind::Program,
        Kind::MailFetch,
        Kind::Shown,
        Kind::Row,
        Kind::MailGap,
    ];
    for i in 0..12u64 {
        for k in kinds {
            with.mark(
                Mark::new(k, Src::Aterm, Conf::Strong)
                    .at(run)
                    .fields(words(k)),
                at(20_000 + i),
            )
            .expect("a 3B mark");
            base.mark(
                Mark::new(Kind::Agent, Src::Screen, Conf::Strong).at(run),
                at(20_000 + i),
            )
            .expect("an agent mark");
        }
    }
    let body = |t: &str| t.lines().skip(1).map(str::to_string).collect::<Vec<_>>();
    let counts = ["mail in 12 · out 0", "acts 12 · stories 12", "shown 12"];
    let full = base.brief("s-1", 100_000, now);
    let mut counted = 0;
    for budget in (0..=full.used + 40).rev() {
        let b = body(&base.brief("s-1", budget, now).text);
        let w = body(&with.brief("s-1", budget, now).text);
        let shown = counts.iter().filter(|c| w.iter().any(|l| l == *c)).count();
        assert!(shown == 0 || shown == counts.len(), "all or none: {w:?}");
        if shown == counts.len() {
            counted += 1;
            let without: Vec<String> = w
                .iter()
                .filter(|l| !counts.contains(&l.as_str()))
                .cloned()
                .collect();
            assert_eq!(without, b, "budget {budget}");
        } else {
            assert_eq!(w, b, "budget {budget}: the count lines go first");
        }
    }
    assert!(counted > 0, "the count lines show when they fit");
    assert!(
        body(&full.text).iter().any(|l| l.contains(" human ")),
        "a pre-3B mark still is a line: {}",
        full.text
    );
}

#[test]
fn a_codex_alab_sessions_acts_are_one_count_line_shed_first() {
    let (mut base, now) = session(6);
    let (mut with, _) = session(6);
    let run = with.state().run().expect("the run").node;
    let acts = [
        ("approve", "approve-all.v1"),
        ("answer", "answer-recommended.v1"),
        ("continue", "continue.v1"),
        ("continue", "usage-resume.v1"),
        ("relaunch", "upgrade.v1"),
        ("dispatch", "harness-turn.v1"),
    ];
    for (i, (word, rule)) in acts.iter().enumerate() {
        let ms = 30_000 + i as u64;
        with.mark(
            Mark::new(Kind::Act, Src::Mark, Conf::Claim)
                .by(Actor::Ctl)
                .fields(
                    Fields::new()
                        .word(Key::Word, word)
                        .and_then(|f| f.id(Key::Rule, rule))
                        .expect("an act"),
                ),
            at(ms),
        )
        .expect("an act mark");
        base.mark(
            Mark::new(Kind::Agent, Src::Screen, Conf::Strong).at(run),
            at(ms),
        )
        .expect("an agent mark");
    }
    let body = |t: &str| t.lines().skip(1).map(str::to_string).collect::<Vec<_>>();
    let count = "acts 6 · stories 0";
    let full = with.brief("s-1", 100_000, now);
    assert!(body(&full.text).iter().any(|l| l == count), "{}", full.text);
    let mut counted = 0;
    for budget in (0..=full.used + 40).rev() {
        let b = body(&base.brief("s-1", budget, now).text);
        let w = body(&with.brief("s-1", budget, now).text);
        if w.iter().any(|l| l == count) {
            counted += 1;
            let without: Vec<String> = w.iter().filter(|l| *l != count).cloned().collect();
            assert_eq!(without, b, "budget {budget}");
        } else {
            assert_eq!(w, b, "budget {budget}: the count line goes first");
        }
    }
    assert!(counted > 0, "the count line shows when it fits");
}

fn relaunched(acts: bool) -> (Log, u64) {
    let (mut log, _) = session(6);
    let run = log.state().run().expect("the run").node;
    let mut ms = 20_000;
    log.close(
        run,
        Close::new(Out::Run(RunOut::Signal), Src::Shell, Conf::Exact),
        at(ms),
    )
    .expect("the agent exits");
    for row in ["relaunch", "carry-on"] {
        ms += 10;
        let m = if acts {
            Mark::new(Kind::Act, Src::Aterm, Conf::Strong)
                .at(Node::SESSION)
                .by(Actor::Harness)
                .fields(
                    Fields::new()
                        .word(Key::Word, "relaunch")
                        .and_then(|f| f.id(Key::Rule, row))
                        .expect("an act"),
                )
        } else {
            Mark::new(Kind::Agent, Src::Screen, Conf::Strong).at(Node::SESSION)
        };
        log.mark(m, at(ms)).expect("a mark");
    }
    let f = Fields::new().tok(Key::Agent, "claude").expect("an agent");
    let again = log
        .open(
            Open::new(Kind::Agent, Src::Status, Conf::Strong).fields(f),
            at(ms + 10),
        )
        .expect("the relaunched run");
    let mut tr = TurnTracker::new(again);
    for _ in 0..2 {
        for v in [Verdict::Busy, Verdict::Idle] {
            ms += 100;
            tr.observe(&mut log, v, None, at(ms));
        }
        ms += 1000;
        tr.tick(&mut log, at(ms));
    }
    (log, WALL0 + ms + 40_000)
}

#[test]
fn a_relaunched_sessions_acts_are_one_count_line_shed_first() {
    let (base, now) = relaunched(false);
    let (with, _) = relaunched(true);
    let body = |t: &str| t.lines().skip(1).map(str::to_string).collect::<Vec<_>>();
    let count = "acts 2 · stories 0";
    let full = with.brief("s-1", 100_000, now);
    assert!(body(&full.text).iter().any(|l| l == count), "{}", full.text);
    let as_of = |t: &str| {
        t.split(' ')
            .find(|w| w.starts_with("as_of="))
            .map(str::to_string)
    };
    assert_eq!(
        as_of(&full.text),
        as_of(&base.brief("s-1", 100_000, now).text),
        "one as_of"
    );
    let mut counted = 0;
    for budget in (0..=full.used + 40).rev() {
        let b = body(&base.brief("s-1", budget, now).text);
        let w = body(&with.brief("s-1", budget, now).text);
        if w.iter().any(|l| l == count) {
            counted += 1;
            let without: Vec<String> = w.iter().filter(|l| *l != count).cloned().collect();
            assert_eq!(without, b, "budget {budget}");
        } else {
            assert_eq!(w, b, "budget {budget}: the count line goes first");
        }
    }
    assert!(counted > 0, "the count line shows when it fits");
}

/// A relaunch's carry-on turn briefs by the actor the host hands the tracker, beside the
/// relaunch's acts: `by=harness` for a turn typed over the harness's own token, which the
/// window's drive presents (aterm-gui's `control_tests` drive that socket path end to end), and
/// `by=ctl`, the manager's word, for one typed over the token beside the socket.
#[test]
fn a_relaunchs_carry_on_turn_briefs_as_the_harness() {
    let carried = |by: Actor| {
        let (mut log, _) = relaunched(true);
        let run = log.state().run().expect("the relaunched run").node;
        let mut tr = TurnTracker::new(run);
        let ms = 40_000;
        tr.ctl_turn(&mut log, by, 9, at(ms));
        tr.observe(&mut log, Verdict::Busy, None, at(ms + 10));
        let t = log.state().turn().expect("the carry-on turn").node;
        let b = log.brief("s-1", 100_000, WALL0 + ms + 100);
        assert!(b.text.contains("\nacts 2 "), "{}", b.text);
        b.text
            .lines()
            .find(|l| l.starts_with(&format!("{t} ")))
            .unwrap_or_else(|| panic!("no line for {t}:\n{}", b.text))
            .to_string()
    };
    let line = carried(Actor::Harness);
    assert!(line.contains(" by=harness ledger=9"), "{line}");
    let line = carried(Actor::Ctl);
    assert!(
        line.contains(" by=ctl ledger=9"),
        "NEGATIVE CONTROL: the beside token's turn: {line}"
    );
}

fn tx_step(log: &mut Log, run: Node, tx: bool, ms: u64, close: bool) -> Node {
    let open = if tx {
        let turn = log.state().turn().expect("an open turn").node;
        let f = Fields::new()
            .word(Key::Cls, "shell")
            .and_then(|f| f.text(Key::Tool, "Bash"))
            .and_then(|f| f.id(Key::Vref, "tu1"))
            .expect("fields");
        Open::new(Kind::Tool, Src::Transcript, Conf::Strong)
            .under(turn)
            .by(Actor::Vendor)
            .fields(f)
    } else {
        Open::new(Kind::Tool, Src::Screen, Conf::Strong).under(run)
    };
    let p = log.open(open, at(ms)).expect("a step");
    if close {
        let c = if tx {
            Close::new(Out::Step(StepOut::Ok), Src::Transcript, Conf::Strong)
                .by(Actor::Vendor)
                .dur_wall(5)
        } else {
            Close::new(Out::Step(StepOut::Ok), Src::Screen, Conf::Strong)
        };
        log.close(p, c, at(ms + 5)).expect("a close");
    }
    p
}

fn tx_amend(log: &mut Log, run: Node, tx: Option<(Node, Kind, Fields)>, ms: u64) {
    match tx {
        Some((node, kind, f)) => {
            log.amend(
                node,
                Amend::new(kind, Src::Transcript, Conf::Exact, f).by(Actor::Vendor),
                at(ms),
            )
            .expect("an amend");
        }
        None => {
            log.mark(
                Mark::new(Kind::Agent, Src::Screen, Conf::Strong).at(run),
                at(ms),
            )
            .expect("an agent mark");
        }
    }
}

fn tx_session(turns: u64, tx: bool) -> (Log, u64) {
    let mut log = Log::new(2);
    let run = agent_session(&mut log, 0);
    log.mark(
        Mark::new(Kind::Title, Src::Engine, Conf::Exact).fields(
            Fields::new()
                .text(Key::Title, "SECRET TITLE")
                .expect("title"),
        ),
        at(1),
    )
    .expect("title");
    let mut tr = TurnTracker::new(run);
    let mut ms = 10;
    for i in 0..turns {
        for v in [
            Verdict::Busy,
            Verdict::Prompt(BoxWord::from_detail("bash:not-read-only")),
            Verdict::Busy,
        ] {
            ms += 50;
            tr.observe(&mut log, v, None, at(ms));
        }
        ms += 10;
        tx_step(&mut log, run, tx, ms, true);
        ms += 10;
        let p = tx_step(&mut log, run, tx, ms, true);
        ms += 10;
        let out = tx.then(|| {
            let f = Fields::new().word(Key::Out, "error").expect("out");
            (p, Kind::Tool, f)
        });
        tx_amend(&mut log, run, out, ms);
        if i % 3 == 0 {
            log.mark(
                Mark::new(Kind::Human, Src::Input, Conf::Strong).by(Actor::Human),
                at(ms),
            )
            .expect("human");
        }
        if i + 1 < turns {
            let turn = log.state().turn().expect("the turn").node;
            ms += 50;
            tr.observe(&mut log, Verdict::Idle, None, at(ms));
            ms += 1000;
            tr.tick(&mut log, at(ms));
            let numbers = tx.then(|| {
                let f = Fields::new()
                    .u(Key::VdurMs, 900)
                    .and_then(|f| f.u(Key::TokIn, 1200 + i))
                    .and_then(|f| f.u(Key::TokOut, 300))
                    .expect("numbers");
                (turn, Kind::Turn, f)
            });
            tx_amend(&mut log, run, numbers, ms);
        }
    }
    ms += 50;
    tr.observe(
        &mut log,
        Verdict::Prompt(BoxWord::from_detail("edit")),
        None,
        at(ms),
    );
    for _ in 0..3 {
        ms += 10;
        tx_step(&mut log, run, tx, ms, false);
    }
    (log, WALL0 + ms + 40_000)
}

#[test]
fn the_transcript_sheds_before_what_the_brief_held_without_it() {
    let (base, now) = tx_session(12, false);
    let (with, _) = tx_session(12, true);
    let body = |t: &str| t.lines().skip(1).map(str::to_string).collect::<Vec<_>>();
    let id = |l: &str| l.split(' ').next().unwrap_or("").to_string();
    let full = with.brief("s-1", 100_000, now);
    let (mut collapsed, mut same, mut shed) = (0, 0, 0);
    for budget in (0..=full.used + 40).rev() {
        let b = base.brief("s-1", budget, now);
        let w = with.brief("s-1", budget, now);
        let (bl, wl) = (body(&b.text), body(&w.text));
        if bl == wl {
            same += 1;
            if b.level > 0 {
                shed += 1;
            }
            continue;
        }
        let ids: Vec<String> = wl.iter().map(|l| id(l)).collect();
        for l in &bl {
            assert!(
                ids.contains(&id(l)),
                "budget {budget}: {l:?} shed before the transcript's lines\nbase:\n{}\nwith:\n{}",
                b.text,
                w.text
            );
        }
        assert!(
            w.text.contains("tool shell") || w.text.contains("tok ↑"),
            "budget {budget}: it differs by the transcript's lines: {}",
            w.text
        );
        if w.text.contains(" · +2 open") {
            collapsed += 1;
        }
    }
    assert!(
        full.text.contains("tok ↑") && full.text.contains("p3 tool shell error"),
        "the transcript's lines show when they fit: {}",
        full.text
    );
    assert!(
        collapsed > 0,
        "the open steps collapse to the newest and a count"
    );
    assert!(shed > 0, "past the transcript, the brief sheds as it did");
    assert!(same > shed, "and reads as it did");
}
