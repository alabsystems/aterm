// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Hand-written verdict sequences through the real tracker (design §2.4,
//! §10 slice 1). Every sequence goes through `conformance_turn::drive`, so
//! each step is also checked against the derived `TurnTracker` model; the
//! assertions here are about the tree the log ends up holding. Every host
//! call is a checked [`Step`], so each test's path is its `.seq` twin's
//! (`seq_twins`, plan 5B-3, T29).

use aterm_slog::{
    Actor, BoxWord, Close, Conf, Key, Kind, Lvl, Op, Out, Rec, RunOut, ShortId, Src, TurnOut,
    Verdict, WallKind, Words, encode,
};

use crate::support::WALL0;

use crate::conformance_turn::{Rig, Step, checked, drive};

fn records(rig: &Rig) -> Vec<Rec> {
    rig.log.window().cloned().collect()
}

fn closes(rig: &Rig, lvl: Lvl) -> Vec<Rec> {
    records(rig)
        .into_iter()
        .filter(|r| r.op == Op::Close && r.node.lvl == lvl)
        .collect()
}

fn opens(rig: &Rig, kind: Kind) -> Vec<Rec> {
    records(rig)
        .into_iter()
        .filter(|r| r.op == Op::Open && r.k == kind)
        .collect()
}

fn words(rig: &Rig) -> String {
    rig.log
        .fold()
        .clause(rig.stamp().wall_ms)
        .map_or_else(String::new, |c| c.words.as_str().to_string())
}

use Step::{Half, See};

/// A prompt box as the glue publishes it: the word and its detail (the
/// box's kind), which the tracker reads for a wall's reset only.
fn shown(detail: &str) -> Step {
    let v = Verdict::Prompt(BoxWord::from_detail(detail));
    Step::SeeWith(v, ShortId::new(detail).expect("a detail"))
}

/// A Claude Code turn with an approval box: the box is a step of the turn,
/// its need lives while it is up, and the turn's close counts it.
#[test]
pub(crate) fn claude_turn_with_an_approval_box() {
    let mut rig = drive(&[See(Verdict::Busy), Half, shown("bash:not-read-only")]);
    let need = rig.log.state().top_need().expect("the box needs someone");
    assert_eq!(need.word, "approval:bash");
    assert_eq!(words(&rig), "approval · bash");
    let step = opens(&rig, Kind::Approval)
        .pop()
        .expect("the box is a step");
    assert_eq!(step.par.map(|p| p.lvl), Some(Lvl::Turn));
    assert_eq!(step.f.get_word(Key::Box), Some("bash:not-read-only"));

    for s in [See(Verdict::Busy), Half, See(Verdict::Idle), Half, Half] {
        apply_on(&mut rig, s);
    }
    assert!(
        rig.log.state().top_need().is_none(),
        "leaving the box lifted it"
    );
    let turn = closes(&rig, Lvl::Turn).pop().expect("the turn closed");
    assert_eq!(turn.out(), Some("done"));
    assert_eq!(turn.f.get_u(Key::Appr), Some(1));
    assert_eq!(turn.f.get_u(Key::Steps), Some(1));
    let step = closes(&rig, Lvl::Step).pop().expect("the box closed");
    assert_eq!(step.out(), Some("passed"));
    assert_eq!(words(&rig), "idle");
}

/// The AskUserQuestion dialog mid-turn is a `question` step: it never ends
/// the turn, and its need is `question`.
#[test]
pub(crate) fn question_dialog_mid_turn_stays_in_the_turn() {
    let mut rig = drive(&[See(Verdict::Busy), shown("question"), Half, Half, Half]);
    assert!(rig.tracker.turn().is_some(), "the dialog stays in the turn");
    assert_eq!(rig.log.state().top_need().map(|n| n.word), Some("question"));
    assert_eq!(opens(&rig, Kind::Question).len(), 1);
    assert!(opens(&rig, Kind::Approval).is_empty(), "never an approval");
    for s in [See(Verdict::Busy), See(Verdict::Idle), Half, Half] {
        apply_on(&mut rig, s);
    }
    let turn = closes(&rig, Lvl::Turn).pop().expect("the turn closed");
    assert_eq!(turn.out(), Some("done"));
    assert_eq!(turn.f.get_u(Key::Qs), Some(1));
}

/// A usage wall at rest closes its turn `out=wall:usage-session`; the wall
/// stays open on the run, so its need holds until the wall lifts.
#[test]
pub(crate) fn usage_wall_at_rest_closes_the_turn_and_holds_its_need() {
    let reset = ShortId::new("3:15pm").expect("a detail");
    let mut rig = drive(&[
        See(Verdict::Busy),
        Step::SeeWith(Verdict::Wall(WallKind::UsageSession), reset),
        Step::Wait(500),
    ]);
    assert!(rig.tracker.turn().is_some(), "half a settle closes nothing");
    apply_on(&mut rig, Step::Wait(500));
    let turn = closes(&rig, Lvl::Turn)
        .pop()
        .expect("the wall closed the turn");
    assert_eq!(turn.out(), Some("wall:usage-session"));
    assert_eq!(turn.f.get_u(Key::Walls), Some(1));
    let walls = opens(&rig, Kind::Wall);
    assert_eq!(walls.len(), 2, "the turn's wall, then the run's");
    let on_run = walls.last().expect("reopened");
    assert_eq!(on_run.par.map(|p| p.lvl), Some(Lvl::Run));
    assert_eq!(on_run.f.get_u(Key::Orphan), Some(1));
    assert_eq!(on_run.f.get_str(Key::Detail), Some("3:15pm"));
    // The run's wall continues the turn's: one wall, its need dated when
    // it first showed.
    let in_turn = walls.first().expect("the turn's wall");
    assert_eq!(
        on_run.f.get_str(Key::Cont),
        Some(in_turn.node.to_string().as_str())
    );
    let need = rig.log.state().top_need().expect("the wall needs someone");
    assert_eq!(need.word, "wall:usage-session");
    assert_eq!(need.t, in_turn.t, "the need's age is the wall's");
    assert_eq!(
        turn.t, in_turn.t,
        "the turn closed when the wall came, not at the tick"
    );
    assert_eq!(words(&rig), "limit · 3:15pm");
    assert_eq!(rig.log.state().totals.steps, 1, "one wall, counted once");

    // The limit resets: busy lifts the wall and opens the next turn.
    apply_on(&mut rig, See(Verdict::Busy));
    assert!(rig.log.state().top_need().is_none());
    assert_eq!(opens(&rig, Kind::Turn).len(), 2);
    apply_on(&mut rig, Step::Wait(1000));
    let stamp = rig.stamp();
    rig.log
        .close(
            rig.run,
            Close::new(Out::Run(RunOut::Ok), Src::Status, Conf::Strong),
            stamp,
        )
        .expect("the run closes");
    let run = closes(&rig, Lvl::Run).pop().expect("the run's close");
    assert_eq!(
        (run.f.get_u(Key::Steps), run.f.get_u(Key::Walls)),
        (Some(1), Some(1)),
        "the run's rollup counts the wall once"
    );
}

/// The agent leaving (`-`) over an open box closes the box `out=gone`, not
/// `passed`: nothing on screen moved past it.
#[test]
pub(crate) fn agent_leaving_closes_its_box_gone() {
    let rig = drive(&[
        See(Verdict::Busy),
        shown("bash:not-read-only"),
        See(Verdict::Gone),
    ]);
    let step = closes(&rig, Lvl::Step).pop().expect("the box closed");
    assert_eq!((step.k, step.out()), (Kind::Approval, Some("gone")));
    let turn = closes(&rig, Lvl::Turn).pop().expect("the turn closed");
    assert_eq!(turn.out(), Some("gone"));
    assert!(step.seq < turn.seq, "innermost first");
}

/// A settled turn is dated when it came to rest: busy for 1 s, then idle,
/// closes `t=` the idle and `dur_ms=1000`, however late the wake.
#[test]
pub(crate) fn a_settled_turn_is_dated_at_its_rest() {
    let mut rig = drive(&[See(Verdict::Busy), Half, Half, See(Verdict::Idle)]);
    let idle_at = WALL0 + rig.now;
    for s in [Step::Elapse, Step::Elapse, Step::Elapse, Step::Wake] {
        apply_on(&mut rig, s);
    }
    let turn = closes(&rig, Lvl::Turn).pop().expect("closed");
    assert_eq!(turn.out(), Some("done"));
    assert_eq!((turn.t, turn.f.get_u(Key::DurMs)), (idle_at, Some(1000)));
}

/// A late wake: the idle held past its settle and the next verdict is busy.
/// The idle's turn closes first and the busy opens the next: two
/// exchanges, never one.
#[test]
pub(crate) fn a_late_wake_never_merges_two_turns() {
    let rig = drive(&[
        See(Verdict::Busy),
        See(Verdict::Idle),
        Step::Elapse,
        Step::Elapse,
        Step::Elapse,
        See(Verdict::Busy),
    ]);
    assert_eq!(opens(&rig, Kind::Turn).len(), 2);
    let first = closes(&rig, Lvl::Turn).pop().expect("the first closed");
    assert_eq!(first.out(), Some("done"));
    assert!(rig.tracker.turn().is_some_and(|t| t != first.node));
}

/// An input stall is a `stall` step with need `unresponsive`; it never
/// closes the turn, however long it holds.
#[test]
pub(crate) fn input_stall_never_closes_the_turn() {
    let mut rig = drive(&[See(Verdict::Busy), See(Verdict::Unresponsive)]);
    for _ in 0..20 {
        apply_on(&mut rig, Half);
    }
    assert!(rig.tracker.turn().is_some());
    assert_eq!(
        rig.log.state().top_need().map(|n| n.word),
        Some("unresponsive")
    );
    assert_eq!(words(&rig), "not responding");
    assert!(opens(&rig, Kind::Wall).is_empty(), "never a wall step");
    apply_on(&mut rig, See(Verdict::Busy));
    assert!(rig.log.state().top_need().is_none());
    let stall = closes(&rig, Lvl::Step).pop().expect("the stall lifted");
    assert_eq!((stall.k, stall.out()), (Kind::Stall, Some("passed")));
    assert!(closes(&rig, Lvl::Turn).is_empty());
}

/// Codex's 220 ms mid-answer idle must not close the turn: one turn, closed
/// once, by the idle that held.
#[test]
pub(crate) fn codex_mid_answer_idle_does_not_close_the_turn() {
    let mut rig = drive(&[See(Verdict::Busy), See(Verdict::Idle), Step::Wait(220)]);
    apply_on(&mut rig, See(Verdict::Busy));
    apply_on(&mut rig, Step::Wait(2000));
    assert!(rig.tracker.turn().is_some(), "busy cancelled the settle");
    apply_on(&mut rig, See(Verdict::Idle));
    apply_on(&mut rig, Step::Wait(999));
    assert!(rig.tracker.turn().is_some(), "999 ms is not a settle");
    apply_on(&mut rig, Step::Wait(1));
    assert_eq!(opens(&rig, Kind::Turn).len(), 1);
    let turn = closes(&rig, Lvl::Turn);
    assert_eq!(turn.len(), 1);
    assert_eq!(turn[0].out(), Some("done"));
}

/// An interrupted turn closes `out=interrupted` at its idle, and the clause
/// says so.
#[test]
pub(crate) fn interrupted_turn_closes_interrupted() {
    let mut rig = drive(&[See(Verdict::Busy), Step::Interrupted]);
    for s in [See(Verdict::Idle), Half, Half] {
        apply_on(&mut rig, s);
    }
    let turn = closes(&rig, Lvl::Turn).pop().expect("closed");
    assert_eq!(
        turn.out(),
        Some(aterm_slog::Out::Turn(TurnOut::Interrupted).word())
    );
    assert_eq!(words(&rig), "interrupted");
}

/// `unknown` is never idle: the turn stays open through it.
#[test]
pub(crate) fn unknown_never_closes_a_turn() {
    let mut rig = drive(&[See(Verdict::Busy), See(Verdict::Idle), Half]);
    apply_on(&mut rig, See(Verdict::Unknown));
    for _ in 0..10 {
        apply_on(&mut rig, Half);
    }
    assert!(rig.tracker.turn().is_some());
    apply_on(&mut rig, See(Verdict::Gone));
    let turn = closes(&rig, Lvl::Turn).pop().expect("closed");
    assert_eq!(turn.out(), Some("gone"));
}

/// A prose question closes the turn `asked` at `conf=heuristic`.
#[test]
pub(crate) fn prose_question_closes_asked_heuristic() {
    let rig = drive(&[See(Verdict::Busy), See(Verdict::Question), Half, Half]);
    let turn = closes(&rig, Lvl::Turn).pop().expect("closed");
    assert_eq!(turn.out(), Some("asked"));
    assert_eq!(turn.conf, aterm_slog::Conf::Heuristic);
}

/// `ctl turn` attributes the turn it starts (`by=`, `ledger=`); input older
/// than 5 s attributes nothing.
#[test]
pub(crate) fn ctl_turn_and_input_attribute_the_opening_turn() {
    let mut rig = drive(&[Step::Ctl, Step::Wait(300), See(Verdict::Busy)]);
    let ledger = rig.ledger;
    let t = opens(&rig, Kind::Turn).pop().expect("opened");
    assert_eq!(t.by, Actor::Harness);
    assert_eq!(t.f.get_u(Key::Ledger), Some(ledger));
    let line = encode(&t, aterm_slog::Tier::Wire).expect("fits");
    assert!(line.contains(" by=harness"), "{line}");

    for s in [
        See(Verdict::Gone),
        Step::Input(Actor::Human),
        Step::Wait(6000),
        See(Verdict::Busy),
    ] {
        apply_on(&mut rig, s);
    }
    let t = opens(&rig, Kind::Turn).pop().expect("opened");
    assert_eq!(t.by, Actor::Nobody, "input 6 s old attributes nothing");
}

/// A harness answering a prose question within the settle: the asked turn
/// closes at once, dated when the agent asked, and the harness's exchange
/// is a turn of its own carrying its `by=` and `ledger=` (probe P8a).
#[test]
pub(crate) fn ctl_turn_while_resting_starts_its_own_turn() {
    let mut rig = drive(&[See(Verdict::Busy), See(Verdict::Question)]);
    let asked_at = WALL0 + rig.now;
    checked(&mut rig, Step::Pass(400));
    checked(&mut rig, Step::Ctl);
    let asked = closes(&rig, Lvl::Turn)
        .pop()
        .expect("the asked turn closed");
    assert_eq!((asked.out(), asked.conf), (Some("asked"), Conf::Heuristic));
    assert_eq!(asked.t, asked_at);
    checked(&mut rig, Step::Pass(200));
    checked(&mut rig, See(Verdict::Busy));
    let mine = opens(&rig, Kind::Turn).pop().expect("the ctl turn's own");
    assert_ne!(mine.node, asked.node);
    assert_eq!(
        (mine.by, mine.f.get_u(Key::Ledger)),
        (Actor::Harness, Some(rig.ledger))
    );
    for s in [See(Verdict::Idle), Half, Half] {
        apply_on(&mut rig, s);
    }
    assert_eq!(closes(&rig, Lvl::Turn).len(), 2, "two exchanges, two turns");

    // An hour later the human types: that turn carries no ledger (P8b).
    checked(&mut rig, Step::Pass(3_600_000));
    checked(&mut rig, Step::Input(Actor::Human));
    checked(&mut rig, See(Verdict::Busy));
    let later = opens(&rig, Kind::Turn).pop().expect("the human's turn");
    assert_eq!((later.by, later.f.get_u(Key::Ledger)), (Actor::Human, None));
}

/// A `ctl turn` queued while the agent is busy labels only a turn that
/// opens within the attribution window, from its sender: an hour later, a
/// human's turn never carries it (probe P8c).
#[test]
pub(crate) fn a_stale_ledger_labels_no_later_turn() {
    let mut rig = drive(&[See(Verdict::Busy)]);
    checked(&mut rig, Step::Ctl);
    for s in [See(Verdict::Idle), Half, Half] {
        apply_on(&mut rig, s);
    }
    assert_eq!(closes(&rig, Lvl::Turn).len(), 1);
    checked(&mut rig, Step::Pass(3_600_000));
    checked(&mut rig, Step::Input(Actor::Human));
    checked(&mut rig, See(Verdict::Busy));
    let later = opens(&rig, Kind::Turn).pop().expect("the human's turn");
    assert_eq!((later.by, later.f.get_u(Key::Ledger)), (Actor::Human, None));

    // Nor does a harness turn opened past the window, though by the sender.
    let mut rig = drive(&[See(Verdict::Busy)]);
    checked(&mut rig, Step::Ctl);
    for s in [See(Verdict::Idle), Half, Half] {
        apply_on(&mut rig, s);
    }
    checked(&mut rig, Step::Pass(6_000));
    checked(&mut rig, See(Verdict::Busy));
    let late = opens(&rig, Kind::Turn).pop().expect("a later turn");
    assert_eq!(late.f.get_u(Key::Ledger), None);
}

/// The clause's subject is cut on a cluster boundary until ` · <age>` fits.
#[test]
fn clause_words_fit_their_bytes() {
    let w = Words::fit(&"é".repeat(40), "4m");
    assert!(w.as_str().len() <= 48);
    assert!(w.as_str().ends_with(" · 4m"), "{:?}", w.as_str());
    // A combining accent is never cut from its base.
    let combining = "e\u{301}".repeat(20);
    let w = Words::fit(&combining, "12m");
    let head = w.as_str().trim_end_matches(" · 12m");
    assert!(
        !head.ends_with('e'),
        "a base split from its accent: {head:?}"
    );
}

/// One more host step, checked against the model like `drive`'s.
fn apply_on(rig: &mut Rig, step: Step) {
    crate::conformance_turn::checked(rig, step);
}
