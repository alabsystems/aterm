// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What is the worker doing RIGHT NOW, from one screen read? The busy signals
//! were measured on Claude Code 2.1.267: the `esc to interrupt` footer, the
//! spinner row (`✶ Deliberating…`, with or without its `(4s · …)` suffix — the
//! suffix is absent for the first second), `Still working`, `ctrl+b to run in
//! background`, `Waiting for N dynamic workflow`, and a footer that says a
//! background shell is still running (a turn can END while a shell keeps the
//! work going; that is still busy for a supervisor). In AUTO mode the footer
//! drops `esc to interrupt` entirely, which is why the spinner row is the
//! primary signal, not the footer.

use super::prompt::parse_prompt;

/// The worker's phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// A turn (or a background shell) is running.
    Busy,
    /// An approval box is showing ([`parse_prompt`] is `Some`).
    Prompt,
    /// Not busy, no prompt, the last thing said does not end in `?`.
    Idle,
    /// Not busy, no prompt, and the last non-blank row above the composer
    /// separator ends with `?` — it is waiting on an answer.
    Question,
}

impl Phase {
    /// The lowercase word the CLI prints.
    pub fn name(self) -> &'static str {
        match self {
            Phase::Busy => "busy",
            Phase::Prompt => "prompt",
            Phase::Idle => "idle",
            Phase::Question => "question",
        }
    }
}

/// The glyphs Claude Code's spinner cycles through, plus the static ones it
/// leaves on a row mid-thought.
const SPINNERS: &[char] = &['✢', '✽', '✻', '✶', '✳', '✧', '✦', '⚡', '·', '*'];

/// Classify one screen. A prompt wins over busy: a worker blocked on an
/// approval box cannot proceed however many background shells its footer
/// counts, and a supervisor that waited for those would wait forever.
pub fn worker_phase(rows: &[String]) -> Phase {
    if parse_prompt(rows).is_some() {
        return Phase::Prompt;
    }
    if is_busy(rows) {
        return Phase::Busy;
    }
    if last_said_row(rows).is_some_and(|r| r.trim_end().ends_with('?')) {
        return Phase::Question;
    }
    Phase::Idle
}

/// Any busy signal on the screen.
pub fn is_busy(rows: &[String]) -> bool {
    rows.iter().any(|r| busy_reason(r).is_some())
}

/// Which busy signal `row` carries, if any (named for the CLI's diagnostics).
pub fn busy_reason(row: &str) -> Option<&'static str> {
    if row.contains("esc to interrupt") {
        Some("esc to interrupt")
    } else if is_spinner_row(row) {
        Some("spinner row")
    } else if row.contains("Still working") {
        Some("Still working")
    } else if row.contains("ctrl+b to run in background") {
        Some("ctrl+b to run in background")
    } else if number_then(row, "Waiting for ", " dynamic workflow") {
        Some("waiting for a dynamic workflow")
    } else if number_then(row, "", " shell still running")
        || number_then(row, "", " shells still running")
        || number_then(row, "· ", " shell ·")
        || number_then(row, "· ", " shells ·")
    {
        Some("a shell still running")
    } else {
        None
    }
}

/// `^\s*[✢✽✻✶✳✧✦⚡·*]\s+\S+…` — a spinner glyph, whitespace, then a word that
/// runs into an ellipsis (`…` or `...`).
pub fn is_spinner_row(row: &str) -> bool {
    let t = row.trim_start();
    let mut cs = t.chars();
    let Some(g) = cs.next() else {
        return false;
    };
    if !SPINNERS.contains(&g) {
        return false;
    }
    let rest = cs.as_str();
    if !rest.starts_with(char::is_whitespace) {
        return false;
    }
    let word = rest.split_whitespace().next().unwrap_or("");
    let mut chars = word.chars();
    chars.next().is_some_and(|c| c != '…' && c != '.')
        && (chars.as_str().contains('…') || chars.as_str().contains("..."))
}

/// `<prefix><digits><suffix>` somewhere in `row`.
fn number_then(row: &str, prefix: &str, suffix: &str) -> bool {
    let mut from = 0;
    while let Some(rel) = row[from..].find(suffix) {
        let end = from + rel;
        let before = &row[..end];
        let digits = before
            .chars()
            .rev()
            .take_while(char::is_ascii_digit)
            .count();
        if digits > 0 && before[..before.len() - digits].ends_with(prefix) {
            return true;
        }
        from = end + suffix.len();
    }
    false
}

/// The composer row: the last row whose first glyph is `❯` and that is not an
/// option row (`❯ 1. Yes`).
pub fn composer_index(rows: &[String]) -> Option<usize> {
    rows.iter().rposition(|r| {
        let t = r.trim_start();
        t.starts_with('❯') && !is_option_like(t)
    })
}

fn is_option_like(t: &str) -> bool {
    let after = t.trim_start_matches('❯').trim_start();
    let digits = after.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && after[digits..].starts_with('.')
}

/// The text typed (or suggested) in the composer, without the caret.
pub fn composer_text(rows: &[String]) -> Option<String> {
    let i = composer_index(rows)?;
    Some(
        rows[i]
            .trim_start()
            .trim_start_matches('❯')
            .trim()
            .to_string(),
    )
}

/// Whether the composer's text is Claude Code's DIM placeholder suggestion
/// rather than typed input: the caret row reads `❯ <text>` but the cursor sits
/// at column 2, where typing would have pushed it right. Measured: `m7 is
/// reachable as ssh m7, go` was a suggestion, not a human.
pub fn is_placeholder(rows: &[String], cursor_col: usize) -> bool {
    composer_text(rows).is_some_and(|t| !t.is_empty()) && cursor_col == 2
}

/// A composer separator: a row that is mostly `─`.
fn is_separator(row: &str) -> bool {
    row.chars().filter(|c| *c == '─').count() >= 10
}

/// Claude Code's own status rows, never the worker's words: the done row
/// (`✻ Baked for 1m 10s · done 2:50 PM`, a spinner glyph first) and the
/// banners it parks between the transcript and the composer (`✔ Update
/// installed · Restart to update`, measured on the live worker).
const STATUS_GLYPHS: &[char] = &['✔', '✓', '✘', '✗', '⚠', 'ℹ'];

fn is_status_row(row: &str) -> bool {
    row.trim_start()
        .chars()
        .next()
        .is_some_and(|g| SPINNERS.contains(&g) || STATUS_GLYPHS.contains(&g))
}

/// The last non-blank row above the composer separator that is the worker's
/// (the last thing it said): blanks, separators and Claude Code's status rows
/// are skipped, so a question is still a question with the done row and an
/// update banner under it. Without a composer, the last non-blank row of the
/// screen.
fn last_said_row(rows: &[String]) -> Option<&str> {
    let mut i = composer_index(rows).unwrap_or(rows.len());
    // Skip blanks and the separator directly above the caret.
    while i > 0 && (rows[i - 1].trim().is_empty() || is_separator(&rows[i - 1])) {
        i -= 1;
    }
    rows[..i]
        .iter()
        .rev()
        .find(|r| !r.trim().is_empty() && !is_status_row(r))
        .map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::super::prompt::fixtures::{bash_one_row, composer, rows};
    use super::*;

    fn screen(body: &[&str], footer: &str) -> Vec<String> {
        let mut r = rows(body);
        r.extend(composer(footer));
        r
    }

    #[test]
    fn idle_prompt_question_busy_each_have_a_fixture() {
        let idle = screen(
            &[
                "⏺ Merged and pushed.",
                "",
                "✻ Cogitated for 2m 54s · done 2:41 PM",
                "",
            ],
            "  ? for shortcuts",
        );
        assert_eq!(worker_phase(&idle), Phase::Idle);

        assert_eq!(worker_phase(&bash_one_row()), Phase::Prompt);

        let question = screen(
            &[
                "⏺ Two ways to do this: keep the harness or rewrite it.",
                "  Which do you prefer?",
                "",
            ],
            "  ? for shortcuts",
        );
        assert_eq!(worker_phase(&question), Phase::Question);

        let busy = screen(
            &[
                "⏺ Running the tests.",
                "",
                "✻ Synthesizing… (18s · ↓ 1.1k tokens)",
                "",
            ],
            "  ⏵⏵ auto mode on (shift+tab to cycle) · esc to interrupt",
        );
        assert_eq!(worker_phase(&busy), Phase::Busy);
    }

    /// The four misclassifications the real session produced, each now busy.
    #[test]
    fn the_four_misclassified_busy_screens_read_busy() {
        // 1. An idle-looking screen mid-thought with a STATIC spinner row and an
        //    idle-looking footer.
        let static_spinner = screen(
            &[
                "⏺ Reading the harness helper before changing anything.",
                "",
                "✢ Thinking…",
                "",
            ],
            "  ? for shortcuts",
        );
        assert_eq!(worker_phase(&static_spinner), Phase::Busy);

        // 2. A spinner word without its timer (the first second).
        let no_timer = screen(&["✶ Deliberating…", ""], "  ? for shortcuts");
        assert_eq!(worker_phase(&no_timer), Phase::Busy);

        // 3. A DONE row whose footer still counts a background shell.
        let shell = screen(
            &[
                "⏺ Started the build in the background.",
                "",
                "✻ Cooked for 9m 27s · done 11:07 AM · 1 shell still running",
                "",
            ],
            "  ? for shortcuts",
        );
        assert_eq!(worker_phase(&shell), Phase::Busy);
        assert_eq!(
            busy_reason("✻ Cooked for 9m 27s · done 11:07 AM · 1 shell still running"),
            Some("a shell still running")
        );

        // 4. Auto mode: the footer drops `esc to interrupt`; the live spinner
        //    is the only signal.
        let auto = screen(
            &[
                "⏺ Running the tests.",
                "",
                "✻ Synthesizing… (18s · ↓ 1.1k tokens)",
                "",
            ],
            "  ⏵⏵ auto mode on (shift+tab to cycle)",
        );
        assert_eq!(worker_phase(&auto), Phase::Busy);
    }

    #[test]
    fn every_busy_signal_is_recognised_and_done_rows_are_not() {
        assert_eq!(busy_reason("  esc to interrupt"), Some("esc to interrupt"));
        assert_eq!(busy_reason("  Still working (12s)"), Some("Still working"));
        assert_eq!(
            busy_reason("  esc to interrupt · ctrl+b to run in background"),
            Some("esc to interrupt")
        );
        assert_eq!(
            busy_reason("  ctrl+b to run in background"),
            Some("ctrl+b to run in background")
        );
        assert_eq!(
            busy_reason("  Waiting for 2 dynamic workflows…"),
            Some("waiting for a dynamic workflow")
        );
        assert_eq!(
            busy_reason("  ⏵⏵ auto mode on · 2 shells still running"),
            Some("a shell still running")
        );
        assert_eq!(
            busy_reason("  ? for shortcuts · 1 shell ·"),
            Some("a shell still running")
        );
        assert_eq!(
            busy_reason("✻ Synthesizing… (18s · ↓ 1.1k tokens)"),
            Some("spinner row")
        );
        assert_eq!(busy_reason("  · Deliberating… (4s)"), Some("spinner row"));
        assert_eq!(busy_reason("* Thinking..."), Some("spinner row"));
        assert_eq!(busy_reason("✻ Cogitated for 2m 54s · done 2:41 PM"), None);
        assert_eq!(busy_reason("✻ Cooked for 9m 27s · done 11:07 AM"), None);
        assert_eq!(busy_reason("⏺ Merged and pushed."), None);
        assert_eq!(busy_reason("  · summarized"), None);
        assert_eq!(busy_reason("✶ …"), None);
        assert_eq!(busy_reason("✶…"), None);
        assert_eq!(busy_reason("  ⏵⏵ auto mode on (shift+tab to cycle)"), None);
    }

    /// A prompt box with a background shell in the footer is a PROMPT: the
    /// worker is blocked on it, whatever the shell does.
    #[test]
    fn a_prompt_beats_a_background_shell() {
        let mut r = bash_one_row();
        let last = r.len() - 1;
        r[last] = "  ⏵⏵ auto mode on · 1 shell still running".to_string();
        assert_eq!(worker_phase(&r), Phase::Prompt);
    }

    /// A `?` in the last said row is a question only when nothing is busy and
    /// no box is up; a `?` earlier in the transcript is not.
    #[test]
    fn question_reads_the_last_said_row_only() {
        let earlier = screen(
            &[
                "⏺ Should I continue?",
                "",
                "⏺ Continuing.",
                "",
                "✻ Cogitated for 4s · done 2:41 PM",
                "",
            ],
            "  ? for shortcuts",
        );
        assert_eq!(worker_phase(&earlier), Phase::Idle);
        let busy_q = screen(&["⏺ Should I continue?", ""], "  esc to interrupt");
        assert_eq!(worker_phase(&busy_q), Phase::Busy);
        // No composer at all (a plain shell): the last non-blank row decides.
        let shell = rows(&["$ ls", "a b", "Continue?", ""]);
        assert_eq!(worker_phase(&shell), Phase::Question);
    }

    /// The live worker's screen: the done row and an update banner sit between
    /// the transcript and the composer separator (which carries the git branch),
    /// the composer holds a dim suggestion after a no-break space. A question
    /// above them is still a question; a statement is idle.
    #[test]
    fn status_rows_under_the_transcript_do_not_hide_a_question() {
        let bottom = [
            "",
            "✻ Baked for 1m 10s · done 2:50 PM",
            "                                              ✔ Update installed · Restart to update",
            "──────────────────────────────────────────────────────────────────── feature-1 ─",
            "❯\u{a0}Wait for the tests, then commit and push as planned",
            "────────────────────────────────────────────────────────────────────────────────",
            "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents                /rc active",
        ];
        let mut question = rows(&[
            "⏺ Local main matches origin/main with nothing ahead or behind.",
            "",
            "  Should I commit and push now, or wait for the tests?",
        ]);
        question.extend(rows(&bottom));
        assert_eq!(worker_phase(&question), Phase::Question);
        assert!(
            is_placeholder(&question, 2),
            "the dim suggestion after ❯\u{a0}"
        );
        assert_eq!(
            composer_text(&question).as_deref(),
            Some("Wait for the tests, then commit and push as planned")
        );

        let mut idle = rows(&["⏺ Local main matches origin/main with nothing ahead or behind."]);
        idle.extend(rows(&bottom));
        assert_eq!(worker_phase(&idle), Phase::Idle);

        // The done row alone under a question is also skipped.
        let done = screen(
            &[
                "⏺ Which do you prefer?",
                "",
                "✻ Cogitated for 4s · done 2:41 PM",
                "",
            ],
            "  ? for shortcuts",
        );
        assert_eq!(worker_phase(&done), Phase::Question);
        assert!(is_status_row("✔ Update installed · Restart to update"));
        assert!(is_status_row("  ✻ Baked for 1m 10s · done 2:50 PM"));
        assert!(!is_status_row("⏺ Done?"));
        assert!(!is_status_row("  Which do you prefer?"));
    }

    #[test]
    fn placeholder_is_composer_text_with_the_cursor_at_column_two() {
        let mut r = screen(&["⏺ Done.", ""], "  ? for shortcuts");
        let c = composer_index(&r).expect("composer");
        assert_eq!(r[c], "❯");
        assert!(
            !is_placeholder(&r, 2),
            "an empty composer is not a placeholder"
        );
        r[c] = "❯ m7 is reachable as ssh m7, go".to_string();
        assert!(is_placeholder(&r, 2), "dim suggestion: cursor at column 2");
        assert!(!is_placeholder(&r, 32), "typed text: the cursor moved");
        assert_eq!(
            composer_text(&r).as_deref(),
            Some("m7 is reachable as ssh m7, go")
        );
        // The option rows of a prompt box are not the composer.
        let p = bash_one_row();
        let ci = composer_index(&p).expect("composer");
        assert_eq!(p[ci], "❯");
    }
}
