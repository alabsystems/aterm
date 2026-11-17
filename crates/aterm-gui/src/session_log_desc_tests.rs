// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Description claims, host side: what `mark … desc` writes and refuses, where `-` lands,
//! the redaction at the write, and the `described` reply in lines and in JSON.

use aterm_slog::{Actor, ClaimRefused, Conf, Key, Kind, Mark, Node, Src};

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

/// A minted sid no session of the test holds: the creator a task's `from=` names.
const CREATOR: &str = "s-0000000000000000c0de";

/// aterm's own writer's line on `on`: a history line reporting a commit, record `re` (`by=aterm
/// src=aterm`, rank derived). The `mark` verb writes `src=mark`, a writer's rank, so it cannot
/// write one.
fn event_line(h: &SessionHandle, on: Node, text: &str, re: u64) -> u64 {
    let f = aterm_slog::Fields::new()
        .tok(Key::Field, aterm_slog::HISTORY)
        .and_then(|f| f.text(Key::Value, text))
        .and_then(|f| f.text(Key::As, "commit"))
        .and_then(|f| f.id(Key::Re, &re.to_string()))
        .expect("an event line's fields");
    let mut tl = h.ctx.timeline.lock().unwrap();
    let at = tl.slog().now();
    tl.slog_mut()
        .log_mut_for_tests()
        .mark(
            Mark::new(Kind::Desc, Src::Aterm, Conf::Heuristic)
                .at(on)
                .by(Actor::Aterm)
                .fields(f),
            at,
        )
        .expect("aterm's event line")
}

/// The agent's word, as a screen read with Claude Code's reader publishes it.
fn agent_says(h: &SessionHandle, word: &'static str) {
    h.ctx.timeline.lock().unwrap().publish_agent(
        word,
        None,
        None,
        Some(aterm_phase::Program::Claude),
        super::tests::stamp(1),
    );
}

/// Seed the session's task record: `state`, created by `from`.
fn task_is(h: &SessionHandle, state: &str, from: &str) {
    let row = format!("rev=1 state={state} from={from} since={WALL0} result_bytes=0 attention=0");
    let record = crate::task_record::TaskRecord::from_carry_row(
        &row,
        None,
        std::time::Instant::now(),
        WALL0,
    )
    .expect("a task row");
    h.ctx.timeline.lock().unwrap().seed_task(record);
}

/// The JSON object a `--json` reply holds.
fn json_of(reply: &str) -> aterm_json::Value {
    let line = reply.strip_prefix("OK 1\n").expect("one line");
    aterm_json::from_str(line.trim_end()).expect("one JSON object")
}

/// The word `described` prints after `state word=`.
fn state_word(h: &SessionHandle) -> String {
    let reply = cmd_described(&h.ctx, "");
    reply
        .split_once("\nstate word=")
        .and_then(|(_, rest)| rest.split([' ', '\n']).next())
        .unwrap_or_else(|| panic!("no state line: {reply}"))
        .to_string()
}

/// `described` prints the whole card: every slot's standing claim with its scope, author, rank,
/// evidence class, `as=` name, the record it cites, turns behind and seq, each other author's
/// claim as an `other` line and a judge's `verdict word=done` under `done`; then the newest
/// history lines, newest first, the event word only on aterm's own; then the eight derived lines:
/// `state word=` with no time, the two `observed` lines, `judged track= track_why= used=` and
/// `label word= line=` (each key `-`, since aterm observes and judges nothing yet), `is`, `born`
/// and `agent`. The reply is formatted with the timeline unlocked.
///
/// CONTROL: before any claim the same read is the six known slots, unset, no history line, and
/// derived lines that read `-` for every unread key (a shell's activity among them) and `turns=0`
/// as a count; at work the `state` word is a value while the observation, judgement and label
/// lines still read `-` for every key; the one `title` slot four authors claim prints four
/// `rank=` words and the three history lines three `ev=` classes, so the class is each author's
/// and not the slot's; the harness's `as=landed` prints no `event=` while aterm's `as=commit`
/// does; and `described title` answers the usage line.
#[test]
fn described_prints_slots_history_derived_lines_and_evidence() {
    let (h, hand) = session(81);
    let sid = h.ctx.self_id.as_str().to_string();
    let (ep, base, opened) = {
        let tl = h.ctx.timeline.lock().unwrap();
        let log = tl.slog().log();
        (
            log.ep(),
            log.base_ep(),
            tl.slog().opened_ms().expect("the log opened"),
        )
    };
    hand.set(7);
    assert_eq!(
        cmd_described(&h.ctx, ""),
        format!(
            "OK 14 described v=1 sid={sid} as_of=1 ep={ep} base={base} chapter=- rev=0 \
             trust=screen\n\
             title - value=-\n\
             matters - value=-\n\
             description - value=-\n\
             track - value=-\n\
             done - value=-\n\
             package - value=-\n\
             state word=-\n\
             observed tries=- try=- on_main=- running=- red=- reds=- conflicts=- landed=- \
             receipt=- ahead=- files=- turns=- work_min=- errors=- task=- need=- limit=- \
             says_done=- working=- ended=-\n\
             observed conflict paths=-\n\
             judged track=- track_why=- used=-\n\
             label word=- line=-\n\
             is conclusion=- landed=- away=-\n\
             born opened_ms={opened} age_ms={} by=- kind=- via=- title_src=- matters_src=none\n\
             agent kind=- run=- turns=0 need=- tok_in=- tok_out=- wall_s=-\n",
            WALL0 + 7 - opened
        )
    );

    hand.set(9);
    let plan = seq_of(&mark(
        &h,
        Actor::Aterm,
        "- desc field=history value=Plan%20read as=planner",
    ));
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
    agent_says(&h, "busy");
    hand.set(30);
    let said = seq_of(&mark(
        &h,
        Actor::Harness,
        "- desc field=history value=Read%20fold_desc.rs as=landed",
    ));
    hand.set(40);
    let commit = event_line(&h, agent, "Committed 9f3c2ab41d07", ctl);
    let done = seq_of(&mark(
        &h,
        Actor::Ctl,
        &format!("- desc field=done value=tests%20pass re={ctl}"),
    ));
    let verdict = seq_of(&mark(&h, Actor::Harness, "- verdict word=done"));
    task_is(&h, "done", CREATOR);
    {
        use aterm_agent::harness::acts::{LandReceipt, ReceiptLeg, TaskFace};
        let mut tl = h.ctx.timeline.lock().unwrap();
        tl.set_task_face(Some(TaskFace::Landed), 0);
        tl.set_land_receipt(Some(std::sync::Arc::new(LandReceipt {
            head: "3f9a1c2e4b7d".to_string(),
            main_ref: "origin/main".to_string(),
            leg: ReceiptLeg::OnMain,
            ..LandReceipt::default()
        })));
    }
    hand.set(140_010);

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
    let (as_of, rev, turns) = {
        let tl = h.ctx.timeline.lock().unwrap();
        let st = tl.slog().log().state();
        (st.as_of, st.desc.card_rev(), st.totals.turns)
    };
    assert_eq!(
        reply,
        format!(
            "OK 22 described v=1 sid={sid} as_of={as_of} ep={ep} base={base} chapter=- \
             rev={rev} trust=screen\n\
             title - value=Fold%20laws by=human rank=person ev=declared behind=- seq={person}\n\
             title {agent} other value=%2D by=aterm rank=writer ev=inferred behind=0 \
             seq={aterm}\n\
             title {agent} other value=Review%20the%20fold by=ctl rank=ctl ev=declared \
             as=reviewer behind=0 seq={ctl}\n\
             title {agent} other value=mine by=s-00000000000000000abc rank=listed ev=declared \
             behind=- seq={other}\n\
             matters - value=-\n\
             description {agent} value=Reading%20fold_desc.rs by=harness rank=program \
             ev=declared behind=0 seq={program}\n\
             track - value=-\n\
             done {agent} value=tests%20pass by=ctl rank=ctl ev=declared re={ctl} behind=- \
             seq={done}\n\
             done - other verdict=done by=harness ev=declared seq={verdict}\n\
             package - value=-\n\
             stage {agent} value=review by=ctl rank=ctl ev=declared behind=- seq={stage}\n\
             history {agent} t={} value=Committed%209f3c2ab41d07 by=aterm rank=derived \
             ev=derived as=commit event=commit re={ctl} seq={commit}\n\
             history {agent} t={} value=Read%20fold_desc.rs by=harness rank=program \
             ev=declared as=landed seq={said}\n\
             history - t={} value=Plan%20read by=aterm rank=writer ev=inferred as=planner \
             seq={plan}\n\
             state word=working\n\
             observed tries=- try=- on_main=- running=- red=- reds=- conflicts=- landed=- \
             receipt=- ahead=- files=- turns=- work_min=- errors=- task=- need=- limit=- \
             says_done=- working=- ended=-\n\
             observed conflict paths=-\n\
             judged track=- track_why=- used=-\n\
             label word=- line=-\n\
             is conclusion=finished landed=3f9a1c2e4b7d away=-\n\
             born opened_ms={opened} age_ms={} by={CREATOR} kind=session via=- \
             title_src=stated matters_src=none\n\
             agent kind=claude run={agent} turns={turns} need=- tok_in=- tok_out=- \
             wall_s=140\n",
            WALL0 + 40,
            WALL0 + 30,
            WALL0 + 9,
            WALL0 + 140_010 - opened,
        )
    );
    assert_eq!(
        cmd_described(&h.ctx, "title"),
        super::desc::DESCRIBED_USAGE,
        "the card takes no slot"
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

/// `described` names a title a program printed `by=inband`, in lines and in `--json`: its record's
/// author is no actor (`Actor::Nobody`, whose wire word `-` is a rename a control client's keys
/// touched), so the word comes from the in-band rank.
///
/// CONTROL: a person's title on the same session stands `by=human rank=person`, and the in-band
/// claim it outranks is still listed `by=inband` as an `other` line.
#[test]
fn described_names_an_in_band_title_by_inband() {
    let (h, hand) = session(82);
    hand.set(10);
    let read = super::InbandRead {
        shown: "Printed topic",
        untitled: false,
        stands: super::observed::TitleStands::Own,
        program: None,
        program_seq: 0,
    };
    h.ctx
        .timeline
        .lock()
        .unwrap()
        .slog_mut()
        .observe_inband_title(&read, super::tests::at(10));
    let inband = h.ctx.timeline.lock().unwrap().slog().log().last_seq();
    let reply = cmd_described(&h.ctx, "");
    assert!(
        reply.contains(&format!(
            "\ntitle - value=Printed%20topic by=inband rank=inband ev=declared behind=- seq={inband}\n"
        )),
        "{reply}"
    );
    let json = cmd_described_json(&h.ctx, "");
    assert!(
        json.contains("\"by\":\"inband\",\"rank\":\"inband\""),
        "{json}"
    );

    hand.set(20);
    let person = h
        .ctx
        .timeline
        .lock()
        .unwrap()
        .slog_mut()
        .claim_desc(
            &DescWrite::of_session("title", Some("Fold laws")),
            Actor::Human,
            super::tests::at(20),
        )
        .expect("a person's title")
        .0;
    let reply = cmd_described(&h.ctx, "");
    assert!(
        reply.contains(&format!(
            "\ntitle - value=Fold%20laws by=human rank=person ev=declared behind=- seq={person}\n"
        )),
        "{reply}"
    );
    assert!(
        reply.contains(&format!(
            "\ntitle - other value=Printed%20topic by=inband rank=inband ev=declared behind=- \
             seq={inband}\n"
        )),
        "{reply}"
    );
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
            super::desc_stored("description", &format!("({token})."), Actor::Ctl, Src::Mark),
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
    let stored = |raw: &str, by: Actor| super::desc_stored("description", raw, by, Src::Mark);
    assert_eq!(
        stored("  spaced out  ", Actor::Ctl).as_deref(),
        Ok("spaced out")
    );
    assert_eq!(stored("->", Actor::Ctl).as_deref(), Ok("->"));
    assert_eq!(
        stored(&"x".repeat(257), Actor::Human),
        Err(DescRefused::Claim(ClaimRefused::TooLong { max: 256 }))
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
        "\ntitle - value=for%20now by=ctl rank=ctl ev=declared behind=- seq={seq}\n"
    )),);
    hand.set(60_000);
    let reply = cmd_described(&h.ctx, "");
    assert!(
        reply.contains(&format!(
            "\ntitle - value=-\n\
             title - other value=for%20now by=ctl rank=ctl ev=declared behind=- seq={seq} \
             lapsed=1\n"
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

/// A `package` claim is 1 to 4 tracker ids exactly as written: one with a space, a lower-case
/// prefix or a repeated id is refused by name through the `mark` verb, and nothing is written.
/// CONTROL: the same ids joined by commas alone are written as given.
#[test]
fn a_malformed_package_mark_is_refused_by_name() {
    let (h, _hand) = session(89);
    for bad in ["FL-04%20CX-01", "fl-04", "FL-04,FL-04"] {
        assert_eq!(
            mark(&h, Actor::Ctl, &format!("- desc field=package value={bad}")),
            "ERR mark: package is 1 to 4 tracker ids like FL-04, comma-separated\n",
            "{bad}"
        );
    }
    assert_eq!(desc_lines(&h), Vec::<String>::new(), "nothing written");
    let reply = mark(&h, Actor::Ctl, "- desc field=package value=FL-04,CX-01");
    assert!(reply.starts_with("OK seq="), "CONTROL: {reply}");
    let lines = desc_lines(&h);
    assert!(
        lines.len() == 1 && lines[0].contains(" field=package value=FL-04,CX-01 by=ctl "),
        "{lines:?}"
    );
}

/// AN ADOPTED CARD KEEPS ITS REVISION THROUGH THE SPAWN PATH'S TELLS: a worker whose creator
/// stated its title and why its task matters crosses a seamless update in the spawn path's order
/// (who it is said first, `spawn::tell_adopted_ident`; its carried log adopted; its task seeded;
/// the session registered), and at each step its card's revision and the creator's rank are the
/// predecessor's. CONTROL: adopted with nobody said first, the carry reads the creator's claims
/// as no author who stands, and the task seed's tell then moves the card's revision by one.
#[test]
#[cfg(unix)]
fn an_adopted_card_keeps_its_revision_through_the_spawn_paths_tells() {
    use crate::session_log::{Epochs, LogAdopt};
    use crate::session_store::SessionStore;
    use aterm_session::{LaunchNonce, SessionId};
    const OWN: &str = "s-0123456789abcdef0123";
    const CREATOR: &str = "s-00000000000000000abc";
    let handle = |local: u64| {
        crate::session_store::test_handle_identified(
            local,
            SessionId::parse(OWN).expect("a minted sid"),
            LaunchNonce::generate(),
            std::sync::Arc::new(Epochs::new()),
            aterm_slog::Stamp::new(WALL0, 0),
        )
    };
    let card = |h: &SessionHandle| {
        let tl = h.ctx.timeline.lock().unwrap();
        let st = tl.slog().log().state();
        let title = aterm_slog::shown(st, "title", WALL0 + 1_000).map(|s| s.rank);
        (st.desc.card_rev(), title)
    };
    let row =
        format!("rev=1 state=working from={CREATOR} since={WALL0} result_bytes=0 attention=0");
    let pred = handle(1);
    let mut store = SessionStore::default();
    store.register(pred.clone());
    crate::task_record::seed_carried(&pred.ctx, Some(&row));
    let creator = crate::session_log::sid_actor(CREATOR).expect("a minted sid");
    {
        let mut tl = pred.ctx.timeline.lock().unwrap();
        let now = tl.slog().now();
        for (slot, text) in [
            ("title", "fix the parser"),
            ("matters", "the release waits on it"),
        ] {
            tl.slog_mut()
                .claim_desc(&DescWrite::of_session(slot, Some(text)), creator, now)
                .expect("the creator's claim");
        }
    }
    let before = card(&pred);
    assert_eq!(
        before.1,
        Some(aterm_slog::Rank::Creator),
        "PRECONDITION: the creator's title stands"
    );
    let (ids, seq, lines) = {
        let mut tl = pred.ctx.timeline.lock().unwrap();
        let slog = tl.slog_mut();
        let ids = slog.handoff_carry();
        let seq = slog.handoff_seq();
        let lines = slog
            .carry_source()
            .encode(aterm_slog::CarryBudget::SESSION)
            .expect("carried")
            .lines;
        (ids, seq, lines)
    };
    let adopt = |told: bool| {
        let next = handle(2);
        if told {
            crate::spawn::tell_adopted_ident(&next.ctx.timeline, OWN, Some(&row));
        }
        crate::spawn::mark_handoff_gap(
            &next.ctx.timeline,
            &next.ctx.cast,
            &next.ctx.temporal,
            None,
            0,
            LogAdopt {
                ids: Some(ids),
                seq,
                lines: Some(lines.clone()),
                ..LogAdopt::default()
            },
            false,
        );
        assert!(
            next.ctx.timeline.lock().unwrap().slog().log_carried(),
            "PRECONDITION: the carry was adopted"
        );
        let adopted = card(&next);
        crate::spawn::seed_adopted_rows(&next.ctx, 2, Some(&row), None, None, None);
        let seeded = card(&next);
        SessionStore::default().register(next.clone());
        (adopted, seeded, card(&next))
    };
    assert_eq!(
        adopt(true),
        (before, before, before),
        "adopted, seeded and registered, the card is the predecessor's"
    );
    let moved = (before.0 + 1, before.1);
    assert_eq!(
        adopt(false),
        ((before.0, None), moved, moved),
        "CONTROL: said nothing first, the carry reads the creator as nobody, and the seed moves it"
    );
}

/// `state word=` is one of the five activity words or `-`, read as a peer reads the session's
/// survey row (`docs/DESIGN-comprehension-2026-11-14.md` §5.6's third column, no track verdict
/// derived): an agent at work, or at a prompt its row says nobody need answer, is `working`; one
/// waiting on a person or at a wall is `quiet`; one at rest is `idle`; a stalled or failed task is
/// `stuck`; a closed log is `ended`; an agent word this build does not read, and a session that is
/// no agent's, is `-`. `--json`'s `derived.state.word` is the same word, `null` for `-`.
///
/// CONTROL: the agent's own word never prints (`prompt` reads `working` or `quiet` by its need
/// alone); a task `done` under an idle agent reads `idle`, never `ended` (only the log's close
/// is); a waiting agent reads `quiet` and never `stuck`, since nothing judges it blocked; and a
/// shell reads `-` whether or not a command runs in it, so the shell's run is not the word.
#[test]
fn described_state_is_one_of_five_activity_words() {
    use aterm_types::needs::NeedWord;
    let need = |h: &SessionHandle, word: NeedWord| {
        h.ctx
            .timeline
            .lock()
            .unwrap()
            .set_need(crate::needs::Verdict {
                need: Some(crate::needs::Need {
                    word,
                    key: 1,
                    what: String::new(),
                    harness: None,
                    report: None,
                }),
                ..crate::needs::Verdict::default()
            });
    };
    let claude = |local: u64, word: &'static str| {
        let (h, hand) = session(local);
        with_claude(&h.ctx.timeline);
        agent_says(&h, word);
        (h, hand)
    };
    let mut read: Vec<(&str, String)> = Vec::new();
    let mut check = |case: &'static str, h: &SessionHandle| {
        let word = state_word(h);
        let json = json_of(&cmd_described_json(&h.ctx, ""));
        let state = &json["derived"]["state"];
        assert!(state.get("word").is_some(), "{case}: {json:?}");
        assert_eq!(
            state["word"].as_str().unwrap_or("-"),
            word,
            "{case}: the lines and --json read one word"
        );
        assert!(
            word == "-" || aterm_db::words::Activity::of(&word).is_some(),
            "{case}: {word}"
        );
        read.push((case, word));
    };

    let (shell, _hand) = session(91);
    check("a shell at its prompt", &shell);
    let (running, hand) = session(92);
    hand.set(10);
    running
        .ctx
        .timeline
        .lock()
        .unwrap()
        .slog_mut()
        .log_mut_for_journal()
        .open(
            aterm_slog::Open::new(Kind::Cmd, Src::Shell, Conf::Strong),
            super::tests::at(10),
        )
        .expect("a command's run opens");
    check("a shell running a command", &running);
    check("an agent at work", &claude(93, "busy").0);
    let (prompt, _hand) = claude(94, "prompt");
    check("an agent at a prompt, no need", &prompt);
    need(&prompt, NeedWord::Question);
    check("the same agent, a question standing", &prompt);
    check(
        "an agent at its weekly wall",
        &claude(95, "wall:usage-weekly").0,
    );
    let (idle, _hand) = claude(96, "idle");
    check("an agent at rest", &idle);
    task_is(&idle, "done", "ctl");
    check("an agent at rest, its task done", &idle);
    let (stalled, _hand) = claude(97, "busy");
    task_is(&stalled, "stalled", "ctl");
    check("an agent at work, its task stalled", &stalled);
    let (failed, _hand) = claude(98, "busy");
    task_is(&failed, "failed", "ctl");
    check("an agent at work, its task failed", &failed);
    let (closed, _hand) = claude(99, "busy");
    {
        let mut tl = closed.ctx.timeline.lock().unwrap();
        let at = tl.slog().now();
        tl.slog_mut()
            .close_session(aterm_slog::SessionOut::CtlClose, Actor::Ctl, "-", None, at);
    }
    check("an agent whose log closed", &closed);
    check("an agent whose word is unknown", &claude(100, "unknown").0);

    let read: Vec<(&str, &str)> = read.iter().map(|(c, w)| (*c, w.as_str())).collect();
    assert_eq!(
        read,
        vec![
            ("a shell at its prompt", "-"),
            ("a shell running a command", "-"),
            ("an agent at work", "working"),
            ("an agent at a prompt, no need", "working"),
            ("the same agent, a question standing", "quiet"),
            ("an agent at its weekly wall", "quiet"),
            ("an agent at rest", "idle"),
            ("an agent at rest, its task done", "idle"),
            ("an agent at work, its task stalled", "stuck"),
            ("an agent at work, its task failed", "stuck"),
            ("an agent whose log closed", "ended"),
            ("an agent whose word is unknown", "-"),
        ]
    );
}

/// `described history` prints the session's and its open agent's history lines, newest first by
/// seq, at most `n=` (1 to 16, 16 by default), and `older=` counts the lines the two kept lists
/// pushed out; `described` prints the same newest 8.
///
/// CONTROL: `n=0`, `n=17`, `n=`, `n=x`, `n=2 n=3`, `history extra` and `histories` each answer
/// the usage line (refused, never cut); with 11 lines held the reply is 11 lines (nothing padded)
/// and `older=0` (nothing counted that is held).
#[test]
fn described_history_prints_at_most_sixteen_lines_newest_first() {
    let (h, hand) = session(101);
    let sid = h.ctx.self_id.as_str().to_string();
    let seqs = |reply: &str| -> Vec<u64> {
        reply
            .lines()
            .skip(1)
            .map(|line| {
                line.rsplit_once(" seq=")
                    .and_then(|(_, n)| n.parse().ok())
                    .unwrap_or_else(|| panic!("no seq: {line}"))
            })
            .collect()
    };
    let head = |reply: &str| reply.lines().next().unwrap_or_default().to_string();
    hand.set(5);
    let shell = seq_of(&mark(
        &h,
        Actor::Ctl,
        "- desc field=history value=Shell%20ready",
    ));
    with_claude(&h.ctx.timeline);
    let agent = open_agent(&h).expect("the agent run is open");
    let mut events = Vec::new();
    for k in 1..=10u64 {
        hand.set(10 + k);
        events.push(event_line(&h, agent, &format!("Committed {k}"), k));
    }
    let reply = cmd_described(&h.ctx, "history");
    let (as_of, base, ep, rev) = {
        let tl = h.ctx.timeline.lock().unwrap();
        let log = tl.slog().log();
        (
            log.last_seq(),
            log.base_ep(),
            log.ep(),
            log.state().desc.card_rev(),
        )
    };
    assert_eq!(
        head(&reply),
        format!(
            "OK 11 described history v=1 sid={sid} as_of={as_of} ep={ep} base={base} chapter=- \
             rev={rev} older=0 trust=screen"
        )
    );
    let mut want: Vec<u64> = events.iter().rev().copied().collect();
    want.push(shell);
    assert_eq!(
        seqs(&reply),
        want,
        "newest first, the session's line last: {reply}"
    );
    assert!(
        reply
            .lines()
            .last()
            .is_some_and(|l| l.starts_with("history - t=") && l.contains(" by=ctl rank=ctl ")),
        "{reply}"
    );

    for k in 11..=20u64 {
        hand.set(10 + k);
        events.push(event_line(&h, agent, &format!("Committed {k}"), k));
    }
    let reply = cmd_described(&h.ctx, "history");
    assert!(
        reply.starts_with("OK 16 described history v=1 ") && head(&reply).contains(" older=4 "),
        "{reply}"
    );
    let newest: Vec<u64> = events.iter().rev().copied().collect();
    assert_eq!(seqs(&reply), newest[..16], "the 16 newest agent lines");
    assert!(
        reply
            .lines()
            .skip(1)
            .all(|l| l.starts_with(&format!("history {agent} "))
                && l.contains(" by=aterm rank=derived ev=derived as=commit event=commit re=")),
        "{reply}"
    );
    let five = cmd_described(&h.ctx, "history n=5");
    assert!(five.starts_with("OK 5 described history v=1 "), "{five}");
    assert_eq!(seqs(&five), newest[..5]);
    let card = cmd_described(&h.ctx, "");
    let card_history: Vec<&str> = card.lines().filter(|l| l.starts_with("history ")).collect();
    let history_head: Vec<&str> = reply.lines().skip(1).take(8).collect();
    assert_eq!(
        card_history, history_head,
        "the card prints the same newest 8"
    );
    let json = json_of(&cmd_described_json(&h.ctx, "history n=2"));
    assert_eq!(json["v"].as_u64(), Some(1));
    assert_eq!(json["older"].as_u64(), Some(4), "{json:?}");
    assert!(json.get("chapter").is_some_and(aterm_json::Value::is_null));
    let entries = json["history"].as_array().expect("history entries");
    assert_eq!(
        entries
            .iter()
            .map(|e| e["seq"].as_u64())
            .collect::<Vec<_>>(),
        vec![Some(newest[0]), Some(newest[1])]
    );
    assert!(
        entries
            .iter()
            .all(|e| e["event"].as_str() == Some("commit") && e["ev"].as_str() == Some("derived")),
        "{json:?}"
    );

    for refused in [
        "history n=0",
        "history n=17",
        "history n=",
        "history n=x",
        "history n=2 n=3",
        "history extra",
        "histories",
    ] {
        assert_eq!(
            cmd_described(&h.ctx, refused),
            super::desc::DESCRIBED_USAGE,
            "{refused}"
        );
        assert_eq!(
            cmd_described_json(&h.ctx, refused),
            super::desc::DESCRIBED_USAGE,
            "{refused} --json"
        );
    }
}

/// `described --json` reads unread as `null`, never 0 and never absent: on a fresh shell every
/// derived key the lines print `-` is a member whose value is `null` (`derived.state {word}`,
/// each of `derived.observed`'s 20 names, `derived.judged {track, why, used}` and
/// `derived.label {word, line}` among them), `chapter` is `null`, and `history` is `[]`; no
/// member holds a `since_ms` and there is no `derived.track`. A `done` slot that holds only a
/// judge's `verdict word=done` is an object, not `null`: its standing members are `null`, and
/// the verdict is its one `others` entry (`verdict`, `by`, `ev`, `scope` and `seq` values;
/// `value`, `rank`, `behind`, `as` and `re` `null`).
///
/// CONTROL: what is read is a value: `rev` and `derived.agent.turns` are the number 0 (counted
/// zeros), `opened_ms` and `age_ms` are numbers and `matters_src` is the word `none`; with a
/// claude agent at work and a ctl `done` claim that cites a record, the agent's kind, run and
/// age, the activity word and the claim's `re` and `ev` are values, the verdict stays the slot's
/// one `other` and never its value, while `is.away`, `judged.track` and `label.word` are still
/// `null`. Dropping the verdicts from `others` empties it, and reading a slot of verdicts alone
/// as unclaimed makes `slots.done` `null`: either fails here.
#[test]
fn described_json_reads_unread_as_null() {
    const OBSERVED: [&str; 20] = [
        "tries",
        "try",
        "on_main",
        "running",
        "red",
        "reds",
        "conflicts",
        "landed",
        "receipt",
        "ahead",
        "files",
        "turns",
        "work_min",
        "errors",
        "task",
        "need",
        "limit",
        "says_done",
        "working",
        "ended",
    ];
    let members = |v: &aterm_json::Value| -> Vec<String> {
        let mut keys: Vec<String> = v
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        keys.sort();
        keys
    };
    let sorted = |names: &[&str]| -> Vec<String> {
        let mut keys: Vec<String> = names.iter().map(|n| (*n).to_string()).collect();
        keys.sort();
        keys
    };
    let (h, hand) = session(111);
    hand.set(3);
    let reply = cmd_described_json(&h.ctx, "");
    assert!(!reply.contains("since_ms"), "no time is kept: {reply}");
    let v = json_of(&reply);
    let at = |path: &[&str]| -> Option<aterm_json::Value> {
        let mut node = &v;
        for key in path {
            node = node.get(*key)?;
        }
        Some(node.clone())
    };
    let observed: Vec<[&str; 3]> = OBSERVED
        .iter()
        .map(|k| ["derived", "observed", k])
        .collect();
    let unread = [
        &["chapter"][..],
        &["derived", "state", "word"],
        &["derived", "judged", "track"],
        &["derived", "judged", "why"],
        &["derived", "judged", "used"],
        &["derived", "label", "word"],
        &["derived", "label", "line"],
        &["derived", "is", "conclusion"],
        &["derived", "is", "landed"],
        &["derived", "is", "away"],
        &["derived", "title_src"],
        &["derived", "creator", "id"],
        &["derived", "creator", "kind"],
        &["derived", "creator", "via"],
        &["derived", "agent", "kind"],
        &["derived", "agent", "run"],
        &["derived", "agent", "need"],
        &["derived", "agent", "tok_in"],
        &["derived", "agent", "tok_out"],
        &["derived", "agent", "wall_s"],
    ];
    for path in unread.into_iter().chain(observed.iter().map(|p| &p[..])) {
        assert!(
            at(path).is_some_and(|value| value.is_null()),
            "{path:?} is there, and null: {v:?}"
        );
    }
    assert_eq!(members(&v["derived"]["state"]), sorted(&["word"]), "{v:?}");
    assert_eq!(
        members(&v["derived"]["observed"]),
        sorted(&OBSERVED),
        "{v:?}"
    );
    assert_eq!(
        members(&v["derived"]["judged"]),
        sorted(&["track", "why", "used"]),
        "{v:?}"
    );
    assert_eq!(
        members(&v["derived"]["label"]),
        sorted(&["word", "line"]),
        "{v:?}"
    );
    assert!(
        v["derived"].get("track").is_none(),
        "no derived.track: {v:?}"
    );
    assert_eq!(v["history"].as_array().map(Vec::len), Some(0), "{v:?}");
    assert_eq!(v["rev"].as_u64(), Some(0), "a counted zero: {v:?}");
    assert_eq!(v["derived"]["agent"]["turns"].as_u64(), Some(0), "{v:?}");
    assert!(v["derived"]["opened_ms"].as_u64().is_some(), "{v:?}");
    assert_eq!(v["derived"]["age_ms"].as_u64(), Some(3), "{v:?}");
    assert_eq!(v["derived"]["matters_src"].as_str(), Some("none"), "{v:?}");
    assert!(v["base"].as_u64().is_some(), "{v:?}");

    with_claude(&h.ctx.timeline);
    let agent = open_agent(&h).expect("the agent run is open");
    agent_says(&h, "busy");
    let verdict = seq_of(&mark(&h, Actor::Harness, "- verdict word=done"));
    let lines = cmd_described(&h.ctx, "");
    assert!(
        lines.contains(&format!(
            "\ndone - value=-\ndone - other verdict=done by=harness ev=declared seq={verdict}\n"
        )),
        "a done of verdicts alone: {lines}"
    );
    let v = json_of(&cmd_described_json(&h.ctx, ""));
    let slot = &v["slots"]["done"];
    assert!(
        slot.is_object(),
        "a slot holding a verdict is not null: {v:?}"
    );
    for key in [
        "value", "scope", "by", "rank", "ev", "as", "re", "behind", "seq",
    ] {
        assert!(
            slot.get(key).is_some_and(aterm_json::Value::is_null),
            "slots.done.{key} is there, and null: {v:?}"
        );
    }
    assert_eq!(slot["dropped"].as_u64(), Some(0), "{v:?}");
    let others = slot["others"].as_array().expect("slots.done.others");
    assert_eq!(others.len(), 1, "{v:?}");
    let entry = &others[0];
    assert_eq!(entry["verdict"].as_str(), Some("done"), "{v:?}");
    assert_eq!(entry["by"].as_str(), Some("harness"), "{v:?}");
    assert_eq!(entry["ev"].as_str(), Some("declared"), "{v:?}");
    assert_eq!(entry["scope"].as_str(), Some("-"), "{v:?}");
    assert_eq!(entry["seq"].as_u64(), Some(verdict), "{v:?}");
    for key in ["value", "rank", "behind", "as", "re"] {
        assert!(
            entry.get(key).is_some_and(aterm_json::Value::is_null),
            "the verdict's {key} is there, and null: {v:?}"
        );
    }

    hand.set(2_003);
    let cited = h.ctx.timeline.lock().unwrap().slog().log().last_seq();
    let done = seq_of(&mark(
        &h,
        Actor::Ctl,
        &format!("- desc field=done value=tests%20pass re={cited}"),
    ));
    let reply = cmd_described_json(&h.ctx, "");
    assert!(!reply.contains("since_ms"), "no time is kept: {reply}");
    let v = json_of(&reply);
    let derived = &v["derived"];
    assert_eq!(derived["agent"]["kind"].as_str(), Some("claude"), "{v:?}");
    assert_eq!(
        derived["agent"]["run"].as_str(),
        Some(agent.to_string().as_str())
    );
    assert_eq!(derived["agent"]["wall_s"].as_u64(), Some(2), "{v:?}");
    assert_eq!(derived["state"]["word"].as_str(), Some("working"), "{v:?}");
    let slot = &v["slots"]["done"];
    assert_eq!(slot["seq"].as_u64(), Some(done));
    assert_eq!(
        slot["re"].as_str(),
        Some(cited.to_string().as_str()),
        "{v:?}"
    );
    assert_eq!(slot["ev"].as_str(), Some("declared"));
    assert_eq!(slot["value"].as_str(), Some("tests pass"), "{v:?}");
    let others = slot["others"].as_array().expect("slots.done.others");
    assert_eq!(others.len(), 1, "the verdict stays an other: {v:?}");
    assert_eq!(others[0]["verdict"].as_str(), Some("done"), "{v:?}");
    assert_eq!(others[0]["seq"].as_u64(), Some(verdict), "{v:?}");
    assert!(
        others[0]
            .get("value")
            .is_some_and(aterm_json::Value::is_null),
        "{v:?}"
    );
    assert!(
        derived["is"]
            .get("away")
            .is_some_and(aterm_json::Value::is_null)
            && derived["judged"]
                .get("track")
                .is_some_and(aterm_json::Value::is_null)
            && derived["label"]
                .get("word")
                .is_some_and(aterm_json::Value::is_null),
        "{v:?}"
    );
}
