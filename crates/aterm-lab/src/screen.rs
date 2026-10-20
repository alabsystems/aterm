// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ONE idea of what a Claude Code screen shows, for every lab verb that
//! judges one: the survey card, the composer row, the busy row and an idle
//! turn's done row; and the composer of the other agents the lab runs
//! (Codex, Gemini CLI, agy, Muse Code).

/// The no-break space Claude Code puts after its `❯`.
const NBSP: char = '\u{a0}';

/// The survey card's question (`survey.question`).
const CARD: &str = aterm_phase::anchors::anchor_text("survey.question");

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
    s.chars().count() >= 40 && s.chars().all(|c| c == '─' || c == '━')
}

/// Whether the survey card is on the screen.
#[must_use]
pub fn has_card(rows: &[String]) -> bool {
    rows.iter().any(|r| is_card_row(r))
}

/// The composer's first row (its `❯` and no-break space taken off), or `None`
/// when the frame has no composer.
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

/// The first row of the composer on a screen `ctl text` gave, without its
/// marker and blanks, for the agent in the tab; `None` when it has none.
/// Codex's composer is its LAST row that begins with `›` (those above it are
/// turns already sent), and none while a box draws its `›` cursor at column
/// 2 under it (the lab's own reading: aterm reads no Codex screen since its
/// Codex screen lane was retired on 2026-10-16): its question dialog does,
/// so the user's message over it was the last `›` row, read as a prompt
/// still standing, and `turn`'s second Enter answered the question with its
/// focused answer (measured on 0.163.0, 2026-10-16); Claude Code's the first row between
/// its last two rules; Gemini CLI's the caret row of the band its own reader
/// finds (`aterm_phase::gemini::composer_draft`: under the live zone's rule
/// as far down as a narrow pane's context summary puts it, or the last thing
/// drawn with the UI details hidden; `>` or YOLO's `*`; never a sent
/// prompt's band, which no rule stands over, nor shell mode's `!`, where a
/// prompt would run as a command; its placeholder read as empty); agy's
/// the caret row of the frame its own reader finds
/// (`aterm_phase::agy::composer_draft`: the last `>` row between two rules
/// of one width, the footer the last row drawn — under the background-work
/// panel too, where the first row between the screen's last two rules is a
/// panel row; a prompt it sent sits under a shorter rule, and a box or its
/// survey has no composer); Muse Code's the caret row of the frame its own reader finds
/// (`aterm_phase::muse::composer_draft`: the last `❯` row under a rule, the
/// footer the last row drawn), never a sent message's `❯` row in the history.
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

    #[test]
    fn a_turn_stands_in_the_agents_own_composer() {
        let rule = "─".repeat(60);
        let claude = format!("⏺ Hi\n\n{rule}\n❯\u{a0}hi \n{rule}\n  ⏵⏵ auto mode on");
        assert_eq!(composer_row("claude", &claude).as_deref(), Some("hi"));
        // Codex: its LAST `›` row (those above it are turns already sent)
        let codex = "› hi\n\n• Hi! (reply 1)\n\n› carry on \n\n  ? for shortcuts";
        assert_eq!(composer_row("codex", codex).as_deref(), Some("carry on"));
        assert_eq!(composer_row("codex", "no composer"), None);
        assert_eq!(composer_row("claude", "no frame"), None);
    }

    /// Codex's question dialog is no composer: the user's message over it is
    /// the screen's last row in column 0 that opens `›` (the dialog's cursor
    /// is at column 2), and read as a prompt still standing, `turn` pressed
    /// Enter on the dialog and answered it (the rows measured on 0.163.0,
    /// 2026-10-16). NEGATIVE CONTROL: the idle screen's composer is read, its
    /// placeholder its text.
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
    /// between two full-width ones. NEGATIVE CONTROLS: with a box on the
    /// screen (its title's full rule last, the sent prompt's short rule
    /// before it) there is no composer, so `> shell` is never read as one;
    /// nor with its survey up (only the sent prompts' short rules stand, under
    /// a full-width footer), so `> turn 8` is never one either.
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

    /// agy 1.4.0 with its background-work panel under the composer (the
    /// lab's 2026-10-17 captures): the draft held over it is the composer's,
    /// and the empty caret over it reads empty — where the first row between
    /// the screen's last two rules, a panel row, was read before, and `turn`
    /// refused a draft standing there. NEGATIVE CONTROL: the screen agy left
    /// with the panel's rows in its last frame has no composer.
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
    /// NEGATIVE CONTROLS: the trust box has no composer, and with the composer
    /// erased at exit the sent message's `❯ hello there` is never taken for one.
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
        // Claude Code's reading of the same screen takes the labelled rule for no rule: no frame at all
        assert_eq!(composer_row("claude", &body(f::DRAFT)), None);
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
