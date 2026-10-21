// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lab forever`'s pure parts: the stub's per-turn script (each chain's reply
//! to each act of the harness, by the real act texts), the request readers it
//! reads conversations by, and THE JUDGE over records each scenario passes,
//! fails and goes unobserved on — each pass beside the negative controls that
//! must fail it.

use aterm_agent::harness::repo_work::RepoWork;
use aterm_agent::supervise::approvals::LEFT_REASON;
use aterm_agent::supervise::config::CONTINUE_TEXT;
use aterm_agent::supervise::policy::needs::needs_you;
use aterm_agent::supervise::policy::turn_end::{
    DONE_CHECK, LAND, LAND_ANSWER, LAND_NUDGE, REVERSIBLE_ANSWER, RULE_LEFT, Said, classify_said,
    clean_defense, open_work_suffix, repo_work_suffix, says_done,
};

use super::*;
use crate::stub::{gemini::gemini_users, message_users, responses_users};

fn users(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_string()).collect()
}

/// The text of the reply a spec scripts ([`crate::stub::scripted_reply`]).
fn scripted_reply(spec: &J, users: &[String]) -> Option<String> {
    crate::stub::scripted_reply(spec, users)?
        .get("text")
        .and_then(J::as_str)
        .map(str::to_string)
}

/// The reply the lab's spec picks for a conversation of `chain` whose
/// user texts after its prompt are `acts` (every mode's rules at once: no
/// chain's word holds another's).
fn reply(chain: Chain, acts: &[&str]) -> Option<String> {
    let mut u = vec![chain.prompt()];
    u.extend(acts.iter().map(|s| (*s).to_string()));
    scripted_reply(&spec(&Chain::every()), &u)
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
    // L16, L17: the land act (or a nudge) answered clean, long
    for c in [Chain::LandJ, Chain::LandK] {
        assert_eq!(reply(c, &[]).as_deref(), Some(WORK), "{c:?}");
        assert_eq!(reply(c, &[LAND]).as_deref(), Some(CLEAN), "{c:?}");
        assert_eq!(
            reply(c, &[LAND, LAND_NUDGE]).as_deref(),
            Some(CLEAN),
            "{c:?}"
        );
    }
    // N1-N4: two needs; the continuation answered with a defense naming none;
    // the done check with the first again; the next continuation with it and
    // another — told from the first continuation by the check before it
    let n1 = needs_reply(N1_HEAD, &[NEED_A, NEED_B]);
    assert_eq!(reply(Chain::CruiseN, &[]), Some(n1));
    assert_eq!(
        reply(Chain::CruiseN, &[CONTINUE_TEXT]).as_deref(),
        Some(CLEAN)
    );
    assert_eq!(
        reply(Chain::CruiseN, &[CONTINUE_TEXT, DONE_CHECK]),
        Some(needs_reply(N3_HEAD, &[NEED_A]))
    );
    assert_eq!(
        reply(Chain::CruiseN, &[CONTINUE_TEXT, DONE_CHECK, CONTINUE_TEXT]),
        Some(needs_reply(N4_HEAD, &[NEED_A, NEED_C]))
    );
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
    // and no chain's word holds another's, in any mode
    for a in Chain::every() {
        for b in Chain::every() {
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
    // `after` reads the user texts before the newest one: a turn of the same
    // words told apart by an act the conversation met earlier
    let after = J::parse(
        r#"{"replies": [
            {"when": "A", "last": "go", "after": "check", "text": "go-after-check"},
            {"when": "A", "last": "go", "text": "go"}
        ]}"#,
    )
    .unwrap();
    assert_eq!(
        scripted_reply(&after, &users(&["A", "go"])).as_deref(),
        Some("go")
    );
    assert_eq!(
        scripted_reply(&after, &users(&["A", "check", "go"])).as_deref(),
        Some("go-after-check")
    );
    // the newest text is not one before it
    assert_eq!(
        scripted_reply(&after, &users(&["A", "go check"])).as_deref(),
        Some("go")
    );
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

/// The P2 chains, whose replies open `Wrote ` on purpose.
const P2_CHAINS: [Chain; 2] = [Chain::LandW, Chain::WroteC];

#[test]
fn no_scripted_reply_opens_like_a_muse_tool_row() {
    let heads = ["Ran ", "Wrote ", "Edited "];
    for c in Chain::every()
        .into_iter()
        .filter(|c| !P2_CHAINS.contains(c))
    {
        for r in c.rules() {
            let text = r.get("text").and_then(J::as_str).unwrap_or("");
            assert!(!heads.iter().any(|h| text.starts_with(h)), "{c:?}: {text}");
        }
    }
}

/// P2 LIVE: every reply of the P2 chains opens `Wrote ` with a SENTENCE after
/// it, the shape Muse Code's fixed reader reads as the agent's words
/// (`aterm_phase::muse`'s `names_a_path` is false: its first word holds no
/// `/`, `.` or `…`, it is no lone word, and no `(+` or `— ` row tail follows
/// it); before the fix any `Wrote ` head was its tool row.
#[test]
fn the_p2_replies_open_wrote_with_a_sentence_after_it() {
    let mut n = 0;
    for c in P2_CHAINS {
        for r in c.rules() {
            let text = r.get("text").and_then(J::as_str).unwrap_or("");
            let head = text.lines().next().unwrap_or("");
            let rest = head
                .strip_prefix("Wrote ")
                .unwrap_or_else(|| panic!("{c:?}: {text}"));
            let (first, after) = rest.split_once(' ').unwrap_or((rest, ""));
            assert!(!first.contains(['/', '.', '…']), "{c:?}: {head}");
            assert!(!after.is_empty(), "{c:?}: {head}");
            assert!(
                !after.starts_with("(+") && !after.starts_with("— "),
                "{c:?}: {head}"
            );
            n += 1;
        }
    }
    assert_eq!(n, 6);
}

/// The coverage chains' words read as the product reads them: the question
/// is a question (no choice, nothing destructive) and no done claim; the
/// person's reply asks and claims nothing; P2's replies carry the claim, the
/// need and the clean defenses L14 and C3 need; Land's answer opens with the
/// key the stub picks its yield by; and the harness's answers, unmarked,
/// still count as its turns.
#[test]
fn the_coverage_replies_read_as_their_scenarios_need() {
    assert_eq!(classify_said(Some(QUESTION)), Said::Question);
    assert!(!says_done(Some(QUESTION)) && needs_you(QUESTION).is_empty());
    assert!(QUESTION.contains(QUESTION_KEY));
    assert_eq!(classify_said(Some(PERSON_REPLY)), Said::Plain);
    assert!(!says_done(Some(PERSON_REPLY)));
    assert_eq!(classify_said(Some(W_WORK)), Said::Plain);
    assert!(!says_done(Some(W_WORK)) && !clean_defense(Some(W_WORK)));
    assert!(W_WORK.starts_with(W_WORK_KEY) && W_DONE.starts_with(W_DONE_KEY));
    assert_eq!(needs_you(W_NEEDS), vec![NEED.to_string()]);
    assert!(!clean_defense(Some(W_NEEDS)));
    assert!(clean_defense(Some(W_DEFENSE)) && clean_defense(Some(W_CHECKED)));
    assert!(says_done(Some(W_DONE)) && !clean_defense(Some(W_DONE)));
    assert!(clean_defense(Some(CLEAN)) && !clean_defense(Some(&open_defense())));
    assert!(LAND_ANSWER.starts_with(ANSWER_KEY) && ANSWER_KEY.starts_with(ANSWER_HEAD));
    for said in [
        LAND_ANSWER,
        REVERSIBLE_ANSWER,
        LAND,
        LAND_NUDGE,
        DONE_CHECK,
        CONTINUE_TEXT,
    ] {
        assert!(harness_said(said), "{said}");
    }
    for not in [PERSON_REPLY, WORK, QUESTION, ORCHESTRATED, RESUMED] {
        assert!(!harness_said(not), "{not}");
    }
    // each act key stands in its act alone, the answer's included
    assert!(!LAND_ANSWER.contains(LAND_KEY) && !LAND_ANSWER.contains(NUDGE_KEY));
    assert!(!LAND.contains(ANSWER_KEY) && !LAND_NUDGE.contains(ANSWER_KEY));
    assert!(ORCHESTRATED.contains(ORCHESTRATED_KEY) && RESUMED.contains(RESUMED_KEY));
    for act in [LAND, LAND_NUDGE, LAND_ANSWER] {
        assert!(
            !act.contains(ORCHESTRATED_KEY) && !act.contains(RESUMED_KEY),
            "{act}"
        );
    }
    assert_eq!(cap_reason(), "continue_per_hour = 1 reached");
    assert_eq!(cap_window_ms(), 60 * 60_000);
}

#[test]
fn the_coverage_chains_are_scripted_turn_by_turn() {
    use Chain::*;
    assert_eq!(reply(LandQ, &[]).as_deref(), Some(WORK));
    assert_eq!(reply(LandQ, &[LAND]).as_deref(), Some(QUESTION));
    assert_eq!(reply(LandQ, &[LAND, LAND_ANSWER]).as_deref(), Some(CLEAN));
    assert_eq!(
        reply(LandQ, &[LAND, LAND_ANSWER, LAND_NUDGE]).as_deref(),
        Some(CLEAN)
    );
    // the person's question, then the orchestrator's turn, then Land's act
    assert!(LandP.prompt().ends_with(PERSON_ASKS));
    assert_eq!(reply(LandP, &[]).as_deref(), Some(PERSON_REPLY));
    assert_eq!(reply(LandP, &[ORCHESTRATED]).as_deref(), Some(WORK));
    assert_eq!(reply(LandP, &[ORCHESTRATED, LAND]).as_deref(), Some(CLEAN));
    assert_eq!(reply(LandW, &[]).as_deref(), Some(W_WORK));
    assert_eq!(reply(LandW, &[LAND]).as_deref(), Some(W_NEEDS));
    assert_eq!(
        reply(LandW, &[LAND, LAND_NUDGE]).as_deref(),
        Some(W_DEFENSE)
    );
    assert_eq!(reply(WroteC, &[]).as_deref(), Some(W_DONE));
    assert_eq!(reply(WroteC, &[DONE_CHECK]).as_deref(), Some(W_CHECKED));
    assert_eq!(
        reply(WroteC, &[DONE_CHECK, CONTINUE_TEXT]).as_deref(),
        Some(W_DONE)
    );
    // P5: land@v1 answered as work (a second short turn), the nudge landing it
    assert_eq!(reply(LandT, &[]).as_deref(), Some(WORK));
    assert_eq!(reply(LandT, &[LAND]).as_deref(), Some(WORK));
    assert_eq!(reply(LandT, &[LAND, LAND_NUDGE]).as_deref(), Some(CLEAN));
    assert_eq!(reply(Cap, &[]).as_deref(), Some(WORK));
    assert_eq!(reply(Cap, &[LAND]), Some(open_defense()));
    assert_eq!(reply(Cap, &[LAND, RESUMED]), Some(open_defense()));
    assert_eq!(
        reply(Cap, &[LAND, RESUMED, LAND_NUDGE]).as_deref(),
        Some(CLEAN)
    );
    assert_eq!(reply(Worker, &[]).as_deref(), Some(WORK));
    assert_eq!(reply(Worker, &[LAND]).as_deref(), Some(CLEAN));
    assert!(Mgr.rules().is_empty());
    // W1: the land act meets the wall; whatever marked text goes on after
    // it (the resume, Land's words) gets the clean defense
    assert_eq!(reply(Wall, &[]).as_deref(), Some(WORK));
    assert_eq!(reply(Wall, &[LAND]).as_deref(), Some(CLEAN));
    assert_eq!(reply(Wall, &[LAND, LAND_NUDGE]).as_deref(), Some(CLEAN));
    let resume = format!("{MARK} carry on");
    assert_eq!(reply(Wall, &[LAND, &resume]).as_deref(), Some(CLEAN));
    // W2: scripted as W1, under its own word
    assert_eq!(reply(WallHid, &[]).as_deref(), Some(WORK));
    assert_eq!(reply(WallHid, &[LAND]).as_deref(), Some(CLEAN));
    assert_eq!(reply(WallHid, &[LAND, &resume]).as_deref(), Some(CLEAN));
    assert_ne!(WallHid.token(), Wall.token());
}

/// W2's wall resets far enough past the land act (two minutes on), its take
/// window (half a minute) and every command the hide may enter (about four
/// seconds each) that the notice is out of sight at least
/// [`HIDE_BEFORE_RESET_MS`] before the reset; W1's is as it was.
#[test]
fn the_remembered_walls_reset_leaves_room_for_the_hide() {
    assert_eq!(Chain::Wall.wall_ahead_ms(), WALL_AHEAD_MS);
    for now in [1_792_500_010_000_i64, 1_792_500_059_999, 1_792_500_060_000] {
        let r = reset_s_ahead(now, Chain::WallHid.wall_ahead_ms()) * 1000;
        assert_eq!(r % 60_000, 0, "{now}");
        let hidden_by = now + 120_000 + 30_000 + i64::try_from(HIDE_TRIES).unwrap() * 4_000;
        assert!(r - hidden_by >= HIDE_BEFORE_RESET_MS, "{now}");
    }
}

/// W1's wall resets on a whole minute at least four minutes ahead, so the
/// land act two minutes on meets it and Muse Code's words name its reset.
#[test]
fn the_wall_resets_on_a_whole_minute_past_the_land_act() {
    for now in [1_792_500_010_000_i64, 1_792_500_059_999, 1_792_500_060_000] {
        let r = reset_s_ahead(now, WALL_AHEAD_MS) * 1000;
        assert_eq!(r % 60_000, 0, "{now}");
        assert!(
            r >= now + WALL_AHEAD_MS && r < now + WALL_AHEAD_MS + 60_000,
            "{now}"
        );
        assert!(
            r > now + 120_000 + 30_000,
            "the 2 min step's act meets it: {now}"
        );
    }
}

#[test]
fn the_coverage_modes_pick_their_chains() {
    use Chain::*;
    assert_eq!(
        Chain::of_mode(Mode::Cover),
        [LandQ, LandP, LandW, WroteC, LandT, Mgr, Worker, Off]
    );
    assert_eq!(Chain::of_mode(Mode::Cap), [Cap, Off]);
    // `spawn agent=claude` starts Claude Code alone
    assert_eq!(
        Chain::of_mode_on(Mode::Cover, Harness::Gemini),
        [LandQ, LandP, LandW, WroteC, LandT, Off]
    );
    assert_eq!(
        Chain::of_mode_on(Mode::Cover, Harness::Claude),
        Chain::of_mode(Mode::Cover)
    );
    // the measured modes are as they were
    assert_eq!(Chain::of_mode_on(Mode::Both, Harness::Muse), Chain::ALL);
    assert_eq!(
        [LandQ, LandP, LandW, WroteC, LandT, Mgr, Worker, Cap].map(Chain::autopilot),
        [
            "land", "land", "land", "cruise", "land", "off", "land", "land"
        ]
    );
    assert_eq!(
        [LandP, Mgr, Worker, LandQ].map(Chain::opening),
        [
            Opening::Person,
            Opening::Idle,
            Opening::Spawned,
            Opening::Prompt
        ]
    );
    assert!(LandT.tasked() && LandT.managed() && Worker.managed() && !Worker.tasked());
    assert!(LandQ.lands() && LandW.lands() && Worker.lands() && !LandP.lands());
    assert_eq!(Cap.limit_ms(), CAP_LIMIT_MS);
    assert_eq!(LandQ.limit_ms(), CHAIN_LIMIT_MS);
    assert_eq!(
        [LandP, Cap, LandQ].map(Chain::orchestrated),
        [Some(ORCHESTRATED), Some(RESUMED), None]
    );
    assert_eq!(Chain::every().len(), 26);
    assert!(
        Chain::every()
            .iter()
            .all(|c| !c.instant() || Chain::ALL.contains(c))
    );
    // the wall alone, on every harness: the stub draws each one's own wall
    assert_eq!(Chain::of_mode(Mode::Wall), [Wall]);
    for h in Harness::ALL {
        assert_eq!(Chain::of_mode_on(Mode::Wall, h), [Wall], "{h:?}");
        assert_eq!(wall_out_of_reach(h, Mode::Wall), None, "{h:?}");
    }
    assert!(Wall.lands() && !Wall.managed() && Wall.autopilot() == "land");
    assert_eq!(Wall.scenarios(), [Scenario::W1]);
    // the remembered wall alone, where a local command takes the notice off
    // the screen (Muse Code, agy); Claude Code and Gemini CLI name why not
    assert_eq!(Chain::of_mode(Mode::WallHid), [WallHid]);
    for h in [Harness::Muse, Harness::Agy] {
        assert_eq!(Chain::of_mode_on(Mode::WallHid, h), [WallHid], "{h:?}");
        assert_eq!(wall_out_of_reach(h, Mode::WallHid), None, "{h:?}");
    }
    for h in [Harness::Claude, Harness::Gemini] {
        assert!(Chain::of_mode_on(Mode::WallHid, h).is_empty(), "{h:?}");
        assert!(wall_out_of_reach(h, Mode::WallHid).is_some(), "{h:?}");
    }
    assert_eq!(
        Harness::ALL.map(|h| hide_of(h).map(|x| x.command)),
        [
            Err(hide_of(Harness::Claude).unwrap_err()),
            Err(hide_of(Harness::Gemini).unwrap_err()),
            Ok(AGY_HIDE_COMMAND),
            Ok(HIDE_COMMAND)
        ]
    );
    // the claim each harness's wall is raised with, and its grace
    assert_eq!(
        Harness::ALL.map(wall_claim),
        ["five_hour", "code_assist", "five_hour", "five_hour"]
    );
    assert_eq!(
        Harness::ALL.map(|h| wall_grace_ms(h.name())),
        [150_000, 60_000, 60_000, 60_000]
    );
    // no other mode is out of reach anywhere
    for m in [Mode::Land, Mode::Cruise, Mode::Both, Mode::Cover, Mode::Cap] {
        assert!(
            Harness::ALL
                .iter()
                .all(|&h| wall_out_of_reach(h, m).is_none())
        );
    }
    assert!(WallHid.lands() && !WallHid.managed() && WallHid.autopilot() == "land");
    assert_eq!(WallHid.scenarios(), [Scenario::W2]);
    assert!(Wall.walled() && WallHid.walled() && !LandA.walled() && !Off.walled());
    assert_eq!(
        [Mode::Wall, Mode::WallHid, Mode::Cap, Mode::Both].map(Mode::walled),
        [Some(Scenario::W1), Some(Scenario::W2), None, None]
    );
    // no other mode's chains meet the wall
    for m in [Mode::Land, Mode::Cruise, Mode::Both, Mode::Cover, Mode::Cap] {
        assert!(Chain::of_mode(m).iter().all(|c| !c.walled()), "{m:?}");
    }
    let v = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    for (w, m) in [
        ("cover", Mode::Cover),
        ("cap", Mode::Cap),
        ("wall", Mode::Wall),
        ("wall-hid", Mode::WallHid),
    ] {
        let a = parse_args(&v(&["--harness", "claude", "--mode", w]))
            .unwrap()
            .unwrap();
        assert_eq!(a.mode, m);
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
            LandA,
            LandB,
            LandC,
            LandD,
            LandE,
            LandF,
            LandG,
            LandH,
            LandI,
            Off,
            InstantLand,
            LandJ,
            LandK
        ]
    );
    assert_eq!(
        Chain::of_mode(Mode::Cruise),
        [Cruise, Off, InstantCruise, CruiseN]
    );
    // the window's front tab: cruise-n, else the Off control, then the
    // other Cruise chains; never a tab whose end END looks at, nor one that
    // lands
    let front = |cs: &[Chain]| surfaces::front_of(cs).map(|i| cs[i]);
    assert_eq!(front(&Chain::ALL), Some(CruiseN));
    assert_eq!(front(&Chain::of_mode(Mode::Land)), Some(Off));
    assert_eq!(front(&[LandJ, LandK, Cruise, Off]), Some(Off));
    assert_eq!(front(&[LandJ, LandK, InstantCruise]), Some(InstantCruise));
    assert_eq!(front(&[LandJ, LandK]), None);
    assert_eq!(front(&[LandA, InstantLand, LandK]), None);
    for c in surfaces::FRONT {
        assert!(!c.lands() && !surfaces::ENDING.contains(&c), "{c:?}");
    }
    // the chains that pause at END: neither landing nor ending, nor Off
    assert_eq!(
        Chain::ALL
            .into_iter()
            .filter(|c| surfaces::pauses(*c))
            .collect::<Vec<_>>(),
        [
            LandB,
            LandC,
            LandD,
            LandF,
            LandG,
            LandH,
            LandI,
            Cruise,
            InstantCruise,
            CruiseN
        ]
    );
    assert_eq!(CruiseN.limit_ms(), CRUISE_N_LIMIT_MS);
    assert!(!LandJ.lands() && !LandK.lands() && !LandJ.tasked());
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
    assert!(LandA.lands() && LandE.lands() && InstantLand.lands() && !LandC.lands());
    assert!(InstantLand.instant() && InstantCruise.instant());
    assert!(Chain::ALL.iter().filter(|c| c.instant()).count() == 2);
}

/// THE P4 CHAINS ANSWER AT ONCE, THE OTHERS PACED: each rule of an instant
/// chain carries a zero delay and pace of its own, which the stub's reply
/// takes over the spec's; a paced chain's rules carry none, so its replies
/// wait [`REPLY_DELAY_MS`].
#[test]
fn the_instant_chains_answer_at_once_and_the_others_are_paced() {
    let s = spec(&Chain::ALL);
    let ask = Chain::InstantCruise
        .asks()
        .expect("the instant cruise chain asks");
    let picked = |chain: Chain, acts: &[&str]| {
        let mut u = vec![chain.prompt()];
        u.extend(acts.iter().map(|a| (*a).to_string()));
        crate::stub::with_reply(s.clone(), &u)
    };
    for (chain, acts) in [
        (Chain::InstantLand, &[][..]),
        (Chain::InstantLand, &[LAND][..]),
        (Chain::InstantLand, &[LAND, LAND_NUDGE][..]),
        (Chain::InstantCruise, &[][..]),
        (Chain::InstantCruise, &[DONE_CHECK][..]),
        (Chain::InstantCruise, &[DONE_CHECK, CONTINUE_TEXT][..]),
        (Chain::InstantCruise, &[DONE_CHECK, ask.as_str()][..]),
    ] {
        let r = picked(chain, acts);
        assert_eq!(
            r.get("delay_ms").and_then(J::as_i64),
            Some(0),
            "{chain:?} {acts:?}"
        );
        assert_eq!(
            r.get("pace_s").and_then(J::as_f64),
            Some(0.0),
            "{chain:?} {acts:?}"
        );
    }
    assert_eq!(reply(Chain::InstantLand, &[]).as_deref(), Some(WORK));
    assert_eq!(reply(Chain::InstantLand, &[LAND]).as_deref(), Some(CLEAN));
    assert_eq!(
        reply(Chain::InstantCruise, &[]).as_deref(),
        Some(DONE_CLAIM)
    );
    assert_eq!(
        reply(Chain::InstantCruise, &[DONE_CHECK]).as_deref(),
        Some(CLEAN)
    );
    // L16, L17: the land reply held, then paced word by word, past the end's
    // work floor; their work reply paced as any
    for chain in [Chain::LandJ, Chain::LandK] {
        let r = picked(chain, &[LAND]);
        assert_eq!(r.get("delay_ms").and_then(J::as_i64), Some(LONG_DELAY_MS));
        assert_eq!(r.get("pace_s").and_then(J::as_f64), Some(LONG_PACE_S));
        assert_eq!(
            picked(chain, &[]).get("delay_ms").and_then(J::as_i64),
            Some(REPLY_DELAY_MS)
        );
    }
    assert!(long_reply_ms(CLEAN) > surfaces::END_WORK_FLOOR_MS + 5_000);
    // one delta (words gathered to 12 characters), or two
    assert_eq!(long_reply_ms("one two"), LONG_DELAY_MS + LONG_PACE_MS);
    assert_eq!(
        long_reply_ms("one two three four"),
        LONG_DELAY_MS + 2 * LONG_PACE_MS
    );
    // negative control: a paced chain's reply keeps the spec's hold
    for chain in [Chain::LandA, Chain::Cruise, Chain::Off, Chain::CruiseN] {
        let r = picked(chain, &[]);
        assert_eq!(
            r.get("delay_ms").and_then(J::as_i64),
            Some(REPLY_DELAY_MS),
            "{chain:?}"
        );
        assert_eq!(
            r.get("pace_s").and_then(J::as_f64),
            Some(REPLY_PACE_S),
            "{chain:?}"
        );
    }
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

/// `status`'s `end= end_seen= end_cheered=` (§12.5.2) are read by key, and
/// a line from an aterm without them (main before 5a) reads as before: its
/// old keys the same, the new ones absent — never `-`, which says "no end".
/// A key is the whole word before `=`: `end` is not `end_seen`'s prefix.
/// NEGATIVE CONTROL (as run): a reader that drops the `=` anchor reads
/// `end` off `end_seen=1` and fails the last row.
#[test]
fn the_end_fields_are_read_by_key_and_an_older_line_reads_as_before() {
    let tail = "task_state=done task_seq=3 editing=0 gen=1 seq=9";
    let older = format!("OK land_at=- task=landed pause=- {tail}");
    let newer =
        format!("OK land_at=- task=landed pause=- end=1792500000 end_seen=0 end_cheered=1 {tail}");
    for key in ["land_at", "task", "pause", "task_state", "task_seq", "seq"] {
        assert_eq!(word(&older, key), word(&newer, key), "{key}");
        assert!(word(&older, key).is_some(), "{key}");
    }
    let ends = |line: &str| {
        (
            word(line, "end"),
            word(line, "end_seen"),
            word(line, "end_cheered"),
        )
    };
    assert_eq!(ends(&older), (None, None, None), "absent, not `-`");
    assert_eq!(
        ends(&newer),
        (
            Some("1792500000".into()),
            Some("0".into()),
            Some("1".into())
        )
    );
    assert_eq!(
        ends("OK pause=- end=- end_seen=- end_cheered=- task_state=-"),
        (Some("-".into()), Some("-".into()), Some("-".into())),
        "no end: a dash each"
    );
    assert_eq!(word("OK end_seen=1 end_cheered=1", "end"), None);
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
    assert_eq!(b.chains(Harness::Muse), Chain::ALL);
    assert_eq!((b.harnesses, b.mode), (vec![Harness::Muse], Mode::Both));
    // --chains narrows the mode's chains
    let c = parse_args(&v(&[
        "--harness",
        "agy",
        "--mode",
        "land",
        "--chains",
        "instant-land,instant-cruise,land-a",
    ]))
    .unwrap()
    .unwrap();
    // ...and, the window read and no chain of it to put in front, the Off
    // control joins it (`--no-surfaces`: not)
    assert_eq!(
        c.chains(Harness::Agy),
        [Chain::LandA, Chain::Off, Chain::InstantLand]
    );
    let words = |extra: &[&str]| {
        let mut w = vec!["--harness", "claude", "--chains", "land-j,land-k"];
        w.extend_from_slice(extra);
        parse_args(&v(&w)).unwrap().unwrap().chains(Harness::Claude)
    };
    assert_eq!(words(&[]), [Chain::Off, Chain::LandJ, Chain::LandK]);
    assert_eq!(words(&["--no-surfaces"]), [Chain::LandJ, Chain::LandK]);
    let with_cruise = parse_args(&v(&[
        "--harness",
        "claude",
        "--chains",
        "land-j,land-k,cruise",
    ]))
    .unwrap()
    .unwrap();
    assert_eq!(
        with_cruise.chains(Harness::Claude),
        [Chain::Cruise, Chain::LandJ, Chain::LandK]
    );
    // --chains names a chain of any mode, narrowing the mode's on each
    // harness: L15's two run on Claude Code alone, a chain of another mode
    // runs nowhere
    let d = parse_args(&v(&[
        "--harness",
        "all",
        "--mode",
        "cover",
        "--chains",
        "mgr,worker,land-q,instant-land",
    ]))
    .unwrap()
    .unwrap();
    assert_eq!(
        d.chains(Harness::Claude),
        [Chain::LandQ, Chain::Mgr, Chain::Worker]
    );
    assert_eq!(d.chains(Harness::Gemini), [Chain::LandQ]);
    // Worker-only selection retains its prerequisite, in the mode's order.
    let worker = parse_args(&v(&[
        "--harness",
        "all",
        "--mode",
        "cover",
        "--chains",
        "worker",
    ]))
    .unwrap()
    .unwrap();
    assert_eq!(worker.chains(Harness::Claude), [Chain::Mgr, Chain::Worker]);
    assert!(worker.chains(Harness::Gemini).is_empty());
    // Asking only for the manager starts no worker; unrelated chains gain none.
    let manager = parse_args(&v(&[
        "--harness",
        "claude",
        "--mode",
        "cover",
        "--chains",
        "mgr",
    ]))
    .unwrap()
    .unwrap();
    assert_eq!(manager.chains(Harness::Claude), [Chain::Mgr]);
    let question = parse_args(&v(&[
        "--harness",
        "claude",
        "--mode",
        "cover",
        "--chains",
        "land-q",
    ]))
    .unwrap()
    .unwrap();
    assert_eq!(question.chains(Harness::Claude), [Chain::LandQ]);
    let e = parse_args(&v(&["--harness", "muse", "--chains", "wall"]))
        .unwrap()
        .unwrap();
    assert!(e.chains(Harness::Muse).is_empty());
    assert!(parse_args(&v(&["-h"])).unwrap().is_none());
    for bad in [
        &["t"][..],
        &["--harness"],
        &["--harness", "codex"],
        &["--harness", "agy", "--mode", "x"],
        &["--harness", "agy", "--wat"],
        &["--harness", "agy", "--chains"],
        &["--harness", "agy", "--chains", ","],
        &["--harness", "agy", "--chains", "land-z"],
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

/// A journal line at `t` after [`T0`]. A back-off's `until=x` reads as the
/// due it names, the stamp the journal writes (cut to its second): `t` and
/// its `M min back-off`.
fn j(t: i64, line: &str) -> (i64, String) {
    let line = match line.split_once("until=x ") {
        Some((head, words)) => match words
            .split_once(" min back-off")
            .and_then(|(w, _)| w.rsplit(' ').next()?.parse::<i64>().ok())
        {
            Some(min) => format!("{head}until={} {words}", stamp(t + min * 60_000)),
            None => line.to_string(),
        },
        None => line.to_string(),
    };
    (T0 + t, line)
}

/// The journal's stamp of `t` after [`T0`] (`until=`), cut to its second.
fn stamp(t: i64) -> String {
    let iso = crate::util::utc_iso((T0 + t).div_euclid(1000));
    iso.replace(".000Z", "Z")
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
        asked_ms: None,
        ask_refused_ms: None,
        end_ms: T0 + 920_000,
        stub: Some(StubSeen {
            baseline: 2,
            read: 3,
            new_turns: 0,
        }),
        ..Seen::default()
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
    // a record written after the landing's own point, at its very moment
    let mut s = landed_a();
    let at = s.ledger.len() - 1;
    let ts = s.ledger[at].ts;
    s.ledger.insert(at, row(ts, WALL_END, "waiting", "", "x"));
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
        // L5's judge is no part of it: it fails a chain that never landed by
        // construction (F16 of the review of `b9/forever-gaps`)
        assert!(!why.contains("L5's judge"), "{why}");
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
    // THE NEED RAISED, THEN NOTHING MORE TYPED past the back-off's due (4 min
    // from +130.1 s) and a minute more: PERSISTENCE LOST, a FAIL (F9 of the
    // review of `b9/forever-gaps`; it read UNOBSERVED)
    let mut blocked = tall();
    blocked.journal.retain(|(_, l)| !l.contains("land-nudge"));
    blocked.ledger.pop();
    blocked.end_ms = T0 + 960_000;
    let (w, why) = one(Chain::LandD, &blocked, "muse", Scenario::L6);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("PERSISTENCE LOST"), "{why}");
    // watched to before that due and its minute: the carry-on unobserved, never a pass
    blocked.end_ms = T0 + 420_000;
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
        asked_ms: None,
        ask_refused_ms: None,
        end_ms: T0 + 110_000,
        stub: Some(StubSeen {
            baseline: 1,
            read: 2,
            new_turns: 0,
        }),
        ..Seen::default()
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
    // a change of the record nobody read (F13): its rev moved by two
    let mut s = tasked();
    s.task[1].1 = s.task[1].1.replace("rev=2", "rev=3");
    let (w, why) = one(Chain::LandE, &s, "gemini", Scenario::L7);
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("changed between looks unread: rev 1 then 3"),
        "{why}"
    );
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
    // F12 of the review of `b9/forever-gaps`: a doubled act fails every
    // chain's scenarios, not P4's alone (C1 passed it)
    assert_eq!(
        verdicts(Chain::Cruise, &blind, "agy"),
        [("C1", "FAIL"), ("C2", "FAIL")]
    );
    let (_, why) = one(Chain::Cruise, &blind, "agy", Scenario::C1);
    assert!(
        why.contains("acts typed with no point judged between"),
        "{why}"
    );
    // NO NEXT ACT past the check's defense's back-off (4 min from +128.6 s)
    // and a minute more: PERSISTENCE LOST (F9; it read UNOBSERVED)
    let mut lost = cruised();
    lost.journal.pop();
    lost.ledger.pop();
    lost.end_ms = T0 + 430_000;
    let (w, why) = one(Chain::Cruise, &lost, "agy", Scenario::C2);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("PERSISTENCE LOST"), "{why}");
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

// ---- P4: replies sent at once

/// P4L as a live run that passes it records it: the first prompt's reply
/// read at once and backed off (one short turn), `land@v1` typed, its clean
/// defense judged 1 s after it — the landing — and nothing typed through
/// the ladder's next step (two short turns: 4 min) and a minute more.
fn instant_landed() -> Seen {
    Seen {
        journal: vec![
            j(900, "EVENT idle seq=1 Implemented the parser"),
            j(
                1_000,
                "WAITING seq=1 until=x 1 short turn(s) in a row: 2 min back-off",
            ),
            j(
                121_000,
                "CONTINUED seq=1 rule=land@v1 [aterm harness] Autopilot is Land: land",
            ),
            j(122_000, "EVENT idle seq=2 Nothing left: the parser"),
            j(
                122_000,
                "DONE seq=2 rule=land@v1 the worker defended that nothing is left",
            ),
        ],
        ledger: vec![
            point(900, 1),
            typed(121_000, "land@v1", LAND),
            point(122_000, 2),
            task_done(122_000),
        ],
        status: vec![
            status(500, "open", "land"),
            status(123_000, "landed", "land"),
        ],
        status_reads: vec![T0 + 500, T0 + 123_000],
        meta: vec![no_meta(500)],
        task: vec![j(500, "OK task -")],
        reply_ms: None,
        asked_ms: None,
        ask_refused_ms: None,
        end_ms: T0 + 122_000 + 5 * 60_000 + 5_000,
        cover: Cover::default(),
        window: surfaces::Window::default(),
        stub: Some(StubSeen {
            baseline: 1,
            read: 1,
            new_turns: 0,
        }),
    }
}

#[test]
fn p4_land_passes_only_where_every_instant_reply_is_judged_once() {
    let p4l = |s: &Seen| one(Chain::InstantLand, s, "gemini", Scenario::P4L);
    let (w, why) = p4l(&instant_landed());
    assert_eq!(w, "PASS", "{why}");
    // THE FIRST INSTANT REPLY NEVER READ (F7 of the review of
    // `b9/forever-gaps`, which passed it): its point read as no words, the
    // land act typed at that point's back-off
    let mut unread = instant_landed();
    unread.journal[0].1 = "EVENT idle seq=1 -".into();
    let (w, why) = p4l(&unread);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("was never read"), "{why}");
    // read, but the back-off before the act another point's
    let mut other = instant_landed();
    other.journal[1].1 = other.journal[1].1.replace("seq=1", "seq=9");
    let (w, why) = p4l(&other);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("no back-off decided at that point"), "{why}");
    // P4 ITSELF: the first prompt's reply never judged — nothing typed
    let never = Seen {
        journal: vec![j(900, "EVENT idle seq=1 -")],
        status: vec![status(500, "open", "land")],
        end_ms: T0 + 900_000,
        ..Seen::default()
    };
    let (w, why) = p4l(&never);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("never judged"), "{why}");
    // the land act's instant defense never taken: no landing, the act again
    let mut again = instant_landed();
    again.journal.truncate(3);
    again.journal.push(j(
        151_000,
        "WAITING seq=1 until=x the worker to take the land@v1 act",
    ));
    again.journal.push(j(
        391_000,
        "CONTINUED seq=2 rule=land@v1 [aterm harness] Autopilot is Land: land",
    ));
    let (w, why) = p4l(&again);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("never landed"), "{why}");
    // landed, but at the take window's end, not as the answer showed
    let mut late = instant_landed();
    for (t, l) in &mut late.journal {
        if l.starts_with("EVENT idle seq=2") || l.starts_with("DONE ") {
            *t = T0 + 151_500;
        }
    }
    let (w, why) = p4l(&late);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("take window"), "{why}");
    // a second act before the landing, no point judged between: doubled
    let mut doubled = instant_landed();
    doubled.journal.insert(
        3,
        j(
            121_500,
            "CONTINUED seq=1 rule=land@v1 [aterm harness] Autopilot is Land: land",
        ),
    );
    let (w, why) = p4l(&doubled);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("2 acts before the landing"), "{why}");
    // typed after the landing: the landing's own judge fails it
    let mut after = instant_landed();
    after.journal.push(j(
        250_000,
        "CONTINUED seq=3 rule=land-nudge@v1 [aterm harness] Still landing:",
    ));
    assert_eq!(p4l(&after).0, "FAIL");
    // watched under the window: unobserved
    let mut short = instant_landed();
    short.end_ms = T0 + 200_000;
    assert_eq!(p4l(&short).0, "UNOBSERVED");
}

/// P4C as a live run that passes it records it: the done claim read at once
/// and backed off, the check typed, its defense read 0.6 s after it and
/// backed off (two short turns), the next point's act after it.
fn instant_cruised() -> Seen {
    Seen {
        journal: vec![
            j(900, "EVENT idle seq=1 Merged the parser. All done."),
            j(
                1_000,
                "WAITING seq=1 until=x 1 short turn(s) in a row: 2 min back-off",
            ),
            j(
                121_000,
                "CONTINUED seq=1 rule=continue-done-check@v1 [aterm harness] x",
            ),
            j(121_600, "EVENT idle seq=2 Nothing left: the parser"),
            j(
                121_700,
                "WAITING seq=2 until=x 2 short turn(s) in a row: 4 min back-off",
            ),
            j(
                361_700,
                "CONTINUED seq=2 rule=continue-done-check@v1 [aterm harness] x",
            ),
        ],
        ledger: vec![
            typed(121_000, "continue-done-check@v1", DONE_CHECK),
            typed(361_700, "continue-done-check@v1", DONE_CHECK),
        ],
        status: vec![status(500, "open", "cruise")],
        meta: vec![no_meta(500)],
        end_ms: T0 + 365_000,
        ..Seen::default()
    }
}

#[test]
fn p4_cruise_passes_only_where_the_checks_instant_defense_is_its_answer() {
    let p4c = |s: &Seen| one(Chain::InstantCruise, s, "muse", Scenario::P4C);
    let (w, why) = p4c(&instant_cruised());
    assert_eq!(w, "PASS", "{why}");
    // P4 ITSELF: the done claim never judged
    let never = Seen {
        journal: vec![j(900, "EVENT idle seq=1 -")],
        end_ms: T0 + 900_000,
        ..Seen::default()
    };
    let (w, why) = p4c(&never);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("never judged"), "{why}");
    // the defense judged at the take window's end: the act never taken
    let mut late = instant_cruised();
    late.journal[3].0 = T0 + 151_100;
    late.journal[4] = j(
        151_200,
        "WAITING seq=2 until=x 2 short turn(s) in a row: 4 min back-off",
    );
    let (w, why) = p4c(&late);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("take window"), "{why}");
    // the take window's own wait is no decision of the point
    let mut window = instant_cruised();
    window.journal[4] = j(
        121_700,
        "WAITING seq=2 until=x the worker to take the continue-done-check@v1 act",
    );
    assert_eq!(p4c(&window).0, "FAIL");
    // the check typed twice with no point judged between
    let mut doubled = instant_cruised();
    doubled.journal.insert(
        3,
        j(
            121_300,
            "CONTINUED seq=1 rule=continue-done-check@v1 [aterm harness] x",
        ),
    );
    let (w, why) = p4c(&doubled);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("no point judged between"), "{why}");
    // ended under Cruise
    let mut ended = instant_cruised();
    ended.journal.push(j(362_000, "DONE seq=3 rule=land@v1 x"));
    assert_eq!(p4c(&ended).0, "FAIL");
    // the next act not reached: unobserved
    let mut waiting = instant_cruised();
    waiting.journal.pop();
    assert_eq!(p4c(&waiting).0, "UNOBSERVED");
    // A DECISION WITH NOTHING TYPED AFTER IT IS NO GOING ON (F5 of the review
    // of `b9/forever-gaps`, where "then the person's prompt" passed): the
    // person's point decided, nothing typed by its due and a minute more
    let mut stopped = instant_asked();
    stopped.journal.pop();
    stopped.ledger.pop();
    stopped.end_ms = T0 + 128_000 + 8 * 60_000 + 61_000;
    let (w, why) = p4c(&stopped);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("PERSISTENCE LOST"), "{why}");
    // …and watched 3 s past that decision, as the watch once ended: unobserved
    stopped.end_ms = T0 + 131_000;
    assert_eq!(p4c(&stopped).0, "UNOBSERVED");
}

/// P4P's record: the check's instant defense judged and decided at +121.7 s,
/// a person's prompt at +127 s, its instant reply judged at +127.9 s and
/// decided there (an 8 min back-off), the act at its due.
fn instant_asked() -> Seen {
    let mut s = instant_cruised();
    s.journal.truncate(5);
    s.ledger.truncate(1);
    s.journal
        .push(j(127_900, &format!("EVENT idle seq=3 {ASK_REPLY}")));
    s.journal.push(j(
        128_000,
        "WAITING seq=3 until=x 3 short turn(s) in a row: 8 min back-off",
    ));
    s.journal.push(j(
        608_500,
        "CONTINUED seq=3 rule=continue@v1 [aterm harness] Autopilot is Cruise: keep going",
    ));
    s.ledger.push(typed(608_500, "continue@v1", CONTINUE_TEXT));
    s.asked_ms = Some(T0 + 127_000);
    s.end_ms = T0 + 612_000;
    s
}

#[test]
fn p4_a_persons_instant_turn_passes_only_where_it_is_judged_as_it_shows() {
    let p4p = |s: &Seen| one(Chain::InstantCruise, s, "gemini", Scenario::P4P);
    let (w, why) = p4p(&instant_asked());
    assert_eq!(w, "PASS", "{why}");
    // the check's own point stands when the person's prompt follows it
    let (w, why) = one(
        Chain::InstantCruise,
        &instant_asked(),
        "gemini",
        Scenario::P4C,
    );
    assert_eq!(w, "PASS", "{why}");
    // never typed: unobserved, never a pass
    let mut untyped = instant_asked();
    untyped.asked_ms = None;
    assert_eq!(p4p(&untyped).0, "UNOBSERVED");
    // typed but not taken by the composer: unobserved, and said so
    untyped.ask_refused_ms = Some(T0 + 127_000);
    let (w, why) = p4p(&untyped);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("not taken by the composer"), "{why}");
    // P4 ITSELF: the person's turn never judged
    let mut never = instant_asked();
    never.journal.truncate(5);
    let (w, why) = p4p(&never);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("never judged"), "{why}");
    // judged only past the take window: not as it showed
    let mut late = instant_asked();
    late.journal[5].0 = T0 + 158_000;
    late.journal[6].0 = T0 + 158_100;
    let (w, why) = p4p(&late);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("not as it showed"), "{why}");
    // an act typed into the person's turn before its reply was judged
    let mut typed_over = instant_asked();
    typed_over.journal.insert(
        5,
        j(
            127_500,
            "CONTINUED seq=2 rule=continue-done-check@v1 [aterm harness] x",
        ),
    );
    let (w, why) = p4p(&typed_over);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("before its reply was judged"), "{why}");
    // the take window's own wait decides nothing
    let mut window = instant_asked();
    window.journal[6] = j(
        128_000,
        "WAITING seq=3 until=x the worker to take the continue-done-check@v1 act",
    );
    let (w, why) = p4p(&window);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("no back-off decided at that point"), "{why}");
    // DECIDED AT ANOTHER POINT, OR LEFT (F6 of the review of
    // `b9/forever-gaps`, both passed): another point's back-off is no
    // decision of this one, and the control verb stamps no person's hand
    let mut other = instant_asked();
    other.journal[6].1 = other.journal[6].1.replace("seq=3", "seq=2");
    let (w, why) = p4p(&other);
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("no back-off decided at that point (seq=3)"),
        "{why}"
    );
    let mut left = instant_asked();
    left.journal[6] = j(
        128_000,
        "LEFT seq=3 rule=left-to-person@v1 a person's own exchange (their message, answered within 10 min): nothing is typed until the next turn ends",
    );
    let (w, why) = p4p(&left);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("though no person's hand typed it"), "{why}");
    // NOTHING TYPED AFTER THE PERSON'S POINT (F5, it passed 3 s after the
    // decision): past its due and a minute more, persistence lost; before
    // it, unobserved
    let mut stopped = instant_asked();
    stopped.journal.pop();
    stopped.ledger.pop();
    stopped.end_ms = T0 + 128_000 + 8 * 60_000 + 61_000;
    let (w, why) = p4p(&stopped);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("PERSISTENCE LOST"), "{why}");
    stopped.end_ms = T0 + 131_000;
    assert_eq!(p4p(&stopped).0, "UNOBSERVED");
    // the act after it typed before its due: every scenario of the chain fails
    let mut early = instant_asked();
    let n = early.journal.len() - 1;
    early.journal[n].0 = T0 + 300_000;
    early.ledger[1].ts = T0 + 300_000;
    let (w, why) = p4p(&early);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("BEFORE the due its back-off named"), "{why}");
    // an act typed twice with no point judged between
    let mut doubled = instant_asked();
    doubled.journal.insert(
        3,
        j(
            121_300,
            "CONTINUED seq=1 rule=continue-done-check@v1 [aterm harness] x",
        ),
    );
    let (w, why) = p4p(&doubled);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("no point judged between"), "{why}");
}

#[test]
fn a_persons_prompt_is_due_only_while_the_loop_waits_out_a_decided_back_off() {
    let s = instant_asked();
    // the check's defense decided at +121.7 s
    let at = |t: i64| T0 + t;
    let mut before = s.clone();
    before.journal.truncate(5);
    assert!(!ask_due(&before, at(126_000)), "under five seconds");
    assert!(ask_due(&before, at(127_000)));
    // the take window's wait is no decision…
    let mut window = before.clone();
    window.journal[4] = j(
        121_700,
        "WAITING seq=2 until=x the worker to take the continue-done-check@v1 act",
    );
    assert!(!ask_due(&window, at(127_000)));
    // …the back-off decided at its end is
    window.journal.push(j(
        151_700,
        "WAITING seq=2 until=x 2 short turn(s) in a row: 4 min back-off",
    ));
    assert!(!ask_due(&window, at(155_000)));
    assert!(ask_due(&window, at(157_000)));
    // another act typed since: the back-off is over
    let mut moved = before.clone();
    moved.journal.push(j(
        126_500,
        "CONTINUED seq=2 rule=continue-done-check@v1 [aterm harness] x",
    ));
    assert!(!ask_due(&moved, at(127_000)));
    // no check typed yet
    let mut early = before;
    early.journal.truncate(2);
    assert!(!ask_due(&early, at(127_000)));
}

#[test]
fn the_persons_prompt_has_its_own_instant_reply() {
    let ask = Chain::InstantCruise
        .asks()
        .expect("the instant cruise chain asks");
    assert_eq!(
        reply(Chain::InstantCruise, &[DONE_CHECK, &ask]),
        Some(ASK_REPLY.to_string())
    );
    // the turn after it is the chain's own again
    assert_eq!(
        reply(Chain::InstantCruise, &[&ask, DONE_CHECK]),
        reply(Chain::InstantCruise, &[DONE_CHECK])
    );
    for c in Chain::ALL {
        assert_eq!(
            c.asks().is_some(),
            c == Chain::InstantCruise,
            "{}",
            c.name()
        );
    }
}

/// P3'S REPORT: a tab whose first point record follows its first prompt
/// reports it, the first back-off at 2 min, and no point record before the
/// prompt. NEGATIVE CONTROL: the records of Gemini CLI's `land-b` in the
/// forever lab's `wf8b` pass — a `point@v1` 5.7 s before the first prompt —
/// name that record, and the first reply's back-off at 4 min after it.
#[test]
fn a_point_before_the_first_prompt_is_reported_and_none_after_it() {
    let clean = Seen {
        journal: vec![
            j(-7_000, "EVENT idle seq=480 -"),
            j(5_600, "EVENT idle seq=1592 Implemented the parser"),
            j(
                5_604,
                "WAITING seq=1592 until=2026-10-20T15:56:11Z 1 short turn(s) in a row: 2 min back-off",
            ),
        ],
        ledger: vec![
            row(
                4_000,
                "turn-ran@v1",
                "waiting",
                "",
                "a turn was read running",
            ),
            point(5_600, 1),
        ],
        ..Seen::default()
    };
    assert!(points_before_prompt(&clean, T0).is_empty());
    assert_eq!(
        first_point_line(&clean, T0),
        "first point +5.6 s (short turns in a row: 1), first back-off +5.6 s \
         \"1 short turn(s) in a row: 2 min back-off\", 0 point record(s) before the first prompt"
    );
    // NEGATIVE CONTROL: the phantom.
    let mut phantom = clean.clone();
    phantom.ledger.insert(0, point(-5_700, 1));
    phantom.ledger[2] = point(5_600, 2);
    phantom.journal.insert(
        1,
        j(
            -5_696,
            "WAITING seq=480 until=2026-10-20T15:55:00Z 1 short turn(s) in a row: 2 min back-off",
        ),
    );
    phantom.journal[3] = j(
        5_604,
        "WAITING seq=1592 until=2026-10-20T15:56:11Z 2 short turn(s) in a row: 4 min back-off",
    );
    assert_eq!(points_before_prompt(&phantom, T0).len(), 1);
    assert_eq!(
        first_point_line(&phantom, T0),
        "first point -5.7 s (short turns in a row: 1), first back-off -5.7 s \
         \"1 short turn(s) in a row: 2 min back-off\", 1 point record(s) before the first prompt: \
         -5.7 s (1)"
    );
    // No records at all (Off's chain: no loop): nothing to report.
    assert_eq!(
        first_point_line(&Seen::default(), T0),
        "no point, no back-off, 0 point record(s) before the first prompt"
    );
}

// ---- the coverage chains' judges

/// The stub's witness of a window counts what falls inside it alone.
#[test]
fn the_window_witness_counts_inside_its_window() {
    let dumps = [(100, 0), (200, 1), (300, 1), (400, 2), (500, 3)];
    // CAP's hour: from the act (the conversation then holds its one text) to the turn
    assert_eq!(
        StubSeen::within(&dumps, 150, 350, 1),
        StubSeen {
            baseline: 1,
            read: 2,
            new_turns: 0
        }
    );
    assert_eq!(StubSeen::within(&dumps, 150, 450, 1).new_turns, 1);
    // L13's exchange: any harness text before the orchestrator's turn
    assert_eq!(StubSeen::within(&dumps, 0, 150, 0).new_turns, 0);
    assert_eq!(StubSeen::within(&dumps, 0, 250, 0).new_turns, 1);
    // Land's answer carries no mark, and counts all the same
    let tok = Chain::LandQ.token();
    assert_eq!(
        marked_turns(&messages(&tok, &[LAND, LAND_ANSWER]), &tok),
        Some(2)
    );
    assert!(new_harness_turn(
        &messages(&tok, &[LAND, LAND_ANSWER, LAND_ANSWER]),
        &tok,
        2
    ));
    assert!(!new_harness_turn(
        &messages(&tok, &[LAND, LAND_ANSWER, PERSON_REPLY]),
        &tok,
        2
    ));
}

/// L12 as a live run that passes it records it: land@v1 at +125 s, the
/// question read, Land's answer after the 4 min step, its clean defense
/// landing the task at +375 s, watched past the next step (8 min) and a
/// minute more.
fn answered() -> Seen {
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
            // the live shape (Claude Code 2.1.304): the point read as a question
            j(
                130_000,
                "EVENT question seq=2 ⏺ Should the parser also accept tab-indented input?",
            ),
            j(
                130_100,
                "WAITING seq=2 until=x 2 short turn(s) in a row: 4 min back-off",
            ),
            j(
                370_000,
                "CONTINUED seq=2 rule=land-answer@v1 Nobody is here to choose: take the option",
            ),
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
            typed(370_000, "land-answer@v1", LAND_ANSWER),
            point(375_000, 3),
            task_done(375_000),
        ],
        status: vec![
            status(1_000, "open", "land"),
            status(376_000, "landed", "land"),
        ],
        meta: vec![no_meta(1_000)],
        task: vec![j(1_000, "OK task -")],
        end_ms: T0 + 920_000,
        stub: Some(StubSeen {
            baseline: 2,
            read: 3,
            new_turns: 0,
        }),
        ..Seen::default()
    }
}

#[test]
fn land_q_passes_on_lands_answer_and_the_landing_at_its_yield() {
    let l12 = |s: &Seen| one(Chain::LandQ, s, "claude", Scenario::L12);
    let (w, why) = l12(&answered());
    assert_eq!(w, "PASS", "{why}");
    assert!(why.contains("EVENT question seq=2"), "{why}");
    // the same point journaled idle reads the question too
    let mut s = answered();
    s.journal[3].1 = "EVENT idle seq=2 Should the parser also accept tab-indented input?".into();
    assert_eq!(l12(&s).0, "PASS", "{}", l12(&s).1);
    // NEGATIVE CONTROLS. The question nudged as a defense would be:
    let mut s = answered();
    s.journal[5].1 = "CONTINUED seq=2 rule=land-nudge@v1 [aterm harness] Still landing:".into();
    s.ledger[3] = typed(370_000, "land-nudge@v1", LAND_NUDGE);
    assert_eq!(l12(&s).0, "FAIL", "{}", l12(&s).1);
    // continued as Cruise would be
    let mut s = answered();
    s.journal[5].1 = "CONTINUED seq=2 rule=continue@v1 [aterm harness] x".into();
    assert_eq!(l12(&s).0, "FAIL");
    // the question LANDED (no answer first)
    let mut s = answered();
    s.journal.insert(4, j(130_050, "DONE seq=2 rule=land@v1 x"));
    assert_eq!(l12(&s).0, "FAIL");
    let mut s = answered();
    s.status.insert(1, status(131_000, "landed", "land"));
    assert_eq!(l12(&s).0, "FAIL");
    // the answer journaled with no ledger row, or not Land's words
    let mut s = answered();
    s.ledger.remove(3);
    assert_eq!(l12(&s).0, "FAIL");
    let mut s = answered();
    s.ledger[3].command = "Nobody is here to answer. Decide for yourself".into();
    assert_eq!(l12(&s).0, "FAIL");
    // a suffix where nothing stood open
    let mut s = answered();
    s.ledger[3].command = format!("{LAND_ANSWER} {}", open_suffix());
    assert_eq!(l12(&s).0, "FAIL");
    let mut s = answered();
    s.ledger[3].command = format!("{LAND_ANSWER} {NEEDS_ACK}");
    assert_eq!(l12(&s).0, "FAIL");
    // the question never read before the answer, or another point's words read
    let mut s = answered();
    s.journal.remove(3);
    assert_eq!(l12(&s).0, "FAIL");
    let mut s = answered();
    s.journal[3].1 = "EVENT question seq=2 Which branch should I use?".into();
    let (w, why) = l12(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("no EVENT line read the question"), "{why}");
    // the answer's clean defense did not land it
    let mut s = answered();
    s.journal.pop();
    assert_eq!(l12(&s).0, "FAIL");
    // landed at another act's yield
    let mut s = answered();
    s.journal.insert(
        6,
        j(
            372_000,
            "CONTINUED seq=2 rule=land-nudge@v1 [aterm harness] x",
        ),
    );
    assert_eq!(l12(&s).0, "FAIL");
    // typed after the landing, or the ladder went on
    let mut s = answered();
    s.journal
        .push(j(500_000, "CONTINUED seq=4 rule=land-nudge@v1 x"));
    assert_eq!(l12(&s).0, "FAIL");
    let mut s = answered();
    s.stub = Some(StubSeen {
        baseline: 2,
        read: 4,
        new_turns: 1,
    });
    assert_eq!(l12(&s).0, "FAIL");
    // a need raised where none was named
    let mut s = answered();
    s.meta.push(needs_meta(131_000, "claude"));
    assert_eq!(l12(&s).0, "FAIL");
    // unobserved: nothing typed for the question yet, or watched under the window
    let mut s = answered();
    s.journal.truncate(5);
    s.ledger.truncate(3);
    s.status.truncate(1);
    s.end_ms = T0 + 300_000;
    assert_eq!(l12(&s).0, "UNOBSERVED");
    let mut s = answered();
    s.end_ms = T0 + 500_000;
    assert_eq!(l12(&s).0, "UNOBSERVED");
    // no land act at all
    assert_eq!(
        l12(&Seen {
            end_ms: T0 + 900_000,
            ..Seen::default()
        })
        .0,
        "FAIL"
    );
}

/// L13 as a live run that passes it records it: the person's Enter at +10 s,
/// their answer left to them (LEFT, task=theirs; Claude Code's skipped row),
/// nothing typed to the orchestrator's turn at +310 s, then Land's act after
/// its 2 min step and the landing.
fn left_to_them() -> Seen {
    Seen {
        journal: vec![
            j(
                14_000,
                "EVENT idle seq=1 The repository holds a README and nothing else yet.",
            ),
            j(
                14_000,
                "LEFT seq=1 rule=left-to-person@v1 a person's own exchange (their message, answered within 10 min): nothing is typed until the next turn ends",
            ),
            j(314_000, "EVENT idle seq=2 Implemented the parser"),
            j(
                314_100,
                "WAITING seq=2 until=x 1 short turn(s) in a row: 2 min back-off",
            ),
            j(
                434_000,
                "CONTINUED seq=2 rule=land@v1 [aterm harness] Autopilot is Land:",
            ),
            j(437_000, "EVENT idle seq=3 Nothing left: the parser"),
            j(437_000, "DONE seq=3 rule=land@v1 x"),
        ],
        ledger: vec![
            point(14_000, 0),
            row(14_000, RULE_LEFT, "skipped", "", LEFT_REASON),
            point(314_000, 1),
            typed(434_000, "land@v1", LAND),
            task_done(437_000),
        ],
        status: vec![
            status(1_000, "open", "land"),
            status(15_000, "theirs", "land"),
            status(316_000, "open", "land"),
            status(438_000, "landed", "land"),
        ],
        meta: vec![no_meta(1_000)],
        task: vec![j(1_000, "OK task -")],
        end_ms: T0 + 460_000,
        cover: Cover {
            person_ms: Some(T0 + 10_000),
            person_stamp: Some("1200".to_string()),
            turn_ms: Some(T0 + 310_000),
            window_stub: Some(StubSeen {
                baseline: 0,
                read: 2,
                new_turns: 0,
            }),
            ..Cover::default()
        },
        ..Seen::default()
    }
}

#[test]
fn land_p_passes_only_on_a_persons_exchange_left_to_them() {
    let l13 = |s: &Seen| one(Chain::LandP, s, "claude", Scenario::L13);
    let (w, why) = l13(&left_to_them());
    assert_eq!(w, "PASS", "{why}");
    assert!(why.contains("land@v1 typed at +434.0 s"), "{why}");
    // NEGATIVE CONTROLS. The person's turn taken as the policy's: Land's act
    // after the 2 min step, inside the exchange
    let mut s = left_to_them();
    s.journal.insert(
        2,
        j(
            134_000,
            "CONTINUED seq=1 rule=land@v1 [aterm harness] Autopilot is Land:",
        ),
    );
    let (w, why) = l13(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("TYPED in the person's exchange"), "{why}");
    let mut s = left_to_them();
    s.ledger.insert(2, typed(134_000, "land@v1", LAND));
    assert_eq!(l13(&s).0, "FAIL");
    // no LEFT, or the face never theirs
    let mut s = left_to_them();
    s.journal.remove(1);
    assert_eq!(l13(&s).0, "FAIL");
    let mut s = left_to_them();
    s.status.remove(1);
    assert_eq!(l13(&s).0, "FAIL");
    // the person's exchange landed
    let mut s = left_to_them();
    s.journal.insert(2, j(14_100, "DONE seq=1 rule=land@v1 x"));
    assert_eq!(l13(&s).0, "FAIL");
    // the stub saw a harness text in the exchange
    let mut s = left_to_them();
    s.cover.window_stub = Some(StubSeen {
        baseline: 0,
        read: 3,
        new_turns: 1,
    });
    assert_eq!(l13(&s).0, "FAIL");
    // a need raised where none was named
    let mut s = left_to_them();
    s.meta.push(needs_meta(20_000, "claude"));
    assert_eq!(l13(&s).0, "FAIL");
    // unobserved: watched under 270 s, the stub unread, no act after the turn, no person
    let mut s = left_to_them();
    s.cover.turn_ms = Some(T0 + 200_000);
    assert_eq!(l13(&s).0, "UNOBSERVED");
    let mut s = left_to_them();
    s.cover.window_stub = None;
    assert_eq!(l13(&s).0, "UNOBSERVED");
    let mut s = left_to_them();
    s.journal.truncate(4);
    s.end_ms = T0 + 400_000;
    assert_eq!(l13(&s).0, "UNOBSERVED");
    // THE ORCHESTRATOR'S TURN NOT JUDGED (F8 of the review of
    // `b9/forever-gaps`, which passed any act after it): Land's act with no
    // point read at the turn's reply, or before the due its back-off named
    let mut s = left_to_them();
    s.journal.drain(2..4);
    let (w, why) = l13(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("at no point judged at its reply"), "{why}");
    let mut s = left_to_them();
    s.journal[4].0 = T0 + 330_000;
    s.ledger[3].ts = T0 + 330_000;
    let (w, why) = l13(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("BEFORE the due its back-off named"), "{why}");
    // nothing typed after the turn past its back-off's due and a minute: lost
    let mut s = left_to_them();
    s.journal.truncate(4);
    s.end_ms = T0 + 500_000;
    let (w, why) = l13(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("PERSISTENCE LOST"), "{why}");
    // P3 AS A VERDICT (F11, P3 was reported only): a point judged between the
    // draft's typing and the person's Enter, or a first point after the
    // Enter that counted a short turn though it was left to the person
    let mut s = left_to_them();
    s.ledger.insert(0, point(5_000, 1));
    let (w, why) = l13(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("before the first prompt (P3)"), "{why}");
    let mut s = left_to_them();
    s.ledger[0] = point(14_000, 1);
    let (w, why) = l13(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("closed 1 short turn(s), not 0"), "{why}");
    let mut s = left_to_them();
    s.cover.person_ms = None;
    assert_eq!(l13(&s).0, "UNOBSERVED");
    // a prompt no person's hand stamped (typed over the control socket) is
    // nobody's: what the harness did after it judges no exchange, the act in
    // it included
    for stamp in [None, Some("-"), Some("?")] {
        let mut s = left_to_them();
        s.cover.person_stamp = stamp.map(str::to_string);
        s.journal.insert(
            2,
            j(
                134_000,
                "CONTINUED seq=1 rule=land@v1 [aterm harness] Autopilot is Land:",
            ),
        );
        let (w, why) = l13(&s);
        assert_eq!(w, "UNOBSERVED", "{stamp:?}: {why}");
        assert!(
            why.contains("no person's hand reached the session"),
            "{why}"
        );
    }
}

/// L14 is L1, L3 and L5 on replies opening `Wrote `, their words read.
fn wrote_landed() -> Seen {
    let mut s = landed_a();
    s.journal[0].1 = format!("EVENT idle seq=1 {W_WORK}");
    s
}

#[test]
fn land_w_passes_only_where_the_wrote_replies_are_read() {
    let l14 = |s: &Seen| one(Chain::LandW, s, "muse", Scenario::L14);
    let mut pass = wrote_landed();
    pass.meta[1] = needs_meta(131_000, "muse");
    let (w, why) = l14(&pass);
    assert_eq!(w, "PASS", "{why}");
    // NEGATIVE CONTROL, P2 itself: the head read as Muse Code's tool row, no words
    let mut s = pass.clone();
    s.journal[0].1 = "EVENT idle seq=1 -".into();
    let (w, why) = l14(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("not the reply opening"), "{why}");
    // …and so even where a later line read the words before the land act:
    // the first point JUDGED read none (F15 of the review of
    // `b9/forever-gaps`, which passed it)
    s.journal
        .insert(2, j(60_000, &format!("EVENT idle seq=7 {W_WORK}")));
    let (w, why) = l14(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("not the reply opening"), "{why}");
    // the need in a `Wrote …` reply never raised (its words unread)
    let mut s = pass.clone();
    s.journal.remove(3);
    s.meta.truncate(1);
    s.status.remove(1);
    assert_eq!(l14(&s).0, "FAIL");
    // the defense in one never landed it
    let mut s = pass.clone();
    s.journal.pop();
    assert_eq!(l14(&s).0, "FAIL");
    // watched under the window after the landing
    let mut s = pass;
    s.end_ms = T0 + 470_000;
    assert_eq!(l14(&s).0, "UNOBSERVED");
}

/// C3 is C1 and C2 on a done claim and a defense opening `Wrote `.
fn wrote_cruised() -> Seen {
    let mut s = cruised();
    s.journal[0].1 = format!("EVENT idle seq=1 {W_DONE}");
    s.journal[3].1 = format!("EVENT idle seq=2 {}", W_CHECKED.replace('\n', " "));
    s
}

#[test]
fn wrote_c_passes_only_where_the_done_claim_is_read() {
    let c3 = |s: &Seen| one(Chain::WroteC, s, "muse", Scenario::C3);
    let (w, why) = c3(&wrote_cruised());
    assert_eq!(w, "PASS", "{why}");
    // NEGATIVE CONTROLS: the claim read as a tool row (no words), and so
    // continued as a plain turn; the check's defense unread
    let mut s = wrote_cruised();
    s.journal[0].1 = "EVENT idle seq=1 -".into();
    assert_eq!(c3(&s).0, "FAIL");
    let mut s = wrote_cruised();
    s.journal[2].1 = "CONTINUED seq=1 rule=continue@v1 [aterm harness] x".into();
    assert_eq!(c3(&s).0, "FAIL");
    let mut s = wrote_cruised();
    s.journal[3].1 = "EVENT idle seq=2 -".into();
    assert_eq!(c3(&s).0, "UNOBSERVED");
}

/// P5 as a live run that passes it records it: the record `working` from
/// before the agent started to age 152 s, then landed.
fn started() -> Seen {
    let rec = |age: i64| {
        format!(
            "OK task rev=1 state=working from=ctl since_ms={age} result_bytes=0 agent=idle attention=-"
        )
    };
    Seen {
        task: vec![
            j(-2_000, &rec(0)),
            j(60_000, &rec(62_000)),
            j(150_000, &rec(152_000)),
            j(
                400_000,
                "OK task rev=2 state=done from=ctl since_ms=402000 result_bytes=120 why=landed agent=idle attention=-",
            ),
        ],
        status: vec![status(1_000, "open", "land")],
        meta: vec![no_meta(1_000)],
        end_ms: T0 + 420_000,
        ..Seen::default()
    }
}

#[test]
fn land_t_passes_on_a_record_working_past_its_startup_deadline() {
    let p5 = |s: &Seen| one(Chain::LandT, s, "gemini", Scenario::P5);
    let (w, why) = p5(&started());
    assert_eq!(w, "PASS", "{why}");
    assert!(why.contains("then done why=landed"), "{why}");
    // NEGATIVE CONTROL, P5 itself (wf8-agy): stalled why=startup at age 111 s
    let mut s = started();
    s.task.insert(
        2,
        j(
            109_000,
            "OK task rev=2 state=stalled from=ctl since_ms=111000 result_bytes=0 why=startup agent=idle attention=the%20worker%20never%20started",
        ),
    );
    let (w, why) = p5(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("why=startup"), "{why}");
    // its other faces: the stall's need, the never-started attention
    let mut s = started();
    s.status.push(j(
        110_000,
        "task=open needs=stalled autopilot=land supervisor=aterm-harness@1 agent=idle",
    ));
    assert_eq!(p5(&s).0, "FAIL");
    let mut s = started();
    s.meta.push(j(
        110_000,
        "attention_owners=1 attention_kinds=ask attention=the worker never started",
    ));
    assert_eq!(p5(&s).0, "FAIL");
    // A STALL RAISED AND LIFTED BETWEEN TWO LOOKS (F13 of the review of
    // `b9/forever-gaps`, which passed it): the record reads working at each
    // look, but its rev moved by two
    let mut s = started();
    s.task[2].1 = s.task[2].1.replace("rev=1", "rev=3");
    s.task[3].1 = s.task[3].1.replace("rev=2", "rev=4");
    let (w, why) = p5(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("changed between looks unread: rev 1 then 3"),
        "{why}"
    );
    // unobserved: never read past the deadline and 30 s, or never working
    let mut s = started();
    s.task.truncate(2);
    assert_eq!(p5(&s).0, "UNOBSERVED");
    let mut s = started();
    s.task = vec![j(-2_000, "OK task -")];
    assert_eq!(p5(&s).0, "UNOBSERVED");
}

/// L15 as a live run that passes it records it: L7's landing on a worker
/// the manager `@s-mgr` spawned, its wait returning at the landing, the
/// result as stubbed.
fn spawned() -> Seen {
    let mut s = tasked();
    s.task[0].1 =
        "OK task rev=1 state=working from=s-mgr since_ms=0 result_bytes=0 agent=- attention=-"
            .into();
    s.task[1].1 = s.task[1].1.replace("from=ctl", "from=s-mgr");
    s.cover = Cover {
        starter: Some("s-mgr".into()),
        spawn: Some("OK s-w1".into()),
        awaited: Some((
            T0 + 9_500,
            0,
            "OK 1\n@s-w1 rev=2 state=done agent=idle attention=- result_bytes=120\n".into(),
        )),
        result: Some(format!("{WORK}\n\n{CLEAN}\n")),
        ..Cover::default()
    };
    s
}

#[test]
fn worker_passes_on_a_spawned_workers_landing_its_managers_wait_and_result() {
    let l15 = |s: &Seen| one(Chain::Worker, s, "claude", Scenario::L15);
    let (w, why) = l15(&spawned());
    assert_eq!(w, "PASS", "{why}");
    // NEGATIVE CONTROLS. The record not from its manager
    let mut s = spawned();
    s.task[0].1 = s.task[0].1.replace("from=s-mgr", "from=ctl");
    assert_eq!(l15(&s).0, "FAIL");
    // not under Land
    let mut s = spawned();
    s.status[0] = status(1_000, "open", "cruise");
    assert_eq!(l15(&s).0, "FAIL");
    // the manager's wait returned before the landing (an attention, a stall)
    let mut s = spawned();
    s.cover.awaited = Some((
        T0 + 3_000,
        0,
        "OK 1\n@s-w1 rev=2 state=stalled agent=idle attention=x result_bytes=0\n".into(),
    ));
    assert_eq!(l15(&s).0, "FAIL");
    // …and a wait that returned `state=done` 6 s before the landing fails by
    // its time alone (F17 of the review of `b9/forever-gaps`: the control
    // above fails on its `state=stalled` too)
    let mut s = spawned();
    s.cover.awaited = Some((
        T0 + 3_000,
        0,
        "OK 1\n@s-w1 rev=2 state=done agent=idle attention=- result_bytes=120\n".into(),
    ));
    let (w, why) = l15(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("before the landing"), "{why}");
    assert!(!why.contains("not state=done"), "{why}");
    let mut s = spawned();
    if let Some(a) = s.cover.awaited.as_mut() {
        a.2 = a.2.replace("state=done", "state=working");
    }
    assert_eq!(l15(&s).0, "FAIL");
    let mut s = spawned();
    if let Some(a) = s.cover.awaited.as_mut() {
        a.1 = 124;
    }
    assert_eq!(l15(&s).0, "FAIL");
    let mut s = spawned();
    s.cover.awaited = None;
    assert_eq!(l15(&s).0, "FAIL");
    // the result not the report and the defense
    let mut s = spawned();
    s.cover.result = Some(format!("{CLEAN}\n"));
    assert_eq!(l15(&s).0, "FAIL");
    // the landing L7 needs: no TASK line
    let mut s = spawned();
    s.journal.pop();
    assert_eq!(l15(&s).0, "FAIL");
    // no worker spawned
    let mut s = spawned();
    s.cover.spawn = None;
    assert_eq!(l15(&s).0, "UNOBSERVED");
}

/// CAP as a live run that passes it records it: land@v1 at +125 s, the next
/// point escalated at +370 s naming the cap, nothing typed for the hour, the
/// orchestrator's turn 30 s past it, and the nudge after its 8 min step.
fn capped() -> Seen {
    let turn = 125_000 + 3_600_000 + 30_000;
    Seen {
        journal: vec![
            j(5_000, "EVENT idle seq=1 Implemented the parser"),
            j(
                5_100,
                "WAITING seq=1 until=x 1 short turn(s) in a row: 2 min back-off",
            ),
            j(
                125_000,
                "CONTINUED seq=1 rule=land@v1 [aterm harness] Autopilot is Land:",
            ),
            j(130_000, "EVENT idle seq=2 The docs PR is still open."),
            j(
                130_100,
                "WAITING seq=2 until=x 2 short turn(s) in a row: 4 min back-off",
            ),
            j(370_000, "ESCALATED seq=2 attention=OK"),
            j(turn + 4_000, "EVENT idle seq=3 The docs PR is still open."),
            j(
                turn + 4_100,
                "WAITING seq=3 until=x 3 short turn(s) in a row: 8 min back-off",
            ),
            j(
                turn + 484_000,
                "CONTINUED seq=3 rule=land-nudge@v1 [aterm harness] Still landing:",
            ),
        ],
        ledger: vec![
            typed(125_000, "land@v1", LAND),
            typed(turn + 484_000, "land-nudge@v1", LAND_NUDGE),
        ],
        // the live shape (Claude Code 2.1.304): the escalation's own face is
        // `needs=ask`, its entry the supervisor's
        status: vec![
            status(1_000, "open", "land"),
            status_of(371_000, "open", "ask", "land", "aterm-harness@1"),
        ],
        meta: vec![
            no_meta(1_000),
            j(
                371_000,
                "attention_owners=1 attention_kinds=-:supervisor attention=claude idle: The docs PR is still open. (continue_per_hour = 1 reached: 1 typed in the last 60 min)",
            ),
        ],
        end_ms: T0 + turn + 545_000,
        cover: Cover {
            turn_ms: Some(T0 + turn),
            window_stub: Some(StubSeen {
                baseline: 1,
                read: 3,
                new_turns: 0,
            }),
            ..Cover::default()
        },
        ..Seen::default()
    }
}

#[test]
fn cap_passes_on_acts_stopped_for_the_hour_and_resumed_at_a_new_point() {
    let cap = |s: &Seen| one(Chain::Cap, s, "claude", Scenario::Cap);
    let turn = 125_000 + 3_600_000 + 30_000;
    let (w, why) = cap(&capped());
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains("resumed: its reply read at +3759.0 s"),
        "{why}"
    );
    assert!(why.contains("land-nudge@v1 typed at +4239.0 s"), "{why}");
    // NEGATIVE CONTROLS. No cap: the nudge typed at the 4 min step
    let mut s = capped();
    s.journal[5].1 = "CONTINUED seq=2 rule=land-nudge@v1 [aterm harness] Still landing:".into();
    assert_eq!(cap(&s).0, "FAIL");
    let mut s = capped();
    s.ledger
        .insert(1, typed(370_000, "land-nudge@v1", LAND_NUDGE));
    assert_eq!(cap(&s).0, "FAIL");
    // the act typed again inside the hour after the escalation (a timer re-deciding it)
    let mut s = capped();
    s.journal.insert(
        6,
        j(
            2_000_000,
            "CONTINUED seq=2 rule=land-nudge@v1 [aterm harness] Still landing:",
        ),
    );
    assert_eq!(cap(&s).0, "FAIL");
    // the capped point never escalated, or its attention not the cap's
    let mut s = capped();
    s.journal.remove(5);
    assert_eq!(cap(&s).0, "FAIL");
    let mut s = capped();
    s.meta[1].1 = "attention_owners=1 attention_kinds=ask attention=claude: idle (a box)".into();
    assert_eq!(cap(&s).0, "FAIL");
    // a NEEDS YOU need raised where no reply named one (the escalation's own
    // `needs=ask` is the cap's, and passes above)
    let mut s = capped();
    s.journal
        .insert(6, j(372_000, "NEEDS seq=2 count=1 attention=OK x"));
    assert_eq!(cap(&s).0, "FAIL");
    let mut s = capped();
    s.meta.push(needs_meta(372_000, "claude"));
    assert_eq!(cap(&s).0, "FAIL");
    // the stub saw a harness text inside the hour
    let mut s = capped();
    s.cover.window_stub = Some(StubSeen {
        baseline: 1,
        read: 4,
        new_turns: 1,
    });
    assert_eq!(cap(&s).0, "FAIL");
    // the act after the turn journaled with no ledger row
    let mut s = capped();
    s.ledger.pop();
    assert_eq!(cap(&s).0, "FAIL");
    // the hour freed, and the escalated point decided again on a timer: an
    // act before the orchestrator's turn, or with no new point at all
    let window_end = 125_000 + 3_600_000;
    let mut s = capped();
    s.journal.insert(
        6,
        j(
            window_end + 10_000,
            "CONTINUED seq=2 rule=land-nudge@v1 [aterm harness] Still landing:",
        ),
    );
    s.ledger
        .insert(1, typed(window_end + 10_000, "land-nudge@v1", LAND_NUDGE));
    let (w, why) = cap(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("before any new point"), "{why}");
    let mut s = capped();
    s.cover.turn_ms = None;
    assert_eq!(cap(&s).0, "FAIL");
    // THE ORCHESTRATOR'S TURN NOT JUDGED (F8 of the review of
    // `b9/forever-gaps`, where any act after it passed): an act after the
    // turn with no point read at its reply, or none decided there, or one
    // typed before the due its back-off named
    let mut s = capped();
    s.journal.drain(6..8);
    let (w, why) = cap(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("at no point judged at its reply"), "{why}");
    let mut s = capped();
    s.journal.remove(7);
    let (w, why) = cap(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("NO back-off decided at that point"), "{why}");
    let mut s = capped();
    s.journal[8].0 = T0 + turn + 60_000;
    s.ledger[1].ts = T0 + turn + 60_000;
    let (w, why) = cap(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("BEFORE the due its back-off named"), "{why}");
    // NO ACT AFTER THE TURN past its back-off's due (8 min) and a minute
    // more: PERSISTENCE LOST (F9; it read UNOBSERVED)
    let mut s = capped();
    s.journal.pop();
    s.ledger.pop();
    let (w, why) = cap(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("PERSISTENCE LOST"), "{why}");
    // unobserved: no act after the turn yet, no turn, the turn inside the hour, watched under it
    let mut s = capped();
    s.journal.pop();
    s.ledger.pop();
    s.end_ms = T0 + turn + 500_000;
    assert_eq!(cap(&s).0, "UNOBSERVED");
    let mut s = capped();
    s.journal.pop();
    s.ledger.pop();
    s.cover.turn_ms = None;
    assert_eq!(cap(&s).0, "UNOBSERVED");
    let mut s = capped();
    s.cover.turn_ms = Some(T0 + 3_000_000);
    assert_eq!(cap(&s).0, "UNOBSERVED");
    let mut s = capped();
    s.journal.truncate(6);
    s.ledger.truncate(1);
    s.cover.turn_ms = None;
    s.end_ms = T0 + 2_000_000;
    assert_eq!(cap(&s).0, "UNOBSERVED");
    // no act at all
    assert_eq!(
        cap(&Seen {
            end_ms: T0 + 900_000,
            ..Seen::default()
        })
        .0,
        "FAIL"
    );
}

/// The lab's drive of the coverage chains: each orchestrator turn once, at
/// its time; CAP watched to an act in the hour or a minute past the act after
/// the turn.
#[test]
fn the_orchestrator_turns_come_once_at_their_time_and_cap_is_watched_to_its_verdict() {
    let mut s = left_to_them();
    s.cover.turn_ms = None;
    let p = T0 + 10_000;
    assert!(!orchestrator_due(
        Chain::LandP,
        &s,
        p + EXCHANGE_WATCH_MS - 1
    ));
    assert!(orchestrator_due(Chain::LandP, &s, p + EXCHANGE_WATCH_MS));
    s.cover.turn_ms = Some(p + EXCHANGE_WATCH_MS);
    assert!(!orchestrator_due(
        Chain::LandP,
        &s,
        p + 2 * EXCHANGE_WATCH_MS
    ));
    assert!(!orchestrator_due(Chain::LandQ, &answered(), T0 + 9_999_999));
    let mut c = capped();
    c.cover.turn_ms = None;
    let hour = T0 + 125_000 + cap_window_ms();
    assert!(!orchestrator_due(Chain::Cap, &c, hour));
    assert!(orchestrator_due(Chain::Cap, &c, hour + PAST_WINDOW_MS));
    // CAP: watched on through the hour, done a minute after the act after the turn
    let mut early = capped();
    early.journal.truncate(6);
    early.cover.turn_ms = None;
    assert!(!cover::cap_watched(&early, T0 + 2_000_000));
    let done = capped();
    let resumed = T0 + 125_000 + 3_600_000 + 30_000 + 484_000;
    assert!(!cover::cap_watched(&done, resumed + 59_000));
    assert!(cover::cap_watched(&done, resumed + 60_000));
    // an act inside the hour ends the watch: the verdict is in
    let mut broken = early;
    broken.journal.push(j(
        900_000,
        "CONTINUED seq=2 rule=land-nudge@v1 [aterm harness] x",
    ));
    assert!(cover::cap_watched(&broken, T0 + 900_000));
}

/// W1's records: the wall raised at the first point (+6 s), land@v1 meeting
/// it, the limited point read and waited out, the resume typed past the
/// reset (+300 s) and its grace, and the clean defense landing it.
/// The record of a wall's track ending (`wall-wait-end@v1`).
const WALL_END: &str = "wall-wait-end@v1";

fn walled() -> Seen {
    Seen {
        journal: vec![
            j(5_000, "EVENT idle seq=1 Implemented the parser"),
            j(
                5_100,
                "WAITING seq=1 until=x 1 short turn(s) in a row: 2 min back-off",
            ),
            j(
                125_000,
                "CONTINUED seq=1 rule=land@v1 [aterm harness] Autopilot is Land:",
            ),
            j(
                140_000,
                "EVENT limited seq=2 message=◆ Usage limit reached · /upgrade (x) for increased limits, or wait for usage to reset at 1:06 PM reset=1:06 PM",
            ),
            j(
                140_100,
                &format!(
                    "WAITING seq=2 until={} usage-session resets",
                    stamp(300_000)
                ),
            ),
            j(
                361_000,
                "CONTINUED seq=2 rule=usage-resume@v1 [aterm harness] Still landing:",
            ),
            j(365_000, "EVENT idle seq=3 Nothing left: the parser"),
            j(365_000, "DONE seq=3 rule=land@v1 x"),
        ],
        // the live shape (`wall1-muse`): the wall's track ends twice, the
        // second time 4 ms ahead of the landing's own point, at its read
        ledger: vec![
            point(5_000, 1),
            typed(125_000, "land@v1", LAND),
            row(
                140_000,
                "wall-wait@v1",
                "waiting",
                "",
                "the turn-end policy (wall: x)",
            ),
            point(140_000, 2),
            typed(361_000, "usage-resume@v1", LAND_NUDGE),
            row(362_500, WALL_END, "waiting", "", "the wall left the screen"),
            row(364_996, WALL_END, "waiting", "", "the limit's track ended"),
            point(365_000, 3),
            task_done(365_000),
        ],
        status: vec![
            status(1_000, "open", "land"),
            status(366_000, "landed", "land"),
        ],
        meta: vec![no_meta(1_000)],
        task: vec![j(1_000, "OK task -")],
        end_ms: T0 + 365_000 + 545_000,
        stub: Some(StubSeen {
            baseline: 2,
            read: 0,
            new_turns: 0,
        }),
        cover: Cover {
            wall: Some((T0 + 6_000, (T0 + 300_000) / 1000)),
            lifted_ms: Some(T0 + 297_000),
            window_stub: Some(StubSeen {
                baseline: 1,
                read: 3,
                new_turns: 0,
            }),
            ..Cover::default()
        },
        ..Seen::default()
    }
}

#[test]
fn wall_passes_on_nothing_typed_into_it_gone_on_past_it_and_landed() {
    let w1 = |s: &Seen| one(Chain::Wall, s, "muse", Scenario::W1);
    let (w, why) = w1(&walled());
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains("gone on: usage-resume@v1 typed at +361.0 s"),
        "{why}"
    );
    // THE PROGRAM'S OWN RETRY landed it past the reset: nothing of the
    // harness's going on shows (F14 of the review of `b9/forever-gaps`, it
    // passed)
    let mut s = walled();
    s.journal.remove(5);
    s.ledger.remove(4);
    let (w, why) = w1(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("the program's own retry landed it"), "{why}");
    // …and so from the lab's lift (3 s before the reset), where the stub
    // answers again: no landing while the wall stood (F14, a false FAIL)
    let mut s = walled();
    s.journal.remove(5);
    s.ledger.remove(4);
    for (t, l) in &mut s.journal {
        if l.starts_with("EVENT idle seq=3") || l.starts_with("DONE ") {
            *t = T0 + 298_000;
        }
    }
    s.status[1].0 = T0 + 299_000;
    let (w, why) = w1(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(!why.contains("while the wall stood"), "{why}");
    // NEGATIVE CONTROLS. Typed into the wall (journal, or ledger alone)
    let mut s = walled();
    s.journal.insert(
        5,
        j(
            200_000,
            "CONTINUED seq=2 rule=land-nudge@v1 [aterm harness] Still landing:",
        ),
    );
    let (w, why) = w1(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("TYPED into the wall"), "{why}");
    let mut s = walled();
    s.ledger
        .insert(4, typed(200_000, "land-nudge@v1", LAND_NUDGE));
    assert_eq!(w1(&s).0, "FAIL");
    // the wall's point read as an idle one
    let mut s = walled();
    s.journal[3].1 =
        "EVENT idle seq=2 ◆ Usage limit reached · /upgrade (x) for increased limits".into();
    let (w, why) = w1(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("never read as one"), "{why}");
    // landed while the wall stood (the journal, or the status)
    let mut s = walled();
    s.journal.insert(4, j(140_050, "DONE seq=2 rule=land@v1 x"));
    assert_eq!(w1(&s).0, "FAIL");
    let mut s = walled();
    s.status.insert(1, status(150_000, "landed", "land"));
    assert_eq!(w1(&s).0, "FAIL");
    // the stub saw a harness text in the wall
    let mut s = walled();
    s.cover.window_stub = Some(StubSeen {
        baseline: 1,
        read: 4,
        new_turns: 1,
    });
    assert_eq!(w1(&s).0, "FAIL");
    // a need raised where none was named
    let mut s = walled();
    s.meta.push(needs_meta(150_000, "muse"));
    assert_eq!(w1(&s).0, "FAIL");
    // the act past the reset journaled with no ledger row
    let mut s = walled();
    s.ledger.remove(4);
    assert_eq!(w1(&s).0, "FAIL");
    // PERSISTENCE LOST: nothing goes on past the reset, or it never lands
    let lost = T0 + 300_000 + wall_grace_ms("muse") + WALL_LOST_MS;
    let mut s = walled();
    s.journal.truncate(5);
    s.ledger.truncate(4);
    s.status.truncate(1);
    s.end_ms = lost + 1_000;
    let (w, why) = w1(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("PERSISTENCE LOST"), "{why}");
    let mut s = walled();
    s.journal.truncate(6);
    s.ledger.truncate(5);
    s.status.truncate(1);
    s.end_ms = lost + 1_000;
    let (w, why) = w1(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("NO landing"), "{why}");
    // A later recovery cannot erase a missed persistence deadline. Keep the
    // observed order and the landing's quiet, but move the first resumed act
    // and its answer/landing past the reset's grace and lost window.
    let mut s = walled();
    let late = lost - (T0 + 361_000) + 1_000;
    for (t, _) in s.journal.iter_mut().skip(5) {
        *t += late;
    }
    for r in s.ledger.iter_mut().skip(4) {
        r.ts += late;
    }
    for (t, _) in s.status.iter_mut().skip(1) {
        *t += late;
    }
    s.end_ms += late;
    let (w, why) = w1(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("first act after the reset"), "{why}");
    // Even an on-time act must still land within that deadline.
    s.journal[5].0 = T0 + 361_000;
    s.ledger[4].ts = T0 + 361_000;
    let (w, why) = w1(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("the landing came"), "{why}");
    // The program's own retry (no act) has the same landing deadline.
    s.journal.remove(5);
    s.ledger.remove(4);
    let (w, why) = w1(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("the landing came"), "{why}");
    // The original on-time continuation and landing still pass.
    assert_eq!(w1(&walled()).0, "PASS");
    // typed after the landing (L5's quiet)
    let mut s = walled();
    s.journal
        .push(j(500_000, "CONTINUED seq=4 rule=land-nudge@v1 x"));
    assert_eq!(w1(&s).0, "FAIL");
    // the wall's track ended after the landing's own point, not at its read
    let mut s = walled();
    s.ledger
        .insert(8, row(365_002, WALL_END, "waiting", "", "x"));
    let (w, why) = w1(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("ledger rows after the landing's point"),
        "{why}"
    );
    // no land act at all
    let mut s = walled();
    s.journal.remove(2);
    assert_eq!(w1(&s).0, "FAIL");
    // UNOBSERVED: no wall raised; the land act before the raise or after the
    // reset; the wall never drawn; the stub unread; watched short of the loss
    let mut s = walled();
    s.cover.wall = None;
    assert_eq!(w1(&s).0, "UNOBSERVED");
    let mut s = walled();
    s.cover.wall = Some((T0 + 130_000, (T0 + 300_000) / 1000));
    assert_eq!(w1(&s).0, "UNOBSERVED");
    let mut s = walled();
    s.cover.wall = Some((T0 + 6_000, (T0 + 100_000) / 1000));
    assert_eq!(w1(&s).0, "UNOBSERVED");
    // (and nothing typed for it: an act after the land act with no point
    // between is an act typed twice, every chain's fault)
    let mut s = walled();
    s.journal.drain(3..6);
    s.ledger.drain(2..5);
    let (w, why) = w1(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("never drawn as a limit point"), "{why}");
    let mut s = walled();
    s.journal.drain(3..5);
    s.ledger.drain(2..4);
    let (w, why) = w1(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("acts typed with no point judged between"),
        "{why}"
    );
    let mut s = walled();
    s.cover.window_stub = None;
    assert_eq!(w1(&s).0, "UNOBSERVED");
    let mut s = walled();
    s.journal.truncate(5);
    s.ledger.truncate(4);
    s.status.truncate(1);
    s.end_ms = T0 + 400_000;
    assert_eq!(w1(&s).0, "UNOBSERVED");
}

/// AN ACT BEFORE THE DUE ITS BACK-OFF NAMED FAILS EVERY SCENARIO OF ITS
/// CHAIN (F10 of the review of `b9/forever-gaps`: no verdict compared an act
/// with its `WAITING`'s `until=`). `landed_a`'s land act is due at +125.0 s
/// (the 2 min back-off from +5.1 s, its stamp cut to the second): 1.9 s
/// early passes (the stamp's cut and the act's re-read, [`DUE_SLACK_MS`]),
/// 2.1 s early fails L1, L3 and L5, each of which passed it. NEGATIVE
/// CONTROLS: a point judged after the back-off (its own decision typed the
/// act), the take window's wait (no back-off), and a back-off whose stamp
/// does not read (a fault of its own).
#[test]
fn an_act_before_the_due_its_back_off_named_fails_every_scenario_of_its_chain() {
    let moved = |t: i64| {
        let mut s = landed_a();
        s.journal[2].0 = T0 + t;
        s.ledger[1].ts = T0 + t;
        s
    };
    assert_eq!(
        verdicts(Chain::LandA, &moved(123_100), "gemini"),
        [("L1", "PASS"), ("L3", "PASS"), ("L5", "PASS")]
    );
    let early = moved(122_900);
    assert_eq!(
        verdicts(Chain::LandA, &early, "gemini"),
        [("L1", "FAIL"), ("L3", "FAIL"), ("L5", "FAIL")]
    );
    let (_, why) = one(Chain::LandA, &early, "gemini", Scenario::L1);
    assert!(
        why.contains("+122.9 s land@v1 typed 2.1 s BEFORE the due"),
        "{why}"
    );
    assert!(why.contains("its own judge: PASS"), "{why}");
    assert_eq!(
        verdicts(Chain::LandA, &moved(60_000), "gemini")[0],
        ("L1", "FAIL")
    );
    // a nudge typed a minute into its 4 min back-off
    let mut s = landed_a();
    s.journal[6].0 = T0 + 190_000;
    s.ledger[3].ts = T0 + 190_000;
    let (w, why) = one(Chain::LandA, &s, "gemini", Scenario::L3);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("land-nudge@v1 typed"), "{why}");
    // NEGATIVE CONTROLS. A new point judged after the back-off, its act at once
    let mut s = moved(60_000);
    s.journal
        .insert(2, j(59_000, "EVENT idle seq=9 another turn ended"));
    assert_eq!(verdicts(Chain::LandA, &s, "gemini")[0], ("L1", "PASS"));
    // the take window's wait names no back-off
    let mut s = moved(60_000);
    s.journal[1].1 = "WAITING seq=1 until=x the worker to take the land@v1 act".into();
    assert_eq!(verdicts(Chain::LandA, &s, "gemini")[0], ("L1", "PASS"));
    // a back-off whose stamp does not read
    let mut s = landed_a();
    s.journal[1].1 = "WAITING seq=1 until=soon 1 short turn(s) in a row: 2 min back-off".into();
    let (w, why) = one(Chain::LandA, &s, "gemini", Scenario::L1);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("names no due that reads"), "{why}");
    // a wall's reset is a due too: W1's act a second into the wall
    let mut s = walled();
    s.journal[5].0 = T0 + 141_000;
    s.ledger[4].ts = T0 + 141_000;
    let (_, why) = one(Chain::Wall, &s, "muse", Scenario::W1);
    assert!(why.contains("BEFORE the due its back-off named"), "{why}");
}

/// AN ACT TYPED TWICE FAILS ANY CHAIN (F12 of the review of
/// `b9/forever-gaps`: only P4's chains asked): `land-b`'s nudge typed again
/// a second after itself, no point judged between, fails L2 (it passed).
/// NEGATIVE CONTROLS: a back-off between, a `LEFT` between.
#[test]
fn an_act_typed_twice_with_no_point_between_fails_any_chain() {
    let open = open_suffix();
    let mut s = nudged(&open);
    s.journal.insert(
        4,
        j(
            371_000,
            "CONTINUED seq=2 rule=land-nudge@v1 [aterm harness] Still landing:",
        ),
    );
    let (w, why) = one(Chain::LandB, &s, "claude", Scenario::L2);
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("acts typed with no point judged between"),
        "{why}"
    );
    assert!(why.contains("its own judge: PASS"), "{why}");
    // a LEFT between is a point judged
    let mut s = nudged(&open);
    s.journal
        .insert(4, j(370_500, "LEFT seq=3 rule=left-to-person@v1 x"));
    s.journal.insert(
        5,
        j(
            371_000,
            "CONTINUED seq=3 rule=land-nudge@v1 [aterm harness] Still landing:",
        ),
    );
    assert!(doubled_acts(&s, T0).is_empty());
    assert_eq!(doubled_acts(&nudged(&open), T0), Vec::<String>::new());
}

/// P3 IS A VERDICT (F11 of the review of `b9/forever-gaps`: it was reported
/// only): a point judged before the first prompt, or a first point that
/// closed two short turns, fails every scenario of the chain (each passed
/// it); the first point closing one passes, and so does one closing none
/// that was left to its person (L13's, above).
#[test]
fn a_point_before_the_first_prompt_or_a_late_ladder_fails_the_chain() {
    let mut s = landed_a();
    s.ledger.insert(0, point(-3_000, 1));
    assert_eq!(
        verdicts(Chain::LandA, &s, "gemini"),
        [("L1", "FAIL"), ("L3", "FAIL"), ("L5", "FAIL")]
    );
    let (_, why) = one(Chain::LandA, &s, "gemini", Scenario::L1);
    assert!(
        why.contains("a point judged 3.0 s before the first prompt (P3)"),
        "{why}"
    );
    let mut s = landed_a();
    s.ledger[0] = point(5_000, 2);
    let (w, why) = one(Chain::LandA, &s, "gemini", Scenario::L5);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("closed 2 short turn(s), not 1"), "{why}");
    assert!(chain_faults(&landed_a(), T0).is_empty());
}

/// THE INSTANT CRUISE CHAIN IS WATCHED UNTIL IT GOES ON (F5 of the review of
/// `b9/forever-gaps`: the watch ended 3 s after the person's point was
/// decided, so nothing after it was ever judged): past the decision, until
/// an act follows it (3 s after that act), or past its back-off's due and a
/// minute more.
#[test]
fn the_instant_cruise_tab_is_watched_until_the_chain_goes_on() {
    let tab = |seen: Seen| Tab {
        chain: Chain::InstantCruise,
        sid: "s-ic".into(),
        dir: PathBuf::new(),
        seen,
        t0: T0,
        ask_tried: true,
        over: false,
    };
    let mut stopped = instant_asked();
    stopped.journal.pop();
    stopped.ledger.pop();
    let t = tab(stopped);
    assert!(!finished(&t, T0 + 131_500), "3.5 s past the decision");
    assert!(!finished(&t, T0 + 600_000), "inside its back-off");
    assert!(
        !finished(&t, T0 + 608_000 + 60_000),
        "at its due and a minute"
    );
    assert!(finished(&t, T0 + 608_000 + 66_000));
    let t = tab(instant_asked());
    assert!(!finished(&t, T0 + 610_000), "1.5 s past the act");
    assert!(finished(&t, T0 + 612_000));
}

/// W1's records with everything from the resume on moved `by` ms later: the
/// first act past the reset and the landing after it.
fn walled_late(by: i64) -> Seen {
    let mut s = walled();
    for (t, _) in s.journal.iter_mut().skip(5) {
        *t += by;
    }
    for r in s.ledger.iter_mut().skip(4) {
        r.ts += by;
    }
    for (t, _) in s.status.iter_mut().skip(1) {
        *t += by;
    }
    s.end_ms += by;
    s
}

/// W1 holds each harness to its OWN program's grace past the reset
/// ([`wall_grace_ms`]): a resume 400 s past the reset is in time on Claude
/// Code (150 s grace: its own continue comes 30 to 90 s past the reset), and
/// persistence lost on the programs with no auto-continue (60 s). The
/// records hold no continuation of Claude Code's own ([`Cover::went_on_ms`]
/// `None`: its countdown cancelled): with one before the resume, the resume
/// is typed on top of it ([`an_act_after_the_programs_own_continuation_fails_w1`]).
#[test]
fn the_walls_deadline_is_the_programs_own_grace() {
    let reset = T0 + 300_000;
    let late = walled_late(reset + 400_000 - (T0 + 361_000));
    let (w, why) = one(Chain::Wall, &late, "claude", Scenario::W1);
    assert_eq!(w, "PASS", "{why}");
    assert!(why.contains("(400.0 s past the reset)"), "{why}");
    let mut continued = late.clone();
    continued.cover.went_on_ms = Some(reset + 60_000);
    let (w, why) = one(Chain::Wall, &continued, "claude", Scenario::W1);
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("TYPED ON TOP of the program's own continuation"),
        "{why}"
    );
    // NEGATIVE CONTROLS: the same records on every other harness
    for program in ["gemini", "agy", "muse"] {
        let (w, why) = one(Chain::Wall, &late, program, Scenario::W1);
        assert_eq!(w, "FAIL", "{program}: {why}");
        assert!(
            why.contains("first act after the reset"),
            "{program}: {why}"
        );
    }
    // and past Claude Code's own deadline too
    let later = walled_late(reset + 451_000 - (T0 + 361_000));
    let (w, why) = one(Chain::Wall, &later, "claude", Scenario::W1);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("first act after the reset"), "{why}");
}

/// W1 WHERE THE PROGRAM GOES ON BY ITSELF (the review of the W1 judge on
/// Claude Code): Claude Code's own continuation reached the stub past the
/// reset ([`Cover::went_on_ms`]), and the loop's `usage-resume@v1` came after
/// it, typed into a conversation the vendor had already carried past the
/// wall — a double continue (`r21amc15b`'s class): FAIL by name, though it
/// came inside the grace and the defense landed. The continuation answered
/// and landed with nothing typed after the reset: PASS, the continuation
/// named. NEGATIVE CONTROLS: the loop's act before the continuation (the
/// loop went on first) and no continuation at all pass as before.
#[test]
fn an_act_after_the_programs_own_continuation_fails_w1() {
    let reset = T0 + 300_000;
    let w1 = |s: &Seen| one(Chain::Wall, s, "claude", Scenario::W1);
    // Claude Code went on 60 s past the reset; the loop typed its resume at
    // its grace, 150 s past it.
    let mut s = walled_late(reset + 150_000 - (T0 + 361_000));
    s.cover.went_on_ms = Some(reset + 60_000);
    let (w, why) = w1(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains(
            "TYPED ON TOP of the program's own continuation: it went on at +360.0 s (60.0 s past the reset), and usage-resume@v1 was typed at +450.0 s, 90.0 s after it"
        ),
        "{why}"
    );
    // The continuation answered and landed, nothing typed after the reset.
    let mut s = walled();
    s.journal.remove(5);
    s.ledger.remove(4);
    s.cover.went_on_ms = Some(T0 + 362_000);
    let (w, why) = w1(&s);
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains("the program's own continuation at +362.0 s (62.0 s past the reset) answered"),
        "{why}"
    );
    // NEGATIVE CONTROLS. The loop went on first (its act at +361 s).
    let mut s = walled();
    s.cover.went_on_ms = Some(T0 + 362_000);
    let (w, why) = w1(&s);
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains("gone on: usage-resume@v1 typed at +361.0 s"),
        "{why}"
    );
    // No continuation of the program's own: the act is the harness going on.
    let (w, why) = w1(&walled_late(reset + 150_000 - (T0 + 361_000)));
    assert_eq!(w, "PASS", "{why}");
}

/// THE PROGRAM'S OWN CONTINUATION IN THE STUB'S DUMPS ([`first_continuation`],
/// [`carries_continuation`]): the first request of the conversation from the
/// wall's lift on whose user texts hold Claude Code's words for it
/// ([`CLAUDE_CONTINUES`], as 2.1.306 sent them beside the cut act). NEGATIVE
/// CONTROLS: a request before the lift (a retry the wall answered); another
/// conversation's; the harness's own resume (marked, no such words); the
/// words in the assistant's text, not a user's.
#[test]
fn the_programs_own_continuation_is_the_first_request_holding_its_words() {
    let tok = Chain::Wall.token();
    let words = "Your claude.ai usage limit has reset. Continue the task you were working on \
                 when the limit was reached; do not repeat work that is already complete.";
    let continued = messages(&tok, &[&format!("{LAND}\n{words}")]);
    assert!(carries_continuation(&continued, &tok));
    assert!(!carries_continuation(&continued, &Chain::LandB.token()));
    let resumed = messages(&tok, &[LAND, LAND_NUDGE]);
    assert!(!carries_continuation(&resumed, &tok));
    let said = J::obj([(
        "messages",
        J::Arr(vec![
            J::obj([("role", J::s("user")), ("content", J::s(&tok))]),
            J::obj([("role", J::s("assistant")), ("content", J::s(words))]),
        ]),
    )])
    .dump(crate::pyjson::COMPACT)
    .into_bytes();
    assert!(!carries_continuation(&said, &tok), "the assistant's words");
    let dumps = vec![
        (100, continued.clone()),
        (200, resumed.clone()),
        (300, continued.clone()),
        (400, continued),
    ];
    assert_eq!(first_continuation(&dumps, &tok, 150), Some(300));
    assert_eq!(first_continuation(&dumps, &tok, 0), Some(100));
    assert_eq!(first_continuation(&dumps, &tok, 401), None);
    assert_eq!(first_continuation(&dumps[1..2], &tok, 0), None);
}

/// The reason the policy escalates Gemini CLI's usage-limit dialog with
/// (`approval.rs`'s `foreign_limit`).
const GEMINI_LIMIT_HELD: &str = "Gemini CLI's usage limit: neither Keep trying nor Stop is \
     pressed; /stats model in this tab says which limit holds it";

/// W1 on Gemini CLI as the live lab drew it (Gemini CLI 0.46.0 on the stub's
/// `limit:code_assist`, 2026-10-21): the land act met by the usage-limit
/// dialog, a box (`EVENT prompt … kind=usage-limit`), escalated to a person
/// (`needs=ask`), and nothing going on past the reset.
fn gemini_walled() -> Seen {
    let mut s = walled();
    s.journal.truncate(3);
    s.journal.push(j(130_000, "ESCALATED seq=2 attention=OK"));
    s.journal.push(j(
        130_000,
        "EVENT prompt seq=2 kind=usage-limit classify=- command=-",
    ));
    s.ledger.truncate(2);
    s.ledger
        .push(row(130_000, "-", "escalated", "", GEMINI_LIMIT_HELD));
    s.status.truncate(1);
    s.status
        .push(status_of(130_500, "open", "ask", "land", "aterm-harness@1"));
    s.end_ms = T0 + 300_000 + wall_grace_ms("gemini") + WALL_LOST_MS + 1_000;
    s
}

/// A usage-limit DIALOG reads the wall (Gemini CLI draws its limit as a box,
/// not a notice at a turn's end), and the dialog handed to a person is a stall
/// W1 FAILS by name — never "typed into the wall", never unobserved.
#[test]
fn a_usage_limit_dialog_reads_the_wall_and_its_escalation_fails_w1() {
    let w1 = |s: &Seen| one(Chain::Wall, s, "gemini", Scenario::W1);
    let (w, why) = w1(&gemini_walled());
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("ESCALATED the wall to a person at +130.0 s: Gemini CLI's usage limit"),
        "{why}"
    );
    assert!(
        why.contains("+130.0 s EVENT prompt seq=2 kind=usage-limit"),
        "{why}"
    );
    assert!(!why.contains("TYPED into the wall"), "{why}");
    // NEGATIVE CONTROLS. The dialog read, nothing escalated, gone on past the
    // reset and landed: the box is the wall's point, and W1 passes
    let mut s = walled();
    s.journal[3].1 = "EVENT prompt seq=2 kind=usage-limit classify=- command=-".into();
    let (w, why) = w1(&s);
    assert_eq!(w, "PASS", "{why}");
    // a box of another kind reads no wall (its point still judged, a wait
    // that names no wall: two acts with none between fail every chain)
    s.journal[3].1 = "EVENT prompt seq=2 kind=permission classify=- command=touch".into();
    s.journal[4].1 = format!(
        "WAITING seq=2 until={} a person's answer to the box",
        stamp(300_000)
    );
    s.ledger.remove(2);
    let (w, why) = w1(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("never drawn as a limit point"), "{why}");
    // an escalation after the reset is no stall of the wall
    let mut s = walled();
    s.ledger
        .insert(4, row(300_500, "-", "escalated", "", GEMINI_LIMIT_HELD));
    let (w, why) = w1(&s);
    assert!(!why.contains("ESCALATED the wall"), "{w}: {why}");
}

/// W1's records on Gemini CLI kept trying (`limit-retry@v1`): the land act
/// met by the account's usage-limit dialog, a box read as the wall's point,
/// waited on to the end of the minute it names and a minute's grace past the
/// reset (+300 s), its `Keep trying` pressed then (ledgered `approved`), and
/// the turn it sent again landing the task.
fn gemini_kept_trying() -> Seen {
    let mut s = walled();
    s.journal.truncate(3);
    s.journal.extend([
        j(
            140_000,
            "EVENT prompt seq=2 kind=usage-limit classify=- command=-",
        ),
        j(
            140_100,
            &format!(
                "WAITING seq=2 until={} rule=limit-retry@v1 Gemini CLI's usage limit resets at \
                 1:05 PM: Keep trying is pressed past it",
                stamp(420_000)
            ),
        ),
        j(421_000, "APPROVED seq=2 Keep trying (reset 1:05 PM)"),
        j(425_000, "EVENT idle seq=3 Nothing left: the parser"),
        j(425_000, "DONE seq=3 rule=land@v1 x"),
    ]);
    s.ledger = vec![
        point(5_000, 1),
        typed(125_000, "land@v1", LAND),
        point(140_000, 2),
        row(
            421_000,
            "limit-retry@v1",
            "approved",
            "Keep trying (reset 1:05 PM)",
            "",
        ),
        point(425_000, 3),
        task_done(425_000),
    ];
    s.status = vec![
        status(1_000, "open", "land"),
        status(426_000, "landed", "land"),
    ];
    s.end_ms = T0 + 425_000 + 545_000;
    s
}

/// GEMINI CLI KEPT TRYING PASSES W1: the usage-limit dialog takes no typing,
/// so the harness going on past the wall is its `Keep trying` pressed past
/// the reset (`limit-retry@v1`, ledgered `approved`), and the turn that press
/// sent landing the task. NEGATIVE CONTROLS: the press in the wall is an act
/// in it (FAIL); with no press ledgered, the landing is the program's own
/// retry (UNOBSERVED, F14); a press past the deadline is persistence lost.
#[test]
fn a_keep_trying_pressed_past_the_reset_goes_on_past_the_wall() {
    let w1 = |s: &Seen| one(Chain::Wall, s, "gemini", Scenario::W1);
    let (w, why) = w1(&gemini_kept_trying());
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains(
            "gone on: limit-retry@v1 pressed at +421.0 s (121.0 s past the reset), ledgered"
        ),
        "{why}"
    );
    let mut s = gemini_kept_trying();
    s.ledger[3].ts = T0 + 200_000;
    s.journal[4].0 = T0 + 200_000;
    let (w, why) = w1(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("TYPED into the wall: 0 journal act(s), 1 ledger act(s) (the first limit-retry@v1 approved at +200.0 s)"),
        "{why}"
    );
    let mut s = gemini_kept_trying();
    s.ledger.remove(3);
    let (w, why) = w1(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("the program's own retry landed it"), "{why}");
    let mut s = gemini_kept_trying();
    s.ledger[3].ts = T0 + 700_000;
    let (w, why) = w1(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("PERSISTENCE LOST: the first press after the reset"),
        "{why}"
    );
}

/// Muse Code's `/rules` output, entered once ([`HIDE_COMMAND`]; measured on
/// 1.4.10, 2026-10-21).
const RULES_ROWS: &str =
    "  Rules loaded this session (in precedence order):\n  user: none\n  project: none\n";

/// Muse Code's usage wall ([`USAGE_WALL`](aterm_phase::muse::fixtures::USAGE_WALL))
/// split at its composer frame: the rows above it, and the frame with what
/// follows.
fn wall_split() -> (String, String) {
    let body = aterm_phase::muse::fixtures::USAGE_WALL
        .split_once('\n')
        .map_or("", |(_, b)| b);
    let at = body.find("── Voice input").expect("the frame");
    (body[..at].to_string(), body[at..].to_string())
}

/// Muse Code's usage wall taken off the screen ([`notice_gone`]): `/rules`
/// output over the composer, the notice's words scrolled out of sight (the
/// tail of its wrapped reset still at the top, as measured). NEGATIVE
/// CONTROLS: the wall's own screen; `/rules` output under the notice still in
/// sight; a screen with no frame.
#[test]
fn the_notice_is_gone_only_when_no_row_draws_it() {
    let (above, frame) = wall_split();
    let rules = format!("{RULES_ROWS}\n").repeat(4);
    let scrolled = format!("  PM\n\n{rules}{frame}");
    assert!(notice_gone(Harness::Muse, &scrolled), "{scrolled}");
    let wall = format!("{above}{frame}");
    assert!(!notice_gone(Harness::Muse, &wall));
    let under = format!("{above}{rules}{frame}");
    assert!(
        under.contains(WALL_WORDS) && !notice_gone(Harness::Muse, &under),
        "{under}"
    );
    assert!(!notice_gone(Harness::Muse, &rules));
}

/// The hide's guard ([`menu_offers`]): Muse Code's composer holding
/// `/rules` with its argument hint, the menu's first row that command — as
/// measured. NEGATIVE CONTROLS: the first row another command, or a longer
/// one; the composer holding another draft, or more after the command; no
/// frame.
#[test]
fn the_hide_enters_only_the_command_the_menu_offers_first() {
    let (above, _) = wall_split();
    let shown = |draft: &str, first: &str| {
        format!(
            "{above}── Voice input (⌥ + v to start) ──────────\n❯ {draft}\n  {first}\n──────────\n  lab-stub · high · /private/tmp/at.lab0/proj · Auto-review\n"
        )
    };
    let offered = shown(
        "/rules [import]",
        "/rules  Show which md files govern this session",
    );
    assert!(
        menu_offers(Harness::Muse, &offered, HIDE_COMMAND),
        "{offered}"
    );
    assert!(menu_offers(
        Harness::Muse,
        &shown("/rules", "/rules  Show which md files govern this session"),
        HIDE_COMMAND
    ));
    assert!(!menu_offers(
        Harness::Muse,
        &shown("/rules [import]", "/review  Review the changes"),
        HIDE_COMMAND
    ));
    assert!(!menu_offers(
        Harness::Muse,
        &shown("/rules [import]", "/rulesets  Show the rule sets"),
        HIDE_COMMAND
    ));
    assert!(!menu_offers(
        Harness::Muse,
        &shown("/rul", "/rules  Show which md files govern this session"),
        HIDE_COMMAND
    ));
    assert!(!menu_offers(
        Harness::Muse,
        &shown(
            "/rules import",
            "/rules  Show which md files govern this session"
        ),
        HIDE_COMMAND
    ));
    assert!(!menu_offers(Harness::Muse, RULES_ROWS, HIDE_COMMAND));
}

/// agy's `/title` output, entered once ([`AGY_HIDE_COMMAND`]; measured on
/// 1.4.0, 2026-10-21, with no `title` block in the lab's settings).
const TITLE_ROWS: &str = "> /title\n  ⎿  Error: No title command configured in settings.json. Please add a \"title\" block.\n";

/// agy's per-day quota wall ([`ERROR_429`](aterm_phase::agy::fixtures::lab::ERROR_429),
/// measured) split at its composer frame: the rows above it (the `⚠ agent
/// executor error: …` block and its `Error ID:` row), and the frame — its
/// top rule, the empty composer, its bottom rule, the footer.
fn agy_wall_split() -> (String, String) {
    let body = aterm_phase::agy::fixtures::lab::ERROR_429
        .split_once('\n')
        .map_or("", |(_, b)| b);
    let caret = body.rfind("\n>\n").expect("the empty composer");
    let at = body[..caret].rfind('\n').expect("its top rule") + 1;
    (body[..at].to_string(), body[at..].to_string())
}

/// agy's quota wall taken off the screen ([`notice_gone`]): `/title` output
/// over the composer, every row of the `⚠` block scrolled out of sight.
/// NEGATIVE CONTROLS: the wall's own screen; `/title` output under the block
/// still in sight — where agy's reader already reads no wall (the `> /title`
/// row is the last message: [`aterm_phase::agy::wall`]), so the words alone
/// say the notice stands; only its last row, `Error ID:`, still at the top;
/// a screen with no frame; and Muse Code's and the harnesses with no hide.
#[test]
fn agys_notice_is_gone_only_when_no_row_of_its_block_draws() {
    let (above, frame) = agy_wall_split();
    let titles = TITLE_ROWS.repeat(6);
    let scrolled = format!("{titles}{frame}");
    assert!(notice_gone(Harness::Agy, &scrolled), "{scrolled}");
    let wall = format!("{above}{frame}");
    let rows = |t: &str| t.lines().map(str::to_string).collect::<Vec<_>>();
    assert!(
        aterm_phase::agy::wall(&rows(&wall)).is_some(),
        "the wall reads"
    );
    assert!(!notice_gone(Harness::Agy, &wall));
    let under = format!("{above}{titles}{frame}");
    assert!(
        aterm_phase::agy::wall(&rows(&under)).is_none(),
        "the reader lets the wall go under the first command: {under}"
    );
    assert!(!notice_gone(Harness::Agy, &under), "{under}");
    let id_row = above
        .lines()
        .find(|l| l.starts_with("Error ID: "))
        .expect("the block's last row");
    let tail = format!("{id_row}\n\n{titles}{frame}");
    assert!(!notice_gone(Harness::Agy, &tail), "{tail}");
    assert!(!notice_gone(Harness::Agy, &titles));
    // the scrolled screen is no other harness's
    for h in [Harness::Muse, Harness::Claude, Harness::Gemini] {
        assert!(!notice_gone(h, &scrolled), "{h:?}");
    }
}

/// The hide's guard on agy ([`menu_offers`]): the composer holding `/title`
/// alone and the menu's focused row, right under the composer's bottom rule,
/// that command — as measured. NEGATIVE CONTROLS: the focused row another
/// command, or a longer one; the composer holding a prefix of it, or more
/// after it; no frame; the same screen read as Muse Code's.
#[test]
fn agys_hide_enters_only_the_command_its_menu_offers_first() {
    let (above, frame) = agy_wall_split();
    let rule = frame.lines().next().expect("the top rule").to_string();
    let shown = |draft: &str, first: &str| {
        format!(
            "{above}{rule}\n> {draft}\n{rule}\n> {first}\n\n  ↑/↓ Navigate · enter Select · tab Complete\nesc to cancel                                                                                                               Gemini 3.1 Pro · low\n"
        )
    };
    let title = "/title  Toggle custom terminal window title";
    let offered = shown("/title", title);
    assert!(
        menu_offers(Harness::Agy, &offered, AGY_HIDE_COMMAND),
        "{offered}"
    );
    assert!(!menu_offers(
        Harness::Agy,
        &shown("/title", "/tasks              View background tasks"),
        AGY_HIDE_COMMAND
    ));
    assert!(!menu_offers(
        Harness::Agy,
        &shown("/title", "/titlebar  Toggle the title bar"),
        AGY_HIDE_COMMAND
    ));
    assert!(!menu_offers(
        Harness::Agy,
        &shown("/tit", title),
        AGY_HIDE_COMMAND
    ));
    assert!(!menu_offers(
        Harness::Agy,
        &shown("/title now", title),
        AGY_HIDE_COMMAND
    ));
    assert!(!menu_offers(Harness::Agy, TITLE_ROWS, AGY_HIDE_COMMAND));
    assert!(!menu_offers(Harness::Muse, &offered, AGY_HIDE_COMMAND));
    assert!(!menu_offers(Harness::Gemini, &offered, AGY_HIDE_COMMAND));
}

/// W2's records: W1's ([`walled`]) with the wall resetting further ahead
/// (+420 s), the loop waiting on it (+156 s), the notice scrolled away by ten
/// `/rules` (from +160 s; the hiding Enter at +170 s, out of sight at +172.5 s,
/// screen seq 40), a screen without the wall read (+168.4 s, seq 30, a row of
/// the notice still in sight) and waited on, and the resume typed at the
/// hidden screen past the reset and its grace, landed.
fn walled_hidden() -> Seen {
    Seen {
        journal: vec![
            j(5_000, "EVENT idle seq=1 Implemented the parser"),
            j(
                5_100,
                "WAITING seq=1 until=x 1 short turn(s) in a row: 2 min back-off",
            ),
            j(
                125_000,
                "CONTINUED seq=1 rule=land@v1 [aterm harness] Autopilot is Land:",
            ),
            j(
                126_000,
                "EVENT limited seq=2 message=◆ Usage limit reached · /upgrade (x) for increased limits, or wait for usage to reset at 1:06 PM reset=1:06 PM",
            ),
            j(
                126_100,
                "WAITING seq=2 until=x the worker to take the land@v1 act",
            ),
            j(
                156_000,
                &format!(
                    "WAITING seq=2 until={} usage-session resets",
                    stamp(420_000)
                ),
            ),
            j(168_400, "EVENT idle seq=30 -"),
            j(
                168_500,
                &format!(
                    "WAITING seq=30 until={} usage-session resets",
                    stamp(420_000)
                ),
            ),
            j(
                481_000,
                "CONTINUED seq=40 rule=usage-resume@v1 [aterm harness] Still landing:",
            ),
            j(485_000, "EVENT idle seq=41 Nothing left: the parser"),
            j(485_000, "DONE seq=41 rule=land@v1 x"),
        ],
        ledger: vec![
            point(5_000, 1),
            typed(125_000, "land@v1", LAND),
            row(
                126_000,
                "wall-wait@v1",
                "waiting",
                "",
                "the turn-end policy (wall: x)",
            ),
            point(156_000, 2),
            point(168_400, 2),
            typed(481_000, "usage-resume@v1", LAND_NUDGE),
            row(482_500, WALL_END, "waiting", "", "the wall left the screen"),
            row(484_996, WALL_END, "waiting", "", "the limit's track ended"),
            point(485_000, 3),
            task_done(485_000),
        ],
        status: vec![
            status(1_000, "open", "land"),
            status(486_000, "landed", "land"),
        ],
        meta: vec![no_meta(1_000)],
        task: vec![j(1_000, "OK task -")],
        end_ms: T0 + 485_000 + 545_000,
        stub: Some(StubSeen {
            baseline: 2,
            read: 0,
            new_turns: 0,
        }),
        cover: Cover {
            wall: Some((T0 + 6_000, (T0 + 420_000) / 1000)),
            lifted_ms: Some(T0 + 417_000),
            window_stub: Some(StubSeen {
                baseline: 1,
                read: 3,
                new_turns: 0,
            }),
            hidden: Some(Hidden {
                first_ms: T0 + 160_000,
                enter_ms: T0 + 170_000,
                seen_ms: T0 + 172_500,
                seq: Some(40),
                commands: 10,
                command: HIDE_COMMAND,
            }),
            ..Cover::default()
        },
        ..Seen::default()
    }
}

/// The waits the remembered wall's look writes while the conversation is
/// unread (`limit-conversation-unread`, 0ab6eaf3e6): five minutes at a time,
/// the reset or not.
const UNREAD_LOOK: &str =
    "until=x the remembered limit's conversation is unread; no act on its account";

#[test]
fn the_remembered_wall_passes_where_the_screen_without_its_notice_was_waited_out_and_resumed() {
    let w2 = |s: &Seen| one(Chain::WallHid, s, "muse", Scenario::W2);
    let (w, why) = w2(&walled_hidden());
    assert_eq!(w, "PASS", "{why}");
    assert!(why.contains("10 /rules from +160.0 s"), "{why}");
    assert!(why.contains("read +168.4 s EVENT idle seq=30"), "{why}");
    assert!(
        why.contains("past the reset usage-resume@v1 typed at +481.0 s (seq=40)"),
        "{why}"
    );
    // W1's records under W2's judge: the notice never left, the remembered
    // wall never driven — W1 does not exercise it
    let (w, why) = w2(&walled());
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("the notice never left the screen"), "{why}");
    // NEGATIVE CONTROL, the product's: 0ab6eaf3e6's look on a conversation
    // nothing reads — the hidden screen held five minutes at a time, past the
    // reset too, nothing typed, never landed: PERSISTENCE LOST, the look named
    let lost = T0 + 420_000 + wall_grace_ms("muse") + WALL_LOST_MS;
    let mut s = walled_hidden();
    s.journal.truncate(7);
    s.journal
        .push(j(168_500, &format!("WAITING seq=30 {UNREAD_LOOK}")));
    s.journal
        .push(j(420_100, &format!("WAITING seq=40 {UNREAD_LOOK}")));
    s.journal
        .push(j(720_100, &format!("WAITING seq=40 {UNREAD_LOOK}")));
    s.ledger.truncate(5);
    s.status.truncate(1);
    s.end_ms = lost + 1_000;
    let (w, why) = w2(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("PERSISTENCE LOST"), "{why}");
    assert!(why.contains("conversation is unread"), "{why}");
    // typed into the hidden wall before its reset
    let mut s = walled_hidden();
    s.journal.insert(
        8,
        j(
            200_000,
            "CONTINUED seq=40 rule=usage-resume@v1 [aterm harness] Still landing:",
        ),
    );
    let (w, why) = w2(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("TYPED into the wall"), "{why}");
    // UNOBSERVED: the hide given up, or never tried
    let mut s = walled_hidden();
    s.cover.hidden = None;
    s.cover.hide_gave_up = Some("the menu did not offer /rules first".into());
    let (w, why) = w2(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("did not offer /rules first"), "{why}");
    // out of sight only at or after the reset
    let mut s = walled_hidden();
    s.cover.hidden.as_mut().unwrap().seen_ms = T0 + 420_000;
    assert_eq!(w2(&s).0, "UNOBSERVED");
    // the hide begun before the land act met the wall
    let mut s = walled_hidden();
    s.cover.hidden.as_mut().unwrap().first_ms = T0 + 100_000;
    assert_eq!(w2(&s).0, "UNOBSERVED");
    // no point read a screen without the wall: none after the first command,
    // only the limit's own, or only one from before the hide began
    let mut s = walled_hidden();
    s.journal.remove(6);
    let (w, why) = w2(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(
        why.contains("no point was read on a screen without the wall"),
        "{why}"
    );
    let mut s = walled_hidden();
    s.journal[6].1 = "EVENT limited seq=30 message=◆ Usage limit reached · x".into();
    assert_eq!(w2(&s).0, "UNOBSERVED");
    let mut s = walled_hidden();
    s.cover.hidden.as_mut().unwrap().first_ms = T0 + 168_500;
    assert_eq!(w2(&s).0, "UNOBSERVED");
    // the resume typed at a screen from before the notice left
    let mut s = walled_hidden();
    s.journal[8].1 = "CONTINUED seq=2 rule=usage-resume@v1 [aterm harness] Still landing:".into();
    let (w, why) = w2(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("from before the notice left"), "{why}");
    // nothing typed past the reset, the program's own retry landing it: no
    // resume decided from the remembered wall
    let mut s = walled_hidden();
    s.journal.remove(8);
    s.ledger.remove(5);
    let (w, why) = w2(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("nothing typed past the reset"), "{why}");
    // a screen seq the lab could not read: the times alone place the point
    let mut s = walled_hidden();
    s.cover.hidden.as_mut().unwrap().seq = None;
    assert_eq!(w2(&s).0, "PASS");
    // W1's judge over W2's records passes: W2 holds W1 and more
    assert_eq!(
        one(Chain::Wall, &walled_hidden(), "muse", Scenario::W1).0,
        "PASS"
    );
}

// ---- the window (slice 5c, §12.5.4)

/// The window as a run that read it records it of the tab labelled `label`.
fn window(label: &str) -> surfaces::Window {
    surfaces::Window {
        read: true,
        label: label.into(),
        ..surfaces::Window::default()
    }
}

/// A `status` read with the end's and the needs' fields (§12.5.2).
fn st(t: i64, task: &str, needs: &str, n: &str, end: &str, seen: &str) -> (i64, String) {
    j(
        t,
        &format!(
            "task={task} needs={needs} needs_n={n} autopilot=land supervisor=aterm-harness@1 agent=- end={end} end_seen={seen} end_cheered=0"
        ),
    )
}

/// `line` with Land's hold's three fields (slice 3c, §12.3.1): `land_held=`
/// its legs, `land_nudges=`, `land_since=` its land act's second.
fn hold(line: (i64, String), held: &str, nudges: &str, since: &str) -> (i64, String) {
    (
        line.0,
        format!(
            "{} land_held={held} land_nudges={nudges} land_since={since}",
            line.1
        ),
    )
}

/// The second land-a's and land-j's `land@v1` was ledgered at (+125 s).
const LAND_SINCE: &str = "1125";

/// A chip line of a tab behind the front one, or Gemini CLI's.
fn behind(program: &str, face: &str) -> String {
    if program == "gemini" {
        "window=2 program=gemini face=- drawn=- why=no-footer spoken=-".to_string()
    } else {
        format!("window=2 program={program} face={face} drawn=- why=not-drawn spoken=-")
    }
}

/// L1, L3, L5 with the window read as a passing run reads it: land-a behind,
/// its need's row and attention while it stood, landed under the floor (no
/// end, no •, no row), the page's state `landed <time>`.
fn seen_land_a(program: &str) -> Seen {
    let mut s = landed_a();
    s.meta[1] = needs_meta(131_000, program);
    s.status = vec![
        hold(st(1_000, "open", "-", "-", "-", "-"), "-", "-", "-"),
        hold(
            st(126_000, "open", "-", "-", "-", "-"),
            "-",
            "0",
            LAND_SINCE,
        ),
        hold(
            st(131_000, "open", "ask", "1", "-", "-"),
            "needs",
            "0",
            LAND_SINCE,
        ),
        hold(
            st(371_000, "open", "ask", "1", "-", "-"),
            "needs",
            "1",
            LAND_SINCE,
        ),
        hold(st(376_000, "landed", "-", "-", "-", "-"), "-", "-", "-"),
    ];
    s.status_reads.push(T0 + 380_000);
    s.window = surfaces::Window {
        hover_reads: vec![T0 + 380_000],
        page_reads: vec![T0 + 380_000],
        chip: vec![
            j(1_000, &behind(program, "land")),
            j(377_000, &behind(program, "landed")),
        ],
        strip: vec![
            j(1_000, "index=1 selected=0 states=busy"),
            j(131_000, "index=1 selected=0 states=attention"),
            j(376_000, "index=1 selected=0 states=-"),
        ],
        hover: vec![
            j(1_000, "driven by aterm (land)"),
            j(131_000, "driven by aterm (land: not landed yet, needs you)"),
            j(377_000, "driven by aterm (land, landed)"),
        ],
        menu: vec![
            j(
                131_000,
                &format!("needs-head|Needs you (1) rows=1\nneeds|◇ land-a — {NEED} · #"),
            ),
            j(376_000, ""),
        ],
        page: vec![
            j(1_000, "head=land-a: Window 2, tab 2"),
            j(
                131_000,
                &format!(
                    "head=land-a: Window 2, tab 2\nstate=not landed yet: needs you: {NEED} \u{b7} \
                     landing since 03:18"
                ),
            ),
            j(377_000, "head=land-a: Window 2, tab 2\nstate=landed 03:24"),
        ],
        ..window("land-a")
    };
    s
}

#[test]
fn land_a_passes_on_the_window_a_person_would_see() {
    for program in ["claude", "gemini"] {
        assert_eq!(
            verdicts(Chain::LandA, &seen_land_a(program), program),
            [("L1", "PASS"), ("L3", "PASS"), ("L5", "PASS")],
            "{program}"
        );
    }
    let (_, why) = one(Chain::LandA, &seen_land_a("claude"), "claude", Scenario::L5);
    assert!(why.contains("state=landed 03:24"), "{why}");
}

/// §12.5.6's controls on land-a: each wrong window read fails its scenario.
#[test]
fn land_a_fails_a_wrong_window_read() {
    let l = |s: &Seen, program: &str, sc: Scenario| one(Chain::LandA, s, program, sc);
    // a `done` state at L5: a • under the floor
    let mut s = seen_land_a("claude");
    s.window
        .strip
        .push(j(380_000, "index=1 selected=0 states=done"));
    assert_eq!(l(&s, "claude", Scenario::L5).0, "FAIL");
    // an end under the floor
    let mut s = seen_land_a("claude");
    s.status
        .push(st(380_000, "landed", "-", "-", "1792578282", "0"));
    let (w, why) = l(&s, "claude", Scenario::L5);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("an END"), "{why}");
    // a Done for you row under the floor
    let mut s = seen_land_a("claude");
    s.window.menu.push(j(380_000, "done|✓ land-a — landed #"));
    assert_eq!(l(&s, "claude", Scenario::L5).0, "FAIL");
    // the page never says landed
    let mut s = seen_land_a("claude");
    s.window.page.pop();
    assert_eq!(l(&s, "claude", Scenario::L5).0, "FAIL");
    // its row never on the page's screen: unread, never a pass
    let mut s = seen_land_a("claude");
    for (_, p) in &mut s.window.page {
        *p = "-".into();
    }
    let (w, why) = l(&s, "claude", Scenario::L5);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("never on the page's screen"), "{why}");
    // no face=landed after the landing
    let mut s = seen_land_a("claude");
    s.window.chip.pop();
    assert_eq!(l(&s, "claude", Scenario::L5).0, "FAIL");
    // a missing Needs you row at L3, and one in the entry's words
    let mut s = seen_land_a("claude");
    s.window.menu.remove(0);
    let (w, why) = l(&s, "claude", Scenario::L3);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("no Needs you row"), "{why}");
    let mut s = seen_land_a("claude");
    s.window.menu[0].1 =
        format!("needs-head|Needs you (1) rows=1\nneeds|◇ land-a — claude needs you: {NEED} · #");
    assert_eq!(l(&s, "claude", Scenario::L3).0, "FAIL");
    // its header counting another number than the rows under it, or none
    // over it
    for head in ["needs-head|Needs you (2) rows=1\n", ""] {
        let mut s = seen_land_a("claude");
        s.window.menu[0].1 = format!("{head}needs|◇ land-a — {NEED} · #");
        let (w, why) = l(&s, "claude", Scenario::L3);
        assert_eq!(w, "FAIL", "{head:?}: {why}");
        assert!(why.contains("header"), "{why}");
    }
    // the tab's attention state never shown
    let mut s = seen_land_a("claude");
    s.window.strip[1].1 = "index=1 selected=0 states=busy".into();
    assert_eq!(l(&s, "claude", Scenario::L3).0, "FAIL");
    // a count that is not the one need's
    let mut s = seen_land_a("claude");
    s.status[2] = hold(
        st(131_000, "open", "ask", "2", "-", "-"),
        "needs",
        "0",
        LAND_SINCE,
    );
    assert_eq!(l(&s, "claude", Scenario::L3).0, "FAIL");
    // L1: a tab behind drawing a stale face, and a face not Land's
    let mut s = seen_land_a("claude");
    s.window.chip[0].1 = "window=2 program=claude face=land drawn=\"⤓ land\" spoken=\"x\"".into();
    assert_eq!(l(&s, "claude", Scenario::L1).0, "FAIL");
    let mut s = seen_land_a("claude");
    s.window.chip[0].1 = behind("claude", "cruise");
    assert_eq!(l(&s, "claude", Scenario::L1).0, "FAIL");
    // each of L1's two face wants alone: a Cruise face before the land act,
    // with `land` read while it landed, fails the Autopilot's faces and
    // passes `face=land while it lands`...
    let mut s = seen_land_a("claude");
    s.window.chip = vec![
        j(1_000, &behind("claude", "cruise")),
        j(126_000, &behind("claude", "land")),
        j(377_000, &behind("claude", "landed")),
    ];
    let (w, why) = l(&s, "claude", Scenario::L1);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("face=cruise (wanted"), "{why}");
    assert!(why.contains("face=land while it lands: PASS"), "{why}");
    // ...and `landed` read first 5 s before its DONE (a face the Autopilot
    // allows there) fails `face=land while it lands` alone
    let mut s = seen_land_a("claude");
    s.window.chip = vec![
        j(370_000, &behind("claude", "landed")),
        j(377_000, &behind("claude", "landed")),
    ];
    let (w, why) = l(&s, "claude", Scenario::L1);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("never land"), "{why}");
    assert!(
        why.contains("a tab behind drawn=- why=not-drawn: PASS"),
        "{why}"
    );
    // a landed tab still reading `land` 35 s after its DONE: a stale face
    let mut s = seen_land_a("claude");
    s.window.chip.push(j(410_000, &behind("claude", "land")));
    let (w, why) = l(&s, "claude", Scenario::L1);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("face=land (wanted [\"landed\"])"), "{why}");
    // Gemini CLI draws no chip: a face on its line fails
    let mut s = seen_land_a("gemini");
    s.window.chip[0].1 = behind("claude", "land");
    assert_eq!(l(&s, "gemini", Scenario::L1).0, "FAIL");
}

#[test]
fn the_window_wants_are_named_skips_without_the_window() {
    let mut s = seen_land_a("claude");
    s.window.read = false;
    s.window
        .strip
        .push(j(380_000, "index=1 selected=0 states=done"));
    let (w, why) = one(Chain::LandA, &s, "claude", Scenario::L5);
    assert_eq!(w, "PASS", "{why}");
    assert!(why.contains("SKIPPED (--no-surfaces)"), "{why}");
    assert!(
        why.contains("under the floor"),
        "the skip names its wants: {why}"
    );
}

/// N1–N4 as a passing run records them: cruise-n the front tab, its chip
/// drawn `∞` throughout; two needs, none, one, two.
fn seen_cruise_n() -> Seen {
    let need = |t: i64, more: &str| {
        j(
            t,
            &format!(
                "attention_owners=1 attention_kinds=ask:needs attention=claude needs you: {NEED_A}{more} — carrying on"
            ),
        )
    };
    let status = |t: i64, needs: &str, n: &str| {
        j(
            t,
            &format!(
                "task=open needs={needs} needs_n={n} autopilot=cruise supervisor=aterm-harness@1 agent=- end=- end_seen=- end_cheered=-"
            ),
        )
    };
    // Its row says the first need with no count: every other one has a row
    // of its own one level in under it (slice 2d).
    let row = |t: i64, others: &[&str]| {
        let under: String = others
            .iter()
            .map(|other| format!("\nneeds|{other} indent=1"))
            .collect();
        j(t, &format!("needs|◇ cruise-n — {NEED_A} · #{under}"))
    };
    let page = |t: i64, state: &str| {
        j(
            t,
            &format!("head=cruise-n: Window 2, tab 17\nstate={state}"),
        )
    };
    Seen {
        journal: vec![
            j(5_000, &format!("EVENT idle seq=1 NEEDS YOU: {NEED_B}")),
            j(
                5_000,
                "NEEDS seq=1 count=2 attention=OK grant Screen Recording",
            ),
            j(
                5_100,
                "WAITING seq=1 until=x 1 short turn(s) in a row: 2 min back-off",
            ),
            j(
                125_000,
                "CONTINUED seq=1 rule=continue@v1 [aterm harness] Autopilot is Cruise",
            ),
            j(130_000, "EVENT idle seq=2 ⏺ Nothing left: the parser"),
            j(130_000, "CLEARED seq=2 needs attention=OK none stands now"),
            j(
                130_100,
                "WAITING seq=2 until=x 2 short turn(s) in a row: 4 min back-off",
            ),
            j(
                370_000,
                "CONTINUED seq=2 rule=continue-done-check@v1 [aterm harness] x",
            ),
            j(375_000, &format!("EVENT idle seq=3 NEEDS YOU: {NEED_A}")),
            j(
                375_000,
                "NEEDS seq=3 count=1 attention=OK grant Screen Recording",
            ),
            j(
                375_100,
                "WAITING seq=3 until=x 3 short turn(s) in a row: 8 min back-off",
            ),
            j(
                855_000,
                "CONTINUED seq=3 rule=continue@v1 [aterm harness] Autopilot is Cruise",
            ),
            j(860_000, &format!("EVENT idle seq=4 NEEDS YOU: {NEED_C}")),
            j(
                860_000,
                "NEEDS seq=4 count=2 attention=OK grant Screen Recording",
            ),
        ],
        status: vec![
            status(1_000, "-", "-"),
            status(6_000, "ask", "2"),
            status(131_000, "-", "-"),
            status(376_000, "ask", "1"),
            status(861_000, "ask", "2"),
        ],
        meta: vec![
            no_meta(1_000),
            need(6_000, " (+1 more)"),
            no_meta(131_000),
            need(376_000, ""),
            need(861_000, " (+1 more)"),
        ],
        end_ms: T0 + 875_000,
        window: surfaces::Window {
            front_tab: true,
            chip: vec![j(
                1_000,
                "window=2 program=claude face=cruise drawn=\"∞\" spoken=\"Autopilot: cruise.\"",
            )],
            strip: vec![j(1_000, "index=16 selected=1 states=selected")],
            menu: vec![
                row(6_000, &[NEED_B]),
                j(131_000, ""),
                row(376_000, &[]),
                row(861_000, &[NEED_C]),
            ],
            page: vec![
                page(
                    6_000,
                    &format!("needs you: {NEED_A} (+1 more); carrying on · pausing"),
                ),
                page(131_000, "pausing until 03:28 (a short turn)"),
                page(
                    376_000,
                    &format!("needs you: {NEED_A}; carrying on · pausing"),
                ),
                page(
                    861_000,
                    &format!("needs you: {NEED_A} (+1 more); carrying on · pausing"),
                ),
            ],
            ..window("cruise-n")
        },
        ..Seen::default()
    }
}

#[test]
fn cruise_n_passes_on_its_needs_moving_drawn_as_its_face() {
    assert_eq!(
        verdicts(Chain::CruiseN, &seen_cruise_n(), "claude"),
        [
            ("N1", "PASS"),
            ("N2", "PASS"),
            ("N3", "PASS"),
            ("N4", "PASS")
        ]
    );
}

/// §12.5.6's controls on cruise-n: `drawn=` unequal to `face=` fails each
/// step; a step after a failed one is unobserved, never a pass.
#[test]
fn cruise_n_fails_a_wrong_window_read_and_leaves_a_step_after_a_failed_one_unobserved() {
    let mut s = seen_cruise_n();
    s.window.chip[0].1 = "window=2 program=claude face=cruise drawn=\"⤓ land\" spoken=\"x\"".into();
    for (sc, w) in verdicts(Chain::CruiseN, &s, "claude") {
        assert_eq!(w, "FAIL", "{sc}");
    }
    // the front tab undrawn: a frame never drew it
    let mut s = seen_cruise_n();
    s.window.chip[0].1 = behind("claude", "cruise");
    assert_eq!(one(Chain::CruiseN, &s, "claude", Scenario::N1).0, "FAIL");
    // N4's count wrong
    let mut s = seen_cruise_n();
    s.status[4] = j(
        861_000,
        "task=open needs=ask needs_n=1 autopilot=cruise supervisor=a agent=- end=- end_seen=- end_cheered=-",
    );
    assert_eq!(
        verdicts(Chain::CruiseN, &s, "claude"),
        [
            ("N1", "PASS"),
            ("N2", "PASS"),
            ("N3", "PASS"),
            ("N4", "FAIL")
        ]
    );
    // the records: N4's entry moved to the wrong count, N2's never cleared,
    // N3 asked by a continuation rather than the done check
    let n = |s: &Seen, k| surfaces::judge_needs_step(s, T0, k).word();
    let good = seen_cruise_n();
    assert_eq!((0..4).map(|k| n(&good, k)).collect::<Vec<_>>(), ["PASS"; 4]);
    let mut s = seen_cruise_n();
    for (_, l) in &mut s.journal {
        *l = l.replace("NEEDS seq=4 count=2", "NEEDS seq=4 count=1");
    }
    assert_eq!(n(&s, 3), "FAIL");
    let mut s = seen_cruise_n();
    s.journal.retain(|(_, l)| !l.starts_with("CLEARED "));
    assert_eq!(n(&s, 1), "FAIL");
    let mut s = seen_cruise_n();
    for (_, l) in &mut s.journal {
        *l = l.replace("rule=continue-done-check@v1", "rule=continue@v1");
    }
    assert_eq!(n(&s, 2), "FAIL");
    // its row never on the page's screen: N1 unread; a wrong row beside it
    // still fails
    let mut s = seen_cruise_n();
    for (_, p) in &mut s.window.page {
        *p = "-".into();
    }
    let (w, why) = one(Chain::CruiseN, &s, "claude", Scenario::N1);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("never on the page's screen"), "{why}");
    s.window.menu[0].1 = format!("needs|◇ cruise-n — {NEED_A} · #");
    assert_eq!(one(Chain::CruiseN, &s, "claude", Scenario::N1).0, "FAIL");
    // N2's need row left standing
    let mut s = seen_cruise_n();
    s.window.menu[1].1 = format!("needs|◇ cruise-n — {NEED_A} · #");
    assert_eq!(one(Chain::CruiseN, &s, "claude", Scenario::N2).0, "FAIL");
    // the page alone: N1's state without the others' count, N2's still
    // naming a need
    let page = |state: &str| format!("head=cruise-n: Window 2, tab 17\nstate={state}");
    let mut s = seen_cruise_n();
    s.window.page[0].1 = page(&format!("needs you: {NEED_A}; carrying on · pausing"));
    let (w, why) = one(Chain::CruiseN, &s, "claude", Scenario::N1);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("the page's state reads"), "{why}");
    let mut s = seen_cruise_n();
    s.window.page[1].1 = page(&format!("needs you: {NEED_A} (+1 more); carrying on"));
    let (w, why) = one(Chain::CruiseN, &s, "claude", Scenario::N2);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("the page still names a need"), "{why}");
    // meta's entry alone: N1's naming the second need first, N2's left up
    let mut s = seen_cruise_n();
    s.meta[1].1 = s.meta[1].1.replace(NEED_A, NEED_B);
    let (w, why) = one(Chain::CruiseN, &s, "claude", Scenario::N1);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("wanted needs_n=2 and an entry"), "{why}");
    let mut s = seen_cruise_n();
    s.meta[2] = s.meta[1].clone();
    s.meta[2].0 = T0 + 131_000;
    let (w, why) = one(Chain::CruiseN, &s, "claude", Scenario::N2);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("no needs entry"), "{why}");
    // N1's row as 2a drew it: the others' count and no row one level in
    let mut s = seen_cruise_n();
    s.window.menu[0].1 = format!("needs|◇ cruise-n — {NEED_A} (+1 more) · #");
    let (w, why) = one(Chain::CruiseN, &s, "claude", Scenario::N1);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("no need row"), "{why}");
    assert!(why.contains("wanted [\""), "{why}");
    // the done check never typed: N3 and N4 unreached
    let mut s = seen_cruise_n();
    let before = |v: &mut Vec<(i64, String)>| v.retain(|(t, _)| *t < T0 + 370_000);
    for v in [
        &mut s.journal,
        &mut s.status,
        &mut s.meta,
        &mut s.window.menu,
        &mut s.window.page,
    ] {
        before(v);
    }
    s.end_ms = T0 + 400_000;
    assert_eq!(
        verdicts(Chain::CruiseN, &s, "claude"),
        [
            ("N1", "PASS"),
            ("N2", "PASS"),
            ("N3", "UNOBSERVED"),
            ("N4", "UNOBSERVED")
        ]
    );
    // a landing under Cruise ends nothing it should
    let mut s = seen_cruise_n();
    s.journal.push(j(870_000, "DONE seq=5 rule=land@v1 x"));
    assert_eq!(one(Chain::CruiseN, &s, "claude", Scenario::N4).0, "FAIL");
    // not the front tab: its chip is never drawn, so its drawn= is unread
    let mut s = seen_cruise_n();
    s.window.front_tab = false;
    s.window.chip[0].1 = behind("claude", "cruise");
    assert_eq!(
        one(Chain::CruiseN, &s, "claude", Scenario::N1).0,
        "UNOBSERVED"
    );
}

/// cruise-n's OTHER NEEDS ON ROWS ONE LEVEL IN (slice 2d, §12.5.4's
/// N1–N4 row): each step wants exactly its other needs on rows one level in
/// under its row — (b) at N1, none at N2 and N3, (c) at N4. NEGATIVE
/// CONTROLS, each a FAIL: N4 still drawing (b) under the same first need
/// (a set that moved, drawn stale); N1's (b) missing (a row the menu
/// never drew, or one `chrome` printed as a submenu's, which the reader
/// passes over); N3 with a row one level in that its one need has no
/// reason for.
#[test]
fn cruise_n_wants_each_other_need_on_a_row_one_level_in() {
    let row = |others: &[&str]| {
        let under: String = others
            .iter()
            .map(|other| format!("\nneeds|{other} indent=1"))
            .collect();
        format!("needs|◇ cruise-n — {NEED_A} · #{under}")
    };
    let mut s = seen_cruise_n();
    s.window.menu[3].1 = row(&[NEED_B]);
    let (w, why) = one(Chain::CruiseN, &s, "claude", Scenario::N4);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains(&format!("wanted [{NEED_C:?}]")), "{why}");
    let mut s = seen_cruise_n();
    s.window.menu[0].1 = row(&[]);
    let (w, why) = one(Chain::CruiseN, &s, "claude", Scenario::N1);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains(&format!("wanted [{NEED_B:?}]")), "{why}");
    let mut s = seen_cruise_n();
    s.window.menu[2].1 = row(&[NEED_B]);
    assert_eq!(one(Chain::CruiseN, &s, "claude", Scenario::N3).0, "FAIL");
    // The menu as `chrome` printed it: (b) one level in is read as such; the
    // same row printed as a submenu's is no row of the tab's.
    let chrome = |b: &str| {
        format!(
            "statusmenu \"Needs you (1)\" enabled=no at=0\n\
             statusmenu \"◇ cruise-n — {NEED_A} · 4s\" enabled=yes checked=no at=1\n\
             {b}\n\
             statusmenu --- at=3"
        )
    };
    let read = |b: &str| surfaces::menu_of(&surfaces::menu_rows(&chrome(b)), "cruise-n");
    let one_in = read(&format!(
        "statusmenu {NEED_B:?} enabled=yes checked=no \
         tip=\"Claude Code in cruise-n needs this too\" indent=1 at=2"
    ));
    assert_eq!(
        one_in,
        format!("needs-head|Needs you (1) rows=1\n{}", row(&[NEED_B]))
    );
    let lead = read(&format!(
        "statusmenu   {NEED_B:?} enabled=yes checked=no at=1.0"
    ));
    assert_eq!(
        lead,
        format!("needs-head|Needs you (1) rows=1\n{}", row(&[]))
    );
}

/// A tab behind is judged by its face, never its cells: Cruise's C1 passes
/// on `face=cruise drawn=- why=not-drawn` and fails on a stale face drawn.
#[test]
fn a_tab_behind_passes_on_its_face_with_nothing_drawn() {
    let mut s = cruised();
    s.status = vec![j(
        1_000,
        "task=open needs=- needs_n=- autopilot=cruise supervisor=a agent=- end=- end_seen=- end_cheered=-",
    )];
    s.window = surfaces::Window {
        chip: vec![j(1_000, &behind("claude", "cruise"))],
        strip: vec![j(1_000, "index=10 selected=0 states=-")],
        ..window("cruise")
    };
    assert_eq!(
        verdicts(Chain::Cruise, &s, "claude"),
        [("C1", "PASS"), ("C2", "PASS")]
    );
    let mut stale = s.clone();
    stale.window.chip[0].1 = "window=2 program=claude face=cruise drawn=\"∞\" spoken=\"x\"".into();
    assert_eq!(one(Chain::Cruise, &stale, "claude", Scenario::C1).0, "FAIL");
    // never an end, never a row
    let mut ended = s.clone();
    ended.status.push(j(
        9_000,
        "task=open needs=- needs_n=- autopilot=cruise supervisor=a agent=- end=1792578282 end_seen=0 end_cheered=0",
    ));
    assert_eq!(one(Chain::Cruise, &ended, "claude", Scenario::C1).0, "FAIL");
    let mut row = s.clone();
    row.window.menu.push(j(9_000, "done|✓ cruise — landed #"));
    assert_eq!(one(Chain::Cruise, &row, "claude", Scenario::C1).0, "FAIL");
    let mut need = s;
    need.window.menu.push(j(9_000, "needs|◇ cruise — x · #"));
    assert_eq!(one(Chain::Cruise, &need, "claude", Scenario::C1).0, "FAIL");
}

/// L16 (land-j) as a passing run records it: the land act's defense long,
/// landed 40 s after it, the end standing unseen with its • and its row;
/// END's sweep left it, and so did the look begun on the front tab; under
/// the look, `tab <land-j>` saw land-j's (`look`) and left land-k's
/// (`still`); the press saw land-k's. One fixture for both chains: each
/// judge reads its own steps.
fn seen_land_j(label: &str) -> Seen {
    let status = |t: i64, task: &str, end: &str, seen: &str| {
        j(
            t,
            &format!(
                "task={task} needs=- needs_n=- autopilot=land supervisor=a agent=- end={end} end_seen={seen} end_cheered=0"
            ),
        )
    };
    Seen {
        journal: vec![
            j(5_000, "EVENT idle seq=1 Implemented the parser"),
            j(
                5_100,
                "WAITING seq=1 until=x 1 short turn(s) in a row: 2 min back-off",
            ),
            j(
                125_000,
                "CONTINUED seq=1 rule=land@v1 [aterm harness] Autopilot is Land:",
            ),
            j(165_000, "EVENT idle seq=2 Nothing left: the parser"),
            j(
                165_000,
                "DONE seq=2 rule=land@v1 the worker defended that nothing is left",
            ),
        ],
        ledger: vec![
            point(5_000, 1),
            typed(125_000, "land@v1", LAND),
            task_done(165_000),
        ],
        status: vec![
            hold(status(1_000, "open", "-", "-"), "-", "-", "-"),
            hold(status(126_000, "open", "-", "-"), "-", "0", LAND_SINCE),
            hold(status(166_000, "landed", "1792578282", "0"), "-", "-", "-"),
        ],
        status_reads: vec![T0 + 170_000],
        meta: vec![no_meta(1_000)],
        end_ms: T0 + 186_000,
        window: surfaces::Window {
            hover: vec![j(166_000, "driven by aterm (land, landed)")],
            hover_reads: vec![T0 + 170_000],
            page: vec![j(
                166_000,
                &format!("head={label}: Window 2, tab 2\nstate=landed 03:24"),
            )],
            page_reads: vec![T0 + 170_000],
            chip: vec![
                j(1_000, &behind("claude", "land")),
                j(166_000, &behind("claude", "landed")),
            ],
            strip: vec![
                j(1_000, "index=14 selected=0 states=-"),
                j(166_000, "index=14 selected=0 states=done"),
            ],
            menu: vec![j(166_000, &format!("done|✓ {label} — landed #"))],
            looked: vec![
                j(
                    900_000,
                    "step=sweep end=1792578282 end_seen=0 selected=0 states=done done_row=yes",
                ),
                j(
                    900_500,
                    "step=look-on end=1792578282 end_seen=0 selected=0 states=busy,done done_row=yes",
                ),
                j(
                    901_000,
                    "step=look end=1792578282 end_seen=1 selected=1 states=selected done_row=no",
                ),
                j(
                    901_500,
                    "step=still end=1792578282 end_seen=0 selected=0 states=done done_row=yes",
                ),
                j(
                    902_000,
                    "step=press end=1792578282 end_seen=1 selected=1 states=selected done_row=no pressed=OK",
                ),
            ],
            ..window(label)
        },
        ..Seen::default()
    }
}

#[test]
fn land_j_and_land_k_pass_on_an_end_a_look_and_a_press_see() {
    assert_eq!(
        verdicts(Chain::LandJ, &seen_land_j("land-j"), "claude"),
        [("L16", "PASS")]
    );
    assert_eq!(
        verdicts(Chain::LandK, &seen_land_j("land-k"), "claude"),
        [("L17", "PASS")]
    );
}

/// §12.5.6's controls on L16 and L17.
#[test]
fn land_j_and_land_k_fail_a_wrong_window_read() {
    let l16 = |s: &Seen| one(Chain::LandJ, s, "claude", Scenario::L16);
    let l17 = |s: &Seen| one(Chain::LandK, s, "claude", Scenario::L17);
    // a • still standing after end_seen=1
    let mut s = seen_land_j("land-j");
    s.window.looked[2].1 =
        "step=look end=1792578282 end_seen=1 selected=1 states=selected,done done_row=no".into();
    assert_eq!(l16(&s).0, "FAIL");
    // the row still standing after the look
    let mut s = seen_land_j("land-j");
    s.window.looked[2].1 =
        "step=look end=1792578282 end_seen=1 selected=1 states=selected done_row=yes".into();
    assert_eq!(l16(&s).0, "FAIL");
    // the tab switch left land-j behind
    let mut s = seen_land_j("land-j");
    s.window.looked[2].1 =
        "step=look end=1792578282 end_seen=1 selected=0 states=- done_row=no".into();
    assert_eq!(l16(&s).0, "FAIL");
    // the look begun on another tab saw an end behind it: each chain fails
    /// A scenario's judge of a fixture: its verdict word and its words.
    type Judge = fn(&Seen) -> (&'static str, String);
    let judges: [(&str, Judge); 2] = [("land-j", l16), ("land-k", l17)];
    for (label, judge) in judges {
        let mut s = seen_land_j(label);
        s.window.looked[1].1 =
            "step=look-on end=1792578282 end_seen=1 selected=0 states=busy done_row=no".into();
        let (w, why) = judge(&s);
        assert_eq!(w, "FAIL", "{label}: {why}");
        assert!(why.contains("behind the tab the look began on"), "{why}");
        // its end seen alone, its • and its row still drawn
        let mut s = seen_land_j(label);
        s.window.looked[1].1 =
            "step=look-on end=1792578282 end_seen=1 selected=0 states=busy,done done_row=yes"
                .into();
        assert_eq!(judge(&s).0, "FAIL", "{label}");
        // ...and never read there: unobserved, never a pass
        let mut s = seen_land_j(label);
        s.window.looked.remove(1);
        assert_eq!(judge(&s).0, "UNOBSERVED", "{label}");
    }
    // the tab switch to land-j under the look saw land-k's too
    let mut s = seen_land_j("land-k");
    s.window.looked[3].1 =
        "step=still end=1792578282 end_seen=1 selected=0 states=- done_row=no".into();
    let (w, why) = l17(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("behind land-j"), "{why}");
    let mut s = seen_land_j("land-k");
    s.window.looked.remove(3);
    assert_eq!(l17(&s).0, "UNOBSERVED");
    // the press refused, the rest as a pass reads it
    let mut s = seen_land_j("land-k");
    s.window.looked[4].1 = s.window.looked[4].1.replace("pressed=OK", "pressed=ERR");
    let (w, why) = l17(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("wanted pressed=OK"), "{why}");
    // no Done for you row once landed, its • standing
    let mut s = seen_land_j("land-j");
    s.window.menu.clear();
    let (w, why) = l16(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("yet no Done for you row once landed"), "{why}");
    // the sweep with no look saw the end
    let mut s = seen_land_j("land-j");
    s.window.looked[0].1 =
        "step=sweep end=1792578282 end_seen=1 selected=0 states=- done_row=no".into();
    assert_eq!(l16(&s).0, "FAIL");
    // no end ever marked, or one seen before anyone looked
    let mut s = seen_land_j("land-j");
    s.status[2] = hold(
        j(
            166_000,
            "task=landed needs=- needs_n=- autopilot=land supervisor=a agent=- end=- end_seen=- end_cheered=-",
        ),
        "-",
        "-",
        "-",
    );
    assert_eq!(l16(&s).0, "FAIL");
    let mut s = seen_land_j("land-j");
    s.status[2].1 = s.status[2].1.replace("end_seen=0", "end_seen=1");
    assert_eq!(l16(&s).0, "FAIL");
    // no • once landed
    let mut s = seen_land_j("land-j");
    s.window.strip.pop();
    assert_eq!(l16(&s).0, "FAIL");
    // in front when it landed: the • is never on the front tab, so no pass
    // and no fail on it; behind again, a missing • fails once more
    let mut s = seen_land_j("land-k");
    s.window.strip = vec![
        j(1_000, "index=2 selected=1 states=selected"),
        j(166_000, "index=2 selected=1 states=selected"),
    ];
    let (w, why) = l17(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("never on the front tab"), "{why}");
    s.window.strip[1] = j(100_000, "index=2 selected=0 states=-");
    assert_eq!(l17(&s).0, "FAIL");
    // landed under the floor: the record fails
    let mut s = seen_land_j("land-j");
    s.journal[3].0 = T0 + 140_000;
    s.journal[4].0 = T0 + 140_000;
    let (w, why) = l16(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("under the 30 s floor"), "{why}");
    // the look never ran: unobserved, never a pass
    let mut s = seen_land_j("land-j");
    s.window.looked.truncate(2);
    assert_eq!(l16(&s).0, "UNOBSERVED");
    // L17: the press left the tab behind, or did not take
    let mut s = seen_land_j("land-k");
    s.window.looked[4].1 =
        "step=press end=1792578282 end_seen=1 selected=0 states=- done_row=no pressed=OK".into();
    assert_eq!(l17(&s).0, "FAIL");
    let mut s = seen_land_j("land-k");
    s.window.looked[4].1 =
        "step=press end=1792578282 end_seen=0 selected=0 states=done done_row=yes pressed=no-row"
            .into();
    assert_eq!(l17(&s).0, "FAIL");
}

#[test]
fn land_e_passes_on_its_row_and_this_mac_counting_every_landed_tab() {
    let seen = |rest: Option<&str>| {
        let mut s = tasked();
        s.status = vec![
            st(1_000, "open", "-", "-", "-", "-"),
            st(10_000, "landed", "-", "-", "-", "-"),
        ];
        s.window = surfaces::Window {
            chip: vec![j(1_000, &behind("gemini", ""))],
            strip: vec![j(10_000, "index=5 selected=0 states=done")],
            menu: vec![j(
                10_000,
                "done-head|Done for you (1) rows=1\ndone|✓ land-e — landed #",
            )],
            rest: rest.map(str::to_string),
            ..window("land-e")
        };
        s
    };
    // the rest line as END prints it: This Mac, the tabs under Land, the
    // run's landings and the page's two counts
    let rest = |here: &str, landed: usize, today: &str, every: &str| {
        format!(
            "here=\"{here}\" landed_tabs={landed} landing_tabs=6 landings=3 today=\"{today}\" every=\"{every}\""
        )
    };
    const TODAY: &str = "3 landed today · 2 kept going · 7 land prompts";
    const EVERY: &str = "Every session, 12 sessions: 2 cruising, 6 landing, 3 landed, 1 off";
    let l7 = |s: &Seen| one(Chain::LandE, s, "gemini", Scenario::L7);
    let good = seen(Some(&rest(
        "This Mac — 3 landed · 2 shells",
        3,
        TODAY,
        EVERY,
    )));
    let (w, why) = l7(&good);
    assert_eq!(w, "PASS", "{why}");
    assert!(why.contains("today 3 landed today"), "{why}");
    // its • and its row are its task record's: no fail for them
    assert_eq!(one(Chain::LandE, &good, "gemini", Scenario::L7).0, "PASS");
    // This Mac with no landed count, or another count than the tabs', or
    // 0 of 0 (a run that landed nothing proves no count)
    for (here, landed) in [
        ("This Mac — 2 shells", 3),
        ("This Mac — 2 landed · 2 shells", 3),
        ("This Mac — 0 landed", 0),
    ] {
        let (w, why) = l7(&seen(Some(&rest(here, landed, TODAY, EVERY))));
        assert_eq!(w, "FAIL", "{here}: {why}");
        assert!(why.contains("This Mac reads"), "{why}");
    }
    // no row for it
    let mut s = good.clone();
    s.window.menu.clear();
    assert_eq!(l7(&s).0, "FAIL");
    // END never reached: unobserved; and with no row, a fail all the same
    assert_eq!(l7(&seen(None)).0, "UNOBSERVED");
    let mut s = seen(None);
    s.window.menu.clear();
    let (w, why) = l7(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("no Done for you row"), "{why}");
    // THE PAGE (4c): the today line not led by the run's landings, or by
    // another count; the Every row calling a landed tab landing, or
    // counting fewer
    for (today, every) in [
        ("2 kept going today · 3 landed · 7 land prompts", EVERY),
        ("2 landed today · 2 kept going", EVERY),
        (
            TODAY,
            "Every session, 12 sessions: 2 cruising, 9 landing, 1 off",
        ),
        (
            TODAY,
            "Every session, 12 sessions: 2 cruising, 7 landing, 2 landed, 1 off",
        ),
        // each count alone: the landing ones, then the landed ones
        (
            TODAY,
            "Every session, 12 sessions: 2 cruising, 5 landing, 3 landed, 1 off",
        ),
        (
            TODAY,
            "Every session, 12 sessions: 2 cruising, 6 landing, 2 landed, 1 off",
        ),
    ] {
        let (w, why) = l7(&seen(Some(&rest(
            "This Mac — 3 landed · 2 shells",
            3,
            today,
            every,
        ))));
        assert_eq!(w, "FAIL", "{today} / {every}: {why}");
        assert!(why.contains("beside the landing: FAIL"), "{why}");
    }
    // a page row never read: unobserved, never a pass
    for (today, every) in [("-", EVERY), (TODAY, "-")] {
        let (w, why) = l7(&seen(Some(&rest(
            "This Mac — 3 landed · 2 shells",
            3,
            today,
            every,
        ))));
        assert_eq!(w, "UNOBSERVED", "{today} / {every}: {why}");
    }
    // no landing in the run, yet the page counts one
    let none =
        rest("This Mac — 3 landed · 2 shells", 3, TODAY, EVERY).replace("landings=3", "landings=0");
    assert_eq!(l7(&seen(Some(&none))).0, "FAIL");
}

/// END over a desk at rest and a sweep, as `judge_end` reads them.
fn rest_desk(metrics: &str, n_render: &str) -> surfaces::Desk {
    surfaces::Desk {
        idle: Some(surfaces::Idle {
            from: T0,
            ms: 20_050,
            metrics: metrics.to_string(),
            percentiles: format!("n_render={n_render} render_p50_ms=0.00"),
            moved: Vec::new(),
            relogged: Vec::new(),
            ends: vec!["land-j".into(), "land-k".into()],
            holds: vec!["cruise".into()],
            page_open: true,
        }),
        ..surfaces::Desk::default()
    }
}

#[test]
fn end_passes_on_a_window_at_rest_and_each_chain_drawn_as_its_face() {
    const REST: &str = "timer_wakes=0 event_wakes=3 deadline_owner=presence deadline_arms_by_owner=presence:1/0 deadline_fires_by_owner=none wakes_by_variant=HostNote:2,HarnessFact:1";
    let front = |line: &str| surfaces::Window {
        front: Some((T0, line.to_string())),
        ..window("x")
    };
    let n = front("window=2 program=claude face=cruise drawn=\"∞\" spoken=\"x\"");
    let j_ = front("window=2 program=claude face=landed drawn=\"✓ landed\" spoken=\"x\"");
    let off = front("-");
    let tabs = [
        (Chain::CruiseN, &n),
        (Chain::LandJ, &j_),
        (Chain::Off, &off),
    ];
    let end = |d: &surfaces::Desk, t: &[(Chain, &surfaces::Window)]| {
        surfaces::judge_end(d, t, "claude").word()
    };
    assert_eq!(end(&rest_desk(REST, "0"), &tabs), "PASS");
    // timer_wakes=2 at rest
    assert_eq!(
        end(
            &rest_desk(&REST.replace("timer_wakes=0", "timer_wakes=2"), "0"),
            &tabs
        ),
        "FAIL"
    );
    // a frame composed, a deadline armed or fired beside presence
    assert_eq!(end(&rest_desk(REST, "1"), &tabs), "FAIL");
    assert_eq!(
        end(
            &rest_desk(
                &REST.replace("presence:1/0", "presence:1/0,session_status:2/0"),
                "0"
            ),
            &tabs
        ),
        "FAIL"
    );
    assert_eq!(
        end(
            &rest_desk(
                &REST.replace("fires_by_owner=none", "fires_by_owner=presence:1"),
                "0"
            ),
            &tabs
        ),
        "FAIL"
    );
    // the programs wrote at idle (Muse Code, measured: 16 tabs, 32 Output
    // wakes in 20 s), answered by the status sweep alone: a pass; a timer
    // wake beside its fires, a fire or an arm of another owner, a frame: a
    // fail
    let wrote = "timer_wakes=53 event_wakes=124 deadline_owner=session_status deadline_arms_by_owner=session_status:124/0 deadline_fires_by_owner=session_status:53 wakes_by_variant=ReadSessionStatus:39,Output:32";
    let (w, why) = {
        let v = surfaces::judge_end(&rest_desk(wrote, "0"), &tabs, "muse");
        (v.word(), v.why().to_string())
    };
    assert_eq!(w, "PASS", "{why}");
    assert!(why.contains("32 Output wake(s)"), "{why}");
    assert!(
        why.contains("(53 fire(s), 1.66 a write, at most 3)"),
        "{why}"
    );
    for bent in [
        wrote.replace("timer_wakes=53", "timer_wakes=54"),
        wrote.replace(
            "fires_by_owner=session_status:53",
            "fires_by_owner=session_status:53,presence:1",
        ),
        wrote.replace(
            "arms_by_owner=session_status:124/0",
            "arms_by_owner=session_status:124/0,video:1/0",
        ),
    ] {
        assert_eq!(end(&rest_desk(&bent, "0"), &tabs), "FAIL", "{bent}");
    }
    assert_eq!(end(&rest_desk(wrote, "2"), &tabs), "FAIL");
    // a sweep that re-arms itself while output trickles: its timer wakes all
    // its fires, yet past three a write
    let spun = wrote
        .replace("timer_wakes=53", "timer_wakes=400")
        .replace("session_status:53", "session_status:400")
        .replace("session_status:124/0", "session_status:420/0");
    let v = surfaces::judge_end(&rest_desk(&spun, "0"), &tabs, "muse");
    assert_eq!(v.word(), "FAIL", "{}", v.why());
    assert!(v.why().contains("past 3 a write"), "{}", v.why());
    // three a write is the bound itself: a pass
    let three = wrote
        .replace("timer_wakes=53", "timer_wakes=96")
        .replace("session_status:53", "session_status:96");
    assert_eq!(end(&rest_desk(&three, "0"), &tabs), "PASS");
    // the same with no Output wake: the sweep had nothing to answer
    let unasked = wrote.replace(",Output:32", "");
    assert_eq!(end(&rest_desk(&unasked, "0"), &tabs), "FAIL");
    // an act or a journal line in the window: no rest, unobserved
    let mut d = rest_desk(wrote, "0");
    if let Some(i) = d.idle.as_mut() {
        i.moved
            .push("cruise +3.0 s CONTINUED seq=9 rule=continue@v1".into());
    }
    assert_eq!(end(&d, &tabs), "UNOBSERVED");
    // a chain in front drawn as another face
    let stale = front("window=2 program=claude face=landed drawn=\"⤓ land\" spoken=\"x\"");
    assert_eq!(
        end(
            &rest_desk(REST, "0"),
            &[(Chain::CruiseN, &n), (Chain::LandJ, &stale)]
        ),
        "FAIL"
    );
    // never brought in front
    let never = window("y");
    assert_eq!(
        end(&rest_desk(REST, "0"), &[(Chain::LandA, &never)]),
        "FAIL"
    );
    // Gemini CLI: its no-footer line in front
    let g = front("window=2 program=gemini face=- drawn=- why=no-footer spoken=-");
    assert_eq!(
        surfaces::judge_end(&rest_desk(REST, "0"), &[(Chain::CruiseN, &g)], "gemini").word(),
        "PASS"
    );
    assert_eq!(
        surfaces::judge_end(&rest_desk(REST, "0"), &[(Chain::CruiseN, &n)], "gemini").word(),
        "FAIL"
    );
    // no idle window, or no end standing in it
    assert_eq!(end(&surfaces::Desk::default(), &tabs), "UNOBSERVED");
    let mut d = rest_desk(REST, "0");
    if let Some(i) = d.idle.as_mut() {
        i.ends.clear();
    }
    assert_eq!(end(&d, &tabs), "UNOBSERVED");
    // the page not open in it
    let mut d = rest_desk(REST, "0");
    if let Some(i) = d.idle.as_mut() {
        i.page_open = false;
    }
    let v = surfaces::judge_end(&d, &tabs, "claude");
    assert_eq!(v.word(), "UNOBSERVED", "{}", v.why());
    assert!(v.why().contains("the page was not open"), "{}", v.why());
    // no pause standing in it, where the run holds a chain that pauses
    // (cruise-n); a run of none (land-j, land-k and the Off control) wants
    // none
    let mut d = rest_desk(REST, "0");
    if let Some(i) = d.idle.as_mut() {
        i.holds.clear();
    }
    let v = surfaces::judge_end(&d, &tabs, "claude");
    assert_eq!(v.word(), "UNOBSERVED", "{}", v.why());
    assert!(v.why().contains("no hold stood in it"), "{}", v.why());
    let k = front("window=2 program=claude face=landed drawn=\"✓ landed\" spoken=\"x\"");
    let unpaused = [(Chain::LandJ, &j_), (Chain::LandK, &k), (Chain::Off, &off)];
    let v = surfaces::judge_end(&d, &unpaused, "claude");
    assert_eq!(v.word(), "PASS", "{}", v.why());
    assert!(
        v.why().contains("no chain of the run pauses"),
        "{}",
        v.why()
    );
}

/// Every chain of the measured pass is held to the window, and every
/// scenario §12.5.4 gives 5c has a want of its own.
#[test]
fn every_chain_and_scenario_of_the_pass_has_its_window_wants() {
    let wanted = |c: Chain, s: Scenario| {
        let mut seen = Seen::default();
        seen.window.read = false;
        let cx = surfaces::Ctx {
            chain: c,
            program: "claude",
            t0: T0,
        };
        surfaces::with_window(s, &seen, &cx, Verdict::Pass(String::new()))
            .why()
            .contains("SKIPPED")
    };
    for c in Chain::ALL {
        assert!(wanted(c, c.scenarios()[0]), "{c:?}");
    }
    for (c, s) in [
        (Chain::LandA, Scenario::L1),
        (Chain::LandA, Scenario::L3),
        (Chain::LandA, Scenario::L5),
        (Chain::LandE, Scenario::L7),
        (Chain::LandJ, Scenario::L16),
        (Chain::LandK, Scenario::L17),
        (Chain::CruiseN, Scenario::N1),
        (Chain::CruiseN, Scenario::N2),
        (Chain::CruiseN, Scenario::N3),
        (Chain::CruiseN, Scenario::N4),
    ] {
        assert!(wanted(c, s), "{s:?}");
    }
    // a coverage chain is held to none (not judged on the window in 5c)
    assert!(!wanted(Chain::LandQ, Scenario::L12));
}
