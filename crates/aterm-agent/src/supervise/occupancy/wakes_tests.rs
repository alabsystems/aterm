// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for wake grammar parsing, refusals, and book state transitions.

use super::*;
use crate::supervise::occupancy::fault::{self, Fault};
use crate::supervise::tape::core::TapeValue;

const AGENT: AgentId = AgentId {
    pid: 200,
    birth_us: 2_000,
};

const T0: i64 = 1_800_000_000_000;

const NOTHING: [(u16, Change); 0] = [];

fn req(text: &str) -> WakeReq {
    WakeReq::parse(text).unwrap_or_else(|why| panic!("{text}: {why:?}"))
}

fn kinds(text: &str) -> Vec<AtomKind> {
    req(text).when.iter().map(Atom::kind).collect()
}

fn declared(text: &str) -> (WakeBook, u16) {
    let mut book = WakeBook::default();
    let ord = book
        .declare(req(text), Declarer::Agent, AGENT, T0, 0)
        .expect("declared");
    (book, ord)
}

/// Levels read by kind: every `file:` atom, every `gone:` atom, every `pid:`, the machine terms
/// and the agent.
#[derive(Clone, Copy)]
struct Lv {
    file: Option<bool>,
    gone: Option<bool>,
    pid: Option<bool>,
    sys: Option<bool>,
    alive: bool,
}

const QUIET: Lv = Lv {
    file: Some(false),
    gone: Some(false),
    pid: Some(false),
    sys: Some(false),
    alive: true,
};

impl Levels for Lv {
    fn source(&self, atom: &SourceAtom) -> Option<bool> {
        match atom {
            SourceAtom::File { .. } => self.file,
            SourceAtom::Gone { .. } => self.gone,
            SourceAtom::Pid { .. } => self.pid,
        }
    }
    fn sys(&self, _: &SysWait) -> Option<bool> {
        self.sys
    }
    fn agent_alive(&self, _: AgentId) -> bool {
        self.alive
    }
}

fn state(book: &WakeBook, ord: u16) -> WakeState {
    book.get(ord).expect("in the book").state
}

fn ready() -> (WakeBook, u16, u64) {
    let (mut book, ord) = declared("label=t when=file:~/scratch.noindex/x/flag");
    let seen = Lv {
        file: Some(true),
        ..QUIET
    };
    assert_eq!(book.on_look(&seen, T0 + 1), [(ord, Change::Ready)]);
    let episode = book.get(ord).expect("declared").episode();
    (book, ord, episode)
}

#[test]
fn every_atom_reads_as_its_kind() {
    use AtomKind::{At, File, Gone, In, Match, Pid, Sys};
    for (text, want, await_only) in [
        (
            "label=t when=file:~/scratch.noindex/x/flag",
            vec![File],
            false,
        ),
        (
            "label=t when=file:~/scratch.noindex/x/log~/ERROR%20[0-9]+/",
            vec![Match],
            false,
        ),
        (
            "label=t when=gone:~/scratch.noindex/x/lock",
            vec![Gone],
            false,
        ),
        ("label=t when=pid:4242 exited", vec![Pid], false),
        ("label=t when=at:23:40", vec![At], true),
        ("label=t when=in:30m", vec![In], true),
        ("label=t when=in:5m and at:07:00", vec![In, At], true),
        (
            "label=t when=slots>=1 and avail>=8GiB for=20m",
            vec![Sys],
            false,
        ),
        (
            "label=t when=file:/opt/x and slots>=1 and in:5m and load<0.5",
            vec![File, Sys, In],
            false,
        ),
        ("label=t when=in:5m and slots>=1", vec![In, Sys], false),
    ] {
        assert_eq!(kinds(text), want, "{text}");
        assert_eq!(req(text).await_only(), await_only, "{text}");
    }
    let r = req(
        "label=build.done_1 when=file:~/a need=slots>=1 and avail>=8GiB for=20m \
                 unless=gone:~/b unless=pid:9 exited prio=critical by=+2h",
    );
    assert_eq!(r.label.as_str(), "build.done_1");
    let need = r.need.as_ref().expect("need");
    assert_eq!(
        need.canonical(),
        "sys slots>=1 and avail>=8589934592B for=1200000ms"
    );
    assert!(need.names_slots());
    assert_eq!(
        r.unless.iter().map(SourceAtom::kind).collect::<Vec<_>>(),
        [Gone, Pid]
    );
    assert_eq!((r.prio, r.by_ms), (Prio::Critical, 2 * HOUR_MS));
    let r = req("label=t when=file:~/scratch.noindex/x/log~/ERROR%20[0-9]+/");
    match &r.when[0] {
        Atom::Source(SourceAtom::File {
            path,
            pattern: Some(pattern),
        }) => {
            assert!(path.home_relative());
            assert_eq!(path.as_str(), "scratch.noindex/x/log");
            assert_eq!(pattern.as_str(), "ERROR [0-9]+");
        }
        other => panic!("{other:?}"),
    }
    let r = req("label=t when=file:~/a");
    assert_eq!(
        (r.prio, r.by_ms, r.need.is_none()),
        (Prio::Normal, BY_DEFAULT_MS, true)
    );
    assert!(
        !req("label=t when=cpu.l1<50%").await_only(),
        "a level by index"
    );
}

#[test]
fn each_refusal_names_its_reason() {
    use WakeUsage as U;
    let nine = (0..9)
        .map(|i| format!("in:{}m", i + 1))
        .collect::<Vec<_>>()
        .join(" and ");
    let nine = format!("label=t when={nine}");
    let five = format!("label=t when=file:~/a {}", "unless=gone:~/b ".repeat(5));
    let eight = (0..8)
        .map(|i| format!("in:{}m", i + 1))
        .collect::<Vec<_>>()
        .join(" and ");
    assert_eq!(
        WakeReq::parse(&format!("label=t when={eight}")).map(|r| r.when.len()),
        Ok(WHEN_MAX),
        "eight atoms are not too many"
    );
    let four = format!("label=t when=file:~/a {}", "unless=gone:~/b ".repeat(4));
    assert_eq!(
        WakeReq::parse(&four).map(|r| r.unless.len()),
        Ok(UNLESS_MAX),
        "four unless= are not too many"
    );
    for (text, want) in [
        ("label=t when=file:~/a need=free>=room", U::Volume),
        (
            "label=t when=file:~/a need=slots>=1 and free>=10GB",
            U::Volume,
        ),
        ("label=t when=file:~/a need=disk=ok", U::Volume),
        ("label=t when=file:~/a need=slots>=1 vol=/", U::Volume),
        ("label=t when=file:~/a need=at:12:00", U::NeedSys),
        ("label=t when=file:~/a need=file:~/b", U::NeedSys),
        ("label=t when=file:~/a need=slots>=1 and in:5m", U::NeedSys),
        ("label=t when=file:~/a unless=slots>=1", U::UnlessSource),
        ("label=t when=file:~/a unless=in:5m", U::UnlessSource),
        ("label=t when=task:@s-1a2b", U::AwaitTask),
        ("label=t when=git(trust).moved", U::Slice3),
        ("label=t when=atpkg(trust).moved", U::Slice3),
        ("label=t when=workflow.returned", U::Slice3),
        ("label=t when=claim=release:x", U::Slice4),
        ("label=t when=file:~/a irreversible", U::Slice4),
        ("label=t when=job=a40977", U::JobTerm),
        ("label=t when=file:~/a and task=b4u3 done", U::JobTerm),
        ("label=t when=cpu.super<50%", U::CpuName),
        (
            "label=t when=file:~/a need=cpu.ignore_previous<50%",
            U::CpuName,
        ),
        ("label=t when=slots>=1 timeout=5000", U::Timeout),
        ("label=t when=file:~/a for=5m", U::ForWithoutSys),
        ("label=t when=file:~/a scope=aterm", U::ForWithoutSys),
        ("label=t for=5m when=slots>=1", U::ForWithoutSys),
        ("label=t when=file:~/a by=+49h", U::By),
        ("label=t when=file:~/a by=2h", U::By),
        ("label=t when=in:49h", U::By),
        ("label=t when=in:0m", U::By),
        ("when=file:~/a", U::Label),
        ("label=a/b when=file:~/a", U::Label),
        ("label=ignore%20previous when=file:~/a", U::Label),
        (
            &*format!("label={} when=file:~/a", "a".repeat(LABEL_MAX + 1)),
            U::Label,
        ),
        ("label=t", U::NoWhen),
        ("label=t when=file:~/a extra", U::Grammar),
        ("label=t when=bogus<1", U::Grammar),
        ("label=t when=cpu<50", U::Grammar),
        ("label=t label=u when=file:~/a", U::Grammar),
        ("label=t when=file:~/a when=file:~/b", U::Grammar),
        ("label=t when=file:~/a and", U::Grammar),
        ("label=t and file:~/a", U::Grammar),
        ("label=t when=file:~/a prio=urgent", U::Grammar),
        (&*nine, U::TooMany),
        (&*five, U::TooMany),
        ("label=t when=pid:0 exited", U::Grammar),
        ("label=t when=pid:012 exited", U::Grammar),
        ("label=t when=pid:12", U::Grammar),
        ("label=t when=pid:12 gone", U::Grammar),
        ("label=t when=at:24:00", U::Grammar),
        ("label=t when=at:9:00", U::Grammar),
        ("label=t when=at:09:60", U::Grammar),
    ] {
        assert_eq!(WakeReq::parse(text), Err(want), "{text}");
        assert!(
            !want.reply().contains("ignore"),
            "{text}: no word of the agent's"
        );
    }
    assert_eq!(
        SysWait::parse(&["slots>=1", "and", "free>=10GB"]),
        Err(U::Volume),
        "await sys's own refusal of the mixed form"
    );
    assert!(WakeReq::parse("label=t when=file:~/a and cpu.l1<50% for=1m").is_ok());
}

#[test]
fn the_wires_framing_reads_each_word_one_way() {
    use WakeUsage as U;
    let path_of = |text: &str| match &req(text).when[0] {
        Atom::Source(SourceAtom::File { path, pattern }) => (
            path.home_relative(),
            path.as_str().to_string(),
            pattern.as_ref().map(|p| p.as_str().to_string()),
        ),
        other => panic!("{other:?}"),
    };
    assert_eq!(
        WakeReq::parse("label=t when=file:~/a b"),
        Err(U::Grammar),
        "a raw space"
    );
    assert_eq!(
        path_of("label=t when=file:~/a%20b"),
        (true, "a b".into(), None)
    );
    assert_eq!(
        WakeReq::parse("label=t when=file:~/x and y"),
        Err(U::Grammar),
        "a raw and joins a term"
    );
    assert_eq!(
        path_of("label=t when=file:~/x%20and%20y"),
        (true, "x and y".into(), None)
    );
    assert_eq!(
        path_of("label=t when=file:~/a%7E/b"),
        (true, "a~/b".into(), None)
    );
    assert_eq!(
        path_of("label=t when=file:~/a~/b/"),
        (true, "a".into(), Some("b".into()))
    );
    assert_eq!(
        path_of("label=t when=file:/a~b"),
        (false, "/a~b".into(), None)
    );
    assert_eq!(
        path_of("label=t when=file:%7E/x"),
        (false, "~/x".into(), None)
    );
    for (text, want) in [
        ("label=t when=file:~/a%0Ab", U::Control),
        ("label=t when=file:~/a\u{1}b", U::Control),
        ("label=t when=file:~/log~/a%0A/", U::Control),
        ("label=t when=file:~/a%G1", U::Escape),
        ("label=t when=file:~/a%E9", U::Escape),
        ("label=t when=file:~/a%2", U::Escape),
        ("label=t when=file:~/log~/(/", U::Pattern),
        ("label=t when=gone:~/a~/b/", U::Grammar),
        ("label=t when=file:~/log~/a/b/", U::Grammar),
        ("label=t when=file:~/log~/b", U::Grammar),
        ("label=t when=file:~/log~//", U::Grammar),
        ("label=t when=file:", U::Grammar),
    ] {
        assert_eq!(WakeReq::parse(text), Err(want), "{text:?}");
    }
    let at_limit = "a".repeat(PATH_MAX - 2);
    assert!(WakeReq::parse(&format!("label=t when=file:~/a/{at_limit}")).is_ok());
    assert_eq!(
        WakeReq::parse(&format!("label=t when=file:~/ab/{at_limit}")),
        Err(U::PathLong)
    );
    let pattern = "a".repeat(PATTERN_MAX);
    assert!(WakeReq::parse(&format!("label=t when=file:~/log~/{pattern}/")).is_ok());
    assert_eq!(
        WakeReq::parse(&format!("label=t when=file:~/log~/{pattern}a/")),
        Err(U::PatternLong)
    );
}

#[test]
fn the_seventeenth_wake_is_refused() {
    let mut book = WakeBook::default();
    for n in 1..=MAX_WAKES {
        let ord = book
            .declare(req("label=t when=file:~/a"), Declarer::Agent, AGENT, T0, 0)
            .expect("room");
        assert_eq!(usize::from(ord), n);
    }
    assert_eq!(
        book.declare(req("label=t when=file:~/a"), Declarer::Agent, AGENT, T0, 0),
        Err(DeclareRefusal::Full)
    );
    assert!(book.taken(3));
    assert_eq!(
        book.declare(req("label=t when=file:~/a"), Declarer::Agent, AGENT, T0, 0),
        Ok(17)
    );
    assert!(book.get(3).is_none(), "the finished one is forgotten");
    assert_eq!(book.wakes.len(), MAX_WAKES);

    let mut book = WakeBook::default();
    assert!(
        book.declare(
            req("label=t when=in:47h by=+48h"),
            Declarer::Agent,
            AGENT,
            T0,
            0
        )
        .is_ok()
    );
    assert_eq!(
        book.declare(
            req("label=t when=in:3h by=+2h"),
            Declarer::Agent,
            AGENT,
            T0,
            0
        ),
        Err(DeclareRefusal::PastBy)
    );
    assert_eq!(
        WakeReq::parse("label=t when=file:~/a by=+49h"),
        Err(WakeUsage::By)
    );
    let (book, ord) = declared("label=t when=in:30m");
    assert!(book.get(ord).expect("declared").await_only);
    assert!(book.seen().wakes[0].await_only);

    let mut book = WakeBook {
        next_ord: u16::MAX - 1,
        ..WakeBook::default()
    };
    assert_eq!(
        book.declare(req("label=t when=file:~/a"), Declarer::Agent, AGENT, T0, 0),
        Ok(u16::MAX - 1)
    );
    assert_eq!(
        book.declare(req("label=t when=file:~/a"), Declarer::Agent, AGENT, T0, 0),
        Err(DeclareRefusal::Spent),
        "an ordinal is never taken twice"
    );
    assert_eq!(book.wakes.len(), 1);
}

#[test]
fn a_time_atom_is_its_next_instant() {
    let midnight_utc = 1_799_971_200_000;
    let now = midnight_utc + 22 * HOUR_MS;
    let mut book = WakeBook::default();
    let at = book
        .declare(req("label=t when=at:23:40"), Declarer::Agent, AGENT, now, 0)
        .expect("declared");
    let tomorrow = book
        .declare(
            req("label=t when=at:07:00"),
            Declarer::Agent,
            AGENT,
            now,
            3_600,
        )
        .expect("declared");
    let soon = book
        .declare(req("label=t when=in:90s"), Declarer::Agent, AGENT, now, 0)
        .expect("declared");
    let instant = |book: &WakeBook, ord| book.get(ord).expect("declared").when[0].at_ms;
    assert_eq!(instant(&book, at), Some(now + 100 * MINUTE_MS));
    assert_eq!(
        instant(&book, tomorrow),
        Some(now + 8 * HOUR_MS),
        "07:00 at UTC+1"
    );
    assert_eq!(instant(&book, soon), Some(now + 90_000));
    assert_eq!(book.on_look(&QUIET, now + 89_999), NOTHING);
    assert_eq!(book.on_look(&QUIET, now + 90_000), [(soon, Change::Ready)]);
}

#[test]
fn an_edge_latches_through_a_false_level() {
    let blip = || {
        let (mut book, ord) = declared("label=t when=file:~/scratch.noindex/x/flag");
        let edge = book.on_source(ord, 0, T0 + 1);
        book.on_look(&QUIET, T0 + 2);
        (edge, state(&book, ord))
    };
    assert_eq!(blip(), (Some(Change::Ready), WakeState::Ready));
    assert_eq!(
        fault::with(Fault::LevelOnly, blip),
        (None, WakeState::Pending),
        "the blip is lost"
    );

    let (mut book, ord) = declared("label=t when=file:~/a and slots>=1");
    assert_eq!(
        book.on_source(ord, 1, T0 + 1),
        None,
        "an edge latches a source atom alone"
    );
    assert_eq!(
        book.on_source(ord, 0, T0 + 1),
        None,
        "the machine terms still read false"
    );
    assert_eq!(book.on_look(&QUIET, T0 + 2), NOTHING);
    assert_eq!(
        book.on_look(&Lv { sys: None, ..QUIET }, T0 + 2),
        NOTHING,
        "an unread level changes nothing"
    );
    let free = Lv {
        sys: Some(true),
        ..QUIET
    };
    assert_eq!(book.on_look(&free, T0 + 3), [(ord, Change::Ready)]);
    assert_eq!(book.get(ord).and_then(|w| w.ready_since_ms), Some(T0 + 3));
    assert_eq!(book.on_look(&QUIET, T0 + 4), NOTHING, "ready stays ready");
    assert_eq!(state(&book, ord), WakeState::Ready);
}

#[test]
fn a_claim_after_told_is_refused() {
    let (mut book, ord, episode) = ready();
    assert_eq!(
        book.claim(ord, episode + 1, WakeDoor::Break, T0),
        WakeClaim::Gone
    );
    assert_eq!(
        book.claim(ord, episode, WakeDoor::Await, T0),
        WakeClaim::Busy
    );
    assert_eq!(
        book.claim(ord, episode, WakeDoor::Break, T0),
        WakeClaim::Granted
    );
    assert_eq!(
        book.claim(ord, episode, WakeDoor::Point, T0),
        WakeClaim::Busy
    );
    assert!(
        !book.told(ord, episode, WakeDoor::Point, T0),
        "not its claim"
    );
    assert!(book.release(ord, episode));
    assert_eq!(state(&book, ord), WakeState::Ready);
    assert_eq!(
        book.claim(ord, episode, WakeDoor::Point, T0),
        WakeClaim::Granted
    );
    assert!(book.told(ord, episode, WakeDoor::Point, T0 + 5));
    assert_eq!(
        state(&book, ord),
        WakeState::Told {
            door: WakeDoor::Point,
            at_ms: T0 + 5
        }
    );
    assert!(!book.release(ord, episode), "a told wake is not released");
    assert_eq!(
        book.claim(ord, episode, WakeDoor::Break, T0 + 6),
        WakeClaim::Gone
    );
    let twice = fault::with(Fault::ClaimTwice, || {
        book.claim(ord, episode, WakeDoor::Break, T0 + 6)
    });
    assert_eq!(twice, WakeClaim::Granted, "two tells");

    let (mut book, ord) = declared("label=t when=file:~/a");
    let episode = book.get(ord).expect("declared").episode();
    assert_eq!(
        book.claim(ord, episode, WakeDoor::Break, T0),
        WakeClaim::Unmet
    );

    for door in [WakeDoor::Break, WakeDoor::Point] {
        let (mut book, ord) = declared("label=alarm when=in:1s");
        assert_eq!(book.on_look(&QUIET, T0 + 1_000), [(ord, Change::Ready)]);
        let episode = book.get(ord).expect("declared").episode();
        assert!(book.head_due().is_none());
        assert_eq!(book.claim(ord, episode, door, T0 + 1_000), WakeClaim::Gone);
        assert!(!book.told(ord, episode, door, T0 + 1_000));
        assert_eq!(state(&book, ord), WakeState::Ready);
        assert_eq!(book.for_await(), [ord]);

        assert!(book.answer_begin(ord, T0 + 1_001));
        book.answer_done(ord, true, true, T0 + 1_002);
        let answered = WakeState::Answered { at_ms: T0 + 1_002 };
        let retell_at = T0 + 1_002 + UNHEARD_MS;
        assert_eq!(book.claim(ord, episode, door, retell_at), WakeClaim::Gone);
        assert!(!book.told(ord, episode, door, retell_at));
        assert_eq!(state(&book, ord), answered);
        assert!(book.heard(ord, retell_at + 1));
        assert_eq!(
            state(&book, ord),
            WakeState::Told {
                door: WakeDoor::Await,
                at_ms: retell_at + 1,
            }
        );
    }
}

#[test]
fn a_failed_answer_is_ready_again() {
    let failed = || {
        let (mut book, ord, _) = ready();
        assert!(book.answer_begin(ord, T0 + 2));
        assert!(!book.answer_begin(ord, T0 + 2), "one answer at a time");
        book.answer_done(ord, false, true, T0 + 3);
        state(&book, ord)
    };
    assert_eq!(failed(), WakeState::Ready);
    assert_eq!(
        fault::with(Fault::ToldBeforeWrite, failed),
        WakeState::Answered { at_ms: T0 + 3 },
        "answered and lost"
    );

    let (mut book, ord, episode) = ready();
    book.answer_done(ord, true, true, T0 + 2);
    assert_eq!(state(&book, ord), WakeState::Ready, "no answer was begun");
    book.claim(ord, episode, WakeDoor::Break, T0 + 2);
    book.answer_done(ord, false, true, T0 + 3);
    assert!(
        matches!(state(&book, ord), WakeState::Claimed { .. }),
        "a late write's end leaves a door's claim"
    );

    let (mut book, ord, _) = ready();
    book.answer_begin(ord, T0 + 2);
    book.answer_done(ord, true, false, T0 + 3);
    assert_eq!(
        state(&book, ord),
        WakeState::Told {
            door: WakeDoor::Await,
            at_ms: T0 + 3
        },
        "no door could tell it again"
    );

    let (mut book, ord, episode) = ready();
    book.answer_begin(ord, T0 + 2);
    book.answer_done(ord, true, true, T0 + 3);
    let answered = WakeState::Answered { at_ms: T0 + 3 };
    assert_eq!(state(&book, ord), answered);
    assert_eq!(
        book.claim(ord, episode, WakeDoor::Break, T0 + 3 + UNHEARD_MS - 1),
        WakeClaim::Answered
    );
    assert_eq!(
        book.claim(ord, episode, WakeDoor::Break, T0 + 3 + UNHEARD_MS),
        WakeClaim::Granted,
        "the one retell"
    );
    assert!(book.release(ord, episode));
    assert_eq!(state(&book, ord), answered);
    assert!(book.heard(ord, T0 + 9));
    assert_eq!(
        state(&book, ord),
        WakeState::Told {
            door: WakeDoor::Await,
            at_ms: T0 + 9
        }
    );
    assert_eq!(
        book.claim(ord, episode, WakeDoor::Break, T0 + 10 + UNHEARD_MS),
        WakeClaim::Gone,
        "heard, never retold"
    );
}

#[test]
fn an_answer_begins_on_a_ready_wake_and_is_heard_on_an_answered_one() {
    let (mut book, ord, episode) = ready();
    assert!(!book.heard(ord, T0 + 2), "never answered");
    assert!(!book.told(ord, episode, WakeDoor::Await, T0 + 2));
    assert_eq!(state(&book, ord), WakeState::Ready);
    assert_eq!(
        book.claim(ord, episode, WakeDoor::Break, T0 + 2),
        WakeClaim::Granted
    );
    assert!(!book.heard(ord, T0 + 3), "a door's claim is no answer");
    assert!(!book.answer_begin(ord, T0 + 3), "a door holds it");
    assert!(matches!(state(&book, ord), WakeState::Claimed { .. }));
    assert!(book.release(ord, episode));

    assert!(book.answer_begin(ord, T0 + 4));
    assert!(!book.heard(ord, T0 + 4), "its reply not written yet");
    book.answer_done(ord, true, true, T0 + 5);
    let answered = WakeState::Answered { at_ms: T0 + 5 };
    assert!(!book.answer_begin(ord, T0 + 6), "answered once");
    assert_eq!(state(&book, ord), answered);
    assert!(book.heard(ord, T0 + 7));
    assert_eq!(
        state(&book, ord),
        WakeState::Told {
            door: WakeDoor::Await,
            at_ms: T0 + 7
        }
    );
}

/// A claim a successor reconciles ends as its outcome says, whichever door held it (a restored
/// record's `Await` claim too, which no live claim makes).
#[test]
fn a_reconciled_claim_ends_as_its_outcome_says() {
    for door in [WakeDoor::Break, WakeDoor::Point, WakeDoor::Await] {
        let claimed = || {
            let (mut book, ord, episode) = ready();
            book.wakes[0].state = WakeState::Claimed {
                door,
                at_ms: T0 + 2,
            };
            (book, ord, episode)
        };
        let (mut book, ord, episode) = claimed();
        let typed = [(episode, SeedOutcome::Typed)];
        assert_eq!(book.reconcile(&typed, T0 + 9), [(ord, Reconciled::Told)]);
        assert_eq!(
            state(&book, ord),
            WakeState::Told {
                door,
                at_ms: T0 + 9
            },
            "{door:?}"
        );
        let (mut book, ord, _) = claimed();
        assert_eq!(book.reconcile(&[], T0 + 9), [(ord, Reconciled::Released)]);
        assert_eq!(state(&book, ord), WakeState::Ready, "{door:?}");
    }
}

#[test]
fn reconcile_with_a_typed_row_is_told() {
    let claimed = || {
        let (mut book, ord, episode) = ready();
        assert_eq!(
            book.claim(ord, episode, WakeDoor::Break, T0 + 2),
            WakeClaim::Granted
        );
        (book, ord, episode)
    };
    let (mut book, ord, episode) = claimed();
    let typed = [
        (episode, SeedOutcome::Intent),
        (episode ^ 1, SeedOutcome::Intent),
        (episode, SeedOutcome::Typed),
    ];
    assert_eq!(book.reconcile(&typed, T0 + 9), [(ord, Reconciled::Told)]);
    assert_eq!(
        state(&book, ord),
        WakeState::Told {
            door: WakeDoor::Break,
            at_ms: T0 + 9
        }
    );
    let released = fault::with(Fault::ReleaseAfterTyped, || {
        let (mut book, ord, _) = claimed();
        book.reconcile(&typed, T0 + 9);
        state(&book, ord)
    });
    assert_eq!(released, WakeState::Ready, "a typed line told again");
    for rows in [
        vec![(episode, SeedOutcome::Intent)],
        vec![
            (episode, SeedOutcome::Typed),
            (episode, SeedOutcome::Skipped),
        ],
        vec![(episode ^ 1, SeedOutcome::Typed)],
        Vec::new(),
    ] {
        let (mut book, ord, _) = claimed();
        assert_eq!(
            book.reconcile(&rows, T0 + 9),
            [(ord, Reconciled::Released)],
            "{rows:?}"
        );
        assert_eq!(state(&book, ord), WakeState::Ready, "{rows:?}");
    }
}

#[test]
fn agent_gone_expires_the_wake() {
    let unless = "label=t when=file:~/a unless=gone:~/b by=+1h";
    for (lv, at, want) in [
        (QUIET, T0 + HOUR_MS, Some(ExpireWhy::Due)),
        (
            Lv {
                alive: false,
                ..QUIET
            },
            T0 + 1,
            Some(ExpireWhy::Agent),
        ),
        (
            Lv {
                gone: Some(true),
                file: Some(true),
                ..QUIET
            },
            T0 + 1,
            Some(ExpireWhy::Unless),
        ),
        (QUIET, T0 + HOUR_MS - 1, None),
        (
            Lv {
                gone: None,
                ..QUIET
            },
            T0 + 1,
            None,
        ),
    ] {
        let (mut book, ord) = declared(unless);
        let changed = book.on_look(&lv, at);
        match want {
            Some(why) => {
                assert_eq!(changed, [(ord, Change::Expired(why))]);
                assert_eq!(state(&book, ord), WakeState::Expired { why });
                assert!(
                    book.seen().wakes.is_empty(),
                    "an ended wake leaves the view"
                );
            }
            None => assert_eq!(state(&book, ord), WakeState::Pending),
        }
    }
    let gone = Lv {
        alive: false,
        ..QUIET
    };
    for (lv, at) in [(gone, T0 + 4), (QUIET, T0 + BY_DEFAULT_MS)] {
        let (mut book, ord, _) = ready();
        book.answer_begin(ord, T0 + 2);
        book.answer_done(ord, true, true, T0 + 3);
        assert_eq!(book.on_look(&QUIET, T0 + 4), NOTHING, "heard yet or not");
        assert_eq!(
            book.on_look(&lv, at),
            [(ord, Change::Told(WakeDoor::Await))]
        );
    }
}

/// Levels whose `file:` paths are lost for `why` and whose `gone:` atoms read `gone`.
struct LostFiles {
    why: ExpireWhy,
    gone: Option<bool>,
}

impl Levels for LostFiles {
    fn source(&self, atom: &SourceAtom) -> Option<bool> {
        match atom {
            SourceAtom::Gone { .. } => self.gone,
            SourceAtom::File { .. } | SourceAtom::Pid { .. } => None,
        }
    }
    fn sys(&self, _: &SysWait) -> Option<bool> {
        None
    }
    fn agent_alive(&self, _: AgentId) -> bool {
        true
    }
    fn lost(&self, atom: &SourceAtom) -> Option<ExpireWhy> {
        matches!(atom, SourceAtom::File { .. }).then_some(self.why)
    }
}

/// A LOST PATH ENDS ITS WAKE for its reason: a `when=` atom that has not held, or an `unless=`
/// atom, whose path met a link (`why=path`) or the rules no longer admit (`why=admit`).
/// NEGATIVE CONTROL: a `when=` atom that held before its path was lost keeps its wake ready.
#[test]
fn a_lost_path_ends_its_wake_unless_its_atom_held() {
    for why in [ExpireWhy::Path, ExpireWhy::Admit] {
        for decl in [
            "label=t when=file:~/scratch.noindex/x/flag",
            "label=t when=gone:~/scratch.noindex/x/lock unless=file:~/scratch.noindex/x/stop",
        ] {
            let (mut book, ord) = declared(decl);
            let lv = LostFiles { why, gone: None };
            assert_eq!(book.on_look(&lv, T0 + 1), [(ord, Change::Expired(why))]);
            assert_eq!(state(&book, ord), WakeState::Expired { why }, "{decl}");
        }
    }
    let (mut book, ord, _) = ready();
    let lost = LostFiles {
        why: ExpireWhy::Path,
        gone: None,
    };
    assert_eq!(book.on_look(&lost, T0 + 2), NOTHING);
    assert_eq!(
        state(&book, ord),
        WakeState::Ready,
        "a held atom's path is read no more"
    );
}

#[test]
fn a_held_wake_ends_at_its_by_or_with_its_agent() {
    let gone = Lv {
        alive: false,
        ..QUIET
    };
    let held = |claim: bool| {
        let (mut book, ord, episode) = ready();
        if claim {
            book.claim(ord, episode, WakeDoor::Break, T0 + 2);
        } else {
            book.answer_begin(ord, T0 + 2);
        }
        (book, ord, episode)
    };
    for claim in [false, true] {
        let (mut book, ord, _) = held(claim);
        let before = state(&book, ord);
        assert_eq!(book.on_look(&QUIET, T0 + BY_DEFAULT_MS - 1), NOTHING);
        assert_eq!(state(&book, ord), before, "the hold stands");
        for (lv, at, why) in [
            (gone, T0 + 3, ExpireWhy::Agent),
            (QUIET, T0 + BY_DEFAULT_MS, ExpireWhy::Due),
        ] {
            let (mut book, ord, episode) = held(claim);
            assert_eq!(book.on_look(&lv, at), [(ord, Change::Expired(why))]);
            book.answer_done(ord, true, true, at + 1);
            assert!(!book.release(ord, episode));
            assert!(!book.told(ord, episode, WakeDoor::Break, at + 1));
            assert_eq!(state(&book, ord), WakeState::Expired { why }, "{claim}");
            assert!(book.seen().wakes.is_empty());
        }
    }
}

#[test]
fn a_book_restored_mid_answer_is_ready_again() {
    let (mut book, answering, _) = ready();
    book.answer_begin(answering, T0 + 2);
    let mut claimed = 0;
    let mut answered = 0;
    for n in [&mut claimed, &mut answered] {
        *n = book
            .declare(req("label=t when=file:~/a"), Declarer::Agent, AGENT, T0, 0)
            .expect("room");
    }
    let file = Lv {
        file: Some(true),
        ..QUIET
    };
    book.on_look(&file, T0 + 3);
    let episode = book.get(claimed).expect("declared").episode();
    book.claim(claimed, episode, WakeDoor::Point, T0 + 4);
    book.answer_begin(answered, T0 + 4);
    book.answer_done(answered, true, true, T0 + 5);
    let json = aterm_json::to_string(&book).expect("json");
    let back: WakeBook = aterm_json::from_str(&json).expect("reads back");
    assert_eq!(back, book);
    let restored = back.restored();
    assert_eq!(state(&restored, answering), WakeState::Ready);
    assert_eq!(restored.for_await(), [answering]);
    assert_eq!(state(&restored, claimed), state(&book, claimed));
    assert_eq!(
        state(&restored, answered),
        WakeState::Answered { at_ms: T0 + 5 }
    );
}

#[test]
fn taken_rm_and_disarm_end_wakes() {
    let (mut book, ord, _) = ready();
    assert!(book.taken(ord));
    assert_eq!(state(&book, ord), WakeState::Taken);
    assert!(book.rm(ord));
    assert!(book.get(ord).is_none());
    assert!(!book.rm(ord));
    let mut book = WakeBook::default();
    for _ in 0..3 {
        book.declare(req("label=t when=file:~/a"), Declarer::Agent, AGENT, T0, 0)
            .expect("room");
    }
    book.taken(2);
    assert_eq!(book.disarm(ExpireWhy::Off), [1, 3]);
    assert!(!book.taken(1), "expired");
    let (mut book, ord, episode) = ready();
    book.claim(ord, episode, WakeDoor::Break, T0 + 2);
    assert!(book.told(ord, episode, WakeDoor::Break, T0 + 3));
    assert!(book.taken(ord), "told");
    assert_eq!(state(&book, ord), WakeState::Taken);
    assert!(book.seen().wakes.is_empty());
    assert!(!book.any_open());
}

#[test]
fn the_head_is_critical_first() {
    let mut book = WakeBook::default();
    for text in [
        "label=t when=in:1m",
        "label=t when=file:~/a",
        "label=t when=file:~/a prio=critical",
        "label=t when=file:~/a",
    ] {
        book.declare(req(text), Declarer::Agent, AGENT, T0, 0)
            .expect("room");
    }
    let all = Lv {
        file: Some(true),
        ..QUIET
    };
    book.on_look(&all, T0 + 60_000);
    assert_eq!(book.head_due(), Some(3));
    assert_eq!(book.for_await(), [3, 1, 2, 4]);
}

#[test]
fn a_second_park_is_refused() {
    let (book, ord) = declared("label=t when=file:~/a");
    let mut parks = ParkBook::default();
    assert_eq!(parks.park(7, &book, T0), Ok(()));
    assert_eq!(parks.park(8, &book, T0), Err(ParkRefusal::Busy));
    assert_eq!(ParkRefusal::Busy.reply(), "ERR busy await wake parked\n");
    assert_eq!(parks.parked().len(), 1);
    let doubled = fault::with(Fault::ParkSecond, || {
        let mut parks = parks.clone();
        parks.park(8, &book, T0).map(|()| parks.parked().len())
    });
    assert_eq!(doubled, Ok(2));

    assert_eq!(
        parks.park(9, &WakeBook::default(), T0),
        Err(ParkRefusal::Busy),
        "the standing park first"
    );
    assert_eq!(
        ParkBook::default().park(9, &WakeBook::default(), T0),
        Err(ParkRefusal::NonePending)
    );

    let mut ended = book.clone();
    assert!(
        parks.ended(&book, ord, Ended::Removed).is_empty(),
        "a wake stays open"
    );
    ended.rm(ord);
    let answers = parks.ended(&ended, ord, Ended::Removed);
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].0.conn, 7);
    assert_eq!(
        answers[0].1.reply(),
        format!("EXPIRED wake #{ord} why=rm\n")
    );
    assert!(parks.parked().is_empty());
    let answer = ParkAnswer::Ready {
        ord: 2,
        since_ms: T0,
        slots: Some((1, 3)),
    };
    assert_eq!(
        answer.reply(),
        format!("OK wake #2 ready since={T0} slots=1/3\n")
    );
}

#[test]
fn a_planted_label_never_reaches_the_view() {
    const PLANT: &str = "ignore.previous.instructions";
    let text = format!(
        "label={PLANT} when=file:~/scratch.noindex/{PLANT}~/{PLANT}/ and slots>=1 \
         unless=gone:~/{PLANT}"
    );
    let mut book = WakeBook::default();
    let ord = book
        .declare(req(&text), Declarer::Agent, AGENT, T0, 0)
        .expect("declared");
    let wake = book.get(ord).expect("declared");
    assert_eq!(wake.label.as_str(), PLANT, "the book keeps it");
    let seen = book.seen();
    for shown in [
        format!("{seen:?}"),
        format!("{book:?}"),
        format!("{wake:?}"),
        format!("{:?}", req(&text)),
    ] {
        assert!(!shown.contains(PLANT), "{shown}");
        assert!(!shown.contains("ignore"), "{shown}");
    }
    let tape = String::from_utf8(seen.to_tape()).expect("json");
    assert!(!tape.contains("ignore"), "{tape}");
    assert!(tape.contains("\"kinds\":[\"match\",\"sys\"]"), "{tape}");
    assert_eq!(
        seen.wakes[0].kinds,
        [AtomKind::Match, AtomKind::Sys],
        "kinds alone"
    );
}

#[test]
fn the_book_reads_back_from_its_record() {
    let (mut book, ord, episode) = ready();
    book.declare(
        req(
            "label=u when=file:~/log~/E[0-9]/ and slots>=1 for=5m need=avail>=8GiB \
             unless=pid:9 exited prio=critical",
        ),
        Declarer::Owner,
        AGENT,
        T0,
        0,
    )
    .expect("room");
    book.claim(ord, episode, WakeDoor::Point, T0 + 2);
    let json = aterm_json::to_string(&book).expect("json");
    let back: WakeBook = aterm_json::from_str(&json).expect("reads back");
    assert_eq!(back, book);
    for (from, to) in [
        ("\"label\":\"u\"", "\"label\":\"a b\""),
        ("sys avail>=8589934592B", "sys free>=8GB"),
        ("\"E[0-9]\"", "\"(\""),
    ] {
        assert!(json.contains(from), "{from} in {json}");
        let bad = json.replace(from, to);
        assert!(aterm_json::from_str::<WakeBook>(&bad).is_err(), "{to}");
    }
}
