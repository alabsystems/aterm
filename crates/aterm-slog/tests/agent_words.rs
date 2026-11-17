// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The agent words `stopped` and a bare `wall`: in the `word=` vocabulary, typed verdicts of
//! their own; `stopped` is no step of the tracker's, and a bare `wall` is a wall like any other.

use aterm_slog::vocab::WORD_WORDS;
use aterm_slog::{
    Actor, BoxWord, Conf, Fields, Key, Kind, Log, Mark, Open, Src, Stamp, TurnTracker, Verdict,
    WallKind,
};

fn at(ms: u64) -> Stamp {
    Stamp::new(1_760_000_000_000 + ms, ms)
}

fn need_words(log: &Log) -> Vec<&'static str> {
    log.state().needs.iter().flatten().map(|n| n.word).collect()
}

/// D13, D14: `stopped` and a wall with no kind are agent words. A `word=` field takes each, so
/// the log row and the fold's phase read the word and not `unknown`; each is a verdict of its
/// own that reads back as its word; and `stopped` leaves an open box as it was (what a bare
/// wall does is `a_wall_with_no_kind_is_a_wall_step_a_need_and_a_settle_cause`). NEGATIVE CONTROLS: a word outside the
/// vocabulary is refused by the field and reads `unknown` as a verdict, a wall of an unknown
/// kind is still `unknown` (no kind is guessed), a wall with a kind is still that wall, and
/// `busy` does end the box `stopped` left open.
#[test]
fn stopped_and_wall_are_agent_words() {
    for (w, v) in [("stopped", Verdict::Stopped), ("wall", Verdict::Walled)] {
        assert!(WORD_WORDS.contains(&w), "{w} is a word= word");
        assert!(
            Fields::new().word(Key::Word, w).is_ok(),
            "word={w} is taken"
        );
        let read = Verdict::from_agent_word(w, None);
        assert_eq!(read, v);
        assert_ne!(read, Verdict::Unknown);
        assert_eq!(read.word(), w, "{w} reads back");
    }
    assert!(
        Fields::new().word(Key::Word, "wall:new-kind").is_err(),
        "CONTROL: a word outside the vocabulary is refused"
    );
    assert_eq!(
        Verdict::from_agent_word("wall:new-kind", None),
        Verdict::Unknown,
        "CONTROL: an unknown wall kind is not the bare wall"
    );
    assert_eq!(
        Verdict::from_agent_word("wall:usage-session", None),
        Verdict::Wall(WallKind::UsageSession),
        "CONTROL: a wall with a kind is unchanged"
    );
    assert_eq!(
        Verdict::from_agent_word("wall:unresponsive", Some("stopped")),
        Verdict::Unresponsive,
        "CONTROL: the input stall is unchanged"
    );

    let mut log = Log::new(0);
    log.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        Actor::Aterm,
        Fields::new(),
        at(0),
    )
    .expect("the session opens");
    let run = log
        .open(
            Open::new(Kind::Agent, Src::Status, Conf::Strong)
                .fields(Fields::new().tok(Key::Agent, "claude").expect("agent")),
            at(0),
        )
        .expect("the agent run opens");
    for (ms, w) in [(10, "wall"), (20, "stopped")] {
        log.mark(
            Mark::new(Kind::Agent, Src::Harness, Conf::Claim)
                .at(run)
                .fields(Fields::new().word(Key::Word, w).expect("word")),
            at(ms),
        )
        .expect("the agent mark");
        assert_eq!(log.state().phase.word, w, "the fold's phase reads {w}");
    }

    let mut tr = TurnTracker::new(run);
    tr.observe(&mut log, Verdict::Busy, None, at(100));
    tr.observe(
        &mut log,
        Verdict::Prompt(BoxWord::from_detail("bash")),
        None,
        at(200),
    );
    let standing = need_words(&log);
    assert_eq!(standing.len(), 1, "PRECONDITION: the box's need stands");
    assert!(standing[0].starts_with("approval:"), "{standing:?}");
    tr.observe(&mut log, Verdict::Stopped, None, at(300));
    assert_eq!(
        need_words(&log),
        standing,
        "`stopped` closes no open box and opens no step"
    );
    tr.observe(&mut log, Verdict::Busy, None, at(500));
    assert!(
        need_words(&log).is_empty(),
        "CONTROL: a word that ends the box does end it"
    );
}

/// D13: a wall whose kind nothing names is TREATED AS EVERY WALL IS. `Verdict::Walled` opens a
/// wall step (with no `wall=`, since no kind is guessed), the step raises the observed need
/// `wall`, and the word held for the settle time closes the open turn `out=wall`, the wall step
/// standing on past it. The tracker carried across an update keeps the step and its settle.
/// NEGATIVE CONTROLS: `stopped` and `unknown` open no step, raise no need and arm no settle on
/// the same turn; a wall with a kind still writes `wall=<kind>`, raises `wall:<kind>` and
/// closes its turn `out=wall:<kind>`; and a kind the screen names later replaces the bare step.
#[test]
fn a_wall_with_no_kind_is_a_wall_step_a_need_and_a_settle_cause() {
    use aterm_slog::{Op, ScreenStep, SettleCause, TurnOut, settle_out};

    let walled = |v: Verdict| {
        let mut log = Log::new(0);
        log.open_session(
            Kind::Spawn,
            Src::Aterm,
            Conf::Exact,
            Actor::Aterm,
            Fields::new(),
            at(0),
        )
        .expect("the session opens");
        let run = log
            .open(
                Open::new(Kind::Agent, Src::Status, Conf::Strong)
                    .fields(Fields::new().tok(Key::Agent, "claude").expect("agent")),
                at(0),
            )
            .expect("the agent run opens");
        let mut tr = TurnTracker::new(run);
        tr.observe(&mut log, Verdict::Busy, None, at(100));
        let wake = tr.observe(&mut log, v, None, at(200));
        (log, tr, wake)
    };
    let wall_steps = |log: &Log| -> Vec<(Op, Option<&'static str>)> {
        log.window()
            .filter(|r| r.k == Kind::Wall)
            .map(|r| (r.op, r.f.get_word(Key::Wall)))
            .collect()
    };
    let turn_outs = |log: &Log| -> Vec<&'static str> {
        log.window()
            .filter(|r| r.k == Kind::Turn && r.op == Op::Close)
            .filter_map(aterm_slog::Rec::out)
            .collect()
    };

    // The bare wall: a step, its need, the settle.
    let (mut log, mut tr, wake) = walled(Verdict::Walled);
    assert_eq!(
        wall_steps(&log),
        [(Op::Open, None)],
        "a wall step, no kind guessed"
    );
    assert_eq!(need_words(&log), ["wall"]);
    let need = log
        .state()
        .needs
        .iter()
        .flatten()
        .next()
        .cloned()
        .expect("the need");
    assert!(need.observed(), "the step's need is observed: {need:?}");
    assert_eq!(
        aterm_slog::words::need_rank("wall"),
        aterm_slog::words::need_rank("wall:spend")
    );
    assert!(
        aterm_slog::fold::observed_need_words().any(|w| w == "wall"),
        "`link notify --on need:wall` is a word a reader can ask for"
    );
    assert!(wake.is_some(), "the settle is armed");
    assert_eq!(
        tr.view(200).step.map(|(_, what, _)| what),
        Some(ScreenStep::Wall(None))
    );
    assert!(turn_outs(&log).is_empty(), "the turn is still open");
    // Carried across an update, the step and its settle are kept.
    let carried = TurnTracker::from_carry_line(&tr.carry_line(at(300)), at(300))
        .expect("a bare wall step is carried");
    assert_eq!(
        carried.view(300).step.map(|(_, what, _)| what),
        Some(ScreenStep::Wall(None))
    );
    assert_eq!(carried.wake_at(), tr.wake_at());
    tr.tick(&mut log, at(200 + aterm_slog::SETTLE_MS));
    assert_eq!(
        turn_outs(&log),
        ["wall"],
        "the turn settled as a walled turn"
    );
    assert_eq!(
        settle_out(SettleCause::Wall(None), false),
        (TurnOut::Wall(None), Conf::Strong)
    );
    assert_eq!(need_words(&log), ["wall"], "the wall stands past its turn");
    assert_eq!(log.state().totals.walled, 1, "counted as a walled turn");
    // A kind named later replaces the bare step.
    tr.observe(
        &mut log,
        Verdict::Wall(WallKind::UsageSession),
        None,
        at(5_000),
    );
    assert_eq!(need_words(&log), ["wall:usage-session"]);
    assert_eq!(
        tr.view(5_000).step.map(|(_, what, _)| what),
        Some(ScreenStep::Wall(Some(WallKind::UsageSession)))
    );

    // NEGATIVE CONTROLS: the two words that are no wall.
    for v in [Verdict::Stopped, Verdict::Unknown] {
        let (mut log, mut tr, wake) = walled(v);
        assert!(wall_steps(&log).is_empty(), "{v:?} opens no wall step");
        assert!(need_words(&log).is_empty(), "{v:?} raises no need");
        assert!(wake.is_none(), "{v:?} arms no settle");
        tr.tick(&mut log, at(200 + aterm_slog::SETTLE_MS));
        assert!(turn_outs(&log).is_empty(), "{v:?} settles no turn");
    }
    // NEGATIVE CONTROL: a wall with a kind is written, needed and settled by its kind.
    let (mut log, mut tr, _) = walled(Verdict::Wall(WallKind::Spend));
    assert_eq!(wall_steps(&log), [(Op::Open, Some("spend"))]);
    assert_eq!(need_words(&log), ["wall:spend"]);
    tr.tick(&mut log, at(200 + aterm_slog::SETTLE_MS));
    assert_eq!(turn_outs(&log), ["wall:spend"]);
}
