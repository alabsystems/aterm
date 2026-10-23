// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! FILE-TOOL CANDIDATES IN THE AGENT'S TRANSCRIPT, AND THE SCREEN'S WORD.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use aterm_json::Value;
use aterm_phase::PromptV2;

use super::approval::unvoiced_tools;

/// The file-naming tools considered by the candidate inference (`file_path`, `notebook_path`).
pub const PROVABLE: &[&str] = &["Edit", "Write", "NotebookEdit", "Read"];

/// How many times a transcript that names no call the box may be is read again before candidate inference gives up.
pub const LAG_TRIES: u32 = 4;

/// How long apart those reads are.
pub const LAG_STEP: Duration = Duration::from_millis(250);

/// A main-chain tool call with no result yet: its id, its tool, the file it names (`file_path`, or a notebook's `notebook_path`) and a Bash call's `command`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCall {
    pub id: String,
    pub tool: String,
    pub file: Option<String>,
    pub command: Option<String>,
}

/// A matching transcript candidate ([`drew`]), without current-box identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoxCall {
    pub tool: String,
    pub id: String,
}

/// THE MAIN CHAIN'S CALLS WITH NO RESULT, in a transcript tail, IN THE CONVERSATION'S ORDER (the one reader of both box lanes: this module's and [`super::box_command::pending_bash`]).
#[must_use]
pub fn pending_calls(jsonl: &str) -> Vec<PendingCall> {
    let mut parent: HashMap<String, String> = HashMap::new();
    let mut calls: Vec<(usize, Option<String>, PendingCall)> = Vec::new();
    let mut answered: Vec<String> = Vec::new();
    let mut prompts: Vec<(usize, Option<String>)> = Vec::new();
    for (at, line) in jsonl.lines().enumerate() {
        let Ok(row) = crate::harness::transcript::calls(line) else {
            continue;
        };
        let uuid = row.get("uuid").and_then(Value::as_str).map(str::to_owned);
        if let (Some(u), Some(p)) = (uuid.as_ref(), row.get("parentUuid").and_then(Value::as_str)) {
            parent.insert(u.clone(), p.to_owned());
        }
        let kind = row.get("type").and_then(Value::as_str);
        if !matches!(kind, Some("user" | "assistant"))
            || row.get("isSidechain").and_then(Value::as_bool) == Some(true)
        {
            continue;
        }
        let parts = row
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        if kind == Some("assistant") {
            for p in parts.iter().filter(|p| part_type(p) == Some("tool_use")) {
                let (Some(id), Some(tool)) = (
                    p.get("id").and_then(Value::as_str),
                    p.get("name").and_then(Value::as_str),
                ) else {
                    continue;
                };
                let input = p.get("input");
                let arg = |k: &str| {
                    input
                        .and_then(|i| i.get(k))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                };
                calls.push((
                    at,
                    uuid.clone(),
                    PendingCall {
                        id: id.to_owned(),
                        tool: tool.to_owned(),
                        file: arg("file_path").or_else(|| arg("notebook_path")),
                        command: arg("command"),
                    },
                ));
            }
            continue;
        }
        let results: Vec<String> = parts
            .iter()
            .filter(|p| part_type(p) == Some("tool_result"))
            .filter_map(|p| p.get("tool_use_id").and_then(Value::as_str))
            .map(str::to_owned)
            .collect();
        if results.is_empty() {
            prompts.push((at, uuid));
        } else {
            answered.extend(results);
        }
    }
    let ended = |at: usize, of: Option<&String>| {
        prompts.iter().any(|(row, prompt)| {
            *row > at
                && !matches!(
                    (prompt, of),
                    (Some(prompt), Some(of)) if ancestor(&parent, of, prompt)
                )
        })
    };
    calls
        .into_iter()
        .filter(|(at, of, c)| !answered.contains(&c.id) && !ended(*at, of.as_ref()))
        .map(|(_, _, c)| c)
        .collect()
}

/// Whether `who` is on the `parentUuid` chain above the row `of`, followed
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

/// A content part's `type`.
fn part_type(part: &Value) -> Option<&str> {
    part.get("type").and_then(Value::as_str)
}

/// Whether the box `p` has the shape required for candidate inference: a box many tools draw ([`unvoiced_tools`]), read whole (its title on the screen), with no ` · from …` origin in its title, and a footer of its own keys alone.
#[must_use]
pub fn box_may_prove(p: &PromptV2) -> bool {
    unvoiced_tools(p).len() > 1
        && !p.head_off_screen
        && p.origin().is_none()
        && p.footer
            .as_deref()
            .is_some_and(|f| aterm_phase::footer_keys(f).len() == f.split(" · ").count())
}

/// Whether no unspent pending call names a possible tool for `p`, or Bash.
#[must_use]
pub fn lagging(p: &PromptV2, pending: &[PendingCall], spent: &[String]) -> bool {
    let tools = unvoiced_tools(p);
    !live(pending, spent).any(|c| c.tool == "Bash" || tools.contains(&c.tool.as_str()))
}

/// A file-tool candidate matching the box `p`, or `None`.
#[must_use]
pub fn drew(
    p: &PromptV2,
    pending: &[PendingCall],
    spent: &[String],
    home: Option<&Path>,
) -> Option<BoxCall> {
    if !box_may_prove(p) {
        return None;
    }
    let tool = live(pending, spent).next()?.tool.as_str();
    if !PROVABLE.contains(&tool)
        || !unvoiced_tools(p).contains(&tool)
        || live(pending, spent).any(|c| c.tool != tool)
    {
        return None;
    }
    let shown = p.path.as_deref()?;
    live(pending, spent)
        .find(|c| c.file.as_deref().is_some_and(|f| names(shown, f, home)))
        .map(|c| BoxCall {
            tool: c.tool.clone(),
            id: c.id.clone(),
        })
}

/// WHETHER THE SCREEN DRAWS THE `Edit file` BOX `p` (on `rows`) AS THE EDIT TOOL'S OWN CALL, not a Bash `sed -i` (module docs, "THE SCREEN'S WORD").
#[must_use]
pub fn screen_shows_edit_call(p: &PromptV2, rows: &[String]) -> bool {
    if !reads_the_screen_above(p) || p.head_off_screen {
        return false;
    }
    let Some(path) = p.path.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        return false;
    };
    let Some(rule) = p.span.0.checked_sub(1) else {
        return false;
    };
    if !rows
        .get(rule)
        .is_some_and(|r| aterm_phase::phase::is_rule(r))
    {
        return false;
    }
    let above = &rows[..rule];
    let mut drawn = above
        .iter()
        .rev()
        .map(|r| r.trim_end())
        .filter(|r| !r.is_empty())
        .peekable();
    let mut own = None;
    while let Some(file) = drawn.peek().and_then(|r| update_of(r)) {
        own = Some(file);
        drawn.next();
    }
    let Some(over) = drawn.find(|r| !text_row(r)) else {
        return false;
    };
    own == Some(path)
        && !call_row(over)
        && (cut_output(over) || !over.contains('…'))
        && !above.iter().any(|r| bash_unrun(r))
}

/// Whether the decision on the box `p` reads every row above it, to the screen's top ([`screen_shows_edit_call`]): an `Edit file` box, which the Edit tool and a Bash `sed -i` both draw.
#[must_use]
pub fn reads_the_screen_above(p: &PromptV2) -> bool {
    unvoiced_tools(p) == ["Edit", "Bash"]
}

/// The file `row` names where it is an Edit call's own row, `⏺ Update(<file>)`
fn update_of(row: &str) -> Option<&str> {
    row.strip_prefix("⏺ ")
        .or_else(|| row.strip_prefix("  "))?
        .strip_prefix("Update(")?
        .strip_suffix(')')
}

/// Whether `row` is a row of the agent's own text: its first (`⏺ ` and no
fn text_row(row: &str) -> bool {
    if row.contains('…') || call_row(row) {
        return false;
    }
    row.starts_with("⏺ ")
        || row
            .strip_prefix("  ")
            .and_then(|r| r.chars().next())
            .is_some_and(|c| !c.is_whitespace() && c != '⎿')
}

/// Whether `row` is the row Claude Code ends a finished call's cut output
fn cut_output(row: &str) -> bool {
    row.trim_start().strip_prefix("… +").is_some_and(|r| {
        let digits = r.chars().take_while(char::is_ascii_digit).count();
        digits > 0 && r[digits..].starts_with(" line")
    })
}

/// Whether `row` is a tool call's own row: `⏺ ` (or its blinked-off dot) and
fn call_row(row: &str) -> bool {
    let Some(rest) = row.strip_prefix("⏺ ").or_else(|| row.strip_prefix("  ")) else {
        return false;
    };
    let mut chars = rest.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_uppercase()) {
        return false;
    }
    let name: String = chars
        .clone()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '.' | '-'))
        .collect();
    chars.nth(name.chars().count()) == Some('(')
}

/// Whether `row` is the child row a Bash call draws before it has run:
fn bash_unrun(row: &str) -> bool {
    row.trim_start()
        .strip_prefix('⎿')
        .map(str::trim_start)
        .is_some_and(|c| {
            c.starts_with("Waiting…") || c.starts_with("Running…") || c.starts_with("$ ")
        })
}

/// The calls of `pending` not `spent`.
fn live<'a>(
    pending: &'a [PendingCall],
    spent: &'a [String],
) -> impl Iterator<Item = &'a PendingCall> {
    pending.iter().filter(|c| !spent.contains(&c.id))
}

/// Whether the call's `file` is the path a box shows (`shown`): `shown`'s
fn names(shown: &str, file: &str, home: Option<&Path>) -> bool {
    let shown = shown.trim();
    let shown: PathBuf = match shown.strip_prefix("~/") {
        Some(rest) => match home {
            Some(home) => home.join(rest),
            None => return false,
        },
        None => PathBuf::from(shown),
    };
    let (Some((want, rooted)), Some((have, file_rooted))) =
        (components(&shown), components(Path::new(file)))
    else {
        return false;
    };
    if want.is_empty() {
        return false;
    }
    if rooted {
        file_rooted && want == have
    } else {
        have.ends_with(&want)
    }
}

/// A path's named components and whether it is absolute; `None` for one
fn components(path: &Path) -> Option<(Vec<&std::ffi::OsStr>, bool)> {
    let mut named = Vec::new();
    let mut rooted = false;
    for c in path.components() {
        match c {
            Component::RootDir => rooted = true,
            Component::CurDir => {}
            Component::Normal(n) => named.push(n),
            Component::ParentDir | Component::Prefix(_) => return None,
        }
    }
    Some((named, rooted))
}

#[cfg(test)]
#[path = "box_call_tests.rs"]
mod tests;
