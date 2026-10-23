// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A CANDIDATE COMMAND FOR A BARRED BASH BOX, TAKEN FROM THE AGENT'S TRANSCRIPT AND CHECKED FOR THE SAME DRAWN ROWS (`docs/measured/breaker-transcript-and-zsh-2026-10-22.md`).

use aterm_phase::prompt::{PromptKind, RmBreakerKind};
use aterm_phase::{Program, PromptV2};

use super::box_call::PendingCall;

/// How many lag steps ([`super::box_call::LAG_STEP`]) the loop waits after it finds a candidate before it reads the transcript again: the candidate must be the same call, alone and waiting, after it.
pub const SETTLE_STEPS: u32 = 4;

/// The one pending Bash call of the agent's transcript, its id and command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoxCommand {
    pub id: String,
    pub command: String,
}

/// THE CALL A BARRED BASH BOX MAY BE DRAWN FOR, from a transcript tail: the main chain's calls with no result in the conversation's order ([`super::box_call::pending_calls`], the reader the Edit and Write lane shares: a result answers its call wherever it is written, and a user row with no result ends every call written before it unless it is the call's own ancestor; MEASURED 2026-10-22, a first turn's call row written before its prompt row), less the calls in `spent`.
/// # Errors
/// None left, more than one, one that is no Bash call, or a Bash call with no command: each names what it found.
pub fn pending_bash(jsonl: &str, spent: &[String]) -> Result<BoxCommand, String> {
    let live: Vec<PendingCall> = super::box_call::pending_calls(jsonl)
        .into_iter()
        .filter(|c| !spent.contains(&c.id))
        .collect();
    match live.as_slice() {
        [] => Err("the transcript holds no call waiting that no earlier box spent".to_string()),
        [call] => {
            if call.tool != "Bash" {
                return Err(format!(
                    "the call waiting is {}'s, not a Bash call",
                    call.tool
                ));
            }
            let command = call
                .command
                .clone()
                .ok_or_else(|| "the Bash call waiting has no command".to_string())?;
            Ok(BoxCommand {
                id: call.id.clone(),
                command,
            })
        }
        many => Err(format!(
            "the transcript holds {} calls waiting: none is the box's alone",
            many.len()
        )),
    }
}

/// Whether the loop reads the agent's transcript for the box `p` that `program` draws: only Claude Code writes the transcript this module reads, only a box [`may_prove`] admits is matched to a command from it, and only a box the policy would hand a person on the screen's own readings (`escalates`, asked last: it decides the box) is worth the read, its lag retries and its settle.
pub fn wanted(program: Program, p: &PromptV2, escalates: impl FnOnce() -> bool) -> bool {
    let claude = match program {
        Program::Claude => true,
        Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => false,
    };
    claude && may_prove(p) && escalates()
}

/// Whether the box `p` is one a transcript's command is matched to: Claude Code's `Bash command` box carrying the rm breaker's too-many note ([`RmBreakerKind::TooManySubstitutions`]), its command in barred rows (`gutter`), read whole (its title on the screen), from the main agent (no ` · from …` origin in its title), and a footer of its own keys alone (a subagent's box ends its footer in its background agents' note).
#[must_use]
pub fn may_prove(p: &PromptV2) -> bool {
    p.kind == PromptKind::Bash
        && p.base_title().as_deref() == Some(aterm_phase::anchor("box.bash"))
        && p.gutter
        && !p.head_off_screen
        && p.origin().is_none()
        && p.footer
            .as_deref()
            .is_some_and(|f| aterm_phase::footer_keys(f).len() == f.split(" · ").count())
        && p.rm_breaker()
            .is_some_and(|b| b.kind == RmBreakerKind::TooManySubstitutions)
}

/// The East Asian Wide and Fullwidth blocks this model draws, each two cells
const WIDE: &[(char, char)] = &[
    ('\u{3041}', '\u{3096}'),
    ('\u{30a1}', '\u{30fa}'),
    ('\u{3400}', '\u{4dbf}'),
    ('\u{4e00}', '\u{9fff}'),
    ('\u{ac00}', '\u{d7a3}'),
    ('\u{ff01}', '\u{ff60}'),
    ('\u{ffe0}', '\u{ffe6}'),
];

/// How many cells Claude Code gives `c` where this model draws it: two for
fn cells(c: char) -> usize {
    if WIDE.iter().any(|(a, b)| (*a..=*b).contains(&c)) {
        2
    } else {
        1
    }
}

/// The cells `text` takes ([`cells`]).
fn width(text: &str) -> usize {
    text.chars().map(cells).sum()
}

/// `Err` naming the first character of `command` this model does not draw:
fn drawable(command: &str) -> Result<(), String> {
    match command
        .chars()
        .find(|c| !matches!(c, '\n' | '\t' | ' '..='~') && cells(*c) != 2)
    {
        Some(c) => Err(format!(
            "the command holds `{}`, which this check does not draw again",
            c.escape_default()
        )),
        None => Ok(()),
    }
}

/// `line` with each tab replaced by spaces to the next stop of eight cells,
fn expand_tabs(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut column = 0;
    for c in line.chars() {
        if c == '\t' {
            let blanks = 8 - column % 8;
            out.extend(std::iter::repeat_n(' ', blanks));
            column += blanks;
        } else {
            out.push(c);
            column += cells(c);
        }
    }
    out
}

/// `text`'s rows `width` cells wide as Claude Code's renderer wraps it: each
fn wrapped(text: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    for line in text.split('\n') {
        for (k, row) in wrap_line(&expand_tabs(line), width).into_iter().enumerate() {
            let row = match row.strip_prefix(' ') {
                Some(rest) if k > 0 => rest.to_string(),
                _ => row,
            };
            rows.push(row);
        }
    }
    rows
}

/// THE ROWS CLAUDE CODE DRAWS `command` IN, in a barred box whose text is `width` cells wide ([`wrapped`]; MEASURED 2026-10-22, eleven 2.1.307 captures at 144 and 100 columns, and the 2.1.308 captures of the measured note's §W: tabs, CJK, kana, Hangul and fullwidth forms, a wide character one cell short of a row's end going to the next row).
/// # Errors
/// A character this model does not draw, or a width too narrow to read.
pub fn barred_rows(command: &str, width: usize) -> Result<Vec<String>, String> {
    if width < 8 {
        return Err(format!("a box {width} cells wide"));
    }
    drawable(command)?;
    Ok(wrapped(command, width))
}

/// How many UTF-16 units of a command the call's row shows at most, and how
const HEADER_UNITS: usize = 160;
const HEADER_LINES: usize = 2;

/// THE ROWS OF THE CALL'S OWN ROW ABOVE THE BOX, `⏺ Bash(…)` (MEASURED 2026-10-22 on 2.1.307 and 2.1.308, the measured note's §H), each without its six-cell lead (`⏺ Bash` on the first, blanks on the rest): Claude Code shows a command of more than two lines or more than 160 UTF-16 units as its first two lines, cut to 160 units, trimmed, and `…`; any other command as it is.
/// # Errors
/// A command whose row this model does not draw, or a width too narrow.
pub fn header_rows(command: &str, width: usize) -> Result<Vec<String>, String> {
    if width < 8 {
        return Err(format!("a call's row {width} cells wide"));
    }
    drawable(command)?;
    let lines: Vec<&str> = command.split('\n').collect();
    let comment = |l: &str| l.trim_matches([' ', '\t']).starts_with('#');
    if lines.first().is_some_and(|l| comment(l))
        && lines
            .iter()
            .skip(1)
            .all(|l| comment(l) || l.trim_matches([' ', '\t']).is_empty())
    {
        return Err("the call's row may show the command's comment in its place".to_string());
    }
    let text = if lines.len() > HEADER_LINES || command.chars().count() > HEADER_UNITS {
        let head: String = lines[..HEADER_LINES.min(lines.len())]
            .join("\n")
            .chars()
            .take(HEADER_UNITS)
            .collect();
        format!("{}\u{2026}", head.trim_matches([' ', '\t', '\n']))
    } else {
        command.to_string()
    };
    Ok(wrapped(&format!("({text})"), width))
}

/// One line wrapped as `wrap-ansi` 9 wraps it with `{hard: true, trim:
fn wrap_line(line: &str, columns: usize) -> Vec<String> {
    let mut rows = vec![String::new()];
    for (index, word) in line.split(' ').enumerate() {
        let mut row_len = rows.last().map_or(0, |r| width(r));
        if index != 0 {
            if row_len >= columns {
                rows.push(String::new());
                row_len = 0;
            }
            if let Some(row) = rows.last_mut() {
                row.push(' ');
            }
            row_len += 1;
        }
        let len = width(word);
        if len > columns {
            let remaining = columns.saturating_sub(row_len);
            let starting_this_row = 1 + (len - remaining - 1) / columns;
            let starting_next_row = (len - 1) / columns;
            if starting_next_row < starting_this_row {
                rows.push(String::new());
            }
            wrap_word(&mut rows, word, columns);
            continue;
        }
        if row_len + len > columns && row_len > 0 && len > 0 {
            rows.push(String::new());
        }
        if let Some(row) = rows.last_mut() {
            row.push_str(word);
        }
    }
    rows
}

/// `wrap-ansi`'s hard break of a word wider than a row: its characters
fn wrap_word(rows: &mut Vec<String>, word: &str, columns: usize) {
    let chars: Vec<char> = word.chars().collect();
    let mut visible = rows.last().map_or(0, |r| width(r));
    for (index, c) in chars.iter().enumerate() {
        let w = cells(*c);
        if visible + w <= columns {
            if let Some(row) = rows.last_mut() {
                row.push(*c);
            }
        } else {
            rows.push(c.to_string());
            visible = 0;
        }
        visible += w;
        if visible == columns && index + 1 < chars.len() {
            rows.push(String::new());
            visible = 0;
        }
    }
}

/// The row under the call's own row while its box stands: `⎿`, a space, a
const WAITING: &str = "  \u{23bf} \u{a0}Waiting\u{2026}";

/// `Ok` when `command` is what the box `p` (on `rows`) draws: the box passes [`may_prove`], its top rule of `─` gives its width (the text is four cells narrower: ` │ ` and a column of margin, MEASURED at 144 and 100 columns), and the rows between its first and last dashed rule, each with its trailing blanks dropped, are exactly ` │ ` and each of [`barred_rows`] with its trailing blanks dropped, as many as it has.
/// # Errors
/// Why not: a box of another shape, no rule or no dashed rules, a command this check does not draw, another count of rows, the first row that differs, or a call's row that is not the command's.
pub fn drawn_as(p: &PromptV2, rows: &[String], command: &str) -> Result<(), String> {
    if !may_prove(p) {
        return Err("the box is not one a transcript's command is matched to".to_string());
    }
    let (top, bottom) = p.span;
    let rule_at = top
        .checked_sub(1)
        .ok_or_else(|| "the box has no top rule to measure it by".to_string())?;
    let rule = rows
        .get(rule_at)
        .map(|r| r.trim_end())
        .filter(|r| !r.is_empty() && r.chars().all(|c| c == '─'))
        .ok_or_else(|| "the box has no top rule to measure it by".to_string())?;
    let width = rule.chars().count().saturating_sub(4);
    let span = top..=bottom.min(rows.len().saturating_sub(1));
    let dashed: Vec<usize> = span.filter(|&k| is_dashed_rule(&rows[k])).collect();
    let (Some(&open), Some(&close)) = (dashed.first(), dashed.last()) else {
        return Err("the box has no dashed rules around its command".to_string());
    };
    if open >= close {
        return Err("the box has no dashed rules around its command".to_string());
    }
    let drawn: Vec<String> = barred_rows(command, width)?
        .into_iter()
        .map(|r| format!(" \u{2502} {r}").trim_end().to_string())
        .collect();
    let shown: Vec<&str> = rows[open + 1..close].iter().map(|r| r.trim_end()).collect();
    if drawn.len() != shown.len() {
        return Err(format!(
            "the command draws {} rows {width} cells wide, and the box shows {}",
            drawn.len(),
            shown.len()
        ));
    }
    if let Some(k) = drawn.iter().zip(&shown).position(|(d, s)| d != s) {
        return Err(format!(
            "the command's row {} draws otherwise than the box's",
            k + 1
        ));
    }
    called_as(&rows[..rule_at], command, width.saturating_sub(2))
}

/// `Err` naming a form of `text`, a reading of a Bash box's rows, in which the shell may read a blank the screen does not show.
/// # Errors
/// The form found.
pub fn blanks_read_as_drawn(text: &str) -> Result<(), String> {
    let chars: Vec<char> = text.chars().collect();
    let mut k = 0;
    while k < chars.len() {
        let run = chars[k..].iter().take_while(|c| **c == '<').count();
        if run >= 2 && run != 3 {
            return Err(
                "it holds a here-document (`<<`), whose lines the shell matches blank for blank"
                    .to_string(),
            );
        }
        k += run.max(1);
    }
    let blank = |at: usize| matches!(chars.get(at), Some(' ' | '\t'));
    let ends = |at: usize| matches!(chars.get(at), None | Some('\n'));
    for (k, c) in chars.iter().enumerate() {
        if *c != '\\' {
            continue;
        }
        if ends(k + 1) {
            return Err(
                "a backslash ends a line of it, where a blank the screen does not show \
                        would end the line instead of joining the next"
                    .to_string(),
            );
        }
        if chars.get(k + 1) == Some(&'\t') || (blank(k + 1) && (blank(k + 2) || ends(k + 2))) {
            return Err(
                "a backslash stands before blanks that may be a tab, which the shell \
                        reads escaped as one word with what follows"
                    .to_string(),
            );
        }
    }
    Ok(())
}

/// `Ok` when the rows right above a box's top rule (`above`, the screen's
fn called_as(above: &[String], command: &str, width: usize) -> Result<(), String> {
    let row = |k: usize| above.get(k).map(|r| r.trim_end());
    let n = above.len();
    if n < 2 || row(n - 1) != Some("") || row(n - 2) != Some(WAITING) {
        return Err("the box is not right under a Bash call waiting on it".to_string());
    }
    let header = header_rows(command, width).map_err(|why| format!("the call's row: {why}"))?;
    let first = n
        .checked_sub(2 + header.len())
        .ok_or_else(|| "the call's row is not all on the screen".to_string())?;
    for (k, text) in header.iter().enumerate() {
        let shown = row(first + k).unwrap_or_default();
        let ok = if k == 0 {
            ["\u{23fa} Bash", "  Bash"]
                .iter()
                .any(|lead| format!("{lead}{text}").trim_end() == shown)
        } else {
            format!("      {text}").trim_end() == shown
        };
        if !ok {
            return Err(format!(
                "the call's row {} above the box draws otherwise than the command's",
                k + 1
            ));
        }
    }
    Ok(())
}

/// A dashed rule, Claude Code's own chrome around a tool's content: a row at
fn is_dashed_rule(row: &str) -> bool {
    let t = row.trim_end();
    t.starts_with('\u{254c}') && t.chars().all(|c| c == '\u{254c}') && t.chars().count() >= 10
}

#[cfg(test)]
#[path = "box_command_tests.rs"]
pub(crate) mod tests;
