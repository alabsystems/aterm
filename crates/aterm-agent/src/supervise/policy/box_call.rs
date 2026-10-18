// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! FILE-TOOL CANDIDATES IN THE AGENT'S TRANSCRIPT.
//!
//! A candidate is not current-mount identity: the file can lag the screen,
//! including at a complete EOF while an older call remains pending. The
//! approval policy therefore never uses this inference to remove a
//! possible tool's settings or hook hold. The helpers below describe the
//! transcript observation, not authority to press the current box.
//!
//! Claude Code draws a box for more than one tool under the same title and
//! no reason row ([`unvoiced_tools`]): the Bash tool previews a `sed -i` as
//! the Edit tool's `Edit file` box, row for row (2.1.295, 2.1.299 and 2.1.300
//! captures), and `Read file` is every read-only file tool's. So the owner's
//! ask rule or hook for any of those tools holds the box
//! (`approval.rs`'s `settings_ask`), and a status hook with matcher `*` holds
//! every Edit of the session (ATERM-KI-1015-1 F1, the over-hold).
//!
//! [`drew`] returns a transcript candidate ([`BoxCall`]) where the screen
//! shape passes [`box_may_prove`], every unspent pending call names one
//! possible file tool, and one names the shown file. A Bash call names a
//! command rather than a file, so it cannot produce such a candidate.
//! These conditions do not bind a call to the current box: an older call
//! can remain pending while a later box is already on screen. Every
//! possible tool's settings and hook hold therefore remains in force.
//!
//! THE LAG (MEASURED 2026-10-15 on 2.1.300, the lab's stub): Claude Code
//! draws the box BEFORE its call's row reaches the file, 5 to 86 ms later
//! (23 runs; once the row came first), and under a PreToolUse hook on every
//! tool it stood there 350 ms after the box. The former narrowing path used
//! the loop's settle and retry reads ([`LAG_TRIES`], [`LAG_STEP`], [`lagging`]).
//! Those observations did not establish current-call identity; the policy
//! no longer reads a transcript to lift a possible-tool hold.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use aterm_json::Value;
use aterm_phase::PromptV2;

use super::approval::unvoiced_tools;

/// The file-naming tools considered by the candidate inference (`file_path`,
/// `notebook_path`). A Bash call names a command, a Glob's or a Grep's a
/// pattern. The retained name does not imply current-box identity.
pub const PROVABLE: &[&str] = &["Edit", "Write", "NotebookEdit", "Read"];

/// How many times a transcript that names no call the box may be is read
/// again before candidate inference gives up.
pub const LAG_TRIES: u32 = 4;

/// How long apart those reads are. The historical lag measurement informed
/// this interval; waiting it does not establish current-call identity.
pub const LAG_STEP: Duration = Duration::from_millis(250);

/// A main-chain tool call with no result yet: its id, its tool, and the file
/// it names (`file_path`, or a notebook's `notebook_path`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCall {
    pub id: String,
    pub tool: String,
    pub file: Option<String>,
}

/// A matching transcript candidate ([`drew`]), without current-box identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoxCall {
    pub tool: String,
    pub id: String,
}

/// The main chain's calls with no result, in a transcript tail. Only `user`
/// and `assistant` rows count, and no subagent's (`isSidechain`): an
/// assistant row's calls are added, a user row's results take theirs away,
/// and a user row with no result (a person's turn, a task notification, a
/// meta row) ends every call before it. Any row that does not parse is
/// passed over, including an internal malformed row as well as a torn tail
/// boundary; the result does not attest complete or current evidence.
#[must_use]
pub fn pending_calls(jsonl: &str) -> Vec<PendingCall> {
    let mut pending: Vec<PendingCall> = Vec::new();
    for line in jsonl.lines() {
        let Ok(row) = crate::harness::transcript::calls(line) else {
            continue;
        };
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
                let file = ["file_path", "notebook_path"]
                    .iter()
                    .find_map(|k| input.and_then(|i| i.get(k)).and_then(Value::as_str));
                pending.push(PendingCall {
                    id: id.to_owned(),
                    tool: tool.to_owned(),
                    file: file.map(str::to_owned),
                });
            }
            continue;
        }
        let results: Vec<&Value> = parts
            .iter()
            .filter(|p| part_type(p) == Some("tool_result"))
            .collect();
        if results.is_empty() {
            pending.clear();
        } else {
            let answered = |c: &PendingCall| {
                results
                    .iter()
                    .any(|r| r.get("tool_use_id").and_then(Value::as_str) == Some(c.id.as_str()))
            };
            pending.retain(|c| !answered(c));
        }
    }
    pending
}

/// A content part's `type`.
fn part_type(part: &Value) -> Option<&str> {
    part.get("type").and_then(Value::as_str)
}

/// Whether the box `p` has the shape required for candidate inference: a
/// box many tools draw ([`unvoiced_tools`]), read whole (its title on the
/// screen), with no ` · from …` origin in its title, and a footer of its
/// own keys alone. Claude Code ends a stoppable subagent's box's footer in
/// its background agents' note, and a pane cut short cuts a footer's last
/// item: either leaves fewer keys than items ([`aterm_phase::footer_keys`]).
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
/// This retry heuristic cannot establish that the transcript has caught up
/// with the screen, even when it returns `false`.
#[must_use]
pub fn lagging(p: &PromptV2, pending: &[PendingCall], spent: &[String]) -> bool {
    let tools = unvoiced_tools(p);
    !live(pending, spent).any(|c| c.tool == "Bash" || tools.contains(&c.tool.as_str()))
}

/// A file-tool candidate matching the box `p`, or `None`. Calls in `spent`
/// are excluded. The shape must pass [`box_may_prove`], every unspent call
/// must name the same possible file tool ([`PROVABLE`]: never Bash), and one
/// must match the shown file ([`names`], `home` for a `~/` path). This is a
/// transcript/screen match, not evidence that the current mount belongs to
/// that call; settings and hook holds never narrow from this result.
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

/// The calls of `pending` not `spent`.
fn live<'a>(
    pending: &'a [PendingCall],
    spent: &'a [String],
) -> impl Iterator<Item = &'a PendingCall> {
    pending.iter().filter(|c| !spent.contains(&c.id))
}

/// Whether the call's `file` is the path a box shows (`shown`): `shown`'s
/// components, a leading `./` dropped and `~/` the `home` it names, are the
/// call's last ones; an absolute `shown` must be the call's whole path. A
/// path with `..` in it, or a `~/` with no home, names nothing.
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
/// with `..` or a prefix in it.
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
