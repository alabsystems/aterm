// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn want() -> Vec<WantInstance> {
    vec![
        WantInstance {
            i: 0,
            pid: 100,
            port: 7100,
            local: true,
            sessions: vec![WantSession {
                sid: "s-aaa".into(),
                program: "zsh".into(),
                agent: "-".into(),
            }],
        },
        WantInstance {
            i: 1,
            pid: 101,
            port: 7101,
            local: false,
            sessions: vec![
                WantSession {
                    sid: "s-bbb".into(),
                    program: "claude".into(),
                    agent: "busy".into(),
                },
                WantSession {
                    sid: "s-ccc".into(),
                    program: "build-loop".into(),
                    agent: "-".into(),
                },
            ],
        },
    ]
}

/// The client's ranks of the Claude sessions the tests make, read from
/// their agent words: busy `s-bbb`, asking `s-ddd`.
fn ranked() -> BTreeMap<String, u8> {
    [("s-bbb", "busy"), ("s-ddd", "prompt")]
        .into_iter()
        .map(|(sid, agent)| (sid.to_owned(), aterm_ctl::survey_rank([("agent", agent)])))
        .collect()
}

const GOOD: &str = "\
== mac pid 100 0.105.0 · 1 sessions trust=screen
s-aaa (you) zsh idle | /tmp/af.x/m00/home
== fleet 127.0.0.1:7101 (host mac) pid 101 0.105.0 · 2 sessions (1 busy) trust=screen
s-bbb claude busy 4s Bash | /tmp/af.x/m01/home
    asked: Fix the flaky test
    said: Found it
s-ccc running build-loop | build-loop 1 | /tmp/af.x/m01/home
";

#[test]
fn a_healthy_read_passes_every_check() {
    let checks = judge_text(GOOD, &want(), 24_000, "mac", true, &ranked());
    assert!(checks.iter().all(|c| c.ok), "{checks:#?}");
}

#[test]
fn a_missing_block_a_wrong_program_and_a_lost_excerpt_are_each_named() {
    let bad = GOOD
        .replace("s-ccc running build-loop", "s-ccc zsh idle")
        .replace("    said: Found it\n", "");
    let checks = judge_text(&bad, &want(), 24_000, "mac", true, &ranked());
    let failed: Vec<&str> = checks
        .iter()
        .filter(|c| !c.ok)
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(
        failed,
        [
            "every session row shows its program (and agent word)",
            "every Claude session's excerpts (asked, said)",
            "each machine's most pressing Claude session keeps its words"
        ],
        "{checks:#?}"
    );
    let cut = GOOD.replace(
        "· 2 sessions (1 busy) trust=screen",
        "· 2 sessions… trust=screen",
    );
    let failed: Vec<String> = judge_text(&cut, &want(), 24_000, "mac", true, &ranked())
        .into_iter()
        .filter(|c| !c.ok)
        .map(|c| c.name)
        .collect();
    assert_eq!(
        failed,
        ["each header carries its session and state counts whole"]
    );
    let short: String = GOOD.lines().take(2).map(|l| format!("{l}\n")).collect();
    let checks = judge_text(&short, &want(), 24_000, "mac", true, &ranked());
    assert!(
        checks
            .iter()
            .any(|c| c.name == "one header block per instance" && !c.ok)
    );
    assert!(
        !judge_text(GOOD, &want(), 100, "mac", true, &ranked())[0].ok,
        "over the budget"
    );
}

/// A READ THE BUDGET CUT is held to the client's fill order, its marks and
/// its budget: the asking session's excerpts whole before the busy one
/// shows a word; in the round robin, the first session whole before the
/// second shows a word, and every `said:` of a round before any `asked:`;
/// each session short of an excerpt marked, the closing line counting the
/// marks; the budget used to within one excerpt line.
#[test]
fn a_cut_read_is_held_to_the_order_the_client_fills_in() {
    let mut want = want();
    want[1].sessions.push(WantSession {
        sid: "s-ddd".into(),
        program: "claude".into(),
        agent: "prompt".into(),
    });
    want[1].sessions.push(WantSession {
        sid: "s-eee".into(),
        program: "claude".into(),
        agent: "idle".into(),
    });
    let mut ranks = ranked();
    ranks.insert("s-eee".into(), aterm_ctl::survey_rank([("agent", "idle")]));
    let both = GOOD
        .replace(
            "· 2 sessions (1 busy) trust=screen",
            "· 4 sessions (1 asking, 1 busy) trust=screen",
        )
        .replace(
            "s-ccc running build-loop",
            "s-ddd claude asking 9s Bash | /tmp/af.x/m01/home\n    asked: Delete it?\n    \
             said: Shall I?\ns-eee claude idle 9s | /tmp/af.x/m01/home\n    asked: Sum it\n    \
             said: It is 42\ns-ccc running build-loop",
        );
    let mark = |text: &str, sid: &str| {
        text.lines()
            .map(|l| match l.split_once(" | ") {
                Some((facts, rest)) if l.starts_with(sid) => {
                    format!("{facts} {} | {rest}\n", aterm_ctl::EXCERPTS_CUT)
                }
                _ => format!("{l}\n"),
            })
            .collect::<String>()
    };
    let cut = |text: &str, marked: usize| {
        let what = if marked == 1 {
            "1 session's".to_owned()
        } else {
            format!("{marked} sessions'")
        };
        format!(
            "{text}{}{what} excerpts (marked {}); --budget, --sid <s> or --json for more\n",
            aterm_ctl::LEFT_OUT_LEAD,
            aterm_ctl::EXCERPTS_CUT
        )
    };
    let failed = |text: &str, ranks: &BTreeMap<String, u8>, spare: usize| -> Vec<String> {
        judge_text(
            text,
            &want,
            text.chars().count() + spare,
            "mac",
            false,
            ranks,
        )
        .into_iter()
        .filter(|c| !c.ok)
        .map(|c| c.name)
        .collect()
    };
    const ORDER: &str = "every Claude session's excerpts (asked, said)";
    const MARKS: &str =
        "every cut is marked, and the closing line counts the marks and the +N lines";
    let none: Vec<String> = Vec::new();
    assert_eq!(failed(&both, &ranks, 0), none, "whole: every excerpt");
    let idle_short = cut(&mark(&both.replace("    asked: Sum it\n", ""), "s-eee"), 1);
    assert_eq!(failed(&idle_short, &ranks, 10), none, "{idle_short}");
    let asking_kept = cut(
        &mark(
            &mark(
                &both
                    .replace("    asked: Fix the flaky test\n    said: Found it\n", "")
                    .replace("    asked: Sum it\n    said: It is 42\n", ""),
                "s-bbb",
            ),
            "s-eee",
        ),
        2,
    );
    assert_eq!(failed(&asking_kept, &ranks, 10), none, "{asking_kept}");
    assert_eq!(
        failed(&asking_kept, &BTreeMap::new(), 10),
        [
            "every session row shows its program (and agent word)",
            ORDER
        ]
    );
    let reverse = cut(
        &mark(
            &both.replace("    asked: Delete it?\n    said: Shall I?\n", ""),
            "s-ddd",
        ),
        1,
    );
    assert_eq!(failed(&reverse, &ranks, 10), [ORDER], "{reverse}");
    let second_first = cut(
        &mark(
            &both.replace("    asked: Fix the flaky test\n", ""),
            "s-bbb",
        ),
        1,
    );
    let second_first = second_first.replace("    asked: Sum it\n", "");
    assert!(
        failed(&second_first, &ranks, 10).contains(&ORDER.to_owned()),
        "{second_first}"
    );
    let unmarked = cut(&both.replace("    asked: Sum it\n", ""), 1);
    assert_eq!(failed(&unmarked, &ranks, 10), [MARKS], "{unmarked}");
    assert_eq!(
        failed(&idle_short, &ranks, aterm_ctl::EXCERPT_CHARS + 1),
        ["the budget is used, to within one excerpt line"]
    );
    assert_eq!(failed(&idle_short, &ranks, aterm_ctl::EXCERPT_CHARS), none);
}

/// IN A ROUND OF THE ROUND ROBIN every machine's session shows its
/// `said:` before any shows its `asked:`.
#[test]
fn in_a_round_every_said_comes_before_any_asked() {
    let seen = |asked: bool, said: bool| Seen {
        rank: Some(2),
        round: Some(0),
        asked,
        said,
        marked: !(asked && said),
    };
    assert!(filled_in_order(&[seen(true, true), seen(false, true)]));
    assert!(filled_in_order(&[seen(false, true), seen(false, true)]));
    assert!(!filled_in_order(&[seen(true, true), seen(true, false)]));
    assert!(!filled_in_order(&[seen(true, false), seen(false, false)]));
}

/// An excerpt line of `width` characters, cut keeping its head (or, with
/// `end`, its end).
fn cut_line(word: &str, width: usize, end: bool) -> String {
    let head = format!("    {word}: ");
    let fill = "x".repeat(width - head.chars().count() - 1);
    if end {
        format!("{head}…{fill}")
    } else {
        format!("{head}{fill}…")
    }
}

/// THE NARROWING IS HELD TO THE CLIENT'S RULE: every cut line one width, no
/// narrower than the floor, and the widest that fits — one character wider
/// is over the budget.
#[test]
fn the_narrowing_is_held_to_the_widest_width_that_fits() {
    let at = |width: usize| {
        format!(
            "== mac pid 100 · 1 sessions trust=screen
s-bbb claude busy 4s
{}
{}
                     said: whole
",
            cut_line("asked", width, false),
            cut_line("said", width, true)
        )
    };
    let text = at(200);
    let chars = text.chars().count();
    assert!(
        judge_narrowing(&text, chars + 1, false).ok,
        "one wider is over"
    );
    assert!(
        !judge_narrowing(&text, chars + 2, false).ok,
        "one wider fits: not the widest"
    );
    let floor = at(aterm_ctl::EXCERPT_CHARS);
    assert!(judge_narrowing(&floor, floor.chars().count(), true).ok);
    assert!(
        !judge_narrowing(&text, chars, true).ok,
        "cut past narrowing, yet above the floor"
    );
    let under = at(aterm_ctl::EXCERPT_CHARS - 1);
    assert!(!judge_narrowing(&under, under.chars().count(), false).ok);
    let unlike = text.replace(&cut_line("said", 200, true), &cut_line("said", 199, true));
    assert!(
        !judge_narrowing(&unlike, unlike.chars().count(), false).ok,
        "two widths"
    );
    assert!(judge_narrowing(GOOD, 24_000, false).ok, "nothing cut");
}

/// A FINISHED TURN'S CUT `said:` KEEPS ITS END, any other its head, as the
/// client cuts them.
#[test]
fn a_finished_turns_cut_said_keeps_its_end() {
    let named = |text: &str, want: &[WantInstance]| {
        judge_text(text, want, 24_000, "mac", true, &ranked())
            .into_iter()
            .find(|c| c.name.starts_with("a finished turn's cut said"))
            .expect("the end check")
    };
    let busy_head = GOOD.replace("    said: Found it", "    said: Found it and more…");
    assert!(named(&busy_head, &want()).ok);
    let busy_end = GOOD.replace("    said: Found it", "    said: …and Found it");
    assert!(!named(&busy_end, &want()).ok);
    let mut idle = want();
    idle[1].sessions[0].agent = "idle".into();
    let idle_end = busy_end.replace("s-bbb claude busy 4s Bash", "s-bbb claude idle 4s");
    let check = named(&idle_end, &idle);
    assert!(check.ok && check.detail.starts_with('1'), "{check:?}");
    let idle_head = busy_head.replace("s-bbb claude busy 4s Bash", "s-bbb claude idle 4s");
    assert!(!named(&idle_head, &idle).ok);
}

#[test]
fn a_peers_block_is_found_by_its_pid() {
    let block = block_text(GOOD, 101).expect("m01's block");
    assert!(block.starts_with("== fleet 127.0.0.1:7101 "), "{block}");
    assert!(block.contains("    said: Found it\n"), "{block}");
    assert!(!block.contains("s-aaa"), "{block}");
    assert_eq!(block_text(GOOD, 102), None);
}

#[test]
fn a_raw_escape_or_a_forged_label_fails_the_escaping() {
    let ok = "x trust\\=human \\x1b[31m \\u{202e}\n";
    assert!(judge_escaped(ok, &["trust\\=human", "\\x1b[31m"], &["trust=human"]).ok);
    assert!(!judge_escaped("x \u{1b}[31m\n", &[], &[]).ok);
    assert!(!judge_escaped("x \u{202e}\n", &[], &[]).ok);
    assert!(!judge_escaped("x trust=human\n", &[], &["trust=human"]).ok);
    assert!(
        !judge_escaped(ok, &["\\x07"], &[]).ok,
        "an escape not found"
    );
}

#[test]
fn the_fault_rows_are_one_each_with_their_reasons() {
    let text = "\
== mac pid 100 0.105.0 · 1 sessions trust=screen
s-aaa zsh idle | ~
== 127.0.0.1:7102 did not answer: connected, but no fleet handshake within 3.0s: an aterm stopped trust=screen
== 127.0.0.1:7103 did not answer: nothing listens on 127.0.0.1:7103: no aterm running there trust=screen
";
    let checks = judge_faults(text, 1, 7102, 7103);
    assert!(checks.iter().all(|c| c.ok), "{checks:#?}");
    let checks = judge_faults(text, 1, 7103, 7102);
    assert!(checks.iter().filter(|c| !c.ok).count() == 2, "{checks:#?}");
    assert_eq!(header_pids(text), [100]);
    assert_eq!(unanswered_rows(text, 7103).len(), 1);
    assert!(unanswered_rows(text, 7104).is_empty());
}

#[test]
fn a_facts_reply_with_a_content_field_or_a_typed_title_fails() {
    let facts = r#"[{"label":"x","fleet":true,"instance":{"pid":"7","level":"facts"},"sessions":[{"sid":"s-1","program":"zsh","attention":"-"}]}]"#;
    assert!(judge_facts_only(facts, 7, &["my secret title"]).ok);
    let leak = facts.replace(r#""attention":"-""#, r#""attention":"-","title":"t""#);
    assert!(!judge_facts_only(&leak, 7, &[]).ok);
    let typed = facts.replace("zsh", "my secret title");
    assert!(!judge_facts_only(&typed, 7, &["my secret title"]).ok);
    assert!(!judge_facts_only(facts, 8, &[]).ok, "another instance");
}

#[test]
fn the_json_judge_finds_each_session_and_its_excerpt() {
    let json = r#"[{"label":"pid 100","instance":{"pid":"100","level":"content"},"sessions":[{"sid":"s-aaa","program":"zsh","program_kind":"shell","agent":"-","cwd":"~"}]},{"label":"127.0.0.1:7101","fleet":true,"instance":{"pid":"101","level":"content"},"sessions":[{"sid":"s-bbb","program":"claude","agent":"busy","prompt":"p","said":"s"},{"sid":"s-ccc","program":"build-loop","agent":"-","cwd":"~"}]}]"#;
    let checks = judge_json(json, &want());
    assert!(checks.iter().all(|c| c.ok), "{checks:#?}");
    let checks = judge_json(&json.replace(r#""said":"s""#, r#""said":"-""#), &want());
    assert!(checks.iter().any(|c| !c.ok), "{checks:#?}");
    let ranks = ranks(json);
    assert_eq!(ranks.get("s-bbb"), ranked().get("s-bbb"));
    assert!(ranks["s-bbb"] < ranks["s-ccc"] && ranks["s-ccc"] < ranks["s-aaa"]);
}
