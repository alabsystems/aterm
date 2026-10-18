// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `brief` (§7.4): its shape, its budget and its shedding order.

use aterm_slog::{
    Actor, BoxWord, Conf, Fields, Key, Kind, Limits, Log, Mark, ShortId, Src, TurnTracker, Verdict,
    WallKind, tokens,
};

use crate::support::{WALL0, agent_session, at};

/// A session of `turns` turns, each with an approval box, a bell mark on
/// every third, the last turn left open on a box; the OSC title set once.
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
            log.mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), at(ms))
                .expect("bell");
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
            // Marks first, then the oldest turn lines: the newest stays.
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

/// Every need and claim is named (§3.7, §7.4): the session line carries the
/// most urgent observed need, a `need` line each other one, and a `claim
/// need` line each need a mark raised; the one-line form counts them in
/// `more=`.
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

/// The open turn's line comes from the fold, not the window: level-aware
/// eviction never takes an open node's open record (it is pinned), and a
/// window that lacks it (a reader's cursor past it) still briefs the turn.
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
    let b = aterm_slog::brief("s-1", without, None, log.state(), 100_000, WALL0 + 60_000);
    let turn = b
        .text
        .lines()
        .find(|l| l.starts_with("t1 open "))
        .unwrap_or_else(|| panic!("no open turn line:\n{}", b.text));
    assert!(turn.contains(" by=harness ledger=7"), "{turn}");
    assert!(turn.contains("· p1 approval edit open"), "{turn}");
}

/// Many walled turns (never ordinary, so never ranges) shed oldest first
/// at level 3 rather than collapsing the whole session to one line.
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

/// THE GAP IS THE READ'S, NEVER INFERRED: a brief over a window narrowed to
/// the records past a cursor (or one node's subtree) reports no loss when its
/// read crossed none, however late its first seq; the gap a read did cross is
/// printed as `log` prints it. NEGATIVE CONTROL: the same narrowed window
/// with the read's gap says it.
#[test]
fn a_narrowed_window_reports_only_the_gap_its_read_crossed() {
    let (log, now) = session(3);
    let past = log.read(
        aterm_slog::Cursor::new(log.ep(), log.last_seq() - 3),
        usize::MAX,
    );
    assert!(past.gap.is_none());
    let b = aterm_slog::brief("s-1", past.recs, past.gap, log.state(), 2000, now);
    assert!(!b.text.contains("gap "), "{}", b.text);
    let stale = log.read(aterm_slog::Cursor::new(log.ep() + 7, 3), usize::MAX);
    let g = stale.gap.expect("another epoch's cursor is reset");
    let b = aterm_slog::brief("s-1", stale.recs, stale.gap, log.state(), 2000, now);
    assert!(
        b.text.contains(&format!(
            "\ngap why=reset lost={} resync_to={}\nmore=",
            g.lost, g.resync_to
        )),
        "{}",
        b.text
    );
}

/// A phase nothing has observed carries no reliability suffix: `?` is a
/// claim's, and nothing claimed anything. NEGATIVE CONTROL: an observed phase
/// keeps its `[src+]`.
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
