// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Codex composer row placement ([`crate::reader::CodexReader`], [`composer_pad`]).

use crate::text::blank;

/// Major component of the Codex (`codex-alab`) harness version, starting at `0.1`.
pub const HARNESS_VERSION_MAJOR: u16 = 0;

/// Minor component of the Codex (`codex-alab`) harness version, starting at `0.1`.
pub const HARNESS_VERSION_MINOR: u16 = 1;

/// Version of the Codex harness, tracked independently per harness starting at `"0.1"`.
pub const HARNESS_VERSION: &str = "0.1";

/// Structured incremental numeric version of the Codex (`codex-alab`) harness.
pub const HARNESS_VER: crate::HarnessVersion = crate::HarnessVersion::new(
    HARNESS_VERSION_MAJOR,
    HARNESS_VERSION_MINOR,
    HARNESS_VERSION,
);

/// The prompt glyph, in column 0 (U+203A).
const PROMPT: char = '›';

/// A prompt row: `›` in column 0.
fn is_prompt_row(row: &str) -> bool {
    let Some(rest) = row.strip_prefix(PROMPT) else {
        return false;
    };
    if crate::text::trim_end(rest).is_empty() {
        return true;
    }
    let Some(rest) = rest.strip_prefix(' ') else {
        return false;
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    !(digits > 0 && rest[digits..].starts_with(". "))
}

/// The row of the composer's top pad: the blank row right above the prompt row, the last row
/// with `›` in column 0.
#[must_use]
pub fn composer_pad(rows: &[String]) -> Option<usize> {
    let prompt = rows.iter().rposition(|r| r.starts_with(PROMPT))?;
    let under_is_composer = rows[prompt + 1..]
        .iter()
        .all(|r| !r.trim_start().starts_with(PROMPT) && r.chars().next().is_none_or(|c| c == ' '));
    if !is_prompt_row(&rows[prompt]) || !under_is_composer {
        return None;
    }
    let pad = prompt.checked_sub(1)?;
    blank(&rows[pad]).then_some(pad)
}

/// The Codex screens the tests read, each with its program, version and
/// provenance on its first line.
#[cfg(test)]
pub(crate) mod fixtures {
    use crate::prompt::fixtures::rows;

    /// codex 0.154.0's first frame.
    pub(crate) const COMPOSER_0_154: &str = include_str!("fixtures/codex-0.154.0-composer.txt");

    /// codex 0.155.1's trust gate, measured.
    pub(crate) fn codex_0_155_trust() -> Vec<String> {
        rows(&[
            "> You are in /Users//x/aterm",
            "",
            "  Do you trust the contents of this directory? Working with untrusted contents comes with higher risk of",
            "  prompt injection. Trusting the directory allows project-local config, hooks, and exec policies to load.",
            "",
            "› 1. Yes, continue",
            "  2. No, quit",
            "",
            "  Press enter to continue",
        ])
    }

    /// codex 0.163.0's question dialog in the composer's place, under the
    /// message that asked for it (the rows `aterm-lab`'s
    /// `codexs_question_dialog_is_no_composer` measured): its pointer at
    /// column 2.
    pub(crate) fn codex_0_163_question() -> Vec<String> {
        rows(&[
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
        ])
    }

    /// A turn running over the composer (the reader's sample).
    pub(crate) fn working() -> Vec<String> {
        rows(&[
            "• Working (12s • esc to interrupt)",
            "",
            "› Ask Codex to do anything",
            "",
            "  gpt-5 · ~/src",
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{COMPOSER_0_154, codex_0_155_trust, codex_0_163_question, working};
    use super::*;
    use crate::prompt::fixtures::{provenance, rows, screen};

    /// THE COMPOSER'S TOP PAD is the blank row above the prompt: on the
    /// reader's sample of a running turn and on 0.154.0's measured first
    /// frame (the card, a blank row, the slab's pad, `›`, the pad, the
    /// footer), and with a draft typed or wrapped, under the same `›`; a
    /// transcript's earlier `›` row is passed over for the last one.
    #[test]
    fn codex_composer_pad_is_the_row_above_the_prompt() {
        assert_eq!(composer_pad(&working()), Some(1));
        let take = screen(COMPOSER_0_154);
        assert!(provenance(COMPOSER_0_154).is_some_and(|p| p.starts_with("codex 0.154.0")));
        let prompt = take
            .iter()
            .position(|r| r.starts_with("› Ask Codex to do anything"))
            .expect("the prompt row");
        assert_eq!(
            (prompt, composer_pad(&take)),
            (8, Some(7)),
            "row 9 of the take is `›`, row 8 its pad"
        );
        assert!(take[6].is_empty() && take[5].starts_with('╰'), "{take:#?}");
        let mut typed = take.clone();
        typed[prompt] = "› think slowly".to_string();
        assert_eq!(composer_pad(&typed), Some(7), "a draft");
        let mut wrapped = take.clone();
        wrapped[prompt] = "› a draft long enough to wrap onto".to_string();
        wrapped.insert(prompt + 1, "  the next row".to_string());
        assert_eq!(composer_pad(&wrapped), Some(7), "a wrapped draft");
        let mut empty = take.clone();
        empty[prompt] = "›".to_string();
        assert_eq!(composer_pad(&empty), Some(7), "`›` alone");
        let mut history = rows(&["› an earlier message", "", "• Ran ls"]);
        history.extend(take.iter().cloned());
        assert_eq!(composer_pad(&history), Some(10), "the last `›` row");

        assert_eq!(composer_pad(&codex_0_155_trust()), None, "the trust gate");
        let mut choice = codex_0_155_trust();
        choice[5] = "› 12. Yes, continue".to_string();
        assert_eq!(composer_pad(&choice), None, "a two-digit choice");
        let sent = |under: &[&str]| {
            let mut screen = take[..prompt - 1].to_vec();
            screen.extend(rows(&["", "› fix the tests", ""]));
            screen.extend(rows(under));
            screen
        };
        assert_eq!(
            composer_pad(&sent(&["", "  ? for shortcuts"])),
            Some(prompt - 1),
            "CONTROL: the same rows as the composer name its pad"
        );
        assert_eq!(
            composer_pad(&sent(&[
                "",
                "  Would you like to run the following command?",
                "",
                "› 1. Yes, proceed",
                "  2. No, and tell Codex what to do differently",
            ])),
            None,
            "an approval under a sent message"
        );
        let mut question = take[..prompt - 1].to_vec();
        question.push(String::new());
        question.extend(codex_0_163_question());
        assert_eq!(
            composer_pad(&question),
            None,
            "0.163.0's question dialog under a sent message"
        );
        assert_eq!(
            composer_pad(&sent(&["", "• Ran ls", "  └ a b c"])),
            None,
            "the transcript going on under a sent message"
        );
        let mut under_text = take.clone();
        under_text[7] = "• Working (3s • esc to interrupt)".to_string();
        assert_eq!(composer_pad(&under_text), None, "text above the prompt");
        assert_eq!(composer_pad(&take[prompt..]), None, "`›` first");
        let mut indented = take.clone();
        indented[prompt] = format!(" {}", take[prompt]);
        assert_eq!(composer_pad(&indented), None, "`›` in column 1");
        let mut glued = take.clone();
        glued[prompt] = "›Ask Codex to do anything".to_string();
        assert_eq!(composer_pad(&glued), None, "`›` before a word");
        assert_eq!(composer_pad(&rows(&["% ls", "", "a b c"])), None);
        assert_eq!(composer_pad(&[]), None);
    }
}
