// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Description claims, host side: what `mark … desc` writes and refuses, where `-` lands,
//! the redaction at the write, and the `described` reply in lines and in JSON.

use aterm_slog::{Actor, Conf, Node};

use super::tests::{Hand, WALL0, hand_clock, lines_of, with_claude};
use super::{DescRefused, DescWrite, MarkReq, cmd_described, cmd_described_json, cmd_mark};
use crate::session_store::{SessionHandle, test_handle};

/// A session whose log is open on a hand clock, its root shell known, so an agent can run in
/// it.
fn session(local: u64) -> (SessionHandle, Hand) {
    let h = test_handle(local);
    let hand = hand_clock(&h.ctx.timeline);
    {
        let mut g = h.ctx.timeline.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().open_session(None, None, now);
        g.note_root_group(100);
    }
    (h, hand)
}

/// `mark <rest>` by `by`, a reader; the reply.
fn mark(h: &SessionHandle, by: Actor, rest: &str) -> String {
    cmd_mark(&h.ctx, by, true, rest).0
}

/// The seq an `OK seq=<n> node=… by=…` reply names.
fn seq_of(reply: &str) -> u64 {
    reply
        .strip_prefix("OK seq=")
        .and_then(|r| r.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no seq in {reply:?}"))
}

/// The `desc` records of the session's log, at the host tier.
fn desc_lines(h: &SessionHandle) -> Vec<String> {
    lines_of(&h.ctx.timeline)
        .into_iter()
        .filter(|l| l.contains(" desc "))
        .collect()
}

/// The open agent run the fold reads as the session's second scope.
fn open_agent(h: &SessionHandle) -> Option<Node> {
    h.ctx
        .timeline
        .lock()
        .unwrap()
        .slog()
        .log()
        .state()
        .desc
        .open_agent()
}

/// A sid no session of the test holds: another session's author.
fn stranger() -> Actor {
    Actor::Sid(aterm_slog::ShortId::new("s-00000000000000000abc").expect("a sid"))
}

/// `described` prints, for every slot, the claim that stands with its scope, author, rank,
/// `as=` name, turns behind and seq, then each other author's claim as an `other` line, and
/// formats the reply with the timeline unlocked.
///
/// CONTROL: before any claim the same read is the two known slots, unset; and the one slot
/// four authors claim prints four different `rank=` words, so the rank is each author's and
/// not the slot's.
#[test]
fn described_prints_every_slot_with_its_author_and_rank() {
    let (h, hand) = session(81);
    let sid = h.ctx.self_id.as_str().to_string();
    let ep = h.ctx.timeline.lock().unwrap().slog().log().ep();
    assert_eq!(
        cmd_described(&h.ctx, ""),
        format!(
            "OK 2 described v=1 sid={sid} as_of=1 ep={ep} trust=screen\n\
             title - value=-\n\
             description - value=-\n"
        )
    );

    hand.set(10);
    let person = h
        .ctx
        .timeline
        .lock()
        .unwrap()
        .slog_mut()
        .claim_desc(
            &DescWrite::of_session("title", Some("Fold laws")),
            Actor::Human,
            super::tests::at(10),
        )
        .expect("a person's title")
        .0;
    with_claude(&h.ctx.timeline);
    let agent = open_agent(&h).expect("the agent run is open");
    hand.set(20);
    let head = h.ctx.timeline.lock().unwrap().slog().log().last_seq();
    let ctl = seq_of(&mark(
        &h,
        Actor::Ctl,
        &format!("- desc field=title value=Review%20the%20fold as=reviewer upto={head}"),
    ));
    let other = seq_of(&mark(&h, stranger(), "- desc field=title value=mine"));
    let program = seq_of(&mark(
        &h,
        Actor::Harness,
        "- desc field=description value=Reading%20fold_desc.rs",
    ));
    let aterm = seq_of(&mark(&h, Actor::Aterm, "- desc field=title value=-"));
    let stage = seq_of(&mark(&h, Actor::Ctl, "- desc field=stage value=review"));

    let held = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let probe_held = std::sync::Arc::clone(&held);
    let ctx = std::sync::Arc::clone(&h.ctx);
    super::ENCODE_PROBE.with(|p| {
        *p.borrow_mut() = Some(Box::new(move || {
            probe_held
                .lock()
                .unwrap()
                .push(ctx.timeline.try_lock().is_err());
        }));
    });
    let reply = cmd_described(&h.ctx, "");
    super::ENCODE_PROBE.with(|p| *p.borrow_mut() = None);
    assert_eq!(
        *held.lock().unwrap(),
        vec![false],
        "the reply is formatted once, with the timeline unlocked"
    );
    let as_of = h.ctx.timeline.lock().unwrap().slog().log().last_seq();
    assert_eq!(
        reply,
        format!(
            "OK 6 described v=1 sid={sid} as_of={as_of} ep={ep} trust=screen\n\
             title - value=Fold%20laws by=human rank=person behind=- seq={person}\n\
             title {agent} other value=%2D by=aterm rank=writer behind=0 seq={aterm}\n\
             title {agent} other value=Review%20the%20fold by=ctl rank=ctl as=reviewer behind=0 \
             seq={ctl}\n\
             title {agent} other value=mine by=s-00000000000000000abc rank=listed behind=- \
             seq={other}\n\
             description {agent} value=Reading%20fold_desc.rs by=harness rank=program behind=0 \
             seq={program}\n\
             stage {agent} value=review by=ctl rank=ctl behind=- seq={stage}\n"
        )
    );
    assert_eq!(
        cmd_described(&h.ctx, "title"),
        super::desc::DESCRIBED_USAGE,
        "the verb takes no argument"
    );
}

/// `described --json` is one object: a slot nobody claimed is `null`, and a claim whose
/// `behind` nobody knows reads `null`, never 0.
///
/// CONTROL: the claimed slot of the same reply is an object with its text, author and rank,
/// and a claim whose writer named `upto=` reads `behind` as the number 0.
#[test]
fn described_json_reads_an_unset_slot_as_null() {
    let (h, _hand) = session(82);
    let seq = seq_of(&mark(
        &h,
        Actor::Ctl,
        "- desc field=title value=a%20%22quoted%22%20title",
    ));
    let reply = cmd_described_json(&h.ctx, "");
    let line = reply.strip_prefix("OK 1\n").expect("one line");
    let v: aterm_json::Value = aterm_json::from_str(line.trim_end()).expect("one JSON object");
    assert_eq!(v["v"].as_u64(), Some(1));
    assert_eq!(v["sid"].as_str(), Some(h.ctx.self_id.as_str()));
    assert_eq!(v["as_of"].as_u64(), Some(seq));
    assert!(v["ep"].as_u64().is_some(), "{line}");
    assert!(
        v["slots"]
            .get("description")
            .is_some_and(aterm_json::Value::is_null),
        "an unset slot is there, and null: {line}"
    );
    let title = &v["slots"]["title"];
    assert_eq!(title["value"].as_str(), Some("a \"quoted\" title"));
    assert_eq!(title["scope"].as_str(), Some("-"));
    assert_eq!(title["by"].as_str(), Some("ctl"));
    assert_eq!(title["rank"].as_str(), Some("ctl"));
    assert!(
        title.get("as").is_some_and(aterm_json::Value::is_null),
        "{line}"
    );
    assert!(
        title.get("behind").is_some_and(aterm_json::Value::is_null),
        "unknown is null, never 0: {line}"
    );
    assert_eq!(title["seq"].as_u64(), Some(seq));
    assert_eq!(title["dropped"].as_u64(), Some(0));
    assert_eq!(title["others"].as_array().map(Vec::len), Some(0));

    let known = seq_of(&mark(
        &h,
        Actor::Ctl,
        &format!("- desc field=title value=read upto={seq}"),
    ));
    let listed = seq_of(&mark(
        &h,
        stranger(),
        "- desc field=description value=theirs",
    ));
    let reply = cmd_described_json(&h.ctx, "");
    let line = reply.strip_prefix("OK 1\n").expect("one line");
    let v: aterm_json::Value = aterm_json::from_str(line.trim_end()).expect("one JSON object");
    assert_eq!(v["slots"]["title"]["seq"].as_u64(), Some(known));
    assert_eq!(v["slots"]["title"]["behind"].as_u64(), Some(0), "{line}");
    let description = &v["slots"]["description"];
    assert!(
        description
            .get("value")
            .is_some_and(aterm_json::Value::is_null)
            && description
                .get("by")
                .is_some_and(aterm_json::Value::is_null),
        "a slot whose only claim never stands shows no claim: {line}"
    );
    let theirs = &description["others"][0];
    assert_eq!(theirs["seq"].as_u64(), Some(listed));
    assert_eq!(theirs["rank"].as_str(), Some("listed"));
    assert_eq!(theirs["value"].as_str(), Some("theirs"));
    assert_eq!(theirs["scope"].as_str(), Some("-"));
}

/// The `desc` grammar refuses, and never rewrites: a slot that is not a canonical token, an
/// empty value, a value of two lines or with hidden formatting, a value over 256 encoded
/// bytes, an `upto=` past the record itself, and any node but `-` or an open agent run.
///
/// CONTROL: each request with its fault taken out is written (`OK`), and nothing a refusal
/// named reached the log.
#[test]
fn desc_mark_grammar_refuses_what_a_description_may_not_say() {
    let slot = "ERR mark: field= is a slot name of lower-case [a-z0-9.-], at most 24\n";
    let refused = |rest: &str| MarkReq::parse(rest).expect_err(rest);
    for bad in [
        "- desc field=Title value=x",
        "- desc field=a_b value=x",
        "- desc field=title%20 value=x",
        &format!("- desc field={} value=x", "s".repeat(25)),
    ] {
        assert_eq!(refused(bad), slot, "{bad}");
    }
    assert_eq!(
        refused("- desc field=title value=%20%20"),
        "ERR mark: value is empty\n"
    );
    for bad in [
        "- desc field=title value=one%0Atwo",
        "- desc field=title value=a%E2%80%AEb",
    ] {
        assert_eq!(
            refused(bad),
            "ERR mark: value has forbidden formatting\n",
            "{bad}"
        );
    }
    assert_eq!(
        refused(&format!("- desc field=title value={}", "x".repeat(257))),
        "ERR mark: value over 256 bytes encoded\n"
    );
    assert_eq!(
        refused(&format!(
            "- desc field=title value={}",
            "%E6%9E%84".repeat(86)
        )),
        "ERR mark: value over 256 bytes encoded\n",
        "the cap counts encoded bytes: 86 three-byte characters are 774"
    );
    for bad in ["t1 desc field=title value=x", "p1 desc field=title value=x"] {
        assert_eq!(
            refused(bad),
            "ERR mark: a desc is the session's (-) or an open agent run's (r<n>)\n",
            "{bad}"
        );
    }
    for bad in [
        "- desc value=x",
        "- desc field=title word=done",
        "- desc field=title text=x",
        "- desc field=title upto=soon",
        "- desc field=title field=title",
        "- desc field=title value=",
    ] {
        assert_eq!(refused(bad), super::MARK_USAGE, "{bad}");
    }
    assert_eq!(
        refused("- desc field=title ttl=86401"),
        "ERR mark: ttl is 1..=86400\n"
    );
    for ok in [
        "- desc field=title value=x",
        "- desc field=a.b-9 value=x",
        "- desc field=title",
        &format!("- desc field={} value={}", "s".repeat(24), "x".repeat(256)),
        "r3 desc field=title value=x upto=7 ttl=86400 as=lead re=3",
    ] {
        assert!(MarkReq::parse(ok).is_ok(), "{ok}: {:?}", MarkReq::parse(ok));
    }

    let (h, hand) = session(83);
    hand.set(10);
    let program = h
        .ctx
        .timeline
        .lock()
        .unwrap()
        .slog_mut()
        .log_mut_for_journal()
        .open(
            aterm_slog::Open::new(aterm_slog::Kind::Cmd, aterm_slog::Src::Shell, Conf::Strong),
            super::tests::at(10),
        )
        .expect("a command's run opens");
    let head = h.ctx.timeline.lock().unwrap().slog().log().last_seq();
    assert_eq!(
        mark(
            &h,
            Actor::Ctl,
            &format!("{program} desc field=title value=x")
        ),
        format!("ERR mark: {program} is not an agent run\n")
    );
    assert_eq!(
        mark(&h, Actor::Ctl, "r999 desc field=title value=x"),
        "ERR mark: no node r999\n"
    );
    assert_eq!(
        mark(
            &h,
            Actor::Ctl,
            &format!("- desc field=title value=x upto={}", head + 2)
        ),
        format!("ERR mark: upto={} is ahead of the log\n", head + 2)
    );
    assert_eq!(
        desc_lines(&h),
        Vec::<String>::new(),
        "a refusal writes nothing"
    );
    let reply = mark(
        &h,
        Actor::Ctl,
        &format!("- desc field=title value=x upto={}", head + 1),
    );
    assert_eq!(
        reply,
        format!("OK seq={} node=- by=ctl\n", head + 1),
        "the record itself is the furthest a claim covers"
    );
    assert_eq!(
        desc_lines(&h),
        vec![format!(
            "{} mark - desc field=title value=x upto={} by=ctl src=mark conf=claim t={}",
            head + 1,
            head + 1,
            WALL0 + 10
        )]
    );
}

/// `-` is the session's open agent run, else the session: a claim written while an agent
/// runs is the agent's and lapses with it, and the session's own stays.
///
/// CONTROL: the same request written before the agent opened, and again after it ended,
/// lands on the session (`node=-`).
#[test]
fn a_desc_on_dash_lands_on_the_open_agent_else_the_session() {
    let (h, hand) = session(84);
    let before = mark(&h, Actor::Ctl, "- desc field=description value=a%20shell");
    assert!(before.ends_with(" node=- by=ctl\n"), "{before}");

    with_claude(&h.ctx.timeline);
    let agent = open_agent(&h).expect("the agent run is open");
    hand.set(2_000);
    let during = mark(&h, Actor::Ctl, "- desc field=description value=reviewing");
    assert!(
        during.ends_with(&format!(" node={agent} by=ctl\n")),
        "{during}"
    );
    let named = mark(
        &h,
        Actor::Harness,
        &format!("{agent} desc field=title value=the%20agent"),
    );
    assert!(
        named.ends_with(&format!(" node={agent} by=harness\n")),
        "{named}"
    );
    let listed = cmd_described(&h.ctx, "");
    assert!(
        listed.contains(&format!("\ndescription - value=a%20shell by=ctl rank=ctl "))
            && listed.contains(&format!(
                "\ndescription {agent} other value=reviewing by=ctl rank=ctl "
            ))
            && listed.contains(&format!(
                "\ntitle {agent} value=the%20agent by=harness rank=program "
            )),
        "a tie of rank goes to the session, and the agent's is listed: {listed}"
    );

    hand.set(4_000);
    h.ctx.timeline.lock().unwrap().note_foreground_group(100);
    assert_eq!(open_agent(&h), None, "the agent ended");
    assert_eq!(
        mark(
            &h,
            Actor::Harness,
            &format!("{agent} desc field=title value=late")
        ),
        format!("ERR mark: {agent} is not open\n")
    );
    hand.set(6_000);
    let after = mark(&h, Actor::Ctl, "- desc field=stage value=idle");
    assert!(after.ends_with(" node=- by=ctl\n"), "{after}");
    let listed = cmd_described(&h.ctx, "");
    assert!(
        listed.contains("\ntitle - value=-\n")
            && listed.contains("\ndescription - value=a%20shell by=ctl ")
            && !listed.contains("reviewing"),
        "the agent's claims lapsed with it and the session's stayed: {listed}"
    );
}

/// A writer that may not read the log may not pass `upto=`: the refusal of one ahead of the
/// log would tell it where the log's head is.
///
/// CONTROL: the same writer's claim with no `upto=` is written, and its reply names no seq
/// and no node.
#[test]
fn a_desc_writer_that_may_not_read_names_no_upto() {
    let (h, _hand) = session(85);
    for probe in [
        "- desc field=title value=x upto=1",
        "- desc field=title value=x upto=999999",
    ] {
        assert_eq!(
            cmd_mark(&h.ctx, stranger(), false, probe).0,
            "ERR denied\n",
            "{probe}"
        );
    }
    assert_eq!(
        cmd_mark(&h.ctx, stranger(), false, "- desc field=title value=x").0,
        "OK by=s-00000000000000000abc\n"
    );
}

/// A value of which redaction leaves nothing but its notices is refused for every author but
/// a person, be it a sensitive phrase (the whole-line notice) or a bare token (`[redacted]`
/// alone), and one it redacts in part is stored redacted: the record never holds the
/// credential, and no claim says only that something was redacted.
///
/// CONTROL: a person's claim of the same texts is stored as typed, and a value with no
/// letter or digit that holds no credential is stored for any author.
#[test]
fn a_value_that_reads_whole_as_a_credential_is_refused() {
    let (h, _hand) = session(86);
    let key = format!("sk-proj-{}", "a1B2c3D4".repeat(4));
    let whole = "- desc field=description value=password%20hunter2";
    let part = format!("- desc field=description value=use%20{key}%20here");
    for by in [Actor::Ctl, Actor::Harness, Actor::Aterm, stranger()] {
        assert_eq!(
            mark(&h, by, whole),
            "ERR mark: value reads as a credential\n",
            "{by:?}"
        );
    }
    assert_eq!(desc_lines(&h), Vec::<String>::new());
    let bare = [format!("ghp_{}", "0123456789abcdefghij"), "ab12".repeat(16)];
    for token in &bare {
        for by in [Actor::Ctl, Actor::Harness, Actor::Aterm, stranger()] {
            assert_eq!(
                mark(&h, by, &format!("- desc field=description value={token}")),
                "ERR mark: value reads as a credential\n",
                "{by:?} {token}"
            );
        }
        assert_eq!(
            super::desc_stored(&format!("({token})."), Actor::Ctl),
            Err(DescRefused::Credential),
            "punctuation around the notice says nothing either"
        );
    }
    assert_eq!(desc_lines(&h), Vec::<String>::new());
    assert!(mark(&h, Actor::Ctl, &part).starts_with("OK seq="));
    let lines = desc_lines(&h);
    assert!(
        lines.len() == 1 && lines[0].contains(" value=use%20[redacted]%20here by=ctl "),
        "{lines:?}"
    );
    assert!(mark(&h, Actor::Human, whole).starts_with("OK seq="));
    assert!(mark(&h, Actor::Human, &part).starts_with("OK seq="));
    let lines = desc_lines(&h);
    assert!(
        lines[1].contains(" value=password%20hunter2 by=human ")
            && lines[2].contains(&format!(" value=use%20{key}%20here by=human ")),
        "a person's text is theirs: {lines:?}"
    );
    for (i, token) in bare.iter().enumerate() {
        let typed = format!("- desc field=description value={token}");
        assert!(mark(&h, Actor::Human, &typed).starts_with("OK seq="));
        let lines = desc_lines(&h);
        assert!(
            lines[3 + i].contains(&format!(" value={token} by=human ")),
            "a person's bare token is theirs: {lines:?}"
        );
    }
    assert_eq!(
        super::desc_stored("  spaced out  ", Actor::Ctl).as_deref(),
        Ok("spaced out")
    );
    assert_eq!(super::desc_stored("->", Actor::Ctl).as_deref(), Ok("->"));
    assert_eq!(
        super::desc_stored(&"x".repeat(257), Actor::Human),
        Err(DescRefused::TooLong)
    );
}

/// A description spends its author's mark tokens like any other claim: the twenty-first in
/// a burst is busy, per author. A person's is never refused for rate.
///
/// CONTROL: another author's claim at the same moment is written, and a second later the
/// busy author's is too.
#[test]
fn the_twenty_first_desc_is_busy_and_a_person_is_never() {
    let (h, hand) = session(87);
    for i in 0..aterm_slog::MARK_BURST {
        let reply = mark(&h, Actor::Ctl, &format!("- desc field=title value=t{i}"));
        assert!(reply.starts_with("OK seq="), "{i}: {reply}");
    }
    let busy = mark(&h, Actor::Ctl, "- desc field=title value=one-more");
    assert!(busy.starts_with("ERR busy retry_ms="), "{busy}");
    assert!(mark(&h, Actor::Harness, "- desc field=title value=mine").starts_with("OK seq="));
    for i in 0..aterm_slog::MARK_BURST + 5 {
        let seq = h.ctx.timeline.lock().unwrap().slog_mut().claim_desc(
            &DescWrite::of_session("title", Some(&format!("rename {i}"))),
            Actor::Human,
            super::tests::at(0),
        );
        assert!(seq.is_ok(), "{i}: {seq:?}");
    }
    hand.advance(aterm_slog::MARK_REFILL_MS);
    assert!(mark(&h, Actor::Ctl, "- desc field=title value=one-more").starts_with("OK seq="));
}

/// A claim's `ttl` lapse is judged at the read: once it runs out the claim no longer stands,
/// and `described` lists it as lapsed.
///
/// CONTROL: one second before its `ttl` ends the same claim stands.
#[test]
fn a_lapsed_desc_is_listed_and_no_longer_stands() {
    let (h, hand) = session(88);
    let seq = seq_of(&mark(
        &h,
        Actor::Ctl,
        "- desc field=title value=for%20now ttl=60",
    ));
    hand.set(59_000);
    assert!(cmd_described(&h.ctx, "").contains(&format!(
        "\ntitle - value=for%20now by=ctl rank=ctl behind=- seq={seq}\n"
    )),);
    hand.set(60_000);
    let reply = cmd_described(&h.ctx, "");
    assert!(
        reply.contains(&format!(
            "\ntitle - value=-\n\
             title - other value=for%20now by=ctl rank=ctl behind=- seq={seq} lapsed=1\n"
        )),
        "{reply}"
    );
    let json = cmd_described_json(&h.ctx, "");
    let v: aterm_json::Value =
        aterm_json::from_str(json.strip_prefix("OK 1\n").expect("one line").trim_end())
            .expect("one JSON object");
    assert!(
        v["slots"]["title"]
            .get("value")
            .is_some_and(aterm_json::Value::is_null),
        "{json}"
    );
    assert_eq!(
        v["slots"]["title"]["others"][0]["lapsed"].as_bool(),
        Some(true),
        "{json}"
    );
}
