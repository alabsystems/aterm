// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::phase::{Phase, busy_signal, last_said_row, survey_open, transcript_end, worker_phase};

const RULE: &str =
    "──────────────────────────────────────────────────────────────────────────────────";
const FOOTER: &str = "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents";

fn r(rows: &[&str]) -> Vec<String> {
    rows.iter().map(|s| (*s).to_string()).collect()
}

/// A screen: `above` rows, the composer frame under them.
fn screen(above: &[&str]) -> Vec<String> {
    let mut v = r(above);
    v.extend(r(&[RULE, "❯", RULE, FOOTER]));
    v
}

const SESSION: [&str; 3] = [
    "",
    "● How is Claude doing this session? (optional)",
    "  1: Bad    2: Fine   3: Good   0: Dismiss",
];
const LONG: [&str; 3] = [
    "",
    "● How well is Claude following the instructions you gave earlier in this conversation? (optional)",
    "  1: Bad    2: Fine   3: Good   0: Dismiss",
];
const PLUGIN: [&str; 4] = [
    "",
    "● How helpful has the gh plugin been? (optional)",
    "  Used this session: the skill, the command",
    "  1: Bad    2: Fine   3: Good   4: Unsure 0: Dismiss",
];
const MEMORY: [&str; 8] = [
    "",
    "● Claude recalled a memory:",
    "",
    "  The owner prefers small commits and reviews each diff before it lands.",
    "",
    "  How was Claude's recollection? (optional)",
    "  1: Bad    2: Fine   3: Good   4: Unsure 0: Dismiss",
    "",
];
const MEMORY_PLAIN: [&str; 3] = [
    "",
    "● How well did Claude use its memory? (optional)",
    "  1: Bad    2: Fine   3: Good   4: Unsure 0: Dismiss",
];
const DRAFTS: [&str; 6] = [
    "",
    "✻ Bug report drafted: the stand-in title of a queued draft",
    "│ details preview line one",
    "│ details preview line two",
    "1 to review · 2 to send · 0 to dismiss · +1 more queued",
    "",
];

/// The transcript-share ask and the web-setup offer.
const ASK: [&str; 4] = [
    "",
    "⏺ Can Anthropic look at your session transcript to help us improve Claude Code?",
    "  https://code.claude.com/docs/en/data-usage#session-quality-surveys",
    "  y: Yes    n: No     d: Don't ask again",
];
const WEB: [&str; 3] = [
    "",
    "⏺ Pushed to GitHub. Connect GitHub to claude.ai so you can work on this repo even when this machine is offline?",
    "  1: Yes, run /web-setup   2: Not now   3: Don't ask again",
];

fn with(body: &[&str], card: &[&str]) -> Vec<String> {
    let mut above = body.to_vec();
    above.extend_from_slice(card);
    screen(&above)
}

const DONE: [&str; 3] = [
    "⏺ Merged and pushed.",
    "",
    "✻ Cooked for 52m 54s · done 12:31 PM",
];
const WAITING: [&str; 3] = [
    "⏺ Workflow(Second review pass)",
    "  ⎿  Allowed by auto mode classifier",
    "✻ Waiting for 1 dynamic workflow to finish",
];
const SPINNER: [&str; 3] = [
    "⏺ Running 1 shell command…",
    "✽ Ebbing… (3m 2s · ↓ 10.8k tokens)",
    "  ⎿  Tip: Use /btw to ask a quick side question",
];

/// No card, recognised or not, hides the status row: a busy worker never reads idle.
#[test]
fn no_card_turns_a_busy_worker_idle() {
    for card in [
        &SESSION[..],
        &LONG[..],
        &PLUGIN[..],
        &MEMORY[..],
        &MEMORY_PLAIN[..],
    ] {
        for body in [&WAITING[..], &SPINNER[..]] {
            let rows = with(body, card);
            assert!(busy_signal(&rows).is_some(), "{card:?} over {body:?}");
            assert_eq!(worker_phase(&rows), Phase::Busy, "{card:?} over {body:?}");
        }
        let idle = with(&DONE, card);
        assert_eq!(worker_phase(&idle), Phase::Idle);
        assert_eq!(last_said_row(&idle), Some("⏺ Merged and pushed."));
        assert_eq!(transcript_end(&idle), 3, "{card:?}");
    }
    for card in [&DRAFTS[..], &ASK[..], &WEB[..]] {
        for body in [&WAITING[..], &SPINNER[..]] {
            let rows = with(body, card);
            assert_eq!(worker_phase(&rows), Phase::Busy, "{card:?} over {body:?}");
            assert!(!survey_open(&rows), "{card:?} is no survey");
        }
        let idle = with(&DONE, card);
        assert_eq!(worker_phase(&idle), Phase::Idle);
        assert_eq!(
            last_said_row(&idle),
            Some("⏺ Merged and pushed."),
            "{card:?}"
        );
        assert_eq!(transcript_end(&idle), 3, "{card:?}");
    }
}

/// The hand-built 2.1.284 screens, one per kind, are read whole: the card's kind and
/// guard, the worker's phase under it, what it last said, where the transcript ends.
#[test]
fn every_kind_of_fixture_is_read_whole() {
    use crate::prompt::fixtures::screen;
    for (text, kind, guard, phase) in [
        (
            include_str!("fixtures/claude-2.1.284-survey-session-busy-workflow.txt"),
            Some(SurveyKind::Session),
            "^●.How.is.Claude.doing",
            Phase::Busy,
        ),
        (
            include_str!("fixtures/claude-2.1.284-survey-long-context.txt"),
            Some(SurveyKind::LongContext),
            "^●.How.well.is.Claude",
            Phase::Idle,
        ),
        (
            include_str!("fixtures/claude-2.1.284-survey-long-context-busy-spinner.txt"),
            Some(SurveyKind::LongContext),
            "^●.How.well.is.Claude",
            Phase::Busy,
        ),
        (
            include_str!("fixtures/claude-2.1.284-survey-plugin.txt"),
            Some(SurveyKind::Plugin),
            "^●.How.helpful.has.the",
            Phase::Idle,
        ),
        (
            include_str!("fixtures/claude-2.1.284-survey-memory-cited.txt"),
            Some(SurveyKind::Memory),
            "^●.Claude.recalled.a.memory:",
            Phase::Idle,
        ),
        (
            include_str!("fixtures/claude-2.1.284-survey-memory-plain-busy-workflow.txt"),
            Some(SurveyKind::Memory),
            "^●.How.well.did.Claude",
            Phase::Busy,
        ),
        (
            include_str!("fixtures/claude-2.1.284-drafts-card.txt"),
            None,
            "",
            Phase::Idle,
        ),
        (
            include_str!("fixtures/claude-2.1.284-drafts-card-busy-workflow.txt"),
            None,
            "",
            Phase::Busy,
        ),
        (
            include_str!("fixtures/claude-2.1.284-share-ask-busy-workflow.txt"),
            None,
            "",
            Phase::Busy,
        ),
        (
            include_str!("fixtures/claude-2.1.284-share-ask-idle.txt"),
            None,
            "",
            Phase::Idle,
        ),
        (
            include_str!("fixtures/claude-2.1.284-web-setup-busy-workflow.txt"),
            None,
            "",
            Phase::Busy,
        ),
    ] {
        let rows = screen(text);
        let found = survey_block(&rows);
        assert_eq!(found.as_ref().map(|b| b.kind), kind, "{text:.80}");
        if let Some(b) = found {
            assert_eq!(b.guard, guard);
        } else {
            assert!(
                drafts_card(&rows).is_some() || ask_card(&rows).is_some(),
                "the drafts or an ask card: {text:.80}"
            );
        }
        assert_eq!(worker_phase(&rows), phase, "{text:.80}");
        assert_eq!(survey_open(&rows), kind.is_some());
        let said = last_said_row(&rows).expect("a last said row");
        assert!(
            !said.starts_with('●') && !said.contains("Dismiss"),
            "{said}"
        );
        let end = transcript_end(&rows);
        let (first, _) = parked_span(&rows).expect("a parked block");
        assert!(end <= first, "{end} <= {first}");
    }
}

/// A person's `1`/`2`/`3` replaces the card with its aftermath, parked like the card and
/// never what the worker last said, also under the `Interrupted` notice an Esc leaves.
#[test]
fn the_ratings_aftermath_is_parked_and_never_what_the_worker_said() {
    use crate::prompt::fixtures::screen;
    const REPLY: &str = "⏺ Hi! I am the local lab stub, not a model. (reply 1)";
    const INTERRUPTED: &str = "  ⎿  Interrupted · What should Claude do instead?";
    for (text, span) in [
        (
            include_str!("fixtures/claude-2.1.286-survey-after-1-pending.txt"),
            (11, 11),
        ),
        (
            include_str!("fixtures/claude-2.1.286-survey-after-1-thanks.txt"),
            (11, 12),
        ),
    ] {
        let done = screen(text);
        assert_eq!(
            rating_aftermath(&Transcript::of(&done)),
            Some(span),
            "{text:.90}"
        );
        assert!(!survey_open(&done));
        assert_eq!(worker_phase(&done), Phase::Idle);
        assert_eq!(last_said_row(&done), Some(REPLY));
        let mut esc = done.clone();
        esc[9] = INTERRUPTED.to_string();
        assert_eq!(rating_aftermath(&Transcript::of(&esc)), Some(span));
        assert_eq!(last_said_row(&esc), Some(INTERRUPTED), "{text:.90}");
        assert!(transcript_end(&esc) <= span.0, "{text:.90}");
        let quoted: Vec<String> = esc
            .iter()
            .enumerate()
            .map(|(i, r)| {
                if (span.0..=span.1).contains(&i) {
                    format!("  {r}")
                } else {
                    r.clone()
                }
            })
            .collect();
        assert_eq!(rating_aftermath(&Transcript::of(&quoted)), None);
        assert_eq!(
            last_said_row(&quoted),
            Some(quoted[span.1].as_str()),
            "{text:.90}"
        );
        let mut other = esc.clone();
        other[span.0] = "Feedback: Great · Esc to undo".to_string();
        assert_eq!(
            rating_aftermath(&Transcript::of(&other)),
            None,
            "{text:.90}"
        );
    }
}

#[test]
fn a_future_card_with_the_same_shape_is_a_card() {
    let rows = with(
        &DONE,
        &[
            "",
            "● How likely are you to recommend this mode? (optional)",
            "  1: Bad    2: Fine   3: Good   0: Dismiss",
        ],
    );
    let b = survey_block(&rows).expect("the shape is the contract");
    assert_eq!(b.kind, SurveyKind::Unknown);
    assert_eq!(b.guard, "^●.How.likely.are.you");
    assert!(b.dismissable);
    let plain = with(
        &DONE,
        &[
            "",
            "● How likely are you to recommend this mode?",
            "  1: Bad    2: Fine   3: Good   0: Dismiss",
        ],
    );
    let b = survey_block(&plain).expect("read through");
    assert!(!b.dismissable);
    assert_eq!(worker_phase(&plain), Phase::Idle);
}

#[test]
fn a_question_the_wire_cannot_carry_gets_no_guard() {
    let rows = with(
        &DONE,
        &[
            "",
            "● Claude's recall of a memory (optional)",
            "  1: Bad    2: Fine   3: Good   0: Dismiss",
        ],
    );
    let b = survey_block(&rows).expect("the shape is a card");
    assert_eq!(b.kind, SurveyKind::Unknown);
    assert_eq!(b.guard, "");
    let rows = with(
        &DONE,
        &[
            "",
            "● How was Claude's recall (optional)",
            "  1: Bad    2: Fine   3: Good   0: Dismiss",
        ],
    );
    assert_eq!(survey_block(&rows).expect("card").guard, "");
}

#[test]
fn copies_and_lookalikes_are_not_open() {
    for above in [
        r(&[
            "⏺ Bash(cat survey-open.txt)",
            "  ⎿  ● How is Claude doing this session? (optional)",
            "       1: Bad    2: Fine   3: Good   0: Dismiss",
            "",
        ]),
        r(&[
            "⏺ Claude Code parks this above the composer:",
            "  ● How is Claude doing this session? (optional)",
            "    1: Bad    2: Fine   3: Good   0: Dismiss",
            "",
        ]),
        r(&["", "● How is Claude doing this session? (optional)", ""]),
        r(&["⏺ done", "", "  1: Bad    2: Fine   3: Good   0: Dismiss"]),
        r(&[
            "",
            "● How is Claude doing this session? (optional)",
            "  1: Bad    2: Fine   3: Good   0: Dismiss",
            "  I left it for you to answer.",
        ]),
        r(&[
            "● How is Claude doing this session? (optional)",
            "⏺ text",
            "  1: Bad    2: Fine   3: Good   0: Dismiss",
        ]),
        r(&[
            "⏺ How is Claude doing this session? (optional)",
            "  1: Bad    2: Fine   3: Good   0: Dismiss",
        ]),
        r(&[
            SESSION[1],
            SESSION[2],
            "",
            "✻ Cogitated for 4s · done 2:41 PM",
            "",
        ]),
    ] {
        let rows = screen(&above.iter().map(String::as_str).collect::<Vec<_>>());
        assert!(survey_block(&rows).is_none(), "{above:?}");
        assert!(!survey_open(&rows), "{above:?}");
    }
}

/// A card Ink wrapped is still the card.
#[test]
fn a_card_ink_wrapped_is_still_the_card() {
    let narrow = |above: &[&str], width: usize, footer: &str| {
        let rule = "─".repeat(width);
        let mut v = r(above);
        v.extend(r(&[&rule, "❯", &rule, footer]));
        v
    };
    let idle = narrow(
        &[
            "⏺ Done with the change.",
            "",
            "● How is Claude doing this session?",
            "  (optional)",
            "  1: Bad  2: Fine 3: Good 0:",
            "                          Dismiss",
            "",
        ],
        36,
        "  ⏵⏵ auto mode on (shift+tab to",
    );
    let b = survey_block(&idle).expect("the 36-column card");
    assert_eq!(
        (b.kind, b.head, b.options, b.dismissable),
        (SurveyKind::Session, 2, 5, true)
    );
    assert_eq!(b.guard, "^●.How.is.Claude.doing");
    assert!(survey_open(&idle));
    assert_eq!(last_said_row(&idle), Some("⏺ Done with the change."));

    let busy = narrow(
        &[
            "⏺ Working.",
            "",
            "✶ Deliberating… (12s)",
            "",
            "● How is Claude doing this",
            "  session? (optional)",
            "  1: Bad 2: Fine3: Good0:",
            "                       Dismiss",
            "",
        ],
        30,
        "  ⏵⏵ auto mode on (shift+tab",
    );
    assert!(survey_open(&busy));
    assert_eq!(
        survey_block(&busy).map(|b| b.kind),
        Some(SurveyKind::Session)
    );
    assert_eq!(worker_phase(&busy), Phase::Busy);
    assert_eq!(
        busy_signal(&busy).map(|s| s.to_string()),
        Some("status row: spinner".to_string())
    );

    let plugin = narrow(
        &[
            "⏺ Working.",
            "",
            "✶ Deliberating… (12s)",
            "",
            "● How helpful has the lab plugin been?",
            "  (optional)",
            "  Used this session: lab-tool",
            "  1: Bad  2: Fine 3: Good 4:     0:",
            "                          Unsure Dismiss",
            "",
        ],
        40,
        "  ⏵⏵ auto mode on (shift+tab to cycle)",
    );
    assert_eq!(
        survey_block(&plugin).map(|b| b.kind),
        Some(SurveyKind::Plugin)
    );
    assert_eq!(worker_phase(&plugin), Phase::Busy);

    let quoted = narrow(
        &[
            "⏺ The card reads:",
            "  1: Bad  2: Fine 3: Good 0:",
            "                          Dismiss",
            "",
        ],
        36,
        "  ⏵⏵ auto mode on (shift+tab to",
    );
    assert!(survey_block(&quoted).is_none());
    assert!(!survey_open(&quoted));
    let no_dismiss = narrow(
        &[
            "⏺ Done.",
            "",
            "● How is Claude doing this session?",
            "  (optional)",
            "  1: Bad  2: Fine",
            "",
        ],
        36,
        "  ⏵⏵ auto mode on (shift+tab to",
    );
    assert!(survey_block(&no_dismiss).is_none());
    let said_under = narrow(
        &[
            "",
            "● How is Claude doing this session? (optional)",
            "  1: Bad    2: Fine   3: Good   0: Dismiss",
            "  I left it for you to answer.",
        ],
        82,
        FOOTER,
    );
    assert!(survey_block(&said_under).is_none());
}

/// A worker quoting the drafts card (or an ask) in its message, its rows at column 2, is
/// not the card: the worker's last words are not parked away.
#[test]
fn a_quote_of_the_drafts_or_an_ask_card_in_a_message_is_not_the_card() {
    let quoted_drafts = with(
        &DONE[..0],
        &[
            "⏺ The card reads:",
            "",
            "  ✻ Bug report drafted: foo",
            "  │ details",
            "  1 to review · 2 to send · 0 to dismiss",
        ],
    );
    assert!(drafts_card(&quoted_drafts).is_none());
    assert_eq!(
        last_said_row(&quoted_drafts),
        Some("  1 to review · 2 to send · 0 to dismiss")
    );
    assert_eq!(transcript_end(&quoted_drafts), 5);
    let quoted_ask = with(
        &DONE[..0],
        &[
            "⏺ The ask reads:",
            "  ⏺ Can Anthropic look at your session transcript to help us improve Claude Code?",
            "    y: Yes    n: No     d: Don't ask again",
        ],
    );
    assert!(ask_card(&quoted_ask).is_none());
    assert_eq!(
        last_said_row(&quoted_ask),
        Some("    y: Yes    n: No     d: Don't ask again")
    );
    let plain = with(
        &DONE[..0],
        &[
            "⏺ The ask reads:",
            "  y: Yes    n: No     d: Don't ask again",
        ],
    );
    assert!(ask_card(&plain).is_none());
    assert_eq!(
        last_said_row(&plain),
        Some("  y: Yes    n: No     d: Don't ask again")
    );
    assert_eq!(transcript_end(&plain), 2);
    let card = with(
        &DONE[..0],
        &[
            "⏺ The card reads:",
            "",
            "✻ Bug report drafted: foo",
            "│ details",
            "1 to review · 2 to send · 0 to dismiss",
        ],
    );
    assert!(drafts_card(&card).is_some());
}

/// An ask card is its options row (`<key>: …` two columns in, `Don't ask again` on it)
/// under a `⏺` head in column 0, and nothing else.
#[test]
fn an_ask_card_needs_its_options_and_its_head() {
    assert!(ask_card(&with(&DONE, &ASK)).is_some());
    assert!(ask_card(&with(&DONE, &WEB)).is_some());
    for above in [
        r(&["⏺ done", "", "  y: Yes    n: No     d: Don't ask again"]),
        r(&[
            "",
            "Can Anthropic look?",
            "  y: Yes    n: No     d: Don't ask again",
        ]),
        r(&["", "⏺ Ask?", "  Don't ask again"]),
        r(&["", "⏺ Ask?", "     y: Yes    n: No     d: Don't ask again"]),
        r(&[
            "",
            "● How is Claude doing this session? (optional)",
            "  1: Bad    2: Fine   3: Good   0: Dismiss",
        ]),
    ] {
        let rows = screen(&above.iter().map(String::as_str).collect::<Vec<_>>());
        assert!(ask_card(&rows).is_none(), "{above:?}");
    }
    let wrapped = with(
        &DONE,
        &[
            "",
            "⏺ Can Anthropic look at your session transcript to help us",
            "  improve Claude Code?",
            "  https://code.claude.com/docs/en/data-usage#session-quality-surveys",
            "  y: Yes    n: No     d: Don't ask again",
        ],
    );
    assert_eq!(
        ask_card(&wrapped).map(|(h, _)| wrapped[h].as_str()),
        Some("⏺ Can Anthropic look at your session transcript to help us")
    );
    let rows = with(&DONE, &ASK);
    assert!(survey_block(&rows).is_none());
    assert!(!survey_open(&rows));
}

#[test]
fn the_drafts_card_needs_its_head_and_its_hint() {
    let rows = with(&DONE, &DRAFTS);
    assert!(drafts_card(&rows).is_some());
    let no_head = with(&DONE, &["", "1 to review · 2 to send · 0 to dismiss", ""]);
    assert!(drafts_card(&no_head).is_none());
    let other = with(&DONE, &["", "✻ Bug report drafted: x", "Press any key", ""]);
    assert!(drafts_card(&other).is_none());
}

/// Below 41 columns (2.1.300 at 26, 34 and 40) the Ink-wrapped card is one session
/// card: guarded, dismissable, and not the worker's last words.
#[test]
fn a_survey_card_ink_wrapped_into_columns_is_one_card() {
    use crate::prompt::fixtures::{
        SURVEY_SESSION_26_2_1_300, SURVEY_SESSION_34_2_1_300, SURVEY_SESSION_40_2_1_300,
        screen as fixture,
    };
    for text in [
        SURVEY_SESSION_26_2_1_300,
        SURVEY_SESSION_34_2_1_300,
        SURVEY_SESSION_40_2_1_300,
    ] {
        let r = fixture(text);
        let block = survey_block(&r).unwrap_or_else(|| panic!("the card: {r:#?}"));
        assert_eq!(block.kind, SurveyKind::Session);
        assert!(r[block.head].starts_with("● How is Claude doing this"));
        assert!(r[block.options + 1].trim().is_empty(), "{r:#?}");
        assert!(crate::phase::is_rule(&r[block.options + 2]));
        assert_eq!(block.withheld(), None);
        assert_eq!(block.guard, "^●.How.is.Claude.doing");
        assert!(survey_open(&r));
        assert_eq!(worker_phase(&r), Phase::Idle);
        let said = last_said_row(&r).expect("a last said row");
        assert!(
            said.contains("(reply 1)") || said.ends_with("not a model."),
            "{said:?}"
        );
    }
    let r = fixture(SURVEY_SESSION_34_2_1_300);
    let cut: Vec<String> = r
        .iter()
        .map(|x| {
            if x.trim() == "Dismiss" {
                String::new()
            } else {
                x.clone()
            }
        })
        .collect();
    assert!(survey_block(&cut).is_none(), "{cut:#?}");
    let off: Vec<String> = r
        .iter()
        .map(|x| {
            if x.trim() == "Dismiss" {
                format!(" {x}")
            } else {
                x.clone()
            }
        })
        .collect();
    assert!(survey_block(&off).is_none(), "{off:#?}");
}
/// Claude Code's rating card as Ink lays it out `width` columns wide (2.1.300's
/// `PD`).
fn rating_card(question: &str, extra: &[&str], labels: &[&str], width: usize) -> Vec<String> {
    fn wrap(text: &str, cols: usize) -> Vec<String> {
        let mut rows = vec![String::new()];
        for (k, word) in text.split(' ').enumerate() {
            let mut len = rows.last().map_or(0, |r| r.chars().count());
            if k > 0 && len > 0 {
                rows.last_mut().expect("a row").push(' ');
                len += 1;
            }
            let n = word.chars().count();
            if n > cols {
                let here = 1 + (n + len).saturating_sub(cols + 1) / cols;
                let next = (n - 1) / cols;
                if next < here {
                    rows.push(String::new());
                }
                for c in word.chars() {
                    if rows.last().is_some_and(|r| r.chars().count() >= cols) {
                        rows.push(String::new());
                    }
                    rows.last_mut().expect("a row").push(c);
                }
            } else {
                if len + n > cols && len > 0 {
                    rows.push(String::new());
                }
                rows.last_mut().expect("a row").push_str(word);
            }
        }
        rows.iter().map(|r| r.trim_end().to_string()).collect()
    }
    let mut out = vec!["⏺ Done.".to_string(), String::new()];
    for (k, row) in wrap(question, width - 2).iter().enumerate() {
        out.push(format!("{}{row}", if k == 0 { "● " } else { "  " }));
    }
    for row in extra.iter().flat_map(|e| wrap(e, width - 2)) {
        out.push(format!("  {row}"));
    }
    let n = labels.len();
    let each = if width - 2 >= 10 * n {
        10.0
    } else {
        (width - 2) as f64 / n as f64
    };
    let edge = |k: usize| (2.0 + k as f64 * each + 0.5).floor() as usize;
    let boxes: Vec<(usize, Vec<String>)> = labels
        .iter()
        .enumerate()
        .map(|(k, l)| (edge(k), wrap(l, edge(k + 1) - edge(k))))
        .collect();
    let height = boxes.iter().map(|(_, b)| b.len()).max().unwrap_or(0);
    for r in 0..height {
        let mut row: Vec<char> = vec![' '; width];
        for (left, b) in &boxes {
            for (j, c) in b.get(r).map_or("", String::as_str).chars().enumerate() {
                row[left + j] = c;
            }
        }
        out.push(row.iter().collect::<String>().trim_end().to_string());
    }
    out.push(String::new());
    out.extend(r(&[
        &"─".repeat(width),
        "❯",
        &"─".repeat(width),
        "  ⏵⏵ auto mode on",
    ]));
    out
}

const SESSION_QUESTION: &str = "How is Claude doing this session? (optional)";
const SESSION_LABELS: [&str; 4] = ["1: Bad", "2: Fine", "3: Good", "0: Dismiss"];
const PLUGIN_QUESTION: &str = "How helpful has the gh plugin been? (optional)";
const PLUGIN_USED: &str = "Used this session: the skill, the command";
const PLUGIN_LABELS: [&str; 5] = ["1: Bad", "2: Fine", "3: Good", "4: Unsure", "0: Dismiss"];

/// [`rating_card`] draws the session card's question and options exactly as Claude
/// Code 2.1.300 drew them at 26, 34 and 40 columns.
#[test]
fn the_rating_card_layout_is_the_measured_one() {
    use crate::prompt::fixtures::{
        SURVEY_SESSION_26_2_1_300, SURVEY_SESSION_34_2_1_300, SURVEY_SESSION_40_2_1_300,
        screen as fixture,
    };
    for (width, text) in [
        (26, SURVEY_SESSION_26_2_1_300),
        (34, SURVEY_SESSION_34_2_1_300),
        (40, SURVEY_SESSION_40_2_1_300),
    ] {
        let measured = fixture(text);
        let head = measured.iter().position(|x| x.starts_with('●')).expect("●");
        let card = |rows: &[String]| -> Vec<String> {
            rows.iter().take_while(|x| !x.is_empty()).cloned().collect()
        };
        let drawn = rating_card(SESSION_QUESTION, &[], &SESSION_LABELS, width);
        let at = drawn.iter().position(|x| x.starts_with('●')).expect("●");
        assert_eq!(card(&drawn[at..]), card(&measured[head..]), "{width}");
    }
}

/// Where Ink shrinks the card's first box under its label, the options' first row
/// opens `  1:` and spaces, and the row under it opens at column two.
#[test]
fn a_rating_card_whose_first_box_wraps_is_the_card() {
    for width in 16..=60 {
        for (question, extra, labels, kind) in [
            (
                SESSION_QUESTION,
                &[][..],
                &SESSION_LABELS[..],
                SurveyKind::Session,
            ),
            (
                PLUGIN_QUESTION,
                &[PLUGIN_USED][..],
                &PLUGIN_LABELS[..],
                SurveyKind::Plugin,
            ),
        ] {
            let rows = rating_card(question, extra, labels, width);
            let block =
                survey_block(&rows).unwrap_or_else(|| panic!("{width}: the card: {rows:#?}"));
            assert_eq!(block.kind, kind, "{width}");
            assert_eq!(rows[block.head], rows[2], "{width}");
            assert!(rows[block.options + 1].is_empty(), "{width}: {rows:#?}");
            assert_eq!(
                block.withheld().is_some(),
                block.guard.is_empty(),
                "{width}"
            );
            assert!(survey_open(&rows), "{width}");
            assert_eq!(worker_phase(&rows), Phase::Idle, "{width}");
            assert_eq!(last_said_row(&rows), Some("⏺ Done."), "{width}");
        }
    }
    let rows = rating_card(SESSION_QUESTION, &[], &SESSION_LABELS, 23);
    let cut: Vec<String> = rows
        .iter()
        .map(|x| x.replace("smiss", "     ").trim_end().to_string())
        .collect();
    assert!(survey_block(&cut).is_none(), "{cut:#?}");
    let first = rows.iter().position(|x| x.starts_with("  1:")).expect("1:");
    let mut said = rows.clone();
    said[first + 1] = "  but the build is red".to_string();
    assert!(survey_block(&said).is_none(), "{said:#?}");
}
