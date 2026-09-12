// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Parse Claude Code's approval box out of a screen. The shapes are the ones
//! measured on Claude Code 2.1.x (fixtures in the tests): a tool box headed
//! ` Bash command` (optionally `· from the "<name>" workflow`), an optional
//! ` Tip: auto mode …` row, the command (one indented row, or several rows each
//! led by `│`), its description, optional note rows, `Do you want to proceed?`,
//! the numbered options, and ` Esc to cancel · Tab to amend`; or the workflow
//! box ` Run a dynamic workflow?` with `│`-led description rows. Anything else
//! that says `Esc to cancel` is a prompt of kind [`PromptKind::Other`] — the
//! supervisor hands it to the manager rather than guess.

/// What the box is asking to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// ` Bash command` — run `command`.
    Bash,
    /// ` Edit file`.
    Edit,
    /// ` Write file` / ` Create file`.
    Write,
    /// ` Read file(s)`.
    Read,
    /// ` Run a dynamic workflow?`.
    Workflow,
    /// A box that says `Esc to cancel` in a shape this parser does not know.
    Other,
}

impl PromptKind {
    /// The lowercase word the CLI prints (`kind bash`).
    pub fn name(self) -> &'static str {
        match self {
            PromptKind::Bash => "bash",
            PromptKind::Edit => "edit",
            PromptKind::Write => "write",
            PromptKind::Read => "read",
            PromptKind::Workflow => "workflow",
            PromptKind::Other => "other",
        }
    }
}

/// One parsed approval box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    /// The box's kind.
    pub kind: PromptKind,
    /// The command line (Bash), the path (Edit/Write/Read), empty otherwise.
    pub command: String,
    /// The one-line description under the command; the workflow's description;
    /// the surrounding rows for [`PromptKind::Other`].
    pub description: String,
    /// The numbered options as `(number, text)` — `(1, "Yes")`,
    /// `(2, "Yes, and don't ask again for: git log *")`, `(4, "No")`.
    pub options: Vec<(u8, String)>,
    /// Whether an `Esc to cancel` row is present.
    pub has_cancel: bool,
}

/// The row that closes every approval box.
const CANCEL: &str = "Esc to cancel";

/// Parse the approval box on `rows`, if one is showing.
pub fn parse_prompt(rows: &[String]) -> Option<Prompt> {
    let esc = rows.iter().rposition(|r| r.contains(CANCEL))?;
    let has_cancel = true;
    let header = header_above(rows, esc);
    let Some((h, kind)) = header else {
        let start = esc.saturating_sub(30);
        let end = (esc + 10).min(rows.len());
        return Some(Prompt {
            kind: PromptKind::Other,
            command: String::new(),
            description: rows[start..end].join("\n"),
            options: options_in(&rows[start..esc]),
            has_cancel,
        });
    };
    let body = &rows[h + 1..esc];
    let options = options_in(body);
    if kind == PromptKind::Workflow {
        let description = body
            .iter()
            .map(|r| r.trim())
            .filter_map(|r| r.strip_prefix('│'))
            .map(str::trim)
            .collect::<Vec<_>>()
            .join(" ");
        return Some(Prompt {
            kind,
            command: String::new(),
            description,
            options,
            has_cancel,
        });
    }
    // The content rows run up to the question (or the first option row).
    let stop = body
        .iter()
        .position(|r| r.trim().starts_with("Do you want to proceed?") || option_row(r).is_some())
        .unwrap_or(body.len());
    // The first blank-separated block after the header, tips dropped, is the
    // command and its description; later blocks are notes.
    let mut block: Vec<&str> = Vec::new();
    for r in &body[..stop] {
        let t = r.trim();
        if t.starts_with("Tip:") {
            continue;
        }
        if t.is_empty() {
            if block.is_empty() {
                continue;
            }
            break;
        }
        block.push(t);
    }
    let (command, description) = if kind == PromptKind::Bash {
        let bars: Vec<&str> = block
            .iter()
            .filter_map(|r| r.strip_prefix('│'))
            .map(str::trim)
            .collect();
        if bars.is_empty() {
            let cmd = block.first().copied().unwrap_or("").to_string();
            let desc = block.iter().skip(1).copied().collect::<Vec<_>>().join(" ");
            (cmd, desc)
        } else {
            let desc = block
                .iter()
                .filter(|r| !r.starts_with('│'))
                .copied()
                .collect::<Vec<_>>()
                .join(" ");
            (bars.join(" "), desc)
        }
    } else {
        (
            block.first().copied().unwrap_or("").to_string(),
            String::new(),
        )
    };
    Some(Prompt {
        kind,
        command,
        description,
        options,
        has_cancel,
    })
}

/// The inclusive row span of the box (`header..=Esc row`; for an unknown shape,
/// the same window the description covers), for printing it verbatim.
pub fn prompt_box_span(rows: &[String]) -> Option<(usize, usize)> {
    let esc = rows.iter().rposition(|r| r.contains(CANCEL))?;
    let start = match header_above(rows, esc) {
        Some((h, _)) => h,
        None => esc.saturating_sub(30),
    };
    Some((start, esc))
}

/// The nearest header row above `esc` (within the box's plausible height).
fn header_above(rows: &[String], esc: usize) -> Option<(usize, PromptKind)> {
    let floor = esc.saturating_sub(60);
    (floor..esc)
        .rev()
        .find_map(|i| header_kind(&rows[i]).map(|k| (i, k)))
}

fn header_kind(row: &str) -> Option<PromptKind> {
    let t = row.trim();
    if t.starts_with("Bash command") {
        Some(PromptKind::Bash)
    } else if t.starts_with("Edit file") {
        Some(PromptKind::Edit)
    } else if t.starts_with("Write file") || t.starts_with("Create file") {
        Some(PromptKind::Write)
    } else if t.starts_with("Read file") {
        Some(PromptKind::Read)
    } else if t.starts_with("Run a dynamic workflow?") {
        Some(PromptKind::Workflow)
    } else {
        None
    }
}

/// `❯ 1. Yes` / `  2. No` → `(1, "Yes")`.
fn option_row(row: &str) -> Option<(u8, String)> {
    let t = row.trim_start();
    let t = t.strip_prefix('❯').map(str::trim_start).unwrap_or(t);
    let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.len() > 2 {
        return None;
    }
    let rest = t[digits.len()..].strip_prefix('.')?;
    if !rest.starts_with(' ') {
        return None;
    }
    Some((digits.parse().ok()?, rest.trim().to_string()))
}

fn options_in(rows: &[String]) -> Vec<(u8, String)> {
    rows.iter().filter_map(|r| option_row(r)).collect()
}

#[cfg(test)]
pub(crate) mod fixtures {
    /// The composer + footer every idle-or-prompt screen ends with (auto mode
    /// off): the separator, the caret row, the separator, the hint row.
    pub fn composer(footer: &str) -> Vec<String> {
        vec![
            "─".repeat(120),
            "❯".to_string(),
            "─".repeat(120),
            footer.to_string(),
        ]
    }

    pub fn rows(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    /// A one-row Bash box, permission mode (four options).
    pub fn bash_one_row() -> Vec<String> {
        let mut r = rows(&[
            "⏺ Let me look at the recent history.",
            "",
            " Bash command",
            "",
            "   git log --oneline -5",
            "   Show the five most recent commits",
            "",
            " Do you want to proceed?",
            " ❯ 1. Yes",
            "   2. Yes, and don’t ask again for: git log *",
            "   3. Yes, and switch to auto mode · Claude edits, runs, and asks only for the risky ones",
            "   4. No",
            "",
            " Esc to cancel · Tab to amend",
        ]);
        r.extend(composer("  ? for shortcuts"));
        r
    }

    /// A multi-row Bash box from a workflow, auto mode (three options), with a
    /// note row and a tip row.
    pub fn bash_multi_row() -> Vec<String> {
        let mut r = rows(&[
            " Bash command · from the \"verify-merge\" workflow",
            " Tip: auto mode approves reads for you",
            "",
            "   │ cd ~/ay && git status --short --branch && git pull 2>&1 | tail",
            "   │ -20",
            "   Sync the checkout before verifying",
            "",
            " This command requires approval",
            " Do you want to proceed?",
            " ❯ 1. Yes",
            "   2. Yes, and don’t ask again for: git pull *",
            "   3. No",
            "",
            " Esc to cancel · Tab to amend",
        ]);
        r.extend(composer("  ⏵⏵ auto mode on (shift+tab to cycle)"));
        r
    }

    pub fn workflow_box() -> Vec<String> {
        let mut r = rows(&[
            " Run a dynamic workflow?",
            "  │ Fan out the 12 benchmark families to 4 agents and collect the",
            "  │ standings table.",
            "",
            "  ❯ 1. Yes, run it",
            "    2. View raw script",
            "    3. No",
            "",
            "  Esc to cancel · Tab to amend",
        ]);
        r.extend(composer("  ? for shortcuts"));
        r
    }

    pub fn edit_box() -> Vec<String> {
        let mut r = rows(&[
            " Edit file",
            "",
            "   crates/ay-test-support/src/lib.rs",
            "",
            "   2158    -    let cargo = \"cargo\";",
            "   2158    +    let cargo = std::env::var(\"CARGO\").unwrap_or_else(|_| \"cargo\".into());",
            "",
            " Do you want to make this edit to lib.rs?",
            " ❯ 1. Yes",
            "   2. Yes, allow all edits during this session (shift+tab)",
            "   3. No",
            "",
            " Esc to cancel · Tab to amend",
        ]);
        r.extend(composer("  ? for shortcuts"));
        r
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    #[test]
    fn a_one_row_bash_box_parses_command_description_and_four_options() {
        let p = parse_prompt(&bash_one_row()).expect("a prompt");
        assert_eq!(p.kind, PromptKind::Bash);
        assert_eq!(p.command, "git log --oneline -5");
        assert_eq!(p.description, "Show the five most recent commits");
        assert!(p.has_cancel);
        assert_eq!(p.options.len(), 4, "{:?}", p.options);
        assert_eq!(p.options[0], (1, "Yes".to_string()));
        assert_eq!(
            p.options[1],
            (2, "Yes, and don’t ask again for: git log *".to_string())
        );
        assert!(p.options[2].1.starts_with("Yes, and switch to auto mode"));
        assert_eq!(p.options[3], (4, "No".to_string()));
        assert_eq!(prompt_box_span(&bash_one_row()), Some((2, 13)));
    }

    /// `│`-led rows are ONE command joined with single spaces; the workflow
    /// suffix on the header, the tip and the note rows are not content; auto
    /// mode has three options.
    #[test]
    fn a_multi_row_bash_box_joins_the_bars_and_skips_tips_and_notes() {
        let p = parse_prompt(&bash_multi_row()).expect("a prompt");
        assert_eq!(p.kind, PromptKind::Bash);
        assert_eq!(
            p.command,
            "cd ~/ay && git status --short --branch && git pull 2>&1 | tail -20"
        );
        assert_eq!(p.description, "Sync the checkout before verifying");
        assert_eq!(
            p.options,
            vec![
                (1, "Yes".to_string()),
                (2, "Yes, and don’t ask again for: git pull *".to_string()),
                (3, "No".to_string()),
            ]
        );
    }

    #[test]
    fn a_workflow_box_is_kind_workflow_with_its_description() {
        let p = parse_prompt(&workflow_box()).expect("a prompt");
        assert_eq!(p.kind, PromptKind::Workflow);
        assert_eq!(p.command, "");
        assert_eq!(
            p.description,
            "Fan out the 12 benchmark families to 4 agents and collect the standings table."
        );
        assert_eq!(p.options[0], (1, "Yes, run it".to_string()));
        assert_eq!(p.options[1], (2, "View raw script".to_string()));
        assert_eq!(p.options[2], (3, "No".to_string()));
    }

    #[test]
    fn an_edit_box_is_kind_edit_with_the_path() {
        let p = parse_prompt(&edit_box()).expect("a prompt");
        assert_eq!(p.kind, PromptKind::Edit);
        assert_eq!(p.command, "crates/ay-test-support/src/lib.rs");
        assert_eq!(p.options.len(), 3);
    }

    /// An unknown box that still says `Esc to cancel` is kind Other, carrying the
    /// surrounding rows so the manager can read it; no cancel row → no prompt.
    #[test]
    fn an_unknown_box_is_other_and_a_plain_screen_is_none() {
        let mut r = rows(&[
            "⏺ Done.",
            "",
            " Select a model",
            " ❯ 1. Opus",
            "   2. Sonnet",
            "",
            " Esc to cancel",
        ]);
        r.extend(composer("  ? for shortcuts"));
        let p = parse_prompt(&r).expect("a prompt");
        assert_eq!(p.kind, PromptKind::Other);
        assert!(p.description.contains("Select a model"));
        assert_eq!(
            p.options,
            vec![(1, "Opus".to_string()), (2, "Sonnet".to_string())]
        );
        assert!(p.has_cancel);

        let mut idle = rows(&["⏺ Done.", "", "✻ Cogitated for 2m 54s · done 2:41 PM", ""]);
        idle.extend(composer("  ? for shortcuts"));
        assert_eq!(parse_prompt(&idle), None);
    }

    #[test]
    fn option_rows_need_a_number_a_dot_and_a_space() {
        assert_eq!(option_row(" ❯ 1. Yes"), Some((1, "Yes".to_string())));
        assert_eq!(option_row("   12. Many"), Some((12, "Many".to_string())));
        assert_eq!(option_row("   2.5 GB free"), None);
        assert_eq!(option_row("   2158    -    let x = 1;"), None);
        assert_eq!(option_row("  1.Yes"), None);
        assert_eq!(option_row("  ❯ 3. No"), Some((3, "No".to_string())));
    }
}
