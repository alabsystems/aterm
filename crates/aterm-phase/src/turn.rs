// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a finished (or stalled) turn leaves on the screen for whoever decides
//! what comes next: Claude Code's own suggestion for the next message, whether
//! a `/goal` is armed, the worker's last words, and whether a running turn's
//! token count has stopped moving.

use crate::phase::{
    composer_frame, composer_text, is_against_right_edge, is_done_row, is_placeholder,
    is_tool_call, last_said_index, leading_spaces, status_block, status_row, transcript_end,
};
use crate::prompt::parse_prompt;

/// Claude Code's own suggestion for the next message: the DIM placeholder
/// in an empty composer ([`is_placeholder`]: the caret row reads `❯ keep
/// going` but the cursor sits at column 2, where typing would have pushed
/// it). Measured 2026-09-21 on a live session
/// ([`crate::prompt::fixtures::GOAL_ACTIVE_SUGGESTION`]). `None` without the
/// composer frame (the caret-looking row is then a user row or an option),
/// with a box up, or when the composer holds typed text.
#[must_use]
pub fn continuation_suggestion(rows: &[String], cursor_col: usize) -> Option<String> {
    composer_frame(rows)?;
    if parse_prompt(rows).is_some() || !is_placeholder(rows, cursor_col) {
        return None;
    }
    composer_text(rows).filter(|t| !t.is_empty())
}

/// Whether a `/goal` is armed: Claude Code's `◎ /goal active (3h)` indicator
/// in the live zone — between the status row (the last transcript row when
/// there is none) and the composer's top rule, or in the footer — against
/// the right edge, where it was measured (2026-09-21, right-aligned above
/// the rule). A worker's words quoting it are transcript: with no done row
/// under them they fall in the live zone too, but at the message's column,
/// so they do not count. Without the frame, `false`.
#[must_use]
pub fn goal_active(rows: &[String]) -> bool {
    let Some(frame) = composer_frame(rows) else {
        return false;
    };
    let width = rows[frame.bottom].trim_end().chars().count();
    let from = status_block(rows, frame.top).from;
    rows[from..frame.top]
        .iter()
        .chain(&rows[frame.bottom + 1..])
        .any(|r| r.trim_start().starts_with("◎ /goal active") && is_against_right_edge(r, width))
}

/// The worker's last words: the `⏺` message that holds the last thing said
/// ([`last_said_index`]), from its `⏺` row down to that row, the marker and
/// indentation stripped, blank rows and any `⎿` block under it (the
/// vendor's or a tool's rows) left out, one line per row. `None` when the
/// last thing said is not in a message — a user row, a tool call, a shell's
/// screen — so a stop-phrase check never reads anyone else's words.
///
/// A message TALLER than the screen has its `⏺` row scrolled off: every
/// visible row above the last said is indented (the message's continuation
/// column). Its tail is then the visible rows from the top — what the
/// worker ended on — never `None`, which a policy would read as "nothing
/// asked" (the safety review of 2026-09-24: `Should I drop the prod
/// table…?` under 30 rows of summary was continued).
#[must_use]
pub fn said_tail(rows: &[String]) -> Option<String> {
    let last = last_said_index(rows)?;
    let head = (0..=last).rev().find(|&i| {
        let r = &rows[i];
        !r.trim().is_empty() && leading_spaces(r) == 0
    });
    let (head, headless) = match head {
        Some(h) => (h, false),
        // The message's head is above the screen: its visible rows, from
        // the top.
        None => (0, true),
    };
    if !headless && (!rows[head].starts_with(['⏺', '●']) || is_tool_call(&rows[head])) {
        return None;
    }
    let mut out: Vec<&str> = Vec::new();
    let mut in_gutter = false;
    for (i, r) in rows.iter().enumerate().take(last + 1).skip(head) {
        let t = r.trim();
        if t.is_empty() {
            in_gutter = false;
            continue;
        }
        if t.starts_with('⎿') {
            in_gutter = true;
            continue;
        }
        if in_gutter && leading_spaces(r) >= 5 {
            continue;
        }
        in_gutter = false;
        let t = if i == head && !headless {
            t.trim_start_matches(['⏺', '●']).trim_start()
        } else {
            t
        };
        out.push(t);
    }
    (!out.is_empty()).then(|| out.join("\n"))
}

/// Whether a PERSON stopped the last turn: the vendor's `⎿  Interrupted ·
/// What should Claude do instead?` row (anchor `turn.interrupted`) under
/// the transcript's last block — the last `⏺` message or tool row, with
/// no user `❯` row after it — above the live zone. A turn stopped so is a
/// stop, never a point to continue: the person is at the keyboard and the
/// next message is theirs (the safety review of 2026-09-24 measured a
/// `keep going` typed into exactly this screen). Negative: the same words
/// in a message, or an interrupt a new user message has answered.
#[must_use]
pub fn interrupted(rows: &[String]) -> bool {
    let end = transcript_end(rows).min(rows.len());
    let Some(block) = (0..end)
        .rev()
        .find(|&i| rows[i].starts_with(['⏺', '●', '❯']))
    else {
        return false;
    };
    let text = crate::anchors::anchor_text("turn.interrupted");
    rows[block..end].iter().any(|r| {
        r.trim_start()
            .strip_prefix('⎿')
            .is_some_and(|rest| rest.trim_start().starts_with(text))
    })
}

/// A running turn's progress as its status row shows it: `✻ Synthesizing…
/// (1m 12s · ↓ 3.4k tokens · esc to interrupt)` → 72 seconds and `↓ 3.4k
/// tokens`. `None` without a live status row or without the timing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// The turn's elapsed time, in seconds.
    pub elapsed_secs: u64,
    /// The token count as drawn (`↓ 3.4k tokens`), when the row has one.
    pub tokens: Option<String>,
}

/// The progress on the live status row ([`Progress`]).
#[must_use]
pub fn status_row_progress(rows: &[String]) -> Option<Progress> {
    let row = status_row(rows).filter(|r| !is_done_row(r))?;
    let inner = row.split_once(" (")?.1;
    let inner = inner.rsplit_once(')').map_or(inner, |(a, _)| a);
    let mut items = inner.split(" · ");
    let elapsed_secs = duration_secs(items.next()?.trim())?;
    let tokens = items
        .map(str::trim)
        .find(|i| i.ends_with("tokens"))
        .map(str::to_string);
    Some(Progress {
        elapsed_secs,
        tokens,
    })
}

/// Whether a turn has stalled between two reads of the same session: the
/// elapsed time moved on while the token count stayed exactly as drawn.
/// Either read without a timing or a count is no evidence, so `false`.
#[must_use]
pub fn status_row_stall(before: &[String], after: &[String]) -> bool {
    match (status_row_progress(before), status_row_progress(after)) {
        (Some(b), Some(a)) => {
            a.elapsed_secs > b.elapsed_secs && a.tokens.is_some() && a.tokens == b.tokens
        }
        _ => false,
    }
}

/// `18s`, `1m 12s`, `2h 3m` → seconds; anything else `None`.
fn duration_secs(text: &str) -> Option<u64> {
    let mut total = 0u64;
    let mut any = false;
    for word in text.split_whitespace() {
        let unit = word.chars().last()?;
        let n: u64 = word[..word.len() - unit.len_utf8()].parse().ok()?;
        total += n * match unit {
            'h' => 3600,
            'm' => 60,
            's' => 1,
            _ => return None,
        };
        any = true;
    }
    any.then_some(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::fixtures::{
        BOX_BASH_TOUCH, END_529, END_OFFER, GOAL_ACTIVE_SUGGESTION, composer, rows, screen,
    };

    fn framed(body: &[&str], footer: &str) -> Vec<String> {
        let mut r = rows(body);
        r.extend(composer(footer));
        r
    }

    /// The measured screen: the dim `keep going` with the cursor at column 2
    /// is the suggestion; the same text with the cursor past it is typed.
    #[test]
    fn the_dim_suggestion_is_read_and_a_typed_draft_is_not() {
        let r = screen(GOAL_ACTIVE_SUGGESTION);
        assert_eq!(
            continuation_suggestion(&r, 2).as_deref(),
            Some("keep going")
        );
        assert_eq!(continuation_suggestion(&r, 12), None, "typed text");
        // A box's option row and a user row are never a suggestion: no frame.
        let b = screen(BOX_BASH_TOUCH);
        assert_eq!(continuation_suggestion(&b, 2), None);
        // An empty composer suggests nothing.
        let idle = framed(&["⏺ Done.", ""], "  ? for shortcuts");
        assert_eq!(continuation_suggestion(&idle, 2), None);
    }

    #[test]
    fn goal_active_is_read_from_the_live_zone_only() {
        assert!(goal_active(&screen(GOAL_ACTIVE_SUGGESTION)));
        let quoted = framed(
            &[
                "⏺ The indicator reads",
                "  ◎ /goal active (3h)",
                "  when a goal is armed.",
                "",
                "✻ Worked for 3s · done 9:01 AM",
                "",
            ],
            "  ? for shortcuts",
        );
        assert!(!goal_active(&quoted), "quoted in the transcript");
        assert!(!goal_active(&screen(END_OFFER)));
        // No done row under the quote: the message's rows reach the live
        // zone, and still do not count — they sit at the message's column.
        for body in [
            &[
                "⏺ The manager's footer read:",
                "",
                "  ◎ /goal active (3h)",
                "",
            ][..],
            &["⏺ The manager's footer read:", "  ◎ /goal active (3h)", ""],
        ] {
            assert!(!goal_active(&framed(body, "  ? for shortcuts")), "{body:?}");
        }
        // The control: the indicator against the right edge over the rule.
        let right = format!("{}◎ /goal active (3h)", " ".repeat(99));
        let armed = framed(&["⏺ Done.", "", &right], "  ? for shortcuts");
        assert!(goal_active(&armed));
    }

    #[test]
    fn said_tail_is_the_last_message_without_its_gutter() {
        assert_eq!(
            said_tail(&screen(END_OFFER)).as_deref(),
            Some(
                "Done: 12 of 67 divisions now solve. Next: the FP-sorted binder path; want me to take that on?"
            )
        );
        // The 529 row under the message is the vendor's, not the worker's.
        assert_eq!(
            said_tail(&screen(END_529)).as_deref(),
            Some("Suites are running; I'll report the number when it lands.")
        );
        let para = framed(
            &[
                "⏺ Merged the fix.",
                "",
                "  Next I would port the reader. Shall I go on?",
                "",
            ],
            "  ? for shortcuts",
        );
        assert_eq!(
            said_tail(&para).as_deref(),
            Some("Merged the fix.\nNext I would port the reader. Shall I go on?")
        );
        // Negative controls: a user row, a tool call's output.
        let user = framed(&["❯ carry on with the parser", ""], "  ? for shortcuts");
        assert_eq!(said_tail(&user), None);
        let tool = framed(
            &["⏺ Bash(cargo test)", "  ⎿  test result: ok", ""],
            "  ? for shortcuts",
        );
        assert_eq!(said_tail(&tool), None);
    }

    /// A message taller than the screen: its `⏺` row scrolled off, every
    /// visible row indented. The tail is the visible rows — the question the
    /// worker ended on is read (the safety review of 2026-09-24). The
    /// measured goal fixture (row 0 `  with zero hooks installed…`) reads
    /// its last words too. Negative control: a column-0 user row above
    /// still ends the message there.
    #[test]
    fn a_message_whose_head_scrolled_off_still_has_a_tail() {
        let mut body: Vec<String> = (0..30)
            .map(|i| format!("  - finding {i}: details"))
            .collect();
        body.push(String::new());
        body.push("  Should I drop the prod table or keep it?".to_string());
        body.push(String::new());
        body.push("✻ Cooked for 3m 2s · done 4:24 PM".to_string());
        body.push(String::new());
        let refs: Vec<&str> = body.iter().map(String::as_str).collect();
        let long = framed(&refs, "  ? for shortcuts");
        let tail = said_tail(&long).expect("a tail");
        assert!(tail.starts_with("- finding 0: details"), "{tail}");
        assert!(
            tail.ends_with("Should I drop the prod table or keep it?"),
            "{tail}"
        );
        let goal = said_tail(&screen(GOAL_ACTIVE_SUGGESTION)).expect("the goal fixture's tail");
        assert!(goal.starts_with("with zero hooks installed."), "{goal}");
        let user = framed(
            &[
                "❯ summarise",
                "  - finding 1: details",
                "  Should I drop it?",
                "",
            ],
            "  ? for shortcuts",
        );
        assert_eq!(said_tail(&user), None);
    }

    /// The measured interrupt row, under a tool row and under a message
    /// row, reads as a person's stop. Negative controls: the words in the
    /// worker's own message, and a user message typed after the interrupt.
    #[test]
    fn a_persons_interrupt_is_read_under_the_last_block_only() {
        let foot = "  ⏵⏵ bypass permissions on (shift+tab to cycle)";
        let tool = framed(
            &[
                "⏺ Running the schema migration against the staging database now.",
                "",
                "⏺ Bash(./migrate.sh --env staging)",
                "  ⎿  Interrupted · What should Claude do instead?",
                "",
            ],
            foot,
        );
        assert!(interrupted(&tool));
        let message = framed(
            &[
                "⏺ Running the schema migration against the staging database now.",
                "  ⎿  Interrupted · What should Claude do instead?",
                "",
            ],
            foot,
        );
        assert!(interrupted(&message));
        let quoted = framed(
            &[
                "⏺ The vendor prints Interrupted · What should Claude do instead? on Esc.",
                "",
            ],
            foot,
        );
        assert!(!interrupted(&quoted));
        let answered = framed(
            &[
                "⏺ Bash(./migrate.sh --env staging)",
                "  ⎿  Interrupted · What should Claude do instead?",
                "",
                "❯ run it against the scratch database instead",
                "",
                "⏺ Running it against the scratch database.",
                "",
            ],
            foot,
        );
        assert!(!interrupted(&answered));
    }

    #[test]
    fn a_stall_is_time_moving_with_the_count_flat() {
        let at = |timing: &str| {
            framed(
                &["⏺ Running.", "", &format!("✻ Synthesizing… ({timing})"), ""],
                "  esc to interrupt",
            )
        };
        let a = at("1m 12s · ↓ 3.4k tokens · esc to interrupt");
        assert_eq!(
            status_row_progress(&a),
            Some(Progress {
                elapsed_secs: 72,
                tokens: Some("↓ 3.4k tokens".to_string())
            })
        );
        let b = at("1m 40s · ↓ 3.4k tokens · esc to interrupt");
        assert!(status_row_stall(&a, &b));
        // Negative controls: the count moved; no count; the same instant; a
        // done row.
        assert!(!status_row_stall(&a, &at("1m 40s · ↓ 3.9k tokens")));
        assert!(!status_row_stall(&at("12s"), &at("40s")));
        assert!(!status_row_stall(&a, &a));
        let done = framed(
            &["⏺ Done.", "", "✻ Cooked for 3m 2s · done 4:24 PM", ""],
            "  ? for shortcuts",
        );
        assert_eq!(status_row_progress(&done), None);
        assert_eq!(duration_secs("2h 3m"), Some(7380));
        assert_eq!(duration_secs("thinking"), None);
    }
}
