// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ONE idea of what a Claude Code screen shows, for every lab verb that judges one.

const NBSP: char = '\u{a0}';

/// The survey card's question (`survey.question`).
const CARD: &str = aterm_phase::anchor!("survey.question");

/// The survey card's head row: `●`, a space, the question.
#[must_use]
pub fn is_card_row(r: &str) -> bool {
    r.strip_prefix('●')
        .and_then(|rest| {
            let mut it = rest.chars();
            it.next().filter(|c| c.is_whitespace()).map(|_| it.as_str())
        })
        .is_some_and(|rest| rest.starts_with(CARD))
}

/// `  1: Bad    2: Fine   3: Good   0: Dismiss`: the card's options row.
#[must_use]
pub fn is_options_row(r: &str) -> bool {
    let mut rest = r;
    if !rest.starts_with(char::is_whitespace) {
        return false;
    }
    for word in ["1: Bad", "2: Fine", "3: Good", "0: Dismiss"] {
        let t = rest.trim_start();
        if t.len() == rest.len() {
            return false;
        }
        let Some(after) = t.strip_prefix(word) else {
            return false;
        };
        rest = after;
    }
    true
}

/// `✻ Baked for 0s · done 5:39 AM`: the done row of a turn.
#[must_use]
pub fn is_done_row(r: &str) -> bool {
    r.strip_prefix("✻ ").is_some_and(|rest| {
        rest.match_indices(" for ").any(|(i, _)| {
            rest[i + 5..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit())
        })
    })
}

/// A full-width rule of the composer's frame.
#[must_use]
pub fn is_rule(r: &str) -> bool {
    let s = r.trim();
    s.chars().count() >= 40
        && (s.chars().all(|c| c == '─' || c == '━') || aterm_phase::phase::is_rule(s))
}

/// Whether the survey card is on the screen.
#[must_use]
pub fn has_card(rows: &[String]) -> bool {
    rows.iter().any(|r| is_card_row(r))
}

/// The composer's first row, or `None` when the frame has no composer.
#[must_use]
pub fn composer_of(rows: &[String]) -> Option<String> {
    let idx: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| is_rule(r))
        .map(|(i, _)| i)
        .collect();
    let [.., top, bot] = idx.as_slice() else {
        return None;
    };
    let Some(first) = rows.get(top + 1..*bot).and_then(<[String]>::first) else {
        return Some(String::new());
    };
    let t = first.replace(NBSP, " ");
    let t = t.strip_prefix('❯').unwrap_or(&t);
    let t = t.strip_prefix(' ').unwrap_or(t);
    Some(t.trim_end().to_string())
}

/// The status row of a vi-family editor holding the screen `ctl text` gave: the last row that
/// is neither blank nor vi's `~` (the filler under a short buffer), on a screen with at least
/// three `~` rows. `None`: no such editor's screen.
#[must_use]
pub fn editor_row(screen: &str) -> Option<String> {
    let rows: Vec<&str> = screen.split('\n').map(str::trim_end).collect();
    if rows.iter().filter(|r| **r == "~").count() < 3 {
        return None;
    }
    rows.iter()
        .rev()
        .find(|r| !r.trim().is_empty() && **r != "~")
        .map(|r| (*r).to_string())
}

/// The first row of the composer on a screen `ctl text` gave.
#[must_use]
pub fn composer_row(agent: &str, screen: &str) -> Option<String> {
    let rows: Vec<String> = screen
        .replace(NBSP, " ")
        .split('\n')
        .map(str::to_string)
        .collect();
    if agent == "muse" {
        let (_, lines) = aterm_phase::muse::composer_draft(&rows)?;
        return lines.into_iter().next();
    }
    if agent == "codex" {
        let at = rows.iter().rposition(|r| r.starts_with('›'))?;
        if rows[at + 1..].iter().any(|r| r.starts_with("  › ")) {
            return None;
        }
        return Some(rows[at].trim_start_matches('›').trim().to_string());
    }
    if agent == "gemini" {
        let (_, lines) = aterm_phase::gemini::composer_draft(&rows)?;
        return lines.into_iter().next();
    }
    if agent == "agy" {
        let (_, lines) = aterm_phase::agy::composer_draft(&rows)?;
        return lines.into_iter().next();
    }
    let idx: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| is_rule(r))
        .map(|(i, _)| i)
        .collect();
    let [.., top, bot] = idx.as_slice() else {
        return None;
    };
    let first = rows
        .get(top + 1..*bot)
        .and_then(<[String]>::first)
        .cloned()
        .unwrap_or_default();
    let t = first.strip_prefix('❯').unwrap_or(&first);
    Some(t.trim().to_string())
}

/// The spinner's busy row (`✻ Verb… (Ns · …)`), if any.
#[must_use]
pub fn status_row(rows: &[String]) -> Option<String> {
    rows.iter()
        .map(|r| r.trim_start())
        .find(|s| s.starts_with(['✻', '✶', '✳', '✢', '·', '*', '✽']) && s.contains('…'))
        .map(str::to_string)
}

/// An idle turn: its done row, and no spinner.
#[must_use]
pub fn is_idle(rows: &[String]) -> bool {
    rows.iter().any(|r| is_done_row(r)) && status_row(rows).is_none()
}

/// The `key=value` words of a line.
#[must_use]
pub fn kv(text: &str) -> Vec<(String, String)> {
    text.split_whitespace()
        .filter_map(|t| {
            t.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
        })
        .collect()
}

/// The value of `key` in a line of `key=value` words.
#[must_use]
pub fn field(text: &str, key: &str) -> Option<String> {
    kv(text).into_iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(t: &str) -> Vec<String> {
        t.lines().map(str::to_string).collect()
    }

    #[test]
    fn the_card_and_its_options_are_read_as_drawn() {
        assert!(is_card_row(
            "● How is Claude doing this session? (optional)"
        ));
        assert!(!is_card_row("●How is Claude doing"));
        assert!(!is_card_row("  ● How is Claude doing"));
        assert!(is_options_row("  1: Bad    2: Fine   3: Good   0: Dismiss"));
        assert!(!is_options_row("1: Bad 2: Fine 3: Good 0: Dismiss"));
    }

    #[test]
    fn the_composer_row_is_the_first_row_between_the_last_two_rules() {
        let rule = "─".repeat(60);
        let screen = rows(&format!(
            "⏺ Hi\n\n{rule}\n❯\u{a0}draft text  \n{rule}\n  ⏵⏵ auto mode on"
        ));
        assert_eq!(composer_of(&screen).as_deref(), Some("draft text"));
        let empty = rows(&format!("{rule}\n❯\u{a0}\n{rule}"));
        assert_eq!(composer_of(&empty).as_deref(), Some(""));
        assert_eq!(composer_of(&rows("no frame here")), None);
    }

    /// CLAUDE CODE'S LABELLED TOP RULE (`──── <the mode's name> ─`, drawn
    /// once the top-effort mode is on: a windowed aterm's harness writes it as
    /// an unset default, measured on 2.1.306 in `forever --window`) frames the
    /// composer as a bare rule does, from 40 columns.
    #[test]
    fn claude_code_s_labelled_top_rule_frames_its_composer() {
        let mode = concat!("ultra", "code");
        let rule = "─".repeat(140);
        let label = format!("{} {mode} ─", "─".repeat(128));
        let nbsp = '\u{a0}';
        let screen = |top: &str, draft: &str| {
            format!(
                " ▐▛███▛█▄╱✦  Claude Code v2.1.306\n{top}\n❯{nbsp}{draft}\n{rule}\n  ⏵⏵ auto mode on (shift+tab to cycle)"
            )
        };
        assert!(is_rule(&label) && is_rule(&rule));
        let labelled = |dashes: usize| format!("{} {mode} ─", "─".repeat(dashes));
        assert_eq!(labelled(28).chars().count(), 40);
        assert!(is_rule(&labelled(28)));
        assert!(
            aterm_phase::phase::is_rule(&labelled(27))
                && aterm_phase::phase::is_rule(&labelled(11))
        );
        assert_eq!(
            composer_row("claude", &screen(&label, "")).as_deref(),
            Some("")
        );
        assert_eq!(
            composer_row("claude", &screen(&label, "FOREVERLAB-LAND-P. tell me")).as_deref(),
            Some("FOREVERLAB-LAND-P. tell me")
        );
        assert_eq!(
            composer_of(&rows(&screen(&label, "hi"))).as_deref(),
            Some("hi")
        );
        assert_eq!(
            composer_row("claude", &screen(&rule, "hi")).as_deref(),
            Some("hi")
        );
        for no_rule in [
            format!("──── {mode} ─"),
            labelled(27),
            labelled(11),
            format!("{}┬{}", "─".repeat(60), "─".repeat(60)),
            format!(
                "{} words in the middle of a row {}",
                "─".repeat(5),
                "x".repeat(40)
            ),
        ] {
            assert!(!is_rule(&no_rule), "{no_rule}");
            assert_eq!(
                composer_row("claude", &screen(&no_rule, "hi")),
                None,
                "{no_rule}"
            );
        }
    }

    #[test]
    fn a_turn_stands_in_the_agents_own_composer() {
        let rule = "─".repeat(60);
        let claude = format!("⏺ Hi\n\n{rule}\n❯\u{a0}hi \n{rule}\n  ⏵⏵ auto mode on");
        assert_eq!(composer_row("claude", &claude).as_deref(), Some("hi"));
        let codex = "› hi\n\n• Hi! (reply 1)\n\n› carry on \n\n  ? for shortcuts";
        assert_eq!(composer_row("codex", codex).as_deref(), Some("carry on"));
        assert_eq!(composer_row("codex", "no composer"), None);
        assert_eq!(composer_row("claude", "no frame"), None);
    }

    /// Codex's question dialog is no composer: the user's message over it is
    /// the screen's last row in column 0 that opens `›` (the dialog's cursor
    /// is at column 2), and read as a prompt still standing it would make
    /// `turn` press Enter on the dialog and answer it (the rows measured on
    /// 0.163.0).
    #[test]
    fn codexs_question_dialog_is_no_composer() {
        let dialog = [
            "› QUESTION: plan the database migration",
            "",
            "  Question 1/1 (1 unanswered)",
            "  Which rollout should the migration use?",
            "",
            "  › 1. Short-term (Recommended)  Patch it now, revisit later.",
            "    2. Long-term                 Redesign the schema properly.",
            "    3. None of the above         Optionally, add details in notes (tab)",
            "",
            "  tab to add notes | enter to submit answer | esc to interrupt",
        ]
        .join("\n");
        assert_eq!(composer_row("codex", &dialog), None);
        let idle = [
            "› QUESTION: plan the database migration",
            "",
            "• Hi! I am the local lab stub, not a model. (reply 3)",
            "",
            "› Ask Codex to do anything",
            "",
            "  gpt-6.1-sol medium · /private/tmp/at.lab0/proj",
        ]
        .join("\n");
        assert_eq!(
            composer_row("codex", &idle).as_deref(),
            Some("Ask Codex to do anything")
        );
    }

    #[test]
    fn gemini_cli_s_composer_is_the_band_its_reader_finds() {
        use aterm_phase::gemini::fixtures as f;
        let body = |fixture: &str| fixture.split_once('\n').map_or("", |(_, b)| b).to_string();
        for (fixture, draft) in [
            (f::DRAFT, "a draft the lab never sends"),
            (f::NARROW_DRAFT, "count in a narrow pane"),
            (f::YOLO_DRAFT, "hello in yolo"),
            (f::DETAILS_HIDDEN_DRAFT, "a hidden draft"),
            (f::IDLE, ""),
            (f::NARROW_IDLE, ""),
            (f::END_OF_TURN, ""),
        ] {
            assert_eq!(
                composer_row("gemini", &body(fixture)).as_deref(),
                Some(draft),
                "{}",
                fixture.lines().next().unwrap_or_default()
            );
        }
        assert_eq!(composer_row("gemini", &body(f::BOX_SHELL)), None);
        assert_eq!(composer_row("gemini", &body(f::SHELL_MODE_DRAFT)), None);
        assert_eq!(composer_row("gemini", "no frame"), None);
    }

    /// agy 1.4.0: a sent prompt sits under a 60-column rule, the composer
    /// between two full-width ones.
    #[test]
    fn agy_s_composer_sits_between_two_full_width_rules() {
        let (short, full) = ("─".repeat(60), "─".repeat(120));
        let footer = format!("? for shortcuts{}Gemini 3.1 Pro · low", " ".repeat(85));
        let idle = format!("{short}\n> hi\n  Hi! (reply 1)\n{full}\n> carry on\n{full}\n{footer}");
        assert_eq!(composer_row("agy", &idle).as_deref(), Some("carry on"));
        let empty = format!("{full}\n>\n{full}");
        assert_eq!(composer_row("agy", &empty).as_deref(), Some(""));
        let boxed = format!(
            "{short}\n> shell\n\nCommand\n{full}\nRun this command?\n> 1. Yes, run command"
        );
        assert_eq!(composer_row("agy", &boxed), None);
        let survey = format!(
            "{short}\n> turn 8\n  Hi! (reply 18)\n{short}\n> carry on after error 1\n⚠ agent executor error: \
             Error 401\n How's the CLI experience so far? Help us improve:\n [1] Good  [2] Fine  [3] Bad  [0] Skip\n{footer}"
        );
        assert_eq!(composer_row("agy", &survey), None);
    }

    /// agy 1.4.0 with its background-work panel under the composer (the lab's
    /// captures): the draft held over it is the composer's, and the empty caret
    /// over it reads empty; the first row between the screen's last two rules is
    /// a panel row, not the composer.
    #[test]
    fn agy_s_composer_over_its_background_panel_is_its_caret_row() {
        use aterm_phase::agy::fixtures::lab as f;
        let body = |fixture: &str| fixture.split_once('\n').map_or("", |(_, b)| b).to_string();
        assert_eq!(
            composer_row("agy", &body(f::BG_DRAFT)).as_deref(),
            Some("busy while a task runs")
        );
        assert_eq!(composer_row("agy", &body(f::BG_TASK)).as_deref(), Some(""));
        assert_eq!(composer_row("agy", &body(f::BG_BUSY)).as_deref(), Some(""));
        assert_eq!(composer_row("agy", &body(f::BG_EXITED)), None);
    }

    /// Muse Code (the repo's 1.4.9 fixtures): the caret row of its frame.
    #[test]
    fn muse_code_s_composer_is_the_caret_row_of_its_frame() {
        use aterm_phase::muse::fixtures as f;
        let body = |fixture: &str| fixture.split_once('\n').map_or("", |(_, b)| b).to_string();
        assert_eq!(
            composer_row("muse", &body(f::DRAFT)).as_deref(),
            Some("hello there")
        );
        assert_eq!(composer_row("muse", &body(f::IDLE)).as_deref(), Some(""));
        assert_eq!(
            composer_row("muse", &body(f::END_OF_TURN)).as_deref(),
            Some("")
        );
        assert_eq!(composer_row("muse", &body(f::BOX_TRUST)), None);
        assert_eq!(composer_row("muse", &body(f::EXITED)), None);
        assert_eq!(
            composer_row("claude", &body(f::DRAFT)).as_deref(),
            Some("hello there")
        );
    }

    #[test]
    fn a_turn_is_idle_on_its_done_row_with_no_spinner() {
        let done = rows("✻ Worked for 0s · done 12:16 PM");
        assert!(is_idle(&done));
        let busy = rows("✻ Worked for 0s · done 12:16 PM\n✽ Working… (3s · ↓ 12 tokens)");
        assert_eq!(
            status_row(&busy).as_deref(),
            Some("✽ Working… (3s · ↓ 12 tokens)")
        );
        assert!(!is_idle(&busy));
        assert!(!is_done_row("✻ Worked for a while"));
    }

    #[test]
    fn key_value_words_are_found_by_key() {
        assert_eq!(
            field("OK schema=1 agent=idle render=ok", "render").as_deref(),
            Some("ok")
        );
        assert_eq!(
            field("aterm-ctl: OK 0 total=0 nudge=idle nudged=0", "nudged").as_deref(),
            Some("0")
        );
        assert_eq!(field("x", "y"), None);
    }
}
