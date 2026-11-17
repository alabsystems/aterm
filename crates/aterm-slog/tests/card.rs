// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The card: the six slots a session's description claims fill and the history list beside
//! them, the rule that admits a claim or a line (refused, never cut), the stale-line rule, the
//! two scopes' rule, and the card's revision.

use aterm_slog::{
    Actor, BoundedText, CarryAdopt, CarryBudget, CarryError, ClaimRefused, Close, Conf,
    DESC_HISTORY, DESC_SLOTS, DescIdent, Fields, Fold, HISTORY, JournalImage, KNOWN_SLOTS, Key,
    Kind, Limits, Log, Mark, Node, Op, Open, Out, Rank, Rec, Refused, SessionOut, Src, TurnOut,
    desc_text, is_birth, shown, shown_history,
};

use crate::support::desc::{agent, desc, listing, say, sid, stands, turn, unsay, writer};
use crate::support::{WALL0, agent_session, at};

/// aterm's own derived writer: `by=aterm src=aterm`, rank 7.
const T0: (Actor, Src) = (Actor::Aterm, Src::Aterm);
/// The session's own program as the harness reports it, rank 2.
const HARNESS: (Actor, Src) = (Actor::Harness, Src::Mark);
/// A control client from no session, rank 5.
const CTL: (Actor, Src) = (Actor::Ctl, Src::Mark);
/// The person at the window, rank 1.
const PERSON: (Actor, Src) = (Actor::Human, Src::Mark);
/// Bytes a program printed, rank 6.
const INBAND: (Actor, Src) = (Actor::Nobody, Src::Engine);

/// A `desc` mark by `who` on `node`, as `Log::mark` answers it.
fn try_desc(
    log: &mut Log,
    who: (Actor, Src),
    node: Node,
    f: Fields,
    ms: u64,
) -> Result<u64, Refused> {
    log.mark(
        Mark::new(Kind::Desc, who.1, Conf::Claim)
            .by(who.0)
            .at(node)
            .fields(f),
        at(ms),
    )
}

/// A `desc` mark by `who` on `node` that the log appends.
fn put(log: &mut Log, who: (Actor, Src), node: Node, f: Fields, ms: u64) -> u64 {
    desc(log, who.0, who.1, node, f, ms)
}

/// A history line's fields: its text, and the `as=` and `re=` (a seq's digits) it names.
fn hist(text: &str, as_name: Option<&str>, re: Option<u64>) -> Fields {
    let mut f = say(HISTORY, text);
    if let Some(name) = as_name {
        f = f.text(Key::As, name).expect("as=");
    }
    if let Some(seq) = re {
        f = f.id(Key::Re, &seq.to_string()).expect("re=");
    }
    f
}

/// The same `desc` record a log would have made of `f` by `who` on `node`, built by hand at `seq`,
/// so it reaches a fold without `Log::mark` (as an earlier build's journal or carry would hold it).
fn rec(seq: u64, who: (Actor, Src), node: Node, f: Fields, ms: u64) -> Rec {
    Rec {
        seq,
        t: WALL0 + ms,
        op: Op::Mark,
        node,
        par: None,
        k: Kind::Desc,
        src: who.1,
        conf: Conf::Claim,
        by: who.0,
        f,
    }
}

/// Every record of `bad` applied to a copy of `log`'s fold, each by its author on the session, at
/// the seqs after the log's: the fold that results.
fn fed(log: &Log, bad: Vec<((Actor, Src), Fields)>) -> Fold {
    let mut fold = log.fold().clone();
    let from = log.last_seq();
    for (i, (who, f)) in bad.into_iter().enumerate() {
        let seq = from + 1 + i as u64;
        fold.apply(&rec(seq, who, Node::SESSION, f, 900 + i as u64));
    }
    fold
}

/// A turn of `run` opened at `ms`.
fn open_turn(log: &mut Log, run: Node, ms: u64) -> Node {
    log.open(
        Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
        at(ms),
    )
    .expect("a turn opens")
}

/// Close turn `t` at `ms`; the close's seq.
fn close_turn(log: &mut Log, t: Node, ms: u64) -> u64 {
    log.close(
        t,
        Close::new(Out::Turn(TurnOut::Done), Src::Screen, Conf::Strong),
        at(ms),
    )
    .expect("the turn closes")
}

/// The texts of `scope`'s history list, oldest first.
fn lines_of(log: &Log, scope: Node) -> Vec<&str> {
    log.state()
        .desc
        .history(scope)
        .iter()
        .map(|l| l.value.as_str())
        .collect()
}

/// THE CARD'S SLOTS: six known slots in the card's order, `description` the second named
/// (ST-27's name, kept), and fourteen in all with the eight unknown ones. A control client fills
/// eight unknown slots and then each known one: all fourteen are stored, each known one stands
/// at its rank with its text, and the listing orders the six as the card does, then the others
/// by name. A `history` mark is a line of the subject's list, never a slot.
/// CONTROL: a ninth unknown slot is dropped and counted while every known slot was admitted beside
/// eight unknown ones, and the history line took no unknown slot's room.
#[test]
fn the_fold_knows_six_slots() {
    assert_eq!(
        KNOWN_SLOTS,
        [
            "title",
            "matters",
            "description",
            "track",
            "done",
            "package"
        ]
    );
    assert!(!KNOWN_SLOTS.contains(&"summary"));
    assert_eq!(DESC_SLOTS, 14);
    let mut log = Log::new(1);
    agent_session(&mut log, 0);
    let mut ms = 10;
    for i in 0..8 {
        ms += 1;
        put(
            &mut log,
            CTL,
            Node::SESSION,
            say(&format!("x{i}"), "a later build's"),
            ms,
        );
    }
    let text = |slot: &str| {
        if slot == "package" {
            "FL-04".to_string()
        } else {
            format!("the session's {slot}")
        }
    };
    for slot in KNOWN_SLOTS {
        ms += 1;
        put(&mut log, CTL, Node::SESSION, say(slot, &text(slot)), ms);
    }
    let session = |log: &Log| {
        let s = log
            .state()
            .desc
            .subject(Node::SESSION)
            .expect("the session");
        (s.slots.len(), s.dropped_slots)
    };
    assert_eq!(session(&log), (DESC_SLOTS, 0), "all fourteen are stored");
    for slot in KNOWN_SLOTS {
        assert_eq!(
            stands(&log, slot, 100),
            Some((text(slot).as_str(), Rank::Ctl)),
            "{slot}"
        );
    }
    let mut want: Vec<String> = KNOWN_SLOTS.iter().map(|s| (*s).to_string()).collect();
    want.extend((0..8).map(|i| format!("x{i}")));
    let order: Vec<&str> = listing(&log, 100).iter().map(|r| r.slot).collect();
    assert_eq!(order, want, "the card's order, then the others by name");
    put(
        &mut log,
        CTL,
        Node::SESSION,
        hist("ran the suite", None, None),
        200,
    );
    let sub = log
        .state()
        .desc
        .subject(Node::SESSION)
        .expect("the session");
    assert_eq!(lines_of(&log, Node::SESSION), ["ran the suite"]);
    assert!(sub.slot(HISTORY).is_none(), "history is no slot");
    assert!(listing(&log, 210).iter().all(|r| r.slot != HISTORY));
    put(&mut log, CTL, Node::SESSION, say("x8", "one too many"), 220);
    assert_eq!(
        session(&log),
        (DESC_SLOTS, 1),
        "CONTROL: the ninth unknown slot is dropped and counted; the line took no slot's room"
    );
    assert_eq!(stands(&log, "x8", 230), None);
}

/// A PACKAGE IS 1 TO 4 TRACKER IDS, refused and never cut: five ids, a lower-case or nine-letter
/// prefix, a 17-character rest, no dash, a space, an empty element, a trailing comma, a repeat,
/// another separator and a non-ASCII rest are each malformed, and 104 bytes is over the bound.
/// The log refuses such a record even where it bypasses the writer's check (nothing appended, the
/// standing package unchanged), and a fold handed it stores nothing and counts it.
/// CONTROL: one id, four, and four of 25 characters (103 bytes) pass unchanged, are appended, and
/// stand exactly as written.
#[test]
fn a_malformed_package_is_refused_not_cut() {
    let longest = ["AAAAAAAA", "BBBBBBBB", "CCCCCCCC", "DDDDDDDD"]
        .map(|p| format!("{p}-0123456789abcdef"))
        .join(",");
    assert_eq!(longest.len(), 103);
    let seventeen = format!("FL-{}", "a".repeat(17));
    for raw in [
        "AA-1,BB-2,CC-3,DD-4,EE-5",
        "fl-04",
        "ABCDEFGHI-1",
        seventeen.as_str(),
        "FL04",
        "FL-04, CX-01",
        "FL-04,,CX-01",
        "FL-04,",
        "FL-04,FL-04",
        "FL-04/CX-01",
        "FL-0\u{e9}",
    ] {
        assert_eq!(
            desc_text("package", raw),
            Err(ClaimRefused::Package),
            "{raw:?}"
        );
    }
    assert_eq!(
        desc_text("package", &format!("{longest}0")),
        Err(ClaimRefused::TooLong { max: 103 }),
        "over the bound is refused, never cut back to the 103 bytes before it"
    );
    let mut log = Log::new(1);
    agent_session(&mut log, 0);
    put(&mut log, CTL, Node::SESSION, say("package", "FL-04"), 10);
    let before = log.last_seq();
    let spaced = say("package", "FL-04 CX-01");
    assert_eq!(
        try_desc(&mut log, CTL, Node::SESSION, spaced.clone(), 20),
        Err(Refused::Claim(ClaimRefused::Package))
    );
    assert_eq!(log.last_seq(), before, "nothing appended");
    assert_eq!(stands(&log, "package", 30), Some(("FL-04", Rank::Ctl)));
    let fold = fed(&log, vec![(CTL, spaced)]);
    assert_eq!(fold.state().desc.refused(), 1);
    assert_eq!(
        shown(fold.state(), "package", WALL0 + 30).map(|s| s.claim.value.as_str()),
        Some("FL-04"),
        "the fold stored nothing"
    );
    for (i, ok) in ["FL-04", "FL-04,CX-01,ST-27,LT-02", longest.as_str()]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            desc_text("package", ok).as_deref(),
            Ok(ok),
            "CONTROL: {ok:?}"
        );
        let seq = log.last_seq();
        put(
            &mut log,
            CTL,
            Node::SESSION,
            say("package", ok),
            40 + i as u64,
        );
        assert_eq!(log.last_seq(), seq + 1, "CONTROL: appended");
        assert_eq!(
            stands(&log, "package", 50 + i as u64),
            Some((ok, Rank::Ctl)),
            "CONTROL: it stands exactly as written"
        );
    }
}

/// IN-BAND BYTES STATE A TITLE ONLY: a program's printed bytes (no author, `src=engine`) claiming
/// `done` with a `title=`, with a `value=` or with no text (a withdrawal of a slot it may not
/// hold), or claiming `matters`, `description`, `track` or `package`, or writing a history line,
/// are each refused by the log and nothing is appended; handed to a fold, none is stored and each
/// is counted. CONTROL: the in-band `title` is appended and listed at its rank with content-tier
/// text, and a control client's `done` is appended and stands.
#[test]
fn an_in_band_done_is_refused() {
    let mut log = Log::new(1);
    agent_session(&mut log, 0);
    let printed = |slot: &str, key: Key| {
        Fields::new()
            .tok(Key::Field, slot)
            .and_then(|f| f.text(key, "printed by a file"))
            .expect("in-band fields")
    };
    let records = vec![
        printed("done", Key::Title),
        printed("done", Key::Value),
        unsay("done"),
        printed("matters", Key::Title),
        printed("description", Key::Title),
        printed("track", Key::Title),
        printed("package", Key::Title),
        printed(HISTORY, Key::Value),
    ];
    let before = log.last_seq();
    for (i, f) in records.iter().enumerate() {
        assert_eq!(
            try_desc(&mut log, INBAND, Node::SESSION, f.clone(), 10 + i as u64),
            Err(Refused::Claim(ClaimRefused::InBand)),
            "{f:?}"
        );
    }
    assert_eq!(log.last_seq(), before, "nothing appended");
    let fold = fed(&log, records.into_iter().map(|f| (INBAND, f)).collect());
    assert!(fold.state().desc.listing(WALL0 + 50).is_empty());
    assert!(fold.state().desc.history(Node::SESSION).is_empty());
    assert_eq!(fold.state().desc.refused(), 8, "each counted");
    put(
        &mut log,
        INBAND,
        Node::SESSION,
        printed("title", Key::Title),
        60,
    );
    let rows = listing(&log, 70);
    assert!(
        rows.len() == 1
            && rows[0].slot == "title"
            && rows[0].rank == Rank::Inband
            && rows[0].claim.content,
        "CONTROL: the in-band title is its claim: {rows:?}"
    );
    put(
        &mut log,
        CTL,
        Node::SESSION,
        say("done", "every check is green"),
        80,
    );
    assert_eq!(
        stands(&log, "done", 90),
        Some(("every check is green", Rank::Ctl)),
        "CONTROL: a control client's done stands"
    );
}

/// A PROTOCOL LINE IS REFUSED FROM EVERY AUTHOR BUT A PERSON: for each of the four heads the
/// harness's turn-end readers read (a need, a defense, a reason the work ends unfinished, the land
/// check's answer, also beside a NEEDS YOU head that says none stands, which Land sets aside), and
/// for a NEEDS YOU head that says none stands (the session withdrawing its needs), first the
/// readers themselves read it; then the log refuses it as a
/// `description`, `title`, `track` or history line from a control client, the harness, the
/// session's own sid and aterm's derived writer, and as a `title` from in-band bytes: nothing is
/// appended and what is shown is unchanged; handed to a fold, nothing is stored and each is
/// counted. CONTROL: the same texts from a person are appended and stand, and a sentence the
/// readers read no head in (`landed the retry fix; …`) is appended from a control client, so the
/// refusal is the readers' and no word list of aterm-slog's.
#[test]
fn a_value_the_hosts_predicate_reads_as_a_protocol_line_is_refused() {
    let heads = [
        "NEEDS YOU: sign in to the vault",
        "Nothing left: every change is on main",
        "Could not finish: the Windows cell is red",
        "LANDED",
        "LANDED. NEEDS YOU: none",
        "NEEDS YOU: none",
    ];
    let mut log = Log::new(1);
    agent_session(&mut log, 0);
    let own = sid("s-0wn0000000000000000a");
    log.set_desc_ident(Some(own), None);
    let slots = ["description", "title", "track"];
    for (i, slot) in slots.iter().enumerate() {
        put(
            &mut log,
            CTL,
            Node::SESSION,
            say(slot, "fixing the parser"),
            10 + i as u64,
        );
    }
    let shown_now = |log: &Log| -> Vec<Option<(String, Rank)>> {
        slots
            .iter()
            .map(|slot| stands(log, slot, 500).map(|(v, r)| (v.to_string(), r)))
            .collect()
    };
    let before = (log.last_seq(), shown_now(&log));
    let authors = [CTL, HARNESS, (Actor::Sid(own), Src::Mark), T0];
    let mut bad: Vec<((Actor, Src), Fields)> = Vec::new();
    for (i, text) in heads.into_iter().enumerate() {
        assert!(aterm_slog::protocol::protocol_line(text), "{text:?}");
        let mut tries: Vec<((Actor, Src), Fields)> = Vec::new();
        for who in authors {
            for slot in ["description", "title", "track", HISTORY] {
                tries.push((who, say(slot, text)));
            }
        }
        let inband = Fields::new()
            .tok(Key::Field, "title")
            .and_then(|f| f.text(Key::Title, text))
            .expect("an in-band title");
        tries.push((INBAND, inband));
        for (j, (who, f)) in tries.into_iter().enumerate() {
            assert_eq!(
                try_desc(
                    &mut log,
                    who,
                    Node::SESSION,
                    f.clone(),
                    100 + (i * 20 + j) as u64
                ),
                Err(Refused::Claim(ClaimRefused::Protocol)),
                "{text:?} by {who:?}: {f:?}"
            );
            bad.push((who, f));
        }
    }
    assert_eq!(
        (log.last_seq(), shown_now(&log)),
        before,
        "nothing appended, nothing shown changed"
    );
    assert!(log.state().desc.history(Node::SESSION).is_empty());
    let tried = bad.len() as u32;
    let fold = fed(&log, bad);
    assert_eq!(fold.state().desc.refused(), tried, "each counted");
    assert_eq!(
        fold.state().desc.listing(WALL0 + 500).len(),
        slots.len(),
        "the fold stored none of them"
    );
    for (i, text) in heads.into_iter().enumerate() {
        let ms = 600 + 10 * i as u64;
        put(
            &mut log,
            PERSON,
            Node::SESSION,
            say("description", text),
            ms,
        );
        put(
            &mut log,
            PERSON,
            Node::SESSION,
            hist(text, None, None),
            ms + 1,
        );
        assert_eq!(
            stands(&log, "description", ms + 2),
            Some((text, Rank::Person)),
            "CONTROL: a person is exempt"
        );
    }
    let mention = "landed the retry fix; the Windows cell is next";
    assert!(!aterm_slog::protocol::protocol_line(mention));
    put(&mut log, CTL, Node::SESSION, say("track", mention), 700);
    assert_eq!(
        stands(&log, "track", 710),
        Some((mention, Rank::Ctl)),
        "CONTROL"
    );
}

/// NOTHING THE GENERIC SANITIZER WOULD DROP IS STORED SILENTLY: for every known slot and the
/// history list, a text holding a line break, a carriage return, a tab, an escape, a NUL, a C1
/// control, a line or paragraph separator, a bidi override, a zero-width space, a byte-order mark
/// or a tag character is refused as formatting, before a `package`'s grammar is judged. The rule
/// takes no author: it binds a person too. CONTROL: the generic sanitizer keeps a text of each
/// (it would have cut the character silently, and turned `FL-04\nCX-01` into `FL-04CX-01`, a
/// well-formed id nobody wrote); the same words without the character, and with only a trailing
/// newline (trimmed), pass unchanged.
#[test]
fn a_value_with_a_line_break_or_a_control_character_is_refused() {
    let hidden = [
        '\n',
        '\r',
        '\t',
        '\u{1b}',
        '\u{0}',
        '\u{85}',
        '\u{2028}',
        '\u{2029}',
        '\u{202e}',
        '\u{200b}',
        '\u{feff}',
        '\u{e0041}',
    ];
    let words = |slot: &str, between: &str| {
        if slot == "package" {
            format!("FL-04{between}CX-01")
        } else {
            format!("fix the{between}parser")
        }
    };
    for slot in KNOWN_SLOTS.iter().copied().chain([HISTORY]) {
        for c in hidden {
            let w = words(slot, &c.to_string());
            assert_eq!(
                desc_text(slot, &w),
                Err(ClaimRefused::Formatting),
                "{slot} {c:?}"
            );
            assert!(
                BoundedText::new(Key::Value, &w).is_some(),
                "CONTROL: the sanitizer keeps {w:?}"
            );
        }
        let clean = words(slot, if slot == "package" { "," } else { " " });
        assert_eq!(
            desc_text(slot, &clean).as_deref(),
            Ok(clean.as_str()),
            "CONTROL"
        );
        assert_eq!(
            desc_text(slot, &format!("{clean}\n")).as_deref(),
            Ok(clean.as_str()),
            "CONTROL: a trailing newline is trimmed"
        );
    }
    let cut = BoundedText::new(Key::Value, "FL-04\nCX-01").expect("kept");
    assert_eq!(
        cut.as_str(),
        "FL-04CX-01",
        "CONTROL: what the sanitizer would store"
    );
    assert_eq!(
        desc_text("package", cut.as_str()).as_deref(),
        Ok("FL-04CX-01")
    );
}

/// A STALE PER-TURN LINE YIELDS: a writer's `description` and `track` stand over aterm's derived
/// ones while they are 1 to 3 turns behind, and at the fourth turn's close aterm's stand, which
/// are never behind. CONTROL: the writer's current claim stands again; its `title`, as far behind,
/// still stands over aterm's (a title is not per-turn); a person's `description` six turns behind
/// stands over aterm's (a person's is never stale); with no fresh claim, two writers both more
/// than three turns behind still give a standing `description`, the incumbent, never none; and
/// the rule holds inside one scope: a writer's session line four turns behind is still shown over
/// a control client's fresh line on the agent, which stands in the agent's own scope, since the
/// two scopes are compared by rank alone.
#[test]
fn a_description_more_than_three_turns_behind_yields_to_a_fresh_one() {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    for (i, slot) in ["description", "track", "title"].into_iter().enumerate() {
        let ms = 10 + 2 * i as u64;
        writer(
            &mut log,
            "planner",
            run,
            slot,
            "planning the parser fix",
            ms,
        );
        put(
            &mut log,
            T0,
            run,
            say(slot, "running the parser tests"),
            ms + 1,
        );
    }
    let behind = |log: &Log, slot: &str| {
        shown(log.state(), slot, WALL0 + 10_000).map(|s| (s.claim.value.clone(), s.rank, s.behind))
    };
    let planner = |n: u32| Some(("planning the parser fix".to_string(), Rank::Writer, Some(n)));
    let aterm = Some((
        "running the parser tests".to_string(),
        Rank::Derived,
        Some(0),
    ));
    let mut ms = 100;
    for n in 1..=3 {
        ms += 10;
        turn(&mut log, run, ms);
        for slot in ["description", "track"] {
            assert_eq!(behind(&log, slot), planner(n), "{slot}, {n} behind");
        }
    }
    ms += 10;
    turn(&mut log, run, ms);
    for slot in ["description", "track"] {
        assert_eq!(behind(&log, slot), aterm, "{slot}: the fourth close");
    }
    assert_eq!(
        behind(&log, "title"),
        planner(4),
        "CONTROL: a title is not per-turn"
    );
    writer(
        &mut log,
        "planner",
        run,
        "description",
        "planning the next fix",
        ms + 5,
    );
    assert_eq!(
        stands(&log, "description", ms + 6),
        Some(("planning the next fix", Rank::Writer)),
        "CONTROL: a current claim stands again"
    );
    put(
        &mut log,
        PERSON,
        run,
        say("description", "my own words"),
        ms + 7,
    );
    for _ in 0..6 {
        ms += 10;
        turn(&mut log, run, ms);
    }
    assert_eq!(
        stands(&log, "description", ms + 5),
        Some(("my own words", Rank::Person)),
        "CONTROL: a person's is never stale"
    );

    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    writer(
        &mut log,
        "first",
        run,
        "description",
        "the first writer's",
        10,
    );
    writer(
        &mut log,
        "second",
        run,
        "description",
        "the second writer's",
        11,
    );
    for i in 0..5 {
        turn(&mut log, run, 20 + 10 * i);
    }
    assert_eq!(
        stands(&log, "description", 100),
        Some(("the first writer's", Rank::Writer)),
        "CONTROL: all stale, the incumbent stands"
    );

    let mut log = Log::new(1);
    log.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        Actor::Aterm,
        Fields::new(),
        at(0),
    )
    .expect("the session opens");
    writer(
        &mut log,
        "planner",
        Node::SESSION,
        "description",
        "planning",
        10,
    );
    let run = agent(&mut log, 20);
    turn(&mut log, run, 30);
    turn(&mut log, run, 40);
    put(&mut log, CTL, run, say("description", "current"), 45);
    turn(&mut log, run, 50);
    turn(&mut log, run, 60);
    let now = WALL0 + 100;
    assert_eq!(
        shown(log.state(), "description", now).map(|s| (s.scope, s.claim.value.as_str(), s.behind)),
        Some((Node::SESSION, "planning", Some(4))),
        "CONTROL: across the two scopes the ranks alone decide"
    );
    assert_eq!(
        log.state()
            .desc
            .shown_in(run, "description", now)
            .map(|s| (s.claim.value.as_str(), s.rank)),
        Some(("current", Rank::Ctl)),
        "CONTROL: the agent's own fresh line stands in its scope"
    );
}

/// R1: WHAT THE SESSION WAS MADE FOR STANDS OVER ITS AGENT'S TOPIC. With the creator known, its
/// `title`, `matters` and `done` on the session (rank 3) stand over the harness's on the open
/// agent (rank 2), and the agent's are listed. CONTROL: a control client's title on the session
/// (rank 5) does not, so the harness's stands; and for `package` and `description` the agent's
/// better rank stands, one claim and never a union of the two.
#[test]
fn a_stated_session_title_stands_over_its_agents() {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let creator = sid("s-cre8or0000000000000a");
    log.set_desc_ident(Some(sid("s-0wn0000000000000000a")), Some(creator));
    let stated = ["title", "matters", "done"];
    for (i, slot) in stated.into_iter().enumerate() {
        let ms = 10 + 2 * i as u64;
        let by_creator = (Actor::Sid(creator), Src::Mark);
        put(
            &mut log,
            by_creator,
            Node::SESSION,
            say(slot, "the task it was made for"),
            ms,
        );
        put(
            &mut log,
            HARNESS,
            run,
            say(slot, "the agent's topic"),
            ms + 1,
        );
    }
    for slot in stated {
        let s = shown(log.state(), slot, WALL0 + 100).expect("one stands");
        assert_eq!(
            (s.scope, s.claim.value.as_str(), s.rank),
            (Node::SESSION, "the task it was made for", Rank::Creator),
            "{slot}"
        );
        assert!(
            listing(&log, 100)
                .iter()
                .any(|r| r.slot == slot && r.scope == run && r.rank == Rank::Program),
            "{slot}: the agent's is listed"
        );
    }

    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let creator = sid("s-cre8or0000000000000a");
    log.set_desc_ident(None, Some(creator));
    let by_creator = (Actor::Sid(creator), Src::Mark);
    put(
        &mut log,
        CTL,
        Node::SESSION,
        say("title", "a script's title"),
        10,
    );
    put(
        &mut log,
        HARNESS,
        run,
        say("title", "the agent's topic"),
        11,
    );
    put(
        &mut log,
        by_creator,
        Node::SESSION,
        say("package", "FL-04"),
        12,
    );
    put(&mut log, HARNESS, run, say("package", "CX-01"), 13);
    put(
        &mut log,
        by_creator,
        Node::SESSION,
        say("description", "dispatched"),
        14,
    );
    put(
        &mut log,
        HARNESS,
        run,
        say("description", "writing the fold"),
        15,
    );
    for (slot, want) in [
        ("title", "the agent's topic"),
        ("package", "CX-01"),
        ("description", "writing the fold"),
    ] {
        let s = shown(log.state(), slot, WALL0 + 100).expect("one stands");
        assert_eq!(
            (s.scope, s.claim.value.as_str(), s.rank),
            (run, want, Rank::Program),
            "CONTROL: {slot}: the better rank, one claim"
        );
    }
}

/// THE CARD'S REVISION MOVES ONLY WITH WHAT THE CARD SHOWS, step by step: a control client's
/// title (moves, to at least its record's seq); the same title again (no); another session's
/// `matters` (no: listed); aterm's title under the control client's (no); a turn (no); a note
/// (no); a person's same title (moves: a new author); a control client's history line (moves);
/// aterm's line for the same turn (no: outranked, and dropped); a protocol line (no, nothing
/// appended); the control client withdrawing its hidden title (no); a writer's description going
/// stale under aterm's (moves at that close); the host naming the other session the session's own
/// (moves by one: its `matters` stands now).
/// CONTROL: `rev` moved at every appended record, those that did not move the card included, so
/// the card's revision is narrower than the state's.
#[test]
fn card_rev_moves_only_with_a_standing_value_or_a_history_line() {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let peer = sid("s-peer000000000000000a");
    let card = |log: &Log| (log.state().rev, log.state().desc.card_rev());
    let mut steps: Vec<&str> = Vec::new();
    // One record appended: `rev` moves by one and the card's revision as `moves` says.
    let mut step =
        |log: &mut Log, what: &'static str, moves: bool, write: &dyn Fn(&mut Log) -> u64| {
            let (rev, card_rev) = card(log);
            let seq = write(log);
            let (rev2, card_rev2) = card(log);
            assert_eq!(rev2, rev + 1, "CONTROL: {what}: rev moves at every record");
            if moves {
                assert!(card_rev2 > card_rev && card_rev2 >= seq, "{what}: moves");
            } else {
                assert_eq!(card_rev2, card_rev, "{what}: does not move");
                steps.push(what);
            }
        };
    step(&mut log, "a ctl title", true, &|l| {
        put(l, CTL, Node::SESSION, say("title", "A"), 10)
    });
    step(&mut log, "the same title again", false, &|l| {
        put(l, CTL, Node::SESSION, say("title", "A"), 11)
    });
    step(&mut log, "a listed matters", false, &|l| {
        put(
            l,
            (Actor::Sid(peer), Src::Mark),
            Node::SESSION,
            say("matters", "why"),
            12,
        )
    });
    step(&mut log, "aterm's title under ctl's", false, &|l| {
        put(l, T0, Node::SESSION, say("title", "derived"), 13)
    });
    let t = std::cell::Cell::new(None);
    step(&mut log, "a turn opens", false, &|l| {
        let node = open_turn(l, run, 14);
        t.set(Some(node));
        l.last_seq()
    });
    step(&mut log, "the turn closes", false, &|l| {
        close_turn(l, t.get().expect("open"), 15)
    });
    step(&mut log, "a note", false, &|l| {
        let f = Fields::new().text(Key::Text, "a note").expect("text");
        l.mark(
            Mark::new(Kind::Note, Src::Mark, Conf::Claim)
                .by(Actor::Ctl)
                .fields(f),
            at(16),
        )
        .expect("a note")
    });
    step(&mut log, "a person's same title", true, &|l| {
        put(l, PERSON, Node::SESSION, say("title", "A"), 17)
    });
    step(&mut log, "a ctl history line", true, &|l| {
        put(l, CTL, Node::SESSION, hist("ran the suite", None, None), 18)
    });
    let dropped = log.state().desc.lines_dropped();
    step(&mut log, "aterm's line for the same turn", false, &|l| {
        put(l, T0, Node::SESSION, hist("the suite ran", None, None), 19)
    });
    assert_eq!(log.state().desc.lines_dropped(), dropped + 1, "outranked");
    let before = (log.last_seq(), card(&log));
    assert_eq!(
        try_desc(&mut log, CTL, Node::SESSION, hist("LANDED", None, None), 20),
        Err(Refused::Claim(ClaimRefused::Protocol))
    );
    assert_eq!(
        (log.last_seq(), card(&log)),
        before,
        "a protocol line: nothing appended"
    );
    step(&mut log, "ctl withdraws its hidden title", false, &|l| {
        put(l, CTL, Node::SESSION, unsay("title"), 21)
    });
    step(&mut log, "a writer's description", true, &|l| {
        writer(l, "planner", run, "description", "planning", 22)
    });
    step(&mut log, "aterm's description under it", false, &|l| {
        put(l, T0, run, say("description", "derived"), 23)
    });
    for i in 0..3u64 {
        step(&mut log, "a turn of three opens", false, &|l| {
            let node = open_turn(l, run, 30 + 10 * i);
            t.set(Some(node));
            l.last_seq()
        });
        step(&mut log, "a turn of three closes", false, &|l| {
            close_turn(l, t.get().expect("open"), 31 + 10 * i)
        });
    }
    step(&mut log, "the fourth turn opens", false, &|l| {
        let node = open_turn(l, run, 70);
        t.set(Some(node));
        l.last_seq()
    });
    step(&mut log, "the fourth close makes it stale", true, &|l| {
        close_turn(l, t.get().expect("open"), 71)
    });
    assert_eq!(
        stands(&log, "description", 80),
        Some(("derived", Rank::Derived))
    );
    let card_rev = log.state().desc.card_rev();
    log.set_desc_ident(Some(peer), None);
    assert_eq!(stands(&log, "matters", 90), Some(("why", Rank::Program)));
    assert_eq!(
        log.state().desc.card_rev(),
        card_rev + 1,
        "naming the session moves it by one"
    );
    assert!(steps.len() >= 10, "CONTROL: {steps:?}");
}

/// ONE LINE A TURN AND THE NEWEST SIXTEEN: over 20 agent turns the harness writes a line during
/// each turn and aterm one after each close, and aterm a `commit` event line at every fifth: the
/// list holds 16, newest first as a reader is shown them, one turn line a turn and each the
/// harness's; aterm's 20 turn lines are dropped and counted; the 8 the 16 pushed out are counted;
/// each event line kept stands beside its turn's line, taking no turn's place.
/// CONTROL: a turn where only aterm writes keeps aterm's line; the harness writing twice in one
/// turn keeps only its second; a control client's line after the harness's in the same turn is
/// dropped, and the harness's after a control client's replaces it; ten turns later every kept
/// line is still listed (a line is never stale); aterm repeating an event's `re=` replaces it.
#[test]
fn history_keeps_one_line_a_turn_and_the_newest_sixteen() {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let mut ms = 10;
    let mut last_event = 0;
    for k in 1..=20u64 {
        let t = open_turn(&mut log, run, ms);
        put(
            &mut log,
            HARNESS,
            run,
            hist(&format!("harness {k}"), None, None),
            ms + 1,
        );
        let close = close_turn(&mut log, t, ms + 2);
        put(
            &mut log,
            T0,
            run,
            hist(&format!("aterm {k}"), None, None),
            ms + 3,
        );
        if k % 5 == 0 {
            last_event = close;
            let f = hist(&format!("committed {k}"), Some("commit"), Some(close));
            put(&mut log, T0, run, f, ms + 4);
        }
        ms += 10;
    }
    let d = &log.state().desc;
    let kept = d.history(run);
    assert_eq!(kept.len(), DESC_HISTORY);
    assert_eq!(d.lines_dropped(), 20, "aterm's turn lines, outranked");
    assert_eq!(
        d.subject(run).expect("the agent").lines_older,
        8,
        "24 lines kept, the newest 16 of them listed"
    );
    let rows = shown_history(log.state(), DESC_HISTORY);
    assert_eq!(rows.len(), DESC_HISTORY);
    assert!(
        rows.windows(2).all(|w| w[0].line.seq > w[1].line.seq),
        "newest first"
    );
    let turns: Vec<u32> = kept
        .iter()
        .filter(|l| l.event().is_none())
        .map(|l| l.turn)
        .collect();
    let mut once = turns.clone();
    once.dedup();
    assert_eq!(turns, once, "one turn line a turn");
    assert!(
        kept.iter()
            .filter(|l| l.event().is_none())
            .all(|l| l.by == Actor::Harness && l.value == format!("harness {}", l.turn))
    );
    let events: Vec<&aterm_slog::HistLine> = kept
        .iter()
        .filter(|l| l.event() == Some("commit"))
        .collect();
    assert_eq!(
        events.iter().map(|l| l.value.as_str()).collect::<Vec<_>>(),
        ["committed 10", "committed 15", "committed 20"],
        "the fifth turn's went out with the oldest"
    );
    for e in &events {
        assert!(turns.contains(&e.turn), "{e:?} beside its turn's line");
    }

    let t = open_turn(&mut log, run, ms);
    close_turn(&mut log, t, ms + 1);
    put(&mut log, T0, run, hist("aterm alone", None, None), ms + 2);
    let t = open_turn(&mut log, run, ms + 10);
    put(&mut log, HARNESS, run, hist("first", None, None), ms + 11);
    put(&mut log, HARNESS, run, hist("second", None, None), ms + 12);
    close_turn(&mut log, t, ms + 13);
    let dropped = log.state().desc.lines_dropped();
    let t = open_turn(&mut log, run, ms + 20);
    put(
        &mut log,
        HARNESS,
        run,
        hist("the harness's", None, None),
        ms + 21,
    );
    put(&mut log, CTL, run, hist("a script's", None, None), ms + 22);
    close_turn(&mut log, t, ms + 23);
    let t = open_turn(&mut log, run, ms + 30);
    put(
        &mut log,
        CTL,
        run,
        hist("a script's first", None, None),
        ms + 31,
    );
    put(
        &mut log,
        HARNESS,
        run,
        hist("the harness's after", None, None),
        ms + 32,
    );
    close_turn(&mut log, t, ms + 33);
    let newest: Vec<&str> = lines_of(&log, run).into_iter().rev().take(4).collect();
    assert_eq!(
        newest,
        [
            "the harness's after",
            "the harness's",
            "second",
            "aterm alone"
        ],
        "CONTROL: one line each of the four turns"
    );
    assert_eq!(
        log.state().desc.lines_dropped(),
        dropped + 2,
        "CONTROL: the script's line dropped, and the one the harness replaced"
    );
    let held = log.state().desc.history(run).to_vec();
    for i in 0..10 {
        turn(&mut log, run, ms + 100 + 10 * i);
    }
    assert_eq!(log.state().desc.history(run), held, "CONTROL: never stale");
    let again = hist("committed 20, amended", Some("commit"), Some(last_event));
    put(&mut log, T0, run, again, ms + 300);
    let commits: Vec<&str> = log
        .state()
        .desc
        .history(run)
        .iter()
        .filter(|l| {
            l.event() == Some("commit")
                && l.re.map(|r| r.to_string()) == Some(last_event.to_string())
        })
        .map(|l| l.value.as_str())
        .collect();
    assert_eq!(
        commits,
        ["committed 20, amended"],
        "CONTROL: one line a reported record"
    );
}

/// AN EVENT WORD IS READ ONLY FROM ATERM'S OWN WRITER: the harness's history line `as=landed` with
/// a `re=` is stored with its name, reports no event, and is its turn's line (the harness's next
/// line in the turn replaces it); a writer aterm ran (rank 4) writing the same is the same: its
/// line is its turn's, so aterm's turn line there is dropped. CONTROL: aterm's own identical
/// record reports `landed` and stands beside that turn's line, taking no turn's place.
#[test]
fn an_event_word_on_a_programs_history_line_is_read_by_nothing() {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let close = turn(&mut log, run, 5);
    let t = open_turn(&mut log, run, 10);
    put(
        &mut log,
        HARNESS,
        run,
        hist("landed it", Some("landed"), Some(close)),
        11,
    );
    let line = log
        .state()
        .desc
        .history(run)
        .last()
        .expect("stored")
        .clone();
    assert_eq!(
        (line.as_name.as_deref(), line.event(), line.turn),
        (Some("landed"), None, 2),
        "stored with its name, read by nothing, the open turn's line"
    );
    put(&mut log, HARNESS, run, hist("next", None, None), 12);
    assert_eq!(
        lines_of(&log, run),
        ["next"],
        "the harness's next line replaces it"
    );
    close_turn(&mut log, t, 13);
    let t = open_turn(&mut log, run, 20);
    let named = hist("landed by a writer", Some("landed"), Some(close));
    put(&mut log, (Actor::Aterm, Src::Mark), run, named, 21);
    let line = log
        .state()
        .desc
        .history(run)
        .last()
        .expect("stored")
        .clone();
    assert_eq!(
        (line.event(), line.turn, line.rank),
        (None, 3, Rank::Writer)
    );
    let dropped = log.state().desc.lines_dropped();
    put(&mut log, T0, run, hist("aterm's line", None, None), 22);
    assert_eq!(
        log.state().desc.lines_dropped(),
        dropped + 1,
        "the writer's is the turn's line"
    );
    put(
        &mut log,
        T0,
        run,
        hist("the fix is on main", Some("landed"), Some(close)),
        23,
    );
    let held = log.state().desc.history(run);
    assert_eq!(
        held.iter()
            .map(|l| (l.value.as_str(), l.event()))
            .collect::<Vec<_>>(),
        [
            ("next", None),
            ("landed by a writer", None),
            ("the fix is on main", Some("landed"))
        ],
        "CONTROL: aterm's own line reports the event, beside the turn's line"
    );
    close_turn(&mut log, t, 24);
}

/// ATERM NEVER DECLARES DONE: neither its own derived writer nor a writer it ran may claim or
/// withdraw `done`. CONTROL: the harness's `done` stands.
#[test]
fn aterms_writers_cannot_declare_done() {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let before = log.last_seq();
    for src in [Src::Aterm, Src::Mark] {
        let claim = say("done", "the work is finished")
            .text(Key::As, "planner")
            .expect("as=");
        for f in [claim, unsay("done")] {
            assert_eq!(
                try_desc(&mut log, (Actor::Aterm, src), run, f, 10),
                Err(Refused::Claim(ClaimRefused::DoneAuthor)),
                "{src:?}"
            );
        }
    }
    assert_eq!(log.last_seq(), before);
    put(
        &mut log,
        HARNESS,
        run,
        say("done", "every change is on main"),
        20,
    );
    assert_eq!(
        stands(&log, "done", 30),
        Some(("every change is on main", Rank::Program)),
        "CONTROL"
    );
}

/// A CARRY CROSSES HISTORY LINES AND THE CARD'S REVISION: lines with their authors, names,
/// reported records, turns and ranks, the counts of lines dropped and pushed out, a repeated
/// claim's `since` and the card's revision cross a snapshot, the state reads the same after it,
/// and it folds on the same. CONTROL: with room for less, the newest lines are kept whoever wrote
/// them (aterm's own newest reports over a control client's older lines, a better rank), and
/// `desc-left … lines=` counts the rest, which the fold that reads it knows.
#[test]
fn a_carry_round_trips_history_lines_and_the_card_rev() {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    put(
        &mut log,
        CTL,
        Node::SESSION,
        say("title", "fix the parser"),
        10,
    );
    put(
        &mut log,
        CTL,
        Node::SESSION,
        say("title", "fix the parser"),
        11,
    );
    let mut ms = 20;
    for k in 0..(DESC_HISTORY as u64 + 2) {
        let close = turn(&mut log, run, ms);
        put(
            &mut log,
            HARNESS,
            run,
            hist(&format!("turn {k}"), Some("claude"), None),
            ms + 2,
        );
        put(
            &mut log,
            T0,
            run,
            hist(&format!("aterm {k}"), None, None),
            ms + 3,
        );
        put(
            &mut log,
            CTL,
            Node::SESSION,
            hist(&format!("session {k}"), None, None),
            ms + 4,
        );
        let event = hist(&format!("commit {k}"), Some("commit"), Some(close));
        put(&mut log, T0, Node::SESSION, event, ms + 5);
        ms += 10;
    }
    let d = &log.state().desc;
    assert!(d.lines_dropped() > 0 && d.subject(run).is_some_and(|s| s.lines_older > 0));
    let title = d
        .subject(Node::SESSION)
        .and_then(|s| s.slot("title"))
        .expect("a title");
    assert!(
        title.claims[0].since < title.claims[0].seq,
        "a repeat keeps its since"
    );
    let snap = log.fold().snapshot();
    assert!(
        snap.iter().any(|l| l.starts_with("fold desc-line ")),
        "{snap:?}"
    );
    let refs: Vec<&str> = snap.iter().map(String::as_str).collect();
    let mut back = Fold::from_snapshot(&refs).expect("the snapshot reads");
    assert_eq!(back.state(), log.state(), "the state as it stood");
    assert_eq!(back.snapshot(), snap, "and it writes the same lines");
    let from = log.last_seq();
    let close = turn(&mut log, run, ms);
    put(&mut log, HARNESS, run, hist("one more", None, None), ms + 2);
    put(
        &mut log,
        T0,
        run,
        hist("the fix is on main", Some("landed"), Some(close)),
        ms + 3,
    );
    put(
        &mut log,
        PERSON,
        Node::SESSION,
        say("title", "fix the lexer"),
        ms + 4,
    );
    for r in log.window().filter(|r| r.seq > from) {
        back.apply(r);
    }
    assert_eq!(back.state(), log.state(), "folding on alike");

    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let value = "v".repeat(250);
    for (i, slot) in KNOWN_SLOTS.iter().enumerate() {
        let text = if *slot == "package" {
            "FL-04"
        } else {
            value.as_str()
        };
        put(&mut log, CTL, Node::SESSION, say(slot, text), 10 + i as u64);
    }
    for i in 0..8u64 {
        put(
            &mut log,
            CTL,
            Node::SESSION,
            say(&format!("x{i}"), &value),
            20 + i,
        );
    }
    for k in 0..DESC_HISTORY as u64 {
        let ms = 100 + 10 * k;
        let close = turn(&mut log, run, ms + 1);
        // The older lines a control client's (rank 5), the newest four aterm's own reports of a
        // commit (rank 7): the room goes by recency, never by an author's rank.
        let line = if k < 12 {
            hist(&format!("{k:02} {value}"), None, None)
        } else {
            hist(&format!("{k:02} {value}"), Some("commit"), Some(close))
        };
        put(
            &mut log,
            if k < 12 { CTL } else { T0 },
            Node::SESSION,
            line,
            ms + 3,
        );
    }
    let lines = log.state().desc.history(Node::SESSION).to_vec();
    assert_eq!(lines.len(), DESC_HISTORY);
    assert_eq!(lines.last().and_then(|l| l.event()), Some("commit"));
    let snap = log.fold().snapshot();
    let left: u32 = snap
        .iter()
        .find_map(|l| l.strip_prefix("fold desc-left n=0 lines="))
        .and_then(|n| n.parse().ok())
        .expect("CONTROL: the lines left out are counted");
    let refs: Vec<&str> = snap.iter().map(String::as_str).collect();
    let back = Fold::from_snapshot(&refs).expect("the snapshot reads");
    let kept = back.state().desc.history(Node::SESSION);
    assert!(left > 0 && !kept.is_empty());
    assert_eq!(
        kept.len() + left as usize,
        DESC_HISTORY,
        "CONTROL: kept or counted"
    );
    assert_eq!(
        kept,
        &lines[left as usize..],
        "CONTROL: the newest are kept"
    );
    assert_eq!(
        kept.last(),
        lines.last(),
        "CONTROL: the newest line, a T0 event, is kept"
    );
    assert_eq!(kept.last().and_then(|l| l.event()), Some("commit"));
    assert_eq!(back.state().desc.uncarried_lines(), left);
}

/// A RESTORE READS THE CARD AS THE SESSION THE HOST SAYS IT IS: the host says who the session is
/// before anyone describes it, as it does at registration; its creator states its `title`,
/// `matters` and `done`, its own program its `track`, and the harness a `title` on the open agent
/// run. A snapshot read as the same session ([`Fold::from_snapshot_as`]) is the fold it was, its
/// identity, ranks and `card_rev` included, and the host's tell after the read moves nothing; a
/// carry adopted with that identity ([`CarryAdopt::ident`]) keeps the card and its revision
/// across its handoff mark the same way. CONTROL: read as nobody, the creator's claims do not
/// stand until the host's tell, which then moves `card_rev` by one; and a tell of another
/// creator, a real change, moves it by exactly one.
#[test]
fn a_carry_keeps_who_the_session_is() {
    let own = sid("s-0wn0000000000000000a");
    let creator = sid("s-cre8000000000000000b");
    let other = sid("s-other00000000000000c");
    let ident = DescIdent {
        own: Some(own),
        creator: Some(creator),
    };
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    log.set_desc_ident(ident.own, ident.creator);
    let by_creator = (Actor::Sid(creator), Src::Mark);
    for (i, (slot, text)) in [
        ("title", "fix the parser"),
        ("matters", "the release waits on it"),
        ("done", "every check is green"),
    ]
    .into_iter()
    .enumerate()
    {
        put(
            &mut log,
            by_creator,
            Node::SESSION,
            say(slot, text),
            10 + i as u64,
        );
    }
    let by_own = (Actor::Sid(own), Src::Mark);
    put(
        &mut log,
        by_own,
        Node::SESSION,
        say("track", "on track"),
        20,
    );
    put(
        &mut log,
        HARNESS,
        run,
        say("title", "reading the lexer"),
        21,
    );
    assert_eq!(
        stands(&log, "title", 30),
        Some(("fix the parser", Rank::Creator)),
        "PRECONDITION: the creator's stated title stands over the harness's"
    );
    let (card_rev, rev) = (log.state().desc.card_rev(), log.state().rev);
    assert!(card_rev > 0);
    let snap = log.fold().snapshot();
    let refs: Vec<&str> = snap.iter().map(String::as_str).collect();

    let mut back = Fold::from_snapshot_as(&refs, ident).expect("the snapshot reads");
    assert_eq!(
        back.state(),
        log.state(),
        "the fold as it stood, who the session is included"
    );
    back.set_desc_ident(ident.own, ident.creator);
    assert_eq!(
        back.state(),
        log.state(),
        "the host's tell after the read moves nothing"
    );

    let out = log.carry(CarryBudget::SESSION, &[]).expect("carries");
    let adopt = CarryAdopt {
        ep: log.ep() + 1,
        next_seq: log.last_seq() + 1000,
        ids: log.id_counters(),
        limits: Limits::SESSION,
        at: at(40),
        ident,
    };
    let (mut carried, _, _) = Log::from_carry(&out.lines, adopt).expect("the carry reads");
    assert_eq!(
        carried.state().desc,
        log.state().desc,
        "across its handoff mark the card is as it was"
    );
    carried.set_desc_ident(ident.own, ident.creator);
    assert_eq!(
        carried.state().desc.card_rev(),
        card_rev,
        "and the host's tell moves nothing"
    );

    let mut nobody = Fold::from_snapshot(&refs).expect("the snapshot reads");
    assert_eq!(
        shown(nobody.state(), "title", WALL0 + 30).map(|s| (s.claim.value.as_str(), s.rank)),
        Some(("reading the lexer", Rank::Program)),
        "CONTROL: read as nobody, the creator's title does not stand"
    );
    nobody.set_desc_ident(ident.own, ident.creator);
    assert_eq!(
        nobody.state().desc.card_rev(),
        card_rev + 1,
        "CONTROL: and the host's tell moves the card's revision"
    );

    back.set_desc_ident(ident.own, Some(other));
    assert_eq!(
        (back.state().desc.card_rev(), back.state().rev),
        (card_rev + 1, rev + 1),
        "CONTROL: another creator moves each by exactly one"
    );
}

/// A bell at `ms`: a record that says nothing of the card. Its seq.
fn bell(log: &mut Log, ms: u64) -> u64 {
    log.mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), at(ms))
        .expect("a bell")
}

/// A `ttl=` LAPSE MOVES THE CARD'S REVISION AT THE NEXT RECORD: a program printed a title and a
/// control client claimed one for ten minutes, which stands over it, and a `matters` nobody else
/// claimed, for as long. A record a minute before the lapse leaves the card as it was (CONTROL);
/// the first record after it moves `card_rev` to at least its seq, the card's title now the
/// printed one, as a reader is shown, and its `matters` gone from it.
#[test]
fn a_ttl_lapse_moves_card_rev_at_the_next_record() {
    let mut log = Log::new(1);
    agent_session(&mut log, 0);
    let printed = Fields::new()
        .tok(Key::Field, "title")
        .and_then(|f| f.text(Key::Title, "vim foo"))
        .expect("an in-band title");
    put(&mut log, INBAND, Node::SESSION, printed, 10);
    let lapsing = |slot: &str, value: &str| say(slot, value).u(Key::Ttl, 600).expect("ttl=");
    let title = lapsing("title", "reviewing the PR");
    put(&mut log, CTL, Node::SESSION, title, 20);
    let matters = lapsing("matters", "the release waits on it");
    put(&mut log, CTL, Node::SESSION, matters, 21);
    let incumbent = |log: &Log, slot: &str| -> Option<Actor> {
        let s = log
            .state()
            .desc
            .subject(Node::SESSION)
            .and_then(|s| s.slot(slot))?;
        s.standing.map(|i| s.claims[i].by)
    };
    assert_eq!(
        stands(&log, "title", 30),
        Some(("reviewing the PR", Rank::Ctl))
    );
    let card_rev = log.state().desc.card_rev();
    let lapse = 20 + 600_000;
    bell(&mut log, lapse - 60_000);
    assert_eq!(
        (log.state().desc.card_rev(), incumbent(&log, "title")),
        (card_rev, Some(Actor::Ctl)),
        "CONTROL: before the lapse nothing moved"
    );
    let seq = bell(&mut log, lapse + 1_000);
    assert!(
        log.state().desc.card_rev() > card_rev && log.state().desc.card_rev() >= seq,
        "the first record after the lapse moves the card's revision"
    );
    assert_eq!(
        incumbent(&log, "title"),
        Some(Actor::Nobody),
        "the card's title is the printed one"
    );
    assert_eq!(
        stands(&log, "title", lapse + 2_000),
        Some(("vim foo", Rank::Inband)),
        "as a reader is shown"
    );
    assert_eq!(
        (
            incumbent(&log, "matters"),
            stands(&log, "matters", lapse + 2_000)
        ),
        (None, None),
        "and the lapsed `matters` left the card"
    );
}

/// A JOURNAL REPLAY JUDGES A HISTORY LINE AS THE LIVE LOG DID: the host says who the session is,
/// then its own program and its creator each write a history line, in turns of their own, and its
/// own program a `title`. A replay of the log's records told the same identity before its first
/// record ([`Log::from_journal`]) keeps both lines, drops what the live log dropped, and gives the
/// same `card_rev` and title. CONTROL: replayed as nobody, the two lines are dropped and counted
/// and the title does not stand, so a later tell could not bring the lines back.
#[test]
fn a_journal_replay_keeps_the_sessions_own_and_its_creators_history_lines() {
    let own = sid("s-0wn0000000000000000a");
    let creator = sid("s-cre8000000000000000b");
    let ident = DescIdent {
        own: Some(own),
        creator: Some(creator),
    };
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    log.set_desc_ident(ident.own, ident.creator);
    let by_own = (Actor::Sid(own), Src::Mark);
    let by_creator = (Actor::Sid(creator), Src::Mark);
    turn(&mut log, run, 10);
    put(
        &mut log,
        by_own,
        Node::SESSION,
        hist("parser fixed", None, None),
        12,
    );
    turn(&mut log, run, 20);
    put(
        &mut log,
        by_creator,
        Node::SESSION,
        hist("review asked for", None, None),
        22,
    );
    put(
        &mut log,
        by_own,
        Node::SESSION,
        say("title", "the lexer"),
        30,
    );
    assert_eq!(
        lines_of(&log, Node::SESSION),
        ["parser fixed", "review asked for"]
    );
    let img = JournalImage {
        ep: log.ep(),
        base: log.base_ep(),
        first: log.first_seq(),
        head: log.last_seq(),
        recs: log.window().cloned().collect(),
        holes: Vec::new(),
    };
    let back = Log::from_journal(&img, ident).expect("the journal image reads");
    let card = |log: &Log| {
        let d = &log.state().desc;
        (
            d.history(Node::SESSION).to_vec(),
            d.lines_dropped(),
            d.card_rev(),
        )
    };
    assert_eq!(card(&back), card(&log), "the replay judged each line alike");
    assert_eq!(
        stands(&back, "title", 40),
        Some(("the lexer", Rank::Program))
    );

    let nobody = Log::from_journal(&img, DescIdent::default()).expect("the journal image reads");
    assert!(
        lines_of(&nobody, Node::SESSION).is_empty(),
        "CONTROL: replayed as nobody, the lines are lost"
    );
    assert_eq!(
        nobody.state().desc.lines_dropped(),
        log.state().desc.lines_dropped() + 2,
        "CONTROL: and counted"
    );
    assert_eq!(stands(&nobody, "title", 40), None, "CONTROL");
}

/// A CLOSED SESSION'S CARD IS KEPT AS WHO THE SESSION WAS, AND ITS SUMMARY HOLDS NONE OF IT: the
/// host says who the session is; its own program states its `title` and its creator its
/// `matters`, each writes a history line in a turn of its own, and the session closes. The log a
/// close keeps whole ([`Log::kept_within`], the newest closes' skeleton) is the fold as it stood,
/// its identity included, so with no identity passed it shows the same `shown`, `shown_history`
/// and `card_rev`. The summary ([`Log::closed_summary`]) keeps no description claim and no line:
/// read back it shows no card, as nobody and after the host's tell of who the session was, so no
/// identity, carried in it or passed to its reader, changes what it holds. CONTROL: the carry's
/// description lines spliced into the summary are refused as a description claim, where read as
/// [`Log::from_closed_summary`] reads a fold, as nobody, the program's title and the creator's
/// `matters` would not stand (they do under the session's identity).
#[test]
fn a_closed_sessions_card_keeps_who_it_was_and_its_summary_holds_none() {
    let own = sid("s-0wn0000000000000000a");
    let creator = sid("s-cre8000000000000000b");
    let ident = DescIdent {
        own: Some(own),
        creator: Some(creator),
    };
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    log.set_desc_ident(ident.own, ident.creator);
    let by_own = (Actor::Sid(own), Src::Mark);
    let by_creator = (Actor::Sid(creator), Src::Mark);
    put(
        &mut log,
        by_own,
        Node::SESSION,
        say("title", "fix the parser"),
        5,
    );
    put(
        &mut log,
        by_creator,
        Node::SESSION,
        say("matters", "the release waits on it"),
        6,
    );
    turn(&mut log, run, 10);
    put(
        &mut log,
        by_own,
        Node::SESSION,
        hist("parser fixed", None, None),
        12,
    );
    turn(&mut log, run, 20);
    put(
        &mut log,
        by_creator,
        Node::SESSION,
        hist("review asked for", None, None),
        22,
    );
    log.close_session(
        Close::new(Out::Session(SessionOut::UiClose), Src::Aterm, Conf::Exact),
        at(40),
    )
    .expect("the session closes");
    let now = WALL0 + 50;
    let card = |log: &Log| {
        let st = log.state();
        let slots = ["title", "matters"]
            .map(|slot| shown(st, slot, now).map(|s| (s.claim.value.clone(), s.rank)));
        let lines: Vec<(String, Rank)> = shown_history(st, DESC_HISTORY)
            .iter()
            .map(|r| (r.line.value.clone(), r.rank))
            .collect();
        (slots, lines, st.desc.card_rev())
    };
    let live = card(&log);
    assert_eq!(
        live.0,
        [
            Some(("fix the parser".to_string(), Rank::Program)),
            Some(("the release waits on it".to_string(), Rank::Creator)),
        ],
        "PRECONDITION: the program's title and the creator's reason stand"
    );
    assert_eq!(
        live.1,
        [
            ("review asked for".to_string(), Rank::Creator),
            ("parser fixed".to_string(), Rank::Program),
        ],
        "PRECONDITION: both lines are kept, newest first"
    );
    assert!(live.2 > 0, "PRECONDITION: the card moved");

    let kept = log.kept_within(64 * 1024);
    assert_eq!(kept.state().desc.ident(), &ident, "the kept log knows who");
    assert_eq!(card(&kept), live, "and shows the card as the live log did");

    let summary = log.closed_summary();
    let blank = ([None, None], Vec::new(), 0);
    let mut back = Log::from_closed_summary(&summary).expect("the summary reads");
    assert_eq!(card(&back), blank, "the summary holds no card");
    back.set_desc_ident(ident.own, ident.creator);
    assert_eq!(
        (card(&back), back.state().rev),
        (
            blank,
            Log::from_closed_summary(&summary)
                .expect("reads")
                .state()
                .rev
        ),
        "and the host's tell of who the session was moves nothing"
    );

    let desc_lines: Vec<String> = log
        .fold()
        .snapshot()
        .into_iter()
        .filter(|l| l.starts_with("fold desc"))
        .collect();
    assert!(
        desc_lines.iter().any(|l| l.starts_with("fold desc-line ")),
        "CONTROL: the carry holds the card and its lines"
    );
    let forged = format!("{summary}\n{}", desc_lines.join("\n"));
    assert_eq!(
        Log::from_closed_summary(&forged).err(),
        Some(CarryError::Shape("a summary holding a description claim")),
        "CONTROL: a summary holding the card is refused"
    );
    let fold_lines: Vec<&str> = forged.lines().filter(|l| l.starts_with("fold ")).collect();
    let as_of = |fold: &Fold| {
        ["title", "matters"]
            .map(|slot| shown(fold.state(), slot, now).map(|s| (s.claim.value.clone(), s.rank)))
    };
    assert_eq!(
        as_of(&Fold::from_snapshot(&fold_lines).expect("the fold lines read")),
        [None, None],
        "CONTROL: read as nobody, neither claim would stand"
    );
    assert_eq!(
        as_of(&Fold::from_snapshot_as(&fold_lines, ident).expect("the fold lines read")),
        live.0,
        "CONTROL: as the session, both do"
    );
}

/// A BIRTH CLAIM IS TOLD BY ITS RECORD'S `conf` AND `as=`, NEVER ITS TEXT: every claim of a slot
/// holds the same text, and a creator's or a control client's own at `src=mark conf=exact`, or aterm's
/// derived one at `conf=exact` naming a spawn's word (`as=why`, `creator`, `spawn`), is a birth
/// claim. CONTROLS: the same authors at `conf=claim` (the `mark` verb's), aterm's default writer
/// at `conf=heuristic` naming the same words or `task`, aterm's derived claim at `conf=exact`
/// naming a word no spawn writes (`prompt`, none), and a person's or the program's at
/// `conf=exact` are not.
#[test]
fn birth_claims_are_told_by_conf_and_as_not_text() {
    let creator = sid("s-cre8or0000000000000a");
    let by_creator = (Actor::Sid(creator), Src::Mark);
    let cases: [(&str, (Actor, Src), Conf, &str, Option<&str>, bool); 15] = [
        (
            "the creator's at birth",
            by_creator,
            Conf::Exact,
            "title",
            Some("task"),
            true,
        ),
        (
            "the creator's, no as=",
            by_creator,
            Conf::Exact,
            "matters",
            None,
            true,
        ),
        ("ctl's at birth", CTL, Conf::Exact, "matters", None, true),
        (
            "aterm's why=",
            T0,
            Conf::Exact,
            "matters",
            Some("why"),
            true,
        ),
        (
            "aterm's copy",
            T0,
            Conf::Exact,
            "matters",
            Some("creator"),
            true,
        ),
        (
            "aterm's package=",
            T0,
            Conf::Exact,
            "package",
            Some("spawn"),
            true,
        ),
        (
            "CONTROL: the creator's mark",
            by_creator,
            Conf::Claim,
            "title",
            Some("task"),
            false,
        ),
        (
            "CONTROL: ctl's mark",
            CTL,
            Conf::Claim,
            "matters",
            None,
            false,
        ),
        (
            "CONTROL: T0's task",
            T0,
            Conf::Heuristic,
            "title",
            Some("task"),
            false,
        ),
        (
            "CONTROL: T0's why",
            T0,
            Conf::Heuristic,
            "matters",
            Some("why"),
            false,
        ),
        (
            "CONTROL: T0's spawn",
            T0,
            Conf::Heuristic,
            "package",
            Some("spawn"),
            false,
        ),
        (
            "CONTROL: aterm's prompt",
            T0,
            Conf::Exact,
            "title",
            Some("prompt"),
            false,
        ),
        (
            "CONTROL: aterm's, no as=",
            T0,
            Conf::Exact,
            "matters",
            None,
            false,
        ),
        (
            "CONTROL: a person's",
            PERSON,
            Conf::Exact,
            "matters",
            None,
            false,
        ),
        (
            "CONTROL: the program's",
            HARNESS,
            Conf::Exact,
            "matters",
            None,
            false,
        ),
    ];
    for (name, who, conf, slot, as_name, want) in cases {
        let mut log = Log::new(1);
        log.set_desc_ident(Some(sid("s-0wn0000000000000000a")), Some(creator));
        // One text a slot: a `package` holds tracker ids.
        let text = if slot == "package" {
            "FL-04"
        } else {
            "fix the parser"
        };
        let mut f = say(slot, text);
        if let Some(as_name) = as_name {
            f = f.text(Key::As, as_name).expect("as=");
        }
        log.mark(
            Mark::new(Kind::Desc, who.1, conf)
                .by(who.0)
                .at(Node::SESSION)
                .fields(f),
            at(10),
        )
        .expect("a desc mark");
        let s = shown(log.state(), slot, WALL0 + 100).expect("PRECONDITION: the claim stands");
        assert_eq!(s.claim.value, text, "{name}");
        assert_eq!(is_birth(s.claim, s.rank), want, "{name}");
    }
}
