// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE COMMAND A BARRED BASH BOX DRAWS, TAKEN FROM THE AGENT'S TRANSCRIPT AND
//! PROVEN BY DRAWING IT AGAIN (`docs/measured/breaker-transcript-and-zsh-2026-10-22.md`).
//!
//! Claude Code draws a Bash command longer than 80 cells, or with a newline in
//! it, in barred rows (` │ …`), and its renderer wraps a long line onto
//! barred rows of the same look. So the screen cannot tell a wrap from a
//! newline, and the read-only checks judge every join of the rows
//! ([`aterm_phase::PromptV2::readings`]). The rm breaker's too-many box
//! (2.1.307, `breaker-read-only`) draws one long line of 65 `$(true)` over
//! four such rows, and its newline join puts a `$(true)` where a command's
//! name goes: no reading of the screen alone proves it a read.
//!
//! The agent's transcript holds the call's exact command (the pending Bash
//! `tool_use`, `input.command`). It is taken for the box only where every
//! link of the join holds:
//!
//! * THE FILE IS THE TAB'S: the host finds it from the agent's own process
//!   (`sessions/<pid>.json`, its `procStart` checked, its `sessionId`, the
//!   project folder of its `cwd`), as the footer and the wall reader do
//!   (`crate::supervise::IdleHost::transcript_tail`).
//! * THE CALL IS THE ONE WAITING: the main chain holds exactly one call with
//!   no result, not spent on an earlier box, and it is a Bash call
//!   ([`pending_bash`], which reads the conversation's own order where the
//!   file's differs: MEASURED, a first turn's prompt row written after its
//!   call); a read a settle ([`SETTLE_STEPS`]) or more after the call's
//!   first sighting on the box's mount must find the same call with the
//!   same command, still alone and still waiting. MEASURED: the box draws 60
//!   to 82 ms BEFORE its call's row reaches the file (10 runs), so the loop
//!   retries a file that names none ([`super::box_call::LAG_TRIES`]).
//! * THE SCREEN DRAWS THAT TEXT: the call's command, wrapped the way 2.1.307
//!   draws a barred box ([`barred_rows`]), is the box's rows between its
//!   dashed rules, row for row and character for character ([`drawn_as`]).
//!
//! Then the policy judges that one command and no join. The transcript alone
//! is never a proof: a candidate that does not draw as the box's rows is no
//! reading, and the box is judged on the screen's readings as before.
//!
//! A call taken is spent when its box leaves or a new mount begins, so it
//! answers no later box. What this does not close (the measured note's
//! residual): an OLDER Bash call the loop never took (its box came and went
//! between the loop's reads, or stood while the file named no call, and a
//! person answered it) still shows no result because the agent's writer
//! stalled past the settle, the new box's own call row is held back as
//! long, and the older call's text draws exactly as the new box's rows (the
//! same characters with a wrap where the box's call has a newline, or the
//! reverse). A word named `rm` or `rmdir` is still refused there: it is the
//! same characters.

use std::collections::HashMap;

use aterm_json::Value;
use aterm_phase::prompt::{PromptKind, RmBreakerKind};
use aterm_phase::{Program, PromptV2};

/// How many lag steps ([`super::box_call::LAG_STEP`]) the loop waits after it
/// finds a candidate before it reads the transcript again: the candidate
/// must be the same call, alone and waiting, after it. Four steps (1 s) is
/// over ten times the measured lag (82 ms at most, 2.1.307) and nearly three
/// times the 350 ms a PreToolUse hook held a row back (2.1.300).
pub const SETTLE_STEPS: u32 = 4;

/// The one pending Bash call of the agent's transcript, its id and command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoxCommand {
    pub id: String,
    pub command: String,
}

/// A tool call of the main chain: the row it is written on (its place in the
/// file and its `uuid`), its id, tool and `input.command`.
struct Call {
    row: usize,
    uuid: Option<String>,
    id: String,
    tool: String,
    command: Option<String>,
}

/// THE CALL A BARRED BASH BOX MAY BE DRAWN FOR, from a transcript tail: the
/// main chain's calls with no result (only `user` and `assistant` rows, no
/// subagent's `isSidechain` row), less the calls in `spent`. An assistant
/// row's `tool_use` parts are calls; a `tool_result` in any user row of the
/// tail answers its call, wherever it is written; and a user row with no
/// result (a person's prompt, an interrupt's note, a meta row) ends every
/// call written before it, unless it is that call's own ANCESTOR (on the
/// call row's `parentUuid` chain, followed through every row of the tail
/// that has a `uuid`): MEASURED 2026-10-22 (2.1.307, a bypass session's
/// first turn), Claude Code wrote the assistant row of the call before the
/// prompt row and the attachment rows it leads from, so in file order the
/// call's own prompt follows it. `Ok` only when exactly one call is left
/// and it is a Bash call whose `input.command` is a string. Any row that
/// does not parse is passed over (a torn first row of a tail).
///
/// # Errors
///
/// None left, more than one, one that is no Bash call, or a Bash call with
/// no command: each names what it found.
pub fn pending_bash(jsonl: &str, spent: &[String]) -> Result<BoxCommand, String> {
    let mut parent: HashMap<String, String> = HashMap::new();
    let mut calls: Vec<Call> = Vec::new();
    let mut answered: Vec<String> = Vec::new();
    let mut prompts: Vec<(usize, Option<String>)> = Vec::new();
    for (row, line) in jsonl.lines().enumerate() {
        let Ok(value) = aterm_json::from_str::<Value>(line) else {
            continue;
        };
        let uuid = value.get("uuid").and_then(Value::as_str).map(str::to_owned);
        if let (Some(u), Some(p)) = (
            uuid.as_ref(),
            value.get("parentUuid").and_then(Value::as_str),
        ) {
            parent.insert(u.clone(), p.to_owned());
        }
        let kind = value.get("type").and_then(Value::as_str);
        if !matches!(kind, Some("user" | "assistant"))
            || value.get("isSidechain").and_then(Value::as_bool) == Some(true)
        {
            continue;
        }
        let parts = value
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        let part_type = |p: &Value| p.get("type").and_then(Value::as_str).map(str::to_owned);
        if kind == Some("assistant") {
            for p in parts
                .iter()
                .filter(|p| part_type(p).as_deref() == Some("tool_use"))
            {
                let (Some(id), Some(tool)) = (
                    p.get("id").and_then(Value::as_str),
                    p.get("name").and_then(Value::as_str),
                ) else {
                    continue;
                };
                calls.push(Call {
                    row,
                    uuid: uuid.clone(),
                    id: id.to_owned(),
                    tool: tool.to_owned(),
                    command: p
                        .get("input")
                        .and_then(|i| i.get("command"))
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                });
            }
            continue;
        }
        let results: Vec<String> = parts
            .iter()
            .filter(|p| part_type(p).as_deref() == Some("tool_result"))
            .filter_map(|p| p.get("tool_use_id").and_then(Value::as_str))
            .map(str::to_owned)
            .collect();
        if results.is_empty() {
            prompts.push((row, uuid));
        } else {
            answered.extend(results);
        }
    }
    let ended = |call: &Call| {
        prompts.iter().any(|(row, uuid)| {
            *row > call.row
                && !matches!(
                    (uuid, &call.uuid),
                    (Some(prompt), Some(of)) if ancestor(&parent, of, prompt)
                )
        })
    };
    let live: Vec<&Call> = calls
        .iter()
        .filter(|c| !answered.contains(&c.id) && !spent.contains(&c.id) && !ended(c))
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

/// Whether `who` is on the `parentUuid` chain above the row `of`, followed
/// through `parent` (each row's `uuid` to its parent's) for at most as many
/// steps as it has rows: a chain that leaves the tail, or loops, is no proof.
fn ancestor(parent: &HashMap<String, String>, of: &str, who: &str) -> bool {
    let mut at = of;
    for _ in 0..parent.len() {
        let Some(up) = parent.get(at) else {
            return false;
        };
        if up == who {
            return true;
        }
        at = up;
    }
    false
}

/// Whether the loop reads the agent's transcript for the box `p` that
/// `program` draws: only Claude Code writes the transcript this module reads,
/// only a box [`may_prove`] admits is matched to a command from it, and only
/// a box the policy would hand a person on the screen's own readings
/// (`escalates`, asked last: it decides the box) is worth the read, its lag
/// retries and its settle. A box the screen's readings already decide costs
/// no read and spends no call.
pub fn wanted(program: Program, p: &PromptV2, escalates: impl FnOnce() -> bool) -> bool {
    let claude = match program {
        Program::Claude => true,
        Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => false,
    };
    claude && may_prove(p) && escalates()
}

/// Whether the box `p` is one a transcript's command is matched to: Claude
/// Code's `Bash command` box carrying the rm breaker's too-many note
/// ([`RmBreakerKind::TooManySubstitutions`]), its command in barred rows
/// (`gutter`), read whole (its title on the screen), from the main agent (no
/// ` · from …` origin in its title), and a footer of its own keys alone (a
/// subagent's box ends its footer in its background agents' note).
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

/// THE ROWS CLAUDE CODE 2.1.307 DRAWS `command` IN, in a barred box whose
/// text is `width` cells wide (MEASURED 2026-10-22, eleven captures at 144
/// and 100 columns): each line (split at `\n`) wrapped on its own by the
/// renderer's `wrap-ansi` with `hard` and no `trim` ([`wrap_line`]), a row a
/// wrap starts losing its one leading space, an empty line an empty row.
/// Only printable ASCII and `\n` are drawn by this model: a tab (expanded to
/// a stop of eight from the line's start, measured but not modelled here), a
/// wide or combining character, a control character or a `\r` is refused.
///
/// # Errors
///
/// A character this model does not draw, or a width too narrow to read.
pub fn barred_rows(command: &str, width: usize) -> Result<Vec<String>, String> {
    if width < 8 {
        return Err(format!("a box {width} cells wide"));
    }
    if let Some(c) = command
        .chars()
        .find(|c| *c != '\n' && !(' '..='~').contains(c))
    {
        return Err(format!(
            "the command holds `{}`, which this check does not draw again",
            c.escape_default()
        ));
    }
    let mut rows = Vec::new();
    for line in command.split('\n') {
        for (k, row) in wrap_line(line, width).into_iter().enumerate() {
            let row = match row.strip_prefix(' ') {
                Some(rest) if k > 0 => rest.to_string(),
                _ => row,
            };
            rows.push(row);
        }
    }
    Ok(rows)
}

/// One line wrapped as `wrap-ansi` 9 wraps it with `{hard: true, trim:
/// false}` (word wrap on), over printable ASCII, where a character is one
/// cell: the line split at single spaces into words; a space put before
/// every word but the first, on a new row when the row is already full; a
/// word that does not fit on a row with something on it goes to the next;
/// a word wider than `columns` is broken across rows, starting on a fresh
/// one when that saves a break.
fn wrap_line(line: &str, columns: usize) -> Vec<String> {
    let mut rows = vec![String::new()];
    for (index, word) in line.split(' ').enumerate() {
        let mut row_len = rows.last().map_or(0, String::len);
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
        let len = word.len();
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
/// appended to the last row, a fresh row begun wherever the row is full and
/// more characters follow.
fn wrap_word(rows: &mut Vec<String>, word: &str, columns: usize) {
    let chars: Vec<char> = word.chars().collect();
    let mut visible = rows.last().map_or(0, String::len);
    for (index, c) in chars.iter().enumerate() {
        if visible < columns {
            if let Some(row) = rows.last_mut() {
                row.push(*c);
            }
        } else {
            rows.push(c.to_string());
            visible = 0;
        }
        visible += 1;
        if visible == columns && index + 1 < chars.len() {
            rows.push(String::new());
            visible = 0;
        }
    }
}

/// `Ok` when `command` is what the box `p` (on `rows`) draws: the box passes
/// [`may_prove`], its top rule of `─` gives its width (the text is four
/// cells narrower: ` │ ` and a column of margin, MEASURED at 144 and 100
/// columns), and the rows between its first and last dashed rule, each with
/// its trailing blanks dropped, are exactly ` │ ` and each of
/// [`barred_rows`] with its trailing blanks dropped, as many as it has.
///
/// # Errors
///
/// Why not: a box of another shape, no rule or no dashed rules, a command
/// this check does not draw, another count of rows, or the first row that
/// differs.
pub fn drawn_as(p: &PromptV2, rows: &[String], command: &str) -> Result<(), String> {
    if !may_prove(p) {
        return Err("the box is not one a transcript's command is matched to".to_string());
    }
    let (top, bottom) = p.span;
    let rule = top
        .checked_sub(1)
        .and_then(|r| rows.get(r))
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
    match drawn.iter().zip(&shown).position(|(d, s)| d != s) {
        Some(k) => Err(format!(
            "the command's row {} draws otherwise than the box's",
            k + 1
        )),
        None => Ok(()),
    }
}

/// A dashed rule, Claude Code's own chrome around a tool's content: a row at
/// column zero of ten or more `╌` (U+254C) and nothing else (aterm-phase's
/// reading of the bracketed box).
fn is_dashed_rule(row: &str) -> bool {
    let t = row.trim_end();
    t.starts_with('\u{254c}') && t.chars().all(|c| c == '\u{254c}') && t.chars().count() >= 10
}

#[cfg(test)]
#[path = "box_command_tests.rs"]
pub(crate) mod tests;
