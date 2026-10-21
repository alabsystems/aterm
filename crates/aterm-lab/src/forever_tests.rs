// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lab forever`'s pure parts: the stub's per-turn script (each chain's reply
//! to each act of the harness, by the real act texts), the request readers it
//! reads conversations by, and THE JUDGE over records each scenario passes,
//! fails and goes unobserved on — each pass beside the negative controls that
//! must fail it.

use aterm_agent::harness::repo_work::RepoWork;
use aterm_agent::supervise::config::CONTINUE_TEXT;
use aterm_agent::supervise::policy::turn_end::{
    DONE_CHECK, LAND, LAND_NUDGE, open_work_suffix, repo_work_suffix,
};

use super::*;
use crate::stub::{gemini::gemini_users, message_users, responses_users, scripted_reply};

fn users(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_string()).collect()
}

/// The reply the lab's spec picks for a conversation of `chain` whose
/// user texts after its prompt are `acts`.
fn reply(chain: Chain, acts: &[&str]) -> Option<String> {
    let mut u = vec![chain.prompt()];
    u.extend(acts.iter().map(|s| (*s).to_string()));
    scripted_reply(&spec(&Chain::ALL), &u)
}

#[test]
fn each_chain_is_scripted_turn_by_turn_by_the_harness_acts() {
    assert_eq!(reply(Chain::LandA, &[]).as_deref(), Some(WORK));
    assert_eq!(reply(Chain::LandA, &[LAND]), Some(needs_defense()));
    assert_eq!(
        reply(Chain::LandA, &[LAND, LAND_NUDGE]).as_deref(),
        Some(CLEAN)
    );
    assert_eq!(reply(Chain::LandB, &[]).as_deref(), Some(WORK));
    assert_eq!(reply(Chain::LandB, &[LAND]), Some(open_defense()));
    assert_eq!(
        reply(Chain::LandB, &[LAND, LAND_NUDGE]),
        Some(open_defense())
    );
    for c in [
        Chain::LandC,
        Chain::LandE,
        Chain::LandF,
        Chain::LandG,
        Chain::LandH,
        Chain::LandI,
    ] {
        assert_eq!(reply(c, &[]).as_deref(), Some(WORK), "{c:?}");
        assert_eq!(reply(c, &[LAND]).as_deref(), Some(CLEAN), "{c:?}");
        assert_eq!(
            reply(c, &[LAND, LAND_NUDGE]).as_deref(),
            Some(CLEAN),
            "{c:?}"
        );
    }
    assert_eq!(reply(Chain::LandD, &[LAND]), Some(tall_reply(80)));
    assert_eq!(reply(Chain::Cruise, &[]).as_deref(), Some(DONE_CLAIM));
    assert_eq!(reply(Chain::Cruise, &[DONE_CHECK]).as_deref(), Some(CLEAN));
    assert_eq!(
        reply(Chain::Cruise, &[DONE_CHECK, CONTINUE_TEXT]).as_deref(),
        Some(DONE_CLAIM)
    );
    assert_eq!(reply(Chain::Off, &[]).as_deref(), Some(DONE_CLAIM));
    // a conversation of no chain is left to the spec's own text
    assert_eq!(scripted_reply(&spec(&Chain::ALL), &users(&["hi"])), None);
}

#[test]
fn the_act_keys_stand_in_their_acts_alone() {
    // each act is told by its own words, never another act's
    assert!(LAND.contains(LAND_KEY) && !LAND.contains(NUDGE_KEY));
    assert!(LAND_NUDGE.contains(NUDGE_KEY) && !LAND_NUDGE.contains(LAND_KEY));
    assert!(DONE_CHECK.contains(CHECK_KEY) && !DONE_CHECK.contains(CONTINUE_KEY));
    assert!(CONTINUE_TEXT.contains(CONTINUE_KEY) && !CONTINUE_TEXT.contains(CHECK_KEY));
    // every act bears the mark the stub's witness counts
    for act in [LAND, LAND_NUDGE, DONE_CHECK, CONTINUE_TEXT] {
        assert!(act.contains(MARK), "{act}");
    }
    // and no chain's word holds another's
    for a in Chain::ALL {
        for b in Chain::ALL {
            assert!(a == b || !a.token().contains(&b.token()), "{a:?} {b:?}");
        }
    }
}

#[test]
fn a_scripted_reply_takes_the_first_rule_its_conversation_meets() {
    let spec = J::parse(
        r#"{"replies": [
            {"when": "A", "last": "go", "text": "a-go"},
            {"when": "A", "text": "a"},
            {"last": "zz", "text": "any-zz"}
        ]}"#,
    )
    .unwrap();
    assert_eq!(
        scripted_reply(&spec, &users(&["A x", "go"])).as_deref(),
        Some("a-go")
    );
    // `last` reads the newest user text only
    assert_eq!(
        scripted_reply(&spec, &users(&["A go", "x"])).as_deref(),
        Some("a")
    );
    assert_eq!(
        scripted_reply(&spec, &users(&["q", "zz"])).as_deref(),
        Some("any-zz")
    );
    assert_eq!(scripted_reply(&spec, &users(&["q"])), None);
    assert_eq!(scripted_reply(&spec, &[]), None);
    assert_eq!(
        scripted_reply(&J::parse(r#"{"text":"t"}"#).unwrap(), &users(&["A"])),
        None
    );
}

#[test]
fn the_request_readers_find_each_apis_user_texts() {
    let messages = br#"{"messages": [
        {"role": "user", "content": [{"type": "text", "text": "FIRST"}]},
        {"role": "assistant", "content": "ok"},
        {"role": "user", "content": [{"type": "tool_result", "content": "r"}]},
        {"role": "user", "content": "NEWEST"}
    ]}"#;
    assert_eq!(message_users(messages), users(&["FIRST", "NEWEST"]));
    let responses = br#"{"input": [
        {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "<system-reminder>x</system-reminder>"}]},
        {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "FIRST"}]},
        {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "ok"}]},
        {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "NEWEST"}]}
    ]}"#;
    assert_eq!(responses_users(responses), users(&["FIRST", "NEWEST"]));
    let gemini = br#"{"contents": [
        {"role": "user", "parts": [{"text": "FIRST"}]},
        {"role": "model", "parts": [{"text": "ok"}]},
        {"role": "user", "parts": [{"functionResponse": {"name": "f"}}]},
        {"role": "user", "parts": [{"text": "NEWEST"}]}
    ]}"#;
    assert_eq!(gemini_users(gemini), users(&["FIRST", "NEWEST"]));
    assert!(message_users(b"not json").is_empty());
}

/// Whether a request carries a new harness turn past `baseline`, as
/// [`StubSeen::of`] counts one.
fn new_harness_turn(body: &[u8], tok: &str, baseline: usize) -> bool {
    StubSeen::of(
        &marked_turns(body, tok)
            .map(|n| (1, n))
            .into_iter()
            .collect::<Vec<_>>(),
        0,
        baseline,
    )
    .new_turns
        == 1
}

/// A Messages API body of conversation `tok` whose user texts are `texts`.
fn messages(tok: &str, texts: &[&str]) -> Vec<u8> {
    let mut m = vec![J::obj([("role", J::s("user")), ("content", J::s(tok))])];
    for t in texts {
        m.push(J::obj([
            ("role", J::s("assistant")),
            ("content", J::s("ok")),
        ]));
        m.push(J::obj([("role", J::s("user")), ("content", J::s(*t))]));
    }
    J::obj([("messages", J::Arr(m))])
        .dump(crate::pyjson::COMPACT)
        .into_bytes()
}

/// THE STUB'S WITNESS COUNTS THE HARNESS'S TURNS: a request after the
/// landing holding more harness-marked user texts than the conversation held
/// at it is a new turn of the harness's — a new act, and THE SAME ACT TYPED
/// AGAIN (the likeliest regression, which a reader that skipped texts typed
/// before missed). NEGATIVE CONTROLS: the conversation as it stood (a side
/// call carrying it), an agent's own question (no mark), another
/// conversation.
#[test]
fn only_a_new_harness_turn_counts_after_the_landing() {
    let tok = Chain::LandA.token();
    let at_landing = [LAND, LAND_NUDGE];
    assert_eq!(marked_turns(&messages(&tok, &at_landing), &tok), Some(2));
    let with = |more: &str| messages(&tok, &[LAND, LAND_NUDGE, more]);
    assert!(new_harness_turn(&with(CONTINUE_TEXT), &tok, 2));
    // the nudge typed again after the landing
    assert!(new_harness_turn(&with(LAND_NUDGE), &tok, 2));
    // the conversation as it stood, sent again (a side call carrying it)
    assert!(!new_harness_turn(&messages(&tok, &at_landing), &tok, 2));
    // an agent's own side call: its question, no mark
    assert!(!new_harness_turn(
        &with("You are picking one optional feature tip for a user"),
        &tok,
        2
    ));
    // another conversation
    assert!(!new_harness_turn(
        &with(CONTINUE_TEXT),
        &Chain::LandB.token(),
        2
    ));
    assert_eq!(
        marked_turns(&with(CONTINUE_TEXT), &Chain::LandB.token()),
        None
    );
    // the Responses API's and the Gemini API's turns are counted too
    let responses = format!(
        r#"{{"input": [{{"type": "message", "role": "user", "content": [{{"type": "input_text", "text": "{tok}"}}]}},
        {{"type": "message", "role": "user", "content": [{{"type": "input_text", "text": "[aterm harness] one"}}]}},
        {{"type": "message", "role": "user", "content": [{{"type": "input_text", "text": "[aterm harness] two"}}]}}]}}"#
    );
    assert_eq!(marked_turns(responses.as_bytes(), &tok), Some(2));
    assert!(new_harness_turn(responses.as_bytes(), &tok, 1));
    let gemini = format!(
        r#"{{"contents": [{{"role": "user", "parts": [{{"text": "{tok}"}}]}}, {{"role": "user", "parts": [{{"text": "[aterm harness] new"}}]}}]}}"#
    );
    assert!(new_harness_turn(gemini.as_bytes(), &tok, 0));
    assert!(!new_harness_turn(gemini.as_bytes(), &tok, 1));
}

/// The witness's baseline is the most harness turns any request before the
/// landing held, or the acts typed, whichever is more; Off's is none.
#[test]
fn the_stub_witness_counts_from_the_landing() {
    let dumps = [(100, 0), (200, 1), (300, 2), (400, 2), (500, 3)];
    assert_eq!(
        StubSeen::of(&dumps, 350, 2),
        StubSeen {
            baseline: 2,
            read: 2,
            new_turns: 1
        }
    );
    // a request the dumps missed before the landing: the acts typed hold the baseline
    assert_eq!(StubSeen::of(&[(100, 0), (400, 2)], 350, 2).new_turns, 0);
    // Off: any marked text is a harness turn
    assert_eq!(StubSeen::of(&[(1, 0), (2, 0)], 0, 0).new_turns, 0);
    assert_eq!(StubSeen::of(&[(1, 0), (2, 1)], 0, 0).new_turns, 1);
}

#[test]
fn no_scripted_reply_opens_like_a_muse_tool_row() {
    let heads = ["Ran ", "Wrote ", "Edited "];
    for c in Chain::ALL {
        for r in c.rules() {
            let text = r.get("text").and_then(J::as_str).unwrap_or("");
            assert!(!heads.iter().any(|h| text.starts_with(h)), "{c:?}: {text}");
        }
    }
}

#[test]
fn the_spec_paces_every_reply_and_holds_every_chains_rules() {
    let s = spec(&Chain::of_mode(Mode::Land));
    assert_eq!(s.get("delay_ms").and_then(J::as_i64), Some(REPLY_DELAY_MS));
    let n: usize = Chain::of_mode(Mode::Land)
        .iter()
        .map(|c| c.rules().len())
        .sum();
    assert_eq!(
        s.get("replies").and_then(J::as_arr).map(<[J]>::len),
        Some(n)
    );
}

#[test]
fn the_modes_pick_their_chains_and_off_runs_in_each() {
    use Chain::*;
    assert_eq!(
        Chain::of_mode(Mode::Land),
        [
            LandA, LandB, LandC, LandD, LandE, LandF, LandG, LandH, LandI, Off
        ]
    );
    assert_eq!(Chain::of_mode(Mode::Cruise), [Cruise, Off]);
    assert_eq!(Chain::of_mode(Mode::Both), Chain::ALL);
    assert_eq!(
        [LandC, LandF, LandG, LandH, LandI].map(Chain::repo),
        [
            Repo::Untracked,
            Repo::Changed,
            Repo::Stashed,
            Repo::Unpushed,
            Repo::Unlanded
        ]
    );
    assert!(LandA.repo() == Repo::Clean && LandE.repo() == Repo::Clean);
    assert!(LandE.tasked() && !LandA.tasked());
    assert!(LandA.lands() && LandE.lands() && !LandC.lands());
}

#[test]
fn the_tall_reply_outgrows_the_window_with_the_need_on_top() {
    let t = tall_reply(80);
    let lines: Vec<&str> = t.lines().collect();
    assert!(lines.len() > 60, "taller than the lab's 50 rows");
    assert_eq!(lines[1], format!("NEEDS YOU: {NEED}"));
    assert_eq!(lines.last().copied(), Some(CLEAN));
}

#[test]
fn words_and_escapes_are_read() {
    assert_eq!(
        word("OK task=landed needs=-", "task").as_deref(),
        Some("landed")
    );
    assert_eq!(word("OK task_x=1", "task"), None);
    assert_eq!(
        pct_decode("claude%20needs%20you%3A%20x"),
        "claude needs you: x"
    );
    assert_eq!(pct_decode("a%2Cb%"), "a,b%");
    assert_eq!(pct_decode("%zz%e2%80%94"), "%zz\u{2014}");
    assert_eq!(
        act_rule("CONTINUED seq=4 rule=land@v1 text"),
        Some("land@v1")
    );
    assert_eq!(act_rule("TYPED seq=4 rule=x@v1"), Some("x@v1"));
    assert_eq!(act_rule("WAITING seq=4 rule=x@v1"), None);
    assert_eq!(
        idle_words("EVENT idle seq=9 ⏺ Nothing left: x"),
        Some("⏺ Nothing left: x")
    );
    assert_eq!(idle_words("EVENT idle seq=9 -"), Some("-"));
    assert_eq!(idle_words("WAITING seq=9 x"), None);
}

#[test]
fn the_words_are_read() {
    let v = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let a = parse_args(&v(&["--harness", "all", "--mode", "land", "t1", "--keep"]))
        .unwrap()
        .unwrap();
    assert_eq!(a.harnesses, Harness::ALL);
    assert_eq!(
        (a.mode, a.tag.as_deref(), a.keep),
        (Mode::Land, Some("t1"), true)
    );
    let b = parse_args(&v(&["--harness", "muse"])).unwrap().unwrap();
    assert_eq!((b.harnesses, b.mode), (vec![Harness::Muse], Mode::Both));
    assert!(parse_args(&v(&["-h"])).unwrap().is_none());
    for bad in [
        &["t"][..],
        &["--harness"],
        &["--harness", "codex"],
        &["--harness", "agy", "--mode", "x"],
        &["--harness", "agy", "--wat"],
    ] {
        assert_eq!(
            parse_args(&v(bad)).err().map(|f| f.code),
            Some(2),
            "{bad:?}"
        );
    }
}

// ---- the judge

const T0: i64 = 1_000_000;

fn j(t: i64, line: &str) -> (i64, String) {
    (T0 + t, line.to_string())
}

fn row(t: i64, rule: &str, decision: &str, command: &str, reason: &str) -> Row {
    Row {
        ts: T0 + t,
        rule: rule.into(),
        decision: decision.into(),
        command: command.into(),
        reason: reason.into(),
    }
}

fn typed(t: i64, rule: &str, command: &str) -> Row {
    row(
        t,
        rule,
        "typed",
        command,
        "the turn-end policy (short turns in a row: 1)",
    )
}

/// The `point@v1` record of a point that closed `streak` short turns.
fn point(t: i64, streak: u32) -> Row {
    row(
        t,
        RULE_POINT,
        "waiting",
        "",
        &format!("the turn-end policy judged the point (short turns in a row: {streak})"),
    )
}

fn task_done(t: i64) -> Row {
    row(t, RULE_TASK_DONE, "waiting", "", TASK_DONE)
}

fn status(t: i64, task: &str, autopilot: &str) -> (i64, String) {
    status_of(t, task, "-", autopilot, "aterm-harness@1")
}

fn status_of(t: i64, task: &str, needs: &str, autopilot: &str, sup: &str) -> (i64, String) {
    j(
        t,
        &format!("task={task} needs={needs} autopilot={autopilot} supervisor={sup} agent=-"),
    )
}

fn needs_meta(t: i64, program: &str) -> (i64, String) {
    j(
        t,
        &format!(
            "attention_owners=1 attention_kinds=ask:needs attention={program} needs you: {NEED} — carrying on"
        ),
    )
}

fn no_meta(t: i64) -> (i64, String) {
    j(t, "attention_owners=0 attention_kinds=- attention=-")
}

fn verdicts(chain: Chain, seen: &Seen, program: &str) -> Vec<(&'static str, &'static str)> {
    judge(chain, seen, program, T0)
        .iter()
        .map(|(s, v)| (s.id(), v.word()))
        .collect()
}

/// One scenario's verdict word and its words.
fn one(chain: Chain, seen: &Seen, program: &str, s: Scenario) -> (&'static str, String) {
    judge(chain, seen, program, T0)
        .into_iter()
        .find(|(x, _)| *x == s)
        .map(|(_, v)| (v.word(), v.why().to_string()))
        .expect("the scenario")
}

/// The land nudge as the harness types it after a new need: the carry-on.
fn needs_nudge() -> String {
    format!("{LAND_NUDGE} {NEEDS_ACK}")
}

/// L1, L3 and L5 as a live run that passes them records them: the landing
/// at +375 s closed three short turns, so it is watched past the ladder's
/// next step (8 min) and a minute more.
fn landed_a() -> Seen {
    Seen {
        journal: vec![
            j(5_000, "EVENT idle seq=1 Implemented the parser"),
            j(
                5_100,
                "WAITING seq=1 until=x 1 short turn(s) in a row: 2 min back-off",
            ),
            j(
                125_000,
                "CONTINUED seq=1 rule=land@v1 [aterm harness] Autopilot is Land: land",
            ),
            j(
                130_000,
                "NEEDS seq=2 count=1 attention=OK rotate the deploy key",
            ),
            j(130_000, "EVENT idle seq=2 Nothing left: the parser"),
            j(
                130_100,
                "WAITING seq=2 until=x 2 short turn(s) in a row: 4 min back-off",
            ),
            j(
                370_000,
                "CONTINUED seq=2 rule=land-nudge@v1 [aterm harness] Still landing:",
            ),
            j(375_000, "CLEARED seq=3 needs attention=OK none stands now"),
            j(375_000, "EVENT idle seq=3 Nothing left: the parser"),
            j(
                375_000,
                "DONE seq=3 rule=land@v1 the worker defended that nothing is left",
            ),
        ],
        ledger: vec![
            point(5_000, 1),
            typed(125_000, "land@v1", LAND),
            point(130_000, 2),
            typed(370_000, "land-nudge@v1", &needs_nudge()),
            point(375_000, 3),
            task_done(375_000),
        ],
        status: vec![
            status(1_000, "open", "land"),
            status_of(131_000, "open", "ask", "land", "aterm-harness@1"),
            status(376_000, "landed", "land"),
        ],
        meta: vec![
            no_meta(1_000),
            needs_meta(131_000, "gemini"),
            no_meta(376_000),
        ],
        task: vec![j(1_000, "OK task -")],
        reply_ms: None,
        end_ms: T0 + 920_000,
        stub: Some(StubSeen {
            baseline: 2,
            read: 3,
            new_turns: 0,
        }),
    }
}

#[test]
fn land_a_passes_on_a_landing_with_its_need_raised_then_cleared() {
    assert_eq!(
        verdicts(Chain::LandA, &landed_a(), "gemini"),
        [("L1", "PASS"), ("L3", "PASS"), ("L5", "PASS")]
    );
}

/// THE WATCH AFTER A LANDING RUNS PAST THE LADDER'S NEXT STEP: the step
/// the streak at the landing would take next (2 min doubling: 8 min after
/// three short turns, 16 after four), a minute more, at least 90 s; none for
/// a worker its manager waits on through its first three.
#[test]
fn a_landing_is_watched_past_the_ladders_next_step() {
    let s = landed_a();
    let d = T0 + 375_000;
    assert_eq!(quiet_after_landed_ms(&s, d, false), 9 * 60_000);
    assert_eq!(next_step_ms(1, false), 2 * 60_000);
    assert_eq!(next_step_ms(4, false), 16 * 60_000);
    assert_eq!(next_step_ms(2, true), 0);
    assert_eq!(next_step_ms(4, true), 16 * 60_000);
    assert_eq!(quiet_after_landed_ms(&Seen::default(), d, true), 90_000);
    // with no point row, the last WAITING before it says the streak
    let mut w = landed_a();
    w.ledger.retain(|r| r.rule != RULE_POINT);
    assert_eq!(quiet_after_landed_ms(&w, d, false), 9 * 60_000);
}

#[test]
fn land_a_fails_what_it_must_and_says_what_it_did_not_see() {
    let l3 = |s: &Seen, p: &str| verdicts(Chain::LandA, s, p)[1];
    let l5 = |s: &Seen| verdicts(Chain::LandA, s, "gemini")[2];
    // the needs entry names another program
    assert_eq!(l3(&landed_a(), "agy"), ("L3", "FAIL"));
    // the entry does not say it carries on
    let mut s = landed_a();
    s.meta[1].1 = s.meta[1].1.replace("— carrying on", "— waiting for you");
    assert_eq!(l3(&s, "gemini"), ("L3", "FAIL"));
    // status never said needs=ask while it stood
    let mut s = landed_a();
    s.status.remove(1);
    assert_eq!(l3(&s, "gemini"), ("L3", "FAIL"));
    // the nudge did not tell the worker its need reached the person
    let mut s = landed_a();
    s.ledger[3].command = LAND_NUDGE.to_string();
    assert_eq!(l3(&s, "gemini"), ("L3", "FAIL"));
    // something typed after the landing
    let mut s = landed_a();
    s.journal.push(j(
        430_000,
        "CONTINUED seq=4 rule=continue@v1 [aterm harness] x",
    ));
    assert_eq!(l5(&s), ("L5", "FAIL"));
    let mut s = landed_a();
    s.ledger.push(typed(430_000, "continue@v1", CONTINUE_TEXT));
    assert_eq!(l5(&s), ("L5", "FAIL"));
    // a key pressed after the landing (a box approved) is an act too
    let mut s = landed_a();
    s.ledger
        .push(row(430_000, "approve@v1", "approved", "1", "a box"));
    assert_eq!(l5(&s), ("L5", "FAIL"));
    // THE LADDER WENT ON: a WAITING after the DONE, even with nothing typed in the watch
    let mut s = landed_a();
    s.journal.push(j(
        375_000,
        "WAITING seq=3 until=x 3 short turn(s) in a row: 8 min back-off",
    ));
    assert_eq!(l5(&s), ("L5", "FAIL"));
    // the ledger's last row is not the task's end (another point judged after it)
    let mut s = landed_a();
    s.ledger.push(point(500_000, 4));
    assert_eq!(l5(&s), ("L5", "FAIL"));
    let mut s = landed_a();
    s.ledger.pop();
    assert_eq!(l5(&s), ("L5", "FAIL"));
    // the stub answered a new harness turn of the conversation after the landing
    let mut s = landed_a();
    s.stub = Some(StubSeen {
        baseline: 2,
        read: 4,
        new_turns: 1,
    });
    assert_eq!(l5(&s), ("L5", "FAIL"));
    // the stub's dumps were never read
    let mut s = landed_a();
    s.stub = None;
    assert_eq!(l5(&s), ("L5", "UNOBSERVED"));
    // the need never cleared
    let mut s = landed_a();
    s.meta.pop();
    s.journal.remove(7);
    assert_eq!(l5(&s), ("L5", "FAIL"));
    // a task record where none was given
    let mut s = landed_a();
    s.task
        .push(j(377_000, "OK task rev=4 state=done why=marker from=ctl"));
    assert_eq!(l5(&s), ("L5", "FAIL"));
    // the land face never said landed
    let mut s = landed_a();
    s.status.pop();
    assert_eq!(l5(&s), ("L5", "FAIL"));
    // the nudge that landed it named the repository
    let mut s = landed_a();
    s.ledger[3].command = format!("{} {REPO_SUFFIX}: x", needs_nudge());
    assert_eq!(l5(&s), ("L5", "FAIL"));
    // watched 95 s after the landing: under the ladder's next step, not a pass
    let mut s = landed_a();
    s.end_ms = T0 + 470_000;
    let (w, why) = one(Chain::LandA, &s, "gemini", Scenario::L5);
    assert_eq!(w, "UNOBSERVED", "{why}");
    // no land act at all: L1 fails, the rest were never reached
    let s = Seen {
        journal: vec![j(5_000, "EVENT idle seq=1 x")],
        end_ms: T0 + 900_000,
        ..Seen::default()
    };
    assert_eq!(
        verdicts(Chain::LandA, &s, "gemini"),
        [("L1", "FAIL"), ("L3", "UNOBSERVED"), ("L5", "UNOBSERVED")]
    );
    // the land act journaled with no ledger row
    let mut s = landed_a();
    s.ledger.retain(|r| r.rule != "land@v1");
    assert_eq!(verdicts(Chain::LandA, &s, "gemini")[0], ("L1", "FAIL"));
    // landed at the need (no nudge first): L3 fails
    let mut s = landed_a();
    s.journal.retain(|(_, l)| !l.contains("land-nudge"));
    s.journal.push(j(131_000, "DONE seq=2 rule=land@v1 x"));
    s.journal.sort();
    assert_eq!(l3(&s, "gemini"), ("L3", "FAIL"));
}

/// The repository suffix the product types for `repo` (`repo_work_suffix`
/// over the probe's own parse of what `git` says of it).
fn repo_suffix(repo: Repo) -> String {
    let r = "/r/proj/land-x";
    let work = match repo {
        Repo::Clean => None,
        Repo::Untracked => RepoWork::of_git(r, "?? wip-notes.txt\0", "", 0, 0, "origin/main"),
        Repo::Changed => RepoWork::of_git(r, " M README.md\0", "", 0, 0, "origin/main"),
        Repo::Stashed => RepoWork::of_git(r, "", "WIP on main: 1a2b lab\n", 0, 0, "origin/main"),
        Repo::Unpushed => RepoWork::of_git(r, "", "", 1, 1, "origin/main"),
        Repo::Unlanded => RepoWork::of_git(r, "", "", 0, 1, "origin/main"),
    };
    repo_work_suffix(&work.expect("work found"))
}

fn open_suffix() -> String {
    open_work_suffix(&["still open"]).expect("open work")
}

/// L2, L4 and L8–L11 as a live run that passes them records them: the
/// nudge, and its yield read and waited on.
fn nudged(suffix: &str) -> Seen {
    Seen {
        journal: vec![
            j(
                125_000,
                "CONTINUED seq=1 rule=land@v1 [aterm harness] Autopilot is Land:",
            ),
            j(130_000, "EVENT idle seq=2 Nothing left: the parser"),
            j(
                130_100,
                "WAITING seq=2 until=x 2 short turn(s) in a row: 4 min back-off",
            ),
            j(
                370_000,
                "CONTINUED seq=2 rule=land-nudge@v1 [aterm harness] Still landing:",
            ),
            j(373_500, "EVENT idle seq=3 Nothing left: the parser"),
            j(
                373_600,
                "WAITING seq=3 until=x 3 short turn(s) in a row: 8 min back-off",
            ),
        ],
        ledger: vec![
            typed(125_000, "land@v1", LAND),
            typed(370_000, "land-nudge@v1", &format!("{LAND_NUDGE} {suffix}")),
        ],
        status: vec![status(1_000, "open", "land")],
        meta: vec![no_meta(1_000)],
        end_ms: T0 + 380_000,
        ..Seen::default()
    }
}

/// The repository chains and their scenarios.
const REPO_CHAINS: [(Chain, &str); 5] = [
    (Chain::LandC, "L4"),
    (Chain::LandF, "L8"),
    (Chain::LandG, "L9"),
    (Chain::LandH, "L10"),
    (Chain::LandI, "L11"),
];

#[test]
fn land_b_and_the_repository_chains_pass_on_a_nudge_naming_what_is_left() {
    assert_eq!(
        verdicts(Chain::LandB, &nudged(&open_suffix()), "claude"),
        [("L2", "PASS")]
    );
    for (c, id) in REPO_CHAINS {
        let (w, why) = one(
            c,
            &nudged(&repo_suffix(c.repo())),
            "claude",
            c.scenarios()[0],
        );
        assert_eq!((id, w), (id, "PASS"), "{c:?}: {why}");
        // each carries the live negative control of L5's judge
        assert!(why.contains("L5's judge over these records: FAIL"), "{why}");
        // the wrong leg named, or none
        for other in [
            Repo::Untracked,
            Repo::Changed,
            Repo::Stashed,
            Repo::Unpushed,
            Repo::Unlanded,
        ] {
            if other != c.repo() {
                let (w, why) = one(c, &nudged(&repo_suffix(other)), "claude", c.scenarios()[0]);
                assert_eq!(w, "FAIL", "{c:?} named {other:?}: {why}");
            }
        }
        assert_eq!(verdicts(c, &nudged(""), "claude"), [(id, "FAIL")]);
        assert_eq!(
            verdicts(c, &nudged(&open_suffix()), "claude"),
            [(id, "FAIL")]
        );
    }
}

#[test]
fn the_nudge_chains_fail_a_stray_suffix_a_need_or_an_unread_yield() {
    let open = open_suffix();
    let repo = repo_suffix(Repo::Untracked);
    // the wrong suffix, or none
    assert_eq!(
        verdicts(Chain::LandB, &nudged(&repo), "claude"),
        [("L2", "FAIL")]
    );
    // L2's nudge names the repository beside the open work (its repository is clean)
    assert_eq!(
        verdicts(Chain::LandB, &nudged(&format!("{open} {repo}")), "claude"),
        [("L2", "FAIL")]
    );
    // L4's names open work beside the repository (its defense named none)
    assert_eq!(
        verdicts(Chain::LandC, &nudged(&format!("{repo} {open}")), "claude"),
        [("L4", "FAIL")]
    );
    // a needs carry-on where no need was named
    assert_eq!(
        verdicts(
            Chain::LandB,
            &nudged(&format!("{open} {NEEDS_ACK}")),
            "claude"
        ),
        [("L2", "FAIL")]
    );
    // NEGATIVE CONTROL OF THE NEEDS ENTRY: none was named, none may be raised
    let mut s = nudged(&open);
    s.journal.insert(
        2,
        j(
            130_000,
            "NEEDS seq=2 count=1 attention=OK rotate the deploy key",
        ),
    );
    assert_eq!(verdicts(Chain::LandB, &s, "claude"), [("L2", "FAIL")]);
    let mut s = nudged(&repo);
    s.meta.push(needs_meta(131_000, "claude"));
    assert_eq!(verdicts(Chain::LandC, &s, "claude"), [("L4", "FAIL")]);
    let mut s = nudged(&repo);
    s.status
        .push(status_of(131_000, "open", "ask", "land", "aterm-harness@1"));
    assert_eq!(verdicts(Chain::LandC, &s, "claude"), [("L4", "FAIL")]);
    // a landing fails it
    let mut s = nudged(&open);
    s.journal.push(j(374_000, "DONE seq=3 rule=land@v1 x"));
    assert_eq!(verdicts(Chain::LandB, &s, "claude"), [("L2", "FAIL")]);
    let mut s = nudged(&repo);
    s.status.push(status(374_000, "landed", "land"));
    assert_eq!(verdicts(Chain::LandC, &s, "claude"), [("L4", "FAIL")]);
    // the nudge's yield never read: unobserved
    let mut s = nudged(&repo);
    s.journal.truncate(4);
    assert_eq!(verdicts(Chain::LandC, &s, "claude"), [("L4", "UNOBSERVED")]);
    // read, but no point decided there
    let mut s = nudged(&repo);
    s.journal.pop();
    assert_eq!(verdicts(Chain::LandC, &s, "claude"), [("L4", "FAIL")]);
    // not nudged yet: unobserved
    let mut s = nudged(&open);
    s.journal.truncate(3);
    s.ledger.pop();
    assert_eq!(verdicts(Chain::LandB, &s, "claude"), [("L2", "UNOBSERVED")]);
}

fn tall() -> Seen {
    let mut s = nudged(NEEDS_ACK);
    s.journal.insert(
        1,
        j(
            130_000,
            "NEEDS seq=2 count=1 attention=OK rotate the deploy key",
        ),
    );
    s.meta = vec![no_meta(1_000), needs_meta(131_000, "muse")];
    s
}

#[test]
fn land_d_passes_unlanded_with_the_need_raised_and_carried_on() {
    assert_eq!(verdicts(Chain::LandD, &tall(), "muse"), [("L6", "PASS")]);
    let mut landed = tall();
    landed.journal.push(j(131_000, "DONE seq=2 rule=land@v1 x"));
    landed.journal.sort();
    assert_eq!(verdicts(Chain::LandD, &landed, "muse"), [("L6", "FAIL")]);
    let mut unraised = tall();
    unraised.journal.remove(1);
    unraised.meta.truncate(1);
    assert_eq!(verdicts(Chain::LandD, &unraised, "muse"), [("L6", "FAIL")]);
    // the need raised, then nothing more typed: the carry-on unobserved, never a pass
    let mut blocked = tall();
    blocked.journal.retain(|(_, l)| !l.contains("land-nudge"));
    blocked.ledger.pop();
    blocked.end_ms = T0 + 960_000;
    assert_eq!(
        verdicts(Chain::LandD, &blocked, "muse"),
        [("L6", "UNOBSERVED")]
    );
    // a nudge with no word of the need
    let mut silent = tall();
    silent.ledger[1].command = LAND_NUDGE.to_string();
    assert_eq!(verdicts(Chain::LandD, &silent, "muse"), [("L6", "FAIL")]);
}

/// L7 as a live run that passes it records it: the record `working` before
/// the agent started, the land act at once (its manager waits: no back-off),
/// the landing at its yield, the record `done why=landed`.
fn tasked() -> Seen {
    Seen {
        journal: vec![
            j(5_000, "EVENT idle seq=1 Implemented the parser"),
            j(
                5_200,
                "CONTINUED seq=1 rule=land@v1 [aterm harness] Autopilot is Land:",
            ),
            j(9_000, "EVENT idle seq=2 Nothing left: the parser"),
            j(9_000, "DONE seq=2 rule=land@v1 the worker defended"),
            j(9_050, "TASK seq=2 done why=landed rev=2 result_bytes=120"),
        ],
        ledger: vec![
            point(5_000, 1),
            typed(5_200, "land@v1", LAND),
            point(9_000, 2),
            task_done(9_000),
        ],
        status: vec![
            status(1_000, "open", "land"),
            status(10_000, "landed", "land"),
        ],
        meta: vec![no_meta(1_000)],
        task: vec![
            j(
                -2_000,
                "OK task rev=1 state=working from=ctl since_ms=0 result_bytes=0 agent=- attention=-",
            ),
            j(
                10_000,
                "OK task rev=2 state=done from=ctl since_ms=0 result_bytes=120 why=landed result_path=x agent=gemini attention=-",
            ),
        ],
        reply_ms: None,
        end_ms: T0 + 110_000,
        stub: Some(StubSeen {
            baseline: 1,
            read: 2,
            new_turns: 0,
        }),
    }
}

#[test]
fn land_e_passes_on_a_workers_task_marked_done_why_landed() {
    let (w, why) = one(Chain::LandE, &tasked(), "gemini", Scenario::L7);
    assert_eq!(w, "PASS", "{why}");
    // the record done by another word
    let mut s = tasked();
    s.task[1].1 = s.task[1].1.replace("why=landed", "why=marker");
    assert_eq!(verdicts(Chain::LandE, &s, "gemini"), [("L7", "FAIL")]);
    // never done
    let mut s = tasked();
    s.task.pop();
    assert_eq!(verdicts(Chain::LandE, &s, "gemini"), [("L7", "FAIL")]);
    // no TASK line journaled
    let mut s = tasked();
    s.journal.pop();
    assert_eq!(verdicts(Chain::LandE, &s, "gemini"), [("L7", "FAIL")]);
    // the ladder went on after the landing
    let mut s = tasked();
    s.journal
        .push(j(9_100, "WAITING seq=2 until=x 2 short turn(s) in a row"));
    assert_eq!(verdicts(Chain::LandE, &s, "gemini"), [("L7", "FAIL")]);
    // no landing at all
    let mut s = tasked();
    s.journal.truncate(3);
    assert_eq!(verdicts(Chain::LandE, &s, "gemini"), [("L7", "FAIL")]);
    // watched under its window
    let mut s = tasked();
    s.end_ms = T0 + 50_000;
    assert_eq!(verdicts(Chain::LandE, &s, "gemini"), [("L7", "UNOBSERVED")]);
    // the mark never took: unobserved
    let mut s = tasked();
    s.task = vec![j(-2_000, "OK task -")];
    assert_eq!(verdicts(Chain::LandE, &s, "gemini"), [("L7", "UNOBSERVED")]);
    // the startup deadline stalled the record while the worker worked (agy
    // 1.4.0, wf8-agy: `stalled why=startup` at +111 s, "no Claude Code
    // started; `agy` holds the tab"), then the landing marked it done: FAIL
    let mut s = tasked();
    s.task.insert(
        1,
        j(
            3_000,
            "OK task rev=2 state=stalled from=ctl since_ms=1208 result_bytes=0 why=startup agent=idle attention=the%20worker%20never%20started",
        ),
    );
    let (w, why) = one(Chain::LandE, &s, "agy", Scenario::L7);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("state=stalled"), "{why}");
    // a status need of any kind while it worked: FAIL
    let mut s = tasked();
    s.status.insert(
        1,
        j(
            3_000,
            "task=open needs=stalled autopilot=land supervisor=aterm-harness@1 agent=busy",
        ),
    );
    let (w, why) = one(Chain::LandE, &s, "agy", Scenario::L7);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("needs=stalled"), "{why}");
}

fn cruised() -> Seen {
    Seen {
        journal: vec![
            j(5_000, "EVENT idle seq=1 Merged the parser. All done."),
            j(
                5_100,
                "WAITING seq=1 until=x 1 short turn(s) in a row: 2 min back-off",
            ),
            j(
                125_000,
                "CONTINUED seq=1 rule=continue-done-check@v1 [aterm harness] x",
            ),
            j(128_500, "EVENT idle seq=2 ⏺ Nothing left: the parser"),
            j(
                128_600,
                "WAITING seq=2 until=x 2 short turn(s) in a row: 4 min back-off",
            ),
            j(
                370_000,
                "CONTINUED seq=2 rule=continue-done-check@v1 [aterm harness] x",
            ),
        ],
        ledger: vec![
            typed(125_000, "continue-done-check@v1", DONE_CHECK),
            typed(370_000, "continue-done-check@v1", DONE_CHECK),
        ],
        status: vec![status(1_000, "open", "cruise")],
        meta: vec![no_meta(1_000)],
        end_ms: T0 + 372_000,
        ..Seen::default()
    }
}

#[test]
fn cruise_passes_on_a_check_whose_defense_is_read_and_a_next_point_that_goes_on() {
    assert_eq!(
        verdicts(Chain::Cruise, &cruised(), "agy"),
        [("C1", "PASS"), ("C2", "PASS")]
    );
    let c2 = |s: &Seen| verdicts(Chain::Cruise, s, "agy")[1];
    let mut ended = cruised();
    ended
        .journal
        .push(j(371_000, "DONE seq=3 rule=continue-done-check@v1 x"));
    assert_eq!(c2(&ended), ("C2", "FAIL"));
    let mut landed = cruised();
    landed.status.push(status(371_000, "landed", "cruise"));
    assert_eq!(c2(&landed), ("C2", "FAIL"));
    let mut waiting = cruised();
    waiting.journal.pop();
    assert_eq!(c2(&waiting), ("C2", "UNOBSERVED"));
    // THE CHECK TYPED AGAIN WITH NOTHING READ BETWEEN: the defense never read
    let blind = Seen {
        journal: vec![
            j(
                125_000,
                "CONTINUED seq=1 rule=continue-done-check@v1 [aterm harness] x",
            ),
            j(
                126_000,
                "CONTINUED seq=1 rule=continue-done-check@v1 [aterm harness] x",
            ),
        ],
        ledger: vec![
            typed(125_000, "continue-done-check@v1", DONE_CHECK),
            typed(126_000, "continue-done-check@v1", DONE_CHECK),
        ],
        status: vec![status(1_000, "open", "cruise")],
        end_ms: T0 + 130_000,
        ..Seen::default()
    };
    assert_eq!(
        verdicts(Chain::Cruise, &blind, "agy"),
        [("C1", "PASS"), ("C2", "UNOBSERVED")]
    );
    // the defense read, but no point decided before the next act
    let mut undecided = cruised();
    undecided.journal.remove(4);
    assert_eq!(c2(&undecided), ("C2", "FAIL"));
    // a need raised where none was named
    let mut needs = cruised();
    needs.meta.push(needs_meta(129_000, "agy"));
    assert_eq!(c2(&needs), ("C2", "FAIL"));
    // C1: the check with no ledger row, or naming the (clean) repository
    let mut unledgered = cruised();
    unledgered.ledger.clear();
    assert_eq!(
        verdicts(Chain::Cruise, &unledgered, "agy")[0],
        ("C1", "FAIL")
    );
    let mut repo = cruised();
    repo.ledger[0].command = format!("{DONE_CHECK} {}", repo_suffix(Repo::Untracked));
    assert_eq!(verdicts(Chain::Cruise, &repo, "agy")[0], ("C1", "FAIL"));
    let none = Seen {
        end_ms: T0 + 900_000,
        ..Seen::default()
    };
    assert_eq!(
        verdicts(Chain::Cruise, &none, "agy"),
        [("C1", "FAIL"), ("C2", "UNOBSERVED")]
    );
}

fn off_quiet() -> Seen {
    Seen {
        status: vec![status_of(1_000, "-", "-", "off", "-")],
        meta: vec![no_meta(1_000)],
        reply_ms: Some(T0 + 5_000),
        end_ms: T0 + 280_000,
        stub: Some(StubSeen {
            baseline: 0,
            read: 2,
            new_turns: 0,
        }),
        ..Seen::default()
    }
}

#[test]
fn off_passes_only_with_no_supervisor_and_quiet_past_a_four_minute_step() {
    let off = |s: &Seen| verdicts(Chain::Off, s, "claude");
    assert_eq!(off(&off_quiet()), [("OFF", "PASS")]);
    let mut typed_off = off_quiet();
    typed_off
        .ledger
        .push(typed(125_000, "continue@v1", CONTINUE_TEXT));
    assert_eq!(off(&typed_off), [("OFF", "FAIL")]);
    // a key pressed is an act too
    let mut approved = off_quiet();
    approved
        .ledger
        .push(row(60_000, "approve@v1", "approved", "1", "a box"));
    assert_eq!(off(&approved), [("OFF", "FAIL")]);
    // a supervisor on the tab, though it typed nothing in the watch
    let mut supervised = off_quiet();
    supervised
        .status
        .push(status_of(2_000, "open", "-", "off", "aterm-harness@1"));
    assert_eq!(off(&supervised), [("OFF", "FAIL")]);
    let mut journaled = off_quiet();
    journaled.journal.push(j(5_000, "EVENT idle seq=1 x"));
    assert_eq!(off(&journaled), [("OFF", "FAIL")]);
    // the stub saw a harness turn of the conversation
    let mut stubbed = off_quiet();
    stubbed.stub = Some(StubSeen {
        baseline: 0,
        read: 3,
        new_turns: 1,
    });
    assert_eq!(off(&stubbed), [("OFF", "FAIL")]);
    // 150 s is no longer enough: a first act after a 4 min step comes at ~244 s
    let mut short = off_quiet();
    short.end_ms = T0 + 160_000;
    assert_eq!(off(&short), [("OFF", "UNOBSERVED")]);
    let mut unread = off_quiet();
    unread.stub = None;
    assert_eq!(off(&unread), [("OFF", "UNOBSERVED")]);
    let mut not_off = off_quiet();
    not_off.status = vec![status_of(1_000, "-", "-", "cruise", "-")];
    assert_eq!(off(&not_off), [("OFF", "FAIL")]);
}
