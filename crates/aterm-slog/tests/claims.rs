// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claims (design §3.7, §7.4): a claim is an
//! attributed opinion that never overrides an observation, the trusted
//! authors' newest claims are kept beside the newest of any author, a label
//! reaches `brief` and the clause only from the harness or the owner, and no
//! name someone chose ever reads back as one of the actors a connection's
//! class names.

use aterm_slog::{
    Actor, BoxWord, Close, Conf, Fields, Fold, Key, Kind, Log, Mark, Node, Open, Out, RunOut,
    ShortId, SnapError, Src, Tier, TurnTracker, Verdict, decode, encode, need_raised,
};

use crate::support::{WALL0, agent_session, at};

fn sid(s: &str) -> Actor {
    Actor::Sid(ShortId::new(s).expect("a sid"))
}

/// A claim mark: `kind` from `by`, its word or text, `as=` when given.
fn claim(log: &mut Log, kind: Kind, by: Actor, value: &str, as_name: Option<&str>, ms: u64) -> u64 {
    let f = if kind == Kind::Label {
        Fields::new()
            .text(Key::Text, value)
            .and_then(|f| f.u(Key::Ttl, 600))
    } else {
        Fields::new().word(Key::Word, value)
    }
    .expect("the claim's value");
    let f = match as_name {
        Some(a) => f.text(Key::As, a).expect("as="),
        None => f,
    };
    log.mark(
        Mark::new(kind, Src::Mark, Conf::Claim).by(by).fields(f),
        at(ms),
    )
    .expect("the mark")
}

/// R13: an edge's newer verdict and label never displace the harness's. The
/// fold stores every claim as made (the newest of any author), and keeps the
/// newest trusted one beside it; the clause and `brief` show the label only
/// from a trusted author, and `brief` names both verdicts, the trusted first.
#[test]
fn an_edges_claims_never_displace_the_harnesss() {
    let mut log = Log::new(2);
    let _run = agent_session(&mut log, 0);
    claim(
        &mut log,
        Kind::Verdict,
        Actor::Harness,
        "on-track",
        None,
        10,
    );
    claim(
        &mut log,
        Kind::Label,
        Actor::Harness,
        "fixing the parser",
        None,
        11,
    );
    let edge = sid("s-0123456789abcdef0123");
    claim(
        &mut log,
        Kind::Verdict,
        edge,
        "blocked",
        Some("reviewer"),
        20,
    );
    claim(&mut log, Kind::Label, edge, "all is lost", None, 21);

    let st = log.state();
    assert_eq!(
        st.verdict.as_ref().map(|c| (c.word, c.by)),
        Some((Some("blocked"), edge))
    );
    assert_eq!(
        st.label.as_ref().map(|c| c.by),
        Some(edge),
        "stored as made"
    );
    assert_eq!(
        st.verdict_trusted.as_ref().map(|c| (c.word, c.by)),
        Some((Some("on-track"), Actor::Harness))
    );
    assert_eq!(
        st.label_trusted.as_ref().and_then(|c| c.text.clone()),
        Some("fixing the parser".to_string())
    );
    let now = WALL0 + 30_000;
    let c = log.fold().clause(now).expect("a clause");
    assert_eq!(
        (c.words.as_str(), c.by),
        ("fixing the parser · 29s", Actor::Harness)
    );

    let b = log.brief("s-1", 600, now);
    let claims: Vec<&str> = b.text.lines().filter(|l| l.starts_with("claim ")).collect();
    assert_eq!(claims.len(), 3, "{}", b.text);
    assert!(
        claims[0].starts_with("claim verdict on-track by=harness "),
        "{}",
        b.text
    );
    assert!(
        claims[1].starts_with("claim verdict blocked by=s-0123456789abcdef0123 as=reviewer "),
        "{}",
        b.text
    );
    assert!(
        claims[2].starts_with("claim label fixing%20the%20parser by=harness "),
        "{}",
        b.text
    );
    assert!(
        !b.text.contains("all%20is%20lost"),
        "an edge's label is never briefed"
    );
    // NEGATIVE: the edge's label is in the ring, as `log` shows it.
    assert!(
        log.window()
            .filter_map(|r| encode(r, Tier::Host).ok())
            .any(|l| l.contains("text=all%20is%20lost")),
        "every claim stays in the ring"
    );
}

/// The trusted slot is what a later trusted claim replaces: an owner's
/// verdict after the edge's is both the newest and the trusted one, and
/// `brief` names it once.
#[test]
fn a_trusted_claim_is_both_slots_when_newest() {
    let mut log = Log::new(2);
    let _ = agent_session(&mut log, 0);
    claim(
        &mut log,
        Kind::Verdict,
        sid("s-aaaaaaaaaaaaaaaaaaaa"),
        "blocked",
        None,
        5,
    );
    claim(&mut log, Kind::Verdict, Actor::Ctl, "done", None, 6);
    let st = log.state();
    assert_eq!(st.verdict, st.verdict_trusted);
    let b = log.brief("s-1", 600, WALL0 + 10_000);
    let verdicts = b
        .text
        .lines()
        .filter(|l| l.starts_with("claim verdict"))
        .count();
    assert_eq!(verdicts, 1, "{}", b.text);
}

/// Correction 5 (§3.7): a claim's `clear` (src=mark) lifts only needs a claim
/// raised; an observed need closes by observation alone. NEGATIVE CONTROL: a
/// clear from another source (the owner's meta flag) keeps the 2A rule and
/// lifts the observed failure.
#[test]
fn a_mark_clear_never_lifts_an_observed_need() {
    let words = |log: &Log| -> Vec<&'static str> {
        log.state().needs.iter().flatten().map(|n| n.word).collect()
    };
    let failed = |log: &mut Log| {
        let r = log
            .open(Open::new(Kind::Cmd, Src::Shell, Conf::Exact), at(1))
            .expect("cmd");
        log.close(
            r,
            Close::new(Out::Run(RunOut::Fail), Src::Shell, Conf::Exact)
                .fields(Fields::new().i(Key::Code, 1).expect("code")),
            at(2),
        )
        .expect("closes");
    };
    let mut log = Log::new(0);
    failed(&mut log);
    claim(&mut log, Kind::Need, Actor::Ctl, "help", None, 3);
    assert_eq!(words(&log), vec!["failure", "help"]);
    log.mark(
        Mark::new(Kind::Clear, Src::Mark, Conf::Claim).by(Actor::Ctl),
        at(4),
    )
    .expect("clear");
    assert_eq!(
        words(&log),
        vec!["failure"],
        "the claim lifted, the failure stands"
    );

    let mut meta = Log::new(0);
    failed(&mut meta);
    meta.mark(
        Mark::new(Kind::Clear, Src::Meta, Conf::Strong).by(Actor::Ctl),
        at(4),
    )
    .expect("clear");
    assert!(
        words(&meta).is_empty(),
        "a non-claim clear keeps the 2A rule"
    );
}

/// `need_raised` IS the fold's rule: every record kind driven through both,
/// a need raised by the fold exactly when the predicate names one, with the
/// predicate's word and the record's seq.
#[test]
fn need_raised_is_the_folds_rule() {
    let mut log = Log::new(0);
    let run = agent_session(&mut log, 0);
    let mut tr = TurnTracker::new(run);
    let mut ms = 10;
    let mut seen = 0usize;
    let mut raised = std::collections::BTreeSet::new();
    let check = |log: &Log, seen: &mut usize, raised: &mut std::collections::BTreeSet<&str>| {
        let recs: Vec<_> = log.window().skip(*seen).cloned().collect();
        *seen += recs.len();
        for r in recs {
            let held = log
                .state()
                .needs
                .iter()
                .flatten()
                .find(|n| n.since_seq == r.seq)
                .map(|n| n.word);
            // A need a record raised may be lifted later in the same batch
            // (a step closes): judge only what still stands, and only that
            // a predicate's silence leaves no need behind.
            match need_raised(&r) {
                Some(w) => {
                    if let Some(h) = held {
                        assert_eq!(h, w, "{r:?}");
                    }
                    raised.insert(w);
                }
                None => assert_eq!(held, None, "a need the predicate never named: {r:?}"),
            }
        }
    };
    for v in [
        Verdict::Busy,
        Verdict::Prompt(BoxWord::from_detail("bash:not-read-only")),
    ] {
        ms += 50;
        tr.observe(&mut log, v, None, at(ms));
    }
    check(&log, &mut seen, &mut raised);
    assert!(
        log.state()
            .needs
            .iter()
            .flatten()
            .any(|n| n.word == "approval:bash")
    );
    tr.observe(&mut log, Verdict::Busy, None, at(ms + 10));
    for w in [
        "help",
        "review",
        "failure",
        "exited",
        "escalated",
        "on-track",
    ] {
        let kind = if w == "on-track" {
            Kind::Verdict
        } else {
            Kind::Need
        };
        claim(&mut log, kind, Actor::Ctl, w, None, ms + 20);
    }
    log.mark(
        Mark::new(Kind::Need, Src::Meta, Conf::Strong)
            .fields(Fields::new().word(Key::Word, "escalated").expect("w")),
        at(ms + 30),
    )
    .expect("meta escalated");
    check(&log, &mut seen, &mut raised);
    let cmd = log
        .open(Open::new(Kind::Cmd, Src::Shell, Conf::Exact), at(ms + 40))
        .expect("cmd");
    log.close(
        cmd,
        Close::new(Out::Run(RunOut::Fail), Src::Shell, Conf::Exact),
        at(ms + 41),
    )
    .expect("fail");
    let ok = log
        .open(
            Open::new(Kind::Prog, Src::Status, Conf::Strong),
            at(ms + 42),
        )
        .expect("prog");
    log.close(
        ok,
        Close::new(Out::Run(RunOut::Ok), Src::Status, Conf::Strong),
        at(ms + 43),
    )
    .expect("ok");
    check(&log, &mut seen, &mut raised);
    log.close_session(
        Close::new(
            Out::Session(aterm_slog::SessionOut::ShellExit),
            Src::Aterm,
            Conf::Exact,
        ),
        at(ms + 50),
    )
    .expect("closes");
    check(&log, &mut seen, &mut raised);
    let want: std::collections::BTreeSet<&str> = [
        "approval:bash",
        "help",
        "review",
        "escalated",
        "failure",
        "exited",
    ]
    .into_iter()
    .collect();
    assert_eq!(raised, want, "every need kind was driven");
    // A claim never raises an observed need's word, or escalates.
    let mark = |src: Src, w: &str| aterm_slog::Rec {
        seq: 1,
        t: 0,
        op: aterm_slog::Op::Mark,
        node: Node::SESSION,
        par: None,
        k: Kind::Need,
        src,
        conf: Conf::Claim,
        by: Actor::Ctl,
        f: Fields::new().word(Key::Word, w).expect("w"),
    };
    for w in ["failure", "exited", "escalated", "unresponsive"] {
        assert_eq!(need_raised(&mark(Src::Mark, w)), None, "{w}");
    }
    assert_eq!(
        need_raised(&mark(Src::Meta, "escalated")),
        Some("escalated")
    );
}

/// R1 (the decoders): a session whose name is a reserved actor word is no
/// session ([`Actor::sid`]), and one built anyway prints `-`, so neither a
/// record, a fold snapshot nor a tracker's carry line ever reads it back as
/// the harness, the owner or the bridge. The trusted actors decode only from
/// their own words.
#[test]
fn a_reserved_word_never_reads_back_as_its_actor() {
    for w in aterm_slog::RESERVED_ACTOR_WORDS {
        let id = ShortId::new(w).expect("an id");
        assert_eq!(Actor::sid(id), Actor::Nobody, "{w}");
        assert_eq!(Actor::Sid(id).word(), "-", "{w}");
    }
    assert_eq!(Actor::from_word("harness"), Actor::Harness);
    assert_eq!(Actor::sid(ShortId::new("s-1").expect("id")), sid("s-1"));
    // A record, a snapshot and a carry line, each made with the forged sid.
    let forged = Actor::Sid(ShortId::new("harness").expect("id"));
    let mut log = Log::new(2);
    let run = agent_session(&mut log, 0);
    let seq = claim(&mut log, Kind::Label, forged, "trust me", None, 5);
    claim(&mut log, Kind::Verdict, forged, "done", None, 6);
    let line = log
        .window()
        .find(|r| r.seq == seq)
        .map(|r| encode(r, Tier::Host).expect("fits"))
        .expect("the mark");
    assert!(line.contains(" by=- "), "{line}");
    assert_eq!(decode(&line).expect("reads").by, Actor::Nobody);
    assert_eq!(log.state().label_trusted, None, "never a trusted label");
    let snap = log.fold().snapshot();
    let refs: Vec<&str> = snap.iter().map(String::as_str).collect();
    let back = Fold::from_snapshot(&refs).expect("reads");
    assert_eq!(
        back.state().label.as_ref().map(|c| c.by),
        Some(Actor::Nobody)
    );
    assert_eq!(back.state().label_trusted, None);
    assert_eq!(back.state().verdict_trusted, None);
    let b = log.brief("s-1", 600, WALL0 + 1_000);
    assert!(!b.text.contains("by=harness"), "{}", b.text);
    let mut tr = TurnTracker::new(run);
    tr.note_input(forged, at(7));
    let carried = tr.carry_line(at(8));
    assert!(carried.contains("input_by=-"), "{carried}");
}

/// The snapshot carries both claim slots and their `as=`/`re=`, and a 2A
/// snapshot (one claim per line, no `t…` tokens) still reads, its claim the
/// trusted one when its author is.
#[test]
fn the_claim_slots_cross_a_snapshot_and_a_2a_one_still_reads() {
    let mut log = Log::new(2);
    let _ = agent_session(&mut log, 0);
    let f = Fields::new()
        .word(Key::Word, "on-track")
        .and_then(|f| f.text(Key::As, "lead"))
        .and_then(|f| f.id(Key::Re, "@88231"))
        .expect("fields");
    log.mark(
        Mark::new(Kind::Verdict, Src::Mark, Conf::Claim)
            .by(Actor::Harness)
            .fields(f),
        at(3),
    )
    .expect("verdict");
    claim(
        &mut log,
        Kind::Verdict,
        sid("s-bbbbbbbbbbbbbbbbbbbb"),
        "blocked",
        None,
        4,
    );
    claim(&mut log, Kind::Label, Actor::Ctl, "wiring", Some("ops"), 5);
    let snap = log.fold().snapshot();
    let refs: Vec<&str> = snap.iter().map(String::as_str).collect();
    let back = Fold::from_snapshot(&refs).expect("reads");
    assert_eq!(back.state(), log.state(), "{snap:#?}");
    let trusted = back.state().verdict_trusted.as_ref().expect("trusted");
    assert_eq!(
        (
            trusted.as_name.as_deref(),
            trusted.re.map(|r| r.to_string())
        ),
        (Some("lead"), Some("@88231".to_string()))
    );
    // A 2A snapshot: the claim lines as 2A wrote them.
    let two_a: Vec<String> = snap
        .iter()
        .map(|l| {
            if l.starts_with("fold verdict ") {
                format!("fold verdict word=done by=ctl t={} seq=9", WALL0)
            } else if l.starts_with("fold label ") {
                format!(
                    "fold label text=x by=s-1 t={WALL0} until={} seq=10",
                    WALL0 + 1
                )
            } else {
                l.clone()
            }
        })
        .collect();
    let refs: Vec<&str> = two_a.iter().map(String::as_str).collect();
    let old = Fold::from_snapshot(&refs).expect("a 2A snapshot reads");
    assert_eq!(
        old.state().verdict,
        old.state().verdict_trusted,
        "ctl's is trusted"
    );
    assert_eq!(old.state().label_trusted, None, "s-1's is not");
    assert_eq!(old.state().label.as_ref().map(|c| c.by), Some(sid("s-1")));
    // A malformed trusted token refuses the whole snapshot, as any line does.
    let bad: Vec<String> = snap
        .iter()
        .map(|l| {
            if l.starts_with("fold verdict ") {
                l.replace(" tseq=", " tseq=x")
            } else {
                l.clone()
            }
        })
        .collect();
    let refs: Vec<&str> = bad.iter().map(String::as_str).collect();
    assert_eq!(Fold::from_snapshot(&refs).err(), Some(SnapError::Line));
}

/// `ctl` IS ANY IN-SESSION CLIENT (the supervised agent among them), so it
/// never displaces the harness: the harness marks `off-track`, the worker
/// inside its session marks `done` as `ctl`, and `brief` still names the
/// harness's verdict first, the clause and `brief` still show the harness's
/// label, and a `ctl` claim's `clear` lifts no need the harness raised (the
/// harness's own does). The proven slot crosses a fold snapshot. NEGATIVE
/// CONTROL: with no harness claim, the `ctl` verdict and label are the ones
/// shown.
#[test]
fn a_ctl_claim_never_displaces_the_harnesss() {
    let mut log = Log::new(2);
    let _ = agent_session(&mut log, 0);
    claim(
        &mut log,
        Kind::Verdict,
        Actor::Harness,
        "off-track",
        None,
        5,
    );
    claim(&mut log, Kind::Label, Actor::Harness, "judging", None, 6);
    claim(&mut log, Kind::Verdict, Actor::Ctl, "done", None, 7);
    claim(&mut log, Kind::Label, Actor::Ctl, "all good", None, 8);
    claim(&mut log, Kind::Need, Actor::Harness, "review", None, 9);
    let st = log.state();
    assert_eq!(
        st.verdict_proven.as_ref().map(|c| (c.word, c.by)),
        Some((Some("off-track"), Actor::Harness))
    );
    assert_eq!(
        st.verdict_trusted.as_ref().map(|c| c.by),
        Some(Actor::Ctl),
        "ctl's is the newest trusted"
    );
    let now = WALL0 + 30_000;
    let b = log.brief("s-1", 600, now);
    let claims: Vec<&str> = b.text.lines().filter(|l| l.starts_with("claim ")).collect();
    assert!(
        claims[0].starts_with("claim verdict off-track by=harness "),
        "{}",
        b.text
    );
    assert!(
        claims[1].starts_with("claim verdict done by=ctl "),
        "{}",
        b.text
    );
    assert!(
        claims[2].starts_with("claim label judging by=harness "),
        "{}",
        b.text
    );
    assert!(!b.text.contains("all%20good"), "{}", b.text);
    let c = log.fold().clause(now).expect("a clause");
    assert_eq!(c.by, Actor::Harness, "{c:?}");

    // A `ctl` claim's clear, every word: the harness's need stands.
    log.mark(
        Mark::new(Kind::Clear, Src::Mark, Conf::Claim).by(Actor::Ctl),
        at(10),
    )
    .expect("clear");
    let needs = |log: &Log| -> Vec<(&'static str, Actor)> {
        log.state()
            .needs
            .iter()
            .flatten()
            .map(|n| (n.word, n.by))
            .collect()
    };
    assert_eq!(needs(&log), vec![("review", Actor::Harness)]);
    let b = log.brief("s-1", 600, now);
    assert!(
        b.text.contains("claim need review by=harness "),
        "{}",
        b.text
    );

    // The proven slot crosses a snapshot, as `p…` tokens.
    let snap = log.fold().snapshot();
    assert!(
        snap.iter()
            .any(|l| l.starts_with("fold verdict ") && l.contains(" pword=off-track ")),
        "{snap:#?}"
    );
    let refs: Vec<&str> = snap.iter().map(String::as_str).collect();
    let back = Fold::from_snapshot(&refs).expect("reads");
    assert_eq!(back.state(), log.state(), "{snap:#?}");

    // The harness's own clear lifts it.
    log.mark(
        Mark::new(Kind::Clear, Src::Mark, Conf::Claim).by(Actor::Harness),
        at(11),
    )
    .expect("clear");
    assert!(needs(&log).is_empty());

    // NEGATIVE CONTROL: no harness claim, and `ctl`'s are what is shown.
    let mut solo = Log::new(2);
    let _ = agent_session(&mut solo, 0);
    claim(&mut solo, Kind::Verdict, Actor::Ctl, "done", None, 7);
    claim(&mut solo, Kind::Label, Actor::Ctl, "all good", None, 8);
    let b = solo.brief("s-1", 600, now);
    assert!(b.text.contains("claim verdict done by=ctl "), "{}", b.text);
    assert!(
        b.text.contains("claim label all%20good by=ctl "),
        "{}",
        b.text
    );
}

/// A NEED IS ITS RAISER'S: the same word raised again by another author is
/// that author's own need beside it, never a takeover, so its `clear` lifts
/// only its own — an edge cannot take the owner's or the harness's
/// escalation over and lift it. A flood of other authors' needs gives way
/// before the harness's. NEGATIVE CONTROL: the raiser's own clear lifts its
/// need, and `ctl`'s lifts an edge's.
#[test]
fn a_need_stays_its_raisers() {
    let needs = |log: &Log| -> Vec<(&'static str, Actor)> {
        log.state()
            .needs
            .iter()
            .flatten()
            .map(|n| (n.word, n.by))
            .collect()
    };
    let clear = |log: &mut Log, by: Actor, ms: u64| {
        log.mark(
            Mark::new(Kind::Clear, Src::Mark, Conf::Claim)
                .by(by)
                .fields(Fields::new().word(Key::Word, "help").expect("w")),
            at(ms),
        )
        .expect("clear");
    };
    let edge = sid("s-0123456789abcdef0123");
    let mut log = Log::new(0);
    claim(&mut log, Kind::Need, Actor::Ctl, "help", None, 1);
    claim(&mut log, Kind::Need, edge, "help", None, 2);
    assert_eq!(needs(&log), vec![("help", Actor::Ctl), ("help", edge)]);
    clear(&mut log, edge, 3);
    assert_eq!(
        needs(&log),
        vec![("help", Actor::Ctl)],
        "the edge lifted its own, the owner's stands"
    );
    let b = log.brief("s-1", 600, WALL0 + 10);
    assert!(b.text.contains("claim need help by=ctl "), "{}", b.text);

    let mut log = Log::new(0);
    claim(&mut log, Kind::Need, Actor::Harness, "help", None, 1);
    claim(&mut log, Kind::Need, edge, "help", None, 2);
    clear(&mut log, edge, 3);
    clear(&mut log, Actor::Ctl, 4);
    assert_eq!(needs(&log), vec![("help", Actor::Harness)]);
    clear(&mut log, Actor::Harness, 5);
    assert!(needs(&log).is_empty(), "NEGATIVE CONTROL: the raiser's own");

    // `ctl` lifts an edge's.
    let mut log = Log::new(0);
    claim(&mut log, Kind::Need, edge, "help", None, 1);
    clear(&mut log, Actor::Ctl, 2);
    assert!(needs(&log).is_empty(), "NEGATIVE CONTROL: ctl over an edge");

    // A flood: twelve edges, each its three words, then the harness's.
    let mut log = Log::new(0);
    for i in 0..12u32 {
        let e = sid(&format!("s-{i:020}"));
        for w in ["help", "review", "decision"] {
            claim(&mut log, Kind::Need, e, w, None, u64::from(i) + 1);
        }
    }
    claim(&mut log, Kind::Need, Actor::Harness, "help", None, 100);
    claim(&mut log, Kind::Need, Actor::Ctl, "decision", None, 101);
    let held = needs(&log);
    assert_eq!(held.len(), aterm_slog::fold::MAX_NEEDS);
    assert_eq!(
        &held[..2],
        &[("help", Actor::Harness), ("decision", Actor::Ctl)],
        "the harness's, then ctl's, before any edge's: {held:?}"
    );
}

/// No CHOSEN NAME is an actor word (class): an `as=` naming one,
/// in any case, is no field at all — refused by the fields a mark is built
/// from, skipped by the line decoder, refused by the fold snapshot — so
/// nothing reads `by=- as=harness`. NEGATIVE CONTROL: any other name holds.
#[test]
fn a_chosen_name_is_never_an_actor_word() {
    for w in ["harness", "HARNESS", "Ctl", "human", "bridge", "-", "aterm"] {
        assert!(
            Fields::new().text(Key::As, w).is_err(),
            "as={w} must never be a field"
        );
        let line = format!("7 mark - verdict word=done as={w} by=- src=mark conf=claim t=1");
        let r = decode(&line).expect("the line reads");
        assert_eq!(r.f.get_str(Key::As), None, "{line}");
    }
    assert!(Fields::new().text(Key::As, "reviewer").is_ok());
    assert!(Fields::new().text(Key::As, "harness-2").is_ok());
    let mut log = Log::new(2);
    let _ = agent_session(&mut log, 0);
    claim(
        &mut log,
        Kind::Verdict,
        Actor::Nobody,
        "done",
        Some("lead"),
        5,
    );
    let snap: Vec<String> = log
        .fold()
        .snapshot()
        .into_iter()
        .map(|l| l.replace(" as=lead", " as=Harness"))
        .collect();
    let refs: Vec<&str> = snap.iter().map(String::as_str).collect();
    assert_eq!(Fold::from_snapshot(&refs).err(), Some(SnapError::Line));
}
