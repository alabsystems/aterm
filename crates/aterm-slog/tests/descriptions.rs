// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Descriptions as claims: `desc` marks folded per subject, slot and author, the rank that
//! decides which claim stands, turns behind, lapses, and the carry snapshot's lines.

use aterm_slog::{
    Actor, CarryBudget, ClaimRefused, Close, Conf, DESC_AGENTS, DESC_CARRY_BYTES, DESC_CLAIMS,
    DESC_HEAP_BYTES, DESC_HISTORY, DESC_SLOTS, DESC_TURN_RING, DESC_UNKNOWN_SLOTS, Fields, Fold,
    KNOWN_SLOTS, Key, Kind, Limits, Log, Mark, Node, Open, Out, Rank, Rec, Refused, RunOut,
    SnapError, Src, shown,
};

use crate::support::desc::{agent, desc, listing, say, sid, stands, turn, unsay, writer};
use crate::support::{WALL0, agent_session, at};

/// A person's title stands over a control client's, whichever was written last: the client's claim
/// is recorded and listed under the person's, and takes nothing from it. CONTROL: once the person
/// has cleared the slot, the same ctl claim written again stands.
#[test]
fn a_ctl_title_does_not_replace_a_persons() {
    let mut log = Log::new(1);
    agent_session(&mut log, 0);
    let (person, ctl) = ("release notes", "renamed by a script");
    desc(
        &mut log,
        Actor::Human,
        Src::Mark,
        Node::SESSION,
        say("title", person),
        10,
    );
    desc(
        &mut log,
        Actor::Ctl,
        Src::Mark,
        Node::SESSION,
        say("title", ctl),
        20,
    );
    assert_eq!(stands(&log, "title", 30), Some((person, Rank::Person)));
    let listed: Vec<(&str, Actor, Rank, bool)> = listing(&log, 30)
        .iter()
        .map(|r| (r.claim.value.as_str(), r.claim.by, r.rank, r.standing))
        .collect();
    assert_eq!(
        listed,
        [
            (person, Actor::Human, Rank::Person, true),
            (ctl, Actor::Ctl, Rank::Ctl, false),
        ],
        "both are listed, each with its author"
    );
    desc(
        &mut log,
        Actor::Human,
        Src::Mark,
        Node::SESSION,
        unsay("title"),
        40,
    );
    assert_eq!(
        stands(&log, "title", 45),
        None,
        "a person's withdrawal clears the slot: the script's title goes with their own"
    );
    desc(
        &mut log,
        Actor::Ctl,
        Src::Mark,
        Node::SESSION,
        say("title", ctl),
        50,
    );
    assert_eq!(
        stands(&log, "title", 55),
        Some((ctl, Rank::Ctl)),
        "CONTROL: with no person's claim above it the ctl title stands"
    );
    desc(
        &mut log,
        Actor::Human,
        Src::Mark,
        Node::SESSION,
        say("title", person),
        60,
    );
    assert_eq!(
        stands(&log, "title", 70),
        Some((person, Rank::Person)),
        "written after the ctl claim that stood, the person's stands"
    );
}

/// A slot keeps one claim per author and eight authors: a ninth is inserted and the lowest-ranked
/// claim, the oldest of that rank, is dropped and counted, so a flood of other sessions' claims
/// costs the person, the program and the owner nothing. CONTROL: eight authors drop nothing, and
/// an author who writes again replaces its own claim in place.
#[test]
fn nine_authors_keep_eight_claims() {
    let mut log = Log::new(1);
    agent_session(&mut log, 0);
    let peers: Vec<Actor> = (0..6)
        .map(|i| Actor::Sid(sid(&format!("s-{i}123456789abcdef0123"))))
        .collect();
    let mut ms = 10;
    let mut put = |log: &mut Log, by: Actor, value: &str| {
        ms += 1;
        desc(log, by, Src::Mark, Node::SESSION, say("title", value), ms)
    };
    put(&mut log, Actor::Human, "the person's");
    put(&mut log, Actor::Harness, "the program's");
    put(&mut log, Actor::Ctl, "the owner's");
    for (i, by) in peers[..5].iter().enumerate() {
        put(&mut log, *by, &format!("peer {i}"));
    }
    put(&mut log, peers[2], "peer 2 again");
    let slot = |log: &Log| {
        let s = log.state().desc.subject(Node::SESSION).expect("session");
        let s = s.slot("title").expect("the slot");
        (s.claims.iter().map(|c| c.by).collect::<Vec<_>>(), s.dropped)
    };
    let (authors, dropped) = slot(&log);
    assert_eq!(
        (authors.len(), dropped),
        (DESC_CLAIMS, 0),
        "CONTROL: eight authors, one claim each, nothing dropped"
    );
    writer(
        &mut log,
        "planner",
        Node::SESSION,
        "title",
        "a writer's",
        19,
    );
    let (authors, dropped) = slot(&log);
    assert_eq!((authors.len(), dropped), (DESC_CLAIMS, 1));
    assert!(
        !authors.contains(&peers[0]),
        "the oldest lowest-ranked claim went"
    );
    put(&mut log, peers[5], "a sixth peer");
    let (authors, dropped) = slot(&log);
    assert_eq!((authors.len(), dropped), (DESC_CLAIMS, 2));
    assert!(!authors.contains(&peers[1]) && authors.contains(&peers[5]));
    for by in [Actor::Human, Actor::Harness, Actor::Ctl, Actor::Aterm] {
        assert!(authors.contains(&by), "{by:?} kept its claim");
    }
    assert_eq!(
        stands(&log, "title", 60),
        Some(("the person's", Rank::Person))
    );
    assert_eq!(
        listing(&log, 60).last().map(|r| r.dropped),
        Some(2),
        "a listing says how many the slot dropped"
    );
}

/// Inside one rank the claim that stands goes on standing: a second writer's claim, as current as
/// the first, does not take the slot, not even once the incumbent has written again and so holds
/// the newer record of the two, and neither does it once both fall a turn behind. CONTROL: when
/// the incumbent is behind and the challenger writes a current claim, the challenger stands, and
/// then holds against the first writer in turn.
#[test]
fn a_same_rank_challenger_does_not_displace_a_current_incumbent() {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let slot = "description";
    let mut ms = 0;
    let mut says = |log: &mut Log, name: &str, value: &str| {
        ms += 10;
        writer(log, name, Node::SESSION, slot, value, ms);
        stands(log, slot, ms + 1).map(|(text, _)| text.to_string())
    };
    let stood = |text: &str| Some(text.to_string());
    says(&mut log, "planner", "planning the release");
    assert_eq!(
        says(&mut log, "reviewer", "reviewing the diff"),
        stood("planning the release"),
        "the incumbent holds"
    );
    assert_eq!(
        says(&mut log, "planner", "planning the tests"),
        stood("planning the tests"),
        "and holds with its own newer claim, though the challenger's is now the older"
    );
    turn(&mut log, run, 35);
    let st = shown(log.state(), slot, WALL0 + 38).expect("a description");
    assert_eq!(
        (st.claim.value.as_str(), st.rank, st.behind),
        ("planning the tests", Rank::Writer, Some(1)),
        "both a turn behind: it holds"
    );
    assert_eq!(
        says(&mut log, "reviewer", "reviewing the fix"),
        stood("reviewing the fix"),
        "CONTROL: a current claim displaces an incumbent that is behind"
    );
    assert_eq!(
        says(&mut log, "planner", "planning again"),
        stood("reviewing the fix"),
        "and holds in its turn"
    );
}

/// An agent's claims describe that agent's run and end with it: while it runs its program's title
/// stands over the owner's title for the session, and once the run closes the claim is gone, not
/// merely outranked. CONTROL: the session's own claim, written the same way, outlives the agent.
#[test]
fn an_agents_claims_lapse_when_it_ends() {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    desc(
        &mut log,
        Actor::Ctl,
        Src::Mark,
        Node::SESSION,
        say("title", "the tab"),
        10,
    );
    desc(
        &mut log,
        Actor::Harness,
        Src::Mark,
        run,
        say("title", "fixing the parser"),
        20,
    );
    let st = shown(log.state(), "title", WALL0 + 25).expect("a title");
    assert_eq!(
        (st.scope, st.claim.value.as_str(), st.rank),
        (run, "fixing the parser", Rank::Program)
    );
    assert_eq!(log.state().desc.open_agent(), Some(run));
    log.close(
        run,
        Close::new(Out::Run(RunOut::Ok), Src::Status, Conf::Strong),
        at(30),
    )
    .expect("the agent ends");
    assert!(log.state().desc.subject(run).is_none(), "its claims lapsed");
    assert_eq!(log.state().desc.open_agent(), None);
    assert_eq!(listing(&log, 35).len(), 1, "only the session's is listed");
    assert_eq!(
        stands(&log, "title", 35),
        Some(("the tab", Rank::Ctl)),
        "CONTROL: the session's claim outlives its agent"
    );
}

/// An epoch reset (a successor that carried no records) forgets every run, and an agent's claims
/// with it; the session's claims cross it, as its label and verdict do. CONTROL: up to the reset
/// the agent's claim is the one shown.
#[test]
fn an_epoch_reset_forgets_an_agents_claims_and_keeps_the_sessions() {
    let mut old = Log::new(2);
    let run = agent_session(&mut old, 0);
    desc(
        &mut old,
        Actor::Ctl,
        Src::Mark,
        Node::SESSION,
        say("title", "the tab"),
        10,
    );
    desc(
        &mut old,
        Actor::Harness,
        Src::Mark,
        run,
        say("title", "fixing the parser"),
        20,
    );
    turn(&mut old, run, 30);
    let next = old.last_seq() + 1;
    let new = Log::after_handoff(3, Limits::SESSION, old.id_counters(), next, at(90_000));
    let mut fold = Fold::new(0);
    for r in old.window() {
        fold.apply(r);
    }
    let title = |f: &Fold| {
        shown(f.state(), "title", WALL0 + 95_000).map(|s| (s.scope, s.claim.value.clone()))
    };
    assert_eq!(
        title(&fold),
        Some((run, "fixing the parser".to_string())),
        "CONTROL: the agent's claim stands until the reset"
    );
    for r in new.window() {
        fold.apply(r);
    }
    assert_eq!(fold.state().ep, 3);
    assert!(fold.state().desc.subject(run).is_none());
    assert_eq!(title(&fold), Some((Node::SESSION, "the tab".to_string())));
}

/// A mark written on a turn describes the agent run the turn belongs to, and one written under no
/// agent describes the session. Of the two scopes a reader is shown the better-ranked claim, and a
/// tie goes to the session. CONTROL: before the session has a claim of the same rank, the agent's
/// is the one shown.
#[test]
fn a_claim_on_a_turn_describes_its_agent_and_a_tie_goes_to_the_session() {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let open_turn = log
        .open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
            at(5),
        )
        .expect("a turn");
    desc(
        &mut log,
        Actor::Harness,
        Src::Mark,
        open_turn,
        say("title", "from inside a turn"),
        10,
    );
    let d = &log.state().desc;
    assert!(d.subject(run).and_then(|s| s.slot("title")).is_some());
    assert!(d.subject(Node::SESSION).expect("session").slots.is_empty());
    assert_eq!(
        shown(log.state(), "title", WALL0 + 15).map(|s| s.scope),
        Some(run),
        "CONTROL: the agent's claim is shown while the session has none"
    );
    let cmd = log
        .open(
            Open::new(Kind::Cmd, Src::Shell, Conf::Strong).under(Node::SESSION),
            at(20),
        )
        .expect("a command beside the agent");
    desc(
        &mut log,
        Actor::Harness,
        Src::Mark,
        cmd,
        say("title", "from a command"),
        25,
    );
    let st = shown(log.state(), "title", WALL0 + 30).expect("a title");
    assert_eq!(
        (st.scope, st.claim.value.as_str()),
        (Node::SESSION, "from a command"),
        "under no agent it is the session's, and the session wins the tie"
    );
}

/// A withdrawal (a `desc` mark with no value) by anyone but a person removes its own author's
/// claim and nobody else's: a control client cannot clear a person's title. CONTROL: the person's
/// own withdrawal does.
#[test]
fn a_withdrawal_removes_only_its_authors_claim() {
    let mut log = Log::new(1);
    agent_session(&mut log, 0);
    let mut ms = 10;
    let mut put = |log: &mut Log, by: Actor, f: Fields| {
        ms += 1;
        desc(log, by, Src::Mark, Node::SESSION, f, ms)
    };
    put(&mut log, Actor::Human, say("title", "mine"));
    put(&mut log, Actor::Ctl, say("title", "the script's"));
    put(&mut log, Actor::Ctl, unsay("title"));
    let authors =
        |log: &Log| -> Vec<Actor> { listing(log, 100).iter().map(|r| r.claim.by).collect() };
    assert_eq!(authors(&log), [Actor::Human], "the ctl claim went");
    let own = |log: &Log, by: Actor| {
        let d = &log.state().desc;
        d.claim_of(Node::SESSION, "title", by, Src::Mark, None)
            .map(|c| c.value.clone())
    };
    assert_eq!(
        (own(&log, Actor::Ctl), own(&log, Actor::Human)),
        (None, Some("mine".to_string())),
        "each author is asked for its own claim"
    );
    put(&mut log, Actor::Ctl, unsay("title"));
    assert_eq!(
        authors(&log),
        [Actor::Human],
        "a second ctl withdrawal has no claim of its own to remove"
    );
    assert_eq!(stands(&log, "title", 100), Some(("mine", Rank::Person)));
    put(&mut log, Actor::Human, unsay("title"));
    assert!(
        authors(&log).is_empty() && stands(&log, "title", 100).is_none(),
        "CONTROL: its own author withdraws it"
    );
}

/// Eight authors' titles on the session, one of each rank a claim can have: a person's, the
/// harness's (program), the creator's (a session the log is told created this one), a writer's
/// aterm ran, a control client's, a program's own in-band title, aterm's derived one, and
/// another session's (listed). Returns the next free millisecond.
fn a_title_of_every_rank(log: &mut Log) -> u64 {
    let creator = sid("s-bbbbbbbbbbbbbbbbbbbb");
    let peer = sid("s-cccccccccccccccccccc");
    log.set_desc_ident(None, Some(creator));
    let mark = |log: &mut Log, by: Actor, value: &str, ms: u64| {
        desc(log, by, Src::Mark, Node::SESSION, say("title", value), ms)
    };
    mark(log, Actor::Human, "a person's", 10);
    mark(log, Actor::Harness, "the program's", 11);
    mark(log, Actor::Sid(creator), "its creator's", 12);
    writer(log, "namer", Node::SESSION, "title", "a writer's", 13);
    mark(log, Actor::Ctl, "a script's", 14);
    let inband = Fields::new()
        .tok(Key::Field, "title")
        .and_then(|f| f.text(Key::Title, "vim README.md"))
        .expect("an in-band title");
    desc(log, Actor::Nobody, Src::Engine, Node::SESSION, inband, 15);
    let derived = say("title", "derived from the command");
    desc(log, Actor::Aterm, Src::Aterm, Node::SESSION, derived, 16);
    mark(log, Actor::Sid(peer), "another session's", 17);
    18
}

/// The ranks of the session's `title` claims, best first.
fn title_ranks(log: &Log) -> Vec<Rank> {
    listing(log, 100).iter().map(|r| r.rank).collect()
}

/// A person's withdrawal clears the slot of every claim somebody put on it (the person's own, the
/// program's, the creator's, a writer's, a control client's, another session's) and leaves the
/// two aterm observes or derives itself: the program's in-band title and the derived value. It
/// clears a control client's title even where the person holds no claim of their own. CONTROL:
/// before the withdrawal all eight are held, and the person's claim stood.
#[test]
fn a_persons_withdrawal_clears_what_others_put_on_the_slot() {
    let mut log = Log::new(1);
    agent_session(&mut log, 0);
    let next = a_title_of_every_rank(&mut log);
    assert_eq!(
        title_ranks(&log),
        [
            Rank::Person,
            Rank::Program,
            Rank::Creator,
            Rank::Writer,
            Rank::Ctl,
            Rank::Inband,
            Rank::Derived,
            Rank::Listed,
        ],
        "CONTROL: every author's claim is held"
    );
    assert_eq!(stands(&log, "title", 100).map(|s| s.1), Some(Rank::Person));
    let withdraws = |log: &Log, by: Actor| {
        let d = &log.state().desc;
        d.withdraws(Node::SESSION, "title", by, Src::Mark, None)
    };
    assert!(withdraws(&log, Actor::Human));
    let human = Actor::Human;
    desc(
        &mut log,
        human,
        Src::Mark,
        Node::SESSION,
        unsay("title"),
        next,
    );
    assert_eq!(
        title_ranks(&log),
        [Rank::Inband, Rank::Derived],
        "what aterm observes of the program and its own default stay"
    );
    assert_eq!(
        stands(&log, "title", 100),
        Some(("vim README.md", Rank::Inband))
    );
    assert!(
        !withdraws(&log, Actor::Human),
        "a second withdrawal would remove nothing"
    );
    let say_ctl = say("title", "a script's, again");
    desc(
        &mut log,
        Actor::Ctl,
        Src::Mark,
        Node::SESSION,
        say_ctl,
        next + 1,
    );
    assert_eq!(
        stands(&log, "title", 100),
        Some(("a script's, again", Rank::Ctl))
    );
    assert!(
        withdraws(&log, Actor::Human),
        "the person holds no claim, and their withdrawal would still clear the script's"
    );
    let at_ms = next + 2;
    desc(
        &mut log,
        human,
        Src::Mark,
        Node::SESSION,
        unsay("title"),
        at_ms,
    );
    assert_eq!(title_ranks(&log), [Rank::Inband, Rank::Derived]);
}

/// A control client's withdrawal removes its own claim and no other author's, whoever they are:
/// not a person's, the program's, the creator's, a writer's, an in-band or derived one, or
/// another session's. The same holds for the program's withdrawal. CONTROL: the client's own
/// claim does go, and a person's withdrawal of the same slot removes the others (the test
/// above).
#[test]
fn nobody_but_a_person_withdraws_another_authors_claim() {
    let mut log = Log::new(1);
    agent_session(&mut log, 0);
    let next = a_title_of_every_rank(&mut log);
    let ctl = Actor::Ctl;
    desc(
        &mut log,
        ctl,
        Src::Mark,
        Node::SESSION,
        unsay("title"),
        next,
    );
    assert_eq!(
        title_ranks(&log),
        [
            Rank::Person,
            Rank::Program,
            Rank::Creator,
            Rank::Writer,
            Rank::Inband,
            Rank::Derived,
            Rank::Listed,
        ],
        "CONTROL: the client's own claim went, and only it"
    );
    let harness = Actor::Harness;
    let at_ms = next + 1;
    desc(
        &mut log,
        harness,
        Src::Mark,
        Node::SESSION,
        unsay("title"),
        at_ms,
    );
    assert_eq!(
        title_ranks(&log),
        [
            Rank::Person,
            Rank::Creator,
            Rank::Writer,
            Rank::Inband,
            Rank::Derived,
            Rank::Listed,
        ],
        "the program takes back its own"
    );
    assert_eq!(
        stands(&log, "title", 100),
        Some(("a person's", Rank::Person)),
        "the person's claim stands through both"
    );
    let d = &log.state().desc;
    assert!(
        !d.withdraws(Node::SESSION, "title", ctl, Src::Mark, None),
        "a client that holds no claim would remove nothing"
    );
}

/// A claim covers the log up to its own record at most: `upto=` one past the record it would
/// become is refused and nothing is appended. CONTROL: `upto=` naming the record itself is taken.
#[test]
fn an_upto_ahead_of_the_record_is_refused() {
    let mut log = Log::new(1);
    agent_session(&mut log, 0);
    let next = log.last_seq() + 1;
    let claim = |upto: u64| {
        Mark::new(Kind::Desc, Src::Mark, Conf::Claim)
            .by(Actor::Ctl)
            .at(Node::SESSION)
            .fields(say("title", "ahead").u(Key::Upto, upto).expect("upto"))
    };
    assert_eq!(
        log.mark(claim(next + 1), at(10)),
        Err(Refused::UptoAhead(next + 1))
    );
    assert_eq!(log.last_seq(), next - 1, "nothing was appended");
    assert!(listing(&log, 20).is_empty());
    assert_eq!(
        log.mark(claim(next), at(10)),
        Ok(next),
        "CONTROL: the record itself"
    );
}

/// Turns behind counts the turns closed on the claim's own subject after the seq it covers: every
/// turn for the session, an agent's own turns for an agent. A claim that names an older `upto=` is
/// charged the turns that closed since. CONTROL: the same author naming the current head is
/// behind by none, and one naming no `upto=` at a rank that does not default it is unknown.
#[test]
fn turns_behind_count_on_the_claims_own_subject() {
    let mut log = Log::new(1);
    let first = agent_session(&mut log, 0);
    let second = agent(&mut log, 1);
    let start = log.last_seq();
    let work = |v: &str| say("description", v);
    desc(
        &mut log,
        Actor::Harness,
        Src::Mark,
        Node::SESSION,
        work("the session's"),
        10,
    );
    desc(
        &mut log,
        Actor::Harness,
        Src::Mark,
        first,
        work("the first agent's"),
        11,
    );
    let behind = |log: &Log, scope: Node, by: Actor| {
        listing(log, 100)
            .iter()
            .find(|r| r.scope == scope && r.claim.by == by)
            .map(|r| r.behind)
            .expect("the claim is listed")
    };
    assert_eq!(behind(&log, Node::SESSION, Actor::Harness), Some(0));
    turn(&mut log, second, 20);
    assert_eq!(behind(&log, Node::SESSION, Actor::Harness), Some(1));
    assert_eq!(
        behind(&log, first, Actor::Harness),
        Some(0),
        "another agent's turn is not this agent's"
    );
    turn(&mut log, first, 30);
    assert_eq!(behind(&log, Node::SESSION, Actor::Harness), Some(2));
    assert_eq!(behind(&log, first, Actor::Harness), Some(1));
    let old = work("as of the start").u(Key::Upto, start).expect("upto");
    desc(&mut log, Actor::Ctl, Src::Mark, Node::SESSION, old, 40);
    assert_eq!(
        behind(&log, Node::SESSION, Actor::Ctl),
        Some(2),
        "charged the two turns closed since the seq it covers"
    );
    let head = log.last_seq() + 1;
    let current = work("as of now").u(Key::Upto, head).expect("upto");
    desc(&mut log, Actor::Ctl, Src::Mark, Node::SESSION, current, 50);
    assert_eq!(
        behind(&log, Node::SESSION, Actor::Ctl),
        Some(0),
        "CONTROL: the same author at the head is behind by none"
    );
    desc(
        &mut log,
        Actor::Ctl,
        Src::Mark,
        Node::SESSION,
        work("no upto"),
        60,
    );
    assert_eq!(
        behind(&log, Node::SESSION, Actor::Ctl),
        None,
        "CONTROL: a ctl claim that names no upto is of unknown freshness"
    );
}

/// Two agent runs under one session, `planner`'s claim for the first's `description`, then two
/// turns of the first and eight of the second, which leave none of the first's closes among the
/// store's last eight. The first agent, and the last seq before its turns.
fn an_agent_outrun_by_another(log: &mut Log) -> (Node, u64) {
    let first = agent_session(log, 0);
    let second = agent(log, 1);
    writer(log, "planner", first, "description", "planning", 10);
    let before = log.last_seq();
    turn(log, first, 20);
    turn(log, first, 30);
    for i in 0..DESC_TURN_RING as u64 {
        turn(log, second, 40 + 10 * i);
    }
    (first, before)
}

/// `reviewer`'s claim for `first`'s `description`, covering the log up to `upto`.
fn review(log: &mut Log, first: Node, value: &str, upto: u64, ms: u64) -> u64 {
    let f = say("description", value)
        .text(Key::As, "reviewer")
        .and_then(|f| f.u(Key::Upto, upto))
        .expect("a claim with upto=");
    desc(log, Actor::Aterm, Src::Mark, first, f, ms)
}

/// How far behind `name`'s claim for `first`'s `description` is listed, and whether it stands.
fn reviewed(fold: &Fold, first: Node, name: &str) -> (Option<u32>, bool) {
    let rows = fold.state().desc.listing(WALL0 + 1000);
    rows.iter()
        .find(|r| r.scope == first && r.claim.as_name.as_deref() == Some(name))
        .map(|r| (r.behind, r.standing))
        .expect("the claim is listed")
}

/// An agent's claim is charged its own agent's turn closes however many turns another agent has
/// closed since: each agent subject remembers its own last eight. A claim that covers the log only
/// up to before the agent's two turns reads two behind, and so does not take the slot from an
/// incumbent of its rank that is just as far behind. CONTROL: the same author covering the log up
/// to the head is behind by none and takes the slot.
#[test]
fn another_agents_turns_do_not_hide_an_agents_own_closes() {
    let mut log = Log::new(1);
    let (first, before) = an_agent_outrun_by_another(&mut log);
    assert_eq!(
        reviewed(log.fold(), first, "planner"),
        (Some(2), true),
        "the incumbent fell behind by its own agent's two turns, and by no other's"
    );
    review(&mut log, first, "as of before the turns", before, 200);
    assert_eq!(
        reviewed(log.fold(), first, "reviewer"),
        (Some(2), false),
        "charged the first agent's two closes, which the second's eight did not push out"
    );
    assert_eq!(reviewed(log.fold(), first, "planner"), (Some(2), true));
    let head = log.last_seq() + 1;
    review(&mut log, first, "as of now", head, 210);
    assert_eq!(
        reviewed(log.fold(), first, "reviewer"),
        (Some(0), true),
        "CONTROL: covering the log up to the head it is current, and stands"
    );
}

/// An agent subject's own turn closes cross a snapshot on its `fold desc-sub` line, so a claim
/// written after the update is charged as one written before it would have been. The line is read
/// as strictly as the head line's ring: more than eight closes, or one not past the close before
/// it, refuse the whole snapshot. CONTROL: the snapshot as written reads back to the same state.
#[test]
fn an_agents_turn_closes_cross_a_carry() {
    let mut log = Log::new(1);
    let (first, before) = an_agent_outrun_by_another(&mut log);
    let snap = log.fold().snapshot();
    let sub = format!("fold desc-sub {first} n=2 tr=");
    let at = snap
        .iter()
        .position(|l| l.starts_with(&sub))
        .expect("the agent's subject line carries its closes");
    let closes: Vec<&str> = snap[at][sub.len()..].split(',').collect();
    assert_eq!(closes.len(), 2, "{}", snap[at]);
    let read = |lines: &[String]| {
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        Fold::from_snapshot(&refs)
    };
    let mut back = read(&snap).expect("the snapshot reads");
    assert_eq!(back.state(), log.state(), "CONTROL: the state as it stood");
    assert_eq!(back.snapshot(), snap, "and it writes the same lines");
    let from = log.last_seq();
    review(&mut log, first, "as of before the turns", before, 200);
    let later: Vec<&Rec> = log.window().filter(|r| r.seq > from).collect();
    for r in later {
        back.apply(r);
    }
    assert_eq!(
        reviewed(&back, first, "reviewer"),
        (Some(2), false),
        "after the carry the claim is charged the same two closes"
    );
    assert_eq!(back.state(), log.state(), "folding on alike");
    let with = |ring: &str| {
        let mut lines = snap.clone();
        lines[at] = format!("{sub}{ring}");
        read(&lines).err()
    };
    let nine: Vec<String> = (1..=DESC_TURN_RING as u64 + 1)
        .map(|n| n.to_string())
        .collect();
    assert_eq!(with(&nine.join(",")), Some(SnapError::Bound));
    assert_eq!(
        with(&format!("{0},{0}", closes[0])),
        Some(SnapError::Line),
        "a close not past the one before it"
    );
    assert_eq!(
        with(""),
        Some(SnapError::Line),
        "an empty ring is not written"
    );
    assert_eq!(
        with(&closes.join(",")),
        None,
        "CONTROL: the ring as written reads"
    );
}

/// Inside one rank a claim whose freshness is known and current stands over an incumbent whose
/// freshness nobody knows. CONTROL: a second claim of the same rank that is just as unknown does
/// not displace the first.
#[test]
fn an_unknown_upto_loses_to_a_known_one_inside_a_rank() {
    let mut log = Log::new(1);
    agent_session(&mut log, 0);
    desc(
        &mut log,
        Actor::Human,
        Src::Mark,
        Node::SESSION,
        say("title", "typed in the tab"),
        10,
    );
    desc(
        &mut log,
        Actor::Human,
        Src::Meta,
        Node::SESSION,
        say("title", "set in settings"),
        20,
    );
    assert_eq!(
        stands(&log, "title", 25),
        Some(("typed in the tab", Rank::Person)),
        "CONTROL: unknown against unknown, the incumbent holds"
    );
    let head = log.last_seq() + 1;
    let known = say("title", "set just now")
        .u(Key::Upto, head)
        .expect("upto");
    desc(&mut log, Actor::Human, Src::Meta, Node::SESSION, known, 30);
    let st = shown(log.state(), "title", WALL0 + 35).expect("a title");
    assert_eq!(
        (st.claim.value.as_str(), st.rank, st.behind),
        ("set just now", Rank::Person, Some(0))
    );
}

/// A `ttl` lapse is judged when the value is read, against the reader's clock: no record is
/// written and the fold does not change, so the claim stands again for a reader whose clock is
/// before the lapse. CONTROL: one millisecond before its end the person's claim still stands.
#[test]
fn a_ttl_lapse_is_judged_at_the_read() {
    let mut log = Log::new(1);
    agent_session(&mut log, 0);
    desc(
        &mut log,
        Actor::Ctl,
        Src::Mark,
        Node::SESSION,
        say("title", "standing order"),
        10,
    );
    let brief = say("title", "for a minute").u(Key::Ttl, 60).expect("ttl");
    desc(&mut log, Actor::Human, Src::Mark, Node::SESSION, brief, 20);
    let (rev, end) = (log.state().rev, 20 + 60_000);
    let person = Some(("for a minute", Rank::Person));
    assert_eq!(
        stands(&log, "title", end - 1),
        person,
        "CONTROL: still live"
    );
    assert_eq!(
        stands(&log, "title", end),
        Some(("standing order", Rank::Ctl)),
        "lapsed: the next claim stands"
    );
    let rows = listing(&log, end);
    assert_eq!(
        rows.iter()
            .map(|r| (r.claim.by, r.live, r.standing))
            .collect::<Vec<_>>(),
        [(Actor::Ctl, true, true), (Actor::Human, false, false)],
        "the lapsed claim is still listed, as lapsed"
    );
    assert_eq!(
        stands(&log, "title", 30),
        person,
        "an earlier clock reads it"
    );
    assert_eq!(log.state().rev, rev, "no read moved the fold");
}

/// A session's sid ranks by who the host says the session is: the session's own sid is its
/// program, its creator's sid the creator, and any other session is listed and never stands.
/// CONTROL: before the host names the session no sid stands, and the same three claims are all
/// only listed.
#[test]
fn a_sid_ranks_by_the_sessions_identity() {
    let mut log = Log::new(1);
    agent_session(&mut log, 0);
    let own = sid("s-aaaaaaaaaaaaaaaaaaaa");
    let creator = sid("s-bbbbbbbbbbbbbbbbbbbb");
    let other = sid("s-cccccccccccccccccccc");
    for (i, (id, value)) in [
        (other, "a peer's"),
        (creator, "its creator's"),
        (own, "its own"),
    ]
    .into_iter()
    .enumerate()
    {
        desc(
            &mut log,
            Actor::Sid(id),
            Src::Mark,
            Node::SESSION,
            say("title", value),
            10 + i as u64,
        );
    }
    let ranks = |log: &Log| -> Vec<Rank> { listing(log, 50).iter().map(|r| r.rank).collect() };
    assert_eq!(stands(&log, "title", 50), None, "CONTROL: nobody is named");
    assert_eq!(ranks(&log), [Rank::Listed; 3]);
    let rev = log.state().rev;
    log.set_desc_ident(Some(own), Some(creator));
    assert_eq!(log.state().desc.ident().own, Some(own));
    assert_eq!(log.state().rev, rev + 1, "what is shown moved");
    assert_eq!(stands(&log, "title", 50), Some(("its own", Rank::Program)));
    assert_eq!(ranks(&log), [Rank::Program, Rank::Creator, Rank::Listed]);
    desc(
        &mut log,
        Actor::Sid(own),
        Src::Mark,
        Node::SESSION,
        unsay("title"),
        20,
    );
    assert_eq!(
        stands(&log, "title", 50),
        Some(("its creator's", Rank::Creator))
    );
    desc(
        &mut log,
        Actor::Sid(creator),
        Src::Mark,
        Node::SESSION,
        unsay("title"),
        21,
    );
    assert_eq!(
        (stands(&log, "title", 50), ranks(&log)),
        (None, vec![Rank::Listed]),
        "another session's claim is listed and never stands"
    );
}

/// A slot this build does not know is stored, shown and listed like a known one, up to eight a
/// subject; a claim for a ninth is dropped and counted. CONTROL: a known slot is admitted beside
/// eight unknown ones, and withdrawing an unknown slot's claim makes room for another.
#[test]
fn an_unknown_slot_is_listed_and_a_ninth_is_dropped_and_counted() {
    let mut log = Log::new(1);
    agent_session(&mut log, 0);
    let mut ms = 10;
    let mut put = |log: &mut Log, f: Fields| {
        ms += 1;
        desc(log, Actor::Ctl, Src::Mark, Node::SESSION, f, ms)
    };
    for i in 0..DESC_UNKNOWN_SLOTS {
        put(&mut log, say(&format!("x{i}"), "a later build's slot"));
    }
    put(&mut log, say("x8", "one too many"));
    let session = |log: &Log| {
        let s = log.state().desc.subject(Node::SESSION).expect("session");
        (s.slots.len(), s.dropped_slots)
    };
    assert_eq!(session(&log), (DESC_UNKNOWN_SLOTS, 1));
    assert_eq!(stands(&log, "x8", 100), None, "the ninth was dropped");
    assert_eq!(
        stands(&log, "x0", 100),
        Some(("a later build's slot", Rank::Ctl))
    );
    put(&mut log, say("description", "a known slot"));
    assert_eq!(
        session(&log),
        (DESC_UNKNOWN_SLOTS + 1, 1),
        "CONTROL: a known slot is always admitted"
    );
    let slots: Vec<&str> = listing(&log, 100).iter().map(|r| r.slot).collect();
    assert_eq!(
        slots,
        [
            "description",
            "x0",
            "x1",
            "x2",
            "x3",
            "x4",
            "x5",
            "x6",
            "x7"
        ],
        "known slots first, the others by name"
    );
    put(&mut log, unsay("x3"));
    put(&mut log, say("x8", "now there is room"));
    assert_eq!(
        (session(&log), stands(&log, "x8", 100).map(|s| s.0)),
        ((DESC_UNKNOWN_SLOTS + 1, 1), Some("now there is room")),
        "CONTROL: a withdrawn slot makes room"
    );
}

/// The store is bounded however many claims and lines are written: at most three agents beside
/// the session (a fourth agent's first claim drops the oldest agent's claims and counts it),
/// fourteen slots a subject (the six known ones and eight others), eight claims a slot and
/// sixteen history lines, and the fold's footprint charges exactly the store's heap, which stays
/// under the declared bound. `done` is claimed by other sessions (aterm's writers may not hold
/// one) and `package` by four ids of 25 characters. CONTROL: with every claim and line at its
/// longest the heap comes within a kilobyte of that bound, so the bound is the arithmetic and not
/// slack.
#[test]
fn the_store_is_bounded_and_the_fold_charges_its_heap() {
    let mut log = Log::new(1);
    let mut agents = vec![agent_session(&mut log, 0)];
    for i in 0..DESC_AGENTS {
        agents.push(agent(&mut log, 1 + i as u64));
    }
    assert_eq!(
        log.state().desc.heap(),
        0,
        "CONTROL: an empty store is free"
    );
    // A turn has closed before anything is measured, so the rest of the fold holds the newest
    // closed turn throughout and only the store grows.
    turn(&mut log, agents[1], 5);
    let (empty, ring) = (log.fold().footprint(), log.state().desc.heap());
    let mut slots: Vec<String> = KNOWN_SLOTS.iter().map(|s| (*s).to_string()).collect();
    slots.extend((0..=DESC_UNKNOWN_SLOTS).map(|i| format!("{i}{}", "x".repeat(23))));
    let value = "v".repeat(256);
    let package = ["AAAAAAAA", "BBBBBBBB", "CCCCCCCC", "DDDDDDDD"]
        .map(|p| format!("{p}-{}", "0123456789abcdef"))
        .join(",");
    assert_eq!(package.len(), 103);
    let mut ms = 10;
    for scope in std::iter::once(Node::SESSION).chain(agents.iter().copied()) {
        for slot in &slots {
            for author in 0..=DESC_CLAIMS {
                ms += 1;
                let name = format!("{author}{}", "n".repeat(39));
                let text = if slot == "package" { &package } else { &value };
                if slot == "done" {
                    let peer = sid(&format!("s-{author:0>20}"));
                    let f = say(slot, text).text(Key::As, &name).expect("as=");
                    desc(&mut log, Actor::Sid(peer), Src::Mark, scope, f, ms);
                } else {
                    writer(&mut log, &name, scope, slot, text, ms);
                }
            }
        }
    }
    let name = "n".repeat(40);
    for _ in 0..=DESC_HISTORY {
        for run in &agents[1..] {
            ms += 2;
            turn(&mut log, *run, ms);
            for scope in [Node::SESSION, *run] {
                ms += 1;
                let f = say("history", &value).text(Key::As, &name).expect("as=");
                desc(&mut log, Actor::Aterm, Src::Mark, scope, f, ms);
            }
        }
    }
    let d = &log.state().desc;
    assert_eq!(d.subjects().count(), 1 + DESC_AGENTS);
    assert_eq!(d.dropped_agents(), 1);
    assert!(d.subject(agents[0]).is_none(), "the oldest agent's went");
    for sub in d.subjects() {
        assert_eq!(
            (sub.slots.len(), sub.dropped_slots),
            (DESC_SLOTS, 1 + DESC_CLAIMS as u32),
            "{}: fourteen slots, and the fifteenth's nine claims counted",
            sub.node
        );
        for slot in &sub.slots {
            assert_eq!((slot.claims.len(), slot.dropped), (DESC_CLAIMS, 1));
        }
        assert_eq!(sub.history.len(), DESC_HISTORY, "{}", sub.node);
        assert!(sub.lines_older > 0, "{}: older lines counted", sub.node);
    }
    assert!(d.heap() <= DESC_HEAP_BYTES, "{} B", d.heap());
    assert!(DESC_HEAP_BYTES - d.heap() < 1024, "{} B", d.heap());
    assert_eq!(log.fold().footprint() - empty, d.heap() - ring);
}

/// A fold with claims crosses a snapshot whole: every claim with its author, source, `as=`, `re=`,
/// its incumbent and how far behind it is, so the fold read as the session the host named before
/// anyone described it reads the same after it and folds on the same. The host names the session
/// again afterwards, and nothing moves. A program's own title (content tier) is never written:
/// neither the snapshot nor a carry's records hold its text. CONTROL: a host-tier claim's text is
/// in the carry.
#[test]
fn a_carry_round_trips_claims_with_their_authors_and_no_content_text() {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let peer = sid("s-0123456789abcdef0123");
    log.set_desc_ident(Some(peer), None);
    let ident = *log.state().desc.ident();
    for (i, by) in [Actor::Human, Actor::Ctl, Actor::Sid(peer)]
        .into_iter()
        .enumerate()
    {
        let f = say("title", &format!("title {i} of 3"));
        desc(&mut log, by, Src::Mark, Node::SESSION, f, 10 + i as u64);
    }
    let about = say("description", "the agent's work")
        .text(Key::As, "planner")
        .and_then(|f| f.id(Key::Re, "3"))
        .and_then(|f| f.u(Key::Ttl, 600))
        .and_then(|f| f.u(Key::Upto, 2))
        .expect("a full claim");
    desc(&mut log, Actor::Aterm, Src::Mark, run, about, 20);
    turn(&mut log, run, 30);
    desc(
        &mut log,
        Actor::Harness,
        Src::Mark,
        run,
        say("title", "fixing the parser"),
        40,
    );
    let snap = log.fold().snapshot();
    let refs: Vec<&str> = snap.iter().map(String::as_str).collect();
    let mut back = Fold::from_snapshot_as(&refs, ident).expect("the snapshot reads");
    assert_eq!(back.state(), log.state(), "the state as it stood");
    assert_eq!(back.snapshot(), snap, "and it writes the same lines");
    let authors = |d: &aterm_slog::Described| -> Vec<(Node, String, Actor, Option<u32>)> {
        d.listing(WALL0 + 50)
            .iter()
            .map(|r| (r.scope, r.slot.to_string(), r.claim.by, r.behind))
            .collect()
    };
    assert_eq!(authors(&back.state().desc).len(), 5);
    back.set_desc_ident(Some(peer), None);
    assert_eq!(back.state(), log.state(), "named again, nothing moves");
    let from = log.last_seq();
    turn(&mut log, run, 60);
    writer(&mut log, "planner", run, "description", "next", 70);
    let later: Vec<&Rec> = log.window().filter(|r| r.seq > from).collect();
    for r in later {
        back.apply(r);
    }
    assert_eq!(back.state(), log.state(), "folding on alike");

    let secret = "QZP8 A PROGRAM'S OWN TITLE";
    let inband = Fields::new()
        .tok(Key::Field, "title")
        .and_then(|f| f.text(Key::Title, secret))
        .expect("an in-band title");
    log.mark(
        Mark::new(Kind::Desc, Src::Engine, Conf::Strong)
            .at(Node::SESSION)
            .fields(inband),
        at(80),
    )
    .expect("the program's title");
    let held = listing(&log, 90)
        .iter()
        .any(|r| r.claim.content && r.claim.value == secret && r.rank == Rank::Inband);
    assert!(held, "the fold holds it, in memory");
    let out = log.carry(CarryBudget::SESSION, &[]).expect("carries");
    assert!(
        out.lines.iter().all(|l| !l.contains("QZP8")),
        "content-tier text never crosses"
    );
    assert!(
        out.lines
            .iter()
            .any(|l| l.starts_with("fold desc - title ") && l.contains("title%200%20of%203")),
        "CONTROL: a host-tier claim's text does"
    );
    let snap = log.fold().snapshot();
    let refs: Vec<&str> = snap.iter().map(String::as_str).collect();
    let back = Fold::from_snapshot_as(&refs, ident).expect("the snapshot reads");
    let mut want = authors(&log.state().desc);
    want.retain(|(_, _, by, _)| *by != Actor::Nobody);
    assert_eq!(
        authors(&back.state().desc),
        want,
        "every other claim crossed"
    );
}

/// The claims a carry writes keep to their byte budget: when they do not all fit, whole claims
/// are left out and counted on a line of their own, never cut, and every slot's standing claim is
/// written before any other. The fold that reads the carry knows how many were left out.
/// CONTROL: the count and the lines written add up to every claim there was.
#[test]
fn a_carry_leaves_out_what_does_not_fit_and_counts_it() {
    let mut log = Log::new(1);
    agent_session(&mut log, 0);
    let slots = ["title", "description", "x1", "x2", "x3"];
    let value = "v".repeat(250);
    let mut ms = 10;
    for slot in slots {
        for author in 0..DESC_CLAIMS {
            ms += 1;
            let name = format!("writer-{author}");
            writer(&mut log, &name, Node::SESSION, slot, &value, ms);
        }
    }
    let snap = log.fold().snapshot();
    let lines: Vec<&String> = snap.iter().filter(|l| l.starts_with("fold desc")).collect();
    let bytes: usize = lines.iter().map(|l| l.len() + 1).sum();
    assert!(bytes <= DESC_CARRY_BYTES, "{bytes} B");
    let written: Vec<&&String> = lines
        .iter()
        .filter(|l| l.starts_with("fold desc - "))
        .collect();
    let left: usize = snap
        .iter()
        .find_map(|l| l.strip_prefix("fold desc-left n="))
        .and_then(|n| n.parse().ok())
        .expect("a count of what was left out");
    assert!(left > 0, "they cannot all fit");
    assert_eq!(
        written.len() + left,
        slots.len() * DESC_CLAIMS,
        "CONTROL: every claim is written or counted"
    );
    assert!(
        written.iter().all(|l| l.ends_with(&value)),
        "a written claim is whole"
    );
    assert_eq!(
        written.iter().filter(|l| l.contains(" st=1 ")).count(),
        slots.len(),
        "every slot's standing claim came first"
    );
    let refs: Vec<&str> = snap.iter().map(String::as_str).collect();
    let back = Fold::from_snapshot(&refs).expect("the snapshot reads");
    assert_eq!(back.state().desc.uncarried() as usize, left);
    for slot in slots {
        assert_eq!(
            shown(back.state(), slot, WALL0 + 100).map(|s| s.claim.seq),
            shown(log.state(), slot, WALL0 + 100).map(|s| s.claim.seq),
            "{slot}: the same claim stands"
        );
    }
}

/// IN-BAND BYTES CARRY A TITLE ONLY: a `desc` aterm read off what a program printed (no author,
/// `src=engine`) of any slot but `title` is refused by the log, whether its text is a `title=` or
/// a `value=`, and nothing is appended; the same records applied to a fold directly (as a journal
/// an earlier build wrote would hold them) are not stored and are counted, and the count crosses
/// a carry. Its `title` is stored, at the in-band rank, with its text at the content tier.
/// CONTROL: the same `description` from a control client is stored and stands.
#[test]
fn an_in_band_claim_of_any_slot_but_the_title_is_dropped_and_counted() {
    let mut log = Log::new(1);
    agent_session(&mut log, 0);
    let printed = |slot: &str, key: Key| {
        Fields::new()
            .tok(Key::Field, slot)
            .and_then(|f| f.text(key, "what a printed file says"))
            .expect("an in-band claim")
    };
    let records = [
        printed("description", Key::Title),
        printed("description", Key::Value),
        printed("summary", Key::Title),
    ];
    let before = log.last_seq();
    let mut fold = log.fold().clone();
    for (i, f) in records.into_iter().enumerate() {
        let refused = log.mark(
            Mark::new(Kind::Desc, Src::Engine, Conf::Claim)
                .by(Actor::Nobody)
                .at(Node::SESSION)
                .fields(f.clone()),
            at(10 + i as u64),
        );
        assert_eq!(refused, Err(Refused::Claim(ClaimRefused::InBand)));
        fold.apply(&Rec {
            seq: before + 1 + i as u64,
            t: WALL0 + 10 + i as u64,
            op: aterm_slog::Op::Mark,
            node: Node::SESSION,
            par: None,
            k: Kind::Desc,
            src: Src::Engine,
            conf: Conf::Claim,
            by: Actor::Nobody,
            f,
        });
    }
    assert_eq!(log.last_seq(), before, "the log appended nothing");
    assert!(listing(&log, 20).is_empty(), "{:?}", listing(&log, 20));
    assert!(fold.state().desc.listing(WALL0 + 20).is_empty());
    assert_eq!(fold.state().desc.refused(), 3);
    let snap = fold.snapshot();
    assert!(
        snap.iter()
            .any(|l| l.starts_with("fold desc-sub - ") && l.contains(" refused=3")),
        "{snap:?}"
    );
    let refs: Vec<&str> = snap.iter().map(String::as_str).collect();
    let back = Fold::from_snapshot(&refs).expect("the snapshot reads");
    assert_eq!(back.state().desc.refused(), 3, "the count crosses");
    desc(
        &mut log,
        Actor::Nobody,
        Src::Engine,
        Node::SESSION,
        printed("title", Key::Title),
        30,
    );
    let rows = listing(&log, 40);
    assert!(
        rows.len() == 1
            && rows[0].slot == "title"
            && rows[0].rank == Rank::Inband
            && rows[0].claim.content,
        "the title is the in-band claim it may be: {rows:?}"
    );
    desc(
        &mut log,
        Actor::Ctl,
        Src::Mark,
        Node::SESSION,
        say("description", "what a printed file says"),
        50,
    );
    assert_eq!(
        stands(&log, "description", 60),
        Some(("what a printed file says", Rank::Ctl)),
        "CONTROL: a control client's description is stored"
    );
    assert_eq!(
        log.state().desc.refused(),
        0,
        "the log refused, so the fold saw nothing"
    );
}
