// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! FILE-TOOL CANDIDATES IN THE AGENT'S TRANSCRIPT, AND THE SCREEN'S WORD.
//!
//! A candidate alone is not current-mount identity: the file can lag the
//! screen, including at a complete EOF while an older call remains pending.
//! Neither a candidate nor its agreement with screen rows establishes
//! current-mount identity. The approval policy retains every possible
//! tool's settings or hook hold.
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
//! can remain pending while a later box is already on screen.
//!
//! THE SCREEN'S WORD (ATERM-KI-1016-1, MEASURED 2026-10-18 on 2.1.303, and
//! in the 2.1.295 and 2.1.300 captures): the rows above the box tell the two
//! tools of an `Edit file` box apart where its own rows cannot. The Edit
//! tool's call row stands childless, `⏺ Update(notes.txt)`, while its box
//! does; a Bash `sed -i`'s, drawn as the same `⏺ Update(notes.txt)`, carries
//! `⎿  Waiting…`, waiting on its box or queued ([`screen_shows_edit_call`]).
//! The screen helper describes this captured layout, but it is diagnostic
//! only. A queued Edit can stand under a Bash box whose Waiting row is
//! off-screen, and a code-block row may look like a finished separator.
//! Agreement with an older pending Edit therefore never removes a hold.
//!
//! THE LAG (MEASURED 2026-10-15 on 2.1.300, the lab's stub): Claude Code
//! draws the box BEFORE its call's row reaches the file, 5 to 86 ms later
//! (23 runs; once the row came first), and under a PreToolUse hook on every
//! tool it stood there 350 ms after the box. The candidate reader can retry while
//! it names no call the box may be ([`LAG_TRIES`], [`LAG_STEP`],
//! [`lagging`]). Waiting does not establish identity, and the loop does
//! not perform these reads where the owner's hold cannot be lifted.

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
/// transcript/screen match, not evidence that the current mount belongs
/// to that call. No settings or hook hold narrows from it, including where
/// [`screen_shows_edit_call`] agrees.
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

/// WHETHER THE SCREEN DRAWS THE `Edit file` BOX `p` (on `rows`) AS THE EDIT
/// TOOL'S OWN CALL, not a Bash `sed -i` (module docs, "THE SCREEN'S WORD").
/// The box's rows are the same for both, but the call's own row in the
/// transcript above it is not (MEASURED on 2.1.295, 2.1.300 and 2.1.303 in
/// the real-Claude lab, unruled, under `Bash(sed:*)` and under a PreToolUse
/// hook on `*`): the Edit tool draws `⏺ Update(<path>)` with no child row
/// while its box stands, and a Bash call waiting on its box (or queued behind
/// another) carries `⎿  Waiting…` under its row (`⎿  $ <command>` on 2.1.280
/// to 2.1.283). And the row right above the box need not be the box's own:
/// Claude Code stands the box of a message's first call that has not run and
/// draws the calls queued behind it under that call's row, in call order.
/// In one message calling a `sed -i` then an Edit of the same file, the
/// sed's box stands under the QUEUED Edit's childless row, the sed's
/// `Waiting…` above that (`claude-2.1.303-box-bash-sed-then-edit-parallel.txt`);
/// an Edit then a sed, the Edit's box under the queued sed's `Waiting…`
/// (`claude-2.1.303-box-edit-then-bash-sed-parallel.txt`). And the agent's
/// own text can stand between two calls of one message.
///
/// So `true` only where: the box is read whole, its top rule right above its
/// title; the drawn rows right above the rule are a run of `⏺ Update(<file>)`
/// rows (or the same with the dot blinked off), each childless, and the
/// run's TOP, the box's own call, names the box's `<path>` (the rows under
/// it are Edits of its message queued behind it); over the run, past any of
/// the agent's text ([`text_row`]), the screen draws a row, and it is no
/// call's row (`⏺ Name(…`) and nothing still in progress (a `…`, but for a
/// finished output's `… +5 lines`, [`cut_output`]); and no row above the box
/// is a Bash call's that has not run (`⎿  Waiting…`, `⎿  Running…`,
/// `⎿  $ …`). Anything else is `false`: a run whose top is another file's
/// (that call's box would stand), a
/// queued call of another tool, an Edit then a Bash call of one message, a
/// message whose text reaches the screen's top (a sed's `Waiting…` row can
/// lie over it, off the screen), a cut box. This is diagnostic only: the
/// policy retains every possible tool's hold even where [`drew`] agrees.
/// The two can be wrong where the transcript lags with an
/// older Edit of the same file pending AND the screen misdraws the sed: its
/// row childless (never measured), or a row inside the text over the queued
/// Edit's row read as no text (a row three columns in or more, as a code
/// block can be) while the sed's `Waiting…` row is off the screen.
///
/// `rows` are the screen's from its top: the loop takes a `tail=` read of
/// such a box whole ([`reads_the_screen_above`]), so the top this walk can
/// reach is the screen's and never a tail's.
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
    // The box's own call's row and the rows of the calls of its message
    // queued behind it, which stand under it: the box is the run's top's.
    let mut own = None;
    while let Some(file) = drawn.peek().and_then(|r| update_of(r)) {
        own = Some(file);
        drawn.next();
    }
    // Over that, any of the agent's text of the message, and over the text a
    // row the screen draws: the box's message has no call before it that
    // has not run.
    let Some(over) = drawn.find(|r| !text_row(r)) else {
        return false;
    };
    own == Some(path)
        && !call_row(over)
        && (cut_output(over) || !over.contains('…'))
        && !above.iter().any(|r| bash_unrun(r))
}

/// Whether the decision on the box `p` reads every row above it, to the
/// screen's top ([`screen_shows_edit_call`]): an `Edit file` box, which the
/// Edit tool and a Bash `sed -i` both draw. The loop reads such a box off the
/// whole grid, never off a `tail=` read alone (`run.rs`'s
/// `tail_misses_the_live_rows`): on a fresh pane Claude Code draws its REPL
/// from the top, and a `tail=40` read of a 50-row pane began one row under
/// the Edit's own `⏺ Update(notes.txt)` (MEASURED 2026-10-18 on 2.1.303, the
/// lab's s3e), so the call's row was not among the rows and the box went to
/// the owner. A whole read costs one `text` more per look at such a box and
/// types nothing.
#[must_use]
pub fn reads_the_screen_above(p: &PromptV2) -> bool {
    unvoiced_tools(p) == ["Edit", "Bash"]
}

/// The file `row` names where it is an Edit call's own row, `⏺ Update(<file>)`
/// (or the same with its dot blinked off).
fn update_of(row: &str) -> Option<&str> {
    row.strip_prefix("⏺ ")
        .or_else(|| row.strip_prefix("  "))?
        .strip_prefix("Update(")?
        .strip_suffix(')')
}

/// Whether `row` is a row of the agent's own text: its first (`⏺ ` and no
/// call's row) or one carried on under it (two columns in, no `⎿`), with no
/// `…` in it (a call's progress row, `  Reading 1 file…`, is never text). A
/// row further in (a call's output), a `⎿` row, a call's row, the prompt's,
/// a turn's `✻` row and the banner are not.
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
/// with, `… +<n> line(s)` (`     … +5 lines`, `… +1 line (ctrl+o to expand)`):
/// no call in progress, though it carries a `…`.
fn cut_output(row: &str) -> bool {
    row.trim_start().strip_prefix("… +").is_some_and(|r| {
        let digits = r.chars().take_while(char::is_ascii_digit).count();
        digits > 0 && r[digits..].starts_with(" line")
    })
}

/// Whether `row` is a tool call's own row: `⏺ ` (or its blinked-off dot) and
/// a name of one word opening a `(`, as `⏺ Bash(ls)` and `⏺ Update(x)` are and
/// `⏺ Read 1 file` and an assistant's sentence are not.
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
/// `⎿  Waiting…` (waiting on its box, or queued), `⎿  Running…`, or a
/// 2.1.280-style `⎿  $ <command>`.
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
