// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Kimi Code's own record of the approvals its box waits on: each agent's `wire.jsonl` under its
//! session (`docs/DESIGN-kimi-wire-verdict-2026-11-07.md`). A box on the grid shows a command
//! as its control characters left it; the record holds the string Kimi will run.
//!
//! Measured on Kimi Code 2.5.2: while a box stands, the agent's wire holds an
//! `interaction.request` of kind `approval` whose `request.display` is `{"kind":"command",
//! "command":…}` for Bash (`{"kind":"file_io","operation":"write"|"edit","path":…}` for Write and
//! Edit); answering it appends an `interaction.resolved` of the same `id`. Every event record
//! (each but the wire's opening `metadata`, which has `created_at`) carries `time`, the wall
//! clock's milliseconds since the Unix epoch when Kimi built it (`Date.now()`), so an
//! `interaction.request`'s `time` is when Kimi asked.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use aterm_json::Value;

// The words of Kimi's record and session layout this reader reads, as the anchors name them
// (`aterm_phase::anchors::KIMI_WIRE_KEYS`, which `vendor_drift` holds Kimi's bundle to).
const TYPE_REQUEST: &str = aterm_phase::anchor!("kimi.wire.interaction.request");
const TYPE_RESOLVED: &str = aterm_phase::anchor!("kimi.wire.interaction.resolved");
const KEY_TYPE: &str = aterm_phase::anchor!("kimi.wire.type");
const KEY_KIND: &str = aterm_phase::anchor!("kimi.wire.kind");
const KIND_APPROVAL: &str = aterm_phase::anchor!("kimi.wire.approval");
const KEY_ID: &str = aterm_phase::anchor!("kimi.wire.id");
const KEY_TIME: &str = aterm_phase::anchor!("kimi.wire.time");
const KEY_REQUEST: &str = aterm_phase::anchor!("kimi.wire.request");
const KEY_TOOL: &str = aterm_phase::anchor!("kimi.wire.toolName");
const KEY_DISPLAY: &str = aterm_phase::anchor!("kimi.wire.display");
/// Both the `kind` of a command's display and the key of its string.
const COMMAND: &str = aterm_phase::anchor!("kimi.wire.command");
const TOOL_BASH: &str = aterm_phase::anchor!("kimi.wire.Bash");
const FILE_PROJECTS: &str = aterm_phase::anchor!("kimi.wire.projects.json");
const DIR_SESSIONS: &str = aterm_phase::anchor!("kimi.wire.sessions");
const DIR_AGENTS: &str = aterm_phase::anchor!("kimi.wire.agents");
const FILE_STATE: &str = aterm_phase::anchor!("kimi.wire.state.json");
const KEY_CWD: &str = aterm_phase::anchor!("kimi.wire.cwd");
const FILE_WIRE: &str = aterm_phase::anchor!("kimi.wire.wire.jsonl");

/// One approval Kimi asked for and has not had answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    /// The request's `id` (`approval_<uuid>`), which its `interaction.resolved` names.
    pub id: String,
    /// `request.toolName`: `Bash`, `Write`, `Edit`, or another tool's name.
    pub tool: String,
    /// `request.display.command` of a `command` display, exactly as Kimi runs it.
    pub command: Option<String>,
    /// The request's `time`: when Kimi asked, in milliseconds since the Unix epoch.
    pub asked: u64,
}

/// Kimi Code's home, read off the files its process holds open (`open`, as `lsof -p` names
/// them): the folder of its `projects.json`, which Kimi 2.5.2 watches for its whole life at
/// `join(bootstrap.homeDir, "projects.json")` beside `join(homeDir, "sessions")` (read in its
/// bundle; measured held open). None unless exactly one such folder is held. A process's
/// environment cannot say: Kimi writes its title (`kimi-code`) over its exec arguments and
/// environment, which `ps -E` then shows empty (measured on 2.5.2).
#[must_use]
pub fn kimi_home_of(open: &[PathBuf]) -> Option<PathBuf> {
    let mut homes = open
        .iter()
        .filter(|path| path.is_absolute() && path.ends_with(FILE_PROJECTS))
        .filter_map(|path| path.parent());
    let home = homes.next()?;
    homes.all(|other| other == home).then(|| home.to_path_buf())
}

/// What Kimi's record says of a Bash box whose screen already passes the read-only rule
/// (`docs/DESIGN-kimi-wire-verdict-2026-11-07.md` §3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Every pending request is a Bash command with no control character that the rule passes,
    /// and one asked since the program last read its tab's input is what the box draws.
    ReadOnly,
    /// A pending request fails the rule, holds a control character, or is not a Bash command:
    /// the box may stand for it.
    NotReadOnly,
    /// The record cannot say: nothing bound, or no pending command asked since the program last
    /// read its tab's input is what the box draws.
    Withheld,
}

/// [`Verdict`] of `pending` (none: the record could not be read) for a box whose screen
/// readings are `readings`: conditions 2 and 3 of the design's rule, the screen's own reading
/// (condition 1) being the caller's. `last_input_ms` is no earlier than when the box's program
/// last read its tab's input (milliseconds since the Unix epoch, 0 for none): a request asked
/// then or before may be the one that input answered, its answer not yet on disk, so it never
/// stands for the box drawn now (condition 3), though it still fails one that is not read-only
/// (condition 2).
#[must_use]
pub fn verdict(
    pending: Option<&[Pending]>,
    readings: &[String],
    python_allow: &[String],
    last_input_ms: u64,
) -> Verdict {
    let Some(pending) = pending else {
        return Verdict::Withheld;
    };
    let passes = |p: &Pending| {
        p.tool == TOOL_BASH
            && p.command.as_deref().is_some_and(|command| {
                !command.chars().any(|c| c.is_control() && c != '\n')
                    && crate::supervise::policy::approval::readings_read_only(
                        &[command.to_string()],
                        python_allow,
                    )
                    .is_ok()
            })
    };
    if !pending.iter().all(passes) {
        return Verdict::NotReadOnly;
    }
    // Nothing pending draws nothing, so an empty set is withheld here too.
    let drawn = pending
        .iter()
        .filter(|p| p.asked > last_input_ms)
        .filter_map(|p| p.command.as_deref())
        .any(|command| readings.iter().any(|r| r == command));
    if drawn {
        Verdict::ReadOnly
    } else {
        Verdict::Withheld
    }
}

/// One `wire.jsonl` folded as far as its last whole line.
#[derive(Clone, Debug, Default)]
pub struct Wire {
    /// The byte after the last whole line folded.
    offset: u64,
    /// The file the offset is into (device and inode on unix): Kimi rewrites a wire by an
    /// atomic rename, which a new identity shows even where the new file is no shorter.
    identity: Option<(u64, u64)>,
    /// The requests asked and not resolved, in the order asked.
    open: Vec<Pending>,
    /// A line holding an event type the fold reads did not parse, or an approval or answer lacks
    /// what this reader needs of it: the record is not this reader's. It holds for the rest of
    /// this file, since the line it could not read may be a request still open.
    broken: bool,
    /// The file's last byte is not a newline: an append is in progress.
    partial: bool,
}

impl Wire {
    /// Fold `appended`, the bytes after [`Self::offset`]: every whole line in it, leaving a
    /// trailing partial line for the next read.
    pub fn advance(&mut self, appended: &[u8]) {
        let whole = appended
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |nl| nl + 1);
        self.partial = whole < appended.len();
        for line in appended[..whole].split(|&b| b == b'\n') {
            self.fold(line);
        }
        self.offset += whole as u64;
    }

    /// Fold one line. Only a line holding, as a JSON string, one of the two event types the fold
    /// reads is parsed: the rest of the record (prompts, loop events, tool snapshots) never moves
    /// the open set.
    fn fold(&mut self, line: &[u8]) {
        if !quotes(line, TYPE_REQUEST) && !quotes(line, TYPE_RESOLVED) {
            return;
        }
        let Ok(v) = aterm_json::from_slice::<Value>(&lone_surrogates_mended(line)) else {
            self.broken = true;
            return;
        };
        let text = |v: Option<&Value>, key: &str| {
            v.and_then(|v| v.get(key))
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        match v.get(KEY_TYPE).and_then(Value::as_str) {
            Some(TYPE_REQUEST) => {
                if v.get(KEY_KIND).and_then(Value::as_str) != Some(KIND_APPROVAL) {
                    return;
                }
                let request = v.get(KEY_REQUEST);
                let (Some(id), Some(tool), Some(asked)) = (
                    text(Some(&v), KEY_ID),
                    text(request, KEY_TOOL),
                    v.get(KEY_TIME).and_then(Value::as_u64),
                ) else {
                    self.broken = true;
                    return;
                };
                let display = request.and_then(|r| r.get(KEY_DISPLAY));
                let command = display
                    .filter(|d| d.get(KEY_KIND).and_then(Value::as_str) == Some(COMMAND))
                    .and_then(|d| text(Some(d), COMMAND));
                self.open.retain(|p| p.id != id);
                self.open.push(Pending {
                    id,
                    tool,
                    command,
                    asked,
                });
            }
            Some(TYPE_RESOLVED) => match text(Some(&v), KEY_ID) {
                Some(id) => self.open.retain(|p| p.id != id),
                None => self.broken = true,
            },
            _ => {}
        }
    }

    /// The open requests, or none when the record cannot say: a line that did not parse, or an
    /// append in progress.
    #[must_use]
    pub fn pending(&self) -> Option<&[Pending]> {
        (!self.broken && !self.partial).then_some(self.open.as_slice())
    }
}

/// Whether `line` holds `word` between two quotes, as a JSON string of it is written.
fn quotes(line: &[u8], word: &str) -> bool {
    let word = word.as_bytes();
    line.windows(word.len() + 2)
        .any(|w| w[0] == b'"' && w[w.len() - 1] == b'"' && &w[1..w.len() - 1] == word)
}

/// `line` with each `\u` escape of a lone UTF-16 surrogate written as `\ufffd`, the character
/// Node's UTF-8 encoding gives one. Kimi 2.5.2 cuts a Bash approval's `action` at 50 UTF-16 units
/// (`args.command.slice(0, 50)`, read in its bundle), which can split a pair, and
/// `JSON.stringify` writes the half it keeps as such an escape, which a strict parser refuses.
/// A pair, an escaped backslash before `u`, and every other escape are left as they are.
fn lone_surrogates_mended(line: &[u8]) -> Cow<'_, [u8]> {
    let mut escapes = Vec::new();
    let mut at = 0;
    while at < line.len() {
        if line[at] != b'\\' {
            at += 1;
            continue;
        }
        let unit = line
            .get(at + 1..at + 6)
            .filter(|e| e[0] == b'u' && e[1..].iter().all(u8::is_ascii_hexdigit))
            .and_then(|e| std::str::from_utf8(&e[1..]).ok())
            .and_then(|hex| u16::from_str_radix(hex, 16).ok());
        match unit {
            Some(unit) => {
                escapes.push((at, unit));
                at += 6;
            }
            None => at += 2,
        }
    }
    let high = |unit: u16| (0xD800..0xDC00).contains(&unit);
    let low = |unit: u16| (0xDC00..0xE000).contains(&unit);
    let mut lone = Vec::new();
    let mut k = 0;
    while let Some(&(at, unit)) = escapes.get(k) {
        if high(unit)
            && escapes
                .get(k + 1)
                .is_some_and(|&(next, other)| next == at + 6 && low(other))
        {
            k += 2;
            continue;
        }
        if high(unit) || low(unit) {
            lone.push(at);
        }
        k += 1;
    }
    if lone.is_empty() {
        return Cow::Borrowed(line);
    }
    let mut mended = Vec::with_capacity(line.len());
    let mut from = 0;
    for at in lone {
        mended.extend_from_slice(&line[from..at]);
        mended.extend_from_slice(b"\\ufffd");
        from = at + 6;
    }
    mended.extend_from_slice(&line[from..]);
    Cow::Owned(mended)
}

/// The wires of every Kimi session working in one folder, kept between reads so each read
/// folds only what was appended since.
#[derive(Debug, Default)]
pub struct Bound {
    wires: HashMap<PathBuf, Wire>,
}

impl Bound {
    /// Every approval the asking process, started at `started`, has pending in a session of
    /// `home` whose `state.json` names `cwd`: read from each of its agents' wires written at or
    /// after `started` (its own request is appended after it), and asked at or after it. A
    /// request asked before is an earlier Kimi's, stranded open when that one died with its box
    /// standing: a resumed session appends to the same wire, and no process answers an id it did
    /// not ask (Kimi's own replay counts such a request cancelled). `None` when the record cannot
    /// say: no session found, a folder that cannot be read (under `agents/`, of a session in
    /// `cwd`), a wire that cannot be read or does not parse, or one whose append is in progress.
    pub fn read(&mut self, home: &Path, cwd: &Path, started: SystemTime) -> Option<Vec<Pending>> {
        let want = canonical(cwd);
        let started_ms = started
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |since| {
                u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
            });
        let mut seen = Vec::new();
        let mut pending = Vec::new();
        for workspace in std::fs::read_dir(home.join(DIR_SESSIONS)).ok()? {
            let workspace = workspace.ok()?;
            if !workspace.file_type().ok()?.is_dir() {
                continue;
            }
            for session in std::fs::read_dir(workspace.path()).ok()? {
                let session = session.ok()?;
                if !session.file_type().ok()?.is_dir() {
                    continue;
                }
                let dir = session.path();
                let mut wires = Vec::new();
                let readable = wires_under(&dir.join(DIR_AGENTS), started, &mut wires).is_some();
                if readable && wires.is_empty() || !session_in(&dir, &want) {
                    continue;
                }
                if !readable {
                    return None;
                }
                for path in wires {
                    let wire = self.wires.entry(path.clone()).or_default();
                    if !refresh(wire, &path) {
                        return None;
                    }
                    pending.extend(
                        wire.pending()?
                            .iter()
                            .filter(|p| p.asked >= started_ms)
                            .cloned(),
                    );
                    seen.push(path);
                }
            }
        }
        self.wires.retain(|path, _| seen.contains(path));
        (!seen.is_empty()).then_some(pending)
    }
}

/// `path` with its links resolved, or as given when it cannot be.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Whether the session at `dir` works in `want` (its `state.json`'s `cwd`, canonical).
fn session_in(dir: &Path, want: &Path) -> bool {
    let Some((file, _)) = super::footer::open_regular_with_meta(&dir.join(FILE_STATE)) else {
        return false;
    };
    let mut text = String::new();
    if std::io::Read::read_to_string(&mut std::io::Read::take(file, 1 << 20), &mut text).is_err() {
        return false;
    }
    aterm_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v.get(KEY_CWD).and_then(Value::as_str).map(PathBuf::from))
        .is_some_and(|cwd| canonical(&cwd) == want)
}

/// Every `wire.jsonl` under `dir`, at any depth (Kimi's own `collectWireFiles` walks a
/// session's `agents/` so), modified at or after `started`. A session with no `dir` has none;
/// none when a folder or file under it cannot be read, since the wire it hides may hold the
/// request a box stands for.
fn wires_under(dir: &Path, started: SystemTime, out: &mut Vec<PathBuf>) -> Option<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(()),
        Err(_) => return None,
    };
    for entry in entries {
        let entry = entry.ok()?;
        let kind = entry.file_type().ok()?;
        let path = entry.path();
        if kind.is_dir() {
            wires_under(&path, started, out)?;
        } else if kind.is_file()
            && entry.file_name() == FILE_WIRE
            && entry.metadata().and_then(|m| m.modified()).ok()? >= started
        {
            out.push(path);
        }
    }
    Some(())
}

/// Fold what was appended to `path` since `wire` last read it. A file of another identity, or
/// one shorter than the offset, was rewritten (Kimi rewrites a wire by an atomic rename when it
/// compacts or migrates it), so it is read again from 0. False when the file cannot be read.
fn refresh(wire: &mut Wire, path: &Path) -> bool {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let Some((mut file, meta)) = super::footer::open_regular_with_meta(path) else {
        return false;
    };
    let identity = identity_of(&meta);
    if meta.len() < wire.offset || identity != wire.identity {
        *wire = Wire {
            identity,
            ..Wire::default()
        };
    }
    if file.seek(SeekFrom::Start(wire.offset)).is_err() {
        return false;
    }
    let mut appended = Vec::new();
    if file
        .take(meta.len() - wire.offset)
        .read_to_end(&mut appended)
        .is_err()
    {
        return false;
    }
    wire.advance(&appended);
    true
}

/// A file's device and inode, which an atomic rename over it changes.
#[cfg(unix)]
fn identity_of(meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    Some((meta.dev(), meta.ino()))
}

/// No identity off unix: a rewrite shows only as a shorter file.
#[cfg(not(unix))]
fn identity_of(_meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    None
}

/// Lines Kimi Code 2.5.2 wrote to its sessions' `agents/main/wire.jsonl` (MEASURED, each one
/// whole with its newline; `kimi_wire_tests.rs` says how they were taken), for tests here and in
/// dependent crates; not part of the documented API.
#[doc(hidden)]
pub mod fixtures {
    /// The wire's small opening records in order, the system prompt and tool list left out.
    pub const HEAD: &str = include_str!("fixtures/kimi-wire/kimi-2.5.2-head.jsonl");
    /// The approval a `cat README.md` Bash box (described `Show the readme`) waits on.
    pub const REQUEST: &str = include_str!("fixtures/kimi-wire/kimi-2.5.2-request.jsonl");
    /// The approval a `git push --force`, 16 backspaces, `cat README.md` and three spaces Bash
    /// box waits on, drawn as `$ cat README.md`.
    pub const REQUEST_BACKSPACED: &str =
        include_str!("fixtures/kimi-wire/kimi-2.5.2-request-backspaced.jsonl");
    /// The approval a Write of `notes.txt` waits on.
    pub const REQUEST_WRITE: &str =
        include_str!("fixtures/kimi-wire/kimi-2.5.2-request-write.jsonl");
    /// The approval an Edit of `notes.txt` waits on.
    pub const REQUEST_EDIT: &str = include_str!("fixtures/kimi-wire/kimi-2.5.2-request-edit.jsonl");
    /// An `AskUserQuestion`: an interaction of kind `question` that names no tool, from another
    /// session of the same Kimi Code (`kimi_wire_tests.rs` says which).
    pub const REQUEST_QUESTION: &str =
        include_str!("fixtures/kimi-wire/kimi-2.5.2-request-question.jsonl");
    /// The answer approving [`REQUEST`].
    pub const RESOLVED_APPROVED: &str =
        include_str!("fixtures/kimi-wire/kimi-2.5.2-resolved-approved.jsonl");
    /// The answer rejecting [`REQUEST_BACKSPACED`].
    pub const RESOLVED_REJECTED: &str =
        include_str!("fixtures/kimi-wire/kimi-2.5.2-resolved-rejected.jsonl");
    /// `git push --force`, 16 backspaces, `cat README.md` and three spaces: the command
    /// [`REQUEST_BACKSPACED`] names.
    pub const BACKSPACED: &str = "git push --force\u{8}\u{8}\u{8}\u{8}\u{8}\u{8}\u{8}\u{8}\u{8}\u{8}\u{8}\u{8}\u{8}\u{8}\u{8}\u{8}cat README.md   ";
}

#[cfg(test)]
#[path = "kimi_wire_tests.rs"]
mod tests;
