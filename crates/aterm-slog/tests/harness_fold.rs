// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fold's policy for `src=harness`, the session's own program speaking over its record
//! stream: its phase stands against the shell's status, a need it raises is observed, ranks
//! after what aterm observed and before every other claim, and its clear lifts only its own.

use aterm_slog::{
    Actor, Conf, Fields, Key, Kind, Log, Mark, Node, Open, ShortId, Src, Stamp, observed_need_words,
};

/// Wall time of the test epoch.
const WALL0: u64 = 1_760_000_000_000;

fn at(ms: u64) -> Stamp {
    Stamp::new(WALL0 + ms, ms)
}

fn sid(s: &str) -> Actor {
    Actor::Sid(ShortId::new(s).expect("a tab id"))
}

/// The tab's own session, the author of every record its harness's stream writes.
fn own() -> Actor {
    sid("t0000000001")
}

/// A session with an agent run and a turn open: `(run, turn)`.
fn session(log: &mut Log) -> (Node, Node) {
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
    let turn = log
        .open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
            at(1),
        )
        .expect("the turn opens");
    (run, turn)
}

fn word(w: &str) -> Fields {
    Fields::new().word(Key::Word, w).expect("a word")
}

/// `mark agent word=<w>` at `run`, from `src`.
fn agent_mark(log: &mut Log, run: Node, src: Src, conf: Conf, w: &str, ms: u64) {
    log.mark(
        Mark::new(Kind::Agent, src, conf)
            .at(run)
            .by(own())
            .fields(word(w)),
        at(ms),
    )
    .expect("the agent mark");
}

/// The shell's `mark status phase=running`.
fn shell_status(log: &mut Log, ms: u64) {
    log.mark(
        Mark::new(Kind::Status, Src::Status, Conf::Strong)
            .fields(Fields::new().word(Key::Phase, "running").expect("phase")),
        at(ms),
    )
    .expect("the status mark");
}

/// The confidence a producer writes at `src`: a consumer's and a harness's marks are claims.
fn conf_of(src: Src) -> Conf {
    if matches!(src, Src::Mark | Src::Harness) {
        Conf::Claim
    } else {
        Conf::Exact
    }
}

/// `mark need word=<w>` from `src` by `by`.
fn need(log: &mut Log, src: Src, by: Actor, w: &str, ms: u64) {
    log.mark(
        Mark::new(Kind::Need, src, conf_of(src))
            .by(by)
            .fields(word(w)),
        at(ms),
    )
    .expect("the need mark");
}

/// `mark clear` of every need, from `src` by `by`.
fn clear(log: &mut Log, src: Src, by: Actor, ms: u64) {
    log.mark(Mark::new(Kind::Clear, src, conf_of(src)).by(by), at(ms))
        .expect("the clear mark");
}

/// Each standing need as `(word, src, by)`, most urgent first.
fn needs(log: &Log) -> Vec<(&'static str, Src, Actor)> {
    log.state()
        .needs
        .iter()
        .flatten()
        .map(|n| (n.word, n.src, n.by))
        .collect()
}

/// D11: a state the harness wrote as `mark agent` stands when the shell next marks its status,
/// as the screen reader's does. NEGATIVE CONTROLS: the same phase from a consumer's mark is
/// displaced, and so is the harness's own `-` (the program ended; the shell's phase is the
/// tab's again).
#[test]
fn a_harness_phase_is_not_displaced_by_a_shell_status_mark() {
    let mut log = Log::new(0);
    let (run, _) = session(&mut log);
    agent_mark(&mut log, run, Src::Harness, Conf::Claim, "busy", 10);
    shell_status(&mut log, 20);
    let phase = &log.state().phase;
    assert_eq!(
        (phase.word, phase.src, phase.conf),
        ("busy", Src::Harness, Conf::Claim),
        "the harness's phase stands against the shell's status"
    );

    agent_mark(&mut log, run, Src::Harness, Conf::Claim, "-", 30);
    shell_status(&mut log, 40);
    let phase = &log.state().phase;
    assert_eq!(
        (phase.word, phase.src),
        ("running", Src::Status),
        "CONTROL: a harness phase of `-` gives way to the shell's"
    );

    let mut other = Log::new(0);
    let (run, _) = session(&mut other);
    agent_mark(&mut other, run, Src::Mark, Conf::Claim, "busy", 10);
    shell_status(&mut other, 20);
    let phase = &other.state().phase;
    assert_eq!(
        (phase.word, phase.src),
        ("running", Src::Status),
        "CONTROL: a phase from any source but the screen and the harness is displaced"
    );
}

/// A need the session's own harness raised (a box it wrote as a step, a question it marked) is
/// OBSERVED though the stream writes it `conf=claim`: `brief`'s session line names it, no
/// `claim need` line does, and its word is one `link notify --on need:` takes. NEGATIVE
/// CONTROLS: the same step from a consumer's mark source is a claim, and a harness's `help`
/// (no word a fold observes) stays one.
#[test]
fn a_harness_need_is_observed_for_brief_and_notify() {
    let approval = |src: Src| {
        let mut log = Log::new(0);
        let (_, turn) = session(&mut log);
        log.open(
            Open::new(Kind::Approval, src, Conf::Claim)
                .under(turn)
                .by(own())
                .fields(Fields::new().word(Key::Box, "bash").expect("box")),
            at(10),
        )
        .expect("the approval step");
        log
    };
    let now = WALL0 + 40_000;

    let mut log = approval(Src::Harness);
    let top = log.state().top_observed_need().expect("an observed need");
    assert_eq!(
        (top.word, top.src, top.conf, top.by),
        ("approval:bash", Src::Harness, Conf::Claim, own())
    );
    assert!(top.observed());
    assert!(
        observed_need_words().any(|w| w == top.word),
        "notify takes the word"
    );
    let text = log.brief("t0000000001", 600, now).text;
    let line = text.lines().nth(1).unwrap_or("");
    assert!(
        line.contains("need=approval:bash") && line.contains("[harness?]"),
        "the session line names the harness's need and where it came from: {text}"
    );
    assert!(
        !text.lines().any(|l| l.starts_with("claim need ")),
        "no claim line: {text}"
    );

    need(&mut log, Src::Harness, own(), "question", 20);
    need(&mut log, Src::Harness, own(), "help", 30);
    let observed: Vec<&str> = log
        .state()
        .needs
        .iter()
        .flatten()
        .filter(|n| n.observed())
        .map(|n| n.word)
        .collect();
    assert_eq!(
        observed,
        ["approval:bash", "question"],
        "a harness's mark raises a word its program shows, observed; CONTROL: its `help` is no \
         observation"
    );
    let text = log.brief("t0000000001", 600, now).text;
    assert!(
        text.lines()
            .any(|l| l.starts_with("claim need help by=t0000000001 ") && l.ends_with("[?]")),
        "CONTROL: the harness's `help` is a claim line: {text}"
    );
    need(&mut log, Src::Harness, own(), "exited", 40);
    assert!(
        !log.state()
            .needs
            .iter()
            .flatten()
            .any(|n| n.word == "exited"),
        "CONTROL: what only aterm sees, no harness mark raises"
    );

    let claimed = approval(Src::Mark);
    assert_eq!(
        needs(&claimed),
        [("approval:bash", Src::Mark, own())],
        "PRECONDITION: the control's need stands"
    );
    assert!(
        claimed.state().top_observed_need().is_none(),
        "CONTROL: the same step from a consumer's source is not observed"
    );
    let text = claimed.brief("t0000000001", 600, now).text;
    assert!(
        !text.contains("need=approval:bash")
            && text
                .lines()
                .any(|l| l.starts_with("claim need approval:bash by=t0000000001 ")),
        "CONTROL: it is a claim line, never the session line's need: {text}"
    );
}

/// RANK: a harness's need sorts after every need aterm observed (a strong stall, the exact
/// attention flag) and before every other claim (a proven author's, an Owner client's, another
/// session's, the session's own consumer mark), whatever order they arrived in. CLEAR: a
/// `clear` from `src=harness` lifts the needs the stream raised and nothing else, not even the
/// same author's `src=mark` need nor an observed step's. NEGATIVE CONTROLS: the same author's
/// `src=mark` clear lifts its own mark's need and no harness need, and a person's clear from
/// another source lifts what is left, so each need the harness's clear left standing was one a
/// clear can lift.
#[test]
fn a_harness_clear_lifts_only_harness_needs_and_ranks_below_exact_and_strong() {
    let build = || {
        let mut log = Log::new(0);
        let (_, turn) = session(&mut log);
        let other = sid("t0000000002");
        need(&mut log, Src::Mark, own(), "help", 2);
        need(&mut log, Src::Mark, other, "help", 3);
        need(&mut log, Src::Mark, Actor::Ctl, "help", 4);
        need(&mut log, Src::Mark, Actor::Harness, "decision", 5);
        need(&mut log, Src::Harness, own(), "question", 6);
        need(&mut log, Src::Harness, own(), "review", 7);
        need(&mut log, Src::Meta, Actor::Aterm, "escalated", 8);
        log.open(
            Open::new(Kind::Stall, Src::Screen, Conf::Strong).under(turn),
            at(9),
        )
        .expect("the stall step");
        log
    };
    let other = sid("t0000000002");
    let stall = ("unresponsive", Src::Screen, Actor::Nobody);
    let flag = ("escalated", Src::Meta, Actor::Aterm);
    let claims = [
        ("decision", Src::Mark, Actor::Harness),
        ("help", Src::Mark, Actor::Ctl),
        ("help", Src::Mark, own()),
        ("help", Src::Mark, other),
    ];

    let mut log = build();
    let mut ranked = vec![
        stall,
        flag,
        ("question", Src::Harness, own()),
        ("review", Src::Harness, own()),
    ];
    ranked.extend(claims);
    assert_eq!(
        needs(&log),
        ranked,
        "aterm's observations, then the harness's needs, then every other claim"
    );

    clear(&mut log, Src::Harness, own(), 20);
    let mut left = vec![stall, flag];
    left.extend(claims);
    assert_eq!(
        needs(&log),
        left,
        "the harness's clear lifts the needs its stream raised and no other"
    );
    clear(&mut log, Src::Ctl, Actor::Human, 30);
    assert_eq!(
        needs(&log),
        [stall],
        "CONTROL: every mark need the harness's clear left was liftable; a step's own need \
         ends only with its step"
    );

    let mut log = build();
    clear(&mut log, Src::Mark, own(), 20);
    let after: Vec<_> = ranked
        .iter()
        .copied()
        .filter(|n| *n != ("help", Src::Mark, own()))
        .collect();
    assert_eq!(
        needs(&log),
        after,
        "CONTROL: the same author's consumer clear lifts its consumer need and no harness need"
    );
}
